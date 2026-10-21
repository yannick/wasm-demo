//! The shared spreadsheet: a headless GridCraft engine plus the log of every command it ran.
//!
//! Whoever changes the sheet (an agent tool, a widget, a browser) goes through [`Sheet`], which
//! runs the change on the server's engine and appends the commands the engine journaled to the
//! log. Browsers replay the log into their own GridCraft (WebAssembly), so all copies converge;
//! the server's copy is what the agent reads.
//!
//! Widgets rewrite the same cells every second, so the log is compacted: a `cell.set` that a
//! later `cell.set` of the same cell overwrites, with only `cell.set`s in between, is dropped.
//! Ops keep their `seq` (the log gets gaps), so a follower's cursor stays valid and nobody has to
//! reload; a new client replays far fewer ops.
//!
//! Concurrent edits are last-writer-wins: each client applies remote ops as they arrive, so two
//! edits racing on one cell can leave clients briefly apart until the next write of that cell.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use agent_demo_proto::agentdemo::v1 as pb;
use futures_util::stream::{self, BoxStream, StreamExt as _};
use gridcraft_mcp::Headless;
use gridcraft_mcp::{ToolResult, call_tool};
use serde_json::{Value, json};
use tokio::sync::watch;

/// Commands that only read; the engine may journal them, but they change nothing.
const READ_ONLY: &[&str] = &[
    "sheet.read",
    "cell.get",
    "document.inspect",
    "selection.stats",
    "formulas.evaluate",
    "formulas.functions",
];

/// Commands nobody may run on the server's engine, by prefix: files and paths on the server,
/// network imports, the app and UI-only commands. Checked for the agent and for browsers alike.
const DENIED_PREFIXES: &[&str] = &[
    "file.",
    "app.",
    "ui.",
    "view.",
    "options.",
    "data.from",
    "data.getData",
    "data.csv",
    "data.tsv",
    "data.xlsx",
    "data.refresh",
    "data.cancelRefresh",
    "data.recentSources",
    "data.existingConnections",
    "data.connectionProperties",
    "data.dataSourceSettings",
    "data.queriesConnections",
    "data.powerQueryEditor",
    "data.stocks",
    "data.geography",
];

/// Exceptions to [`DENIED_PREFIXES`]: view settings stored in the workbook.
const ALLOWED: &[&str] = &[
    "view.freezePanes",
    "view.freezeTopRow",
    "view.freezeFirstColumn",
    "view.unfreezePanes",
    "view.gridlines",
    "view.headings",
    "view.showZeros",
    "view.zoom",
];

/// Compact when the log grows past this many ops.
const COMPACT_AT: usize = 4_096;

fn denied(command: &str) -> bool {
    !ALLOWED.contains(&command) && DENIED_PREFIXES.iter().any(|p| command.starts_with(p))
}

#[derive(Debug)]
pub struct Sheet {
    /// Process start, so a log id never repeats across restarts.
    boot: u128,
    state: Mutex<State>,
    /// Bumped on every change; followers wait for it.
    version: watch::Sender<u64>,
}

struct State {
    engine: Headless,
    /// Ascending `seq`, with gaps where compaction dropped ops.
    ops: Vec<pb::SheetOp>,
    next_seq: u64,
    /// `ops.len()` after the last compaction; the next one waits for the log to grow again.
    compacted_len: usize,
    /// Bumped by a reset; a follower from an older epoch starts over.
    epoch: u64,
}

impl State {
    fn new(epoch: u64) -> Self {
        Self {
            engine: Headless::new(),
            ops: Vec::new(),
            next_seq: 0,
            compacted_len: 0,
            epoch,
        }
    }

    /// Index of the first op with `seq >= from`.
    fn index_of(&self, from: u64) -> usize {
        self.ops.partition_point(|op| op.seq < from)
    }
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State")
            .field("ops", &self.ops.len())
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

impl Sheet {
    pub fn new() -> Arc<Self> {
        let boot = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        Arc::new(Self {
            boot,
            state: Mutex::new(State::new(0)),
            version: watch::channel(0).0,
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs a gridcraft MCP tool (`read_range`, `write_range`, `format_range`, …) for `origin`.
    pub fn tool(&self, origin: &str, name: &str, args: &Value) -> ToolResult {
        if name == "execute_command"
            && let Some(command) = args.get("command").and_then(Value::as_str)
            && denied(command)
        {
            return ToolResult::error(format!(
                "execute_command: `{command}` is not available here"
            ));
        }
        let mut state = self.lock();
        let result = call_tool(&mut state.engine, name, args);
        self.record(&mut state, origin);
        result
    }

    /// Several `cell.set`s as one change (a widget tick); returns how many were written.
    pub fn set_cells(&self, origin: &str, cells: &[(String, String)]) -> Result<usize, String> {
        let mut state = self.lock();
        let mut first_error = None;
        for (cell, input) in cells {
            if let Err(e) = state
                .engine
                .session
                .execute_headless("cell.set", json!({ "cell": cell, "input": input }))
            {
                first_error.get_or_insert(format!("{cell}: {e}"));
            }
        }
        let written = self.record(&mut state, origin);
        first_error.map_or(Ok(written), Err)
    }

    /// Applies what a browser ran locally; returns the ops the engine refused.
    pub fn apply_client(&self, client_id: &str, ops: &[pb::SheetOp]) -> Vec<String> {
        let origin = format!("client:{client_id}");
        let mut state = self.lock();
        let mut rejected = Vec::new();
        for op in ops {
            if denied(&op.command) {
                rejected.push(format!("{}: not available on the server", op.command));
                continue;
            }
            let params = serde_json::from_str(&op.params_json).unwrap_or_else(|_| json!({}));
            if let Err(e) = state.engine.session.execute_headless(&op.command, params) {
                rejected.push(format!("{}: {e}", op.command));
            }
        }
        self.record(&mut state, &origin);
        rejected
    }

    /// Starts over with a blank workbook.
    pub fn reset(&self) {
        let mut state = self.lock();
        let epoch = state.epoch + 1;
        *state = State::new(epoch);
        drop(state);
        self.version.send_modify(|v| *v += 1);
    }

    /// Moves what the engine journaled into the log; returns the number of ops logged.
    fn record(&self, state: &mut State, origin: &str) -> usize {
        let journal = std::mem::take(&mut state.engine.session.journal);
        let before = state.ops.len();
        for (command, params) in journal {
            if READ_ONLY.contains(&command.as_str()) {
                continue;
            }
            let seq = state.next_seq;
            state.next_seq += 1;
            state.ops.push(pb::SheetOp {
                seq,
                command,
                params_json: params.to_string(),
                origin: origin.to_owned(),
                ..Default::default()
            });
        }
        let added = state.ops.len() - before;
        if added > 0 {
            self.version.send_modify(|v| *v += 1);
        }
        if state.ops.len() >= COMPACT_AT && state.ops.len() >= state.compacted_len * 2 {
            let len = state.ops.len();
            compact(&mut state.ops);
            state.compacted_len = state.ops.len().max(COMPACT_AT / 2);
            tracing::debug!(from = len, to = state.ops.len(), "sheet log compacted");
        }
        added
    }

    fn log_id(&self, epoch: u64) -> String {
        format!("{:x}-{epoch}", self.boot)
    }

    /// The log from `from_seq`, then every change as it happens. After a reset, or when the
    /// client followed another log (`log_id`, empty for a first visit), the stream sends `reset`
    /// and the log from 0.
    pub fn follow(
        self: Arc<Self>,
        from_seq: u64,
        log_id: &str,
    ) -> BoxStream<'static, pb::WatchSheetResponse> {
        let changes = self.version.subscribe();
        let current = self.lock().epoch;
        // An impossible epoch makes the first batch a reset.
        let epoch = if log_id.is_empty() || log_id == self.log_id(current) {
            current
        } else {
            u64::MAX
        };
        stream::unfold(
            (self, changes, epoch, from_seq),
            |(sheet, mut changes, mut epoch, mut cursor)| async move {
                loop {
                    let batch = {
                        let state = sheet.lock();
                        let reset = state.epoch != epoch;
                        if reset {
                            epoch = state.epoch;
                            cursor = 0;
                        }
                        let start = state.index_of(cursor);
                        cursor = state.next_seq;
                        (reset || start < state.ops.len()).then(|| pb::WatchSheetResponse {
                            ops: state.ops[start..].to_vec(),
                            reset,
                            log_id: sheet.log_id(epoch),
                            ..Default::default()
                        })
                    };
                    if let Some(batch) = batch {
                        return Some((batch, (sheet, changes, epoch, cursor)));
                    }
                    changes.changed().await.ok()?;
                }
            },
        )
        .boxed()
    }
}

/// Drops each `cell.set` that a later `cell.set` of the same target overwrites with only
/// `cell.set`s in between (anything else, a row insert or a sort, may move cells: it ends the run).
fn compact(ops: &mut Vec<pb::SheetOp>) {
    let mut keep = vec![true; ops.len()];
    let mut start = 0;
    while start < ops.len() {
        let end = ops[start..]
            .iter()
            .position(|op| op.command != "cell.set")
            .map_or(ops.len(), |n| start + n);
        let mut seen = std::collections::HashSet::new();
        for i in (start..end).rev() {
            if !seen.insert(target(&ops[i])) {
                keep[i] = false;
            }
        }
        start = end + 1;
    }
    let mut keep = keep.into_iter();
    ops.retain(|_| keep.next().unwrap_or(true));
}

/// What a `cell.set` writes to: its params without the value.
fn target(op: &pb::SheetOp) -> String {
    match serde_json::from_str::<Value>(&op.params_json) {
        Ok(Value::Object(mut params)) => {
            params.remove("input");
            Value::Object(params).to_string()
        }
        // Unreadable: never equal to another op's target.
        _ => format!("#{}", op.seq),
    }
}

/// The gridcraft MCP tools offered to the agent, as OpenAI function definitions.
pub fn tool_definitions() -> Vec<Value> {
    const OFFERED: &[&str] = &[
        "execute_command",
        "list_commands",
        "inspect_workbook",
        "read_range",
        "write_range",
        "set_cell",
        "get_cell",
        "evaluate_formula",
        "format_range",
        "insert_chart",
        "create_table",
        "sort_range",
        "filter",
        "undo",
        "redo",
        "list_functions",
    ];
    gridcraft_mcp::tool_definitions()
        .into_iter()
        .filter(|t| t["name"].as_str().is_some_and(|n| OFFERED.contains(&n)))
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": format!("sheet_{}", t["name"].as_str().unwrap_or_default()),
                    "description": t["description"],
                    "parameters": t["inputSchema"],
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tool_writes_are_logged_and_replayable() {
        let sheet = Sheet::new();
        let r = sheet.tool(
            "agent",
            "write_range",
            &json!({ "range": "A1", "values": [["CPU", 12.5]] }),
        );
        assert!(!r.is_error, "{:?}", r.to_value());
        let read = sheet.tool("agent", "read_range", &json!({ "range": "A1:B1" }));
        assert!(read.to_value().to_string().contains("12.5"));
        let first = Arc::clone(&sheet).follow(0, "").next().await.unwrap();
        assert_ne!(
            first.ops,
            [] as [agent_demo_proto::agentdemo::v1::SheetOp; 0]
        );
        assert!(
            first
                .ops
                .iter()
                .all(|op| op.origin == "agent" && !READ_ONLY.contains(&op.command.as_str()))
        );

        // Replaying the log into a fresh engine gives the same values.
        let mut replica = Headless::new();
        for op in &first.ops {
            replica
                .session
                .execute_headless(&op.command, serde_json::from_str(&op.params_json).unwrap())
                .unwrap();
        }
        let replayed = call_tool(&mut replica, "read_range", &json!({ "range": "A1:B1" }));
        assert_eq!(replayed.to_value(), read.to_value());
    }

    #[tokio::test]
    async fn compaction_keeps_the_last_write_and_the_seqs() {
        let sheet = Sheet::new();
        sheet.tool(
            "agent",
            "write_range",
            &json!({ "range": "A1", "values": [["title"]] }),
        );
        let mut follower = Arc::clone(&sheet).follow(0, "");
        let first = follower.next().await.unwrap().ops;
        for i in 0..COMPACT_AT {
            sheet
                .set_cells(
                    "widget:w",
                    &[("B2".into(), i.to_string()), ("B3".into(), "x".into())],
                )
                .unwrap();
        }
        // A follower that was there all along goes on from its cursor, without a reset.
        let live = follower.next().await.unwrap();
        assert!(!live.reset && live.ops[0].seq > first.last().unwrap().seq);
        // A new one replays a short log that ends with the same values.
        let fresh = Arc::clone(&sheet).follow(0, "").next().await.unwrap().ops;
        assert!(fresh.len() < 20, "{} ops", fresh.len());
        assert!(fresh.windows(2).all(|w| w[0].seq < w[1].seq));
        let mut replica = Headless::new();
        for op in &fresh {
            replica
                .session
                .execute_headless(&op.command, serde_json::from_str(&op.params_json).unwrap())
                .unwrap();
        }
        let read =
            |h: &mut Headless| call_tool(h, "read_range", &json!({ "range": "A1:B3" })).to_value();
        let mut server = sheet.lock();
        assert_eq!(read(&mut replica), read(&mut server.engine));
    }

    #[test]
    fn clients_cannot_reach_files_or_the_network() {
        let sheet = Sheet::new();
        let op = |command: &str| pb::SheetOp {
            command: command.into(),
            params_json: "{}".into(),
            ..Default::default()
        };
        let rejected = sheet.apply_client(
            "c1",
            &[op("file.save"), op("data.fromWeb"), op("view.freezePanes")],
        );
        assert_eq!(rejected.len(), 2, "{rejected:?}");
        assert!(!denied("view.freezePanes") && denied("view.darkMode") && denied("file.open"));
    }

    #[tokio::test]
    async fn denied_commands_and_resets() {
        let sheet = Sheet::new();
        let r = sheet.tool(
            "agent",
            "execute_command",
            &json!({ "command": "file.open", "params": { "path": "/etc/passwd" } }),
        );
        assert!(r.is_error);
        sheet
            .set_cells("widget:w", &[("B2".into(), "42".into())])
            .unwrap();
        let mut follow = Arc::clone(&sheet).follow(0, "");
        assert_eq!(follow.next().await.unwrap().ops.len(), 1);
        sheet.reset();
        let after = follow.next().await.unwrap();
        assert!(after.reset && after.ops.is_empty());
        // A client of the old log (or another process) is told to start over.
        sheet
            .set_cells("widget:w", &[("B2".into(), "1".into())])
            .unwrap();
        let stale = Arc::clone(&sheet).follow(9, "old-0").next().await.unwrap();
        assert!(stale.reset && stale.ops.len() == 1);
        let same = Arc::clone(&sheet)
            .follow(0, &after.log_id)
            .next()
            .await
            .unwrap();
        assert!(!same.reset && same.ops.len() == 1);
    }
}
