use tracing::warn;
use uint::construct_uint;

use super::{ProtocolReplay, QuoteRow};

construct_uint! {
    pub struct U256(4);
}
construct_uint! {
    pub struct U512(8);
}

const FP_SHIFT: u32 = 48;
const FP_ONE: u128 = 1u128 << FP_SHIFT;

#[inline(always)]
fn isqrt_u128(n: u128) -> u128 {
    if n <= 1 {
        return n;
    }
    let mut x = n;
    let mut y = (x + 1) >> 1;
    while y < x {
        x = y;
        y = (x + n / x) >> 1;
    }
    x
}

#[inline(always)]
fn get_normalization_divisor(l0_upper: u64, l0_lower: u64) -> u64 {
    (l0_upper << 16) | (l0_lower >> 48)
}

mod xor_keys {
    pub const L0_COEFF: u64 = 0xb957ed15dc877c26;
    pub const L0_COEFF_HIGH: u64 = 0x46a912eb237873d9;
    pub const L1_VIRTUAL: u64 = 0xb957ed15dc877426;
    pub const RESERVE_ADJ_V0_3: u64 = 0x6e9de2b30b19f1ea;
    pub const RESERVE_ADJ_V4: u64 = 0x6e9de2b30b19f9ea;
    pub const FEE_STRUCTURE_KEY: u64 = 0x47d26c2e77aa1400;
    pub const FIELD_0X30: u64 = 0x96286f3f7c145a29;
    pub const FIELD_0X90: u64 = 0xbf03b62bffacf846;
    pub const FIELD_0XB0: u64 = 0x40f849d0005707ba;
    pub const OTHER_UNKNOWN_POOL_PARAM_QUOTE_TO_BASE: u64 = 0x69d190c683eda5d3;
    pub const Q2B_SCALE_FACTOR_KEY: u64 = 0x96246f337c185a25;
    pub const ACTIVATION_THRESHOLD_KEY: u64 = 0x40fb49d3005407bf;
    pub const ISQRT_THRESHOLD_KEY: u64 = 0x40f049d8005f07b2;
    pub const IMPACT_MULTIPLIER_KEY: u64 = 0x40f649de005907b0;
    pub const POOL_PARAM_1_SCALED_DELTA_POS_KEY: u64 = 0x40f249da005d07b4;
    pub const POOL_PARAM_1_SCALED_KEY: u64 = 0x40e449cc004b07ae;
    pub const IMPACT_ACTIVATION_THRESHOLD_2_KEY: u64 = 0x40ed49c5004207a9;
    pub const IMPACT_MULTIPLIER_2_KEY: u64 = 0x40e849c0004707aa;
    pub const ISQRT_THRESHOLD_2_KEY: u64 = 0x40ea49c2004507ac;
    pub const FEE_TIER_BOUNDARY_KEY: u64 = 0x47d16c2d77a91401;
    pub const FEE_FACTOR_HIGH_KEY: u64 = 0x47d06c2c77a81402;
    pub const FIELD_0XD0: u64 = 0x40f449dc005b07be;
    pub const TIER_3_4_BOUNDARY_KEY: u64 = 0x47D76C2B77AF1403;
}

#[inline(always)]
fn xor_decode_u64(data: &[u8], offset: usize, key: u64) -> u64 {
    if data.len() < offset + 8 {
        return 0;
    }
    let value = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
    value ^ key
}

#[derive(Debug, Clone, Default)]
struct PoolParams {
    l0_lower: u64,
    l0_upper: u64,
    price_q48: u64,
    adjust_liq: u64,
    other_param_base_to_quote: u64,
    other_param_quote_to_base: u64,
    pool_param_1: u64,
    scaled_pp1: u64,
    fee_bps: u32,
    fee_denominator: u32,
    imbalance_m0: u32,
    tick_offset: u32,
    fee_factor_high: u32,
    fee_denominator_high: u32,
    q2b_scale_factor: u64,
    activation_output_threshold: u64,
    isqrt_threshold: u64,
    impact_multiplier: u64,
    pool_param_1_scaled_delta_pos: u64,
    pool_param_1_scaled: u64,
    impact_activation_threshold_2: u64,
    impact_multiplier_2: u64,
    isqrt_threshold_2: u64,
    linear_mode: u8,
    max_tick: u32,
    x1: u32,
    valid_thresh: u64,
    last_update: u64,
}

fn decrypt_pool_params(pool_data: &[u8]) -> Option<PoolParams> {
    if pool_data.len() < 0x6c0 {
        warn!(
            data_len = pool_data.len(),
            "decrypt_pool_params: data too small"
        );
        return None;
    }

    let tier = if pool_data.len() < 0x6c0 {
        0u64
    } else {
        u64::from_le_bytes(pool_data[0x6b8..0x6c0].try_into().unwrap())
    };

    let l0_lower = xor_decode_u64(pool_data, 0x220, xor_keys::L0_COEFF);
    let l0_upper = xor_decode_u64(pool_data, 0x228, xor_keys::L0_COEFF_HIGH);
    let price_q48 = xor_decode_u64(pool_data, 0x240, xor_keys::L1_VIRTUAL);

    let reserve_adj_key = if tier <= 3 {
        xor_keys::RESERVE_ADJ_V0_3
    } else {
        xor_keys::RESERVE_ADJ_V4
    };
    let adjust_liq = xor_decode_u64(pool_data, 0x250, reserve_adj_key);

    let other_param_base_to_quote = xor_decode_u64(pool_data, 0x30, xor_keys::FIELD_0X30);
    let other_param_quote_to_base = xor_decode_u64(
        pool_data,
        0x0,
        xor_keys::OTHER_UNKNOWN_POOL_PARAM_QUOTE_TO_BASE,
    );
    let pool_param_1 = xor_decode_u64(pool_data, 0xb0, xor_keys::FIELD_0XB0);
    let scaled_pp1 = xor_decode_u64(pool_data, 0x90, xor_keys::FIELD_0X90);

    let fee_structure = xor_decode_u64(pool_data, 0x2d8, xor_keys::FEE_STRUCTURE_KEY);
    let fee_denominator = (fee_structure & 0xFFFF_FFFF) as u32;
    let fee_bps = ((fee_structure >> 32) & 0xFFFF_FFFF) as u32;

    let q2b_scale_factor = xor_decode_u64(pool_data, 0x50, xor_keys::Q2B_SCALE_FACTOR_KEY);
    let activation_output_threshold =
        xor_decode_u64(pool_data, 0xc8, xor_keys::ACTIVATION_THRESHOLD_KEY);
    let isqrt_threshold = xor_decode_u64(pool_data, 0xf0, xor_keys::ISQRT_THRESHOLD_KEY);
    let impact_multiplier = xor_decode_u64(pool_data, 0xe0, xor_keys::IMPACT_MULTIPLIER_KEY);
    let pool_param_1_scaled_delta_pos = xor_decode_u64(
        pool_data,
        0x100,
        xor_keys::POOL_PARAM_1_SCALED_DELTA_POS_KEY,
    );
    let pool_param_1_scaled = xor_decode_u64(pool_data, 0x150, xor_keys::POOL_PARAM_1_SCALED_KEY);

    let impact_activation_threshold_2 = xor_decode_u64(
        pool_data,
        0x118,
        xor_keys::IMPACT_ACTIVATION_THRESHOLD_2_KEY,
    );
    let impact_multiplier_2 = xor_decode_u64(pool_data, 0x130, xor_keys::IMPACT_MULTIPLIER_2_KEY);
    let isqrt_threshold_2 = xor_decode_u64(pool_data, 0x140, xor_keys::ISQRT_THRESHOLD_2_KEY);

    let tier_boundary = xor_decode_u64(pool_data, 0x2e0, xor_keys::FEE_TIER_BOUNDARY_KEY);
    let imbalance_m0 = (tier_boundary & 0xFFFF_FFFF) as u32;
    let tick_offset = ((tier_boundary >> 32) & 0xFFFF_FFFF) as u32;

    let fee_factor_struct = xor_decode_u64(pool_data, 0x2e8, xor_keys::FEE_FACTOR_HIGH_KEY);
    let fee_denominator_high = (fee_factor_struct & 0xFFFF_FFFF) as u32;
    let fee_factor_high = ((fee_factor_struct >> 32) & 0xFFFF_FFFF) as u32;

    let tier_3_4_boundary = xor_decode_u64(pool_data, 0x2f0, xor_keys::TIER_3_4_BOUNDARY_KEY);
    let max_tick = ((tier_3_4_boundary >> 32) & 0xFFFF_FFFF) as u32;
    let x1 = (tier_3_4_boundary & 0xFFFF_FFFF) as u32;

    let linear_mode = (xor_decode_u64(pool_data, 0xd0, xor_keys::FIELD_0XD0) & 0xFF) as u8;

    let valid_thresh = xor_decode_u64(pool_data, 0x260, xor_keys::RESERVE_ADJ_V0_3);
    let last_update = xor_decode_u64(pool_data, 0x268, xor_keys::RESERVE_ADJ_V0_3);

    Some(PoolParams {
        l0_lower,
        l0_upper,
        price_q48,
        adjust_liq,
        other_param_base_to_quote,
        other_param_quote_to_base,
        pool_param_1,
        scaled_pp1,
        fee_bps,
        fee_denominator,
        imbalance_m0,
        tick_offset,
        fee_factor_high,
        fee_denominator_high,
        q2b_scale_factor,
        activation_output_threshold,
        isqrt_threshold,
        impact_multiplier,
        pool_param_1_scaled_delta_pos,
        pool_param_1_scaled,
        impact_activation_threshold_2,
        impact_multiplier_2,
        isqrt_threshold_2,
        linear_mode,
        max_tick,
        x1,
        valid_thresh,
        last_update,
    })
}

#[derive(Debug, Clone, Default)]
struct HumidifiState {
    base_amount: u64,
    quote_amount: u64,
    params: PoolParams,
}

impl HumidifiState {
    fn is_valid(&self) -> bool {
        self.params.price_q48 != 0 && self.base_amount != 0 && self.quote_amount != 0
    }

    fn is_stale(&self, current_slot: u64) -> bool {
        let expiry = self
            .params
            .valid_thresh
            .saturating_add(self.params.last_update);
        current_slot > expiry
    }

    fn get_quote(&self, input_amount: u64, is_base_to_quote: bool) -> u64 {
        let base_reserve = self.base_amount;
        let adjust_liq = self.params.adjust_liq;
        let impact_version = self.params.q2b_scale_factor;
        let price_q48 = self.params.price_q48;

        let delta_base = (base_reserve as i128) - (adjust_liq as i128);

        let effective_input: u64;
        let delta_with_input: i128;

        if is_base_to_quote {
            effective_input = input_amount;
            delta_with_input = delta_base + (effective_input as i128);
        } else {
            effective_input = ((input_amount as u128 * FP_ONE) / (price_q48 as u128)) as u64;
            delta_with_input = delta_base - (effective_input as i128);
        }

        let delta1 = delta_with_input.unsigned_abs() as u64;
        let delta2 = delta_base.unsigned_abs() as u64;

        let x1_full = self.compute_x_full(delta1, true);
        let x2_full = self.compute_x_full(delta2, false);

        let sf_2 = self.compute_sf_2(effective_input, x1_full, x2_full, impact_version);

        let quote_equivalent = if is_base_to_quote {
            (effective_input as u128 * price_q48 as u128 / FP_ONE) as u64
        } else {
            input_amount
        };

        let is_impact_level_2 = quote_equivalent >= self.params.impact_activation_threshold_2;

        let impact = self.compute_impact_term(effective_input, quote_equivalent, is_impact_level_2);

        let is_delta_negative = x1_full < x2_full;
        let pool_param = if is_delta_negative {
            self.params.other_param_quote_to_base
        } else {
            self.params.other_param_base_to_quote
        };

        let sf_dividend_raw = self.compute_sf_dividend_raw(
            sf_2,
            effective_input,
            input_amount,
            pool_param,
            impact,
            is_delta_negative,
            is_impact_level_2,
            is_base_to_quote,
        );

        self.compute_final_result(sf_dividend_raw, price_q48, input_amount, is_base_to_quote)
    }

    fn get_quote_checked(
        &self,
        input_amount: u64,
        is_base_to_quote: bool,
        current_slot: u64,
    ) -> Option<u64> {
        if !self.is_valid() {
            return None;
        }
        if self.is_stale(current_slot) {
            return None;
        }
        let result = self.get_quote(input_amount, is_base_to_quote);
        if result == 0 {
            return None;
        }
        Some(result)
    }

    #[inline(always)]
    fn compute_x_full(&self, delta: u64, round_up: bool) -> u128 {
        let divisor =
            ((self.params.l0_upper as u128) << 50) | ((self.params.l0_lower as u128) >> 14);
        if divisor == 0 {
            return 0;
        }
        let result_raw = ((U256::from(delta as u128) << 82) / U256::from(divisor)).as_u128();
        if round_up {
            result_raw + 1
        } else {
            result_raw
        }
    }

    #[inline(always)]
    fn compute_sf_2(
        &self,
        input_amount: u64,
        x1_full: u128,
        x2_full: u128,
        impact_version: u64,
    ) -> u128 {
        let norm_divisor = get_normalization_divisor(self.params.l0_upper, self.params.l0_lower);
        if norm_divisor == 0 || input_amount == 0 {
            return 1;
        }

        let x_or_diff_80: U256;
        if impact_version == 3 {
            let x1 = U256::from(x1_full);
            let x2 = U256::from(x2_full);
            let x1_pow = x1 * x1;
            let x2_pow = x2 * x2;
            let diff = if x1_pow > x2_pow {
                x1_pow - x2_pow
            } else {
                x2_pow - x1_pow
            };
            x_or_diff_80 = diff >> 48;
        } else {
            let x1_sqrt = isqrt_u128(x1_full);
            let x2_sqrt = isqrt_u128(x2_full);
            let x1 = U256::from(x1_full);
            let x2 = U256::from(x2_full);
            let x1_pow = x1 * U256::from(x1_sqrt);
            let x2_pow = x2 * U256::from(x2_sqrt);
            let diff = if x1_pow > x2_pow {
                x1_pow - x2_pow
            } else {
                x2_pow - x1_pow
            };
            x_or_diff_80 = diff >> 24;
        }

        let sf_2 = (x_or_diff_80 * U256::from(norm_divisor)) / U256::from(input_amount);
        sf_2.as_u128() + 1
    }

    #[inline(always)]
    fn compute_impact_term(
        &self,
        input_amount_base: u64,
        quote_equivalent: u64,
        is_impact_level_2: bool,
    ) -> u128 {
        if quote_equivalent <= self.params.activation_output_threshold {
            return 0;
        }

        let norm_divisor = get_normalization_divisor(self.params.l0_upper, self.params.l0_lower);
        if norm_divisor == 0 {
            return 0;
        }
        let normalized_input = (input_amount_base as u128 * FP_ONE) / (norm_divisor as u128);

        let (effective_multiplier, effective_isqrt_threshold) = if is_impact_level_2 {
            (
                self.params.impact_multiplier_2,
                self.params.isqrt_threshold_2,
            )
        } else {
            (self.params.impact_multiplier, self.params.isqrt_threshold)
        };

        let diff = normalized_input.abs_diff(effective_isqrt_threshold as u128);

        if diff == 0 {
            return 0;
        }

        let isqrt_diff = isqrt_u128(diff);
        let product = (effective_multiplier as u128) * isqrt_diff;
        product >> 24
    }

    #[inline(always)]
    fn compute_sf_dividend_raw(
        &self,
        sf_2: u128,
        effective_input: u64,
        _input_amount: u64,
        pool_param: u64,
        impact: u128,
        is_delta_negative: bool,
        is_impact_level_2: bool,
        is_base_to_quote: bool,
    ) -> i128 {
        let term_offset: i128 = if impact == 0 {
            let norm_divisor =
                get_normalization_divisor(self.params.l0_upper, self.params.l0_lower);
            if norm_divisor > 0 {
                if self.params.linear_mode != 0 {
                    ((effective_input as u128 * self.params.scaled_pp1 as u128)
                        / (norm_divisor as u128)) as i128
                } else {
                    let magic_base =
                        isqrt_u128((effective_input as u128 * FP_ONE) / (norm_divisor as u128));
                    ((self.params.scaled_pp1 as u128 * magic_base) >> 24) as i128
                }
            } else {
                0
            }
        } else {
            0
        };

        let prod = U256::from(sf_2) * U256::from(pool_param as u128);
        let ceil_div = ((prod + U256::from(FP_ONE - 1)) / U256::from(FP_ONE)).as_u128();
        let factor: i128 = if is_delta_negative { -1 } else { 1 };
        let term_sf_signed: i128 = factor * (ceil_div as i128);

        let raw_imbalance = self.compute_raw_imbalance(effective_input, is_base_to_quote);

        const MUL_PARAM: i128 = 0x86A0000000000000;
        let off = raw_imbalance * MUL_PARAM;
        let off = off + raw_imbalance;
        let off = off >> 48;
        let off = (off + (raw_imbalance << 16)).unsigned_abs();

        let swap_helps_rebalance =
            (raw_imbalance < 0 && !is_base_to_quote) || (raw_imbalance > 0 && is_base_to_quote);

        let imbalance = if swap_helps_rebalance {
            self.compute_imbalance(off)
        } else {
            0
        };
        let term_sf_with_imbalance = term_sf_signed + imbalance;

        let eff_param: u64 = if impact == 0 {
            self.params.pool_param_1
        } else if is_impact_level_2 {
            self.params.pool_param_1_scaled
        } else {
            self.params.pool_param_1_scaled_delta_pos
        };

        let pool_param_1_eff = (eff_param as u128) + impact;
        let acc_signed = term_sf_with_imbalance + (pool_param_1_eff as i128) + term_offset;

        acc_signed + 0x666666666666
    }

    #[inline(always)]
    fn compute_raw_imbalance(&self, input_amount: u64, is_base_to_quote: bool) -> i128 {
        let price_q48 = U256::from(self.params.price_q48);
        let base_reserve = U256::from(self.base_amount);
        let quote_reserve = U256::from(self.quote_amount);
        let input_in_quote = (U256::from(input_amount) * price_q48) >> 48;

        let (new_base_value, adj_quote_abs, adj_quote_neg) = if is_base_to_quote {
            let new_base = base_reserve + U256::from(input_amount);
            let new_base_value = new_base * price_q48;
            let (abs_val, neg) = if quote_reserve >= input_in_quote {
                ((quote_reserve - input_in_quote) << 48, false)
            } else {
                ((input_in_quote - quote_reserve) << 48, true)
            };
            (new_base_value, abs_val, neg)
        } else {
            let (new_base_abs, new_base_neg) = if base_reserve >= U256::from(input_amount) {
                (base_reserve - U256::from(input_amount), false)
            } else {
                (U256::from(input_amount) - base_reserve, true)
            };
            let new_base_value = new_base_abs * price_q48;
            let adjusted_quote = (quote_reserve + input_in_quote) << 48;
            if new_base_neg {
                let num = new_base_value + adjusted_quote;
                let (den_abs, den_neg) = if adjusted_quote >= new_base_value {
                    (adjusted_quote - new_base_value, false)
                } else {
                    (new_base_value - adjusted_quote, true)
                };
                if den_abs.is_zero() {
                    return 0;
                }
                let shifted_num = num << 58;
                let divided = shifted_num / den_abs;
                let result = (divided >> 11).as_u128() as i128;
                return if den_neg { result } else { -result };
            }
            (new_base_value, adjusted_quote, false)
        };

        let (num_abs, num_neg, den_abs, den_neg) = if adj_quote_neg {
            let num = new_base_value + adj_quote_abs;
            let (den, dn) = if new_base_value >= adj_quote_abs {
                (new_base_value - adj_quote_abs, false)
            } else {
                (adj_quote_abs - new_base_value, true)
            };
            (num, false, den, dn)
        } else {
            let num_neg = adj_quote_abs > new_base_value;
            let num = if num_neg {
                adj_quote_abs - new_base_value
            } else {
                new_base_value - adj_quote_abs
            };
            let den = new_base_value + adj_quote_abs;
            (num, num_neg, den, false)
        };

        if den_abs.is_zero() {
            return 0;
        }

        let shifted_num = num_abs << 58;
        let divided = shifted_num / den_abs;
        let result = (divided >> 11).as_u128() as i128;
        let result_neg = num_neg ^ den_neg;
        if result_neg {
            -result
        } else {
            result
        }
    }

    #[inline(always)]
    fn compute_imbalance(&self, off: u128) -> i128 {
        let ratio = off >> 48;

        let fee_bps = self.params.fee_bps as u128;
        let fee_denominator = self.params.fee_denominator as u128;

        if fee_bps != 0 && fee_denominator != 0 {
            let tick = (fee_bps * ratio) / fee_denominator;
            return ((tick << 48) / 10) as i128;
        }

        if fee_denominator == 0 {
            return 0;
        }

        if ratio < fee_denominator {
            return 0;
        }

        let m0 = self.params.imbalance_m0 as u128;
        let m1 = self.params.fee_denominator_high as u128;
        let tick_offset = self.params.tick_offset as u128;
        let fee_factor_high = self.params.fee_factor_high as u128;
        let max_tick = if self.params.max_tick != 0 {
            self.params.max_tick as u128
        } else {
            100_000_000u128
        };
        let x1 = if self.params.x1 != 0 {
            self.params.x1 as u128
        } else {
            2 * m1 - m0
        };

        let tick = if ratio < m0 {
            let denom = m0 - fee_denominator;
            if denom == 0 {
                0
            } else {
                tick_offset * (ratio - fee_denominator) / denom
            }
        } else if ratio < m1 {
            let denom = m1 - m0;
            if denom == 0 {
                tick_offset
            } else {
                tick_offset + (fee_factor_high - tick_offset) * (ratio - m0) / denom
            }
        } else if ratio < x1 {
            let denom = x1 - m1;
            if denom == 0 {
                fee_factor_high
            } else {
                fee_factor_high + (max_tick - fee_factor_high) * (ratio - m1) / denom
            }
        } else {
            max_tick
        };

        ((tick << 48) / 10) as i128
    }

    #[inline(always)]
    fn compute_final_result(
        &self,
        sf_dividend_raw: i128,
        price_q48: u64,
        amount: u64,
        is_base_to_quote: bool,
    ) -> u64 {
        const FP_VAL_14: u128 = 0x2710000000000000;
        let divisor = FP_VAL_14 + 1;

        let q: i128 = if sf_dividend_raw >= 0 {
            let q_u256 = (U256::from(sf_dividend_raw as u128) << 48) / U256::from(divisor);
            q_u256.as_u128() as i128 + 1
        } else {
            let neg_raw = (-sf_dividend_raw) as u128;
            let q_u256 = (U256::from(neg_raw) << 48) / U256::from(divisor);
            -(q_u256.as_u128() as i128 + 1)
        };

        let eff_denominator = (FP_ONE as i128 + q) as u128;

        if eff_denominator == 0 {
            return 0;
        }

        if is_base_to_quote {
            let numerator = (price_q48 as u128) * (amount as u128);
            (numerator / eff_denominator) as u64
        } else {
            let combined_denom = ((price_q48 as u128) * eff_denominator) >> 48;
            if combined_denom == 0 {
                return 0;
            }
            let result = ((amount as u128) << 48) / combined_denom;
            if result > u64::MAX as u128 {
                return 0;
            }
            result as u64
        }
    }
}

fn read_spl_token_balance(data: &[u8]) -> Option<u64> {
    if data.len() < 72 {
        return None;
    }
    Some(u64::from_le_bytes(data[64..72].try_into().unwrap()))
}

pub struct HumidifiReplay {
    state: HumidifiState,
    has_pool: bool,
    has_base_vault: bool,
    has_quote_vault: bool,
    last_slot: u64,
}

impl HumidifiReplay {
    pub fn new() -> Self {
        Self {
            state: HumidifiState::default(),
            has_pool: false,
            has_base_vault: false,
            has_quote_vault: false,
            last_slot: 0,
        }
    }

    fn usd_to_usdc(usd: f64) -> u64 {
        (usd * 1_000_000.0) as u64
    }

    fn mid_price_usd(&self) -> f64 {
        if self.state.params.price_q48 == 0 {
            return 0.0;
        }
        (self.state.params.price_q48 as f64) / (FP_ONE as f64)
    }
}

impl ProtocolReplay for HumidifiReplay {
    fn apply_update(&mut self, role: &str, data: &[u8], slot: u64) {
        self.last_slot = slot;
        match role {
            "pool" => {
                if let Some(params) = decrypt_pool_params(data) {
                    self.state.params = params;
                    self.has_pool = true;
                }
            }
            "base_vault" => {
                if let Some(balance) = read_spl_token_balance(data) {
                    self.state.base_amount = balance;
                    self.has_base_vault = true;
                }
            }
            "quote_vault" => {
                if let Some(balance) = read_spl_token_balance(data) {
                    self.state.quote_amount = balance;
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
        self.state.get_quote_checked(input_amount, is_b2q, slot)
    }

    fn compute_quotes(&self, slot: u64, tiers_usd: &[f64]) -> Vec<QuoteRow> {
        let mut rows = Vec::new();
        let price = self.mid_price_usd();
        if price <= 0.0 {
            return rows;
        }

        for &tier_usd in tiers_usd {
            let sol_amount = (tier_usd * 1e6 / price) as u64;
            if sol_amount > 0 {
                if let Some(out) = self.state.get_quote_checked(sol_amount, true, slot) {
                    rows.push(QuoteRow {
                        direction: "B2Q".into(),
                        input_amount: sol_amount,
                        output_amount: out,
                        input_usd_equiv: tier_usd,
                    });
                }
            }

            let usdc_amount = Self::usd_to_usdc(tier_usd);
            if usdc_amount > 0 {
                if let Some(out) = self.state.get_quote_checked(usdc_amount, false, slot) {
                    rows.push(QuoteRow {
                        direction: "Q2B".into(),
                        input_amount: usdc_amount,
                        output_amount: out,
                        input_usd_equiv: tier_usd,
                    });
                }
            }
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
        self.has_pool && self.has_base_vault && self.has_quote_vault && self.state.is_valid()
    }
}
