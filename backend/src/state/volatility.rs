use serde::{Deserialize, Serialize};

/// Volatility regime derived from ATR-14 percentile relative to rolling candle history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VolatilityRegime {
    /// ATR below 20th percentile — tight range, breakout energy coiling.
    Compression,
    /// ATR 20th–70th percentile — balanced volatility.
    Normal,
    /// ATR 70th–90th percentile — elevated risk, widen stops.
    Elevated,
    /// ATR above 90th percentile — crisis/event conditions, avoid new entries.
    Extreme,
    /// Insufficient candle history for regime classification.
    Unknown,
}

/// Volatility regime state computed by `compute_volatility_regime` in quant.rs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolatilityState {
    pub regime: VolatilityRegime,
    /// Position of current ATR in rolling history (0–100). None if history too short.
    pub atr_percentile: Option<f64>,
    /// ATR direction: Some(true) = expanding, Some(false) = contracting, None = stable.
    pub expanding: Option<bool>,
    /// Classification confidence (rises with more historical samples).
    pub regime_confidence: f32,
}

impl Default for VolatilityState {
    fn default() -> Self {
        Self {
            regime: VolatilityRegime::Unknown,
            atr_percentile: None,
            expanding: None,
            regime_confidence: 0.0,
        }
    }
}
