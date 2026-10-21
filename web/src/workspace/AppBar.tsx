// The bar above the workspace: one button per panel. A panel's "–" minimizes it into the bar;
// its button here brings it back. While the sheet is undocked, its button docks it again.
import { Button, cx } from "../design/index.js";
import { PANELS, type Minimized, type PanelId } from "./panels.js";

export interface AppBarProps {
  minimized: Minimized;
  undocked: boolean;
  undockError: string;
  onDock(): void;
}

function hint(id: PanelId, shown: boolean, undocked: boolean): string {
  if (id === "sheet" && undocked) return "The sheet is in its own window: dock it here";
  return shown ? "Minimize to the bar" : "Show again";
}

export function AppBar({ minimized, undocked, undockError, onDock }: AppBarProps) {
  return (
    <nav className="appbar" aria-label="Workspace panels">
      <span className="appbar__title">Panels</span>
      <ul className="appbar__items">
        {PANELS.map(({ id, label }) => {
          const away = id === "sheet" && undocked;
          const shown = !away && !minimized.has(id);
          return (
            <li key={id}>
              <button
                type="button"
                className={cx("sd-frame appbar__item", !shown && "appbar__item--away")}
                aria-pressed={shown}
                title={hint(id, shown, undocked)}
                onClick={() => (away ? onDock() : minimized.toggle(id))}
              >
                <span className="appbar__dot" aria-hidden="true" />
                {label}
                {away && <span aria-hidden="true"> ↗</span>}
              </button>
            </li>
          );
        })}
      </ul>
      {undockError && <span className="error appbar__error">{undockError}</span>}
      {minimized.count > 0 && (
        <Button size="sm" variant="ghost" className="appbar__restore" onClick={minimized.restoreAll}>
          Show all
        </Button>
      )}
    </nav>
  );
}

/** The "–" in a panel's header. */
export function MinimizeButton({ label, onClick }: { label: string; onClick(): void }) {
  return (
    <Button size="sm" variant="ghost" className="minimize" aria-label={`Minimize ${label}`} title="Minimize to the bar" onClick={onClick}>
      –
    </Button>
  );
}
