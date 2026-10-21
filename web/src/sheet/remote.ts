// Calls into a sheet in another window of this page (the undocked sheet), over a
// BroadcastChannel: the sheet window serves them, the main window sends them.
import type { CallResult } from "./useSheet.js";

const CHANNEL = "agent-demo.sheet";

interface Request {
  kind: "call";
  id: string;
  method: string;
  params: unknown;
}

interface Reply {
  kind: "reply";
  id: string;
  result: CallResult;
}

/**
 * In the sheet window: answers calls with `call` until the returned function stops it. Every
 * call gets a reply, so the main window never waits out its timeout: an error while the sheet
 * is still loading (`ready()` false) or when the call fails.
 */
export function serveCalls(call: (method: string, params: unknown) => Promise<CallResult>, ready: () => boolean): () => void {
  const channel = new BroadcastChannel(CHANNEL);
  channel.onmessage = (e: MessageEvent<Request | Reply>) => {
    const msg = e.data;
    if (msg.kind !== "call") return;
    const reply = (result: CallResult) => channel.postMessage({ kind: "reply", id: msg.id, result } satisfies Reply);
    if (!ready()) {
      reply({ ok: false, error: "the sheet window is still loading (or hidden): bring it to the front and retry" });
      return;
    }
    call(msg.method, msg.params).then(reply, (err: unknown) => reply({ ok: false, error: String(err) }));
  };
  return () => channel.close();
}

/**
 * From the main window: a call answered by the sheet window, or an error after `timeoutMs`
 * (longer than the sheet's own call timeout, so its answer wins).
 */
export function remoteCall(method: string, params: unknown, timeoutMs = 15_000): Promise<CallResult> {
  const channel = new BroadcastChannel(CHANNEL);
  const id = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
  return new Promise<CallResult>((resolve) => {
    const done = (result: CallResult) => {
      clearTimeout(timer);
      channel.close();
      resolve(result);
    };
    const timer = setTimeout(() => done({ ok: false, error: "the sheet window did not answer" }), timeoutMs);
    channel.onmessage = (e: MessageEvent<Request | Reply>) => {
      if (e.data.kind === "reply" && e.data.id === id) done(e.data.result);
    };
    channel.postMessage({ kind: "call", id, method, params } satisfies Request);
  });
}
