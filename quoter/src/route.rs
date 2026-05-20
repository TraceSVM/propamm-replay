use std::io::Write;
use std::path::Path;

use crate::protocols::{self, ProtocolReplay};
use crate::reader;
use crate::session::SessionMetadata;

const PROPAMM_TYPES: &[&str] = &[
    "humidifi", "bisonfi", "solfiv2", "goonfi", "tesserav", "zerofi",
];

const SKIP_SWAP_DETECTION: &[&str] = &["tesserav"];

struct TaggedUpdate {
    pool_idx: usize,
    timestamp_ms: u64,
    slot: u64,
    write_version: u64,
    account_role: String,
    account_data: Vec<u8>,
}

pub struct RouteRow {
    pub swap_ts_ms: u64,
    pub swap_slot: u64,
    pub swap_pool_id: String,
    pub swap_amm_type: String,
    pub direction: String,
    pub input_amount: u64,
    pub actual_output: u64,
    pub candidate_pool_id: String,
    pub candidate_amm_type: String,
    pub candidate_output: u64,
}

pub fn run_route_analysis(
    session_dir: &Path,
    metadata: &SessionMetadata,
) -> anyhow::Result<Vec<RouteRow>> {
    let pools: Vec<_> = metadata
        .pools
        .iter()
        .filter(|p| {
            PROPAMM_TYPES.contains(&p.amm_type.as_str())
                && p.symbol
                    .as_deref()
                    .is_some_and(|s| s.contains("SOL") && s.contains("USDC"))
        })
        .collect();

    tracing::info!(n_pools = pools.len(), "starting route analysis");

    let mut protos: Vec<Box<dyn ProtocolReplay>> = Vec::new();
    let mut pool_ids: Vec<String> = Vec::new();
    let mut amm_types: Vec<String> = Vec::new();
    for pool in &pools {
        let proto = protocols::create_protocol(&pool.amm_type)?;
        protos.push(proto);
        pool_ids.push(pool.pool_id.clone());
        amm_types.push(pool.amm_type.clone());
    }
    let n = protos.len();

    let mut all_updates: Vec<TaggedUpdate> = Vec::new();
    for (idx, pool) in pools.iter().enumerate() {
        let pool_dir = metadata.pool_data_path(session_dir, pool);
        let updates = reader::read_pool_state(&pool_dir)?;
        tracing::info!(pool_id = %pool.pool_id, amm = %pool.amm_type, rows = updates.len(), "loaded state");
        for u in updates {
            all_updates.push(TaggedUpdate {
                pool_idx: idx,
                timestamp_ms: u.timestamp_ms,
                slot: u.slot,
                write_version: u.write_version,
                account_role: u.account_role,
                account_data: u.account_data,
            });
        }
    }

    all_updates.sort_by_key(|u| (u.slot, u.write_version));
    tracing::info!(total_updates = all_updates.len(), "sorted all updates");

    let mut slot_start_vaults: Vec<Option<(u64, u64)>> = vec![None; n];
    let mut slot_end_ts: Vec<u64> = vec![0; n];
    let mut results: Vec<RouteRow> = Vec::new();
    let mut swap_count = 0u64;

    let total = all_updates.len();
    let mut i = 0;
    while i < total {
        let current_slot = all_updates[i].slot;

        if current_slot == 0 {
            while i < total && all_updates[i].slot == 0 {
                let u = &all_updates[i];
                protos[u.pool_idx].apply_update(&u.account_role, &u.account_data, u.slot);
                if let Some(v) = protos[u.pool_idx].vault_balances() {
                    slot_start_vaults[u.pool_idx] = Some(v);
                }
                i += 1;
            }
            continue;
        }

        for idx in 0..n {
            if slot_start_vaults[idx].is_none() {
                if let Some(v) = protos[idx].vault_balances() {
                    slot_start_vaults[idx] = Some(v);
                }
            }
        }

        while i < total && all_updates[i].slot == current_slot {
            let u = &all_updates[i];
            protos[u.pool_idx].apply_update(&u.account_role, &u.account_data, u.slot);
            slot_end_ts[u.pool_idx] = u.timestamp_ms;
            i += 1;
        }

        for idx in 0..n {
            let Some((base, quote)) = protos[idx].vault_balances() else {
                continue;
            };
            let Some((prev_base, prev_quote)) = slot_start_vaults[idx] else {
                slot_start_vaults[idx] = Some((base, quote));
                continue;
            };

            let bd = base as i64 - prev_base as i64;
            let qd = quote as i64 - prev_quote as i64;

            if bd != 0 && qd != 0 && (bd > 0) != (qd > 0) {
                let is_b2q = bd > 0;
                let direction = if is_b2q { "B2Q" } else { "Q2B" };
                let input_amount = if is_b2q { bd as u64 } else { qd as u64 };
                let actual_output = if is_b2q { (-qd) as u64 } else { (-bd) as u64 };

                if SKIP_SWAP_DETECTION.contains(&amm_types[idx].as_str()) {
                    slot_start_vaults[idx] = Some((base, quote));
                    continue;
                }
                if actual_output > 0 {
                    swap_count += 1;

                    for other_idx in 0..n {
                        if other_idx == idx {
                            continue;
                        }
                        if !protos[other_idx].is_ready() {
                            continue;
                        }

                        if let Some(candidate_output) =
                            protos[other_idx].quote_single(input_amount, direction, current_slot)
                        {
                            if candidate_output > 0 {
                                results.push(RouteRow {
                                    swap_ts_ms: slot_end_ts[idx],
                                    swap_slot: current_slot,
                                    swap_pool_id: pool_ids[idx].clone(),
                                    swap_amm_type: amm_types[idx].clone(),
                                    direction: direction.to_string(),
                                    input_amount,
                                    actual_output,
                                    candidate_pool_id: pool_ids[other_idx].clone(),
                                    candidate_amm_type: amm_types[other_idx].clone(),
                                    candidate_output,
                                });
                            }
                        }
                    }
                }
            }

            slot_start_vaults[idx] = Some((base, quote));
        }

        if swap_count > 0 && swap_count.is_multiple_of(10000) {
            tracing::info!(swaps = swap_count, "progress");
        }
    }

    tracing::info!(
        swaps = swap_count,
        route_rows = results.len(),
        "route analysis complete"
    );
    Ok(results)
}

pub fn write_route_csv(rows: &[RouteRow], path: &Path) -> anyhow::Result<()> {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    writeln!(f, "swap_ts_ms,swap_slot,swap_pool_id,swap_amm_type,direction,input_amount,actual_output,candidate_pool_id,candidate_amm_type,candidate_output")?;
    for r in rows {
        writeln!(
            f,
            "{},{},{},{},{},{},{},{},{},{}",
            r.swap_ts_ms,
            r.swap_slot,
            r.swap_pool_id,
            r.swap_amm_type,
            r.direction,
            r.input_amount,
            r.actual_output,
            r.candidate_pool_id,
            r.candidate_amm_type,
            r.candidate_output
        )?;
    }
    f.flush()?;
    Ok(())
}
