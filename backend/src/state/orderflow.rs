use serde::{Deserialize, Serialize};

/// Advanced orderflow signals derived from the per-candle buy/sell volume split.
/// All signals are computed from the existing aggTrade stream via the candle buffer —
/// no additional data sources are required.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderflowState {
    /// Net buy − sell volume of the current (live) candle in base-asset units.
    /// Positive = net buying pressure in the open candle; negative = net selling.
    pub current_delta: f64,
    /// Linear-regression slope of per-candle deltas over the last 10 closed candles.
    /// Positive = buying is accelerating across candles; negative = selling accelerating.
    pub delta_momentum: f64,
    /// Buy volume as a fraction of total volume, averaged over the last 5 closed candles.
    /// 0.5 = perfectly balanced; > 0.55 = buy pressure; < 0.45 = sell pressure.
    pub buy_pressure_pct: f64,
    /// True when the last closed candle shows elevated volume but a tight price range
    /// near a known wall level — classic high-volume absorption pattern.
    pub absorption_detected: bool,
    /// True when the last closed candle has a disproportionately large wick vs ATR,
    /// suggesting a price sweep followed by rejection.
    pub sweep_detected: bool,
    /// Direction of the detected sweep: "bid" = lower wick rejection (potential bear trap),
    /// "ask" = upper wick rejection (potential bull trap). None when sweep_detected is false.
    pub sweep_direction: Option<String>,
    /// false when cvd_seeded is false; signals are approximate from kline-seeded CVD.
    pub feed_healthy: bool,
}

impl Default for OrderflowState {
    fn default() -> Self {
        Self {
            current_delta: 0.0,
            delta_momentum: 0.0,
            buy_pressure_pct: 0.5,
            absorption_detected: false,
            sweep_detected: false,
            sweep_direction: None,
            feed_healthy: false,
        }
    }
}
