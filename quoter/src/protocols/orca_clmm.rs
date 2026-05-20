use bytemuck::{Pod, Zeroable};
use tracing::warn;

use super::clmm_math::{
    self, AdaptiveFeeConstants, AdaptiveFeeInfo, AdaptiveFeeVariables, ClmmPoolType,
    TickArraySequence,
};
use super::{ProtocolReplay, QuoteRow};

const ORCA_CLMM_TICK_ARR_SIZE: i32 = 88;

const WHIRLPOOL_DISCRIMINATOR: [u8; 8] = [63, 149, 209, 12, 225, 0, 85, 169];

#[repr(C, packed)]
#[derive(Default, Debug, Copy, Clone)]
pub struct OrcaCLMMAdaptiveFeeConstants {
    pub filter_period: u16,
    pub decay_period: u16,
    pub reduction_factor: u16,
    pub adaptive_fee_control_factor: u32,
    pub max_volatility_accumulator: u32,
    pub tick_group_size: u16,
    pub major_swap_threshold_ticks: u16,
    pub reserved: [u8; 16],
}

#[repr(C, packed)]
#[derive(Default, Debug, Copy, Clone)]
pub struct OrcaCLMMAdaptiveFeeVariables {
    pub last_reference_update_timestamp: u64,
    pub last_major_swap_timestamp: u64,
    pub volatility_reference: u32,
    pub tick_group_index_reference: i32,
    pub volatility_accumulator: u32,
    pub reserved: [u8; 16],
}

#[repr(C, packed)]
#[derive(Copy, Clone)]
pub struct OrcaCLMMOracle {
    pub whirlpool: [u8; 32],
    pub trade_enable_timestamp: u64,
    pub adaptive_fee_constants: OrcaCLMMAdaptiveFeeConstants,
    pub adaptive_fee_variables: OrcaCLMMAdaptiveFeeVariables,
    pub reserved: [u8; 128],
}

unsafe impl Zeroable for OrcaCLMMOracle {}
unsafe impl Pod for OrcaCLMMOracle {}
unsafe impl Zeroable for OrcaCLMMAdaptiveFeeConstants {}
unsafe impl Pod for OrcaCLMMAdaptiveFeeConstants {}
unsafe impl Zeroable for OrcaCLMMAdaptiveFeeVariables {}
unsafe impl Pod for OrcaCLMMAdaptiveFeeVariables {}

impl OrcaCLMMOracle {
    fn to_adaptive_fee_info(&self) -> AdaptiveFeeInfo {
        let c = self.adaptive_fee_constants;
        let v = self.adaptive_fee_variables;
        AdaptiveFeeInfo {
            constants: AdaptiveFeeConstants {
                filter_period: c.filter_period,
                decay_period: c.decay_period,
                reduction_factor: c.reduction_factor,
                adaptive_fee_control_factor: c.adaptive_fee_control_factor,
                max_volatility_accumulator: c.max_volatility_accumulator,
                tick_group_size: c.tick_group_size,
            },
            variables: AdaptiveFeeVariables {
                last_reference_update_timestamp: v.last_reference_update_timestamp,
                last_major_swap_timestamp: v.last_major_swap_timestamp,
                volatility_reference: v.volatility_reference,
                tick_group_index_reference: v.tick_group_index_reference,
                volatility_accumulator: v.volatility_accumulator,
            },
        }
    }
}

#[derive(Debug, Clone, Default)]
struct OrcaPoolState {
    liquidity: u128,
    sqrt_price: u128,
    tick_spacing: u16,
    fee_rate: u16,
}

fn parse_orca_pool(data: &[u8]) -> Option<OrcaPoolState> {
    if data.len() < 86 {
        warn!(len = data.len(), "orca pool data too small");
        return None;
    }

    if data[..8] != WHIRLPOOL_DISCRIMINATOR {
        warn!("orca pool discriminator mismatch");
        return None;
    }

    let tick_spacing = u16::from_le_bytes(data[42..44].try_into().unwrap());
    let fee_rate = u16::from_le_bytes(data[46..48].try_into().unwrap());
    let liquidity = u128::from_le_bytes(data[50..66].try_into().unwrap());
    let sqrt_price = u128::from_le_bytes(data[66..82].try_into().unwrap());

    Some(OrcaPoolState {
        liquidity,
        sqrt_price,
        tick_spacing,
        fee_rate,
    })
}

fn parse_orca_tick_array(data: &[u8], tick_spacing: u16) -> Option<(i32, Vec<(i32, i128)>)> {
    const TICK_SIZE: usize = 113;
    const DISCRIMINATOR_SIZE: usize = 8;
    const MIN_SIZE: usize = DISCRIMINATOR_SIZE + 4 + ORCA_CLMM_TICK_ARR_SIZE as usize * TICK_SIZE;

    if data.len() < MIN_SIZE {
        return None;
    }

    let start_tick_index = i32::from_le_bytes(data[8..12].try_into().unwrap());

    let mut ticks = Vec::new();
    for i in 0..ORCA_CLMM_TICK_ARR_SIZE as usize {
        let offset = 12 + i * TICK_SIZE;
        let initialized = data[offset] != 0;
        if initialized {
            let liquidity_net =
                i128::from_le_bytes(data[offset + 1..offset + 17].try_into().unwrap());
            let tick_index = start_tick_index + (i as i32) * (tick_spacing as i32);
            ticks.push((tick_index, liquidity_net));
        }
    }

    Some((start_tick_index, ticks))
}

#[derive(Debug, Clone)]
struct StoredTickArray {
    start_tick_index: i32,
    ticks: Vec<(i32, i128)>,
}

pub struct OrcaClmmReplay {
    pool_state: Option<OrcaPoolState>,
    adaptive_fee_info: Option<AdaptiveFeeInfo>,
    tick_arrays: Vec<StoredTickArray>,
    last_slot: u64,

    has_pool: bool,
}

impl OrcaClmmReplay {
    pub fn new() -> Self {
        Self {
            pool_state: None,
            adaptive_fee_info: None,
            tick_arrays: Vec::new(),
            last_slot: 0,
            has_pool: false,
        }
    }

    fn build_tick_sequence(&self) -> Option<TickArraySequence> {
        let pool = self.pool_state.as_ref()?;
        let tick_spacing = pool.tick_spacing as i32;

        let mut all_ticks: Vec<(i32, i128)> = Vec::new();
        for ta in &self.tick_arrays {
            all_ticks.extend_from_slice(&ta.ticks);
        }

        if all_ticks.is_empty() {
            return None;
        }

        let ticks_per_array = ORCA_CLMM_TICK_ARR_SIZE * tick_spacing;
        let min_start = self.tick_arrays.iter().map(|a| a.start_tick_index).min()?;
        let max_start = self.tick_arrays.iter().map(|a| a.start_tick_index).max()?;

        let left_boundary = min_start;
        let right_boundary = max_start + ticks_per_array;

        Some(TickArraySequence::from_raw_ticks(
            &all_ticks,
            ClmmPoolType::Orca,
            left_boundary,
            right_boundary,
        ))
    }

    fn quote_one_direction(
        &self,
        amount_in: u64,
        a_to_b: bool,
        pool: &OrcaPoolState,
        tick_seq: &TickArraySequence,
    ) -> Option<u64> {
        if amount_in == 0 {
            return Some(0);
        }

        let current_tick_index = clmm_math::tick_index_from_sqrt_price(&pool.sqrt_price);
        let current_internal_index =
            tick_seq.get_internal_bin_index_from_tick_index(current_tick_index);

        clmm_math::clmm_swap(
            a_to_b,
            ClmmPoolType::Orca,
            pool.sqrt_price,
            pool.liquidity,
            tick_seq,
            amount_in,
            pool.fee_rate,
            current_internal_index,
            &self.adaptive_fee_info,
            self.last_slot,
            current_tick_index,
            ORCA_CLMM_TICK_ARR_SIZE,
        )
    }

    fn mid_price_usd(&self) -> f64 {
        let pool = match &self.pool_state {
            Some(p) => p,
            None => return 0.0,
        };
        if pool.sqrt_price == 0 {
            return 0.0;
        }

        let sp = pool.sqrt_price as f64 / (1u128 << 64) as f64;

        sp * sp * 1e3
    }
}

impl ProtocolReplay for OrcaClmmReplay {
    fn apply_update(&mut self, role: &str, data: &[u8], slot: u64) {
        self.last_slot = slot;
        match role {
            "pool" => {
                if let Some(pool_state) = parse_orca_pool(data) {
                    self.pool_state = Some(pool_state);
                    self.has_pool = true;
                }
            }
            "oracle" => {
                if data.len() >= 8 + std::mem::size_of::<OrcaCLMMOracle>() {
                    let oracle: &OrcaCLMMOracle =
                        bytemuck::from_bytes(&data[8..8 + std::mem::size_of::<OrcaCLMMOracle>()]);
                    self.adaptive_fee_info = Some(oracle.to_adaptive_fee_info());
                }
            }
            _ => {
                if let Some(pool) = &self.pool_state {
                    let ts = pool.tick_spacing;
                    if let Some((start_idx, ticks)) = parse_orca_tick_array(data, ts) {
                        if let Some(existing) = self
                            .tick_arrays
                            .iter_mut()
                            .find(|a| a.start_tick_index == start_idx)
                        {
                            existing.ticks = ticks;
                        } else {
                            self.tick_arrays.push(StoredTickArray {
                                start_tick_index: start_idx,
                                ticks,
                            });
                        }

                        self.tick_arrays.sort_by_key(|a| a.start_tick_index);
                    }
                }
            }
        }
    }

    fn quote_single(&self, _input_amount: u64, _direction: &str, _slot: u64) -> Option<u64> {
        None
    }

    fn compute_quotes(&self, _slot: u64, tiers_usd: &[f64]) -> Vec<QuoteRow> {
        let mut rows = Vec::new();
        let pool = match &self.pool_state {
            Some(p) => p,
            None => return rows,
        };
        let tick_seq = match self.build_tick_sequence() {
            Some(ts) => ts,
            None => return rows,
        };

        let price = self.mid_price_usd();
        if price <= 0.0 {
            return rows;
        }

        for &tier_usd in tiers_usd {
            let sol_amount = (tier_usd / price * 1e9) as u64;
            if sol_amount > 0 {
                if let Some(out) = self.quote_one_direction(sol_amount, true, pool, &tick_seq) {
                    if out > 0 {
                        rows.push(QuoteRow {
                            direction: "B2Q".into(),
                            input_amount: sol_amount,
                            output_amount: out,
                            input_usd_equiv: tier_usd,
                        });
                    }
                }
            }

            let usdc_amount = (tier_usd * 1e6) as u64;
            if usdc_amount > 0 {
                if let Some(out) = self.quote_one_direction(usdc_amount, false, pool, &tick_seq) {
                    if out > 0 {
                        rows.push(QuoteRow {
                            direction: "Q2B".into(),
                            input_amount: usdc_amount,
                            output_amount: out,
                            input_usd_equiv: tier_usd,
                        });
                    }
                }
            }
        }

        rows
    }

    fn vault_balances(&self) -> Option<(u64, u64)> {
        None
    }

    fn is_ready(&self) -> bool {
        self.has_pool
            && self.pool_state.as_ref().is_some_and(|p| p.liquidity > 0)
            && !self.tick_arrays.is_empty()
    }
}
