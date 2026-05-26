mod ai;
mod hardware;
mod quant;

use ai::{AiBroker, ChatMessage, TradePlan};
use anyhow::{Context, Result};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
use hardware::{determine_execution_target, AgentTarget};
use quant::{
    apply_cvd_trade_delta, build_unified_market_state, compute_tf_bias, decimal_from_trade_qty,
    CandleData, LiquidityWalls, TfBiases, UnifiedMarketState,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::{
    env,
    net::SocketAddr,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{broadcast, mpsc, watch, Mutex};
use tokio_tungstenite::connect_async;
use tower_http::cors::CorsLayer;
use tracing::{error, info, warn};

#[derive(Clone)]
struct AppState {
    market: Arc<Mutex<UnifiedMarketState>>,
    ai: AiBroker,
    symbol_tx: Arc<watch::Sender<String>>,
    trade_tx: mpsc::Sender<TradeMessage>,
    current_interval: Arc<Mutex<String>>,
    tf_biases: Arc<Mutex<TfBiases>>,
    current_oi: Arc<Mutex<Option<f64>>>,
    snapshot_tx: Arc<broadcast::Sender<String>>,
}

#[derive(Debug, Deserialize)]
struct AnalyzeRequest {
    messages: Vec<ChatMessage>,
    openai_api_key: Option<String>,
    ollama_url: Option<String>,
}

#[derive(Debug, Serialize)]
struct AnalyzeResponse {
    target: TradePlan,
}

#[derive(Debug, Deserialize)]
struct SetSymbolRequest {
    symbol: String,
}

#[derive(Debug, Deserialize)]
struct SetIntervalRequest {
    interval: String,
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

async fn merge_open_interest(candles: &mut Vec<CandleData>, symbol: &str, interval: &str) {
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
            warn!("failed to fetch OI history for {symbol}: {e:?}");
            return;
        }
    };

    if oi_data.is_empty() {
        return;
    }

    for candle in candles.iter_mut() {
        if let Some((_, oi)) = oi_data
            .iter()
            .min_by_key(|(ts, _)| (ts - candle.timestamp).unsigned_abs())
        {
            candle.open_interest = Some(*oi);
        }
    }
}

async fn fetch_klines_with_oi(symbol: &str, interval: &str) -> Result<Vec<CandleData>> {
    let mut candles = fetch_recent_klines(symbol, interval).await?;
    merge_open_interest(&mut candles, symbol, interval).await;
    Ok(candles)
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
    current_oi: Arc<Mutex<Option<f64>>>,
) {
    let mut symbol = symbol_rx.borrow().clone();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let url = format!(
                    "https://fapi.binance.com/fapi/v1/openInterest?symbol={}",
                    symbol.to_ascii_uppercase()
                );
                match reqwest::get(&url).await {
                    Ok(resp) => {
                        match resp.json::<OiCurrentResponse>().await {
                            Ok(data) => {
                                if let Ok(oi) = data.open_interest.parse::<f64>() {
                                    *current_oi.lock().await = Some(oi);
                                }
                            }
                            Err(e) => warn!("failed to parse OI response: {e:?}"),
                        }
                    }
                    Err(e) => warn!("OI poll request failed: {e:?}"),
                }
            }
            Ok(()) = symbol_rx.changed() => {
                symbol = symbol_rx.borrow_and_update().clone();
                *current_oi.lock().await = None;
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
                .unwrap_or_else(|_| "http://localhost:11434/v1".to_string());
            AiBroker::ollama_at(model, url)
        }
        AgentTarget::Cloud(_model) if api_key.is_empty() => {
            warn!("OPENAI_API_KEY is unset; falling back to local Ollama model");
            AiBroker::ollama("llama3.2:3b".to_string())
        }
        AgentTarget::Cloud(model) if api_key.starts_with("sk-ant-") => {
            let anthropic_model =
                env::var("ANTHROPIC_MODEL").unwrap_or(model);
            info!("Anthropic key detected; using model {anthropic_model}");
            AiBroker::anthropic(api_key, anthropic_model)
        }
        AgentTarget::Cloud(model) => AiBroker::openai(api_key, model),
    };

    let initial_candles = match fetch_klines_with_oi(&symbol, "1m").await {
        Ok(candles) => candles,
        Err(err) => {
            warn!("failed to seed recent Binance klines: {err:?}");
            Vec::new()
        }
    };

    let initial_tf_biases = fetch_tf_biases(&symbol).await;
    let tf_biases = Arc::new(Mutex::new(initial_tf_biases.clone()));
    let current_oi: Arc<Mutex<Option<f64>>> = Arc::new(Mutex::new(None));

    let initial_market = build_unified_market_state(
        symbol.to_uppercase(),
        initial_candles,
        LiquidityWalls {
            bid_wall_price: None,
            bid_wall_size: None,
            ask_wall_price: None,
            ask_wall_size: None,
        },
        Some(initial_tf_biases),
    );

    let (symbol_tx, symbol_rx) = watch::channel(symbol.clone());
    let (trade_tx, trade_rx) = mpsc::channel::<TradeMessage>(4096);
    let (snapshot_tx, _) = broadcast::channel::<String>(64);
    let snapshot_tx = Arc::new(snapshot_tx);

    let state = AppState {
        market: Arc::new(Mutex::new(initial_market)),
        ai,
        symbol_tx: Arc::new(symbol_tx),
        trade_tx: trade_tx.clone(),
        current_interval: Arc::new(Mutex::new("1m".to_string())),
        tf_biases,
        current_oi,
        snapshot_tx: Arc::clone(&snapshot_tx),
    };

    let oi_symbol_rx = symbol_rx.clone();
    tokio::spawn(stream_binance_agg_trades(symbol.clone(), trade_tx, symbol_rx));
    tokio::spawn(process_trade_deltas(
        trade_rx,
        Arc::clone(&state.market),
        Arc::clone(&state.tf_biases),
        Arc::clone(&state.current_oi),
        snapshot_tx,
    ));
    tokio::spawn(poll_open_interest(oi_symbol_rx, Arc::clone(&state.current_oi)));

    let cors = CorsLayer::permissive();

    let app = Router::new()
        .route("/api/analyze", post(analyze))
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
    Json(state.market.lock().await.clone())
}

async fn set_symbol(
    State(state): State<AppState>,
    Json(req): Json<SetSymbolRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let sym = req.symbol.to_ascii_lowercase();
    let interval = state.current_interval.lock().await.clone();
    info!("switching active symbol to {} (interval={})", sym.to_ascii_uppercase(), interval);

    let candles = fetch_klines_with_oi(&sym, &interval).await.map_err(internal_error)?;
    let tf_biases_val = fetch_tf_biases(&sym).await;
    *state.tf_biases.lock().await = tf_biases_val.clone();
    *state.current_oi.lock().await = None;

    let new_market = build_unified_market_state(
        sym.to_ascii_uppercase(),
        candles.clone(),
        LiquidityWalls {
            bid_wall_price: None,
            bid_wall_size: None,
            ask_wall_price: None,
            ask_wall_size: None,
        },
        Some(tf_biases_val),
    );
    *state.market.lock().await = new_market;

    // Seed the trade processor with the fetched candles so it broadcasts a full
    // snapshot immediately instead of starting from scratch on the first live trade.
    state
        .trade_tx
        .send(TradeMessage::ResetSymbol { candles })
        .await
        .map_err(|e| internal_error(anyhow::anyhow!("trade channel closed: {e}")))?;

    state.symbol_tx.send(sym).ok();

    Ok(StatusCode::OK)
}

async fn set_interval(
    State(state): State<AppState>,
    Json(req): Json<SetIntervalRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let bucket_ms = bucket_ms_from_interval(&req.interval).ok_or_else(|| {
        (StatusCode::BAD_REQUEST, format!("unknown interval: {}", req.interval))
    })?;

    let sym = state.market.lock().await.symbol.to_ascii_lowercase();
    info!("switching interval to {} for {}", req.interval, sym.to_ascii_uppercase());

    let candles = fetch_klines_with_oi(&sym, &req.interval).await.map_err(internal_error)?;
    let tf_biases_val = fetch_tf_biases(&sym).await;
    *state.tf_biases.lock().await = tf_biases_val.clone();

    let new_market = build_unified_market_state(
        sym.to_ascii_uppercase(),
        candles.clone(),
        LiquidityWalls {
            bid_wall_price: None,
            bid_wall_size: None,
            ask_wall_price: None,
            ask_wall_size: None,
        },
        Some(tf_biases_val),
    );
    *state.market.lock().await = new_market;
    *state.current_interval.lock().await = req.interval;

    state
        .trade_tx
        .send(TradeMessage::ResetInterval { bucket_ms, candles })
        .await
        .map_err(|e| internal_error(anyhow::anyhow!("trade channel closed: {e}")))?;

    Ok(StatusCode::OK)
}

async fn analyze(
    State(state): State<AppState>,
    Json(request): Json<AnalyzeRequest>,
) -> Result<Json<AnalyzeResponse>, (axum::http::StatusCode, String)> {
    let market = state.market.lock().await.clone();

    let broker = match (request.openai_api_key, request.ollama_url) {
        (Some(key), _) if key.starts_with("sk-ant-") => {
            let model = env::var("ANTHROPIC_MODEL")
                .unwrap_or_else(|_| "claude-sonnet-4-6".to_string());
            AiBroker::anthropic(key, model)
        }
        (Some(key), _) if !key.is_empty() => {
            let preferred_model =
                env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
            AiBroker::openai(key, preferred_model)
        }
        (_, Some(url)) if !url.is_empty() => {
            let model = env::var("OPENAI_MODEL").unwrap_or_else(|_| "llama3.2:3b".to_string());
            AiBroker::ollama_at(model, url)
        }
        _ => state.ai.clone(),
    };

    let target = broker
        .request_trade_plan(request.messages, &market)
        .await
        .map_err(internal_error)?;

    Ok(Json(AnalyzeResponse { target }))
}

fn internal_error(err: anyhow::Error) -> (axum::http::StatusCode, String) {
    error!("{err:?}");
    (axum::http::StatusCode::BAD_GATEWAY, err.to_string())
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_client(socket, state))
}

async fn handle_ws_client(mut socket: WebSocket, state: AppState) {
    // Send current state immediately so the client renders without waiting for the next trade.
    if let Ok(json) = serde_json::to_string(&*state.market.lock().await) {
        let _ = socket.send(Message::Text(json.into())).await;
    }

    let mut rx = state.snapshot_tx.subscribe();
    while let Ok(data) = rx.recv().await {
        if socket.send(Message::Text(data.into())).await.is_err() {
            break;
        }
    }
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

        match connect_async(endpoint.as_str()).await {
            Ok((socket, _)) => {
                info!("connected to Binance aggTrade stream: {endpoint}");
                let (_, mut read) = socket.split();

                loop {
                    tokio::select! {
                        msg = read.next() => {
                            match msg {
                                Some(Ok(msg)) if msg.is_text() => {
                                    match serde_json::from_str::<BinanceAggTrade>(
                                        msg.to_text().unwrap_or_default(),
                                    ) {
                                        Ok(trade) => {
                                            if sender.send(TradeMessage::Trade(trade)).await.is_err() {
                                                warn!("trade processor channel closed");
                                                return;
                                            }
                                        }
                                        Err(err) => warn!("invalid Binance aggTrade payload: {err}"),
                                    }
                                }
                                Some(Ok(_)) => {}
                                Some(Err(err)) => {
                                    warn!("Binance websocket read error: {err}");
                                    break;
                                }
                                None => break,
                            }
                        }
                        Ok(()) = symbol_rx.changed() => {
                            current_symbol = symbol_rx.borrow_and_update().clone();
                            info!("symbol change detected, reconnecting to {}", current_symbol.to_ascii_uppercase());
                            // ResetSymbol with seeded candles is sent by set_symbol handler directly.
                            break;
                        }
                    }
                }
            }
            Err(err) => warn!("Binance websocket connection failed: {err}"),
        }

        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}

async fn process_trade_deltas(
    mut receiver: mpsc::Receiver<TradeMessage>,
    market: Arc<Mutex<UnifiedMarketState>>,
    tf_biases: Arc<Mutex<TfBiases>>,
    current_oi: Arc<Mutex<Option<f64>>>,
    snapshot_tx: Arc<broadcast::Sender<String>>,
) {
    let mut cvd = Decimal::ZERO;
    let mut candles: Vec<CandleData> = Vec::new();
    let mut active_bucket = 0_i64;
    let mut bucket_ms: i64 = 60_000;
    let mut last_broadcast: Option<Instant> = None;

    while let Some(message) = receiver.recv().await {
        match message {
            TradeMessage::ResetSymbol { candles: seeded } => {
                cvd = seeded.last().map(|c| c.cvd).unwrap_or(Decimal::ZERO);
                active_bucket = seeded.last().map(|c| c.timestamp / bucket_ms).unwrap_or(0);
                candles = seeded;
                // Broadcast the market state (already updated by set_symbol) so the
                // client gets the full seeded history immediately, not just 1 live candle.
                let current = market.lock().await.clone();
                if let Ok(json) = serde_json::to_string(&current) {
                    let _ = snapshot_tx.send(json);
                }
                last_broadcast = Some(Instant::now());
            }
            TradeMessage::ResetInterval { bucket_ms: new_ms, candles: seeded } => {
                bucket_ms = new_ms;
                cvd = seeded.last().map(|c| c.cvd).unwrap_or(Decimal::ZERO);
                active_bucket = seeded.last().map(|c| c.timestamp / new_ms).unwrap_or(0);
                let symbol = market.lock().await.symbol.clone();
                let biases = tf_biases.lock().await.clone();
                let reset_state = build_unified_market_state(
                    symbol,
                    seeded.clone(),
                    LiquidityWalls {
                        bid_wall_price: None,
                        bid_wall_size: None,
                        ask_wall_price: None,
                        ask_wall_size: None,
                    },
                    Some(biases),
                );
                if let Ok(json) = serde_json::to_string(&reset_state) {
                    let _ = snapshot_tx.send(json);
                }
                *market.lock().await = reset_state;
                candles = seeded;
                last_broadcast = Some(Instant::now());
            }
            TradeMessage::Trade(trade) => {
                let price = trade.price.parse::<f64>().unwrap_or_default();
                let quantity = trade.quantity.parse::<f64>().unwrap_or_default();
                let quantity_decimal = decimal_from_trade_qty(quantity);
                cvd = apply_cvd_trade_delta(cvd, quantity_decimal, trade.buyer_is_maker);

                let bucket = trade.trade_time / bucket_ms;
                if active_bucket != bucket {
                    active_bucket = bucket;
                    let oi = *current_oi.lock().await;
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

                let symbol = market.lock().await.symbol.clone();
                let biases = tf_biases.lock().await.clone();
                let next_state = build_unified_market_state(
                    symbol,
                    candles.clone(),
                    LiquidityWalls {
                        bid_wall_price: None,
                        bid_wall_size: None,
                        ask_wall_price: None,
                        ask_wall_size: None,
                    },
                    Some(biases),
                );

                *market.lock().await = next_state.clone();

                let should_broadcast = last_broadcast
                    .map(|t| t.elapsed() >= Duration::from_millis(250))
                    .unwrap_or(true);

                if should_broadcast {
                    last_broadcast = Some(Instant::now());
                    if let Ok(json) = serde_json::to_string(&next_state) {
                        let _ = snapshot_tx.send(json);
                    }
                }
            }
        }
    }
}

#[allow(dead_code)]
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}
