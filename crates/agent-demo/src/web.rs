//! Web UI: a proa-rendered shell with React islands, served from assets embedded at build time.
//!
//! `web/` builds the islands and a self-hosted React runtime into `web/dist/static/` (see
//! `build.rs`). The shell renders each island's server-side fallback, an import map that points
//! React at the embedded vendor modules, and proa's island bootstrap, which mounts the islands.

use std::collections::HashMap;

use axum::Router;
use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use include_dir::{Dir, include_dir};
use proa_core::{Ctx, WebRenderSync as _, WriteError};
use proa_macros::html_sync;
use proa_react::{
    HydrationStrategy, ReactBootstrapConfig, ReactModuleDescriptor, react_bootstrap, react_island,
    render_import_map_entries,
};

/// Shown in the page header; see `COPYRIGHT` at the repository root.
const COPYRIGHT: &str = concat!("© 2026 Yannick Koechlin · ", env!("CARGO_PKG_LICENSE"));

/// `web/dist`, embedded; its `static/` directory is served under `/static`.
static DIST: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../web/dist");

/// Import map: bare React specifiers to the embedded vendor modules (no CDN).
const IMPORT_MAP: [(&str, &str); 3] = [
    ("react", "/static/vendor/react.js"),
    ("react/jsx-runtime", "/static/vendor/react-jsx-runtime.js"),
    ("react-dom/client", "/static/vendor/react-dom-client.js"),
];

/// Islands are mounted client-side (`createRoot`) rather than hydrated: the server renders a
/// static fallback, the island then takes over with live data.
const BOOTSTRAP: ReactBootstrapConfig<'static> = ReactBootstrapConfig {
    selector: "[data-island]",
    react_import: "react",
    react_dom_client_import: "react-dom/client",
    hydrate_fn: "createRoot",
    auto_run: true,
    expose_global: None,
    use_external_runtime: true,
    react_module_url: "/static/vendor/react.js",
    react_dom_module_url: "/static/vendor/react-dom-client.js",
};

const ISLANDS: [ReactModuleDescriptor<'static>; 1] = [ReactModuleDescriptor {
    island: "AgentConsole",
    module_path: "/static/islands/AgentConsole.js",
    export: None,
    strategy: HydrationStrategy::Immediate,
}];

pub fn router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/static/{*path}", get(static_asset))
}

/// Serves `web/dist/static/<path>` from the binary.
async fn static_asset(Path(path): Path<String>) -> Response {
    match DIST.get_file(format!("static/{path}")) {
        Some(file) => (
            [
                (header::CONTENT_TYPE, content_type(&path)),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            file.contents(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("json" | "map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

async fn index() -> Html<String> {
    Html(render_index().expect("rendering into a String cannot fail"))
}

fn render_index() -> Result<String, WriteError> {
    let mut cx = Ctx::new();
    html_sync! {
        <header class="hud">
            <div class="brand">
                <h1>"agent demo"</h1>
                <span class="build-version">{env!("CARGO_PKG_VERSION")}</span>
            </div>
            <span class="meta">"An LLM agent, a shared spreadsheet and WebAssembly widgets, streamed live"</span>
            <span class="meta copyright">{COPYRIGHT}</span>
        </header>
        <main class="console-main">
            {react_island("AgentConsole", "agent-console", "{}".to_owned(), |cx| {
                html_sync! {
                    <section class="panel">
                        <h2>"CONNECTING…"</h2>
                        <p>"Loading the agent console. It needs JavaScript."</p>
                    </section>
                }
                .render(cx)
            })}
        </main>
    }
    .render(&mut cx)?;
    render_runtime(&mut cx)?;
    Ok(document("agent demo", &cx.into_output().into_string()))
}

/// Import map plus proa's island bootstrap, appended to every page body.
fn render_runtime(cx: &mut Ctx) -> Result<(), WriteError> {
    let imports: HashMap<String, String> = IMPORT_MAP
        .iter()
        .map(|&(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    render_import_map_entries(cx, &imports)?;
    react_bootstrap(&ISLANDS, &BOOTSTRAP).render(cx)
}

fn document(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"theme-color\" content=\"#02070a\">\
         <title>{title}</title><link rel=\"stylesheet\" href=\"/static/theme.css\">\
         <link rel=\"stylesheet\" href=\"/static/console.css\">\
         </head><body>{body}</body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::{DIST, render_index};

    #[test]
    fn index_has_island_import_map_and_bootstrap() {
        let html = render_index().unwrap();
        assert!(html.contains(r#"data-island="AgentConsole""#), "{html}");
        assert!(html.contains(r#"type="importmap""#), "{html}");
        assert!(
            html.contains("/static/vendor/react-dom-client.js"),
            "{html}"
        );
        assert!(html.contains("/static/islands/AgentConsole.js"), "{html}");
    }

    #[test]
    fn embedded_assets_cover_import_map_and_islands() {
        if std::env::var_os("AGENT_DEMO_SKIP_WEB_BUILD").is_some() {
            return;
        }
        for path in [
            "static/theme.css",
            "static/console.css",
            "static/vendor/react.js",
            "static/vendor/react-jsx-runtime.js",
            "static/vendor/react-dom-client.js",
            "static/islands/AgentConsole.js",
            "static/design/tokens.css",
            "static/design/components.css",
        ] {
            assert!(
                DIST.get_file(path).is_some(),
                "missing embedded asset {path}"
            );
        }
    }
}
