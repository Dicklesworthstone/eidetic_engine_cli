# Dueling-Wizards Migration Sequencing

This registry is the human-facing companion to
`tests/fixtures/contracts/dueling_wizards_migration_registry.json`. It is
owned by `bd-1n0np.23.1` and enforced by
`tests/contracts/dueling_wizards_migration_registry.rs`.

The current compiled migration tail in `src/db/mod.rs` is `V066`. Every
dueling-wizards schema task must allocate from the registry before adding a
runtime migration, then keep the runtime `MIGRATIONS` array contiguous. The
registry is a plan artifact, not a substitute for the real migration constants.

## Policy

The migration plan is forward-only and idempotent. Reversible behavior is
allowed where the existing migration surface supports safe round trips, but
rollback must never be required for ordinary repair. A task that adds durable
or derived storage must also name the backup/export/restore asset class and the
boundary migration coverage path before source work starts.

Do not reuse migration numbers. If the compiled tail moves past `V066`, update
this registry in the same change that adds the runtime migration.

## Planned Allocations

| Version | Allocation | Bead | Durable scope |
| --- | --- | --- | --- |
| `V066` | `memory_anchors` | `bd-1n0np.3.2` | Implemented typed anchors for paths, commands, env vars, schemas, degraded codes, dependencies, and config keys. |
| `V067` | `pack_candidate_impressions` | `bd-1n0np.2.2` | Pack selected/omitted candidate impressions. |
| `V068` | `outcome_evidence_rows` | `bd-1n0np.2.3` | Derived outcome evidence rows from verification, Beads, commit, and recorder sources. |
| `V069` | `error_fingerprints` | `bd-1n0np.4.3` | Error fingerprints plus repair, proof, and outcome links. |
| `V070` | `memory_sentinel_specs` | `bd-1n0np.16.2` | Declarative sentinel specifications attached to memories. |
| `V071` | `memory_sentinel_results` | `bd-1n0np.16.2` | Sentinel check results and hashes. |
| `V072` | `typed_memory_kind_sidecar` | `bd-1n0np.12.1` | Optional validated per-kind memory JSON sidecar fields. |
| `V073` | `attestation_bundles` | `bd-1n0np.22.1` | Canonical attestation bundle rows and bundle item hashes. |
| `V074` | `query_miss_ledger` | `bd-1n0np.6.3` | Redacted low-utility query miss ledger with TTL posture. |
| `V075` | `workspace_generations` | `bd-1n0np.8.2` | Monotonic workspace and derived-asset generation state. |
| `V076` | `source_write_stats` | `bd-1n0np.8.5` | Per-source write-stream statistics for write-immune quarantine decisions. |

## Transition Matrix

The manifest's `transitionMatrix` mirrors the allocation table one-for-one.
This is the implementation gate: `implemented` rows must name the compiled
migration constant and stay at or behind the current compiled tail (`V066` at
the time of this registry). `planned` rows must stay ahead of the compiled tail
and keep `migrationConstant`, `boundaryMigrationEvidence`, and
`backupCoverageEvidence` set to `required_before_implemented`.

All transition rows use `proofPosture: rch_only_no_local_fallback`. Moving an
allocation from `planned` to `implemented` requires the runtime migration,
boundary migration coverage, backup coverage, and RCH-only proof to land in the
same change.

### `V066_MEMORY_ANCHORS` Implemented Shape

The `memory_anchors` allocation is owned by `bd-1n0np.3.2`. Its registry entry
includes a `plannedShape` block that now matches the runtime V066 migration. The
columns are `memory_id`, `anchor_kind`, `anchor_value_hash`,
`redacted_anchor_value`, `confidence`, `source`, `provenance`,
`captured_span_hash`, `freshness_state`, `generation`, `created_at`, and
`updated_at`.

Required indexes are:

- `memory_id_anchor_kind_value_hash_unique`
- `anchor_kind_value_hash_lookup`
- `freshness_state_generation_lookup`

Raw anchor values are not durable public payloads. The storage policy is
`hash_required_raw_value_forbidden`, and mesh export is
`redacted_or_hashed_values_only`. Freshness changes use
`rank_down_only_no_tombstone`, so symbol drift can demote or mark a memory for
revalidation but must not silently remove it. Writes are
`append_or_upsert_by_generation` to keep repeated extraction idempotent and
auditable.

## Coverage Rule

Each allocation must be covered by `scripts/e2e_boundary_migration.sh` and the
Rust migration boundary tests once it moves from `planned` to implemented
runtime code. The allocation must also be included in the backup/export/restore
asset manifest owned by `bd-1n0np.23.2`.

Local Cargo fallback is not valid proof for migration sequencing.
