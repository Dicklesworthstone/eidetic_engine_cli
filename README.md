<div align="center">

<img src="./ee_illustration.webp" alt="Eidetic Engine illustration" width="720">

# Eidetic Engine (`ee`)

**Durable, local-first, explainable memory for coding agents.**

[![License: MIT+Rider](https://img.shields.io/badge/License-MIT%2BOpenAI%2FAnthropic%20Rider-yellow.svg)](./LICENSE)
[![Rust 2024](https://img.shields.io/badge/rust-2024-orange.svg)](rust-toolchain.toml)
[![No Tokio](https://img.shields.io/badge/runtime-Asupersync-blueviolet.svg)](#architecture)

</div>

---

## The problem

Coding agents forget everything between sessions.

A fresh agent re-discovers the same project conventions, re-reads the same files, re-derives the same constraints, and walks straight into a trap another agent documented an hour ago. Bad assumptions calcify into "facts" because the harness has nowhere durable to look up the decisions, failures, rules, and evidence from prior runs.

`ee` is that durable place. Your agent harness still owns the loop, the tools, and the approvals — `ee` gives it a searchable, **explainable** memory it can write to and pull from.

```bash
ee pack "prepare a release for this project" --max-tokens 4000 --format markdown
```

That returns a ready-to-prepend Markdown context pack: the project's release rules, prior release incidents mined from your agent-session history, the exact verification commands, branch traps, and high-severity warnings — **each item carrying an evidence pointer and a score breakdown** so the agent (or you) can see *why* it's there.

---

## Why `ee` instead of a vector DB or an MCP memory server

Most "agent memory" is a vector store with `save` and `search`. `ee` is built around the parts that actually matter once an agent *relies* on memory to make decisions:

**1. Every memory explains itself.** `ee why <id>` tells you why a memory was stored, how it scored, what evidence supports it, how fresh it is, and how reliable it is — with provenance URIs back to the originating session. A vector DB hands you an opaque cosine score and nothing else.

**2. It can stop you from doing damage.** `ee preflight check --cmd "<shell command>"` runs the command against your accumulated risk and anti-pattern memories *before* it executes. A memory recording "`git push --force` to main wiped a teammate's work last quarter" becomes a guard that halts the command (exit 7). No other agent-memory tool turns memory into a safety interlock.

**3. Memory that forgets correctly.** Procedural rules carry confidence that **decays** over time, weighted by how structurally load-bearing the memory is in the graph. Harmful feedback demotes a rule faster than helpful feedback promotes it. A fact you confirmed once last year shouldn't outrank a rule three agents validated this week — and here it doesn't.

**4. The memory graph is a first-class analytic object.** Memories link to each other, and `ee` runs real graph algorithms over those links — PageRank, HITS, Personalized PageRank, Gomory-Hu min-cut **proximity**, causal-path tracing, and "Pack DNA" that explains *which graph structure shaped a given context pack*. Built on `franken_networkx`, not hand-rolled.

**5. It bootstraps from history you already have.** `ee import cass` mines your existing [`coding_agent_session_search`](https://github.com/Dicklesworthstone/coding_agent_session_search) corpus (Claude Code, Codex, Cursor, Gemini, ChatGPT transcripts) into evidence-backed memory candidates. You don't start from an empty store.

**6. Local, deterministic, single binary.** No cloud, no paid API, no background service for the core loop. Embeddings run locally. The same database + indexes + query produces a **byte-identical pack hash** every time — so packs are replayable and auditable.

### Comparison

| | `ee` | Vector DB (Chroma, Qdrant) | MCP memory server | Plain notes / CLAUDE.md |
|---|:---:|:---:|:---:|:---:|
| Local-first, single binary | ✅ | ❌ | varies | ✅ |
| Hybrid lexical + semantic retrieval | ✅ | ❌ vector-only | partial | ❌ |
| Provenance + score breakdown per fact | ✅ | ❌ | partial | manual |
| Procedural rules with decay | ✅ | ❌ | ❌ | ❌ |
| Anti-pattern guard on shell commands | ✅ | ❌ | ❌ | ❌ |
| Graph analytics over memory | ✅ | ❌ | ❌ | ❌ |
| Deterministic, replayable packs | ✅ | varies | varies | n/a |
| Imports your agent-session history | ✅ | manual ETL | ❌ | manual |
| Works with zero background processes | ✅ | ❌ | ❌ | ✅ |
| Audited curation (no silent rewrites) | ✅ | ❌ | ❌ | git only |

---

## Install

`ee` is pre-release; build it from source with a nightly Rust toolchain:

```bash
git clone https://github.com/Dicklesworthstone/eidetic_engine_cli
cd eidetic_engine_cli
cargo build --release
./target/release/ee --version
```

Confirm the install:

```bash
ee doctor --json        # database, schema, index, embedding, and capability posture
ee capabilities --json  # what's available in this build
```

Signed GitHub-release binaries, a Homebrew tap, and a `cargo install` path are planned but not yet published.

---

## Quick start

```bash
# 1. Open a workspace (idempotent — creates the DB, runs migrations, prepares indexes)
ee init --workspace .

# 2. Optionally seed from your agent-session history (recommended, once)
ee import cass --workspace . --limit 50

# 3. Pull context before substantive work
ee pack "what should I know before refactoring the storage layer?" \
  --workspace . --max-tokens 4000 --format markdown

# 4. Capture a durable lesson when you learn one
ee remember "Integration tests must hit a real database, never a mock — see the 2025-Q3 incident." \
  --workspace . --level procedural --kind rule --tags testing,db

# 5. Close the loop when a memory helps or misleads you
ee outcome <memory-id> --signal helpful --reason "Caught a mocked-test regression"
```

That is the core loop. Everything else is in service of it.

---

## The essential commands

`ee` has a large surface (`ee <command> --help` for any of it), but day-to-day agent work uses a small, stable set:

| Command | What it does |
|---|---|
| `ee init --workspace .` | Create/open a workspace; run migrations; prepare indexes |
| `ee pack "<task>" [--max-tokens N] [--format markdown\|json\|toon]` | Assemble a task-specific context pack with provenance and scores |
| `ee search "<query>" [--explain] [--json]` | Hybrid lexical + semantic retrieval over memories, rules, and evidence |
| `ee remember "<text>" --level <l> [--kind <k>] [--tags a,b]` | Capture a durable memory |
| `ee why <memory-id> [--json]` | Explain why a memory was stored, scored, and selected |
| `ee outcome <id> --signal helpful\|harmful [--reason "<r>"]` | Record feedback; updates utility and confidence |
| `ee preflight check --cmd "<shell command>" [--json]` | Check a risky command against the policy / trauma guard |
| `ee import cass [--limit N]` | Mine your `cass` agent-session corpus into memory candidates |
| `ee swarm brief [--json]` | Read-only coordination preflight for crowded multi-agent repos |

**Memory levels:** `working` (hours) · `episodic` (days) · `semantic` (long-lived facts) · `procedural` (rules, longest-lived).
**Memory kinds:** `rule`, `fact`, `decision`, `failure`, `command`, `convention`, `anti-pattern`, `risk`, `playbook-step`.

Output: `--format markdown` for prompt text, `--json` for parsers, `--format toon` for token-tight budgets, `--stream --json` for NDJSON frames.

### Curation: turning sessions into rules

```bash
ee review session <cass-session-id> --propose --dry-run --json  # distill evidence into candidates
ee curate candidates                                            # list pending candidates
ee curate validate <candidate-id>                              # specificity / duplication / scope / evidence checks
ee curate apply <candidate-id>                                 # promote, with an audit entry
```

Promotions, consolidations, and tombstones always produce an audit record. The steward proposes; it never silently rewrites procedural memory.

### Graph-derived insight (when retrieval looks surprising)

```bash
ee pack "<task>" --explain --json | jq '.data.pack.packDna'   # which graph structure shaped this pack
ee why <id> --causal-explain --json                           # causal ancestry + min-cost path evidence
ee insights --section bridges --json                          # articulation-point memories worth reviewing
ee proximity <mem-a> <mem-b> --json                           # Gomory-Hu min-cut between two memories
```

Graph views explain relationships; they never replace the provenance on the memory records themselves.

---

## The machine contract

Every machine-facing command emits a versioned envelope:

```jsonc
{ "schema": "ee.response.v2", "success": true, "data": {}, "degraded": [] }
```

| Check | Read |
|---|---|
| Envelope | `schema` + `success` |
| Degradations that affected this response | `degraded[]` (each with a `repair` hint) |
| Structured recovery actions on error | `error.details.recovery[]` |
| Provenance / trust | `provenance[]`, `evidence_spans[]`, `trustClass` |
| Pack identity (replayable) | `data.pack.hash` |

Exit codes: `0` success · `1` usage · `2` config · `3` storage · `4` search/index · `5` import · `6` degraded-required · `7` policy denied · `8` migration required.

When a capability is missing, `ee` degrades instead of failing: semantic model down → lexical BM25 + FTS5; graph snapshot stale → retrieval without graph boosts; `cass` absent → explicit `ee remember` records still work; no network → everything (it's local-first). Each degradation names itself in `degraded[]` with a repair command.

---

## Architecture

```
agent / human
   │  ee pack / search / remember / why / preflight / import
   ▼
ee-cli → ee-core
            ├── db        FrankenSQLite via SQLModel — the source of truth
            ├── search    Frankensearch — hybrid lexical + vector (derived, rebuildable)
            ├── cass      imports evidence from coding_agent_session_search
            ├── graph     franken_networkx projections + metrics
            ├── pack      deterministic context packs with provenance
            ├── curate    rule candidates, validation, decay, audit
            └── policy    redaction, scope, retention, trauma-guard
```

Design contracts the code holds itself to:

- **Source of truth is the database.** Indexes, embeddings, graph snapshots, and caches are derived and rebuildable (`ee index rebuild`).
- **Deterministic by default.** Same DB + indexes + config + query → byte-identical pack hash. Golden tests assert it.
- **Evidence before promotion.** A rule with no source, no feedback, and no validation stays low-confidence.
- **Explainable retrieval.** Every returned memory answers: why selected, what supports it, how fresh, how reliable, what scores mattered.
- **No silent mutation.** Every promotion, consolidation, and tombstone is audited.

Hard constraints (CI-enforced): the binary is `ee`; runtime is [`asupersync`](https://github.com/Dicklesworthstone/asupersync) (**no Tokio**); storage is FrankenSQLite via SQLModel (**no `rusqlite`/`sqlx`/`diesel`**); search is Frankensearch (no custom BM25/vector code); graph is `franken_networkx` (**no `petgraph`**).

### Memory model

| Level | Examples | Decay | Packing priority |
|---|---|---|---|
| `working` | Active task notes, scratch | fastest | low (suppressed across sessions) |
| `episodic` | "On 2026-03-12 the release failed because…" | medium | medium |
| `semantic` | Project conventions, architectural facts | slow | high |
| `procedural` | Rules, anti-patterns, playbooks | slowest, decays mainly on contradiction | highest |

Level changes are explicit, audited lifecycle transitions between adjacent levels — never silent rewrites. Every memory carries `id`, `level`, `kind`, `content`, `content_hash`, `tags[]`, `confidence`, `utility`, `importance`, timestamps, `source_uri`, `evidence_spans[]`, `links[]`, and `trust_class`.

---

## Privacy & trust

Secrets are detected and redacted **before** storage (default classes: `api_key`, `jwt`, `password`, `private_key`, `ssh_key`, `aws_secret`, `oauth_token`):

```bash
ee remember "DATABASE_URL=postgres://user:hunter2@host/db"
# stored as: "DATABASE_URL=postgres://user:***REDACTED:password***@host/db"
```

Memories carry a **trust class** (`human_explicit` → `agent_validated` → `agent_assertion` → `cass_evidence` → `legacy_import`) that bumps on validation and demotes on contradiction, plus an advisory priority (`blocked` / `quarantined` / `degraded` / `advisory` / `clear`) that bounds packing. A prompt-injection guard flags fake instructions, role-override attempts, and exfiltration cues, routing suspicious content to the curation queue instead of silently into the procedural layer. Full model: [`docs/trust-model.md`](docs/trust-model.md).

---

## Configuration

Precedence (highest first): CLI flags → `EE_*` env vars → `<workspace>/.ee/config.toml` → `~/.config/ee/config.toml` → built-in defaults.

```toml
# .ee/config.toml
[storage]
database_path = "~/.local/share/ee/ee.db"

[pack]
default_profile  = "balanced"   # compact | balanced | grounding | orientation | thorough | submodular
adaptive_budget  = true          # size the token budget from retrieval entropy + graph fanout

[curation]
harmful_weight   = 2.5           # harmful feedback demotes faster than helpful feedback promotes
```

The full surface is in [`docs/env_vars.md`](docs/env_vars.md) and [`docs/configuration/`](docs/configuration/). Data lives under `~/.local/share/ee/` (DB + derived indexes + cache); per-project overrides live in `<workspace>/.ee/`.

---

## Optional surfaces

Off the core path, present when you want them:

- **`ee daemon`** — supervised foreground process with a Unix-domain-socket RPC. Currently a hardened skeleton (peer-cred auth, bounded workers, panic supervision, atomic socket bind); the RAM-pinned hot-mode ANN it's built toward is not yet shipped.
- **`ee mesh`** — optional Tailscale-based memory sharing across a trusted tailnet, off by default. Exchanges redaction-safe rows under explicit lane policy.
- **`ee handoff` / `ee support bundle`** — signed, redacted capsules for resuming work across agents/machines and filing bug reports without leaking content.
- **`ee backup` / `ee export`** — verified backups with content-hash manifests; portable redacted JSONL export.

---

## Beyond coding

The same model works outside software wherever the work has durable facts, recurring decisions, and cited sources — investment research, legal work, incident response, product management. Isolate sensitive domains by workspace (`ee init --workspace ./matters/smith-v-jones`) and feed memory via `ee remember` or `ee import jsonl --source <file>`. A particularly useful pattern is a **negative-evidence ledger** for long optimization campaigns: record each failed attempt (`--kind failure`), what it lost, and the smallest artifact that proves it lost, then `ee playbook extract` clusters the repeats into a validated anti-pattern that primes the next attempt.

---

## Development

```bash
./scripts/verify.sh          # full readiness gate: forbidden-dep audit, lints, tests, E2E, determinism
cargo test --workspace
cargo clippy --all-targets -- -D warnings
```

See [`AGENTS.md`](AGENTS.md) for contributor rules and [`docs/`](docs/) for architecture, ADRs, schemas, and the agent-UX onboarding guides.

---

## FAQ

**Does this replace Claude Code, Codex, or my agent harness?**
No. It's the durable memory those harnesses call. The harness owns the loop; `ee` owns memory.

**Does it phone home or call any external API?**
`ee` itself makes no network calls. Embedding is delegated to Frankensearch, which runs locally by default; pointing it at a remote model is an explicit operator choice.

**Can I use `ee` without `cass`?**
Yes. `cass` is an evidence source, not a hard dependency. Without it, `ee remember`, `ee pack`, `ee search`, curation, graph, and packing all work normally.

**How big does the database get?**
On a typical multi-project machine, expect ~50–500 MB after a year. Cold/warm/hot tiering keeps the hot path small; `ee backup create` produces portable, verified archives.

**What happens if my index gets corrupted?**
`ee index rebuild` reproduces it from the DB. Indexes are derived assets — losing them is annoying but recoverable.

**Can multiple agents on one machine share a database?**
Yes. Reads are concurrent; writes serialize through a job lock. For heavy multi-writer swarms, run `ee daemon` and let it own the write side.

**Does `ee` ever rewrite my memories silently?**
No. The steward proposes; you approve. Every promotion, consolidation, replacement, and tombstone is recorded and visible via `ee why <id>` and the curation queue.

**Where are the architectural decisions?**
[`docs/adr/`](docs/adr/) — every major subsystem has an ADR with rejected alternatives and verification hooks.

---

## About contributions

Please don't take this the wrong way, but I do not accept outside contributions for any of my projects. I simply don't have the mental bandwidth to review anything, and it's my name on the thing, so I'm responsible for any problems it causes; thus, the risk-reward is highly asymmetric from my perspective. I'd also have to worry about other "stakeholders," which seems unwise for tools I mostly make for myself for free. Feel free to submit issues, and even PRs if you want to illustrate a proposed fix, but know I won't merge them directly. Instead, I'll have Claude or Codex review submissions via `gh` and independently decide whether and how to address them. Bug reports in particular are welcome. Sorry if this offends, but I want to avoid wasted time and hurt feelings. I understand this isn't in sync with the prevailing open-source ethos, but it's the only way I can move at this velocity and keep my sanity.

---

## License

MIT, with an OpenAI/Anthropic rider — see [`LICENSE`](LICENSE).

© 2026 Jeffrey Emanuel
