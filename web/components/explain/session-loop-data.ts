// The session loop's stations, their geometry on the ring, and the small tokenizers the figure draws
// with. Kept apart from the component so `bun test` can check every station against the repository:
// each JSON key and identifier-like value in an excerpt must appear in a file the station cites
// (tests/session-loop.test.ts).

export type EffectKind = "read" | "write" | "harness";

/** A file a station's claims were checked against, and a string that must still be in it. */
export interface Source {
  file: string;
  mark: string;
}

export interface JsonLine {
  text: string;
  /** A line the agent reads: one of the station's `reads` paths ends on this line's key. */
  hot: boolean;
}

export interface Station {
  id: string;
  /** The label on the ring: the subcommand. */
  name: string;
  /** The panel's heading. */
  title: string;
  /** The question the agent is asking at this station. */
  question: string;
  /** The same question in at most two short lines, for the ring's hub. */
  hub: readonly string[];
  effect: EffectKind;
  /** The ring label's second line. */
  tag: string;
  /** The panel's effect chip. */
  chip: string;
  command: string | null;
  /** A line under the command. */
  note?: string;
  /** The JSON paths the agent reads, in the order it reads them. */
  reads: readonly string[];
  /** A footnote under the paths. */
  footnote?: string;
  returns: string;
  because: string;
  /** What the excerpt shows, when it is one case of several. */
  case?: string;
  json: readonly JsonLine[] | null;
  sources: readonly Source[];
}

/**
 * An excerpt written as JSON text. A line whose first character is `>` is one the agent reads; the `>`
 * stands in for the line's first space of indentation, so the excerpt stays aligned in the source.
 */
function excerpt(text: string): JsonLine[] {
  return text
    .replace(/^\n/, "")
    .replace(/\n\s*$/, "")
    .split("\n")
    .map((line) => (line.startsWith(">") ? { text: ` ${line.slice(1)}`, hot: true } : { text: line, hot: false }));
}

export const STATIONS: readonly Station[] = [
  {
    id: "resume",
    name: "resume",
    title: "Pick up the thread",
    question: "Where was I?",
    hub: ["Where was I?"],
    effect: "read",
    tag: "read-only",
    chip: "read-only",
    command: "ee resume --workspace . --json",
    reads: ["data.report.sessions", "data.report.openLoops", "data.report.nearbyStores", "data.report.nextCommands"],
    returns:
      "The last three sessions' episodic memories, decisions waiting on a revisit, items tagged next, todo or blocking, and anything a newer memory has since superseded.",
    because:
      "A missing or empty store is not an error. Resume returns a successful empty report that names populated stores nearby, with the exact command to retarget, and never creates a store. An agent started in the wrong directory is pointed at the right one instead of being nudged into ee init.",
    case: "a store with nothing to resume",
    json: excerpt(`
{
  "schema": "ee.response.v2",
  "data": {
    "report": {
      "schema": "ee.resume.v1",
      "episodicTotal": 0,
>     "sessions": [],
>     "openLoops": { "revisitDecisionsTotal": 0, … },
>     "nearbyStores": {
        "stores": [{ "documents": …, "provenance": "child_scan", … }],
        "outcome": "complete"
      },
>     "nextCommands": ["ee resume --workspace … --database … --json"]
    }
  },
  "degraded": [{ "code": "resume_no_session_evidence", … }]
}
`),
    sources: [
      { file: "src/core/resume.rs", mark: "genuinely_missing_database_returns_empty_resume_without_initializing_store" },
      { file: "src/cli/mod.rs", mark: "resume_no_session_evidence" },
      { file: "docs/schemas/ee.resume.v1.json", mark: "nearbyStores" },
      { file: "src/core/effect.rs", mark: "Assemble the session-resume bundle" },
    ],
  },
  {
    id: "orient",
    name: "orient",
    title: "Read the room",
    question: "What am I walking into?",
    hub: ["What am I", "walking into?"],
    effect: "read",
    tag: "read-only",
    chip: "read-only",
    command: 'ee orient "<task>" --workspace . --json',
    reads: ["data.doctor.posture", "data.pack.pack.items", "data.nextCommands", "degraded[]"],
    returns:
      "One bundle: the doctor's posture and its top repairs, workspace hygiene, the swarm brief, a pack for the task, open decisions, and the commands to run next.",
    because:
      "A thin pack can mean a cold store or a broken one. Orient puts the doctor's posture beside the pack so the agent can tell which, and says on its face that it changed nothing: sideEffectFree is true and configMutation is never.",
    json: excerpt(`
{
  "schema": "ee.response.v2",
  "data": {
    "schema": "ee.orient.v1",
    "mode": "full",
    "sideEffectFree": true,
    "configMutation": "never",
>   "doctor": {
>     "posture": "degraded_recoverable",
      "topActionable": [{ "severity": "warning", "repair": "…", … }]
    },
    "workspaceHygiene": { … },
>   "pack": { "pack": { "items": [ … ] }, … },
>   "nextCommands": [ … ]
  },
> "degraded": [ … ]
}
`),
    sources: [
      { file: "src/cli/mod.rs", mark: '"sideEffectFree": true' },
      { file: "src/cli/mod.rs", mark: '"schema": "ee.doctor.robot_triage.v1"' },
      { file: "src/core/doctor.rs", mark: '"degraded_recoverable"' },
      { file: "docs/schemas/ee.orient.v1.json", mark: "workspaceHygiene" },
      { file: "docs/schemas/ee.pack.v2.json", mark: '"items"' },
      { file: "src/core/effect.rs", mark: "Assemble read-only agent orientation bundle" },
    ],
  },
  {
    id: "pack",
    name: "pack",
    title: "The brief",
    question: "What should I know before I start?",
    hub: ["What should I", "know first?"],
    effect: "read",
    tag: "read-only",
    chip: "read-only with --read-only",
    command: 'ee pack "<task>" --workspace . --read-only --max-tokens 4000 --json',
    note: "--read-only writes no pack record, audit row or cache entry.",
    reads: ["data.pack.items[]", "data.pack.skipped[]", "data.pack.skippedTotal", "degraded[]"],
    footnote: "ee pack --help calls the left-out list data.pack.omitted; the key in the JSON is skipped.",
    returns:
      "The memories that fit the token budget, each with why it was chosen, its provenance and its trust class; then the ones left out, and why.",
    because:
      "The budget is the agent's context window, so the pack is honest about its edge. Every memory left out is listed with a reason, such as token_budget_exceeded or redundant_candidate, and anything that weakened retrieval is in degraded[] with a repair command. An empty pack never reads as 'nothing is known'.",
    json: excerpt(`
{
  "schema": "ee.response.v2",
  "data": {
    "pack": {
      "budget": { "maxTokens": 4000, "usedTokens": …, … },
>     "items": [{
        "why": "matched … via lexical (relevance …)",
        "provenance": [{ "uri": "file://AGENTS.md#L164-173", … }],
        "trust": { "class": "human_explicit", … }
      }, …],
>     "skippedTotal": …,
>     "skipped": [
        { "memoryId": "mem_…", "reason": "token_budget_exceeded", … }
      ]
    }
  },
> "degraded": [
    { "code": "embed_model_unavailable", "repair": "ee index …", … }
  ]
}
`),
    sources: [
      { file: "src/cli/mod.rs", mark: "Assemble context without writing pack_records, audit rows, or L2 cache entries." },
      { file: "src/output/mod.rs", mark: "fn build_pack_skipped_item" },
      { file: "src/pack/mod.rs", mark: '"token_budget_exceeded"' },
      { file: "docs/schemas/ee.pack.v2.json", mark: '"skippedTotal"' },
      { file: "tests/fixtures/golden/agent/context_pack.json.golden", mark: '"embed_model_unavailable"' },
    ],
  },
  {
    id: "recall",
    name: "recall",
    title: "Before the edit",
    question: "What is pinned to the code I am changing?",
    hub: ["What is pinned", "to this code?"],
    effect: "read",
    tag: "read-only",
    chip: "read-only",
    command: "ee recall --diff-staged --workspace . --budget-tokens 400 --json",
    reads: [
      "data.recall.items[]",
      "data.recall.items[].anchor",
      "data.recall.items[].freshnessState",
      "data.recall.continuationCursor",
    ],
    returns:
      "The memories anchored to the staged paths, ranked by freshness, confidence and level with warnings first, each marked current, suspect or stale.",
    because:
      "The agent asks with its diff, not with a query it has to phrase. Recall reads git diff --cached and nothing else, and when the budget cuts the list short it hands back a cursor to continue from.",
    json: excerpt(`
{
  "schema": "ee.response.v2",
  "data": {
    "recall": {
      "schema": "ee.recall.v1",
      "query": { "diffStaged": true, "budgetTokens": 400, … },
>     "items": [{
        "memoryId": "mem_…",
>       "anchor": { "kind": "path", "path": "src/db/mod.rs", … },
>       "freshnessState": "current",
        "kind": "failure",
        "contentPreview": "Linker OOM at default -j when rebuilding …"
      }, …],
      "truncated": false,
>     "continuationCursor": null
    }
  },
  "degraded": []
}
`),
    sources: [
      { file: "src/cli/mod.rs", mark: "Recall against the staged changed paths (`git diff --cached`, read-only)." },
      { file: "docs/schemas/ee.recall.v1.json", mark: "warnings-first kind bonus" },
      { file: "tests/fixtures/golden/recall/json.json.golden", mark: '"freshnessState":"current"' },
      { file: "docs/agent-ux/anchored-recall.md", mark: "ee recall --diff-staged" },
    ],
  },
  {
    id: "preflight",
    name: "preflight",
    title: "Hazard history",
    question: "Has this command hurt anyone before?",
    hub: ["Has this command", "hurt anyone?"],
    effect: "read",
    tag: "advisory",
    chip: "read-only · advisory",
    command: 'ee preflight check --cmd "git reset --hard origin/main" --workspace . --json',
    note: "Not wrapped in ee.response.v2: the advisory report is the whole response.",
    reads: ["exitCode", "matches[]", "matches[].ruleId", "matches[].action", "matchedMemories[]"],
    returns: "The rules, and the risk, failure and anti-pattern memories, that match the command, each saying where it came from.",
    because:
      "It is evidence, not a gate. A high_risk match still exits 0; ee never authorizes, denies or runs the command, and an old hook cannot turn a match into a block. --stdin and --cmd-base64 keep the command off argv, where an outer shell guard could misread it.",
    json: excerpt(`
{
  "schema": "ee.preflight.guard.v1",
  "command": "git reset --hard origin/main",
> "exitCode": 0,
> "matches": [{
>   "ruleId": "builtin:git_reset_hard",
>   "action": "high_risk",
    "message": "git reset --hard is on the AGENTS.md …",
    "source": { "kind": "builtin", "name": "git_reset_hard" },
    "resolution": "matched"
  }],
> "matchedMemories": [{ "kind": "failure", "severity": "high", … }],
  "degraded": []
}
`),
    sources: [
      { file: "docs/schemas/ee.preflight.guard.v1.json", mark: "intentionally not wrapped in the ee.response.v2 envelope" },
      { file: "src/core/preflight_guard.rs", mark: '"builtin:git_reset_hard"' },
      { file: "src/core/preflight_guard.rs", mark: 'matches!(kind, "risk" | "anti-pattern" | "failure" | "rule")' },
      { file: "src/cli/mod.rs", mark: "Keeps the raw command off argv" },
      { file: "src/core/effect.rs", mark: "without granting or revoking execution authority" },
    ],
  },
  {
    id: "work",
    name: "the work",
    title: "The work",
    question: "Do the work.",
    hub: ["Do the work."],
    effect: "harness",
    tag: "harness",
    chip: "the harness",
    command: null,
    reads: [],
    returns: "Nothing from ee. Edits, builds, tests and approvals belong to the harness.",
    because:
      "ee is the memory layer the harness calls, not a second agent loop. The harness still owns tools, approvals and the prompt loop; ee comes back in when there is something worth keeping.",
    json: null,
    sources: [{ file: "README.md", mark: "still owns tools, approvals, and the prompt loop" }],
  },
  {
    id: "remember",
    name: "remember",
    title: "Write it down",
    question: "What did I learn that will matter next time?",
    hub: ["What did I", "learn?"],
    effect: "write",
    tag: "writes",
    chip: "writes · memories",
    command: 'ee remember "<lesson>" --workspace . --level procedural --kind rule --idempotency-key <key> --json',
    reads: ["data.memoryId", "data.memory_id", "data.persisted", "data.indexStatus"],
    returns:
      "The new memory's id, level, kind and confidence (0.8 unless you give one), and whether it was persisted and indexed.",
    because:
      "The id comes back in the response, so the agent can grade the memory later without searching for it. With --idempotency-key, a retry after a lost reply returns the original id with status already_recorded instead of writing a duplicate.",
    json: excerpt(`
{
  "schema": "ee.response.v2",
  "success": true,
  "data": {
    "command": "remember",
>   "memory_id": "mem_…",
>   "memoryId": "mem_…",
    "level": "procedural",
    "kind": "rule",
    "confidence": 0.8,
>   "persisted": true,
>   "indexStatus": "indexed",
    "near_duplicates": [ … ],
    …
  },
  "degraded": []
}
`),
    sources: [
      { file: "src/cli/mod.rs", mark: "returns the original memory id with status=already_recorded" },
      { file: "src/cli/mod.rs", mark: '.field_str("memoryId", &memory_id)' },
      { file: "src/core/memory.rs", mark: '"completed" | "completed_no_documents" => "indexed".to_owned(),' },
      { file: "src/models/memory.rs", mark: 'Self::Procedural => "procedural",' },
      { file: "src/models/memory.rs", mark: 'Self::Rule => "rule",' },
      { file: "src/core/effect.rs", mark: '"memory_anchors"' },
    ],
  },
  {
    id: "outcome",
    name: "outcome",
    title: "Grade the memory",
    question: "Did that memory help, or mislead?",
    hub: ["Did that", "memory help?"],
    effect: "write",
    tag: "writes",
    chip: "writes · feedback",
    command: 'ee outcome <memory-id> --workspace . --signal helpful --reason "<what it changed>" --json',
    reads: ["data.status", "data.confidence", "data.quarantine"],
    returns: "Whether the feedback was recorded, and the memory's confidence just before and just after it.",
    because:
      "The agent sees what its signal moved without a follow-up call. Past five harmful reports an hour from one source, the next is quarantined instead of applied, so one confused agent cannot sink a memory; --event-id makes a retry safe.",
    json: excerpt(`
{
  "schema": "ee.response.v2",
  "data": {
    "command": "outcome",
>   "status": "recorded",
    "target": { "type": "memory", "id": "mem_…", "verified": true },
    "event": { "signal": "helpful", "weight": …, … },
>   "quarantine": null,
>   "confidence": { "before": …, "after": …, "delta": … },
    …
  },
  "degraded": []
}
`),
    sources: [
      { file: "src/core/outcome.rs", mark: "pub const DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR: u32 = 5;" },
      { file: "src/core/outcome.rs", mark: '"confidence": self.confidence_before' },
      { file: "src/core/outcome.rs", mark: 'Self::Recorded => "recorded",' },
      { file: "src/core/outcome.rs", mark: 'const HELPFUL_SIGNALS: &[&str] = &["positive", "confirmation", "helpful"];' },
      { file: "src/cli/mod.rs", mark: "Optional caller-supplied feedback event ID for idempotent retries." },
      { file: "src/core/effect.rs", mark: '"feedback_events"' },
    ],
  },
  {
    id: "handoff",
    name: "handoff",
    title: "Pass the baton",
    question: "What does the next session need?",
    hub: ["What does the next", "session need?"],
    effect: "write",
    tag: "writes",
    chip: "writes · one file",
    command: "ee handoff create --out .ee/handoffs/<capsule>.json --workspace . --json",
    note: "Flat and snake_case: this report has no data wrapper.",
    reads: ["capsule_id", "output_path", "content_hash", "canonical_content_hash"],
    returns: "A redacted, signed continuity capsule, written to the path you give, with its id and two content hashes.",
    because:
      "The next session, perhaps a different agent, loads it with ee handoff resume latest, which refuses an unsigned or tampered capsule unless told otherwise, and audits the override. canonical_content_hash leaves out ids and times, so two captures of the same state match.",
    json: excerpt(`
{
  "schema": "ee.handoff.create.v1",
> "capsule_id": "…",
> "output_path": "…/.ee/handoffs/<capsule>.json",
  "profile": "resume",
  "token_count": …,
> "content_hash": "…",
> "canonical_content_hash": "…",
  "redaction_summary": { … },
  "dry_run": false,
  …
}
`),
    sources: [
      { file: "src/output/mod.rs", mark: "pub fn render_handoff_create_json" },
      { file: "src/core/handoff.rs", mark: "fn find_latest_capsule" },
      { file: "src/core/handoff.rs", mark: 'pub const HANDOFF_CREATE_SCHEMA_V1: &str = "ee.handoff.create.v1";' },
      { file: "src/core/handoff.rs", mark: 'Self::Resume => "resume",' },
      { file: "src/cli/mod.rs", mark: "Load an unsigned or tampered capsule anyway and emit an audited" },
      { file: "src/core/effect.rs", mark: "Write a redacted continuity capsule to a user-specified output path" },
    ],
  },
];

export const STATION_COUNT = STATIONS.length;

/** How long the figure rests on a station before it moves on by itself. */
export const STEP_MS = 5200;

/** The last key of a JSON path: `data.pack.skipped[]` → `skipped`. */
export function lastKey(path: string): string {
  const parts = path.replace(/\[\]/g, "").split(".");
  return parts[parts.length - 1] ?? path;
}

/** The step, from `from` to `to` around a ring of `n`, the short way (a tie goes forward). */
export function shortestDelta(from: number, to: number, n = STATION_COUNT): number {
  const forward = (((to - from) % n) + n) % n;
  return forward > n / 2 ? forward - n : forward;
}

// ---- The ring ----

/** Degrees between stations, and station 0's angle (straight up). */
export const STEP_DEG = 360 / STATION_COUNT;
const START_DEG = -90;

const NODE_R = 17;
const NAME_PX = 7.3; // 12 px JetBrains Mono, per character
const TAG_PX = 6.4; // 8.5 px, letter-spaced, per character

export interface LabelBox {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface RingNode {
  x: number;
  y: number;
  angle: number;
  anchor: "start" | "middle" | "end";
  /** Baselines of the label's two lines. */
  nameX: number;
  nameY: number;
  tagY: number;
  box: LabelBox;
}

export interface RingGeometry {
  W: number;
  H: number;
  cx: number;
  cy: number;
  R: number;
  nodeR: number;
  nodes: RingNode[];
}

export const toRad = (deg: number) => (deg * Math.PI) / 180;

export function stationAngle(index: number): number {
  return START_DEG + index * STEP_DEG;
}

/** The ring at width `W`: nodes on a circle, each labelled outside it, the whole drawn one unit to the pixel. */
export function ringGeometry(width: number): RingGeometry {
  const W = Math.round(Math.max(420, width));
  const R = Math.round(Math.min(190, Math.max(118, W / 2 - 94)));
  const cx = W / 2;
  const cy = R + 58;
  const nodes = STATIONS.map((station, index) => {
    const angle = stationAngle(index);
    const cos = Math.cos(toRad(angle));
    const sin = Math.sin(toRad(angle));
    const x = cx + R * cos;
    const y = cy + R * sin;
    const nameW = station.name.length * NAME_PX;
    const tagW = station.tag.length * TAG_PX;
    const w = Math.max(nameW, tagW);
    let anchor: RingNode["anchor"];
    let nameX: number;
    let nameY: number;
    if (Math.abs(cos) < 0.36) {
      anchor = "middle";
      nameX = x;
      nameY = sin < 0 ? y - NODE_R - 26 : y + NODE_R + 18;
    } else {
      anchor = cos > 0 ? "start" : "end";
      nameX = x + (cos > 0 ? 1 : -1) * (NODE_R + 11);
      nameY = y - 1;
    }
    const tagY = nameY + 13;
    const left = anchor === "middle" ? nameX - w / 2 : anchor === "start" ? nameX : nameX - w;
    const box = { x: left, y: nameY - 10, w, h: 24 };
    return { x, y, angle, anchor, nameX, nameY, tagY, box };
  });
  const bottom = Math.max(...nodes.map((node) => node.box.y + node.box.h));
  return { W, H: Math.ceil(bottom + 8), cx, cy, R, nodeR: NODE_R, nodes };
}

/** An SVG arc along a circle, clockwise from `fromDeg` to `toDeg`. */
export function arcPath(cx: number, cy: number, r: number, fromDeg: number, toDeg: number): string {
  const sweep = (((toDeg - fromDeg) % 360) + 360) % 360;
  const a = toRad(fromDeg);
  const b = toRad(fromDeg + sweep);
  const x1 = cx + r * Math.cos(a);
  const y1 = cy + r * Math.sin(a);
  const x2 = cx + r * Math.cos(b);
  const y2 = cy + r * Math.sin(b);
  const large = sweep > 180 ? 1 : 0;
  return `M ${x1.toFixed(2)} ${y1.toFixed(2)} A ${r} ${r} 0 ${large} 1 ${x2.toFixed(2)} ${y2.toFixed(2)}`;
}

// ---- Tokenizers ----

export type CommandToken = { text: string; kind: "bin" | "sub" | "flag" | "str" | "hole" | "arg" };

/** A command split into words and coloured by role: ee, its subcommand, flags, quoted text, placeholders. */
export function commandTokens(command: string): CommandToken[] {
  const words = command.match(/"[^"]*"|\S+/g) ?? [];
  let inSub = true;
  return words.map((text, index) => {
    if (index === 0) return { text, kind: "bin" };
    if (text.startsWith("--")) {
      inSub = false;
      return { text, kind: "flag" };
    }
    if (text.startsWith('"')) {
      inSub = false;
      return { text, kind: text.includes("<") ? "hole" : "str" };
    }
    if (text.startsWith("<") || text.includes("<")) {
      inSub = false;
      return { text, kind: "hole" };
    }
    if (inSub && /^[a-z][a-z-]*$/.test(text)) return { text, kind: "sub" };
    return { text, kind: "arg" };
  });
}

export type JsonToken = { text: string; kind: "key" | "string" | "literal" | "elided" | "punct" | "space" | "other" };

const JSON_TOKEN = /("(?:[^"\\]|\\.)*")(\s*:)?|(-?\d+(?:\.\d+)?|\btrue\b|\bfalse\b|\bnull\b)|(…)|([{}[\],:])|(\s+)|([^\s"{}[\],:…]+)/g;

/** One line of an excerpt as coloured tokens. A string followed by a colon is a key. */
export function jsonTokens(line: string): JsonToken[] {
  const tokens: JsonToken[] = [];
  for (const match of line.matchAll(JSON_TOKEN)) {
    const [, string, colon, literal, elided, punct, space, other] = match;
    if (string !== undefined) {
      if (colon !== undefined) {
        tokens.push({ text: string, kind: "key" });
        tokens.push({ text: colon, kind: "punct" });
      } else {
        tokens.push({ text: string, kind: "string" });
      }
    } else if (literal !== undefined) tokens.push({ text: literal, kind: "literal" });
    else if (elided !== undefined) tokens.push({ text: elided, kind: "elided" });
    else if (punct !== undefined) tokens.push({ text: punct, kind: "punct" });
    else if (space !== undefined) tokens.push({ text: space, kind: "space" });
    else if (other !== undefined) tokens.push({ text: other, kind: "other" });
  }
  return tokens;
}

/** The keys on one line of an excerpt, without their quotes. */
export function lineKeys(line: string): string[] {
  return jsonTokens(line)
    .filter((token) => token.kind === "key")
    .map((token) => token.text.slice(1, -1));
}

/** The longest excerpt, in lines: every excerpt box is drawn this tall, so the figure does not jump. */
export const MAX_JSON_LINES = Math.max(...STATIONS.map((station) => station.json?.length ?? 0));
