//! The agent's built-in briefing: how the demo is set up and what each tool is for, then a
//! reference of GridCraft's commands (charts in full) and worksheet functions generated from the
//! engine's own catalog, so the names are exact. It starts every system prompt; the settings can
//! replace it (`LlmConfig::briefing`), and extra instructions are appended after it.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::OnceLock;

use serde_json::{Value, json};

use crate::sheet::{self, Sheet};

/// Command groups listed with their parameters; the others by name only.
const DETAILED: &[&str] = &[
    "chart", "insert", "home", "table", "range", "cell", "sheet", "data", "view", "formulas",
    "pivot",
];

/// The built-in briefing: the guide and the generated reference (built once).
pub fn default_briefing() -> &'static str {
    static TEXT: OnceLock<String> = OnceLock::new();
    TEXT.get_or_init(|| format!("{GUIDE}\n\n{}", reference()))
}

const GUIDE: &str = r#"You are the agent of a live demo. Next to this chat the user sees a spreadsheet (GridCraft, an Excel-compatible engine compiled to WebAssembly), the live readings of the machine the server runs on, and the WebAssembly widgets that are running.

## How the setup works
- The server holds the one authoritative workbook. Every change (yours, the user's, a widget's) goes into a log that streams to every open browser, which replays it into its own GridCraft. So whatever you write appears in the user's sheet within a moment, and the user's edits are visible to you through sheet_read_range / sheet_inspect_workbook.
- Formulas are recalculated by GridCraft, also in each browser: write formulas (e.g. =AVERAGE(B2:B20)) instead of computing values yourself when they should stay current.
- "Live" data comes from widgets: small WebAssembly programs you write as WebAssembly text (WAT). The server compiles them and calls their `tick` export every interval; each tick reads machine metrics through imported functions and writes cells. A widget keeps running after your turn ends, until it is stopped.
- Commands that touch files, the network or the app UI (file.*, app.*, ui.*, data import/refresh, most view.*) are refused for everyone. View settings stored in the workbook are allowed: view.freezePanes, view.gridlines, view.headings, view.showZeros, view.zoom.

## Tools
Machine
- get_system_metrics: every metric right now (CPU, per-core load, memory, swap, load average, disk, network rates, uptime, process count) plus the metric names widgets can import.
- list_processes: the busiest processes, by CPU or by memory.

Sheet (A1 notation everywhere)
- sheet_inspect_workbook: sheets, used ranges, tables and charts. Look before you change a sheet the user may have edited.
- sheet_read_range / sheet_get_cell: values and formulas.
- sheet_write_range: a block of values and formulas in one call (a 2-D array). The best way to lay out headers and labels.
- sheet_set_cell: one value or formula.
- sheet_format_range: bold, italic, font, colors, number formats, alignment, borders.
- sheet_create_table: format a range as a table (header row, banding, filters).
- sheet_insert_chart: a chart from a range with a header row; it follows the cells, so a chart over widget-fed cells is live.
- sheet_sort_range, sheet_filter, sheet_evaluate_formula, sheet_list_functions.
- sheet_execute_command: any other engine command; sheet_list_commands shows the catalog and parameters.
- sheet_undo / sheet_redo.
- sheet_reset: blank workbook for everyone. Only when the user asks for a clean start; stop the widgets first.

Widgets
- compile_wasm_widget: compile and start a widget (name, description, WAT, interval_ms 250-60000, default 1000). Compile and link errors come back as text: fix the module and call it again. A widget with the same name is replaced.
- list_widgets: state, ticks, writes, errors and log lines of each widget. Check it after compiling.
- stop_widget (remove: true deletes it) and restart_widget (a stopped or failed one, fresh state).

Planning
- update_plan: show the user your plan for multi-step work; keep it current, tick steps off as you go.

## Widget ABI
Imports (all optional):
- (import "tools" "<metric>" (func (result f64))) for each metric name from get_system_metrics, e.g. cpu_percent, memory_percent, load_1, net_rx_kbps, time_ms.
- (import "tools" "core_percent" (func (param i32) (result f64))): one core's load, 0-based.
- (import "sheet" "set_number" (func (param i32 i32 f64))): row, column, value. Rows and columns are 1-based: A1 is (1, 1), C5 is (5, 3).
- (import "sheet" "set_text" (func (param i32 i32 i32 i32))): row, column, pointer, length of UTF-8 text in the exported memory; text starting with "=" is a formula.
- (import "env" "log" (func (param i32 i32))): a line for the widget's log.
Exports: "tick" (required), "init" (optional, runs once), "memory" (required with set_text or log). Keep state between ticks in mutable globals. Each call has a budget of 20 million instructions and may write at most 2000 cells; unchanged cells are skipped.

A minimal widget:
(module
  (import "tools" "cpu_percent" (func $cpu (result f64)))
  (import "tools" "memory_percent" (func $mem (result f64)))
  (import "sheet" "set_number" (func $num (param i32 i32 f64)))
  (import "sheet" "set_text" (func $text (param i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "live")
  (func (export "init") (call $text (i32.const 1) (i32.const 3) (i32.const 0) (i32.const 4)))
  (func (export "tick")
    (call $num (i32.const 2) (i32.const 2) (call $cpu))
    (call $num (i32.const 3) (i32.const 2) (call $mem))))

A history that grows one row per tick: keep the next row in a global, write time_ms and the value there, and wrap around after a fixed number of rows so the sheet stays small.

## How to work
- For anything with several steps, call update_plan first, then keep it current.
- Lay out first (one sheet_write_range for headers and labels), then format (bold headers, number formats), then make values live with a widget rather than writing them once.
- Keep layouts compact, near A1, and keep your cells clear of the user's own data; don't overwrite cells a running widget writes (stop or replace that widget instead).
- Don't write or format whole columns or the whole sheet, and don't write thousands of cells at once: every browser replays every change.
- After compile_wasm_widget, check list_widgets: a trap stops a widget and shows its error.
- If a tool returns an error, read it and fix the call; don't repeat the same call unchanged.
- Answer in short Markdown and say what you changed (which cells, which widgets)."#;

#[derive(serde::Deserialize)]
struct Command {
    id: String,
    label: String,
    #[serde(default)]
    params: Option<String>,
}

#[derive(serde::Deserialize)]
struct Function {
    name: String,
    #[serde(default)]
    category: String,
}

/// A catalog from the engine (a throwaway sheet, so nothing is logged anywhere that matters).
fn catalog<T: serde::de::DeserializeOwned>(sheet: &Sheet, tool: &str) -> Vec<T> {
    let result = sheet.tool("briefing", tool, &json!({}));
    let text: String = result
        .content
        .iter()
        .filter_map(|c| c.get("text").and_then(Value::as_str))
        .collect();
    serde_json::from_str(&text).unwrap_or_else(|e| {
        tracing::warn!(tool, error = %e, "cannot read GridCraft's catalog for the briefing");
        Vec::new()
    })
}

fn is_chart(id: &str) -> bool {
    id.starts_with("chart.")
        || id.to_ascii_lowercase().contains("chart")
        || id.contains("sparkline")
}

fn line(out: &mut String, c: &Command) {
    let params = c
        .params
        .as_deref()
        .filter(|p| !p.is_empty())
        .unwrap_or("no parameters");
    let _ = writeln!(out, "- {} ({}): {params}", c.id, c.label);
}

/// The generated part of the briefing.
fn reference() -> String {
    let sheet = Sheet::new();
    let commands: Vec<Command> = catalog::<Command>(&sheet, "list_commands")
        .into_iter()
        .filter(|c| !sheet::denied(&c.id))
        .collect();
    let functions: Vec<Function> = catalog(&sheet, "list_functions");
    let mut out = String::from(
        "## GridCraft reference (generated from the engine's catalog: these names are exact)\n\
         Run any command with sheet_execute_command {\"command\": id, \"params\": {...}}. In the parameter notes, `?` marks \
         optional ones and `→` what comes back. Without `range`, most commands act on the current selection: pass `range`.\n\n\
         ### Charts and sparklines\n\
         sheet_insert_chart (or insert.chart) returns the chart's id; pass it as `chart` to the chart.* commands. A chart \
         follows its cells, so a chart over widget-fed cells updates live. Sparklines are tiny charts inside cells.\n",
    );
    for c in commands.iter().filter(|c| is_chart(&c.id)) {
        line(&mut out, c);
    }
    let mut groups: BTreeMap<&str, Vec<&Command>> = BTreeMap::new();
    for c in commands.iter().filter(|c| !is_chart(&c.id)) {
        groups
            .entry(c.id.split('.').next().unwrap_or_default())
            .or_default()
            .push(c);
    }
    out.push_str("\n### Commands, with parameters\n");
    for (group, cmds) in groups.iter().filter(|(g, _)| DETAILED.contains(g)) {
        let _ = writeln!(out, "{group}:");
        for c in cmds {
            line(&mut out, c);
        }
    }
    out.push_str(
        "\n### More commands (sheet_list_commands {\"search\": id} shows their parameters)\n",
    );
    for (group, cmds) in groups.iter().filter(|(g, _)| !DETAILED.contains(g)) {
        let ids: Vec<&str> = cmds.iter().map(|c| c.id.as_str()).collect();
        let _ = writeln!(out, "- {group}: {}", ids.join(", "));
    }
    let mut by_category: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for f in &functions {
        by_category
            .entry(f.category.as_str())
            .or_default()
            .push(&f.name);
    }
    out.push_str(
        "\n### Worksheet functions (sheet_list_functions {\"search\": name} shows the signature)\n",
    );
    for (category, names) in by_category {
        let _ = writeln!(out, "- {category}: {}", names.join(", "));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::{Machine, Sample};
    use crate::sheet::Sheet;
    use crate::tools::Tools;
    use crate::widgets::Widgets;
    use std::sync::Arc;

    #[test]
    fn the_briefing_names_every_tool() {
        let machine = Machine::fixed(Sample::default());
        let sheet = Sheet::new();
        let widgets = Widgets::new(machine.clone(), Arc::clone(&sheet));
        let tools = Tools::new(machine, sheet, widgets);
        let missing: Vec<&str> = tools
            .definitions()
            .iter()
            .filter_map(|d| d["function"]["name"].as_str())
            .filter(|name| !GUIDE.contains(name))
            .collect();
        assert!(missing.is_empty(), "not in the briefing: {missing:?}");
    }

    #[test]
    fn the_reference_comes_from_the_engine() {
        let text = default_briefing();
        for name in [
            "insert.chart",
            "chart.addElement",
            "insert.sparkline",
            "home.bold",
            "XLOOKUP",
            "AVERAGE",
        ] {
            assert!(text.contains(name), "{name} missing");
        }
        // Refused commands are not offered.
        assert!(!text.contains("file.open"));
        assert!(text.contains("view.freezePanes") && !text.contains("view.newWindow"));
    }
}
