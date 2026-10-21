// Keeps the GridCraft frame in step with the server's sheet log (SheetService).
//
// Remote ops (the agent, widgets, other tabs) are queued until the frame is ready and then
// applied; the frame does not report them back. Local ops (the user editing the sheet) go to
// ApplySheetOps in order, and come back in the log with this client's origin, so they are
// skipped there. A `reset` (the sheet was cleared, or the server restarted) reloads the frame.
// Ops listed in `skip` (by seq) are never applied: one crashed the sheet, so the replay after a
// reload leaves it out.
import type { WatchSheetResponse } from "../gen/agentdemo/v1/agent_pb.js";

/** An engine command as the frame takes it; `seq` (from the server's log) names it in a crash. */
export interface FrameOp {
  seq?: string;
  command: string;
  params: unknown;
}

export interface Frame {
  apply(ops: FrameOp[]): void;
  /** Reloads the frame with a blank workbook; it signals ready again. */
  reload(): void;
}

export interface SyncStats {
  /** Ops applied from the log (including this client's own, already shown). */
  seen: number;
  /** Local ops sent to the server. */
  sent: number;
  /** Local ops the server refused (the sheets may differ until the cells are written again). */
  rejected: string[];
  /** Origin of the last remote op, e.g. "agent" or "widget:cpu". */
  lastOrigin: string;
  /** Ops left out because they crashed the sheet. */
  skipped?: number;
}

export class SheetSync {
  readonly clientId: string;
  /** Resume point: the next seq to ask for, and which log it belongs to. */
  cursor = 0n;
  logId = "";
  stats: SyncStats = { seen: 0, sent: 0, rejected: [], lastOrigin: "" };
  private ready = false;
  private pending: FrameOp[] = [];
  private sending: Promise<void> = Promise.resolve();

  constructor(
    private readonly frame: Frame,
    private readonly send: (ops: FrameOp[]) => Promise<string[]>,
    private readonly onStats: (stats: SyncStats) => void = () => {},
    clientId?: string,
    private readonly skip: ReadonlySet<string> = new Set(),
  ) {
    this.clientId = clientId ?? `tab-${Math.random().toString(36).slice(2, 10)}`;
  }

  /** A batch from WatchSheet. */
  receive(batch: WatchSheetResponse): void {
    if (batch.reset) {
      this.pending = [];
      this.ready = false;
      this.stats = { ...this.stats, seen: 0, rejected: [] };
      this.frame.reload();
    }
    this.logId = batch.logId;
    const own = `client:${this.clientId}`;
    for (const op of batch.ops) {
      this.cursor = op.seq + 1n;
      if (op.origin === own) continue;
      const seq = op.seq.toString();
      if (this.skip.has(seq)) {
        this.stats.skipped = (this.stats.skipped ?? 0) + 1;
        continue;
      }
      this.pending.push({ seq, command: op.command, params: parse(op.paramsJson) });
      this.stats.lastOrigin = op.origin;
    }
    this.stats = { ...this.stats, seen: this.stats.seen + batch.ops.length };
    this.flush();
    this.onStats(this.stats);
  }

  /** The frame (re)started and listens. */
  frameReady(): void {
    this.ready = true;
    this.flush();
  }

  /** The user changed the sheet; sent in order, one request at a time. */
  local(ops: FrameOp[]): void {
    if (ops.length === 0) return;
    this.sending = this.sending.then(async () => {
      try {
        const rejected = await this.send(ops);
        this.stats = { ...this.stats, sent: this.stats.sent + ops.length, rejected: [...this.stats.rejected, ...rejected].slice(-20) };
      } catch (err) {
        this.stats = { ...this.stats, rejected: [...this.stats.rejected, `not sent: ${String(err)}`].slice(-20) };
      }
      this.onStats(this.stats);
    });
  }

  private flush(): void {
    if (!this.ready || this.pending.length === 0) return;
    const ops = this.pending;
    this.pending = [];
    this.frame.apply(ops);
  }
}

function parse(json: string): unknown {
  try {
    return JSON.parse(json);
  } catch {
    return {};
  }
}
