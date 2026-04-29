# COMPREHENSIVE PLAN — `ee` (Eidetic Engine CLI)

**A single-binary Rust CLI memory substrate for AI coding agents, built on the franken-stack.**

> Status: design plan, pre-implementation. This document is the authoritative starting point. Read it end-to-end before opening `Cargo.toml`.

---

## Table of Contents

0. [Reading guide](#0-reading-guide)
1. [Executive summary](#1-executive-summary)
2. [Background — what changed since the original Eidetic Engine](#2-background)
3. [Core design principles](#3-core-design-principles)
4. [North Star acceptance scenarios](#4-north-star-acceptance-scenarios)
5. [Non-goals for v1](#5-non-goals)
6. [Technology stack & dependencies](#6-technology-stack)
7. [Dependency integration contracts](#7-dependency-integration-contracts)
8. [Runtime architecture with asupersync](#8-runtime-architecture)
9. [Crate / module layout](#9-crate-layout)
10. [Storage architecture](#10-storage-architecture)
11. [Data model](#11-data-model)
12. [Memory lifecycle](#12-memory-lifecycle)
13. [Hybrid retrieval pipeline](#13-hybrid-retrieval-pipeline)
14. [Knowledge graph layer](#14-knowledge-graph-layer)
15. [Session ingestion via `cass`](#15-session-ingestion)
16. [Curation, consolidation, and review](#16-curation)
17. [Procedural memory & playbooks](#17-procedural-memory)
18. [Trauma guard & confidence decay](#18-trauma-guard)
19. [Deterministic context packing](#19-context-packing)
20. [CLI surface](#20-cli-surface)
21. [Diagnostics, repair, and `ee why`](#21-diagnostics)
22. [Privacy, redaction, and safety](#22-privacy)
23. [Agent hook integration](#23-hook-integration)
24. [Optional MCP server mode](#24-mcp-server)
25. [Configuration](#25-configuration)
26. [On-disk layout](#26-on-disk-layout)
27. [Testing strategy & evaluation harness](#27-testing-strategy)
28. [Performance budget](#28-performance-budget)
29. [Walking skeleton acceptance gate & rollout milestones](#29-walking-skeleton)
30. [Granular backlog](#30-granular-backlog)
31. [Risks & open questions](#31-risks)
32. [Appendix A — full SQL schema](#appendix-a)
33. [Appendix B — JSON output contracts](#appendix-b)
34. [Appendix C — example end-to-end agent flow](#appendix-c)

---

## 0. Reading guide

`ee` is a Rust CLI memory substrate. The single most important sentence in this document is:

> **`ee` does NOT replace agent harnesses (Claude Code, Codex, Cursor, Gemini); it is the persistent memory those harnesses delegate to.**

The original Eidetic Engine tried to be the agent loop and the memory store and the orchestrator. It failed because it owned too much. `ee` owns exactly one thing: durable, structured, queryable memory that agents read from and write to via a stable, scriptable interface (CLI primarily; MCP optionally; Rust library for in-process embedding).

Everything else in this plan derives from that constraint.

If you find yourself proposing a feature that requires `ee` to know what an agent is currently doing — *don't*. Agents push facts in; agents pull context out. `ee` never reaches.

---

## 1. Executive summary

**Goal.** Ship a single Rust binary, `ee`, that gives any AI coding agent durable structured memory: working / episodic / semantic / procedural, plus a typed knowledge graph linking the four. Memories are searched via hybrid lexical + semantic retrieval through frankensearch, ranked with graph-derived signals, and packed into reproducible context bundles with a token budget. Procedural rules age via confidence decay and a CASS-style trauma guard; harmful rules invert into anti-patterns rather than hanging around as bad advice.

**Anti-goal.** `ee` is not an agent runtime, not a planner, not a tool router, not a chat shell, not a workflow orchestrator, and not a destructive-command guard (that's `dcg`'s job). It does not run background workers by default. It does not call LLMs unless explicitly asked.

**Tech stack — non-negotiable per the user's brief.**
- `frankensqlite` (`fsqlite` crate) for storage. **No `rusqlite`.**
- `sqlmodel_rust` for the typed model/query layer.
- `asupersync` for async/IO. **No `tokio`.**
- `frankensearch` for hybrid retrieval. We use whatever embedding models frankensearch ships with default features — model selection is owned by frankensearch, not by `ee`.
- `coding_agent_session_search` (the `cass` CLI + its frankensqlite DB) for ingestion of agent session history.
- `franken_networkx` (its native Rust crates: `fnx-runtime`, `fnx-classes`, `fnx-algorithms`, `fnx-cgse`, `fnx-convert`) for graph metrics. **No PyO3 needed; the Rust workspace is real.**
- Concept transplants from `cass_memory_system`: playbook bullets with maturity progression, confidence decay, trauma guard, and the `cm context` "before-work" call.

**Surface.**
- A CLI `ee` with subcommands grouped naturally (`init`, `remember`, `search`, `context`, `pack`, `link`, `outcome`, `playbook`, `rule`, `import`, `review`, `curate`, `graph`, `why`, `doctor`, `eval`, `db`, `index`, `daemon`, `mcp`, `serve`, `export`, `completion`).
- A `ee mcp` mode that exposes the same operations as MCP tools (feature-gated).
- A Rust library crate (`ee-core`) for in-process use.

**Storage default.** A single user-level database at `~/.local/share/ee/ee.db` tracks any number of workspaces. A workspace's `.ee/config.toml` is optional but supported, and per-project DBs are available via `--db <path>` for teams that want full isolation.

**The walking skeleton** (what proves the architecture works) ships in M2: `ee init`, `ee remember`, `ee search`, `ee context`, `ee why`, `ee status`. Everything else layers on this skeleton.

---

## 2. Background — what changed since the original Eidetic Engine

The user's original Python project, `/dp/eidetic_engine_agent`, defined a **Unified Memory System (UMS)** with the following architecture:

- A FastAPI + FastMCP HTTP server.
- A SQLite (or Postgres) backing store with tables `memories`, `memory_links`, `embeddings`, `memory_graph_edges`, `memory_graph_metadata`.
- A four-level hierarchy: `WORKING`, `EPISODIC`, `SEMANTIC`, `PROCEDURAL`.
- A typed memory taxonomy and typed graph-relation vocabulary.
- A "Memory Steward" background worker that consolidated clusters into summaries, ran auto-linking, did Hebbian weight updates, ran retention policies, and snapshotted the graph.
- An "Agent Master Loop" (AML) that owned the full agent loop: plan → act → reflect → consolidate.
- An MCP tool surface for `ums/core/store_memory`, `ums/core/hybrid_search`, `ums/core/search_orchestrated`, `ums/core/graph_neighborhood`, `ums/core/consolidate_cluster`, `ums/core/promote_consistent_patterns`, etc.

**Why it stopped working.** The user states the project *"pre-dated AI agent harnesses like Codex and Claude Code, and became unworkable."* Concretely:

1. **The AML assumption is dead.** Modern harnesses *are* the agent loop. They own planning, tool dispatch, context-window management, and recovery. Anything `ee` says about "this is the next plan step" gets ignored or, worse, fights the harness.
2. **MCP-as-only-interface assumption is dead.** Claude Code natively supports MCP, Codex has its own protocol, Cursor has yet another. Agents also call CLI tools directly via Bash, and increasingly call inline scripts. A memory system needs to expose a CLI first; MCP is one face of many.
3. **Background workers fight modern agent harnesses.** A Memory Steward that wakes up and rewrites memory rows mid-session causes nondeterminism. Agents read state, the steward mutates it, agents read again, the world has shifted. Modern harnesses prefer pure functions called explicitly.
4. **Centralized server assumption is dead.** Agents now run on the user's laptop, on rented Contabo workers, on CI runners, in cloud VMs. A single FastAPI server is a SPOF and a network round-trip. `ee` must be a local process.
5. **Schema-validation tax was too high.** The original UMS had strict Pydantic models, JSONSchemas per tool, deprecation versioning, and a ten-page tool contract. CLI-first design lets us version with `--api 2` rather than full schema-migration dances.
6. **Scope creep.** The Python project tried to do artifact ingestion, NER/RE weaving, document search, telemetry, ACL, audit logs, inter-agent bundles, retention policies, and more. About 80% of those features had no real users.

**What's worth keeping** (verbatim, from the paper):

- The four-level hierarchy.
- The typed memory taxonomy.
- The typed relation vocabulary.
- The hybrid fusion pipeline (lexical + dense + graph + temporal).
- The deterministic context-packing algorithm with budget, type quotas, MMR redundancy.
- Hebbian link strengthening from co-occurrence.
- Consolidation as an explicit operation (cluster → SUMMARY memory + DERIVED_FROM links).
- The "explain why this was retrieved" surface.

**What `cass_memory_system` (the user's procedural-memory project) adds:**

- The `playbook.yaml` artifact: a YAML/JSON list of `PlaybookBullet` rules with `id`, `category`, `content`, `confidence ∈ [0..1]`, `maturity ∈ {candidate, established, proven, deprecated, retired}`, `helpful_count`, `harmful_count`, `marked` timestamp, `decayHalfLifeDays` (default 90).
- **Confidence decay**: `confidence_now = confidence_initial * 0.5^(days_since_marked / half_life)`.
- **Trauma guard**: harmful marks count 4× helpful. After a configurable threshold (default ≥2 harmful + low trust) a rule inverts to a `deprecatedPattern` ("AVOID" anti-pattern).
- The **ACE pipeline** (Analyze → Curate → Extract): a session's diary is reflected into new candidate playbook bullets, with evidence-gating against historical precedent.
- The `cm context "<task>"` call as the **before-work** primitive that returns `relevantBullets`, `antiPatterns`, `historySnippets`, `suggestedQueries`.

`ee` absorbs the playbook concept directly. It does not absorb CASS's Bun/TypeScript runtime; the Rust port is part of the cleanup.

### Keep / drop / reinterpret matrix

| Original idea | Disposition |
|---|---|
| Working/episodic/semantic/procedural levels | **Keep** as first-class with type-specific scoring & quotas |
| Typed memory links | **Keep** (`memory_links` + graph snapshots) |
| Hybrid retrieval | **Keep** (frankensearch `TwoTierSearcher` + SQL filters + graph features) |
| Workflow / action / artifact / thought traceability | **Keep** as first-class tables |
| Memory steward | **Reinterpret** — explicit `ee steward run` jobs and an optional supervised daemon, not always-on |
| Consolidation / promotion | **Reinterpret** — explicit propose/validate/apply curation flow, not opaque autonomous rewriting |
| Context packing | **Keep** with provenance and pack records |
| Retrieval explanations | **Keep** — `ee why` is a first-class command |
| Declarative memory queries | **Keep** as a compact EQL-inspired JSON schema |
| Recency / utility / confidence / importance / access counts | **Keep** as scoring inputs in the DB |
| Full Python FastAPI service as primary product | **Drop** — local CLI fits agent harnesses better |
| Always-on LLM orchestrator | **Drop** — harnesses already orchestrate |
| MCP as central architecture | **Drop** — MCP is an adapter, not the core |
| Heavyweight web UI dependency | **Drop** for v1 |
| Tool registry & action execution engine | **Drop** — harnesses own tool calling |
| Assumption that the memory system controls the agent | **Drop** — `ee` advises, never replaces |
| Agent Master Loop | **Reinterpret** — a set of CLI entry points harnesses call at lifecycle points |
| Goal stack | **Reinterpret** — workflow / task records |
| Reflection | **Reinterpret** — agent-native review commands that produce proposed memory deltas |
| Semantic memory | **Reinterpret** — curated facts and rules with evidence, confidence, and decay |

---

## 3. Core design principles

These are the rules that let me kill features later when in doubt.

1. **CLI first.** Every operation has a CLI subcommand with stable JSON output (`--json`). MCP and library APIs derive from the CLI argument schema, not the other way around.
2. **Local first.** All primary data lives on the developer's machine. No cloud dependency required. Remote APIs and model downloads must be explicit opt-in.
3. **Harness agnostic.** `ee` works from any shell and can be called by Codex, Claude Code, custom scripts, or humans. It does not assume control over the agent loop.
4. **Daemon later, never required.** Every essential feature must work as a direct CLI command. A daemon may improve latency and background maintenance, but no core command should require it in v1.
5. **Deterministic by default.** Given the same database, indexes, config, and query, JSON output is stable. Ranking ties are deterministic (via `fnx-cgse`). Context-pack hashes are reproducible.
6. **Explainable retrieval.** Every returned memory must answer: why selected? which source supports it? how fresh? how reliable? what score components mattered? what would change the decision?
7. **Search indexes are derived assets.** FrankenSQLite + SQLModel hold source of truth. Frankensearch indexes, embeddings, graph snapshots, caches are rebuildable.
8. **Graceful degradation.** If semantic search is unavailable, lexical search still works. If graph metrics are stale, retrieval still works. If `cass` is unavailable, explicit `ee` memories still work. Every degraded result says **what** degraded and **why**.
9. **No silent memory mutation.** The system may *propose* rules, promotions, consolidations, and tombstones. It does not silently rewrite procedural memory without a recorded audit entry.
10. **Evidence over vibes.** Procedural rules need evidence pointers. A rule with no source session, no feedback, and no validation stays low-confidence.
11. **Synchronous core, async edges.** `frankensqlite` is sync; the DB-touching code paths are sync. Async (`asupersync`) is reserved for two things: (a) embedding-model inference where it overlaps with disk I/O, and (b) the optional MCP/HTTP servers and daemon.
12. **Single writer.** `ee` does not pretend multi-process concurrent SQLite writers are safe. Writes are serialized via the daemon (when present) or an advisory file lock; readers are unconstrained.
13. **Hybrid fusion is the retrieval default.** Never expose a single-index search as the recommended path. We rely on frankensearch's `TwoTierSearcher` and its built-in fusion; `ee` never hand-rolls BM25, vector storage, or RRF.
14. **Memories are immutable by default.** Updates create a new row with `supersedes` pointing at the old. Auditing trivial; matches how the original UMS handled `update_memory` in practice.
15. **Trust internal code.** Don't validate at internal boundaries, only at the CLI/MCP edge. No defensive `Option::expect` chains in the storage layer.
16. **Comments only when WHY is non-obvious.** No what-it-does comments.
17. **No backwards-compat shims.** Fresh codebase. Renames happen freely until v1.0.
18. **The right answer is the small one.** Three similar SQL queries beat a query-builder abstraction.

---

## 4. North Star acceptance scenarios

These are the eight concrete scenarios `ee` is judged by. They're product-level tests, not unit tests; if any of them fail, the design is wrong, regardless of how clean the code looks. Each scenario has a command, a "good output must include" list, and a success signal.

### Scenario 1 — Release memory saves a bad release

```bash
ee context "what should I know before releasing this project?" \
  --workspace . --format markdown
```

**Good output must include:**
- Project-specific release rules.
- Known branch-naming or publishing traps.
- Prior release failures from imported `cass` sessions.
- Required verification commands.
- High-severity anti-patterns.
- Evidence pointers for every claim.

**Success signal.** An agent that has never worked in the repo avoids a previously-repeated release mistake.

### Scenario 2 — Async migration honors the real runtime model

```bash
ee context "replace a tokio service with asupersync" --workspace . --json
```

**Good output must include:**
- No-tokio constraints.
- `&Cx`-first API guidance.
- `Outcome`, budget, and capability rules.
- Examples of owned `Scope` work.
- Deterministic-testing requirements.
- References to prior sessions or project rules that justify the advice.

**Success signal.** The pack prevents a shallow "swap the executor" attempt.

### Scenario 3 — Repeated CI failure becomes procedural memory

```bash
ee import cass --workspace . --since 90d --json
ee search "clippy warning release failed" --workspace . --json
ee review session --cass-session <id> --propose --json
ee curate apply <candidate-id> --json
```

**Good output must include:** the relevant prior session, the exact failure pattern, the eventual fix, a proposed scoped procedural rule, and duplicate-rule warnings if a similar memory already exists.

**Success signal.** The next `ee context` call surfaces the rule before the agent repeats the failure.

### Scenario 4 — New repository onboarding without a web UI

```bash
ee context "start working in this repository" \
  --workspace . --max-tokens 3000 --format markdown
```

**Good output must include:** known project conventions, dominant language and tooling patterns, previous high-value sessions for the same workspace, commands to run before editing, warnings about dangerous or unusual project rules, degraded-mode warnings if cass or semantic search is unavailable.

**Success signal.** A first-time agent gets enough local memory to make fewer wrong assumptions in its first turn.

### Scenario 5 — Catastrophic mistake avoidance

```bash
ee context "clean up generated files and reset the repo state" \
  --workspace . --format markdown
```

**Good output must include:** high-severity risk memories about destructive cleanup, safer alternatives, scope-specific approval rules, provenance for the prior incident or policy.

**Success signal.** The pack makes the safe path obvious before the harness attempts risky commands. (Note: `ee` does not *block* commands — that's `dcg` — but it *informs* the agent enough to ask for permission rather than acting.)

### Scenario 6 — Offline degraded mode still helps

Setup: no semantic model available, cass unavailable or not indexed, only explicit `ee remember` records exist.

```bash
ee context "run tests before release" --workspace . --json
```

**Good output must include:** lexical results over explicit memory, clear `degraded` capability fields, no false claim that semantic or cass contributed, actionable next steps (`ee doctor --fix-plan`).

**Success signal.** `ee` remains useful without fragile optional systems.

### Scenario 7 — Post-session distillation is auditable

```bash
ee review session --cass-session <id> --propose --json
ee curate validate <candidate-id> --json
ee curate apply <candidate-id> --json
ee memory show <new-memory-id> --json
ee why <new-memory-id> --json
```

**Good output must include:** proposed memory or rule, validation warnings, evidence spans, audit entries, and search/index job status.

**Success signal.** Future readers can tell why a rule exists and which session produced it.

### Scenario 8 — Multi-agent local work doesn't corrupt memory

```bash
# (two agents running concurrently)
ee remember --workspace . --level semantic "Agent A learned X" --json
ee remember --workspace . --level semantic "Agent B learned Y" --json
ee status --json
```

**Good output must include:** serialized or safely-coordinated writes, no duplicate IDs, no broken index manifest, clear lock or daemon guidance if contention occurred.

**Success signal.** Storage posture survives normal local multi-agent workflows without pretending arbitrary concurrent writers are safe.

---

## 5. Non-goals for v1

- Do not build a replacement for Codex, Claude Code, or any agent harness.
- Do not build a new general-purpose workflow engine.
- Do not build a web UI before the CLI is useful.
- Do not require MCP for normal operation.
- Do not require paid LLM APIs.
- Do not depend on Tokio.
- Do not use `rusqlite`.
- Do not lead with browser/QUIC/H3/RaptorQ surfaces from asupersync.
- Do not rely on multi-process concurrent SQLite writers for correctness.
- Do not implement custom RRF, custom vector storage, or custom BM25 — frankensearch already provides them.
- Do not store secrets in context packs.
- Do not try to make all memories permanent. Forgetting and decay are features.
- Do not pick embedding models — that's frankensearch's job.

---

## 6. Technology stack & dependencies

The exact top-level `Cargo.toml` shape. Versions are current-as-of-research; pin and bump deliberately.

```toml
[workspace]
resolver = "2"
members = [
  "crates/ee-cli",
  "crates/ee-core",
  "crates/ee-models",
  "crates/ee-db",
  "crates/ee-search",
  "crates/ee-cass",
  "crates/ee-graph",
  "crates/ee-pack",
  "crates/ee-curate",
  "crates/ee-steward",
  "crates/ee-policy",
  "crates/ee-output",
  "crates/ee-test-support",
]

[workspace.package]
edition       = "2024"
version       = "0.1.0"
rust-version  = "1.85.0-nightly"
license       = "MIT OR Apache-2.0"

[workspace.dependencies]
# --- Storage (forbidden: rusqlite) -------------------------------------------
fsqlite              = "0.1"
fsqlite-core         = "0.1"
fsqlite-types        = "0.1"
fsqlite-error        = "0.1"
fsqlite-ext-fts5     = { version = "0.1", optional = true }
fsqlite-ext-json     = { version = "0.1", optional = true }

sqlmodel             = "0.2"
sqlmodel-core        = "0.2"
sqlmodel-query       = "0.2"
sqlmodel-schema      = "0.2"
sqlmodel-session     = "0.2"
sqlmodel-pool        = "0.2"
sqlmodel-frankensqlite = "0.2"

# --- Async / IO (forbidden: tokio, async-std, smol, hyper, axum, reqwest) ---
asupersync           = { version = "0.3", default-features = false, features = ["proc-macros"] }

# --- Search ------------------------------------------------------------------
# Use frankensearch's default features as configured by Jeffrey. Do NOT specify
# embedding models here; frankensearch picks the best CPU-friendly defaults.
frankensearch        = "0.3"

# --- Graph (native Rust crates from /dp/franken_networkx workspace) ----------
fnx-runtime          = { path = "../franken_networkx/crates/fnx-runtime", features = ["asupersync-integration"] }
fnx-classes          = { path = "../franken_networkx/crates/fnx-classes" }
fnx-algorithms       = { path = "../franken_networkx/crates/fnx-algorithms" }
fnx-cgse             = { path = "../franken_networkx/crates/fnx-cgse" }
fnx-convert          = { path = "../franken_networkx/crates/fnx-convert" }

# --- CLI / output ------------------------------------------------------------
clap                 = { version = "4", features = ["derive", "env", "wrap_help"] }
clap_complete        = "4"
serde                = { version = "1", features = ["derive"] }
serde_json           = "1"
serde_yaml           = "0.9"
toml                 = "0.8"
toml_edit            = "0.22"
chrono               = { version = "0.4", features = ["serde"] }
ulid                 = { version = "1", features = ["serde"] }   # public IDs
indicatif            = "0.17"
console              = "0.15"
comfy-table          = "7"
colored              = "2"

# --- Hashing / IDs -----------------------------------------------------------
sha2                 = "0.10"
blake3               = "1"

# --- Tokenisation / token budgets --------------------------------------------
tiktoken-rs          = "0.6"

# --- MCP server (optional) ---------------------------------------------------
rust-mcp-sdk         = { version = "0.4", optional = true }

# --- Logging / errors --------------------------------------------------------
tracing              = "0.1"
tracing-subscriber   = { version = "0.3", features = ["env-filter", "json"] }
thiserror            = "2"

[workspace.lints.rust]
unsafe_code = "forbid"
```

**Forbidden dependencies** (enforced via `cargo deny` in CI):
- `rusqlite`.
- `tokio`, `tokio-util`, `async-std`, `smol`, `hyper`, `axum`, `tower`, `tonic`, `reqwest`.
- `sqlx`, `diesel`, `sea-orm`.
- `petgraph` (we use `fnx-*` instead).

A CI gate runs `cargo tree -e features --workspace -i <forbidden>` for each name and fails on any hit.

**Why every line is what it is.**
- `fsqlite` is the user's required SQLite. The `fts5` and `json` extensions are feature-gated because the FrankenSQLite README flags them as still in active wiring. We can fall back to a minimal inverted-index helper in `ee-db` if FTS5 isn't ready — see §13.
- `sqlmodel-frankensqlite` is the bridge: its `FrankenConnection` wraps the sync `fsqlite::Connection` in `Arc<Mutex<>>` and reports `Dialect::Sqlite` so query macros emit `?1, ?2`-style placeholders.
- `asupersync` with `default-features = false` keeps us off the optional SQLite feature (which could pull `rusqlite`) and gives us only what we explicitly opt into.
- `frankensearch` is depended on with default features. Jeffrey already chose the best CPU-friendly embedding models inside frankensearch; `ee` does not override them.
- `fnx-*` are native Rust crates from `/dp/franken_networkx/crates/`, including `fnx-runtime` (with an `asupersync-integration` feature), `fnx-cgse` (deterministic tie-breaking), and `fnx-algorithms` (PageRank, betweenness, Louvain, k-core, articulation points, HITS, shortest paths, etc.). They're path-deps until published.
- `ulid` for public IDs (`mem_<ulid>`, `rule_<ulid>`, etc.) — sortable, URL-safe, 26 chars. Internal row IDs may still be `INTEGER PRIMARY KEY` for performance; ULIDs are the *public* identifier surface.
- `tiktoken-rs` for token budgeting; Anthropic models tokenize close enough to BPE-100k for our purposes, with a small reserve margin.
- `rust-mcp-sdk` is the same crate `dcg` already uses.

---

## 7. Dependency integration contracts

Each major dependency enters `ee` through exactly one narrow integration crate. This prevents API churn, forbidden-feature leakage, and accidental reimplementation.

### 7.1 Asupersync contract

**Owned by** `ee-core`, `ee-steward`, `ee-test-support`, and the command boundary in `ee-cli`.

**Use for:**
- runtime bootstrap (`RuntimeBuilder`)
- `Cx`, `Scope`, `Outcome`, `Budget`
- capability narrowing
- native process / filesystem / time / sync / channel surfaces
- deterministic tests (`LabRuntime`, `test_utils::run_test*`)
- daemon supervision when needed (`AppSpec`, supervision trees)

**Do not use:**
- tokio compatibility shims as a default
- detached tasks
- full-power `Cx` everywhere
- flattened `Result` as the internal async contract

**Verification:**
- forbidden-dep audit in CI
- deterministic LabRuntime tests for cancellation and quiescence
- tests that preserve `Cancelled` and `Panicked` through policy boundaries
- region-quiescence tests on every command path

### 7.2 FrankenSQLite + SQLModel contract

**Owned by** `ee-db` exclusively.

**Use for:**
- source-of-truth storage
- migrations
- transactional repositories
- schema metadata
- idempotency and import ledgers

**Do not use:**
- `rusqlite`, `sqlx`, `diesel`, `sea-orm`
- JSONL as primary storage (it's an export, not a store)
- raw SQL outside `ee-db` (other crates depend on repository methods)

**Verification:**
- migration tests from empty DB and from prior schemas
- transaction-cleanup tests including cancellation mid-transaction
- `cargo tree -e features` audit for `rusqlite`
- repository-level tests with temp DBs

### 7.3 `cass` (coding_agent_session_search) contract

**Owned by** `ee-cass` exclusively.

**Use for:**
- discovering coding-agent sessions
- searching raw historical sessions
- viewing or expanding evidence spans
- importing session metadata and selected excerpts (not whole sessions)

**Do not use:**
- bare interactive `cass` commands (they launch a TUI)
- duplicated raw session stores
- ad-hoc parsing of unstable human output

**Integration paths:**
1. **CLI subprocess** for ad-hoc queries: `cass search <q> --robot --json`. Use asupersync `process::*` so cancellation, timeouts, and reaping behave correctly.
2. **Direct DB read** for bulk import: open `~/.local/share/cass/cass.db` via `FrankenConnection` and read `conversations` / `messages` tables directly. We never write to that DB.

**Verification:**
- fixture tests for every consumed JSON contract
- degraded-mode tests when cass is absent
- cancellation and process-reaping tests
- idempotent-import tests against a vendored fixture cass DB

### 7.4 Frankensearch contract

**Owned by** `ee-search` exclusively.

**Use for:**
- lexical and semantic candidate retrieval (`TwoTierSearcher`)
- two-tier progressive search (Phase-1 fast, Phase-2 quality)
- fusion and ranking primitives (RRF — built in, do not reimplement)
- persistent indexes (FSVI files + optional Tantivy BM25)

**Do not use:**
- custom RRF, custom vector storage, custom BM25
- direct index writes from unrelated crates
- overriding frankensearch's default embedding-model choices

**Verification:**
- "rebuild index from FrankenSQLite source of truth" test
- deterministic fixtures using frankensearch's hash embedder
- explain-output golden tests
- stale-index and degraded-mode tests

### 7.5 FrankenNetworkX contract

**Owned by** `ee-graph` exclusively.

**Use for:**
- graph projection from DB
- centrality (PageRank, betweenness, eigenvector, HITS)
- communities (Louvain, label propagation)
- shortest paths
- link prediction
- articulation points and bridges

**Do not use:**
- hand-rolled graph algorithms for core metrics
- nondeterministic tie-breaking — always pass `fnx-cgse` as the comparator
- graph metrics without snapshot metadata

**Verification:**
- karate-club fixture tests
- deterministic witness hashes
- stale-graph degradation tests
- graph-feature explanation tests

### 7.6 CASS Memory System concept contract

**Owned by** `ee-curate`, `ee-pack`, `ee-models`.

**Use for:**
- procedural-memory lifecycle (candidate → established → proven → deprecated → retired)
- anti-pattern handling
- confidence decay (`0.5^(days/half_life)`)
- harmful-feedback weighting (4×)
- agent-native curation
- diary entries
- tombstones and replacements

**Do not use:**
- TypeScript implementation details as runtime dependencies
- automatic LLM rewriting as a required path
- evidence-free rule promotion

**Verification:**
- scoring tests (decay at 0×, 1×, 2×, 10× half-life)
- rule validation tests (specificity, evidence)
- duplicate-rule tests
- harmful-feedback demotion tests

---

## 8. Runtime architecture with asupersync

Asupersync is the **semantic foundation**, not just an executor swap. Its structured concurrency, four-valued `Outcome`, capability narrowing, deterministic tests, and supervised long-lived services are the architecture.

### 8.1 Asupersync design commitments

- `&Cx` is the first argument in async APIs that `ee` controls.
- `Outcome<T, E>` is preserved through internal layers and only collapsed at real boundaries (CLI exit, MCP response, job ledger entry, supervisor decision).
- `Cancelled` is **not** flattened into an ordinary application error.
- `Panicked` is **not** treated as retryable domain failure.
- Budgets are part of request semantics, not just timeout wrappers.
- Long-running loops call `cx.checkpoint()` at natural yield points.
- Child work runs in `Scope` or child regions.
- Capability exposure is narrowed at crate and service boundaries.
- Pure domain code (scoring, validation, query parsing) does **not** take `Cx`.
- Tests use deterministic helpers from day one.

### 8.2 Outcome policy

Asupersync outcomes are four-valued. `ee` preserves the distinction until a real boundary:

| Boundary | Mapping |
|---|---|
| CLI success | exit 0, normal output |
| CLI domain error | nonzero exit code (per §20.4), stable JSON if `--json` |
| CLI cancellation | exit 130 (SIGINT) or documented `ee` cancellation code |
| CLI panic | hard failure, diagnostic, no misleading partial success |
| Steward job | job-ledger record: `ok` / `err` / `cancelled` / `panicked` |
| Daemon supervisor | restart-or-stop policy sees the original outcome severity |

Internal failures are **not** prematurely converted to `anyhow::Error` or `String`. That erases the information asupersync provides for retry, shutdown, cleanup, and supervision.

### 8.3 Budget policy

Every request path has a budget. Budgets compose by meeting parent and child constraints; child work cannot consume more than the caller intended.

Initial budget posture:

| Surface | Budget posture |
|---|---|
| `ee status` | short request budget |
| `ee remember` | moderate request + short cleanup |
| `ee search` | request budget with search/index fallback budget |
| `ee context` | explicit context budget; child budgets for search, graph, packing |
| `ee import cass` | batch budget, checkpoint after each batch |
| `ee index rebuild` | long-job budget, cancellable and resumable |
| `ee graph refresh` | long-job budget, cancellable and snapshot-based |
| daemon shutdown | short masked cleanup budget |

Rules:
- Avoid `Budget::INFINITE` except in carefully justified root-service cases.
- Retrying import / process calls / indexing / search consumes a **total** retry budget.
- Cleanup/finalization may mask cancellation only inside narrow bounded sections.
- Speculative or hedged work gets a tighter child budget than the primary path.
- Tests assert budget-exhaustion behavior for import, indexing, and steward jobs.

### 8.4 Capability narrowing

`Cx` carries authority. We don't pass full-power context everywhere.

| Layer | Capability shape |
|---|---|
| pure scoring & validation | no `Cx` |
| query parsing | no `Cx` |
| pack selection | read-only, time/budget only |
| repositories | DB and time as needed |
| cass adapter | process, IO, time, cancellation |
| indexer | IO, time, spawn inside owned scope |
| graph refresh | CPU budget, time, cancellation |
| daemon supervisor | spawn, time, signal, registry |

**Boundary rule:** receive a runtime-managed `Cx`, narrow it at the command/service boundary, pass only the narrowed context into deeper layers. Never use ambient global authority as a substitute for explicit capabilities.

### 8.5 CLI runtime

```
main
  → parse args
  → build runtime (RuntimeBuilder)
  → load config
  → build services
  → run command future inside an owned region with a budget
  → render output
  → map Outcome to exit code
```

`main` is thin. Command handlers are request regions with budgets. Subprocess calls to `cass` use asupersync `process::*`, never tokio APIs. Filesystem reads/writes use asupersync `fs::*` where async is needed.

### 8.6 Daemon runtime (post-v1)

`ee daemon --foreground` is optional and arrives in M8. It supervises:
- import watcher
- indexing queue processor
- graph refresh queue
- curation candidate queue
- retention and redaction audits
- health reporter
- the **single write owner** for the user DB

Daemon constraints: single write owner, cancellable jobs, crash leaves resumable queue state, every job has a budget, every job writes an audit/job-ledger record.

Likely supervised children: `ImportWorker`, `IndexWorker`, `GraphWorker`, `CurationWorker`, `PrivacyAuditWorker`, `HealthReporter`, `WriteOwner`.

Supervision policy:
- independent workers: one-for-one restart
- workers depending on `WriteOwner`: rest-for-one or explicit startup ordering
- shared critical state: one-for-all when justified

Daemon handles are obligations: shutdown stops, drains, and joins children — not "drop and hope."

### 8.7 Primitive selection

| Problem | Preferred primitive |
|---|---|
| local concurrent request work | `Scope` + child regions |
| single-owner mutable state | actor or `GenServer` |
| typed internal request/reply | `GenServer` or session channels |
| many long-lived services | `AppSpec` + supervision |
| latest config snapshot | `watch` |
| event fan-out | `broadcast` |
| many producers, one queue owner | `mpsc` with two-phase send |
| one result, one waiter | `oneshot` |
| bounded concurrent use | `Semaphore` |
| resource checkout | `Pool` / `GenericPool` |
| acquire / use / release | `bracket`-style orchestration |
| retry with bounded total cost | native retry combinator + budget |

Avoid: background-task-plus-`Arc<Mutex<State>>` when one service should own the state; ad-hoc `select!` timeout/retry forests; `watch` for durable streams; `broadcast` for request/reply.

---

## 9. Crate / module layout

The repository is a Rust workspace from day one. The first slice ships only `ee-cli`, `ee-core`, `ee-models`, `ee-db`, `ee-output`, `ee-test-support`. Other crates appear when their first user does.

```
eidetic_engine_cli/
├── Cargo.toml                    # workspace root
├── rust-toolchain.toml           # nightly pin
├── crates/
│   ├── ee-cli/                   # the binary; clap parser, output formatting, exit codes
│   ├── ee-core/                  # use cases, application services, runtime wiring
│   ├── ee-models/                # domain types, IDs, enums, output contracts
│   ├── ee-db/                    # SQLModel models, migrations, repositories, transactions
│   ├── ee-search/                # frankensearch integration, indexing jobs, retrieval scoring
│   ├── ee-cass/                  # cass adapter (subprocess + DB read)
│   ├── ee-graph/                 # graph projection, fnx algorithms, snapshots
│   ├── ee-pack/                  # context packing, token budgets, MMR, provenance
│   ├── ee-curate/                # rule candidates, validation, feedback scoring, maturity
│   ├── ee-steward/               # maintenance jobs, daemon mode, scheduled refreshes
│   ├── ee-policy/                # redaction, privacy, scope, retention, trust
│   ├── ee-output/                # JSON, Markdown, TOON, human terminal rendering
│   └── ee-test-support/          # LabRuntime helpers, fixtures, golden utilities
├── docs/
│   ├── storage.md                # DB, indexes, migrations, backup
│   ├── query-schema.md           # EQL-inspired request schema
│   ├── context-packs.md          # packing algorithm and contracts
│   ├── cass-integration.md       # cass import & provenance
│   ├── scoring.md                # confidence / utility / decay / maturity
│   ├── graph.md                  # graph model and algorithms
│   ├── integration.md            # Codex, Claude Code, shell usage
│   └── privacy.md                # redaction, secret handling, remote-model policy
└── tests/
    └── fixtures/                 # cass DB fixtures, evaluation fixtures, golden snapshots
```

### Dependency direction

```
ee-cli  → ee-core
ee-core → ee-db, ee-search, ee-cass, ee-graph, ee-pack, ee-curate, ee-policy, ee-output
ee-db, ee-search, ee-graph, ee-pack, ee-curate → ee-models
ee-test-support → asupersync test utilities only
```

Rules:
- Lower-level crates **never** depend on `ee-cli`.
- Domain types live in `ee-models`, not `ee-cli`.
- Repositories return domain types, not CLI output structs.
- Search indexes are written through `ee-search`, not directly from command handlers.
- Graph metrics are derived from DB records, not maintained in unrelated code.
- `ee-output` depends on `ee-models` only; it has no logic, just rendering.

---

## 10. Storage architecture

### 10.1 Source of truth

FrankenSQLite is the source of truth, accessed through SQLModel:

```
ee-db
  → sqlmodel
  → sqlmodel-frankensqlite
  → fsqlite
```

JSONL is **never** primary storage. It's an export/backup format for Git-friendly project memory (see §10.4).

### 10.2 Concurrency posture (realistic)

FrankenSQLite currently supports single-process multi-connection MVCC WAL better than multi-process multi-writer workloads. `ee` designs around that reality.

V1 storage posture:
- One-shot CLI commands open one logical connection and complete quickly.
- Write-heavy background work is serialized through (a) an **advisory file lock** at `~/.local/share/ee/ee.db.write-lock` for v1 CLI-only mode, or (b) the daemon `WriteOwner` actor when daemon mode is enabled.
- Multi-process writes coordinate via the advisory lock; writers that can't acquire it within budget return a clear "writer busy, retry or run `ee daemon`" error.
- Search indexes are rebuildable and may lag.
- Imports are resumable through the `import_ledger`.

**Do not assume:** arbitrary concurrent CLI writers are always safe; a swarm of agents can all write to the same DB without coordination; the search index is always current. Each is a contract `ee` does not provide.

### 10.3 Database locations

Default paths:

```
User database:    ~/.local/share/ee/ee.db                (single canonical store)
User indexes:     ~/.local/share/ee/indexes/             (FSVI, Tantivy, manifests)
User config:      ~/.config/ee/config.toml
Project config:   <workspace>/.ee/config.toml            (optional)
Project export:   <workspace>/.ee/memory.jsonl           (optional, opt-in)
```

The user DB tracks any number of workspaces via the `workspaces` table; `--workspace <path>` filters queries to a workspace's scope. Per-project DB isolation is available via `--db <path>` for teams that prefer it, but the default is a single user store.

### 10.4 JSONL export

JSONL is useful for backup, review, and Git-friendly project memory. It is not the source of truth.

Rules:
- DB → JSONL export is explicit (`ee export jsonl`) or configured.
- JSONL writes are atomic: temp + fsync + rename.
- JSONL contains schema-version markers.
- JSONL import is idempotent (uses `idempotency_keys`).
- JSONL export never runs concurrently with migrations.
- JSONL export omits `secret`-classified fields by default.

---

## 11. Data model

The schema is the contract. Every other component reads or writes through it.

### 11.1 Why sqlmodel + raw SQL coexist

`sqlmodel_rust` covers typed CRUD on regular tables. Three needs go beyond its DSL:
1. **FTS5 virtual tables** — created via raw `CREATE VIRTUAL TABLE … USING fts5(…)` and queried via `MATCH`. Hand-written; exposed as `queries::fts_search()` in `ee-db`.
2. **Vector columns** — stored as `BLOB` (LE-packed f32) with a sidecar `embeddings` table. The *index* lives in frankensearch's FSVI files, not in SQLite. The DB is the durable backup; FSVI is regenerable.
3. **Recursive CTEs for graph reachability** — a few queries are easier as `WITH RECURSIVE`. Live in `queries.rs`.

Everything else is `#[derive(Model)]`.

### 11.2 ID strategy

Public IDs are stable typed strings, ULID-encoded:

```
mem_<ulid>         memory
link_<ulid>        memory_link
sess_<ulid>        session
ev_<ulid>          evidence_span
rule_<ulid>        procedural_rule
pack_<ulid>        pack record
job_<ulid>         steward job / index job
ws_<ulid>          workspace
ag_<ulid>          agent
wf_<ulid>          workflow
act_<ulid>         action
art_<ulid>         artifact
cand_<ulid>        curation_candidate
fb_<ulid>          feedback_event
diary_<ulid>       diary_entry
graph_<ulid>       graph_snapshot
```

DB primary keys may be `INTEGER` row IDs for performance. Public IDs are the surface — every JSON output uses them, never raw row IDs.

### 11.3 Core enums

```rust
pub enum MemoryLevel { Working, Episodic, Semantic, Procedural }

pub enum MemoryKind {
    Fact, Decision, ProjectConvention, WorkflowStep, Artifact, Command,
    Failure, Fix, Preference, Rule, AntiPattern, Warning, Question, Summary,
    Goal, Plan, Hypothesis, Mistake, Entity, Note,
}

pub enum Scope {
    Global, Workspace, Repository, Language, Framework, Tool, Task, Session, Agent,
}

pub enum RuleMaturity { Candidate, Established, Proven, Deprecated, Retired }
pub enum RuleState { Draft, Active, NeedsReview, Retired, Tombstoned }

pub enum Relation {
    Related, Causal, Supports, Contradicts, Hierarchical, Sequential,
    References, Duplicates, Replaces, DerivedFrom, Evidences, Invalidates,
    Blocks, Unblocks, CoOccurs, SameTask, SameFile, SameError,
    Generalizes, Specializes, PartOf, Follows, CoTag, CoMention,
}

pub enum LinkSource { Agent, Auto, Consolidation, Ingestion }

pub enum FeedbackKind { Helpful, Harmful, Confirmed, Contradicted, Obsolete, Duplicated, Ignored }

pub enum RedactionClass { Public, Project, Private, Secret, Blocked }

pub enum CandidateType { Memory, Rule, AntiPattern, Link, Tombstone, DiaryEntry }
pub enum ValidationStatus { Pending, Ok, Warning, Rejected, Applied }
```

### 11.4 Tables (overview — full DDL in Appendix A)

| Table | Purpose |
|---|---|
| `meta` | Free-form key/value (`schema_version`, `install_id`). |
| `migrations` | Migration runner state. |
| `workspaces` | First-class workspace identity (root path, git remote, project name). |
| `agents` | Agent identity (`codex`, `claude_code`, `cursor`, `aider`, etc.). |
| `sessions` | Imported coding-agent sessions (from cass or recorded directly). |
| `evidence_spans` | Pointers to raw session messages, file excerpts, command outputs. |
| `memories` | The central table. |
| `memory_tags` | Normalized tags. |
| `memory_links` | Typed graph edges. |
| `embeddings` | One row per (memory, model, segment). Vector blob. |
| `memory_fts` | FTS5 virtual table over memory content. |
| `procedural_rules` | Specialized view of procedural memory (joined with a memory row). |
| `feedback_events` | Helpful/harmful events for memories or rules. |
| `playbook_bullets` | (Convenience view materialized over `procedural_rules` + `feedback_events`.) |
| `artifacts` | Files, URLs, generated outputs, plans. |
| `workflows` | Task/goal records. |
| `actions` | Commands, edits, searches, tests — meaningful steps within workflows. |
| `diary_entries` | Session-level summaries (CASS-style). |
| `curation_candidates` | Proposed memories/rules/links/tombstones awaiting validation/apply. |
| `pack_records` | Audit log of every context pack emitted. |
| `retrieval_policies` | Named retrieval/packing policies. |
| `search_index_jobs` | Indexing queue. |
| `graph_snapshots` | Materialized graph metric outputs (with TTL). |
| `import_ledger` | Resumable-import cursor state. |
| `audit_log` | Append-only operational audit trail. |
| `idempotency_keys` | Prevents duplicate imports and repeated direct writes. |
| `steward_jobs` | Job ledger for daemon/manual steward runs. |

### 11.5 The `memories` table — annotated

```rust
#[derive(Model, Debug, Clone, Serialize, Deserialize)]
#[sqlmodel(table = "memories")]
pub struct Memory {
    #[sqlmodel(primary_key)] pub id: i64,                 // internal row id
    pub public_id: String,                                // mem_<ulid>; UNIQUE
    pub level: MemoryLevel,
    pub kind: MemoryKind,
    pub scope: Scope,
    pub scope_key: Option<String>,                        // e.g., the workspace path or "rust"
    pub workspace_id: Option<i64>,
    pub session_id: Option<i64>,
    pub primary_evidence_id: Option<i64>,
    pub content: String,
    pub summary: Option<String>,                          // optional short form
    pub schema_name: Option<String>,                      // for typed kinds (e.g., "release_failure_v1")
    pub schema_version: Option<i32>,
    pub event_time: Option<DateTime<Utc>>,                // when the underlying event happened
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_until: Option<DateTime<Utc>>,
    pub importance: f32,                                  // [0..1]
    pub confidence: f32,                                  // [0..1]
    pub utility_score: f32,                               // [0..1], updated by feedback
    pub reuse_count: i64,
    pub access_count: i64,
    pub last_accessed_at: Option<DateTime<Utc>>,
    pub affect_valence: Option<f32>,                      // [-1..1]; for trauma flagging
    pub ttl_seconds: Option<i64>,
    pub expires_at: Option<DateTime<Utc>>,
    pub redaction_class: RedactionClass,
    pub content_hash: String,                             // BLAKE3 of normalized content
    pub dedupe_hash: String,                              // narrower hash for dedupe within scope
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: String,                               // e.g., "agent:claude-code", "user", "cass-import"
    pub supersedes: Option<i64>,
    pub legal_hold: bool,
    pub idempotency_key: Option<String>,
    pub meta_json: Option<String>,
}
```

Indexes (full DDL in Appendix A):
- `(public_id)` UNIQUE
- `(workspace_id, level, kind)`
- `(scope, scope_key)`
- `(content_hash)`, `(dedupe_hash)`
- `(expires_at)`, `(updated_at)`
- `(confidence DESC)`, `(utility_score DESC)`
- `(idempotency_key)` UNIQUE WHERE NOT NULL
- `(workspace_id, dedupe_hash)` UNIQUE WHERE workspace_id IS NOT NULL

**Why content_hash + dedupe_hash + idempotency_key?** Agents repeat themselves. Two `remember` calls with identical content in the same workspace collapse to one row; `access_count` bumps. The agent doesn't have to remember whether it has already saved a thought.

### 11.6 The `procedural_rules` view

A row in `procedural_rules` joins 1:1 with a row in `memories` where `level = procedural`. Splitting them gives clean specialized scoring fields without polluting `memories` with rule-only columns.

```rust
#[derive(Model)]
#[sqlmodel(table = "procedural_rules")]
pub struct ProceduralRule {
    pub id: i64,
    pub public_id: String,                  // rule_<ulid>
    pub memory_id: i64,                     // FK into memories
    pub rule_type: RuleType,                // Rule | AntiPattern | Warning | Preference | Checklist
    pub category: String,                   // "release", "testing", "performance", ...
    pub scope: Scope,
    pub scope_key: Option<String>,
    pub content: String,                    // human-readable rule
    pub rationale: Option<String>,
    pub search_pointer: Option<String>,     // suggested search string for evidence
    pub state: RuleState,
    pub maturity: RuleMaturity,
    pub helpful_count: i64,
    pub harmful_count: i64,
    pub decayed_helpful_score: f32,
    pub decayed_harmful_score: f32,
    pub effective_score: f32,               // computed by ee-curate
    pub last_validated_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub replaced_by_rule_id: Option<i64>,
    pub meta_json: Option<String>,
}
```

The on-disk `playbook.yaml` is a **derived export** of `procedural_rules`. Edits to YAML are imported back via `ee playbook import`.

### 11.7 The `evidence_spans` table

```rust
#[derive(Model)]
#[sqlmodel(table = "evidence_spans")]
pub struct EvidenceSpan {
    pub id: i64,
    pub public_id: String,                  // ev_<ulid>
    pub session_id: Option<i64>,
    pub source_type: SourceType,            // CassMessage | CassSnippet | File | Command | Manual | ImportedJsonl
    pub source_uri: String,                 // e.g., "cass://<session-id>:<message-id>"
    pub message_id: Option<String>,
    pub line_start: Option<i64>,
    pub line_end: Option<i64>,
    pub role: Option<String>,
    pub excerpt: Option<String>,            // compact; not the whole session
    pub excerpt_hash: String,
    pub redaction_class: RedactionClass,
    pub created_at: DateTime<Utc>,
    pub meta_json: Option<String>,
}
```

Rules:
- Store **compact excerpts**, not entire session logs.
- Always keep enough source URI data for `cass view` / `cass expand`.
- `secret`-classified excerpts require explicit policy to store.

### 11.8 Other key tables

`workflows` — task/goal records with `workspace_id`, `session_id`, `title`, `goal`, `status`, `started_at`, `completed_at`, `outcome_summary`.

`actions` — commands, edits, tests within a workflow. `(workflow_id, kind, command, status, exit_code, started_at, completed_at, artifact_id)`.

`feedback_events` — generalized helpful/harmful tracking for any memory or rule. `(target_type, target_id, event_type, weight, note, session_id, workspace_id, created_at, created_by)`.

`diary_entries` — per-session summaries. `(session_id, accomplishments_json, decisions_json, challenges_json, preferences_json, key_learnings_json, related_sessions_json, tags_json, search_anchors_json)`.

`curation_candidates` — proposed memories/rules/links/tombstones awaiting validation/apply. `(candidate_type, source_session_id, source_evidence_id, proposed_payload_json, validation_status, validation_warnings_json, score, reviewed_at, reviewed_by, applied_at)`.

`pack_records` — every `ee pack` / `ee context` call: `(workspace_id, query_text, query_json, format, max_tokens, estimated_tokens, pack_hash, selected_items_json, explain_json, created_at)`.

`graph_snapshots` — `(graph_kind, node_count, edge_count, algorithm_versions_json, metrics_json, witness_hash, created_at, ttl_seconds)`.

Full DDL in Appendix A.

### 11.9 The FTS5 virtual table

```sql
CREATE VIRTUAL TABLE memory_fts USING fts5(
    content,
    public_id UNINDEXED,
    tokenize = 'porter unicode61 remove_diacritics 1'
);
-- AFTER INSERT/DELETE/UPDATE triggers keep fts in sync (full DDL in Appendix A).
```

If `fsqlite-ext-fts5` is not yet wired at implementation time, `ee-db` ships a fallback inverted-index table behind the same `queries::fts_search()` API. Switching back to native FTS5 when it lands is a one-line code change.

---

## 12. Memory lifecycle

```
┌──────────┐    ee remember       ┌──────────┐
│  Agent   │ ──────────────────▶ │ WORKING  │
└──────────┘                      └────┬─────┘
                                       │ workflow close
                                       │ (or ee promote)
                                       ▼
                                  ┌──────────┐
                                  │ EPISODIC │ ◀──  ee import cass
                                  └────┬─────┘     (sessions become
                                       │           evidence + memories)
                                       │ ee curate apply (consolidation)
                                       ▼
                                  ┌──────────┐
                                  │ SEMANTIC │
                                  └────┬─────┘
                                       │ ee curate apply (rule extraction)
                                       ▼
                                  ┌────────────┐
                                  │ PROCEDURAL │ ──── decay (90d half-life)
                                  └────────────┘     trauma-guard inverts
```

### 12.1 Creation (write path)

`ee remember "<content>" --kind fact --level episodic --tag a,b --workspace .` :

1. Resolve workspace (upward search for `.ee/`, fallback to user-DB-wide).
2. Compute `content_hash = blake3(normalize(content))` and `dedupe_hash = blake3(normalize_strict(content) + workspace_id)`.
3. Acquire advisory write lock (or send to daemon write owner).
4. Check `(workspace_id, dedupe_hash)` for existing row. If found, bump `access_count`, write `feedback_event` if a flag was provided, return existing public_id with `deduplicated: true`.
5. Insert new row with ULID public_id.
6. Insert FTS row (via trigger) and any tags.
7. Optionally enqueue an embedding job (default: lazy on first recall; `--index-now` runs synchronously).
8. Optionally auto-link to recent memories in the same workflow (Hebbian, weight 0.5, source = AUTO).
9. Append to `audit_log`.
10. Release lock.
11. Print ULID (or `--json` envelope).

### 12.2 Promotion (Working → Episodic)

When a workflow ends — by explicit `ee workflow close <id>` or by hitting TTL — all WORKING memories in that workflow either upgrade to EPISODIC or expire:

```sql
UPDATE memories
   SET level = 'EPISODIC', ttl_seconds = NULL, expires_at = NULL, updated_at = ?
 WHERE workflow_id = ? AND level = 'WORKING'
   AND (access_count >= 2 OR importance >= 0.5);
```

Conservative default; configurable.

### 12.3 Consolidation (Episodic → Semantic)

`ee curate candidates --workspace . --kind summary` runs the consolidator and produces *proposed* `Summary` memories as `curation_candidates`. The agent (or user) reviews via `ee curate validate <cand>` and applies via `ee curate apply <cand>`.

This is the **propose / validate / apply** flow that replaces the original Memory Steward's autonomous rewriting. See §16.

### 12.4 Rule extraction (Semantic → Procedural)

Same flow: `ee playbook extract --workspace .` produces `curation_candidates` with `candidate_type = rule`. Validation checks specificity, evidence, and duplication. Apply persists a new `procedural_rules` row at `Maturity::Candidate`.

### 12.5 Decay & forgetting

`ee maintenance` (or `ee steward run --job memory.decay`):
- TTL-expires WORKING memories whose `expires_at` has passed.
- Recomputes `decayed_helpful_score` / `decayed_harmful_score` / `effective_score` for every procedural rule.
- Marks rules with `effective_score < threshold` as `state = needs_review`.
- Inverts rules with `harmful_count >= 2 AND trust_score < 0.3` to anti-patterns (see §18.3).

**No memory is ever physically deleted by automation.** Hard delete is only via explicit `ee forget <id>` and respects `legal_hold`. Default forgetting is `state = retired` + `redaction_class` adjustment.

---

## 13. Hybrid retrieval pipeline

`ee` does not implement BM25, vector storage, or RRF. Frankensearch's `TwoTierSearcher` provides those primitives. `ee` wraps it with memory-specific scoring and policy.

### 13.1 Pipeline

```
              ┌──────────────────────────────────────────┐
              │  query string + filters + budget          │
              └─────────────────────┬─────────────────────┘
                                     ▼
              ┌──────────────────────────────────────────┐
              │  ee-search builds canonical doc query     │
              │  + workspace/scope/level/kind filters     │
              └─────────────────────┬─────────────────────┘
                                     ▼
              ┌──────────────────────────────────────────┐
              │  frankensearch TwoTierSearcher             │
              │  (lexical + semantic + RRF, all built-in)  │
              │  emits Phase-1 fast, then Phase-2 quality  │
              └─────────────────────┬─────────────────────┘
                                     ▼
              ┌──────────────────────────────────────────┐
              │  ee-search hydrates results from DB        │
              │  (memory rows + evidence spans + tags)     │
              └─────────────────────┬─────────────────────┘
                                     ▼
              ┌──────────────────────────────────────────┐
              │  Apply ee-specific scoring multipliers    │
              │  (recency, confidence, utility, maturity, │
              │   harmful_penalty, scope_match,           │
              │   graph_centrality, redundancy)           │
              └─────────────────────┬─────────────────────┘
                                     ▼
              ┌──────────────────────────────────────────┐
              │  Apply policy filters (redaction, legal,  │
              │  expiration, scope)                        │
              └─────────────────────┬─────────────────────┘
                                     ▼
              ┌──────────────────────────────────────────┐
              │  Optional MMR diversity                    │
              └─────────────────────┬─────────────────────┘
                                     ▼
              ┌──────────────────────────────────────────┐
              │  Top-K result list (with --explain        │
              │  showing per-component contributions)     │
              └──────────────────────────────────────────┘
```

### 13.2 Scoring formula (conceptual)

Frankensearch supplies the candidate ranking. `ee` adds memory-specific multipliers:

```
base      = frankensearch_fused_score                              // RRF over BM25 + vector
quality   = confidence × utility × maturity × recency
structure = graph_centrality × graph_neighborhood × scope_match
risk      = harmful_penalty × stale_penalty × contradiction_penalty
final     = base × quality × structure × risk
```

Each multiplier is `[0..k]`, default 1.0; agents can pass `--explain` to see per-component contributions in the output. See Appendix B for the JSON shape.

### 13.3 Recency, confidence, utility, maturity multipliers

```
recency_multiplier  = exp(-Δt_days / τ)                    // τ default 30
confidence_mult     = max(0.1, confidence)                  // 0.1 floor
utility_mult        = 0.5 + 0.5 * utility_score             // [0.5, 1.0]
maturity_mult       = match maturity {
    Candidate     => 0.5,
    Established   => 1.0,
    Proven        => 1.5,
    Deprecated    => 0.0,
    Retired       => 0.0,
}
harmful_penalty     = max(0.2, 1.0 - 0.1 * harmful_count)
```

All values configurable in `[scoring]` section of `config.toml`.

### 13.4 Speed modes

The CLI accepts `--speed` ∈ {`instant`, `default`, `quality`}:

- `instant`: lexical only, p50 ~50 ms. Use when an agent wants quick recall.
- `default`: returns Phase-1 fused results; cap ~300 ms.
- `quality`: waits for Phase-2 refinement; cap ~2s.

This maps directly onto frankensearch's `TwoTierSearcher` phases.

### 13.5 EQL-inspired JSON query schema

For programmatic access (especially MCP and HTTP):

```json
{
  "q": "release automation failed after branch rename",
  "workspace": ".",
  "levels": ["procedural", "episodic", "semantic"],
  "kinds": ["rule", "anti_pattern", "failure", "fix", "decision"],
  "tags": ["release", "git", "ci"],
  "tags_mode": "any",
  "scope": ["workspace", "repository"],
  "time": { "since": "180d" },
  "confidence": { "min": 0.4 },
  "graph": {
    "center": "mem_01HXX...",
    "hops": 2,
    "relations": ["supports", "same_error", "derived_from"]
  },
  "limit": 20,
  "speed": "default",
  "rerank": true,
  "return_subgraph": true,
  "explain": true
}
```

`ee search --query-file <path>` accepts this; the CLI flags are sugar over the same schema.

### 13.6 Answer envelope

Stable JSON in Appendix B. Every result includes `score`, `components` (per-multiplier), `provenance` (evidence span IDs and source URIs), and a `why` array of short tags (`"workspace_scope"`, `"tag_match:release"`, `"proven_rule"`, ...).

---

## 14. Knowledge graph layer

The graph is a typed directed multigraph: nodes are memories (and optionally rules, sessions, evidence spans, artifacts, workflows, actions, agents, workspaces); edges are typed relations. `franken_networkx`'s native Rust crates provide the types and algorithms.

### 14.1 Construction

```rust
use fnx_classes::DiGraph;
use fnx_algorithms as alg;
use fnx_cgse::Cgse;

pub struct MemoryGraph {
    pub g: DiGraph<MemoryId, EdgeAttrs>,
    pub built_at: DateTime<Utc>,
    pub cgse: Cgse,                  // deterministic tie-breaking
    pub graph_kind: GraphKind,       // Memory | Session | Workflow | Combined | …
}

impl MemoryGraph {
    pub fn build_from_db(conn: &FrankenConnection, scope: GraphScope) -> Result<Self> {
        // Fetch nodes within scope; add to g.
        // Fetch links within scope; add edges.
        // For undirected relations (CoTag/CoMention), insert reverse edge too.
    }

    pub fn pagerank(&self, conn: &FrankenConnection) -> Result<HashMap<String, f32>> {
        if let Some(snap) = load_fresh_snapshot(conn, self.scope_hash(), "pagerank")? {
            return Ok(snap);
        }
        let ranks = alg::pagerank_with_cgse(&self.g, 0.85, &self.cgse);
        save_snapshot(conn, self.scope_hash(), "pagerank", &ranks)?;
        Ok(ranks)
    }
}
```

### 14.2 Algorithms exposed

| Subcommand | Algorithm | Use |
|---|---|---|
| `ee graph pagerank` | `alg::pagerank` | Memory salience |
| `ee graph betweenness` | `alg::betweenness_centrality` | Bridge memories |
| `ee graph louvain` | `alg::louvain_communities` | Topic clusters |
| `ee graph k-core --k 3` | `alg::k_core` | Tightly-connected memory groups |
| `ee graph articulation` | `alg::articulation_points` | Cut vertices |
| `ee graph hits` | `alg::hits` | Hubs / authorities |
| `ee graph path <src> <dst>` | `alg::shortest_path` | Trace `DerivedFrom` chains |
| `ee graph neighborhood <id> --hops 2 --relation Supports` | BFS | Local subgraph for context |
| `ee graph explain-link <src> <dst>` | path + scoring | Why are these linked? |

### 14.3 Determinism

Always pass `fnx-cgse` as the tie-breaking comparator. Same DB → same metrics. Required for `--audit-hash` stability.

### 14.4 Graph features for retrieval

| Feature | Source |
|---|---|
| `centrality_score` | PageRank |
| `authority_score` / `hub_score` | HITS |
| `community_id` | Louvain |
| `distance_to_query_seed` | Shortest path from a seed memory |
| `same_cluster_as_top_result` | Boolean from community detection |
| `evidence_support_count` | Count of `Supports` edges |
| `contradiction_count` | Count of `Contradicts` edges |
| `orphan_penalty` | Inverse degree |
| `stale_bridge_penalty` | Bridge with old `last_reinforced_at` |

These features are explainable and optional. Search must continue if graph snapshots are stale; `ee why` shows whether graph features contributed.

### 14.5 Graph jobs

`graph.refresh.workspace`, `graph.refresh.global`, `graph.compute.centrality`, `graph.compute.communities`, `graph.compute.link_candidates`, `graph.detect.orphans`, `graph.detect.contradictions`. Each writes a `graph_snapshots` row with algorithm versions, node/edge counts, witness hash, duration.

### 14.6 Auto-links

After every `ee maintenance`:
- **CoTag**: pairs sharing ≥2 tags get a `CoTag` edge (undirected, weight = `0.3 × (shared_tag_count - 1)`).
- **CoMention**: pairs both linking to the same `Entity` memory get `CoMention`.
- **Hebbian reinforce**: edges traversed during recent recalls get `weight += 0.05` and `evidence_count += 1`, capped at 1.0.

Auto-links are written with `source = AUTO` so they can be GC'd when underlying memories change.

---

## 15. Session ingestion via `cass`

The original Eidetic Engine had no awareness of agent harnesses' on-disk session formats. `cass` already solved that problem.

`cass` exposes no Rust library. We integrate via two paths:

### 15.1 CLI subprocess (read-time)

For ad-hoc lookups during agent work:

```bash
ee recall-session "panic in serde_json" --agent claude --days 7
# Internally:
#   cass search "panic in serde_json" --robot --robot-meta --agent claude --days 7
#   parse stdout JSON
#   render in ee's hit format
```

Use asupersync `process::*` so cancellation, timeouts, and reaping behave correctly. Never call bare interactive `cass` (it launches a TUI).

### 15.2 Direct DB read (bulk import)

For bulk ingestion (`ee import cass`):

```rust
let cass_db = FrankenConnection::open(&cass_db_path)?;
let conversations = sqlmodel::query!(
    "SELECT * FROM conversations WHERE workspace = ?1",
    &[Value::Text(workspace.into())]
).all(cx, &cass_db).await?;

for c in conversations {
    let key = format!("cass:{}:{}:{}", c.agent_slug, c.id, c.import_hash);
    if idempotency_seen(key)? { continue; }

    let session = Session::from_cass(&c);
    let messages = fetch_messages_for_session(&cass_db, &c.id).await?;

    insert_session(&our_db, &session, &messages).await?;

    // Create evidence spans pointing back into cass
    for m in &messages {
        let span = EvidenceSpan {
            source_type: SourceType::CassMessage,
            source_uri: format!("cass://{}:{}:{}", c.agent_slug, c.id, m.idx),
            excerpt: extract_compact_excerpt(&m.content),
            ...
        };
        insert_evidence_span(&our_db, &span).await?;
    }

    // Optionally: heuristic auto-memorization of "important" turns
    if opts.auto_memorize {
        for m in &messages {
            if turn_looks_important(m) {
                let memory = derive_episodic_memory(m, &session);
                insert_memory(&our_db, &memory).await?;
                link_evidence(&our_db, memory.id, evidence_for(m).id).await?;
            }
        }
    }

    advance_import_ledger(&our_db, key)?;
}
```

`turn_looks_important` heuristics:
- Tool call with non-zero exit code.
- Long assistant message (>500 tokens) following a "why did that fail" prompt.
- Conventional markers: `# Lesson learned`, `# Decision:`, `# TIL:`.
- High-token-cost message (heavy thinking-mode response).

### 15.3 The `ee import cass` subcommand

```
ee import cass [--workspace PATH] [--days N] [--agent <slug>] [--auto-memorize]
                [--limit N] [--batch-size 200] [--dry-run] [--json]
```

Idempotent. Run twice, no duplicate rows. Resumable via `import_ledger`.

### 15.4 Live ingestion (post-v1)

`ee daemon` can use inotify to watch agent session directories and auto-import new turns. Out of scope for v1; agents call `ee import cass` themselves at session end (often via the Stop hook).

---

## 16. Curation, consolidation, and review

The original Memory Steward's autonomous rewriting is replaced by an explicit **propose → validate → apply** flow. All durable promotions go through `curation_candidates`.

### 16.1 The flow

```
Episodic memories ──┐
Diary entries     ──┼─▶ proposer (consolidator / extractor / autolinker)
Imported sessions ──┘                      │
                                            ▼
                                 curation_candidates (status=Pending)
                                            │
                              ee curate validate <id>
                                            ▼
                                 status=Ok | Warning | Rejected
                                            │
                              ee curate apply <id>
                                            ▼
                                  Persisted memory / rule / link
                                  audit_log entry written
```

### 16.2 Consolidation as a proposer

`ee curate candidates --kind summary --workspace .` (called manually, or scheduled via daemon):

1. Pull EPISODIC memories within scope.
2. Embed missing ones (frankensearch fast tier).
3. Cluster: single-link agglomerative on cosine similarity, threshold 0.78 (configurable). Deterministic given fixed input order + cgse tie-breaker.
4. For each cluster of size ≥3:
   a. Concatenate cluster-member content with separators.
   b. Summarize: extractive (top-N TF-IDF sentences with position bias) by default; LLM-driven if `--llm anthropic` is configured.
   c. Emit a `curation_candidate` with `candidate_type = memory`, `proposed_payload_json` containing the new SEMANTIC `Summary` memory plus `DerivedFrom` link payloads.
5. Auto-link pass over the same scope (CoTag, CoMention, Hebbian) emits `candidate_type = link` candidates.

Nothing is persisted yet.

### 16.3 Rule extraction as a proposer

`ee curate candidates --kind rule --workspace .` (or `ee playbook extract`):

1. Analyze recent SEMANTIC memories.
2. For each, emit candidate rules using either pattern extraction (sentences containing "always", "never", "prefer", "avoid", "make sure to", "we found that") or LLM extraction.
3. Curate: search existing playbook for near-duplicates. If found, propose `feedback_event(kind=Confirmed)` instead of new rule.
4. Emit `curation_candidate` with `candidate_type = rule`.

### 16.4 Validation

`ee curate validate <cand_id>`:

- **Specificity**: rule mentions concrete tools, files, branches, commands? (Vague rules get a warning.)
- **Evidence**: `source_evidence_id` resolves? (Missing evidence → reject.)
- **Duplication**: similar rule already exists in same scope? (Warn; suggest merge.)
- **Scope sanity**: scope/scope_key well-formed?
- **Redaction**: content scanned for secrets (see §22.2).

Output: `validation_status ∈ {Ok, Warning, Rejected}` plus `validation_warnings_json`.

### 16.5 Apply

`ee curate apply <cand_id>`:

- Re-runs validation.
- Persists the proposed payload(s) in a single transaction.
- Writes `audit_log` row with `before_hash` (null for new) and `after_hash`.
- Enqueues index jobs and link auto-detect.
- Sets `applied_at`, `reviewed_by`.

### 16.6 `ee review session`

Agent-native review:

```bash
ee review session --cass-session <id> --propose --json
```

Loads session metadata + snippets from cass, identifies decisions / failures / fixes / recurring commands / project conventions / possible anti-patterns, and emits **proposed candidates**. The agent inspects them, optionally edits, and applies.

For v1 this is partially agent-native: `ee` gathers structured evidence; the calling agent writes proposed summaries/rules into the candidate; `ee curate validate` quality-checks; `ee curate apply` persists. This avoids requiring paid LLM APIs inside `ee`.

---

## 17. Procedural memory & playbooks

The data model lives in `procedural_rules` (§11.6). The on-disk artifact `playbook.yaml` is a derived export.

### 17.1 The bullet schema (YAML serialization)

```yaml
# .ee/playbook.yaml — generated from procedural_rules; safe to hand-edit.
schema_version: 1
metadata:
  generated_at: "2026-04-29T03:00:00Z"
  total_rules: 23
  total_anti_patterns: 4
deprecated_patterns:
  - id: "rule_01HXX..."
    category: "concurrency"
    content: "AVOID: never call BEGIN inside a sqlmodel transaction; results in nested-tx panic"
    confidence: 0.31
    harmful_count: 3
bullets:
  - id: "rule_01HXX..."
    category: "migrations"
    content: "When adding a NOT NULL column to a large table, prefer a 3-step migration."
    rationale: "Atomic ALTER on large tables locks reads in WAL mode."
    confidence: 0.74
    maturity: "established"
    helpful_count: 8
    harmful_count: 1
    last_validated_at: "2026-04-25T14:08:13Z"
    decay_half_life_days: 90
    workflow_scope: null
```

### 17.2 Maturity transitions

```
candidate ─(helpful_count ≥ 3 ∧ confidence ≥ 0.65)──▶ established
established ─(helpful_count ≥ 8 ∧ confidence ≥ 0.85 ∧ harmful_count = 0)──▶ proven
*           ─(harmful_count ≥ 2 ∧ trust < 0.3)──────▶ deprecated  (anti-pattern)
deprecated  ─(no use, decayed_score < 0.05)─────────▶ retired
*           ─(replaced_by_rule_id set)──────────────▶ tombstoned
```

### 17.3 `ee outcome` — generalized feedback

Feedback isn't restricted to playbook bullets; any memory can accept it:

```
ee outcome --memory <id>      --helpful "saved 30 min on the migration"
ee outcome --rule   <id>      --harmful "rule didn't apply, broke the cache layer"
ee outcome --pack   <pack-id> --helpful "this context was exactly what I needed"
```

Each call inserts a `feedback_events` row, increments target counters, sets `last_marked_at`, recomputes `decayed_*` and `effective_score`, and may transition maturity.

### 17.4 `ee context` — the before-work primitive

```
ee context "<task>" [--budget 4000] [--scope global|workspace] [--format json|markdown]
```

Returns:
- `relevant_bullets` (procedural rules ranked by retrieval pipeline + maturity multiplier)
- `anti_patterns` (deprecated rules that match scope, pinned even if low score)
- `history_snippets` (top similar prior sessions from `cass`-imported `evidence_spans`)
- `chunks` (general semantic/episodic context up to budget)
- `suggested_queries` (`ee search` and `cass search` invocations to dig deeper)
- `pack_hash` (BLAKE3 of canonical selection)
- `degraded` (any subsystems that didn't contribute, with reason codes)

This is the single most valuable agent-facing call. Agents run it as the first action of every coding task.

---

## 18. Trauma guard & confidence decay

Borrowed wholesale from `cass_memory_system`. Lives in `ee-curate`.

### 18.1 Confidence decay

```rust
fn decayed_helpful_score(r: &ProceduralRule, now: DateTime<Utc>) -> f32 {
    let last = r.last_validated_at.unwrap_or(r.created_at);
    let days = (now - last).num_seconds() as f32 / 86_400.0;
    let half_life = r.decay_half_life_days.max(1.0);
    (r.helpful_count as f32) * 0.5_f32.powf(days / half_life)
}

fn decayed_harmful_score(r: &ProceduralRule, now: DateTime<Utc>) -> f32 {
    // Harmful evidence decays slower, so it weighs more heavily over time.
    let last = r.last_validated_at.unwrap_or(r.created_at);
    let days = (now - last).num_seconds() as f32 / 86_400.0;
    let half_life = (r.decay_half_life_days * 1.5).max(1.0);
    (r.harmful_count as f32) * 4.0 * 0.5_f32.powf(days / half_life)
}

fn effective_score(r: &ProceduralRule, now: DateTime<Utc>) -> f32 {
    let h = decayed_helpful_score(r, now);
    let n = decayed_harmful_score(r, now);
    let m = maturity_multiplier(r.maturity);
    ((h - n) * m).max(0.0)
}
```

Run via `ee maintenance` or as a step inside `ee context` (so even read-only calls update decayed values without writing). Write-back when the recomputed value diverges from the persisted one by more than 0.05.

### 18.2 Trust score (CASS rule)

```rust
fn trust_score(r: &ProceduralRule) -> f32 {
    let helpful = r.helpful_count as f32;
    let harmful = r.harmful_count as f32 * 4.0;        // CASS multiplier
    let total = helpful + harmful;
    if total == 0.0 { 0.5 } else { helpful / total }
}
```

`trust_score < 0.4` is yellow (shown but flagged). `trust_score < 0.2` is red (proactive deprecation suggested).

### 18.3 Auto-inversion to anti-pattern

In `ee maintenance`:

```rust
for r in active_rules {
    if r.harmful_count >= 2 && trust_score(&r) < 0.3 {
        // Update procedural_rules row
        r.state = RuleState::Deprecated;
        r.maturity = RuleMaturity::Deprecated;
        save(&r)?;

        // Update the linked memory
        let mut mem = load_memory(r.memory_id)?;
        mem.kind = MemoryKind::AntiPattern;
        mem.content = format!("AVOID: {}", mem.content);
        mem.updated_at = now;
        save(&mem)?;

        // Record in audit_log
        write_audit("rule.invert_to_anti_pattern", r.public_id, before_hash, after_hash)?;
    }
}
```

### 18.4 Trauma records

High-severity catastrophic mistakes (destructive git commands, cloud-resource deletion, DB drop/truncate, secret leakage, wrong-branch release) get `affect_valence < -0.5` and `importance >= 0.9`. The retrieval pipeline pins matching trauma records into context packs even when they don't dominate semantic similarity — Scenario 5 from §4.

---

## 19. Deterministic context packing

The agent says: "give me the most useful context for `<task>` in 4000 tokens." `ee` returns a packed bundle that is:

- Within budget.
- Diverse (MMR penalty).
- Reproducible (same query + same seed + same DB snapshot → byte-identical).
- Auditable (every chunk annotated with memory id, score, rank).
- Inspectable later (`pack_records` row).

### 19.1 Pack sections

Default sections (configurable):
1. Task interpretation
2. High-priority rules (procedural, scope-matched)
3. High-risk anti-patterns (pinned even if low score)
4. Project conventions (procedural, scope-matched)
5. Similar prior sessions (from `cass`-imported `evidence_spans`)
6. Relevant decisions and facts (semantic / episodic)
7. Related files and artifacts
8. Suggested searches (`ee search …` and `cass search …`)
9. Provenance and explanation
10. Degradation warnings

### 19.2 Type quotas

Default percentages (soft; unused budget reallocates):

```toml
[pack.quotas]
procedural_rules     = 0.25
anti_patterns        = 0.15
similar_sessions     = 0.20
facts_and_decisions  = 0.20
artifacts_and_files  = 0.10
provenance           = 0.10
```

### 19.3 Packer algorithm

```rust
pub fn pack_context(
    cx: &Cx, query: &Query, budget: usize, cfg: &PackConfig,
) -> Outcome<ContextBundle> {
    let cands_per_section = retrieval::candidates_by_section(cx, query, cfg)?;
    let mut picked = Vec::new();
    let mut spent = 0;
    let mut quotas = cfg.quotas.clone();

    // 1. Pin: critical warnings + proven anti-patterns matching query scope
    for cand in trauma_pins(&cands_per_section) {
        let tokens = tiktoken::count(&cand.content);
        if spent + tokens > budget { break; }
        picked.push(cand); spent += tokens;
    }

    // 2. Per-section selection with MMR diversity
    for (section, cands) in cands_per_section {
        let section_budget = quotas.budget_for(section, budget);
        for cand in cands {
            if spent >= budget || picked.len_in(section) * tokens > section_budget { break; }
            let redundancy = max_cosine_sim(&cand, &picked);
            let mmr = cfg.lambda * cand.score - (1.0 - cfg.lambda) * redundancy;
            if mmr < cfg.min_mmr { continue; }
            let tokens = tiktoken::count(&cand.content);
            if spent + tokens > budget { continue; }
            picked.push(cand); spent += tokens; quotas.consume(section, tokens);
        }
    }

    let bundle = ContextBundle {
        chunks: picked,
        tokens_used: spent,
        budget,
        seed: cfg.seed,
        audit_hash: blake3_canonical(&picked),
    };
    save_pack_record(&bundle)?;
    Outcome::Ok(bundle)
}
```

### 19.4 Audit hash

`audit_hash = blake3(canonical_json([(public_id, content_hash, score, rank), ...]))`. Two `ee pack` calls with identical query, budget, config, seed, and DB content produce identical hashes. CI tests assert this.

### 19.5 Output formats

- `json` — stable machine output (canonical contract; see Appendix B).
- `markdown` — agent-readable; renders sections with provenance footnotes.
- `toon` — compact structured text, if the project standardizes on it.
- `summary` — terse human terminal format.

JSON is the contract. Other formats render from the same `ContextPack` struct.

---

## 20. CLI surface

Every command supports `--json` and `--quiet`. Subcommands group naturally.

### 20.1 Top-level

```
ee 0.1.0 — eidetic engine memory substrate for AI coding agents

USAGE:
    ee <COMMAND>

COMMANDS:
    init               Initialize ee for current workspace (writes .ee/config.toml; idempotent)
    status             Effective config, DB state, degraded capabilities
    doctor             Health check; --fix-plan emits non-destructive repair commands
    context            Before-work primitive (rules + anti-patterns + history + chunks)
    search             Hybrid search across memories / sessions / evidence
    remember           Store a memory
    outcome            Record helpful/harmful feedback for a memory, rule, or pack
    why                Explain why a memory exists / was retrieved / was packed
    import
        cass           Import sessions from coding_agent_session_search
        jsonl          Import a JSONL export
    review
        session        Review a cass session and emit candidates (--propose)
        workspace      Review all sessions in current workspace and emit candidates
    curate
        candidates     List pending candidates
        validate       Validate a candidate (specificity / evidence / duplication / redaction)
        apply          Persist a validated candidate
        retire         Retire a memory or rule
        tombstone      Tombstone a memory or rule with replacement
    rule
        list           List rules
        show           Show a rule
        add            Add a rule directly (manual; bypasses curation flow)
        update         Update a rule
        mark           Same as `outcome --rule`
    memory
        show           Show a memory
        list           List memories
        link           Manually create a memory_link
        tags           Manage tags
        expire         Expire a memory (set ttl)
    playbook
        list           Same as `rule list --format=playbook`
        export         Export procedural rules to playbook.yaml
        import         Import edits from playbook.yaml back into the DB
        extract        Run rule extraction proposer (emits curation_candidates)
    pack               Build a context pack from an EQL JSON query file
    graph
        refresh        Recompute graph snapshots
        neighborhood   Local subgraph around a memory
        centrality     PageRank / HITS / etc.
        communities    Louvain / label propagation
        path           Shortest path between two memories
        explain-link   Why are these two memories linked?
    index
        status         Index manifest, generation, freshness
        rebuild        Rebuild from DB
        vacuum         Compact / GC
    db
        status         DB version, integrity, migrations applied
        migrate        Run pending migrations
        check          Integrity check
        backup         Snapshot DB (and optionally indexes) to a path
    export
        jsonl          Export memories / rules / sessions to JSONL
    eval
        run            Run evaluation fixture(s); emit metrics
        report         Render evaluation results (markdown)
    workflow
        create / list / close
    daemon             Foreground supervisor (post-v1)
    serve              HTTP+SSE server (post-v1, feature-gated)
    mcp                MCP stdio server (feature-gated)
    completion         Generate shell completions

GLOBAL OPTIONS:
    --workspace <PATH>   Override workspace (default: cwd, then upward search for .ee/)
    --config <PATH>      Override config file
    --db <PATH>          Override DB path
    --json               Stable machine-readable JSON output
    --format <FMT>       human | json | markdown | toon
    --quiet              Suppress humanized output
    --verbose / -v       Increase log verbosity
    --no-color           Disable terminal colors
    --trace              Full tracing to stderr
    -V, --version        Version + build metadata
```

### 20.2 The first-ten commands every agent will touch

```
ee init
ee remember "<content>" [--kind fact] [--level episodic] [--tag a,b] [--workspace .]
ee search "<query>" [--limit 10] [--level …] [--kind …] [--tags …] [--speed instant|default|quality] [--explain]
ee context "<task>" [--max-tokens 4000] [--scope workspace] [--format markdown]
ee outcome --memory <id> --helpful "<reason>"      # or --rule, --pack, --harmful
ee why <id>                                        # memory, rule, result, or pack
ee import cass --workspace . --since 30d
ee review session --cass-session <id> --propose
ee curate apply <cand-id>
ee doctor [--fix-plan]
```

### 20.3 Output rules

- JSON to stdout. Human diagnostics to stderr. Never mix.
- `--json` output is parseable and stable per §Appendix B.
- Long-running commands use stderr progress bars only when attached to a TTY.

### 20.4 Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Usage error (bad args, missing file) |
| 2 | Configuration error |
| 3 | Storage error (DB lock, integrity) |
| 4 | Search/index error |
| 5 | Import error |
| 6 | Degraded but command could not satisfy required mode |
| 7 | Policy denied operation (redaction, scope) |
| 8 | Migration required |
| 130 | Cancelled (SIGINT) |

### 20.5 Stable JSON schema versioning

Every `--json` output has `schema: "ee.<noun>.v<N>"` and `version: <int>`. When we evolve a contract we bump `v<N>` and document the change. Agents pin to a version they understand. Old versions remain available behind a flag for one full minor-version cycle.

---

## 21. Diagnostics, repair, and `ee why`

Memory tools fail in subtle ways: stale indexes, missing cass data, redaction policy, low-quality rules, old graph snapshots, locked DBs, and degraded semantic models can all produce plausible-but-incomplete answers. `ee` makes diagnosis a first-class command surface.

### 21.1 Diagnostic principles

- Every degraded result says **what** degraded and **why**.
- Every repair suggestion names a concrete command.
- Every long-running job leaves a durable record.
- Every context pack is inspectable after generation (`ee pack show`).
- Every index has a manifest explaining how it was built.
- Every graph snapshot names its source data and algorithm versions.
- Every import is resumable and explains its cursor.
- Human diagnostics → stderr; JSON diagnostics → stdout when `--json`.

### 21.2 Core diagnostic commands

```bash
ee status --json
ee doctor --json
ee doctor --fix-plan --json     # never mutates by default
ee index status --json
ee graph status --json
ee job list --json
ee job show <id> --json
ee pack show <pack-id> --json
ee why <result-or-id> --json
```

`ee doctor --fix-plan` returns an ordered, non-destructive checklist. It does not run anything until `--apply` (or per-step manual approval).

### 21.3 Health checks

| Check | Detects | Suggested repair |
|---|---|---|
| DB opens | missing/corrupt DB | `ee db check`, restore backup, or reinitialize |
| migrations current | schema drift | `ee db migrate` |
| cass available | missing session source | install cass or set `EE_CASS_BIN` |
| cass healthy | stale or broken cass index | `cass health --json`, `cass index --full` |
| search index manifest | stale / incompatible | `ee index rebuild` |
| pending index jobs | lagging retrieval | `ee steward run --job index.process` |
| graph snapshot freshness | stale graph boosts | `ee graph refresh` |
| redaction policy | unsafe stored excerpts | `ee steward run --job privacy.audit` |
| daemon lock | stuck writer or worker | inspect job; restart daemon if safe |
| config conflicts | surprising settings | show config source + effective value |
| forbidden deps | accidental tokio / rusqlite | inspect feature tree |

### 21.4 Stable degradation codes

Every degraded JSON response includes a `degraded[]` array of objects with `code`, `severity`, `message`, `subsystem`, `still_useful`, `repair_command`. Stable codes:

```
cass_unavailable
cass_unhealthy
semantic_disabled
semantic_model_missing
search_index_stale
search_index_missing
graph_snapshot_stale
graph_disabled
db_lock_contention
privacy_redaction_applied
policy_denied_excerpt
budget_exhausted
job_queue_backlog
embedder_offline
fts_extension_missing
```

### 21.5 `ee why`

The trust command:

```bash
ee why <memory-id>                  # why does this exist? where from?
ee why <rule-id>                    # what evidence supports this?
ee why result:<doc-id>              # why was this retrieved? score components?
ee why pack:<pack-id>               # what was selected and why?
```

It answers:
- Why this memory exists (source, original session, evidence span).
- Why it was retrieved (per-component scores).
- What links / graph metrics matter.
- What rules or policies would hide / demote it.
- Whether newer evidence contradicts it.

### 21.6 Repair UX

```json
{
  "schema": "ee.doctor.fix_plan.v1",
  "repairs": [
    {
      "id": "repair_index_rebuild",
      "severity": "medium",
      "reason": "Search index generation 2 is older than DB generation 3.",
      "command": "ee index rebuild --workspace .",
      "destructive": false,
      "estimated_duration_seconds": 45
    }
  ]
}
```

Rules:
- No hidden destructive repair.
- No deletion-based cleanup in v1.
- No automatic remote model download unless explicitly configured.
- No automatic migration without explicit `ee db migrate`.
- No mutation in `doctor` unless `--apply` is passed.

### 21.7 Audit events

`audit_log` and `steward_jobs` capture: command started/completed/cancelled/failed, job started/completed/cancelled/panicked, migration applied, import advanced cursor, index manifest changed, graph snapshot created, context pack emitted, memory written, memory redacted, rule promoted, rule demoted.

---

## 22. Privacy, redaction, and safety

`ee` stores selected excerpts of agent sessions. That includes file contents, command outputs, and conversation turns. Without privacy discipline, secrets leak.

### 22.1 Redaction classes

```
public      — safe to share broadly; OK in JSONL exports
project     — safe to commit to project Git
private     — safe locally; NOT in exports without --include-private
secret      — never in context packs; never sent to remote models
blocked     — never stored at all; only the URI pointer is kept
```

### 22.2 Secret detection

Initial scanners run during import, before storing manual memory, before emitting context packs, and during `privacy.audit`:

- API keys (AWS, GCP, Anthropic, OpenAI, GitHub PAT, Stripe, etc.) — pattern + entropy heuristics
- Private keys (`-----BEGIN [...] PRIVATE KEY-----`)
- Bearer/JWT tokens
- Passwords (in URLs, env-var assignments)
- `.env` content
- Cloud credential files
- Database URLs with embedded credentials
- High-entropy hex/base64 strings ≥ 32 chars in suspicious contexts

When a scanner triggers:
- Storage is **denied** for `secret`+ class by default.
- Or excerpt is replaced with `[REDACTED:<scanner_name>]` and `redaction_class = secret`.
- An audit entry records the detection.

### 22.3 Risk memories

High-severity memories are explicitly labeled (`affect_valence < -0.5`, `importance >= 0.9`) and pinned in context packs when relevant. Examples:

- destructive git command caused loss
- wrong branch released
- wrong account billed
- secret leaked
- DB migration corrupted data

### 22.4 Data deletion

V1 prefers tombstone / retire / expire / redact / hide-from-packs over physical deletion. If physical deletion is added later, it requires explicit confirmation and produces an audit record.

### 22.5 Remote-model policy

If `[llm]` is configured, `ee consolidate --llm anthropic` (or similar) sends content to a remote model. Policy:

- `allow_remote_models = false` in `[privacy]` blocks all remote calls.
- Content with `redaction_class >= private` is never sent to remote models.
- Every remote call is audit-logged (timestamp, model, tokens, target, response hash).

---

## 23. Agent hook integration

The point of `ee` is that agents lean on it. Three integration shapes, in increasing investment.

### 23.1 Bash subprocess (zero-config)

Default. Any harness with shell-tool support uses `ee` directly. Suggested AGENTS.md / CLAUDE.md boilerplate:

```markdown
## ee — durable memory

Before non-trivial work, run `ee context "<your task>" --workspace . --format markdown`.
Save important findings with `ee remember "<content>" --kind fact --workspace .`.
Mark feedback after applying a rule: `ee outcome --rule <id> --helpful "<reason>"`.
At session end: `ee import cass --workspace . --days 1 --auto-memorize`.
```

### 23.2 Claude Code hooks

`.claude/settings.json` hooks let us automate three things:

- **`SessionStart`** — print "Run `ee context "<task>"` first" as a system hint.
- **`Stop`** — run `ee maintenance && ee import cass --workspace . --days 1 --auto-memorize` (opt-in).
- **`PreToolUse`** on Bash — optionally inject relevant `ee context` warnings if the command matches a known anti-pattern (advisory, never blocking).

`ee init` offers to write a starter `.claude/settings.local.json` interactively.

### 23.3 MCP server (`ee mcp`)

Feature-gated. Exposes the same operations as MCP tools using `rust-mcp-sdk` over stdio. Tool list: `ee.context`, `ee.search`, `ee.remember`, `ee.outcome`, `ee.curate_candidates`, `ee.curate_apply`, `ee.memory_show`, `ee.why`, `ee.graph_neighborhood`. Schemas derive from the same clap structs as the CLI so we never have two contracts.

### 23.4 HTTP+SSE server (`ee serve`)

Feature-gated. Localhost-only by default. JSON over POST + SSE for streamed pack/recall results. Auth arrives in v0.2 if remote use is contemplated.

### 23.5 In-process Rust library (`ee_core`)

`ee_core::{remember, search, context, pack, link, curate, why}` for embedding. No subprocess, no network.

---

## 24. Optional MCP server mode

Already covered in §23.3. JSONSchema for tools is derived from clap+`schemars` so changes propagate. MCP server has **no** business logic of its own — it's a thin adapter over the same `ee_core` calls the CLI makes. Output schemas mirror CLI JSON. Asupersync `process::*` for stdio; no tokio.

---

## 25. Configuration

Three precedence layers (later wins):

1. Compiled defaults.
2. `~/.config/ee/config.toml` (user).
3. `<workspace>/.ee/config.toml` (project).
4. Environment variables `EE_*`.
5. CLI flags.

### 25.1 Config shape

```toml
schema_version = 1

[storage]
database_path     = "~/.local/share/ee/ee.db"
index_dir         = "~/.local/share/ee/indexes"
jsonl_export      = false
page_size         = 8192
journal_mode      = "WAL"
write_lock_path   = "~/.local/share/ee/ee.db.write-lock"

[runtime]
daemon            = false
job_budget_ms     = 5000
import_batch_size = 200

[search]
mode              = "hybrid"              # hybrid | lexical
default_speed     = "default"             # instant | default | quality
max_results       = 50
index_generation  = 1

# NOTE: embedding model selection lives in frankensearch's defaults; ee does not
# override it. To change models, configure frankensearch.

[scoring]
recency_tau_days       = 30
helpful_half_life_days = 90
harmful_multiplier     = 4.0
candidate_multiplier   = 0.5
established_multiplier = 1.0
proven_multiplier      = 1.5
deprecated_multiplier  = 0.0

[pack]
default_max_tokens     = 4000
mmr_lambda             = 0.7
min_mmr                = -1.0
include_explanations   = true
[pack.quotas]
procedural_rules    = 0.25
anti_patterns       = 0.15
similar_sessions    = 0.20
facts_and_decisions = 0.20
artifacts_and_files = 0.10
provenance          = 0.10

[playbook]
decay_half_life_days        = 90.0
trauma_multiplier           = 4.0
inversion_threshold_harmful = 2
inversion_threshold_trust   = 0.3

[curate]
similarity_threshold = 0.78
min_cluster_size     = 3
auto_propose         = false              # propose without explicit ee curate candidates
extract_facts        = false

[graph]
enabled               = true
snapshot_ttl_seconds  = 600
refresh_after_import  = false
max_hops_default      = 2

[cass]
enabled       = true
binary        = "cass"
db_path       = "~/.local/share/cass/cass.db"
default_since = "90d"

[privacy]
store_secret_excerpts = false
redact_by_default     = true
allow_remote_models   = false

[hooks]
on_stop_run_maintenance = true
on_stop_import_cass     = false

[llm]
provider          = "none"                # "none" | "anthropic"
model             = "claude-haiku-4-5"
api_key_env       = "ANTHROPIC_API_KEY"
max_tokens_per_call = 4000

[evaluation]
fixture_dir = "tests/fixtures/eval"
```

### 25.2 Environment variables

`EE_CONFIG`, `EE_DB`, `EE_INDEX_DIR`, `EE_WORKSPACE`, `EE_JSON`, `EE_NO_SEMANTIC`, `EE_CASS_BIN`, `EE_LOG`.

### 25.3 `ee config` subcommand

```
ee config show                  # effective config (merged); show source per key
ee config get scoring.recency_tau_days
ee config set scoring.recency_tau_days 7  # writes to workspace config.toml
ee config edit                  # opens $EDITOR
ee config validate              # schema check + value sanity
```

---

## 26. On-disk layout

```
~/.local/share/ee/
├── ee.db                        # canonical user DB (frankensqlite)
├── ee.db-wal                    # WAL
├── ee.db-shm                    # shared memory
├── ee.db.write-lock             # advisory write lock (CLI-only mode)
├── indexes/
│   ├── memory/                  # frankensearch FSVI + manifest
│   ├── session/
│   ├── artifact/
│   └── tantivy/                 # BM25 if lexical feature enabled
├── audit.jsonl                  # append-only audit (rotates monthly)
└── steward.log                  # daemon log when running

<workspace>/.ee/                 # OPTIONAL per-workspace overrides
├── config.toml                  # workspace-level config
├── playbook.yaml                # exported view of procedural_rules; safe to commit
├── playbook.yaml.bak            # rolling backup
├── memory.jsonl                 # optional Git-friendly export
└── README.txt                   # "managed by ee; safe to .gitignore most files"
```

**Gitignore policy.** `ee init` offers to add `.ee/playbook.yaml.bak` and `.ee/memory.jsonl` to `.gitignore`; `playbook.yaml` and `config.toml` are left committed (they're the human-curated bits).

---

## 27. Testing strategy & evaluation harness

Five layers.

### 27.1 Unit tests (`#[cfg(test)]`)

In every module. Notable:
- `ee-models`: enum round-trips, ID parsing.
- `ee-db::repositories`: typed CRUD round-trips through serde + sqlmodel.
- `ee-search::fuse`: scoring composition correctness on hand-crafted candidates.
- `ee-curate::cluster`: single-link clustering on synthetic similarity matrices.
- `ee-curate::decay`: decay formula at boundary times (0×, 1×, 2×, 10× half-life).
- `ee-curate::trauma`: inversion edge cases.
- `ee-pack::mmr`: packing under tight budgets, type quotas, ties.
- `ee-pack::hash`: same input → same hash invariant.
- `ee-graph::metrics`: PageRank/betweenness on tiny known graphs (karate-club).
- `ee-policy::redact`: secret detection on a fixture corpus.

### 27.2 Integration tests (`tests/`)

Each test starts with a temp DB, runs `ee init`, executes a sequence of CLI calls, asserts on JSON output:

- `tests/end_to_end_remember_search.rs`
- `tests/curate_candidates_propose_validate_apply.rs`
- `tests/playbook_decay_inversion.rs`
- `tests/import_cass_idempotent.rs` (uses fixture cass DB)
- `tests/pack_audit_hash_stable.rs`
- `tests/graceful_degradation_no_cass.rs`

### 27.3 Deterministic runtime tests (asupersync `LabRuntime`)

Required invariants:
- No orphan tasks
- No forgotten reply obligations
- No leaked resource checkouts
- Region close implies quiescence
- Race losers drain or are explicitly safe to abandon
- Shutdown follows request → stop → drain → finalize
- Futurelock detection catches stuck steward jobs
- Fixed seeds reproduce lab failures

Labs cover: cancellation during import, indexing, steward jobs, daemon supervisor restart, concurrent-read-while-write-queued, cancellation during cass subprocess, cancellation during DB tx boundaries, cancellation during search index rebuild, cancellation during graph snapshot construction, region quiescence after command completion.

### 27.4 Golden snapshot tests (`insta`)

For human-readable output we don't want to change accidentally:
- `ee status --json`
- `ee context --json` (fixture inputs)
- `ee context --format markdown` (fixture inputs)
- `ee search --json --explain`
- curation candidate JSON
- migration status
- `ee why` output

Rules: deterministic IDs in fixtures, fixed timestamps, fixed scoring constants, stable result ordering, explicit schema versions.

### 27.5 Memory evaluation harness

Technical tests prove `ee` runs. Evaluation tests prove it **helps**. The harness:

```bash
ee eval run --fixture release_failure --json
ee eval run --all --json
ee eval report --format markdown
```

Each fixture is a self-contained directory:

```
tests/fixtures/eval/release_failure/
├── seed.sql               # initial DB state
├── cass_fixture.db        # tiny cass DB to import
├── queries.json           # context/search queries to run
├── expected.json          # expected memory IDs, sections, degradations
└── README.md              # what this fixture proves
```

Initial fixture families:

| Fixture | What it proves |
|---|---|
| `release_failure` | Prior release mistakes surface before release work |
| `async_migration` | Asupersync guidance outranks generic async advice |
| `ci_clippy_failure` | Repeated CI failures become useful procedural memory |
| `dangerous_cleanup` | High-severity anti-patterns are pinned when relevant |
| `offline_degraded` | Lexical/manual memory works without cass or semantic |
| `stale_rule` | Old contradicted rules are demoted or flagged |
| `secret_redaction` | Sensitive evidence does not leak into packs |
| `graph_linked_decision` | Graph proximity improves explanation without dominating |

Metrics:
- Precision @ K
- Recall @ K
- Mean reciprocal rank
- Provenance coverage
- Token waste in packs
- Duplicate-item rate
- Stale-rule suppression rate
- Anti-pattern pinning rate
- Degraded-mode honesty (claims match reality)
- Redaction correctness
- Explanation completeness

Output:

```json
{
  "schema": "ee.eval.v1",
  "fixture": "release_failure",
  "passed": true,
  "metrics": {
    "precisionAt5": 0.8,
    "recallAt10": 1.0,
    "provenanceCoverage": 1.0,
    "duplicateRate": 0.0,
    "degradedHonesty": 1.0
  },
  "failures": []
}
```

Rules: fixtures are deterministic; output is golden-tested; every major ranking or packer change runs the suite; failing evaluations block releases once thresholds stabilize; metrics must not incentivize giant packs.

### 27.6 Property and fuzz tests (`proptest`)

Targets: query schema parser, config parser, JSONL import, evidence URI parser, token-budget packer, redaction scanner, ID parser, RRF monotonicity, MMR budget invariants.

### 27.7 CI gates

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check                                 # forbidden deps
ee eval run --all --json                         # must pass once stabilized
```

---

## 28. Performance budget

Latency targets (p50, on a M-class laptop, 5k-memory workspace):

| Operation | Target p50 | Hard ceiling |
|---|---|---|
| `ee status` | 30 ms | 100 ms |
| `ee remember` (no embed) | 8 ms | 50 ms |
| `ee remember` (sync embed) | 25 ms | 250 ms |
| `ee search --speed instant` | 12 ms | 50 ms |
| `ee search --speed default` (Phase-1) | 60 ms | 250 ms |
| `ee search --speed quality` (Phase-2) | 250 ms | 2000 ms |
| `ee context` | 120 ms | 500 ms |
| `ee pack` (4k budget, 100 cands) | 80 ms | 400 ms |
| `ee why <id>` | 25 ms | 100 ms |
| `ee link` | 5 ms | 30 ms |
| `ee graph pagerank` (cached) | 3 ms | 15 ms |
| `ee graph pagerank` (cold, 5k memories) | 350 ms | 2000 ms |
| `ee curate candidates` (50 episodic) | 800 ms | 5000 ms |
| `ee outcome` | 8 ms | 30 ms |
| `ee import cass` (1000 messages) | 4 s | 30 s |

These become criterion benchmarks under `benches/`. CI fails on regression > 30% from the moving baseline.

Memory: < 100 MB resident for typical CLI invocation. Whatever embedding model frankensearch loads adds overhead per its choices.

Disk: a 5k-memory workspace is ~30 MB DB + ~10–30 MB indexes (depends on frankensearch defaults).

---

## 29. Walking skeleton acceptance gate & rollout milestones

Each milestone ends with a working binary at a tagged version and documented usage. **Implement strictly in order.** Don't start M(N+1) until M(N)'s tests and exit criteria pass.

### 29.1 Walking skeleton (M0+M1+M2)

The smallest build that proves the architecture. It must demonstrate:

```
manual memory → FrankenSQLite → search document → Frankensearch
              → context pack → pack record → why output
```

Required commands:
```bash
ee init --workspace .
ee remember --workspace . --level procedural --kind rule \
  "Run cargo fmt --check before release." --json
ee search "format before release" --workspace . --json
ee context "prepare release" --workspace . --format markdown
ee why <memory-id> --json
ee status --json
```

Acceptance criteria:
- All commands work without daemon mode.
- All commands have stable JSON mode.
- Memory is stored in FrankenSQLite through `ee-db`.
- Search results come from Frankensearch (or a documented degraded lexical path).
- Context pack includes provenance.
- `ee why` explains storage, retrieval, and pack selection.
- Pack record is persisted.
- `ee status` reports DB, index, and degraded capabilities.
- At least one cancellation test covers a command path.
- No tokio or rusqlite anywhere in the workspace.

First-slice cut lines:
- Include: 1 memory table, 1 workspace table, 1 pack record table, 1 search index, 1 Markdown renderer, 1 JSON contract, 1 `why` path, 1 deterministic eval fixture.
- Exclude: graph metrics, cass import, daemon, MCP, JSONL export, automatic curation, semantic-model acquisition (rely on frankensearch defaults).

### 29.2 Milestones

#### M0 — Repository foundation (~3 days)

- Workspace `Cargo.toml` + crates `ee-cli`, `ee-core`, `ee-models`, `ee-db`, `ee-output`, `ee-test-support`.
- `rust-toolchain.toml` for nightly.
- `#![forbid(unsafe_code)]` on all crates.
- Initial deps; `cargo deny` config for forbidden runtime crates.
- Asupersync `RuntimeBuilder` bootstrap.
- `Outcome` → CLI exit-code mapping.
- Initial budget constants.
- Capability-narrowing example at command boundary.
- `ee --version` runs.
- CI commands documented.

**Exit:** fmt/clippy/test pass; `cargo tree` shows no forbidden deps; `ee --version` works.

#### M1 — Config, runtime, DB skeleton (~4 days)

- Config discovery & merging.
- Workspace detection (upward search for `.ee/`).
- Data directory resolution.
- SQLModel/FrankenSQLite connection factory.
- Migration table.
- Initial tables: workspaces, agents, sessions, memories, memory_tags, audit_log.
- `ee init`, `ee status --json`, `ee db status --json`.
- Golden tests for `ee status`.

**Exit:** empty machine can `ee init`; project workspace registers; repeated init idempotent; status reports DB + degraded capabilities.

#### M2 — Walking skeleton (~5 days)

- Typed public IDs.
- `ee remember`, `ee memory show`, `ee memory list`.
- Tags. Content/dedupe hash.
- Audit entries for writes.
- `ee outcome` feedback (against memories).
- Frankensearch integration for memory docs.
- `ee search --json` (with explain).
- `ee context` with simple section logic.
- `ee why` for memory + result + pack.
- First eval fixture (`offline_degraded`).
- Walking skeleton acceptance gate passes.

**Exit:** all walking-skeleton commands work; eval fixture passes.

#### M3 — cass import MVP (~5 days)

- `ee-cass` crate.
- cass binary discovery + `cass health --json` integration.
- cass session models.
- `ee import cass --since`.
- `sessions`, `evidence_spans`, `import_ledger`, `idempotency_keys` tables.
- Fixture cass DB in `tests/fixtures/cass_v1.db`.
- Resumable-import test.

**Exit:** import interruptible & resumable; idempotent re-import; evidence pointers preserve source location; clear degraded output without cass.

#### M4 — Frankensearch retrieval (~5 days)

- `ee-search` crate.
- Canonical search-document schema.
- Index manifest.
- `search_index_jobs` table.
- `ee index rebuild`, `ee index status`.
- Search hits sessions + memories (via `kind` filter).
- Score explanation output.
- Deterministic fixtures with frankensearch's hash embedder.

**Exit:** `ee search` returns memories + sessions; index rebuilds from DB; stale status visible; JSON includes score components.

#### M5 — Context packing (~4 days)

- `ee-pack` crate.
- Query model.
- Candidate retrieval per section.
- Token estimation (tiktoken-rs).
- Quotas, MMR, provenance.
- Markdown + JSON renderers.
- `pack_records` persistence.
- Golden pack tests.
- `ee why pack:<id>` works.

**Exit:** `ee context "<task>" --json` emits stable packs; markdown packs are agent-usable; pack records inspectable; degraded capabilities shown.

#### M6 — Procedural rules & curation (~6 days)

- `procedural_rules`, `feedback_events`, `curation_candidates` tables.
- `ee-curate` crate.
- Rule validation (specificity, evidence, duplication, redaction).
- Maturity transitions.
- Decay & trauma-guard inversion.
- `ee curate candidates` / `validate` / `apply` / `retire` / `tombstone`.
- `ee rule list` / `show` / `add` / `mark`.
- `ee outcome --rule`.
- `ee playbook export` / `import`.

**Exit:** candidate rules can be proposed, validated, applied; bad/vague rules warn; harmful feedback demotes; proven rules prioritized in packs; playbook.yaml round-trips.

#### M7 — Graph analytics (~5 days)

- `ee-graph` crate using `fnx-runtime` + `fnx-algorithms` + `fnx-cgse`.
- `memory_links`, `graph_snapshots` tables (if not done in M6).
- `ee link`, `ee graph neighborhood` / `centrality` / `communities` / `path` / `explain-link` / `refresh`.
- Graph features in retrieval scoring.
- Auto-link pass (CoTag, CoMention, Hebbian).
- Karate-club test fixture.

**Exit:** `ee graph neighborhood --json` works; refresh stores snapshot metadata; retrieval explanations include graph features; stale snapshots degrade gracefully.

#### M8 — Steward & daemon (~5 days)

- `ee-steward` crate.
- `steward_jobs` ledger.
- Job budgets and cancellation.
- Manual `ee steward run`.
- Indexing-queue processor.
- Graph refresh job.
- Decay job.
- cass-import refresh job.
- `ee daemon --foreground` with single write owner.
- LabRuntime tests for cancellation and supervisor restart.

**Exit:** manual jobs run + record status; daemon processes index + graph jobs; daemon shutdown graceful; failed jobs visible + resumable.

#### M9 — Export, backup, project memory (~3 days)

- JSONL export with schema markers.
- JSONL import (idempotent).
- `ee db backup`.
- Redacted export mode.
- Round-trip tests.

**Exit:** export/import preserves memories + rules; redacted export omits secrets; JSONL not required for normal operation; project memory commitable.

#### M10 — Integration polish (~5 days)

- Shell completions.
- `docs/integration.md`.
- Examples for Codex / Claude Code.
- Optional MCP adapter design doc + skeleton implementation.
- Performance benchmarks in CI.
- Cross-platform release builds (Linux x86/aarch64, macOS Intel/Silicon, Windows) modeled after `dcg`'s `dist.yml`.
- Sigstore signing + checksums.

**Exit:** new repo init in <1 min; agent instructions can call `ee context`; common failures have clear `doctor` output; binary installable + updatable.

#### M11 — Evaluation hardening (~5 days)

- All eight eval fixtures.
- Retrieval quality metrics.
- Pack quality metrics.
- Degraded-mode honesty checks.
- Redaction-leak checks.
- Expanded `ee doctor --fix-plan`.
- Index/graph/job diagnostic outputs.
- Release gates once metrics stabilize.

**Exit:** all eight fixtures pass; release process includes eval output; metric regressions visible before release.

### 29.3 Post-v1

- `ee daemon` with inotify-driven live ingestion.
- LLM consolidation summarizer (anthropic provider).
- Multi-workspace bidirectional sync.
- Vector quantization in indexes for >100k memories.
- TUI mode (`ratatui`) for interactive memory browsing.
- Skill definition with parameterized procedural memories.
- HTTP+SSE server (`ee serve`).

Each post-v1 feature gets its own design doc.

---

## 30. Granular backlog

Task-tracked, dependency-edged. Drop into beads or any issue tracker.

### 30.1 Foundation (M0)

| ID | Task | Depends |
|---|---|---|
| EE-001 | Create Rust workspace and crate skeleton | — |
| EE-002 | Project-wide lint and formatting policy | EE-001 |
| EE-003 | Add `#![forbid(unsafe_code)]` to all `ee` crates | EE-001 |
| EE-004 | Asupersync runtime bootstrap | EE-001 |
| EE-005 | Add CLI parser and global flags | EE-001 |
| EE-006 | Stable error and exit-code model | EE-005 |
| EE-007 | JSON output helper | EE-006 |
| EE-008 | Golden test harness | EE-007 |
| EE-009 | `Outcome` → CLI boundary mapping | EE-004, EE-006 |
| EE-010 | Request-budget model for command handlers | EE-004 |
| EE-011 | Capability-narrowed command context wrapper | EE-004 |
| EE-012 | Forbidden-dependency audit (cargo-deny) | EE-001 |
| EE-013 | Deterministic async test helper around asupersync | EE-008 |

### 30.2 Config and workspace (M1)

| ID | Task | Depends |
|---|---|---|
| EE-020 | Path expansion utility | EE-001 |
| EE-021 | Config-file parser | EE-020 |
| EE-022 | Config precedence merge | EE-021 |
| EE-023 | Workspace detection (upward `.ee/` search) | EE-020 |
| EE-024 | `ee status --json` skeleton | EE-005, EE-022, EE-023 |
| EE-025 | `ee doctor --json` skeleton | EE-024 |

### 30.3 Database (M1)

| ID | Task | Depends |
|---|---|---|
| EE-040 | Wire SQLModel-FrankenSQLite connection | EE-004 |
| EE-041 | Migration table | EE-040 |
| EE-042 | Initial migration (workspaces, agents, memories, memory_tags, audit_log) | EE-041 |
| EE-043 | Workspace repository | EE-042 |
| EE-044 | Memory repository | EE-042 |
| EE-045 | Audit repository | EE-042 |
| EE-046 | Transaction helper | EE-040 |
| EE-047 | DB integrity command | EE-041 |
| EE-048 | Advisory file write lock | EE-040 |

### 30.4 Manual memory (M2)

| ID | Task | Depends |
|---|---|---|
| EE-060 | Typed public IDs (ULID) | EE-001 |
| EE-061 | Memory domain validation | EE-060 |
| EE-062 | `ee remember` | EE-044, EE-061 |
| EE-063 | `ee memory show` | EE-044 |
| EE-064 | `ee memory list` | EE-044 |
| EE-065 | Tag storage | EE-044 |
| EE-066 | Dedupe warnings | EE-044 |
| EE-067 | Audit entries for memory writes | EE-045, EE-062 |

### 30.5 Feedback and rules (M6)

| ID | Task | Depends |
|---|---|---|
| EE-080 | `feedback_events` table | EE-042 |
| EE-081 | Feedback scoring constants | EE-080 |
| EE-082 | Confidence decay implementation | EE-081 |
| EE-083 | `ee outcome` (memory / rule / pack) | EE-080 |
| EE-084 | `procedural_rules` table | EE-042 |
| EE-085 | Rule lifecycle transitions | EE-084 |
| EE-086 | `ee rule add` | EE-084 |
| EE-087 | `ee rule list` / `show` | EE-084 |
| EE-088 | Trauma-guard inversion job | EE-082, EE-085 |

### 30.6 cass import (M3)

| ID | Task | Depends |
|---|---|---|
| EE-100 | `ee-cass` crate | EE-001 |
| EE-101 | cass binary discovery | EE-100 |
| EE-102 | `cass health --json` parser | EE-101 |
| EE-103 | `sessions` table | EE-042 |
| EE-104 | `evidence_spans` table | EE-042 |
| EE-105 | `import_ledger` table | EE-042 |
| EE-106 | cass session import models | EE-102 |
| EE-107 | `ee import cass` | EE-103, EE-104, EE-105, EE-106 |
| EE-108 | Resumable import tests | EE-107 |
| EE-109 | Fixture cass DB committed to repo | EE-107 |

### 30.7 Search (M4)

| ID | Task | Depends |
|---|---|---|
| EE-120 | `ee-search` crate | EE-001 |
| EE-121 | Frankensearch dependency wiring | EE-120 |
| EE-122 | Canonical search-document schema | EE-121 |
| EE-123 | `search_index_jobs` table | EE-042 |
| EE-124 | Document builder for memories | EE-122 |
| EE-125 | Document builder for sessions | EE-122, EE-103 |
| EE-126 | `ee index rebuild` | EE-123, EE-124, EE-125 |
| EE-127 | `ee search --json` | EE-126 |
| EE-128 | Score-explanation output | EE-127 |

### 30.8 Context packing (M5)

| ID | Task | Depends |
|---|---|---|
| EE-140 | `ee-pack` crate | EE-001 |
| EE-141 | Context request/response structs | EE-140 |
| EE-142 | `pack_records` table | EE-042 |
| EE-143 | Token estimator | EE-141 |
| EE-144 | Section quotas | EE-143 |
| EE-145 | MMR redundancy control | EE-144 |
| EE-146 | Provenance rendering | EE-141 |
| EE-147 | `ee context --json` | EE-127, EE-142, EE-145 |
| EE-148 | Markdown renderer | EE-147 |
| EE-149 | Pack golden tests | EE-148 |
| EE-150 | `ee why pack:<id>` | EE-147 |
| EE-151 | Persist pack-selection reasons | EE-142, EE-147 |

### 30.9 Graph (M7)

| ID | Task | Depends |
|---|---|---|
| EE-160 | `ee-graph` crate | EE-001 |
| EE-161 | fnx-runtime/algorithms/cgse path deps | EE-160 |
| EE-162 | `memory_links` table | EE-042 |
| EE-163 | `graph_snapshots` table | EE-042 |
| EE-164 | Graph projection from DB | EE-162 |
| EE-165 | Centrality refresh + snapshot caching | EE-161, EE-164 |
| EE-166 | Neighborhood command | EE-164 |
| EE-167 | Graph-feature enrichment in retrieval | EE-165, EE-147 |
| EE-168 | Autolink candidate generation | EE-127, EE-164 |

### 30.10 Curation (M6)

| ID | Task | Depends |
|---|---|---|
| EE-180 | `curation_candidates` table | EE-042 |
| EE-181 | Candidate validation | EE-180 |
| EE-182 | Duplicate-rule check | EE-181, EE-127 |
| EE-183 | `ee curate candidates` | EE-180 |
| EE-184 | `ee curate validate` | EE-181 |
| EE-185 | `ee curate apply` | EE-084, EE-180 |
| EE-186 | `ee review session --propose` | EE-107, EE-180 |
| EE-187 | Cluster-based summary proposer | EE-127, EE-180 |
| EE-188 | Rule extraction proposer (pattern + LLM optional) | EE-180 |

### 30.11 Steward & daemon (M8)

| ID | Task | Depends |
|---|---|---|
| EE-200 | `steward_jobs` ledger | EE-042 |
| EE-201 | `ee-steward` crate | EE-001 |
| EE-202 | Job-budget model | EE-201 |
| EE-203 | Manual steward runner | EE-200, EE-202 |
| EE-204 | Index processing job | EE-123, EE-203 |
| EE-205 | Graph refresh job | EE-165, EE-203 |
| EE-206 | Score decay job | EE-082, EE-203 |
| EE-207 | Daemon foreground mode + supervision | EE-203 |
| EE-208 | LabRuntime cancellation tests | EE-207 |
| EE-209 | Single-write-owner actor | EE-207 |

### 30.12 Privacy (cross-cutting)

| ID | Task | Depends |
|---|---|---|
| EE-220 | `ee-policy` crate | EE-001 |
| EE-221 | Redaction classes & secret scanners | EE-220 |
| EE-222 | Redaction at import + remember + pack | EE-221, EE-062, EE-107 |
| EE-223 | `privacy.audit` job | EE-221, EE-203 |
| EE-224 | Redacted JSONL export | EE-221 |

### 30.13 Diagnostics & evaluation (M11)

| ID | Task | Depends |
|---|---|---|
| EE-240 | Stable degradation codes | EE-024 |
| EE-241 | `ee doctor --fix-plan` | EE-025, EE-240 |
| EE-242 | Index diagnostic output | EE-126, EE-240 |
| EE-243 | Graph diagnostic output | EE-165, EE-240 |
| EE-244 | Job diagnostic output | EE-200, EE-240 |
| EE-245 | `ee why <memory-id>` | EE-044, EE-128, EE-151 |
| EE-246 | Evaluation fixture schema | EE-008 |
| EE-247 | Add `release_failure` fixture | EE-246, EE-147 |
| EE-248 | Add `async_migration` fixture | EE-246, EE-147 |
| EE-249 | Add `dangerous_cleanup` fixture | EE-246, EE-147 |
| EE-250 | `ee eval run` | EE-246 |
| EE-251 | Retrieval metrics | EE-250 |
| EE-252 | Pack-quality metrics | EE-250 |
| EE-253 | Degraded-mode honesty checks | EE-240, EE-250 |
| EE-254 | Redaction-leak evaluation | EE-250 |
| EE-255 | Evaluation report renderer | EE-250 |

### 30.14 Export & backup (M9)

| ID | Task | Depends |
|---|---|---|
| EE-260 | Define JSONL schema | EE-042 |
| EE-261 | Redacted JSONL export | EE-221, EE-260 |
| EE-262 | JSONL import | EE-261 |
| EE-263 | Backup command | EE-040 |
| EE-264 | Round-trip tests | EE-262 |

---

## 31. Risks & open questions

### 31.1 Risks

**R1 — Rebuilding the old overlarge system.**
Failure mode: `ee` becomes another agent runner, web service, or orchestration project.
Mitigation: keep the CLI context workflow as the center; defer daemon/MCP/UI; explicitly reject tool-execution and planning scope.

**R2 — Storage concurrency surprises.**
Failure mode: multiple agent processes write concurrently and corrupt or lock the DB.
Mitigation: serialize writes through advisory lock or daemon; import ledgers + idempotency keys; rebuildable indexes; document single-writer assumptions.

**R3 — `fsqlite` FTS5 may not be production-ready.**
Mitigation: `ee-db` ships an inverted-index fallback behind the same `queries::fts_search()` API; switching to native FTS5 when wired is a one-line change.

**R4 — `sqlmodel_rust` is on Edition 2024 (nightly).**
Mitigation: pin `rust-toolchain.toml` to a known-good nightly; document the upgrade procedure in CONTRIBUTING.md; isolate manual SQL in `ee-db` if SQLModel features lag.

**R5 — `franken_networkx` Rust crates are unpublished (path deps).**
Mitigation: vendor needed crates via `cargo vendor` for releases, or wait for upstream publication. Track as one of the first things to resolve.

**R6 — Accidental tokio / rusqlite dependency.**
Mitigation: `cargo deny` gate; CI runs `cargo tree -e features` audits per forbidden crate; do NOT enable asupersync's optional SQLite feature.

**R7 — Asupersync becomes only an executor.**
Failure mode: code accepts `&Cx` but flattens `Outcome`, ignores budgets, passes full authority everywhere.
Mitigation: preserve `Outcome` until boundary; explicit budget models for every command class; narrow capabilities at boundaries; LabRuntime tests for cancellation, quiescence, futurelock, obligation leaks.

**R8 — Bad rules pollute context.**
Mitigation: require evidence; candidate state; specificity validation; decay; harmful weighting (4×); show provenance in packs.

**R9 — Search becomes unexplainable.**
Mitigation: store component scores; return `why` arrays; expose `ee why <result-id>`; golden-test explanations.

**R10 — Too much data in context packs.**
Mitigation: strict token budgets; quotas by type; redundancy control; section summaries; suggested-search instead of dump.

**R11 — Secret leakage.**
Mitigation: redaction before storage and output; default no remote model calls; privacy.audit job; redacted-by-default export.

**R12 — Dependency API drift (frankensearch / sqlmodel / asupersync / fnx).**
Mitigation: each dependency behind one `ee-*` crate; pin path/git revisions during early dev; add compile-time contract tests; avoid sprinkling third-party APIs through command handlers.

**R13 — Retrieval feels plausible but isn't actually useful.**
Mitigation: the evaluation harness with named fixture families; precision/recall/provenance metrics tied to real workflows; eval output in release prep.

**R14 — Diagnostics too weak for local-first software.**
Mitigation: stable degradation codes; `ee doctor --fix-plan`; `ee why`; job/index/graph/import/pack inspection commands.

**R15 — cass schema evolves.**
Mitigation: vendored fixture cass DB versioned in `tests/fixtures/cass_v<N>.db`; CI breaks loudly when fixture / parser drift.

**R16 — Embedding model downloads block cold-start.**
Mitigation: `ee init --download-models` does it eagerly; `ee search --speed instant` works without semantic; degraded codes report `embedder_offline` clearly. (We don't choose the models — frankensearch does — so this is a UX issue, not a design one.)

**R17 — Plan still leaves ambiguous build order.**
Mitigation: walking-skeleton acceptance gate is explicit; granular backlog has dependencies; every milestone has exit criteria; non-goals stay close to the roadmap.

### 31.2 Open questions for the user

These need a yes/no/preference before final design lock:

1. **Default Stop hook on `ee init`.** Should `ee init` interactively offer to write a `.claude/settings.local.json` snippet that runs `ee maintenance` on Stop? Recommendation: yes, default opt-in.
2. **Confidence-decay half-life.** 90 days is the CASS default. For coding-agent timescales (libraries upgrade fast), maybe 30–45 days is better. User preference?
3. **LLM-driven consolidation in v1?** Recommendation: defer to post-v1; v1 ships extractive summarizer + agent-native review. The `[llm]` config block is reserved.
4. **Default DB scope.** Recommendation locked: single user DB at `~/.local/share/ee/ee.db` with workspaces table; per-project via `--db <path>`. Confirm.
5. **MCP feature default.** Recommendation: off by default; `ee mcp` users opt in via `--features mcp` build.
6. **Sync model.** Cross-machine sync (e.g., via Tailscale) for shared playbooks: defer to post-v1. Confirm.
7. **TOON output format.** `--format toon`: in v1 or post-v1? Recommendation: post-v1 unless `toon` is already a project standard.
8. **Multi-modal content.** Text-only for v1 (code blocks/JSON snippets stored as text content with metadata). Confirm.

---

## Appendix A — full SQL schema

Canonical reference; the actual DDL is generated by `sqlmodel-schema` from `#[derive(Model)]` types in `ee-db`. CI's `tests/schema_drift.rs` regenerates and diffs.

```sql
-- ============================================================================
-- ee schema v1
-- Generated for SQLite/frankensqlite dialect.
-- ============================================================================

-- meta + migrations -----------------------------------------------------------

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT INTO meta (key, value) VALUES
    ('schema_version', '1'),
    ('install_id', /* ulid */ 'TBD');

CREATE TABLE migrations (
    version    TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    applied_at TEXT NOT NULL
);

-- workspaces & agents ---------------------------------------------------------

CREATE TABLE workspaces (
    id                    INTEGER PRIMARY KEY,
    public_id             TEXT NOT NULL UNIQUE,
    root_path             TEXT NOT NULL,
    canonical_path_hash   TEXT NOT NULL UNIQUE,
    git_remote            TEXT,
    git_main_branch       TEXT,
    project_name          TEXT,
    first_seen_at         TEXT NOT NULL,
    last_seen_at          TEXT NOT NULL,
    metadata_json         TEXT
);
CREATE INDEX idx_workspaces_remote ON workspaces(git_remote);

CREATE TABLE agents (
    id              INTEGER PRIMARY KEY,
    public_id       TEXT NOT NULL UNIQUE,
    kind            TEXT NOT NULL,        -- codex | claude_code | cursor | aider | unknown
    name            TEXT NOT NULL,
    version         TEXT,
    source          TEXT,
    first_seen_at   TEXT NOT NULL,
    last_seen_at    TEXT NOT NULL,
    metadata_json   TEXT
);

-- sessions & evidence ---------------------------------------------------------

CREATE TABLE sessions (
    id                INTEGER PRIMARY KEY,
    public_id         TEXT NOT NULL UNIQUE,
    cass_session_id   TEXT,
    source_uri        TEXT,
    workspace_id      INTEGER REFERENCES workspaces(id),
    agent_id          INTEGER REFERENCES agents(id),
    started_at        TEXT NOT NULL,
    ended_at          TEXT,
    status            TEXT,
    title             TEXT,
    summary           TEXT,
    task_text         TEXT,
    import_hash       TEXT,
    metadata_json     TEXT
);
CREATE UNIQUE INDEX uniq_sessions_cass ON sessions(cass_session_id) WHERE cass_session_id IS NOT NULL;
CREATE INDEX idx_sessions_workspace ON sessions(workspace_id, started_at);
CREATE INDEX idx_sessions_agent     ON sessions(agent_id, started_at);

CREATE TABLE evidence_spans (
    id                 INTEGER PRIMARY KEY,
    public_id          TEXT NOT NULL UNIQUE,
    session_id         INTEGER REFERENCES sessions(id) ON DELETE CASCADE,
    source_type        TEXT NOT NULL,
    source_uri         TEXT NOT NULL,
    message_id         TEXT,
    line_start         INTEGER,
    line_end           INTEGER,
    role               TEXT,
    excerpt            TEXT,
    excerpt_hash       TEXT NOT NULL,
    redaction_class    TEXT NOT NULL DEFAULT 'public',
    created_at         TEXT NOT NULL,
    metadata_json      TEXT
);
CREATE INDEX idx_evidence_session ON evidence_spans(session_id);
CREATE INDEX idx_evidence_source  ON evidence_spans(source_type);

-- memories --------------------------------------------------------------------

CREATE TABLE memories (
    id                  INTEGER PRIMARY KEY,
    public_id           TEXT NOT NULL UNIQUE,
    level               TEXT NOT NULL CHECK (level IN ('working','episodic','semantic','procedural')),
    kind                TEXT NOT NULL,
    scope               TEXT NOT NULL,
    scope_key           TEXT,
    workspace_id        INTEGER REFERENCES workspaces(id),
    session_id          INTEGER REFERENCES sessions(id),
    primary_evidence_id INTEGER REFERENCES evidence_spans(id),
    content             TEXT NOT NULL,
    summary             TEXT,
    schema_name         TEXT,
    schema_version      INTEGER,
    event_time          TEXT,
    valid_from          TEXT,
    valid_until         TEXT,
    importance          REAL NOT NULL DEFAULT 0.5,
    confidence          REAL NOT NULL DEFAULT 1.0,
    utility_score       REAL NOT NULL DEFAULT 0.5,
    reuse_count         INTEGER NOT NULL DEFAULT 0,
    access_count        INTEGER NOT NULL DEFAULT 0,
    last_accessed_at    TEXT,
    affect_valence      REAL,
    ttl_seconds         INTEGER,
    expires_at          TEXT,
    redaction_class     TEXT NOT NULL DEFAULT 'public',
    content_hash        TEXT NOT NULL,
    dedupe_hash         TEXT NOT NULL,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    created_by          TEXT NOT NULL,
    supersedes          INTEGER REFERENCES memories(id),
    legal_hold          INTEGER NOT NULL DEFAULT 0,
    idempotency_key     TEXT,
    metadata_json       TEXT
);
CREATE INDEX idx_memories_ws_lk     ON memories(workspace_id, level, kind);
CREATE INDEX idx_memories_scope     ON memories(scope, scope_key);
CREATE INDEX idx_memories_chash     ON memories(content_hash);
CREATE INDEX idx_memories_expires   ON memories(expires_at);
CREATE INDEX idx_memories_updated   ON memories(updated_at);
CREATE INDEX idx_memories_conf      ON memories(confidence DESC);
CREATE INDEX idx_memories_utility   ON memories(utility_score DESC);
CREATE UNIQUE INDEX uniq_memories_idem  ON memories(idempotency_key) WHERE idempotency_key IS NOT NULL;
CREATE UNIQUE INDEX uniq_memories_chash ON memories(workspace_id, dedupe_hash) WHERE workspace_id IS NOT NULL;

CREATE TABLE memory_tags (
    memory_id INTEGER NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    tag       TEXT NOT NULL,
    PRIMARY KEY (memory_id, tag)
);
CREATE INDEX idx_tags_tag ON memory_tags(tag);

CREATE TABLE memory_links (
    id                  INTEGER PRIMARY KEY,
    public_id           TEXT NOT NULL UNIQUE,
    src_memory_id       INTEGER NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    dst_memory_id       INTEGER NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    relation            TEXT NOT NULL,
    weight              REAL NOT NULL DEFAULT 1.0,
    confidence          REAL NOT NULL DEFAULT 1.0,
    directed            INTEGER NOT NULL DEFAULT 1,
    evidence_id         INTEGER REFERENCES evidence_spans(id),
    evidence_count      INTEGER NOT NULL DEFAULT 1,
    last_reinforced_at  TEXT,
    source              TEXT NOT NULL DEFAULT 'agent',
    created_at          TEXT NOT NULL,
    created_by          TEXT,
    metadata_json       TEXT,
    UNIQUE (src_memory_id, dst_memory_id, relation)
);
CREATE INDEX idx_links_src      ON memory_links(src_memory_id);
CREATE INDEX idx_links_dst      ON memory_links(dst_memory_id);
CREATE INDEX idx_links_relation ON memory_links(relation);

CREATE TABLE embeddings (
    id            INTEGER PRIMARY KEY,
    memory_id     INTEGER NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    model         TEXT NOT NULL,
    segment_type  TEXT NOT NULL,
    dims          INTEGER NOT NULL,
    vector        BLOB NOT NULL,
    created_at    TEXT NOT NULL,
    UNIQUE (memory_id, model, segment_type)
);

CREATE VIRTUAL TABLE memory_fts USING fts5(
    content, public_id UNINDEXED,
    tokenize = 'porter unicode61 remove_diacritics 1'
);
CREATE TRIGGER memory_fts_ai AFTER INSERT ON memories BEGIN
    INSERT INTO memory_fts(rowid, content, public_id)
    VALUES (new.rowid, new.content, new.public_id);
END;
CREATE TRIGGER memory_fts_ad AFTER DELETE ON memories BEGIN
    INSERT INTO memory_fts(memory_fts, rowid, content, public_id)
    VALUES ('delete', old.rowid, old.content, old.public_id);
END;
CREATE TRIGGER memory_fts_au AFTER UPDATE OF content ON memories BEGIN
    INSERT INTO memory_fts(memory_fts, rowid, content, public_id)
    VALUES ('delete', old.rowid, old.content, old.public_id);
    INSERT INTO memory_fts(rowid, content, public_id)
    VALUES (new.rowid, new.content, new.public_id);
END;

-- procedural rules ------------------------------------------------------------

CREATE TABLE procedural_rules (
    id                       INTEGER PRIMARY KEY,
    public_id                TEXT NOT NULL UNIQUE,
    memory_id                INTEGER NOT NULL UNIQUE REFERENCES memories(id) ON DELETE CASCADE,
    rule_type                TEXT NOT NULL,
    category                 TEXT NOT NULL,
    scope                    TEXT NOT NULL,
    scope_key                TEXT,
    content                  TEXT NOT NULL,
    rationale                TEXT,
    search_pointer           TEXT,
    state                    TEXT NOT NULL DEFAULT 'active',
    maturity                 TEXT NOT NULL DEFAULT 'candidate',
    helpful_count            INTEGER NOT NULL DEFAULT 0,
    harmful_count            INTEGER NOT NULL DEFAULT 0,
    decayed_helpful_score    REAL NOT NULL DEFAULT 0.0,
    decayed_harmful_score    REAL NOT NULL DEFAULT 0.0,
    effective_score          REAL NOT NULL DEFAULT 0.0,
    decay_half_life_days     REAL NOT NULL DEFAULT 90.0,
    last_validated_at        TEXT,
    created_at               TEXT NOT NULL,
    updated_at               TEXT NOT NULL,
    replaced_by_rule_id      INTEGER REFERENCES procedural_rules(id),
    metadata_json            TEXT
);
CREATE INDEX idx_rules_category   ON procedural_rules(category);
CREATE INDEX idx_rules_score      ON procedural_rules(effective_score DESC);
CREATE INDEX idx_rules_maturity   ON procedural_rules(maturity);

-- feedback --------------------------------------------------------------------

CREATE TABLE feedback_events (
    id            INTEGER PRIMARY KEY,
    public_id     TEXT NOT NULL UNIQUE,
    target_type   TEXT NOT NULL,                 -- memory | rule | pack
    target_id     INTEGER NOT NULL,
    event_type    TEXT NOT NULL,
    weight        REAL NOT NULL DEFAULT 1.0,
    note          TEXT,
    session_id    INTEGER REFERENCES sessions(id),
    workspace_id  INTEGER REFERENCES workspaces(id),
    created_at    TEXT NOT NULL,
    created_by    TEXT,
    metadata_json TEXT
);
CREATE INDEX idx_feedback_target ON feedback_events(target_type, target_id);

-- workflows / actions / artifacts ---------------------------------------------

CREATE TABLE artifacts (
    id              INTEGER PRIMARY KEY,
    public_id       TEXT NOT NULL UNIQUE,
    workspace_id    INTEGER REFERENCES workspaces(id),
    session_id      INTEGER REFERENCES sessions(id),
    kind            TEXT NOT NULL,
    uri             TEXT,
    path            TEXT,
    content_hash    TEXT,
    summary         TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    metadata_json   TEXT
);

CREATE TABLE workflows (
    id              INTEGER PRIMARY KEY,
    public_id       TEXT NOT NULL UNIQUE,
    workspace_id    INTEGER REFERENCES workspaces(id),
    session_id      INTEGER REFERENCES sessions(id),
    title           TEXT,
    goal            TEXT,
    status          TEXT,
    started_at      TEXT NOT NULL,
    completed_at    TEXT,
    outcome_summary TEXT,
    metadata_json   TEXT
);

CREATE TABLE actions (
    id              INTEGER PRIMARY KEY,
    public_id       TEXT NOT NULL UNIQUE,
    workflow_id     INTEGER REFERENCES workflows(id),
    session_id      INTEGER REFERENCES sessions(id),
    kind            TEXT NOT NULL,
    command         TEXT,
    description     TEXT,
    status          TEXT,
    started_at      TEXT NOT NULL,
    completed_at    TEXT,
    exit_code       INTEGER,
    artifact_id     INTEGER REFERENCES artifacts(id),
    metadata_json   TEXT
);

-- diary / curation / packs ----------------------------------------------------

CREATE TABLE diary_entries (
    id                          INTEGER PRIMARY KEY,
    public_id                   TEXT NOT NULL UNIQUE,
    session_id                  INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    workspace_id                INTEGER REFERENCES workspaces(id),
    status                      TEXT,
    accomplishments_json        TEXT,
    decisions_json              TEXT,
    challenges_json             TEXT,
    preferences_json            TEXT,
    key_learnings_json          TEXT,
    related_sessions_json       TEXT,
    tags_json                   TEXT,
    search_anchors_json         TEXT,
    created_at                  TEXT NOT NULL,
    metadata_json               TEXT
);

CREATE TABLE curation_candidates (
    id                          INTEGER PRIMARY KEY,
    public_id                   TEXT NOT NULL UNIQUE,
    candidate_type              TEXT NOT NULL,
    source_session_id           INTEGER REFERENCES sessions(id),
    source_evidence_id          INTEGER REFERENCES evidence_spans(id),
    proposed_payload_json       TEXT NOT NULL,
    validation_status           TEXT NOT NULL DEFAULT 'pending',
    validation_warnings_json    TEXT,
    score                       REAL,
    created_at                  TEXT NOT NULL,
    reviewed_at                 TEXT,
    reviewed_by                 TEXT,
    applied_at                  TEXT,
    metadata_json               TEXT
);
CREATE INDEX idx_candidates_status ON curation_candidates(validation_status);

CREATE TABLE pack_records (
    id                  INTEGER PRIMARY KEY,
    public_id           TEXT NOT NULL UNIQUE,
    workspace_id        INTEGER REFERENCES workspaces(id),
    query_text          TEXT,
    query_json          TEXT,
    format              TEXT NOT NULL,
    max_tokens          INTEGER NOT NULL,
    estimated_tokens    INTEGER NOT NULL,
    pack_hash           TEXT NOT NULL,
    selected_items_json TEXT NOT NULL,
    explain_json        TEXT,
    seed                INTEGER,
    created_at          TEXT NOT NULL,
    metadata_json       TEXT
);
CREATE INDEX idx_pack_hash ON pack_records(pack_hash);

CREATE TABLE retrieval_policies (
    id            INTEGER PRIMARY KEY,
    name          TEXT NOT NULL UNIQUE,
    scope         TEXT,
    scope_key     TEXT,
    policy_json   TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);

-- queues / snapshots / ledgers ------------------------------------------------

CREATE TABLE search_index_jobs (
    id            INTEGER PRIMARY KEY,
    public_id     TEXT NOT NULL UNIQUE,
    target_type   TEXT NOT NULL,
    target_id     INTEGER NOT NULL,
    operation     TEXT NOT NULL,                -- upsert | delete | rebuild
    status        TEXT NOT NULL DEFAULT 'pending',
    attempts      INTEGER NOT NULL DEFAULT 0,
    last_error    TEXT,
    created_at    TEXT NOT NULL,
    started_at    TEXT,
    completed_at  TEXT
);
CREATE INDEX idx_index_jobs_status ON search_index_jobs(status, created_at);

CREATE TABLE graph_snapshots (
    id                       INTEGER PRIMARY KEY,
    public_id                TEXT NOT NULL UNIQUE,
    workspace_id             INTEGER REFERENCES workspaces(id),
    graph_kind               TEXT NOT NULL,
    scope_hash               TEXT NOT NULL,
    algorithm                TEXT NOT NULL,
    algorithm_versions_json  TEXT,
    node_count               INTEGER,
    edge_count               INTEGER,
    metrics_json             TEXT,
    witness_hash             TEXT,
    ttl_seconds              INTEGER NOT NULL DEFAULT 600,
    created_at               TEXT NOT NULL
);
CREATE UNIQUE INDEX uniq_graph_snapshots ON graph_snapshots(scope_hash, algorithm);

CREATE TABLE import_ledger (
    id              INTEGER PRIMARY KEY,
    public_id       TEXT NOT NULL UNIQUE,
    source          TEXT NOT NULL,
    source_uri      TEXT,
    source_cursor   TEXT,
    workspace_id    INTEGER REFERENCES workspaces(id),
    status          TEXT NOT NULL DEFAULT 'pending',
    items_seen      INTEGER NOT NULL DEFAULT 0,
    items_imported  INTEGER NOT NULL DEFAULT 0,
    last_error      TEXT,
    started_at      TEXT NOT NULL,
    completed_at    TEXT,
    metadata_json   TEXT
);

CREATE TABLE steward_jobs (
    id            INTEGER PRIMARY KEY,
    public_id     TEXT NOT NULL UNIQUE,
    job_kind      TEXT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'pending',
    outcome_kind  TEXT,                          -- ok | err | cancelled | panicked
    started_at    TEXT,
    completed_at  TEXT,
    elapsed_ms    INTEGER,
    summary_json  TEXT
);
CREATE INDEX idx_steward_status ON steward_jobs(status, started_at);

CREATE TABLE audit_log (
    id            INTEGER PRIMARY KEY,
    public_id     TEXT NOT NULL UNIQUE,
    actor         TEXT NOT NULL,
    action        TEXT NOT NULL,
    target_type   TEXT,
    target_id     INTEGER,
    before_hash   TEXT,
    after_hash    TEXT,
    reason        TEXT,
    created_at    TEXT NOT NULL,
    metadata_json TEXT
);
CREATE INDEX idx_audit_target ON audit_log(target_type, target_id);

CREATE TABLE idempotency_keys (
    key           TEXT PRIMARY KEY,
    operation     TEXT NOT NULL,
    target_type   TEXT,
    target_id     INTEGER,
    created_at    TEXT NOT NULL,
    expires_at    TEXT
);
```

---

## Appendix B — JSON output contracts

All `--json` output has `schema: "ee.<noun>.v<N>"` and `version: <int>`. Examples:

### B.1 `ee context`

```json
{
  "schema": "ee.context.v1",
  "version": 1,
  "pack_id": "pack_01HXX...",
  "workspace": { "id": "ws_01HXX...", "root": "/data/projects/example" },
  "query": { "text": "fix failing release workflow", "max_tokens": 4000 },
  "tokens_used": 3812,
  "audit_hash": "blake3:9c1d…",
  "seed": 42,
  "sections": [
    {
      "kind": "rules",
      "title": "Rules",
      "items": [
        {
          "id": "rule_01HXX...",
          "memory_id": "mem_01HXX...",
          "content": "Use main as the working branch.",
          "score": 0.92,
          "components": {
            "frankensearch_fused": 0.71, "confidence": 1.0, "utility": 1.2,
            "maturity": 1.5, "recency": 0.9, "graph_centrality": 1.1,
            "scope_match": 1.2, "harmful_penalty": 1.0
          },
          "why": ["workspace_scope", "tag_match:git", "proven_rule"],
          "provenance": [
            { "type": "session", "id": "sess_01HXX...", "uri": "cass://..." }
          ]
        }
      ]
    }
  ],
  "explain": {
    "search_mode": "hybrid",
    "graph_snapshot": "graph_01HXX...",
    "pack_hash": "blake3:abc…"
  },
  "degraded": []
}
```

### B.2 `ee search`

```json
{
  "schema": "ee.search.v1",
  "version": 1,
  "query": "release failed branch stale",
  "elapsed_ms": 87,
  "phase": "default",
  "results": [
    {
      "doc_id": "mmem_01HXX...",
      "target_type": "memory",
      "target_id": "mem_01HXX...",
      "title": "Release branch compatibility",
      "snippet": "The default branch is main...",
      "score": 0.88,
      "components": {
        "frankensearch_fused": 0.71, "recency": 0.9, "confidence": 1.0,
        "utility": 1.2, "graph_centrality": 1.1
      },
      "why": ["tag_match:release", "graph_central"]
    }
  ],
  "degraded": [
    { "code": "graph_snapshot_stale", "severity": "low",
      "message": "Graph snapshot is 14h old.", "subsystem": "graph",
      "still_useful": true,
      "repair_command": "ee graph refresh --workspace ." }
  ]
}
```

### B.3 `ee curate candidates`

```json
{
  "schema": "ee.curate.candidates.v1",
  "version": 1,
  "candidates": [
    {
      "id": "cand_01HXX...",
      "type": "rule",
      "content": "Run cargo clippy with -D warnings before release.",
      "scope": "workspace",
      "scope_key": "/data/projects/example",
      "validation": { "status": "warning", "warnings": ["similar_existing_rule"] },
      "evidence": [{ "type": "session", "id": "sess_01HXX..." }]
    }
  ]
}
```

### B.4 `ee why`

```json
{
  "schema": "ee.why.v1",
  "version": 1,
  "target": { "type": "memory", "id": "mem_01HXX..." },
  "creation": {
    "created_at": "2026-04-15T...",
    "created_by": "agent:claude-code",
    "source_session_id": "sess_01HXX...",
    "primary_evidence": "ev_01HXX..."
  },
  "retrieval_history": [
    { "pack_id": "pack_01HXX...", "score": 0.92, "rank": 1, "components": {...} }
  ],
  "links": [
    { "relation": "supports", "neighbor": "mem_01HZZ...", "weight": 0.8 }
  ],
  "graph_metrics": { "pagerank_norm": 0.74, "community_id": 3 },
  "policy": { "redaction_class": "project", "would_be_demoted_by": [] },
  "contradictions": []
}
```

### B.5 `ee doctor --fix-plan`

```json
{
  "schema": "ee.doctor.fix_plan.v1",
  "version": 1,
  "repairs": [
    {
      "id": "repair_index_rebuild",
      "severity": "medium",
      "reason": "Search index generation 2 is older than DB generation 3.",
      "command": "ee index rebuild --workspace .",
      "destructive": false,
      "estimated_duration_seconds": 45
    }
  ]
}
```

### B.6 `ee eval run`

```json
{
  "schema": "ee.eval.v1",
  "version": 1,
  "fixture": "release_failure",
  "passed": true,
  "metrics": {
    "precision_at_5": 0.8, "recall_at_10": 1.0,
    "provenance_coverage": 1.0, "duplicate_rate": 0.0,
    "stale_rule_suppression_rate": 1.0, "anti_pattern_pinning_rate": 1.0,
    "degraded_honesty": 1.0, "redaction_correctness": 1.0
  },
  "failures": []
}
```

---

## Appendix C — example end-to-end agent flow

A representative trace of an agent using `ee` during a real coding task. Read this if anything in the design feels abstract.

### Setup (once per machine)

```bash
$ ee init
[ee] Initializing user database at ~/.local/share/ee/ee.db
[ee] Schema v1 created.
[ee] Default config written to ~/.config/ee/config.toml.
[ee] Optional: register workspace at /data/projects/myapp? [Y/n] Y
[ee] Workspace registered (ws_01HXX...).
[ee] Optional: write Claude Code Stop-hook to .claude/settings.local.json? [Y/n] Y

$ ee import cass --workspace . --since 90d --auto-memorize
[ee] Found 47 cass sessions matching workspace and time range.
[ee] Imported 47 sessions, 3,201 messages, 412 evidence spans.
[ee] Auto-memorized 124 episodic memories from important turns.
[ee] Done in 8.3s.
```

### Start of a coding task

User: *"Add concurrent rate limiting to the API gateway."*

```bash
$ ee context "add concurrent rate limiting to the API gateway" \
    --workspace . --max-tokens 4000 --json
```

`ee` returns the envelope from B.1, including:
- A `Rule` from playbook: "Use the `governor` crate for token-bucket rate limiting."
- An `AntiPattern`: "AVOID: in-memory HashMap<IP, Instant> as a rate limiter."
- 3 `history_snippets` from imported cass sessions where this was previously discussed.
- Provenance for every claim.

### During work

```bash
$ ee remember --workspace . --kind fact --level episodic \
    --tag governor,performance --workflow rate-limit-feb-2026 \
    "governor::DirectRateLimiter requires the 'std' feature; 'jitter' default slows hot path ~8%."
{"schema":"ee.remember.v1","version":1,"id":"mem_01HZZ...","deduplicated":false,...}

$ ee search "Send + Sync error governor" --top-k 5 --json
{...top hits...}
```

### End of work

```bash
$ ee curate candidates --kind summary --workflow rate-limit-feb-2026 --json
[ee] Found 3 clusters of size ≥3 from 14 EPISODIC memories. Emitted 3 candidate summaries.

$ ee curate apply cand_01HXX...
[ee] Applied: 1 SEMANTIC summary + 5 DerivedFrom links.

$ ee curate candidates --kind rule --workflow rate-limit-feb-2026 --json
[ee] Emitted 2 candidate rules.

$ ee curate validate cand_01HZZ...
[ee] OK: specificity ✓ evidence ✓ duplication ✓ redaction ✓.

$ ee curate apply cand_01HZZ...
[ee] Applied: rule_01HXX... at maturity=Candidate.

$ ee outcome --rule rule_01H7E2A3C --helpful "governor recommendation saved an hour"
[ee] Marked helpful (helpful_count=9, effective_score=0.84, maturity=proven).
```

### A week later

The user starts a new task. Agent runs `ee context "<new task>"`. The two new bullets from last week are now Candidate-maturity with decayed effective score ~0.47 (one week of decay against a 90-day half-life). They surface if relevant; if no one ever marks them helpful, in 9 months they fade below the cutoff.

Six weeks later, one rule turns out to be wrong. The agent marks it harmful twice. Trauma-guard fires. The rule auto-inverts to an anti-pattern; `playbook.yaml` shows it under `deprecated_patterns`; future packs surface it as a warning rather than a recommendation.

---

## Closing note

Two design forces tug in opposite directions: ambition (this could do everything) and restraint (it should do one thing well). The original Eidetic Engine died of ambition. `ee` will live by restraint.

Every decision can be traced to one of:
- A concrete pain point in the original UMS paper (§2).
- A proven idea from CASS (§17, §18).
- A documented capability of the franken-stack (§6, §7).
- A North Star scenario (§4).
- A principle in §3.

When in doubt, pick the smaller thing that satisfies the principles. Ship the walking skeleton. Add a milestone. Run the eval suite. Repeat.

— end of plan —
