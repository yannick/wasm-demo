//! The tools the LLM agent may call: machine readings, the shared sheet (GridCraft), and
//! WebAssembly widgets that keep the sheet live.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::machine::{METRICS, Machine};
use crate::sheet::{self, Sheet};
use crate::widgets::{ABI_DOC, Widgets};

/// What a tool returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub text: String,
    pub is_error: bool,
}

impl Output {
    fn ok(v: &Value) -> Self {
        Self {
            text: serde_json::to_string_pretty(v).unwrap_or_default(),
            is_error: false,
        }
    }

    fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }
}

/// Origin of the agent's sheet edits in the op log.
pub const AGENT_ORIGIN: &str = "agent";

#[derive(Debug)]
pub struct Tools {
    machine: Machine,
    sheet: Arc<Sheet>,
    widgets: Arc<Widgets>,
    /// Built once: the sheet's definitions come from gridcraft's catalog.
    definitions: Vec<Value>,
}

impl Tools {
    pub fn new(machine: Machine, sheet: Arc<Sheet>, widgets: Arc<Widgets>) -> Self {
        let mut definitions = own_definitions();
        definitions.push(crate::agent::llm::plan_definition());
        definitions.extend(sheet::tool_definitions());
        Self {
            machine,
            sheet,
            widgets,
            definitions,
        }
    }

    /// OpenAI function definitions of every tool.
    pub fn definitions(&self) -> &[Value] {
        &self.definitions
    }

    /// Runs tool `name` with `arguments` (JSON text from the model).
    pub fn call(&self, name: &str, arguments: &str) -> Output {
        let args: Value = if arguments.trim().is_empty() {
            json!({})
        } else {
            match serde_json::from_str(arguments) {
                Ok(v) => v,
                Err(e) => {
                    return Output::error(format!(
                        "arguments are not valid JSON ({e}); send a JSON object"
                    ));
                }
            }
        };
        match name {
            "get_system_metrics" => Output::ok(&self.machine.report()),
            "list_processes" => {
                let by_memory = args.get("sort_by").and_then(Value::as_str) == Some("memory");
                let limit = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map_or(10, |n| usize::try_from(n).unwrap_or(10));
                Output::ok(&self.machine.processes(by_memory, limit))
            }
            "compile_wasm_widget" | "stop_widget" | "restart_widget" | "list_widgets" => {
                self.widget_tool(name, &args)
            }
            "sheet_reset" => {
                self.sheet.reset();
                Output::ok(&json!({ "reset": true }))
            }
            other => match other.strip_prefix("sheet_") {
                Some(tool) => {
                    let result = self.sheet.tool(AGENT_ORIGIN, tool, &args);
                    Output {
                        text: text_of(&result.content),
                        is_error: result.is_error,
                    }
                }
                None => Output::error(format!("unknown tool {other:?}")),
            },
        }
    }

    fn widget_tool(&self, name: &str, args: &Value) -> Output {
        match name {
            "compile_wasm_widget" => self.compile(args),
            "stop_widget" => {
                let name = str_arg(args, "name");
                let remove = args.get("remove").and_then(Value::as_bool).unwrap_or(false);
                if self.widgets.stop(name, remove) {
                    Output::ok(&json!({ "stopped": name, "removed": remove }))
                } else {
                    Output::error(format!("no widget named {name:?}"))
                }
            }
            "restart_widget" => match self.widgets.restart(str_arg(args, "name")) {
                Ok(w) => Output::ok(&json!({ "restarted": true, "widget": widget_json(&w) })),
                Err(e) => Output::error(e.to_string()),
            },
            _ => Output::ok(&json!(
                self.widgets
                    .list()
                    .iter()
                    .map(widget_json)
                    .collect::<Vec<_>>()
            )),
        }
    }

    fn compile(&self, args: &Value) -> Output {
        let interval = args
            .get("interval_ms")
            .and_then(Value::as_u64)
            .map_or(1000, |n| u32::try_from(n).unwrap_or(u32::MAX));
        match self.widgets.compile(
            str_arg(args, "name"),
            str_arg(args, "description"),
            str_arg(args, "wat"),
            interval,
        ) {
            Ok(w) => Output::ok(&json!({
                "compiled": true,
                "widget": widget_json(&w),
                "note": "It is running; its writes appear in the sheet every interval. Check list_widgets for errors.",
            })),
            Err(e) => Output::error(format!(
                "{e}\n\nFix the module and call compile_wasm_widget again."
            )),
        }
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn widget_json(w: &agent_demo_proto::agentdemo::v1::Widget) -> Value {
    json!({
        "name": w.name,
        "description": w.description,
        "state": format!("{:?}", w.state.as_known().unwrap_or_default()).trim_start_matches("WIDGET_STATE_").to_lowercase(),
        "interval_ms": w.interval_ms,
        "ticks": w.ticks,
        "writes": w.writes,
        "wasm_bytes": w.wasm_bytes,
        "error": w.error,
        "log": w.log,
    })
}

/// MCP content items as one text: the text items joined, anything else as JSON.
fn text_of(content: &[Value]) -> String {
    content
        .iter()
        .map(|item| {
            item.get("text")
                .and_then(Value::as_str)
                .map_or_else(|| item.to_string(), str::to_owned)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn function(name: &str, description: &str, parameters: &Value) -> Value {
    json!({ "type": "function", "function": { "name": name, "description": description, "parameters": parameters } })
}

fn own_definitions() -> Vec<Value> {
    let metric_list = METRICS
        .iter()
        .map(|(name, doc, _)| format!("{name} ({doc})"))
        .collect::<Vec<_>>()
        .join(", ");
    let no_params = json!({ "type": "object", "properties": {} });
    vec![
        function(
            "get_system_metrics",
            &format!(
                "Live readings of the machine this server runs on (sampled every second): {metric_list}; plus per-core CPU load."
            ),
            &no_params,
        ),
        function(
            "list_processes",
            "The busiest processes on the machine, by CPU (default) or by memory.",
            &json!({ "type": "object", "properties": {
                "sort_by": { "type": "string", "enum": ["cpu", "memory"] },
                "limit": { "type": "integer", "minimum": 1, "maximum": 30, "description": "default 10" },
            }}),
        ),
        function(
            "compile_wasm_widget",
            &format!(
                "Compile a WebAssembly widget from WebAssembly text (WAT) and run it on the server every interval_ms. \
                 It reads live machine metrics through imported functions and writes them into cells of the shared sheet, \
                 which every user sees update live. Use it to make a sheet live after laying it out with the sheet_ tools. \
                 A widget with the same name is replaced. Compile errors come back as text: fix and retry. {ABI_DOC}"
            ),
            &json!({ "type": "object", "required": ["name", "wat"], "properties": {
                "name": { "type": "string", "description": "short id: a-z, 0-9, - and _" },
                "description": { "type": "string", "description": "what it shows, one line" },
                "wat": { "type": "string", "description": "the module in WebAssembly text format" },
                "interval_ms": { "type": "integer", "minimum": 250, "maximum": 60000, "description": "default 1000" },
            }}),
        ),
        function(
            "stop_widget",
            "Stop a running widget (its cells keep their last values); remove: true also deletes it.",
            &json!({ "type": "object", "required": ["name"], "properties": {
                "name": { "type": "string" },
                "remove": { "type": "boolean" },
            }}),
        ),
        function(
            "restart_widget",
            "Start a stopped or failed widget again from its source (a fresh instance).",
            &json!({ "type": "object", "required": ["name"], "properties": { "name": { "type": "string" } }}),
        ),
        function(
            "list_widgets",
            "Every widget with its state, tick count, errors and log lines.",
            &no_params,
        ),
        function(
            "sheet_reset",
            "Start over with a blank workbook (every client reloads it). Stop widgets that write to it first.",
            &no_params,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::Sample;

    fn tools() -> Tools {
        let machine = Machine::fixed(Sample::default());
        let sheet = Sheet::new();
        let widgets = Widgets::new(machine.clone(), Arc::clone(&sheet));
        Tools::new(machine, sheet, widgets)
    }

    #[test]
    fn definitions_are_named_and_unique() {
        let tools = tools();
        let mut names: Vec<_> = tools
            .definitions()
            .iter()
            .map(|d| d["function"]["name"].as_str().unwrap().to_owned())
            .collect();
        assert!(names.iter().any(|n| n == "sheet_write_range"), "{names:?}");
        let count = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), count);
    }

    #[test]
    fn calls_route_and_explain_mistakes() {
        let tools = tools();
        assert!(!tools.call("get_system_metrics", "").is_error);
        let write = tools.call(
            "sheet_write_range",
            r#"{"range":"A1","values":[["CPU",1]]}"#,
        );
        assert!(!write.is_error, "{}", write.text);
        assert!(
            tools
                .call("sheet_read_range", r#"{"range":"A1:B1"}"#)
                .text
                .contains("CPU")
        );
        assert!(
            tools
                .call("get_system_metrics", "{oops")
                .text
                .contains("not valid JSON")
        );
        assert!(tools.call("nope", "{}").is_error);
        assert!(tools.call("stop_widget", r#"{"name":"none"}"#).is_error);
    }
}
