"use client";

import { Pause, Play, RotateCcw, SkipForward } from "lucide-react";
import { type CSSProperties, useEffect, useRef, useState } from "react";

import { DemoButton, DemoFrame, Readout } from "@/components/explain/demo";
import { Cmd } from "@/components/instrument";
import { useInView, useReducedMotion } from "@/lib/hooks";
import { cn, seeded } from "@/lib/utils";

/**
 * Six sessions on one repository, two agents: one with no durable memory, one
 * that pulls an `ee pack` at the start of each session and records what it
 * learns. A toy: the proportions are chosen to tell the story, not measured.
 */

type Kind = "conventions" | "rereading" | "trap" | "warned" | "pack" | "useful";

const KIND: Record<Kind, { label: string; className: string; swatch: string }> = {
  conventions: { label: "Re-discovering conventions", className: "bg-slate-500/70", swatch: "bg-slate-500/70" },
  rereading: { label: "Re-reading the same files", className: "bg-slate-400/35", swatch: "bg-slate-400/35" },
  trap: {
    label: "Hitting a trap",
    className: "bg-rose-400/85 bg-[repeating-linear-gradient(135deg,transparent_0_5px,rgba(0,0,0,0.25)_5px_8px)]",
    swatch: "bg-rose-400/85",
  },
  warned: { label: "Recalled · preflight warned", className: "bg-sky-300", swatch: "bg-sky-300" },
  pack: { label: "ee pack · 4k-token budget", className: "bg-violet-200", swatch: "bg-violet-200" },
  useful: { label: "Useful work", className: "bg-violet-400", swatch: "bg-violet-400" },
};

const LEGEND: readonly Kind[] = ["useful", "conventions", "rereading", "trap", "pack", "warned"];

type Segment = { kind: Kind; share: number };

/** What each session holds. A trap is either the recurring one (first met in session 1) or the new one (session 3). */
const SESSIONS = [
  { trap: "new", noMemory: "Hits a trap: the migration test needs a fresh DB", withEe: "Hits it too, then records it as a rule", learned: 2 },
  { trap: "known", noMemory: "Hits the migration trap again", withEe: "The rule is in the pack, so it avoids the trap", learned: 0 },
  { trap: "new", noMemory: "Hits a trap: tagging before the changelog lands", withEe: "Hits a new trap that no memory covers yet", learned: 1 },
  { trap: "known", noMemory: "Hits the migration trap again", withEe: "Preflight recalls the rule before the test", learned: 0 },
  { trap: "known", noMemory: "Hits the tagging trap again", withEe: "Preflight on git tag recalls the lesson", learned: 1 },
  { trap: "known", noMemory: "Hits the migration trap again", withEe: "Rule recalled; most of the session goes to the task", learned: 0 },
] as const;

const random = seeded(41);
const jitter = SESSIONS.map(() => (random() - 0.5) * 0.06);

/** A session's time, in shares that sum to one. Session 1 is the same for both agents: there is nothing to recall yet. */
function segments(session: number, withEe: boolean): Segment[] {
  const wobble = jitter[session] ?? 0;
  const conventions = 0.2 + wobble;
  const rereading = 0.21 - wobble / 2;
  const trap = 0.17;
  if (!withEe || session === 0) {
    return [
      { kind: "conventions", share: conventions },
      { kind: "rereading", share: rereading },
      { kind: "trap", share: trap },
      { kind: "useful", share: 1 - conventions - rereading - trap },
    ];
  }
  // Memory shrinks the re-discovery, never to nothing: a pack is still read, and a session still orients.
  const fade = Math.max(0.35, 1 - session * 0.14);
  const known = SESSIONS[session]?.trap === "known";
  const parts: Segment[] = [
    { kind: "pack", share: 0.035 },
    { kind: "conventions", share: 0.07 * fade },
    { kind: "rereading", share: 0.09 * fade },
    known ? { kind: "warned", share: 0.025 } : { kind: "trap", share: trap },
  ];
  const used = parts.reduce((sum, part) => sum + part.share, 0);
  return [...parts, { kind: "useful", share: 1 - used }];
}

const share = (parts: Segment[], kind: Kind) => parts.filter((part) => part.kind === kind).reduce((s, p) => s + p.share, 0);

const STEP_MS = 1150;
const percent = (value: number) => `${Math.round(value * 100)}%`;

export function Amnesia() {
  const frame = useRef<HTMLDivElement>(null);
  const reduced = useReducedMotion();
  const inView = useInView(frame);
  // The HTML (and a reader who asks for less motion) sees all six sessions; the first time the figure comes into
  // view it starts again from session one and plays.
  const [shown, setShown] = useState<number>(SESSIONS.length);
  const [playing, setPlaying] = useState(false);
  const started = useRef(false);

  useEffect(() => {
    if (!inView || reduced || started.current) return;
    started.current = true;
    setShown(1);
    setPlaying(true);
  }, [inView, reduced]);

  useEffect(() => {
    if (!playing || reduced || !inView) return;
    if (shown >= SESSIONS.length) {
      setPlaying(false);
      return;
    }
    const handle = window.setTimeout(() => setShown((count) => Math.min(SESSIONS.length, count + 1)), STEP_MS);
    return () => window.clearTimeout(handle);
  }, [playing, reduced, inView, shown]);

  const visible = SESSIONS.slice(0, shown);
  const noMemory = visible.map((_, index) => segments(index, false));
  const withEe = visible.map((_, index) => segments(index, true));
  const useful = (lanes: Segment[][]) =>
    lanes.length === 0 ? 0 : lanes.reduce((sum, parts) => sum + share(parts, "useful"), 0) / lanes.length;
  const traps = (lanes: Segment[][]) => lanes.filter((parts) => share(parts, "trap") > 0).length;
  const lessons = visible.reduce((sum, session) => sum + session.learned, 0);
  const done = shown >= SESSIONS.length;

  return (
    <DemoFrame
      eyebrow="Six sessions, two agents"
      title="The same repository, with and without a memory."
      basis="toy"
      source={
        <>
          README.md · Why This Exists (&ldquo;Coding agents forget&rdquo;) · the operating loop: ee pack at the start, ee remember
          and ee outcome as lessons are learned, ee preflight check before a risky command (advisory; it never blocks).
          The proportions are invented for this page and are not measured.
        </>
      }
    >
      <div ref={frame}>
        {/* Legend */}
        <ul className="flex flex-wrap gap-x-4 gap-y-2 text-[11px] text-slate-400">
          {LEGEND.map((kind) => (
            <li key={kind} className="flex items-center gap-2">
              <span aria-hidden="true" className={cn("h-2.5 w-4 rounded-[3px]", KIND[kind].swatch)} />
              {KIND[kind].label}
            </li>
          ))}
        </ul>

        <div className="mt-6 grid grid-cols-1 gap-6 lg:grid-cols-[minmax(0,1fr)_15rem]">
          <ol className="ruled min-w-0">
            {SESSIONS.map((session, index) => {
              const on = index < shown;
              return (
                <li
                  key={index}
                  className={cn(
                    "grid grid-cols-[2.6rem_minmax(0,1fr)] gap-x-3 py-3 transition-opacity duration-500 sm:grid-cols-[3rem_minmax(0,1fr)]",
                    on ? "opacity-100" : "opacity-25",
                  )}
                >
                  <div className="pt-0.5">
                    <p className="font-mono text-[10px] tracking-[0.18em] text-slate-500 uppercase">Sess</p>
                    <p className="font-mono text-lg leading-none font-medium text-white tabular-nums">{String(index + 1).padStart(2, "0")}</p>
                  </div>
                  <div className="min-w-0 space-y-2.5">
                    <Lane
                      label="No memory"
                      parts={segments(index, false)}
                      note={session.noMemory}
                      on={on}
                      tone="rose"
                    />
                    <Lane label="With ee" parts={segments(index, true)} note={session.withEe} on={on} tone="chapter" />
                  </div>
                </li>
              );
            })}
          </ol>

          <div className="space-y-3">
            <div className="grid grid-cols-2 gap-2 lg:grid-cols-1">
              <Readout
                label="Useful work · no memory"
                value={percent(useful(noMemory))}
                note={`${traps(noMemory)} trap${traps(noMemory) === 1 ? "" : "s"} hit in ${shown} session${shown === 1 ? "" : "s"}`}
                tone="wrong"
              />
              <Readout
                label="Useful work · with ee"
                value={percent(useful(withEe))}
                note={`${traps(withEe)} trap${traps(withEe) === 1 ? "" : "s"} hit: only the new ones`}
                tone="rule"
              />
            </div>
            <div className="rounded-xl border border-white/10 bg-black/30 px-3 py-2.5">
              <p className="text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase">In memory</p>
              <p className="mt-1 font-mono text-base text-white tabular-nums sm:text-lg">
                {lessons} lesson{lessons === 1 ? "" : "s"}
              </p>
              <p className="mt-1 text-[11px] leading-snug text-slate-500">
                Each is written once with <Cmd>ee remember</Cmd> and can be packed in any later session.
              </p>
            </div>
            <div className="flex flex-wrap gap-2">
              {/* The label says what a press does, so no aria-pressed as well: "Pause, pressed" contradicts itself. */}
              <DemoButton
                onClick={() => {
                  // Nothing animates for a reader who asks for less motion, so Play shows every session at once
                  // (restarting from session one would leave the figure stuck there).
                  if (reduced) {
                    setPlaying(false);
                    setShown(SESSIONS.length);
                    return;
                  }
                  if (done) setShown(1);
                  setPlaying((on) => !on || done);
                }}
                label={playing && !reduced ? "Pause the sessions" : "Play the sessions"}
                className={cn(playing && !reduced && "border-violet-400/60 text-violet-200")}
              >
                {playing && !reduced ? <Pause className="h-3.5 w-3.5" /> : <Play className="h-3.5 w-3.5" />}
                {playing && !reduced ? "Pause" : "Play"}
              </DemoButton>
              <DemoButton
                onClick={() => {
                  setPlaying(false);
                  setShown((count) => Math.min(SESSIONS.length, count + 1));
                }}
                disabled={done}
                label="Show the next session"
              >
                <SkipForward className="h-3.5 w-3.5" /> Step
              </DemoButton>
              <DemoButton
                onClick={() => {
                  setShown(1);
                  setPlaying(!reduced);
                }}
                label="Replay from session one"
              >
                <RotateCcw className="h-3.5 w-3.5" /> Replay
              </DemoButton>
            </div>
          </div>
        </div>
      </div>
    </DemoFrame>
  );
}

function Lane({
  label,
  parts,
  note,
  on,
  tone,
}: {
  label: string;
  parts: Segment[];
  note: string;
  on: boolean;
  tone: "rose" | "chapter";
}) {
  const description = parts.map((part) => `${KIND[part.kind].label} ${percent(part.share)}`).join(", ");
  return (
    <div className="grid grid-cols-1 gap-x-4 gap-y-1 sm:grid-cols-[5.5rem_minmax(0,1fr)] md:grid-cols-[5.5rem_minmax(0,1fr)_minmax(0,15rem)] md:items-center">
      <p
        className={cn(
          "font-mono text-[10px] tracking-[0.16em] uppercase",
          tone === "rose" ? "text-rose-300/80" : "text-violet-300",
        )}
      >
        {label}
      </p>
      <div
        role="img"
        aria-label={`${label}: ${description}`}
        className="flex h-3.5 min-w-0 overflow-hidden rounded-full bg-white/[0.04] ring-1 ring-white/[0.06]"
      >
        {parts.map((part, index) => (
          <span
            key={`${part.kind}-${index}`}
            className={cn(
              "h-full origin-left transition-transform duration-700 ease-[cubic-bezier(0.19,1,0.22,1)]",
              index > 0 && "border-l border-[#05040d]/80",
              KIND[part.kind].className,
            )}
            style={
              {
                width: `${part.share * 100}%`,
                transform: on ? "scaleX(1)" : "scaleX(0)",
                transitionDelay: on ? `${index * 90}ms` : "0ms",
              } as CSSProperties
            }
          />
        ))}
      </div>
      <p className="min-w-0 text-[12px] leading-snug text-slate-400 sm:col-start-2 md:col-start-auto">{note}</p>
    </div>
  );
}
