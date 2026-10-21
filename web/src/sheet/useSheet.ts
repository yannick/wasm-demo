// The live sheet: an iframe running GridCraft (WebAssembly) kept in step with the server's log.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { follow, sheetClient } from "../api/client.js";
import { SheetSync, type FrameOp, type SyncStats } from "./sync.js";

export const SHEET_URL = "/static/sheet/index.html";

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

export interface Sheet extends SheetCaller {
  frameRef: React.RefObject<HTMLIFrameElement | null>;
  /** Changes when the frame must reload (a reset); use it as the iframe's key. */
  frameKey: number;
  status: "loading" | "ready" | "failed";
  error: string;
  connected: boolean;
  stats: SyncStats;
  /** Runs a control-channel method in the frame; its edits sync like the user's own. */
  call(method: string, params: unknown): Promise<CallResult>;
  /** Clears the workbook for everyone. */
  reset(): Promise<void>;
}

interface Message {
  type: string;
  body: Record<string, unknown>;
}

function post(frame: HTMLIFrameElement | null, type: string, body: unknown): void {
  frame?.contentWindow?.postMessage({ type, json: JSON.stringify(body) }, window.location.origin);
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

/** The sheet of this page; `active` false: no frame and no stream (the sheet is elsewhere). */
export function useSheet(active = true): Sheet {
  const frameRef = useRef<HTMLIFrameElement | null>(null);
  const [frameKey, setFrameKey] = useState(0);
  const [status, setStatus] = useState<Sheet["status"]>("loading");
  const [error, setError] = useState("");
  const [connected, setConnected] = useState(false);
  const [stats, setStats] = useState<SyncStats>({ seen: 0, sent: 0, rejected: [], lastOrigin: "" });
  const calls = useRef(new Map<number, (r: CallResult) => void>());
  const nextCall = useRef(1);

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
      ),
    // A fresh sync (cursor 0) whenever the frame comes back: a new frame starts blank.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [active],
  );

  const onMessage = useCallback(
    (m: Message) => {
      if (m.type === "sheet:ready") {
        setStatus("ready");
        sync.frameReady();
      } else if (m.type === "sheet:changed") {
        sync.local((m.body.ops as FrameOp[] | undefined) ?? []);
      } else if (m.type === "sheet:reply") {
        const id = Number(m.body.id);
        calls.current.get(id)?.(m.body.response as CallResult);
        calls.current.delete(id);
      } else if (m.type === "sheet:failed") {
        setStatus("failed");
        setError(String(m.body.error));
      }
    },
    [sync],
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

  const call = useCallback((method: string, params: unknown) => {
    const id = nextCall.current++;
    return new Promise<CallResult>((resolve) => {
      calls.current.set(id, resolve);
      post(frameRef.current, "sheet:call", { id, method, params });
      setTimeout(() => {
        if (calls.current.delete(id)) resolve({ ok: false, error: "the sheet did not answer" });
      }, 10_000);
    });
  }, []);
  const reset = useCallback(async () => {
    await sheetClient.resetSheet({});
  }, []);

  return { frameRef, frameKey, status, error, connected, stats, call, reset, ready: status === "ready" };
}
