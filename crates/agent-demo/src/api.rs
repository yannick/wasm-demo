//! The connectrpc services: `AgentService` (runs and settings), `SheetService` (the shared
//! workbook's op log) and `MachineService` (live metrics and WebAssembly widgets).

use std::sync::Arc;

use agent_demo_proto::agentdemo::v1::{self as pb, AgentService, MachineService, SheetService};
use connectrpc::{
    ConnectError, RequestContext, Response, ServiceRequest, ServiceResult, ServiceStream,
};
use futures_util::{StreamExt as _, stream};
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use tokio_stream::wrappers::WatchStream;

use crate::activity;
use crate::agent::{self, Agent};
use crate::machine::Machine;
use crate::run::{DecideError, Decision, Runs};
use crate::settings::{Settings, SettingsError};
use crate::sheet::Sheet;
use crate::widgets::{WidgetError, Widgets};

/// Everything the services serve.
#[derive(Debug, Clone)]
pub struct State {
    pub runs: Arc<Runs>,
    pub agent: Arc<dyn Agent>,
    pub settings: Arc<Settings>,
    pub sheet: Arc<Sheet>,
    pub machine: Machine,
    pub widgets: Arc<Widgets>,
}

/// The connectrpc router serving every service.
pub fn router(state: &State) -> connectrpc::Router {
    connectrpc::Router::new()
        .add_service(Arc::new(AgentApi {
            runs: Arc::clone(&state.runs),
            agent: Arc::clone(&state.agent),
            settings: Arc::clone(&state.settings),
        }))
        .add_service(Arc::new(SheetApi {
            sheet: Arc::clone(&state.sheet),
        }))
        .add_service(Arc::new(MachineApi {
            machine: state.machine.clone(),
            widgets: Arc::clone(&state.widgets),
        }))
}

#[derive(Debug)]
pub struct AgentApi {
    runs: Arc<Runs>,
    agent: Arc<dyn Agent>,
    settings: Arc<Settings>,
}

// Concrete return types instead of the trait's `impl Encodable`: intended.
#[allow(refining_impl_trait)]
impl AgentService for AgentApi {
    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn list_scenarios(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::ListScenariosRequest>,
    ) -> ServiceResult<pb::ListScenariosResponse> {
        Response::ok(pb::ListScenariosResponse {
            scenarios: self.agent.scenarios(),
            ..Default::default()
        })
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn start_run(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::StartRunRequest>,
    ) -> ServiceResult<pb::StartRunResponse> {
        let scenario = request.scenario_id;
        if !self.agent.scenarios().iter().any(|s| s.id == scenario) {
            return Err(ConnectError::invalid_argument(format!(
                "unknown scenario {scenario:?}"
            )));
        }
        let mut start = agent::Request {
            scenario_id: scenario.to_owned(),
            prompt: request.prompt.trim().to_owned(),
            conversation_id: request.conversation_id.trim().to_owned(),
            model: request.model.trim().to_owned(),
        };
        start.model = self.agent.model_for(&start);
        let run = self.runs.create_in(
            scenario,
            &start.prompt,
            &start.conversation_id,
            &start.model,
        );
        let run_id = run.id();
        tracing::info!(run = %run_id, scenario, model = %start.model, "run started");
        activity::record(
            "prompt",
            json!({
                "prompt": activity::cut(&start.prompt),
                "model": start.model,
                "scenario_id": scenario,
                "conversation_id": start.conversation_id,
                "run_id": run_id,
            }),
        );
        agent::spawn(self.agent.as_ref(), start, run);
        Response::ok(pb::StartRunResponse {
            run_id,
            ..Default::default()
        })
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn watch_runs(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::WatchRunsRequest>,
    ) -> ServiceResult<ServiceStream<pb::WatchRunsResponse>> {
        let rx = self.runs.subscribe();
        let snapshot = self.runs.summaries();
        let runs = Arc::clone(&self.runs);
        // A client that falls behind gets the whole list again instead of the changes it missed.
        let updates = stream::unfold((rx, runs), |(mut rx, runs)| async move {
            let batch = match rx.recv().await {
                Ok(summary) => vec![summary],
                Err(RecvError::Lagged(_)) => runs.summaries(),
                Err(RecvError::Closed) => return None,
            };
            Some((batch, (rx, runs)))
        });
        let all = stream::iter([snapshot])
            .chain(updates)
            .flat_map(stream::iter)
            .map(|run| {
                Ok(pb::WatchRunsResponse {
                    run: run.into(),
                    ..Default::default()
                })
            });
        Response::ok(Box::pin(all) as ServiceStream<_>)
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn watch_run(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::WatchRunRequest>,
    ) -> ServiceResult<ServiceStream<pb::WatchRunResponse>> {
        let run = self
            .runs
            .get(request.run_id)
            .ok_or_else(|| ConnectError::not_found(format!("no run {}", request.run_id)))?;
        let events = run.follow(request.from_seq).map(|event| {
            Ok(pb::WatchRunResponse {
                event: event.into(),
                ..Default::default()
            })
        });
        Response::ok(Box::pin(events) as ServiceStream<_>)
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn decide_tool_call(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::DecideToolCallRequest>,
    ) -> ServiceResult<pb::DecideToolCallResponse> {
        let decision = Decision {
            approved: request.approved,
            reason: request.reason.to_owned(),
        };
        self.runs
            .decide(request.run_id, request.call_id, decision)
            .map_err(|e| match e {
                DecideError::UnknownRun(_) => ConnectError::not_found(e.to_string()),
                DecideError::NotWaiting(_) => ConnectError::failed_precondition(e.to_string()),
            })?;
        Response::ok(pb::DecideToolCallResponse::default())
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn cancel_run(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::CancelRunRequest>,
    ) -> ServiceResult<pb::CancelRunResponse> {
        if !self.runs.cancel(request.run_id) {
            return Err(ConnectError::not_found(format!(
                "no run {}",
                request.run_id
            )));
        }
        Response::ok(pb::CancelRunResponse::default())
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn get_settings(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::GetSettingsRequest>,
    ) -> ServiceResult<pb::GetSettingsResponse> {
        Response::ok(pb::GetSettingsResponse {
            settings: self.settings.llm().to_proto().into(),
            ..Default::default()
        })
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn save_settings(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::SaveSettingsRequest>,
    ) -> ServiceResult<pb::SaveSettingsResponse> {
        let update = request
            .to_owned_message()
            .settings
            .into_option()
            .ok_or_else(|| ConnectError::invalid_argument("settings missing"))?;
        let saved = self.settings.save(&update).map_err(|e| match e {
            SettingsError::Invalid(_) => ConnectError::invalid_argument(e.to_string()),
            SettingsError::Write { .. } => ConnectError::internal(e.to_string()),
        })?;
        tracing::info!(model = %saved.model, base_url = %saved.base_url, "settings saved");
        activity::record(
            "settings",
            json!({
                "model": saved.model,
                "base_url": saved.base_url,
                "temperature": saved.temperature,
                "max_tokens": saved.max_tokens,
                "max_tool_rounds": saved.max_tool_rounds,
                "custom_briefing": !saved.briefing.is_empty(),
                "extra_instructions": !saved.system_prompt.trim().is_empty(),
            }),
        );
        Response::ok(pb::SaveSettingsResponse {
            settings: saved.to_proto().into(),
            ..Default::default()
        })
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn list_prompts(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListPromptsRequest>,
    ) -> ServiceResult<pb::ListPromptsResponse> {
        let limit = match request.limit {
            0 => 50,
            n => n.min(500),
        };
        let prompts = activity::prompts(limit as usize)
            .into_iter()
            .map(|p| pb::PromptEntry {
                prompt: p.text,
                at_ms: p.at_ms,
                model: p.model,
                scenario_id: p.scenario_id,
                run_id: p.run_id,
                ..Default::default()
            })
            .collect();
        Response::ok(pb::ListPromptsResponse {
            prompts,
            ..Default::default()
        })
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn report_client_event(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::ReportClientEventRequest>,
    ) -> ServiceResult<pb::ReportClientEventResponse> {
        const MAX_KIND: usize = 64;
        let kind = request.kind.trim();
        if kind.is_empty() || kind.len() > MAX_KIND {
            return Err(ConnectError::invalid_argument(format!(
                "kind must be 1-{MAX_KIND} characters"
            )));
        }
        // Clients send what they like: keep it small, and JSON only if it parses.
        let raw = request.detail_json;
        let detail = match serde_json::from_str::<serde_json::Value>(raw) {
            Ok(v) if raw.len() <= activity::MAX_FIELD => v,
            _ => json!(activity::cut(raw)),
        };
        tracing::warn!(event = kind, message = request.message, "client event");
        activity::record(
            "client",
            json!({ "event": kind, "message": activity::cut(request.message), "detail": detail }),
        );
        Response::ok(pb::ReportClientEventResponse::default())
    }
}

#[derive(Debug)]
pub struct SheetApi {
    sheet: Arc<Sheet>,
}

#[allow(refining_impl_trait)]
impl SheetService for SheetApi {
    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn watch_sheet(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::WatchSheetRequest>,
    ) -> ServiceResult<ServiceStream<pb::WatchSheetResponse>> {
        let batches = Arc::clone(&self.sheet)
            .follow(request.from_seq, request.log_id)
            .map(Ok);
        Response::ok(Box::pin(batches) as ServiceStream<_>)
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn apply_sheet_ops(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::ApplySheetOpsRequest>,
    ) -> ServiceResult<pb::ApplySheetOpsResponse> {
        let request = request.to_owned_message();
        if request.client_id.is_empty() {
            return Err(ConnectError::invalid_argument("client_id is required"));
        }
        let sheet = Arc::clone(&self.sheet);
        let commands: Vec<String> = request.ops.iter().map(|op| op.command.clone()).collect();
        let client = request.client_id.clone();
        let rejected = tokio::task::spawn_blocking(move || {
            sheet.apply_client(&request.client_id, &request.ops)
        })
        .await
        .map_err(|e| ConnectError::internal(e.to_string()))?;
        activity::record(
            "sheet_client_ops",
            json!({ "client": client, "commands": commands, "rejected": rejected }),
        );
        Response::ok(pb::ApplySheetOpsResponse {
            rejected,
            ..Default::default()
        })
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn reset_sheet(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::ResetSheetRequest>,
    ) -> ServiceResult<pb::ResetSheetResponse> {
        self.sheet.reset();
        activity::record("sheet_reset", json!({ "by": "user" }));
        Response::ok(pb::ResetSheetResponse::default())
    }
}

#[derive(Debug)]
pub struct MachineApi {
    machine: Machine,
    widgets: Arc<Widgets>,
}

#[allow(refining_impl_trait)]
impl MachineService for MachineApi {
    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn watch_metrics(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::WatchMetricsRequest>,
    ) -> ServiceResult<ServiceStream<pb::WatchMetricsResponse>> {
        let samples = WatchStream::new(self.machine.subscribe()).map(|sample| {
            Ok(pb::WatchMetricsResponse {
                metrics: sample.metrics.clone().into(),
                ..Default::default()
            })
        });
        Response::ok(Box::pin(samples) as ServiceStream<_>)
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn watch_widgets(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::WatchWidgetsRequest>,
    ) -> ServiceResult<ServiceStream<pb::WatchWidgetsResponse>> {
        let rx = self.widgets.subscribe();
        let snapshot: Vec<_> = self
            .widgets
            .list()
            .into_iter()
            .map(|w| pb::WatchWidgetsResponse {
                widget: w.into(),
                ..Default::default()
            })
            .collect();
        let widgets = Arc::clone(&self.widgets);
        // A client that falls behind gets every widget again instead of the changes it missed.
        let updates = stream::unfold((rx, widgets), |(mut rx, widgets)| async move {
            let batch = match rx.recv().await {
                Ok(change) => vec![change],
                Err(RecvError::Lagged(_)) => widgets
                    .list()
                    .into_iter()
                    .map(|w| pb::WatchWidgetsResponse {
                        widget: w.into(),
                        ..Default::default()
                    })
                    .collect(),
                Err(RecvError::Closed) => return None,
            };
            Some((batch, (rx, widgets)))
        });
        let all = stream::iter([snapshot])
            .chain(updates)
            .flat_map(stream::iter)
            .map(Ok);
        Response::ok(Box::pin(all) as ServiceStream<_>)
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn compile_widget(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::CompileWidgetRequest>,
    ) -> ServiceResult<pb::CompileWidgetResponse> {
        let widget = self
            .widgets
            .compile(
                request.name,
                request.description,
                request.wat,
                request.interval_ms,
            )
            .map_err(|e| match e {
                WidgetError::Invalid(_) => ConnectError::invalid_argument(e.to_string()),
                _ => ConnectError::failed_precondition(e.to_string()),
            })?;
        Response::ok(pb::CompileWidgetResponse {
            widget: widget.into(),
            ..Default::default()
        })
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn restart_widget(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::RestartWidgetRequest>,
    ) -> ServiceResult<pb::RestartWidgetResponse> {
        let widget = self.widgets.restart(request.name).map_err(|e| match e {
            WidgetError::Invalid(_) => ConnectError::not_found(e.to_string()),
            _ => ConnectError::failed_precondition(e.to_string()),
        })?;
        Response::ok(pb::RestartWidgetResponse {
            widget: widget.into(),
            ..Default::default()
        })
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn stop_widget(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::StopWidgetRequest>,
    ) -> ServiceResult<pb::StopWidgetResponse> {
        if !self.widgets.stop(request.name, request.remove) {
            return Err(ConnectError::not_found(format!(
                "no widget {}",
                request.name
            )));
        }
        Response::ok(pb::StopWidgetResponse::default())
    }
}
