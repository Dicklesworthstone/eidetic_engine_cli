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

*End of bridge plan. Next step: Phase 3a bead generation per the
frozen template.*
