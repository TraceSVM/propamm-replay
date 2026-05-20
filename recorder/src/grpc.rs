use std::collections::HashMap;

use futures::stream::Stream;
use futures::{SinkExt, StreamExt};
use solana_pubkey::Pubkey;
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};
use yellowstone_grpc_client::GeyserGrpcClient;
use yellowstone_grpc_proto::prelude::{
    subscribe_update::UpdateOneof, CommitmentLevel, SubscribeRequest,
    SubscribeRequestFilterAccounts, SubscribeRequestPing,
};

#[derive(Debug, Clone)]
pub struct AccountUpdate {
    pub pubkey: Pubkey,
    pub data: Vec<u8>,
    pub slot: u64,
    pub write_version: u64,
    pub txn_signature: Option<Vec<u8>>,
}

pub async fn subscribe_accounts(
    endpoint: String,
    account_pubkeys: Vec<Pubkey>,
) -> anyhow::Result<impl Stream<Item = AccountUpdate>> {
    let account_strings: Vec<String> = account_pubkeys.iter().map(|p| p.to_string()).collect();
    let (tx, rx) = tokio::sync::mpsc::channel::<AccountUpdate>(4096);

    tokio::spawn(async move {
        let mut backoff_secs: u64 = 1;
        const MAX_BACKOFF: u64 = 60;

        loop {
            info!(endpoint = %endpoint, "connecting to Yellowstone gRPC");

            let client_result = GeyserGrpcClient::build_from_shared(endpoint.clone())
                .and_then(|b| b.x_token(None::<String>));

            let mut client = match client_result {
                Ok(builder) => match builder.connect().await {
                    Ok(c) => c,
                    Err(e) => {
                        error!(error = %e, "failed to connect");
                        sleep(Duration::from_secs(backoff_secs)).await;
                        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF);
                        continue;
                    }
                },
                Err(e) => {
                    error!(error = %e, "failed to build client");
                    sleep(Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF);
                    continue;
                }
            };

            backoff_secs = 1;
            info!("connected to Yellowstone gRPC");

            let mut accounts_filter: HashMap<String, SubscribeRequestFilterAccounts> =
                HashMap::new();
            accounts_filter.insert(
                "recorder".to_owned(),
                SubscribeRequestFilterAccounts {
                    account: account_strings.clone(),
                    owner: vec![],
                    filters: vec![],
                    nonempty_txn_signature: None,
                },
            );

            let request = SubscribeRequest {
                accounts: accounts_filter,
                commitment: Some(CommitmentLevel::Processed as i32),
                ..Default::default()
            };

            let (mut subscribe_tx, mut stream) =
                match client.subscribe_with_request(Some(request)).await {
                    Ok(s) => {
                        info!("subscription established, waiting for account updates");
                        s
                    }
                    Err(e) => {
                        error!(error = %e, "failed to subscribe");
                        sleep(Duration::from_secs(backoff_secs)).await;
                        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF);
                        continue;
                    }
                };

            while let Some(message) = stream.next().await {
                match message {
                    Ok(msg) => match msg.update_oneof {
                        None => {
                            warn!("received message with no update_oneof");
                        }
                        Some(UpdateOneof::Account(account_msg)) => {
                            let slot = account_msg.slot;
                            if let Some(account_info) = account_msg.account {
                                let pubkey = match Pubkey::try_from(account_info.pubkey.as_slice())
                                {
                                    Ok(pk) => pk,
                                    Err(_) => continue,
                                };
                                let update = AccountUpdate {
                                    pubkey,
                                    data: account_info.data.to_vec(),
                                    slot,
                                    write_version: account_info.write_version,
                                    txn_signature: account_info.txn_signature,
                                };
                                if tx.send(update).await.is_err() {
                                    return;
                                }
                            }
                        }
                        Some(UpdateOneof::Ping(_)) => {
                            let _ = subscribe_tx
                                .send(SubscribeRequest {
                                    ping: Some(SubscribeRequestPing { id: 1 }),
                                    ..Default::default()
                                })
                                .await;
                        }
                        _ => {}
                    },
                    Err(e) => {
                        error!(error = %e, "stream error, reconnecting");
                        break;
                    }
                }
            }

            warn!("stream closed, reconnecting");
            sleep(Duration::from_secs(backoff_secs)).await;
            backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF);
        }
    });

    Ok(tokio_stream::wrappers::ReceiverStream::new(rx))
}
