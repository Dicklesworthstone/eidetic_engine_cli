# Reality-Check Snapshot — 2026-05-22

> Static-reading audit of `origin/main` against `README.md` + `AGENTS.md`
> "Five Core Jobs", the Walking-Skeleton acceptance gate, and the
> documented `0.1 → 0.6` milestone path. No source edits; this file is the
> deliverable. Companion to `CLOSE_THE_GAP_PLAN.md`, focused on the
> three questions the orchestrator asked rather than a full plan refresh.

## TL;DR

- **Version vs surface:** `Cargo.toml = 0.2.0`. The `v0.2.0` release published
  2026-05-21 ships *graph-derived retrieval, mesh, doctor first-aid* — the
  roadmap puts those at `0.4.0 / 0.5.0`. The project compressed milestones
  rather than drifting into ad-hoc work, but the version number no longer
  describes the surface.
- **"Most important workflow":** The 5-command walking-skeleton path is
  source-present and recently hardened (UTF-8 decode `bd-2ysyd`, context-delta
  envelope `bd-1h96m`, cooperative refresh partial results `bd-qiyo3`, schema
  validator `bd-1rwxf`). Production-quality on the *static* axis. NOT
  verifiable on the *runnable* axis today because RCH is firmly broken at
  vendored `asupersync/sync/once_cell.rs:3451`; no swarm pane has been able
  to run `cargo test` against the tree in this session.
- **Biggest doc/surface gap:** `README.md` and `AGENTS.md` both centre the
  agent contract on `ee context`, but the CLI is actively deprecating that
  surface in favour of `ee pack` (`deprecated_alias` degradation ships on
  every `ee context` invocation — confirmed via the `--since` happy-path
  merge that response-side degradations now reach the delta envelope per
  `bd-270ep`). The README's "most important workflow" still says
  `ee context`; agents reading the README and then receiving a
  `deprecated_alias` degraded entry on every call get conflicting signals.

## (a) Milestone Tracking — Compression, Not Drift

| Milestone | Documented (`AGENTS.md:778-783`) | Shipped on origin/main as of 2026-05-22 |
|-----------|----------------------------------|------------------------------------------|
| `0.1.0`   | Walking skeleton: `init`, `remember`, `search`, `context`, `why`, `status` | Tagged + released 2026-05-15 |
| `0.2.0`   | CASS import MVP, indexing queue | Tagged + released 2026-05-21 — *but blurb is "graph-derived retrieval, mesh, doctor first-aid"* (= `0.4.0` + `0.5.0` features) |
| `0.3.0`   | Procedural rules and curation | `ee curate`, `ee learn`, rule promotion — already in tree, no tag |
| `0.4.0`   | Graph analytics | PageRank / Betweenness / HITS cooperative refresh, Pack DNA — already shipped under `v0.2.0` |
| `0.5.0`   | Steward + optional daemon | `ee daemon --foreground`, mesh e2e suite, steward CUSUM evidence — already shipped under `v0.2.0` |
| `0.6.0`   | Export + backup + MCP adapter | Backup/restore in tree, MCP feature-gated, release tooling (SLSA, install.sh, homebrew tap tracked by `bd-3usjw.13`) — partial |

The pattern is consistent telescoping: each new release rolls forward more
features than its roadmap line called for, with no semver gymnastics
(major bumps would be the alternative). This is *intentional release
compression*, not ad-hoc subsystem drift — `bd-3usjw` ("Bridge Plan
Part II — post-2026-05-14 reality-check follow-through", currently
`bv --robot-triage`'s `top_picks[0]`) is the meta-tracker.

But the version number is now actively misleading. Two consequences:

1. README install URL (line 24-26) still points to
   `releases/download/v0.1.0/install.sh` with the prose label
   *"Planned release installer: not published yet"* — neither claim is
   true any more. `v0.1.0` and `v0.2.0` are both published; the latest is
   `v0.2.0`. An agent following the README installs an artefact two
   milestones behind the documented "current" version.
2. Anyone reading `Cargo.toml`'s `version = "0.2.0"` and the
   `AGENTS.md` roadmap together expects "CASS import MVP". They get
   graph analytics + mesh + steward + daemon foreground. The roadmap
   table is the canonical place to fix this — either advance the
   labels, or rename the column from "Milestone" to
   "Originally-scoped milestone (compressed since)".

## (b) "Most Important Workflow" Production-Quality Assessment

Per `AGENTS.md:403-407` the canonical workflow is:

```bash
ee context "fix failing release workflow" --workspace . --max-tokens 4000 --json
```

| Walking-skeleton acceptance gate item (`AGENTS.md:497-508`) | Status from static reading |
|---|---|
| All commands work without daemon mode | Source-present; runtime unverified (RCH-E327 blocks every `cargo` invocation through `asupersync/sync/once_cell.rs:3451`) |
| All commands have stable JSON mode | `ee.response.v2` + `ee.error.v2` envelopes documented; `bd-2tf9h` realigned daemon foreground to v2 |
| Memory in FrankenSQLite through `ee-db` | Schema in place (`src/db/mod.rs` V006 `pack_records`, V007+ migrations) |
| Search from Frankensearch / documented degraded lexical path | `BM25-only fallback` degraded code defined; bloom prefilter removed (`bd-2oyx9`) so the search hot path's diagnostics no longer overclaim |
| Context pack includes provenance | `ContextDeltaItemSnapshot` carries `provenance`/`trustClass`/`trustSubclass`; `bd-270ep` ensures `response.data.degraded` reaches the delta envelope |
| `ee why` explains storage / retrieval / pack selection | Source-present (`src/cli/mod.rs::handle_why`, `tests/why_conformance.rs`) |
| Pack record is persisted | `persist_pack_record` chain in `src/core/context.rs:3185`+, exercised by `bd-1zpmh` slice-2 lookup |
| `ee status` reports DB / index / degraded capabilities | Source-present (`src/cli/mod.rs` status command + `src/core/status.rs`) |
| Cancellation tests cover ≥1 command path | `tests/contracts/asupersync_cancellation.rs`, `cooperative_refresh_honors_pre_cancelled_context` |
| No Tokio / `rusqlite` in dep tree | `tests/forbidden_deps.rs` enforces |

**Static verdict:** the surface clears the gate. **Runtime verdict:**
unverifiable from origin/main today because the vendored asupersync at
`/data/projects/asupersync/src/sync/once_cell.rs:3451` has had an
"unexpected closing delimiter" error across every RCH attempt by every
pane (panes 2, 4, 5, 6 all hit it independently). This is the *real*
v0.2.0-release health question; everything else is downstream of it.
`bd-17c65.10.17.1.2 / bd-17c65.10.17.1.4` is the remediation track.

## (c) Largest docs/agent-ux ↔ Surface Gap

The 19 `docs/agent-ux/*.md` files cover deep operational concerns —
NUMA pinning, flight-recorder, workload-replay, subscribe-onboarding,
auto-enrollment, schema-evolution, disk-pressure. The agent-UX corpus
has matured into a fleet-operations reference.

**Missing:** an `apply-doc` for the canonical workflow itself.

`docs/agent-ux/context-delta-apply.md` exists for the `--since`
transport optimisation; there is no companion
`docs/agent-ux/most-important-workflow.md` (or equivalent) explaining
how an agent should consume the `data.pack.text` block, when to
refresh, what to do with `data.degraded`, how `--explain` and
`--pack-profile` interact, and — critically — that `ee context` is
deprecated in favour of `ee pack`. `AGENTS.md:528-538` does the bare
minimum: lists the 5 commands, says "everything else is in service of
these five", and links onward, but the line at 538 references
`ee --help`'s "Most-used commands (start here)" as the authoritative
list, not a single document.

**Concrete symptom:** every `ee context` invocation now emits
`deprecated_alias` (`AGENTS.md:495`-area workflow command → CLI emits
"`ee context` is a compatibility alias for the promoted triad
command; Use `ee pack \"<task>\"`."). The README and AGENTS.md still
quote `ee context` in nine separate places (six in README, three in
AGENTS.md). An agent following the docs gets the deprecation message
on first use.

**Either** flip the README + AGENTS.md examples to `ee pack`, **or**
remove the `deprecated_alias` degradation if the team decided
`ee context` is the supported long-form. Today the binary and the
docs disagree.

## What's Filed Where

| Concern | Tracking |
|---|---|
| Release readiness (publish, signed assets) | `bd-3usjw` family — currently the top robot-triage pick, with 5+ in-flight children |
| `v0.1.0` install URL in README | Not yet filed |
| `ee context` ↔ `ee pack` deprecation messaging | Not yet filed |
| Roadmap table updates | Not yet filed |
| Vendored `asupersync` build break | `bd-17c65.10.17.1.2 / bd-17c65.10.17.1.4` |
| Recent surface hardening (this session) | `bd-2ysyd`, `bd-1h96m`, `bd-1es1m`, `bd-1zpmh`, `bd-270ep`, `bd-xm5qz`, `bd-qiyo3`, `bd-n0vkg`, `bd-367gc`, `bd-2pgex`, `bd-18bue` |

## Not Filing

Per the saturation guidance from the orchestrator, this snapshot
stays as a markdown gap-summary rather than a bead. The structural
points above benefit from the table form; converting them to beads
would lose the cross-reference clarity. The two action-shaped items
(`README v0.1.0 URL`, `ee context vs ee pack`) are small enough that
the next implementer pane can pick them up from this file directly
when convergence permits.
