<div align="center">

<img src="./ee_illustration.webp" alt="Eidetic Engine illustration" width="720">

# Eidetic Engine (`ee`)

**Durable, local-first, explainable memory for coding agents.**

[![CI](https://img.shields.io/github/actions/workflow/status/Dicklesworthstone/eidetic_engine_cli/ci.yml?branch=main&label=CI)](https://github.com/Dicklesworthstone/eidetic_engine_cli/actions)
[![crates.io planned](https://img.shields.io/badge/crates.io-planned-lightgrey.svg)](#installation-status)
[![License: MIT+Rider](https://img.shields.io/badge/License-MIT%2BOpenAI%2FAnthropic%20Rider-yellow.svg)](./LICENSE)
[![Rust 2024](https://img.shields.io/badge/rust-2024-orange.svg)](rust-toolchain.toml)
[![No Tokio](https://img.shields.io/badge/runtime-Asupersync-blueviolet.svg)](#hard-requirements)

**Install**

```bash
curl -fsSL https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/latest/download/install.sh | bash
```

Verifies checksums, drops the `ee` binary into `~/.local/bin`, installs shell
completions, and auto-configures the Claude Code / Codex / Gemini agent hooks
if those harnesses are detected. Pass `--help` (e.g. `bash install.sh --help`)
for offline tarballs, proxy options, `--no-gum`, and `--force` reinstall.

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

The command returns a Markdown pack with project release rules, prior release
incidents from `cass`, verification commands, branch traps, and high-severity
warnings. Each item carries an evidence pointer and a score breakdown.

> `ee pack "<task>"` is the canonical context-pack command after the triad
> promotion. `ee context "<task>"` runs the same code path and is retained as a
> soft-deprecated alias that emits `deprecated_alias` (severity `info`) in its
> `degraded[]`. Examples elsewhere in this README that still use `ee context`
> remain valid during the deprecation window; new harnesses and scripts should
> prefer `ee pack`. See `docs/triad_compat_plan.md` for the disposition table.

### What You Get

| Capability | What you get |
|---|---|
| **Hybrid retrieval** | BM25 + vector search via Frankensearch's `TwoTierSearcher`, with deterministic ranking and fusion; local RRF-shaped data is diagnostic-only and never owns final ordering |
| **Explainable scores** | Every returned memory shows component scores, freshness, confidence, and which sources support it |
| **Procedural rules with decay** | Confidence ages out, harmful feedback demotes faster than helpful feedback promotes |
| **Anti-patterns first-class** | Trauma-guard surfaces high-severity risk memories before destructive actions |
| **Graph-aware** | PageRank, HITS, PPR, Gomory-Hu proximity, dominance, causal paths, structural health, Pack DNA, and skyline views |
| **CASS session import** | Mines your existing `cass` corpus (Claude Code, Codex, Cursor, Gemini, ChatGPT) for evidence |
| **Context profiles** | `compact`, `balanced`, `grounding`, `orientation`, `thorough`, and `submodular` quota/objective mixes |
| **Local-first** | No cloud. No paid LLM APIs required. Embeddings run locally through Frankensearch |
| **Stable JSON contract** | Every machine-facing command emits versioned JSON with `schema` field for parsing and validation |
| **Deterministic** | Same DB + indexes + config + query → identical pack hash |
| **Cancellation-aware core** | Runtime-facing async APIs use Asupersync `&Cx` and `Outcome`; cancellable storage retry loops remain tracked by `bd-37r5a` |
| **CLI first, daemon optional** | Every essential workflow runs as a one-shot. No background process required |
| **Auditable curation** | Promotions, consolidations, and tombstones produce audit entries; no silent rewrites |
| **Crowded-agent posture** | Swarm brief, workspace hygiene, verification broker, QoS lanes, and flight recorder help agents coordinate without taking over the loop |

### Current State Snapshot

| Area | Current status |
|---|---|
| Latest tag | `v0.1.0` git tag exists |
| GitHub Releases | No published release assets yet |
| crates.io | Package name selected as `eidetic-engine`; `publish = false` in current `Cargo.toml` |
| Live install path | Source build with Cargo |
| Default feature set | `fts5`, `json`, `embed-fast`, `lexical-bm25`, `graph` |
| Optional adapters | MCP adapter is feature-gated; `serve` and `science-analytics` remain reserved/degraded |
| Mesh | Optional and off by default; foreground CLI, Tailscale probe/autodiscovery, policy, hello, and sync surfaces exist |
| Verification | `scripts/verify.sh` is the central gate runner; heavy Cargo work in agent sessions should go through RCH |

### Agent Operating Loop

For agent use, the core rhythm is small and repetitive:

```bash
ee swarm brief --workspace . --json
ee context "<task>" --workspace . --max-tokens 4000 --format markdown
ee search "<specific question>" --workspace . --limit 20 --explain --json
ee why <memory-id> --workspace . --json
ee preflight check --cmd "<risky shell command>" --workspace . --json
ee remember "<durable lesson>" --workspace . --level procedural --kind rule --json
ee outcome <memory-id> --workspace . --signal helpful --reason "<what it changed>"
```

| Situation | First `ee` command |
|---|---|
| Starting substantive work | `ee context "<task>" --workspace . --max-tokens 4000 --format markdown` |
| Joining a crowded checkout | `ee swarm brief --workspace . --json` |
| Learning a durable rule | `ee remember "<text>" --workspace . --level procedural --kind rule --json` |
| A memory helped or misled you | `ee outcome <id> --signal helpful\|harmful --reason "<one sentence>"` |
| A high-ranked memory looks suspicious | `ee why <id> --workspace . --json` |
| A context pack looks odd | `ee context "<task>" --workspace . --explain --json` |
| About to run a destructive command | `ee preflight check --cmd "<exact command>" --workspace . --json` |
| You need a safe handoff | `ee handoff create --workspace . --out <capsule.json> --json` |
| You need a support artifact | `ee support bundle --out <dir> --workspace . --json` |

### Operator rules in practice

The private operator playbook boils down to a few habits:

| ID | Rule | Command habit |
|---|---|---|
| EE-001 | Pull task context before real work | `ee context "<task>" --workspace . --max-tokens 4000 --format markdown` |
| EE-002 / EE-003 | Choose memory level and kind by what the fact is | Use `working`, `episodic`, `semantic`, `procedural`; use `rule`, `fact`, `decision`, `failure`, `command`, `convention`, `anti-pattern`, `risk`, or `playbook-step` |
| EE-004 / EE-025 | Inspect surprising results before discarding them | `ee context "<task>" --explain --json`; `ee why <id> --workspace . --json` |
| EE-005 / EE-017 | Treat pack replay as forensics | `ee pack replay <pack-id> --json`; rerun live `ee context` when evidence freshness changed |
| EE-009 / EE-011 | Save memory IDs and close the feedback loop promptly | `ee outcome <id> --signal helpful\|harmful --reason "<one sentence>"` |
| EE-010 / EE-030 | Run preflight per risky command | `ee preflight check --cmd "<exact command>" --workspace . --json` |
| EE-013 / EE-021 | In crowded repos, coordinate before staging | `ee swarm brief --workspace . --json`; `ee workspace hygiene --workspace . --json` |
| EE-019 | Use TOON for tight prompt budgets and JSON for parsers | `ee context "<task>" --format toon`; `ee context "<task>" --json` |
| EE-020 / EE-026 | Use the right sharing artifact | `ee support bundle` for bug reports; `ee handoff create` for resumable work |

---

## Quick Example

A typical session:

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

The flow runs locally with no daemon and no cloud. On a typical project, the
interactive steps are fast enough to use before ordinary agent work.

---

## Design Philosophy

> `ee` is the durable memory layer your agent harness calls. The harness still
> owns tools, approvals, and the prompt loop.

The code and tests back these contracts where they can.

### 1. Local First

All primary data lives on your machine. No cloud dependency is required. Remote APIs and model downloads are explicit opt-in. Frankensearch handles embedding so `ee` never decides which model you run.

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

Mechanized proof artifacts now live alongside the test suite: [`proofs/lean4/pack_determinism.lean`](proofs/lean4/pack_determinism.lean) models the pack-hash determinism invariant, and [`proofs/tla/agent_mail_coordination.tla`](proofs/tla/agent_mail_coordination.tla) models exclusive Agent Mail reservation safety. The proof-check report schema is registered as `ee.proof_check.v1`; `ee verify proofs` and the non-blocking `verify.sh` stage are tracked under `bd-nnfq4`.

### 5. Explainable Retrieval

Every returned memory answers six questions:

- **Why selected?** Score components per stage.
- **What supports it?** Provenance URI(s).
- **How fresh?** Recency decay term.
- **How reliable?** Confidence, evidence count, harmful-feedback weight.
- **What scores mattered?** Component breakdown.
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

### Installation status

`ee` is still pre-release. The GitHub release, Homebrew tap, and crates.io
install paths below are planned channels, not live distribution channels yet.
Current release posture:

| Path | Status | Provenance | Tracking |
|---|---|---|---|
| Git tag | `v0.1.0` exists | n/a | [`CHANGELOG.md`](CHANGELOG.md) |
| GitHub release installer | planned; no release assets published yet | SLSA provenance planned; installer supports `--require-provenance` | `bd-3usjw.9` / `bd-3usjw.9.1` |
| Homebrew tap | planned; tap formula not published yet | release-asset provenance applies after tap publish | `bd-3usjw.13` |
| crates.io | planned; package name selected as `eidetic-engine`; binary remains `ee`; `publish = false` today | n/a | `bd-3usjw.10` |
| Source build | available now | local build only | this README |

### Release installer (planned)

Planned after the first signed GitHub release ships; see `bd-3usjw.9`.

```bash
curl -fsSL https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/download/v0.1.0/install.sh \
  | EE_VERSION=v0.1.0 bash
```

This will verify the binary against the published Sigstore bundle, drop it in
`~/.local/bin/ee`, and run `ee doctor` to confirm. Pass
`--require-provenance` when invoking `install.sh` to also require the
SLSA provenance JSON and its Sigstore bundle.

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
| 6.5 | Overhaul integration when `VERIFY_OVERHAUL` is enabled |
| 6.6 | Fake Tailscale harness |
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
| Core memory loop | `init`, `remember`, `search`, `context`, `why`, `status`, `doctor`, `capabilities`, `check`, `health` |
| Memory lifecycle | `memory`, `rule`, `curate`, `review`, `playbook`, `procedure`, `workflow`, `outcome`, `outcome-quarantine` |
| Packing and retrieval | `pack`, `context-show`, `show`, `link`, `tag`, `history`, `proximity`, `insights`, `subscribe` |
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
| `ee pack "<task>" [--profile <p>] [--max-tokens N] [--format <fmt>]` | Assemble a task-specific context pack (the headline command, post-triad-promotion canonical) |
| `ee context "<task>" [--profile <p>] [--max-tokens N] [--format <fmt>]` | Soft-deprecated alias of `ee pack`; emits `deprecated_alias` (severity `info`) and runs the same code path |
| `ee search "<query>" [--limit N] [--explain] [--json]` | Hybrid retrieval over memories, sessions, rules, evidence |
| `ee remember "<text>" --level <l> [--kind <k>] [--tags a,b]` | Capture a durable memory |
| `ee outcome <id> --signal helpful\|harmful [--reason "<reason>"]` | Record feedback, updating utility/confidence |
| `ee why <memory-id> [--json]` | Explain why a memory was selected, scored, or curated the way it was |
| `ee pack build --query-file task.eeq.json --max-tokens N --format toon` | Build a pack from an explicit EQL query document |
| `ee pack replay <pack-id> --json` | Inspect the persisted, redaction-safe selection ledger for a historical pack |
| `ee pack diff <old-pack-id> <new-pack-id> --json` | Compare two persisted pack ledgers and explain selection, freshness, redaction, or derived-asset changes |
| `ee support bundle --out <dir> --json` | Create a redacted diagnostic bundle, including pack replay and swarm-brief summaries without raw query, mail body, memory, or full file-listing content |
| `ee preflight check --cmd "<shell command>" --json` | Check shell commands against the policy/trauma guard before risky operations |
| `ee verify proofs --json` | Check committed Lean4 and TLA+ proof artifacts |

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
| Pack identity | `data.pack.hash` for batch packs; `packHash` on stream trailer frames |
| Graph explanation | `data.pack.packDna` when `ee pack --explain --json` (or its alias `ee context --explain --json`) is used |
| Feature gaps | `ee capabilities --json` at `data.unimplemented[]`, not command `degraded[]` |
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

Common red flags:

| Signal | First response |
|---|---|
| `data.posture.overall = "blocked"` | Run `ee doctor --json` and follow the failing check repair |
| `data.posture.overall = "degraded_required"` | Read `degraded[]` and `error.details.recovery[]` |
| `search_index_stale` | `ee index rebuild --workspace .` |
| `embed_model_unavailable` | Continue lexical-only or run `ee index reembed --workspace .` |
| `graph_snapshot_stale` | Continue retrieval, then refresh graph snapshots when graph scores matter |
| `pack_budget_too_small` | Raise `--max-tokens` or switch to `--profile compact` |
| `data.workspace.diagnostics[].severity = "warning"` | Workspace selection conflict; use `ee workspace list`, then pass an explicit workspace or alias |
| exit `7` | Stop and get human approval before using any bypass-token path |
| exit `8` | Run `ee migrate run --workspace . --json` |

### Context pack controls

`ee context` and `ee pack build` expose three layers of control:

| Layer | Flags | Use |
|---|---|---|
| Retrieval profile | `--profile compact\|balanced\|grounding\|orientation\|thorough\|submodular` | Choose the memory mix and graph bias |
| Output profile | `--pack-profile lean\|standard\|verbose` | Trim or expand JSON metadata |
| Resource profile | `--resource-profile lean\|standard\|swarm_heavy` | Pick pack assembly SLO posture |
| Size | `--max-tokens N`, `--candidate-pool N` | Bound prompt budget and candidate pool |
| Output format | `--format markdown\|json\|toon`, `--stream --json` | Prompt text, parser output, token-tight TOON, or NDJSON frames |
| JSON diet | `--no-rendered-text`, `--no-skipped`, `--no-meta`, `--no-pack-dna` | Suppress bulky sections for structured consumers |
| Coordination | `--coordination-snapshot <path>`, `--coordination-stale-after-ms N` | Embed a redacted coordination snapshot |
| Code-change hints | `--changed-symbol <selector>`, `--changed-symbols-from-git` | Bias toward memories linked to changed symbols |
| Time windows | `--as-of <RFC3339>`, `--include-expired`, `--include-future`, `--include-stale`, `--include-tombstoned` | Inspect validity-window behavior |
| Trust lane | `--memory-scope self\|team\|workspace\|verified\|swarm`, `--strict-scope` | Bound which trust lane can contribute |
| Privacy | `--redaction none\|minimal\|standard\|strict\|paranoid` | Tune output redaction where the command allows it |

Examples:

```bash
ee context "debug release failure" \
  --workspace . \
  --profile thorough \
  --pack-profile verbose \
  --resource-profile swarm_heavy \
  --max-tokens 8000 \
  --explain \
  --json

ee context "small hook context" \
  --workspace . \
  --profile compact \
  --pack-profile lean \
  --max-tokens 1200 \
  --format toon

ee context "large agent handoff" \
  --workspace . \
  --stream \
  --format jsonl
```

When `[pack] adaptive_budget = true`, omitted `--max-tokens` lets `ee` compute
a budget from retrieval entropy, graph fanout, and task keywords. Passing
`--max-tokens N` pins the budget for prompt caches, eval fixtures, CI gates, or
multi-pack composition.

When `[pack] memory_tier_admission = true`, `ee context` treats hot/warm/cold
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

For bug reports and handoffs, attach
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
listings. Treat it as diagnostic context. Before claiming work or coordinating
edits, run a fresh `ee swarm brief`.

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

The brief sits beside the existing tools. `br ready --json` remains the source
of ready-work records, and `bv --robot-triage` remains the graph-aware ranking
engine. Agent Mail remains the authority for reservations and coordination
messages. Handoff capsules and support bundles carry diagnostic snapshots such
as `swarm_brief_summary.json`, but a live brief is still the preflight before
new claims. Profile reports and performance forensics diagnose host behavior in
detail; the brief only carries enough posture to steer choices such as routing
Cargo through RCH.

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
```

The report can include Agent Mail reservations and Beads links, so an agent can
see whether a path is risky because of content, ownership, or current work
coordination.

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

Every schema carries an `x-ee-status` marker. Agents should treat
`"shipped": false` as documentation for a future surface, not runtime
availability. The schema catalog does not turn `ee` into a scheduler, web
service, mail sender, Beads mutator, or agent loop.

### Mesh and Tailscale

Mesh is optional. Local-first operation is the default. Use mesh when a trusted
tailnet or local file-exchange path is part of the agent workflow.

| Command | Purpose |
|---|---|
| `ee mesh init --json` | Inspect foreground mesh readiness without starting a daemon |
| `ee mesh status --json` | Report local mesh posture, cache counts, and repair commands |
| `ee mesh peers --json` | List configured peers and anti-entropy cursors |
| `ee mesh peer add\|list\|show\|rotate\|revoke\|unknown-attempt` | Manage app-level mesh peer records after explicit consent |
| `ee mesh auto-enroll --json` | Materialize Tailscale-discovered peers from fresh autodiscovery |
| `ee mesh discovery-policy [set\|allow\|deny] --json` | Inspect or update caller/responder discovery policy |
| `ee mesh hello-responder status --json` | Inspect the local hello responder lifecycle job |
| `ee mesh preview-grant <nodekey> --lane <lane> --json` | Preview lane grants without mutating policy |
| `ee mesh export --out <file> --json` | Write a redaction-safe foreground mesh artifact |
| `ee mesh import --file <file> --json` | Import a foreground mesh artifact from a local file |
| `ee mesh sync --once --json` | Run one foreground sync cycle |

Mesh command mode can be selected per command or through `EE_MESH_MODE`:

```bash
ee search "release proof" --workspace . --mesh off --json
ee context "handoff this bead" --workspace . --mesh cache --json
ee status --workspace . --mesh revisable --json
ee mesh discovery-policy --explain --json
```

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
| `ee insights [--section <name>] [--explain <id>] --json` | Inspect graph-derived findings and memory-centric topology |
| `ee proximity <memory-a> <memory-b> --json` | Explain Gomory-Hu min-cut proximity between two memory nodes |

### Index

| Command | Purpose |
|---|---|
| `ee index status` / `rebuild` / `reembed` | Manage derived search indexes (Frankensearch owns model selection) |
| `ee index vacuum` | Preview reclaimable derived search-index artifacts without deleting or rewriting files |

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
| `ee preflight run "<task>"` / `show` / `close` | Task risk assessment, tripwire context, and post-run feedback |
| `ee preflight check --cmd "<command>" --json` | Command-facing policy guard for shell hooks |
| `ee tripwire list` / `check` | Inspect and check preflight tripwires |
| `ee diag plan-cache` | EQL query plan-cache counters and integration posture |
| `ee diag disk-pressure` / `build-admission` / `artifacts` | Storage, artifact, and build-admission diagnostics |
| `ee diag graph` / `graph-snapshot` / `search` | Graph, snapshot, and retrieval diagnostics |
| `ee diag integrity` / `dependencies` / `streams` | Integrity, dependency, and stdout/stderr stream checks |
| `ee verify ingest` / `ee verify rch ingest` / `ee verify rch blockers` / `ee verify rch runs` / `proofs` / `broker lookup` / `closure-guidance` | Verification evidence, durable RCH proof ledger queries, proof checks, reusable RCH evidence, and closeout guidance |
| `ee maintenance run` / `status` / `wal-checkpoint` / `graph-snapshot-prune` / `graph-witnesses-prune` | Explicit maintenance jobs and retention helpers |
| `ee job run` / `list` / `show` | Durable steward job history and explicit job execution |
| `ee install check` / `plan` and `ee update` | Agent-safe install/update checks and dry-run plans |
| `ee eval run` / `list` | Run or list retrieval-quality evaluation fixtures |
| `ee eval report [fixture]` | Summarize fixture IDs, data hashes, aggregate retrieval metrics, and the first failing query |
| `ee eval run <fixture> --pack-quality --json` | Check whether deterministic fixtures still select required context-pack evidence |
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
query_plan_cache_entries = 1024

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
mode = "off"                          # off | cache | revisable | blocking

[mesh.tailscale]
discovery_mode = "service_tag"         # service_tag | auto_admit | allowlist
respond_mode   = "service_tag"
```

Environment variable overrides:

| Variable | Equivalent |
|---|---|
| `EE_DATABASE_PATH` | `[storage].database_path` |
| `EE_INDEX_DIR`     | `[storage].index_dir` |
| `EE_PROFILE`       | `[pack].default_profile` |
| `EE_MAX_TOKENS`    | `[pack].default_max_tokens` |
| `EE_AGENT_NAME` | agent identity for outcome attribution and per-agent bias |
| `EE_SECURITY_PROFILE` | preflight policy posture |
| `EE_HARMFUL_PER_SOURCE_PER_HOUR` | `[feedback].harmful_per_source_per_hour` |
| `EE_HARMFUL_BURST_WINDOW_SECONDS` | `[feedback].harmful_burst_window_seconds` |
| `EE_QUERY_PLAN_CACHE_ENTRIES` | query-plan cache size |
| `EE_PPR_CACHE_ENTRIES` | PPR prefetch cache size |
| `EE_L2_PACK_CACHE_BYTES` / `EE_L2_PACK_CACHE_DIR` / `EE_L2_PACK_CACHE_DISABLE` | pack L2 cache controls |
| `EE_READ_POOL_SIZE` / `EE_READ_POOL_ACQUIRE_TIMEOUT_MS` / `EE_READ_POOL_MAX_PIN_SECONDS` | read-pool controls |
| `EE_GRAPH_MEMORY_SNAPSHOT_CAP_MB` / `EE_GRAPH_MEMORY_PER_ALGORITHM_CAP_MB` | graph working-set admission controls |
| `EE_MESH_ENABLED` / `EE_MESH_MODE` | optional mesh default posture |
| `EE_TAILSCALE_DISCOVERY_MODE` / `EE_TAILSCALE_RESPOND_MODE` | Tailscale discovery and responder policy |
| `EE_TAILSCALE_PEER_PROBE_TIMEOUT_MS` / `EE_TAILSCALE_DISCOVERY_BUDGET_MS` | Tailscale peer-discovery budgets |
| `EE_FLIGHT_RECORDER` / `EE_FLIGHT_RECORDER_DIR` / `EE_FLIGHT_RECORDER_RETENTION_DAYS` | flight-recorder controls; see [`docs/agent-ux/flight-recorder.md`](docs/agent-ux/flight-recorder.md) |
| `EE_WORKSPACE_HYGIENE_SECRET_PATTERNS` / `EE_WORKSPACE_HYGIENE_GENERATED_PATTERNS` / `EE_WORKSPACE_HYGIENE_SCRATCH_PATTERNS` / `EE_WORKSPACE_HYGIENE_IGNORE_PATTERNS` | workspace hygiene classifier overlays |
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
| `embed-fast` | active | Frankensearch `model2vec` semantic embedder |
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

**Native Asupersync.** Runtime-facing async APIs take `&Cx`, return `Outcome<T>`, and preserve budget/cancellation semantics where wired. Contended storage retry sleeps are not yet universally cancellable; `bd-37r5a` tracks the implementation and LabRuntime/e2e proof needed before restoring that broader claim.

Additional runtime-adjacent modules:

| Module | Role |
|---|---|
| `mesh` | Optional peer exchange, Tailscale autodiscovery, hello responder, anti-entropy, policy, and lane preview |
| `obs` | Flight recorder, structured tracing, posture helpers, and diagnostic evidence |
| `hooks` | Agent harness hook helpers, including preflight shell snippets |
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
├── mesh/                   # optional mesh cache and peer metadata
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
| `grounding` | Uses balanced quotas and boosts HITS authority evidence when graph scores are available |
| `orientation` | Uses balanced quotas and boosts HITS hub memories when graph scores are available |
| `thorough` | Expands evidence and artifact coverage for higher-recall work |
| `submodular` | Uses the facility-location objective with thorough section quotas for deterministic diversity |

Output formats:

| Format | Use |
|---|---|
| `markdown` | Prompt-prepend text for agents and humans |
| `json` | Full structured contract for parsers |
| `toon` | Token-tight prompt material with stable field order |
| `jsonl` with `--stream` | Incremental `ee.pack.stream.v1` frames |

Stream consumers should read until a terminal frame. `kind: "cancelled"` can
still carry emitted items; `kind: "error"` is the hard failure path.

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

For privileged domains, isolate by workspace:

```bash
ee init --workspace ./matters/smith-v-jones --json
ee init --workspace ./deals/2026-q3-acme --json
ee init --workspace ./positions/AAPL-long --json
```

`cass` is specific to coding sessions. For other domains, use direct `ee remember` calls or
structured imports through `ee import jsonl --source <file>`.

## Negative Evidence Ledger

For long-running optimization work, record failed attempts before they disappear
into a revert. The useful artifact is the attempt, why it lost, and the smallest
measurement or source that proves it lost.

| Loop step | `ee` surface |
|---|---|
| Start a campaign | `ee init --workspace ./optimization/<campaign> --json` |
| Capture a failed attempt | `ee remember "...what lost and why..." --level episodic --kind failure --tags family-...,cause-...,regression-... --source <artifact-uri> --json` |
| Cluster repeated failures | `ee playbook extract --workspace ./optimization/<campaign> --dry-run --json` |
| Promote a validated anti-pattern | `ee curate validate <candidate-id>` then `ee curate apply <candidate-id>` |
| Prime the next attempt | `ee context "<next hypothesis>" --workspace ./optimization/<campaign> --profile thorough --format markdown` |

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
| `family-<name>` | Approach family, such as `family-aggressive-prefetch` |
| `regression-<surface>` | Where it lost, such as `regression-tail-latency` |
| `cause-<root>` | Inferred root cause, such as `cause-cache-pollution` |
| `reverted-at-<sha>` | Decision point or revert commit |

---

## Agent Harness Integration

### Claude Code

Add to your `AGENTS.md` or hook setup:

```text
Before starting substantial work, run:
  ee swarm brief --workspace . --json
  ee context "<task>" --workspace . --max-tokens 4000 --format markdown

When you discover a durable project convention:
  ee remember --workspace . --level procedural --kind rule "<rule>"

Before risky shell commands:
  ee preflight check --cmd "<shell-command>" --workspace . --json

After a remembered rule helps or harms:
  ee outcome <id> --signal helpful
  ee outcome <id> --signal harmful
```

You can also wire it into a PreToolUse hook that injects context before risky
commands. The `ee context` JSON is stable and parseable.

### Codex

Codex shells out, so the same calls work. `ee context "<task>" --json` can be
inserted directly into a system or developer message.

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
in [`docs/trust-model.md`](docs/trust-model.md); ADR 0009 remains the canonical
trust taxonomy.

### Prompt-injection guard

The trust pipeline flags suspicious patterns before promotion: fake
instructions, role override attempts, and exfiltration cues. Flagged memories
go into `curate candidates` and do not silently enter the procedural layer.

### Mesh sharing posture

Outbound sharing goes through policy and preview paths before lane grants.

| Surface | What to use |
|---|---|
| Preview lane access | `ee mesh preview-grant <nodekey> --lane metadata --json` |
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
Codex hook as not installed. Until that local installation is upgraded, invoke
the current RCH client by absolute path and fail closed to remote execution:

```bash
RCH_REQUIRE_REMOTE=1 \
TMPDIR=/tmp \
RCH_VISIBILITY=summary \
RCH_CANONICAL_PROJECT_ROOT=/Users/jemanuel/projects \
RCH_ALIAS_PROJECT_ROOT=/data/projects \
/Users/jemanuel/projects/remote_compilation_helper/target-local/release/rch exec -- \
  env TMPDIR=/tmp CARGO_TARGET_DIR=/Volumes/USBNVME16TB/temp_agent_space/cargo-target \
  cargo test --lib search_sync_attaches_rebuilt_lexical_index_for_literal_queries -- --nocapture
```

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
ee --workspace <name> context "..."
```

### `error: embed model not loaded`

The semantic stack is in degraded lexical-only mode. Frankensearch owns model
selection; configure it there, then re-embed:

```bash
ee index reembed --workspace .
```

You can also keep running lexical-only; `ee status` shows the degraded capability.

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

### Preflight blocks a shell command

Inspect the policy result and follow the repair text:

```bash
ee preflight check --cmd 'cargo test --all-targets' --json
ee preflight issue-bypass-token --reason "human approved exact command" --json
```

Bypass tokens are one-shot recorded artifacts. They are for explicit human
approval, not automated retries.

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
| Model choice | Embeddings are delegated to Frankensearch. Semantic quality follows the model and index Frankensearch is configured to use. |
| MCP | MCP sits above the CLI. The CLI has the richest contract surface. |
| Release distribution | Source builds are live. GitHub Release assets, Homebrew, and crates.io publication are planned. |
| Mesh | Mesh exchanges redaction-safe rows and posture under policy. FrankenSQLite remains the local source of truth. |
| Reserved adapters | `serve` and `science-analytics` report capability gaps until their adapters mature. |
| Doctor repairs | Start with `ee doctor --fix-plan --json`; use `--fix` only after reviewing the run summary and undo path. |

---

## FAQ

**Does this replace Claude Code, Codex, or my agent harness?**
No. It is the durable memory those harnesses call. The harness owns the loop; `ee` owns memory.

**Does it phone home or call any external API?**
`ee` itself makes no network calls. Embedding is delegated to Frankensearch,
which runs locally by default. Configuring Frankensearch to use a remote model
is an explicit operator choice.

**Why no Tokio?**
The runtime is Asupersync, which gives us structured concurrency, capability narrowing, deterministic tests via `LabRuntime`, and an `Outcome` lattice. Tokio is forbidden in the dep tree, audited by CI.

**Why no `rusqlite`?**
The storage layer is FrankenSQLite via SQLModel. `rusqlite` is forbidden in the dep tree, audited by CI.

**Can I use `ee` without `cass`?**
Yes. `cass` is an evidence source, not a hard dependency. Without it, `ee remember`, `ee context`, `ee search`, curation, graph, and packing all work normally.

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

**Should I use the curl installer today?**
Not yet. The curl installer is documented as the planned release path. Build
from source until release assets are published.

**Should I enable mesh?**
Usually no. Mesh helps trusted peers exchange redaction-safe posture and memory
metadata, but single-machine local-first usage works with `--mesh off`.

**What should an agent run first in a crowded checkout?**
Start with `ee swarm brief --workspace . --json` and
`ee workspace hygiene --workspace . --json`, then use Agent Mail or Beads for
the actual claim/reservation workflow.

**How do I inspect current command contracts?**
Use `ee --help`, `ee help <command path>`, `ee --help-json`, `ee schema list`,
and `ee capabilities --json`.

**How do I integrate with my CI?**
Run `ee context "<the task this CI run is doing>" --json` and pipe relevant
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
