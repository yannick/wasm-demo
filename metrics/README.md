# Code metrics

`tools/metrics.py` collects a snapshot and gates regressions.

| Recipe | What |
|---|---|
| `just metrics` | Snapshot → `target/metrics/current.json`, summary on stdout |
| `just metrics-check` | Fails (exit 2) on a regression; part of `just check` |
| `just metrics-record LABEL` | Snapshot → `metrics/history/<date>-<label>.json` (commit it) |
| `just metrics-baseline` | Accept the current unsafe count and long functions |
| `just coverage` / `coverage-open` | Line coverage (cargo-llvm-cov); goes into the next snapshot |
| `just bloat` | Biggest crates in the release binary (cargo-bloat) |
| `just hotspots` | Churn × complexity from git history (`bca vcs`) |
| `just tools-check` / `tools-install` | Pinned producers |

## What is measured

- **Complexity** (bca 2.1.0, `bca.toml`): per-function cyclomatic and cognitive complexity and
  length, for `crates/` and `wasm/sheet-web/src`. Thresholds are cyclomatic 15 and cognitive 20.
  Existing offenders are listed in `.bca-baseline.toml`, and an offender that gets worse still
  fails. Refresh with `bca check --write-baseline` only after a deliberate change.
- **Long functions**: more than 80 lines. Each one is listed in `baseline.json`, and a new or
  longer one fails.
- **unsafe**: `unsafe` blocks, fns, impls and traits in our Rust. The workspace denies
  `unsafe_code`; the one use (`settings::set_env`) is a commented `allow`.
- **Lines of code** (tokei), **dependencies** (crates in the server binary, from
  `cargo metadata`), **release binary size**, and **coverage** when collected.

The web code has its own gates in ESLint (complexity 10, depth 4, 150 lines per function).
