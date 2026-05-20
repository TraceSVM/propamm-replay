use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result};
use serde::Deserialize;
use solana_pubkey::Pubkey;
use tracing::info;

#[derive(Debug, Clone)]
pub struct AccountEntry {
    pub role: String,
    pub pubkey: Pubkey,
}

#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub amm_type: String,

    pub pool_id: Pubkey,

    pub symbol: String,

    pub accounts: Vec<AccountEntry>,
}

#[derive(Debug, Clone)]
pub struct PythFeedConfig {
    pub feed_id: String,
    pub symbol: String,
}

#[derive(Deserialize)]
struct RawPoolConfig {
    #[serde(rename = "type")]
    pool_type: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    feed_id: Option<String>,
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    accounts: Option<HashMap<String, String>>,
}

pub fn load_configs(dir: &Path) -> Result<(Vec<PoolConfig>, Vec<PythFeedConfig>)> {
    let mut pools = Vec::new();
    let mut pyth_feeds = Vec::new();

    for entry in std::fs::read_dir(dir).with_context(|| format!("reading dir {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let raw: RawPoolConfig = serde_json::from_str(&contents)
            .with_context(|| format!("parsing {}", path.display()))?;

        if raw.pool_type == "pyth" {
            let feed_id = raw
                .feed_id
                .or(raw.id)
                .with_context(|| format!("missing feed_id in {}", path.display()))?;
            let symbol = raw.symbol.unwrap_or_default();
            info!(feed_id = %feed_id, symbol = %symbol, "loaded Pyth feed config");
            pyth_feeds.push(PythFeedConfig { feed_id, symbol });
            continue;
        }

        let id_str = raw
            .id
            .with_context(|| format!("missing id in {}", path.display()))?;
        let pool_id = Pubkey::from_str(&id_str)
            .with_context(|| format!("invalid pool id in {}", path.display()))?;

        let symbol = raw.symbol.unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string()
        });

        let mut account_entries = Vec::new();

        account_entries.push(AccountEntry {
            role: "pool".to_string(),
            pubkey: pool_id,
        });

        if let Some(accounts_map) = raw.accounts {
            for (role, pubkey_str) in &accounts_map {
                if role == "pool" {
                    continue;
                }
                let pubkey = Pubkey::from_str(pubkey_str).with_context(|| {
                    format!("invalid pubkey for role {} in {}", role, path.display())
                })?;
                account_entries.push(AccountEntry {
                    role: role.clone(),
                    pubkey,
                });
            }
        }

        info!(
            pool_type = %raw.pool_type,
            pool_id = %pool_id,
            symbol = %symbol,
            accounts = account_entries.len(),
            "loaded pool config"
        );

        pools.push(PoolConfig {
            amm_type: raw.pool_type,
            pool_id,
            symbol,
            accounts: account_entries,
        });
    }

    Ok((pools, pyth_feeds))
}
