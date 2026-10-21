# Where code is compiled, where it runs, and how results reach the browser

Two pieces of WebAssembly are involved, and they run in different places:

| | Widget (the agent's code) | Sheet (GridCraft) |
|---|---|---|
| Written by | the LLM (or you, in **Wasm widgets → Compile your own**), as WebAssembly text | this repo (`wasm/sheet-web`), Rust built with `just sheet` |
| Compiled | **on the server**, at request time: `wat` turns the text into a module | ahead of time, embedded in the binary |
| Runs | **on the server**, in `wasmi` (an interpreter), one task per widget | **in each browser**, in an iframe (`/static/sheet/index.html`) |
| Can reach | only the host functions listed below | only its page, through `postMessage` |

The widget does not run in the browser. What the browser runs is the effect of the widget: a
stream of spreadsheet commands that every open GridCraft replays live. The sections below follow
one value from the widget to the screen.

## 1. Compile (server)

`compile_wasm_widget` (agent tool) or `MachineService.CompileWidget` (UI) calls
`Widgets::compile` (`crates/agent-demo/src/widgets.rs`):

1. **Check the input.** The name must be 1–64 characters of letters, digits, `-` and `_`. The text may be at most 256 KiB, and
   the interval is clamped to 250–60 000 ms.
2. **Compile.** `wat::parse_str` turns the WAT into WebAssembly bytes. Syntax errors come back to
   the model as text, so it can fix them and call the tool again.
3. **Validate and link.** `wasmi::Module::new` validates the module. The `Linker` then offers
   only the imports below. Importing anything else, or leaving out the `tick` export, fails here,
   and that error goes back to the model too.
4. **Start.** The module is instantiated with a fresh `Store`, and the optional `init` runs once.
   Then a tokio task calls `tick` every interval. A widget with the same name is replaced.

## 2. Run (server)

On every tick:

1. The task takes the latest machine sample. It is sampled once a second by `machine.rs`, so all
   imports in one tick see the same instant.
2. It gives `tick` a budget of 20 M instructions of fuel. Running out of fuel, or any trap,
   stops the widget, and its card shows the error. **Restart** (or the `restart_widget` tool)
   builds a fresh instance from the same source.
3. Writes are collected, not applied one by one. At most 2 000 cells per tick are allowed, and
   cells whose value didn't change are dropped.
4. The rest go to the shared workbook in one batch: `Sheet::set_cells` with origin
   `widget:<name>`.

### What widget code can call

These are the only functions a widget can call. There is no WASI, no clock beyond `time_ms`, no
file system, no network, and no access to other widgets or the LLM.

| Import | Signature | Does |
|---|---|---|
| `tools.<metric>` | `() -> f64` | One reading from the current sample. The metrics are `cpu_percent`, `core_count`, `memory_used_mb`, `memory_total_mb`, `memory_percent`, `swap_used_mb`, `load_1`, `load_5`, `load_15`, `uptime_s`, `process_count`, `disk_used_gb`, `disk_total_gb`, `disk_percent`, `net_rx_kbps`, `net_tx_kbps` and `time_ms` (the list is `machine::METRICS`). |
| `tools.core_percent` | `(core: i32) -> f64` | The load of one logical core, 0-based. It returns 0 when the core doesn't exist. |
| `sheet.set_number` | `(row: i32, col: i32, value: f64)` | Writes a number, rounded to 2 decimals. Rows and columns start at 1, so A1 is `1, 1`. |
| `sheet.set_text` | `(row: i32, col: i32, ptr: i32, len: i32)` | Writes UTF-8 text from the exported `memory`. Text that starts with `=` is a formula, which each browser's GridCraft recalculates. |
| `env.log` | `(ptr: i32, len: i32)` | Adds a line to the widget's log, which shows on its card and in `list_widgets`. |

| Export | |
|---|---|
| `tick` | Required. Called every `interval_ms`. |
| `init` | Optional. Called once, after instantiation. |
| `memory` | Required when the widget uses `set_text` or `log`. |

State that must survive between ticks lives in mutable globals or in memory. For example, a row
counter lets a widget append one row per tick.

## 3. Stream (server → browser)

`Sheet::set_cells` turns the batch into `cell.set` engine commands and applies them to the
server's headless GridCraft, which is the authoritative workbook. It also appends them to the op
log as `SheetOp { seq, command, params_json, origin }`.

Every browser holds one long-lived `SheetService.WatchSheet` server stream (ConnectRPC, binary
protobuf):

- The first response is the whole log, or only the part after the `seq` the tab already has.
  After that, each change arrives as soon as it is appended.
- A widget that rewrites the same cells every second would grow the log forever. Compaction
  drops the overwritten `cell.set`s and keeps the `seq` numbers, so open tabs simply carry on.
- A reset, or a server restart, changes the `log_id`, and the tab then reloads the workbook from
  scratch.

Agent tool calls (`sheet_*`) and other people's edits travel the same way. Only the origin
differs: `agent`, `widget:<name>`, or the id of a browser tab.

## 4. Execute (browser)

`web/src/sheet/sync.ts` receives the ops. It skips the ops that this tab sent itself and forwards
the rest to the iframe as `sheet:apply`. In the iframe, GridCraft compiled to wasm (with its egui
UI) runs each command against its own engine. That engine recalculates dependent formulas, so a
`=AVERAGE(B2:B60)` next to a live column updates in the browser, and it redraws the grid,
formats and charts. So the client executes spreadsheet commands, not widget code.

The other direction works the same way. Your own edits leave the iframe as `sheet:changed`
(taken from the engine's journal) and go to the server with `ApplySheetOps`. The server checks
them against the same deny list as the agent, applies them, and streams them to the other tabs.

### What the page can ask the sheet to do

The page talks to the iframe only via `postMessage`, and only with its own origin
(`wasm/sheet-web/src/bridge.rs`):

| Message | Direction | |
|---|---|---|
| `sheet:ready` / `sheet:failed` | sheet → page | Started, with its version, or why it could not start. |
| `sheet:apply` | page → sheet | Ops from the server log. They run as given and are not reported back. |
| `sheet:call` / `sheet:reply` | page → sheet → page | Any method of GridCraft's control channel: `engine.execute`, `sheet.read`, `document.inspect`, …. The **Snapshot → sheet** button uses it. |
| `sheet:changed` | sheet → page | Local edits, to be sent to the server. |

When the sheet is undocked, it lives in its own window. The main window then sends its
`sheet:call`s over a `BroadcastChannel` (`web/src/sheet/remote.ts`), and the sheet window
forwards them to its iframe.

### What neither side may run

The server's deny list applies to the agent's commands and to every browser's ops. It refuses
commands that touch files, the network or the UI: `file.*`, `app.*`, `ui.*`, `options.*`,
`data.from*`, `data.csv`/`tsv`/`xlsx`, data refresh, and the `view.*` commands except the
workbook view settings (freeze panes, gridlines, headings, zeros, zoom). A refused op is
reported back to the tab and isn't logged. That keeps the commands every browser replays down
to workbook edits.

## Why the widget runs on the server

- **The readings come from the server.** CPU, memory and processes are the server's own; a
  browser could only fetch them over the network again.
- **It keeps running when no tab is open.** A widget fills the sheet even with every tab
  closed, and late joiners see the history in the log.
- **The sandbox is easy to reason about.** Fuel limits, a short import list and no WASI keep
  code the model wrote on the fly contained, and a misbehaving widget is stopped with an error
  the model can read.
- **The workbook stays consistent.** There is one writer per widget, and every browser sees
  the same cells.

Running widgets in the browser instead would be a small change. The same bytes could be sent
down and instantiated with `WebAssembly.instantiate`, with JS imports that call `WatchMetrics`
and write via `sheet:call`. But each open tab would then run its own copy, so the tabs would
either need to elect one writer, or keep their results local and unshared.
