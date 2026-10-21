// The workspace: chat with the agent, the shared sheet, and the machine and its widgets, in
// columns the user can resize (kept per browser). Any panel can be minimized into the app bar
// above; the others take its space. The sheet can also be resized at its corner (width and
// height) and undocked into its own window.
import { useRef, useState } from "react";
import type { CSSProperties } from "react";

import { AppBar, MinimizeButton } from "./AppBar.js";
import { Chat } from "./Chat.js";
import { MachinePanel } from "./MachinePanel.js";
import { SheetPane } from "./SheetPane.js";
import { ResizeGrip, Splitter } from "./Splitter.js";
import { WidgetsPanel } from "./WidgetsPanel.js";
import { template, useHeightBelow, useMediaQuery, usePersistedNumber, type Shown } from "./layout.js";
import { useMinimized, type Minimized } from "./panels.js";
import { useUndock } from "./undock.js";
import { useSheet, type Sheet, type SheetCaller } from "../sheet/useSheet.js";
import { remoteCall } from "../sheet/remote.js";
import { cx } from "../design/index.js";

/** Columns side by side from this width; stacked below it. */
const WIDE = "(min-width: 1100px)";
const SHEET_WIDTH = 720;
const SIDE_WIDTH = { fallback: 330, min: 260, max: 720 };
/** The sheet is never made shorter than this; a stored height of 0 means "fill the column". */
const SHEET_MIN_HEIGHT = 320;

function useSizes() {
  const [sheetWidth, setSheetWidth] = usePersistedNumber("agent-demo.sheet-width", SHEET_WIDTH, 360, 2400);
  const [sheetHeight, setSheetHeight] = usePersistedNumber("agent-demo.sheet-height", 0, 0, 4000);
  const [sideWidth, setSideWidth] = usePersistedNumber("agent-demo.side-width", SIDE_WIDTH.fallback, SIDE_WIDTH.min, SIDE_WIDTH.max);
  // Drags start from the rendered sizes: a stored width may be wider than the window allows.
  const start = useRef({ w: 0, h: 0, side: 0 });
  const measure = (el: Element | null | undefined) => {
    const r = el?.getBoundingClientRect();
    const side = document.querySelector(".workspace__side")?.getBoundingClientRect().width ?? 0;
    start.current = { w: r?.width ?? 0, h: r?.height ?? 0, side };
  };
  return {
    sheetWidth,
    sheetHeight,
    sideWidth,
    /** A splitter resizes the column after it. */
    beginColumn: (handle: HTMLElement) => measure(handle.nextElementSibling),
    dragSheetColumn: (dx: number) => setSheetWidth(start.current.w - dx),
    dragSideColumn: (dx: number) => setSideWidth(start.current.w - dx),
    beginSheet: (handle: HTMLElement) => measure(handle.closest(".sheet-pane")),
    /**
     * The corner follows the pointer: the sheet's left edge stays, so its width comes from (or
     * goes to) the side panel on its right, within the side panel's limits.
     */
    dragSheet: (dx: number, dy: number, width: SheetWidth) => {
      if (width !== "none") {
        const { w, side } = start.current;
        const move = Math.min(side - SIDE_WIDTH.min, Math.max(side - SIDE_WIDTH.max, dx));
        setSideWidth(side - move);
        if (width === "own") setSheetWidth(w + move);
      }
      if (dy !== 0) setSheetHeight(Math.max(SHEET_MIN_HEIGHT, start.current.h + dy));
    },
    resetSheet: () => {
      setSheetWidth(SHEET_WIDTH);
      setSheetHeight(0);
    },
  };
}

function Side({ minimized, sheet }: { minimized: Minimized; sheet: SheetCaller }) {
  return (
    <div className="workspace__side" hidden={minimized.has("machine") && minimized.has("widgets")}>
      <MachinePanel
        sheet={sheet}
        hidden={minimized.has("machine")}
        minimize={<MinimizeButton label="Machine" onClick={() => minimized.minimize("machine")} />}
      />
      <WidgetsPanel
        hidden={minimized.has("widgets")}
        minimize={<MinimizeButton label="Wasm widgets" onClick={() => minimized.minimize("widgets")} />}
      />
    </div>
  );
}

function shownPanels(minimized: Minimized, docked: boolean): Shown {
  return {
    chat: !minimized.has("chat"),
    sheet: docked && !minimized.has("sheet"),
    side: !minimized.has("machine") || !minimized.has("widgets"),
  };
}

/** What the snapshot button writes to: the undocked window, or the sheet here while it's drawn. */
function snapshotTarget(docked: boolean, shown: Shown, sheet: Sheet): SheetCaller {
  if (!docked) return { ready: true, call: remoteCall };
  // A minimized sheet is not drawn and may not answer: the button waits for it.
  return shown.sheet ? sheet : { ready: false, call: sheet.call };
}

interface SheetSlotProps {
  sheet: Sheet;
  shown: Shown;
  wide: boolean;
  sizes: ReturnType<typeof useSizes>;
  minimized: Minimized;
  onUndock(): void;
  onDragging(dragging: boolean): void;
}

/**
 * How the corner grip changes the sheet's width: "own" (its width, next to the chat), "fill" (it
 * fills the row, so only the side panel's width changes), or "none" (nothing on its right).
 */
type SheetWidth = "own" | "fill" | "none";

function sheetWidthMode(wide: boolean, shown: Shown): SheetWidth {
  if (!wide || !shown.side) return "none";
  return shown.chat ? "own" : "fill";
}

function SheetSlot({ sheet, shown, wide, sizes, minimized, onUndock, onDragging }: SheetSlotProps) {
  const width = sheetWidthMode(wide, shown);
  return (
    <SheetPane
      sheet={sheet}
      hidden={!shown.sheet}
      height={sizes.sheetHeight || undefined}
      onUndock={onUndock}
      minimize={<MinimizeButton label="Sheet" onClick={() => minimized.minimize("sheet")} />}
      grip={
        <ResizeGrip
          label={width === "none" ? "Resize the sheet's height" : "Resize the sheet"}
          onStart={sizes.beginSheet}
          onDrag={(dx, dy) => sizes.dragSheet(dx, dy, width)}
          onDragging={onDragging}
          onReset={sizes.resetSheet}
        />
      }
    />
  );
}

interface Layout {
  nothing: boolean;
  /** Splitter before the sheet; before the side panel. */
  splitSheet: boolean;
  splitSide: boolean;
  className: string;
  style: CSSProperties | undefined;
}

function layoutOf(shown: Shown, wide: boolean, dragging: boolean, sizes: ReturnType<typeof useSizes>, height: number | undefined): Layout {
  const main = shown.chat || shown.sheet;
  return {
    nothing: !main && !shown.side,
    splitSheet: wide && shown.chat && shown.sheet,
    splitSide: wide && shown.side && main,
    className: cx("workspace", wide && "workspace--columns", dragging && "is-resizing"),
    style: wide ? { gridTemplateColumns: template(shown, sizes.sheetWidth, sizes.sideWidth), height } : undefined,
  };
}

export function Workspace({ hidden }: { hidden: boolean }) {
  const undock = useUndock();
  const docked = !undock.undocked;
  const sheet = useSheet(docked);
  const minimized = useMinimized();
  const wide = useMediaQuery(WIDE);
  const sizes = useSizes();
  const [dragging, setDragging] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const height = useHeightBelow(ref, 16);
  const shown = shownPanels(minimized, docked);
  const layout = layoutOf(shown, wide, dragging, sizes, height);
  return (
    <div className="workspace-shell" hidden={hidden}>
      <AppBar minimized={minimized} undocked={undock.undocked} undockError={undock.error} onDock={undock.dock} />
      <div ref={ref} className={layout.className} style={layout.style}>
        {layout.nothing && <p className="muted workspace__empty">Every panel is minimized: bring one back from the bar above.</p>}
        <Chat hidden={!shown.chat} minimize={<MinimizeButton label="Chat" onClick={() => minimized.minimize("chat")} />} />
        {layout.splitSheet && <Splitter label="Sheet width" onStart={sizes.beginColumn} onDrag={sizes.dragSheetColumn} onDragging={setDragging} />}
        {docked && (
          <SheetSlot sheet={sheet} shown={shown} wide={wide} sizes={sizes} minimized={minimized} onUndock={undock.open} onDragging={setDragging} />
        )}
        {layout.splitSide && <Splitter label="Side panel width" onStart={sizes.beginColumn} onDrag={sizes.dragSideColumn} onDragging={setDragging} />}
        <Side minimized={minimized} sheet={snapshotTarget(docked, shown, sheet)} />
      </div>
    </div>
  );
}
