// Shell-like history keys for the prompt: ↑ in an empty prompt recalls the previous prompt.
import { useRef } from "react";

import type { PromptEntry } from "../gen/agentdemo/v1/agent_pb.js";

function plain(e: React.KeyboardEvent): boolean {
  return !(e.shiftKey || e.altKey || e.metaKey || e.ctrlKey);
}

const STEP: Record<string, number> = { ArrowUp: 1, ArrowDown: -1 };

/**
 * Where ↑ (`step` 1) or ↓ (-1) goes from history entry `index` (-1: none shown): the next index,
 * or null to leave the key to the caret. Only from an empty prompt or one showing the entry.
 */
export function historyStep(index: number, step: number, count: number, showing: boolean, empty: boolean): number | null {
  const from = showing ? index : -1;
  if (!empty && from < 0) return null;
  const next = from + step;
  return next >= count ? null : Math.max(-1, next);
}

/** ↑ in an empty prompt shows the previous prompt, ↓ the next; past the newest it clears. */
export function useHistoryKeys(prompts: PromptEntry[], prompt: string, setPrompt: (text: string) => void) {
  const index = useRef(-1);
  return (e: React.KeyboardEvent<HTMLTextAreaElement>): void => {
    const step = plain(e) ? STEP[e.key] : undefined;
    if (step === undefined) return;
    // Edited since it was recalled: the arrows move the caret again.
    const showing = index.current >= 0 && prompts[index.current]?.prompt === prompt;
    const next = historyStep(index.current, step, prompts.length, showing, prompt === "");
    if (next === null) return;
    e.preventDefault();
    index.current = next;
    setPrompt(next < 0 ? "" : (prompts[next]?.prompt ?? ""));
  };
}
