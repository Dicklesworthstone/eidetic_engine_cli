# ADR 0038: Optional Zero-Touch Tailscale Mesh Auto-Enrollment

Status: proposed
Date: 2026-05-16
Bead: bd-36bbk.1 (SRR6.46 epic; sub-beads bd-36bbk.1.1 through bd-36bbk.1.20)
Supersedes: none
Builds on: ADR 0037 (Optional Mesh Memory)

## Context

ADR 0037 establishes that SRR6 mesh memory is an optional, local-first cache plus
explicit revision channel that composes with — but never replaces — local
FrankenSQLite truth. It also pins the invariants that make mesh-disabled mode
zero-cost: no listener, no peer config, no network activity, byte-stable local
command output.

SRR6.46 is the **UX layer** that sits on top of SRR6's primitives:

- SRR6.9 (`bd-1o1v5`) provides the Tailscale transport adapter.
- SRR6.5 (`bd-29ulx`) provides the peer trust, lane policy, and redaction model.
- SRR6.24 (`bd-1x87h`) provides peer enrollment with capability handshake and
  key rotation.
- SRR6.30 (`bd-2jb3s`) provides workspace scope and peer-group binding.
- SRR6.8 (`bd-2wngl`) provides the foreground `ee mesh` CLI surface.

A user with `tailscaled` already running and authenticated, sharing a tailnet
with other ee instances, should not need to read peer-group docs, generate
identity keys, or hand-edit configuration to start sharing memory. The
ergonomic floor for "optional but useful" is zero manual setup.

Without an explicit auto-enrollment UX layer, mesh is gated behind operator
expertise: the user must know that SRR6.5 trust lanes exist, that SRR6.30
workspace scopes need binding, that SRR6.24 enrollment has a handshake
sequence, that SRR6.8 has a `sync-once` trigger, and that all of these need to
be composed in the correct order with the correct defaults. Most users will
never enable mesh at all if that's the floor.

The constraint is that "zero manual setup" must not become "silent mutation."
AGENTS.md is explicit that ee never silently rewrites memory state; every
durable change must be auditable. The naive solution — a confirmation prompt
before enrollment — defeats the agent-first UX (agents cannot answer y/N
prompts cleanly). The naive alternative — eager auto-enrollment without
disclosure — violates the audit invariant.

This ADR records the design decisions that resolve those tensions, plus the
related decisions about identity guards, discovery cadence, response
ergonomics, and security surfaces that the 20 SRR6.46 sub-beads cover.

## Decision

SRR6.46 is an optional UX layer that turns a healthy `tailscaled` posture into
materialized peer-group configuration without prompts and without silent
mutation. The decisions below are load-bearing — they're the reasoning future
SRR6.46 contributors should not re-litigate without an explicit ADR amendment.

### D1: Full automation, not a wizard

When the user sets `EE_MESH_ENABLED=1` and `tailscaled` is reachable with
peers, running `ee mesh auto-enroll --workspace . [--json]` materializes the
peer-group binding end-to-end in one command. No interactive prompts. No
multi-step wizard.

**Why**: A wizard UX optimizes for first-time human readers; an agent harness
cannot drive an interactive wizard. The user-first instinct ("walk them
through it") loses to the agent-first reality ("emit machine-parseable JSON
that an LLM consumes directly"). Forcing every implementation of SRR6.46 to
also implement a CLI wizard would double the surface area and double the
test matrix.

### D2: Consent via forensic audit row, not prompt

Before any durable write, SRR6.46.5 (`bd-36bbk.1.5`) emits an
`ee.mesh.auto_enrollment_summary.v1` audit row that records exactly what the
caller intends to enroll — workspace id, tailnet id, intended peers (sorted),
intended lane policy, trigger reason, prior peer-group id and hash (when
overwriting), the literal `ee mesh disable` command an operator can copy-paste
to undo, and a `summaryHash` for forensic correlation. The row is emitted
**even for dry-run invocations** (with `triggerReason=dry_run_preview`,
`materializationOutcome=dry_run`) and **even for idempotent no-op invocations**
(with `triggerReason=drift_reconciliation`, `materializationOutcome=audit_only`)
so the audit log records every attempted enrollment, not only successful
materializations.

If audit emission fails for any reason, SRR6.46.3 must abort with
`auto_enrollment_audit_failed` (critical severity) and must not touch the
peer-group table. **Fail-closed by construction.**

**Why**: The audit row is the user's continuous-time consent record. An
operator reviewing `ee audit timeline --event-type mesh.auto_enrollment_intended
--json` after the fact sees every attempted enrollment with the literal
reversal command embedded — strictly more informative than a single y/N prompt
at first-run time. The fail-closed property guarantees a silent enrollment is
impossible: no peer-group write happens before its forensic precursor row.

### D3: Multi-workspace = own peer-group row per workspace

When the same machine hosts two workspaces auto-enrolling on the same tailnet
with the same peer set, each workspace gets its **own** peer-group row. The
peer-group lifecycle stays workspace-scoped.

**Why**: Workspaces are the natural unit of SRR6.30 binding. Sharing one
peer-group across workspaces would couple their lifecycles: `ee mesh disable
--workspace A` could not detach from peers without also affecting workspace
B, and lane policy widening on B (`ee mesh grant ... --lane body --workspace
B`) would silently expand exposure for A. The cost — a small number of
duplicate peer-group rows in the typical n<10 workspaces per machine — is
acceptable compared to the lifecycle entanglement an alternative would
introduce. SRR6.46.9 (`bd-36bbk.1.9`) handles the multi-binding inverse
correctly: it unlinks the workspace binding and only deletes the peer-group
row when no other binding remains.

### D4: Hello responder lives inside `ee daemon`, not a new `ee mesh serve`

The SRR6.46.6 (`bd-36bbk.1.6`) hello protocol's responder side runs as a
supervised job inside the existing foreground `ee daemon` (SRR6.8 / bd-2wngl).
When `EE_MESH_ENABLED=1` and `EE_MESH_HELLO_RESPONDER_DISABLED=0` (default),
the daemon registers a `MeshHelloResponder` supervised job that binds to the
local tailscale interface on a tailnet-only port (default 41888,
configurable via `EE_MESH_HELLO_PORT`). When the daemon is not running, the
auto-status view (SRR6.46.4) emits a `hello_responder_not_running` (medium
severity) degraded code with the literal `ee daemon --foreground` repair
command. SRR6.46.12 (`bd-36bbk.1.12`) owns the lifecycle, supervision, and
audit emission for responder start/stop/crash-restart.

**Why**: Reusing the existing daemon's supervision tree, write-owner gating,
and observability surfaces (status + health) avoids the second-binary-to-manage
problem and the split "is mesh on?" responsibility between two processes.
Users already run `ee daemon` for SRR6.8 sync-once and the SRR6.46.14 steward;
piggybacking is cheap. A new `ee mesh serve` command would duplicate
supervision and double the install-and-run surface for marginal architectural
benefit.

### D5: Identity guard covers tailnet AND node-key (backup-restored class)

SRR6.46.8 (`bd-36bbk.1.8`) refuses to apply auto-enrollment config when EITHER
the bound `tailnetId` differs from the current `tailscaled` probe's
`tailnetId` (user switched tailnets) OR the bound `materializedOnNodeKey`
differs from the current `selfNodeKey` (database was restored from a backup
taken on a different machine, or `tailscale` reissued a node key after
reinstall). Each refusal emits a `medium` degraded code with the literal
reversal-then-re-enroll repair command. The daemon (SRR6.46.12) refuses to
bind the hello responder on node-key mismatch as the symmetric protection.

**Why**: Without the node-key detector, restoring `~/.local/share/ee/ee.db`
from a backup of machine A onto machine B would leave peer-group rows
referencing A's node key, producing strange "peer not found" errors and
potentially leaking workspace data if a future enrollment is attempted under
the stale identity. Tailnet-change-only detection (the v2 of this plan) was
too narrow once the backup-restored class was identified. Treating tailnet
change and node-key change as the same shape of guard (different binding
field, same refusal semantics) keeps the detector logic, audit-row schema,
and operator response identical across both failure modes.

### D6: Discovery cache (TTL) + per-peer state machine (grace period)

SRR6.46.13 (`bd-36bbk.1.13`) introduces two related but distinct concepts:

- A **TTL cache** for autodiscovery results (default 30s, configurable via
  `EE_MESH_DISCOVERY_CACHE_TTL_SECONDS`), invalidated on TTL expiry, on
  `tailnetId` change, on explicit `ee mesh status --refresh`, on
  `workspaceId` change, and on auto-enroll completion.
- A **per-peer state machine** (`active → soft_stale → hard_stale → denylisted`)
  with two-threshold escalation: `soft_stale` after 1 consecutive missed probe
  plus 5 minutes since last success; `hard_stale` after 3 consecutive misses
  plus 1 hour since last success. Only `hard_stale` peers surface in
  `drift.stalePeersInConfig`; `soft_stale` peers surface in
  `drift.transientUnreachable` without triggering removal-class drift.

**Why**: A single wall-clock TTL hammers tailnets when agent harnesses poll
`ee mesh status` between operations. A single-miss "stale" classification
causes peer churn whenever a peer's laptop sleeps for 10 minutes — the user
runs `ee mesh auto-enroll` to "fix" the drift, the peer gets removed, then
the peer wakes up and the user has to re-enroll. The two-tier grace lets
laptops close lids and tailnet ACL hiccups recover without manual
intervention, while still flagging genuinely-departed peers within an hour.
Separating cache (latency optimization) from state machine (correctness)
keeps each concern independently testable.

### D7: `ee.repair_action_graph.v1` shared schema across doctor and status

SRR6.46.16 (`bd-36bbk.1.16`) emits a structured action graph from `ee doctor
--json` with `actions[]` (each having `id`, `kind`, `command`,
`prerequisites[]`, `expectedOutcome`, `priority`, `estimatedDurationSeconds`,
`reversible`, `requiresUserConfirmation`, `executionContext`),
`topologicallyOrderedExecution[]`, and `parallelizableGroups[]`. SRR6.46.4
(`bd-36bbk.1.4`) emits a subset of the same schema (typically 1-3 actions)
from `ee mesh status` drift hints. Both surfaces validate against
`ee.repair_action_graph.v1`.

**Why**: A `repairPlan: Vec<String>` is unusable by agent harnesses — they
have to parse prose to extract commands, infer ordering, and guess what each
command will do. A structured action graph lets the consuming agent
topologically execute repairs, parallelize where the graph allows, and gate
each step on the previous step's expected outcome. Sharing the schema between
doctor (15 checks, full graph) and status (1-3 drift actions, subset) means
agent harnesses learn one parser and apply it to both surfaces.

### D8: Conservative default lane policy (deny body/embedding/graph_link)

When SRR6.46.3 materializes a fresh auto-enrollment, the default lane policy
is `metadata=allow, revision_notice=allow, curation_signal=allow,
body=deny, embedding=deny, graph_link=deny`. Widening to body/embedding/
graph_link requires explicit `ee mesh grant` (owned by SRR6.5).

**Why**: Auto-enrollment is a trust-on-first-use moment. The user has not
yet had the chance to review which peers are getting access. Defaulting to
metadata + revision_notice + curation_signal gives the user the coordination
benefit (peers can announce that they have memory updates) without the
exposure cost (peers cannot read memory bodies or pull embeddings). Widening
is an explicit, audited action when the user is ready.

### D9: Pre-grant lane visibility preview as a load-bearing safety surface

SRR6.46.17 (`bd-36bbk.1.17`) introduces `ee mesh preview-grant <peer-key>
--lane <lane> [--json]` as a read-only visibility audit. Before granting a
more-permissive lane (e.g. `body` or `embedding`), the user can see exactly
which memories would become visible to that peer, with cautions for
`high_trust_class_exposure`, `large_volume_exposure`, `sensitive_tags_in_exposure`,
and `cross_workspace_overlap`.

**Why**: The trust model is rich (5 trust classes, ~6 lanes, redaction
classes, workspace scope filters, memory tags). Without a preview surface,
a user contemplating "should I let Bob see my memory bodies?" has to read
SRR6.5 trust policy + SRR6.30 workspace scope + the redaction-class catalog
+ their own tag conventions and reason about exposure by hand —
practically impossible. The preview computes the actual exposed memory set
under the proposed policy and surfaces hazards. This is the security-
conscious user's safety net.

## Invariants

- Mesh defaults to off. With `EE_MESH_ENABLED=0`, no SRR6.46 code path
  executes, no auto-enrollment writes occur, and `tests/mesh_off_no_network.rs`
  continues to pass byte-stably (per ADR 0037 invariant).
- `ee mesh auto-enroll` is the only command that materializes auto-enrollment
  config. `ee status`, `ee mesh status`, `ee mesh hello-responder status`, and
  `ee mesh steward status` are all read-only.
- Every materialization is preceded by a forensic audit row. Audit emission
  failure is fail-closed: no peer-group write happens without a successful
  precursor row.
- Each workspace gets its own peer-group row. Disable on workspace A leaves
  workspace B's binding intact.
- The hello responder lives in `ee daemon`. When the daemon is not running,
  `ee mesh status` surfaces a `hello_responder_not_running` degraded code with
  the literal repair command.
- Tailnet change and node-key change are both refused at auto-enrollment time
  with a `medium` severity degraded code and a literal copy-paste reversal
  command.
- Discovery results are cached with a 30s default TTL; per-peer drift uses a
  two-tier `soft_stale` / `hard_stale` state machine with documented thresholds.
- `ee doctor` and `ee mesh status` both emit `ee.repair_action_graph.v1`
  payloads so an agent harness learns one schema and parses both surfaces.
- Auto-enrollment defaults are conservative: metadata + revision_notice +
  curation_signal allowed; body + embedding + graph_link denied.
- `ee mesh preview-grant` is required before any operator can grant
  body/embedding/graph_link with confidence in what they're exposing.

## Reserved Schemas

SRR6.46 reserves the following schemas (each tracked by its owning sub-bead):

| Schema | Owner | Purpose |
| --- | --- | --- |
| `ee.tailscale.local.v1` | bd-36bbk.1.1 | tailscaled probe result (existing, extended) |
| `ee.tailscale.autodiscovery.v1` | bd-36bbk.1.2 | enumerated ee-capable peers + skipped peers |
| `ee.mesh.auto_enrollment_summary.v1` | bd-36bbk.1.5 | forensic audit-row payload |
| `ee.mesh.auto_enrollment_outcome.v1` | bd-36bbk.1.5 | back-fill outcome row |
| `ee.mesh.auto_enrollment_result.v1` | bd-36bbk.1.3 | command response envelope |
| `ee.mesh.hello.v1` | bd-36bbk.1.6 | hello-handshake request |
| `ee.mesh.hello.response.v1` | bd-36bbk.1.6 | hello-handshake response |
| `ee.mesh.hello.error.v1` | bd-36bbk.1.6 | hello-handshake decline |
| `ee.mesh.discovery_policy.v1` | bd-36bbk.1.7 | service-tag / allowlist / denylist config |
| `ee.mesh.auto_status.v1` | bd-36bbk.1.4 | enriched `ee mesh status` block |
| `ee.mesh.disable_result.v1` | bd-36bbk.1.9 | rollback envelope |
| `ee.mesh.revoke_result.v1` | bd-36bbk.1.9 | per-peer revocation envelope |
| `ee.mesh.hello_responder.status.v1` | bd-36bbk.1.12 | daemon-side responder posture |
| `ee.mesh.steward.status.v1` | bd-36bbk.1.14 | periodic reconciliation posture |
| `ee.mesh.lane_grant_preview.v1` | bd-36bbk.1.17 | pre-grant visibility audit |
| `ee.repair_action_graph.v1` | bd-36bbk.1.16 | shared structured repair plan |
| `ee.doctor.action_graph.v1` | bd-36bbk.1.16 | doctor-specific action-graph wrapper |

## Threat Model Extensions (over ADR 0037)

ADR 0037 covers the SRR6 transport-level threat model. SRR6.46 inherits all
of that and adds the auto-enrollment-specific threats:

| Threat | Required control |
| --- | --- |
| PATH-hijack of `tailscale` binary | Absolute-path allowlist + `tailscale --version` output validation in SRR6.46.1 |
| Information leak via hello probes | Service-tag opt-in (default) + symmetric responder policy + decline-response omits responder metadata (SRR6.46.6 + .7) |
| Cross-workspace bleed during auto-enrollment | Hello response carries `workspaceIds[]`; SRR6.46.2 filters peers whose workspaces don't intersect |
| Cross-tailnet config bleed | SRR6.46.8 tailnet-change guard refuses |
| Backup restored to different machine | SRR6.46.8 node-key change guard refuses (symmetric with tailnet check); daemon refuses to bind responder on mismatch |
| Concurrent agent race on auto-enroll | SRR6.46.3 acquires the workspace write-owner lock before any state read |
| Audit row write failure leaving partial state | SRR6.46.3 fails closed: no peer-group write without successful audit row first |
| Silent over-enrollment via steward | SRR6.46.14 steward is opt-in via `EE_MESH_AUTO_ENROLL_ON_DEMAND=1`; defaults off; emits its own audit row per pass; daily cap |
| Pre-grant surprise exposure (e.g. body lane) | SRR6.46.17 preview-grant surfaces exposed memory count + cautions for high-trust / large-volume / sensitive-tag / cross-workspace overlap |

## Implementation Beads And Proof Obligations

SRR6.46 ships as 20 sub-beads under `bd-36bbk.1` (epic):

1. `bd-36bbk.1.10` — fake-tailscale e2e harness (deterministic CI fixture).
2. `bd-36bbk.1.1` — tailscale local probe (cross-platform; shields-up; binary authenticity).
3. `bd-36bbk.1.5` — auto-enrollment safety snapshot (audit row + outcome back-fill).
4. `bd-36bbk.1.7` — peer discovery policy (service-tag opt-in default).
5. `bd-36bbk.1.6` — hello handshake protocol (schemas + per-request handler).
6. `bd-36bbk.1.8` — tailnet + node-key change guard.
7. `bd-36bbk.1.12` — hello responder lifecycle (supervised in `ee daemon`).
8. `bd-36bbk.1.13` — discovery cache + drift grace state machine.
9. `bd-36bbk.1.2` — peer autodiscovery.
10. `bd-36bbk.1.3` — auto-enrollment composed flow (the orchestrator).
11. `bd-36bbk.1.9` — `ee mesh disable` + per-peer revocation.
12. `bd-36bbk.1.4` — `ee mesh status` enrichment + drift hints + action graph subset.
13. `bd-36bbk.1.14` — opt-in steward periodic reconciliation.
14. `bd-36bbk.1.17` — pre-grant lane visibility preview.
15. `bd-36bbk.1.16` — `ee doctor` integration + action graph emitter.
16. `bd-36bbk.1.15` — performance gate (active workloads + idle daemon + 500-peer scale).
17. `bd-36bbk.1.18` — ADR (this document) + agent onboarding + help text + migration guide.
18. `bd-36bbk.1.19` — CI integration (verify.sh wiring + MCP manifest + J6 fixtures + schema lifecycle).
19. `bd-36bbk.1.11` — opt-in real-tailscale smoke test.
20. `bd-36bbk.1.20` — CHANGELOG + version bump + release notes + advisory promotion plan.

Implementation order is enforced by the dependency graph (see `bv
--robot-plan` for the topological view). The harness (`bd-36bbk.1.10`) is
the only zero-dependency entry point; every other bead transitively depends
on it for e2e proof.

The minimum proof set for SRR6.46 (above and beyond ADR 0037's SRR6 proof
set) is:

- Mesh-disabled byte-stability: `tests/mesh_off_no_network.rs` continues to
  pass after the 17 implementation beads land.
- Network-off-with-mesh-enabled byte-stability: `ee remember/search/context/why`
  produce byte-identical output when `EE_MESH_ENABLED=1` but tailscaled is
  unreachable. The probe degrades; ordinary commands are unaffected.
- Fail-closed audit: an injected audit-write failure in SRR6.46.3 produces
  `auto_enrollment_audit_failed` (critical) and zero peer-group rows
  (`tests/mesh_auto_enrollment_safety_audit.rs`).
- Concurrent-safe auto-enroll: two parallel `ee mesh auto-enroll` invocations
  on the same workspace produce exactly one materialization and one
  `auto_enrollment_concurrent_attempt` warning (SRR6.46.3 scenario G).
- Tailnet-change refusal: cross-tailnet auto-enroll produces a refusal with
  the literal reversal command (SRR6.46.8).
- Node-key-change refusal: backup-restored-on-different-machine produces a
  refusal symmetric to the tailnet check (SRR6.46.8).
- Forensic correlation: every audit row's `summaryHash` matches the canonical
  JSON of the row's content excluding the hash field itself; rollback rows
  carry `previousSummaryHash` referencing the materialization row.
- Identity privacy: hello-decline responses omit responder version /
  workspace / capability fields (SRR6.46.6 + .7).
- Cross-platform probe: socket discovery succeeds on macOS sandboxed +
  macOS open + Linux + Windows named pipe; CLI fallback resolves to an
  allowlisted absolute path; PATH-hijack fixture produces
  `tailscale_binary_inauthentic` (SRR6.46.1).
- MCP parity: every new `ee_mesh_*` tool appears in the MCP manifest and is
  asserted in `tests/mcp_parity.rs` (`bd-36bbk.1.19`).
- Performance budgets: every workload row in `auto_enroll_perf_v0.json`
  passes within the 15% (active) / 20% (idle) regression margin
  (`bd-36bbk.1.15`).
- Real-tailscale smoke: opt-in `EE_E2E_REAL_TAILSCALE=1` smoke test passes
  against an actual tailnet before tagging a release that touches SRR6.46
  surfaces (`bd-36bbk.1.11`).

## Consequences

SRR6.46 turns mesh from an expert-only feature into a one-command surface
when the prerequisites are met. The cost is a richer surface area: 17 new
schemas, ~30 new degraded codes, 5+ new env vars, a daemon-side supervised
job, a discovery cache + state machine, and a documentation set that needs
to stay in sync.

The benefit is that ordinary multi-machine swarm operators can opt into
shared memory in one command and rollback in one command, with continuous
forensic visibility into every attempt. Agent harnesses get a structured
action graph they can topologically execute when drift surfaces.

Mesh-disabled users pay no cost: the entire SRR6.46 surface is gated behind
`EE_MESH_ENABLED=1`, with the byte-stability gate ensuring no code-path
leaks into the default path.

## Rejected Alternatives

- **Wizard UI**: Rejected because the agent harness contract cannot drive an
  interactive prompt. Doubles the test matrix (wizard + agent path) for
  every implementation bead. See D1.
- **Always-on auto-reconciliation by default**: Rejected because silently
  mutating peer-group config on a schedule violates AGENTS.md "no silent
  memory mutation" without an explicit opt-in. SRR6.46.14 steward is
  opt-in via `EE_MESH_AUTO_ENROLL_ON_DEMAND=1`; default is off. See D2.
- **Hello responder as a separate `ee mesh serve` command**: Rejected because
  splitting "is mesh on?" responsibility between `ee daemon` and a second
  process doubles the install-and-run surface and fragments supervision.
  The supervised-job-inside-daemon approach reuses existing infrastructure.
  See D4.
- **Single peer-group shared across workspaces**: Rejected because it
  couples workspace lifecycles. Disabling on workspace A would detach from
  peers still in use by workspace B; lane widening on B would silently
  expand exposure for A. The duplicate-rows cost is acceptable. See D3.
- **Body lane default-allow on auto-enrollment**: Rejected because
  auto-enrollment is a trust-on-first-use moment; defaulting to "share
  bodies with everyone on the tailnet" violates principle-of-least-exposure.
  Default-deny + explicit `ee mesh grant` preserves the coordination
  benefit without the exposure cost. See D8.
- **Wall-clock cache TTL only (no per-peer state machine)**: Rejected
  because a single missed probe should not cause peer removal. Laptops
  close lids; tailnet ACL hiccups happen. The two-tier soft_stale /
  hard_stale state machine with documented thresholds prevents churn while
  still flagging genuinely-departed peers within an hour. See D6.
- **Tailnet-change-only identity guard**: Rejected because the
  backup-restored-on-different-machine class is real and produces the
  same shape of failure. Symmetric tailnet + node-key guard with the same
  refusal semantics is the better factoring. See D5.
- **`repairPlan: Vec<String>` for status / doctor**: Rejected because
  agent harnesses cannot topologically execute a list of prose strings.
  The `ee.repair_action_graph.v1` shared schema lets agents parse one
  format and apply it to both surfaces. See D7.

## Verification

ADR 0038 is actionable when the 20 SRR6.46 sub-beads attach executable
evidence:

- `bd-36bbk.1.10` provides the deterministic fake-tailscale harness every
  other e2e imports. Without this, the other proofs cannot run in CI.
- `bd-36bbk.1.5` provides the audit-row infrastructure that every
  materialization-bearing bead depends on. Without this, the fail-closed
  invariant cannot be enforced.
- `bd-36bbk.1.3` provides the orchestrator that exercises the full chain:
  probe → discovery → policy → handshake → safety snapshot → materialize →
  sync-once → result envelope. Its e2e (14 scenarios A-N) is the integration
  test for the entire epic.
- `bd-36bbk.1.9` provides the rollback path. Without this, the "reversible"
  invariant claimed in D2 is hollow.
- `bd-36bbk.1.19` provides the CI integration that wires every e2e into
  `./scripts/verify.sh`. Without this, the proofs above do not run on
  every commit.
- `bd-36bbk.1.11` provides the opt-in real-tailscale smoke test
  (`EE_E2E_REAL_TAILSCALE=1`). Without this, the fake-tailscale-only proof
  has no path to detecting drift between the harness's tailscale model and
  real-world tailscale behavior.
- `bd-36bbk.1.20` provides the CHANGELOG, version-bump, and advisory
  promotion plan. Without this, SRR6.46 ships in an unreleased state that
  users cannot evaluate.

Passing the sub-bead proofs is stronger evidence than this document. Until
the 20 beads close (and the SRR6 primitives they depend on close), SRR6.46
remains a proposed UX layer with explicit invariants and a finished design,
not an implemented auto-enrollment surface.
