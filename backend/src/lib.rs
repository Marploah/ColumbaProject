mod ai;
mod hardware;
pub mod hardware_profiles;
mod liquidations;
pub mod model_manager;
pub mod model_selector;
mod quant;
pub mod state;
mod trade_log;

use ai::{AiBroker, ChatMessage, TradePlan};
use anyhow::{Context, Result};
use liquidations::{parse_force_order, LiquidationAggregator};
use state::derivatives::{ExchangeOiEntry, GlobalOIState, LiquidationState};
use state::liquidity::WallTracker;
use state::sentiment::FearGreedState;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path as AxumPath, State,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, patch, post},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use hardware::{determine_execution_target, AgentTarget};
use trade_log::TradeLog;
use quant::{
    apply_cvd_trade_delta, build_unified_market_state, compute_tf_bias, decimal_from_trade_qty,
    CandleData, SignalCache, TfBiases, UnifiedMarketState,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::{
    env,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use axum::http::{header, HeaderValue, Method};
use tokio::sync::{broadcast, mpsc, watch, Mutex, RwLock, Semaphore};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        handshake::client::generate_key,
        http::Request as WsRequest,
        Message as WsMessage,
    },
};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::{error, info, warn};

#[derive(Clone)]
struct AppState {
    market: Arc<RwLock<UnifiedMarketState>>,
    ai: AiBroker,
    symbol_tx: Arc<watch::Sender<String>>,
    trade_tx: mpsc::Sender<TradeMessage>,
    current_interval: Arc<Mutex<String>>,
    tf_biases: Arc<Mutex<TfBiases>>,
    binance_oi_tracker: Arc<Mutex<OiTracker>>,
    bybit_oi_tracker: Arc<Mutex<OiTracker>>,
    okx_oi_tracker: Arc<Mutex<OiTracker>>,
    snapshot_tx: Arc<broadcast::Sender<String>>,
    price_tx: Arc<broadcast::Sender<f64>>,
    oi_is_real: Arc<Mutex<bool>>,
    wall_tracker: Arc<Mutex<WallTracker>>,
    current_funding_rate: Arc<Mutex<Option<f64>>>,
    liquidation_aggregator: Arc<Mutex<LiquidationAggregator>>,
    spot_price: Arc<Mutex<Option<f64>>>,
    fear_greed: Arc<Mutex<FearGreedState>>,
    trade_log: TradeLog,
    monitor_semaphore: Arc<Semaphore>,
}

#[derive(Debug, Deserialize)]
struct AnalyzeRequest {
    messages: Vec<ChatMessage>,
    llama_server_url: Option<String>,
    position_size_pct: Option<f64>,
    leverage: Option<f64>,
}

#[derive(Debug, Serialize)]
struct AnalyzeResponse {
    target: TradePlan,
    trade_log_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct SetOutcomeRequest {
    outcome: String,
}

#[derive(Debug, Deserialize)]
struct SetSymbolRequest {
    symbol: String,
}

#[derive(Debug, Deserialize)]
struct SetIntervalRequest {
    interval: String,
}

/// Per-exchange OI tracking: previous + current sample for change_pct computation.
/// Not serialized — internal only.
#[derive(Default)]
struct OiTracker {
    current: Option<f64>,
    previous: Option<f64>,
    consecutive_failures: u32,
}

impl OiTracker {
    fn update(&mut self, value: f64) {
        self.previous = self.current;
        self.current = Some(value);
        self.consecutive_failures = 0;
    }

    fn fail(&mut self) {
        self.consecutive_failures += 1;
    }

    fn reset(&mut self) {
        self.current = None;
        self.previous = None;
        self.consecutive_failures = 0;
    }

    fn to_entry(&self) -> ExchangeOiEntry {
        let change_pct = match (self.current, self.previous) {
            (Some(c), Some(p)) if p != 0.0 => Some((c - p) / p * 100.0),
            _ => None,
        };
        ExchangeOiEntry {
            oi: self.current,
            change_pct,
            healthy: self.consecutive_failures < 3,
        }
    }
}

/// Converts a Binance symbol to OKX perpetual swap instId.
/// `btcusdt` / `BTCUSDT` → `BTC-USDT-SWAP`
fn to_okx_symbol(binance_symbol: &str) -> Option<String> {
    let upper = binance_symbol.to_ascii_uppercase();
    if let Some(base) = upper.strip_suffix("USDT") {
        Some(format!("{base}-USDT-SWAP"))
    } else if let Some(base) = upper.strip_suffix("BUSD") {
        Some(format!("{base}-BUSD-SWAP"))
    } else {
        None
    }
}

/// Aggregates three exchange OI trackers into a divergence-annotated `GlobalOIState`.
fn compute_global_oi_state(
    binance: &OiTracker,
    bybit: &OiTracker,
    okx: &OiTracker,
) -> GlobalOIState {
    let b_entry = binance.to_entry();
    let by_entry = bybit.to_entry();
    let o_entry = okx.to_entry();

    let changes: Vec<f64> = [b_entry.change_pct, by_entry.change_pct, o_entry.change_pct]
        .into_iter()
        .flatten()
        .collect();

    let (divergence_score, divergence_label) = if changes.len() < 2 {
        (0.0, "insufficient cross-exchange data".to_string())
    } else {
        let mean = changes.iter().sum::<f64>() / changes.len() as f64;
        let variance =
            changes.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / changes.len() as f64;
        let std_dev = variance.sqrt();
        let score = (std_dev / 0.5_f64).min(1.0);

        let label = compute_oi_divergence_label(
            b_entry.change_pct,
            by_entry.change_pct,
            o_entry.change_pct,
        );
        (score, label)
    };

    GlobalOIState {
        binance: b_entry,
        bybit: by_entry,
        okx: o_entry,
        divergence_score,
        divergence_label,
    }
}

fn compute_oi_divergence_label(
    binance: Option<f64>,
    bybit: Option<f64>,
    okx: Option<f64>,
) -> String {
    let threshold = 0.1_f64;
    let named: Vec<(&str, f64)> = [("Binance", binance), ("Bybit", bybit), ("OKX", okx)]
        .into_iter()
        .filter_map(|(name, v)| v.map(|v| (name, v)))
        .collect();

    if named.len() < 2 {
        return "single-exchange data".to_string();
    }

    let rising: Vec<&str> = named
        .iter()
        .filter(|(_, v)| *v > threshold)
        .map(|(n, _)| *n)
        .collect();
    let falling: Vec<&str> = named
        .iter()
        .filter(|(_, v)| *v < -threshold)
        .map(|(n, _)| *n)
        .collect();

    if rising.len() == named.len() {
        return "broad leverage expansion".to_string();
    }
    if falling.len() == named.len() {
        return "broad deleveraging".to_string();
    }
    if rising.len() == 1 && falling.is_empty() {
        return format!("{} OI rising alone", rising[0]);
    }
    if falling.len() == 1 && rising.is_empty() {
        return format!("{} OI falling alone", falling[0]);
    }
    "exchange disagreement".to_string()
}

fn bucket_ms_from_interval(s: &str) -> Option<i64> {
    match s {
        "1m"  => Some(60_000),
        "3m"  => Some(180_000),
        "5m"  => Some(300_000),
        "15m" => Some(900_000),
        "1h"  => Some(3_600_000),
        "4h"  => Some(14_400_000),
        "1d"  => Some(86_400_000),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct BinanceAggTrade {
    #[serde(rename = "a")]
    agg_id: i64,
    #[serde(rename = "p")]
    price: String,
    #[serde(rename = "q")]
    quantity: String,
    #[serde(rename = "m")]
    buyer_is_maker: bool,
    #[serde(rename = "T")]
    trade_time: i64,
}

#[derive(Debug, Deserialize)]
struct OiHistEntry {
    #[serde(rename = "sumOpenInterest")]
    sum_open_interest: String,
    timestamp: i64,
}

#[derive(Debug, Deserialize)]
struct OiCurrentResponse {
    #[serde(rename = "openInterest")]
    open_interest: String,
}

#[derive(Debug, Deserialize)]
struct PremiumIndexResponse {
    #[serde(rename = "lastFundingRate")]
    last_funding_rate: String,
}

#[derive(Debug, Deserialize)]
struct DepthResponse {
    bids: Vec<[String; 2]>,
    asks: Vec<[String; 2]>,
}

/// Exponential backoff duration for consecutive Binance 429 responses.
/// 5s → 10s → 20s → 40s → 80s → 160s → 300s (cap)
fn rate_limit_backoff(consecutive_429s: u32) -> Duration {
    Duration::from_secs((5_u64 << consecutive_429s.min(6)).min(300))
}

async fn fetch_depth(symbol: &str) -> reqwest::Result<DepthResponse> {
    let url = format!(
        "https://fapi.binance.com/fapi/v1/depth?symbol={}&limit=500",
        symbol.to_ascii_uppercase()
    );
    reqwest::get(&url).await?.error_for_status()?.json().await
}

async fn poll_tf_biases(
    mut symbol_rx: watch::Receiver<String>,
    tf_biases: Arc<Mutex<TfBiases>>,
) {
    let mut symbol = symbol_rx.borrow().clone();
    // First tick after 5 minutes — startup already seeded the biases.
    let mut interval = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(300),
        Duration::from_secs(300),
    );

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let fresh = fetch_tf_biases(&symbol).await;
                *tf_biases.lock().await = fresh;
            }
            Ok(()) = symbol_rx.changed() => {
                symbol = symbol_rx.borrow_and_update().clone();
                // set_symbol already reseeds tf_biases; just track the new symbol.
            }
        }
    }
}

async fn poll_funding_rate(
    mut symbol_rx: watch::Receiver<String>,
    current_funding_rate: Arc<Mutex<Option<f64>>>,
) {
    let mut symbol = symbol_rx.borrow().clone();
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    let mut consecutive_429: u32 = 0;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let url = format!(
                    "https://fapi.binance.com/fapi/v1/premiumIndex?symbol={}",
                    symbol.to_ascii_uppercase()
                );
                match reqwest::get(&url).await {
                    Ok(resp) if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS => {
                        consecutive_429 += 1;
                        let delay = rate_limit_backoff(consecutive_429);
                        warn!("Binance rate limit on funding rate poll for {symbol}; backing off {}s", delay.as_secs());
                        tokio::time::sleep(delay).await;
                    }
                    Ok(resp) => {
                        consecutive_429 = 0;
                        match resp.json::<PremiumIndexResponse>().await {
                            Ok(data) => {
                                if let Ok(rate) = data.last_funding_rate.parse::<f64>() {
                                    *current_funding_rate.lock().await = Some(rate);
                                }
                            }
                            Err(e) => warn!("failed to parse funding rate for {symbol}: {e}"),
                        }
                    }
                    Err(e) => warn!("funding rate request failed for {symbol}: {e}"),
                }
            }
            Ok(()) = symbol_rx.changed() => {
                symbol = symbol_rx.borrow_and_update().clone();
                consecutive_429 = 0;
                *current_funding_rate.lock().await = None;
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct FngDataPoint {
    value: String,
    value_classification: String,
    timestamp: String,
}

#[derive(Debug, Deserialize)]
struct FngResponse {
    data: Vec<FngDataPoint>,
}

/// Polls the alternative.me Fear & Greed Index every 5 minutes.
/// No symbol dependency — this is a global crypto sentiment signal.
/// On failure marks feed_healthy=false without blocking any other pipeline.
async fn poll_fear_greed(fear_greed: Arc<Mutex<FearGreedState>>) {
    let mut interval = tokio::time::interval(Duration::from_secs(300));
    loop {
        interval.tick().await;
        match reqwest::get("https://api.alternative.me/fng/?limit=1&format=json").await {
            Ok(resp) => match resp.json::<FngResponse>().await {
                Ok(fng) if !fng.data.is_empty() => {
                    let pt = &fng.data[0];
                    let value = pt.value.parse::<u8>().unwrap_or(50);
                    let timestamp = pt.timestamp.parse::<i64>().unwrap_or(0);
                    *fear_greed.lock().await = FearGreedState {
                        value,
                        classification: pt.value_classification.clone(),
                        timestamp,
                        feed_healthy: true,
                    };
                }
                Ok(_) => {
                    fear_greed.lock().await.feed_healthy = false;
                    warn!("fear/greed API returned empty data");
                }
                Err(e) => {
                    fear_greed.lock().await.feed_healthy = false;
                    warn!("failed to parse fear/greed response: {e}");
                }
            },
            Err(e) => {
                fear_greed.lock().await.feed_healthy = false;
                warn!("fear/greed fetch failed: {e}");
            }
        }
    }
}

async fn poll_liquidity_walls(
    mut symbol_rx: watch::Receiver<String>,
    wall_tracker: Arc<Mutex<WallTracker>>,
) {
    let mut symbol = symbol_rx.borrow().clone();
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    let mut consecutive_429: u32 = 0;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                match fetch_depth(&symbol).await {
                    Ok(depth) => {
                        consecutive_429 = 0;
                        wall_tracker.lock().await.update(&depth.bids, &depth.asks, Instant::now());
                    }
                    Err(e) if e.status() == Some(reqwest::StatusCode::TOO_MANY_REQUESTS) => {
                        consecutive_429 += 1;
                        let delay = rate_limit_backoff(consecutive_429);
                        warn!("Binance rate limit on liquidity walls poll for {symbol}; backing off {}s", delay.as_secs());
                        tokio::time::sleep(delay).await;
                    }
                    Err(e) => {
                        warn!("order book fetch failed for {symbol}: {e}");
                        wall_tracker.lock().await.mark_unhealthy();
                    }
                }
            }
            Ok(()) = symbol_rx.changed() => {
                symbol = symbol_rx.borrow_and_update().clone();
                consecutive_429 = 0;
                wall_tracker.lock().await.reset();
            }
        }
    }
}

enum TradeMessage {
    Trade(BinanceAggTrade),
    ResetSymbol { candles: Vec<CandleData> },
    ResetInterval { bucket_ms: i64, candles: Vec<CandleData> },
}

async fn fetch_open_interest_history(symbol: &str, period: &str, limit: u32) -> Result<Vec<(i64, f64)>> {
    let url = format!(
        "https://fapi.binance.com/futures/data/openInterestHist?symbol={}&period={}&limit={}",
        symbol.to_ascii_uppercase(),
        period,
        limit
    );
    let entries: Vec<OiHistEntry> = reqwest::get(&url)
        .await
        .context("failed to request OI history")?
        .error_for_status()
        .context("OI history returned non-success status")?
        .json()
        .await
        .context("failed to parse OI history JSON")?;

    Ok(entries
        .into_iter()
        .filter_map(|e| e.sum_open_interest.parse::<f64>().ok().map(|v| (e.timestamp, v)))
        .collect())
}

/// Returns true if real OI data was merged; false if candles retain quote_volume proxy.
async fn merge_open_interest(candles: &mut Vec<CandleData>, symbol: &str, interval: &str) -> bool {
    let oi_period = match interval {
        "1m" | "3m" | "5m" => "5m",
        "15m" => "15m",
        "1h" => "1h",
        "4h" | "6h" | "12h" => "4h",
        "1d" => "1d",
        _ => "5m",
    };

    let oi_data = match fetch_open_interest_history(symbol, oi_period, 500).await {
        Ok(data) => data,
        Err(e) => {
            warn!("failed to fetch OI history for {symbol}: {e:?}; open_interest will use quote_volume proxy");
            return false;
        }
    };

    if oi_data.is_empty() {
        return false;
    }

    for candle in candles.iter_mut() {
        if let Some((_, oi)) = oi_data
            .iter()
            .min_by_key(|(ts, _)| (ts - candle.timestamp).unsigned_abs())
        {
            candle.open_interest = Some(*oi);
        }
    }

    true
}

async fn fetch_klines_with_oi(symbol: &str, interval: &str) -> Result<(Vec<CandleData>, bool)> {
    let mut candles = fetch_recent_klines(symbol, interval).await?;
    let oi_is_real = merge_open_interest(&mut candles, symbol, interval).await;
    Ok((candles, oi_is_real))
}

async fn fetch_tf_biases(symbol: &str) -> TfBiases {
    let (r5m, r15m, r1h, r4h) = tokio::join!(
        fetch_tf_bias_for(symbol, "5m"),
        fetch_tf_bias_for(symbol, "15m"),
        fetch_tf_bias_for(symbol, "1h"),
        fetch_tf_bias_for(symbol, "4h"),
    );
    TfBiases { tf_5m: r5m, tf_15m: r15m, tf_1h: r1h, tf_4h: r4h }
}

async fn fetch_tf_bias_for(symbol: &str, interval: &str) -> String {
    match fetch_recent_klines(symbol, interval).await {
        Ok(candles) => compute_tf_bias(&candles),
        Err(e) => {
            warn!("failed to fetch {interval} klines for TF bias ({symbol}): {e:?}");
            "Neutral".to_string()
        }
    }
}

async fn poll_open_interest(
    mut symbol_rx: watch::Receiver<String>,
    tracker: Arc<Mutex<OiTracker>>,
) {
    let mut symbol = symbol_rx.borrow().clone();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
    let mut consecutive_429: u32 = 0;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let url = format!(
                    "https://fapi.binance.com/fapi/v1/openInterest?symbol={}",
                    symbol.to_ascii_uppercase()
                );
                match reqwest::get(&url).await {
                    Ok(resp) if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS => {
                        consecutive_429 += 1;
                        let delay = rate_limit_backoff(consecutive_429);
                        warn!("Binance rate limit on OI poll for {symbol}; backing off {}s", delay.as_secs());
                        tokio::time::sleep(delay).await;
                    }
                    Ok(resp) => {
                        consecutive_429 = 0;
                        match resp.json::<OiCurrentResponse>().await {
                            Ok(data) => {
                                if let Ok(oi) = data.open_interest.parse::<f64>() {
                                    tracker.lock().await.update(oi);
                                }
                            }
                            Err(e) => {
                                tracker.lock().await.fail();
                                warn!("failed to parse OI response: {e:?}");
                            }
                        }
                    }
                    Err(e) => {
                        tracker.lock().await.fail();
                        warn!("OI poll request failed: {e:?}");
                    }
                }
            }
            Ok(()) = symbol_rx.changed() => {
                symbol = symbol_rx.borrow_and_update().clone();
                consecutive_429 = 0;
                tracker.lock().await.reset();
            }
        }
    }
}

#[derive(Deserialize)]
struct BybitOiItem {
    #[serde(rename = "openInterest")]
    open_interest: String,
}

#[derive(Deserialize)]
struct BybitOiResult {
    list: Vec<BybitOiItem>,
}

#[derive(Deserialize)]
struct BybitOiResponse {
    #[serde(rename = "retCode")]
    ret_code: i32,
    result: BybitOiResult,
}

#[derive(Deserialize)]
struct OkxOiItem {
    oi: String,
}

#[derive(Deserialize)]
struct OkxOiResponse {
    code: String,
    data: Vec<OkxOiItem>,
}

#[derive(Deserialize)]
struct SpotPriceResponse {
    price: String,
}

async fn poll_bybit_oi(
    mut symbol_rx: watch::Receiver<String>,
    tracker: Arc<Mutex<OiTracker>>,
) {
    let mut symbol = symbol_rx.borrow().clone();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
    let mut consecutive_errors: u32 = 0;
    let mut circuit_open_until: Option<Instant> = None;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if circuit_open_until.map(|u| Instant::now() < u).unwrap_or(false) {
                    continue;
                }
                circuit_open_until = None;
                let sym = symbol.to_ascii_uppercase();
                let url = format!(
                    "https://api.bybit.com/v5/market/open-interest?category=linear&symbol={sym}&intervalTime=5min&limit=1"
                );
                let mut failed = false;
                match reqwest::get(&url).await {
                    Ok(resp) => {
                        match resp.json::<BybitOiResponse>().await {
                            Ok(data) if data.ret_code == 0 => {
                                consecutive_errors = 0;
                                if let Some(item) = data.result.list.first() {
                                    if let Ok(oi) = item.open_interest.parse::<f64>() {
                                        tracker.lock().await.update(oi);
                                    }
                                }
                            }
                            Ok(data) => {
                                tracker.lock().await.fail();
                                warn!("Bybit OI returned retCode={}", data.ret_code);
                                failed = true;
                            }
                            Err(e) => {
                                tracker.lock().await.fail();
                                warn!("failed to parse Bybit OI response for {sym}: {e:?}");
                                failed = true;
                            }
                        }
                    }
                    Err(e) => {
                        tracker.lock().await.fail();
                        warn!("Bybit OI poll failed for {sym}: {e:?}");
                        failed = true;
                    }
                }
                if failed {
                    consecutive_errors += 1;
                    if consecutive_errors >= 10 {
                        warn!("Bybit OI: circuit open after {consecutive_errors} consecutive failures; suppressing polls for 300s");
                        circuit_open_until = Some(Instant::now() + Duration::from_secs(300));
                        consecutive_errors = 0;
                    }
                }
            }
            Ok(()) = symbol_rx.changed() => {
                symbol = symbol_rx.borrow_and_update().clone();
                consecutive_errors = 0;
                circuit_open_until = None;
                tracker.lock().await.reset();
            }
        }
    }
}

async fn poll_okx_oi(
    mut symbol_rx: watch::Receiver<String>,
    tracker: Arc<Mutex<OiTracker>>,
) {
    let mut symbol = symbol_rx.borrow().clone();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
    let mut consecutive_errors: u32 = 0;
    let mut circuit_open_until: Option<Instant> = None;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if circuit_open_until.map(|u| Instant::now() < u).unwrap_or(false) {
                    continue;
                }
                circuit_open_until = None;
                let Some(inst_id) = to_okx_symbol(&symbol) else {
                    warn!("cannot map symbol {} to OKX instId; skipping OI poll", symbol);
                    continue;
                };
                let url = format!(
                    "https://www.okx.com/api/v5/public/open-interest?instType=SWAP&instId={inst_id}"
                );
                let mut failed = false;
                match reqwest::get(&url).await {
                    Ok(resp) => {
                        match resp.json::<OkxOiResponse>().await {
                            Ok(data) if data.code == "0" => {
                                consecutive_errors = 0;
                                if let Some(item) = data.data.first() {
                                    if let Ok(oi) = item.oi.parse::<f64>() {
                                        tracker.lock().await.update(oi);
                                    }
                                }
                            }
                            Ok(data) => {
                                tracker.lock().await.fail();
                                warn!("OKX OI returned code={}", data.code);
                                failed = true;
                            }
                            Err(e) => {
                                tracker.lock().await.fail();
                                warn!("failed to parse OKX OI response for {inst_id}: {e:?}");
                                failed = true;
                            }
                        }
                    }
                    Err(e) => {
                        tracker.lock().await.fail();
                        warn!("OKX OI poll failed for {inst_id}: {e:?}");
                        failed = true;
                    }
                }
                if failed {
                    consecutive_errors += 1;
                    if consecutive_errors >= 10 {
                        warn!("OKX OI: circuit open after {consecutive_errors} consecutive failures; suppressing polls for 300s");
                        circuit_open_until = Some(Instant::now() + Duration::from_secs(300));
                        consecutive_errors = 0;
                    }
                }
            }
            Ok(()) = symbol_rx.changed() => {
                symbol = symbol_rx.borrow_and_update().clone();
                consecutive_errors = 0;
                circuit_open_until = None;
                tracker.lock().await.reset();
            }
        }
    }
}

async fn poll_spot_price(
    mut symbol_rx: watch::Receiver<String>,
    spot_price_arc: Arc<Mutex<Option<f64>>>,
) {
    let mut symbol = symbol_rx.borrow().clone();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
    let mut consecutive_errors: u32 = 0;
    let mut circuit_open_until: Option<Instant> = None;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if circuit_open_until.map(|u| Instant::now() < u).unwrap_or(false) {
                    continue;
                }
                circuit_open_until = None;
                let sym = symbol.to_ascii_uppercase();
                let url = format!("https://api.binance.com/api/v3/ticker/price?symbol={sym}");
                let mut failed = false;
                match reqwest::get(&url).await {
                    Ok(resp) => match resp.json::<SpotPriceResponse>().await {
                        Ok(data) => {
                            consecutive_errors = 0;
                            if let Ok(price) = data.price.parse::<f64>() {
                                *spot_price_arc.lock().await = Some(price);
                            }
                        }
                        Err(e) => {
                            warn!("spot price parse error for {sym}: {e:?}");
                            failed = true;
                        }
                    },
                    Err(e) => {
                        warn!("spot price poll failed for {sym}: {e:?}");
                        failed = true;
                    }
                }
                if failed {
                    consecutive_errors += 1;
                    if consecutive_errors >= 10 {
                        warn!("spot price: circuit open after {consecutive_errors} consecutive failures; suppressing polls for 300s");
                        circuit_open_until = Some(Instant::now() + Duration::from_secs(300));
                        consecutive_errors = 0;
                    }
                }
            }
            Ok(()) = symbol_rx.changed() => {
                symbol = symbol_rx.borrow_and_update().clone();
                consecutive_errors = 0;
                circuit_open_until = None;
                *spot_price_arc.lock().await = None;
            }
        }
    }
}

pub async fn run() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "columba_backend=info,tower_http=info".into()),
        )
        .try_init();

    let symbol = env::var("COLUMBA_SYMBOL").unwrap_or_else(|_| "btcusdt".to_string());
    let preferred_model = env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
    let execution_mode = env::var("COLUMBA_EXECUTION_MODE").unwrap_or_else(|_| "Auto".to_string());
    let api_key = env::var("OPENAI_API_KEY").unwrap_or_default();
    let ai = match determine_execution_target(&execution_mode, preferred_model) {
        AgentTarget::Local(model) => {
            let url = env::var("COLUMBA_LLAMA_SERVER_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8081/v1".to_string());
            AiBroker::llama_cpp_at(model, url)
        }
        AgentTarget::Cloud(_model) if api_key.is_empty() => {
            warn!("OPENAI_API_KEY is unset; falling back to local llama-server");
            let model = env::var("OPENAI_MODEL").unwrap_or_else(|_| "qwen3-4b".to_string());
            let url = env::var("COLUMBA_LLAMA_SERVER_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8081/v1".to_string());
            AiBroker::llama_cpp_at(model, url)
        }
        AgentTarget::Cloud(model) if api_key.starts_with("sk-ant-") => {
            let anthropic_model =
                env::var("ANTHROPIC_MODEL").unwrap_or(model);
            info!("Anthropic key detected; using model {anthropic_model}");
            AiBroker::anthropic(api_key, anthropic_model)
        }
        AgentTarget::Cloud(model) => AiBroker::openai(api_key, model),
    };

    // Start with empty defaults — all three network fetches run concurrently in a background
    // task so Axum starts immediately instead of waiting 5-7 s for Binance REST responses.
    let tf_biases = Arc::new(Mutex::new(TfBiases::default()));
    let binance_oi_tracker: Arc<Mutex<OiTracker>> = Arc::new(Mutex::new(OiTracker::default()));
    let bybit_oi_tracker: Arc<Mutex<OiTracker>> = Arc::new(Mutex::new(OiTracker::default()));
    let okx_oi_tracker: Arc<Mutex<OiTracker>> = Arc::new(Mutex::new(OiTracker::default()));
    let oi_is_real_arc: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    let wall_tracker_arc: Arc<Mutex<WallTracker>> = Arc::new(Mutex::new(WallTracker::default()));
    let (initial_walls, initial_liquidity_analytics) = {
        let t = wall_tracker_arc.lock().await;
        (t.snapshot_walls(), t.snapshot_analytics())
    };

    let funding_rate_arc: Arc<Mutex<Option<f64>>> = Arc::new(Mutex::new(None));

    let liquidation_aggregator_arc: Arc<Mutex<LiquidationAggregator>> =
        Arc::new(Mutex::new(LiquidationAggregator::default()));
    let spot_price_arc: Arc<Mutex<Option<f64>>> = Arc::new(Mutex::new(None));
    let fear_greed_arc: Arc<Mutex<FearGreedState>> =
        Arc::new(Mutex::new(FearGreedState::default()));

    let initial_market = build_unified_market_state(
        symbol.to_uppercase(),
        vec![],
        initial_walls,
        initial_liquidity_analytics,
        None,
        false,
        None,
        None,
        false,
        LiquidationState::default(),
        GlobalOIState::default(),
        None,
        FearGreedState::default(),
    );

    let (symbol_tx, symbol_rx) = watch::channel(symbol.clone());
    let (trade_tx, trade_rx) = mpsc::channel::<TradeMessage>(4096);
    let (snapshot_tx, _) = broadcast::channel::<String>(64);
    let snapshot_tx = Arc::new(snapshot_tx);
    let (price_tx, _) = broadcast::channel::<f64>(512);
    let price_tx = Arc::new(price_tx);

    let db_path = env::var("COLUMBA_TRADE_LOG").unwrap_or_else(|_| {
        env::var("COLUMBA_STORAGE_DIR")
            .map(|dir| std::path::Path::new(&dir).join("columba_trades.db").display().to_string())
            .unwrap_or_else(|_| "columba_trades.db".to_string())
    });
    let trade_log = match TradeLog::open(std::path::Path::new(&db_path)) {
        Ok(log) => log,
        Err(e) => {
            warn!("failed to open trade log at {db_path}: {e:?}; plans will not be persisted");
            TradeLog::open(std::path::Path::new(":memory:")).expect("in-memory db always works")
        }
    };

    let state = AppState {
        market: Arc::new(RwLock::new(initial_market)),
        ai,
        symbol_tx: Arc::new(symbol_tx),
        trade_tx: trade_tx.clone(),
        current_interval: Arc::new(Mutex::new("1m".to_string())),
        tf_biases,
        binance_oi_tracker: Arc::clone(&binance_oi_tracker),
        bybit_oi_tracker: Arc::clone(&bybit_oi_tracker),
        okx_oi_tracker: Arc::clone(&okx_oi_tracker),
        snapshot_tx: Arc::clone(&snapshot_tx),
        price_tx: Arc::clone(&price_tx),
        oi_is_real: Arc::clone(&oi_is_real_arc),
        wall_tracker: Arc::clone(&wall_tracker_arc),
        current_funding_rate: Arc::clone(&funding_rate_arc),
        liquidation_aggregator: Arc::clone(&liquidation_aggregator_arc),
        spot_price: Arc::clone(&spot_price_arc),
        fear_greed: Arc::clone(&fear_greed_arc),
        trade_log,
        monitor_semaphore: Arc::new(Semaphore::new(5)),
    };

    let oi_symbol_rx = symbol_rx.clone();
    let bybit_oi_rx = symbol_rx.clone();
    let okx_oi_rx = symbol_rx.clone();
    let walls_symbol_rx = symbol_rx.clone();
    let fr_symbol_rx = symbol_rx.clone();
    let tf_symbol_rx = symbol_rx.clone();

    // Background seeding: fetch klines + TF biases + depth concurrently, write to state,
    // then send ResetSymbol so the trade processor seeds its local candle buffer.
    // Axum starts below without waiting for this — eliminates "connection refused" on fast launch.
    {
        let seed_market = Arc::clone(&state.market);
        let seed_tf = Arc::clone(&state.tf_biases);
        let seed_wall = Arc::clone(&wall_tracker_arc);
        let seed_oi = Arc::clone(&oi_is_real_arc);
        let seed_tx = trade_tx.clone();
        let seed_sym = symbol.clone();
        tokio::spawn(async move {
            let (kline_result, fresh_biases, depth_result) = tokio::join!(
                fetch_klines_with_oi(&seed_sym, "1m"),
                fetch_tf_biases(&seed_sym),
                fetch_depth(&seed_sym)
            );
            let (candles, oi_real) = match kline_result {
                Ok(r) => r,
                Err(e) => {
                    warn!("initial kline seed failed: {e:?}");
                    (vec![], false)
                }
            };
            *seed_tf.lock().await = fresh_biases;
            *seed_oi.lock().await = oi_real;
            if let Ok(depth) = depth_result {
                seed_wall.lock().await.update(&depth.bids, &depth.asks, Instant::now());
            }
            let (walls, liquidity_analytics) = {
                let t = seed_wall.lock().await;
                (t.snapshot_walls(), t.snapshot_analytics())
            };
            let biases = seed_tf.lock().await.clone();
            let seeded_market = build_unified_market_state(
                seed_sym.to_uppercase(),
                candles.clone(),
                walls,
                liquidity_analytics,
                Some(biases),
                oi_real,
                None,
                None,
                false,
                LiquidationState::default(),
                GlobalOIState::default(),
                None,
                FearGreedState::default(),
            );
            *seed_market.write().await = seeded_market;
            seed_tx.send(TradeMessage::ResetSymbol { candles }).await.ok();
        });
    }

    let liq_symbol_rx = symbol_rx.clone();
    let spot_symbol_rx = symbol_rx.clone();
    tokio::spawn(stream_binance_agg_trades(symbol.clone(), trade_tx, symbol_rx));
    tokio::spawn(process_trade_deltas(
        trade_rx,
        Arc::clone(&state.market),
        Arc::clone(&state.tf_biases),
        Arc::clone(&binance_oi_tracker),
        Arc::clone(&bybit_oi_tracker),
        Arc::clone(&okx_oi_tracker),
        Arc::clone(&oi_is_real_arc),
        Arc::clone(&wall_tracker_arc),
        Arc::clone(&funding_rate_arc),
        Arc::clone(&liquidation_aggregator_arc),
        Arc::clone(&spot_price_arc),
        Arc::clone(&fear_greed_arc),
        snapshot_tx,
        price_tx,
    ));
    tokio::spawn(poll_open_interest(oi_symbol_rx, Arc::clone(&binance_oi_tracker)));
    tokio::spawn(poll_bybit_oi(bybit_oi_rx, Arc::clone(&bybit_oi_tracker)));
    tokio::spawn(poll_okx_oi(okx_oi_rx, Arc::clone(&okx_oi_tracker)));
    tokio::spawn(poll_spot_price(spot_symbol_rx, Arc::clone(&spot_price_arc)));
    tokio::spawn(poll_liquidity_walls(walls_symbol_rx, Arc::clone(&wall_tracker_arc)));
    tokio::spawn(poll_funding_rate(fr_symbol_rx, Arc::clone(&funding_rate_arc)));
    tokio::spawn(poll_tf_biases(tf_symbol_rx, Arc::clone(&state.tf_biases)));
    tokio::spawn(stream_liquidations(liq_symbol_rx, Arc::clone(&liquidation_aggregator_arc)));
    tokio::spawn(poll_fear_greed(Arc::clone(&fear_greed_arc)));

    let allowed_origins: Vec<HeaderValue> = [
        "http://localhost:5173",
        "http://127.0.0.1:5173",
        "tauri://localhost",
        "https://tauri.localhost",
    ]
    .iter()
    .filter_map(|o| o.parse().ok())
    .collect();

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(allowed_origins))
        .allow_methods([Method::GET, Method::POST, Method::PATCH])
        .allow_headers([header::CONTENT_TYPE]);

    let app = Router::new()
        .route("/api/analyze", post(analyze))
        .route("/api/trades", get(trades))
        .route("/api/trades/summary", get(trades_summary))
        .route("/api/trades/:id/outcome", patch(patch_trade_outcome))
        .route("/api/snapshot", get(snapshot))
        .route("/api/symbol", post(set_symbol))
        .route("/api/interval", post(set_interval))
        .route("/ws", get(ws_handler))
        .with_state(state)
        .layer(cors);

    let addr: SocketAddr = env::var("BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_string())
        .parse()
        .context("invalid BIND_ADDR")?;

    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("backend listening on http://{addr}");
    axum::serve(listener, app).await?;

    Ok(())
}

async fn fetch_recent_klines(symbol: &str, interval: &str) -> Result<Vec<CandleData>> {
    let url = format!(
        "https://fapi.binance.com/fapi/v1/klines?symbol={}&interval={}&limit=1500",
        symbol.to_ascii_uppercase(),
        interval,
    );
    let rows: Vec<Vec<serde_json::Value>> = reqwest::get(url)
        .await
        .context("failed to request Binance kline seed")?
        .error_for_status()
        .context("Binance kline seed returned non-success status")?
        .json()
        .await
        .context("failed to parse Binance kline seed JSON")?;

    let mut cvd = Decimal::ZERO;
    let mut candles = Vec::with_capacity(rows.len());

    for row in rows {
        if row.len() < 10 {
            continue;
        }

        let timestamp = row[0].as_i64().unwrap_or_default();
        let open = value_as_f64(&row[1]);
        let high = value_as_f64(&row[2]);
        let low = value_as_f64(&row[3]);
        let close = value_as_f64(&row[4]);
        let volume = value_as_f64(&row[5]);
        let quote_volume = value_as_f64(&row[7]);
        let taker_buy_base = value_as_decimal(&row[9]);
        let total_base = value_as_decimal(&row[5]);
        let taker_sell_base = (total_base - taker_buy_base).max(Decimal::ZERO);
        cvd += taker_buy_base - taker_sell_base;

        candles.push(CandleData {
            timestamp,
            open,
            high,
            low,
            close,
            volume,
            buy_volume: taker_buy_base,
            sell_volume: taker_sell_base,
            cvd,
            // quote_volume placeholder; overwritten by merge_open_interest when called via fetch_klines_with_oi
            open_interest: if quote_volume.is_finite() && quote_volume > 0.0 {
                Some(quote_volume)
            } else {
                None
            },
        });
    }

    Ok(candles)
}

fn value_as_f64(value: &serde_json::Value) -> f64 {
    value
        .as_str()
        .and_then(|raw| raw.parse::<f64>().ok())
        .or_else(|| value.as_f64())
        .unwrap_or_default()
}

fn value_as_decimal(value: &serde_json::Value) -> Decimal {
    value
        .as_str()
        .and_then(|raw| Decimal::from_str(raw).ok())
        .or_else(|| value.as_f64().and_then(decimal_from_trade_qty_checked))
        .unwrap_or(Decimal::ZERO)
}

fn decimal_from_trade_qty_checked(value: f64) -> Option<Decimal> {
    if value.is_finite() {
        Some(decimal_from_trade_qty(value))
    } else {
        None
    }
}

async fn snapshot(State(state): State<AppState>) -> Json<UnifiedMarketState> {
    Json(state.market.read().await.clone())
}

async fn set_symbol(
    State(state): State<AppState>,
    Json(req): Json<SetSymbolRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let sym = req.symbol.to_ascii_lowercase();
    let interval = state.current_interval.lock().await.clone();

    // Idempotent: skip full reset if symbol didn't change (prevents double-fire on page load).
    if state.market.read().await.symbol.to_ascii_lowercase() == sym {
        return Ok(StatusCode::OK);
    }
    info!("switching active symbol to {} (interval={})", sym.to_ascii_uppercase(), interval);

    // Notify polling tasks immediately so they stop writing stale-symbol data
    // before we fetch and assemble the new state. Each task clears its value
    // upon receiving the watch change.
    state.symbol_tx.send(sym.clone()).ok();
    state.binance_oi_tracker.lock().await.reset();
    state.bybit_oi_tracker.lock().await.reset();
    state.okx_oi_tracker.lock().await.reset();
    *state.current_funding_rate.lock().await = None;
    state.wall_tracker.lock().await.reset();

    let (candles, oi_is_real) = fetch_klines_with_oi(&sym, &interval).await.map_err(internal_error)?;
    let tf_biases_val = fetch_tf_biases(&sym).await;
    *state.tf_biases.lock().await = tf_biases_val.clone();
    *state.oi_is_real.lock().await = oi_is_real;

    *state.spot_price.lock().await = None;
    let (walls, liquidity_analytics) = {
        let t = state.wall_tracker.lock().await;
        (t.snapshot_walls(), t.snapshot_analytics())
    };
    let fr = *state.current_funding_rate.lock().await;
    let liq_snap = state.liquidation_aggregator.lock().await.snapshot();
    let fg = state.fear_greed.lock().await.clone();
    let new_market = build_unified_market_state(
        sym.to_ascii_uppercase(),
        candles.clone(),
        walls,
        liquidity_analytics,
        Some(tf_biases_val),
        oi_is_real,
        fr,
        None,
        false,
        liq_snap,
        GlobalOIState::default(),
        None, // spot resets on symbol change; poll_spot_price will refresh
        fg,
    );
    *state.market.write().await = new_market;

    // Seed the trade processor with the fetched candles so it broadcasts a full
    // snapshot immediately instead of starting from scratch on the first live trade.
    state
        .trade_tx
        .send(TradeMessage::ResetSymbol { candles })
        .await
        .map_err(|e| internal_error(anyhow::anyhow!("trade channel closed: {e}")))?;

    Ok(StatusCode::OK)
}

async fn set_interval(
    State(state): State<AppState>,
    Json(req): Json<SetIntervalRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let bucket_ms = bucket_ms_from_interval(&req.interval).ok_or_else(|| {
        (StatusCode::BAD_REQUEST, format!("unknown interval: {}", req.interval))
    })?;

    let sym = state.market.read().await.symbol.to_ascii_lowercase();

    // Idempotent: skip reset if interval didn't change.
    if *state.current_interval.lock().await == req.interval {
        return Ok(StatusCode::OK);
    }
    info!("switching interval to {} for {}", req.interval, sym.to_ascii_uppercase());

    let (candles, oi_is_real) = fetch_klines_with_oi(&sym, &req.interval).await.map_err(internal_error)?;
    let tf_biases_val = fetch_tf_biases(&sym).await;
    *state.tf_biases.lock().await = tf_biases_val.clone();
    *state.oi_is_real.lock().await = oi_is_real;

    let (walls, liquidity_analytics) = {
        let t = state.wall_tracker.lock().await;
        (t.snapshot_walls(), t.snapshot_analytics())
    };
    let fr = *state.current_funding_rate.lock().await;
    let liq_snap = state.liquidation_aggregator.lock().await.snapshot();
    let spot = *state.spot_price.lock().await;
    let global_oi = {
        let b = state.binance_oi_tracker.lock().await;
        let by = state.bybit_oi_tracker.lock().await;
        let o = state.okx_oi_tracker.lock().await;
        compute_global_oi_state(&b, &by, &o)
    };
    let fg = state.fear_greed.lock().await.clone();
    let new_market = build_unified_market_state(
        sym.to_ascii_uppercase(),
        candles.clone(),
        walls,
        liquidity_analytics,
        Some(tf_biases_val),
        oi_is_real,
        fr,
        None,
        false,
        liq_snap,
        global_oi,
        spot,
        fg,
    );
    *state.market.write().await = new_market;
    *state.current_interval.lock().await = req.interval;

    state
        .trade_tx
        .send(TradeMessage::ResetInterval { bucket_ms, candles })
        .await
        .map_err(|e| internal_error(anyhow::anyhow!("trade channel closed: {e}")))?;

    Ok(StatusCode::OK)
}

fn is_local_url(url: &str) -> bool {
    let parsed = match url::Url::parse(url) {
        Ok(u) => u,
        Err(_) => return false,
    };

    if parsed.scheme() != "http" {
        return false;
    }

    if parsed.username() != "" || parsed.password().is_some() {
        return false;
    }

    match parsed.host() {
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(addr)) => addr == Ipv4Addr::LOCALHOST,
        Some(url::Host::Ipv6(addr)) => addr == Ipv6Addr::LOCALHOST,
        None => false,
    }
}

async fn analyze(
    State(state): State<AppState>,
    Json(request): Json<AnalyzeRequest>,
) -> Result<Json<AnalyzeResponse>, (axum::http::StatusCode, String)> {
    let market = state.market.read().await.clone();

    let broker = match request.llama_server_url {
        Some(url) if !url.is_empty() => {
            if !is_local_url(&url) {
                return Err((StatusCode::BAD_REQUEST, "llama_server_url must point to localhost".to_string()));
            }
            let model = env::var("OPENAI_MODEL").unwrap_or_else(|_| "qwen3-4b".to_string());
            AiBroker::llama_cpp_at(model, url)
        }
        _ => state.ai.clone(),
    };

    let target = broker
        .request_trade_plan(request.messages, &market)
        .await
        .map_err(analyze_error)?;

    let trade_log_id = match state.trade_log.insert(
        &market.symbol,
        target.entry_price,
        target.take_profit,
        target.stop_loss,
        target.thesis.as_deref(),
        request.position_size_pct,
        request.leverage,
    ) {
        Ok(id) => Some(id),
        Err(e) => {
            warn!("failed to log trade plan: {e:?}");
            None
        }
    };

    if let Some(id) = trade_log_id {
        match Arc::clone(&state.monitor_semaphore).try_acquire_owned() {
            Ok(permit) => {
                let log = state.trade_log.clone();
                let price_rx = state.price_tx.subscribe();
                let entry = target.entry_price;
                let tp = target.take_profit;
                let sl = target.stop_loss;
                tokio::spawn(async move {
                    let _permit = permit;
                    monitor_trade_outcome(log, id, entry, tp, sl, price_rx).await;
                });
            }
            Err(_) => {
                warn!("trade {id}: monitor capacity full (5 active); outcome auto-detection skipped");
            }
        }
    }

    Ok(Json(AnalyzeResponse { target, trade_log_id }))
}

async fn trades(State(state): State<AppState>) -> Result<Json<Vec<trade_log::TradeRecord>>, (StatusCode, String)> {
    state.trade_log.recent(200).map(Json).map_err(internal_error)
}

async fn trades_summary(State(state): State<AppState>) -> Result<Json<trade_log::TradeSummary>, (StatusCode, String)> {
    state.trade_log.summary().map(Json).map_err(internal_error)
}

async fn patch_trade_outcome(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<i64>,
    Json(body): Json<SetOutcomeRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    state.trade_log
        .update_outcome(id, &body.outcome)
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(internal_error)
}

fn internal_error(err: anyhow::Error) -> (axum::http::StatusCode, String) {
    error!("{err:?}");
    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "internal server error".to_string())
}

fn analyze_error(err: anyhow::Error) -> (axum::http::StatusCode, String) {
    error!("{err:?}");
    let msg = err.to_string();
    if msg.contains("plan rejected") {
        return (
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            format!("Trade plan rejected by risk rules: {msg}"),
        );
    }
    (
        axum::http::StatusCode::BAD_GATEWAY,
        format!("LLM request failed: {msg}"),
    )
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_client(socket, state))
}

async fn handle_ws_client(mut socket: WebSocket, state: AppState) {
    // Send current state immediately so the client renders without waiting for the next trade.
    if let Ok(json) = serde_json::to_string(&*state.market.read().await) {
        let _ = socket.send(Message::Text(json.into())).await;
    }

    let mut rx = state.snapshot_tx.subscribe();
    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(data) => {
                        if socket.send(Message::Text(data.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("ws client lagged by {n} messages; resuming from next");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Ping(data))) => {
                        let _ = socket.send(Message::Pong(data)).await;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}

async fn poll_rest_agg_trades(
    symbol: &str,
    last_agg_id: &mut i64,
    sender: &mpsc::Sender<TradeMessage>,
) -> bool {
    let url = format!(
        "https://fapi.binance.com/fapi/v1/aggTrades?symbol={}&limit=100",
        symbol.to_ascii_uppercase()
    );
    let trades: Vec<BinanceAggTrade> = match reqwest::get(&url).await {
        Ok(r) => match r.json().await {
            Ok(v) => v,
            Err(e) => { warn!("aggTrade REST parse error: {e}"); return true; }
        },
        Err(e) => { warn!("aggTrade REST request error: {e}"); return true; }
    };
    for trade in trades {
        if trade.agg_id <= *last_agg_id {
            continue;
        }
        *last_agg_id = trade.agg_id;
        if sender.send(TradeMessage::Trade(trade)).await.is_err() {
            return false;
        }
    }
    true
}

async fn stream_binance_agg_trades(
    initial_symbol: String,
    sender: mpsc::Sender<TradeMessage>,
    mut symbol_rx: watch::Receiver<String>,
) {
    let mut current_symbol = initial_symbol;

    loop {
        let endpoint = format!(
            "wss://fstream.binance.com/ws/{}@aggTrade",
            current_symbol.to_ascii_lowercase()
        );

        let ws_request = WsRequest::builder()
            .uri(endpoint.as_str())
            .header("Host", "fstream.binance.com")
            .header("User-Agent", "Mozilla/5.0 columba/0.1")
            .header("Origin", "https://fstream.binance.com")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", generate_key())
            .body(())
            .expect("valid ws request");

        let ws_ok = match connect_async(ws_request).await {
            Ok((mut socket, _)) => {
                info!("connected to Binance futures aggTrade stream: {endpoint}");
                let mut frames_received = false;
                let silence_deadline = Duration::from_secs(6);

                let result = loop {
                    tokio::select! {
                        msg = tokio::time::timeout(silence_deadline, socket.next()) => {
                            match msg {
                                Ok(Some(Ok(raw))) if raw.is_text() => {
                                    frames_received = true;
                                    match serde_json::from_str::<BinanceAggTrade>(
                                        raw.to_text().unwrap_or_default(),
                                    ) {
                                        Ok(trade) => {
                                            if sender.send(TradeMessage::Trade(trade)).await.is_err() {
                                                break Err("channel closed");
                                            }
                                        }
                                        Err(err) => warn!("invalid futures aggTrade payload: {err}"),
                                    }
                                }
                                Ok(Some(Ok(WsMessage::Ping(data)))) => {
                                    let _ = socket.send(WsMessage::Pong(data)).await;
                                }
                                Ok(Some(Ok(_))) => {}
                                Ok(Some(Err(err))) => {
                                    warn!("futures WS read error: {err}");
                                    break Ok(false);
                                }
                                Ok(None) => break Ok(false),
                                Err(_) => {
                                    // timeout — silence detected
                                    if !frames_received {
                                        info!("futures WS silent for {}s, switching to REST fallback", silence_deadline.as_secs());
                                    }
                                    break Ok(frames_received);
                                }
                            }
                        }
                        Ok(()) = symbol_rx.changed() => {
                            current_symbol = symbol_rx.borrow_and_update().clone();
                            info!("symbol change, reconnecting to {}", current_symbol.to_ascii_uppercase());
                            break Ok(true);
                        }
                    }
                };

                match result {
                    Err(_) => return,
                    Ok(had_frames) => had_frames,
                }
            }
            Err(err) => {
                warn!("futures WS connect failed: {err}");
                false
            }
        };

        if !ws_ok {
            // Futures WS delivered no frames — fall back to REST polling.
            // Retry WS periodically so we switch back if the block lifts.
            info!("entering REST-poll fallback for {}", current_symbol.to_ascii_uppercase());
            let mut last_agg_id: i64 = -1;
            let mut ws_retry_interval = tokio::time::interval(Duration::from_secs(60));
            ws_retry_interval.tick().await; // consume the immediate tick

            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {
                        if !poll_rest_agg_trades(&current_symbol, &mut last_agg_id, &sender).await {
                            return;
                        }
                    }
                    _ = ws_retry_interval.tick() => {
                        // Periodically try WS again — exit REST loop to re-attempt.
                        info!("retrying futures WS for {}", current_symbol.to_ascii_uppercase());
                        break;
                    }
                    Ok(()) = symbol_rx.changed() => {
                        current_symbol = symbol_rx.borrow_and_update().clone();
                        info!("symbol change in REST fallback, reconnecting to {}", current_symbol.to_ascii_uppercase());
                        break;
                    }
                }
            }
            // No extra sleep before WS retry.
            continue;
        }

        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

async fn monitor_trade_outcome(
    trade_log: TradeLog,
    trade_log_id: i64,
    entry_price: f64,
    take_profit: f64,
    stop_loss: f64,
    mut price_rx: broadcast::Receiver<f64>,
) {
    let is_long = take_profit >= entry_price;
    // Must see price on the approach side (above entry for long, below for short) before
    // counting a fill. Prevents phantom fills when price is already past entry at plan time.
    let mut has_approach = false;
    let mut has_entry = false;
    let mut ticks_since_entry: u32 = 0;
    const MIN_TICKS: u32 = 3;

    let timeout = tokio::time::sleep(Duration::from_secs(86_400));
    tokio::pin!(timeout);

    loop {
        let price = tokio::select! {
            _ = &mut timeout => {
                let _ = trade_log.update_outcome(trade_log_id, "EXPIRED");
                info!("trade {trade_log_id} monitor timed out after 24h");
                return;
            }
            result = price_rx.recv() => match result {
                Ok(p) => p,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("trade {trade_log_id} monitor lagged {n} ticks; TP/SL may have been missed");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        };

        if !has_approach {
            has_approach = if is_long { price > entry_price } else { price < entry_price };
            continue;
        }

        if !has_entry {
            let hit = if is_long { price <= entry_price } else { price >= entry_price };
            if hit {
                has_entry = true;
                ticks_since_entry = 0;
            }
            continue;
        }

        ticks_since_entry += 1;
        if ticks_since_entry < MIN_TICKS {
            continue;
        }

        if (is_long && price >= take_profit) || (!is_long && price <= take_profit) {
            let _ = trade_log.update_outcome(trade_log_id, "TP_HIT");
            info!("trade {trade_log_id} TP hit at {price:.2}");
            return;
        }
        if (is_long && price <= stop_loss) || (!is_long && price >= stop_loss) {
            let _ = trade_log.update_outcome(trade_log_id, "SL_HIT");
            info!("trade {trade_log_id} SL hit at {price:.2}");
            return;
        }
    }
}

async fn process_trade_deltas(
    mut receiver: mpsc::Receiver<TradeMessage>,
    market: Arc<RwLock<UnifiedMarketState>>,
    tf_biases: Arc<Mutex<TfBiases>>,
    binance_oi_tracker: Arc<Mutex<OiTracker>>,
    bybit_oi_tracker: Arc<Mutex<OiTracker>>,
    okx_oi_tracker: Arc<Mutex<OiTracker>>,
    oi_is_real: Arc<Mutex<bool>>,
    wall_tracker: Arc<Mutex<WallTracker>>,
    current_funding_rate: Arc<Mutex<Option<f64>>>,
    liquidation_aggregator: Arc<Mutex<LiquidationAggregator>>,
    spot_price: Arc<Mutex<Option<f64>>>,
    fear_greed: Arc<Mutex<FearGreedState>>,
    snapshot_tx: Arc<broadcast::Sender<String>>,
    price_tx: Arc<broadcast::Sender<f64>>,
) {
    let mut cvd = Decimal::ZERO;
    let mut candles: Vec<CandleData> = Vec::new();
    let mut active_bucket = 0_i64;
    let mut bucket_ms: i64 = 60_000;
    let mut last_broadcast: Option<Instant> = None;
    let mut signal_cache = SignalCache::default();
    let mut live_candle_closes: u32 = 0;

    while let Some(message) = receiver.recv().await {
        match message {
            TradeMessage::ResetSymbol { candles: seeded } => {
                signal_cache.invalidate();
                live_candle_closes = 0;
                cvd = seeded.last().map(|c| c.cvd).unwrap_or(Decimal::ZERO);
                active_bucket = seeded.last().map(|c| c.timestamp / bucket_ms).unwrap_or(0);
                candles = seeded;
                // Broadcast the market state (already updated by set_symbol) so the
                // client gets the full seeded history immediately, not just 1 live candle.
                let current = market.read().await.clone();
                if let Ok(json) = serde_json::to_string(&current) {
                    let _ = snapshot_tx.send(json);
                }
                last_broadcast = Some(Instant::now());
            }
            TradeMessage::ResetInterval { bucket_ms: new_ms, candles: seeded } => {
                bucket_ms = new_ms;
                cvd = seeded.last().map(|c| c.cvd).unwrap_or(Decimal::ZERO);
                active_bucket = seeded.last().map(|c| c.timestamp / new_ms).unwrap_or(0);
                let symbol = market.read().await.symbol.clone();
                let biases = tf_biases.lock().await.clone();
                let oi_real = *oi_is_real.lock().await;
                let (walls, liquidity_analytics) = {
                    let t = wall_tracker.lock().await;
                    (t.snapshot_walls(), t.snapshot_analytics())
                };
                let fr = *current_funding_rate.lock().await;
                signal_cache.invalidate();
                live_candle_closes = 0;
                let liq_snap = liquidation_aggregator.lock().await.snapshot();
                let global_oi = {
                    let b = binance_oi_tracker.lock().await;
                    let by = bybit_oi_tracker.lock().await;
                    let o = okx_oi_tracker.lock().await;
                    compute_global_oi_state(&b, &by, &o)
                };
                let spot = *spot_price.lock().await;
                let fg = fear_greed.lock().await.clone();
                let reset_state = build_unified_market_state(
                    symbol,
                    seeded.clone(),
                    walls,
                    liquidity_analytics,
                    Some(biases),
                    oi_real,
                    fr,
                    None,
                    false,
                    liq_snap,
                    global_oi,
                    spot,
                    fg,
                );
                if let Ok(json) = serde_json::to_string(&reset_state) {
                    let _ = snapshot_tx.send(json);
                }
                *market.write().await = reset_state;
                candles = seeded;
                last_broadcast = Some(Instant::now());
            }
            TradeMessage::Trade(trade) => {
                let price = match trade.price.parse::<f64>() {
                    Ok(p) if p.is_finite() && p > 0.0 => p,
                    _ => {
                        warn!("rejected malformed aggTrade: price={:?}", trade.price);
                        continue;
                    }
                };
                let quantity = match trade.quantity.parse::<f64>() {
                    Ok(q) if q.is_finite() && q > 0.0 => q,
                    _ => {
                        warn!("rejected malformed aggTrade: quantity={:?}", trade.quantity);
                        continue;
                    }
                };
                if trade.trade_time <= 0 {
                    warn!("rejected malformed aggTrade: trade_time={}", trade.trade_time);
                    continue;
                }
                // Broadcast raw price to trade monitors before candle logic.
                let _ = price_tx.send(price);
                let quantity_decimal = decimal_from_trade_qty(quantity);
                cvd = apply_cvd_trade_delta(cvd, quantity_decimal, trade.buyer_is_maker);

                let bucket = trade.trade_time / bucket_ms;
                if active_bucket != bucket {
                    active_bucket = bucket;
                    live_candle_closes = live_candle_closes.saturating_add(1);
                    let oi = binance_oi_tracker.lock().await.current;
                    candles.push(CandleData {
                        timestamp: bucket * bucket_ms,
                        open: price,
                        high: price,
                        low: price,
                        close: price,
                        volume: quantity,
                        buy_volume: if trade.buyer_is_maker {
                            Decimal::ZERO
                        } else {
                            quantity_decimal
                        },
                        sell_volume: if trade.buyer_is_maker {
                            quantity_decimal
                        } else {
                            Decimal::ZERO
                        },
                        cvd,
                        open_interest: oi,
                    });
                } else if let Some(candle) = candles.last_mut() {
                    candle.high = candle.high.max(price);
                    candle.low = candle.low.min(price);
                    candle.close = price;
                    candle.volume += quantity;
                    candle.cvd = cvd;

                    if trade.buyer_is_maker {
                        candle.sell_volume += quantity_decimal;
                    } else {
                        candle.buy_volume += quantity_decimal;
                    }
                }

                if candles.len() > 1500 {
                    let drain_to = candles.len() - 1500;
                    candles.drain(0..drain_to);
                }

                let should_broadcast = last_broadcast
                    .map(|t| t.elapsed() >= Duration::from_millis(250))
                    .unwrap_or(true);

                if should_broadcast {
                    let t0 = Instant::now();
                    last_broadcast = Some(t0);
                    let symbol = market.read().await.symbol.clone();
                    let biases = tf_biases.lock().await.clone();
                    let oi_real = *oi_is_real.lock().await;
                    let (walls, liquidity_analytics) = {
                        let t = wall_tracker.lock().await;
                        (t.snapshot_walls(), t.snapshot_analytics())
                    };
                    let fr = *current_funding_rate.lock().await;
                    let liq_snap = liquidation_aggregator.lock().await.snapshot();
                    let global_oi = {
                        let b = binance_oi_tracker.lock().await;
                        let by = bybit_oi_tracker.lock().await;
                        let o = okx_oi_tracker.lock().await;
                        compute_global_oi_state(&b, &by, &o)
                    };
                    let spot = *spot_price.lock().await;
                    let fg = fear_greed.lock().await.clone();
                    let next_state = build_unified_market_state(
                        symbol,
                        candles.clone(),
                        walls,
                        liquidity_analytics,
                        Some(biases),
                        oi_real,
                        fr,
                        Some(&mut signal_cache),
                        live_candle_closes >= 1,
                        liq_snap,
                        global_oi,
                        spot,
                        fg,
                    );
                    let json = serde_json::to_string(&next_state).ok();
                    *market.write().await = next_state;
                    if let Some(j) = json {
                        let _ = snapshot_tx.send(j);
                    }
                    let elapsed = t0.elapsed();
                    if elapsed.as_millis() > 20 {
                        warn!("broadcast cycle slow: {}ms", elapsed.as_millis());
                    }
                }
            }
        }
    }
}

/// Connects to the Binance !forceOrder@arr combined liquidation stream and pushes
/// parsed events into the shared aggregator. No REST fallback exists for this stream.
/// On failure, the aggregator is marked unhealthy and the stream retries with
/// exponential backoff — failure never blocks the core aggTrade pipeline.
async fn stream_liquidations(
    symbol_rx: watch::Receiver<String>,
    aggregator: Arc<Mutex<LiquidationAggregator>>,
) {
    let endpoint = "wss://fstream.binance.com/ws/!forceOrder@arr";
    let mut retry_delay = Duration::from_secs(5);

    loop {
        let ws_req = match WsRequest::builder()
            .uri(endpoint)
            .header("Host", "fstream.binance.com")
            .header("User-Agent", "columba/1.0")
            .header("Origin", "https://fstream.binance.com")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", generate_key())
            .body(())
        {
            Ok(r) => r,
            Err(e) => {
                warn!("liquidation stream: failed to build WS request: {e}");
                tokio::time::sleep(retry_delay).await;
                retry_delay = (retry_delay * 2).min(Duration::from_secs(300));
                continue;
            }
        };

        match connect_async(ws_req).await {
            Ok((mut socket, _)) => {
                info!("liquidation stream connected");
                retry_delay = Duration::from_secs(5);
                aggregator.lock().await.mark_healthy();

                loop {
                    match socket.next().await {
                        Some(Ok(WsMessage::Text(text))) => {
                            let active_sym = symbol_rx.borrow().to_ascii_uppercase();
                            if let Some(event) = parse_force_order(&text, &active_sym) {
                                aggregator.lock().await.push(event);
                            }
                        }
                        Some(Ok(WsMessage::Ping(p))) => {
                            let _ = socket.send(WsMessage::Pong(p)).await;
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => {
                            warn!("liquidation stream error: {e}");
                            break;
                        }
                        None => break,
                    }
                }

                aggregator.lock().await.mark_unhealthy();
                warn!("liquidation stream disconnected");
            }
            Err(e) => {
                warn!("liquidation stream: connect failed: {e}");
            }
        }

        tokio::time::sleep(retry_delay).await;
        retry_delay = (retry_delay * 2).min(Duration::from_secs(300));
    }
}

#[allow(dead_code)]
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}
