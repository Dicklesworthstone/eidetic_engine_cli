<div align="center">

<img src="./ee_illustration.webp" alt="Eidetic Engine illustration" width="720">

# Eidetic Engine (`ee`)

**Durable, local-first, explainable memory for coding agents.**

[![CI](https://img.shields.io/github/actions/workflow/status/Dicklesworthstone/eidetic_engine_cli/ci.yml?branch=main&label=CI)](https://github.com/Dicklesworthstone/eidetic_engine_cli/actions)
[![Latest release](https://img.shields.io/github/v/release/Dicklesworthstone/eidetic_engine_cli?label=release)](https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/latest)
[![License: MIT+Rider](https://img.shields.io/badge/License-MIT%2BOpenAI%2FAnthropic%20Rider-yellow.svg)](./LICENSE)
[![Rust 2024](https://img.shields.io/badge/rust-2024-orange.svg)](rust-toolchain.toml)
[![No Tokio](https://img.shields.io/badge/runtime-Asupersync-blueviolet.svg)](#hard-requirements)

**Install**

```bash
curl -fsSL "https://raw.githubusercontent.com/Dicklesworthstone/eidetic_engine_cli/main/install.sh?$(date +%s)" | bash -s -- --easy-mode --verify
```

Always verifies the release binary's SHA-256 checksum, verifies its Sigstore
bundle when one is published and `cosign` is available, drops `ee` into
`~/.local/bin`, repairs `PATH`, installs shell completions, runs a self-test,
and prints guidance for detected agent harnesses; settings remain untouched. Pass
`--require-provenance` for fail-closed signature and SLSA provenance
verification. Pass `--help` (e.g. `bash install.sh --help`) for offline
tarballs, proxy options, `--no-gum`, and `--force` reinstall.

</div>

---

## TL;DR

### Why This Exists

Coding agents forget.

A fresh session re-discovers project conventions, re-reads the same files, and
walks into traps another agent already hit. Bad assumptions become "facts"
because the harness has no durable place to look for decisions, failures,
rules, and evidence from prior runs.

The agent harness owns the loop. `ee` handles the memory layer.

### What `ee` Does

`ee` is a Rust CLI that gives agents a durable, searchable memory layer. It
stores facts, decisions, procedural rules, anti-patterns, session evidence, and
outcomes; indexes them with lexical and semantic search; connects them with
graph features; and emits compact context packs with provenance.

```bash
ee pack "prepare release for this project" --workspace . --max-tokens 4000 --format markdown
```

The command returns a Markdown pack of matching durable memories, such as
project release rules, verification commands, branch traps, and high-severity
warnings. Each item carries an evidence pointer and a score breakdown. Imported
`cass` excerpts become searchable after the import report's indexing action;
they reach the memory-centric pack only after curation links an excerpt to a
distilled memory.

### What You Get

| Capability | What you get |
|---|---|
| **Hybrid retrieval** | BM25 + neural-local vector search via Frankensearch's `TwoTierSearcher`; default builds use the pinned `potion-multilingual-128M` Model2Vec embedder, with deterministic hash fallback only when the local model path is unavailable |
| **Cancellation-safe index intake** | Single-memory and coalesced jobs build a complete validated generation in staging, publish it through a masked rollback-guarded in-process tail, and preserve exact cooperative-cancellation reasons with no partial active index; filesystem/DB hard-crash reconciliation is a separate protocol |
| **Explainable scores** | Every returned memory shows component scores, freshness, confidence, and which sources support it |
| **Typed memory fields** | Registry-backed sidecars for failures, decisions, commands, rules, conventions, risks, and anti-patterns; search filters use stable field names instead of prose parsing |
| **Procedural rules with decay** | Confidence ages out, harmful feedback demotes faster than helpful feedback promotes |
| **Anti-patterns first-class** | Explicit advisory queries surface matching risk, anti-pattern, and failure memories with provenance |
| **Memory hygiene** | `ee curate doctor` ranks content debt, `ee learn gaps` turns missed demand into capture templates, and steward snapshots show whether hygiene is improving |
| **Graph-aware** | PageRank, HITS, PPR, Gomory-Hu proximity, dominance, causal paths, structural health, Pack DNA, and skyline views |
| **CASS session import** | Mines your existing `cass` corpus (Claude Code, Codex, Cursor, Gemini, ChatGPT) for evidence |
| **Context profiles** | `compact`, `balanced`, `grounding`, `orientation`, `thorough`, and `submodular` quota/objective mixes |
| **Local-first** | No cloud service or paid LLM API is required. Embeddings run locally through Frankensearch, with one-time pinned model download and offline hash fallback |
| **Stable JSON contract** | Every machine-facing command emits versioned JSON with `schema` field for parsing and validation |
| **Deterministic** | Same DB + indexes + config + query → identical pack hash |
| **Cancellation-aware core** | Runtime-facing async APIs use Asupersync `&Cx` and `Outcome` |
| **CLI first, daemon optional** | Every essential workflow runs as a one-shot. No background process required |
| **Auditable curation** | Promotions, consolidations, and tombstones produce audit entries; no silent rewrites |
| **Crowded-agent posture** | Swarm brief, workspace hygiene, verification broker, QoS lanes, and flight recorder help agents coordinate without taking over the loop |

### Agent Operating Loop

For agent use, the core rhythm is small and repetitive:

```bash
ee resume --workspace . --json
ee orient "<task>" --workspace . --include-primer --fast --json
ee swarm brief --workspace . --json
ee pack "<task>" --workspace . --read-only --max-tokens 4000 --format markdown
ee recall --path <path> --workspace . --budget-tokens 400 --format markdown
ee search "<specific question>" --workspace . --limit 20 --explain --json
ee ask "<direct question>" --workspace . --json
ee why <memory-id> --workspace . --json
ee preflight check --cmd "<risky shell command>" --workspace . --json  # advisory memory lookup; never blocks
ee journal append "<observation>" --workspace . --source manual --json
ee journal distill --workspace . --dry-run --json
ee remember "<durable lesson>" --workspace . --level procedural --kind rule --json
ee remember "<blocked lesson>" --workspace . --revive-when path_exists:path/to/marker --json
ee tripwire check --revivals --workspace . --json
ee remember --batch --stdin --workspace . --json
ee remember "<existing lesson>" --workspace . --reinforce --json
ee outcome <memory-id> --workspace . --signal helpful --reason "<what it changed>"
ee outcome --pack <pack-id> --item <n> --workspace . --signal helpful --reason "<why>"
ee outcome trace <memory-id> --workspace . --json
```

| Situation | First `ee` command |
|---|---|
| Resuming work — "where was I?" | `ee resume --workspace . --json` |
| Starting from a cold agent session | `ee resume --workspace . --json` (read `data.report`, then follow `nextCommands`) |
| You want the standing workspace charter | `ee primer --workspace . --format markdown` |
| AGENTS.md might be lying about the rules | `ee diag agentsmd-drift --workspace . --json` |
| Starting substantive work | `ee pack "<task>" --workspace . --read-only --max-tokens 4000 --format markdown` |
| About to edit known files or a diff | `ee recall --path <path> --workspace . --budget-tokens 400 --format markdown` |
| Joining a crowded checkout | `ee swarm brief --workspace . --json` |
| Capturing raw session observations | `ee journal append "<text>" --workspace . --source manual --json` |
| Ending a session with reviewable candidates | `ee journal distill --workspace . --dry-run --json` |
| Learning a durable rule | `ee remember "<text>" --workspace . --level procedural --kind rule --json` |
| Recording a retired memory that becomes relevant when a condition clears | `ee remember "<text>" --workspace . --revive-when path_exists:path/to/marker --json` |
| Listing revival conditions that pass now | `ee tripwire check --revivals --workspace . --json` (read-only; no trust or tombstone mutation) |
| Importing several curated facts | `ee remember --batch --stdin --workspace . --json` |
| Corroborating an existing lesson | `ee remember "<text>" --workspace . --reinforce --json` |
| A memory helped or misled you | `ee outcome <id> --signal helpful\|harmful --reason "<one sentence>"` |
| A specific pack item helped | `ee outcome --pack <pack-id> --item <n> --signal helpful --reason "<one sentence>"` |
| You need feedback provenance | `ee outcome trace <memory-id> --workspace . --json` |
| Need a direct cited answer | `ee ask "<question>" --workspace . --json` |
| A high-ranked memory looks suspicious | `ee why <id> --workspace . --json` |
| A context pack looks odd | `ee pack "<task>" --workspace . --explain --json` |
| Want risk history before a destructive command | `ee preflight check --cmd "<exact command>" --workspace . --json` (advisory only) |
| You need a safe handoff | `ee handoff create --workspace . --out <capsule.json> --json` |
| You need a support artifact | `ee support bundle --out <dir> --workspace . --json` |

---

## Quick Example

A typical session:

```bash
# 1. Initialize a workspace
$ ee init --workspace .
✓ database opened at ~/.local/share/ee/ee.db
✓ workspace registered: eidetic_engine_cli (a7f2c19e)
✓ index dir ready: ~/.local/share/ee/indexes/combined
✓ semantic backend: ready (neural_local, potion-multilingual-128M)

# 2. Capture a durable rule you just learned
$ ee remember --workspace . --level procedural --kind rule \
    --tags rust,ci \
    "This project treats clippy warnings as errors with pedantic and nursery enabled."
✓ memory mem_01HQ3K5Z stored (procedural · rule · confidence 0.80)
✓ indexed in 14ms

# 3. Pull session evidence from your cass history and inspect the v2 payload
$ ee import cass --workspace . --limit 50 --json | jq '.data | {schema, status, sessionsDiscovered, sessionsImported, sessionsSkipped, spansImported, indexJobsQueued, indexRequiredAction}'
{
  "schema": "ee.import.cass.v1",
  "status": "completed",
  "sessionsDiscovered": 50,
  "sessionsImported": 47,
  "sessionsSkipped": 3,
  "spansImported": 312,
  "indexJobsQueued": 47,
  "indexRequiredAction": "ee index rebuild --workspace /path/to/project --database /path/to/project/.ee/ee.db"
}

# 4. Apply the reported indexing action (default workspace form shown)
$ ee index rebuild --workspace .

# 5. Search the indexed CASS excerpts directly
$ ee search "release workflow failure" --workspace . --limit 20 --explain --json

# 6. Pack durable memories for the task; the manual rule from step 2 is eligible
$ ee pack "enforce clippy warnings as errors in CI" --workspace . --profile thorough

# 7. Inspect that manually remembered rule
$ ee why mem_01HQ3K5Z --workspace . --json

# 8. Record that the rule helped
$ ee outcome mem_01HQ3K5Z --signal helpful --reason "Caught a clippy regression"
✓ utility +0.08 → confidence 0.63
```

The manual rule and imported CASS evidence are separate records in this
example. Step 6 can select the rule because step 2 created a durable memory;
the import does not retroactively give that rule CASS provenance. Fresh
imported excerpts are searchable after step 4, but an excerpt requires a
linked, distilled memory before it can hydrate into a memory-centric pack. A
matching unlinked excerpt is reported as
`context_evidence_hit_unhydrated` instead of being represented as a pack item.

The flow runs locally with no daemon and no cloud. On a typical project, the
interactive steps are fast enough to use before ordinary agent work.

---

## Design Philosophy

> `ee` is the durable memory layer your agent harness calls. The harness still
> owns tools, approvals, and the prompt loop.

The code and tests back these contracts where they can.

### 1. Local First

All primary data lives on your machine. No cloud dependency is required. Remote APIs stay explicit opt-in; the default local embedding model is a pinned, verified Frankensearch download cached under ee's data directory, with deterministic hash fallback for offline runs.

### 2. Harness Agnostic

`ee` is callable from any shell: Claude Code hooks, Codex shell-outs, custom
scripts, plain humans, and MCP adapters. Agents push evidence in and pull
context out.

### 3. CLI First, Daemon Later

Core workflows run as one-shot CLI commands. The daemon (`ee daemon`) is opt-in
for supervised foreground maintenance and write-owner work; bounded `job` and
`maintenance` commands handle explicit steward work from the shell.

### 4. Deterministic By Default

Given the same database, indexes, config, profile, budget, seed, and query, the JSON output is byte-stable, ranking ties resolve deterministically, and context pack hashes reproduce exactly. Golden tests assert this.

Mechanized proof artifacts now live alongside the test suite: [`proofs/lean4/pack_determinism.lean`](proofs/lean4/pack_determinism.lean) models the pack-hash determinism invariant, and [`proofs/tla/agent_mail_coordination.tla`](proofs/tla/agent_mail_coordination.tla) models exclusive Agent Mail reservation safety. The proof-check report schema is registered as `ee.proof_check.v1` and is checkable via `ee verify proofs`.

### 5. Explainable Retrieval

Every returned memory answers six questions:

- **Why selected?** Score components per stage.
- **What supports it?** Provenance URI(s).
- **How fresh?** Recency decay term.
- **How reliable?** Confidence, evidence count, harmful-feedback weight.
- **What scores mattered?** Raw score, `scoreKind`, normalized `relevanceScore`, and component breakdown.
- **What would change the decision?** Counterfactual hint when available.

### 6. Search Indexes Are Derived Assets

FrankenSQLite + SQLModel hold the source of truth. Frankensearch indexes,
embeddings, graph snapshots, and caches are rebuildable from scratch. If the
index directory is lost, run `ee index rebuild`.

### 7. Graceful Degradation

| If this is missing | These still work |
|---|---|
| Semantic model | Lexical BM25 + FTS5 fallback |
| Graph snapshot | Retrieval without graph boosts |
| `cass` binary | Explicit `ee remember` records |
| Network | Everything (we are local-first) |

Each degradation surfaces in the JSON `degraded` array with a repair command.

### 8. Evidence Before Promotion

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
| Graph analytics (PPR, HITS, PageRank, proximity, causal paths) | ✅ | ❌ | ❌ | ❌ |
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

- Binary is named `ee`. Single CLI binary.
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

**Recommended — release installer:**

```bash
curl -fsSL "https://raw.githubusercontent.com/Dicklesworthstone/eidetic_engine_cli/main/install.sh?$(date +%s)" | bash -s -- --easy-mode --verify
```

This downloads the latest release binary for your platform, always verifies its
SHA-256 checksum, verifies its Sigstore bundle when one is published and
`cosign` is available, drops `ee` into `~/.local/bin`, installs shell
completions, repairs writable zsh/bash startup files (creating the active
shell's file for a fresh home), and runs `ee --version` plus `ee doctor --json`.
The informational agent scan prints setup guidance without changing agent settings. Open a new shell (or source its rc file) afterward.
Re-running the command repairs `PATH` and completions and re-verifies a matching version without downloading or rebuilding it.
Pass `--require-provenance` to require both a verified release signature and a
verified SLSA provenance attestation; otherwise a missing bundle is reported
and the checksum-verified install continues.

[Release binaries](https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/latest)
cover macOS (`aarch64`, `x86_64`), Linux (`aarch64` and `x86_64` GNU,
with musl where published), and Windows (`x86_64`). On x86_64 Linux the
installer prefers the portable musl build, then automatically retries the
compatible GNU build when that release does not include musl.

**Windows (PowerShell):**

```powershell
$f = Join-Path $env:TEMP 'install-ee.ps1'; $u = "https://raw.githubusercontent.com/Dicklesworthstone/eidetic_engine_cli/main/install.ps1?cache=$([guid]::NewGuid())"; Invoke-WebRequest -UseBasicParsing $u -OutFile $f; & $f -Verify
```

Downloads the current installer to a temporary file before running it. This
keeps the script inspectable and avoids fragile `Invoke-Expression` and
content-type behavior. The script SHA-256-verifies and installs `ee.exe`
into `%LOCALAPPDATA%\ee\bin`, updates your user `PATH`, installs PowerShell completions, and runs the same version/doctor self-test. Add
`-RequireProvenance`, or set `EE_REQUIRE_PROVENANCE=1`, to also
enforce Sigstore signature verification.
The Windows installer conformance contract is tracked in
[`tests/CONFORMANCE.md`](tests/CONFORMANCE.md#windows-installer-conformance-installps1).

**From source** (nightly Rust toolchain):

```bash
mkdir ee-source
cd ee-source
git clone https://github.com/Dicklesworthstone/eidetic_engine_cli
cd eidetic_engine_cli
./scripts/checkout-franken-stack.sh ..
cargo build --release
./target/release/ee --version
```

`ee` uses sibling path dependencies during early development. The checkout
helper reads [`franken-stack.lock`](franken-stack.lock), fetches the exact
compatible revisions next to the `eidetic_engine_cli` checkout, verifies every
result, and refuses to modify an unrelated or dirty existing repository.
`install.sh --from-source` and `install.ps1 -FromSource` run the same locked
setup automatically.

### Verify

```bash
ee --version
ee doctor --json
ee capabilities --json
```

`ee doctor` reports database health, schema version, index posture, embedding
model posture, `cass` binary detection, workspace identity, mesh posture,
capabilities, and repair actions. Useful focused modes:

```bash
ee doctor --quick --json
ee doctor --robot-triage --json
ee doctor --capabilities --json
ee doctor --gc-plan 30 --json
```

---

## Quick Start

Start read-only: `ee resume` reports the recent session end-state, open
decisions, queued work, provenance/redaction posture, and safe next commands.
If the addressed workspace has no store, it does not initialize one; it can
instead point at a nearby populated store with an executable retarget command.

```bash
# 0. Resume an existing campaign without mutating or initializing the store
ee resume --workspace . --json

# 1. Open a workspace when this is genuinely a new campaign (idempotent)
ee init --workspace .

# 2. Optionally import cass history, then build the derived evidence index
ee import cass --workspace . --limit 50 --json
ee index rebuild --workspace .

# 3. Get context from durable memories for a task
ee pack "what should I know before refactoring the storage layer?" \
  --workspace . --profile thorough --max-tokens 4000 --format markdown

# 4. When you learn something durable, capture it
ee remember --workspace . --level procedural --kind rule \
  --tags rust,testing \
  "Integration tests must hit a real Postgres instance, never a mock. See incident 2025-Q3."

# 5. Preview CASS-backed candidates, then persist and apply a reviewed candidate
ee review session <cass-session-id> --workspace . --propose --dry-run --json
ee review session <cass-session-id> --workspace . --propose --json
ee curate candidates --workspace . --json
ee curate validate <candidate-id> --workspace . --json
ee curate apply <candidate-id> --workspace . --json
ee index rebuild --workspace .

# 6. Search at any time
ee search "release failure clippy" --workspace . --limit 20 --explain --json
```

That is the core loop.

---

## Development & Verification

To run the full verification suite before committing or pushing:

```bash
./scripts/verify.sh
```

This runs all readiness gates in order, stopping at the first failure:

| Stage | Gate |
|---:|---|
| 1 | Forbidden dependency audit |
| 2 | Closure linter |
| 3 | Snapshot proposal guard |
| 4 | Untracked work audit |
| 4.5 | Bridge staleness advisory |
| 4.6 | Plan drift advisory |
| 4.7 | Fuzz target audit |
| 5 | Vision coverage |
| 5.5 | Proof verification |
| 6 | Unit, contract, golden, binary, test, and example targets |
| 6 | Basic E2E |
| 6.06 | Replay lab smoke E2E |
| 6.5 | Overhaul integration when `VERIFY_OVERHAUL` is enabled |
| 6.6 | Fake Tailscale harness |
| 6.7 | Fake OIDC happy-path, defect, and deterministic capability/privacy/time matrix harnesses |
| 7 | Advanced E2E |
| 8 | Boundary migration |
| 8.5 | `ee doctor` safety harness |
| 9 | Benchmarks when `--include-bench` is passed |

The runner reports exit code, elapsed time, and artifact directories. In agent
sessions, route heavy Cargo stages through RCH rather than using local fallback.

---

## Command Reference

`ee` has core commands and command groups. Run `ee <command> --help` or
`ee <group> --help` for full details.

Current top-level groups:

| Group | Commands |
|---|---|
| Core memory loop | `init`, `remember`, `decide`, `search`, `ask`, `pack`, `why`, `status`, `doctor`, `capabilities`, `check`, `health` |
| Memory lifecycle | `memory`, `rule`, `journal`, `curate`, `review`, `playbook`, `procedure`, `workflow`, `outcome`, `outcome-quarantine` |
| Packing and retrieval | `recall`, `timeline`, `similar`, `lens`, `context-show`, `show`, `link`, `tag`, `history`, `proximity`, `insights`, `subscribe` |
| Graph and structure | `graph`, `causal`, `economy`, `focus`, `learn`, `lab`, `rehearse`, `rationale`, `situation`, `task-frame` |
| Storage and derived assets | `db`, `migrate`, `index`, `model`, `schema`, `backup`, `export`, `artifact`, `config`, `workspace` |
| Diagnostics and release gates | `diag`, `eval`, `perf`, `preflight`, `tripwire`, `verify`, `verification`, `audit`, `claim`, `certificate`, `demo` |
| Agent integration | `agent`, `agent-docs`, `hook`, `mcp`, `support`, `swarm`, `handoff`, `recorder`, `completion` |
| Optional adapters and operations | `daemon`, `job`, `maintenance`, `mesh`, `share`, `serve`, `install`, `update`, `version`, `introspect`, `plan` |

### Core workflow

| Command | Purpose |
|---|---|
| `ee help [command path]` | Show top-level help or help for nested commands such as `ee help memory show` |
| `ee init [--workspace .]` | Create or open a workspace, run migrations, prepare indexes |
| `ee status [--json]` | DB generation, index generation, degraded capabilities, recent jobs |
| `ee doctor [--json]` | Health checks with repair commands for every failure |
| `ee capabilities [--json]` | Feature, schema, renderer, env-var, and capability posture |
| `ee resume [--sessions N] [--json]` | The read-only "where was I" bundle: last N episodic sessions newest-first (`N` is 1–64; stable `session-*` identity survives interleaved/backfilled rows), public-redacted items and decisions with provenance/redaction posture, bounded revisit/queue lanes with exact totals and `truncated` flags, staleness from a strictly newer same-kind memory sharing a non-control subject tag (`session-*` and open-loop tags never establish identity; `staleCount` deduplicates memory IDs across projections), and nearby populated stores when the addressed store is empty (`ee.resume.v1`) |
| `ee orient "<task>" --fast --json` | Fast read-only session-start bundle: bounded swarm brief, install/path posture, workspace hygiene, and explicit follow-up commands for full doctor/pack surfaces |
| `ee primer [--tokens N] [--refresh] [--json]` | Deterministic, cached workspace charter (~600 tokens): top rules, unresolved warnings, key decisions, load-bearing memories, every line provenance-backed (`ee orient --include-primer` folds it into orientation) |
| `ee export agentsmd [--file AGENTS.md] [--create] [--dry-run]` | Render the primer rules+warnings into a marker-delimited managed block; never edits outside its markers, backs up before mutating, refuses hand-edited blocks without `--force-managed-block` |
| `ee import agentsmd [--apply] [--json]` | Parse rule-like statements outside the ee markers into curation candidates (trust capped at agent_assertion, `file://<path>#L<n>` provenance); dry-run by default |
| `ee diag agentsmd-drift [--json]` | Read-only audit of AGENTS.md vs memory: stale export, file-vs-memory contradictions, missing rules, suggested commands |
| `ee pack "<task>" [--profile <p>] [--max-tokens N] [--format <fmt>]` | Assemble a task-specific context pack (the canonical headline command; `ee context "<task>"` remains a soft-deprecated compatibility alias) |
| `ee lens list --json` / `ee lens explain <id> --json` | Inspect named task lenses such as `bugfix`, `code-review`, and `release-readiness` before applying them |
| `ee search "<query>" [--limit N] [--explain] [--json]` | Hybrid retrieval over memories, sessions, rules, evidence |
| `ee search --family <family-id> [--memory-scope <scope>] [--strict-scope] [--json]` | Queryless, workspace-scoped retrieval of every recorded attempt-family member, including rejected attempts |
| `ee search "<query>" --all-workspaces [--json]` | Inspection-only fan-out over registered workspaces plus the user-global lane (bounded, per-row `workspaceId` and lane labels); never mutates any store |
| `ee similar <memory-id> [--limit N] [--min-score T] [--explain] [--json]` | Find embedding-native nearest-neighbor memories for a seed memory; degrades to lexical similarity with an explicit degraded note when semantic vectors are unavailable |
| `ee ask "<question>" [--require-confidence T] [--json]` | Direct extractive answer from stored memories, with citations, conflict sides, calibrated abstention, and exit 6 fail-closed mode |
| `ee recall --path <glob>` / `--symbol <name>` / `--diff <ref>` | Fetch memories anchored to a code surface before editing; returns `ee.recall.v1` under the standard response envelope |
| `ee timeline "<topic>" --as-of <RFC3339> --json` | Reconstruct read-only memory state for a topic at a historical timestamp; returns `ee.timeline.v1` under the standard response envelope |
| `ee remember "<text>" --level <l> [--kind <k>] [--tags a,b] [--sentinel <kind>:<target>] [--revive-when <kind>:<target>]` | Capture a durable memory with optional Gate and Revive sentinel predicates; both forms are fully validated before any memory, idempotency, or dry-run write |
| `ee journal append "<text>" [--source hook\|manual] --json` | Append a working-tier observation that can later be distilled; JSONL batches use `ee journal append --stdin --json` |
| `ee journal distill [--dry-run\|--apply] --json` | Turn repeated or surprising journal entries into reviewable curation candidates; dry-run is the safe default |
| `ee journal list` / `ee journal show <entry-id>` | Inspect append-only journal entries, truncation/redaction state, and distillation bookkeeping |
| `ee remember --batch --stdin --json` | Record a JSONL batch of curated memories with independent per-line results and quarantine reporting |
| `ee remember "<text>" --reinforce --json` | Corroborate an existing near-duplicate memory through bounded reinforcement instead of creating a redundant row |
| `ee decide record "<topic>" --chosen <x> --alternative <y> --rationale "<why>" [--revisit-by <RFC3339\|+ND>]` | Record a decision-kind memory with typed fields and fork protection |
| `ee decide list [--about <text>] [--include-superseded] --json` | Review current decision heads or full supersede history before proposing architecture changes |
| `ee decide revisit [--warning-days N] --json` | Find decisions whose revisit horizon is due or near due |
| `ee outcome <id> --signal helpful\|harmful [--reason "<reason>"]` | Record feedback, updating utility/confidence |
| `ee outcome --batch --stdin --json` | Record a JSONL batch of outcome events with independent quarantine and rate-limit handling per line |
| `ee outcome --pack <pack-id> --item <n> --signal helpful\|harmful --json` | Grade a specific persisted pack item without manually copying its memory id |
| `ee outcome trace <memory-id> --json` | Read the feedback events, posterior updates, and trust transitions that affected a memory |
| `ee audit timeline --target <id> --json` | Inspect the audit rows for a memory, pack, candidate, or other target id in one bounded call |
| `ee why <memory-id> [--json]` | Explain why a memory was selected, scored, or curated the way it was |
| `ee why-not <memory-id> --task "<task>" [--json]` | Counterfactual reverse of `ee why`: explain why a memory was not selected for a task's context pack, with the minimal change that would include it (read-only) |
| `ee pack build --query-file task.eeq.json --max-tokens N --format toon` | Build a pack from an explicit EQL query document |
| `ee pack replay <pack-id> --json` | Inspect the persisted, redaction-safe selection ledger for a historical pack |
| `ee pack diff <old-pack-id> <new-pack-id> --json` | Compare two persisted pack ledgers and explain selection, freshness, redaction, or derived-asset changes |
| `ee support bundle --out <dir> --json` | Create a redacted diagnostic bundle, including pack replay and swarm-brief summaries without raw query, mail body, memory, or full file-listing content |
| `ee preflight check --cmd "<shell command>" --json` | Retrieve advisory risk, failure, and anti-pattern memory for a command; never block execution |
| `ee verify proofs --json` | Check committed Lean4 and TLA+ proof artifacts |

### Agent integration

| Command | Purpose |
|---|---|
| `ee hook claude-code --print\|--install\|--undo --json` | Preview, install, or undo managed Claude Code recall hooks; report schema `ee.hook.harness_install.v1` |
| `ee hook codex --print\|--install\|--undo --json` | Preview, install, or undo managed Codex recall hooks; unsupported targets report capability gaps instead of mutating settings |

### JSON output and exit codes

Machine readers should inspect the JSON contract before trusting a result:

```jsonc
{
  "schema": "ee.response.v2",
  "success": true,
  "data": {},
  "degraded": []
}
{
  "schema": "ee.error.v2",
  "error": {
    "code": "migration_required",
    "message": "Database schema migration is required.",
    "severity": "high",
    "repair": "ee migrate run --workspace .",
    "details": {
      "recovery": [
        {
          "priority": 0,
          "kind": "migration",
          "rationale": "Apply pending local schema migrations.",
          "command": "ee migrate run --workspace ."
        }
      ]
    }
  }
}
```

| Check | What to read |
|---|---|
| Envelope | `schema` and `success` |
| Exit status | `0` clean, `6` degraded-required, `7` policy denied, `8` migration required |
| Degradations | `degraded[]` for issues that affected this response |
| Recoveries | `error.details.recovery[]`, which is structured for agents |
| Posture | `ee status --json` uses `data.posture.overall`; `ee doctor --json` returns a doctor-specific posture view |
| Provenance | `provenance[]`, `evidence_spans[]`, and `trustClass` on memory and pack items |
| Typed fields | `data.memory.typedFields` on memory show, `typedFields`/`metadata.typed_field.*` on search-derived memory results when present |
| Pack identity | `data.pack.hash` for batch packs; `packHash` on stream trailer frames |
| Graph explanation | `data.pack.packDna` when `ee pack --explain --json` is used |
| Feature gaps | `ee capabilities --json` at `data.unimplemented[]`, not command `degraded[]` |
| Output budget | `meta.tokensEstimated` is stamped whenever `--max-output-tokens` / `EE_MAX_OUTPUT_TOKENS` governs the response; never above the ceiling unless the response failed closed with `output_budget_unsatisfiable` |
| Truncation + resume | `output_truncated_budget` in `degraded[]` carries `details.droppedCount` and `details.continuationCursor`; resume with `--cursor <token>` — a rejected cursor (`cursor_invalid` / `cursor_stale`) is an EMPTY page, never a restart. See [`docs/agent-ux/output-budgets.md`](docs/agent-ux/output-budgets.md) |
| Streams | `ee.pack.stream.v1` NDJSON frames: `header`, `item`, terminal `trailer`, `error`, or `cancelled` |

Severity vocabulary:

| Order | Values |
|---|---|
| Low to high | `info` < `low` < `warning` < `medium` < `high` < `critical` |

Status and doctor posture are separate contracts:

| Command | Field | Values |
|---|---|---|
| `ee status --json` | `data.posture.overall` | `ok`, `degraded_recoverable`, `degraded_required`, `blocked`, `unimplemented`, `initializing` |
| `ee doctor --json` | `data.posture` | `ready`, `degraded`, `needs_attention` |

Exit code vocabulary:

| Code | Meaning |
|---:|---|
| `0` | success |
| `1` | usage error |
| `2` | configuration error |
| `3` | storage error |
| `4` | search/index error |
| `5` | import error |
| `6` | degraded-required |
| `7` | policy denied |
| `8` | migration required |
| `10` | workspace store missing (the addressed workspace has no initialized store) |

Common red flags:

| Signal | First response |
|---|---|
| `data.posture.overall = "blocked"` | Run `ee doctor --json` and follow the failing check repair |
| `data.posture.overall = "degraded_required"` | Read `degraded[]` and `error.details.recovery[]` |
| `search_index_stale` | `ee index rebuild --workspace .` |
| `embed_model_unavailable` | Continue lexical fallback, inspect bundled-model cache/download posture, or run `ee index reembed --workspace .` |
| `graph_snapshot_stale` | Continue retrieval, then refresh graph snapshots when graph scores matter |
| `pack_budget_too_small` | Raise `--max-tokens` or switch to `--profile compact` |
| `output_budget_unsatisfiable` | Raise `--max-output-tokens` or narrow the `--fields` preset; the page failed closed rather than lie |
| `cursor_stale` | A write advanced the DB generation mid-pagination; re-run without `--cursor` for a fresh sequence |
| `data.workspace.diagnostics[].severity = "warning"` | Workspace selection conflict; use `ee workspace list`, then pass an explicit workspace or alias |
| exit `7` | An `ee` operation refused its own requested mutation; inspect that command's error details. Advisory `ee preflight check` never emits this status and never has an approval or allowlist path. |
| exit `8` | Run `ee migrate run --workspace . --json` |

### Context pack controls

`ee pack` and `ee pack build` expose three layers of control:

| Layer | Flags | Use |
|---|---|---|
| Retrieval profile | `--profile compact\|balanced\|grounding\|orientation\|thorough\|submodular` | Choose the memory mix and graph bias |
| Task lens | `--lens <id>`, `--no-lens`; inspect with `ee lens list --json` and `ee lens explain <id> --json` | Apply a named, hash-stable policy overlay for common tasks such as bugfix, code-review, release-readiness, dependency-update, schema-contract, performance-investigation, or coordination-handoff |
| Output profile | `--pack-profile lean\|standard\|verbose` | Trim or expand JSON metadata |
| Resource profile | `--resource-profile lean\|standard\|swarm_heavy` | Pick pack assembly SLO posture |
| Retrieval source | `--source-mode lexical_only\|semantic_only\|hybrid`, `--strict-source-mode` | Force lexical-only, semantic-only, or hybrid retrieval before packing; strict mode fails instead of falling back |
| Size | `--max-tokens N`, `--candidate-pool N` | Bound prompt budget and candidate pool |
| Output format | `--format markdown\|json\|toon`, `--stream --json` | Token-tight prompt text (markdown), parser output (json), stable-field structured output (toon — not smaller than json for packs), or NDJSON frames |
| JSON diet | `--no-rendered-text`, `--no-skipped`, `--no-meta`, `--no-pack-dna` | Suppress bulky sections for structured consumers |
| Persistence | `--read-only`, `--no-persist` | Assemble prompt context without writing pack records, audit rows, or L2 cache entries |
| Coordination | `--coordination-snapshot <path>`, `--coordination-stale-after-ms N` | Embed a redacted coordination snapshot |
| Code-change hints | `--changed-symbol <selector>`, `--changed-symbols-from-git` | Bias toward memories linked to changed symbols |
| Time windows | `--as-of <RFC3339>`, `--include-expired`, `--include-future`, `--include-stale`, `--include-tombstoned` | Inspect validity-window behavior |
| Trust lane | `--memory-scope self\|team\|global\|workspace\|verified\|swarm`, `--strict-scope` | Bound which trust lane can contribute. On `ee pack`/`pack build` an explicit value overrides any task-lens scope overlay; omitted keeps lens-then-`swarm` behavior. `self`/`swarm` are agent scopes; `team` covers explicit local-origin ownership plus receiver-derived member projections (no `trust.team_members` nickname compatibility) |
| Privacy | `--redaction none\|minimal\|standard\|strict\|paranoid` | Tune output redaction where the command allows it |

Examples:

```bash
ee pack "debug release failure" \
  --workspace . \
  --lens bugfix \
  --profile thorough \
  --pack-profile verbose \
  --resource-profile swarm_heavy \
  --max-tokens 8000 \
  --explain \
  --json

ee pack "small hook context" \
  --workspace . \
  --profile compact \
  --pack-profile lean \
  --max-tokens 1200 \
  --format toon

ee pack "large agent handoff" \
  --workspace . \
  --stream \
  --format jsonl
```

Task-lens runs persist the lens id, version, and stable lens hash in the pack
replay ledger. Use `ee pack replay <pack-id> --json` to audit which lens shaped
a historical pack, and rerun with `--no-lens` when you need an un-lensed
comparison.

When `[pack] adaptive_budget = true`, omitted `--max-tokens` lets `ee` compute
a budget from retrieval entropy, graph fanout, and task keywords. Passing
`--max-tokens N` pins the budget for prompt caches, eval fixtures, CI gates, or
multi-pack composition.

When `[pack] memory_tier_admission = true`, `ee pack` treats hot/warm/cold
memory tiers as advisory candidate signals. Hot and warm candidates can receive
small deterministic ranking boosts, but cold items are not filtered; explicit
query matches and safety/failure evidence remain eligible for the pack.

### Graph-derived insights

Graph views show relationships between memories for navigation, packing,
curation, and triage; they do not replace provenance from the memory records
themselves.

| Command | Purpose |
|---|---|
| `ee insights --json` | Bundle graph-derived findings such as top memories, bridges, contradiction clusters, proximity hotspots, load-bearing memories, HITS hubs/authorities, and skyline posture |
| `ee insights --section <name> --json` | Return one deterministic section when a full bundle is too broad |
| `ee pack "<task>" --explain --json` | Include a Pack DNA block that explains pack composition with dominators, communities, ego subgraphs, and PPR neighbors when available |
| `ee why <memory-id> --causal-explain --json` | Add a causalExplanation block with causal ancestry and min-cost path evidence |
| `ee insights --section causalBottlenecks --json` | Inspect causal bottleneck findings across failure-oriented causal evidence |
| `ee health --robot-insights --json` | Surface structural health through k-truss and contradiction-cluster summaries |
| `ee health scorecard --json` | Summarize memory-store health across coverage, freshness, trust, redundancy, and graph structure with trend and top actions |
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
ee pack "prepare release" --workspace . --explain --json \
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
the stored ledger only after its hash, shape, invariants, and containing-record
bindings pass, then emits a current-policy-redacted public projection. Missing
or untrusted ledgers produce empty replay selections rather than falling back
to denormalized item rows. Replay does not claim that a fresh search would make
the same choices today. Use a new `ee pack` run when you want
live re-retrieval against current memories, indexes, graph snapshots, and trust
state.

Use `ee pack diff <old-pack-id> <new-pack-id> --json` when a later pack changed
and you need to separate selection, freshness, redaction, trust, or derived-asset
causes. Freshness states and degradation codes identify evidence that was
changed, missing, stale, or unavailable at replay time; treat those as repair or
revalidation signals instead of silently dropping the memory from the story.

For bug reports and handoffs, attach
`ee support bundle --out <dir> --json`. The bundle includes
`pack_replay_summary.json`, which keeps pack IDs, pack hashes, ledger hashes,
freshness counts, degradation codes, redaction classes, and derived-asset
metadata, plus a compact attestation status and bundle hash for every summarized
pack. It hashes query and actor text, nulls record metadata that was not
integrity-verified, reports truncation explicitly, and omits raw memory content,
`why` text, provenance text, and full ledger payloads.

Bundles also include `swarm_brief_summary.json`, a compact coordination posture
snapshot for support and handoff triage. It keeps source statuses, ready/blocked
work counts, active-conflict counts, resource-pressure posture, degraded codes,
top recommendation IDs, and hashes/provenance for the underlying brief. It
omits raw Agent Mail bodies, raw query text, raw provenance text, and full file
listings. Treat it as diagnostic context. Before claiming work or coordinating
edits, run a fresh `ee swarm brief`.

Bundles and handoff capsules also carry
`environment_attestation_summary.json`, a redaction-safe source-authority
projection. It separates RCH proof admission from source-test verdicts, keeps
verdicts, degraded codes, recovery-action posture, first-failure diagnosis, and
hashed evidence references, and omits raw mail bodies, source snippets, command
argv, and host-private absolute paths. Treat embedded attestation summaries as
handoff context only; rerun
`ee diag environment-attestation --workspace . --include-rch --json` before
claiming, closing, or relying on proof posture.

### Swarm brief workflow

`ee swarm brief` is the read-only coordination preflight for crowded repos. Run
it before claiming a bead, after large dirty-state or reservation changes, and
before using handoff or support-bundle evidence as the basis for new work.

Start with a summary view when a routine agent preflight needs compact,
budget-friendly output. The `--fields` flag may appear before the command or
after `swarm brief`:

```bash
ee swarm brief --fields summary --workspace . --json
```

If either summary form (`ee swarm brief --fields summary ...` or
`ee --fields summary swarm brief ...`) returns an `ee.error.v2` usage failure
such as `usage_unknown_field`, and `error.details.presetsAvailable` still lists
`summary`, treat the installed binary as stale relative to the current
source/docs contract. For read-only inspection, fall back to
`ee swarm brief --workspace . --json`. That fallback does not authorize Beads
mutation: claim work only after the work-packet claim gate succeeds, and
coordinate for an approved RCH/release-path rebuild if compact field projection
is required.

Use the complete output when a harness needs every source array, including
file-surface risks and resource-pressure hints. This output is intentionally
larger; keep it behind an explicit `--fields full` in agent loops:

```bash
ee swarm brief --fields full --workspace . --include-rch --json
```

Require selected live coordination sources when degraded output is unacceptable:

```bash
ee swarm brief --workspace . --sources git,beads,bv,agent-mail --require-sources --json
```

If live Agent Mail is unavailable, provide a redacted snapshot instead of raw
mail bodies. When no snapshot is configured, `ee swarm brief` also does a tiny
bounded probe of `127.0.0.1:8765/health`; a reachable probe means Agent Mail
looks live, but `ee` still needs an explicit redacted snapshot for deterministic
briefs.

```bash
SNAPSHOT_PATH=/private/tmp/ee-agent-mail-snapshot.json
scripts/agent_mail_snapshot.sh \
  --project "$PWD" \
  --agent "$AGENT_NAME" \
  --output "$SNAPSHOT_PATH"

ee swarm brief --workspace . --agent-mail-snapshot "$SNAPSHOT_PATH" --json
```

Use a canonical, non-symlink snapshot path. On macOS, `/tmp` is normally a
symlink to `/private/tmp`, and `ee swarm brief --agent-mail-snapshot /tmp/...`
refuses the file before reading it. `scripts/swarm_coordination_health.sh`
emits health evidence only; it is not a full reservation, roster, inbox, or
thread snapshot.

When the claim gate stops on missing Agent Mail evidence, bridge it with a
snapshot before mutating Beads:

```bash
CANDIDATE=bd-example.1
ee swarm work-packet --workspace . --include-rch \
  --claim-gate --candidate "$CANDIDATE" --json \
  | jq '.data | {schema, verdict, safeToClaim, agentMailStatus: .sourceAuthority.agentMailStatus, unsafeReasons, degradedCodes}'

scripts/agent_mail_snapshot.sh \
  --project "$PWD" \
  --agent "$AGENT_NAME" \
  --output "$SNAPSHOT_PATH"

ee swarm work-packet --workspace . --include-rch \
  --agent-mail-snapshot "$SNAPSHOT_PATH" \
  --claim-gate --candidate "$CANDIDATE" --json \
  | jq '.data | {schema, verdict, safeToClaim, agentMailStatus: .sourceAuthority.agentMailStatus, unsafeReasons, degradedCodes}'
```

The first response is `ee.swarm.work_packet.claim_gate.v1`; if it reports
`agent_mail_unavailable`, `safeToClaim=false`, or
`sourceAuthority.agentMailStatus` as `unavailable`, `skipped`, or
`degraded_read_only`, do not claim. The retry is still read-only: a fresh
snapshot may change `agentMailStatus` to `fresh` and make reservation and inbox
evidence authoritative, but it does not authorize Beads mutation by itself.
Keep coordinating through Agent Mail when `unsafeReasons` still name an active
reservation, tracker stale state, a BV/Beads disagreement, or an RCH blocker.

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
3. Choose a candidate from the fail-closed Beads queue first:
   ```bash
   scripts/br_retry.sh actionable --json
   ```
   The wrapper reports open, unassigned, non-epic rows after retrying
   transient Beads JSONL read races. Treat `[]` as no safe claimable leaf.
   The full broad inspection command
   `br ready --limit 0 --json --no-auto-import --no-auto-flush --allow-stale`
   is still useful, and
   `bv --robot-triage` is still useful for ranking, but BV claim commands are
   advisory until the candidate also appears in the actionable queue and
   passes the read-only claim gate.
4. Run the read-only claim gate before any mutation:
   ```bash
   ee swarm work-packet --workspace . --include-rch --claim-gate --candidate <id> --json
   ```
   If the installed `ee` rejects `--claim-gate` or `--candidate` as an
   unexpected argument, treat that binary as stale relative to the current
   source/docs contract. Stop at inspection, coordinate for an approved
   RCH/release-path rebuild, run no BV claim command, and do not use local
   Cargo install as a workaround.
   When the gate, support-bundle summary, or handoff evidence disagrees about
   source authority, run the read-only environment attestation for the
   per-source explanation:
   ```bash
   ee diag environment-attestation --workspace . --include-rch --json
   ```
   See [`docs/environment_attestation.md`](docs/environment_attestation.md) for
   `sourceAuthority`, verdict, severity, and recovery-action interpretation.
5. Reserve edit surfaces through Agent Mail and mark the bead with
   `br update <id> --status in_progress --json` only when the gate reports
   `safeToClaim=true`, `verdict=safe_to_claim`,
   `selectedCandidate.ownership=unassigned`,
   `selectedCandidate.editScope.state=known` with nonempty paths, and a
   structured `claimCommandAction` for that candidate. Self-owned work reports
   `continue_owned_work` and deliberately emits no second claim.
   If the only blocker is missing Agent Mail evidence, generate a redacted
   `ee.agent_mail.snapshot.v1` file and retry the same claim-gate command with
   `--agent-mail-snapshot` before deciding. A snapshot is read-only evidence,
   not authorization; remaining `unsafeReasons` still require coordination.
   The RCH authority fields are intentionally separate:
   `sourceAuthority.rchRemoteOnlyRequired=true` requires
   `sourceAuthority.rchSafeToLaunchCargoVerification=true`. Harnesses fail
   closed when remote-only verification is required and the positive RCH proof
   is missing or false; a green local compile posture is not enough to claim
   Rust work.
   The reference consumer output schema is
   `docs/schemas/ee.agent.work_packet_gate_decision.v1.json`.
6. Use RCH for Cargo verification, especially when the brief reports `rec.resource_pressure.use_rch_for_cargo`.
7. Rerun the brief after large edits, after reservation changes, and before handoff.

The brief sits beside the existing tools. The `scripts/br_retry.sh actionable --json`
command is the safe claim queue for open, unassigned, non-epic leaves. Raw
`br ready --limit 0 --json --no-auto-import --no-auto-flush --allow-stale`
remains the complete broad source of ready-work records and can include parent
epics or rows that should not be claimed without cross-checking.
`bv --robot-triage` remains the graph-aware ranking engine. The
`ee swarm work-packet --claim-gate --json` command is the claim-safety gate
that must agree before an agent uses a BV copy-paste claim command or mutates
Beads in a shared checkout. Agent Mail remains the authority for reservations
and coordination messages. Handoff capsules and support bundles carry
diagnostic snapshots such as `swarm_brief_summary.json` and
`environment_attestation_summary.json`, but a live brief, live attestation when
source authority is disputed, and claim gate are still the preflight before new
claims. Profile reports and performance forensics diagnose host behavior in
detail; the brief only carries enough posture to steer choices such as routing
Cargo through RCH.

Raw `bv --robot-*` probes are liveness-sensitive. Run them only with an
explicit external timeout, or route work selection through `ee swarm brief` /
`ee swarm work-packet`, which converts timeout or no-output cases into
`bv_command_timeout` / `bv_no_output` degradations. Those degradations make
BV ranking advisory only: continue from bounded stale-safe Beads evidence such
as `br ready --limit 0 --json --no-auto-import --no-auto-flush --allow-stale`,
cross-check with
`scripts/br_retry.sh actionable --json`, and do not use a BV claim command
unless the same candidate is present in the actionable queue and the claim gate
later reports `safeToClaim=true` with a structured `claimCommandAction`.

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

### Workspace hygiene and commit readiness

`ee workspace hygiene` is a read-only dirty-checkout classifier for agents and
pre-commit hooks.

| Bucket | Meaning |
|---|---|
| `stage_candidate` | Regular source, tests, and docs after content review |
| `do_not_commit` | Generated files, scratch files, local-machine state, or secret risk |
| `needs_human_review` | Large diffs, dependency changes, infrastructure changes, or schema migrations |
| `ignore_for_now` | Known transient state, such as logs or peer-owned churn |

| Kind | Examples |
|---|---|
| `source`, `test`, `docs` | Normal tracked code surfaces |
| `beads_metadata` | Beads state and workflow files |
| `generated`, `scratch`, `local_machine` | Build output, temp logs, editor config, local env |
| `secret_risk` | API keys, private keys, `.env` files, cloud credentials, tokens |
| `binary`, `unknown` | Large binaries or paths without a known class |

Useful checks:

```bash
ee workspace hygiene --workspace . --json \
  | jq '.data.pathClassifications | group_by(.bucket) | map({bucket: .[0].bucket, count: length})'

ee workspace hygiene --workspace . --mode precommit --strict-advisory --json

scripts/commit-hygiene-classifier.sh --strict --json
```

The report can include Agent Mail reservations and Beads links, so an agent can
see whether a path is risky because of content, ownership, or current work
coordination.

Use the commit-hygiene classifier after staging and before source commits in
crowded checkouts. A `mixed_full_tracker_export_churn` verdict means split the
source/docs/test commit from the tracker-only `.beads/issues.jsonl` sync; an
intentional mixed commit needs the classifier JSON pasted into the Beads or
Agent Mail handoff.

### Handoff and resume

Use a handoff capsule when another agent or another machine should resume a
mid-task state.

| Command | Purpose |
|---|---|
| `ee handoff create --workspace . --out <capsule.json> --json` | Write a signed capsule |
| `ee handoff inspect <capsule.json> --workspace . --json` | Inspect capsule contents without consuming it |
| `ee handoff preview <capsule.json> --workspace . --json` | Show resume effects before consuming it |
| `ee handoff resume <capsule.json> --workspace . --json` | Consume the capsule and re-warm context |
| `ee handoff rotate-key <capsule.json> --workspace . --json` | Re-sign after suspected exposure |

Capsules carry bead context, recent commits, reservations, last pack ID,
posture, next steps, redaction summary, and content hashes. They are
HMAC-signed files, so treat the capsule path as a credential. Use `ee support
bundle` for bug reports and `ee export` for memory transfer; handoff is for
resuming work.

### Swarm schema contracts

Swarm-scale JSON contracts live in [`docs/schemas/swarm/`](docs/schemas/swarm/)
with companion agent-facing notes in [`docs/swarm/`](docs/swarm/). The catalog
covers producer metadata, trust lanes, verification evidence, coordination
snapshots, resource profiles, pack SLOs, recommendations, consensus, conflicts,
fixture manifests, and planned handoff memory-set fingerprints.

The replay-lab workflow is documented in
[`docs/agent-ux/workload-replay.md`](docs/agent-ux/workload-replay.md) and
[`docs/agent-ux/swarm-replay-contracts.md`](docs/agent-ux/swarm-replay-contracts.md).
Use `ee lab swarm replay --trace <workload.json> --dry-run --json` for the
side-effect-free smoke path. `./scripts/verify.sh` runs the no-Cargo smoke
script, while standard and large-host replay proofs stay behind RCH-only
`scripts/rch_verify.sh` commands.

Every schema carries an `x-ee-status` marker. Agents should treat
`"shipped": false` as documentation for a future surface, not runtime
availability. The schema catalog does not turn `ee` into a scheduler, web
service, mail sender, Beads mutator, or agent loop.

### Mesh and Tailscale

Mesh is optional. Local-first operation is the default. Use mesh when a trusted
tailnet or local file-exchange path is part of the agent workflow.

The shipped sharing path is policy-governed file export/import plus local
Tailscale observation. Live EE-to-EE synchronization is not yet available:
the production foreground supervisor currently uses a no-op transport, so a
successful `ee mesh sync --once` cycle does not prove peer contact, network
hello, anti-entropy exchange, or body transfer.

| Command | Purpose |
|---|---|
| `ee mesh init --json` | Inspect foreground mesh readiness without starting a daemon |
| `ee mesh status --json` | Report local mesh posture, cache counts, and repair commands |
| `ee mesh peers --json` | List configured peers, including the opaque `peerId` used by lane-consent commands, and anti-entropy cursors |
| `ee mesh peer add\|list\|show\|rotate\|revoke\|unknown-attempt` | Manage app-level mesh peer records after explicit consent |
| `ee mesh auto-enroll --json` | Materialize Tailscale-discovered peers from fresh autodiscovery |
| `ee mesh discovery-policy [set\|allow\|deny] --json` | Inspect or update caller/responder discovery policy |
| `ee mesh hello-responder status --json` | Inspect the local hello responder lifecycle job |
| `ee mesh preview-grant <peer-id> --lane <lane> --json` | Emit the deterministic, token-free `ee.mesh.lane_grant_preview.v2` snapshot without mutating policy |
| `ee mesh preview-grant <peer-id> --lane <lane> --issue-approval-token --json` | Explicitly issue a sensitive, short-lived approval bearer bound to the canonical preview |
| `ee mesh grant <peer-id> --lane <lane> --preview-token-stdin --json` | Verify a bearer from bounded stdin, advance the target generation, grant the lane, and audit atomically |
| `ee mesh revoke-lane <peer-id> --lane <lane> --json` | Deny one lane, always advance its generation, invalidate prior previews, and audit atomically |
| `ee mesh export --peer <peer-id> --out <file> --json` | Write a redaction-safe artifact for an enrolled, enabled peer; use `ee export` or `ee backup` for local backups |
| `ee mesh import --file <file> --json` | Import a foreground artifact; peer rows require exact prior local consent, and cursors advance only through locally durable contiguous accepted replay |
| `ee mesh sync --once --json` | Run one foreground supervisor cycle; the current no-op transport may contact zero peers |

Mesh command mode can be selected per command or through `EE_MESH_MODE`:

```bash
ee search "release proof" --workspace . --mesh off --json
ee pack "handoff this bead" --workspace . --mesh cache --json
ee status --workspace . --mesh revisable --json
ee mesh discovery-policy --explain --json
```

Lane consent targets the opaque enrolled `peerId`, not a raw Tailscale node
key. Inspect an ordinary preview first. For a non-interactive JSON grant, issue
the approval bearer explicitly and pipe only that field into the bounded stdin
surface so it is not stored in a shell variable:

```bash
PEER_ID=peer_example123
ee mesh preview-grant "$PEER_ID" --lane body --workspace . --json
ee mesh preview-grant "$PEER_ID" --lane body --workspace . \
  --issue-approval-token --json \
  | jq -r '.data.preview.approvalToken.value' \
  | ee mesh grant "$PEER_ID" --lane body --workspace . \
      --preview-token-stdin --json
ee mesh revoke-lane "$PEER_ID" --lane body --workspace . --json
```

Ordinary previews remain deterministic and contain no bearer. Explicitly
issued approval tokens are secrets: do not log, persist, echo, or place them in
arguments. They expire after 15 minutes and become stale when the target,
policy, generic memory/mesh-ledger candidate revisions, source-derived
redaction-scanner generation, redacted sample, or grant generation changes.
Opted-in issuance necessarily writes the bearer to stdout. ee-controlled sinks
scrub it, but external or third-party stdout/session recorders outside ee's
control may retain it until the 15-minute expiry.
Revocation stops future serving but cannot erase bytes a peer already cached or
copied.

Related docs:

| Doc | Purpose |
|---|---|
| [`docs/adr/0037-optional-mesh-memory.md`](docs/adr/0037-optional-mesh-memory.md) | Optional mesh design |
| [`docs/adr/0041-mesh-anti-entropy-model.md`](docs/adr/0041-mesh-anti-entropy-model.md) | Anti-entropy model |
| [`docs/mesh/operator_onboarding.md`](docs/mesh/operator_onboarding.md) | Operator workflow |
| [`docs/mesh/command_modes.md`](docs/mesh/command_modes.md) | `off`, `cache`, `revisable`, and `blocking` modes |
| [`docs/agent-ux/auto_enrollment_onboarding.md`](docs/agent-ux/auto_enrollment_onboarding.md) | Agent auto-enrollment checklist |

### Import & ingestion

| Command | Purpose |
|---|---|
| `ee import cass --workspace . [--limit N] [--dry-run]` | Pull session evidence from `coding_agent_session_search` |
| `ee import jsonl --source <file>` | Restore from a JSONL records file, including backup record exports |
| `ee import eidetic-legacy --source <path> --dry-run` | One-time migration of legacy Eidetic Engine artifacts (read-only) |

### Curation & rules

| Command | Purpose |
|---|---|
| `ee journal append "<text>" [--source hook\|manual] [--json]` | Append one working-tier observation for later review; `--stdin` accepts JSONL batches |
| `ee journal distill [--dry-run\|--apply] [--json]` | Distill journal observations into pending curation candidates without applying them as memories |
| `ee journal list` / `show <entry-id>` | Inspect journal entries, redaction/truncation state, and whether an entry has been distilled |
| `ee review session <id> --propose [--dry-run]` | Distill imported CASS session evidence into proposed memories/rules |
| `ee curate candidates [--workspace .]` | List pending curation candidates |
| `ee curate doctor [--limit N] [--trend] [--json]` | Read-only memory-debt report with ranked suggested repairs for stale anchors, unresolved contradictions, never-retrieved or orphan memories, low-trust high-rank items, and decay-imminent high-utility rows |
| `ee curate validate <id>` | Run validation (specificity, duplication, scope, evidence) |
| `ee curate apply <id>` / `accept <id>` / `reject <id>` / `snooze <id>` / `merge <a> <b>` | Lifecycle transitions |
| `ee curate disposition` | Evaluate TTL disposition policy without silent mutation (`--apply` is required to write) |
| `ee learn gaps [--since <RFC3339>] [--limit N] [--json]` | Cluster retained search/ask miss demand into redacted representatives, nearest existing evidence, and remember templates; clusters flip to `likely_covered` (with `coveredBy`) once a newer memory satisfies the demand |
| `ee playbook extract [--since <RFC3339>] [--dry-run]` | Propose procedural-rule candidates from repeated semantic memories |
| `ee playbook list [--limit N]` | List procedural rules in portable playbook form |
| `ee playbook export --out <file> [--dry-run]` | Write a no-overwrite portable playbook artifact |
| `ee playbook import --source <file> [--apply]` | Dry-run or apply a portable playbook import through audited rule writes |
| `ee rule add` / `list` / `show <id>` / `mark <id>` / `protect <id>` / `update <id>` | Direct rule management |

Outcome signal vocabulary:

| Signal | Use |
|---|---|
| `helpful` | Memory directly changed the result for the better |
| `harmful` | Memory misled the operator or agent |
| `confirmation` | Independent evidence supported the memory |
| `contradiction` | New evidence conflicts with the memory |
| `stale` | Convention or fact was superseded |
| `inaccurate` | Body contains a factual error |
| `outdated` | Version-specific fact no longer applies |
| `positive` | Useful but weaker than `helpful` |
| `negative` | Unhelpful but weaker than `harmful` |

Targets can be memories, packs, or curation candidates:

```bash
ee outcome <memory-id> --signal helpful --reason "Caught a release gate omission" --workspace .
ee outcome <pack-id> --target-type pack --signal helpful --reason "Included the missing RCH rule" --workspace .
ee outcome --pack <pack-id> --item 2 --signal harmful --reason "Selected stale advice" --workspace .
ee outcome <candidate-id> --target-type candidate --signal negative --reason "Too vague after review" --workspace .
```

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
| `ee memory promote-global <id> [--dry-run]` | Evidence-gated copy-with-link promotion of a workspace memory into the user-global lane; refusals are typed exit-7 plans (`ee.global_promotion.plan.v1`) |
| `ee memory demote-global <global-id>` | Audited tombstone of a promoted global row (`ee.global_demotion.report.v1`); tombstoned rows never re-enter candidate pools |
| `ee memory outcome-global <global-id> --signal helpful\|harmful [--dry-run]` | Feedback on a global row with clamped confidence backflow to the origin workspace row (`ee.global_promotion.backflow.v1`) |

### Graph

| Command | Purpose |
|---|---|
| `ee graph pagerank [--limit N]` | Compute PageRank scores over memory links |
| `ee graph betweenness [--limit N]` | Compute betweenness centrality over memory links |
| `ee graph hits [--limit N]` | Compute HITS hub and authority scores |
| `ee graph louvain [--resolution R]` | Compute Louvain communities |
| `ee graph communities [--limit N]` | Compute label-propagation communities |
| `ee graph k-core [--k K]` | Extract a k-core, defaulting to the main core |
| `ee graph articulation [--limit N]` | List articulation points for structural-decay and bridge analysis |
| `ee graph path <src> <dst>` | Find the shortest memory-link path between two memories |
| `ee graph explain-link <src> <dst>` | Explain direct and path-based graph evidence between memories |
| `ee graph export [--workspace .]` | Export a deterministic graph snapshot artifact |
| `ee graph snapshot refresh --graph <type>` | Refresh typed snapshots: `memory_links`, `causal`, `revision`, `rules`, `contradictions`, or `all` |
| `ee graph neighborhood <id> [--direction both] [--limit N]` | Expand around a memory/session/rule |
| `ee graph centrality [--algorithm <name>]` | Read persisted centrality scores, including `pagerank`, `betweenness`, `authority`, `hits-hubs`, and `hits-authorities` |
| `ee graph centrality-refresh [--dry-run]` | Refresh PageRank / betweenness metrics |
| `ee graph feature-enrichment [--dry-run]` | Compute bounded graph-derived retrieval features |
| `ee graph suggest-links [--limit N] [--min-score S] [--propose]` | Typed link prediction (related/supports/contradicts) with blended, explained scoring; `--propose` writes curation candidates, never links directly (`ee.graph.suggest_links.v1`) |
| `ee graph diff [--graph <family>] [--from ID] [--to ID] [--since RFC3339]` | Temporal structural diff between two persisted snapshots: content-hash-keyed add/remove sets, fingerprint-matched community deltas, persisted-centrality movers (`ee.graph.diff.v1`) |
| `ee insights [--section <name>] [--explain <id>] --json` | Inspect graph-derived findings and memory-centric topology |
| `ee proximity <memory-a> <memory-b> --json` | Explain Gomory-Hu min-cut proximity between two memory nodes |

### Conflicts

| Command | Purpose |
|---|---|
| `ee conflict list` / `explain <id>` / `cluster` | Read-only ranked contradiction surface: conflicting pairs with both bodies + the preferred side, and k-truss/Louvain clusters (`ee.conflict.v1`) |
| `ee conflict resolve <a> <b> --verb supersede\|reject-one\|scope-split\|both-valid [--keep ID] [--reason "..."] [--apply]` | Audited resolution against the LIVE surface; dry-run plan by default. Every mutation maps onto existing audited atoms and the rationale persists as a `kind=decision` memory (`ee.conflict.resolve.v1`) |

Resolution is terminal: a tombstoned side (superseded or rejected) drops the
pair from the actionable surface, and re-running against a moved surface
refuses with `conflict_resolve_stale_surface` plus the focused live state.
See [`docs/agent-ux/graph-intelligence.md`](docs/agent-ux/graph-intelligence.md)
for the full densification and resolution loop.

### Index

| Command | Purpose |
|---|---|
| `ee index status` / `rebuild` / `reembed` | Manage derived search indexes (Frankensearch owns model selection) |
| `ee index vacuum` | Preview reclaimable derived search-index artifacts without deleting or rewriting files |

Index intake never mutates active Frankensearch tiers in place. The previous
generation remains readable while a complete replacement is built and
validated; a failed commit restores it and preserves the unpublished generation
in a non-recoverable rejected quarantine. See [`docs/indexing.md`](docs/indexing.md) for the cancellation,
equivalence, fallback, and RCH-only E2E contracts.

### Workspace, models, schemas

| Command | Purpose |
|---|---|
| `ee workspace resolve` / `list` / `alias <name>` | Identity, monorepo subscopes, and aliases |
| `ee workspace hygiene [--mode report\|precommit] --json` | Dirty-path hygiene, secret-risk, generated/scratch/local-machine classification, and commit-readiness guidance |
| `ee migrate status` / `run` / `shard-fanout --dry-run` | Migration posture and shard-fanout planning |
| `ee db status` / `inspect <table>` / `check-integrity` / `reindex --dry-run` | Inspect FrankenSQLite schema, table rows, integrity, and derived-index rebuild plans without bypassing `ee` |
| `ee model status` / `list` | Inspect embedding model registry posture |
| `ee schema list` / `export <schema-id>` | Inspect stable machine-output schemas |

### Focus and memory bias

Focus state is a small workspace-local bias for the next task family. It changes
ranking, not trust class or stored content.

| Command | Purpose |
|---|---|
| `ee focus set <mem...> --workspace . --json` | Replace the active focus set |
| `ee focus show --workspace . --json` | Inspect active focus |
| `ee focus add <mem> --workspace . --json` | Add one memory |
| `ee focus remove <mem> --workspace . --json` | Remove one memory |
| `ee focus clear --workspace . --json` | Clear the focus state |
| `ee focus explain --workspace . --json` | Explain focus and per-agent bias effects |

`EE_AGENT_NAME` lets `ee` attribute outcomes to an agent identity. After enough
outcome events, per-agent bias can nudge familiar memories while keeping the
base retrieval signal dominant.

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
| `ee doctor --quick\|--robot-triage\|--capabilities\|--gc-plan <days>` | Focused repair and operator triage surfaces |
| `ee health scorecard [--record-snapshot] --json` | Trend-aware memory-health scorecard with coverage, freshness, trust, redundancy, graph, and top-action signals |
| `ee curate doctor --trend --json` / `ee learn gaps --json` | Content-health diagnostics: memory-debt queue, steward trend snapshots, and demand-driven gap templates |
| `ee preflight run "<task>"` / `show` / `close` | Task risk assessment, tripwire context, and post-run feedback |
| `ee preflight check --cmd "<command>" --json` | Advisory command-risk memory lookup; use `--stdin` or `--cmd-base64` to keep command text off argv when needed. `ee` never denies execution |
| `ee tripwire list` / `check` | Inspect and check preflight tripwires |
| `ee tripwire check --revivals [--limit 1..=100] --json` | Evaluate a bounded, deterministic prefix of current workspace-local Revive specs and return those whose predicate passes; the default limit is 25 and capped responses include a higher-limit repair. This explicit revival-sentinel evaluator may run allowlisted `ee ... --help` introspection under strict wall-time and redacted output caps. The implicit revival-sentinel evaluator used inside `ee orient` uses only local read-only predicates and does not execute command-help processes; this is not a claim that every other orient component is process-free. Both evaluator modes exclude Gate specs, replace raw targets with domain-separated digests, and perform no automatic result, trust, or tombstone mutation. There is intentionally no cursor because live predicate results can change without a DB generation. |
| `ee diag plan-cache` | EQL query plan-cache counters and integration posture |
| `ee diag contention [--use-daemon] [--json]` | Read-only swarm hot-path contention posture: write-lock, read-pool, single-flight (plus group-commit / incremental-index / L2 when present), with a severity-ranked `topContention` list (see [`docs/agent-ux/contention-observability.md`](docs/agent-ux/contention-observability.md)) |
| `ee diag environment-attestation --workspace . --include-rch --json` / `disk-pressure` / `build-admission` / `artifacts` | Read-only environment source-authority, storage, artifact, and build-admission diagnostics |
| `ee diag graph` / `graph-snapshot` / `search` | Graph, snapshot, and retrieval diagnostics |
| `ee diag integrity` / `dependencies` / `streams` | Integrity, dependency, and stdout/stderr stream checks |
| `ee verify ingest` / `ee verify rch ingest` / `ee verify rch blockers` / `ee verify rch runs` / `proofs` / `broker lookup` / `closure-guidance` | Verification evidence, durable RCH proof ledger queries, proof checks, reusable RCH evidence, and closeout guidance |
| `ee maintenance run` / `status` / `wal-checkpoint` / `graph-snapshot-prune` / `graph-witnesses-prune` | Explicit maintenance jobs and retention helpers |
| `ee job run` / `list` / `show` | Durable steward job history and explicit job execution |
| `ee install check` / `plan` and `ee update` | Agent-safe install/update checks and dry-run plans |
| `ee eval run` / `list` | Run or list retrieval-quality evaluation fixtures |
| `ee eval report [fixture]` | Summarize fixture IDs, data hashes, aggregate retrieval metrics, and the first failing query |
| `ee eval run <fixture> --pack-quality --json` | Check whether deterministic fixtures still select required context-pack evidence |
| `ee ask "<question>" --workspace . --json` | Answer a narrow question extractively from stored memories, with citations, conflict sides, and calibrated abstention |
| `ee perf compare --baseline <baseline.json> --candidate <candidate.json> --json` | Compare normalized performance artifact summaries without mutating state |
| `ee perf budget check --profile <name> --report <artifact.json> --json` | Check one normalized performance artifact against a profile budget |
| `ee perf explain-latency --surface search\|context --report <artifact.json> [--log <j1.jsonl>] --json` | Explain deterministic latency stages and cache posture from normalized search/context artifacts and optional J1 timing evidence |
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

Use ask-quality evaluation for direct answers that must stay extractive and
citation-backed. `ee eval run ask_v1 --json` gates citation precision, answer
exactness, calibrated abstention, and conflict recall against the committed
Project Zephyr fixture corpus; `scripts/e2e_ask.sh` exercises the same public
CLI path end to end.

---

## Configuration

`ee` reads config in this precedence order (highest wins):

1. CLI flags
2. Environment variables (`EE_*`)
3. Project config: `<workspace>/.ee/config.toml`
4. User config: `~/.config/ee/config.toml`
5. Built-in defaults

Unknown TOML keys are rejected rather than silently ignored. The error names the
full key path and, when there is one unambiguous close match among sibling keys,
includes that key as a conservative suggestion.

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
subprocess_timeout_secs = 30        # wall-clock budget per cass subprocess call
                                    # (raise for large corpora; env override: EE_CASS_TIMEOUT_SECS)

[search]
default_speed   = "balanced"         # fast | balanced | thorough
lexical_weight  = 0.45
semantic_weight = 0.45
# Wired for default hybrid retrieval. `semantic_weight` applies to the
# neural-local Model2Vec arm when the bundled model is available; it
# deterministically renormalizes to lexical scoring when hash fallback is active.
graph_weight    = 0.10
# Query-plan cache sizing is environment-only: EE_QUERY_PLAN_CACHE_ENTRIES=1024
query_miss_retention_days = 30        # retained hash-only miss demand for `ee learn gaps`

[pack]
default_profile  = "balanced"
default_format   = "markdown"
default_max_tokens = 4000
adaptive_budget  = false
mmr_lambda       = 0.7
candidate_pool   = 100
memory_tier_admission = false

[curation]
duplicate_similarity = 0.92
harmful_weight       = 2.5            # harmful feedback hits harder than helpful
decay_half_life_days = 60

[journal]
enabled = true
retention_days = 14                   # applied only by the explicit journal-retention steward job

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

[graph.memory]
snapshot_cap_mb = 250
per_algorithm_cap_mb = 100

[graph.witnesses]
retention_days = 30

[cache.pack_l2]
enabled = true
max_bytes = 1073741824

[mesh]
enabled = false
command_mode = "off"                  # off | cache | revisable | blocking

# Discovery/responder policy is NOT config.toml: it lives in workspace-local
# TOML files (<workspace>/.ee/discovery_policy.toml plus the
# discovery_allowlist / discovery_denylist / respond_allowlist files),
# managed by `ee mesh discovery-policy set|allow|deny`, with
# EE_TAILSCALE_DISCOVERY_MODE / EE_TAILSCALE_RESPOND_MODE as env overrides.
```

Environment variable overrides:

| Variable | Equivalent |
|---|---|
| `EE_DATABASE_PATH` | `[storage].database_path` |
| `EE_INDEX_DIR`     | `[storage].index_dir` |
| `EE_PROFILE`       | `[pack].default_profile` |
| `EE_MAX_TOKENS`    | `[pack].default_max_tokens` |
| `EE_AGENT_NAME` | agent identity for outcome attribution and per-agent bias |
| `EE_SECURITY_PROFILE` | workspace/import security posture; it does not govern shell commands |
| `EE_JOURNAL_ENABLED` | `[journal].enabled` capture gate; false makes journal surfaces report `journal_disabled` |
| `EE_JOURNAL_RETENTION_DAYS` | `[journal].retention_days` for the explicit journal-retention steward job |
| `EE_HARMFUL_PER_SOURCE_PER_HOUR` | `[feedback].harmful_per_source_per_hour` |
| `EE_HARMFUL_BURST_WINDOW_SECONDS` | `[feedback].harmful_burst_window_seconds` |
| `EE_QUERY_PLAN_CACHE_ENTRIES` | query-plan cache size (environment-only; no TOML key) |
| `EE_QUERY_MISS_RETENTION_DAYS` | `[search].query_miss_retention_days` for hash-only search/ask miss demand retained by `ee learn gaps` |
| `EE_PPR_CACHE_ENTRIES` | PPR prefetch cache size |
| `EE_L2_PACK_CACHE_BYTES` / `EE_L2_PACK_CACHE_DIR` / `EE_L2_PACK_CACHE_DISABLE` | pack L2 cache controls |
| `EE_READ_POOL_SIZE` / `EE_READ_POOL_ACQUIRE_TIMEOUT_MS` / `EE_READ_POOL_MAX_PIN_SECONDS` | read-pool controls |
| `EE_GRAPH_MEMORY_SNAPSHOT_CAP_MB` / `EE_GRAPH_MEMORY_PER_ALGORITHM_CAP_MB` | graph working-set admission controls |
| `EE_MESH_ENABLED` / `EE_MESH_MODE` | `[mesh].enabled` / `[mesh].command_mode` |
| `EE_TAILSCALE_DISCOVERY_MODE` / `EE_TAILSCALE_RESPOND_MODE` | Tailscale discovery and responder policy |
| `EE_TAILSCALE_PEER_PROBE_TIMEOUT_MS` / `EE_TAILSCALE_DISCOVERY_BUDGET_MS` | Tailscale peer-discovery budgets |
| `EE_FLIGHT_RECORDER` / `EE_FLIGHT_RECORDER_DIR` / `EE_FLIGHT_RECORDER_RETENTION_DAYS` | flight-recorder controls; see [`docs/agent-ux/flight-recorder.md`](docs/agent-ux/flight-recorder.md) |
| `EE_WORKSPACE_HYGIENE_ALWAYS_REVIEW_PATTERNS` / `EE_WORKSPACE_HYGIENE_GENERATED_PATTERNS` / `EE_WORKSPACE_HYGIENE_LOCAL_MACHINE_PATTERNS` / `EE_WORKSPACE_HYGIENE_SCRATCH_PATTERNS` | workspace hygiene classifier overlays |
| `EE_SCIENCE_BACKEND_PATH` | optional science analytics backend health path |
| `EE_DISABLE_REMEMBER_SEARCH_NEIGHBORS` | disables Frankensearch neighbors for remember-time curation proposal |
| `EE_DISABLE_TOON` | disables TOON capability reporting and auto-selection |
| `EE_NO_COLOR`      | disables ANSI styling on stderr |
| `EE_TRACE`         | enables structured tracing to stderr |

The full registry is [`docs/env_vars.md`](docs/env_vars.md) and the code source
is `src/config/env_registry.rs`.

Feature flags:

| Flag | Status | Notes |
|---|---|---|
| `default` | active | `fts5`, `json`, `embed-fast`, `lexical-bm25`, `graph` |
| `fts5` | active | Frankensearch FTS5 lexical fallback |
| `json` | reserved | JSON output is unconditional today; flag is reserved for a minimal profile |
| `embed-fast` | active | Frankensearch `model2vec` semantic embedder plus the asupersync-backed model download path |
| `lexical-bm25` | active | Frankensearch BM25 scorer |
| `graph` | active | Default-on graph analytics surface |
| `differential-networkx` | active test gate | Heavy Python NetworkX differential suite |
| `mcp` | active optional adapter | Stdio adapter module; default builds keep manifest discovery |
| `serve` | reserved | Future localhost HTTP/SSE adapter |
| `science-analytics` | reserved | Future analytics subsystem; current CLI reports degraded/unavailable posture |

See [`docs/feature_flag_registry.md`](docs/feature_flag_registry.md) for the
tracked owner and status of each flag.

---

## Architecture

```
                ┌─────────────────────────────────────────────────┐
                │  Coding Agent (Claude Code · Codex · Cursor …)  │
                └──────────────────────┬──────────────────────────┘
                                       │
                   ee pack · search · remember · import · curate
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

**Native Asupersync.** Runtime-facing async APIs take `&Cx`, return `Outcome<T>`, and preserve budget/cancellation semantics where wired.

Additional runtime-adjacent modules:

| Module | Role |
|---|---|
| `mesh` | Optional peer exchange, Tailscale autodiscovery, hello responder, anti-entropy, policy, authenticated lane consent, and revocation |
| `obs` | Flight recorder, structured tracing, posture helpers, and diagnostic evidence |
| `hooks` | Memory-oriented agent harness helpers for recall, orientation, journaling, and capture |
| `steward` | Bounded maintenance jobs, spec packs, and optional daemon work |
| `shadow` | Read-only shadow/diagnostic support paths |

---

## Storage Layout

```
~/.local/share/ee/
├── ee.db                   # FrankenSQLite source of truth (WAL mode)
├── indexes/
│   └── combined/
│       ├── manifest.json   # generation, model id, lexical+vector files
│       └── ...             # Frankensearch artifacts
├── cache/                  # transient, safe to wipe
└── logs/                   # tracing-subscriber JSON logs

<workspace>/.ee/            # optional project artifacts (git-friendly)
├── config.toml             # checked-in project overrides
├── backups/                # default `ee backup create` root
├── index/                  # default workspace index dir for local runs
├── discovery_policy.toml   # optional mesh discovery/responder policy
├── discovery_allowlist.toml / discovery_denylist.toml / respond_allowlist.toml
├── auto_enroll_overrides.toml  # reviewed mesh auto-enrollment overrides
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

Registry-backed kinds can also carry a typed sidecar with schema
`ee.memory.typed_fields.v2`. Use repeatable `--field NAME=VALUE` on
`ee remember` or `ee note`, or let `ee` extract explicit labels such as
`Family:`, `Chosen:`, `Command:`, `Condition:`, or `Scope:` from the body.
Explicit assignments override same-name extracted values. Bare prose stays
bare; `ee` does not fabricate machine fields.

| Kind | Typed fields |
|---|---|
| `failure` | `cause`, `regression_surface`, `reverted_at_sha`, `family` |
| `decision` | `options`, `chosen`, `rationale`, `supersedes`, `revisit_by` |
| `command` | `command`, `when_to_use`, `exit_meaning` |
| `rule` | `condition`, `action`, `exceptions` |
| `convention` | `scope`, `pattern` |
| `risk` / `anti-pattern` | `trigger`, `blast_radius`, `safer_alternative` |

`ee search` filters typed fields with `--kind <kind>` plus repeatable `--field`
operators: `name=value` for exact, `name~value` for contains, and `name^value`
for prefix. The full registry, bounds, indexed-field status, and v1-to-v2
compatibility notes live in
[`docs/memory-typed-fields.md`](docs/memory-typed-fields.md).

Attempt-family multiplicity is a separate source-of-truth ledger, not the
free-form typed `family` field. Record each sibling with the same `--family`
and declared denominator plus a unique slot/outcome, then retrieve the whole
family without a dummy query or an index rebuild:

```bash
ee remember "selected approach" --family release-v4 --of-n 3 \
  --attempt 1 --attempt-outcome selected --json
ee remember "timeout failure" --family release-v4 --of-n 3 \
  --attempt 2 --attempt-outcome rejected --json
ee remember "permission failure" --family release-v4 --of-n 3 \
  --attempt 3 --attempt-outcome rejected --json
ee search --family release-v4 --json
```

```bash
ee remember "Remote verification won the storage decision." \
  --kind decision \
  --field "chosen=RCH remote" \
  --field "options=local Cargo" \
  --field "options=RCH remote" \
  --json
```

Decision memories have a dedicated micro-ADR workflow through `ee decide`.
`record` creates or supersedes a decision chain head, `list` reviews current
heads, and `revisit` surfaces due or near-due decisions. See
[`docs/agent-ux/decide.md`](docs/agent-ux/decide.md).

### Memory lanes: workspace and user-global

Memories live in one of two lanes. The **workspace lane** is the default:
every row belongs to the workspace whose `.ee` store holds it. The
**user-global lane** is a separate store under the user data root
(`$XDG_DATA_HOME/ee/global`, falling back under `$HOME`) that shares
procedural knowledge across all of one user's workspaces — house rules,
hard-won anti-patterns, cross-project playbooks. It never crosses the mesh.

Rows enter the global lane two ways: authored directly
(`ee remember --global`), or promoted from a workspace
(`ee memory promote-global <id>`). Promotion is **copy-with-link** — the
origin workspace keeps its row and audit history; the global copy carries
`derived_from` provenance and its own feedback life — and it is
**evidence-gated**: only `human_explicit` or `agent_validated` memories
qualify, everything else gets a typed exit-7 refusal plan
(`--dry-run` previews the verdict without writing).

Precedence at retrieval is fixed: global rows compete in the same
pack/recall/primer/search sections as workspace rows, always labeled
(`storeLane=global`, provenance `source_type=global_store`); an
exact-content twin resolves workspace-wins; a genuine contradiction defers
to the operator via `global_lane_conflict_deferred` with both sides kept
visible. Two `[memory]` config keys control participation:
`include_global = false` stops reading the lane in a workspace, and
`participate = false` isolates a workspace in both directions. When the
lane is off or empty, retrieval says so with `global_lane_disabled`
instead of silently narrowing. See the
[trust model](docs/trust-model.md#global-knowledge-lane) for how the lane
sits in the trust taxonomy.

---

## Context Profiles

Different tasks need different memory mixes. `--profile` currently selects one of the shipped context-packing profiles without bypassing trust or privacy:

| Profile | Bias |
|---|---|
| `compact` | Prioritizes procedural rules and known failure modes in a tight budget |
| `balanced` | Default mix across rules, decisions, failures, evidence, and artifacts |
| `grounding` | Uses balanced quotas and boosts HITS authority evidence when graph scores are available |
| `orientation` | Uses balanced quotas and boosts HITS hub memories when graph scores are available |
| `thorough` | Expands evidence and artifact coverage for higher-recall work |
| `submodular` | Uses the facility-location objective with thorough section quotas for deterministic diversity |

Output formats:

| Format | Use |
|---|---|
| `markdown` | **The token-tight prompt format for packs** — prepend text for agents and humans (smallest output) |
| `json` | Full structured contract for parsers |
| `toon` | Structured output with stable field order for parsers; **not** smaller than JSON for packs |
| `jsonl` with `--stream` | Incremental `ee.pack.stream.v1` frames |

For a context pack, prefer `markdown` when you want the most token-efficient
prompt material: a pack is a deeply nested structure, and TOON only compresses
uniform tabular arrays, so `--format toon` is typically *larger* than `--format
json` for packs and several times larger than `--format markdown`. TOON's token
savings apply to flat/tabular command outputs (e.g. `ee status`, `ee health`),
not to packs.

Stream consumers should read until a terminal frame. `kind: "cancelled"` can
still carry emitted items; `kind: "error"` is the hard failure path.

---

## CASS Integration

`ee` consumes `coding_agent_session_search` (`cass`) as the raw session source;
it does **not** duplicate the underlying store. An imported evidence span keeps
the source session and exact line range as provenance.

```bash
# Discover what cass has
ee import cass --workspace . --limit 50 --dry-run --json

# Real import (idempotent, resumable, ledger-tracked); read fields under .data
ee import cass --workspace . --limit 50 --json \
  | jq '.data | {status, sessionsDiscovered, sessionsImported, sessionsSkipped, spansImported, indexJobsQueued, indexRequiredAction}'

# Apply data.indexRequiredAction (the default workspace form is shown here)
ee index rebuild --workspace .

# Imported excerpts are now directly retrievable as evidence
ee search "<phrase from a prior session>" --workspace . --limit 20 --explain --json

# Preview curation candidates without writing
ee review session <cass-session-id> --workspace . --propose --dry-run --json

# Persist proposals only after review, then validate and apply one
ee review session <cass-session-id> --workspace . --propose --json
ee curate candidates --workspace . --json
ee curate validate <candidate-id> --workspace . --json
ee curate apply <candidate-id> --workspace . --json
ee index rebuild --workspace .
```

Fresh imported spans have no memory link. They remain searchable, but a
memory-centric pack skips them with `context_evidence_hit_unhydrated` until a
reviewed curation candidate creates the linked memory. Rebuilding the derived
index after curation makes the new linkage visible to retrieval.

Required `cass` commands consumed (all with stable contracts):

- `cass health --json`
- `cass search "<q>" --robot`
- `cass view <path> -n <line> --json`
- `cass expand <path> -n <line> -C <ctx> --json`
- `cass capabilities --json`

If `cass` is missing, `ee` runs in degraded mode. Explicit `ee remember` records still work fully, and `ee status` clearly reports the missing capability with the install command.

---

## Beyond Coding

The same memory model works outside software when the work has durable facts,
recurring decisions, and cited sources.

| Domain | Useful memories | Typical source URI |
|---|---|---|
| Investment research | Thesis revisions, valuation methods, failed screens, peer sets | `sec-filing://...`, `earnings-call://...`, `analyst-note://...` |
| Legal work | Case rules, drafting conventions, negotiation outcomes, due-diligence steps | `case://...`, `pacer://...`, `westlaw://...` |
| Marketing analysis | Campaign retrospectives, A/B results, channel rules, segmentation methods | `ga4://...`, `mixpanel://...`, `campaign://...` |
| Product management | User research, launch retrospectives, personas, prioritization rules | `interview://...`, `linear://...`, `notion://...` |
| Security and incident response | IOCs, TTPs, response playbooks, detection-rule outcomes | `cve://...`, `mitre://...`, `incident://...` |
| Medicine or clinical operations | Guideline facts, near misses, differential-diagnosis procedures | `pubmed://...`, `guideline://...`, `emr://...` |
| Sales and account work | Call notes, objection patterns, account maps, qualification playbooks | `crm://...`, `salesforce://...`, `gong://...` |

For privileged domains, isolate by workspace, and take the workspace out of
the user-global lane entirely — a matter-confidential or patient-adjacent
workspace should neither read shared memories nor leak its own into them:

```bash
ee init --workspace ./matters/smith-v-jones --json
ee config set memory.participate false --workspace ./matters/smith-v-jones --json
ee init --workspace ./deals/2026-q3-acme --json
ee init --workspace ./positions/AAPL-long --json
```

With `memory.participate = false`, retrieval in that workspace reports
`global_lane_disabled` honestly instead of silently narrowing, and
`ee memory promote-global` refuses to move anything out.

`cass` is specific to coding sessions. For other domains, use direct `ee remember` calls or
structured imports through `ee import jsonl --source <file>`.

## Negative Evidence Ledger

For long-running optimization work, record failed attempts before they disappear
into a revert. The useful artifact is the attempt, why it lost, and the smallest
measurement or source that proves it lost. Failure memories now use typed
memory fields as the formal machine-readable convention: write `Family:`,
`Cause:`, and `Reverted at SHA ...` in the body so `ee remember --kind failure`
can populate the typed sidecar. Legacy tags remain useful for broad grouping,
but `ee search --kind failure --field family=<name> --json` is the precise
filtering surface. The canonical v2 field table and bounds are documented in
[`docs/memory-typed-fields.md`](docs/memory-typed-fields.md).

| Loop step | `ee` surface |
|---|---|
| Start a campaign | `ee init --workspace ./optimization/<campaign> --json` |
| Capture a failed attempt | `ee remember "...what lost and why... Family: <name>. Cause: <root>. Reverted at SHA <sha>." --level episodic --kind failure --source <artifact-uri> --json` |
| Cluster repeated failures | `ee playbook extract --workspace ./optimization/<campaign> --dry-run --json` |
| Promote a validated anti-pattern | `ee curate validate <candidate-id>` then `ee curate apply <candidate-id>` |
| Prime the next attempt | `ee pack "<next hypothesis>" --workspace ./optimization/<campaign> --profile thorough --format markdown` |

Example capture:

```bash
ee remember "Tried: page-level cache prefetch on btree leaf reads, 64-byte stride. \
Result: -8% on small-N reads from cache pollution, +2% on scan-heavy. \
Reverted at SHA 9af3c21. Family: aggressive prefetch, third failure in this family." \
  --workspace ./optimization/query-engine \
  --level episodic \
  --kind failure \
  --tags perf,prefetch,btree-leaf,cache-pollution,family-aggressive-prefetch,regression-small-n-read \
  --source "bench-run://2026-09-12T14:23/oltp-mixed-small-n" \
  --source "git-sha://9af3c21-pre-revert" \
  --source "flamegraph://artifacts/9af3c21/cpu-prof.svg" \
  --json
```

Useful tag prefixes:

| Prefix | Meaning |
|---|---|
| `family-<name>` | Approach family, such as `family-aggressive-prefetch`; canonical typed field is `family` |
| `regression-<surface>` | Where it lost, such as `regression-tail-latency` |
| `cause-<root>` | Inferred root cause, such as `cause-cache-pollution`; canonical typed field is `cause` |
| `reverted-at-<sha>` | Decision point or revert commit; canonical typed field is `reverted_at_sha` |

---

## Shadow Policy Inventory

Shadow policy surfaces are side-effect-free by default. The public inventory
contract is `ee.shadow_policy_inventory.v1`; it lists stable policy IDs,
domains, maturity, required inputs, supported cohorts, known degraded modes, and
whether the policy can be shadowed without changing user-visible output.

Initial inventoried policies include:

| Domain | Incumbent | Candidate |
|---|---|---|
| Pack selection | `incumbent.pack.mmr_redundancy` | `candidate.pack.facility_location` |
| Cache admission | `incumbent.cache.no_cache` | `candidate.cache.s3_fifo` |
| Verification admission | `incumbent.verification.rch_only` | `candidate.verification.environment_attestation` |
| Retrieval weights | static `[search]` config weights (not a shadowable policy id) | `candidate.retrieval.outcome_tuned_weights` |

Unsupported decision surfaces must abstain instead of promote or reject. The
current inventory records `unsupported.resource_profile_budget_admission` with
`abstentionReason=unsupported_policy_domain` until that decision plane has a
safe shadow implementation.

Use `--shadow compare --policy <policy-id>` only to collect comparison evidence.
Shadow mode does not promote candidates, mutate live policy, or replace the
incumbent result without an explicit future apply step.

The retrieval-weights domain is the first with a full runnable loop:
`ee shadow run --policy candidate.retrieval.outcome_tuned_weights --json`
evaluates outcome-labeled evidence offline and persists an
`ee.shadow.retrieval_tuning_report.v1` report (abstaining honestly below the
evidence gate); `ee shadow promote [--dry-run]` applies a promotable winner
as an audited `[search]` config overlay carrying the full prior bytes, and
`ee shadow demote` restores those bytes exactly. Determinism is preserved
because adaptation is an explicit, reviewable config change — see
[docs/agent-ux/retrieval-adaptation.md](docs/agent-ux/retrieval-adaptation.md).

---

## Agent Harness Integration

### Claude Code

Add to your `AGENTS.md` or hook setup:

```text
Before starting substantial work, run:
  ee swarm brief --workspace . --json
  ee swarm work-packet --workspace . --include-rch --claim-gate --candidate <id> --json
  ee pack "<task>" --workspace . --read-only --max-tokens 4000 --format markdown

Before editing known files or a diff:
  ee recall --path <path> --workspace . --budget-tokens 400 --format markdown
  ee recall --diff HEAD --workspace . --budget-tokens 400 --json

When you discover a durable project convention:
  ee remember --workspace . --level procedural --kind rule "<rule>"

To retrieve relevant risk memory before a shell command:
  ee preflight check --cmd "<shell-command>" --workspace . --json
  printf '%s' "$cmd" | ee preflight check --stdin --workspace . --json

After a remembered rule helps or harms:
  ee outcome <id> --signal helpful
  ee outcome <id> --signal harmful
```

Managed hooks inject recall, orientation, journal, and capture context only.
They never intercept or deny shell commands. Preview the managed memory hooks
before installation:

```bash
ee hook claude-code --print --workspace . --json
ee hook claude-code --install --workspace . --json
```

The `ee pack`, `ee recall`, and hook-install JSON outputs are stable and parseable.

### Codex

Codex shells out, so the same calls work. `ee pack "<task>" --json` can be
inserted directly into a system or developer message. Use `ee recall --path`
as the pre-edit surface and preview managed hooks with:

```bash
ee hook codex --print --workspace . --json
ee hook codex --install --workspace . --json
```

### MCP

The MCP manifest is available so agents can discover the CLI contract
from default builds:

```bash
ee mcp manifest --json
ee mcp serve-stdio
ee mcp validate --json
```

When the `mcp` feature is not enabled, the manifest succeeds and reports
`capabilityGap.code=mcp_feature_disabled` for the stdio adapter. Build with
`cargo build --release --features mcp` from source when you need the adapter.
The feature gates the in-tree synchronous JSON-RPC stdio adapter; it does not
link `rust-mcp-sdk` because that SDK currently requires Tokio, which is outside
this crate's allowed runtime stack.
The manifest mirrors the CLI contracts for tools such as `ee_context`, `ee_search`,
`ee_remember`, `ee_outcome`, `ee_curate_candidates`, and `ee_memory_show`.
Default builds keep `ee mcp serve-stdio --json` discoverable and return the
same `mcp_feature_disabled` capability gap instead of starting an adapter.
Feature-enabled builds use `ee mcp serve-stdio` to run the JSON-RPC stdio
server; MCP clients should then speak the protocol over stdin/stdout.
`ee mcp validate --json` checks that manifest contract against the public schema
without starting the stdio adapter. Schemas match CLI JSON exactly; the CLI is
the compatibility contract.

### Plain humans

Use it from a shell.

---

## Privacy & Trust

### Redaction

Secrets are detected before storage. Default redaction classes: `api_key`,
`jwt`, `password`, `private_key`, `ssh_key`, `aws_secret`, `oauth_token`.
Redacted spans are replaced with stable placeholders; the original is not
written to disk.

```bash
ee remember "DATABASE_URL=postgres://user:hunter2@host/db"
# stored as: "DATABASE_URL=postgres://user:***REDACTED:password***@host/db"
```

Redaction levels:

| Level | Typical use |
|---|---|
| `none` | Local inspection only |
| `minimal` | Storage and context JSON default posture |
| `standard` | Export and handoff artifacts |
| `strict` | Shared artifacts with body truncation |
| `paranoid` | Support bundles and public diagnostics |

### Trust classes

Memories carry a trust class that affects packing priority:

| Class | Source | Initial confidence |
|---|---|---|
| `human_explicit` | User-typed `ee remember` | 0.85 |
| `peer_human_attested` | Signed origin from an active team member whose store declared `human_explicit`; this attests the member's declaration, not who typed it | 0.75 |
| `agent_validated` | Agent assertion + outcome confirmation | 0.65 |
| `agent_assertion` | Agent assertion, no validation | 0.50 |
| `cass_evidence` | Imported session span | 0.45 |
| `legacy_import` | Old Eidetic Engine artifact | 0.30 (caps until validated) |

Advisory priority at retrieval time:

| Tier | Packing behavior |
|---|---|
| `blocked` | Excluded because of policy, secret risk, or prompt-injection match |
| `quarantined` | Held for curation review |
| `degraded` | Lower rank because freshness, contradiction, or evidence is weak |
| `advisory` | Low-confidence hint |
| `clear` | Normal ranking |

Lifecycle rules, advisory priority, and prompt-injection handling are specified
in [`docs/trust-model.md`](docs/trust-model.md); ADR 0009 as amended by ADR 0086
TC-D7 remains the canonical trust taxonomy.

### Prompt-injection guard

The trust pipeline flags suspicious patterns before promotion: fake
instructions, role override attempts, and exfiltration cues. Flagged memories
go into `curate candidates` and do not silently enter the procedural layer.

### Mesh sharing posture

Outbound sharing goes through policy and an authenticated canonical preview
before lane grants. Lane-consent commands use the opaque enrolled `peerId`, not
a raw Tailscale node key.

| Surface | What to use |
|---|---|
| Deterministic preview | `ee mesh preview-grant <peer-id> --lane metadata --json` |
| Explicit JSON grant | Pipe `.data.preview.approvalToken.value` from `preview-grant --issue-approval-token --json` into `ee mesh grant <peer-id> --lane metadata --preview-token-stdin --json` |
| Narrow one lane | `ee mesh revoke-lane <peer-id> --lane metadata --json` |
| Discovery consent | `ee mesh discovery-policy --explain --json` |
| Share preview | `ee share preview --peer <peer> --json` |
| Operator docs | [`docs/mesh/share_preview.md`](docs/mesh/share_preview.md), [`docs/mesh/peer_policy.md`](docs/mesh/peer_policy.md) |

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

Backups include the durable DB/JSONL source of truth, the curation audit log,
and a `manifest.json` with content hashes. By default, `ee backup create` also
includes graph-cache derived assets: graph snapshots, graph algorithm
witnesses, and graph algorithm result-cache rows. Use
`--include-graph-cache=false` for a source-only backup, and use
`ee backup restore --skip-graph-cache` when restore should leave that cache cold
and re-warm it on first use. Missing index manifests are reported as degraded.
Verification re-hashes everything included on disk.

---

## Performance

Canonical hardware class: `mac-m3-pro` (`benches/baselines/hardware_classes.toml`).
Measured on a 2024 MacBook Pro M3 against a workspace with 25 projects, 14k
memories, 8k imported CASS sessions, and about 120k indexed documents. CI and
release tooling should only update these rows with artifacts from the same
hardware class.

<!-- perf:begin hardware-class=mac-m3-pro baseline=benches/baselines/perf_v0_2.json -->
| Operation | Hardware class | p50 | p99 |
|---|---|---:|---:|
| `ee remember` (single record) | `mac-m3-pro` | 8 ms | 22 ms |
| `ee search "<q>"` (hybrid) | `mac-m3-pro` | 38 ms | 110 ms |
| `ee pack "<task>"` (markdown, 4k tokens) | `mac-m3-pro` | 95 ms | 240 ms |
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
TMPDIR=/tmp RCH_REQUIRE_REMOTE=1 rch exec -- \
  env TMPDIR=/tmp CARGO_TARGET_DIR=/Volumes/USBNVME16TB/temp_agent_space/cargo-target \
  ./scripts/bench.sh --profile ci-smoke --json

# Broader nightly profile over all benchmark groups
./scripts/bench.sh --profile nightly

# Exploratory large-machine run for 256GB+/64-core hosts
./scripts/bench.sh --profile stress

# J9 broad regression wrapper pinned to benches/baselines/perf_v0_2.json
./scripts/bench_perf_regression.sh --profile nightly --check-regression

# SRR6.46 auto-enroll performance baseline contract
EE_BENCH_BASELINE_FILE=benches/baselines/auto_enroll_perf_v0.json \
  ./scripts/bench.sh --profile auto_enroll --json --check-regression
./scripts/e2e_overhaul/auto_enroll_perf_gate.sh
```

Budgets are currently advisory while deterministic scale fixtures stabilize.
The harness emits `ee.perf.v1` JSON with profile, workload, artifact paths,
latency fields, resource fields when available, and regression status. A J10
coverage test keeps every row in the table above tied to a benchmark/baseline
or an explicit advisory marker. Profiles can become release-blocking once their
fixture variance is low enough for CI.

Performance and resource posture commands:

| Command | Use |
|---|---|
| `ee perf compare --baseline <baseline.json> --candidate <candidate.json>` | Compare normalized perf artifacts |
| `ee perf budget check --profile <name> --report <artifact.json>` | Check one artifact against a host profile |
| `ee perf explain-latency --surface search\|context --report <artifact.json>` | Explain search/context latency stages and cache posture |
| `ee diag host-profile --json` | Redacted host/resource profile inputs |
| `ee diag plan-cache --json` | EQL query plan-cache counters |
| `ee status --skyline --json` | Knowledge skyline posture when graph support is available |

### Codex RCH Workaround

Some Mac Codex sessions may still find an older `rch` on `PATH` or report the
Codex hook as not installed. Until that local installation is upgraded, keep
using the repo wrapper and pass the current RCH client as the wrapper binary:

```bash
RCH_VISIBILITY=summary \
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD \
  --summary --no-write \
  --rch-bin /Users/jemanuel/.local/bin/rch-manifestfix-20260605-5 -- \
  cargo test --locked --lib \
  search_sync_attaches_rebuilt_lexical_index_for_literal_queries -- --nocapture
```

Do not use `/Users/jemanuel/projects/remote_compilation_helper/target-local/release/rch`
directly from this Mac; that path can contain a Linux worker artifact and fail
with `exec format error`.

RCH rewrites the local USB-NVMe `CARGO_TARGET_DIR` to a worker-local target path
for remote execution, so the external-drive setting is safe for both local
artifact retrieval and remote Linux workers. `TMPDIR=/tmp` is still required:
the Mac USB scratch path is not present on Linux workers, and Rust tests using
`tempfile` inherit `TMPDIR`.

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

The schema version on disk is older than the binary expects. Run initialization
again to apply the migration path:

```bash
ee init --workspace . --json
```

Failed migrations leave clear recovery instructions in stderr and stop before a
partial apply.

### `error: workspace ambiguous (3 candidates)`

The current path resolves to multiple registered workspaces. Disambiguate it
explicitly:

```bash
ee workspace list
ee workspace alias --pick <id> --as <name>
ee --workspace <name> pack "..."
```

### `error: embed model not loaded`

The semantic stack is in degraded lexical-fallback mode because the bundled
local Model2Vec model could not be loaded or an explicit fault-injection path
was set. Default installs use the pinned `potion-multilingual-128M` model from
Frankensearch. Resolution checks `EE_EMBED_MODEL_DIR` first, then the verified
local `source_uri` of a matching available workspace model-registry row, then
the machine registry layout under `models/model2vec/`, then the legacy cache,
and only then permits a one-time download. Registry selection verifies the
pinned model identity, frozen manifest, dimension, cosine metric, and persisted
content hash. `EE_EMBED_DOWNLOAD=off` forbids that network step without
disabling a verified local model. Pre-populate either supported cache layout or
register another verified local path for an air-gapped host, then re-embed:

```bash
ee index reembed --workspace .
```

You can also keep running lexical fallback; `ee status` and `ee doctor --full`
show the degraded capability. `EE_EMBED_MODEL_PATH` is a diagnostics/fault
injection knob, not the model loader.

### `ee doctor` reports a repair plan

Start with the agent-oriented triage view, then inspect one finding:

```bash
ee doctor --robot-triage --json
ee doctor --only <failure-mode-code> --json
ee doctor --fix-plan --json
```

Use `--fix` and `--undo <RUN_ID>` only after reviewing the generated plan.

### Mesh or Tailscale is unavailable

Mesh is optional. Local memory commands can stay on `--mesh off`.

```bash
ee status --mesh off --json
ee mesh status --json
ee mesh discovery-policy --explain --json
```

For fake-tailnet and operator workflows, see
[`docs/mesh/operator_onboarding.md`](docs/mesh/operator_onboarding.md).

### Inspect command-risk memory

Ask `ee` for relevant risk history and provenance without changing whether the
command runs:

```bash
ee preflight check --cmd 'cargo test --all-targets' --json
printf '%s' "$cmd" | ee preflight check --stdin --json
```

The response is advisory. `ee` does not install a command-denial hook, return a
policy-denied process status, or require a workspace allowlist for Cargo/RCH.
For a syntactically valid check, missing or unhealthy optional memory/token
storage is reported in `degraded[]` while both `exitCode` and the process status
remain `0`.

### A crowded checkout has unknown dirty files

Use the read-only hygiene report before staging anything:

```bash
ee workspace hygiene --workspace . --json
ee swarm brief --workspace . --json
```

The report classifies generated, scratch, local-machine, review-needed, and
secret-risk paths without changing the worktree.

---

## Limitations

Boundaries to know:

| Boundary | Practical meaning |
|---|---|
| Concurrent writes | FrankenSQLite uses single-process MVCC WAL. Many agents can read at once; heavy write swarms should route through job locks or the optional daemon write owner. |
| Agent loop | `ee` stores and retrieves memory. Claude Code, Codex, or another harness still owns tools, approvals, and the prompt loop. |
| User interface | The primary interface is the CLI. Graph exports are CLI artifacts, not an interactive web app. |
| Retention model | Forgetting and decay are product features. Export JSONL into git when you need sealed long-term records. |
| Model choice | Embeddings are delegated to Frankensearch. Default installs use the pinned local `potion-multilingual-128M` fast tier; semantic quality follows that model and the derived index unless the operator explicitly changes Frankensearch posture. |
| MCP | MCP sits above the CLI. The CLI has the richest contract surface. |
| Release distribution | Multi-platform GitHub release binaries use mandatory SHA-256 verification via the `curl | bash` installer. Sigstore/provenance verification is enforced when published or explicitly required; asset coverage can vary by release. Homebrew and crates.io publication are still planned. |
| Mesh | Mesh exchanges redaction-safe rows and posture under policy. FrankenSQLite remains the local source of truth. |
| Reserved adapters | `serve` and `science-analytics` report capability gaps until their adapters mature. |
| Doctor repairs | Start with `ee doctor --fix-plan --json`; use `--fix` only after reviewing the run summary and undo path. |

---

## FAQ

**Does this replace Claude Code, Codex, or my agent harness?**
No. It is the durable memory those harnesses call. The harness owns the loop; `ee` owns memory.

**Does it phone home or call any external API?**
`ee` itself does not call paid model APIs or remote embedding services. The
default embedding path delegates to Frankensearch's local Model2Vec backend,
which may perform one pinned, verified download into the local model cache; it
runs from disk afterward. Configuring Frankensearch to use a remote model is an
explicit operator choice.

**Why no Tokio?**
The runtime is Asupersync, which gives us structured concurrency, capability narrowing, deterministic tests via `LabRuntime`, and an `Outcome` lattice. Tokio is forbidden in the dep tree, audited by CI.

**Why no `rusqlite`?**
The storage layer is FrankenSQLite via SQLModel. `rusqlite` is forbidden in the dep tree, audited by CI.

**Can I use `ee` without `cass`?**
Yes. `cass` is an evidence source, not a hard dependency. Without it, `ee remember`, `ee pack`, `ee search`, curation, graph, and packing all work normally.

**How big does the database get?**
On a typical multi-project developer machine, expect 50-500 MB after a year.
Cold/warm/hot tiering keeps the hot path small. `ee backup create` produces
portable, verified record archives.

**What happens if my index gets corrupted?**
`ee index rebuild` reproduces it from the DB. Indexes are derived assets, so
losing them is annoying but recoverable.

**Does it work on Windows?**
Yes. It is a single CLI binary, with a PowerShell installer script in the repo.
Paths follow platform conventions (`%APPDATA%`, `%LOCALAPPDATA%`).

**Can multiple agents on the same machine share one database?**
Yes. Reads are concurrent. Writes serialize through a job lock. For heavy
multi-writer swarms, run `ee daemon` and let the daemon own the write side.

**Should I use the curl installer?**
Yes — it's the recommended install. It fetches the binary for your platform
from the latest GitHub release, always verifies the checksum, verifies Sigstore
when the release includes a bundle and `cosign` is available, repairs `PATH`,
installs shell completions, and verifies the installed binary. It prints agent
integration guidance without changing agent settings. Use `--require-provenance`
for fail-closed signature and provenance verification. Build from source if you
want a local debug build or are hacking on `ee` itself.

**Should I enable mesh?**
Usually no. Mesh helps trusted peers exchange redaction-safe posture and memory
metadata, but single-machine local-first usage works with `--mesh off`.

**What should an agent run first in a crowded checkout?**
Start with `ee swarm brief --workspace . --json` and
`ee workspace hygiene --workspace . --json`, use Beads/BV to identify a
candidate, and run
`ee swarm work-packet --workspace . --include-rch --claim-gate --candidate <id> --json`
before any Beads claim mutation. Use Agent Mail for the actual reservation and
coordination workflow once the gate is safe.

**How do I inspect current command contracts?**
Use `ee --help`, `ee help <command path>`, `ee --help-json`, `ee schema list`,
and `ee capabilities --json`.

**How do I integrate with my CI?**
Run `ee pack "<the task this CI run is doing>" --json` and pipe relevant
rules into your agent's system prompt. JSON output is stable across patch
versions.

**Does `ee` ever rewrite my memories silently?**
No. The steward proposes; you approve. Promotions, consolidations,
replacements, and tombstones each produce recorded entries visible via
`ee why <id>` and the curation queue commands.

**Where do I see the architectural decisions?**
[`docs/adr/`](docs/adr/). Every major subsystem has an ADR with rejected alternatives and verification hooks.

---

## Documentation

| Doc | Purpose |
|---|---|
| [`CHANGELOG.md`](CHANGELOG.md) | Reconstructed release history and current release posture |
| [`CHANGELOG_RESEARCH.md`](CHANGELOG_RESEARCH.md) | Evidence ledger behind the changelog reconstruction |
| [`docs/query-schema.md`](docs/query-schema.md) | EQL-inspired request schema for `ee pack` |
| [`docs/trust-model.md`](docs/trust-model.md) | Memory advisory priority, trust classes, prompt-injection defenses |
| [`docs/agent-outcome-scenarios.md`](docs/agent-outcome-scenarios.md) | North-star agent journey matrix and acceptance scenarios |
| [`docs/agent-ux/insights-onboarding.md`](docs/agent-ux/insights-onboarding.md) | Agent workflow for graph-derived insights, Pack DNA, skyline, and proximity surfaces |
| [`docs/agent-ux/auto_enrollment_onboarding.md`](docs/agent-ux/auto_enrollment_onboarding.md) | Agent workflow and use/no-use checklist for optional Tailscale mesh, auto-enrollment, drift handling, and safety previews |
| [`docs/agent-ux/ee-doctor-first-aid-precedence.md`](docs/agent-ux/ee-doctor-first-aid-precedence.md) | Doctor-first repair workflow for agents |
| [`docs/agent-ux/memory-hygiene.md`](docs/agent-ux/memory-hygiene.md) | Weekly content-health workflow for curate doctor, learn gaps, and debt trends |
| [`docs/agent-ux/journal-capture.md`](docs/agent-ux/journal-capture.md) | Append-only journal capture, end-of-session distillation, reinforcement, and pack-item grading workflow |
| [`docs/agent-ux/flight-recorder.md`](docs/agent-ux/flight-recorder.md) | Redacted workload flight-recorder operator and agent reference |
| [`docs/agent-ux/workspace-hygiene.md`](docs/agent-ux/workspace-hygiene.md) | Dirty-checkout and commit-readiness workflow |
| [`docs/mesh/operator_onboarding.md`](docs/mesh/operator_onboarding.md) | Operator guide for optional mesh usage, trust/redaction posture, revision tokens, and troubleshooting |
| [`docs/mesh/command_modes.md`](docs/mesh/command_modes.md) | Optional mesh command modes and degraded behavior |
| [`docs/mesh/anti_entropy.md`](docs/mesh/anti_entropy.md) | Mesh anti-entropy workflow |
| [`docs/mesh/peer_policy.md`](docs/mesh/peer_policy.md) | Mesh peer-policy and lane semantics |
| [`docs/cli-reference/graph-flags.md`](docs/cli-reference/graph-flags.md) | Aggregated graph-related CLI flags by command, including implemented and pending surfaces |
| [`docs/configuration/graph.md`](docs/configuration/graph.md) | Graph feature flags, thresholds, and tuning guidance |
| [`docs/configuration/cache.md`](docs/configuration/cache.md) | Pack and query cache configuration |
| [`docs/configuration/storage.md`](docs/configuration/storage.md) | Read pool, snapshot pin, and storage configuration |
| [`docs/architecture/graph-snapshots.md`](docs/architecture/graph-snapshots.md) | Graph snapshot families, lifecycle, locks, budgets, and degraded behavior |
| [`docs/architecture/shard-fanout.md`](docs/architecture/shard-fanout.md) | Shard-fanout architecture and migration posture |
| [`docs/search/plan-cache.md`](docs/search/plan-cache.md) | EQL plan-cache behavior and diagnostics |
| [`docs/env_vars.md`](docs/env_vars.md) | Complete `EE_*` environment variable registry |
| [`docs/feature_flag_registry.md`](docs/feature_flag_registry.md) | Cargo feature flag status and owner tracking |
| [`docs/degraded_code_taxonomy.md`](docs/degraded_code_taxonomy.md) | Degraded-code classification and severity vocabulary |
| [`docs/dependency-contract-matrix.md`](docs/dependency-contract-matrix.md) | Franken-stack integration contracts and version pins |
| [`docs/testing-strategy.md`](docs/testing-strategy.md) | Test categories, verification gates, golden test structure |
| [`docs/command_classification.md`](docs/command_classification.md) | Command effect taxonomy and read/write classification |
| [`docs/migration-guide.md`](docs/migration-guide.md) | DB schema migrations and upgrade paths |
| [`docs/toon-output.md`](docs/toon-output.md) | TOON (Text-Only Object Notation) output format |
| [`docs/pack-replay.md`](docs/pack-replay.md) | Pack replay, support-bundle safety, pack-quality operator guidance, and fixture authoring |
| [`docs/agent-ux/regression-causality.md`](docs/agent-ux/regression-causality.md) | Regression-causality capsule workflow, redaction rules, and failed-gate operator examples |
| [`docs/adr/0025-replayable-context-pack-selection-ledgers.md`](docs/adr/0025-replayable-context-pack-selection-ledgers.md) | Pack replay/diff ledger contract, freshness states, and support-bundle safety rules |
| [`docs/adr/0038-auto-enrollment-zero-touch.md`](docs/adr/0038-auto-enrollment-zero-touch.md) | Optional zero-touch Tailscale mesh auto-enrollment design, invariants, and rejected alternatives |
| [`docs/adr/`](docs/adr/) | Architectural decision records |

---

## About Contributions

*About Contributions:* Please don't take this the wrong way, but I do not accept outside contributions for any of my projects. I simply don't have the mental bandwidth to review anything, and it's my name on the thing, so I'm responsible for any problems it causes; thus, the risk-reward is highly asymmetric from my perspective. I'd also have to worry about other "stakeholders," which seems unwise for tools I mostly make for myself for free. Feel free to submit issues, and even PRs if you want to illustrate a proposed fix, but know I won't merge them directly. Instead, I'll have Claude or Codex review submissions via `gh` and independently decide whether and how to address them. Bug reports in particular are welcome. Sorry if this offends, but I want to avoid wasted time and hurt feelings. I understand this isn't in sync with the prevailing open-source ethos that seeks community contributions, but it's the only way I can move at this velocity and keep my sanity.

---

## License

MIT License (with OpenAI/Anthropic Rider). See [`LICENSE`](LICENSE).

© 2026 Jeffrey Emanuel
