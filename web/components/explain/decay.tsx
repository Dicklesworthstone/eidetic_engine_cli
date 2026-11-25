"use client";

import { type CSSProperties, useState } from "react";

import { DemoFrame, Readout, Slider } from "@/components/explain/demo";
import { Cmd } from "@/components/instrument";
import { useFigureWidth } from "@/lib/hooks";
import { LEVEL_COLOUR } from "@/lib/palette";
import { cn, fixed } from "@/lib/utils";

/**
 * The decay policy, computed here with the constants src/policy/memory_decay.rs declares:
 *   freshness = exp(-ln 2 · age / half-life)                    (memory_decay_freshness_score)
 *   lifecycle = freshness × confidence × utility                (evaluate_memory_decay_with_settings)
 *   lifecycle < 0.01 → tombstone; < 0.05 → demote; else preserve (DEFAULT_DECAY_*_THRESHOLD)
 */
export const DEMOTE = 0.05;
export const FORGET = 0.01;
const MAX_DAYS = 730;

/** MemoryDecayHalfLives::default(), in days. */
export const KINDS = [
  { id: "working", label: "Working", pair: "working · any kind", half: 1, colour: LEVEL_COLOUR.working, level: "working", dash: undefined },
  { id: "event", label: "Episodic event", pair: "episodic · event", half: 30, colour: LEVEL_COLOUR.episodic, level: "episodic", dash: undefined },
  { id: "failure", label: "Episodic failure", pair: "episodic · failure", half: 90, colour: "#fda4af", level: "episodic", dash: "5 4" },
  { id: "fact", label: "Semantic fact", pair: "semantic · fact", half: 180, colour: LEVEL_COLOUR.semantic, level: "semantic", dash: undefined },
  { id: "rule", label: "Procedural rule", pair: "procedural · rule", half: 365, colour: LEVEL_COLOUR.procedural, level: "procedural", dash: undefined },
] as const;

/** demoted_memory_level: one step down, where there is a step. */
export const DEMOTED: Record<string, string | undefined> = { procedural: "semantic", semantic: "episodic" };

const freshness = (days: number, half: number) => Math.exp((-Math.LN2 * Math.max(0, days)) / half);

/** The time axis is logarithmic (log(1 + days)), so a one-day half-life and a one-year one share the chart. */
const LOG_MAX = Math.log1p(MAX_DAYS);
const toUnit = (days: number) => Math.log1p(days) / LOG_MAX;
const fromUnit = (unit: number) => Math.expm1(unit * LOG_MAX);
const TICKS = [
  { days: 0, label: "0" },
  { days: 1, label: "1d" },
  { days: 7, label: "1w" },
  { days: 30, label: "1mo" },
  { days: 90, label: "3mo" },
  { days: 180, label: "6mo" },
  { days: 365, label: "1y" },
  { days: 730, label: "2y" },
] as const;

export type Action = "preserve" | "demote" | "tombstone";

export function classify(score: number): Action {
  if (score < FORGET) return "tombstone";
  if (score < DEMOTE) return "demote";
  return "preserve";
}

/** The age at which `confidence × utility × freshness` falls under `threshold`, or 0 if it starts there. */
const crossing = (half: number, product: number, threshold: number) =>
  product <= threshold ? 0 : half * Math.log2(product / threshold);

function age(days: number): string {
  if (days < 1) return `${fixed(days * 24, 0)} h`;
  if (days < 60) return `${fixed(days, days < 10 ? 1 : 0)} d`;
  if (days < 730) return `${fixed(days / 30.4, 1)} mo`;
  return `${fixed(days / 365, 1)} y`;
}

const ACTION_STYLE: Record<Action, string> = {
  preserve: "border-emerald-400/30 bg-emerald-400/10 text-emerald-200",
  demote: "border-amber-400/35 bg-amber-400/10 text-amber-200",
  tombstone: "border-rose-400/35 bg-rose-400/10 text-rose-200",
};

export function DecayCurves() {
  const { ref, width } = useFigureWidth(760);
  // The day slider moves along the same logarithmic axis as the chart, in thousandths.
  const [cursor, setCursor] = useState(Math.round(toUnit(90) * 1000));
  const [confidence, setConfidence] = useState(0.5);
  const [utility, setUtility] = useState(0.5);
  const days = fromUnit(cursor / 1000);
  const product = confidence * utility;

  const W = Math.max(280, width);
  const H = W < 520 ? 230 : 290;
  const m = { left: 40, right: 14, top: 14, bottom: 34 };
  const plotW = W - m.left - m.right;
  const plotH = H - m.top - m.bottom;
  const x = (d: number) => m.left + toUnit(d) * plotW;
  const y = (f: number) => m.top + (1 - Math.min(1, Math.max(0, f))) * plotH;
  // The cursor's age tag, kept inside the figure at either end of the axis.
  const tag = Math.min(W - 27, Math.max(m.left + 26, x(days)));

  // A lifecycle threshold, drawn on the freshness axis: freshness < threshold / (confidence × utility).
  const demoteAt = product > 0 ? DEMOTE / product : Number.POSITIVE_INFINITY;
  const forgetAt = product > 0 ? FORGET / product : Number.POSITIVE_INFINITY;

  const curve = (half: number) => {
    const steps = 180;
    let d = "";
    for (let i = 0; i <= steps; i++) {
      const day = fromUnit(i / steps);
      d += `${i === 0 ? "M" : "L"} ${x(day).toFixed(2)} ${y(freshness(day, half)).toFixed(2)} `;
    }
    return d;
  };

  const rows = KINDS.map((kind) => {
    const f = freshness(days, kind.half);
    const score = f * product;
    const action = classify(score);
    return {
      ...kind,
      f,
      score,
      action,
      demoteAfter: crossing(kind.half, product, DEMOTE),
      forgetAfter: crossing(kind.half, product, FORGET),
    };
  });
  const counts = { preserve: 0, demote: 0, tombstone: 0 };
  for (const row of rows) counts[row.action] += 1;

  return (
    <DemoFrame
      eyebrow="Half-lives by kind"
      title="Age, confidence and utility decide when a memory is demoted or tombstoned."
      basis="rule"
      source={
        <>
          src/policy/memory_decay.rs · MemoryDecayHalfLives::default (working 1 d, episodic event 30 d, episodic failure 90 d,
          semantic fact 180 d, procedural rule 365 d) · memory_decay_freshness_score · DEFAULT_DECAY_DEMOTE_THRESHOLD 0.05 ·
          DEFAULT_DECAY_FORGET_THRESHOLD 0.01 · demoted_memory_level · src/steward/mod.rs · memory_decay_reference_time ·
          src/core/curate.rs · run_curate_untombstone
        </>
      }
    >
      <div className="grid grid-cols-1 gap-6 lg:grid-cols-[minmax(0,1fr)_17rem]">
        <div className="min-w-0">
          <div ref={ref} className="w-full">
            <svg
              className="diagram block h-auto w-full"
              width={W}
              height={H}
              viewBox={`0 0 ${W} ${H}`}
              role="img"
              aria-label={`Freshness of five memory kinds over two years on a logarithmic time axis. At ${age(days)}: ${rows
                .map((row) => `${row.label} freshness ${fixed(row.f, 3)}, ${row.action}`)
                .join("; ")}.`}
            >
              {/* The demote and tombstone zones: lifecycle below a threshold, drawn where freshness puts it. */}
              {demoteAt < 1 ? (
                <rect x={m.left} y={y(demoteAt)} width={plotW} height={y(0) - y(demoteAt)} fill="rgba(251,191,36,0.06)" />
              ) : (
                <rect x={m.left} y={m.top} width={plotW} height={plotH} fill="rgba(251,191,36,0.06)" />
              )}
              {forgetAt < 1 ? (
                <rect x={m.left} y={y(forgetAt)} width={plotW} height={y(0) - y(forgetAt)} fill="rgba(251,113,133,0.09)" />
              ) : (
                <rect x={m.left} y={m.top} width={plotW} height={plotH} fill="rgba(251,113,133,0.09)" />
              )}

              {/* Grid */}
              {[0, 0.25, 0.5, 0.75, 1].map((f) => (
                <g key={f}>
                  <line x1={m.left} x2={W - m.right} y1={y(f)} y2={y(f)} stroke="rgba(255,255,255,0.05)" />
                  <text x={m.left - 8} y={y(f) + 3.5} textAnchor="end" className="mono" fontSize="10" fill="#77839c">
                    {f === 0 ? "0" : f === 1 ? "1" : f.toFixed(2).replace(/^0/, "")}
                  </text>
                </g>
              ))}
              {TICKS.map((tick) => (
                <g key={tick.label}>
                  <line x1={x(tick.days)} x2={x(tick.days)} y1={m.top} y2={H - m.bottom} stroke="rgba(255,255,255,0.035)" />
                  <text x={x(tick.days)} y={H - m.bottom + 16} textAnchor="middle" className="mono" fontSize="10" fill="#77839c">
                    {tick.label}
                  </text>
                </g>
              ))}
              <text x={W - m.right} y={H - 4} textAnchor="end" className="mono" fontSize="9" letterSpacing="1" fill="#5b6478">
                TIME SINCE LAST USE · LOG SCALE
              </text>
              <text x={m.left + 4} y={m.top + 11} className="mono" fontSize="9" letterSpacing="1" fill="#5b6478">
                FRESHNESS
              </text>

              {/* Threshold lines */}
              {demoteAt < 1 ? (
                <g>
                  <line x1={m.left} x2={W - m.right} y1={y(demoteAt)} y2={y(demoteAt)} stroke="#fbbf24" strokeOpacity="0.7" strokeDasharray="4 4" />
                  <text x={W - m.right - 4} y={y(demoteAt) - 5} textAnchor="end" className="mono" fontSize="9.5" fill="#fcd34d">
                    demote · lifecycle &lt; 0.05
                  </text>
                </g>
              ) : null}
              {forgetAt < 1 ? (
                <g>
                  <line x1={m.left} x2={W - m.right} y1={y(forgetAt)} y2={y(forgetAt)} stroke="#fb7185" strokeOpacity="0.7" strokeDasharray="2 4" />
                  {y(0) - y(forgetAt) > 13 ? (
                    <text x={W - m.right - 4} y={y(forgetAt) + 12} textAnchor="end" className="mono" fontSize="9.5" fill="#fda4af">
                      tombstone · &lt; 0.01
                    </text>
                  ) : null}
                </g>
              ) : null}

              {/* The curves, each with a mark at its half-life */}
              {KINDS.map((kind) => (
                <g key={kind.id}>
                  <path d={curve(kind.half)} fill="none" stroke={kind.colour} strokeWidth="2" strokeDasharray={kind.dash} strokeLinecap="round" />
                  <circle cx={x(kind.half)} cy={y(0.5)} r="2.5" fill="#05040d" stroke={kind.colour} strokeWidth="1.5" />
                </g>
              ))}

              {/* The cursor */}
              <line x1={x(days)} x2={x(days)} y1={m.top} y2={H - m.bottom} stroke="#e2e8f0" strokeOpacity="0.55" />
              <rect x={tag - 26} y={m.top - 1} width="52" height="15" rx="4" fill="#e2e8f0" />
              <text x={tag} y={m.top + 10} textAnchor="middle" className="mono" fontSize="9.5" fontWeight="600" fill="#05040d">
                {age(days)}
              </text>
              {rows.map((row) => (
                <circle
                  key={row.id}
                  cx={x(days)}
                  cy={y(row.f)}
                  r="4.5"
                  fill={row.colour}
                  stroke="#05040d"
                  strokeWidth="2"
                  opacity={row.action === "tombstone" ? 0.45 : 1}
                />
              ))}
            </svg>
          </div>

          <div className="mt-5 grid grid-cols-1 gap-4 sm:grid-cols-3">
            <Slider
              label="Time since last use"
              min={0}
              max={1000}
              value={cursor}
              onChange={setCursor}
              display={age(days)}
              valueText={age(days)}
            />
            <Slider
              label="Confidence"
              min={0.05}
              max={1}
              step={0.05}
              value={confidence}
              onChange={setConfidence}
              display={fixed(confidence, 2)}
            />
            <Slider label="Utility" min={0.05} max={1} step={0.05} value={utility} onChange={setUtility} display={fixed(utility, 2)} />
          </div>
        </div>

        <div className="space-y-3">
          <div className="grid grid-cols-3 gap-2 lg:grid-cols-1">
            <Readout label="Preserve" value={counts.preserve} />
            <Readout label="Demote" value={counts.demote} />
            <Readout label="Tombstone" value={counts.tombstone} tone={counts.tombstone > 0 ? "wrong" : "neutral"} />
          </div>
          <Readout
            label="Confidence × utility"
            value={fixed(product, 3)}
            note={
              product <= DEMOTE
                ? "Confidence × utility is at most 0.05, so the memory sits on or under the demote line even when fresh."
                : `Demote once freshness drops below ${fixed(demoteAt, 3)}; tombstone below ${fixed(Math.min(1, forgetAt), 3)}.`
            }
            tone="rule"
          />
        </div>
      </div>

      <div className="mt-6 overflow-x-auto [scrollbar-width:thin]">
        <table className="w-full min-w-[34rem] text-left text-[12.5px]">
          <thead>
            <tr className="font-mono text-[9.5px] tracking-[0.16em] text-slate-500 uppercase">
              <th className="py-2 pr-3 font-medium">Kind</th>
              <th className="py-2 pr-3 font-medium">Half-life</th>
              <th className="py-2 pr-3 text-right font-medium">Freshness</th>
              <th className="py-2 pr-3 text-right font-medium">Lifecycle</th>
              <th className="py-2 pr-3 font-medium">Decision</th>
              <th className="py-2 text-right font-medium">Demotes after</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <tr key={row.id} className="border-t border-white/[0.06]">
                <td className="py-2.5 pr-3">
                  <span className="flex items-center gap-2.5">
                    <span aria-hidden="true" className="sphere h-2.5 w-2.5" style={{ "--sphere": row.colour } as CSSProperties} />
                    <span className="text-slate-200">{row.label}</span>
                  </span>
                </td>
                <td className="py-2.5 pr-3 font-mono text-slate-400 tabular-nums">{row.half} d</td>
                <td className="py-2.5 pr-3 text-right font-mono text-white tabular-nums">{fixed(row.f, 3)}</td>
                <td className="py-2.5 pr-3 text-right font-mono text-white tabular-nums">{fixed(row.score, 4)}</td>
                <td className="py-2.5 pr-3">
                  <span className={cn("inline-block rounded-full border px-2 py-0.5 font-mono text-[10.5px] whitespace-nowrap", ACTION_STYLE[row.action])}>
                    {row.action === "demote" && DEMOTED[row.level] ? `demote → ${DEMOTED[row.level]}` : row.action}
                  </span>
                </td>
                <td className="py-2.5 text-right font-mono text-slate-400 tabular-nums">
                  {row.demoteAfter === 0 ? "at once" : age(row.demoteAfter)}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      <div className="mt-5 grid grid-cols-1 gap-3 text-[13px] leading-relaxed text-slate-400 md:grid-cols-3">
        <p>
          <span className="font-semibold text-slate-200">Age.</span> Age runs from the latest of a memory&rsquo;s creation,
          update and last access, so a memory that keeps being recalled stays fresh.
        </p>
        <p>
          <span className="font-semibold text-slate-200">Demotion.</span> A demoted procedural memory moves down to semantic and
          a semantic one to episodic, and its importance halves.
        </p>
        <p>
          <span className="font-semibold text-slate-200">Tombstones.</span> A tombstoned memory leaves retrieval and gets an
          audit entry. The row is kept, and <Cmd>ee curate untombstone &lt;id&gt;</Cmd> restores it.
        </p>
      </div>
    </DemoFrame>
  );
}
