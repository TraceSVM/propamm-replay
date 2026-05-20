use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::*;
use arrow::datatypes::*;
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use tracing::{error, info};

pub struct StateRow {
    pub timestamp_ms: u64,
    pub slot: u64,
    pub write_version: u64,
    pub txn_signature: String,
    pub pool_id: String,
    pub account_pubkey: String,
    pub account_role: String,
    pub account_data_b64: String,
    pub is_heartbeat: bool,
}

pub struct PythPriceRow {
    pub timestamp_ms: u64,
    pub feed_id: String,
    pub symbol: String,
    pub price: i64,
    pub conf: u64,
    pub expo: i32,
    pub publish_time: i64,
}

pub struct PoolBuffer {
    pub rows: Vec<StateRow>,
    pub dir: PathBuf,
}

const DEFAULT_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

pub struct SessionWriter {
    buffers: HashMap<String, PoolBuffer>,
    flush_interval: std::time::Duration,
    last_flush: std::time::Instant,
    flush_epoch: u32,
}

impl SessionWriter {
    pub fn new(pool_dir_map: &HashMap<String, PathBuf>) -> Self {
        let mut buffers = HashMap::new();
        for (pool_id, dir) in pool_dir_map {
            let _ = std::fs::create_dir_all(dir);
            buffers.insert(
                pool_id.clone(),
                PoolBuffer {
                    rows: Vec::new(),
                    dir: dir.clone(),
                },
            );
        }
        Self {
            buffers,
            flush_interval: DEFAULT_FLUSH_INTERVAL,
            last_flush: std::time::Instant::now(),
            flush_epoch: 0,
        }
    }

    pub fn push(&mut self, pool_id: &str, row: StateRow) {
        if let Some(buf) = self.buffers.get_mut(pool_id) {
            buf.rows.push(row);
        }
    }

    pub fn maybe_flush(&mut self) -> bool {
        if self.last_flush.elapsed() < self.flush_interval {
            return false;
        }
        self.flush_epoch += 1;
        let suffix = format!("_{:04}", self.flush_epoch);
        info!(epoch = self.flush_epoch, "periodic flush triggered");
        for (pool_id, buffer) in &mut self.buffers {
            if buffer.rows.is_empty() {
                continue;
            }
            info!(
                pool_id = %pool_id,
                rows = buffer.rows.len(),
                epoch = self.flush_epoch,
                "flushing pool buffer (periodic)"
            );
            if let Err(e) = write_state_parquet(&buffer.rows, &buffer.dir, &suffix) {
                error!(pool_id = %pool_id, error = %e, "periodic state flush failed");
            }
            buffer.rows.clear();
        }
        self.last_flush = std::time::Instant::now();
        true
    }

    pub fn finish_all(mut self) -> anyhow::Result<()> {
        let suffix = if self.flush_epoch > 0 {
            self.flush_epoch += 1;
            format!("_{:04}", self.flush_epoch)
        } else {
            String::new()
        };
        for (pool_id, buffer) in &self.buffers {
            if buffer.rows.is_empty() {
                continue;
            }
            info!(
                pool_id = %pool_id,
                rows = buffer.rows.len(),
                "flushing pool buffer to parquet (final)"
            );
            if let Err(e) = write_state_parquet(&buffer.rows, &buffer.dir, &suffix) {
                error!(pool_id = %pool_id, error = %e, "failed to write state parquet");
            }
        }
        Ok(())
    }
}

fn parquet_writer_props() -> WriterProperties {
    WriterProperties::builder()
        .set_compression(Compression::ZSTD(Default::default()))
        .build()
}

fn strings_to_array(iter: impl Iterator<Item = String>) -> StringArray {
    let v: Vec<String> = iter.collect();
    let refs: Vec<&str> = v.iter().map(|s| s.as_str()).collect();
    StringArray::from(refs)
}

fn write_state_parquet(rows: &[StateRow], dir: &Path, suffix: &str) -> anyhow::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("timestamp_ms", DataType::UInt64, false),
        Field::new("slot", DataType::UInt64, false),
        Field::new("write_version", DataType::UInt64, false),
        Field::new("txn_signature", DataType::Utf8, false),
        Field::new("pool_id", DataType::Utf8, false),
        Field::new("account_pubkey", DataType::Utf8, false),
        Field::new("account_role", DataType::Utf8, false),
        Field::new("account_data_b64", DataType::Utf8, false),
        Field::new("is_heartbeat", DataType::Boolean, false),
    ]));

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(UInt64Array::from(
                rows.iter().map(|r| r.timestamp_ms).collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter().map(|r| r.slot).collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter().map(|r| r.write_version).collect::<Vec<_>>(),
            )),
            Arc::new(strings_to_array(
                rows.iter().map(|r| r.txn_signature.clone()),
            )),
            Arc::new(strings_to_array(rows.iter().map(|r| r.pool_id.clone()))),
            Arc::new(strings_to_array(
                rows.iter().map(|r| r.account_pubkey.clone()),
            )),
            Arc::new(strings_to_array(
                rows.iter().map(|r| r.account_role.clone()),
            )),
            Arc::new(strings_to_array(
                rows.iter().map(|r| r.account_data_b64.clone()),
            )),
            Arc::new(BooleanArray::from(
                rows.iter().map(|r| r.is_heartbeat).collect::<Vec<_>>(),
            )),
        ],
    )?;

    let path = dir.join(format!("state{suffix}.parquet"));
    let file = File::create(&path)?;
    let mut writer = ArrowWriter::try_new(file, schema, Some(parquet_writer_props()))?;
    writer.write(&batch)?;
    writer.close()?;
    info!(path = %path.display(), rows = rows.len(), "wrote state parquet");
    Ok(())
}

pub fn write_pyth_parquet(rows: &[PythPriceRow], dir: &Path, suffix: &str) -> anyhow::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("timestamp_ms", DataType::UInt64, false),
        Field::new("feed_id", DataType::Utf8, false),
        Field::new("symbol", DataType::Utf8, false),
        Field::new("price", DataType::Int64, false),
        Field::new("conf", DataType::UInt64, false),
        Field::new("expo", DataType::Int32, false),
        Field::new("publish_time", DataType::Int64, false),
    ]));

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(UInt64Array::from(
                rows.iter().map(|r| r.timestamp_ms).collect::<Vec<_>>(),
            )),
            Arc::new(strings_to_array(rows.iter().map(|r| r.feed_id.clone()))),
            Arc::new(strings_to_array(rows.iter().map(|r| r.symbol.clone()))),
            Arc::new(Int64Array::from(
                rows.iter().map(|r| r.price).collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter().map(|r| r.conf).collect::<Vec<_>>(),
            )),
            Arc::new(Int32Array::from(
                rows.iter().map(|r| r.expo).collect::<Vec<_>>(),
            )),
            Arc::new(Int64Array::from(
                rows.iter().map(|r| r.publish_time).collect::<Vec<_>>(),
            )),
        ],
    )?;

    let path = dir.join(format!("pyth_prices{suffix}.parquet"));
    let file = File::create(&path)?;
    let mut writer = ArrowWriter::try_new(file, schema, Some(parquet_writer_props()))?;
    writer.write(&batch)?;
    writer.close()?;
    info!(path = %path.display(), rows = rows.len(), "wrote pyth prices parquet");
    Ok(())
}
