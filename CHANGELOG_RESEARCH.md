# Changelog Research Memo

This memo records the evidence trail behind `CHANGELOG.md`. It is deliberately
more procedural than the changelog so future release-note work can resume from
the same facts instead of redoing the whole archaeology pass.

## Method

Instructions applied:

- Read `AGENTS.md` in full.
- Read `README.md` in full.
- Use code investigation / architecture archaeology before writing release
  notes.
- Apply the `changelog-md-workmanship` workflow: do not write from memory; use
  git, tags, release surfaces, docs, code, tests, and issue tracker data; keep
  GitHub Releases distinct from tags; retain a research ledger.

Coordination:

- Tried Agent Mail bootstrap/reservation. The MCP service reported degraded
  read-only/archive parity problems, so no reservation was acquired.
- Ran local inspection only and limited edits to new changelog files.

No destructive commands, worktrees, branch switches, stashes, resets, cleanups,
or file deletions were used.

## Repository State At Audit

```text
cwd: /Users/jemanuel/projects/eidetic_engine_cli
branch: main...origin/main
audit head: 050602500c566e1e2603bb36a1f1cdcae1d292c3
remote: https://github.com/Dicklesworthstone/eidetic_engine_cli
existing changelog: none at repository root
dirty scope noted before changelog edit: README.md and many unrelated files were already modified
```

## Release Surface Findings

Commands and observations:

```bash
git tag --sort=creatordate
git for-each-ref refs/tags --format='%(refname:short) %(creatordate:short) %(objectname:short) %(contents:subject)'
gh release list --repo Dicklesworthstone/eidetic_engine_cli --limit 100
```

Findings:

- One tag exists: `v0.1.0`, dated 2026-05-15, subject `v0.1.0 - initial release`.
- No GitHub Release rows were returned.
- Therefore `v0.1.0` is recorded as a git tag, not as a GitHub Release with
  published assets.

## History Scale

Commands:

```bash
git rev-list --count --no-merges HEAD
git log --no-merges --date=short --pretty='%ad' | sort | uniq -c
```

Findings:

```text
3,516 non-merge commits reviewed at history level.

2026-04-29  120
2026-04-30  154
2026-05-01   33
2026-05-02   63
2026-05-03   78
2026-05-04  185
2026-05-05  171
2026-05-06  164
2026-05-07  112
2026-05-08  210
2026-05-09   31
2026-05-10   43
2026-05-11   32
2026-05-12   90
2026-05-13   61
2026-05-14   80
2026-05-15  129
2026-05-16  748
2026-05-17  264
2026-05-18  133
2026-05-19  373
2026-05-20  242
```

## Tracker Scale

Source: `.beads/issues.jsonl`.

Status counts during this audit:

```text
closed       2113
open          150
blocked        67
in_progress     3
deferred        1
```

High-signal closed labels:

```text
test-coverage-required 678
ee-plan 454
idea-wizard 170
graph 123
testing 120
agent-ux-overhaul 117
swarm-scale 104
schema-golden-required 99
logged-e2e-required 96
closure-linted 90
test-harness 70
e2e 66
wave-4 65
rch 60
mesh 57
tailscale 53
verification 52
pack 51
golden 50
epic 49
```

Representative closed tracker workstreams used in the changelog:

- `bd-3usjw.*`: implements-surface, trauma guard, adapter honesty, README/CLI
  parity, release gate, performance metadata, verification discipline.
- `bd-w2ts`, `bd-zn8i`, `bd-v454`, `bd-dcub`: replayable context packs,
  freshness, redaction egress, pack replay/diff, pack ledger.
- `bd-rnfh`, `bd-igvt`, `bd-t6wd`, `bd-8jvg`, `bd-ov09`, `bd-fdvt`,
  `bd-qnfw`, `bd-zx2v`, `bd-mvld`, `bd-5vqr`, `bd-a7mm`: graph algorithm and
  graph-derived retrieval surfaces.
- `bd-fcq1`, `bd-k8dp`, `bd-mwjq`, `bd-3a5la`: swarm-scale, host-adaptive,
  performance forensics, and operator workflow.
- `bd-s7vd`, `bd-s38h`: project-local skills and skill e2e standards.
- `bd-x08h`, `bd-t5v49`, `bd-zp75`: verification baseline and closure-lint
  recovery.

## Documentation Read

`AGENTS.md` themes:

- User override, no file deletion, no destructive git/filesystem commands.
- No worktrees, no feature branches, no stash, main branch only.
- Cargo-only Rust 2024 project; unsafe forbidden.
- Forbidden dependencies include Tokio, rusqlite, sqlx, diesel, petgraph, and
  HTTP stacks in core.
- Mac build output routes to external USB-NVMe paths and must not be committed
  into repo config.
- Architecture: `cli -> core -> {db, search, cass, graph, pack, curate, policy,
  output} -> models`.
- `ee` is a local-first CLI memory layer for existing agent harnesses, not a
  replacement agent loop.
- Core path: `init`, `remember`, `search`, `context`, `why`, plus status.
- Stable JSON envelopes, degraded taxonomy, schema docs, env var registry, exit
  codes, determinism requirements, Agent Mail/Beads/RCH workflow.

`README.md` themes:

- `ee` is a single Rust binary for durable, local-first, explainable memory.
- Current install status is pre-release/source build; release channels are
  planned.
- Quick example covers `init`, `remember`, `import cass`, `context`, `why`,
  and `outcome`.
- Broad command surface covers core workflow, graph-derived insights,
  replay/diff, support bundles, swarm brief, import, curation, inspection,
  graph, index, workspace/db/model/schema, backup/restore, diagnostics, eval,
  and ops.
- Storage and architecture remain CLI-first, deterministic, local-first,
  explainable, redaction-aware, and graph/search/DB-backed.

## Code Architecture Pass

The Morph codebase search tool was attempted first for architecture discovery
but returned a 502/522 upstream error. Manual archaeology was used instead.

Files and surfaces inspected:

- `Cargo.toml`: package `eidetic-engine` v0.1.0, binary `ee`, library `ee`,
  default features `fts5`, `json`, `embed-fast`, `lexical-bm25`, `graph`, and
  optional `mcp`, `serve`, `science`.
- `src/main.rs`: lightweight entrypoint; initializes tracing, handles
  agent/hook JSON defaults, then delegates to `ee::cli::run`.
- `src/lib.rs`: public module index for cache, cass, cli, config, core,
  curate, db, eval, graph, hooks, mesh, models, obs, output, pack, policy,
  runtime, search, shadow, steward, util, optional mcp/serve/science.
- `src/cli/mod.rs`: Clap command definitions, help prelude, global output
  options, workspace resolution, renderer handling, and command dispatch.
- `src/core/mod.rs`: central use-case module exports, build info, feature and
  schema registry, supported schemas.
- `src/db/mod.rs`: FrankenSQLite/SQLModel connection wrapper, migrations,
  repository helpers, audit actions, audit constants, and open modes.
- `src/core/memory.rs`: remember/report contracts, memory metadata, producer
  metadata, revision/workflow options, auto-links, audit/index jobs.
- `src/core/search.rs`: search options, source modes, validity filters,
  Frankensearch/Tantivy integration, degraded aggregation.
- `src/core/context.rs`: context pack assembly, search integration, graph/PPR
  signals, snapshot/slot locks, pack persistence, deterministic seed support.
- `src/pack/mod.rs`: token budgets, profile resources, MMR/facility-location,
  deterministic hashing, pack metadata, tiktoken integration.
- `src/models/memory.rs`: memory levels, memory kinds, tag/content validation,
  scoring fields, and domain invariants.
- `src/output/mod.rs`: JSON, human, TOON, JSONL, compact, hook, and markdown
  renderers plus response schema versions and field filtering.
- `src/cass/import.rs`: CASS session discovery, robot JSON contracts, import
  ledger, persistence, index jobs, source-reference redaction.
- `src/graph/mod.rs`: FrankenNetworkX integration, graph snapshots, PageRank,
  betweenness, witnesses, typed graph snapshot support, algorithm wrappers.

Tests and docs inspected:

- `tests/` has extensive contract, smoke, golden, schema, failure-mode,
  graph, mesh, doctor, determinism, and e2e coverage.
- `docs/` includes ADRs, agent UX docs, schema docs, degraded-code docs, RCH
  docs, pack replay docs, graph docs, testing strategy, and migration notes.
- `scripts/` includes `verify.sh`, forbidden-deps, closure-lint, verification
  drift, vision coverage, e2e overhaul stages, RCH helpers, and release checks.

## History Spine

The public changelog compresses the following commit-date themes:

| Date | Theme |
| --- | --- |
| 2026-04-29 | Project foundation: docs/plans, Rust skeleton, forbidden deps, config/workspace/models, DB/search/pack/CASS scaffolds. |
| 2026-04-30 | CLI globals, output schemas, TOON, status/check/capabilities, provenance, tests, fuzz/property scaffolds, CASS robot contract. |
| 2026-05-01 | Planner, recovery docs, signing policy, Mermaid/certificate/counterfactual lab readiness. |
| 2026-05-02 | Performance proof stream, benchmarks, degraded/offline scenario, causal/lab readiness. |
| 2026-05-03 | Boundary migration, side-effect contracts, skills standards, mechanical command inventory. |
| 2026-05-04 | Reality/security hardening, curation/rule fixes, outcome/focus/situation surfaces. |
| 2026-05-05 | Trauma guard, no-silent-fallback, trust, workflow IDs, races/deadlocks, markdown escaping. |
| 2026-05-06 | Release CI, installer, daemon scaffold, recorder/tripwire, closure lint, append-only audit, claim verification. |
| 2026-05-07 | Implements-surface wave: audit, certificate, eval, handoff, support bundle, recorder, causal, swarm-scale. |
| 2026-05-08 | Query v1, graph/index, no-silent-fallback, pack replay groundwork, performance forensics. |
| 2026-05-09 | Pack replay ledgers, replay/diff, evidence freshness, redaction egress. |
| 2026-05-10 | Rule/memory/curation/workflow and graph/index surfaces. |
| 2026-05-11 | Validity-window and deterministic fixture work. |
| 2026-05-12 | Schema/degraded/env-var catalogs, migrations, tombstones, acceptance gate cleanup. |
| 2026-05-13 | Backup manifest v2, Bayesian posterior, handoff HMAC, build admission. |
| 2026-05-14 | Typed graph subgraphs, witnesses, test tracing, hardware manifest, status/golden rebaseline. |
| 2026-05-15 | Initial tag: graph G1-G10, Pack DNA, insights, trauma guard, adapter honesty, read-pool/singleflight. |
| 2026-05-16 | Huge post-tag mesh/graph/context/determinism/RCH/read-pool/write-owner hardening wave. |
| 2026-05-17 | Mesh, graph, hygiene, redaction, transaction recovery, witness retention. |
| 2026-05-18 | Workspace hygiene, preflight patterns, search plan cache diagnostics, tripwire widening. |
| 2026-05-19 | Graph rendering/help/perf tie-breakers, mesh foreground CLI, shard fan-out, symbol graph, PPR/NUMA/cache. |
| 2026-05-20 | Mesh/Tailscale, doctor, flight recorder, QoS, HITS, Gomory-Hu, load-bearing, EQL cache, audit lane. |

## Representative Commit Link Ledger

Foundation:

- [`b478d7e5`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/b478d7e5) - Rust skeleton.
- [`0e650413`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/0e650413) - forbidden dependency audit.
- [`12ad584d`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/12ad584d) - CLI parser.
- [`f8606c8`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/f8606c8) - DB connection.
- [`6ee2f964`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/6ee2f964) - memory repository.
- [`4e67cfd9`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/4e67cfd9) - search integration.
- [`4bbb409f`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/4bbb409f) - pack records.

Graph and retrieval:

- [`df459466`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/df459466) - graph G1-G10 work.
- [`23ff6a70`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/23ff6a70) - insights.
- [`ebeda496`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/ebeda496) - Pack DNA provenance.
- [`4a12ec0c`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/4a12ec0c) - HITS role scores.
- [`2ff0d4e8`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/2ff0d4e8) - EQL plan cache.
- [`d92d0995`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/d92d0995) - PPR prefetch cache.

Ops, safety, and mesh:

- [`907d6879`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/907d6879) - trauma guard.
- [`6520a9b1`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/6520a9b1) - backup manifest v2.
- [`d527adcc`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/d527adcc) - Bayesian memory posterior.
- [`6025cf40`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/6025cf40) - Tailscale peer autodiscovery.
- [`9fd1d9f4`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/9fd1d9f4) - Tailscale auto-enrollment.
- [`73d1d181`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/73d1d181) - doctor robot triage.
- [`657d0386`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/657d0386) - flight recorder.

## Caveats

- This is a changelog reconstruction, not a signed release note generated at
  release time.
- GitHub links to short hashes assume they remain unique in the repository.
- Beads links point at the checked-in JSONL tracker file because individual
  tracker records are not exposed as stable GitHub issue URLs.
- The repository was already in a multi-agent dirty state. The changelog pass
  intentionally avoided modifying existing source/docs/test files.
