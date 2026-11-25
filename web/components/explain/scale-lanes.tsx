"use client";

import { Pause, Play } from "lucide-react";
import { Fragment, type ReactNode, useEffect, useId, useMemo, useRef, useState } from "react";

import { DemoButton, DemoFrame, Readout, Segmented } from "@/components/explain/demo";
import { Cmd } from "@/components/instrument";
import { useFigureWidth, useInView, useReducedMotion } from "@/lib/hooks";
import { GRAPH, INK, PROOF, RECALL } from "@/lib/palette";
import { clamp, cn, seeded } from "@/lib/utils";

// ─── Colours ─────────────────────────────────────────────────────────────────
// The chapter's accent marks the thing that owns a store's writes (the write owner,
// the evidence gate, your own node). Writes are working-memory amber and reads recall
// violet, as in the loop diagram; the team mesh is graph mint and the global lane fuchsia.

const ACCENT = "var(--chapter, #fb923c)";
const WRITE = "#fbbf24";
const READ = RECALL;
const MESH = GRAPH;
const GLOBAL_LANE = PROOF;
const SLATE = "#94a3b8";
const DIM = "#64748b";

// ─── The ladder's facts ──────────────────────────────────────────────────────
// Every sentence below was checked against the repository; each names its file.

export type RungId = "one" | "swarm" | "team" | "everywhere";

export interface Rung {
  id: RungId;
  short: string;
  title: string;
  lead: string;
  facts: readonly { text: string; source: string }[];
  commands: readonly string[];
  readouts: readonly { label: string; value: string; note: string; long?: boolean }[];
  /** How many lanes of the precedence stack are in play at this scale. */
  lanes: 1 | 2 | 3;
}

export const RUNGS: readonly Rung[] = [
  {
    id: "one",
    short: "One agent",
    title: "One agent in one workspace",
    lead: "The store lives in the repository's own .ee directory. It holds one database, and everything else in it is derived from that database.",
    facts: [
      {
        text: "`.ee/ee.db` is a FrankenSQLite database and the source of truth. Its search index is stored next to it in `.ee/index/`.",
        source: "src/core/status.rs · src/config/workspace.rs",
      },
      {
        text: "Indexes, embeddings, graph snapshots and caches are derived data. If the index is lost, `ee index rebuild` regenerates it from the database.",
        source: "README.md · Design Philosophy §6",
      },
      {
        text: "Every core workflow runs as a one-shot command and needs no daemon, network or cloud.",
        source: "README.md · Design Philosophy §1, §3",
      },
      {
        text: "Every promotion, consolidation, replacement and tombstone writes an audit entry.",
        source: "README.md · Design Philosophy §9",
      },
    ],
    commands: [
      "ee init --workspace .",
      'ee remember "<lesson>" --workspace . --json',
      'ee pack "<task>" --workspace . --max-tokens 4000 --format markdown',
      "ee index rebuild --workspace .",
    ],
    readouts: [
      { label: "Agents", value: "1", note: "a one-shot CLI" },
      { label: "Writers at once", value: "1", note: "one write lock per store" },
      { label: "Stores", value: "1", note: ".ee/ee.db, in the repository" },
      { label: "Incoming trust class", value: "human_explicit", note: "a direct ee remember · the highest class", long: true },
    ],
    lanes: 1,
  },
  {
    id: "swarm",
    short: "A swarm",
    title: "A swarm on one repository",
    lead: "Many agents share one store. They can all read at once, but writes go one at a time, so a busy swarm sends them through a single write owner.",
    facts: [
      {
        text: "FrankenSQLite is single-process MVCC in WAL mode. Many agents can read at once; each durable write takes the store's one write lock in turn.",
        source: "README.md · Limitations · docs/adr/0077-group-commit-write-intake.md",
      },
      {
        text: "`ee daemon start` hosts one long-lived write owner for the workspace. Writes that reach it together can share a single transaction, up to 64 in a 1 ms window, and each keeps its own audit row.",
        source: "src/daemon/server.rs · src/core/write_owner.rs",
      },
      {
        text: "`ee swarm brief` is the read-only coordination preflight for a checkout shared by several agents; `ee swarm work-packet --claim-gate` must report safeToClaim before an agent takes work.",
        source: "README.md · Swarm brief workflow",
      },
      {
        text: "Every agent reads the swarm's memories. `--memory-scope self` narrows a pack to the current agent's own, named by `EE_AGENT_NAME`.",
        source: "src/core/memory_scope.rs · src/config/env_registry.rs",
      },
    ],
    commands: [
      "ee swarm brief --workspace . --json",
      "ee swarm work-packet --workspace . --claim-gate --candidate <id> --json",
      "ee daemon start --workspace .",
      "ee diag contention --use-daemon --json",
    ],
    readouts: [
      { label: "Agents", value: "many", note: "all reading at once" },
      { label: "Writers at once", value: "1", note: "the write owner, or the lock holder" },
      { label: "Stores", value: "1", note: "shared by the swarm" },
      { label: "Incoming trust class", value: "unchanged", note: "the same rules as for one agent" },
    ],
    lanes: 1,
  },
  {
    id: "team",
    short: "A team",
    title: "A team",
    lead: "Each person keeps their own store. Teammates' memories arrive over the mesh as local rows that name the sender, and they never overwrite your own rows.",
    facts: [
      {
        text: "Built for trusted teams of 2 to 20 people. The team plan sets twenty active members as a hard bound for the v1 protocol.",
        source: "README.md · Team confederation · docs/mesh/team_confederation_plan.md §1.2",
      },
      {
        text: "Members' ee nodes exchange Ed25519-signed events directly over one TCP port on the tailnet. There is no server.",
        source: "docs/adr/0086-team-memory-confederation.md · TC-D1 · docs/team/trusted_vs_contractor.md",
      },
      {
        text: "A memory marked `human_explicit` in a teammate's store arrives here as `peer_human_attested`, one class lower, with teamProvenance naming the member, project and time. The `human_explicit` class is never sent.",
        source: "src/mesh/team.rs · src/models/trust.rs · docs/trust-model.md",
      },
      {
        text: "Metadata is sent first. A memory's body is sent only after its owner confirms a share and the reader's fetch is granted.",
        source: "docs/team/quickstart.md · docs/team/trusted_vs_contractor.md",
      },
    ],
    commands: [
      "ee team create --name <team> --json",
      "ee team invite --json",
      "ee team join --invite <code> --json",
      "ee team steward once --json",
      'ee pack "<task>" --memory-scope team --json',
    ],
    readouts: [
      { label: "Agents", value: "per person", note: "2–20 people, each with their own" },
      { label: "Writers at once", value: "1 per store", note: "teammates never write into yours" },
      { label: "Stores", value: "1 per person", note: "no central store" },
      {
        label: "Incoming trust class",
        value: "peer_human_attested",
        note: "one below human_explicit, which is never sent",
        long: true,
      },
    ],
    lanes: 2,
  },
  {
    id: "everywhere",
    short: "Everywhere",
    title: "Across all your workspaces",
    lead: "One user-global store on your machine supplies your house rules to every repository. Only human_explicit and agent_validated memories can be promoted into it, and a workspace can opt out.",
    facts: [
      {
        text: "The user-global lane is a separate store under your user data root, `~/.local/share/ee/global/`, with the same schema as a workspace store. It stays on this machine and is never sent over the mesh.",
        source: "docs/adr/0083-user-global-memory-store.md §1–2 · docs/trust-model.md",
      },
      {
        text: "`ee memory promote-global` copies a memory into the global store with a link back to its origin. Only `human_explicit` and `agent_validated` memories qualify; any other class gets a typed refusal with exit code 7.",
        source: "src/core/global_promotion.rs · PROMOTABLE_TRUST_CLASSES",
      },
      {
        text: "Global rows compete in every pack and are always labelled. Those that do not overlap a workspace row take at most 15% of the pack's token budget.",
        source: "src/core/global_store.rs · DEFAULT_GLOBAL_FAN_IN_BASIS_POINTS · src/core/context.rs",
      },
      {
        text: "A privileged workspace sets `participate = false` (or `include_global = false`): its packs stop reading the lane, and `ee memory promote-global` refuses.",
        source: "docs/adr/0083-user-global-memory-store.md §5 · src/core/global_promotion.rs",
      },
    ],
    commands: [
      'ee remember --global "<house rule>" --json',
      "ee memory promote-global <id> --dry-run",
      'ee search "<question>" --all-workspaces --json',
    ],
    readouts: [
      { label: "Agents", value: "all of yours", note: "in every workspace that joins" },
      { label: "Writers at once", value: "1 per store", note: "global/ has its own" },
      { label: "Stores", value: "N + 1", note: "your workspaces, plus global/" },
      {
        label: "Incoming trust class",
        value: "human_explicit · agent_validated",
        note: "the only classes promoted",
        long: true,
      },
    ],
    lanes: 3,
  },
];

// ─── The lanes and their precedence ──────────────────────────────────────────

export type Lane = "workspace" | "team" | "global";

/** src/core/global_store.rs, lane_specificity_rank: lower is more specific (ADR 0086 TC-D16). */
export const LANE_RANK: Record<Lane, number> = { workspace: 0, team: 1, global: 2 };

export const LANES: readonly { id: Lane; colour: string; what: string }[] = [
  { id: "workspace", colour: ACCENT, what: "this repository's own .ee store" },
  { id: "team", colour: MESH, what: "teammates' rows, peer_human_attested" },
  { id: "global", colour: GLOBAL_LANE, what: "your house rules, in global/" },
];

export interface Overlap {
  kind: "corroboration" | "contradiction";
  moreSpecific: Lane;
  lessSpecific: Lane;
  bothSurfaced: true;
  moreSpecificOverrides: boolean;
}

/**
 * Two rows from different lanes on the same subject, classified the way
 * surface_precedence_conflicts (src/core/global_store.rs) does: same content is
 * corroboration and the more specific row takes recorded precedence; different
 * content is a contradiction, both stay visible, and neither wins by rank.
 */
export function overlap(a: { lane: Lane; text: string }, b: { lane: Lane; text: string }): Overlap | null {
  if (a.lane === b.lane) return null;
  const [more, less] = LANE_RANK[a.lane] < LANE_RANK[b.lane] ? [a, b] : [b, a];
  const kind = a.text === b.text ? "corroboration" : "contradiction";
  return {
    kind,
    moreSpecific: more.lane,
    lessSpecific: less.lane,
    bothSurfaced: true,
    moreSpecificOverrides: kind === "corroboration",
  };
}

// ─── The swarm's writes, slowed down ─────────────────────────────────────────
// The daemon's write actor (src/daemon/server.rs) coalesces writes that arrive
// within group_commit_max_us = 1,000 µs, up to group_commit_max_rows = 64, into one
// transaction. One-shot CLI writes take the per-file write lock one at a time
// (FileWriteOwnerGuard, ADR 0077). The drawing stretches a millisecond to seconds.

export type WriteMode = "daemon" | "oneshot";

/** Seconds per drawn batch. */
export const CYCLE = 3;
/** The daemon write actor's batch bound (src/daemon/server.rs). */
export const MAX_BATCH = 64;
/** Seconds a write takes to travel from an agent to the ring. */
const TRAVEL = 0.7;
/** When, in each drawn cycle, the daemon closes its batch. */
const COMMIT_AT = 2.05;

export interface SwarmWrite {
  agent: number;
  emit: number;
  arrive: number;
  batch: number;
  commitStart: number;
  commitEnd: number;
}

/** One drawn cycle of the swarm: who writes, when each write arrives, and when it commits. Deterministic. */
export function swarmCycle(cycle: number, agents: number, mode: WriteMode): { writes: SwarmWrite[]; commits: number } {
  const random = seeded(0x5eed + cycle * 7919);
  const drafted: { agent: number; emit: number }[] = [];
  for (let agent = 0; agent < agents; agent++) {
    const roll = random();
    const at = random();
    if (roll < 0.55) drafted.push({ agent, emit: 0.15 + at });
  }
  // At least two writers, so every drawn batch makes the point.
  for (const agent of [cycle % agents, (cycle + Math.ceil(agents / 2)) % agents]) {
    if (!drafted.some((write) => write.agent === agent)) {
      drafted.push({ agent, emit: 0.15 + ((agent * 37 + cycle * 11) % 100) / 100 });
    }
  }
  const arrivals = drafted
    .map((write) => ({ ...write, arrive: write.emit + TRAVEL }))
    .sort((a, b) => a.arrive - b.arrive || a.agent - b.agent);
  const count = arrivals.length;

  if (mode === "daemon") {
    const batches = Math.max(1, Math.ceil(count / MAX_BATCH));
    const span = Math.min(0.3, (CYCLE - 0.08 - COMMIT_AT) / batches);
    const writes = arrivals.map((write, index) => {
      const batch = Math.floor(index / MAX_BATCH);
      const commitStart = COMMIT_AT + batch * span;
      return { ...write, batch, commitStart, commitEnd: commitStart + span };
    });
    return { writes, commits: count === 0 ? 0 : batches };
  }

  // One at a time: each write waits for the lock, then commits alone.
  const last = arrivals[arrivals.length - 1]?.arrive ?? 0;
  const step = Math.min(0.2, (CYCLE - 0.08 - last) / Math.max(1, count));
  let free = 0;
  const writes = arrivals.map((write, index) => {
    const commitStart = Math.max(write.arrive, free);
    free = commitStart + step;
    return { ...write, batch: index, commitStart, commitEnd: free };
  });
  return { writes, commits: count };
}

// ─── The team's events and the global lane's promotions ─────────────────────

type Member = "you" | "hana" | "priya" | "sam";
const MEMBER_NAME: Record<Member, string> = { you: "you", hana: "Hana", priya: "Priya", sam: "Sam" };
const TEAMMATES: readonly Member[] = ["hana", "priya", "sam"];
const KINDS = ["rule", "decision", "failure", "convention"] as const;
const EVENT_EVERY = 1.25;
const EVENT_TRAVEL = 1;

interface TeamEvent {
  n: number;
  from: Member;
  to: Member;
  start: number;
  kind: string;
}

function teamEvent(n: number): TeamEvent {
  const random = seeded(0x7ea + n * 104729);
  const peer = TEAMMATES[Math.floor(random() * TEAMMATES.length)] ?? "hana";
  const kind = KINDS[Math.floor(random() * KINDS.length)] ?? "rule";
  // Every fourth event is yours going out; the rest arrive.
  const outgoing = n % 4 === 3;
  return { n, from: outgoing ? "you" : peer, to: outgoing ? peer : "you", start: n * EVENT_EVERY, kind };
}

const WORKSPACES = [
  { name: "api/", joins: true },
  { name: "web/", joins: true },
  { name: "infra/", joins: true },
  { name: "notes/", joins: true },
  { name: "payroll/", joins: false },
] as const;

/** Promotion candidates in turn. src/core/global_promotion.rs admits only human_explicit and agent_validated. */
const CANDIDATES = [
  { trust: "agent_validated", admitted: true },
  { trust: "agent_assertion", admitted: false },
  { trust: "human_explicit", admitted: true },
  { trust: "cass_evidence", admitted: false },
] as const;
const PROMOTE_EVERY = 0.95;

// ─── Geometry and drawing helpers ────────────────────────────────────────────

type Pt = { x: number; y: number };
type Curve = readonly [Pt, Pt, Pt, Pt];

const easeInOut = (u: number) => (u < 0.5 ? 2 * u * u : 1 - (-2 * u + 2) ** 2 / 2);
const progress = (phase: number, start: number, end: number) => clamp((phase - start) / (end - start), 0, 1);
const pulse = (phase: number, start: number, end: number) =>
  phase <= start || phase >= end ? 0 : Math.sin((Math.PI * (phase - start)) / (end - start));
const mix = (a: Pt, b: Pt, u: number): Pt => ({ x: a.x + (b.x - a.x) * u, y: a.y + (b.y - a.y) * u });
const polar = (centre: Pt, r: number, angle: number): Pt => ({
  x: centre.x + r * Math.cos(angle),
  y: centre.y + r * Math.sin(angle),
});

function bezier([p0, p1, p2, p3]: Curve, u: number): Pt {
  const v = 1 - u;
  const a = v * v * v;
  const b = 3 * v * v * u;
  const c = 3 * v * u * u;
  const d = u * u * u;
  return { x: a * p0.x + b * p1.x + c * p2.x + d * p3.x, y: a * p0.y + b * p1.y + c * p2.y + d * p3.y };
}

const curvePath = ([p0, p1, p2, p3]: Curve) => `M ${p0.x} ${p0.y} C ${p1.x} ${p1.y}, ${p2.x} ${p2.y}, ${p3.x} ${p3.y}`;

/** A database: the shape every store in the figure wears. `colour` may be a CSS variable. */
function Store({
  x,
  y,
  w,
  h,
  colour,
  glow = 0,
  dashed = false,
}: {
  x: number;
  y: number;
  w: number;
  h: number;
  colour: string;
  glow?: number;
  dashed?: boolean;
}) {
  const rx = w / 2;
  const ry = Math.max(3.5, w * 0.16);
  const top = y - h / 2 + ry;
  const bottom = y + h / 2 - ry;
  const body = `M ${x - rx} ${top} V ${bottom} A ${rx} ${ry} 0 0 0 ${x + rx} ${bottom} V ${top}`;
  const dash = dashed ? "4 3" : undefined;
  return (
    <g style={{ color: colour }}>
      {glow > 0.01 ? <ellipse cx={x} cy={y} rx={w * 0.95} ry={h * 0.88} fill="currentColor" opacity={0.16 * glow} /> : null}
      <path d={body} fill={INK} />
      <path d={body} fill="currentColor" fillOpacity={0.1 + 0.14 * glow} stroke="currentColor" strokeWidth={1.3} strokeDasharray={dash} />
      <path d={`M ${x - rx} ${y + ry * 0.2} A ${rx} ${ry} 0 0 0 ${x + rx} ${y + ry * 0.2}`} fill="none" stroke="currentColor" strokeOpacity={0.4} />
      <ellipse cx={x} cy={top} rx={rx} ry={ry} fill={INK} />
      <ellipse
        cx={x}
        cy={top}
        rx={rx}
        ry={ry}
        fill="currentColor"
        fillOpacity={0.22 + 0.3 * glow}
        stroke="currentColor"
        strokeWidth={1.3}
        strokeDasharray={dash}
      />
    </g>
  );
}

/** An agent: a dot. */
function AgentDot({ x, y, r = 6, lit = 0 }: { x: number; y: number; r?: number; lit?: number }) {
  return (
    <g>
      {lit > 0.01 ? <circle cx={x} cy={y} r={r + 6} fill={WRITE} opacity={0.3 * lit} /> : null}
      <circle cx={x} cy={y} r={r} fill={INK} stroke="#e2e8f0" strokeWidth={1.4} />
      <circle cx={x} cy={y} r={r * 0.45} fill={lit > 0.4 ? WRITE : "#e2e8f0"} />
    </g>
  );
}

/** Something in flight: a memory, a write, an event. */
function Spark({ at, colour, r = 3.6, opacity = 1 }: { at: Pt; colour: string; r?: number; opacity?: number }) {
  return (
    <g style={{ color: colour }} opacity={opacity}>
      <circle cx={at.x} cy={at.y} r={r * 2.5} fill="currentColor" opacity={0.18} />
      <circle cx={at.x} cy={at.y} r={r} fill="currentColor" />
    </g>
  );
}

/** A pill of monospace text; its width is estimated from the character count. */
function Tag({
  x,
  y,
  text,
  colour,
  anchor = "middle",
  size = 9.5,
  opacity = 1,
}: {
  x: number;
  y: number;
  text: string;
  colour: string;
  anchor?: "start" | "middle" | "end";
  size?: number;
  opacity?: number;
}) {
  const width = text.length * size * 0.62 + 12;
  const height = size + 8;
  const left = anchor === "middle" ? x - width / 2 : anchor === "start" ? x : x - width;
  return (
    <g style={{ color: colour }} opacity={opacity}>
      <rect x={left} y={y - height / 2} width={width} height={height} rx={height / 2} fill={INK} stroke="currentColor" strokeOpacity={0.55} />
      <text x={left + width / 2} y={y + size * 0.36} textAnchor="middle" className="mono" fontSize={size} fill="currentColor">
        {text}
      </text>
    </g>
  );
}

function Lock({ x, y }: { x: number; y: number }) {
  return (
    <g fill="none" stroke={SLATE} strokeWidth={1.3}>
      <path d={`M ${x - 3.2} ${y - 1} V ${y - 4} A 3.2 3.2 0 0 1 ${x + 3.2} ${y - 4} V ${y - 1}`} />
      <rect x={x - 5.5} y={y - 1} width={11} height={8.5} rx={1.6} fill={INK} />
    </g>
  );
}

function Person({ x, y, colour }: { x: number; y: number; colour: string }) {
  return (
    <g fill="none" style={{ color: colour }} stroke="currentColor" strokeWidth={1.3}>
      <circle cx={x} cy={y - 4} r={3.3} />
      <path d={`M ${x - 6} ${y + 6} Q ${x - 6} ${y} ${x} ${y} Q ${x + 6} ${y} ${x + 6} ${y + 6}`} />
    </g>
  );
}

interface SceneProps {
  W: number;
  H: number;
  wide: boolean;
  time: number;
  animate: boolean;
  id: string;
}

// ─── Rung 1: one agent, one workspace ────────────────────────────────────────

function OneAgent({ W, H, wide, time, id }: SceneProps) {
  const phase = time % 4.2;
  let agent: Pt;
  let folder: { x: number; y: number; w: number; h: number };
  let ee: { x: number; y: number; w: number; h: number };
  let db: { x: number; y: number; w: number; h: number };
  let index: { x: number; y: number; w: number; h: number };
  let write: Curve;
  let read: Curve;

  if (wide) {
    agent = { x: Math.max(64, W * 0.11), y: H / 2 };
    const fx = Math.round(W * 0.34);
    folder = { x: fx, y: 34, w: W - fx - 1, h: H - 34 - 16 };
    const ex = folder.x + Math.max(128, folder.w * 0.3);
    ee = { x: ex, y: folder.y + 24, w: folder.x + folder.w - 16 - ex, h: folder.h - 40 };
    db = { x: ee.x + ee.w * 0.3, y: ee.y + ee.h / 2 - 8, w: 74, h: 78 };
    const iw = Math.min(150, ee.w * 0.36);
    index = { x: ee.x + ee.w * 0.74 - iw / 2, y: db.y - 37, w: iw, h: 74 };
    const dbLeft = db.x - db.w / 2 - 7;
    write = [
      { x: agent.x + 14, y: agent.y - 8 },
      { x: agent.x + 90, y: agent.y - 46 },
      { x: dbLeft - 104, y: db.y - 46 },
      { x: dbLeft, y: db.y - 12 },
    ];
    read = [
      { x: dbLeft, y: db.y + 16 },
      { x: dbLeft - 104, y: db.y + 50 },
      { x: agent.x + 90, y: agent.y + 50 },
      { x: agent.x + 14, y: agent.y + 8 },
    ];
  } else {
    agent = { x: W / 2, y: 32 };
    folder = { x: 1, y: 96, w: W - 2, h: H - 100 };
    ee = { x: 12, y: folder.y + 60, w: W - 24, h: folder.y + folder.h - 12 - (folder.y + 60) };
    db = { x: ee.x + ee.w * 0.24, y: ee.y + ee.h * 0.4, w: 60, h: 62 };
    const iw = Math.min(104, ee.w * 0.36);
    index = { x: ee.x + ee.w * 0.76 - iw / 2, y: db.y - 31, w: iw, h: 62 };
    write = [
      { x: W / 2 - 8, y: 44 },
      { x: W / 2 - 8, y: 84 },
      { x: db.x, y: 104 },
      { x: db.x, y: db.y - db.h / 2 - 8 },
    ];
    read = [
      { x: index.x + index.w / 2, y: index.y - 6 },
      { x: index.x + index.w / 2, y: 104 },
      { x: W / 2 + 8, y: 84 },
      { x: W / 2 + 8, y: 44 },
    ];
  }

  const derive: [Pt, Pt] = [
    { x: db.x + db.w / 2 + 6, y: db.y },
    { x: index.x - 7, y: db.y },
  ];
  const ix = index.x + index.w / 2;
  const writing = phase < 1.1;
  const reading = phase >= 2.9 && phase < 4;
  const deriving = phase >= 1.7 && phase < 2.4;
  const dbGlow = pulse(phase, 1.0, 1.9);
  const indexGlow = pulse(phase, 2.3, 3.1);
  const labelY = db.y + db.h / 2 + 18;
  const files = wide ? ["src/", "tests/", "docs/", "README.md"] : [];

  return (
    <g>
      {/* The repository */}
      {wide ? (
        <g>
          <rect x={folder.x} y={folder.y - 15} width={104} height={20} rx={6} fill="rgba(255,255,255,0.03)" stroke="rgba(221,214,254,0.16)" />
          <text x={folder.x + 12} y={folder.y - 1} className="mono" fontSize={11} fill="#cbd5e1">
            your-repo/
          </text>
        </g>
      ) : null}
      <rect x={folder.x} y={folder.y} width={folder.w} height={folder.h} rx={16} fill="rgba(255,255,255,0.02)" stroke="rgba(221,214,254,0.16)" />
      {wide ? null : (
        <text x={folder.x + 12} y={folder.y + 20} className="mono" fontSize={11} fill="#cbd5e1">
          your-repo/
        </text>
      )}
      {wide ? (
        files.map((file, row) => (
          <text key={file} x={folder.x + 18} y={folder.y + 34 + row * 18} className="mono" fontSize={11} fill={DIM}>
            {file}
          </text>
        ))
      ) : (
        <text x={W / 2} y={folder.y + 40} textAnchor="middle" className="mono" fontSize={10.5} fill={DIM}>
          src/ · tests/
        </text>
      )}

      {/* The store: .ee/ */}
      <g style={{ color: ACCENT }}>
        <rect x={ee.x} y={ee.y} width={ee.w} height={ee.h} rx={14} fill="currentColor" fillOpacity={0.04} stroke="currentColor" strokeOpacity={0.5} />
        <text x={ee.x + 14} y={ee.y + 21} className="mono" fontSize={12} fontWeight={700} fill="currentColor">
          .ee/
        </text>
      </g>
      <text x={ee.x + 14} y={ee.y + ee.h - 14} fontSize={10.5} fill={SLATE}>
        the store, in the repo
      </text>

      <Store x={db.x} y={db.y} w={db.w} h={db.h} colour={READ} glow={dbGlow} />
      <text x={db.x} y={labelY} textAnchor="middle" className="mono" fontSize={12} fontWeight={700} fill="#fff">
        ee.db
      </text>
      <text x={db.x} y={labelY + 15} textAnchor="middle" fontSize={10.5} fill={SLATE}>
        FrankenSQLite
      </text>
      <text x={db.x} y={labelY + 30} textAnchor="middle" className="mono" fontSize={9} letterSpacing={1.5} fill="#c4b5fd">
        SOURCE OF TRUTH
      </text>

      <rect
        x={index.x}
        y={index.y}
        width={index.w}
        height={index.h}
        rx={10}
        fill={indexGlow > 0 ? `rgba(167,139,250,${0.04 + 0.12 * indexGlow})` : "rgba(255,255,255,0.015)"}
        stroke="rgba(196,181,253,0.55)"
        strokeDasharray="4 3"
      />
      <text x={ix} y={db.y - 4} textAnchor="middle" className="mono" fontSize={12} fontWeight={700} fill="#fff">
        index/
      </text>
      <text x={ix} y={db.y + 12} textAnchor="middle" fontSize={10} fill={SLATE}>
        BM25 + vectors
      </text>
      <text x={ix} y={labelY} textAnchor="middle" className="mono" fontSize={9} letterSpacing={1.5} fill="#77839c">
        DERIVED
      </text>
      <text x={ix} y={labelY + 13} textAnchor="middle" className="mono" fontSize={9} letterSpacing={1.5} fill="#77839c">
        REBUILDABLE
      </text>

      {/* Derive: the index is rebuilt from the database */}
      <path
        d={`M ${derive[0].x} ${derive[0].y} H ${derive[1].x}`}
        stroke="rgba(196,181,253,0.6)"
        strokeDasharray="3 4"
        markerEnd={`url(#${id}-read)`}
      />
      <text x={(derive[0].x + derive[1].x) / 2} y={wide ? db.y - 52 : db.y - 9} textAnchor="middle" className="mono" fontSize={9.5} fill={SLATE}>
        {wide ? "ee index rebuild" : "derive"}
      </text>
      {deriving ? <Spark at={mix(derive[0], derive[1], easeInOut(progress(phase, 1.7, 2.4)))} colour={READ} r={3} /> : null}

      {/* Write in, read out */}
      <path d={curvePath(write)} fill="none" stroke={WRITE} strokeWidth={writing ? 2.2 : 1.4} strokeOpacity={writing ? 1 : 0.45} markerEnd={`url(#${id}-write)`} />
      <path d={curvePath(read)} fill="none" stroke={READ} strokeWidth={reading ? 2.2 : 1.4} strokeOpacity={reading ? 1 : 0.45} markerEnd={`url(#${id}-read)`} />
      {writing ? <Spark at={bezier(write, easeInOut(progress(phase, 0, 1.1)))} colour={WRITE} /> : null}
      {reading ? <Spark at={bezier(read, easeInOut(progress(phase, 2.9, 4)))} colour={READ} /> : null}

      {wide ? (
        <>
          <text x={(agent.x + 14 + folder.x) / 2} y={agent.y - 58} textAnchor="middle" className="mono" fontSize={10} fill={WRITE}>
            remember · outcome
          </text>
          <text x={(agent.x + 14 + folder.x) / 2} y={agent.y + 68} textAnchor="middle" className="mono" fontSize={10} fill={READ}>
            pack · search · recall
          </text>
        </>
      ) : (
        <>
          <text x={W / 2 - 40} y={74} textAnchor="end" className="mono" fontSize={10} fill={WRITE}>
            remember
          </text>
          <text x={W / 2 + 40} y={74} className="mono" fontSize={10} fill={READ}>
            pack · search
          </text>
        </>
      )}

      {/* The agent */}
      <AgentDot x={agent.x} y={agent.y} r={8} lit={writing ? 1 - progress(phase, 0, 0.5) : 0} />
      {wide ? (
        <>
          <text x={agent.x} y={agent.y + 34} textAnchor="middle" fontSize={12.5} fontWeight={800} fill="#fff">
            1 agent
          </text>
          <text x={agent.x} y={agent.y + 49} textAnchor="middle" fontSize={10.5} fill={SLATE}>
            one-shot CLI
          </text>
        </>
      ) : (
        <>
          <text x={W / 2 - 16} y={36} textAnchor="end" fontSize={12.5} fontWeight={800} fill="#fff">
            1 agent
          </text>
          <text x={W / 2 + 16} y={36} fontSize={10.5} fill={SLATE}>
            one-shot CLI
          </text>
        </>
      )}
    </g>
  );
}

// ─── Rung 2: a swarm on one repository ───────────────────────────────────────

function Swarm({ W, H, wide, time, animate, mode }: SceneProps & { mode: WriteMode }) {
  const agents = wide ? 10 : 8;
  const R = wide ? Math.min(H / 2 - 42, 138) : Math.min(W / 2 - 24, 130);
  // Wide: the ring and its three callouts (about 230 px) centred together.
  const centre: Pt = wide ? { x: Math.round(Math.max(R + 24, (W - (2 * R + 46 + 230)) / 2 + R)), y: H / 2 + 4 } : { x: W / 2, y: 186 };
  const ring = wide ? 58 : 50;
  const cycle = Math.floor(time / CYCLE);
  const phase = time - cycle * CYCLE;
  const { writes, commits } = useMemo(() => swarmCycle(cycle, agents, mode), [cycle, agents, mode]);
  const angle = (agent: number) => -Math.PI / 2 + (agent * Math.PI * 2) / agents;

  const total = writes.length;
  const arrived = writes.filter((write) => phase >= write.arrive).length;
  const queued = writes.filter((write) => phase >= write.arrive && phase < write.commitStart).length;
  const done = writes.filter((write) => phase >= write.commitEnd).length;
  const plural = (count: number, word: string) => `${count} ${word}${count === 1 ? "" : "s"}`;
  const batchLine =
    mode === "daemon"
      ? phase < COMMIT_AT
        ? `${plural(arrived, "write")} waiting`
        : `${plural(total, "write")} → ${plural(commits, "commit")}`
      : done < total
        ? `${done} of ${plural(total, "write")} in`
        : `${plural(total, "write")} → ${plural(total, "commit")}`;

  // The ring brightens as writes queue on it; in daemon mode the whole batch then commits at once.
  const ringGlow = clamp(queued / 6, 0, 1);
  const commitWave = mode === "daemon" ? progress(phase, COMMIT_AT, COMMIT_AT + 0.3) : 0;
  const committing = mode === "daemon" && phase >= COMMIT_AT && phase < COMMIT_AT + 0.3;
  const storeGlow =
    mode === "daemon"
      ? pulse(phase, COMMIT_AT + 0.2, COMMIT_AT + 0.85)
      : Math.max(0, ...writes.map((write) => 0.7 * pulse(phase, write.commitEnd - 0.05, write.commitEnd + 0.3)));

  const owner = mode === "daemon";
  const ownerTitle = owner ? "WRITE OWNER" : "WRITE LOCK";
  const ownerName = owner ? "ee daemon" : "one-shot CLI";
  const ownerNote = owner ? "one owner, writes coalesced" : "each write waits its turn";

  return (
    <g>
      {/* Reads: every agent at once, outward from the store */}
      {Array.from({ length: agents }, (_, agent) => {
        const from = polar(centre, 30, angle(agent));
        const to = polar(centre, R - 11, angle(agent));
        return (
          <path
            key={`spoke-${agent}`}
            d={`M ${from.x} ${from.y} L ${to.x} ${to.y}`}
            stroke={READ}
            strokeOpacity={0.4}
            strokeWidth={1.3}
            strokeDasharray="3 9"
            className={cn(animate && "flow")}
          />
        );
      })}

      {/* The ring: the one writer */}
      <g style={{ color: ACCENT }}>
        <circle cx={centre.x} cy={centre.y} r={ring + 10} fill="currentColor" opacity={0.05 + 0.08 * ringGlow} />
        <circle
          cx={centre.x}
          cy={centre.y}
          r={ring}
          fill="none"
          stroke="currentColor"
          strokeWidth={1.6 + 1.4 * ringGlow}
          strokeOpacity={0.6 + 0.4 * ringGlow}
          strokeDasharray={owner ? undefined : "6 4"}
        />
        {committing ? (
          <circle cx={centre.x} cy={centre.y} r={ring - (ring - 26) * commitWave} fill="none" stroke="currentColor" strokeWidth={3} opacity={1 - commitWave} />
        ) : null}
      </g>
      <Store x={centre.x} y={centre.y} w={wide ? 46 : 40} h={wide ? 48 : 42} colour={READ} glow={storeGlow} />

      {/* Agents */}
      {Array.from({ length: agents }, (_, agent) => {
        const at = polar(centre, R, angle(agent));
        const emitted = writes.find((write) => write.agent === agent);
        const lit = emitted ? 1 - progress(phase, emitted.emit, emitted.emit + 0.35) : 0;
        return <AgentDot key={`agent-${agent}`} x={at.x} y={at.y} lit={emitted && phase >= emitted.emit ? lit : 0} />;
      })}

      {/* Writes: inward to the ring, then into the store */}
      {writes.map((write) => {
        if (phase < write.emit || phase >= write.commitEnd) return null;
        const theta = angle(write.agent);
        let r: number;
        if (phase < write.arrive) r = R - 11 + (ring + 1 - (R - 11)) * easeInOut(progress(phase, write.emit, write.arrive));
        else if (phase < write.commitStart) r = ring + 1;
        else r = ring + 1 + (16 - (ring + 1)) * easeInOut(progress(phase, write.commitStart, write.commitEnd));
        const waiting = phase >= write.arrive && phase < write.commitStart;
        return <Spark key={`write-${write.agent}`} at={polar(centre, r, theta)} colour={WRITE} r={waiting ? 4 : 3.4} />;
      })}

      {/* What is happening, in words */}
      {wide ? (
        <g>
          <path d={`M ${centre.x + ring + 8} ${centre.y} H ${centre.x + R + 34}`} style={{ stroke: ACCENT }} strokeOpacity={0.45} strokeDasharray="2 4" />
          <g transform={`translate(${centre.x + R + 46} 0)`}>
            <text y={centre.y - 96} className="mono" fontSize={9.5} letterSpacing={1.5} style={{ fill: ACCENT }}>
              {ownerTitle}
            </text>
            <text y={centre.y - 77} fontSize={13.5} fontWeight={800} fill="#fff">
              {ownerName}
            </text>
            <text y={centre.y - 60} fontSize={11} fill={SLATE}>
              {ownerNote}
            </text>
            <text y={centre.y - 12} className="mono" fontSize={9.5} letterSpacing={1.5} fill="#77839c">
              {owner ? "THIS BATCH" : "THIS ROUND"}
            </text>
            <text y={centre.y + 12} className="mono" fontSize={15} fill="#fff">
              {batchLine}
            </text>
            <text y={centre.y + 62} className="mono" fontSize={9.5} letterSpacing={1.5} fill={READ}>
              READS
            </text>
            <text y={centre.y + 81} fontSize={13.5} fontWeight={800} fill="#fff">
              every agent, at once
            </text>
            <text y={centre.y + 98} fontSize={11} fill={SLATE}>
              each reads a snapshot
            </text>
          </g>
        </g>
      ) : (
        <g>
          <text x={W / 2} y={centre.y + R + 32} textAnchor="middle" className="mono" fontSize={9.5} letterSpacing={1.2} style={{ fill: ACCENT }}>
            {ownerTitle} · {ownerName}
          </text>
          <text x={W / 2} y={centre.y + R + 54} textAnchor="middle" className="mono" fontSize={14} fill="#fff">
            {batchLine}
          </text>
          <text x={W / 2} y={centre.y + R + 76} textAnchor="middle" fontSize={11} fill={READ}>
            reads: every agent at once
          </text>
          <text x={W / 2} y={centre.y + R + 93} textAnchor="middle" fontSize={10.5} fill={SLATE}>
            {ownerNote}
          </text>
        </g>
      )}
    </g>
  );
}

// ─── Rung 3: a team ──────────────────────────────────────────────────────────

function Team({ W, H, wide, time }: SceneProps) {
  const at: Record<Member, Pt> = wide
    ? {
        sam: { x: W * 0.11, y: H * 0.5 },
        hana: { x: W * 0.32, y: H * 0.19 },
        priya: { x: W * 0.32, y: H * 0.83 },
        you: { x: W * 0.53, y: H * 0.5 },
      }
    : {
        hana: { x: W / 2, y: 40 },
        sam: { x: W * 0.16, y: 128 },
        priya: { x: W * 0.84, y: 128 },
        you: { x: W / 2, y: 216 },
      };
  const members: readonly Member[] = ["you", "hana", "priya", "sam"];
  const links: [Member, Member][] = [];
  members.forEach((a, i) => members.slice(i + 1).forEach((b) => links.push([a, b])));

  const n = Math.floor(time / EVENT_EVERY);
  const inFlight = [n - 1, n]
    .filter((m) => m >= 0)
    .map(teamEvent)
    .map((event) => ({ event, u: (time - event.start) / EVENT_TRAVEL }))
    .filter(({ u }) => u >= 0 && u < 1);
  const busy = new Set(inFlight.map(({ event }) => [event.from, event.to].sort().join("-")));

  // Arrivals in your store, newest first.
  const maxChips = wide ? 3 : 2;
  const chips: TeamEvent[] = [];
  for (let m = n; m >= 0 && chips.length < maxChips; m--) {
    const event = teamEvent(m);
    if (event.to === "you" && event.start + EVENT_TRAVEL <= time) chips.push(event);
  }
  const newest = chips[0];
  const sinceArrival = newest ? time - (newest.start + EVENT_TRAVEL) : 99;
  const youGlow = pulse(sinceArrival, 0, 0.6);

  const centroid: Pt = wide ? { x: W * 0.32, y: H * 0.5 } : { x: W / 2, y: 128 };
  const panel = wide ? { x: Math.round(W * 0.64), y: 40 } : { x: 1, y: 294 };
  const chipW = wide ? clamp(W - panel.x - 2, 200, 300) : W - 2;
  const chipH = 44;

  return (
    <g>
      {/* The mesh: every member linked to every other, nothing in the middle */}
      {links.map(([a, b]) => {
        const key = [a, b].sort().join("-");
        const on = busy.has(key);
        return (
          <line
            key={key}
            x1={at[a].x}
            y1={at[a].y}
            x2={at[b].x}
            y2={at[b].y}
            stroke={MESH}
            strokeOpacity={on ? 0.75 : 0.32}
            strokeWidth={on ? 1.6 : 1.2}
            strokeDasharray="2 5"
          />
        );
      })}
      <Tag x={centroid.x} y={centroid.y} text="no server" colour={SLATE} size={9.5} />

      {inFlight.map(({ event, u }) => (
        <Spark key={`event-${event.n}`} at={mix(at[event.from], at[event.to], easeInOut(u))} colour={event.from === "you" ? ACCENT : MESH} />
      ))}

      {/* The members: a store each, with its own agents in orbit */}
      {members.map((member) => {
        const p = at[member];
        const you = member === "you";
        return (
          <g key={member}>
            {you ? (
              <g style={{ color: ACCENT }}>
                <circle cx={p.x} cy={p.y} r={36} fill="currentColor" opacity={0.06 + 0.12 * youGlow} />
                <circle cx={p.x} cy={p.y} r={34} fill="none" stroke="currentColor" strokeWidth={1.5} />
              </g>
            ) : null}
            <Store x={p.x} y={p.y} w={28} h={28} colour={you ? ACCENT : MESH} glow={you ? youGlow : 0} />
            {[0, 1, 2].map((k) => {
              const orbit = polar(p, 23, time * 0.7 + (k * Math.PI * 2) / 3 + members.indexOf(member));
              return <circle key={k} cx={orbit.x} cy={orbit.y} r={2.6} fill="#e2e8f0" />;
            })}
          </g>
        );
      })}

      {/* Names */}
      {members.map((member) => {
        const p = at[member];
        const you = member === "you";
        const side = wide ? member === "hana" || member === "priya" : member === "hana";
        const x = side ? p.x + 34 : p.x;
        const y = side ? p.y + 4 : p.y + (you ? 50 : 42);
        return (
          <g key={`name-${member}`}>
            {side ? <Person x={x + 6} y={y - 4} colour={you ? ACCENT : MESH} /> : null}
            <text
              x={side ? x + 17 : x}
              y={y}
              textAnchor={side ? "start" : "middle"}
              fontSize={12.5}
              fontWeight={800}
              style={{ fill: you ? ACCENT : "#fff" }}
            >
              {MEMBER_NAME[member]}
            </text>
            {wide ? (
              <text x={side ? x + 17 : x} y={y + 15} textAnchor={side ? "start" : "middle"} fontSize={10.5} fill={SLATE}>
                {you ? "your own store" : "own .ee store"}
              </text>
            ) : null}
          </g>
        );
      })}

      {/* What arrives in your store */}
      <text x={panel.x} y={panel.y} className="mono" fontSize={9.5} letterSpacing={1.5} style={{ fill: ACCENT }}>
        ARRIVING IN YOUR STORE
      </text>
      {chips.map((event, index) => {
        const age = index === 0 ? progress(sinceArrival, 0, 0.35) : 1;
        const top = panel.y + 12 + index * (chipH + 8);
        return (
          <g
            key={`chip-${event.n}`}
            opacity={(index === 0 ? age : 1) * (1 - index * 0.22)}
            style={{ transform: `translateY(${top - 8 * (1 - age)}px)`, transition: "transform 300ms ease" }}
          >
            <rect x={panel.x} y={0} width={chipW} height={chipH} rx={10} fill={INK} stroke={MESH} strokeOpacity={index === 0 ? 0.7 : 0.3} />
            <text x={panel.x + 12} y={17} fontSize={12} fontWeight={800} fill="#fff">
              {MEMBER_NAME[event.from]}
              <tspan fontWeight={400} fill={SLATE}>
                {" "}
                · {event.kind}
              </tspan>
            </text>
            <Tag x={panel.x + 12} y={31} text="peer_human_attested" colour={MESH} anchor="start" size={9} />
          </g>
        );
      })}
      {wide ? (
        <g>
          <text x={panel.x} y={panel.y + 12 + 3 * (chipH + 8) + 14} fontSize={11} fill={SLATE}>
            a local row, with teamProvenance
          </text>
          <text x={panel.x} y={panel.y + 12 + 3 * (chipH + 8) + 30} fontSize={11} fill={SLATE}>
            never overwrites yours
          </text>
          <text x={panel.x} y={panel.y + 12 + 3 * (chipH + 8) + 46} fontSize={11} fill={SLATE}>
            human_explicit is never sent
          </text>
        </g>
      ) : (
        <text x={W / 2} y={H - 10} textAnchor="middle" fontSize={10.5} fill={SLATE}>
          a local row · human_explicit is never sent
        </text>
      )}
    </g>
  );
}

// ─── Rung 4: everywhere you work ─────────────────────────────────────────────

function Everywhere({ W, H, wide, time, animate }: SceneProps) {
  const global: Pt = wide ? { x: W / 2, y: 80 } : { x: W / 2, y: 74 };
  const ring = wide ? 48 : 40;
  // The label columns either side of the gate. Wide, they stand off far enough that a refused candidate's pill,
  // waiting at the gate below and to the left, clears "others are refused" even at the narrowest wide layout.
  const leftX = global.x - ring - (wide ? 20 : 14);
  const rightX = global.x + ring + (wide ? 20 : 14);
  const wsY = wide ? H - 78 : 296;
  const spots = WORKSPACES.map((workspace, i) => ({
    ...workspace,
    at: { x: W * ((wide ? 0.12 : 0.1) + (wide ? 0.19 : 0.2) * i), y: wsY },
  }));
  const joiners = spots.filter((spot) => spot.joins);
  const top = (p: Pt): Pt => ({ x: p.x, y: p.y - 20 });
  const edge = (p: Pt, r: number): Pt => {
    const dx = p.x - global.x;
    const dy = p.y - global.y;
    const length = Math.hypot(dx, dy) || 1;
    return { x: global.x + (dx / length) * r, y: global.y + (dy / length) * r };
  };

  // A candidate leaves a joining workspace every PROMOTE_EVERY seconds and takes up to 1.55 s,
  // so two are in flight at once: one climbing, one at the gate.
  const n = Math.floor(time / PROMOTE_EVERY);
  const promotions = [n - 1, n]
    .filter((m) => m >= 0)
    .map((m) => {
      const local = time - m * PROMOTE_EVERY;
      const source = joiners[(m * 3 + 1) % joiners.length] ?? joiners[0]!;
      const candidate = CANDIDATES[m % CANDIDATES.length] ?? CANDIDATES[0];
      const start = top(source.at);
      const gate = edge(start, ring + 5);
      let spark: Pt | null = null;
      let opacity = 1;
      if (local < 0.95) spark = mix(start, gate, easeInOut(progress(local, 0, 0.95)));
      else if (candidate.admitted && local < 1.35) spark = mix(gate, global, easeInOut(progress(local, 0.95, 1.35)) * 0.85);
      else if (!candidate.admitted && local < 1.55) {
        spark = gate;
        opacity = 1 - progress(local, 1.15, 1.55);
      }
      return {
        m,
        source,
        candidate,
        gate,
        spark,
        opacity,
        refused: !candidate.admitted && local >= 0.95 && local < 1.55,
        // An admitted candidate drops its pill as the gate lights: carried into the store, the pill would lie over
        // the gate's own labels on either side.
        labelled: local < 0.9 || !candidate.admitted,
        right: (spark ?? gate).x <= W / 2 + 4,
        gateFlash: candidate.admitted ? pulse(local, 0.85, 1.25) : 0,
        globalGlow: candidate.admitted ? pulse(local, 1.25, 1.6) : 0,
      };
    });
  const climbing = new Set(promotions.filter((promotion) => promotion.spark).map((promotion) => promotion.source.name));
  const gateFlash = Math.max(0, ...promotions.map((promotion) => promotion.gateFlash));
  const globalGlow = Math.max(0, ...promotions.map((promotion) => promotion.globalGlow));

  return (
    <g>
      {/* Reads flow down every joining workspace's link; promotions travel up it */}
      {joiners.map((spot) => {
        const from = edge(top(spot.at), ring + 2);
        const to = top(spot.at);
        return (
          <path
            key={`link-${spot.name}`}
            d={`M ${from.x} ${from.y} L ${to.x} ${to.y}`}
            stroke={GLOBAL_LANE}
            strokeOpacity={climbing.has(spot.name) ? 0.7 : 0.35}
            strokeWidth={1.3}
            strokeDasharray="3 9"
            className={cn(animate && "flow")}
          />
        );
      })}

      {/* The global store and its evidence gate */}
      <g style={{ color: ACCENT }}>
        <circle cx={global.x} cy={global.y} r={ring} fill="currentColor" fillOpacity={0.04 + 0.1 * gateFlash} stroke="currentColor" strokeWidth={1.6 + 1.6 * gateFlash} strokeDasharray="7 4" />
      </g>
      <Store x={global.x} y={global.y} w={wide ? 50 : 42} h={wide ? 48 : 42} colour={GLOBAL_LANE} glow={globalGlow} />

      <g>
        <text x={leftX} y={global.y - 12} textAnchor="end" className="mono" fontSize={wide ? 12 : 11} fontWeight={700} fill="#fff">
          global/
        </text>
        <text x={leftX} y={global.y + 3} textAnchor="end" className={wide ? "mono" : undefined} fontSize={wide ? 10 : 10.5} fill={SLATE}>
          {wide ? "~/.local/share/ee/global" : "house rules"}
        </text>
        <text x={leftX} y={global.y + 18} textAnchor="end" fontSize={10.5} fill={SLATE}>
          {wide ? "this machine only · off the mesh" : "this machine"}
        </text>
      </g>
      <g>
        <text x={rightX} y={global.y - 14} className="mono" fontSize={wide ? 9.5 : 9} letterSpacing={1.4} style={{ fill: ACCENT }}>
          EVIDENCE GATE
        </text>
        <text x={rightX} y={global.y + 2} className="mono" fontSize={wide ? 10.5 : 9.5} fill="#fff">
          human_explicit
        </text>
        <text x={rightX} y={global.y + 17} className="mono" fontSize={wide ? 10.5 : 9.5} fill="#fff">
          agent_validated
        </text>
        {wide ? (
          <text x={rightX} y={global.y + 33} fontSize={10.5} fill={SLATE}>
            others are refused
          </text>
        ) : null}
      </g>

      {/* Workspaces */}
      {spots.map((spot) => (
        <g key={spot.name}>
          {spot.joins ? null : (
            <>
              <circle cx={spot.at.x} cy={spot.at.y} r={27} fill="none" stroke={SLATE} strokeOpacity={0.4} strokeDasharray="2 4" />
              <Lock x={spot.at.x} y={spot.at.y - 38} />
            </>
          )}
          <Store x={spot.at.x} y={spot.at.y} w={30} h={30} colour={spot.joins ? ACCENT : SLATE} dashed={!spot.joins} />
          <text x={spot.at.x} y={spot.at.y + 36} textAnchor="middle" className="mono" fontSize={wide ? 11 : 10} fill={spot.joins ? "#fff" : SLATE}>
            {spot.name}
          </text>
        </g>
      ))}
      {(() => {
        const locked = spots.find((spot) => !spot.joins);
        if (!locked) return null;
        return (
          <text
            x={wide ? locked.at.x : W - 2}
            y={locked.at.y + 52}
            textAnchor={wide ? "middle" : "end"}
            className="mono"
            fontSize={9.5}
            fill={SLATE}
          >
            participate = false
          </text>
        );
      })()}

      {/* The promotions in flight */}
      {promotions.map((promotion) =>
        promotion.spark ? (
          <g key={`promotion-${promotion.m}`}>
            <Spark at={promotion.spark} colour={promotion.candidate.admitted ? GLOBAL_LANE : SLATE} opacity={promotion.opacity} />
            {promotion.labelled ? (
              <Tag
                x={promotion.spark.x + (promotion.right ? 10 : -10)}
                y={promotion.spark.y}
                text={promotion.candidate.trust}
                colour={promotion.candidate.admitted ? GLOBAL_LANE : SLATE}
                anchor={promotion.right ? "start" : "end"}
                size={9}
                opacity={promotion.opacity}
              />
            ) : null}
            {promotion.refused ? (
              <Tag
                x={promotion.gate.x + (promotion.right ? 10 : -10)}
                y={promotion.gate.y + 20}
                text="refused · exit 7"
                colour="#fb7185"
                anchor={promotion.right ? "start" : "end"}
                size={9}
                opacity={promotion.opacity}
              />
            ) : null}
          </g>
        ) : null,
      )}

      {wide ? null : (
        <g>
          <text x={W / 2} y={H - 46} textAnchor="middle" fontSize={10.5} fill={GLOBAL_LANE}>
            ↑ promote-global: copied, with a link back
          </text>
          <text x={W / 2} y={H - 28} textAnchor="middle" fontSize={10.5} fill={SLATE}>
            ↓ read into every pack, labelled, ≤ 15%
          </text>
        </g>
      )}
    </g>
  );
}

// ─── The figure ──────────────────────────────────────────────────────────────

/** Seconds into each scene's loop that a still frame shows: what a reader who asks for less motion sees. */
const STATIC_T = [0.62, 1.95, 4.3, 2.15] as const;

const ARIA = [
  "One agent working in a repository. The repository's .ee directory holds ee.db, the FrankenSQLite database that is the source of truth, and a search index derived from it that can be rebuilt. The agent writes memories into the database with remember and outcome, and reads packs and search results from it.",
  "A swarm of agents around one shared store. All the agents read at the same time. Writes move inward to a ring that stands for the store's single writer. Through the daemon it can commit a whole batch in one transaction; with the one-shot CLI it lets one write in at a time.",
  "Four teammates, each with their own store and agents, linked peer to peer with no server. Signed events pass between them, and each one that reaches you becomes a local row in your store with trust class peer_human_attested.",
  "Your workspaces around one user-global store on this machine. Promotions move up to the evidence gate around it: human_explicit and agent_validated memories pass, and others are refused with exit 7. House rules are read back into every workspace except payroll, which sets participate = false.",
] as const;

/** A clock in seconds that advances only while `running`, at no more than 40 frames a second. */
function useClock(running: boolean): number {
  const [time, setTime] = useState(0);
  useEffect(() => {
    if (!running) return;
    let frame = 0;
    let last: number | null = null;
    let pending = 0;
    const tick = (now: number) => {
      if (last !== null) pending += Math.min(0.1, (now - last) / 1000);
      last = now;
      if (pending >= 1 / 40) {
        const step = pending;
        pending = 0;
        setTime((value) => value + step);
      }
      frame = window.requestAnimationFrame(tick);
    };
    frame = window.requestAnimationFrame(tick);
    return () => window.cancelAnimationFrame(frame);
  }, [running]);
  return time;
}

function Scene({
  rung,
  width,
  running,
  reduced,
  mode,
}: {
  rung: number;
  width: number;
  running: boolean;
  reduced: boolean;
  mode: WriteMode;
}) {
  const t = useClock(running);
  const W = Math.max(300, width);
  const wide = W >= 640;
  const H = wide ? 360 : 440;
  const time = t + (STATIC_T[rung] ?? 0);
  // React's ids carry punctuation that a url(#…) reference cannot.
  const id = `scale${useId().replace(/[^a-zA-Z0-9_-]/g, "")}`;
  const group = useRef<SVGGElement>(null);
  const previous = useRef(rung);

  // Zooming out, the new scene arrives from closer in; zooming back in, from further out.
  useEffect(() => {
    const from = previous.current;
    previous.current = rung;
    const node = group.current;
    if (from === rung || reduced || !node || typeof node.animate !== "function") return;
    const out = rung > from;
    node.animate(
      [
        { opacity: 0, transform: `scale(${out ? 1.16 : 0.86})` },
        { opacity: 1, transform: "scale(1)" },
      ],
      { duration: 650, easing: "cubic-bezier(0.2, 0.7, 0.2, 1)" },
    );
  }, [rung, reduced]);

  const props: SceneProps = { W, H, wide, time, animate: running, id };
  return (
    <svg
      className="diagram block h-auto w-full overflow-visible"
      width={W}
      height={H}
      viewBox={`0 0 ${W} ${H}`}
      role="img"
      aria-label={ARIA[rung as 0 | 1 | 2 | 3]}
    >
      <defs>
        <marker id={`${id}-write`} viewBox="0 0 10 10" refX="8" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
          <path d="M 0 0 L 10 5 L 0 10 z" fill={WRITE} />
        </marker>
        <marker id={`${id}-read`} viewBox="0 0 10 10" refX="8" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
          <path d="M 0 0 L 10 5 L 0 10 z" fill={READ} />
        </marker>
      </defs>
      <g ref={group} style={{ transformBox: "fill-box", transformOrigin: "center" }}>
        {rung === 0 ? <OneAgent {...props} /> : null}
        {rung === 1 ? <Swarm {...props} mode={mode} /> : null}
        {rung === 2 ? <Team {...props} /> : null}
        {rung === 3 ? <Everywhere {...props} /> : null}
      </g>
    </svg>
  );
}

/** Text with `backticked` spans set as inline commands. */
function rich(text: string): ReactNode {
  return text.split(/(`[^`]+`)/).map((part, index) =>
    part.startsWith("`") && part.endsWith("`") ? <Cmd key={index}>{part.slice(1, -1)}</Cmd> : <Fragment key={index}>{part}</Fragment>,
  );
}

/** Four nested frames, the current scale lit: a minimap of the zoom. */
function ZoomGlyph({ level }: { level: number }) {
  return (
    <span className="inline-flex items-center gap-2 text-[color:var(--chapter,#fb923c)]" aria-hidden="true">
      <svg width="30" height="30" viewBox="0 0 30 30">
        {[0, 1, 2, 3].map((step) => {
          const half = 3.5 + step * 3.6;
          const on = step === level;
          return (
            <rect
              key={step}
              x={15 - half}
              y={15 - half}
              width={half * 2}
              height={half * 2}
              rx={1.5 + step}
              fill="none"
              stroke={on ? "currentColor" : "rgba(148,163,184,0.35)"}
              strokeWidth={on ? 1.7 : 1}
            />
          );
        })}
      </svg>
      <span className="font-mono text-[10px] tracking-[0.16em] text-slate-500 uppercase">zoom {level + 1}/4</span>
    </span>
  );
}

function Lanes({ inPlay }: { inPlay: number }) {
  const same = overlap({ lane: "team", text: "pin the toolchain" }, { lane: "workspace", text: "pin the toolchain" });
  const different = overlap({ lane: "global", text: "rebase, never merge" }, { lane: "workspace", text: "merge, never rebase" });
  return (
    <div className="min-w-0 rounded-2xl border border-white/[0.08] bg-black/25 p-4 sm:p-5">
      <p className="text-[9px] font-black tracking-[0.2em] text-[color:var(--chapter,#fb923c)] uppercase">
        Lanes · fixed precedence on overlap
      </p>
      <ol className="mt-3 space-y-1.5" aria-label="Memory lanes, most specific first">
        {LANES.map((lane, index) => {
          const on = index < inPlay;
          return (
            <li
              key={lane.id}
              className={cn(
                "flex items-center gap-3 rounded-xl border px-3 py-2 transition-opacity duration-500",
                on ? "border-white/[0.12] bg-white/[0.03]" : "border-white/[0.05] opacity-40",
              )}
            >
              <span className="font-mono text-[11px] text-slate-500 tabular-nums">{index + 1}</span>
              <span aria-hidden="true" className="h-2.5 w-2.5 shrink-0 rounded-full" style={{ background: lane.colour }} />
              <span className="min-w-0">
                <span className="block font-mono text-[12.5px] text-white">{lane.id}</span>
                <span className="block text-[12px] leading-snug text-slate-400">{lane.what}</span>
              </span>
              {on ? null : (
                <span className="ml-auto shrink-0 font-mono text-[9.5px] tracking-[0.12em] text-slate-500 uppercase">zoom out</span>
              )}
            </li>
          );
        })}
      </ol>
      <ul className="mt-4 space-y-2.5 text-[12.5px] leading-relaxed text-slate-400">
        {same ? (
          <li>
            <span className="text-white">Same subject, same text:</span> the {same.moreSpecific} row wins over the{" "}
            {same.lessSpecific} copy, and the decision is recorded.
          </li>
        ) : null}
        {different ? (
          <li>
            <span className="text-white">Same subject, different text:</span> both stay in the pack, flagged, and ranking
            does not choose between them (<span className="font-mono text-[11px] text-amber-200/90">team_lane_conflict_deferred</span>,{" "}
            <span className="font-mono text-[11px] text-amber-200/90">global_lane_conflict_deferred</span>).
          </li>
        ) : null}
      </ul>
    </div>
  );
}

/** One scale's card: what it adds, the sourced facts, and the commands that drive it. */
function RungCard({ rung, at }: { rung: Rung; at: number }) {
  return (
    <>
      <p className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <span className="font-mono text-[11px] text-[color:var(--chapter,#fb923c)] tabular-nums">
          {String(at + 1).padStart(2, "0")} / {String(RUNGS.length).padStart(2, "0")}
        </span>
        <span className="text-lg font-extrabold tracking-[-0.02em] text-white">{rung.title}</span>
      </p>
      <p className="mt-2 text-[14.5px] leading-relaxed text-slate-300">{rung.lead}</p>
      <ul className="mt-4 space-y-2.5">
        {rung.facts.map((fact) => (
          <li key={fact.text} className="flex gap-3 text-[13.5px] leading-relaxed text-slate-300">
            <span aria-hidden="true" className="mt-[0.6em] h-1.5 w-1.5 shrink-0 rounded-full bg-[color:var(--chapter,#fb923c)]" />
            <span className="min-w-0">
              {rich(fact.text)}{" "}
              <span className="font-mono text-[10.5px] [overflow-wrap:anywhere] text-slate-500">{fact.source}</span>
            </span>
          </li>
        ))}
      </ul>
      <div className="mt-4 rounded-xl border border-white/[0.06] bg-black/30 px-3 py-2.5">
        <p className="text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase">Commands</p>
        {/* One command per line, scrolling sideways on a phone rather than breaking inside a flag. */}
        <pre className="mt-1.5 overflow-x-auto font-mono text-[11px] leading-relaxed whitespace-pre text-violet-100 sm:text-[12px]">
          <code>{rung.commands.join("\n")}</code>
        </pre>
      </div>
    </>
  );
}

/** The line under the drawing: one per scale, and one for each of the swarm's two ways of writing. */
const CAPTIONS: readonly { key: string; text: string }[] = [
  {
    key: "one",
    text: "A write goes into the database, the index is derived from it, and a pack reads from both.",
  },
  {
    key: "swarm-daemon",
    text: "A real batch closes within a millisecond, or at 64 writes; the drawing runs far slower. Reads do not go through the ring.",
  },
  {
    key: "swarm-oneshot",
    text: "Without the daemon, each one-shot write takes the store's write lock and commits on its own.",
  },
  {
    key: "team",
    text: "Signed events pass from member to member. Each one that reaches you becomes a row in your own store, labelled with its sender.",
  },
  {
    key: "everywhere",
    text: "Promotions are checked at the evidence gate around the global store. House rules are read back into every workspace that joins the lane.",
  },
];

const STEP_MS = 7000;
const MODES = [
  { value: "daemon", label: "Through the daemon" },
  { value: "oneshot", label: "One-shot, in turn" },
] as const satisfies readonly { value: WriteMode; label: string }[];

/**
 * The same memory model at four scales, zooming out: one agent beside its store; a swarm
 * sharing that store through one writer; a team of stores linked peer to peer; and the
 * user-global lane every workspace can read. Below the drawing, what each scale adds and
 * the fixed precedence of the three lanes.
 */
export function ScaleLadder() {
  const { ref, width } = useFigureWidth(860);
  const frame = useRef<HTMLDivElement>(null);
  const bar = useRef<HTMLSpanElement>(null);
  const reduced = useReducedMotion();
  const inView = useInView(frame);
  const running = inView && !reduced;
  const [rungId, setRungId] = useState<RungId>("one");
  const [auto, setAuto] = useState(true);
  const [mode, setMode] = useState<WriteMode>("daemon");
  const index = Math.max(0, RUNGS.findIndex((entry) => entry.id === rungId));
  const rung = RUNGS[index] ?? RUNGS[0]!;

  // The tour: until the reader picks a scale, zoom out one rung at a time.
  useEffect(() => {
    if (!running || !auto) return;
    const handle = window.setTimeout(() => {
      setRungId((current) => {
        const at = RUNGS.findIndex((entry) => entry.id === current);
        return RUNGS[(at + 1) % RUNGS.length]?.id ?? "one";
      });
    }, STEP_MS);
    return () => window.clearTimeout(handle);
  }, [running, auto, rungId]);

  // A hairline that fills while the tour holds this scale.
  useEffect(() => {
    const node = bar.current;
    if (!node) return;
    node.style.transform = "scaleX(0)";
    if (!running || !auto || typeof node.animate !== "function") return;
    const animation = node.animate([{ transform: "scaleX(0)" }, { transform: "scaleX(1)" }], {
      duration: STEP_MS,
      easing: "linear",
      fill: "forwards",
    });
    return () => animation.cancel();
  }, [running, auto, rungId]);

  const caption = rung.id === "swarm" ? `swarm-${mode}` : rung.id;
  // While the tour moves on by itself, a screen reader is not read each new scale; once the reader drives, it is.
  const touring = running && auto;

  return (
    <DemoFrame
      eyebrow="Zoom out · four scales"
      title="How ee's memory works at four scales."
      basis="diagram"
      source={
        <>
          Each fact names its own source above. Also: src/daemon/server.rs (the write actor: 64 rows, 1,000 µs) ·
          src/core/global_store.rs (lane_specificity_rank, surface_precedence_conflicts) · src/core/context.rs (the
          lane-conflict codes) · docs/adr/0077, 0083, 0086. The agents, people, workspaces and memories drawn are
          illustrative, and every motion is slowed down.
        </>
      }
    >
      <div ref={frame}>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <p className="max-w-2xl text-[15px] leading-relaxed text-slate-300">
            ee uses the same kind of store, the same trust classes and the same rules for one agent on one repository as
            for every workspace you use. No scale needs a server. Where lanes overlap, the precedence rules below apply.
          </p>
          {/* The label says what a press does, so no aria-pressed as well: "Play, pressed" would read as playing. */}
          <DemoButton
            onClick={() => setAuto((on) => !on)}
            className={cn(!auto && "border-violet-400/60 text-violet-200")}
            disabled={reduced}
            label={auto && !reduced ? "Pause the zoom" : "Play the zoom"}
          >
            {auto && !reduced ? <Pause className="h-3.5 w-3.5" aria-hidden="true" /> : <Play className="h-3.5 w-3.5" aria-hidden="true" />}
            {auto && !reduced ? "Pause" : "Play"}
          </DemoButton>
        </div>

        <div className="mt-5 flex flex-wrap items-center gap-x-4 gap-y-3">
          <Segmented
            label="Scale"
            value={rungId}
            onChange={(next) => {
              setAuto(false);
              setRungId(next);
            }}
            options={RUNGS.map((entry, at) => ({ value: entry.id, label: `${at + 1} · ${entry.short}` }))}
          />
          <ZoomGlyph level={index} />
        </div>
        <div className="mt-3 h-[2px] overflow-hidden rounded-full bg-white/[0.06]" aria-hidden="true">
          <span ref={bar} className="block h-full origin-left bg-[color:var(--chapter,#fb923c)]" style={{ transform: "scaleX(0)" }} />
        </div>

        <div ref={ref} className="mt-5 w-full">
          <Scene rung={index} width={width} running={running} reduced={reduced} mode={mode} />
        </div>

        {/* Below the drawing, every scale's caption, readouts and card are laid out in one cell each and only the
            current one shows, so each block is as tall as its tallest and the page never moves as the tour zooms out.
            The swarm's write-mode switch keeps its place, unseen, on the other scales for the same reason. */}
        <div className="mt-3 flex min-h-9 flex-wrap items-center justify-between gap-x-4 gap-y-2">
          <div className="grid max-w-2xl">
            {CAPTIONS.map((entry) => (
              <p
                key={entry.key}
                aria-hidden={entry.key === caption ? undefined : true}
                className={cn("min-w-0 text-[12.5px] leading-relaxed text-slate-500 [grid-area:1/1]", entry.key !== caption && "invisible")}
              >
                {entry.text}
              </p>
            ))}
          </div>
          <Segmented
            className={cn(rung.id !== "swarm" && "invisible")}
            label="How writes reach the store"
            value={mode}
            onChange={(next) => {
              setAuto(false);
              setMode(next);
            }}
            options={MODES}
          />
        </div>

        <div className="mt-4 grid">
          {RUNGS.map((entry, at) => (
            <div
              key={entry.id}
              aria-hidden={at === index ? undefined : true}
              className={cn("grid min-w-0 grid-cols-2 gap-2 [grid-area:1/1] lg:grid-cols-4", at !== index && "invisible")}
            >
              {entry.readouts.map((item) => (
                <Readout
                  key={item.label}
                  label={item.label}
                  value={
                    item.long ? (
                      <span className="block text-[13px] leading-snug [overflow-wrap:anywhere] sm:text-[14px]">{item.value}</span>
                    ) : (
                      item.value
                    )
                  }
                  note={item.note}
                />
              ))}
            </div>
          ))}
        </div>

        <div className="mt-4 grid grid-cols-1 gap-4 lg:grid-cols-[minmax(0,1.45fr)_minmax(0,1fr)]">
          <div
            className="grid min-w-0 rounded-2xl border border-white/[0.08] bg-black/25 p-4 sm:p-5"
            aria-live={touring ? "off" : "polite"}
          >
            {RUNGS.map((entry, at) => (
              <div
                key={entry.id}
                aria-hidden={at === index ? undefined : true}
                className={cn("min-w-0 [grid-area:1/1]", at !== index && "invisible")}
              >
                <RungCard rung={entry} at={at} />
              </div>
            ))}
          </div>
          <Lanes inPlay={rung.lanes} />
        </div>
      </div>
    </DemoFrame>
  );
}
