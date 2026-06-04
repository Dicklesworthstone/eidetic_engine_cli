# Environment Attestation

`ee.environment_attestation.v1` is the schema-first contract for a read-only
readiness artifact that tells agents which sources are authoritative before
they claim work, close a bead, or treat remote verification as evidence. The
schema lives at
[`docs/schemas/ee.environment_attestation.v1.json`](schemas/ee.environment_attestation.v1.json).

The payload is deliberately narrower than a support bundle. It contains counts,
ids, statuses, path patterns, command templates, degraded codes, recovery
actions, and source references. It must not contain mail bodies, raw source
snippets, secrets, or unredacted home paths.

## Source Authority

Every entry in `sourceAuthority[]` describes one readiness source:

- installed binary command surface
- source tree HEAD and dirty state
- Beads DB/JSONL tracker state
- BV recommendation freshness
- Agent Mail MCP state and probe state
- RCH availability and source materialization
- build admission and local Cargo tripwire posture
- host profile, claim gate, file reservations, and support-bundle redaction

Each source has an `authority` value of `authoritative`, `advisory`,
`degraded`, `stale`, `unavailable`, or `contradicted`. A source can be useful
without being authoritative. For example, metadata-only Beads drift can remain
visible as degraded context without making ordinary `br` reads non-authoritative.

## Verdicts

`verdict` and `summary.environmentVerdict` use this stable vocabulary:

- `safe_to_claim`
- `coordinate_before_claim`
- `unsafe_due_to_conflict`
- `remote_verification_admitted`
- `proof_environment_blocked`
- `source_authority_ambiguous`
- `stale_binary_suspected`
- `tracker_stale`
- `local_cargo_bypass_detected`
- `unknown_insufficient_evidence`

`summary.sourceTestVerdict` is separate. It answers whether source compile/test
evidence exists and what it proved. RCH-E327, worker topology failures, source
materialization failures, and remote-required local fallback refusal are
environment/readiness blockers. They must be reported as
`environment_blocked_before_source`, not as `source_failed`.

## Recovery Actions

`recoveryActions[]` is structured. Each action has a priority, kind, optional
structured command, mutation flag, required substrate, and rationale. Mutating
actions remain explicit; the attestation itself is read-only and must not claim
Beads, reserve files, run Cargo, rebuild binaries, or send Agent Mail.

The intended flow is:

```bash
ee swarm brief --workspace . --include-rch --json
ee swarm work-packet --workspace . --include-rch --claim-gate --candidate <id> --json
ee schema export ee.environment_attestation.v1
```

The first implementation of a collector should consume existing read-only
surfaces rather than duplicate their internals. In particular, claim-gate
`sourceAuthority` is a summary projection; an environment attestation should
preserve the per-source inventory that explains why a source is authoritative,
advisory, stale, unavailable, or contradicted.
