# CI Proof-Lane Snapshot

`ee.ci_proof_lane_snapshot.v1` is the contract for read-only GitHub Actions proof-lane evidence. It lets agents decide whether a CI artifact can be used as source-authority evidence for a no-mock harness, whether they should wait for an active run, or whether they should abstain and file a follow-up bead.

The snapshot is not a scheduler. It must not dispatch workflows, cancel runs, download artifacts by default, run Cargo, reserve files, acknowledge Agent Mail, or mutate Beads. It summarizes existing evidence only.

## Contract

Canonical schemas:

- [`ee.ci_proof_lane_snapshot.v1`](schemas/ee.ci_proof_lane_snapshot.v1.json)
- [`ee.remote_build_artifact_manifest.v1`](schemas/ee.remote_build_artifact_manifest.v1.json)
- [`ee.remote_build_artifact_manifest.verification.v1`](schemas/ee.remote_build_artifact_manifest.verification.v1.json)

Producer:

```bash
scripts/ci_proof_lane_snapshot.sh --json
scripts/ci_proof_lane_snapshot.sh --head-sha <sha> --json
scripts/ci_proof_lane_snapshot.sh --input tests/fixtures/ci_proof_lane_live/stale_artifact.json --json
```

The producer has live and offline modes. Live mode reads bounded GitHub Actions
state through `gh run list`, `gh run view`, and metadata-only artifact API calls.
It does not dispatch workflows, cancel runs, download artifacts, reserve files,
mutate Beads, acknowledge Agent Mail, run Cargo, or build binaries. Offline mode
transforms `ee.ci_proof_lane_input.v1` fixtures under
`tests/fixtures/ci_proof_lane_live/` into the public snapshot schema so contract
checks can run without network access.

The offline fixture contract is pinned by
`scripts/ci_proof_lane_snapshot_fixture_test.sh`. The central
`scripts/verify.sh` runner executes that harness as the
`CI Proof-Lane Snapshot Contract` stage before Cargo-backed gates, so duplicate
dispatch, artifact freshness, checksum, surface-probe, `gh` unavailability, and
invalid-SHA behavior drift fails without launching CI, RCH, or local Cargo.
The same runner also executes `scripts/ci_proof_lane_hygiene.sh --json` as the
`CI Proof-Lane Hygiene Advisory` stage. That static workflow-policy scan emits
`ee.ci_proof_lane_hygiene.v1` and surfaces duplicate-dispatch,
cancel-in-progress, artifact-retention, release-artifact, and unclassified
artifact-lane posture without network access.

The snapshot records:

- repository owner/name/default branch/current head SHA
- whether the requested head SHA is reachable from GitHub Actions metadata
- workflow name/path/proof-lane kind/concurrency group/dispatch policy
- run ids, job ids, event, ref, head SHA, run status, conclusion, and timestamps
- bounded job labels, runner assignment state, runner name/group when GitHub exposes them, and queue age
- artifact id/name, producing run id, source commit/tree, retention/freshness, archive and binary hashes, build-input/command/provenance hashes, checksum status, architecture, and the exact canonical probe set
- a single `activeRecommendation` telling the agent to reuse, wait, download and verify, dispatch, abstain, or file a bead
- per-run `queueDiagnosis` for active runs, which explains ordinary waits versus stale unassigned runner capacity without changing the top-level safe action
- degraded codes for unavailable GitHub state, duplicate dispatch, cancelled-before-artifact runs, missing/stale artifacts, checksum mismatch, and surface probe failures

The contract deliberately separates artifact authority from source compile/test evidence. GitHub metadata alone never establishes artifact authority. A consumer must download the named artifact, independently verify its source-bound manifest, checksum, packaged bytes, and canonical probes, then pass the self-hashed verification report back to the snapshot producer. The report is accepted only for the exact repository, dedicated workflow, run id, artifact id, source commit, and artifact name. It does not prove the source passed tests unless another evidence source says so.

## Verdicts

`fresh_artifact_available`: the dedicated macOS lane produced the recommended current-head artifact, and a consumer report bound to that exact run/artifact id independently verified its manifest, checksum, archive/binary hashes, build inputs, command, and both canonical probes (`ee version --json` source provenance and `ee diag environment-attestation --help`).

`artifact_attestation_required`: artifact metadata exists, but no exact consumer verification report has established authority yet. Download and verify; do not reuse.

`artifact_attestation_invalid`: the supplied report matched the run/artifact identity but source, build-input, byte, checksum, or probe evidence was rejected. Preserve the rejection and do not reuse.

`wait_for_active_run`: a queued or in-progress run for the current head SHA exists. Agents should poll that run instead of dispatching a duplicate workflow.

If the active run has been queued or in progress beyond the conservative
handoff window, the verdict remains `wait_for_active_run`, but `degraded[]`
includes `ci_proof_lane_active_run_stale`. That code means the named run is
still authoritative; agents should hand off or keep polling rather than
dispatching a duplicate or cancelling without explicit human approval. Inspect
the run's `queueDiagnosis` and `jobEvidence[]` to distinguish an ordinary wait
from stale unassigned runner capacity or a runner/workflow state that needs
human-authorized inspection.

`duplicate_dispatch_detected`: multiple active workflow_dispatch runs target the same proof lane and head SHA. Agents should coordinate through Agent Mail and wait for one authoritative run.

`run_cancelled_before_artifact`: a terminal run was cancelled before the artifact was uploaded. This is proof-lane evidence, not a source failure.

`artifact_missing`: the workflow completed without the expected artifact.

`artifact_stale`: the artifact belongs to an older head SHA, expired retention window, or missing required CLI surface.

`checksum_mismatch`: the artifact checksum is missing or disagrees with the downloaded payload.

`surface_probe_failed`: the binary exists but a required command or schema surface is unavailable.

`gh_unavailable`: the producer could not read GitHub Actions state.

`local_only_head_unavailable`: the requested head SHA is not reachable from
GitHub Actions metadata. A workflow dispatch against `main` or another branch
can build a different remote SHA, so the snapshot abstains until the checkout is
reconciled or an approved push path makes the exact SHA available.

`abstain_manual_review`: the snapshot found contradictory evidence and cannot recommend a safe next command.

`no_matching_run`: no run exists for the requested proof lane and head SHA.

## Redaction

Snapshots may include workflow names, workflow paths, run ids, job ids, artifact names, refs, SHA hashes, status values, timestamps, and command templates. They must not include GitHub tokens, raw logs, raw stdout/stderr, local extraction paths, home-directory paths, Agent Mail bodies, source snippets, or secret-shaped values.

## Agent Runbook

Use this flow before dispatching a proof-lane workflow, downloading an artifact,
or treating a CI-built binary as source-authority evidence.

1. Generate the snapshot for the exact source SHA you intend to prove:

   ```bash
   scripts/ci_proof_lane_snapshot.sh --head-sha "$(git rev-parse HEAD)" --json
   ```

2. Read `summary.verdict`, `activeRecommendation.workflowName`,
   `activeRecommendation.runId`, `activeRecommendation.nextAction`, and
   `activeRecommendation.rationale`.
3. Check `repository.headShaReachability`. If it is `github_unreachable`, do not
   dispatch a branch ref as proof for the local checkout; it can build a
   different remote SHA.
4. Act only on the recommendation. Do not dispatch, cancel, download, or run a
   harness because a mail thread or old support bundle said a lane was usable.
5. When you need first-failure detail, use `degraded[]` and the matching
   `workflows[].runs[].firstFailureDiagnosis` row.
6. For `wait_for_active_run`, read `workflows[].runs[].queueDiagnosis` before
   escalating the blocker. A stale unassigned run with a comparable prior
   success on the same labels is runner-capacity evidence, not source failure.
7. Record the snapshot verdict in Agent Mail before mutating Beads or invoking a
   no-mock harness.

For a downloaded artifact, obtain its numeric artifact id from the run metadata,
extract the uploaded files into an external temporary directory, and create the
consumer report against the exact run:

```bash
python3 scripts/ci_artifact_attestation.py verify \
  --workspace . \
  --binary <download-dir>/ee \
  --archive <download-dir>/ee-aarch64-apple-darwin-debug.tar.gz \
  --checksum <download-dir>/ee-aarch64-apple-darwin-debug.tar.gz.sha256 \
  --manifest <download-dir>/ee-aarch64-apple-darwin-debug.manifest.json \
  --expected-commit <40-char-sha> \
  --repository Dicklesworthstone/eidetic_engine_cli \
  --expected-run-id <run-id> \
  --artifact-id <artifact-id> \
  --output <external-temp>/artifact-verification.json

scripts/ci_proof_lane_snapshot.sh \
  --head-sha <40-char-sha> \
  --artifact-verification <external-temp>/artifact-verification.json \
  --json
```

| Recommendation | Agent action |
| --- | --- |
| `reuse_active_run` | Poll the named run id and tell the swarm which run is authoritative. Do not dispatch another run for the same workflow and SHA. |
| `wait` | Poll the named queued or in-progress run. If it stays queued, hand off the run id rather than starting a duplicate. |
| `download_and_verify_artifact` | Download only the named artifact, verify the checksum, run the listed surface probe, then invoke the no-mock harness with the verified binary path. |
| `reuse_verified_artifact` | Reuse only the artifact whose repository, workflow, run id, artifact id, source identities, byte hashes, and canonical probes appear in the accepted consumer report. |
| `dispatch_new_run` | Send Agent Mail first with workflow path and head SHA. Dispatch exactly one run only after confirming no other agent has an active run for that lane. |
| `abstain_manual_review` | Stop at evidence collection. Preserve the first-failure diagnosis and file or update a Bead if source work is needed. |
| `file_followup_bead` | Create a follow-up Bead with the snapshot verdict, degraded codes, and next safe command. Do not claim source/test proof. |

Heavy Rust verification remains RCH-only. A local Cargo build is never
acceptable proof for a fresh-binary lane unless a human explicitly authorizes
that exact exception.

## Failure Examples

### Cancelled main CI before upload

Use when a push-triggered CI run had the artifact job queued or running, then a
later push cancelled it before upload.

Required evidence:

- workflow path and run id
- head SHA for the cancelled run
- artifact job name and conclusion
- `summary.verdict=run_cancelled_before_artifact`
- `activeRecommendation.nextAction` from the newest snapshot

Agent action: treat this as proof-lane evidence only. It is not a source test
failure and not a usable artifact. Reuse or wait on a newer active run when the
snapshot names one; otherwise coordinate before dispatching a dedicated lane.

### Duplicate workflow dispatch

Use when multiple active `workflow_dispatch` runs target the same proof lane and
head SHA.

Required evidence:

- every active run id for the lane
- workflow path
- shared head SHA
- `summary.verdict=duplicate_dispatch_detected`
- first run created time and newest run created time

Agent action: announce one authoritative run in Agent Mail and wait. Do not
cancel any workflow unless the human explicitly authorizes that exact action.

### Stale queued macOS artifact runner

Use when a dedicated artifact run remains queued beyond the handoff window.

Required evidence:

- authoritative active run id and job id
- job labels, especially hosted runner labels such as `macos-14`
- runner assignment state for the active job
- `queueDiagnosis.status`
- comparable prior successful run id using the same labels, when available
- comparable prior runner name/group, when GitHub exposes them
- `degraded[].code=ci_proof_lane_active_run_stale`

Agent action: keep the run authoritative and preserve the queue diagnosis in
Agent Mail. Do not dispatch a duplicate run or cancel the queued run. If
`queueDiagnosis.nextAction=inspect_github_runner_capacity_or_labels`, inspect
GitHub-hosted runner capacity or workflow label configuration only through a
read-only path unless the human authorizes a mutating operation.

### Stale artifact or missing surface

Use when an artifact exists but belongs to an older head SHA, has expired or
unknown retention posture, or lacks a required command surface such as
`ee diag environment-attestation`.

Required evidence:

- artifact name
- artifact source SHA
- requested source SHA
- checksum posture
- surface probe command and result
- `summary.verdict=artifact_stale` or `surface_probe_failed`

Agent action: do not feed the binary to a no-mock harness. File a follow-up Bead
or wait for a current artifact according to the snapshot recommendation.

### Local Cargo fallback under RCH-shaped command

Use when a command such as `rch exec -- bash -lc 'cargo build ...'` spawned
local `cargo`, `rustc`, or `rustdoc` processes in this checkout.

Required evidence:

- exact command shape, redacted if needed
- tripwire status and detected process count
- `local_cargo_bypass_detected` or equivalent degraded code
- whether remote Cargo ever started

Agent action: stop using the output as proof. Report the bypass in Agent Mail,
preserve the tripwire evidence, and switch to a CI artifact or
`scripts/rch_verify.sh` proof. Do not kill processes or clean build outputs
unless the human has explicitly authorized the exact destructive command.

## Agent Mail Handoff Template

Use this template when handing a proof lane to another agent or closing a Bead
whose proof depends on a CI artifact. Keep artifact-source proof separate from
source/test proof.

```text
[proof-lane] <verdict> for <bead-or-surface>
- workflow: <workflow name> (<workflow path>)
- run_id: <run id or none>
- job_id: <job id or none>
- head_sha: <40-char source SHA>
- artifact: <artifact name or none>
- artifact_id: <numeric id or none>
- attested_run_id: <run id from consumer report or none>
- manifest/archive/binary hashes: <sha256 values or none>
- build/effective-input/provenance hashes: <sha256 values or none>
- checksum: <verified | mismatch | missing | not_checked>
- surface_probes: <version_json and environment_attestation_help> => <passed | failed | not_run>
- queue_diagnosis: <status or none> next=<nextAction or none> comparable_prior_run=<run id or none>
- local_cargo_tripwire: <ok count=0 | bypass_detected count=N | not_checked>
- snapshot_verdict: <summary.verdict>
- degraded_codes: <codes or none>
- source_test_verdict: <not_evaluated | artifact_authority_only | source_not_tested | source_passed_elsewhere | source_failed_elsewhere | unknown>
- next_action: <activeRecommendation.nextAction>
- recommendation_rationale: <activeRecommendation.rationale>
- first_failure: <workflows[].runs[].firstFailureDiagnosis or degraded code, if any>
```

Acceptable closeout language:

- `Artifact source authority established`: the exact run/artifact identity,
  source commit/tree, build inputs, packaged bytes, checksum, and canonical
  probes match the consumer report. This still does not mean source
  tests passed.
- `Source tests passed through RCH`: separate RCH proof reached remote Cargo and
  passed. Include the `ee.rch.verify.v1` status and command hash.
- `Environment blocked before source`: CI/RCH/source-authority evidence blocked
  before source tests ran. Preserve the blocker and do not mark source failed.

## Cross-Links

- [`docs/rch_runbook.md`](rch_runbook.md) explains remote-only Cargo proof and
  local-Cargo tripwire evidence.
- [`docs/environment_attestation.md`](environment_attestation.md) explains how
  source authority is projected into readiness and handoff summaries.
- [`docs/swarm/support_bundle_swarm_brief_summary.md`](swarm/support_bundle_swarm_brief_summary.md)
  explains how redacted support bundles carry historical proof context without
  replacing a fresh snapshot.
- [`docs/swarm/coordination_runbook.md`](swarm/coordination_runbook.md)
  explains Agent Mail fallback and Beads coordination when the live channel is
  degraded.
