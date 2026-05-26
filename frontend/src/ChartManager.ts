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

export interface CandlePayload {
  timestamp: number;
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
  cvd: string | number;
  open_interest?: number | null;
}

export interface MarketSnapshot {
  symbol: string;
  last_price: number;
  candles: CandlePayload[];
  atr_14?: number | null;
  volatility_upper_limit?: number | null;
  volatility_lower_limit?: number | null;
}

export interface TradePlanPayload {
  entry_price: number;
  take_profit: number;
  stop_loss: number;
}

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

const BASE_CHART_OPTIONS = {
  autoSize: true,
  layout: {
    background: { type: ColorType.Solid, color: '#101418' },
    textColor: '#d7dde5',
  },
  grid: {
    vertLines: { color: '#1c232b' },
    horzLines: { color: '#1c232b' },
  },
  rightPriceScale: {
    borderColor: '#2a323d',
    scaleMargins: { top: 0.05, bottom: 0.05 },
  },
  timeScale: {
    borderColor: '#2a323d',
    timeVisible: true,
    secondsVisible: false,
  },
  crosshair: { mode: 1 },
} as const;

export class ChartManager {
  private priceChart: IChartApi;
  private indicatorChart: IChartApi;
  private candleSeries: ISeriesApi<'Candlestick'>;
  private atrUpperSeries: ISeriesApi<'Line'>;
  private atrLowerSeries: ISeriesApi<'Line'>;
  private openInterestSeries: ISeriesApi<'Line'>;
  private cvdSeries: ISeriesApi<'Line'>;
  private activePriceLines: IPriceLine[] = [];
  private cleanupFns: Array<() => void> = [];
  private atrChipEl: HTMLElement | null = null;
  private hasInitialized = false;
  private syncingTimeScale = false;
  private emaSeries: Map<number, ISeriesApi<'Line'>> = new Map();
  private bbUpperSeries: ISeriesApi<'Line'> | null = null;
  private bbMiddleSeries: ISeriesApi<'Line'> | null = null;
  private bbLowerSeries: ISeriesApi<'Line'> | null = null;
  private lastSnapshot: MarketSnapshot | null = null;
  private indicatorConfig: IndicatorConfig = DEFAULT_INDICATOR_CONFIG;
  private indicatorLegendEl: HTMLElement | null = null;
  private pendingSymbol: string | null = null;

  public constructor(private container: HTMLElement) {
    container.replaceChildren();

    const pricePane = this.makePane(3);
    const sep1 = this.makeSeparator();
    const indicatorPane = this.makePane(1);

    container.append(pricePane, sep1, indicatorPane);

    const pricePaneLegend = this.addPaneLegend(pricePane, [
      { text: 'Candlestick', color: '#d7dde5' },
      { text: 'ATR-14', color: '#f6c85f', live: true },
    ]);
    this.indicatorLegendEl = document.createElement('div');
    this.indicatorLegendEl.style.cssText = 'display:flex;gap:5px;';
    pricePaneLegend.append(this.indicatorLegendEl);
    this.addPaneLegend(indicatorPane, [
      { text: 'OI', color: '#4d96ff' },
      { text: 'CVD', color: '#8f6fff' },
    ]);

    this.priceChart = createChart(pricePane, BASE_CHART_OPTIONS);
    this.indicatorChart = createChart(indicatorPane, {
      ...BASE_CHART_OPTIONS,
      leftPriceScale: { visible: true, borderColor: '#2a323d', scaleMargins: { top: 0.05, bottom: 0.05 } },
      rightPriceScale: { ...BASE_CHART_OPTIONS.rightPriceScale, visible: true },
    });

    this.candleSeries = this.priceChart.addSeries(CandlestickSeries, {
      upColor: '#20bf75',
      downColor: '#ef476f',
      wickUpColor: '#20bf75',
      wickDownColor: '#ef476f',
      borderVisible: false,
    });

    this.atrUpperSeries = this.priceChart.addSeries(LineSeries, {
      color: '#f6c85f',
      lineWidth: 1,
      priceLineVisible: false,
    });

    this.atrLowerSeries = this.priceChart.addSeries(LineSeries, {
      color: '#f6c85f',
      lineWidth: 1,
      priceLineVisible: false,
    });

    this.openInterestSeries = this.indicatorChart.addSeries(LineSeries, {
      color: '#4d96ff',
      lineWidth: 2,
      priceLineVisible: false,
      priceScaleId: 'left',
      priceFormat: { type: 'volume' },
    });

    this.cvdSeries = this.indicatorChart.addSeries(LineSeries, {
      color: '#8f6fff',
      lineWidth: 2,
      priceLineVisible: false,
      priceScaleId: 'right',
      priceFormat: { type: 'volume' },
    });

    this.bindDrag(sep1, pricePane, indicatorPane);
    this.bindTimeScaleSync();
  }

  public updateSnapshot(snapshot: MarketSnapshot): void {
    // Drop stale snapshots from the previous symbol to prevent the wrong data
    // from initializing the chart before the new symbol's data arrives.
    if (this.pendingSymbol && snapshot.symbol !== this.pendingSymbol) {
      return;
    }
    this.pendingSymbol = null;
    const isFirstLoad = !this.hasInitialized;
    this.lastSnapshot = snapshot;
    const last = snapshot.candles.at(-1);

    if (isFirstLoad) {
      this.candleSeries.setData(
        snapshot.candles.map((c) => ({
          time: this.toTime(c.timestamp),
          open: c.open,
          high: c.high,
          low: c.low,
          close: c.close,
        })),
      );

      this.atrUpperSeries.setData(
        snapshot.candles
          .filter(() => Number.isFinite(snapshot.volatility_upper_limit ?? NaN))
          .map((c) => ({ time: this.toTime(c.timestamp), value: snapshot.volatility_upper_limit as number })),
      );
      this.atrLowerSeries.setData(
        snapshot.candles
          .filter(() => Number.isFinite(snapshot.volatility_lower_limit ?? NaN))
          .map((c) => ({ time: this.toTime(c.timestamp), value: snapshot.volatility_lower_limit as number })),
      );

      this.openInterestSeries.setData(
        snapshot.candles
          .filter((c) => Number.isFinite(c.open_interest ?? NaN))
          .map((c) => ({ time: this.toTime(c.timestamp), value: c.open_interest as number })),
      );

      this.cvdSeries.setData(
        snapshot.candles.map((c) => {
          const value = typeof c.cvd === 'string' ? Number.parseFloat(c.cvd) : c.cvd;
          return { time: this.toTime(c.timestamp), value };
        }),
      );

      this.priceChart.timeScale().fitContent();
      this.indicatorChart.timeScale().fitContent();
      this.hasInitialized = true;
    } else if (last) {
      // Live update: only touch the last candle — avoids re-rendering 1500 bars per tick.
      this.candleSeries.update({
        time: this.toTime(last.timestamp),
        open: last.open,
        high: last.high,
        low: last.low,
        close: last.close,
      });

      const cvdValue = typeof last.cvd === 'string' ? Number.parseFloat(last.cvd) : last.cvd;
      this.cvdSeries.update({ time: this.toTime(last.timestamp), value: cvdValue });

      if (last.open_interest != null && Number.isFinite(last.open_interest)) {
        this.openInterestSeries.update({ time: this.toTime(last.timestamp), value: last.open_interest });
      }
    }

    if (this.atrChipEl && snapshot.atr_14 != null) {
      this.atrChipEl.textContent = `ATR-14  ${snapshot.atr_14.toFixed(2)}`;
    }

    this.renderIndicators(snapshot, !isFirstLoad);
  }

  public resetChartData(): void {
    this.hasInitialized = false;
    this.candleSeries.setData([]);
    this.atrUpperSeries.setData([]);
    this.atrLowerSeries.setData([]);
    this.openInterestSeries.setData([]);
    this.cvdSeries.setData([]);
    for (const series of this.emaSeries.values()) series.setData([]);
    if (this.bbUpperSeries) this.bbUpperSeries.setData([]);
    if (this.bbMiddleSeries) this.bbMiddleSeries.setData([]);
    if (this.bbLowerSeries) this.bbLowerSeries.setData([]);
  }

  public setPendingSymbol(symbol: string): void {
    this.pendingSymbol = symbol.toUpperCase();
  }

  public clearPreviousAiDrawings(): void {
    for (const line of this.activePriceLines) {
      this.candleSeries.removePriceLine(line);
    }
    this.activePriceLines = [];
  }

  public drawTradePlan(plan: TradePlanPayload): void {
    this.clearPreviousAiDrawings();
    this.activePriceLines.push(
      this.candleSeries.createPriceLine({ price: plan.entry_price, color: '#ffffff', lineWidth: 2, lineStyle: 2, axisLabelVisible: true, title: 'Entry' }),
      this.candleSeries.createPriceLine({ price: plan.take_profit, color: '#20bf75', lineWidth: 2, lineStyle: 0, axisLabelVisible: true, title: 'TP' }),
      this.candleSeries.createPriceLine({ price: plan.stop_loss, color: '#ef476f', lineWidth: 2, lineStyle: 0, axisLabelVisible: true, title: 'SL' }),
    );
  }

  public takeScreenshot(): HTMLCanvasElement {
    return this.priceChart.takeScreenshot();
  }

  public setIndicatorConfig(config: IndicatorConfig): void {
    for (const series of this.emaSeries.values()) {
      this.priceChart.removeSeries(series);
    }
    this.emaSeries.clear();

    if (this.bbUpperSeries) { this.priceChart.removeSeries(this.bbUpperSeries); this.bbUpperSeries = null; }
    if (this.bbMiddleSeries) { this.priceChart.removeSeries(this.bbMiddleSeries); this.bbMiddleSeries = null; }
    if (this.bbLowerSeries) { this.priceChart.removeSeries(this.bbLowerSeries); this.bbLowerSeries = null; }

    this.indicatorConfig = config;

    for (const period of [...new Set(config.emas)]) {
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

  public destroy(): void {
    this.clearPreviousAiDrawings();
    for (const fn of this.cleanupFns) fn();
    for (const series of this.emaSeries.values()) {
      this.priceChart.removeSeries(series);
    }
    this.emaSeries.clear();
    if (this.bbUpperSeries) this.priceChart.removeSeries(this.bbUpperSeries);
    if (this.bbMiddleSeries) this.priceChart.removeSeries(this.bbMiddleSeries);
    if (this.bbLowerSeries) this.priceChart.removeSeries(this.bbLowerSeries);
    this.priceChart.remove();
    this.indicatorChart.remove();
    this.container.replaceChildren();
  }

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

  private makePane(flex: number): HTMLElement {
    const el = document.createElement('div');
    el.style.cssText = `flex:${flex};min-height:60px;overflow:hidden;`;
    return el;
  }

  private makeSeparator(): HTMLElement {
    const el = document.createElement('div');
    el.style.cssText = 'height:4px;background:#2a323d;cursor:ns-resize;flex-shrink:0;';
    el.addEventListener('mouseenter', () => { el.style.background = '#516070'; });
    el.addEventListener('mouseleave', () => { if (!el.dataset.drag) el.style.background = '#2a323d'; });
    return el;
  }

  private bindDrag(sep: HTMLElement, above: HTMLElement, below: HTMLElement): void {
    let startY = 0;
    let startAbove = 0;
    let startBelow = 0;

    const onDown = (e: MouseEvent) => {
      startY = e.clientY;
      startAbove = above.getBoundingClientRect().height;
      startBelow = below.getBoundingClientRect().height;
      above.style.flex = 'none';
      below.style.flex = 'none';
      sep.dataset.drag = '1';
      document.body.style.cursor = 'ns-resize';
      document.body.style.userSelect = 'none';
    };

    const onMove = (e: MouseEvent) => {
      if (!sep.dataset.drag) return;
      const delta = e.clientY - startY;
      above.style.height = `${Math.max(60, startAbove + delta)}px`;
      below.style.height = `${Math.max(60, startBelow - delta)}px`;
    };

    const onUp = () => {
      if (!sep.dataset.drag) return;
      delete sep.dataset.drag;
      sep.style.background = '#2a323d';
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
    };

    sep.addEventListener('mousedown', onDown);
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);

    this.cleanupFns.push(() => {
      sep.removeEventListener('mousedown', onDown);
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    });
  }

  private bindTimeScaleSync(): void {
    const priceTs = this.priceChart.timeScale();
    const indicatorTs = this.indicatorChart.timeScale();

    const onPriceRange = (range: { from: number; to: number } | null) => {
      if (this.syncingTimeScale || range === null) return;
      this.syncingTimeScale = true;
      indicatorTs.setVisibleLogicalRange(range);
      this.syncingTimeScale = false;
    };

    const onIndicatorRange = (range: { from: number; to: number } | null) => {
      if (this.syncingTimeScale || range === null) return;
      this.syncingTimeScale = true;
      priceTs.setVisibleLogicalRange(range);
      this.syncingTimeScale = false;
    };

    priceTs.subscribeVisibleLogicalRangeChange(onPriceRange);
    indicatorTs.subscribeVisibleLogicalRangeChange(onIndicatorRange);

    this.cleanupFns.push(
      () => priceTs.unsubscribeVisibleLogicalRangeChange(onPriceRange),
      () => indicatorTs.unsubscribeVisibleLogicalRangeChange(onIndicatorRange),
    );
  }

  private toTime(timestamp: number): Time {
    return Math.floor(timestamp / 1000) as Time;
  }

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

  private emaColor(period: number): string {
    if (period <= 9) return '#ff9f43';
    if (period <= 20) return '#54a0ff';
    if (period <= 50) return '#5f27cd';
    return '#ff6b6b';
  }

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

  private renderIndicators(snapshot: MarketSnapshot, liveUpdate = false): void {
    const closes = snapshot.candles.map((c) => c.close);
    const times = snapshot.candles.map((c) => this.toTime(c.timestamp));
    const lastIdx = closes.length - 1;

    for (const [period, series] of this.emaSeries) {
      const values = this.computeEMA(closes, period);
      if (liveUpdate) {
        const lastVal = values[lastIdx];
        if (lastVal !== null) series.update({ time: times[lastIdx], value: lastVal });
      } else {
        series.setData(
          values
            .map((v, i) => (v !== null ? { time: times[i], value: v } : null))
            .filter((p): p is { time: Time; value: number } => p !== null),
        );
      }
    }

    if (this.bbUpperSeries && this.bbMiddleSeries && this.bbLowerSeries) {
      const bb = this.computeBB(closes, this.indicatorConfig.bb.period, this.indicatorConfig.bb.stddev);
      if (liveUpdate) {
        const lastBb = bb[lastIdx];
        if (lastBb.upper !== null) this.bbUpperSeries.update({ time: times[lastIdx], value: lastBb.upper });
        if (lastBb.middle !== null) this.bbMiddleSeries.update({ time: times[lastIdx], value: lastBb.middle });
        if (lastBb.lower !== null) this.bbLowerSeries.update({ time: times[lastIdx], value: lastBb.lower });
      } else {
        const toPoints = (key: 'upper' | 'middle' | 'lower') =>
          bb
            .map((b, i) => (b[key] !== null ? { time: times[i], value: b[key] as number } : null))
            .filter((p): p is { time: Time; value: number } => p !== null);
        this.bbUpperSeries.setData(toPoints('upper'));
        this.bbMiddleSeries.setData(toPoints('middle'));
        this.bbLowerSeries.setData(toPoints('lower'));
      }
    }
  }
}
