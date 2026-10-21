// The undocked sheet: the whole window is the sheet. It follows the server on its own and serves
// the main window's calls (e.g. the metrics snapshot) over a BroadcastChannel.
import { useEffect, useRef } from "react";

import { SheetPane } from "./SheetPane.js";
import { serveCalls } from "../sheet/remote.js";
import { useSheet } from "../sheet/useSheet.js";

export function SheetWindow() {
  const sheet = useSheet();
  const { call, ready } = sheet;
  const readyRef = useRef(ready);
  useEffect(() => {
    readyRef.current = ready;
  }, [ready]);
  useEffect(() => serveCalls(call, () => readyRef.current), [call]);
  useEffect(() => {
    document.title = "agent demo · sheet";
    document.body.classList.add("is-sheet-window");
  }, []);
  // Closing the window docks the sheet in the page (it polls for that).
  return (
    <div className="sheet-window">
      <SheetPane sheet={sheet} onDock={() => window.close()} />
    </div>
  );
}
