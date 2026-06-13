# Support Bundle Swarm Brief Summary

Schema: `ee.support_bundle.swarm_brief_summary.v1`

Support bundles include a compact, redacted summary derived from
`ee.swarm.brief.v1`. The ownership posture appears under
`fileSurfaceRiskSummary`, with counts by severity, hashed reservation-holder
label, and git status bucket plus the top risks represented by hashes and IDs.
Ready-work reservation pressure appears under `readyReservationPressureSummary`
with action, severity, and hashed reservation-holder-label counts plus the top
ready Beads represented by Bead IDs, hashed holder labels, hashed titles,
hashed likely surfaces, and hashed suggested commands.
Stalled in-progress work posture appears under `stalledBeadLivenessSummary`
with counts by posture/action/severity and top Bead IDs, title hashes, assignee
hashes, evidence hashes, and suggested-command hashes only. It is advisory and non-mutating:
support bundles may show that a Bead looks stale, but they never reopen Beads,
release reservations, or expose raw mail/thread content. Push-safety,
verification reuse, and symbol-risk posture are included under `gitAhead`,
`verificationBroker`, and `symbolRiskSummary`, respectively, using counts,
status codes, IDs, and hashes only.
The duplicate-work posture appears under `singleFlight` using the same
redaction-safe `ee.singleflight.posture.v1` shape exposed by `ee status` and
`ee doctor`, so handoff readers can see active leaders, follower waits,
timeouts, failures, reused-result counts, surface names, and key hashes without
raw queries or memory content.
Memory drift posture appears under `memoryDrift` with recent-pack counts,
affected memory IDs, source-kind counts, and degraded codes only. It does not
include source snippets, command output bodies, or full file listings.
When read-only memory-drift collection is blocked by the workspace write lock,
the support-bundle summary uses `status=lock_contention` and carries an
`evidenceInspection` block with `memoryEvidenceInspected=false`,
`sourceFreshness=not_inspected`, `lockAcquisitionClass=workspace_write_lock`,
the workspace-relative lock path, and non-mutating recovery suggestions. This
means the collector stopped before inspecting memory evidence; it is not the
same as stale or unverifiable selected memories.
RCH worker pressure appears under `rchWorkerPressure`; when no RCH capability
snapshot is present, its status is `not_collected`. Otherwise it uses the
redaction-safe `ee.rch.worker_pressure.v1` shape to distinguish
`healthy_but_pressure_blocked`, `telemetry_stale`, `pressure_policy_denied`,
`pressure_unknown`, and usable/degraded worker posture without embedding raw
worker filesystem listings or executing any operator remediation.
Environment attestation is narrower than this support-bundle summary: it
explains which readiness sources are authoritative before a claim, closeout, or
remote-proof admission decision. When a support bundle is created because source
authority was confusing, include a fresh redacted
`ee diag environment-attestation --workspace . --include-rch --json` excerpt as
separate evidence. The bundle summary remains non-mutating and does not replace
the live claim gate.

For handoffs, include the attestation excerpt next to the bundle summary when
the incident involved stale binaries, Agent Mail probe mismatch, Beads/BV
disagreement, RCH topology or source-materialization blockers, dirty checkout
collisions, reservation conflicts, or local Cargo bypass. The bundle may explain
why a previous agent stopped; a future agent still needs a current claim gate
before mutating Beads or closing work.

When the handoff involves a GitHub Actions artifact or native binary proof lane,
also include a compact `ee.ci_proof_lane_snapshot.v1` excerpt from
[`docs/ci-proof-lane-snapshot.md`](../ci-proof-lane-snapshot.md). The support
bundle may retain the historical run id, head SHA, artifact name, checksum
posture, surface probe, verdict, and next action, but it must not replace a
fresh snapshot before a later agent dispatches a workflow or consumes an
artifact.

Versioning: field renames or incompatible redaction semantics require a new
schema version and a migration note. Additive counts may remain in
`ee.support_bundle.swarm_brief_summary.v1` when existing consumers can safely
ignore them.

Redaction rules: counts, severity labels, risk codes, hashed reservation holder
labels, Bead IDs, affected memory IDs, drift status codes, the relative
`.ee/ee.write.lock` path for lock-contention posture, path hashes, command
hashes, single-flight key hashes, surface names, and generation counters are
allowed. Raw reservation holder labels, raw paths in the top-risk summary, raw
commands, mail bodies, raw logs, raw memory content, raw source snippets, raw
query text, raw symbol names, env dumps, file contents, and secret-like tokens
are not allowed.

Example:

```bash
ee support bundle --workspace . --redacted --out <dir> --json
```

Fixture catalog: `tests/fixtures/swarm/ownership_posture_cases.json` covers the
compact summary shape for healthy, degraded-source, and unattributed-blocker
ownership posture cases.

Related schemas: `ee.swarm.brief.v1`, `ee.swarm.recommendation.v1`.

Non-goals: support-bundle summaries do not expose full file listings, recover
mail bodies, preserve raw query text, or replace the full swarm brief when local
inspection is safe.

Tracking Beads: `bd-1zb7k.16.4`, `bd-1z1fd.4`
