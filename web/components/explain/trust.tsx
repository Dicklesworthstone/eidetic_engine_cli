"use client";

import { ArrowDown, ArrowUp, Pause, Play, RotateCcw, ThumbsDown, ThumbsUp, UserCheck } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";

import { DemoButton, DemoFrame, Readout, Segmented } from "@/components/explain/demo";
import { useFigureWidth, useInView, useReducedMotion } from "@/lib/hooks";
import { LEVEL_COLOUR } from "@/lib/palette";
import { cn, fixed } from "@/lib/utils";

// ─── The rule (src/core/bayes.rs) ────────────────────────────────────────────
// A memory's helpful-rate is a Beta(alpha, beta) posterior. It starts at the
// Jeffreys prior (0.5, 0.5) (DEFAULT_PRIOR_ALPHA/BETA; also the bayes_alpha and
// bayes_beta column defaults), a helpful outcome adds 1 to alpha and a harmful
// one adds DEFAULT_HARMFUL_WEIGHT (2.5) to beta. After every outcome
// src/core/outcome.rs evaluates trust_class_transition ONCE against the
// equal-tailed 90% credible interval, so a class moves at most one rung per
// outcome. The interval is found the way bayes.rs finds it: bisection on the
// regularized incomplete beta function.

const PRIOR = 0.5;
const HARMFUL_WEIGHT = 2.5;
const HELPFUL = "#38bdf8";
const HARMFUL = "#fb7185";
const HUMAN = LEVEL_COLOUR.working;

type Klass =
  | "human_explicit"
  | "peer_human_attested"
  | "agent_validated"
  | "agent_assertion"
  | "cass_evidence"
  | "legacy_import";

/** The six trust classes, top rung first, with src/models/trust.rs's initial confidences. */
const LADDER: readonly { id: Klass; short: string; initial: number; meaning: string }[] = [
  { id: "human_explicit", short: "Human", initial: 0.85, meaning: "A person ran ee remember" },
  { id: "peer_human_attested", short: "Peer", initial: 0.75, meaning: "A teammate's signed human memory" },
  { id: "agent_validated", short: "Validated", initial: 0.65, meaning: "Agent claim backed by outcomes" },
  { id: "agent_assertion", short: "Agent", initial: 0.5, meaning: "Agent claim with no outcome yet" },
  { id: "cass_evidence", short: "CASS", initial: 0.45, meaning: "An imported session span" },
  { id: "legacy_import", short: "Legacy", initial: 0.3, meaning: "Imported from pre-v1 ee" },
];

type Event = "helpful" | "harmful" | "human";

// ─── Beta arithmetic ────────────────────────────────────────────────────────

/** ln Γ(x), Lanczos approximation (g = 7). */
function lnGamma(x: number): number {
  const c = [
    0.99999999999980993, 676.5203681218851, -1259.1392167224028, 771.32342877765313, -176.61502916214059,
    12.507343278686905, -0.13857109526572012, 9.9843695780195716e-6, 1.5056327351493116e-7,
  ];
  if (x < 0.5) return Math.log(Math.PI / Math.abs(Math.sin(Math.PI * x))) - lnGamma(1 - x);
  const z = x - 1;
  let sum = c[0] as number;
  for (let i = 1; i < 9; i++) sum += (c[i] as number) / (z + i);
  const t = z + 7.5;
  return 0.5 * Math.log(2 * Math.PI) + (z + 0.5) * Math.log(t) - t + Math.log(sum);
}

/** The continued fraction of the incomplete beta function (modified Lentz). */
function betaFraction(a: number, b: number, x: number): number {
  const tiny = 1e-300;
  let c = 1;
  let d = 1 - ((a + b) * x) / (a + 1);
  if (Math.abs(d) < tiny) d = tiny;
  d = 1 / d;
  let h = d;
  for (let m = 1; m <= 300; m++) {
    const m2 = 2 * m;
    let step = (m * (b - m) * x) / ((a - 1 + m2) * (a + m2));
    d = 1 + step * d;
    if (Math.abs(d) < tiny) d = tiny;
    c = 1 + step / c;
    if (Math.abs(c) < tiny) c = tiny;
    d = 1 / d;
    h *= d * c;
    step = (-(a + m) * (a + b + m) * x) / ((a + m2) * (a + 1 + m2));
    d = 1 + step * d;
    if (Math.abs(d) < tiny) d = tiny;
    c = 1 + step / c;
    if (Math.abs(c) < tiny) c = tiny;
    d = 1 / d;
    const delta = d * c;
    h *= delta;
    if (Math.abs(delta - 1) < 3e-14) break;
  }
  return h;
}

/** I_x(a, b), the regularized incomplete beta function: the Beta CDF. */
function betaCdf(x: number, a: number, b: number): number {
  if (x <= 0) return 0;
  if (x >= 1) return 1;
  const front = Math.exp(lnGamma(a + b) - lnGamma(a) - lnGamma(b) + a * Math.log(x) + b * Math.log(1 - x));
  return x < (a + 1) / (a + b + 2) ? (front * betaFraction(a, b, x)) / a : 1 - (front * betaFraction(b, a, 1 - x)) / b;
}

/** The inverse CDF by bracketed bisection, as bayes.rs's beta_inv_cdf. */
function betaQuantile(p: number, a: number, b: number): number {
  let low = 0;
  let high = 1;
  for (let i = 0; i < 64; i++) {
    const mid = (low + high) / 2;
    if (betaCdf(mid, a, b) < p) low = mid;
    else high = mid;
  }
  return (low + high) / 2;
}

function betaDensity(x: number, a: number, b: number): number {
  return Math.exp((a - 1) * Math.log(x) + (b - 1) * Math.log(1 - x) + lnGamma(a + b) - lnGamma(a) - lnGamma(b));
}

// ─── The ADR 0032 table (trust_class_transition) ─────────────────────────────

type Direction = "promote" | "demote" | "stable";
type Transition = { next: Klass; direction: Direction; reason: string };

function transition(
  current: Klass,
  lo: number,
  hi: number,
  ess: number,
  validations: number,
  human: boolean,
): Transition {
  const up = (next: Klass, reason: string): Transition => ({ next, direction: "promote", reason });
  const down = (next: Klass, reason: string): Transition => ({ next, direction: "demote", reason });
  const stay = (reason: string): Transition => ({ next: current, direction: "stable", reason });
  switch (current) {
    case "legacy_import":
      if (lo > 0.5 && validations > 0) return up("cass_evidence", "legacy_import_promote_ci90_lower_gt_0_50_with_validation");
      if (lo > 0.5) return stay("legacy_import_validation_required");
      return stay("no_transition_threshold_crossed");
    case "cass_evidence":
      if (lo > 0.6) return up("agent_assertion", "cass_evidence_promote_ci90_lower_gt_0_60");
      if (hi < 0.3) return down("legacy_import", "cass_evidence_demote_ci90_upper_lt_0_30");
      return stay("no_transition_threshold_crossed");
    case "agent_assertion":
      if (lo > 0.7 && ess >= 6) return up("agent_validated", "agent_assertion_promote_ci90_lower_gt_0_70_sample_size_ge_6");
      if (lo > 0.7) return stay("agent_assertion_sample_size_gate_unmet");
      if (hi < 0.35) return down("cass_evidence", "agent_assertion_demote_ci90_upper_lt_0_35");
      return stay("no_transition_threshold_crossed");
    case "agent_validated":
      if (human) return up("human_explicit", "agent_validated_promote_explicit_human_operator");
      if (hi < 0.4) return down("agent_assertion", "agent_validated_demote_ci90_upper_lt_0_40");
      return stay("human_explicit_promotion_requires_operator");
    case "peer_human_attested":
      if (human) return up("human_explicit", "peer_human_attested_promote_explicit_local_operator");
      if (hi < 0.45) return down("agent_validated", "peer_human_attested_demote_ci90_upper_lt_0_45");
      return stay("local_human_promotion_requires_operator");
    case "human_explicit":
      if (hi < 0.45) return down("agent_validated", "human_explicit_demote_ci90_upper_lt_0_45");
      return stay("no_transition_threshold_crossed");
  }
}

/** One condition on the current rung: what it needs, what it has now, whether it is met. */
type Condition = { label: string; now: string; met: boolean };

function conditions(current: Klass, lo: number, hi: number, ess: number, validations: number) {
  const lower = (threshold: number): Condition => ({
    label: `CI90 lower > ${threshold.toFixed(2)}`,
    now: lo.toFixed(3),
    met: lo > threshold,
  });
  const upper = (threshold: number): Condition => ({
    label: `CI90 upper < ${threshold.toFixed(2)}`,
    now: hi.toFixed(3),
    met: hi < threshold,
  });
  const human: Condition = { label: "an explicit human promotion", now: "never automatic", met: false };
  const table: Record<Klass, { up: Condition[] | null; down: Condition[] | null }> = {
    legacy_import: {
      up: [lower(0.5), { label: "≥ 1 helpful outcome", now: String(validations), met: validations > 0 }],
      down: null,
    },
    cass_evidence: { up: [lower(0.6)], down: [upper(0.3)] },
    agent_assertion: {
      up: [lower(0.7), { label: "α + β ≥ 6", now: fixed(ess, 1), met: ess >= 6 }],
      down: [upper(0.35)],
    },
    agent_validated: { up: [human], down: [upper(0.4)] },
    peer_human_attested: { up: [human], down: [upper(0.45)] },
    human_explicit: { up: null, down: [upper(0.45)] },
  };
  return table[current];
}

type Step = {
  event: Event | null;
  alpha: number;
  beta: number;
  lo: number;
  hi: number;
  klass: Klass;
  transition: Transition | null;
};

/** Replays outcomes from the prior, one transition evaluation per outcome, as src/core/outcome.rs does. */
function replay(start: Klass, events: readonly Event[]): Step[] {
  let alpha = PRIOR;
  let beta = PRIOR;
  let klass = start;
  let validations = 0;
  const first = { lo: betaQuantile(0.05, alpha, beta), hi: betaQuantile(0.95, alpha, beta) };
  const steps: Step[] = [{ event: null, alpha, beta, ...first, klass, transition: null }];
  for (const event of events) {
    if (event === "harmful") beta += HARMFUL_WEIGHT;
    else {
      alpha += 1;
      validations += 1;
    }
    const lo = betaQuantile(0.05, alpha, beta);
    const hi = betaQuantile(0.95, alpha, beta);
    const moved = transition(klass, lo, hi, alpha + beta, validations, event === "human");
    klass = moved.next;
    steps.push({ event, alpha, beta, lo, hi, klass, transition: moved });
  }
  return steps;
}

/** The story: six helpful outcomes earn a promotion; then harm arrives. */
const STORY: readonly Event[] = [
  "helpful",
  "helpful",
  "helpful",
  "helpful",
  "helpful",
  "helpful",
  "harmful",
  "harmful",
  "harmful",
  "harmful",
  "harmful",
  "harmful",
  "harmful",
  "harmful",
];
const STORY_MS = 900;
const MAX_EVENTS = 40;

export function TrustPosterior() {
  const [start, setStart] = useState<Klass>("agent_assertion");
  const [events, setEvents] = useState<Event[]>([]);
  const [playing, setPlaying] = useState(false);
  const reduced = useReducedMotion();
  const frame = useRef<HTMLDivElement>(null);
  const inView = useInView(frame);

  const steps = useMemo(() => replay(start, events), [start, events]);
  const now = steps[steps.length - 1] as Step;
  const ess = now.alpha + now.beta;
  const mean = now.alpha / ess;
  const validations = events.filter((event) => event !== "harmful").length;
  const lastMove = [...steps].reverse().find((step) => step.transition && step.transition.direction !== "stable");
  const rung = conditions(now.klass, now.lo, now.hi, ess, validations);

  // The story plays one outcome at a time while the figure is on screen.
  useEffect(() => {
    if (!playing) return;
    if (reduced) {
      setEvents([...STORY]);
      setPlaying(false);
      return;
    }
    if (!inView) return;
    const timer = window.setInterval(() => {
      setEvents((current) => {
        if (current.length >= STORY.length) {
          setPlaying(false);
          return current;
        }
        return [...current, STORY[current.length] as Event];
      });
    }, STORY_MS);
    return () => window.clearInterval(timer);
  }, [playing, inView, reduced]);

  const add = (event: Event) => {
    setPlaying(false);
    setEvents((current) => (current.length >= MAX_EVENTS ? current : [...current, event]));
  };
  const reset = (klass = start) => {
    setPlaying(false);
    setStart(klass);
    setEvents([]);
  };
  const story = () => {
    setStart("agent_assertion");
    setEvents([]);
    setPlaying(true);
  };

  const humanAvailable = now.klass === "agent_validated" || now.klass === "peer_human_attested";

  return (
    <DemoFrame
      eyebrow="Outcome feedback · a Beta posterior per memory"
      title="How outcomes move a memory's trust class."
      basis="rule"
      source={
        <>
          src/core/bayes.rs · Jeffreys prior Beta(0.5, 0.5), helpful α + 1, harmful β + DEFAULT_HARMFUL_WEIGHT (2.5),
          equal-tailed 90% interval by bisection on I_x(α, β), trust_class_transition (ADR 0032) · src/core/outcome.rs ·
          one transition evaluated per outcome · src/models/trust.rs · initial_confidence
        </>
      }
    >
      <div ref={frame} className="grid grid-cols-1 gap-7 lg:grid-cols-12 lg:gap-8">
        <div className="min-w-0 space-y-5 lg:col-span-8">
          <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
            <span className="text-xs text-slate-400">Memory starts as</span>
            <Segmented
              label="Starting trust class"
              value={start}
              onChange={(value) => reset(value)}
              options={LADDER.map((rung) => ({ value: rung.id, label: rung.short, tone: "rule" as const }))}
            />
          </div>

          <div className="flex flex-wrap gap-2">
            <DemoButton onClick={() => add("helpful")} label="Record a helpful outcome">
              <ThumbsUp className="h-3.5 w-3.5" aria-hidden="true" style={{ color: HELPFUL }} /> Helpful
              <span className="font-mono text-[10px] text-slate-500">α + 1</span>
            </DemoButton>
            <DemoButton onClick={() => add("harmful")} label="Record a harmful outcome">
              <ThumbsDown className="h-3.5 w-3.5" aria-hidden="true" style={{ color: HARMFUL }} /> Harmful
              <span className="font-mono text-[10px] text-slate-500">β + 2.5</span>
            </DemoButton>
            <DemoButton
              onClick={() => add("human")}
              disabled={!humanAvailable}
              label="Record a helpful outcome signed by a named human with a reason"
            >
              <UserCheck className="h-3.5 w-3.5" aria-hidden="true" style={{ color: HUMAN }} /> Human sign-off
            </DemoButton>
            <span aria-hidden="true" className="mx-1 hidden w-px self-stretch bg-white/10 sm:block" />
            {/* The label already says the state, so no aria-pressed: "Pause, pressed" would announce the opposite. */}
            <DemoButton
              onClick={() => (playing ? setPlaying(false) : story())}
              className={playing ? "border-violet-400/60 text-violet-200" : undefined}
            >
              {playing ? <Pause className="h-3.5 w-3.5" aria-hidden="true" /> : <Play className="h-3.5 w-3.5" aria-hidden="true" />}
              {playing ? "Pause" : "Play an example"}
            </DemoButton>
            <DemoButton onClick={() => reset()} disabled={events.length === 0 && !playing}>
              <RotateCcw className="h-3.5 w-3.5" aria-hidden="true" /> Reset
            </DemoButton>
          </div>

          <PosteriorChart step={now} klass={now.klass} />
          <Tape steps={steps} />

          <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 xl:grid-cols-6">
            <Readout label={<><Greek>α</Greek> helpful</>} value={fixed(now.alpha, 1)} />
            <Readout label={<><Greek>β</Greek> harmful</>} value={fixed(now.beta, 1)} tone={now.beta > PRIOR ? "wrong" : "neutral"} />
            <Readout label="Mean" value={fixed(mean, 3)} note="the legacy confidence" />
            <Readout label="CI90" value={`${now.lo.toFixed(2)}–${now.hi.toFixed(2)}`} tone="rule" />
            <Readout label={<><Greek>α</Greek> + <Greek>β</Greek></>} value={fixed(ess, 1)} note="sample-size gate" />
            <Readout label="Helpful outcomes" value={validations} />
          </div>
        </div>

        <div className="min-w-0 lg:col-span-4">
          <p className="text-[10px] font-black tracking-[0.2em] text-slate-500 uppercase">The trust ladder</p>
          <ol className="mt-3 space-y-1.5">
            {LADDER.map((entry) => {
              const on = entry.id === now.klass;
              return (
                <li
                  key={entry.id}
                  aria-current={on ? "step" : undefined}
                  className={cn(
                    "rounded-xl border px-3 py-2.5 transition-[border-color,background-color] duration-500",
                    on
                      ? "border-[color:var(--chapter)]/50 bg-[color:var(--chapter)]/[0.08] shadow-[0_0_32px_-14px_var(--chapter)]"
                      : "border-white/[0.06] bg-black/20",
                  )}
                >
                  <div className="flex items-center gap-2.5">
                    <span
                      aria-hidden="true"
                      className={cn("sphere h-2.5 w-2.5 shrink-0 transition-opacity", on ? "opacity-100" : "opacity-25")}
                    />
                    <span className={cn("min-w-0 flex-1 truncate font-mono text-[12px]", on ? "text-white" : "text-slate-400")}>
                      {entry.id}
                    </span>
                    <span className="font-mono text-[10px] text-slate-500 tabular-nums" title="initial confidence">
                      {fixed(entry.initial, 2)}
                    </span>
                  </div>
                  {on ? (
                    <div className="mt-2 space-y-2 pl-5">
                      <p className="text-[12px] leading-snug text-slate-400">{entry.meaning}</p>
                      <RungConditions direction="promote" items={rung.up} />
                      <RungConditions direction="demote" items={rung.down} />
                    </div>
                  ) : null}
                </li>
              );
            })}
          </ol>
          <div className="mt-4 rounded-xl border border-white/10 bg-black/30 p-3" aria-live="polite">
            <p className="text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase">Last class change</p>
            {lastMove?.transition ? (
              <>
                <p className="mt-1.5 flex items-center gap-1.5 text-sm text-white">
                  {lastMove.transition.direction === "promote" ? (
                    <ArrowUp className="h-3.5 w-3.5 text-[color:var(--chapter)]" aria-hidden="true" />
                  ) : (
                    <ArrowDown className="h-3.5 w-3.5 text-rose-300" aria-hidden="true" />
                  )}
                  {lastMove.transition.direction === "promote" ? "Promoted" : "Demoted"} to{" "}
                  <span className="font-mono text-[12px]">{lastMove.klass}</span>
                </p>
                <p className="mt-1 font-mono text-[10px] leading-relaxed [overflow-wrap:anywhere] text-slate-500">
                  {lastMove.transition.reason}
                </p>
              </>
            ) : (
              <p className="mt-1.5 text-[12px] leading-snug text-slate-500">
                None yet. Every change writes a trust_class.transition audit row.
              </p>
            )}
          </div>
        </div>
      </div>

      <p className="mt-7 max-w-3xl text-sm leading-relaxed text-slate-300">
        A harmful outcome adds 2.5 to β and a helpful one adds 1 to α, so the mean falls faster than it rises. The
        class is decided by the 90% interval rather than the mean: promotion needs the <em>lower</em> bound above one
        threshold, demotion needs the <em>upper</em> bound below another, and between the two the class does not
        change. Outcomes alone never promote a memory to <span className="font-mono text-[13px]">human_explicit</span>;
        that step needs a named person and a reason.
      </p>
    </DemoFrame>
  );
}

/** A Greek letter inside an uppercase label: kept lower-case, or α would read as a Latin A. */
function Greek({ children }: { children: string }) {
  return <span className="text-[11px] tracking-normal normal-case">{children}</span>;
}

function RungConditions({ direction, items }: { direction: "promote" | "demote"; items: Condition[] | null }) {
  const up = direction === "promote";
  return (
    <div>
      <p className="flex items-center gap-1 text-[10px] font-bold tracking-[0.14em] text-slate-500 uppercase">
        {up ? <ArrowUp className="h-3 w-3" aria-hidden="true" /> : <ArrowDown className="h-3 w-3" aria-hidden="true" />}
        {up ? "Promote when" : "Demote when"}
      </p>
      {items ? (
        <ul className="mt-1 space-y-0.5">
          {items.map((item) => (
            <li key={item.label} className="flex flex-wrap items-baseline justify-between gap-x-2 text-[12px]">
              <span className={item.met ? (up ? "text-[color:var(--chapter)]" : "text-rose-300") : "text-slate-300"}>
                {item.met ? "✓ " : ""}
                {item.label}
              </span>
              <span className="font-mono text-[11px] text-slate-500 tabular-nums">now {item.now}</span>
            </li>
          ))}
        </ul>
      ) : (
        <p className="mt-1 text-[12px] text-slate-500">{up ? "The top rung." : "The bottom rung."}</p>
      )}
    </div>
  );
}

const CHART_HEIGHT = 210;
const PAD = { left: 10, right: 10, top: 26, bottom: 30 };

/** The posterior density, its 90% interval shaded, the mean, and the current rung's thresholds. */
function PosteriorChart({ step, klass }: { step: Step; klass: Klass }) {
  const { ref, width } = useFigureWidth(640);
  const plotW = Math.max(120, width - PAD.left - PAD.right);
  const plotH = CHART_HEIGHT - PAD.top - PAD.bottom;
  const { alpha, beta, lo, hi } = step;
  const mean = alpha / (alpha + beta);

  const { line, area } = useMemo(() => {
    const samples: [number, number][] = [];
    for (let i = 0; i <= 240; i++) {
      const x = 0.002 + (0.996 * i) / 240;
      samples.push([x, betaDensity(x, alpha, beta)]);
    }
    // Scale to the density away from the edges, where a Jeffreys-shaped posterior runs to infinity.
    const inner = samples.filter(([x]) => x > 0.03 && x < 0.97).map(([, y]) => y);
    const top = Math.min(16, Math.max(1.6, ...inner) * 1.12);
    const sx = (x: number) => PAD.left + x * plotW;
    const sy = (y: number) => PAD.top + plotH - (Math.min(y, top) / top) * plotH;
    const path = samples.map(([x, y], i) => `${i === 0 ? "M" : "L"}${sx(x).toFixed(1)},${sy(y).toFixed(1)}`).join("");
    const band = samples.filter(([x]) => x >= lo && x <= hi);
    const shaded =
      band.length > 1
        ? `M${sx(lo).toFixed(1)},${sy(0).toFixed(1)}` +
          band.map(([x, y]) => `L${sx(x).toFixed(1)},${sy(y).toFixed(1)}`).join("") +
          `L${sx(hi).toFixed(1)},${sy(0).toFixed(1)}Z`
        : "";
    return { line: path, area: shaded };
  }, [alpha, beta, lo, hi, plotW, plotH]);

  const sx = (x: number) => PAD.left + x * plotW;
  const base = PAD.top + plotH;
  const rungThresholds: Record<Klass, { up?: number; down?: number }> = {
    legacy_import: { up: 0.5 },
    cass_evidence: { up: 0.6, down: 0.3 },
    agent_assertion: { up: 0.7, down: 0.35 },
    agent_validated: { down: 0.4 },
    peer_human_attested: { down: 0.45 },
    human_explicit: { down: 0.45 },
  };
  const marks = rungThresholds[klass];

  return (
    <div ref={ref} className="rounded-2xl border border-white/[0.07] bg-black/30">
      <svg
        className="diagram block h-auto w-full"
        width={width}
        height={CHART_HEIGHT}
        viewBox={`0 0 ${width} ${CHART_HEIGHT}`}
        role="img"
        aria-label={`Posterior Beta(${fixed(alpha, 1)}, ${fixed(beta, 1)}): mean ${fixed(mean, 2)}, 90% interval ${fixed(lo, 2)} to ${fixed(hi, 2)}.`}
      >
        {[0, 0.25, 0.5, 0.75, 1].map((tick) => (
          <g key={tick}>
            <line x1={sx(tick)} x2={sx(tick)} y1={PAD.top} y2={base} stroke="rgba(255,255,255,0.05)" />
            <text x={sx(tick)} y={base + 16} textAnchor={tick === 0 ? "start" : tick === 1 ? "end" : "middle"} className="mono" fontSize="10" fill="#77839c">
              {tick}
            </text>
          </g>
        ))}
        <text x={sx(0.5)} y={CHART_HEIGHT - 2} textAnchor="middle" fontSize="10" fill="#77839c">
          helpful rate
        </text>
        {area ? <path d={area} style={{ fill: "var(--chapter)" }} opacity={0.16} /> : null}
        <path d={line} fill="none" style={{ stroke: "var(--chapter)" }} strokeWidth={2} strokeLinejoin="round" />
        {/* The interval's ends. */}
        {[lo, hi].map((x, index) => (
          <line key={index} x1={sx(x)} x2={sx(x)} y1={PAD.top + 6} y2={base} style={{ stroke: "var(--chapter)" }} strokeWidth={1} opacity={0.7} />
        ))}
        {/* The current rung's thresholds: the lower bound must clear `up`, the upper bound must fall under `down`. */}
        {/* On a narrow chart the two labels can meet in the middle; drop the demotion label a line. */}
        {marks.up !== undefined ? (
          <Threshold x={sx(marks.up)} top={PAD.top} base={base} colour="#e2e8f0" label={`lower > ${marks.up.toFixed(2)}`} met={lo > marks.up} anchor="end" />
        ) : null}
        {marks.down !== undefined ? (
          <Threshold x={sx(marks.down)} top={PAD.top} base={base} colour={HARMFUL} label={`upper < ${marks.down.toFixed(2)}`} met={hi < marks.down} anchor="start" drop={marks.up !== undefined && sx(marks.up) - sx(marks.down) < 150} />
        ) : null}
        <line x1={sx(mean)} x2={sx(mean)} y1={PAD.top - 4} y2={base} stroke="#fff" strokeWidth={1.5} strokeDasharray="2 3" />
        <text x={sx(mean)} y={PAD.top - 9} textAnchor={mean > 0.85 ? "end" : mean < 0.15 ? "start" : "middle"} className="mono" fontSize="10" fill="#fff">
          mean {fixed(mean, 2)}
        </text>
        <line x1={PAD.left} x2={PAD.left + plotW} y1={base} y2={base} stroke="rgba(255,255,255,0.18)" />
      </svg>
    </div>
  );
}

function Threshold({
  x,
  top,
  base,
  colour,
  label,
  met,
  anchor,
  drop = false,
}: {
  x: number;
  top: number;
  base: number;
  colour: string;
  label: string;
  met: boolean;
  anchor: "start" | "end";
  drop?: boolean;
}) {
  return (
    <g opacity={met ? 1 : 0.55}>
      <line x1={x} x2={x} y1={top + 14} y2={base} stroke={colour} strokeWidth={1} strokeDasharray="4 4" />
      <text x={anchor === "end" ? x - 5 : x + 5} y={top + (drop ? 36 : 22)} textAnchor={anchor} className="mono" fontSize="10" fill={colour}>
        {label}
      </text>
    </g>
  );
}

/** The outcomes so far, each as wide as its weight, with the class changes marked above them. */
function Tape({ steps }: { steps: Step[] }) {
  const outcomes = steps.slice(1);
  return (
    <div className="rounded-2xl border border-white/[0.07] bg-black/20 px-3 py-3">
      <div className="flex items-center justify-between gap-3">
        <p className="text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase">Outcomes, sized by weight</p>
        <p className="flex items-center gap-3 font-mono text-[10px] text-slate-500">
          <span className="flex items-center gap-1">
            <span aria-hidden="true" className="inline-block h-2 w-2 rounded-sm" style={{ background: HELPFUL }} /> 1
          </span>
          <span className="flex items-center gap-1">
            <span aria-hidden="true" className="inline-block h-2 w-5 rounded-sm" style={{ background: HARMFUL }} /> 2.5
          </span>
        </p>
      </div>
      <div className="mt-2.5 flex min-h-[2.4rem] flex-wrap items-end gap-x-[3px] gap-y-2" aria-label={`${outcomes.length} outcomes recorded`} role="list">
        {outcomes.length === 0 ? (
          <span className="text-[12px] text-slate-500">No outcomes yet: the prior, Beta(0.5, 0.5).</span>
        ) : (
          outcomes.map((step, index) => {
            const moved = step.transition && step.transition.direction !== "stable" ? step.transition.direction : null;
            const colour = step.event === "harmful" ? HARMFUL : step.event === "human" ? HUMAN : HELPFUL;
            return (
              <span key={index} role="listitem" className="type-in flex flex-col items-center gap-1" aria-label={`${step.event}${moved ? `, ${moved}d to ${step.klass}` : ""}`}>
                <span aria-hidden="true" className={cn("h-3 text-[10px] leading-3", moved === "promote" ? "text-[color:var(--chapter)]" : "text-rose-300")}>
                  {moved === "promote" ? "▲" : moved === "demote" ? "▼" : ""}
                </span>
                <span
                  aria-hidden="true"
                  className="block h-3.5 rounded-[3px]"
                  style={{ width: step.event === "harmful" ? 25 : 10, background: colour, opacity: 0.85 }}
                />
              </span>
            );
          })
        )}
      </div>
    </div>
  );
}
