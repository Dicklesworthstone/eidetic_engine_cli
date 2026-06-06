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

## Command Contract

The agent-facing command is:

```bash
ee diag environment-attestation --workspace . --include-rch --json
```

Use an explicit `--workspace` in harnesses. JSON mode wraps the payload in an
`ee.response.v2` envelope and places the `ee.environment_attestation.v1` report
under `data`. Human or TOON renderers may summarize the same facts, but they are
not the contract for automation.

The command is read-only and daemon-optional. It may read local diagnostic
state, source-control status, Beads/BV state, Agent Mail probe or redacted
snapshot state, RCH readiness, build-admission posture, and file-reservation
metadata that the CLI can observe. It must not claim Beads, reserve or release
files, send Agent Mail, run Cargo, rebuild binaries, mutate the EE store, mutate
git, or start a daemon.

## Source Authority

Every entry in `sourceAuthority[]` describes one readiness source:

- installed binary command surface
- source tree HEAD and dirty state
- Beads DB/JSONL tracker state
- BV recommendation freshness
- Agent Mail MCP state and probe state
- RCH availability and source materialization
- build admission and local Cargo tripwire posture
- CI proof-lane artifact source authority
- host profile, claim gate, file reservations, and support-bundle redaction

Each source has an `authority` value of `authoritative`, `advisory`,
`degraded`, `stale`, `unavailable`, or `contradicted`. A source can be useful
without being authoritative. For example, metadata-only Beads drift can remain
visible as degraded context without making ordinary `br` reads non-authoritative.

The `sourceAuthority[]` entry is the place to look before trusting a compact
summary verdict:

| Field | Agent interpretation |
| --- | --- |
| `source` | Stable substrate label such as `beads_tracker`, `bv_recommendation`, `agent_mail_probe`, `rch`, `local_cargo_tripwire`, `ci_proof_lane`, or `file_reservations`. |
| `authority` | Whether the source may decide readiness, is only advisory, is stale, is unavailable, or contradicts another source. |
| `status` | The observed state of that source, for example `ok`, `remote_ready`, `remote_blocked`, `stale`, `blocked`, or `contradicted`. |
| `freshness` | Whether the observation is current enough to use. |
| `degradedCodes[]` | Stable codes explaining why a source was downgraded. |
| `recoveryActions[]` | Structured recovery or inspection actions. Do not execute mutating actions without the coordination and approval required by their substrate. |

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
`environment_blocked_before_source`, not as `source_failed`. CI proof-lane
artifact evidence is artifact source authority: stale, missing, cancelled,
checksum-mismatched, or surface-probe-failed artifacts fail closed as source
authority blockers instead of becoming source test failures.

| Verdict | Meaning | Agent action |
| --- | --- | --- |
| `safe_to_claim` | Required readiness sources are authoritative and no conflict/blocker code was observed. | The attestation is compatible with claiming, but the live claim gate still decides the actual Beads mutation. |
| `remote_verification_admitted` | RCH/readiness evidence says remote Cargo verification can be launched or admitted as proof. | Treat `summary.safeToClaim=true` as environment readiness only; this is not proof that source compile/tests passed. |
| `coordinate_before_claim` | A source is degraded but not a hard proof blocker, such as dirty checkout, Agent Mail unavailable, Agent Mail probe mismatch, or stale BV advisory evidence. | Inspect and coordinate before mutating Beads or reservations. |
| `unsafe_due_to_conflict` | Active or stale reservation evidence makes the selected work surface unsafe. | Do not claim; coordinate through Agent Mail or wait for reservation expiry. |
| `proof_environment_blocked` | RCH/build-admission/source-materialization blocked before source verification could run. | Preserve the exact blocker and do not replace it with local Cargo proof. |
| `source_authority_ambiguous` | The collector could not map enough source evidence into authoritative readiness. | Run the listed read-only inspection commands and avoid claims until the ambiguity is resolved. |
| `stale_binary_suspected` | The installed `ee` command surface appears older than the source/docs contract. | Stop at inspection and coordinate an approved RCH or release-path rebuild. |
| `tracker_stale` | Beads tracker state is stale or inconsistent enough to decide against claiming. | Use the structured Beads recovery action only when coordination permits it. |
| `local_cargo_bypass_detected` | A local Cargo verification process or fallback contradicted remote-only proof policy. | Treat as high severity and require a human decision; do not count local Cargo output as source proof. |
| `unknown_insufficient_evidence` | The collector did not observe enough readiness sources. | Gather source-authority inputs first. |

## Degraded Code Map

The current severity policy is mechanical: RCH/build-admission blockers and
local Cargo bypass are `high`; other attestation degradation codes are
`warning`. The `repair` field is either a structured read-only command, a
coordination step, or `null` when the next action requires human judgment.

| Code | Severity | Trigger | Repair and source-authority meaning |
| --- | --- | --- | --- |
| `agent_mail_unavailable` | warning | Agent Mail evidence could not be observed. | Coordinate out of band or provide a redacted snapshot; do not treat missing Mail evidence as no reservations. |
| `agent_mail_probe_mismatch` | warning | Agent Mail probe and semantic readiness evidence disagree. | Inspect the probe/snapshot source and coordinate before claim. |
| `beads_tracker_stale` | warning | Beads DB/JSONL tracker content may be stale. | Structured repair may name `br sync --import-only`; it mutates Beads coordination state and requires coordination. |
| `beads_metadata_only_stale` | warning | Beads metadata is stale while content remains synchronized. | Treat as degraded context and refresh metadata when safe. |
| `bv_recommendation_stale` | warning | BV recommendation evidence is stale, timed out, or unavailable. | Prefer bounded Beads fallback such as `br --no-auto-import --allow-stale ready --json`; BV cannot override Beads. |
| `rch_unavailable` | warning | RCH readiness evidence was not collected or unavailable. | Inspect RCH posture before treating remote proof as admitted. |
| `rch_worker_topology_blocked` | high | RCH topology blocked before Cargo. | Run read-only RCH inspection such as `rch status --json`; report the blocker as environment-readiness failure. |
| `rch_source_materialization_blocked` | high | RCH could not materialize the source for remote verification. | Fix the remote proof environment; do not mark source compile/test failed. |
| `rch_remote_required_fallback_prevented` | high | Remote-required mode refused local fallback. | Preserve the refusal as proof-environment blocked. |
| `stale_binary_suspected` | warning | Installed binary help/version does not match current source contract. | Inspect `ee --version` and coordinate an approved RCH or release-path rebuild; do not bypass with local Cargo install. |
| `source_authority_ambiguous` | warning | Required evidence could not be mapped into authoritative sources. | Run the listed read-only collection command, commonly `ee swarm brief --workspace . --include-rch --json`. |
| `local_cargo_bypass_detected` | high | Local Cargo verification was observed where remote-only proof is required. | Require human decision; local output is not valid proof for source closeout. |
| `dirty_checkout_observed` | warning | Dirty source tree paths were observed. | Inspect `git status --short --branch --untracked-files=all` and coordinate ownership before claiming. |
| `build_admission_blocked` | high | Disk-pressure or build-admission policy blocked remote-only verification. | Use build-admission/RCH inspection and report as environment blocked. |
| `support_bundle_redaction_unverified` | warning | Support-bundle redaction posture was not verified. | Verify redaction before attaching bundle evidence. |
| `reservation_evidence_stale` | warning | File reservation evidence is stale or conflicting. | Coordinate through Agent Mail; do not treat stale reservations as absent. |
| `ci_proof_lane_artifact_missing` | warning | A CI proof-lane run completed but the expected artifact is unavailable. | Treat the artifact as absent and file or coordinate workflow follow-up before reuse. |
| `ci_proof_lane_artifact_stale` | warning | A CI proof-lane artifact source SHA does not match the requested repository head SHA. | Do not reuse the artifact; coordinate before dispatching or waiting for a current-head run. |
| `ci_proof_lane_cancelled_before_artifact` | warning | The CI run was cancelled before artifact upload completed. | Treat as proof-lane abstention and use a non-cancelling artifact lane or follow-up bead. |
| `ci_proof_lane_checksum_mismatch` | high | The artifact checksum did not verify. | Reject the artifact and repair the proof lane before using binary evidence. |
| `ci_proof_lane_surface_probe_failed` | high | The artifact failed the required command-surface probe. | Reject the artifact until the expected `ee` surface is proven by the lane. |
| `ci_proof_lane_unknown_source` | warning | GitHub Actions or matching-run evidence was unavailable or unknown. | Coordinate before dispatching a new lane; missing source authority is not permission to reuse stale binaries. |
| `ci_proof_lane_duplicate_dispatch` | warning | Multiple active proof-lane dispatches target the same source authority. | Reuse or wait for one active run; do not dispatch another lane blindly. |

## Support Bundle and Handoff Summary

`ee support bundle` writes `environment_attestation_summary.json` as a
redaction-safe projection of `ee.environment_attestation.v1`. Handoff preview,
create, and resume surfaces embed the same projection under
`environment_attestation_summary`.

The summary keeps only compact source-authority posture: verdicts,
`sourceAuthority` statuses, status/authority counts, degraded codes, recovery
action kinds, redacted command display text, command `argv` hashes, first-failure
diagnosis, evidence-reference hashes, and the proof-admission block. It does not
include raw Agent Mail bodies, raw source snippets, raw command `argv`, raw
evidence references, or host-private absolute paths.

Interpret the embedded block as diagnostic context, not as current permission to
claim or close work. In particular, `proofAdmission.remoteVerificationAdmitted`
is separate from `proofAdmission.sourceTestVerdict`: an RCH environment can be
ready while source tests have not run, and an RCH topology/materialization
failure is an environment blocker rather than a source failure. Before mutating
Beads, reservations, or closeout state, rerun:

```bash
ee diag environment-attestation --workspace . --include-rch --json
```

When the summary reports Beads/BV/Agent Mail or claim-gate disagreement, treat
the `disagreementEvidence` booleans and `firstFailure` code as routing hints.
They explain which authority is stale, ambiguous, contradicted, or blocked; they
do not override Agent Mail reservations or the live claim gate.

CI proof-lane evidence appears in support bundles as a compact
`ci_proof_lane` source-authority entry. The retained fields are workflow
path/name, run id, job id, requested head SHA, run head SHA, artifact name,
checksum status, source/artifact freshness, surface probe status, and first
failure diagnosis. The bundle stores evidence-reference hashes and redacted
metric values only; it does not copy artifacts, raw logs, local paths, mail
bodies, or shell output.

## Recovery Actions

`recoveryActions[]` is structured. Each action has a priority, kind, optional
structured command, mutation flag, required substrate, and rationale. Mutating
actions remain explicit; the attestation itself is read-only and must not claim
Beads, reserve files, run Cargo, rebuild binaries, or send Agent Mail.

The intended flow is:

```bash
ee diag environment-attestation --workspace . --include-rch --json
ee swarm brief --workspace . --include-rch --json
ee swarm work-packet --workspace . --include-rch --claim-gate --candidate <id> --json
ee schema export ee.environment_attestation.v1
```

The first implementation of a collector should consume existing read-only
surfaces rather than duplicate their internals. In particular, claim-gate
`sourceAuthority` is a summary projection; an environment attestation should
preserve the per-source inventory that explains why a source is authoritative,
advisory, stale, unavailable, or contradicted.

`ee swarm work-packet --claim-gate` remains the first surface to check before a
Beads claim. Use `ee diag environment-attestation` when the claim gate,
support-bundle summary, or handoff evidence looks contradictory and you need the
per-source reason. Support bundles are for redacted handoff and debugging
artifacts; they do not replace a fresh live claim gate or a fresh attestation.

## Agent Routing Checklist

Use this order when a crowded checkout has contradictory readiness evidence:

1. Run the live claim gate for the exact Bead you intend to claim.
2. If `safeToClaim=true`, inspect `claimCommandAction`, reserve the relevant
   files through Agent Mail, then mutate Beads through the structured command.
3. If `safeToClaim=false`, inspect this attestation report before acting on a
   fallback. `summary.environmentVerdict` tells you why the gate is blocked;
   `sourceAuthority[]` tells you which substrate produced that blocker.
   When the blocker is `agent_mail_unavailable`, generate a redacted
   `ee.agent_mail.snapshot.v1` and retry the same claim-gate candidate with
   `--agent-mail-snapshot`; do not treat missing Agent Mail as empty
   reservations.
4. If BV recommends a Bead that Beads reports as blocked, assigned, or missing
   from ready work, Beads plus the claim gate wins. Treat the BV command as
   stale advisory text and do not paste its claim command.
5. If a support bundle or handoff says a lane was safe earlier, treat it as
   historical evidence only. Re-run the live claim gate and this attestation
   before claiming, closing, or reopening work.
6. If the only available progress is a human-directed static/docs slice while
   the claim gate is closed, keep the slice non-overlapping: reserve exact files,
   announce the exception, avoid Beads mutation while tracker files are
   reserved, and record that no Cargo/source proof was produced.

## Agent Onboarding Examples

These are redacted excerpts. Fields not relevant to the decision are omitted.

### Stale binary surface

```json
{
  "summary": {
    "safeToClaim": false,
    "environmentVerdict": "stale_binary_suspected",
    "sourceTestVerdict": "not_evaluated"
  },
  "sourceAuthority": [
    {
      "source": "installed_binary",
      "authority": "contradicted",
      "status": "stale",
      "degradedCodes": ["stale_binary_suspected"],
      "recoveryActions": [
        {
          "kind": "inspect",
          "command": {
            "displayCommand": "ee --version",
            "argv": ["ee", "--version"],
            "shellRequired": false,
            "copySafety": "safe_structured_argv"
          },
          "mutatesState": false,
          "requiredSubstrate": "static_local"
        }
      ]
    }
  ]
}
```

Action: stop at inspection. Coordinate an approved RCH or release-path rebuild
before using binary output to claim or close work.

### Agent Mail probe mismatch

```json
{
  "summary": {
    "safeToClaim": false,
    "environmentVerdict": "coordinate_before_claim"
  },
  "sourceAuthority": [
    {
      "source": "agent_mail_probe",
      "authority": "degraded",
      "status": "contradicted",
      "freshness": "current",
      "degradedCodes": ["agent_mail_probe_mismatch"],
      "recoveryActions": [
        {
          "kind": "coordinate",
          "command": null,
          "mutatesState": false,
          "requiredSubstrate": "agent_mail"
        }
      ]
    }
  ]
}
```

Action: use live Agent Mail or a redacted snapshot. Do not infer that no one
holds reservations from a failed or contradictory probe.

Snapshot-backed retry:

```bash
CANDIDATE=bd-example.1
SNAPSHOT_PATH=/private/tmp/ee-agent-mail-snapshot.json
scripts/agent_mail_snapshot.sh --project "$PWD" --agent "$AGENT_NAME" \
  --output "$SNAPSHOT_PATH"
ee swarm work-packet --workspace . --include-rch \
  --agent-mail-snapshot "$SNAPSHOT_PATH" \
  --claim-gate --candidate "$CANDIDATE" --json
```

The retry is still read-only evidence collection. It may change the claim-gate
`sourceAuthority.agentMailStatus` to `fresh` or `healthy`; it must not override
remaining reservation conflicts, stale tracker evidence, Beads/BV disagreement,
RCH blockers, or a false `safeToClaim`.

### Beads and BV disagreement

```json
{
  "summary": {
    "safeToClaim": false,
    "environmentVerdict": "tracker_stale"
  },
  "sourceAuthority": [
    {
      "source": "beads_tracker",
      "authority": "stale",
      "status": "stale",
      "degradedCodes": ["beads_tracker_stale"]
    },
    {
      "source": "bv_recommendation",
      "authority": "degraded",
      "status": "degraded",
      "degradedCodes": ["bv_recommendation_stale"]
    }
  ]
}
```

Action: Beads plus the claim gate wins. Ignore stale BV copy-paste claim
commands until a fresh Beads read and claim gate agree.

### RCH environment blocker

```json
{
  "summary": {
    "safeToClaim": false,
    "remoteVerificationAdmitted": false,
    "sourceTestVerdict": "environment_blocked_before_source",
    "environmentVerdict": "proof_environment_blocked"
  },
  "sourceAuthority": [
    {
      "source": "rch",
      "authority": "degraded",
      "status": "remote_blocked",
      "degradedCodes": ["rch_worker_topology_blocked"]
    }
  ],
  "degraded": [
    {
      "code": "rch_worker_topology_blocked",
      "severity": "high",
      "repair": "rch status --json"
    }
  ]
}
```

Action: report the exact RCH blocker. Do not call this a source failure and do
not use local Cargo as replacement proof.

### Dirty checkout or reservation conflict

```json
{
  "summary": {
    "safeToClaim": false,
    "environmentVerdict": "unsafe_due_to_conflict"
  },
  "sourceAuthority": [
    {
      "source": "file_reservations",
      "authority": "advisory",
      "status": "blocked",
      "degradedCodes": ["reservation_evidence_stale"],
      "recoveryActions": [
        {
          "kind": "coordinate",
          "command": null,
          "mutatesState": false,
          "requiredSubstrate": "agent_mail"
        }
      ]
    }
  ]
}
```

Action: coordinate with the holder or wait for expiry. A dirty or reserved
surface is not safe to overwrite because the report is read-only.

### Local Cargo bypass

```json
{
  "summary": {
    "safeToClaim": false,
    "environmentVerdict": "local_cargo_bypass_detected",
    "sourceTestVerdict": "not_evaluated",
    "localCargoFallbackObserved": true
  },
  "sourceAuthority": [
    {
      "source": "local_cargo_tripwire",
      "authority": "contradicted",
      "status": "blocked",
      "degradedCodes": ["local_cargo_bypass_detected"],
      "recoveryActions": [
        {
          "kind": "human_decision",
          "command": null,
          "mutatesState": false,
          "requiredSubstrate": "human"
        }
      ]
    }
  ]
}
```

Action: preserve the high-severity contradiction. Local Cargo output is not
proof for remote-required verification closeout.
