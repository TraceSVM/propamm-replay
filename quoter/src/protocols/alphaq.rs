use super::{ProtocolReplay, QuoteRow};

const PRICE_SCALE: u128 = 10_000_000_000;
const Q_SCALE: u128 = 1_000_000_000_000;
const ISQRT_DIVISOR: u128 = 100_000_000_000_000;

const OFF_CONFIG: usize = 0x68;
const OFF_L: usize = 0x168;
const OFF_Q_FLAT: usize = 0x198;
const OFF_P: usize = 0x1A8;
const OFF_QQ: usize = 0x1B0;
const OFF_QB: usize = 0x1B8;
const OFF_D: usize = 0x1D0;
const OFF_F: usize = 0x1D8;
const OFF_SCALE2: usize = 0x1E0;
const OFF_K: usize = 0x1E8;
const MIN_MARKET_SIZE: usize = 0x1F0;

#[inline]
fn isqrt(n: u128) -> u128 {
    if n == 0 {
        return 0;
    }
    let mut x = n;
    let mut y = x.div_ceil(2);
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

#[inline]
fn read_u64_le(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

#[derive(Debug, Clone, Default)]
struct AlphaQState {
    base_amount: u64,
    quote_amount: u64,
    p: u64,
    qq: u64,
    qb: u64,
    q_flat: u64,
    l: u64,
    d: u64,
    f: u64,
    k: u64,
    scale2: u64,
    mint_a_decimals: u8,
    mint_b_decimals: u8,
}

impl AlphaQState {
    fn update_from_market_data(&mut self, data: &[u8]) -> Result<(), &'static str> {
        if data.len() < MIN_MARKET_SIZE {
            return Err("AlphaQ market data too small");
        }
        let config = &data[OFF_CONFIG..OFF_CONFIG + 6];
        self.mint_a_decimals = config[2];
        self.mint_b_decimals = config[3];
        self.l = read_u64_le(data, OFF_L);
        self.q_flat = read_u64_le(data, OFF_Q_FLAT);
        self.p = read_u64_le(data, OFF_P);
        self.qq = read_u64_le(data, OFF_QQ);
        self.qb = read_u64_le(data, OFF_QB);
        self.d = read_u64_le(data, OFF_D);
        self.f = read_u64_le(data, OFF_F);
        self.scale2 = read_u64_le(data, OFF_SCALE2);
        self.k = read_u64_le(data, OFF_K);
        Ok(())
    }

    fn get_quote(&self, amount_in: u64, is_base_to_quote: bool) -> u64 {
        if amount_in == 0 || self.p == 0 || self.base_amount == 0 || self.quote_amount == 0 {
            return 0;
        }

        let p = self.p as u128;
        let l = self.l as u128;
        let d = (self.d as u128).min(10_000_000_000);
        let f = self.f as u128;
        let k = self.k as u128;
        let scale2 = self.scale2 as u128;
        let q_flat = self.q_flat as u128;

        let dec_diff = self.mint_a_decimals as i32 - self.mint_b_decimals as i32;
        let eff_price_scale = if dec_diff > 0 {
            PRICE_SCALE * 10u128.pow(dec_diff as u32)
        } else {
            PRICE_SCALE
        };

        let vault_a = self.base_amount as u128;
        let inventory = vault_a * p / eff_price_scale;
        let (abs_remaining, df_positive) = if l >= inventory {
            (l - inventory, true)
        } else {
            (inventory - l, false)
        };

        let isqrt_remaining = isqrt(abs_remaining * Q_SCALE);
        let d_term = isqrt_remaining * d / ISQRT_DIVISOR;

        let capped = if self.l > 0 {
            abs_remaining.min(l)
        } else {
            abs_remaining
        };

        let max_dec = self.mint_a_decimals.max(self.mint_b_decimals) as u32;
        let quad_divisor: u128 = 10u128.pow(5 + 2 * max_dec);
        let f_term = capped
            .checked_mul(capped)
            .and_then(|c2| c2.checked_mul(f))
            .map(|c2f| c2f / quad_divisor)
            .unwrap_or(0);

        let df_sum = d_term + f_term;

        if is_base_to_quote {
            let qq = self.qq as u128;
            let input_equiv = amount_in as u128 * p / eff_price_scale;

            let isqrt_val = isqrt(input_equiv * Q_SCALE);
            let isqrt_term = isqrt_val * scale2 / ISQRT_DIVISOR;
            let k_term = input_equiv
                .checked_mul(input_equiv)
                .and_then(|ie2| ie2.checked_mul(k))
                .map(|ie2k| ie2k / quad_divisor)
                .unwrap_or(0);

            let adjusted_q = qq as i128 - isqrt_term as i128 - k_term as i128
                + if df_positive {
                    df_sum as i128
                } else {
                    -(df_sum as i128)
                };

            if adjusted_q <= 0 {
                return 0;
            }

            let adjusted_price = adjusted_q as u128 * p / Q_SCALE;
            let output_complex = amount_in as u128 * adjusted_price / eff_price_scale;

            let flat_price = (2 * Q_SCALE - q_flat) * p / Q_SCALE;
            let output_flat = amount_in as u128 * flat_price / eff_price_scale;

            output_complex.min(output_flat) as u64
        } else {
            let qb = self.qb as u128;

            let isqrt_val = isqrt(amount_in as u128 * Q_SCALE);
            let isqrt_term = isqrt_val * scale2 / ISQRT_DIVISOR;
            let k_term = (amount_in as u128)
                .checked_mul(amount_in as u128)
                .and_then(|ie2| ie2.checked_mul(k))
                .map(|ie2k| ie2k / quad_divisor)
                .unwrap_or(0);

            let adjusted_q = qb as i128
                + isqrt_term as i128
                + k_term as i128
                + if df_positive {
                    df_sum as i128
                } else {
                    -(df_sum as i128)
                };

            if adjusted_q <= 0 {
                return 0;
            }

            let adjusted_price = (adjusted_q as u128 * p).div_ceil(Q_SCALE);
            if adjusted_price == 0 {
                return 0;
            }
            let output_complex = amount_in as u128 * eff_price_scale / adjusted_price;

            let flat_price = (q_flat * p).div_ceil(Q_SCALE);
            let output_flat = if flat_price > 0 {
                amount_in as u128 * eff_price_scale / flat_price
            } else {
                0
            };

            output_complex.min(output_flat) as u64
        }
    }
}

pub struct AlphaQReplay {
    state: AlphaQState,
    has_pool: bool,
    has_base_vault: bool,
    has_quote_vault: bool,
}

impl AlphaQReplay {
    pub fn new() -> Self {
        Self {
            state: AlphaQState::default(),
            has_pool: false,
            has_base_vault: false,
            has_quote_vault: false,
        }
    }
}

impl ProtocolReplay for AlphaQReplay {
    fn apply_update(&mut self, role: &str, data: &[u8], _slot: u64) {
        match role {
            "pool" => {
                if self.state.update_from_market_data(data).is_ok() {
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

    fn quote_single(&self, input_amount: u64, direction: &str, _slot: u64) -> Option<u64> {
        if !self.is_ready() || input_amount == 0 {
            return None;
        }

        let is_b2q = direction == "Q2B";
        let out = self.state.get_quote(input_amount, is_b2q);
        if out > 0 {
            Some(out)
        } else {
            None
        }
    }

    fn compute_quotes(&self, _slot: u64, tiers_usd: &[f64]) -> Vec<QuoteRow> {
        let mut rows = Vec::new();
        if !self.is_ready() {
            return rows;
        }
        for &usd in tiers_usd {
            let b2q_input = (usd * 1_000_000.0) as u64;
            let b2q_output = self.state.get_quote(b2q_input, true);
            rows.push(QuoteRow {
                direction: "B2Q".into(),
                input_amount: b2q_input,
                output_amount: b2q_output,
                input_usd_equiv: usd,
            });

            if self.state.p > 0 {
                let dec_diff =
                    self.state.mint_a_decimals as i32 - self.state.mint_b_decimals as i32;
                let eff_price_scale = if dec_diff > 0 {
                    PRICE_SCALE * 10u128.pow(dec_diff as u32)
                } else {
                    PRICE_SCALE
                };
                let q2b_input =
                    (usd * 1_000_000.0) as u128 * eff_price_scale / self.state.p as u128;
                let q2b_input = q2b_input as u64;
                let q2b_output = self.state.get_quote(q2b_input, false);
                rows.push(QuoteRow {
                    direction: "Q2B".into(),
                    input_amount: q2b_input,
                    output_amount: q2b_output,
                    input_usd_equiv: usd,
                });
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
        self.has_pool && self.has_base_vault && self.has_quote_vault
    }
}
