// WebAssembly widgets: what the agent (or you) compiled, live state, source and log, controls,
// and a box to compile your own.
import { useState } from "react";
import type { ReactNode } from "react";

import { Badge, Button, Disclosure, Panel, type Tone } from "../design/index.js";
import { machineClient } from "../api/client.js";
import { WidgetState, type Widget } from "../gen/agentdemo/v1/agent_pb.js";
import { useWidgets } from "./live.js";

const STATE: Record<WidgetState, { label: string; tone: Tone }> = {
  [WidgetState.UNSPECIFIED]: { label: "—", tone: "muted" },
  [WidgetState.RUNNING]: { label: "Running", tone: "success" },
  [WidgetState.STOPPED]: { label: "Stopped", tone: "muted" },
  [WidgetState.FAILED]: { label: "Failed", tone: "critical" },
};

export const EXAMPLE_WAT = `(module
  (import "tools" "cpu_percent" (func $cpu (result f64)))
  (import "tools" "memory_percent" (func $mem (result f64)))
  (import "sheet" "set_number" (func $num (param i32 i32 f64)))
  (import "sheet" "set_text" (func $text (param i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "CPU %Memory %")
  (func (export "init")
    (call $text (i32.const 1) (i32.const 14) (i32.const 0) (i32.const 5))
    (call $text (i32.const 2) (i32.const 14) (i32.const 5) (i32.const 8)))
  (func (export "tick")
    (call $num (i32.const 1) (i32.const 15) (call $cpu))
    (call $num (i32.const 2) (i32.const 15) (call $mem))))`;

function WidgetCard({ widget }: { widget: Widget }) {
  const state = STATE[widget.state];
  const running = widget.state === WidgetState.RUNNING;
  const [error, setError] = useState("");
  const report = (err: unknown) => setError(err instanceof Error ? err.message : String(err));
  const stop = (remove: boolean) => void machineClient.stopWidget({ name: widget.name, remove }).catch(report);
  const restart = () => {
    setError("");
    machineClient.restartWidget({ name: widget.name }).catch(report);
  };
  return (
    <li className="sd-frame widget" data-tone={state.tone}>
      <header className="widget__header">
        <code className="widget__name">{widget.name}</code>
        <Badge tone={state.tone}>{state.label}</Badge>
      </header>
      {widget.description && <p className="widget__desc">{widget.description}</p>}
      <p className="muted widget__stats">
        every {widget.intervalMs} ms · {String(widget.ticks)} ticks · {String(widget.writes)} writes · {(widget.wasmBytes / 1024).toFixed(1)} KiB wasm
      </p>
      {widget.error && <p className="error">{widget.error}</p>}
      <Disclosure title="Source (WAT)"><pre className="tool__json">{widget.wat}</pre></Disclosure>
      {widget.log.length > 0 && (
        <Disclosure title="Log" aside={widget.log.length}><pre className="tool__json">{widget.log.join("\n")}</pre></Disclosure>
      )}
      <div className="widget__actions">
        {running ? (
          <Button size="sm" onClick={() => stop(false)}>Stop</Button>
        ) : (
          <Button size="sm" variant="primary" onClick={restart}>Restart</Button>
        )}
        <Button size="sm" variant="ghost" onClick={() => stop(true)}>Remove</Button>
      </div>
      {error && <p className="error">{error}</p>}
    </li>
  );
}

function Compiler() {
  const [name, setName] = useState("my-widget");
  const [wat, setWat] = useState(EXAMPLE_WAT);
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);
  const compile = () => {
    setMessage(null);
    machineClient
      .compileWidget({ name, wat, intervalMs: 1000, description: "compiled from the browser" })
      .then((res) => setMessage({ ok: true, text: `${res.widget?.name ?? name} is running` }))
      .catch((err: unknown) => setMessage({ ok: false, text: err instanceof Error ? err.message : String(err) }));
  };
  return (
    <Disclosure title="Compile your own">
      <div className="compiler">
        <input className="sd-input" aria-label="Widget name" value={name} onChange={(e) => setName(e.target.value)} />
        <textarea className="sd-input compiler__wat" rows={12} spellCheck={false} value={wat} onChange={(e) => setWat(e.target.value)} />
        <Button size="sm" variant="primary" onClick={compile}>Compile &amp; run</Button>
        {message && <p className={message.ok ? "muted" : "error"}>{message.text}</p>}
      </div>
    </Disclosure>
  );
}

export function WidgetsPanel({ hidden, minimize }: { hidden?: boolean; minimize?: ReactNode }) {
  const widgets = useWidgets();
  const running = widgets.filter((w) => w.state === WidgetState.RUNNING).length;
  return (
    <Panel
      title="Wasm widgets"
      hidden={hidden}
      aside={
        <span className="panel-aside">
          {running} running
          {minimize}
        </span>
      }
    >
      {widgets.length === 0 && (
        <p className="muted">None yet. Ask the agent for a live sheet, e.g. “keep B2 at the current CPU load”.</p>
      )}
      <ul className="widgets">
        {widgets.map((w) => <WidgetCard key={w.name} widget={w} />)}
      </ul>
      <Compiler />
    </Panel>
  );
}
