import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";

import { BlockKind, RunEventSchema, RunStatus, type RunEvent } from "../gen/agentdemo/v1/agent_pb.js";
import { applyEvent, emptyRun, itemsOf, pendingApprovals, type RunView } from "./state.js";

let seq = 0n;
function ev(kind: RunEvent["kind"], agentId = ""): RunEvent {
  return create(RunEventSchema, { seq: seq++, agentId, kind });
}
const play = (events: RunEvent[], from: RunView = emptyRun) => events.reduce(applyEvent, from);

describe("applyEvent", () => {
  it("concatenates streamed deltas into one block", () => {
    seq = 0n;
    const view = play([
      ev({ case: "blockStarted", value: { blockId: "b", kind: BlockKind.TEXT } as never }),
      ev({ case: "blockDelta", value: { blockId: "b", text: "Hello " } as never }),
      ev({ case: "blockDelta", value: { blockId: "b", text: "world" } as never }),
      ev({ case: "blockFinished", value: { blockId: "b" } as never }),
    ]);
    expect(view.items).toMatchObject([{ type: "block", text: "Hello world", done: true, thinking: false }]);
    expect(view.nextSeq).toBe(4n);
  });

  it("ignores events it already applied (replay after a reconnect)", () => {
    seq = 0n;
    const start = ev({ case: "blockStarted", value: { blockId: "b", kind: BlockKind.TEXT } as never });
    const delta = ev({ case: "blockDelta", value: { blockId: "b", text: "x" } as never });
    const view = play([start, delta, delta]);
    expect(view.items).toMatchObject([{ text: "x" }]);
    expect(view.events).toBe(2);
  });

  it("tracks approvals and nests sub-agent items", () => {
    seq = 0n;
    const view = play([
      ev({ case: "subagentStarted", value: { subagentId: "s1", name: "Reviewer", task: "t" } as never }),
      ev({ case: "toolCallStarted", value: { callId: "c1", tool: "write_file", inputJson: "{}" } as never }, "s1"),
      ev({ case: "toolCallApprovalRequested", value: { callId: "c1", rationale: "why" } as never }, "s1"),
    ]);
    expect(view.status).toBe(RunStatus.AWAITING_APPROVAL);
    expect(pendingApprovals(view).map((t) => t.id)).toEqual(["c1"]);
    expect(itemsOf(view, "").map((i) => i.id)).toEqual(["s1"]);
    expect(itemsOf(view, "s1").map((i) => i.id)).toEqual(["c1"]);
    const decided = play([ev({ case: "toolCallDecided", value: { callId: "c1", approved: true, reason: "" } as never })], view);
    expect(pendingApprovals(decided)).toEqual([]);
    expect(decided.status).toBe(RunStatus.RUNNING);
  });
});
