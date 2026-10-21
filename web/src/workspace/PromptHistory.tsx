// The recent prompts, from the server's activity log (so they survive restarts and are the same
// in every browser): a History menu in the composer, and ↑ / ↓ in an empty prompt to step
// through them like a shell (history.ts).
import { useCallback, useEffect, useRef, useState } from "react";

import { Button } from "../design/index.js";
import { agentClient } from "../api/client.js";
import type { PromptEntry } from "../gen/agentdemo/v1/agent_pb.js";

const SHOWN = 50;

export function usePromptHistory(): { prompts: PromptEntry[]; refresh(): void } {
  const [prompts, setPrompts] = useState<PromptEntry[]>([]);
  const refresh = useCallback(() => {
    agentClient
      .listPrompts({ limit: SHOWN })
      .then((res) => setPrompts(res.prompts))
      .catch(() => {
        // The history is a convenience: without it the composer still works.
      });
  }, []);
  useEffect(refresh, [refresh]);
  return { prompts, refresh };
}

function ago(atMs: bigint): string {
  const s = Math.max(0, (Date.now() - Number(atMs)) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86_400) return `${Math.floor(s / 3600)} h ago`;
  return new Date(Number(atMs)).toLocaleDateString();
}

/** The History button and its list. */
export function HistoryMenu({ prompts, onPick, onOpen }: { prompts: PromptEntry[]; onPick(text: string): void; onOpen(): void }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: Event) => {
      if (e instanceof KeyboardEvent ? e.key === "Escape" : !ref.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", close);
    document.addEventListener("keydown", close);
    return () => {
      document.removeEventListener("pointerdown", close);
      document.removeEventListener("keydown", close);
    };
  }, [open]);
  return (
    <div className="history" ref={ref}>
      <Button
        size="sm"
        variant="ghost"
        aria-expanded={open}
        title="Recent prompts (↑ in an empty prompt steps through them)"
        onClick={() => {
          if (!open) onOpen();
          setOpen(!open);
        }}
      >
        History
      </Button>
      {open && (
        <ul className="sd-frame history__list" role="menu" aria-label="Recent prompts">
          {prompts.length === 0 && <li className="muted history__empty">No prompts yet.</li>}
          {prompts.map((p) => (
            <li key={`${p.runId}-${p.atMs}`} role="none">
              <button
                type="button"
                role="menuitem"
                className="history__item"
                onClick={() => {
                  onPick(p.prompt);
                  setOpen(false);
                }}
              >
                <span className="history__text">{p.prompt}</span>
                <span className="history__meta">
                  {ago(p.atMs)}
                  {p.model && ` · ${p.model}`}
                  {p.scenarioId !== "chat" && ` · ${p.scenarioId}`}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
