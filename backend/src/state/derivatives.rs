use serde::{Deserialize, Serialize};

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
