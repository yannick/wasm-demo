// A vertical drag handle between two columns. It reports the total horizontal move since the drag
// started, so the owner sets "width at start + move" and never accumulates rounding or stale
// state. Arrow keys move it by 24 px (Shift: 96 px).
import { useRef } from "react";

export interface SplitterProps {
  label: string;
  /** A drag starts: remember the widths it moves from. */
  onStart(): void;
  /** The pointer moved `dx` px from where the drag started (right is positive). */
  onDrag(dx: number): void;
  /** Dragging or not (frames must not swallow the pointer meanwhile). */
  onDragging?(dragging: boolean): void;
}

export function Splitter({ label, onStart, onDrag, onDragging }: SplitterProps) {
  const origin = useRef<number | null>(null);
  const end = () => {
    origin.current = null;
    onDragging?.(false);
  };
  return (
    <div
      className="splitter"
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      tabIndex={0}
      onPointerDown={(e) => {
        e.currentTarget.setPointerCapture(e.pointerId);
        origin.current = e.clientX;
        onStart();
        onDragging?.(true);
      }}
      onPointerMove={(e) => {
        if (origin.current !== null) onDrag(e.clientX - origin.current);
      }}
      onPointerUp={end}
      onPointerCancel={end}
      onKeyDown={(e) => {
        const step = e.shiftKey ? 96 : 24;
        if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
        e.preventDefault();
        onStart();
        onDrag(e.key === "ArrowLeft" ? -step : step);
      }}
    />
  );
}
