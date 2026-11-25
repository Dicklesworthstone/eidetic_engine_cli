"use client";

import { ArrowDownToLine, ArrowUpFromLine } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";

import { DemoFrame } from "@/components/explain/demo";
import { Cmd } from "@/components/instrument";
import { useFigureWidth, useInView, useReducedMotion } from "@/lib/hooks";
import { RECALL } from "@/lib/palette";
import { cn } from "@/lib/utils";

/** The write direction's colour (working-memory amber) and the read direction's (recall violet). */
const PUSH = "#fbbf24";
const PULL = RECALL;

type Direction = "push" | "pull";
type Store = "db" | "index" | "graph";

/**
 * The operating loop's commands (README.md · Agent Operating Loop), each with the direction it moves memory and the
 * stores it reads or writes by default. remember publishes its own index document unless --defer-index; outcome and
 * import cass only queue index jobs. recall reads the memory_anchor_index table, ask and preflight check score the
 * database's own corpus, and why reads the latest graph snapshot (src/core/why.rs · build_graph_retrieval_explanation).
 */
export const COMMANDS: readonly {
  id: string;
  name: string;
  direction: Direction;
  line: string;
  what: string;
  touches: readonly Store[];
}[] = [
  {
    id: "remember",
    name: "remember",
    direction: "push",
    line: 'ee remember "<lesson>" --workspace . --level procedural --kind rule --json',
    what: "Stores a lesson as a typed memory, with its level, kind and provenance, and adds it to the search index.",
    touches: ["db", "index"],
  },
  {
    id: "journal",
    name: "journal append",
    direction: "push",
    line: 'ee journal append "<observation>" --workspace . --source manual --json',
    what: "Appends a raw observation. Later, ee journal distill previews curation candidates from the journal, and --apply saves them for review.",
    touches: ["db"],
  },
  {
    id: "outcome",
    name: "outcome",
    direction: "push",
    line: 'ee outcome <id> --signal helpful --reason "<why>"',
    what: "Records whether a memory helped or misled. Harmful feedback weighs 2.5 times as much as helpful.",
    touches: ["db"],
  },
  {
    id: "cass",
    name: "import cass",
    direction: "push",
    line: "ee import cass --workspace . --json",
    what: "Imports past agent sessions (Claude Code, Codex, Cursor, Gemini, ChatGPT) as evidence. A ledger records each import, so a rerun resumes and adds no duplicates.",
    touches: ["db"],
  },
  {
    id: "pack",
    name: "pack",
    direction: "pull",
    line: 'ee pack "<task>" --workspace . --max-tokens 4000 --format markdown',
    what: "Assembles a token-budgeted context pack; every item carries an evidence pointer and a score breakdown.",
    touches: ["db", "index", "graph"],
  },
  {
    id: "recall",
    name: "recall",
    direction: "pull",
    line: "ee recall --path <path> --workspace . --budget-tokens 400 --format markdown",
    what: "Before an edit, returns the memories anchored to a file, a symbol or a diff, within a 400-token budget.",
    touches: ["db"],
  },
  {
    id: "search",
    name: "search",
    direction: "pull",
    line: 'ee search "<question>" --workspace . --explain --json',
    what: "Runs hybrid lexical and semantic search and explains each result's score components.",
    touches: ["db", "index"],
  },
  {
    id: "ask",
    name: "ask",
    direction: "pull",
    line: 'ee ask "<question>" --workspace . --json',
    what: "Answers a direct question and cites the memories the answer rests on.",
    touches: ["db"],
  },
  {
    id: "why",
    name: "why",
    direction: "pull",
    line: "ee why <memory-id> --workspace . --json",
    what: "Explains one memory: why it ranks, what supports it, how fresh and how reliable it is.",
    touches: ["db", "graph"],
  },
  {
    id: "preflight",
    name: "preflight",
    direction: "pull",
    line: 'ee preflight check --cmd "<command>" --workspace . --json',
    what: "Before a risky shell command, looks up matching risk and failure memories. It only advises and never blocks the command.",
    touches: ["db"],
  },
];

type Box = { x: number; y: number; w: number; h: number };

/** The figure's geometry at `width`: side by side on a wide screen, stacked on a phone. */
function layout(width: number) {
  const W = Math.max(300, width);
  if (W >= 640) {
    const H = 300;
    const harness: Box = { x: 1, y: 16, w: Math.round(W * 0.34), h: H - 32 };
    const ee: Box = { x: Math.round(W * 0.56), y: 16, w: W - Math.round(W * 0.56) - 1, h: H - 32 };
    const gapMid = (harness.x + harness.w + ee.x) / 2;
    const pushY = { from: H * 0.36, to: ee.y + 73 };
    const pullY = { from: ee.y + 143, to: H * 0.68 };
    const a = harness.x + harness.w;
    const b = ee.x;
    return {
      W,
      H,
      harness,
      ee,
      wide: true,
      push: `M ${a + 2} ${pushY.from} C ${gapMid} ${pushY.from}, ${gapMid} ${pushY.to}, ${b - 6} ${pushY.to}`,
      pull: `M ${b - 2} ${pullY.from} C ${gapMid} ${pullY.from}, ${gapMid} ${pullY.to}, ${a + 6} ${pullY.to}`,
      pushLabel: { x: gapMid, y: Math.min(pushY.from, pushY.to) - 14 },
      pullLabel: { x: gapMid, y: Math.max(pullY.from, pullY.to) + 22 },
    };
  }
  const harness: Box = { x: 1, y: 8, w: W - 2, h: 176 };
  const gap = 104;
  const ee: Box = { x: 1, y: harness.y + harness.h + gap, w: W - 2, h: 270 };
  const H = ee.y + ee.h + 8;
  const top = harness.y + harness.h;
  const bottom = ee.y;
  const mid = (top + bottom) / 2;
  const left = Math.round(W * 0.3);
  const right = Math.round(W * 0.7);
  return {
    W,
    H,
    harness,
    ee,
    wide: false,
    push: `M ${left} ${top + 2} C ${left} ${mid}, ${left} ${mid}, ${left} ${bottom - 6}`,
    pull: `M ${right} ${bottom - 2} C ${right} ${mid}, ${right} ${mid}, ${right} ${top + 6}`,
    pushLabel: { x: left - 12, y: mid + 4 },
    pullLabel: { x: right + 12, y: mid + 4 },
  };
}

const LOOP_STEPS = ["plan", "act", "observe"] as const;
const AUTO_MS = 2800;

export function MemoryLoop() {
  const { ref, width } = useFigureWidth(860);
  const frame = useRef<HTMLDivElement>(null);
  const reduced = useReducedMotion();
  const inView = useInView(frame);
  const running = inView && !reduced;
  const [selected, setSelected] = useState("pack");
  const [auto, setAuto] = useState(true);
  const [beat, setBeat] = useState(0);
  // React's ids carry punctuation that a url(#…) reference cannot.
  const id = `loop${useId().replace(/[^a-zA-Z0-9_-]/g, "")}`;

  // Until the reader picks a command, the figure walks through them.
  useEffect(() => {
    if (!running || !auto) return;
    const handle = window.setInterval(() => {
      setSelected((current) => {
        const at = COMMANDS.findIndex((command) => command.id === current);
        return COMMANDS[(at + 1) % COMMANDS.length]?.id ?? "pack";
      });
    }, AUTO_MS);
    return () => window.clearInterval(handle);
  }, [running, auto]);

  // The harness's own loop turns on its own clock.
  useEffect(() => {
    if (!running) return;
    const handle = window.setInterval(() => setBeat((step) => (step + 1) % LOOP_STEPS.length), 900);
    return () => window.clearInterval(handle);
  }, [running]);

  const command = COMMANDS.find((entry) => entry.id === selected) ?? COMMANDS[4]!;
  const g = layout(width);
  const active = command.direction;
  const touched = new Set<Store>(command.touches);

  // The harness's plan → act → observe ring.
  const { harness, ee } = g;
  const ringWide = harness.w > harness.h * 1.3;
  const ring = {
    r: Math.min(harness.h * 0.24, harness.w * 0.19, 54),
    cx: ringWide ? harness.x + harness.w * 0.7 : harness.x + harness.w / 2,
    cy: ringWide ? harness.y + harness.h / 2 + 6 : harness.y + harness.h * 0.6,
  };
  const ringAngles = [-90, 30, 150].map((deg) => (deg * Math.PI) / 180);
  const ringPoint = (angle: number, r = ring.r) => ({ x: ring.cx + r * Math.cos(angle), y: ring.cy + r * Math.sin(angle) });
  const arc = (from: number, to: number) => {
    const start = ringPoint(from + 0.32);
    const end = ringPoint(to - 0.32);
    return `M ${start.x} ${start.y} A ${ring.r} ${ring.r} 0 0 1 ${end.x} ${end.y}`;
  };

  // The ee box: source of truth, then the derived assets.
  const inner = { x: ee.x + 14, w: ee.w - 28 };
  const truth = { y: ee.y + 46, h: 54 };
  const derivedTop = truth.y + truth.h + 26;
  const rowH = 34;
  const derived: { store: Store | "cache"; title: string; note: string }[] = [
    { store: "index", title: "Frankensearch index", note: "BM25 + Model2Vec vectors" },
    { store: "graph", title: "Graph snapshot", note: "FrankenNetworkX" },
    { store: "cache", title: "Caches", note: "query plans, warm packs" },
  ];

  const pathStyle = (direction: Direction) => ({
    opacity: active === direction ? 1 : 0.22,
    strokeWidth: active === direction ? 2.4 : 1.4,
  });

  return (
    <DemoFrame
      eyebrow="Who owns what"
      title="Agents write evidence to ee and read context from it."
      basis="diagram"
      source={
        <>
          README.md · Agent Operating Loop, Design Philosophy §2 (harness agnostic) and §6 (search indexes are derived
          assets) · src/cli/mod.rs (the commands) · src/db (FrankenSQLite + SQLModel, the source of truth)
        </>
      }
    >
      <div ref={frame}>
        <div ref={ref} className="w-full">
          <svg
            className="diagram block h-auto w-full overflow-visible"
            width={g.W}
            height={g.H}
            viewBox={`0 0 ${g.W} ${g.H}`}
            role="img"
            aria-label={`The agent harness runs its own plan, act and observe loop and calls ee. Writes such as remember, journal append, outcome and import cass push memory into ee's FrankenSQLite source of truth; reads such as pack, recall, search, ask, why and preflight pull context out of it and its derived, rebuildable Frankensearch index and graph snapshot. Highlighted now: ee ${command.name}, a ${command.direction === "push" ? "write" : "read"}.`}
          >
            <defs>
              <marker id={`${id}-push`} viewBox="0 0 10 10" refX="8" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
                <path d="M 0 0 L 10 5 L 0 10 z" fill={PUSH} />
              </marker>
              <marker id={`${id}-pull`} viewBox="0 0 10 10" refX="8" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
                <path d="M 0 0 L 10 5 L 0 10 z" fill={PULL} />
              </marker>
              <marker id={`${id}-ring`} viewBox="0 0 10 10" refX="7" refY="5" markerWidth="5" markerHeight="5" orient="auto-start-reverse">
                <path d="M 0 0 L 10 5 L 0 10 z" fill="#94a3b8" />
              </marker>
              <radialGradient id={`${id}-glow`}>
                <stop offset="0" stopColor={active === "push" ? PUSH : PULL} stopOpacity="0.35" />
                <stop offset="1" stopColor={active === "push" ? PUSH : PULL} stopOpacity="0" />
              </radialGradient>
            </defs>

            {/* The harness */}
            <rect x={harness.x} y={harness.y} width={harness.w} height={harness.h} rx="18" fill="rgba(255,255,255,0.02)" stroke="rgba(221,214,254,0.14)" />
            <text x={harness.x + 16} y={harness.y + 28} fill="#fff" fontSize="14" fontWeight="800">
              Agent harness
            </text>
            <text x={harness.x + 16} y={harness.y + 46} fill="#94a3b8" fontSize="11">
              Claude Code · Codex
            </text>
            <text x={harness.x + 16} y={harness.y + 61} fill="#94a3b8" fontSize="11">
              any shell · MCP
            </text>
            <text x={harness.x + 16} y={harness.y + harness.h - 30} fill="#77839c" fontSize="10.5">
              owns tools, approvals
            </text>
            <text x={harness.x + 16} y={harness.y + harness.h - 16} fill="#77839c" fontSize="10.5">
              and the prompt loop
            </text>
            <circle cx={ring.cx} cy={ring.cy} r={ring.r} fill="none" stroke="rgba(255,255,255,0.05)" />
            {ringAngles.map((angle, index) => {
              const next = ringAngles[(index + 1) % ringAngles.length]!;
              return (
                <path
                  key={`arc-${index}`}
                  d={arc(angle, next < angle ? next + Math.PI * 2 : next)}
                  fill="none"
                  stroke="#64748b"
                  strokeWidth="1.2"
                  markerEnd={`url(#${id}-ring)`}
                />
              );
            })}
            {ringAngles.map((angle, index) => {
              const at = ringPoint(angle);
              const label = ringPoint(angle, ring.r + 15);
              const on = index === beat;
              return (
                <g key={LOOP_STEPS[index]}>
                  <circle cx={at.x} cy={at.y} r={on ? 5 : 3.5} fill={on ? "#e2e8f0" : "#475569"} className="transition-all duration-300" />
                  <text
                    x={label.x}
                    y={label.y + 3.5}
                    textAnchor={Math.abs(Math.cos(angle)) < 0.2 ? "middle" : Math.cos(angle) > 0 ? "start" : "end"}
                    className="mono"
                    fontSize="10"
                    fill={on ? "#e2e8f0" : "#77839c"}
                  >
                    {LOOP_STEPS[index]}
                  </text>
                </g>
              );
            })}

            {/* The two flows */}
            <path id={`${id}-push-path`} d={g.push} fill="none" stroke={PUSH} strokeOpacity="0.18" strokeWidth="6" style={{ opacity: active === "push" ? 1 : 0 }} />
            <path
              d={g.push}
              fill="none"
              stroke={PUSH}
              className={cn(running && "flow")}
              strokeDasharray="3 9"
              strokeLinecap="round"
              markerEnd={`url(#${id}-push)`}
              style={pathStyle("push")}
            />
            <path id={`${id}-pull-path`} d={g.pull} fill="none" stroke={PULL} strokeOpacity="0.18" strokeWidth="6" style={{ opacity: active === "pull" ? 1 : 0 }} />
            <path
              d={g.pull}
              fill="none"
              stroke={PULL}
              className={cn(running && "flow")}
              strokeDasharray="3 9"
              strokeLinecap="round"
              markerEnd={`url(#${id}-pull)`}
              style={pathStyle("pull")}
            />
            {running ? (
              <circle key={`${selected}-dot`} r="4.5" fill={active === "push" ? PUSH : PULL}>
                <animateMotion dur="1.4s" repeatCount="indefinite">
                  <mpath href={`#${id}-${active}-path`} />
                </animateMotion>
              </circle>
            ) : null}
            <text
              x={g.pushLabel.x}
              y={g.pushLabel.y}
              textAnchor={g.wide ? "middle" : "end"}
              className="mono"
              fontSize="10"
              letterSpacing="2"
              fill={PUSH}
              style={{ opacity: active === "push" ? 1 : 0.45 }}
            >
              PUSH IN
            </text>
            <text
              x={g.pullLabel.x}
              y={g.pullLabel.y}
              textAnchor={g.wide ? "middle" : "start"}
              className="mono"
              fontSize="10"
              letterSpacing="2"
              fill={PULL}
              style={{ opacity: active === "pull" ? 1 : 0.45 }}
            >
              PULL OUT
            </text>
            {g.wide ? (
              <text x={g.pushLabel.x} y={g.pushLabel.y + 13} textAnchor="middle" fontSize="10.5" fill="#94a3b8" style={{ opacity: active === "push" ? 1 : 0.45 }}>
                remember · outcome · cass
              </text>
            ) : null}
            {g.wide ? (
              <text x={g.pullLabel.x} y={g.pullLabel.y + 13} textAnchor="middle" fontSize="10.5" fill="#94a3b8" style={{ opacity: active === "pull" ? 1 : 0.45 }}>
                pack · recall · search · why
              </text>
            ) : null}

            {/* ee */}
            <rect x={ee.x} y={ee.y} width={ee.w} height={ee.h} rx="18" fill="rgba(167,139,250,0.035)" stroke="rgba(167,139,250,0.3)" />
            <text x={inner.x + 2} y={ee.y + 30} fill="#fff" fontSize="17" fontWeight="800">
              ee
              <tspan dx="8" fontSize="11" fontWeight="500" fill="#94a3b8">
                stores memory
              </tspan>
            </text>

            <rect
              x={inner.x}
              y={truth.y}
              width={inner.w}
              height={truth.h}
              rx="11"
              fill={touched.has("db") ? "rgba(167,139,250,0.14)" : "rgba(167,139,250,0.05)"}
              stroke={touched.has("db") ? PULL : "rgba(167,139,250,0.4)"}
              strokeWidth={touched.has("db") ? 1.6 : 1}
              className="transition-all duration-500"
            />
            {touched.has("db") ? <circle cx={inner.x + inner.w - 22} cy={truth.y + truth.h / 2} r="22" fill={`url(#${id}-glow)`} /> : null}
            <text x={inner.x + 14} y={truth.y + 22} fill="#fff" fontSize="12.5" fontWeight="700">
              FrankenSQLite + SQLModel
            </text>
            <text x={inner.x + 14} y={truth.y + 40} className="mono" fontSize="9.5" letterSpacing="1.5" fill="#c4b5fd">
              SOURCE OF TRUTH
            </text>
            <circle cx={inner.x + inner.w - 22} cy={truth.y + truth.h / 2} r="5" fill={touched.has("db") ? "#fff" : "#6d5bb0"} />

            <text x={inner.x} y={derivedTop - 9} className="mono" fontSize="9" letterSpacing="1.5" fill="#77839c">
              DERIVED · REBUILDABLE
            </text>
            <text x={inner.x + inner.w} y={derivedTop - 9} textAnchor="end" className="mono" fontSize="9" fill="#77839c">
              ee index rebuild
            </text>
            <path
              d={`M ${inner.x + 10} ${truth.y + truth.h} V ${derivedTop + 2 * (rowH + 7) + rowH / 2}`}
              stroke="rgba(167,139,250,0.35)"
              strokeDasharray="2 4"
              fill="none"
            />
            {derived.map((row, index) => {
              const y = derivedTop + index * (rowH + 7);
              const on = row.store !== "cache" && touched.has(row.store);
              return (
                <g key={row.title}>
                  <path d={`M ${inner.x + 10} ${y + rowH / 2} H ${inner.x + 20}`} stroke="rgba(167,139,250,0.35)" />
                  <rect
                    x={inner.x + 20}
                    y={y}
                    width={inner.w - 20}
                    height={rowH}
                    rx="9"
                    fill={on ? "rgba(167,139,250,0.12)" : "rgba(255,255,255,0.015)"}
                    stroke={on ? PULL : "rgba(221,214,254,0.22)"}
                    strokeDasharray="4 3"
                    className="transition-all duration-500"
                  />
                  <text x={inner.x + 32} y={y + 21.5} fontSize="11.5" fontWeight="700" fill={on ? "#fff" : "#cbd5e1"}>
                    {row.title}
                    <tspan dx="8" fontSize="10" fontWeight="400" fill="#77839c">
                      {inner.w > 250 ? row.note : ""}
                    </tspan>
                  </text>
                </g>
              );
            })}
          </svg>
        </div>

        {/* The commands */}
        <div className="mt-6 grid grid-cols-1 gap-5 md:grid-cols-[minmax(0,1fr)_minmax(0,1.15fr)]">
          {/* A reader who tabs into the commands has taken the controls: the walk-through stops under the focus. */}
          <div className="space-y-4" onFocus={() => setAuto(false)}>
            {(["push", "pull"] as const).map((direction) => (
              <div key={direction}>
                <p
                  className="mb-2 flex items-center gap-2 font-mono text-[10px] tracking-[0.2em] uppercase"
                  style={{ color: direction === "push" ? PUSH : PULL }}
                >
                  {direction === "push" ? <ArrowDownToLine className="h-3.5 w-3.5" aria-hidden="true" /> : <ArrowUpFromLine className="h-3.5 w-3.5" aria-hidden="true" />}
                  {direction === "push" ? "Push in · writes" : "Pull out · reads"}
                </p>
                <div className="flex flex-wrap gap-1.5">
                  {COMMANDS.filter((entry) => entry.direction === direction).map((entry) => {
                    const on = entry.id === selected;
                    return (
                      <button
                        key={entry.id}
                        type="button"
                        aria-pressed={on}
                        onClick={() => {
                          setAuto(false);
                          setSelected(entry.id);
                        }}
                        className={cn(
                          "rounded-lg border px-2.5 py-1.5 font-mono text-[12px] transition focus-visible:ring-2 focus-visible:ring-violet-400 focus-visible:outline-none",
                          on
                            ? "border-transparent text-black"
                            : "border-white/10 bg-black/30 text-slate-300 hover:border-white/25 hover:text-white",
                        )}
                        style={on ? { background: direction === "push" ? PUSH : PULL } : undefined}
                      >
                        {entry.name}
                      </button>
                    );
                  })}
                </div>
              </div>
            ))}
          </div>

          {/* Announced only once the reader picks: the walk-through would otherwise speak every 2.8 s. */}
          <div className="min-w-0 rounded-2xl border border-white/10 bg-black/30 p-4" aria-live={auto ? "off" : "polite"}>
            <p className="flex flex-wrap items-center gap-x-3 gap-y-1">
              <span
                className="rounded-full px-2 py-0.5 font-mono text-[10px] font-medium tracking-[0.16em] text-black uppercase"
                style={{ background: command.direction === "push" ? PUSH : PULL }}
              >
                {command.direction === "push" ? "write" : "read"}
              </span>
              <span className="font-mono text-[11px] text-slate-500">
                touches {command.touches.map((store) => (store === "db" ? "source of truth" : store === "index" ? "index" : "graph")).join(" · ")}
              </span>
            </p>
            <pre className="mt-3 overflow-x-auto font-mono text-[12.5px] leading-relaxed whitespace-pre-wrap text-violet-100 [overflow-wrap:anywhere]">
              <code>{command.line}</code>
            </pre>
            <p className="mt-2.5 text-[13px] leading-relaxed text-slate-400">{command.what}</p>
          </div>
        </div>

        <p className="mt-5 text-[13px] leading-relaxed text-slate-400">
          The harness keeps its tools, approvals and prompt loop; ee stores memory and answers queries. The dashed boxes are
          derived data: if the index is lost, <Cmd>ee index rebuild</Cmd> regenerates it from the source of truth.
        </p>
      </div>
    </DemoFrame>
  );
}
