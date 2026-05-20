use super::{ProtocolReplay, QuoteRow};

const FEE_SCALE: i64 = 10_000_000;
const MAX_TABLE_ENTRIES: usize = 8;

const ORACLE_XOR_KEY: [u8; 168] = [
    0xFF, 0xAA, 0x55, 0xCC, 0x33, 0xF0, 0x0F, 0x99, 0x66, 0x11, 0xEE, 0x77, 0x88, 0x22, 0xDD, 0x44,
    0xBB, 0x55, 0xAA, 0xFF, 0x00, 0x33, 0xCC, 0x66, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x99, 0x77, 0x11, 0xEE, 0x22, 0xDD, 0x88, 0x44, 0x55, 0xAA, 0xFF, 0x00, 0xCC, 0x33, 0x66, 0x99,
    0x00, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE,
    0xFF, 0x00, 0xAA, 0x55, 0x33, 0xCC, 0x66, 0x99, 0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0,
    0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0,
    0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0,
    0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0,
    0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0,
    0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

const OFF_LAST_SLOT: usize = 0x010;
const OFF_TIER_TABLE_SET1: usize = 0x2E0;
const OFF_TIER_TABLE_SET2: usize = 0x368;
const OFF_T3_BOUNDS: usize = 0x3E8;
const OFF_T3_FEES: usize = 0x428;
const OFF_T3_COUNT: usize = 0x468;
const OFF_S4_BOUNDS: usize = 0x470;
const OFF_S4_FEES: usize = 0x4B0;
const OFF_S4_COUNT: usize = 0x4F0;
const OFF_S4_FLAG_RAW: usize = 0x500;
const OFF_EFFECTIVE_LEVEL: usize = 0x504;
const OFF_CORRECTION_THRESHOLD: usize = 0x508;
const OFF_VAULT_ADJ_MULT: usize = 0x510;
const OFF_DIVISOR2: usize = 0x518;
const OFF_MAX_FEE: usize = 0x530;
const OFF_ORACLE_THRESH: usize = 0x538;
const OFF_CORRECTION_CAP_NEG: usize = 0x520;
const OFF_CORRECTION_CAP_POS: usize = 0x528;
const OFF_POOL_ADJ_SLOT: usize = 0x548;
const OFF_STORED_ORACLE_0X18: usize = 0x2D0;
const MIN_POOL_DATA_SIZE: usize = 0x550;

#[inline(always)]
fn read_u64_le(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}
#[inline(always)]
fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

fn decrypt_oracle(data: &[u8]) -> [u8; 168] {
    let mut dec = [0u8; 168];
    let len = data.len().min(168);
    dec[..len].copy_from_slice(&data[..len]);
    for i in 0..168 {
        dec[i] ^= ORACLE_XOR_KEY[i];
    }
    dec
}

fn decode_base_spread(orderbook_data: &[u8]) -> i64 {
    if orderbook_data.len() < 92 {
        return 0;
    }
    let node_count = read_u32_le(orderbook_data, 88) as usize;
    let vec_bytes = 21 * node_count;
    let offset = 92 + vec_bytes + 4303;
    if offset + 4 > orderbook_data.len() {
        return 0;
    }
    let f32_val = f32::from_le_bytes(orderbook_data[offset..offset + 4].try_into().unwrap());
    f32_val as i64
}

fn table3_interp(boundaries: &[u64], fees: &[u64], count: usize, value: u64) -> u64 {
    if count == 0 {
        return 0;
    }
    let n = count - 1;
    if n == 0 {
        return fees[0];
    }
    let mut lower_bound = boundaries[0];
    if lower_bound >= value {
        return fees[0];
    }
    for i in 1..count {
        let upper_bound = boundaries[i];
        if upper_bound <= value {
            if i == n {
                return fees[n];
            }
            lower_bound = upper_bound;
            continue;
        }
        let fee_low = fees[i - 1] as i128;
        let fee_high = fees[i] as i128;
        let span = (upper_bound - lower_bound) as i128;
        if span == 0 {
            return fee_low as u64;
        }
        let result =
            ((fee_high - fee_low) * (value - lower_bound) as i128 + (span >> 1)) / span + fee_low;
        return result.max(0) as u64;
    }
    fees[n]
}

fn tier_interp(amount: u64, boundaries: &[u64; 7], fees: &[u64; 8]) -> u64 {
    if amount == 0 {
        return fees[0];
    }
    let mut lo_b: u64 = 0;
    for i in 0..7 {
        let hi_b = boundaries[i];
        if hi_b == 0 {
            return fees[i];
        }
        if amount <= hi_b {
            let lo_f = fees[i] as i128;
            let hi_f = fees[i + 1] as i128;
            let span = (hi_b - lo_b) as i128;
            if span == 0 {
                return lo_f as u64;
            }
            let result = lo_f + ((hi_f - lo_f) * (amount - lo_b) as i128 + (span >> 1)) / span;
            return result.max(0) as u64;
        }
        lo_b = hi_b;
    }
    fees[7]
}

#[inline(always)]
fn f32_add(a: i64, b: i64) -> i64 {
    ((a as f32) + (b as f32)) as i64
}

fn read_tier_table(data: &[u8], base_offset: usize) -> ([u64; 7], [u64; 8]) {
    let mut boundaries = [0u64; 7];
    let mut fees = [0u64; 8];
    for i in 0..7 {
        boundaries[i] = read_u64_le(data, base_offset + i * 8);
    }
    for i in 0..8 {
        fees[i] = read_u64_le(data, base_offset + 56 + i * 8);
    }
    (boundaries, fees)
}

#[derive(Debug, Clone, Default)]
struct SolFiV2State {
    base_amount: u64,
    quote_amount: u64,
    oracle_price: u64,
    divisor: u64,
    base_mult: u64,
    q2b_offset: u32,
    b2q_offset: u32,
    oracle_update_slot: u64,
    oracle_current_0x18: u64,
    base_spread: i64,
    pool_last_slot: u64,
    vault_adj_mult: u64,
    divisor2: u64,
    correction_threshold: u64,
    oracle_thresh: u64,
    correction_cap_neg: u64,
    correction_cap_pos: u64,
    max_fee: u64,
    effective_level: u32,
    s4_flag_raw: u32,
    pool_adj_slot: u64,
    pool_stored_oracle: u64,
    s4_bounds: [u64; MAX_TABLE_ENTRIES],
    s4_fees: [u64; MAX_TABLE_ENTRIES],
    s4_count: usize,
    s4_age_threshold: u64,
    bounds_set1: [u64; 7],
    fees_set1: [u64; 8],
    bounds_set2: [u64; 7],
    fees_set2: [u64; 8],
    t3_bounds: [u64; MAX_TABLE_ENTRIES],
    t3_fees: [u64; MAX_TABLE_ENTRIES],
    t3_count: usize,
}

impl SolFiV2State {
    fn update_from_pool_data(&mut self, data: &[u8]) -> Result<(), &'static str> {
        if data.len() < MIN_POOL_DATA_SIZE {
            return Err("SolFi V2 pool data too small");
        }
        self.pool_last_slot = read_u64_le(data, OFF_LAST_SLOT);
        self.vault_adj_mult = read_u64_le(data, OFF_VAULT_ADJ_MULT);
        self.divisor2 = read_u64_le(data, OFF_DIVISOR2);
        self.correction_threshold = read_u64_le(data, OFF_CORRECTION_THRESHOLD);
        self.oracle_thresh = read_u64_le(data, OFF_ORACLE_THRESH);
        self.correction_cap_neg = read_u64_le(data, OFF_CORRECTION_CAP_NEG);
        self.correction_cap_pos = read_u64_le(data, OFF_CORRECTION_CAP_POS);
        self.max_fee = read_u64_le(data, OFF_MAX_FEE);
        self.pool_adj_slot = read_u64_le(data, OFF_POOL_ADJ_SLOT);
        self.effective_level = read_u32_le(data, OFF_EFFECTIVE_LEVEL);
        self.s4_flag_raw = read_u32_le(data, OFF_S4_FLAG_RAW);
        self.pool_stored_oracle = read_u64_le(data, OFF_STORED_ORACLE_0X18);
        let (b1, f1) = read_tier_table(data, OFF_TIER_TABLE_SET1);
        self.bounds_set1 = b1;
        self.fees_set1 = f1;
        let (b2, f2) = read_tier_table(data, OFF_TIER_TABLE_SET2);
        self.bounds_set2 = b2;
        self.fees_set2 = f2;
        let t3_count = read_u64_le(data, OFF_T3_COUNT) as usize;
        self.t3_count = t3_count.min(MAX_TABLE_ENTRIES);
        for i in 0..self.t3_count {
            self.t3_bounds[i] = read_u64_le(data, OFF_T3_BOUNDS + i * 8);
            self.t3_fees[i] = read_u64_le(data, OFF_T3_FEES + i * 8);
        }
        let s4_count = read_u64_le(data, OFF_S4_COUNT) as usize;
        self.s4_count = s4_count.min(MAX_TABLE_ENTRIES);
        for i in 0..self.s4_count {
            self.s4_bounds[i] = read_u64_le(data, OFF_S4_BOUNDS + i * 8);
            self.s4_fees[i] = read_u64_le(data, OFF_S4_FEES + i * 8);
        }
        self.s4_age_threshold = if self.s4_count >= 2 {
            self.s4_bounds[1]
        } else {
            0
        };
        Ok(())
    }

    fn update_from_oracle_data(&mut self, oracle_data: &[u8]) {
        let dec = decrypt_oracle(oracle_data);
        self.oracle_price = u64::from_le_bytes(dec[0x08..0x10].try_into().unwrap());
        self.oracle_update_slot = u64::from_le_bytes(dec[0x10..0x18].try_into().unwrap());
        self.base_mult = u64::from_le_bytes(dec[0x20..0x28].try_into().unwrap());
        self.q2b_offset = u32::from_le_bytes(dec[0x38..0x3C].try_into().unwrap());
        self.b2q_offset = u32::from_le_bytes(dec[0x3C..0x40].try_into().unwrap());
        self.divisor = 10u64.pow(oracle_data[0] as u32 + 1);
        self.oracle_current_0x18 = read_u64_le(oracle_data, 0x18);
    }

    fn update_from_orderbook_data(&mut self, orderbook_data: &[u8]) {
        self.base_spread = decode_base_spread(orderbook_data);
    }

    fn get_quote(&self, input_amount: u64, is_base_to_quote: bool, cache_slot: u64) -> u64 {
        if self.oracle_price == 0 || self.divisor == 0 {
            return 0;
        }
        if self.base_amount == 0 || self.quote_amount == 0 {
            return 0;
        }
        if cache_slot.saturating_sub(self.pool_last_slot) > 1000 {
            return 0;
        }
        if cache_slot.saturating_sub(self.oracle_update_slot) > 1000 {
            return 0;
        }

        let (correction, _correction_mag) = self.compute_correction();
        let correction_negligible = correction.unsigned_abs() <= self.correction_threshold;
        let adj_oracle = ((self.oracle_price as i128) * (FEE_SCALE as i128 + correction as i128)
            / FEE_SCALE as i128) as u64;
        let spread_zeroed = correction_negligible && self.correction_threshold > self.oracle_thresh;
        let eff_base_spread = if spread_zeroed { 0 } else { self.base_spread };
        let effective_slot = cache_slot.max(self.pool_last_slot);
        let oracle_age = effective_slot.saturating_sub(self.oracle_update_slot);
        let t3_fee = table3_interp(&self.t3_bounds, &self.t3_fees, self.t3_count, oracle_age);
        let (b2q_skip, q2b_skip) = self.compute_oracle_skip(cache_slot);
        let q2b_s4 = self.compute_q2b_s4();
        let b2q_s4 = self.compute_b2q_s4();

        if is_base_to_quote {
            let use_oracle = if b2q_skip {
                self.oracle_price
            } else {
                adj_oracle
            };
            let raw = (input_amount as u128 * use_oracle as u128 / self.divisor as u128) as u64;
            let tw = tier_interp(raw, &self.bounds_set2, &self.fees_set2);
            let widening = self.compute_widening(tw, t3_fee, b2q_s4);
            let fee = self.compute_fee(self.b2q_offset, widening, eff_base_spread);
            let fee_factor = FEE_SCALE - fee;
            if fee_factor <= 0 {
                return 0;
            }
            let out = (raw as u128 * fee_factor as u128 / FEE_SCALE as u128) as u64;
            out.min(self.quote_amount)
        } else {
            let use_oracle = if q2b_skip {
                self.oracle_price
            } else {
                adj_oracle
            };
            let tw = tier_interp(input_amount, &self.bounds_set1, &self.fees_set1);
            let widening = self.compute_widening(tw, t3_fee, q2b_s4);
            let fee = self.compute_fee(self.q2b_offset, widening, eff_base_spread);
            let fee_factor = FEE_SCALE - fee;
            if fee_factor <= 0 {
                return 0;
            }
            if use_oracle == 0 {
                return 0;
            }
            let raw = (input_amount as u128 * self.divisor as u128 / use_oracle as u128) as u64;
            let out = (raw as u128 * fee_factor as u128 / FEE_SCALE as u128) as u64;
            out.min(self.base_amount)
        }
    }

    #[inline(always)]
    fn s4_fee_lookup(&self, level: u64) -> u64 {
        table3_interp(&self.s4_bounds, &self.s4_fees, self.s4_count, level)
    }

    fn compute_q2b_s4(&self) -> u64 {
        let flag = self.s4_flag_raw;
        if flag == 0 {
            return 0;
        }
        if self.pool_last_slot < self.oracle_update_slot {
            return 0;
        }
        if self.pool_stored_oracle != self.oracle_current_0x18 {
            return 0;
        }
        self.s4_fee_lookup(flag as u64)
    }

    fn compute_b2q_s4(&self) -> u64 {
        let eff = self.effective_level;
        if eff == 0 {
            return 0;
        }
        if self.pool_last_slot < self.oracle_update_slot {
            return 0;
        }
        if self.pool_stored_oracle != self.oracle_current_0x18 {
            return 0;
        }
        self.s4_fee_lookup(eff as u64)
    }

    fn compute_correction(&self) -> (i64, u64) {
        if self.divisor2 == 0 || self.divisor == 0 {
            return (0, 0);
        }
        let approx_quot =
            (self.base_amount as u128 * self.oracle_price as u128 / self.divisor as u128) as u64;
        let imbalance = self.quote_amount as i128 - approx_quot as i128;
        let mut magnitude = (imbalance.unsigned_abs() * self.vault_adj_mult as u128
            / (2 * self.divisor2 as u128)) as u64;
        if magnitude > self.oracle_thresh {
            let cap = if imbalance < 0 {
                self.correction_cap_neg
            } else {
                self.correction_cap_pos
            };
            magnitude = magnitude.min(cap);
        }
        let signed = if imbalance >= 0 {
            magnitude as i64
        } else {
            -(magnitude as i64)
        };
        (signed, magnitude)
    }

    fn compute_oracle_skip(&self, cache_slot: u64) -> (bool, bool) {
        let (correction, correction_mag) = self.compute_correction();
        let correction_negligible = correction.unsigned_abs() <= self.correction_threshold;
        let effective_slot = cache_slot.max(self.pool_last_slot);
        let pool_age = effective_slot - self.pool_last_slot;
        let pool_current = pool_age == 0;
        let pool_ahead = self.pool_last_slot > self.oracle_update_slot;
        let pool_behind = self.pool_last_slot < self.oracle_update_slot;
        let oracle_age = effective_slot.saturating_sub(self.oracle_update_slot);
        let s4_age_threshold = self.s4_age_threshold;
        let pool_oracle_gap = self.oracle_update_slot.saturating_sub(self.pool_last_slot);
        let fresh_sync = oracle_age == 0 && pool_oracle_gap <= s4_age_threshold;
        let correction_exceeds_strict = correction_mag > self.oracle_thresh;

        let b2q_skip = correction > 0
            && self.pool_last_slot <= self.pool_adj_slot
            && pool_current
            && (!pool_behind || correction_negligible || fresh_sync)
            && (correction_exceeds_strict || self.s4_flag_raw > 0 || fresh_sync || pool_ahead);

        let pool_behind_gap = if pool_behind {
            self.oracle_update_slot - self.pool_last_slot
        } else {
            0
        };
        let q2b_skip = correction < 0
            && self.pool_last_slot <= self.pool_adj_slot
            && pool_current
            && (pool_ahead
                || (self.effective_level > 0 && pool_behind_gap <= s4_age_threshold)
                || correction_negligible
                || fresh_sync);

        (b2q_skip, q2b_skip)
    }

    #[inline(always)]
    fn compute_widening(&self, tw: u64, t3_fee: u64, s4_offset: u64) -> u64 {
        s4_offset + (t3_fee as u128 * tw as u128 * self.base_mult as u128 / 1_000_000u128) as u64
    }

    #[inline(always)]
    fn compute_fee(&self, direction_offset: u32, widening: u64, eff_base_spread: i64) -> i64 {
        let fee_before = direction_offset as i64 + widening as i64;
        let fee = if fee_before > 999999 {
            fee_before.min(1_000_000)
        } else {
            f32_add(fee_before, eff_base_spread)
        };
        if fee > self.max_fee as i64 {
            self.max_fee as i64
        } else {
            fee
        }
    }
}

pub struct SolFiV2Replay {
    state: SolFiV2State,
    has_pool: bool,
    has_oracle: bool,
    has_base_vault: bool,
    has_quote_vault: bool,
}

impl SolFiV2Replay {
    pub fn new() -> Self {
        Self {
            state: {
                let mut s = SolFiV2State::default();
                s.base_spread = 1932;
                s
            },
            has_pool: false,
            has_oracle: false,
            has_base_vault: false,
            has_quote_vault: false,
        }
    }
}

impl ProtocolReplay for SolFiV2Replay {
    fn apply_update(&mut self, role: &str, data: &[u8], _slot: u64) {
        match role {
            "pool" => {
                if self.state.update_from_pool_data(data).is_ok() {
                    self.has_pool = true;
                }
            }
            "oracle" => {
                self.state.update_from_oracle_data(data);
                self.has_oracle = true;
            }
            "orderbook" => {
                self.state.update_from_orderbook_data(data);
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
            if self.state.oracle_price > 0 && self.state.divisor > 0 {
                let b2q_input = (usd * 1_000_000.0) as u128 * self.state.divisor as u128
                    / self.state.oracle_price as u128;
                let b2q_input = b2q_input as u64;
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
        if self.has_base_vault && self.has_quote_vault {
            Some((self.state.base_amount, self.state.quote_amount))
        } else {
            None
        }
    }

    fn is_ready(&self) -> bool {
        self.has_pool && self.has_oracle && self.has_base_vault && self.has_quote_vault
    }
}
