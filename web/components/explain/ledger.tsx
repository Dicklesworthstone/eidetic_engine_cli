"use client";

import { Eye, Pause, Play, RotateCcw, Shuffle, SkipForward } from "lucide-react";
import { type ReactNode, useEffect, useId, useMemo, useRef, useState } from "react";

import { DemoButton, DemoFrame, Readout } from "@/components/explain/demo";
import {
  ATTEMPTS_PER_SESSION,
  cellX,
  cellY,
  COLS,
  DEFAULT_SEED,
  DISTILL_AT,
  FAMILY_NAMES,
  type Landscape,
  makeLandscape,
  type Mode,
  ROWS,
  runCampaign,
  type Session,
  SESSIONS,
  type Snapshot,
  snapshot,
  WIN_COUNT,
} from "@/components/explain/ledger-model";
import { useFigureWidth, useInView, useReducedMotion } from "@/lib/hooks";
import { GRAPH } from "@/lib/palette";
import { cn } from "@/lib/utils";

// A campaign of 24 sessions over one hidden landscape, searched twice by the same agent with the same taste
// for promising-looking ideas: once with nothing carried between sessions, once with a ledger in the shape of
// ee's. The landscape, the agent and the moment of distillation are invented (ledger-model.ts); the loop
// they act out (capture a failure with its family, group a family, distill it through curation, read it back
// in the next pack) is ee's, and the caption names where each piece lives.

const STEP_MS = 900;
const PROMISE = "#cbd5e1";
const FAILED = "#64748b";
const REPEAT = "#f43f5e";
const WIN = GRAPH;
const CHAPTER = "var(--chapter, #fb923c)";
const INK = "#05040d";
/** The page's muted tone (styles/site.tailwind raises slate-500 to #77839c for contrast on the ink). */
const MUTED = "#77839c";

const sessionLabel = (session: number | null) => (session === null ? "—" : `S${session}`);
const plural = (count: number, one: string, many = `${one}s`) => `${count} ${count === 1 ? one : many}`;

export function NegativeLedger() {
  const frame = useRef<HTMLDivElement>(null);
  const reduced = useReducedMotion();
  const inView = useInView(frame);
  const [seed, setSeed] = useState(DEFAULT_SEED);
  // The HTML (and a reader who asks for less motion) shows the whole campaign; the first time the figure
  // comes into view it starts again from session zero and plays.
  const [shown, setShown] = useState<number>(SESSIONS);
  const [playing, setPlaying] = useState(false);
  const [reveal, setReveal] = useState(false);
  const started = useRef(false);

  const landscape = useMemo(() => makeLandscape(seed), [seed]);
  const logs = useMemo(() => ({ none: runCampaign(landscape, "none"), ee: runCampaign(landscape, "ee") }), [landscape]);
  const none = useMemo(() => snapshot(landscape, logs.none, shown), [landscape, logs, shown]);
  const ee = useMemo(() => snapshot(landscape, logs.ee, shown), [landscape, logs, shown]);
  // The longest of the campaign's descriptions holds the paragraph's height, so the page below it stays put as it plays.
  const longest = useMemo(
    () =>
      Array.from({ length: SESSIONS + 1 }, (_, count) => describe(count, logs.none, logs.ee)).reduce((a, b) =>
        b.length > a.length ? b : a,
      ),
    [logs],
  );

  useEffect(() => {
    if (!inView || reduced || started.current) return;
    started.current = true;
    setShown(0);
    setPlaying(true);
  }, [inView, reduced]);

  useEffect(() => {
    if (!playing || reduced || !inView) return;
    if (shown >= SESSIONS) {
      setPlaying(false);
      return;
    }
    const handle = window.setTimeout(
      () => setShown((count) => Math.min(SESSIONS, count + 1)),
      shown === 0 ? STEP_MS / 2 : STEP_MS,
    );
    return () => window.clearTimeout(handle);
  }, [playing, reduced, inView, shown]);

  const done = shown >= SESSIONS;
  const animating = playing && !reduced;
  const latest = shown > 0 ? logs.ee[shown - 1] : undefined;

  return (
    <DemoFrame
      eyebrow="One campaign, two agents"
      title="The same search, with and without a failure ledger."
      basis="toy"
      source={
        <>
          README.md · Negative Evidence Ledger · src/models/memory.rs · FAILURE_TYPED_MEMORY_FIELDS: cause,
          regression_surface, reverted_at_sha, family (from &ldquo;Family:&rdquo; in the body, or --field family=…) ·
          src/cli/mod.rs · ee search &lt;query&gt; --kind failure --field family=…; attempt families are a separate ledger (ee remember
          --family --of-n --attempt --attempt-outcome, ee search --family) · src/core/rule.rs · PLAYBOOK_MIN_EVIDENCE = 3 ·
          src/core/outcome.rs · ANTI_PATTERN_PROPOSAL_THRESHOLD = 3, from at least two sources · src/core/curate.rs · an
          applied anti_pattern_proposal becomes a protected procedural rule · src/pack/mod.rs · include_anti_pattern_first
          (&ldquo;What NOT to do&rdquo;) · docs/adr/0006-procedural-memory-requires-evidence.md. The landscape, the agent
          and the automatic distillation are invented for this page.
        </>
      }
    >
      <div ref={frame}>
        <Legend />

        <div className="mt-5 grid grid-cols-1 gap-6 lg:grid-cols-2 lg:gap-5">
          <Panel mode="none" landscape={landscape} log={logs.none} snap={none} reveal={reveal} reduced={reduced} />
          <Panel mode="ee" landscape={landscape} log={logs.ee} snap={ee} reveal={reveal} reduced={reduced} />
        </div>

        <div className="mt-5 flex flex-col gap-3 md:flex-row md:items-center md:gap-5">
          <div className="flex flex-wrap gap-2">
            <DemoButton
              onClick={() => {
                if (reduced) {
                  setShown(SESSIONS);
                  return;
                }
                if (done) {
                  setShown(0);
                  setPlaying(true);
                  return;
                }
                setPlaying((on) => !on);
              }}
              // The label says what a press does, so no aria-pressed as well.
              className={cn(animating && "border-violet-400/60 text-violet-200")}
              disabled={reduced && done}
              label={animating ? "Pause the campaign" : reduced ? "Show all 24 sessions" : "Play the campaign"}
            >
              {animating ? <Pause className="h-3.5 w-3.5" /> : <Play className="h-3.5 w-3.5" />}
              {animating ? "Pause" : reduced ? "All 24" : done ? "Replay" : "Play"}
            </DemoButton>
            <DemoButton
              onClick={() => {
                setPlaying(false);
                setShown((count) => Math.min(SESSIONS, count + 1));
              }}
              disabled={done}
              label="Run the next session"
            >
              <SkipForward className="h-3.5 w-3.5" /> Step
            </DemoButton>
            <DemoButton
              onClick={() => {
                setPlaying(false);
                setShown(0);
              }}
              disabled={shown === 0}
              label="Back to session zero"
            >
              <RotateCcw className="h-3.5 w-3.5" /> Reset
            </DemoButton>
            <DemoButton
              onClick={() => {
                setSeed((current) => (current % 999) + 1);
                setShown(reduced ? SESSIONS : 0);
                setPlaying(!reduced);
              }}
              label="Draw another landscape"
            >
              <Shuffle className="h-3.5 w-3.5" /> New landscape
            </DemoButton>
            <DemoButton onClick={() => setReveal((on) => !on)} pressed={reveal} label="Show the hidden landscape">
              <Eye className="h-3.5 w-3.5" /> Reveal
            </DemoButton>
          </div>
          <Timeline log={logs.ee} shown={shown} seed={seed} reduced={reduced} />
        </div>

        {/* Quiet while it plays: a session every 900 ms would queue sentences faster than a screen reader speaks them. */}
        <div className="mt-3 grid min-h-[2.5rem] text-[13px] leading-relaxed text-slate-400">
          <p aria-hidden="true" className="invisible [grid-area:1/1]">
            {longest}
          </p>
          <p aria-live={animating ? "off" : "polite"} className="[grid-area:1/1]">
            {describe(shown, logs.none, logs.ee)}
          </p>
        </div>

        <div className="mt-5 grid grid-cols-1 gap-5 lg:grid-cols-2 lg:items-start">
          <FamilyLedger landscape={landscape} none={none} ee={ee} />
          <Loop latest={latest} />
        </div>

        <p className="mt-6 rounded-xl border border-white/[0.07] bg-black/20 px-4 py-3 text-[13px] leading-relaxed text-slate-400">
          <span className="font-semibold text-slate-200">What is invented, and what is ee&rsquo;s.</span> The landscape,
          the agent and its preference for promising-looking ideas are made up, and so is curation running by itself at
          a family&rsquo;s third failure. Nothing here is a measurement. The loop is ee&rsquo;s: a failure carries its
          family as a typed field, a search on that field returns the family&rsquo;s failures together, a rule drawn
          from them exists only after <span className="font-mono text-[12px] text-slate-300">ee curate apply</span> has
          accepted it, and the next pack puts what not to do first. The threshold of three is also ee&rsquo;s:{" "}
          <span className="font-mono text-[12px] text-slate-300">ee playbook extract</span> waits for three notes that
          carry one lesson, and ee proposes an anti-pattern after three harmful outcomes. In this toy no family contains a
          win. A real family might, so
          an anti-pattern remains an audited rule with its evidence attached.
        </p>
      </div>
    </DemoFrame>
  );
}

/** What the latest session did, in words, for the live region and for a reader who skims. */
function describe(shown: number, none: readonly Session[], ee: readonly Session[]): string {
  if (shown === 0) {
    return `Session 0 of ${SESSIONS}. Both sides search the same hidden landscape with the same agent, which makes ${ATTEMPTS_PER_SESSION} attempts a session and favours whatever looks most promising.`;
  }
  const left = none[shown - 1];
  const right = ee[shown - 1];
  if (!left || !right) return "";
  const repeats = left.attempts.filter((a) => a.repeat).length;
  const leftWins = left.attempts.filter((a) => a.win).length;
  const rightWins = right.attempts.filter((a) => a.win).length;
  // A session can land on two of the wins (rare, but a new landscape can draw one).
  const hit = (wins: number) => (wins === 0 ? "." : wins === 1 ? ", and one hit a win." : `, and ${wins} hit wins.`);
  const names = right.distilled.map((f) => FAMILY_NAMES[f] ?? "?");
  const distilled =
    names.length === 0
      ? ""
      : `${names.length === 1 ? `Family ${names[0]} has` : `Families ${names.join(" and ")} have`} reached ${DISTILL_AT} recorded failures, and curation turns ${
          names.length === 1 ? "it into an anti-pattern" : "them into anti-patterns"
        } for the next pack.`;
  if (shown === 1) {
    const win = leftWins === 0 ? "" : leftWins === 1 ? " One of them is a win." : ` ${leftWins} of them are wins.`;
    return `Session 1 of ${SESSIONS}. Nothing is remembered yet, so both agents make the same ${ATTEMPTS_PER_SESSION} attempts.${win} ${distilled}`.trim();
  }
  const parts = [`Session ${shown} of ${SESSIONS}.`];
  const spent =
    repeats === 0
      ? "No memory: no attempt repeated a known dead end"
      : `No memory: ${repeats} of ${ATTEMPTS_PER_SESSION} attempts repeated a known dead end`;
  parts.push(`${spent}${hit(leftWins)}`);
  parts.push(`With ee: ${ATTEMPTS_PER_SESSION} attempts, none on a recorded failure or a pruned family${hit(rightWins)}`);
  if (distilled) parts.push(distilled);
  return parts.join(" ");
}

function Legend() {
  const items: { key: string; label: string; swatch: ReactNode }[] = [
    {
      key: "promise",
      label: "Brighter: looks more promising",
      swatch: (
        <>
          <rect x={0} y={3} width={4} height={8} rx={1} fill={PROMISE} fillOpacity={0.08} />
          <rect x={5} y={3} width={4} height={8} rx={1} fill={PROMISE} fillOpacity={0.2} />
          <rect x={10} y={3} width={4} height={8} rx={1} fill={PROMISE} fillOpacity={0.36} />
        </>
      ),
    },
    {
      key: "failed",
      label: "Tried, failed",
      swatch: (
        <>
          <rect x={2} y={2} width={10} height={10} rx={2} fill={FAILED} fillOpacity={0.6} />
          <path d="M4.5 4.5l5 5M9.5 4.5l-5 5" stroke="#e2e8f0" strokeOpacity={0.7} strokeWidth={1.2} />
        </>
      ),
    },
    {
      key: "repeat",
      label: "A known dead end, tried again",
      swatch: <rect x={2} y={2} width={10} height={10} rx={2} fill={REPEAT} />,
    },
    {
      key: "pruned",
      label: "Pruned by an anti-pattern",
      swatch: (
        <>
          <rect x={1} y={1} width={12} height={12} rx={1} style={{ fill: CHAPTER }} fillOpacity={0.1} />
          <path d="M-2 6l8-8M-2 12l14-14M2 16l14-14M8 16l8-8" style={{ stroke: CHAPTER }} strokeWidth={1.4} strokeOpacity={0.75} />
          <rect x={1} y={1} width={12} height={12} rx={1} fill="none" style={{ stroke: CHAPTER }} strokeWidth={1.2} />
        </>
      ),
    },
    {
      key: "win",
      label: "A win",
      swatch: <rect x={2} y={2} width={10} height={10} rx={2} fill={WIN} />,
    },
    {
      key: "current",
      label: "This session's attempts",
      swatch: <rect x={1.5} y={1.5} width={11} height={11} rx={2.5} fill="none" stroke="#fff" strokeWidth={1.5} />,
    },
  ];
  return (
    <ul className="flex flex-wrap gap-x-4 gap-y-2 text-[11px] text-slate-400">
      {items.map((item) => (
        <li key={item.key} className="flex items-center gap-2">
          <svg aria-hidden="true" width={14} height={14} viewBox="0 0 14 14" className="shrink-0 overflow-hidden">
            {item.swatch}
          </svg>
          {item.label}
        </li>
      ))}
    </ul>
  );
}

function Panel({
  mode,
  landscape,
  log,
  snap,
  reveal,
  reduced,
}: {
  mode: Mode;
  landscape: Landscape;
  log: readonly Session[];
  snap: Snapshot;
  reveal: boolean;
  reduced: boolean;
}) {
  const withEe = mode === "ee";
  const inFamilies = snap.familyAttempts.reduce((sum, count) => sum + count, 0);
  const accent = (value: ReactNode) => (withEe ? <span className="text-[color:var(--chapter)]">{value}</span> : value);
  return (
    <div className="min-w-0">
      <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
        <p
          className={cn(
            "font-mono text-[11px] font-bold tracking-[0.16em] uppercase",
            withEe ? "text-[color:var(--chapter)]" : "text-rose-300",
          )}
        >
          {withEe ? "With ee" : "No memory"}
        </p>
        <p className="text-[11px] text-slate-500">
          {withEe ? "Each session starts from the ledger's pack" : "Each session starts with no record of the others"}
        </p>
      </div>
      <div
        className={cn(
          "mt-2 rounded-2xl border bg-black/30 p-2 sm:p-3",
          withEe ? "border-[color:var(--chapter)]/25" : "border-white/[0.07]",
        )}
      >
        <Field mode={mode} landscape={landscape} log={log} snap={snap} reveal={reveal} reduced={reduced} />
      </div>
      <div className="mt-2 grid grid-cols-3 gap-2">
        <Readout
          label="Dead ends re-tried"
          value={accent(snap.repeats)}
          tone={withEe ? "neutral" : "wrong"}
          note={`of ${snap.attempts} attempts`}
        />
        <Readout
          label="First win"
          value={accent(sessionLabel(snap.firstWin))}
          note={
            snap.firstWin === null && snap.shown >= SESSIONS
              ? `none in ${SESSIONS} sessions`
              : `${snap.winsFound} of ${WIN_COUNT} found`
          }
        />
        <Readout
          label="Families pruned"
          value={accent(withEe ? `${snap.pruned.size} of ${landscape.families.length}` : "0")}
          note={withEe ? `${plural(snap.skipped, "untried cell")} ruled out` : `${plural(inFamilies, "try", "tries")} spent in them`}
        />
      </div>
    </div>
  );
}

/** Coordinates to the hundredth of a pixel: the figure draws ~900 cells, and full doubles would bloat the page's HTML. */
const n = (value: number) => Math.round(value * 100) / 100;

/** The outline of a set of cells: every cell edge that does not face another cell of the set. */
function outlinePath(cells: readonly number[], size: number): string {
  const set = new Set(cells);
  let d = "";
  for (const cell of cells) {
    const cx = cellX(cell);
    const cy = cellY(cell);
    const x = n(cx * size);
    const y = n(cy * size);
    const s = n(size);
    if (cy === 0 || !set.has(cell - COLS)) d += `M${x} ${y}h${s}`;
    if (cy === ROWS - 1 || !set.has(cell + COLS)) d += `M${x} ${n(y + size)}h${s}`;
    if (cx === 0 || !set.has(cell - 1)) d += `M${x} ${y}v${s}`;
    if (cx === COLS - 1 || !set.has(cell + 1)) d += `M${n(x + size)} ${y}v${s}`;
  }
  return d;
}

/** A set of cells as one shape, square by square, so a family's hatch is one element rather than one per cell. */
function cellsPath(cells: readonly number[], size: number): string {
  const s = n(size);
  return cells.map((cell) => `M${n(cellX(cell) * size)} ${n(cellY(cell) * size)}h${s}v${s}h${-s}z`).join("");
}

const PAD = { left: 16, top: 3, right: 3, bottom: 18 };

/** One panel's hypothesis space: COLS × ROWS cells, each one an idea the agent could try. */
function Field({
  mode,
  landscape,
  log,
  snap,
  reveal,
  reduced,
}: {
  mode: Mode;
  landscape: Landscape;
  log: readonly Session[];
  snap: Snapshot;
  reveal: boolean;
  reduced: boolean;
}) {
  const { ref, width } = useFigureWidth(520);
  // React's ids carry punctuation that a url(#…) reference cannot.
  const hatch = `ledger-hatch${useId().replace(/[^a-zA-Z0-9_-]/g, "")}`;
  const withEe = mode === "ee";
  const cell = Math.max(6, (width - PAD.left - PAD.right) / COLS);
  const gridW = cell * COLS;
  const gridH = cell * ROWS;
  const height = Math.ceil(PAD.top + gridH + PAD.bottom);
  const inset = cell >= 14 ? 1.1 : 0.7;
  const size = n(cell - 2 * inset);
  const radius = n(Math.min(3, size * 0.22));
  const big = cell >= 15;
  const marks = cell >= 9;
  // Only a cell this campaign will try ever changes colour, so only those carry a transition.
  const touched = useMemo(() => new Set(log.flatMap((session) => session.attempts.map((attempt) => attempt.cell))), [log]);
  const fade = reduced ? undefined : { transition: "fill 450ms, fill-opacity 450ms" };

  const outlines = useMemo(() => landscape.families.map((family) => outlinePath(family.cells, cell)), [landscape, cell]);
  const shapes = useMemo(() => landscape.families.map((family) => cellsPath(family.cells, cell)), [landscape, cell]);
  const centres = useMemo(
    () =>
      landscape.families.map((family) => {
        const count = family.cells.length || 1;
        const x = family.cells.reduce((sum, c) => sum + cellX(c) + 0.5, 0) / count;
        const y = family.cells.reduce((sum, c) => sum + cellY(c) + 0.5, 0) / count;
        return { x, y };
      }),
    [landscape],
  );

  const labelFor = (name: string, pruned: boolean) => (pruned && big ? `anti-pattern ${name}` : name);
  const fontSize = big ? 10.5 : 9;
  const placeLabel = (text: string, family: number) => {
    const centre = centres[family] ?? { x: 0, y: 0 };
    const half = (text.length * fontSize * 0.58) / 2 + 2;
    return {
      x: n(Math.min(gridW - half, Math.max(half, centre.x * cell))),
      y: n(centre.y * cell + fontSize * 0.35),
    };
  };

  const label = withEe ? "With ee" : "No memory";
  const summary =
    snap.shown === 0
      ? `${label}: a ${COLS} by ${ROWS} grid of untried ideas, brighter where they look more promising.`
      : `${label}, after ${plural(snap.shown, "session")}: ${snap.attempts} attempts, ${snap.repeats} of them on a dead end already tried, first win ${
          snap.firstWin === null ? "not yet" : `in session ${snap.firstWin}`
        }, ${plural(snap.pruned.size, "family", "families")} pruned by an anti-pattern.`;

  return (
    <div ref={ref}>
      <svg
        className="diagram block h-auto w-full select-none"
        width={width}
        height={height}
        viewBox={`0 0 ${width} ${height}`}
        role="img"
        aria-label={summary}
      >
        <defs>
          <pattern id={hatch} width={5} height={5} patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
            <rect width={5} height={5} style={{ fill: CHAPTER }} fillOpacity={0.1} />
            <line x1={0} y1={0} x2={0} y2={5} style={{ stroke: CHAPTER }} strokeWidth={1.6} strokeOpacity={0.7} />
          </pattern>
        </defs>

        <text x={PAD.left} y={height - 4} fontSize={10} fill={MUTED} className="mono">
          approach →
        </text>
        <text transform={`translate(10 ${n(PAD.top + gridH)}) rotate(-90)`} fontSize={10} fill={MUTED} className="mono">
          parameter →
        </text>

        <g transform={`translate(${PAD.left} ${PAD.top})`}>
          {/* The ideas: lit by how promising they look until tried, then by what trying them cost. */}
          <g fill={PROMISE}>
          {snap.cells.map((state, index) => {
            const appeal = landscape.appeal[index] ?? 0;
            let fill = PROMISE;
            let opacity = 0.03 + 0.34 * appeal ** 1.5;
            if (state.win) {
              fill = WIN;
              opacity = 0.95;
            } else if (state.repeats > 0) {
              fill = REPEAT;
              opacity = Math.min(1, 0.55 + 0.15 * state.repeats);
            } else if (state.failures > 0) {
              fill = FAILED;
              opacity = 0.55;
            }
            return (
              <rect
                key={index}
                x={n(cellX(index) * cell + inset)}
                y={n(cellY(index) * cell + inset)}
                width={size}
                height={size}
                rx={radius}
                fill={fill === PROMISE ? undefined : fill}
                fillOpacity={Math.round(opacity * 1000) / 1000}
                style={touched.has(index) ? fade : undefined}
              />
            );
          })}
          </g>

          {/* An anti-pattern's family, hatched from the session curation applied it. Drawn for every family, so it fades in. */}
          {withEe
            ? landscape.families.map((family) => (
                <g
                  key={family.id}
                  opacity={snap.pruned.has(family.id) ? 1 : 0}
                  style={{ transition: reduced ? undefined : "opacity 700ms ease" }}
                >
                  <path d={shapes[family.id]} fill={`url(#${hatch})`} />
                  <path d={outlines[family.id]} fill="none" style={{ stroke: CHAPTER }} strokeWidth={1.6} strokeLinecap="square" />
                </g>
              ))
            : null}

          {/* What each tried idea cost: a cross for a failure, the count for a dead end tried more than once. */}
          {marks
            ? snap.cells.map((state, index) => {
                if (state.win || state.failures === 0) return null;
                const x = n(cellX(index) * cell + cell / 2);
                const y = n(cellY(index) * cell + cell / 2);
                if (state.repeats > 0 && big) {
                  return (
                    <text key={index} x={x} y={n(y + 3.5)} textAnchor="middle" fontSize={10} fontWeight={800} fill="#fff" className="mono">
                      {state.failures}
                    </text>
                  );
                }
                if (state.repeats > 0) return null;
                const arm = n(size * 0.2);
                return (
                  <path
                    key={index}
                    d={`M${x - arm} ${y - arm}l${2 * arm} ${2 * arm}M${x + arm} ${y - arm}l${-2 * arm} ${2 * arm}`}
                    stroke="#e2e8f0"
                    strokeOpacity={0.6}
                    strokeWidth={1.2}
                  />
                );
              })
            : null}

          {/* The hidden landscape, on request: every dead-end family dashed, every win ringed. */}
          {reveal
            ? landscape.families.map((family) =>
                withEe && snap.pruned.has(family.id) ? null : (
                  <path
                    key={`reveal-${family.id}`}
                    d={outlines[family.id]}
                    fill="none"
                    stroke="#94a3b8"
                    strokeOpacity={0.8}
                    strokeWidth={1.1}
                    strokeDasharray="3 3"
                  />
                ),
              )
            : null}
          {reveal
            ? landscape.wins.map((win) =>
                snap.cells[win]?.win ? null : (
                  <rect
                    key={`win-${win}`}
                    x={n(cellX(win) * cell + inset)}
                    y={n(cellY(win) * cell + inset)}
                    width={size}
                    height={size}
                    rx={radius}
                    fill="none"
                    stroke={WIN}
                    strokeWidth={1.4}
                    strokeDasharray="2 2"
                  />
                ),
              )
            : null}

          {/* A win, ringed so it holds the eye. */}
          {snap.cells.map((state, index) =>
            state.win ? (
              <rect
                key={`won-${index}`}
                x={n(cellX(index) * cell - 1)}
                y={n(cellY(index) * cell - 1)}
                width={n(cell + 2)}
                height={n(cell + 2)}
                rx={radius + 1.5}
                fill="none"
                stroke={WIN}
                strokeOpacity={0.55}
                strokeWidth={2}
              />
            ) : null,
          )}

          {/* This session's attempts. */}
          {snap.cells.map((state, index) =>
            state.current ? (
              <rect
                key={`now-${index}`}
                x={n(cellX(index) * cell + inset - 1.2)}
                y={n(cellY(index) * cell + inset - 1.2)}
                width={n(size + 2.4)}
                height={n(size + 2.4)}
                rx={radius + 1}
                fill="none"
                stroke="#fff"
                strokeWidth={1.6}
              />
            ) : null,
          )}

          {/* Family names: an anti-pattern's in the chapter's colour, the rest only when the landscape is revealed. */}
          <g pointerEvents="none">
            {landscape.families.map((family) => {
              const pruned = withEe && snap.pruned.has(family.id);
              if (!pruned && !reveal) return null;
              const text = labelFor(family.name, pruned);
              const at = placeLabel(text, family.id);
              return (
                <text
                  key={`label-${family.id}`}
                  x={at.x}
                  y={at.y}
                  textAnchor="middle"
                  fontSize={fontSize}
                  fontWeight={800}
                  style={{ fill: pruned ? CHAPTER : "#cbd5e1" }}
                  paintOrder="stroke"
                  stroke={INK}
                  strokeWidth={3}
                  strokeLinejoin="round"
                >
                  {text}
                </text>
              );
            })}
          </g>
        </g>
      </svg>
    </div>
  );
}

/** The session counter, the landscape's number, and a tick where curation distilled a family. */
function Timeline({ log, shown, seed, reduced }: { log: readonly Session[]; shown: number; seed: number; reduced: boolean }) {
  return (
    <div className="flex min-w-0 flex-1 flex-wrap items-center gap-x-3 gap-y-1.5">
      <p className="shrink-0 font-mono text-xs text-slate-400 tabular-nums">
        Session <span className="text-white">{String(shown).padStart(2, "0")}</span>/{SESSIONS}
      </p>
      <div aria-hidden="true" className="flex h-4 min-w-[9rem] flex-1 items-end gap-[2px]">
        {log.map((session, index) => {
          const on = index < shown;
          const distilled = on && session.distilled.length > 0;
          return (
            <div key={session.number} className="relative h-1.5 min-w-0 flex-1">
              <div
                className={cn("h-full rounded-full", on ? "" : "bg-white/[0.08]")}
                style={{
                  background: on ? CHAPTER : undefined,
                  opacity: on ? 0.75 : 1,
                  transition: reduced ? undefined : "background-color 300ms, opacity 300ms",
                }}
              />
              {distilled ? (
                <span
                  className="absolute -top-2.5 left-1/2 h-1.5 w-1.5 -translate-x-1/2 rotate-45"
                  style={{ background: CHAPTER }}
                />
              ) : null}
            </div>
          );
        })}
      </div>
      <p className="shrink-0 font-mono text-[11px] text-slate-500 tabular-nums">
        {shown * ATTEMPTS_PER_SESSION} attempts each · landscape #{seed}
      </p>
    </div>
  );
}

/** The ledger itself, one row per family: what ee has recorded, and what the agent without memory has spent. */
function FamilyLedger({ landscape, none, ee }: { landscape: Landscape; none: Snapshot; ee: Snapshot }) {
  return (
    <div className="min-w-0 rounded-2xl border border-white/[0.07] bg-black/20 p-4">
      <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
        <p className="text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase">The ledger, by family</p>
        <p className="font-mono text-[10px] text-slate-500">distilled at {DISTILL_AT} recorded failures</p>
      </div>
      <table className="mt-3 w-full border-collapse text-left text-[12px]">
        <thead>
          <tr className="text-[9px] tracking-[0.18em] text-slate-500 uppercase">
            <th scope="col" className="pb-2 pr-3 font-black">
              Family
            </th>
            <th scope="col" className="pb-2 pr-3 font-black text-[color:var(--chapter)]">
              With ee
            </th>
            <th scope="col" className="pb-2 text-right font-black text-rose-300/80">
              No memory
            </th>
          </tr>
        </thead>
        <tbody>
          {landscape.families.map((family) => {
            const failures = ee.familyFailures[family.id] ?? 0;
            const prunedAt = ee.pruned.get(family.id);
            const ruledOut = family.cells.filter((c) => (ee.cells[c]?.failures ?? 0) === 0 && !ee.cells[c]?.win).length;
            const tries = none.familyAttempts[family.id] ?? 0;
            const repeats = family.cells.reduce((sum, c) => sum + (none.cells[c]?.repeats ?? 0), 0);
            return (
              <tr key={family.id} className="border-t border-white/[0.06] align-middle">
                <th scope="row" className="py-2 pr-3 font-mono text-[13px] font-bold text-white">
                  {family.name}
                </th>
                <td className="py-2 pr-3">
                  <span className="flex flex-wrap items-center gap-x-2 gap-y-1">
                    <span aria-hidden="true" className="flex gap-1">
                      {Array.from({ length: Math.max(DISTILL_AT, failures) }, (_, index) => (
                        <span
                          key={index}
                          className={cn("h-2 w-2 rounded-full", index < failures ? "" : "border border-white/20")}
                          style={index < failures ? { background: CHAPTER } : undefined}
                        />
                      ))}
                    </span>
                    {prunedAt !== undefined ? (
                      <span className="whitespace-nowrap text-[color:var(--chapter)]">anti-pattern from S{prunedAt}</span>
                    ) : failures > 0 ? (
                      <span className="text-slate-300">
                        {failures} of {DISTILL_AT} failures recorded
                      </span>
                    ) : (
                      <span className="text-slate-600">no failure yet</span>
                    )}
                  </span>
                  {prunedAt !== undefined ? (
                    <span className="mt-0.5 block text-[11px] text-slate-500">{plural(ruledOut, "cell")} never tried</span>
                  ) : null}
                </td>
                {/* Two unbreakable halves, so on a narrow phone the cell wraps rather than pushing the table out of its box. */}
                <td className="py-2 text-right font-mono tabular-nums">
                  <span className={cn("whitespace-nowrap", tries > 0 ? "text-slate-300" : "text-slate-600")}>
                    {plural(tries, "try", "tries")}
                  </span>
                  {repeats > 0 ? (
                    <>
                      {" "}
                      <span className="whitespace-nowrap text-rose-300">· {repeats} again</span>
                    </>
                  ) : null}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

const STEPS = [
  {
    id: "capture",
    title: "Capture",
    command: 'ee remember "Tried … Family: B. Cause: …" --level episodic --kind failure',
    note: "Each failed attempt is recorded before the revert, with its family as a typed field.",
  },
  {
    id: "group",
    title: "Group",
    command: 'ee search "<the idea>" --kind failure --field family=B',
    note: "A filter on that field returns the family's failures together.",
  },
  {
    id: "distill",
    title: "Distill",
    command: "ee curate validate <id> · ee curate apply <id>",
    note: "A candidate anti-pattern becomes a rule only when curation applies it.",
  },
  {
    id: "pack",
    title: "Pack",
    command: 'ee pack "<the next idea>"',
    note: "When an anti-pattern clears the relevance floor, the next pack selects it first.",
  },
] as const;

/** The part of the figure that is ee's: the loop each panel either runs or does not. */
function Loop({ latest }: { latest: Session | undefined }) {
  const distilled = latest?.distilled.map((f) => FAMILY_NAMES[f] ?? "?") ?? [];
  return (
    <div className="min-w-0 rounded-2xl border border-[color:var(--chapter)]/30 bg-[color:var(--chapter)]/[0.05] p-4">
      <p className="text-[9px] font-black tracking-[0.2em] text-[color:var(--chapter)] uppercase">The ee commands in this loop</p>
      <ol className="mt-2 space-y-1">
        {STEPS.map((step, index) => {
          const lit = step.id === "distill" && distilled.length > 0;
          return (
            <li
              key={step.id}
              className={cn(
                "grid grid-cols-[1.25rem_minmax(0,1fr)] gap-x-2 rounded-lg border px-2.5 py-1.5 transition-colors duration-500",
                lit ? "border-[color:var(--chapter)]/60 bg-[color:var(--chapter)]/[0.08]" : "border-transparent",
              )}
            >
              <span className="pt-px font-mono text-[11px] text-slate-500 tabular-nums">{index + 1}</span>
              <div className="min-w-0">
                <p className="text-[13px] font-semibold text-white">
                  {step.title}
                  {lit ? (
                    <span className="ml-2 text-[12px] font-normal text-[color:var(--chapter)]">
                      session {latest?.number}: {distilled.length === 1 ? "family" : "families"} {distilled.join(" and ")}
                    </span>
                  ) : null}
                </p>
                <p className="mt-0.5 font-mono text-[11.5px] [overflow-wrap:anywhere] text-violet-200">$ {step.command}</p>
                <p className="mt-0.5 hidden text-[12px] leading-snug text-slate-400 sm:block">{step.note}</p>
              </div>
            </li>
          );
        })}
      </ol>
    </div>
  );
}
