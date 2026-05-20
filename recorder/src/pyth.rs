use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

use crate::config::PythFeedConfig;
use crate::writer::{write_pyth_parquet, PythPriceRow};

pub async fn stream_pyth_prices(
    feeds: Vec<PythFeedConfig>,
    session_dir: PathBuf,
    shutdown_flag: Arc<AtomicBool>,
) {
    if feeds.is_empty() {
        return;
    }

    let pyth_dir = session_dir.join("pools").join("pyth_prices");
    let _ = std::fs::create_dir_all(&pyth_dir);

    let subscribe_msg = {
        let ids: Vec<String> = feeds
            .iter()
            .map(|f| f.feed_id.trim_start_matches("0x").to_string())
            .collect();
        serde_json::json!({
            "type": "subscribe",
            "ids": ids,
            "parsed": true,
            "allow_unordered": true,
            "verbose": false,
        })
        .to_string()
    };

    let mut records: Vec<PythPriceRow> = Vec::new();
    let mut flush_epoch: u32 = 0;
    let mut last_flush = Instant::now();
    const FLUSH_INTERVAL: Duration = Duration::from_secs(60);
    const MAX_BACKOFF: u64 = 60;
    let mut backoff_secs: u64 = 1;

    loop {
        if shutdown_flag.load(Ordering::SeqCst) {
            break;
        }

        info!("connecting to Pyth Hermes WebSocket");
        let ws_result = tokio_tungstenite::connect_async("wss://hermes.pyth.network/ws").await;

        let (mut ws_stream, _) = match ws_result {
            Ok(conn) => {
                backoff_secs = 1;
                info!("connected to Pyth Hermes WebSocket");
                conn
            }
            Err(e) => {
                error!(error = %e, "failed to connect to Pyth WebSocket");
                tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF);
                continue;
            }
        };

        if let Err(e) = ws_stream
            .send(Message::Text(subscribe_msg.clone().into()))
            .await
        {
            error!(error = %e, "failed to send subscribe message");
            tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
            backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF);
            continue;
        }
        info!(feeds = feeds.len(), "sent Pyth subscribe message");

        loop {
            if shutdown_flag.load(Ordering::SeqCst) {
                break;
            }

            if last_flush.elapsed() >= FLUSH_INTERVAL && !records.is_empty() {
                flush_epoch += 1;
                let suffix = format!("_{:04}", flush_epoch);
                info!(
                    epoch = flush_epoch,
                    records = records.len(),
                    "periodic Pyth flush"
                );
                if let Err(e) = write_pyth_parquet(&records, &pyth_dir, &suffix) {
                    error!(error = %e, "failed to write Pyth prices parquet");
                }
                records.clear();
                last_flush = Instant::now();
            }

            let msg = tokio::time::timeout(Duration::from_secs(5), ws_stream.next()).await;

            match msg {
                Err(_) => {
                    continue;
                }
                Ok(None) => {
                    warn!("Pyth WebSocket stream closed");
                    break;
                }
                Ok(Some(Err(e))) => {
                    error!(error = %e, "Pyth WebSocket error");
                    break;
                }
                Ok(Some(Ok(Message::Text(text)))) => {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                        if json.get("type").and_then(|v| v.as_str()) == Some("price_update") {
                            let ts_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64;

                            if let Some(pf) = json.get("price_feed") {
                                let feed_id_raw =
                                    pf.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                if let Some(p) = pf.get("price") {
                                    let price: i64 = p
                                        .get("price")
                                        .and_then(|v| v.as_str())
                                        .and_then(|s| s.parse().ok())
                                        .unwrap_or(0);
                                    let conf: u64 = p
                                        .get("conf")
                                        .and_then(|v| v.as_str())
                                        .and_then(|s| s.parse().ok())
                                        .unwrap_or(0);
                                    let expo: i32 = p
                                        .get("expo")
                                        .and_then(|v| v.as_i64())
                                        .map(|v| v as i32)
                                        .unwrap_or(0);
                                    let publish_time: i64 =
                                        p.get("publish_time").and_then(|v| v.as_i64()).unwrap_or(0);

                                    let symbol = feeds
                                        .iter()
                                        .find(|f| f.feed_id.trim_start_matches("0x") == feed_id_raw)
                                        .map(|f| f.symbol.clone())
                                        .unwrap_or_default();

                                    records.push(PythPriceRow {
                                        timestamp_ms: ts_ms,
                                        feed_id: format!("0x{}", feed_id_raw),
                                        symbol,
                                        price,
                                        conf,
                                        expo,
                                        publish_time,
                                    });
                                }
                            }
                        }
                    }
                }
                Ok(Some(Ok(Message::Ping(data)))) => {
                    let _ = ws_stream.send(Message::Pong(data)).await;
                }
                Ok(Some(Ok(Message::Close(_)))) => {
                    info!("Pyth WebSocket received close frame");
                    break;
                }
                Ok(Some(Ok(_))) => {}
            }
        }

        if shutdown_flag.load(Ordering::SeqCst) {
            break;
        }

        warn!(backoff_secs, "Pyth WebSocket disconnected, reconnecting");
        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF);
    }

    if !records.is_empty() {
        flush_epoch += 1;
        let suffix = if flush_epoch > 1 {
            format!("_{:04}", flush_epoch)
        } else {
            String::new()
        };
        info!(records = records.len(), "final Pyth flush on shutdown");
        if let Err(e) = write_pyth_parquet(&records, &pyth_dir, &suffix) {
            error!(error = %e, "failed to write final Pyth prices parquet");
        }
    }
}
