// Live readings of the server's machine, and a button that writes a snapshot into the sheet
// from the browser (through the sheet frame, so it syncs like a user's edit).
import { useState } from "react";
import type { ReactNode } from "react";

import { Badge, Button, DataList, Meter, Panel, Readout, type Tone } from "../design/index.js";
import type { MachineMetrics } from "../gen/agentdemo/v1/agent_pb.js";
import type { SheetCaller } from "../sheet/useSheet.js";
import { useMetrics } from "./live.js";

const GIB = 1024 ** 3;

function pct(used: bigint, total: bigint): number {
  return total === 0n ? 0 : Number((used * 10_000n) / total) / 100;
}

function tone(percent: number): Exclude<Tone, "muted" | "info"> {
  if (percent >= 90) return "critical";
  return percent >= 70 ? "warning" : "accent";
}

function gib(bytes: bigint): string {
  return (Number(bytes) / GIB).toFixed(1);
}

function rate(bytesPerS: bigint): string {
  const kib = Number(bytesPerS) / 1024;
  return kib >= 1024 ? `${(kib / 1024).toFixed(1)} MiB/s` : `${kib.toFixed(0)} KiB/s`;
}

function uptime(s: bigint): string {
  const h = Number(s) / 3600;
  return h >= 48 ? `${(h / 24).toFixed(1)} d` : `${h.toFixed(1)} h`;
}

/** The snapshot as rows for `range.setValues`. */
export function snapshotRows(m: MachineMetrics): Array<Array<string | number>> {
  return [
    ["Snapshot", new Date(Number(m.atMs)).toLocaleTimeString()],
    ["CPU %", Number(m.cpuPercent.toFixed(1))],
    ["Memory %", pct(m.memoryUsedBytes, m.memoryTotalBytes)],
    ["Memory used GiB", Number(gib(m.memoryUsedBytes))],
    ["Load (1 min)", Number(m.load1.toFixed(2))],
    ["Disk %", pct(m.diskUsedBytes, m.diskTotalBytes)],
    ["Processes", m.processCount],
  ];
}

function Snapshot({ metrics, sheet }: { metrics: MachineMetrics; sheet: SheetCaller }) {
  const [anchor, setAnchor] = useState("K1");
  const [result, setResult] = useState("");
  const write = () => {
    setResult("");
    sheet
      .call("engine.execute", { command: "range.setValues", params: { range: anchor.trim() || "K1", values: snapshotRows(metrics) } })
      .then((r) => setResult(r.ok ? `written at ${anchor}` : `failed: ${JSON.stringify(r.error)}`))
      .catch((err: unknown) => setResult(String(err)));
  };
  return (
    <div className="snapshot">
      <input className="sd-input snapshot__anchor" aria-label="Snapshot cell" value={anchor} onChange={(e) => setAnchor(e.target.value)} />
      <Button size="sm" disabled={!sheet.ready} onClick={write}>Snapshot → sheet</Button>
      {result && <span className="muted">{result}</span>}
    </div>
  );
}

function Cores({ metrics }: { metrics: MachineMetrics }) {
  return (
    <div className="cores" aria-label="Per-core load">
      {metrics.cores.map((c, i) => (
        <Meter key={i} label={`Core ${i}`} value={c.percent / 100} tone={tone(c.percent)} />
      ))}
    </div>
  );
}

export function MachinePanel({ sheet, hidden, minimize }: { sheet: SheetCaller; hidden?: boolean; minimize?: ReactNode }) {
  const { metrics, connected } = useMetrics();
  if (!metrics) {
    return <Panel title="Machine" hidden={hidden} aside={minimize}><p className="muted">Waiting for the first sample…</p></Panel>;
  }
  const mem = pct(metrics.memoryUsedBytes, metrics.memoryTotalBytes);
  const disk = pct(metrics.diskUsedBytes, metrics.diskTotalBytes);
  return (
    <Panel
      title="Machine"
      hidden={hidden}
      aside={
        <span className="panel-aside">
          {connected ? <Badge tone="success">1 s</Badge> : <Badge tone="warning">offline</Badge>}
          {minimize}
        </span>
      }
    >
      <p className="muted machine__host">{metrics.host} · {metrics.os}</p>
      <div className="machine__readouts">
        <Readout size="sm" label="CPU" value={metrics.cpuPercent.toFixed(0)} unit="%" />
        <Readout size="sm" label="Memory" value={mem.toFixed(0)} unit="%" />
        <Readout size="sm" label="Load" value={metrics.load1.toFixed(2)} />
      </div>
      <Meter label="CPU" value={metrics.cpuPercent / 100} tone={tone(metrics.cpuPercent)} />
      <Cores metrics={metrics} />
      <DataList
        inline
        items={[
          { term: "Memory", value: `${gib(metrics.memoryUsedBytes)} / ${gib(metrics.memoryTotalBytes)} GiB` },
          { term: "Swap", value: `${gib(metrics.swapUsedBytes)} GiB` },
          { term: "Disk", value: `${disk.toFixed(0)} % of ${gib(metrics.diskTotalBytes)} GiB` },
          { term: "Network", value: `↓ ${rate(metrics.netRxBytesPerS)} ↑ ${rate(metrics.netTxBytesPerS)}` },
          { term: "Processes", value: String(metrics.processCount) },
          { term: "Uptime", value: uptime(metrics.uptimeS) },
        ]}
      />
      <Snapshot metrics={metrics} sheet={sheet} />
    </Panel>
  );
}
