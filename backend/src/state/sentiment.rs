use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FearGreedState {
    /// Fear & Greed Index value 0–100 (0 = Extreme Fear, 100 = Extreme Greed).
    pub value: u8,
    /// Human-readable classification from the API (e.g. "Extreme Fear", "Greed").
    pub classification: String,
    /// Unix timestamp (seconds) when this reading was published by alternative.me.
    pub timestamp: i64,
    /// false until the first successful poll or when the feed fails.
    pub feed_healthy: bool,
}

impl Default for FearGreedState {
    fn default() -> Self {
        Self {
            value: 50,
            classification: "Neutral".to_string(),
            timestamp: 0,
            feed_healthy: false,
        }
    }
}
