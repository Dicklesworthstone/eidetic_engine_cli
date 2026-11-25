"use client";

import { Pause, Play, RotateCcw } from "lucide-react";
import { type PointerEvent, type ReactNode, useEffect, useId, useMemo, useRef, useState } from "react";

import { DemoButton, DemoFrame, Segmented, Slider, type Tone } from "@/components/explain/demo";
import {
  campaign,
  type Category,
  EE_BUDGET_TOKENS,
  EE_CANDIDATE_POOL,
  EE_CATCH_PER_WEEK,
  LANE_SESSION,
  type Lane,
  LESSON_TOKENS,
  LESSONS_PER_WEEK,
  NOTES_TRIM_AT,
  NOTES_TRIM_TO,
  RECEIPT_TOKENS,
  RESTATE_SHARE,
  series,
  type SeriesPoint,
  type Session,
  SIMILARITY,
  STALE_PER_WEEK,
  TASKS,
  type TaskId,
  TRIM_WEEKS,
  VECTOR_K,
  WEEKS,
} from "@/components/explain/scale-model";
import { Cmd } from "@/components/instrument";
import { useFigureWidth, useInView, useReducedMotion } from "@/lib/hooks";
import { clamp, cn, fixed, grouped } from "@/lib/utils";

/**
 * One campaign's lessons, handed back three ways: a notes file loaded whole, a vector store's top k, and ee's
 * budgeted pack. The arithmetic and every assumption live in scale-model.ts; this file only draws it.
 */

const STEP_MS = 110;
/** The shared token scale of the gauges and the tokens chart: past the largest session any lane reaches. */
const TOKEN_SCALE = 12_000;
/** Cells per row in a lane, widest first. */
const COLUMN_CHOICES = [50, 40, 25, 20] as const;
const EE_LINES = EE_BUDGET_TOKENS / LESSON_TOKENS;
const INK = "#05040d";

const CATEGORY: Record<Category, { label: string; fill: string; stroke?: string; swatch: string }> = {
  useful: { label: "On-task and true", fill: "#34d399", swatch: "#34d399" },
  offtask: { label: "True, off-task", fill: "#475569", swatch: "#475569" },
  repeat: {
    label: "Repeats an earlier line",
    fill: "rgba(226,232,240,0.07)",
    stroke: "rgba(226,232,240,0.8)",
    swatch: "rgba(226,232,240,0.07)",
  },
  stale: {
    label: "Stale or contradicted",
    fill: "#fb7185",
    swatch: `repeating-linear-gradient(135deg, #fb7185 0 2px, rgba(5,4,13,0.55) 2px 3.2px)`,
  },
};

const LEGEND: readonly Category[] = ["useful", "offtask", "repeat", "stale"];

/** The task selector's short names, so the three fit one row on a phone. */
const TASK_SHORT: Record<TaskId, string> = { flaky: "Flaky test", migrate: "Migration", release: "Release" };

/** `dash` sets the vector store's line apart from ee's in the charts by more than hue: sky and violet are close. */
const LANES: readonly { id: Lane; name: string; tag: string; colour: string; capacity: number; dash?: string }[] = [
  { id: "notes", name: "Notes file", tag: "CLAUDE.md · AGENTS.md", colour: "#fbbf24", capacity: NOTES_TRIM_AT },
  {
    id: "vector",
    name: "Vector store",
    tag: `top ${VECTOR_K} by similarity`,
    colour: "#38bdf8",
    capacity: VECTOR_K,
    dash: "6 4",
  },
  { id: "ee", name: "ee pack", tag: "--max-tokens 4000", colour: "#a78bfa", capacity: EE_LINES },
];

type Metric = "tokens" | "useful" | "stale";

interface MetricSpec {
  id: Metric;
  label: string;
  title: string;
  max: number;
  ticks: readonly number[];
  tick: (value: number) => string;
  format: (value: number) => string;
  value: (point: SeriesPoint) => number;
}

const percent = (value: number) => `${Math.round(value * 100)}%`;

const METRICS: readonly MetricSpec[] = [
  {
    id: "tokens",
    label: "Tokens",
    title: "Tokens a session",
    max: TOKEN_SCALE,
    ticks: [0, 4000, 8000, 12000],
    tick: (value) => (value === 0 ? "0" : `${value / 1000}k`),
    format: grouped,
    value: (point) => point.tokens,
  },
  {
    id: "useful",
    label: "Useful",
    title: "Useful lines",
    max: 0.5,
    ticks: [0, 0.1, 0.2, 0.3, 0.4, 0.5],
    tick: percent,
    format: percent,
    value: (point) => point.usefulShare,
  },
  {
    id: "stale",
    label: "Stale",
    title: "Stale or contradicted lines",
    max: 30,
    ticks: [0, 10, 20, 30],
    tick: String,
    format: String,
    value: (point) => point.stale,
  },
];

const TONE_CLASS: Record<Tone, string> = {
  rule: "text-violet-200",
  wrong: "text-rose-300",
  neutral: "text-white",
};

export function MarkdownVsMemory() {
  const frame = useRef<HTMLDivElement>(null);
  const reduced = useReducedMotion();
  const inView = useInView(frame);
  // The HTML (and a reader who asks for less motion) shows the whole year; the first time the figure comes into
  // view it starts again from week one and plays the campaign through.
  const [week, setWeek] = useState(WEEKS);
  const [task, setTask] = useState<TaskId>("flaky");
  const [metric, setMetric] = useState<Metric>("tokens");
  const [playing, setPlaying] = useState(false);
  const started = useRef(false);

  useEffect(() => {
    if (!inView || reduced || started.current) return;
    started.current = true;
    setWeek(1);
    setPlaying(true);
  }, [inView, reduced]);

  useEffect(() => {
    if (!playing || reduced || !inView) return;
    if (week >= WEEKS) {
      setPlaying(false);
      return;
    }
    const handle = window.setTimeout(() => setWeek((current) => Math.min(WEEKS, current + 1)), STEP_MS);
    return () => window.clearTimeout(handle);
  }, [playing, reduced, inView, week]);

  const sessions = useMemo(
    () => ({
      notes: LANE_SESSION.notes(week, task),
      vector: LANE_SESSION.vector(week, task),
      ee: LANE_SESSION.ee(week, task),
    }),
    [week, task],
  );
  const data = useMemo(
    () => ({ notes: series("notes", task), vector: series("vector", task), ee: series("ee", task) }),
    [task],
  );
  // The year's last sessions, whose notes are the longest each lane shows: LaneRow lays them out unseen so that a
  // note growing as the campaign plays never changes the figure's height.
  const finals = useMemo(
    () => ({
      notes: LANE_SESSION.notes(WEEKS, task),
      vector: LANE_SESSION.vector(WEEKS, task),
      ee: LANE_SESSION.ee(WEEKS, task),
    }),
    [task],
  );
  const status = useMemo(() => campaign(week, task), [week, task]);
  const trims = TRIM_WEEKS.filter((trim) => trim <= week).length;
  const taskLabel = (TASKS.find((entry) => entry.id === task) ?? TASKS[0]).label;
  const done = week >= WEEKS;
  const animating = playing && !reduced;

  const pick = (next: number) => {
    setPlaying(false);
    setWeek(clamp(Math.round(next), 1, WEEKS));
  };

  return (
    <DemoFrame
      eyebrow={`${WEEKS} weeks of lessons · three memory setups`}
      title="What a session gets from a notes file, a vector store and ee."
      basis="derived"
      source={
        <>
          A model, not a benchmark: one seeded draw of {grouped(WEEKS * LESSONS_PER_WEEK)} lessons that all three lanes
          share. Assumed: {LESSONS_PER_WEEK} lessons a week, {LESSON_TOKENS} tokens each · one new lesson in{" "}
          {Math.round(1 / RESTATE_SHARE)} restates one still true · each week {fixed(STALE_PER_WEEK * 100, 2)}% of true lessons are
          overtaken · the file is trimmed past {NOTES_TRIM_AT} lines back to {NOTES_TRIM_TO}, stale and repeated lines
          first, then the oldest · the vector store returns {VECTOR_K} bare chunks, the {grouped(EE_BUDGET_TOKENS)} tokens
          of lessons ee&apos;s budget buys · both rank by one similarity: on-task lessons 0.3 to 1, others 0 to 0.58 · ee
          notices a stale lesson with chance {fixed(EE_CATCH_PER_WEEK * 100, 2)}% a week (a revision, a harmful outcome, the
          contradiction guard, decay) · MMR similarity {SIMILARITY.sameFact} for two copies of a lesson,{" "}
          {SIMILARITY.sameTopic} within a topic, {SIMILARITY.other} otherwise · an ee item&apos;s id, why, trust and
          provenance lines about {RECEIPT_TOKENS} tokens · not modeled: section quotas, the preview and link-only tiers,
          the query-relative relevance cutoff, the memory-tier boost, and the helpful/harmful outcome bias (at most ±0.05, AGENT_PROFILE_BIAS_CAP in
          src/models/context_profile.rs). ee&apos;s side: src/pack/mod.rs ·
          DEFAULT_CONTEXT_MAX_TOKENS 4,000 (counted on each memory&apos;s own text) · DEFAULT_MMR_RELEVANCE_WEIGHT 0.75 ·
          DEFAULT_COVERAGE_FILL_RELEVANCE_FLOOR 0.05 · PackOmissionReason · apply_contradiction_guard · the markdown
          renderer · tests/fixtures/golden/agent/context_pack.md.golden · src/config/merge.rs · [pack] candidate_pool{" "}
          {EE_CANDIDATE_POOL} · src/output/mod.rs · data.pack.skipped[], skippedTotal · src/core/context.rs ·
          include_tombstoned · src/core/agentsmd.rs · src/core/primer.rs · PRIMER_DEFAULT_BUDGET_TOKENS 600 · README.md ·
          Agent Harness Integration.
        </>
      }
    >
      <div ref={frame}>
        {/* Controls */}
        <div className="flex flex-col gap-4 lg:flex-row lg:items-end lg:gap-6">
          <Slider
            className="lg:flex-1"
            label="Weeks into the campaign"
            min={1}
            max={WEEKS}
            value={week}
            onChange={pick}
            display={`week ${week} of ${WEEKS}`}
            valueText={`week ${week} of ${WEEKS}`}
          />
          <div className="flex flex-wrap items-center gap-2">
            <Segmented
              label="This session's task"
              options={TASKS.map((entry) => ({ value: entry.id, label: TASK_SHORT[entry.id] }))}
              value={task}
              onChange={setTask}
            />
            {reduced ? null : (
              <>
                {/* The label says what a press does, so no aria-pressed as well: "Pause, pressed" contradicts itself. */}
                <DemoButton
                  onClick={() => {
                    if (done) setWeek(1);
                    setPlaying((on) => !on || done);
                  }}
                  label={animating ? "Pause the campaign" : "Play the campaign"}
                  className={cn(animating && "border-violet-400/60 text-violet-200")}
                >
                  {animating ? <Pause className="h-3.5 w-3.5" /> : <Play className="h-3.5 w-3.5" />}
                  {animating ? "Pause" : "Play"}
                </DemoButton>
                <DemoButton
                  onClick={() => {
                    setWeek(1);
                    setPlaying(true);
                  }}
                  label="Replay the campaign from week one"
                >
                  <RotateCcw className="h-3.5 w-3.5" /> Replay
                </DemoButton>
              </>
            )}
          </div>
        </div>

        {/* Where the campaign stands */}
        <p className="mt-5 font-mono text-[11.5px] leading-relaxed text-slate-400 tabular-nums">
          <span className="text-[color:var(--chapter,#fb7185)]">Week {week}</span> · {grouped(status.written)} lessons
          written · {status.stale} now stale · <span className="text-slate-200">{status.onTaskTrue}</span> true lessons
          apply to &ldquo;{taskLabel.toLowerCase()}&rdquo;
        </p>

        {/* Legend */}
        <ul className="mt-3 flex flex-wrap gap-x-4 gap-y-1.5 text-[11px] text-slate-400">
          {LEGEND.map((category) => (
            <li key={category} className="flex items-center gap-2">
              <Swatch category={category} />
              {CATEGORY[category].label}
            </li>
          ))}
          <li className="flex items-center gap-2">
            <span aria-hidden="true" className="h-2.5 w-2.5 rounded-[2px] border border-white/15" />
            Empty slot
          </li>
        </ul>

        {/* The three lanes */}
        <ol className="ruled mt-3 border-t border-white/[0.06]">
          {LANES.map((lane) => (
            <LaneRow
              key={lane.id}
              lane={lane}
              session={sessions[lane.id]}
              final={finals[lane.id]}
              trims={trims}
              written={status.written}
            />
          ))}
        </ol>
        <p className="mt-1 text-[11.5px] leading-relaxed text-slate-500">
          <span className="text-slate-400">Useful</span>: on-task, true and not a repeat.{" "}
          <span className="text-slate-400">Missed</span>: true lessons about this task that the session does not have.{" "}
          <span className="text-slate-400">Deleted</span>: true lessons the file&apos;s trims have removed so far.{" "}
          <span className="text-slate-400">Listed</span>: candidates the lane reports as left out.
        </p>

        {/* The year, week by week */}
        <div className="mt-7">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <ul className="flex flex-wrap gap-x-4 gap-y-1.5 text-[11px] text-slate-400">
              {LANES.map((lane) => (
                <li key={lane.id} className="flex items-center gap-2">
                  <span
                    aria-hidden="true"
                    className="h-[3px] w-4 rounded-full"
                    style={{
                      background: lane.dash
                        ? `repeating-linear-gradient(90deg, ${lane.colour} 0 6px, transparent 6px 9px)`
                        : lane.colour,
                    }}
                  />
                  {lane.name}
                </li>
              ))}
              {/* The trims are marked on the tokens chart, which is the only chart on a phone until another is picked. */}
              <li className={cn("items-center gap-2", metric === "tokens" ? "flex" : "hidden lg:flex")}>
                <span aria-hidden="true" className="h-2.5 w-px bg-rose-300/70" />
                File trimmed
              </li>
            </ul>
            <Segmented
              className="lg:hidden"
              label="Chart"
              options={METRICS.map((spec) => ({ value: spec.id, label: spec.label }))}
              value={metric}
              onChange={setMetric}
            />
          </div>
          <div className="mt-3 grid grid-cols-1 gap-6 lg:grid-cols-3">
            {METRICS.map((spec) => (
              <Chart
                key={spec.id}
                spec={spec}
                data={data}
                week={week}
                onScrub={pick}
                className={cn(metric !== spec.id && "hidden lg:block")}
              />
            ))}
          </div>
        </div>

        {/* What to take from it */}
        <div className="mt-7 grid grid-cols-1 gap-x-6 gap-y-3 text-[13px] leading-relaxed text-slate-400 md:grid-cols-3">
          <p>
            <span className="font-semibold text-slate-200">Ranking.</span> The vector store and ee&apos;s search use the
            same similarity score here, so both miss the on-task lessons it scores low. Top-k has no record of which
            lessons went stale or repeat one already in the session, so it passes them through.
          </p>
          <p>
            <span className="font-semibold text-slate-200">Token cost.</span> ee&apos;s budget counts only the
            memories&apos; own text. In markdown, each item also lists its id, why it was chosen, its trust class and its
            provenance, about {RECEIPT_TOKENS} tokens here. A smaller <Cmd>--max-tokens</Cmd> gives a smaller pack.
          </p>
          <p>
            <span className="font-semibold text-slate-200">Keeping AGENTS.md.</span> <Cmd>ee export agentsmd</Cmd> writes
            the primer&apos;s rules and warnings into a marker-delimited block of AGENTS.md, 600 tokens by default. It
            never edits outside its markers. <Cmd>ee diag agentsmd-drift</Cmd> reports where the file and the memory disagree.
          </p>
        </div>
      </div>
    </DemoFrame>
  );
}

function Swatch({ category }: { category: Category }) {
  const style = CATEGORY[category];
  return (
    <span
      aria-hidden="true"
      className="h-2.5 w-2.5 shrink-0 rounded-[2px]"
      style={{ background: style.swatch, boxShadow: style.stroke ? `inset 0 0 0 1px ${style.stroke}` : undefined }}
    />
  );
}

function laneNote(lane: Lane, session: Session, trims: number): ReactNode {
  if (lane === "notes") {
    return trims === 0 ? (
      <>
        Every line goes into every session, on-task or off. Stale lines stay until the file is trimmed, which here
        happens once it passes {NOTES_TRIM_AT} lines.
      </>
    ) : (
      <>
        Every line goes into every session, on-task or off. The file has been trimmed{" "}
        {trims === 1 ? "once" : `${trims} times`}, stale and repeated lines first, and the trims have deleted{" "}
        <span className="text-rose-300">{grouped(session.lost)}</span> lessons that were still true.
      </>
    );
  }
  if (lane === "vector") {
    return (
      <>
        Returns up to {VECTOR_K} chunks, the ones most similar to the task. The score does not mark a chunk as stale
        or as a repeat, and the store keeps no record of what it left out.
      </>
    );
  }
  const left = Object.values(session.skipped).reduce<number>((sum, count) => sum + (count ?? 0), 0);
  return (
    <>
      Strict MMR fills at most {grouped(EE_BUDGET_TOKENS)} tokens of memory text and demotes repeats. Lessons ee has
      detected as stale are excluded.{" "}
      {left > 0 ? (
        <>
          {left === 1 ? "The 1 candidate it left out is" : `The ${left} candidates it left out are`} listed, each with a
          reason, in <Cmd>data.pack.skipped[]</Cmd>.
        </>
      ) : (
        <>No candidate has been left out yet.</>
      )}
    </>
  );
}

function LaneRow({
  lane,
  session,
  final,
  trims,
  written,
}: {
  lane: (typeof LANES)[number];
  session: Session;
  /** The lane's session in the campaign's last week. */
  final: Session;
  trims: number;
  written: number;
}) {
  const left = Object.values(session.skipped).reduce<number>((sum, count) => sum + (count ?? 0), 0);
  const last: { label: string; value: ReactNode; tone: Tone } =
    lane.id === "notes"
      ? { label: "Deleted", value: grouped(session.lost), tone: session.lost > 0 ? "wrong" : "neutral" }
      : lane.id === "vector"
        ? { label: "Listed", value: "none", tone: written > VECTOR_K ? "wrong" : "neutral" }
        : { label: "Listed", value: left, tone: "rule" };
  const stats: { label: string; value: ReactNode; tone: Tone }[] = [
    { label: "Useful", value: percent(session.usefulShare), tone: "neutral" },
    { label: "Stale", value: session.counts.stale, tone: session.counts.stale > 0 ? "wrong" : "neutral" },
    { label: "Repeats", value: session.counts.repeat, tone: "neutral" },
    { label: "Missed", value: session.missing, tone: "neutral" },
    last,
  ];

  return (
    <li className="grid grid-cols-1 gap-x-6 gap-y-3 py-4 md:grid-cols-[11rem_minmax(0,1fr)] lg:grid-cols-[13rem_minmax(0,1fr)]">
      <div className="min-w-0">
        <p className="flex flex-wrap items-baseline gap-x-2.5 gap-y-0.5">
          <span className="flex items-center gap-2 text-sm font-bold text-white">
            <span
              aria-hidden="true"
              className="h-2 w-2 shrink-0 rounded-full"
              style={{ background: lane.colour, boxShadow: `0 0 10px ${lane.colour}` }}
            />
            {lane.name}
          </span>
          <span className="font-mono text-[10.5px] text-slate-500">{lane.tag}</span>
        </p>
        {/* The last week's note sits in the same grid cell, unseen, so the row keeps its final height throughout. */}
        <p className="mt-1.5 grid text-[12px] leading-snug text-slate-400">
          <span className="col-start-1 row-start-1">{laneNote(lane.id, session, trims)}</span>
          <span aria-hidden="true" className="invisible col-start-1 row-start-1">
            {laneNote(lane.id, final, TRIM_WEEKS.length)}
          </span>
        </p>
      </div>
      <div className="min-w-0 space-y-3">
        <Cells session={session} capacity={lane.capacity} name={lane.name} />
        <TokenGauge session={session} colour={lane.colour} />
        {/* Three columns on a phone: at five, a 320 px screen leaves 43 px a column and cuts "Repeats" and "Deleted". */}
        <dl className="grid max-w-2xl grid-cols-3 gap-x-2 gap-y-2.5 sm:grid-cols-5 sm:gap-x-4">
          {stats.map((stat) => (
            <div key={stat.label} className="min-w-0">
              <dt className="truncate text-[9px] font-black tracking-[0.14em] text-slate-500 uppercase">{stat.label}</dt>
              <dd className={cn("mt-1 font-mono text-[15px] leading-none tabular-nums", TONE_CLASS[stat.tone])}>
                {stat.value}
              </dd>
            </div>
          ))}
        </dl>
      </div>
    </li>
  );
}

/** One square per injected line, in the order the session reads them, inside the lane's room. */
function Cells({ session, capacity, name }: { session: Session; capacity: number; name: string }) {
  const { ref, width } = useFigureWidth(860);
  const hatch = `scale-hatch-${useId().replace(/[^a-zA-Z0-9_-]/g, "")}`;
  const W = Math.max(240, width);
  // A column count that divides the lanes' rooms (200 and 80) into whole rows, as many as keep a square legible;
  // the squares then fill the row exactly.
  const columns = COLUMN_CHOICES.find((count) => W / count >= 11) ?? COLUMN_CHOICES[COLUMN_CHOICES.length - 1] ?? 20;
  const pitch = W / columns;
  const gap = pitch >= 14 ? 3 : 2;
  // Whole-pixel squares, so their edges stay crisp; columns start at the rounded pitch, rows at the square's own.
  const size = Math.floor(pitch - gap);
  const slots = Math.max(capacity, session.ids.length);
  const rows = Math.ceil(slots / columns);
  const H = rows * (size + gap) - gap;
  const { counts } = session;
  const label = `${name}, week ${session.week}: ${session.ids.length} lines, with space for ${capacity}. ${counts.useful} on-task and true, ${counts.offtask} off-task, ${counts.repeat} repeats, ${counts.stale} stale or contradicted.`;

  return (
    <div ref={ref} className="w-full">
      <svg className="block h-auto w-full" width={W} height={H} viewBox={`0 0 ${W} ${H}`} role="img" aria-label={label}>
        <defs>
          <pattern id={hatch} width="4" height="4" patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
            <rect width="1.6" height="4" fill={INK} fillOpacity="0.55" />
          </pattern>
        </defs>
        {Array.from({ length: slots }, (_, index) => {
          const cx = Math.round((index % columns) * pitch);
          const cy = Math.floor(index / columns) * (size + gap);
          const category = session.categories[index];
          if (!category) {
            return (
              <rect
                key={index}
                x={cx + 0.5}
                y={cy + 0.5}
                width={size - 1}
                height={size - 1}
                rx="1.5"
                fill="none"
                stroke="rgba(255,255,255,0.09)"
              />
            );
          }
          const style = CATEGORY[category];
          return (
            <g key={index}>
              <rect
                x={style.stroke ? cx + 0.5 : cx}
                y={style.stroke ? cy + 0.5 : cy}
                width={style.stroke ? size - 1 : size}
                height={style.stroke ? size - 1 : size}
                rx="1.5"
                fill={style.fill}
                stroke={style.stroke}
              />
              {category === "stale" ? <rect x={cx} y={cy} width={size} height={size} rx="1.5" fill={`url(#${hatch})`} /> : null}
            </g>
          );
        })}
      </svg>
    </div>
  );
}

/** The session's size on one scale for all three lanes; ee's id, why, trust and provenance lines hatched beside its lessons. */
function TokenGauge({ session, colour }: { session: Session; colour: string }) {
  const receipts = session.tokens - session.lessonTokens;
  const at = (tokens: number) => `${(Math.min(tokens, TOKEN_SCALE) / TOKEN_SCALE) * 100}%`;
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
      <div
        role="img"
        aria-label={`${grouped(session.tokens)} tokens a session${receipts > 0 ? `: ${grouped(session.lessonTokens)} of lesson text and ${grouped(receipts)} of id, why, trust and provenance lines` : ""}.`}
        className="relative h-2 min-w-[9rem] flex-1 overflow-hidden rounded-full bg-white/[0.05] ring-1 ring-white/[0.05]"
      >
        <span className="absolute inset-y-0 left-0" style={{ width: at(session.lessonTokens), background: colour, opacity: 0.85 }} />
        {receipts > 0 ? (
          <span
            className="absolute inset-y-0"
            style={{
              left: at(session.lessonTokens),
              width: at(receipts),
              background: `repeating-linear-gradient(135deg, ${colour} 0 2px, transparent 2px 4px)`,
              opacity: 0.8,
            }}
          />
        ) : null}
        <span aria-hidden="true" className="absolute inset-y-0 w-px bg-white/45" style={{ left: at(EE_BUDGET_TOKENS) }} />
      </div>
      <p className="shrink-0 font-mono text-[11px] text-slate-300 tabular-nums">
        {grouped(session.tokens)} tokens
        {receipts > 0 ? <span className="text-slate-500"> · {grouped(receipts)} of metadata</span> : null}
      </p>
    </div>
  );
}

function Chart({
  spec,
  data,
  week,
  onScrub,
  className,
}: {
  spec: MetricSpec;
  data: Record<Lane, SeriesPoint[]>;
  week: number;
  onScrub: (week: number) => void;
  className?: string;
}) {
  const { ref, width } = useFigureWidth(360);
  const W = Math.max(220, width);
  const H = 150;
  const m = { left: 34, right: 10, top: 10, bottom: 22 };
  const plotW = W - m.left - m.right;
  const plotH = H - m.top - m.bottom;
  const x = (at: number) => m.left + ((at - 1) / (WEEKS - 1)) * plotW;
  const y = (value: number) => m.top + (1 - clamp(value / spec.max, 0, 1)) * plotH;
  const path = (points: readonly SeriesPoint[]) =>
    points.map((point, index) => `${index === 0 ? "M" : "L"}${x(point.week).toFixed(1)} ${y(spec.value(point)).toFixed(1)}`).join(" ");
  const current = LANES.map((lane) => ({ lane, value: spec.value(data[lane.id][week - 1] as SeriesPoint) }));
  // The cursor's week tag, kept inside the plot at either end.
  const tag = clamp(x(week), m.left + 17, W - m.right - 17);

  const scrub = (event: PointerEvent<SVGSVGElement>) => {
    const box = event.currentTarget.getBoundingClientRect();
    if (box.width === 0) return;
    const px = ((event.clientX - box.left) / box.width) * W;
    onScrub(1 + clamp((px - m.left) / plotW, 0, 1) * (WEEKS - 1));
  };

  return (
    <div className={cn("min-w-0", className)}>
      <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
        <p className="text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase">{spec.title}</p>
        <p className="font-mono text-[11px] tabular-nums">
          {current.map(({ lane, value }, index) => (
            <span key={lane.id}>
              {index > 0 ? <span className="text-slate-600"> · </span> : null}
              <span style={{ color: lane.colour }}>{spec.format(value)}</span>
            </span>
          ))}
        </p>
      </div>
      <div ref={ref} className="mt-2 w-full">
        <svg
          className="diagram block h-auto w-full cursor-ew-resize select-none"
          style={{ touchAction: "pan-y" }}
          width={W}
          height={H}
          viewBox={`0 0 ${W} ${H}`}
          role="img"
          aria-label={`${spec.title} over ${WEEKS} weeks. At week ${week}: ${current
            .map(({ lane, value }) => `${lane.name} ${spec.format(value)}`)
            .join(", ")}.`}
          onPointerDown={(event) => {
            event.currentTarget.setPointerCapture(event.pointerId);
            scrub(event);
          }}
          onPointerMove={(event) => {
            if (event.currentTarget.hasPointerCapture(event.pointerId)) scrub(event);
          }}
        >
          {spec.ticks.map((value) => (
            <g key={value}>
              <line x1={m.left} x2={W - m.right} y1={y(value)} y2={y(value)} stroke="rgba(255,255,255,0.05)" />
              <text x={m.left - 7} y={y(value) + 3.5} textAnchor="end" className="mono" fontSize="9.5" fill="#77839c">
                {spec.tick(value)}
              </text>
            </g>
          ))}
          {[1, 13, 26, 39, 52].map((at) => (
            <text
              key={at}
              x={x(at)}
              y={H - 6}
              textAnchor={at === 1 ? "start" : at === WEEKS ? "end" : "middle"}
              className="mono"
              fontSize="9.5"
              fill="#77839c"
            >
              {at === 1 ? "wk 1" : at}
            </text>
          ))}

          {spec.id === "tokens"
            ? TRIM_WEEKS.map((at) => (
                <line
                  key={at}
                  x1={x(at)}
                  x2={x(at)}
                  y1={m.top}
                  y2={m.top + plotH}
                  stroke="#fda4af"
                  strokeOpacity="0.35"
                  strokeDasharray="2 3"
                />
              ))
            : null}

          {LANES.map((lane) => (
            <path
              key={lane.id}
              d={path(data[lane.id])}
              fill="none"
              stroke={lane.colour}
              strokeWidth="1.75"
              strokeDasharray={lane.dash}
              strokeLinejoin="round"
              strokeLinecap="round"
            />
          ))}

          <line x1={x(week)} x2={x(week)} y1={m.top} y2={m.top + plotH} stroke="#e2e8f0" strokeOpacity="0.5" />
          {current.map(({ lane, value }) => (
            <circle key={lane.id} cx={x(week)} cy={y(value)} r="3.5" fill={lane.colour} stroke={INK} strokeWidth="1.5" />
          ))}
          <rect x={tag - 17} y={m.top - 9} width="34" height="13" rx="4" style={{ fill: "var(--chapter, #fb7185)" }} />
          <text x={tag} y={m.top + 0.5} textAnchor="middle" className="mono" fontSize="9" fontWeight="700" fill={INK}>
            wk {week}
          </text>
        </svg>
      </div>
    </div>
  );
}
