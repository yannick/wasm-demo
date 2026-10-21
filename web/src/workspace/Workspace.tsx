// The workspace: chat with the agent, the shared sheet, and the machine and its widgets, in
// columns the user can resize (kept per browser). The sheet can be undocked into its own window;
// the chat then takes its place.
import { useRef, useState } from "react";

import { Chat } from "./Chat.js";
import { MachinePanel } from "./MachinePanel.js";
import { SheetPane } from "./SheetPane.js";
import { Splitter } from "./Splitter.js";
import { WidgetsPanel } from "./WidgetsPanel.js";
import { useHeightBelow, useMediaQuery, usePersistedNumber } from "./layout.js";
import { useUndock } from "./undock.js";
import { useSheet, type SheetCaller } from "../sheet/useSheet.js";
import { remoteCall } from "../sheet/remote.js";
import { cx } from "../design/index.js";

/** Columns side by side from this width; stacked below it. */
const WIDE = "(min-width: 1100px)";

function useColumns() {
  const [chat, setChat] = usePersistedNumber("agent-demo.chat-width", 440, 300, 1400);
  const [side, setSide] = usePersistedNumber("agent-demo.side-width", 330, 260, 720);
  const start = useRef({ chat, side });
  return {
    chat,
    side,
    begin: () => (start.current = { chat, side }),
    dragChat: (dx: number) => setChat(start.current.chat + dx),
    dragSide: (dx: number) => setSide(start.current.side - dx),
  };
}

function template(docked: boolean, chat: number, side: number): string {
  return docked ? `${chat}px auto minmax(360px, 1fr) auto ${side}px` : `minmax(360px, 1fr) auto ${side}px`;
}

function Side({ undock, sheet }: { undock: ReturnType<typeof useUndock>; sheet: SheetCaller }) {
  return (
    <div className="workspace__side">
      {undock.undocked && <UndockedNote onDock={undock.dock} />}
      {undock.error && <p className="error">{undock.error}</p>}
      <MachinePanel sheet={sheet} />
      <WidgetsPanel />
    </div>
  );
}

export function Workspace({ hidden }: { hidden: boolean }) {
  const undock = useUndock();
  const docked = !undock.undocked;
  const sheet = useSheet(docked);
  const wide = useMediaQuery(WIDE);
  const cols = useColumns();
  const [dragging, setDragging] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const height = useHeightBelow(ref, 16);
  return (
    <div
      ref={ref}
      className={cx("workspace", wide && "workspace--columns", dragging && "is-resizing")}
      hidden={hidden}
      style={wide ? { gridTemplateColumns: template(docked, cols.chat, cols.side), height } : undefined}
    >
      <Chat />
      {docked && wide && <Splitter label="Chat width" onStart={cols.begin} onDrag={cols.dragChat} onDragging={setDragging} />}
      {docked && <SheetPane sheet={sheet} onUndock={undock.open} />}
      {wide && <Splitter label="Side panel width" onStart={cols.begin} onDrag={cols.dragSide} onDragging={setDragging} />}
      <Side undock={undock} sheet={docked ? sheet : { ready: true, call: remoteCall }} />
    </div>
  );
}

function UndockedNote({ onDock }: { onDock(): void }) {
  return (
    <div className="sd-frame undocked-note">
      <span>The sheet is in its own window.</span>
      <button type="button" className="sd-frame sd-button sd-button--sm" onClick={onDock}>Dock it here</button>
    </div>
  );
}
