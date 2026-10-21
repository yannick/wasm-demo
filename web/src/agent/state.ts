// A run's view state, built from its RunEvents by a pure reducer: the transcript (text and
// thinking blocks, tool calls, sub-agents), the plan, usage and status. `nextSeq` is the resume
// cursor: after a reconnect WatchRun continues from there, so nothing is applied twice.
import {
  BlockKind,
  RunStatus,
  type PlanStep,
  type RunEvent,
  type RunSummary,
  type Usage,
} from "../gen/agentdemo/v1/agent_pb.js";

export interface BlockItem {
  type: "block";
  id: string;
  agentId: string;
  thinking: boolean;
  text: string;
  done: boolean;
}

export interface Approval {
  rationale: string;
  decision?: { approved: boolean; reason: string };
}

export interface ToolItem {
  type: "tool";
  id: string;
  agentId: string;
  tool: string;
  input: string;
  output?: string;
  isError: boolean;
  durationMs?: number;
  approval?: Approval;
}

export interface SubagentItem {
  type: "subagent";
  id: string;
  /** The parent agent ("" for the top-level one). */
  agentId: string;
  name: string;
  task: string;
  summary?: string;
  done: boolean;
}

export type Item = BlockItem | ToolItem | SubagentItem;

export interface RunView {
  summary?: RunSummary;
  status: RunStatus;
  error: string;
  /** Transcript items in arrival order; `index` finds one by id in O(1). */
  items: Item[];
  index: Record<string, number>;
  plan: PlanStep[];
  usage?: Usage;
  nextSeq: bigint;
  /** Number of events applied, for the event counter. */
  events: number;
}

export const emptyRun: RunView = {
  status: RunStatus.UNSPECIFIED,
  error: "",
  items: [],
  index: {},
  plan: [],
  nextSeq: 0n,
  events: 0,
};

export function isEnded(status: RunStatus): boolean {
  return status === RunStatus.SUCCEEDED || status === RunStatus.FAILED || status === RunStatus.CANCELLED;
}

/** The view after `event`; an event the view already has (seq < nextSeq) changes nothing. */
export function applyEvent(view: RunView, event: RunEvent): RunView {
  if (event.seq < view.nextSeq) return view;
  const next = { ...view, nextSeq: event.seq + 1n, events: view.events + 1 };
  return applyKind(next, event);
}

function applyKind(view: RunView, event: RunEvent): RunView {
  const { kind, agentId } = event;
  switch (kind.case) {
    case "runStarted":
      return { ...view, summary: kind.value.run, status: RunStatus.RUNNING };
    case "blockStarted": {
      const thinking = kind.value.kind === BlockKind.THINKING;
      return append(view, { type: "block", id: kind.value.blockId, agentId, thinking, text: "", done: false });
    }
    case "blockDelta":
      return patch<BlockItem>(view, kind.value.blockId, (b) => ({ ...b, text: b.text + kind.value.text }));
    case "blockFinished":
      return patch<BlockItem>(view, kind.value.blockId, (b) => ({ ...b, done: true }));
    case "planUpdated":
      return { ...view, plan: kind.value.steps };
    case "usageUpdated":
      return { ...view, usage: kind.value.usage };
    case "runFinished":
      return { ...view, status: kind.value.status, error: kind.value.error };
    default:
      return applyAgentKind(view, event);
  }
}

/** Tool calls and sub-agents. */
function applyAgentKind(view: RunView, { kind, agentId }: RunEvent): RunView {
  switch (kind.case) {
    case "toolCallStarted": {
      const { callId, tool, inputJson } = kind.value;
      return append(view, { type: "tool", id: callId, agentId, tool, input: inputJson, isError: false });
    }
    case "toolCallApprovalRequested":
      return patch<ToolItem>({ ...view, status: RunStatus.AWAITING_APPROVAL }, kind.value.callId, (t) => ({
        ...t,
        approval: { rationale: kind.value.rationale },
      }));
    case "toolCallDecided": {
      const { approved, reason } = kind.value;
      return patch<ToolItem>({ ...view, status: RunStatus.RUNNING }, kind.value.callId, (t) => ({
        ...t,
        approval: { rationale: t.approval?.rationale ?? "", decision: { approved, reason } },
      }));
    }
    case "toolCallFinished": {
      const { output, isError, durationMs } = kind.value;
      return patch<ToolItem>(view, kind.value.callId, (t) => ({ ...t, output, isError, durationMs }));
    }
    case "subagentStarted": {
      const { subagentId, name, task } = kind.value;
      return append(view, { type: "subagent", id: subagentId, agentId, name, task, done: false });
    }
    case "subagentFinished":
      return patch<SubagentItem>(view, kind.value.subagentId, (s) => ({ ...s, summary: kind.value.summary, done: true }));
    default:
      return view;
  }
}

function append(view: RunView, item: Item): RunView {
  return { ...view, items: [...view.items, item], index: { ...view.index, [item.id]: view.items.length } };
}

function patch<T extends Item>(view: RunView, id: string, update: (item: T) => T): RunView {
  const at = view.index[id];
  if (at === undefined) return view;
  const items = view.items.slice();
  items[at] = update(items[at] as T);
  return { ...view, items };
}

/** The items `agentId` produced, in order (the top-level agent's for ""). */
export function itemsOf(view: RunView, agentId: string): Item[] {
  return view.items.filter((item) => item.agentId === agentId);
}

/** The tool calls waiting for a human. */
export function pendingApprovals(view: RunView): ToolItem[] {
  return view.items.filter((i): i is ToolItem => i.type === "tool" && i.approval !== undefined && !i.approval.decision);
}
