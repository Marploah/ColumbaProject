# Indicators (EMAs + Bollinger Bands) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add configurable EMA lines and Bollinger Bands to the candlestick price pane, controlled from the Settings modal and persisted in localStorage.

**Architecture:** All indicator series live inside `ChartManager` on `priceChart`. Computation (EMA, BBands) runs in TypeScript from the `candles[]` array of each snapshot. `App.tsx` holds config state and passes it to `ChartManager.setIndicatorConfig()` on mount and on settings save.

**Tech Stack:** TypeScript, lightweight-charts v5, React 18, Vite (no test runner — verification via `tsc --noEmit` + visual browser checks)

---

## File Map

| File | Change |
|------|--------|
| `frontend/src/ChartManager.ts` | Add `IndicatorConfig`, `DEFAULT_INDICATOR_CONFIG`, private series fields, `computeEMA`, `computeBB`, `emaColor`, `renderIndicators`, `setIndicatorConfig`, `updateIndicatorLegend`; update `addPaneLegend`, `updateSnapshot`, `destroy` |
| `frontend/src/App.tsx` | Import new types, add `indicatorConfig` state, call `setIndicatorConfig` on mount, add Indicators section to Settings modal, update `saveSettings` |

---

## Task 1: IndicatorConfig types + math utilities

**Files:**
- Modify: `frontend/src/ChartManager.ts`

- [ ] **Step 1: Add `LineStyle` to the lightweight-charts import**

In `frontend/src/ChartManager.ts`, replace the existing import block:

```typescript
import {
  CandlestickSeries,
  ColorType,
  createChart,
  IChartApi,
  IPriceLine,
  ISeriesApi,
  LineSeries,
  LineStyle,
  Time,
} from 'lightweight-charts';
```

- [ ] **Step 2: Add `IndicatorConfig` interface and `DEFAULT_INDICATOR_CONFIG` after the existing `TradePlanPayload` interface**

```typescript
export interface IndicatorConfig {
  emas: number[];
  bb: {
    enabled: boolean;
    period: number;
    stddev: number;
  };
}

export const DEFAULT_INDICATOR_CONFIG: IndicatorConfig = {
  emas: [],
  bb: { enabled: false, period: 20, stddev: 2.0 },
};
```

- [ ] **Step 3: Add private fields for indicator series inside the `ChartManager` class, after the existing `private cleanupFns` field**

```typescript
private emaSeries: Map<number, ISeriesApi<'Line'>> = new Map();
private bbUpperSeries: ISeriesApi<'Line'> | null = null;
private bbMiddleSeries: ISeriesApi<'Line'> | null = null;
private bbLowerSeries: ISeriesApi<'Line'> | null = null;
private lastSnapshot: MarketSnapshot | null = null;
private indicatorConfig: IndicatorConfig = DEFAULT_INDICATOR_CONFIG;
private indicatorLegendEl: HTMLElement | null = null;
```

- [ ] **Step 4: Add the `computeEMA` private method inside the `ChartManager` class, after the `toTime` method**

```typescript
private computeEMA(closes: number[], period: number): (number | null)[] {
  if (closes.length < period) return closes.map(() => null);
  const k = 2 / (period + 1);
  const seed = closes.slice(0, period).reduce((a, b) => a + b, 0) / period;
  const result: (number | null)[] = closes.map(() => null);
  result[period - 1] = seed;
  let prev = seed;
  for (let i = period; i < closes.length; i++) {
    prev = closes[i] * k + prev * (1 - k);
    result[i] = prev;
  }
  return result;
}
```

- [ ] **Step 5: Add the `computeBB` private method after `computeEMA`**

```typescript
private computeBB(
  closes: number[],
  period: number,
  std: number,
): { upper: number | null; middle: number | null; lower: number | null }[] {
  return closes.map((_, i) => {
    if (i < period - 1) return { upper: null, middle: null, lower: null };
    const slice = closes.slice(i - period + 1, i + 1);
    const sma = slice.reduce((a, b) => a + b, 0) / period;
    const variance = slice.reduce((a, b) => a + (b - sma) ** 2, 0) / period;
    const sd = Math.sqrt(variance);
    return { upper: sma + std * sd, middle: sma, lower: sma - std * sd };
  });
}
```

- [ ] **Step 6: Add the `emaColor` helper after `computeBB`**

```typescript
private emaColor(period: number): string {
  if (period <= 9) return '#ff9f43';
  if (period <= 20) return '#54a0ff';
  if (period <= 50) return '#5f27cd';
  return '#ff6b6b';
}
```

- [ ] **Step 7: Type-check**

```bash
cd frontend && npx tsc --noEmit
```

Expected: no errors.

- [ ] **Step 8: Commit**

```bash
git add frontend/src/ChartManager.ts
git commit -m "feat: add IndicatorConfig types and EMA/BB math utilities to ChartManager"
```

---

## Task 2: Series management — setIndicatorConfig, renderIndicators, legend

**Files:**
- Modify: `frontend/src/ChartManager.ts`

- [ ] **Step 1: Change `addPaneLegend` to return the legend `HTMLElement`**

Find the `addPaneLegend` method. Change the signature from `): void {` to `): HTMLElement {` and add `return legend;` before the closing brace:

```typescript
private addPaneLegend(
  pane: HTMLElement,
  chips: Array<{ text: string; color: string; live?: boolean }>,
): HTMLElement {
  pane.style.position = 'relative';
  const legend = document.createElement('div');
  legend.style.cssText =
    'position:absolute;top:6px;left:6px;z-index:4;display:flex;gap:5px;pointer-events:none;';

  for (const chip of chips) {
    const el = document.createElement('span');
    el.style.cssText =
      `font-family:monospace;font-size:10px;padding:2px 6px;border-radius:3px;` +
      `background:rgba(16,20,24,0.8);color:${chip.color};border:1px solid ${chip.color}55;letter-spacing:0.02em;`;
    el.textContent = chip.text;
    legend.append(el);
    if (chip.live) this.atrChipEl = el;
  }

  pane.append(legend);
  return legend;
}
```

- [ ] **Step 2: Update the constructor to capture `indicatorLegendEl` from the price pane legend**

In the constructor, the first `addPaneLegend` call becomes:

```typescript
const pricePaneLegend = this.addPaneLegend(pricePane, [
  { text: 'Candlestick', color: '#d7dde5' },
  { text: 'ATR-14', color: '#f6c85f', live: true },
]);
this.indicatorLegendEl = document.createElement('div');
this.indicatorLegendEl.style.cssText = 'display:flex;gap:5px;';
pricePaneLegend.append(this.indicatorLegendEl);
```

The second `addPaneLegend` call (for `indicatorPane`) stays the same but the return value is unused — that is fine.

- [ ] **Step 3: Add the `updateIndicatorLegend` private method after `emaColor`**

```typescript
private updateIndicatorLegend(): void {
  if (!this.indicatorLegendEl) return;
  this.indicatorLegendEl.replaceChildren();

  for (const period of this.indicatorConfig.emas) {
    const color = this.emaColor(period);
    const chip = document.createElement('span');
    chip.style.cssText =
      `font-family:monospace;font-size:10px;padding:2px 6px;border-radius:3px;` +
      `background:rgba(16,20,24,0.8);color:${color};border:1px solid ${color}55;letter-spacing:0.02em;`;
    chip.textContent = `EMA${period}`;
    this.indicatorLegendEl.append(chip);
  }

  if (this.indicatorConfig.bb.enabled) {
    const chip = document.createElement('span');
    chip.style.cssText =
      `font-family:monospace;font-size:10px;padding:2px 6px;border-radius:3px;` +
      `background:rgba(16,20,24,0.8);color:#00d2d3;border:1px solid #00d2d355;letter-spacing:0.02em;`;
    chip.textContent = `BB${this.indicatorConfig.bb.period}`;
    this.indicatorLegendEl.append(chip);
  }
}
```

- [ ] **Step 4: Add the `renderIndicators` private method after `updateIndicatorLegend`**

```typescript
private renderIndicators(snapshot: MarketSnapshot): void {
  const closes = snapshot.candles.map((c) => c.close);
  const times = snapshot.candles.map((c) => this.toTime(c.timestamp));

  for (const [period, series] of this.emaSeries) {
    const values = this.computeEMA(closes, period);
    series.setData(
      values
        .map((v, i) => (v !== null ? { time: times[i], value: v } : null))
        .filter((p): p is { time: Time; value: number } => p !== null),
    );
  }

  if (this.bbUpperSeries && this.bbMiddleSeries && this.bbLowerSeries) {
    const bb = this.computeBB(closes, this.indicatorConfig.bb.period, this.indicatorConfig.bb.stddev);
    const toPoints = (key: 'upper' | 'middle' | 'lower') =>
      bb
        .map((b, i) => (b[key] !== null ? { time: times[i], value: b[key] as number } : null))
        .filter((p): p is { time: Time; value: number } => p !== null);

    this.bbUpperSeries.setData(toPoints('upper'));
    this.bbMiddleSeries.setData(toPoints('middle'));
    this.bbLowerSeries.setData(toPoints('lower'));
  }
}
```

- [ ] **Step 5: Add the `setIndicatorConfig` public method after `takeScreenshot`**

```typescript
public setIndicatorConfig(config: IndicatorConfig): void {
  for (const series of this.emaSeries.values()) {
    this.priceChart.removeSeries(series);
  }
  this.emaSeries.clear();

  if (this.bbUpperSeries) { this.priceChart.removeSeries(this.bbUpperSeries); this.bbUpperSeries = null; }
  if (this.bbMiddleSeries) { this.priceChart.removeSeries(this.bbMiddleSeries); this.bbMiddleSeries = null; }
  if (this.bbLowerSeries) { this.priceChart.removeSeries(this.bbLowerSeries); this.bbLowerSeries = null; }

  this.indicatorConfig = config;

  for (const period of config.emas) {
    const series = this.priceChart.addSeries(LineSeries, {
      color: this.emaColor(period),
      lineWidth: 1,
      priceLineVisible: false,
      lastValueVisible: false,
      crosshairMarkerVisible: false,
    });
    this.emaSeries.set(period, series);
  }

  if (config.bb.enabled) {
    this.bbUpperSeries = this.priceChart.addSeries(LineSeries, {
      color: '#00d2d3',
      lineWidth: 1,
      lineStyle: LineStyle.Dashed,
      priceLineVisible: false,
      lastValueVisible: false,
      crosshairMarkerVisible: false,
    });
    this.bbMiddleSeries = this.priceChart.addSeries(LineSeries, {
      color: '#00d2d3',
      lineWidth: 1,
      priceLineVisible: false,
      lastValueVisible: false,
      crosshairMarkerVisible: false,
    });
    this.bbLowerSeries = this.priceChart.addSeries(LineSeries, {
      color: '#00d2d3',
      lineWidth: 1,
      lineStyle: LineStyle.Dashed,
      priceLineVisible: false,
      lastValueVisible: false,
      crosshairMarkerVisible: false,
    });
  }

  this.updateIndicatorLegend();

  if (this.lastSnapshot) {
    this.renderIndicators(this.lastSnapshot);
  }
}
```

- [ ] **Step 6: Update `updateSnapshot` — add `lastSnapshot` assignment and call `renderIndicators` at the end**

At the top of `updateSnapshot`, add:
```typescript
this.lastSnapshot = snapshot;
```

At the very end of `updateSnapshot` (after the ATR chip update block), add:
```typescript
this.renderIndicators(snapshot);
```

- [ ] **Step 7: Update `destroy()` to clean up indicator series**

In `destroy()`, before `this.priceChart.remove()`, add:

```typescript
for (const series of this.emaSeries.values()) {
  this.priceChart.removeSeries(series);
}
this.emaSeries.clear();
if (this.bbUpperSeries) this.priceChart.removeSeries(this.bbUpperSeries);
if (this.bbMiddleSeries) this.priceChart.removeSeries(this.bbMiddleSeries);
if (this.bbLowerSeries) this.priceChart.removeSeries(this.bbLowerSeries);
```

- [ ] **Step 8: Type-check**

```bash
cd frontend && npx tsc --noEmit
```

Expected: no errors.

- [ ] **Step 9: Commit**

```bash
git add frontend/src/ChartManager.ts
git commit -m "feat: add setIndicatorConfig, renderIndicators, dynamic legend to ChartManager"
```

---

## Task 3: App.tsx — state, mount wiring, Settings modal inputs

**Files:**
- Modify: `frontend/src/App.tsx`

- [ ] **Step 1: Update the import from `ChartManager` to include the new types**

Replace:
```typescript
import { ChartManager, MarketSnapshot, TradePlanPayload } from './ChartManager';
```
With:
```typescript
import { ChartManager, DEFAULT_INDICATOR_CONFIG, IndicatorConfig, MarketSnapshot, TradePlanPayload } from './ChartManager';
```

- [ ] **Step 2: Add `indicatorConfig` state after the `settingsOpen` state**

```typescript
const [indicatorConfig, setIndicatorConfig] = useState<IndicatorConfig>(() => {
  try {
    return (
      (JSON.parse(localStorage.getItem('columba_indicators') ?? 'null') as IndicatorConfig | null) ??
      DEFAULT_INDICATOR_CONFIG
    );
  } catch {
    return DEFAULT_INDICATOR_CONFIG;
  }
});
```

- [ ] **Step 3: Call `setIndicatorConfig` immediately after `ChartManager` construction**

In the `useEffect` that creates the chart (the one with `if (!chartHostRef.current || chartManagerRef.current) return;`), after `chartManagerRef.current = new ChartManager(chartHostRef.current);`, add:

```typescript
chartManagerRef.current.setIndicatorConfig(indicatorConfig);
```

- [ ] **Step 4: Update `saveSettings` to persist and apply indicator config**

Replace the existing `saveSettings` function:

```typescript
function saveSettings() {
  localStorage.setItem('columba_openai_api_key', openaiApiKey);
  localStorage.setItem('columba_ollama_url', ollamaUrl);
  localStorage.setItem('columba_indicators', JSON.stringify(indicatorConfig));
  chartManagerRef.current?.setIndicatorConfig(indicatorConfig);
  setSettingsOpen(false);
}
```

- [ ] **Step 5: Add the Indicators section to the Settings modal**

Inside the modal `<div>`, after the Ollama URL `<label>` and before `<p className="settings-hint">`, insert:

```tsx
<p className="settings-section-label">Indicators</p>

<label className="select-label">
  EMA periods (comma-separated)
  <input
    type="text"
    value={indicatorConfig.emas.join(', ')}
    onChange={(e) => {
      const emas = e.target.value
        .split(',')
        .map((s) => parseInt(s.trim(), 10))
        .filter((n) => !Number.isNaN(n) && n > 0);
      setIndicatorConfig((prev) => ({ ...prev, emas }));
    }}
    placeholder="9, 20, 50"
    autoComplete="off"
  />
</label>

<label className="select-label">
  Bollinger Bands
  <input
    type="checkbox"
    checked={indicatorConfig.bb.enabled}
    onChange={(e) =>
      setIndicatorConfig((prev) => ({
        ...prev,
        bb: { ...prev.bb, enabled: e.target.checked },
      }))
    }
  />
</label>

{indicatorConfig.bb.enabled && (
  <div style={{ display: 'flex', gap: '8px' }}>
    <label className="select-label" style={{ flex: 1 }}>
      Period
      <input
        type="number"
        min={2}
        max={500}
        value={indicatorConfig.bb.period}
        onChange={(e) =>
          setIndicatorConfig((prev) => ({
            ...prev,
            bb: { ...prev.bb, period: Math.max(2, parseInt(e.target.value, 10) || 20) },
          }))
        }
      />
    </label>
    <label className="select-label" style={{ flex: 1 }}>
      Std dev
      <input
        type="number"
        min={0.1}
        max={5}
        step={0.1}
        value={indicatorConfig.bb.stddev}
        onChange={(e) =>
          setIndicatorConfig((prev) => ({
            ...prev,
            bb: { ...prev.bb, stddev: Math.max(0.1, parseFloat(e.target.value) || 2) },
          }))
        }
      />
    </label>
  </div>
)}
```

- [ ] **Step 6: Type-check**

```bash
cd frontend && npx tsc --noEmit
```

Expected: no errors.

- [ ] **Step 7: Commit**

```bash
git add frontend/src/App.tsx
git commit -m "feat: add indicator config state and Settings modal inputs to App.tsx"
```

---

## Task 4: Visual verification

**Files:** none — verification only

- [ ] **Step 1: Start the dev server**

```bash
cd frontend && npm run dev
```

Open `http://127.0.0.1:5173` in a browser. Backend must be running (`cd backend && cargo run`).

- [ ] **Step 2: Verify default state**

Chart loads with no EMA or BB lines. Price pane legend shows only `[Candlestick]` and `[ATR-14 ...]` chips.

- [ ] **Step 3: Enable EMA 9, 20, 50**

Open Settings (gear icon). In "EMA periods", type `9, 20, 50`. Click Save.

Expected:
- Three colored lines appear on the candlestick pane.
- Legend chips `[EMA9]` (orange), `[EMA20]` (blue), `[EMA50]` (violet) appear.

- [ ] **Step 4: Enable Bollinger Bands**

Open Settings. Check "Bollinger Bands". Leave period=20, stddev=2. Click Save.

Expected:
- Three teal lines appear (upper dashed, middle solid, lower dashed).
- `[BB20]` chip appears in the legend.

- [ ] **Step 5: Switch symbol**

Click a different symbol in the left panel.

Expected: EMA and BB lines recompute immediately on the new symbol's candles without manual intervention.

- [ ] **Step 6: Reload and verify persistence**

Reload the page (`F5`).

Expected: EMA and BB lines appear immediately on load without opening Settings — config was restored from localStorage.

- [ ] **Step 7: Disable all indicators**

Open Settings. Clear the EMA field. Uncheck Bollinger Bands. Click Save.

Expected: all indicator lines and legend chips disappear. Chart returns to baseline state.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat: indicators (EMA + Bollinger Bands) complete"
```
