"use client";

import { ChevronLeft, ChevronRight, Pause, Play, RotateCcw, SkipForward } from "lucide-react";
import { type PointerEvent as ReactPointerEvent, useEffect, useId, useMemo, useRef, useState } from "react";

import {
  BUDGET,
  type Campaign,
  type CampaignEvent,
  DAYS,
  DOMAINS,
  type DomainId,
  type EventType,
  LEVELS_BOTTOM_UP,
  mondayOf,
  type Pack,
  type PackLine,
  packAt,
  SECTION_KEY,
  type SectionPack,
  simulate,
  storeTokensAt,
  totalAt,
  type Trust,
  weekOf,
} from "@/components/explain/campaign-model";
import { DemoButton, DemoFrame, Readout, Segmented, Slider } from "@/components/explain/demo";
import { Cmd } from "@/components/instrument";
import { useFigureWidth, useInView, useReducedMotion } from "@/lib/hooks";
import { EVIDENCE, LEVEL_COLOUR, type Level, type PackSection, SECTION_COLOUR } from "@/lib/palette";
import { clamp, cn, grouped } from "@/lib/utils";

/**
 * Twenty-six weeks of one campaign, in five domains: the store's memories by level, the events that move them, and the
 * pack a Monday session would get. A toy (components/explain/campaign-model.ts): the campaigns are invented and
 * simulated here; the half-lives, the forget threshold, the budget, the quotas and the trust classes are ee's.
 */

const CAMPAIGNS = Object.fromEntries(DOMAINS.map((domain) => [domain.id, simulate(domain)])) as Record<DomainId, Campaign>;

const DOW = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"] as const;
const when = (day: number) => `Week ${weekOf(day)} · ${DOW[Math.floor(day) % 7] ?? "Mon"}`;

/** Days of campaign per millisecond of playback, and how long playback rests on each event. */
const SPEED = 11 / 1000;
const HOLD_MS = 1100;

const EVENT_STYLE: Record<EventType, { label: string; colour: string }> = {
  import: { label: "Playbook import", colour: LEVEL_COLOUR.procedural },
  promote: { label: "Rules promoted", colour: LEVEL_COLOUR.procedural },
  antipattern: { label: "Anti-pattern", colour: LEVEL_COLOUR.procedural },
  validated: { label: "Trust moves up", colour: EVIDENCE },
  decision: { label: "Decision recorded", colour: SECTION_COLOUR.decisions },
  revisit: { label: "Revisit due", colour: SECTION_COLOUR.decisions },
  demote: { label: "Rule deprecated", colour: "#fb7185" },
  tombstone: { label: "Decay sweep", colour: "#94a3b8" },
};

/** The legend, top of the stack first, with each level's half-life (MemoryDecayHalfLives::default). */
const LEGEND: readonly { level: Level; half: string }[] = [
  { level: "working", half: "1 d" },
  { level: "episodic", half: "30 / 90 d" },
  { level: "semantic", half: "180 d" },
  { level: "procedural", half: "365 d" },
];

const TRUST_STYLE: Record<Trust, string> = {
  human_explicit: "border-amber-300/35 bg-amber-300/10 text-amber-200",
  peer_human_attested: "border-orange-300/35 bg-orange-300/10 text-orange-200",
  agent_validated: "border-emerald-300/35 bg-emerald-300/10 text-emerald-200",
  agent_assertion: "border-white/15 bg-white/[0.04] text-slate-300",
  cass_evidence: "border-cyan-300/30 bg-cyan-300/10 text-cyan-200",
  legacy_import: "border-white/10 bg-white/[0.02] text-slate-500",
};

const EMPTY: Record<PackSection, string> = {
  rules: "Nothing yet: no rule has been earned.",
  decisions: "No decision recorded yet.",
  failures: "No failure recorded yet.",
  evidence: "No evidence yet.",
  artifacts: "No artifact registered yet.",
};

export function CampaignTimeline() {
  const frame = useRef<HTMLDivElement>(null);
  const reduced = useReducedMotion();
  const inView = useInView(frame);
  const [domainId, setDomainId] = useState<DomainId>("perf");
  // The HTML (and a reader who asks for less motion) sees the whole campaign; the first time the figure comes into
  // view it starts again from week one and plays.
  const [day, setDay] = useState(DAYS - 1);
  const [playing, setPlaying] = useState(false);
  const started = useRef(false);
  const clock = useRef(day);
  const campaign = CAMPAIGNS[domainId];
  const { domain, timeline } = campaign;

  useEffect(() => {
    clock.current = day;
  });

  useEffect(() => {
    if (!inView || reduced || started.current) return;
    started.current = true;
    setDay(0);
    clock.current = 0;
    setPlaying(true);
  }, [inView, reduced]);

  // Playback: a day every ~90 ms, resting on each event so its card can be read. Stops off screen.
  useEffect(() => {
    if (!playing || reduced || !inView) return;
    let raf = 0;
    let last = performance.now();
    let holdUntil = 0;
    const tick = (now: number) => {
      const elapsed = Math.min(80, now - last);
      last = now;
      if (now >= holdUntil) {
        const current = clock.current;
        let next = Math.min(DAYS - 1, current + elapsed * SPEED);
        const crossed = timeline.find((event) => event.day > current && event.day <= next);
        if (crossed) {
          next = crossed.day;
          holdUntil = now + HOLD_MS;
        }
        clock.current = next;
        setDay(next);
        if (next >= DAYS - 1) {
          setPlaying(false);
          return;
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [playing, reduced, inView, timeline]);

  const whole = Math.floor(day);
  const monday = mondayOf(whole);
  const pack = useMemo(() => packAt(campaign, monday), [campaign, monday]);
  const latest = lastIndexAtOrBefore(timeline, day);
  const atEnd = day >= DAYS - 1;
  const animating = playing && !reduced;

  const jump = (to: number) => {
    setPlaying(false);
    const next = clamp(to, 0, DAYS - 1);
    clock.current = next;
    setDay(next);
  };
  const toggle = () => {
    if (reduced) {
      jump(mondayOf(whole) + 7);
      return;
    }
    if (atEnd) {
      clock.current = 0;
      setDay(0);
      setPlaying(true);
      return;
    }
    setPlaying((on) => !on);
  };
  const restart = () => {
    clock.current = 0;
    setDay(0);
    setPlaying(!reduced);
  };

  const total = totalAt(campaign, whole);
  const storeK = Math.round(storeTokensAt(campaign, whole) / 1000);

  return (
    <DemoFrame
      eyebrow="Five campaigns · 26 weeks each"
      title="The store grows. The pack keeps its size."
      basis="toy"
      source={
        <>
          A toy: five invented campaigns simulated in your browser, nothing measured; the pack is ranked by trust and
          recency here, not by ee&rsquo;s MMR. ee&rsquo;s own: src/policy/memory_decay.rs · MemoryDecayHalfLives::default
          (working 1 d, episodic event 30 d, failure 90 d, semantic 180 d, procedural 365 d) ·
          DEFAULT_DECAY_FORGET_THRESHOLD 0.01 · src/pack/mod.rs · DEFAULT_CONTEXT_MAX_TOKENS 4000 ·
          src/models/context_profile.rs · balanced mix 30/20/20/20/10 · src/models/trust.rs · TrustClass ·
          src/core/rule.rs · PLAYBOOK_MIN_EVIDENCE 3, portable_import_trust_class · src/core/outcome.rs ·
          ANTI_PATTERN_PROPOSAL_THRESHOLD 3 from two sources · src/models/rule.rs · has_harmful_quorum (deprecation
          through curation) · src/core/bayes.rs · trust_class_transition · src/cli/mod.rs · ee decide record
          --revisit-by, ee decide revisit, ee maintenance run --include-decay · README.md · Beyond Coding, Negative
          Evidence Ledger, Memory Model
        </>
      }
    >
      <div ref={frame}>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <Segmented
            label="Campaign"
            value={domainId}
            onChange={setDomainId}
            options={DOMAINS.map((entry) => ({ value: entry.id, label: entry.label }))}
          />
          <div className="flex flex-wrap gap-2">
            <DemoButton
              onClick={toggle}
              pressed={animating}
              label={reduced ? "Step to next week" : animating ? "Pause the campaign" : "Play the campaign"}
            >
              {reduced ? (
                <SkipForward className="h-3.5 w-3.5" aria-hidden="true" />
              ) : animating ? (
                <Pause className="h-3.5 w-3.5" aria-hidden="true" />
              ) : (
                <Play className="h-3.5 w-3.5" aria-hidden="true" />
              )}
              {reduced ? "Next week" : animating ? "Pause" : "Play"}
            </DemoButton>
            <DemoButton onClick={restart} label="Restart from week one">
              <RotateCcw className="h-3.5 w-3.5" aria-hidden="true" /> Restart
            </DemoButton>
          </div>
        </div>

        <div className="mt-5 flex flex-wrap items-baseline gap-x-3 gap-y-1">
          <p className="text-[15px] font-bold text-white">{domain.title}</p>
          <p className="text-[12.5px] text-slate-400">{domain.who}</p>
          <p className="font-mono text-[11px] [overflow-wrap:anywhere] text-slate-500">
            {domain.workspace}
            {domain.isolated ? " · memory.participate = false" : ""}
          </p>
        </div>

        <div className="mt-5 grid grid-cols-1 gap-6 lg:grid-cols-[minmax(0,1fr)_22.5rem]">
          <div className="min-w-0">
            <ul className="flex flex-wrap items-center gap-x-4 gap-y-1.5 text-[11.5px] text-slate-400">
              <li className="font-mono text-[9.5px] tracking-[0.16em] text-slate-500 uppercase">Memories live</li>
              {LEGEND.map(({ level, half }) => (
                <li key={level} className="flex items-center gap-1.5">
                  <span aria-hidden="true" className="h-2.5 w-2.5 rounded-[3px]" style={{ background: LEVEL_COLOUR[level] }} />
                  <span className="text-slate-300">{level}</span>
                  <span className="font-mono text-white tabular-nums">{campaign.levels[level][whole] ?? 0}</span>
                  <span className="font-mono text-[10px] text-slate-500">t½ {half}</span>
                </li>
              ))}
            </ul>

            <Chart campaign={campaign} cursor={day} latest={latest} onScrub={jump} />

            <Slider
              className="mt-3"
              label="Scrub the campaign"
              min={0}
              max={DAYS - 1}
              value={whole}
              onChange={jump}
              display={when(whole)}
            />

            <div className="mt-4 grid grid-cols-2 gap-2 sm:grid-cols-4 lg:grid-cols-2 xl:grid-cols-4">
              <Readout
                label="Memories live"
                value={grouped(total)}
                note={`${grouped(campaign.written[whole] ?? 0)} written · ${grouped(campaign.retired[whole] ?? 0)} expired or tombstoned`}
              />
              <Readout
                label="Procedural rules"
                value={campaign.rules[whole] ?? 0}
                note={`+${campaign.promoted[whole] ?? 0} promoted · −${campaign.deprecated[whole] ?? 0} deprecated`}
                tone="rule"
              />
              <Readout
                label="Pack tokens"
                value={grouped(pack.used)}
                note={`of a fixed ${grouped(BUDGET)}; the store holds ≈ ${storeK}k`}
              />
              <Readout
                label="Dead ends avoided"
                value={campaign.avoided[whole] ?? 0}
                note="a known failure surfaced before a retry"
              />
            </div>

            <EventCard campaign={campaign} latest={latest} onJump={jump} />
            <EventList timeline={timeline} latest={latest} cursor={day} onJump={jump} />
          </div>

          <PackCard campaign={campaign} pack={pack} />
        </div>

        <div className="mt-7 grid grid-cols-1 gap-3 text-[13px] leading-relaxed text-slate-400 md:grid-cols-3">
          <p>
            <span className="font-semibold text-slate-200">Lessons become rules.</span> Three notes carrying one lesson make a
            candidate (<Cmd>ee playbook extract</Cmd>); a reviewer validates and applies it. Three harmful outcomes from two
            sources propose an anti-pattern.
          </p>
          <p>
            <span className="font-semibold text-slate-200">Decisions come due.</span> <Cmd>ee decide record</Cmd> keeps the
            choice, the alternatives and the reason. With <Cmd>--revisit-by +90d</Cmd> it comes back in{" "}
            <Cmd>ee decide revisit</Cmd>, and the new decision supersedes the old.
          </p>
          <p>
            <span className="font-semibold text-slate-200">Forgetting keeps the pack sharp.</span> Scratch expires when a
            session closes, unrecalled notes are tombstoned by the decay sweep, and a rule that starts doing harm is
            deprecated. However large the store grows, the pack stays within its budget.
          </p>
        </div>
      </div>
    </DemoFrame>
  );
}

/** The index of the last event on or before `day`, or -1. */
function lastIndexAtOrBefore(timeline: readonly CampaignEvent[], day: number): number {
  let found = -1;
  timeline.forEach((event, index) => {
    if (event.day <= day + 1e-9) found = index;
  });
  return found;
}

// ─── The chart ───────────────────────────────────────────────────────────────

function niceMax(value: number): number {
  return Math.max(50, Math.ceil((value * 1.08) / 50) * 50);
}

function Chart({
  campaign,
  cursor,
  latest,
  onScrub,
}: {
  campaign: Campaign;
  cursor: number;
  latest: number;
  onScrub: (day: number) => void;
}) {
  const { ref, width } = useFigureWidth(760);
  const uid = useId().replace(/[^a-zA-Z0-9_-]/g, "");
  const W = Math.max(280, width);
  const compact = W < 560;

  const geometry = useMemo(() => {
    const m = { left: compact ? 30 : 36, right: 9 };
    const plotW = W - m.left - m.right;
    const x = (day: number) => m.left + (day / (DAYS - 1)) * plotW;

    // Event pins, in rows wherever two would touch.
    const rowEnds: number[] = [];
    const pins = campaign.timeline.map((event, index) => {
      const px = x(event.day);
      let row = rowEnds.findIndex((end) => px - end >= 17);
      if (row < 0) row = rowEnds.length < 3 ? rowEnds.length : 0;
      rowEnds[row] = px;
      return { event, index, x: px, row };
    });
    const rows = Math.max(1, rowEnds.length);
    const plotTop = 6 + rows * 17 + 6;
    const plotH = compact ? 176 : 244;
    const plotBottom = plotTop + plotH;
    const laneTop = plotBottom + 12;
    const laneH = compact ? 40 : 46;
    const laneBottom = laneTop + laneH;
    const axisY = laneBottom + 16;
    const H = axisY + 8;

    const yMax = niceMax(campaign.maxTotal);
    const y = (value: number) => plotBottom - (value / yMax) * plotH;
    const ruleMax = Math.max(4, campaign.maxRules + 1);
    const yr = (value: number) => laneBottom - 3 - (value / ruleMax) * (laneH - 12);

    // The stack, bottom up: procedural bedrock, then semantic, episodic, and working scratch on top.
    let lower = new Array<number>(DAYS).fill(0);
    const layers = LEVELS_BOTTOM_UP.map((level) => {
      const upper = lower.map((base, day) => base + (campaign.levels[level][day] ?? 0));
      let line = "";
      upper.forEach((value, day) => {
        line += `${day === 0 ? "M" : "L"}${x(day).toFixed(1)} ${y(value).toFixed(1)}`;
      });
      let back = "";
      for (let day = DAYS - 1; day >= 0; day--) back += `L${x(day).toFixed(1)} ${y(lower[day] ?? 0).toFixed(1)}`;
      const layer = { level, area: `${line}${back}Z`, line, upper };
      lower = upper;
      return layer;
    });

    // The rules lane: procedural rules in force, a step at each promotion or deprecation.
    const rules = campaign.rules;
    let laneLine = `M${x(0).toFixed(1)} ${yr(rules[0] ?? 0).toFixed(1)}`;
    const steps: { x: number; y: number; delta: number }[] = [];
    for (let day = 1; day < DAYS; day++) {
      const before = rules[day - 1] ?? 0;
      const now = rules[day] ?? 0;
      if (now !== before) {
        laneLine += ` H${x(day).toFixed(1)} V${yr(now).toFixed(1)}`;
        steps.push({ x: x(day), y: yr(Math.max(now, before)), delta: now - before });
      }
    }
    if ((rules[0] ?? 0) > 0) steps.unshift({ x: x(0), y: yr(rules[0] ?? 0), delta: rules[0] ?? 0 });
    laneLine += ` H${x(DAYS - 1).toFixed(1)}`;
    const laneArea = `${laneLine} V${laneBottom} H${x(0).toFixed(1)} Z`;

    const step = yMax > 300 ? 100 : 50;
    const yTicks: number[] = [];
    for (let value = 0; value <= yMax; value += step) yTicks.push(value);
    const weekStep = compact ? 4 : 2;
    const weekTicks: number[] = [];
    for (let week = 0; week < 26; week += weekStep) weekTicks.push(week);

    return { m, plotW, x, y, yr, pins, rows, plotTop, plotBottom, laneTop, laneBottom, axisY, H, layers, laneLine, laneArea, steps, yTicks, weekTicks };
  }, [campaign, W, compact]);

  const { m, x, y, yr, pins, plotTop, plotBottom, laneTop, laneBottom, axisY, H, layers } = geometry;
  const whole = Math.floor(cursor);
  const cx = x(cursor);
  const tagX = clamp(cx, m.left + 22, W - m.right - 22);

  const scrub = (event: ReactPointerEvent<SVGSVGElement>) => {
    const box = event.currentTarget.getBoundingClientRect();
    if (box.width <= 0) return;
    const px = ((event.clientX - box.left) / box.width) * W;
    onScrub(Math.round(((px - m.left) / geometry.plotW) * (DAYS - 1)));
  };

  const counts = LEVELS_BOTTOM_UP.map((level) => `${level} ${campaign.levels[level][whole] ?? 0}`).join(", ");
  const label = `${campaign.domain.title}: memories by level over 26 weeks, with ${campaign.timeline.length} events marked. At ${when(whole)}: ${counts}; ${campaign.rules[whole] ?? 0} procedural rules in force.`;

  return (
    <div ref={ref} className="mt-3 w-full">
      <svg
        className="diagram block h-auto w-full cursor-crosshair select-none"
        width={W}
        height={H}
        viewBox={`0 0 ${W} ${H}`}
        role="img"
        aria-label={label}
        style={{ touchAction: "pan-y" }}
        onPointerDown={(event) => {
          event.currentTarget.setPointerCapture(event.pointerId);
          scrub(event);
        }}
        onPointerMove={(event) => {
          if (event.currentTarget.hasPointerCapture(event.pointerId)) scrub(event);
        }}
      >
        <defs>
          {LEVELS_BOTTOM_UP.map((level) => (
            <linearGradient key={level} id={`${uid}-g-${level}`} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor={LEVEL_COLOUR[level]} stopOpacity="0.92" />
              <stop offset="100%" stopColor={LEVEL_COLOUR[level]} stopOpacity="0.42" />
            </linearGradient>
          ))}
          <clipPath id={`${uid}-past`}>
            <rect x="0" y="0" width={Math.max(0, cx)} height={H} />
          </clipPath>
        </defs>

        {/* Bursts: kickoffs, incidents, prints, deadlines. */}
        {campaign.domain.bursts.map(([from, to, , name]) => (
          <g key={`${name}-${from}`}>
            <rect x={x(from)} y={plotTop} width={Math.max(2, x(to) - x(from))} height={laneBottom - plotTop} fill="rgba(255,255,255,0.028)" />
            {!compact ? (
              <text x={x(from) + 4} y={plotTop + 11} className="mono" fontSize="8.5" letterSpacing="0.8" fill="#5b6478">
                {name.toUpperCase()}
              </text>
            ) : null}
          </g>
        ))}

        {/* Grid */}
        {geometry.yTicks.map((value) => (
          <g key={value}>
            <line x1={m.left} x2={W - m.right} y1={y(value)} y2={y(value)} stroke="rgba(255,255,255,0.05)" />
            <text x={m.left - 6} y={y(value) + 3.5} textAnchor="end" className="mono" fontSize="10" fill="#77839c">
              {value}
            </text>
          </g>
        ))}
        {geometry.weekTicks.map((week) => (
          <g key={week}>
            <line x1={x(week * 7)} x2={x(week * 7)} y1={plotTop} y2={laneBottom} stroke="rgba(255,255,255,0.035)" />
            {/* A label the cursor's pill would cover is left out rather than half-hidden behind it. */}
            {Math.abs(x(week * 7) - tagX) >= 36 && (
              <text x={x(week * 7)} y={axisY + 3.5} textAnchor="middle" className="mono" fontSize="10" fill="#77839c">
                W{week + 1}
              </text>
            )}
          </g>
        ))}

        {/* The stack: the future faint, the past lit. */}
        <g opacity="0.15">
          {layers.map((layer) => (
            <path key={layer.level} d={layer.area} fill={LEVEL_COLOUR[layer.level]} />
          ))}
        </g>
        <g clipPath={`url(#${uid}-past)`}>
          {layers.map((layer) => (
            <path key={layer.level} d={layer.area} fill={`url(#${uid}-g-${layer.level})`} />
          ))}
          {layers.map((layer) => (
            <path key={`${layer.level}-edge`} d={layer.line} fill="none" stroke={LEVEL_COLOUR[layer.level]} strokeWidth="1.1" strokeLinejoin="round" />
          ))}
        </g>
        {/* The rules lane */}
        <line x1={m.left} x2={W - m.right} y1={laneBottom} y2={laneBottom} stroke="rgba(255,255,255,0.08)" />
        <path d={geometry.laneArea} fill={LEVEL_COLOUR.procedural} opacity="0.1" />
        <path d={geometry.laneLine} fill="none" stroke={LEVEL_COLOUR.procedural} strokeOpacity="0.3" strokeWidth="1.5" />
        <g clipPath={`url(#${uid}-past)`}>
          <path d={geometry.laneArea} fill={LEVEL_COLOUR.procedural} opacity="0.28" />
          <path d={geometry.laneLine} fill="none" stroke={LEVEL_COLOUR.procedural} strokeWidth="2" />
        </g>
        <text x={m.left + 4} y={laneTop + 9} className="mono" fontSize="9" letterSpacing="1" fill="#77839c">
          PROCEDURAL RULES IN FORCE
        </text>
        {!compact
          ? geometry.steps.map((entry) => (
              <text
                key={`${entry.x}-${entry.delta}`}
                x={entry.x + 4}
                y={entry.y - 3}
                className="mono"
                fontSize="9.5"
                fontWeight="600"
                fill={entry.delta > 0 ? "#c4b5fd" : "#fda4af"}
              >
                {entry.delta > 0 ? `+${entry.delta}` : `−${-entry.delta}`}
              </text>
            ))
          : null}

        {/* Event pins */}
        {pins.map((pin) => {
          const past = pin.event.day <= cursor + 1e-9;
          const current = pin.index === latest;
          const colour = EVENT_STYLE[pin.event.type].colour;
          const cy = 6 + pin.row * 17 + 7;
          return (
            <g key={pin.index} opacity={past ? 1 : 0.42}>
              <title>{`${when(pin.event.day)} · ${pin.event.title}`}</title>
              <line x1={pin.x} x2={pin.x} y1={cy + 7} y2={laneBottom} stroke={colour} strokeOpacity={past ? 0.45 : 0.25} strokeDasharray="2 3" />
              {current ? <circle cx={pin.x} cy={cy} r="10.5" fill="none" stroke={colour} strokeOpacity="0.55" /> : null}
              <circle cx={pin.x} cy={cy} r="7.5" fill={past ? colour : "#05040d"} stroke={colour} strokeWidth="1.25" />
              <text x={pin.x} y={cy + 3.2} textAnchor="middle" className="mono" fontSize="9" fontWeight="700" fill={past ? "#05040d" : colour}>
                {pin.index + 1}
              </text>
            </g>
          );
        })}

        {/* The cursor */}
        <line x1={cx} x2={cx} y1={plotTop} y2={laneBottom} stroke="#e2e8f0" strokeOpacity="0.6" />
        {layers.map((layer) => (
          <circle
            key={layer.level}
            cx={cx}
            cy={y(layer.upper[whole] ?? 0)}
            r="3.5"
            fill={LEVEL_COLOUR[layer.level]}
            stroke="#05040d"
            strokeWidth="1.5"
          />
        ))}
        <circle cx={cx} cy={yr(campaign.rules[whole] ?? 0)} r="3.5" fill={LEVEL_COLOUR.procedural} stroke="#05040d" strokeWidth="1.5" />
        <rect x={tagX - 22} y={axisY - 9} width="44" height="17" rx="8.5" style={{ fill: "var(--chapter)" }} stroke="#05040d" strokeWidth="2" />
        <text x={tagX} y={axisY + 3.5} textAnchor="middle" className="mono" fontSize="10" fontWeight="700" fill="#05040d">
          W{weekOf(whole)}
        </text>
      </svg>
    </div>
  );
}

// ─── The events ──────────────────────────────────────────────────────────────

function EventCard({ campaign, latest, onJump }: { campaign: Campaign; latest: number; onJump: (day: number) => void }) {
  const { timeline, domain } = campaign;
  const event = timeline[latest];
  const next = timeline[latest + 1];
  const previous = latest > 0 ? timeline[latest - 1] : undefined;
  const style = event ? EVENT_STYLE[event.type] : { label: "Week one", colour: "#94a3b8" };
  return (
    <div className="mt-4 rounded-xl border border-white/10 bg-black/30 p-3.5 sm:p-4" aria-live="polite">
      <div className="flex items-center justify-between gap-3">
        <div className="flex min-w-0 flex-wrap items-center gap-x-2.5 gap-y-1">
          <span
            className="inline-flex items-center gap-1.5 rounded-full border px-2 py-0.5 text-[10px] font-black tracking-[0.12em] uppercase"
            style={{ color: style.colour, borderColor: `${style.colour}55`, background: `${style.colour}14` }}
          >
            {event ? <span className="font-mono">{latest + 1}</span> : null}
            {style.label}
          </span>
          <span className="font-mono text-[11px] text-slate-500">{event ? when(event.day) : "Week 1 · Mon"}</span>
        </div>
        <div className="flex shrink-0 gap-1.5">
          <DemoButton
            className="px-2 py-1.5"
            onClick={() => onJump(previous ? previous.day : 0)}
            disabled={latest < 0}
            label="Previous event"
          >
            <ChevronLeft className="h-3.5 w-3.5" aria-hidden="true" />
          </DemoButton>
          <DemoButton className="px-2 py-1.5" onClick={() => onJump(next ? next.day : DAYS - 1)} disabled={!next} label="Next event">
            <ChevronRight className="h-3.5 w-3.5" aria-hidden="true" />
          </DemoButton>
        </div>
      </div>
      <p className="mt-2 text-[14px] font-bold text-white">{event ? event.title : "A fresh workspace"}</p>
      <p className="mt-1 text-[12.5px] leading-relaxed text-slate-400">
        {event
          ? event.detail
          : "Nothing to recall yet. The first sessions write scratch notes, capture what happened, and record a few facts."}
      </p>
      <p className="mt-2 font-mono text-[11px] leading-relaxed [overflow-wrap:anywhere] text-violet-200">
        <span className="text-slate-600 select-none">$ </span>
        {event ? event.command : `ee init --workspace ${domain.workspace}`}
      </p>
    </div>
  );
}

function EventList({
  timeline,
  latest,
  cursor,
  onJump,
}: {
  timeline: readonly CampaignEvent[];
  latest: number;
  cursor: number;
  onJump: (day: number) => void;
}) {
  return (
    <ol className="mt-3 hidden grid-cols-2 gap-x-5 sm:grid">
      {timeline.map((event, index) => {
        const past = event.day <= cursor + 1e-9;
        const colour = EVENT_STYLE[event.type].colour;
        return (
          <li key={index}>
            <button
              type="button"
              onClick={() => onJump(event.day)}
              aria-current={index === latest ? "step" : undefined}
              className={cn(
                "flex w-full items-center gap-2.5 rounded-lg px-1.5 py-1.5 text-left transition-colors hover:bg-white/[0.04] focus-visible:ring-2 focus-visible:ring-violet-400 focus-visible:outline-none",
                index === latest && "bg-white/[0.05]",
              )}
            >
              <span
                aria-hidden="true"
                className="flex h-[18px] w-[18px] shrink-0 items-center justify-center rounded-full border font-mono text-[9.5px] font-bold"
                style={past ? { background: colour, borderColor: colour, color: "#05040d" } : { borderColor: `${colour}88`, color: colour }}
              >
                {index + 1}
              </span>
              <span className="w-8 shrink-0 font-mono text-[10.5px] text-slate-500 tabular-nums">W{weekOf(event.day)}</span>
              <span className={cn("min-w-0 truncate text-[12px]", past ? "text-slate-200" : "text-slate-500")}>{event.title}</span>
            </button>
          </li>
        );
      })}
    </ol>
  );
}

// ─── Monday's pack ───────────────────────────────────────────────────────────

function PackCard({ campaign, pack }: { campaign: Campaign; pack: Pack }) {
  return (
    <div className="relative min-w-0 rounded-2xl border border-white/10 bg-[#07051a]/80 p-4 sm:p-5">
      <div className="flex items-baseline justify-between gap-3">
        <p className="font-mono text-[10px] font-medium tracking-[0.22em] text-[color:var(--chapter)] uppercase">
          Monday · week {weekOf(pack.day)}
        </p>
        <p className="font-mono text-[10px] text-slate-500 tabular-nums">
          {grouped(pack.used)} / {grouped(BUDGET)} tokens
        </p>
      </div>
      <p className="mt-1 text-[15px] font-bold text-white">The session starts with this pack</p>
      <p className="mt-2 font-mono text-[11px] leading-snug [overflow-wrap:anywhere] text-slate-400">
        <span className="text-slate-600 select-none">$ </span>
        ee pack &ldquo;{campaign.domain.task}&rdquo; --max-tokens {BUDGET}
      </p>

      <ol className="mt-4 space-y-3.5">
        {pack.sections.map((section) => (
          <SectionBlock key={section.section} section={section} day={pack.day} />
        ))}
      </ol>

      <div className="mt-4 border-t border-white/[0.06] pt-3">
        <div
          role="img"
          aria-label={`Pack spend by section: ${pack.sections.map((entry) => `${SECTION_KEY[entry.section]} ${entry.used} of ${entry.quota}`).join(", ")}; ${pack.used} of ${BUDGET} tokens.`}
          className="flex h-2.5 overflow-hidden rounded-full bg-white/[0.05] ring-1 ring-white/[0.06]"
        >
          {pack.sections.map((entry) => (
            <span
              key={entry.section}
              className="h-full border-r border-[#05040d]/70 transition-[width] duration-500 motion-reduce:transition-none"
              style={{ width: `${(entry.used / BUDGET) * 100}%`, background: SECTION_COLOUR[entry.section] }}
            />
          ))}
        </div>
        <p className="mt-1.5 flex justify-between gap-3 font-mono text-[10px] text-slate-500">
          <span>spend by section</span>
          <span>budget {grouped(BUDGET)}, never more</span>
        </p>
      </div>
    </div>
  );
}

function SectionBlock({ section, day }: { section: SectionPack; day: number }) {
  const colour = SECTION_COLOUR[section.section];
  return (
    <li>
      <div className="flex items-center gap-2">
        <span className="font-mono text-[10px] tracking-[0.14em] uppercase" style={{ color: colour }}>
          ## {SECTION_KEY[section.section]}
        </span>
        <span aria-hidden="true" className="relative h-[3px] min-w-6 flex-1 overflow-hidden rounded-full bg-white/[0.06]">
          <span
            className="absolute inset-y-0 left-0 rounded-full transition-[width] duration-500 motion-reduce:transition-none"
            style={{ width: `${(section.used / section.quota) * 100}%`, background: colour }}
          />
        </span>
        <span className="font-mono text-[10px] text-slate-500 tabular-nums">
          {grouped(section.used)}/{grouped(section.quota)}
        </span>
      </div>
      <ul className="mt-2 space-y-2.5">
        {section.lines.map((line) => (
          <PackRow key={line.item.id} line={line} day={day} />
        ))}
        {section.more > 0 ? (
          <li className="pl-4 font-mono text-[10.5px] text-slate-500">
            + {section.more} more · {grouped(section.moreTokens)} tokens
          </li>
        ) : null}
        {section.lines.length === 0 && section.more === 0 ? (
          <li className="pl-4 text-[11.5px] text-slate-500 italic">{EMPTY[section.section]}</li>
        ) : null}
      </ul>
    </li>
  );
}

function PackRow({ line, day }: { line: PackLine; day: number }) {
  const { item, trust } = line;
  const colour = LEVEL_COLOUR[item.level];
  const upcoming = !line.due && item.revisitBy !== undefined && item.revisitBy < DAYS && item.revisitBy > day;
  return (
    <li className="type-in grid grid-cols-[0.55rem_minmax(0,1fr)] gap-x-2">
      <span aria-hidden="true" className="mt-[5px] h-2 w-2 rounded-full" style={{ background: colour, boxShadow: `0 0 8px ${colour}` }} />
      <div className="min-w-0">
        <p className="text-[12.5px] leading-snug text-slate-200">{item.text}</p>
        <p className="mt-1 flex flex-wrap items-center gap-x-1.5 gap-y-1 font-mono text-[9.5px] text-slate-500">
          <span>
            {item.level} · {item.kind}
          </span>
          <span className={cn("rounded border px-1 py-px", TRUST_STYLE[trust])}>
            {trust}
            {line.upgraded ? " ↑" : ""}
          </span>
          {line.due ? (
            <span className="inline-flex items-center gap-1 rounded border border-orange-300/50 bg-orange-400/15 px-1 py-px text-orange-200">
              <span aria-hidden="true" className="pulse-dot h-1.5 w-1.5 rounded-full bg-orange-300" />
              revisit due
            </span>
          ) : upcoming ? (
            <span className="rounded border border-white/10 px-1 py-px">revisit W{weekOf(item.revisitBy ?? 0)}</span>
          ) : null}
          {line.fresh ? <span className="font-bold text-[color:var(--chapter)]">new</span> : null}
        </p>
        <p className="mt-0.5 truncate font-mono text-[10px] text-slate-500">{item.source}</p>
      </div>
    </li>
  );
}
