# agent-demo — all project commands. Run `just` for the menu.

set shell := ["bash", "-cu"]

image     := "agent-demo:dev"
container := "agent-demo"
port      := env_var_or_default("AGENT_DEMO_PORT", "7400")
# GridCraft (the spreadsheet engine) is a path dependency: a GridCraft checkout next to this repo
# (path set in Cargo.toml; this default matches it).
gridcraft := env_var_or_default("GRIDCRAFT_DIR", "../../rust/gridcraft")
# Resolver for build steps (apk, cargo) and the running container (the LLM API). The vmnet
# gateway's DNS forwarder can stop answering on macOS; a public resolver sidesteps it.
# CONTAINER_DNS= (empty) uses the default.
container_dns := env_var_or_default("CONTAINER_DNS", "1.1.1.1")
dns_flag      := if container_dns != "" { "--dns " + container_dns } else { "" }

# ── ANSI palette ─────────────────────────────────────────────────────────
reset  := '\033[0m'
bold   := '\033[1m'
dim    := '\033[2m'
green  := '\033[32m'
blue   := '\033[34m'
purple := '\033[35m'
cyan   := '\033[36m'

# colorful command menu (also: `just --list`)
[private]
default:
    @printf '{{bold}}{{cyan}}\n  ┌──────────────────────────────────────────────────────┐\n'
    @printf '  │  agent-demo — an LLM agent, a shared spreadsheet     │\n'
    @printf '  │  and WebAssembly widgets, streamed live to the UI    │\n'
    @printf '  └──────────────────────────────────────────────────────┘{{reset}}\n\n'
    @just --list --unsorted --list-heading '' --color always
    @printf '\n{{dim}}docs: README.md · docs/adr/ · metrics/README.md{{reset}}\n'

# ══ dev ══════════════════════════════════════════════════════════════════

# run the demo (UI on http://127.0.0.1:7400; the API key comes from ./.env); e.g. `just run --speed 0.3`
[group('dev')]
run *ARGS:
    cargo run -p agent-demo -- {{ARGS}}

# build everything (crates/agent-demo/build.rs also builds web/)
[group('dev')]
build:
    @printf '{{bold}}{{blue}}▸ building workspace (debug){{reset}}\n'
    cargo build --workspace --all-targets

# optimized release build
[group('dev')]
release:
    @printf '{{bold}}{{blue}}▸ building agent-demo (release){{reset}}\n'
    cargo build --release -p agent-demo

# build the web UI (React islands) into web/dist
[group('dev')]
web:
    cd web && pnpm install --frozen-lockfile && pnpm build

# build the GridCraft sheet (WebAssembly) into web/public/sheet; needs the rustup toolchain's wasm target
[group('dev')]
sheet:
    #!/usr/bin/env bash
    set -euo pipefail
    # Homebrew's cargo/rustc have no wasm32 std: use rustup's toolchain explicitly.
    toolchain="$(dirname "$(rustup which --toolchain 1.99.1 cargo)")"
    export PATH="$toolchain:$PATH"
    rustup target add --toolchain 1.99.1 wasm32-unknown-unknown >/dev/null
    printf '{{bold}}{{blue}}▸ building the sheet (wasm32, release){{reset}}\n'
    target="$(cd wasm/sheet-web && cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
    # Panic locations embed source paths: map the home and checkout dirs so no host path ships.
    src="$(cd ../.. && pwd)"
    export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$src=/src"$'\x1f'"--remap-path-prefix=$HOME=/home"
    (cd wasm/sheet-web && cargo build --release --target wasm32-unknown-unknown)
    wasm-bindgen --target web --no-typescript --out-dir web/public/sheet --out-name sheet-web \
        "$target/wasm32-unknown-unknown/release/sheet-web.wasm"
    cp wasm/sheet-web/index.html web/public/sheet/index.html
    ls -lh web/public/sheet

# regenerate the TypeScript API from proto/ (after changing agent.proto); Rust regenerates on build
[group('dev')]
gen:
    cd web && pnpm gen

# format the Rust code
[group('dev')]
fmt:
    cargo fmt --all

# ══ quality ══════════════════════════════════════════════════════════════

# run all tests (Rust and web)
[group('quality')]
test:
    cargo test --workspace
    cd web && pnpm test

# format check, clippy (warnings are errors), proto lint, web lint and typecheck
[group('quality')]
lint:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cd proto && buf lint
    cd web && pnpm lint && pnpm typecheck

# everything a CI run would check, including the metrics ratchets
[group('quality')]
check: lint test metrics-check

# ══ metrics ══════════════════════════════════════════════════════════════

# collect a code metrics snapshot (complexity, lines, unsafe, deps, binary size) → target/metrics/current.json
[group('metrics')]
metrics:
    python3 tools/metrics.py collect

# fail on a regression: bca complexity thresholds, unsafe uses, long functions (vs metrics/baseline.json)
[group('metrics')]
metrics-check:
    python3 tools/metrics.py check

# append the current snapshot to metrics/history/<date>-<label>.json
[group('metrics')]
metrics-record label:
    python3 tools/metrics.py record "{{label}}"

# accept the current unsafe uses and long functions as the baseline (after a deliberate change)
[group('metrics')]
metrics-baseline:
    python3 tools/metrics.py baseline

# line coverage with cargo-llvm-cov (added to the next snapshot)
[group('metrics')]
coverage:
    python3 tools/metrics.py coverage

# line coverage, then open the HTML report
[group('metrics')]
coverage-open:
    python3 tools/metrics.py coverage --open

# the biggest crates in the release binary (cargo-bloat)
[group('metrics')]
bloat:
    AGENT_DEMO_SKIP_WEB_BUILD=1 cargo bloat --release -p agent-demo --crates -n 25

# churn × complexity hotspots from git history → target/metrics/hotspots.html
[group('metrics')]
hotspots:
    mkdir -p target/metrics
    bca vcs --format html --output target/metrics/hotspots.html
    @printf '{{green}}✓ target/metrics/hotspots.html{{reset}}\n'

# show which metrics tools are installed
[group('metrics')]
tools-check:
    python3 tools/metrics.py tools-check

# install the pinned metrics tools
[group('metrics')]
tools-install:
    cargo install --locked big-code-analysis-cli@2.1.0
    cargo install --locked tokei
    cargo install --locked cargo-llvm-cov@0.8.7
    cargo install --locked cargo-bloat@0.12.1

# ══ container (Apple container CLI) ══════════════════════════════════════

# stage a clean build context: this repo and GridCraft side by side, without build outputs
[private]
_stage-ctx: web
    #!/usr/bin/env bash
    set -euo pipefail
    test -f web/public/sheet/sheet-web_bg.wasm || { echo "no sheet build: run 'just sheet' first"; exit 1; }
    test -d "{{gridcraft}}" || { echo "GridCraft not found at {{gridcraft}} (set GRIDCRAFT_DIR)"; exit 1; }
    rm -rf .build-ctx && mkdir -p .build-ctx/app .build-ctx/rust
    excludes=(--exclude target --exclude .git --exclude node_modules --exclude .build-ctx --exclude .data --exclude .env)
    rsync -a "${excludes[@]}" ./ .build-ctx/app/agent-demo/
    rsync -a "${excludes[@]}" "{{gridcraft}}/" .build-ctx/rust/gridcraft/
    du -sh .build-ctx

# build the FROM-scratch image with the Apple container CLI
[group('container')]
apple-build: _stage-ctx
    @printf '{{bold}}{{purple}}▸ container build {{image}}{{reset}}\n'
    container system start || true
    container build {{dns_flag}} -f Containerfile -t {{image}} .build-ctx
    @printf '{{green}}✓ {{image}}{{reset}}\n'

# run the image (http://127.0.0.1:7400, or AGENT_DEMO_PORT); the API key comes from ./.env, settings persist in a volume
[group('container')]
apple-run: apple-stop
    #!/usr/bin/env bash
    set -euo pipefail
    env_file=(); [ -f .env ] && env_file=(--env-file .env)
    container volume inspect agent-demo-data >/dev/null 2>&1 || container volume create agent-demo-data
    container run -d --name {{container}} {{dns_flag}} "${env_file[@]}" \
        -p 127.0.0.1:{{port}}:7400 -v agent-demo-data:/data {{image}}
    printf '{{green}}✓ agent-demo on http://127.0.0.1:{{port}}{{reset}}  (just apple-logs · just apple-stop)\n'

# follow the container's log
[group('container')]
apple-logs:
    container logs -f {{container}}

# stop and remove the container (the settings volume stays)
[group('container')]
apple-stop:
    -container stop {{container}} >/dev/null 2>&1
    -container delete {{container}} >/dev/null 2>&1
