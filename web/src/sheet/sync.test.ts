import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";

import { SheetOpSchema, WatchSheetResponseSchema } from "../gen/agentdemo/v1/agent_pb.js";
import { SheetSync, type FrameOp } from "./sync.js";

function op(seq: number, origin: string, cell = "A1") {
  return create(SheetOpSchema, { seq: BigInt(seq), command: "cell.set", paramsJson: JSON.stringify({ cell, input: "1" }), origin });
}

function setup() {
  const applied: FrameOp[][] = [];
  const sent: FrameOp[][] = [];
  let reloads = 0;
  const sync = new SheetSync(
    { apply: (ops) => applied.push(ops), reload: () => reloads++ },
    async (ops) => {
      sent.push(ops);
      return [];
    },
    () => {},
    "me",
  );
  return { sync, applied, sent, reloads: () => reloads };
}

describe("SheetSync", () => {
  it("queues remote ops until the frame is ready and skips its own", () => {
    const { sync, applied } = setup();
    sync.receive(create(WatchSheetResponseSchema, { ops: [op(0, "agent"), op(1, "client:me"), op(5, "widget:w", "B2")], logId: "L1" }));
    expect(applied).toEqual([]);
    sync.frameReady();
    expect(applied).toHaveLength(1);
    expect(applied[0]?.map((o) => (o.params as { cell: string }).cell)).toEqual(["A1", "B2"]);
    expect(sync.cursor).toBe(6n);
    expect(sync.logId).toBe("L1");
    expect(sync.stats.lastOrigin).toBe("widget:w");
  });

  it("reloads the frame on reset and replays from the new log", () => {
    const { sync, applied, reloads } = setup();
    sync.frameReady();
    sync.receive(create(WatchSheetResponseSchema, { ops: [op(0, "agent")], logId: "L1" }));
    sync.receive(create(WatchSheetResponseSchema, { reset: true, ops: [op(0, "agent", "C3")], logId: "L2" }));
    expect(reloads()).toBe(1);
    expect(applied).toHaveLength(1);
    sync.frameReady();
    expect(applied[1]?.[0]?.params).toEqual({ cell: "C3", input: "1" });
    expect(sync.cursor).toBe(1n);
  });

  it("sends local changes in order", async () => {
    const { sync, sent } = setup();
    sync.local([{ command: "cell.set", params: { cell: "A1" } }]);
    sync.local([]);
    sync.local([{ command: "cell.set", params: { cell: "A2" } }]);
    await new Promise((r) => setTimeout(r, 0));
    expect(sent.map((ops) => (ops[0]?.params as { cell: string }).cell)).toEqual(["A1", "A2"]);
    expect(sync.stats.sent).toBe(2);
  });

  it("leaves out ops that crashed the sheet and names the others by seq", () => {
    const applied: FrameOp[][] = [];
    const sync = new SheetSync({ apply: (ops) => applied.push(ops), reload: () => {} }, async () => [], () => {}, "me", new Set(["1"]));
    sync.frameReady();
    sync.receive(create(WatchSheetResponseSchema, { ops: [op(0, "agent"), op(1, "agent", "B1"), op(2, "agent", "C1")], logId: "L1" }));
    expect(applied[0]?.map((o) => o.seq)).toEqual(["0", "2"]);
    expect(sync.stats.skipped).toBe(1);
    expect(sync.cursor).toBe(3n);
  });
});
