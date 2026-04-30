# Dependency Research Notes

> **Status:** research notes, non-normative.
> **Author:** SilentMountain (claude-opus-4-7-1m, 2026-04-29).
> **Purpose:** factual catalog of franken-stack source crates at `/dp/` for the
> EE-001 Cargo skeleton author and the Gate 0A dependency-contract matrix.
> The Cargo manifest committed under EE-001 is the source of truth for actual
> pins. ADRs 0001–0008 govern the architectural why; this file only collects
> the *what* and *where*.

All facts in this document were read directly from each project's `Cargo.toml`
and the public `lib.rs` of each crate as of 2026-04-29. If a crate version,
feature flag, or exported type below differs from the upstream repository at
the time of EE-001 implementation, the upstream wins.

## How To Read This

- **Workspace root** = the repo's top-level `Cargo.toml` (the one shipped at
  `/dp/<project>/Cargo.toml`).
- **Edition** = the Rust edition declared in `[workspace.package]` or
  `[package]`.
- **Default features** = the features pulled in when no `default-features =
  false` override is applied.
- **Forbidden-dep status** is asserted against AGENTS.md's hard-rule list:
  `tokio`, `tokio-util`, `async-std`, `smol`, `rusqlite`, `sqlx`, `diesel`,
  `sea-orm`, `petgraph`, `hyper`, `axum`, `tower`, `reqwest`. Where a crate
  pulls one of these in only behind a non-default feature, the feature gate is
  named so EE-001 can pick a profile that respects the rule.
- "(not found)" means the field was absent in the crate's `Cargo.toml`/`lib.rs`
  — not that the value is unknown in the upstream project.

## Crate Inventory

### 1. asupersync (`/dp/asupersync`)

Workspace edition: 2024. Workspace `rust-version`: not declared at the
workspace root; individual crates may declare their own.

| Crate | Version | Location | Notes |
| --- | --- | --- | --- |
| `asupersync` | 0.3.1 | `/dp/asupersync/asupersync` (root) | Public API: `Cx`, `Scope`, `LabRuntime`, `LabConfig` (re-exported from `asupersync-core`). |
| `asupersync-macros` | 0.3.1 | `/dp/asupersync/asupersync-macros` | Proc-macros for `scope!`, `spawn!`, `join!`, `race!`. |
| `asupersync-tokio-compat` | (see crate `Cargo.toml`) | `/dp/asupersync/asupersync-tokio-compat` | Quarantine adapter. **Not for `ee` core** — pulls `tokio`. |

**Default features (root `asupersync` crate):**
`["test-internals", "proc-macros"]`.

**Feature flags (selected, ~40 declared in total):**
`messaging-fabric`, `waker-profiling`, `wasm-browser-preview`, `wasm-runtime`,
`browser-io`, `browser-trace`, `deterministic-mode`, `native-runtime`,
`metrics`, `tracing-integration`, `proc-macros`, `tower`, `trace-compression`,
`debug-server`, `config-file`, `lock-metrics`, `obligation-leak-detection`,
`io-uring`, `tls`, `tls-native-roots`, `tls-webpki-roots`, `cli`, `sqlite`,
`postgres`, `mysql`, `quic`, `http3`, `kafka`, `compression`,
`simd-intrinsics`, `loom-tests`, `cancel-correctness-oracle`,
`lab-stack-traces`, `fuzz`.

**Forbidden-dep posture:**
- `tokio` / `tokio-util`: not pulled in by default. The `tower` feature exists
  but does **not** itself pull `tokio` (the gating happens inside the
  associated adapter crates).
- `rusqlite`: optional, gated behind the `sqlite` feature. Only used in
  asupersync's own conformance/test paths. **Do not enable `sqlite`** in `ee`.
- `petgraph`, `hyper`, `axum`, `reqwest`: not pulled in by default.

**EE wiring guidance (research only):**
- For EE-001, the minimal default profile is what the AGENTS.md `default`
  feature set implies — leave `asupersync` at default features.
- If structured tracing in tests is desired, opt into `tracing-integration`.
- For deterministic LabRuntime tests, `deterministic-mode` is the relevant flag.

### 2. frankensqlite (`/dp/frankensqlite`)

Workspace edition: 2024. Workspace `rust-version`: 1.85.

| Crate | Version | Location |
| --- | --- | --- |
| `fsqlite` | 0.1.2 | `/dp/frankensqlite` (root) |
| `fsqlite-core` | 0.1.2 | `/dp/frankensqlite/crates/fsqlite-core` |
| `fsqlite-types` | 0.1.2 | `/dp/frankensqlite/crates/fsqlite-types` |
| `fsqlite-error` | 0.1.2 | `/dp/frankensqlite/crates/fsqlite-error` |
| `fsqlite-ext-fts5` | 0.1.2 | `/dp/frankensqlite/crates/fsqlite-ext-fts5` |
| `fsqlite-ext-json` | 0.1.2 | `/dp/frankensqlite/crates/fsqlite-ext-json` |
| `fsqlite-ext-icu` | 0.1.2 | `/dp/frankensqlite/crates/fsqlite-ext-icu` |
| `fsqlite-ext-misc` | 0.1.2 | `/dp/frankensqlite/crates/fsqlite-ext-misc` |
| `fsqlite-ext-rtree` | 0.1.2 | `/dp/frankensqlite/crates/fsqlite-ext-rtree` |
| `fsqlite-ext-session` | 0.1.2 | `/dp/frankensqlite/crates/fsqlite-ext-session` |
| `fsqlite-func` | 0.1.2 | `/dp/frankensqlite/crates/fsqlite-func` |
| `fsqlite-harness` | 0.1.2 | `/dp/frankensqlite/crates/fsqlite-harness` |

**`fsqlite` default features:**
`["native", "linux-asupersync-uring", "json", "fts5", "rtree"]`.

**`fsqlite` declared features:**
`native`, `wasm`, `linux-asupersync-uring`, `json`, `fts5`, `fts3`, `rtree`,
`session`, `icu`, `misc`, `async-api`, `raptorq`, `mvcc`.

**`fsqlite-types` features:** `default = ["native"]`, plus `wasm`. Public API
includes `Cx`, `ObjectId`, `PayloadHash`, `Region`, `SymbolRecord`,
`SymbolRecordFlags`, `Budget`, `Outcome`, `TxnId`, `EpochId`, `RowId`.

**Forbidden-dep posture:**
- `tokio`, `petgraph`, `hyper`, `axum`, `reqwest`: not present.
- `rusqlite`: appears only as a `dev-dependency` for parity testing; never in
  the runtime path.

**EE wiring guidance:**
- AGENTS.md fixes the `fts5` and `json` features; both are already in the
  `fsqlite` default feature set, so the hard rule is satisfied at default.
- The `linux-asupersync-uring` default is Linux-specific. If EE-001 needs
  cross-platform builds, explicitly disable defaults and re-enable
  `["native", "json", "fts5"]` for the portable profile.
- `async-api` is the feature that exposes the asupersync-friendly futures
  surface. Confirm it is enabled when `ee-db` calls into FrankenSQLite from
  inside an `&Cx` future.

### 3. sqlmodel_rust (`/dp/sqlmodel_rust`)

Workspace edition: 2024. Workspace `rust-version`: 1.85. Workspace
`version`: 0.2.2 (used as the default for member crates).

| Crate | Version | Location |
| --- | --- | --- |
| `sqlmodel` | 0.2.2 | `/dp/sqlmodel_rust/crates/sqlmodel` |
| `sqlmodel-core` | 0.2.2 | `/dp/sqlmodel_rust/crates/sqlmodel-core` |
| `sqlmodel-query` | 0.2.2 | `/dp/sqlmodel_rust/crates/sqlmodel-query` (present per AGENTS.md; verify path before pinning) |
| `sqlmodel-schema` | 0.2.2 | `/dp/sqlmodel_rust/crates/sqlmodel-schema` |
| `sqlmodel-session` | 0.2.2 | `/dp/sqlmodel_rust/crates/sqlmodel-session` |
| `sqlmodel-pool` | 0.2.2 | `/dp/sqlmodel_rust/crates/sqlmodel-pool` (present per AGENTS.md; verify path before pinning) |
| `sqlmodel-frankensqlite` | 0.2.2 | `/dp/sqlmodel_rust/crates/sqlmodel-frankensqlite` |
| `sqlmodel-macros` | 0.2.2 | `/dp/sqlmodel_rust/crates/sqlmodel-macros` |
| `sqlmodel-sqlite` | 0.2.2 | `/dp/sqlmodel_rust/crates/sqlmodel-sqlite` |
| `sqlmodel-mysql` | 0.2.2 | `/dp/sqlmodel_rust/crates/sqlmodel-mysql` |
| `sqlmodel-console` | 0.2.2 | `/dp/sqlmodel_rust/crates/sqlmodel-console` |

**`sqlmodel` features:** `default = []`, plus `console`, `c-sqlite-tests`.

**Forbidden-dep posture:**
- `tokio`, `petgraph`, `hyper`, `axum`, `reqwest`: not present.
- `rusqlite`: only behind the `c-sqlite-tests` feature for parity tests; do
  not enable in `ee`.
- `sqlx`, `diesel`, `sea-orm`: not present.

**Public API names (sampled from `sqlmodel-core`/`sqlmodel-session`):**
`Connection`, `Transaction`, `ConnectionConfig`, `SslMode`, `Session<C>`,
`UnitOfWork`, `ChangeTracker`, `IdentityMap`, `Pool`, `SessionConfig`,
`SessionEventCallbacks`, `N1DetectionScope`.

**EE wiring guidance:**
- `ee-db` will likely depend on `sqlmodel`, `sqlmodel-frankensqlite`,
  `sqlmodel-session`, and `sqlmodel-schema`. Pin via the workspace `version
  = "0.2.2"` and use `path = "/dp/sqlmodel_rust/crates/<crate>"` only inside
  the local development profile. Production publication should switch to
  registry pins per ADR 0002 follow-ups (the ADR mentions "release pin
  decision" but does not yet record it).

### 4. frankensearch (`/dp/frankensearch`)

Workspace edition: 2024. Workspace `rust-version`: 1.95.

| Crate | Version | Location |
| --- | --- | --- |
| `frankensearch` (top-level) | 0.3.0 | `/dp/frankensearch/frankensearch` |
| `frankensearch-core` | 0.2.0 | `/dp/frankensearch/crates/frankensearch-core` |
| `frankensearch-embed` | 0.2.0 | `/dp/frankensearch/crates/frankensearch-embed` |
| `frankensearch-storage` | 0.2.0 | `/dp/frankensearch/crates/frankensearch-storage` |
| `frankensearch-rerank` | 0.2.0 | `/dp/frankensearch/crates/frankensearch-rerank` |
| `frankensearch-ops` | 0.2.0 | `/dp/frankensearch/crates/frankensearch-ops` |
| `frankensearch-durability` | 0.2.0 | `/dp/frankensearch/crates/frankensearch-durability` |
| `frankensearch-tui` | 0.2.0 | `/dp/frankensearch/crates/frankensearch-tui` |

> The version delta (top-level 0.3.0 vs sub-crates 0.2.0) is intentional in
> the upstream workspace as of 2026-04-29.

**`frankensearch` (top-level) features:**
`default = ["hash"]`. Declared features include `hash`, `model2vec`,
`fastembed`, `lexical`, `graph`, `storage`, `durability`, `fts5`, `rerank`,
`fastembed-reranker`, `ann`, `download`, `api`, `semantic`, `hybrid`,
`persistent`, `durable`, `full`, `full-fts5`.

**EE feature mapping (from AGENTS.md `[features]`):**
- `embed-fast` → `frankensearch/model2vec`.
- `lexical-bm25` → `frankensearch/lexical`.

`embed-quality` is intentionally not exposed in `ee` yet. Enabling
`frankensearch/fastembed` pulls `fastembed -> hf-hub -> reqwest`, which brings
`tokio`, `tokio-util`, `hyper`, and `tower` into the `--all-features` tree.
That violates the forbidden-dependency gate, so high-quality embedding remains
blocked until upstream has a clean local profile.

**Public API names (in `frankensearch-core`/`frankensearch`):**
`TwoTierSearcher`, `IndexBuilder`, `TwoTierConfig`, `TwoTierMetrics`,
`SearchError`, `SearchResult`, `EmbedderStack`, `HashEmbedder`,
`Model2VecEmbedder`, `FastEmbedEmbedder`, `TantivyIndex`, `HnswIndex`,
`FlashRankReranker`, `GraphRanker`, `Cx`, `QueryClass`, `Canonicalizer`.

**Forbidden-dep posture:**
- `tokio`, `tokio-util`, `hyper`, `axum`, `tower`, `reqwest`, `rusqlite`,
  `sqlx`, `diesel`, `sea-orm`, `async-std`, `smol`, and `petgraph` must be
  absent from the default, no-default, and all-features trees.
- `frankensearch/fastembed` is blocked because it currently introduces
  forbidden async/network crates through `reqwest`.
- Reranking remains opt-in until its resolved tree is audited under the same
  forbidden-dependency contract.

### 5. franken_networkx (`/dp/franken_networkx`)

Workspace edition: 2024. Workspace `version`: 0.1.0.

| Crate | Version | Location |
| --- | --- | --- |
| `fnx-runtime` | 0.1.0 | `/dp/franken_networkx/crates/fnx-runtime` |
| `fnx-classes` | 0.1.0 | `/dp/franken_networkx/crates/fnx-classes` |
| `fnx-algorithms` | 0.1.0 | `/dp/franken_networkx/crates/fnx-algorithms` |
| `fnx-cgse` | 0.1.0 | `/dp/franken_networkx/crates/fnx-cgse` |
| `fnx-convert` | 0.1.0 | `/dp/franken_networkx/crates/fnx-convert` |
| `fnx-views` | 0.1.0 | `/dp/franken_networkx/crates/fnx-views` |
| `fnx-readwrite` | 0.1.0 | `/dp/franken_networkx/crates/fnx-readwrite` |

**`fnx-runtime` features:** `default = []`, plus `asupersync-integration`,
`ftui-integration`. EE-001 will want `asupersync-integration` enabled to bind
into the runtime contract.

**Forbidden-dep posture:** none of the forbidden crates are pulled in;
`petgraph` is intentionally absent (the project ships its own graph
implementation through `fnx-classes`).

**Public API surface includes:** `Graph`, `MultiGraph` (in `fnx-classes`),
plus algorithm entry points (matching, traversal) in `fnx-algorithms`.

### 6. coding_agent_session_search / `cass` (`/dp/coding_agent_session_search`)

Edition: 2024. Resolver: 2.

| Crate | Version | Location |
| --- | --- | --- |
| `coding-agent-search` (binary `cass`) | 0.4.1 | `/dp/coding_agent_session_search` |

`ee` does **not** depend on the `cass` binary as a Rust crate — `ee-cass`
shells out to the installed binary and consumes its `--robot`/`--json` output.
This file is included only because the `cass` `Cargo.toml` lists the exact
asupersync, fsqlite, and frankensearch feature combination that has shipped a
working binary, which can serve as a reference profile for EE-001:

- `asupersync` with `tls-native-roots`.
- `fsqlite` with `fts5` (and presumably the rest of its default set).
- `frankensearch` with `hash`, `lexical`, `ann`, `fastembed-reranker`.

**Forbidden-dep posture:** Compliant in production; `rusqlite` is dev-only.

## Cross-Cutting Notes

### Default Feature Composition Hint For EE-001

Per AGENTS.md the default feature set is
`["fts5", "json", "embed-fast", "lexical-bm25"]`, expanding to:

- `fsqlite-ext-fts5` enabled (covered by `fsqlite/fts5`).
- `fsqlite-ext-json` enabled (covered by `fsqlite/json`).
- `frankensearch/model2vec` enabled.
- `frankensearch/lexical` enabled.

The `fsqlite/linux-asupersync-uring` default needs an explicit decision: it is
Linux-only. Either (a) keep `fsqlite` at default features and let non-Linux
builds fail at the workspace level, or (b) set `default-features = false` on
the `fsqlite` dependency in EE-001 and explicitly opt into `["native", "fts5",
"json"]`. ADR 0002 does not yet record this decision; flagging here for the
EE-001 author.

### Forbidden-Dep Audit Targets For Gate 0A

Build the audit so that any of these names appearing in
`cargo tree -e features --target <triple> --no-default-features --features
"<EE default profile>"` fails CI:

```text
tokio, tokio-util, async-std, smol, rusqlite, sqlx, diesel, sea-orm,
petgraph, hyper, axum, tower, reqwest
```

Sources of risk to watch:
- Enabling `asupersync/sqlite` would pull `rusqlite`. Do not.
- Enabling `asupersync/tower` itself does not pull `tokio`, but any adapter
  crate downstream that uses Tower's MakeService glue may. Default off.
- `sqlmodel/c-sqlite-tests` pulls `rusqlite`. Default off.
- `frankensearch-rerank` (only behind the `rerank` feature) pulls `ort` and
  related ML crates — heavy build, not forbidden, but worth gating.

### Open Questions For EE-001 / Gate 0A

These are not blockers; they are research questions the author will probably
need to answer before pinning Cargo.toml:

1. **Local path deps vs registry pins.** The franken-stack crates are not
   currently published to crates.io (none of the inspected `Cargo.toml`
   declares `[package.publish]` aimed at a public registry). EE-001 will
   probably need `path = "/dp/<project>"` dev pins plus an ADR that records
   how release builds resolve them.
2. **Workspace MSRV.** asupersync does not declare `rust-version` at the
   workspace level. frankensqlite/sqlmodel_rust pin 1.85, frankensearch pins
   1.95. EE's `rust-toolchain.toml` per AGENTS.md requires nightly Rust 2024;
   the highest declared MSRV (1.95) governs effective compatibility but
   nightly clears it.
3. **Optional `mcp` feature.** AGENTS.md lists `mcp = ["dep:rust-mcp-sdk"]`.
   `rust-mcp-sdk` is not in the franken-stack inventory; the EE-001 author
   should verify the crate name and version against the upstream MCP project
   and record the decision either in EE-001 or in a follow-up ADR.

## Verification Hooks This Document Did Not Add

This file is non-normative and adds no tests. It is meant to feed:

- **EE-001** Cargo.toml dependency pinning.
- **Gate 0A** Dependency Contract Matrix (`docs/dependency-contract-matrix.md`,
  `tests/contracts/dependency_contract_matrix.rs`,
  `tests/golden/dependencies/contract_matrix.json`).
- The future ADR on local path deps vs published pins (open question 1 above).

When any of those land, this document can be retired or compressed into a
short footnote.
