// The arithmetic behind components/explain/scale.tsx: one campaign's lessons, handed back three ways.
//
// Derived, not measured. Every number here that is not one of ee's constants is an assumption, and
// the figure states each one. The model is one fixed, seeded draw of 520 lessons. The three lanes see
// the SAME lessons and differ only in how they decide what goes into a session.

import { seeded } from "@/lib/utils";

// ─── ee's constants (verified at HEAD) ───────────────────────────────────────

/** src/pack/mod.rs · DEFAULT_CONTEXT_MAX_TOKENS. The budget counts each memory's own content tokens. */
export const EE_BUDGET_TOKENS = 4_000;
/** src/config/merge.rs · built_in_config · [pack] candidate_pool. */
export const EE_CANDIDATE_POOL = 100;
/** src/pack/mod.rs · DEFAULT_MMR_RELEVANCE_WEIGHT: gain = 0.75 × relevance − 0.25 × max similarity to the pack. */
export const MMR_RELEVANCE_WEIGHT = 0.75;
/** src/pack/mod.rs · DEFAULT_COVERAGE_FILL_RELEVANCE_FLOOR. */
export const FILL_FLOOR = 0.05;

// ─── The assumptions ─────────────────────────────────────────────────────────

export const WEEKS = 52;
/** Lessons written per week, by every agent on the project together: about two a working day. */
export const LESSONS_PER_WEEK = 10;
/** One lesson: a markdown line in the file, a chunk in the vector store, a memory's content in ee. */
export const LESSON_TOKENS = 50;
/**
 * What ee's markdown pack adds to each item beyond its content, which the budget does not count: the heading with
 * the memory id, the fence, why it was chosen, its trust class and its provenance (src/pack/mod.rs, the markdown
 * renderer). tests/fixtures/golden/agent/context_pack.md.golden spends 248 characters on them for one item; with
 * its identifiers and digit runs that is roughly 80 tokens.
 */
export const RECEIPT_TOKENS = 80;
/** Share of new lessons that restate a lesson already written and still true. */
export const RESTATE_SHARE = 0.2;
/** Each week, this share of still-true lessons is overtaken by a change: a renamed flag, a reversed decision. */
export const STALE_PER_WEEK = 0.015;
/** The notes file is trimmed whenever it passes this many lines (10,000 tokens)... */
export const NOTES_TRIM_AT = 200;
/** ...back to this many (6,000 tokens): stale and repeated lines first, then the oldest. */
export const NOTES_TRIM_TO = 120;
/** The vector store returns its top k lessons: the same 4,000 tokens of lessons ee's budget buys. */
export const VECTOR_K = EE_BUDGET_TOKENS / LESSON_TOKENS;
/**
 * The chance each week that something marks a stale lesson in ee, so that it stops being retrieved: a revision,
 * a harmful outcome, a contradiction the pack's guard settles, or decay. ee cannot tell on its own that a
 * lesson went stale.
 */
export const EE_CATCH_PER_WEEK = 0.15;
/** Toy similarity between two lessons, for MMR: two copies of one fact, two lessons on one topic, any two others. */
export const SIMILARITY = { sameFact: 0.9, sameTopic: 0.3, other: 0.1 } as const;

// ─── Topics and tasks ────────────────────────────────────────────────────────

export const TOPICS = [
  { id: "tests", weight: 0.22 },
  { id: "api", weight: 0.2 },
  { id: "storage", weight: 0.16 },
  { id: "build", weight: 0.15 },
  { id: "docs", weight: 0.15 },
  { id: "release", weight: 0.12 },
] as const;

export type Topic = (typeof TOPICS)[number]["id"];

export const TASKS = [
  { id: "flaky", label: "Fix a flaky test", topic: "tests" },
  { id: "migrate", label: "Migrate the schema", topic: "storage" },
  { id: "release", label: "Cut a release", topic: "release" },
] as const satisfies readonly { id: string; label: string; topic: Topic }[];

export type TaskId = (typeof TASKS)[number]["id"];

export function taskTopic(task: TaskId): Topic {
  return (TASKS.find((entry) => entry.id === task) ?? TASKS[0]).topic;
}

// ─── The lessons ─────────────────────────────────────────────────────────────

export interface Lesson {
  id: number;
  /** The week it was written, 1..WEEKS. */
  week: number;
  topic: Topic;
  /** The id of the lesson this one restates, or its own id: every copy of a fact shares it. */
  fact: number;
  /** Similarity to each task in [0, 1]: what both the vector store and ee's search rank by. */
  score: Record<TaskId, number>;
  /** The first week in which the fact is no longer true (may lie past WEEKS). Every copy shares it. */
  staleWeek: number;
  /** The first week in which ee no longer retrieves this copy: some weeks after it went stale. */
  caughtWeek: number;
}

/** Weeks until an event with probability `p` a week, at least 1; `u` is uniform in [0, 1). */
const geometric = (u: number, p: number) => Math.max(1, Math.ceil(Math.log(1 - u) / Math.log(1 - p)));

function pickTopic(u: number): Topic {
  let sum = 0;
  for (const topic of TOPICS) {
    sum += topic.weight;
    if (u < sum) return topic.id;
  }
  return (TOPICS[TOPICS.length - 1] ?? TOPICS[0]).id;
}

/** On-task lessons score in [0.3, 1], others in [0, 0.58]: the top of a ranking is mostly on-task, never only. */
const freshScore = (u: number, onTask: boolean) => (onTask ? 0.3 + 0.7 * u : 0.58 * u);

function draw(): Lesson[] {
  const random = seeded(20_261_124);
  const lessons: Lesson[] = [];
  for (let week = 1; week <= WEEKS; week++) {
    for (let i = 0; i < LESSONS_PER_WEEK; i++) {
      // Eight draws for every lesson, whichever branch it takes, so no branch shifts the stream after it.
      const u = Array.from({ length: 8 }, random);
      const at = (index: number) => u[index] ?? 0;
      const id = lessons.length;
      const topic = pickTopic(at(0));
      const roots =
        at(1) < RESTATE_SHARE
          ? lessons.filter((lesson) => lesson.fact === lesson.id && lesson.topic === topic && lesson.staleWeek > week)
          : [];
      const root = roots[Math.floor(at(2) * roots.length)];
      const score = {} as Record<TaskId, number>;
      TASKS.forEach((task, index) => {
        const value = root ? root.score[task.id] + (at(3 + index) - 0.5) * 0.04 : freshScore(at(3 + index), topic === task.topic);
        score[task.id] = Math.min(1, Math.max(0, value));
      });
      const staleWeek = root ? root.staleWeek : week + geometric(at(6), STALE_PER_WEEK);
      // Each copy is caught on its own: marking one restatement does not retire its siblings.
      const caughtWeek = staleWeek + geometric(at(7), EE_CATCH_PER_WEEK);
      lessons.push({ id, week, topic, fact: root ? root.fact : id, score, staleWeek, caughtWeek });
    }
  }
  return lessons;
}

export const LESSONS: readonly Lesson[] = draw();

const lessonAt = (id: number) => LESSONS[id] as Lesson;

// ─── What a session is handed ────────────────────────────────────────────────

/** How one injected line serves this session. Checked in this order: stale, then a repeat, then on-task or not. */
export type Category = "useful" | "repeat" | "offtask" | "stale";

export const CATEGORIES: readonly Category[] = ["useful", "repeat", "offtask", "stale"];

/** Each injected line, in injection order: a later copy of a fact already in the session is a repeat. */
export function categorize(ids: readonly number[], week: number, task: TaskId): Category[] {
  const topic = taskTopic(task);
  const seen = new Set<number>();
  return ids.map((id) => {
    const lesson = lessonAt(id);
    if (lesson.staleWeek <= week) return "stale";
    if (seen.has(lesson.fact)) return "repeat";
    seen.add(lesson.fact);
    return lesson.topic === topic ? "useful" : "offtask";
  });
}

export type Lane = "notes" | "vector" | "ee";

export type OmissionReason = "token_budget_exceeded" | "redundant_candidate" | "below_relevance_floor";

export interface Session {
  lane: Lane;
  week: number;
  /** Lesson ids in injection order: file order for the notes file, rank order for the other two. */
  ids: number[];
  categories: Category[];
  counts: Record<Category, number>;
  /** Tokens of lessons in the session. */
  lessonTokens: number;
  /** Everything the session reads: the lessons plus, for ee, each item's receipts. */
  tokens: number;
  /** Useful lines / lines; 0 for an empty session. */
  usefulShare: number;
  /** True lessons about the task, written by this week, with no copy in the session. */
  missing: number;
  /** ee: candidates it dropped, by reason (data.pack.skipped[]). Empty for the other two. */
  skipped: Partial<Record<OmissionReason, number>>;
  /** ee: of `missing`, how many were among the dropped candidates, so listed with a reason. */
  missingListed: number;
  /** Notes: true lessons deleted by trims so far, every topic. */
  lost: number;
}

const byScore = (task: TaskId) => (a: Lesson, b: Lesson) => b.score[task] - a.score[task] || a.id - b.id;

interface NotesWeek {
  ids: number[];
  lost: number;
}

/**
 * The notes file week by week. Every lesson is appended. Past NOTES_TRIM_AT lines a careful person trims it:
 * every stale line and every repeat first, then the oldest lines, down to NOTES_TRIM_TO. A lesson is lost when
 * it was still true and no copy of it is left.
 */
function notesHistory(): NotesWeek[] {
  const history: NotesWeek[] = [];
  let file: number[] = [];
  let lost = 0;
  for (let week = 1; week <= WEEKS; week++) {
    file = [...file, ...LESSONS.filter((lesson) => lesson.week === week).map((lesson) => lesson.id)];
    if (file.length > NOTES_TRIM_AT) {
      const before = new Set(file.map(lessonAt).filter((lesson) => lesson.staleWeek > week).map((lesson) => lesson.fact));
      const seen = new Set<number>();
      const kept = file.filter((id) => {
        const lesson = lessonAt(id);
        if (lesson.staleWeek <= week || seen.has(lesson.fact)) return false;
        seen.add(lesson.fact);
        return true;
      });
      file = kept.slice(Math.max(0, kept.length - NOTES_TRIM_TO));
      const after = new Set(file.map((id) => lessonAt(id).fact));
      for (const fact of before) if (!after.has(fact)) lost += 1;
    }
    history.push({ ids: file, lost });
  }
  return history;
}

const NOTES = notesHistory();

/** The weeks in which the notes file was trimmed. */
export const TRIM_WEEKS: readonly number[] = NOTES.flatMap((entry, index) =>
  index > 0 && entry.lost > (NOTES[index - 1] as NotesWeek).lost ? [index + 1] : [],
);

/** Facts about the task, written by `week` and still true. */
function trueFacts(week: number, task: TaskId): Set<number> {
  const topic = taskTopic(task);
  const facts = new Set<number>();
  for (const lesson of LESSONS) {
    if (lesson.week > week) break;
    if (lesson.topic === topic && lesson.staleWeek > week) facts.add(lesson.fact);
  }
  return facts;
}

function session(
  lane: Lane,
  week: number,
  task: TaskId,
  ids: number[],
  extra: { receipts?: boolean; lost?: number; skipped?: Partial<Record<OmissionReason, number>>; listed?: Set<number> } = {},
): Session {
  const categories = categorize(ids, week, task);
  const counts: Record<Category, number> = { useful: 0, repeat: 0, offtask: 0, stale: 0 };
  for (const category of categories) counts[category] += 1;
  const delivered = new Set(ids.filter((_, index) => categories[index] !== "stale").map((id) => lessonAt(id).fact));
  let missing = 0;
  let missingListed = 0;
  for (const fact of trueFacts(week, task)) {
    if (delivered.has(fact)) continue;
    missing += 1;
    if (extra.listed?.has(fact)) missingListed += 1;
  }
  const lessonTokens = ids.length * LESSON_TOKENS;
  return {
    lane,
    week,
    ids,
    categories,
    counts,
    lessonTokens,
    tokens: lessonTokens + (extra.receipts ? ids.length * RECEIPT_TOKENS : 0),
    usefulShare: ids.length === 0 ? 0 : counts.useful / ids.length,
    missing,
    skipped: extra.skipped ?? {},
    missingListed,
    lost: extra.lost ?? 0,
  };
}

/** The whole file, every session. */
export function notesSession(week: number, task: TaskId): Session {
  const entry = NOTES[week - 1] as NotesWeek;
  return session("notes", week, task, entry.ids, { lost: entry.lost });
}

/** The k most similar lessons ever written, whatever their age, truth or repetition. */
export function vectorSession(week: number, task: TaskId): Session {
  const ids = LESSONS.filter((lesson) => lesson.week <= week)
    .sort(byScore(task))
    .slice(0, VECTOR_K)
    .map((lesson) => lesson.id);
  return session("vector", week, task, ids);
}

function similarity(a: Lesson, b: Lesson): number {
  if (a.fact === b.fact) return SIMILARITY.sameFact;
  return a.topic === b.topic ? SIMILARITY.sameTopic : SIMILARITY.other;
}

/**
 * ee's pack at toy scale. Search ranks by the same similarity as the vector store, without the lessons ee has
 * caught; the top EE_CANDIDATE_POOL are candidates. Strict MMR then takes the candidate with the highest
 * 0.75 × relevance − 0.25 × (its greatest similarity to anything packed), while it fits the budget. A candidate
 * whose gain is not positive waits for coverage fill, which admits it above the relevance floor if budget is
 * left, up to as many as MMR chose. Section quotas and ee's preview and link-only tiers are not modelled:
 * every line is a full lesson.
 */
export function eeSession(week: number, task: TaskId): Session {
  const pool = LESSONS.filter((lesson) => lesson.week <= week && lesson.caughtWeek > week)
    .sort(byScore(task))
    .slice(0, EE_CANDIDATE_POOL);
  const remaining = [...pool];
  const nearest = remaining.map(() => 0);
  const packed: Lesson[] = [];
  const deferred: Lesson[] = [];
  const skipped: Partial<Record<OmissionReason, number>> = {};
  const listed = new Set<number>();
  const omit = (lesson: Lesson, reason: OmissionReason) => {
    skipped[reason] = (skipped[reason] ?? 0) + 1;
    listed.add(lesson.fact);
  };
  const fits = () => (packed.length + 1) * LESSON_TOKENS <= EE_BUDGET_TOKENS;
  const gain = (index: number) =>
    MMR_RELEVANCE_WEIGHT * (remaining[index] as Lesson).score[task] - (1 - MMR_RELEVANCE_WEIGHT) * (nearest[index] ?? 0);

  while (remaining.length > 0) {
    let best = 0;
    for (let index = 1; index < remaining.length; index++) {
      const a = remaining[index] as Lesson;
      const b = remaining[best] as Lesson;
      const difference = gain(index) - gain(best);
      if (difference > 1e-12 || (Math.abs(difference) <= 1e-12 && byScore(task)(a, b) < 0)) best = index;
    }
    const bestGain = gain(best);
    const [lesson] = remaining.splice(best, 1) as [Lesson];
    nearest.splice(best, 1);
    if (bestGain <= 0) {
      deferred.push(lesson);
      continue;
    }
    if (!fits()) {
      omit(lesson, "token_budget_exceeded");
      continue;
    }
    packed.push(lesson);
    remaining.forEach((other, index) => {
      nearest[index] = Math.max(nearest[index] ?? 0, similarity(lesson, other));
    });
  }

  const limit = packed.length;
  let filled = 0;
  for (const lesson of deferred.sort(byScore(task))) {
    if (lesson.score[task] < FILL_FLOOR) omit(lesson, "below_relevance_floor");
    else if (filled >= limit) omit(lesson, "redundant_candidate");
    else if (!fits()) omit(lesson, "token_budget_exceeded");
    else {
      packed.push(lesson);
      filled += 1;
    }
  }
  // A fact is listed as left out only if no copy of it made the pack.
  for (const lesson of packed) listed.delete(lesson.fact);
  return session(
    "ee",
    week,
    task,
    packed.map((lesson) => lesson.id),
    { receipts: true, skipped, listed },
  );
}

export const LANE_SESSION: Record<Lane, (week: number, task: TaskId) => Session> = {
  notes: notesSession,
  vector: vectorSession,
  ee: eeSession,
};

export interface SeriesPoint {
  week: number;
  tokens: number;
  lessonTokens: number;
  usefulShare: number;
  stale: number;
}

const seriesCache = new Map<string, SeriesPoint[]>();

/** Weeks 1..WEEKS for one lane and task: what the line charts draw. */
export function series(lane: Lane, task: TaskId): SeriesPoint[] {
  const key = `${lane}:${task}`;
  const cached = seriesCache.get(key);
  if (cached) return cached;
  const points = Array.from({ length: WEEKS }, (_, index) => {
    const result = LANE_SESSION[lane](index + 1, task);
    return {
      week: result.week,
      tokens: result.tokens,
      lessonTokens: result.lessonTokens,
      usefulShare: result.usefulShare,
      stale: result.counts.stale,
    };
  });
  seriesCache.set(key, points);
  return points;
}

/** The campaign so far, for the status line. */
export function campaign(week: number, task: TaskId) {
  const topic = taskTopic(task);
  const written = LESSONS.filter((lesson) => lesson.week <= week);
  return {
    written: written.length,
    stale: written.filter((lesson) => lesson.staleWeek <= week).length,
    onTask: written.filter((lesson) => lesson.topic === topic).length,
    onTaskTrue: trueFacts(week, task).size,
  };
}
