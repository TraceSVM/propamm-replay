use super::{ProtocolReplay, QuoteRow};

const FEE_DENOM: u64 = 100_000;
const FEE_DENOM_V3: u64 = 2_560_000;
const DIVISOR: u64 = 10_000;
const NUM_TIERS: usize = 4;

const OFF_VERSION: usize = 0x08;
const OFF_BASE_RESERVE: usize = 0x30;
const OFF_QUOTE_RESERVE: usize = 0x38;
const OFF_FP_VAL: usize = 0x40;
const OFF_POOL_SLOT: usize = 0x48;
const OFF_B2Q_TIER_TABLE: usize = 0x120;
const OFF_Q2B_TIER_TABLE: usize = 0x160;
const OFF_MULT_0_Q2B: usize = 0x1A0;
const OFF_MULT_0_B2Q: usize = 0x1A4;
const OFF_DECIMAL_SCALE: usize = 0x1D0;
const OFF_FEE_MULT_B2Q: usize = 0x1E0;
const OFF_FEE_MULT_Q2B: usize = 0x1E2;
const OFF_FEE_BASE_B2Q: usize = 0x1E4;
const OFF_FEE_BASE_Q2B: usize = 0x1E5;
const OFF_FEE_PENALTY_EXTRA: usize = 0x1E6;
const OFF_STORED_FEE_MULT_B2Q: usize = 0x1E8;
const OFF_SLOT_PENALTY_MULT: usize = 0x20A;
const OFF_TEMPLATE_B2Q_TIER_TABLE: usize = 0x210;
const OFF_TEMPLATE_Q2B_TIER_TABLE: usize = 0x250;
const OFF_FEE_STEP_THRESHOLD: usize = 0x1F0;
const OFF_FEE_STEP_SIZE: usize = 0x1F8;
const OFF_FEE_ADJ: usize = 0x202;
const OFF_ADJ_CONST: usize = 0x288;
const OFF_MULT_0_TEMPLATE: usize = 0x290;
const OFF_LAST_INTERACTION_SLOT: usize = 0x2B0;
const OFF_ADJ_DOUBLE_FLAG: usize = 0x338;
const OFF_EFFECTIVE_MULT_0_Q2B: usize = 0x33A;
const OFF_EFFECTIVE_MULT_0: usize = 0x33C;
const OFF_DYNAMIC_FEE_MULT_B2Q: usize = 0x340;
const OFF_DYNAMIC_FEE_BASE_B2Q: usize = 0x342;
const OFF_DYNAMIC_FEE_MULT_Q2B: usize = 0x344;
const OFF_DYNAMIC_FEE_BASE_Q2B: usize = 0x346;

const OFF_V3_FP_HI_U64: usize = 0x340;
const OFF_V3_FP_LO: usize = 0x348;
const OFF_V3_ADJ_CONST: usize = 0x352;
const OFF_V3_FEE_B2Q: usize = 0x354;
const OFF_V3_FEE_Q2B: usize = 0x356;
const OFF_V3_FEE_B2Q_BASE: usize = 0x358;
const OFF_V3_FEE_Q2B_BASE: usize = 0x35A;
const OFF_V3_FEE_ADJ: usize = 0x35C;
const OFF_V3_FEE_STEP_THRESHOLD: usize = 0x360;
const OFF_V3_FEE_STEP_SIZE: usize = 0x368;
const OFF_V3_FEE_STALENESS_PEN_B2Q: usize = 0x372;
const OFF_V3_FEE_STALENESS_PEN_Q2B: usize = 0x374;
const OFF_V3_MULT_0_Q2B: usize = 0x378;
const OFF_V3_MULT_0_B2Q: usize = 0x37A;
const OFF_V3_FEE_FLOOR_B2Q: usize = 0x37E;
const OFF_V3_FEE_FLOOR_Q2B: usize = 0x380;

const MIN_POOL_DATA_SIZE: usize = 0x347;
const MIN_POOL_DATA_SIZE_V3: usize = 0x382;

#[derive(Clone, Copy, Default, Debug)]
struct TierEntry {
    mult: u32,
    adj_idx: i32,
    adj_val: i32,
}

#[inline]
fn read_u64_le(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}
#[inline]
fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}
#[inline]
fn read_u16_le(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap())
}
#[inline]
fn read_i32_le(data: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

#[inline(always)]
fn ceil_fee(fee_bps: u64, amount: u64) -> u64 {
    (fee_bps as u128 * amount as u128).div_ceil(FEE_DENOM as u128) as u64
}

#[inline(always)]
fn ceil_fee_v3(fee_bps: u64, amount: u64) -> u64 {
    (fee_bps as u128 * amount as u128).div_ceil(FEE_DENOM_V3 as u128) as u64
}

#[inline(always)]
fn b2q_v3_core(amount: u128, fp_hi: u128, fp_lo: u128) -> u128 {
    (((amount * fp_hi) >> 32) + amount * fp_lo) >> 24
}

#[inline(always)]
fn q2b_v3_core(amount: u128, fp_lo: u64, fp_hi_u64: u64) -> u128 {
    let fp_combined = ((fp_lo as u128) << 23) | ((fp_hi_u64 as u128) >> 41);
    (amount << 47) / fp_combined
}

#[derive(Debug, Clone, Default)]
struct BisonFiState {
    base_amount: u64,
    quote_amount: u64,
    fp_val: u64,
    decimal_scale: u64,
    pool_slot: u64,
    version: u64,
    fee_base_b2q: u64,
    fee_base_q2b: u64,
    fee_mult_b2q: u16,
    fee_mult_q2b: u16,
    fee_penalty_extra: u8,
    stored_fee_mult_b2q: u32,
    slot_penalty_mult: u64,
    mult_0_b2q: u32,
    mult_0_q2b: u32,
    adj_const: u32,
    b2q_tiers: [TierEntry; NUM_TIERS],
    q2b_tiers: [TierEntry; NUM_TIERS],
    template_mult_0: u32,
    template_b2q_tiers: [TierEntry; NUM_TIERS],
    template_q2b_tiers: [TierEntry; NUM_TIERS],
    effective_mult_0: u16,
    effective_mult_0_q2b: u16,
    base_reserve: u64,
    quote_reserve: u64,
    last_interaction_slot: u64,
    fee_step_threshold: u64,
    fee_step_size: u64,
    #[allow(dead_code)]
    fee_adj: u8,
    dynamic_fee_b2q: u64,
    dynamic_fee_q2b: u64,
    adj_double_flag: u16,
    v3_fp_hi_u64: u64,
    v3_fp_lo: u64,
    v3_fee_b2q: u64,
    v3_fee_q2b: u64,
    v3_fee_b2q_base: u64,
    v3_fee_q2b_base: u64,
    v3_fee_floor_b2q: u64,
    v3_fee_floor_q2b: u64,
    v3_fee_staleness_pen_b2q: u64,
    v3_fee_staleness_pen_q2b: u64,
    v3_mult_0_b2q: u16,
    v3_mult_0_q2b: u16,
    v3_adj_const: u64,
    v3_fee_step_threshold: u64,
    v3_fee_step_size: u64,
    v3_fee_adj: u64,
}

impl BisonFiState {
    fn update_from_pool_data(&mut self, data: &[u8]) -> Result<(), &'static str> {
        if data.len() < MIN_POOL_DATA_SIZE {
            return Err("BisonFi pool data too small");
        }
        if &data[..8] != b"POOLSTAT" {
            return Err("Invalid BisonFi pool magic");
        }

        self.version = read_u64_le(data, OFF_VERSION);
        self.base_reserve = read_u64_le(data, OFF_BASE_RESERVE);
        self.quote_reserve = read_u64_le(data, OFF_QUOTE_RESERVE);
        self.fp_val = read_u64_le(data, OFF_FP_VAL);
        self.pool_slot = read_u64_le(data, OFF_POOL_SLOT);
        self.decimal_scale = read_u64_le(data, OFF_DECIMAL_SCALE);

        self.fee_mult_b2q = read_u16_le(data, OFF_FEE_MULT_B2Q);
        self.fee_mult_q2b = read_u16_le(data, OFF_FEE_MULT_Q2B);
        self.fee_base_b2q = data[OFF_FEE_BASE_B2Q] as u64 + self.fee_mult_b2q as u64 * 10;
        self.fee_base_q2b = data[OFF_FEE_BASE_Q2B] as u64 + self.fee_mult_q2b as u64 * 10;
        self.fee_penalty_extra = data[OFF_FEE_PENALTY_EXTRA];
        self.stored_fee_mult_b2q = read_u32_le(data, OFF_STORED_FEE_MULT_B2Q);
        self.slot_penalty_mult = read_u16_le(data, OFF_SLOT_PENALTY_MULT) as u64;

        self.mult_0_b2q = read_u32_le(data, OFF_MULT_0_B2Q);
        self.mult_0_q2b = read_u32_le(data, OFF_MULT_0_Q2B);
        self.adj_const = read_u32_le(data, OFF_ADJ_CONST);
        self.last_interaction_slot = read_u64_le(data, OFF_LAST_INTERACTION_SLOT);

        for i in 0..NUM_TIERS {
            let base = OFF_B2Q_TIER_TABLE + i * 16;
            self.b2q_tiers[i] = TierEntry {
                mult: read_u32_le(data, base + 4),
                adj_idx: read_i32_le(data, base + 8),
                adj_val: read_i32_le(data, base + 12),
            };
        }
        for i in 0..NUM_TIERS {
            let base = OFF_Q2B_TIER_TABLE + i * 16;
            self.q2b_tiers[i] = TierEntry {
                mult: read_u32_le(data, base),
                adj_idx: read_i32_le(data, base + 8),
                adj_val: read_i32_le(data, base + 12),
            };
        }
        for i in 0..NUM_TIERS {
            let base = OFF_TEMPLATE_B2Q_TIER_TABLE + i * 16;
            self.template_b2q_tiers[i] = TierEntry {
                mult: read_u32_le(data, base + 4),
                adj_idx: read_i32_le(data, base + 8),
                adj_val: read_i32_le(data, base + 12),
            };
        }
        for i in 0..NUM_TIERS {
            let base = OFF_TEMPLATE_Q2B_TIER_TABLE + i * 16;
            self.template_q2b_tiers[i] = TierEntry {
                mult: read_u32_le(data, base),
                adj_idx: read_i32_le(data, base + 8),
                adj_val: read_i32_le(data, base + 12),
            };
        }

        self.template_mult_0 = read_u32_le(data, OFF_MULT_0_TEMPLATE);
        self.effective_mult_0 = read_u16_le(data, OFF_EFFECTIVE_MULT_0);
        self.effective_mult_0_q2b = read_u16_le(data, OFF_EFFECTIVE_MULT_0_Q2B);
        self.fee_step_threshold = read_u64_le(data, OFF_FEE_STEP_THRESHOLD);
        self.fee_step_size = read_u64_le(data, OFF_FEE_STEP_SIZE);
        self.fee_adj = data[OFF_FEE_ADJ];

        if self.version >= 3 && data.len() >= MIN_POOL_DATA_SIZE_V3 {
            self.v3_fp_hi_u64 = read_u64_le(data, OFF_V3_FP_HI_U64);
            self.v3_fp_lo = read_u64_le(data, OFF_V3_FP_LO);
            self.v3_adj_const = read_u16_le(data, OFF_V3_ADJ_CONST) as u64;
            self.v3_fee_b2q = read_u16_le(data, OFF_V3_FEE_B2Q) as u64;
            self.v3_fee_q2b = read_u16_le(data, OFF_V3_FEE_Q2B) as u64;
            self.v3_fee_b2q_base = read_u16_le(data, OFF_V3_FEE_B2Q_BASE) as u64;
            self.v3_fee_q2b_base = read_u16_le(data, OFF_V3_FEE_Q2B_BASE) as u64;
            self.v3_fee_adj = data[OFF_V3_FEE_ADJ] as u64;
            self.v3_fee_step_threshold = read_u64_le(data, OFF_V3_FEE_STEP_THRESHOLD);
            self.v3_fee_step_size = read_u64_le(data, OFF_V3_FEE_STEP_SIZE);
            self.v3_fee_staleness_pen_b2q = read_u16_le(data, OFF_V3_FEE_STALENESS_PEN_B2Q) as u64;
            self.v3_fee_staleness_pen_q2b = read_u16_le(data, OFF_V3_FEE_STALENESS_PEN_Q2B) as u64;
            self.v3_mult_0_q2b = read_u16_le(data, OFF_V3_MULT_0_Q2B);
            self.v3_mult_0_b2q = read_u16_le(data, OFF_V3_MULT_0_B2Q);
            self.v3_fee_floor_b2q = read_u16_le(data, OFF_V3_FEE_FLOOR_B2Q) as u64;
            self.v3_fee_floor_q2b = read_u16_le(data, OFF_V3_FEE_FLOOR_Q2B) as u64;
            self.dynamic_fee_b2q = 0;
            self.dynamic_fee_q2b = 0;
        } else if self.version >= 3 {
            self.v3_fp_hi_u64 = read_u64_le(data, OFF_V3_FP_HI_U64);
            self.v3_fp_lo = read_u64_le(data, OFF_V3_FP_LO);
            self.v3_adj_const = read_u16_le(data, OFF_V3_ADJ_CONST) as u64;
            self.v3_fee_b2q = read_u16_le(data, OFF_V3_FEE_B2Q) as u64;
            self.v3_fee_q2b = read_u16_le(data, OFF_V3_FEE_Q2B) as u64;
            self.v3_fee_adj = data[OFF_V3_FEE_ADJ] as u64;
            self.v3_fee_step_threshold = read_u64_le(data, OFF_V3_FEE_STEP_THRESHOLD);
            self.v3_fee_step_size = read_u64_le(data, OFF_V3_FEE_STEP_SIZE);
            self.v3_mult_0_q2b = read_u16_le(data, OFF_V3_MULT_0_Q2B);
            self.v3_mult_0_b2q = read_u16_le(data, OFF_V3_MULT_0_B2Q);
            self.dynamic_fee_b2q = 0;
            self.dynamic_fee_q2b = 0;
        } else {
            self.dynamic_fee_b2q = read_u16_le(data, OFF_DYNAMIC_FEE_MULT_B2Q) as u64 * 10
                + data[OFF_DYNAMIC_FEE_BASE_B2Q] as u64;
            self.dynamic_fee_q2b = read_u16_le(data, OFF_DYNAMIC_FEE_MULT_Q2B) as u64 * 10
                + data[OFF_DYNAMIC_FEE_BASE_Q2B] as u64;
        }

        self.adj_double_flag = read_u16_le(data, OFF_ADJ_DOUBLE_FLAG);
        Ok(())
    }

    fn get_quote(&self, amount_in: u64, is_base_to_quote: bool, clock_slot: u64) -> u64 {
        if self.base_amount == 0
            || self.quote_amount == 0
            || self.decimal_scale == 0
            || clock_slot.saturating_sub(self.pool_slot) > 100
        {
            return 0;
        }

        if self.version >= 3 {
            return self.get_quote_v3(amount_in, is_base_to_quote, clock_slot);
        }

        if self.fp_val == 0 {
            return 0;
        }

        let slot_diff = clock_slot.saturating_sub(self.pool_slot);
        let fee_bps = self.get_fee_bps(is_base_to_quote, clock_slot);
        let fee = ceil_fee(fee_bps, amount_in);
        let net = amount_in.saturating_sub(fee);

        if net == 0 {
            return 0;
        }

        let penalty = self.slot_penalty_mult * slot_diff;
        let penalty_active = penalty > 0;

        if is_base_to_quote {
            self.b2q_tiered(net, slot_diff)
        } else {
            self.q2b_tiered(net, penalty_active, slot_diff)
        }
    }

    #[inline(always)]
    fn cpi0_resets(&self, slot_diff: u64) -> bool {
        let stale_diff = self.pool_slot.saturating_sub(self.last_interaction_slot);
        stale_diff > 0 && slot_diff == 0
    }

    #[inline(always)]
    fn resolve_mult_0(&self, is_b2q: bool, penalty_active: bool, slot_diff: u64) -> u64 {
        if !self.cpi0_resets(slot_diff) {
            return if is_b2q {
                self.mult_0_b2q as u64
            } else {
                self.mult_0_q2b as u64
            };
        }
        if !is_b2q && penalty_active {
            return self.mult_0_q2b as u64;
        }
        if is_b2q {
            let eff = self.effective_mult_0 as u64;
            if eff > 0 {
                eff
            } else {
                self.mult_0_b2q as u64
            }
        } else {
            self.effective_mult_0_q2b as u64
        }
    }

    fn get_fee_bps(&self, is_base_to_quote: bool, clock_slot: u64) -> u64 {
        let slot_diff = clock_slot.saturating_sub(self.pool_slot);
        let penalty = self.slot_penalty_mult * slot_diff;
        let penalty_active = penalty > 0;

        let base_fee = if is_base_to_quote {
            self.fee_base_b2q
        } else {
            self.fee_base_q2b
        };
        let amount_fee = if self.fee_step_size > 0 && slot_diff > 0 {
            let dyn_fee = if is_base_to_quote {
                self.dynamic_fee_b2q
            } else {
                self.dynamic_fee_q2b
            };
            base_fee.max(dyn_fee)
        } else {
            base_fee
        };

        if penalty_active {
            let mut fee = amount_fee;
            let cpi0_resets = self.cpi0_resets(slot_diff);
            let b2q_split = (self.effective_mult_0 as u32) < self.mult_0_b2q;
            let both_mults_zero = self.fee_mult_b2q == 0 && self.fee_mult_q2b == 0;

            if is_base_to_quote && b2q_split && both_mults_zero && !cpi0_resets {
                fee = fee.max(self.stored_fee_mult_b2q as u64);
            }

            let opp_mult = if is_base_to_quote {
                self.fee_mult_q2b
            } else {
                self.fee_mult_b2q
            };
            let cross = if self.fee_penalty_extra > 0 {
                let cross_val = 1 + self.stored_fee_mult_b2q as u64 * self.fee_penalty_extra as u64;
                if opp_mult > 0 {
                    cross_val
                } else {
                    0
                }
            } else {
                let cross_ref = if is_base_to_quote {
                    fee
                } else {
                    self.fee_base_b2q
                };
                (self.stored_fee_mult_b2q as u64).saturating_sub(cross_ref)
            };

            fee + penalty + cross
        } else {
            amount_fee
        }
    }

    fn b2q_tiered(&self, net: u64, slot_diff: u64) -> u64 {
        let fp_val = self.fp_val;
        let decimal_scale = self.decimal_scale;
        let quote_reserve = self.quote_reserve;
        let adj_const = if self.adj_double_flag > 0 {
            self.adj_const as i64 * 2
        } else {
            self.adj_const as i64
        };

        if fp_val == 0 || decimal_scale == 0 {
            return 0;
        }

        let b2q_split = (self.effective_mult_0 as u32) < self.mult_0_b2q;
        let use_template;
        let eff_adj: u64;
        let mult_0_b2q: u64;
        let b2q_tiers: &[TierEntry; NUM_TIERS];
        let q2b_tiers = &self.template_q2b_tiers;

        if b2q_split {
            b2q_tiers = &self.template_b2q_tiers;
            eff_adj =
                (self.effective_mult_0_q2b as u64).saturating_sub(self.effective_mult_0 as u64);
            mult_0_b2q = self.effective_mult_0 as u64;
            use_template = true;
        } else if self.cpi0_resets(slot_diff) {
            b2q_tiers = &self.template_b2q_tiers;
            eff_adj =
                (self.effective_mult_0_q2b as u64).saturating_sub(self.effective_mult_0 as u64);
            mult_0_b2q = self.resolve_mult_0(true, false, slot_diff);
            use_template = true;
        } else {
            b2q_tiers = &self.b2q_tiers;
            eff_adj = 0;
            mult_0_b2q = self.mult_0_b2q as u64;
            use_template = false;
        }

        let mut remaining = net as u128;
        let mut output: u128 = 0;
        let pivot0_quote = (quote_reserve as u128 * mult_0_b2q as u128) / DIVISOR as u128;
        let pivot0_base = (pivot0_quote * decimal_scale as u128 * (1u128 << 48)) / fp_val as u128;

        if remaining <= pivot0_base {
            return (((remaining * fp_val as u128) >> 48) / decimal_scale as u128) as u64;
        }
        output += ((pivot0_base * fp_val as u128) >> 48) / decimal_scale as u128;
        remaining -= pivot0_base;

        for (i, tier) in b2q_tiers.iter().enumerate() {
            let adj_mult = (DIVISOR as i64 + tier.adj_idx as i64 * adj_const) as u128;
            let adj_fp = (fp_val as u128 * adj_mult) / DIVISOR as u128;
            let boundary_mult = if use_template {
                let q2b_mult = q2b_tiers[i].mult;
                let base = tier.mult.max(q2b_mult) as u64;
                if i == 0 {
                    base + eff_adj
                } else {
                    base
                }
            } else {
                tier.mult as u64
            };
            let tier_pivot_quote =
                (quote_reserve as u128 * boundary_mult as u128) / DIVISOR as u128;
            let tier_pivot_base = if adj_fp == 0 {
                0
            } else {
                (tier_pivot_quote * decimal_scale as u128 * (1u128 << 48)) / adj_fp
            };

            if remaining <= tier_pivot_base {
                output += ((remaining * adj_fp) >> 48) / decimal_scale as u128;
                return output as u64;
            }
            output += ((tier_pivot_base * adj_fp) >> 48) / decimal_scale as u128;
            remaining -= tier_pivot_base;
        }

        let last = &b2q_tiers[NUM_TIERS - 1];
        let last_adj_mult = (DIVISOR as i64 + last.adj_idx as i64 * adj_const) as u128;
        let last_adj_fp = (fp_val as u128 * last_adj_mult) / DIVISOR as u128;
        output += ((remaining * last_adj_fp) >> 48) / decimal_scale as u128;
        output as u64
    }

    fn q2b_tiered(&self, net: u64, penalty_active: bool, slot_diff: u64) -> u64 {
        let fp_val = self.fp_val;
        let decimal_scale = self.decimal_scale;
        let base_reserve = self.base_reserve;
        let adj_const = if self.adj_double_flag > 0 {
            self.adj_const as i64 * 2
        } else {
            self.adj_const as i64
        };

        if fp_val == 0 || decimal_scale == 0 {
            return 0;
        }

        let resets = self.cpi0_resets(slot_diff);
        let eff_adj: u64;
        let q2b_tiers_ref: &[TierEntry; NUM_TIERS];
        if penalty_active {
            q2b_tiers_ref = &self.q2b_tiers;
            eff_adj = 0;
        } else if resets {
            q2b_tiers_ref = &self.template_q2b_tiers;
            eff_adj =
                (self.effective_mult_0 as u64).saturating_sub(self.effective_mult_0_q2b as u64);
        } else {
            q2b_tiers_ref = &self.q2b_tiers;
            eff_adj = 0;
        }

        let mult_0 = self.resolve_mult_0(false, penalty_active, slot_diff);
        let eff_mult_q2b = self.effective_mult_0_q2b as u64;
        let has_inner_split = eff_mult_q2b < mult_0;

        let mut remaining = net as u128;
        let mut result: u128 = 0;
        let tier_start: usize;

        if has_inner_split {
            let inner_pivot_base = (base_reserve as u128 * eff_mult_q2b as u128) / DIVISOR as u128;
            let inner_pivot_quote =
                ((inner_pivot_base * fp_val as u128) >> 48) / decimal_scale as u128;
            if remaining <= inner_pivot_quote {
                let scaled = remaining * decimal_scale as u128;
                return ((scaled << 48) / fp_val as u128) as u64;
            }
            let inner_scaled = inner_pivot_quote * decimal_scale as u128;
            result += (inner_scaled << 48) / fp_val as u128;
            remaining -= inner_pivot_quote;

            let first_adj_idx = q2b_tiers_ref[0].adj_idx as i64;
            let middle_adj_mult = (DIVISOR as i64 + first_adj_idx * adj_const) as u128;
            let middle_adj_fp = (fp_val as u128 * middle_adj_mult) / DIVISOR as u128;
            let pivot0_base = (base_reserve as u128 * mult_0 as u128) / DIVISOR as u128;
            let middle_pivot_base = pivot0_base - inner_pivot_base;
            let middle_pivot_quote =
                ((middle_pivot_base * middle_adj_fp) >> 48) / decimal_scale as u128;

            let tier0_boundary_mult = q2b_tiers_ref[0].mult as u64 + eff_adj;
            let tier0_pivot_base =
                (base_reserve as u128 * tier0_boundary_mult as u128) / DIVISOR as u128;
            let tier0_pivot_quote =
                ((tier0_pivot_base * middle_adj_fp) >> 48) / decimal_scale as u128;
            let combined_quote = middle_pivot_quote + tier0_pivot_quote;

            if remaining <= combined_quote {
                let scaled = remaining * decimal_scale as u128;
                result += (scaled << 48) / middle_adj_fp;
                return result as u64;
            }
            let combined_scaled = combined_quote * decimal_scale as u128;
            result += (combined_scaled << 48) / middle_adj_fp;
            remaining -= combined_quote;
            tier_start = 1;
        } else {
            let pivot0_base = (base_reserve as u128 * mult_0 as u128) / DIVISOR as u128;
            let pivot0_quote = ((pivot0_base * fp_val as u128) >> 48) / decimal_scale as u128;
            if remaining <= pivot0_quote {
                let scaled = remaining * decimal_scale as u128;
                return ((scaled << 48) / fp_val as u128) as u64;
            }
            let pivot0_scaled = pivot0_quote * decimal_scale as u128;
            result += (pivot0_scaled << 48) / fp_val as u128;
            remaining -= pivot0_quote;
            tier_start = 0;
        }

        for (i, tier) in q2b_tiers_ref.iter().enumerate() {
            if i < tier_start {
                continue;
            }
            let adj_mult = (DIVISOR as i64 + tier.adj_idx as i64 * adj_const) as u128;
            let adj_fp = (fp_val as u128 * adj_mult) / DIVISOR as u128;
            let boundary_mult = if i == tier_start {
                tier.mult as u64 + eff_adj
            } else {
                tier.mult as u64
            };
            let tier_pivot_base = (base_reserve as u128 * boundary_mult as u128) / DIVISOR as u128;
            let tier_pivot_quote = if adj_fp == 0 {
                0
            } else {
                ((tier_pivot_base * adj_fp) >> 48) / decimal_scale as u128
            };

            if remaining <= tier_pivot_quote {
                if adj_fp == 0 {
                    return result as u64;
                }
                let scaled = remaining * decimal_scale as u128;
                result += (scaled << 48) / adj_fp;
                return result as u64;
            }
            if adj_fp > 0 {
                let tier_scaled = tier_pivot_quote * decimal_scale as u128;
                result += (tier_scaled << 48) / adj_fp;
            }
            remaining -= tier_pivot_quote;
        }

        let last = &q2b_tiers_ref[NUM_TIERS - 1];
        let last_adj_mult = (DIVISOR as i64 + last.adj_idx as i64 * adj_const) as u128;
        let last_adj_fp = (fp_val as u128 * last_adj_mult) / DIVISOR as u128;
        if last_adj_fp > 0 {
            let scaled = remaining * decimal_scale as u128;
            result += (scaled << 48) / last_adj_fp;
        }
        result as u64
    }

    fn get_quote_v3(&self, input_amount: u64, is_base_to_quote: bool, clock_slot: u64) -> u64 {
        let slot_diff = clock_slot.saturating_sub(self.pool_slot);
        let ds = if self.decimal_scale > 0 {
            self.decimal_scale
        } else {
            1
        };

        let stored_fee = if is_base_to_quote {
            self.v3_fee_b2q
        } else {
            self.v3_fee_q2b
        };

        let mut fee_field = if ds == 1 && self.v3_fee_step_size > 0 {
            self.v3_ds1_fee(input_amount, is_base_to_quote, slot_diff)
        } else if self.fee_step_threshold > 0 && self.v3_fee_step_size > 0 {
            stored_fee
        } else {
            stored_fee
        };

        let stale_pen = if is_base_to_quote {
            self.v3_fee_staleness_pen_b2q
        } else {
            self.v3_fee_staleness_pen_q2b
        };
        if stale_pen > 0 && slot_diff > 0 {
            fee_field += stale_pen * slot_diff / 24;
        }

        let fee = ceil_fee_v3(fee_field, input_amount);
        let net = input_amount.saturating_sub(fee);
        if net == 0 {
            return 0;
        }

        if is_base_to_quote {
            self.b2q_tiered_v3(net, clock_slot)
        } else {
            self.q2b_tiered_v3(net, clock_slot)
        }
    }

    fn v3_ds1_fee(&self, input_amount: u64, is_base_to_quote: bool, slot_diff: u64) -> u64 {
        let fp_128 = ((self.v3_fp_lo as u128) << 64) | (self.v3_fp_hi_u64 as u128);
        let threshold = self.v3_fee_step_threshold;

        let new_base = if is_base_to_quote {
            self.base_reserve.saturating_add(input_amount)
        } else {
            let est_output = if fp_128 > 0 {
                (((input_amount as u128) << 24) / fp_128) as u64
            } else {
                0
            };
            self.base_reserve.saturating_sub(est_output)
        };

        let floor_fee = if is_base_to_quote {
            self.v3_fee_floor_b2q
        } else {
            self.v3_fee_floor_q2b
        };
        let base_q2b = self.v3_fee_q2b_base;

        let mut fee = if new_base > threshold {
            let steps = (new_base - threshold) / self.v3_fee_step_size;
            if is_base_to_quote {
                floor_fee + steps * self.v3_fee_adj
            } else {
                base_q2b
            }
        } else if new_base < threshold {
            let steps = (threshold - new_base) / self.v3_fee_step_size;
            if is_base_to_quote {
                base_q2b
            } else {
                floor_fee + steps * self.v3_fee_adj
            }
        } else {
            floor_fee
        };

        if slot_diff > 0 {
            fee = fee.max(floor_fee);
        }

        fee
    }

    fn v3_pre_swap_tiers(&self, clock_slot: u64, is_b2q: bool) -> (u64, [TierEntry; NUM_TIERS]) {
        let stale = self.pool_slot == clock_slot
            && self.last_interaction_slot < clock_slot
            && clock_slot > 0;

        let (v3_mult, dyn_mult, dyn_tiers, tmpl_tiers) = if is_b2q {
            (
                self.v3_mult_0_b2q as u64,
                self.mult_0_b2q as u64,
                &self.b2q_tiers,
                &self.template_b2q_tiers,
            )
        } else {
            (
                self.v3_mult_0_q2b as u64,
                self.mult_0_q2b as u64,
                &self.q2b_tiers,
                &self.template_q2b_tiers,
            )
        };

        let tmpl_reset = self.template_mult_0 as u64;

        if stale {
            let mult_0 = if v3_mult > 0 { v3_mult } else { tmpl_reset };
            let excess = tmpl_reset.saturating_sub(if v3_mult > 0 { v3_mult } else { tmpl_reset });
            let mut tiers = *tmpl_tiers;
            if excess > 0 {
                tiers[0].mult += excess as u32;
            }
            (mult_0, tiers)
        } else {
            let (mult_0, clamp_excess) = if v3_mult > 0 && dyn_mult > v3_mult {
                (v3_mult, dyn_mult - v3_mult)
            } else {
                (dyn_mult, 0)
            };
            let mut tiers = *dyn_tiers;
            if clamp_excess > 0 {
                tiers[0].mult += clamp_excess as u32;
            }
            (mult_0, tiers)
        }
    }

    fn b2q_tiered_v3(&self, net: u64, clock_slot: u64) -> u64 {
        let fp_hi = (self.v3_fp_hi_u64 >> 32) as u128;
        let fp_lo = self.v3_fp_lo as u128;
        let fp_56 = (fp_lo << 32) | fp_hi;
        let quote_reserve = self.quote_reserve as u128;
        let decimal_scale = if self.decimal_scale > 0 {
            self.decimal_scale as u128
        } else {
            1
        };

        let (mult_0, b2q_tiers) = self.v3_pre_swap_tiers(clock_slot, true);
        let mult_0 = mult_0 as u128;

        let mut remaining = net as u128;
        let mut output: u128 = 0;

        let pivot_quote = (quote_reserve * mult_0) / DIVISOR as u128;
        let pq_scaled = pivot_quote * decimal_scale;
        let tier0_base = if fp_56 > 0 && pivot_quote > 0 {
            (pq_scaled << 56) / fp_56
        } else {
            0
        };

        if tier0_base > 0 {
            if remaining <= tier0_base {
                return (b2q_v3_core(remaining, fp_hi, fp_lo) / decimal_scale) as u64;
            }
            output += b2q_v3_core(tier0_base, fp_hi, fp_lo) / decimal_scale;
            remaining -= tier0_base;
        }

        let mut last_adj_fp_hi: u128 = fp_hi;
        let mut last_adj_fp_lo: u128 = fp_lo;
        for tier in b2q_tiers.iter() {
            let adj_val = if tier.adj_val != 0 {
                tier.adj_val as i64
            } else {
                tier.adj_idx as i64 * self.v3_adj_const as i64
            };
            let adj_factor = (FEE_DENOM_V3 as i64 + adj_val) as u128;
            let adj_fp_56 = (fp_56 * adj_factor) / FEE_DENOM_V3 as u128;
            let adj_fp_hi = adj_fp_56 & 0xFFFF_FFFF;
            let adj_fp_lo = adj_fp_56 >> 32;
            last_adj_fp_hi = adj_fp_hi;
            last_adj_fp_lo = adj_fp_lo;

            let tier_alloc = (quote_reserve * tier.mult as u128) / DIVISOR as u128;
            let tier_alloc_scaled = tier_alloc * decimal_scale;
            let tier_base = if adj_fp_56 > 0 && tier_alloc > 0 {
                (tier_alloc_scaled << 56) / adj_fp_56
            } else {
                0
            };

            if tier_base == 0 {
                continue;
            }

            if remaining <= tier_base {
                output += b2q_v3_core(remaining, adj_fp_hi, adj_fp_lo) / decimal_scale;
                return output as u64;
            }

            output += b2q_v3_core(tier_base, adj_fp_hi, adj_fp_lo) / decimal_scale;
            remaining -= tier_base;
        }

        output += b2q_v3_core(remaining, last_adj_fp_hi, last_adj_fp_lo) / decimal_scale;

        output as u64
    }

    fn q2b_tiered_v3(&self, net: u64, clock_slot: u64) -> u64 {
        let fp_hi = (self.v3_fp_hi_u64 >> 32) as u128;
        let fp_lo = self.v3_fp_lo as u128;
        let fp_hi_u64 = self.v3_fp_hi_u64;
        let fp_56 = (fp_lo << 32) | fp_hi;
        let base_reserve = self.base_reserve as u128;
        let decimal_scale = if self.decimal_scale > 0 {
            self.decimal_scale as u128
        } else {
            1
        };

        let (mult_0, q2b_tiers) = self.v3_pre_swap_tiers(clock_slot, false);
        let mult_0 = mult_0 as u128;

        let mut remaining_scaled = net as u128 * decimal_scale;
        let mut result: u128 = 0;

        if mult_0 > 0 {
            let pivot_base = (base_reserve * mult_0) / DIVISOR as u128;
            let boundary_q = b2q_v3_core(pivot_base, fp_hi, fp_lo);

            if remaining_scaled <= boundary_q {
                return q2b_v3_core(remaining_scaled, self.v3_fp_lo, fp_hi_u64) as u64;
            }

            result += q2b_v3_core(boundary_q, self.v3_fp_lo, fp_hi_u64);
            remaining_scaled -= boundary_q;
        }

        let mut last_adj_fp_hi_u64: u64 = fp_hi_u64;
        let mut last_adj_fp_lo_u64: u64 = self.v3_fp_lo;
        for tier in q2b_tiers.iter() {
            let adj_val = if tier.adj_val != 0 {
                tier.adj_val as i64
            } else {
                tier.adj_idx as i64 * self.v3_adj_const as i64
            };
            let adj_factor = (FEE_DENOM_V3 as i64 + adj_val) as u128;
            let adj_fp_56 = (fp_56 * adj_factor) / FEE_DENOM_V3 as u128;
            let adj_fp_hi = adj_fp_56 & 0xFFFF_FFFF;
            let adj_fp_lo = adj_fp_56 >> 32;
            let adj_fp_hi_u64 = (adj_fp_hi as u64) << 32;
            let adj_fp_lo_u64 = adj_fp_lo as u64;
            last_adj_fp_hi_u64 = adj_fp_hi_u64;
            last_adj_fp_lo_u64 = adj_fp_lo_u64;

            let tier_pivot_base = (base_reserve * tier.mult as u128) / DIVISOR as u128;
            if tier_pivot_base == 0 {
                continue;
            }

            let boundary_q = b2q_v3_core(tier_pivot_base, adj_fp_hi, adj_fp_lo);

            if remaining_scaled <= boundary_q {
                result += q2b_v3_core(remaining_scaled, adj_fp_lo_u64, adj_fp_hi_u64);
                return result as u64;
            }

            result += q2b_v3_core(boundary_q, adj_fp_lo_u64, adj_fp_hi_u64);
            remaining_scaled -= boundary_q;
        }

        result += q2b_v3_core(remaining_scaled, last_adj_fp_lo_u64, last_adj_fp_hi_u64);

        result as u64
    }
}

pub struct BisonFiReplay {
    state: BisonFiState,
    has_pool: bool,
    has_base_vault: bool,
    has_quote_vault: bool,
}

impl BisonFiReplay {
    pub fn new() -> Self {
        Self {
            state: BisonFiState::default(),
            has_pool: false,
            has_base_vault: false,
            has_quote_vault: false,
        }
    }
}

impl ProtocolReplay for BisonFiReplay {
    fn apply_update(&mut self, role: &str, data: &[u8], _slot: u64) {
        match role {
            "pool" => {
                if self.state.update_from_pool_data(data).is_ok() {
                    self.has_pool = true;
                }
            }
            "base_vault" => {
                if data.len() >= 72 {
                    self.state.base_amount = read_u64_le(data, 64);
                    self.has_base_vault = true;
                }
            }
            "quote_vault" => {
                if data.len() >= 72 {
                    self.state.quote_amount = read_u64_le(data, 64);
                    self.has_quote_vault = true;
                }
            }
            _ => {}
        }
    }

    fn quote_single(&self, input_amount: u64, direction: &str, slot: u64) -> Option<u64> {
        if !self.is_ready() || input_amount == 0 {
            return None;
        }
        let is_b2q = direction == "B2Q";
        let out = if self.state.version >= 3 {
            self.state.get_quote_v3(input_amount, is_b2q, slot)
        } else {
            self.state.get_quote(input_amount, is_b2q, slot)
        };
        if out > 0 {
            Some(out)
        } else {
            None
        }
    }

    fn compute_quotes(&self, slot: u64, tiers_usd: &[f64]) -> Vec<QuoteRow> {
        let mut rows = Vec::new();
        if !self.is_ready() {
            return rows;
        }
        for &usd in tiers_usd {
            let b2q_input = {
                let ds = self.state.decimal_scale as f64;
                if self.state.version >= 3 {
                    let rate = self.state.v3_fp_hi_u64 as f64 / (1u128 << 64) as f64
                        + self.state.v3_fp_lo as f64 / (1u128 << 24) as f64;
                    if rate > 0.0 {
                        (usd * 1e6 * ds / rate) as u64
                    } else {
                        0
                    }
                } else if self.state.fp_val > 0 {
                    ((usd * 1e6) as u128 * self.state.decimal_scale as u128 * (1u128 << 48)
                        / self.state.fp_val as u128) as u64
                } else {
                    0
                }
            };
            let b2q_output = self.state.get_quote(b2q_input, true, slot);
            rows.push(QuoteRow {
                direction: "B2Q".into(),
                input_amount: b2q_input,
                output_amount: b2q_output,
                input_usd_equiv: usd,
            });

            let q2b_input = (usd * 1_000_000.0) as u64;
            let q2b_output = self.state.get_quote(q2b_input, false, slot);
            rows.push(QuoteRow {
                direction: "Q2B".into(),
                input_amount: q2b_input,
                output_amount: q2b_output,
                input_usd_equiv: usd,
            });
        }
        rows
    }

    fn vault_balances(&self) -> Option<(u64, u64)> {
        if self.has_base_vault && self.has_quote_vault {
            Some((self.state.base_amount, self.state.quote_amount))
        } else {
            None
        }
    }

    fn is_ready(&self) -> bool {
        self.has_pool && self.has_base_vault && self.has_quote_vault
    }
}
