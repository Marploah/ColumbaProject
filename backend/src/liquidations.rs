use crate::state::derivatives::LiquidationState;
use serde::Deserialize;
use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_EVENTS: usize = 2000;
const WINDOW_5M_MS: i64 = 5 * 60 * 1_000;
const WINDOW_1M_MS: i64 = 60 * 1_000;

pub enum LiquidationSide {
    Long,
    Short,
}

pub struct LiquidationEvent {
    pub side: LiquidationSide,
    pub value_usd: f64,
    pub timestamp_ms: i64,
}

/// Rolling liquidation buffer. All time windows are pruned lazily on each `push`.
/// `snapshot()` computes 1m/5m windows, imbalance ratio, and velocity in O(n).
#[derive(Default)]
pub struct LiquidationAggregator {
    events: VecDeque<LiquidationEvent>,
    feed_healthy: bool,
}

impl LiquidationAggregator {
    pub fn mark_healthy(&mut self) {
        self.feed_healthy = true;
    }

    pub fn mark_unhealthy(&mut self) {
        self.feed_healthy = false;
    }

    pub fn push(&mut self, event: LiquidationEvent) {
        let now = now_ms();
        // Prune stale events before inserting.
        while self
            .events
            .front()
            .map(|e| now - e.timestamp_ms > WINDOW_5M_MS)
            .unwrap_or(false)
        {
            self.events.pop_front();
        }
        self.events.push_back(event);
        // Hard cap prevents unbounded growth on very active markets.
        if self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
    }

    pub fn snapshot(&self) -> LiquidationState {
        let now = now_ms();
        let mut long_1m = 0.0_f64;
        let mut short_1m = 0.0_f64;
        let mut long_5m = 0.0_f64;
        let mut short_5m = 0.0_f64;
        let mut count_5m: u32 = 0;

        for e in &self.events {
            let age_ms = now - e.timestamp_ms;
            if age_ms > WINDOW_5M_MS {
                continue;
            }
            count_5m += 1;
            match e.side {
                LiquidationSide::Long => {
                    long_5m += e.value_usd;
                    if age_ms <= WINDOW_1M_MS {
                        long_1m += e.value_usd;
                    }
                }
                LiquidationSide::Short => {
                    short_5m += e.value_usd;
                    if age_ms <= WINDOW_1M_MS {
                        short_1m += e.value_usd;
                    }
                }
            }
        }

        let imbalance_ratio = if short_5m > 0.0 {
            long_5m / short_5m
        } else {
            0.0
        };
        // Events per second over the 5m window.
        let velocity = count_5m as f64 / (WINDOW_5M_MS as f64 / 1_000.0);

        LiquidationState {
            long_1m,
            short_1m,
            long_5m,
            short_5m,
            imbalance_ratio,
            velocity,
            feed_healthy: self.feed_healthy,
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ── Wire deserialization for Binance !forceOrder@arr ───────────────────────

#[derive(Deserialize)]
struct ForceOrderMsg {
    o: ForceOrderData,
}

#[derive(Deserialize)]
struct ForceOrderData {
    #[serde(rename = "s")]
    symbol: String,
    /// BUY = short position liquidated; SELL = long position liquidated.
    #[serde(rename = "S")]
    side: String,
    /// Average fill price.
    #[serde(rename = "ap")]
    avg_price: String,
    /// Original quantity.
    #[serde(rename = "q")]
    quantity: String,
    /// Trade time in milliseconds.
    #[serde(rename = "T")]
    trade_time: i64,
}

/// Parse a raw `!forceOrder@arr` text frame and return a `LiquidationEvent`
/// if it matches `active_symbol` (case-insensitive) and is well-formed.
pub fn parse_force_order(text: &str, active_symbol: &str) -> Option<LiquidationEvent> {
    let msg: ForceOrderMsg = serde_json::from_str(text).ok()?;
    let o = msg.o;

    if !o.symbol.eq_ignore_ascii_case(active_symbol) {
        return None;
    }

    let avg_price: f64 = o.avg_price.parse().ok().filter(|p: &f64| p.is_finite() && *p > 0.0)?;
    let quantity: f64 = o.quantity.parse().ok().filter(|q: &f64| q.is_finite() && *q > 0.0)?;
    let value_usd = avg_price * quantity;

    // BUY order = exchange covering a SHORT position → short liquidated.
    // SELL order = exchange covering a LONG position → long liquidated.
    let side = match o.side.as_str() {
        "BUY" => LiquidationSide::Short,
        "SELL" => LiquidationSide::Long,
        _ => return None,
    };

    Some(LiquidationEvent {
        side,
        value_usd,
        timestamp_ms: o.trade_time,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(side: LiquidationSide, usd: f64, age_ms: i64) -> LiquidationEvent {
        LiquidationEvent {
            side,
            value_usd: usd,
            timestamp_ms: now_ms() - age_ms,
        }
    }

    #[test]
    fn aggregator_prunes_stale_events() {
        let mut agg = LiquidationAggregator::default();
        agg.mark_healthy();
        // Event older than 5m should not appear in snapshot.
        agg.push(make_event(LiquidationSide::Long, 100_000.0, WINDOW_5M_MS + 1_000));
        agg.push(make_event(LiquidationSide::Short, 50_000.0, 30_000)); // 30s ago
        let snap = agg.snapshot();
        assert_eq!(snap.long_5m, 0.0, "stale long should be pruned");
        assert!((snap.short_5m - 50_000.0).abs() < 1.0);
    }

    #[test]
    fn aggregator_1m_window_subset_of_5m() {
        let mut agg = LiquidationAggregator::default();
        agg.mark_healthy();
        agg.push(make_event(LiquidationSide::Long, 10_000.0, 30_000)); // 30s → in both
        agg.push(make_event(LiquidationSide::Long, 20_000.0, 120_000)); // 2m → 5m only
        let snap = agg.snapshot();
        assert!((snap.long_1m - 10_000.0).abs() < 1.0);
        assert!((snap.long_5m - 30_000.0).abs() < 1.0);
    }

    #[test]
    fn aggregator_imbalance_ratio_computed() {
        let mut agg = LiquidationAggregator::default();
        agg.mark_healthy();
        agg.push(make_event(LiquidationSide::Long, 200_000.0, 10_000));
        agg.push(make_event(LiquidationSide::Short, 100_000.0, 10_000));
        let snap = agg.snapshot();
        assert!((snap.imbalance_ratio - 2.0).abs() < 0.01, "expected ratio 2.0, got {}", snap.imbalance_ratio);
    }

    #[test]
    fn aggregator_hard_cap() {
        let mut agg = LiquidationAggregator::default();
        for _ in 0..MAX_EVENTS + 100 {
            agg.push(make_event(LiquidationSide::Short, 1_000.0, 10_000));
        }
        assert!(agg.events.len() <= MAX_EVENTS);
    }

    #[test]
    fn parse_force_order_sell_is_long_liquidated() {
        let json = r#"{"e":"forceOrder","E":1591089831983,"o":{"s":"BTCUSDT","S":"SELL","o":"LIMIT","f":"IOC","q":"0.014","p":"9910","ap":"9910","X":"FILLED","l":"0.014","z":"0.014","T":1591089831983}}"#;
        let event = parse_force_order(json, "BTCUSDT").expect("should parse");
        assert!(matches!(event.side, LiquidationSide::Long));
        assert!((event.value_usd - 9910.0 * 0.014).abs() < 0.01);
    }

    #[test]
    fn parse_force_order_buy_is_short_liquidated() {
        let json = r#"{"e":"forceOrder","E":1591089831983,"o":{"s":"BTCUSDT","S":"BUY","o":"LIMIT","f":"IOC","q":"1.0","p":"50000","ap":"50000","X":"FILLED","l":"1.0","z":"1.0","T":1591089831983}}"#;
        let event = parse_force_order(json, "BTCUSDT").expect("should parse");
        assert!(matches!(event.side, LiquidationSide::Short));
    }

    #[test]
    fn parse_force_order_wrong_symbol_returns_none() {
        let json = r#"{"e":"forceOrder","E":1591089831983,"o":{"s":"ETHUSDT","S":"SELL","o":"LIMIT","f":"IOC","q":"1.0","p":"3000","ap":"3000","X":"FILLED","l":"1.0","z":"1.0","T":1591089831983}}"#;
        assert!(parse_force_order(json, "BTCUSDT").is_none());
    }
}
