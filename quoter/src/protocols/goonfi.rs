use super::{ProtocolReplay, QuoteRow};

const SCALE: u64 = 1_000_000;
const NUM_TIERS: usize = 10;

const OFF_POSITION_B2Q: usize = 0x10;
const OFF_POSITION_Q2B: usize = 0x18;
const OFF_SLOT_A: usize = 0x20;
const OFF_SLOT_B: usize = 0x28;
const OFF_VAULT_A_BALANCE: usize = 0x40;
const OFF_SPREAD_B2Q: usize = 0xF0;
const OFF_SPREAD_Q2B: usize = 0x140;
const OFF_FEE_BASE: usize = 0x190;
const OFF_CUM_BOUNDARIES_B2Q: usize = 0x5C0;
const OFF_CUM_BOUNDARIES_Q2B: usize = 0x610;
const OFF_VAULT_THRESHOLD: usize = 0x660;
const OFF_VAULT_SCALE: usize = 0x670;
const OFF_STALE_VAULT_THRESHOLD: usize = 0x678;
const OFF_VAULT_SCALE_HIGH: usize = 0x680;
const OFF_VAULT_SCALE_PW: usize = 0x688;
const OFF_VAULT_REFERENCE: usize = 0x690;
const OFF_FEE_MULTIPLIER: usize = 0x699;
const OFF_MINT_A_DECIMALS: usize = 0x7BC;
const OFF_MINT_B_DECIMALS: usize = 0x7BD;
const MIN_MARKET_DATA_SIZE: usize = 0x800;

const OFF_ORACLE_BID: usize = 0x00;
const OFF_ORACLE_ASK: usize = 0x08;
const OFF_ORACLE_SLOT: usize = 0x10;
const OFF_ORACLE_SPREAD_PARAM: usize = 0x14;
const MIN_ORACLE_DATA_SIZE: usize = 0x18;

#[inline(always)]
fn read_u64_le(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}
#[inline(always)]
fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

#[derive(Debug, Clone)]
struct GoonFiState {
    mint_a_decimals: u8,
    #[allow(dead_code)]
    mint_b_decimals: u8,
    price_scale: u64,
    position_b2q: u64,
    position_q2b: u64,
    slot_a: u64,
    slot_b: u64,
    fee_multiplier: u64,
    fee_base: u64,
    spread_b2q: [u64; NUM_TIERS],
    spread_q2b: [u64; NUM_TIERS],
    cum_boundaries_b2q: [u64; NUM_TIERS],
    cum_boundaries_q2b: [u64; NUM_TIERS],
    vault_threshold: u64,
    vault_scale: u64,
    stale_vault_threshold: u64,
    vault_scale_high: u64,
    vault_scale_pw: u64,
    vault_reference: u64,
    vault_a_balance: u64,
    bid_price: u64,
    ask_price: u64,
    oracle_slot: u64,
    spread_param: u64,

    base_amount: u64,
    quote_amount: u64,
}

impl Default for GoonFiState {
    fn default() -> Self {
        Self {
            mint_a_decimals: 0,
            mint_b_decimals: 0,
            price_scale: 1,
            position_b2q: 0,
            position_q2b: 0,
            slot_a: 0,
            slot_b: 0,
            fee_multiplier: 0,
            fee_base: 0,
            spread_b2q: [0; NUM_TIERS],
            spread_q2b: [0; NUM_TIERS],
            cum_boundaries_b2q: [0; NUM_TIERS],
            cum_boundaries_q2b: [0; NUM_TIERS],
            vault_threshold: 0,
            vault_scale: 0,
            stale_vault_threshold: 0,
            vault_scale_high: 0,
            vault_scale_pw: 0,
            vault_reference: 0,
            vault_a_balance: 0,
            bid_price: 0,
            ask_price: 0,
            oracle_slot: 0,
            spread_param: 0,
            base_amount: 0,
            quote_amount: 0,
        }
    }
}

impl GoonFiState {
    fn update_from_market_data(&mut self, data: &[u8]) -> Result<(), &'static str> {
        if data.len() < MIN_MARKET_DATA_SIZE {
            return Err("GoonFi market data too small");
        }
        self.mint_a_decimals = data[OFF_MINT_A_DECIMALS];
        self.mint_b_decimals = data[OFF_MINT_B_DECIMALS];
        self.price_scale = 10u64.pow(self.mint_a_decimals as u32);
        self.position_b2q = read_u64_le(data, OFF_POSITION_B2Q);
        self.position_q2b = read_u64_le(data, OFF_POSITION_Q2B);
        self.slot_a = read_u64_le(data, OFF_SLOT_A);
        self.slot_b = read_u64_le(data, OFF_SLOT_B);
        self.fee_multiplier = data[OFF_FEE_MULTIPLIER] as u64;
        self.fee_base = read_u64_le(data, OFF_FEE_BASE);
        for i in 0..NUM_TIERS {
            self.spread_b2q[i] = read_u64_le(data, OFF_SPREAD_B2Q + i * 8);
            self.spread_q2b[i] = read_u64_le(data, OFF_SPREAD_Q2B + i * 8);
            self.cum_boundaries_b2q[i] = read_u64_le(data, OFF_CUM_BOUNDARIES_B2Q + i * 8);
            self.cum_boundaries_q2b[i] = read_u64_le(data, OFF_CUM_BOUNDARIES_Q2B + i * 8);
        }
        self.vault_threshold = read_u64_le(data, OFF_VAULT_THRESHOLD);
        self.vault_scale = read_u64_le(data, OFF_VAULT_SCALE);
        self.stale_vault_threshold = read_u64_le(data, OFF_STALE_VAULT_THRESHOLD);
        self.vault_scale_high = read_u64_le(data, OFF_VAULT_SCALE_HIGH);
        self.vault_scale_pw = read_u64_le(data, OFF_VAULT_SCALE_PW);
        self.vault_reference = read_u64_le(data, OFF_VAULT_REFERENCE);

        self.vault_a_balance = read_u64_le(data, OFF_VAULT_A_BALANCE);
        Ok(())
    }

    fn update_from_oracle_data(&mut self, data: &[u8]) -> Result<(), &'static str> {
        if data.len() < MIN_ORACLE_DATA_SIZE {
            return Err("GoonFi oracle data too small");
        }
        self.bid_price = read_u64_le(data, OFF_ORACLE_BID);
        self.ask_price = read_u64_le(data, OFF_ORACLE_ASK);
        self.oracle_slot = read_u32_le(data, OFF_ORACLE_SLOT) as u64;
        self.spread_param = read_u32_le(data, OFF_ORACLE_SPREAD_PARAM) as u64;
        Ok(())
    }

    fn get_quote(&self, amount_in: u64, is_base_to_quote: bool, clock_slot: u64) -> u64 {
        if amount_in == 0 {
            return 0;
        }
        let base_price = if is_base_to_quote {
            self.bid_price
        } else {
            self.ask_price
        };
        let sp = self.spread_param;
        let spread = if is_base_to_quote {
            &self.spread_b2q
        } else {
            &self.spread_q2b
        };
        let bounds = if is_base_to_quote {
            &self.cum_boundaries_b2q
        } else {
            &self.cum_boundaries_q2b
        };
        let ps = self.price_scale;
        let vs = self.vault_scale;
        let vs_pw = self.vault_scale_pw;
        let fb = self.fee_base;
        let eq = vs == vs_pw;

        let (signed_d, abs_d) = if vs > 0 && self.base_amount > 0 {
            let sd = self.vault_reference as i128 - self.base_amount as i128;
            (sd, sd.unsigned_abs() as u64)
        } else {
            (0i128, 0u64)
        };

        let sa = staleness(clock_slot, self.slot_a);
        let sb = staleness(clock_slot, self.slot_b);
        let bstart: usize = if bounds[0] == 0 { 1 } else { 0 };

        let output = if is_base_to_quote {
            self.quote_b2q(
                amount_in, base_price, sp, spread, bounds, ps, vs_pw, fb, eq, signed_d, abs_d, sa,
                sb, bstart, clock_slot,
            )
        } else {
            self.quote_q2b(
                amount_in, base_price, sp, spread, bounds, ps, fb, eq, signed_d, abs_d, sa, sb,
                bstart, clock_slot,
            )
        };
        if output < 0 {
            0
        } else {
            output as u64
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn quote_b2q(
        &self,
        input_amount: u64,
        base_price: u64,
        sp: u64,
        spread: &[u64; NUM_TIERS],
        bounds: &[u64; NUM_TIERS],
        ps: u64,
        vs_pw: u64,
        fb: u64,
        eq: bool,
        signed_d: i128,
        abs_d: u64,
        sa: u64,
        sb: u64,
        bstart: usize,
        clock_slot: u64,
    ) -> i128 {
        if sa == 0 {
            let corr = if eq {
                simple_vault_corr(abs_d, signed_d, vs_pw)
            } else {
                stale_vault_corr(abs_d, signed_d, self, false).0
            };
            let adj = adj_price(base_price, corr);
            let old_p = self.position_b2q;
            let new_p = self.position_b2q.wrapping_add(input_amount);
            if !eq && spread[0] == 0 && new_p <= bounds[bstart] {
                let raw_adj =
                    clock_slot as i128 - self.oracle_slot as i128 - self.fee_multiplier as i128;
                let adj_st = 1i128.max(raw_adj);
                let stale_fee = 1i128 + fb as i128 * adj_st;
                let eff_price = adj as i128 * (SCALE as i128 - stale_fee) / SCALE as i128;
                let term_new = new_p as i128 * eff_price / ps as i128;
                let term_old = old_p as i128 * eff_price / ps as i128;
                term_new - term_old
            } else {
                let eff_fee = if eq {
                    let adj_st =
                        oracle_adj_stale(clock_slot, self.oracle_slot, self.fee_multiplier, true);
                    fb * adj_st
                } else {
                    fb
                };
                pos_integral_b2q(old_p, new_p, adj, spread, eff_fee, sp, bounds, ps, bstart)
            }
        } else if sb == 0 {
            let (corr, _) = stale_vault_corr(abs_d, signed_d, self, false);
            let adj = adj_price(base_price, corr);
            let adj_pos = self.position_b2q as i128 - self.vault_threshold as i128 * sa as i128;
            if adj_pos > 0 {
                let adj_pos_u = adj_pos as u64;
                pos_integral_b2q(
                    adj_pos_u,
                    adj_pos_u.wrapping_add(input_amount),
                    adj,
                    spread,
                    fb,
                    sp,
                    bounds,
                    ps,
                    0,
                )
            } else {
                input_weighted_output_b2q(input_amount, adj, spread, fb, sp, bounds, ps, 0)
            }
        } else if eq {
            let (corr, _) = stale_vault_corr(abs_d, signed_d, self, false);
            let adj = adj_price(base_price, corr);
            let tier = find_tier(input_amount, bounds);
            let adj_st =
                oracle_adj_stale(clock_slot, self.oracle_slot, self.fee_multiplier, sa == 0);
            let spread_sub = spread[tier] as i128 + fb as i128 * adj_st as i128;
            let eff_price = adj as i128 * (SCALE as i128 - spread_sub) / SCALE as i128;
            input_amount as i128 * eff_price / ps as i128
        } else {
            let (corr, _) = stale_vault_corr(abs_d, signed_d, self, false);
            let adj = adj_price(base_price, corr);
            let adj_st =
                oracle_adj_stale(clock_slot, self.oracle_slot, self.fee_multiplier, sa == 0);
            let eff_fee = fb * adj_st;
            input_weighted_output_b2q(input_amount, adj, spread, eff_fee, sp, bounds, ps, bstart)
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn quote_q2b(
        &self,
        input_amount: u64,
        base_price: u64,
        sp: u64,
        spread: &[u64; NUM_TIERS],
        bounds: &[u64; NUM_TIERS],
        ps: u64,
        fb: u64,
        eq: bool,
        signed_d: i128,
        abs_d: u64,
        sa: u64,
        sb: u64,
        _bstart: usize,
        clock_slot: u64,
    ) -> i128 {
        if sb == 0 {
            let (corr, _) = stale_vault_corr(abs_d, signed_d, self, true);
            let adj = adj_price(base_price, corr);
            let old_p = self.position_q2b;
            let new_p = self.position_q2b.wrapping_add(input_amount);
            pos_integral_q2b(old_p, new_p, adj, spread, fb, sp, bounds, ps, 0)
        } else {
            let adj_pos = self.position_q2b as i128 - self.vault_threshold as i128 * sb as i128;
            if adj_pos > 0 {
                let (corr, _) = stale_vault_corr(abs_d, signed_d, self, true);
                let adj = adj_price(base_price, corr);
                let eff_fee = if sa == 0 { fb } else { 0 };
                let adj_pos_u = adj_pos as u64;
                pos_integral_q2b(
                    adj_pos_u,
                    adj_pos_u.wrapping_add(input_amount),
                    adj,
                    spread,
                    eff_fee,
                    sp,
                    bounds,
                    ps,
                    0,
                )
            } else if eq {
                let (corr, _) = stale_vault_corr(abs_d, signed_d, self, true);
                let adj = adj_price(base_price, corr);
                let tier = find_tier(input_amount, bounds);
                let fee = spread[tier] as i128 + if sa == 0 { fb as i128 } else { 0 };
                let eff_price = adj as i128 * (SCALE as i128 + fee) / SCALE as i128;
                if eff_price == 0 {
                    return 0;
                }
                ps as i128 * input_amount as i128 / eff_price
            } else {
                let (corr, _) = stale_vault_corr(abs_d, signed_d, self, true);
                let adj = adj_price(base_price, corr);
                let adj_st =
                    oracle_adj_stale(clock_slot, self.oracle_slot, self.fee_multiplier, sa == 0);
                let eff_fee = fb * adj_st;
                input_weighted_output_q2b(input_amount, adj, spread, eff_fee, sp, bounds, ps, 0)
            }
        }
    }
}

#[inline(always)]
fn staleness(clock_slot: u64, slot: u64) -> u64 {
    if clock_slot == 0 {
        return 999;
    }
    if clock_slot >= slot {
        clock_slot - slot
    } else {
        999
    }
}

#[inline(always)]
fn oracle_adj_stale(clock_slot: u64, oracle_slot: u64, fee_mult: u64, sa_is_zero: bool) -> u64 {
    let raw = clock_slot as i128 - oracle_slot as i128 - fee_mult as i128;
    if sa_is_zero {
        1u64.max(raw.max(0) as u64)
    } else if clock_slot == oracle_slot {
        1u64.max(raw.max(0) as u64)
    } else {
        raw.max(0) as u64
    }
}

#[inline(always)]
fn simple_vault_corr(abs_delta: u64, signed_delta: i128, divisor: u64) -> i128 {
    if divisor == 0 {
        return 0;
    }
    let corr = (SCALE as u128 * abs_delta as u128 + divisor as u128 / 2) / divisor as u128;
    let corr = corr as i128;
    if signed_delta < 0 {
        -corr
    } else {
        corr
    }
}

fn stale_vault_corr(
    abs_delta: u64,
    signed_delta: i128,
    state: &GoonFiState,
    is_q2b: bool,
) -> (i128, u128) {
    let threshold = state.stale_vault_threshold;
    let vs = state.vault_scale;
    let vs_pw = state.vault_scale_pw;
    let vsh = state.vault_scale_high;
    if vs == 0 {
        return (0, 0);
    }
    if abs_delta <= threshold {
        let corr = (abs_delta as u128 * SCALE as u128 + vs as u128 / 2) / vs as u128;
        let corr_i = corr as i128;
        let total = if signed_delta < 0 { -corr_i } else { corr_i };
        return (total, 0);
    }
    let base = (threshold as u128 * SCALE as u128 + vs as u128 / 2) / vs as u128;
    let excess_d = abs_delta as u128 - threshold as u128;
    let div = if vs_pw >= vs {
        if is_q2b {
            vs_pw
        } else {
            vsh
        }
    } else if is_q2b {
        vsh
    } else {
        vs_pw
    };
    let excess = if div > 0 {
        (excess_d * SCALE as u128 + div as u128 / 2) / div as u128
    } else {
        0
    };
    let total = base + excess;
    let total_i = total as i128;
    let result = if signed_delta < 0 { -total_i } else { total_i };
    (result, excess)
}

#[inline(always)]
fn adj_price(base_price: u64, correction: i128) -> u64 {
    let result = base_price as i128 * (SCALE as i128 + correction) / SCALE as i128;
    if result < 0 {
        0
    } else {
        result as u64
    }
}

#[inline(always)]
fn find_tier(input_amount: u64, bounds: &[u64; NUM_TIERS]) -> usize {
    for i in 0..NUM_TIERS {
        if input_amount <= bounds[i] {
            return i;
        }
    }
    NUM_TIERS - 1
}

#[inline(always)]
fn tier_price_b2q(adj: u64, spread: &[u64; NUM_TIERS], eff_fee: u64, sp: u64, tier: usize) -> i128 {
    let sa_val = ((spread[tier] + eff_fee) as u128 * sp as u128 / SCALE as u128) as i128;
    adj as i128 * (SCALE as i128 - sa_val) / SCALE as i128
}

#[inline(always)]
fn tier_price_q2b(adj: u64, spread: &[u64; NUM_TIERS], eff_fee: u64, sp: u64, tier: usize) -> i128 {
    let sa_val = ((spread[tier] + eff_fee) as u128 * sp as u128 / SCALE as u128) as i128;
    adj as i128 * (SCALE as i128 + sa_val) / SCALE as i128
}

fn interp_price_at_pos_b2q(
    pos: u64,
    adj: u64,
    spread: &[u64; NUM_TIERS],
    eff_fee: u64,
    sp: u64,
    bounds: &[u64; NUM_TIERS],
    start: usize,
) -> i128 {
    if pos <= bounds[start] {
        return tier_price_b2q(adj, spread, eff_fee, sp, start);
    }
    for k in start..NUM_TIERS - 1 {
        if pos <= bounds[k + 1] {
            let tw = bounds[k + 1] - bounds[k];
            let dist = pos - bounds[k];
            let rem = tw - dist;
            let p_hi = tier_price_b2q(adj, spread, eff_fee, sp, k + 1);
            let p_lo = tier_price_b2q(adj, spread, eff_fee, sp, k);
            return (dist as i128 * p_hi + rem as i128 * p_lo) / tw as i128;
        }
    }
    tier_price_b2q(adj, spread, eff_fee, sp, NUM_TIERS - 1)
}

fn interp_price_at_pos_q2b(
    pos: u64,
    adj: u64,
    spread: &[u64; NUM_TIERS],
    eff_fee: u64,
    sp: u64,
    bounds: &[u64; NUM_TIERS],
    start: usize,
) -> i128 {
    if pos <= bounds[start] {
        return tier_price_q2b(adj, spread, eff_fee, sp, start);
    }
    for k in start..NUM_TIERS - 1 {
        if pos <= bounds[k + 1] {
            let tw = bounds[k + 1] - bounds[k];
            let dist = pos - bounds[k];
            let rem = tw - dist;
            let p_hi = tier_price_q2b(adj, spread, eff_fee, sp, k + 1);
            let p_lo = tier_price_q2b(adj, spread, eff_fee, sp, k);
            return (dist as i128 * p_hi + rem as i128 * p_lo + tw as i128 - 1) / tw as i128;
        }
    }
    tier_price_q2b(adj, spread, eff_fee, sp, NUM_TIERS - 1)
}

#[allow(clippy::too_many_arguments)]
fn pos_integral_b2q(
    old_pos: u64,
    new_pos: u64,
    adj: u64,
    spread: &[u64; NUM_TIERS],
    eff_fee: u64,
    sp: u64,
    bounds: &[u64; NUM_TIERS],
    ps: u64,
    start: usize,
) -> i128 {
    let p_new = interp_price_at_pos_b2q(new_pos, adj, spread, eff_fee, sp, bounds, start);
    let p_old = interp_price_at_pos_b2q(old_pos, adj, spread, eff_fee, sp, bounds, start);
    new_pos as i128 * p_new / ps as i128 - old_pos as i128 * p_old / ps as i128
}

#[allow(clippy::too_many_arguments)]
fn pos_integral_q2b(
    old_pos: u64,
    new_pos: u64,
    adj: u64,
    spread: &[u64; NUM_TIERS],
    eff_fee: u64,
    sp: u64,
    bounds: &[u64; NUM_TIERS],
    ps: u64,
    start: usize,
) -> i128 {
    let p_new = interp_price_at_pos_q2b(new_pos, adj, spread, eff_fee, sp, bounds, start);
    let p_old = interp_price_at_pos_q2b(old_pos, adj, spread, eff_fee, sp, bounds, start);
    if p_new == 0 || p_old == 0 {
        return 0;
    }
    ps as i128 * new_pos as i128 / p_new - ps as i128 * old_pos as i128 / p_old
}

#[allow(clippy::too_many_arguments)]
fn input_weighted_output_b2q(
    input_amount: u64,
    adj: u64,
    spread: &[u64; NUM_TIERS],
    eff_fee: u64,
    sp: u64,
    bounds: &[u64; NUM_TIERS],
    ps: u64,
    start: usize,
) -> i128 {
    if input_amount <= bounds[start] {
        let tp0 = tier_price_b2q(adj, spread, eff_fee, sp, start);
        return input_amount as i128 * tp0 / ps as i128;
    }
    for i in start..NUM_TIERS - 1 {
        if input_amount <= bounds[i + 1] {
            let tw = bounds[i + 1] - bounds[i];
            let consumed = input_amount - bounds[i];
            let unused = tw - consumed;
            let p_hi = tier_price_b2q(adj, spread, eff_fee, sp, i + 1);
            let p_lo = tier_price_b2q(adj, spread, eff_fee, sp, i);
            let weighted = (consumed as i128 * p_hi + unused as i128 * p_lo) / tw as i128;
            return input_amount as i128 * weighted / ps as i128;
        }
    }
    let last = tier_price_b2q(adj, spread, eff_fee, sp, NUM_TIERS - 1);
    input_amount as i128 * last / ps as i128
}

#[allow(clippy::too_many_arguments)]
fn input_weighted_output_q2b(
    input_amount: u64,
    adj: u64,
    spread: &[u64; NUM_TIERS],
    eff_fee: u64,
    sp: u64,
    bounds: &[u64; NUM_TIERS],
    ps: u64,
    start: usize,
) -> i128 {
    if input_amount <= bounds[start] {
        let tp0 = tier_price_q2b(adj, spread, eff_fee, sp, start);
        if tp0 == 0 {
            return 0;
        }
        return ps as i128 * input_amount as i128 / tp0;
    }
    for i in start..NUM_TIERS - 1 {
        if input_amount <= bounds[i + 1] {
            let tw = bounds[i + 1] - bounds[i];
            let consumed = input_amount - bounds[i];
            let unused = tw - consumed;
            let p_hi = tier_price_q2b(adj, spread, eff_fee, sp, i + 1);
            let p_lo = tier_price_q2b(adj, spread, eff_fee, sp, i);
            let weighted =
                (consumed as i128 * p_hi + unused as i128 * p_lo + tw as i128 - 1) / tw as i128;
            if weighted == 0 {
                return 0;
            }
            return ps as i128 * input_amount as i128 / weighted;
        }
    }
    let last = tier_price_q2b(adj, spread, eff_fee, sp, NUM_TIERS - 1);
    if last == 0 {
        return 0;
    }
    ps as i128 * input_amount as i128 / last
}

pub struct GoonFiReplay {
    state: GoonFiState,
    has_pool: bool,
    has_oracle: bool,
    has_base_vault: bool,
    has_quote_vault: bool,
}

impl GoonFiReplay {
    pub fn new() -> Self {
        Self {
            state: GoonFiState::default(),
            has_pool: false,
            has_oracle: false,
            has_base_vault: false,
            has_quote_vault: false,
        }
    }
}

impl ProtocolReplay for GoonFiReplay {
    fn apply_update(&mut self, role: &str, data: &[u8], _slot: u64) {
        match role {
            "pool" => {
                if self.state.update_from_market_data(data).is_ok() {
                    self.has_pool = true;
                }
            }
            "oracle" => {
                let _ = self.state.update_from_oracle_data(data);
                self.has_oracle = true;
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
            if self.state.bid_price > 0 && self.state.price_scale > 0 {
                let b2q_input = (usd * 1_000_000.0) as u128 * self.state.price_scale as u128
                    / self.state.bid_price as u128;
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
        self.has_pool && self.has_oracle && self.has_quote_vault
    }
}
