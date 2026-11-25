"use client";

import { Fragment, type KeyboardEvent, type ReactNode, useRef } from "react";

import { Heading } from "@/components/heading-level";
import { MicroLabel, Reticle } from "@/components/instrument";
import { cn } from "@/lib/utils";

/**
 * What a figure is, said on its face: a picture of a rule is never presented as a run.
 * - `rule`: the page computes ee's own formula, with the constants the source declares, on small
 *   numbers. Not ee's code, but the same arithmetic; the caption names the file.
 * - `derived`: arithmetic from stated assumptions, not a measurement.
 * - `record`: figures the repository records (a test count, a changelog, a measured run).
 * - `diagram`: a schematic of how the software is organised.
 * - `toy`: a small model written for this page with ee's kinds of rules at toy scale. Not ee,
 *   and nothing it shows is a result.
 */
export type Basis = "rule" | "derived" | "record" | "diagram" | "toy";

export const BASIS: Record<Basis, { label: string; className: string }> = {
  rule: {
    label: "ee's rule, computed here",
    className: "text-cyan-200 border-cyan-400/30 bg-cyan-400/10",
  },
  derived: {
    label: "Derived · arithmetic, not a benchmark",
    className: "text-amber-200 border-amber-400/30 bg-amber-400/10",
  },
  record: {
    label: "From the record",
    className: "text-violet-200 border-violet-400/35 bg-violet-400/10",
  },
  diagram: {
    label: "Diagram · not program output",
    className: "text-slate-300 border-white/15 bg-white/5",
  },
  toy: {
    label: "A toy in your browser · not ee",
    className: "text-fuchsia-200 border-fuchsia-300/35 bg-fuchsia-300/10",
  },
};

export function BasisBadge({ basis, className }: { basis: Basis; className?: string }) {
  const style = BASIS[basis];
  // A badge breaks only after a " · ", and only where one line will not fit (a 320 px phone): each part stays whole.
  const parts = style.label.split(" · ");
  return (
    <span
      className={cn(
        "inline-block max-w-full rounded-full border px-2.5 py-0.5 text-[10px] font-black tracking-[0.12em] uppercase",
        style.className,
        className,
      )}
    >
      {parts.map((part, index) => (
        <Fragment key={part}>
          {index > 0 ? " " : null}
          <span className="whitespace-nowrap">
            {part}
            {index < parts.length - 1 ? " ·" : null}
          </span>
        </Fragment>
      ))}
    </span>
  );
}

/** An explainer's frame: eyebrow, title, what kind of figure it is, the figure, and where its numbers come from. */
export function DemoFrame({
  eyebrow,
  title,
  basis,
  source,
  children,
  className,
}: {
  eyebrow: string;
  title: ReactNode;
  basis: Basis;
  /** Where the rule or the numbers are stated: "src/core/bayes.rs · DEFAULT_HARMFUL_WEIGHT". */
  source?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <figure className={cn("panel relative min-w-0 rounded-[1.75rem] p-5 sm:p-8", className)}>
      <Reticle inset={9} size={12} />
      <div className="flex flex-wrap items-start justify-between gap-x-4 gap-y-3">
        <div className="min-w-0 space-y-2.5">
          <MicroLabel className="text-[color:var(--chapter,#a78bfa)]">{eyebrow}</MicroLabel>
          <Heading level={4} className="text-xl font-extrabold tracking-[-0.025em] text-white sm:text-2xl">
            {title}
          </Heading>
        </div>
        <BasisBadge basis={basis} />
      </div>
      <div className="mt-6">{children}</div>
      {source ? (
        <figcaption className="mt-6 flex gap-3 border-t border-white/[0.06] pt-4 font-mono text-[11px] leading-relaxed [overflow-wrap:anywhere] text-slate-500">
          <span aria-hidden="true" className="mt-[3px] h-2.5 w-2.5 shrink-0 rounded-full border border-white/20" />
          <span className="min-w-0">{source}</span>
        </figcaption>
      ) : null}
    </figure>
  );
}

export type Tone = "rule" | "wrong" | "neutral";

const TONE_ON: Record<Tone, string> = {
  rule: "bg-violet-400 text-black",
  wrong: "bg-rose-400 text-black",
  neutral: "bg-white/15 text-white",
};

/** Where each key moves the selection in a radio group of `count` options, or null for a key the group ignores. */
export function radioKeyTarget(key: string, current: number, count: number): number | null {
  if (count === 0) return null;
  switch (key) {
    case "ArrowRight":
    case "ArrowDown":
      return (current + 1) % count;
    case "ArrowLeft":
    case "ArrowUp":
      return (current - 1 + count) % count;
    case "Home":
      return 0;
    case "End":
      return count - 1;
    default:
      return null;
  }
}

/**
 * A small radio group of buttons: one choice of a few. It follows the ARIA radio-group keyboard pattern: only the
 * checked option is in the tab order (the first, if none is checked), and the arrow keys, Home and End move the focus
 * and the selection together.
 */
export function Segmented<T extends string>({
  label,
  options,
  value,
  onChange,
  className,
}: {
  label: string;
  options: readonly { value: T; label: ReactNode; tone?: Tone }[];
  value: T;
  onChange: (value: T) => void;
  className?: string;
}) {
  const group = useRef<HTMLDivElement>(null);
  const checked = options.findIndex((option) => option.value === value);
  const onKeyDown = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    // A modified arrow is the browser's or the system's (Alt+Left goes back a page), not the group's.
    if (event.altKey || event.ctrlKey || event.metaKey) return;
    const target = radioKeyTarget(event.key, index, options.length);
    const option = target === null ? undefined : options[target];
    if (target === null || !option) return;
    event.preventDefault();
    group.current?.querySelectorAll<HTMLButtonElement>('[role="radio"]')[target]?.focus();
    if (option.value !== value) onChange(option.value);
  };
  return (
    <div
      ref={group}
      role="radiogroup"
      aria-label={label}
      className={cn("inline-flex max-w-full flex-wrap gap-1 rounded-xl border border-white/10 bg-black/30 p-1", className)}
    >
      {options.map((option, index) => {
        const on = option.value === value;
        return (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={on}
            tabIndex={on || (checked < 0 && index === 0) ? 0 : -1}
            onClick={() => onChange(option.value)}
            onKeyDown={(event) => onKeyDown(event, index)}
            className={cn(
              "rounded-lg px-3 py-1.5 text-xs font-bold transition focus-visible:ring-2 focus-visible:ring-violet-400 focus-visible:outline-none",
              on ? TONE_ON[option.tone ?? "neutral"] : "text-slate-300 hover:bg-white/5 hover:text-white",
            )}
          >
            {option.label}
          </button>
        );
      })}
    </div>
  );
}

/** A labelled range input with its value shown. */
export function Slider({
  label,
  value,
  min,
  max,
  step = 1,
  onChange,
  display,
  valueText,
  className,
}: {
  label: ReactNode;
  value: number;
  min: number;
  max: number;
  step?: number;
  onChange: (value: number) => void;
  display?: ReactNode;
  /** What a screen reader announces for the value (aria-valuetext), when the bare number would mean nothing. */
  valueText?: string;
  className?: string;
}) {
  return (
    <label className={cn("block min-w-0", className)}>
      <span className="flex items-baseline justify-between gap-3">
        <span className="text-xs text-slate-400">{label}</span>
        <span className="font-mono text-xs text-white tabular-nums">{display ?? value}</span>
      </span>
      <input
        type="range"
        className="demo-range mt-2 w-full"
        min={min}
        max={max}
        step={step}
        value={value}
        aria-valuetext={valueText}
        onChange={(event) => onChange(Number(event.target.value))}
      />
    </label>
  );
}

const READOUT_TONE: Record<Tone, string> = {
  rule: "text-violet-200",
  wrong: "text-rose-300",
  neutral: "text-white",
};

/** One figure with its label: an instrument's readout. */
export function Readout({
  label,
  value,
  note,
  tone = "neutral",
  className,
}: {
  label: ReactNode;
  value: ReactNode;
  note?: ReactNode;
  tone?: Tone;
  className?: string;
}) {
  return (
    <div className={cn("min-w-0 rounded-xl border border-white/10 bg-black/30 px-3 py-2.5", className)}>
      <p className="text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase">{label}</p>
      <p className={cn("mt-1 font-mono text-base tabular-nums sm:text-lg", READOUT_TONE[tone])}>{value}</p>
      {note ? <p className="mt-1 text-[11px] leading-snug text-slate-500">{note}</p> : null}
    </div>
  );
}

/** A plain action button in the explainers' style. */
export function DemoButton({
  children,
  onClick,
  disabled,
  pressed,
  className,
  label,
}: {
  children: ReactNode;
  onClick: () => void;
  disabled?: boolean;
  pressed?: boolean;
  className?: string;
  label?: string;
}) {
  return (
    // aria-disabled, not disabled: a button that disables itself when pressed keeps the keyboard's focus.
    <button
      type="button"
      onClick={disabled ? undefined : onClick}
      aria-disabled={disabled || undefined}
      aria-pressed={pressed}
      aria-label={label}
      className={cn(
        "inline-flex items-center gap-2 rounded-xl border border-white/15 bg-black/30 px-3.5 py-2 text-xs font-bold text-white transition hover:border-violet-400/50 hover:text-violet-200 focus-visible:ring-2 focus-visible:ring-violet-400 focus-visible:outline-none aria-disabled:pointer-events-none aria-disabled:opacity-40",
        pressed && "border-violet-400/60 text-violet-200",
        className,
      )}
    >
      {children}
    </button>
  );
}
