# CLOSE_THE_GAP_PLAN — `ee` (Eidetic Engine CLI) — **PART III, TWO-TRACK CONVERGENCE (2026-08-17; reality-check revisions 2026-09-04, 2026-10-01, 2026-10-22)**

> Track A: mesh / team-confederation acceptance reconciliation after the Unix
> EE-to-EE campaign. Track B: core-product, durability, verification, and
> release convergence uncovered by the 2026-08-23 full-project reality check.
>
> **Status: ACTIVE (Part III).** Parts I and II (2026-05) are archived at
> `docs/archive/close_the_gap_2026-05.md`. This file is the in-place Part III
> revision required by `AGENTS.md` *Reality-Check Cadence*. Do not create a
> second plan file at the repo root.
>
> **2026-10-22 re-baseline (§17, read this first; it supersedes the §16 header
> below).** Track R worked. Measured black-box on the shipped `v0.18.0` against
> the `.43` oracle corpus, five of the six headline 2026-10-01 failures are
> fixed and two of the five outcome metrics now MEET their targets:
> **UTR 1.0000** (was ~0.65, target ≥ 0.95) and **PAP precision 1.00 / recall
> 0.83** (was 0/10, target ≥ 0.70). Search is **0.83 s** and pack **1.14 s** at
> 4,009 spans (were 11.1 s / 8.7 s), import is **0.60 s per session** (was ~18 s),
> retained generations are **2** (were one full copy per write, capped at 1,000),
> and CASS evidence now reads as projected `assistant:` / `user:` text instead
> of raw JSONL envelopes.
>
> Three things block the bridge, and the first two are new:
> - **A fully populated store can return zero results while every health
>   surface says it is fine.** With the default model-download policy, a
>   CASS-imported store answers every query with 0 results and
>   `index_incompatible`, while `ee index status` reports `ready`/no
>   degradation, `ee status` reports `posture.overall = ok`, and **`ee doctor`
>   reports `posture: ok` with zero non-ok checks**. The per-response repair
>   (`ee index rebuild`) is correct and works; no health surface ever offers it.
>   NO_BEAD at audit → `.66` (P0).
> - **The instrument for every Track R metric cannot run on the developer's own
>   platform.** `scripts/real_shape_corpus/probe.py` (`.63`) dies at import on
>   macOS, reports an empty reason when it does, and its model passthrough sets
>   a variable that is not in the env registry. Run as shipped it publishes
>   UTR null, FAR null and **recall@5 = 0.0** against a product that retrieves
>   correctly — a false-red gate. NO_BEAD at audit → `.67` (P0).
> - **Hosted verification has been dark for 14 days.** GitHub Actions is
>   `enabled: false` repo-wide; the last run of anything was 2026-10-08 and it
>   failed on an **allowed-actions policy** ("all actions must be from a
>   repository owned by Dicklesworthstone") that rejects `actions/checkout@v4`,
>   `dtolnay/rust-toolchain@nightly` and `cargo-deny-action` — a configuration
>   failure, not a code verdict. Owned by `bd-actions-disabled-repo-wide-33mfm`;
>   the policy root cause is recorded there by this pass.
>
> `bd-3k1mg`, `bd-16imy` and `bd-3h6bz` — three named §15 archive criteria —
> are now closed. §8 rows were revised in place; §17 carries the evidence.
>
> **2026-10-01 real-data re-baseline (§16, historical).** The shipped
> `v0.16.0` now passes the small-store loop end to end: concurrent determinism,
> complete fresh-store backup, and cited `ask`. On real Claude Code transcripts
> it fails usefulness and scale:
> - CASS is refused at its standard install path, and the documented config
>   opt-in is dead code;
> - evidence is packed as raw JSONL;
> - session proposals are template junk;
> - search and pack take 9–11 s after 4.9k spans;
> - every write rebuilds the whole index (a regression since 2026-08-06) and
>   leaves a full retained copy, with a hard failure after 1,000 writes.
>
> Main is red on `CI Static`. Children `.41`–`.65` own the fixes, and §16.5
> defines the outcome metrics (UTR / TTFUC / PAP / FAR / WCS) that gate them.
> The §8 rows below were revised in place.
>
> Companions: `docs/adr/0085-typed-pack-entity-identity.md`,
> `docs/adr/0086-team-memory-confederation.md`,
> `docs/mesh/team_confederation_plan.md`,
> `docs/mesh/verification_matrix.md`, `README.md`.
>
> **2026-08-23 audit addendum (full project reality check).** All remainder
> closures were re-verified against current Beads evidence. That audit found a
> mismatch: `.3.8` closed on two-host evidence while §5 required two humans.
> The two-host boundary was accepted on 2026-09-17 on delegated authority
> (`bd-reality-core-convergence-1azkt.8`; its close struck the earlier "user
> approved in WildBluff's session" provenance as unverifiable). The earlier claim that `.3.9` had an empty
> close reason is stale: its current reason names the property/fuzz surface,
> remote host, duration, and commit. Windows, the publication fence, the
> narrowed fake-IdP v1 decision, remaining fuzz, and the program closeout all
> have recorded dispositions. The scope amendment resolves this mismatch;
> Part III still cannot archive until the broader §15 criteria are satisfied.
>
> The same audit found a broader core-product bridge (§§6–13 below). The
> implementation is substantial, but the core CASS → retrieval → pack and
> curate → rule → retrieval loops are open, identical concurrent search/pack
> requests are not deterministic, current verification is red or inconclusive,
> and release/performance evidence is weaker than the README claims. This file
> remains Part III instead of opening a competing Part IV while §15 is unresolved.
>
> **2026-08-24 independent rerun addendum.** The reality check was repeated from
> the complete `AGENTS.md`, `README.md`, controlling plans/ADRs, implementation,
> tracker, installed release, hosted release/CI, and source-attested verification
> surfaces. Current `main` is `c355087fe23f809719a3b1d510fc3f735389a606`
> (`v0.14.2-98-gc355087f`), with source package version `0.14.3`; the installed
> probe binary remains release `0.14.2`. Source now has a real, first-class
> `PackEvidenceItem` path for positively admitted CASS spans, persists those
> rows in `pack_evidence_items`, accepts targetless audit records during backup,
> and opens existing store-auth state without creating it during dry-run. Those
> are material improvements and invalidate the earlier `REGRESSED` labels for
> the whole CASS and backup goals. They do not yet provide the generic ADR 0085
> entity model, native rule identity, a complete durable backup inventory,
> evolved-store recovery proof, calibrated lexical relevance, or deterministic
> immutable index generations. README still describes unlinked CASS evidence as
> un-packable, so source and public contract have also drifted in the opposite
> direction.
>
> A fresh installed-`0.14.2` walking-skeleton probe did persist, search, pack,
> and explain one manual procedural memory with provenance, confirming that the
> ordinary component path is real. Its status is nevertheless
> `degraded_recoverable` because the selected workspace index is missing/stale,
> and its version output cannot attest a source commit or target. The pinned
> current-source RCH command did not reach compilation: `rch_verify.sh`
> correctly refused the source state because `franken-stack.lock` materializes
> FrankenSQLite/fsqlite `0.3.9` while `Cargo.lock` still resolves `0.3.7`.
> `contract-drift-radar --strict` separately found that the dependency contract
> documents still expect Asupersync `0.3.9` while `Cargo.toml` requests `0.4.9`.
> Closure lint, verification drift guard, and the 134/134 surface-oriented
> vision inventory pass; none compiles or proves product behavior. Hosted CI run
> `32759250334` for this exact SHA was cancelled with zero jobs, as were the
> immediately preceding main runs. This is a source-state/proof failure, not
> evidence that compilation or the product suite passed.
>
> The bridge was then re-run independently through three ambition rounds
> (agent outcome, durable recovery/operation, and proof/release ambition) and
> five plan-space refinement passes (dependency order, observability/oracles,
> privacy/failure handling, shared-checkout/execution risk, and final
> clarity/scope). No additional unowned controlling goal emerged. The passes
> sharpened `.1`, `.10`, `.13`, and `.18`; the existing epic plus reused blockers
> still covers all 24 checklist rows. Its 23 records remain 20 open and 3 in
> progress, and `.22` still graph-blocks on every mandatory child and reused
> blocker. No duplicate plan or Bead was created.
>
> **2026-09-01 current-HEAD refresh.** The audit was repeated end to end at
> `c716ae31ecdc2628fc80f609aa453222eddd6787` (`v0.14.4-26-gc716ae31`), source
> package `0.14.4`, while the installed probe remains unattested release
> `0.14.2`. A pinned committed-tree RCH `cargo check --locked --all-targets`
> now reaches compilation and passes against the exact seven-repository
> `franken-stack.lock` bundle. That materially supersedes the 2026-08-24
> lock-mismatch refusal, but the proof remains degraded by unavailable local
> build admission and a proof-broker source-state mismatch. Exact-HEAD pinned
> `cargo clippy --locked --all-targets -- -D warnings` also passes; focused
> behavioral tests, the complete manifest, and hosted CI remain separate gates.
>
> Current source also supersedes the old literal raw-BM25 saturation finding:
> a complete pure-lexical result pool now uses Frankensearch's canonical
> min-max normalization while retaining raw `lexicalScore`, and unit tests keep
> distinct `9/5/2` BM25 values distinct. `.11` remains open for the public
> query-relative/calibration contract and every downstream admission/quality
> consumer, not for re-applying the obsolete clamp fix. Rule North-Star tests
> now require the applied `RuleId` to be searchable by its own content and
> verify post-mutation projection metadata. The no-mock CASS flow now requires
> exact imported `SessionId` and `EvidenceId` search, but still stops before a
> public pack/replay/why/outcome closure on that same evidence identity. The
> exact focused test fails on the required RCH lane before import semantics:
> its workspace is derived from the remote `CARGO_TARGET_DIR`, and CASS path
> safety rejects a symlink component in that materialized path. This is a
> non-hermetic verifier failure, not proof of an ordinary physical-path import
> defect, but it prevents the test from serving as current readiness evidence.
>
> The proof surface is still materially weaker than the command inventory:
> vision coverage reports 137/137 and zero gaps from dispatch/file mappings,
> while bridge staleness still inspects Part II and recommends planning Part
> III despite this active 23-node bridge. README also contradicts itself by
> saying both that CASS excerpts require curation before packing and that a
> live-admitted unlinked evidence span packs directly. Fresh installed-release
> init/remember/search/pack/why succeeds, but search/pack/status report a stale
> or missing index while `ee index status` reports ready with equal generations.
> GitHub reports CI, macOS artifact, and release workflows as manually disabled;
> public `v0.14.4` is 26 commits behind this source and has archives, checksums,
> installers, and a manifest but no Sigstore or SLSA provenance asset.
>
> Four ambition passes (core agent outcome, operator recovery, integration and
> release, and outcome-measured product ambition) plus five refinement passes
> (deduplication, granularity, adversarial acceptance, dependency order, and
> final scope/clarity) found no missing top-level work item. Existing children
> `.5`, `.7`, `.9`, `.11`, `.16`, and `.17` received dated evidence comments;
> no new Bead was created, no status was advanced, and the scoped graph still
> has no cycle (20 open, 3 in progress, 0 closed).
>
> **2026-09-02 fourth-pass reality check (shipped-binary focus).** Repeated
> end to end at `a4289680` (`v0.14.4-77-ga4289680`), this time probing the
> **public `v0.14.4` release archive** (`ee-aarch64-apple-darwin.tar.xz`,
> `ee version --json` reports `gitCommit: null`) in a fresh isolated
> workspace, alongside the installed `0.14.2`. New or sharpened findings:
>
> - **Shipped search truth is wrong.** With the pinned Model2Vec model
>   present and `ee model status` reporting `neural_local`, 9/9 vectors
>   embedded, every `search`/`similar`/`recall` call emits high-severity
>   `embed_model_unavailable` ("semantic index metadata does not record a
>   vector dimension") even immediately after `ee index rebuild`; `ee orient`
>   reports `embed_backend: hash_fallback` while `ee search` reports
>   `neural_local`; `ee status` reports `search_index_degraded` +
>   `search_unavailable` + `graph_feature_disabled` (a skyline *config* flag,
>   on a build whose graph feature is on) while `ee index status` reports
>   `ready` with equal generations. §B's fix (`1898c975`) is on `main` only;
>   the release users install predates it by 77 commits.
> - **Concurrency non-determinism reproduced on `0.14.4`, not just `0.14.2`.**
>   Eight identical concurrent searches split 5 `cosine_similarity` +
>   `source_mode_fallback` (lexical arm unavailable) vs 3 `rrf_fused`, with
>   different result ordering; six concurrent read-only packs produced two
>   distinct hashes; four packs alongside one `remember` produced three hashes
>   plus `search_index_stale`. Serial runs are byte-stable (10/10 searches,
>   6/6 packs). On installed `0.14.2`, 5/8 concurrent searches return zero
>   results and 2/8 error `search_index`. Owners remain `.10`, `.2`, `.3`.
> - **`ee ask` abstains on a direct hit.** "Which command must run before
>   every release tag?" against a store containing "Run cargo fmt --check
>   before every release tag." returns `abstained: true`, 0 citations,
>   `ask_semantic_degraded`. This is an `.11`/`.12` usefulness failure, not
>   calibration theater.
> - **Latency is 20–50× the README table on a 9-memory store.** `search`,
>   `pack`, `status`, `capabilities`, `index status` each cost ~1.6–2.5 s
>   wall; `why` 0.35 s, `memory list` 0.27 s, `db status` 0.22 s. The fixed
>   cost sits on the index/model-lifecycle path (unchanged with
>   `EE_EMBED_MODEL_DIR=/nonexistent`), not DB open. `.6` must reproduce or
>   remove the table; nobody owns the root cause.
> - **Distribution claims are false or stale.** crates.io has no
>   `eidetic-engine` crate (HTTP 404) although README and CHANGELOG say it is
>   available since `0.14.3`; Homebrew serves `0.14.2`; the `v0.14.4` manifest
>   says `signed: false` for every asset (no Sigstore/SLSA); `ee version`
>   cannot attest a commit. All three GitHub workflows are `disabled_manually`
>   (CI since 2026-08-27; 105 commits since have no hosted verdict). The last
>   CI run that executed had 8 failing jobs, of which four are still code-side
>   at HEAD: yanked `bisync 0.3.0` in `Cargo.lock` (cargo-deny), unconditional
>   `rustix::fs::flock` in `tests/memory_drift_no_mock_e2e.rs:1353` (Windows
>   compile), the Windows `handoff create` envelope smoke, and `lib-core` /
>   `targets` shards exceeding the 90-minute runner cap. No full-suite tally
>   newer than 2026-08-09 (106 reds, `bd-1eeyw`) exists anywhere.
> - **Local reproducibility is gone on this Mac.** Every sibling checkout is
>   ahead of `franken-stack.lock` (asupersync sibling `0.4.10` vs `=0.4.9`),
>   so a `--locked` local build cannot resolve; the pinned RCH lane is the only
>   compile path (12/13 workers healthy).
> - **Tracker inertia.** 2 closes in the last 7 days after an 85-close week
>   on 2026-08-17; 31/44 `in_progress` beads untouched for >14 days; 118 `bv`
>   stale alerts; `bd-reality-core-convergence-1azkt` is 0/22 closed with
>   `.5`, `.10`, `.18` sitting unclaimed in `br ready`. Three THEME epics
>   (`bd-185zb`, `bd-29xee`, `bd-2yg7d`) have every dependency closed and can
>   close now. `bd-provenance-redaction-local-reads-xisfl` (P0) is parked
>   `status=blocked` with zero blockers.
> - **Static gates are green and prove presence, not behavior.** Closure lint
>   0 violations, verification drift 0, contract-drift radar 0, vision
>   coverage 138/138 (dispatch/file mapping). `bridge-staleness` still
>   recommends "planning Part III" against this active Part III.
> - **Docs still contradict code and each other.** README carries two
>   different Mesh limitation rows, calls `serve` both "reserved" and
>   "implemented", and dates its perf table to 2026-05-13.
>   `COMPREHENSIVE_PLAN.md` still says "design plan, pre-implementation",
>   `version = "0.1.0"`, `ee.response.v1`, workspace crates, and `cargo deny`.
>   `docs/plan-sweep-report.md` (2026-05-06) still labels §16/§17 stubbed.
>   AGENTS.md describes a version-bump-triggered release workflow; the real
>   workflow is tag-only and disabled, and releases are hand-cut.
>
> Items with no owning Bead after this pass: release the `1898c975` index
> truth fix to users; the crates.io/Homebrew claim correction; the four
> concrete CI reds; the `ask` direct-hit abstention; the ~1.5 s fixed
> per-command cost; the `graph_feature_disabled` config/feature misnomer;
> `scope_metadata_unavailable` silently degrading every search when the
> user-global store needs migration; the `COMPREHENSIVE_PLAN.md` /
> `plan-sweep-report.md` refresh; and the stale `in_progress` sweep. The
> existing epic still covers every §8 row; these are execution and
> sequencing gaps, so the recommended steering is to front-load one green,
> signed `0.14.5` cut from the pinned RCH lane over the verifier-first
> critical path, rather than opening a competing plan.

### Execution pass 2026-09-03 (`bd-reality-core-convergence-1azkt.23`–`.32`)

Ten children were filed from the 2026-09-02 addendum's unowned list and wired
into the existing epic; no competing plan or epic was created. What actually
landed, with the verdict that backs each claim:

| Bead | State | Evidence |
| --- | --- | --- |
| `.23` lexical arm lost under concurrent reads | **closed, proven** | Root cause: `TantivyIndex::open` builds an `IndexWriter` and takes Tantivy's exclusive `.tantivy-writer.lock`, so a second read-only process failed the open and `resolve_source_mode_with_tiers` degraded Hybrid to SemanticOnly. Frankensearch `715a29b3` adds `open_read_only` (writer is `Option`, typed `InvalidConfig` on writes); ee uses it and pins the commit. `cargo test --locked --test concurrent_search_lexical_arm_e2e` at `551f62da` → `remote_pass`, `1 passed; 0 failed`, 114.99 s: 8 simultaneous searches and 6 simultaneous read-only packs all agree on mode, order, and hash with no lexical-loss degradation. The frankensearch-side unit test remains UNRUN (no lane for that sibling). |
| `.24` per-search DB reopen | code landed, latency unproven | The bound-workspace-id open that only labelled a trace event is now behind `tracing::enabled!`. Acceptance asks for a fresh sample against a source-built binary, which this Mac cannot produce. |
| `.25` Model2Vec re-hash per process | code landed, tests unrun | Frankensearch `715a29b3` reuses the `.verified` receipt in `ModelArtifactManifestV1::verify_dir_cached`; three unit tests added. They are **UNRUN**: the sibling is a symlink outside RCH's canonical root and frankensearch's hosted CI is disabled. |
| `.26` posture path loads the model | not started | Implementation plan recorded on the bead (verified-not-loaded state on the existing lazy embedder). |
| `.27` ship 0.14.5 | blocked | Waits on `.23`/`.28` and the `.20` human authorization. |
| `.28` hosted-CI reds | 2 of 4 | Yanked `bisync` gone via vergen-gix 10.0.3 / gix 0.87.1 (every `bisync` release is yanked, so no in-place bump existed); the `rustix::fs::flock` helper and its two scenarios are `cfg(unix)`. Windows handoff smoke and the 90-minute shard cap remain. |
| `.29` docs factual errors | **closed** | crates.io, Homebrew, Sigstore, `serve`, the perf table, the duplicate Mesh row, AGENTS release process, plan headers. Contract-drift radar clean. |
| `.30` `ee ask` abstains on a direct hit | code landed + proven | Root cause was **not** the semantic arm (ask marks it degraded unconditionally): the lexical arm was pure Jaccard, which counts a span's answer terms against it. Now the mean of question coverage and Jaccard, with interrogatives as stopwords. `cargo test --locked --lib -- core::ask::tests` passed on the pinned lane. |
| `.31` mislabelled degraded codes | implemented, partly verified | `graph_skyline_disabled` (info) replaces the build-time `graph_feature_disabled` for the `[graph.feature.skyline] enabled=false` config case; `global_lane_migration_required` (info) replaces `scope_metadata_unavailable` when the optional user-global lane is skipped pending its own migration. Both ship a fixture, a taxonomy row, and a regenerated catalog doc. `failure_mode_fixtures_validate_catalog` ok and `degraded_codes_doc_coverage` 7 passed / 0 failed (including byte-identical doc generation). The status golden and insta snapshot edits are **not** execution-verified: the `golden` target timed out compiling and three snapshot reruns died in `bd-glivu` sync failures. |
| `.33` eval gate harness red since June | filed | `eval_run_happy_path` fails on a fixture-count drift (expects 9, there are 10 since `49fdd720`) and on `fx.async_migration.v1` evaluating zero queries. This is the harness behind "failing evaluations block releases", and it blocked using the public eval surface as the CLI-level proof for `.30`. |
| `.34` status skyline unit tests red | filed | Two `core::status::tests` skyline tests fail at HEAD; one expects a disabled degradation that never fires for a bare workspace, the other expects one Louvain community and gets zero. Attributed to pre-existing state by an invariance argument over the `.31` diff, since a baseline run at the parent commit returned `proof_broker_refused`. |
| `.32` tracker hygiene | **closed** | 3 dependency-complete epics closed; 11 shipped ScarletMill beads closed with commit evidence (self-verified, scripts not re-executed); 16 pane-orphaned beads returned to open/unassigned; the parked P0 unblocked. |

Two born-red defects were found by the verification lane itself and fixed in
passing: commit `971c72b2` had added `shannon_entropy_bits_per_byte`,
`looks_like_word_shaped_identifier`, `STANDALONE_HIGH_ENTROPY_MIN_BITS_PER_BYTE`,
and `detect_secret_like_matches` plus tests that call them, without importing
them into the test module, so **every `--all-targets` build of `main` was red
with E0425** — including the one this bridge's `.5`/`.19` want to make green.

Two more red gates were found the same way and filed rather than fixed (`.33`,
`.34`). Taken together with the E0425s, the pattern matters more than any one
defect: **three independent gates this bridge depends on were failing before
this session touched anything**, one of them since June, and each surfaced only
because a change was pushed through the verification lane rather than reasoned
about. Any earlier statement that `main` was green was measuring a tree that
could not compile its own tests.

A structural limit was also confirmed: the frankensearch sibling's unit tests
cannot be executed from this Mac at all. `cargo test -p frankensearch-lexical`
fails with `package ... cannot be tested because it requires dev-dependencies
and is not a member of the workspace`, ee consumes it as a path dependency, the
sibling sits outside RCH's canonical root, and its six hosted workflows are
`disabled_manually`. Sibling changes can therefore be compiled and proven only
through an ee-level test; their own `#[test]`s stay unrun until that CI is
re-enabled or someone runs the suite on Linux.

Verdicts on the pinned RCH lane at the exact committed tree: `cargo check
--locked --all-targets` **passed** (`remote_pass`, exit 0), `cargo test
--locked --lib -- core::ask::tests` **passed**, and `cargo test --locked --test
concurrent_search_lexical_arm_e2e` **passed** (`1 passed; 0 failed`, 114.99 s).

That last one took four attempts, all three failures environmental and none a
test result: twice `bd-glivu` (30 s `sync_to_remote` timeout against a cold
pinned-bundle transfer read off the ExFAT USB drive, on two different workers)
and once `rch_verify_franken_stack_materialization_failed` because a peer had
left an untracked build artifact in the frankensqlite sibling. Exporting the
pinned tree to internal disk with `RCH_VERIFY_COMMITTED_TREE_BASE` cleared it.
That env var is the practical workaround for `bd-glivu` on this Mac.

### Fresh-eyes review of the 2026-09-03 pass

Re-reading everything this pass wrote found twelve defects, several of them
introduced by the pass itself and invisible to every lane it had run:

- **Three compile breaks behind `bench-internals`** in frankensearch. Making
  the Tantivy writer optional broke two `let Self { .. }` patterns that list
  every field with no `..`, left `benchmark_join_writer` typed to a
  non-optional writer, and left the rearm path reconstructing `Self` without
  the new field. `cargo check --all-targets` in ee builds dependencies as
  libraries only, so **no verification in this bridge compiles those paths**.
- **A Windows dead-code hazard** in ee: `cfg(unix)`-gating the flock tests
  orphaned three helpers used only by them, which `-D warnings` would fail —
  the same platform whose compile error the pass had just fixed.
- **An invalid repair command**: the new `global_lane_migration_required`
  pointed at `ee migrate run --global`, a flag that does not exist. That is
  precisely the defect class the change set out to remove.
- **A wrong assertion in a new test**: a drifted download manifest correctly
  falls through to the full hash pass and *succeeds* on correct bytes; the
  no-borrow property is only observable while the file is unreadable.
- **A root-unsafe test**: `chmod 000` is a no-op for root, so the receipt test
  would have proved nothing in a typical CI container.
- **A vacuous-pass hole** in the concurrency E2E, which compared a field
  across processes without checking it existed.
- Plus a trace target that could silently drift from its guard, an
  undocumented precondition on the new scorer, two imprecise README claims
  (the perf probe ran on an Apple M4 host, *not* the `mac-m3-pro` class the
  table was measured on), a fragile message-literal coupling now bound by a
  constant and a test, and **eleven stale section counts** in
  `docs/degraded_code_taxonomy.md` that were wrong before this pass touched
  them, one by 26 rows.

Verification at the reviewed tree: `cargo check --locked --all-targets`,
`core::global_store::tests` (20 passed), `concurrent_search_lexical_arm_e2e`
(1 passed, 196 s), and `degraded_codes_doc_coverage` (7 passed) all
`remote_pass`. The frankensearch bench paths remain **unverified by
execution** — nothing on this Mac can compile them — so they are fixed by
inspection only.

The transferable lesson is narrow and worth keeping: *a green
`--all-targets` check in ee says nothing about a sibling's feature-gated or
test-only code, and cfg-gating a test silently changes what compiles on the
platform you were trying to fix.*

### Reality-check refresh — 2026-09-04

**Verdict: the local memory product is real, but complete recovery, predictable
retrieval under concurrency, demonstrated usefulness, and release readiness
are still partial or unproven.** Closing 4,177 of 4,327 tracker records (96.5%)
does not establish any of those properties. At this audit's starting snapshot,
150 records were nonclosed: 85 open, 21 in progress, 41 explicitly blocked,
and 3 deferred. Dependency-blocked and actionable counts use different
definitions; they must not be substituted for those status counts.

This refresh read all of `AGENTS.md`, `README.md`, `COMPREHENSIVE_PLAN.md`,
and the active bridge. It examined architectural decisions, mesh acceptance,
source paths, fixtures, verification scripts, and the active bridge's complete
task descriptions. Historical design documents remain design evidence, not
proof that their scenarios execute. This is a comprehensive product and
coverage assessment, not a claim to have executed every test or audited every
line of the optional subsystems.

#### Fresh evidence and its limits

The source snapshot is `ea25b367b16a22bfc47dcc9790075b97c43cf66f` on `main`.
Evidence is retained at `/private/tmp/ee-reality-20260904.wmzHtT/`; this local
directory is an audit attachment location, not the portable release capsule.
Older dated evidence below retains its original source and date.

| Check | Observation | What it establishes |
| --- | --- | --- |
| Public macOS ARM64 v0.14.4 archive | Downloaded from the release, SHA-256 matched its published checksum; `version --json` reports clean source `b0958e42cde8f8e2cedacf1f0a7c5804880ed9e9`, release profile, and `aarch64-apple-darwin` | Reproducible released-binary identity. The old `gitCommit: null` diagnosis applies to the installed 0.14.2 binary, not this archive. Checksums and self-reported identity are not signed build attestation. |
| Isolated offline core loop | `init`, `remember`, `search`, `pack`, `why` all exit zero with v2 envelopes; exact unique memory content/ID is retrieved and packed, and `why.selection.latestPackSelection` identifies the persisted pack and ledger | The serial manual-memory loop works in the released lexical fallback configuration. Semantic download was deliberately disabled; this does not prove neural quality or concurrent behavior. |
| Negative retrieval control | An unrelated unique query returns zero results | This small fixture rejects that distractor; it is not the general retrieval-quality evaluation. |
| Persisted replay | Public `pack replay` succeeds for the exact pack ID obtained from `why`, with matching ledger/pack hash | The serial released memory-ledger path is usable. Full snapshot and mixed-entity replay acceptance remains separate. |
| Public-CLI timings | Single observations: init 5.737 s, remember 1.065 s, search 0.913 s, pack 1.242 s, why 0.826 s, minimal status 0.895 s | End-to-end cost on this host/fixture. These are neither percentiles nor a comparison to the historical M3 corpus. Pack's inner `slo.actuals.elapsedMs=1` is not whole-command latency. |
| Rust AST placeholder scan | No `todo!` or `unimplemented!` invocations under `src/` | Narrow syntactic result only. Partial restore coverage and architectural substitutes exist without either macro. |
| Vision coverage script | `pass`, no missing documented surfaces | File/registration coverage, not 100% behavioral completion. |
| Strict contract drift radar | Four dependency-document violations; 43 docs scanned, 230 schemas loaded; 577 documented degraded codes match 577 fixtures | Asupersync 0.4.9 references lag the accepted 0.4.10 profile. Catalog consistency passes; executable behavior remains separate. |
| Closure linter | `pass`, zero new violations, 94 audit-baseline matches; automatic reopening disabled for the audit | The existing baseline is honored. This is not zero historical closure debt; no tracker status was changed by the audit. |
| Existing shell walking skeleton | Seven checks execute against the explicit v0.14.4 binary: six pass, `why` exits 3 on a `/var` symlink ancestor. The identical memory/database succeeds with its physical `/private/var` workspace path | Current script contains the v2 walking skeleton. Its trusted temporary-root normalization needs correction under `.5`/`.17`; do not weaken product descendant-symlink guards or relabel the original run green. |
| Bridge staleness script | Advises “consider planning Part III” against the active Part III plan and reads only old Part II labels | A real steering defect: completion-looking metadata produces obsolete advice. |
| Public release/workflows | Latest remains v0.14.4 (2026-08-29), 16 assets without Sigstore/SLSA files; all three workflows report `disabled_manually` | The current public distribution is not a verified green candidate for current source. No workflow was enabled or release published by this audit. |
| Current-source Cargo check | Both pinned RCH attempts fail syncing Asupersync after 30 seconds; the retry sets `RCH_SYNC_TIMEOUT_MS=180000` but the prescribed sidecar still reports 30000 ms. Local fallback is refused | Infrastructure failure, no compiler verdict. `check.json` and `check-retry.json` retain both attempts; `bd-glivu` owns the blocker. No current check, Clippy, or Rust-test pass is claimed. |

#### Post-public measurement — 2026-09-17 (`bd-reality-core-convergence-1azkt.21`)

The 2026-09-04 row above is historical. Live channels on 2026-09-17:

| Channel | Observation | What it does **not** establish |
| --- | --- | --- |
| GitHub release `v0.15.2` | Published 2026-09-12T18:48:32Z, not draft. 16 assets: six `ee-{target}.tar.xz` + `.sha256`, `install.sh`, `install.ps1`, `SHA256SUMS`, `ee-v0.15.2-manifest.json`. Manifest `status=success`, `source.git_sha=1478b2f3b0a912d5302808f6dc6de9e56e589c05`, lockfile 573 packages. Every artifact `signed: false`. | Signature/provenance. No `.sigstore.json`, SLSA, in-toto, or SBOM asset names. The Release workflow is still `disabled_manually`; this is a hand-cut publish. |
| crates.io `eidetic-engine` | Newest version 0.15.2. Binary name remains `ee`. | That current `main` equals the published crate. |
| Homebrew `Dicklesworthstone/tap` `Formula/ee.rb` | `version "0.15.2"`; bottle URLs are the GitHub `v0.15.2` archives. | Formula SHA-256 re-hash on this host. |
| Hosted CI | `CI`, `Release`, and `macOS EE Artifact` remain `disabled_manually`. New workflow `CI Static` (`ci-static.yml`, id 360498922) is active. Green run [35229903877](https://github.com/Dicklesworthstone/eidetic_engine_cli/actions/runs/35229903877) on `289da4c5d` (2026-09-17T13:52:51Z): forbidden-deps, migration-registry, closure-lint, vision-coverage, MCP self-test, contract-drift-radar, `cargo fmt --check` all success (`bd-o7wh0`). | Clippy, cargo-deny, or `cargo test --workspace --lib --bins --tests --examples`. A green CI Static run is not full CI restored. |

Do not close `.27` on this inventory: that bead still requires `.20` authorization and signed provenance. Do not treat unsigned `v0.15.2` as the Part III green candidate.

#### Correct the diagnosis before assigning more work

1. **Retrieval:** `.23` has already removed the read-only Tantivy writer-lock
   cause, with focused pinned-source proof. Keep that progress. The two renames
   in `publish_staged_index_inner` still move the active directory away before
   staging becomes active; rollback on ordinary errors does not prove abrupt
   crash safety or a coherent reader snapshot. `.2`/`.3` retain those obligations.
2. **Recovery:** `src/core/backup.rs` now contains aggregate CASS session/evidence
   serialization and transactional rehydration, safe source-path omission,
   workspace rebinding, exact-count coverage checks, and rollback tests.
   Task-episode recovery already has retained proof. CASS's current code/test
   presence is progress, not a fresh passing run in this audit. Many nonempty
   durable tables—including rules, packs, impressions/outcomes, curation, and
   journals—still have `not_implemented` recovery coverage. `.13`/`.14` stay open.
3. **Architecture:** private test-only BM25 and Frankensearch global retrieval
   are not missing implementation. Production pack PPR explicitly degrades;
   the local personalized algorithm remains in direct/exported graph paths.
   `.4` must finish the call-site boundary rather than reintroduce a local scorer.
4. **Proof:** the eval and skyline failure reports are dated, source-specific
   evidence (`.33`, `.34`). Current eval tests already enumerate ten families
   and explicitly expect the async-migration empty-failure golden. The missing
   positive query workload remains `.33`; a passing error golden is not
   retrieval-quality success. The old 106-failure suite inventory is historical;
   neither that number nor a historical green check describes current HEAD.
5. **Planning coverage:** `.27`, `.30`, `.31`, `.33`, and `.34` are not in
   `.22`'s transitive blocking closure at audit start. `.24`–`.26` and `.28`
   already are. Add the missing gates at their owning quality/status/release
   stages; do not mistake parent-child membership for blocking dependencies.

#### Bridge delta and execution order

The existing owners cover the 24 vision goals in §8 in prose. The unanswered
question is executable, stage-specific coverage: the original task graph can
close while newer user-visible failures remain, dependency tests outside the
EE crate can be omitted, and an advisory script can recommend an obsolete
next phase. Those are coverage gaps even though the broad verification epic
exists. Add bounded implementation/proof children and refine existing owners;
do not create another root plan or parallel release program.

1. Repair RCH source-transfer reliability under `bd-glivu`; preserve all
   failed attempts and never substitute local Cargo or an installed binary.
2. Establish a usable evaluation gate (`.33` → `.12`) and correct status
   regression proof (`.31`/`.34` → `.19`). Keep the direct-hit ask test in
   that evaluation path (`.30` → `.12`).
3. Implement and prove the remaining native rule loop, durable recovery,
   immutable publication, library boundaries, privacy, and Maintain under
   their existing owners. Each implementation has a distinct acceptance test
   or proof owner, with exact positive and forbidden-result assertions.
4. Make bridge advice consume the active phase and distinguish metadata
   coverage from behavioral evidence. Add tests that deliberately create
   completion-looking metadata with a missing or failed proof.
5. Verify the exact pinned sibling features and changed upstream tests, then
   complete `.19`'s functional candidate. Measure that exact candidate via
   `.6`, classify every advertised claim via `.9`, and privately stage via `.7`.
6. `.20` remains the publication authority boundary; `.27` is the concrete
   release/channel deliverable, `.21` audits it, and `.22` closes last.

#### Ambition round 1 — make useful context the measured outcome

The success boundary is the first usable, provenance-bearing context returned
to a fresh agent. Measure the entire process and the complete recommended
orientation sequence, alongside—not as a substitute for—inner pack/search
timers. A fast empty pack, incorrect fallback, or unusable repair is a failed
sample. `.6` owns cold/warm command and sequence latency; `.12` owns exact
usefulness and abstention. Record corpus size, model state, source mode,
candidate count, failures, and resource use with every sample. The current
one-memory observations establish a measurement recipe, not a percentile SLO.

Improve `.33` beyond changing 9 to 10: every registered fixture must resolve
its corpus, execute its exact expected query inventory, and retain positive
and negative assertions. Derive inventory from an authoritative manifest;
cross-check actual directories to catch an unregistered addition. Repair
`fx.async_migration.v1` before treating any new evaluation threshold as a
release gate. Put `.30`'s lexical direct hit and distractor through that same
public evaluator. This joins code correctness to the user's actual task.

#### Ambition round 2 — require recovery to preserve later behavior

Recovery acceptance is observational equivalence over the five jobs, in
addition to record equality. After side-path restore and derived rebuild,
the same admitted entities must search, pack, explain, accept permitted
feedback, and continue curation/maintenance with preserved identity and audit
history. Explicit workspace remapping, secret rekeying, host-path omission,
and intentionally rebuilt caches are the named differences; none permits
dropping durable business state. Compare both durable projections and those
public observations in `.14`.

The current CASS/task-episode mechanism is stored among `derived` artifacts
but contains required durable rows. `.13` must make this distinction visible:
the default `include_derived=false` must not yield a “complete” backup of a
store containing those rows. Test both flag settings, exact nonzero counts,
missing/duplicate references, aggregate truncation, transaction rollback, and
rebackup after portable source-path omission. Define partial versus complete
restoration explicitly; transport checksum success cannot imply recoverability.
Do not weaken live denied-evidence admission to make a restored fixture pass.

#### Ambition round 3 — make proof compositional across repositories

Model acceptance as a directed graph of required claims and their evidence.
A claim passes only when every required child is present, current for the
candidate, and passing. Parent membership, byte counts, comments, and green
unrelated tests do not satisfy a missing edge. An explicitly scoped
not-applicable decision differs from missing, skipped, or infrastructure-failed
evidence. This is a small typed composition rule, not another workflow engine.

Add a dedicated test owner for the exact pinned sibling feature matrix:
downstream EE tests do not automatically execute Frankensearch's own unit
tests or compile its benches/dev targets. The recently changed read-only
Tantivy and cached verification-receipt paths need their own positive and
tamper/forbidden-write negatives. Use exact lock revisions and declared
features, with no post-capture source changes. `.18` owns effective build
identity; `.17` owns inventory/aggregation; `.19` consumes both. These tests
can develop independently of a final green product and must not create a
`.19` dependency cycle.

#### Beads and refinement record

Phase 3a used the skill's frozen prompt unchanged, retained on the root Bead.
The baseline added `.35` (active-phase advice) and `.36` (independent false-
completion proof); after the three ambition rounds it added `.37` (executing
pinned sibling/API/feature proof) and refined `.6`, `.12`, `.13`, `.14`, `.18`,
and `.33`. No new root epic was created.

The frozen Phase 5 prompt is likewise retained verbatim on the root Bead and
applied to each pass:

| Pass | Review and resulting change |
| --- | --- |
| 1 | Separate current source, released binaries, and historical failures. Correct `.27`'s false null-provenance premise; retain completed CASS, lexical, backup, and upstream-integration progress. |
| 2 | Add 11 blocking edges for evaluation, status, release authorization/delivery, local provenance, and new proof owners. All current mandatory nonclosed bridge children become reachable from `.22`. |
| 3 | Remove implicit prerequisite ambiguity: `.35` can report unknown before the full runner exists; `.36` owns the independent test oracle before `.17` integrates it; `.37` consumes `.18` and feeds `.19`, never the reverse. Keep compile-only and executed matrix rows distinct. |
| 4 | Execute the existing shell test and inspect its failure. Assign trusted-root normalization to `.5`/`.17`; correct stale walking-skeleton and fixture-count diagnoses, and require real positive eval workloads under `.33`. Preserve baseline-matched closure debt and RCH failures explicitly. |
| 5 | Final review found no further planning changes: every active mandatory bridge child is reachable through blocking edges, new implementation/proof tasks have exact positive and negative acceptance, source/release evidence remains separated, and existing ownership/status is preserved. `br dep cycles` reports zero active cycles; bounded BV triage completes. |

An implementation dependency means “must complete before closure,” not “no
planning, test design, or diagnosis can start before the dependency closes.”
The new exact edges are `.36 → .35`, `.17 → .36`, `.37 → .18`,
`.19 → .37/.31/.34`, `.12 → .30/.33`, `.27 → .20`, `.21 → .27`, and
`.19 → bd-provenance-redaction-local-reads-xisfl`, where the left side depends
on the right. Existing paths already cover `.24`–`.26` and `.28`. The local
provenance owner preserves usable local evidence locators while retaining
the established redaction boundary for exported/agent-facing material.

Final validation: the tracker contains 4,330 records, including the three new
children; no pre-existing issue changed status. The closeout blocking closure
contains 76 records and omits no nonclosed mandatory bridge child. BV computed
ranking/triage but skipped its own cycle algorithm because the graph exceeds
2,000 nodes; the explicit `br dep cycles --json` check supplies that result
(zero active cycles, one archived closed-only cycle). Required new proof
tasks remain open; this audit neither executes them nor closes the product.
`git diff --check` passes. No Rust source was edited, and current-source
compiler/test verification remains infrastructure-blocked as recorded above.

### Current execution ledger (started 2026-09-01; evidence refreshed 2026-09-04)

This is the bounded working checklist requested by the operator. It is not a
second requirements source: §§9–15 and the Part III Beads remain authoritative.
Its consumer is the implementation session, it gates claims made from this work,
and it is retired into the dated evidence summary when every item is either
proved or returned to its owning Bead with an exact blocker.

#### A. Close the direct CASS evidence loop first (`.17`, ADR 0085)

- [x] Make `tests/no_mocks_e2e.rs` place workspaces under a canonical physical
  target root so the required RCH materialization cannot trip its own symlink
  safety policy before product behavior executes.
- [x] Add a focused harness regression proving a symlinked target-root alias is
  resolved only at the trusted test-artifact boundary and descendants remain
  ordinary physical paths.
- [x] Rerun the exact no-mock CASS import/search test on pinned current-HEAD RCH
  and require exact `SessionId`, exact `EvidenceId`, ready index health, equal
  generations, and clean retry publication.
- [x] Extend that same scenario—not a synthetic substitute—from exact evidence
  search into `ee pack`, requiring selection under native `EvidenceId`,
  `entityKind=evidence_span`, exact revision, `cass_evidence` trust, redacted
  session/line provenance, and no synthetic `MemoryId`.
- [x] Query the persisted pack through repository APIs and prove the selected
  evidence row, rank, section, revision, scores, explanation, trust, and
  provenance round-trip through `pack_evidence_items`.
- [x] Replay the persisted pack and require integrity-verified typed evidence;
  missing/malformed/hash-mismatched ledger paths must remain fail-closed.
- [x] Exercise typed `ee why` for the same `EvidenceId`, including storage,
  retrieval, selection, screening, redaction, session, and line provenance.
- [x] Grade the evidence selection with `ee outcome --pack --item`; prove the
  typed impression is recorded while immutable evidence and unrelated memory
  Bayesian confidence do not mutate.
- [x] Add at least one tempting denied evidence control and prove it remains
  absent from search admission, pack selection, persistence, replay, why, and
  feedback without leaking raw path/content.
- [x] Correct README CASS wording only after the executable behavior is green;
  one canonical explanation must distinguish direct safe evidence packing from
  optional curation into a durable learned memory.

Closure evidence (2026-09-01): commits `bb20a989`, `daca312e`, `c1935d82`,
`3f57b1b5`, `19d713d1`, `9f30f8df`, `f7daea02`, `0f74e46b`, `1e106539`, and
`876df57c` connect and adversarially exercise the native `EvidenceId` lifecycle.
The pinned current-HEAD RCH invocation
`cargo test --test no_mocks_e2e no_mocks_import_cass_fixture_sessions_stores_spans_and_searches --locked -- --exact --nocapture`
ran exactly one test and passed (`1 passed; 0 failed; 8 filtered out`) in
231.22 seconds after compilation. The scenario proves real CASS import and
retry, exact typed search, direct pack persistence, verified replay, typed
`why`, evidence-target outcome recording without evidence or unrelated-memory
mutation, and fail-closed exclusion of an admission-boundary denied control.
`no_mocks_log_dir_resolves_trusted_target_root_alias` covers the test-artifact
symlink boundary, and the README CASS section now states that live-admitted
evidence is directly searchable/packable while curation is an optional,
identity-changing promotion into durable learned memory.

#### B. Make index/status truth coherent (`.10`, `.16`)

- [x] Reproduce the installed `search/pack/status` degraded-vs-`index status`
  ready contradiction against an exact current-source binary and retained
  workspace; do not infer current behavior from the stale installed release.
- [x] Identify whether the mismatch is backend selection, index path,
  generation read, capability projection, or stale diagnostic aggregation.
- [x] Add one current-source public-CLI regression asserting that the same
  workspace snapshot cannot report mutually exclusive readiness postures.
- [x] Fix the narrow authority split and retain truthful degraded behavior for
  genuinely missing, stale, corrupt, lexical-only, and semantic-only states.
- [x] Verify `search`, `pack`, `status`, `index status`, and `doctor` share the
  same generation/backend evidence while preserving command-specific posture.

Closure evidence (2026-09-01): the contradictory installed probe resolves to
`/Users/jemanuel/.local/bin/ee`, an unattested `0.14.2` binary, while current
source is package `0.14.4` at `1898c975`. Current source has no remaining
authority split to patch: `index status` and `doctor` call `get_index_status`,
aggregate `status` reuses that classifier inside its pinned snapshot, and
`search`/`pack` consume the same snapshot-aware health report. Commit
`1898c975` adds the public-CLI regression
`ready_index_posture_is_coherent_across_public_cli_surfaces`. Its exact pinned
RCH invocation ran one test and passed (`1 passed; 0 failed; 5 filtered out`)
in 332.62 seconds, proving exact source/asset watermark equality plus coherent
ready posture and recall across all five surfaces in fresh processes. Existing
no-mock stale/corrupt recovery cases and the `IndexHealth` mapping continue to
preserve missing, stale, corrupt, lexical-only, and semantic-degraded truth.
The appropriate fix was therefore to retire a stale-release diagnosis and pin
the current shared authority with executable coverage, not introduce another
status layer.

#### C. Complete durable recovery inventory (`.13`, `.14`)

- [x] Enumerate every source-of-truth table by five-job owner and classify it as
  export/restore required, derived/rebuildable, secret/rekeyed, or intentionally
  ephemeral; reconcile the inventory against migrations rather than prose.
- [ ] Complete export/restore coverage and executed proof for rules, CASS sessions/evidence,
  packs and typed selected items, outcomes/impressions, curation lineage,
  durable jobs, and any other required source-of-truth rows found by inventory.
  - [ ] Prove the implemented recovery-only exact insert paths for CASS sessions and evidence;
    restore the new workspace binding while preserving IDs, hashes, admission
    posture, revisions, timestamps, and memory/session foreign keys.
  - [ ] Prove the implemented capture of CASS sessions/evidence in bounded aggregate artifacts rather
    than one filesystem entry per span, and prove artifact row counts match the
    same-snapshot inventory before claiming either table covered.
  - [ ] Prove the implemented omission of host-private session source paths from portable artifacts while
    retaining safe upstream identity and explicit restore semantics.
  - [ ] Prove the implemented restoration of sessions before evidence after memory import, report both row
    counts, and fail closed on malformed schema, ambiguous workspace mapping,
    missing session/memory references, or duplicate identities.
  - [ ] Round-trip one admitted and one denied evidence row and prove restore
    preserves both exact stored posture and live fail-closed admission behavior.
- [ ] Preserve IDs, revisions, foreign keys, audit ordering, redaction posture,
  and pack-ledger integrity without exporting host-private or key material.
- [ ] Prove evolved-store backup → verify → restore → migrate → rebuild → query
  with manual memory, rule, evidence, pack, outcome, and curation records.
- [ ] Add negative cases for partial archive, tampered hash, wrong workspace,
  incompatible schema, missing side path, symlink traversal, and interrupted
  restore; every failure must leave the destination recoverable.

Inventory closure evidence (2026-09-01): commits `7448e76f`, `17668658`,
`c9b6faff`, and `3ca07dc5` add a live migrated-table recovery inventory to
backup create/export/manifest/verify/restore. Every table has an explicit
five-job owner, disposition, coverage posture, and snapshot row count; unknown
migration drift is high severity, absent typed schema coverage is explicit,
and any nonempty required-but-uncovered table makes the artifact `partial`
with `incomplete_source_coverage`. Exported rows and inventory counts are read
inside the same database snapshot, so the claim cannot race the archive it
describes. Commit `4fc459d1` documents the integrity-versus-coverage boundary
and adds the three required failure-mode fixtures. The exact pinned current-
source RCH invocation
`cargo test --lib recovery_inventory --locked -- --nocapture` passed both
tests (`2 passed; 0 failed; 9,294 filtered out`) in 373.54 seconds after
compilation. This closes inventory and false-completeness detection only;
typed CASS, pack, outcome, curation, durable-job, and other required restore
coverage—and lossless identity/ledger proof—remain open in the four unchecked
rows above.

Task-episode recovery closure (2026-09-02): commit `248aaf87` closes one
previously stranded durable row family. Backup collection now enumerates the
complete workspace episode table rather than truncating at 256 rows, verified
derived episode artifacts are rehydrated into the isolated side-path database,
and restore preserves episode IDs, workspace binding, references, actions,
outcome data, hashes, and the original `created_at`. The recovery inventory
only marks `task_episodes` covered when derived capture is enabled and the
captured artifact count exactly matches the same-snapshot table count; otherwise
the backup remains honestly `partial`. The restore envelope now reports
`counts.taskEpisodesRestored`. Commits `b9cdefbd` and `c421df6b` correct the
fixtures to the database's canonical episode/session ID contracts;
`f53db53d` makes the system test use physical output and side paths without
weakening the production symlink guard. To separate restore logic from the
expensive whole-backup path, `b8f79f6f` adds a focused production-path
serializer/rehydrator round trip. Its exact pinned RCH invocation passed one
test in 185.92 seconds. The original full
`restore_backup_to_side_path_materializes_derived_assets` test then passed on
the same exact commit in 193.47 seconds after compilation, proving backup
creation, verification, isolated import, derived copy, task-episode restore,
workspace remap, and exact preservation of every other episode field. The
verifier used an external committed-tree/tmp cache after local proof staging
had twice hit `ENOSPC`; one cold attempt timed out during final-crate
compilation, while the subsequent exact runs completed remotely. This closes
task-episode recovery only; the broader four recovery rows remain open.

#### D. Restore mandated dependency boundaries (`.4`, `.15`, `.18`)

- [ ] Replace pack-path local personalized PageRank with the required
  FrankenNetworkX projection/API or obtain an explicit upstream capability;
  preserve deterministic ordering and cancellation semantics.
- [x] Remove the public custom BM25 production surface once all real callers use
  Frankensearch; retain only legitimate differential test code if still useful.
- [x] Replace global-store token-overlap/substr ranking with a rebuildable
  Frankensearch lexical index, including immediate promotion/demotion lifecycle
  updates and positive/negative behavior coverage.
- [x] Delegate configured hybrid fusion weights, diagnostic RRF, and shadow
  candidate fusion to Frankensearch's weighted RRF implementation; retain only
  EE-specific orchestration and explanation projections.
- [x] Classify remaining score-changing local paths and eliminate those that
  duplicate Frankensearch responsibility; retain only the documented
  index-failure lexical fallback, policy enforcement, pack-owned hints, and the
  separately tracked PPR violation.
- [x] Disable production pack PPR score influence while the pinned
  FrankenNetworkX release lacks deterministic personalization: preserve textual
  ranking, retain non-PPR Pack DNA signals, emit one typed
  `graph_ppr_upstream_unavailable` degradation, and catalog the code. Focused
  runtime verification is still blocked by local source-staging disk pressure.
- [ ] Pin and expose one coherent dependency/version identity across manifest,
  lock, runtime status, proof capsule, and release artifacts.
  - [x] Align the Frankensearch index-manifest/runtime constant with the
    `0.4.0` dependency and retain the existing executable manifest-parity
    contract.
  - [x] Align linked franken-stack versions and default feature posture across
    `Cargo.toml`, `Cargo.lock`, runtime dependency diagnostics, the canonical
    dependency-matrix golden, its Markdown contract, and the install audit's
    crates.io resolution inventory. Lock-backed runtime parity and
    golden-to-runtime parity tests now fail on renewed drift.
  - [ ] Refresh the older command-output dependency/doctor goldens from a
    current source-built binary, bind the proof capsule to the same matrix
    revision, and carry that identity through staged release artifacts.

BM25 boundary closure evidence (2026-09-01): repository-wide call-site search
found no caller of `search::bm25_simd`; production lexical indexing and
retrieval already use Frankensearch. Commit `58fcb914` therefore makes the
fixed-point scorer a private `#[cfg(test)]` differential module instead of a
public production API, retaining its useful scalar/chunked parity and numeric
edge-case coverage without shipping a second BM25 implementation. The exact
pinned current-source RCH invocation
`cargo test --lib search::bm25_simd::tests:: --locked -- --nocapture` passed
all seven tests (`7 passed; 0 failed; 9,289 filtered out`). This does not close
the neighboring PPR, remaining local retrieval-boundary, or
dependency-identity rows.

Frankensearch boundary progress (2026-09-02): commit `95f89c95` replaces the
user-global substring/token-overlap scorer with a dedicated Frankensearch
lexical index and wires promotion plus demotion into its write lifecycle. The
exact committed-HEAD promotion test passed through pinned RCH proof reuse, and
the global promotion schema contract executed two tests successfully. Commit
`1632b108` then removes EE's configured post-retrieval score multiplier, local
diagnostic RRF formula, and shadow-tuner RRF reconstruction. Live hybrid search
passes normalized lexical/semantic weights into
`TwoTierSearcher::with_rrf_weights`; diagnostics and shadow evaluation call
upstream `frankensearch::rrf_fuse`. The pinned `cargo test --lib fusion` proof
completed remotely with exit 0 on exact commit `1632b108`; proof reuse retained
the verdict but not individual test-count stdout. PPR and any still-unclassified
local score-changing paths remain open, so the hard dependency boundary is not
declared fully closed.

Remaining-path classification (2026-09-02): repository-wide call-site and
assignment searches found no second live BM25, vector, fusion, or RRF
implementation after the two commits above. `context::lexical_memory_fallback`
is invoked only for `IndexError`/`IndexNotFound`, emits
`context_lexical_fallback`, and is the documented degraded lexical path allowed
by the walking-skeleton acceptance gate. Mesh trust adjustment is a policy
boundary; memory-tier, changed-symbol, query-file graph, and related boosts are
pack-candidate selection hints after retrieval. `src/search/scoring.rs` exposes
a historical composite multiplier model only to unit/integration
monotonicity tests and has no production consumer; its module documentation now
says so rather than implying live wiring. This closes the duplicate-search
classification row, not the independent local PPR row or the broader score-
calibration work.

Pack-PPR boundary progress (2026-09-02): the pinned FrankenNetworkX Rust API
supports ordinary weighted PageRank but does not expose personalization; its
Python adapter explicitly rejects `personalization`, `nstart`, and custom
dangling vectors. Commit `26a7c4b8` wires the production pack rerank path to the
already-defined honest fallback instead of ee's local ACL-push reference
implementation. A nonzero requested weight with the PPR feature enabled now
preserves textual selection, emits exactly one medium-severity
`graph_ppr_upstream_unavailable`, creates no PPR score explanation/cache/witness,
and leaves non-PPR Pack DNA active; zero weight remains a silent no-op. The new
failure fixture is indexed in the generated degraded-code catalog. Rustfmt,
fixture JSON validation, generated-doc regeneration/check, and static diff
checks pass. The focused committed-source RCH test has not run because the
post-backup rerun exhausted local source/proof staging first. Direct
link-suggestion and exported/reference PPR paths still require upstream
delegation or explicit degradation, so the broader `.4` stack-boundary row
remains open even though `.15`'s production pack influence is implemented.

Dependency-identity progress evidence (2026-09-01): the search runtime had
continued writing Frankensearch `0.3.0` into new index manifests after the
workspace advanced to `0.4.0`; `ee doctor --franken-health` also called graph
feature-gated despite `graph` being default-on and reported obsolete versions
for every linked franken-stack family. Commits `a51ae376`, `1b918bef`,
`5e3edeb9`, and `c8ea16b2` align and guard these surfaces. The runtime matrix
now reports the locked versions (`asupersync 0.4.9`, FrankenSQLite `0.3.11`,
SQLModel `0.4.1`,
Frankensearch `0.4.0`, FrankenNetworkX `0.2.0`, Tru `0.2.4`, and agent detection
`0.2.2`) and treats the graph family as ready in the default profile. The
remaining identity row stays open because historical command-output goldens,
the current proof capsule, and release artifacts have not yet been regenerated
and attested together. The exact focused RCH Frankensearch contract attempt did
not execute: source sync to worker `worker-h` timed out after 30 seconds and the
required remote lane correctly refused local fallback. A concurrent exact-HEAD
all-target check reached `eidetic-engine` compilation but the remote SSH command
timed out at 900 seconds; neither infrastructure result is claimed as a green
test.

#### E. Turn component success into a releasable product (`.1`–`.7`, `.19`)

- [ ] Prove fresh-process serial determinism for stable envelopes, ordered IDs
  and scores, omissions, typed entities, provenance, and pack hash.
- [ ] Prove concurrent publication/read linearizability and bounded recovery
  across process races, cancellation, stale generations, and backend fallback.
- [ ] Finish score-calibration semantics for lexical, semantic, hybrid, reranked,
  mixed-kind, singleton, and degenerate pools; downstream admission/ask/quality
  must consume calibration identity or explicit unknown posture.
- [ ] Make the canonical readiness manifest run the exact behavioral inventory
  once, reject zero/ignored/filtered/duplicated/skipped required tests, and bind
  all evidence to source, dependency bundle, toolchain, target, and binary hash.
- [ ] Re-enable hosted CI only when it invokes that same manifest, then produce
  one immutable current-SHA green candidate capsule.
- [ ] Stage native release artifacts privately, verify install/smoke/rollback,
  checksums, signatures, and SLSA/Sigstore provenance, and require explicit human
  authorization before publication.
- [ ] Replace historical performance prose with reproducible current-source raw
  samples, host fingerprint, baseline identity, countermetrics, and enforced
  regression budgets—or demote the claims until that evidence exists.

---

## 0. Premise

The 2026-08-17 mesh-campaign reality check found an inverted tracker, not an
unbuilt product:

- Unix live EE-to-EE works on `main`: create/invite/join, inbound listen,
  `TcpMeshForegroundSyncTransport` EventFetch + grant-gated BodyFetch,
  hydrate, `--memory-scope team` search/pack, `teamProvenance`, P4.4/P4.5,
  US-5 last-sync/reachability.
- README and `docs/mesh/real_tailscale_smoke.md` still said the production
  supervisor used a no-op transport. That claim is false as of this Part III
  honesty edit.
- Beads still showed ~52 open `bd-tc-epic-qzk7o.*` children. Most of those
  slices are shipped. Open-count was being misread as "not built."
- Two-human Tailscale, Windows-host soak, production IdP vendor soak, T2.7
  frame/session fuzz beyond origin properties, and the T5.7 publication fence
  were the **real remainders** at Part III opening. All now have evidence or an
  allowed product decision, including the 2026-09-17 two-host amendment (delegated authority, `.8`).
  None is an excuse to rebuild transport.

**Non-negotiables for Part III:**

- Do not rebuild shipped Unix team-confed.
- Do not steal `bd-d67os.28` (NavyLotus; T5.7 fence).
- Do not start `bd-1nl13`.
- Do not invent a T6.7 ceremony. `.7.7` waits for the remainder children.
- Do not close the epic until the remainder children close.
- ADR 0086 Context stays historical (2026-07-30). Correct the plan and
  README, not the ADR's original problem statement.
- No file deletion. No worktrees. No local Cargo on this Mac.

---

## 1. What is already true

Unix product on `main` (proof ledger:
`docs/mesh/verification_matrix.md`):

| Surface | State |
| --- | --- |
| `ee team create` / `invite` / `join` | Live signed TCP; join first-sync imports origin genesis; invite `--wait` waits for it |
| Inbound listen | `ee mesh hello-responder run` / `ee daemon --foreground`; Tailscale LocalAPI or loopback `TeamJoinLocalApi` |
| Foreground sync | `TcpMeshForegroundSyncTransport` — not `Noop` |
| Unified recall | Authorized BodyFetch hydrates stubs; search/pack/ask/why carry `teamProvenance` |
| Conflicts / insights / why | P4.4 precedence, T5.6 `peerConflicts`, P4.2 elevation, T5.8 origin-time invariance |
| Status | US-5 `lastSeenAt` + reachability (`self` / `never_synced` / `synced` / `soft_stale` / `hard_stale`) |
| Fake IdP + identity_attest | T7.1–T7.6 proven against the fake harness |
| Windows inbound compile | `x86_64-pc-windows-gnu --lib` compiles; TeamJoin TCP is not Unix-gated |

---

## 2. Original Part III remainder ledger — current disposition

| Gap | Bead | Current disposition |
| --- | --- | --- |
| Original two-human Tailscale criterion; US-4 search/pack, cursor advance, no deferred sync | `bd-tc-epic-qzk7o.3.8` (T2.6) | **Amended 2026-09-17:** the user in WildBluff's session approved “Accept two-host boundary” (Branch B, `bd-reality-core-convergence-1azkt.8`). Two independent tailnet hosts are the v1 boundary because the retained artifact exercises cross-host exchange and team-scoped recall. It remains two-host evidence, not two-human proof. Independent-operator usability is unproven; no two-human soak is promised or assigned. Authentication/privacy requirements and broader §15 criteria are unchanged. |
| Frame/session/bootstrap fuzz beyond `tests/property_origin_stream.rs` | `bd-tc-epic-qzk7o.3.9` (T2.7) | Closed with frame/session/bootstrap properties and fuzz, MAC-before-counter proof, RCH host/duration, and commit `acc230aa`. |
| Source-snapshot publication fence | `bd-d67os.28` then `.6.7` | Both closed; `.6.7` records coalesced intake plus the source-snapshot publication fence. |
| Windows-host DACL / inbound crash / owner-only key-path | `bd-tc-epic-qzk7o.12` + `.2.4` | Closed with a retained Windows-host DACL and crash/restart artifact. Current cross-platform CI remains a separate release-readiness concern. |
| Production Entra / Okta / Google IdP soak | `bd-tc-epic-qzk7o.8.8` | Closed under §5's allowed explicit decision: fake IdP is the v1 ceiling; a vendor soak is post-v1 unless that decision changes. |
| Program closeout | `bd-tc-epic-qzk7o.7.7` (T6.7) | Closed, as are the milestone parents and root epic. The `.3.8` wording mismatch was resolved 2026-09-17 by the approved two-host scope amendment (see ledger row above). |

---

## 3. Tracker policy for this bridge

1. Close a shipped `bd-tc-epic-qzk7o.*` child only with verification-matrix
   evidence (test name + isolated host + duration + commit). No abstention
   close. No "docs-only" close of an implements-surface bead.
2. Split environment remainders into explicit children instead of leaving
   fifty implementation beads open.
3. Historical instruction: keep `.3.8`, `.3.9`, `.2.4`, `.6.7`, `.7.7`,
   `.12`, `.8.8`, affected milestone parents, and the epic open until their
   proof rows were resolved. Those records are now closed; do not pretend that
   tracker state by itself amended `.3.8`'s written two-human acceptance.
4. Unblock `.2.4` from "blocked" once T5.9's body-approval consumer is on
   `main`. Remaining work is Windows key-path, not missing Unix crypto.
5. Comment `.6.7` that protocol tests passed and the fence stays
   `bd-d67os.28`.
6. After README honesty lands, close `.2.5` (T1.7). That bead existed to
   stop README from lying about mesh.

---

## 4. Docs honesty landed in this Part III opening

- `README.md` Mesh / Team / Limitations / FAQ now describe live Unix
  `TcpMeshForegroundSyncTransport` and name the remainders.
- `docs/mesh/real_tailscale_smoke.md` no longer claims a no-op transport.
- `docs/mesh/operator_onboarding.md` points at `ee team` and the ledger.
- `docs/mesh/verification_matrix.md` has an explicit remainder table.
- `docs/mesh/team_confederation_plan.md` status line matches `main`.
- ADR 0086 historical Context is **not** rewritten.

---

## 5. Original mesh close criteria, extended by the full closeout in §15

These were the mesh-only criteria at Part III opening. All rows now have an
evidence-backed disposition or approved amendment. They remain historical
inputs to §15 rather than a second, competing close gate.

Archive this file to `docs/archive/close_the_gap_2026-08.md` and start Part IV
**in this same path** only when these criteria and §15 are both satisfied:

- `.3.8`: the 2026-09-17 amendment (delegated authority, `.8`) accepts the retained two-independent-host Tailscale artifact. Independent-operator usability remains unproven; no two-human soak is promised or assigned.
- `.12` has a Windows-host soak artifact (or an explicit fail-closed
  product decision recorded in the matrix).
- `.8.8` has a production IdP soak artifact (or an explicit "fake-IdP is
  the v1 ceiling" decision).
- `.3.9` either grows the remaining fuzz or is narrowed and closed with
  the origin-slice evidence plus a filed follow-up.
- `bd-d67os.28` closes and `.6.7` reuses the fence (or `.6.7` is rewritten
  as honesty-only with a new implements-surface sibling).
- `.7.7` can then write the T6.7 rollup without inventing ceremony.
- README / smoke / matrix still match the code.

Until then, this file stays at the repo root.

---

## 6. 2026-08-23 full-project reality-check verdict

`ee` is **real, broad, and architecturally recognizable**, but it is **not
finished, not currently verified, and not yet delivering every controlling
promise end to end**.

This is not a stub-shell diagnosis. The repository contains real FrankenSQLite/
SQLModel persistence, Frankensearch integration, Asupersync runtime wiring,
FrankenNetworkX projections, stable response envelopes, provenance-rich pack
rendering, explicit degradation, audited curation/maintenance machinery, and a
large CLI/test surface. The forbidden dependency names do not appear in the
current `Cargo.lock`.

The decisive failures are at integration and proof boundaries:

1. CASS transcript spans are persisted, projected, and can enter a pack through
   a first-class positively admitted `PackEvidenceItem` path. The public
   import-to-index-to-pack proof, full job/revision coverage, README contract,
   and generic ADR 0085 entity model have not converged.
2. Applied procedural rules are persisted and partially projected, but corpus/
   reembed accounting and native rule-item hydration remain incomplete.
3. Incremental evidence/linkage jobs can stamp a derived index whose actual
   document set is absent or stale.
4. Identical concurrent search and read-only pack calls can observe different
   embedding backends, index availability, selected memories, and pack hashes.
5. The named North Star and vision gates prove file/dispatch presence more
   readily than the exact public behavior they claim.
6. Exact-HEAD pinned RCH check and strict Clippy now pass, but the proof has
   admission/source-state degradations, focused and full behavioral manifests
   remain incomplete, and hosted CI is manually disabled; no immutable current
   SHA has the required complete green readiness proof.
7. Release automation is tag-only, but its dependency/tool inputs are not yet
   hermetic and the latest release lacks the signed/provenance asset set the
   checked-in workflow claims to produce.
8. README performance numbers are historical and not reproducibly tied to the
   current baseline file, raw samples, host fingerprint, or a release-blocking
   gate.
9. Production pack ranking now explicitly degrades requested PPR influence
   instead of invoking the local ACL-push implementation while FrankenNetworkX
   lacks personalization. Direct link-suggestion and exported/reference PPR
   paths still use local code. The former public custom-BM25 residue is
   test-only; global-store recall and weighted fusion/diagnostic RRF delegate to
   Frankensearch. Release/proof dependency identity is not yet attested end to
   end.
10. The former lexical raw-BM25 clamp is fixed in current source by normalizing
    the complete pure-lexical pool and retaining raw `lexicalScore`. The public
    score contract is still query-relative without a proved calibration identity,
    and downstream floor, pack-quality, and ask-confidence semantics have not
    been shown to interpret that value truthfully across degenerate or mixed
    retrieval cases.
11. Backup now accepts legitimate targetless audit rows, uses one read snapshot,
    avoids creating authentication-key state during dry-run, and has a complete
    typed capture/restore path for task episodes. Export still omits rules, CASS
    sessions/evidence, outcomes, packs, curation lineage, and durable jobs. The
    documented recovery surface therefore remains materially incomplete.
12. CASS pack admission now implements the evidence-specific core of ADR 0085:
    a positively admitted safe evidence span is first-class and unsafe or
    unclassified spans fail closed. The general typed-entity contract, native
    rule entities, replay/outcome migration, schema version decision, public
    E2E, and README wording have not converged.
13. Effective build inputs are not hermetic: sibling trees and semantic
    checkout-time patches are incompletely represented in provenance, release
    tooling has mutable inputs, and release Cargo builds omit `--locked`.

The honest summary is therefore: **the architecture and many component
surfaces work; useful retrieval, recoverable durable memory, the five-job
product loop, and release-readiness do not yet work reliably as one cohesive
product.**

---

## 7. Evidence snapshot and authority boundary

Source base under the latest audit: `main` at
`c716ae31ecdc2628fc80f609aa453222eddd6787`
(`v0.14.4-26-gc716ae31`). The committed source tree was audited independently
from the installed binary. Pre-existing untracked tracker journals, `.ci/`, and
a Cargo manifest backup were left untouched; this refresh changes only this
plan and additive comments in the existing Part III Beads. Static claims below
are source/contract findings, not a green-candidate proof.

Operational probes used `/Users/jemanuel/.local/bin/ee` version `0.14.2`,
SHA-256
`d7e50bc8831c29437fdc23bf6ff6e57e1b2131665a01c8af937dea02323857f5`.
Its `ee version --json` reports `gitCommit: null`, `gitTag: null`, and
`targetTriple: unknown`. Therefore the live runtime results are
**released-binary evidence against the current workspace**, not proof that
commit `c716ae31` behaves identically. Source inspection contains a plausible
matching race. Bead `.10` must reproduce or refute it with a source-attested
candidate before `.2` changes the implementation.

### 7.1 Positive evidence

- The installed release binary emits `ee.response.v2` for ordinary status, capability,
  search, pack, ask, and diagnostic probes.
- `ee pack --read-only` emits item-level provenance, trust, relevance/utility,
  selection explanation, lifecycle, redaction, and degradation data.
- Offline/hash fallback is explicit rather than falsely labeled semantic.
- `ee ask` abstains when evidence confidence is inadequate.
- The store/index/status surfaces disclose stale generations and document
  counts rather than silently claiming ready.
- `Cargo.lock` contains none of `tokio`, `tokio-util`, `async-std`, `smol`,
  `rusqlite`, `sqlx`, `diesel`, `sea-orm`, `petgraph`, `hyper`, `axum`,
  `tower`, or `reqwest`.
- Core command dispatch and durable use-case implementations are real rather
  than TODO/unimplemented macros.
- Current source implements live-admitted direct CASS pack entities with stable
  `EvidenceId`, revision hash, session/span provenance, trust class, and durable
  `pack_evidence_items` persistence. This is real partial ADR 0085 delivery,
  not yet a proof that the complete CASS loop works.
- Current backup source uses a consistent DB read snapshot, accepts nullable
  audit targets, and does not create store-auth state during dry-run; focused
  inline tests cover both repaired cases.
- The active Beads dependency graph has no active cycle.
- `closure-lint --audit --json` reports no formal violation, while this plan
  explicitly records what that linter does not prove.

### 7.2 Negative evidence

- In the installed release binary, eight identical concurrent search calls
  split between two `search_index` errors, three hash-fallback/no-result
  responses, two neural one-result responses, and one neural four-result
  response.
- In the installed release binary, six identical concurrent read-only pack
  calls produced four empty packs, one two-item pack, and one three-item pack
  with three distinct hashes.
- Current source still exposes a matching two-rename publication hole and
  process-local model admission; `.10` owns source-attested attribution.
- Historical installed-binary probes rendered distinct raw BM25 values as
  `relevanceScore: 1.0`; current source instead min-max normalizes one complete
  pure-lexical pool through Frankensearch and retains raw `lexicalScore`.
  Public downstream calibration, degenerate-pool semantics, and cross-query
  non-comparability remain unproven under `.11` and `.12`.
- The installed `0.14.2` backup dry-run failed on a nullable audit target, but
  current source fixes that failure and the dry-run key mutation. The remaining
  source defect is broader: the export inventory omits durable five-job state
  and restore cannot yet prove lossless behavior across it.
- The installed release reports source rows that do not become documents: 147
  evidence spans but zero indexed evidence documents and zero rule documents.
- A live Asupersync migration query returned either no result or unrelated RCH,
  stale-binary, and tracker-process memories instead of the required runtime
  rules. A release-preparation pack likewise lacked the promised complete
  project release context.
- `scripts/vision-coverage.sh --json` reports 137/137 implemented and zero
  gaps, but its mapping is based mainly on registered surfaces/file presence;
  `bd-2mpct.1` already records this proof weakness.
- The historical plan sweep labels North Star coverage verified by checking
  that two test files exist, while its own narrative says most scenarios were
  only partial.
- The verifier's basic `e2e_test.sh` header claims the walking skeleton, but
  its actual scenarios omit init/remember/search/pack/why and still assert the
  end-of-life `ee.response.v1` schema.
- Pack candidate relevance is changed by the local `src/graph/ppr.rs`
  algorithm rather than the mandated FrankenNetworkX graph layer. The former
  public `src/search/bm25_simd.rs` surface is now test-only; global-store recall
  and weighted fusion/diagnostic RRF now use Frankensearch. Remaining local
  score-changing paths still require exhaustive classification.
- The pinned current-SHA RCH `cargo check --locked --all-targets` now passes the
  exact committed tree and pinned Franken stack. Its proof is degraded by
  unavailable local build admission and proof-broker source-state mismatch.
  Strict `cargo clippy --locked --all-targets -- -D warnings` also passes; the
  full suite, exact North Stars, and candidate capsule still require independent
  verdicts. `contract-drift-radar --json` now passes.
- Hosted `CI` / `Release` / `macOS EE Artifact` remain `disabled_manually`.
  `CI Static` is a cargo-free hosted gate with a green run on 2026-09-17
  (`bd-o7wh0`, 35229903877). That is not clippy or the full `--tests` suite.
  Public latest is `v0.15.2` (2026-09-12): six native archives + checksums +
  installers + manifest, still no Sigstore/SLSA; every manifest artifact
  `signed: false`. crates.io and Homebrew both serve 0.15.2. Unsigned
  publication does not close `.27`.

---

## 8. Vision checklist and current status

| # | Controlling promise | Status | Evidence / gap owner |
| ---: | --- | --- | --- |
| 1 | Local-first single CLI; core commands need no daemon | **WORKING** | Real direct CLI paths and source-backed storage exist. |
| 2 | Franken-stack foundations; no forbidden substitute dependencies or core algorithms | **PARTIAL / WRONG-APPROACH** | Static dependency scan is clean; custom BM25 is test-only and global recall plus weighted fusion/diagnostics use Frankensearch. Production pack PPR now degrades explicitly, but direct/exported local PPR and release/proof identity remain open. `.4`, `.18`; `.15` awaits focused runtime proof. |
| 3 | Manual memory → DB → search → pack → why | **WORKING (bounded release proof)** | Fresh v0.14.4 isolated serial offline probe returns the exact memory, pack provenance, and persisted ledger through why. Current-source and concurrent acceptance remain `.10`, `.2`/`.3`. |
| 4 | CASS import makes permitted prior incident content searchable and safely packable | **WORKING on the oracle corpus; first-run onboarding still fails** | 2026-10-22 (§17.2): on the `.43` oracle, v0.18.0 imports 308 sessions / 4,009 spans at 0.60 s per session, admits 988 and quarantines 3,021 by kind, and search/pack return **projected** `assistant:` / `user:` text at **UTR 1.0000**. The `[cass] binary` opt-in is live at all three former dead sites and executes. Remaining: a standard `~/.local/bin/cass` is still refused by default so README Quick Start step 2 exits 5 (the refusal is now actionable and names both opt-ins) — `.44`; and **N1**, where a CASS-populated store can return zero results under the default download policy — `.66`. Measured against the emulator, not real transcripts: this host has no cass index. |
| 5 | Hybrid BM25 + neural-local retrieval by default | **WORKING at 4k spans; one default-policy path returns nothing (N1)** | 2026-10-22: at 4,009 spans search is 0.83–1.19 s and pack 1.14 s (were 11.1 s / 8.7 s), with `degraded: []` under a coherent neural index. The scale cliff is gone. But under the **default** model-download policy the same corpus answers every query with 0 results and `index_incompatible` while three health surfaces report healthy (§17.3 N1) — `.66`. Whole-process concurrency still `.2`, `.3`. |
| 6 | Same declared snapshot gives byte-stable canonical JSON and pack hash | **WORKING (bounded, released binary)** | 2026-10-01: v0.16.0 serial packs are byte-stable; 6/6 concurrent packs equal the serial hash; packs and searches racing a writer report one coherent stale-flagged snapshot (§16.2). The 100-run, linearizability and crash matrix remain `.3`. ADR 0087 identity slices remain `bd-pack-identity-*`. |
| 7 | Retrieval scores and pack quality mean what their names claim | **PARTIAL / UNPROVEN** | Pure lexical pools now use Frankensearch min-max normalization and retain raw BM25, but the query-relative/calibration contract and every downstream admission/quality consumer remain unproven. `.11`, `.12`. |
| 8 | Explainable packs with typed identity, provenance, freshness, trust, and score reasons | **PARTIAL** | Rendering is strong for admitted memories; rule/evidence v3 identity, calibration, and deterministic admission remain open. |
| 9 | Learn loop turns repeated evidence into a rule used by later search/pack | **WORKING on the oracle; packable-rule identity still partial** | 2026-10-22 (§17.2): **PAP is precision 1.00 / recall 0.833** on the oracle (5 proposals, 0 polluted by any `must_not_propose` substring), against 0/10 on 2026-10-01 — target ≥ 0.70/0.70 **MET**. Rule hits now resolve into packs (`resolve_rule_hit` / `RuleHitResolution`, `src/core/context.rs:14188`) and README documents `entityKind: "rule"`. Remaining: typed `RuleId`/`EvidenceId` pack identity (`bd-vp087`, in progress; `ee.pack.v3` consumer sweep still pending at `src/pack/mod.rs:1247`), incident cards `.59`, fingerprints `.60`. |
| 10 | Maintain loop links, decays, consolidates, validates, repairs, and converges | **PARTIAL / UNPROVEN** | Decay/machinery exist; public consolidate → apply → index → retrieve is `bd-1oep7`. |
| 11 | Complete durable backup, verify, migration, restore, and rebuild | **WORKING on a fresh store; evolved-store proof open** | 2026-10-01: v0.16.0 `recoveryInventory` reports complete schema and snapshot coverage with 0 uncovered required tables; verify passes; side-path restore selects identical pack items (§16.2). `.13` is proof-owed (commented). Evolved, CASS-populated stores remain `.14`. |
| 12 | Graceful offline degradation remains useful and truthful | **PARTIAL** | Honest fallback/abstention exists; released binary may instead return no result/error, and uncalibrated quality is misleading. |
| 13 | Stable machine envelopes and truthful repair exits | **PARTIAL** | `bd-34l8k`, `bd-3ak9b`, `bd-vv2dw`, `bd-aav4p`, `bd-5k6k7`; typed pack v3 is an intentional future break. |
| 14 | Exact eight North Star public-CLI scenarios | **UNPROVEN as a complete set** | The focused CASS loop now passes its retained pinned-source test (§A); this does not prove all eight. `bd-2mpct`, `bd-2mpct.1`, and `.17` own exact complete coverage. |
| 15 | Privacy/trust holds from ingest through index/model/pack/proof/backup/mesh | **PARTIAL / UNPROVEN** | ADR 0085 and source screening are strong; cross-source live admission, retained-generation, proof-sink, and recovery negatives remain. |
| 16 | Graph insight and optional adapters are real or explicitly degraded | **PARTIAL / UNPROVEN** | Core graph/team/serve/insight implementations are substantial. Several formerly placeholder insight sections now have real code and regression assertions; remaining acceptance is tracked by `bd-2pos6` and `.34`, not inferred from stale placeholder labels. `.9` owns stable claims. |
| 17 | Multi-agent local writes preserve integrity and truthful freshness | **PARTIAL / UNPROVEN** | Strong tests exist; current full-suite proof is red and evidence/linkage/index generation gaps remain. |
| 18 | Unix team-confederation and documented environment posture | **PARTIAL** | Unix/two-host/Windows/fake-IdP evidence exists. The 2026-09-17 amendment, decided on delegated authority, resolves the two-human criterion mismatch (`.8`); this is not a new current-source runtime proof. |
| 19 | Canonical readiness verification and green CI | **REGRESSED (hosted verification dark) / UNPROVEN** | 2026-10-22 (§17.3 N3): **Actions is `enabled: false` repo-wide.** The last run of any workflow was 2026-10-08T22:58Z and it failed on an allowed-actions policy, not on code — `actions/checkout@v4`, `dtolnay/rust-toolchain@nightly` and `cargo-deny-action` are rejected because "all actions must be from a repository owned by Dicklesworthstone". ~994 non-tracker commits carry no hosted verdict. `.41` closed honestly on `CI Static` run 37056106739 (`c211215a7`, 2026-10-02); that green is now 20 days stale and cannot be re-established while the policy stands. Workflow files grew to 88, of which **47** publish to main (N8). Local static gates are all green and prove presence, not behavior — none of them sees N1. Owners: `bd-actions-disabled-repo-wide-33mfm`, `bd-479c6`, `.5`, `.58`, `bd-fy92m`. |
| 20 | Reproducible performance and usable first-agent latency | **MEETING its targets at 4k spans; 50k and README rows still unproven** | 2026-10-22 (§17.2): at 4,009 spans search 0.83–1.19 s, pack 1.14 s, status 0.37 s, `ask` 0.89 s, import 0.60 s/session, `why` 0.079 s. `remember` is 0.92–1.07 s at 10 memories and 1.24–1.33 s at 4,009 spans, a log-log slope of ≈ 0.04 against the ≤ 0.15 WCS target. Caveats that keep this row open: the slope is a two-point estimate and the 50k point is unmeasured; `single_document_as_full_rebuild` is still the intake label (`src/core/index.rs:4485`) so `.57`'s wiring is unlanded and the risk is structural; `bd-xayfm` remains open with a colder cold-start basis (2.5–3 s, tokenizer build) than this host measured, and `.61` closed as a duplicate of it with no work done. README rows remain historical; `.6` owns evidence. |
| 21 | Hermetic multi-platform release/install chain | **PARTIAL / UNPROVEN** | Current release has archives/checksums/installers but no candidate checks/provenance set; tag-only workflow inputs remain non-hermetic. `.7`, `.18`, `.20`, `.21`. |
| 22 | Canonical walking skeleton proves init → remember → search → pack → why | **WORKING (released binary, serial)** | 2026-10-01: public v0.16.0 init → remember → search → pack → why → replay → outcome → ask passes black-box in an isolated store (§16.2). Readiness-gate wiring remains `.5`/`.17`. |
| 23 | Recommended agent journey is coherent and fast without a daemon | **WORKING at 4k spans; the journey's own first step can still fail silently** | 2026-10-22: TTFUC is 1.14 s cold at 4,009 imported spans (was 8.7 s), inside the ≤ 2 s target, with no daemon. The journey's weak link is no longer latency but N1: a user who follows README Quick Start (`init` → `import cass` → `pack`) under the default download policy can reach a store that reports healthy and packs nothing. `.66` owns that; the metric program still runs through `.63`/`.50`, which `.67` must first make trustworthy. |
| 24 | No-silent-mutation lifecycle, helpful/harmful feedback, decay/inversion | **PARTIAL / UNPROVEN** | Machinery exists; exact later-pack behavior, audit semantics, and docs wording need behavioral proof or maturity demotion. `.9`, `.17`, `bd-2mpct`. |

---

## 9. Existing work that this bridge reuses

Do not duplicate these Beads. Their current acceptance text already describes
the implementation or proof slice needed:

| Concern | Existing Beads |
| --- | --- |
| Evidence/index generation truth | `bd-3k1mg`, `bd-index-auto-freshness-m5kwf` |
| CASS transcript search + pack hydration | `bd-16imy` |
| Applied procedural rule search + pack hydration | `bd-3h6bz` |
| Typed Memory/Rule/Evidence identity and admission design | Closed design bead `bd-12ubv`; accepted ADR 0085; implementation stays in `bd-16imy` / `bd-3h6bz` |
| Exact eight North Star flows and behavioral vision gate | `bd-2mpct`, `bd-2mpct.1` |
| Consolidation close-loop proof | `bd-1oep7` |
| Current red contracts/lib suites | `bd-g3yh5`, `bd-2yz9p`, `bd-1eeyw` |
| Stable doctor/Windows machine contracts and repair safety | `bd-34l8k`, `bd-3ak9b`, `bd-vv2dw`, `bd-aav4p`, `bd-5k6k7` |
| Fresh-workspace model cache resolution | `bd-fresh-workspace-hash-fallback-kvltg` |
| Remaining real insight sections | `bd-2pos6` and children |
| Installer matching-version repair/verification | `bd-xww0x` |
| Broad performance-gate hygiene | `bd-je0nb` |
| Warm public-search latency | `bd-search-warm-latency-0bh05` |
| Stable-surface maturity choices | `.9` must close the relevant blocker or demote the advertised surface; examples include `bd-rs4cm`, `bd-d67os.27`, `bd-resume-verb-v0f57`, `bd-orient-fast-content-iubub`, `bd-fyack`, `bd-3ap2m`, `bd-multiplicity-aware-trust-p0u7g`, `bd-d67os.19`, and `bd-degraded-advisory-noise-vfx8u` |

---

## 10. Newly uncovered work

The end-to-end reality check filed one self-contained bridge epic,
`bd-reality-core-convergence-1azkt`, with these dependency-linked children:

| Label | Priority | Gap | Required outcome |
| --- | ---: | --- | --- |
| `bd-reality-core-convergence-1azkt.1` | P0 | Determinism promise lacks a complete snapshot/numeric/serialization domain | Freeze canonical product payload versus telemetry/state-creation; version snapshot identity and pack hashing. |
| `bd-reality-core-convergence-1azkt.2` | P0 | Plausible reader-visible index hole and cross-process model race | Immutable content-addressed generations, atomic DB pointer, reader leases, publisher fences, bounded model admission, zero-mutation read-only. |
| `bd-reality-core-convergence-1azkt.3` | P1 | Equality-only tests miss invalid concurrent histories | Post-fix no-mock linearizability, crash, privacy, cache, platform, and resource matrix. |
| `bd-reality-core-convergence-1azkt.4` | P0 | Local PPR, still-unclassified score-changing paths, and incomplete release/proof identity violate the hard stack boundary | Exhaustive call-site classification; only Frankensearch/FrankenNetworkX changes retrieval/metrics. |
| `bd-reality-core-convergence-1azkt.5` | P0 | No single executable manifest/pinned composite RCH/proof-capsule contract | Build the verifier now; final green truth belongs to `.19`, avoiding a dependency deadlock. |
| `bd-reality-core-convergence-1azkt.6` | P1 | Public latency/SLO claims lack reproducible correct-output evidence | RCH-built attested candidate, local M3 black-box driver, raw samples/correctness, explicit reproduce-or-remove decision. |
| `bd-reality-core-convergence-1azkt.7` | P1 | Release staging is non-hermetic and publication currently precedes native installer proof | Private draft/local staging only; locked signed/provenance-complete assets and native smoke before human publish. |
| `bd-reality-core-convergence-1azkt.8` | P1 | Original two-human criterion exceeded the retained two-host evidence | Two-host boundary accepted on 2026-09-17 on delegated authority (the earlier "user approved" provenance was struck as unverifiable); preserve artifact identity and residual independent-operator risk, without an ownerless roadmap promise. |
| `bd-reality-core-convergence-1azkt.9` | P1 | Shipped claims, maturity, primary journey, docs, and release copy disagree | Pre-release stable/beta/experimental/reserved ledger; close blocker or demote claim; generated truthful copy. |
| `bd-reality-core-convergence-1azkt.10` | P0 | Live race evidence lacks source authority | Build exact attested candidate and reproduce or refute before `.2` changes code. |
| `bd-reality-core-convergence-1azkt.11` | P0 | Raw BM25 saturates public relevance and contaminates quality/admission | Frankensearch-backed calibration or explicit unknown; correct per-source domains and every downstream consumer. |
| `bd-reality-core-convergence-1azkt.12` | P1 | No product oracle rejects plausible irrelevant retrieval | Adversarial no-mock quality fixtures, calibrated abstention, MRR/nDCG/precision and false-admission gates. |
| `bd-reality-core-convergence-1azkt.13` | P0 | Backup create fails, dry-run can mutate, and durable state is omitted/lost | Complete versioned five-job snapshot and lossless side-path recovery contract. |
| `bd-reality-core-convergence-1azkt.14` | P1 | Backup contract lacks evolved-store runtime proof | Tamper/failure/migration/restore/rebuild/query E2E with exact durable-state comparison. |
| `bd-reality-core-convergence-1azkt.15` | P0 | FrankenNetworkX lacks personalized seed PageRank required to remove local PPR | Add/pin upstream capability or disable/degrade pack PPR influence. |
| `bd-reality-core-convergence-1azkt.16` | P1 | Generation/model lifecycle can amplify disk/RSS/latency and privacy risk | Bounded admission, lease-aware GC, secure paths, operator inspection/repair, large-corpus proof. |
| `bd-reality-core-convergence-1azkt.17` | P0 | Existing release oracles can pass empty/OR/ignored/file-presence evidence | Adversarial exact assertions, mutation tripwires, linearizability model, exact shard/test inventory. |
| `bd-reality-core-convergence-1azkt.18` | P0 | Effective Franken-stack/release inputs and tools are not hermetic/provenance-complete | No semantic post-checkout rewrite; pin/bind every tree/tool/container/action; locked builds and least privilege. |
| `bd-reality-core-convergence-1azkt.19` | P0 | Verifier implementation is not itself a green product verdict | One immutable clean main candidate receives a complete, tamper-detecting `ee.release_candidate_proof.v1` capsule. |
| `bd-reality-core-convergence-1azkt.20` | P0 | Publication is a separate external authority boundary | Record exact human approval; publish only the verified private draft; no authority is inferred from the bead. |
| `bd-reality-core-convergence-1azkt.21` | P1 | Public assets/channels/links can drift after publish | Read-only post-public audit and generated evidence reconciliation. |
| `bd-reality-core-convergence-1azkt.22` | P0 | Prose/related edges can permit premature epic closure | Final graph-encoded evidence-ledger closeout blocked by every mandatory child and reused blocker. |
| `bd-reality-core-convergence-1azkt.35` | P1 | Bridge advice still targets Part II and recommends the already-active Part III | Active-phase advice distinguishes source coverage, behavioral proof, and tracker activity; unknown remains explicit. |
| `bd-reality-core-convergence-1azkt.36` | P1 | Metadata-complete input and missing blocking edges can imply false completion | Independent positive/negative graph/evidence fixtures plus real shell advisory tests, integrated through `.17`. |
| `bd-reality-core-convergence-1azkt.37` | P1 | Downstream tests omit changed sibling unit tests and dev/bench feature targets | Execute exact pinned upstream API negatives and the supported matrix; source identity from `.18`, acceptance consumed by `.19`. |
| `bd-reality-core-convergence-1azkt.41` | P0 | Main red on 4 `CI Static` steps; unlanded delivery-patch test file | Green main at one SHA, plus a guard for the class |
| `bd-reality-core-convergence-1azkt.42` | P0 | Every write keeps a full index copy; hard failure after 1,000 | Bounded, lease-safe retained generations; audited vacuum; cap recovery |
| `bd-reality-core-convergence-1azkt.43` | P0 | No real-shape oracle; every fixture is clean | Schema-faithful CASS corpus, scale generator, split judgments |
| `bd-reality-core-convergence-1azkt.44` | P1 | cass refused at its standard path; `[cass].binary` dead code | Hash-pinned explicit trust wired everywhere; doctor check |
| `bd-reality-core-convergence-1azkt.45` | P0 | Evidence is raw JSONL envelopes | Typed transcript projection; turn-level units; UTR ≥ 0.95 |
| `bd-reality-core-convergence-1azkt.46` | P0 | Session proposals are junk | Projected, correctly detected, corroboration-weighted, abstaining proposals |
| `bd-reality-core-convergence-1azkt.47` | P0 | Evidence-scale latency | Materialized verdicts (I-1), scale invariance (I-2), SLOs |
| `bd-reality-core-convergence-1azkt.48` | P1 | Import about 18 s per session | Profiled, batched, ≥ 5× throughput |
| `bd-reality-core-convergence-1azkt.49` | P1 | Over-broad instruction-risk quarantine | Contextual, measured screening (FPR ≤ 5%, FNR 0%) |
| `bd-reality-core-convergence-1azkt.50` | P0 | No real-data release gate | Real-corpus gate in ENFORCE mode; one Real-Data Suite stage |
| `bd-reality-core-convergence-1azkt.51` | P1 | README and AGENTS claims vs real behavior | Docs truth after the fixes |
| `bd-reality-core-convergence-1azkt.52`–`.56` | P2/P3 | why-not typed ids; eval outside repo; hits-profile code; leaked model staging; pack timing noise | Small truth fixes |
| `bd-reality-core-convergence-1azkt.57` | P0 | Incremental intake regressed to full rebuild per write | LSM-style delta generations inside safe publication; WCS ≤ 0.15 |
| `bd-reality-core-convergence-1azkt.58` | P0 | Unreachable tests and publishing workflows | Inventory drift check in CI Static; shard runners; recorded retirement decision |
| `bd-reality-core-convergence-1azkt.59` | P1 | Packs carry fragments, not incidents | Derived incident cards as typed evidence entities (ADR amendment) |
| `bd-reality-core-convergence-1azkt.60` | P1 | Error-fingerprint store never fed by CASS | Failure arcs → fingerprints + repair links |
| `bd-reality-core-convergence-1azkt.61` | P1 | About 1.1 s model cold start per process | mmap plus compiled tokenizer cache upstream; ≤ 0.2 s |
| `bd-reality-core-convergence-1azkt.62` | P1 | Swarm never runs ee on its own history | Redacted own-corpus dogfood reports |
| `bd-reality-core-convergence-1azkt.63` | P0 | Gate would be built last | Report-mode probe first; v0.16.0 baselines |
| `bd-reality-core-convergence-1azkt.64` | P0 | Triple admission scans plus N+1 | No-contract-change read-path hotfix |
| `bd-reality-core-convergence-1azkt.65` | P1 | 0.16.0 users carry R1/R4/R5 | 0.16.1 candidate; publication only with explicit human approval |

**2026-08-24 coverage result (historical).** At that audit's start, closing all 122 nonclosed records would
still have left determinism attribution, score truth, complete recovery,
verification authority, reproducible performance, supply-chain hermeticity,
release staging/publication separation, and two-human disposition without
complete owners. The bridge epic plus `.1`–`.22` now gives every §8 row an
implementation, proof, decision, or maturity-demotion owner. That means the
*plan* is covered; it does not mean the product gaps are implemented or green.
The final closeout child encodes mandatory edges, and `br dep cycles --json`
reports zero active cycles after refinement.

**2026-09-04 coverage result.** The refreshed 24-goal checklist retains the
existing implementation owners and adds `.35`–`.37` for concrete steering and
proof omissions. Eleven blocking edges now connect newer evaluation, status,
provenance, and release work to the right completion gates. Completing all
open work would close the enumerated vision only if the exact behavioral
acceptance and retained current-candidate evidence also pass. Task closure
alone, or completion of the former `.1`–`.22` range, remains insufficient.

---

## 11. Dependency-ordered bridge

### T0 — Establish authority, contracts, red oracles, and decisions

1. Implement canonical verifier/proof-capsule machinery `.5` while the product
   is still red; it must not depend on already-green product evidence.
2. Freeze determinism `.1`, create the source-attested pre-fix oracle `.10`,
   harden test-oracle integrity `.17`, and bind hermetic inputs `.18`.
3. Resolve the external personalized-PageRank path `.15`: upstream capability
   or explicit disabled/degraded influence.
4. Two-human scope `.8` was amended by the user on 2026-09-17 to two independent
   tailnet hosts; keep documentation and evidence identity aligned.
5. Record the performance branch in `.6`: reproduce each stable claim or
   remove/narrow it; measurement happens after a candidate exists.

**Gate:** every later result can name exact source/binary/dependency authority,
the released-binary race has a red-or-green current-source oracle, and no
verification or scope decision is circularly blocked on final success.

### T1 — Fix source-of-truth, retrieval, durability, and security boundaries

1. Land immutable-generation/model admission `.2` and bounded lifecycle `.16`.
2. Complete `bd-3k1mg`, `bd-index-auto-freshness-m5kwf`, `bd-16imy`, and
   `bd-3h6bz`: every **eligible, positively admitted** native entity has a
   canonical document, truthful generation, and ADR-0085 typed hydration.
3. Complete stack conformance `.4` and relevance semantics `.11`.
4. Complete full durable recovery `.13`; keep derived indexes/cache rebuildable.
5. Enforce redaction/admission before index persistence, embedding, trace,
   proof, backup, or mesh egress; recheck live authorization/revision/security
   epoch within the pinned snapshot.

**Gate:** core code has no known partial-publication, local core-algorithm,
uncalibrated-confidence, stranded-rule/evidence, or lossy-recovery path.

### T2 — Prove the five jobs, quality, recovery, and eight North Stars

1. Run post-fix linearizability/determinism `.3`, retrieval-quality `.12`, and
   evolved-store recovery `.14` through adversarial oracle `.17`.
2. Complete public manual-memory, CASS, rule, and consolidation loops under
   `bd-16imy`, `bd-3h6bz`, and `bd-1oep7` with exact entity/content/provenance,
   audit, generation, retry, and why assertions.
3. Complete `bd-2mpct` and `bd-2mpct.1` only after Learn and Maintain behavior exists;
   all eight scenarios use public commands and no hand-seeded substitute.
4. Exercise cancellation, contention, offline semantic/CASS, stale graph,
   corrupt derived state, denied content, and harmful/helpful lifecycle paths.

**Gate:** all eight North Stars plus durability and quality pass; empty output,
abstention, OR assertions, ignored tripwires, and file presence cannot pass.

### T3 — Produce the functional green candidate, then measure it

1. Close current red contract/lib/doctor/security/platform prerequisites named
   by `.19` and run the canonical manifest on one clean immutable main SHA.
2. Produce and independently verify the content-addressed green candidate
   capsule `.19`; hosted CI and pinned RCH must agree.
3. Use that exact returned binary for `.6` on the claimed M3 host. Retain
   correctness digests with latency/resource samples, then generate or remove
   public rows according to the T0 decision.

**Gate:** a functionally green, source-attested candidate exists and every
remaining performance claim has reproducible evidence or has been demoted.

### T4 — Make claims truthful and stage a private release

1. Complete pre-release maturity/docs ledger `.9`: every stable claim closes
   its blocker or is demoted; one canonical agent journey remains.
2. Build the hermetic private draft/staging pipeline `.7` from the exact `.19`
   candidate and `.18` effective inputs.
3. Verify packages/installers/signatures/provenance/SBOM posture and native or
   explicitly emulated platform behavior before any public mutation.
4. Keep the release private on every failure; draft asset mismatch fails rather
   than clobbers.

**Gate:** the exact asset set and release copy are privately verified and no
credentialed publication has happened.

### T5 — Human publication, public audit, and closeout

1. `.20` records explicit human authorization for the exact candidate/tag and
   only then publishes the already verified draft and explicitly authorized
   channels.
2. `.21` performs the post-public read-only asset/channel/link/provenance audit
   and regenerates public evidence state.
3. `.22` checks every enumerated checklist row and dependency, runs the final
   cycle/closure/evidence audit, and is the sole Part III rollup.
4. Only after `.22` closes may Part III archive and this path become Part IV.

**Gate:** code, candidate capsule, public release, README/AGENTS/plans/ADRs,
matrix/coverage/CHANGELOG, release page, and Beads describe one coherent state.

---

## 12. Required proof matrix

| Proof | Required assertions |
| --- | --- |
| Source/binary authority | Clean immutable main tree; exact sibling/dependency/toolchain/target/features; candidate binary hash and `ee version` provenance; installed historical evidence never masquerades as current-source proof. |
| Serial determinism | At least 100 identical fresh-process requests pin the same source snapshot, backend/model identity, ordered result IDs/scores, admitted items, provenance, and pack hash. |
| Concurrent linearizability | Multi-process reads/writes/crashes map wholly to one committed DB/index/model/security snapshot; no missing/partial generation, stale fence publication, or mixed-epoch recovery. |
| Retrieval calibration/quality | Raw source score and kind remain truthful; calibration identity or unknown posture; distinct BM25 values do not saturate; distractors fail admission; MRR/nDCG/precision/false-admission/abstention gates pass. |
| CASS closed loop | Import unique positively screened phrase → emitted jobs only → search exact `EvidenceId` → ADR-0085 typed admission or exact denial → pack/why/replay/outcome preserve redacted session/span provenance; no synthetic memory. |
| Rule closed loop | Curate/apply unique rule → emitted jobs only → search exact rule → pack correct procedural section → why resolves source memories/evidence. |
| Maintain closed loop | Duplicates → dry-run no mutation → candidate → validate/apply → lineage/audit → emitted jobs → one retrieval result → idempotent retry. |
| Data durability and upgrade recovery | Evolved historical store → non-mutating dry-run → atomic backup → tamper verification → isolated restore/migrate → derived rebuild → identical durable inventory, audit/trust/provenance, and continued five-job behavior. |
| Privacy/trust boundary | Denied/secret/path-bearing content is absent from index staging/retained generations, model input, result/pack ledgers, stderr/logs, proof/support artifacts, backups/restores, and mesh egress. |
| Harmful/helpful lifecycle | Outcome → audited confidence/decay/demotion/inversion decision → later search/pack behavior; no silent or target-type-confused mutation. |
| North Stars | All eight §4 command sequences, exact good-output fields, and exact success signals; no hand-seeded substitute for CASS/rule flow. |
| Oracle integrity | Mutation/kill-switch proof makes each release scenario fail when its required behavior is broken; empty/OR/ignored/file-presence/degraded alternatives cannot pass. |
| Test inventory | Exact test IDs appear once across shards; zero/omitted/duplicate/filtered/ignored counts and all retries are explicit. |
| Verification truth | Required stage cannot be skipped, tracked-red, infra-failed, timed out, cancelled, OOM-killed, bypassed, or run against another binary and still yield overall pass. |
| Stack conformance | Pack graph influence comes through FrankenNetworkX; retrieval/scoring comes through Frankensearch; no exported custom BM25/PPR core substitute remains; version identity is exact. |
| Hermetic inputs | Effective ee/sibling trees, locks/config/toolchain/linker/SDK/features/tools/actions/containers/advisories are pinned and provenance-bound; clean-cache rebuild matches. |
| Candidate capsule | Tamper-detecting `ee.release_candidate_proof.v1` binds all identities, stage/test inventory, attempts, results, binaries, performance, and CI/RCH evidence. |
| Native packaged behavior | Every advertised archive runs on its native target or is labeled compile/emulated; packaged walking skeleton, model claim, glibc/musl, Windows stdout, macOS targets, and installer modes pass before publish. |
| Performance and first-agent journey | Correct-output raw samples + sufficient count + host/power/thermal/workload/source identity + variance/RSS/disk/backlog; stable core journey meets declared SLO without daemon or claim is demoted. |
| Documentation preflight | Executed snippets/schema selectors, explicit maturity, one primary journey, no contradictions/historical proof theater/ownerless promise, generated release copy. |
| Post-public audit | Exact tag/checks/assets/hashes/signatures/provenance/SBOM/install/channel/link state matches the candidate and generated public docs. |

Every E2E emits structured `ee.test_event.v1` logs, preserves stdout/stderr
separation, uses no mocks where the acceptance is a live integration, and runs
Cargo only through the repository RCH wrapper on this Mac.

Safe deterministic failpoints are permitted for crash/partial-write testing;
"no mocks" means the real FrankenSQLite, Frankensearch, filesystem publication,
and public binary paths remain in use. Proof output goes to an unpredictable,
owner-only, redaction-aware, content-addressed sink.

---

## 13. Architecture and product invariants for implementation

### 13.1 Immutable derived-state publication

- Derived index generations are immutable and content-addressed. Tier files and
  manifests are durable before one atomic SQLModel/FrankenSQLite pointer commit.
- Readers pin DB, generation, model, config, authorization, and security epoch
  before retrieval and hold the generation lease through hydration and hashing.
- Publisher fencing prevents a paused/stolen owner from publishing. GC never
  reclaims a leased or currently eligible generation.
- Recovery never prefers a structurally complete but privacy-stale generation.
- Model admission is cross-process and resource-bounded for the whole semantic
  operation; core CLI correctness never depends on a daemon.

### 13.2 Score semantics and library ownership

- Raw backend scores remain raw and source-typed. A number is called relevance
  or confidence only with a named valid calibration artifact.
- Unknown calibration produces unknown/abstaining quality, not synthetic `1.0`.
- Frankensearch owns lexical/vector/fusion/rerank/calibration math.
  FrankenNetworkX owns core graph metrics. EE owns policy, eligibility, packing,
  provenance, and rendering, but does not recreate dependency algorithms.

### 13.3 Typed CASS/rule admission

ADR 0085 is controlling: `MemoryId`, `RuleId`, and `EvidenceId` remain distinct
pack identities. A safe undistilled span or sourceless advisory rule may enter a
typed pack after live fail-closed admission. Search metadata is never
authorization. Instruction-like, secret-bearing, stale-revision, malformed,
scope-mismatched, unscreened, or wrong-workspace evidence is denied. Curated
memory/rule remains the preferred durable interpretation; no synthetic memory
is invented merely to satisfy a pack schema.

### 13.4 Durable versus rebuildable state

FrankenSQLite/SQLModel is the source of truth. Backup must enumerate and round
trip every durable five-job table and audit/provenance ledger. Search indexes,
embeddings, graph snapshots, and caches are derived and are either omitted or
verified as optional accelerators, then rebuildable. Persistent-data migration
is a durability obligation, not an obsolete public-API compatibility shim.

### 13.5 Verification and release authority

- Building a verifier is different from proving a candidate green.
- Historical/released-binary evidence is different from a current source-
  attested candidate.
- Private staging is different from credentialed publication.
- An open publication bead is not authorization. `.20` requires explicit human
  approval for the exact candidate/tag before any external mutation.
- Post-public audit is independent evidence, not release-job self-attestation.

---

## 14. Complexity, risk, and sequencing rationale

| Workstream | Complexity | Primary risk if rushed | Risk control |
| --- | --- | --- | --- |
| Snapshot/determinism/index/model (`.1`–`.3`, `.10`, `.16`) | Very high | Mixed-generation reads, privacy rollback, memory/disk herd | Immutable generations, SQL pointer, leases/fences, linearizability and resource proofs |
| Franken-stack/relevance (`.4`, `.11`, `.12`, `.15`, `.18`) | High + upstream | A second algorithm or fake confidence survives under a new name | Upstream dependency boundary, call-site/type enforcement, calibrated/unknown score contract |
| Typed CASS/rule and Maintain (existing P0/P1 work) | Very high | Trust laundering, invented identity, false closed loop | ADR 0085 live admission, exact entity/provenance, adversarial public E2Es |
| Backup/recovery (`.13`, `.14`) | Very high | Silent durable-history loss or mutating dry-run | Table inventory, consistent snapshot, atomic create, side-path restore, evolved-store comparison |
| Verification/oracles (`.5`, `.17`, `.19`) | High | Green theater from skips, wrong binary, empty output, incomplete shards | Declarative manifest, mutation tests, exact inventory, immutable proof capsule |
| Performance (`.6`) | Medium-high | Fast incorrect output or irreproducible marketing number | Correctness digests, source-attested local driver, reproduce-or-remove decision |
| Release/docs (`.7`–`.9`, `.18`, `.20`, `.21`) | High + external | Public broken/misrepresented release or unauthorized channel mutation | Hermetic private staging, maturity preflight, explicit human publish, independent audit |
| Mesh criterion (`.8`) | Low code / external | Endless blocker or dishonest retroactive closure | Early literal proof-or-amend decision with approver and residual risk |

The dependency graph intentionally front-loads decisions, contracts, and red
oracles; then implementation; then behavioral proof; then green convergence;
then performance/docs/private staging; and only finally human publication. This
is the shortest honest critical path because it avoids building tests after a
fix, blocking verifier construction on already-green tests, or publishing
before the copy/assets they advertise are verified.

---

## 15. Part III final close criteria

The bridge may archive only when all of the following are simultaneously true:

- The two-human criterion is satisfied or explicitly amended with a recorded
  product decision and an owned post-v1 follow-up if still promised.
- `bd-3k1mg`, `bd-16imy`, and `bd-3h6bz` are behaviorally closed.
- Every mandatory child in the current evidence ledger, including later
  implementation/proof additions, is closed with its required artifacts and
  is reachable through blocking edges from `.22`, the final rollup. Parent-child
  edges and a historical `.1`–`.22` numeric range are insufficient. Publication `.20` closes only
  after exact written human authorization if publication remains in scope.
- `bd-2mpct` and `bd-2mpct.1` prove all eight exact North Stars and no ignored core-loop
  tripwire remains.
- `bd-1oep7` proves the public Maintain loop.
- One evolved real store backs up, verifies, restores to an isolated side path,
  migrates, rebuilds, and resumes all five jobs without durable-state loss.
- Lexical, semantic, hybrid, reranked, and degraded relevance/quality fields
  remain truthful; irrelevant distractors do not become maximal relevance.
- CASS evidence follows ADR 0085 live admission and privacy policy end to end.
- The documented primary first-agent journey works within its declared latency
  posture without requiring a daemon.
- The real-corpus gate (`.50`) runs in ENFORCE mode on the candidate and is
  green for UTR, TTFUC, PAP, FAR, WCS, bounded retention and the hook budget on
  the real-shape oracle corpus (`.43`). Clean-fixture proofs alone are
  insufficient (2026-10-01).
- One immutable current SHA is green for format, clippy `-D warnings`, complete
  required tests, exact North Stars, representative E2Es, dependency audit,
  and the canonical readiness manifest.
- Hosted CI and the release dry-run agree with that same verdict.
- README performance/release/Windows/Homebrew/crates.io/mesh claims match
  retained evidence.
- Active Beads have no dependency cycles and every remaining vision promise
  in the enumerated §8 checklist has an active owner, maturity demotion, or an
  explicit approved non-goal decision. Unrelated experimental tracker history
  is not silently promoted into the close gate.

Bead-count percentage, file presence, command registration, an abstention
sentinel, a scheduled CI success with substantive jobs skipped, or a remote
run that timed out/OOMed is not sufficient closure evidence.

---

## 16. 2026-10-01 reality check: real-data re-baseline

**Verdict.** On the small, clean data its own tests use, `ee` now works. Use
the shipped `v0.16.0` binary on real Claude Code transcripts, though, and two
of its five jobs break down. Ingest (CASS) and Learn produce mostly noise and
slow down sharply with corpus size. Retrieve and Pack slow down too, and Pack
fills the token budget with JSON scaffolding. Main is red again, and no single
gate can say otherwise.

That is a different failure shape from every earlier pass. The 2026-09-04 gaps
in concurrency determinism, backup coverage and `ask` usefulness are closed in
the shipped binary. The failures the swarm cannot see are the ones that only
show up on real-shaped data at realistic scale. Every committed acceptance
fixture is clean, synthetic, and small.

### 16.1 Evidence authority

| Item | Value |
| --- | --- |
| Source HEAD | `552f7ba4f` (2026-10-01 20:38Z), package `0.16.0`, 243 commits after `v0.16.0` |
| Shipped binary | public `v0.16.0` `ee-aarch64-apple-darwin.tar.xz`, SHA-256 verified against its published `.sha256`; `ee version --json` attests clean `b445401ac`, release profile, `aarch64-apple-darwin` |
| Probe isolation | Private `XDG_*` roots under the gitignored `.ntm/reality-check-2026-10-01/`. The pinned Model2Vec model came from the local cache, with `EE_EMBED_DOWNLOAD=off`. The real CASS corpus was used through an explicit `EE_CASS_BINARY` opt-in. |
| Hosted CI | `CI`, `Release` and `macOS EE Artifact` have been `disabled_manually` since 2026-08-27. `CI Static` now also runs `clippy --all-targets -D warnings`, `cargo xwin check` for `x86_64-pc-windows-msvc`, and cargo-deny. It was last green on `b979c1777` (2026-09-29) and has been red from 2026-09-30 through `552f7ba4f`. |
| Distribution | GitHub `v0.16.0`: 6 native targets plus checksums, installers, and `release-probe-aarch64-apple-darwin.json` (12 checks, `verdict: pass`). crates.io `eidetic-engine` 0.16.0. Homebrew formula 0.16.0, whose SHA matches the release asset. Unsigned: no Sigstore, SLSA, or `ee-v0.16.0-manifest.json` asset, although AGENTS.md lists the manifest as expected. |
| Tracker | 4,767 records: 268 open, 46 `blocked`, 2 in progress, 3 deferred. 261 records closed since 2026-09-17. Bridge epic children: 15 closed, 25 open, 2 blocked. |
| Velocity | 2,754 commits on main since 2026-09-04. Since 2026-09-17, 1,385 were not tracker syncs, and their top scopes are `ask` 79, `backup` 78, `search` 70, `e2e` 48, `pack` 41, `doctor` 41. |

Static gates at HEAD all pass locally:
- vision coverage: 142 surfaces, 135 behaviorally exercised, 0% gap
- closure lint
- contract-drift radar: 601 fixture codes, 0 violations
- bridge staleness, which now names Part III correctly

They still prove presence, not behavior.

### 16.2 What works in the shipped binary

Black-box on `v0.16.0`, each with retained JSON under `probe/out/`:

1. **Walking skeleton.** `init → remember ×8 → search → pack → why → pack
   replay → outcome → ask` works. Provenance, trust, and selection reasons are
   present, and `why` resolves the persisted pack selection.
2. **Determinism and concurrency (closes the `0.14.x` failure).**
   - 8 concurrent identical searches: one result order and `rrf_fused` throughout.
   - 6 concurrent read-only packs: one hash, equal to the serial hash.
   - 6 packs and 4 searches racing one `remember`: all report one coherent
     snapshot with a truthful `search_index_stale`.
   - Serial reruns are byte-stable.
3. **Backup and recovery.** `recoveryInventory` reports
   `schemaCoverageComplete=true`, `uncoveredRequiredTableCount=0`. Verify
   passes, side-path restore completes (6.6 s), and the restored store selects
   the identical pack items. The component digests differ only in request,
   rendered-text and degraded, which are legitimate path and index-freshness
   differences.
4. **`ask`.**
   - A direct hit is answered with the exact memory span cited (confidence 0.68).
   - An unrelated question abstains (`no_confident_answer`).
   - A native rule is cited by `RuleId`.
5. **Honest posture.**
   - A distractor query is flagged `weak_query_recall`.
   - `status`, `index status` and `doctor` agree.
   - A stale model receipt is named, with a working repair (`ee model fetch`
     re-minted it, and `status` dropped from 0.49 s to 0.17 s).
6. **Distribution.** GitHub, crates.io and Homebrew agree at 0.16.0, and the
   binary self-attests its commit.

### 16.3 What does not work: new failures found on real data

| # | Finding (measured) | Status | Owner |
| --- | --- | --- | --- |
| R1 | **CASS is refused by default.** `cass` at `~/.local/bin/cass` (its standard install path) is outside ee's auto-trust allowlist, which holds only `/usr/local/bin`, `/usr/bin` and `/opt/homebrew/bin` (`src/cass/client.rs:233`). README Quick Start step 2 therefore fails with `cass_unavailable`. README never mentions `EE_CASS_BINARY`, which works. The documented `[cass].binary` config opt-in is dead code: config parses it (`src/config/merge.rs:52`), but `ee import cass`, `status` and output discovery all call `discover_import_binary(None)` (`src/cli/mod.rs:24270`, `src/core/status.rs:1386`, `src/output/mod.rs:10038`). The security rationale (EE-3qgw) is sound; the onboarding is not. | NOT WORKING (first run) | NO_BEAD at audit → `.44` |
| R2 | **Evidence is raw transcript JSONL.** Every Claude Code span is the verbatim `cass view` line, envelope included: `{"parentUuid":…,"isSidechain":…,"promptId":…,"message":{…}}`. This is by design (`src/cass/ingestion.rs:5-7`). Search shows it, and a 3,000-token pack spent 2,949 tokens on six such lines, about 35% of them scaffolding characters. A role and text extractor exists (`curate_session_arc_text.rs:18`), but only session-arc curation uses it. | WRONG_APPROACH (usefulness) | NO_BEAD at audit → `.45` (+ `.59` cards) |
| R3 | **Learn from sessions produces junk.** `ee review session <id> --propose` on a real session returned 10 candidates. Six read "For \`formatting\` work, follow the evidence-backed procedure shown in this session: {\"parentUuid\":…" at confidence 0.85. `review_candidate_content` takes the first 180 characters of two raw excerpts, which is all metadata. `review_candidate_confidence` is `0.45 + 0.08 × span_count` (`src/core/curate.rs:4111-4150`). Its unit test uses the clean excerpt "Run golden tests / Keep JSON stable". The other four candidates came from the existing session-arc proposer (`curate_session_arc*.rs`). It projects text, but it read "bridge suite passed … (21 passed, 0 failed …)" as a failure and proposed the generic "use the observed repair for this failure". | WRONG_APPROACH (Learn job) | NO_BEAD at audit → `.46` (quality across both proposers; `bd-2vq2z.9` keeps the linked-pair schema) |
| R4 | **Retrieval scale cliff.** Importing 3 sessions (4,919 spans: 970 admitted, 3,949 quarantined) took `search` from 1.25 s to **11.1 ± 0.35 s**, `pack` to 8.7 ± 0.7 s and `status` to 3.0 s, all CPU-bound. A warm daemon does not help (pack 9.4 s, search 13.3 s). The cause: each search runs the full evidence admission scan three times (pre-read reconcile, search status, model lifecycle). Each scan reruns classification, hashing, JSON parses and screening on every row, quarantined rows included (`index.rs:8656`, `db/mod.rs:15470-15580`). Evidence hits are then hydrated with two point queries each (`search.rs:13733-13763`). HEAD raised that pool from 10 to 100 (`search.rs:9496`), so unreleased source may be worse. No benchmark seeds evidence spans. README's table (38 ms search on 120k docs) is about 300× off. | REGRESSED at realistic scale | root cause NO_BEAD at audit → `.64` (hotfix slice) + `.47` |
| R5 | **Retained index generations never reclaimed.** Every publication, including each `remember`, keeps a full index copy (`index.previous.NNN`); the probe store had 12 after 12 writes. `allocate_retained_index_dir` gives up after 1,000 (`index.rs:5540-5556`), after which every publication fails. `ee index vacuum` only previews. Disk use grows as writes × index size. | LATENT HARD FAILURE | `.42` (P0); `.16` now depends on it |
| R6 | **Main is red now on four `CI Static` steps at `552f7ba4f`.** (a) Module reachability: `src/pack/facility_cache_budget_tests.rs` (11 `#[test]`, added in `fe4c6c7cc`) is declared by no `mod` and cannot compile against HEAD. It calls `with_byte_limit`, `dense_cell_count` and `fallback_signatures`, which exist only inside the `facility-cache-budget-20260930.yml` delivery patch. (b) `cargo fmt --check` fails on 5 files (`ask_candidate_saturation.rs`, `backup_evidence_export.rs`, `backup_evidence_metadata.rs`, `recall_admission.rs`, `pack/binary_validation.rs`). (c) The include!-only format gate fails too. (d) `clippy --all-targets -D warnings` fails on a duplicated `#[test]` at `src/graph/skyline.rs:746` (from `552f7ba4f`). CI Static was last green on `b979c1777` (2026-09-29). | REGRESSED (main) | NO_BEAD at audit → `.41`; class guard `.58`; lane analysis `bd-fy92m` |
| R7 | **Native rules cannot be packed.** A sourceless `ee rule add` rule is the top search hit and is cited by `ask`, but `pack` drops it (`context_rule_hit_unhydrated`, `context.rs:13588`). ADR 0085 slice c (`ee.pack.v3` typed `RuleId` items) has not landed; `PackEntityRef` is "deliberately not wired" (`src/pack/mod.rs:1200`). `ee why rule_<id>` fails in `v0.16.0` but is fixed on main (`1f7d8df78`). `ee why-not` rejects rule ids at HEAD. | PARTIAL | `bd-vp087` (P1) |
| R8 | **Verification is fragmented and partly untestable.** There are 80 per-feature workflows. 44 publish to main (`contents: write` or `git push`), and 38 apply patches or assert blob hashes at runtime. Most of their latest runs are red, and the tested tree is often not a main tree. The proof capsule emitter never populates (`manifestHash: None`). There is no list of required test IDs; `verify-budget.toml` lists 119 stages, not tests. | PARTIAL / proof hole | `bd-fy92m` (P0), `.5`, `.19` |
| R9 | **Pack admits off-topic memories when the budget allows.** "prepare release" packed all 9 memories, including "The office espresso machine needs descaling" (relevance 0.47, floor 0). An unrelated search returned 9/9 hits, correctly flagged `weak`. | PARTIAL | `.11`, `.12` |
| R10 | **Stack boundary.** Local ACL-push PPR still runs in production through `ee graph suggest-links` (`src/cli/mod.rs:36410`); production pack PPR correctly degrades. FrankenNetworkX 0.3.0 still has no personalization. Local cosine ranks semantic spans in `ask` (`src/core/ask_semantic.rs:262-279`), and causal ancestry uses a local BFS (`src/graph/causal.rs:401`). | PARTIAL | `.4`, `.15` |
| R11 | **Ingest throughput.** `ee import cass --limit 3` took 54 s, about 18 s per session. README claims 4.1 s p50 for `--limit 50`. | NOT MEETING CLAIM | NO_BEAD at audit → `.48` |
| R12 | **Small truth defects.** `ee eval list` exits 2 outside the ee repo (`Fixture directory does not exist: tests/fixtures/eval`). `graph_feature_disabled` is still emitted for the `graph.feature.hits_profiles.enabled=false` config case. 414 empty `.potion-multilingual-128M-download-*` staging dirs have leaked into the model root. `pack_assembly_elapsed_over_budget` fires on every pack because model load counts as assembly time. `review_stopword`/kind detection use substring heuristics. The over-broad instruction-risk phrase `curl` (Medium) quarantines any span containing "curl" (`policy/mod.rs:1533-1538`). | MINOR / PARTIAL | NO_BEAD at audit → `.49` (curl), `.52`–`.56` |
| R13 | **Every write rebuilds the whole index (regression).** Incremental intake shipped in June (`bd-d67os.6`, `.7`, closed 2026-06-18, ADR 0078). On 2026-08-06, `a968f7f44`/`a7b486d63` made the incremental apply paths `#[cfg(test)]` (`src/core/index.rs:3220-3300`) when cancellation-safe staged publication landed. Production now labels single writes `single_document_as_full_rebuild` (`index.rs:3988`). Measured on v0.16.0: `remember` costs 1.7–2.6 s on 10 memories but 6.5–6.8 s on 983 indexed documents, and each write adds a full retained copy (R5). Write cost and disk growth are both O(corpus). Frankensearch supports incremental upsert and soft-delete on both tiers, so this is an ee wiring choice. | REGRESSED (closed bead, lost property) | NO_BEAD at audit → `.57` |
| R14 | **About 1.1 s Model2Vec cold start in every process.** A JSON trace of one search shows a 1.17 s gap between "model verification receipt accepted" and "Model2Vec model loaded". Every search, pack, ask and remember process pays it (the receipt only skips re-hashing). Harnesses call `ee` many times per session, so this is the floor of every interaction. Static embeddings are a lookup table, so an mmap'd vocabulary plus a pre-compiled tokenizer cache should load in tens of milliseconds. The fix belongs upstream in Frankensearch. | NOT MEETING CLAIM | NO_BEAD at audit → `.61` (upstream) |

Fixed per-command cost is unchanged in kind. `remember` takes 1.7–2.6 s, because it loads the model and publishes a full index generation synchronously. `init` takes 2.1 s, and search/pack about 1.25 s on 10 memories, of which about 1.1 s is model load. This remains `.6`, `.26` and `bd-search-warm-latency-0bh05`.

### 16.4 Answers to the five reality-check questions

1. **Working now:** everything in §16.2. Concurrent determinism, complete
   small-store recovery, cited `ask`, and coherent posture were all `PARTIAL`
   or `UNPROVEN` on 2026-09-04.
2. **Not working:** R1–R14. The decisive ones are R2, R3 and R4. Together they
   mean a user who follows the README's headline flow ("mines your existing
   cass corpus") gets a slow store that packs JSON scaffolding and proposes
   nonsense rules.
3. **Blocking:**
   - **No real-shape oracle.** Every acceptance fixture is clean and small, so
     nothing executes the product on the data it exists for.
   - **No canonical proof of main.** Full CI has been off for five weeks, and
     delivery workflows publish trees that differ from what they tested. As a
     result, main can be red (R6) while 80 workflows run.
   - **Effort concentrated on hardening already-working paths.** Since 09-17,
     backup has had 78 commits and ask 79, while first contact with real data
     was never measured.
4. **Would closing every open bead close the gap? No.**
   - R1, R2, R3, R6, R11, R13 (a regression behind closed `bd-d67os.6`/`.7`), R14 and most of R12 had no owner.
   - R4's root cause and R5's severity were not owned. They are now `.64`/`.47` and `.42`.
   - §8 row 4 ("CASS … WORKING") and row 9 rest on clean-fixture proofs, so
     the checklist itself overstated the state.
5. **Vision goals with no bead:**
   - Useful CASS mining at realistic scale (README *What You Get*, *CASS
     Integration*; COMPREHENSIVE_PLAN §15; North Stars 1, 3, 4 and 7, which
     need imported-session content an agent can read).
   - Learn that yields reviewable rules from real sessions (Five Core Jobs #4;
     North Stars 3 and 7).
   - Interactive latency once CASS history is imported (README *Quick Example*:
     "fast enough to use before ordinary agent work").
   - Bounded disk growth under ordinary writes (Product Principles: derived
     assets are rebuildable and bounded).

### 16.5 Product thesis and outcome metrics

`ee` exists to put the right prior experience in front of an agent, within a
token budget, quickly enough that the agent asks for it every time. Every Track R
item is judged by five outcome metrics. Each is measured black-box by the
real-corpus gate (`.50`) on the real-shape oracle corpus (`.43`), and, when
opted in, on the developer's own corpus:

| Metric | Definition | v0.16.0 measured | Target |
| --- | --- | --- | --- |
| **UTR** useful-token ratio | Pack tokens that are human-meaningful content (not envelope keys, ids or JSON escapes) ÷ pack tokens | about 0.65 on CASS evidence (35% scaffolding) | ≥ 0.95 |
| **TTFUC** time to first useful context | Wall time for one cold `ee pack` on a store holding the oracle's 5k-span variant | 8.7 s at 4.9k spans | ≤ 2 s cold, ≤ 0.4 s warm |
| **PAP** proposal acceptance precision | Judged-acceptable `review session --propose` candidates ÷ all candidates | 0/10 on one real session | ≥ 0.7 with recall ≥ 0.7 |
| **FAR** false-admission rate | Judged distractors admitted into packs ÷ packed items | 1/9 (espresso) on a 10-memory store; uncalibrated | ≤ 0.05 at the calibrated floor |
| **WCS** write-cost scaling | Slope of log(remember wall) against log(corpus docs) over {1k, 5k, 50k} | O(corpus): 2.0 s → 6.6 s for 10 → 983 docs | slope ≤ 0.15 (near-constant) |

Two invariants follow. They are encoded as tests, so the swarm cannot regress
them silently:
- **I-1 (no read-path re-screening).** Interactive read commands never re-run
  content screening or classification. Admission is decided once per row per
  policy epoch, at write time.
- **I-2 (scale invariance).** For every read command, statement count and rows
  scanned grow at most logarithmically with corpus size. A harness runs each
  read command at 1k, 5k and 50k documents and fails if the log-log slope of
  statements executed exceeds 0.2.

### 16.6 Bridge delta: Track S (stability), Track R (real data), Track T (truth)

These are new children of `bd-reality-core-convergence-1azkt`. Existing owners
keep their scope, and every new P0–P2 child is graph-reachable from `.22`.

**Track S — make main and its proof trustworthy.**
- **S1 `.41` (P0, R6).** Main green on every `CI Static` step:
  - land the facility-cache companion source, or allowlist the file with a reason;
  - fix the formatting drift and the duplicate `#[test]`;
  - add a guard against test files that depend on unlanded delivery-patch symbols.
- **S2 `.42` (P0, R5).** Bounded retained generations:
  - keep the newest K plus every leased generation, garbage-collecting on publish;
  - audited `index vacuum --apply`;
  - one-time migration cleanup;
  - a library-level 1,100-publication test, a 60-write black-box E2E, and a seeded-999 cap-recovery E2E.
- **S3 `.57` (P0, R13).** Restore incremental intake inside the cancellation-safe
  publication model. Build the staged generation as the previous generation plus
  a delta: copy-on-write or hard-linked immutable segment files, Frankensearch
  `VectorIndex::append`/`soft_delete` and `TantivyIndex::index_document`/
  `delete_document` on the staged copy, then validate and publish atomically as
  today. Periodic full rebuild becomes `compact`/`vacuum` maintenance.
  Acceptance: WCS slope ≤ 0.15. Cancellation leaves no partial active
  generation (reuse the existing cancellation tests). Each single-write
  generation's retained delta costs O(delta) bytes, not O(index). Byte-identical
  search results versus a full rebuild of the same corpus on the oracle.

- **S4 `.58` (P0, R8; needs an orchestrator/human decision).** `.5` already
  owns the declarative manifest, exact test inventory and shards, and the
  capsule skeleton. `.58` adds three things:
  1. A test-file inventory drift check in `CI Static`, so a test file
     unreachable from any cargo target reds the one hosted gate that executes.
  2. Per-feature workflows turned into `.5` manifest-shard runners on exact
     main SHAs, with no runtime patching.
  3. A recorded orchestrator or human decision on retiring the 44
     source-publishing workflows; only after that decision are the push steps
     removed.

  `bd-fy92m` keeps the delivery-lane analysis.

**Track R — the five jobs on real data.**
- **R-oracle `.43` (P0).** A real-shape corpus. Schema-faithful, authored
  Claude Code and Codex transcripts with failure→fix arcs, rules, decisions,
  noise, secrets and injection bait. Plus a scale generator (5k/50k/500k) and
  judgments (retrieval, proposals, admission). This is the oracle for all of
  Track R.
- **R-trust `.44` (P1, R1).** Wire the dead `[cass].binary` override into every
  discovery site, with hash-pinned explicit trust, a doctor check, a structured
  `recovery[]` and README coverage. EE-3qgw is preserved.
- **R-project `.45` (P0, R2).** A typed transcript projection, so search, pack,
  ask and learn use role, text and tool fields, not envelopes. Also change the
  *retrieval unit*. One JSONL line (`#L553-553`) is a fragment, so index at
  turn level (user ask plus assistant answer) with exact line-range
  provenance and a projected text budget. Tool calls and results contribute only
  structured metadata (tool name, exit status, shell-parsed redacted command)
  under the derivation policy in §16.6a.
- **R-learn `.46` (P0, R3).** Proposal quality across both existing proposers.
  - The topic-template proposer must stop emitting raw excerpts.
  - The session-arc proposer must stop reading "0 failed" as a failure and must state the actual repair.
  - Confidence comes from corroboration, with abstention and dedup.
  - There is one arc detector, improved in place in `curate_session_arc_sequence.rs`.
  - `bd-2vq2z.9` keeps the linked-pair schema.
- **R-cards `.59` (P1).** Derived **incident cards**: symptom, failed attempt, fix, verifying command, with exact spans, in about 60–120 tokens. They are packable as a typed evidence entity, need an ADR amendment, and depend on `bd-vp087`'s typed pack identity. They serve North Stars 1, 3 and 4 directly.
- **R-fingerprint `.60` (P1).** Feed CASS failure arcs into the existing
  error-fingerprint store (`src/core/error_recall.rs` canonicalizers,
  `src/core/error_diagnosis.rs::record_error_fingerprint`, V072
  `error_fingerprints`) with repair links to the arc's fix spans or incident
  card. Today the store is populated only by a manual
  `ee diagnose-error --record`. After this, `ee diagnose-error "<log>"` and
  `ee pack --error-log "<log>"` recall how the same `(tool, canonical_code)`
  failure was fixed in prior sessions, with exact provenance. North Star 3
  becomes automatic: the repeated CI failure is recognised by its canonical
  code, not by fuzzy text.
- **R-scale hotfix `.64` (P0).** One admission scan per command, cheap rejects first, batch hydration. No contract change, so it is eligible for 0.16.1.
- **R-scale `.47` (P0, R4; extends `.64`).** Materialized admission verdicts
  (I-1) via a migration that preserves the admitted set exactly, a policy epoch, evidence benchmarks, and the I-2
  harness.
- **R-coldstart `.61` (P1, R14, upstream).** Frankensearch Model2Vec cold load ≤
  150 ms on an M-class Mac: mmap the safetensors embedding table, cache a
  compiled tokenizer artifact beside the verified receipt, and keep verification
  semantics intact. Consume it in ee through the sibling-crate release flow.
  Acceptance: a `remember`/`search` cold-process floor (model portion) ≤ 0.2 s,
  measured black-box.
- **R-ingest `.48` (P1, R11).** Profile first, then batch, aiming for ≥ 5×
  throughput. Re-measure README import claims.
- **R-admit `.49` (P1).** Contextual instruction-risk patterns, measured
  false-positive ≤ 5% and false-negative 0% on the oracle's bait set.
- **Probe scaffold `.63` (P0).** The gate's script in REPORT mode, built first so every Track R bead shows its before/after numbers. It records the v0.16.0 baselines.
- **R-gate `.50` (P0).** A black-box real-corpus gate (trust → import → index →
  retrieve → pack → learn → recover → retention) that computes UTR, TTFUC, PAP,
  FAR and WCS. It flips each metric from REPORT to ENFORCE as the metric's owner
  closes. It owns ONE new `verify.sh` "Real-Data Suite" stage, which needs a
  budget ruling because `verify-budget.toml` has no headroom, plus the
  `release-probe` checks.
- **R-dogfood `.62` (P1).** Continuous own-corpus mode. A scheduled local job on
  the developer machine runs the R-gate scenario against the swarm's own CASS
  history using the current candidate binary. It posts a redacted metrics
  summary (counts, latencies, metric values, hashes, never content) as a
  comment on the bridge epic. The swarm's own installed `ee` is still 0.14.2.
  The product's heaviest real user is the swarm, and it is not using the
  product.

**Track T — truth and small defects.**
- **T1 `.51` (P1).** README and AGENTS truth after the fixes: CASS opt-in,
  measured performance, release assets, why/eval scope, Learn wording.
- **T2 `.52`–`.56` (P2/P3).** `why-not` typed ids; `eval list` outside the
  repo; the `graph_hits_profiles_disabled` code; leaked model staging plus a
  precise receipt-stale reason; `pack_assembly_elapsed_over_budget` measuring
  assembly only.

**Reclassify (proof-owed, not code-owed).**
- `.13`: complete small-store coverage in the shipped binary. Remaining is `.14`.
- `.35`: bridge staleness is now Part III-aware.

Both were commented with evidence on 2026-10-01.

### 16.6a Governance and reuse

**Derived incident cards are not memories.** AGENTS.md requires no silent
memory mutation, and curation is how an excerpt becomes durable memory. So
incident cards (R-learn) are **derived, rebuildable evidence entities**:
- they pack as a typed entity under ADR 0085's model (for example
  `entityKind: evidence_card`, its own id, a derivation version, and the exact
  source span ids);
- they are recomputed from admitted spans plus the extractor version, and
  dropped and rebuilt like indexes;
- they are never written as memory rows;
- they become durable only through `review session --propose → curate apply`.

This needs an ADR amendment (0085, or a new ADR) before implementation.
Cards inherit the most restrictive trust and redaction class of their source
spans, and never include text from a quarantined span.

**Derivation policy** (shared by `.45`, `.46`, `.59` and `.60`).

| Quarantine class | What it covers | May feed derivations? |
| --- | --- | --- |
| (A) Kind/role | tool_call, tool_result, metadata and system/developer records. Not indexable for retrieval; already secret-screened at ingest. | Yes, masked and structured only: arc features, shell-parsed redacted commands, Drain-masked error templates. Any derived text that enters search or pack must itself pass instruction-risk screening and redaction. |
| (B) Instruction-risk | Injection-like or medium/high instruction-risk content. | Never. |

**Reuse map (no second implementations).**

| Need | Existing machinery to reuse |
| --- | --- |
| Command-position and pipe-to-shell detection (R-admit) | `parse_shell_command_segments` in `src/core/preflight_guard.rs:2035`, so preflight and admission classify a command identically |
| Incident-card sentence selection under a token budget (R-learn) | pack's facility-location / `submodular` objective |
| Near-duplicate tool output and proposal dedup | `src/search/simhash.rs` (SimHash) |
| Independent-session corroboration (R-learn confidence) | `ee ask` session-aware corroboration grouping (`src/core/ask_candidate_diversity.rs`) |
| Failure-class recall (R-fingerprint) | `error_recall` canonicalizers + `error_fingerprints` store |
| Latency regression decisions (R-scale, R-coldstart, R-gate) | `ee perf compare` / `ee perf budget check` with `ee.perf.v1` artifacts |
| Release-level black-box checks (R-gate) | `release-probe-*.json` (`ee.release_probe.v1`) |
| Message text extraction (R-project) | `message_text` (`src/core/curate_session_arc_text.rs:18`), moved and generalized |

**Hook budget.** Managed SessionStart and pre-edit hooks are where TTFUC
matters most. After R-scale and R-coldstart, the hook path must meet its own
budget (cold ≤ 1 s, warm ≤ 0.3 s) on the 5k-span oracle variant, or degrade
to lexical-only memory recall within budget and say so in `degraded[]`.

### 16.6b Algorithmic spine (round 3)

Each method below was chosen because it gives a stated guarantee or complexity
bound the product needs, stays deterministic, and reuses existing machinery
where it can. None needs an LLM or a paid API.

| Problem | Method | Why this one | Owner | Proof |
| --- | --- | --- | --- | --- |
| Pack and search admit off-topic items (FAR) | **Conformal risk control / Learn-then-Test** (Angelopoulos et al. 2021–22). Choose the smallest admission threshold λ whose upper confidence bound (Hoeffding–Bentkus) on false-admission rate over calibration queries is ≤ α = 0.05 with probability ≥ 1−δ. Persist it as the `calibrationId` that search results already expose. | A finite-sample, distribution-free guarantee on exactly the quantity users feel. The 0.16.0 "heuristic_uncalibrated" label then becomes a real calibration. | `.11`, `.12` (calibration set: `.43` judgments) | Held-out oracle queries keep FAR ≤ α. A recalibration run is byte-reproducible. |
| No calibration yet (fresh store) | **Unsupervised per-query cutoff.** Largest normalized gap in the sorted relevance scores (elbow), or a 2-component mixture split, used only as an explicitly labelled fallback. | Stops "budget fills with tail" on small stores without pretending to be calibrated. | `.11` | On a 10-memory store, the espresso memory is excluded from "prepare release" and the fallback is labelled. |
| Find failure→fix arcs in a session | **PELT change-point detection** (Killick et al. 2012) over per-turn features: error-line density, non-zero exit, test-fail tokens, user-correction cues. BIC penalty. | Exact optimal segmentation, O(n) expected, deterministic. Better for byte-stable output than Bayesian online detection. | `.46` | Every scripted oracle arc boundary is recovered within ±1 turn. |
| Rule confidence that means corroboration | **Beta-Binomial lower bound with a session design effect.** Effective n = n/(1+(m−1)ρ) for m spans per session. Rank candidates by a one-sided **hypergeometric (Fisher) enrichment test**: is the action over-represented in success-terminated arcs versus failure-terminated ones? Apply **Benjamini–Hochberg FDR** at q = 0.1 across a run's candidates. | Span count is not evidence. This gives "the command that actually correlates with fixes", with false discoveries bounded across many candidates. | `.46` | Single-session confidence ≤ 0.6. Monotone in independent sessions. BH keeps planted noise candidates out at the stated q. |
| Incident card in about 100 tokens | **Budgeted submodular maximization**: facility location plus facet coverage (symptom / attempt / fix / verify) under a token knapsack, using cost-benefit greedy plus the best singleton (Lin & Bilmes 2011). | A provable (1−1/e)/2 approximation, and it reuses pack's `submodular` objective. | `.46` | Each card covers all four facets when present. Token budget is respected. Deterministic tie-breaks. |
| Canonical error classes from raw tool output | **Drain log-template mining** (He et al. 2017): fixed-depth parse tree that masks numbers, paths and hashes. Feeds the existing message-template layer of `error_recall`. | O(n), deterministic, and exactly the "message-template" key that ADR 0057's layered fingerprint already reserves. | R-fingerprint | The same rustc or cargo failure across 3 sessions maps to one template and one fingerprint. |
| Near-duplicate tool output and proposals | **MinHash LSH** with b = 10 bands × r = 10 rows (S-curve threshold ≈ 0.79 Jaccard) on top of SimHash for exact-ish dupes. | Bounded false-merge rate at a tunable threshold, and sublinear candidate generation. | `.45`, `.46` | Planted 0.85-similar duplicates collapse. 0.5-similar pairs do not. |
| Write cost O(corpus) and retained-copy bloat | **LSM-style delta generations.** A staged generation = hard-linked immutable base segments (Tantivy segments, FSVI main) + a small delta (Tantivy upsert, FSVI WAL append/soft-delete). Size-tiered compaction when the delta ratio exceeds a threshold. | O(delta) per write, amortized O(log N) merge. Retained generations share base inodes, so retention costs O(delta). This fixes R5's disk growth at the root and keeps the atomic exchange. | S3 (+ `.42`) | WCS slope ≤ 0.15. Retained bytes per write ≈ delta size. Results are byte-identical to a full rebuild. |
| Recovery breadth with bounded retention | **Exponential (grandfather-father-son) retention.** Keep generations at publication distances 1, 2, 4, 8, …, so O(log n) retained still covers long recovery windows. | Bounded disk with no loss of the snapshot-bounded recovery feature. | `.42` | Retained count stays ≤ ⌈log2 n⌉+K over 1,100 writes. |
| Statement count grows with corpus (I-2) | **Empirical complexity regression**: OLS slope of log(statements) and log(wall) against log(N) for N ∈ {1k, 5k, 50k}, with a bootstrap CI. Fail if the CI lower bound > 0.2. Statement count is the primary signal because it is deterministic. | Catches the whole class (any new full scan) rather than one instance. | `.47` | A planted per-search full scan fails the gate (negative control). |
| Perf regressions versus noise | **Mann–Whitney U + Hodges–Lehmann shift** with a minimum effect of 10%, a bootstrap CI on the p50 and p95 ratios, and SPRT to stop sampling early. | Honest regression calls on 10–30 noisy samples. | `.47`, R-coldstart, `.6` | A 2× synthetic slowdown is flagged. A same-binary rerun is not. |
| 1.1 s tokenizer and model load per process | **mmap'd embedding table + a minimal perfect hash** (for example PtrHash) over the 500k-token vocabulary, compiled once and cached beside the verified receipt. | O(1) lookups with near-zero load time, and no 18 MB JSON parse per process. | R-coldstart (Frankensearch) | Model portion of a cold `search` ≤ 0.2 s. Vectors are byte-identical to the current embedder. |
| Admission precision | **Asymmetric-cost operating point on a ROC** over interpretable structural features (shell-parse command position, pipe-to-shell, imperative mood, role). Fixed weights are frozen in code with a test. Hard constraint FNR = 0, minimize FPR. | Transparent, deterministic, and measured instead of guessed phrase lists. | `.49` | Oracle bait FPR ≤ 5%, FNR 0%. |

### 16.7 Order and release steering

```
.41 main green ─┬─► .58 inventory drift + shard runners (+ .5) ─────────────────────────────┐
                └─► .65 0.16.1 hotfix ◄── .42 retention, .44 cass trust, .64 read-path hotfix │
.43 oracle ─┬─► .63 probe scaffold (REPORT mode, v0.16.0 baselines) ─► .62 dogfood         │
            ├─► .45 projection ─► .46 proposal quality ─┬─► .59 incident cards (+ bd-vp087) ├─► .50 gate (ENFORCE) ─► .19 ─► .27/.20 ─► .21 ─► .22
            │                                           └─► .60 CASS error recall           │
            ├─► .47 evidence scale (needs .64) ; .57 incremental intake (needs .42) ────────┤
            ├─► .48 ingest, .49 admission ──────────────────────────────────────────────────┤
            └─► .12 quality thresholds (calibration half of judgments)                      │
.61 cold start (Frankensearch upstream) ────────────────────────────────────────────────────┘
.51 docs after .44/.45/.46/.47/.48/.57/.61 ; .52–.56 any time.
```

Ready now (`br ready`): `.41`, `.42`, `.43`, `.44`, `.61`, `.64` (all P0/P1) and the small `.52`–`.56`.

Release steering:
- Users on 0.16.0 carry the retention time bomb (R5) and the dead config opt-in
  (R1). A **0.16.1 hotfix**, gated by `.41` and the existing release probe plus
  the S2 E2E, is worth cutting before the larger contract-changing Track R work
  (projection changes pack content and hashes, so it ships as 0.17.0).
- Publication remains the `.20` human authority boundary.

### 16.8 Beads, ambition and refinement record

The workflow ran in the skill's order, applied in place, with no competing plan
or epic created:
- **Phase 1 (reality check).** The black-box v0.16.0 probe, a source audit of
  every probe failure (three read-only investigation agents), static gates, and
  tracker coverage.
- **Phase 2 (bridge).** Sections 16.1–16.4 plus the first bridge delta.
- **Phase 3a (frozen bead prompt).** Baseline children `.41`–`.56` (16 beads),
  36 blocking edges, and dated evidence comments on `.13`, `.35` (both
  proof-owed signals), `.4` (local-algorithm census), `.12` (off-topic
  admission repro), `.16`, `.19`, `bd-vp087`, `bd-fy92m` and
  `bd-search-warm-latency-0bh05`, plus the epic root.
- **Phase 4 (three ambition rounds, revised in place).**
  - Round 1: outcome metrics (UTR / TTFUC / PAP / FAR / WCS), invariants I-1
    and I-2, turn-level retrieval units, the R13 incremental-intake
    regression, R14 cold start, the 0.16.1 hotfix, and dogfooding.
  - Round 2: S4 one proof of main, R-fingerprint, incident-card governance,
    the reuse map, and the hook budget.
  - Round 3: the algorithmic spine in §16.6b.
- **Phase 3a again.** `.57`–`.62` were created, and existing children were
  revised with ambition content.
- **Phase 5 (frozen refinement prompt), seven passes:**

| Pass | Finding → change |
| --- | --- |
| 1 | `.42` acceptance contradicted its own retention policy; 1,100 black-box writes would take ≥ 37 min, so tests were split into library-level, small black-box and seeded-999 variants. The oracle got a calibration/held-out judgment split and a cass discovery layout. A shared **derivation policy** separates kind/role quarantine (tool records may feed masked derivations) from instruction-risk quarantine (never feeds anything). `.47` materialized verdicts gained a migration-equivalence guarantee, so evidence cannot vanish on upgrade, plus a policy epoch. `.50` gained profiles and pinned cass provisioning. `.44` gained effect, help-prelude, schema and fixture obligations. |
| 2 | `br ready` hides blocked beads, so the gate would have been built last. Split out `.63` (report-mode probe scaffold, depends only on `.43`) and `.64` (no-contract-change read-path hotfix), and filed `.65` (0.16.1 candidate, human authorization required). `.62` was re-pointed from `.50` to `.63`. |
| 3 | Non-vacuity. Every absence or bound assertion (`.42`, `.45`, `.46`, `.47`, `.49`, `.57`, `.59`, `.62`, `.64`) is paired with a presence or liveness assertion, so empty output cannot pass. |
| 4 | `verify-budget.toml` has no headroom and the unmeasured-stage allowance is a ratchet. All Track R E2E scripts therefore run under ONE "Real-Data Suite" stage owned by `.50`, after a recorded budget ruling. Unnamed E2E scripts were named. |
| 5 | Duplication audit. `.58` overlapped `.5`, so it was re-scoped to the inventory drift check, shard runners and the publishing-workflow decision. `.46` overlapped `bd-2vq2z.9` and the existing session-arc code, so it was re-scoped to proposal quality across both proposers, improving the detector in place. The real-session evidence showed the arc path misreading "21 passed, 0 failed" as a failure. |
| 6 | `.57` gets a copy fallback where hard links are unsupported (ExFAT/FAT/network filesystems; the dev Mac's external drive is ExFAT). `.44` accepts `cass.exe` on Windows. `.65` release notes cover existing stores near the 1,000-generation cap. `.46`'s title was updated. |
| 7 | No further material change. Stop condition met. |

**Final validation.**
- 25 new children (`.41`–`.65`); 4,792 tracker records.
- `br dep cycles` reports 0 active cycles.
- `bv --robot-triage` ranks `.5` and `.43` among the top picks, and `bv --robot-plan --label reality-check` names `.43` the highest-impact unblocker.
- Every P0–P2 child except `.65` is reachable from `.22`. `.65` (a release) and the P3 cosmetic items `.53`, `.54` and `.56` are children only, by design.

**What closing these beads would and would not mean.** Closing every new
child closes the real-data gap only if `.50`'s metrics are in ENFORCE mode
and green on a candidate from a green main (`.41`/`.58` → `.19`). Bead count,
report-mode numbers, or green unit tests on clean fixtures do not meet that
bar. That is the exact failure this pass found.

---

## 17. 2026-10-22 reality check: Track R landed; the instruments did not

**Verdict.** The real-data work of the last three weeks worked. On the data
`ee` exists for, the shipped `v0.18.0` now does what the README says: it mines
a CASS corpus, packs readable incident text inside a token budget, proposes
usable rules, and answers with citations — fast, at 4k spans, with bounded
disk. Five of the six headline 2026-10-01 failures are fixed and two of the
five §16.5 outcome metrics meet their targets.

What did not improve is the project's ability to *see* any of that. The gate
that measures Track R cannot run on the developer's platform and reports zeros
for a product that retrieves correctly. Hosted verification has produced no
verdict in fourteen days and ~994 non-tracker commits, for a configuration
reason nobody had diagnosed. And one new product defect lets a fully populated
store answer every query with nothing while `status`, `index status` and
`doctor` all report it healthy.

That is a different failure shape again. 2026-08 was "the loops are open".
2026-10-01 was "the loops break on real data". 2026-10-22 is **"the loops
work and nothing can prove it"** — three independent measurement surfaces are
broken, and each one fails in the direction that hides good news and would hide
bad news equally.

### 17.1 Evidence authority

| Item | Value |
| --- | --- |
| Source HEAD | `2bad70949` (2026-10-22 22:51:50 -0400), `v0.18.0-940-g2bad70949`, package `0.18.0`. 1,892 commits since the 2026-10-01 pass (`552f7ba4f`), 994 of them not tracker syncs; top scopes `index` 76, `cass` 70, `e2e` 52, `ci` 44, `recall` 40, `learn` 37 |
| Shipped binary | public `v0.18.0` `ee-aarch64-apple-darwin.tar.xz`, SHA-256 verified **OK** against its published `.sha256`. `ee version --json` attests clean `969f0cbf19a5fdaa367328350c937cf566063efc`, tag `v0.18.0`, release profile, `aarch64-apple-darwin`, and the full franken-stack version list |
| Probe isolation | private `HOME` / `XDG_*` roots under `/private/tmp`. **Physical paths only**: on macOS `/var` and `/tmp` are symlinks and ee's CASS path guard correctly refuses a binary path with a symlink component |
| CASS provisioning | the `.43` emulator (`scripts/real_shape_corpus/cass_fixture.py`, every line prefixed `stub`). **Real `cass` could not be used: this host has no cass index** — `cass sessions` exits 3 `missing-db`, "Run 'cass index --full'". So no claim here rests on real transcripts; it rests on the committed real-shape oracle |
| Oracle corpus | `tests/fixtures/real_shape_cass`, 311 files, graded `judgments.json` with distractors, secret bait and injection bait. Served 308 sessions / 4,009 spans; ee admitted 988 and quarantined 3,021 (`tool_call` 1,434, `tool_result` 1,434, `metadata` 90, `record_projection:unavailable` 63) |
| Tracker | 4,918 records: 4,611 closed, 255 open, 33 blocked, 16 in progress, 3 deferred. Bridge epic: 68 records — 18 closed, 40 open, 8 in progress, 2 blocked |
| Hosted CI | **Actions is `enabled: false` repo-wide.** Last run of any workflow: 2026-10-08T22:58Z (`CI Static`, failure). 88 workflow files, 47 carrying `contents: write` or `git push`, 31 applying patches or asserting blob hashes at runtime |
| Distribution | GitHub `v0.18.0` (2026-10-14), crates.io `0.18.0`, Homebrew formula `0.18.0` — all coherent. 7 `.minisig` signatures (v0.16.0 had none). No Sigstore, SLSA, or manifest asset. **No `release-probe-*.json` asset on v0.17.0 or v0.18.0**, although v0.16.0 published one |

Static gates at HEAD, all green locally: `cargo fmt --check`; forbidden-deps
(root and fuzz trees); vision coverage 144 surfaces, 0 stubbed, 0 missing, 0%
gap; contract-drift radar ok (43 docs, 241 schemas, 606 documented codes
against 608 fixtures, 0 violations); verification-drift 0; closure lint 231
beads, 0 violations, 86 baseline matches; bridge staleness now names Part III
correctly and emits one low `vision_coverage_gap_low`; plan drift 0 warnings.

They still prove presence, not behavior — and §17.3 N1 is a defect that every
one of them passes over.

### 17.2 What works, measured on the shipped binary

1. **Walking skeleton.** `init` (1.34 s) → `remember` ×10 → `search` → `pack`
   → `why` → `status` → `index status` → `capabilities` → `doctor`, all exit 0
   with v2 envelopes. `why` resolves the persisted pack selection in 0.079 s.
2. **Two outcome metrics now meet their targets.**

   | Metric | v0.16.0 (2026-10-01) | v0.18.0 (2026-10-22) | Target | Verdict |
   | --- | --- | --- | --- | --- |
   | **UTR** | ~0.65 | **1.0000** (29 evidence blocks, 5,255 content bytes, zero JSON keys / uuids / message ids / escapes) | ≥ 0.95 | **MET** |
   | **PAP** | 0/10 | **precision 1.00, recall 0.833** (5 proposals, 0 polluted by `must_not_propose` substrings) | ≥ 0.70 / ≥ 0.70 | **MET** |
   | **TTFUC** | 8.7 s at 4.9k spans | **1.14 s** pack at 4,009 spans (0.35 s p50 on the 123-record base corpus) | ≤ 2 s cold | **MET at this scale** |
   | **WCS** | 2.0 s → 6.6 s for 10 → 983 docs | `remember` 0.92–1.07 s at 10 memories, **1.24–1.33 s at 4,009 spans** (log-log slope ≈ 0.04) | slope ≤ 0.15 | **MET at this scale; 50k point unmeasured** |
   | **FAR** | 1/9, uncalibrated | **1/10, uncalibrated** | ≤ 0.05 at a calibrated floor | **unchanged — see N4** |

3. **The CASS loop produces readable incidents.** Evidence now arrives as
   projected transcript text, not envelopes: `assistant: Published vault 1.2.0.
   Lesson for next time: bump the version in Cargo.toml and commit it before
   running cargo publish; crates.io rejects a…`. A persisted `reader_text`
   column, `src/db/evidence_reader_projection.rs`, and `TRANSCRIPT_PROJECTION_VERSION = 2`
   back it; `src/search/mod.rs:1098` and `src/core/context.rs:15437` both
   consume `span.reader_text()`. R2 is fixed in the surfaces that matter.
4. **Scale cliff gone.** At 4,009 spans / 988 admitted evidence / 308 sessions:
   search 0.83–1.19 s (was 11.1 s), pack 1.14 s (was 8.7 s), status 0.37 s (was
   3.0 s), `ask` 0.89 s. Import 0.603 s per session (was ~18 s), about 30×.
5. **Retention is bounded.** 2 retained generations after 11 writes, and still
   2 after an explicit rebuild — against one full index copy per write and a
   hard failure at 1,000 in v0.16.0. `index_retention_gc` keeps the newest 2
   valid generations, `reclaim_after_publish` runs inside the exclusive
   generation fence, and `ee index vacuum --apply` is audited. Store size 54 MB
   for the 4k-span corpus.
6. **CASS trust is wired and honest.** The `[cass] binary` override is live at
   all three former dead sites (`src/cli/mod.rs:24501`, `src/core/status.rs:1395`,
   and `src/output/mod.rs`, which no longer discovers on its own) and it
   **executes**. A standard `~/.local/bin/cass` is still refused by default, but
   the refusal now names the path, both opt-ins, and carries
   `repairKind: "actionable"`.
7. **`ask` is honest and useful.** A direct hit answers with the exact span
   cited at confidence 0.678 and `confidenceCalibration.status =
   "heuristic_uncalibrated"`, `calibrationId: null`. An unrelated question
   abstains (`no_confident_answer`, confidence 0.218). Oracle rates:
   negative-abstain 1.00, negative-query flag rate 1.00. A distractor search
   returns results but flags `weak_query_recall`.
8. **Three §15 archive criteria closed.** `bd-3k1mg`, `bd-16imy` and
   `bd-3h6bz` are all closed. `.43` closed today on executed evidence with a
   mutation control and a non-vacuous determinism verdict.

### 17.3 What does not work

| # | Finding (measured) | Status | Owner |
| --- | --- | --- | --- |
| **N1** | **A populated store returns zero results while every health surface reports it healthy.** Reproduced three times from a clean root: import the oracle under the **default** model-download policy, then `ee search` returns **0 results** with `index_incompatible` and `ee pack` **0 items**, while `ee index status` reports `health: ready`, `degradationCode: null`, 988 evidence + 308 session documents, equal generations (4368/4368) and equal corpus revision; `ee status` reports `posture.overall: ok`; and **`ee doctor` reports `posture: ok` with zero non-ok checks**. The search-side degraded entry is correct and actionable ("the stored vector producer identity cannot be verified against the active embedder", repair `ee index rebuild --workspace .`) and both `ee index reembed` and `ee index rebuild` restore full retrieval (10 results, `degraded: []`). The trigger is the documented default path: with no model present, `ee` builds the index with `deterministic_hash`, then acquires and verifies the real 507 MB model during the same run, after which the existing index is no longer compatible. Setting `EE_EMBED_DOWNLOAD=off` is coherent and returns results with an honest `embed_model_unavailable`; the **default** is the broken case. A memory-only store is unaffected, so this is specific to CASS-populated stores. Adjacent to closed `bd-fresh-workspace-hash-fallback-kvltg` and outside the coherence test `ready_index_posture_is_coherent_across_public_cli_surfaces` that closed §B. | **NEW — total retrieval loss, silent to every health surface** | NO_BEAD at audit → `.66` (P0) |
| **N2** | **The Track R measuring instrument is broken three ways and publishes false reds.** `scripts/real_shape_corpus/probe.py` (`.63`): (a) it allocates its root with `tempfile.mkdtemp()`, which on macOS yields `/var/folders/…`; `/var` is a symlink, so ee's CASS path guard refuses the emulator and the probe dies at the import step — **the identical defect was diagnosed and fixed for `tests/no_mocks_e2e.rs` in §A on 2026-09-01 and not carried over**; (b) its harness reports only `stderr`, while `ee` writes error envelopes to **stdout**, so the failure message is the empty string `"ee import cass failed (5): "` and the real reason is only in `import.json`; (c) its model passthrough sets **`EE_MODEL_DIR`**, which appears **0 times** in `src/config/env_registry.rs` and `docs/env_vars.md` — the registered name is `EE_EMBED_MODEL_DIR`. Run as shipped against v0.18.0 it reports `UTR: null`, `FAR: null`, `recall_at_5: 0.0` across 40 judged queries and 0 packed items. With a physical `TMPDIR` and a real model dir the same binary and corpus return 10 results per query and UTR 1.0000. **Every Track R metric in §16.5 is gated on this script, which is why no Track R bead carries a before/after number three weeks on.** | **NEW — false-red gate; (c) is R1's dead-config shape reproduced inside the gate** | NO_BEAD at audit → `.67` (P0) |
| **N3** | **Hosted verification has been dark for 14 days, for a configuration reason.** Actions is `enabled: false` repo-wide; the last run of anything was 2026-10-08T22:58Z and its root cause is **not** code: `##[error]The actions actions/checkout@v4, dtolnay/rust-toolchain@nightly, and embarkstudios/cargo-deny-action@… are not allowed … because all actions must be from a repository owned by Dicklesworthstone`. An allowed-actions policy restricted to owner-owned actions fails every job before checkout. ~994 non-tracker commits have landed with no hosted verdict. `.41` closed legitimately on a specific green (`CI Static` run 37056106739 on `c211215a7`, 2026-10-02) — that green is now 20 days and ~994 commits stale and cannot be re-established while the policy stands. | **NEW root cause on an owned gap** | `bd-actions-disabled-repo-wide-33mfm` (P0, open) + `bd-479c6`; policy cause recorded by this pass |
| **N4** | **Relevance does not discriminate at the tail, so FAR cannot be fixed by a threshold.** "prepare release" on a 10-memory store packs **10/10** items, `omittedCount: 0`, at **4.2%** budget utilization — so nothing was forced in by budget pressure. The office-espresso distractor ranks **4th** with relevance **0.1906**, inside a band of 0.1743–0.1936 that also contains "clippy warnings are errors" (0.1848) and "forbidden dependencies" (0.1821). The separation between a distractor and a genuinely relevant rule is 0.006. `.11`'s conformal-risk plan chooses a threshold λ; **no λ separates that band**, so a discriminative score is a prerequisite, not a consequence, of calibration. | **UNCHANGED (1/10 vs 1/9), sharper diagnosis** | `.11`, `.12`; evidence comment added |
| **N5** | **Published releases carry no black-box evidence.** `.7.1` closed 2026-09-23 delivering `scripts/release_binary_probe.sh`, and `ci-static.yml:515` runs it `--self-test`. v0.16.0 published a `release-probe-aarch64-apple-darwin.json` (12 checks, `verdict: pass`); **v0.17.0 and v0.18.0 publish none.** The gate exists and is self-tested; nothing in either release proves it ran against the artifact users install. | **NEW regression in release evidence** | NO_BEAD at audit → `.68` (P1) |
| **N6** | **Two releases shipped past an open authorization boundary.** v0.17.0 (2026-10-03) and v0.18.0 (2026-10-14) were published while `.20` ("human-authorize publication only after verified private staging"), `.27` (signed/attested release) and `.65` (the 0.16.1 hotfix candidate those two overtook) are all still open. §13.5 says an open publication bead is not authorization. The releases are good — coherent across three channels, signed with minisign, self-attesting their commit — which is exactly why the gap is procedural rather than harmful, and why `.20`/`.27` need to be reconciled to what actually happens rather than left as unmet gates. | **NEW process gap** | `.20`, `.27`, `.65`; reconcile or demote |
| **N7** | **ee discards a precise upstream repair.** When `cass sessions` fails with `kind: "missing-db"` and the upstream hint "Run 'cass index --full' to create the database", ee reports `repair: "run cass health --json"` with `repairKind: "unknown"`, dropping the one instruction that fixes it. The structured `subprocessDiagnostics` are good and the DB path is correctly redacted. | **NEW small truth defect** | NO_BEAD at audit → `.69` (P2) |
| **N8** | **Delivery-lane topology grew.** 88 workflow files (was 80), **47** carrying `contents: write` or `git push` (was 44), 31 patching or hash-asserting at runtime (was 38). All 88 are currently inert because of N3, so `.58`'s "turn them into manifest-shard runners" cannot be validated until Actions returns. | **WORSE** | `.58`, `bd-fy92m`, `bd-5glwp` |
| **N9** | **Unchanged small defects, each re-measured.** 414 leaked `.potion-multilingual-128M-download-*` staging dirs in the real model root — **exactly the 2026-10-01 count** (`.55`). `ee eval list` outside the repo still errors `Fixture directory does not exist: tests/fixtures/eval` with the repair "Ensure tests/fixtures/eval/ exists" (`.53`). `pack_assembly_elapsed_over_budget` still fires on an ordinary successful 29-item pack (`.56`, in progress). Local `cosine` still ranks semantic spans in `src/core/ask_semantic.rs:332`, local BFS still walks causal ancestry in `src/graph/causal.rs:390`, and PPR signals still surface through `graph suggest-links` (`.4`; `.15` remains blocked on upstream personalization). `single_document_as_full_rebuild` is still the label for single-document intake (`src/core/index.rs:4485`), so `.57`'s wiring is unlanded even though wall-clock write cost no longer scales with the corpus at this size. | **UNCHANGED** | `.53`–`.57`, `.4`, `.15` |

### 17.4 Answers to the five reality-check questions

1. **Working now:** everything in §17.2. Four of five outcome metrics meet
   their targets at the measured scale, three §15 criteria closed, and the
   product does the job the README advertises on the corpus it advertises.
2. **Not working:** N1–N9. The decisive ones are N1 (a healthy-looking store
   that answers nothing) and N2 (the gate that would have caught N1 reports
   zeros for everything, so a real zero and a harness zero are indistinguishable).
3. **Blocking:**
   - **Measurement, not implementation.** Three independent surfaces that are
     supposed to tell the truth about the product cannot: the Track R probe
     (N2), hosted CI (N3), and `doctor`/`status`/`index status` for the N1
     condition. The implementation outran all three.
   - **No discriminative relevance.** N4 means `.11`/`.12` cannot be closed by
     calibration alone.
   - **The oracle is served by an emulator here.** The host has no cass index
     (`cass sessions` → `missing-db`), so nothing in this pass measured real
     transcripts. `.62`'s premise is now literal: the product's heaviest
     potential user cannot use it without `cass index --full`.
4. **Would closing every open bead close the gap? No.** N1, N2, N5 and N7 had
   no owner at audit, and N1 is the kind of defect that would ship: it is
   invisible to `fmt`, forbidden-deps, vision coverage (144/144, 0% gap),
   contract-drift, closure lint, `status`, and `doctor`. N3's root cause was
   unrecorded on its own P0 bead. §8 rows 4, 5, 9, 20 and 23 were also stale in
   the pessimistic direction and are revised below.
5. **Vision goals with no bead:** retrieval that is *provably* non-empty on a
   store the product itself reports healthy (README *Graceful Degradation*:
   "Each degradation surfaces in the JSON `degraded` array with a repair
   command" — true per-response, false on the three health surfaces); and a
   measurement instrument that fails closed rather than reporting zero
   (AGENTS.md *Control Markers*, "constant-green, constant-red, and cannot-pass
   assertions are the same defect"). Both are now `.66` and `.67`.

### 17.5 Bridge delta — Track V (make the instruments tell the truth)

New children of `bd-reality-core-convergence-1azkt`, all graph-reachable from
`.22`. Existing owners keep their scope; no competing plan or epic was created.

- **V0 `.70` (P0, the generalization).** Ambition round 1 asked what N1, N2 and
  N3 have in common, and the answer is one defect in three subsystems: **every
  verdict surface in this project can emit a value indistinguishable from the
  healthy case without saying which.** `doctor` renders an unevaluated dimension
  as `posture: ok`; the probe renders a dead harness as `recall_at_5: 0.0`; a CI
  conclusion renders a policy refusal as `failure`. So the standing property is
  a **tri-state verdict contract** — *measured healthy*, *measured unhealthy*,
  *could not measure* — with a code-derived census of every health, gate and
  metric surface, a declared third state for each, a cargo-free gate that fails
  when a surface cannot express it, and a terminator classification for verdicts
  that come from a subprocess or a remote lane. AGENTS.md already names the
  class in the abstract (bd-2phcw: constant-green, constant-red and cannot-pass
  are the same defect); this makes it enumerable. `.66` and `.67` land first and
  become its positive control; its negative control is a legitimately two-state
  surface that must NOT be flagged, so the gate cannot become noise and get
  exempted wholesale. No new posture layer — the third state lives inside the
  existing `posture` / `degraded[]` / capability contracts.
- **V1 `.66` (P0, N1).** One coherent readiness answer, and a `doctor` check
  that sees `index_incompatible`. `ee search`, `ee pack`, `ee index status`,
  `ee status` and `ee doctor` must not be able to report mutually exclusive
  readiness for one snapshot; the condition must appear as a non-ok `doctor`
  check carrying the working `ee index rebuild` repair; and acquiring the model
  mid-run must either re-stamp the index or declare the store degraded at every
  surface. Acceptance is adversarial and non-vacuous: a positive control that
  reproduces the zero-result state and fails before the fix, a negative control
  on a memory-only store, and an explicit assertion that `doctor` has at least
  one non-ok check whenever retrieval returns zero on a non-empty index.
- **V2 `.67` (P0, N2).** Make the probe scaffold fail closed. Canonicalize its
  root to a physical path (reuse the §A trusted-target-root resolution rather
  than re-deriving it), report `stdout` envelopes in harness errors because
  that is where `ee` writes them, use `EE_EMBED_MODEL_DIR`, and add a
  self-test that asserts a *known-good* corpus produces non-zero recall — so a
  harness fault can never be published as a product zero. `.63` depends on this.
- **V3 `.68` (P1, N5).** Publish the release probe's capture as a release asset
  for every tag, and make a missing capture fail the release rather than the
  release succeed silently. Reuses `scripts/release_binary_probe.sh`.
- **V4 `.69` (P2, N7).** Carry the upstream repair when a subprocess supplies
  one, instead of substituting a generic hint.
- **N3** is recorded on `bd-actions-disabled-repo-wide-33mfm` with the policy
  error text and the three rejected actions; the fix is an allowed-actions
  policy decision (allow the three, or vendor them), not code.
- **N4** is recorded on `.11`/`.12` with the measured 0.1743–0.1936 band, and
  ambition round 3 adds a **feasibility precondition** before any λ is fitted:
  compute the ROC of the relevance projection over the `.43` oracle's judged
  relevant-versus-distractor items and report AUC plus the *minimum achievable
  FAR at the target recall*. If that minimum exceeds α, **no threshold
  satisfies the target** and the work is upstream of calibration — better
  features, a different fusion, or reranking — so the calibration step must
  fail with that explicit reason instead of emitting a λ. The judgments,
  distractor labels (`distract_espresso`, `distract_lunch`) and
  `negative_queries` already exist in
  `tests/fixtures/real_shape_cass/judgments.json`, so this is cheap and it turns
  "calibration is pending" into a decidable question. Sequence the unsupervised
  elbow fallback first: on the measured distribution the largest normalized gap
  falls between rank 2 (0.3968) and rank 3 (0.1936), which cuts the "prepare
  release" pack to exactly the two correct items.
- **N6** is recorded on `.20`/`.27`: reconcile the authorization gate with the
  hand-cut release practice, or demote the claim.

### 17.6 Order and steering

```
.66 coherent readiness + doctor check ──┬─► .70 tri-state verdict census + gate ──┐
.67 probe fails closed ─────────────────┴─► .63 report-mode baselines ─► .50 (ENFORCE) ──┤
bd-actions-disabled-repo-wide-33mfm (policy decision) ──► .58 shard runners ──► .19 ─────┼─► .20/.27 ─► .21 ─► .22
.11 ROC feasibility ─► .11 elbow ─► .11 λ ─► .12 quality thresholds ─────────────────────┘
.68 release-probe asset ; .69 upstream repair ; .53–.57 any time
```

`.66` is deliberately **not** blocked on `.10`. Refinement pass 3 removed that
edge: `.10` owns a general source-attested retrieval/index oracle, and gating a
user-facing P0 behind it is the dependency deadlock this plan warns about (the
same reason `.5` was allowed to proceed while the product was red). `.66`'s own
acceptance requires a positive control that fails at the parent commit, which is
the source attribution for this defect, and that control becomes an input to
`.10` rather than waiting on it.

### 17.7 Beads, ambition and refinement record

The skill ran in its documented order, applied in place, with no competing plan
or epic created. Both frozen prompts are retained verbatim on the root bead.

- **Phase 3a.** `.66`–`.69` filed with blocking edges to `.22`, `.19` and `.63`.
- **Phase 4, three ambition rounds.** Round 1 produced `.70` (the tri-state
  verdict contract above). Round 2 hardened it with a code-derived enumeration,
  an empty-world guard, and a negative control for legitimately two-state
  surfaces. Round 3 produced the `.11`/`.12` ROC feasibility precondition.
- **Phase 5, five refinement passes.**

| Pass | Finding → change |
| --- | --- |
| 1 | `.66`'s own invariant was wrong as first written: "doctor must be non-ok whenever retrieval returns zero against a non-empty index" would fire on every legitimate no-match query and turn `doctor` red for any unanswerable question — the noise-then-blanket-exemption failure that `degraded[]` nearly suffered before E5. Narrowed to key on an index/embedder-caused degradation rather than on the absence of results. |
| 2 | Both new E2E scripts must emit `ee.test_event.v1` per step with stdout/stderr separation, as every other E2E in §12 does; otherwise a future CI failure is as unreadable as the probe failure this pass fixed. |
| 3 | Removed the `.66 → .10` edge as over-coupling (see §17.6). |
| 4 | Paired every absence assertion with a presence assertion — the exact 988 admitted-evidence count and at least one exact span id — so an empty corpus or a dead harness cannot pass. That is precisely how N2 passed. |
| 5 | Scope: no new posture layer, no new report family, and the honest `EE_EMBED_DOWNLOAD=off` degradation path explicitly preserved. No further material change; stop condition met. |

**Final validation.** 4,923 tracker records. Five new children (`.66`–`.70`),
all graph-reachable from `.22`. `br dep cycles` reports **0 active cycles**.
`bv --robot-triage` completes and ranks `.5`, `.15` and `.18` as its top picks.
Dated evidence comments were added to `.4`, `.11`, `.12`, `.20`, `.27`, `.42`,
`.44`–`.48`, `.53`–`.57`, `.63`–`.65`, `bd-xayfm` and
`bd-actions-disabled-repo-wide-33mfm`. **No pre-existing issue changed status
and nothing was closed by this pass.** Cargo was not run locally (tripwire) and
no RCH dispatch was launched; every product claim above is a black-box
measurement of the published v0.18.0 archive.

### 17.8 Steering

Steering: **fix the two instruments before funding more product work.** N1 is
the proof that the current gate set cannot see a total retrieval failure, and
N2 is the proof that the one gate aimed at real data would have reported the
same zeros whether the product worked or not. Both are small, bounded, and
they are what makes every later number believable. The product work of the
last three weeks does not need more ambition; it needs a measurement it can be
graded by.

## 18. 2026-10-23 execution pass: the instrument now works, and it found things

Not a new reality check. This is the execution record for the steering in
§17.8, kept here because §17's findings are only honest while their
dispositions are visible beside them. Same evidence authority as §17.1 — the
sha256-verified shipped v0.18.0 archive, black-box, isolated `HOME`/`XDG_*`
roots on physical paths — plus, for the first time in this bridge, **numbers
produced by the project's own instrument rather than by a hand-rolled probe**.

### 18.1 N2 is closed, and the first real baselines exist

`.67` closed on an execution at `ce034a12b`. `scripts/real_shape_corpus/probe.py`
now carries all four required outcomes with both directions of every control:
`--self-test` runs 24 binary-free arms and 28 with a binary. The repaired probe
produced the oracle-scale v0.18.0 baselines `.63` had been blocked on for three
weeks (308 sessions / 4,009 spans / 988 admitted / 3,021 quarantined):

| §16.5 metric | measured | target | verdict |
| --- | --- | --- | --- |
| UTR | **0.9998** | ≥ 0.95 | MET |
| TTFUC (pack p50) | **0.571 s** (max 0.759 s) | ≤ 2 s cold | MET |
| PAP | **1.00 precision / 0.833 recall**, 5 proposals, 0 polluted | ≥ 0.70 both | MET |
| FAR | **0.0** over 335 packed items | ≤ 0.05 | MET |
| WCS slope | **0.437** over 502 → 4,009 records | ≤ 0.15 | **MISSED** |

Supporting readings: search p50 0.253 s; import 0.171 s/session (but see the
correction in §18.7 — the 30× import claim in §17.2 does NOT survive
instrument measurement); negative-query
flag rate 1.0; `ask` negative-abstain rate 1.0; `scenario_hit_at_5` 0.925.

**WCS is the one new red, and it is the first measurement of invariant I-2 on
real shape.** `remember` p50 went 0.372 s at 502 records to 0.923 s at 4,009 —
an 8× corpus for a 2.5× write cost, slope 0.437 against a ≤ 0.15 target. Two
points only, so the figure is directional rather than final; a third point is
needed before it is quoted as the verdict. Note it supersedes nothing: §17's
≈0.04 was measured from 10 memories to 4,009 spans, a range dominated by fixed
cost, and over that range the slope genuinely is flat. The 502 → 4,009 band is
where the growth lives.

**Two readings on this corpus must never be quoted without their reason.**
`recall_at_5` fell 0.4583 → 0.0375 → 0.0 across 502 → 4,009 → 12,009 records on
a product that retrieved correctly the whole way up, because the oracle reaches
scale by seeding copies of each judged scenario while judgments label only the
authored core. `scenario_hit_at_5` is the scale-invariant form of the same
question and stayed 1.0 / 0.925. **Anyone promoting `.50` to ENFORCE must gate
on `scenario_hit_at_5`, not `recall_at_5`.** This cost a correction to the
probe's own fail-closed guard (`cb246f91e`): as first shipped it keyed on
`recall_at_5` and therefore false-fired at 12,009 records — the constant-red
half of `bd-2phcw`, reintroduced inside the gate written against it. The arm
that now pins it is a twin that must NOT fire.

**And the honest limit on every number above: they are the LEXICAL
configuration.** The default download policy cannot be measured, because it
returns zero results (N1). The only embedding configuration in which ee's own
outcome metrics can be measured today is the one that turns the model off.

### 18.2 N1 is attributed to exact source, and deliberately not yet fixed

Reproduced exactly — `init` → `import cass` → `search`, nothing written between,
so all five surfaces observe one immutable snapshot: search 0 results with
`index_incompatible`; `ee index status` health **READY**, `degradationCode`
null, `repairHint` null, generations equal (139/139), 60 docs; `ee status`
`posture.overall` **OK**; `ee doctor --full` **posture OK** with
`search_index = ok "Search index is ready"`.

The chain, with lines: a hash-tier build records `storedModelId`,
`storedDimension`, `storedModelRevision` and `storedModelHash` as **null**;
`embedding_space_compatibility_error` (`src/core/index.rs:5936`) opens
`let (stored_model_id, stored_dimension) = stored?;` and so returns
*compatible* whenever the stored side declares nothing — its doc comment states
the assumption ("a lexical/hash-tier index legitimately carries no
fingerprint") that frankensearch contradicts by binding a producer revision
regardless of tier; `determine_health` therefore receives
`compatibility_error = false`; and `check_search_index`
(`src/core/doctor.rs:3616`) maps `IndexHealth` over only four states, so
`Ready → CheckResult::ok` and doctor has no finding and no repair to offer. The
decisive identity lives in frankensearch's own index header
(`index.embedder_revision()` vs `query.identity_fingerprint()`), which **no
readiness surface consults**.

Two corrections to §17's N1 row. Doctor does not report "zero non-ok checks" —
it reports posture OK with five warnings, and the relevant one,
`embedding_posture`, **misdiagnoses** this store as a quality degradation
("deterministic-hash fallback … Explicit memories remain available through
lexical or anchored retrieval"). Zero are. So the condition is not merely
unreported; it is reported as something an agent would reasonably act on
differently.

Recommended fix recorded on `.66`: resolve a **workspace-scoped** embedder stack
in the readiness path and compare fingerprints, never touching the process
global — the mechanism `src/core/index.rs:18557` already uses for exactly this
divergence. Not implemented in this pass: it changes the readiness contract,
needs a degraded-code fixture plus its `docs/degraded_code_taxonomy.md` row in
the same commit, and will move goldens. None of that could be graded locally.

### 18.3 The unification: one divergence, three symptoms

N1 is not a reporting bug on its own. **Which embedder a process resolves
depends on which code path asked first, and nothing binds the answer per
snapshot.** Measured on one store at one instant: `index status` reports
`semantic false / source frankensearch_hash_fallback / available_model_count 0`
while a complete, `.verified` 489 MB potion model sits in the model root and
`search`, in its own process, resolves a producer that rejects the stored
vectors.

Three symptoms, same fact:

1. **`.66`** — the readiness surfaces resolve the non-semantic side, so they
   see no conflict and report healthy.
2. **`.56`** — the model-load subtraction added in `cf8c95030` (which **did**
   ship in v0.18.0) reads the process-global `DEFAULT_SEARCH_EMBEDDER_LOAD`
   OnceLock on both halves of its guard. On a store whose model comes from the
   workspace registry that global is never initialised, so it subtracts **zero**
   and `pack_assembly_elapsed_over_budget` still fires on **6 of 6** packs
   (assembly 894/564/550/553/629/633 ms against a 500 ms threshold). The proof
   is arithmetic: the cold pack paid ~330 ms more than the warm ones and its
   reported assembly rose by the same ~330 ms, which cannot happen if the load
   is being subtracted. Control: with the model absent the warning fires on
   2 of 40, and 0 of 20 at 502 records — the threshold is not the problem.
   Raised P3 → P2, not for latency (contents are unaffected and the message
   says so) but because a warning on 100% of the primary command's invocations
   trains agents to ignore `degraded[]`, which is the channel the honesty
   contract runs on.
3. **`bd-3k1mg` / `bd-qf3l4`** (closed) — the same divergence broke coalesced
   drains with a `producer_revision` mismatch, and was repaired **pointwise** at
   one call site rather than as a property.

This already has an owning P0 — **`.2`, "make index publication and embedder
selection deterministic across CLI processes"**, whose acceptance includes
"equal snapshots choose one backend/model and semantic result across
processes". No new bead was filed; the evidence went onto `.2` and `.66`/`.56`
are linked to it as `related`. Its EVIDENCE STATUS block previously rested on a
provenance-less 0.14.2 binary; the divergence now has a reproduction on a
binary with a known commit plus two downstream symptoms carrying numbers.

### 18.4 bd-betm5: the Learn loop's last link

Three conjuncts govern gap coverage — not tombstoned, `created_at >
cluster.last_seen_at`, and carrying the exact `knowledge-gap:<clusterId>` tag.
Executed with the new third diagnostic in `scripts/e2e_memory_debt.sh`
(27 steps, this bead's the only failure):

- **tag conjunct: satisfied.** `coverTags` is literally
  `["docking","knowledge-gap:gap_80bd12d26846daceb312"]` against a cluster
  whose `clusterId` is `gap_80bd12d26846daceb312`.
- **recency conjunct: satisfied, and the suspected mechanism is eliminated.**
  The worry was that the second `ee learn gaps` re-observes the misses and
  advances the window past the answer. It does not: `lastSeenAt` is
  byte-identical before and after (`2026-10-23T05:11:53.826655000+00:00`) with
  `scannedMissCount` 3 both times, and the remember landed ~1 s later.
- **so the cause on that binary is that the predicate is not in it.**
  `5c3392996` ("let hash-only gaps close via explicit coverage tags") landed
  **2026-10-14 18:14, eight hours after the v0.18.0 release commit**, and
  `GAP_COVERAGE_TAG_PREFIX` exists only in it and its test commit. The shipped
  binary proves it from outside: its `suggestedCommand` carries **no `--tags`
  at all**, so an agent following the product's own suggestion could not bind
  an answer to a gap even in principle.

**The trap that hid this, and it generalises.** The suite's staleness guard
passed and printed `binary_version=0.18.0 source_version=0.18.0`, which reads
like an attestation. It is semver equality, not commit identity, and the two
sides differed by the one commit implementing the feature under test — exactly
what its own header warns about (`bd-smxdr`). **Any measurement in this bridge
must name the commit, not the version.**

**The dispatched HEAD verdict came back and the pre-registered prediction held.
`bd-betm5` is CLOSED.** Base `7f765483b`, `BUILD_EXIT=0`, `E2E_EXIT=0`, remote
exit 0 in 32m25s, `remembered_demand_marks_cluster_covered: pass` with 12 of 12
labels green, graded against a binary that job compiled. The pass predicate
requires `status == "likely_covered"` **and** `coveredBy != null`, so the
coverage link is established.

So the shape of that bead was: real when filed against 0.17.0, fixed
2026-10-14, open for nine days because no verdict existed. **The second instance
of §18.5 confirmed by execution today.**

`a55b8c75e` fixes the reason the literal `coveredBy` id was unavailable: the
suite printed a rich diagnosis on the FAIL path and *nothing* on the PASS path,
so the one value the bead's acceptance asked for was the one value no passing
run emitted. A `[witness]` line now prints it beside the pass event — not
through `event()`'s third argument, which lands in `first_failure_diagnosis`, a
field other suites read as "why it failed". Both directions were observed before
committing, since v0.18.0 cannot take the pass path locally.

The `createdAt` hole is now **`bd-h1f81`** (P2) rather than a loose note: ee's
own suggested command tells an agent to write a memory whose `created_at` must
beat a timestamp ee publishes, and then does not report the `created_at` it
wrote. Four validity-window fields are exposed and the row's own creation time
is not.

**One process lesson from the aborted first dispatch, which is mine and not the
tooling's.** It synced `ce034a12b` and spent 33 minutes to report
`BUILD_EXIT=101` with the `read_fence` duplicate-initializer errors — i.e. it
rediscovered §18.5's broken-main window the expensive way. Two guards worth
keeping: run `git merge-base --is-ancestor <last-compile-fix> HEAD` *before*
dispatching, and never read a dispatch's fate from the wrapper's exit status —
I ended the command with `; echo "dispatch exit=$?"`, which masked a remote
exit of 90 as a local 0. Read `BUILD_EXIT`/`E2E_EXIT` out of the log.

Smaller finding, separately actionable: `ee remember --json` returns 39 data
fields — `audit_id`, `confidence`, `revision_number`, `valid_from`, `valid_to`,
`version`, `tags` and more — and **no `createdAt` in either spelling**. So the
timestamp the recency conjunct turns on is not observable from the write that
is supposed to close the gap, and neither an agent nor a test can confirm that
conjunct from outside the process.

### 18.5 The pattern this pass exposes, which is bigger than any one bead

Three of the beads examined today were **already fixed in source and still
open**, because the only artifact anyone can measure is a published binary that
predates the fix: `.56`'s subtraction shipped and is inert, `bd-betm5`'s
predicate landed eight hours after the tag, and `.44`/R1's config override was
fixed and verified only by source reading. With hosted Actions off
(`bd-actions-disabled-repo-wide-33mfm`), cargo tripwired locally, and — until
`ce034a12b` — the real-corpus probe constant-red, **the project's default
outcome for a correct fix is that it stays open.** That is the same shape as
§17's N1/N2 one level up: the work is not the bottleneck, the verdict is.

Concretely, two cheap changes would move this more than any single product fix:
decide the Actions allowed-actions policy (configuration, not code), and give
the repaired probe a home in the gate set once `bd-2di61` settles the
gated-stage exclusion. Until then every claim in this bridge rests on one
developer's laptop and a hand-verified archive.

### 18.6 Dispositions

| §17 finding | 2026-10-23 disposition |
| --- | --- |
| N1 readiness incoherence | attributed to exact source; `.66` holds the recommended fix; linked to `.2` |
| N2 probe false reds | **CLOSED** (`.67`, `ce034a12b`); baselines captured; guard sentinel corrected (`cb246f91e`) |
| N3 Actions disabled | unchanged; now identified as the single highest-leverage non-code decision (§18.5) |
| N4 FAR / relevance band | **FAR measured 0.0** over 335 items on the oracle; the λ-feasibility precondition on `.11`/`.12` stands |
| N5 release-probe asset | unchanged (`.68`) |
| N6 open beads across releases | sharpened: §18.5 explains the mechanism, not just the count |
| N7 upstream repair hint | unchanged (`.69`) |
| N8 workflow sprawl | unchanged |
| N9 leaked staging dirs | unchanged; the probe no longer adds to it (controls use auto-cleaned roots) |

### 18.7 One instrument, both releases — and a correction to §17.2

With the probe repaired, **v0.16.0 and v0.18.0 were measured by the same
instrument on the same corpus** (the `.43` oracle at 308 sessions / 4,009 spans,
both archives sha256-verified against their published `.sha256`). Until now
every Track R delta in this bridge compared a hand-measurement against a
different hand-measurement. These are comparable:

| metric | v0.16.0 | v0.18.0 | target |
| --- | --- | --- | --- |
| UTR | 0.4994 | **0.9998** | ≥ 0.95 |
| TTFUC pack p50 | 4.102 s | **0.571 s** | ≤ 2 s |
| pack max | 14.232 s | 0.759 s | — |
| search p50 | 3.766 s | 0.253 s | — |
| PAP precision / recall | 0.0 / 0.0 | **1.00 / 0.833** | ≥ 0.70 |
| PAP proposals | 11, none acceptable | 5, all acceptable | — |
| FAR | 0.0 | 0.0 | ≤ 0.05 |
| packed items | 678 | 335 | — |
| `ask` answered / cited-relevant | 0.0 / 0.0 | 0.625 / 0.625 | — |
| negative-query flag rate | 0.333 | 1.0 | — |
| `remember` p50 | 3.624 s | 0.923 s | — |
| evidence admission | *surface absent* | 988 admitted / 3,021 quarantined | — |

Two readings deserve stating. **Packed items halved while UTR doubled** — the
pack got smaller and better at once, which is §16.5's thesis in two numbers. And
**PAP improved by proposing less, not more**: eleven proposals none of which
were acceptable became five that all were, across the same six judged arcs. The
entire remaining 0.167 recall gap is one arc (`rustc-borrow-error`) that now
yields no proposal at all. `ee ask` answering nothing at all on v0.16.0 was
never previously quantified anywhere in this bridge.

**THE CORRECTION.** §17.2 and the `ce034a12b` commit message report import at
"~18 s/session → 0.603 s/session, about 30×". **That does not survive
instrument measurement.** On this corpus the instrument puts v0.16.0 at
0.213 s/session and v0.18.0 at 0.171 s/session — **1.25×, not 30×**. The two
figures are not comparable rather than one being fabricated: the 18 s/session
came from a configuration with the embedding model ACTIVE, where import pays
per-span embedding cost, and both runs above have `EE_EMBED_DOWNLOAD=off`, which
removes that cost from both sides. So the defensible claim is narrower, and it
should replace the old one wherever quoted:

> With embedding disabled, import throughput improved 1.25× between v0.16.0 and
> v0.18.0. The large import speedup attributed to Track R is a property of the
> **embedding** path and has **not** been reproduced by this instrument, because
> the only configuration the instrument can measure is the one with embedding
> off (`.66`).

That is a genuine hole in the Track R evidence, not a bookkeeping nit: the
headline import number cannot be re-derived until `.66` makes the default policy
measurable. It is the second place in this bridge where `.66` is the thing
standing between a claim and its proof, the first being every §16.5 metric in
§18.1.

`.63` is consequently **2 of 4 acceptance items done, 1 partial, 1 not done** —
both published versions measured, HEAD not (no dispatch was launched for it),
and the `real_corpus_cass_unavailable` code its self-test bullet names is not
actually emitted by the probe, which is the same dead-code shape `.67` was filed
for and must be either emitted or restated.

### 18.8 The default backup level is not recovery-grade for evidence, and nothing says so

**Heading corrected twice, and the second correction overturns my own
conclusion.** This section first said "a restore reports success while denying
95% of a store's evidence", then "the backup is written already 95% denied". The
*measurements* in both were right. **Both attributions were wrong: the denial is
intentional.** The settling evidence and the narrowed defects are at the end of
this section — read that before quoting anything above it.

`.13`'s ninth acceptance bullet asks to "replace planned-only coverage with
executed current-candidate evidence". Executed today, create → verify → restore
on the shipped v0.18.0, and it found a **data-safety defect in the recovery
path**:

| surface | source | restored |
| --- | --- | --- |
| `dbEvidenceCount` | 123 | 123 |
| `dbEvidenceAdmittedCount` | **46** | **6** |
| `dbEvidenceQuarantinedCount` | 77 | 0 |
| `dbEvidenceDeniedCount` | 0 | **117** |
| `indexDocumentCounts.evidence` | 46 | 6 |
| `dbGeneration` | 139 | 139 |

The restore reported `status: "completed"`, `degraded: []`, `issues: 0`,
`cassSessionsRestored: 14`, `evidenceSpansRestored: 123`. So the span **count**
round-trips perfectly and the admission **verdicts** do not. Two things make it
worse than a count mismatch. It is a **severity escalation**: the source's 77
*quarantined* spans return as *denied*, along with 40 of the 46 *admitted* —
quarantine is reviewable, denied is terminal. And **it does not heal**: `search`,
`pack` and a full `ee index rebuild` against the restored store leave it at
6 admitted / 117 denied, so this is not a lazy rebuild catching up on first use.

Control: the same procedure on a memory-only store restores 3 of 3 memories with
full fidelity, so the defect is specific to evidence admission, not to restore.

The manifest's own inventory shows the fault line. `evidence_spans` is
`export_restore_required` and comes back intact; **every table that decides
whether a span may be packed** — `evidence_reader_projections` (123 rows),
`evidence_admission_verdicts` (46), `evidence_admission_counts` (2),
`evidence_admission_count_state` (1) — is `derived_rebuildable /
rebuild_on_restore`. Since R2 established that search and pack read
`reader_text` from `evidence_reader_projections`, the likely chain is that those
projections are not actually rebuilt, admission cannot read text it cannot
project, and so it denies. The confirming measurement is the projection row
count in the restored store, which this pass had no public surface for — so the
mechanism is a hypothesis while the behaviour above is measured.

**This is the same class as `.56` and `.66`:** something classified as
rebuildable whose rebuild does not happen, with no surface reporting the gap. It
is the third instance recorded on `.2` today, and the first where the
consequence is lost user data rather than a wrong number.

Also found, smaller: `counts.issues` does not reach `degraded[]` in either
direction — the restore that lost nothing reported `issues: 3`, the restore that
denied 117 spans reported `issues: 0`, and neither produced a degraded entry.

**THE HYPOTHESIS ABOVE IS REFUTED, and the real finding is worse.** The
pre-registered test — count the projections in the restored store — was run
with `sqlite3 "file:<db>?immutable=1"`:

```
source    evidence_spans 123 | evidence_reader_projections 123 | verdicts 46
restored  evidence_spans 123 | evidence_reader_projections 123 | verdicts  6
```

Projections are **123 in both**. They restore perfectly. So the loss is not a
missing rebuild, and `pack_eligibility` is a column on `evidence_spans`, which
is `export_restore_required` — carried by the artifact, not rebuilt. Reading the
artifact itself:

| | admitted | quarantined | denied |
| --- | --- | --- | --- |
| source DB `evidence_spans.pack_eligibility` | 46 | 77 | 0 |
| exported `derived/cass/evidence-spans-0000.json` | **6** | — | **117** |
| same manifest, `recoveryInventory` verdict rowCount | **46** | — | — |

**One backup artifact contains two mutually inconsistent records of the same
fact**, and the restore is faithful to the wrong one. Re-creating the backup
from the unchanged store reproduces 6/117 exactly, so it is deterministic, not a
timing artifact. The 6 it keeps are a true subset of the DB's 46, and the 46 are
indistinguishable on `producer_kind` (`cass_import`), `instruction_risk`
(`none`) and `search_eligibility` (`admitted`) — the one clean separator is the
excerpt's shape: all 40 dropped spans begin `{"parentUuid":…` (ordinary
conversation messages) while the 6 kept are summary/timestamp records. Note also
that `evidence_admission_verdicts` has `CHECK (verdict IN ('admitted','denied'))`
— **no `quarantined`** — so a three-state column is being round-tripped through
a two-state channel, which accounts for the 77 exactly.

So the correct framing is: **`ee backup create` writes a backup whose evidence
is already 95% denied, and reports success.** The damage is baked into the
artifact operators keep and ship, which is why nothing on the restore side could
recover it. It is not confined to disaster recovery either — any handoff,
migration or shared-corpus workflow that round-trips a store loses 95% of its
packable evidence, at `--redaction minimal`, the gentlest setting.

### 18.8.1 Settled: the denial is intentional, and two narrower defects remain

**Both attributions above are wrong.** `redact_evidence`
(`src/core/backup_evidence_export.rs:60-118`) returns early for
`RedactionLevel::None` and otherwise ends:

```rust
if excerpt != row.excerpt || !provenance_admitted || auxiliary_changed {
    …
    row.search_eligibility = "denied".to_owned();
    row.pack_eligibility  = "denied".to_owned();
}
```

So **if the export redacted the excerpt, it denies the span** — defensible, since
a redacted excerpt is not the evidence. Swept across all five levels on one
store at one instant:

| `--redaction` | artifact `packEligibility` | `secretRedactionStatus` |
| --- | --- | --- |
| `none` | **admitted 46 / quarantined 77** | redacted 109 / clean 14 |
| `minimal` *(default)* | denied 117 / admitted 6 | redacted 117 / clean 6 |
| `standard` | denied 117 / admitted 6 | redacted 117 / clean 6 |
| `strict` | denied 123 | redacted 123 |
| `paranoid` | denied 123 | redacted 123 |
| *source DB* | *admitted 46 / quarantined 77* | — |

`--redaction none` round-trips eligibility **exactly**, including the
`quarantined` state — which also kills the "two-state channel" theory above
(that was reasoning from the `evidence_admission_verdicts` CHECK constraint
instead of measuring). Denial tracks redaction one-for-one at every level. And
note the `none` row: 109 spans were *already* redacted upstream by import-time
secret screening and keep their eligibility, so the export correctly
distinguishes "redacted upstream" from "redacted by me".

**What is still a defect, narrowed to three things:**

1. **The manifest contradicts its own artifact.** At `minimal` the
   `recoveryInventory` reports `evidence_admission_verdicts rowCount 46` while
   the span artifact in the same file holds 6 admitted. One describes the source
   store, the other what was exported, and nothing distinguishes them. Not a
   design decision — an inconsistency inside a single file, detectable with no
   second store.
2. **Nothing says the backup is not recovery-grade for evidence.** At the
   default level `backup create` reports `status: "completed"`, `degraded: []`,
   `issues: 0`, and restore reports `evidenceSpansRestored: 123`. All true, all
   reading as full fidelity, with no signal that 117 of 123 spans will be
   unpackable and that `ee index rebuild` cannot recover them. `degraded[]`
   exists for exactly this, and the response *was* affected.
3. **The default is the lossy one.** `--redaction` defaults to `minimal` while
   this bead's goal is "lossless, privacy-preserving recovery". Those are in
   real tension for evidence excerpts and the default silently picks privacy;
   the configuration that meets the goal is neither the default nor signposted.

Cheapest arms: emit a degraded entry naming the counts and the
`--redaction none` repair; assert the manifest's internal invariant; and pin
both measured properties — that `none` reproduces the source histogram exactly,
and that at `minimal` the denied count equals the redacted count, since that
equality is what makes the denial defensible rather than arbitrary.

**Method note on myself.** Three write-ups, two wrong attributions, and the same
move corrected both: stop reasoning about the mechanism and measure one more
variable. The projection count killed version 1; the five-level sweep killed
version 2. Each was one command, and `--redaction` was visible in
`backup create --help` the entire time. The instrument was fine here — the
theory kept outrunning the data.

What is genuinely good and retires part of `.13`'s PROBLEM statement: the
dry-run non-mutation contract **passes**, measured with the control that makes
it non-vacuous (two pure filesystem snapshots with nothing between are
identical; a read-only `ee audit timeline` moves `.ee/ee.write.lock`; `backup
create --dry-run` moves *nothing at all*, so it perturbs the store less than a
read does). The `db.check_integrity` dry-run failure in the PROBLEM statement
does not reproduce. The durable-vs-rebuildable inventory exists and is good —
108 tables, each with `owner`, `disposition`, `coverage`, `rowCount` and
`schemaCovered`. And export is understated rather than incomplete: all 14
sessions and all 123 spans ship under `derived/`, not in `records.jsonl`.

### 18.9 Two P0 test suites executed for the first time, and the runbook was inside the test

The dispatch lane costs ~32 minutes and is idle most of the time, so the highest
-leverage use of it is grading suites that nobody has ever run. Two P0 beads own
test files named after themselves. Both files compile, both are mounted into
live shard targets, and neither had a recorded execution.

**Finding: the test target names are not the file names.** A first dispatch
failed instantly with `error: no test target named 'index_retention_e2e' in
default-run packages`. Files under `tests/` are `#[path]`-mounted into shard
targets in `tests/suites/`, so a test's real id is `<file_stem>::<fn>` and the
target is the shard. Six test files are named for still-open beads; all six need
the shard+module-prefix form, not their own name.

#### `.42` (retained index generations): 3/3 green, three of six acceptance bullets

`--test integration_g_m -- index_retention_e2e::`, announced 3, passed 3,
failed 0, reconciled, one `Running` line, one summary, 132.33s. The arms are
real product behaviour, not self-tests: `vacuum` previews 5 planted stale
copies and deletes none, `--apply` reclaims exactly those 5 and writes an audit
row, a second `--apply` reclaims nothing, and 8 consecutive `remember` calls
each hold retained ≤ 2 **while the live generation strictly advances** — that
second conjunct is what stops the bound being satisfied by publications that
stopped happening.

Not proven, and not rounded up: the write count is 8, not the acceptance's
1,100, so the allocator's 0..1000 ceiling — the bead's actual hard-failure mode
— is untouched. The 28 inline GC unit tests, which are where every negative
bullet lives (never reclaim the active generation, a leased one, live staging,
or a rejected-quarantine dir), have also never been executed; they sit in the
lib target, whose unfiltered run exits 101 on 11 chronic failures, so they need
a module filter to be gradeable at all. And `scripts/e2e_index_retention.sh`,
which the acceptance names, does not exist — it is the only item with nothing
behind it, and also the only one that would produce the per-write BYTES series
the disk-bound claim is about. The integration test counts directories.

#### `.10` (source-attested retrieval oracle): the instrument passes its own calibration, and has never been pointed at anything

`--test integration_n_r -- retrieval_index_regression_oracle::`, announced 55,
passed 53, failed 0, **ignored 2**, reconciled. Read the 2:

    concurrent_retrieval_over_one_generation_is_classified
      ... ignored, "oracle: point it at a candidate deliberately"
    read_only_window_attribution
      ... ignored, "attribution probe for the durable-mutation red; run it deliberately"

The 53 green are `classifier::*`, `attestation::*`, `build_environment::*`,
`model_cell_controls::*` — the oracle's self-tests. They prove the instrument
works, and the module header is explicit that this is deliberate: the live probe
is `#[ignore]` because under swarm load `Inconclusive` is the likely outcome and
a load-dependent red in a shared suite is the manufactured-wrong-answer failure
the design exists to prevent. That reasoning is correct. The consequence is that
the bead's decisive bullet — "reproduce or fail to reproduce the released-binary
divergence; either outcome becomes the implementation baseline" — has no
evidence either way, and a green on 53 calibration tests must not be read as
one. That would be bd-2phcw's three-shapes defect from the other side: a
verdict that does not vary with the property, because the property is never
read.

**The dispatch this bead needed was written into its own refusal path.**
`tests/retrieval_index_regression_oracle.rs:756` — the message `attest_fields`
emits when `ORACLE_REQUIRE_ATTESTATION=1` finds an unattested candidate — spells
out the whole command: which `VERGEN_*` to forward, why `VERGEN_GIT_DIRTY=false`
is honest only under a no-overlay baseline, and that `scripts/rch_verify.sh`
spells the same thing `--treeish <sha> --committed-tree`. Somebody wrote the
runbook into the error text and nobody ran it. Dispatched now on the pinned
lane, which is acceptance bullet 1 verbatim, with `--event-log` for bullet 6 and
a pre-registered reading of all four verdict classes recorded on the bead before
the result returns.

**Mechanical finding worth keeping: `rch exec --job` can never carry a committed
baseline.** `--job` is mutually exclusive with `--clean-overlay` (exit 2) while
`--base`/`--no-overlay` *require* `--clean-overlay` (exit 1). So the lane that
gives you `--result-dir` and a shell body is working-tree-sync only, and cannot
satisfy a source-attestation bullet at all. That is why the test prescribes a
plain `rch exec`; it is not a flag combination to work around. The gradeable
form of two ignored arms is one cargo invocation, because libtest accepts
several filters after `--`.

#### The pattern, now measured twice more

§18.5 said the project's default outcome for a correct fix is that it stays
open, because there is no local cargo, Actions is off repo-wide, and the probe
is red. These two beads sharpen it: the code was written, the tests were
written, the runbook was written, and the only missing step was somebody
spending 32 minutes of an idle lane to turn any of it into a verdict. `.42` was
already carrying two independent readings — a source audit on 2026-10-20 and a
black-box measurement on the shipped v0.18.0 — and still needed a third before
anyone would close it. Writing the test is not the expensive part here.

### 18.10 The arithmetic behind §18.5, and the single action that answers it

§18.5 described the pattern — a correct fix's default outcome here is that it
stays open — as an observation. It is measurable, and the number is larger than
the two beads §18.9 happened to find.

**26 open beads have a test file that names them. Six are P0. The 38 citing
files land in only 14 cargo test targets.** Measured at cf0df2719 by matching
every open/in_progress/blocked bead id against the text of all 531 files under
`tests/`, requiring the character after the id not to continue it — a bare
substring test credits `…1azkt.1` for every citation by `.10`, `.11`, `.13` and
`.18`. The targets concentrate hard: `integration_a_d` alone carries seven
beads, `integration_g_m` five, `integration_s_z` five, `integration_n_r` four.

So one dispatched `cargo test --locked --no-fail-fast --tests` run grades every
one of them, for the price of one build.

**The prior for what that run returns is already in the tracker, and it is
emphatic.** bd-hwye2 enumerated 33 red rows in `integration_s_z`, split them by
a two-host two-base intersection, and re-measured its 16-row Group B at HEAD on
2026-10-17: **15 were already fixed; 1 was real.** Its author's own summary is
"Group B is clear." A 94% stale-red rate in the only sample anybody has taken
is the argument.

Filed as **bd-z368i** (P1), `related` to bd-hwye2 (the prior), bd-lkizl (85
closed beads citing a compile as behavioural proof — the same delivery
mechanism), and to `.10` and `.42`.

What the bead deliberately does NOT claim is that 26 beads become closeable. The
counter-example is `.10` from §18.9: its named target ran, 53 passed, 0 failed,
and the bead correctly stays open because the 53 are the instrument's own
calibration and the two product-pointing arms are `#[ignore]`d. **A test file
citing a bead is not a test proving it.** The deliverable is therefore a
per-bead evidence table with ignored counts reported separately and never
folded into passed, and "its test does not test it" admitted as a third
legitimate outcome beside green and red.

Two grading constraints are load-bearing rather than stylistic, and a
multi-target run is more exposed to both than the single-target runs in §18.9.
`--no-fail-fast` is mandatory, because cargo stops at the first failing target
while the command still reads as the broader check. And `tail -1` is wrong by
construction: several `ee` tests spawn children that emit their own complete
libtest summaries onto the same stream, sometimes before the parent target
finishes, so the summaries must be summed and reconciled against the `running N
tests` announcements per target. `--lib` stays out of the same invocation
entirely — its 11 chronic failures make the run's exit status uninterpretable,
and it needs its own filtered dispatch.

### 18.11 The oracle fired: RACE_REPRODUCED, and the cause is not what the shape suggests

§18.9 reported that `.10`'s instrument had never been pointed at a candidate. It
has now, and it returned a verdict: **`RACE_REPRODUCED`** on `pack.hash`, at an
attested commit. This is the first product classification this bridge has for
the retrieval/index divergence, and `.10`'s gate — "no code fix lands before
this classification" — is discharged.

    base          cf0df2719        gitDirty false   sourceState clean
    target        x86_64-unknown-linux-gnu   profile debug   version 0.18.0
    featureSet    embed-fast,fts5,graph,json,lexical-bm25
    binarySha256  503054924fea…7e7bde2
    franken-stack version-identical to franken-stack.lock, 9/9 declared
                  families, 0 mismatches (checked, not assumed)
    run           announced 2, 1 passed 1 failed, reconciled, one summary, 828s

Pack only. Search was `RACE_ABSENT` in all three rounds with 8/8 probes agreeing
and `results.order` byte-identical between the warm and cold serial reads;
`indexInvisible: []` throughout, so the `.66`-shaped condition did not occur
here. `read_only_window_attribution` PASSED, so the durable-mutation-under-
read-only red does not reproduce either.

**The whole run contains exactly two pack hashes**, and which windows got which
is the finding:

    serial baseline (serialIsFirstTouch)  65aac94e…
    round 0, 8 concurrent probes          65aac94e…   agree
    round 1, 8 concurrent probes          cd61f5a5…   diverge
    round 2, 8 concurrent probes          cd61f5a5…   diverge

So it is **not** a race among simultaneous readers — all eight concurrent probes
agree with each other in both failing rounds — and it is **not**
serial-vs-concurrent either, because round 0 was concurrent and agreed. The
shape is **early vs late**: the first two pack windows produce one hash and
every later one produces the other, stably. Filed as **bd-wubqf** (P0), since
"same workspace → byte-identical context pack hash" is listed in AGENTS.md
under Determinism (non-negotiable), and the failure is a function of how many
packs a workspace has already served — which is how a test that passes in
isolation fails in a suite and makes the suite look like the problem.

**I nearly attributed this to the wrong bead, and the discipline that caught it
was the one §18.8.1 cost three write-ups to learn.** `bd-ve7c6` ("pack hash
binds live runtime measurements: `queue_depth` and `retry_after_ms` are
interpolated into hash-bearing degradation prose") predicts this signature
almost exactly, and it is filed twice over as "found by reading, not by
execution" — so this run looked like its missing proof. The data refuses it: the
hash-bearing degraded set is byte-equal on both sides of the divergence
(`embed_model_unavailable,pack_assembly_elapsed_over_budget` on the serial
baseline and on every concurrent probe), and neither code `bd-ve7c6` names fired
anywhere. An identical canonical input cannot carry a hash difference. That
negative result is recorded on `bd-ve7c6` itself, because the dangerous outcome
was not being wrong for an hour — it was leaving a dispatch on the record that a
later reader would cite as confirmation.

**The cheap discriminator is pre-registered rather than theorised**: re-run the
serial baseline *after* the concurrent rounds. If it reports `cd61f5a5`, the
variable is accumulated state and "race" is the wrong frame entirely; if it
reports `65aac94e`, the concurrent context really is the variable and round 0 is
the anomaly. Nothing else in the captured evidence separates those, and they
need opposite fixes. The oracle already takes a cold serial read and stamps
`serialIsFirstTouch`; it simply never re-baselines at the end.

**Two instrument defects, found by using the instrument.** The final verdict
reports "round 3" and "round 5" when only rounds 0–2 ran — those are flat
indices over the six (round, kind) windows, so a reader who trusts the verdict
looks for rounds that do not exist. And the verdict diffs hashes without diffing
selections: it captured `pack.selection` for the serial baseline and none for
the concurrent probes, so it can say the hashes differ but not what differs,
which is exactly what attribution needs next.

**`.56` got independent confirmation for free**, and in the configuration that
settles it: **25 of 25** pack invocations emitted
`pack_assembly_elapsed_over_budget` — one serial plus 3 × 8 concurrent, at
1.6–2.0 s against a 500 ms budget — while every probe ran
`embed_backend: hash_fallback` with `embed_model_unavailable` and the index
reported `embeddingMode: deterministic_hash`. There was no model to load, so the
alternative story that the warning honestly reports a one-off model load is
closed off: there is nothing to subtract and the warning is still universal.

**`.10` is NOT closed, and that departs from what I pre-registered.** I wrote on
the bead that a semantic red would close it as the baseline. Two acceptance
bullets are unmet: the cold/warm MODEL cells were never exercised (`cell:
"none"` throughout — `ORACLE_MODEL` was deliberately left unset because the
model cells need worker network and "no network is INFRA_ERROR", which would
have bought an unattributable non-verdict instead of this classification), and
bullet 1's pinned-Franken-stack lane is blocked by **bd-zpr8x**. The
franken-stack came back version-identical to the lock, but `rch exec` does not
materialise siblings at lock *revisions*, so the revisions stay unattested.
Saying so beats closing on partial acceptance, which is the pattern the
honesty-only / implements-surface taxonomy exists to prevent.
