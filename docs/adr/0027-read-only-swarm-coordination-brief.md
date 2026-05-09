# ADR 0027: Read-Only Swarm Coordination Brief

Status: proposed
Date: 2026-05-09

## Context

Swarm-scale agent sessions often run 6-20 agents across multiple tmux panes,
each working on separate beads, files, or repositories. Agents encounter
coordination questions they cannot answer from their own state:

1. Which beads are already claimed by other panes?
2. What files have reservations held by other agents?
3. Is the shared build machine under pressure from parallel jobs?
4. Are there unread Agent Mail messages relevant to my work?
5. What is the overall swarm health and convergence state?

Today agents must shell out to `br`, `ntm`, `rch status`, and Agent Mail
individually, then parse and merge those outputs. This is error-prone and
varies across harnesses.

Existing surfaces do not fully cover this need:

- **ADR 0017 (resource governance)** defines profile budgets but does not
  aggregate cross-pane state.
- **ADR 0023 (host profiles)** reports local resources, not swarm pressure.
- **ADR 0024 (performance forensics)** compares artifacts, not live state.
- **Handoff capsules** transfer state to a new agent, not inspect peers.
- **Support bundles** are heavyweight artifacts for debugging, not preflight.
- **Agent Mail/BV/Beads** each report their own domain, not a merged brief.

A lightweight, read-only coordination brief would let agents preflight their
work and avoid stepping on peers without becoming an agent scheduler.

## Decision

`ee` will add a read-only swarm coordination brief command. The brief
aggregates advisory coordination state from external sources into a single
JSON response. It is explicitly advisory and non-mutating.

### Command Shape

```
ee swarm brief --json [--workspace <path>] [--sources <comma-list>]
```

Stdout is `ee.response.v1` carrying `ee.swarm.brief.v1` data. Stderr receives
progress and diagnostics. The command is read-only: it must not claim beads,
reserve files, release reservations, send mail, run builds, mutate `ee` state,
or schedule agents.

### Source Boundaries

The brief may query these optional sources:

| Source | What it contributes | Default | Degradation |
|--------|---------------------|---------|-------------|
| `beads` | Open/in-progress beads, blocked chains, assignees | enabled | list unavailable beads, report `beads_unavailable` |
| `bv` | BV session state, pane assignments, convergence | enabled | report `bv_unavailable` |
| `agent_mail` | Unacknowledged inbox count, reservation map | enabled | report `agent_mail_unavailable` |
| `git` | Dirty files, recent commits, branch | enabled | report `git_unavailable` |
| `rch` | Build slot status, queue depth | opt-in | report `rch_unavailable` |
| `host_profile` | CPU/memory pressure, profile tier | enabled | report `profile_unavailable` |
| `ee_agent_status` | Local `ee agent status` inventory | enabled | report `agent_status_unavailable` |

Sources are queried in parallel where possible. A source failure degrades the
brief but does not fail the command unless all sources fail.

### Advisory Outputs

The brief includes:

- **claimed_beads**: IDs of beads with `in_progress` status and their pane/agent
- **blocked_chains**: beads that are transitively blocked by in-progress work
- **file_reservations**: paths with active Agent Mail reservations and holders
- **inbox_unread**: count of unacknowledged messages per mailbox
- **git_dirty**: files with uncommitted changes (paths only, no diffs)
- **recent_commits**: last N commits on current branch (subject lines only)
- **rch_pressure**: build slot count, queue depth, estimated wait
- **host_pressure**: CPU/memory utilization tier, profile recommendation
- **agent_inventory**: agents seen by `ee agent status`
- **convergence_hint**: advisory signal (e.g., "mostly_converged", "active")
- **coordination_warnings**: actionable advisories (overlap, contention)

### Privacy Rules

The brief must not include:

- Raw Agent Mail message bodies or recipient lists
- Raw transcript text from recorder or CASS
- Environment variable values beyond presence flags
- Raw secret-like spans (API keys, credentials, tokens)
- Arbitrary path listings beyond declared reservations and dirty files
- Unredacted support artifacts or handoff payloads
- Commit diffs, file contents, or repository content beyond metadata

The `redactionStatus` field indicates the posture (e.g.,
`paths_counts_subjects_only_no_content`).

### Overlap Note

This brief is distinct from other surfaces:

- **Handoff capsules** are for state transfer when handing off to a new agent.
  The brief is for inspecting peers during concurrent work.
- **Support bundles** are heavyweight artifacts for post-incident debugging.
  The brief is lightweight preflight for active sessions.
- **Performance forensics** compares historical artifacts. The brief reports
  live coordination state.
- **Profile reports** describe host resources. The brief includes host pressure
  as one input among several.
- **Agent Mail/BV/Beads** each report their own domain. The brief merges them
  into a single advisory response.

The brief does not replace any of these surfaces. It aggregates a subset of
their output into a preflight-oriented summary.

## Consequences

Agents gain a single command for preflight coordination checks. This reduces
ad-hoc shell parsing and makes swarm behavior more consistent across harnesses.

Implementation obligations:

- The command must remain read-only. No mutations, no reservations, no mail.
- Source queries should have short timeouts with graceful degradation.
- JSON output must use versioned schemas and stable field names.
- Privacy rules must be enforced before any output, not just at rendering.
- The brief is advisory. Agents must still use the primary tools (br, ntm,
  Agent Mail) for actual coordination actions.

## Rejected Alternatives

- **Make `ee` an agent scheduler.** Out of scope. `ee` is a memory substrate,
  not a tmux orchestrator. NTM and harness-side tools own scheduling.

- **Duplicate BV/Agent Mail/Beads functionality in `ee`.** The brief queries
  these systems, not reimplements them. If they change, the brief adapts.

- **Include raw diffs, mail bodies, or transcripts.** Privacy risk. The brief
  is designed for preflight, not deep inspection. Use support bundles for that.

- **Require all sources to succeed.** Too fragile. Swarm agents often run in
  partial states. Graceful degradation is essential.

- **Make the brief a daemon or push-based service.** Adds complexity. A
  one-shot CLI command fits the existing `ee` model and agent harness patterns.

## Verification

The decision remains true when the `eidetic_engine_cli-1r8g` track proves:

1. `eidetic_engine_cli-c5p2` lands this ADR before implementation starts.
2. `eidetic_engine_cli-abwd` normalizes source queries with stable degradation
   codes and timeout handling.
3. `eidetic_engine_cli-u7r5` adds non-overlap and resource-pressure
   recommendations without mutating any state.
4. `eidetic_engine_cli-v731` exposes the CLI surface with JSON stdout, stderr
   diagnostics, and effect classification as read-only.
5. `eidetic_engine_cli-pdav` freezes schema and golden contracts for stable
   and degraded responses.
6. `eidetic_engine_cli-8x4x` proves real-binary E2E coordination scenarios
   with logged artifacts.
7. `eidetic_engine_cli-9xv0` documents operator workflows only after behavior
   works.
8. `eidetic_engine_cli-pswb` adds redacted support-bundle summaries for swarm
   coordination diagnostics.
9. `br dep cycles --json` remains empty for the track.
10. Forbidden-dependency audits continue to pass.
