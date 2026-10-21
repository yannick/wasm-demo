//! A streaming client for OpenAI-compatible chat completions (Melious, OVH AI Endpoints, vLLM…).
//!
//! `POST {base_url}/v1/chat/completions` with `stream: true`; the server-sent events carry
//! `delta.content`, `delta.reasoning_content` (DeepSeek, Qwen thinking), `delta.tool_calls`
//! (arguments arrive in pieces, keyed by `index`) and a final `usage` chunk.

use std::time::Duration;

use futures_util::StreamExt as _;
use serde_json::{Value, json};

use crate::settings::LlmConfig;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Longest silence between two chunks before the call fails (thinking models pause).
const READ_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_ERROR_BODY: usize = 600;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("no API key: set {0} (in the environment or ./.env) or store one in the settings")]
    NoKey(String),
    #[error("cannot reach the LLM at {url}: {source}")]
    Transport { url: String, source: reqwest::Error },
    #[error("the LLM answered {status}: {body}")]
    Status {
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("the LLM stream broke off: {0}")]
    Stream(String),
}

/// Streamed output, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delta {
    Reasoning(String),
    Text(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// JSON text, as the model wrote it.
    pub arguments: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// One whole answer.
#[derive(Debug, Clone, Default)]
pub struct Completion {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: String,
    pub usage: Option<Usage>,
}

impl Completion {
    /// The answer as a message for the next request's history.
    pub fn to_message(&self) -> Value {
        let mut msg = json!({ "role": "assistant", "content": self.text });
        if !self.tool_calls.is_empty() {
            msg["tool_calls"] = self
                .tool_calls
                .iter()
                .map(|c| json!({ "id": c.id, "type": "function", "function": { "name": c.name, "arguments": c.arguments } }))
                .collect();
            // DeepSeek wants its reasoning back within a tool-calling turn; others ignore it.
            if !self.reasoning.is_empty() {
                msg["reasoning_content"] = json!(self.reasoning);
            }
        }
        msg
    }
}

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
}

impl Client {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .user_agent(concat!("agent-demo/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("building the HTTP client");
        Self { http }
    }

    /// Streams one completion, calling `on_delta` for each piece of text as it arrives.
    pub async fn complete(
        &self,
        cfg: &LlmConfig,
        model: &str,
        messages: &[Value],
        tools: &[Value],
        mut on_delta: impl FnMut(Delta) + Send,
    ) -> Result<Completion, LlmError> {
        let key = cfg
            .resolved_key()
            .ok_or_else(|| LlmError::NoKey(cfg.api_key_env.clone()))?;
        let url = format!("{}/v1/chat/completions", cfg.base_url.trim_end_matches('/'));
        let mut body = json!({
            "model": model,
            "messages": messages,
            "stream": true,
            "stream_options": { "include_usage": true },
            "temperature": cfg.temperature,
            "max_tokens": cfg.max_tokens,
        });
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }
        let transport = |source| LlmError::Transport {
            url: url.clone(),
            source,
        };
        let res = self
            .http
            .post(&url)
            .bearer_auth(key)
            .json(&body)
            .send()
            .await
            .map_err(transport)?;
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            return Err(LlmError::Status {
                status,
                body: truncate(text.trim(), MAX_ERROR_BODY),
            });
        }
        let mut bytes = res.bytes_stream();
        let mut sse = SseParser::default();
        let mut acc = Accumulator::default();
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|e| LlmError::Stream(e.to_string()))?;
            for data in sse.push(&chunk) {
                if data == "[DONE]" {
                    return Ok(acc.finish());
                }
                let event: Value = serde_json::from_str(&data)
                    .map_err(|e| LlmError::Stream(format!("bad chunk: {e}")))?;
                if let Some(error) = event.get("error") {
                    return Err(LlmError::Stream(error.to_string()));
                }
                acc.apply(&event).into_iter().for_each(&mut on_delta);
            }
        }
        // Some servers end without [DONE]; what came is the answer.
        Ok(acc.finish())
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Splits a byte stream into server-sent event `data:` payloads.
#[derive(Debug, Default)]
struct SseParser {
    buf: Vec<u8>,
    /// Data lines of the event being read (an event may have several).
    data: Vec<String>,
}

impl SseParser {
    fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(self.data.join("\n"));
                    self.data.clear();
                }
            } else if let Some(data) = line.strip_prefix("data:") {
                self.data
                    .push(data.strip_prefix(' ').unwrap_or(data).to_owned());
            }
            // `event:`, `id:`, `retry:` and `:` comments carry nothing we use.
        }
        out
    }
}

/// Builds a [`Completion`] from chunks.
#[derive(Debug, Default)]
struct Accumulator {
    done: Completion,
}

impl Accumulator {
    fn apply(&mut self, event: &Value) -> Vec<Delta> {
        let mut deltas = Vec::new();
        if let Some(usage) = event.get("usage").filter(|u| u.is_object()) {
            let n = |k: &str| {
                usage
                    .get(k)
                    .and_then(Value::as_u64)
                    .and_then(|v| u32::try_from(v).ok())
                    .unwrap_or(0)
            };
            self.done.usage = Some(Usage {
                input_tokens: n("prompt_tokens"),
                output_tokens: n("completion_tokens"),
            });
        }
        let Some(choice) = event.get("choices").and_then(|c| c.get(0)) else {
            return deltas;
        };
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            reason.clone_into(&mut self.done.finish_reason);
        }
        let Some(delta) = choice.get("delta") else {
            return deltas;
        };
        let text = |k: &str| {
            delta
                .get(k)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        };
        if let Some(r) = text("reasoning_content").or_else(|| text("reasoning")) {
            self.done.reasoning.push_str(r);
            deltas.push(Delta::Reasoning(r.to_owned()));
        }
        if let Some(t) = text("content") {
            self.done.text.push_str(t);
            deltas.push(Delta::Text(t.to_owned()));
        }
        for call in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            self.tool_call(call);
        }
        deltas
    }

    fn tool_call(&mut self, call: &Value) {
        let calls = &mut self.done.tool_calls;
        let index = call
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|i| usize::try_from(i).ok())
            .unwrap_or(calls.len());
        if calls.len() <= index {
            calls.resize_with(index + 1, ToolCall::default);
        }
        let slot = &mut calls[index];
        let s = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or_default().to_owned();
        let id = s(call.get("id"));
        if !id.is_empty() {
            slot.id = id;
        }
        let function = call.get("function");
        slot.name.push_str(&s(function.and_then(|f| f.get("name"))));
        slot.arguments
            .push_str(&s(function.and_then(|f| f.get("arguments"))));
    }

    fn finish(mut self) -> Completion {
        self.done.tool_calls.retain(|c| !c.name.is_empty());
        for (i, call) in self.done.tool_calls.iter_mut().enumerate() {
            if call.id.is_empty() {
                call.id = format!("call_{i}");
            }
        }
        self.done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_lines_split_across_chunks() {
        let mut p = SseParser::default();
        assert_eq!(p.push(b"data: {\"a\""), [] as [std::string::String; 0]);
        assert_eq!(
            p.push(b":1}\r\n\r\n: keepalive\n\ndata: [DONE]\n\n"),
            ["{\"a\":1}", "[DONE]"]
        );
    }

    #[test]
    fn deltas_reasoning_and_pieced_tool_calls() {
        let chunks = [
            json!({"choices":[{"delta":{"role":"assistant","content":"","reasoning_content":null}}]}),
            json!({"choices":[{"delta":{"reasoning_content":"Need the tool."}}]}),
            json!({"choices":[{"delta":{"content":"Checking"}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"get_system_metrics","arguments":""}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":null,"function":{"name":null,"arguments":"{\"top\":"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"3}"}}]},"finish_reason":"tool_calls"}]}),
            json!({"choices":[],"usage":{"prompt_tokens":364,"completion_tokens":73}}),
        ];
        let mut acc = Accumulator::default();
        let deltas: Vec<_> = chunks.iter().flat_map(|c| acc.apply(c)).collect();
        assert_eq!(
            deltas,
            [
                Delta::Reasoning("Need the tool.".into()),
                Delta::Text("Checking".into())
            ]
        );
        let done = acc.finish();
        assert_eq!(
            done.tool_calls,
            [ToolCall {
                id: "c1".into(),
                name: "get_system_metrics".into(),
                arguments: "{\"top\":3}".into()
            }]
        );
        assert_eq!(done.finish_reason, "tool_calls");
        assert_eq!(
            done.usage,
            Some(Usage {
                input_tokens: 364,
                output_tokens: 73
            })
        );
        let msg = done.to_message();
        assert_eq!(msg["tool_calls"][0]["function"]["arguments"], "{\"top\":3}");
        assert_eq!(msg["reasoning_content"], "Need the tool.");
    }

    #[test]
    fn long_errors_are_cut_on_a_char_boundary() {
        assert_eq!(truncate("ééé", 3), "é…");
    }
}
