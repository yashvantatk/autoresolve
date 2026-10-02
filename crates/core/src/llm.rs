use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value, // JSON-schema object
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub name: String,
    pub args: Value,
}

#[derive(Debug, Clone)]
pub struct ModelTurn {
    pub text: String,
    pub calls: Vec<ToolCall>,
    /// Provider's original content, replayed verbatim on the next request.
    pub raw: Value,
}

#[derive(Debug, Clone)]
pub enum Message {
    User(String),
    Model(ModelTurn),
    ToolResults(Vec<(String, Value)>),
}

/// Anything that can run one step of an agent conversation.
#[async_trait]
pub trait Provider: Send + Sync {
    async fn complete(
        &self,
        system: &str,
        history: &[Message],
        tools: &[ToolSpec],
    ) -> Result<ModelTurn>;

    /// Ask for a final answer constrained to a JSON schema (grammar-constrained decoding).
    /// Backends that cannot do this keep the default, which reports "unsupported".
    async fn complete_json(&self, _system: &str, _history: &[Message], _schema: &Value) -> Result<Value> {
        bail!("structured output is not supported by this provider")
    }

    /// Successful API calls so far (for the usage line).
    fn calls(&self) -> usize {
        0
    }
}

/// Lets `Box<dyn Provider>` be passed anywhere a provider is expected.
#[async_trait]
impl Provider for Box<dyn Provider> {
    async fn complete(&self, system: &str, history: &[Message], tools: &[ToolSpec]) -> Result<ModelTurn> {
        (**self).complete(system, history, tools).await
    }
    async fn complete_json(&self, system: &str, history: &[Message], schema: &Value) -> Result<Value> {
        (**self).complete_json(system, history, schema).await
    }
    fn calls(&self) -> usize {
        (**self).calls()
    }
}

// ---------- Gemini ----------

pub struct Gemini {
    key: String,
    model: String,
    http: reqwest::Client,
    calls: AtomicUsize,
}

impl Gemini {
    pub fn from_env() -> Result<Self> {
        let key = std::env::var("GEMINI_API_KEY").context("set the GEMINI_API_KEY environment variable")?;
        let model = std::env::var("AUTORESOLVE_MODEL").unwrap_or_else(|_| "gemini-3.1-flash-lite".into());
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(180))
            .build()?;
        Ok(Self { key, model, http, calls: AtomicUsize::new(0) })
    }

    /// Like `from_env`, but a role-specific env var can override the model.
    pub fn from_env_role(var: &str) -> Result<Self> {
        let mut g = Self::from_env()?;
        if let Ok(m) = std::env::var(var) {
            g.model = m;
        }
        Ok(g)
    }
}

fn to_contents(history: &[Message]) -> Vec<Value> {
    history
        .iter()
        .map(|m| match m {
            Message::User(t) => json!({"role": "user", "parts": [{"text": t}]}),
            Message::Model(turn) => turn.raw.clone(),
            Message::ToolResults(rs) => json!({
                "role": "user",
                "parts": rs.iter().map(|(name, v)| json!({
                    "functionResponse": {"name": name, "response": {"result": v}}
                })).collect::<Vec<_>>()
            }),
        })
        .collect()
}

#[async_trait]
impl Provider for Gemini {
    async fn complete(&self, system: &str, history: &[Message], tools: &[ToolSpec]) -> Result<ModelTurn> {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
            self.model
        );
        let decls: Vec<Value> = tools
            .iter()
            .map(|t| {
                let mut d = json!({"name": t.name, "description": t.description});
                // Gemini rejects an object schema with empty `properties`, so omit it.
                if t.parameters["properties"].as_object().map_or(false, |o| !o.is_empty()) {
                    d["parameters"] = t.parameters.clone();
                }
                d
            })
            .collect();
        let mut body = json!({
            "systemInstruction": {"parts": [{"text": system}]},
            "contents": to_contents(history),
            "tools": [{"functionDeclarations": decls}],
        });
        // Optional: cap the model's hidden reasoning to make agent steps faster.
        if let Ok(level) = std::env::var("AUTORESOLVE_THINKING") {
            body["generationConfig"] = json!({"thinkingConfig": {"thinkingLevel": level}});
        }

        eprintln!("[waiting for {} ...]", self.model);
        let mut attempt: u32 = 0;
        let v: Value = loop {
            let sent = self
                .http
                .post(&url)
                .header("x-goog-api-key", &self.key)
                .json(&body)
                .send()
                .await;
            let resp = match sent {
                Ok(r) => r,
                // network-level failure (connect/timeout): back off and retry
                Err(e) if attempt < 4 => {
                    let wait = 2u64.pow(attempt + 1);
                    eprintln!("[retry] network error ({e}), waiting {wait}s (attempt {}/4)", attempt + 1);
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                    attempt += 1;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            let status = resp.status();
            let v: Value = resp.json().await?;
            if status.is_success() {
                self.calls.fetch_add(1, Ordering::Relaxed);
                break v;
            }
            // A daily quota cannot be fixed by waiting a few seconds: stop the whole run.
            if status.as_u16() == 429 && v.to_string().contains("PerDay") {
                bail!(
                    "QUOTA_EXHAUSTED: the daily request quota for {} is used up. It usually resets at \
                     midnight Pacific time. Meanwhile set AUTORESOLVE_MODEL to another model (each model \
                     has its own quota), or use AUTORESOLVE_PROVIDER=ollama.",
                    self.model
                );
            }
            // other 429s (per-minute) and 5xx are transient: wait (honoring the server's hint) and retry
            let retryable = status.as_u16() == 429 || status.is_server_error();
            if retryable && attempt < 4 {
                let wait = retry_delay(&v).unwrap_or(2u64.pow(attempt + 1)).min(90);
                eprintln!("[retry] Gemini returned {status}, waiting {wait}s (attempt {}/4)", attempt + 1);
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                attempt += 1;
                continue;
            }
            bail!("Gemini API error {status}: {v}");
        };

        let content = v["candidates"][0]["content"].clone();
        if content.is_null() {
            bail!("response had no content: {v}");
        }
        let mut text = String::new();
        let mut calls = Vec::new();
        for p in content["parts"].as_array().cloned().unwrap_or_default() {
            if let Some(t) = p["text"].as_str() {
                text.push_str(t);
            }
            if let Some(fc) = p.get("functionCall") {
                calls.push(ToolCall {
                    name: fc["name"].as_str().unwrap_or("").to_string(),
                    args: fc["args"].clone(),
                });
            }
        }
        Ok(ModelTurn { text, calls, raw: content })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

// ---------- Ollama: local models, no key, no quota ----------

pub struct Ollama {
    host: String,
    model: String,
    num_ctx: u32,
    think: Option<bool>,
    http: reqwest::Client,
    calls: AtomicUsize,
}

impl Ollama {
    pub fn from_env() -> Result<Self> {
        Self::build(None)
    }

    /// Role-specific model override (e.g. for the tester/fixer/gate).
    pub fn from_env_role(var: &str) -> Result<Self> {
        Self::build(std::env::var(var).ok())
    }

    fn build(model_override: Option<String>) -> Result<Self> {
        let host = std::env::var("AUTORESOLVE_OLLAMA_URL").unwrap_or_else(|_| "http://localhost:11434".into());
        let model = model_override
            .or_else(|| std::env::var("AUTORESOLVE_OLLAMA_MODEL").ok())
            .unwrap_or_else(|| "qwen3:8b".into());
        let num_ctx = std::env::var("AUTORESOLVE_NUM_CTX")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8192);
        // only sent when set: AUTORESOLVE_OLLAMA_THINK=false turns off reasoning on thinking models
        let think = std::env::var("AUTORESOLVE_OLLAMA_THINK").ok().map(|s| s == "true" || s == "1");
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(900)) // first call also loads the model
            .build()?;
        Ok(Self { host, model, num_ctx, think, http, calls: AtomicUsize::new(0) })
    }

    /// One /api/chat round trip; returns the assistant message.
    async fn chat(&self, mut body: Value) -> Result<Value> {
        if let Some(t) = self.think {
            body["think"] = json!(t);
        }
        eprintln!("[waiting for {} (ollama) ...]", self.model);
        let url = format!("{}/api/chat", self.host);
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("cannot reach Ollama at {} (is `ollama serve` running?)", self.host))?;
        let status = resp.status();
        let v: Value = resp.json().await?;
        if !status.is_success() {
            bail!("Ollama error {status}: {v}");
        }
        self.calls.fetch_add(1, Ordering::Relaxed);
        eprintln!(
            "[ollama] {} prompt tokens, {} generated tokens, {:.1}s",
            v["prompt_eval_count"].as_u64().unwrap_or(0),
            v["eval_count"].as_u64().unwrap_or(0),
            v["total_duration"].as_f64().unwrap_or(0.0) / 1e9
        );
        let msg = v["message"].clone();
        if msg.is_null() {
            bail!("no message in response: {v}");
        }
        Ok(msg)
    }
}

fn ollama_messages(system: &str, history: &[Message]) -> Vec<Value> {
    let mut messages = vec![json!({"role": "system", "content": system})];
    for m in history {
        match m {
            Message::User(t) => messages.push(json!({"role": "user", "content": t})),
            Message::Model(turn) => messages.push(turn.raw.clone()),
            Message::ToolResults(rs) => {
                for (name, v) in rs {
                    messages.push(json!({"role": "tool", "tool_name": name, "content": v.to_string()}));
                }
            }
        }
    }
    messages
}

#[async_trait]
impl Provider for Ollama {
    async fn complete(&self, system: &str, history: &[Message], tools: &[ToolSpec]) -> Result<ModelTurn> {
        let tool_defs: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({"type": "function",
                       "function": {"name": t.name, "description": t.description, "parameters": t.parameters}})
            })
            .collect();
        let msg = self
            .chat(json!({
                "model": self.model,
                "messages": ollama_messages(system, history),
                "tools": tool_defs,
                "stream": false,
                "options": {"num_ctx": self.num_ctx, "temperature": 0.2},
            }))
            .await?;

        let text = msg["content"].as_str().unwrap_or("").to_string();
        let mut calls = Vec::new();
        for c in msg["tool_calls"].as_array().cloned().unwrap_or_default() {
            let mut args = c["function"]["arguments"].clone();
            // some models return the arguments as a JSON string
            if let Some(s) = args.as_str() {
                if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                    args = parsed;
                }
            }
            calls.push(ToolCall { name: c["function"]["name"].as_str().unwrap_or("").to_string(), args });
        }
        Ok(ModelTurn { text, calls, raw: msg })
    }

    async fn complete_json(&self, system: &str, history: &[Message], schema: &Value) -> Result<Value> {
        let mut messages = ollama_messages(system, history);
        messages.push(json!({
            "role": "user",
            "content": "Stop investigating. Based on everything above, give your final answer now as JSON in the required format."
        }));
        let msg = self
            .chat(json!({
                "model": self.model,
                "messages": messages,
                "format": schema,
                "stream": false,
                "options": {"num_ctx": self.num_ctx, "temperature": 0.2},
            }))
            .await?;
        let text = msg["content"].as_str().unwrap_or("");
        serde_json::from_str(text).with_context(|| format!("model returned invalid JSON: {text}"))
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

/// Pick the backend: AUTORESOLVE_PROVIDER=gemini (default) | ollama.
/// `strong` selects the stronger-model override used by the tester, fixer and patch gate.
pub fn provider_from_env(strong: bool) -> Result<Box<dyn Provider>> {
    // the `_STRONG` variables configure the second role (tester, fixer, patch gate)
    // and fall back to the main ones when unset
    let var = if strong {
        std::env::var("AUTORESOLVE_PROVIDER_STRONG").or_else(|_| std::env::var("AUTORESOLVE_PROVIDER"))
    } else {
        std::env::var("AUTORESOLVE_PROVIDER")
    };
    let which = var.unwrap_or_else(|_| "gemini".into());
    match which.as_str() {
        "gemini" => Ok(Box::new(if strong {
            Gemini::from_env_role("AUTORESOLVE_MODEL_STRONG")?
        } else {
            Gemini::from_env()?
        })),
        "ollama" => Ok(Box::new(if strong {
            Ollama::from_env_role("AUTORESOLVE_OLLAMA_MODEL_STRONG")?
        } else {
            Ollama::from_env()?
        })),
        other => bail!("unknown AUTORESOLVE_PROVIDER `{other}` (use gemini or ollama)"),
    }
}

/// Server-suggested wait from a 429 body, e.g. "retryDelay": "13s" or "34.7s".
fn retry_delay(v: &Value) -> Option<u64> {
    v["error"]["details"]
        .as_array()?
        .iter()
        .find_map(|d| d["retryDelay"].as_str())
        .and_then(|s| s.trim_end_matches('s').parse::<f64>().ok())
        .map(|secs| secs.ceil() as u64 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_retry_delay_hint() {
        let v = json!({"error": {"details": [{"@type": "x"}, {"retryDelay": "13.2s"}]}});
        assert_eq!(retry_delay(&v), Some(15));
        assert_eq!(retry_delay(&json!({})), None);
    }
}