<div align="center">

# Eidetic Engine (`ee`)

**Durable, local-first, explainable memory for coding agents.**

[![CI](https://img.shields.io/github/actions/workflow/status/Dicklesworthstone/eidetic_engine_cli/ci.yml?branch=main&label=CI)](https://github.com/Dicklesworthstone/eidetic_engine_cli/actions)
[![crates.io](https://img.shields.io/crates/v/ee.svg)](https://crates.io/crates/ee)
[![License: MIT+Rider](https://img.shields.io/badge/License-MIT%2BOpenAI%2FAnthropic%20Rider-yellow.svg)](./LICENSE)
[![Rust 2024](https://img.shields.io/badge/rust-2024-orange.svg)](rust-toolchain.toml)
[![No Tokio](https://img.shields.io/badge/runtime-Asupersync-blueviolet.svg)](#hard-requirements)

```bash
curl -fsSL https://raw.githubusercontent.com/Dicklesworthstone/eidetic_engine_cli/main/install.sh | bash
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
| **Context profiles** | `release`, `debug`, `onboarding`, `refactor`, `security`, and more, each with its own quota mix |
| **Local-first** | No cloud. No paid LLM APIs required. Embeddings run locally through Frankensearch |
| **Stable JSON contract** | Every machine-facing command emits versioned JSON with `schema` field and `data_hash` |
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
    --tag rust --tag ci \
    "This project treats clippy warnings as errors with pedantic and nursery enabled."
✓ memory mem_01HQ3K5Z stored (procedural · rule · confidence 0.55)
✓ indexed in 14ms

# 3. Pull session evidence from your cass history
$ ee import cass --workspace . --since 30d --json | jq '.summary'
{
  "sessions_imported": 47,
  "evidence_spans": 312,
  "candidates_proposed": 8,
  "duration_ms": 2341
}

# 4. Ask for context before working
$ ee context "fix the failing release workflow" --workspace . --profile release
## Project Rules
- Run `cargo fmt --check` before tagging  (mem_01HQ3K5Z · confidence 0.71)
- Push release changes to main after verification  (mem_01HPCC3T · confidence 0.92)

## Prior Failures
- Release v0.2.13 failed because release artifacts were generated from stale branch state
  evidence: cass session 7f4e · 2026-03-12

## Verification Commands
  cargo test --lib && cargo clippy -- -D warnings && ./scripts/e2e_test.sh

## Warnings
⚠  HIGH  Forced pushes around release time have caused user-visible installer staleness

provenance footer: 14 memories, 3 sessions, 1 graph snapshot, pack hash 4b1c…7e90

# 5. Ask why a memory was selected
$ ee why mem_01HPCC3T --json | jq '.score_components'
{
  "lexical_bm25": 8.42,
  "semantic_cosine": 0.71,
  "recency_decay": 0.88,
  "confidence": 0.92,
  "graph_pagerank_boost": 0.15,
  "profile_bonus": 0.30,
  "final": 9.04
}

# 6. Record that the rule helped
$ ee outcome --memory mem_01HQ3K5Z --helpful --note "Caught a clippy regression"
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

Every essential feature works as a one-shot CLI command. The daemon (`ee daemon`) is opt-in and adds background indexing, scheduled steward jobs, and a write owner for high-contention multi-agent workloads. No core command requires it.

### 4. Deterministic By Default

Given the same database, indexes, config, profile, budget, seed, and query, the JSON output is byte-stable, ranking ties resolve deterministically, and context pack hashes reproduce exactly. Golden tests assert this.

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

### Quick install (recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/Dicklesworthstone/eidetic_engine_cli/main/install.sh | bash
```

Verifies the binary against the published Sigstore bundle, drops it in `~/.local/bin/ee`, and runs `ee doctor` to confirm.

PowerShell (Windows):

```powershell
iwr -useb https://raw.githubusercontent.com/Dicklesworthstone/eidetic_engine_cli/main/install.ps1 | iex
```

### Homebrew (macOS / Linux)

```bash
brew install Dicklesworthstone/tap/ee
```

### Cargo

```bash
cargo install ee
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
ee import cass --workspace . --since 90d

# 3. Get context for a task
ee context "what should I know before refactoring the storage layer?" \
  --workspace . --profile refactor --max-tokens 4000 --format markdown

# 4. When you learn something durable, capture it
ee remember --workspace . --level procedural --kind rule \
  --tag rust --tag testing \
  "Integration tests must hit a real Postgres instance, never a mock. See incident 2025-Q3."

# 5. After a session, distill it
ee review session --cass-session <session-id> --propose
ee curate candidates --workspace .
ee curate apply <candidate-id>

# 6. Search at any time
ee search "release failure clippy" --workspace . --limit 20 --explain --json
```

That's the core loop.

---

## Command Reference

`ee` is organized into nine command groups. Run `ee <group> --help` for full details.

### Core workflow

| Command | Purpose |
|---|---|
| `ee init [--workspace .]` | Create or open a workspace, run migrations, prepare indexes |
| `ee status [--json]` | DB generation, index generation, degraded capabilities, recent jobs |
| `ee doctor [--json]` | Health checks with repair commands for every failure |
| `ee context "<task>" [--profile <p>] [--max-tokens N] [--format <fmt>]` | Assemble a task-specific context pack (the headline command) |
| `ee search "<query>" [--limit N] [--explain] [--json]` | Hybrid retrieval over memories, sessions, rules, evidence |
| `ee remember "<text>" --level <l> [--kind <k>] [--tag <t>]` | Capture a durable memory |
| `ee outcome --memory <id> --helpful\|--harmful [--note "<note>"]` | Record feedback, updating utility/confidence |
| `ee why <memory-id> [--json]` | Explain why a memory was selected, scored, or curated the way it was |
| `ee pack --query-file task.eeq.json --max-tokens N --format toon` | Build a pack from an explicit EQL query document |

### Import & ingestion

| Command | Purpose |
|---|---|
| `ee import cass --workspace . [--since 30d]` | Pull session evidence from `coding_agent_session_search` |
| `ee import jsonl <file>` | Restore from a `ee export jsonl` archive |
| `ee import eidetic-legacy --source <path> --dry-run` | One-time migration of legacy Eidetic Engine artifacts (read-only) |

### Curation & rules

| Command | Purpose |
|---|---|
| `ee review session --cass-session <id> --propose` | Distill a session into proposed memories/rules |
| `ee curate candidates [--workspace .]` | List pending curation candidates |
| `ee curate validate <id>` | Run validation (specificity, duplication, scope, evidence) |
| `ee curate apply <id>` / `accept <id>` / `reject <id>` / `snooze <id>` / `merge <a> <b>` | Lifecycle transitions |
| `ee curate retire <rule-id> --reason "..."` / `tombstone <id>` | Non-destructive removal |
| `ee rule list` / `show <id>` / `add` / `update` / `mark <id> --as <state>` | Direct rule management |

### Memory inspection

| Command | Purpose |
|---|---|
| `ee memory show <id> [--json]` | Full record with provenance, links, audit trail |
| `ee memory list [--workspace .] [--level <l>] [--kind <k>]` | Filtered listing |
| `ee memory link <a> <b> --relation <r>` | Add a typed link between memories |
| `ee memory tags <id> --add foo --remove bar` | Tag management |
| `ee memory expire <id> [--reason "..."]` | Mark stale without deleting |

### Graph

| Command | Purpose |
|---|---|
| `ee graph refresh [--workspace .]` | Rebuild graph snapshot from current DB |
| `ee graph neighborhood <id> --hops N` | Expand around a memory/session/rule |
| `ee graph centrality --kind memory` | PageRank / betweenness / eigenvector |
| `ee graph communities` | Community detection with deterministic seeds |
| `ee graph path <src> <dst>` | Shortest evidence path between two nodes |
| `ee graph explain-link <src> <dst>` | Why these are connected, with witness |

### Index

| Command | Purpose |
|---|---|
| `ee index status` / `rebuild` / `reembed` / `vacuum` | Manage derived search indexes (Frankensearch owns model selection) |

### Workspace, profile, db

| Command | Purpose |
|---|---|
| `ee workspace resolve` / `list` / `alias <name>` | Identity, monorepo subscopes, forks, worktrees |
| `ee profile list` / `show <name>` | Inspect built-in or user-defined context profiles |
| `ee db status` / `migrate` / `check` / `backup` | Database lifecycle |

### Backup, restore, export

| Command | Purpose |
|---|---|
| `ee backup create [--label <name>]` | Create a verified backup with manifest |
| `ee backup list` / `verify <id>` / `inspect <id>` | Audit existing backups |
| `ee restore <backup-id> --to <path>` | Side-path restore (never overwrites without explicit confirmation) |
| `ee export jsonl > snapshot.jsonl` | Stream durable records to a portable JSONL file |

### Diagnostics, eval, ops

| Command | Purpose |
|---|---|
| `ee eval run` / `report` | Run retrieval-quality evaluation on shipped fixtures |
| `ee job list` / `show <id>` | Inspect indexing/import/steward jobs |
| `ee daemon` | Optional supervised maintenance daemon |
| `ee completion <shell>` | Generate shell completions |

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
since   = "90d"                     # default --since for `ee import cass`

[search]
default_speed   = "balanced"         # fast | balanced | thorough
lexical_weight  = 0.45
semantic_weight = 0.45
graph_weight    = 0.10

[pack]
default_profile  = "default"
default_format   = "markdown"
default_max_tokens = 4000
mmr_lambda       = 0.7
candidate_pool   = 100

[curation]
duplicate_similarity = 0.92
harmful_weight       = 2.5            # harmful feedback hits harder than helpful
decay_half_life_days = 60

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

Memory `kind` is orthogonal: `rule`, `fact`, `decision`, `failure`, `command`, `convention`, `anti-pattern`, `risk`, `playbook-step`, …

Every memory carries: `id`, `level`, `kind`, `content`, `content_hash`, `tags[]`, `confidence`, `utility`, `importance`, `created_at`, `last_seen_at`, `access_count`, `source_type`, `source_uri`, `evidence_spans[]`, `links[]`, `trust_class`.

---

## Context Profiles

Different tasks need different memory mixes. `--profile` shifts quotas and pinned sections without bypassing trust or privacy:

| Profile | Bias |
|---|---|
| `default` | Balanced rules, sessions, decisions, warnings |
| `onboarding` | Project conventions, architecture, common commands, recent successful sessions |
| `debug` | Similar failures, fixes, logs, test commands, error patterns |
| `release` | Release rules, prior release incidents, verification checklists, branch/package warnings |
| `review` | Coding standards, known bug classes, risky files, testing expectations |
| `refactor` | Architecture decisions, invariants, coupling, previous refactor failures |
| `security` | Secrets, auth, destructive actions, policy memories, high-severity warnings |
| `performance` | Benchmarks, hot paths, previous optimization sessions, measurement rules |
| `migration` | Dependency decisions, migration playbooks, compatibility traps |

Define your own:

```toml
# ~/.config/ee/profiles/release-strict.toml
extends = "release"
quotas = { rules = 0.4, failures = 0.3, verification = 0.3 }
pinned_sections = ["release", "branch_safety"]
```

---

## CASS Integration

`ee` consumes `coding_agent_session_search` (`cass`) as the raw session source; it does **not** duplicate the underlying store. Every fact imported from a session carries a provenance URI back to the exact session and line range.

```bash
# Discover what cass has
ee import cass --workspace . --since 30d --dry-run --json

# Real import (idempotent, resumable, ledger-tracked)
ee import cass --workspace . --since 30d

# Distill a single session into proposed memories
ee review session --cass-session 7f4e --propose
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
  ee outcome --memory <id> --helpful
  ee outcome --memory <id> --harmful
```

Or wire it into a PreToolUse hook that injects context before risky commands. The `ee context` JSON is stable and parseable.

### Codex

Codex shells out, so the same calls work. The output of `ee context "<task>" --json` is designed to drop directly into a system or developer message.

### MCP

Optional MCP adapter (feature-gated, off by default):

```bash
cargo install ee --features mcp
ee daemon --mcp-stdio
```

Tools mirror the CLI: `ee_context`, `ee_search`, `ee_remember`, `ee_outcome`, `ee_curate_candidates`, `ee_memory_show`. Schemas match CLI JSON exactly; the CLI is the compatibility contract.

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
# Verified backup
ee backup create --label pre-refactor
✓ backup bk_01HQ4… (32 MB) verified

# List
ee backup list

# Inspect contents without restoring
ee backup inspect bk_01HQ4… --json

# Restore to a side path (never overwrites without explicit --in-place)
ee restore bk_01HQ4… --to ~/ee-restored/
```

Backups include the DB, the index manifest, the graph snapshot, the curation audit log, and a `manifest.json` with content hashes. Verification re-hashes everything on disk.

---

## Performance

Measured on a 2024 MacBook Pro M3 against a workspace with 25 projects, 14k memories, 8k imported CASS sessions, ~120k indexed documents:

| Operation | p50 | p99 |
|---|---:|---:|
| `ee remember` (single record) | 8 ms | 22 ms |
| `ee search "<q>"` (hybrid) | 38 ms | 110 ms |
| `ee context "<task>"` (markdown, 4k tokens) | 95 ms | 240 ms |
| `ee why <id>` | 6 ms | 14 ms |
| `ee import cass --since 30d` (cold) | 4.1 s | 11 s |
| `ee graph refresh` (full rebuild) | 2.3 s | 5.8 s |
| `ee index rebuild` (full) | 18 s | 41 s |

Performance budgets are enforced in CI. Regressions panic the bench job.

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

# Or disable
ee config set cass.enabled false
```

`ee` continues to work without `cass`; explicit `ee remember` is unaffected.

### `error: migration required`

The schema version on disk is older than the binary expects. This is safe and reversible:

```bash
ee db migrate --json
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
- **Not a chat UI.** No web frontend, no inline visualizer beyond `ee graph --export-graph file.html` (which produces a static page).
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
On a typical multi-project developer machine, expect 50–500 MB after a year. Cold/warm/hot tiering keeps the hot path small. `ee db backup` and `ee export jsonl` produce portable archives.

**What happens if my index gets corrupted?**
`ee index rebuild` reproduces it from the DB. Indexes are derived assets, so losing them is annoying but never catastrophic.

**Does it work on Windows?**
Yes. Single static binary. PowerShell installer included. Paths follow platform conventions (`%APPDATA%`, `%LOCALAPPDATA%`).

**Can multiple agents on the same machine share one database?**
Yes, that's the default. Reads are concurrent. Writes serialize through a job lock. For heavy multi-writer swarms, run `ee daemon` and let the daemon own the write side.

**How do I integrate with my CI?**
Run `ee context "<the task this CI run is doing>" --json` and pipe relevant rules into your agent's system prompt. JSON output is stable across patch versions.

**Does `ee` ever rewrite my memories silently?**
No. The steward proposes; you approve. Every promotion, consolidation, replacement, and tombstone produces an audit entry visible via `ee why <id>` and `ee curate review`.

**Where do I see the architectural decisions?**
[`docs/adr/`](docs/adr/). Every major subsystem has an ADR with rejected alternatives and verification hooks.

---

## Documentation

| Doc | Purpose |
|---|---|
| [`docs/storage.md`](docs/storage.md) | DB layout, migrations, backup |
| [`docs/query-schema.md`](docs/query-schema.md) | EQL-inspired request schema for `ee pack` |
| [`docs/context-packs.md`](docs/context-packs.md) | Packing algorithm and output contracts |
| [`docs/cass-integration.md`](docs/cass-integration.md) | Import contracts and provenance model |
| [`docs/scoring.md`](docs/scoring.md) | Confidence, utility, decay, maturity |
| [`docs/graph.md`](docs/graph.md) | Graph model, algorithms, witness format |
| [`docs/integration.md`](docs/integration.md) | Codex, Claude Code, shell usage |
| [`docs/privacy.md`](docs/privacy.md) | Redaction, secret handling, remote-model policy |
| [`docs/diagnostics.md`](docs/diagnostics.md) | `status`, `doctor`, degradation codes, repair plans |
| [`docs/evaluation.md`](docs/evaluation.md) | Eval fixtures, retrieval metrics, pack quality gates |
| [`docs/agent-outcome-scenarios.md`](docs/agent-outcome-scenarios.md) | North-star agent journey matrix and acceptance scenarios |
| [`docs/dependency-contracts.md`](docs/dependency-contracts.md) | Asupersync / SQLModel / FrankenSQLite / CASS / Frankensearch / FrankenNetworkX integration contracts |
| [`docs/trust-model.md`](docs/trust-model.md) | Memory advisory priority, trust classes, prompt-injection defenses |
| [`docs/schema-evolution.md`](docs/schema-evolution.md) | Versioned JSON contracts, JSONL headers, index manifests |
| [`docs/legacy-eidetic-import.md`](docs/legacy-eidetic-import.md) | Old Eidetic Engine artifact mapping |
| [`docs/adr/`](docs/adr/) | Architectural decision records |

---

## About Contributions

*About Contributions:* Please don't take this the wrong way, but I do not accept outside contributions for any of my projects. I simply don't have the mental bandwidth to review anything, and it's my name on the thing, so I'm responsible for any problems it causes; thus, the risk-reward is highly asymmetric from my perspective. I'd also have to worry about other "stakeholders," which seems unwise for tools I mostly make for myself for free. Feel free to submit issues, and even PRs if you want to illustrate a proposed fix, but know I won't merge them directly. Instead, I'll have Claude or Codex review submissions via `gh` and independently decide whether and how to address them. Bug reports in particular are welcome. Sorry if this offends, but I want to avoid wasted time and hurt feelings. I understand this isn't in sync with the prevailing open-source ethos that seeks community contributions, but it's the only way I can move at this velocity and keep my sanity.

---

## License

MIT License (with OpenAI/Anthropic Rider). See [`LICENSE`](LICENSE).

© 2026 Jeffrey Emanuel
