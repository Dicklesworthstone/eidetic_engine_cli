# Doctor failure modes: scored population and repair specs

This directory holds the tracked contract for `ee doctor` failure-mode (FM)
fixtures (bead `bd-2oh15`):

- `failure_mode_scores.jsonl` is the scored population of durable-state failure
  modes, one row per state object and failure class.
- `repair-specs/<subsystem>.md` holds one section per fixture in
  `tests/doctor_fixtures/manifest.json`.

`doctor_workspace/` (ignored) remains a runtime directory; nothing here depends
on it. `tests/doctor_fixtures_contract.rs` enforces the rules below.

## Labels

Every fixture carries exactly one label, in the manifest and in its spec:

| Label | Meaning | Counts as coverage |
| --- | --- | --- |
| REPAIR | Doctor detects the damage, `--fix` applies a repair, and undo restores the damaged baseline exactly. | yes |
| GUIDANCE-ONLY | Doctor detects the damage; `--fix` makes no write and reports it before and after. | yes |
| NOT-DETECTED | Real damage that doctor reports as healthy. The fixture pins the gap with an independent witness. | **no**: reported as a GAP |
| PINNED-DEFECT | Detected, but `--fix` behaves wrongly; the fixture pins the named defect bead until it is fixed. | **no**: reported as a GAP |
| UNRESOLVED | A real trigger has been attempted and did not yet produce the failure. | no |
| UNCLASSIFIED | Marker-only: no real trigger has been built. | no |
| OUT-OF-SCOPE | Not a doctor failure mode (a correct command outcome, or state another subsystem owns). Marker-only, never run, and it carries a manifest `scopeReason`: a category, the enumerated search, and file:line citations. | **no**: never a pass, listed on its own line |

A NOT-DETECTED or PINNED-DEFECT fixture passing means the gap is still there.
When a detector or fix lands, the fixture goes red on purpose and is relabelled.

The UNCLASSIFIED and UNRESOLVED counts are held to one shared pin in
`tests/doctor_fixtures/lib.sh` (`doctor_fixture_untested_ratchet`), which every
counting sub-harness enforces. The pin is exact in both directions, so it drops
in the same commit that classifies a fixture and never rises. The OUT-OF-SCOPE
set is pinned by exact id, so relabelling a fixture OUT-OF-SCOPE can never
satisfy the pin.

## Spec fields

Each `## <fm-id>` section lists, in this order: Label, Severity, Detector, Real
trigger, Repair, Undo, Oracle, Negative control, Pinned sha. None may be empty.
Severity starts with the manifest severity; when the scored row differs, the
line says so and gives the reason.

## Scoring method

The population was built from a blind inventory of doctor's reachable state: an
agent read the doctor checks, the error-code registry and the durable state
files, and listed which checks exist and which state no check reads. It did not
see the fixture manifest. Each state object was then crossed with the failure
classes missing, empty/truncated, corrupt-bytes, stale/generation-drift,
malformed-text, permission, locked/contended, resource-pressure,
external-tool-absent and external-tool-contract-mismatch, keeping only the
combinations that are reachable.

Severity rubric:

- **P0**: the failure can lose or silently corrupt durable memory data, or
  blocks every command.
- **P1**: the failure degrades a subsystem and is recoverable.
- **P2**: cosmetic or advisory.

Row fields: `fm_id`, `state_object`, `failure_class`, `severity`,
`rubric_clause`, `detected_by` (check and error code, or `null` when no check
reports it), `fixtures` (manifest ids that exercise it), `gap_reason` (why a
P0/P1 row has no fixture) and `severity_note` (why the score differs from a
mapped fixture's manifest severity).

## Limitations

- The scorer also wrote the P0 fixtures and had seen the manifest. Independence
  rests on the blind inventory, not on the scoring step.
- The failure-class list may be incomplete.
- Six manifest FMs were outside the blind inventory's doctor surface and are
  not in the population. An enumerated code search (bd-2oh15 c9985) then
  settled them. `fm-policy_safety-trauma-guard-policy-denied-exit-7` (a correct
  command outcome) and `fm-policy_safety-redaction-class-coverage-gap` (mesh
  lane policy) are OUT-OF-SCOPE. `fm-state_files-jsonl-tombstone-drift` (the
  FM-SF-02 family), `fm-agent_coordination-mcp-agent-mail-file-reservation-conflict`
  (FM-AC-01), `fm-state_files-workspace-ambiguous-multiple-candidates` and
  `fm-workspace_config-nested-ee-markers` are doctor-owned and still
  UNCLASSIFIED until their NOT-DETECTED fixtures are built.
- Five fixtures were P0 in the manifest but score P1: `index_corrupt`,
  `cass_not_found`, `rch-workers-all-blocked-by-pressure`,
  `snapshot-write-lock-held` and `merge-conflict-markers`. By the bd-2oh15
  c9891 ruling the rubric governs, so the manifest now says P1 for all five.
  `merge-conflict-markers` was settled by running status, remember, search
  and doctor on the conflicted workspace: only search fails. Each row's
  `severity_note` records the reason.
