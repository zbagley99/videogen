//! A small blocking client for OpenAI-compatible chat completions.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::time::Duration;

/// How the server was asked to structure its answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Structure {
    /// `response_format: json_schema`: the server constrains generation to the schema.
    Schema,
    /// `response_format: json_object`: valid JSON, shape not enforced.
    JsonMode,
    /// No help from the server; the prompt asks for JSON.
    Plain,
}

pub struct Client {
    /// Base URL ending in `/v1`.
    pub base: String,
    /// The model to use; the server's first one when `None`.
    pub model: Option<String>,
    /// What worked last time, so later requests skip rejected options.
    pub structure: Structure,
    /// Send `enable_thinking: false` (cleared if the server rejects it).
    pub thinking_off: bool,
    agent: ureq::Agent,
}

impl Client {
    /// `http://host:port`, `…/v1` or `…/v1/` all work.
    pub fn new(url: &str) -> Self {
        let mut base = url.trim().trim_end_matches('/').to_string();
        if !base.starts_with("http") {
            base = format!("http://{base}");
        }
        if !base.ends_with("/v1") {
            base += "/v1";
        }
        let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(300))).http_status_as_error(false).build().into();
        Client { base, model: None, structure: Structure::Schema, thinking_off: true, agent }
    }

    /// Model ids the server offers.
    pub fn models(&self) -> Result<Vec<String>> {
        let url = format!("{}/models", self.base);
        let mut resp = self.agent.get(&url).call().with_context(|| format!("can't reach {url}"))?;
        if !resp.status().is_success() {
            bail!("{url} answered {}", resp.status());
        }
        let v: Value = resp.body_mut().read_json().with_context(|| format!("{url} didn't return JSON"))?;
        let ids: Vec<String> = v["data"].as_array().map(|a| a.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect()).unwrap_or_default();
        if ids.is_empty() {
            bail!("{url} lists no models");
        }
        Ok(ids)
    }

    pub fn model(&mut self) -> Result<String> {
        if let Some(m) = &self.model {
            return Ok(m.clone());
        }
        let m = self.models()?.remove(0);
        self.model = Some(m.clone());
        Ok(m)
    }

    /// One chat completion. With a schema, tries the strongest structuring the
    /// server accepts (schema, then JSON mode, then none) and remembers it.
    /// Reasoning models that think past `max_tokens` without answering get one
    /// retry with a bigger budget.
    pub fn chat(&mut self, messages: &[Value], schema: Option<&Value>, max_tokens: u32, temperature: f32) -> Result<String> {
        match self.chat_once(messages, schema, max_tokens, temperature) {
            Err(e) if e.to_string().contains("ran out of tokens") => self.chat_once(messages, schema, (max_tokens * 3).min(12_000), temperature),
            r => r,
        }
    }

    fn chat_once(&mut self, messages: &[Value], schema: Option<&Value>, max_tokens: u32, temperature: f32) -> Result<String> {
        let model = self.model()?;
        loop {
            let mut body = json!({
                "model": model,
                "messages": messages,
                "max_tokens": max_tokens,
                "temperature": temperature,
                "stream": false,
            });
            if self.thinking_off {
                // Ask reasoning models to skip thinking (vLLM, llama.cpp; others ignore it).
                body["chat_template_kwargs"] = json!({"enable_thinking": false});
            }
            match (schema, self.structure) {
                (Some(s), Structure::Schema) => body["response_format"] = json!({"type": "json_schema", "json_schema": {"name": "answer", "strict": true, "schema": s}}),
                (Some(_), Structure::JsonMode) => body["response_format"] = json!({"type": "json_object"}),
                _ => {}
            }
            let url = format!("{}/chat/completions", self.base);
            let mut resp = self.agent.post(&url).send_json(&body).with_context(|| format!("can't reach {url}"))?;
            let status = resp.status();
            let text = resp.body_mut().read_to_string().unwrap_or_default();
            if status.is_success() {
                let v: Value = serde_json::from_str(&text).with_context(|| format!("{url} returned something that isn't JSON"))?;
                let msg = &v["choices"][0]["message"];
                let content = msg["content"].as_str().unwrap_or("").to_string();
                if content.trim().is_empty() {
                    let why = v["choices"][0]["finish_reason"].as_str().unwrap_or("?");
                    let thought = msg["reasoning_content"].as_str().or(msg["reasoning"].as_str()).map(str::len).unwrap_or(0);
                    if why == "length" {
                        bail!("the model ran out of tokens before answering ({thought} characters of reasoning)");
                    }
                    bail!("the model returned an empty answer (finish reason: {why})");
                }
                return Ok(content);
            }
            // Some servers reject unknown fields: drop the thinking switch first.
            if self.thinking_off && (status.as_u16() == 400 || status.as_u16() == 422) && text.contains("chat_template_kwargs") {
                self.thinking_off = false;
                continue;
            }
            // The server may not know response_format: step down and retry.
            let rejected = status.as_u16() == 400 || status.as_u16() == 422 || status.as_u16() == 500;
            if schema.is_some() && rejected && self.structure != Structure::Plain {
                self.structure = if self.structure == Structure::Schema { Structure::JsonMode } else { Structure::Plain };
                continue;
            }
            return Err(anyhow!("{url} answered {status}: {}", text.chars().take(300).collect::<String>()));
        }
    }
}
