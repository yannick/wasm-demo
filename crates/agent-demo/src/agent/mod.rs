//! Agents: what produces a run's events.
//!
//! The UI does not care where events come from. [`scripted::ScriptedAgent`] plays canned
//! scenarios offline so the demo always works; [`llm::LlmAgent`] talks to a configured
//! OpenAI-compatible model with tools. [`Agents`] offers both behind one trait.

pub mod llm;
pub mod scripted;

use std::sync::Arc;

use std::future::Future;
use std::pin::Pin;

use agent_demo_proto::agentdemo::v1 as pb;

use crate::run::{Cancelled, RunHandle};

pub type AgentFuture = Pin<Box<dyn Future<Output = Result<(), AgentError>> + Send>>;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
    #[error("unknown scenario {0:?}")]
    UnknownScenario(String),
    #[error("{0}")]
    Failed(String),
}

/// What a run was started with.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub scenario_id: String,
    pub prompt: String,
    /// Runs with the same id continue one conversation; empty: a standalone run.
    pub conversation_id: String,
    /// The model to use; empty: the configured one.
    pub model: String,
}

pub trait Agent: Send + Sync + std::fmt::Debug + 'static {
    /// What the "new run" picker offers.
    fn scenarios(&self) -> Vec<pb::Scenario>;

    /// The model that will answer `request` ("" for agents without one).
    fn model_for(&self, _request: &Request) -> String {
        String::new()
    }

    /// Plays one run, writing every step to `run`. The caller ends the run from the result, so
    /// an agent never emits `RunFinished` itself.
    fn run(&self, request: Request, run: RunHandle) -> AgentFuture;
}

/// Several agents behind one: each scenario goes to the agent that offers it.
#[derive(Debug)]
pub struct Agents(pub Vec<Arc<dyn Agent>>);

impl Agent for Agents {
    fn scenarios(&self) -> Vec<pb::Scenario> {
        self.0.iter().flat_map(|a| a.scenarios()).collect()
    }

    fn model_for(&self, request: &Request) -> String {
        self.agent_for(&request.scenario_id)
            .map(|a| a.model_for(request))
            .unwrap_or_default()
    }

    fn run(&self, request: Request, run: RunHandle) -> AgentFuture {
        if let Some(agent) = self.agent_for(&request.scenario_id) {
            agent.run(request, run)
        } else {
            let id = request.scenario_id;
            Box::pin(async move { Err(AgentError::UnknownScenario(id)) })
        }
    }
}

impl Agents {
    fn agent_for(&self, scenario_id: &str) -> Option<&Arc<dyn Agent>> {
        self.0
            .iter()
            .find(|a| a.scenarios().iter().any(|s| s.id == scenario_id))
    }
}

/// Runs `agent` in the background and ends the run from what it returns.
pub fn spawn(agent: &dyn Agent, request: Request, run: RunHandle) {
    let fut = agent.run(request, run.clone());
    tokio::spawn(async move {
        let (status, error) = match fut.await {
            Ok(()) => (pb::RunStatus::RUN_STATUS_SUCCEEDED, String::new()),
            Err(AgentError::Cancelled(_)) => (pb::RunStatus::RUN_STATUS_CANCELLED, String::new()),
            Err(e) => (pb::RunStatus::RUN_STATUS_FAILED, e.to_string()),
        };
        tracing::info!(run = %run.id(), ?status, %error, "run ended");
        crate::activity::record(
            "run_end",
            serde_json::json!({ "run_id": run.id(), "status": format!("{status:?}"), "error": error }),
        );
        run.finish(status, &error);
    });
}
