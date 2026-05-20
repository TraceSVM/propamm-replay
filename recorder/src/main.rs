mod config;
mod grpc;
mod metadata;
mod pyth;
mod writer;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context;
use base64::{engine::general_purpose::STANDARD, Engine};
use clap::{Parser, Subcommand};
use futures::StreamExt;
use solana_pubkey::Pubkey;
use tracing::{error, info, warn};

use writer::{SessionWriter, StateRow};

fn parse_duration(s: &str) -> Result<Duration, String> {
    let mut total_secs: u64 = 0;
    let mut current_num = String::new();

    for c in s.chars() {
        if c.is_ascii_digit() {
            current_num.push(c);
        } else {
            let num: u64 = current_num
                .parse()
                .map_err(|_| format!("invalid number in duration: {}", current_num))?;
            current_num.clear();
            match c {
                'h' | 'H' => total_secs += num * 3600,
                'm' | 'M' => total_secs += num * 60,
                's' | 'S' => total_secs += num,
                _ => return Err(format!("invalid duration unit: {}", c)),
            }
        }
    }

    if !current_num.is_empty() {
        let num: u64 = current_num
            .parse()
            .map_err(|_| format!("invalid number in duration: {}", current_num))?;
        total_secs += num;
    }

    if total_secs == 0 {
        return Err("duration must be greater than 0".to_string());
    }

    Ok(Duration::from_secs(total_secs))
}

#[derive(Parser)]
#[command(name = "recorder", about = "Generic Solana account state recorder")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Record {
        #[arg(long)]
        grpc: String,

        #[arg(long)]
        pools: PathBuf,

        #[arg(long, default_value = "/var/solana/data/recordings")]
        output: PathBuf,

        #[arg(long, value_parser = parse_duration)]
        duration: Option<Duration>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        Command::Record {
            grpc,
            pools,
            output,
            duration,
        } => run_record(grpc, pools, output, duration).await,
    }
}

struct AccountLookup {
    pool_id: String,
    role: String,
}

async fn run_record(
    grpc_endpoint: String,
    pools_dir: PathBuf,
    base_output_dir: PathBuf,
    duration: Option<Duration>,
) -> anyhow::Result<()> {
    let (pools, pyth_feeds) = config::load_configs(&pools_dir)
        .with_context(|| format!("failed to load configs from {}", pools_dir.display()))?;
    info!(
        pools = pools.len(),
        pyth_feeds = pyth_feeds.len(),
        "loaded configs"
    );

    if pools.is_empty() {
        anyhow::bail!("no pool configs found in {}", pools_dir.display());
    }

    let mut account_lookup: HashMap<Pubkey, AccountLookup> = HashMap::new();
    for pool in &pools {
        for entry in &pool.accounts {
            account_lookup.insert(
                entry.pubkey,
                AccountLookup {
                    pool_id: pool.pool_id.to_string(),
                    role: entry.role.clone(),
                },
            );
        }
    }

    let all_pubkeys: Vec<Pubkey> = {
        let mut v: Vec<Pubkey> = account_lookup.keys().copied().collect();
        v.sort();
        v.dedup();
        v
    };
    info!(accounts = all_pubkeys.len(), "unique accounts to subscribe");

    let mut session = metadata::SessionMetadata::new(grpc_endpoint.clone(), &pools);
    info!(session_id = %session.session_id, "starting recording session");

    std::fs::create_dir_all(&base_output_dir)?;
    session.write(&base_output_dir)?;
    session.create_pool_dirs(&base_output_dir)?;

    let session_dir = session.session_dir(&base_output_dir);
    info!(path = %session_dir.display(), "session directory created");

    let pool_dir_map = metadata::build_pool_dir_map(&session, &base_output_dir);
    let mut session_writer = SessionWriter::new(&pool_dir_map);

    let rpc_url = std::env::var("PROGRAM_FETCH_RPC_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8899".to_string());
    let mut last_data: HashMap<Pubkey, Vec<u8>> = HashMap::new();
    info!("bootstrapping initial account state via RPC");
    let _slot = get_current_slot(&rpc_url).await.unwrap_or(0);
    let mut bootstrapped = 0u64;
    for pubkey in &all_pubkeys {
        match get_account_data(&rpc_url, pubkey).await {
            Ok(data) => {
                last_data.insert(*pubkey, data);
                bootstrapped += 1;
            }
            Err(e) => {
                warn!(pubkey = %pubkey, error = %e, "failed to bootstrap account");
            }
        }
    }
    info!(
        bootstrapped,
        total = all_pubkeys.len(),
        "initial state bootstrapped"
    );

    let shutdown_flag = Arc::new(AtomicBool::new(false));
    let shutdown_clone = shutdown_flag.clone();
    ctrlc::set_handler(move || {
        shutdown_clone.store(true, Ordering::SeqCst);
    })?;

    let pyth_handle = if !pyth_feeds.is_empty() {
        info!(count = pyth_feeds.len(), "starting Pyth price streaming");
        let sd = session_dir.clone();
        let sf = shutdown_flag.clone();
        Some(tokio::spawn(pyth::stream_pyth_prices(pyth_feeds, sd, sf)))
    } else {
        None
    };

    let mut stream = grpc::subscribe_accounts(grpc_endpoint, all_pubkeys)
        .await
        .context("failed to open gRPC subscription")?;

    let mut update_count: u64 = 0;
    let start_time = Instant::now();
    let mut last_heartbeat = Instant::now();
    const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

    if let Some(d) = duration {
        info!(
            duration_secs = d.as_secs(),
            "recording will stop after {}h {}m",
            d.as_secs() / 3600,
            (d.as_secs() % 3600) / 60
        );
    }

    while let Some(update) = stream.next().await {
        if shutdown_flag.load(Ordering::SeqCst) {
            info!("shutdown signal received");
            break;
        }

        if let Some(d) = duration {
            if start_time.elapsed() >= d {
                info!(
                    elapsed_secs = start_time.elapsed().as_secs(),
                    "duration limit reached"
                );
                break;
            }
        }

        let Some(lookup) = account_lookup.get(&update.pubkey) else {
            continue;
        };

        let txn_sig_str = update
            .txn_signature
            .as_ref()
            .map(|sig| bs58::encode(sig).into_string())
            .unwrap_or_default();

        let timestamp_ms = now_ms();
        let data_b64 = STANDARD.encode(&update.data);

        session_writer.push(
            &lookup.pool_id,
            StateRow {
                timestamp_ms,
                slot: update.slot,
                write_version: update.write_version,
                txn_signature: txn_sig_str,
                pool_id: lookup.pool_id.clone(),
                account_pubkey: update.pubkey.to_string(),
                account_role: lookup.role.clone(),
                account_data_b64: data_b64,
                is_heartbeat: false,
            },
        );

        last_data.insert(update.pubkey, update.data);

        update_count += 1;
        if update_count.is_multiple_of(1000) {
            info!(updates = update_count, "recording progress");
        }

        if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
            info!("heartbeat: snapshotting all accounts");
            let hb_ts = now_ms();
            for (pubkey, lookup) in &account_lookup {
                if let Some(data) = last_data.get(pubkey) {
                    let data_b64 = STANDARD.encode(data);
                    session_writer.push(
                        &lookup.pool_id,
                        StateRow {
                            timestamp_ms: hb_ts,
                            slot: 0,
                            write_version: 0,
                            txn_signature: String::new(),
                            pool_id: lookup.pool_id.clone(),
                            account_pubkey: pubkey.to_string(),
                            account_role: lookup.role.clone(),
                            account_data_b64: data_b64,
                            is_heartbeat: true,
                        },
                    );
                }
            }
            last_heartbeat = Instant::now();
        }

        session_writer.maybe_flush();
    }

    if let Err(e) = session_writer.finish_all() {
        error!(error = %e, "failed to flush parquet writers");
    }

    shutdown_flag.store(true, Ordering::SeqCst);

    if let Some(handle) = pyth_handle {
        let _ = handle.await;
    }

    if let Err(e) = session.finalize(&base_output_dir) {
        error!(error = %e, "failed to finalize session metadata");
    }

    info!(
        updates = update_count,
        session_id = %session.session_id,
        "recording session complete"
    );
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

async fn get_current_slot(rpc_url: &str) -> anyhow::Result<u64> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "getSlot"});
    let resp: serde_json::Value = client
        .post(rpc_url)
        .json(&body)
        .send()
        .await?
        .json()
        .await?;
    resp["result"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("failed to get slot"))
}

async fn get_account_data(rpc_url: &str, pubkey: &Pubkey) -> anyhow::Result<Vec<u8>> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getAccountInfo",
        "params": [pubkey.to_string(), {"encoding": "base64"}]
    });
    let resp: serde_json::Value = client
        .post(rpc_url)
        .json(&body)
        .send()
        .await?
        .json()
        .await?;
    let data_b64 = resp["result"]["value"]["data"][0]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no account data for {}", pubkey))?;
    let data = STANDARD
        .decode(data_b64)
        .with_context(|| format!("failed to decode base64 for {}", pubkey))?;
    Ok(data)
}
