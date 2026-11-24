// `bun test`: the invariants of the campaign model behind components/explain/scale.tsx, and a drift check
// that the ee constants and commands the figure names still say what it says they do.

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import {
  categorize,
  EE_BUDGET_TOKENS,
  EE_CANDIDATE_POOL,
  eeSession,
  FILL_FLOOR,
  LANE_SESSION,
  LESSON_TOKENS,
  LESSONS,
  LESSONS_PER_WEEK,
  MMR_RELEVANCE_WEIGHT,
  NOTES_TRIM_AT,
  NOTES_TRIM_TO,
  notesSession,
  RECEIPT_TOKENS,
  series,
  TASKS,
  TRIM_WEEKS,
  VECTOR_K,
  vectorSession,
  WEEKS,
} from "@/components/explain/scale-model";

const LANES = ["notes", "vector", "ee"] as const;
const everyWeek = Array.from({ length: WEEKS }, (_, index) => index + 1);
const total = (counts: Partial<Record<string, number>>) =>
  Object.values(counts).reduce<number>((sum, n) => sum + (n ?? 0), 0);

describe("the lessons", () => {
  test("one fixed draw: ten a week for a year, ids in order", () => {
    expect(LESSONS.length).toBe(WEEKS * LESSONS_PER_WEEK);
    LESSONS.forEach((lesson, index) => {
      expect(lesson.id).toBe(index);
      expect(lesson.week).toBe(Math.floor(index / LESSONS_PER_WEEK) + 1);
    });
  });

  test("a restatement points at an earlier original on its topic, and shares its fate", () => {
    for (const lesson of LESSONS) {
      if (lesson.fact === lesson.id) continue;
      const root = LESSONS[lesson.fact];
      expect(root).toBeDefined();
      if (!root) continue;
      expect(root.id).toBeLessThan(lesson.id);
      expect(root.fact).toBe(root.id);
      expect(root.topic).toBe(lesson.topic);
      expect(lesson.staleWeek).toBe(root.staleWeek);
      // Only a lesson that is still true gets restated.
      expect(root.staleWeek).toBeGreaterThan(lesson.week);
    }
  });

  test("ee only ever catches a lesson after it has gone stale", () => {
    for (const lesson of LESSONS) expect(lesson.caughtWeek).toBeGreaterThan(lesson.staleWeek);
  });
});

describe("every session", () => {
  test("is deterministic", () => {
    for (const lane of LANES) {
      for (const task of TASKS) {
        expect(LANE_SESSION[lane](37, task.id)).toEqual(LANE_SESSION[lane](37, task.id));
      }
    }
  });

  test("accounts for every line, and a repeat always follows an earlier copy in the same session", () => {
    for (const lane of LANES) {
      for (const task of TASKS) {
        for (const week of everyWeek) {
          const session = LANE_SESSION[lane](week, task.id);
          expect(total(session.counts)).toBe(session.ids.length);
          expect(session.usefulShare).toBeGreaterThanOrEqual(0);
          expect(session.usefulShare).toBeLessThanOrEqual(1);
          expect(new Set(session.ids).size).toBe(session.ids.length);
          session.ids.forEach((id, index) => {
            const lesson = LESSONS[id];
            expect(lesson?.week ?? Number.POSITIVE_INFINITY).toBeLessThanOrEqual(week);
            if (session.categories[index] !== "repeat") return;
            const earlier = session.ids.slice(0, index).map((other) => LESSONS[other]?.fact);
            expect(earlier).toContain(lesson?.fact);
          });
        }
      }
    }
  });

  test("the series the charts draw are the sessions, week by week", () => {
    for (const lane of LANES) {
      const points = series(lane, "flaky");
      expect(points.length).toBe(WEEKS);
      for (const point of points) {
        const session = LANE_SESSION[lane](point.week, "flaky");
        expect(point.tokens).toBe(session.tokens);
        expect(point.usefulShare).toBe(session.usefulShare);
        expect(point.stale).toBe(session.counts.stale);
      }
    }
  });
});

describe("the notes file", () => {
  test("is injected whole, never passes the trim line, and is cut back to the trim target", () => {
    let previous = 0;
    for (const week of everyWeek) {
      const session = notesSession(week, "flaky");
      expect(session.ids.length).toBeLessThanOrEqual(NOTES_TRIM_AT);
      expect(session.tokens).toBe(session.ids.length * LESSON_TOKENS);
      if (TRIM_WEEKS.includes(week)) expect(session.ids.length).toBeLessThanOrEqual(NOTES_TRIM_TO);
      else expect(session.ids.length).toBe(Math.min(NOTES_TRIM_AT, previous + LESSONS_PER_WEEK));
      // File order: oldest first.
      expect([...session.ids].sort((a, b) => a - b)).toEqual(session.ids);
      previous = session.ids.length;
    }
    expect(TRIM_WEEKS.length).toBeGreaterThan(0);
  });

  test("a trim leaves no stale line and no repeat, and lessons lost only ever grows", () => {
    let lost = 0;
    for (const week of everyWeek) {
      const session = notesSession(week, "migrate");
      expect(session.lost).toBeGreaterThanOrEqual(lost);
      lost = session.lost;
      if (!TRIM_WEEKS.includes(week)) continue;
      // The week's new lessons arrive before the trim, so the trimmed file is clean.
      expect(session.counts.stale).toBe(0);
      expect(session.counts.repeat).toBe(0);
    }
    expect(lost).toBeGreaterThan(0);
  });
});

describe("the vector store", () => {
  test("returns the k most similar lessons ever written, in rank order", () => {
    for (const task of TASKS) {
      for (const week of everyWeek) {
        const session = vectorSession(week, task.id);
        expect(session.ids.length).toBe(Math.min(VECTOR_K, week * LESSONS_PER_WEEK));
        expect(session.tokens).toBe(session.ids.length * LESSON_TOKENS);
        const scores = session.ids.map((id) => LESSONS[id]?.score[task.id] ?? -1);
        for (let index = 1; index < scores.length; index++) {
          expect(scores[index - 1] ?? -1).toBeGreaterThanOrEqual(scores[index] ?? -1);
        }
      }
    }
  });
});

describe("ee's pack", () => {
  test("keeps its lessons within the budget, and adds its receipts on top", () => {
    for (const task of TASKS) {
      for (const week of everyWeek) {
        const session = eeSession(week, task.id);
        expect(session.lessonTokens).toBeLessThanOrEqual(EE_BUDGET_TOKENS);
        expect(session.tokens).toBe(session.lessonTokens + session.ids.length * RECEIPT_TOKENS);
      }
    }
  });

  test("packs only candidates it has not caught going stale, and accounts for every candidate", () => {
    for (const task of TASKS) {
      for (const week of everyWeek) {
        const session = eeSession(week, task.id);
        for (const id of session.ids) expect(LESSONS[id]?.caughtWeek ?? 0).toBeGreaterThan(week);
        const eligible = LESSONS.filter((lesson) => lesson.week <= week && lesson.caughtWeek > week).length;
        // Packed or listed with a reason: nothing in the candidate pool vanishes silently.
        expect(session.ids.length + total(session.skipped)).toBe(Math.min(EE_CANDIDATE_POOL, eligible));
        expect(session.missingListed).toBeLessThanOrEqual(session.missing);
      }
    }
  });

  test("its first line is the candidate with the highest relevance", () => {
    for (const task of TASKS) {
      const session = eeSession(30, task.id);
      const pool = LESSONS.filter((lesson) => lesson.week <= 30 && lesson.caughtWeek > 30);
      const top = Math.max(...pool.map((lesson) => lesson.score[task.id]));
      expect(LESSONS[session.ids[0] ?? -1]?.score[task.id]).toBe(top);
    }
  });

  test("is not flattered: a year in, it still carries stale, repeated and off-task lines, and misses lessons", () => {
    for (const task of TASKS) {
      const session = eeSession(WEEKS, task.id);
      expect(session.counts.stale).toBeGreaterThan(0);
      expect(session.counts.repeat).toBeGreaterThan(0);
      expect(session.counts.offtask).toBeGreaterThan(0);
      expect(session.missing).toBeGreaterThan(0);
      expect(session.usefulShare).toBeLessThan(0.6);
    }
  });
});

test("categorize: stale beats repeat beats topic", () => {
  const stale = LESSONS.find((lesson) => lesson.staleWeek <= 10 && lesson.week <= 10);
  expect(stale).toBeDefined();
  if (!stale) return;
  expect(categorize([stale.id, stale.id], 10, "flaky")).toEqual(["stale", "stale"]);
});

describe("the ee facts the figure states", () => {
  const repo = join(import.meta.dir, "..", "..");
  const source = (path: string) => readFileSync(join(repo, path), "utf8");

  test("the pack's constants", () => {
    const pack = source("src/pack/mod.rs");
    expect(EE_BUDGET_TOKENS).toBe(4000);
    expect(pack).toContain("pub const DEFAULT_CONTEXT_MAX_TOKENS: u32 = 4_000;");
    expect(pack).toContain(`pub const DEFAULT_MMR_RELEVANCE_WEIGHT: f32 = ${MMR_RELEVANCE_WEIGHT};`);
    expect(pack).toContain(`pub const DEFAULT_COVERAGE_FILL_RELEVANCE_FLOOR: f32 = ${FILL_FLOOR};`);
    expect(pack).toContain('Self::RedundantCandidate => "redundant_candidate"');
    expect(pack).toContain('Self::ContradictionSuppressed => "contradiction_suppressed"');
    expect(pack).toContain("pub fn apply_contradiction_guard(");
    // The budget counts each memory's own content, and the markdown renderer adds the receipts around it.
    expect(pack).toContain('output.push_str(&format!("**Why:** {}\\n\\n"');
    expect(source("src/core/context.rs")).toContain("estimated_tokens: estimate_tokens_default(&memory.content)");
  });

  test("the built-in [pack] defaults", () => {
    const merge = source("src/config/merge.rs");
    expect(merge).toContain(`candidate_pool: Some(${EE_CANDIDATE_POOL}),`);
    expect(merge).toContain('default_profile: Some("balanced".to_string()),');
  });

  test("omissions are reported at data.pack.skipped[]", () => {
    const output = source("src/output/mod.rs");
    expect(output).toContain('pack.field_array_of_objects("skipped"');
    expect(output).toContain('"skippedTotal"');
  });

  test("the AGENTS.md bridge", () => {
    const agentsmd = source("src/core/agentsmd.rs");
    expect(agentsmd).toContain("`ee export agentsmd` renders the primer `rules` + `warnings` sections");
    expect(agentsmd).toContain("It NEVER edits outside its markers");
    expect(agentsmd).toContain("`ee diag agentsmd-drift` (read-only)");
    expect(source("src/core/primer.rs")).toContain("pub const PRIMER_DEFAULT_BUDGET_TOKENS: u32 = 600;");
  });
});
