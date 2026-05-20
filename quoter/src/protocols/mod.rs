pub mod alphaq;
pub mod bisonfi;
pub mod clmm_math;
pub mod goonfi;
pub mod humidifi;
pub mod orca_clmm;
pub mod raydium_clmm;
pub mod solfiv2;
pub mod tesserav;
pub mod zerofi;

#[derive(Debug, Clone)]
pub struct QuoteRow {
    pub direction: String,

    pub input_amount: u64,

    pub output_amount: u64,

    pub input_usd_equiv: f64,
}

pub trait ProtocolReplay: Send {
    fn apply_update(&mut self, role: &str, data: &[u8], slot: u64);

    fn compute_quotes(&self, slot: u64, tiers_usd: &[f64]) -> Vec<QuoteRow>;

    fn quote_single(&self, input_amount: u64, direction: &str, slot: u64) -> Option<u64>;

    fn vault_balances(&self) -> Option<(u64, u64)>;

    fn is_ready(&self) -> bool;
}

pub fn create_protocol(amm_type: &str) -> anyhow::Result<Box<dyn ProtocolReplay>> {
    match amm_type {
        "humidifi" => Ok(Box::new(humidifi::HumidifiReplay::new())),
        "raydium_clmm" => Ok(Box::new(raydium_clmm::RaydiumClmmReplay::new())),
        "orca_clmm" => Ok(Box::new(orca_clmm::OrcaClmmReplay::new())),
        "alphaq" => Ok(Box::new(alphaq::AlphaQReplay::new())),
        "bisonfi" => Ok(Box::new(bisonfi::BisonFiReplay::new())),
        "solfiv2" => Ok(Box::new(solfiv2::SolFiV2Replay::new())),
        "goonfi" => Ok(Box::new(goonfi::GoonFiReplay::new())),
        "tesserav" => Ok(Box::new(tesserav::TesseraVReplay::new())),
        "zerofi" => Ok(Box::new(zerofi::ZeroFiReplay::new())),
        other => anyhow::bail!("unsupported amm_type: {other}"),
    }
}
