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
