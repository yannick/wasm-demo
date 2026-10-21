//! Builds the React islands in `web/` (pnpm + Vite) so `src/web.rs` can embed `web/dist/`.
//!
//! Set `AGENT_DEMO_SKIP_WEB_BUILD=1` to skip the web build (for example in a Rust-only CI job);
//! the binary then embeds whatever `web/dist/` holds, possibly nothing.

use std::path::Path;
use std::process::Command;

fn main() {
    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web");
    for input in [
        "src",
        "public",
        "package.json",
        "pnpm-lock.yaml",
        "vite.config.ts",
        "tsconfig.json",
    ] {
        println!("cargo::rerun-if-changed={}", web.join(input).display());
    }
    println!("cargo::rerun-if-env-changed=AGENT_DEMO_SKIP_WEB_BUILD");

    if std::env::var_os("AGENT_DEMO_SKIP_WEB_BUILD").is_some() {
        std::fs::create_dir_all(web.join("dist/static")).expect("create web/dist/static");
        return;
    }
    if !web.join("node_modules").exists() {
        let lockfile = web.join("pnpm-lock.yaml").exists();
        pnpm(
            &web,
            if lockfile {
                &["install", "--frozen-lockfile"]
            } else {
                &["install"]
            },
        );
    }
    pnpm(&web, &["run", "build"]);
}

fn pnpm(dir: &Path, args: &[&str]) {
    let status = Command::new("pnpm")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to run pnpm (is it installed?): {e}"));
    assert!(
        status.success(),
        "`pnpm {}` failed in {}",
        args.join(" "),
        dir.display()
    );
}
