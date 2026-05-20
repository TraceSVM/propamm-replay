use bytemuck::{Pod, Zeroable};
use tracing::warn;

use super::clmm_math::{self, ClmmPoolType, TickArraySequence};
use super::{ProtocolReplay, QuoteRow};

const RAYDIUM_TICK_ARRAY_SIZE: i32 = 60;

#[repr(C, packed)]
#[derive(Default, Copy, Clone)]
pub struct RaydiumPoolState {
    pub bump: [u8; 1],
    pub amm_config: [u8; 32],
    pub owner: [u8; 32],
    pub token_mint_0: [u8; 32],
    pub token_mint_1: [u8; 32],
    pub token_vault_0: [u8; 32],
    pub token_vault_1: [u8; 32],
    pub observation_key: [u8; 32],
    pub mint_decimals_0: u8,
    pub mint_decimals_1: u8,
    pub tick_spacing: u16,
    pub liquidity: u128,
    pub sqrt_price_x64: u128,
    pub tick_current: i32,
    pub padding3: u16,
    pub padding4: u16,
    pub fee_growth_global_0_x64: u128,
    pub fee_growth_global_1_x64: u128,
    pub protocol_fees_token_0: u64,
    pub protocol_fees_token_1: u64,
    pub swap_in_amount_token_0: u128,
    pub swap_out_amount_token_1: u128,
    pub swap_in_amount_token_1: u128,
    pub swap_out_amount_token_0: u128,
    pub status: u8,
    pub padding: [u8; 7],
    pub reward_infos: [RaydiumRewardInfo; 3],
    pub tick_array_bitmap: [u64; 16],
    pub total_fees_token_0: u64,
    pub total_fees_claimed_token_0: u64,
    pub total_fees_token_1: u64,
    pub total_fees_claimed_token_1: u64,
    pub fund_fees_token_0: u64,
    pub fund_fees_token_1: u64,
    pub open_time: u64,
    pub recent_epoch: u64,
    pub padding1: [u64; 24],
    pub padding2: [u64; 32],
}

#[repr(C, packed)]
#[derive(Default, Debug, PartialEq, Eq, Copy, Clone)]
pub struct RaydiumRewardInfo {
    pub reward_state: u8,
    pub open_time: u64,
    pub end_time: u64,
    pub last_update_time: u64,
    pub emissions_per_second_x64: u128,
    pub reward_total_emissioned: u64,
    pub reward_claimed: u64,
    pub token_mint: [u8; 32],
    pub token_vault: [u8; 32],
    pub authority: [u8; 32],
    pub reward_growth_global_x64: u128,
}

unsafe impl Zeroable for RaydiumPoolState {}
unsafe impl Zeroable for RaydiumRewardInfo {}
unsafe impl Pod for RaydiumPoolState {}
unsafe impl Pod for RaydiumRewardInfo {}

#[derive(Debug, Clone, Default)]
struct ParsedRaydiumPool {
    liquidity: u128,
    sqrt_price_x64: u128,
    tick_current: i32,
    tick_spacing: u16,
    fee_rate: u16,
}

fn parse_raydium_pool(data: &[u8]) -> Option<ParsedRaydiumPool> {
    if data.len() < 8 + std::mem::size_of::<RaydiumPoolState>() {
        warn!(len = data.len(), "raydium pool data too small");
        return None;
    }

    let pool: &RaydiumPoolState =
        bytemuck::from_bytes(&data[8..8 + std::mem::size_of::<RaydiumPoolState>()]);

    Some(ParsedRaydiumPool {
        liquidity: pool.liquidity,
        sqrt_price_x64: pool.sqrt_price_x64,
        tick_current: pool.tick_current,
        tick_spacing: pool.tick_spacing,
        fee_rate: 2500,
    })
}

fn parse_raydium_tick_array(data: &[u8], tick_spacing: u16) -> Option<(i32, Vec<(i32, i128)>)> {
    const TICK_SIZE: usize = 116;
    const DISCRIMINATOR_SIZE: usize = 8;
    const POOL_ID_SIZE: usize = 32;
    const NUM_TICKS: usize = 60;

    const MIN_SIZE: usize = DISCRIMINATOR_SIZE + POOL_ID_SIZE + NUM_TICKS * TICK_SIZE + 4;

    if data.len() < MIN_SIZE {
        return None;
    }

    let ticks_start = DISCRIMINATOR_SIZE + POOL_ID_SIZE;
    let ticks_end = ticks_start + NUM_TICKS * TICK_SIZE;

    let start_tick_index = i32::from_le_bytes(data[ticks_end..ticks_end + 4].try_into().unwrap());

    let mut ticks = Vec::new();
    for i in 0..NUM_TICKS {
        let offset = ticks_start + i * TICK_SIZE;
        let liquidity_net = i128::from_le_bytes(data[offset + 4..offset + 20].try_into().unwrap());
        let liquidity_gross =
            u128::from_le_bytes(data[offset + 20..offset + 36].try_into().unwrap());

        if liquidity_gross > 0 {
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

pub struct RaydiumClmmReplay {
    pool_state: Option<ParsedRaydiumPool>,
    tick_arrays: Vec<StoredTickArray>,
    last_slot: u64,
    has_pool: bool,
}

impl RaydiumClmmReplay {
    pub fn new() -> Self {
        Self {
            pool_state: None,
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

        let ticks_per_array = RAYDIUM_TICK_ARRAY_SIZE * tick_spacing;
        let min_start = self.tick_arrays.iter().map(|a| a.start_tick_index).min()?;
        let max_start = self.tick_arrays.iter().map(|a| a.start_tick_index).max()?;

        let left_boundary = min_start;
        let right_boundary = max_start + ticks_per_array;

        Some(TickArraySequence::from_raw_ticks(
            &all_ticks,
            ClmmPoolType::Raydium,
            left_boundary,
            right_boundary,
        ))
    }

    fn quote_one_direction(
        &self,
        amount_in: u64,
        a_to_b: bool,
        pool: &ParsedRaydiumPool,
        tick_seq: &TickArraySequence,
    ) -> Option<u64> {
        if amount_in == 0 {
            return Some(0);
        }

        let current_tick_index = pool.tick_current;
        let current_internal_index =
            tick_seq.get_internal_bin_index_from_tick_index(current_tick_index);

        clmm_math::clmm_swap(
            a_to_b,
            ClmmPoolType::Raydium,
            pool.sqrt_price_x64,
            pool.liquidity,
            tick_seq,
            amount_in,
            pool.fee_rate,
            current_internal_index,
            &None,
            0,
            current_tick_index,
            RAYDIUM_TICK_ARRAY_SIZE,
        )
    }

    fn mid_price_usd(&self) -> f64 {
        let pool = match &self.pool_state {
            Some(p) => p,
            None => return 0.0,
        };
        if pool.sqrt_price_x64 == 0 {
            return 0.0;
        }
        let sp = pool.sqrt_price_x64 as f64 / (1u128 << 64) as f64;

        sp * sp * 1e3
    }
}

impl ProtocolReplay for RaydiumClmmReplay {
    fn apply_update(&mut self, role: &str, data: &[u8], slot: u64) {
        self.last_slot = slot;
        match role {
            "pool" => {
                if let Some(pool_state) = parse_raydium_pool(data) {
                    self.pool_state = Some(pool_state);
                    self.has_pool = true;
                }
            }
            _ => {
                if let Some(pool) = &self.pool_state {
                    let ts = pool.tick_spacing;
                    if let Some((start_idx, ticks)) = parse_raydium_tick_array(data, ts) {
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
