// The negative evidence ledger, as a toy. A hypothesis space with a hidden
// landscape (a few winning cells, several contiguous "families" of ideas that
// look promising and fail, and a quiet remainder), searched one session at a
// time by the same heuristic agent twice: once with no memory beyond its own
// session, once with a ledger in the shape of ee's (every failure recorded with
// its family; three failures in one family distilled into an anti-pattern that
// prunes the family). Everything here is invented for the page; only the loop
// (capture, family, distill, pack) is ee's. Pure and deterministic: the same
// seed draws the same landscape and runs the same two campaigns everywhere.

import { seeded } from "@/lib/utils";

export const COLS = 28;
export const ROWS = 16;
export const CELL_COUNT = COLS * ROWS;
export const ATTEMPTS_PER_SESSION = 4;
export const SESSIONS = 24;
/** Recorded failures in one family before curation distills it: ee's own two proposers wait for three. */
export const DISTILL_AT = 3;
export const FAMILY_NAMES = ["A", "B", "C", "D", "E", "F"] as const;
export const WIN_COUNT = 3;
/** The page opens on this landscape. The test suite checks that the toy's averages, not just this seed, tell the same story. */
export const DEFAULT_SEED = 22;

/** How strongly the agent is drawn to what looks promising: it picks a cell with weight appeal ** SHARPNESS. */
const SHARPNESS = 5;
/** Every cell stays possible, however dull it looks. */
const FLOOR_WEIGHT = 1e-4;

export type Mode = "none" | "ee";

export type Family = {
  id: number;
  name: string;
  cells: number[];
  /** The family's centre in cell units, for its label. */
  cx: number;
  cy: number;
};

export type Landscape = {
  seed: number;
  /** How promising each cell looks to the agent, 0 to 1. Row-major, COLS × ROWS. */
  appeal: number[];
  /** The dead-end family a cell belongs to, or -1. */
  family: number[];
  isWin: boolean[];
  wins: number[];
  families: Family[];
};

export type Attempt = {
  cell: number;
  win: boolean;
  /** The cell's family, or -1. The agent knows which family an idea belongs to; it does not know the family fails. */
  family: number;
  /** The cell had already failed in an earlier session: a dead end paid for twice. */
  repeat: boolean;
};

export type Session = {
  /** 1-based. */
  number: number;
  attempts: Attempt[];
  /** Families curation distilled into an anti-pattern at the end of this session. Always empty without memory. */
  distilled: number[];
};

export const cellX = (cell: number) => cell % COLS;
export const cellY = (cell: number) => Math.floor(cell / COLS);
export const cellAt = (x: number, y: number) => y * COLS + x;

/** A 32-bit mix of two integers, so each session's draws are independent of the others' and the same in both panels. */
export function mix(a: number, b: number): number {
  let h = Math.imul(a ^ 0x9e3779b9, 0x85ebca6b) ^ Math.imul((b + 0x632be5ab) | 0, 0xc2b2ae35);
  h ^= h >>> 15;
  h = Math.imul(h, 0x27d4eb2f);
  h ^= h >>> 13;
  return h >>> 0;
}

const at = <T>(list: readonly T[], index: number) => list[index] as T;

function neighbours4(cell: number): number[] {
  const x = cellX(cell);
  const y = cellY(cell);
  const out: number[] = [];
  if (x > 0) out.push(cell - 1);
  if (x < COLS - 1) out.push(cell + 1);
  if (y > 0) out.push(cell - COLS);
  if (y < ROWS - 1) out.push(cell + COLS);
  return out;
}

function neighbours8(cell: number): number[] {
  const x = cellX(cell);
  const y = cellY(cell);
  const out: number[] = [];
  for (let dy = -1; dy <= 1; dy++) {
    for (let dx = -1; dx <= 1; dx++) {
      if (dx === 0 && dy === 0) continue;
      const nx = x + dx;
      const ny = y + dy;
      if (nx >= 0 && nx < COLS && ny >= 0 && ny < ROWS) out.push(cellAt(nx, ny));
    }
  }
  return out;
}

type Blob = { x: number; y: number; rx: number; ry: number; amp: number };

/** The hidden landscape for one seed. */
export function makeLandscape(seed: number): Landscape {
  const random = seeded(mix(seed, 0x1ed6e));
  const range = (low: number, high: number) => low + (high - low) * random();

  // Family centres, spaced apart and inset from the edges; the spacing relaxes if a seed packs them badly.
  const blobs: Blob[] = [];
  let spacing = 8;
  for (let tries = 0; blobs.length < FAMILY_NAMES.length; tries++) {
    if (tries > 0 && tries % 400 === 0) spacing -= 0.5;
    const x = range(3.2, COLS - 4.2);
    const y = range(2.6, ROWS - 3.6);
    if (blobs.some((b) => Math.hypot(b.x - x, (b.y - y) * 1.35) < spacing)) continue;
    blobs.push({ x, y, rx: range(2.0, 3.4), ry: range(1.3, 2.2), amp: range(0.62, 1) });
  }
  // Named along the approach axis, left to right.
  blobs.sort((a, b) => a.x - b.x);

  const edgeNoise = Array.from({ length: CELL_COUNT }, () => random() - 0.5);
  const family = new Array<number>(CELL_COUNT).fill(-1);
  const families: Family[] = [];
  blobs.forEach((blob, id) => {
    const inside = (cell: number) => {
      const dx = (cellX(cell) - blob.x) / blob.rx;
      const dy = (cellY(cell) - blob.y) / blob.ry;
      return dx * dx + dy * dy + 0.5 * at(edgeNoise, cell) < 1;
    };
    // A family never touches another, so each reads as its own region.
    const free = (cell: number) =>
      at(family, cell) === -1 && neighbours8(cell).every((n) => at(family, n) === -1 || at(family, n) === id);
    // Grow from the centre over 4-connected cells inside the noisy ellipse: one contiguous region.
    const start = cellAt(Math.round(blob.x), Math.round(blob.y));
    const cells: number[] = [];
    if (free(start)) {
      const queue = [start];
      family[start] = id;
      for (let head = 0; head < queue.length; head++) {
        const cell = at(queue, head);
        cells.push(cell);
        for (const n of neighbours4(cell)) {
          if (at(family, n) === -1 && inside(n) && free(n)) {
            family[n] = id;
            queue.push(n);
          }
        }
      }
    }
    cells.sort((a, b) => a - b);
    families.push({ id, name: at(FAMILY_NAMES, id), cells, cx: blob.x, cy: blob.y });
  });

  // Wins sit a short walk outside a family: the answer is often next door to the obvious wrong idea.
  const nearFamily = (cell: number) => neighbours8(cell).some((n) => at(family, n) !== -1);
  const distanceTo = (cell: number, cells: readonly number[]) =>
    Math.min(...cells.map((c) => Math.max(Math.abs(cellX(c) - cellX(cell)), Math.abs(cellY(c) - cellY(cell)))));
  const order = families.map((f) => f.id).sort((a, b) => (at(blobs, b).amp - at(blobs, a).amp) || a - b);
  const wins: number[] = [];
  for (let w = 0; w < WIN_COUNT; w++) {
    const host = at(families, at(order, (w * 2) % order.length));
    const clear = (cell: number) =>
      at(family, cell) === -1 && !nearFamily(cell) && wins.every((other) => distanceTo(cell, [other]) >= 5);
    let pool: number[] = [];
    for (let cell = 0; cell < CELL_COUNT; cell++) {
      if (!clear(cell) || host.cells.length === 0) continue;
      const d = distanceTo(cell, host.cells);
      if (d >= 2 && d <= 4) pool.push(cell);
    }
    if (pool.length === 0) pool = Array.from({ length: CELL_COUNT }, (_, cell) => cell).filter(clear);
    wins.push(at(pool, Math.floor(random() * pool.length)));
  }
  wins.sort((a, b) => a - b);
  const isWin = Array.from({ length: CELL_COUNT }, (_, cell) => wins.includes(cell));

  // What looks promising: a low rolling ground, a strong rise over each family (that is what makes it a trap),
  // and a gentler one around each win.
  const waves = Array.from({ length: 3 }, () => ({
    kx: range(0.18, 0.5),
    ky: range(0.2, 0.6),
    phase: range(0, Math.PI * 2),
    amp: range(0.04, 0.08),
  }));
  const raw = Array.from({ length: CELL_COUNT }, (_, cell) => {
    const x = cellX(cell);
    const y = cellY(cell);
    let value = 0.14 + 0.05 * (random() - 0.5);
    for (const wave of waves) value += wave.amp * Math.sin(wave.kx * x + wave.ky * y + wave.phase);
    for (const blob of blobs) {
      const dx = (x - blob.x) / (blob.rx + 0.6);
      const dy = (y - blob.y) / (blob.ry + 0.6);
      value += blob.amp * Math.exp(-(dx * dx + dy * dy) * 0.9);
    }
    for (const win of wins) {
      const d2 = (x - cellX(win)) ** 2 + (y - cellY(win)) ** 2;
      value += 0.42 * Math.exp(-d2 / (2 * 1.3 * 1.3));
    }
    return Math.max(0, value);
  });
  const top = Math.max(...raw) || 1;
  const appeal = raw.map((value) => Math.min(1, value / top));

  return { seed, appeal, family, isWin, wins, families };
}

/**
 * One campaign: SESSIONS sessions of ATTEMPTS_PER_SESSION attempts each, every attempt drawn with weight
 * appeal ** SHARPNESS from the cells the agent still considers open. Without memory that is every cell it has
 * not already tried this session. With the ledger it also excludes every recorded failure, every recorded win and
 * every cell of a family an anti-pattern has pruned. Both modes use the same random draws for the same session,
 * so session 1, before anything is remembered, is identical in both.
 */
export function runCampaign(landscape: Landscape, mode: Mode, sessions = SESSIONS): Session[] {
  const weight = landscape.appeal.map((value) => value ** SHARPNESS + FLOOR_WEIGHT);
  const failedBefore = new Set<number>();
  const recorded = new Set<number>();
  const found = new Set<number>();
  const familyFailures = new Array<number>(landscape.families.length).fill(0);
  const pruned = new Set<number>();
  const log: Session[] = [];

  for (let number = 1; number <= sessions; number++) {
    const random = seeded(mix(landscape.seed, number));
    const picked = new Set<number>();
    const attempts: Attempt[] = [];
    const open = (cell: number) => {
      if (picked.has(cell)) return false;
      if (mode === "none") return true;
      const f = at(landscape.family, cell);
      return !recorded.has(cell) && !found.has(cell) && !(f >= 0 && pruned.has(f));
    };
    for (let k = 0; k < ATTEMPTS_PER_SESSION; k++) {
      let total = 0;
      for (let cell = 0; cell < CELL_COUNT; cell++) if (open(cell)) total += at(weight, cell);
      if (total <= 0) break;
      let target = random() * total;
      let choice = -1;
      for (let cell = 0; cell < CELL_COUNT; cell++) {
        if (!open(cell)) continue;
        choice = cell;
        target -= at(weight, cell);
        if (target < 0) break;
      }
      if (choice < 0) break;
      picked.add(choice);
      attempts.push({
        cell: choice,
        win: at(landscape.isWin, choice),
        family: at(landscape.family, choice),
        repeat: failedBefore.has(choice),
      });
    }

    // The session ends: what happened is known to the world, and to the ledger if there is one.
    for (const attempt of attempts) {
      if (!attempt.win) failedBefore.add(attempt.cell);
      if (mode === "none") continue;
      if (attempt.win) {
        found.add(attempt.cell);
      } else {
        recorded.add(attempt.cell);
        if (attempt.family >= 0) familyFailures[attempt.family] = at(familyFailures, attempt.family) + 1;
      }
    }
    const distilled: number[] = [];
    if (mode === "ee") {
      landscape.families.forEach((f) => {
        if (!pruned.has(f.id) && at(familyFailures, f.id) >= DISTILL_AT) {
          pruned.add(f.id);
          distilled.push(f.id);
        }
      });
    }
    log.push({ number, attempts, distilled });
  }
  return log;
}

export type CellState = {
  /** Times the cell was tried and failed. */
  failures: number;
  /** Of those, how many came after it had already failed in an earlier session. */
  repeats: number;
  win: boolean;
  /** Tried in the latest session shown. */
  current: boolean;
};

export type Snapshot = {
  shown: number;
  attempts: number;
  repeats: number;
  winsFound: number;
  firstWin: number | null;
  /** Family id → the session at whose end curation pruned it. */
  pruned: Map<number, number>;
  /** Failures recorded per family. */
  familyFailures: number[];
  /** Attempts that landed in each family, failures or not. */
  familyAttempts: number[];
  /** Untried cells inside pruned families: ideas the anti-patterns ruled out without anyone paying for them. */
  skipped: number;
  cells: CellState[];
};

/** The campaign as it stands after `shown` sessions. */
export function snapshot(landscape: Landscape, log: readonly Session[], shown: number): Snapshot {
  const cells: CellState[] = Array.from({ length: CELL_COUNT }, () => ({ failures: 0, repeats: 0, win: false, current: false }));
  const familyFailures = new Array<number>(landscape.families.length).fill(0);
  const familyAttempts = new Array<number>(landscape.families.length).fill(0);
  const pruned = new Map<number, number>();
  const winCells = new Set<number>();
  let attempts = 0;
  let repeats = 0;
  let firstWin: number | null = null;
  for (const session of log.slice(0, shown)) {
    for (const attempt of session.attempts) {
      const state = at(cells, attempt.cell);
      attempts += 1;
      if (attempt.family >= 0) familyAttempts[attempt.family] = at(familyAttempts, attempt.family) + 1;
      if (attempt.win) {
        state.win = true;
        winCells.add(attempt.cell);
        firstWin ??= session.number;
      } else {
        state.failures += 1;
        if (attempt.family >= 0) familyFailures[attempt.family] = at(familyFailures, attempt.family) + 1;
      }
      if (attempt.repeat) {
        state.repeats += 1;
        repeats += 1;
      }
      state.current = session.number === shown;
    }
    for (const f of session.distilled) pruned.set(f, session.number);
  }
  let skipped = 0;
  for (const f of pruned.keys()) {
    for (const cell of at(landscape.families, f).cells) if (at(cells, cell).failures === 0) skipped += 1;
  }
  return {
    shown,
    attempts,
    repeats,
    winsFound: winCells.size,
    firstWin,
    pruned,
    familyFailures,
    familyAttempts,
    skipped,
    cells,
  };
}
