//! The `postMessage` bridge to the agent-demo page that frames this sheet (same origin only).
//!
//! Every message is `{ type, json }`, where `json` is a JSON string, so nothing depends on how
//! JS objects map to Rust:
//!
//! - `sheet:ready` (to the page): the sheet listens; `json` is `{ version }`.
//! - `sheet:apply` (from the page): `{ ops: [{ command, params }] }`, engine commands from the
//!   server's sheet log (the agent, widgets, other tabs). They run as given and are not reported
//!   back as local changes.
//! - `sheet:call` (from the page): `{ id, method, params }`, any control-channel method
//!   (`sheet.read`, `engine.execute`, `document.inspect`, …; see gridcraft's
//!   `docs/control-protocol.md`). Answered with `sheet:reply` `{ id, response }`, where
//!   `response` is `{ ok, result }` or `{ ok: false, error }`. Commands it runs count as local
//!   changes, like the user's own.
//! - `sheet:changed` (to the page): `{ ops: [{ command, params }] }`, the commands the user (or a
//!   `sheet:call`) ran since the last report, taken from the engine's replayable journal.
//! - `sheet:failed` (to the page): `{ error }` when the sheet couldn't start, or
//!   `{ error, crashed: true, op }` when it panicked (it stops after a panic). `op` is
//!   `{ seq, command }` of the remote op that was running, or null when the panic came later
//!   (e.g. while drawing).
//! - `sheet:ping` (from the page) `{ id }`: answered with `sheet:pong` `{ id }` at the next frame,
//!   so the page can tell a sheet that stopped drawing from a busy one.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use gridcraft_ui_egui::SheetApp;
use gridcraft_ui_egui::control::{Outcome, handle};
use serde_json::{Value as Json, json};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;

thread_local! {
    /// The bridge, for the panic hook (a hook must be `Send + Sync`, so it can't hold it).
    static HOOKED: RefCell<Option<Bridge>> = const { RefCell::new(None) };
    /// The remote op running right now (`seq`, command), so a panic can name it.
    static APPLYING: RefCell<Option<(Json, String)>> = const { RefCell::new(None) };
}

#[derive(Clone)]
pub struct Bridge {
    window: web_sys::Window,
    parent: web_sys::Window,
    origin: String,
    /// Messages from the page, handled at the start of the next frame (they need the app).
    queue: Rc<RefCell<VecDeque<Json>>>,
    /// Journal entries up to here were reported or came from the page.
    reported: Rc<Cell<usize>>,
}

impl Bridge {
    /// `None` when the page isn't framed (there is no page to talk to).
    pub fn new() -> Option<Self> {
        let window = web_sys::window()?;
        let parent = window.parent().ok().flatten()?;
        if js_sys::Object::is(&parent, &window) {
            return None;
        }
        let origin = window.location().origin().ok()?;
        Some(Self { window, parent, origin, queue: Rc::default(), reported: Rc::default() })
    }

    pub fn post_ready(&self, app: &SheetApp) {
        self.reported.set(app.session.journal.len());
        self.send("sheet:ready", &json!({ "version": env!("CARGO_PKG_VERSION") }));
    }

    pub fn post_failed(&self, error: &str) {
        self.send("sheet:failed", &json!({ "error": error }));
    }

    /// Reports a panic to the page as `sheet:failed`. Call after the eframe runner installed its
    /// own hook: that one runs first and stops the app.
    pub fn report_panics(&self) {
        HOOKED.with(|b| *b.borrow_mut() = Some(self.clone()));
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            previous(info);
            let op = APPLYING.with(|a| a.try_borrow().ok().and_then(|a| a.clone()));
            let body = json!({
                "error": format!("the sheet crashed: {info}"),
                "crashed": true,
                "op": op.map(|(seq, command)| json!({ "seq": seq, "command": command })),
            });
            HOOKED.with(|b| {
                if let Ok(b) = b.try_borrow()
                    && let Some(bridge) = b.as_ref()
                {
                    bridge.send("sheet:failed", &body);
                }
            });
        }));
    }

    /// Queues the page's messages and wakes egui to handle them.
    pub fn listen(&self, ctx: egui::Context) {
        let parent = self.parent.clone();
        let origin = self.origin.clone();
        let queue = Rc::clone(&self.queue);
        let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            if event.origin() != origin || !event.source().is_some_and(|s| js_sys::Object::is(&s, &parent)) {
                return;
            }
            let data = event.data();
            let (Some(kind), Some(body)) = (string(&data, "type"), string(&data, "json")) else { return };
            if !kind.starts_with("sheet:") {
                return;
            }
            match serde_json::from_str::<Json>(&body) {
                Ok(body) => {
                    queue.borrow_mut().push_back(json!({ "type": kind, "body": body }));
                    ctx.request_repaint();
                }
                Err(e) => log::warn!("ignoring {kind}: bad json: {e}"),
            }
        });
        if self.window.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref()).is_ok() {
            // The listener lives as long as the page.
            on_message.forget();
        }
    }

    /// Handles the queued messages (before the frame).
    pub fn process(&self, app: &mut SheetApp, ctx: &egui::Context) {
        let messages: Vec<Json> = self.queue.borrow_mut().drain(..).collect();
        for message in messages {
            let body = &message["body"];
            match message["type"].as_str() {
                Some("sheet:apply") => self.apply(app, ctx, body),
                Some("sheet:call") => self.call(app, ctx, body),
                Some("sheet:ping") => {
                    self.send("sheet:pong", &json!({ "id": body["id"] }));
                }
                other => log::warn!("unknown message {other:?}"),
            }
        }
    }

    /// Remote commands: local changes made before them are reported first, then the remote ones
    /// run and are marked as seen, so they never echo back to the server.
    fn apply(&self, app: &mut SheetApp, ctx: &egui::Context, body: &Json) {
        self.report_local(app);
        for op in body["ops"].as_array().map(Vec::as_slice).unwrap_or_default() {
            let Some(command) = op["command"].as_str() else { continue };
            let params = json!({ "command": command, "params": op.get("params").cloned().unwrap_or(json!({})) });
            APPLYING.with(|a| *a.borrow_mut() = Some((op["seq"].clone(), command.to_owned())));
            let outcome = handle(app, ctx, "engine.execute", &params);
            APPLYING.with(|a| *a.borrow_mut() = None);
            if let Outcome::Done(res) = outcome
                && res["ok"] == json!(false)
            {
                log::warn!("remote {command} failed: {}", res["error"]);
            }
        }
        self.reported.set(app.session.journal.len());
    }

    fn call(&self, app: &mut SheetApp, ctx: &egui::Context, body: &Json) {
        let method = body["method"].as_str().unwrap_or_default();
        let params = body.get("params").cloned().unwrap_or(json!({}));
        let response = match handle(app, ctx, method, &params) {
            Outcome::Done(res) => res,
            Outcome::Screenshot { .. } => json!({ "ok": false, "error": "screenshots are not supported in the browser" }),
        };
        self.send("sheet:reply", &json!({ "id": body["id"], "response": response }));
    }

    /// Reports journal entries the page hasn't seen (after each frame).
    pub fn report_local(&self, app: &SheetApp) {
        let journal = &app.session.journal;
        // The engine trims a long journal from the front; what was cut can't be reported.
        let from = self.reported.get().min(journal.len());
        if from == journal.len() {
            self.reported.set(journal.len());
            return;
        }
        let ops: Vec<Json> = journal[from..].iter().map(|(command, params)| json!({ "command": command, "params": params })).collect();
        if self.send("sheet:changed", &json!({ "ops": ops })) {
            self.reported.set(journal.len());
        }
    }

    /// Posts `{ type, json }` to the page; false if it couldn't be sent.
    fn send(&self, kind: &str, body: &Json) -> bool {
        let m = js_sys::Object::new();
        let set = |k: &str, v: JsValue| js_sys::Reflect::set(&m, &JsValue::from_str(k), &v).is_ok();
        set("type", kind.into()) && set("json", body.to_string().into()) && self.parent.post_message(&m, &self.origin).is_ok()
    }
}

fn string(data: &JsValue, key: &str) -> Option<String> {
    js_sys::Reflect::get(data, &JsValue::from_str(key)).ok()?.as_string()
}
