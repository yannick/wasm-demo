# agent-demo

A live GUI for demonstrating agentic features. A chat with a real LLM (Melious:
`deepseek-v4-flash-0731` or `qwen3.5-397b-a17b`, any OpenAI-compatible API) sits next to a full
spreadsheet (GridCraft compiled to WebAssembly). The spreadsheet is shared live between you, the
agent and the server. The agent can:

- read the machine the server runs on (CPU, cores, memory, swap, load, disk, network, processes),
- lay out, fill and format the sheet through GridCraft's tools (write ranges, formats, tables,
  charts, any engine command),
- **write and compile WebAssembly widgets**: small WAT programs, compiled and run on the server
  (sandboxed, fuel-limited), that call machine tools and write the results into the sheet every
  second.

Every step streams to the UI: thinking, answer text, plan, tool calls with input/output and
timing, token usage. Runs replay losslessly after a reconnect. The scripted scenarios from the
first version (approval, parallel sub-agents, error recovery) still work offline under **Runs**.

The stack: one Rust binary (axum + connectrpc, binary protobuf), a proa-rendered
page shell, and React/TypeScript islands built by Vite. The UI and the sheet's WebAssembly are
embedded in the binary. See [docs/adr](docs/adr/).

```sh
just                     # the menu: every recipe, grouped
just run                 # http://127.0.0.1:7400; the API key comes from ./.env (MELIOUS_API_KEY=…)
just check               # fmt, clippy, buf lint, eslint, tsc, all tests, metrics ratchets
just sheet               # rebuild the GridCraft sheet (wasm) after changing wasm/ or GridCraft
just apple-build         # FROM-scratch image with the Apple container CLI
just apple-run           # … and run it on http://127.0.0.1:7400
just metrics             # code metrics snapshot (see metrics/README.md)
```

Needs Rust (pinned in `rust-toolchain.toml`, via rustup), `protoc`, `pnpm`, Node ≥ 23, and a
GridCraft checkout next to this repo (a path dependency; path set in Cargo.toml, `GRIDCRAFT_DIR`
overrides it for `just`). `just sheet` also needs the `wasm32-unknown-unknown` target and
`wasm-bindgen` 0.2.129. `just gen` needs `buf`.

## Settings

**Chat → Settings** sets the base URL, models, key, temperature, max tokens, tool rounds and an
extra system prompt. They are stored in `.data/settings.json` (`--data` or `AGENT_DEMO_DATA_DIR`).
The key is write-only over the API. Without a stored key, the variable named in the settings
(`MELIOUS_API_KEY` by default) is used; `./.env` is loaded at startup and is never baked into the
container image (`apple-run` passes it with `--env-file`).

## How the sheet stays in sync

The server holds the authoritative workbook, a headless GridCraft engine, plus an append-only
log of engine commands (`SheetService`). Agent tools, widgets and browsers all change the
workbook through the server, and every browser replays the log into its own GridCraft
(`web/src/sheet/`, `wasm/sheet-web/src/bridge.rs`). Your edits go to the server as journaled
commands and come back to the other tabs.

- Widgets rewrite the same cells every second, so the log is compacted. Overwritten `cell.set`s
  are dropped, but ops keep their `seq`, so open tabs never reload.
- A reset, or a server restart, changes the log id, and tabs then reload the workbook.
- Concurrent edits are last-writer-wins.
- Commands that touch server files, the network or the UI (`file.*`, `data.from*`, `ui.*`, …)
  are refused for the agent and for browsers alike.

## WebAssembly widgets

`compile_wasm_widget` (and **Wasm widgets → Compile your own**) takes WebAssembly text. The widget
is compiled **and run on the server**. Browsers never run widget code. They receive its cell
writes as a live stream of spreadsheet commands, which each browser's GridCraft (WebAssembly too)
replays, recalculating formulas and redrawing as values arrive:

```
WAT ─▶ wat (compile) ─▶ wasmi (sandbox, tick every N ms) ─▶ cell writes ─▶ server workbook + op log
                                                                        │ WatchSheet stream
                                                     every browser: GridCraft wasm replays the ops
```

**[docs/execution.md](docs/execution.md)** walks through the whole path step by step. It covers
what widget code may call and what the page and the sheet can ask each other to do, and why the
widget runs on the server.

- **Imports:** `tools.<metric>() -> f64` for each metric, `tools.core_percent(i32) -> f64`,
  `sheet.set_number(row, col, f64)`, `sheet.set_text(row, col, ptr, len)` and `env.log(ptr, len)`.
- **Exports:** `tick` (required) and `init` (optional).
- **Runtime:** the server compiles the module with `wat` and runs it in `wasmi`, with 20 M
  instructions of fuel per call and at most 2000 cell writes per tick. Unchanged cells are not
  rewritten.
- **Errors:** compile and link errors go back to the model as text, so it can fix the module and
  retry. A trap stops the widget and shows the error.
- **Restart:** a stopped or failed widget restarts from its source, as a fresh instance. Use
  **Restart** on its card or the `restart_widget` tool.

## Using the workspace

- **Panels bar:** the bar above the workspace lists Chat, Sheet, Machine and Wasm widgets. The
  **–** in a panel's header minimizes it into the bar, and the other panels take its space. The
  panel's button in the bar (or **Show all**) brings it back. Minimized panels keep running: the
  chat keeps streaming and the sheet keeps syncing.
- **Resize:** drag the bars between the columns, or focus one and use the arrow keys.
  - The sheet's **corner grip** sets its width and height together, taking width from the side
    panel. With nothing to its right, the grip sets only the height. A double click on the grip
    resets the sheet to fill its column.
  - The chat takes whatever width is left.
  - Sizes and minimized panels are remembered per browser.
- **Undock ↗:** moves the sheet into a window of its own, so the chat gets the width. That
  window syncs with the server on its own. **Snapshot → sheet** still writes into it, over a
  `BroadcastChannel`. Closing the window, or **Dock**, brings the sheet back.
- **Auto-scroll:** the checkbox in the chat header keeps the newest output in view while the
  agent streams. Turn it off to read back while a run is going.
- **Settings:** endpoint and key, models, generation limits and extra system prompt.

## Layout

| Path | What |
|---|---|
| `proto/agentdemo/v1/agent.proto` | The API: `AgentService` (runs, settings), `SheetService`, `MachineService` |
| `crates/agent-demo/src/agent/llm.rs` | LLM agent: tool loop, streaming blocks, per-conversation history |
| `crates/agent-demo/src/llm.rs` | OpenAI-compatible streaming client (SSE, reasoning, pieced tool calls) |
| `crates/agent-demo/src/tools.rs` | The agent's tools (machine, `sheet_*`, widgets, plan) |
| `crates/agent-demo/src/sheet.rs` | Shared workbook: headless engine, op log, compaction, deny list |
| `crates/agent-demo/src/widgets.rs` | WAT → wasm compile, wasmi sandbox, tick loop |
| `crates/agent-demo/src/machine.rs` | sysinfo sampler (1 s) |
| `crates/agent-demo/src/settings.rs` | Settings file, `.env` loading |
| `crates/agent-demo/src/run.rs`, `agent/scripted.rs` | Run logs, approvals, cancel; offline scenarios |
| `wasm/sheet-web/` | GridCraft's egui UI for the browser, with the `postMessage` bridge |
| `web/src/workspace/` | Chat, settings, sheet pane, machine and widget panels |
| `web/src/sheet/` | Sheet sync (`sync.ts`) and the frame bridge (`useSheet.ts`) |
| `Containerfile`, `justfile` | FROM-scratch image, tasks |
| `tools/metrics.py`, `metrics/` | Code metrics and ratchets |

## Copyright

© 2026 Yannick Koechlin. Licensed under MIT OR Apache-2.0 (see [COPYRIGHT](COPYRIGHT)). GridCraft and
third-party Rust crates and npm packages keep their own terms.
