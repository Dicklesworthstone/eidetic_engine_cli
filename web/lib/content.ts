// Every word and figure on the home page that is not inside an explainer.
// Figures are counted from the repository at the commit named in COUNTED, and
// each one says where it comes from; the explainers in components/explain/
// cite their own sources on their own faces.

const REPO = "https://github.com/Dicklesworthstone/eidetic_engine_cli";

export const siteConfig = {
  name: "Eidetic Engine",
  short: "ee",
  url: "https://eidetic-engine.org",
  title: "Eidetic Engine (ee): durable, local-first, explainable memory for coding agents",
  description:
    "ee gives coding agents a durable memory. It stores the rules, decisions, failures and evidence your agents learn, and hands every new session a compact context pack in which each line says where it came from and why it was chosen. Local-first Rust, one binary, no cloud.",
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
  number: string;
  /** The nav's name for it. */
  label: string;
  /** The phone dock's one word. */
  short: string;
  title: string;
  thesis: string;
  colour: string;
  sections: Section[];
};

export const chapters: Chapter[] = [
  {
    id: "forget",
    number: "01",
    label: "Amnesia",
    short: "Why",
    title: "Every session starts [from zero].",
    thesis:
      "A fresh agent re-learns the project's conventions, re-reads the files it read yesterday, and walks into the trap another session already hit. Not because it is careless: because nothing it learned had anywhere durable to go.",
    colour: "#fb7185",
    sections: [
      { id: "amnesia", label: "The cost of forgetting" },
      { id: "layer", label: "Push in, pull out" },
      { id: "first", label: "Five commands" },
    ],
  },
  {
    id: "memory",
    number: "02",
    label: "Memory",
    short: "Memory",
    title: "Memory that ages like [knowledge].",
    thesis:
      "A scratch note should fade by tomorrow; a procedural rule should last a year. ee stores typed memories at four levels, each with its own half-life, and forgets on purpose: stale memories are demoted, then tombstoned with an audit entry, never silently deleted.",
    colour: "#fbbf24",
    sections: [
      { id: "decay", label: "Half-lives" },
      { id: "kinds", label: "Typed memories" },
    ],
  },
  {
    id: "pack",
    number: "03",
    label: "The pack",
    short: "Pack",
    title: "Context with [receipts].",
    thesis:
      "The pack is what your agent actually reads: a token-budgeted selection of memories, assembled by a fixed pipeline, diversified so near-duplicates do not crowd each other out, and hashed so the same inputs give the same pack. Every line carries its provenance and its score.",
    colour: "#a78bfa",
    sections: [
      { id: "pipeline", label: "The pipeline" },
      { id: "builder", label: "Build a pack" },
      { id: "explain", label: "Six questions" },
      { id: "hash", label: "Same inputs, same hash" },
    ],
  },
  {
    id: "learn",
    number: "04",
    label: "Learning",
    short: "Learn",
    title: "It learns from [outcomes].",
    thesis:
      "When a memory helps, say so; when it misleads, say so louder. ee keeps a Bayesian posterior for every memory, promotes it only on evidence, demotes it faster than it promotes, and never lets an agent's say-so stand in for a human's.",
    colour: "#38bdf8",
    sections: [
      { id: "trust", label: "Earned trust" },
      { id: "fixes", label: "Fixes, recalled" },
      { id: "curate", label: "No silent edits" },
      { id: "guard", label: "Poison-resistant" },
    ],
  },
  {
    id: "graph",
    number: "05",
    label: "Graph",
    short: "Graph",
    title: "Memories form a [graph].",
    thesis:
      "Rules support decisions, failures derive from sessions, one fact contradicts another. ee links them and runs real graph algorithms over the links: which memories everything leans on, which ones bridge two topics, which ones fall apart if one is wrong.",
    colour: "#34d399",
    sections: [
      { id: "lenses", label: "Five lenses" },
      { id: "toolbox", label: "The toolbox" },
    ],
  },
  {
    id: "proof",
    number: "06",
    label: "Proof",
    short: "Proof",
    title: "Built to be [trusted].",
    thesis:
      "Memory is infrastructure: if it lies, every session after it inherits the lie. So ee is built the slow way: forbidden shortcuts, a documented failure mode for every degraded state, versioned contracts for every command, and a public list of what is not done yet.",
    colour: "#e879f9",
    sections: [
      { id: "compare", label: "Compared" },
      { id: "degrade", label: "Pull a plug" },
      { id: "rigor", label: "The rules" },
      { id: "status", label: "Honest status" },
      { id: "install", label: "Install" },
    ],
  },
];

export const heroStats = [
  {
    label: "Tests",
    value: "20,700+",
    note: "#[test] functions across src/ and tests/, with 46 property suites and 27 fuzz targets.",
  },
  {
    label: "Failure modes",
    value: "630",
    note: "Each degraded state has a fixture that triggers it and the repair it names.",
  },
  {
    label: "JSON contracts",
    value: "280",
    note: "Versioned schemas: every machine-facing command answers in stable JSON.",
  },
  {
    label: "Cloud APIs needed",
    value: "0",
    note: "Local embeddings, a local store, one binary. The network is optional.",
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
    note: "Secrets are redacted before anything is written. The rule starts at the trust class its source earns.",
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
  { kind: "command", level: "procedural", fields: ["command", "when_to_use", "exit_meaning"], example: "exit_meaning: 124 is a timeout, not a red" },
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
  { q: "How fresh?", a: "The recency term, from its level's half-life." },
  { q: "How reliable?", a: "Confidence, evidence count, and the weight of harmful feedback." },
  { q: "What scores mattered?", a: "The final score, its kind, a normalized relevance, and the raw breakdown." },
  { q: "What would change it?", a: "A counterfactual hint, when there is one." },
] as const;

/** README "Comparison". "yes" / "no" / a word. */
export const comparison = {
  columns: ["ee", "Vector DB", "MCP memory server", "CLAUDE.md / notes"],
  rows: [
    { feature: "Local-first by default", cells: ["yes", "varies", "varies", "yes"] },
    { feature: "Hybrid lexical + semantic retrieval", cells: ["yes", "vector only", "partial", "no"] },
    { feature: "Provenance for every item", cells: ["yes", "no", "partial", "manual"] },
    { feature: "Procedural rules that decay", cells: ["yes", "no", "no", "no"] },
    { feature: "Anti-patterns and harmful feedback", cells: ["yes", "no", "no", "manual"] },
    { feature: "Explainable scores", cells: ["yes", "no", "partial", "n/a"] },
    { feature: "Graph analytics over memories", cells: ["yes", "no", "no", "no"] },
    { feature: "Deterministic JSON output", cells: ["yes", "varies", "varies", "n/a"] },
    { feature: "Imports past agent sessions", cells: ["yes", "manual ETL", "no", "manual"] },
    { feature: "Works with no daemon running", cells: ["yes", "no", "no", "yes"] },
    { feature: "Single-binary install", cells: ["yes", "no", "no", "n/a"] },
    { feature: "Audit log of curation", cells: ["yes", "no", "no", "git only"] },
  ],
} as const;

/** README "Hard Requirements" and CLAUDE.md "Forbidden dependencies": each is a CI failure. */
export const forbidden = [
  { no: "Tokio, async-std, smol", instead: "Asupersync", why: "Structured concurrency, cancellation as a value, deterministic lab tests." },
  { no: "rusqlite, SQLx, Diesel", instead: "FrankenSQLite + SQLModel", why: "One Rust storage stack with MVCC, owned end to end." },
  { no: "Hand-rolled BM25, RRF, vectors", instead: "Frankensearch", why: "Hybrid search lives in one audited place." },
  { no: "petgraph", instead: "FrankenNetworkX", why: "Graph algorithms with NetworkX semantics, in Rust." },
  { no: "hyper, axum, reqwest", instead: "No HTTP in the core", why: "Memory does not need a network to work." },
] as const;

/** Counts behind the "rules" section, taken at COUNTED. */
export const rigor = [
  { value: "1.44M", label: "lines of Rust in src/", note: "771 files; another 443k lines of tests." },
  { value: "91", label: "architecture decisions", note: "Numbered ADRs in docs/adr, each with status and date." },
  { value: "133", label: "forward-only migrations", note: "No down-migrations: back up, then move forward." },
  { value: "14,486", label: "commits", note: "Since the first one, 2026-04-29." },
] as const;

/** Releases, from CHANGELOG.md and the tags' dates. */
export const releases = [
  { version: "0.1", date: "2026-05-15", what: "First release: remember, search, pack." },
  { version: "0.13", date: "2026-07-30", what: "A user-global memory lane beside each workspace." },
  { version: "0.14", date: "2026-08-18", what: "Team confederation: signed, hash-chained sharing between stores." },
  { version: "0.15", date: "2026-09-12", what: "Published on crates.io." },
  { version: "0.16", date: "2026-09-27", what: "ee ask: direct answers with citations." },
  { version: "0.17", date: "2026-10-02", what: "Quality ranking and the knowledge skyline." },
  { version: "0.18", date: "2026-10-14", what: "Failure-to-fix recall from imported sessions." },
  { version: "0.19", date: "2026-11-21", what: "Keyed CASS commitments, warm daemon packs, Go/Rust/Vitest/Jest error recall, cargo audit 12 → 0." },
] as const;

/** README "Limitations" and the ADRs: what is not done, said plainly. */
export const limitations = [
  {
    title: "Personalized PageRank is unavailable",
    text: "The pinned graph library has no deterministic personalized PageRank yet, so that ranking signal and Pack DNA's PPR neighbours report graph_ppr_upstream_unavailable and fall back. Every other graph surface works.",
  },
  {
    title: "Graph ranking features are opt-in",
    text: "The graph commands are built in, but every graph.feature.* flag that lets graph structure move a pack's ranking defaults to off.",
  },
  {
    title: "Determinism is per machine, for now",
    text: "Same inputs give the same pack hash. Cross-process and cross-host determinism gates are not established by the current hash contract (ADR 0087).",
  },
  {
    title: "Heavy write swarms need a write owner",
    text: "Many agents can read at once. FrankenSQLite is single-process MVCC, so heavy concurrent writing goes through job locks or the optional daemon.",
  },
  {
    title: "Release signatures, not SLSA provenance",
    text: "Current releases ship SHA-256 sums and minisign signatures. There are no Sigstore bundles or SLSA provenance, so --require-provenance fails by design.",
  },
  {
    title: "Some performance gates are unproven",
    text: "A warm daemon measured 708 ms against 3,155 ms for the same pack on one worker; the 10,000-memory, 2x median gate is still unverified.",
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
  { name: "Claude Code", command: "ee hook claude-code --install --workspace . --json", note: "Managed SessionStart and pre-edit hooks inject recall and orientation. They never intercept or deny a command." },
  { name: "Codex", command: "ee hook codex --install --workspace . --json", note: "Codex shells out, so every ee command works as is; ee pack --json drops straight into a message." },
  { name: "MCP", command: "ee mcp manifest --json", note: "Tools such as ee_context, ee_search and ee_remember mirror the CLI's contracts exactly." },
  { name: "Anything with a shell", command: 'ee pack "<task>" --workspace . --format markdown', note: "The CLI is the contract. Scripts and humans get the same JSON the agents do." },
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
    title: "Secrets never reach the disk",
    text: "API keys, JWTs, passwords, private and SSH keys, AWS secrets and OAuth tokens are replaced with stable placeholders before storage. Five redaction levels, from none to paranoid, govern every export.",
  },
  {
    title: "Memories cannot hijack the agent",
    text: "Before packing, stored memories are screened for role overrides, hidden-prompt requests, credential requests and authority claims. A match is omitted with an explanation and kept for inspection.",
  },
  {
    title: "Advice, never a veto",
    text: "ee preflight looks up the risk history of a command before you run it. It informs; it never blocks a shell command, and no hook ever denies one.",
  },
  {
    title: "Imported evidence is committed",
    text: "Lines imported from past sessions carry keyed BLAKE3 commitments under the store's root key, so evidence can be checked without keeping raw digests of it.",
  },
] as const;

/** README "Graph" and "Graph-derived insights": what each tool answers, and the command. */
export const graphTools = [
  { name: "PageRank", answers: "Which memories does everything else lean on?", command: "ee graph pagerank --limit 10" },
  { name: "HITS", answers: "Which memories are hubs that point at good ones, and which are the authorities?", command: "ee graph hits --limit 10" },
  { name: "Articulation points", answers: "Which single memory, if wrong, cuts a topic off from the rest?", command: "ee graph articulation" },
  { name: "Gomory-Hu proximity", answers: "How tightly are two memories bound, and through what?", command: "ee proximity <memory-a> <memory-b> --json" },
  { name: "Louvain communities", answers: "Which memories form a topic, whatever their tags say?", command: "ee graph louvain" },
  { name: "k-core and k-truss", answers: "Where is the knowledge dense, and where is it thin?", command: "ee health --robot-insights --json" },
  { name: "Causal paths", answers: "What chain of events led to this failure?", command: "ee why <memory-id> --causal-explain --json" },
  { name: "Link prediction", answers: "Which links are missing? Proposed, never written silently.", command: "ee graph suggest-links --propose" },
  { name: "Pack DNA", answers: "Why does this pack look the way it does: dominators, communities, ego graphs.", command: 'ee pack "<task>" --explain --json' },
  { name: "Knowledge skyline", answers: "The store's posture across layers, communities, trust, age and support.", command: "ee insights --section knowledgeSkyline --json" },
] as const;
