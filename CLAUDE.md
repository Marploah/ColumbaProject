# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

ColumbaProject is a crypto futures technical analysis dashboard. Rust Axum backend ingests Binance Futures data, computes CVD/ATR quant signals, and routes trade-plan requests to local Ollama or cloud OpenAI. React + Lightweight Charts frontend renders multi-pane charts and hosts a chat UI.

## Commands

### Backend
```bash
cd backend && cargo check          # type-check without full build
cd backend && cargo build          # compile
cd backend && cargo run            # run (seeds klines then starts Axum on :8080)
```

### Frontend
```bash
cd frontend && npm install         # install deps
cd frontend && npm run dev         # dev server at http://127.0.0.1:5173
cd frontend && npm run build       # tsc + vite build → dist/
```

## Environment Variables

| Variable | Default | Notes |
|---|---|---|
| `COLUMBA_SYMBOL` | `btcusdt` | Binance Futures symbol at startup |
| `COLUMBA_EXECUTION_MODE` | `Auto` | `Auto`, `Local`, or `Cloud` |
| `OPENAI_API_KEY` | _(empty)_ | Keys starting with `sk-ant-` auto-route to Anthropic; unset → falls back to Ollama |
| `OPENAI_MODEL` | `gpt-4o-mini` | Model for OpenAI / Ollama routing |
| `ANTHROPIC_MODEL` | `claude-sonnet-4-6` | Model used when an Anthropic key is detected |
| `BIND_ADDR` | `127.0.0.1:8080` | Backend listen address |

If `OPENAI_API_KEY` is unset and mode resolves to `Cloud`, the backend falls back to `llama3.1:8b` via Ollama at `http://localhost:11434/v1`.
If `OPENAI_API_KEY` starts with `sk-ant-`, the backend routes to `api.anthropic.com` using the Anthropic Messages API format.

## Architecture

### Data flow
```
Binance REST klines  ──▶  startup candle seed ──▶┐
Binance Futures WS   ──▶  Tokio socket task       │
                              │ mpsc channel       ▼
                              └──▶ processor ──▶ Arc<Mutex<UnifiedMarketState>>
                                                   │
                             GET /api/snapshot ◀───┤
                             POST /api/analyze ◀───┘──▶ AiBroker ──▶ OpenAI / Ollama
                                   │
                               React app
                                   ├── ChartManager (Lightweight Charts)
                                   └── SimulationEngine
```

### Backend modules (`backend/src/`)
- **`main.rs`** — Axum server, kline seeding, WebSocket + processor task spawn, routes `/api/snapshot`, `/api/analyze`, `/api/symbol`
- **`quant.rs`** — `UnifiedMarketState`, `CandleData`, CVD accumulation (via `rust_decimal::Decimal`), CVD slope (linear regression over 20 candles), ATR-14 (`ta` crate)
- **`ai.rs`** — `AiBroker`, `TradePlan`, chat history pruning (master prompt + last 3 exchanges), market state injection, markdown fence stripping before JSON parse
- **`hardware.rs`** — `nvidia-smi` VRAM scan, 3 GB safety buffer deduction, `AgentTarget` routing (Local / Cloud)

### Frontend modules (`frontend/src/`)
- **`ChartManager.ts`** — initializes three chart panes (candlestick+ATR, open interest, CVD), tracks `activePriceLines`; **must call `removePriceLine` on all active lines before drawing a new AI trade plan**
- **`SimulationEngine.ts`** — tracks entry/TP/SL state, fires browser alerts on hits
- **`App.tsx`** — three-panel layout, exchange definition loader, snapshot polling, chat → analyze → draw plan flow

## Critical Invariants

- CVD accumulation must use `rust_decimal::Decimal`, not `f64`, to prevent drift over high-frequency trade streams.
- WebSocket ingestion stays isolated from Axum handlers via `mpsc`; never perform socket reads inside request paths.
- `AiBroker` must strip ` ```json ` / ` ``` ` fences before calling `serde_json::from_str`.
- `ChartManager` must remove all `activePriceLines` before rendering a new trade plan.
- The open-interest pane currently uses quote-volume as a proxy; a dedicated OI endpoint is not yet integrated.
- Symbol switching is handled at runtime via `POST /api/symbol`, which reseeds klines and sends a value over a `watch` channel causing the WebSocket task to reconnect.
