# Verification Broker View

Schema: `ee.verification.broker_view.v1`

Tracking bead: `bd-6boyo.2`

This schema defines the compact operator view emitted by
`ee verify broker lookup --json`. It sits on top of retained
`ee.verification.run.v1` records and does not replace the base verification
ledger. It maps prior run evidence into agent-facing states:
`reusable`, `stale`, `incompatible`, `in_progress`, `known_blocker`, and
`unavailable`.

The view is intentionally small and redaction-safe. It carries command and
source fingerprints, target profile, execution substrate, optional RCH worker
metadata, compatibility reason codes, stale reason codes, and a hashed
first-failure reference for known blockers. It does not include raw stdout,
raw stderr, compiler logs, mail bodies, secrets, or source excerpts.

## Status Semantics

| Status | Meaning | Suggested action |
| --- | --- | --- |
| `reusable` | A prior run matched source, command, substrate, and environment class and exited 0. | `cite_existing_run` |
| `known_blocker` | A matching or source-compatible prior run failed. | `inspect_known_blocker` |
| `in_progress` | A matching run exists but has no final exit code yet. | `wait_for_in_progress_run` |
| `stale` | The command and substrate match, but the source fingerprint differs or is unavailable. | `rerun_current_source` |
| `incompatible` | The source and substrate match, but the command fingerprint differs. | `adjust_command_or_profile` |
| `unavailable` | No imported verification run record can answer the request. | `import_or_run_verification` |

## Reason Codes

Compatibility reason codes are intentionally terse so agents can branch without
parsing prose:

| Code | Meaning |
| --- | --- |
| `source_match` | Requested source fingerprint matches the run record. |
| `command_match` | Requested command hash matches the run record. |
| `substrate_match` | Requested execution substrate matches the run record. |
| `env_class_match` | Requested environment fingerprint class matches the run record. |
| `nonzero_exit_code` | Exact matching run has a recorded nonzero exit code. |
| `prior_nonzero_exit_code` | A source-compatible prior run failed under a different command. |
| `no_final_exit_code` | Matching run has no final exit code. |
| `no_matching_record` | No imported record matched source, command, or substrate closely enough. |

Stale reason codes explain why a prior run cannot be reused directly:

| Code | Meaning |
| --- | --- |
| `source_hash_mismatch` | Command/substrate matched, but source fingerprint did not. |
| `command_hash_mismatch` | Source/substrate matched, but command fingerprint did not. |

## Non-goals

- It does not launch Cargo, RCH, or any verifier.
- It does not mutate Beads, Agent Mail, Git, or the verification ledger.
- It does not fork `ee.verification.run.v1`; it is a derived view.
- It does not make `ee` an agent scheduler or build coordinator.

## Lookup Inputs

`ee verify broker lookup --json` accepts retained records from either
`--records-json <path>` (a JSON array of `ee.verification.run.v1` records) or
`--runs-jsonl <path>` (J1 test-event JSONL with artifact-manifest events). When
neither source is supplied, it returns an `unavailable` broker view instead of
launching a verifier.

## Operator Workflow

Run broker lookup before spending a fresh RCH slot on common closeout checks:

1. Query the broker with the intended command hash, source fingerprint, command
   class, and substrate.
2. If the result is `reusable`, cite the matched run ID, worker, artifact
   manifest hash, source fingerprint, and command hash in the Beads closeout.
3. If the result is `known_blocker`, inspect the redacted
   `firstFailureSummaryRef`, coordinate with the owner or remediation bead, and
   do not rerun RCH unless source state, command scope, worker topology, or the
   blocker TTL changed.
4. If the result is `in_progress`, wait for the owning verifier or coordinate
   before launching another equivalent command.
5. If the result is `stale`, rerun only against the current source state and
   record the new artifact manifest.
6. If the result is `unavailable` or `incompatible`, import retained J1 logs
   first when available; otherwise run the narrowest RCH command that can prove
   the bead.

Suggested commands must prefer reuse or coordination before RCH. The broker does
not override Beads, BV, Agent Mail, Git reservations, or RCH admission policy.

## Command Examples

All examples below are lookup shapes. They do not run Cargo, launch RCH, mutate
the ledger, or claim a bead.

```bash
ee verify broker lookup \
  --runs-jsonl artifacts/j1.jsonl \
  --bead-id bd-example \
  --command-class cargo_check_all_targets \
  --command-hash blake3:all-targets-command \
  --source-hash blake3:current-tree \
  --execution-substrate rch \
  --env-fingerprint-class class:external_cargo_target \
  --json
```

```bash
ee verify broker lookup \
  --runs-jsonl artifacts/j1.jsonl \
  --bead-id bd-example \
  --command-class cargo_test_focused \
  --command-hash blake3:focused-unit-command \
  --normalized-argv-hash blake3:focused-unit-argv \
  --source-hash blake3:current-tree \
  --execution-substrate rch \
  --target-profile debug \
  --json
```

```bash
ee verify broker lookup \
  --runs-jsonl artifacts/j1.jsonl \
  --bead-id bd-example \
  --command-class e2e_harness \
  --command-hash blake3:e2e-harness-command \
  --source-hash blake3:current-tree \
  --execution-substrate rch \
  --json
```

```bash
ee verify broker lookup \
  --runs-jsonl artifacts/j1.jsonl \
  --bead-id bd-example \
  --command-class cargo_clippy_all_targets \
  --command-hash blake3:clippy-command \
  --source-hash blake3:current-tree \
  --execution-substrate rch \
  --json
```

For a stale dirty-tree case, keep the old command hash but pass the current
source fingerprint. A returned `stale` view means the old artifact is useful
context, not proof for the current checkout.

```bash
ee verify broker lookup \
  --runs-jsonl artifacts/j1.jsonl \
  --bead-id bd-example \
  --command-class cargo_test_focused \
  --command-hash blake3:focused-unit-command \
  --source-hash blake3:dirty-current-tree \
  --execution-substrate rch \
  --json
```

## Failure-Mode Catalog Mapping

The broker uses in-band statuses for most lookup outcomes. Do not invent new
`degraded[]` codes for statuses that are already represented by
`ee.verification.broker_view.v1`.

| Operator case | Current representation | Fixture-backed catalog entry |
| --- | --- | --- |
| Broker evidence unavailable | `status: "unavailable"`, `compatibilityReasonCodes: ["no_matching_record"]`, `suggestedAction: "import_or_run_verification"` | `verification_evidence_not_found` covers linked-memory `why` evidence gaps; broker lookup itself is in-band. |
| Artifact manifest stale or missing | `status: "stale"` for source drift; verification posture may report `artifact_manifest_missing` as an evidence-health reason. | Broker stale is covered by `tests/fixtures/golden/verification/broker_views.json.golden`; no top-level degraded fixture is emitted until this reason is promoted to `degraded[]`. |
| RCH posture unavailable | Swarm brief/RCH posture reports `rch_unavailable`, `rch_remote_required_fallback_prevented`, or `rch_worker_topology_blocked`. | Existing failure-mode fixtures under `tests/fixtures/failure_modes/` cover all three codes. |
| First-failure redacted | `firstFailureSummaryRef.rawOutputIncluded` is `false`; hashes such as `stderrExcerptHash` and `artifactManifestHash` replace raw logs. | Broker redaction is covered by the broker-view golden and `verification_evidence_schema_unit`; internal compile-attribution fallback is documented by the retired `unattributed_compile_blocker` fixture. |

If a future implementation promotes any broker status or evidence-health reason
to a top-level response `degraded[]` entry, it must land a
`tests/fixtures/failure_modes/<code>.json` fixture in the same commit.
