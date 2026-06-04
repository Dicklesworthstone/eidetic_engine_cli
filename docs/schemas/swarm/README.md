# Swarm Schemas

This directory contains draft-07 JSON Schemas for the agent-facing swarm
surfaces. The filename is the canonical schema identifier plus `.json`, and the
`$id` field must end with that filename.

The shipped coordination schema is `ee.coordination_snapshot.v1`. Earlier plan
notes used `ee.coordination.snapshot.v1`; the underscore form matches the Rust
constant and emitted JSON.

The shipped fallback ledger schema is
`ee.coordination_fallback_evidence.v1`. `bd-1zb7k.13.2` added the ingest path,
idempotent ledger storage, redacted support-bundle summaries, and `ee why`
inclusion.

The shipped source-run watchdog schema is `ee.source_run_evidence.v1`.
`bd-12v87.1` defines the shared timeout, redaction, degraded, recovery, and
provenance contract; `bd-12v87.2` adds the shared Rust runner that emits it for
bounded external source commands.

The verification broker view schema is `ee.verification.broker_view.v1`.
`ee verify broker lookup --json` emits it as the derived broker block over
retained verification run records. It is marked shipped because `bd-6boyo.2`
is closed and the lookup surface is available in current builds.

The work-packet contract is `ee.swarm.work_packet.v1`. It is emitted by
`ee swarm work-packet --json` as a deterministic read-only onboarding artifact
composed from existing swarm brief and next-action evidence.

The planned claim-gate contract is `ee.swarm.work_packet.claim_gate.v1`. It is
the read-only `ee swarm work-packet --claim-gate --json` projection for
answering whether a selected candidate may be claimed. It is tracked by
`bd-1tlcd.1` and remains marked unshipped until the CLI surface is emitted by
current builds.

Each schema carries `x-ee-status` so agents can distinguish implemented
surfaces from documented future contracts. A schema with `"shipped": false`
must point at an open or in-progress Bead and must also set
`"available_in_build": false`.

Every schema has:

- one companion narrative in `docs/swarm/`
- one or more examples
- a row in `tests/swarm_schema_lifecycle.rs`
- a fixture entry in `tests/fixtures/swarm_schemas/all_examples.json`

Non-goals:

- These schemas do not make `ee` a scheduler, agent loop, or web service.
- These schemas do not require live Agent Mail, RCH, or network services.
- These schemas do not promote unimplemented surfaces as available.

Related degraded codes are documented in `docs/degraded_code_taxonomy.md`:

- `coordination_source_stale`
- `coordination_source_unavailable`
- `verification_evidence_not_found`
- `pack_assembly_slow`
- `pack_assembly_budget_exceeded`
- `pack_concurrent_limit_reached`

Unknown producer identity is represented in-band as
`producer.identity.status = "unknown"` or `"unobserved"` rather than as a
degraded code.
