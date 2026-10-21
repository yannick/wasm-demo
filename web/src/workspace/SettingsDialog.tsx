// LLM settings: endpoint, models, key (write-only), sampling and the agent's limits.
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

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <fieldset className="settings__section">
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
      <Section title="System prompt">
        <Field label="Extra instructions" hint="Appended to the built-in prompt, for new chats.">
          <textarea className="sd-input" rows={5} value={draft.systemPrompt} onChange={text("systemPrompt")} />
        </Field>
      </Section>
    </div>
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
