# Dependency Upgrade Log

**Date:** 2026-08-11  |  **Project:** eidetic_engine_cli  |  **Language:** Rust (franken-stack sibling pins + crates.io)

Scope requested: franken-stack pins, especially the new FrankenSQLite release.
This repo's primary dependencies are sibling checkouts pinned by commit in
`franken-stack.lock` and wired through `[patch.crates-io]`; registry versions
matter only where a requirement falls outside the patched version line.
Verification is remote-only (RCH pinned bundles) per repo policy.

## Summary
- **Updated:** 5 pins (frankensqlite 0.2.1, sqlmodel_rust 0.3.2, franken_networkx, frankensearch, franken_agent_detection)
- **Unchanged:** toon_rust (pin == tip)
- **Rolled back:** asupersync 0.4.3 (transitively blocked — see below)
- **Registry-wide bumps:** deliberately skipped — local cargo resolves for an older toolchain and rewrites the shared lock with downgrades (observed windows-sys/hashbrown regressions); registry churn belongs to a session on the current toolchain.

## Unit 1 — FrankenSQLite 0.2.1 closure (frankensqlite + sqlmodel_rust)

### frankensqlite: 85f5c488 → 1471829d (version 0.1.19 → 0.2.1 + ExpressionTooDeep fix)
- **Why now:** 0.2.1 published to crates.io; 838 commits ahead of the pin.
  Carries the GLOB dash-range fix (a998b05a), FTS5 porter-stem corrections,
  trigger-depth budget + ExpressionTooDeep mapping (1471829d), and the
  0.2.x engine line ee's sqlmodel side already requests.
- **Mechanics:** ee's `[patch.crates-io]` supplies ONE fsqlite version from
  the sibling path. At 0.1.19 the patch satisfied frankensearch's `0.1.2`
  requirement and sqlmodel's `0.2` fell back to registry 0.2.1 (the dual
  entry visible in Cargo.lock). After this bump the patch satisfies
  sqlmodel — ee's PRIMARY DB engine — and frankensearch's storage engine
  falls back to registry 0.1.x. Same dual closure, sides swapped, with the
  patched side now the one ee's own DB layer runs on.
- **Breaking-change research:** the async VFS/pager + Connection API
  migration sits in this range; ee does not consume fsqlite directly —
  sqlmodel-frankensqlite 0.3.2 (already in ee's Cargo.lock via a peer's
  update) is the adapter built for the 0.2 API. Engine-behavior deltas are
  exactly what bd-022z1 measures; the nested-transaction refusal filed as
  bd-mwsdr may change shape under 0.2.1 savepoints.
- **Tests:** remote pinned-bundle compile gate + targeted DB/engine tests,
  then the standing full-suite re-measure (bd-022z1) picks up the rest.

### sqlmodel_rust: f034a97b → 4b355f05 (workspace 0.3.2)
- **Why:** the coherent adapter revision for fsqlite 0.2; ee's Cargo.lock
  already resolved sqlmodel-core/-frankensqlite 0.3.2, so the pin catches
  the lock up to reality.

## Needs Attention

### frankensearch: pinned 83ef0195, tip d8945ad9 — still on the fsqlite 0.1 line
- fsfs/durability/ops crates at tip request `fsqlite 0.1.2`; the project has
  not migrated to 0.2. Until it does, its storage engine resolves from the
  registry 0.1 line (unpatched). Pin bump to d8945ad9 deferred to its own
  unit so unit-1 blame stays clean.

### asupersync: exact-pinned =0.3.10; sibling tip 0.4.3 — MAJOR, deferred
- 0.3.x is known not to compile on macOS (recheck scheduled at 0.4 per
  session memory). Live diagnostics show peer lanes (mesh transport
  T2.2/T2.3 surfaces) currently mid-churn against asupersync API visibility
  (`request_cx_with_budget` went private). Bumping under them would collide
  with in-flight peer WIP. Needs its own migration pass with the transport
  owners; not attempted here.

## Unit 2 — asupersync =0.3.10 → =0.4.3: ROLLED BACK (transitive wall)

- 0.4.0 is a semver re-anchor of the 0.3.10 API (near-drop-in for ee), and
  the `request_cx_with_budget` privacy diagnostics were stale-analyzer noise
  (public at tip). The wall is transitive, not API-shaped:
  frankensearch requires the fsqlite 0.1 line, and **fsqlite-core 0.1.x
  itself requires asupersync <0.4**. ee cannot hold asupersync =0.4.3 while
  frankensearch (and through it the 0.1 engine) sits in the closure.
- **Unblock path:** frankensearch migrates fsfs/durability/ops to fsqlite
  0.2 (its own porting project) → then asupersync 0.4 clears stack-wide.
- **Forward-prep landed:** fnx-runtime accepts asupersync `>=0.3.4, <0.5`
  (franken_networkx 58fe5d19); frankensearch's ceiling raised to `<0.5`
  with its lock unchanged (f65efa25). Both are no-ops today and remove two
  of the three walls in advance.
- A peer sweep committed the mid-flight 0.4.3 manifest (b7ed4ee6) before
  the wall surfaced; c066cbaa restored the resolvable closure.

## Unit 3 — remaining pins

- **franken_networkx** 7faf0a1b → 58fe5d19: the asupersync-range widening.
- **frankensearch** 83ef0195 → f65efa25: tip + the ceiling widening; ee
  consumes the 0.3.x API either way.
- **franken_agent_detection** 5b0d6498 → 88fc6783: includes its fsqlite-0.2
  async-engine bridge — coherent with unit 1.
- **toon_rust**: pin already at tip; untouched.
- **Verification:** remote pinned-bundle compile gate at the final pin set;
  the standing bd-022z1 full-suite measure covers behavior.

---

# 2026-08-15 — v0.13.1 cut: reconcile manifest, Cargo.lock, and franken-stack.lock

Main had become unbuildable in a clean environment: `Cargo.toml` pinned
`ed25519-dalek =3.0.0` and `getrandom 0.4` (used by the mesh signer code)
while the committed `Cargo.lock` still carried ed25519-dalek 2.2.0 /
getrandom 0.2 and no curve25519-dalek 5 tree, and the crate pinned
`asupersync =0.3.10` while `src/mesh` uses 0.4.4-only APIs. `--locked`
builds and the pinned RCH verify lane both failed.

## Changes
- **asupersync:** `=0.3.10` → `=0.4.4` (deps + dev-deps). The 2026-08-11
  transitive wall (frankensearch storage → registry fsqlite 0.1.x →
  asupersync <0.4) has since cleared: the resolved graph now carries a
  single asupersync 0.4.4 from the sibling path patch, and the registry
  0.3.10 line (with franken-kernel/evidence/decision 0.3.10) dropped out
  of `Cargo.lock` entirely.
- **sqlmodel-core / sqlmodel-frankensqlite:** req `0.3.0` → `0.4.0` to
  match the sibling `harmonize/vlsf2-fsqlite03` branch (workspace 0.4.0,
  fsqlite 0.3 adapter).
- **Cargo.lock:** regenerated coherently — adds the ed25519-dalek 3.0.0 /
  ed25519 3.0.0 / signature 3.0.0 / curve25519-dalek 5.0.0 / fiat-crypto
  0.3.0 tree the manifest already pinned; fsqlite path crates 0.3.0 →
  0.3.2; no other registry churn.
- **franken-stack.lock:** refreshed all 7 pins to the sibling revisions the
  build was actually validated against (all reachable on their GitHub
  remotes; sqlmodel_rust rides the pushed `harmonize/vlsf2-fsqlite03`
  branch head 021bd17a).
- **tests/search_fts5.rs:** frankensearch drift fix — `doc_count()` now
  returns `Result<usize, SearchError>`; unwrap through `map_search_error`.

## Verification (local darwin, isolated CARGO_TARGET_DIR)
- `cargo metadata --locked` clean; `cargo check --all-targets` green.
- Targeted gate: `model_status_contract` (8/10 — the 2 failures are
  pre-existing sibling-drift/environment failures, identical at HEAD~),
  `rerank_posture_contract` 7/7, `search_fts5` 4/4, `--lib model` filter
  855/860. All five GH#26 regression tests pass.
- Known pre-existing failures (unchanged by this cut, sources untouched by
  HEAD~..HEAD): `model_status_auto_declares_bundled_embedding_model`,
  `model_status_picks_first_available_registry_entry`,
  `cli::tests::model_status_and_list_keep_json_degraded_and_toon_envelopes_in_parity`,
  `cli::tests::model_status_and_list_toon_errors_match_json_error_envelopes`,
  `core::model::tests::rerank_model_artifact_read_rejects_length_mismatch_before_hashing`,
  `core::proof_verify::tests::tla_command_uses_sibling_model_config_when_present`,
  `models::jsonl::tests::export_record_union_round_trips_line_delimited_jsonl`.

---

# 2026-09-03 — bd-022z1 Franken-stack convergence

The manifest, resolved lock, pinned source graph, runtime diagnostics, install
audit, and dependency-contract golden had drifted onto different versions.
This unit restores one explicit identity across those surfaces.

## Changes

- **Asupersync:** direct and dev requirements `=0.4.9` → `=0.4.10`; source
  pin `86988e38` → the `v0.4.10` release commit `997e8d11`. The selected
  no-default `tracing-integration` profile is unchanged.
- **FrankenSQLite:** direct requirement and every resolved family member
  `0.3.15` → `0.3.16`; source pin `067d5016` → `a6ae92aa`, which contains
  the `v0.3.16` release plus the prefix-BM25 and WAL tail-index fixes.
- **Frankensearch:** declared requirement `0.4.0` → the already-resolved
  `0.4.2`; source pin `3d8d25ca` → current `main` at `4bd29d44`, including
  the receipt-skip verification fix. Resolved crate versions are unchanged.
- **SQLModel:** declared `sqlmodel-core` and `sqlmodel-frankensqlite` floors
  `0.4.1` → the already-resolved `0.4.2`. Its source stays at the known-good
  `3d79be0b` pin because upstream has no newer release tag and current `main`
  is a large unreleased API/test expansion.
- **Identity surfaces:** dependency doctor matrix revision 4, search manifest
  metadata, install-pipeline publication requirements, Markdown research and
  contract matrices, and the contract golden now report the same versions.

## Verification

Pinned RCH verification is pending on the committed tree. Results will be
recorded here and on `bd-022z1`; no local Cargo command is permitted.
