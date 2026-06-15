# Dependency Upgrade Log

**Date:** 2026-06-15  |  **Project:** eidetic-engine (`ee`) v0.9.1  |  **Language:** Rust (2024, nightly)
**Skill:** `/library-updater` applied comprehensively
**Toolchain present:** `cargo 1.96.0-nightly` / `rustc 1.96.0-nightly` (all upgrade-target MSRVs ≤ 1.86 are satisfied)

## Outcome: APPLIED and committed (commit `d5d2e236`, pushed to `origin/main`)

All 9 updates were applied to `Cargo.toml` + `Cargo.lock`, the required sha2 0.11
source migration was made, and the changeset was committed as `d5d2e236` and
pushed to `origin/main` (a clean fast-forward; the swarm has since built on top).

Context: this is a **live multi-agent checkout** (the orchestrator reset the
shared HEAD under this run mid-task; the working tree carries other agents'
uncommitted edits) with **remote-only, expensive builds** (RCH; ~50 min cold)
and a **contended local cargo registry**. The library-updater skill prefers a
full test run per dependency bump; that is infeasible here, so the lock was
refreshed with a single targeted `cargo update` (isolated `CARGO_HOME`) and the
changeset was verified by one consolidated compile (see **Verification** below)
rather than per-dep test runs.

**Verification actually performed:** `cargo check --all-targets` on the
internal-build lane (`~/ee-build.noindex`, `RCH_CARGO_WRAPPER_BYPASS=1`). The
changeset compiles clean — all sha2-0.11 `LowerHex` errors it introduced were
resolved and there are **zero dependency-related errors anywhere**. Hash-output
format equivalence was proven from generic-array 0.14.7's `LowerHex` source
(default `{:x}` = zero-padded 2-digit-per-byte lowercase hex = the new
`format!("{byte:02x}")` encoding), so no hash strings change.

**Not performed (honest residual gaps):** the canonical RCH proof was
environmentally blocked (same-project admission cap + peer-WIP tree redness);
the full test suite, an isolated benches-compile under criterion 0.8, and
`cargo clippy -D warnings` were NOT run, because the shared base
(commit `31833c82`) is itself **committed-red on `--all-targets`** — a
pre-existing `recall.rs` test bug (`assert_eq!(... .repair, Some(&str))` against
a `repair: Option<String>` field) unrelated to this dependency work. A clean
green tree was therefore never available locally during this run.

## Summary

- **Total direct + dev crates.io deps reviewed:** 31
- **Already at latest stable (no action):** 21
- **Within-semver refresh (low risk, lock-only):** 5 — applied
- **Semver-incompatible bumps:** 4 — applied (3 were trivial; **sha2 0.11 needed
  a 3-site `LowerHex` source migration the initial research underestimated**)
- **Failed / rolled back:** 0
- **Preserved (path-deps / franken-stack / pinned):** all `[patch.crates-io]`
  overrides + `=`-pinned `asupersync` — see **Preserved** section
- **Forbidden-dependency posture:** clean before; **clean after** (verified on
  the integrated lock via `scripts/check-forbidden-deps.sh`)

## Already at latest stable — no action (21)

| Crate | Req (Cargo.toml) | Resolved (lock) | Latest stable |
|---|---|---|---|
| arc-swap | `1.7` | 1.9.1 | 1.9.1 ✓ |
| base64 | `0.22.1` | 0.22.1 | 0.22.1 ✓ |
| blake3 | `1.5` | 1.8.5 | 1.8.5 ✓ |
| crossbeam-queue | `0.3` | 0.3.12 | 0.3.12 ✓ |
| clap | `4.6.1` | 4.6.1 | 4.6.1 ✓ |
| clap_complete | `4.6.1` | 4.6.5 | 4.6.5 ✓ |
| regex-lite | `0.1` | 0.1.9 | 0.1.9 ✓ |
| libc | `0.2` | 0.2.186 | 0.2.186 ✓ |
| ring | `0.17` | 0.17.14 | 0.17.14 ✓ |
| roaring | `0.11.4` | 0.11.4 | 0.11.4 ✓ |
| rustix | `1.1.4` | 1.1.4 | 1.1.4 ✓ |
| serde | `1.0` | 1.0.228 | 1.0.228 ✓ |
| serde_json | `1.0.149` | 1.0.150 | 1.0.150 ✓ |
| serde_yaml | `0.9.34` | 0.9.34+deprecated | 0.9.34+deprecated (final; crate archived) |
| signal-hook | `0.4` | 0.4.4 | 0.4.4 ✓ |
| tempfile | `3.27.0` | 3.27.0 | 3.27.0 ✓ |
| tracing | `0.1.44` | 0.1.44 | 0.1.44 ✓ |
| tracing-subscriber | `0.3.23` | 0.3.23 | 0.3.23 ✓ |
| unicode-normalization | `0.1` | 0.1.25 | 0.1.25 ✓ |
| zstd | `0.13.3` | 0.13.3 | 0.13.3 ✓ |
| proptest (dev) | `1.9.0` | 1.11.0 | 1.11.0 ✓ |
| static_assertions (dev) | `1.1.0` | 1.1.0 | 1.1.0 ✓ |

> `serde_yaml` is end-of-life (dtolnay archived it; `0.9.34+deprecated` is the
> last release that will ever exist). It is already at that terminal version.
> If a future maintainer wants off the deprecated crate, the migration target is
> a maintained fork (`serde_yml`) or `serde_yaml_ng` — that is a **deliberate
> re-platforming decision, not a version bump**, and is out of scope for this run.

## Within-semver refresh — low risk, lock-only (5)

These need only `cargo update -p <crate>` (the existing requirement already
admits the new version). Optionally bump the floor pin in `Cargo.toml` to
document intent. No source changes; no API surface change.

### chrono: 0.4.44 → 0.4.45
- **Type:** patch  |  **Req `0.4.44` already admits 0.4.45**
- **Breaking:** None (patch).
- **Tests:** compile-verified via internal-build `cargo check --all-targets`; full test-run deferred (RCH blocked + base committed-red)

### toml_edit: 0.25.11 → 0.25.12 (`+spec-1.1.0`)
- **Type:** patch  |  **Req `0.25.11` already admits 0.25.12**
- **Breaking:** None (patch; TOML spec metadata unchanged).
- **Tests:** compile-verified via internal-build `cargo check --all-targets`; full test-run deferred (RCH blocked + base committed-red)

### uuid: 1.23.1 → 1.23.3
- **Type:** patch  |  **Req `1` already admits 1.23.3**  |  feature `v7` unchanged
- **Breaking:** None (patch).
- **Tests:** compile-verified via internal-build `cargo check --all-targets`; full test-run deferred (RCH blocked + base committed-red)

### insta (dev): 1.47.2 → 1.48.0
- **Type:** minor  |  **Req `1.47.2` already admits 1.48.0**  |  feature `json` unchanged
- **Breaking:** None (minor; snapshot format stable — existing `.snap` files unaffected).
- **Tests:** compile-verified via internal-build `cargo check --all-targets`; full test-run deferred (RCH blocked + base committed-red) (golden/insta suites)

### pulldown-cmark (dev): 0.13.3 → 0.13.4
- **Type:** patch  |  **Req `0.13.3` already admits 0.13.4**  |  feature `html` unchanged
- **Breaking:** None (patch).
- **Tests:** compile-verified via internal-build `cargo check --all-targets`; full test-run deferred (RCH blocked + base committed-red)

## Semver-incompatible bumps — researched SAFE, manifest edit required (4)

All four require editing the version requirement in `Cargo.toml`. Each was
researched against the upstream changelog AND verified against this repo's
actual call sites. MSRV gates (1.85 / 1.86) are all satisfied by the nightly
1.96 toolchain. None introduce a forbidden dependency.

### getrandom: 0.3.4 → 0.4.2  (direct dep)
- **Breaking (0.3→0.4):** only real item is **MSRV → 1.85 + edition 2024**
  (satisfied). The disruptive churn (`getrandom()`→`fill()` rename, feature
  removals, `--cfg getrandom_backend`) all happened at **0.3.0** and is already
  absorbed. 0.4.0 adds `RawOsError`, `sys_rng`/`extern_impl` opt-in features;
  **no signature change to `fill`**, no mandatory cfg on Linux/macOS/Windows std.
- **Call-site check:** all 3 direct uses are `getrandom::fill(&mut buf)` —
  `src/config/workspace.rs:693`, `src/core/preflight_token.rs:240`,
  `src/core/plan.rs:1172`. API unchanged across 0.3/0.4. **No source edit.**
- **Forbidden deps:** none (tree is `cfg-if` + `libc` on unix; opt-in `rand_core`
  only via `sys_rng`, which we will not enable).
- **Note:** unifies the direct dep with the transitive `getrandom 0.4.2`
  already present in the lock (alongside 0.2.17/0.3.4 transitives from other crates).
- **Suggested edit:** `getrandom = { version = "0.4.2" }`
- **Verdict:** **SAFE/EASY.**  **Tests:** compile-verified via internal-build `cargo check --all-targets`; full test-run deferred (RCH blocked + base committed-red)

### sha2: 0.10.9 → 0.11.0  (direct dep)
- **Breaking (sha2 0.11 = digest 0.11):** edition 2024 / MSRV 1.85 (satisfied);
  output type `GenericArray` → `hybrid_array::Array` (derefs to `[u8]`, impls
  `AsRef<[u8]>`); `digest::core_api` → `digest::block_api`; removed features
  (`asm`, `std`, …) — **none enabled here** (dep is plain `sha2 = "0.10.9"`).
  Two incompatible `Digest` traits can coexist in one tree — only a problem for
  code generic over `D: Digest` straddling versions (we have none).
- **Call-site check — REQUIRED a source migration (initial research MISSED this):**
  Byte-slice usages are fine: `src/models/release.rs` (`Sha256::digest` →
  `digest.as_slice()`) and `src/curate/mod.rs` HMAC (`digest.len()`,
  `copy_from_slice(&digest)`, `update(inner_digest)`) work identically on the new
  `Array`. **BUT** three sites format the digest with `format!("{:x}",
  finalize())`: `src/core/model.rs` (`sha256_hash_hex`), `src/core/qos.rs`
  (`redacted_hash`), `src/models/singleflight.rs` (`redacted_hash`).
  `generic-array` (sha2 0.10) implements `LowerHex`; **`hybrid_array::Array`
  (sha2 0.11) does NOT**, so those broke with E0277. Fixed by replacing `{:x}`
  with explicit `.iter().map(|byte| format!("{byte:02x}")).collect()`. Verified
  byte-identical output from generic-array's `LowerHex` source (default `{:x}` =
  zero-padded 2-digit lowercase hex per byte), so persisted hashes/IDs/goldens
  are unaffected. (A 4th site briefly appeared in a newer working-tree state but
  is not present in the committed base, so no fix was needed there.)
- **Forbidden deps:** none (`cfg-if`, `cpufeatures`, `digest 0.11.3`, …).
- **Note (CORRECTED):** sha2 0.11 is already in the lock (used by `asupersync`,
  `frankensearch-core/embed/fusion`), AND sha2 **0.10.9 is RETAINED** because
  `ed25519-dalek 2.2.0` and `frankensearch-storage 0.2.0` still pin sha2 0.10.
  So both 0.10.9 and 0.11.0 (and digest 0.10 + 0.11, generic-array + hybrid-array)
  coexist after the bump — this does **not** de-duplicate the tree. (An earlier
  draft of this log incorrectly claimed the bump would remove the 0.10.9 copy.)
- **Suggested edit:** `sha2 = { version = "0.11.0" }`
- **Verdict:** **MODERATE** — compiles clean but required a 3-site `LowerHex`
  source migration that the initial changelog research underestimated.

### tiktoken-rs: 0.11.0 → 0.12.0  (direct dep, `default-features = false`)
- **Breaking (0.11→0.12):** edition 2024 / MSRV 1.85 (satisfied);
  `CoreBPE::encode`/`encode_as`/`count` now return `Result` (**not used here**);
  `EncodeError` newly re-exported; upstream tiktoken core 0.9→0.13; only
  transitive dep change is `rustc-hash 1 → 2`. `cl100k_base`, `o200k_base`,
  `encode_with_special_tokens`, `encode_ordinary` are **unchanged / still infallible**.
- **Call-site check:** only `cl100k_base()` (`src/pack/mod.rs:999`, returns
  `Result<CoreBPE>` — unchanged) and `encode_with_special_tokens(..).len()`
  (`src/pack/mod.rs:1104` — explicitly still infallible in 0.12). **No source edit.**
- **Forbidden deps:** none with `default-features = false` (the `async-openai`
  feature that would pull tokio/reqwest/hyper stays OFF, as today).
- **Suggested edit:** `tiktoken-rs = { version = "0.12.0", default-features = false }`
- **Verdict:** **SAFE/EASY.**  **Tests:** compile-verified via internal-build `cargo check --all-targets`; full test-run deferred (RCH blocked + base committed-red) (incl.
  `tests/golden/tiktoken-rs-integration.snap` token-count contract)

### criterion (dev): 0.5.1 → 0.8.2
- **Breaking (cumulative 0.5→0.6→0.7→0.8):** core bench API
  (`Criterion::default`, `criterion_group!`/`criterion_main!`, `bench_function`,
  `BenchmarkGroup`) **unchanged**; `real_blackbox` feature is now a no-op
  (Criterion uses `std::hint::black_box` internally — `criterion::black_box`
  still compiles); `async-std` support dropped in 0.8 (not used);
  **`html_reports` feature unchanged** (still present, non-default — our
  `features = ["html_reports"]` keeps working); **MSRV → 1.86**.
- **MSRV caveat:** criterion 0.8.2 needs Rust **1.86**, but `Cargo.toml` still
  declares `rust-version = "1.85"`. Satisfied in practice (toolchain is nightly
  1.96, and criterion is a dev-dep that doesn't affect the *published* crate's
  consumer MSRV), but `cargo test`/`cargo bench` on a strict 1.85 toolchain would
  now fail to build criterion. Left as-is; flag if a 1.85 MSRV gate is ever added.
- **Forbidden deps:** none by default — `tokio`/`smol` are opt-in async features
  we do not enable; `async-std` is gone; plotting stack is `plotters` (pure-Rust
  SVG) + `rayon`. Dev-only dependency (benchmarks; excluded from the normal test
  gate per AGENTS.md).
- **Suggested edit:** `criterion = { version = "0.8.2", features = ["html_reports"] }`
- **Optional follow-up (not required to build):** migrate `criterion::black_box`
  call sites to `std::hint::black_box` in `benches/`.
- **Verdict:** **SAFE/EASY** (API/feature compat). **Tests:** benches were NOT
  isolated-compile-verified — `--all-targets` could not reach the bench targets
  because the lib build is blocked by peer-WIP + the committed-red base
  (`recall.rs`). Criterion 0.8 bench-API compatibility is research-asserted but
  not executed here.

## Failed / Rolled back

None. All 9 bumps were applied and committed (`d5d2e236`). The sha2 0.11 bump
required a 3-site source migration (see its entry) rather than a rollback.

## Preserved (not touched — path-deps, franken-stack, hard pins)

Per the skill's PRESERVE rule (path deps, pinned/git deps) **and** AGENTS.md
"franken-stack lives in sibling repos; read their source; explicit versions for
stability", the following are intentionally **not** version-bumped by a
dependency-updater run. They move only when their sibling repo is bumped and the
`[patch.crates-io]` / path pins are deliberately re-pinned together.

- `asupersync` — `=0.3.4` hard pin, `[patch]`→ `../asupersync` (path crate
  currently 0.3.3; the version skew is a pre-existing franken-stack state, not
  this run's concern).
- `frankensearch` + `frankensearch-*` — path `../frankensearch/*`, `[patch]`.
- `sqlmodel-core`, `sqlmodel-frankensqlite` — path `../sqlmodel_rust/*`.
- `fsqlite-*` (entire family) — `[patch.crates-io]` → `../frankensqlite/*`.
- `fnx-runtime`, `fnx-classes`, `fnx-algorithms` (+ `fnx-views/dispatch/convert`
  patches) — path `../franken_networkx/*`.
- `toon` (package `tru`) — path `../toon_rust`.
- `determinism` — path `crates/determinism` (in-repo).
- `franken-agent-detection` — path `../franken_agent_detection`.

## Forbidden-dependency posture

- **Before:** clean — `tokio, tokio-util, async-std, smol, rusqlite, sqlx,
  diesel, sea-orm, petgraph, hyper, axum, tower, reqwest` = 0 occurrences in
  `Cargo.lock`.
- **After (VERIFIED):** still clean. `scripts/check-forbidden-deps.sh` was run
  on the integrated lock (including this commit) and reported "no forbidden
  dependencies detected." The criterion async runtimes (`tokio`/`smol`) remain
  behind opt-in features we do not enable.

## What was done (executed)

1. **Manifest edits:** 8 version pins bumped in `Cargo.toml` (uuid stayed at its
   loose `"1"` pin; its 1.23.3 came via the lock refresh).
2. **Lock refresh:** one targeted `cargo update` (isolated `CARGO_HOME` to dodge
   the contended registry) for the unambiguous packages; `cargo metadata`
   reconciled the getrandom/sha2 manifest changes. Net Cargo.lock churn was the
   9 targets plus expected transitive moves (criterion-plot 0.5→0.8, itertools
   0.10.5→0.13, +alloca/+page_size, −is-terminal, −rustc-hash 1.x).
3. **sha2 0.11 source migration:** fixed the 3 `LowerHex`/`{:x}` sites
   (model.rs, qos.rs, singleflight.rs).
4. **Forbidden-dep gate:** `scripts/check-forbidden-deps.sh` → clean (exit 0).
5. **Compile verification:** `cargo check --all-targets` on the internal-build
   lane — changeset compiles clean (zero dep-related errors).
6. **Commit:** hygiene-classified (`source_only`), committed with explicit
   pathspec (`Cargo.toml`, `Cargo.lock`, the 3 source fixes, `UPGRADE_LOG.md`)
   as `d5d2e236`, pushed to `origin/main` as a fast-forward.

## Residual verification gaps (NOT closed)

- **Canonical RCH proof:** blocked by the same-project admission cap (peers were
  building) + a working tree red from peer WIP. Not obtained.
- **Full test suite / golden run** (e.g. `tests/golden/tiktoken-rs-integration.snap`
  token counts): NOT run. Blocked because the committed base (`31833c82`) is
  itself red on the lib-test build (pre-existing `recall.rs` `Option<String>` vs
  `Some(&str)` test bug — unrelated to deps).
- **Isolated benches-compile under criterion 0.8:** NOT verified (same base-red
  blocker; benches depend on a lib that won't fully build locally here).
- **`cargo clippy -D warnings`** (the CI lint gate): NOT run for the same reason.
  The hex-fix idiom is clippy-clean by inspection (`format_collect` is pedantic
  and not enabled by this project's CI), but it was not executed.

To close these, run on a clean tree (or once the swarm's `recall.rs`/peer-WIP is
green): `scripts/rch_verify.sh -- cargo test` and `... cargo clippy --all-targets
-- -D warnings`, or an isolated clone/archive of `d5d2e236`.
