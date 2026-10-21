//! An offline agent that plays canned scenarios with realistic pacing: streamed thinking and
//! text, a live plan, tool calls (one fails and is retried, one needs a human's approval) and
//! sub-agents working in parallel. It shows every feature of the UI without an API key.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use agent_demo_proto::agentdemo::v1::{self as pb, __buffa::oneof::run_event::Kind};
use futures_util::future::try_join_all;
use serde_json::{Value, json};

use super::{Agent, AgentError, AgentFuture, Request};
use crate::run::RunHandle;

use pb::StepStatus::{
    STEP_STATUS_DONE as DONE, STEP_STATUS_IN_PROGRESS as ACTIVE, STEP_STATUS_SKIPPED as SKIPPED,
};

/// Time between two streamed text chunks at speed 1.
const CHUNK: Duration = Duration::from_millis(35);

const SCENARIOS: [(&str, &str, &str, &str); 4] = [
    (
        "research",
        "Research & answer",
        "Plans, searches the web, reads sources and streams a cited answer.",
        "What are the trade-offs between server-sent events, WebSockets and Connect server streams?",
    ),
    (
        "approval",
        "Human in the loop",
        "Proposes a config change and waits for your approval before it writes anything.",
        "Raise the API rate limit for the staging environment to 200 requests per second.",
    ),
    (
        "subagents",
        "Parallel sub-agents",
        "Splits the task, runs three sub-agents at once and merges their findings.",
        "Review this service for security, performance and test coverage.",
    ),
    (
        "recovery",
        "Tool error & recovery",
        "A tool call fails; the agent reads the error, adjusts and retries.",
        "How many orders did we ship last week?",
    ),
];

#[derive(Debug, Clone, Copy)]
pub struct ScriptedAgent {
    /// Multiplies every delay: 1.0 for the demo, 0.0 in tests.
    speed: f64,
}

impl ScriptedAgent {
    pub fn new(speed: f64) -> Self {
        Self {
            speed: speed.max(0.0),
        }
    }
}

impl Agent for ScriptedAgent {
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

    fn run(&self, request: Request, run: RunHandle) -> AgentFuture {
        let Request {
            scenario_id,
            prompt,
            ..
        } = request;
        let act = Act::new(run, self.speed, &prompt);
        match scenario_id.as_str() {
            "research" => Box::pin(research(act, prompt)),
            "approval" => Box::pin(approval(act)),
            "subagents" => Box::pin(subagents(act)),
            "recovery" => Box::pin(recovery(act)),
            _ => Box::pin(async move { Err(AgentError::UnknownScenario(scenario_id)) }),
        }
    }
}

async fn research(act: Act, prompt: String) -> Result<(), AgentError> {
    act.think(
        "The user wants a comparison of three streaming transports. I should check current \
               browser support and how each handles reconnects and binary payloads, then answer \
               with a short table.",
    )
    .await?;
    act.plan(&[
        "Search for current comparisons",
        "Read the two best sources",
        "Write the answer",
    ]);
    act.step(0, ACTIVE);
    act.tool("web_search", json!({ "query": "SSE vs WebSocket vs Connect server streaming", "limit": 5 }),
             json!({ "results": [
                 { "title": "Streaming RPCs over HTTP/1.1 and HTTP/2", "url": "https://connectrpc.com/docs/protocol" },
                 { "title": "Server-sent events (HTML Living Standard)", "url": "https://html.spec.whatwg.org/multipage/server-sent-events.html" },
                 { "title": "The WebSocket Protocol (RFC 6455)", "url": "https://www.rfc-editor.org/rfc/rfc6455" } ] }),
             900).await?;
    act.step(0, DONE);
    act.step(1, ACTIVE);
    let pages = [
        ("https://connectrpc.com/docs/protocol", 1200),
        ("https://www.rfc-editor.org/rfc/rfc6455", 800),
    ];
    for (url, ms) in pages {
        act.tool(
            "fetch_page",
            json!({ "url": url }),
            json!({ "url": url, "status": 200, "words": ms * 3 }),
            ms,
        )
        .await?;
    }
    act.step(1, DONE);
    act.step(2, ACTIVE);
    act.say(&format!(
        "You asked: *{prompt}*\n\n\
        | | SSE | WebSocket | Connect stream |\n|---|---|---|---|\n\
        | Direction | server → client | both | server → client (unary up) |\n\
        | Payload | text only | text or binary | protobuf binary or JSON |\n\
        | Reconnect | built in (`Last-Event-ID`) | your code | your code, resume by sequence |\n\
        | Schema | none | none | `.proto`, typed clients |\n\n\
        For an agent UI, a **Connect server stream** gives typed events and works through plain \
        HTTP proxies; resuming from a sequence number (as this demo does) covers reconnects."
    ))
    .await?;
    act.step(2, DONE);
    Ok(())
}

async fn approval(act: Act) -> Result<(), AgentError> {
    act.plan(&[
        "Read the current config",
        "Propose the change",
        "Apply it",
        "Verify",
    ]);
    act.step(0, ACTIVE);
    act.tool(
        "read_file",
        json!({ "path": "deploy/staging/ratelimit.toml" }),
        json!({ "content": "[api]\nrequests_per_second = 50\nburst = 100\n" }),
        400,
    )
    .await?;
    act.step(0, DONE);
    act.step(1, ACTIVE);
    act.think(
        "The limit is 50 rps with a burst of 100. Raising it to 200 means the burst should \
               go up too, or it would cap the first second below the new rate.",
    )
    .await?;
    act.say("I'll set `requests_per_second = 200` and `burst = 400`.")
        .await?;
    act.step(1, DONE);
    act.step(2, ACTIVE);
    let write = act.tool_with_approval(
        "write_file",
        json!({ "path": "deploy/staging/ratelimit.toml", "content": "[api]\nrequests_per_second = 200\nburst = 400\n" }),
        "Writes the staging rate limit config; staging picks it up on the next deploy.",
        json!({ "bytes_written": 46 }),
        300,
    ).await?;
    let Some(_) = write else {
        act.step(2, SKIPPED);
        act.step(3, SKIPPED);
        act.say("Understood, I left the config unchanged.").await?;
        return Ok(());
    };
    act.step(2, DONE);
    act.step(3, ACTIVE);
    act.tool(
        "run_command",
        json!({ "cmd": "ratelimit-lint deploy/staging/ratelimit.toml" }),
        json!({ "exit_code": 0, "stdout": "ok: 1 file, 0 warnings" }),
        700,
    )
    .await?;
    act.step(3, DONE);
    act.say("Done: staging now allows **200 rps** (burst 400); the linter is happy.")
        .await?;
    Ok(())
}

async fn subagents(act: Act) -> Result<(), AgentError> {
    act.think("Three independent reviews: they can run in parallel, each with its own context.")
        .await?;
    act.plan(&[
        "Security review",
        "Performance review",
        "Test coverage review",
        "Merge findings",
    ]);
    let reviews = [
        (
            "security",
            "Security reviewer",
            "grep_code",
            json!({ "pattern": "unwrap\\(|unsafe" }),
            json!({ "matches": 3 }),
            "Found 3 `unwrap()`s on request input; no `unsafe`.",
        ),
        (
            "performance",
            "Performance reviewer",
            "profile",
            json!({ "endpoint": "/v1/orders", "seconds": 10 }),
            json!({ "p99_ms": 184, "hot": "serde_json::to_vec" }),
            "p99 is 184 ms; JSON encoding dominates.",
        ),
        (
            "tests",
            "Coverage reviewer",
            "coverage",
            json!({ "crate": "orders" }),
            json!({ "lines": 0.71, "untested": ["refunds.rs"] }),
            "71 % line coverage; refunds are untested.",
        ),
    ];
    let work =
        reviews
            .into_iter()
            .enumerate()
            .map(|(i, (id, name, tool, input, output, summary))| {
                let act = act.clone();
                async move {
                    act.step(i, ACTIVE);
                    let sub = act.subagent(id, name, &format!("{name}: look at the service"));
                    sub.think(&format!(
                        "Start with the cheapest signal for the {id} review."
                    ))
                    .await?;
                    sub.tool(tool, input, output, 700 + 400 * i as u64).await?;
                    sub.say(summary).await?;
                    act.subagent_done(id, summary);
                    act.step(i, DONE);
                    Ok::<_, AgentError>(summary)
                }
            });
    let findings = try_join_all(work).await?;
    act.step(3, ACTIVE);
    let list = findings
        .iter()
        .fold(String::new(), |list, f| list + "- " + f + "\n");
    act.say(&format!("All three reviews are in:\n\n{list}\nFix the `unwrap()`s first; they can crash the service.")).await?;
    act.step(3, DONE);
    Ok(())
}

async fn recovery(act: Act) -> Result<(), AgentError> {
    act.plan(&["Query the orders database", "Answer"]);
    act.step(0, ACTIVE);
    let first = act.tool_result("sql_query",
        json!({ "sql": "SELECT count(*) FROM orders WHERE shipped_at > now() - interval '7 days'" }),
        Err(json!({ "error": "column \"shipped_at\" does not exist", "hint": "did you mean \"shipped_on\"?" })),
        500).await?;
    if let Err(error) = first {
        act.think(&format!(
            "The query failed ({}). The column is `shipped_on`, a date. And last \
                            week means the previous Monday to Sunday, not the last 7 days.",
            error["error"]
        ))
        .await?;
    }
    act.tool("sql_query",
        json!({ "sql": "SELECT count(*) FROM orders WHERE shipped_on >= date_trunc('week', now()) - interval '1 week' \
                        AND shipped_on < date_trunc('week', now())" }),
        json!({ "rows": [{ "count": 1284 }] }), 600).await?;
    act.step(0, DONE);
    act.step(1, ACTIVE);
    act.say("We shipped **1,284 orders** last week (Monday to Sunday).")
        .await?;
    act.step(1, DONE);
    Ok(())
}

/// One agent's view of the run: the top-level agent, or a sub-agent (same run, own `agent_id`).
#[derive(Debug, Clone)]
struct Act {
    shared: Arc<Shared>,
    agent_id: String,
}

#[derive(Debug)]
struct Shared {
    run: RunHandle,
    speed: f64,
    ids: AtomicU64,
    plan: Mutex<Vec<pb::PlanStep>>,
    input_tokens: AtomicU32,
    output_tokens: AtomicU32,
}

impl Act {
    fn new(run: RunHandle, speed: f64, prompt: &str) -> Self {
        let shared = Arc::new(Shared {
            run,
            speed,
            ids: AtomicU64::new(1),
            plan: Mutex::new(Vec::new()),
            input_tokens: AtomicU32::new(tokens(prompt) + 600),
            output_tokens: AtomicU32::new(0),
        });
        Self {
            shared,
            agent_id: String::new(),
        }
    }

    fn emit(&self, kind: Kind) {
        self.shared.run.emit(&self.agent_id, kind);
    }

    fn next_id(&self, prefix: &str) -> String {
        format!(
            "{prefix}-{}",
            self.shared.ids.fetch_add(1, Ordering::Relaxed)
        )
    }

    async fn pause(&self, ms: u64) -> Result<(), AgentError> {
        let duration = Duration::from_millis(ms).mul_f64(self.shared.speed);
        Ok(self.shared.run.sleep(duration).await?)
    }

    async fn think(&self, text: &str) -> Result<(), AgentError> {
        self.stream(pb::BlockKind::BLOCK_KIND_THINKING, text).await
    }

    async fn say(&self, text: &str) -> Result<(), AgentError> {
        self.stream(pb::BlockKind::BLOCK_KIND_TEXT, text).await
    }

    /// Streams `text` as one block, a few words per delta.
    async fn stream(&self, kind: pb::BlockKind, text: &str) -> Result<(), AgentError> {
        let block_id = self.next_id("block");
        self.emit(Kind::BlockStarted(Box::new(pb::BlockStarted {
            block_id: block_id.clone(),
            kind: kind.into(),
            ..Default::default()
        })));
        let words: Vec<&str> = text.split_inclusive(char::is_whitespace).collect();
        for chunk in words.chunks(2) {
            self.pause(u64::try_from(CHUNK.as_millis()).unwrap_or(u64::MAX))
                .await?;
            self.emit(Kind::BlockDelta(Box::new(pb::BlockDelta {
                block_id: block_id.clone(),
                text: chunk.concat(),
                ..Default::default()
            })));
        }
        self.emit(Kind::BlockFinished(Box::new(pb::BlockFinished {
            block_id,
            ..Default::default()
        })));
        self.usage(0, tokens(text));
        Ok(())
    }

    fn plan(&self, titles: &[&str]) {
        let steps: Vec<_> = titles
            .iter()
            .enumerate()
            .map(|(i, title)| pb::PlanStep {
                id: format!("step-{i}"),
                title: (*title).to_owned(),
                status: pb::StepStatus::STEP_STATUS_PENDING.into(),
                ..Default::default()
            })
            .collect();
        *self
            .shared
            .plan
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = steps;
        self.emit_plan();
    }

    fn step(&self, index: usize, status: pb::StepStatus) {
        if let Some(step) = self
            .shared
            .plan
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(index)
        {
            step.status = status.into();
        }
        self.emit_plan();
    }

    fn emit_plan(&self) {
        let steps = self
            .shared
            .plan
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        // The plan belongs to the top-level agent, whoever updates it.
        self.shared.run.emit(
            "",
            Kind::PlanUpdated(Box::new(pb::PlanUpdated {
                steps,
                ..Default::default()
            })),
        );
    }

    async fn tool(
        &self,
        tool: &str,
        input: Value,
        output: Value,
        ms: u64,
    ) -> Result<Value, AgentError> {
        self.tool_result(tool, input, Ok(output), ms)
            .await?
            .map_err(|e| AgentError::Failed(format!("{tool} failed: {e}")))
    }

    /// Calls a tool whose outcome is `result`; an `Err` is a tool error the agent can react to.
    async fn tool_result(
        &self,
        tool: &str,
        input: Value,
        result: Result<Value, Value>,
        ms: u64,
    ) -> Result<Result<Value, Value>, AgentError> {
        let call_id = self.start_tool(tool, &input);
        self.finish_tool(&call_id, result, ms).await
    }

    /// Like [`Act::tool`], but asks a human first; `None` when they reject it.
    async fn tool_with_approval(
        &self,
        tool: &str,
        input: Value,
        rationale: &str,
        output: Value,
        ms: u64,
    ) -> Result<Option<Value>, AgentError> {
        let call_id = self.start_tool(tool, &input);
        let decision = self
            .shared
            .run
            .request_approval(&self.agent_id, &call_id, rationale)
            .await?;
        if !decision.approved {
            let reason = if decision.reason.is_empty() {
                "rejected by the user".to_owned()
            } else {
                decision.reason
            };
            self.finish_tool(&call_id, Err(json!({ "rejected": reason })), 0)
                .await?
                .ok();
            return Ok(None);
        }
        Ok(self.finish_tool(&call_id, Ok(output), ms).await?.ok())
    }

    fn start_tool(&self, tool: &str, input: &Value) -> String {
        let call_id = self.next_id("call");
        self.emit(Kind::ToolCallStarted(Box::new(pb::ToolCallStarted {
            call_id: call_id.clone(),
            tool: tool.to_owned(),
            input_json: input.to_string(),
            ..Default::default()
        })));
        self.usage(0, tokens(&input.to_string()));
        call_id
    }

    async fn finish_tool(
        &self,
        call_id: &str,
        result: Result<Value, Value>,
        ms: u64,
    ) -> Result<Result<Value, Value>, AgentError> {
        let started = Instant::now();
        self.pause(ms).await?;
        let (output, is_error) = match &result {
            Ok(v) => (v, false),
            Err(v) => (v, true),
        };
        self.emit(Kind::ToolCallFinished(Box::new(pb::ToolCallFinished {
            call_id: call_id.to_owned(),
            output: serde_json::to_string_pretty(output).unwrap_or_default(),
            is_error,
            duration_ms: u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX),
            ..Default::default()
        })));
        // The tool result goes back into the model's context.
        self.usage(tokens(&output.to_string()), 0);
        Ok(result)
    }

    fn subagent(&self, id: &str, name: &str, task: &str) -> Act {
        self.emit(Kind::SubagentStarted(Box::new(pb::SubagentStarted {
            subagent_id: id.to_owned(),
            name: name.to_owned(),
            task: task.to_owned(),
            ..Default::default()
        })));
        Act {
            shared: Arc::clone(&self.shared),
            agent_id: id.to_owned(),
        }
    }

    fn subagent_done(&self, id: &str, summary: &str) {
        self.emit(Kind::SubagentFinished(Box::new(pb::SubagentFinished {
            subagent_id: id.to_owned(),
            summary: summary.to_owned(),
            ..Default::default()
        })));
    }

    fn usage(&self, input: u32, output: u32) {
        let usage = pb::Usage {
            input_tokens: self.shared.input_tokens.fetch_add(input, Ordering::Relaxed) + input,
            output_tokens: self
                .shared
                .output_tokens
                .fetch_add(output, Ordering::Relaxed)
                + output,
            ..Default::default()
        };
        self.shared.run.emit(
            "",
            Kind::UsageUpdated(Box::new(pb::UsageUpdated {
                usage: usage.into(),
                ..Default::default()
            })),
        );
    }
}

/// A rough token count (about 4 characters per token), good enough for a demo meter.
fn tokens(text: &str) -> u32 {
    u32::try_from(text.len().div_ceil(4)).unwrap_or(u32::MAX)
}
