import type { ReactNode } from "react";

import { cn } from "@/lib/utils";

/** A small industrial bolt for a container's corners. */
export function Bolt({ className }: { className?: string }) {
  return (
    <span
      aria-hidden="true"
      className={cn(
        "pointer-events-none relative flex h-3 w-3 items-center justify-center rounded-full border border-white/10 bg-gradient-to-br from-slate-700 via-slate-900 to-black shadow-[inset_0_1px_1px_rgba(255,255,255,0.05),0_1px_3px_rgba(0,0,0,0.6)]",
        className,
      )}
    >
      <span className="absolute h-[60%] w-[1.5px] rotate-45 bg-slate-800" />
      <span className="absolute h-[60%] w-[1.5px] -rotate-45 bg-slate-800" />
    </span>
  );
}

/** A panel held by four bolts. */
export function BoltedPanel({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div className={cn("panel relative rounded-2xl", className)}>
      <Bolt className="absolute top-3 left-3" />
      <Bolt className="absolute top-3 right-3" />
      <Bolt className="absolute bottom-3 left-3" />
      <Bolt className="absolute right-3 bottom-3" />
      {children}
    </div>
  );
}

/** The micro-label: an instrument's eyebrow. */
export function MicroLabel({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <span className={cn("text-[10px] font-black tracking-[0.3em] text-violet-300/80 uppercase", className)}>
      {children}
    </span>
  );
}

/**
 * The reticle: four corner ticks around a specimen or a figure, the instrument's frame.
 * Decorative only; the parent must be positioned.
 */
export function Reticle({ className, inset = 10, size = 14 }: { className?: string; inset?: number; size?: number }) {
  const corner = "absolute border-[color:var(--reticle,rgba(221,214,254,0.45))]";
  const box = { width: size, height: size };
  return (
    <span aria-hidden="true" className={cn("pointer-events-none absolute inset-0 z-10", className)}>
      <span className={cn(corner, "border-t border-l")} style={{ ...box, top: inset, left: inset }} />
      <span className={cn(corner, "border-t border-r")} style={{ ...box, top: inset, right: inset }} />
      <span className={cn(corner, "border-b border-l")} style={{ ...box, bottom: inset, left: inset }} />
      <span className={cn(corner, "border-b border-r")} style={{ ...box, bottom: inset, right: inset }} />
    </span>
  );
}

/**
 * A heading whose `[bracketed]` words are the one violet word: solid, or, with
 * `lit`, the hero's one gradient.
 */
export function RecallTitle({ text, lit = false }: { text: string; lit?: boolean }) {
  const parts = text.split(/(\[[^\]]+\])/);
  return (
    <>
      {parts.map((part, index) =>
        part.startsWith("[") && part.endsWith("]") ? (
          <span key={index} className={lit ? "recall-gradient" : "recall-word"}>
            {part.slice(1, -1)}
          </span>
        ) : (
          <span key={index}>{part}</span>
        ),
      )}
    </>
  );
}

/** An inline command or identifier in running text. */
export function Cmd({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <code
      className={cn(
        "rounded-md border border-white/[0.08] bg-white/[0.04] px-1.5 py-0.5 font-mono text-[0.86em] text-violet-200",
        className,
      )}
    >
      {children}
    </code>
  );
}
