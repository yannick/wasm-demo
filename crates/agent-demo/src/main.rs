//! `agent-demo`: runs agents and streams every step to a live web UI.
//!
//! One binary: the proa page shell and React islands (embedded), the GridCraft sheet
//! (WebAssembly, embedded), and the Connect API (`/agentdemo.v1.*`, binary protobuf) that the
//! islands stream from.

mod agent;
mod api;
mod llm;
mod machine;
mod run;
mod settings;
mod sheet;
mod tools;
mod web;
mod widgets;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use axum::{Router, routing::get};
use tracing_subscriber::EnvFilter;

use crate::agent::llm::LlmAgent;
use crate::agent::scripted::ScriptedAgent;
use crate::agent::{Agent, Agents};
use crate::api::State;
use crate::machine::Machine;
use crate::run::Runs;
use crate::settings::Settings;
use crate::sheet::Sheet;
use crate::tools::Tools;
use crate::widgets::Widgets;

const DEFAULT_LISTEN: &str = "127.0.0.1:7400";
const DEFAULT_DATA_DIR: &str = ".data";
/// Open run streams get this long to finish after Ctrl-C before the server stops anyway.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

const USAGE: &str = "usage: agent-demo [--listen ADDR] [--data DIR] [--speed FACTOR]

  --listen ADDR    address for the UI and API (default 127.0.0.1:7400, env AGENT_DEMO_LISTEN)
  --data DIR       where settings are stored (default ./.data, env AGENT_DEMO_DATA_DIR)
  --speed FACTOR   delay multiplier of the scripted agent: 1 normal, 0.2 fast, 3 slow-motion

The LLM API key is read from the variable named in the settings (MELIOUS_API_KEY by default),
also from ./.env.";

#[derive(Debug)]
struct Options {
    listen: SocketAddr,
    data: PathBuf,
    speed: f64,
}

fn options(mut args: impl Iterator<Item = String>) -> anyhow::Result<Options> {
    let env_listen = std::env::var("AGENT_DEMO_LISTEN").ok();
    let mut listen = env_listen.as_deref().unwrap_or(DEFAULT_LISTEN).to_owned();
    let mut data = std::env::var_os("AGENT_DEMO_DATA_DIR")
        .map_or_else(|| PathBuf::from(DEFAULT_DATA_DIR), PathBuf::from);
    let mut speed = 1.0;
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .with_context(|| format!("{arg} needs a value\n\n{USAGE}"))
        };
        match arg.as_str() {
            "--listen" => listen = value()?,
            "--data" => data = value()?.into(),
            "--speed" => speed = value()?.parse().context("--speed takes a number")?,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument {other:?}\n\n{USAGE}"),
        }
    }
    Ok(Options {
        listen: listen
            .parse()
            .with_context(|| format!("bad listen address {listen:?}"))?,
        data,
        speed,
    })
}

fn main() -> anyhow::Result<()> {
    // Before any thread exists: setting variables is only sound then.
    settings::load_dotenv(std::path::Path::new(".env"));
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let opts = options(std::env::args().skip(1))?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?
        .block_on(serve(opts))
}

/// Wires the agents, the sheet, the machine sampler and the widgets together.
fn state(settings: Settings, machine: Machine, speed: f64) -> State {
    let settings = Arc::new(settings);
    let sheet = Sheet::new();
    let widgets = Widgets::new(machine.clone(), Arc::clone(&sheet));
    let tools = Arc::new(Tools::new(
        machine.clone(),
        Arc::clone(&sheet),
        Arc::clone(&widgets),
    ));
    let agent: Arc<dyn Agent> = Arc::new(Agents(vec![
        Arc::new(LlmAgent::new(Arc::clone(&settings), tools)),
        Arc::new(ScriptedAgent::new(speed)),
    ]));
    State {
        runs: Runs::new(),
        agent,
        settings,
        sheet,
        machine,
        widgets,
    }
}

async fn serve(opts: Options) -> anyhow::Result<()> {
    let settings = Settings::load(&opts.data);
    let llm = settings.llm();
    tracing::info!(
        data = %opts.data.display(),
        base_url = %llm.base_url,
        model = %llm.model,
        api_key = if llm.resolved_key().is_some() { "set" } else { "missing" },
        "LLM settings"
    );
    let router = app(&state(settings, Machine::start(), opts.speed));
    let listener = tokio::net::TcpListener::bind(opts.listen)
        .await
        .with_context(|| format!("cannot listen on {}", opts.listen))?;
    tracing::info!("agent-demo on http://{}", opts.listen);

    let server = axum::serve(listener, router).with_graceful_shutdown(terminated());
    // Graceful shutdown waits for every connection, and a browser keeps its run streams open.
    tokio::select! {
        result = server => result?,
        () = async {
            terminated().await;
            tokio::time::sleep(SHUTDOWN_GRACE).await;
        } => {}
    }
    Ok(())
}

/// The web UI, `/health`, and the Connect API.
fn app(state: &State) -> Router {
    web::router()
        .route("/health", get(|| async { "ok" }))
        .fallback_service(api::router(state).into_axum_service())
}

/// Resolves on Ctrl-C or SIGTERM.
async fn terminated() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("installing the SIGTERM handler");
        tokio::select! {
            () = ctrl_c => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    ctrl_c.await;
    tracing::info!("shutting down");
}

#[cfg(test)]
mod tests {
    use agent_demo_proto::agentdemo::v1::{
        self as pb, __buffa::oneof::run_event::Kind, AgentServiceClient,
    };
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use connectrpc::client::{ClientConfig, HttpClient};
    use tower::ServiceExt as _;

    use super::*;

    /// The app with the scripted agent at full speed and fixed machine readings.
    fn test_app() -> Router {
        let dir = std::env::temp_dir().join(format!("agent-demo-test-{}", std::process::id()));
        app(&state(
            Settings::load(&dir),
            Machine::fixed(machine::Sample::default()),
            0.0,
        ))
    }

    /// Serves the app (scripted agent at full speed) on an ephemeral port; returns a client.
    async fn serve() -> AgentServiceClient<HttpClient> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, test_app()).await.unwrap();
        });
        let config = ClientConfig::new(format!("http://{addr}").parse().unwrap())
            .with_default_timeout(Duration::from_secs(5));
        AgentServiceClient::new(HttpClient::plaintext(), config)
    }

    async fn start(client: &AgentServiceClient<HttpClient>, scenario: &str) -> String {
        let req = pb::StartRunRequest {
            scenario_id: scenario.into(),
            prompt: "test".into(),
            ..Default::default()
        };
        client.start_run(req).await.unwrap().into_owned().run_id
    }

    /// Reads a run's events until `stop` says so or the stream ends.
    async fn read(
        client: &AgentServiceClient<HttpClient>,
        run_id: &str,
        from_seq: u64,
        stop: impl Fn(&pb::RunEvent) -> bool,
    ) -> Vec<pb::RunEvent> {
        let req = pb::WatchRunRequest {
            run_id: run_id.into(),
            from_seq,
            ..Default::default()
        };
        let mut stream = client.watch_run(req).await.unwrap();
        let mut events = Vec::new();
        while let Some(msg) = stream.message().await.unwrap() {
            let event = msg
                .to_owned_message()
                .event
                .into_option()
                .expect("event set");
            let done = stop(&event);
            events.push(event);
            if done {
                break;
            }
        }
        events
    }

    fn finished(events: &[pb::RunEvent]) -> Option<pb::RunStatus> {
        events.iter().find_map(|e| match &e.kind {
            Some(Kind::RunFinished(f)) => f.status.as_known(),
            _ => None,
        })
    }

    #[tokio::test]
    async fn index_serves_html() {
        let res = test_app()
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn every_scenario_runs_to_the_end_in_binary_protobuf() {
        let client = serve().await;
        let scenarios = client
            .list_scenarios(pb::ListScenariosRequest::default())
            .await
            .unwrap()
            .into_owned();
        // "approval" waits for a human, "chat" and "monitor" need the LLM.
        let offline = |id: &str| !matches!(id, "approval" | "chat" | "monitor");
        for scenario in scenarios.scenarios.iter().filter(|s| offline(&s.id)) {
            let run_id = start(&client, &scenario.id).await;
            let events = read(&client, &run_id, 0, |_| false).await;
            let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
            assert_eq!(
                seqs,
                (0..events.len() as u64).collect::<Vec<_>>(),
                "{}: gapless",
                scenario.id
            );
            assert_eq!(
                finished(&events),
                Some(pb::RunStatus::RUN_STATUS_SUCCEEDED),
                "{}",
                scenario.id
            );
        }
    }

    #[tokio::test]
    async fn approval_scenario_waits_for_the_human_and_resumes_after_reconnect() {
        let client = serve().await;
        let run_id = start(&client, "approval").await;
        let before = read(&client, &run_id, 0, |e| {
            matches!(e.kind, Some(Kind::ToolCallApprovalRequested(_)))
        })
        .await;
        let Some(Kind::ToolCallApprovalRequested(ask)) = &before.last().unwrap().kind else {
            panic!("no approval request in {before:?}");
        };
        let decide = pb::DecideToolCallRequest {
            run_id: run_id.clone(),
            call_id: ask.call_id.clone(),
            approved: true,
            ..Default::default()
        };
        client.decide_tool_call(decide).await.unwrap();
        // Resume where the first stream stopped, as the UI does after a reconnect.
        let after = read(&client, &run_id, before.len() as u64, |_| false).await;
        assert_eq!(after[0].seq, before.len() as u64);
        assert!(
            after
                .iter()
                .any(|e| matches!(&e.kind, Some(Kind::ToolCallDecided(d)) if d.approved))
        );
        assert_eq!(finished(&after), Some(pb::RunStatus::RUN_STATUS_SUCCEEDED));
    }

    #[tokio::test]
    async fn cancelling_a_waiting_run_ends_it_cancelled() {
        let client = serve().await;
        let run_id = start(&client, "approval").await;
        read(&client, &run_id, 0, |e| {
            matches!(e.kind, Some(Kind::ToolCallApprovalRequested(_)))
        })
        .await;
        let cancel = pb::CancelRunRequest {
            run_id: run_id.clone(),
            ..Default::default()
        };
        client.cancel_run(cancel).await.unwrap();
        let events = read(&client, &run_id, 0, |_| false).await;
        assert_eq!(finished(&events), Some(pb::RunStatus::RUN_STATUS_CANCELLED));
    }

    #[test]
    fn options_parse_listen_and_speed() {
        let args = ["--listen", "0.0.0.0:9000", "--speed", "0.5"].map(String::from);
        let opts = options(args.into_iter()).unwrap();
        assert_eq!(opts.listen, "0.0.0.0:9000".parse().unwrap());
        assert!((opts.speed - 0.5).abs() < f64::EPSILON);
        assert!(options(["--bogus".to_owned()].into_iter()).is_err());
        let data = options(["--data", "/tmp/x"].map(String::from).into_iter()).unwrap();
        assert_eq!(data.data, PathBuf::from("/tmp/x"));
    }
}
