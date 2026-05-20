use super::{ProtocolReplay, QuoteRow};

const MAX_TIERS: usize = 10;
const TIER_STRIDE: usize = 24;
const OFF_POS_CORRECTION: usize = 0x00;
const OFF_B2Q_POSITION_RAW: usize = 0x08;
const OFF_FEE_BPS: usize = 0x10;
const OFF_FP_B2Q: usize = 0x80;
const OFF_FP_Q2B: usize = 0x90;
const OFF_B2Q_TIER_TABLE: usize = 0xA0;
const OFF_Q2B_POSITION_RAW: usize = 0x278;
const OFF_Q2B_TIER_TABLE: usize = 0x280;
const FP_SCALE: u128 = 1_000_000_000_000_000;
const FEE_THING_SCALE: u128 = 1_000_000;
const MIN_POOL_DATA_SIZE: usize = OFF_Q2B_TIER_TABLE + TIER_STRIDE;

#[inline]
fn read_u64_le(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

#[inline]
fn read_u16_le(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap())
}

#[derive(Debug, Clone, Copy, Default)]
struct Tier {
    width: u64,
    fee_thing: u64,
    _flag: u64,
}

#[derive(Debug, Clone, Default)]
struct TesseraVState {
    fee_bps: u16,
    fp_b2q: u64,
    fp_q2b: u64,
    b2q_position: u64,
    q2b_position: u64,
    b2q_tiers: Vec<Tier>,
    q2b_tiers: Vec<Tier>,
    base_amount: u64,
    quote_amount: u64,
}

impl TesseraVState {
    fn update_from_pool_data(&mut self, data: &[u8]) -> Result<(), &'static str> {
        if data.len() < MIN_POOL_DATA_SIZE {
            return Err("Tessera V pool data too small");
        }
        let pos_correction = read_u64_le(data, OFF_POS_CORRECTION);
        let b2q_position_raw = read_u64_le(data, OFF_B2Q_POSITION_RAW);
        let q2b_position_raw = read_u64_le(data, OFF_Q2B_POSITION_RAW);
        self.fee_bps = read_u16_le(data, OFF_FEE_BPS);
        self.fp_b2q = read_u64_le(data, OFF_FP_B2Q);
        self.fp_q2b = read_u64_le(data, OFF_FP_Q2B);

        let b2q_correction = (pos_correction as u128 * self.fp_q2b as u128 / FP_SCALE) as u64;
        if b2q_correction > b2q_position_raw {
            self.b2q_position = 0;
            let b2q_conv = (b2q_position_raw as u128 * self.fp_b2q as u128 / FP_SCALE) as u64;
            self.q2b_position = q2b_position_raw
                .wrapping_add(pos_correction)
                .wrapping_sub(b2q_conv);
        } else {
            self.b2q_position = b2q_position_raw - b2q_correction;
            self.q2b_position = q2b_position_raw;
        }

        self.b2q_tiers.clear();
        for i in 0..MAX_TIERS {
            let off = OFF_B2Q_TIER_TABLE + i * TIER_STRIDE;
            if off + TIER_STRIDE > data.len() {
                break;
            }
            let width = read_u64_le(data, off);
            let fee_thing = read_u64_le(data, off + 8);
            if width == 0 && fee_thing == 0 {
                break;
            }
            let flag = read_u64_le(data, off + 16);
            self.b2q_tiers.push(Tier {
                width,
                fee_thing,
                _flag: flag,
            });
        }

        self.q2b_tiers.clear();
        for i in 0..MAX_TIERS {
            let off = OFF_Q2B_TIER_TABLE + i * TIER_STRIDE;
            if off + TIER_STRIDE > data.len() {
                break;
            }
            let width = read_u64_le(data, off);
            let fee_thing = read_u64_le(data, off + 8);
            if width == 0 && fee_thing == 0 {
                break;
            }
            let flag = read_u64_le(data, off + 16);
            self.q2b_tiers.push(Tier {
                width,
                fee_thing,
                _flag: flag,
            });
        }

        Ok(())
    }

    #[inline]
    fn tier_raw(fee_thing: u64, fp: u64, amount: u64) -> u64 {
        let mid = fee_thing as u128 * fp as u128 / FEE_THING_SCALE;
        (mid * amount as u128 / FP_SCALE) as u64
    }

    fn get_quote(&self, amount_in: u64, is_base_to_quote: bool) -> u64 {
        if amount_in == 0 {
            return 0;
        }
        let fp = if is_base_to_quote {
            self.fp_b2q
        } else {
            self.fp_q2b
        };
        let tiers = if is_base_to_quote {
            &self.b2q_tiers
        } else {
            &self.q2b_tiers
        };
        let position = if is_base_to_quote {
            self.b2q_position
        } else {
            self.q2b_position
        };

        let mut remaining = amount_in as u128;
        let mut total_raw: u128 = 0;
        let mut remaining_pos = position as u128;

        for tier in tiers {
            if remaining == 0 {
                break;
            }
            let width = tier.width as u128;
            if remaining_pos >= width {
                remaining_pos -= width;
                continue;
            }
            let effective = width - remaining_pos;
            remaining_pos = 0;
            let chunk = remaining.min(effective);
            total_raw += Self::tier_raw(tier.fee_thing, fp, chunk as u64) as u128;
            remaining -= chunk;
        }

        let fee_num = FEE_THING_SCALE;
        (total_raw * fee_num / FEE_THING_SCALE) as u64
    }
}

pub struct TesseraVReplay {
    state: TesseraVState,
    has_pool: bool,
    has_base_vault: bool,
    has_quote_vault: bool,
}

impl TesseraVReplay {
    pub fn new() -> Self {
        Self {
            state: TesseraVState::default(),
            has_pool: false,
            has_base_vault: false,
            has_quote_vault: false,
        }
    }
}

impl ProtocolReplay for TesseraVReplay {
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

    fn quote_single(&self, input_amount: u64, direction: &str, _slot: u64) -> Option<u64> {
        if !self.is_ready() || input_amount == 0 {
            return None;
        }
        let is_b2q = direction == "B2Q";
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
            let b2q_input = if self.state.fp_b2q > 0 {
                ((usd * 1e6 * 1e15) as u128 / self.state.fp_b2q as u128) as u64
            } else {
                0
            };
            let b2q_output = self.state.get_quote(b2q_input, true);
            rows.push(QuoteRow {
                direction: "B2Q".into(),
                input_amount: b2q_input,
                output_amount: b2q_output,
                input_usd_equiv: usd,
            });

            let q2b_input = (usd * 1_000_000.0) as u64;
            let q2b_output = self.state.get_quote(q2b_input, false);
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
