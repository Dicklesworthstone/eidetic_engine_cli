"use client";

import { type ReactNode, type RefObject, useEffect, useMemo, useRef, useState } from "react";

import { Segmented } from "@/components/explain/demo";
import { Reticle } from "@/components/instrument";
import { useInView, useReducedMotion } from "@/lib/hooks";
import { LEVEL_COLOUR, type Level, type PackSection, SECTION_COLOUR } from "@/lib/palette";
import { clamp, cn, grouped, seeded } from "@/lib/utils";

// The hero's specimen: a toy memory store of 96 memories in six topics. Pick a
// task and the memories that match it light up; ee's selection rule (strict MMR,
// 0.75 relevance against 0.25 redundancy, src/pack/mod.rs) packs the most
// relevant, least redundant ones into the token budget, under the balanced
// profile's section quotas (src/models/context_profile.rs). The memories, their
// scores and their token counts are made up for this page; the rule is ee's.

type Topic = "release" | "ci" | "tests" | "migration" | "search" | "docs";

type Memory = {
  id: number;
  topic: Topic;
  level: Level;
  section: PackSection;
  text: string;
  tokens: number;
  quality: number;
  x: number;
  y: number;
  phase: number;
};

const NAMED: [Topic, Level, PackSection, string, number][] = [
  ["release", "procedural", "rules", "Run cargo fmt --check before tagging", 140],
  ["release", "semantic", "decisions", "Releases are cut with dsr, not hosted CI", 210],
  ["release", "episodic", "failures", "Tag pushed before CHANGELOG broke the installer checksum", 330],
  ["release", "episodic", "evidence", "Session 41: v0.19 sums re-signed with minisign", 520],
  ["release", "procedural", "failures", "Never re-tag a published version", 160],
  ["release", "semantic", "artifacts", "scripts/release_binary_probe.sh", 120],
  ["ci", "procedural", "rules", "Exit 124 on the lane is a timeout, not a red", 150],
  ["ci", "episodic", "failures", "A red lib hid a test target that no longer compiled", 360],
  ["ci", "episodic", "evidence", "Run log: 48 Executable lines, 0 errors", 480],
  ["ci", "semantic", "decisions", "Verification runs on the pinned lane", 220],
  ["ci", "procedural", "rules", "Strip ANSI escapes before grading a log", 130],
  ["tests", "procedural", "rules", "--exact matches the full module path", 140],
  ["tests", "episodic", "failures", "A zero-match filter printed ok with nothing run", 300],
  ["tests", "semantic", "rules", "Every tests/*.rs carries the clippy allow header", 150],
  ["tests", "episodic", "evidence", "integration_g_m ran at 96 s a test in debug", 450],
  ["tests", "semantic", "artifacts", "tests/suites/integration_s_z.rs", 110],
  ["migration", "procedural", "rules", "Migrations are forward-only: back up first", 150],
  ["migration", "episodic", "failures", "V133 trigger missed the workspace generation", 340],
  ["migration", "semantic", "decisions", "No down-migrations, by decision record", 200],
  ["migration", "episodic", "evidence", "Session 38: V132 rebuilt pack_rule_items", 470],
  ["migration", "semantic", "artifacts", "src/db/migrations.rs", 100],
  ["search", "procedural", "rules", "The index is derived: rebuild it, never patch it", 150],
  ["search", "semantic", "decisions", "Hybrid retrieval: BM25 plus Model2Vec, fused by rank", 230],
  ["search", "episodic", "failures", "A stale index generation hid new memories", 320],
  ["search", "episodic", "evidence", "Warm daemon: 708 ms against 3,155 ms, same hash", 430],
  ["search", "semantic", "artifacts", "src/search/mod.rs", 100],
  ["docs", "semantic", "rules", "Pinned examples in the docs carry --locked", 140],
  ["docs", "semantic", "decisions", "Schemas live in docs/schemas, versioned", 180],
  ["docs", "episodic", "evidence", "ADR 0087: the pack-hash contract, v7", 400],
];

export const STORE_SIZE = 96;

const TOPICS: Topic[] = ["release", "ci", "tests", "migration", "search", "docs"];

/** Cluster centres in the constellation's unit square, below the HUD's strip. */
const CENTRES: Record<Topic, [number, number]> = {
  release: [0.2, 0.38],
  ci: [0.5, 0.32],
  tests: [0.8, 0.42],
  migration: [0.22, 0.76],
  search: [0.5, 0.64],
  docs: [0.8, 0.8],
};

function buildStore(): { memories: Memory[]; edges: [number, number][] } {
  const random = seeded(19);
  const memories: Memory[] = [];
  const place = (topic: Topic) => {
    const [cx, cy] = CENTRES[topic];
    const angle = random() * Math.PI * 2;
    const radius = 0.035 + random() * 0.11;
    return { x: cx + Math.cos(angle) * radius * 1.15, y: cy + Math.sin(angle) * radius };
  };
  for (const [topic, level, section, text, tokens] of NAMED) {
    memories.push({ id: memories.length, topic, level, section, text, tokens, quality: 0.62 + random() * 0.38, phase: random() * 6.28, ...place(topic) });
  }
  // The rest of the store: session excerpts and working scratch, the long tail every real store has.
  const FILL: [Level, PackSection, string, number][] = [
    ["episodic", "evidence", "session excerpt", 380],
    ["working", "artifacts", "scratch note", 90],
    ["episodic", "evidence", "imported cass span", 420],
  ];
  while (memories.length < STORE_SIZE) {
    const topic = TOPICS[memories.length % TOPICS.length] as Topic;
    const [level, section, kind, tokens] = FILL[memories.length % FILL.length] as [Level, PackSection, string, number];
    memories.push({
      id: memories.length,
      topic,
      level,
      section,
      text: `${kind} · ${topic}`,
      tokens: Math.round(tokens * (0.7 + random() * 0.6)),
      quality: 0.25 + random() * 0.45,
      phase: random() * 6.28,
      ...place(topic),
    });
  }
  // Each memory links to its two nearest neighbours in its topic, and a few links cross topics.
  const edges: [number, number][] = [];
  for (const a of memories) {
    const near = memories
      .filter((b) => b.id !== a.id && b.topic === a.topic)
      .sort((p, q) => Math.hypot(p.x - a.x, p.y - a.y) - Math.hypot(q.x - a.x, q.y - a.y))
      .slice(0, 2);
    for (const b of near) if (a.id < b.id || !edges.some(([s, t]) => s === b.id && t === a.id)) edges.push([a.id, b.id]);
  }
  for (let i = 0; i < 9; i++) {
    const a = Math.floor(random() * 29);
    const b = Math.floor(random() * 29);
    if (a !== b && memories[a]?.topic !== memories[b]?.topic) edges.push([a, b]);
  }
  return { memories, edges };
}

const STORE = buildStore();

export const TASKS = [
  { value: "release", label: "Prepare the release", query: "prepare the v0.20 release", weights: { release: 1, ci: 0.55, docs: 0.35, tests: 0.2 } },
  { value: "flaky", label: "Fix a flaky test", query: "fix the flaky migration test", weights: { tests: 1, migration: 0.85, ci: 0.4 } },
  { value: "ci", label: "Why is CI red?", query: "why is the verification lane red?", weights: { ci: 1, tests: 0.6, release: 0.25 } },
  { value: "search", label: "Refactor search", query: "refactor the search index", weights: { search: 1, migration: 0.3, tests: 0.3, docs: 0.2 } },
] as const;

type TaskId = (typeof TASKS)[number]["value"];

const SECTION_ORDER: PackSection[] = ["rules", "decisions", "failures", "evidence", "artifacts"];
/** The balanced profile's section mix, in basis points (src/models/context_profile.rs). */
export const BALANCED_BPS: Record<PackSection, number> = { rules: 3000, decisions: 2000, failures: 2000, evidence: 2000, artifacts: 1000 };
const MMR_RELEVANCE_WEIGHT = 0.75;
export const CANDIDATE_FLOOR = 0.22;

type Packed = { relevance: number[]; candidates: number[]; selected: number[]; used: Record<PackSection, number>; total: number };

function similarity(a: Memory, b: Memory): number {
  if (a.topic !== b.topic) return 0.05;
  return a.section === b.section ? 0.6 : 0.3;
}

export function pack(task: TaskId, budget: number): Packed {
  const spec = TASKS.find((item) => item.value === task) ?? TASKS[0];
  const weights = spec.weights as Partial<Record<Topic, number>>;
  const relevance = STORE.memories.map((memory) => {
    const topical = weights[memory.topic] ?? 0.04;
    const tilt = memory.level === "working" ? 0.45 : memory.level === "procedural" ? 1.05 : 1;
    return clamp(topical * memory.quality * tilt, 0, 1);
  });
  const candidates = STORE.memories
    .filter((memory) => (relevance[memory.id] ?? 0) >= CANDIDATE_FLOOR)
    .sort((a, b) => (relevance[b.id] ?? 0) - (relevance[a.id] ?? 0) || a.id - b.id)
    .map((memory) => memory.id);
  const quota = Object.fromEntries(SECTION_ORDER.map((section) => [section, Math.ceil((budget * BALANCED_BPS[section]) / 10000)])) as Record<PackSection, number>;
  const used = Object.fromEntries(SECTION_ORDER.map((section) => [section, 0])) as Record<PackSection, number>;
  const selected: number[] = [];
  let total = 0;
  const open = new Set(candidates);
  for (;;) {
    let best = -1;
    let bestGain = Number.NEGATIVE_INFINITY;
    for (const id of open) {
      const memory = STORE.memories[id] as Memory;
      if (used[memory.section] + memory.tokens > quota[memory.section] || total + memory.tokens > budget) continue;
      const maxSim = selected.reduce((most, other) => Math.max(most, similarity(memory, STORE.memories[other] as Memory)), 0);
      const gain = MMR_RELEVANCE_WEIGHT * (relevance[id] ?? 0) - (1 - MMR_RELEVANCE_WEIGHT) * maxSim;
      if (gain > bestGain) {
        bestGain = gain;
        best = id;
      }
    }
    if (best < 0) break;
    const memory = STORE.memories[best] as Memory;
    open.delete(best);
    selected.push(best);
    used[memory.section] += memory.tokens;
    total += memory.tokens;
  }
  return { relevance, candidates, selected, used, total };
}

/** A short, stable digest of the selection, shown as the toy's pack hash (FNV-1a, not ee's BLAKE3). */
export function toyHash(task: string, selected: number[]): string {
  let a = 0x811c9dc5;
  let b = 0x01000193 ^ 0x5bd1e995;
  for (const char of `${task}|${selected.join(",")}`) {
    a = Math.imul(a ^ char.charCodeAt(0), 0x01000193) >>> 0;
    b = Math.imul(b ^ char.charCodeAt(0), 0x5bd1e995) >>> 0;
  }
  return `${a.toString(16).padStart(8, "0")}${b.toString(16).padStart(8, "0")}`;
}

export const BUDGETS = { min: 1000, max: 8000, step: 250, initial: 4000 };

export function Hero({ words }: { words: ReactNode }) {
  const [task, setTask] = useState<TaskId>("release");
  const [budget, setBudget] = useState(BUDGETS.initial);
  const [touched, setTouched] = useState(false);
  const result = useMemo(() => pack(task, budget), [task, budget]);
  const spec = TASKS.find((item) => item.value === task) ?? TASKS[0];
  const hash = toyHash(task, result.selected);
  const order = new Map(result.selected.map((id, index) => [id, index + 1]));

  const box = useRef<HTMLDivElement>(null);
  const inView = useInView(box, "0px");
  const reduced = useReducedMotion();

  // Until the reader takes the controls, the specimen walks through the four tasks by itself.
  useEffect(() => {
    if (touched || reduced || !inView) return;
    const timer = window.setTimeout(() => {
      const at = TASKS.findIndex((item) => item.value === task);
      setTask((TASKS[(at + 1) % TASKS.length] as (typeof TASKS)[number]).value);
    }, 7200);
    return () => window.clearTimeout(timer);
  }, [task, touched, reduced, inView]);

  const sections = SECTION_ORDER.map((section) => ({
    section,
    items: result.selected.filter((id) => STORE.memories[id]?.section === section),
  }));

  return (
    <>
      <div className="mx-auto grid max-w-[100rem] grid-cols-1 xl:min-h-[calc(100svh-9rem)] xl:grid-cols-[minmax(0,0.95fr)_minmax(0,1.05fr)] xl:items-center">
        <div className="rise-in relative order-2 px-3 [--rise-delay:80ms] sm:px-6 lg:px-12 xl:pt-24 xl:pr-12 xl:pl-2">
          <Specimen
            boxRef={box}
            task={task}
            result={result}
            order={order}
            animate={inView && !reduced}
            reduced={reduced}
            query={spec.query}
            hash={hash}
            budget={budget}
            sections={sections}
          />
        </div>
        <div className="rise-in relative z-10 order-1 px-5 pt-24 pb-10 sm:px-8 sm:pt-28 lg:px-12 lg:pt-32 xl:pr-4 xl:pb-20 xl:pl-20">{words}</div>
      </div>

      {/* The control strip: the reader drives the specimen. */}
      <div className="rise-in relative z-10 mx-auto max-w-[100rem] px-3 pb-12 [--rise-delay:240ms] sm:px-6 lg:px-12 xl:px-20">
        <div className="panel grid grid-cols-1 gap-6 rounded-[1.6rem] p-5 sm:p-6 lg:grid-cols-[auto_minmax(0,1fr)_minmax(0,1.15fr)] lg:items-center lg:gap-10">
          <div className="min-w-0">
            <p className="mb-2.5 font-mono text-[10px] font-medium tracking-[0.3em] text-slate-400 uppercase">Pick a task</p>
            <Segmented
              label="Task"
              value={task}
              onChange={(value) => {
                setTouched(true);
                setTask(value);
              }}
              options={TASKS.map((item) => ({ value: item.value, label: item.label, tone: "rule" as const }))}
            />
          </div>
          <label className="block min-w-0">
            <span className="flex items-baseline justify-between gap-3">
              <span className="text-[13px] font-semibold text-slate-200">Token budget</span>
              <span className="font-mono text-[13px] text-white tabular-nums">{grouped(budget)}</span>
            </span>
            <input
              type="range"
              className="demo-range mt-2 w-full [--chapter:#a78bfa]"
              min={BUDGETS.min}
              max={BUDGETS.max}
              step={BUDGETS.step}
              value={budget}
              aria-label="Token budget"
              onChange={(event) => {
                setTouched(true);
                setBudget(Number(event.target.value));
              }}
            />
            <span className="mt-1 flex justify-between font-mono text-[10px] text-slate-500">
              <span>{grouped(BUDGETS.min)}</span>
              <span>ee&apos;s default is 4,000</span>
              <span>{grouped(BUDGETS.max)}</span>
            </span>
          </label>
          <div className="min-w-0" aria-live="polite">
            <p className="flex flex-wrap items-baseline gap-x-2.5">
              <span className="text-4xl font-extrabold tracking-[-0.04em] text-violet-300 tabular-nums sm:text-5xl">
                {result.selected.length}
              </span>
              <span className="text-[15px] text-slate-300">
                of {result.candidates.length} matching memories packed
              </span>
              <span className="font-mono text-[12px] text-slate-500 tabular-nums">
                {grouped(result.total)} / {grouped(budget)} tokens
              </span>
            </p>
            <div className="mt-3 flex h-2.5 overflow-hidden rounded-full bg-white/[0.06]" aria-hidden="true">
              {SECTION_ORDER.map((section) => (
                <span
                  key={section}
                  className="h-full transition-[width] duration-500"
                  style={{ width: `${(result.used[section] / budget) * 100}%`, background: SECTION_COLOUR[section] }}
                />
              ))}
            </div>
            <p className="mt-2 flex flex-wrap gap-x-4 gap-y-1 font-mono text-[11px] text-slate-400">
              {sections.map(({ section, items }) => (
                <span key={section} className="inline-flex items-center gap-1.5">
                  <span aria-hidden="true" className="h-2 w-2 rounded-full" style={{ background: SECTION_COLOUR[section] }} />
                  <span className="text-white tabular-nums">{items.length}</span> {section}
                </span>
              ))}
            </p>
          </div>
        </div>
        <p className="mt-3 px-1 text-[13px] leading-relaxed text-slate-500">
          Pick a task or drag the budget slider. The memories that match the task light up, and ee&apos;s selection rule
          packs the most relevant, least redundant ones that fit the budget and the balanced profile&apos;s section
          quotas. The 96 memories are made up for this page. The selection rule and its weights come from{" "}
          <span className="font-mono text-[12px]">src/pack/mod.rs</span>.
        </p>
      </div>
    </>
  );
}

function Specimen({
  boxRef,
  task,
  result,
  order,
  animate,
  reduced,
  query,
  hash,
  budget,
  sections,
}: {
  boxRef: RefObject<HTMLDivElement | null>;
  task: TaskId;
  result: Packed;
  order: Map<number, number>;
  animate: boolean;
  reduced: boolean;
  query: string;
  hash: string;
  budget: number;
  sections: { section: PackSection; items: number[] }[];
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const card = useRef<HTMLDivElement>(null);
  // The drawing reads the latest selection through a ref, so a new task never restarts the loop.
  const state = useRef({ task, result, changedAt: 0, lit: new Float32Array(STORE_SIZE), sel: new Float32Array(STORE_SIZE), animate, reduced });
  state.current.animate = animate;
  state.current.reduced = reduced;
  if (state.current.task !== task || state.current.result !== result) {
    const taskChanged = state.current.task !== task;
    state.current.task = task;
    state.current.result = result;
    if (taskChanged) state.current.changedAt = typeof performance === "undefined" ? 0 : performance.now();
  }

  useEffect(() => {
    const element = canvas.current;
    const host = boxRef.current;
    if (!element || !host) return;
    const context = element.getContext("2d");
    if (!context) return;
    let frame = 0;
    let width = 0;
    let height = 0;
    let anchor: [number, number] = [0, 0];

    const resize = () => {
      const rect = element.getBoundingClientRect();
      const ratio = Math.min(window.devicePixelRatio || 1, 2);
      width = rect.width;
      height = rect.height;
      element.width = Math.round(width * ratio);
      element.height = Math.round(height * ratio);
      context.setTransform(ratio, 0, 0, ratio, 0, 0);
      const cardRect = card.current?.getBoundingClientRect();
      // The beams end at the pack card's left edge when it sits over the canvas, and at the canvas's foot otherwise.
      if (cardRect && cardRect.top < rect.bottom - 8) {
        anchor = [cardRect.left - rect.left, cardRect.top - rect.top + Math.min(cardRect.height * 0.3, 150)];
      } else {
        anchor = [width / 2, height + 6];
      }
    };
    resize();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(resize);
    observer?.observe(element);
    if (card.current) observer?.observe(card.current);

    const draw = (now: number) => {
      const s = state.current;
      const { relevance, candidates, selected } = s.result;
      const candidateSet = new Set(candidates);
      const selectedAt = new Map(selected.map((id, index) => [id, index]));
      const since = now - s.changedAt;
      const still = s.reduced || !s.animate;
      // Ease each memory's light toward its target; selections arrive one after another, in the order ee chose them.
      for (let id = 0; id < STORE_SIZE; id++) {
        const litTarget = candidateSet.has(id) ? 0.35 + 0.65 * (relevance[id] ?? 0) : 0;
        const rank = selectedAt.get(id);
        const due = rank === undefined ? false : since > 520 + rank * 110;
        const selTarget = rank !== undefined && (due || still) ? 1 : 0;
        if (still) {
          s.lit[id] = litTarget;
          s.sel[id] = selTarget;
        } else {
          const litNow = s.lit[id] ?? 0;
          const selNow = s.sel[id] ?? 0;
          s.lit[id] = litNow + (litTarget - litNow) * (since < 420 ? 0.05 : 0.12);
          s.sel[id] = selNow + (selTarget - selNow) * 0.14;
        }
      }

      context.clearRect(0, 0, width, height);
      const pad = 18;
      const w = width - pad * 2;
      const h = height - pad * 2;
      const t = still ? 0 : now / 1000;
      const point = (memory: Memory): [number, number] => [
        pad + memory.x * w + Math.sin(t * 0.5 + memory.phase) * 2.2,
        pad + memory.y * h + Math.cos(t * 0.42 + memory.phase) * 2.2,
      ];

      // Links.
      context.lineWidth = 1;
      for (const [a, b] of STORE.edges) {
        const ma = STORE.memories[a] as Memory;
        const mb = STORE.memories[b] as Memory;
        const glow = Math.min(s.lit[a] ?? 0, s.lit[b] ?? 0);
        const [ax, ay] = point(ma);
        const [bx, by] = point(mb);
        context.strokeStyle = `rgba(196, 181, 253, ${0.07 + glow * 0.28})`;
        context.beginPath();
        context.moveTo(ax, ay);
        context.lineTo(bx, by);
        context.stroke();
      }

      // The query's pulse, from the centre of the field.
      if (!still && since < 1600) {
        const progress = since / 1600;
        const radius = progress * Math.max(w, h) * 0.75;
        context.strokeStyle = `rgba(232, 121, 249, ${0.45 * (1 - progress)})`;
        context.lineWidth = 2;
        context.beginPath();
        context.arc(pad + 0.44 * w, pad + 0.46 * h, radius, 0, Math.PI * 2);
        context.stroke();
      }

      // Beams from each packed memory to the pack.
      for (const id of selected) {
        const amount = s.sel[id] ?? 0;
        if (amount < 0.02) continue;
        const [x, y] = point(STORE.memories[id] as Memory);
        const colour = SECTION_COLOUR[(STORE.memories[id] as Memory).section];
        const gradient = context.createLinearGradient(x, y, anchor[0], anchor[1]);
        gradient.addColorStop(0, `${colour}${Math.round(amount * 150).toString(16).padStart(2, "0")}`);
        gradient.addColorStop(1, `${colour}00`);
        context.strokeStyle = gradient;
        context.lineWidth = 1.4;
        context.beginPath();
        context.moveTo(x, y);
        const midX = (x + anchor[0]) / 2;
        context.bezierCurveTo(midX, y, midX, anchor[1], anchor[0] - (anchor[0] - x) * (1 - amount), anchor[1] - (anchor[1] - y) * (1 - amount));
        context.stroke();
      }

      // Memories.
      for (const memory of STORE.memories) {
        const [x, y] = point(memory);
        const lit = s.lit[memory.id] ?? 0;
        const sel = s.sel[memory.id] ?? 0;
        const colour = LEVEL_COLOUR[memory.level];
        const radius = 2 + Math.sqrt(memory.tokens) / 9 + lit * 1.6;
        if (lit > 0.05) {
          const halo = context.createRadialGradient(x, y, 0, x, y, radius * 5);
          halo.addColorStop(0, `${colour}${Math.round(lit * 90).toString(16).padStart(2, "0")}`);
          halo.addColorStop(1, `${colour}00`);
          context.fillStyle = halo;
          context.beginPath();
          context.arc(x, y, radius * 5, 0, Math.PI * 2);
          context.fill();
        }
        context.globalAlpha = 0.28 + lit * 0.72;
        context.fillStyle = colour;
        context.beginPath();
        context.arc(x, y, radius, 0, Math.PI * 2);
        context.fill();
        context.globalAlpha = 1;
        if (sel > 0.02) {
          context.strokeStyle = `rgba(255, 255, 255, ${0.85 * sel})`;
          context.lineWidth = 1.5;
          context.beginPath();
          context.arc(x, y, radius + 4 + (1 - sel) * 8, 0, Math.PI * 2);
          context.stroke();
          const rank = selectedAt.get(memory.id);
          if (rank !== undefined && sel > 0.6) {
            context.fillStyle = `rgba(255, 255, 255, ${sel})`;
            context.font = "600 10px ui-monospace, monospace";
            context.fillText(String(rank + 1), x + radius + 6, y - radius - 3);
          }
        }
      }

      if (!still) frame = requestAnimationFrame(draw);
    };

    if (animate && !reduced) {
      frame = requestAnimationFrame(draw);
    } else {
      // A still frame: drawn once, and again whenever the selection changes (this effect re-runs).
      frame = requestAnimationFrame(draw);
    }
    return () => {
      cancelAnimationFrame(frame);
      observer?.disconnect();
    };
    // The loop restarts when animation turns on or off, and redraws a still frame when the selection changes.
  }, [animate, reduced, boxRef, result]);

  return (
    <div ref={boxRef} className="relative">
      <div className="panel-strong relative overflow-hidden rounded-[1.75rem] border bg-[radial-gradient(80%_70%_at_40%_50%,rgba(76,29,149,0.3),transparent_70%),rgba(8,6,20,0.7)] lg:grid lg:h-[34rem] lg:grid-cols-[minmax(0,1fr)_20rem] xl:h-[min(36rem,calc(100svh-13rem))] xl:min-h-[28rem] xl:grid-cols-[minmax(0,1fr)_17rem] 2xl:grid-cols-[minmax(0,1fr)_19rem]">
        <Reticle inset={12} size={16} />
        <div className="relative aspect-[4/3.4] w-full sm:aspect-[16/11] lg:aspect-auto lg:h-full">
          <canvas
            ref={canvas}
            className="absolute inset-0 h-full w-full"
            role="img"
            aria-label={`A toy memory store of ${STORE_SIZE} memories. For the task "${query}", ${result.candidates.length} match and ${result.selected.length} are packed into ${grouped(result.total)} of ${grouped(budget)} tokens.`}
          />
          {/* The HUD: one line of readings along the top. */}
          <dl className="pointer-events-none absolute inset-x-5 top-5 flex flex-wrap gap-x-5 gap-y-1.5 font-mono text-[10px] sm:inset-x-7 sm:top-6 sm:text-[10.5px]">
            <div className="flex min-w-0 basis-full gap-2">
              <dt className="tracking-[0.2em] text-slate-500 uppercase">Query</dt>
              <dd className="min-w-0 truncate text-fuchsia-200" key={query}>
                <span className="type-in inline-block">&ldquo;{query}&rdquo;</span>
              </dd>
            </div>
            <div className="flex gap-2">
              <dt className="tracking-[0.2em] text-slate-500 uppercase">Match</dt>
              <dd className="text-white tabular-nums">{result.candidates.length}/{STORE_SIZE}</dd>
            </div>
            <div className="flex gap-2">
              <dt className="tracking-[0.2em] text-slate-500 uppercase">Packed</dt>
              <dd className="text-violet-200 tabular-nums">{result.selected.length}</dd>
            </div>
            <div className="flex gap-2">
              <dt className="tracking-[0.2em] text-slate-500 uppercase">Tokens</dt>
              <dd className="text-white tabular-nums">
                {grouped(result.total)}/{grouped(budget)}
              </dd>
            </div>
            <div className="flex gap-2">
              <dt className="tracking-[0.2em] text-slate-500 uppercase">Hash</dt>
              <dd className="text-cyan-200 tabular-nums">
                {hash.slice(0, 8)}…{hash.slice(-4)}
              </dd>
            </div>
          </dl>
          {/* The level legend. */}
          <p className="pointer-events-none absolute bottom-4 left-4 hidden flex-wrap gap-x-3 gap-y-1 font-mono text-[10px] text-slate-400 sm:bottom-6 sm:left-6 sm:flex">
            {(Object.keys(LEVEL_COLOUR) as Level[]).map((level) => (
              <span key={level} className="inline-flex items-center gap-1.5">
                <span aria-hidden="true" className="h-2 w-2 rounded-full" style={{ background: LEVEL_COLOUR[level] }} />
                {level}
              </span>
            ))}
          </p>
        </div>
        {/* The pack: beside the field on wide screens, below it on phones; the beams end at its edge. */}
        <div
          ref={card}
          className="relative flex flex-col border-t border-white/[0.07] bg-[#07051a]/80 p-4 sm:p-5 lg:overflow-hidden lg:border-t-0 lg:border-l lg:border-violet-300/15 lg:py-7"
        >
          <div className="flex items-center justify-between gap-3">
            <p className="font-mono text-[10px] font-medium tracking-[0.22em] text-violet-300 uppercase">Context pack</p>
            <span className="rounded-full border border-fuchsia-300/35 bg-fuchsia-300/10 px-2 py-0.5 text-[9px] font-black tracking-[0.12em] whitespace-nowrap text-fuchsia-200 uppercase">
              Toy · ee&apos;s rule
            </span>
          </div>
          <ol className="mt-3 space-y-2.5 font-mono text-[11px] leading-snug" key={task}>
            {sections
              .filter(({ items }) => items.length > 0)
              .map(({ section, items }) => {
                // Wide screens show two items a section (one of evidence, the longest); phones show one.
                const shown = section === "evidence" ? 1 : 2;
                return (
                  <li key={section}>
                    <p className="text-[10px] tracking-[0.16em] uppercase" style={{ color: SECTION_COLOUR[section] }}>
                      ## {section}
                    </p>
                    <ul className="mt-1 space-y-1">
                      {items.slice(0, shown).map((id, at) => {
                        const memory = STORE.memories[id] as Memory;
                        return (
                          <li
                            key={id}
                            className={cn("type-in flex gap-2 text-slate-300", at > 0 && "max-lg:hidden")}
                            style={{ animationDelay: `${(order.get(id) ?? 0) * 90}ms` }}
                          >
                            <span className="shrink-0 text-slate-500 tabular-nums">{order.get(id)}</span>
                            <span className="min-w-0 truncate">{memory.text}</span>
                          </li>
                        );
                      })}
                      {items.length > 1 ? (
                        <li className="pl-4 text-slate-500 lg:hidden">+ {items.length - 1} more</li>
                      ) : null}
                      {items.length > shown ? (
                        <li className="pl-4 text-slate-500 max-lg:hidden">+ {items.length - shown} more</li>
                      ) : null}
                    </ul>
                  </li>
                );
              })}
          </ol>
          <p className={cn("mt-3 border-t border-white/[0.06] pt-2.5 font-mono text-[10px] text-slate-500 lg:mt-auto")}>
            every item: provenance · scores · why
          </p>
        </div>
      </div>
    </div>
  );
}
