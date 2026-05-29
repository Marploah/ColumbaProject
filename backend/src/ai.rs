use crate::quant::UnifiedMarketState;
use crate::state::semantic::SignalInterpreter;

/// GBNF grammar injected into llama.cpp requests to guarantee valid JSON output.
/// Enforces exact field order (numerics first, thesis last) so the model focuses
/// on numerical precision before generating the free-text thesis.
const TRADE_PLAN_GRAMMAR: &str = r#"root  ::= "{" ws fld-entry "," ws fld-tp "," ws fld-sl "," ws fld-thesis ws "}"
fld-entry  ::= "\"entry_price\""  ws ":" ws number
fld-tp     ::= "\"take_profit\""  ws ":" ws number
fld-sl     ::= "\"stop_loss\""    ws ":" ws number
fld-thesis ::= "\"thesis\""       ws ":" ws string
number ::= "-"? ( [0-9]+ ( "." [0-9]+ )? )
string ::= "\"" char* "\""
char   ::= [^"\\] | "\\" ( ["\\/bfnrt] | "u" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] )
ws     ::= ( " " | "\t" | "\n" | "\r" )*"#;
use anyhow::{anyhow, Context, Result};
use async_openai::{config::OpenAIConfig, Client};
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradePlan {
    pub entry_price: f64,
    pub take_profit: f64,
    pub stop_loss: f64,
    pub thesis: Option<String>,
}

#[derive(Clone, Debug)]
enum Provider {
    OpenAI,
    Anthropic,
    LlamaCpp,
}

#[derive(Clone)]
pub struct AiBroker {
    http: reqwest::Client,
    _openai_client: Client<OpenAIConfig>,
    api_base: String,
    api_key: String,
    model: String,
    provider: Provider,
}

impl AiBroker {
    pub fn openai(api_key: String, model: String) -> Self {
        let config = OpenAIConfig::new().with_api_key(api_key.clone());
        Self {
            http: reqwest::Client::new(),
            _openai_client: Client::with_config(config),
            api_base: "https://api.openai.com/v1".to_string(),
            api_key,
            model,
            provider: Provider::OpenAI,
        }
    }

    pub fn anthropic(api_key: String, model: String) -> Self {
        let config = OpenAIConfig::new().with_api_key(api_key.clone());
        Self {
            http: reqwest::Client::new(),
            _openai_client: Client::with_config(config),
            api_base: "https://api.anthropic.com/v1".to_string(),
            api_key,
            model,
            provider: Provider::Anthropic,
        }
    }

    pub fn llama_cpp_at(model: String, base_url: String) -> Self {
        let api_key = "no-key".to_string();
        let api_base = base_url;
        let config = OpenAIConfig::new()
            .with_api_base(api_base.clone())
            .with_api_key(api_key.clone());

        Self {
            http: reqwest::Client::new(),
            _openai_client: Client::with_config(config),
            api_base,
            api_key,
            model,
            provider: Provider::LlamaCpp,
        }
    }

    pub async fn request_trade_plan(
        &self,
        messages: Vec<ChatMessage>,
        state: &UnifiedMarketState,
    ) -> Result<TradePlan> {
        let pruned = prune_context_window(messages, state)?;
        let response = self.chat_completion(pruned).await?;
        parse_trade_plan(&response)
    }

    async fn chat_completion(&self, messages: Vec<ChatMessage>) -> Result<String> {
        match self.provider {
            Provider::Anthropic => self.anthropic_completion(messages).await,
            _ => self.openai_completion(messages).await,
        }
    }

    async fn openai_completion(&self, messages: Vec<ChatMessage>) -> Result<String> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.api_key))
                .context("invalid API key header")?,
        );

        let response = self
            .http
            .post(format!(
                "{}/chat/completions",
                self.api_base.trim_end_matches('/')
            ))
            .headers(headers)
            .json(&{
                let mut body = json!({
                    "model": self.model,
                    "temperature": 0.1,
                    "response_format": { "type": "json_object" },
                    "messages": messages,
                });
                if matches!(self.provider, Provider::LlamaCpp) {
                    body["chat_template_kwargs"] = json!({ "enable_thinking": false });
                    body["grammar"] = json!(TRADE_PLAN_GRAMMAR);
                }
                body
            })
            .send()
            .await
            .context("failed to send LLM chat completion request")?;

        let status = response.status();
        let body: Value = response.json().await.context("invalid LLM JSON response")?;

        if !status.is_success() {
            return Err(anyhow!("LLM provider returned {status}: {body}"));
        }

        body.pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("LLM response did not contain choices[0].message.content"))
    }

    async fn anthropic_completion(&self, messages: Vec<ChatMessage>) -> Result<String> {
        let system = messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.clone())
            .unwrap_or_default();

        let non_system: Vec<&ChatMessage> =
            messages.iter().filter(|m| m.role != "system").collect();

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            HeaderName::from_static("x-api-key"),
            HeaderValue::from_str(&self.api_key).context("invalid Anthropic API key header")?,
        );
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );

        let response = self
            .http
            .post(format!(
                "{}/messages",
                self.api_base.trim_end_matches('/')
            ))
            .headers(headers)
            .json(&json!({
                "model": self.model,
                "max_tokens": 1024,
                "system": system,
                "messages": non_system,
            }))
            .send()
            .await
            .context("failed to send Anthropic chat completion request")?;

        let status = response.status();
        let body: Value = response
            .json()
            .await
            .context("invalid Anthropic JSON response")?;

        if !status.is_success() {
            return Err(anyhow!("Anthropic API returned {status}: {body}"));
        }

        body.pointer("/content/0/text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("Anthropic response did not contain content[0].text"))
    }
}

/// Formats a human-readable, annotated market brief for the LLM.
/// Avoids dumping the raw candles array (thousands of tokens the LLM cannot use)
/// and instead provides each signal with units, magnitude context, and data-quality
/// warnings so the model can reason about them correctly.
fn format_market_brief(state: &UnifiedMarketState) -> String {
    let mut lines = vec![
        format!("=== MARKET STATE: {} ===", state.symbol),
        format!("Last price: {:.4}", state.last_price),
    ];

    if let Some(vwap) = state.vwap {
        let delta = state.last_price - vwap;
        let pct = delta / vwap * 100.0;
        lines.push(format!(
            "VWAP: {:.4} — price is {:.2}% {} VWAP ({})",
            vwap,
            pct.abs(),
            if delta >= 0.0 { "above" } else { "below" },
            if delta >= 0.0 { "premium to fair value" } else { "discount to fair value" },
        ));
    }

    if let Some(atr) = state.atr_14 {
        let atr_pct = atr / state.last_price * 100.0;
        let upper = state.volatility_upper_limit.unwrap_or(f64::NAN);
        let lower = state.volatility_lower_limit.unwrap_or(f64::NAN);
        lines.push(format!(
            "ATR-14: {:.4} ({:.2}% of price) | 1.5× ATR band: [{:.4}, {:.4}]",
            atr, atr_pct, lower, upper,
        ));
    }

    let cvd = state.confluence.cvd_slope;
    lines.push(format!(
        "CVD slope (20-candle linear regression): {:.4} — {} flow momentum",
        cvd,
        if cvd > 0.0 { "bullish (net buying)" } else { "bearish (net selling)" },
    ));

    let oi_pct = state.confluence.oi_change_pct;
    let oi_quality = if state.oi_is_real {
        "real Binance OI"
    } else {
        "PROXY DATA (quote-volume) — discount OI signals, do not rely on them for confirmation"
    };
    lines.push(format!(
        "OI change: {:.2}% ({}) — open interest is {}",
        oi_pct,
        oi_quality,
        if oi_pct > 0.5 { "expanding (new positions opening)" }
        else if oi_pct < -0.5 { "contracting (positions closing)" }
        else { "flat" },
    ));

    if let Some(fr) = state.funding_rate {
        let fr_pct = fr * 100.0;
        let sentiment = if fr > 0.001 {
            "longs paying heavy premium — crowded long, elevated squeeze risk"
        } else if fr > 0.0001 {
            "mild long bias"
        } else if fr < -0.001 {
            "shorts paying heavy premium — crowded short, elevated squeeze risk"
        } else if fr < -0.0001 {
            "mild short bias"
        } else {
            "neutral"
        };
        let h = state.funding_hours_to_settlement;
        let urgency = if h < 0.5 {
            "imminent settlement — funding impact is immediate"
        } else if h < 2.0 {
            "settlement approaching"
        } else {
            "mid-cycle"
        };
        lines.push(format!(
            "Funding rate: {:.4}% ({}) | {:.1}h to settlement ({})",
            fr_pct, sentiment, h, urgency,
        ));
    }

    if let (Some(bp), Some(bs)) = (state.liquidity_walls.bid_wall_price, state.liquidity_walls.bid_wall_size) {
        lines.push(format!("Bid liquidity wall: {:.4} (size {:.2}) — demand cluster / support", bp, bs));
    }
    if let (Some(ap), Some(as_)) = (state.liquidity_walls.ask_wall_price, state.liquidity_walls.ask_wall_size) {
        lines.push(format!("Ask liquidity wall: {:.4} (size {:.2}) — supply cluster / resistance", ap, as_));
    }

    let rsi_div = &state.confluence.rsi_divergence;
    if rsi_div != "none" {
        lines.push(format!("RSI-14 divergence: {} — potential momentum reversal", rsi_div));
    }

    lines.push(format!(
        "Multi-timeframe bias (fetched from real Binance klines): 5m={} | 15m={} | 1h={} | 4h={}",
        state.confluence.tf_5m,
        state.confluence.tf_15m,
        state.confluence.tf_1h,
        state.confluence.tf_4h,
    ));

    // Semantic summary — gated by COLUMBA_SEMANTIC_BRIEF (default on).
    // Adds compressed interpretations and contradiction warnings without
    // duplicating the raw metrics already printed above.
    let semantic_on = std::env::var("COLUMBA_SEMANTIC_BRIEF")
        .map(|v| v != "0")
        .unwrap_or(true);

    if semantic_on {
        let sem = SignalInterpreter::interpret(state);

        // Collect high-confidence signals (≥0.6) that have a severity tag first,
        // then remaining signals above the floor, sorted by confidence desc.
        const CONFIDENCE_FLOOR: f32 = 0.6;
        let signals = [
            &sem.leverage_state,
            &sem.volatility_state,
            &sem.liquidity_state,
            &sem.orderflow_state,
            &sem.liquidation_state,
        ];

        let mut summary_lines: Vec<String> = signals
            .iter()
            .filter(|s| s.confidence >= CONFIDENCE_FLOOR)
            .filter(|s| !s.label.contains("unknown") && !s.label.contains("unavailable"))
            .map(|s| {
                let sev = s.severity.as_deref().map(|sv| format!(" [{sv}]")).unwrap_or_default();
                format!("  • {}{} — {}", s.label, sev, s.explanation)
            })
            .collect();

        // Directional bias always included regardless of confidence floor.
        summary_lines.push(format!(
            "  • directional: {}",
            sem.directional_bias.explanation,
        ));

        if !summary_lines.is_empty() {
            lines.push("--- SEMANTIC SUMMARY ---".to_string());
            lines.extend(summary_lines);
        }

        if !sem.contradictions.is_empty() {
            lines.push("--- SIGNAL CONTRADICTIONS ---".to_string());
            for c in &sem.contradictions {
                lines.push(format!("  ⚠ {c}"));
            }
        }
    }

    lines.join("\n")
}

pub fn prune_context_window(
    mut messages: Vec<ChatMessage>,
    state: &UnifiedMarketState,
) -> Result<Vec<ChatMessage>> {
    if messages.is_empty() {
        return Err(anyhow!("chat context cannot be empty"));
    }

    let system = messages
        .iter()
        .find(|message| message.role == "system")
        .cloned()
        .ok_or_else(|| anyhow!("chat context must include a system instruction"))?;

    messages.retain(|message| message.role != "system");

    let latest_user_idx = messages
        .iter()
        .rposition(|message| message.role == "user")
        .ok_or_else(|| anyhow!("chat context must include a latest user query"))?;

    let latest_user = messages.remove(latest_user_idx);
    let prior = &messages[..latest_user_idx.min(messages.len())];

    let mut exchanges = Vec::new();
    let mut cursor = prior.len();

    while cursor > 0 && exchanges.len() < 6 {
        cursor -= 1;
        let message = prior[cursor].clone();
        if message.role == "user" || message.role == "assistant" {
            exchanges.push(message);
        }
    }

    exchanges.reverse();

    let brief = format_market_brief(state);
    let snapshot_message = ChatMessage {
        role: "user".to_string(),
        content: format!("Current market data:\n\n{brief}"),
    };

    let mut pruned = Vec::with_capacity(exchanges.len() + 3);
    pruned.push(system);
    pruned.extend(exchanges);
    pruned.push(snapshot_message);
    pruned.push(latest_user);

    Ok(pruned)
}

pub fn clean_llm_json(raw: &str) -> String {
    let fence_re = Regex::new(r"(?im)^\s*```(?:json|javascript|js|ts)?\s*$|^\s*```\s*$")
        .expect("valid markdown fence regex");
    let cleaned = fence_re.replace_all(raw.trim(), "");

    let leading_re = Regex::new(r"(?is)^[^{\[]*").expect("valid leading trim regex");
    let trailing_re = Regex::new(r"(?is)[^}\]]*$").expect("valid trailing trim regex");
    let cleaned = leading_re.replace(&cleaned, "");
    let cleaned = trailing_re.replace(&cleaned, "");

    cleaned.trim().to_string()
}

pub fn parse_trade_plan(raw: &str) -> Result<TradePlan> {
    let cleaned = clean_llm_json(raw);

    if cleaned.is_empty() {
        return Err(anyhow!("LLM response did not contain a JSON object"));
    }

    let value: Value = serde_json::from_str(&cleaned)
        .with_context(|| format!("failed to parse cleaned LLM JSON: {cleaned}"))?;

    let entry_price = get_number(&value, "entry_price")?;
    let take_profit = get_number(&value, "take_profit")?;
    let stop_loss = get_number(&value, "stop_loss")?;

    let plan = TradePlan {
        entry_price,
        take_profit,
        stop_loss,
        thesis: value
            .get("thesis")
            .and_then(Value::as_str)
            .map(str::to_string),
    };

    validate_trade_plan(&plan)?;
    Ok(plan)
}

const MIN_REWARD_RISK_RATIO: f64 = 1.5;

fn validate_trade_plan(plan: &TradePlan) -> Result<()> {
    if (plan.take_profit - plan.entry_price).abs() < f64::EPSILON {
        return Err(anyhow!(
            "take_profit ({}) equals entry_price ({}) — zero-profit plan rejected",
            plan.take_profit, plan.entry_price
        ));
    }

    let is_long = plan.take_profit > plan.entry_price;

    if is_long && plan.stop_loss >= plan.entry_price {
        return Err(anyhow!(
            "long trade invalid: stop_loss ({}) must be below entry_price ({})",
            plan.stop_loss, plan.entry_price
        ));
    }

    if !is_long && plan.stop_loss <= plan.entry_price {
        return Err(anyhow!(
            "short trade invalid: stop_loss ({}) must be above entry_price ({})",
            plan.stop_loss, plan.entry_price
        ));
    }

    let reward = (plan.take_profit - plan.entry_price).abs();
    let risk = (plan.entry_price - plan.stop_loss).abs();

    if risk < f64::EPSILON {
        return Err(anyhow!(
            "stop_loss ({}) equals entry_price ({}) — zero-risk plan rejected",
            plan.stop_loss, plan.entry_price
        ));
    }

    let rr = reward / risk;
    if rr < MIN_REWARD_RISK_RATIO {
        return Err(anyhow!(
            "R:R {:.2}:1 below minimum {:.1}:1 (reward {:.4} / risk {:.4}) — plan rejected",
            rr, MIN_REWARD_RISK_RATIO, reward, risk
        ));
    }

    Ok(())
}

fn get_number(value: &Value, key: &str) -> Result<f64> {
    value
        .get(key)
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite())
        .ok_or_else(|| anyhow!("LLM JSON missing finite numeric field `{key}`"))
}
