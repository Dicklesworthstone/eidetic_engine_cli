<div align="center">

# Eidetic Engine (`ee`)

**Durable, local-first, explainable memory for coding agents.**

[![CI](https://img.shields.io/github/actions/workflow/status/Dicklesworthstone/eidetic_engine_cli/ci.yml?branch=main&label=CI)](https://github.com/Dicklesworthstone/eidetic_engine_cli/actions)
[![crates.io planned](https://img.shields.io/badge/crates.io-planned-lightgrey.svg)](#installation-status)
[![License: MIT+Rider](https://img.shields.io/badge/License-MIT%2BOpenAI%2FAnthropic%20Rider-yellow.svg)](./LICENSE)
[![Rust 2024](https://img.shields.io/badge/rust-2024-orange.svg)](rust-toolchain.toml)
[![No Tokio](https://img.shields.io/badge/runtime-Asupersync-blueviolet.svg)](#hard-requirements)

```bash
git clone https://github.com/Dicklesworthstone/eidetic_engine_cli
cd eidetic_engine_cli
cargo build --release
```

</div>

---

## TL;DR

### The Problem

Coding agents have **amnesia**.

Every fresh agent session walks in blank. It re-discovers your project's conventions, re-reads the same files, and hits the same trap another agent hit yesterday on the same repo. It promotes a wrong-but-plausible idea into "fact" because nothing reminded it of a decision the team already made three months ago.

The agent harness owns the loop. It does not own *memory*.

### The Solution

`ee` is a single-binary Rust CLI that gives every agent on your machine a **durable, hybrid-searchable, explainable memory substrate**. It captures facts, decisions, procedural rules, anti-patterns, and session evidence; indexes them with hybrid lexical and semantic search; reasons over them with graph algorithms; and emits compact, provenance-tagged context packs that any harness can paste into its prompt.

```bash
ee context "prepare release for this project" --workspace . --max-tokens 4000 --format markdown
```

It returns a Markdown pack of project release rules, prior release incidents from your `cass` history, verification commands, branch traps, and high-severity warnings, each with an evidence pointer and a score breakdown.

### Why Use `ee`?

| Capability | What you get |
|---|---|
| **Hybrid retrieval** | BM25 + vector search via Frankensearch's `TwoTierSearcher`, with deterministic ranking and fusion |
| **Explainable scores** | Every returned memory shows component scores, freshness, confidence, and which sources support it |
| **Procedural rules with decay** | Confidence ages out, harmful feedback demotes faster than helpful feedback promotes |
| **Anti-patterns first-class** | Trauma-guard surfaces high-severity risk memories before destructive actions |
| **Graph-aware** | PageRank, communities, shortest paths, and link prediction over memory/session/decision graphs |
| **CASS session import** | Mines your existing `cass` corpus (Claude Code, Codex, Cursor, Gemini, ChatGPT) for evidence |
| **Context profiles** | `compact`, `balanced`, `thorough`, and `submodular` quota/objective mixes |
| **Local-first** | No cloud. No paid LLM APIs required. Embeddings run locally through Frankensearch |
| **Stable JSON contract** | Every machine-facing command emits versioned JSON with `schema` field for parsing and validation |
| **Deterministic** | Same DB + indexes + config + query → identical pack hash |
| **Cancellation correct** | Built on Asupersync, so every long operation respects `&Cx`, budgets, and `Outcome` |
| **CLI first, daemon optional** | Every essential workflow runs as a one-shot. No background process required |
| **Auditable curation** | Promotions, consolidations, and tombstones produce audit entries; no silent rewrites |

---

## Quick Example

A real session, top to bottom:

```bash
# 1. Initialize a workspace
$ ee init --workspace .
✓ database opened at ~/.local/share/ee/ee.db
✓ workspace registered: eidetic_engine_cli (a7f2c19e)
✓ index dir ready: ~/.local/share/ee/indexes/combined
✓ semantic backend: frankensearch ready (local)

# 2. Capture a durable rule you just learned
$ ee remember --workspace . --level procedural --kind rule \
    --tags rust,ci \
    "This project treats clippy warnings as errors with pedantic and nursery enabled."
✓ memory mem_01HQ3K5Z stored (procedural · rule · confidence 0.80)
✓ indexed in 14ms

# 3. Pull session evidence from your cass history
$ ee import cass --workspace . --limit 50 --json | jq '.summary'
{
  "sessions_imported": 47,
  "evidence_spans": 312,
  "candidates_proposed": 8,
  "duration_ms": 2341
}

# 4. Ask for context before working
$ ee context "fix the failing release workflow" --workspace . --profile thorough
## procedural_rules

### 1. mem_01HQ3K5Z (42 tokens)

**Why:** procedural rule matched release workflow query

**Trust:** `procedural` / `accepted`

**Provenance:**
- `cass-session://7f4e` (cass-session)

## failures

### 2. mem_01HPCC3T (58 tokens)

**Why:** prior failure linked to release artifacts

# 5. Ask why a memory was selected
$ ee why mem_01HPCC3T --json | jq '.data | {retrieval, graphRetrievalFeatures}'
{
  "retrieval": {
    "confidence": 0.92,
    "utility": 0.74,
    "importance": 0.81,
    "tags": ["release", "ci"],
    "level": "procedural",
    "kind": "rule"
  },
  "graphRetrievalFeatures": {
    "status": "available",
    "centralityScore": 0.64,
    "authorityScore": 0.57,
    "reasons": ["linked to recent release evidence"]
  }
}

# 6. Record that the rule helped
$ ee outcome mem_01HQ3K5Z --signal helpful --reason "Caught a clippy regression"
✓ utility +0.08 → confidence 0.63
```

The whole flow runs locally with no daemon and no cloud, in well under a second on a typical project.

---

## Design Philosophy

> `ee` is the durable memory layer your agent harness calls. It does not replace the agent harness.

These principles are enforced in code wherever possible.

### 1. Local First

All primary data lives on your machine. No cloud dependency is required. Remote APIs and model downloads are explicit opt-in. Frankensearch handles embedding so `ee` never decides which model you run.

### 2. Harness Agnostic

`ee` is callable from any shell: Claude Code hooks, Codex shell-outs, custom scripts, plain humans, MCP adapters. It never assumes control of the agent loop. Agents push evidence in; agents pull context out.

### 3. CLI First, Daemon Later

Every essential feature works as a one-shot CLI command. The daemon (`ee daemon`) is opt-in; the current slice provides supervised foreground/write-owner plumbing, while scheduled steward jobs report a degraded status until real maintenance handlers land. No core command requires it.

### 4. Deterministic By Default

Given the same database, indexes, config, profile, budget, seed, and query, the JSON output is byte-stable, ranking ties resolve deterministically, and context pack hashes reproduce exactly. Golden tests assert this.

Mechanized proof artifacts now live alongside the test suite: [`proofs/lean4/pack_determinism.lean`](proofs/lean4/pack_determinism.lean) models the pack-hash determinism invariant, and [`proofs/tla/agent_mail_coordination.tla`](proofs/tla/agent_mail_coordination.tla) models exclusive Agent Mail reservation safety. The proof-check report schema is registered as `ee.proof_check.v1`; the `ee verify proofs` CLI surface and non-blocking `verify.sh` stage are tracked under `bd-nnfq4`.

### 5. Explainable Retrieval

Every returned memory answers six questions:

- **Why selected?** Score components per stage.
- **What supports it?** Provenance URI(s).
- **How fresh?** Recency decay term.
- **How reliable?** Confidence, evidence count, harmful-feedback weight.
- **What scores mattered?** Component breakdown.
- **What would change the decision?** Counterfactual hint when available.

### 6. Search Indexes Are Derived Assets

FrankenSQLite + SQLModel hold the source of truth. Frankensearch indexes, embeddings, graph snapshots, and caches are rebuildable from scratch. Lose your index dir? `ee index rebuild` and you are whole.

### 7. Graceful Degradation

| If this is missing | These still work |
|---|---|
| Semantic model | Lexical BM25 + FTS5 fallback |
| Graph snapshot | Retrieval without graph boosts |
| `cass` binary | Explicit `ee remember` records |
| Network | Everything (we are local-first) |

Each degradation surfaces in the JSON `degraded` array with a repair command.

### 8. Evidence Over Vibes

A procedural rule with no source session, no feedback events, and no validation stays low-confidence. Promotion to high-confidence requires evidence. Harmful feedback demotes faster than helpful feedback promotes.

### 9. No Silent Memory Mutation

Every promotion, consolidation, replacement, and tombstone produces an audit entry. The steward proposes; it does not silently rewrite procedural memory.

---

## Comparison

| Feature | `ee` | Vector DB (Chroma, Qdrant) | MCP memory server | Plain notes / CLAUDE.md |
|---|:---:|:---:|:---:|:---:|
| Local-first by default | ✅ | varies | varies | ✅ |
| Hybrid lexical + semantic | ✅ | ❌ vector-only | partial | ❌ |
| Provenance per fact | ✅ | ❌ | partial | manual |
| Procedural rules with decay | ✅ | ❌ | ❌ | ❌ |
| Anti-patterns + harmful feedback | ✅ | ❌ | ❌ | manual |
| Explainable scores | ✅ | ❌ | partial | n/a |
| Graph analytics (PageRank, paths) | ✅ | ❌ | ❌ | ❌ |
| Deterministic JSON output | ✅ | varies | varies | n/a |
| CASS session corpus import | ✅ | manual ETL | ❌ | manual |
| Works without daemon | ✅ | ❌ | ❌ | ✅ |
| Single-binary install | ✅ | ❌ | ❌ | n/a |
| No Tokio in dependency tree | ✅ | rarely | rarely | n/a |
| Audit log of curation events | ✅ | ❌ | ❌ | git only |
| Backup + side-path restore | ✅ | ❌ | ❌ | git only |

---

## Hard Requirements

Hard constraints. CI fails if any of them break.

- Binary is named `ee`. Single static binary.
- Implementation is **Rust 2024**, nightly toolchain.
- Runtime is `/dp/asupersync`. **No Tokio.** Anywhere. Ever.
- Database is `/dp/frankensqlite` through `/dp/sqlmodel_rust`. **No `rusqlite`, no SQLx, no Diesel, no SeaORM.**
- Search is `/dp/frankensearch`. No custom RRF/BM25/vector code.
- Graph is `/dp/franken_networkx`. **No `petgraph`.**
- Procedural-memory concepts come from `/dp/cass_memory_system` (concepts only).
- Every machine-facing command supports stable JSON output.
- Every generated context includes provenance and score explanation.

---

## Installation

### Installation status

`ee` is still pre-release. The GitHub release, Homebrew tap, and crates.io
install paths below are planned surfaces, not live distribution channels yet.
The current audit (`scripts/audit_install_pipeline.sh`) reports:

| Path | Status | Tracking |
|---|---|---|
| GitHub release installer | planned; no release assets published yet | `bd-3usjw.9` |
| Homebrew tap | planned; tap formula not published yet | `bd-3usjw.13` |
| crates.io | planned; package name selected as `eidetic-engine`; binary remains `ee` | `bd-3usjw.10` |
| Source build | available now | this README |

### Release installer (planned)

Planned after the first signed GitHub release ships; see `bd-3usjw.9`.

```bash
curl -fsSL https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/download/v0.1.0/install.sh \
  | EE_VERSION=v0.1.0 sh
```

This will verify the binary against the published Sigstore bundle, drop it in
`~/.local/bin/ee`, and run `ee doctor` to confirm.

PowerShell (Windows):

Planned after the first signed GitHub release ships; see `bd-3usjw.9`.

```powershell
& ([scriptblock]::Create((iwr -useb https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/download/v0.1.0/install.ps1).Content)) -Version "0.1.0"
```

### Homebrew (macOS / Linux)

Planned after `Dicklesworthstone/homebrew-tap` publishes `Formula/ee.rb`; see
`bd-3usjw.13`.

```bash
brew install Dicklesworthstone/tap/ee
```

### Cargo

Planned as the `eidetic-engine` package, which installs the `ee` binary. The
short crate name `ee` remains unavailable because `crates.io/crates/ee` points
at `https://github.com/ewpratten/ee`, not this project; see `bd-3usjw.10`.

```bash
cargo install eidetic-engine
```

### From source

```bash
git clone https://github.com/Dicklesworthstone/eidetic_engine_cli
cd eidetic_engine_cli
cargo build --release
./target/release/ee --version
```

### Verify

```bash
ee --version
ee doctor --json
```

`ee doctor` checks: database health, schema version, index manifest, embedding model presence, `cass` binary detection, workspace identity, and degraded-mode capabilities. Every check has a copy-paste repair command.

---

## Quick Start

```bash
# 1. Open a workspace (idempotent)
ee init --workspace .

# 2. Optionally seed from your cass history (recommended once)
ee import cass --workspace . --limit 50

# 3. Get context for a task
ee context "what should I know before refactoring the storage layer?" \
  --workspace . --profile thorough --max-tokens 4000 --format markdown

# 4. When you learn something durable, capture it
ee remember --workspace . --level procedural --kind rule \
  --tags rust,testing \
  "Integration tests must hit a real Postgres instance, never a mock. See incident 2025-Q3."

# 5. After a session, distill evidence-backed curation candidates
ee review session <cass-session-id> --workspace . --propose --dry-run --json
ee curate candidates --workspace .
ee curate validate <candidate-id>
ee curate apply <candidate-id>

# 6. Search at any time
ee search "release failure clippy" --workspace . --limit 20 --explain --json
```

That's the core loop.

---

## Development & Verification

To run the full verification suite before committing or pushing:

```bash
./scripts/verify.sh
```

This orchestrates all readiness gates in order, failing fast on the first failure:
1. Forbidden dependency audit (no tokio, rusqlite, petgraph, etc.)
2. Unit, contract, and golden tests
3. Basic E2E tests
4. Advanced E2E tests
5. Boundary migration tests

Each gate reports exit code and elapsed time.

---

## Command Reference

`ee` is organized into core commands and command groups. Run `ee <command> --help` or `ee <group> --help` for full details.

### Core workflow

| Command | Purpose |
|---|---|
| `ee init [--workspace .]` | Create or open a workspace, run migrations, prepare indexes |
| `ee status [--json]` | DB generation, index generation, degraded capabilities, recent jobs |
| `ee doctor [--json]` | Health checks with repair commands for every failure |
| `ee context "<task>" [--profile <p>] [--max-tokens N] [--format <fmt>]` | Assemble a task-specific context pack (the headline command) |
| `ee search "<query>" [--limit N] [--explain] [--json]` | Hybrid retrieval over memories, sessions, rules, evidence |
| `ee remember "<text>" --level <l> [--kind <k>] [--tags a,b]` | Capture a durable memory |
| `ee outcome <id> --signal helpful\|harmful [--reason "<reason>"]` | Record feedback, updating utility/confidence |
| `ee why <memory-id> [--json]` | Explain why a memory was selected, scored, or curated the way it was |
| `ee pack build --query-file task.eeq.json --max-tokens N --format toon` | Build a pack from an explicit EQL query document |
| `ee pack replay <pack-id> --json` | Inspect the persisted, redaction-safe selection ledger for a historical pack |
| `ee pack diff <old-pack-id> <new-pack-id> --json` | Compare two persisted pack ledgers and explain selection, freshness, redaction, or derived-asset changes |
| `ee support bundle --out <dir> --json` | Create a redacted diagnostic bundle, including pack replay and swarm-brief summaries without raw query, mail body, memory, or full file-listing content |

### Graph-derived insights

Graph views show relationships between memories for navigation, packing,
curation, and triage; they do not replace provenance from the memory records
themselves.

| Command | Purpose |
|---|---|
| `ee insights --json` | Bundle graph-derived findings such as top memories, bridges, contradiction clusters, proximity hotspots, load-bearing memories, HITS hubs/authorities, and skyline posture |
| `ee insights --section <name> --json` | Return one deterministic section when a full bundle is too broad |
| `ee context "<task>" --explain --json` | Include a Pack DNA block that explains pack composition with dominators, communities, ego subgraphs, and PPR neighbors when available |
| `ee why <memory-id> --causal-explain --json` | Add a causalExplanation block with causal ancestry and min-cost path evidence |
| `ee insights --section causalBottlenecks --json` | Inspect causal bottleneck findings across failure-oriented causal evidence |
| `ee health --robot-insights --json` | Surface structural health through k-truss and contradiction-cluster summaries |
| `ee insights --section knowledgeSkyline --json` | Summarize portfolio-level memory posture across onion layers, communities, trust, age, and graph support |

Worked example: inspect bridge memories before curation.

```bash
ee insights --section bridges --workspace . --json \
  | jq '.data.sections[] | select(.name == "bridges") | .items[0]'
```

```json
{
  "memoryId": "mem_release_policy",
  "articulationPoint": true,
  "nextCommands": ["ee why mem_release_policy --workspace . --json"]
}
```

Worked example: debug a surprising context pack.

```bash
ee context "prepare release" --workspace . --explain --json \
  | jq '.data.pack.packDna'
```

```json
{
  "schema": "ee.context.pack_dna.v1",
  "voronoiDominator": {"memoryId": "mem_release_policy"},
  "pprNeighbors": [{"memoryId": "mem_rch_remote_required", "rank": 1}]
}
```

Worked example: inspect tightly connected memory pairs before editing related
records. Use `proximityHotspots` to find ranked pairs worth reviewing, then use
`ee proximity` for the pairwise min-cut explanation.

```bash
ee insights --section proximityHotspots --workspace . --json \
  | jq '.data.sections[] | select(.name == "proximityHotspots") | .items[0]'
```

```json
{
  "schema": "ee.proximity.v1",
  "interpretation": "strong",
  "treePath": ["mem_release_policy", "mem_rch_remote_required"]
}
```

```bash
ee proximity mem_release_policy mem_rch_remote_required --workspace . --json
```

Start with [`docs/agent-ux/insights-onboarding.md`](docs/agent-ux/insights-onboarding.md)
for the agent workflow, [`docs/configuration/graph.md`](docs/configuration/graph.md)
for graph feature flags and thresholds, and
[`docs/architecture/graph-snapshots.md`](docs/architecture/graph-snapshots.md)
for snapshot lifecycle rules.

### Pack replay evidence

Use `ee pack replay <pack-id> --json` when you need to explain what a historical
pack actually selected from its persisted ledger. Replay is forensic: it reads
the stored non-secret ledger and does not claim that a fresh search would make
the same choices today. Use a new `ee context` or `ee pack` run when you want
live re-retrieval against current memories, indexes, graph snapshots, and trust
state.

Use `ee pack diff <old-pack-id> <new-pack-id> --json` when a later pack changed
and you need to separate selection, freshness, redaction, trust, or derived-asset
causes. Freshness states and degradation codes identify evidence that was
changed, missing, stale, or unavailable at replay time; treat those as repair or
revalidation signals instead of silently dropping the memory from the story.

For bug reports and handoffs, attach the output of
`ee support bundle --out <dir> --json`. The bundle includes
`pack_replay_summary.json`, which keeps pack IDs, pack hashes, ledger hashes,
freshness counts, degradation codes, redaction classes, and derived-asset
metadata, while hashing query text and omitting raw memory content, `why` text,
provenance text, and full ledger payloads.

Bundles also include `swarm_brief_summary.json`, a compact coordination posture
snapshot for support and handoff triage. It keeps source statuses, ready/blocked
work counts, active-conflict counts, resource-pressure posture, degraded codes,
top recommendation IDs, and hashes/provenance for the underlying brief. It
omits raw Agent Mail bodies, raw query text, raw provenance text, and full file
listings. Treat it as diagnostic context only; it is not a substitute for a
fresh `ee swarm brief` before claiming work or coordinating edits.

### Swarm brief workflow

`ee swarm brief` is the read-only coordination preflight for crowded repos. Run
it before claiming a bead, after large dirty-state or reservation changes, and
before using handoff or support-bundle evidence as the basis for new work.

Start with the compact operator view:

```bash
ee swarm brief --workspace . --json
```

Use full output when a harness needs every source array, including file-surface
risks and resource-pressure hints:

```bash
ee --fields full swarm brief --workspace . --include-rch --json
```

Require selected live coordination sources when degraded output is unacceptable:

```bash
ee swarm brief --workspace . --sources git,beads,bv,agent-mail --require-sources --json
```

If live Agent Mail is unavailable, provide a redacted snapshot instead of raw
mail bodies:

```bash
ee swarm brief --workspace . --agent-mail-snapshot <snapshot.json> --json
```

Useful JSON checks:

```bash
ee --fields summary swarm brief --workspace . --json \
  | jq '.data.topRecommendations[] | select(.kind == "safe_surface_candidate") | {id,severity,confidence,reasonCodes,suggestedCommands}'

ee --fields full swarm brief --workspace . --json \
  | jq '.data.beads.blocked[] | {id,title,priority,sourceBucket}'

ee --fields full swarm brief --workspace . --json \
  | jq '.data.fileSurfaceRisks[] | select((.riskFactors // []) | any(. == "active_exclusive_reservation" or contains("reservation_overlap"))) | {pathPattern,severity,score,riskFactors}'

ee swarm brief --workspace . --json \
  | jq '.data.degraded[] | {source,code,severity,repair}'

ee --fields full swarm brief --workspace . --include-rch --json \
  | jq '.data.recommendations[] | select(.id == "rec.resource_pressure.use_rch_for_cargo") | .suggestedCommands[]'

ee --fields full swarm brief --workspace . --json \
  | jq '.data.recommendations[] | select(.id == "rec.work_selection.no_ready_beads") | {reasonCodes,suggestedCommands}'
```

Operator workflow for crowded repos:

1. Run `ee swarm brief --workspace . --json`.
2. Inspect recommendations, blocked beads, degraded sources, and file-surface risks.
3. Reserve edit surfaces through Agent Mail, then mark the bead with `br update <id> --status in_progress --json`.
4. Use RCH for Cargo verification, especially when the brief reports `rec.resource_pressure.use_rch_for_cargo`.
5. Rerun the brief after large edits, after reservation changes, and before handoff.

The brief complements existing tools; it does not replace their authority.
`br ready --json` remains the source of ready-work records, and
`bv --robot-triage` remains the graph-aware ranking engine. Agent Mail remains
the authority for reservations and coordination messages. Handoff capsules and
support bundles carry diagnostic snapshots such as `swarm_brief_summary.json`,
but a live brief is still the preflight before new claims. Profile reports and
performance forensics diagnose host behavior in detail; the brief only surfaces
enough posture to steer choices such as routing Cargo through RCH.

The command never claims work, never reserves files, never releases files,
never sends mail, never runs builds, never edits files, never mutates Beads,
never mutates the EE store, never mutates git, and never schedules agents.

Privacy is intentionally conservative. The redaction status
`paths_counts_subjects_only_no_content` means the brief and support-bundle
summary keep paths, counts, source statuses, subject-like metadata, hashes, and
recommendation identifiers while omitting raw mail bodies, raw query text, raw
memory content, raw provenance text, environment dumps, and full file listings.
Attach `swarm_brief_summary.json` in support bundles and handoffs when you need
coordination posture without leaking content; attach fresh live output only when
the recipient is allowed to see the underlying repo and coordination metadata.

### Swarm schema contracts

Swarm-scale JSON contracts live in [`docs/schemas/swarm/`](docs/schemas/swarm/)
with companion agent-facing notes in [`docs/swarm/`](docs/swarm/). The catalog
covers producer metadata, trust lanes, verification evidence, coordination
snapshots, resource profiles, pack SLOs, recommendations, consensus, conflicts,
fixture manifests, and planned handoff memory-set fingerprints.

Every schema carries an `x-ee-status` marker. Agents should treat
`"shipped": false` as documentation for a future surface, not runtime
availability. The schema catalog does not turn `ee` into a scheduler, web
service, mail sender, Beads mutator, or agent loop.

### Import & ingestion

| Command | Purpose |
|---|---|
| `ee import cass --workspace . [--limit N] [--dry-run]` | Pull session evidence from `coding_agent_session_search` |
| `ee import jsonl --source <file>` | Restore from a JSONL records file, including backup record exports |
| `ee import eidetic-legacy --source <path> --dry-run` | One-time migration of legacy Eidetic Engine artifacts (read-only) |

### Curation & rules

| Command | Purpose |
|---|---|
| `ee review session <id> --propose [--dry-run]` | Distill imported CASS session evidence into proposed memories/rules |
| `ee curate candidates [--workspace .]` | List pending curation candidates |
| `ee curate validate <id>` | Run validation (specificity, duplication, scope, evidence) |
| `ee curate apply <id>` / `accept <id>` / `reject <id>` / `snooze <id>` / `merge <a> <b>` | Lifecycle transitions |
| `ee curate disposition` | Evaluate TTL disposition policy without silent mutation (`--apply` is required to write) |
| `ee playbook extract [--since <RFC3339>] [--dry-run]` | Propose procedural-rule candidates from repeated semantic memories |
| `ee playbook list [--limit N]` | List procedural rules in portable playbook form |
| `ee playbook export --out <file> [--dry-run]` | Write a no-overwrite portable playbook artifact |
| `ee playbook import --source <file> [--apply]` | Dry-run or apply a portable playbook import through audited rule writes |
| `ee rule add` / `list` / `show <id>` / `mark <id>` / `protect <id>` / `update <id>` | Direct rule management |

### Memory inspection

| Command | Purpose |
|---|---|
| `ee memory show <id> [--json]` | Full record with provenance, links, audit trail |
| `ee memory list [--workspace .] [--level <l>] [--tag <t>]` | Filtered listing |
| `ee memory history <id>` | Audit trail for a memory |
| `ee memory level <id> --to <level> --reason <why> [--dry-run]` | Manual adjacent level transition with `memory.level_transition` audit |
| `ee memory expire <id> [--dry-run]` | Audited soft expiration without deleting memory rows |
| `ee memory link <id> [target-id] --relation <type> [--dry-run]` | Deterministic memory link listing and audited explicit link creation |
| `ee memory tags <id> [--add <tags>] [--remove <tags>] [--set <tags>] [--clear]` | Deterministic audited tag listing and mutation |

### Graph

| Command | Purpose |
|---|---|
| `ee graph export [--workspace .]` | Export a deterministic graph snapshot artifact |
| `ee graph neighborhood <id> [--direction both] [--limit N]` | Expand around a memory/session/rule |
| `ee graph centrality-refresh [--dry-run]` | Refresh PageRank / betweenness metrics |
| `ee graph feature-enrichment [--dry-run]` | Compute bounded graph-derived retrieval features |
| `ee insights [--section <name>] [--explain <id>] --json` | Inspect graph-derived findings and memory-centric topology |

### Index

| Command | Purpose |
|---|---|
| `ee index status` / `rebuild` / `reembed` | Manage derived search indexes (Frankensearch owns model selection) |
| `ee index vacuum` | Preview reclaimable derived search-index artifacts without deleting or rewriting files |

### Workspace, models, schemas

| Command | Purpose |
|---|---|
| `ee workspace resolve` / `list` / `alias <name>` | Identity, monorepo subscopes, and aliases |
| `ee db status` / `inspect <table>` / `check-integrity` / `reindex --dry-run` | Inspect FrankenSQLite schema, table rows, integrity, and derived-index rebuild plans without bypassing `ee` |
| `ee model status` / `list` | Inspect embedding model registry posture |
| `ee schema list` / `export <schema-id>` | Inspect stable machine-output schemas |

### Backup & restore

| Command | Purpose |
|---|---|
| `ee export [--output-dir <dir>] [--redaction standard]` | Export redacted JSONL records as a portable side-path artifact |
| `ee backup create [--label <name>] [--include-graph-cache[=bool]]` | Create a verified backup with manifest; graph-cache derived assets are included by default |
| `ee backup list` / `verify <id>` / `inspect <id>` | Audit existing backups |
| `ee backup restore <backup-id> --side-path <path>` | Restore into an isolated side path |

### Diagnostics, eval, ops

| Command | Purpose |
|---|---|
| `ee eval run` / `list` | Run or list retrieval-quality evaluation fixtures |
| `ee eval report [fixture]` | Summarize fixture IDs, data hashes, aggregate retrieval metrics, and the first failing query |
| `ee eval run <fixture> --pack-quality --json` | Check whether deterministic fixtures still select required context-pack evidence |
| `ee analyze science-status --json` | Report optional science analytics feature posture and degradations |
| `ee capabilities` / `check` / `health` | Inspect feature availability and readiness |
| `ee daemon --foreground` | Optional supervised maintenance daemon |

Use pack-quality evaluation when a canonical task should keep selecting specific
memories across retrieval or packing changes. The report is a deterministic
`ee.eval.pack_quality_report.v1` result with selected and omitted memory IDs,
degradation posture, redaction status, artifact paths, and stable failure
reasons for fixture triage. See
[`docs/pack-replay.md`](docs/pack-replay.md) for operator and fixture-authoring
guidance.

---

## Configuration

`ee` reads config in this precedence order (highest wins):

1. CLI flags
2. Environment variables (`EE_*`)
3. Project config: `<workspace>/.ee/config.toml`
4. User config: `~/.config/ee/config.toml`
5. Built-in defaults

Full annotated example:

```toml
# ~/.config/ee/config.toml

[storage]
database_path = "~/.local/share/ee/ee.db"
index_dir     = "~/.local/share/ee/indexes"
jsonl_export  = false                # auto-export memory.jsonl on each commit

[runtime]
daemon            = false            # one-shot CLI mode
job_budget_ms     = 5000             # cancel any in-process job after this
import_batch_size = 200

[cass]
enabled = true
binary  = "cass"                    # path or PATH lookup
since   = "90d"                     # CASS lookback for import planning and policies

[search]
default_speed   = "balanced"         # fast | balanced | thorough
lexical_weight  = 0.45
semantic_weight = 0.45
graph_weight    = 0.10

[pack]
default_profile  = "balanced"
default_format   = "markdown"
default_max_tokens = 4000
mmr_lambda       = 0.7
candidate_pool   = 100

[curation]
duplicate_similarity = 0.92
harmful_weight       = 2.5            # harmful feedback hits harder than helpful
decay_half_life_days = 60

[learn]
cluster_coherence_threshold = 0.55     # average-linkage merge floor for `ee learn cluster`

[learn.decay]
demote_threshold = 0.05
forget_threshold = 0.01
working_half_life_days = 1
episodic_event_half_life_days = 30
episodic_failure_half_life_days = 90
semantic_fact_half_life_days = 180
procedural_rule_half_life_days = 365
default_half_life_days = 30

[feedback]
harmful_per_source_per_hour = 5        # excess harmful events are quarantined
harmful_burst_window_seconds = 3600

[privacy]
redact_secrets   = true
redaction_classes = ["api_key", "jwt", "password", "private_key", "ssh_key"]

[trust]
default_class = "agent_assertion"     # bumped on validation, demoted on contradiction
prompt_injection_guard = true
```

Environment variable overrides:

| Variable | Equivalent |
|---|---|
| `EE_DATABASE_PATH` | `[storage].database_path` |
| `EE_INDEX_DIR`     | `[storage].index_dir` |
| `EE_PROFILE`       | `[pack].default_profile` |
| `EE_MAX_TOKENS`    | `[pack].default_max_tokens` |
| `EE_HARMFUL_PER_SOURCE_PER_HOUR` | `[feedback].harmful_per_source_per_hour` |
| `EE_HARMFUL_BURST_WINDOW_SECONDS` | `[feedback].harmful_burst_window_seconds` |
| `EE_SCIENCE_BACKEND_PATH` | optional science analytics backend health path |
| `EE_DISABLE_REMEMBER_SEARCH_NEIGHBORS` | disables Frankensearch neighbors for remember-time curation proposal |
| `EE_DISABLE_TOON` | disables TOON capability reporting and auto-selection |
| `EE_NO_COLOR`      | disables ANSI styling on stderr |
| `EE_TRACE`         | enables structured tracing to stderr |

---

## Architecture

```
                ┌─────────────────────────────────────────────────┐
                │  Coding Agent (Claude Code · Codex · Cursor …)  │
                └──────────────────────┬──────────────────────────┘
                                       │
                  ee context · search · remember · import · curate
                                       ▼
                ┌─────────────────────────────────────────────────┐
                │                     ee-cli                      │
                │  Clap commands · process I/O · output rendering │
                └──────────────────────┬──────────────────────────┘
                                       ▼
                ┌─────────────────────────────────────────────────┐
                │                     ee-core                     │
                │ use-cases · services · runtime wiring · policy  │
                └──┬──────┬──────┬──────┬──────┬──────┬──────┬───┘
                   ▼      ▼      ▼      ▼      ▼      ▼      ▼
                ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌─────┐
                │ db │ │srch│ │cass│ │grph│ │pack│ │cura│ │stwd │
                └─┬──┘ └─┬──┘ └─┬──┘ └─┬──┘ └─┬──┘ └─┬──┘ └──┬──┘
                  │      │      │      │      │      │       │
                  ▼      ▼      ▼      ▼      ▼      ▼       ▼
               FrankenSQ  Franken-  CASS    Franken-  Pack   Steward
               + SQLModel  search   robot/  NetworkX  records jobs
               (truth)    (lex+sem) JSON    (graph)   + audit (opt
                                                              daemon)

                Source of truth ──► Derived assets (rebuildable)
```

**One source of truth.** FrankenSQLite + SQLModel hold every durable fact. Indexes, embeddings, graph snapshots, and caches are derived and reproducible from the DB plus config.

**Strict dependency direction.** `cli → core → { db, search, cass, graph, pack, curate, policy, output } → models`. No upward edges. Repositories never render output. Command handlers never write SQL.

**Native Asupersync.** Every async path takes `&Cx`, returns `Outcome<T>`, respects budgets, and supports cancellation. Tests run on `LabRuntime` for deterministic time and scheduling.

---

## Storage Layout

```
~/.local/share/ee/
├── ee.db                   # FrankenSQLite source of truth (WAL mode)
├── indexes/
│   └── combined/
│       ├── manifest.json   # generation, model id, lexical+vector files
│       └── ...             # Frankensearch artifacts
├── backups/                # `ee backup create` lands here
├── cache/                  # transient, safe to wipe
└── logs/                   # tracing-subscriber JSON logs

<workspace>/.ee/            # optional project artifacts (git-friendly)
├── config.toml             # checked-in project overrides
├── playbook.yaml           # human-editable rules promoted into the project
├── memory.jsonl            # optional auto-export
└── README.txt
```

Workspaces are first-class rows inside the user-global DB. A project can opt into a project-local DB via `[storage] database_path = "./.ee/db.sqlite"` when isolation matters more than global recall.

---

## Memory Model

`ee` distinguishes four memory levels, each with its own scoring tilt and packing quota:

| Level | Examples | Decay | Packing priority |
|---|---|---|---|
| `working` | Active task notes, scratch, in-progress facts | fastest | low (suppressed across sessions) |
| `episodic` | "On 2026-03-12 the release failed because…" | medium | medium |
| `semantic` | Project conventions, architectural facts | slow | high |
| `procedural` | Rules, anti-patterns, playbooks | slowest, decays only on contradiction | highest |

Level changes are explicit lifecycle transitions, not silent rewrites. Automatic
paths include workflow close (`working` -> `episodic`), curate apply for repeated
observations (`episodic` -> `semantic`), curate apply for validated rules
(`semantic` -> `procedural`), `memory expire` for time-bound facts
(`semantic` -> `episodic`), and decay/tombstone maintenance. Manual transitions
use `ee memory level <id> --to <level> --reason <why>` and are restricted to the
same adjacent edges: `working` -> `episodic`, `episodic` -> `semantic`,
`semantic` -> `procedural`, and `procedural` -> `semantic`. Every successful
transition writes a `memory.level_transition` audit row with previous level, new
level, event, reason, evidence references, and a stable details hash.

Memory `kind` is orthogonal: `rule`, `fact`, `decision`, `failure`, `command`, `convention`, `anti-pattern`, `risk`, `playbook-step`, …

Every memory carries: `id`, `level`, `kind`, `content`, `content_hash`, `tags[]`, `confidence`, `utility`, `importance`, `created_at`, `last_seen_at`, `access_count`, `source_type`, `source_uri`, `evidence_spans[]`, `links[]`, `trust_class`.

---

## Context Profiles

Different tasks need different memory mixes. `--profile` currently selects one of the shipped context-packing profiles without bypassing trust or privacy:

| Profile | Bias |
|---|---|
| `compact` | Prioritizes procedural rules and known failure modes in a tight budget |
| `balanced` | Default mix across rules, decisions, failures, evidence, and artifacts |
| `thorough` | Expands evidence and artifact coverage for higher-recall work |
| `submodular` | Uses the facility-location objective with thorough section quotas for deterministic diversity |

---

## CASS Integration

`ee` consumes `coding_agent_session_search` (`cass`) as the raw session source; it does **not** duplicate the underlying store. Every fact imported from a session carries a provenance URI back to the exact session and line range.

```bash
# Discover what cass has
ee import cass --workspace . --limit 50 --dry-run --json

# Real import (idempotent, resumable, ledger-tracked)
ee import cass --workspace . --limit 50

# Review curation candidates proposed from imported session evidence
ee review session <cass-session-id> --workspace . --propose --dry-run --json
ee curate candidates --workspace .
```

Required `cass` commands consumed (all with stable contracts):

- `cass health --json`
- `cass search "<q>" --robot`
- `cass view <path> -n <line> --json`
- `cass expand <path> -n <line> -C <ctx> --json`
- `cass capabilities --json`

If `cass` is missing, `ee` runs in degraded mode. Explicit `ee remember` records still work fully, and `ee status` clearly reports the missing capability with the install command.

---

## Agent Harness Integration

### Claude Code

Add to your `AGENTS.md` or hook setup:

```text
Before starting substantial work, run:
  ee context "<task>" --workspace . --max-tokens 4000 --format markdown

When you discover a durable project convention:
  ee remember --workspace . --level procedural --kind rule "<rule>"

After a remembered rule helps or harms:
  ee outcome <id> --signal helpful
  ee outcome <id> --signal harmful
```

Or wire it into a PreToolUse hook that injects context before risky commands. The `ee context` JSON is stable and parseable.

### Codex

Codex shells out, so the same calls work. The output of `ee context "<task>" --json` is designed to drop directly into a system or developer message.

### MCP

The MCP manifest is always available so agents can discover the CLI contract
from default builds:

```bash
ee mcp manifest --json
ee mcp validate --json
```

When the `mcp` feature is not enabled, the manifest succeeds and reports
`capabilityGap.code=mcp_feature_disabled` for the stdio adapter. Build with
`cargo build --release --features mcp` when you need the adapter itself. The
manifest mirrors the CLI contracts for tools such as `ee_context`, `ee_search`,
`ee_remember`, `ee_outcome`, `ee_curate_candidates`, and `ee_memory_show`.
`ee mcp validate --json` checks that manifest contract against the public schema
without starting the stdio adapter. Schemas match CLI JSON exactly; the CLI is
the compatibility contract.

### Plain humans

It's a CLI. Use it from your shell.

---

## Privacy & Trust

### Redaction

Secrets are detected before storage. Default redaction classes: `api_key`, `jwt`, `password`, `private_key`, `ssh_key`, `aws_secret`, `oauth_token`. Redacted spans are replaced with stable placeholders and the original is **never** written to disk.

```bash
ee remember "DATABASE_URL=postgres://user:hunter2@host/db"
# stored as: "DATABASE_URL=postgres://user:***REDACTED:password***@host/db"
```

### Trust classes

Memories carry a trust class that affects packing priority:

| Class | Source | Initial confidence |
|---|---|---|
| `human_explicit` | User-typed `ee remember` | 0.85 |
| `agent_validated` | Agent assertion + outcome confirmation | 0.65 |
| `agent_assertion` | Agent assertion, no validation | 0.50 |
| `cass_evidence` | Imported session span | 0.45 |
| `legacy_import` | Old Eidetic Engine artifact | 0.30 (caps until validated) |

Lifecycle rules, advisory priority, and prompt-injection handling are specified
in [`docs/trust-model.md`](docs/trust-model.md); ADR 0009 remains the canonical
trust taxonomy.

### Prompt-injection guard

The trust pipeline flags suspicious patterns (fake instructions, role override attempts, exfiltration cues) before promotion. Flagged memories quarantine into `curate candidates` and never silently enter the procedural layer.

---

## Backup & Restore

```bash
# Verified backup, including graph snapshots, witnesses, and result-cache rows
ee backup create --label pre-refactor
✓ backup bk_01HQ4… (32 MB) verified

# Portable redacted JSONL export
ee export --output-dir ./ee-export --redaction standard --json

# List
ee backup list

# Inspect contents without restoring
ee backup inspect bk_01HQ4… --json

# Restore to an isolated side path, replaying graph cache by default
ee backup restore bk_01HQ4… --side-path ~/ee-restored/
```

Backups include the durable DB/JSONL source of truth, the curation audit log, and a `manifest.json` with content hashes. By default, `ee backup create` also includes graph-cache derived assets: graph snapshots, graph algorithm witnesses, and graph algorithm result-cache rows. Use `--include-graph-cache=false` to create a source-only backup, and use `ee backup restore --skip-graph-cache` when you want restore to leave that cache cold and re-warm it on first use. Missing index manifests are reported as degraded. Verification re-hashes everything included on disk.

---

## Performance

Canonical hardware class: `mac-m3-pro` (`benches/baselines/hardware_classes.toml`).
Measured on a 2024 MacBook Pro M3 against a workspace with 25 projects, 14k memories, 8k imported CASS sessions, ~120k indexed documents. CI and release tooling must not overwrite these rows with artifacts from a different hardware class.

<!-- perf:begin hardware-class=mac-m3-pro baseline=benches/baselines/perf_v0_2.json -->
| Operation | Hardware class | p50 | p99 |
|---|---|---:|---:|
| `ee remember` (single record) | `mac-m3-pro` | 8 ms | 22 ms |
| `ee search "<q>"` (hybrid) | `mac-m3-pro` | 38 ms | 110 ms |
| `ee context "<task>"` (markdown, 4k tokens) | `mac-m3-pro` | 95 ms | 240 ms |
| `ee why <id>` | `mac-m3-pro` | 25 ms | 100 ms |
| `ee init --workspace <dir>` (clean) | `mac-m3-pro` | 100 ms | 250 ms |
| `ee audit timeline --limit 1000` | `mac-m3-pro` | 35 ms | 100 ms |
| `ee import cass --limit 50` (cold) | `mac-m3-pro` | 4.1 s | 11 s |
| `ee graph centrality-refresh` (PageRank, 5k links) | `mac-m3-pro` | 350 ms | 2.0 s |
| `ee index rebuild` (full) | `mac-m3-pro` | 18 s | 41 s |
| 4 concurrent audited memory writers | `mac-m3-pro` | 120 ms | 350 ms |
Last synced: 2026-05-13T12:52:12Z from sha256:84433f76b5ae84ba96bb3546a75d432175c2fd0f1c477dff03cb59a31b7ab7e6
<!-- perf:end -->

Benchmark profiles are explicit so agents and CI can pick the right cost tier:

```bash
# Small no-mock smoke run, suitable for agent closeout through rch
rch exec -- env TMPDIR=/Volumes/USBNVME16TB/temp_agent_space/tmp CARGO_TARGET_DIR=/Volumes/USBNVME16TB/temp_agent_space/cargo-target ./scripts/bench.sh --profile ci-smoke --json

# Broader nightly profile over all benchmark groups
./scripts/bench.sh --profile nightly

# Exploratory large-machine run for 256GB+/64-core hosts
./scripts/bench.sh --profile stress

# J9 broad regression wrapper pinned to benches/baselines/perf_v0_2.json
./scripts/bench_perf_regression.sh --profile nightly --check-regression
```

Budgets are currently advisory while deterministic scale fixtures stabilize.
The harness emits `ee.perf.v1` JSON with profile, workload, artifact paths,
latency fields, resource fields when available, and regression status. A J10
coverage test keeps every row in the table above tied to a benchmark/baseline
or an explicit advisory marker. Profiles can become release-blocking once their
fixture variance is low enough for CI.

### Codex RCH Workaround

Some Mac Codex sessions may still find an older `rch` on `PATH` or report the
Codex hook as not installed. Until that local installation is upgraded, invoke
the current RCH client by absolute path and fail closed to remote execution:

```bash
RCH_REQUIRE_REMOTE=1 \
RCH_VISIBILITY=summary \
RCH_CANONICAL_PROJECT_ROOT=/Users/jemanuel/projects \
RCH_ALIAS_PROJECT_ROOT=/data/projects \
/Users/jemanuel/projects/remote_compilation_helper/target-local/release/rch exec -- \
  env CARGO_TARGET_DIR=/Volumes/USBNVME16TB/temp_agent_space/cargo-target \
  cargo test --lib search_sync_attaches_rebuilt_lexical_index_for_literal_queries -- --nocapture
```

RCH rewrites the local USB-NVMe `CARGO_TARGET_DIR` to a worker-local target path
for remote execution, so the external-drive setting is safe for both local
artifact retrieval and remote Linux workers.

---

## Troubleshooting

### `error: search index is stale`

The DB has advanced past the index generation. Rebuild:

```bash
ee index rebuild --workspace .
```

### `error: cass binary not found`

Either install `cass` or disable CASS import:

```bash
# Install
cargo install --path /dp/coding_agent_session_search

# Or disable in your config file
# [cass]
# enabled = false
```

`ee` continues to work without `cass`; explicit `ee remember` is unaffected.

### `error: migration required`

The schema version on disk is older than the binary expects. This is safe and reversible:

```bash
ee init --workspace . --json
```

Failed migrations leave clear recovery instructions in stderr and never partially apply.

### `error: workspace ambiguous (3 candidates)`

You are inside a worktree, fork, or symlinked path that resolves to multiple registered workspaces. Disambiguate explicitly:

```bash
ee workspace list
ee workspace alias --pick <id> --as <name>
ee --workspace <name> context "..."
```

### `error: embed model not loaded`

The semantic stack is in degraded lexical-only mode. Frankensearch owns model selection; configure it there, then re-embed:

```bash
ee index reembed --workspace .
```

You can also keep running lexical-only; `ee status` shows the degraded capability.

---

## Limitations

`ee` is **honest about what it isn't**.

- **Not a multi-process write fortress.** FrankenSQLite is single-process MVCC WAL. Multiple agents on one machine can read freely, but heavy concurrent writes are coordinated via job locks or the optional daemon's write owner. Don't run a swarm of writers without `ee daemon`.
- **Not an agent harness.** `ee` does not run tools, manage approvals, or own the prompt loop. Use Claude Code or Codex for that.
- **Not a chat UI.** No web frontend, and graph exports are CLI artifacts rather than an interactive web surface.
- **Not a permanent archive.** Forgetting and decay are features. If you need a sealed audit log, export to JSONL and version it in git.
- **No paid LLM APIs out of the box.** Embedding is delegated to Frankensearch, which is local by default. No OpenAI/Anthropic/Google API calls in `ee` itself.
- **Semantic quality is bounded by the model Frankensearch loads.** `ee` does not pick embedding models; configure them through Frankensearch.
- **The CLI is primary; MCP sits on top of it.** If MCP is your only interface, you will find the CLI's surface richer.

---

## FAQ

**Does this replace Claude Code, Codex, or my agent harness?**
No. It is the durable memory those harnesses call. The harness owns the loop; `ee` owns memory.

**Does it phone home or call any external API?**
`ee` itself makes no network calls. Embedding is delegated to Frankensearch, which runs locally by default; if you point Frankensearch at a remote model, that's your decision, not `ee`'s.

**Why no Tokio?**
The runtime is Asupersync, which gives us structured concurrency, capability narrowing, deterministic tests via `LabRuntime`, and an `Outcome` lattice. Tokio is forbidden in the dep tree, audited by CI.

**Why no `rusqlite`?**
The storage layer is FrankenSQLite via SQLModel. `rusqlite` is forbidden in the dep tree, audited by CI.

**Can I use `ee` without `cass`?**
Yes. `cass` is an evidence source, not a hard dependency. Without it, `ee remember`, `ee context`, `ee search`, curation, graph, and packing all work normally.

**How big does the database get?**
On a typical multi-project developer machine, expect 50-500 MB after a year. Cold/warm/hot tiering keeps the hot path small. `ee backup create` produces portable, verified record archives.

**What happens if my index gets corrupted?**
`ee index rebuild` reproduces it from the DB. Indexes are derived assets, so losing them is annoying but never catastrophic.

**Does it work on Windows?**
Yes. Single static binary. PowerShell installer included. Paths follow platform conventions (`%APPDATA%`, `%LOCALAPPDATA%`).

**Can multiple agents on the same machine share one database?**
Yes, that's the default. Reads are concurrent. Writes serialize through a job lock. For heavy multi-writer swarms, run `ee daemon` and let the daemon own the write side.

**How do I integrate with my CI?**
Run `ee context "<the task this CI run is doing>" --json` and pipe relevant rules into your agent's system prompt. JSON output is stable across patch versions.

**Does `ee` ever rewrite my memories silently?**
No. The steward proposes; you approve. Every promotion, consolidation, replacement, and tombstone produces an audit entry visible via `ee why <id>` and the curation queue commands.

**Where do I see the architectural decisions?**
[`docs/adr/`](docs/adr/). Every major subsystem has an ADR with rejected alternatives and verification hooks.

---

## Documentation

| Doc | Purpose |
|---|---|
| [`docs/query-schema.md`](docs/query-schema.md) | EQL-inspired request schema for `ee pack` |
| [`docs/trust-model.md`](docs/trust-model.md) | Memory advisory priority, trust classes, prompt-injection defenses |
| [`docs/agent-outcome-scenarios.md`](docs/agent-outcome-scenarios.md) | North-star agent journey matrix and acceptance scenarios |
| [`docs/agent-ux/insights-onboarding.md`](docs/agent-ux/insights-onboarding.md) | Agent workflow for graph-derived insights, Pack DNA, skyline, and proximity surfaces |
| [`docs/cli-reference/graph-flags.md`](docs/cli-reference/graph-flags.md) | Aggregated graph-related CLI flags by command, including implemented and pending surfaces |
| [`docs/configuration/graph.md`](docs/configuration/graph.md) | Graph feature flags, thresholds, and tuning guidance |
| [`docs/architecture/graph-snapshots.md`](docs/architecture/graph-snapshots.md) | Graph snapshot families, lifecycle, locks, budgets, and degraded behavior |
| [`docs/dependency-contract-matrix.md`](docs/dependency-contract-matrix.md) | Franken-stack integration contracts and version pins |
| [`docs/testing-strategy.md`](docs/testing-strategy.md) | Test categories, verification gates, golden test structure |
| [`docs/command_classification.md`](docs/command_classification.md) | Command effect taxonomy and read/write classification |
| [`docs/migration-guide.md`](docs/migration-guide.md) | DB schema migrations and upgrade paths |
| [`docs/toon-output.md`](docs/toon-output.md) | TOON (Text-Only Object Notation) output format |
| [`docs/pack-replay.md`](docs/pack-replay.md) | Pack replay, support-bundle safety, pack-quality operator guidance, and fixture authoring |
| [`docs/adr/0025-replayable-context-pack-selection-ledgers.md`](docs/adr/0025-replayable-context-pack-selection-ledgers.md) | Pack replay/diff ledger contract, freshness states, and support-bundle safety rules |
| [`docs/adr/`](docs/adr/) | Architectural decision records |

---

## About Contributions

*About Contributions:* Please don't take this the wrong way, but I do not accept outside contributions for any of my projects. I simply don't have the mental bandwidth to review anything, and it's my name on the thing, so I'm responsible for any problems it causes; thus, the risk-reward is highly asymmetric from my perspective. I'd also have to worry about other "stakeholders," which seems unwise for tools I mostly make for myself for free. Feel free to submit issues, and even PRs if you want to illustrate a proposed fix, but know I won't merge them directly. Instead, I'll have Claude or Codex review submissions via `gh` and independently decide whether and how to address them. Bug reports in particular are welcome. Sorry if this offends, but I want to avoid wasted time and hurt feelings. I understand this isn't in sync with the prevailing open-source ethos that seeks community contributions, but it's the only way I can move at this velocity and keep my sanity.

---

## License

MIT License (with OpenAI/Anthropic Rider). See [`LICENSE`](LICENSE).

© 2026 Jeffrey Emanuel
