# Dependency Contract Matrix

Status: accepted contract artifact for EE-307; amended by EE-178 for the
planned FrankenMermaid adapter gate.

This matrix freezes the dependency contract for the default `ee` feature
profile. The machine-checkable source for this document is
`tests/fixtures/golden/dependencies/contract_matrix.json.golden`; the contract
test in `tests/contracts/dependency_contract_matrix.rs` validates both files.
The golden schema is `ee.dependency_contract_matrix.v1`.

## Canonical Forbidden Crates

The default, no-default, and all-features trees must exclude these crates unless
a later ADR explicitly quarantines a feature with a removal plan:

- `tokio`
- `tokio-util`
- `async-std`
- `smol`
- `rusqlite`
- `sqlx`
- `diesel`
- `sea-orm`
- `petgraph`
- `hyper`
- `axum`
- `tower`
- `reqwest`

## Matrix

| Dependency | Owning surface | Default profile | Optional or blocked profiles | Minimum smoke test | Degradation code | Status fields | Diagnostic command |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `asupersync` | `ee-runtime` | registry `0.3.1`, `default-features = false`, `tracing-integration` | `deterministic-mode` is test-only; `sqlite` is blocked because it can expose `rusqlite` | `runtime_status_reports_asupersync_engine` | `runtime_unavailable` | `runtime.engine`, `runtime.profile`, `runtime.async_boundary` | `ee status --json` |
| `frankensqlite` | `ee-db` | local `/data/projects/frankensqlite` patches with JSON and FTS5 surfaces | `rtree`, `session`, `icu`, and `misc` are not default release gates | `default_feature_tree_excludes_forbidden_crates` and migration tests | `storage_unavailable` | `capabilities.storage`, `degraded[].code` | `ee doctor --json` |
| `sqlmodel_rust` | `ee-db` | `sqlmodel-core` and `sqlmodel-frankensqlite` `0.2.2` via local paths | `c-sqlite-tests` is blocked because it is parity-only and can expose `rusqlite` | `migration_sequence_is_contiguous` and repository tests | `storage_unavailable` | `capabilities.storage`, `database.schema_version` | `ee db status --json` |
| `frankensearch` | `ee-search` | local `/data/projects/frankensearch`, `default-features = false`, `hash`, `storage`, `model2vec`, `lexical`, `fts5` through `ee` features | `fastembed`, `download`, `api`, and high-quality embed profiles are blocked until forbidden network/runtime crates are absent | search/index smoke tests with hash embeddings | `search_unavailable` | `capabilities.search`, `index.generation`, `degraded[].code` | `ee index status --json` |
| `franken_networkx` | `ee-graph` | disabled by default; `graph` feature enables `fnx-runtime`, `fnx-classes`, and `fnx-algorithms` | `ftui-integration` is not part of `ee`; graph remains feature-gated until contract gates pass | graph projection and centrality tests | `graph_unavailable` | `capabilities.graph`, `graph.snapshot_generation` | `ee graph status --json` |
| `coding_agent_session_search` | `ee-cass` | external `cass` process; no Rust crate dependency | bare interactive output is blocked; only `--robot` and `--json` contracts are consumed | CASS fixture parsing for `capabilities`, `health`, and API version | `cass_unavailable` | `capabilities.cass`, `degraded[].code` | `ee import cass --dry-run --json` |
| `toon_rust` | `ee-output` | local `/data/projects/toon_rust` as package `tru`, `default-features = false` | TOON is a renderer only; JSON remains the canonical schema surface | TOON renderer round-trip/golden parity tests | `toon_unavailable` | `capabilities.output.toon` | `ee status --json` |
| `franken_mermaid` | `ee-diagram` | not linked; plain Mermaid text remains owned by `ee-output` and `ee-graph` | future `franken-mermaid-adapter` profile is blocked until `/dp/franken_mermaid` exists, its API is audited, and `cargo tree -e features` proves no forbidden crates | Gate 11 Mermaid goldens plus a future adapter cargo-tree audit | `diagram_backend_unavailable` | `capabilities.output.diagram`, `degraded[].code` | `ee doctor --json` |
| `franken_agent_detection` | `ee-agent-detect` | local `/data/projects/franken_agent_detection`, `default-features = false` | connector-backed scans are blocked from the default profile until privacy and dependency gates pass | agent detection fixture tests with root overrides | `agent_detection_unavailable` | `capabilities.agent_detection` | `ee agent sources --json` |
| `fastmcp-rust` | `ee-mcp` | not linked in the default profile; `mcp` feature is currently an empty adapter gate | stdio MCP support must prove no forbidden async/network stack before linking | MCP stdio initialize/tools/resources golden tests | `mcp_unavailable` | `capabilities.mcp` | `ee doctor --json` |

## Drift Policy

- A dependency row changes only when the corresponding owner updates the golden
  JSON and this Markdown file in the same bead or commit.
- Path dependencies are local-development decisions. Before release, each row
  must either move to a registry pin or record an ADR-backed local source policy.
- `cargo update --dry-run` is advisory. It should fail CI only when the simulated
  update introduces a forbidden crate, duplicates a franken-stack family, or
  invalidates an accepted feature profile.
- `ee diag dependencies --json` and `ee doctor --franken-health --json` are owned
  by the dependent diagnostic bead. Their output must reproduce this matrix
  rather than inventing a second dependency contract.
