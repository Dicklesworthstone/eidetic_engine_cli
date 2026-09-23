# policy_safety repair specs

Repair specs for the `policy_safety` fixtures under `tests/doctor_fixtures/`.
Labels, fields and the coverage rule are defined in
[`docs/doctor/README.md`](../README.md).

## fm-policy_safety-trauma-guard-policy-denied-exit-7

- **Label:** OUT-OF-SCOPE (a correct command outcome, not a doctor failure mode; bd-2oh15 ruling on c9985)
- **Severity:** P1. Not in the scored population: a policy denial is a command outcome, not doctor-read state.
- **Detector:** None, by design. The trauma guard exists (`src/core/trauma_guard.rs`), and every `DomainError::PolicyDenied` exits 7 (`src/models/mod.rs:2285`, `:2304`). No doctor check reads trauma-guard denials: the doctor-side hits for `trauma|policy_denied|PolicyDenied` are doctor's own unsafe-undo refusal and rch pressure policy. The manifest `scopeReason` records the exact searches.
- **Real trigger:** Not applicable. A denial exiting 7 is the guard working, not damage.
- **Repair:** Not applicable. The nearest doctor family, FM-PS-01 (`src/core/doctor_fixers.rs:292`, policy rules out of sync with the binary's registry), is a different condition and is not stretched over this.
- **Undo:** Not applicable.
- **Oracle:** None: the fixture stays marker-only, is never run by a harness, never counts as a pass, and is pinned by exact id in `doctor_fixture_untested_ratchet`.
- **Negative control:** Not applicable.
- **Pinned sha:** a1d4afe8b (the searches in the manifest `scopeReason`).

## fm-policy_safety-redaction-class-coverage-gap

- **Label:** OUT-OF-SCOPE (mesh lane policy, not doctor-read state; bd-2oh15 ruling on c9985)
- **Severity:** P1. Not in the scored population: redaction coverage is mesh lane policy, not doctor-read state.
- **Detector:** None, by design. Redaction classes decide which memories a mesh lane grant may carry (`src/mesh/lane_grant_preview.rs:232`, `:495`; `src/mesh/team.rs:7215`). The doctor sources (`doctor.rs`, `doctor_fixers.rs`, `doctor_runtime.rs`) have no hit for `redaction_class|RedactionClass|redaction class|lane_grant`.
- **Real trigger:** Not applicable to doctor. If a redaction-class coverage gap matters, it needs a mesh-owned check.
- **Repair:** Not applicable. FM-PS-01 is not stretched over mesh lane policy.
- **Undo:** Not applicable.
- **Oracle:** None: the fixture stays marker-only, is never run by a harness, never counts as a pass, and is pinned by exact id in `doctor_fixture_untested_ratchet`.
- **Negative control:** Not applicable.
- **Pinned sha:** a1d4afe8b (the searches in the manifest `scopeReason`).
