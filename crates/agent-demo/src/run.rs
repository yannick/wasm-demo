//! Runs: one append-only event log per run, followed live by any number of watchers.
//!
//! An agent appends events through its [`RunHandle`]; every append bumps the run's `watch`
//! counter. A watcher keeps its own cursor into the log and reads everything past it whenever the
//! counter moves, so a slow or reconnecting client misses nothing and never slows the agent down
//! (unlike a broadcast channel, which drops what a lagging receiver has not read).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agent_demo_proto::agentdemo::v1::{self as pb, __buffa::oneof::run_event::Kind};
use futures_util::{Stream, stream};
use tokio::sync::{broadcast, oneshot, watch};
use tokio_util::sync::CancellationToken;

/// The run was cancelled; agents return it from wherever they were waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("run cancelled")]
pub struct Cancelled;

#[derive(Debug, thiserror::Error)]
pub enum DecideError {
    #[error("no run {0}")]
    UnknownRun(String),
    #[error("tool call {0} is not waiting for approval")]
    NotWaiting(String),
}

/// A human's answer to a tool call that needed approval.
#[derive(Debug, Clone)]
pub struct Decision {
    pub approved: bool,
    pub reason: String,
}

/// All runs of this process (in memory; they are gone after a restart).
#[derive(Debug)]
pub struct Runs {
    by_id: Mutex<HashMap<String, Arc<Run>>>,
    next_id: AtomicU64,
    summaries: broadcast::Sender<pb::RunSummary>,
}

impl Runs {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            by_id: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            summaries: broadcast::channel(256).0,
        })
    }

    /// Creates a run in `RUN_STATUS_RUNNING` and logs its `RunStarted` event.
    #[cfg(test)]
    pub fn create(self: &Arc<Self>, scenario_id: &str, prompt: &str) -> RunHandle {
        self.create_in(scenario_id, prompt, "", "")
    }

    /// [`Runs::create`] for a turn of conversation `conversation_id`, answered by `model`.
    pub fn create_in(
        self: &Arc<Self>,
        scenario_id: &str,
        prompt: &str,
        conversation_id: &str,
        model: &str,
    ) -> RunHandle {
        let id = format!("run-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let summary = pb::RunSummary {
            run_id: id.clone(),
            scenario_id: scenario_id.to_owned(),
            prompt: prompt.to_owned(),
            status: pb::RunStatus::RUN_STATUS_RUNNING.into(),
            started_at_ms: now_ms(),
            conversation_id: conversation_id.to_owned(),
            model: model.to_owned(),
            ..Default::default()
        };
        let run = Arc::new(Run {
            state: Mutex::new(RunState {
                events: Vec::new(),
                summary: summary.clone(),
                ended: false,
            }),
            appended: watch::channel(0).0,
            cancel: CancellationToken::new(),
            approvals: Mutex::new(HashMap::new()),
        });
        lock(&self.by_id).insert(id, Arc::clone(&run));
        let handle = RunHandle {
            run,
            runs: Arc::clone(self),
        };
        handle.emit(
            "",
            Kind::RunStarted(Box::new(pb::RunStarted {
                run: summary.into(),
                ..Default::default()
            })),
        );
        handle
    }

    pub fn get(&self, id: &str) -> Option<Arc<Run>> {
        lock(&self.by_id).get(id).cloned()
    }

    /// Every run's summary, oldest first.
    pub fn summaries(&self) -> Vec<pb::RunSummary> {
        let mut all: Vec<_> = lock(&self.by_id).values().map(|r| r.summary()).collect();
        all.sort_by_key(|s| s.started_at_ms);
        all
    }

    /// Summary changes from now on. Subscribe before reading [`Runs::summaries`] so no change
    /// falls between the two.
    pub fn subscribe(&self) -> broadcast::Receiver<pb::RunSummary> {
        self.summaries.subscribe()
    }

    pub fn decide(
        &self,
        run_id: &str,
        call_id: &str,
        decision: Decision,
    ) -> Result<(), DecideError> {
        let run = self
            .get(run_id)
            .ok_or_else(|| DecideError::UnknownRun(run_id.to_owned()))?;
        let waiting = lock(&run.approvals).remove(call_id);
        waiting
            .and_then(|tx| tx.send(decision).ok())
            .ok_or_else(|| DecideError::NotWaiting(call_id.to_owned()))
    }

    /// Cancels a run; `false` if there is no such run.
    pub fn cancel(&self, run_id: &str) -> bool {
        self.get(run_id)
            .inspect(|run| run.cancel.cancel())
            .is_some()
    }
}

#[derive(Debug)]
pub struct Run {
    state: Mutex<RunState>,
    /// Number of events in the log; watchers wait for it to change.
    appended: watch::Sender<u64>,
    cancel: CancellationToken,
    approvals: Mutex<HashMap<String, oneshot::Sender<Decision>>>,
}

#[derive(Debug)]
struct RunState {
    events: Vec<pb::RunEvent>,
    summary: pb::RunSummary,
    ended: bool,
}

impl Run {
    pub fn summary(&self) -> pb::RunSummary {
        lock(&self.state).summary.clone()
    }

    /// The run's events from `from_seq` on: the logged ones, then live ones until the run ends.
    pub fn follow(
        self: Arc<Self>,
        from_seq: u64,
    ) -> impl Stream<Item = pb::RunEvent> + Send + 'static {
        // Subscribing first marks the current count as seen, so an append between the first read
        // and the first wait still wakes the watcher.
        let changes = self.appended.subscribe();
        let state = (self, changes, from_seq, VecDeque::new());
        stream::unfold(
            state,
            |(run, mut changes, mut cursor, mut pending)| async move {
                loop {
                    if let Some(event) = pending.pop_front() {
                        return Some((event, (run, changes, cursor, pending)));
                    }
                    let ended = {
                        let state = lock(&run.state);
                        let start = usize::try_from(cursor)
                            .unwrap_or(usize::MAX)
                            .min(state.events.len());
                        pending.extend(state.events[start..].iter().cloned());
                        cursor = state.events.len() as u64;
                        state.ended
                    };
                    if pending.is_empty() && (ended || changes.changed().await.is_err()) {
                        return None;
                    }
                }
            },
        )
    }
}

/// What an agent uses to write to its run: events, approvals and cancellation.
#[derive(Debug, Clone)]
pub struct RunHandle {
    run: Arc<Run>,
    runs: Arc<Runs>,
}

impl RunHandle {
    pub fn id(&self) -> String {
        lock(&self.run.state).summary.run_id.clone()
    }

    /// Appends an event (from the top-level agent if `agent_id` is empty) and returns its `seq`.
    /// Events after `RunFinished` are dropped.
    pub fn emit(&self, agent_id: &str, kind: Kind) -> u64 {
        let (seq, summary) = {
            let mut state = lock(&self.run.state);
            if state.ended {
                return state.events.len() as u64;
            }
            let summary_changed = apply(&mut state, &kind);
            let seq = state.events.len() as u64;
            state.events.push(pb::RunEvent {
                seq,
                at_ms: now_ms(),
                agent_id: agent_id.to_owned(),
                kind: Some(kind),
                ..Default::default()
            });
            (seq, summary_changed.then(|| state.summary.clone()))
        };
        self.run.appended.send_replace(seq + 1);
        if let Some(summary) = summary {
            // No receivers is fine: nobody is watching the run list.
            let _ = self.runs.summaries.send(summary);
        }
        seq
    }

    /// Asks a human to approve tool call `call_id` and waits for the answer.
    pub async fn request_approval(
        &self,
        agent_id: &str,
        call_id: &str,
        rationale: &str,
    ) -> Result<Decision, Cancelled> {
        let (tx, rx) = oneshot::channel();
        lock(&self.run.approvals).insert(call_id.to_owned(), tx);
        self.emit(
            agent_id,
            Kind::ToolCallApprovalRequested(Box::new(pb::ToolCallApprovalRequested {
                call_id: call_id.to_owned(),
                rationale: rationale.to_owned(),
                ..Default::default()
            })),
        );
        let decision = tokio::select! {
            decision = rx => decision.map_err(|_| Cancelled),
            () = self.run.cancel.cancelled() => Err(Cancelled),
        };
        lock(&self.run.approvals).remove(call_id);
        let decision = decision?;
        self.emit(
            agent_id,
            Kind::ToolCallDecided(Box::new(pb::ToolCallDecided {
                call_id: call_id.to_owned(),
                approved: decision.approved,
                reason: decision.reason.clone(),
                ..Default::default()
            })),
        );
        Ok(decision)
    }

    /// Runs `fut` until it completes, or fails as soon as the run is cancelled.
    pub async fn unless_cancelled<T>(
        &self,
        fut: impl std::future::Future<Output = T>,
    ) -> Result<T, Cancelled> {
        tokio::select! {
            out = fut => Ok(out),
            () = self.run.cancel.cancelled() => Err(Cancelled),
        }
    }

    /// Waits `duration`, or fails as soon as the run is cancelled.
    pub async fn sleep(&self, duration: Duration) -> Result<(), Cancelled> {
        tokio::select! {
            () = tokio::time::sleep(duration) => Ok(()),
            () = self.run.cancel.cancelled() => Err(Cancelled),
        }
    }

    /// Ends the run (the last event of its log).
    pub fn finish(&self, status: pb::RunStatus, error: &str) {
        self.emit(
            "",
            Kind::RunFinished(Box::new(pb::RunFinished {
                status: status.into(),
                error: error.to_owned(),
                ..Default::default()
            })),
        );
    }
}

/// Updates the run's summary for `kind`; `true` if it changed.
fn apply(state: &mut RunState, kind: &Kind) -> bool {
    let summary = &mut state.summary;
    match kind {
        Kind::ToolCallApprovalRequested(_) => {
            summary.status = pb::RunStatus::RUN_STATUS_AWAITING_APPROVAL.into();
        }
        Kind::ToolCallDecided(_) => summary.status = pb::RunStatus::RUN_STATUS_RUNNING.into(),
        Kind::UsageUpdated(u) => summary.usage = u.usage.clone(),
        Kind::RunFinished(f) => {
            summary.status = f.status;
            summary.ended_at_ms = now_ms();
            state.ended = true;
        }
        _ => return false,
    }
    true
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Locks a mutex; a panic while holding one leaves plain data, still fine to read.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use futures_util::StreamExt as _;

    use super::*;

    fn delta(text: &str) -> Kind {
        Kind::BlockDelta(Box::new(pb::BlockDelta {
            block_id: "b1".into(),
            text: text.into(),
            ..Default::default()
        }))
    }

    #[tokio::test]
    async fn a_late_watcher_gets_the_whole_log_then_live_events() {
        let runs = Runs::new();
        let run = runs.create("demo", "hi");
        run.emit("", delta("a"));
        let follower = runs.get(&run.id()).unwrap().follow(0);
        let live = tokio::spawn(follower.collect::<Vec<_>>());
        run.emit("", delta("b"));
        run.finish(pb::RunStatus::RUN_STATUS_SUCCEEDED, "");
        let events = live.await.unwrap();
        let seqs: Vec<_> = events.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [0, 1, 2, 3], "no gaps, nothing twice");
        assert!(matches!(events[3].kind, Some(Kind::RunFinished(_))));
    }

    #[tokio::test]
    async fn follow_from_a_seq_skips_what_the_client_has() {
        let runs = Runs::new();
        let run = runs.create("demo", "hi");
        run.emit("", delta("a"));
        run.finish(pb::RunStatus::RUN_STATUS_SUCCEEDED, "");
        let events: Vec<_> = runs.get(&run.id()).unwrap().follow(2).collect().await;
        assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), [2]);
    }

    #[tokio::test]
    async fn approval_waits_for_the_decision_and_updates_the_status() {
        let runs = Runs::new();
        let run = runs.create("demo", "hi");
        let waiting = tokio::spawn({
            let run = run.clone();
            async move { run.request_approval("", "c1", "needs a write").await }
        });
        while runs
            .decide(
                &run.id(),
                "c1",
                Decision {
                    approved: true,
                    reason: String::new(),
                },
            )
            .is_err()
        {
            tokio::task::yield_now().await;
        }
        assert!(waiting.await.unwrap().unwrap().approved);
        assert_eq!(
            runs.summaries()[0].status,
            pb::RunStatus::RUN_STATUS_RUNNING
        );
    }

    #[tokio::test]
    async fn cancel_interrupts_a_pending_approval() {
        let runs = Runs::new();
        let run = runs.create("demo", "hi");
        let waiting = tokio::spawn({
            let run = run.clone();
            async move { run.request_approval("", "c1", "x").await }
        });
        tokio::task::yield_now().await;
        assert!(runs.cancel(&run.id()));
        assert_eq!(waiting.await.unwrap().unwrap_err(), Cancelled);
    }
}
