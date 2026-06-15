# Dependency Upgrade Log

**Date:** 2026-06-15  |  **Project:** eidetic-engine (`ee`) v0.9.1  |  **Language:** Rust (2024, nightly)
**Skill:** `/library-updater` applied comprehensively
**Toolchain present:** `cargo 1.96.0-nightly` / `rustc 1.96.0-nightly` (all upgrade-target MSRVs ≤ 1.86 are satisfied)

## Method note (why this run is research-complete but application-gated)

This is a **live multi-agent checkout** (commits land every ~25 s; the working
tree already carries other agents' uncommitted edits) with **remote-only,
expensive builds** (RCH; ~50 min cold) and a currently **contended local cargo
registry** (`File exists (os error 17)` on `~/.cargo/registry`). The
library-updater skill mandates a **full test run after every single dependency
bump** — infeasible to do per-dep here, and mutating the shared
`Cargo.toml`/`Cargo.lock` without that verification would risk breaking every
other agent's build (the exact "force through / batch untested" anti-pattern the
skill warns against).

So this run completed **all build-free phases** — discovery, latest-version
resolution, per-dependency breaking-change research, source call-site
verification, and forbidden-dependency posture — and produced a vetted,
sequenced application plan. The actual manifest edits + `cargo update` +
verification are gated on an explicit go-ahead and an RCH verification lane (see
**Application Plan / Verification** at the end). Nothing in `Cargo.toml` or
`Cargo.lock` has been modified by this run.

## Summary

- **Total direct + dev crates.io deps reviewed:** 31
- **Already at latest stable (no action):** 21
- **Within-semver refresh (low risk, lock-only):** 5
- **Semver-incompatible bumps available (researched SAFE):** 4
- **Failed / rolled back:** 0
- **Preserved (path-deps / franken-stack / pinned):** all `[patch.crates-io]`
  overrides + `=`-pinned `asupersync` — see **Preserved** section
- **Forbidden-dependency posture:** clean before; **stays clean** after every
  proposed bump (verified against each crate's 0.x/dep tree)

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
- **Tests:** ⏸ pending RCH verification

### toml_edit: 0.25.11 → 0.25.12 (`+spec-1.1.0`)
- **Type:** patch  |  **Req `0.25.11` already admits 0.25.12**
- **Breaking:** None (patch; TOML spec metadata unchanged).
- **Tests:** ⏸ pending RCH verification

### uuid: 1.23.1 → 1.23.3
- **Type:** patch  |  **Req `1` already admits 1.23.3**  |  feature `v7` unchanged
- **Breaking:** None (patch).
- **Tests:** ⏸ pending RCH verification

### insta (dev): 1.47.2 → 1.48.0
- **Type:** minor  |  **Req `1.47.2` already admits 1.48.0**  |  feature `json` unchanged
- **Breaking:** None (minor; snapshot format stable — existing `.snap` files unaffected).
- **Tests:** ⏸ pending RCH verification (golden/insta suites)

### pulldown-cmark (dev): 0.13.3 → 0.13.4
- **Type:** patch  |  **Req `0.13.3` already admits 0.13.4**  |  feature `html` unchanged
- **Breaking:** None (patch).
- **Tests:** ⏸ pending RCH verification

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
- **Verdict:** **SAFE/EASY.**  **Tests:** ⏸ pending RCH verification

### sha2: 0.10.9 → 0.11.0  (direct dep)
- **Breaking (sha2 0.11 = digest 0.11):** edition 2024 / MSRV 1.85 (satisfied);
  output type `GenericArray` → `hybrid_array::Array` (derefs to `[u8]`, impls
  `AsRef<[u8]>`); `digest::core_api` → `digest::block_api`; removed features
  (`asm`, `std`, …) — **none enabled here** (dep is plain `sha2 = "0.10.9"`).
  Two incompatible `Digest` traits can coexist in one tree — only a problem for
  code generic over `D: Digest` straddling versions (we have none).
- **Call-site check:** byte-only usage. `src/models/release.rs:894`
  (`Sha256::digest(bytes)` → `digest.as_slice()`) and `src/curate/mod.rs`
  (HMAC: `Sha256::new().update(..).finalize()` → `digest.len()`,
  `copy_from_slice(&digest)`, `update(inner_digest)`). All operations are
  byte-slice ops that work identically on `Array`. **No `GenericArray` named.
  No source edit.**
- **Forbidden deps:** none (`cfg-if`, `cpufeatures`, `digest 0.11.3`, …).
- **Note:** sha2 **0.11 is already in the lock transitively** (required by
  `asupersync`, `frankensearch-*`). Bumping the direct dep **unifies on 0.11 and
  removes the duplicate 0.10.9 + digest 0.10 + generic-array 0.14** copy →
  *less* duplication, not more.
- **Suggested edit:** `sha2 = { version = "0.11.0" }`
- **Verdict:** **SAFE/EASY (and tree-simplifying).**  **Tests:** ⏸ pending RCH verification

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
- **Verdict:** **SAFE/EASY.**  **Tests:** ⏸ pending RCH verification (incl.
  `tests/golden/tiktoken-rs-integration.snap` token-count contract)

### criterion (dev): 0.5.1 → 0.8.2
- **Breaking (cumulative 0.5→0.6→0.7→0.8):** core bench API
  (`Criterion::default`, `criterion_group!`/`criterion_main!`, `bench_function`,
  `BenchmarkGroup`) **unchanged**; `real_blackbox` feature is now a no-op
  (Criterion uses `std::hint::black_box` internally — `criterion::black_box`
  still compiles); `async-std` support dropped in 0.8 (not used);
  **`html_reports` feature unchanged** (still present, non-default — our
  `features = ["html_reports"]` keeps working); **MSRV → 1.86** (satisfied).
- **Forbidden deps:** none by default — `tokio`/`smol` are opt-in async features
  we do not enable; `async-std` is gone; plotting stack is `plotters` (pure-Rust
  SVG) + `rayon`. Dev-only dependency (benchmarks; excluded from the normal test
  gate per AGENTS.md).
- **Suggested edit:** `criterion = { version = "0.8.2", features = ["html_reports"] }`
- **Optional follow-up (not required to build):** migrate `criterion::black_box`
  call sites to `std::hint::black_box` in `benches/`.
- **Verdict:** **SAFE/EASY.**  **Tests:** ⏸ pending bench-compile verification

## Failed / Rolled back

None. (No bump was applied in this run; nothing to roll back.)

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
- **After (projected):** still clean. Each of the 4 semver bumps was checked for
  transitive forbidden pulls; none introduce any. The criterion async runtimes
  (`tokio`/`smol`) remain behind opt-in features we do not enable.
- **Gate to re-run after applying:** `scripts/check-forbidden-deps.sh`
  (build-independent; `cargo metadata` based) + `tests/forbidden_deps.rs`.

## Application Plan / Verification (gated on go-ahead)

Recommended order (each step is independently revertable via
`git checkout -- Cargo.toml Cargo.lock`):

1. **Within-semver refresh (one `cargo update`):**
   `cargo update -p chrono -p toml_edit -p uuid -p insta -p pulldown-cmark`
   (optionally bump the floor pins in `Cargo.toml` to the new versions).
2. **Major bumps (edit `Cargo.toml`, then `cargo update` the four):**
   `getrandom → "0.4.2"`, `sha2 → "0.11.0"`,
   `tiktoken-rs → "0.12.0"` (keep `default-features = false`),
   `criterion → "0.8.2"` (keep `features = ["html_reports"]`).
3. **Forbidden-dep gate:** `scripts/check-forbidden-deps.sh` (expect exit 0).
4. **Full verification via the project's RCH lane** (local cargo is
   remote-only + the registry is currently contended):
   `scripts/rch_verify.sh -- cargo test --lib` (and the golden/contract suites),
   per AGENTS.md §RCH and the repo's verification notes
   (`RCH_VERIFY_ATTEMPT_TIMEOUT_MS=3000000` for the cold-cache window).
5. **Commit hygiene:** stage `Cargo.toml` + `Cargo.lock` + `UPGRADE_LOG.md`
   only; run `scripts/commit-hygiene-classifier.sh --strict --json`; do **not**
   sweep `.beads/issues.jsonl` into the dependency commit.

**Blockers preventing autonomous completion of step 3–4 right now:** (a) live
swarm blast radius on shared `Cargo.toml`/`Cargo.lock`; (b) remote-only,
~50-min build with the local registry contended; (c) the skill's mandatory
per-bump verification cannot be satisfied locally. These are why application is
gated rather than auto-applied.
