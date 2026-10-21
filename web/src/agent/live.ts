// Live data hooks: the run list (WatchRuns, one stream shared by the page) and one run's view
// (WatchRun, resumed from the view's cursor after every reconnect).
import { useEffect, useReducer, useState, useSyncExternalStore } from "react";

import { agentClient, follow, type RunSummary } from "../api/client.js";
import { applyEvent, emptyRun, isEnded, type RunView } from "./state.js";
import type { RunEvent } from "../gen/agentdemo/v1/agent_pb.js";

interface RunsState {
  runs: RunSummary[];
  connected: boolean;
}

let runsState: RunsState = { runs: [], connected: false };
const listeners = new Set<() => void>();
let started = false;

function setRuns(update: (s: RunsState) => RunsState): void {
  runsState = update(runsState);
  for (const listener of listeners) listener();
}

/** Inserts or replaces a summary, newest run first. */
function upsert(runs: RunSummary[], run: RunSummary): RunSummary[] {
  const rest = runs.filter((r) => r.runId !== run.runId);
  return [run, ...rest].sort((a, b) => Number(b.startedAtMs - a.startedAtMs));
}

function subscribe(listener: () => void): () => void {
  if (!started) {
    started = true;
    void follow({
      open: (signal, onHeader) => agentClient.watchRuns({}, { signal, onHeader }),
      onMessage: (res) => {
        const run = res.run;
        if (run) setRuns((s) => ({ ...s, runs: upsert(s.runs, run) }));
      },
      onConnection: (connected) => setRuns((s) => ({ ...s, connected })),
      signal: new AbortController().signal,
    });
  }
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Every run's summary, newest first, and whether the list stream is up. */
export function useRuns(): RunsState {
  return useSyncExternalStore(subscribe, () => runsState);
}

type RunAction = { type: "reset" } | { type: "event"; event: RunEvent };

function runReducer(view: RunView, action: RunAction): RunView {
  return action.type === "reset" ? emptyRun : applyEvent(view, action.event);
}

/** The live view of run `runId` (empty while `runId` is null), and whether its stream is up. */
export function useRun(runId: string | null): { view: RunView; connected: boolean } {
  const [view, dispatch] = useReducer(runReducer, emptyRun);
  const [connected, setConnected] = useState(false);
  useEffect(() => {
    dispatch({ type: "reset" });
    if (runId === null) return;
    const abort = new AbortController();
    // The cursor and status live outside React state so a reconnect reads the latest ones.
    let cursor = 0n;
    let ended = false;
    void follow({
      open: (signal, onHeader) => agentClient.watchRun({ runId, fromSeq: cursor }, { signal, onHeader }),
      onMessage: ({ event }) => {
        if (!event) return;
        cursor = event.seq + 1n;
        if (event.kind.case === "runFinished") ended = isEnded(event.kind.value.status);
        dispatch({ type: "event", event });
      },
      onConnection: setConnected,
      complete: () => ended,
      signal: abort.signal,
    });
    return () => abort.abort();
  }, [runId]);
  return { view, connected };
}
