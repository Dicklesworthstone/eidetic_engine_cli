# Swarm Work Packet

Schemas: `ee.swarm.work_packet.v1`, `ee.swarm.work_packet.claim_gate.v1`
Reference consumer output: `ee.agent.work_packet_gate_decision.v1`
(`docs/schemas/ee.agent.work_packet_gate_decision.v1.json`).

`ee.swarm.work_packet.v1` is the deterministic, redacted, read-only artifact
emitted by `ee swarm work-packet --json` before an agent chooses work in a
crowded checkout. It packages the recommended lane, candidate Bead decisions,
dirty-file collision evidence, active claims, Agent Mail freshness, Beads
tracker integrity, RCH proof posture, required verification commands, source
provenance, and exact reasons a task is safe, unsafe, blocked, or stale.

The packet composes existing read-only collectors from `ee swarm brief` and
`ee swarm next-action`. It must not parse Beads, BV, Agent Mail, RCH, or Git
with a second independent vocabulary when a collector already exists.

`ee.swarm.work_packet.claim_gate.v1` is the planned `bd-1tlcd.1` projection for
`ee swarm work-packet --claim-gate --json` and
`ee swarm work-packet --claim-gate --candidate <bead-id> --json`. It answers a
single question over the same packet evidence: whether a selected candidate is
safe to claim now. The schema is marked unshipped until that CLI mode emits the
contract in current builds.

If the installed `ee` rejects `--claim-gate` or `--candidate` as an unexpected
argument, treat that binary as stale relative to the current source/docs
contract. Stop at inspection, run no BV claim command, avoid local Cargo
rebuild/install workarounds, and coordinate for an approved RCH/release-path
rebuild.

Versioning: field renames, changed decision semantics, or changed mutation
policy semantics require a new schema version. Additive fields may remain in
`ee.swarm.work_packet.v1` only when consumers can safely ignore them and the
deterministic ordering rules below still hold.

Redaction rules: Bead IDs, titles, statuses, priority values, assignee labels,
path patterns, counts, degraded codes, command templates, and stable source
digests are allowed. Mail bodies, raw command output, raw source snippets, env
dumps, unredacted home paths, file contents, and secret-like tokens are not
allowed.

Determinism rules:

- `packetId` is derived from the redacted payload, not wall-clock time.
- Arrays are sorted by stable keys: source name, Bead ID, path pattern, then
  reason code.
- Source freshness is represented as a class or redacted timestamp supplied by
  the source collector. Packet generation must not add a new volatile timestamp.
- Unknown source state is explicit as `unknown`, `skipped`, `unavailable`, or a
  `degraded[]` record; it is never inferred from missing fields.

Fixture scenarios:

- `healthy_small_repo`: fresh coordination sources, no dirty collisions, and a
  ready candidate that is safe to claim after normal Beads/file-reservation
  steps.
- `crowded_checkout`: active claims and dirty path overlap force
  `coordinate_before_claim`.
- `degraded_mail_rch_topology`: Agent Mail is degraded and remote-only Cargo
  proof is blocked, so only static or docs work can proceed until RCH recovers.
- `agent_mail_degraded_read_only`, `agent_mail_semantic_readiness_failed`,
  `agent_mail_recovery_corrupt`, and `agent_mail_database_contention_timeout`:
  Agent Mail evidence is present but not authoritative enough to infer
  reservation or inbox safety.
- `beads_command_timeout_no_output` and `bv_timeout_no_output`: tracker or
  graph-triage sources timed out or emitted no output; Beads fallback rows are
  advisory until a bounded retry or manual inspection succeeds.
  Raw `bv --robot-*` probes must be externally bounded or routed through the
  `ee swarm` collectors. `bv_command_timeout` and `bv_no_output` are
  source-authority degradations, not evidence that no work exists. Consumers
  must fail closed in this state: emit no runnable claim action, ignore any
  legacy BV copy-paste claim, continue with stale-safe
  `br --no-auto-import --allow-stale` inspection, and rerun
  `ee swarm work-packet --claim-gate` before mutating Beads.
- `tracker_mismatch`: an otherwise claimable leaf is downgraded because Beads
  JSONL and DB state are not authoritative.
- `rollup_only_no_claimable_child`: an open epic or parent has no claimable
  child and must remain inspection-only.

## Same-file proof debt handoff (bd-1n3x1.15.4)

`unproved_same_file_source_debt` is coordination evidence emitted when a
candidate likely touches a relative path already changed by a related bead whose
source work is complete but whose Cargo proof has not reached a Rust verdict.
It is not a dirty-checkout count, reservation conflict, tracker-authority
failure, install-freshness failure, active-build admission blocker, pressure
telemetry blocker, or RCH topology blocker. Those reasons remain present when
they apply; same-file proof debt only explains the same-path proof risk.

The first shipped surface from `bd-1n3x1.15.3` is the internal work-packet and
claim-gate unsafe-reason name. Agents should look for
`unproved_same_file_source_debt` in candidate unsafe reasons and treat the
recommended action as `coordinate_before_claim`. The reason can make
`safeToClaim` false or keep it false, but it never authorizes a claim command
and never converts an environment proof blocker into a Rust source failure.

Use this Beads comment template when a candidate is unsafe because of the
signal:

```text
Same-file proof debt: candidate <candidate-bead> likely touches
<relative-path>, while related bead <related-bead> has source-complete work
whose Cargo proof is blocked before a Rust verdict (<bounded-blocker-codes>).
Coordinate before stacking edits. This is coordination evidence only; do not
close either bead or claim a Rust source failure from this signal.
```

Use this Agent Mail template for the common handoff:

```text
I am evaluating <candidate-bead>. The work packet reports
unproved_same_file_source_debt for <relative-path> against <related-bead>, with
proof blocker codes <bounded-blocker-codes>. Are you still actively owning that
path, or should I wait for proof/closure before editing? I will not stack source
edits without coordination.
```

Operator override is explicit coordination, not automatic permission. A human
or current file owner can decide that a candidate may proceed, but ordinary
claim-gate blockers still matter: live reservations, tracker corruption,
install freshness, Agent Mail authority, active-build admission, pressure
telemetry, and topology blockers must be handled according to their own
contracts. Keep handoffs bounded to bead ids, relative paths, short blocker
codes, and next commands such as `CI=1 br show <id> --json` or the required RCH
proof wrapper; do not paste mail bodies, raw logs, full diffs, private absolute
paths, or secrets.

Shell smoke coverage lives in `scripts/e2e_swarm_work_packet_no_mutation.sh`.
The harness snapshots `.beads/`, a synthetic Agent Mail store, and the Git
index around packet generation, logs `ee.test_event.v1` phases, refuses mutating
`br` subcommands, and shims Cargo/RCH so packet generation cannot accidentally
become verification.

The smoke harness also runs the generated packet through
`scripts/agent_consume_work_packet_gate.py` and records a `consumer_decision`
phase. That phase proves the reference consumer can parse the packet without
executing commands, reports `safeToClaim`, records `whyNotSafe` coverage,
emits the harness decision schema above, and asserts unsafe packets do not
expose runnable mutating or claim actions.

Fixture-matrix coverage is part of the same script. The
`fixture_matrix_consumer` phase feeds every `tests/fixtures/swarm_work_packet`
fixture into the reference consumer, including payload-only fixtures and
`ee.response.v2` envelopes. The expected posture is one safe fixture
(`healthy_small.json`) and all degraded, crowded, tracker-mismatch, rollup,
timeout, and RCH-blocked fixtures failing closed with exit `3`.

The final `summary.jsonl` is the closeout-friendly digest of those phases. It
includes Beads, Cargo, and RCH call counts; the generated packet consumer
schema, exit code, decision, action, and counts; fixture-matrix safe/unsafe
counts; and the per-fixture decision summary.

## Beads tracker integrity (bd-2z5ly.9)

`trackerIntegrity` is the packet's bounded view of Beads JSONL/DB health. It is
derived from `br doctor --json` or equivalent collector evidence, not by
re-parsing raw tracker rows inside the work-packet layer.

- `health` is one of `ok`, `merge_artifacts_warn`,
  `external_changes_pending_import`, `db_jsonl_count_mismatch`, or
  `jsonl_parse_error`.
- `brReadsAuthoritative` means the collected parity evidence is sufficient for
  read-only Beads inspection. It is true for `ok` and can remain true for a
  metadata-only `external_changes_pending_import` warning when DB/JSONL counts
  match, `dirtyIssueCount=0`, `pendingImportCount=0`, and no non-benign merge
  artifacts are present. A prose `br doctor` message alone is not tracker
  corruption evidence in that state.
- `requiresCandidateDowngrade` is true when tracker evidence is not
  authoritative, such as malformed JSONL, DB/JSONL count mismatches, dirty DB
  issues, or non-benign merge artifacts. Candidate safety MUST refuse
  auto-claim-style advice in those states.
- Counts and paths are bounded summaries: JSONL row count, DB row count,
  pending import count, dirty issue count, merge artifact count, and at most a
  small sorted list of merge artifact paths.
- `jsonlParseError` carries only the first invalid line/column plus a redacted,
  length-capped excerpt. It must never include raw issue bodies beyond that
  bounded diagnostic.
- JSONL parse-error diagnostics may also include `invalidLineNumbers`,
  `jsonlValidRecordCount`, `dbIntegrityOk`, sync timestamps, `mutationMustStop`,
  `safeRepairCandidate`, `repairClassification`, and `repairCommandCandidate`.
  The flush-only forced export command is only a candidate when DB integrity is
  clean, DB/JSONL valid counts agree, the malformed line is a trailing row, and
  no dirty or merge evidence makes repair ambiguous.

The work-packet generator never repairs Beads state. Recovery remains explicit:
inspect the malformed row, run `br doctor --json`, use
`br --no-auto-import --allow-stale` for read-only fallback when needed, and only
then claim or update tracker state.

### Tracker authority states (bd-3w4pv.6)

`trackerIntegrity.health` keeps its coarse five-value vocabulary for payload
compatibility, but `brReadsAuthoritative` is derived from a finer
tracker-authority classification that the claim gate surfaces as
`sourceAuthority.trackerHealth`:

| State | Concrete evidence | `trackerAuthoritative` |
| --- | --- | --- |
| `clean` | No parse, merge, count, dirty, or metadata signal. | `true` |
| `doctor_metadata_message_only` | The doctor `sync.metadata` message claims pending external changes while dirty issues are 0, DB/JSONL counts match, and there are no merge artifacts or parse errors. | `true` |
| `db_newer` | DB has rows the JSONL export lacks (`br sync --flush-only`). | `false` |
| `jsonl_newer` | JSONL has importable rows the DB has not absorbed (`br sync --import-only`). | `false` |
| `dirty_issues` | `br doctor` reports locally dirty issues. | `false` |
| `count_mismatch` | DB/JSONL counts differ in a shape auto-import cannot reconcile. | `false` |
| `merge_artifacts` | Non-benign merge artifacts next to `issues.jsonl` (the `beads.base.jsonl` merge anchor is benign). | `false` |
| `parse_error` | At least one malformed JSONL line. | `false` |

Precedence when several concrete signals hold at once (worst first):
`parse_error` > `merge_artifacts` > `count_mismatch` > `dirty_issues` >
`jsonl_newer` > `db_newer` > `doctor_metadata_message_only` > `clean`.

A doctor `sync.metadata` prose message counts as non-authoritative only when it
is paired with concrete dirty/import evidence. When the message appears with
clean concrete evidence, the packet keeps `brReadsAuthoritative=true`, emits
`trackerHealth=doctor_metadata_message_only`, and surfaces the contradiction as
the warning-severity `beads_tracker_metadata_drift` degraded code (bounded
message, no raw `br` output) instead of `beads_tracker_not_authoritative`.
Every other non-clean state fails closed: the unsafe reason
`beads_tracker_not_authoritative:<state>` names the concrete state, candidates
downgrade to `external_state_required`, and `claimCommandAction` stays `null`.
Dirty-checkout conflict evidence is computed separately, so a metadata-only
tracker contradiction never makes a dirty checkout claim-safe — overlapping
dirty surfaces still produce `unsafe_due_to_conflict`.

## Candidate decision vocabulary (bd-2z5ly.7.5)

`candidates[].decision` is a stable diagnostic vocabulary. It explains the
candidate's safety posture; it is not the same field as
`recommendedAction.action`. `recommendedAction.safeToClaim=true` is only
preclaim advice; harnesses may mutate Beads only through the claim-gate
projection when it emits `safeToClaim=true` and a non-null
`claimCommandAction`.

| Decision | Agent meaning | May drive recommended action? |
| --- | --- | --- |
| `safe_to_claim` | The candidate is open, unblocked, unowned, and conflict evidence is authoritative enough to claim after normal reservation/coordination steps. | Yes: `inspect_and_claim` only. |
| `already_owned` | A fresh assignee, active claim, or authoritative coordination signal says another lane owns the work. | No; inspect or coordinate only. |
| `unsafe_due_to_conflict` | Dirty files, reservations, or overlapping edit surfaces make an otherwise useful candidate unsafe for autonomous claim. | No; coordinate first. |
| `blocked_by_dependency` | The candidate itself is blocked by another Bead or prerequisite. | No; work the blocker. |
| `blocked_by_verification` | RCH, verifier-ledger, or remote-only proof posture prevents responsible progress on the candidate. | No; record blocker or choose static/docs work. |
| `stale_but_reclaimable` | Deterministic age and inactivity evidence says a prior claim may be reopened after explicit inspection. | Yes: `reopen_stale_work`, never silent mutation. |
| `stale_review` | Stale evidence exists but is too weak for reclaim guidance. | No; inspect manually. |
| `external_state_required` | Progress needs a service, credential, upstream release, or operator state change outside the checkout. | No. |
| `release_operator_required` | The candidate is gated by release authority, signing, publishing, or tagging decisions. | No. |
| `rollup_only` | The item is an epic or parent used for context, not a claimable leaf. | No. |
| `blocked_rollup` | The item is a blocked epic or parent and should only explain dependency context. | No. |
| `coordinate_first` | Broad compatibility bucket for candidates that need human/agent coordination before a more precise decision is available. | No. |
| `blocked` | Broad compatibility bucket for candidates that cannot proceed. | No. |
| `stale_or_advisory` | Broad compatibility bucket for stale/advisory evidence that is not enough to reclaim. | No. |
| `skip` | Duplicate, out-of-scope, or intentionally rejected candidate. | No. |

Deterministic ordering is part of the contract: candidates sort by their stable
struct keys, and each candidate's `unsafeReasons`, `staleReasons`, and
`sourceRefs` arrays are sorted and deduplicated before `packetId` calculation.
Adding, renaming, or reclassifying a decision requires updating the schema,
this document, the agent UX document, relevant fixtures or goldens, and the
`work_packet_candidate_decision_vocabulary_is_contractual` lifecycle test.

Implementation contract:

- Generate the packet only after reading existing swarm brief and next-action
  snapshots, or equivalent in-memory collector outputs.
- Preserve source provenance for each included decision so an agent can decide
  whether Beads, BV, Agent Mail, or RCH drove the advice.
- Include RCH posture even for docs-first work so closeouts do not accidentally
  imply local Cargo fallback is allowed.
- Keep command templates display-only and prefer structured `commandAction`
  argv fields for executable guidance. They are obligations for the next step,
  not proof that the packet generator ran those commands.

Non-goals: work packets do not claim Beads, reserve files, send Agent Mail,
stage Git changes, run Cargo, delete files, schedule agents, or replace Beads,
Agent Mail, BV, RCH, `ee swarm brief`, or `ee swarm next-action`.

## Claim-gate projection (bd-1tlcd.1)

The claim gate is a read-only projection over `ee.swarm.work_packet.v1`, not a
second collector and not a mutating helper. It carries `schema =
ee.swarm.work_packet.claim_gate.v1`, the source `packetId`, a deterministic
`gateId`, the selected or requested candidate, source-authority booleans, sorted
reason arrays, and the verdict.

`safeToClaim` is `true` only when the selected candidate decision is
`safe_to_claim`, the source packet's `recommendedAction.safeToClaim` is `true`,
and source authority has no hard freshness blocker. `sourceAuthority.rchRemoteOnlyRequired` and
`sourceAuthority.rchSafeToLaunchCargoVerification` are separate so harnesses can
fail closed when remote-only verification is required but the positive RCH proof
is missing or false; a green local compile posture is not enough to claim Rust
work. The same block also carries compact environment-attestation fields:
`environmentVerdict`, `sourceTestVerdict`, `remoteVerificationAdmitted`, and
`localCargoFallbackObserved`. They summarize the source-authority posture
without replacing the gate verdict, candidate decision, unsafe reasons, or the
full `ee diag environment-attestation` surface. `candidate_not_found` and
`no_candidate` are explicit gate verdicts so
harnesses do not infer safety from missing candidate data.

`sourceAuthority.trackerAuthoritative` and `sourceAuthority.trackerHealth`
carry the tracker authority state described in
[Tracker authority states (bd-3w4pv.6)](#tracker-authority-states-bd-3w4pv6).
Live gates emit the split vocabulary (`clean`,
`doctor_metadata_message_only`, `dirty_issues`, `jsonl_newer`, `db_newer`,
`merge_artifacts`, `count_mismatch`, `parse_error`); the legacy coarse
`trackerIntegrity.health` values remain schema-valid for archived payloads.
`doctor_metadata_message_only` is the only non-`clean` state that keeps
`trackerAuthoritative=true`: the doctor metadata message contradicts clean
concrete evidence, so the gate carries the warning-severity
`beads_tracker_metadata_drift` degraded code instead of refusing the claim for
the wrong reason.

Installed-binary freshness is carried beside the other authority fields as
`installFreshnessVerdict`, `installFreshnessAuthoritative`, and
`installFreshnessRepair`. Live work-packet collection runs the offline install
freshness probe for the claim-gate surface; when it reports `fresh`, the gate
may continue to ordinary Beads/coordination checks. If the gate sees
`stale_binary_suspected`, a shadowed/path-missing binary, or a missing required
claim-gate surface, it must emit `safeToClaim=false`, a non-safe verdict such as
`blocked_by_verification`, `claimCommandAction=null`, and an unsafe reason such
as `install_freshness:stale`. It must also populate `recoveryActions[]` with
structured steps to verify the source/install version, plan adoption from a
current artifact, or request an explicit operator exception. Consumers must treat
`installFreshnessAuthoritative=false` as a hard claim blocker even when candidate
evidence otherwise looks safe.

For explicit `--candidate <bead-id>` queries, `recommendedSafeToClaim` must be
candidate-scoped before `safeToClaim` can become `true`. A packet-level
recommendation for a different Bead must not satisfy this condition or unlock
`claimCommandAction`.

If BV names a Bead that is blocked in Beads, or the explicit candidate returns
`candidate_not_found`, consumers must stop at inspection and run the Beads
show/read path for that ID. Do not reinterpret the BV recommendation as a
claimable lane. When the reason is unclear, run
`ee diag environment-attestation --workspace . --include-rch --json` and inspect
the Beads/BV source-authority entries before choosing a static/docs fallback.

When the selected candidate is blocked only because Agent Mail evidence is
missing, the gate should expose read-only fallback actions for:

1. generating a redacted `ee.agent_mail.snapshot.v1` with
   `scripts/agent_mail_snapshot.sh`;
2. retrying the same command with `--agent-mail-snapshot`; and
3. inspecting `sourceAuthority.agentMailStatus`, `unsafeReasons`,
   `staleReasons`, and `degradedCodes`.

The retry command shape is:

```bash
ee swarm work-packet --workspace . --include-rch \
  --agent-mail-snapshot /private/tmp/ee-agent-mail-snapshot.json \
  --claim-gate --candidate <bead-id> --json
```

`sourceAuthority.agentMailStatus=fresh` (or legacy `healthy`) means the gate
consumed current redacted Agent Mail evidence. That only removes the
missing-evidence blocker. The claim command remains absent unless
`safeToClaim=true`, `verdict=safe_to_claim`, reservation and inbox evidence are
authoritative, RCH admission is compatible with the work, and no candidate
`unsafeReasons` or `staleReasons` remain. Snapshot evidence must not turn an
active reservation conflict, stale tracker, Beads/BV disagreement, or RCH
proof-environment blocker into a claimable lane.

`nextCommandActions[]` is restricted to non-mutating inspection commands:
`mutatesState` must be `false`. The mutating Beads claim command, when one is
safe to show, lives only in `claimCommandAction`. When `safeToClaim` is `false`,
`claimCommandAction` must be `null`.

Non-goals for the claim gate: it does not update Beads, reserve files, send
Agent Mail, run Cargo, mutate Git, delete files, or bypass human approval for
destructive actions. It only makes the existing packet decision mechanically
checkable by agent harnesses.

## Agent Mail fallback semantics (bd-2z5ly.8)

`coordination.agentMail` carries enough redacted health metadata for a
downstream candidate-safety classifier to choose a conservative posture without
ever reading mail bodies or raw inbox contents:

- `status` is one of `fresh`/`healthy`, `degraded_read_only`,
  `archive_ahead_of_sqlite`, `inbox_unavailable`, `reservation_unavailable`,
  `outbox_only`, `unreachable`, `unavailable`, or `skipped`. `fresh` and
  `healthy` are aliases; new emitters should prefer `healthy`.
- `recoveryMode` advises the next-step posture: `wait_for_repair`,
  `proceed_via_beads`, `static_work_only`, `manual_coordination`, or `none`.
- `archiveIndexParity` summarises Agent Mail JSONL archive vs SQLite index
  drift: `aligned`, `archive_ahead`, `sqlite_ahead`, or `unknown`.
- `reservationAuthoritative` and `inboxAuthoritative` tell the consumer whether
  reservation evidence or unread/ack counts in this packet can be trusted.
  When either flag is `false` or `null`, candidate safety MUST downgrade
  confidence rather than treating a missing or zero count as evidence that no
  peer conflict exists.
- Recovery and durability signals have the same authority as semantic
  readiness. If Agent Mail reports `recovery.mode=corrupt`, a non-ok
  `recovery.status`, or `durability_state=corrupt`, reservation and inbox
  evidence are non-authoritative even when transport health is green and
  `semantic_readiness.status` is `ok`. Emit bounded reason classes such as
  `archive_corruption` or `storage_recovery_required`; never expose database
  paths, SQLite filenames, B-tree/page offsets, recovery bundle paths, or raw
  repair text.
- `fallbackActions` is an ordered, structured workflow keyed by `kind`. The
  array is sorted lexicographically by `kind` so the packet stays deterministic
  across runs. Action kinds: `beads_comment`, `manual_coordination`,
  `record_only`, `retry_later`, `support_bundle`, `switch_to_static_work`.
  This replaces prose-only repair strings so harnesses can branch mechanically
  instead of parsing natural language.

Redaction invariant: `fallbackActions[].summary`, `command`, and `manualStep`
MUST NOT include raw inbox bodies, message IDs, headers (`From:`, `Subject:`,
`Message-ID:`), agent identities, or unredacted reservation paths. The
`agent_mail_degraded_read_only` and `agent_mail_recovery_corrupt` fixtures under
`tests/fixtures/swarm_work_packet/` show the canonical degraded shapes; the
`work_packet_agent_mail_fallback_semantics_are_contractual` lifecycle test
fences these properties.

## Shell-safe agent command actions (bd-13dmm.3)

Every agent-actionable command in the work packet now has a structured
`commandAction` representation alongside the legacy human-readable
`commandTemplate` string. The structured shape lets a harness execute
the recommended next step without invoking a shell.

- `commandAction` is defined under `definitions/commandAction` in
  `docs/schemas/swarm/ee.swarm.work_packet.v1.json`. Required fields:
  - `commandId` — stable, dot-delimited identifier.
  - `displayCommand` — single-line human-readable form. Must use the
    `safeCommandString` shape (no shell metacharacters, mail headers,
    raw home paths, or secret-looking tokens).
  - `argv` — exact argv vector to execute. Each entry uses the
    `safeCommandString` redaction guard; this is the only field a
    consumer should pass to `Command::new`/`spawn`.
  - `shellRequired` — `false` for safe argv execution; `true` for
    commands that genuinely need shell evaluation.
  - `copySafety` — one of `safe_structured_argv`, `display_only`,
    `shell_required_review`, `forbidden_until_human_approval`. The
    schema's `allOf` cross-check forbids `shellRequired=true` paired
    with `safe_structured_argv`, and forces `shell_required_review`
    or `forbidden_until_human_approval` whenever `shellRequired` is
    `true`.
  - `mutatesState` — `true` for any command that writes Beads, sends
    Agent Mail, mutates git, runs Cargo, or otherwise changes durable
    state. Consumers must require explicit confirmation before
    invoking a mutating action without prior human review.
  - `requiredSubstrate` — `agent_mail`, `beads`, `bv`, `ee`, `git`,
    `human`, `jq`, `rch`, `static_local`, or `none`.
  - `when` — short trigger predicate (also `safeCommandString`).
  - `rationale` — one-line reason (max 240 chars). Must not embed PEM
    blocks, GitHub PATs, `DATABASE_URL=` literals, or mail headers.

- `recommendedAction.suggestedCommandActions[]` is the canonical
  argv-bearing surface for agent-recommended next steps.
  `recommendedAction.suggestedCommands[]` remains for human display
  during migration but MUST NOT be passed to a shell — consumers
  prefer `suggestedCommandActions` when both are present.

- `verification.requiredCommands[].commandAction` and
  `verification.staticChecks[].commandAction` carry the same shape so
  a harness can replay verification commands without parsing the
  legacy `commandTemplate` string. The existing `commandTemplate`
  field is now explicitly marked legacy display-only in the schema
  description.

Redaction invariant: every `safeCommandString` slot (`displayCommand`,
each `argv[]` entry, `when`, `rationale`) blocks raw home paths,
PEM blocks, GitHub PATs, `DATABASE_URL=` strings, and mail headers
(`From:`, `Subject:`, `Message-ID:`). The
`work_packet_command_actions_require_shell_safe_argv_contract`
lifecycle test fences the definition + every reference site + the
legacy `commandTemplate` marker text.

Tracking Bead: `bd-2z5ly.2`
