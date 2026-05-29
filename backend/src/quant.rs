use crate::state::derivatives::LiquidationState;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use ta::indicators::{AverageTrueRange, RelativeStrengthIndex};
use ta::{DataItem, Next};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandleData {
    pub timestamp: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub buy_volume: Decimal,
    pub sell_volume: Decimal,
    pub cvd: Decimal,
    pub open_interest: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LiquidityWalls {
    pub bid_wall_price: Option<f64>,
    pub bid_wall_size: Option<f64>,
    pub ask_wall_price: Option<f64>,
    pub ask_wall_size: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfluenceMatrix {
    pub tf_5m: String,
    pub tf_15m: String,
    pub tf_1h: String,
    pub tf_4h: String,
    pub aggregate_bias: String,
    pub cvd_slope: f64,
    pub oi_change_pct: f64,
    pub rsi_divergence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedMarketState {
    pub symbol: String,
    pub last_price: f64,
    pub candles: Vec<CandleData>,
    pub confluence: ConfluenceMatrix,
    pub liquidity_walls: LiquidityWalls,
    pub atr_14: Option<f64>,
    pub volatility_upper_limit: Option<f64>,
    pub volatility_lower_limit: Option<f64>,
    pub long_short_indicator: String,
    /// false when open_interest fields contain quote_volume as proxy (Binance OI fetch failed)
    pub oi_is_real: bool,
    /// Current 8-hour perpetual funding rate (e.g. 0.0001 = 0.01%). None until first poll.
    pub funding_rate: Option<f64>,
    /// Hours remaining until the next 8-hour funding settlement (00:00, 08:00, 16:00 UTC).
    pub funding_hours_to_settlement: f64,
    /// Volume-weighted average price over the current candle window. None if no volume.
    pub vwap: Option<f64>,
    /// false until at least one full live-stream candle has closed after startup or symbol/interval
    /// reset. When false, CVD is seeded from klines (different source than live aggTrade stream)
    /// and directional signals may be imprecise.
    pub cvd_seeded: bool,
    /// Rolling liquidation metrics from the Binance !forceOrder@arr stream.
    /// feed_healthy=false until the stream connects; all USD values are zero until then.
    pub liquidations: LiquidationState,
}

#[derive(Debug, Clone)]
pub struct TfBiases {
    pub tf_5m: String,
    pub tf_15m: String,
    pub tf_1h: String,
    pub tf_4h: String,
}

impl Default for TfBiases {
    fn default() -> Self {
        Self {
            tf_5m: "Neutral".to_string(),
            tf_15m: "Neutral".to_string(),
            tf_1h: "Neutral".to_string(),
            tf_4h: "Neutral".to_string(),
        }
    }
}

/// Cache for O(n) signals that only change on candle close (ATR-14, RSI divergence).
/// Key: total candle count — a new candle opening increments it, invalidating the cache.
#[derive(Default)]
pub struct SignalCache {
    candle_count: usize,
    atr_14: Option<f64>,
    rsi_divergence: String,
}

impl SignalCache {
    /// Force recompute on next call (use after symbol/interval reset).
    pub fn invalidate(&mut self) {
        self.candle_count = 0;
    }

    fn get_or_refresh(&mut self, candles: &[CandleData]) -> (Option<f64>, String) {
        if self.candle_count != candles.len() {
            self.atr_14 = calculate_atr_14(candles);
            self.rsi_divergence = detect_rsi_divergence(candles);
            self.candle_count = candles.len();
        }
        (self.atr_14, self.rsi_divergence.clone())
    }
}

pub fn decimal_from_trade_qty(qty: f64) -> Decimal {
    Decimal::from_f64(qty).unwrap_or(Decimal::ZERO)
}

pub fn apply_cvd_trade_delta(
    previous_cvd: Decimal,
    trade_quantity: Decimal,
    buyer_is_maker: bool,
) -> Decimal {
    if buyer_is_maker {
        previous_cvd - trade_quantity
    } else {
        previous_cvd + trade_quantity
    }
}

pub fn compute_cvd_slope(candles: &[CandleData]) -> f64 {
    let window_size = candles.len().min(20);

    if window_size < 2 {
        return 0.0;
    }

    let window = &candles[candles.len() - window_size..];
    let n = window_size as f64;
    let mean_x = (n - 1.0) / 2.0;
    let mean_y = window
        .iter()
        .map(|candle| candle.cvd.to_f64().unwrap_or(0.0))
        .sum::<f64>()
        / n;

    let mut numerator = 0.0;
    let mut denominator = 0.0;

    for (idx, candle) in window.iter().enumerate() {
        let x = idx as f64;
        let y = candle.cvd.to_f64().unwrap_or(0.0);

        numerator += (x - mean_x) * (y - mean_y);
        denominator += (x - mean_x).powi(2);
    }

    if denominator.abs() < f64::EPSILON {
        0.0
    } else {
        numerator / denominator
    }
}

/// Computes a symbol-aware CVD divergence threshold (1 sigma of CVD values converted
/// to slope units). Prevents the hardcoded ±5.0 from being meaningless on high-volume
/// symbols (BTC) or never-triggering on low-volume alts.
pub fn compute_cvd_divergence_threshold(candles: &[CandleData]) -> f64 {
    let window = candles.len().min(20);
    if window < 2 {
        return 5.0;
    }
    let slice = &candles[candles.len() - window..];
    let cvd_vals: Vec<f64> = slice
        .iter()
        .map(|c| c.cvd.to_f64().unwrap_or(0.0))
        .collect();
    let mean = cvd_vals.iter().sum::<f64>() / cvd_vals.len() as f64;
    let variance = cvd_vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>()
        / cvd_vals.len() as f64;
    // sigma / window converts CVD magnitude to per-candle slope units; floor at 1.0
    (variance.sqrt() / window as f64).max(1.0)
}

pub fn calculate_long_short_indicator(
    price_trend_up: bool,
    cvd_slope: f64,
    oi_change_pct: f64,
    rsi_div: &str,
    cvd_divergence_threshold: f64,
) -> String {
    let rsi_div = rsi_div.to_ascii_lowercase();

    // Strong: price + CVD + OI all agree (OI expanding confirms real interest)
    if price_trend_up && cvd_slope > 0.0 && oi_change_pct > 0.5 {
        return "StrongLong".to_string();
    }
    if !price_trend_up && cvd_slope < 0.0 && oi_change_pct > 0.5 {
        return "StrongShort".to_string();
    }

    // Weak: price trend + CVD agree — OI direction doesn't gate the signal
    if price_trend_up && cvd_slope > 0.0 && !rsi_div.contains("bearish") {
        return "WeakLong".to_string();
    }
    if !price_trend_up && cvd_slope < 0.0 && !rsi_div.contains("bullish") {
        return "WeakShort".to_string();
    }

    // CVD/price divergence: smart money flow contradicts price action
    // Price rising but buyers drying up hard = distribution = bearish
    if price_trend_up && cvd_slope < -cvd_divergence_threshold && !rsi_div.contains("bullish") {
        return "WeakShort".to_string();
    }
    // Price falling but buyers stepping in hard = accumulation = bullish
    if !price_trend_up && cvd_slope > cvd_divergence_threshold && !rsi_div.contains("bearish") {
        return "WeakLong".to_string();
    }

    // RSI divergence alone as last resort
    if rsi_div.contains("bullish") {
        return "WeakLong".to_string();
    }
    if rsi_div.contains("bearish") {
        return "WeakShort".to_string();
    }

    "Neutral".to_string()
}

pub fn calculate_atr_14(candles: &[CandleData]) -> Option<f64> {
    if candles.len() < 14 {
        return None;
    }

    let mut atr = AverageTrueRange::new(14).ok()?;
    let mut latest_atr = None;

    for candle in candles {
        let item = DataItem::builder()
            .open(candle.open)
            .high(candle.high)
            .low(candle.low)
            .close(candle.close)
            .volume(candle.volume)
            .build()
            .ok()?;

        latest_atr = Some(atr.next(&item));
    }

    latest_atr.filter(|value| value.is_finite())
}

pub fn calculate_volatility_limits(
    last_price: f64,
    atr_14: Option<f64>,
) -> (Option<f64>, Option<f64>) {
    match atr_14 {
        Some(atr) if atr.is_finite() && atr > 0.0 && last_price.is_finite() => {
            let multiplier = 1.5;
            (
                Some(last_price + atr * multiplier),
                Some(last_price - atr * multiplier),
            )
        }
        _ => (None, None),
    }
}

pub fn infer_price_trend(candles: &[CandleData]) -> bool {
    let window_size = candles.len().min(20);

    if window_size < 2 {
        return false;
    }

    let window = &candles[candles.len() - window_size..];
    let first = window
        .first()
        .map(|candle| candle.close)
        .unwrap_or_default();
    let last = window.last().map(|candle| candle.close).unwrap_or_default();

    last >= first
}

pub fn calculate_open_interest_change_pct(candles: &[CandleData]) -> f64 {
    let values: Vec<f64> = candles
        .iter()
        .filter_map(|candle| candle.open_interest)
        .filter(|value| value.is_finite() && *value > 0.0)
        .collect();

    if values.len() < 2 {
        return 0.0;
    }

    let first = values.first().copied().unwrap_or_default();
    let last = values.last().copied().unwrap_or_default();

    if first.abs() < f64::EPSILON {
        0.0
    } else {
        ((last - first) / first) * 100.0
    }
}

fn compute_rsi_series(candles: &[CandleData], period: usize) -> Vec<Option<f64>> {
    let Ok(mut rsi) = RelativeStrengthIndex::new(period) else {
        return vec![None; candles.len()];
    };
    candles
        .iter()
        .map(|c| {
            let item = DataItem::builder()
                .open(c.open)
                .high(c.high)
                .low(c.low)
                .close(c.close)
                .volume(c.volume)
                .build()
                .ok()?;
            let v = rsi.next(&item);
            if v.is_finite() { Some(v) } else { None }
        })
        .collect()
}

/// Detects RSI divergence by comparing price extremes vs RSI at those extremes
/// across two equal halves of the last 30 candles.
/// Returns "bullish", "bearish", or "none".
pub fn detect_rsi_divergence(candles: &[CandleData]) -> String {
    let window = candles.len().min(30);
    if window < 20 {
        return "none".to_string();
    }
    let slice = &candles[candles.len() - window..];
    let rsi_vals = compute_rsi_series(slice, 14);

    let pairs: Vec<(f64, f64)> = slice
        .iter()
        .zip(rsi_vals.iter())
        .filter_map(|(c, r)| r.map(|rv| (c.close, rv)))
        .collect();

    if pairs.len() < 10 {
        return "none".to_string();
    }

    let mid = pairs.len() / 2;
    let first = &pairs[..mid];
    let second = &pairs[mid..];

    // RSI value at the price high in each half (bearish divergence check)
    let first_rsi_at_high = first
        .iter()
        .max_by(|(c1, _), (c2, _)| c1.partial_cmp(c2).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, r)| *r)
        .unwrap_or(0.0);
    let second_rsi_at_high = second
        .iter()
        .max_by(|(c1, _), (c2, _)| c1.partial_cmp(c2).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, r)| *r)
        .unwrap_or(0.0);

    let first_max_close = first.iter().map(|(c, _)| *c).fold(f64::NEG_INFINITY, f64::max);
    let second_max_close = second.iter().map(|(c, _)| *c).fold(f64::NEG_INFINITY, f64::max);

    // RSI value at the price low in each half (bullish divergence check)
    let first_rsi_at_low = first
        .iter()
        .min_by(|(c1, _), (c2, _)| c1.partial_cmp(c2).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, r)| *r)
        .unwrap_or(100.0);
    let second_rsi_at_low = second
        .iter()
        .min_by(|(c1, _), (c2, _)| c1.partial_cmp(c2).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, r)| *r)
        .unwrap_or(100.0);

    let first_min_close = first.iter().map(|(c, _)| *c).fold(f64::INFINITY, f64::min);
    let second_min_close = second.iter().map(|(c, _)| *c).fold(f64::INFINITY, f64::min);

    // Bearish: price makes higher high but RSI makes lower high
    if second_max_close > first_max_close && second_rsi_at_high < first_rsi_at_high {
        return "bearish".to_string();
    }

    // Bullish: price makes lower low but RSI makes higher low
    if second_min_close < first_min_close && second_rsi_at_low > first_rsi_at_low {
        return "bullish".to_string();
    }

    "none".to_string()
}

pub fn compute_tf_bias(candles: &[CandleData]) -> String {
    if candles.len() < 2 {
        return "Neutral".to_string();
    }
    let rsi_div = detect_rsi_divergence(candles);
    let price_trend_up = infer_price_trend(candles);
    let cvd_slope = compute_cvd_slope(candles);
    let cvd_threshold = compute_cvd_divergence_threshold(candles);
    // TF klines from fetch_recent_klines always carry quote_volume as OI proxy,
    // never real open interest. Pass 0.0 so OI-gated Strong* signals are disabled.
    calculate_long_short_indicator(price_trend_up, cvd_slope, 0.0, &rsi_div, cvd_threshold)
}

/// Hours until the next 8-hour perpetual funding settlement (00:00, 08:00, 16:00 UTC).
pub fn hours_to_next_funding_settlement() -> f64 {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let period_ms: i64 = 8 * 3_600_000;
    let ms_into_period = now_ms % period_ms;
    let ms_remaining = period_ms - ms_into_period;
    ms_remaining as f64 / 3_600_000.0
}

/// Session VWAP from all candles in the window.
/// Returns None if total volume is zero.
pub fn calculate_vwap(candles: &[CandleData]) -> Option<f64> {
    let mut sum_pv = 0.0_f64;
    let mut sum_v = 0.0_f64;

    for c in candles {
        let typical = (c.high + c.low + c.close) / 3.0;
        sum_pv += typical * c.volume;
        sum_v += c.volume;
    }

    if sum_v > 0.0 && sum_pv.is_finite() {
        Some(sum_pv / sum_v)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::prelude::FromPrimitive;

    fn make_candle(close: f64, cvd: f64) -> CandleData {
        CandleData {
            timestamp: 0,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
            buy_volume: Decimal::ZERO,
            sell_volume: Decimal::ZERO,
            cvd: Decimal::from_f64(cvd).unwrap_or(Decimal::ZERO),
            open_interest: None,
        }
    }

    fn make_candle_ohlcv(open: f64, high: f64, low: f64, close: f64, volume: f64) -> CandleData {
        CandleData {
            timestamp: 0,
            open,
            high,
            low,
            close,
            volume,
            buy_volume: Decimal::ZERO,
            sell_volume: Decimal::ZERO,
            cvd: Decimal::ZERO,
            open_interest: None,
        }
    }

    fn make_candle_with_oi(close: f64, oi: f64) -> CandleData {
        let mut c = make_candle(close, 0.0);
        c.open_interest = Some(oi);
        c
    }

    // ── CVD accumulation ────────────────────────────────────────────────────────

    #[test]
    fn cvd_taker_buy_increases_cvd() {
        let result = apply_cvd_trade_delta(
            Decimal::ZERO,
            Decimal::from_f64(1.5).unwrap(),
            false, // buyer_is_maker=false → taker buy
        );
        assert_eq!(result, Decimal::from_f64(1.5).unwrap());
    }

    #[test]
    fn cvd_taker_sell_decreases_cvd() {
        let result = apply_cvd_trade_delta(
            Decimal::from_f64(10.0).unwrap(),
            Decimal::from_f64(3.0).unwrap(),
            true, // buyer_is_maker=true → taker sell
        );
        assert_eq!(result, Decimal::from_f64(7.0).unwrap());
    }

    #[test]
    fn cvd_accumulates_correctly_across_trades() {
        // buy 5, sell 2, buy 3 → net +6
        let mut cvd = Decimal::ZERO;
        cvd = apply_cvd_trade_delta(cvd, Decimal::from_f64(5.0).unwrap(), false);
        cvd = apply_cvd_trade_delta(cvd, Decimal::from_f64(2.0).unwrap(), true);
        cvd = apply_cvd_trade_delta(cvd, Decimal::from_f64(3.0).unwrap(), false);
        assert_eq!(cvd, Decimal::from_f64(6.0).unwrap());
    }

    #[test]
    fn decimal_from_trade_qty_nan_returns_zero() {
        assert_eq!(decimal_from_trade_qty(f64::NAN), Decimal::ZERO);
    }

    // ── CVD slope (linear regression) ───────────────────────────────────────────

    #[test]
    fn cvd_slope_empty_returns_zero() {
        assert_eq!(compute_cvd_slope(&[]), 0.0);
    }

    #[test]
    fn cvd_slope_single_candle_returns_zero() {
        assert_eq!(compute_cvd_slope(&[make_candle(100.0, 5.0)]), 0.0);
    }

    #[test]
    fn cvd_slope_flat_returns_zero() {
        let candles: Vec<CandleData> = (0..10).map(|_| make_candle(100.0, 5.0)).collect();
        let slope = compute_cvd_slope(&candles);
        assert!(slope.abs() < 1e-9, "flat CVD should yield slope≈0, got {slope}");
    }

    #[test]
    fn cvd_slope_linearly_increasing_returns_one() {
        // CVD=[0,1,2,...,9] → OLS slope=1.0
        let candles: Vec<CandleData> = (0..10).map(|i| make_candle(100.0, i as f64)).collect();
        let slope = compute_cvd_slope(&candles);
        assert!((slope - 1.0).abs() < 1e-9, "expected slope 1.0, got {slope}");
    }

    #[test]
    fn cvd_slope_linearly_decreasing_returns_negative_one() {
        // CVD=[9,8,...,0] → OLS slope=-1.0
        let candles: Vec<CandleData> = (0..10)
            .map(|i| make_candle(100.0, (9 - i) as f64))
            .collect();
        let slope = compute_cvd_slope(&candles);
        assert!((slope + 1.0).abs() < 1e-9, "expected slope -1.0, got {slope}");
    }

    #[test]
    fn cvd_slope_uses_last_20_candles() {
        // 25 candles: first 5 CVD=0 (noise), last 20 CVD=[0..19] (slope=1.0)
        let mut candles: Vec<CandleData> = (0..5).map(|_| make_candle(100.0, 0.0)).collect();
        candles.extend((0..20).map(|i| make_candle(100.0, i as f64)));
        let slope = compute_cvd_slope(&candles);
        assert!((slope - 1.0).abs() < 1e-9, "expected slope 1.0 from last-20 window, got {slope}");
    }

    // ── CVD divergence threshold ─────────────────────────────────────────────────

    #[test]
    fn cvd_threshold_flat_cvd_floors_at_one() {
        let candles: Vec<CandleData> = (0..20).map(|_| make_candle(100.0, 5.0)).collect();
        let t = compute_cvd_divergence_threshold(&candles);
        assert!((t - 1.0).abs() < 1e-9, "zero-variance CVD should floor at 1.0, got {t}");
    }

    #[test]
    fn cvd_threshold_too_few_candles_returns_default() {
        assert_eq!(compute_cvd_divergence_threshold(&[]), 5.0);
        assert_eq!(compute_cvd_divergence_threshold(&[make_candle(100.0, 0.0)]), 5.0);
    }

    // ── Long/short indicator ─────────────────────────────────────────────────────

    #[test]
    fn long_short_strong_long() {
        assert_eq!(
            calculate_long_short_indicator(true, 1.0, 1.0, "none", 5.0),
            "StrongLong"
        );
    }

    #[test]
    fn long_short_strong_short() {
        assert_eq!(
            calculate_long_short_indicator(false, -1.0, 1.0, "none", 5.0),
            "StrongShort"
        );
    }

    #[test]
    fn long_short_weak_long_without_oi_expansion() {
        // price up + CVD up, OI not expanding → WeakLong not StrongLong
        assert_eq!(
            calculate_long_short_indicator(true, 1.0, 0.0, "none", 5.0),
            "WeakLong"
        );
    }

    #[test]
    fn long_short_weak_short_without_oi_expansion() {
        assert_eq!(
            calculate_long_short_indicator(false, -1.0, 0.0, "none", 5.0),
            "WeakShort"
        );
    }

    #[test]
    fn long_short_distribution_divergence_yields_weak_short() {
        // price rising but CVD strongly negative = distribution
        assert_eq!(
            calculate_long_short_indicator(true, -6.0, 0.0, "none", 5.0),
            "WeakShort"
        );
    }

    #[test]
    fn long_short_accumulation_divergence_yields_weak_long() {
        // price falling but CVD strongly positive = accumulation
        assert_eq!(
            calculate_long_short_indicator(false, 6.0, 0.0, "none", 5.0),
            "WeakLong"
        );
    }

    #[test]
    fn long_short_rsi_bullish_fallback() {
        assert_eq!(
            calculate_long_short_indicator(false, 0.0, 0.0, "bullish", 5.0),
            "WeakLong"
        );
    }

    #[test]
    fn long_short_rsi_bearish_fallback() {
        assert_eq!(
            calculate_long_short_indicator(true, 0.0, 0.0, "bearish", 5.0),
            "WeakShort"
        );
    }

    #[test]
    fn long_short_neutral_when_no_signal() {
        // price up, CVD slightly negative (below threshold), no RSI divergence
        assert_eq!(
            calculate_long_short_indicator(true, -1.0, 0.0, "none", 5.0),
            "Neutral"
        );
    }

    #[test]
    fn long_short_bearish_rsi_blocks_weak_long() {
        // price up + CVD up, but bearish RSI divergence blocks WeakLong → WeakShort via RSI fallback
        assert_eq!(
            calculate_long_short_indicator(true, 1.0, 0.0, "bearish", 5.0),
            "WeakShort"
        );
    }

    // ── ATR-14 ──────────────────────────────────────────────────────────────────

    #[test]
    fn atr_14_none_with_fewer_than_14_candles() {
        let candles: Vec<CandleData> = (0..13)
            .map(|_| make_candle_ohlcv(100.0, 110.0, 90.0, 100.0, 1.0))
            .collect();
        assert!(calculate_atr_14(&candles).is_none());
    }

    #[test]
    fn atr_14_some_with_sufficient_candles() {
        let candles: Vec<CandleData> = (0..20)
            .map(|i| make_candle_ohlcv(100.0 + i as f64, 110.0 + i as f64, 90.0 + i as f64, 100.0 + i as f64, 1.0))
            .collect();
        let atr = calculate_atr_14(&candles);
        assert!(atr.is_some(), "expected Some(ATR) with 20 candles");
        assert!(atr.unwrap() > 0.0, "ATR should be positive for candles with range");
    }

    #[test]
    fn atr_14_constant_range_converges_to_true_range() {
        // high=110, low=90, close=100 → TR=20 every candle; after 30 candles ATR≈20
        let candles: Vec<CandleData> = (0..30)
            .map(|_| make_candle_ohlcv(100.0, 110.0, 90.0, 100.0, 1.0))
            .collect();
        let atr = calculate_atr_14(&candles).expect("should compute ATR with 30 candles");
        assert!(
            (atr - 20.0).abs() < 1.0,
            "ATR should converge to ~20 for constant TR=20, got {atr}"
        );
    }

    // ── Volatility limits ────────────────────────────────────────────────────────

    #[test]
    fn volatility_limits_correct_with_valid_atr() {
        let (upper, lower) = calculate_volatility_limits(1000.0, Some(100.0));
        assert_eq!(upper, Some(1150.0)); // 1000 + 1.5×100
        assert_eq!(lower, Some(850.0));  // 1000 − 1.5×100
    }

    #[test]
    fn volatility_limits_none_when_atr_none() {
        let (upper, lower) = calculate_volatility_limits(1000.0, None);
        assert!(upper.is_none() && lower.is_none());
    }

    #[test]
    fn volatility_limits_none_when_atr_zero() {
        let (upper, lower) = calculate_volatility_limits(1000.0, Some(0.0));
        assert!(upper.is_none() && lower.is_none());
    }

    // ── Price trend ──────────────────────────────────────────────────────────────

    #[test]
    fn price_trend_up_when_last_close_higher() {
        let candles: Vec<CandleData> = [1.0, 2.0, 3.0, 4.0, 5.0]
            .iter()
            .map(|&c| make_candle(c, 0.0))
            .collect();
        assert!(infer_price_trend(&candles));
    }

    #[test]
    fn price_trend_down_when_last_close_lower() {
        let candles: Vec<CandleData> = [5.0, 4.0, 3.0, 2.0, 1.0]
            .iter()
            .map(|&c| make_candle(c, 0.0))
            .collect();
        assert!(!infer_price_trend(&candles));
    }

    #[test]
    fn price_trend_flat_returns_true() {
        let candles = vec![make_candle(100.0, 0.0), make_candle(100.0, 0.0)];
        assert!(infer_price_trend(&candles)); // last >= first
    }

    #[test]
    fn price_trend_single_candle_returns_false() {
        assert!(!infer_price_trend(&[make_candle(100.0, 0.0)]));
    }

    // ── Open interest change % ───────────────────────────────────────────────────

    #[test]
    fn oi_change_pct_increasing() {
        let candles = vec![
            make_candle_with_oi(100.0, 1000.0),
            make_candle_with_oi(100.0, 1100.0),
        ];
        let pct = calculate_open_interest_change_pct(&candles);
        assert!((pct - 10.0).abs() < 1e-9, "expected 10%, got {pct}");
    }

    #[test]
    fn oi_change_pct_decreasing() {
        let candles = vec![
            make_candle_with_oi(100.0, 1000.0),
            make_candle_with_oi(100.0, 900.0),
        ];
        let pct = calculate_open_interest_change_pct(&candles);
        assert!((pct + 10.0).abs() < 1e-9, "expected -10%, got {pct}");
    }

    #[test]
    fn oi_change_pct_no_oi_data_returns_zero() {
        let candles = vec![make_candle(100.0, 0.0), make_candle(100.0, 0.0)];
        assert_eq!(calculate_open_interest_change_pct(&candles), 0.0);
    }

    #[test]
    fn oi_change_pct_single_value_returns_zero() {
        assert_eq!(
            calculate_open_interest_change_pct(&[make_candle_with_oi(100.0, 1000.0)]),
            0.0
        );
    }

    // ── oi_is_real gating in build_unified_market_state ──────────────────────────

    #[test]
    fn proxy_oi_does_not_produce_strong_long() {
        // Without real OI, even expanding quote_volume should not yield StrongLong
        let mut candles: Vec<CandleData> = (0..20)
            .map(|i| {
                let mut c = make_candle_ohlcv(
                    100.0 + i as f64, 101.0 + i as f64,
                    99.0 + i as f64,  100.0 + i as f64, 1.0
                );
                c.cvd = Decimal::from_f64(i as f64).unwrap(); // rising CVD
                c.open_interest = Some(1000.0 + i as f64 * 10.0); // expanding OI proxy
                c
            })
            .collect();
        // Give candles a rising CVD trend so without oi_is_real=false this would be StrongLong
        let oi_change_pct = calculate_open_interest_change_pct(&candles);
        assert!(oi_change_pct > 0.5, "test setup: OI proxy should appear expanding");

        let state = build_unified_market_state(
            "BTCUSDT".to_string(),
            candles,
            LiquidityWalls::default(),
            None,
            false, // oi_is_real = false
            None,
            None,
            false,
            crate::state::derivatives::LiquidationState::default(),
        );
        assert_ne!(
            state.long_short_indicator, "StrongLong",
            "proxy OI should not produce StrongLong, got {}",
            state.long_short_indicator
        );
        assert_ne!(
            state.long_short_indicator, "StrongShort",
            "proxy OI should not produce StrongShort, got {}",
            state.long_short_indicator
        );
    }

    // ── VWAP ────────────────────────────────────────────────────────────────────

    #[test]
    fn vwap_single_candle_equals_typical_price() {
        // typical = (120 + 80 + 100) / 3 = 100
        let c = make_candle_ohlcv(100.0, 120.0, 80.0, 100.0, 10.0);
        let vwap = calculate_vwap(&[c]).expect("should compute VWAP");
        assert!((vwap - 100.0).abs() < 1e-9, "expected VWAP=100, got {vwap}");
    }

    #[test]
    fn vwap_weighted_by_volume() {
        // C1: typical=100, vol=1 → pv=100
        // C2: typical=200, vol=3 → pv=600
        // VWAP = 700/4 = 175
        let c1 = make_candle_ohlcv(100.0, 100.0, 100.0, 100.0, 1.0);
        let c2 = make_candle_ohlcv(200.0, 200.0, 200.0, 200.0, 3.0);
        let vwap = calculate_vwap(&[c1, c2]).expect("should compute VWAP");
        assert!((vwap - 175.0).abs() < 1e-9, "expected VWAP=175, got {vwap}");
    }

    #[test]
    fn vwap_zero_volume_returns_none() {
        let c = make_candle_ohlcv(100.0, 110.0, 90.0, 100.0, 0.0);
        assert!(calculate_vwap(&[c]).is_none());
    }

    #[test]
    fn vwap_empty_candles_returns_none() {
        assert!(calculate_vwap(&[]).is_none());
    }
}

pub fn build_unified_market_state(
    symbol: String,
    candles: Vec<CandleData>,
    liquidity_walls: LiquidityWalls,
    tf_biases: Option<TfBiases>,
    oi_is_real: bool,
    funding_rate: Option<f64>,
    cache: Option<&mut SignalCache>,
    cvd_seeded: bool,
    liquidations: LiquidationState,
) -> UnifiedMarketState {
    let last_price = candles
        .last()
        .map(|candle| candle.close)
        .unwrap_or_default();
    let price_trend_up = infer_price_trend(&candles);
    let cvd_slope = compute_cvd_slope(&candles);
    let oi_change_pct = calculate_open_interest_change_pct(&candles);
    let (atr_14, rsi_divergence) = match cache {
        Some(c) => c.get_or_refresh(&candles),
        None => (calculate_atr_14(&candles), detect_rsi_divergence(&candles)),
    };
    let vwap = calculate_vwap(&candles);
    let funding_hours_to_settlement = hours_to_next_funding_settlement();
    let (volatility_upper_limit, volatility_lower_limit) =
        calculate_volatility_limits(last_price, atr_14);
    let cvd_threshold = compute_cvd_divergence_threshold(&candles);
    // When OI data is a quote_volume proxy, do not let it gate Strong* signals.
    let oi_for_signal = if oi_is_real { oi_change_pct } else { 0.0 };
    let primary_bias = calculate_long_short_indicator(
        price_trend_up,
        cvd_slope,
        oi_for_signal,
        &rsi_divergence,
        cvd_threshold,
    );

    let biases = tf_biases.unwrap_or_else(|| TfBiases {
        tf_5m: primary_bias.clone(),
        tf_15m: primary_bias.clone(),
        tf_1h: primary_bias.clone(),
        tf_4h: primary_bias.clone(),
    });

    let confluence = ConfluenceMatrix {
        tf_5m: biases.tf_5m,
        tf_15m: biases.tf_15m,
        tf_1h: biases.tf_1h,
        tf_4h: biases.tf_4h,
        aggregate_bias: primary_bias.clone(),
        cvd_slope,
        oi_change_pct,
        rsi_divergence,
    };

    UnifiedMarketState {
        symbol,
        last_price,
        candles,
        confluence,
        liquidity_walls,
        atr_14,
        volatility_upper_limit,
        volatility_lower_limit,
        long_short_indicator: primary_bias,
        oi_is_real,
        funding_rate,
        funding_hours_to_settlement,
        vwap,
        cvd_seeded,
        liquidations,
    }
}
