# Indicators Design — EMAs & Bollinger Bands

**Date:** 2026-05-24  
**Scope:** Frontend-only indicator overlay on the candlestick price pane  
**Status:** Approved

---

## Goal

Add configurable EMA lines and Bollinger Bands to the existing candlestick chart pane. Periods and visibility are controlled through the existing Settings modal. No backend changes required.

---

## Architecture

All indicator logic lives in `frontend/src/ChartManager.ts`. App.tsx stores config state and passes it to ChartManager. Computation runs in-browser from the `candles[]` array already present in every snapshot.

```
Settings modal (App.tsx)
  └─ saves to localStorage('columba_indicators')
  └─ calls chartManagerRef.current.setIndicatorConfig(config)
        │
        ▼
ChartManager.setIndicatorConfig(config)
  ├─ destroy existing EMA/BB series
  ├─ create new LineSeries for each EMA period + BB bands
  └─ if lastSnapshot exists → re-render immediately

ChartManager.updateSnapshot(snapshot)
  ├─ stores snapshot in this.lastSnapshot
  ├─ (existing candle/ATR/OI/CVD update)
  └─ if emaSeries.size > 0 or bbUpper exists → compute + setData
```

---

## Data Structures

### IndicatorConfig (new, exported from ChartManager.ts)

```typescript
export interface IndicatorConfig {
  emas: number[];       // EMA periods, e.g. [9, 20, 50]
  bb: {
    enabled: boolean;
    period: number;     // default 20
    stddev: number;     // default 2.0
  };
}

export const DEFAULT_INDICATOR_CONFIG: IndicatorConfig = {
  emas: [],
  bb: { enabled: false, period: 20, stddev: 2.0 },
};
// Exported so App.tsx can use it as the fallback without duplicating the literal.
```

---

## ChartManager Changes

### New private fields

```typescript
private emaSeries: Map<number, ISeriesApi<'Line'>> = new Map();
private bbUpperSeries: ISeriesApi<'Line'> | null = null;
private bbMiddleSeries: ISeriesApi<'Line'> | null = null;
private bbLowerSeries: ISeriesApi<'Line'> | null = null;
private lastSnapshot: MarketSnapshot | null = null;
private indicatorConfig: IndicatorConfig = DEFAULT_INDICATOR_CONFIG;
```

### New public method: setIndicatorConfig

```typescript
public setIndicatorConfig(config: IndicatorConfig): void
```

1. Remove all existing EMA series from `priceChart` and clear `emaSeries` map.
2. Remove BB series if they exist.
3. Validate `config.emas`: filter out NaN values (from bad CSV input).
4. Create one `LineSeries` on `priceChart` per EMA period, using the color map below.
5. If `config.bb.enabled`, create three `LineSeries` (upper, middle, lower) on `priceChart` in `#00d2d3`.
6. Store config in `this.indicatorConfig`.
7. Update the price pane legend chips.
8. If `this.lastSnapshot !== null`, call the private render helpers immediately.

### New private methods

```typescript
private computeEMA(closes: number[], period: number): (number | null)[]
```
- Seed: SMA of first `period` values.
- Then iterate: `ema[i] = close[i] * k + ema[i-1] * (1-k)` where `k = 2 / (period + 1)`.
- Return `null` for indices `< period - 1` (series not yet warmed up).

```typescript
private computeBB(
  closes: number[],
  period: number,
  std: number
): { upper: number | null; middle: number | null; lower: number | null }[]
```
- For each index `i < period - 1`: return `{ upper: null, middle: null, lower: null }`.
- Otherwise: slice last `period` closes, compute SMA and population σ, return `SMA ± std * σ`.

```typescript
private renderIndicators(snapshot: MarketSnapshot): void
```
- Called from `updateSnapshot` and from `setIndicatorConfig` (when snapshot exists).
- Extracts `closes[]` from `snapshot.candles`.
- For each entry in `emaSeries`: compute EMA, call `series.setData(...)`.
- If BB series exist: compute BB, feed upper/middle/lower series.

### updateSnapshot changes

- Add `this.lastSnapshot = snapshot;` at the top of the method.
- At the end, call `this.renderIndicators(snapshot)`.

### destroy() changes

- Iterate `this.emaSeries.values()` → `priceChart.removeSeries(series)` for each.
- Remove BB series if non-null.

### Legend update

`addPaneLegend` is unchanged. After calling it, a second `<div>` (`private indicatorLegendEl`) is appended to the price pane legend row — positioned to the right of the static chips. `setIndicatorConfig` clears and rebuilds only this second div. The static "Candlestick" and "ATR-14" chips (`atrChipEl`) are unaffected.

---

## Color Map

| Period | Color |
|--------|-------|
| 9 | `#ff9f43` (orange) |
| 20 | `#54a0ff` (blue) |
| 50 | `#5f27cd` (violet) |
| 200+ | `#ff6b6b` (red) |
| other | `#a0aec0` (gray) |
| BB all bands | `#00d2d3` (teal) |

BB middle band uses `lineWidth: 1`; upper/lower use `lineWidth: 1` with `lineStyle: 1` (dashed).

---

## App.tsx Changes

### New state

```typescript
const [indicatorConfig, setIndicatorConfig] = useState<IndicatorConfig>(
  () => {
    try {
      return JSON.parse(localStorage.getItem('columba_indicators') ?? 'null')
        ?? DEFAULT_INDICATOR_CONFIG;
    } catch {
      return DEFAULT_INDICATOR_CONFIG;
    }
  }
);
```

### Chart initialization useEffect

After `chartManagerRef.current = new ChartManager(...)`, immediately call:
```typescript
chartManagerRef.current.setIndicatorConfig(indicatorConfig);
```

### saveSettings additions

```typescript
localStorage.setItem('columba_indicators', JSON.stringify(indicatorConfig));
chartManagerRef.current?.setIndicatorConfig(indicatorConfig);
```

### Settings modal — new Indicators section

Placed below the existing Ollama URL input, above the hint text:

```
── Indicators ─────────────────────────
EMA periods    [9, 20, 50]   (comma-separated)
Bollinger Bands [✓ enabled]  Period [20]  Std dev [2]
```

- EMA periods: `<input type="text">`, parsed on save with `value.split(',').map(s => parseInt(s.trim())).filter(n => !isNaN(n) && n > 0)`.
- BB enabled: `<input type="checkbox">`.
- BB period: `<input type="number" min="2" max="500">`.
- BB stddev: `<input type="number" min="0.1" max="5" step="0.1">`.

---

## Edge Cases

| Case | Behavior |
|------|----------|
| `candles.length < ema_period` | `computeEMA` returns all nulls; series renders empty, no crash |
| Invalid CSV (e.g. `"abc,9"`) | `parseInt` + `isNaN` filter removes bad values before series creation |
| BB disabled | `bbUpperSeries === null`; `renderIndicators` skips BB block entirely |
| Symbol / interval switch | `updateSnapshot` called with fresh candles; `renderIndicators` recomputes automatically |
| `setIndicatorConfig([])` | All EMA/BB series removed; legend chips cleared |
| `destroy()` called | All indicator series removed cleanly before chart teardown |

---

## Files Changed

| File | Change |
|------|--------|
| `frontend/src/ChartManager.ts` | Add `IndicatorConfig`, `setIndicatorConfig`, `computeEMA`, `computeBB`, `renderIndicators`, dynamic legend, series cleanup in `destroy()` |
| `frontend/src/App.tsx` | Add indicator state, Settings modal inputs, call `setIndicatorConfig` on mount and save |

No new files. No backend changes.
