// The LLM settings (GetSettings / SaveSettings), shared by the chat and the settings dialog.
import { useCallback, useEffect, useState } from "react";

import { agentClient } from "../api/client.js";
import type { LlmSettings } from "../gen/agentdemo/v1/agent_pb.js";

export function useSettings(): { settings: LlmSettings | null; reload(): void; saved(s: LlmSettings): void } {
  const [settings, setSettings] = useState<LlmSettings | null>(null);
  const reload = useCallback(() => {
    agentClient
      .getSettings({})
      .then((res) => setSettings(res.settings ?? null))
      .catch((err: unknown) => console.warn("GetSettings failed", err));
  }, []);
  useEffect(reload, [reload]);
  return { settings, reload, saved: setSettings };
}
