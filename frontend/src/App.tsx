import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { BookOpen, Camera, Cpu, Download, Send, SlidersHorizontal } from 'lucide-react';
import { FormEvent, useEffect, useMemo, useRef, useState } from 'react';
import tutorialMarkdown from './content/tutorials.md?raw';

const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

interface ModelEntry {
  name: string;
  url: string;
  size_mb: number;
  present: boolean;
}
interface SetupStatus {
  tier: string;
  ram_gb: number;
  vram_gb: number | null;
  physical_cores: number;
  models: ModelEntry[];
  ready: boolean;
}
interface DownloadProgress {
  name: string;
  downloaded_bytes: number;
  total_bytes: number;
}
import { ChartManager, DEFAULT_INDICATOR_CONFIG, IndicatorConfig, MarketSnapshot, TradePlanPayload } from './ChartManager';

function fmtUsd(v: number): string {
  if (v >= 1_000_000) return `$${(v / 1_000_000).toFixed(1)}M`;
  if (v >= 1_000) return `$${(v / 1_000).toFixed(0)}K`;
  return `$${v.toFixed(0)}`;
}

function regimeClass(regime: string): string {
  switch (regime) {
    case 'Compression': return 'regime-compression';
    case 'Normal':      return 'regime-normal';
    case 'Elevated':    return 'regime-elevated';
    case 'Extreme':     return 'regime-extreme';
    default:            return 'regime-unknown';
  }
}

function fgClass(classification: string): string {
  const key = classification.toLowerCase().replace(/ /g, '-');
  return `fg-${key}`;
}

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}

function renderInlineMarkdown(text: string): string {
  return escapeHtml(text)
    .replace(/`([^`]+)`/g, '<code>$1</code>')
    .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
}

function markdownToHtml(markdown: string): string {
  const lines = markdown.replace(/\r\n/g, '\n').trim().split('\n');
  const blocks: string[] = [];
  let paragraph: string[] = [];
  let listType: 'ul' | 'ol' | null = null;

  const flushParagraph = () => {
    if (!paragraph.length) return;
    blocks.push(`<p>${renderInlineMarkdown(paragraph.join(' ').trim())}</p>`);
    paragraph = [];
  };

  const closeList = () => {
    if (!listType) return;
    blocks.push(`</${listType}>`);
    listType = null;
  };

  for (const line of lines) {
    const trimmed = line.trim();

    if (!trimmed) {
      flushParagraph();
      closeList();
      continue;
    }

    if (trimmed.startsWith('### ')) {
      flushParagraph();
      closeList();
      blocks.push(`<h3>${renderInlineMarkdown(trimmed.slice(4))}</h3>`);
      continue;
    }

    if (trimmed.startsWith('## ')) {
      flushParagraph();
      closeList();
      blocks.push(`<h2>${renderInlineMarkdown(trimmed.slice(3))}</h2>`);
      continue;
    }

    if (trimmed.startsWith('# ')) {
      flushParagraph();
      closeList();
      blocks.push(`<h1>${renderInlineMarkdown(trimmed.slice(2))}</h1>`);
      continue;
    }

    if (trimmed.startsWith('- ')) {
      flushParagraph();
      if (listType !== 'ul') {
        closeList();
        blocks.push('<ul>');
        listType = 'ul';
      }
      blocks.push(`<li>${renderInlineMarkdown(trimmed.slice(2))}</li>`);
      continue;
    }

    const orderedMatch = trimmed.match(/^\d+\.\s+(.*)$/);
    if (orderedMatch) {
      flushParagraph();
      if (listType !== 'ol') {
        closeList();
        blocks.push('<ol>');
        listType = 'ol';
      }
      blocks.push(`<li>${renderInlineMarkdown(orderedMatch[1])}</li>`);
      continue;
    }

    paragraph.push(trimmed);
  }

  flushParagraph();
  closeList();

  return blocks.join('');
}
import { SimulationEngine } from './SimulationEngine';

interface ExchangeSymbol {
  symbol: string;
  contractType: string;
  status: string;
  quoteAsset: string;
}

interface ChatMessage {
  role: 'system' | 'user' | 'assistant';
  content: string;
}

const apiBase = (import.meta.env.VITE_API_BASE as string | undefined) ?? 'http://127.0.0.1:8080';
const wsBase = apiBase.replace(/^http/, 'ws');

function formatTradePlan(plan: TradePlanPayload & { thesis?: string }): string {
  const fmt = (n: number) => n.toLocaleString(undefined, { maximumFractionDigits: 4 });
  const levels = `Entry ${fmt(plan.entry_price)} · TP ${fmt(plan.take_profit)} · SL ${fmt(plan.stop_loss)}`;
  return plan.thesis ? `${plan.thesis}\n\n${levels}` : levels;
}

const systemPrompt: ChatMessage = {
  role: 'system',
  content: `You are a crypto futures execution analyst for Binance perpetual futures.

You will receive a structured market brief followed by a user request. Analyze each signal before deciding direction:

- VWAP: price above VWAP = bullish bias; below = bearish bias. Use as entry reference.
- ATR-14 band: suggested TP/SL should not exceed 1.5× ATR from entry unless confluence is strong.
- CVD slope: positive = net buying pressure; negative = net selling. Confirms or contradicts price action.
- OI: if marked as PROXY DATA, do not use OI for directional confirmation — treat it as unreliable.
- Funding rate: above 0.1% favors shorts (longs are crowded); below -0.1% favors longs (shorts are crowded).
- Liquidity walls: bid wall = support / stop-hunt magnet below; ask wall = resistance / stop-hunt magnet above.
- MTF bias: when 1h and 4h disagree with 5m, prefer the higher timeframe for direction, 5m for entry timing.
- RSI divergence: treat as a reversal warning, not a standalone signal.
- If signals conflict, note the conflict in thesis and widen stop-loss to reflect uncertainty.

Return ONLY a JSON object with: entry_price, take_profit, stop_loss, thesis.`,
};

export default function App() {
  const chartHostRef = useRef<HTMLDivElement | null>(null);
  const chartManagerRef = useRef<ChartManager | null>(null);
  const simulationRef = useRef(new SimulationEngine());

  const [symbols, setSymbols] = useState<ExchangeSymbol[]>([]);
  const [selectedSymbol, setSelectedSymbol] = useState('BTCUSDT');
  const [snapshot, setSnapshot] = useState<MarketSnapshot | null>(null);
  const [modelMode, setModelMode] = useState('Auto');
  const [prompt, setPrompt] = useState('');
  const [messages, setMessages] = useState<ChatMessage[]>([
    systemPrompt,
    {
      role: 'assistant',
      content: 'Market context is connected. Ask for an execution plan.',
    },
  ]);
  const [selectedInterval, setSelectedInterval] = useState('1m');
  const [isAnalyzing, setIsAnalyzing] = useState(false);
  const [toasts, setToasts] = useState<{ id: number; text: string }[]>([]);
  const [setupStatus, setSetupStatus] = useState<SetupStatus | null>(null);
  const [downloadingModels, setDownloadingModels] = useState<Record<string, DownloadProgress>>({});
  const [streamingThesis, setStreamingThesis] = useState<{ full: string; displayed: string } | null>(null);
  const typewriterRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const toastIdRef = useRef(0);
  const tradeLogIdRef = useRef<number | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [rightPanelTab, setRightPanelTab] = useState<'chat' | 'tutorials'>('chat');
  const [positionSizePct, setPositionSizePct] = useState(1.0);
  const [leverage, setLeverage] = useState(1);
  const [llamaServerUrl, setLlamaServerUrl] = useState(
    () => localStorage.getItem('columba_llama_server_url') ?? 'http://127.0.0.1:8081/v1',
  );
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
  const indicatorConfigRef = useRef(indicatorConfig);
  const tutorialHtml = useMemo(() => markdownToHtml(tutorialMarkdown), []);

  const addToast = useRef((text: string) => {
    const id = ++toastIdRef.current;
    setToasts((prev) => [...prev, { id, text }]);
    setTimeout(() => setToasts((prev) => prev.filter((t) => t.id !== id)), 4000);
  }).current;

  simulationRef.current.onAlert = addToast;
  // Outcome persistence is handled server-side by the trade monitor task.
  // onOutcome is intentionally not wired to avoid double-writing.
  const [emaRawInput, setEmaRawInput] = useState(indicatorConfig.emas.join(', '));

  // Check setup status in Tauri mode only.
  useEffect(() => {
    if (!isTauri) return;
    invoke<SetupStatus>('get_setup_status')
      .then(setSetupStatus)
      .catch(() => {/* ignore — stay in main app */});
  }, []);

  // Listen for download progress events when in Tauri mode.
  useEffect(() => {
    if (!isTauri) return;
    const unlisten = listen<DownloadProgress>('download_progress', (event) => {
      setDownloadingModels((prev) => ({ ...prev, [event.payload.name]: event.payload }));
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  // Typewriter: drip characters from streamingThesis.full into displayed.
  // Effect only restarts when a new thesis arrives (full string changes).
  useEffect(() => {
    if (!streamingThesis) return;
    const id = setInterval(() => {
      setStreamingThesis((prev) => {
        if (!prev) return null;
        const next = prev.full.slice(0, prev.displayed.length + 4);
        if (next.length >= prev.full.length) {
          clearInterval(id);
          return null;
        }
        return { full: prev.full, displayed: next };
      });
    }, 16);
    typewriterRef.current = id;
    return () => clearInterval(id);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [streamingThesis?.full]);

  useEffect(() => {
    indicatorConfigRef.current = indicatorConfig;
  }, [indicatorConfig]);

  useEffect(() => {
    if (!chartHostRef.current || chartManagerRef.current) {
      return;
    }

    chartManagerRef.current = new ChartManager(chartHostRef.current);
    chartManagerRef.current.setIndicatorConfig(indicatorConfigRef.current);

    return () => {
      chartManagerRef.current?.destroy();
      chartManagerRef.current = null;
    };
  }, []);

  useEffect(() => {
    setSnapshot(null);
    chartManagerRef.current?.clearPreviousAiDrawings();
    chartManagerRef.current?.resetChartData();
    chartManagerRef.current?.setPendingSymbol(selectedSymbol);

    fetch(`${apiBase}/api/symbol`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ symbol: selectedSymbol }),
    }).catch(() => {});
  }, [selectedSymbol]);

  useEffect(() => {
    setSnapshot(null);
    chartManagerRef.current?.clearPreviousAiDrawings();
    chartManagerRef.current?.resetChartData();

    fetch(`${apiBase}/api/interval`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ interval: selectedInterval }),
    }).catch(() => {});
  }, [selectedInterval]);

  useEffect(() => {
    let isMounted = true;

    fetch('https://fapi.binance.com/fapi/v1/exchangeInfo')
      .then((response) => response.json())
      .then((payload) => {
        if (!isMounted) {
          return;
        }

        const loaded = (payload.symbols ?? [])
          .filter(
            (item: ExchangeSymbol) =>
              item.quoteAsset === 'USDT' &&
              item.contractType === 'PERPETUAL' &&
              item.status === 'TRADING',
          )
          .slice(0, 80);
        setSymbols(loaded);
      })
      .catch(() => setSymbols([]));

    return () => {
      isMounted = false;
    };
  }, []);

  useEffect(() => {
    let ws: WebSocket | null = null;
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
    let silenceTimer: ReturnType<typeof setTimeout> | null = null;
    let alive = true;

    function resetSilenceTimer() {
      if (silenceTimer) clearTimeout(silenceTimer);
      silenceTimer = setTimeout(() => {
        // No update received for 15s — backend may have lost Binance stream.
        ws?.close();
      }, 15_000);
    }

    function connect() {
      if (!alive) return;
      ws = new WebSocket(`${wsBase}/ws`);

      ws.onopen = () => resetSilenceTimer();

      ws.onmessage = (event) => {
        resetSilenceTimer();
        try {
          const nextSnapshot = JSON.parse(event.data as string) as MarketSnapshot;
          setSnapshot(nextSnapshot);
          chartManagerRef.current?.updateSnapshot(nextSnapshot);
          simulationRef.current.updatePriceTick(nextSnapshot.last_price);
        } catch {
          // malformed frame — ignore
        }
      };

      ws.onclose = () => {
        if (silenceTimer) clearTimeout(silenceTimer);
        if (alive) reconnectTimer = setTimeout(connect, 2000);
      };

      ws.onerror = () => ws?.close();
    }

    connect();

    return () => {
      alive = false;
      if (reconnectTimer) clearTimeout(reconnectTimer);
      if (silenceTimer) clearTimeout(silenceTimer);
      ws?.close();
    };
  }, []);

  useEffect(() => {
    if (settingsOpen) setEmaRawInput(indicatorConfig.emas.join(', '));
  }, [settingsOpen]);

  const confluence = useMemo(() => {
    const raw = (snapshot as unknown as { confluence?: Record<string, string> } | null)
      ?.confluence;

    return [
      ['5m', raw?.tf_5m ?? 'Neutral'],
      ['15m', raw?.tf_15m ?? 'Neutral'],
      ['1h', raw?.tf_1h ?? 'Neutral'],
      ['4h', raw?.tf_4h ?? 'Neutral'],
    ];
  }, [snapshot]);

  function saveSettings() {
    localStorage.setItem('columba_llama_server_url', llamaServerUrl);
    localStorage.setItem('columba_indicators', JSON.stringify(indicatorConfig));
    chartManagerRef.current?.setIndicatorConfig(indicatorConfig);
    setSettingsOpen(false);
  }

  async function submitAnalysis(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();

    const trimmed = prompt.trim();
    if (!trimmed || isAnalyzing) {
      return;
    }

    const nextMessages: ChatMessage[] = [
      ...messages,
      { role: 'user', content: `${trimmed}\nExecution mode: ${modelMode}` },
    ];

    setMessages(nextMessages);
    setPrompt('');
    setIsAnalyzing(true);

    try {
      const body: Record<string, unknown> = {
        messages: nextMessages,
        position_size_pct: positionSizePct,
        leverage,
      };
      if (llamaServerUrl) body.llama_server_url = llamaServerUrl;

      const response = await fetch(`${apiBase}/api/analyze`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });

      if (!response.ok) {
        throw new Error(await response.text());
      }

      const payload = (await response.json()) as { target: TradePlanPayload & { thesis?: string }; trade_log_id?: number };
      chartManagerRef.current?.drawTradePlan(payload.target);
      tradeLogIdRef.current = payload.trade_log_id ?? null;
      simulationRef.current.armTrade({
        entryPrice: payload.target.entry_price,
        takeProfit: payload.target.take_profit,
        stopLoss: payload.target.stop_loss,
      });

      const levels = (() => {
        const fmt = (n: number) => n.toLocaleString(undefined, { maximumFractionDigits: 4 });
        return `Entry ${fmt(payload.target.entry_price)} · TP ${fmt(payload.target.take_profit)} · SL ${fmt(payload.target.stop_loss)}`;
      })();

      const fullContent = payload.target.thesis
        ? `${payload.target.thesis}\n\n${levels}`
        : levels;

      // Store full content in messages immediately so it persists after typing.
      setMessages((current) => [
        ...current,
        { role: 'assistant', content: fullContent },
      ]);

      // Typewriter starts at levels-only, drips toward fullContent.
      if (payload.target.thesis) {
        setStreamingThesis({ full: fullContent, displayed: levels });
      }
    } catch (error) {
      setMessages((current) => [
        ...current,
        {
          role: 'assistant',
          content: error instanceof Error ? error.message : 'Analysis failed.',
        },
      ]);
    } finally {
      setIsAnalyzing(false);
    }
  }

  function captureScreenshot() {
    const screenshot = chartManagerRef.current?.takeScreenshot();
    if (!screenshot) {
      return;
    }

    const preview = window.open('', '_blank', 'width=1280,height=800');
    if (preview) {
      preview.document.body.style.margin = '0';
      preview.document.body.style.background = '#101418';
      preview.document.body.appendChild(screenshot);
    }
  }

  async function downloadModel(entry: ModelEntry) {
    try {
      await invoke('download_missing_model', { name: entry.name, url: entry.url });
      const updatedModels = (setupStatus?.models ?? []).map((m) =>
        m.name === entry.name ? { ...m, present: true } : m,
      );
      const allReady = updatedModels.every((m) => m.present);
      setSetupStatus((prev) => {
        if (!prev) return prev;
        return { ...prev, models: updatedModels, ready: allReady };
      });
      if (allReady) {
        // All models present — launch llama-server without waiting for a restart.
        invoke('start_llama_server').catch(() => {});
      }
    } catch (e) {
      addToast(`Download failed: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  // Setup screen shown in Tauri mode when models are missing.
  if (isTauri && setupStatus && !setupStatus.ready) {
    const tierLabel: Record<string, string> = {
      low_end: 'Low-end (CPU)',
      mid_range: 'Mid-range (CPU / Vulkan)',
      high_end: 'High-end (CUDA / Vulkan)',
    };
    const hasMissingUrls = setupStatus.models.some((m) => !m.present && !m.url);
    return (
      <main className="setup-screen">
        <div className="setup-card">
          <h1>Columba — First-run Setup</h1>
          <p className="setup-meta">
            Detected <strong>{tierLabel[setupStatus.tier] ?? setupStatus.tier}</strong>
            {' '}· {setupStatus.ram_gb.toFixed(0)} GB RAM
            {setupStatus.vram_gb != null && ` · ${setupStatus.vram_gb.toFixed(0)} GB VRAM`}
            {' '}· {setupStatus.physical_cores} cores
          </p>
          <p className="setup-hint">
            These models must be downloaded before Columba can run local inference.
          </p>
          {hasMissingUrls && (
            <p className="setup-warning">
              The bundled model manifest could not be resolved, so download links are unavailable.
            </p>
          )}
          <ul className="model-list">
            {setupStatus.models.map((m) => {
              const prog = downloadingModels[m.name];
              const pct = prog && prog.total_bytes > 0
                ? Math.round((prog.downloaded_bytes / prog.total_bytes) * 100)
                : null;
              return (
                <li key={m.name} className={`model-entry ${m.present ? 'present' : ''}`}>
                  <span className="model-name">{m.name}</span>
                  <span className="model-size">{m.size_mb >= 1000 ? `${(m.size_mb / 1024).toFixed(1)} GB` : `${m.size_mb} MB`}</span>
                  {m.present ? (
                    <span className="model-status ok">✓ Ready</span>
                  ) : prog ? (
                    <span className="model-status downloading">
                      {pct != null ? `${pct}%` : 'Connecting…'}
                    </span>
                  ) : m.url ? (
                    <button
                      className="dl-btn"
                      type="button"
                      onClick={() => downloadModel(m)}
                    >
                      <Download size={14} /> Download
                    </button>
                  ) : (
                    <span className="model-status error">Unavailable</span>
                  )}
                  {prog && prog.total_bytes > 0 && (
                    <progress
                      className="dl-progress"
                      value={prog.downloaded_bytes}
                      max={prog.total_bytes}
                    />
                  )}
                </li>
              );
            })}
          </ul>
          {setupStatus.models.every((m) => m.present || downloadingModels[m.name]) && (
            <p className="setup-hint">Downloading… app will start automatically when complete.</p>
          )}
        </div>
      </main>
    );
  }

  return (
    <main className="app-shell">
      <aside className="left-panel">
        <header className="panel-header">
          <div>
            <p className="eyebrow">Futures</p>
            <h1>Columba</h1>
          </div>
          <Cpu size={22} />
        </header>

        <section className="instrument-list">
          {symbols.map((item) => (
            <button
              className={item.symbol === selectedSymbol ? 'instrument active' : 'instrument'}
              key={item.symbol}
              onClick={() => setSelectedSymbol(item.symbol)}
              type="button"
            >
              <span>{item.symbol}</span>
              <small>{item.contractType}</small>
            </button>
          ))}
        </section>

        <section className="confluence-grid">
          {confluence.map(([timeframe, status]) => (
            <div className="status-cell" key={timeframe}>
              <span>{timeframe}</span>
              <strong className={status.toLowerCase()}>{status}</strong>
            </div>
          ))}
        </section>

        <section className="intel-panel">
          <p className="eyebrow">Intelligence</p>

          {snapshot?.volatility && (
            <div className="intel-row">
              <span className="intel-label">Regime</span>
              <span className={`intel-val regime-badge ${regimeClass(snapshot.volatility.regime)}`}>
                {snapshot.volatility.regime}
                {snapshot.volatility.atr_percentile != null && (
                  <> · {Math.round(snapshot.volatility.atr_percentile)}p</>
                )}
                {snapshot.volatility.expanding === true && ' ↑'}
                {snapshot.volatility.expanding === false && ' ↓'}
              </span>
            </div>
          )}

          {snapshot?.sentiment?.feed_healthy && (
            <div className="intel-row">
              <span className="intel-label">F&amp;G</span>
              <span className={`intel-val ${fgClass(snapshot.sentiment.classification)}`}>
                {snapshot.sentiment.value} · {snapshot.sentiment.classification}
              </span>
            </div>
          )}

          {snapshot?.funding_rate != null && (
            <div className="intel-row">
              <span className="intel-label">Funding</span>
              <span className={`intel-val ${snapshot.funding_rate > 0.001 ? 'bearish-val' : snapshot.funding_rate < -0.001 ? 'bullish-val' : 'neutral-val'}`}>
                {snapshot.funding_rate >= 0 ? '+' : ''}{(snapshot.funding_rate * 100).toFixed(4)}%
              </span>
            </div>
          )}

          {snapshot?.basis?.feed_healthy && snapshot.basis.basis_pct != null && (
            <div className="intel-row">
              <span className="intel-label">Basis</span>
              <span className={`intel-val ${snapshot.basis.basis_pct > 0.05 ? 'bullish-val' : snapshot.basis.basis_pct < -0.05 ? 'bearish-val' : 'neutral-val'}`}>
                {snapshot.basis.basis_pct >= 0 ? '+' : ''}{snapshot.basis.basis_pct.toFixed(3)}%
              </span>
            </div>
          )}

          {snapshot?.orderflow && (
            <div className="intel-row">
              <span className="intel-label">Flow</span>
              <span className="intel-val">
                {snapshot.orderflow.sweep_detected ? (
                  <span className={snapshot.orderflow.sweep_direction === 'ask' ? 'bearish-val' : 'bullish-val'}>
                    {snapshot.orderflow.sweep_direction === 'ask' ? 'Bear Sweep' : 'Bull Sweep'}
                  </span>
                ) : snapshot.orderflow.absorption_detected ? (
                  <span className="neutral-val">Absorption</span>
                ) : (
                  <span className={snapshot.orderflow.buy_pressure_pct > 0.55 ? 'bullish-val' : snapshot.orderflow.buy_pressure_pct < 0.45 ? 'bearish-val' : 'neutral-val'}>
                    {Math.round(snapshot.orderflow.buy_pressure_pct * 100)}% buy
                  </span>
                )}
              </span>
            </div>
          )}

          {snapshot?.liquidations?.feed_healthy && (snapshot.liquidations.long_5m > 0 || snapshot.liquidations.short_5m > 0) && (
            <div className="intel-row">
              <span className="intel-label">Liq 5m</span>
              <span className="intel-val liq-row">
                <span className="bullish-val">{fmtUsd(snapshot.liquidations.long_5m)}</span>
                <span className="intel-sep">·</span>
                <span className="bearish-val">{fmtUsd(snapshot.liquidations.short_5m)}</span>
              </span>
            </div>
          )}

          {snapshot?.global_oi && snapshot.global_oi.divergence_score > 0.3 && (
            <div className="intel-row">
              <span className="intel-label">OI Div</span>
              <span className={`intel-val ${snapshot.global_oi.divergence_score > 0.6 ? 'bearish-val' : 'neutral-val'}`}>
                {snapshot.global_oi.divergence_label}
              </span>
            </div>
          )}
        </section>
      </aside>

      <section className="center-panel">
        <div className="chart-toolbar">
          <div>
            <span className="symbol">{selectedSymbol}</span>
            <span className="price">
              {snapshot?.last_price ? snapshot.last_price.toLocaleString() : 'Waiting for ticks'}
            </span>
          </div>
          <div className="tf-selector">
            {['1m', '3m', '5m', '15m', '1h', '4h', '1d'].map((tf) => (
              <button
                key={tf}
                type="button"
                className={tf === selectedInterval ? 'tf-btn active' : 'tf-btn'}
                onClick={() => setSelectedInterval(tf)}
              >
                {tf}
              </button>
            ))}
          </div>
          <button className="icon-button" onClick={captureScreenshot} type="button" title="Screenshot">
            <Camera size={18} />
          </button>
        </div>
        {snapshot && !snapshot.cvd_seeded && (
          <div className="cvd-warming-banner">
            CVD warming up — signals may be imprecise until first live candle closes
          </div>
        )}
        <div className="chart-host" ref={chartHostRef} />
      </section>

      <aside className="right-panel">
        <header className="panel-header">
          <div>
            <p className="eyebrow">LLM desk</p>
            <h2>{rightPanelTab === 'chat' ? 'Execution Chat' : 'Tutorials'}</h2>
          </div>
          <button
            className="icon-button"
            onClick={() => setSettingsOpen(true)}
            type="button"
            title="Settings"
          >
            <SlidersHorizontal size={22} />
          </button>
        </header>

        <div className="panel-tabs" role="tablist" aria-label="Right panel tabs">
          <button
            type="button"
            className={rightPanelTab === 'chat' ? 'panel-tab active' : 'panel-tab'}
            onClick={() => setRightPanelTab('chat')}
            role="tab"
            aria-selected={rightPanelTab === 'chat'}
          >
            <span className="panel-tab-icon" aria-hidden="true">⌘</span>
            Chat
          </button>
          <button
            type="button"
            className={rightPanelTab === 'tutorials' ? 'panel-tab active' : 'panel-tab'}
            onClick={() => setRightPanelTab('tutorials')}
            role="tab"
            aria-selected={rightPanelTab === 'tutorials'}
          >
            <BookOpen size={14} aria-hidden="true" />
            Tutorials
          </button>
        </div>

        {rightPanelTab === 'chat' ? (
          <>
            <label className="select-label">
              Model routing
              <select value={modelMode} onChange={(event) => setModelMode(event.target.value)}>
                <option value="Auto">Auto</option>
                <option value="ForceLocal">Force local</option>
                <option value="ForceCloud">Force cloud</option>
              </select>
            </label>

            <section className="chat-log">
              {messages
                .filter((message) => message.role !== 'system')
                .slice(-7)
                .map((message, index, arr) => {
                  const isLastAssistant =
                    message.role === 'assistant' && index === arr.length - 1;
                  const content =
                    isLastAssistant && streamingThesis
                      ? streamingThesis.displayed
                      : message.content;
                  return (
                    <div className={`message ${message.role}`} key={`${message.role}-${index}`}>
                      {content}
                      {isLastAssistant && streamingThesis && (
                        <span className="cursor-blink">▋</span>
                      )}
                    </div>
                  );
                })}
            </section>

            <form className="chat-form" onSubmit={submitAnalysis}>
              <textarea
                value={prompt}
                onChange={(event) => setPrompt(event.target.value)}
                placeholder="Request a trade plan from current CVD, OI, ATR, and liquidity context."
              />
              <div className="position-sizing-row">
                <label>
                  Risk %
                  <input
                    type="number"
                    min={0.1}
                    max={100}
                    step={0.1}
                    value={positionSizePct}
                    onChange={(e) => setPositionSizePct(parseFloat(e.target.value) || 1)}
                  />
                </label>
                <label>
                  Leverage
                  <input
                    type="number"
                    min={1}
                    max={125}
                    step={1}
                    value={leverage}
                    onChange={(e) => setLeverage(parseInt(e.target.value, 10) || 1)}
                  />
                </label>
              </div>
              <button disabled={isAnalyzing || snapshot === null} type="submit">
                <Send size={17} />
                {isAnalyzing ? 'Analyzing…' : snapshot === null ? 'Connecting…' : 'Send'}
              </button>
            </form>
          </>
        ) : (
          <section className="tutorial-shell">
            <p className="tutorial-kicker">
              A quick reference for the indicators and metrics shown in the app.
            </p>
            <article
              className="tutorial-content"
              dangerouslySetInnerHTML={{ __html: tutorialHtml }}
            />
          </section>
        )}
      </aside>
      {settingsOpen && (
        <div className="modal-backdrop" onClick={() => setSettingsOpen(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>Settings</h3>

            <p className="settings-hint">
              AI key is read from <code>OPENAI_API_KEY</code> env var on the backend.
            </p>

            <label className="select-label">
              llama-server URL
              <input
                type="text"
                value={llamaServerUrl}
                onChange={(e) => setLlamaServerUrl(e.target.value)}
                placeholder="http://127.0.0.1:8081/v1"
              />
            </label>

            <p className="settings-section-label">Indicators</p>

            <label className="select-label">
              EMA periods (comma-separated)
              <input
                type="text"
                value={emaRawInput}
                onChange={(e) => setEmaRawInput(e.target.value)}
                onBlur={(e) => {
                  const emas = e.target.value
                    .split(',')
                    .map((s) => parseInt(s.trim(), 10))
                    .filter((n) => !Number.isNaN(n) && n > 0);
                  setIndicatorConfig((prev) => ({ ...prev, emas }));
                  setEmaRawInput(emas.join(', '));
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

            <p className="settings-hint">
              API key takes priority. Leave blank to use backend env vars.
            </p>

            <div className="modal-actions">
              <button type="button" onClick={() => setSettingsOpen(false)}>
                Cancel
              </button>
              <button type="button" className="primary" onClick={saveSettings}>
                Save
              </button>
            </div>
          </div>
        </div>
      )}
      {toasts.length > 0 && (
        <div className="toast-stack">
          {toasts.map((toast) => (
            <div key={toast.id} className="toast">
              {toast.text}
            </div>
          ))}
        </div>
      )}
    </main>
  );
}
