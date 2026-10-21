// Drag handles. They report the total move since the drag started, so the owner sets
// "size at start + move" and never accumulates rounding or stale state.
//
// - Splitter: a vertical bar between two columns. Arrow keys move it by 24 px (Shift: 96 px).
// - ResizeGrip: a corner handle that resizes in both directions; arrow keys work in all four
//   directions, and a double click resets.
import { useRef } from "react";
import type { KeyboardEvent, PointerEvent } from "react";

interface DragHandlers {
  /** A drag starts; `handle` is the element dragged (its siblings are the columns it moves). */
  onStart(handle: HTMLElement): void;
  /** The pointer moved `dx`, `dy` px from where the drag started (right and down are positive). */
  onDrag(dx: number, dy: number): void;
  /** Dragging or not (frames must not swallow the pointer meanwhile). */
  onDragging?(dragging: boolean): void;
}

const ARROWS: Record<string, [number, number]> = {
  ArrowLeft: [-1, 0],
  ArrowRight: [1, 0],
  ArrowUp: [0, -1],
  ArrowDown: [0, 1],
};

function useDrag({ onStart, onDrag, onDragging }: DragHandlers, axes: "x" | "xy") {
  const origin = useRef<{ x: number; y: number } | null>(null);
  const end = () => {
    origin.current = null;
    onDragging?.(false);
  };
  return {
    onPointerDown: (e: PointerEvent<HTMLElement>) => {
      e.currentTarget.setPointerCapture(e.pointerId);
      origin.current = { x: e.clientX, y: e.clientY };
      onStart(e.currentTarget);
      onDragging?.(true);
    },
    onPointerMove: (e: PointerEvent<HTMLElement>) => {
      const o = origin.current;
      if (o) onDrag(e.clientX - o.x, axes === "xy" ? e.clientY - o.y : 0);
    },
    onPointerUp: end,
    onPointerCancel: end,
    onKeyDown: (e: KeyboardEvent<HTMLElement>) => {
      const dir = ARROWS[e.key];
      if (!dir || (axes === "x" && dir[0] === 0)) return;
      e.preventDefault();
      const step = e.shiftKey ? 96 : 24;
      onStart(e.currentTarget);
      onDrag(dir[0] * step, dir[1] * step);
    },
  };
}

export function Splitter({ label, ...handlers }: DragHandlers & { label: string }) {
  const drag = useDrag(handlers, "x");
  return <div className="splitter" role="separator" aria-orientation="vertical" aria-label={label} tabIndex={0} {...drag} />;
}

export function ResizeGrip({ label, onReset, ...handlers }: DragHandlers & { label: string; onReset(): void }) {
  const drag = useDrag(handlers, "xy");
  return (
    <div
      className="resize-grip"
      role="button"
      aria-label={label}
      title={`${label} (double-click resets)`}
      tabIndex={0}
      onDoubleClick={onReset}
      {...drag}
    />
  );
}
