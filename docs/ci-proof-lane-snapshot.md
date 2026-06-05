# CI Proof-Lane Snapshot

`ee.ci_proof_lane_snapshot.v1` is the contract for read-only GitHub Actions proof-lane evidence. It lets agents decide whether a CI artifact can be used as source-authority evidence for a no-mock harness, whether they should wait for an active run, or whether they should abstain and file a follow-up bead.

The snapshot is not a scheduler. It must not dispatch workflows, cancel runs, download artifacts by default, run Cargo, reserve files, acknowledge Agent Mail, or mutate Beads. It summarizes existing evidence only.

## Contract

Canonical schema: [`docs/schemas/ee.ci_proof_lane_snapshot.v1.json`](schemas/ee.ci_proof_lane_snapshot.v1.json)

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

The snapshot records:

- repository owner/name/default branch/current head SHA
- workflow name/path/proof-lane kind/concurrency group/dispatch policy
- run ids, job ids, event, ref, head SHA, run status, conclusion, and timestamps
- artifact names, source SHA, retention/freshness, checksum status, architecture, and surface probes
- a single `activeRecommendation` telling the agent to reuse, wait, download and verify, dispatch, abstain, or file a bead
- degraded codes for unavailable GitHub state, duplicate dispatch, cancelled-before-artifact runs, missing/stale artifacts, checksum mismatch, and surface probe failures

The contract deliberately separates artifact authority from source compile/test evidence. A fresh artifact with a verified checksum can prove that a binary came from a particular workflow run and head SHA. It does not prove the source passed tests unless another evidence source says so.

## Verdicts

`fresh_artifact_available`: a current-head artifact exists, checksum posture is acceptable, and required surface probes passed.

`wait_for_active_run`: a queued or in-progress run for the current head SHA exists. Agents should poll that run instead of dispatching a duplicate workflow.

`duplicate_dispatch_detected`: multiple active workflow_dispatch runs target the same proof lane and head SHA. Agents should coordinate through Agent Mail and wait for one authoritative run.

`run_cancelled_before_artifact`: a terminal run was cancelled before the artifact was uploaded. This is proof-lane evidence, not a source failure.

`artifact_missing`: the workflow completed without the expected artifact.

`artifact_stale`: the artifact belongs to an older head SHA, expired retention window, or missing required CLI surface.

`checksum_mismatch`: the artifact checksum is missing or disagrees with the downloaded payload.

`surface_probe_failed`: the binary exists but a required command or schema surface is unavailable.

`gh_unavailable`: the producer could not read GitHub Actions state.

`abstain_manual_review`: the snapshot found contradictory evidence and cannot recommend a safe next command.

`no_matching_run`: no run exists for the requested proof lane and head SHA.

## Redaction

Snapshots may include workflow names, workflow paths, run ids, job ids, artifact names, refs, SHA hashes, status values, timestamps, and command templates. They must not include GitHub tokens, raw logs, raw stdout/stderr, local extraction paths, home-directory paths, Agent Mail bodies, source snippets, or secret-shaped values.

## Example Agent Loop

1. Generate or inspect a snapshot.
2. Follow `activeRecommendation.nextAction`.
3. If the action is `wait`, poll the named run id.
4. If the action is `download_and_verify_artifact`, verify the checksum and run the listed surface probe before invoking a no-mock harness.
5. If the action is `dispatch_new_run`, coordinate through Agent Mail first so another agent does not dispatch a duplicate lane.
6. If the action is `abstain_manual_review` or `file_followup_bead`, preserve the first-failure diagnosis and do not claim source proof.

Heavy Rust verification remains RCH-only. A local Cargo build is never acceptable proof for a fresh-binary lane unless a human explicitly authorizes that exact exception.
