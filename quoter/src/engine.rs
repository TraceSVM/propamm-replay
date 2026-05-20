use std::path::Path;

use rayon::prelude::*;

use crate::protocols;
use crate::reader;
use crate::session::{PoolInfo, SessionMetadata};

#[derive(Debug, Clone)]
pub struct EngineQuoteRow {
    pub timestamp_ms: u64,
    pub slot: u64,
    pub pool_id: String,
    pub amm_type: String,
    pub direction: String,
    pub input_amount: u64,
    pub output_amount: u64,
    pub input_usd_equiv: f64,
}

#[derive(Debug, Clone)]
pub struct EngineDerivedRow {
    pub timestamp_ms: u64,
    pub slot: u64,
    pub write_version: u64,
    pub txn_signature: String,
    pub pool_id: String,
    pub amm_type: String,
    pub base_vault_balance: u64,
    pub quote_vault_balance: u64,
}

pub struct PoolResult {
    pub pool_id: String,
    pub amm_type: String,
    pub symbol: Option<String>,
    pub quotes: Vec<EngineQuoteRow>,
    pub derived: Vec<EngineDerivedRow>,
    pub updates_processed: usize,
    pub quotes_emitted: usize,
}

pub fn process_session(
    session_dir: &Path,
    metadata: &SessionMetadata,
    tiers_usd: &[f64],
) -> Vec<anyhow::Result<PoolResult>> {
    let pools: Vec<&PoolInfo> = metadata.pools.iter().collect();
    let tiers = tiers_usd.to_vec();

    pools
        .par_iter()
        .map(|pool| {
            let pool_dir = metadata.pool_data_path(session_dir, pool);
            process_pool(pool, &pool_dir, &tiers)
        })
        .collect()
}

fn process_pool(pool: &PoolInfo, pool_dir: &Path, tiers_usd: &[f64]) -> anyhow::Result<PoolResult> {
    tracing::info!(
        pool_id = %pool.pool_id,
        amm_type = %pool.amm_type,
        symbol = ?pool.symbol,
        dir = %pool_dir.display(),
        "processing pool"
    );

    let mut protocol = protocols::create_protocol(&pool.amm_type)?;

    let updates = reader::read_pool_state(pool_dir)?;
    let update_count = updates.len();

    let mut quotes = Vec::new();
    let mut derived = Vec::new();

    let mut last_quote_slot: Option<u64> = None;

    let mut prev_vaults: Option<(u64, u64)> = None;
    let mut slot_vault_sig: Option<(String, u64, u64)> = None;

    for (idx, update) in updates.iter().enumerate() {
        let is_new_slot = idx == 0 || updates[idx - 1].slot != update.slot;
        if is_new_slot {
            slot_vault_sig = None;
        }

        protocol.apply_update(&update.account_role, &update.account_data, update.slot);

        if let Some((base_bal, quote_bal)) = protocol.vault_balances() {
            let changed = prev_vaults.is_none_or(|(pb, pq)| pb != base_bal || pq != quote_bal);
            if changed {
                slot_vault_sig = Some((
                    update.txn_signature.clone(),
                    update.write_version,
                    update.timestamp_ms,
                ));
                prev_vaults = Some((base_bal, quote_bal));
            }
        }

        let is_last_in_slot = idx + 1 >= updates.len() || updates[idx + 1].slot != update.slot;

        if protocol.is_ready() && is_last_in_slot && last_quote_slot != Some(update.slot) {
            last_quote_slot = Some(update.slot);

            let quote_rows = protocol.compute_quotes(update.slot, tiers_usd);
            for qr in &quote_rows {
                quotes.push(EngineQuoteRow {
                    timestamp_ms: update.timestamp_ms,
                    slot: update.slot,
                    pool_id: pool.pool_id.clone(),
                    amm_type: pool.amm_type.clone(),
                    direction: qr.direction.clone(),
                    input_amount: qr.input_amount,
                    output_amount: qr.output_amount,
                    input_usd_equiv: qr.input_usd_equiv,
                });
            }

            if let Some((base_bal, quote_bal)) = protocol.vault_balances() {
                let (sig, wv, ts) = match &slot_vault_sig {
                    Some((s, w, t)) => (s.clone(), *w, *t),
                    None => (
                        update.txn_signature.clone(),
                        update.write_version,
                        update.timestamp_ms,
                    ),
                };
                derived.push(EngineDerivedRow {
                    timestamp_ms: ts,
                    slot: update.slot,
                    write_version: wv,
                    txn_signature: sig,
                    pool_id: pool.pool_id.clone(),
                    amm_type: pool.amm_type.clone(),
                    base_vault_balance: base_bal,
                    quote_vault_balance: quote_bal,
                });
            }
        }
    }

    let quote_count = quotes.len();

    tracing::info!(
        pool_id = %pool.pool_id,
        updates = update_count,
        quotes = quote_count,
        derived_rows = derived.len(),
        "pool processing complete"
    );

    Ok(PoolResult {
        pool_id: pool.pool_id.clone(),
        amm_type: pool.amm_type.clone(),
        symbol: pool.symbol.clone(),
        quotes,
        derived,
        updates_processed: update_count,
        quotes_emitted: quote_count,
    })
}
