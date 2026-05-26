import { Camera, Cpu, Send, SlidersHorizontal } from 'lucide-react';
import { FormEvent, useEffect, useMemo, useRef, useState } from 'react';
import { ChartManager, DEFAULT_INDICATOR_CONFIG, IndicatorConfig, MarketSnapshot, TradePlanPayload } from './ChartManager';
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

const apiBase = 'http://127.0.0.1:8080';

const systemPrompt: ChatMessage = {
  role: 'system',
  content:
    'You are a crypto futures execution analyst. Return only JSON with entry_price, take_profit, stop_loss, and thesis.',
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
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [openaiApiKey, setOpenaiApiKey] = useState(
    () => localStorage.getItem('columba_openai_api_key') ?? '',
  );
  const [ollamaUrl, setOllamaUrl] = useState(
    () => localStorage.getItem('columba_ollama_url') ?? 'http://localhost:11434/v1',
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
  const [emaRawInput, setEmaRawInput] = useState(indicatorConfig.emas.join(', '));

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
    let alive = true;

    function connect() {
      if (!alive) return;
      ws = new WebSocket(`ws://127.0.0.1:8080/ws`);

      ws.onmessage = (event) => {
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
        if (alive) reconnectTimer = setTimeout(connect, 2000);
      };

      ws.onerror = () => ws?.close();
    }

    connect();

    return () => {
      alive = false;
      if (reconnectTimer) clearTimeout(reconnectTimer);
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
    localStorage.setItem('columba_openai_api_key', openaiApiKey);
    localStorage.setItem('columba_ollama_url', ollamaUrl);
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
      const body: Record<string, unknown> = { messages: nextMessages };
      if (openaiApiKey) body.openai_api_key = openaiApiKey;
      else if (ollamaUrl) body.ollama_url = ollamaUrl;

      const response = await fetch(`${apiBase}/api/analyze`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });

      if (!response.ok) {
        throw new Error(await response.text());
      }

      const payload = (await response.json()) as { target: TradePlanPayload & { thesis?: string } };
      chartManagerRef.current?.drawTradePlan(payload.target);
      simulationRef.current.armTrade({
        entryPrice: payload.target.entry_price,
        takeProfit: payload.target.take_profit,
        stopLoss: payload.target.stop_loss,
      });

      setMessages((current) => [
        ...current,
        {
          role: 'assistant',
          content: JSON.stringify(payload.target),
        },
      ]);
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
        <div className="chart-host" ref={chartHostRef} />
      </section>

      <aside className="right-panel">
        <header className="panel-header">
          <div>
            <p className="eyebrow">LLM desk</p>
            <h2>Execution Chat</h2>
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
            .map((message, index) => (
              <div className={`message ${message.role}`} key={`${message.role}-${index}`}>
                {message.content}
              </div>
            ))}
        </section>

        <form className="chat-form" onSubmit={submitAnalysis}>
          <textarea
            value={prompt}
            onChange={(event) => setPrompt(event.target.value)}
            placeholder="Request a trade plan from current CVD, OI, ATR, and liquidity context."
          />
          <button disabled={isAnalyzing} type="submit">
            <Send size={17} />
            {isAnalyzing ? 'Analyzing' : 'Send'}
          </button>
        </form>
      </aside>
      {settingsOpen && (
        <div className="modal-backdrop" onClick={() => setSettingsOpen(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>Settings</h3>

            <label className="select-label">
              OpenAI API key
              <input
                type="password"
                value={openaiApiKey}
                onChange={(e) => setOpenaiApiKey(e.target.value)}
                placeholder="sk-..."
                autoComplete="off"
              />
            </label>

            <label className="select-label">
              Ollama base URL
              <input
                type="text"
                value={ollamaUrl}
                onChange={(e) => setOllamaUrl(e.target.value)}
                placeholder="http://localhost:11434/v1"
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
    </main>
  );
}
