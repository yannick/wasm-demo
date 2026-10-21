// Undocking the sheet: it opens in a window of its own (the same page with `?window=sheet`),
// which syncs with the server on its own. Closing that window docks it again.
import { useCallback, useEffect, useRef, useState } from "react";

export const SHEET_WINDOW = "agent-demo-sheet";

export function isSheetWindow(): boolean {
  return new URLSearchParams(window.location.search).get("window") === "sheet";
}

export function useUndock(): { undocked: boolean; error: string; open(): void; dock(): void } {
  const [undocked, setUndocked] = useState(false);
  const [error, setError] = useState("");
  const popup = useRef<Window | null>(null);

  useEffect(() => {
    if (!undocked) return;
    // No event fires when another window closes: poll.
    const timer = setInterval(() => {
      if (popup.current?.closed ?? true) setUndocked(false);
    }, 500);
    return () => clearInterval(timer);
  }, [undocked]);

  useEffect(() => {
    const closePopup = () => popup.current?.close();
    window.addEventListener("pagehide", closePopup);
    return () => window.removeEventListener("pagehide", closePopup);
  }, []);

  const open = useCallback(() => {
    setError("");
    const w = Math.round(window.screen.availWidth * 0.6);
    const h = Math.round(window.screen.availHeight * 0.8);
    const win = window.open("/?window=sheet", SHEET_WINDOW, `popup,width=${w},height=${h}`);
    if (!win) {
      setError("The browser blocked the sheet window: allow pop-ups for this page.");
      return;
    }
    popup.current = win;
    setUndocked(true);
  }, []);

  const dock = useCallback(() => {
    popup.current?.close();
    popup.current = null;
    setUndocked(false);
  }, []);

  return { undocked, error, open, dock };
}
