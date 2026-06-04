# Swarm Schemas

This directory contains draft-07 JSON Schemas for the agent-facing swarm
surfaces. The filename is the canonical schema identifier plus `.json`, and the
`$id` field must end with that filename.

The shipped coordination schema is `ee.coordination_snapshot.v1`. Earlier plan
notes used `ee.coordination.snapshot.v1`; the underscore form matches the Rust
constant and emitted JSON.

## Redacted Agent Mail Snapshot Producer Contract

Tracking Bead: `bd-6qcwh.1`

`ee swarm brief --agent-mail-snapshot <path>` currently consumes a documented
parser-compatible snapshot shape rather than a separate shipped JSON Schema. The
producer contract is therefore:

- top-level arrays named `agents` or `agent_inventory`, `file_reservations` or
  `reservations`, `inbox` or `mailboxes`, and `threads`
- optional Agent Mail health fields such as `fallback_active`,
  `mcp_http_reachable`, `am_agents_list_ok`, `am_send_single_recipient_ok`,
  `am_send_multi_recipient_ok`, `semantic_readiness`, and `healthLevel`
- deterministic ordering by producer before writing, with consumers still
  sorting and deduplicating after parse
- redaction status `paths_counts_subjects_only_no_content`

This shape is intentionally not `ee.coordination_snapshot.v1`. The coordination
snapshot schema is the pack-level aggregate over Beads, Agent Mail, and other
coordination evidence. The Agent Mail snapshot producer is the lower-level
input that feeds `ee swarm brief`, workspace hygiene, and later pack
coordination conversion.

`ee.swarm.coordination_health.v1` is health evidence only. It may populate
Agent Mail degraded entries when passed as `--agent-mail-snapshot`, but it is
not a full Agent Mail snapshot because it does not carry agent roster entries,
active reservations, inbox summaries, or thread freshness. Operators must not
treat a green health event as proof that there are no reservations or unread
messages.

Required redaction posture:

- agent summaries may include agent name, role/program/model when already part
  of public Agent Mail metadata, last-active timestamp, contact policy, and
  bounded status counts
- reservation summaries may include path patterns, holder names, exclusivity,
  expiry timestamps, and reason labels; they must not include raw file contents
  or expanded file listings
- inbox summaries may include recipient/owner, message id, sender, created
  timestamp, ack-required flag, importance, kind, bounded subject-like metadata,
  and counts; they must not include raw mail bodies by default
- thread summaries may include thread id, participant names, latest timestamp,
  message count, ack-required count, and bounded subject-like metadata; they
  must not include full thread bodies
- producer diagnostics may include stable counts, degraded codes, readiness
  status, redacted command names, and hashes; they must not include full archive
  paths, raw database pages, secret-shaped strings, or host-private absolute
  paths unless redacted

Allowed producer sources are read-only Agent Mail commands or MCP reads:
agent list, active file-reservation list, inbox fetch without bodies by default,
bounded thread summaries, health checks, and semantic-readiness probes. Forbidden
operations for this producer are sending mail, acknowledging messages, marking
messages read, creating reservations, releasing reservations, repairing Agent
Mail storage, mutating Beads, mutating git, running Cargo, or deleting files.

Contract examples:

| Case | Required shape | Expected consumer posture |
| --- | --- | --- |
| healthy | non-empty or empty `agents`, `file_reservations`, `inbox`, and `threads`; readiness pass | Agent Mail source ready; zero reservations means "none observed" only for the captured scope |
| degraded health-only | `schema: "ee.swarm.coordination_health.v1"` plus health booleans, no roster or reservation arrays | degraded Agent Mail health evidence; not authoritative for reservations or unread mail |
| stale | captured timestamp or freshness metadata older than the selected staleness budget | degraded freshness; regenerate the snapshot before claiming work |
| reservation-conflict | `file_reservations` entry with overlapping `path_pattern`, holder, exclusive flag, and expiry | surface risk or claim gate should require coordination before editing |
| inbox-unavailable | roster/reservations present plus a degraded entry or diagnostic saying inbox read failed | reservations may be usable, but unread-message evidence is incomplete |
| semantic-readiness-failed | `semantic_readiness.status = "fail"` with a classified reason and health level | `agent_mail_semantic_readiness_failed`; reservation and inbox reads are not authoritative |

Implementation beads must add parser fixtures and logged tests covering the
examples above, stdout/stderr isolation for any producer command, redaction
assertions for mail bodies and secret-shaped strings, and a no-mock mutation
audit proving the producer leaves Agent Mail read/ack/reservation state
unchanged.

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
