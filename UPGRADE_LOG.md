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

---

# 2026-09-12 — Release dependency refresh

Registry versions were checked against the official crates.io sparse index.
Pinned sibling repositories and the nightly toolchain remain explicit release
inputs. Upgrades are applied and tested individually before the next upgrade.

- [x] Inventory direct registry dependencies and preserve path dependencies.
- [x] crossbeam-queue 0.3.13 → 0.3.14: 58 writer queue tests passed.
- [x] base64 0.22.1 → 0.23.1: 151 selected regression tests passed.
- [x] fs4 0.13.1 → 1.1.0: 78 lock and secret-store tests passed.
- [x] uuid 1.24.1 → 1.26.1: 34 identifier and runtime tests passed.
- [x] zeroize 1.8.2 → 1.9.0: five backup/recovery tests passed.
- [x] zstd 0.13.3 → 0.14.0: 44 compression/cache and eight integration tests passed.
- [x] toml_edit 0.25.13 → 0.25.15: 75 configuration/profile tests passed.
- [x] Run final check, Clippy, formatting, tests and dependency audit; results
  below distinguish passing gates from retained test and advisory failures.
- [x] Qualify six DSR artifacts, publish v0.15.0, verify all 16 public assets,
  and publish/read back the four-platform Homebrew formula.
- [x] Check crates.io eligibility: six unpublished versions and a missing
  published runtime API prevent registry publication of this source graph.

## crossbeam-queue 0.3.14

The [released changelog](https://github.com/crossbeam-rs/crossbeam/blob/crossbeam-queue-0.3.14/crossbeam-queue/CHANGELOG.md)
changes index width on 32-bit platforms with 64-bit atomics and extends the
upstream MSRV support policy. EE's `ArrayQueue` usage needs no API change.
The dependency list is unchanged; the lock checksum was verified against the
official sparse index. Pinned RCH tests at af09aa6bc passed all 58
`core::write_owner::tests` (no failures or ignored tests) on hz2.

## base64 0.23.1

The [release notes](https://github.com/marshallpierce/rust-base64/blob/069bf7067b949f5c0a92b6ceb82492920502f2c2/RELEASE-NOTES.md)
describe the new default SIMD implementation and error-shape changes. EE uses
the unchanged general-purpose encoding/decoding APIs and does not match the
changed error variant. Default features are disabled and `std` remains enabled;
the scalar implementation is sufficient for EE's cursors and credentials.
Version 0.23.1 was already present in the lockfile for Asupersync. The older
0.22.1 entry remains required by other transitive consumers. Pinned RCH tests
at 4eae0038e passed all 151 selected cursor, query, preflight-token,
deterministic-ID and JSONL regression tests on hz4, with no failures or ignores.

## fs4 1.1.0

The [upstream source](https://github.com/al8n/fs4/tree/df476ee1de2926ae4599607c325a5aa1d334501d)
exports synchronous `FileExt` at the crate root, renames exclusive locking to
`lock`/`try_lock`, and returns `TryLockError::WouldBlock` for contention.
EE retains synchronous-only features and distinguishes contention from I/O
errors in doctor locking. Key rotation still takes an exclusive lock while
approval transactions hold shared locks. Existing contention and release
assertions use the new API without weakening their guarantees. Tantivy retains
its separate fs4 0.13.1 requirement. Pinned RCH tests at c1e15d265 passed
all 78 selected doctor-lock and secret-store tests on vmi1152480, with no
failures or ignored tests. The same compiled test artifact also passed three
CLI checks for contention, audited mutation failure and undoable finish failure.
Final Windows qualification also passed real byte-lock contention followed by
two successful doctor repairs with the previous pointer preserved.

## uuid 1.26.1

The [upstream releases](https://github.com/uuid-rs/uuid/releases) retain the
v7 timestamp and builder APIs EE uses. The root requirement is explicit;
the lock checksum was checked against the official sparse index and its
resolved dependency list is unchanged. EE's deterministic generator already
builds its ordinal payload explicitly, so the upgrade does not rely on
ambient randomness. Pinned RCH tests at fffccf651 passed all 34 selected
identifier and deterministic-runtime tests, with no failures or ignored tests.

## zeroize 1.9.0

The [upstream documentation](https://docs.rs/zeroize/1.9.0/zeroize/)
retains the `Zeroize` and `Zeroizing` APIs. EE explicitly enables `alloc`
for its secret-bearing vectors and strings, instead of relying on feature
unification through another dependency. The exact version and checksum match
the official sparse index. Pinned RCH tests at b25703f47 passed all five
backup/recovery tests in 215 seconds, with no failures or ignores. The same
source passed the PPR duplicate-degradation regression and all-targets Clippy
with `-D warnings` on a separate remote worker.

## Compression and configuration upgrades

- **zstd 0.14.0:** the [release notes](https://github.com/gyscos/zstd-rs/releases/tag/v0.14.0)
  tighten prepared-dictionary lifetimes and fix decoder frame completion.
  EE uses owned bulk dictionaries and stream decoding without prepared
  dictionaries, so these changes require no call-site migration. zstd-safe
  moves to 8.0.0; Tantivy still requires the 0.13/7.x pair. zstd-sys 2.1.0
  removes inappropriate MSVC visibility flags. The new BSD-3-Clause license
  is already allowed. Passing tests cover dictionary training, compressed cache
  round-trips, corrupt inputs and compressed replay ledgers.
- **toml_edit 0.25.15:** the [changelog](https://github.com/toml-rs/toml/blob/8e1d5a85c361ac012957441bb4788ae82f5dc9c8/crates/toml_edit/CHANGELOG.md)
  lists allocation and rendering improvements in 0.25.14–0.25.15, with no
  public API migration. The `+spec-1.1.0` suffix is build metadata, not a
  prerelease. All 75 selected configuration/profile tests passed.

The zstd upgrade is now applied with registry-verified checksums for zstd
0.14.0, zstd-safe 8.0.0 and zstd-sys 2.1.0. The separate Tantivy zstd 0.13.3
and zstd-safe 7.2.4 entries remain explicit. Pinned RCH tests at f0b610734
passed all 44 selected compression/cache/ledger tests and all eight pack
metamorphic integration tests, including the corrected timing comparisons.
There were no failures or ignored tests in either selected run.

The toml_edit 0.25.15 upgrade uses its official registry checksum. All 75
config parsing, safe config-write and profile-application tests passed on the
compiled RCH executable from 9e8e72b5c, with no failures or ignores. All-target
Clippy with `-D warnings` also passed on that source. The 9,523 library cases
were accounted for across the original invocation and exact continuation
selections; their results and subsequent fixes are recorded below.

The Asupersync pin now includes ba3342249, whose only change from d69851f4 is
guarding the Unix-only UDP readiness import with `cfg(unix)`. This preserves
the Linux implementation and corrects Windows compilation. The exact updated
archive was staged on both release builders; the final Windows binary compiled
successfully and passed native qualification.

Native qualification preparation also found a Windows doctor defect in the
published 0.14.5 binary: the first `doctor --fix` succeeds, but the next fails
at finish with `doctor_run_root_symlink_refused` on its own `latest` pointer.
The fix validates parent directories while inspecting the leaf without
following it. Existing pointer-preservation and regular-file-refusal tests
now include Windows. The final native probe passed eight commands, including
actual byte-lock contention, release and two completed doctor runs with the
prior pointer preserved.

## Existing release gates

The preceding issue-fix candidate had an E0308 in the optional-reranking
degradation return type; commit daba50a03 fixes that mismatch. A fresh remote
all-targets check passed on daba50a03. The preceding broad integration run had 758
passes and 71 failures; those results predate these upgrades. Rechecks passed
41 previously failing cases. The other 30 retain fixture, performance,
environment, verification-input and product limitations; they are not claimed
fixed by this release.

The subsequent hash-embedder integration run at 2f43ef0cb completed with 784
passes, seven failures and 39 filtered cases. Its PPR duplicate-warning failure
was fixed and passed at b25703f47. Three pack determinism tests compared the
registered volatile `elapsedMs` field; their narrow comparison correction is
included in the passing f0b610734 zstd run. Three semantic north-star scenarios passed
separately with the real Model2Vec model on af09aa6bc. These are distinct runs,
not a claim that the entire integration suite is green.

## Security audit disposition

A fresh RustSec audit on f0b610734 used database revision
`b50980aad8b8f14f77e25a97b32dd94bf008b0af` (1,243 advisories) and scanned
569 locked dependencies. `cargo audit --deny warnings` exits 1 for
[RUSTSEC-2026-0253](https://rustsec.org/advisories/RUSTSEC-2026-0253.html):
Tantivy 0.26.1 still requires affected `lru 0.16.4`. The same dependency was
already present in v0.14.5; this refresh did not introduce it. The existing
`paste` maintenance exception remains unchanged; no LRU exception was added.

Source review of the exact registry code, repeated by a second agent, found
one private Tantivy cache, `LruCache<usize, OwnedBytes>`, in
[`src/store/reader.rs`](https://github.com/quickwit-oss/tantivy/blob/0.26.1/src/store/reader.rs).
It uses `new`, `get`, `put` and `len`, never the affected `pop` operation.
Its integer keys also cannot have the panicking destructor required by this
advisory. This explains the limited impact in EE's pinned use; it does not
turn the audit into a pass or establish that the dependency is free of other
defects. Tantivy 0.26.2, published September 8, retains the same affected
requirement. Clean remediation needs a 0.26.x backport of upstream's LRU
dependency update. This remains an explicit upstream release finding tracked
in [#40](https://github.com/Dicklesworthstone/eidetic_engine_cli/issues/40).

The all-features dependency tree at f0b610734 contains none of the 13 forbidden
runtime, storage, graph or HTTP crates. This is a separate passing gate.

## Publication prerequisites checked

CASS view recovered the August 29 release session
`01a04493-0981-7670-851e-8001f6fc191c`, lines 18068 and 18186: six DSR
targets followed by `dsr release --verify-tag --no-dispatch`. The retained
September 11 release record supplies the updated six-target configuration
and Windows cross-compilation/native qualification procedure. All three
repository Actions workflows remain `disabled_manually`.

The current 39-package path dependency graph was checked against crates.io.
Six exact versions are unavailable: `ee-determinism 0.1.0`,
`fnx-algorithms`, `fnx-cgse`, `fnx-classes`, and `fnx-runtime` at `0.2.1`,
and `frankensearch-embed 0.2.7`. Additionally, published Asupersync 0.4.11
comes from `9b114c1f` and has a crate-private `blocking_pool_handle`; EE's
pinned `ba3342249` exposes the capability-checked API required by reranking.
Publishing this EE manifest against the registry would therefore fail.
Crates.io publication needs upstream version releases; binary releases
and Homebrew can consume the exact pinned sources.

## Final v0.15.0 results

Published [v0.15.0](https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/tag/v0.15.0)
on 2026-09-12 from `d09edf26a0adf8d39897de615610b7d67430bb81`. All-target
check, Clippy with `-D warnings`, formatting and the all-features forbidden
dependency gate passed. RCH check/Clippy receipts retain the authorized
build-admission exception and a proof-broker source-state-mismatch diagnostic;
they are compiler passes, not clean infrastructure attestations.

The 9,523-case library selection at `9e8e72b5c` was accounted for across the
original invocation and exact continuation selections: 9,517 passed, two failed
and four were ignored. The original combined build/test invocation reached its
7,200-second timeout; continuation results do not turn it into a successful
single run. The failures exposed stale doctor dependency metadata and a stale
hook-template assertion. After correcting both and fixing the real macOS
buffered-reply race, the three focused regressions passed on final source:
three passed, zero failed, zero ignored, 9,521 filtered out. Two ignored
real-model cases passed separately on an earlier candidate; two explicit
microbenchmarks were not run. The original high-concurrency SIGSEGV (#29) and
the broader integration baseline remain unresolved.

DSR built six targets without GitHub Actions, using nightly-2026-08-31 and
the seven exact sibling revisions in `franken-stack.lock`. Every final binary
passed memory workflows with source/target/version and binary hashes checked.
Windows and Apple Silicon ran natively; Intel Mac used Rosetta and GNU ARM64
used QEMU on Debian 10. GNU x86-64 and musl x86-64 also ran on Debian 10.
Both GNU binaries require at most glibc 2.28; musl has no ELF interpreter or
dynamic-library dependencies. Windows additionally passed actual byte-lock
contention and successive doctor repairs preserving the previous pointer.

The final GNU binary passed six concurrent writers making 60 interleaved
memory/journal writes, with all eight persistence/index assertions passing in
32.273 seconds. A final real-Model2Vec Linux fixture passed 20 CLI/hook commands
while five capabilities observations demonstrated automatic warming followed by
readiness. SessionStart took 0.817 seconds and PreToolUse 0.266 seconds in two
one-rule workspaces; these are fixture measurements, not general guarantees.
The final native Mac fixture passed 17 commands with warming disabled, including
installed hooks, workspace isolation, fallback, deduplication and shutdown.

On the immediately preceding candidate, three real-model semantic scenarios
passed. A 300-memory reranking fixture requested 160 candidates and scored all
96 allowed by the selected portable profile in 15.319 seconds. No profile ceiling
or timeout was raised. Mac boundary migration passed 8/8. The original reporter
also confirmed #36 resolved in v0.14.5 on the original 549-memory workspace;
that confirmation supports closing the report without claiming a bisected cause.

The unchanged shell installer passed a checksum-verified offline installation
and executable self-test in a fresh retained container. Process-local cleanup
suppression preserved temporary files; cleanup and same-host upgrade behavior
were not qualified. All 16 public release downloads then matched the prepared
hashes, and all six archive members matched the qualified binaries. The Homebrew
formula was published in
[`bbaad4e7414a`](https://github.com/Dicklesworthstone/homebrew-tap/commit/bbaad4e7414a11ea3d77113406caa0da798ff31e)
and read back byte for byte with all four archive URLs/hashes checked. An actual
`brew install` was not run. No crates were published; the registry blockers above
remain. Assets are unsigned and do not satisfy `--require-provenance`.

A late report (#41) reproduced on the final binary: intact Claude Code managed
entries reinstall byte-identically, but entries missing `eeManaged` metadata
duplicate on reinstall while status still reports four fresh hooks. The fixture
deliberately removed metadata and does not establish how the original settings
lost it. This issue remains open and is disclosed in the upgrade notes.
