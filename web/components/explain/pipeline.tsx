"use client";

import { Pause, Play } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import { DemoButton, DemoFrame } from "@/components/explain/demo";
import { useInView, useReducedMotion } from "@/lib/hooks";
import { cn } from "@/lib/utils";

/**
 * The spans ee records while it builds a pack (src/core/context.rs, `trace.record_elapsed`), in the order
 * they start. `count` is an illustrative candidate count after the stage, not a measurement.
 */
type Status = "always" | "optional" | "off";

export interface Stage {
  span: string;
  name: string;
  what: string;
  status: Status;
  statusNote?: string;
  code?: string;
  count: number;
}

const POOL = 64;

export const STAGES: readonly Stage[] = [
  {
    span: "requestValidate",
    name: "Validate the request",
    what: "Parses the task, profile and budget, and caps the token budget and candidate pool to the workspace's runtime profile. Every built-in profile asks for a pool of 64.",
    status: "always",
    count: POOL,
  },
  {
    span: "daemonRetrieval",
    name: "Ask the warm daemon",
    what: "With --use-daemon and a warm daemon, retrieval runs there instead. Its handoff is validated against the local snapshot; anything changed or unverifiable falls back to local execution, and the pack hash is the same either way.",
    status: "optional",
    statusNote: "only with --use-daemon",
    code: "daemon_search_fallback",
    count: POOL,
  },
  {
    span: "dbOpen",
    name: "Pin a snapshot",
    what: "Opens the FrankenSQLite store, the source of truth, and pins a read snapshot so the stages after it read one consistent view of memory.",
    status: "always",
    count: POOL,
  },
  {
    span: "search",
    name: "Hybrid search",
    what: "Frankensearch retrieves lexical (BM25) and semantic (Model2Vec) hits and fuses the two rankings by reciprocal rank; ee then rescales each hit by the quality multipliers below. A stale or missing index is reported as a degradation, with the command that repairs it.",
    status: "always",
    code: "search_index_stale",
    count: 60,
  },
  {
    span: "packL2Lookup",
    name: "Check the pack cache",
    what: "If this exact request was already packed against the same index generation, the cached pack can be replayed. A hit is ignored if the source state has changed, and live graph scoring bypasses the cache.",
    status: "optional",
    statusNote: "a miss here",
    count: 60,
  },
  {
    span: "candidateResolution",
    name: "Resolve candidates",
    what: "Resolves hits to memories, removes duplicates, loads tags, applies the query's filters and any graph hints it names, and attaches freshness and provenance.",
    status: "always",
    count: 51,
  },
  {
    span: "focusState",
    name: "Add the focus set",
    what: "Adds the memories in the workspace's focus set (ee focus add/set), scored like any other candidate. Focus changes ranking only; trust class and stored content stay as they are.",
    status: "optional",
    statusNote: "when a focus set exists",
    code: "context_focus_state_unavailable",
    count: 53,
  },
  {
    span: "memoryTierAdmission",
    name: "Storage-tier nudge",
    what: "Raises hot memories by 0.025 and warm ones by 0.010. It filters nothing, and failure, risk and safety evidence is kept even when cold.",
    status: "off",
    statusNote: "[pack] memory_tier_admission, off by default",
    count: 53,
  },
  {
    span: "attemptFamilyAdmission",
    name: "Attempt families",
    what: "Records whether a candidate is one of several attempts at the same task, and the discount ranking applies to it. Then a policy pass drops candidates whose text reads like instructions to the agent; the stored memories are not changed.",
    status: "always",
    code: "context_filtered_results",
    count: 52,
  },
  {
    span: "pprRerank",
    name: "Personalized PageRank",
    what: "Reranks candidates by personalized PageRank over the memory links, seeded by their search scores and computed by FrankenNetworkX: each score becomes (1 − w) × relevance + w × PPR. ee pack takes w from graph.ppr.alpha (0.30 by default); ee context also takes --ppr-weight. It is off by default, and then the stage passes candidates through untouched.",
    status: "off",
    statusNote: "graph.feature.ppr.enabled, off by default",
    code: "graph_ppr_snapshot_stale",
    count: 52,
  },
  {
    span: "packAssembly",
    name: "Select and render",
    what: "Chooses items under the profile's section quotas and the token budget, by MMR (by default 0.75 × relevance − 0.25 × redundancy; pack.mmr_lambda sets the weight) or, for the submodular profile, facility location; then renders the pack. Every candidate left out is recorded with a reason.",
    status: "always",
    count: 14,
  },
  {
    span: "selectedMemoryDrift",
    name: "Check provenance drift",
    what: "Runs inside assembly, after selection: for each chosen memory, checks whether the files and code its provenance points to have changed since it was learned, attaches a freshness facet, and reports the riskiest drift with a command to revalidate it.",
    status: "always",
    code: "memory_drift_source_changed",
    count: 14,
  },
];

const STATUS: Record<Status, { label: string; className: string }> = {
  always: { label: "every pack", className: "border-violet-300/30 text-violet-200" },
  optional: { label: "when it applies", className: "border-sky-300/30 text-sky-200" },
  off: { label: "opt-in", className: "border-white/15 text-slate-300" },
};

const STEP_MS = 1800;

/** One stage's heading, description and the degradation it can report. */
function StageDetail({ stage, index }: { stage: Stage; index: number }) {
  const status = STATUS[stage.status];
  return (
    <>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <span className="font-mono text-[11px] text-[color:var(--chapter,#a78bfa)] tabular-nums">
          {String(index + 1).padStart(2, "0")} / {STAGES.length}
        </span>
        <span className="text-lg font-extrabold tracking-[-0.02em] text-white">{stage.name}</span>
        <code className="rounded-md bg-white/[0.05] px-1.5 py-0.5 font-mono text-[11px] text-slate-300">{stage.span}</code>
        <span className={cn("rounded-full border px-2 py-0.5 text-[10px] font-black tracking-[0.12em] uppercase", status.className)}>
          {status.label}
          {stage.statusNote ? ` · ${stage.statusNote}` : ""}
        </span>
      </div>
      <p className="mt-3 max-w-4xl text-[14.5px] leading-relaxed text-slate-300">{stage.what}</p>
      {stage.code ? (
        <p className="mt-3 font-mono text-[11px] text-slate-500">
          reports, when it applies: <span className="text-amber-200/90">{stage.code}</span>
          {/* Codes only, no repairs: the figure says what a stage can report, not how to clear it. */}
        </p>
      ) : null}
    </>
  );
}

/**
 * The quality multipliers search applies to each hit after fusion (src/search/scoring.rs,
 * SearchScoreComponents::from_signals), at their defaults.
 */
export const MULTIPLIERS: readonly { name: string; rule: string; low: number; high: number; marks?: number[] }[] = [
  { name: "recency", rule: "exp(−age / 30 days), floor 0.5", low: 0.5, high: 1 },
  { name: "confidence", rule: "the memory's confidence, floor 0.1", low: 0.1, high: 1 },
  { name: "utility", rule: "0.5 + 0.5 × utility", low: 0.5, high: 1 },
  { name: "maturity", rule: "rules: retired 0 · candidate 0.5 · established 1 · proven 1.5", low: 0, high: 1.5, marks: [0, 0.5, 1, 1.5] },
  { name: "harmful", rule: "1 − 0.1 per harmful report, floor 0.2", low: 0.2, high: 1 },
  { name: "scope", rule: "× 1.2 on an exact workspace or scope match", low: 1, high: 1.2, marks: [1, 1.2] },
];
const SCALE_MAX = 1.6;

/**
 * How ee builds a pack, stage by stage: the twelve spans of its timing trace in order, what each does, and which
 * are conditional or opt-in. Below it, the two retrieval arms and the multipliers that order candidates.
 */
export function PackPipeline() {
  const [active, setActive] = useState(0);
  const [playing, setPlaying] = useState(true);
  const reduced = useReducedMotion();
  const ref = useRef<HTMLDivElement>(null);
  const inView = useInView(ref);
  const running = playing && inView && !reduced;

  useEffect(() => {
    if (!running) return;
    const timer = window.setInterval(() => setActive((at) => (at + 1) % STAGES.length), STEP_MS);
    return () => window.clearInterval(timer);
  }, [running]);

  const stage = STAGES[active] ?? STAGES[0]!;

  return (
    <DemoFrame
      eyebrow="Pack assembly · the trace, in order"
      title="Twelve of ee pack's timed stages, in order."
      basis="diagram"
      source={
        <>
          src/core/context.rs · the spans recorded by trace.record_elapsed, apply_memory_tier_candidate_admission
          (CONTEXT_MEMORY_TIER_HOT_BOOST 0.025, WARM 0.010), filter_candidates_by_instruction_authority,
          apply_personalized_pagerank_rerank (DEFAULT_CONTEXT_PPR_WEIGHT 0.30) · src/graph/ppr.rs · src/core/search.rs ·
          DEFAULT_SEARCH_LEXICAL_WEIGHT 0.45, DEFAULT_SEARCH_SEMANTIC_WEIGHT 0.45, RRF k = 60 · src/search/scoring.rs ·
          SearchScoreComponents. The trace also records embedderPrepare, the daemon handoff&apos;s
          validation, sub-spans and the total, which the figure leaves out. Candidate counts on the stages are
          illustrative.
        </>
      }
    >
      <div ref={ref}>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <p className="max-w-2xl text-[15px] leading-relaxed text-slate-300">
            <span className="font-mono text-violet-200">ee pack</span> times each of these stages and names it in its
            timing trace (<span className="font-mono text-slate-200">--explain-performance</span>). Select a stage to
            read what it does; Play steps through them.
          </p>
          {/* The label says what a press does, so no aria-pressed as well: "Pause, pressed" contradicts itself. */}
          <DemoButton
            onClick={() => setPlaying((on) => !on)}
            label={playing ? "Pause the run" : "Play the run"}
            className={cn(playing && "border-violet-400/60 text-violet-200")}
          >
            {playing ? <Pause className="h-3.5 w-3.5" aria-hidden="true" /> : <Play className="h-3.5 w-3.5" aria-hidden="true" />}
            {playing ? "Pause" : "Play"}
          </DemoButton>
        </div>

        {/* The stages: two across on a phone, then four and six. */}
        <ol className="mt-6 grid grid-cols-2 gap-2 md:grid-cols-4 xl:grid-cols-6" aria-label="Pack stages">
          {STAGES.map((item, index) => {
            const on = index === active;
            const past = index < active;
            const muted = item.status === "off";
            return (
              <li key={item.span} className="relative">
                <button
                  type="button"
                  aria-pressed={on}
                  onClick={() => {
                    setActive(index);
                    setPlaying(false);
                  }}
                  className={cn(
                    "relative flex h-full w-full flex-col overflow-hidden rounded-xl border px-3 pt-2.5 pb-3 text-left transition-[border-color,background-color,box-shadow] duration-500",
                    on
                      ? "border-[color:var(--chapter,#a78bfa)]/60 bg-[color:var(--chapter,#a78bfa)]/[0.09] shadow-[0_0_32px_-12px_var(--chapter,#a78bfa)]"
                      : "border-white/[0.08] bg-black/25 hover:border-white/20",
                  )}
                >
                  {/* A hairline that fills while this stage is the one running. */}
                  <span
                    aria-hidden="true"
                    className="absolute inset-x-0 top-0 h-[2px] origin-left bg-[color:var(--chapter,#a78bfa)]"
                    style={{
                      transform: `scaleX(${on ? 1 : past ? 1 : 0})`,
                      opacity: on ? 1 : past ? 0.3 : 0,
                      transition: on && running ? `transform ${STEP_MS}ms linear` : "transform 300ms ease, opacity 300ms ease",
                    }}
                  />
                  <span className="flex items-center justify-between gap-2">
                    <span className={cn("font-mono text-[10px] tabular-nums", on ? "text-white" : "text-slate-500")}>
                      {String(index + 1).padStart(2, "0")}
                    </span>
                    {item.status !== "always" ? (
                      <span
                        className={cn(
                          "h-1.5 w-1.5 rounded-full",
                          item.status === "off" ? "bg-slate-500" : "bg-sky-300",
                        )}
                        aria-hidden="true"
                      />
                    ) : null}
                  </span>
                  <span className={cn("mt-1.5 text-[13px] leading-snug font-bold", muted && !on ? "text-slate-400" : "text-white")}>
                    {item.name}
                  </span>
                  <span className="mt-0.5 truncate font-mono text-[10px] text-slate-500">{item.span}</span>
                  {/* Candidates still in play after this stage (illustrative). */}
                  <span className="mt-auto flex items-center gap-2 pt-3" aria-hidden="true">
                    <span className="h-1 flex-1 overflow-hidden rounded-full bg-white/[0.06]">
                      <span
                        className={cn("block h-full rounded-full", on ? "bg-[color:var(--chapter,#a78bfa)]" : "bg-white/25")}
                        style={{ width: `${(item.count / POOL) * 100}%` }}
                      />
                    </span>
                    <span className="font-mono text-[10px] text-slate-500 tabular-nums">{item.count}</span>
                  </span>
                </button>
              </li>
            );
          })}
        </ol>
        <p className="mt-2 flex flex-wrap gap-x-4 gap-y-1 font-mono text-[10px] text-slate-500">
          <span>bars: candidates remaining, illustrative</span>
          <span className="flex items-center gap-1.5">
            <span className="h-1.5 w-1.5 rounded-full bg-sky-300" aria-hidden="true" /> conditional
          </span>
          <span className="flex items-center gap-1.5">
            <span className="h-1.5 w-1.5 rounded-full bg-slate-500" aria-hidden="true" /> opt-in
          </span>
        </p>

        {/* The stage in focus. Every stage's text sits invisibly in the same grid cell, so the box is always as
            tall as the longest one and the page below it does not jump while the run plays. While it plays the
            region is not live, so a screen reader is not read a new stage every 1.8 seconds. */}
        <div className="mt-4 grid rounded-2xl border border-white/[0.08] bg-black/25 p-4 sm:p-5">
          {STAGES.map((item, index) => (
            <div key={item.span} className="invisible col-start-1 row-start-1 min-w-0" aria-hidden="true">
              <StageDetail stage={item} index={index} />
            </div>
          ))}
          <div className="col-start-1 row-start-1 min-w-0" aria-live={running ? "off" : "polite"}>
            <StageDetail stage={stage} index={active} />
          </div>
        </div>

        {/* What feeds the ranking: the two retrieval arms, then the multipliers. */}
        <div className="mt-6 grid grid-cols-1 gap-4 lg:grid-cols-2">
          <div className="rounded-2xl border border-white/[0.08] bg-black/25 p-4 sm:p-5">
            <p className="text-[9px] font-black tracking-[0.2em] text-[color:var(--chapter,#a78bfa)] uppercase">
              Search · lexical and semantic, fused
            </p>
            <div className="mt-4 grid grid-cols-[minmax(0,1fr)_2.25rem_minmax(0,0.9fr)] items-center gap-y-3">
              <div className="rounded-xl border border-sky-300/25 bg-sky-300/[0.06] px-3 py-2.5">
                <p className="text-[13px] font-bold text-sky-100">Lexical · BM25</p>
                <p className="mt-0.5 font-mono text-[10.5px] text-slate-400">exact words, names, flags</p>
              </div>
              <svg aria-hidden="true" className="row-span-2 h-full w-full" viewBox="0 0 36 100" preserveAspectRatio="none">
                <path d="M0 25 C 22 25, 14 50, 36 50" fill="none" stroke="rgba(125,211,252,0.5)" strokeWidth="1.5" vectorEffect="non-scaling-stroke" className="flow" />
                <path d="M0 75 C 22 75, 14 50, 36 50" fill="none" stroke="rgba(196,181,253,0.55)" strokeWidth="1.5" vectorEffect="non-scaling-stroke" className="flow" />
              </svg>
              <div className="row-span-2 rounded-xl border border-violet-300/30 bg-violet-400/[0.08] px-3 py-3">
                <p className="text-[13px] font-bold text-violet-100">Reciprocal-rank fusion</p>
                <p className="mt-1 font-mono text-[11px] leading-relaxed text-slate-300">Σ 1 / (60 + rank)</p>
                <p className="mt-1 font-mono text-[10.5px] text-slate-400">weights 0.45 · 0.45</p>
              </div>
              <div className="rounded-xl border border-violet-300/25 bg-violet-300/[0.06] px-3 py-2.5">
                <p className="text-[13px] font-bold text-violet-100">Semantic · Model2Vec</p>
                <p className="mt-0.5 font-mono text-[10.5px] text-slate-400">potion-multilingual-128M · 256-d</p>
              </div>
            </div>
            <p className="mt-4 text-[13px] leading-relaxed text-slate-400">
              Both searches run locally. If the model cannot load, a deterministic hash embedder replaces it and gets
              no semantic credit; lexical scores take the semantic weight, and the response records this in{" "}
              <span className="font-mono text-slate-300">degraded</span>.
            </p>
          </div>

          <div className="rounded-2xl border border-white/[0.08] bg-black/25 p-4 sm:p-5">
            <p className="text-[9px] font-black tracking-[0.2em] text-[color:var(--chapter,#a78bfa)] uppercase">
              Ordering · quality multipliers
            </p>
            <p className="mt-3 font-mono text-[11.5px] leading-relaxed [overflow-wrap:anywhere] text-slate-300">
              score = base × recency × confidence × utility × maturity × harmful × scope
              <span className="text-slate-500"> + anchor match (±0.08)</span>
            </p>
            <ul className="mt-4 space-y-2.5" aria-label="Quality multipliers and their ranges">
              {MULTIPLIERS.map((item) => (
                <li key={item.name} className="grid grid-cols-[5.25rem_minmax(0,1fr)] items-center gap-x-3 gap-y-1 sm:grid-cols-[5.25rem_minmax(0,1fr)_minmax(0,1.1fr)]">
                  <span className="font-mono text-[11px] text-white">{item.name}</span>
                  <span className="relative h-3" aria-hidden="true">
                    <span className="absolute inset-x-0 top-1/2 h-px -translate-y-1/2 bg-white/10" />
                    {/* Neutral: × 1. */}
                    <span className="absolute top-0 h-3 w-px bg-white/30" style={{ left: `${(1 / SCALE_MAX) * 100}%` }} />
                    {item.marks ? (
                      item.marks.map((mark) => (
                        <span
                          key={mark}
                          className="absolute top-1/2 h-2 w-2 -translate-x-1/2 -translate-y-1/2 rounded-full bg-[color:var(--chapter,#a78bfa)]"
                          style={{ left: `${(mark / SCALE_MAX) * 100}%` }}
                        />
                      ))
                    ) : (
                      <span
                        className="absolute top-1/2 h-1.5 -translate-y-1/2 rounded-full bg-[color:var(--chapter,#a78bfa)]/70"
                        style={{ left: `${(item.low / SCALE_MAX) * 100}%`, width: `${((item.high - item.low) / SCALE_MAX) * 100}%` }}
                      />
                    )}
                  </span>
                  <span className="col-span-2 font-mono text-[10.5px] leading-snug text-slate-400 sm:col-span-1">{item.rule}</span>
                </li>
              ))}
            </ul>
            <div
              className="mt-1 grid grid-cols-[5.25rem_minmax(0,1fr)] gap-x-3 sm:grid-cols-[5.25rem_minmax(0,1fr)_minmax(0,1.1fr)]"
              aria-hidden="true"
            >
              <span />
              <span className="relative h-3 font-mono text-[9.5px] text-slate-600">
                <span className="absolute left-0">× 0</span>
                <span className="absolute -translate-x-1/2" style={{ left: `${(1 / SCALE_MAX) * 100}%` }}>
                  × 1
                </span>
                <span className="absolute right-0">× {SCALE_MAX}</span>
              </span>
            </div>
            <p className="mt-2 text-[13px] leading-relaxed text-slate-400">
              Recency, confidence and harmful reports each have a floor, so they can lower a memory&apos;s rank but cannot
              remove it. Maturity is the only multiplier that reaches 0, for a deprecated or retired rule. Memories that
              are not rules have maturity 1.
            </p>
          </div>
        </div>
      </div>
    </DemoFrame>
  );
}
