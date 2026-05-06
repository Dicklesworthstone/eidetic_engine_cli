# Swarm-Scale Workload Model

`eidetic_engine_cli-fcq1.1` defines the first shared workload model for proving that `ee` stays responsive under many-agent usage. The workloads are deterministic fixtures, not production code paths. They give later benchmark, cache, write-governance, and support-bundle beads a common corpus shape and traffic vocabulary.

## Workload Families

The fixture manifest in `tests/fixtures/swarm_scale/workloads.json` covers these traffic families:

| Family | Pressure Mode | What It Exercises |
| --- | --- | --- |
| `read_heavy_context_burst` | Many concurrent `ee context` and `ee search` calls | Search index fanout, pack selection, provenance rendering, cache hit ratios |
| `remember_write_burst` | Many agents recording notes/outcomes | Single write-owner behavior, durable queueing, audit rows, backpressure |
| `cass_import_spike` | Session import bursts | CASS robot/JSON parsing, source provenance, ingestion batch sizing |
| `index_rebuild` | Derived index rebuilds | Frankensearch rebuild cost and index generation tracking |
| `graph_refresh` | Graph projection and centrality refresh | FrankenNetworkX projection size and stale-derived-state reporting |
| `daemon_maintenance` | Scheduled maintenance jobs | Asupersync cancellation, job budgets, durable job outcomes |
| `mixed_mode_swarm` | Readers, writers, imports, rebuilds, and maintenance together | Overall resource contention and support-bundle evidence |

## Scale Tiers

| Tier | Intended Use | Memory Count | Agent Count | CI Suitability |
| --- | --- | ---: | ---: | --- |
| `small` | Normal PR smoke and contract tests | 100 | 4 | `normal_ci` |
| `medium` | Nightly or pre-release validation | 5,000 | 16 | `nightly_ci` |
| `large` | Release candidate and 64-core local validation | 25,000 | 64 | `release_candidate` |
| `stress` | 256GB+/64-core swarm validation | 100,000 | 256 | `local_256gb` |

Every tier uses the same seed family (`seed.swarm_scale.v1`), stable public ID formula, fixed clock, hash embedding profile, and no synthetic secrets. This keeps generated memories reproducible without paid APIs or downloaded model state.

## Fixture Contract

The manifest is intentionally declarative. Each tier records:

- deterministic generation parameters: seed, fixed clock, ID prefix, first ID, and expected last ID;
- corpus shape: memories, CASS sessions, provenance links, graph edges, and derived index documents;
- resource profile: expected DB rows, index bytes, graph nodes/edges, RAM class, CPU class, and disk class;
- traffic mix: operation family, agent count, operation count, writer count, and expected artifacts;
- CI suitability: normal, nightly, release-candidate, or local high-resource validation.

The test `tests/swarm_scale_workloads.rs` validates the manifest rather than materializing 100,000 memories during normal test runs. Later beads can use this manifest to drive concrete generators and RCH-backed benchmark harnesses without changing the workload vocabulary.

## Follow-On Beads

- `eidetic_engine_cli-fcq1.2` should consume this manifest for RCH-friendly benchmark budgets.
- `eidetic_engine_cli-fcq1.3` should use the read-heavy and mixed-mode tiers for cache-prewarm validation.
- `eidetic_engine_cli-fcq1.4` should use the write-burst and mixed-mode tiers for write-owner/backpressure tests.
- `eidetic_engine_cli-fcq1.5` should cite the same tier names in explain-plan output.
- `eidetic_engine_cli-fcq1.6` should run the mixed-mode and stress profiles in logged E2E form.
