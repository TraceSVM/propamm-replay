use std::path::Path;

use arrow::array::*;
use arrow::record_batch::RecordBatch;
use base64::{engine::general_purpose::STANDARD, Engine};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

#[derive(Debug, Clone)]
pub struct AccountUpdateRow {
    pub timestamp_ms: u64,
    pub slot: u64,
    pub write_version: u64,
    pub txn_signature: String,
    pub account_role: String,
    pub account_data: Vec<u8>,
}

pub fn read_pool_state(pool_dir: &Path) -> anyhow::Result<Vec<AccountUpdateRow>> {
    let exact_path = pool_dir.join("state.parquet");
    let glob_pattern = pool_dir.join("state_*.parquet");
    let glob_str = glob_pattern.to_string_lossy().to_string();

    let mut parquet_files: Vec<_> = Vec::new();
    if exact_path.exists() {
        parquet_files.push(exact_path);
    }
    for entry in glob::glob(&glob_str)?.filter_map(|e| e.ok()) {
        parquet_files.push(entry);
    }
    parquet_files.sort();

    if parquet_files.is_empty() {
        tracing::warn!(dir = %pool_dir.display(), "no state parquet files found");
        return Ok(Vec::new());
    }

    let mut all_rows = Vec::new();

    for path in &parquet_files {
        let file = std::fs::File::open(path)
            .map_err(|e| anyhow::anyhow!("failed to open {}: {e}", path.display()))?;
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;

        for batch_result in reader {
            let batch = batch_result?;
            parse_state_batch(&batch, &mut all_rows)?;
        }
    }

    all_rows.sort_by_key(|r| (r.slot, r.write_version));

    tracing::info!(
        dir = %pool_dir.display(),
        files = parquet_files.len(),
        rows = all_rows.len(),
        "read state parquet files"
    );

    Ok(all_rows)
}

fn col_u64<'a>(batch: &'a RecordBatch, name: &str) -> anyhow::Result<&'a UInt64Array> {
    batch
        .column_by_name(name)
        .ok_or_else(|| anyhow::anyhow!("missing column: {name}"))?
        .as_any()
        .downcast_ref::<UInt64Array>()
        .ok_or_else(|| anyhow::anyhow!("column {name} is not UInt64"))
}

fn col_str<'a>(batch: &'a RecordBatch, name: &str) -> anyhow::Result<&'a StringArray> {
    batch
        .column_by_name(name)
        .ok_or_else(|| anyhow::anyhow!("missing column: {name}"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow::anyhow!("column {name} is not Utf8"))
}

fn parse_state_batch(batch: &RecordBatch, out: &mut Vec<AccountUpdateRow>) -> anyhow::Result<()> {
    if batch.num_rows() == 0 {
        return Ok(());
    }

    let timestamp_ms = col_u64(batch, "timestamp_ms")?;
    let slot = col_u64(batch, "slot")?;
    let write_version = col_u64(batch, "write_version")?;
    let txn_signature = col_str(batch, "txn_signature")?;
    let account_role = col_str(batch, "account_role")?;
    let account_data_b64 = col_str(batch, "account_data_b64")?;

    for i in 0..batch.num_rows() {
        let b64_str = account_data_b64.value(i);
        let data = STANDARD
            .decode(b64_str)
            .map_err(|e| anyhow::anyhow!("row {i}: base64 decode failed: {e}"))?;

        out.push(AccountUpdateRow {
            timestamp_ms: timestamp_ms.value(i),
            slot: slot.value(i),
            write_version: write_version.value(i),
            txn_signature: txn_signature.value(i).to_string(),
            account_role: account_role.value(i).to_string(),
            account_data: data,
        });
    }

    Ok(())
}
