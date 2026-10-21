// Connect clients for the agent-demo API (agentdemo.v1, binary protobuf only; see docs/adr/0001).
import { createClient, type Client } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";

import {
  AgentService,
  MachineService,
  SheetService,
  type RunEvent,
  type RunSummary,
} from "../gen/agentdemo/v1/agent_pb.js";

export type { RunEvent, RunSummary };

/** The server that served this page. */
const transport = createConnectTransport({ baseUrl: window.location.origin, useBinaryFormat: true });

export const agentClient: Client<typeof AgentService> = createClient(AgentService, transport);
export const sheetClient: Client<typeof SheetService> = createClient(SheetService, transport);
export const machineClient: Client<typeof MachineService> = createClient(MachineService, transport);

export interface FollowOptions<T> {
  /** Opens the stream; called again after every failure (read your resume cursor here). */
  open(signal: AbortSignal, onHeader: () => void): AsyncIterable<T>;
  onMessage(message: T): void;
  /** Called when the stream is up (true) or lost and being retried (false). */
  onConnection?(connected: boolean): void;
  /** `true` once a normal end of the stream means there is nothing more to wait for. */
  complete?(): boolean;
  signal: AbortSignal;
}

/**
 * Follows a server stream until `signal` aborts or `complete()` says it is done, reconnecting
 * with backoff (0.5 s doubling to 5 s) whenever the server goes away.
 */
export async function follow<T>(opts: FollowOptions<T>): Promise<void> {
  let delay = 500;
  while (!opts.signal.aborted) {
    try {
      // Connected once the server answers with response headers, not when the request starts:
      // a failed retry must not look like a (briefly) working stream.
      for await (const message of opts.open(opts.signal, () => opts.onConnection?.(true))) {
        delay = 500;
        opts.onMessage(message);
      }
      if (opts.complete?.()) return;
    } catch (err) {
      if (opts.signal.aborted) return;
      console.warn("stream failed, retrying", err);
    }
    opts.onConnection?.(false);
    await new Promise((resolve) => setTimeout(resolve, delay));
    delay = Math.min(delay * 2, 5000);
  }
}
