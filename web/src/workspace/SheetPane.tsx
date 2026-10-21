// The shared workbook: GridCraft in a frame, live for every client, the agent and the widgets.
import { useState } from "react";
import type { ReactNode } from "react";

import { Badge, Button, Dialog, Panel } from "../design/index.js";
import { SHEET_URL, type Sheet } from "../sheet/useSheet.js";

function originLabel(origin: string): string {
  if (origin === "agent") return "agent";
  if (origin.startsWith("widget:")) return `widget ${origin.slice(7)}`;
  if (origin.startsWith("client:")) return "another tab";
  return origin;
}

function SyncBadge({ sheet }: { sheet: Sheet }) {
  if (sheet.status === "failed") return <Badge tone="critical">Failed</Badge>;
  if (sheet.status === "loading") return <Badge tone="muted">Loading</Badge>;
  return sheet.connected ? <Badge tone="success">Live</Badge> : <Badge tone="warning">Reconnecting</Badge>;
}

export interface SheetPaneProps {
  sheet: Sheet;
  /** In the page: move the sheet into a window of its own. */
  onUndock?(): void;
  /** In the sheet window: put it back into the page. */
  onDock?(): void;
  /** The header button that folds the sheet into the app bar. */
  minimize?: ReactNode;
  /** The corner handle that resizes the sheet. */
  grip?: ReactNode;
  /** A height the user chose; unset, the sheet fills its column. */
  height?: number;
  hidden?: boolean;
}

function Aside({ sheet, onUndock, onDock, minimize }: SheetPaneProps) {
  return (
    <span className="sheet-pane__aside">
      <SyncBadge sheet={sheet} />
      {onUndock && <Button size="sm" variant="ghost" onClick={onUndock}>Undock ↗</Button>}
      {onDock && <Button size="sm" variant="ghost" onClick={onDock}>Dock ↙</Button>}
      {minimize}
    </span>
  );
}

export function SheetPane({ sheet, onUndock, onDock, minimize, grip, height, hidden }: SheetPaneProps) {
  const { stats } = sheet;
  const [confirming, setConfirming] = useState(false);
  const reset = () => {
    setConfirming(false);
    void sheet.reset();
  };
  return (
    <Panel
      title="Sheet"
      className={height === undefined ? "sheet-pane" : "sheet-pane sheet-pane--sized"}
      style={height === undefined ? undefined : { height }}
      hidden={hidden}
      aside={<Aside sheet={sheet} onUndock={onUndock} onDock={onDock} minimize={minimize} />}
    >
      <iframe key={sheet.frameKey} ref={sheet.frameRef} className="sheet-frame" src={SHEET_URL} title="Spreadsheet (GridCraft)" />
      {sheet.status === "failed" && <p className="error">The spreadsheet could not start: {sheet.error}</p>}
      <footer className="sheet-pane__footer">
        <span className="muted">
          {stats.seen} ops from the server · {stats.sent} sent
          {stats.lastOrigin && <> · last change by {originLabel(stats.lastOrigin)}</>}
        </span>
        {stats.rejected.length > 0 && (
          <span className="error" title={stats.rejected.join("\n")}>{stats.rejected.length} refused by the server</span>
        )}
        <Button size="sm" variant="ghost" onClick={() => setConfirming(true)}>Clear sheet</Button>
      </footer>
      <Dialog
        open={confirming}
        title="Clear the sheet?"
        onClose={() => setConfirming(false)}
        actions={
          <>
            <Button onClick={() => setConfirming(false)}>Keep it</Button>
            <Button variant="primary" onClick={reset}>Clear for everyone</Button>
          </>
        }
      >
        <p>Every open tab gets a blank workbook. Running widgets keep writing their cells; stop them first for a clean slate.</p>
      </Dialog>
      {grip}
    </Panel>
  );
}
