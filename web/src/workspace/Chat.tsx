// The chat: one conversation of runs (each turn is a run of the "chat" scenario, so it streams
// and replays like any other), a composer with a model picker, presets and the settings.
import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";

import { Badge, Button, Panel } from "../design/index.js";
import { agentClient, type RunSummary } from "../api/client.js";
import type { LlmSettings } from "../gen/agentdemo/v1/agent_pb.js";
import { Plan, statusBadge } from "../agent/Inspector.js";
import { Transcript } from "../agent/Transcript.js";
import { useRun, useRuns } from "../agent/live.js";
import { isEnded } from "../agent/state.js";
import { SettingsDialog } from "./SettingsDialog.js";
import { useSettings } from "./settings.js";
import { usePersistedFlag } from "./layout.js";
import { useHistoryKeys } from "./history.js";
import { HistoryMenu, usePromptHistory } from "./PromptHistory.js";

const CONVERSATION_KEY = "agent-demo.conversation";

const PRESETS: ReadonlyArray<{ label: string; scenario: string; prompt: string }> = [
  { label: "Live monitor", scenario: "monitor", prompt: "" },
  { label: "Top processes", scenario: "chat", prompt: "Put the ten busiest processes (by CPU) into the sheet as a formatted table, sorted, with a bar chart." },
  { label: "CPU cell", scenario: "chat", prompt: "Make B2 show the live CPU load, refreshed every second, with a label in A2." },
];

function newConversation(): string {
  const id = `chat-${Math.random().toString(36).slice(2, 10)}`;
  try {
    localStorage.setItem(CONVERSATION_KEY, id);
  } catch {
    // Private mode: the conversation lasts as long as the page.
  }
  return id;
}

function storedConversation(): string {
  try {
    return localStorage.getItem(CONVERSATION_KEY) ?? newConversation();
  } catch {
    return newConversation();
  }
}

function Turn({ run }: { run: RunSummary }) {
  const { view } = useRun(run.runId);
  const usage = view.usage ?? run.usage;
  return (
    <li className="turn">
      <p className="turn__prompt">{run.prompt}</p>
      {view.plan.length > 0 && <div className="turn__plan"><Plan steps={view.plan} /></div>}
      <Transcript view={view} />
      {view.error && <p className="error">{view.error}</p>}
      <footer className="turn__meta">
        {statusBadge(view.status || run.status)}
        <span>{run.model || run.scenarioId}</span>
        {usage && <span>{usage.inputTokens} in · {usage.outputTokens} out</span>}
        <a href={`#${run.runId}`}>{run.runId}</a>
      </footer>
    </li>
  );
}

function Composer({ settings, busyRun, onSend }: { settings: LlmSettings | null; busyRun: RunSummary | undefined; onSend(scenario: string, prompt: string, model: string): Promise<void> }) {
  const [prompt, setPrompt] = useState("");
  const [model, setModel] = useState("");
  const [error, setError] = useState("");
  const history = usePromptHistory();
  const historyKeys = useHistoryKeys(history.prompts, prompt, setPrompt);
  const send = (scenario: string, text: string) => {
    if (busyRun || (scenario === "chat" && !text.trim())) return;
    setError("");
    onSend(scenario, text, model)
      .then(() => {
        setPrompt("");
        history.refresh();
      })
      .catch((err: unknown) => setError(err instanceof Error ? err.message : String(err)));
  };
  return (
    <div className="composer">
      <textarea
        className="sd-input composer__prompt"
        rows={3}
        placeholder="Ask the agent: read the machine, fill the sheet, compile a live widget…"
        value={prompt}
        onChange={(e) => setPrompt(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) send("chat", prompt);
          else historyKeys(e);
        }}
      />
      <div className="composer__bar">
        <select className="sd-input composer__model" aria-label="Model" value={model || settings?.model || ""} onChange={(e) => setModel(e.target.value)}>
          {(settings?.models ?? []).map((m) => <option key={m} value={m}>{m}</option>)}
        </select>
        {busyRun ? (
          <Button onClick={() => void agentClient.cancelRun({ runId: busyRun.runId })}>Stop</Button>
        ) : (
          <Button variant="primary" hint="⌘⏎" disabled={!prompt.trim()} onClick={() => send("chat", prompt)}>Send</Button>
        )}
      </div>
      <div className="composer__presets">
        {PRESETS.map((p) => (
          <Button key={p.label} size="sm" variant="ghost" disabled={busyRun !== undefined} onClick={() => send(p.scenario, p.prompt)}>{p.label}</Button>
        ))}
        <HistoryMenu prompts={history.prompts} onPick={setPrompt} onOpen={history.refresh} />
      </div>
      {error && <p className="error">{error}</p>}
    </div>
  );
}

/** Keeps `ref` scrolled to the bottom while `on`, as the transcript inside it grows. */
function useFollow(ref: React.RefObject<HTMLElement | null>, on: boolean): void {
  useEffect(() => {
    const el = ref.current;
    if (!on || !el) return;
    const toBottom = () => (el.scrollTop = el.scrollHeight);
    toBottom();
    // Turns render their own streams, so watch the DOM rather than this component's state.
    const observer = new MutationObserver(toBottom);
    observer.observe(el, { childList: true, subtree: true, characterData: true });
    return () => observer.disconnect();
  }, [ref, on]);
}

/** `minimize` is the header button that folds the chat into the app bar. */
export function Chat({ hidden, minimize }: { hidden?: boolean; minimize?: ReactNode }) {
  const [conversationId, setConversationId] = useState(storedConversation);
  const [editing, setEditing] = useState(false);
  const { settings, saved } = useSettings();
  const { runs } = useRuns();
  const turns = runs.filter((r) => r.conversationId === conversationId).reverse();
  const busyRun = turns.find((r) => !isEnded(r.status));
  const [follow, setFollow] = usePersistedFlag("agent-demo.chat-follow", true);
  const scroller = useRef<HTMLOListElement>(null);
  useFollow(scroller, follow);
  const send = async (scenario: string, prompt: string, model: string) => {
    // A preset with no prompt of its own uses the scenario's.
    let text = prompt;
    if (!text.trim()) {
      const { scenarios } = await agentClient.listScenarios({});
      text = scenarios.find((s) => s.id === scenario)?.prompt ?? "";
    }
    await agentClient.startRun({ scenarioId: scenario, prompt: text, conversationId, model });
  };
  const aside = (
    <span className="chat__aside">
      {settings && !settings.apiKeySet && <Badge tone="warning">no API key</Badge>}
      <label className="chat__follow" title="Scroll along as the agent writes">
        <input type="checkbox" checked={follow} onChange={(e) => setFollow(e.target.checked)} />
        Auto-scroll
      </label>
      <Button size="sm" variant="ghost" onClick={() => setConversationId(newConversation())}>New chat</Button>
      <Button size="sm" variant="ghost" disabled={!settings} onClick={() => setEditing(true)}>Settings</Button>
      {minimize}
    </span>
  );
  return (
    <Panel title="Chat" className="chat" aside={aside} hidden={hidden}>
      <ol className="turns" ref={scroller}>
        {turns.length === 0 && (
          <li className="muted">
            The agent sees this machine and the sheet. Ask it to lay out a sheet, fill it with live data, or compile a WebAssembly widget that keeps it updating.
          </li>
        )}
        {turns.map((r) => <Turn key={r.runId} run={r} />)}
      </ol>
      <Composer settings={settings} busyRun={busyRun} onSend={send} />
      {editing && settings && <SettingsDialog settings={settings} onClose={() => setEditing(false)} onSaved={saved} />}
    </Panel>
  );
}
