use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::PoolConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolMetadata {
    pub pool_id: String,
    pub amm_type: String,
    pub symbol: String,
    pub data_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub session_id: String,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub grpc_endpoint: String,
    pub pools: Vec<PoolMetadata>,
    pub version: String,
}

impl SessionMetadata {
    pub fn new(grpc_endpoint: String, pools: &[PoolConfig]) -> Self {
        let start_time = Utc::now();
        let session_id = start_time.format("%Y-%m-%dT%H-%M-%SZ").to_string();

        let pool_metadata: Vec<PoolMetadata> = pools
            .iter()
            .map(|p| {
                let pool_id_str = p.pool_id.to_string();
                let safe_sym: String = p.symbol.replace("/", "-");
                let data_dir = if safe_sym.is_empty() {
                    pool_id_str[..12].to_string()
                } else {
                    format!("{}_{}", safe_sym, &pool_id_str[..8])
                };

                PoolMetadata {
                    pool_id: pool_id_str,
                    amm_type: p.amm_type.clone(),
                    symbol: p.symbol.clone(),
                    data_dir,
                }
            })
            .collect();

        Self {
            session_id,
            start_time,
            end_time: None,
            grpc_endpoint,
            pools: pool_metadata,
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    pub fn session_dir(&self, base_dir: &Path) -> PathBuf {
        base_dir.join(&self.session_id)
    }

    pub fn write(&self, base_dir: &Path) -> anyhow::Result<()> {
        let session_dir = self.session_dir(base_dir);
        std::fs::create_dir_all(&session_dir)?;
        let metadata_path = session_dir.join("metadata.json");
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(metadata_path, json)?;
        Ok(())
    }

    pub fn finalize(&mut self, base_dir: &Path) -> anyhow::Result<()> {
        self.end_time = Some(Utc::now());
        self.write(base_dir)
    }

    pub fn create_pool_dirs(&self, base_dir: &Path) -> anyhow::Result<()> {
        let pools_dir = self.session_dir(base_dir).join("pools");
        for pool in &self.pools {
            std::fs::create_dir_all(pools_dir.join(&pool.data_dir))?;
        }
        Ok(())
    }
}

pub fn build_pool_dir_map(
    metadata: &SessionMetadata,
    base_dir: &Path,
) -> std::collections::HashMap<String, PathBuf> {
    let session_dir = metadata.session_dir(base_dir);
    metadata
        .pools
        .iter()
        .map(|p| {
            let dir = session_dir.join("pools").join(&p.data_dir);
            (p.pool_id.clone(), dir)
        })
        .collect()
}
