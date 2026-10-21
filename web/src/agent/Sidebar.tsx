// New run (scenario + prompt) and the list of runs, live for every client.
import { useEffect, useState } from "react";

import { Button, Panel } from "../design/index.js";
import { agentClient } from "../api/client.js";
import type { Scenario } from "../gen/agentdemo/v1/agent_pb.js";
import { statusBadge } from "./Inspector.js";
import { useRuns } from "./live.js";

function useScenarios(): Scenario[] {
  const [scenarios, setScenarios] = useState<Scenario[]>([]);
  useEffect(() => {
    agentClient
      .listScenarios({})
      .then((res) => setScenarios(res.scenarios))
      .catch((err: unknown) => console.warn("ListScenarios failed", err));
  }, []);
  return scenarios;
}

function NewRun({ onStarted }: { onStarted(runId: string): void }) {
  const scenarios = useScenarios();
  const [picked, setPicked] = useState<Scenario | null>(null);
  // null: the picked scenario's own prompt.
  const [prompt, setPrompt] = useState<string | null>(null);
  const [error, setError] = useState("");
  const scenario = picked ?? scenarios[0] ?? null;
  const pick = (s: Scenario) => {
    setPicked(s);
    setPrompt(null);
  };
  const start = () => {
    if (!scenario) return;
    setError("");
    agentClient
      .startRun({ scenarioId: scenario.id, prompt: prompt ?? scenario.prompt })
      .then((res) => onStarted(res.runId))
      .catch((err: unknown) => setError(String(err)));
  };
  return (
    <Panel title="New run" accent>
      <div className="scenarios" role="radiogroup" aria-label="Scenario">
        {scenarios.map((s) => (
          <button
            key={s.id}
            type="button"
            role="radio"
            aria-checked={s.id === scenario?.id}
            className="sd-frame scenario"
            onClick={() => pick(s)}
          >
            <span className="scenario__title">{s.title}</span>
            <span className="scenario__desc">{s.description}</span>
          </button>
        ))}
      </div>
      <textarea
        className="sd-input composer__prompt"
        rows={3}
        value={prompt ?? scenario?.prompt ?? ""}
        onChange={(e) => setPrompt(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) start();
        }}
      />
      <Button variant="primary" hint="⌘⏎" disabled={!scenario} onClick={start}>Run agent</Button>
      {error && <p className="error">{error}</p>}
    </Panel>
  );
}

export function Sidebar({ selected, onSelect }: { selected: string | null; onSelect(runId: string): void }) {
  const { runs, connected } = useRuns();
  return (
    <div className="sidebar">
      <NewRun onStarted={onSelect} />
      <Panel title="Runs" aside={connected ? `${runs.length}` : "offline"}>
        {runs.length === 0 && <p className="muted">No runs yet.</p>}
        <ul className="runs">
          {runs.map((r) => (
            <li key={r.runId}>
              <button
                type="button"
                className="run-row"
                aria-current={r.runId === selected ? "true" : undefined}
                onClick={() => onSelect(r.runId)}
              >
                <span className="run-row__prompt">{r.prompt || r.scenarioId}</span>
                <span className="run-row__meta">{r.runId} {statusBadge(r.status)}</span>
              </button>
            </li>
          ))}
        </ul>
      </Panel>
    </div>
  );
}
