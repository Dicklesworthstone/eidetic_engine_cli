# AGENTS.md — ee (Eidetic Engine CLI)

> Guidelines for AI coding agents working in this Rust codebase.

---

## RULE 0 - THE FUNDAMENTAL OVERRIDE PREROGATIVE

If I tell you to do something, even if it goes against what follows below, YOU MUST LISTEN TO ME. I AM IN CHARGE, NOT YOU.

---

## RULE NUMBER 1: NO FILE DELETION

**YOU ARE NEVER ALLOWED TO DELETE A FILE WITHOUT EXPRESS PERMISSION.** Even a new file that you yourself created, such as a test code file. You have a horrible track record of deleting critically important files or otherwise throwing away tons of expensive work. As a result, you have permanently lost any and all rights to determine that a file or folder should be deleted.

**YOU MUST ALWAYS ASK AND RECEIVE CLEAR, WRITTEN PERMISSION BEFORE EVER DELETING A FILE OR FOLDER OF ANY KIND.**

---

## Irreversible Git & Filesystem Actions — DO NOT EVER BREAK GLASS

1. **Absolutely forbidden commands:** `git reset --hard`, `git clean -fd`, `rm -rf`, or any command that can delete or overwrite code/data must never be run unless the user explicitly provides the exact command and states, in the same message, that they understand and want the irreversible consequences.
2. **No guessing:** If there is any uncertainty about what a command might delete or overwrite, stop immediately and ask the user for specific approval. "I think it's safe" is never acceptable.
3. **Safer alternatives first:** When cleanup or rollbacks are needed, request permission to use non-destructive options (`git status`, `git diff`, `git stash`, copying to backups) before ever considering a destructive command.
4. **Mandatory explicit plan:** Even after explicit user authorization, restate the command verbatim, list exactly what will be affected, and wait for a confirmation that your understanding is correct. Only then may you execute it—if anything remains ambiguous, refuse and escalate.
5. **Document the confirmation:** When running any approved destructive command, record (in the session notes / final response) the exact user text that authorized it, the command actually run, and the execution time. If that record is absent, the operation did not happen.

---

## Git Branch: ONLY Use `main`, NEVER `master`

**The default branch is `main`.** All work happens on `main` — commits, PRs, feature branches all merge to `main`. Never reference `master` in code or docs. If you see `master` anywhere, it's a bug that needs fixing.

---

## Toolchain: Rust & Cargo

We only use **Cargo** in this project, NEVER any other package manager.

- **Edition:** Rust 2024 (nightly required — see `rust-toolchain.toml`)
- **Dependency versions:** Explicit versions for stability
- **Configuration:** Cargo.toml only (single binary crate with library surface in the same package; not a workspace in phase 0)
- **Unsafe code:** Forbidden (`#![forbid(unsafe_code)]`)

### Forbidden Dependencies (Hard Rule, Audited By CI)

These are **not allowed** anywhere in the dependency tree. If a transitive dependency pulls one in, the feature must be disabled or the dep quarantined behind an explicit adapter with a removal plan.

| Crate | Reason |
|-------|--------|
| `tokio`, `tokio-util` | Asupersync is the runtime; no Tokio. |
| `async-std`, `smol` | Same — only Asupersync. |
| `rusqlite` | FrankenSQLite via SQLModel is the storage layer. |
| `sqlx`, `diesel`, `sea-orm` | SQLModel is the ORM. |
| `petgraph` | FrankenNetworkX is the graph layer. |
| `hyper`, `axum`, `tower`, `reqwest` | No HTTP stacks in core. Adapters live behind feature flags. |

Run `cargo tree -e features` and grep for these crates. CI must fail if any appear.

### Key Dependencies (Phase 0)

| Crate | Purpose |
|-------|---------|
| `asupersync` | Runtime: `Cx`, `Scope`, `Outcome`, budgets, capabilities, supervision |
| `fsqlite` + `fsqlite-core` + `fsqlite-types` + `fsqlite-error` | FrankenSQLite — durable source of truth |
| `fsqlite-ext-fts5`, `fsqlite-ext-json` | Lexical FTS fallback and JSON ops |
| `sqlmodel` (+ `-core`, `-query`, `-schema`, `-session`, `-pool`, `-frankensqlite`) | ORM and migrations on top of FrankenSQLite |
| `frankensearch` | Hybrid lexical + semantic retrieval (`TwoTierSearcher`) |
| `fnx-runtime`, `fnx-classes`, `fnx-algorithms`, `fnx-cgse`, `fnx-convert` | FrankenNetworkX graph analytics |
| `clap` + `clap_complete` | CLI argument parsing with shell completions |
| `serde` + `serde_json` + `serde_yaml` | Stable JSON/YAML output contracts |
| `toml` + `toml_edit` | Config parsing with formatting preservation |
| `chrono` | RFC 3339 timestamps |
| `uuid` (v7) | Time-ordered IDs |
| `blake3`, `sha2` | Content hashing, pack hashes, source hashes |
| `tiktoken-rs` | Token budgeting for context packs |
| `tracing` + `tracing-subscriber` | Structured logging and diagnostics |
| `rust-mcp-sdk` (optional, behind `mcp` feature) | Optional MCP adapter — must not duplicate CLI logic |

### Feature Flags

```toml
[features]
default = ["fts5", "json", "embed-fast", "lexical-bm25"]
fts5 = ["fsqlite-ext-fts5"]
json = ["fsqlite-ext-json"]
embed-fast = ["frankensearch/model2vec"]
embed-quality = ["frankensearch/fastembed"]
lexical-bm25 = ["frankensearch/lexical"]
mcp = ["dep:rust-mcp-sdk"]
serve = []
```

---

## Code Editing Discipline

### No Script-Based Changes

**NEVER** run a script that processes/changes code files in this repo. Brittle regex-based transformations create far more problems than they solve.

- **Always make code changes manually**, even when there are many instances
- For many simple changes: use parallel subagents
- For subtle/complex changes: do them methodically yourself

### No File Proliferation

If you want to change something or add a feature, **revise existing code files in place**.

**NEVER** create variations like:
- `mainV2.rs`
- `main_improved.rs`
- `main_enhanced.rs`

New files are reserved for **genuinely new functionality** that makes zero sense to include in any existing file. The bar for creating new files is **incredibly high**. The plan calls for a single binary crate with a library surface in the same package — do not split into multiple crates until the dependency graph clearly justifies it.

---

## Backwards Compatibility

We do not care about backwards compatibility—we're in early development with no users. We want to do things the **RIGHT** way with **NO TECH DEBT**.

- Never create "compatibility shims"
- Never create wrapper functions for deprecated APIs
- Just fix the code directly

ADRs record decisions; they are not excuses for keeping old APIs alive.

---

## Compiler Checks (CRITICAL)

**After any substantive code changes, you MUST verify no errors were introduced:**

```bash
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

If you see errors, **carefully understand and resolve each issue**. Read sufficient context to fix them the RIGHT way.

---

## Testing

### Testing Policy

Every module includes inline `#[cfg(test)]` unit tests alongside the implementation. Tests must cover:
- Happy path
- Edge cases (empty input, max values, boundary conditions)
- Error conditions

### Test Categories Required By The Plan

| Category | Purpose |
|----------|---------|
| Unit tests | Per-module behavior, scoring math, ID generation, hashing |
| Integration tests | End-to-end CLI flows against temporary DBs and indexes |
| Deterministic runtime tests | Asupersync `LabRuntime` tests for cancellation, budgets, supervision |
| Golden tests | Stable JSON / Markdown / TOON output contracts (`ee context`, `ee search`, `ee why`, etc.) |
| Memory evaluation harness | Retrieval quality fixtures with deterministic embeddings |
| Property and fuzz tests | Query parser, packing budgets, JSONL header parsing |
| Forbidden-dependency audit | Fails if `tokio`, `rusqlite`, `petgraph`, etc. appear |

### Determinism Rules

- Given the same database, indexes, config, and query, JSON output must be stable.
- Ranking ties must be deterministic.
- Context pack hashes must be reproducible.
- Tests must use `LabRuntime` and a hash embedder (not real models) where applicable.

---

## Third-Party Library Usage

If you aren't 100% sure how to use a third-party library, **SEARCH ONLINE** to find the latest documentation and current best practices. The franken-stack crates (`asupersync`, `fsqlite`, `sqlmodel`, `frankensearch`, `franken_networkx`) live in `/dp/` — read their source rather than guessing.

---

## ee (Eidetic Engine CLI) — This Project

**This is the project you're working on.** `ee` is a local-first Rust CLI memory substrate for coding agents. It captures durable facts, work history, decisions, procedural rules, failures, and evidence; indexes them with hybrid search; reasons over their relationships with graph algorithms; and emits compact, explainable context packs that an existing agent harness (Codex, Claude Code, etc.) can consume before, during, and after work.

### The Controlling Idea

> `ee` does not replace agent harnesses. It is the durable memory layer those harnesses call.

Agents push evidence and lessons in; agents pull context out. `ee` should not reach into the agent's work unless explicitly invoked by a hook or command. If a proposed feature requires `ee` to become the agent loop, planner, tool router, chat shell, autonomous worker, or central web service, it is probably a regression toward the old Eidetic Engine and should be rejected.

### Hard Requirements (Non-Negotiable)

- The binary is named `ee`.
- Implementation language is Rust (edition 2024, nightly).
- Runtime foundation is `/dp/asupersync`. **No Tokio.**
- Database foundation is `/dp/frankensqlite` through `/dp/sqlmodel_rust`. **No `rusqlite`.**
- Raw session history comes from `/dp/coding_agent_session_search` (`cass`) — consume robot/JSON output, do not duplicate the store.
- General search is `/dp/frankensearch` — no custom RRF/BM25/vector code.
- Graph analytics is `/dp/franken_networkx` — no `petgraph`, no hand-rolled algorithms for core metrics.
- Procedural-memory concepts come from `/dp/cass_memory_system` (concept only — no TS runtime dependency).
- The original Eidetic Engine repos are design sources only; do not copy their Python/FastAPI/MCP-first architecture.
- The first deliverable is a robust local CLI, not a web app, daemon, or MCP server.
- All machine-facing commands must support stable JSON output.
- All generated context must include provenance and an explanation of why it was selected.

### Non-Goals For V1

- No replacement for Codex, Claude Code, or other agent harnesses.
- No general-purpose workflow engine.
- No web UI before the CLI is useful.
- MCP is not required for normal operation (it is an optional adapter).
- No paid LLM APIs required.
- No reliance on multi-process concurrent SQLite writers for correctness.
- No leading with Browser Edition / QUIC / RaptorQ / distributed surfaces from Asupersync.
- No custom RRF / vector store / BM25 — use Frankensearch.
- No secrets in context packs.
- Forgetting and decay are features; do not try to make all memories permanent.

### Five Core Jobs

1. **Ingest** — import session history (`cass`) and explicit notes into a durable local store
2. **Retrieve** — hybrid search over memories, sessions, rules, artifacts, and decisions
3. **Pack** — assemble compact task-specific context with provenance and explanations
4. **Learn** — distill repeated experiences into procedural rules and anti-patterns
5. **Maintain** — link, score, decay, consolidate, validate, and repair memory over time

The most important workflow is:

```bash
ee context "fix failing release workflow" --workspace . --max-tokens 4000 --json
```

### High-Level Architecture

```text
Agent/Human
  |
  | ee context/search/remember/import/curate
  v
ee-cli  ->  ee-core
                |
                +--> ee-db        (SQLModel / FrankenSQLite — source of truth)
                +--> ee-search    (Frankensearch — derived lexical + vector indexes)
                +--> ee-cass      (imports evidence from coding_agent_session_search)
                +--> ee-graph     (FrankenNetworkX graph projections)
                +--> ee-pack      (deterministic context packs with provenance)
                +--> ee-curate    (procedural rules, candidates, validation)
                +--> ee-steward   (maintenance jobs, optional supervised daemon)
                +--> ee-policy    (redaction, privacy, scope, retention, trust)
                +--> ee-output    (JSON / Markdown / TOON / human terminal)
                +--> ee-config / ee-hooks / ee-mcp / ee-serve / ee-obs
```

### Module Layout (Phase 0 — Single Crate)

These are module boundaries inside one binary crate. They become separate crates only when the dependency graph or release process clearly justifies it.

```text
src/
  main.rs
  lib.rs
  cli/        # Clap commands, process I/O, formatting, exit codes
  core/       # Use cases, application services, runtime wiring
  models/     # Domain types, IDs, enums, output contracts
  db/         # SQLModel models, migrations, repositories
  search/     # Frankensearch integration, indexing, scoring
  cass/       # CASS import adapter (robot/JSON contracts)
  graph/      # FrankenNetworkX projection and metrics
  pack/       # Context packing, token budgets, MMR, provenance
  curate/     # Rule candidates, validation, feedback, maturity
  steward/    # Maintenance jobs, optional daemon
  policy/     # Redaction, privacy, scope, retention, trust
  output/     # JSON / Markdown / TOON / human rendering
  config/     # Config loading, path resolution, workspace discovery
  hooks/      # Optional hook helpers for agent harnesses
  mcp/        # Optional MCP stdio adapter (feature-gated)
  serve/      # Optional localhost HTTP/SSE adapter (feature-gated)
  obs/        # Tracing, audit log, diagnostics
docs/
  adr/        # Architectural decision records
  ...
tests/
  fixtures/
```

### Dependency Direction (Strict)

```text
cli  ->  core  ->  { db, search, cass, graph, pack, curate, policy, output }
                       |
                       v
                    models
```

- Lower-level modules must not depend on `cli`.
- Domain types live in `models`, not in CLI code.
- Repositories return domain types, not CLI output structs.
- Search indexes are written through `search`, never directly.
- Graph metrics are derived from DB records; never hand-maintained elsewhere.

### Walking Skeleton (First Implementation Slice)

The walking skeleton is the smallest build that proves the architecture, not the smallest code that compiles. It must demonstrate:

```text
manual memory -> FrankenSQLite -> search document -> Frankensearch
              -> context pack -> pack record -> why output
```

Required commands for the first slice:

```bash
ee init --workspace .
ee remember --workspace . --level procedural --kind rule "Run cargo fmt --check before release." --json
ee search "format before release" --workspace . --json
ee context "prepare release" --workspace . --format markdown
ee why <memory-id> --json
ee status --json
```

Acceptance gate:

- All commands work without daemon mode
- All commands have stable JSON mode
- Memory is stored in FrankenSQLite through `ee-db`
- Search results come from Frankensearch or a documented degraded lexical path
- Context pack includes provenance
- `ee why` explains storage, retrieval, and pack selection
- Pack record is persisted
- `ee status` reports DB, index, and degraded capabilities
- Cancellation tests cover at least one command path
- No Tokio or `rusqlite` dependency appears in the dep tree

**Do NOT start with:** daemon, MCP server, web UI, automatic LLM curation, graph analytics, or JSONL sync. Those come after the core context loop works.

### Product Principles (Apply When In Doubt)

- **Local first** — no cloud dependency required; remote APIs and model downloads are explicit opt-in
- **Harness agnostic** — works from any shell; never assumes control of the agent loop
- **CLI first, daemon later** — no core command may require the daemon in v1
- **Deterministic by default** — same DB + indexes + config + query → stable JSON output
- **Explainable retrieval** — every returned memory answers: why selected, what supports it, how fresh, how reliable, what scores mattered
- **Search indexes are derived assets** — FrankenSQLite/SQLModel are truth; indexes are rebuildable
- **Graceful degradation** — semantic down → lexical works; graph stale → retrieval works; CASS missing → explicit memories work
- **No silent memory mutation** — every promotion, consolidation, or tombstone is audited
- **Evidence over vibes** — rules without source/feedback/validation stay low-confidence

### CLI Output Rules

- JSON data goes to **stdout**.
- Human diagnostics go to **stderr**.
- `--json` output must be parseable and stable (stable field names, explicit schema, stable ordering, no terminal styling, no localized strings in machine-critical fields).
- Do not mix progress bars into JSON stdout.
- Long-running commands use stderr progress only when attached to a TTY.
- Color only when stderr is a TTY and `--no-color` is not set.

Human error shape:

```text
error: search index is stale

The memory database has generation 12, but the search index was built at generation 9.

Next:
  ee index rebuild --workspace .

Details:
  ee index status --workspace . --json
```

JSON error shape:

```json
{
  "schema": "ee.error.v1",
  "error": {
    "code": "search_index_stale",
    "message": "Search index is stale.",
    "severity": "medium",
    "repair": "ee index rebuild --workspace .",
    "details": { "databaseGeneration": 12, "indexGeneration": 9 }
  }
}
```

### Exit Codes

| Code | Meaning |
| ---- | ------- |
| 0 | success |
| 1 | usage error |
| 2 | configuration error |
| 3 | storage error |
| 4 | search/index error |
| 5 | import error |
| 6 | degraded but command could not satisfy required mode |
| 7 | policy denied operation |
| 8 | migration required |

### Architectural Decision Records

ADRs live in `docs/adr/` and capture the "why" behind major decisions so future contributors don't re-litigate settled choices or accidentally rebuild the old system.

Initial ADRs to write:

- CLI-first memory substrate (no daemon/MCP/web-first regressions)
- FrankenSQLite + SQLModel as source of truth
- Native Asupersync runtime (no Tokio, `&Cx` first, `Outcome` preserved)
- Frankensearch for retrieval (no custom RRF/BM25/vector stack)
- CASS as the raw session source (no duplicate stores)
- Procedural memory requires evidence (no promotion without provenance)
- Context packs as primary UX
- Graph metrics are explainable derived features

Every major new subsystem gets an ADR before implementation, including rejected alternatives and at least one verification hook.

---

## CI/CD Pipeline

The CI must enforce, at minimum:

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings` (pedantic + nursery enabled)
- `cargo test` (unit + integration + golden)
- Forbidden-dependency audit (`cargo tree -e features` greps for `tokio`, `rusqlite`, `petgraph`, etc. — fail on hit)
- Determinism check (same fixture → same JSON / pack hash)
- UBS static analysis on changed Rust files

Coverage thresholds, performance budgets, and benchmark gates should be added milestone-by-milestone as the corresponding subsystems land. Documented thresholds in this file must stay in sync with the workflow — add a `coverage_threshold_docs` test as soon as coverage gates exist.

---

## Release Process

When fixes are ready for release:

### 1. Verify Locally

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo tree -e features | grep -E '(tokio|rusqlite|petgraph|sqlx|diesel)' && echo "FORBIDDEN DEP" || echo "ok"
```

### 2. Commit Changes

```bash
git add -A
git commit -m "fix: description of fixes

- List specific fixes
- Include any breaking changes

Co-Authored-By: Claude <noreply@anthropic.com>"
```

### 3. Bump Version

The version in `Cargo.toml` determines the release tag.

- **Patch** (0.1.x → 0.1.x+1): Bug fixes
- **Minor** (0.1.x → 0.2.0): New features, backward compatible
- **Major** (0.x → 1.0): Breaking changes (rare; we are still pre-1.0)

Plan-targeted version milestones:

| Version | Slice |
| ------- | ----- |
| `0.1.0` | Walking skeleton: `init`, `remember`, `search`, `context`, `why`, `status` |
| `0.2.0` | CASS import MVP, indexing queue |
| `0.3.0` | Procedural rules and curation |
| `0.4.0` | Graph analytics |
| `0.5.0` | Steward + optional daemon |
| `0.6.0` | Export, backup, MCP adapter |

### 4. Push And Trigger Release

```bash
git push origin main
```

The release workflow should detect a version change in `Cargo.toml`, tag, build cross-platform binaries, sign with Sigstore, and upload to GitHub Releases. Expected assets per release:

- `ee-{target}.tar.xz`
- `ee-{target}.tar.xz.sha256`
- `ee-{target}.tar.xz.sigstore.json`
- `install.sh`, `install.ps1`

### 5. Verify

```bash
gh release list --limit 5
gh release view v0.1.0
```

---

## MCP Agent Mail — Multi-Agent Coordination

A mail-like layer that lets coding agents coordinate asynchronously via MCP tools and resources. Provides identities, inbox/outbox, searchable threads, and advisory file reservations with human-auditable artifacts in Git.

### Why It's Useful

- **Prevents conflicts:** Explicit file reservations (leases) for files/globs
- **Token-efficient:** Messages stored in per-project archive, not in context
- **Quick reads:** `resource://inbox/...`, `resource://thread/...`

### Same Repository Workflow

1. **Register identity:**
   ```
   ensure_project(project_key=<abs-path>)
   register_agent(project_key, program, model)
   ```

2. **Reserve files before editing:**
   ```
   file_reservation_paths(project_key, agent_name, ["src/**"], ttl_seconds=3600, exclusive=true)
   ```

3. **Communicate with threads:**
   ```
   send_message(..., thread_id="FEAT-123")
   fetch_inbox(project_key, agent_name)
   acknowledge_message(project_key, agent_name, message_id)
   ```

4. **Quick reads:**
   ```
   resource://inbox/{Agent}?project=<abs-path>&limit=20
   resource://thread/{id}?project=<abs-path>&include_bodies=true
   ```

### Macros vs Granular Tools

- **Prefer macros for speed:** `macro_start_session`, `macro_prepare_thread`, `macro_file_reservation_cycle`, `macro_contact_handshake`
- **Use granular tools for control:** `register_agent`, `file_reservation_paths`, `send_message`, `fetch_inbox`, `acknowledge_message`

### Common Pitfalls

- `"from_agent not registered"`: Always `register_agent` in the correct `project_key` first
- `"FILE_RESERVATION_CONFLICT"`: Adjust patterns, wait for expiry, or use non-exclusive reservation
- **Auth errors:** If JWT+JWKS enabled, include bearer token with matching `kid`

---

## Beads (br) — Dependency-Aware Issue Tracking

Beads provides a lightweight, dependency-aware issue database and CLI (`br` - beads_rust) for selecting "ready work," setting priorities, and tracking status. It complements MCP Agent Mail's messaging and file reservations.

**Important:** `br` is non-invasive—it NEVER runs git commands automatically. You must manually commit changes after `br sync --flush-only`.

### Conventions

- **Single source of truth:** Beads for task status/priority/dependencies; Agent Mail for conversation and audit
- **Shared identifiers:** Use Beads issue ID (e.g., `br-123`) as Mail `thread_id` and prefix subjects with `[br-123]`
- **Reservations:** When starting a task, call `file_reservation_paths()` with the issue ID in `reason`

### Typical Agent Flow

1. **Pick ready work (Beads):**
   ```bash
   br ready --json  # Choose highest priority, no blockers
   ```

2. **Reserve edit surface (Mail):**
   ```
   file_reservation_paths(project_key, agent_name, ["src/**"], ttl_seconds=3600, exclusive=true, reason="br-123")
   ```

3. **Announce start (Mail):**
   ```
   send_message(..., thread_id="br-123", subject="[br-123] Start: <title>", ack_required=true)
   ```

4. **Work and update:** Reply in-thread with progress

5. **Complete and release:**
   ```bash
   br close 123 --reason "Completed"
   br sync --flush-only  # Export to JSONL (no git operations)
   ```
   ```
   release_file_reservations(project_key, agent_name, paths=["src/**"])
   ```
   Final Mail reply: `[br-123] Completed` with summary

### Mapping Cheat Sheet

| Concept | Value |
|---------|-------|
| Mail `thread_id` | `br-###` |
| Mail subject | `[br-###] ...` |
| File reservation `reason` | `br-###` |
| Commit messages | Include `br-###` for traceability |

---

## bv — Graph-Aware Triage Engine

bv is a graph-aware triage engine for Beads projects (`.beads/beads.jsonl`). It computes PageRank, betweenness, critical path, cycles, HITS, eigenvector, and k-core metrics deterministically.

**Scope boundary:** bv handles *what to work on* (triage, priority, planning). For agent-to-agent coordination (messaging, work claiming, file reservations), use MCP Agent Mail.

**CRITICAL: Use ONLY `--robot-*` flags. Bare `bv` launches an interactive TUI that blocks your session.**

### The Workflow: Start With Triage

**`bv --robot-triage` is your single entry point.** It returns:
- `quick_ref`: at-a-glance counts + top 3 picks
- `recommendations`: ranked actionable items with scores, reasons, unblock info
- `quick_wins`: low-effort high-impact items
- `blockers_to_clear`: items that unblock the most downstream work
- `project_health`: status/type/priority distributions, graph metrics
- `commands`: copy-paste shell commands for next steps

```bash
bv --robot-triage        # THE MEGA-COMMAND: start here
bv --robot-next          # Minimal: just the single top pick + claim command
```

### Command Reference

**Planning:**
| Command | Returns |
|---------|---------|
| `--robot-plan` | Parallel execution tracks with `unblocks` lists |
| `--robot-priority` | Priority misalignment detection with confidence |

**Graph Analysis:**
| Command | Returns |
|---------|---------|
| `--robot-insights` | Full metrics: PageRank, betweenness, HITS, eigenvector, critical path, cycles, k-core, articulation points, slack |
| `--robot-label-health` | Per-label health: `health_level`, `velocity_score`, `staleness`, `blocked_count` |
| `--robot-label-flow` | Cross-label dependency: `flow_matrix`, `dependencies`, `bottleneck_labels` |
| `--robot-label-attention [--attention-limit=N]` | Attention-ranked labels |

**History & Change Tracking:**
| Command | Returns |
|---------|---------|
| `--robot-history` | Bead-to-commit correlations |
| `--robot-diff --diff-since <ref>` | Changes since ref: new/closed/modified issues, cycles |

**Other:**
| Command | Returns |
|---------|---------|
| `--robot-burndown <sprint>` | Sprint burndown, scope changes, at-risk items |
| `--robot-forecast <id\|all>` | ETA predictions with dependency-aware scheduling |
| `--robot-alerts` | Stale issues, blocking cascades, priority mismatches |
| `--robot-suggest` | Hygiene: duplicates, missing deps, label suggestions |
| `--robot-graph [--graph-format=json\|dot\|mermaid]` | Dependency graph export |
| `--export-graph <file.html>` | Interactive HTML visualization |

### Scoping & Filtering

```bash
bv --robot-plan --label backend              # Scope to label's subgraph
bv --robot-insights --as-of HEAD~30          # Historical point-in-time
bv --recipe actionable --robot-plan          # Pre-filter: ready to work
bv --recipe high-impact --robot-triage       # Pre-filter: top PageRank
bv --robot-triage --robot-triage-by-track    # Group by parallel work streams
bv --robot-triage --robot-triage-by-label    # Group by domain
```

### Understanding Robot Output

**All robot JSON includes:**
- `data_hash` — Fingerprint of source beads.jsonl
- `status` — Per-metric state: `computed|approx|timeout|skipped` + elapsed ms
- `as_of` / `as_of_commit` — Present when using `--as-of`

**Two-phase analysis:**
- **Phase 1 (instant):** degree, topo sort, density
- **Phase 2 (async, 500ms timeout):** PageRank, betweenness, HITS, eigenvector, cycles

### jq Quick Reference

```bash
bv --robot-triage | jq '.quick_ref'                        # At-a-glance summary
bv --robot-triage | jq '.recommendations[0]'               # Top recommendation
bv --robot-plan | jq '.plan.summary.highest_impact'        # Best unblock target
bv --robot-insights | jq '.status'                         # Check metric readiness
bv --robot-insights | jq '.Cycles'                         # Circular deps (must fix!)
```

---

## UBS — Ultimate Bug Scanner

**Golden Rule:** `ubs <changed-files>` before every commit. Exit 0 = safe. Exit >0 = fix & re-run.

### Commands

```bash
ubs file.rs file2.rs                    # Specific files (< 1s) — USE THIS
ubs $(git diff --name-only --cached)    # Staged files — before commit
ubs --only=rust,toml src/               # Language filter (3-5x faster)
ubs --ci --fail-on-warning .            # CI mode — before PR
ubs .                                   # Whole project (ignores target/, Cargo.lock)
```

### Output Format

```
Warning  Category (N errors)
    file.rs:42:5 - Issue description
    Suggested fix
Exit code: 1
```

Parse: `file:line:col` -> location | fix hint -> how to fix | Exit 0/1 -> pass/fail

### Fix Workflow

1. Read finding -> category + fix suggestion
2. Navigate `file:line:col` -> view context
3. Verify real issue (not false positive)
4. Fix root cause (not symptom)
5. Re-run `ubs <file>` -> exit 0
6. Commit

### Bug Severity

- **Critical (always fix):** Memory safety, use-after-free, data races, SQL injection
- **Important (production):** Unwrap panics, resource leaks, overflow checks
- **Contextual (judgment):** TODO/FIXME, println! debugging

---

## RCH — Remote Compilation Helper

RCH offloads `cargo build`, `cargo test`, `cargo clippy`, and other compilation commands to a fleet of 8 remote Contabo VPS workers instead of building locally. This prevents compilation storms from overwhelming csd when many agents run simultaneously.

**RCH is installed at `~/.local/bin/rch` and is hooked into Claude Code's PreToolUse automatically.** Most of the time you don't need to do anything if you are Claude Code — builds are intercepted and offloaded transparently.

To manually offload a build:
```bash
rch exec -- cargo build --release
rch exec -- cargo test
rch exec -- cargo clippy
```

Quick commands:
```bash
rch doctor                    # Health check
rch workers probe --all       # Test connectivity to all 8 workers
rch status                    # Overview of current state
rch queue                     # See active/waiting builds
```

If rch or its workers are unavailable, it fails open — builds run locally as normal.

**Note for Codex/GPT-5.2:** Codex does not have the automatic PreToolUse hook, but you can (and should) still manually offload compute-intensive compilation commands using `rch exec -- <command>`. This avoids local resource contention when multiple agents are building simultaneously.

---

## ast-grep vs ripgrep

**Use `ast-grep` when structure matters.** It parses code and matches AST nodes, ignoring comments/strings, and can **safely rewrite** code.

- Refactors/codemods: rename APIs, change import forms
- Policy checks: enforce patterns across a repo
- Editor/automation: LSP mode, `--json` output

**Use `ripgrep` when text is enough.** Fastest way to grep literals/regex.

- Recon: find strings, TODOs, log lines, config values
- Pre-filter: narrow candidate files before ast-grep

### Rule of Thumb

- Need correctness or **applying changes** -> `ast-grep`
- Need raw speed or **hunting text** -> `rg`
- Often combine: `rg` to shortlist files, then `ast-grep` to match/modify

### Rust Examples

```bash
# Find structured code (ignores comments)
ast-grep run -l Rust -p 'fn $NAME($$$ARGS) -> $RET { $$$BODY }'

# Find all unwrap() calls
ast-grep run -l Rust -p '$EXPR.unwrap()'

# Quick textual hunt
rg -n 'println!' -t rust

# Combine speed + precision
rg -l -t rust 'unwrap\(' | xargs ast-grep run -l Rust -p '$X.unwrap()' --json
```

---

## Morph Warp Grep — AI-Powered Code Search

**Use `mcp__morph-mcp__warp_grep` for exploratory "how does X work?" questions.** An AI agent expands your query, greps the codebase, reads relevant files, and returns precise line ranges with full context.

**Use `ripgrep` for targeted searches.** When you know exactly what you're looking for.

**Use `ast-grep` for structural patterns.** When you need AST precision for matching/rewriting.

### When to Use What

| Scenario | Tool | Why |
|----------|------|-----|
| "How is the context packer wired up?" | `warp_grep` | Exploratory; don't know where to start |
| "Where is the Frankensearch indexer invoked?" | `warp_grep` | Need to understand architecture |
| "Find all uses of `Outcome::ok`" | `ripgrep` | Targeted literal search |
| "Find files with `println!`" | `ripgrep` | Simple pattern |
| "Replace all `unwrap()` with `expect()`" | `ast-grep` | Structural refactor |

### warp_grep Usage

```
mcp__morph-mcp__warp_grep(
  repoPath: "/data/projects/eidetic_engine_cli",
  query: "How does the context packing budget enforcement work?"
)
```

Returns structured results with file paths, line ranges, and extracted code snippets.

### Anti-Patterns

- **Don't** use `warp_grep` to find a specific function name -> use `ripgrep`
- **Don't** use `ripgrep` to understand "how does X work" -> wastes time with manual reads
- **Don't** use `ripgrep` for codemods -> risks collateral edits

<!-- bv-agent-instructions-v1 -->

---

## Beads Workflow Integration

This project uses [beads_rust](https://github.com/Dicklesworthstone/beads_rust) (`br`) for issue tracking. Issues are stored in `.beads/` and tracked in git.

**Important:** `br` is non-invasive—it NEVER executes git commands. After `br sync --flush-only`, you must manually run `git add .beads/ && git commit`.

### Essential Commands

```bash
# View issues (launches TUI - avoid in automated sessions)
bv

# CLI commands for agents (use these instead)
br ready              # Show issues ready to work (no blockers)
br list --status=open # All open issues
br show <id>          # Full issue details with dependencies
br create --title="..." --type=task --priority=2
br update <id> --status=in_progress
br close <id> --reason "Completed"
br close <id1> <id2>  # Close multiple issues at once
br sync --flush-only  # Export to JSONL (NO git operations)
```

### Workflow Pattern

1. **Start**: Run `br ready` to find actionable work
2. **Claim**: Use `br update <id> --status=in_progress`
3. **Work**: Implement the task
4. **Complete**: Use `br close <id>`
5. **Sync**: Run `br sync --flush-only` then manually commit

### Key Concepts

- **Dependencies**: Issues can block other issues. `br ready` shows only unblocked work.
- **Priority**: P0=critical, P1=high, P2=medium, P3=low, P4=backlog (use numbers, not words)
- **Types**: task, bug, feature, epic, question, docs
- **Blocking**: `br dep add <issue> <depends-on>` to add dependencies

### Session Protocol

**Before ending any session, run this checklist:**

```bash
git status              # Check what changed
git add <files>         # Stage code changes
br sync --flush-only    # Export beads to JSONL
git add .beads/         # Stage beads changes
git commit -m "..."     # Commit everything together
git push                # Push to remote
```

### Best Practices

- Check `br ready` at session start to find available work
- Update status as you work (in_progress -> closed)
- Create new issues with `br create` when you discover tasks
- Use descriptive titles and set appropriate priority/type
- Always `br sync --flush-only && git add .beads/` before ending session

<!-- end-bv-agent-instructions -->

## Landing the Plane (Session Completion)

**When ending a work session**, you MUST complete ALL steps below.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds, forbidden-dep audit
3. **Update issue status** - Close finished work, update in-progress items
4. **Sync beads** - `br sync --flush-only` to export to JSONL
5. **Hand off** - Provide context for next session


---

## cass — Cross-Agent Session Search

`cass` indexes prior agent conversations (Claude Code, Codex, Cursor, Gemini, ChatGPT, etc.) so we can reuse solved problems.

**Note:** `cass` is also the *raw session source* that `ee` itself imports. Inside this codebase, treat `cass` as both (a) a tool you use to find prior solutions, and (b) the upstream contract that `ee-cass` consumes via robot/JSON output. Never depend on bare interactive `cass` output — fixtures must pin every consumed JSON contract.

**Rules:** Never run bare `cass` (TUI). Always use `--robot` or `--json`.

### Examples

```bash
cass health
cass search "async runtime" --robot --limit 5
cass view /path/to/session.jsonl -n 42 --json
cass expand /path/to/session.jsonl -n 42 -C 3 --json
cass capabilities --json
cass robot-docs guide
```

### Tips

- Use `--fields minimal` for lean output
- Filter by agent with `--agent`
- Use `--days N` to limit to recent history

stdout is data-only, stderr is diagnostics; exit code 0 means success.

Treat cass as a way to avoid re-solving problems other agents already handled.

---

Note for Codex/GPT-5.2:

You constantly bother me and stop working with concerned questions that look similar to this:

```
Unexpected changes (need guidance)

- Working tree still shows edits I did not make in Cargo.toml, Cargo.lock, src/cli/commands/upgrade.rs, src/storage/sqlite.rs, tests/conformance.rs, tests/storage_deps.rs. Please advise whether to keep/commit/revert these before any further work. I did not touch them.

Next steps (pick one)

1. Decide how to handle the unrelated modified files above so we can resume cleanly.
2. Triage beads_rust-orko (clippy/cargo warnings) and beads_rust-ydqr (rustfmt failures).
3. If you want a full suite run later, fix conformance/clippy blockers and re-run cargo test --all.
```

NEVER EVER DO THAT AGAIN. The answer is literally ALWAYS the same: those are changes created by the potentially dozen of other agents working on the project at the same time. This is not only a common occurrence, it happens multiple times PER MINUTE. The way to deal with it is simple: you NEVER, under ANY CIRCUMSTANCE, stash, revert, overwrite, or otherwise disturb in ANY way the work of other agents. Just treat those changes identically to changes that you yourself made. Just fool yourself into thinking YOU made the changes and simply don't recall it for some reason.

---

## Note on Built-in TODO Functionality

Also, if I ask you to explicitly use your built-in TODO functionality, don't complain about this and say you need to use beads. You can use built-in TODOs if I tell you specifically to do so. Always comply with such orders.
