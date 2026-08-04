# Dueling-Wizards Migration Sequencing

This registry is the human-facing companion to
`tests/fixtures/contracts/dueling_wizards_migration_registry.json`. It is
owned by `bd-1n0np.23.1` and enforced by
`tests/contracts/dueling_wizards_migration_registry.rs`.

The current compiled migration tail in `src/db/mod.rs` is `V093`. The next
planned allocation starts at `V094`. Every dueling-wizards schema task must
allocate from the registry before adding a runtime migration, then keep the
runtime `MIGRATIONS` array contiguous. The registry is a plan artifact, not a
substitute for the real migration constants.

The migration number line is shared with non-initiative workstreams:
`V073_ERROR_REPAIR_LINKS` (an extension of the `error_fingerprints` allocation
scope), `V074_JOURNAL_ENTRIES`, `V075_REMEMBER_IDEMPOTENCY_KEYS`,
`V076_MEMORY_ANCHOR_INDEX`, `V077_PRIMER_CACHE`, `V078_PACK_BASELINES`,
`V079_SITUATION_RECORDS` (bd-1tp6p.2.1 persisted situation storage),
`V080_WORKSPACE_GENERATION_FLOOR_REBUILD` (forward-only V071 bootstrap repair),
`V081_AUDIT_LOG_WORKSPACE_TIMELINE_INDEX`, `V082_MEMORY_DEBT_SNAPSHOTS`,
`V083_ERROR_FINGERPRINT_GENERATION_TRIGGERS` (forward-only V072 generation
repair), `V084_PACK_RECORD_PROFILE_DOMAIN` (forward-only canonical pack
profile/check repair), `V085_EVIDENCE_SECURITY_POSTURE` (forward-only
fail-closed evidence admission and provenance-integrity repair),
`V086_RULE_INDEX_GENERATIONS` (forward-only procedural-rule generation
tracking and source-row repair), `V087_EVIDENCE_STORAGE_REBUILD`
(forward-only canonical evidence-table rebuild and generation repair),
`V088_MESH_LANE_GRANT_STATES` (durable exact-peer lane consent with monotonic
grant generations and target-rotation invalidation), and
`V089_MESH_LANE_GRANT_CONFIG_BINDINGS` (forward-only immutable-V088 repair
that binds widened lanes to exact config bytes and invalidates legacy unbound
allows), and the ADR 0086 TC-D7 trust CHECK rebuilds
`V090_MEMORY_PEER_HUMAN_ATTESTED_TRUST`,
`V091_CURATION_PEER_HUMAN_ATTESTED_TRUST`,
`V092_PROCEDURAL_RULE_PEER_HUMAN_ATTESTED_TRUST`, and
`V093_PACK_ITEM_PEER_HUMAN_ATTESTED_TRUST` landed
between the initiative's implemented allocations and the compiled tail.
Implemented allocations therefore record historical fact (two
allocations may share one compiled migration, as the sentinel pair does under
`V069_MEMORY_SENTINELS`), while planned allocations stay strictly contiguous
from `nextPlannedMigration`.

## Policy

The migration plan is forward-only and idempotent. Reversible behavior is
allowed where the existing migration surface supports safe round trips, but
rollback must never be required for ordinary repair. A task that adds durable
or derived storage must also name the backup/export/restore asset class and the
boundary migration coverage path before source work starts.

Do not reuse migration numbers. If the compiled tail moves past `V093`, update
this registry in the same change that adds the runtime migration.

### V085 legacy-evidence remediation

`V085_EVIDENCE_SECURITY_POSTURE` immediately denies pre-migration evidence and
removes its raw upstream references. It does not silently infer trust or
rewrite stored excerpts. An operator can then re-screen legacy rows through the
bounded maintenance job:

```bash
# Apply the fail-closed posture before requesting a rescreen preview.
ee migrate run --workspace . --json

# Preview the next deterministic batch; this never writes evidence or audit rows.
ee job run evidence_rescreen --workspace . --item-limit 500 --dry-run --json

# Apply exactly one preview-sized batch in a transaction.
ee job run evidence_rescreen --workspace . --item-limit 500 --json
```

Repeat the apply command while `pendingAfter` is greater than zero. Each
rewritten row gets one redaction-safe `evidence.security_rescreen` audit record;
reruns skip completed rows. Recognized producers are re-screened under the
current policy, supporting evidence remains denied from direct retrieval, and
ambiguous, malformed, instruction-like, or cross-workspace rows remain
quarantined. The report exposes IDs, dispositions, reason codes, and counts,
never the raw excerpt or upstream path.

Applying the job can permanently replace secrets in legacy excerpts with
redaction markers, while the V085 migration itself permanently replaces raw
upstream references. Before migrating or applying the job, create a protected
offline snapshot of the entire workspace `.ee` state if exact byte-for-byte
recovery is required. Stop all writers first and keep the database, WAL, and
shared-memory files together. The normal `ee backup create` artifact is still
recommended for logical source-of-truth recovery, but it is intentionally
redacted and does not preserve raw legacy evidence as an in-place rollback
mechanism.

There is no automatic in-place rollback because restoring the old evidence
would reintroduce the unsafe bytes. If recovery is required, validate the
protected snapshot in an isolated side path and make the restore decision
explicit. After the final applied batch—and after any explicit restore—rebuild
the derived index so stale pre-remediation bytes cannot remain searchable:

```bash
ee index rebuild --workspace . --json
```

`V087_EVIDENCE_STORAGE_REBUILD` materializes the full post-V085 evidence row
shape in one canonical table. It copies every row in deterministic row order,
recreates the evidence indexes and integrity/generation triggers, and advances
the generation of each workspace containing evidence. The migration does not
grant admission or reinterpret legacy provenance; V085-denied rows remain
denied until the explicit bounded rescreen above evaluates them. Because the
table replacement is forward-only, the same protected offline-snapshot posture
applies before migration when byte-for-byte recovery is required.

### V089 immutable-V088 repair

`V088_MESH_LANE_GRANT_STATES` remains byte-for-byte identical to its first
committed definition. `V089_MESH_LANE_GRANT_CONFIG_BINDINGS` performs the
approved-config binding as a new forward-only table rebuild. It accepts the
official 13-column V088 table and the one briefly shipped 19-column physical
shape, copies only the shared V088 columns, and produces one canonical table
with six lane-specific approval-digest columns and cross-column constraints.
The rebuilt target also aligns `peer_id` with the published opaque-ID grammar
`^peer_[A-Za-z0-9._:-]{6,128}$`, including its minimum and maximum lengths;
the runtime target adapter applies the same bound before emitting grant data.

An old `allow` has no trustworthy binding to the current config bytes. V089
therefore converts each such lane to explicit `deny`, clears every approval
digest, advances that row's consent generation once (saturating at SQLite's
integer maximum), and refreshes its update timestamp. Existing `deny`,
`quarantine`, and inherited `NULL` values remain restrictive or inherited as
before. Migration-history validation recognizes only the exact computed
checksum and exact audit label from the accidental rewritten V088; any other
V088 checksum remains migration drift. Rerunning migration is idempotent and
does not rewrite either the V088 history record or already canonical rows.

## Allocations

| Version | Allocation | Status | Bead | Durable scope |
| --- | --- | --- | --- | --- |
| `V066` | `memory_anchors` | implemented | `bd-1n0np.3.2` | Implemented typed anchors for paths, commands, env vars, schemas, degraded codes, dependencies, and config keys. |
| `V067` | `pack_candidate_impressions` | implemented | `bd-1n0np.2.2` | Pack selected/omitted candidate impressions. |
| `V068` | `outcome_evidence_rows` | implemented | `bd-1n0np.2.3` | Derived outcome evidence rows from verification, Beads, commit, and recorder sources. |
| `V069` | `memory_sentinel_specs` | implemented | `bd-1n0np.16.2` | Declarative sentinel specifications attached to memories (landed with the results table in `V069_MEMORY_SENTINELS`). |
| `V069` | `memory_sentinel_results` | implemented | `bd-1n0np.16.2` | Sentinel check results and hashes (same compiled migration as the specs table). |
| `V070` | `typed_memory_kind_sidecar` | implemented | `bd-1n0np.12.1` | Optional validated per-kind memory JSON sidecar fields (landed as `V070_MEMORY_TYPED_FIELDS` on `memories`). |
| `V071` | `workspace_generations` | implemented | `bd-1n0np.8.2` | Monotonic workspace and derived-asset generation state. |
| `V072` | `error_fingerprints` | implemented | `bd-1n0np.4.3` | Error fingerprints plus repair, proof, and outcome links (`error_repair_links` landed separately as `V073_ERROR_REPAIR_LINKS`). |
| `V094` | `attestation_bundles` | planned | `bd-1n0np.22.1` | Canonical attestation bundle rows and bundle item hashes. |
| `V095` | `query_miss_ledger` | planned | `bd-1n0np.6.3` | Redacted low-utility query miss ledger with TTL posture. |
| `V096` | `source_write_stats` | planned | `bd-1n0np.8.5` | Per-source write-stream statistics for write-immune quarantine decisions. |

`V084_PACK_RECORD_PROFILE_DOMAIN` is covered by the FrankenSQLite regression
`db::tests::v084_pack_profile_rebuild_preserves_parent_children_indexes_and_order`.
It upgrades a populated V083 database, preserves the parent plus all four FK
children and indexes, checks FK/integrity posture and row admission order, and
proves all six canonical profiles plus `contradiction_suppressed` persist.

## Transition Matrix

The manifest's `transitionMatrix` mirrors the allocation table one-for-one.
This is the implementation gate: `implemented` rows must name the compiled
migration constant and stay at or behind the current compiled tail (`V093` at
the time of this registry). `planned` rows must stay ahead of the compiled tail
and keep `migrationConstant`, `boundaryMigrationEvidence`, and
`backupCoverageEvidence` set to `required_before_implemented`.

The implemented constants currently covered by the registry are
`V066_MEMORY_ANCHORS`, `V067_PACK_CANDIDATE_IMPRESSIONS`,
`V068_OUTCOME_EVIDENCE_ROWS`, `V069_MEMORY_SENTINELS`,
`V070_MEMORY_TYPED_FIELDS`, `V071_WORKSPACE_GENERATIONS`, and
`V072_ERROR_FINGERPRINTS`.

All transition rows use `proofPosture: rch_only_no_local_fallback`. Moving an
allocation from `planned` to `implemented` requires the runtime migration,
boundary migration coverage, backup coverage, and RCH-only proof to land in the
same change.

`scripts/e2e_cross_cutting.sh` statically pins the registry's conservative
posture before runtime proof is available. The shell gate pins the exact
implemented version layout (`V066` through `V072`, with the sentinel pair
sharing `V069`) plus the planned reservations (`V094` through `V096`),
transition rows mirror allocation rows by ID, version, and status, implemented
rows stay at or behind the compiled tail, planned rows stay at or beyond the
next allocation, and backup assets mirror each allocation's asset kind,
allocation ID, owner bead, hash policy, and fail-visible missing-asset policy.

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
