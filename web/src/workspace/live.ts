// Live machine metrics (WatchMetrics) and widgets (WatchWidgets), one stream each per hook.
import { useEffect, useState } from "react";

import { follow, machineClient } from "../api/client.js";
import type { MachineMetrics, Widget } from "../gen/agentdemo/v1/agent_pb.js";

export function useMetrics(): { metrics: MachineMetrics | null; connected: boolean } {
  const [metrics, setMetrics] = useState<MachineMetrics | null>(null);
  const [connected, setConnected] = useState(false);
  useEffect(() => {
    const abort = new AbortController();
    void follow({
      open: (signal, onHeader) => machineClient.watchMetrics({}, { signal, onHeader }),
      onMessage: (res) => setMetrics(res.metrics ?? null),
      onConnection: setConnected,
      signal: abort.signal,
    });
    return () => abort.abort();
  }, []);
  return { metrics, connected };
}

/** Every widget, by name, sorted. */
export function useWidgets(): Widget[] {
  const [widgets, setWidgets] = useState<Map<string, Widget>>(new Map());
  useEffect(() => {
    const abort = new AbortController();
    void follow({
      // A new stream starts with every widget: start from an empty map.
      open: (signal, onHeader) =>
        machineClient.watchWidgets({}, { signal, onHeader: () => (setWidgets(new Map()), onHeader()) }),
      onMessage: ({ widget, removed }) => {
        if (!widget) return;
        setWidgets((prev) => {
          const next = new Map(prev);
          if (removed) next.delete(widget.name);
          else next.set(widget.name, widget);
          return next;
        });
      },
      signal: abort.signal,
    });
    return () => abort.abort();
  }, []);
  return [...widgets.values()].sort((a, b) => a.name.localeCompare(b.name));
}
