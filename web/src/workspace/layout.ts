// Layout helpers: widths the user dragged (kept per browser), media queries.
import { useCallback, useEffect, useState } from "react";

function read(key: string, fallback: number): number {
  try {
    const v = Number(localStorage.getItem(key));
    return Number.isFinite(v) && v > 0 ? v : fallback;
  } catch {
    return fallback;
  }
}

/** A number kept in localStorage (when available), clamped to [min, max]. */
export function usePersistedNumber(key: string, fallback: number, min: number, max: number): [number, (v: number) => void] {
  const [value, setValue] = useState(() => Math.min(max, Math.max(min, read(key, fallback))));
  const set = useCallback(
    (v: number) => {
      const next = Math.round(Math.min(max, Math.max(min, v)));
      setValue(next);
      try {
        localStorage.setItem(key, String(next));
      } catch {
        // Private mode: the width lasts as long as the page.
      }
    },
    [key, min, max],
  );
  return [value, set];
}

export function usePersistedFlag(key: string, fallback: boolean): [boolean, (v: boolean) => void] {
  const [value, setValue] = useState(() => {
    try {
      const v = localStorage.getItem(key);
      return v === null ? fallback : v === "1";
    } catch {
      return fallback;
    }
  });
  const set = useCallback(
    (v: boolean) => {
      setValue(v);
      try {
        localStorage.setItem(key, v ? "1" : "0");
      } catch {
        // Private mode.
      }
    },
    [key],
  );
  return [value, set];
}

export function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(() => window.matchMedia(query).matches);
  useEffect(() => {
    const mq = window.matchMedia(query);
    const onChange = () => setMatches(mq.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, [query]);
  return matches;
}

/** Viewport height left below the element (for a layout that fills the window). */
export function useHeightBelow(ref: React.RefObject<HTMLElement | null>, margin: number): number | undefined {
  const [height, setHeight] = useState<number>();
  useEffect(() => {
    const measure = () => {
      const top = ref.current?.getBoundingClientRect().top;
      if (top !== undefined) setHeight(Math.max(420, window.innerHeight - (top + window.scrollY) - margin));
    };
    measure();
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, [ref, margin]);
  return height;
}
