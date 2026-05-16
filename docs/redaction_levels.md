# Redaction levels (`--redaction <level>`)

> **What this file is:** the canonical specification of `ee`'s
> `--redaction <level>` flag — the level vocabulary, per-level behavior
> matrix, per-surface defaults, and the round-trip symmetry property.
>
> **Bead:** [`bd-17c65.11.6`](../README.md) (K6 — Documentation epic).
> Composes with [B10 / `bd-17c65.2.9`](../README.md) (output-time
> snippet redaction) and the per-subsystem redaction helpers in
> `src/core/support_bundle.rs`, `src/core/swarm_brief.rs`,
> `src/core/lab.rs`, and `src/core/task_frame.rs`.
>
> **How to use this file:** as the SINGLE source of truth for level
> semantics. The unit/e2e test matrix at
> `tests/redaction_levels_unit.rs` (and the per-surface integration
> tests) MUST agree with the table below. The doc-consistency gate at
> `tests/redaction_levels_doc_consistency_test.rs` asserts the level
> enum, ordering, and per-surface default declarations are present
> here in canonical form.

## Level vocabulary (5 levels, ordered increasing-redaction)

```
none < minimal < standard < strict < paranoid
```

Every `--redaction` flag, config setting, and code path that names a
level MUST use exactly one of these five strings (lowercase, no
hyphen-variants, no synonyms). The
`tests/redaction_levels_doc_consistency_test.rs` gate enforces this.

## Per-level behavior matrix

| Level     | Secrets (api_key, jwt, password, etc.) | High-entropy tokens                 | Memory content body                          | Tags         | Audit details |
|-----------|----------------------------------------|-------------------------------------|----------------------------------------------|--------------|----------------|
| `none`    | passthrough                            | passthrough                          | passthrough                                  | passthrough  | passthrough    |
| `minimal` | redacted with `[REDACTED:<class>]`     | passthrough                          | passthrough                                  | passthrough  | hash only      |
| `standard`| redacted                                | redacted if entropy ≥ 4.0 bits/byte  | passthrough                                  | passthrough  | hash only      |
| `strict`  | redacted                                | redacted if entropy ≥ 3.5 bits/byte  | truncated to 200 chars + BLAKE3 of full body | passthrough  | hash only      |
| `paranoid`| redacted                                | redacted always                      | replaced with `content_hash` + `[REDACTED]`  | hashed       | omitted        |

Notes on the matrix:

- **`<class>`** in the redaction marker is the secret-detector class
  (e.g. `api_key`, `stripe_secret`, `aws_secret_access_key`, `jwt`,
  `password`, `oauth_token`, `private_key`, `ssh_key`,
  `service_account_json`). Detector classes are emitted by the
  `src/policy/mod.rs::redact_secret_like_content` pipeline through
  `SecretRedactionMatch::pattern_id` and `SecretRedactionReport::redacted_reasons`.
- **"hash only"** for audit details means the `audit.details` JSON
  field is replaced with `{"hash": "blake3:<16-hex>"}` covering the
  canonical-JSON serialization of the original details.
- **Memory content `+ BLAKE3 of full body`** at `strict` lands as
  a sibling field `content_hash` rather than mutating the truncated
  text, so a downstream tool can verify "what was this memory" without
  the body.
- **Tags hashed** at `paranoid` replaces each tag string with
  `tag_<blake3-prefix>` so the cardinality is still observable but
  the literal labels are unreadable.

## Per-surface defaults

| Surface           | Default level | Rationale                                                       | Override status                 |
|-------------------|---------------|-----------------------------------------------------------------|---------------------------------|
| `ee export`       | `standard`    | Round-trip safe; preserves shape for re-import.                  | current `--redaction <level>`   |
| `ee handoff create`| `standard`   | Handoff capsules are redaction-safe artifacts.                 | current `--redaction <level>`   |
| `ee context --json`| `minimal`    | Agent-facing; minimal interference with retrieval intent.        | current `--redaction <level>`   |
| `ee support bundle`| `paranoid`   | Third-party-facing; max safety for bug-report uploads.           | current `--redaction <level>`   |
| `ee why`          | `none`        | Forensic surface; must show the actual stored content.           | no override planned             |

Current implementation note: as of this K6 slice, the canonical five-level
`--redaction <level>` CLI vocabulary is implemented for export/backup-style
JSONL artifacts, `ee handoff create`, `ee context --json`, and
`ee support bundle`. Support bundles apply the level as a final diagnostic
redaction pass: `none`/`--include-raw` keeps collected diagnostics raw,
`minimal` applies only the secret detector, and `standard`/`strict`/`paranoid`
apply the support-bundle diagnostic redactor for secret-like values and
path-like segments. Some nested support-bundle sections are already
summary-only before this final pass, so higher levels may have identical output
when no additional path-like or secret-like text is present.

Per-workspace defaults live in `.ee/config.toml`:

```toml
[redaction.defaults]
export         = "standard"
handoff_create = "strict"
context_json   = "minimal"
support_bundle = "paranoid"
```

The override precedence is:
CLI flag → workspace config → built-in default. No `EE_REDACTION_*`
redaction-level override is currently registered; adding one must update
both `src/config/env_registry.rs` and `docs/env_vars.md` in the same change.

### Response metadata status

Current JSON surfaces expose the effective level with existing fields such as
`redactionLevel` and, where available, `redactionSummary`. They also expose a
source-aware response block so callers can distinguish CLI overrides from
workspace config and built-in defaults:

```json
"redaction": {
  "level_applied": "standard",
  "level_source": "cli",
  "fields_redacted": ["items[0].content"],
  "patterns_matched": ["api_key"]
}
```

`level_source` is one of `cli`, `workspace_config`, or `built_in_default`.
`fields_redacted` and `patterns_matched` are intentionally honest: surfaces
that cannot yet produce field-level detail emit an empty array instead of
fabricating precision. `redactionLevel` remains during the transition, but
callers should prefer the `redaction.level_applied` and
`redaction.level_source` fields for new integrations.

## Round-trip symmetry property

`ee export --redaction <level> --output-dir <a>` followed by
`ee import jsonl --source <a>` MUST produce a workspace where:

1. **Non-redacted fields are byte-identical** to the original
   workspace. The export → import round trip is lossless for any
   field that wasn't classified for redaction at the chosen level.
2. **Redacted fields carry `redaction_markers[]`** in the imported
   memory's metadata. Each marker is
   `{ class: "api_key|...", level: "minimal|...", emitted_at: "<rfc3339>", original_hash: "blake3:..." }`.
   The marker records what was redacted without preserving the original
   content. A future un-redaction step (e.g., from a separately-stored
   key vault) could in principle restore the original, but un-redaction
   itself is OUT OF SCOPE for v0.2.
3. **Audit chain shows `redaction.apply` rows** for every redaction
   event. The audit row carries `level`, `surface`, `class`,
   `memory_id`, and `redaction_marker_id` so the imported workspace's
   audit history is complete.

This property holds for ALL 5 levels (including `none` — trivially —
and `paranoid` — where re-import is still possible, just with all the
secrets gone and tags hashed).

## Failure modes (composes with J6)

Four failure-mode fixtures are pinned under
`tests/fixtures/failure_modes/`:

- **`redaction_pattern_matched.json`** (severity `medium`) — emitted
  on every redaction that triggers a detector class match.
- **`redaction_level_invalid.json`** (severity `low`, error envelope)
  — emitted when `--redaction <not-a-level>` is passed; the error
  envelope's `error.details.acceptedValues` carries the canonical 5
  levels.
- **`redaction_round_trip_marker_preserved.json`** (severity `info`)
  — emitted by `ee import` when one or more `redaction_markers[]` are
  preserved on import; informational confirmation of the round-trip
  property.
- **`redaction_uncertain.json`** (severity `warning`) — emitted when
  a redaction-sensitive surface cannot confidently prove whether a field
  is safe, such as performance-forensics comparisons over redacted or
  partially redacted artifacts.

## Test plan (target — implementation lands in a sub-bead)

- **Unit:** `tests/redaction_levels_unit.rs` — 20-row matrix (5 levels
  × 4 surfaces) that asserts per-row output exactly matches the table
  above. Each row carries a `tests/fixtures/secrets/pre_redaction.jsonl`
  fixture pattern as the seed memory.
- **Round-trip:** `tests/contracts/backup_import_roundtrip.rs`
  (registered by `tests/contracts.rs`) — for each level, seed → export
  → import → assert the symmetry property.
- **E2E:** `scripts/e2e_overhaul/policy_detectors.sh` K6 block —
  exercises every level end-to-end against the real binary, asserts
  the per-surface defaults, and verifies the audit chain shows
  `redaction.apply` rows.
- **Doc consistency (THIS BEAD'S TEST):**
  `tests/redaction_levels_doc_consistency_test.rs` — parses this file,
  asserts the 5-level enum and per-surface default table are present
  in canonical form.

## Logging event (composes with J1 / `ee.test_event.v1`)

```json
{
  "schema": "ee.test_event.v1",
  "kind": "redaction_apply",
  "level": "none|minimal|standard|strict|paranoid",
  "surface": "export|handoff|context|support_bundle|why|other",
  "memory_id": "<id|null>",
  "fields_redacted_count": 0,
  "patterns_matched": ["api_key", "stripe_secret"],
  "tokens_truncated": 0,
  "content_hash_original": "blake3:<hex>",
  "audit_row_id": "<id|null>"
}
```

The `kind: "redaction_apply"` event is pinned in the test-event
schema at `docs/schemas/test_event_v1.json` so log consumers can
validate it.

## Cross-references

- [B10 (`bd-17c65.2.9`)](../README.md) — output-time snippet redaction
  is the secret-detection foundation that K6's level matrix layers on.
- [`tests/fixtures/failure_modes/SCHEMA.md`](../tests/fixtures/failure_modes/SCHEMA.md)
  — fixture catalog format for the redaction failure modes.
- [`docs/degraded_code_taxonomy.md`](degraded_code_taxonomy.md) —
  `redaction_uncertain` and related codes classified as `response_time`.
- [`docs/env_vars.md`](env_vars.md) — the canonical registry for `EE_*`
  overrides if a future redaction-level env override is added.
