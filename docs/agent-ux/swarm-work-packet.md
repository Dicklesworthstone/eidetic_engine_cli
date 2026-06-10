# Swarm Work Packet Contract

`ee.swarm.work_packet.v1` is the read-only artifact emitted by
`ee swarm work-packet --json` for agents joining a crowded checkout. The
canonical payload schema is
`docs/schemas/swarm/ee.swarm.work_packet.v1.json`; the response-envelope schema
is `docs/schemas/ee.swarm.work_packet.v1.json`. Together they package the same
coordination evidence agents currently collect by hand from `ee swarm brief`,
`ee swarm next-action`, Beads, Agent Mail, Git, and RCH status.

The packet is advisory. Generating it must not claim a bead, reserve files,
send mail, stage files, run Cargo, or execute destructive commands. Agents still
claim work through Beads, coordinate through Agent Mail, and reserve files with
the reservation service when those systems are healthy enough.

`ee swarm work-packet --claim-gate --json` is the planned `bd-1tlcd.1`
read-only gate over the same evidence. Its payload schema is
`ee.swarm.work_packet.claim_gate.v1`; it separates non-mutating
`nextCommandActions[]` from the optional mutating `claimCommandAction`.
Harnesses should only consider the claim command when `safeToClaim` is `true`
and `verdict` is `safe_to_claim`. For explicit `--candidate` checks,
`recommendedSafeToClaim` must describe that requested candidate, not a different
packet recommendation.

The gate exposes `sourceAuthority.rchRemoteOnlyRequired` separately from
`sourceAuthority.rchSafeToLaunchCargoVerification`. Harnesses must fail closed
when remote-only verification is required and the positive RCH proof is missing
or false; a green local compile posture is not enough to claim Rust work. It
also mirrors the compact environment-attestation verdict fields on
`sourceAuthority.environmentVerdict`, `sourceAuthority.sourceTestVerdict`,
`sourceAuthority.remoteVerificationAdmitted`, and
`sourceAuthority.localCargoFallbackObserved` so stale binary, tracker, RCH, and
local fallback evidence remains visible at the claim decision.
The same source-authority object carries `installFreshnessVerdict`,
`installFreshnessAuthoritative`, and `installFreshnessRepair`. The live
work-packet collector runs the offline install freshness probe for the
claim-gate surface; `fresh` clears only the binary-freshness precondition. A
stale, shadowed, PATH-missing, or missing-surface installed binary must keep
`safeToClaim=false`, emit no `claimCommandAction`, and include an
install-freshness unsafe reason even when the Beads candidate itself is otherwise
claimable. Inspect `recoveryActions[]` for the structured no-local-Cargo
recovery sequence: verify the install check, plan adoption from a current
artifact, or request an explicit operator exception.

When the claim gate, support-bundle summary, or handoff evidence disagrees about
which readiness source is authoritative, inspect
`ee diag environment-attestation --workspace . --include-rch --json`; it is the
read-only per-source contract for stale binary, Agent Mail probe mismatch,
Beads/BV disagreement, RCH blocker, dirty source, reservation conflict, and
local Cargo bypass verdicts.

Version guard: if the installed `ee` rejects `--claim-gate` or `--candidate`
as an unexpected argument, that binary is stale relative to the current
source/docs contract. Stop at inspection, run no BV claim command, do not
rebuild or install `ee` with local Cargo, and coordinate for an approved
RCH/release-path rebuild instead.

Install-freshness guard: when PATH order or binary freshness is suspect, agents
may pipe `ee install check --json --offline` into
`scripts/agent_consume_work_packet_gate.py`. The consumer accepts
`ee.install.check.v1` only as blocking evidence and emits `safeToClaim=false`;
stale, duplicate-PATH, and version-skew findings appear as
`install_freshness:<verdict>` and `install_finding:<code>`. Even a fresh install
check emits `install_check_is_not_claim_gate` because it is not a claim ticket:
it only clears the binary-freshness precondition, then the normal work-packet
claim gate must decide whether the Beads claim is safe.

For handoffs and postmortems, support bundles persist the same posture in
`install_freshness_summary.json`: a redaction-safe capsule of version status,
PATH counts, finding codes, and hashed install/path references. Treat it as
diagnostic evidence only, not as a Beads claim gate.

When quoting that capsule in Agent Mail, Beads comments, or closeout notes,
prefer the stable decision fields over copied command output:

- `freshness.verdict`, `freshness.authoritative`, and
  `freshness.blockingFindings[]` explain the install-freshness decision.
- `pathPosture.status`, `binaryCount`, `duplicateCount`, and
  `currentBinaryOnPath` explain PATH ordering without leaking PATH entries.
- `findingCounts` and `findings[].code` summarize stale, shadowed, duplicate,
  missing-manifest, unsupported-target, or checksum/manifest blockers.
- `target.targetTriple`, `target.supported`, `permissions.status`, and
  `updateSource.status` explain whether a no-local-Cargo adoption path existed.
- `summaryHash` is the correlation handle for later support bundles or
  regression-causality notes.

Never treat `install_freshness_summary.json` as current by age or by presence
alone. It records the install state when the support bundle was produced; a
fresh claim still requires a live `ee install check --json --offline` followed
by the normal work-packet claim gate.

For macOS adoption without local Cargo, follow
[`docs/agent_integration.md`](../agent_integration.md#no-local-cargo-install-freshness).
The approved path is read-only inspection and planning:

```bash
ee install check --json --offline
ee install plan --json --offline \
  --manifest <release-manifest.json> \
  --artifact-root <release-artifact-dir> \
  --install-dir "$HOME/.local/bin" \
  --target aarch64-apple-darwin
```

A plan is only adoptable when the selected
artifact target matches the host and `data.verification.checksumStatus=verified`.
Running `ee update`, copying from `target/`, or using `cargo install` is an
operator install action and requires explicit approval of the overwrite path and
artifact source.

For claim-gate handoffs, record the machine decision rather than prose:

- Treat a stale installed binary plus a verified plan as operator-ready, not
  claim-safe. The claim gate remains blocked until a post-install
  `ee install check --json --offline` reports `data.freshness.verdict=fresh`.
- Accept the adoption plan for operator action only when the check emits
  `ee.install.check.v1` with `ee.install.freshness.v1` and the plan emits
  `ee.install.plan.v1` with `status=ready|idempotent`,
  `targetTriple=aarch64-apple-darwin`, `targetStatus=matched`, and
  `checksumStatus=verified`.
- Block the claim gate when freshness is not authoritative, the plan has any
  error finding, the artifact target is not macOS for this host, checksum status
  is `planned|failed|missing|not_checked`, or the installed binary is shadowed
  by another PATH entry.
- If no verified macOS artifact exists, send the operator-exception record from
  `docs/agent_integration.md#no-local-cargo-install-freshness`; do not run local
  Cargo or copy from `target/`.

## Intended Flow

```bash
ee swarm next-action --workspace . --include-rch --json
ee swarm work-packet --workspace . --include-rch --json
ee swarm work-packet --workspace . --include-rch --claim-gate --json
```

After inspecting the packet:

1. If the claim gate reports `safeToClaim=true`, inspect
   `nextCommandActions[]`, then use `claimCommandAction` for the Beads claim.
2. Reserve the packet's suggested file patterns through Agent Mail when Mail is healthy.
3. Send a short coordination note in the bead thread.
4. Run only the verification commands listed in `verification.requiredCommands`.
   Prefer the structured `commandAction.argv` vector and pass it directly to
   `Command::new`/`spawn`; never feed `commandTemplate` (or the legacy
   `recommendedAction.suggestedCommands[]` string) to a shell. If
   `commandAction.copySafety` is `shell_required_review` or
   `forbidden_until_human_approval`, treat the entry as display-only until
   a human approves it.
5. If RCH is blocked, record the exact blocker and do not use local Cargo as substitute proof.

When a human explicitly directs progress while the gate is not claim-safe, the
safe fallback is narrower than a Beads claim: pick only static/docs work that
does not overlap dirty files or active reservations, reserve those exact paths,
announce the exception in Agent Mail, and leave Beads mutation for the holder of
`.beads/issues.jsonl` or for a later clean claim gate. That kind of slice can
unblock documentation or triage, but it is not source/test proof and must not be
reported as a successful claim.

When the packet, Beads/BV history, RCH proof, support-bundle summary, or E2E
radar artifacts disagree about why a gate failed, use
[`regression-causality.md`](regression-causality.md) to build an
`ee.regression_causality.v1` capsule from those read-only artifacts. The capsule
may justify a follow-up bead or Agent Mail handoff, but it does not make an
unsafe claim gate safe and must not drive a Beads claim command.

## Agent Mail Snapshot Bridge

Use this bridge only when the claim gate fails closed because Agent Mail
evidence is missing:

```bash
CANDIDATE=bd-example.1
ee swarm work-packet --workspace . --include-rch \
  --claim-gate --candidate "$CANDIDATE" --json \
  | jq '.data | {schema, verdict, safeToClaim, agentMailStatus: .sourceAuthority.agentMailStatus, unsafeReasons, degradedCodes}'

SNAPSHOT_PATH=/private/tmp/ee-agent-mail-snapshot.json
scripts/agent_mail_snapshot.sh \
  --project "$PWD" \
  --agent "$AGENT_NAME" \
  --output "$SNAPSHOT_PATH"

ee swarm work-packet --workspace . --include-rch \
  --agent-mail-snapshot "$SNAPSHOT_PATH" \
  --claim-gate --candidate "$CANDIDATE" --json \
  | jq '.data | {schema, verdict, safeToClaim, agentMailStatus: .sourceAuthority.agentMailStatus, unsafeReasons, staleReasons, degradedCodes}'
```

Treat `agent_mail_unavailable` and `agentMailStatus` values of `unavailable`,
`skipped`, or `degraded_read_only` as unknown coordination evidence, not an
empty inbox or no reservations. A retry that reaches `agentMailStatus=fresh` or
`healthy` only means the gate consumed the redacted snapshot. Claim only when
the retry still reports `safeToClaim=true`, `verdict=safe_to_claim`, and a
runnable `claimCommandAction`. If `unsafeReasons` or `staleReasons` still name
reservation collisions, stale tracker state, Beads/BV disagreement, or RCH
blockers, coordinate instead of claiming.

Treat `bv_command_timeout` and `bv_no_output` as graph-triage liveness
failures, not as "no good work exists." Do not wait indefinitely on raw
`bv --robot-*` commands and do not use a BV copy-paste claim command from stale
or partial output. Retry BV only with an explicit timeout, or continue from
bounded `ee swarm work-packet` / `ee swarm brief` output plus direct
`br --no-auto-import --allow-stale ...` inspection until a fresh claim gate
emits a real `claimCommandAction`.

## Authority-Degraded Conformance Cases

Authority-degraded fixtures prove that a claim gate fails closed until every
source needed for the claim is both fresh and positive. Adding or updating one
of these fixtures requires harness assertions for all of the following:

- `safeToClaim=false` and a non-`safe_to_claim` verdict.
- No runnable mutating claim path: no executable `claimCommandAction`, no
  `inspect_and_claim` action, and no shell-safe mutating argv in the consumer
  output.
- `whyNotSafe` includes `claim_gate_degraded_authority:<code>` for claim-gate
  inputs or `packet_degraded_authority:<code>` for packet-only inputs.
- Stale Beads cases keep `sourceAuthority.trackerAuthoritative=false` and
  include tracker evidence such as `beads_tracker_stale`, `jsonl_newer=true`,
  merge artifacts, or pending external changes.
- A fresh Agent Mail snapshot only clears the Mail unknown. It must not make a
  candidate claimable while stale tracker state, `bv_command_timeout`, missing
  RCH proof, or local Cargo fallback evidence remains.
- Remote-required Rust verification fails closed when
  `sourceAuthority.rchRemoteOnlyRequired=true` and positive remote proof is
  absent or `sourceAuthority.rchSafeToLaunchCargoVerification=false`.
- The no-mutation smoke still records zero Beads, Cargo, and RCH mutations for
  the fixture path and emits an `ee.test_event.v1` summary with the unsafe
  decision, degraded codes, and consumer exit status.

## Copy-Pastable Runs

Healthy checkout:

```bash
ee swarm work-packet --workspace . --include-rch --json
```

Crowded checkout with degraded coordination sources:

```bash
ee swarm work-packet --workspace . --include-rch --json > /tmp/ee-work-packet.json
jq '.data.observedStateClass, .data.recommendedAction.safeToClaim, .data.degraded, .data.sourceProvenance' /tmp/ee-work-packet.json
```

Inspect those fields before acting. A degraded packet can still be useful for
recovery guidance, but it is not a claim ticket. If `safeToClaim` is false or
the candidate decision is anything other than `safe_to_claim`, stop at
inspection and coordinate through Beads comments, Agent Mail when available, or
manual handoff.

## No-Mutation Smoke

The read-only guarantee is covered by
`scripts/e2e_swarm_work_packet_no_mutation.sh`. The default fixture mode does
not require a built `ee` binary and does not invoke Cargo or RCH:

```bash
bash scripts/e2e_swarm_work_packet_no_mutation.sh
```

To drive a prebuilt work-packet command through the same harness, set
`EE_PACKET_NO_MUTATION_CMD`:

```bash
EE_PACKET_NO_MUTATION_CMD='ee swarm work-packet --workspace . --include-rch --json' \
  bash scripts/e2e_swarm_work_packet_no_mutation.sh
```

The script emits structured events and a summary under its artifact root. The
log records before/after snapshots for the sandbox `.beads/` directory, the
Agent Mail stand-in, and the git index. It also installs PATH shims that refuse
mutating Beads subcommands and fail the run if packet generation attempts Cargo
or RCH execution.

Default fixture mode also proves the agent-facing consumer path. After packet
generation, the harness pipes the packet through
`scripts/agent_consume_work_packet_gate.py` and logs a `consumer_decision`
phase with the consumer schema, exit code, `safe_to_claim`, decision, action,
unsafe-reason count, and command-action count. Unsafe packets must produce
non-empty `whyNotSafe` and must not expose runnable mutating or claim actions.

The same run also executes the reference consumer against every
`tests/fixtures/swarm_work_packet/*.json` fixture and logs
`fixture_matrix_consumer`. It also runs the consumer against the real
install-check golden envelopes in `tests/fixtures/golden/install/*_check.json.golden`.
The current matrix expects exactly one claim-safe swarm fixture,
`healthy_small.json`; crowded-checkout, degraded-Mail, tracker-mismatch,
rollup, BV-timeout, Beads-timeout, RCH-blocked, duplicate-PATH install, and
missing-PATH install fixtures must fail closed with consumer exit `3`.

The compact `summary.jsonl` mirrors the same proof surface for closeout tools:
it includes Beads/Cargo/RCH call counts, the generated packet consumer
decision, unsafe-reason, degraded-summary, command-action, and max argv-part
counts, fixture-matrix safe/unsafe counts, `install_fixture_count`, and the
per-fixture decision summary.

## Required Guarantees

- The packet uses the standard `ee.response.v2` success envelope.
- `data.schema` is `ee.swarm.work_packet.v1`.
- `redactionStatus` is
  `counts_ids_statuses_path_patterns_command_templates_no_mail_body_no_file_content`.
- All candidate, source, degraded-code, and command arrays are deterministic.
- Every included source has a provenance record, even when the source is
  degraded or unavailable.
- Agent Mail archive/SQLite parity failures, semantic-readiness contradictions,
  recovery/durability corruption, and timeout/database-contention states are
  represented as degraded source evidence, not as an empty healthy inbox.
- BV robot-source timeout/no-output states are represented as degraded source
  evidence and must recommend stale-safe Beads fallback, not bare interactive
  `bv`.
- Beads JSONL/DB drift, merge artifacts, and malformed JSONL tails are
  represented in `data.trackerIntegrity`; agents downgrade candidate safety
  when `brReadsAuthoritative` is false and must not auto-claim when
  `requiresCandidateDowngrade` is true.
- Open epics and parent Beads without a claimable child stay advisory:
  `rollup_only` and `blocked_rollup` decisions must not emit mutating claim or
  reopen commands.
- `data.candidates[].decision` uses the bd-2z5ly.7.5 candidate decision
  vocabulary: `safe_to_claim`, `already_owned`, `unsafe_due_to_conflict`,
  `blocked_by_dependency`, `blocked_by_verification`,
  `stale_but_reclaimable`, `stale_review`, `external_state_required`,
  `release_operator_required`, `rollup_only`, `blocked_rollup`,
  `coordinate_first`, `blocked`, `stale_or_advisory`, and `skip`.
  Only `safe_to_claim` may drive `inspect_and_claim`; `stale_but_reclaimable`
  may drive `reopen_stale_work` after explicit inspection; every other value is
  diagnostic and must not emit claim or reopen commands.
- Candidate arrays and each candidate's `unsafeReasons`, `staleReasons`, and
  `sourceRefs` arrays are sorted deterministically before `packetId`
  calculation. Decision vocabulary changes require schema, docs, fixture or
  golden, and lifecycle-test updates in the same slice.
- RCH topology and remote-required fallback failures are represented as
  verification blockers.
- The packet never contains mail bodies, raw command output, source snippets, or
  raw file contents.
- Every agent-actionable command exposes a structured
  `commandAction` (`commandId`, `displayCommand`, `argv`, `shellRequired`,
  `copySafety`, `mutatesState`, `requiredSubstrate`, `when`, `rationale`).
  `copySafety=safe_structured_argv` is the only posture a harness may execute
  automatically; `shell_required_review` and `forbidden_until_human_approval`
  require human approval, and `display_only` entries (including legacy
  `commandTemplate` strings) MUST NOT be passed to a shell. See
  `bd-13dmm.3` and the `work_packet_command_actions_require_shell_safe_argv_contract`
  lifecycle test.

## Fixture Set

The contract is seeded by redacted examples:

- `docs/schemas/swarm/ee.swarm.work_packet.v1.json` embeds data-level examples
  for `healthy_small_repo`, `crowded_checkout`, and
  `degraded_mail_rch_topology`.
- `tests/fixtures/swarm_work_packet/healthy_small.json`
- `tests/fixtures/swarm_work_packet/crowded_checkout.json`
- `tests/fixtures/swarm_work_packet/degraded_mail_rch_topology.json`
- `tests/fixtures/swarm_work_packet/agent_mail_degraded_read_only.json`
- `tests/fixtures/swarm_work_packet/agent_mail_semantic_readiness_failed.json`
- `tests/fixtures/swarm_work_packet/agent_mail_recovery_corrupt.json`
- `tests/fixtures/swarm_work_packet/agent_mail_database_contention_timeout.json`
- `tests/fixtures/swarm_work_packet/beads_command_timeout_no_output.json`
- `tests/fixtures/swarm_work_packet/bv_timeout_no_output.json`
- `tests/fixtures/swarm_work_packet/tracker_mismatch.json`
- `tests/fixtures/swarm_work_packet/rollup_only_no_claimable_child.json`
- `tests/fixtures/swarm_work_packet/integrity/malformed_jsonl_tail.json`

Implementation work should keep these aligned with the emitted command and add
failure mode fixtures if new degraded codes are introduced.
