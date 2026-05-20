use super::{ProtocolReplay, QuoteRow};

const MAX_TIERS: usize = 10;
const CONST_1E7_RAW: u64 = 0x3E7AD7F29ABCAF48;
const CONST_ONE_RAW: u64 = 0x3FF0000000000000;

const OFF_ORACLE_BID_RAW: usize = 0xB60;
const OFF_ORACLE_ASK_RAW: usize = 0xB68;
const OFF_FEE_DENOM: usize = 0x388;
const OFF_FEE_DENOM_B: usize = 0xB90;
const OFF_FEE_EXTRA_B: usize = 0xBA8;
const OFF_FEE_THRESHOLD: usize = 0x360;
const OFF_FEE_THRESHOLD_2: usize = 0x368;
const OFF_MAX_SLOT_DIFF: usize = 0x358;
const OFF_POOL_SLOT: usize = 0xBC8;
const OFF_FEE_CAP_B2Q: usize = 0x14C8;
const OFF_FEE_CAP_Q2B: usize = 0x14CC;
const OFF_ADJ_GATE_SLOT: usize = 0x1918;
const OFF_EXTRA_FEE: usize = 0x358;
const OFF_EXTRA_A_TIER_TOTAL: usize = 0x1D8;
const OFF_EXTRA_A_SKIP_AMOUNT: usize = 0x1E0;

#[inline(always)]
fn read_u64_le(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}
#[inline(always)]
fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}
#[inline(always)]
fn read_i32_le(data: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

fn bpf_soft_mul(a_mant_53: u128, b_mant_64: u128, sign: u64, result_exp: u64) -> u64 {
    let full: u128 = a_mant_53 * b_mant_64;
    let top_bit = 128 - full.leading_zeros();
    let (result_mant, round_bits, exp_adj) = if top_bit >= 117 {
        ((full >> 64) & 0xFFFFFFFFFFFFF, (full >> 61) & 7, 1u64)
    } else {
        ((full >> 63) & 0xFFFFFFFFFFFFF, (full >> 60) & 7, 0u64)
    };
    let mut raw = (sign << 63) | ((result_exp + exp_adj) << 52) | (result_mant as u64);
    if round_bits > 4 {
        raw += 1;
    }
    raw
}

fn bpf_correction_mul(corr_raw: u64, oracle_raw: u64) -> u64 {
    let c_exp = (corr_raw >> 52) & 0x7FF;
    let c_mant = (corr_raw & 0xFFFFFFFFFFFFF) | (1u64 << 52);
    let o_exp = (oracle_raw >> 52) & 0x7FF;
    let o_sign = (oracle_raw >> 63) & 1;
    let o64 = ((oracle_raw & 0xFFFFFFFFFFFFF) << 11) | (1u64 << 63);
    bpf_soft_mul(c_mant as u128, o64 as u128, o_sign, c_exp + o_exp - 1023)
}

fn bpf_int_mul(factor_raw: u64, int_val: u64) -> u64 {
    if int_val == 0 {
        return 0;
    }
    let f_exp = (factor_raw >> 52) & 0x7FF;
    let f_mant = (factor_raw & 0xFFFFFFFFFFFFF) | (1u64 << 52);
    let bits = 64 - int_val.leading_zeros();
    let int64 = (int_val as u128) << (64 - bits);
    let int_exp = 1023 + bits as u64 - 1;
    bpf_soft_mul(f_mant as u128, int64, 0, f_exp + int_exp - 1023)
}

fn bpf_f64_to_int(raw: u64) -> i64 {
    if raw == 0 {
        return 0;
    }
    let sign = (raw >> 63) & 1;
    let exp = ((raw >> 52) & 0x7FF) as i64 - 1023;
    let mant = (raw & 0xFFFFFFFFFFFFF) | (1u64 << 52);
    if exp < 0 {
        return 0;
    }
    let result = if exp >= 52 {
        (mant as u128) << (exp as u32 - 52)
    } else {
        (mant >> (52 - exp as u32)) as u128
    };
    if sign != 0 {
        -(result as i64)
    } else {
        result as i64
    }
}

fn u64_to_f64_raw(val: u64) -> u64 {
    if val == 0 {
        return 0;
    }
    let bits = 64 - val.leading_zeros();
    let exp = 1023u64 + bits as u64 - 1;
    let mant = if bits <= 53 {
        (val << (53 - bits)) & 0xFFFFFFFFFFFFF
    } else {
        (val >> (bits - 53)) & 0xFFFFFFFFFFFFF
    };
    (exp << 52) | mant
}

fn bpf_fee_to_correction(fee_val: i64, is_subtract: bool) -> u64 {
    let abs_fee = fee_val.unsigned_abs();
    if abs_fee == 0 {
        return CONST_ONE_RAW;
    }
    let fee_raw = u64_to_f64_raw(abs_fee);
    let fee_rate_raw = bpf_correction_mul(fee_raw, CONST_1E7_RAW);
    let fee_rate = f64::from_bits(fee_rate_raw);
    if is_subtract {
        (1.0f64 - fee_rate).to_bits()
    } else {
        (1.0f64 + fee_rate).to_bits()
    }
}

fn compute_single(amount_in: u64, oracle_raw: u64, fee_val: i64, fee_cap: i32) -> i64 {
    let clamped = fee_val.min(fee_cap as i64);
    let corr_raw = if clamped >= 0 {
        bpf_fee_to_correction(clamped, false)
    } else {
        bpf_fee_to_correction(-clamped, true)
    };
    let adj_raw = bpf_correction_mul(corr_raw, oracle_raw);
    let result_raw = bpf_int_mul(adj_raw, amount_in);
    bpf_f64_to_int(result_raw)
}

fn onset_multiplier(ss: u64, _onset_low: u32, onset_high: u32, ft: u64, ft2: u64) -> u64 {
    if onset_high == 0 {
        return ss * ft;
    }
    let n = onset_high.saturating_sub(_onset_low) as u64;
    ss.min(n) * ft + ss.saturating_sub(n) * ft2
}

#[derive(Debug, Clone)]
struct ZeroFiState {
    oracle_ask_raw: u64,
    oracle_bid_raw: u64,
    fee_denom: u64,
    fee_denom_b: u64,
    fee_extra_b: u64,
    fee_threshold: u64,
    fee_threshold_2: u64,
    max_slot_diff: u32,
    tier0_param: u32,
    tier_params: [u32; MAX_TIERS],
    tier_onsets: [(u32, u32); MAX_TIERS],
    tier_count: usize,
    tier_out_alt: [u64; MAX_TIERS],
    tier_out_adj: [u64; MAX_TIERS],
    tier_out_alt_count: usize,
    q2b_breakpoints: [u64; MAX_TIERS],
    q2b_adj: [u64; MAX_TIERS],
    q2b_count: usize,
    fee_cap_b2q: i32,
    fee_cap_q2b: i32,
    pool_slot: u64,
    adj_gate_slot: u64,
    extra_a_fee: i32,
    extra_a_tier_total: u64,
    extra_a_skip_amount: u64,
    extra_b_fee: i32,
    base_vault_balance: u64,
    quote_vault_balance: u64,
}

impl Default for ZeroFiState {
    fn default() -> Self {
        Self {
            oracle_ask_raw: 0,
            oracle_bid_raw: 0,
            fee_denom: 0,
            fee_denom_b: 0,
            fee_extra_b: 0,
            fee_threshold: 0,
            fee_threshold_2: 0,
            max_slot_diff: 0,
            tier0_param: 0,
            tier_params: [0; MAX_TIERS],
            tier_onsets: [(0, 0); MAX_TIERS],
            tier_count: 0,
            tier_out_alt: [0; MAX_TIERS],
            tier_out_adj: [0; MAX_TIERS],
            tier_out_alt_count: 0,
            q2b_breakpoints: [0; MAX_TIERS],
            q2b_adj: [0; MAX_TIERS],
            q2b_count: 0,
            fee_cap_b2q: 0,
            fee_cap_q2b: 0,
            pool_slot: 0,
            adj_gate_slot: 0,
            extra_a_fee: 0,
            extra_a_tier_total: 0,
            extra_a_skip_amount: 0,
            extra_b_fee: 0,
            base_vault_balance: 0,
            quote_vault_balance: 0,
        }
    }
}

impl ZeroFiState {
    fn update_from_pool_data(&mut self, data: &[u8]) {
        if data.len() <= 0xB70 {
            return;
        }
        self.oracle_bid_raw = read_u64_le(data, OFF_ORACLE_BID_RAW);
        self.oracle_ask_raw = read_u64_le(data, OFF_ORACLE_ASK_RAW);
        if data.len() > 0x390 {
            self.fee_denom = read_u64_le(data, OFF_FEE_DENOM);
        }
        if data.len() > 0xB98 {
            self.fee_denom_b = read_u64_le(data, OFF_FEE_DENOM_B);
        }
        if data.len() > 0xBB0 {
            self.fee_extra_b = read_u64_le(data, OFF_FEE_EXTRA_B);
        }
        if data.len() > 0x368 {
            self.fee_threshold = read_u64_le(data, OFF_FEE_THRESHOLD);
        }
        if data.len() > 0x370 {
            self.fee_threshold_2 = read_u64_le(data, OFF_FEE_THRESHOLD_2);
        }
        if data.len() > 0x35C {
            self.max_slot_diff = read_u32_le(data, OFF_MAX_SLOT_DIFF);
        }
        if data.len() > 0x124 {
            self.tier0_param = read_u32_le(data, 0x120);
        }
        self.tier_count = 0;
        for i in 0..MAX_TIERS {
            let off_in = 0x110 + i * 0x38;
            if data.len() <= off_in + 8 {
                break;
            }
            let t_in = read_u64_le(data, off_in);
            if t_in == 0 {
                break;
            }
            let off_param = 0x120 + i * 0x38;
            if data.len() > off_param + 4 {
                self.tier_params[i] = read_u32_le(data, off_param);
            }
            let off_onset = 0x118 + i * 0x38;
            if data.len() > off_onset + 8 {
                let raw = read_u64_le(data, off_onset);
                self.tier_onsets[i] =
                    ((raw & 0xFFFFFFFF) as u32, ((raw >> 32) & 0xFFFFFFFF) as u32);
            }
            self.tier_count = i + 1;
        }
        self.tier_out_alt_count = 0;
        for i in 0..MAX_TIERS {
            let off_alt = 0xFA8 + i * 0x40;
            let off_adj = 0xFB0 + i * 0x40;
            if data.len() > off_alt + 8 {
                self.tier_out_alt[i] = read_u64_le(data, off_alt);
                self.tier_out_alt_count = i + 1;
            }
            if data.len() > off_adj + 8 {
                self.tier_out_adj[i] = read_u64_le(data, off_adj);
            }
        }
        self.q2b_count = 0;
        for i in 0..MAX_TIERS {
            let off_q2b = 0x1228 + i * 0x40;
            let off_adj = 0x1230 + i * 0x40;
            if data.len() <= off_q2b + 8 {
                break;
            }
            let val = read_u64_le(data, off_q2b);
            if val == 0 {
                break;
            }
            self.q2b_breakpoints[i] = val;
            if data.len() > off_adj + 8 {
                self.q2b_adj[i] = read_u64_le(data, off_adj);
            }
            self.q2b_count = i + 1;
        }
        if data.len() > 0x14CC {
            self.fee_cap_b2q = read_i32_le(data, OFF_FEE_CAP_B2Q);
        }
        if data.len() > 0x14D0 {
            self.fee_cap_q2b = read_i32_le(data, OFF_FEE_CAP_Q2B);
        }
        if data.len() > 0xBD0 {
            self.pool_slot = read_u64_le(data, OFF_POOL_SLOT);
        }
        if data.len() > 0x1920 {
            self.adj_gate_slot = read_u64_le(data, OFF_ADJ_GATE_SLOT);
        }
    }

    fn update_from_extra_a(&mut self, data: &[u8]) {
        if data.len() > OFF_EXTRA_FEE + 4 {
            self.extra_a_fee = read_i32_le(data, OFF_EXTRA_FEE);
        }
        if data.len() > OFF_EXTRA_A_TIER_TOTAL + 8 {
            self.extra_a_tier_total = read_u64_le(data, OFF_EXTRA_A_TIER_TOTAL);
        }
        if data.len() > OFF_EXTRA_A_SKIP_AMOUNT + 8 {
            self.extra_a_skip_amount = read_u64_le(data, OFF_EXTRA_A_SKIP_AMOUNT);
        }
    }

    fn update_from_extra_b(&mut self, data: &[u8]) {
        if data.len() > OFF_EXTRA_FEE + 4 {
            self.extra_b_fee = read_i32_le(data, OFF_EXTRA_FEE);
        }
    }

    fn get_quote(&self, amount_in: u64, is_base_to_quote: bool, clock_slot: u64) -> u64 {
        if amount_in == 0 {
            return 0;
        }
        let slot_diff = if clock_slot > 0 && self.pool_slot > 0 {
            clock_slot.saturating_sub(self.pool_slot)
        } else {
            0
        };
        if self.max_slot_diff > 0 && slot_diff >= self.max_slot_diff as u64 {
            return 0;
        }
        let (oracle_raw, fee_diff, fee_cap) = if is_base_to_quote {
            (
                self.oracle_bid_raw,
                self.extra_a_fee as i64 - self.extra_b_fee as i64,
                self.fee_cap_b2q,
            )
        } else {
            (
                self.oracle_ask_raw,
                self.extra_b_fee as i64 - self.extra_a_fee as i64,
                self.fee_cap_q2b,
            )
        };
        self.multi_tier_quote(
            amount_in,
            oracle_raw,
            is_base_to_quote,
            slot_diff,
            fee_diff,
            fee_cap,
            clock_slot,
        )
    }

    fn tier_half_and_combined(&self, tp: u32, tier_idx: usize, slot_diff: u64) -> (u64, u64) {
        let bh = self.fee_denom_b * tp as u64 / 2000;
        let mut combined = bh + self.fee_extra_b;
        let (ol, oh) = if tier_idx < self.tier_count {
            self.tier_onsets[tier_idx]
        } else if self.tier_count > 0 {
            self.tier_onsets[0]
        } else {
            (0, 0)
        };
        let mut om = 0u64;
        if ol != 0xFFFFFFFF && slot_diff > ol as u64 {
            let ss = slot_diff - ol as u64;
            om = onset_multiplier(ss, ol, oh, self.fee_threshold, self.fee_threshold_2);
        }
        if om > 0 {
            combined = combined * (1_000_000 + om) / 1_000_000;
        }
        let half = self.fee_denom * combined / 1000;
        (half, combined)
    }

    fn compute_half_token(&self, slot_diff: u64) -> u64 {
        if self.fee_threshold >= 500_000 {
            let mut half = self.fee_denom * self.fee_denom_b / 2000;
            if slot_diff >= 2 {
                half = half * (1_000_000 + (slot_diff - 1) * self.fee_threshold_2) / 1_000_000;
            }
            return half;
        }
        if self.tier_count > 0 {
            let (ol, oh) = self.tier_onsets[0];
            if slot_diff > ol as u64 {
                let bh = self.fee_denom_b * self.tier0_param as u64 / 2000;
                let ss = slot_diff - ol as u64;
                let mult = onset_multiplier(ss, ol, oh, self.fee_threshold, self.fee_threshold_2);
                let staled = bh * (1_000_000 + mult) / 1_000_000;
                return self.fee_denom * staled / self.tier0_param as u64;
            }
        }
        let bh = self.fee_denom_b * self.tier0_param as u64 / 2000;
        self.fee_denom * bh / self.tier0_param as u64
    }

    #[allow(clippy::too_many_arguments)]
    fn multi_tier_quote(
        &self,
        amount_in: u64,
        oracle_raw: u64,
        is_b2q: bool,
        slot_diff: u64,
        fee_diff: i64,
        fee_cap: i32,
        clock_slot: u64,
    ) -> u64 {
        let vault_cap = if is_b2q {
            self.quote_vault_balance
        } else {
            self.base_vault_balance
        };
        let is_sol = self.fee_threshold >= 500_000;
        let is_stable = self.fee_denom == self.fee_denom_b;
        let has_extra = self.fee_extra_b > 0;
        let adj_active = clock_slot > 0 && self.adj_gate_slot == clock_slot;

        let (breakpoints, bp_count) = if is_b2q {
            let mut bp = self.tier_out_alt;
            let count = self.tier_out_alt_count;
            if adj_active {
                for j in 0..count {
                    bp[j] = bp[j].saturating_sub(self.tier_out_adj[j]);
                }
            }
            (bp, count)
        } else {
            let mut bp = self.q2b_breakpoints;
            let count = self.q2b_count;
            if adj_active {
                for j in 0..count {
                    bp[j] = bp[j].saturating_sub(self.q2b_adj[j]);
                }
            }
            (bp, count)
        };

        let tier_input = if is_b2q && self.extra_a_tier_total > 0 {
            let depth = self
                .extra_a_tier_total
                .saturating_sub(self.base_vault_balance);
            if depth > 0 && depth < amount_in {
                depth
            } else {
                amount_in
            }
        } else {
            amount_in
        };

        let obs_a = self.extra_a_fee as i64;
        let obs_b = self.extra_b_fee as i64;
        let (obs_a_val, obs_b_val, mut obs_a_budget, mut obs_b_budget): (i64, i64, i64, i64);

        if is_b2q {
            if obs_a <= 0 && obs_b < 0 {
                let (a, b) = (-obs_b, obs_a);
                obs_a_val = a;
                obs_b_val = b;
                obs_a_budget =
                    (self.quote_vault_balance as i64 - self.extra_a_skip_amount as i64).max(0);
                obs_b_budget = 0;
            } else {
                obs_a_val = obs_a;
                obs_b_val = obs_b;
                obs_a_budget = (self.extra_a_skip_amount as i64)
                    .saturating_sub(self.base_vault_balance as i64)
                    .max(0);
                obs_b_budget = self.quote_vault_balance as i64;
            }
        } else {
            obs_a_val = -obs_a;
            obs_b_val = obs_b;
            obs_a_budget = (self.base_vault_balance as i64)
                .saturating_sub(self.extra_a_skip_amount as i64)
                .max(0);
            obs_b_budget = (self.extra_a_skip_amount as i64)
                .saturating_sub(self.quote_vault_balance as i64)
                .max(0);
        }

        let mut obs_a_active = obs_a_val > 0 && obs_a_budget > 0;
        let mut obs_b_active = obs_b_val > 0 && obs_b_budget > 0;
        let mut fee_diff_m = fee_diff;
        let remaining_start = if is_b2q { tier_input } else { amount_in };
        let mut remaining = remaining_start;
        let mut total_output: i64 = 0;
        let mut tier_idx = 0usize;

        for i in 0..bp_count {
            let bp = breakpoints[i];
            if bp == 0 {
                tier_idx = i + 1;
                continue;
            }
            if remaining <= bp {
                break;
            }
            remaining -= bp;
            let tp = if i < self.tier_count {
                self.tier_params[i]
            } else {
                0
            };
            let (mut half, _) = self.tier_half_and_combined(tp, i, slot_diff);
            if !is_b2q && i == 0 && adj_active && !is_stable && !is_sol && !has_extra {
                half = self.compute_half_token(slot_diff);
            }
            if obs_a_active {
                if is_b2q {
                    obs_a_budget -= bp as i64;
                } else {
                    let seg_raw = compute_single(bp, oracle_raw, 0, fee_cap);
                    obs_a_budget -= seg_raw;
                }
                if obs_a_budget <= 0 {
                    fee_diff_m -= obs_a_val;
                    fee_diff_m = fee_diff_m.max(-0x80000000).min(0x7FFFFFFF);
                    obs_a_active = false;
                }
            }
            if obs_b_active {
                if is_b2q {
                    let seg_raw = compute_single(bp, oracle_raw, 0, fee_cap);
                    obs_b_budget -= seg_raw;
                } else {
                    obs_b_budget -= bp as i64;
                }
                if obs_b_budget <= 0 {
                    fee_diff_m -= obs_b_val;
                    fee_diff_m = fee_diff_m.max(-0x80000000).min(0x7FFFFFFF);
                    obs_b_active = false;
                }
            }
            let fee = fee_diff_m - half as i64;
            total_output += compute_single(bp, oracle_raw, fee, fee_cap);
            tier_idx = i + 1;
        }

        if remaining > 0 && tier_idx < self.tier_count {
            let tp_rem = self.tier_params[tier_idx];
            if obs_a_active {
                if is_b2q {
                    obs_a_budget -= remaining as i64;
                } else {
                    let seg_raw = compute_single(remaining, oracle_raw, 0, fee_cap);
                    obs_a_budget -= seg_raw;
                }
                if obs_a_budget <= 0 {
                    fee_diff_m -= obs_a_val;
                    fee_diff_m = fee_diff_m.max(-0x80000000).min(0x7FFFFFFF);
                }
            }
            if obs_b_active {
                if is_b2q {
                    let seg_raw = compute_single(remaining, oracle_raw, 0, fee_cap);
                    obs_b_budget -= seg_raw;
                } else {
                    obs_b_budget -= remaining as i64;
                }
                if obs_b_budget <= 0 {
                    fee_diff_m -= obs_b_val;
                    fee_diff_m = fee_diff_m.max(-0x80000000).min(0x7FFFFFFF);
                }
            }
            let half_rem = if !is_b2q && has_extra && self.tier0_param > 0 && !is_stable {
                let bh_rem = self.fee_denom_b * tp_rem as u64 / 2000;
                let mut combined_r = bh_rem + self.fee_extra_b;
                if slot_diff > 2 {
                    let ss = slot_diff - 2;
                    let (ol0, oh0) = if self.tier_count > 0 {
                        self.tier_onsets[0]
                    } else {
                        (2, 2)
                    };
                    let mult =
                        onset_multiplier(ss, ol0, oh0, self.fee_threshold, self.fee_threshold_2);
                    combined_r = combined_r * (1_000_000 + mult) / 1_000_000;
                }
                self.fee_denom * combined_r / 1000
            } else {
                let (h, _) = self.tier_half_and_combined(tp_rem, tier_idx, slot_diff);
                h
            };
            let fee_rem = fee_diff_m - half_rem as i64;
            total_output += compute_single(remaining, oracle_raw, fee_rem, fee_cap);
        }

        let out = total_output.max(0) as u64;
        out.min(vault_cap)
    }
}

pub struct ZeroFiReplay {
    state: ZeroFiState,
    has_pool: bool,
    has_base_vault: bool,
    has_quote_vault: bool,
}

impl ZeroFiReplay {
    pub fn new() -> Self {
        Self {
            state: ZeroFiState::default(),
            has_pool: false,
            has_base_vault: false,
            has_quote_vault: false,
        }
    }
}

impl ProtocolReplay for ZeroFiReplay {
    fn apply_update(&mut self, role: &str, data: &[u8], _slot: u64) {
        match role {
            "pool" => {
                self.state.update_from_pool_data(data);
                self.has_pool = true;
            }
            "extra_a" => {
                self.state.update_from_extra_a(data);
            }
            "extra_b" => {
                self.state.update_from_extra_b(data);
            }
            "base_vault" => {
                if data.len() >= 72 {
                    self.state.base_vault_balance = read_u64_le(data, 64);
                    self.has_base_vault = true;
                }
            }
            "quote_vault" => {
                if data.len() >= 72 {
                    self.state.quote_vault_balance = read_u64_le(data, 64);
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
        let out = self.state.get_quote(input_amount, is_b2q, slot);
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
            let bid_f64 = f64::from_bits(self.state.oracle_bid_raw);
            if bid_f64 > 0.0 {
                let b2q_input = (usd * 1e6 / bid_f64) as u64;
                let b2q_output = self.state.get_quote(b2q_input, true, slot);
                rows.push(QuoteRow {
                    direction: "B2Q".into(),
                    input_amount: b2q_input,
                    output_amount: b2q_output,
                    input_usd_equiv: usd,
                });
            }

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
        if self.has_base_vault
            && self.has_quote_vault
            && self.state.base_vault_balance > 0
            && self.state.quote_vault_balance > 0
        {
            Some((
                self.state.base_vault_balance,
                self.state.quote_vault_balance,
            ))
        } else {
            None
        }
    }

    fn is_ready(&self) -> bool {
        self.has_pool && self.has_base_vault && self.has_quote_vault
    }
}
