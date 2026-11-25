// Every word and figure on the home page that is not inside an explainer.
// Figures are counted from the repository at the commit named in COUNTED, and
// each one says where it comes from; the explainers in components/explain/
// cite their own sources on their own faces.

const REPO = "https://github.com/Dicklesworthstone/eidetic_engine_cli";

export const siteConfig = {
  name: "Eidetic Engine",
  short: "ee",
  url: "https://eidetic-engine.org",
  title: "Eidetic Engine (ee): long-term memory for coding agents",
  description:
    "ee is a local memory for coding agents. It stores the rules, decisions, failures and fixes your agents learn, and gives each new session a context pack that fits a token budget, with every item's source and the reason it was chosen. It runs as one Rust binary with no cloud service.",
  github: REPO,
  readme: `${REPO}#readme`,
  releases: `${REPO}/releases/latest`,
  changelog: `${REPO}/blob/main/CHANGELOG.md`,
  agents: `${REPO}/blob/main/AGENTS.md`,
  adrs: `${REPO}/tree/main/docs/adr`,
  schemas: `${REPO}/tree/main/docs/schemas`,
  trustModel: `${REPO}/blob/main/docs/trust-model.md`,
  license: `${REPO}/blob/main/LICENSE`,
  crate: "https://crates.io/crates/eidetic-engine",
  version: "v0.19.0",
  released: "2026-11-21",
} as const;

/** The commit and date every count on the page was taken at. */
export const COUNTED = { commit: "c05913f2a", date: "2026-11-24" } as const;

export type Section = { id: string; label: string };

export type Chapter = {
  id: string;
  /** Its own page, /<slug> (no trailing slash), which carries the chapter's own share images. */
  slug: string;
  number: string;
  /** The nav's name for it. */
  label: string;
  /** The phone dock's one word. */
  short: string;
  title: string;
  thesis: string;
  colour: string;
  /** The chapter page's title and description, and its share card's words. */
  share: { title: string; description: string; kicker: string; headline: string; accent: string; line: string };
  /** What a reader gets from this chapter's part of ee, for the summary at the top of the home page. */
  point: { claim: string; detail: string };
  sections: Section[];
};

export const chapters: Chapter[] = [
  {
    id: "forget",
    slug: "amnesia",
    number: "01",
    label: "Amnesia",
    short: "Why",
    title: "Every session starts [from zero].",
    thesis:
      "A new agent session re-learns the project's conventions, re-reads files an earlier session already read, and repeats failures an earlier session already hit, because nothing those sessions learned was kept where it could be found. The usual fix is a notes file loaded into every session, and each lesson added to it costs tokens in every later session, whether or not the task needs it.",
    colour: "#fb7185",
    share: {
      title: "Why agents forget, and why a notes file doesn't fix it",
      description:
        "Every agent session starts with an empty context. A CLAUDE.md or AGENTS.md file helps at first, then grows, collects stale lines and costs tokens in every session. ee gives each session a ranked pack that fits a token budget and explains every item.",
      kicker: "Chapter 01 · Amnesia",
      headline: "Every session starts",
      accent: "from zero.",
      line: "A notes file is loaded whole and grows with every lesson. ee gives each session only what bears on its task, within a token budget.",
    },
    point: {
      claim: "Each session gets what its task needs",
      detail:
        "A notes file is loaded whole into every session and grows with every lesson. ee keeps every lesson in its store and packs only the ones that bear on the current task.",
    },
    sections: [
      { id: "amnesia", label: "The cost of forgetting" },
      { id: "scale", label: "Notes file vs ee" },
      { id: "layer", label: "Where ee sits" },
      { id: "first", label: "Five commands" },
    ],
  },
  {
    id: "memory",
    slug: "memory",
    number: "02",
    label: "Memory",
    short: "Memory",
    title: "Typed memories with [half-lives].",
    thesis:
      "A scratch note is useful for a day and a procedural rule for a year. ee stores typed memories at four levels, each with its own half-life. When the decay pass runs (ee maintenance run --include-decay), a stale memory is demoted, then tombstoned: it leaves retrieval, an audit entry records the change, and the row is kept.",
    colour: "#fbbf24",
    share: {
      title: "Typed memories with half-lives",
      description:
        "ee stores typed memories at four levels, each with its own half-life. Its decay pass demotes a stale memory, then tombstones it with an audit entry, and keeps the row.",
      kicker: "Chapter 02 · Memory",
      headline: "Typed memories",
      accent: "with half-lives.",
      line: "Working notes have a half-life of a day and procedural rules a year. Every demotion and tombstone writes an audit entry.",
    },
    point: {
      claim: "Stale material fades on a schedule",
      detail:
        "Working notes have a half-life of a day, procedural rules a year. When the decay pass runs, a memory that stopped being useful is demoted, then dropped from retrieval with an audit entry.",
    },
    sections: [
      { id: "decay", label: "Half-lives" },
      { id: "kinds", label: "Typed memories" },
    ],
  },
  {
    id: "pack",
    slug: "pack",
    number: "03",
    label: "The pack",
    short: "Pack",
    title: "How ee builds [a pack].",
    thesis:
      "The pack is the text an agent reads at the start of a task. A fixed pipeline selects memories for the task, fits them to a token budget, drops near-duplicates, and hashes the result so the same inputs give the same pack. Every item lists its source and its score.",
    colour: "#a78bfa",
    share: {
      title: "How ee builds a context pack",
      description:
        "A fixed pipeline of timed stages selects memories for the task, fits them to a token budget, removes near-duplicates with MMR and hashes the result, so the same inputs give the same pack. Build one in your browser.",
      kicker: "Chapter 03 · The pack",
      headline: "How ee builds",
      accent: "a pack.",
      line: "A token budget, section quotas, near-duplicates removed, and a hash that reproduces. Every item lists its source.",
    },
    point: {
      claim: "Context fits a budget you set",
      detail:
        "The pack is cut to a token budget (4,000 by default) with near-duplicates removed, and each item says where it came from and why it was picked. The same inputs give the same pack hash.",
    },
    sections: [
      { id: "pipeline", label: "The pipeline" },
      { id: "builder", label: "Build a pack" },
      { id: "explain", label: "Six questions" },
      { id: "hash", label: "Same inputs, same hash" },
    ],
  },
  {
    id: "learn",
    slug: "learning",
    number: "04",
    label: "Learning",
    short: "Learn",
    title: "It learns from [outcomes].",
    thesis:
      "Agents report whether a memory helped or misled them. ee keeps a Bayesian posterior for every memory, promotes it only when the evidence is clear, demotes it faster than it promotes, and does not let an agent's report stand in for a person's.",
    colour: "#38bdf8",
    share: {
      title: "It learns from outcomes",
      description:
        "ee keeps a Bayesian posterior for every memory. A harmful outcome counts two and a half times as much as a helpful one, fixes are recalled by the error's fingerprint, and every change to memory writes an audit entry.",
      kicker: "Chapter 04 · Learning",
      headline: "It learns from",
      accent: "outcomes.",
      line: "Outcomes update a posterior for each memory. A harmful report weighs 2.5 times a helpful one, and agent feedback cannot raise a memory to human trust.",
    },
    point: {
      claim: "Feedback changes what gets recalled",
      detail:
        "Agents report whether a memory helped. A harmful report counts two and a half times a helpful one, and no amount of agent feedback gives a memory the trust of one a person entered.",
    },
    sections: [
      { id: "trust", label: "Trust" },
      { id: "fixes", label: "Fix recall" },
      { id: "curate", label: "Curation" },
      { id: "guard", label: "Safety" },
    ],
  },
  {
    id: "campaigns",
    slug: "campaigns",
    number: "05",
    label: "Campaigns",
    short: "Scale",
    title: "For work that runs for [weeks].",
    thesis:
      "A lot of real work takes more than one session: a performance hunt that runs for a month, an incident that keeps recurring, a research question you return to every week. ee calls these campaigns. Each attempt is recorded as evidence, a repeated family of failures can be proposed as a rule, the decay pass retires unused material, and the pack each session reads stays within the same budget as the store grows.",
    colour: "#fb923c",
    share: {
      title: "For work that runs for weeks",
      description:
        "ee records every failed attempt with its cause, proposes rules from repeated failures, and keeps each session's pack within the same budget as the store grows. The same model works outside code, in research, security and legal work.",
      kicker: "Chapter 05 · Campaigns",
      headline: "Work that runs",
      accent: "for weeks.",
      line: "Each failed attempt is recorded with its cause, a family of failures can become a rule, and week 26's pack is no larger than week 1's.",
    },
    point: {
      claim: "Failed attempts are kept with their cause",
      detail:
        "On work that runs for weeks, each failure is stored with its cause and family, and repeated failures in one family can become a rule that steers later sessions away from it.",
    },
    sections: [
      { id: "campaign", label: "Six months of one" },
      { id: "ledger", label: "Failed attempts" },
      { id: "domains", label: "Beyond code" },
      { id: "team", label: "One agent to a team" },
    ],
  },
  {
    id: "agents",
    slug: "agents",
    number: "06",
    label: "For agents",
    short: "Agents",
    title: "Output an agent can [act on].",
    thesis:
      "Most of ee's callers are agents, so its interface is designed for them. With --json, every command answers in one versioned JSON envelope with a typed exit code. Anything that ran in a reduced mode is listed under degraded with a code and, where ee knows one, a repair. Errors carry a ranked recovery plan when ee has one, and the documentation is a set of commands that return JSON.",
    colour: "#a3e635",
    share: {
      title: "Output an agent can act on",
      description:
        "One versioned JSON envelope, typed exit codes, degraded states listed by code, ranked recovery steps where ee knows them, and 32 agent recipes with jq selectors.",
      kicker: "Chapter 06 · For agents",
      headline: "Output an agent",
      accent: "can act on.",
      line: "With --json, every response is versioned JSON with a typed exit code. Where ee knows the fix, an error names the command, its risk class and whether a person must approve it.",
    },
    point: {
      claim: "Responses are versioned JSON",
      detail:
        "With --json, each command answers in versioned JSON with a typed exit code. Where ee knows the fix, an error names the command, how risky it is, and whether a person has to approve it.",
    },
    sections: [
      { id: "session", label: "A session, end to end" },
      { id: "recovery", label: "Errors that say what to do" },
      { id: "discover", label: "Self-describing" },
    ],
  },
  {
    id: "graph",
    slug: "graph",
    number: "07",
    label: "Graph",
    short: "Graph",
    title: "Memories form a [graph].",
    thesis:
      "A rule supports a decision, a failure derives from a session, one fact contradicts another. ee stores those links and runs graph algorithms over them to find the memories many others depend on, and the ones that are a topic's only connection to the rest, where one wrong memory cuts the topic off.",
    colour: "#34d399",
    share: {
      title: "Memories form a graph",
      description:
        "ee links memories and runs graph algorithms over the links: PageRank, HITS, articulation points and Louvain communities, each a CLI command with deterministic JSON, and Gomory-Hu proximity behind a feature flag.",
      kicker: "Chapter 07 · Graph",
      headline: "Memories form a",
      accent: "graph.",
      line: "Find the rule twelve decisions rest on, the one fact that connects two topics, and the memory that cuts a topic off if it is wrong.",
    },
    point: {
      claim: "Links show what depends on what",
      detail:
        "Rules, decisions, failures and sessions are linked, and graph commands find the memories many others rest on and the ones that are the only bridge between two topics.",
    },
    sections: [
      { id: "lenses", label: "Five lenses" },
      { id: "toolbox", label: "The toolbox" },
    ],
  },
  {
    id: "proof",
    slug: "proof",
    number: "08",
    label: "Proof",
    short: "Proof",
    title: "Evidence, limits and [install].",
    thesis:
      "A wrong memory misleads every session that reads it, so ee is held to strict rules: a check that refuses banned dependencies, 630 failure-mode fixtures, 280 versioned JSON schemas, and a published list of what does not work yet. The chapter ends with the install commands.",
    colour: "#e879f9",
    share: {
      title: "Evidence, limits and install",
      description:
        "ee compared with the alternatives, what still works when a part is missing, the dependencies its checks refuse, the current limitations, and the install commands.",
      kicker: "Chapter 08 · Proof",
      headline: "Evidence, limits",
      accent: "and install.",
      line: "20,700+ tests, 630 failure-mode fixtures, 280 JSON schemas, 91 decision records, and a list of what isn't done.",
    },
    point: {
      claim: "It runs locally and lists its limits",
      detail:
        "One binary and no cloud service. When the embedding model, the index or another part is missing, commands still answer and report what they lost, and the README lists what is not finished.",
    },
    sections: [
      { id: "compare", label: "Compared" },
      { id: "degrade", label: "Missing parts" },
      { id: "rigor", label: "Dependencies" },
      { id: "status", label: "Limitations" },
      { id: "install", label: "Install" },
    ],
  },
];

/** The chapter with this id; a typo is a build failure, not a missing section. */
export function chapterById(id: string): Chapter {
  const found = chapters.find((item) => item.id === id);
  if (!found) throw new Error(`no chapter ${id}`);
  return found;
}

/** A section's number within its chapter: "03.2". */
export function sectionIndex(chapterId: string, sectionId: string): string {
  const owner = chapterById(chapterId);
  const at = owner.sections.findIndex((section) => section.id === sectionId);
  if (at < 0) throw new Error(`chapter ${chapterId} has no section ${sectionId}`);
  return `${owner.number}.${at + 1}`;
}

export const heroStats = [
  {
    label: "Tests",
    value: "20,700+",
    note: "#[test] functions across src/ and tests/, with 46 property suites and 27 fuzz targets.",
  },
  {
    label: "Failure modes",
    value: "630",
    note: "Each fixture triggers a degraded state and records the code it reports, and the repair where there is one.",
  },
  {
    label: "JSON contracts",
    value: "280",
    note: "Versioned schemas for the JSON that ee's commands return.",
  },
  {
    label: "Cloud APIs needed",
    value: "0",
    note: "Embeddings and storage are local. The network is needed once, to download the model.",
  },
] as const;

/** The first five commands: a memory, used. README "Quick Start". */
export const firstCommands = [
  {
    step: "Open a workspace",
    command: "ee init --workspace .",
    note: "Idempotent. The store is FrankenSQLite under .ee/; indexes beside it are derived and rebuildable.",
  },
  {
    step: "Teach it a rule",
    command: 'ee remember "Run cargo fmt --check before tagging a release." \\\n  --workspace . --level procedural --kind rule --json',
    note: "Text that looks like a secret is refused with exit 7, and the error says where it matched. A memory written this way starts in the human_explicit trust class.",
  },
  {
    step: "Hand the next session a pack",
    command: 'ee pack "prepare the release" --workspace . \\\n  --max-tokens 4000 --format markdown',
    note: "Rules, decisions, failures and evidence that match the task, within the budget, each with provenance and a score.",
  },
  {
    step: "Say what helped",
    command: 'ee outcome <memory-id> --workspace . --signal helpful \\\n  --reason "caught the unformatted file before the tag"',
    note: "Feedback moves the memory's posterior. Harmful counts 2.5 times as much as helpful.",
  },
  {
    step: "Ask why",
    command: "ee why <memory-id> --workspace . --json",
    note: "Where a memory came from, what supports it, how fresh and how trusted it is, and what would change its rank.",
  },
] as const;

/** Registry-backed memory kinds and their typed fields. README "Memory Model". */
export const memoryKinds = [
  { kind: "rule", level: "procedural", fields: ["condition", "action", "exceptions"], example: "condition: before tagging · action: cargo fmt --check" },
  { kind: "anti-pattern", level: "procedural", fields: ["trigger", "blast_radius", "safer_alternative"], example: "trigger: git rebase on main · safer: merge" },
  { kind: "decision", level: "semantic", fields: ["options", "chosen", "rationale", "supersedes", "revisit_by"], example: "chosen: Asupersync · rationale: no Tokio" },
  { kind: "convention", level: "semantic", fields: ["scope", "pattern"], example: "scope: tests/*.rs · pattern: allow header after //!" },
  { kind: "command", level: "procedural", fields: ["command", "when_to_use", "exit_meaning"], example: "exit_meaning: 124 means the command timed out" },
  { kind: "failure", level: "episodic", fields: ["cause", "regression_surface", "reverted_at_sha", "family"], example: "family: rustc E0277 · cause: missing bound" },
] as const;

/** The four levels. README "Memory Model". */
export const memoryLevels = [
  { level: "working", examples: "Task notes, scratch, in-progress facts", decay: "fastest", priority: "low; suppressed across sessions" },
  { level: "episodic", examples: "\u201cOn 2026-03-12 the release failed because\u2026\u201d", decay: "medium", priority: "medium" },
  { level: "semantic", examples: "Project conventions, architectural facts", decay: "slow", priority: "high" },
  { level: "procedural", examples: "Rules, anti-patterns, playbooks", decay: "slowest", priority: "highest" },
] as const;

/** README "Explainable Retrieval": the six questions. */
export const sixQuestions = [
  { q: "Why selected?", a: "Score components for each stage of the pipeline." },
  { q: "What supports it?", a: "Provenance URIs back to the session, file or command." },
  { q: "How fresh?", a: "The recency term: a 30-day time constant, floored at 0.5, the same for every level." },
  { q: "How reliable?", a: "Confidence, evidence count, and the weight of harmful feedback." },
  { q: "What scores mattered?", a: "The final score, its kind, a normalized relevance, and the raw breakdown." },
  { q: "What would change it?", a: "A counterfactual hint, when there is one." },
] as const;

/** README "Comparison". "yes" / "no" / a word. */
export const comparison = {
  columns: ["ee", "Vector DB (Chroma, Qdrant)", "MCP memory server", "CLAUDE.md / notes"],
  rows: [
    { feature: "Local-first by default", cells: ["yes", "varies", "varies", "yes"] },
    { feature: "Hybrid lexical + semantic retrieval", cells: ["yes", "varies", "partial", "no"] },
    { feature: "Provenance for every item", cells: ["yes", "no", "partial", "manual"] },
    { feature: "Procedural rules that decay", cells: ["yes", "no", "no", "no"] },
    { feature: "Anti-patterns and harmful feedback", cells: ["yes", "no", "no", "manual"] },
    { feature: "Explainable scores", cells: ["yes", "no", "partial", "n/a"] },
    { feature: "Graph analytics over memories", cells: ["yes", "no", "no", "no"] },
    { feature: "Deterministic JSON output", cells: ["yes", "varies", "varies", "n/a"] },
    { feature: "Imports past agent sessions", cells: ["yes", "manual ETL", "no", "manual"] },
    { feature: "Works with no daemon running", cells: ["yes", "varies", "no", "yes"] },
    { feature: "Single-binary install", cells: ["yes", "varies", "no", "n/a"] },
    { feature: "Audit log of curation", cells: ["yes", "no", "no", "git only"] },
  ],
} as const;

/** README "Hard Requirements" and CLAUDE.md "Forbidden dependencies": each is a CI failure. */
export const forbidden = [
  { no: "Tokio, async-std, smol", instead: "Asupersync", why: "Structured concurrency, cancellation as a value, deterministic lab tests." },
  { no: "rusqlite, SQLx, Diesel", instead: "FrankenSQLite + SQLModel", why: "One Rust storage stack with MVCC, owned end to end." },
  { no: "petgraph", instead: "FrankenNetworkX", why: "Graph algorithms with NetworkX semantics, in Rust." },
  { no: "hyper, axum, reqwest", instead: "No HTTP framework", why: "Memory needs no network. The optional ee serve is a loopback server on Rust's standard library." },
] as const;

/** Counts behind the "rules" section, taken at COUNTED. */
export const rigor = [
  { value: "1.44M", label: "lines of Rust in src/", note: "771 files; another 443k lines of tests." },
  { value: "91", label: "architecture decisions", note: "Numbered ADRs in docs/adr, each with a status." },
  { value: "133", label: "forward-only migrations", note: "No down-migrations: back up, then move forward." },
  { value: "14,486", label: "commits", note: "Since the first one, 2026-04-29." },
] as const;

/** Releases, from CHANGELOG.md and the tags' dates. */
export const releases = [
  { version: "0.1", date: "2026-05-15", what: "First release: remember, search, pack." },
  { version: "0.13", date: "2026-07-30", what: "A user-global memory lane beside each workspace." },
  { version: "0.14", date: "2026-08-18", what: "Team confederation: signed, hash-chained sharing between stores." },
  { version: "0.15", date: "2026-09-12", what: "Published on crates.io." },
  { version: "0.16", date: "2026-09-27", what: "ee ask answers from procedural rules and imported session evidence." },
  { version: "0.17", date: "2026-10-02", what: "Retrieval quality ranking, and a Pareto-frontier knowledge skyline." },
  { version: "0.18", date: "2026-10-14", what: "Failure-to-fix recall from imported sessions." },
  { version: "0.19", date: "2026-11-21", what: "Keyed CASS commitments, warm daemon packs, Go/Rust/Vitest/Jest error recall, cargo audit 12 → 0." },
] as const;

/** README "Limitations" and the ADRs: what is not done, said plainly. */
export const limitations = [
  {
    title: "Personalized PageRank is unavailable",
    text: "The pinned graph library has no deterministic personalized PageRank yet, so that ranking signal and Pack DNA's PPR neighbors are unavailable; both report graph_ppr_upstream_unavailable and fall back. PageRank, HITS, Louvain and articulation points are unaffected.",
  },
  {
    title: "Most graph views are opt-in",
    text: "Every graph.feature.* flag defaults to off, so ee proximity, ee why --causal-explain, ee health --robot-insights, the knowledge skyline and Pack DNA report graph_feature_disabled until you run ee config set graph.feature.<name>.enabled true. PageRank, HITS, Louvain and articulation points need no flag.",
  },
  {
    title: "Determinism is per platform, and not yet gated across hosts",
    text: "The pack hash binds the target architecture, OS and enabled features, so the same inputs give the same hash on the same platform. Tie-breaks still compare raw f32, and no gate yet checks determinism across processes or hosts (ADR 0087).",
  },
  {
    title: "Heavy write swarms need a write owner",
    text: "Many agents can read at once. FrankenSQLite is single-process MVCC, so heavy concurrent writing goes through job locks or the optional daemon.",
  },
  {
    title: "Releases are signed but carry no SLSA provenance",
    text: "Current releases ship SHA-256 sums and minisign signatures. There are no Sigstore bundles or SLSA provenance, so --require-provenance fails by design.",
  },
  {
    title: "Some performance gates are unproven",
    text: "Since v0.16.0, a read-only pack through a warm daemon measured 708 ms against 3,155 ms for the same pack built locally, on one worker; the 10,000-memory, 2x median gate is still unverified.",
  },
] as const;

export const install = [
  {
    comment: "Release installer: checks SHA-256, installs to ~/.local/bin, runs a self-test",
    command: `curl -fsSL ${REPO}/releases/download/v0.19.0/install.sh -o install.sh\nEE_VERSION=v0.19.0 bash install.sh --easy-mode --verify`,
  },
  { comment: "Homebrew (macOS, Linux)", command: "brew install Dicklesworthstone/tap/ee" },
  { comment: "From crates.io (nightly Rust)", command: "cargo +nightly install eidetic-engine --version 0.19.0 --locked" },
] as const;

/** README "Agent Harness Integration". */
export const harnesses = [
  { name: "Claude Code", command: "ee hook claude-code --install --workspace . --json", note: "Four managed hooks: orientation at session start, recall before an edit, a journal entry when a Bash command fails, and capture suggestions at the end. They fail open and never deny a command." },
  { name: "Codex", command: "ee hook codex --install --workspace . --json", note: "Codex shells out, so every ee command works as is; ee pack --json drops straight into a message." },
  { name: "MCP", command: "ee mcp manifest --json", note: "The stdio adapter needs a build with --features mcp; without it the manifest reports the adapter as disabled. Its tools, such as ee_context, ee_search and ee_remember, follow the CLI's contracts." },
  { name: "Anything with a shell", command: 'ee pack "<task>" --workspace . --format markdown', note: "Scripts and people get the same commands and the same JSON as the agents." },
] as const;

/** The FrankenSuite crates ee is built on. */
export const stack = [
  { name: "Asupersync", role: "runtime", text: "Structured concurrency with cancellation as a value." },
  { name: "FrankenSQLite", role: "store", text: "The source of truth, with MVCC, through SQLModel." },
  { name: "Frankensearch", role: "search", text: "BM25 and Model2Vec vectors, fused by reciprocal rank." },
  { name: "FrankenNetworkX", role: "graph", text: "PageRank, HITS, communities, cores, cuts." },
  { name: "cass", role: "evidence", text: "Search over your past Claude Code, Codex, Cursor and Gemini sessions." },
] as const;

/** README "Curation" and "No Silent Memory Mutation". */
export const curationSteps = [
  { verb: "Propose", text: "Candidates come from journals, imported sessions, consolidation and the steward. Nothing is applied yet.", command: "ee curate candidates --workspace . --json" },
  { verb: "Validate", text: "Each candidate is checked against policy, duplicates and contradictions before anyone can apply it.", command: "ee curate validate <candidate-id> --workspace . --json" },
  { verb: "Apply", text: "Applying writes the change and an audit entry together. Promotions, merges and tombstones are all logged.", command: "ee curate apply <candidate-id> --workspace . --json" },
] as const;

export const guards = [
  {
    title: "ee remember refuses secrets",
    text: "Text that looks like an API key, JWT, password, private or SSH key, AWS secret or OAuth token is refused with exit 7, and the error says where it matched. --allow-secret-mention stores it anyway and writes an audit entry. Commands that export take a redaction level from none to paranoid.",
  },
  {
    title: "Stored instructions are screened",
    text: "Before packing, stored memories are screened for role overrides, hidden-prompt requests, credential requests and authority claims. A match is omitted with an explanation and kept for inspection.",
  },
  {
    title: "Preflight only advises",
    text: "ee preflight looks up the risk history of a command before you run it. It reports what it finds, and the risk it finds does not change its exit code; it does not block the command, and no ee hook denies one.",
  },
  {
    title: "Imported evidence is committed",
    text: "Session lines imported since v0.19.0 carry keyed BLAKE3 commitments under the store's root key, so evidence can be checked without keeping raw digests of it.",
  },
] as const;

/**
 * README "Graph" and "Graph-derived insights": what each tool answers, the command, and the config flag it needs,
 * if any. Every graph.feature.* flag defaults to off (src/config/merge.rs), and until it is on, its tool reports
 * graph_feature_disabled.
 */
export const graphTools: readonly { name: string; answers: string; command: string; flag: string | null }[] = [
  { name: "PageRank", answers: "Which memories does everything else lean on?", command: "ee graph pagerank --limit 10", flag: null },
  { name: "HITS", answers: "Which memories are hubs that point at good ones, and which are the authorities?", command: "ee graph hits --limit 10", flag: null },
  { name: "Articulation points", answers: "Which single memory, if wrong, cuts a topic off from the rest?", command: "ee graph articulation", flag: null },
  { name: "Louvain communities", answers: "Which memories form a topic, whatever their tags say?", command: "ee graph louvain", flag: null },
  { name: "k-core", answers: "Where is the knowledge dense, and where is it thin?", command: "ee graph k-core --json", flag: null },
  { name: "Link prediction", answers: "Which links are missing? Proposed, never written silently.", command: "ee graph suggest-links --propose", flag: null },
  { name: "Gomory-Hu proximity", answers: "How tightly are two memories bound, and through what?", command: "ee proximity <memory-a> <memory-b> --json", flag: "graph.feature.proximity.enabled" },
  { name: "k-truss and contradiction clusters", answers: "Which groups of memories hold together, and where do contradictions cluster?", command: "ee health --robot-insights --json", flag: "graph.feature.structural_health.enabled" },
  { name: "Causal paths", answers: "What chain of events led to this failure?", command: "ee why <memory-id> --causal-explain --json", flag: "graph.feature.causal_explain.enabled" },
  { name: "Pack DNA", answers: "Why does this pack look the way it does: dominators, communities, ego graphs.", command: 'ee pack "<task>" --explain --json', flag: "graph.feature.pack_dna.enabled" },
  { name: "Knowledge skyline", answers: "The store's posture across layers, communities, trust, age and support.", command: "ee insights --section knowledgeSkyline --json", flag: "graph.feature.skyline.enabled" },
];

/**
 * README "Beyond Coding": the same memory model outside software. The source URIs are the
 * README's own examples of what to pass to `ee remember --source`; ee does not fetch them.
 */
export const domains = [
  { name: "Investment research", unit: "a position", memories: "Thesis revisions, valuation methods, failed screens, peer sets", uris: ["sec-filing://", "earnings-call://", "analyst-note://"], workspace: "./positions/AAPL-long" },
  { name: "Legal work", unit: "a matter", memories: "Case rules, drafting conventions, negotiation outcomes, due-diligence steps", uris: ["case://", "pacer://", "westlaw://"], workspace: "./matters/smith-v-jones" },
  { name: "Security and incident response", unit: "an incident", memories: "IOCs, TTPs, response playbooks, detection-rule outcomes", uris: ["cve://", "mitre://", "incident://"], workspace: "./incidents/2026-11-phish" },
  { name: "Marketing analysis", unit: "a program", memories: "Campaign retrospectives, A/B results, channel rules, segmentation methods", uris: ["ga4://", "mixpanel://", "campaign://"], workspace: "./growth/q4-launch" },
  { name: "Product management", unit: "a product line", memories: "User research, launch retrospectives, personas, prioritization rules", uris: ["interview://", "linear://", "notion://"], workspace: "./product/onboarding" },
  { name: "Medicine or clinical operations", unit: "a protocol", memories: "Guideline facts, near misses, differential-diagnosis procedures", uris: ["pubmed://", "guideline://", "emr://"], workspace: "./clinic/sepsis-pathway" },
  { name: "Sales and account work", unit: "an account", memories: "Call notes, objection patterns, account maps, qualification playbooks", uris: ["crm://", "salesforce://", "gong://"], workspace: "./deals/2026-q3-acme" },
] as const;

/**
 * The surfaces an agent uses to learn ee without a human: the `ee --help` prelude
 * (src/cli/mod.rs), AgentDocsTopic and AGENT_DOC_RECIPES (src/core/agent_docs.rs),
 * SchemaCommand, and the Introspect command.
 */
export const agentSurfaces = [
  { command: "ee --agent-docs", answers: "A machine-readable overview of ee and the index of every documentation topic." },
  { command: "ee agent-docs recipes", answers: "32 workflows, each with its command, a jq selector for the answer, a success check and its failure branches." },
  { command: "ee agent-docs <topic>", answers: "Twelve focused topics: guide, commands, contracts, schemas, paths, env, exit-codes, fields, errors, formats, examples, recipes." },
  { command: "ee --help-json", answers: "The whole command tree as JSON, so a harness can build its own tool list." },
  { command: "ee schema list", answers: "Every public JSON Schema by id; ee schema export <id> prints one to validate against." },
  { command: "ee introspect", answers: "ee's command, schema and error maps in one response." },
  { command: "ee capabilities", answers: "What this build can do: feature flags, the binaries it found, the environment overrides in force." },
] as const;

/** ProcessExitCode in src/models/mod.rs: each failure class has its own code. */
export const exitCodes = [
  { code: 0, name: "success", act: "read data" },
  { code: 1, name: "usage", act: "fix the call" },
  { code: 2, name: "configuration", act: "fix the config" },
  { code: 3, name: "storage", act: "follow repair" },
  { code: 4, name: "search index", act: "rebuild the index" },
  { code: 5, name: "import", act: "check the source" },
  { code: 6, name: "degraded, required mode unmet", act: "relax or repair" },
  { code: 7, name: "policy denied", act: "ask a human" },
  { code: 8, name: "migration required", act: "ee migrate run" },
  { code: 9, name: "eval found regressions", act: "read the report" },
  { code: 10, name: "workspace store missing", act: "check the path; don't init blindly" },
  { code: 130, name: "cancelled", act: "retry with more budget" },
] as const;

/** The five states of `posture` in ee status and ee doctor (src/models/posture.rs), mildest first. */
export const postures = ["ok", "initializing", "degraded_recoverable", "degraded_required", "blocked"] as const;
