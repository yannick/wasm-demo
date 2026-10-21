// The agent console. Two views: the workspace (chat, shared sheet, machine and widgets) and the
// run browser (new runs, the run list, a run's live transcript, plan, usage and controls). The
// URL hash picks the view and the run (`#run-3`), so a reload or a second tab replays it from the
// server's log. The workspace stays mounted while hidden, so the sheet keeps its state.
// Opened with `?window=sheet` it is the undocked sheet window instead.
import { useEffect, useState } from "react";

import { Panel, Tabs } from "../design/index.js";
import { Inspector } from "../agent/Inspector.js";
import { Sidebar } from "../agent/Sidebar.js";
import { Transcript } from "../agent/Transcript.js";
import { useRun } from "../agent/live.js";
import { SheetWindow } from "../workspace/SheetWindow.js";
import { Workspace } from "../workspace/Workspace.js";
import { isSheetWindow } from "../workspace/undock.js";

type View = "workspace" | "runs";

function readHash(): { view: View; runId: string | null } {
  const hash = decodeURIComponent(window.location.hash.slice(1));
  if (hash.startsWith("run-")) return { view: "runs", runId: hash };
  return { view: hash === "runs" ? "runs" : "workspace", runId: null };
}

function useHash(): [{ view: View; runId: string | null }, (hash: string) => void] {
  const [state, setState] = useState(readHash);
  useEffect(() => {
    const onHash = () => setState(readHash());
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);
  return [state, (hash) => (window.location.hash = encodeURIComponent(hash))];
}

function Runs({ runId, select }: { runId: string | null; select(runId: string): void }) {
  const { view, connected } = useRun(runId);
  return (
    <div className="console">
      <Sidebar selected={runId} onSelect={select} />
      <section className="stage">
        {runId === null ? (
          <Panel title="Runs">
            <p className="muted">Pick a scenario and run the agent. Every step streams in live over a Connect server stream.</p>
          </Panel>
        ) : (
          <Panel title={view.summary?.prompt || "…"}>
            <Transcript view={view} />
          </Panel>
        )}
      </section>
      {runId !== null && <Inspector view={view} connected={connected} />}
    </div>
  );
}

export default function AgentConsole() {
  return isSheetWindow() ? <SheetWindow /> : <Console />;
}

function Console() {
  const [{ view, runId }, go] = useHash();
  return (
    <div className="app">
      <Tabs
        label="View"
        tabs={[
          { id: "workspace", label: "Workspace" },
          { id: "runs", label: "Runs" },
        ]}
        value={view}
        onChange={(v) => go(v)}
      />
      <Workspace hidden={view !== "workspace"} />
      {view === "runs" && <Runs runId={runId} select={go} />}
    </div>
  );
}
