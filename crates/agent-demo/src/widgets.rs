//! Widgets: small WebAssembly programs the agent writes (as WebAssembly text), compiled on the
//! server and run in a sandbox (wasmi, fuel-limited) on an interval. They read the machine's
//! metrics through imported functions and write cells of the shared sheet.
//!
//! The ABI (also given to the agent in the `compile_wasm_widget` tool description):
//!
//! ```wat
//! (import "tools" "cpu_percent" (func (result f64)))       ;; any name in machine::METRICS
//! (import "tools" "core_percent" (func (param i32) (result f64)))  ;; one core, 0-based
//! (import "sheet" "set_number" (func (param i32 i32 f64)))  ;; row, col (1-based), value
//! (import "sheet" "set_text" (func (param i32 i32 i32 i32)));; row, col, ptr, len (UTF-8 in "memory");
//!                                                           ;; text starting with "=" is a formula
//! (import "env" "log" (func (param i32 i32)))               ;; ptr, len
//! (export "init" (func))   ;; optional, once at start
//! (export "tick" (func))   ;; every interval_ms
//! (export "memory" (memory 1))  ;; needed by set_text and log
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use agent_demo_proto::agentdemo::v1 as pb;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use wasmi::{Caller, Config, Engine, Extern, Instance, Linker, Module, Store, TypedFunc};

use crate::machine::{METRICS, Machine, Sample};
use crate::sheet::Sheet;

/// Instructions one `init` or `tick` call may run.
const FUEL_PER_CALL: u64 = 20_000_000;
const MIN_INTERVAL_MS: u32 = 250;
const MAX_INTERVAL_MS: u32 = 60_000;
const MAX_WAT_BYTES: usize = 256 * 1024;
const MAX_LOG_LINES: usize = 20;
/// Cells one tick may write; more is almost certainly a bug in the widget.
const MAX_WRITES_PER_TICK: usize = 2_000;

pub const ABI_DOC: &str = "Widget ABI (WebAssembly text). Imports, all optional: \
(import \"tools\" \"<metric>\" (func (result f64))) for each metric listed by get_system_metrics \
(cpu_percent, core_count, memory_used_mb, memory_total_mb, memory_percent, swap_used_mb, load_1, load_5, load_15, \
uptime_s, process_count, disk_used_gb, disk_total_gb, disk_percent, net_rx_kbps, net_tx_kbps, time_ms); \
(import \"tools\" \"core_percent\" (func (param i32) (result f64))) one core, 0-based; \
(import \"sheet\" \"set_number\" (func (param i32 i32 f64))) row, col (1-based: A1 is 1 1), value; \
(import \"sheet\" \"set_text\" (func (param i32 i32 i32 i32))) row, col, ptr, len of UTF-8 bytes in the exported memory, \
text starting with = is a formula; (import \"env\" \"log\" (func (param i32 i32))) ptr, len. \
Exports: (func (export \"tick\")) required, called every interval_ms; (func (export \"init\")) optional, called once; \
(memory (export \"memory\") 1) required when using set_text or log (put strings in (data (i32.const 0) \"...\")). \
Values are rounded to 2 decimals; unchanged cells are not rewritten. Each call gets 20M instructions of fuel. \
Keep state in mutable globals, e.g. (global $row (mut i32) (i32.const 2)) to append a row per tick.";

#[derive(Debug, thiserror::Error)]
pub enum WidgetError {
    #[error("{0}")]
    Invalid(String),
    #[error("WebAssembly text does not compile: {0}")]
    Compile(String),
    #[error("module is not a valid widget: {0}")]
    Link(String),
    #[error("init failed: {0}")]
    Init(String),
}

#[derive(Debug)]
struct Entry {
    widget: pb::Widget,
    stop: CancellationToken,
}

#[derive(Debug)]
pub struct Widgets {
    engine: Engine,
    machine: Machine,
    sheet: Arc<Sheet>,
    entries: Mutex<HashMap<String, Entry>>,
    changes: broadcast::Sender<pb::WatchWidgetsResponse>,
}

impl Widgets {
    pub fn new(machine: Machine, sheet: Arc<Sheet>) -> Arc<Self> {
        let mut config = Config::default();
        config.consume_fuel(true);
        Arc::new(Self {
            engine: Engine::new(&config),
            machine,
            sheet,
            entries: Mutex::new(HashMap::new()),
            changes: broadcast::channel(256).0,
        })
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn list(&self) -> Vec<pb::Widget> {
        let mut all: Vec<_> = self.lock().values().map(|e| e.widget.clone()).collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        all
    }

    /// Changes from now on; subscribe before [`Widgets::list`] so none falls in between.
    pub fn subscribe(&self) -> broadcast::Receiver<pb::WatchWidgetsResponse> {
        self.changes.subscribe()
    }

    /// Compiles `wat`, runs its `init`, and starts ticking; replaces a widget of the same name.
    pub fn compile(
        self: &Arc<Self>,
        name: &str,
        description: &str,
        wat: &str,
        interval_ms: u32,
    ) -> Result<pb::Widget, WidgetError> {
        let result = self.start(name, description, wat, interval_ms);
        crate::activity::record(
            "widget",
            serde_json::json!({
                "name": name,
                "event": if result.is_ok() { "compiled" } else { "compile_failed" },
                "interval_ms": interval_ms,
                "error": result.as_ref().err().map(ToString::to_string),
                "wat": crate::activity::cut(wat),
            }),
        );
        result
    }

    fn start(
        self: &Arc<Self>,
        name: &str,
        description: &str,
        wat: &str,
        interval_ms: u32,
    ) -> Result<pb::Widget, WidgetError> {
        let name = name.trim();
        if name.is_empty()
            || name.len() > 64
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(WidgetError::Invalid(
                "name must be 1-64 characters of a-z, 0-9, - and _".into(),
            ));
        }
        if wat.len() > MAX_WAT_BYTES {
            return Err(WidgetError::Invalid(format!(
                "source is over {MAX_WAT_BYTES} bytes"
            )));
        }
        let wasm = wat::parse_str(wat).map_err(|e| WidgetError::Compile(e.to_string()))?;
        let module =
            Module::new(&self.engine, &wasm).map_err(|e| WidgetError::Compile(e.to_string()))?;
        let mut runner = Runner::new(self, name, &module)?;
        runner.init()?;
        let widget = pb::Widget {
            name: name.to_owned(),
            description: description.to_owned(),
            wat: wat.to_owned(),
            interval_ms: if interval_ms == 0 {
                1000
            } else {
                interval_ms.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS)
            },
            state: pb::WidgetState::WIDGET_STATE_RUNNING.into(),
            wasm_bytes: u32::try_from(wasm.len()).unwrap_or(u32::MAX),
            writes: runner.writes,
            started_at_ms: self.machine.latest().metrics.at_ms,
            log: runner.take_log(),
            ..Default::default()
        };
        let stop = CancellationToken::new();
        if let Some(old) = self.lock().insert(
            name.to_owned(),
            Entry {
                widget: widget.clone(),
                stop: stop.clone(),
            },
        ) {
            old.stop.cancel();
        }
        self.publish(&widget, false);
        tokio::spawn(Arc::clone(self).tick_forever(
            runner,
            Duration::from_millis(u64::from(widget.interval_ms)),
            stop,
        ));
        tracing::info!(widget = name, bytes = wasm.len(), "widget started");
        Ok(widget)
    }

    /// Compiles a widget's source again and starts it, a fresh instance (its globals start over).
    pub fn restart(self: &Arc<Self>, name: &str) -> Result<pb::Widget, WidgetError> {
        let source = self.lock().get(name).map(|e| {
            (
                e.widget.description.clone(),
                e.widget.wat.clone(),
                e.widget.interval_ms,
            )
        });
        let (description, wat, interval_ms) =
            source.ok_or_else(|| WidgetError::Invalid(format!("no widget named {name:?}")))?;
        self.compile(name, &description, &wat, interval_ms)
    }

    /// Stops (or removes) a widget; `false` if there is none.
    pub fn stop(&self, name: &str, remove: bool) -> bool {
        let mut entries = self.lock();
        let Some(entry) = entries.get_mut(name) else {
            return false;
        };
        entry.stop.cancel();
        crate::activity::record(
            "widget",
            serde_json::json!({ "name": name, "event": if remove { "removed" } else { "stopped" } }),
        );
        if remove {
            let entry = entries.remove(name).expect("present");
            drop(entries);
            self.publish(
                &pb::Widget {
                    name: entry.widget.name,
                    ..Default::default()
                },
                true,
            );
        } else {
            if entry.widget.state == pb::WidgetState::WIDGET_STATE_RUNNING {
                entry.widget.state = pb::WidgetState::WIDGET_STATE_STOPPED.into();
            }
            let widget = entry.widget.clone();
            drop(entries);
            self.publish(&widget, false);
        }
        true
    }

    fn publish(&self, widget: &pb::Widget, removed: bool) {
        let _ = self.changes.send(pb::WatchWidgetsResponse {
            widget: widget.clone().into(),
            removed,
            ..Default::default()
        });
    }

    async fn tick_forever(
        self: Arc<Self>,
        mut runner: Runner,
        every: Duration,
        stop: CancellationToken,
    ) {
        let mut interval = tokio::time::interval(every);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = stop.cancelled() => return,
                _ = interval.tick() => {}
            }
            let result = runner.tick(&self.machine.latest());
            let mut entries = self.lock();
            // Replaced or removed meanwhile: this runner is done.
            let Some(entry) = entries
                .get_mut(&runner.name)
                .filter(|e| !e.stop.is_cancelled())
            else {
                return;
            };
            let w = &mut entry.widget;
            w.ticks += 1;
            w.writes = runner.writes;
            w.log.extend(runner.take_log());
            let excess = w.log.len().saturating_sub(MAX_LOG_LINES);
            w.log.drain(..excess);
            if let Err(e) = &result {
                w.state = pb::WidgetState::WIDGET_STATE_FAILED.into();
                w.error.clone_from(e);
            }
            let widget = w.clone();
            drop(entries);
            self.publish(&widget, false);
            if let Err(error) = result {
                tracing::warn!(widget = %runner.name, %error, "widget failed");
                crate::activity::record(
                    "widget",
                    serde_json::json!({ "name": runner.name, "event": "failed", "error": error }),
                );
                return;
            }
        }
    }
}

/// Host state of one widget instance.
#[derive(Debug, Default)]
struct Host {
    sample: Arc<Sample>,
    pending: Vec<(String, String)>,
    log: Vec<String>,
    error: Option<String>,
}

struct Runner {
    name: String,
    origin: String,
    sheet: Arc<Sheet>,
    store: Store<Host>,
    instance: Instance,
    tick: TypedFunc<(), ()>,
    /// What each cell was last set to, to skip unchanged writes.
    last: HashMap<String, String>,
    writes: u64,
}

impl Runner {
    fn new(widgets: &Widgets, name: &str, module: &Module) -> Result<Self, WidgetError> {
        let mut store = Store::new(
            &widgets.engine,
            Host {
                sample: widgets.machine.latest(),
                ..Host::default()
            },
        );
        let linker = linker(&widgets.engine).map_err(|e| WidgetError::Link(e.to_string()))?;
        store
            .set_fuel(FUEL_PER_CALL)
            .map_err(|e| WidgetError::Link(e.to_string()))?;
        let instance = linker
            .instantiate_and_start(&mut store, module)
            .map_err(|e| WidgetError::Link(link_hint(&e.to_string())))?;
        let tick = instance
            .get_typed_func::<(), ()>(&store, "tick")
            .map_err(|_| {
                WidgetError::Link(
                    "it must export a function \"tick\" with no params and no results".into(),
                )
            })?;
        Ok(Self {
            name: name.to_owned(),
            origin: format!("widget:{name}"),
            sheet: Arc::clone(&widgets.sheet),
            store,
            instance,
            tick,
            last: HashMap::new(),
            writes: 0,
        })
    }

    fn init(&mut self) -> Result<(), WidgetError> {
        let Ok(init) = self.instance.get_typed_func::<(), ()>(&self.store, "init") else {
            return Ok(());
        };
        self.call(init).map_err(WidgetError::Init)
    }

    fn tick(&mut self, sample: &Arc<Sample>) -> Result<(), String> {
        self.store.data_mut().sample = Arc::clone(sample);
        self.call(self.tick)
    }

    /// Runs one export with fresh fuel, then writes what it set.
    fn call(&mut self, func: TypedFunc<(), ()>) -> Result<(), String> {
        self.store
            .set_fuel(FUEL_PER_CALL)
            .map_err(|e| e.to_string())?;
        let result = func.call(&mut self.store, ()).map_err(|e| e.to_string());
        let host = self.store.data_mut();
        let pending = std::mem::take(&mut host.pending);
        if let Some(error) = host.error.take() {
            return Err(error);
        }
        result?;
        let changed: Vec<_> = pending
            .into_iter()
            .filter(|(cell, input)| self.last.get(cell) != Some(input))
            .collect();
        if changed.is_empty() {
            return Ok(());
        }
        let written = self.sheet.set_cells(&self.origin, &changed)?;
        self.writes += written as u64;
        self.last.extend(changed);
        Ok(())
    }

    fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.store.data_mut().log)
    }
}

/// Every import a widget may use.
fn linker(engine: &Engine) -> Result<Linker<Host>, wasmi::errors::LinkerError> {
    let mut linker = Linker::<Host>::new(engine);
    for &(name, _, read) in METRICS {
        linker.func_wrap("tools", name, move |caller: Caller<'_, Host>| -> f64 {
            read(&caller.data().sample.metrics)
        })?;
    }
    linker.func_wrap(
        "tools",
        "core_percent",
        |caller: Caller<'_, Host>, core: i32| -> f64 {
            let cores = &caller.data().sample.metrics.cores;
            usize::try_from(core)
                .ok()
                .and_then(|i| cores.get(i))
                .map_or(0.0, |c| f64::from(c.percent))
        },
    )?;
    linker.func_wrap(
        "sheet",
        "set_number",
        |mut caller: Caller<'_, Host>, row: i32, col: i32, value: f64| {
            let input = if value.is_finite() {
                format!("{}", (value * 100.0).round() / 100.0)
            } else {
                String::new()
            };
            set(&mut caller, row, col, input);
        },
    )?;
    linker.func_wrap(
        "sheet",
        "set_text",
        |mut caller: Caller<'_, Host>, row: i32, col: i32, ptr: i32, len: i32| {
            if let Some(text) = read_str(&mut caller, ptr, len) {
                set(&mut caller, row, col, text);
            }
        },
    )?;
    linker.func_wrap(
        "env",
        "log",
        |mut caller: Caller<'_, Host>, ptr: i32, len: i32| {
            if let Some(text) = read_str(&mut caller, ptr, len) {
                caller.data_mut().log.push(text);
            }
        },
    )?;
    Ok(linker)
}

fn set(caller: &mut Caller<'_, Host>, row: i32, col: i32, input: String) {
    let host = caller.data_mut();
    let Some(cell) = a1(row, col) else {
        host.error.get_or_insert(format!(
            "cell ({row}, {col}) is out of range; rows and columns start at 1"
        ));
        return;
    };
    if host.pending.len() >= MAX_WRITES_PER_TICK {
        host.error.get_or_insert(format!(
            "more than {MAX_WRITES_PER_TICK} writes in one call"
        ));
        return;
    }
    host.pending.push((cell, input));
}

fn read_str(caller: &mut Caller<'_, Host>, ptr: i32, len: i32) -> Option<String> {
    let range = usize::try_from(ptr)
        .ok()
        .zip(usize::try_from(len).ok())
        .map(|(p, l)| p..p.saturating_add(l));
    let memory = caller.get_export("memory").and_then(Extern::into_memory);
    let text = match (memory, range) {
        (Some(memory), Some(range)) => memory
            .data(&*caller)
            .get(range)
            .map(|b| String::from_utf8_lossy(b).into_owned()),
        _ => None,
    };
    if text.is_none() {
        caller
            .data_mut()
            .error
            .get_or_insert("string out of bounds, or no exported \"memory\"".into());
    }
    text
}

/// `A1` for 1-based row and column (`(1, 1)` → `A1`, `(3, 28)` → `AB3`).
pub fn a1(row: i32, col: i32) -> Option<String> {
    if !(1..=1_048_576).contains(&row) || !(1..=16_384).contains(&col) {
        return None;
    }
    let mut n = u32::try_from(col).ok()?;
    let mut letters = Vec::new();
    while n > 0 {
        let rem = (n - 1) % 26;
        letters.push(char::from(b'A' + u8::try_from(rem).ok()?));
        n = (n - 1) / 26;
    }
    letters.reverse();
    Some(format!("{}{row}", letters.into_iter().collect::<String>()))
}

/// Adds the list of valid imports to an unknown-import error.
fn link_hint(error: &str) -> String {
    if error.contains("import") || error.contains("unknown") {
        let names: Vec<_> = METRICS.iter().map(|m| m.0).collect();
        format!(
            "{error}. Valid imports: tools.{{{}}}, tools.core_percent, sheet.set_number, sheet.set_text, env.log",
            names.join(",")
        )
    } else {
        error.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use futures_util::StreamExt as _;

    use super::*;

    const WIDGET: &str = r#"(module
        (import "tools" "cpu_percent" (func $cpu (result f64)))
        (import "sheet" "set_number" (func $num (param i32 i32 f64)))
        (import "sheet" "set_text" (func $text (param i32 i32 i32 i32)))
        (memory (export "memory") 1)
        (data (i32.const 0) "CPU %")
        (func (export "init") (call $text (i32.const 1) (i32.const 1) (i32.const 0) (i32.const 5)))
        (func (export "tick") (call $num (i32.const 1) (i32.const 2) (call $cpu))))"#;

    fn sample(cpu: f32) -> Sample {
        Sample {
            metrics: pb::MachineMetrics {
                cpu_percent: cpu,
                ..Default::default()
            },
            top: vec![],
        }
    }

    #[test]
    fn a1_names() {
        assert_eq!(a1(1, 1).as_deref(), Some("A1"));
        assert_eq!(a1(3, 28).as_deref(), Some("AB3"));
        assert_eq!(a1(0, 1), None);
    }

    #[tokio::test]
    async fn a_widget_writes_metrics_into_the_sheet() {
        let sheet = Sheet::new();
        let widgets = Widgets::new(Machine::fixed(sample(42.123)), Arc::clone(&sheet));
        let mut log = Arc::clone(&sheet).follow(0, "");
        widgets.compile("cpu", "", WIDGET, 250).unwrap();
        let init = log.next().await.unwrap();
        assert!(
            init.ops.iter().any(|op| op.params_json.contains("CPU %")),
            "{init:?}"
        );
        let tick = log.next().await.unwrap();
        assert!(
            tick.ops
                .iter()
                .any(|op| op.params_json.contains("42.12") && op.origin == "widget:cpu"),
            "{tick:?}"
        );
        assert!(widgets.stop("cpu", false));
        assert_eq!(
            widgets.list()[0].state,
            pb::WidgetState::WIDGET_STATE_STOPPED
        );
        let restarted = widgets.restart("cpu").unwrap();
        assert_eq!(restarted.state, pb::WidgetState::WIDGET_STATE_RUNNING);
        assert!(widgets.restart("nope").is_err());
        assert!(widgets.stop("cpu", true));
        assert_eq!(
            widgets.list(),
            [] as [agent_demo_proto::agentdemo::v1::Widget; 0]
        );
    }

    #[tokio::test]
    async fn bad_widgets_are_explained() {
        let widgets = Widgets::new(Machine::fixed(sample(0.0)), Sheet::new());
        let compile = widgets.compile("x", "", "(module (func", 1000).unwrap_err();
        assert!(matches!(compile, WidgetError::Compile(_)), "{compile}");
        let import = widgets
            .compile(
                "x",
                "",
                r#"(module (import "tools" "nope" (func)) (func (export "tick")))"#,
                1000,
            )
            .unwrap_err();
        assert!(import.to_string().contains("Valid imports"), "{import}");
        let no_tick = widgets.compile("x", "", "(module)", 1000).unwrap_err();
        assert!(no_tick.to_string().contains("tick"), "{no_tick}");
        let looping = widgets
            .compile(
                "x",
                "",
                r#"(module (func (export "init") (loop (br 0))) (func (export "tick")))"#,
                1000,
            )
            .unwrap_err();
        assert!(matches!(looping, WidgetError::Init(_)), "{looping}");
    }
}
