#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub unsafe fn optimized_mul_shift(a: u128, b: u64) -> u128 {
    let a_low = a as u64;
    let a_high = (a >> 64) as u64;
    let high: u64;
    std::arch::asm!(
        "mul {b}",
        inlateout("rax") a_low => _,
        lateout("rdx") high,
        b = in(reg) b,
        options(nomem, nostack, preserves_flags)
    );
    u128::from(high) + (u128::from(a_high) * u128::from(b))
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub unsafe fn optimized_mul_shift(a: u128, b: u64) -> u128 {
    let a_low = a as u64;
    let a_high = (a >> 64) as u64;
    let high: u64;
    std::arch::asm!(
        "umulh {high}, {a_low}, {b}",
        high = out(reg) high,
        a_low = in(reg) a_low,
        b = in(reg) b,
        options(nostack, nomem, preserves_flags)
    );
    u128::from(high) + (u128::from(a_high) * u128::from(b))
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[inline(always)]
pub unsafe fn optimized_mul_shift(a: u128, b: u64) -> u128 {
    (a * (b as u128)) >> 64
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct U256([u128; 2]);

impl U256 {
    pub const ZERO: Self = Self([0, 0]);

    pub fn from_u128(v: u128) -> Self {
        Self([v, 0])
    }

    pub fn low_u64(self) -> u64 {
        self.0[0] as u64
    }

    pub fn is_zero(self) -> bool {
        self.0[0] == 0 && self.0[1] == 0
    }

    pub fn shl(self, n: u32) -> Self {
        if n >= 256 {
            return Self::ZERO;
        }
        if n >= 128 {
            Self([0, self.0[0] << (n - 128)])
        } else if n == 0 {
            self
        } else {
            Self([self.0[0] << n, (self.0[1] << n) | (self.0[0] >> (128 - n))])
        }
    }

    pub fn shr(self, n: u32) -> Self {
        if n >= 256 {
            return Self::ZERO;
        }
        if n >= 128 {
            Self([self.0[1] >> (n - 128), 0])
        } else if n == 0 {
            self
        } else {
            Self([(self.0[0] >> n) | (self.0[1] << (128 - n)), self.0[1] >> n])
        }
    }

    pub fn add(self, rhs: Self) -> Self {
        let (lo, carry) = self.0[0].overflowing_add(rhs.0[0]);
        let hi = self.0[1].wrapping_add(rhs.0[1]).wrapping_add(carry as u128);
        Self([lo, hi])
    }

    pub fn mul(self, rhs: Self) -> Self {
        let a0 = self.0[0] as u64 as u128;
        let a1 = self.0[0] >> 64;
        let a2 = self.0[1] as u64 as u128;
        let a3 = self.0[1] >> 64;
        let b0 = rhs.0[0] as u64 as u128;
        let b1 = rhs.0[0] >> 64;
        let b2 = rhs.0[1] as u64 as u128;

        let p00 = a0 * b0;
        let p01 = a0 * b1;
        let p10 = a1 * b0;
        let p02 = a0 * b2;
        let p11 = a1 * b1;
        let p20 = a2 * b0;

        let lo_lo = p00 as u64 as u128;
        let carry1 = (p00 >> 64) + (p01 as u64 as u128) + (p10 as u64 as u128);
        let lo = lo_lo | (carry1 << 64);

        let carry2 = (carry1 >> 64)
            + (p01 >> 64)
            + (p10 >> 64)
            + (p02 as u64 as u128)
            + (p11 as u64 as u128)
            + (p20 as u64 as u128);
        let hi_bits = carry2 + (a3 * b0 + a2 * b1 + a1 * b2);
        let hi_lo = hi_bits as u64 as u128;

        let carry3 = (carry2 >> 64) + (p02 >> 64) + (p11 >> 64) + (p20 >> 64) + (a3 * b1 + a2 * b2);
        let hi = hi_lo | ((carry3 as u64 as u128) << 64);

        Self([lo, hi])
    }

    pub fn div_u128(self, d: u128) -> Self {
        if d == 0 {
            panic!("division by zero");
        }
        if self.0[1] == 0 {
            return Self([self.0[0] / d, 0]);
        }

        let hi_q = self.0[1] / d;
        let hi_r = self.0[1] % d;

        let mut remainder = hi_r;

        let lo_hi = self.0[0] >> 64;
        let lo_lo = self.0[0] & ((1u128 << 64) - 1);

        let combined_hi = (remainder << 64) | lo_hi;
        let q_hi = combined_hi / d;
        remainder = combined_hi % d;

        let combined_lo = (remainder << 64) | lo_lo;
        let q_lo = combined_lo / d;

        let result_lo = (q_hi << 64) | q_lo;

        Self([result_lo, hi_q])
    }

    pub fn div_ceil_u128(self, d: u128) -> Self {
        let q = self.div_u128(d);

        let product = q.mul_u128(d);
        if product < self {
            q.add(Self::from_u128(1))
        } else {
            q
        }
    }

    fn mul_u128(self, b: u128) -> Self {
        let lo = self.0[0] as u64 as u128;
        let lo_hi = self.0[0] >> 64;
        let p0 = lo * (b as u64 as u128);
        let p1 = lo * (b >> 64);
        let p2 = lo_hi * (b as u64 as u128);
        let p3 = lo_hi * (b >> 64);

        let r0 = p0 as u64 as u128;
        let c1 = (p0 >> 64) + (p1 as u64 as u128) + (p2 as u64 as u128);
        let r_lo = r0 | (c1 << 64);
        let c2 = (c1 >> 64) + (p1 >> 64) + (p2 >> 64) + (p3 as u64 as u128);
        let r_hi = c2 + self.0[1] * b;
        Self([r_lo, r_hi])
    }
}

impl std::ops::Shl<u32> for U256 {
    type Output = Self;
    fn shl(self, n: u32) -> Self {
        self.shl(n)
    }
}

impl std::ops::Shr<u32> for U256 {
    type Output = Self;
    fn shr(self, n: u32) -> Self {
        self.shr(n)
    }
}

impl std::cmp::PartialOrd<Self> for U256 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::cmp::Ord for U256 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0[1].cmp(&other.0[1]).then(self.0[0].cmp(&other.0[0]))
    }
}

pub const Q64_RESOLUTION: u8 = 64;
pub const MIN_SQRT_PRICE_X64: u128 = 4295048016;
pub const MAX_SQRT_PRICE_X64: u128 = 79226673521066979257578248091;

#[inline]
pub fn checked_mul_shift_right_round_up_if<const ROUND_UP: bool>(
    n0: u128,
    n1: u128,
) -> Option<u64> {
    if n0 == 0 || n1 == 0 {
        return Some(0);
    }
    let p = n0.checked_mul(n1)?;
    let result = p >> Q64_RESOLUTION;
    let should_round = ROUND_UP && p & u128::from(u64::MAX) > 0;
    if should_round {
        Some((result + 1) as u64)
    } else {
        Some(result as u64)
    }
}

#[inline(always)]
pub fn get_amount_delta_a<const ROUND_UP: bool>(
    sqrt_price_0: u128,
    sqrt_price_1: u128,
    liquidity: u128,
) -> u64 {
    let sqrt_price_diff = sqrt_price_0.abs_diff(sqrt_price_1);
    if sqrt_price_diff == 0 || liquidity == 0 {
        return 0;
    }
    let numerator = U256::from_u128(liquidity)
        .mul(U256::from_u128(sqrt_price_diff))
        .shl(64);
    let denominator = U256::from_u128(sqrt_price_0).mul(U256::from_u128(sqrt_price_1));
    if denominator.is_zero() {
        return 0;
    }

    let num_bits = (128 - liquidity.leading_zeros()) + (128 - sqrt_price_diff.leading_zeros());
    let den_bits = (128 - sqrt_price_0.leading_zeros()) + (128 - sqrt_price_1.leading_zeros());
    if num_bits <= 64 && den_bits <= 128 {
        let num = (liquidity * sqrt_price_diff) << 64;
        let denom = sqrt_price_0 * sqrt_price_1;
        return if ROUND_UP {
            num.div_ceil(denom) as u64
        } else {
            (num / denom) as u64
        };
    }
    if ROUND_UP {
        numerator
            .div_ceil_u128(sqrt_price_0)
            .div_ceil_u128(sqrt_price_1)
            .low_u64()
    } else {
        numerator
            .div_u128(sqrt_price_0)
            .div_u128(sqrt_price_1)
            .low_u64()
    }
}

#[inline(always)]
pub fn get_amount_delta_b<const ROUND_UP: bool>(
    sqrt_price_0: u128,
    sqrt_price_1: u128,
    liquidity: u128,
) -> Option<u64> {
    let diff = sqrt_price_0.abs_diff(sqrt_price_1);
    checked_mul_shift_right_round_up_if::<ROUND_UP>(liquidity, diff)
}

#[inline(always)]
pub fn get_next_sqrt_price_from_a_round_up(sqrt_price: u128, liquidity: u128, amount: u64) -> u128 {
    if amount == 0 || liquidity == 0 {
        return sqrt_price;
    }
    let product = sqrt_price.saturating_mul(amount as u128);
    let numerator = sqrt_price.saturating_mul(liquidity) << 64;
    let denominator = (liquidity << 64).saturating_add(product);
    if denominator == 0 {
        return sqrt_price;
    }
    numerator.div_ceil(denominator)
}

#[inline(always)]
pub const fn get_next_sqrt_price_from_b_round_down(
    sqrt_price: u128,
    liquidity: u128,
    amount: u64,
) -> u128 {
    let amount_x64 = (amount as u128) << Q64_RESOLUTION;
    let delta = amount_x64 / liquidity;
    sqrt_price + delta
}

#[inline(always)]
pub fn get_next_sqrt_price(sqrt_price: u128, liquidity: u128, amount: u64, a_to_b: bool) -> u128 {
    if a_to_b {
        get_next_sqrt_price_from_a_round_up(sqrt_price, liquidity, amount)
    } else {
        get_next_sqrt_price_from_b_round_down(sqrt_price, liquidity, amount)
    }
}

#[inline]
pub fn get_amount_fixed_delta(
    sqrt_price_current: u128,
    sqrt_price_target: u128,
    liquidity: u128,
    a_to_b: bool,
) -> Option<u64> {
    if a_to_b {
        Some(get_amount_delta_a::<true>(
            sqrt_price_current,
            sqrt_price_target,
            liquidity,
        ))
    } else {
        get_amount_delta_b::<true>(sqrt_price_current, sqrt_price_target, liquidity)
    }
}

#[inline]
pub fn get_amount_unfixed_delta(
    sqrt_price_current: u128,
    sqrt_price_target: u128,
    liquidity: u128,
    a_to_b: bool,
) -> Option<u64> {
    if a_to_b {
        get_amount_delta_b::<false>(sqrt_price_current, sqrt_price_target, liquidity)
    } else {
        Some(get_amount_delta_a::<false>(
            sqrt_price_current,
            sqrt_price_target,
            liquidity,
        ))
    }
}

#[inline]
pub fn add_liquidity_delta(liquidity: u128, delta: i128) -> Option<u128> {
    if delta > 0 {
        liquidity.checked_add(delta as u128)
    } else {
        liquidity.checked_sub(u128::try_from(-delta).ok()?)
    }
}

#[inline]
pub fn calculate_update<const A_TO_B: bool>(
    tick_liquidity_net: i128,
    liquidity: u128,
) -> Option<u128> {
    let signed = if A_TO_B {
        -tick_liquidity_net
    } else {
        tick_liquidity_net
    };
    add_liquidity_delta(liquidity, signed)
}

const U1_RATIO: u128 = 0xfffcb933bd6fb800;
const U2_RATIO: u128 = 1 << 64;

const RAYDIUM_RATIOS: [u64; 19] = [
    0xfff97272373d4000,
    0xfff2e50f5f657000,
    0xffe5caca7e10f000,
    0xffcb9843d60f7000,
    0xff973b41fa98e800,
    0xff2ea16466c9b000,
    0xfe5dee046a9a3800,
    0xfcbe86c7900bb000,
    0xf987a7253ac65800,
    0xf3392b0822bb6000,
    0xe7159475a2caf000,
    0xd097f3bdfd2f2000,
    0xa9f746462d9f8000,
    0x70d869a156f31c00,
    0x31be135f97ed3200,
    0x09aa508b5b85a500,
    0x005d6af8dedc582c,
    0x00002216e584f5fa,
    0,
];

pub fn get_sqrt_price_at_tick(tick: i32) -> u128 {
    let abs_tick = tick.unsigned_abs();
    let mut ratio = if abs_tick & 0x1 != 0 {
        U1_RATIO
    } else {
        U2_RATIO
    };
    for bit in 1..=18u32 {
        if abs_tick & (1 << bit) != 0 {
            ratio = unsafe { optimized_mul_shift(ratio, RAYDIUM_RATIOS[bit as usize - 1]) };
        }
    }
    if tick > 0 {
        ratio = u128::MAX / ratio;
    }
    ratio
}

const ORCA_U1_NEG_RATIO: u128 = 18445821805675392311;
const ORCA_U2_NEG_RATIO: u128 = 18446744073709551616;

const ORCA_NEGATIVE_RATIOS: [u64; 19] = [
    0,
    18444899583751176498,
    18443055278223354162,
    18439367220385604838,
    18431993317065449817,
    18417254355718160513,
    18387811781193591352,
    18329067761203520168,
    18212142134806087854,
    17980523815641551639,
    17526086738831147013,
    16651378430235024244,
    15030750278693429944,
    12247334978882834399,
    8131365268884726200,
    3584323654723342297,
    696457651847595233,
    26294789957452057,
    37481735321082,
];

const ORCA_U1_POS_RATIO_Q96: u128 = 79232123823359799118286999567;
const ORCA_U2_POS_RATIO_Q96: u128 = 79228162514264337593543950336;

const ORCA_POSITIVE_RATIOS_Q96: [u128; 19] = [
    0,
    79236085330515764027303304731,
    79244008939048815603706035061,
    79259858533276714757314932305,
    79291567232598584799939703904,
    79355022692464371645785046466,
    79482085999252804386437311141,
    79736823300114093921829183326,
    80248749790819932309965073892,
    81282483887344747381513967011,
    83390072131320151908154831281,
    87770609709833776024991924138,
    97234110755111693312479820773,
    119332217159966728226237229890,
    179736315981702064433883588727,
    407748233172238350107850275304,
    2098478828474011932436660412517,
    55581415166113811149459800483533,
    38992368544603139932233054999993551,
];

#[inline(always)]
fn mul_shift_96(ratio: u128, multiplier: u128) -> u128 {
    let mu_hi = (multiplier >> 64) as u64;
    let mu_lo = multiplier as u64;
    unsafe {
        let t2_hi = optimized_mul_shift(ratio, mu_lo);
        let t1_hi = optimized_mul_shift(ratio, mu_hi);
        let t1_lo = (ratio as u64).wrapping_mul(mu_hi);
        let (w1, c1) = (t2_hi as u64).overflowing_add(t1_lo);
        let (sum_w2, c2a) = ((t2_hi >> 64) as u64).overflowing_add(t1_hi as u64);
        let (w2, c2b) = sum_w2.overflowing_add(u64::from(c1));
        let c2 = c2a || c2b;
        let w3 = ((t1_hi >> 64) as u64) + u64::from(c2);
        let r_low = (w1 >> 32) | (w2 << 32);
        let r_high = (w2 >> 32) | (w3 << 32);
        u128::from(r_low) | (u128::from(r_high) << 64)
    }
}

pub fn get_sqrt_price_at_tick_orca(tick: i32) -> u128 {
    let abs_tick = tick.unsigned_abs();
    if tick >= 0 {
        let mut ratio = if abs_tick & 0x1 != 0 {
            ORCA_U1_POS_RATIO_Q96
        } else {
            ORCA_U2_POS_RATIO_Q96
        };
        for bit in 1..=18u32 {
            if abs_tick & (1 << bit) != 0 {
                ratio = mul_shift_96(ratio, ORCA_POSITIVE_RATIOS_Q96[bit as usize]);
            }
        }
        ratio >> 32
    } else {
        let mut ratio = if abs_tick & 0x1 != 0 {
            ORCA_U1_NEG_RATIO
        } else {
            ORCA_U2_NEG_RATIO
        };
        for bit in 1..=18u32 {
            if abs_tick & (1 << bit) != 0 {
                ratio = unsafe { optimized_mul_shift(ratio, ORCA_NEGATIVE_RATIOS[bit as usize]) };
            }
        }
        ratio
    }
}

const LOG_B_2_X32: i128 = 59543866431248i128;
const BIT_PRECISION: u32 = 14;
const LOG_B_P_ERR_MARGIN_LOWER_X64: i128 = 184467440737095516i128;
const LOG_B_P_ERR_MARGIN_UPPER_X64: i128 = 15793534762490258745i128;

pub fn tick_index_from_sqrt_price(sqrt_price_x64: &u128) -> i32 {
    let msb: u32 = 128 - sqrt_price_x64.leading_zeros() - 1;
    let log2p_integer_x32 = (i128::from(msb) - 64) << 32;
    let mut bit: i128 = 0x8000_0000_0000_0000i128;
    let mut precision = 0;
    let mut log2p_fraction_x64 = 0;
    let mut r = if msb >= 64 {
        sqrt_price_x64 >> (msb - 63)
    } else {
        sqrt_price_x64 << (63 - msb)
    };
    while bit > 0 && precision < BIT_PRECISION {
        r *= r;
        let is_r_more_than_two = r >> 127_u32;
        r >>= 63 + is_r_more_than_two;
        log2p_fraction_x64 += bit * is_r_more_than_two as i128;
        bit >>= 1;
        precision += 1;
    }
    let log2p_fraction_x32 = log2p_fraction_x64 >> 32;
    let log2p_x32 = log2p_integer_x32 + log2p_fraction_x32;
    let logbp_x64 = log2p_x32 * LOG_B_2_X32;
    let tick_low: i32 = ((logbp_x64 - LOG_B_P_ERR_MARGIN_LOWER_X64) >> 64)
        .try_into()
        .unwrap();
    let tick_high: i32 = ((logbp_x64 + LOG_B_P_ERR_MARGIN_UPPER_X64) >> 64)
        .try_into()
        .unwrap();
    if tick_low == tick_high {
        tick_low
    } else {
        let actual = get_sqrt_price_at_tick_orca(tick_high);
        if actual <= *sqrt_price_x64 {
            tick_high
        } else {
            tick_low
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ClmmPoolType {
    Raydium,
    Orca,
}

pub fn get_next_sqrt_prices(pool_type: ClmmPoolType, next_tick: i32) -> (u128, u128) {
    let next_tick_price = match pool_type {
        ClmmPoolType::Orca => get_sqrt_price_at_tick_orca(next_tick),
        ClmmPoolType::Raydium => get_sqrt_price_at_tick(next_tick),
    };
    let next_sqrt_price_limit = next_tick_price.clamp(MIN_SQRT_PRICE_X64, MAX_SQRT_PRICE_X64);
    (next_tick_price, next_sqrt_price_limit)
}

type LiquidityNet = i128;
type TickPrices = (u128, u128);
type ActiveId = i32;

#[derive(Debug, Default, Clone)]
pub struct TickArraySequence {
    arr_indices: Vec<(ActiveId, LiquidityNet, TickPrices)>,
}

impl TickArraySequence {
    pub fn from_raw_ticks(
        ticks: &[(i32, i128)],
        pool_type: ClmmPoolType,
        left_boundary: i32,
        right_boundary: i32,
    ) -> Self {
        let mut entries: Vec<(i32, i128)> = vec![(left_boundary, 0)];
        for &(idx, net) in ticks {
            entries.push((idx, net));
        }
        entries.push((right_boundary, 0));
        entries.sort_by_key(|e| e.0);
        entries.dedup_by_key(|e| e.0);

        let arr_indices = entries
            .into_iter()
            .map(|(tick, net)| {
                let sqrt_price = get_next_sqrt_prices(pool_type, tick);
                (tick, net, sqrt_price)
            })
            .collect();

        Self { arr_indices }
    }

    #[inline]
    pub fn get_internal_bin_index_from_tick_index(&self, tick_index: i32) -> (usize, bool) {
        match self.arr_indices.binary_search_by_key(&tick_index, |x| x.0) {
            Ok(i) => (i, true),
            Err(i) => (i, false),
        }
    }

    #[inline]
    pub fn get_next_liquidity(&self, idx: usize) -> i128 {
        self.arr_indices[idx].1
    }

    #[inline]
    pub fn get_next_sqrt_prices_at(&self, idx: usize) -> (u128, u128) {
        self.arr_indices[idx].2
    }

    #[inline]
    pub fn get_tick_from_internal_index(&self, idx: usize) -> Option<i32> {
        self.arr_indices.get(idx).map(|e| e.0)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.arr_indices.len()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct AdaptiveFeeConstants {
    pub filter_period: u16,
    pub decay_period: u16,
    pub reduction_factor: u16,
    pub adaptive_fee_control_factor: u32,
    pub max_volatility_accumulator: u32,
    pub tick_group_size: u16,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct AdaptiveFeeVariables {
    pub last_reference_update_timestamp: u64,
    pub last_major_swap_timestamp: u64,
    pub volatility_reference: u32,
    pub tick_group_index_reference: i32,
    pub volatility_accumulator: u32,
}

#[derive(Debug, Default, Clone)]
pub struct AdaptiveFeeInfo {
    pub constants: AdaptiveFeeConstants,
    pub variables: AdaptiveFeeVariables,
}

const FEE_RATE_HARD_LIMIT: u32 = 100_000;
const VOLATILITY_ACCUMULATOR_SCALE_FACTOR: u16 = 10_000;
const REDUCTION_FACTOR_DENOMINATOR: u16 = 10_000;
const ADAPTIVE_FEE_CONTROL_FACTOR_DENOMINATOR: u32 = 100_000;
const MAX_REFERENCE_AGE: u64 = 3_600;
const MIN_TICK_INDEX: i32 = -443636;
const MAX_TICK_INDEX: i32 = 443636;

impl AdaptiveFeeVariables {
    pub fn update_reference(
        &mut self,
        tick_group_index: i32,
        current_timestamp: u64,
        c: &AdaptiveFeeConstants,
    ) {
        let max_ts = self
            .last_reference_update_timestamp
            .max(self.last_major_swap_timestamp);
        let reference_age = current_timestamp - self.last_reference_update_timestamp;
        if reference_age > MAX_REFERENCE_AGE {
            self.tick_group_index_reference = tick_group_index;
            self.volatility_reference = 0;
            self.last_reference_update_timestamp = current_timestamp;
            return;
        }
        let elapsed = current_timestamp - max_ts;
        if elapsed < u64::from(c.filter_period) {
        } else if elapsed < u64::from(c.decay_period) {
            self.tick_group_index_reference = tick_group_index;
            self.volatility_reference =
                (u64::from(self.volatility_accumulator) * u64::from(c.reduction_factor)
                    / u64::from(REDUCTION_FACTOR_DENOMINATOR)) as u32;
            self.last_reference_update_timestamp = current_timestamp;
        } else {
            self.tick_group_index_reference = tick_group_index;
            self.volatility_reference = 0;
            self.last_reference_update_timestamp = current_timestamp;
        }
    }

    fn update_volatility_accumulator(&mut self, tick_group_index: i32, c: &AdaptiveFeeConstants) {
        let index_delta = (self.tick_group_index_reference - tick_group_index).unsigned_abs();
        let va = u64::from(self.volatility_reference)
            + u64::from(index_delta) * u64::from(VOLATILITY_ACCUMULATOR_SCALE_FACTOR);
        self.volatility_accumulator =
            std::cmp::min(va, u64::from(c.max_volatility_accumulator)) as u32;
    }
}

pub enum FeeRateManager {
    Adaptive {
        a_to_b: bool,
        tick_group_index: i32,
        static_fee_rate: u16,
        constants: AdaptiveFeeConstants,
        variables: AdaptiveFeeVariables,
        lower_bound: Option<(i32, u128)>,
        upper_bound: Option<(i32, u128)>,
    },
    Static {
        static_fee_rate: u16,
    },
}

impl FeeRateManager {
    pub fn new(
        a_to_b: bool,
        current_tick_index: i32,
        timestamp: u64,
        static_fee_rate: u16,
        info: &Option<AdaptiveFeeInfo>,
    ) -> Option<Self> {
        match info {
            None => Some(Self::Static { static_fee_rate }),
            Some(info) => {
                let tgs = i32::from(info.constants.tick_group_size);
                let tick_group_index = floor_division(current_tick_index, tgs);
                let mut vars = info.variables;
                vars.update_reference(tick_group_index, timestamp, &info.constants);
                let max_delta = (info.constants.max_volatility_accumulator
                    - vars.volatility_reference)
                    .div_ceil(u32::from(VOLATILITY_ACCUMULATOR_SCALE_FACTOR));
                let lower_idx = vars.tick_group_index_reference - max_delta as i32;
                let upper_idx = vars.tick_group_index_reference + max_delta as i32;
                let lower_tick = lower_idx * tgs;
                let upper_tick = upper_idx * tgs + tgs;
                let lower_bound = if lower_tick > MIN_TICK_INDEX {
                    Some((lower_idx, get_sqrt_price_at_tick_orca(lower_tick)))
                } else {
                    None
                };
                let upper_bound = if upper_tick < MAX_TICK_INDEX {
                    Some((upper_idx, get_sqrt_price_at_tick_orca(upper_tick)))
                } else {
                    None
                };
                Some(Self::Adaptive {
                    a_to_b,
                    tick_group_index,
                    static_fee_rate,
                    constants: info.constants,
                    variables: vars,
                    lower_bound,
                    upper_bound,
                })
            }
        }
    }

    pub fn update_volatility_accumulator(&mut self) {
        if let Self::Adaptive {
            tick_group_index,
            constants,
            variables,
            ..
        } = self
        {
            variables.update_volatility_accumulator(*tick_group_index, constants);
        }
    }

    pub fn get_total_fee_rate(&self) -> u64 {
        match self {
            Self::Static { static_fee_rate } => *static_fee_rate as u64,
            Self::Adaptive {
                static_fee_rate,
                constants,
                variables,
                ..
            } => {
                let crossed = variables.volatility_accumulator * (constants.tick_group_size as u32);
                let squared = (crossed as u64) * (crossed as u64);
                let adaptive =
                    ((constants.adaptive_fee_control_factor as u128) * (squared as u128)).div_ceil(
                        (ADAPTIVE_FEE_CONTROL_FACTOR_DENOMINATOR as u128)
                            * (VOLATILITY_ACCUMULATOR_SCALE_FACTOR as u128)
                            * (VOLATILITY_ACCUMULATOR_SCALE_FACTOR as u128),
                    );
                let adaptive = adaptive.min(FEE_RATE_HARD_LIMIT as u128) as u32;
                let total = (*static_fee_rate as u32) + adaptive;
                total.min(FEE_RATE_HARD_LIMIT) as u64
            }
        }
    }

    pub fn get_bounded_sqrt_price_target(
        &self,
        sqrt_price: u128,
        curr_liquidity: u128,
    ) -> (u128, bool) {
        match self {
            Self::Static { .. } => (sqrt_price, false),
            Self::Adaptive {
                a_to_b,
                tick_group_index,
                constants,
                lower_bound,
                upper_bound,
                ..
            } => {
                if constants.adaptive_fee_control_factor == 0 || curr_liquidity == 0 {
                    return (sqrt_price, true);
                }
                if let Some((li, lp)) = lower_bound {
                    if *tick_group_index < *li {
                        if *a_to_b {
                            return (sqrt_price, true);
                        }
                        return (sqrt_price.min(*lp), true);
                    }
                }
                if let Some((ui, up)) = upper_bound {
                    if *tick_group_index > *ui {
                        if *a_to_b {
                            return (sqrt_price.max(*up), true);
                        }
                        return (sqrt_price, true);
                    }
                }
                let tgs = i32::from(constants.tick_group_size);
                let boundary_tick = if *a_to_b {
                    *tick_group_index * tgs
                } else {
                    *tick_group_index * tgs + tgs
                };
                let boundary =
                    get_sqrt_price_at_tick(boundary_tick.clamp(MIN_TICK_INDEX, MAX_TICK_INDEX));
                if *a_to_b {
                    (sqrt_price.max(boundary), false)
                } else {
                    (sqrt_price.min(boundary), false)
                }
            }
        }
    }

    pub fn advance_tick_group(&mut self) {
        if let Self::Adaptive {
            a_to_b,
            tick_group_index,
            ..
        } = self
        {
            *tick_group_index += if *a_to_b { -1 } else { 1 };
        }
    }

    pub fn advance_tick_group_after_skip(
        &mut self,
        sqrt_price: u128,
        next_tick_sqrt_price: u128,
        next_tick_index: i32,
    ) {
        if let Self::Adaptive {
            a_to_b,
            tick_group_index,
            variables,
            constants,
            ..
        } = self
        {
            let tgs = i32::from(constants.tick_group_size);
            let (tick_index, is_on_boundary) = if sqrt_price == next_tick_sqrt_price {
                (next_tick_index, next_tick_index % tgs == 0)
            } else {
                let ti = tick_index_from_sqrt_price(&sqrt_price);
                (
                    ti,
                    ti % tgs == 0 && sqrt_price == get_sqrt_price_at_tick(ti),
                )
            };
            let last_tgi = if is_on_boundary && !*a_to_b {
                tick_index / tgs - 1
            } else {
                floor_division(tick_index, tgs)
            };
            if (*a_to_b && last_tgi < *tick_group_index)
                || (!*a_to_b && last_tgi > *tick_group_index)
            {
                *tick_group_index = last_tgi;
                variables.update_volatility_accumulator(*tick_group_index, constants);
            }
            *tick_group_index += if *a_to_b { -1 } else { 1 };
        }
    }
}

pub struct SwapStepComputation {
    pub amount_in: u64,
    pub amount_out: u64,
    pub next_price: u128,
    pub fee_amount: u64,
}

pub fn orca_compute_swap_step(
    amount_remaining: u64,
    liquidity: u128,
    sqrt_price_current: u128,
    sqrt_price_target: u128,
    fee_rate: u64,
    a_to_b: bool,
) -> Option<SwapStepComputation> {
    const FEE_RATE_MUL_VALUE: u64 = 1_000_000;
    if liquidity == 0 {
        return None;
    }
    let mut is_invalid = false;
    let amount_fixed_delta =
        get_amount_fixed_delta(sqrt_price_current, sqrt_price_target, liquidity, a_to_b)
            .unwrap_or_else(|| {
                is_invalid = true;
                0
            });
    let fee_amount = (amount_remaining.checked_mul(fee_rate)?).div_ceil(FEE_RATE_MUL_VALUE);
    let amount_calc = amount_remaining.saturating_sub(fee_amount);
    let is_max_swap = amount_calc >= amount_fixed_delta && !is_invalid;
    let next_sqrt_price = if is_max_swap {
        sqrt_price_target
    } else {
        get_next_sqrt_price(sqrt_price_current, liquidity, amount_calc, a_to_b)
    };
    if sqrt_price_current == 0 || sqrt_price_target == 0 {
        return None;
    }
    let amount_unfixed =
        get_amount_unfixed_delta(sqrt_price_current, next_sqrt_price, liquidity, a_to_b)?;
    let amount_in = if is_max_swap {
        amount_fixed_delta
    } else {
        get_amount_fixed_delta(sqrt_price_current, next_sqrt_price, liquidity, a_to_b)?
    };
    let fee_amount = if is_max_swap {
        if fee_rate >= FEE_RATE_MUL_VALUE {
            return None;
        }
        (amount_in * fee_rate).div_ceil(FEE_RATE_MUL_VALUE - fee_rate)
    } else {
        amount_remaining.saturating_sub(amount_in)
    };
    Some(SwapStepComputation {
        amount_in,
        amount_out: amount_unfixed,
        next_price: next_sqrt_price,
        fee_amount,
    })
}

pub fn clmm_swap(
    a_to_b: bool,
    pool_type: ClmmPoolType,
    curr_sqrt_price: u128,
    curr_liquidity: u128,
    tick_sequence: &TickArraySequence,
    token_amount: u64,
    fee_rate: u16,
    init_internal_tick_index: (usize, bool),
    adaptive_fee_info: &Option<AdaptiveFeeInfo>,
    timestamp: u64,
    current_tick_index: i32,
    tick_array_size: i32,
) -> Option<u64> {
    if a_to_b {
        clmm_swap_inner::<true>(
            pool_type,
            curr_sqrt_price,
            curr_liquidity,
            tick_sequence,
            token_amount,
            fee_rate,
            init_internal_tick_index,
            adaptive_fee_info,
            timestamp,
            current_tick_index,
            tick_array_size,
        )
    } else {
        clmm_swap_inner::<false>(
            pool_type,
            curr_sqrt_price,
            curr_liquidity,
            tick_sequence,
            token_amount,
            fee_rate,
            init_internal_tick_index,
            adaptive_fee_info,
            timestamp,
            current_tick_index,
            tick_array_size,
        )
    }
}

fn clmm_swap_inner<const A_TO_B: bool>(
    _pool_type: ClmmPoolType,
    curr_sqrt_price: u128,
    curr_liquidity: u128,
    tick_sequence: &TickArraySequence,
    token_amount: u64,
    fee_rate: u16,
    init_internal_tick_index: (usize, bool),
    adaptive_fee_info: &Option<AdaptiveFeeInfo>,
    timestamp: u64,
    current_tick_index: i32,
    _tick_array_size: i32,
) -> Option<u64> {
    let mut amount_remaining = token_amount;
    let mut amount_calculated: u64 = 0;
    let mut curr_sqrt_price = curr_sqrt_price;
    let mut curr_liquidity = curr_liquidity;

    let mut curr_idx = if A_TO_B && !init_internal_tick_index.1 {
        init_internal_tick_index.0.checked_sub(1)?
    } else if !A_TO_B && init_internal_tick_index.1 {
        init_internal_tick_index.0.checked_add(1)?
    } else {
        init_internal_tick_index.0
    };

    let ticks_len = tick_sequence.len();
    if curr_idx >= ticks_len {
        return None;
    }

    if A_TO_B {
        while curr_idx > 0 && tick_sequence.get_next_liquidity(curr_idx) == 0 {
            curr_idx -= 1;
        }
    } else {
        while curr_idx < ticks_len - 1 && tick_sequence.get_next_liquidity(curr_idx) == 0 {
            curr_idx += 1;
        }
    }

    let mut fee_mgr = FeeRateManager::new(
        A_TO_B,
        current_tick_index,
        timestamp,
        fee_rate,
        adaptive_fee_info,
    )?;

    while amount_remaining > 0 {
        fee_mgr.update_volatility_accumulator();
        let total_fee_rate = fee_mgr.get_total_fee_rate();
        let (next_tick_price, target_sqrt_price) = tick_sequence.get_next_sqrt_prices_at(curr_idx);
        let (bounded_target, adaptive_skipped) =
            fee_mgr.get_bounded_sqrt_price_target(target_sqrt_price, curr_liquidity);

        let swap_comp = orca_compute_swap_step(
            amount_remaining,
            curr_liquidity,
            curr_sqrt_price,
            bounded_target,
            total_fee_rate,
            A_TO_B,
        )?;

        let step_cost = swap_comp.amount_in + swap_comp.fee_amount;
        amount_remaining = amount_remaining.checked_sub(step_cost)?;
        amount_calculated += swap_comp.amount_out;
        curr_sqrt_price = swap_comp.next_price;

        if curr_sqrt_price == next_tick_price && amount_remaining > 0 {
            let next_liq = tick_sequence.get_next_liquidity(curr_idx);
            curr_liquidity = calculate_update::<A_TO_B>(next_liq, curr_liquidity)?;
            if (A_TO_B && curr_idx == 0) || (!A_TO_B && curr_idx == ticks_len - 1) {
                if next_liq != 0 || curr_liquidity == 0 || amount_remaining == 0 {
                    break;
                }
                let final_target = if A_TO_B {
                    MIN_SQRT_PRICE_X64
                } else {
                    MAX_SQRT_PRICE_X64
                };
                if let Some(final_swap) = orca_compute_swap_step(
                    amount_remaining,
                    curr_liquidity,
                    curr_sqrt_price,
                    final_target,
                    fee_mgr.get_total_fee_rate(),
                    A_TO_B,
                ) {
                    amount_calculated += final_swap.amount_out;
                }
                break;
            }
            curr_idx = if A_TO_B { curr_idx - 1 } else { curr_idx + 1 };
        }

        if adaptive_skipped {
            let next_tick = tick_sequence
                .get_tick_from_internal_index(curr_idx)
                .unwrap_or(0);
            fee_mgr.advance_tick_group_after_skip(curr_sqrt_price, next_tick_price, next_tick);
        } else {
            fee_mgr.advance_tick_group();
        }
    }

    Some(amount_calculated)
}

#[inline]
pub const fn floor_division(dividend: i32, divisor: i32) -> i32 {
    if dividend % divisor == 0 || dividend.signum() == divisor.signum() {
        dividend / divisor
    } else {
        dividend / divisor - 1
    }
}
