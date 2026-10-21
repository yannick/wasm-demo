//! GridCraft in the agent-demo page: the same `SheetApp` as gridcraft's web build, framed by the
//! page and driven over `postMessage` (see `bridge.rs`).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[cfg(target_arch = "wasm32")]
mod bridge;
#[cfg(target_arch = "wasm32")]
mod web;

#[cfg(target_arch = "wasm32")]
fn main() {
    web::start();
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("sheet-web only runs in the browser: `just sheet-build`");
}
