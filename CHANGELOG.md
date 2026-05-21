# Changelog

This changelog was reconstructed from the repository, not from memory. Sources
used for this pass were `AGENTS.md`, `README.md`, Cargo metadata, source module
entrypoints, tests, docs, checked-in Beads records, tags, and non-merge git
history through the audit head linked below on 2026-05-20.

Scope window: first commit on 2026-04-29 through
[`050602500c566e1e2603bb36a1f1cdcae1d292c3`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/050602500c566e1e2603bb36a1f1cdcae1d292c3)
on 2026-05-20.

Release status:

- The repository has one git tag: [`v0.1.0`](https://github.com/Dicklesworthstone/eidetic_engine_cli/tree/v0.1.0), tagged on 2026-05-15.
- `gh release list` returned no GitHub Release rows at the time of this audit. The tag exists; a GitHub Release page with assets/notes does not.
- Source install is the live path. GitHub binary releases, Homebrew, and crates.io remain planned surfaces in `README.md`.

Evidence scale:

- 3,516 non-merge commits were reviewed at the history level.
- `.beads/issues.jsonl` showed 2,113 closed issues, 150 open issues, 67 blocked issues, 3 in-progress issues, and 1 deferred issue during this audit.
- There was no root `CHANGELOG.md` before this pass.
- The detailed audit ledger lives in [`CHANGELOG_RESEARCH.md`](./CHANGELOG_RESEARCH.md).

## [Unreleased] - 2026-05-16 to 2026-05-20

Post-`v0.1.0` work is a large hardening and expansion wave. The main themes are
graph-derived retrieval, optional mesh/Tailscale coordination, doctor first-aid,
flight recording, QoS, deterministic output contracts, and crowded-checkout
agent ergonomics.

### Added

- Added agent-ergonomics surfaces from `bd-3qs2i`: `ee curate accept/reject
  --reason <TEXT>` preserves reviewer rationale, `pack_budget_too_small`,
  `harmful_burst_quarantine`, and `embed_model_unavailable` are documented
  degraded codes, and `ee rule mark` tracks validation passes and
  contradictions separately from outcome counters.
- Added optional mesh and Tailscale-oriented coordination surfaces:
  peer autodiscovery, auto-enrollment flow, auto-status views, discovery policy,
  explicit revision tokens, foreground mesh CLI mode, emergency-disable paths,
  replay recovery status, quarantine/repair status, and hello responder
  lifecycle contracts.
  Representative commits:
  [`6025cf40`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/6025cf40),
  [`9fd1d9f4`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/9fd1d9f4),
  [`9e3552ce`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/9e3552ce),
  [`baf954de`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/baf954de).
- Added doctor first-aid and operator triage surfaces:
  `ee doctor --quick`, `--only`, `--since`, `--robot-triage`,
  `--gc-plan`, `--fix`, `--undo`, `--capabilities`, mesh auto-enrollment
  checks, safety-harness integration, and a corrected envelope contract where
  `success` means the doctor command ran, not that the system is healthy.
  Representative commits:
  [`6fd75080`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/6fd75080),
  [`73d1d181`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/73d1d181),
  [`587fe9d3`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/587fe9d3),
  [`ed59bc9a`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/ed59bc9a),
  [`2ef934a4`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/2ef934a4),
  [`b95dce7a`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/b95dce7a).
- Added agent workload flight-recorder infrastructure:
  trace schema, recorder module, env registry entries, status/doctor posture
  mapping, e2e harness contract, and operator/agent docs.
  Representative commits:
  [`fc31bec1`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/fc31bec1),
  [`657d0386`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/657d0386),
  [`96820199`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/96820199),
  [`a79e4706`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/a79e4706),
  [`1604c235`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/1604c235),
  [`fdd7b35a`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/fdd7b35a).
- Added graph and retrieval expansion after the tag:
  HITS role scores, HITS profile names, PPR prefetch cache, Gomory-Hu
  self-proximity coverage, load-bearing why/curate guard surfaces, symbol graph
  evidence links, EQL plan cache, bead-affinity scoring, dedup-link evidence,
  conformal score intervals, and graph flag/help coverage.
  Representative commits:
  [`4a12ec0c`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/4a12ec0c),
  [`ebc183c8`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/ebc183c8),
  [`d92d0995`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/d92d0995),
  [`d15d0000`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/d15d0000),
  [`07a0c62b`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/07a0c62b),
  [`c272e9eb`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/c272e9eb),
  [`2ff0d4e8`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/2ff0d4e8).
- Added shard fan-out and multi-agent write-path groundwork:
  migration apply-mode skeleton, per-shard degraded aggregation, audit-lane
  workload e2e, shard schema/config/router tasks, and global timeline work
  recorded in Beads.
  Representative commits:
  [`6e7e7a8f`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/6e7e7a8f),
  [`e3164b14`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/e3164b14),
  [`669bf47e`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/669bf47e).
- Added workspace hygiene and crowded-agent ergonomics:
  comprehensive `ee workspace hygiene` surface, bounded output, secret-risk
  scanning, JSON and combined parser coverage, dirty Beads/RCH proof guidance,
  and command/help docs around graph and hygiene flags.
  Representative commits:
  [`a4597eb3`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/a4597eb3),
  [`cb5ceca4`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/cb5ceca4),
  [`927de076`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/927de076).

### Changed

- Tightened deterministic ranking and output stability across search, graph,
  PPR, dominance, causal traversal, structural health, skyline, Pack DNA, and
  HITS by moving tie-breakers toward stable radix/id ordering.
  Representative commits:
  [`881ec7d7`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/881ec7d7),
  [`655be21a`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/655be21a),
  [`dcae6f13`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/dcae6f13),
  [`589d8ce6`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/589d8ce6).
- Expanded output rendering for graph schema blocks, Pack DNA markdown, causal
  markdown, graph status formats, insights format dispatch, and command-manifest
  parity.
  Representative commits:
  [`81530b7c`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/81530b7c),
  [`b1b89e24`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/b1b89e24),
  [`9006bbf0`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/9006bbf0),
  [`b8da5005`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/b8da5005),
  [`06c4fb13`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/06c4fb13),
  [`c9c5a8b2`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/c9c5a8b2).
- Strengthened safety around symlinks and non-regular files in CASS import,
  preflight rules, QoS lane registries, init metadata, and discovery binaries.
  Representative commits:
  [`48ceb2cc`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/48ceb2cc),
  [`fd14bb94`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/fd14bb94),
  [`c17db4d8`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/c17db4d8),
  [`5de433fb`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/5de433fb),
  [`486aabc3`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/486aabc3).

### Fixed

- Fixed release compile blockers across audit, context, and migration code.
  Representative commit:
  [`e4a525b3`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/e4a525b3).
- Fixed degraded aggregation capping and per-surface routing for graph,
  shard-fanout, and curate/status outputs.
  Representative commits:
  [`57c02dab`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/57c02dab),
  [`59d8432f`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/59d8432f),
  [`669bf47e`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/669bf47e),
  [`9946e34f`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/9946e34f).
- Fixed numerous redaction leaks and source-reference exposures in recorder,
  CASS, status response counts, mesh import paths, and graph outputs.
  Representative commits:
  [`07cbb3b4`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/07cbb3b4),
  [`9c6a3fe0`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/9c6a3fe0),
  [`0461b697`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/0461b697).
- Fixed NaN-sensitive scoring math across retrieval, causal, db, and clustering
  paths.
  Representative commit:
  [`8bf5bb96`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/8bf5bb96).

### Tests And Verification

- Added mesh-off, mesh policy, mesh privacy, byte-stability, discovery,
  auto-enrollment, hello handshake, and two-tier budget contracts.
- Added graph/HITS/PPR/Gomory-Hu/load-bearing/symbol-graph contracts, perf
  gates, parser coverage, CLI help drift guards, schema snapshots, and e2e
  harness coverage.
- Added doctor fixture suites, undo/fix e2e harnesses, safety harness stage
  integration, workspace hygiene logged e2e, audit-lane e2e, and flight-recorder
  e2e contracts.
- Hardened RCH verifier scripts and proof guidance without falling back to local
  Cargo in remote-only contexts.

## [0.1.0] - 2026-05-15

`v0.1.0` is the initial tagged source release of `ee`: a local-first Rust CLI
memory substrate for coding agents. It is not a general agent harness, daemon,
planner, or web service. The controlling loop is:

```bash
ee init --workspace . --json
ee remember --workspace . --level procedural --kind rule "Run cargo fmt --check before release." --json
ee search "format before release" --workspace . --json
ee context "prepare release" --workspace . --format markdown
ee why <memory-id> --json
ee status --json
```

### Core Architecture

- Established a single Rust 2024 binary crate with binary `ee`, library surface
  `ee`, `#![forbid(unsafe_code)]`, Cargo-only project management, and strict
  avoidance of forbidden runtime/database/graph/HTTP stacks.
  Representative commits:
  [`b478d7e5`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/b478d7e5),
  [`0e650413`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/0e650413).
- Established the documented dependency direction:
  `cli -> core -> {db, search, cass, graph, pack, curate, policy, output} -> models`.
- Added source module boundaries for CLI parsing/dispatch, core use cases,
  DB/repositories, search, CASS import, graph analytics, pack assembly, curation,
  policy/redaction, output rendering, config, hooks, optional MCP, optional
  serve, observability, and steward jobs.
- Added stable response and error envelopes, global CLI flags, help/agent docs,
  field filtering, output formats, command manifests, and schema-aware renderers.
  Representative commits:
  [`12ad584d`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/12ad584d),
  [`bd-yh0`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-yyc`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-xf9`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).

### Memory, Storage, And Import

- Added workspace initialization, storage path resolution, TOML config parsing,
  config precedence, workspace repository, DB connection/migration helpers,
  transaction helpers, audit repository, and append-only audit concepts.
  Representative commits:
  [`e028f9b`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/e028f9b),
  [`f8606c8`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/f8606c8),
  [`bd-ywe7`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-trceq`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).
- Added memory IDs, provenance IDs, policy IDs, memory levels, memory kinds,
  tags, validity windows, legal holds, supersession links, idempotency keys,
  confidence/utility/importance fields, and bounded content validation.
- Added `ee remember`, memory repository persistence, memory history, rule
  lifecycle surfaces, expire/tags operations, workflow IDs, links, revision
  groups, and search-index job enqueueing.
  Representative commits:
  [`6ee2f964`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/6ee2f964),
  [`bd-sygu1`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-z4xi`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).
- Added CASS robot/JSON integration, session and span persistence, import
  ledger logic, import diagnostics, CASS health counting, and redaction-aware
  source references.
  Representative commits:
  [`f4623a4a`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/f4623a4a),
  [`bd-s67f`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).

### Search, Context Packs, And Explanation

- Added Frankensearch/Tantivy-backed search plumbing with lexical/semantic
  modes, degraded lexical fallback, validity-window filters, tombstone/expired
  filters, query-file support, pagination, deterministic tie handling, and
  memory-scope search.
  Representative commits:
  [`4e67cfd9`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/4e67cfd9),
  [`bd-w5w5`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-17c65.2.10`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).
- Added deterministic context pack assembly with token budgeting, profile
  support, provenance, explanation metadata, pack records/items, pack hashes,
  replay ledgers, freshness diagnostics, and markdown/JSON/TOON rendering.
  Representative commits:
  [`4bbb409f`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/4bbb409f),
  [`bd-aitk`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-zn8i`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-w2ts`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).
- Added `ee why`, storage/retrieval/pack explanation, memory link rendering,
  causal explanation, revision lineage, pack DNA, why-not-selected style
  diagnostics, and output contracts.
- Added pack replay/diff, pack quality evaluation, support-bundle summaries,
  query-file pack paths, large-fixture freshness scans, and redaction egress
  proofs.
  Representative Beads:
  [`bd-v454`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-4bya6`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-dcub`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-rynf`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).

### Curation, Learning, And Rules

- Added curation candidates, review/propose surfaces, rule mark/update,
  procedure verification, playbook export/import, rule protection, agenda and
  uncertainty outputs, rule provenance, anti-pattern/trauma guard logic, and
  low-evidence rejection paths.
- Added Bayesian memory posterior scoring, harmful-weight-aware credible
  intervals, structural decay hooks, maturity/trust handling, and rule
  promotion constraints.
  Representative commits:
  [`d527adcc`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/d527adcc),
  [`bd-rua0`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-ynzg`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-zgjc`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).
- Added claim parsing/verifying, evidence ledgers, certificate verification,
  real certificate signing, local signing policy, provenance chain hashes, and
  sampled provenance verification.
  Representative Beads:
  [`bd-qigqt`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-xvre`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-s4fk`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-w7ih`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-xxhe`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).

### Graph Analytics And Structural Retrieval

- Added graph snapshot framework, graph centrality and refresh surfaces,
  graph/index maintenance, typed subgraphs, algorithm-result caches, witnesses,
  schema registration, insights sections, health structural reports, Pack DNA,
  PPR reranking, Gomory-Hu proximity, causal paths, dominance/revision
  frontiers, minhash rank, skyline, K-truss, contradiction clusters, structural
  decay, and graph determinism harnesses.
  Representative commits:
  [`df459466`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/df459466),
  [`23ff6a70`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/23ff6a70),
  [`ebeda496`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/ebeda496),
  [`5e9ae784`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/5e9ae784).
- Closed or substantially satisfied the graph workstream around:
  F1 typed subgraphs, F2 algorithm wrappers/witnesses/cache, F3 `ee insights`,
  F4 determinism/golden harnesses, G1 PPR, G2 Pack DNA, G3 causal explanation,
  G4 structural health, G5 structural decay, G6 Gomory-Hu, G7 dominance, G8
  skyline, and G10 HITS.
  Representative Beads:
  [`bd-rnfh`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-igvt`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-t6wd`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-8jvg`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-ov09`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-fdvt`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-qnfw`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-zx2v`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-mvld`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-5vqr`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-a7mm`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).

### Diagnostics, Operations, And Maintenance

- Added `ee status`, `ee doctor`, `ee check`, capability reporting, posture
  summaries, structured suggested actions, failure-mode fixtures, degraded-code
  taxonomy, status/check/capabilities/version/doctor goldens, and machine-facing
  output contracts.
- Added support bundles, backup/restore, derived-asset backup manifest v2,
  WAL/orphan diagnostics, graph state preservation, HMAC handoff capsules,
  install/update recovery recipes, install audit, disk-pressure and build
  admission reporting, and RCH-aware verification documentation.
  Representative commits:
  [`6520a9b1`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/6520a9b1),
  [`da92d844`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/da92d844),
  [`87d58d0c`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/87d58d0c),
  [`bd-wtpl`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-49cvw`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).
- Added optional daemon and serve/MCP scaffolds while keeping normal operation
  CLI-first and honest when adapters are disabled or deferred.
  Representative Beads:
  [`bd-9s0q`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-s9kgl`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-3usjw.3`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-3usjw.4`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).

### Safety, Policy, And Trust

- Added trauma guard / destructive-command preflight policy, hook helper,
  destructive pattern fixtures, policy denied exit behavior, shell-safe gap
  handling, tripwire detection, incident recovery safety fixtures, and
  no-deletion/no-worktree guardrails in docs and tests.
  Representative commits:
  [`907d6879`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/907d6879),
  [`3fd402e5`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/3fd402e5),
  [`d5a25d93`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/d5a25d93),
  [`bd-3usjw.6`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-3usjw.7`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).
- Added privacy/redaction/trust surfaces:
  instruction-like content detection, unknown trust-class rejection, markdown
  escaping, raw JSON/TOON poisoning fixes, redaction leak evaluation, egress
  proofs, path redaction, and model/remote gating docs.
  Representative Beads:
  [`bd-zm78`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-rjrd`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-wtio`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-whxu`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-t7cx`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-rynf`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).

### Agent And Swarm Workflow

- Added swarm brief, swarm next-action, agent profile bias, bead affinity,
  support-bundle scale artifacts, contention/recovery suites, cache governors,
  hotset prewarm, write spool/backpressure contracts, host adaptive profiles,
  and operator cookbook material for swarm-scale work.
- Added Agent Mail posture snapshots, coordination docs, local skills, e2e skill
  standards, and agent-readable workflows for graph, doctor, RCH, and mesh work.
  Representative Beads:
  [`bd-fcq1`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-k8dp`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-3a5la`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-s7vd`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-s38h`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).

### Testing And Release Gates

- Added a central `scripts/verify.sh` orchestrator, forbidden-dependency checks,
  closure lint, vision coverage, verification drift guards, schema drift guards,
  failure-mode catalog validation, command boundary/effect contracts, golden
  snapshots, e2e overhaul scripts, deterministic runtime tests, property/fuzz
  harnesses, benchmark gates, and structured test event logging.
  Representative commits:
  [`d25e6445`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/d25e6445),
  [`d25e6445`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/d25e6445),
  [`2ebcf902`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/2ebcf902),
  [`3dc3c2f3`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/3dc3c2f3).
- Restored a repo-wide verification baseline before the tag and continued
  hardening with RCH-oriented proof records where local Cargo fallback is not
  acceptable.
  Representative Beads:
  [`bd-x08h`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-t5v49`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-zp75`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).

## Version Timeline Before And Around v0.1.0

The tag contains a compressed three-week buildout. This date spine is included
because most features landed before the first public tag and there is no prior
changelog to preserve the sequence.

| Date | History reviewed | Main movement |
| --- | ---: | --- |
| 2026-04-29 | 120 non-merge commits | Initial docs, Rust skeleton, forbidden-dependency audit, config, workspace discovery, models, IDs, response envelopes, DB connection, CLI parser, budget/context groundwork, search/pack/CASS scaffolds. |
| 2026-04-30 | 154 non-merge commits | Global CLI flags, output formats, command manifest, error schema, status/check/capability posture, TOON, migration helpers, provenance, policy, trust, test helpers, fuzz/property scaffolds, CASS robot contracts. |
| 2026-05-01 | 33 non-merge commits | Planner recipes, install/update recovery docs, local signing policy, Mermaid/certificate/counterfactual lab readiness, trust and lifecycle documentation. |
| 2026-05-02 | 63 non-merge commits | Criterion benchmark plan, performance stream, degraded/offline scenarios, causal/graph/lab work, release readiness gates. |
| 2026-05-03 | 78 non-merge commits | Boundary migration, command side-effect/idempotency contracts, project-local skills, mechanical command inventory, redaction/evidence traceability. |
| 2026-05-04 | 185 non-merge commits | Reality-check and bug-finding wave; security fixes; curation/rule hardening; situation/focus/outcome surfaces; closure and boundary test expansion. |
| 2026-05-05 | 171 non-merge commits | Trauma guard, no-silent-fallback inventory, deadlock/race findings, workflow IDs, trust promotion, markdown escaping, verification and gap triage. |
| 2026-05-06 | 164 non-merge commits | Release CI, install script, daemon scaffold, recorder, tripwire, closure lint, vision coverage, append-only DB triggers, claim verification, RCH-aware proof work. |
| 2026-05-07 | 112 non-merge commits | Major implements-surface wave: audit, certificate, claim, demo, eval, handoff, preflight, support bundle, recorder, causal, review, swarm scale, init fixes. |
| 2026-05-08 | 210 non-merge commits | Query v1, graph/index, pack replay groundwork, no-silent-fallback hardening, workspace/reality docs, performance forensics, search/context fixes. |
| 2026-05-09 | 31 non-merge commits | Pack replay ledger, replay/diff CLI, evidence freshness, redaction egress, pack quality and performance proofs. |
| 2026-05-10 | 43 non-merge commits | Rule/memory/curation/workflow surfaces, export/playbook, graph centrality/refresh, index maintenance. |
| 2026-05-11 | 32 non-merge commits | Validity-window filtering, search/context/pack profile refinements, deterministic fixture repairs. |
| 2026-05-12 | 90 non-merge commits | Schema/degraded/env-var catalog work, migration contracts, determinism fixes, validity/tombstone behavior, acceptance gate cleanup. |
| 2026-05-13 | 61 non-merge commits | Backup manifest v2, Bayesian posterior math, handoff HMAC, build admission, graph accretion, config and pack docs. |
| 2026-05-14 | 80 non-merge commits | Graph typed subgraphs, algorithm witnesses, closure-lint/test tracing, performance hardware manifest, status/check/golden rebaseline, release prep. |
| 2026-05-15 | 129 non-merge commits | `v0.1.0` tag day: graph G1-G10 surfaces, Pack DNA, insights, trauma guard, MCP/serve honesty, db inspect, read pool/singleflight/durability, verify orchestrator, README invariant gates. |
| 2026-05-16 | 748 non-merge commits | Large post-tag mesh/graph/context/determinism/RCH/read-pool/write-owner hardening wave, cross-cutting defensive changes, incident recovery, symlink guards. |
| 2026-05-17 | 264 non-merge commits | Mesh, graph, RCH, hygiene, redaction, status, CASS, transaction recovery, witness retention, and parser hardening. |
| 2026-05-18 | 133 non-merge commits | Workspace hygiene, preflight pattern expansion, search plan-cache diagnostics, tripwire widening, dependency refresh, graph/help/docs fixes. |
| 2026-05-19 | 373 non-merge commits | Graph rendering/help/perf tie-breakers, mesh foreground CLI, shard fan-out skeleton, symbol graph scaffold, PPR/NUMA/cache, schema and contract expansion. |
| 2026-05-20 | 242 non-merge commits | Mesh/Tailscale auto-enrollment, doctor first-aid, flight recorder, QoS, PPR cache, HITS, Gomory-Hu, load-bearing surfaces, EQL plan cache, audit lane, closeout audits. |

## Known Gaps And Caveats

- This changelog distinguishes the `v0.1.0` git tag from a GitHub Release. No
  GitHub Release rows were returned during the audit.
- A number of README install paths are intentionally forward-looking: binary
  releases, Homebrew, and crates.io publication are not yet live release
  channels in the checked repository state.
- The working tree was already dirty before this changelog pass, including
  `README.md` and many source/docs/test files owned by other active work. This
  pass added new changelog docs only.
- Agent Mail coordination was attempted but the local service was in
  degraded/read-only archive parity state, so no file reservation could be
  acquired for these new docs.

## Delivered Capability Workstreams

Delivered capability sections are represented in the checked-in Beads tracker
rather than GitHub Issues.

Closed workstreams behind this changelog:

- Core local memory loop: workspace init, memory persistence, search, context
  packs, why explanations, status, stable envelopes, and provenance.
- Pack replay and freshness: deterministic ledgers, replay/diff, evidence
  freshness, redaction egress, quality evaluation, and support-bundle summaries.
- Graph-derived retrieval: typed graph snapshots, algorithm witnesses, insights,
  PPR, Pack DNA, causal explanations, structural health, structural decay,
  proximity, dominance, skyline, HITS, and deterministic graph tests.
- Safety and trust: trauma guard preflight, destructive-pattern fixtures, policy
  denial contracts, redaction, trust promotion checks, signing/certificates, and
  prompt-injection/instruction-like content guards.
- Operations and diagnostics: doctor/status/check/capabilities, backup/restore,
  support bundles, RCH-aware verification, disk/build admission, closure-lint,
  failure-mode catalog, schema drift, and release gates.
- Agent and swarm scale: swarm brief, next-action guidance, Agent Mail posture,
  workspace hygiene, QoS, flight recorder, mesh/Tailscale optionality, duplicate
  work detection, host profiles, and crowded-checkout ergonomics.

[Unreleased]: https://github.com/Dicklesworthstone/eidetic_engine_cli/compare/v0.1.0...main
[0.1.0]: https://github.com/Dicklesworthstone/eidetic_engine_cli/tree/v0.1.0
