"use client";

import { useEffect, useMemo, useRef, useState } from "react";

import { DemoFrame, Segmented } from "@/components/explain/demo";
import { useFigureWidth, useReducedMotion } from "@/lib/hooks";
import { GRAPH, LEVEL_COLOUR, type Level, WARNING } from "@/lib/palette";
import { cn, fixed } from "@/lib/utils";

// A toy memory graph: 22 memories of the kind a project accumulates, in three
// topics joined by a few bridges. Every score below is computed in the browser
// by the textbook algorithm at toy scale. None of it is ee's output; each lens
// names the ee command that computes the same quantity over a real store.

type Relation = "supports" | "derived_from" | "contradicts" | "related" | "co_mention";

type Node = { label: string; level: Level; x: number; y: number };

const NODES: readonly Node[] = [
  { label: "release checklist", level: "procedural", x: 0.23, y: 0.46 },
  { label: "fmt before tag", level: "procedural", x: 0.12, y: 0.24 },
  { label: "sign SHA256SUMS", level: "procedural", x: 0.06, y: 0.6 },
  { label: "bump the version", level: "semantic", x: 0.27, y: 0.16 },
  { label: "v0.19 cut by hand", level: "episodic", x: 0.13, y: 0.86 },
  { label: "refresh the tap", level: "procedural", x: 0.29, y: 0.75 },
  { label: "skip fmt if rushed", level: "episodic", x: 0.04, y: 0.05 },
  { label: "task: cut v0.20", level: "working", x: 0.42, y: 0.2 },
  { label: "verify before tag", level: "procedural", x: 0.5, y: 0.46 },
  { label: "release notes", level: "semantic", x: 0.45, y: 0.8 },
  { label: "flaky CI test", level: "episodic", x: 0.66, y: 0.1 },
  { label: "pin a warm worker", level: "procedural", x: 0.8, y: 0.36 },
  { label: "--locked required", level: "procedural", x: 0.64, y: 0.38 },
  { label: "debug is 10x slower", level: "semantic", x: 0.93, y: 0.12 },
  { label: "timeout ≠ OOM", level: "semantic", x: 0.95, y: 0.52 },
  { label: "SIGKILL: no verdict", level: "procedural", x: 0.73, y: 0.6 },
  { label: "migration V133", level: "episodic", x: 0.62, y: 0.78 },
  { label: "no down-migration", level: "semantic", x: 0.77, y: 0.92 },
  { label: "back up first", level: "procedural", x: 0.55, y: 0.96 },
  { label: "single-writer WAL", level: "semantic", x: 0.88, y: 0.76 },
  { label: "daemon write owner", level: "semantic", x: 0.97, y: 0.96 },
  { label: "APFS export base", level: "procedural", x: 0.97, y: 0.3 },
];

/** [a, b, relation]: memory a leans on memory b (it was derived from it, is supported by it, or cites it). */
const EDGES: readonly (readonly [number, number, Relation])[] = [
  [0, 1, "related"],
  [0, 2, "related"],
  [0, 3, "related"],
  [0, 5, "related"],
  [1, 4, "derived_from"],
  [2, 4, "derived_from"],
  [5, 4, "derived_from"],
  [3, 4, "co_mention"],
  [1, 3, "related"],
  [2, 5, "related"],
  [1, 2, "related"],
  [3, 5, "related"],
  [6, 1, "contradicts"],
  [7, 0, "related"],
  [7, 8, "related"],
  [7, 3, "related"],
  [0, 8, "related"],
  [8, 1, "related"],
  [8, 12, "supports"],
  [8, 15, "related"],
  [8, 11, "related"],
  [0, 9, "related"],
  [9, 16, "co_mention"],
  [9, 4, "co_mention"],
  [12, 10, "derived_from"],
  [11, 10, "derived_from"],
  [11, 13, "supports"],
  [12, 11, "related"],
  [11, 15, "related"],
  [11, 14, "related"],
  [12, 15, "supports"],
  [15, 14, "supports"],
  [14, 13, "related"],
  [10, 13, "co_mention"],
  [21, 11, "related"],
  [17, 16, "derived_from"],
  [18, 17, "supports"],
  [18, 16, "derived_from"],
  [16, 19, "co_mention"],
  [17, 19, "related"],
  [18, 19, "related"],
  [20, 19, "supports"],
];

const N = NODES.length;

const RELATION_STYLE: Record<Relation, { colour: string; dash?: string; label: string }> = {
  supports: { colour: GRAPH, label: "supports" },
  derived_from: { colour: "#38bdf8", label: "derived_from" },
  contradicts: { colour: "#fb7185", dash: "5 4", label: "contradicts" },
  related: { colour: "rgba(203,213,225,0.55)", label: "related" },
  co_mention: { colour: "rgba(203,213,225,0.45)", dash: "1 4", label: "co_mention" },
};

const COMMUNITY_COLOURS = ["#a78bfa", "#38bdf8", "#fbbf24", "#f472b6"];

// ─── The algorithms, at toy scale ────────────────────────────────────────────

const undirected: number[][] = Array.from({ length: N }, () => []);
const outward: number[][] = Array.from({ length: N }, () => []);
for (const [a, b, relation] of EDGES) {
  (undirected[a] as number[]).push(b);
  (undirected[b] as number[]).push(a);
  // Rank and authority flow along leaning links; in this toy a contradiction carries none.
  if (relation !== "contradicts") (outward[a] as number[]).push(b);
}
for (const list of undirected) list.sort((x, y) => x - y);

const at = <T,>(list: readonly T[], index: number) => list[index] as T;

/** PageRank, damping 0.85, dangling mass spread evenly. */
function pageRank(): number[] {
  let rank = new Array<number>(N).fill(1 / N);
  for (let iteration = 0; iteration < 100; iteration++) {
    let dangling = 0;
    for (let u = 0; u < N; u++) if ((outward[u] as number[]).length === 0) dangling += at(rank, u);
    const next = new Array<number>(N).fill((1 - 0.85) / N + (0.85 * dangling) / N);
    for (let u = 0; u < N; u++) {
      const targets = outward[u] as number[];
      for (const v of targets) next[v] = at(next, v) + (0.85 * at(rank, u)) / targets.length;
    }
    rank = next;
  }
  return rank;
}

/** HITS: authorities are leaned on by good hubs; hubs lean on good authorities. */
function hits(): { hub: number[]; authority: number[] } {
  let hub = new Array<number>(N).fill(1);
  let authority = new Array<number>(N).fill(1);
  for (let iteration = 0; iteration < 80; iteration++) {
    const nextAuthority = new Array<number>(N).fill(0);
    for (let u = 0; u < N; u++) for (const v of outward[u] as number[]) nextAuthority[v] = at(nextAuthority, v) + at(hub, u);
    const authorityNorm = Math.hypot(...nextAuthority) || 1;
    authority = nextAuthority.map((value) => value / authorityNorm);
    const nextHub = new Array<number>(N).fill(0);
    for (let u = 0; u < N; u++) for (const v of outward[u] as number[]) nextHub[u] = at(nextHub, u) + at(authority, v);
    const hubNorm = Math.hypot(...nextHub) || 1;
    hub = nextHub.map((value) => value / hubNorm);
  }
  return { hub, authority };
}

/** Brandes' betweenness on the undirected graph, normalised to [0, 1]. */
function betweenness(): number[] {
  const score = new Array<number>(N).fill(0);
  for (let source = 0; source < N; source++) {
    const stack: number[] = [];
    const parents: number[][] = Array.from({ length: N }, () => []);
    const paths = new Array<number>(N).fill(0);
    const distance = new Array<number>(N).fill(-1);
    paths[source] = 1;
    distance[source] = 0;
    const queue = [source];
    for (let head = 0; head < queue.length; head++) {
      const v = at(queue, head);
      stack.push(v);
      for (const w of undirected[v] as number[]) {
        if (at(distance, w) < 0) {
          distance[w] = at(distance, v) + 1;
          queue.push(w);
        }
        if (at(distance, w) === at(distance, v) + 1) {
          paths[w] = at(paths, w) + at(paths, v);
          (parents[w] as number[]).push(v);
        }
      }
    }
    const delta = new Array<number>(N).fill(0);
    while (stack.length > 0) {
      const w = stack.pop() as number;
      for (const v of parents[w] as number[]) delta[v] = at(delta, v) + (at(paths, v) / at(paths, w)) * (1 + at(delta, w));
      if (w !== source) score[w] = at(score, w) + at(delta, w);
    }
  }
  const pairs = ((N - 1) * (N - 2)) / 2;
  return score.map((value) => value / 2 / pairs);
}

/** Articulation points (Tarjan): memories whose removal splits the graph. */
function articulationPoints(): boolean[] {
  const discovered = new Array<number>(N).fill(-1);
  const low = new Array<number>(N).fill(0);
  const cut = new Array<boolean>(N).fill(false);
  let clock = 0;
  const visit = (u: number, parent: number) => {
    discovered[u] = clock;
    low[u] = clock;
    clock += 1;
    let children = 0;
    for (const v of undirected[u] as number[]) {
      if (at(discovered, v) < 0) {
        children += 1;
        visit(v, u);
        low[u] = Math.min(at(low, u), at(low, v));
        if (parent >= 0 && at(low, v) >= at(discovered, u)) cut[u] = true;
      } else if (v !== parent) low[u] = Math.min(at(low, u), at(discovered, v));
    }
    if (parent < 0 && children > 1) cut[u] = true;
  };
  visit(0, -1);
  return cut;
}

/** How many memories lose their path to the rest when `removed` goes. */
function cutOff(removed: number): number {
  const seen = new Array<boolean>(N).fill(false);
  seen[removed] = true;
  let largest = 0;
  for (let start = 0; start < N; start++) {
    if (seen[start]) continue;
    let size = 0;
    const queue = [start];
    seen[start] = true;
    for (let head = 0; head < queue.length; head++) {
      size += 1;
      for (const v of undirected[at(queue, head)] as number[]) {
        if (!seen[v]) {
          seen[v] = true;
          queue.push(v);
        }
      }
    }
    largest = Math.max(largest, size);
  }
  return N - 1 - largest;
}

/** Synchronous label propagation: each memory takes its neighbourhood's commonest label, ties to the smallest. */
function communities(): number[] {
  let labels = Array.from({ length: N }, (_, index) => index);
  for (let iteration = 0; iteration < 50; iteration++) {
    const next = labels.map((own, u) => {
      const votes = new Map<number, number>([[own, 1]]);
      for (const v of undirected[u] as number[]) votes.set(at(labels, v), (votes.get(at(labels, v)) ?? 0) + 1);
      const most = Math.max(...votes.values());
      return Math.min(...[...votes.entries()].filter(([, count]) => count === most).map(([label]) => label));
    });
    if (next.every((label, index) => label === labels[index])) break;
    labels = next;
  }
  // Renumber 0, 1, 2… in order of first appearance.
  const order = new Map<number, number>();
  return labels.map((label) => {
    if (!order.has(label)) order.set(label, order.size);
    return order.get(label) as number;
  });
}

/** Core numbers by peeling: the largest k for which a memory sits in a subgraph of minimum degree k. */
function coreNumbers(): number[] {
  const degree = undirected.map((list) => list.length);
  const core = new Array<number>(N).fill(0);
  const removed = new Array<boolean>(N).fill(false);
  let k = 0;
  for (let step = 0; step < N; step++) {
    let u = -1;
    for (let i = 0; i < N; i++) if (!removed[i] && (u < 0 || at(degree, i) < at(degree, u))) u = i;
    k = Math.max(k, at(degree, u));
    core[u] = k;
    removed[u] = true;
    for (const v of undirected[u] as number[]) if (!removed[v]) degree[v] = at(degree, v) - 1;
  }
  return core;
}

const SCORES = (() => {
  const rank = pageRank();
  const { hub, authority } = hits();
  const between = betweenness();
  const cut = articulationPoints();
  return {
    rank,
    hub,
    authority,
    between,
    cut,
    cutOff: cut.map((isCut, index) => (isCut ? cutOff(index) : 0)),
    community: communities(),
    core: coreNumbers(),
  };
})();

const normalise = (values: readonly number[]) => {
  const top = Math.max(...values) || 1;
  return values.map((value) => value / top);
};

// ─── Lenses ─────────────────────────────────────────────────────────────────

type Lens = "pagerank" | "hits" | "bridges" | "communities" | "core";

const LENSES: Record<
  Lens,
  { label: string; question: string; commands: string[]; note: string; metric: (i: number) => number; show: (i: number) => string }
> = {
  pagerank: {
    label: "PageRank",
    question: "Which memories does everything else lean on?",
    commands: ["ee graph pagerank --limit 10", "ee graph centrality --algorithm pagerank"],
    note: "Rank flows from each memory to the memories it leans on, so it collects in the facts and incidents the rules were built from. In this toy a contradicts link passes no rank.",
    metric: (i) => at(SCORES.rank, i),
    show: (i) => fixed(at(SCORES.rank, i), 3),
  },
  hits: {
    label: "HITS",
    question: "Which memories are hubs that lean on many others, and which are the authorities they lean on?",
    commands: ["ee graph hits --limit 10"],
    note: "Size is authority; the outer ring is hub score. With graph.feature.hits_profiles on and graph scores available, the grounding context profile boosts authorities, orientation boosts hubs, and balanced boosts both at half that weight.",
    metric: (i) => at(SCORES.authority, i),
    show: (i) => `auth ${fixed(at(SCORES.authority, i), 2)} · hub ${fixed(at(SCORES.hub, i), 2)}`,
  },
  bridges: {
    label: "Bridges",
    question: "Which memories connect two topics, and which would split the graph if removed?",
    commands: ["ee graph betweenness --limit 10", "ee graph articulation", "ee insights --section bridges --json"],
    note: "Size is betweenness; a dashed ring marks an articulation point, and removing one in curation disconnects part of the graph. ee proximity explains a pair's Gomory-Hu min cut.",
    metric: (i) => at(SCORES.between, i),
    show: (i) => fixed(at(SCORES.between, i), 3),
  },
  communities: {
    label: "Communities",
    question: "Which memories form one topic?",
    commands: ["ee graph communities", "ee graph louvain"],
    note: "Computed here by synchronous label propagation with ties broken to the smallest label, so the same graph always splits the same way. Pack DNA reports which community a pack's weight sits in.",
    metric: (i) => (undirected[i] as number[]).length,
    show: (i) => `community ${at(SCORES.community, i) + 1}`,
  },
  core: {
    label: "k-core",
    question: "Which memories form the densely linked core, and which sit on the periphery?",
    commands: ["ee graph k-core", "ee insights --section knowledgeSkyline --json"],
    note: "Remove every memory with fewer than k links, raise k and repeat; the memories removed last form the core. ee's knowledge skyline and structural decay use onion layers, which number each round of this peeling.",
    metric: (i) => at(SCORES.core, i),
    show: (i) => `${at(SCORES.core, i)}-core`,
  },
};

const LENS_ORDER: Lens[] = ["pagerank", "hits", "bridges", "communities", "core"];

const LEVEL_NAMES: Record<Level, string> = {
  working: "working",
  episodic: "episodic",
  semantic: "semantic",
  procedural: "procedural",
};

export function GraphLens() {
  const [lens, setLens] = useState<Lens>("pagerank");
  const [selected, setSelected] = useState<number | null>(null);
  // Opening a memory from the ranked list, or clearing it, swaps the side panel and removes the button that held
  // the focus. Those two presses ask for the focus to land on the first button of whatever replaced it.
  const side = useRef<HTMLDivElement>(null);
  const refocus = useRef(false);
  useEffect(() => {
    if (!refocus.current) return;
    refocus.current = false;
    side.current?.querySelector<HTMLButtonElement>("button")?.focus();
  }, [selected]);
  const reduced = useReducedMotion();
  const { ref, width } = useFigureWidth(760);
  const narrow = width < 560;
  const height = narrow ? Math.round(width * 1.25) : Math.round(Math.min(480, Math.max(360, width * 0.58)));
  const pad = { x: narrow ? 14 : 22, top: 22, bottom: 34 };
  const place = (node: Node) => ({
    x: pad.x + node.x * (width - 2 * pad.x),
    y: pad.top + node.y * (height - pad.top - pad.bottom),
  });

  const config = LENSES[lens];
  const metric = useMemo(() => normalise(NODES.map((_, index) => config.metric(index))), [config]);
  const ranked = useMemo(
    () =>
      NODES.map((_, index) => index)
        .sort((a, b) => config.metric(b) - config.metric(a) || a - b)
        .slice(0, 5),
    [config],
  );
  const hubs = useMemo(() => normalise(SCORES.hub), []);

  const radius = (index: number) => {
    const scale = narrow ? 0.75 : 1;
    if (lens === "communities") return (5 + 1.4 * (undirected[index] as number[]).length) * scale;
    if (lens === "core") return (4 + 2.4 * at(SCORES.core, index)) * scale;
    return (4.5 + 13 * Math.sqrt(at(metric, index))) * scale;
  };
  const fill = (index: number) => {
    const node = NODES[index] as Node;
    if (lens === "communities") return COMMUNITY_COLOURS[at(SCORES.community, index) % COMMUNITY_COLOURS.length] as string;
    return LEVEL_COLOUR[node.level];
  };
  const neighbours = selected === null ? null : new Set([selected, ...(undirected[selected] as number[])]);
  // Labels are placed greedily, most important first: the selected memory, then by this lens's score.
  // One whose box would overlap a label already placed is left out, so none ever collide. When a
  // memory is selected, only its neighbourhood is labelled.
  const fontSize = narrow ? 10 : 11;
  const charWidth = fontSize * 0.54;
  const labels: { index: number; x: number; y: number }[] = [];
  {
    const boxes: { x0: number; x1: number; y0: number; y1: number }[] = [];
    const order = NODES.map((_, index) => index).sort((a, b) => config.metric(b) - config.metric(a) || a - b);
    for (const index of selected === null ? order : [selected, ...order.filter((i) => i !== selected)]) {
      if (narrow && labels.length >= 9) break;
      if (neighbours !== null && !neighbours.has(index)) continue;
      const p = place(NODES[index] as Node);
      const half = ((NODES[index] as Node).label.length * charWidth) / 2;
      const x = Math.min(width - half - 4, Math.max(half + 4, p.x));
      const y = p.y + radius(index) + fontSize + 2;
      const box = { x0: x - half - 3, x1: x + half + 3, y0: y - fontSize, y1: y + 3 };
      if (boxes.some((b) => box.x0 < b.x1 && b.x0 < box.x1 && box.y0 < b.y1 && b.y0 < box.y1)) continue;
      boxes.push(box);
      labels.push({ index, x, y });
    }
  }
  const coreMax = Math.max(...SCORES.core);
  const transition = reduced ? undefined : "r 650ms cubic-bezier(0.19, 1, 0.22, 1), fill 500ms, opacity 300ms";

  // Soft blobs behind each community, from its members' positions.
  const blobs = useMemo(() => {
    if (lens !== "communities") return [];
    const groups = new Map<number, { x: number; y: number }[]>();
    NODES.forEach((node, index) => {
      const key = at(SCORES.community, index);
      const list = groups.get(key) ?? [];
      list.push(place(node));
      groups.set(key, list);
    });
    return [...groups.entries()].map(([key, points]) => {
      const cx = points.reduce((sum, p) => sum + p.x, 0) / points.length;
      const cy = points.reduce((sum, p) => sum + p.y, 0) / points.length;
      const rx = Math.max(40, ...points.map((p) => Math.abs(p.x - cx))) + 30;
      const ry = Math.max(40, ...points.map((p) => Math.abs(p.y - cy))) + 30;
      return { key, cx, cy, rx, ry };
    });
    // place depends on width and height only.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [lens, width, height]);

  const ariaSummary = `${config.label} over a toy graph of ${N} memories. Highest: ${ranked
    .map((index) => `${(NODES[index] as Node).label} (${config.show(index)})`)
    .join(", ")}.`;

  return (
    <DemoFrame
      eyebrow="FrankenNetworkX · graph analytics over memory links"
      title={`Five lenses on a toy graph of ${N} memories.`}
      basis="toy"
      source={
        <>
          README · Graph and Graph-derived insights command tables · docs/configuration/graph.md · every
          graph.feature.* flag defaults to false · src/db/mod.rs · MemoryLinkRelation · src/models/context_profile.rs ·
          hits_boost_weights (grounding, orientation, balanced) · src/graph/decay.rs · compute_onion_layers · personalized
          PageRank: graph_ppr_upstream_unavailable
        </>
      }
    >
      <Segmented
        label="Graph lens"
        value={lens}
        onChange={(value) => setLens(value)}
        options={LENS_ORDER.map((id) => ({ value: id, label: LENSES[id].label, tone: "rule" as const }))}
      />

      <div className="mt-5 grid grid-cols-1 gap-6 lg:grid-cols-12">
        <div className="min-w-0 lg:col-span-8">
          <div ref={ref} className="relative overflow-hidden rounded-2xl border border-white/[0.07] bg-black/30">
            <svg
              className="diagram block h-auto w-full select-none"
              width={width}
              height={height}
              viewBox={`0 0 ${width} ${height}`}
              role="img"
              aria-label={ariaSummary}
              onClick={() => setSelected(null)}
            >
              <defs>
                <marker id="graph-lens-arrow" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="6" markerHeight="6" orient="auto-start-reverse">
                  <path d="M0,0 L8,4 L0,8 z" fill="rgba(203,213,225,0.55)" />
                </marker>
              </defs>

              {blobs.map((blob) => (
                <ellipse
                  key={blob.key}
                  cx={blob.cx}
                  cy={blob.cy}
                  rx={blob.rx}
                  ry={blob.ry}
                  fill={COMMUNITY_COLOURS[blob.key % COMMUNITY_COLOURS.length]}
                  opacity={0.07}
                />
              ))}

              {EDGES.map(([a, b, relation], index) => {
                const from = place(NODES[a] as Node);
                const to = place(NODES[b] as Node);
                const style = RELATION_STYLE[relation];
                const dx = to.x - from.x;
                const dy = to.y - from.y;
                const length = Math.hypot(dx, dy) || 1;
                const ra = radius(a) + 2;
                const rb = radius(b) + 4;
                const lit = neighbours ? neighbours.has(a) && neighbours.has(b) && (a === selected || b === selected) : true;
                const directed = lens === "pagerank" || lens === "hits";
                return (
                  <line
                    key={index}
                    x1={from.x + (dx / length) * ra}
                    y1={from.y + (dy / length) * ra}
                    x2={to.x - (dx / length) * rb}
                    y2={to.y - (dy / length) * rb}
                    stroke={style.colour}
                    strokeWidth={lit && selected !== null ? 1.8 : 1.1}
                    strokeDasharray={style.dash}
                    opacity={lit ? (selected !== null ? 0.95 : 0.6) : 0.1}
                    markerEnd={directed && relation !== "contradicts" ? "url(#graph-lens-arrow)" : undefined}
                    style={{ transition: reduced ? undefined : "opacity 300ms" }}
                  />
                );
              })}

              {NODES.map((node, index) => {
                const p = place(node);
                const r = radius(index);
                const dim = neighbours !== null && !neighbours.has(index);
                const isCut = lens === "bridges" && at(SCORES.cut, index);
                const coreShell = lens === "core" ? at(SCORES.core, index) / coreMax : 1;
                return (
                  <g
                    key={node.label}
                    className="cursor-pointer"
                    opacity={dim ? 0.22 : 1}
                    style={{ transition: reduced ? undefined : "opacity 300ms" }}
                    onClick={(event) => {
                      event.stopPropagation();
                      setSelected(selected === index ? null : index);
                    }}
                  >
                    {/* A wide, invisible target, so a small node is still easy to tap. */}
                    <circle cx={p.x} cy={p.y} r={Math.max(r + 6, 16)} fill="transparent" />
                    {lens === "hits" ? (
                      <circle
                        cx={p.x}
                        cy={p.y}
                        r={r + 3 + 7 * Math.sqrt(at(hubs, index))}
                        fill="none"
                        stroke="#e2e8f0"
                        strokeOpacity={0.15 + 0.6 * at(hubs, index)}
                        strokeWidth={1 + 3 * at(hubs, index)}
                        style={{ transition }}
                      />
                    ) : null}
                    {isCut ? (
                      <circle cx={p.x} cy={p.y} r={r + 5} fill="none" stroke={WARNING} strokeWidth={1.6} strokeDasharray="3 3" style={{ transition }} />
                    ) : null}
                    {selected === index ? (
                      <circle cx={p.x} cy={p.y} r={r + 9} fill="none" stroke="#fff" strokeOpacity={0.8} strokeWidth={1.2} style={{ transition }} />
                    ) : null}
                    <circle
                      cx={p.x}
                      cy={p.y}
                      r={r}
                      fill={fill(index)}
                      fillOpacity={0.25 + 0.75 * coreShell}
                      stroke={INK_EDGE}
                      strokeWidth={1.5}
                      style={{ transition }}
                    />
                    <circle cx={p.x - r * 0.3} cy={p.y - r * 0.32} r={Math.max(1.2, r * 0.28)} fill="#fff" opacity={0.55} style={{ transition }} />
                  </g>
                );
              })}

              {/* Labels in their own layer, so no later node is painted over one. */}
              <g pointerEvents="none">
                {labels.map(({ index, x, y }) => (
                  <NodeLabel key={index} x={x} y={y} size={fontSize} text={(NODES[index] as Node).label} strong={selected === index} />
                ))}
              </g>
            </svg>
          </div>

          <div className="mt-3 flex flex-wrap items-center gap-x-4 gap-y-1.5 text-[11px] text-slate-400">
            {lens === "communities"
              ? [...new Set(SCORES.community)].map((community) => (
                  <span key={community} className="flex items-center gap-1.5">
                    <span aria-hidden="true" className="h-2.5 w-2.5 rounded-full" style={{ background: COMMUNITY_COLOURS[community % COMMUNITY_COLOURS.length] }} />
                    community {community + 1}
                  </span>
                ))
              : (Object.keys(LEVEL_NAMES) as Level[]).map((level) => (
                  <span key={level} className="flex items-center gap-1.5">
                    <span aria-hidden="true" className="h-2.5 w-2.5 rounded-full" style={{ background: LEVEL_COLOUR[level] }} />
                    {LEVEL_NAMES[level]}
                  </span>
                ))}
            <span aria-hidden="true" className="hidden h-3 w-px bg-white/10 sm:block" />
            {(Object.keys(RELATION_STYLE) as Relation[]).map((relation) => (
              <span key={relation} className="flex items-center gap-1.5 font-mono text-[10px]">
                <svg aria-hidden="true" width="18" height="6">
                  <line x1="0" y1="3" x2="18" y2="3" stroke={RELATION_STYLE[relation].colour} strokeWidth="1.6" strokeDasharray={RELATION_STYLE[relation].dash} />
                </svg>
                {RELATION_STYLE[relation].label}
              </span>
            ))}
          </div>
        </div>

        <div ref={side} className="min-w-0 space-y-4 lg:col-span-4">
          <div className="rounded-2xl border border-[color:var(--chapter)]/30 bg-[color:var(--chapter)]/[0.05] p-4">
            <p className="text-[9px] font-black tracking-[0.2em] text-[color:var(--chapter)] uppercase">The question</p>
            <p className="mt-1.5 text-[15px] leading-snug font-semibold text-white">{config.question}</p>
            <ul className="mt-3 space-y-1">
              {config.commands.map((command) => (
                <li key={command} className="font-mono text-[11.5px] [overflow-wrap:anywhere] text-violet-200">
                  $ {command}
                </li>
              ))}
            </ul>
            <p className="mt-3 text-[12.5px] leading-relaxed text-slate-400">{config.note}</p>
          </div>

          {selected !== null ? (
            <SelectedNode
              index={selected}
              lens={lens}
              onClose={() => {
                refocus.current = true;
                setSelected(null);
              }}
            />
          ) : (
            <div>
              <p className="text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase">Highest under this lens</p>
              <ol className="mt-2 space-y-1">
                {ranked.map((index, place) => (
                  <li key={index}>
                    <button
                      type="button"
                      onClick={() => {
                        refocus.current = true;
                        setSelected(index);
                      }}
                      className="flex w-full items-center gap-2.5 rounded-lg border border-white/[0.06] bg-black/20 px-2.5 py-1.5 text-left text-[13px] text-slate-200 transition hover:border-[color:var(--chapter)]/40 hover:text-white focus-visible:ring-2 focus-visible:ring-violet-400 focus-visible:outline-none"
                    >
                      <span className="w-3 font-mono text-[10px] text-slate-500">{place + 1}</span>
                      <span aria-hidden="true" className="h-2 w-2 shrink-0 rounded-full" style={{ background: fill(index) }} />
                      <span className="min-w-0 flex-1 truncate">{(NODES[index] as Node).label}</span>
                      <span className="font-mono text-[10px] text-slate-500 tabular-nums">{config.show(index)}</span>
                    </button>
                  </li>
                ))}
              </ol>
              <p className="mt-2 text-[11px] text-slate-500">Tap a memory to see its neighborhood.</p>
            </div>
          )}
        </div>
      </div>

      <p className="mt-6 rounded-xl border border-white/[0.07] bg-black/20 px-4 py-3 text-[13px] leading-relaxed text-slate-400">
        <span className="font-semibold text-slate-200">Current status.</span> Each lens is a command you run on demand
        over your own store. The graph features that would change ranking, retention or pack explanations (Pack DNA,
        proximity, HITS profiles, structural decay), and the knowledge skyline, are off by default under{" "}
        <span className="font-mono text-[12px] text-slate-300">graph.feature.*</span>. Personalized PageRank is not
        available in production: a request for it reports{" "}
        <span className="font-mono text-[12px] text-slate-300">graph_ppr_upstream_unavailable</span> and falls back to
        textual ranking.
      </p>
    </DemoFrame>
  );
}

const INK_EDGE = "#05040d";

/** A node's label, already placed inside the figure. */
function NodeLabel({ x, y, size, text, strong }: { x: number; y: number; size: number; text: string; strong: boolean }) {
  return (
    <text
      x={x}
      y={y}
      textAnchor="middle"
      fontSize={size}
      fill={strong ? "#fff" : "#cbd5e1"}
      fontWeight={strong ? 700 : 500}
      paintOrder="stroke"
      stroke="#05040d"
      strokeWidth={3}
      strokeLinejoin="round"
    >
      {text}
    </text>
  );
}

function SelectedNode({ index, lens, onClose }: { index: number; lens: Lens; onClose: () => void }) {
  const node = NODES[index] as Node;
  const leansOn = EDGES.filter(([a]) => a === index).map(([, b, relation]) => ({ other: b, relation }));
  const leanedOnBy = EDGES.filter(([, b]) => b === index).map(([a, , relation]) => ({ other: a, relation }));
  const cut = at(SCORES.cutOff, index);
  const rows: [string, string][] = [
    ["PageRank", fixed(at(SCORES.rank, index), 3)],
    ["Authority · hub", `${fixed(at(SCORES.authority, index), 2)} · ${fixed(at(SCORES.hub, index), 2)}`],
    ["Betweenness", fixed(at(SCORES.between, index), 3)],
    ["Community", String(at(SCORES.community, index) + 1)],
    ["Core", `${at(SCORES.core, index)}-core`],
  ];
  const lensRow = { pagerank: 0, hits: 1, bridges: 2, communities: 3, core: 4 }[lens];
  return (
    <div className="type-in rounded-2xl border border-white/10 bg-black/30 p-4" aria-live="polite">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="flex items-center gap-2 text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase">
            <span aria-hidden="true" className="h-2 w-2 rounded-full" style={{ background: LEVEL_COLOUR[node.level] }} />
            {node.level} memory
          </p>
          <p className="mt-1 text-[15px] font-semibold text-white">{node.label}</p>
        </div>
        <button
          type="button"
          onClick={onClose}
          className="rounded-lg border border-white/10 px-2 py-1 text-[11px] text-slate-400 transition hover:text-white focus-visible:ring-2 focus-visible:ring-violet-400 focus-visible:outline-none"
        >
          Clear
        </button>
      </div>
      <dl className="ruled mt-3">
        {rows.map(([label, value], row) => (
          <div key={label} className="flex items-baseline justify-between gap-3 py-1.5">
            <dt className={cn("text-[12px]", row === lensRow ? "text-[color:var(--chapter)]" : "text-slate-500")}>{label}</dt>
            <dd className={cn("font-mono text-[12px] tabular-nums", row === lensRow ? "text-white" : "text-slate-300")}>{value}</dd>
          </div>
        ))}
      </dl>
      {at(SCORES.cut, index) ? (
        <p className="mt-2 rounded-lg border border-orange-400/30 bg-orange-400/[0.06] px-2.5 py-1.5 text-[12px] text-orange-200">
          Articulation point: remove it and {cut} {cut === 1 ? "memory loses" : "memories lose"} every path to the rest.
        </p>
      ) : null}
      <LinkList title="Leans on" links={leansOn} />
      <LinkList title="Leaned on by" links={leanedOnBy} />
    </div>
  );
}

function LinkList({ title, links }: { title: string; links: { other: number; relation: Relation }[] }) {
  if (links.length === 0) return null;
  return (
    <div className="mt-3">
      <p className="text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase">{title}</p>
      <ul className="mt-1 space-y-0.5">
        {links.map(({ other, relation }) => (
          <li key={`${other}-${relation}`} className="flex items-baseline justify-between gap-3 text-[12px]">
            <span className="min-w-0 truncate text-slate-300">{(NODES[other] as Node).label}</span>
            <span className="shrink-0 font-mono text-[10px]" style={{ color: RELATION_STYLE[relation].colour }}>
              {relation}
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}
