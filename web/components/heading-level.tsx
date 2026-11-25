"use client";

// Heading levels that follow the page. The home page nests a chapter (h2), its sections (h3), a figure's title (h4)
// and a figure's own sub-heading (h5) under the hero's h1. A chapter's own page makes the chapter title the h1, so
// everything inside it moves up one level; without that the page would jump from h1 to h3. Components name their
// home-page level and this context shifts it, so no call site needs to know which page it is on.

import { createContext, useContext, type ComponentPropsWithoutRef, type ReactNode } from "react";

type Level = 2 | 3 | 4 | 5 | 6;
type Tag = "h1" | "h2" | "h3" | "h4" | "h5" | "h6";

const Shift = createContext(0);

/** Moves every `Heading` inside it up by `by` levels (1 on a chapter's own page, 0 on the home page). */
export function HeadingShift({ by, children }: { by: number; children: ReactNode }) {
  return <Shift.Provider value={by}>{children}</Shift.Provider>;
}

/** A heading at `level` as the home page nests it, moved up by the enclosing `HeadingShift`. */
export function Heading({ level, ...props }: { level: Level } & ComponentPropsWithoutRef<"h2">) {
  const shift = useContext(Shift);
  const Tag = `h${Math.min(6, Math.max(1, level - shift))}` as Tag;
  return <Tag {...props} />;
}
