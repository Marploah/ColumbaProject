use crate::quant::UnifiedMarketState;
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
    Ollama,
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

    pub fn ollama(model: String) -> Self {
        Self::ollama_at(model, "http://localhost:11434/v1".to_string())
    }

    pub fn ollama_at(model: String, base_url: String) -> Self {
        let api_key = "ollama".to_string();
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
            provider: Provider::Ollama,
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
            .json(&json!({
                "model": self.model,
                "temperature": 0.1,
                "response_format": { "type": "json_object" },
                "messages": messages,
            }))
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

    let snapshot = serde_json::to_string(state).context("failed to serialize market snapshot")?;
    let snapshot_message = ChatMessage {
        role: "user".to_string(),
        content: format!("Fresh UnifiedMarketState JSON snapshot: {snapshot}"),
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

    Ok(TradePlan {
        entry_price,
        take_profit,
        stop_loss,
        thesis: value
            .get("thesis")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn get_number(value: &Value, key: &str) -> Result<f64> {
    value
        .get(key)
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite())
        .ok_or_else(|| anyhow!("LLM JSON missing finite numeric field `{key}`"))
}
