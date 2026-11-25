// `bun test`: the negative evidence ledger toy (components/explain/ledger-model.ts). The figure is a toy,
// but it claims a few things on its face: the two panels search the same landscape with the same heuristic,
// the panel with the ledger never pays twice for a dead end, a pruned family stays pruned, and the opening
// landscape is a typical one rather than the best one. These check each claim over many seeds.

import { describe, expect, test } from "bun:test";

import {
  ATTEMPTS_PER_SESSION,
  CELL_COUNT,
  cellX,
  cellY,
  COLS,
  DEFAULT_SEED,
  DISTILL_AT,
  FAMILY_NAMES,
  type Landscape,
  makeLandscape,
  ROWS,
  runCampaign,
  type Session,
  SESSIONS,
  snapshot,
  WIN_COUNT,
} from "@/components/explain/ledger-model";

const SEEDS = Array.from({ length: 120 }, (_, index) => index + 1);

const chebyshev = (a: number, b: number) => Math.max(Math.abs(cellX(a) - cellX(b)), Math.abs(cellY(a) - cellY(b)));

function connected(cells: readonly number[]): boolean {
  const set = new Set(cells);
  const first = cells[0];
  if (first === undefined) return false;
  const seen = new Set([first]);
  const queue = [first];
  for (let head = 0; head < queue.length; head++) {
    const cell = queue[head] as number;
    const x = cellX(cell);
    const next = [x > 0 ? cell - 1 : -1, x < COLS - 1 ? cell + 1 : -1, cell - COLS, cell + COLS];
    for (const n of next) {
      if (n >= 0 && n < CELL_COUNT && set.has(n) && !seen.has(n)) {
        seen.add(n);
        queue.push(n);
      }
    }
  }
  return seen.size === set.size;
}

const median = (values: readonly number[]) => {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.floor(sorted.length / 2)] as number;
};

const NEVER = SESSIONS + 1;

describe("the landscape", () => {
  test("the same seed draws the same landscape and runs the same two campaigns", () => {
    for (const seed of [1, DEFAULT_SEED, 99]) {
      const a = makeLandscape(seed);
      const b = makeLandscape(seed);
      expect(b).toEqual(a);
      expect(runCampaign(b, "none")).toEqual(runCampaign(a, "none"));
      expect(runCampaign(b, "ee")).toEqual(runCampaign(a, "ee"));
    }
    expect(makeLandscape(2).appeal).not.toEqual(makeLandscape(3).appeal);
  });

  test("six contiguous families that never touch, and three wins outside them", () => {
    for (const seed of SEEDS) {
      const landscape = makeLandscape(seed);
      expect(landscape.appeal).toHaveLength(CELL_COUNT);
      expect(landscape.families).toHaveLength(FAMILY_NAMES.length);
      expect(Math.max(...landscape.appeal)).toBe(1);
      expect(Math.min(...landscape.appeal)).toBeGreaterThanOrEqual(0);
      for (const family of landscape.families) {
        expect(family.cells.length).toBeGreaterThanOrEqual(5);
        expect(connected(family.cells)).toBe(true);
        for (const cell of family.cells) {
          expect(landscape.family[cell]).toBe(family.id);
          // No cell of another family within one step, diagonals included.
          for (let other = 0; other < CELL_COUNT; other++) {
            const f = landscape.family[other] as number;
            if (f >= 0 && f !== family.id) expect(chebyshev(cell, other)).toBeGreaterThan(1);
          }
        }
      }
      expect(landscape.wins).toHaveLength(WIN_COUNT);
      expect(new Set(landscape.wins).size).toBe(WIN_COUNT);
      for (const win of landscape.wins) {
        expect(landscape.isWin[win]).toBe(true);
        for (const family of landscape.families) for (const cell of family.cells) expect(chebyshev(win, cell)).toBeGreaterThan(1);
      }
      expect(landscape.isWin.filter(Boolean)).toHaveLength(WIN_COUNT);
    }
  });

  test("every landscape the New landscape button can reach keeps the same shape", () => {
    // The button steps the seed through 1..999 ((seed % 999) + 1), well past the 120 seeds checked in detail above.
    // Cheaper checks here, one expect per seed: six families of five or more connected cells, none touching another
    // (8-neighbours), and three distinct wins, none touching a family.
    for (let seed = 1; seed <= 999; seed++) {
      const landscape = makeLandscape(seed);
      const problems: string[] = [];
      const touches = (cell: number, allowed: number) => {
        const x = cellX(cell);
        const y = cellY(cell);
        for (let dy = -1; dy <= 1; dy++) {
          for (let dx = -1; dx <= 1; dx++) {
            const nx = x + dx;
            const ny = y + dy;
            if (nx < 0 || nx >= COLS || ny < 0 || ny >= ROWS) continue;
            const f = landscape.family[ny * COLS + nx] as number;
            if (f >= 0 && f !== allowed) return true;
          }
        }
        return false;
      };
      if (landscape.families.length !== FAMILY_NAMES.length) problems.push("family count");
      for (const family of landscape.families) {
        if (family.cells.length < 5) problems.push(`family ${family.name}: ${family.cells.length} cells`);
        if (!connected(family.cells)) problems.push(`family ${family.name}: not contiguous`);
        if (family.cells.some((cell) => touches(cell, family.id))) problems.push(`family ${family.name}: touches another`);
      }
      if (new Set(landscape.wins).size !== WIN_COUNT) problems.push("wins");
      if (landscape.wins.some((win) => touches(win, -1))) problems.push("a win touches a family");
      expect(`${seed}: ${problems.join(", ")}`).toBe(`${seed}: `);
    }
  });

  test("the families look more promising than the rest: that is what makes them traps", () => {
    for (const seed of SEEDS) {
      const landscape = makeLandscape(seed);
      const inFamily = landscape.appeal.filter((_, cell) => (landscape.family[cell] as number) >= 0);
      const outside = landscape.appeal.filter((_, cell) => landscape.family[cell] === -1);
      const mean = (values: number[]) => values.reduce((s, v) => s + v, 0) / values.length;
      expect(mean(inFamily)).toBeGreaterThan(1.5 * mean(outside));
    }
  });
});

function check(landscape: Landscape, log: readonly Session[], fn: (session: Session, index: number) => void) {
  expect(log).toHaveLength(SESSIONS);
  log.forEach((session, index) => {
    expect(session.number).toBe(index + 1);
    expect(session.attempts).toHaveLength(ATTEMPTS_PER_SESSION);
    expect(new Set(session.attempts.map((a) => a.cell)).size).toBe(ATTEMPTS_PER_SESSION);
    for (const attempt of session.attempts) {
      expect(attempt.win).toBe(landscape.isWin[attempt.cell] as boolean);
      expect(attempt.family).toBe(landscape.family[attempt.cell] as number);
    }
    fn(session, index);
  });
}

describe("the two campaigns", () => {
  test("session 1, before anything is remembered, is the same in both panels", () => {
    for (const seed of SEEDS) {
      const landscape = makeLandscape(seed);
      expect(runCampaign(landscape, "ee")[0]?.attempts).toEqual(runCampaign(landscape, "none")[0]?.attempts as Session["attempts"]);
    }
  });

  test("without memory, a repeat is exactly a cell that already failed in an earlier session", () => {
    let repeats = 0;
    for (const seed of SEEDS) {
      const landscape = makeLandscape(seed);
      const failed = new Set<number>();
      const log = runCampaign(landscape, "none");
      check(landscape, log, (session) => {
        expect(session.distilled).toEqual([]);
        for (const attempt of session.attempts) {
          expect(attempt.repeat).toBe(failed.has(attempt.cell));
          if (attempt.repeat) repeats += 1;
        }
        for (const attempt of session.attempts) if (!attempt.win) failed.add(attempt.cell);
      });
    }
    expect(repeats).toBeGreaterThan(0);
  });

  test("with the ledger, no cell is ever tried twice, so a recorded failure is never paid for again", () => {
    for (const seed of SEEDS) {
      const landscape = makeLandscape(seed);
      const tried = new Set<number>();
      check(landscape, runCampaign(landscape, "ee"), (session) => {
        for (const attempt of session.attempts) {
          expect(attempt.repeat).toBe(false);
          expect(tried.has(attempt.cell)).toBe(false);
        }
        for (const attempt of session.attempts) tried.add(attempt.cell);
      });
    }
  });

  test(`a family is distilled at the end of the session its recorded failures reach ${DISTILL_AT}, once, and stays pruned`, () => {
    for (const seed of SEEDS) {
      const landscape = makeLandscape(seed);
      const failures = new Array<number>(landscape.families.length).fill(0);
      const pruned = new Set<number>();
      check(landscape, runCampaign(landscape, "ee"), (session) => {
        for (const attempt of session.attempts) {
          // Nothing is tried inside a family pruned at the end of an earlier session.
          if (attempt.family >= 0) expect(pruned.has(attempt.family)).toBe(false);
          if (!attempt.win && attempt.family >= 0) failures[attempt.family] = (failures[attempt.family] as number) + 1;
        }
        const due = landscape.families.filter((f) => !pruned.has(f.id) && (failures[f.id] as number) >= DISTILL_AT).map((f) => f.id);
        expect(session.distilled).toEqual(due);
        for (const id of due) pruned.add(id);
      });
    }
  });

  test("the snapshot adds up to the log it folds", () => {
    for (const seed of SEEDS.slice(0, 30)) {
      const landscape = makeLandscape(seed);
      for (const mode of ["none", "ee"] as const) {
        const log = runCampaign(landscape, mode);
        const empty = snapshot(landscape, log, 0);
        expect(empty.attempts).toBe(0);
        expect(empty.firstWin).toBeNull();
        expect(empty.cells.every((c) => c.failures === 0 && !c.win && !c.current)).toBe(true);
        for (const shown of [1, 7, SESSIONS]) {
          const snap = snapshot(landscape, log, shown);
          const flat = log.slice(0, shown).flatMap((s) => s.attempts);
          expect(snap.attempts).toBe(shown * ATTEMPTS_PER_SESSION);
          expect(snap.repeats).toBe(flat.filter((a) => a.repeat).length);
          expect(snap.cells.reduce((s, c) => s + c.failures, 0)).toBe(flat.filter((a) => !a.win).length);
          expect(snap.cells.filter((c) => c.current).length).toBe(log[shown - 1]?.attempts.length ?? -1);
          const firstWin = log.slice(0, shown).find((s) => s.attempts.some((a) => a.win))?.number ?? null;
          expect(snap.firstWin).toBe(firstWin);
          let skipped = 0;
          for (const id of snap.pruned.keys()) {
            skipped += (landscape.families[id]?.cells ?? []).filter((cell) => snap.cells[cell]?.failures === 0).length;
          }
          expect(snap.skipped).toBe(skipped);
          if (mode === "none") expect(snap.pruned.size).toBe(0);
        }
      }
    }
  });
});

describe("what the caption says the toy shows", () => {
  const runs = Array.from({ length: 200 }, (_, index) => {
    const landscape = makeLandscape(index + 1);
    return {
      none: snapshot(landscape, runCampaign(landscape, "none"), SESSIONS),
      ee: snapshot(landscape, runCampaign(landscape, "ee"), SESSIONS),
    };
  });

  test("without memory a large share of attempts re-try a known dead end; with the ledger none do", () => {
    const repeats = runs.map((run) => run.none.repeats);
    expect(runs.every((run) => run.ee.repeats === 0)).toBe(true);
    expect(median(repeats)).toBeGreaterThan(0.2 * SESSIONS * ATTEMPTS_PER_SESSION);
  });

  test("with the ledger a first win usually comes sooner, but not always: it is a toy, not a rigged race", () => {
    const sooner = runs.filter((run) => (run.ee.firstWin ?? NEVER) < (run.none.firstWin ?? NEVER)).length;
    const later = runs.filter((run) => (run.ee.firstWin ?? NEVER) > (run.none.firstWin ?? NEVER)).length;
    expect(sooner).toBeGreaterThan(runs.length / 2);
    expect(later).toBeGreaterThan(0);
    expect(median(runs.map((run) => run.ee.firstWin ?? NEVER))).toBeLessThan(median(runs.map((run) => run.none.firstWin ?? NEVER)));
  });

  test("the landscape the page opens on is a typical one, not the best one", () => {
    const landscape = makeLandscape(DEFAULT_SEED);
    const none = snapshot(landscape, runCampaign(landscape, "none"), SESSIONS);
    const ee = snapshot(landscape, runCampaign(landscape, "ee"), SESSIONS);
    const repeats = runs.map((run) => run.none.repeats);
    const eeFirst = runs.map((run) => run.ee.firstWin ?? NEVER).sort((a, b) => a - b);
    const quartile = (q: number) => eeFirst[Math.floor(q * (eeFirst.length - 1))] as number;
    expect(Math.abs(none.repeats - median(repeats))).toBeLessThanOrEqual(0.2 * median(repeats));
    expect(ee.firstWin ?? NEVER).toBeGreaterThanOrEqual(quartile(0.25));
    expect(ee.firstWin ?? NEVER).toBeLessThanOrEqual(quartile(0.75));
    expect(ee.pruned.size).toBe(FAMILY_NAMES.length);
  });

  test("every per-cell array the figure indexes covers the whole grid", () => {
    // Field draws COLS × ROWS cells and reads each one's appeal, family, win and snapshot state by index.
    const landscape = makeLandscape(DEFAULT_SEED);
    const snap = snapshot(landscape, runCampaign(landscape, "ee"), SESSIONS);
    for (const values of [landscape.appeal, landscape.family, landscape.isWin, snap.cells]) expect(values).toHaveLength(COLS * ROWS);
    for (const family of landscape.families) for (const cell of family.cells) expect(cell).toBeLessThan(COLS * ROWS);
  });
});
