// Which workspace panels are minimized into the app bar (kept per browser).
import { useCallback, useState } from "react";

export type PanelId = "chat" | "sheet" | "machine" | "widgets";

export const PANELS: ReadonlyArray<{ id: PanelId; label: string }> = [
  { id: "chat", label: "Chat" },
  { id: "sheet", label: "Sheet" },
  { id: "machine", label: "Machine" },
  { id: "widgets", label: "Wasm widgets" },
];

const KEY = "agent-demo.minimized";

function load(): Set<PanelId> {
  try {
    const ids: unknown = JSON.parse(localStorage.getItem(KEY) ?? "[]");
    return new Set(Array.isArray(ids) ? PANELS.map((p) => p.id).filter((id) => ids.includes(id)) : []);
  } catch {
    return new Set();
  }
}

function store(ids: Set<PanelId>) {
  try {
    localStorage.setItem(KEY, JSON.stringify([...ids]));
  } catch {
    // Private mode: it lasts as long as the page.
  }
}

export interface Minimized {
  has(id: PanelId): boolean;
  readonly count: number;
  toggle(id: PanelId): void;
  minimize(id: PanelId): void;
  restoreAll(): void;
}

export function useMinimized(): Minimized {
  const [ids, setIds] = useState(load);
  const update = useCallback((change: (next: Set<PanelId>) => void) => {
    setIds((prev) => {
      const next = new Set(prev);
      change(next);
      store(next);
      return next;
    });
  }, []);
  return {
    has: (id) => ids.has(id),
    count: ids.size,
    toggle: (id) => update((next) => (next.has(id) ? next.delete(id) : next.add(id))),
    minimize: (id) => update((next) => next.add(id)),
    restoreAll: () => update((next) => next.clear()),
  };
}
