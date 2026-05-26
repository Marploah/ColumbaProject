use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

pub fn calculate_long_short_indicator(
    price_trend_up: bool,
    cvd_slope: f64,
    oi_change_pct: f64,
    rsi_div: &str,
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
    if price_trend_up && cvd_slope < -5.0 && !rsi_div.contains("bullish") {
        return "WeakShort".to_string();
    }
    // Price falling but buyers stepping in hard = accumulation = bullish
    if !price_trend_up && cvd_slope > 5.0 && !rsi_div.contains("bearish") {
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
    let oi_change_pct = calculate_open_interest_change_pct(candles);
    calculate_long_short_indicator(price_trend_up, cvd_slope, oi_change_pct, &rsi_div)
}

pub fn build_unified_market_state(
    symbol: String,
    candles: Vec<CandleData>,
    liquidity_walls: LiquidityWalls,
    tf_biases: Option<TfBiases>,
) -> UnifiedMarketState {
    let last_price = candles
        .last()
        .map(|candle| candle.close)
        .unwrap_or_default();
    let price_trend_up = infer_price_trend(&candles);
    let cvd_slope = compute_cvd_slope(&candles);
    let oi_change_pct = calculate_open_interest_change_pct(&candles);
    let atr_14 = calculate_atr_14(&candles);
    let (volatility_upper_limit, volatility_lower_limit) =
        calculate_volatility_limits(last_price, atr_14);
    let rsi_divergence = detect_rsi_divergence(&candles);
    let primary_bias =
        calculate_long_short_indicator(price_trend_up, cvd_slope, oi_change_pct, &rsi_divergence);

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
    }
}
