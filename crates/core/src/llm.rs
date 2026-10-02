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
}

pub struct Gemini {
    key: String,
    model: String,
    http: reqwest::Client,
    calls: AtomicUsize,
}

impl Gemini {
    pub fn from_env() -> Result<Self> {
        let key = std::env::var("GEMINI_API_KEY").context("set the GEMINI_API_KEY environment variable")?;
        let model = std::env::var("AUTORESOLVE_MODEL").unwrap_or_else(|_| "gemini-3.8-flash".into());
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(180))
            .local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
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
        /// Successful API calls made by this client so far.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
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
                     has its own quota).",
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