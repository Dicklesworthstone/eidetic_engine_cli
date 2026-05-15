# CLOSE_THE_GAP_PLAN — `ee` (Eidetic Engine CLI)

> **A comprehensive, no-scope-reduction plan to close every gap between
> what `README.md` + `COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md` promise
> and what the implemented code actually delivers.**
>
> Status: bridge plan, written 2026-05-06 from a reality-check audit.
> This document is the working artifact for ambition rounds and Phase 3a
> bead generation. It does **not** reduce scope from the comprehensive
> plan; it reasserts the full vision and assigns delivery slots.
>
> Companion to: `COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md` (the vision),
> `README.md` (the user-facing promise), `AGENTS.md` (the rules).

---

## 0. Premise & non-negotiables

The reality-check audit (2026-05-06) found three converging problems:

1. **20 distinct CLI surfaces are stubbed** with structured `*_unavailable`
   abstention responses. None implement the underlying feature.
2. **Every follow-up bead referenced by those 20 stubs is closed.** None
   shipped real implementation. The closure pattern was "add an honest
   abstention sentinel" — codified by parent bead
   `eidetic_engine_cli-jp06`.
3. **The user-facing surface promised by README does not yet exist:**
   no `install.sh`, no published crate, no benchmark harness producing
   the README's p50/p99 numbers, no `ee review session --propose`,
   no working daemon, no working procedure store, no working learn
   pipeline, no real claim/certificate verification.

The bead tracker shows 689 closed / 5 open, which is misleading. The
actual remaining work is roughly **30–40 implementation epics, each
with 4–10 child beads**, plus a process change to prevent dishonest
closures from happening again.

**Non-negotiables that govern this plan:**

- **No scope reduction.** Every promise in `README.md` and every
  section of `COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md` ships. If a
  surface is unbuildable in v1, we say so explicitly in code AND in
  docs AND we file the deferred-to-v2 bead — we do not silently shrink.
- **No new forbidden deps.** All AGENTS.md hard rules apply: no tokio,
  no rusqlite, no petgraph, no sqlx/diesel/sea-orm, no reqwest in
  core, no unsafe, no worktrees, no branches, no scripts that mutate
  source files.
- **No close-with-abstention as substitute for implementation.** A bead
  that adds an `*_unavailable` sentinel without backing the feature is
  a *milestone bead* (honest-degradation), not an *implementation bead*.
  Implementation beads close only when the feature works end-to-end
  with real persisted data, real tests, and real golden output.
- **Determinism, provenance, and audit on every new path.** Every new
  feature must pass the existing determinism gates (pack hash, JSON
  field stability, ranking ties) and emit an audit row.
- **CLI first, MCP follows, daemon optional.** No core promise depends
  on the daemon. MCP must mirror CLI schemas exactly.

---

## 1. The Process Fix (gates everything else)

This must land first. Without it, the next wave of beads closes the
same way the last wave did.

### 1.1 Implementation-vs-honesty bead taxonomy

Introduce two distinct bead labels and treat them as mutually exclusive:

- **`honesty-only`** — closes by adding/maintaining a structured
  `*_unavailable` abstention. Acceptance criterion: stable code,
  message, severity, repair, evidence-source, and follow-up bead are
  all present and tested. **Closing this bead does NOT mean the
  feature works.**
- **`implements-surface:<name>`** — closes by shipping the real
  feature. Acceptance criterion: a CLI smoke test calls the surface
  with real inputs, gets real outputs (not the abstention sentinel),
  the underlying repository/store has real persisted rows, and a
  golden output file pins the JSON shape.

**Rule:** Every surface that currently has an `_unavailable` constant
needs both labels — the existing closed bead carries `honesty-only`
(retroactively), and a new open bead carries `implements-surface:X`.

### 1.2 Closure linter

Add a CI check: any bead closed with `close_reason` containing
`"abstain"`, `"unavailable"`, `"degraded"`, `"stub"`, `"placeholder"`,
or `"removed simulation"` must carry the `honesty-only` label and must
have a sibling `implements-surface:<same-name>` bead in the open queue.

Failure mode: the closure linter blocks a PR that closes an
`implements-surface:X` bead while the matching `*_unavailable` constant
still exists in `src/cli/mod.rs`.

### 1.3 The 20-stub recovery sweep

For each of the 20 `_UNAVAILABLE_CODE` constants in `src/cli/mod.rs`:

1. Re-classify the closed follow-up bead as `honesty-only`.
2. File a new `implements-surface:<name>` bead at P0 or P1 with the
   acceptance contract from §9 of this document.
3. Add a closure-linter assertion that pins the constant name to its
   implementing bead.

This is the **first wave** of new beads (~20).

### 1.4 Vision-coverage gate

Add a Phase 1 step to `scripts/verify.sh`: scan the README command
table and the `COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md` walking-skeleton
acceptance criteria against `br list -s open --json` and the live
`*_unavailable` constants. Emit a JSON gap report. CI annotates the PR
with the current gap percentage. Goal: drive the gap percentage to
zero, not just bead-closure percentage to 100.

---

## 2. Stub-to-implementation map (the 20 surfaces)

Each subsection below lists: the stub constant, what the README/plan
promises, what real implementation requires, and which `COMPREHENSIVE_PLAN`
section governs the design. **Every one of these ships in v1. None are cut.**

### 2.1 `eval_fixtures_unavailable` → `ee eval run/list`

- **Promise:** "Run or list retrieval-quality evaluation fixtures"
  (README), `docs/evaluation.md`, plan §27.
- **Required for real:** A deterministic eval-fixture registry
  (`tests/fixtures/eval/*.json`), an in-process eval runner that
  consumes the registry, executes hybrid search, computes
  precision@k / nDCG / MRR / answer-coverage, and emits a
  versioned JSON report. Golden snapshot per fixture.
- **Acceptance:** `ee eval run --fixture release-rules --json` returns
  `{schema:"ee.eval.report.v1", fixture:..., metrics:{...}, ...}`,
  reproducible byte-for-byte.

### 2.2 `handoff_unavailable` → `ee handoff preview/create/resume`

- **Promise:** Continuity capsules between sessions/agents (plan §17,
  §23).
- **Required for real:** A handoff-capsule schema (`ee.handoff.v1`)
  containing redacted memory IDs, last-state pointers, and a resume
  hint; a writer that persists capsule rows; a reader that materialises
  the capsule into a context pack on `--resume`.
- **Acceptance:** `ee handoff create --task "..."` writes a capsule;
  `ee handoff resume <id>` produces a context pack identical (modulo
  timestamps) to one regenerated from the same inputs.

### 2.3 `learning_records_unavailable` → `ee learn agenda/uncertainty/summary/propose`

- **Promise:** Distill repeated experiences into procedural rules
  (plan §17, README pillars).
- **Required for real:** An observation ledger (`learning_observations`
  table), an agenda computation (clustering of episodic memories by
  topic+outcome), an uncertainty surface (entropy over rule
  confidence), a summary aggregator, and a candidate proposer that
  feeds `ee curate candidates`.
- **Acceptance:** After 50+ episodic memories with outcomes,
  `ee learn propose --json` returns ≥1 candidate with provenance,
  routed to the curation queue.

### 2.4 `audit_log_unavailable` → `ee audit timeline/show/diff/verify`

- **Promise:** Audit ledger of every memory mutation (README,
  plan §16).
- **Required for real:** Audit log persistence is partly there
  (`core/audit.rs`); the CLI surface needs the timeline projection,
  the per-event detail view, the diff (between two timestamps), and
  the chain-hash verifier (each row's hash includes prior row's hash).
- **Acceptance:** `ee audit verify --json` re-hashes the log and
  returns `{integrity_ok: true, rows: N, last_hash: "..."}`. Tampering
  detected.

### 2.5 `preflight_evidence_unavailable` → `ee preflight`

- **Promise:** Plan §21 — pre-action checks that warn before risky ops.
- **Required for real:** A preflight check registry, an evidence
  matcher that compares the current command + workspace against
  procedural rules and tripwires, a warning emitter that exits non-zero
  with structured JSON when matches found, and a bypass token system.
- **Acceptance:** `ee preflight "rm -rf /"` exits with code 7 and a
  JSON warning that cites the relevant procedural rule.

### 2.6 `plan_decisioning_unavailable` → `ee plan decisioning`

- **Promise:** Plan §17 procedural-memory routing (was de-scoped to
  "non-decisioning heuristics" by bead 6cks; the **vision is to
  reinstate this**).
- **Required for real:** A plan-recipe store, a decisioning matcher
  that picks a recipe given a task description and current memory
  state, and an explainability path (`ee why plan <recipe-id>`).
- **Acceptance:** `ee plan recommend "release this branch"` returns a
  ranked recipe list with provenance and component scores.

### 2.7 `procedure_store_unavailable` → `ee procedure list/show/promote/retire`

- **Promise:** Procedural memory pillar (README, plan §17).
- **Required for real:** Procedure rows persisted with maturity
  progression (provisional → validated → mature → retired), feedback
  triggers (`ee outcome` updates procedure utility/confidence), real
  verification harness, render-only export (`ee procedure show
  --format markdown`).
- **Acceptance:** Promoting a candidate via `ee curate apply` creates
  a real procedure row; `ee procedure list --json` returns it; outcome
  feedback updates its confidence; retirement on harm threshold works.

### 2.8 `recorder_store_unavailable` & `recorder_tail_unavailable` → `ee recorder *`

- **Promise:** Plan §15 / §23 — recorder captures hook-level events
  for later replay.
- **Required for real:** Recorder event schema, append-only event
  store, tail/follow streaming reader, link-back from events to
  resulting memories.
- **Acceptance:** `ee recorder tail --since 1h --json` returns real
  events; `ee recorder follow` streams new events as they're
  appended.

### 2.9 `quarantine_trust_state_unavailable` → `ee diag quarantine`

- **Promise:** Trust pipeline + prompt-injection guard (plan §22,
  README privacy section, ADR 0009).
- **Required for real:** Persisted quarantine state (rows in a
  `trust_quarantine` table), per-source rate-limit counters,
  diagnostics that report current quarantines and unblock conditions.
- **Acceptance:** Triggering 5+ harmful events from one source within
  the burst window quarantines that source; `ee diag quarantine
  --json` shows it; quarantine expires at the configured window.

### 2.10 `review_evidence_unavailable` → `ee review session --propose`

- **Promise:** README explicitly flags "not yet implemented." Plan §16
  — the post-session distillation flow.
- **Required for real:** Session evidence aggregator (groups CASS
  spans by topic), proposer that generates `Candidate` rows with
  evidence pointers, dry-run preview output, no auto-apply.
- **Acceptance:** `ee review session <cass-id> --propose --json`
  returns ≥1 candidate per non-trivial topic, each with ≥2 evidence
  spans and a provenance URI.

### 2.11 `situation_decisioning_unavailable` → `ee situation classify/compare/link`

- **Promise:** Plan §17 — situation-aware retrieval.
- **Required for real:** Situation classifier (heuristic + future
  embedding-based), comparison metric, link writer that ties
  situations to memories/rules.
- **Acceptance:** `ee situation classify --task "..." --json` returns
  a typed situation; `ee situation link <sit-id> <mem-id>` persists a
  graph edge.

### 2.12 `certificate_store_unavailable` → `ee certificate verify`

- **Promise:** Backup/restore verification chain (README backup
  section, plan §10).
- **Required for real:** Certificate row schema, manifest hashing,
  signature verification harness.
- **Acceptance:** `ee certificate verify <id>` re-hashes the manifest,
  validates signatures, returns `{valid: true/false, mismatches:[...]}`.

### 2.13 `causal_evidence_unavailable` → `ee causal trace/estimate/compare/promote-plan`

- **Promise:** Plan §14 (graph layer) + §17 — causal traces over
  failures/decisions.
- **Required for real:** Causal evidence ledger, trace assembler
  (walks links from outcome back through decisions/failures), point
  estimate of causal contribution, plan-promotion path that converts
  a verified causal chain into a procedural rule.
- **Acceptance:** Given a failure memory with linked decisions/
  evidence, `ee causal trace <mem-id> --json` returns the chain;
  `ee causal estimate` computes a contribution score; `promote-plan`
  routes the chain to curation.

### 2.14 `claim_verification_unavailable` → `ee claim list/show/verify`

- **Promise:** README plan integration with `claims.yaml`.
- **Required for real:** YAML parser for `claims.yaml`, claim store,
  hash/signature verifier per claim, integration with manifests.
- **Acceptance:** `ee claim verify --workspace . --json` walks
  `.ee/claims.yaml`, validates each claim against current state.

### 2.15 `demo_command_execution_unavailable` → `ee demo run`

- **Promise:** Plan §27 — demo manifests for reproducible scenarios.
- **Required for real:** Demo manifest parser exists and dry-run works;
  the `--no-dry-run` execution path needs an audit-ledger writer for
  every executed command and an evidence-artifact recorder.
- **Acceptance:** `ee demo run release-flow` executes the manifest
  end-to-end, every step writes an audit row, evidence artifacts land
  in `~/.local/share/ee/demos/<id>/`.

### 2.16 `daemon_jobs_unavailable` → `ee daemon` (v0.5 milestone)

- **Promise:** README v0.5 — "Steward + optional daemon".
- **Required for real:** Real maintenance handlers (decay tick,
  consolidation pass, link prediction refresh, cache pruning), real
  job rows persisted in `jobs` table, supervisor loop that respects
  Asupersync `Cx` cancellation and budgets, write-owner lock
  coordination.
- **Acceptance:** `ee daemon --foreground` runs scheduled jobs,
  persists job rows, `ee status --json` shows the running daemon and
  recent job outcomes (not stubs).

### 2.17 `maintenance_job_unavailable` → `ee job run/list/show`

- **Promise:** Companion to daemon — manual job triggering.
- **Required for real:** Job dispatcher that runs a named maintenance
  handler synchronously, shares the implementation with the daemon's
  scheduler.
- **Acceptance:** `ee job run decay-tick --json` runs the handler,
  returns a real outcome with rows-affected, persists a job row.

### 2.18 `support_bundle_unavailable` → `ee support bundle`

- **Promise:** Plan §21 — diagnostic bundle for issue reporting.
- **Required for real:** Bundler that collects (with redaction)
  `ee status`, `ee doctor`, last N audit rows, schema version, index
  manifest, recent logs; emits a tarball with a manifest.
- **Acceptance:** `ee support bundle --output bundle.tar.zst` produces
  a verifiable archive with manifest hash.

### 2.19 `tripwire_store_unavailable` → `ee tripwire *` (deeper paths)

- **Promise:** Anti-pattern surfacing pillar (plan §18).
- **Required for real:** Tripwire list/check is partly implemented;
  the deeper paths (event-payload evaluation against rule expressions,
  tripwire promotion from anti-patterns, harm-feedback integration)
  remain stubbed.
- **Acceptance:** `ee tripwire check --event '{"cmd":"git push -f"}'
  --json` returns matched tripwires with citations; harm feedback on
  a memory promotes it to a tripwire above the threshold.

### 2.20 Honesty layer maintenance

- **Promise:** None reduced. The 20 abstention messages stay in place
  *until the implementing bead closes.* Once the implementing bead
  closes, the abstention path becomes dead code and is removed in the
  same PR.

---

## 3. Release & installer infrastructure

The README's headline `curl … | bash` install command does not work
because no `install.sh` exists. Fix:

### 3.1 `install.sh`

- POSIX shell, idempotent, uses `curl`/`wget` fallback, downloads the
  latest release tarball for the host triple from GitHub Releases,
  verifies the SHA256, verifies the Sigstore bundle, places binary at
  `~/.local/bin/ee`, runs `ee doctor` to confirm.
- Mirrors patterns from `dsr` (per the user's `installer-workmanship`
  skill).

### 3.2 `install.ps1`

- PowerShell equivalent for Windows. Same verification chain.

### 3.3 GitHub Actions release workflow

- Triggered by `Cargo.toml` version bump on `main`.
- Builds for: `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`,
  `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`,
  `x86_64-pc-windows-msvc`.
- Outputs per target: `ee-<target>.tar.xz`, `.sha256`, `.sigstore.json`.
- Tags + pushes a GitHub release. `install.sh` pinned to the same tag.
- Use RCH for build offload where possible.

### 3.4 `cargo install ee` correctness

- `Cargo.toml` `[package]` metadata complete (description, license,
  repository, readme, keywords, categories) so `cargo install ee`
  works once published.
- Don't publish until the walking skeleton + first wave of `implements-surface`
  beads close. Set `publish = false` initially and only flip when the
  release-readiness gate passes.

### 3.5 Homebrew tap

- Defer to v0.6 minimum, but file the bead now so it's tracked.
  README claim stays.

---

## 4. Performance benchmark harness

README publishes a perf table (p50/p99 across 7 operations) on a
"2024 MacBook Pro M3" with no harness producing the numbers. Fix:

### 4.1 `benches/` directory with criterion (or hand-rolled, since
criterion may pull non-Asupersync time deps — verify first).

### 4.2 Bench targets matching every README row:

- `bench_remember_single`
- `bench_search_hybrid`
- `bench_context_pack_4k`
- `bench_why`
- `bench_import_cass_50_cold`
- `bench_graph_centrality_full`
- `bench_index_rebuild_full`

### 4.3 Bench fixtures

- 25-project, 14k-memory, 8k-CASS-session corpus stored in
  `tests/fixtures/perf/` (deterministically generated, hashed).
- `cargo bench` regenerates the corpus from the seed if missing.

### 4.4 CI bench gate

- `scripts/verify.sh` adds a `bench` stage (gated behind a flag for
  PRs, mandatory for `main`).
- Per-target threshold pulled from `benches/budgets.toml`.
- Regression > 20% on p50 OR > 50% on p99 fails the run.
- Results posted as JSON artifact (`ee-perf.v1`).

### 4.5 README sync

- README perf table cites the `ee-perf.v1` JSON artifact name and
  date; numbers updated automatically by a release script.

---

## 5. MCP adapter parity

README claims schemas match exactly. Verify or fix:

### 5.1 Parity test

- For every CLI command with `--json`, assert the MCP tool with the
  same name returns identical JSON for identical inputs.
- Fixture-driven; runs in CI behind the `mcp` feature.

### 5.2 Tool enumeration

- `ee mcp manifest --json` must list every CLI command in scope and
  flag any divergence.

### 5.3 Schemas published

- `ee schema list` and `ee schema export` produce machine-readable
  JSON Schema documents matching every `ee.*.v1` payload.

---

## 6. Plan-doc completeness sweep

Walk every section of `COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md` and
classify:

- **Implemented (verified)** — golden test passes.
- **Implemented (unverified)** — code present, no test.
- **Stubbed** — abstention sentinel.
- **Missing** — not in code.

For every "unverified" section, file a test bead. For every "missing"
section, file an implementation bead. Cross-check the appendices
(SQL schema, JSON contracts, end-to-end agent flow) against the live
schema introspection (`ee schema export`).

This is the **second wave** of new beads (~30).

---

## 7. E2E integration test backlog

Keep the existing 5 open beads (they're necessary). Add:

- **CASS import with redaction**: end-to-end import into a workspace
  with secrets in fixtures; verify redaction worked.
- **Multi-agent write contention**: spawn N processes writing
  concurrently; verify job-lock serialises and no rows lost.
- **Backup → restore round-trip**: backup a workspace, restore to
  side-path, diff DB rows + audit log; equal modulo IDs.
- **Migration boundary**: simulate v0.1 DB, run `ee init`, verify
  migration to current schema preserves all rows.
- **Daemon supervised job**: start daemon, trigger decay-tick, kill,
  restart, verify job state recovers.
- **MCP parity smoke**: `ee mcp` adapter returns identical JSON for
  the walking-skeleton commands.

---

## 8. Sequencing & milestones

The user has discipline against scope reduction. The sequencing below
preserves all surfaces while deferring some until earlier ones land,
so the swarm doesn't fragment effort.

### Wave 0 — Process Fix (BLOCKING, 1–2 days)

- Bead taxonomy (`honesty-only` vs `implements-surface:X`) (§1.1)
- Closure linter (§1.2)
- Re-classify the 14 dishonest closures (§1.3)
- Vision-coverage gate in `verify.sh` (§1.4)

### Wave 1 — Walking-skeleton hardening (1–2 weeks)

- All 5 existing E2E beads close
- Real audit log surface (§2.4)
- Real procedure store + curation round-trip (§2.7)
- Real review→propose pipeline (§2.10)

### Wave 2 — Pillar features (2–3 weeks)

- Real learn pipeline (§2.3)
- Real eval harness (§2.1)
- Real handoff capsules (§2.2)
- Real causal traces (§2.13)
- Real claims + certificates (§2.12, §2.14)

### Wave 3 — Operational layer (2–3 weeks)

- Real daemon + maintenance jobs (§2.16, §2.17)
- Real preflight (§2.5)
- Real quarantine diagnostics (§2.9)
- Real situation decisioning (§2.6, §2.11)
- Real recorder (§2.8)
- Real tripwire deeper paths (§2.19)
- Real demo execution (§2.15)
- Real support bundle (§2.18)

### Wave 4 — Release readiness (1–2 weeks)

- `install.sh` + `install.ps1` (§3)
- GH Actions release workflow (§3)
- MCP parity (§5)
- Performance harness + README sync (§4)
- v0.5.0 release tagged

### Wave 5 — Plan-doc completeness sweep (ongoing)

- Per-section bead generation (§6)
- E2E backlog expansion (§7)
- Homebrew tap (deferred from §3.5)

---

## 9. Per-surface acceptance contracts (template)

Every `implements-surface:X` bead carries this contract verbatim
in its `acceptance_criteria` field:

```
SURFACE: <name>
STUB CONSTANT: <name>_UNAVAILABLE_CODE in src/cli/mod.rs

REAL-DATA REQUIREMENT
- The CLI surface returns a non-abstention payload when given inputs
  from the test fixture corpus.
- The underlying repository/store has a row count > 0 after the
  command runs.
- No `*_UNAVAILABLE_CODE` constant remains for this surface in
  src/cli/mod.rs (the abstention path is deleted in the same PR).

DETERMINISM REQUIREMENT
- Same fixture + same seed → byte-identical JSON output.
- Golden snapshot pinned in `tests/golden/<surface>.snap`.
- Field ordering stable; ranking ties resolve by ID.

PROVENANCE REQUIREMENT
- Every returned record carries a provenance URI.
- Every promotion/mutation writes an audit row.

DEGRADATION REQUIREMENT
- When the dependency that made the abstention necessary is missing,
  the surface degrades to a documented lower-tier behavior (e.g.,
  semantic→lexical) and reports it in `degraded[]`.
- The degradation has its own stable code, severity, and repair.

TEST REQUIREMENT
- Unit tests cover happy path, empty store, single-row store, large
  store, malformed input, dependency missing.
- Integration test in `tests/e2e_<surface>.rs` runs the surface end
  to end against a real DB and real fixtures.
- Determinism test re-runs the surface twice and asserts byte equality.

DOCS REQUIREMENT
- Command reference in README.md updated (no "not yet implemented").
- ADR added if a design decision was made (e.g., schema, ranking).
- COMPREHENSIVE_PLAN section updated to mark this surface delivered.

CLOSE REASON FORMAT
The close_reason field MUST contain:
- "Implements <surface>" (literal prefix)
- The fixture file path
- The golden snapshot path
- The line number where the `*_UNAVAILABLE_CODE` constant USED to be
  (now deleted)

CLOSURE LINTER WILL REJECT a close_reason that contains "abstain",
"unavailable", "degraded", "stub", "placeholder", or
"removed simulation" for an `implements-surface:X` bead.
```

---

## 10. Risk register

| Risk | Mitigation |
|---|---|
| Wave 0 process fix slips → next wave closes dishonestly | Wave 0 is BLOCKING; no Wave 1 beads created until Wave 0 closes |
| Daemon work pulls in tokio via transitive deps | Forbidden-deps audit already gates this; if hit, file a quarantine bead |
| MCP parity reveals deeper schema drift | Schema drift is its own bead family; do not block walking-skeleton hardening on it |
| Bench harness adds non-Asupersync time deps | Audit `criterion` first; if unsafe, hand-roll the harness using Asupersync `Cx` clock |
| Eval fixture corpus inflates repo size | Store seeds + generators, not generated artifacts |
| Plan-doc sweep produces 100+ beads | Wave 5 is ongoing background work; never blocks releases |
| Release infrastructure depends on RCH availability | RCH "fail open" semantic; CI builds locally if RCH down |
| Closure-linter false positives on legitimate honesty-only beads | Linter checks the LABEL, not the close_reason alone; honesty-only label exempts |

---

## 11. What this plan explicitly does NOT do

- Reduce ambition or feature scope. Every promise stands.
- Delete the existing 14 closed follow-up beads. They're re-classified
  as `honesty-only`, kept as historical record.
- Touch the 5 existing open E2E beads. They proceed in Wave 1.
- Add new third-party dependencies beyond what AGENTS.md permits.
- Authorize any worktree, branch, rebase, or stash operation.
- Authorize any file deletion. The `*_unavailable` constants are
  removed only as part of an implementing PR, by the author of that PR,
  with the user's standing instruction to clean up dead code in-place.

---

## 12. Definition of done for this plan

This plan is "done" (not "implemented" — implementation is what the
beads do) when:

1. Wave 0 process fix is merged.
2. The bead tracker has one `implements-surface:X` bead per `_UNAVAILABLE_CODE`
   constant in `src/cli/mod.rs`, plus the release/bench/MCP/E2E beads
   from §3–§7.
3. `bv --robot-triage` shows the new beads in the right priority order
   with sensible dependencies.
4. The closure linter is wired into `scripts/verify.sh`.
5. The vision-coverage gate emits a baseline gap percentage.

After that, the plan goes into the COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS
appendix as a delivery roadmap and the swarm picks up `br ready`.

---

# PART II — Post-2026-05-14 Reality-Check Bridge Plan

> Written 2026-05-14 after a fresh reality-check audit (Phase 1 of the
> `reality-check-for-project` skill). The Wave 0 process fix landed.
> Waves 1–3 of Part I largely shipped (95 closed `implements-surface:*`
> beads, 19 closed `honesty-only` beads, closure-linter and
> vision-coverage gates wired into CI). Waves 4–5 never started.
> This Part II adds: forward-looking ambition (GraphAccretion +
> Agent-UX + alien-artifact), the never-executed Wave 4 (release
> readiness) and Wave 5 (plan-doc completeness sweep), plus
> housekeeping gaps Part I did not anticipate.
>
> **Non-negotiables (same as Part I):** No scope reduction. No new
> forbidden deps. No close-with-abstention as substitute for
> implementation. Determinism, provenance, and audit on every new
> path. CLI first, MCP follows, daemon optional. **No new plan files;
> revise this document in-place.**

---

## 13. Phase-1 Findings That Govern This Bridge

The fresh audit found:

1. **Vision-coverage gap = 7.58%.** Five documented surfaces are missing entirely with **zero `implements-surface` beads** covering them: `db`, `graph centrality`, `mcp`, `serve`, `swarm`. (Source: `.vision-coverage-report.json` 2026-05-14T19:21:51Z.) These are the worst gap category — `NO_BEAD`.
2. **Two stub sentinels remain in user-facing surfaces:** `LAB_REPLAY_UNAVAILABLE_CODE` (`src/core/lab.rs:36`) and `SITUATION_DECISIONING_UNAVAILABLE_CODE` (`src/core/situation.rs:20`). The six `swarm_brief` `_UNAVAILABLE_CODE` constants are *legitimate* degradation codes for missing external coordination tools — they stay.
3. **Three install paths the README advertises all fail today.** No release tag → `install.sh` URL 404s. `cargo install ee` blocked by (a) `publish = false` in `Cargo.toml:15`, (b) `ee` crate on crates.io owned by `ewpratten`, (c) 26 path-dependencies on `/data/projects/*` that aren't published. Homebrew tap explicitly planned.
4. **README's "trauma-guard surfaces high-severity risk memories before destructive actions" claim (L52) has no implementing surface.** Preflight exists but isn't wired as a destructive-action hook.
5. **Performance table numbers are aspirational static text.** Bench harness exists; nothing syncs the README table from `ee-perf.v1.json` artifacts. Claimed-vs-measured drift can grow undetected.
6. **Plan-doc policy violation.** `COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md` carries the `__OPUS` suffix AGENTS.md L237–242 forbids — but `scripts/vision-coverage.sh:20` reads `__OPUS` as the canonical plan. The suffix-less `COMPREHENSIVE_PLAN_TO_MAKE_EE.md` (12,938 LOC) is an unreferenced orphan. Also: `GEMINI_PANE*.md × 3` at repo root are unreferenced stale agent-session reports.
7. **GraphAccretion (`bd-bife`) is a 10-feature epic with 0 children started.** All G1-G8 + F1-F4 child beads are open `implements-surface:*` with zero in-progress and zero commits referencing them.
8. **`bd-17c65.2` (P0 — "Search honesty: relevance floor, lexical fusion, dedupe, no silent zero-score returns")** is the highest-priority unfinished bead in the project with zero work logged.
9. **Wave-5 plan-doc completeness sweep (Part I §6) never started.** No bead exists to walk every section of `COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md` and classify each as Implemented/Stubbed/Missing.
10. **Daemon-as-service is partial.** README L154 honestly admits scheduled steward jobs report degraded until real handlers land; the `daemon_jobs_unavailable` constant was deleted (per Part I §2.16), but handler-level degraded reporting is still load-bearing — a finer-grained set of `*_handler_degraded` audit beads is owed.

---

## 14. The NO_BEAD Gaps (highest priority — Wave 4A)

Each of the following must get a new `implements-surface:X` bead at **P1**, with the §9 acceptance contract, **before** anything else in Wave 4 ships:

### 14.1 `implements-surface:db_inspect`
- **Surface:** `ee db status/inspect/reindex/check-integrity --json`
- **What it does:** Read-only inspector exposing schema version, table row counts, FrankenSQLite WAL state, advisory lock holders, generation counters, free-page count, page-cache hit rate.
- **Why:** Audited as missing in vision-coverage; agents need a sanctioned way to introspect the DB without bypassing `ee` (which violates "search indexes are derived assets" if they poke `~/.local/share/ee/ee.db` directly).
- **Acceptance:** `ee db status --json` returns `{schema:"ee.db.status.v1", ...}`; integrity-check fires a chain-hash verify equivalent to `ee audit verify` but for all chained tables.

### 14.2 `implements-surface:graph_centrality_read`
- **Surface:** `ee graph centrality --json` (read-only listing distinct from existing `graph centrality-refresh`)
- **What it does:** Returns persisted PageRank / betweenness / authority scores from the last refresh, with provenance (`snapshot_hash`, `computed_at`, `algorithm_version`).
- **Why:** Existing command is write-only refresh; read path is unaccountably missing.
- **Acceptance:** Reads `graph_algorithm_results` table; honors `--algorithm <name>` filter; degrades cleanly when snapshot stale (`degraded[].code = "graph_snapshot_stale"`).

### 14.3 `implements-surface:mcp_top_level`
- **Surface:** `ee mcp manifest/validate/parity-test --json` exposed even when feature is disabled (returns honest "feature disabled" with capabilities, not a hidden command).
- **What it does:** Always-visible top-level group; with `--features mcp`, all subcommands work; without, each subcommand returns `{success:false, error:{code:"mcp_feature_disabled", repair:"cargo install ee --features mcp"}}`.
- **Why:** Vision-coverage flags `mcp` as missing because `#[cfg(feature="mcp")]` hides it from the no-feature build. README L840 promises the surface exists.
- **Acceptance:** `ee mcp --help` is non-empty in default build; `ee mcp manifest --json` schemas pinned in `docs/schemas/ee.mcp.manifest.v1.json`.

### 14.4 `implements-surface:serve_localhost`
- **Surface:** `ee serve [--port 7070] [--bind 127.0.0.1] --foreground`
- **What it does:** Localhost-only HTTP/SSE adapter for browser/IDE inspection; mirrors CLI JSON contracts; never accepts non-loopback connections.
- **Why:** Vision-coverage flags missing; `src/serve.rs` (602 LOC) exists but is wired only for shadow tests; feature flag `serve = []` has no implementation.
- **Constraints:** No `hyper`/`axum`/`tower` (AGENTS.md). Must use an in-tree minimal HTTP adapter (or postpone implementation behind a feature flag with honest `serve_feature_disabled` degraded code).
- **Acceptance:** `ee serve --foreground --port 0 --json-startup` emits `{schema:"ee.serve.startup.v1", port: <actual>, capabilities:[...]}` on stdout, then accepts GETs on `/v1/context`, `/v1/search`, etc., proxying to CLI handlers.
- **Defer-to-v2 escape hatch:** If forbidden-dep-clean HTTP is unachievable in v1, the surface emits a structured `serve_unavailable` degraded code with severity=`low` and a v2 follow-up bead — but the surface must be reachable, not hidden.

### 14.5 `implements-surface:swarm_subcommand`
- **Surface:** Top-level `ee swarm brief/preflight/recommend` group (existing `ee swarm brief` should be enumerable via `ee swarm --help`)
- **What it does:** Already real — `src/core/swarm_brief.rs` is solid. Gap is that vision-coverage doesn't see `swarm` as a documented surface because it scans for `ee swarm` patterns in the plan doc but doesn't find a `## swarm` heading there.
- **Acceptance (mostly documentation):** Add `COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md` section "20.21 swarm" describing the swarm-brief surface formally; update `scripts/vision-coverage.sh` if the scanner needs the entry.

---

## 15. README "trauma-guard" Wiring (Wave 4B)

The "trauma-guard surfaces high-severity risk memories before destructive actions" promise on README L52 currently has no implementing wire.

### 15.1 `implements-surface:trauma_guard_preflight`
- **Surface:** `ee preflight check --cmd "<shell-command>" --json` (existing preflight extended)
- **What it does:** Given a shell command string, tokenize it (using existing `shell-tokenize` helper), match against the destructive-action ruleset (`rm -rf`, `git push --force`, `git reset --hard`, `kubectl delete`, `DROP TABLE`, `terraform destroy`), and if matched, query memories with `kind:risk` or `kind:anti-pattern` and high `severity` (high/critical), returning them sorted by relevance + freshness. Returns exit code 7 if matches above threshold.
- **Why:** The README claim is dead without this. Agents like dcg + slb already exist but consult their own rule files, not `ee`'s memory store. The bridge is exactly the missing wire.
- **Acceptance:** Given a memory `{kind:"anti-pattern", content:"git reset --hard discards uncommitted work; see incident 2025-10", severity:"high"}` exists, `ee preflight check --cmd "git reset --hard HEAD~3" --json` returns it with score, exit code 7.

### 15.2 `implements-surface:trauma_guard_hook_helper`
- **Surface:** `ee hook preflight-shell --shell <bash|zsh> --json` — outputs a hook snippet for the user's shell that wraps risky commands.
- **What it does:** Generates a `preexec`/`DEBUG`-trap snippet that calls `ee preflight check --cmd "$BASH_COMMAND" --json` and prompts the user if exit code 7 + severity high.
- **Acceptance:** Snippet works in both bash and zsh sandboxes; tests in `tests/preflight_hook_*.rs`.

---

## 16. Release Readiness (Wave 4C — the never-started Part I Wave 4)

### 16.1 `implements-surface:perf_table_sync`
- **Surface:** `scripts/sync_perf_table.sh --input <ee-perf.v1.json> --readme README.md`
- **What it does:** Reads a release-time bench artifact (`ee-perf.v1.json`), updates the README "Performance" table in-place between `<!-- perf:begin -->` / `<!-- perf:end -->` markers, emits a diff.
- **Why:** CLOSE_THE_GAP §4.5; README L920 numbers are static. Without this, claimed-vs-measured drift grows silently.
- **Acceptance:** CI release workflow invokes this after benches; if README is dirty post-sync, fails the release.

### 16.2 `implements-surface:first_signed_release`
- **Surface:** First `v0.1.0` tag pushed; GitHub Release with all assets (`ee-{target}.tar.xz`, `.sha256`, `.sigstore.json`, `install.sh`, `install.ps1`); `install.sh` URL in README starts returning 200.
- **Why:** README's headline `curl … | sh` install method is the dominant onboarding path; today it 404s.
- **Acceptance:** `curl -fsSL https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/download/v0.1.0/install.sh | EE_VERSION=v0.1.0 sh` installs `ee` to `~/.local/bin/ee` and `ee doctor` reports green.

### 16.3 `implements-surface:crate_name_resolution`
- **Surface:** Either (a) `ee` on crates.io transferred to project account, or (b) crate renamed to `ee-cli` / `eidetic-engine` in `Cargo.toml [package].name` and all docs.
- **Why:** PUBLISH_CHECKLIST §3 blocker. crates.io currently has `ee` at `0.0.0` owned by `ewpratten`.
- **Decision required:** Owner ask + email to ewpratten OR pick a new name. Lifting bd-2gill.1 from P3 to P1.
- **Acceptance:** `cargo publish --dry-run --allow-dirty` succeeds; `scripts/audit_install_pipeline.sh` returns `crates_io.repository = Dicklesworthstone/eidetic_engine_cli`.

### 16.4 `implements-surface:franken_dep_publishing`
- **Surface:** All path-dependencies in `Cargo.toml` either (a) published to crates.io with matching versions, or (b) feature-gated and documented as opt-in.
- **The 26 deps:** `asupersync`, `franken-agent-detection`, `frankensearch`, `frankensearch-core`, `frankensearch-embed`, `frankensearch-index`, `fnx-algorithms`, `fnx-classes`, `fnx-runtime`, `fnx-cgse`, `fnx-convert`, `fsqlite`, `fsqlite-core`, `fsqlite-types`, `fsqlite-error`, `fsqlite-func`, `fsqlite-ext-fts5`, `fsqlite-ext-json`, `fsqlite-ast`, `fsqlite-btree`, `fsqlite-pager`, `fsqlite-parser`, `fsqlite-planner`, `fsqlite-vdbe`, `fsqlite-vfs`, `fsqlite-wal`, `fsqlite-mvcc`, `fsqlite-observability`, `sqlmodel-core`, `sqlmodel-frankensqlite`, `toon` (as `tru`).
- **Acceptance:** `cargo publish --dry-run` in eidetic_engine_cli succeeds because all deps are crates.io-resolvable.

### 16.5 `implements-surface:publish_flip`
- **Surface:** `Cargo.toml:15` `publish = false` → `publish = true`. Vision-coverage gate must be at 0% gap; closure-linter green; all dep publishing done; first signed release passed.
- **Acceptance:** `cargo publish --dry-run` clean; PUBLISH_CHECKLIST §1 boxes checkable.

### 16.6 `implements-surface:homebrew_tap`
- **Surface:** `Dicklesworthstone/homebrew-tap/Formula/ee.rb` published; README L268 works.
- **Acceptance:** `brew install Dicklesworthstone/tap/ee` succeeds; `ee --version` matches latest release. Defer to v0.6 per Part I §3.5 *unless* user demand suggests sooner.

---

## 17. Plan-Doc Completeness Sweep (Wave 4D — the never-started Part I Wave 5)

### 17.1 `implements-surface:plan_doc_sweep`
- **Surface:** Mechanical pass over `COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md` (3,658 lines, ~28 major sections) cross-referenced against `vision-coverage.sh` output. Every section classified Implemented / Implemented-unverified / Stubbed / Missing.
- **Output:** `docs/plan-sweep-report.md` (already exists as empty stub — fill it in) with one row per section, citation back to source file, and either ✅ or a follow-up bead ID.
- **Acceptance:** Every section has a verification hook (test file path) or a tracking bead. CI gate `tests/plan_doc_completeness.rs` walks the report and asserts no `Stubbed` row lacks a tracking bead.

### 17.2 `implements-surface:plan_doc_rename`
- **Surface:** Rename `COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md` → `COMPREHENSIVE_PLAN.md` (AGENTS.md L237 forbids `__OPUS` suffix). Delete or merge the orphan `COMPREHENSIVE_PLAN_TO_MAKE_EE.md` (12,938 LOC) into either the renamed plan, or `docs/archive/`.
- **Why:** Policy violation flagged in Phase 1 finding #6.
- **Acceptance:** Only one comprehensive plan file in the repo. `scripts/vision-coverage.sh:20` reads the renamed file. No `__OPUS` suffix anywhere.

### 17.3 `implements-surface:gemini_pane_archive`
- **Surface:** Move `GEMINI_PANE_REPORT.md`, `GEMINI_PANE10_REPORT.md`, `GEMINI_PANE14_REPORT.md` to `docs/archive/agent_reviews/` or delete (with explicit user permission per AGENTS.md Rule 1).
- **Why:** Stale unreferenced agent-session reports at repo root.
- **Acceptance:** Repo root has no `GEMINI_PANE*` files.

### 17.4 `implements-surface:close_the_gap_archive`
- **Surface:** When Part II of CLOSE_THE_GAP_PLAN ships, move this file to `docs/archive/close_the_gap_2026-05.md` and link it from the renamed comprehensive plan.
- **Acceptance:** Repo root has no `CLOSE_THE_GAP_PLAN.md`; the comprehensive plan has a "Historical Bridges" section linking to it.

---

## 18. GraphAccretion (Wave 4E — already-tracked but un-started)

The G1-G8 + F1-F4 epic (`bd-bife.*`) is already well-decomposed into open `implements-surface:*` beads. **This bridge plan does not re-create them.** It declares them in-scope for the next swarm cycle and elevates the rollout/cancellation/migration sub-epics to gating dependencies for any single G-feature ship.

### 18.1 Sequencing constraint
- **F1 (multi-graph snapshot framework, `bd-rnfh`) ships first** — all G1-G8 features depend on typed subgraphs.
- **F2 (algorithm wrapper conventions w/ CGSE witnesses + budget, `bd-igvt`) ships second** — graph algorithms must respect Asupersync `Cx` cancellation; F2 is the convention.
- **F4 (determinism harness for new graph surfaces, `bd-8jvg`) ships in parallel with F2** — determinism gate must be green before any G-feature golden snapshot is pinned.
- **F3 (`ee insights` JSON command, `bd-t6wd`) ships third** — composes G-features' output.
- **G1 (PPR, `bd-ov09`) and G2 (Pack DNA, `bd-fdvt`) ship before G3-G8** — these are query-time hot path; G3-G8 are richer analyses that depend on F1+F2 plumbing.

### 18.2 Cancellation, migration, backup, eval, schema-governance, backcompat
- `bd-bife.4` (cancellation), `bd-bife.12` (migration backfill + rollback), `bd-bife.21` (backup preserves graph state), `bd-bife.11` (PPR eval scenarios), `bd-bife.1` (schema-governance), `bd-bife.2` (backcompat additive-only), `bd-bife.3` (proptests) — all must close as gating dependencies before any G-feature can declare "real."

---

## 19. Agent-UX Overhaul (Wave 4F — bd-17c65 family)

Already-tracked but un-started family. Sequencing:

### 19.1 P0 first
- **bd-17c65.2** (Search honesty: relevance floor, lexical fusion, dedupe, no silent zero-score) ships first. This is the only P0 open bead and it's at the root of multiple downstream UX promises.

### 19.2 P1 batch
- **bd-17c65.1** (Pack format densification)
- **bd-17c65.3** (Policy detector overhaul — value-shape secret detector, relaxed tag validator)
- **bd-17c65.4** (Schema consistency — canonical content field, JSON/Markdown parity)
- **bd-17c65.5** (Diagnostics honesty — three-state posture, conditional banner emission)
- **bd-17c65.7** (Learn/Curate stubs to real or honest-unimplemented)
- **bd-17c65.10** (Shared test infrastructure — logging, fixtures, e2e drivers, regression benches)

### 19.3 P2/P3 batch (after P1)
- **bd-17c65.11/12/13** (docs updates, cross-cutting integrity, handoff overhaul) all P2

### 19.4 Alien-artifact (N1-N15) batch — Wave 5
Pulled into its own wave below.

---

## 20. Alien-Artifact Ambition (Wave 5 — bd-17c65.14.x family)

These N1-N15 children are explicitly ambition-tier. They depend on Waves 4A-4F closing first. They should NOT block release.

Sequencing within Wave 5:
- **N4 (typed determinism `Deterministic<Seed>`)** first — establishes the substrate other Ns depend on. Children N4.3/4.4/4.5 already decomposed.
- **N7 (Beta-Bernoulli posterior on memories)** — needed by N11 (active learning) and N12 (anti-patterns from failed outcomes).
- **N12 (anti-patterns auto-proposal)** — operationalizes the trauma-guard.
- **N15 (counterfactual lab + immutable revisions)** — retires `lab_replay_unavailable` and `revision_write_unavailable` (closes a remaining stub sentinel from Phase 1 finding #2).
- **N3 (structural causal credit, do-calculus)** — retires `causal_evidence_unavailable` deeper paths.
- **N2 (conformal calibration)** — adds 95% intervals to relevance scores.
- **N6 (persistent homology)** — clustering accretion.
- **N8 (cross-encoder reranker)**, **N9 (mmap'd index)**, **N10 (SIMD BM25)** — perf-tier; share a benchmark sub-epic.
- **N1 (pack-as-tensor)**, **N5 (open slot)**, **N13 (CUSUM)**, **N14 (mutual-information dedup)** — independent; ship in any order.

---

## 21. New Process Gates (carry-forward + new)

### 21.1 Reaffirm existing gates
- `scripts/check-forbidden-deps.sh` — keep.
- `scripts/closure-lint.sh --audit --json` — keep.
- `scripts/vision-coverage.sh --json` — keep; **flip from warn to fail at gap > 0** for release-tag commits (already does this) and add a Wave-4-complete check.
- `tests/contracts/schema_drift.rs` — keep.
- `tests/contracts/failure_mode_fixtures.rs` — keep.
- `tests/feature_flag_registry_in_sync.rs` — keep.

### 21.2 New gates this bridge introduces
- `tests/plan_doc_completeness.rs` (per §17.1) — every section in the comprehensive plan has a tracking bead or implementation evidence.
- `tests/readme_perf_sync.rs` (per §16.1) — README perf table matches the latest `ee-perf.v1.json` baseline.
- `tests/install_path_smoke.rs` — for each install path the README advertises, assert it's either (a) reachable now or (b) the README sentence ends with "(planned; see bead bd-…)".
- `tests/trauma_guard_wired.rs` (per §15.1) — `ee preflight check --cmd "rm -rf /"` exits 7 if a high-severity risk memory matches; else fails with a clear "no risk memory matched" — never silent.

---

## 22. Updated Sequencing & Milestones

### Wave 4 (post-2026-05-14) — Release Readiness + NO_BEAD gaps
Ships across two parallel tracks; ETA ~2 weeks if swarm capacity is healthy.

**Track A — Block-release fixes (1 week):**
- 14.1 `db_inspect`
- 14.2 `graph_centrality_read`
- 14.3 `mcp_top_level`
- 14.4 `serve_localhost` (or honest defer-to-v2 with reachable degraded code)
- 14.5 `swarm_subcommand` (doc + scanner fix)
- 15.1 `trauma_guard_preflight`
- 15.2 `trauma_guard_hook_helper`
- 21.2 four new gates
- Close `bd-17c65.2` (P0 search honesty)
- 17.2 plan_doc_rename + 17.3 gemini_pane_archive

**Track B — Release infrastructure (1–2 weeks, can run parallel):**
- 16.3 `crate_name_resolution` (decision required from user)
- 16.4 `franken_dep_publishing` (cross-repo coordination)
- 16.1 `perf_table_sync`
- 17.1 `plan_doc_sweep`
- 16.2 `first_signed_release`
- 16.5 `publish_flip`
- 17.4 `close_the_gap_archive`

### Wave 5 — GraphAccretion + Agent-UX P1 batch (3–4 weeks)
- bd-bife.* (F1, F2, F4, F3, G1, G2 in order; G3-G8 last)
- bd-17c65.1/3/4/5/7/10
- bd-17c65.11/12/13

### Wave 6 — Alien-Artifact (open-ended; ship per swarm capacity)
- bd-17c65.14.* per §20 sequencing.

### Wave 7 — Homebrew tap + v0.5.0 / v0.6.0 milestones
- 16.6 `homebrew_tap`

---

## 23. Updated Risk Register

| Risk | Mitigation |
|---|---|
| Wave 4 stalls because crate-name resolution requires external party (ewpratten) | Pursue both rename and transfer in parallel; rename path doesn't require external coordination |
| Forbidden-dep HTTP for `ee serve` blocks the surface | §14.4 honest defer-to-v2 escape hatch; surface stays reachable with structured degraded code |
| Perf-table-sync wedges README on a minor regression | Gate is advisory unless variance > tolerance in `perf_v0_2.json` |
| Renaming `__OPUS` breaks vision-coverage.sh path resolution | Same-PR update to `scripts/vision-coverage.sh:20` |
| GraphAccretion (8 features × 4 infra) too large for one wave | F1+F2+F4 gating dependencies prevent any G shipping unverified; G3-G8 shipped per swarm capacity |
| Alien-artifact features (N1-N15) leak into release-blocking | Explicit Wave 6 designation; never block release |
| Closure-linter false-positive on Wave 4 honesty work | `honesty-only` label exempts; closure-linter checks labels first |
| Hyper / axum / tower pulled in by `ee serve` work | Forbidden-dep audit gate (already exists) catches it; force defer-to-v2 in that case |

---

## 24. Definition of Done (this bridge)

Part II is "done" — implementation is the swarm's job, not the plan's — when:

1. Every gap in §14–§17 has an open `implements-surface:X` bead with a self-contained description embedding the §9 acceptance contract, properly labeled, properly prioritized, and dependency-linked to its gating predecessors.
2. The new gates in §21.2 each have an open task bead.
3. `bv --robot-triage` shows the Wave-4 beads in priority order with no cycles, and at least one bead is ready (unblocked) for the swarm to pick up.
4. The `__OPUS` rename + GEMINI cleanup beads have prereq-only dependencies (i.e., reachable without further planning).
5. The closure-linter and vision-coverage gates remain green after the new beads are created.

Mechanical verification: `br list -p bd-3usjw --status=open --json 2>/dev/null | jq 'length == 0'` returns `true`.

After this Part II ships, the canonical plan file is the renamed `COMPREHENSIVE_PLAN.md`; the renamed/archived `CLOSE_THE_GAP_PLAN` becomes a historical artifact under `docs/archive/`.

---

# PART II — Ambition Round 1 Additions

> Written 2026-05-14 after self-administered ambition round. The Part II
> baseline (§13–§24) is decent but barely scratches the surface. Five
> dimensions of escalation follow. **None of these new sections create
> new plan documents — they extend Part II in-place.**

## 25. README invariant test suite (post-ambition extension)

The vision-coverage scanner reads CLI surfaces and plan-doc sections. It is blind to **prose promises** in README that aren't a command. Examples Phase 1 audit missed:

- L935: "p50: 8 ms / p99: 22 ms for `ee remember`" — quantitative claim, never re-measured against the baseline
- L1043: "Not a multi-process write fortress. … Don't run a swarm of writers without `ee daemon`." — invariant; no test asserts this
- L1083: "Does it ever rewrite my memories silently? No. The steward proposes; you approve." — invariant; partial coverage in `tests/audit_no_silent_mutation.rs` but it isn't anchored to this README sentence
- L188: "Harmful feedback demotes faster than helpful feedback promotes." — quantitative; no test pins the ratio
- L726: "Asupersync, so every long operation respects `&Cx`, budgets, and `Outcome`." — cross-cutting; no per-command verification

### 25.1 `implements-surface:readme_invariant_harness`
- **Surface:** `tests/readme_invariants/` module
- **What it does:** Mechanical extraction of every README sentence containing one of: `always`, `never`, `every`, `deterministic`, `byte-stable`, `reproducible`, `must`, `cannot`, a number with a unit (`8 ms`, `4k tokens`, `60 days`), or any of these specific verbs in present tense: `surfaces`, `enforces`, `prevents`, `detects`, `quarantines`, `decays`, `audits`, `redacts`. Each extracted sentence is paired with either (a) an existing test file path or (b) a `defer:<bead-id>` marker. The pairing lives in `tests/readme_invariants/manifest.toml`.
- **Acceptance:** Every README invariant sentence has a manifest entry; CI walks the manifest and asserts each entry is current. Drift in either direction (new invariant added to README without manifest entry; manifest entry referencing deleted test) fails CI.

## 26. Plan-doc per-section verification commands (upgrade to §17.1)

The plain "Implemented-verified | Implemented-unverified | Stubbed | Missing" matrix is too shallow. Upgrade:

For every `Implemented-verified` row, the manifest carries a `verify_cmd` column: a one-line shell command that proves the surface works, e.g.:
```
cargo test e2e_pack_determinism::same_input_same_output -- --exact
```
For every `Implemented-unverified` row, the `evidence_path` is `pending`, and a `test_bead_id` is required.

CI gate `scripts/verify.sh --plan-doc-smoke` runs the full set of verify commands. Each must finish in < 60 seconds (else split into a slower lane).

### 26.1 New bead: `implements-surface:plan_doc_verify_cmds` (extends §17.1)
- Blocks on bd-3usjw.14 (plan_doc_sweep)
- Acceptance: every plan section row has either a `verify_cmd` or a `test_bead_id`; running `verify.sh --plan-doc-smoke` exits 0.

## 27. Per-dep micro-beads under `bd-3usjw.11` (franken_dep_publishing)

The 26 path-deps need individual triage. Create 26 sub-beads under `bd-3usjw.11`, one per crate. Each carries:

- **Current state:** path / version published on crates.io / forbidden-dep status of its current default features
- **Target state:** crates.io-resolvable at the version Cargo.toml needs, with forbidden-dep-clean default features
- **Blocker:** specific issue (e.g., "`frankensearch/fastembed` pulls reqwest+tokio; needs feature surgery upstream")
- **Acceptance:** `cargo publish --dry-run -p <crate>` clean from this repo

Top-priority sub-beads (impossible to defer):
- `asupersync` — verify version pin matches crates.io 0.3.1 (likely fast)
- `sqlmodel-core`, `sqlmodel-frankensqlite` — only 2 sub-crates of sqlmodel_rust
- `frankensearch` — core dep, feature surgery needed
- `franken-agent-detection` — small surface, fast publish

Deferrable sub-beads (can be feature-gated):
- All 16 `fsqlite-*` sub-crates (transitive via `sqlmodel-frankensqlite`)
- All 5 `fnx-*` crates (gated behind `graph` feature already, but still need crates.io presence for `cargo install ee --features graph`)
- `toon` (as `tru`) — output format, opt-in via `EE_DISABLE_TOON`

## 28. v2 design ADRs for honest defer-to-v2 surfaces (upgrade to §14.4)

`ee serve` (and any future surface that takes Path B "honest defer-to-v2") MUST have a v2 design ADR landing in the SAME PR as the v1 honesty sentinel. Otherwise the v1 stub becomes load-bearing forever.

### 28.1 New bead: `adr:serve_v2_design`
- Surface: `docs/adr/0030-serve-localhost-v2-design.md`
- Content: how the v2 surface meets forbidden-dep constraints (in-tree HTTP/1.1 + SSE on `std::net`), what features it covers, what it explicitly does not (no WebSocket, no TLS, no remote-access by design), the v2 acceptance contract.
- Blocks on `bd-3usjw.4` (`ee serve` v1 if Path B taken)

## 29. Cross-platform determinism harness (gates Wave 5 GraphAccretion)

Graph algorithms produce different floating-point results across (target_triple, libm implementation, SIMD path). Without a cross-platform determinism harness, the GraphAccretion epic's "byte-identical golden snapshots" promise becomes unenforceable.

### 29.1 New bead: `implements-surface:cross_platform_determinism`
- **Surface:** `tests/cross_platform_determinism.rs`
- **What it does:** Pins a `(target_triple, algorithm, input-seed) -> output-hash` table. CI runs across Mac aarch64, Linux x86_64 glibc, Linux x86_64 musl, Linux aarch64, Windows MSVC matrices. If any platform produces a different hash, the test fails.
- **Floating-point strategy:** All graph algorithms run via `fnx-cgse` deterministic-mode wrappers (already a design goal of CGSE). For algorithms with platform-divergent results, declare the divergence in `tests/cross_platform_determinism/divergences.toml` with a `reason` and `affected_targets` list. Empty divergences file = strict mode.
- **Blocks:** every G-feature golden snapshot (G1-G8) and bd-8jvg (F4 determinism harness). This is the gating dependency for any G-feature ship.

## 30. Closure-linter regex extension for defer-to-v2 pattern

The existing closure-lint regex (`scripts/closure-lint.sh` line 33) catches `abstain|unavailable|degraded|stub|placeholder|removed simulation|honest empty|conservative abstention`. The new defer-to-v2 pattern (introduced in §28) needs detection.

### 30.1 Extend closure-lint
- Add `defer.*v2|deferred until v2|v1 honesty stub` to the abstention regex
- Carve-out: a bead may close with this pattern IF a sibling `adr:<surface>_v2_design` ADR exists AND a `v2:<surface>` bead exists in the open queue
- Carve-out is verified by the linter, not by trust

### 30.2 New bead: `closure_lint_defer_v2_extension`
- Surface: `scripts/closure-lint.sh` regex extension + carve-out logic
- Acceptance: linter detects defer-to-v2 close reasons; falsely closing without the ADR+v2-bead carve-out fails the lint

## 31. Performance hardware-pinning manifest (upgrade to §16.1)

README perf table publishes p50/p99 *on stated hardware* ("2024 MacBook Pro M3"). CI runs on GitHub Actions ubuntu-latest — different CPU, memory, disk profile. Without a hardware-pinning manifest, perf-table-sync writes ubuntu numbers into a README that claims Mac measurements.

### 31.1 New bead: `implements-surface:perf_hardware_manifest`
- **Surface:** `benches/baselines/hardware_classes.toml`
- **Content:** Named hardware classes (`mac-m3-pro`, `linux-x86_64-c5large`, `linux-aarch64-c6g-large`) with bench artifacts pinned per class.
- **README treatment:** The Performance table either (a) declares one canonical hardware class and links the others to a separate doc, or (b) shows per-hardware-class columns.
- **Acceptance:** README perf table cannot be synced from an arbitrary `ee-perf.v1.json` — only from one matching a declared hardware class.
- **Sync script update:** `scripts/sync_perf_table.sh` rejects artifacts not pinned to a hardware class.

## 32. MCP parity fixture-driven test set (upgrade to §16 / §14.3)

§14.3 promised "schemas match CLI JSON exactly" but didn't define the parity test surface.

### 32.1 New bead: `implements-surface:mcp_parity_test_suite`
- **Surface:** `tests/mcp_parity/` module with one `<surface>_parity.rs` per CLI surface that has both a CLI command and an MCP tool
- **Test pattern:** Table-driven over an input-vector corpus per surface (`tests/mcp_parity/<surface>/inputs/*.json`). For each input vector, invoke (a) the CLI with `--json` and (b) the MCP tool. Assert the responses are byte-identical (modulo wall-clock fields like `generated_at`).
- **Coverage gate:** Every surface with `#[derive(McpTool)]` (or equivalent registration) must have a parity test, enforced by `tests/mcp_parity_coverage.rs`.
- **Acceptance:** All parity tests green under `--features mcp`. Drift between CLI and MCP fails CI.

## 33. User-permission request beads for file deletions (AGENTS.md Rule 1 compliance)

AGENTS.md Rule 1 forbids file deletion without explicit user permission. Several Part II beads naturally lead to a deletion request (orphan plan, GEMINI reports, eventually the abstention paths once their implementing bead closes).

### 33.1 New bead: `permission_request_delete_orphan_plan`
- **Surface:** Single user-facing question: "May I delete `COMPREHENSIVE_PLAN_TO_MAKE_EE.md` (12,938 LOC, unreferenced orphan, verified zero unique content vs. `COMPREHENSIVE_PLAN__OPUS.md`)?"
- **Acceptance:** User answers yes or no; either way bead closes with the answer captured.
- **Defaults:** If no answer in 2 weeks, defaults to archive (move to `docs/archive/`), not delete.

### 33.2 New bead: `permission_request_delete_dead_unavailable_paths`
- **Surface:** Single user-facing question per surface in §2 of Part I: "Once `implements-surface:<X>` closes, may I delete the now-dead `*_UNAVAILABLE_CODE` constant + its abstention branch?"
- **Acceptance:** Per-surface user answer captured in the bead's metadata. Default to YES per CLOSE_THE_GAP §11 ("user's standing instruction to clean up dead code in-place") — but this bead exists to make that standing instruction explicit and re-confirmable.

## 34. Cost-of-omission line on every implements-surface bead (retrofit)

Every Part II implements-surface bead should declare what breaks if the bead never closes. Retrofit the existing beads with a `COST OF OMISSION:` line in their description. Examples:

- `db_inspect`: "Agents corrupt WAL via direct sqlite3 access. Probability: medium. Blast radius: a single workspace DB loses a few minutes of writes if a `cleanup_expired_locks` hot loop hits during a checkpoint."
- `trauma_guard_preflight`: "README L52 promise misleading; first user encountering an avoidable `git reset --hard` incident reports it. Probability: high (within first month of v0.1.0). Blast radius: lost work + reputation."
- `first_signed_release`: "All three README install paths fail; users who follow the README hit `curl: (22) … 404 Not Found` and abandon. Probability: 100% for every new user. Blast radius: zero adoption."
- `crate_name_resolution`: "Project cannot publish to crates.io. Probability: 100%. Blast radius: README L283 line is a lie."

## 35. Inter-bead dependency DAG for N1-N15 alien-artifact features

The ambition-tier N1-N15 beads have implicit precondition relationships that aren't in the bead graph. Add edges so the swarm can pick leaves without violating preconditions:

```
N4 (typed determinism) → N1 (pack-as-tensor)
N4 → N7 (Bayesian posterior — needs Deterministic<Seed>)
N4 → N11 (active learning — needs deterministic exploration)
N7 → N11 (active learning consumes Beta posteriors)
N7 → N12 (anti-patterns from outcomes consumes posterior updates)
N2 (conformal calibration) → N11 (uncertainty quantification feeds bandit)
N9 (mmap'd index) → N10 (SIMD BM25 hot path)
N4 → N9 (deterministic mmap layout)
N3 (do-calculus causal credit) ← N7 (needs posterior on outcome events)
N15 (counterfactual lab) ← N4 + N1 (needs determinism + binary pack)
```

### 35.1 New bead: `dag_n1_n15_inter_edges`
- **Surface:** `br dep add` calls
- **Acceptance:** Running `bv --robot-insights` on the bd-17c65.14.* family returns cycle-free with the above edges, and the topological order matches §20 sequencing.

---

*End of Ambition Round 1. Beads from §25–§35 to be created in Phase 3a-redux.*

---

# PART II — Ambition Round 2 Additions

> Written 2026-05-14 after a second self-administered ambition round.
> Round 1 hardened the per-section content. Round 2 attacks
> **process/feedback gaps**: how does this bridge plan stop being stale?
> How does the swarm learn from prior bridges? How does the verify-time
> stay survivable as gates accrete?

## 36. Bridge-plan staleness gate

Part I (2026-05-06) became stale in 8 days. Without a gate, Part II goes stale the same way.

### 36.1 New bead: `implements-surface:bridge_staleness_gate`
- **Surface:** `scripts/bridge-staleness.sh --json` + companion `tests/bridge_staleness_gate.rs`
- **What it checks:** (a) Latest mtime of `CLOSE_THE_GAP_PLAN.md`; if > 30 days, raises severity=`medium`. (b) Vision-coverage gap_percentage; if < 2%, raises severity=`low` ("bridge mostly done, plan Part III"). (c) % of open beads under bd-3usjw with status=in_progress; if = 0% AND mtime > 7 days, raises severity=`medium` ("swarm not eating the bridge").
- **CI:** Runs in `scripts/verify.sh` as a non-blocking advisory gate; emits report at `.bridge-staleness-report.json`.
- **Cost of omission:** Part III planning fires 90 days late because nobody notices Part II completed.

## 37. File-surface labels for swarm coordination

Multiple in-flight beads can compete for the same files. Today's bead graph doesn't declare this; Agent Mail can't pre-reserve.

### 37.1 Retrofit: add `file_surface:` annotations to every implements-surface bead description
- Examples:
  - bd-3usjw.1 (db_inspect): `FILE SURFACE: src/cli/mod.rs (handler registration), src/core/db_inspect.rs (NEW), src/db/mod.rs (read-only queries), tests/e2e_db.rs (NEW), tests/golden/db_*.snap (NEW)`
  - bd-3usjw.11 (franken_dep_publishing): `FILE SURFACE: Cargo.toml [dependencies] + [dependencies.<crate>] blocks (lines 65-95 and 173-225)`
  - bd-3usjw.6 (trauma_guard_preflight): `FILE SURFACE: src/cli/mod.rs, src/core/preflight*.rs, src/policy/destructive_patterns.rs (NEW), tests/fixtures/destructive_patterns/ (NEW)`
- Acceptance: every Part II bead has a `FILE SURFACE:` paragraph. `mcp_agent_mail::file_reservation_paths` can read these to pre-claim.

### 37.2 New bead: `implements-surface:bead_file_surface_extractor`
- **Surface:** `scripts/extract_bead_file_surfaces.sh`
- **What it does:** Parses every bead description for `FILE SURFACE:` paragraphs, emits a JSON map `{bead_id: [paths]}`. Agent Mail's pre-commit guard reads it.

## 38. Reality-check cadence in AGENTS.md

Make the cadence explicit so future bridges fire on schedule, not because someone manually notices drift.

### 38.1 AGENTS.md additions
- New section "Reality-Check Cadence" inserted after "Beads Workflow Integration" (L1113).
- Content: "Every 90 days OR whenever vision-coverage gap_percentage > 5% reappears, run the `/reality-check-for-project` skill end-to-end. The bridge plan lives at `CLOSE_THE_GAP_PLAN.md` (or its archived predecessor). Update Part N→N+1, never proliferate plan files."
- Last bridge ran: 2026-05-14. Next bridge due: 2026-08-13 OR when gap > 5%.

## 39. Bridge execution log

`docs/bridge_execution_log.md` — written by each bridge author with closing summary of the prior bridge.

### 39.1 New bead: `docs_bridge_execution_log_seed`
- **Surface:** `docs/bridge_execution_log.md` (NEW)
- **Seed content:** Captures Part I (2026-05-06): created 20+ stub-recovery beads, # closed by 2026-05-14, lessons (honesty-only-as-substitute pattern caught and process-fixed). Captures Part II (2026-05-14): created 21 beads under bd-3usjw, expected closure timeline, expected blockers (ewpratten communication, franken-dep publishing).
- **Acceptance:** Doc exists; next bridge author has a tabular record of prior cadence and outcomes.

## 40. Closure-quality signal: retroactive-reopens

bd-2xl8v was closed then reopened by the 2026-05-06 audit. That's a tell.

### 40.1 New bead: `closure_lint_retroactive_reopen_signal`
- **Surface:** `scripts/closure-lint.sh` extension + `.closure-quality-report.json`
- **What it does:** For every closed bead, computes `time_to_reopen` if it was ever reopened. If reopened within 14 days, marks as `quality_signal=premature_closure`. Aggregates report shows trend over last quarter.
- **NOT a hard fail:** advisory only. The signal feeds bridge-plan retrospectives.

## 41. README-CLI parity test (extends Vision Coverage)

Vision-coverage gap finds plan-doc <-> CLI drift. It does NOT find README <-> CLI drift.

### 41.1 New bead: `implements-surface:readme_cli_parity`
- **Surface:** `tests/readme_cli_parity.rs`
- **What it does:** Walks every command table row in README.md (sections "Core workflow", "Pack replay evidence", "Swarm brief workflow", "Import & ingestion", "Curation & rules", "Memory inspection", "Graph", "Index", "Workspace, models, schemas", "Backup & restore", "Diagnostics, eval, ops"). For each row, asserts the command exists in `src/cli/mod.rs` clap registration and produces non-empty `--help`.
- **Catches:** README rows for commands that were renamed/removed; CLI commands that exist but README forgot to document.

## 42. Verify-time budget enforcement

`scripts/verify.sh` is already 9 stages. After Part II it'll be 13+. Verify-time must stay survivable.

### 42.1 New bead: `implements-surface:verify_time_budget`
- **Surface:** `scripts/verify-budget.toml` (NEW) + extension to `scripts/verify.sh`
- **Content of budget.toml:** Per-stage `expected_seconds_p50` and `regression_factor=1.5` columns.
- **Enforcement:** Every stage's elapsed time recorded; if > p50 × 1.5, emit advisory; if > p50 × 3, fail the gate.
- **Total budget:** 10 minutes for the full `verify.sh` run on a mid-tier dev machine. Bench gate excluded.

## 43. Defer-to-v2 with explicit calendar deadline

§28 introduced ADR-required defer-to-v2. Add a calendar deadline.

### 43.1 Closure-lint regex extension
- A bead closing with `defer.*v2` MUST also include `defer_until_iso8601: YYYY-MM-DD` (within 180 days of closure date).
- After the date, closure-lint reopens the bead automatically with comment "deferral expired; honest v1 is now a load-bearing stub."
- Carve-out: user can renew once with `defer_renewed_until_iso8601: YYYY-MM-DD` and a `defer_renewal_reason:` paragraph.

## 44. Anti-pattern extraction from prior dishonest closures

AGENTS.md L835 cites the 14 follow-up beads closed without implementation as the pattern Part I fixed. Mine that dataset.

### 44.1 New bead: `learn_from_dishonest_closures_2026_05`
- **Surface:** A single procedural memory captured via `ee remember --kind anti-pattern --severity high`
- **Content:** "When a parent bead has many follow-up beads that close via `add an *_unavailable abstention sentinel + file a follow-up bead`, the pattern is dishonest closure. The follow-up beads recursively close the same way unless a closure linter forbids the pattern. Mitigation: implements-surface labels + closure linter + vision-coverage gate (2026-05-06 fix). Source: 14 beads closed against parent `eidetic_engine_cli-jp06` between 2026-04 and 2026-05."
- **Acceptance:** Memory persisted; future `ee context` for "bead closure" or "honest implementation" surfaces it.

## 45. Bead obsolescence pass

169 open beads — some are likely stale.

### 45.1 New bead: `implements-surface:bead_obsolescence_pass`
- **Surface:** `scripts/bead-obsolescence.sh --json` + `bv --robot-insights` invocation
- **What it does:** For every open bead, computes `days_since_update`. For days > 14 AND not on critical path AND no in_progress siblings AND no recent comment, proposes one of: (a) priority demote, (b) close as obsolete (with user confirmation per AGENTS.md Rule 1 if it requires removing artifacts), (c) re-target to current epic.
- **Output:** `.bead-obsolescence-report.json` with one row per candidate.

## 46. Untracked-work audit

Phase 1 found: `src/graph/ppr.rs` untracked, `docs/architecture/` untracked, several modified files unclaimed by any in_progress bead. This is a violation of "all work in beads."

### 46.1 New bead: `untracked_work_audit_2026_05_14`
- **Surface:** `scripts/untracked-work-audit.sh`
- **What it does:** Walks `git status --porcelain` and `git ls-files --others --exclude-standard`; for every modified or untracked path, queries open in_progress beads' `FILE SURFACE:` annotations (per §37.1). If no bead claims the path, emit `untracked_work_orphan` warning with suggested bead.
- **Run-it-now action:** As part of Phase 3a-redux, manually classify the 11 modified files + 5 untracked items from this session's `git status` and file beads for any unclaimed work.

## 47. Plan-drift warning in bv triage

When the plan doc evolves after bead creation, the bead description goes stale.

### 47.1 New bead: `implements-surface:plan_drift_warning`
- **Surface:** Every implements-surface bead carries a `plan_doc_section: §XX.Y` metadata field
- **Detection:** `scripts/plan-drift.sh` diffs the named section against the bead description; if the section changed since bead creation, emit `plan_drift_warning` in `bv --robot-triage` output for that bead.
- **Acceptance:** Operator sees plan-drift warnings before claiming a bead; can re-read the plan section before working.

---

*End of Ambition Round 2. Round-2 beads from §36–§47 to be created in Phase 3a-redux.*

---

# PART II — Ambition Round 3: Alien-Artifact Math Injection

> Written 2026-05-14 after a third ambition pass focused on esoteric
> math and extreme optimization. `ee` is a memory substrate with
> provenance, confidence, and graph relations — exactly the primitives
> that the last 60 years of statistics, graph theory, and algorithm
> design exploit hardest. The N1-N15 beads gesture at this; Round 3
> goes deeper.
>
> Companion skills referenced: `$alien-artifact-coding`,
> `$extreme-software-optimization`.

## 48. Conformal prediction sets on `ee why` and `ee context`

The N2 bead mentions conformal calibration of *relevance scores*. Go further: **split conformal prediction** produces guaranteed prediction *sets*, not just calibrated scores.

### 48.1 New bead: `implements-surface:conformal_prediction_sets`
- **Surface:** Extension to `ee why <id> --json` response envelope; new `confidence_intervals` field
- **Math:** Split-conformal with exchangeable scoring on a held-out calibration fold of historical query→correct-memory pairs. Yields a *prediction set* `{memory_ids}` with marginal coverage probability ≥ 1−α (default α=0.05). Computed lazily and cached on `graph_algorithm_results`.
- **Why it matters:** Today `ee why` returns ranked scores. Agents can't tell "is the top result likely right or barely beating the runner-up?" Conformal sets give a principled "the correct answer is in this small set with 95% probability" guarantee.
- **Provenance:** Each prediction-set entry carries its conformal nonconformity score.
- **Acceptance:** On the fixture corpus, marginal coverage on a held-out fold matches the target α within ±2%; tests/conformal_coverage.rs verifies.
- **Cost of omission:** Agent confidence calibration is heuristic. Worst case: agents overweight low-quality memories because the score looks high.

## 49. Sieve-Streaming consolidation for the daemon

Daemon consolidation today is O(N²) average-linkage. Replace with **Sieve-Streaming** for monotone submodular maximization under cardinality constraint k.

### 49.1 New bead: `implements-surface:sieve_streaming_consolidator`
- **Surface:** `src/steward/consolidate.rs` consolidation pass
- **Math:** Sieve-Streaming (Badanidiyuru, Mirzasoleiman, Karbasi, Krause 2014). (1/2 − ε)-approximation guarantee. Single pass over the event stream. O(k log k / ε) memory.
- **Implementation:** Submodular objective = facility-location-like coverage of memories under a similarity metric (the same metric already in `src/pack/mod.rs`).
- **Performance target:** 10k-memory consolidation pass under 200ms (was multi-second with N² linkage).
- **Acceptance:** Cross-validation on the eval-fixture set shows consolidation quality within 5% of optimal greedy; performance benchmark `bench_consolidator_sieve_streaming` in `benches/`.
- **Cost of omission:** Daemon consolidation is slow and gets quadratically slower as memory bank grows.

## 50. Min-hash signature ranks for cross-platform determinism

Round 1 §29 framed cross-platform determinism via per-target pinning. Better: replace floating-point centrality with **min-hash signature ranks** over edge sets — integer-only, byte-stable across all platforms by construction.

### 50.1 New bead: `implements-surface:minhash_rank_centrality`
- **Surface:** `src/graph/minhash_rank.rs` (NEW) + integration into `ee graph centrality-refresh`
- **Math:** For each node, compute k min-hash signatures over its incoming-edge set. Rank nodes by signature density. Approximates top-K-PageRank with Spearman correlation > 0.9 on the eval corpus.
- **Why it matters:** Eliminates floating-point determinism problem entirely for top-K queries. Integer arithmetic is bit-for-bit identical across (x86_64, aarch64, MSVC, glibc, musl).
- **Coexistence with PageRank:** PageRank stays for full-spectrum centrality; min-hash-rank is the new default for `ee context` query-seeded re-ranking and `ee insights --section centrality-top-k`. Selector via config: `[search] centrality_algorithm = "minhash" | "pagerank" | "ppr"`.
- **Acceptance:** Spearman between min-hash-rank top-100 and ground-truth PageRank top-100 ≥ 0.9 on fixture corpus; cross-platform byte-equivalence on the determinism gate; benchmarks show ≥5× speedup for top-K queries.
- **Cost of omission:** Cross-platform determinism stays brittle; G-features can't ship a single golden snapshot.

## 51. SPRT for harmful-feedback quarantine

Today's `harmful_per_source_per_hour=5` quarantine threshold is heuristic. Replace with **Wald's Sequential Probability Ratio Test**.

### 51.1 New bead: `implements-surface:sprt_quarantine`
- **Surface:** `src/core/outcome.rs` quarantine pipeline
- **Math:** SPRT on per-source harmful-vs-helpful ratio. Hypothesis H0: source is benign (p_harmful ≤ 0.1). Hypothesis H1: source is bad (p_harmful ≥ 0.4). α=0.01, β=0.05. Stopping bounds A=log((1−β)/α), B=log(β/(1−α)). Update test statistic on every outcome event; quarantine when statistic > A.
- **Why it matters:** Statistically optimal stopping time. Smaller false-positive rate than the current burst-window heuristic. Faster catch of legitimately bad sources (often quarantines in 8-12 events vs. waiting for the 1-hour window to close).
- **Backward-compatibility:** Existing burst-window config stays as a coarse-grained fast-path; SPRT runs in parallel as the precision path. Both can fire; first to trigger quarantines.
- **Acceptance:** Synthetic source-stream tests in `tests/sprt_quarantine_unit.rs`; false-positive rate within α budget; mean-detection-time improved on the corpus.

## 52. Influence-function counterfactuals on `ee why`

Replace heuristic "score components" in `ee why` with **leave-one-out influence-function attribution**.

### 52.1 New bead: `implements-surface:influence_function_why`
- **Surface:** Extension to `ee why <id> --json`; new `counterfactual_influence` field per pack-mate
- **Math:** Koh & Liang 2017 influence-function approximation. For each memory in the pack, compute the influence-function-estimated change in the top-1 rank if the memory were removed. Avoids the O(N) cost of actually re-ranking N times.
- **Acceptance:** Sum of absolute influences ~ matches single-leave-one-out re-rank delta within 5% on fixture; `ee why` JSON shows top-3 influencers and bottom-3 (memories that pull the ranking the most positively and negatively).
- **Cost of omission:** Agents can't ask "remove which memory would most change the answer?" cheaply.

## 53. Andersen-Chung-Lang local PageRank for G1

G1 (Personalized PageRank for query-seeded re-ranking, `bd-ov09`) leaves algorithm choice open. Pin it to **ACL local random walks** for the bounded-cost guarantee.

### 53.1 Upgrade bd-ov09 acceptance
- ACL algorithm (Andersen, Chung, Lang 2006). Localized random walks bounded by ε-error. O(1/ε · 1/(1−c)) cost vs. O(n) for global PageRank.
- For typical pack-size-100 query, this is 100-1000× faster than global PPR with rank-1 accuracy preserved (in practice rank-K with K=100 stays equivalent).
- Add acceptance criterion: `ee context "<task>"` with PPR re-ranker enabled completes in p50 < 50ms on the 14k-memory fixture corpus.

## 54. Radix sort for ULID tie-breaking on hot path

Deterministic tie-breaking by `memory_id` is called 5+ places. `sort_by_key` is O(N log N) with comparator calls.

### 54.1 New bead: `implements-surface:radix_ulid_sort`
- **Surface:** `src/util/radix_ulid_sort.rs` (NEW)
- **Math:** Radix sort on 26-char base32 ULIDs. O(N · L) with L=26 bytes, fully integer arithmetic. Stable by construction.
- **Hot-path callsites to convert:** Search result ranking, pack candidate sorting, graph rank tie-breaking, memory listing, audit timeline projection.
- **Performance target:** ≥5× speedup on the pack-size-100-from-10k-candidates ranking step.
- **Acceptance:** Bench `bench_ulid_tiebreak_radix_vs_compare` shows ≥5× speedup; tests/radix_ulid_sort_proptest.rs verifies stability and permutation invariance.

## 55. Roaring bitmap encoding for graph_algorithm_results param keys

The proposed `graph_algorithm_results` cache table keys results by `(algorithm, snapshot_hash, params)`. Params today are JSON-serialized — verbose and slow to intersect.

### 55.1 New bead: `implements-surface:roaring_params_cache`
- **Surface:** `src/graph/result_cache_keys.rs` (NEW)
- **Math:** Roaring bitmap (Lemire et al., 2016). Params encoded as bit-positions in a compressed bitmap. Intersection in O(min(|A|, |B|)) for "which cached results have overlapping params."
- **Compression:** 3-10× smaller than JSON in the typical case (sparse param sets).
- **Acceptance:** `bench_roaring_vs_json_cache_key` shows ≥3× cache-key size reduction; tests verify intersect/union/difference correctness.
- **Coexistence:** Storage layer accepts both encodings during transition; closure-lint forbids new JSON-keyed inserts after rollout.

## 56. Add a `math_ambition` label and inter-dependency edges

Beads §48–§55 are math-ambition tier. They should NOT block release. Add label `math_ambition` + parent `bd-17c65.14` (alien-artifact umbrella).

Inter-dependency edges within §48–§55:
- §48 (conformal) blocks on §53 (ACL PPR) — needs accurate centrality for the calibration baseline
- §49 (sieve-streaming) blocks on N4 typed-determinism
- §50 (min-hash) blocks bd-8jvg (F4 determinism) — replaces float-based determinism with integer-based for top-K
- §52 (influence-function) blocks on §53 (ACL PPR) — uses gradient of ranking objective
- §54 (radix ULID) is standalone, ships any time
- §55 (Roaring cache) blocks on bd-igvt (F2 algorithm wrappers) — needs the cache plumbing

---

*End of Ambition Round 3. Round-3 beads from §48–§56 to be created in Phase 3a-redux. With 47 numbered sections and 8 ambition-tier math additions, this bridge plan now spans every reachable gap from release-readiness to PhD-level algorithm work, with explicit cost-of-omission for each.*
