//! The activity log: what happened, one JSON object per line in `<data>/activity.jsonl`, kept
//! across restarts so a session can be looked at afterwards (`jq`, `grep`).
//!
//! Every line has `ts` (milliseconds since 1970) and `kind`:
//!
//! - `prompt`: a run started (`prompt`, `model`, `scenario_id`, `conversation_id`, `run_id`).
//! - `tool_call`: the agent called a tool (`run_id`, `tool`, `arguments`, `output`, `is_error`, `ms`).
//! - `answer`: the agent's final text of a turn (`run_id`, `text`, `input_tokens`, `output_tokens`).
//! - `run_end`: (`run_id`, `status`, `error`).
//! - `sheet_client_ops`: a browser changed the sheet (`client`, `commands`, `rejected`).
//! - `sheet_reset`: (`by`).
//! - `widget`: a widget was compiled, failed, stopped or restarted (`name`, `event`, `error`).
//! - `client`: something a browser reported, e.g. the sheet crashed (`event`, `message`, `detail`).
//! - `settings`: settings were saved (never the key).
//!
//! Long text is cut to [`MAX_FIELD`] bytes. At [`MAX_BYTES`] the file moves to
//! `activity.jsonl.1` (replacing the previous one) and a new file starts. The prompt history
//! ([`prompts`]) is read back from both files at startup.
//!
//! One log per process ([`init`]); without one (tests), recording does nothing.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

pub const MAX_BYTES: u64 = 16 * 1024 * 1024;
/// Longest text field written (tool outputs, answers, WAT).
pub const MAX_FIELD: usize = 8 * 1024;
/// Prompts kept in memory for [`prompts`].
const PROMPTS_KEPT: usize = 500;

static LOG: OnceLock<ActivityLog> = OnceLock::new();

/// Makes `log` the process's activity log (the first call wins).
pub fn init(log: ActivityLog) {
    if LOG.set(log).is_err() {
        tracing::warn!("activity log already set");
    }
}

/// Writes one entry to the process's log, if there is one.
pub fn record(kind: &str, fields: Value) {
    if let Some(log) = LOG.get() {
        log.record(kind, fields);
    }
}

/// The most recent prompts, newest first, repeats folded.
pub fn prompts(limit: usize) -> Vec<Prompt> {
    LOG.get().map(|log| log.prompts(limit)).unwrap_or_default()
}

/// `s`, cut to [`MAX_FIELD`] bytes on a char boundary.
pub fn cut(s: &str) -> String {
    if s.len() <= MAX_FIELD {
        return s.to_owned();
    }
    let mut end = MAX_FIELD;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… ({} more bytes)", &s[..end], s.len() - end)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub text: String,
    pub at_ms: i64,
    pub model: String,
    pub scenario_id: String,
    pub run_id: String,
}

impl Prompt {
    fn from_entry(entry: &Value) -> Option<Self> {
        let s = |k: &str| {
            entry
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let text = s("prompt");
        if text.trim().is_empty() {
            return None;
        }
        Some(Self {
            text,
            at_ms: entry.get("ts").and_then(Value::as_i64).unwrap_or_default(),
            model: s("model"),
            scenario_id: s("scenario_id"),
            run_id: s("run_id"),
        })
    }
}

#[derive(Debug)]
pub struct ActivityLog {
    path: PathBuf,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    file: Option<File>,
    size: u64,
    /// Oldest first.
    prompts: VecDeque<Prompt>,
}

impl ActivityLog {
    /// The log at `dir/activity.jsonl`; reads the prompt history back. The file opens lazily,
    /// so a read-only data directory only costs the log, not the server.
    pub fn open(dir: &Path) -> Self {
        let path = dir.join("activity.jsonl");
        let mut prompts = VecDeque::new();
        for file in [rotated(&path), path.clone()] {
            let Ok(f) = File::open(&file) else { continue };
            for line in BufReader::new(f).lines().map_while(Result::ok) {
                let Ok(entry) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if entry.get("kind").and_then(Value::as_str) == Some("prompt")
                    && let Some(p) = Prompt::from_entry(&entry)
                {
                    push_prompt(&mut prompts, p);
                }
            }
        }
        let size = std::fs::metadata(&path).map_or(0, |m| m.len());
        Self {
            path,
            state: Mutex::new(State {
                file: None,
                size,
                prompts,
            }),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn record(&self, kind: &str, fields: Value) {
        let mut entry = Map::new();
        entry.insert("ts".into(), json!(now_ms()));
        entry.insert("kind".into(), json!(kind));
        if let Value::Object(fields) = fields {
            entry.extend(fields);
        }
        let entry = Value::Object(entry);
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if kind == "prompt"
            && let Some(p) = Prompt::from_entry(&entry)
        {
            push_prompt(&mut state.prompts, p);
        }
        let mut line = entry.to_string();
        line.push('\n');
        if let Err(e) = self.write(&mut state, line.as_bytes()) {
            state.file = None;
            tracing::warn!(path = %self.path.display(), error = %e, "cannot write the activity log");
        }
    }

    fn write(&self, state: &mut State, line: &[u8]) -> std::io::Result<()> {
        if state.size > 0 && state.size + line.len() as u64 > MAX_BYTES {
            state.file = None;
            match std::fs::rename(&self.path, rotated(&self.path)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            state.size = 0;
        }
        if state.file.is_none() {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            state.file = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)?,
            );
        }
        if let Some(file) = state.file.as_mut() {
            file.write_all(line)?;
        }
        state.size += line.len() as u64;
        Ok(())
    }

    pub fn prompts(&self, limit: usize) -> Vec<Prompt> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut seen = std::collections::HashSet::new();
        state
            .prompts
            .iter()
            .rev()
            .filter(|p| seen.insert(p.text.trim().to_owned()))
            .take(limit)
            .cloned()
            .collect()
    }
}

fn push_prompt(prompts: &mut VecDeque<Prompt>, p: Prompt) {
    if prompts.len() == PROMPTS_KEPT {
        prompts.pop_front();
    }
    prompts.push_back(p);
}

fn rotated(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("agent-demo-activity-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn entries_are_lines_and_prompts_come_back_after_a_restart() {
        let dir = dir("restart");
        let log = ActivityLog::open(&dir);
        log.record(
            "prompt",
            json!({ "prompt": "fill the sheet", "model": "m", "run_id": "run-1" }),
        );
        log.record(
            "tool_call",
            json!({ "tool": "sheet_write_range", "is_error": false }),
        );
        log.record(
            "prompt",
            json!({ "prompt": "top processes", "run_id": "run-2" }),
        );
        log.record(
            "prompt",
            json!({ "prompt": "fill the sheet", "run_id": "run-3" }),
        );
        let text = std::fs::read_to_string(log.path()).unwrap();
        let kinds: Vec<String> = text
            .lines()
            .map(|l| {
                serde_json::from_str::<Value>(l).unwrap()["kind"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(kinds, ["prompt", "tool_call", "prompt", "prompt"]);

        let again = ActivityLog::open(&dir);
        let prompts: Vec<_> = again
            .prompts(10)
            .into_iter()
            .map(|p| (p.text, p.run_id))
            .collect();
        // Newest first; the repeated prompt shows once, as its latest run.
        assert_eq!(
            prompts,
            [
                ("fill the sheet".to_owned(), "run-3".to_owned()),
                ("top processes".to_owned(), "run-2".to_owned())
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_full_log_moves_aside() {
        let dir = dir("rotate");
        let log = ActivityLog::open(&dir);
        log.record("prompt", json!({ "prompt": "before" }));
        log.state.lock().unwrap().size = MAX_BYTES;
        log.record("prompt", json!({ "prompt": "after" }));
        assert!(
            std::fs::read_to_string(rotated(log.path()))
                .unwrap()
                .contains("before")
        );
        assert!(
            !std::fs::read_to_string(log.path())
                .unwrap()
                .contains("before")
        );
        // Both files feed the history.
        let prompts: Vec<_> = ActivityLog::open(&dir)
            .prompts(5)
            .into_iter()
            .map(|p| p.text)
            .collect();
        assert_eq!(prompts, ["after", "before"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn long_fields_are_cut() {
        let long = "é".repeat(MAX_FIELD);
        let cut = cut(&long);
        assert!(cut.len() < long.len() && cut.contains("more bytes"));
    }
}
