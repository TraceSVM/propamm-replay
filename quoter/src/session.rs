use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolInfo {
    pub pool_id: String,

    pub amm_type: String,

    pub symbol: Option<String>,

    pub data_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub session_id: String,

    pub start_time: String,

    pub end_time: Option<String>,

    #[serde(default)]
    pub grpc_endpoint: Option<String>,

    #[serde(default)]
    pub quote_tiers_usd: Option<Vec<f64>>,

    pub pools: Vec<PoolInfo>,

    #[serde(default)]
    pub version: Option<String>,
}

impl SessionMetadata {
    pub fn load(session_dir: &Path) -> anyhow::Result<Self> {
        let metadata_path = session_dir.join("metadata.json");
        let contents = std::fs::read_to_string(&metadata_path)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", metadata_path.display()))?;
        let metadata: Self = serde_json::from_str(&contents)
            .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", metadata_path.display()))?;
        Ok(metadata)
    }

    pub fn pool_data_path(&self, session_dir: &Path, pool: &PoolInfo) -> PathBuf {
        session_dir.join("pools").join(&pool.data_dir)
    }
}
