# ADR 0081: Doctor / Status Core-vs-Advisory Health Aggregation

Status: accepted
Date: 2026-06-17

Bead: bd-1et0v.12 (doctor-health track keystone). Related: the agent-first UX
posture contract in AGENTS.md ("posture is a five-state enum … not a
`healthy: bool`"), ADR 0080 (bundled default embedder — supplies the new
embedding-posture check).

## Context

The 12 West analyst (2026-06-17) ran the documented liveness check
`ee --workspace . --json doctor` on a functionally-perfect install and concluded
their memory was broken, because the top-line reported `healthy: false` /
`posture: degraded_recoverable`.

The cause is that aggregation treats **all checks equally**:

- `DoctorReport::gather_with_workspace` set
  `overall_healthy = checks.iter().all(is_healthy)` and
  `Posture::from_checks` downgraded the top-line on **any** non-transient
  `Warning`, regardless of which subsystem warned.
- `ee status` (`status_posture_report`, `src/core/status.rs`) aggregated the
  top-line `overall` from **every** subsystem row, including optional/advisory
  ones.

So memory-irrelevant subsystem warnings — `graph_numa_pin` (a platform-
unimplemented optimization), `rch_worker_pressure` (a remote-build fleet signal),
`cass-limited` (an optional evidence source), `host_calibration` (a budget-tuning
hint), `daemon_socket_reachable`, `lexical_ram_tier`, `shard_fanout`, the mesh
posture — flipped the top-line. A non-technical operator running the liveness
check on a perfect store reads "broken".

A memory-recall liveness check must be GREEN when the core memory loop works. The
fix is to distinguish "core memory broken" from "an optional/advisory subsystem
is degraded but memory fully works".

## Decision

### D1 — Two-tier check model

Every health check is classified into exactly one tier:

- **CORE** — drives the top-line posture / `overall_healthy`. These answer the
  single question "can `ee` store and retrieve memory right now?":
  `runtime`, `workspace`, `database`, `search index` (`search`), `memory`, and
  the new **embedding-posture** check *only insofar as it indicates retrieval is
  genuinely unusable*. `pack` is CORE-derived (it is a function of storage +
  search) on the status surface.
- **ADVISORY** — reported with their own severities, **never** flip the top-line
  unless they actually break the memory loop: `graph_numa_pin` /
  `graph_compute`, `rch_worker_pressure`, `rch_verify_ledger`, `cass` (optional
  source), `daemon_socket_reachable`, `lexical_ram_tier`, `shard_fanout`,
  `host_calibration`, `flight_recorder`, `singleflight`, `maintenance`,
  `agent_detection`, `mesh`, and the PATH-shadow / install-path ergonomics check.

The embedding-posture check is ADVISORY/info when retrieval is healthy-neural OR
an honest hash fallback (both are usable); it only becomes a CORE degradation
when retrieval is genuinely unusable. A healthy neural default or an honest hash
fallback must never degrade the top-line.

### D2 — `ee doctor` aggregation

`CheckResult` carries a `tier: CheckTier { Core, Advisory }` field (already
present). `Posture::from_checks` skips `CheckTier::Advisory` checks, and
`overall_healthy` is `checks.iter().all(CheckResult::is_topline_healthy)` where
`is_topline_healthy()` returns `true` for any advisory check. The remaining work
of this keystone is to **tag each advisory check** (`.advisory()`) at the single
call-site policy table in `gather_with_workspace`, so the CORE/ADVISORY split is
auditable in one place and mirrors this ADR. The posture enums
(`ok | degraded_recoverable | blocked` for status; `ready | degraded |
needs_attention` for doctor) and the degraded-code vocabulary are unchanged —
only which checks COUNT toward the verdict changes.

### D3 — `ee status` aggregation (must agree with doctor)

`ee status` keeps its richer per-subsystem `WorkspacePostureReport`, but the
top-line `overall` is computed from **CORE subsystems only**
(`runtime`, `storage`, `search`, `memory`, `pack`). Advisory subsystems remain
visible as their own rows with their own statuses and are summarized in an
advisory section, but they do not degrade `overall`. This makes a working memory
store green on `ee status` for the same install where `ee doctor` is green.

Because the two surfaces use different internal models (doctor: flat
`CheckResult` list; status: per-subsystem `WorkspacePostureReport`), they are not
refactored into one function in this leaf; instead a contract test asserts they
**agree on the same workspace** (both top-line green on a clean install with
advisory-only warnings present). The CORE subsystem set is the shared invariant
both surfaces honor.

### D4 — Advisory visibility

Advisory checks/subsystems are never silently dropped. On `ee doctor` each check
serializes its `tier` so consumers can split CORE vs ADVISORY, and an
`advisories` summary (count + the advisory check names/severities) is surfaced
distinctly from the top-line verdict. The concise-default rendering and gating
the full firehose behind `--full` is the follow-up verbosity leaf (bd-1et0v.15);
this keystone guarantees the data is tier-tagged and the top-line is correct.

## Consequences

- A clean install with NUMA/RCH/CASS/host-calibration advisory warnings reports
  top-line `ok` / `ready` on both `ee doctor` and `ee status`, while the advisory
  warnings remain fully visible in their own rows/section.
- A genuine core failure (DB unopenable, index error, workspace missing, runtime
  broken) still degrades/blocks the top-line exactly as before.
- The change is to AGGREGATION, not vocabulary: posture enums and degraded codes
  are intact, so the degraded-code taxonomy and failure-mode fixtures are
  unaffected except for the aggregation fixtures added here.
- Determinism holds: tier is a static per-check/per-subsystem property; the same
  workspace yields the same posture.
- Unblocks bd-1et0v.13 (numa_pin not-applicable on Linux), bd-1et0v.14
  (cass-limited + rch_worker_pressure advisory), bd-1et0v.15 (concise default).

## Rejected Alternatives

1. **Demote the severity of the noisy checks (Warning → info).** Rejected: that
   hides real advisory signal and is per-check whack-a-mole; the structural fix is
   a tier, so any future advisory check is correct by construction.
2. **Add a new posture state for "core-ok, advisory-degraded".** Rejected: the
   AGENTS.md posture vocabulary is a fixed contract; the bead requires keeping the
   enums intact and changing only which checks count.
3. **Make `ee status` and `ee doctor` share one aggregation function now.**
   Rejected for this leaf: their internal models differ enough that a forced
   merge is risky churn; a cross-surface agreement test plus a shared CORE set
   gives the same guarantee with less blast radius. A future refactor may unify
   them.
4. **Treat every non-DB subsystem as advisory.** Rejected: `search`/`memory`/
   `pack` are part of the retrieve/pack core loop and must stay CORE so a broken
   index or unreadable memory still degrades the top-line.

## Verification Hooks

- Golden/fixture: a workspace that is top-line `ok`/`ready` with advisory
  warnings present, on both `ee doctor --json` and `ee status --json`.
- Contract test: doctor and status agree (both top-line green) on the same
  clean-with-advisories workspace.
- Determinism: `tests/determinism_unit.rs` / the J7 harness — same workspace →
  byte-identical posture.
- Unit: `Posture::from_checks` ignores advisory warnings; a CORE error still
  yields `Blocked`; status `overall` derives from CORE subsystems only.
