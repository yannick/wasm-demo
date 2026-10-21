// The run's transcript: streamed text and thinking blocks, tool call cards (with approval
// controls) and sub-agent cards that nest the sub-agent's own transcript.
import { useState } from "react";

import { Badge, Button, Disclosure, cx, type Tone } from "../design/index.js";
import { agentClient } from "../api/client.js";
import { Markdown } from "./markdown.js";
import { itemsOf, type BlockItem, type Item, type RunView, type SubagentItem, type ToolItem } from "./state.js";

export function Transcript({ view, agentId = "" }: { view: RunView; agentId?: string }) {
  const items = itemsOf(view, agentId);
  return (
    <ol className="transcript">
      {items.map((item) => <li key={item.id} className="transcript__item"><ItemView item={item} view={view} /></li>)}
    </ol>
  );
}

function ItemView({ item, view }: { item: Item; view: RunView }) {
  if (item.type === "block") return <Block block={item} />;
  if (item.type === "tool") return <ToolCall call={item} runId={view.summary?.runId ?? ""} />;
  return <Subagent sub={item} view={view} />;
}

function Block({ block }: { block: BlockItem }) {
  if (block.thinking) {
    return (
      <div className={cx("block block--thinking", !block.done && "is-streaming")}>
        <span className="sd-label">Thinking</span>
        <p>{block.text}</p>
      </div>
    );
  }
  return (
    <div className={cx("block block--text", !block.done && "is-streaming")}>
      <Markdown text={block.text} />
    </div>
  );
}

function toolState(call: ToolItem): { label: string; tone: Tone } {
  const decision = call.approval?.decision;
  if (call.approval && !decision) return { label: "Awaiting approval", tone: "warning" };
  if (decision && !decision.approved) return { label: "Rejected", tone: "critical" };
  if (call.output === undefined) return { label: "Running", tone: "info" };
  return call.isError ? { label: "Error", tone: "critical" } : { label: "Done", tone: "success" };
}

function ToolCall({ call, runId }: { call: ToolItem; runId: string }) {
  const state = toolState(call);
  const waiting = call.approval !== undefined && call.approval.decision === undefined;
  return (
    <div className={cx("sd-frame tool", waiting && "tool--waiting")} data-tone={state.tone}>
      <header className="tool__header">
        <code className="tool__name">{call.tool}</code>
        {call.durationMs !== undefined && <span className="tool__time">{call.durationMs} ms</span>}
        <Badge tone={state.tone}>{state.label}</Badge>
      </header>
      {call.approval && <p className="tool__rationale">{call.approval.rationale}</p>}
      <Disclosure title="Input" open={waiting}><pre className="tool__json">{pretty(call.input)}</pre></Disclosure>
      {call.output !== undefined && (
        <Disclosure title="Output" open={call.isError}><pre className="tool__json">{call.output}</pre></Disclosure>
      )}
      {waiting && <ApprovalControls runId={runId} callId={call.id} />}
    </div>
  );
}

function ApprovalControls({ runId, callId }: { runId: string; callId: string }) {
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const decide = (approved: boolean) => {
    setBusy(true);
    agentClient
      .decideToolCall({ runId, callId, approved, reason })
      .catch((err: unknown) => console.warn("DecideToolCall failed", err))
      .finally(() => setBusy(false));
  };
  return (
    <div className="approval">
      <input
        className="sd-input approval__reason"
        placeholder="Reason (optional, sent with a rejection)"
        value={reason}
        onChange={(e) => setReason(e.target.value)}
      />
      <Button variant="primary" disabled={busy} onClick={() => decide(true)}>Approve</Button>
      <Button disabled={busy} onClick={() => decide(false)}>Reject</Button>
    </div>
  );
}

function Subagent({ sub, view }: { sub: SubagentItem; view: RunView }) {
  return (
    <div className={cx("sd-frame subagent", !sub.done && "is-streaming")}>
      <header className="subagent__header">
        <span className="subagent__name">{sub.name}</span>
        <Badge tone={sub.done ? "success" : "info"}>{sub.done ? "Done" : "Working"}</Badge>
      </header>
      <p className="subagent__task">{sub.task}</p>
      <Transcript view={view} agentId={sub.id} />
    </div>
  );
}

function pretty(json: string): string {
  try {
    return JSON.stringify(JSON.parse(json), null, 2);
  } catch {
    return json;
  }
}
