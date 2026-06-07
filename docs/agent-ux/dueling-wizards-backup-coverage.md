# Dueling-Wizards Backup Coverage

This checklist is the human-facing companion to
`tests/fixtures/contracts/dueling_wizards_backup_coverage.json`. It is owned by
`bd-1n0np.23.2` and enforced by
`tests/contracts/dueling_wizards_backup_coverage.rs`.

The backup coverage registry must stay in lockstep with
`tests/fixtures/contracts/dueling_wizards_migration_registry.json`. Every new
dueling-wizards storage allocation that appears in the migration registry needs
a backup/export/restore asset plan before runtime schema work lands.

## Runtime Surface

`src/core/backup.rs` currently exposes the relevant backup surfaces:

- `records.jsonl` for canonical exported records.
- `manifest.json` for backup artifact metadata and hashes.
- `BackupCreateReport::derived` for optional derived assets captured at backup
  creation time.
- `BackupInspectReport::derived` for inspecting derived asset manifest entries.
- `BackupVerifyReport::checked_derived` for hash and size verification.
- `BackupRestoreReport::restored_derived` for restore-side copied derived
  assets.

The important rule for `bd-1n0np.23.2`: a missing new asset must degrade or fail
verification as an explicit backup issue. Silent loss is never acceptable.

For `memory_anchors`, raw anchor values are also never acceptable backup
payload. Backup artifacts must preserve the planned `memory_anchors` fields from
the migration registry, but the anchor material must stay in
`anchor_value_hash` and `redacted_anchor_value`; fields such as
`anchor_value`, `raw_anchor_value`, `raw_path`, `raw_symbol`, `raw_command`, and
`raw_schema` are forbidden.

## Coverage Requirements

Every planned asset kind must declare coverage for:

| Coverage | Required evidence |
| --- | --- |
| `backup_create` | How `ee backup create` captures the asset. |
| `backup_inspect` | How `ee backup inspect` shows the asset entry. |
| `backup_verify` | How hashes and sizes are rechecked. |
| `backup_restore` | How side-path restore materializes or validates the asset. |
| `manifest_rehash` | Which hash is included in the backup manifest. |
| `roundtrip_e2e` | Which e2e or contract path proves backup -> restore parity. |

The current runtime round-trip proof path is
`tests/e2e_backup_restore_roundtrip.rs`. Once the dueling-wizards runtime
schema tasks are implemented, their storage rows must extend that proof or the
future cross-cutting e2e owned by `bd-1n0np.23.6`.

## Asset Inventory

The machine-readable fixture is authoritative. The current plan covers the
asset kinds allocated by `bd-1n0np.23.1`:

- `memory_anchors`
- `pack_candidate_impressions`
- `derived_outcome_evidence`
- `error_fingerprints`
- `memory_sentinel_specs`
- `memory_sentinel_results`
- `typed_memory_fields`
- `attestation_bundles`
- `query_miss_ledger`
- `workspace_generations`
- `source_write_stats`

`memory_anchors` has an extra privacy contract in the fixture:
`rawAnchorValuesAllowed=false`, `valueMaterialPolicy=hash_or_redacted_only`,
`manifestRedactionClass=hash`, and
`restoreValidation=hashes_roundtrip_without_raw_values`. The serialized backup
field set must match the migration registry's planned `memory_anchors` columns
exactly so restore can validate hashes without reintroducing raw local paths,
symbols, commands, or schema names.

## Asset Coverage Matrix

The fixture's `assetCoverageMatrix` section is the accounting view for this
plan. It has one row per asset kind and mirrors the asset registry's
`storageClass`, `manifestMode`, `hashPolicy`, and `missingAssetFailure` fields.
The contract also checks the row counts for migration allocations, owner beads,
coverage surfaces, and privacy-forbidden fields.

`coverageStatus=full_surface_set_declared` means the asset declares all six
backup surfaces from the checklist. `roundTripEvidenceStatus` is currently
`planned_contract_only` for every row because runtime dueling-wizards schema
work has not landed yet. `privacyStatus=privacy_contract_enforced` applies to
`memory_anchors`; assets without a privacy contract use `not_applicable`.

Every row currently carries `mustClauses=10`, `tested=10`, `passing=10`,
`divergent=0`, `scoreMilli=1000`, and
`complianceStatus=declared_conformant`. If a future row diverges, the matrix
should make the gap visible instead of hiding it in prose.

## Failure Scenarios

The manifest's `failureScenarios` array pins examples that must degrade or fail
visibly instead of silently losing backup material:

| Scenario | Asset kind | Surface | Trigger | Expected failure |
| --- | --- | --- | --- | --- |
| `missing_derived_asset` | `error_fingerprints` | `backup_verify` | `derived_asset_missing` | `degraded_not_silent_loss` |
| `corrupt_derived_asset_hash` | `memory_sentinel_results` | `backup_verify` | `derived_asset_corrupt` | `degraded_not_silent_loss` |
| `restore_manifest_rehash_mismatch` | `workspace_generations` | `manifest_rehash` | `manifest_hash_mismatch` | `degraded_not_silent_loss` |
| `raw_anchor_value_present` | `memory_anchors` | `backup_create` | `raw_anchor_value` | `degraded_not_silent_loss` |

Each scenario must point at a real backup asset kind, one of the required
coverage surfaces, an existing runtime anchor, `hashPolicy=blake3_required`,
and non-empty round-trip evidence. The raw-anchor scenario must keep
`privacyRequirement=rawAnchorValuesAllowed=false` and use a trigger that appears
in the `memory_anchors.privacyContract.forbiddenFields` list.

Local Cargo fallback is not valid proof for backup coverage.
