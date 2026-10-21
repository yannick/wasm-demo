// LLM settings: endpoint, models, key (write-only), sampling, the agent's limits and its
// briefing (the start of every system prompt: the setup, the tools, GridCraft's catalog).
import { useState } from "react";

import { Button, Dialog } from "../design/index.js";
import { agentClient } from "../api/client.js";
import type { LlmSettings } from "../gen/agentdemo/v1/agent_pb.js";

interface Draft {
  baseUrl: string;
  model: string;
  models: string;
  apiKeyEnv: string;
  apiKey: string;
  clearApiKey: boolean;
  temperature: string;
  maxTokens: string;
  maxToolRounds: string;
  systemPrompt: string;
  briefing: string;
  defaultBriefing: string;
}

function draftOf(s: LlmSettings): Draft {
  return {
    baseUrl: s.baseUrl,
    model: s.model,
    models: s.models.join("\n"),
    apiKeyEnv: s.apiKeyEnv,
    apiKey: "",
    clearApiKey: false,
    temperature: String(s.temperature),
    maxTokens: String(s.maxTokens),
    maxToolRounds: String(s.maxToolRounds),
    systemPrompt: s.systemPrompt,
    briefing: s.briefing,
    defaultBriefing: s.defaultBriefing,
  };
}

function Field({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <label className="field">
      <span className="sd-label">{label}</span>
      {children}
      {hint && <span className="field__hint">{hint}</span>}
    </label>
  );
}

function Section({ title, wide, children }: { title: string; wide?: boolean; children: React.ReactNode }) {
  return (
    <fieldset className={wide ? "settings__section settings__section--wide" : "settings__section"}>
      <legend className="sd-label">{title}</legend>
      {children}
    </fieldset>
  );
}

function Fields({ draft, set, keySet }: { draft: Draft; set(patch: Partial<Draft>): void; keySet: boolean }) {
  const text = (k: keyof Draft) => (e: React.ChangeEvent<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>) => set({ [k]: e.target.value });
  const models = draft.models.split(/[\n,]/).map((m) => m.trim()).filter(Boolean);
  return (
    <div className="settings">
      <Section title="Endpoint">
        <Field label="Base URL" hint="OpenAI-compatible; /v1/chat/completions is appended.">
          <input className="sd-input" value={draft.baseUrl} onChange={text("baseUrl")} />
        </Field>
        <Field label="API key" hint={keySet ? "A key is set. Type a new one to replace it." : `No key yet: set ${draft.apiKeyEnv} (./.env works) or paste one.`}>
          <input className="sd-input" type="password" autoComplete="off" value={draft.apiKey} onChange={text("apiKey")} placeholder={keySet ? "•••••••• (unchanged)" : "sk-…"} />
        </Field>
        <Field label="Key variable" hint="Read when no key is stored.">
          <input className="sd-input" value={draft.apiKeyEnv} onChange={text("apiKeyEnv")} />
        </Field>
        <label className="field--check">
          <input type="checkbox" checked={draft.clearApiKey} onChange={(e) => set({ clearApiKey: e.target.checked })} />
          <span>Forget the stored key (use the variable)</span>
        </label>
      </Section>
      <Section title="Models">
        <Field label="Default model">
          <select className="sd-input" value={draft.model} onChange={text("model")}>
            {[...new Set([draft.model, ...models])].map((m) => <option key={m} value={m}>{m}</option>)}
          </select>
        </Field>
        <Field label="Models offered" hint="One per line; the chat's picker shows these.">
          <textarea className="sd-input" rows={4} value={draft.models} onChange={text("models")} />
        </Field>
      </Section>
      <Section title="Generation">
        <div className="settings__row">
          <Field label="Temperature"><input className="sd-input" type="number" min={0} max={2} step={0.1} value={draft.temperature} onChange={text("temperature")} /></Field>
          <Field label="Max tokens"><input className="sd-input" type="number" min={256} step={256} value={draft.maxTokens} onChange={text("maxTokens")} /></Field>
          <Field label="Tool rounds"><input className="sd-input" type="number" min={1} max={64} value={draft.maxToolRounds} onChange={text("maxToolRounds")} /></Field>
        </div>
      </Section>
      <Briefing draft={draft} set={set} />
    </div>
  );
}

/** A rough size: about four characters per token. */
function size(text: string): string {
  return `${(text.length / 1024).toFixed(1)} KB · ~${Math.round(text.length / 4 / 100) / 10}k tokens`;
}

function Briefing({ draft, set }: { draft: Draft; set(patch: Partial<Draft>): void }) {
  const builtIn = draft.briefing.trim() === draft.defaultBriefing.trim();
  return (
    <Section title="Agent briefing" wide>
      <Field
        label={builtIn ? "Briefing (built-in)" : "Briefing (customized)"}
        hint={`The start of every system prompt: how the setup works, every tool, and GridCraft's commands, charts and functions (generated from the engine). ${size(draft.briefing)}, sent with every request.`}
      >
        <textarea className="sd-input settings__briefing" rows={16} spellCheck={false} value={draft.briefing} onChange={(e) => set({ briefing: e.target.value })} />
      </Field>
      <div className="settings__row settings__row--end">
        <Button size="sm" variant="ghost" disabled={builtIn} onClick={() => set({ briefing: draft.defaultBriefing })}>
          Reset to built-in
        </Button>
      </div>
      <Field label="Extra instructions" hint="Appended after the briefing.">
        <textarea className="sd-input" rows={3} value={draft.systemPrompt} onChange={(e) => set({ systemPrompt: e.target.value })} />
      </Field>
      <p className="field__hint">Applies from the next message on, in the current chat too.</p>
    </Section>
  );
}

export function SettingsDialog({ settings, onClose, onSaved }: { settings: LlmSettings; onClose(): void; onSaved(s: LlmSettings): void }) {
  const [draft, setDraft] = useState(() => draftOf(settings));
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const save = () => {
    setBusy(true);
    setError("");
    const update = {
      baseUrl: draft.baseUrl,
      model: draft.model,
      models: draft.models.split(/[\n,]/).map((m) => m.trim()).filter(Boolean),
      apiKeyEnv: draft.apiKeyEnv,
      apiKey: draft.apiKey,
      clearApiKey: draft.clearApiKey,
      temperature: Number(draft.temperature),
      maxTokens: Number(draft.maxTokens),
      maxToolRounds: Number(draft.maxToolRounds),
      systemPrompt: draft.systemPrompt,
      // Empty or unchanged means the built-in one (the server stores it as "none").
      briefing: draft.briefing.trim() ? draft.briefing : draft.defaultBriefing,
    };
    agentClient
      .saveSettings({ settings: update })
      .then((res) => {
        if (res.settings) onSaved(res.settings);
        onClose();
      })
      .catch((err: unknown) => setError(err instanceof Error ? err.message : String(err)))
      .finally(() => setBusy(false));
  };
  return (
    <Dialog
      open
      wide
      title="LLM settings"
      onClose={onClose}
      actions={
        <>
          {error && <span className="error settings__error">{error}</span>}
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={busy} onClick={save}>Save</Button>
        </>
      }
    >
      <Fields draft={draft} set={(patch) => setDraft((d) => ({ ...d, ...patch }))} keySet={settings.apiKeySet} />
    </Dialog>
  );
}
