# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

ColumbaProject is a crypto futures technical analysis dashboard. Rust Axum backend ingests Binance Futures data, computes CVD/ATR quant signals, and routes trade-plan requests to a bundled llama-server (llama.cpp) or cloud OpenAI/Anthropic. React + Lightweight Charts frontend renders multi-pane charts and hosts a chat UI.

## Commands

### Development (recommended — Makefile)
```bash
make setup                    # detect hardware tier, download llama-server + matching Qwen3 model pair
make setup-tier TIER=low_end  # force a specific tier (low_end / mid_range / high_end)
make detect-tier              # print which hardware tier was detected (no downloads)
make dev                      # spawn llama-server + backend + frontend in parallel (colored output)
make app                      # frontend + Tauri desktop app in parallel
make app-build                # tsc build → cargo tauri build (release installer)
```

### Backend (standalone)
```bash
cd backend && cargo check          # type-check without full build
cd backend && cargo build          # compile
cd backend && cargo run            # run (seeds klines then starts Axum on :8080)
cd backend && cargo test           # run unit tests (quant.rs suite)
cd backend && cargo test -- <name> # run single test by name
```

### Frontend (standalone)
```bash
cd frontend && npm install         # install deps
cd frontend && npm run dev         # dev server at http://127.0.0.1:5173
cd frontend && npm run build       # tsc + vite build → dist/
```

### Tauri desktop app
```bash
cd src-tauri && cargo tauri dev    # dev mode (requires frontend dev server already running)
cd src-tauri && cargo tauri build  # release build
```

## Environment Variables

| Variable | Default | Notes |
|---|---|---|
| `COLUMBA_SYMBOL` | `btcusdt` | Binance Futures symbol at startup |
| `COLUMBA_EXECUTION_MODE` | `Auto` | `Auto`, `Local`, or `Cloud` |
| `OPENAI_API_KEY` | _(empty)_ | Keys starting with `sk-ant-` auto-route to Anthropic; unset → falls back to local llama-server |
| `OPENAI_MODEL` | `gpt-4o-mini` | Model name for OpenAI; local fallback uses `qwen3-4b` |
| `ANTHROPIC_MODEL` | `claude-sonnet-4-6` | Model used when an Anthropic key is detected |
| `BIND_ADDR` | `127.0.0.1:8080` | Backend listen address |
| `COLUMBA_LLAMA_SERVER_URL` | `http://127.0.0.1:8081/v1` | llama-server endpoint (set automatically by `make dev` and Tauri when bundled llama-server launches on `:8081`) |
| `COLUMBA_TRADE_LOG` | `columba_trades.db` | SQLite file for persisting trade plans; falls back to `:memory:` if path unwritable |
| `COLUMBA_MODELS_DIR` | `resources/models` | Directory where GGUF models are stored; Tauri sets this to the app data dir |
| `RUST_LOG` | `columba_backend=info,tower_http=info` | Standard tracing filter; set to `debug` to trace WS frames and polling tasks |
| `VITE_API_BASE` | `http://127.0.0.1:8080` | Frontend-only; overrides the backend URL (e.g. for remote backend or Tauri builds) |

If `OPENAI_API_KEY` is unset and mode resolves to `Cloud`, the backend falls back to `qwen3-4b` via the local llama-server at `http://127.0.0.1:8081/v1`.
If `OPENAI_API_KEY` starts with `sk-ant-`, the backend routes to `api.anthropic.com` using the Anthropic Messages API format.

## Architecture

### Data flow
```
Binance REST klines  ──▶  startup candle seed ──▶┐
Binance Futures WS   ──▶  Tokio socket task       │
                              │ mpsc channel       ▼
                              └──▶ processor ──▶ Arc<RwLock<UnifiedMarketState>>
                                                   │
                             GET /api/snapshot ◀───┤
                             POST /api/analyze ◀───┘──▶ AiBroker ──▶ OpenAI / llama-server
                                   │
                               React app
                                   ├── ChartManager (Lightweight Charts)
                                   └── SimulationEngine
```

### Backend modules (`backend/src/`)
The backend compiles as a **library crate** (`columba-backend`). `main.rs` is a thin binary that calls `columba_backend::run()`. The Tauri app (`src-tauri`) also links against the same library.

- **`lib.rs`** — `pub async fn run()`: Axum server, kline seeding, all routes, background task spawning. Routes: `GET /api/snapshot`, `POST /api/analyze`, `GET /api/trades`, `GET /api/trades/summary`, `PATCH /api/trades/:id/outcome`, `POST /api/symbol`, `POST /api/interval`, `GET /ws` (WebSocket). Four background polling tasks spawned here, each receiving a `watch::Receiver<String>` to react to symbol changes: `poll_open_interest` (15s), `poll_liquidity_walls` (10s), `poll_funding_rate` (30s), `poll_tf_biases` (5min). CORS allowlist is hardcoded to `localhost:5173`, `127.0.0.1:5173`, `tauri://localhost`, `https://tauri.localhost` — adding new origins requires editing `run()` directly. Each `POST /api/analyze` spawns a `monitor_trade_outcome` Tokio task that watches the live price broadcast and auto-sets `TP_HIT`/`SL_HIT`/`EXPIRED` in the DB; the monitor times out after 24 hours.
- **`quant.rs`** — `UnifiedMarketState`, `CandleData`, `LiquidityWalls`, `TfBiases`, `ConfluenceMatrix`. CVD accumulation via `Decimal`, CVD slope (linear regression 20 candles), ATR-14 + RSI (`ta` crate), VWAP over candle window, funding settlement hours calculation, `build_unified_market_state` assembles all signals. `SignalCache` is a local cache inside the WS processor loop that avoids recomputing expensive quant signals on every aggTrade tick; it is invalidated on symbol/interval reset.
- **`ai.rs`** — `AiBroker`, `TradePlan`, chat history pruning (master prompt + last 3 exchanges), market state injection, markdown fence stripping before JSON parse. `POST /api/analyze` accepts optional `llama_server_url` in the request body to override the broker URL per-request (used by the frontend settings panel).
- **`hardware.rs`** — `nvidia-smi` VRAM scan, 3 GB safety buffer deduction, `AgentTarget` routing (Local / Cloud).
- **`hardware_profiles.rs`** — `detect_hardware()` returns `HardwareProfile` with `ram_gb`, `physical_cores`, `vram_gb`, and `tier: HardwareTier` (`LowEnd`/`MidRange`/`HighEnd`). Detects CUDA via `nvidia-smi`, Vulkan via `vulkaninfo`. Tier drives model selection.
- **`model_selector.rs`** — `select_model_profile(hw)` maps `HardwareTier` → `ModelProfile` with `main_model`, `draft_model`, `context_size`, `threads` (70% of physical cores), `gpu_layers` (999 for GPU, 0 for CPU-only).
- **`model_manager.rs`** — `model_is_present(name)` validates size ≥ 50 MB and GGUF magic bytes. `download_model(url, dest, on_progress)` streams with progress callback. Models stored at `COLUMBA_MODELS_DIR` env var, fallback to `resources/models/`.
- **`trade_log.rs`** — `TradeLog` wraps `rusqlite::Connection` (bundled SQLite) behind `Arc<Mutex<>>`. Schema: `trade_log` table with `id`, `created_at` (Unix ms), `symbol`, `entry_price`, `take_profit`, `stop_loss`, `thesis`, `outcome`, `position_size_pct`, `leverage`. New columns added via `ALTER TABLE` migrations for pre-existing databases. `POST /api/analyze` inserts a record and returns its `trade_log_id`; `PATCH /api/trades/:id/outcome` updates it post-trade. `GET /api/trades/summary` returns `TradeSummary` with win_rate, `avg_r_multiple`, `expectancy` (TP_HIT = +reward/risk R, SL_HIT = −1R).

### Tauri desktop app (`src-tauri/`)
`src-tauri/src/lib.rs` is the Tauri entry point. On startup it: (1) calls `detect_hardware()` + `select_model_profile()` to determine which Qwen3 model pair to use, (2) checks model presence via `model_is_present()`, (3) if models ready, spawns llama-server via `inference_manager::spawn_llama_server()` with speculative decoding (`--model-draft`, `--parallel 2`, `--cont-batching`, `--flash-attn` when GPU), (4) spawns `columba_backend::run()`. The llama-server process is killed on window close.

`src-tauri/src/inference_manager.rs` — `spawn_llama_server(bin, models_dir, profile, port)` builds the full arg list. Drops `--flash-attn` for CPU-only tiers (unsupported in some llama.cpp builds). `server_url(port)` returns the `/v1` endpoint string.

Tauri commands exposed to frontend: `get_setup_status()` returns `SetupStatus` (tier, RAM/VRAM, model list with `present` flag, `ready` bool). `download_missing_model(name, url)` downloads a GGUF and emits `download_progress` events with `{ name, downloaded_bytes, total_bytes }`.

`resources/model_manifest.json` — maps tier keys (`low_end`, `mid_range`, `high_end`) to `{ main: {name, url, size_mb}, draft: {name, url, size_mb} }`. Source of truth for which models to download per hardware tier.

### Frontend modules (`frontend/src/`)
- **`ChartManager.ts`** — initializes three chart panes (candlestick+ATR, open interest, CVD), tracks `activePriceLines`; **must call `removePriceLine` on all active lines before drawing a new AI trade plan**
- **`SimulationEngine.ts`** — tracks entry/TP/SL state, fires browser alerts on hits
- **`App.tsx`** — three-panel layout, exchange definition loader, snapshot polling, chat → analyze → draw plan flow

## Critical Invariants

- CVD accumulation must use `rust_decimal::Decimal`, not `f64`, to prevent drift over high-frequency trade streams.
- WebSocket ingestion stays isolated from Axum handlers via `mpsc`; never perform socket reads inside request paths.
- `AiBroker` must strip ` ```json ` / ` ``` ` fences before calling `serde_json::from_str`.
- `ChartManager` must remove all `activePriceLines` before rendering a new trade plan.
- `UnifiedMarketState.oi_is_real` flag: when `false`, `CandleData.open_interest` contains `quote_volume` as proxy (Binance OI fetch failed). Frontend and AI prompt must distinguish these.
- WS snapshot broadcast is throttled to 250 ms in `process_trade_deltas`; do not remove this guard — it prevents flooding clients on active markets.
- Symbol and interval switching both send a `TradeMessage::Reset*` to the trade processor so it reseeds its in-memory candle buffer; without this the processor would re-accumulate CVD from zero against a stale buffer.
- `POST /api/symbol` and `POST /api/interval` both call `build_unified_market_state` synchronously and update `AppState.market` before sending the reset message, so the first WS frame after a reset carries the full seeded history.
- `validate_trade_plan` in `ai.rs` enforces a minimum 1.5:1 reward-to-risk ratio server-side; plans below this are rejected before the frontend ever receives them.
- Live candle buffer in `process_trade_deltas` is capped at 1500 candles; older candles are drained. This matches the kline seed limit.
- `UnifiedMarketState.cvd_seeded`: `false` until at least one full live candle has closed after startup/reset. When `false`, CVD came from kline REST data (different accumulation path than aggTrade stream), so directional signals are approximate.
