# Source-Authority Snapshot (`ee.source_authority.snapshot.v1`)

Tracking bead: `bd-3w4pv.1` (contract) / `bd-3w4pv.2` (collectors, ships the
surface). Status: contract-first; `x-ee-status.shipped=false` until the
read-only collectors land.

The source-authority snapshot is the deterministic, redaction-safe record of
**what each coordination source actually said** when a claim gate or
unsafe-claim planner made a decision. It exists because crowded checkouts kept
collapsing distinct failure classes — a Beads read timing out, a stale-safe
fallback answering, an Agent Mail store in corrupt-recovery — into a single
ambiguous `candidate_not_found` or `source_unavailable`, which made fail-closed
behavior impossible to audit and impossible to test.

## Sources

Every snapshot carries one record per source, sorted by `sourceKind` ascending
byte order:

| `sourceKind` | What it covers |
| --- | --- |
| `agent_mail` | Reservations, inbox, roster, durability/recovery posture |
| `beads` | Tracker rows, ready queue, claim-candidate lookups |
| `bv` | Graph-aware triage ranking (advisory) |
| `git` | Branch, dirty-state, HEAD/origin divergence |
| `host_profile` | Resource pressure and host-class posture |
| `installed_binary` | Installed `ee` freshness vs source contract |
| `memory_drift` | EE memory vs tracker/coordination drift probes |
| `rch` | Remote-verification admission and proof posture |
| `support_bundle` | Optional embedded handoff/support-bundle evidence |
| `workspace_hygiene` | Dirty-path classification posture |

A source that was not consulted still appears with `state=unavailable` and a
`statusDetail` explaining why. Absence of a record is never meaningful.

## Source-state taxonomy

| `state` | Meaning | Authoritative? |
| --- | --- | --- |
| `ready` | Live read succeeded inside budget | Yes (unless contradicted) |
| `degraded_read_only` | Source answered but must not authorize mutation | No |
| `unavailable` | Absent, unreachable, or skipped; no evidence captured | No |
| `timed_out` | Budget exhausted before an answer | No |
| `stale_fallback` | Live read failed; bounded stale-safe snapshot answered | No (advisory) |
| `corrupt_recovery` | Store present but integrity-failed or mid-recovery | No |
| `contradicted` | Evidence conflicts with another source, unresolved | No |

Two invariants the taxonomy enforces:

1. **Timeout is not absence.** `timed_out` must never be collapsed into
   `unavailable`, and a candidate lookup that timed out must never be reported
   as `candidate_absent_confirmed`.
2. **Stale fallback is not live truth.** Evidence served from a stale-safe
   snapshot keeps `fallback.active=true` and the source stays
   non-authoritative, even when the fallback contains the candidate.

## Candidate evidence

When the snapshot is taken for a specific claim candidate, `candidateEvidence`
distinguishes the cases consumers historically conflated:

| `lookupOutcome` | Meaning |
| --- | --- |
| `candidate_present` | An authoritative source confirms the candidate |
| `candidate_absent_confirmed` | Every authoritative source answered; none has it |
| `candidate_lookup_unavailable` | The sources that could answer were unavailable |
| `candidate_lookup_timed_out` | Budget exhausted; absence NOT confirmed |
| `candidate_stale_fallback_only` | Only stale-safe fallback evidence has it |
| `candidate_contradicted` | Authoritative sources disagree |

`staleFallbackPresence` records candidate presence in fallback evidence
separately from live presence, so "present in stale-safe Beads but missing from
the live claim-gate packet because Beads timed out" is representable — and the
fixture `tests/fixtures/source_authority/candidate_beads_timeout.json` pins
exactly that case.

## Determinism and redaction

- Producers sort `sources` by `sourceKind`, evidence ids and contradictions by
  id, ascending byte order, before writing. Identical inputs produce identical
  snapshots (`provenanceHash` is the replay/dedup key).
- `redactionStatus` is pinned to `paths_counts_subjects_only_no_content`: the
  snapshot keeps states, counts, budgets, exit classes, evidence ids, hashes,
  and repair-command templates. It must not contain raw mail bodies, raw memory
  bodies, host-private absolute paths (`/Users/...`, `/home/...`), captured
  argv, or environment dumps. Repair commands are templates (for example
  `scripts/br_retry.sh actionable --json`), never replayed captures.

## Fixtures

| Fixture | Pins |
| --- | --- |
| `tests/fixtures/source_authority/all_source_states.json` | Every `state` value across the ten sources |
| `tests/fixtures/source_authority/candidate_beads_timeout.json` | Candidate in stale-safe Beads, live lookup timed out, gate fails closed |
| `tests/fixtures/source_authority/redaction_proof.json` | Redaction posture: forbidden content classes absent |

The contract test `tests/swarm_schema_lifecycle.rs`
(`source_authority_snapshot_contract_covers_source_state_taxonomy`) keeps the
schema, the taxonomy, and the fixtures from drifting.

## Non-goals

- The snapshot does not decide claims; the claim gate consumes it.
- It does not replace `ee.source_run_evidence.v1` (per-command run evidence);
  it aggregates per-source authority for one decision point.
- It never mutates Beads, Agent Mail, git, or the EE store.
