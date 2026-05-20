use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::*;
use arrow::datatypes::*;
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;

use crate::engine::{EngineDerivedRow, EngineQuoteRow};

const MAX_ROWS_PER_FILE: usize = 5_000_000;

fn parquet_props() -> WriterProperties {
    WriterProperties::builder()
        .set_compression(Compression::ZSTD(Default::default()))
        .build()
}

fn strings_to_array(iter: impl Iterator<Item = String>) -> StringArray {
    let v: Vec<String> = iter.collect();
    let refs: Vec<&str> = v.iter().map(|s| s.as_str()).collect();
    StringArray::from(refs)
}

fn quote_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("timestamp_ms", DataType::UInt64, false),
        Field::new("slot", DataType::UInt64, false),
        Field::new("pool_id", DataType::Utf8, false),
        Field::new("amm_type", DataType::Utf8, false),
        Field::new("direction", DataType::Utf8, false),
        Field::new("input_amount", DataType::UInt64, false),
        Field::new("output_amount", DataType::UInt64, false),
        Field::new("input_usd_equiv", DataType::Float64, false),
    ]))
}

pub fn write_quotes(rows: &[EngineQuoteRow], output_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    std::fs::create_dir_all(output_dir)?;
    let schema = quote_schema();
    let mut written_paths = Vec::new();

    for (chunk_idx, chunk) in rows.chunks(MAX_ROWS_PER_FILE).enumerate() {
        let path = output_dir.join(format!("quotes_{:04}.parquet", chunk_idx));

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(UInt64Array::from(
                    chunk.iter().map(|r| r.timestamp_ms).collect::<Vec<_>>(),
                )),
                Arc::new(UInt64Array::from(
                    chunk.iter().map(|r| r.slot).collect::<Vec<_>>(),
                )),
                Arc::new(strings_to_array(chunk.iter().map(|r| r.pool_id.clone()))),
                Arc::new(strings_to_array(chunk.iter().map(|r| r.amm_type.clone()))),
                Arc::new(strings_to_array(chunk.iter().map(|r| r.direction.clone()))),
                Arc::new(UInt64Array::from(
                    chunk.iter().map(|r| r.input_amount).collect::<Vec<_>>(),
                )),
                Arc::new(UInt64Array::from(
                    chunk.iter().map(|r| r.output_amount).collect::<Vec<_>>(),
                )),
                Arc::new(Float64Array::from(
                    chunk.iter().map(|r| r.input_usd_equiv).collect::<Vec<_>>(),
                )),
            ],
        )?;

        let file = File::create(&path)?;
        let mut writer = ArrowWriter::try_new(file, schema.clone(), Some(parquet_props()))?;
        writer.write(&batch)?;
        writer.close()?;

        tracing::debug!(
            path = %path.display(),
            rows = chunk.len(),
            "wrote quotes parquet"
        );
        written_paths.push(path);
    }

    Ok(written_paths)
}

fn derived_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("timestamp_ms", DataType::UInt64, false),
        Field::new("slot", DataType::UInt64, false),
        Field::new("write_version", DataType::UInt64, false),
        Field::new("txn_signature", DataType::Utf8, false),
        Field::new("pool_id", DataType::Utf8, false),
        Field::new("amm_type", DataType::Utf8, false),
        Field::new("base_vault_balance", DataType::UInt64, false),
        Field::new("quote_vault_balance", DataType::UInt64, false),
    ]))
}

pub fn write_derived_state(
    rows: &[EngineDerivedRow],
    output_dir: &Path,
) -> anyhow::Result<Vec<PathBuf>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    std::fs::create_dir_all(output_dir)?;
    let schema = derived_schema();
    let mut written_paths = Vec::new();

    for (chunk_idx, chunk) in rows.chunks(MAX_ROWS_PER_FILE).enumerate() {
        let path = output_dir.join(format!("derived_{:04}.parquet", chunk_idx));

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(UInt64Array::from(
                    chunk.iter().map(|r| r.timestamp_ms).collect::<Vec<_>>(),
                )),
                Arc::new(UInt64Array::from(
                    chunk.iter().map(|r| r.slot).collect::<Vec<_>>(),
                )),
                Arc::new(UInt64Array::from(
                    chunk.iter().map(|r| r.write_version).collect::<Vec<_>>(),
                )),
                Arc::new(strings_to_array(
                    chunk.iter().map(|r| r.txn_signature.clone()),
                )),
                Arc::new(strings_to_array(chunk.iter().map(|r| r.pool_id.clone()))),
                Arc::new(strings_to_array(chunk.iter().map(|r| r.amm_type.clone()))),
                Arc::new(UInt64Array::from(
                    chunk
                        .iter()
                        .map(|r| r.base_vault_balance)
                        .collect::<Vec<_>>(),
                )),
                Arc::new(UInt64Array::from(
                    chunk
                        .iter()
                        .map(|r| r.quote_vault_balance)
                        .collect::<Vec<_>>(),
                )),
            ],
        )?;

        let file = File::create(&path)?;
        let mut writer = ArrowWriter::try_new(file, schema.clone(), Some(parquet_props()))?;
        writer.write(&batch)?;
        writer.close()?;

        tracing::debug!(
            path = %path.display(),
            rows = chunk.len(),
            "wrote derived state parquet"
        );
        written_paths.push(path);
    }

    Ok(written_paths)
}
