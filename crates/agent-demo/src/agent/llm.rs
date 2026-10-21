//! An agent backed by a real model (any OpenAI-compatible API, Melious by default) with the
//! tools of [`crate::tools`]: it streams thinking and text, calls tools in a loop, and keeps a
//! history per conversation so a chat continues across runs.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use agent_demo_proto::agentdemo::v1::{self as pb, __buffa::oneof::run_event::Kind};
use serde_json::{Value, json};

use super::{Agent, AgentError, AgentFuture, Request};
use crate::llm::{Client, Completion, Delta};
use crate::run::RunHandle;
use crate::settings::{LlmConfig, Settings};
use crate::tools::Tools;

/// Tool output longer than this is cut before it goes to the model and the UI.
const MAX_TOOL_OUTPUT: usize = 24_000;
/// Conversations kept in memory; the oldest is dropped beyond this.
const MAX_CONVERSATIONS: usize = 200;

const SCENARIOS: [(&str, &str, &str, &str); 2] = [
    (
        "chat",
        "Chat with the LLM",
        "Talk to the configured model. It can read this machine, edit the shared sheet and compile WebAssembly widgets.",
        "What is this machine doing right now? Put the five busiest processes into the sheet.",
    ),
    (
        "monitor",
        "Live system monitor",
        "The model lays out a sheet, then compiles a WebAssembly widget that keeps it live from the server's metrics.",
        "Set up a live system monitor in the sheet: a titled, formatted table with CPU (total and per core), memory, \
         swap, load averages, disk and network rates. Then compile a WebAssembly widget that refreshes every value once \
         a second, and keeps a rolling history of the last 10 CPU samples next to it.",
    ),
];

#[derive(Debug)]
pub struct LlmAgent {
    settings: Arc<Settings>,
    tools: Arc<Tools>,
    client: Client,
    conversations: Arc<Mutex<Conversations>>,
}

#[derive(Debug, Default)]
struct Conversations {
    by_id: HashMap<String, Vec<Value>>,
    /// Ids, least recently used first.
    order: Vec<String>,
}

impl Conversations {
    fn get(&self, id: &str) -> Vec<Value> {
        self.by_id.get(id).cloned().unwrap_or_default()
    }

    fn put(&mut self, id: &str, history: Vec<Value>) {
        if id.is_empty() {
            return;
        }
        self.order.retain(|o| o != id);
        self.order.push(id.to_owned());
        self.by_id.insert(id.to_owned(), history);
        while self.order.len() > MAX_CONVERSATIONS {
            let oldest = self.order.remove(0);
            self.by_id.remove(&oldest);
        }
    }
}

impl LlmAgent {
    pub fn new(settings: Arc<Settings>, tools: Arc<Tools>) -> Self {
        Self {
            settings,
            tools,
            client: Client::new(),
            conversations: Arc::default(),
        }
    }

    fn model(cfg: &LlmConfig, requested: &str) -> String {
        let requested = requested.trim();
        if requested.is_empty() {
            cfg.model.clone()
        } else {
            requested.to_owned()
        }
    }

    /// The conversation so far, under the current system prompt (an edited briefing applies to
    /// the next turn of an ongoing chat too).
    fn history(&self, cfg: &LlmConfig, conversation_id: &str) -> Vec<Value> {
        let mut history = self
            .conversations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(conversation_id);
        let mut system = cfg.briefing().to_owned();
        if !cfg.system_prompt.trim().is_empty() {
            system.push_str("\n\n");
            system.push_str(cfg.system_prompt.trim());
        }
        let system = json!({ "role": "system", "content": system });
        match history.first_mut() {
            Some(first) if first["role"] == "system" => *first = system,
            _ => history.insert(0, system),
        }
        history
    }
}

impl Agent for LlmAgent {
    fn scenarios(&self) -> Vec<pb::Scenario> {
        SCENARIOS
            .iter()
            .map(|&(id, title, description, prompt)| pb::Scenario {
                id: id.to_owned(),
                title: title.to_owned(),
                description: description.to_owned(),
                prompt: prompt.to_owned(),
                ..Default::default()
            })
            .collect()
    }

    fn model_for(&self, request: &Request) -> String {
        Self::model(&self.settings.llm(), &request.model)
    }

    fn run(&self, request: Request, run: RunHandle) -> AgentFuture {
        let cfg = self.settings.llm();
        let mut history = self.history(&cfg, &request.conversation_id);
        history.push(json!({ "role": "user", "content": request.prompt }));
        let mut turn = Turn {
            model: Self::model(&cfg, &request.model),
            cfg,
            run,
            tools: Arc::clone(&self.tools),
            client: self.client.clone(),
            history,
            usage: pb::Usage::default(),
            next_id: AtomicU64::new(1),
        };
        let conversations = Arc::clone(&self.conversations);
        let conversation_id = request.conversation_id;
        Box::pin(async move {
            let result = turn.play().await;
            // The history ends at the last consistent point (see `Turn::play`), even after an
            // error or a cancel, so the next turn of the conversation can go on from it.
            conversations
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .put(&conversation_id, turn.history);
            result
        })
    }
}

/// One run: the model and tools in a loop until the model answers without tool calls.
struct Turn {
    run: RunHandle,
    cfg: LlmConfig,
    model: String,
    tools: Arc<Tools>,
    client: Client,
    /// Ends with a user message or a complete tool round, never with unanswered tool calls.
    history: Vec<Value>,
    usage: pb::Usage,
    next_id: AtomicU64,
}

impl Turn {
    async fn play(&mut self) -> Result<(), AgentError> {
        let rounds = self.cfg.max_tool_rounds.max(1);
        for round in 0..=rounds {
            // The last round offers no tools, so the model has to answer.
            let offer_tools = round < rounds;
            let answer = self.complete(offer_tools).await?;
            let calls = answer.tool_calls.clone();
            if calls.is_empty() {
                crate::activity::record(
                    "answer",
                    json!({
                        "run_id": self.run.id(),
                        "text": crate::activity::cut(&answer.text),
                        "input_tokens": self.usage.input_tokens,
                        "output_tokens": self.usage.output_tokens,
                    }),
                );
                self.history.push(answer.to_message());
                return Ok(());
            }
            let mut round_history = vec![answer.to_message()];
            for call in &calls {
                let output = self
                    .call_tool(&call.id, &call.name, &call.arguments)
                    .await?;
                round_history
                    .push(json!({ "role": "tool", "tool_call_id": call.id, "content": output }));
            }
            self.history.extend(round_history);
        }
        Err(AgentError::Failed(
            "the model kept calling tools after the last round".into(),
        ))
    }

    /// Streams one completion into thinking and text blocks.
    async fn complete(&mut self, offer_tools: bool) -> Result<Completion, AgentError> {
        let mut blocks = Blocks {
            run: self.run.clone(),
            ids: &self.next_id,
            open: None,
        };
        let tools = if offer_tools {
            self.tools.definitions()
        } else {
            &[]
        };
        let call = self
            .client
            .complete(&self.cfg, &self.model, &self.history, tools, |delta| {
                blocks.push(delta);
            });
        let result = self.run.unless_cancelled(call).await;
        blocks.close();
        let answer = result?.map_err(|e| AgentError::Failed(e.to_string()))?;
        if let Some(u) = &answer.usage {
            self.usage.input_tokens += u.input_tokens;
            self.usage.output_tokens += u.output_tokens;
            self.run.emit(
                "",
                Kind::UsageUpdated(Box::new(pb::UsageUpdated {
                    usage: self.usage.clone().into(),
                    ..Default::default()
                })),
            );
        }
        if answer.finish_reason == "length" && answer.tool_calls.is_empty() {
            tracing::warn!(run = %self.run.id(), "answer cut at max_tokens");
        }
        Ok(answer)
    }

    async fn call_tool(
        &self,
        call_id: &str,
        name: &str,
        arguments: &str,
    ) -> Result<String, AgentError> {
        self.run.emit(
            "",
            Kind::ToolCallStarted(Box::new(pb::ToolCallStarted {
                call_id: call_id.to_owned(),
                tool: name.to_owned(),
                input_json: arguments.to_owned(),
                ..Default::default()
            })),
        );
        let started = Instant::now();
        let output = if name == "update_plan" {
            self.update_plan(arguments)
        } else {
            let tools = Arc::clone(&self.tools);
            let (name, arguments) = (name.to_owned(), arguments.to_owned());
            // Tools lock the sheet and compile WebAssembly: keep them off the async workers.
            let job = tokio::task::spawn_blocking(move || tools.call(&name, &arguments));
            self.run
                .unless_cancelled(job)
                .await?
                .unwrap_or_else(|e| crate::tools::Output {
                    text: format!("the tool crashed: {e}"),
                    is_error: true,
                })
        };
        let text = cut(&output.text, MAX_TOOL_OUTPUT);
        crate::activity::record(
            "tool_call",
            json!({
                "run_id": self.run.id(),
                "tool": name,
                "arguments": crate::activity::cut(arguments),
                "output": crate::activity::cut(&text),
                "is_error": output.is_error,
                "ms": started.elapsed().as_millis(),
            }),
        );
        self.run.emit(
            "",
            Kind::ToolCallFinished(Box::new(pb::ToolCallFinished {
                call_id: call_id.to_owned(),
                output: text.clone(),
                is_error: output.is_error,
                duration_ms: u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX),
                ..Default::default()
            })),
        );
        Ok(if output.is_error {
            format!("ERROR: {text}")
        } else {
            text
        })
    }

    /// `update_plan {steps: [{title, status}]}`: shows the plan in the UI.
    fn update_plan(&self, arguments: &str) -> crate::tools::Output {
        let parsed: Result<Value, _> = serde_json::from_str(arguments);
        let Some(steps) = parsed
            .ok()
            .and_then(|v| v.get("steps").and_then(Value::as_array).cloned())
        else {
            return crate::tools::Output {
                text: "expected {\"steps\": [{\"title\", \"status\"}]}".into(),
                is_error: true,
            };
        };
        let steps: Vec<_> = steps
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let status = match s.get("status").and_then(Value::as_str).unwrap_or("pending") {
                    "in_progress" | "active" => pb::StepStatus::STEP_STATUS_IN_PROGRESS,
                    "done" | "completed" => pb::StepStatus::STEP_STATUS_DONE,
                    "skipped" => pb::StepStatus::STEP_STATUS_SKIPPED,
                    _ => pb::StepStatus::STEP_STATUS_PENDING,
                };
                pb::PlanStep {
                    id: format!("s{i}"),
                    title: s
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    status: status.into(),
                    ..Default::default()
                }
            })
            .collect();
        let count = steps.len();
        self.run.emit(
            "",
            Kind::PlanUpdated(Box::new(pb::PlanUpdated {
                steps,
                ..Default::default()
            })),
        );
        crate::tools::Output {
            text: format!("plan shown ({count} steps)"),
            is_error: false,
        }
    }
}

/// The open output block of a completion: thinking and text alternate, each in its own block.
struct Blocks<'a> {
    run: RunHandle,
    ids: &'a AtomicU64,
    open: Option<(pb::BlockKind, String)>,
}

impl Blocks<'_> {
    fn push(&mut self, delta: Delta) {
        let (kind, text) = match delta {
            Delta::Reasoning(t) => (pb::BlockKind::BLOCK_KIND_THINKING, t),
            Delta::Text(t) => (pb::BlockKind::BLOCK_KIND_TEXT, t),
        };
        if self.open.as_ref().is_none_or(|(k, _)| *k != kind) {
            self.close();
            let block_id = format!("b{}", self.ids.fetch_add(1, Ordering::Relaxed));
            self.run.emit(
                "",
                Kind::BlockStarted(Box::new(pb::BlockStarted {
                    block_id: block_id.clone(),
                    kind: kind.into(),
                    ..Default::default()
                })),
            );
            self.open = Some((kind, block_id));
        }
        let (_, block_id) = self.open.as_ref().expect("opened above");
        self.run.emit(
            "",
            Kind::BlockDelta(Box::new(pb::BlockDelta {
                block_id: block_id.clone(),
                text,
                ..Default::default()
            })),
        );
    }

    fn close(&mut self) {
        if let Some((_, block_id)) = self.open.take() {
            self.run.emit(
                "",
                Kind::BlockFinished(Box::new(pb::BlockFinished {
                    block_id,
                    ..Default::default()
                })),
            );
        }
    }
}

fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… ({} more bytes cut)", &s[..end], s.len() - end)
}

/// The `update_plan` tool, offered next to [`Tools::definitions`].
pub fn plan_definition() -> Value {
    json!({ "type": "function", "function": {
        "name": "update_plan",
        "description": "Show the user your plan as a checklist; call again with new statuses as you progress.",
        "parameters": { "type": "object", "required": ["steps"], "properties": {
            "steps": { "type": "array", "items": { "type": "object", "required": ["title", "status"], "properties": {
                "title": { "type": "string" },
                "status": { "type": "string", "enum": ["pending", "in_progress", "done", "skipped"] },
            }}},
        }},
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversations_keep_the_most_recent() {
        let mut c = Conversations::default();
        for i in 0..=MAX_CONVERSATIONS {
            c.put(&format!("c{i}"), vec![json!(i)]);
        }
        assert_eq!(c.get("c0"), [] as [serde_json::Value; 0]);
        assert_eq!(
            c.get(&format!("c{MAX_CONVERSATIONS}")),
            [json!(MAX_CONVERSATIONS)]
        );
        c.put("", vec![json!(1)]);
        assert_eq!(c.by_id.len(), MAX_CONVERSATIONS);
    }

    #[test]
    fn long_output_is_cut() {
        assert!(cut(&"x".repeat(30), 10).starts_with("xxxxxxxxxx\n… (20 more"));
    }
}
