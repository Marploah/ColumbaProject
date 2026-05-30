use serde::{Deserialize, Serialize};

/// Per-exchange open interest snapshot used for cross-exchange divergence detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeOiEntry {
    /// Raw OI value as returned by the exchange (BTC notional for Binance/Bybit; contracts for OKX).
    pub oi: Option<f64>,
    /// Percentage change vs. the previous poll sample. None until two samples exist.
    pub change_pct: Option<f64>,
    /// false when the exchange has not returned a valid response in the last 3 consecutive polls.
    pub healthy: bool,
}

impl Default for ExchangeOiEntry {
    fn default() -> Self {
        Self { oi: None, change_pct: None, healthy: false }
    }
}

/// Aggregated open interest view across Binance, Bybit, and OKX.
/// Used to detect leveraged positioning that is isolated to one venue vs. broad.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GlobalOIState {
    pub binance: ExchangeOiEntry,
    pub bybit: ExchangeOiEntry,
    pub okx: ExchangeOiEntry,
    /// 0.0 = all exchanges agree; 1.0 = maximum disagreement (one rising, others falling).
    pub divergence_score: f64,
    /// Human-readable interpretation: "broad leverage expansion", "Binance OI rising alone", etc.
    pub divergence_label: String,
}

/// Relationship between the perpetual futures price and the Binance spot price.
/// Positive basis = perp above spot (contango); negative = perp below spot (backwardation).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BasisRegime {
    StrongContango,
    MildContango,
    Neutral,
    MildBackwardation,
    StrongBackwardation,
    Unavailable,
}

/// Perpetual vs spot basis derived from the Binance spot price REST poll (10s).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasisState {
    pub spot_price: Option<f64>,
    /// (perp_price - spot_price) / spot_price * 100. None when spot feed is unavailable.
    pub basis_pct: Option<f64>,
    pub regime: BasisRegime,
    pub feed_healthy: bool,
}

impl Default for BasisState {
    fn default() -> Self {
        Self {
            spot_price: None,
            basis_pct: None,
            regime: BasisRegime::Unavailable,
            feed_healthy: false,
        }
    }
}

/// Rolling liquidation metrics computed from the !forceOrder@arr stream.
/// All USD values represent notional (avg_price × quantity).
/// Windows are time-based (1m / 5m) and pruned on each push.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidationState {
    /// Notional USD of long positions liquidated in the last 1m.
    pub long_1m: f64,
    /// Notional USD of short positions liquidated in the last 1m.
    pub short_1m: f64,
    /// Notional USD of long positions liquidated in the last 5m.
    pub long_5m: f64,
    /// Notional USD of short positions liquidated in the last 5m.
    pub short_5m: f64,
    /// long_5m / short_5m. > 1.0 = longs squeezed harder; < 1.0 = shorts squeezed harder.
    pub imbalance_ratio: f64,
    /// Events per second over the last 5m window.
    pub velocity: f64,
    /// false when the forceOrder stream is not connected or has failed.
    pub feed_healthy: bool,
}

impl Default for LiquidationState {
    fn default() -> Self {
        Self {
            long_1m: 0.0,
            short_1m: 0.0,
            long_5m: 0.0,
            short_5m: 0.0,
            imbalance_ratio: 0.0,
            velocity: 0.0,
            feed_healthy: false,
        }
    }
}
