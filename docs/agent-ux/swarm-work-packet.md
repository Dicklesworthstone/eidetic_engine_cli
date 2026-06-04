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
or false; a green local compile posture is not enough to claim Rust work.

Version guard: if the installed `ee` rejects `--claim-gate` or `--candidate`
as an unexpected argument, that binary is stale relative to the current
source/docs contract. Stop at inspection, run no BV claim command, do not
rebuild or install `ee` with local Cargo, and coordinate for an approved
RCH/release-path rebuild instead.

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
`fixture_matrix_consumer`. The current matrix expects exactly one claim-safe
fixture, `healthy_small.json`; crowded-checkout, degraded-Mail,
tracker-mismatch, rollup, BV-timeout, Beads-timeout, and RCH-blocked fixtures
must fail closed with consumer exit `3`.

The compact `summary.jsonl` mirrors the same proof surface for closeout tools:
it includes Beads/Cargo/RCH call counts, the generated packet consumer
decision, unsafe-reason and command-action counts, fixture-matrix safe/unsafe
counts, and the per-fixture decision summary.

## Required Guarantees

- The packet uses the standard `ee.response.v2` success envelope.
- `data.schema` is `ee.swarm.work_packet.v1`.
- `redactionStatus` is
  `counts_ids_statuses_path_patterns_command_templates_no_mail_body_no_file_content`.
- All candidate, source, degraded-code, and command arrays are deterministic.
- Every included source has a provenance record, even when the source is
  degraded or unavailable.
- Agent Mail archive/SQLite parity failures, semantic-readiness contradictions,
  and timeout/database-contention states are represented as degraded source
  evidence, not as an empty healthy inbox.
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
- `tests/fixtures/swarm_work_packet/agent_mail_database_contention_timeout.json`
- `tests/fixtures/swarm_work_packet/beads_command_timeout_no_output.json`
- `tests/fixtures/swarm_work_packet/bv_timeout_no_output.json`
- `tests/fixtures/swarm_work_packet/tracker_mismatch.json`
- `tests/fixtures/swarm_work_packet/rollup_only_no_claimable_child.json`
- `tests/fixtures/swarm_work_packet/integrity/malformed_jsonl_tail.json`

Implementation work should keep these aligned with the emitted command and add
failure mode fixtures if new degraded codes are introduced.
