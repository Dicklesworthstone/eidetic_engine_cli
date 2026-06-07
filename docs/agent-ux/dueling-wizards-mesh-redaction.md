# Dueling Wizards Mesh Redaction Contract

Bead: bd-1n0np.23.4
Manifest: tests/fixtures/contracts/dueling_wizards_mesh_redaction.json
Contract: tests/contracts/dueling_wizards_mesh_redaction.rs
Shell gate: scripts/check-tracing-fields.sh

This contract pins the mesh export posture for the new dueling-wizards storage
surfaces before their schema migrations land. Each field class listed by the
migration registry must have an explicit redaction class, mesh material lane,
and share-preview behavior. The default is conservative: omit raw sensitive
inputs or export only stable hashes and redacted previews until a workspace
peer policy grants a narrower lane.

The contract links three existing authority points:

- `tests/fixtures/contracts/dueling_wizards_migration_registry.json` owns the
  planned storage allocation list.
- `docs/mesh/peer_policy.md` owns lane grants, outbound decisions, and
  redaction postures.
- `src/policy/mod.rs` owns share previews, mesh export secret scans, content
  hashing, and export policy attestations.

`scripts/check-tracing-fields.sh` also validates the manifest as a
build-independent gate and reports the result under
`duelingWizardsMeshRedaction`. This shell gate is not a replacement for the
Rust contract; it exists so manifest drift is caught during static review when
RCH is blocked or a live checkout cannot compile.

## Field Classes

The mesh redaction manifest covers every planned backup asset kind:

- `memory_anchors` exposes paths, symbols, line ranges, or source hashes. Mesh
  export must hash exact anchor values and ship only redacted metadata. Its
  value material policy is `hash_or_redacted_anchor_value_only`: the only
  outbound value fields are `anchor_value_hash` and `redacted_anchor_value`,
  while `anchor_value`, `raw_anchor_value`, `raw_path`, `raw_symbol`,
  `raw_command`, and `raw_schema` are forbidden outbound fields.
- `typed_memory_fields` exposes kind, subtype, level, and validation-sidecar
  fields. Mesh export may describe the class but must redact values that could
  reveal local ontology or workspace-specific rules.
- `memory_sentinel_specs` can encode watch expressions, prompts, or local
  policy checks. The default posture is omit and deny outbound mesh export.
- `memory_sentinel_results` can reveal which local checks fired. Mesh export
  may publish result hashes and revision-notice metadata, not raw results.
- `attestation_bundles` should leave the node as bundle hashes, policy
  references, and redaction-safe attestations only.
- `query_miss_ledger` records failed information needs and can reveal strategy,
  product names, or private gaps. The default posture is omit and deny outbound
  mesh export.
- `workspace_generations` and `source_write_stats` are freshness signals. They
  may leave as hashed revision notices, never as raw local source paths.
- `pack_candidate_impressions`, `derived_outcome_evidence`, and
  `error_fingerprints` are evidence and curation signals. Mesh export may use
  stable hashes and redacted previews, while raw logs, prompts, bodies, and
  embeddings remain denied.

## Default Policy

The manifest intentionally allows no raw body or raw embedding export. A future
runtime implementation can only export a raw body or embedding after an
explicit `docs/mesh/peer_policy.md` allow/share decision and a source-specific
contract update. Until then, outbound callers must treat:

- `omit` as `meshExportPosture=deny`;
- `hash` and `redact` as `meshExportPosture=redact`;
- `payloadExportAllowed=false` for every class;
- `rawPayloadExportAllowed=false` for every class;
- `redactedPayloadRequired=true` whenever a redacted payload would be allowed.

Share previews are dry-run evidence, not export authorization. They must use
`SHARE_PREVIEW_SCHEMA_V1`, `SharePreviewCandidate.redaction_class`,
`build_share_preview`, `share_preview_hash`, `scan_mesh_export_subjects`, and
`MESH_EXPORT_POLICY_ATTESTATION_SCHEMA_V1` from `src/policy/mod.rs`.
For `memory_anchors`, share previews stay `hash_only`, and any future policy
change that would export raw values or move the asset into a payload lane must
use `required_for_any_raw_value_or_payload_lane_change`.

## Outbound Decision Examples

The manifest's `outboundDecisionExamples` array pins representative caller
decisions. These are examples for implementation and test harnesses, not
authorization to export raw data.

| Example | Asset kind | Requested material | Decision | Preview class | Payload/body/embedding export |
| --- | --- | --- | --- | --- | --- |
| `memory_anchor_hash_preview` | `memory_anchors` | `raw_anchor_value` | redact | `hash` | denied |
| `sentinel_spec_omit` | `memory_sentinel_specs` | `watch_expression` | deny | `omit` | denied |
| `typed_memory_field_redaction` | `typed_memory_fields` | `kind_and_validation_sidecar` | redact | `redact` | denied |

Each example must match its `fieldClasses` entry for export posture,
share-preview class, payload flags, and redacted-payload requirement. The set
also covers every allowed redaction class and every allowed export posture.

Local Cargo fallback is not valid proof for this initiative. Rust test proof
must run through the remote RCH gate; when RCH is blocked, static manifest,
formatting, and tripwire evidence may document the slice but cannot close a
Rust execution gate.
