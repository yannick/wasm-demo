// The live sheet: an iframe running GridCraft (WebAssembly) kept in step with the server's log.
//
// The frame can die: a panic stops it for good (it says so with `sheet:failed`, naming the op it
// was replaying), and a renderer that stops drawing stops answering pings. Either way the page
// reloads the frame once by itself, replaying the log without the op that crashed it; if that
// fails too, it shows the error with buttons to reload (or to switch to WebGL). Every such event
// goes to the server's activity log.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { agentClient, follow, sheetClient } from "../api/client.js";
import { usePersistedFlag } from "../workspace/layout.js";
import { SheetSync, type FrameOp, type SyncStats } from "./sync.js";

export const SHEET_URL = "/static/sheet/index.html";

/** Ping the frame this often while it is on screen; it is stalled after `STALL_MS` without a pong. */
const PING_MS = 5_000;
const STALL_MS = 15_000;
/** At most one automatic reload in this long: a sheet that keeps dying waits for the user. */
const AUTO_RELOAD_EVERY_MS = 60_000;

/** `{ ok, result }` or `{ ok: false, error }`, as GridCraft's control channel answers. */
export interface CallResult {
  ok: boolean;
  result?: unknown;
  error?: unknown;
}

/** What can run control-channel calls in a sheet (this page's frame, or the undocked window). */
export interface SheetCaller {
  ready: boolean;
  call(method: string, params: unknown): Promise<CallResult>;
}

/** Why the frame stopped. */
export interface SheetCrash {
  error: string;
  /** The op from the log that was running when it panicked (left out of the next replay). */
  op?: { seq: string; command: string };
  /** It stopped answering rather than reporting a panic. */
  stalled?: boolean;
  /** Reloaded by itself after this crash. */
  recovered?: boolean;
}

export interface Sheet extends SheetCaller {
  frameRef: React.RefObject<HTMLIFrameElement | null>;
  /** Changes when the frame must reload; use it as the iframe's key. */
  frameKey: number;
  frameUrl: string;
  status: "loading" | "ready" | "failed";
  error: string;
  /** The last crash, also after an automatic reload (so the page can say what happened). */
  crash: SheetCrash | null;
  /** The frame draws with WebGL instead of WebGPU. */
  webgl: boolean;
  connected: boolean;
  stats: SyncStats;
  /** Runs a control-channel method in the frame; its edits sync like the user's own. */
  call(method: string, params: unknown): Promise<CallResult>;
  /** Clears the workbook for everyone. */
  reset(): Promise<void>;
  /** A fresh frame replaying the server's log (minus ops that crashed it); optionally switches renderer. */
  reload(options?: { webgl?: boolean }): void;
  /** Forgets the last crash (hides its notice). */
  dismissCrash(): void;
}

interface Message {
  type: string;
  body: Record<string, unknown>;
}

function post(frame: HTMLIFrameElement | null, type: string, body: unknown): void {
  frame?.contentWindow?.postMessage({ type, json: JSON.stringify(body) }, window.location.origin);
}

/** Tells the server's activity log; failures don't matter here. */
function report(kind: string, message: string, detail: unknown): void {
  agentClient.reportClientEvent({ kind, message, detailJson: JSON.stringify(detail) }).catch(() => {});
}

/** Messages from our frame only (same origin, from its window). */
function useFrameMessages(frameRef: React.RefObject<HTMLIFrameElement | null>, onMessage: (m: Message) => void): void {
  useEffect(() => {
    const listener = (event: MessageEvent) => {
      if (event.origin !== window.location.origin || event.source !== frameRef.current?.contentWindow) return;
      const data = event.data as { type?: unknown; json?: unknown } | null;
      if (typeof data?.type !== "string" || typeof data.json !== "string") return;
      try {
        onMessage({ type: data.type, body: JSON.parse(data.json) as Record<string, unknown> });
      } catch {
        console.warn("bad message from the sheet", data.type);
      }
    };
    window.addEventListener("message", listener);
    return () => window.removeEventListener("message", listener);
  }, [frameRef, onMessage]);
}

function onScreen(frame: HTMLIFrameElement | null): boolean {
  return document.visibilityState === "visible" && !!frame && frame.offsetParent !== null && frame.clientWidth > 0;
}

/**
 * Pings a ready frame while it is on screen (a hidden one may not draw, so it isn't asked);
 * `onStall` when a ping stays unanswered for `STALL_MS`. Returns the pong handler.
 */
function useHeartbeat(frameRef: React.RefObject<HTMLIFrameElement | null>, ready: boolean, onStall: () => void): () => void {
  const waitingSince = useRef<number | null>(null);
  useEffect(() => {
    waitingSince.current = null;
    if (!ready) return;
    const timer = setInterval(() => {
      if (!onScreen(frameRef.current)) {
        waitingSince.current = null;
        return;
      }
      const since = waitingSince.current;
      if (since === null) {
        waitingSince.current = Date.now();
        post(frameRef.current, "sheet:ping", { id: Date.now() });
      } else if (Date.now() - since > STALL_MS) {
        waitingSince.current = null;
        onStall();
      }
    }, PING_MS);
    return () => clearInterval(timer);
  }, [frameRef, ready, onStall]);
  return useCallback(() => {
    waitingSince.current = null;
  }, []);
}

function crashOf(body: Record<string, unknown>): SheetCrash {
  const op = body.op as { seq?: unknown; command?: unknown } | null | undefined;
  const seq = op?.seq;
  return {
    error: String(body.error),
    op: seq !== undefined && seq !== null ? { seq: String(seq), command: String(op?.command ?? "") } : undefined,
  };
}

/** Calls into the frame by id, answered by `sheet:reply`. */
function useCalls(frameRef: React.RefObject<HTMLIFrameElement | null>) {
  const calls = useRef(new Map<number, (r: CallResult) => void>());
  const nextCall = useRef(1);
  const call = useCallback(
    (method: string, params: unknown) => {
      const id = nextCall.current++;
      return new Promise<CallResult>((resolve) => {
        calls.current.set(id, resolve);
        post(frameRef.current, "sheet:call", { id, method, params });
        setTimeout(() => {
          if (calls.current.delete(id)) resolve({ ok: false, error: "the sheet did not answer" });
        }, 10_000);
      });
    },
    [frameRef],
  );
  const answer = useCallback((body: Record<string, unknown>) => {
    const id = Number(body.id);
    calls.current.get(id)?.(body.response as CallResult);
    calls.current.delete(id);
  }, []);
  return { call, answer };
}

/** The sheet of this page; `active` false: no frame and no stream (the sheet is elsewhere). */
export function useSheet(active = true): Sheet {
  const frameRef = useRef<HTMLIFrameElement | null>(null);
  const [frameKey, setFrameKey] = useState(0);
  const [generation, setGeneration] = useState(0);
  const [status, setStatus] = useState<Sheet["status"]>("loading");
  const [error, setError] = useState("");
  const [crash, setCrash] = useState<SheetCrash | null>(null);
  const [webgl, setWebgl] = usePersistedFlag("agent-demo.sheet-webgl", false);
  const [connected, setConnected] = useState(false);
  const [stats, setStats] = useState<SyncStats>({ seen: 0, sent: 0, rejected: [], lastOrigin: "" });
  const skip = useRef(new Set<string>());
  const lastAutoReload = useRef(0);
  const { call, answer } = useCalls(frameRef);

  const sync = useMemo(
    () =>
      new SheetSync(
        {
          apply: (ops) => post(frameRef.current, "sheet:apply", { ops }),
          reload: () => {
            setStatus("loading");
            setFrameKey((k) => k + 1);
          },
        },
        async (ops) => {
          const wire = ops.map((o) => ({ command: o.command, paramsJson: JSON.stringify(o.params ?? {}) }));
          // `sync` is assigned by the time the frame reports a change.
          const res = await sheetClient.applySheetOps({ clientId: sync.clientId, ops: wire });
          return res.rejected;
        },
        (s) => setStats({ ...s }),
        undefined,
        skip.current,
      ),
    // A fresh sync (cursor 0) whenever the frame comes back or is reloaded: a new frame starts
    // blank and gets the whole log.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [active, generation],
  );

  const reload = useCallback(
    (options?: { webgl?: boolean }) => {
      if (options?.webgl !== undefined) setWebgl(options.webgl);
      setStatus("loading");
      setError("");
      setGeneration((g) => g + 1);
      setFrameKey((k) => k + 1);
    },
    [setWebgl],
  );

  const failed = useCallback(
    (next: SheetCrash) => {
      if (next.op) skip.current.add(next.op.seq);
      const auto = Date.now() - lastAutoReload.current > AUTO_RELOAD_EVERY_MS;
      report(next.stalled ? "sheet.stalled" : "sheet.crashed", next.error, { ...next, webgl, autoReload: auto, seen: sync.stats.seen });
      setCrash({ ...next, recovered: auto });
      setError(next.error);
      setStatus("failed");
      if (auto) {
        lastAutoReload.current = Date.now();
        setTimeout(() => reload(), 300);
      }
    },
    [reload, sync, webgl],
  );
  const stalled = useCallback(() => failed({ error: "The sheet stopped responding (it no longer draws).", stalled: true }), [failed]);
  const pong = useHeartbeat(frameRef, status === "ready", stalled);

  const onMessage = useCallback(
    (m: Message) => {
      const handlers: Record<string, () => void> = {
        "sheet:ready": () => {
          setStatus("ready");
          sync.frameReady();
        },
        "sheet:changed": () => sync.local((m.body.ops as FrameOp[] | undefined) ?? []),
        "sheet:reply": () => answer(m.body),
        "sheet:pong": pong,
        "sheet:failed": () => {
          if (m.body.crashed) {
            failed(crashOf(m.body));
          } else {
            // It never started (e.g. no WebGPU here): nothing to reload into.
            setStatus("failed");
            setError(String(m.body.error));
            report("sheet.start_failed", String(m.body.error), { webgl });
          }
        },
      };
      handlers[m.type]?.();
    },
    [sync, answer, pong, failed, webgl],
  );
  useFrameMessages(frameRef, onMessage);

  useEffect(() => {
    if (!active) return;
    setStatus("loading");
    const abort = new AbortController();
    void follow({
      open: (signal, onHeader) => sheetClient.watchSheet({ fromSeq: sync.cursor, logId: sync.logId }, { signal, onHeader }),
      onMessage: (batch) => sync.receive(batch),
      onConnection: setConnected,
      signal: abort.signal,
    });
    return () => abort.abort();
  }, [sync, active]);

  const reset = useCallback(async () => {
    await sheetClient.resetSheet({});
  }, []);

  const dismissCrash = useCallback(() => setCrash(null), []);
  const frameUrl = webgl ? `${SHEET_URL}?webgl` : SHEET_URL;
  return { frameRef, frameKey, frameUrl, status, error, crash, webgl, connected, stats, call, reset, reload, dismissCrash, ready: status === "ready" };
}
