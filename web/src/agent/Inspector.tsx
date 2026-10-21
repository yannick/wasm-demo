// The side panel of a run: status, the agent's live plan, token usage and controls.
import { Badge, Button, DataList, Panel, Readout, type Tone } from "../design/index.js";
import { agentClient } from "../api/client.js";
import { RunStatus, StepStatus, type PlanStep } from "../gen/agentdemo/v1/agent_pb.js";
import { isEnded, pendingApprovals, type RunView } from "./state.js";

const STATUS: Record<RunStatus, { label: string; tone: Tone }> = {
  [RunStatus.UNSPECIFIED]: { label: "Connecting", tone: "muted" },
  [RunStatus.RUNNING]: { label: "Running", tone: "info" },
  [RunStatus.AWAITING_APPROVAL]: { label: "Needs you", tone: "warning" },
  [RunStatus.SUCCEEDED]: { label: "Succeeded", tone: "success" },
  [RunStatus.FAILED]: { label: "Failed", tone: "critical" },
  [RunStatus.CANCELLED]: { label: "Cancelled", tone: "muted" },
};

export function statusBadge(status: RunStatus) {
  const s = STATUS[status];
  return <Badge tone={s.tone}>{s.label}</Badge>;
}

const STEP_MARK: Record<StepStatus, string> = {
  [StepStatus.UNSPECIFIED]: "·",
  [StepStatus.PENDING]: "○",
  [StepStatus.IN_PROGRESS]: "◉",
  [StepStatus.DONE]: "✓",
  [StepStatus.SKIPPED]: "–",
};

export function Plan({ steps }: { steps: PlanStep[] }) {
  if (steps.length === 0) return <p className="muted">No plan yet.</p>;
  return (
    <ol className="plan">
      {steps.map((s) => (
        <li key={s.id} className="plan__step" data-status={StepStatus[s.status].toLowerCase()}>
          <span className="plan__mark" aria-hidden="true">{STEP_MARK[s.status]}</span>
          {s.title}
        </li>
      ))}
    </ol>
  );
}

function streamState(connected: boolean, ended: boolean): string {
  if (connected) return "live";
  return ended ? "closed" : "reconnecting…";
}

function facts(view: RunView, connected: boolean): Array<{ term: string; value: string }> {
  const waiting = pendingApprovals(view).length;
  const items = [
    { term: "Id", value: view.summary?.runId || "—" },
    { term: "Events", value: String(view.events) },
    { term: "Stream", value: streamState(connected, isEnded(view.status)) },
  ];
  if (waiting > 0) items.push({ term: "Approvals", value: `${waiting} waiting` });
  if (view.error) items.push({ term: "Error", value: view.error });
  return items;
}

export function Inspector({ view, connected }: { view: RunView; connected: boolean }) {
  const runId = view.summary?.runId ?? "";
  const ended = isEnded(view.status);
  return (
    <aside className="inspector">
      <Panel title="Run" aside={statusBadge(view.status)}>
        <DataList inline items={facts(view, connected)} />
        {!ended && runId !== "" && (
          <Button size="sm" variant="ghost" onClick={() => void agentClient.cancelRun({ runId })}>Cancel run</Button>
        )}
      </Panel>
      <Panel title="Plan"><Plan steps={view.plan} /></Panel>
      <Panel title="Tokens">
        <div className="usage">
          <Readout size="sm" label="Input" value={view.usage?.inputTokens ?? 0} />
          <Readout size="sm" label="Output" value={view.usage?.outputTokens ?? 0} />
        </div>
      </Panel>
    </aside>
  );
}
