# EE-170: FrankenNumPy/FrankenSciPy Dependency Tree Spike

Status: completed spike  
Recommendation: go for optional analytics only; no default dependency  
Owner: SwiftBasin  
Date: 2026-04-29

## Question

Can `ee` use FrankenNumPy and FrankenSciPy for optional offline science
analytics and deterministic diagram support without slowing the default agent
loop or violating the no-Tokio/no-rusqlite constraints?

The answer is yes, but only behind an explicit opt-in feature and only through
leaf crates. `ee` should not depend on either top-level workspace, conformance
crate, Python adapter, dashboard binary, or fuzz workspace in its default build.

## Recommendation

Add no FrankenNumPy or FrankenSciPy dependencies to the default `ee` feature set.

When the science surface is implemented, create an optional feature such as
`science-analytics` and start with the smallest useful leaf set:

```text
science-analytics
  fnp-dtype
  fnp-ndarray
  fnp-runtime
  fsci-runtime
  fsci-stats
```

Add `fsci-linalg` only when a concrete diagnostic needs matrix solves,
condition estimates, decompositions, or least-squares behavior. Add
`fsci-cluster` only when an evaluation command needs clustering. Do not add
`fnp-python`, `fnp-conformance`, `fsci-conformance`, dashboard features, fuzz
targets, or oracle-capture code to the `ee` CLI.

Diagram exports should stay renderer-owned in `ee-output`. The science crates
can compute statistics and coordinates, but the stable public output should be
`ee.*.v1` JSON first, with Mermaid/DOT/SVG/Markdown renderers as optional
views over the same deterministic data.

## Source Snapshot

Local dependency source inspected:

- `/data/projects/franken_numpy`, git `1aea920`.
- `/data/projects/frankenscipy`, git `cbb7c8c1`.
- Both source worktrees were dirty at inspection time, so this spike records an
  observed source snapshot rather than a clean release state.
- FrankenNumPy files:
  - `Cargo.toml`
  - `README.md`
  - `FEATURE_PARITY.md`
  - `artifacts/contracts/README.md`
  - `docs/adr/ADR-001-parity-pivot.md`
  - `crates/*/Cargo.toml`
  - `crates/fnp-{dtype,ndarray,runtime,linalg,python}/src/lib.rs`
- FrankenSciPy files:
  - `Cargo.toml`
  - `README.md`
  - `FEATURE_PARITY.md`
  - `docs/ARTIFACT_TOPOLOGY.md`
  - `docs/ORACLE_WORKFLOW.md`
  - `docs/TEST_CONVENTIONS.md`
  - `crates/*/Cargo.toml`
  - `crates/fsci-{runtime,arrayapi,stats,linalg,cluster}/src/lib.rs`

No Cargo build, test, or dependency-tree command was run for this docs-only
spike. Future Cargo verification must use `rch exec -- cargo ...`.

## Current EE Need

The source plan describes this area as optional offline science analytics and
deterministic diagram exports for evaluation and diagnostics. That makes it a
supporting subsystem, not part of the walking skeleton.

The selected dependency shape must preserve:

- Fast default CLI startup.
- No paid or external services.
- No Python or SciPy runtime requirement.
- Stable JSON as the canonical public contract.
- No dashboard/TUI dependency in core.
- No Tokio, rusqlite, petgraph, Hyper, Axum, Reqwest, SQLx, Diesel, or SeaORM.
- Graceful `diagram_backend_unavailable` style degraded output when renderers
  are not available.

## FrankenNumPy Shape

The top-level workspace contains:

| Crate | Role | EE decision |
| ----- | ---- | ----------- |
| `fnp-dtype` | NumPy dtype taxonomy and promotion/casting rules | allow behind `science-analytics` |
| `fnp-ndarray` | Shape, broadcast, stride, reshape, and view safety primitives | allow behind `science-analytics` |
| `fnp-runtime` | Strict/hardened compatibility decision runtime and evidence ledger | allow behind `science-analytics` |
| `fnp-iter` | Transfer-loop and iterator semantics | defer |
| `fnp-ufunc` | Large array operation surface | defer until an EE command needs real array math |
| `fnp-linalg` | NumPy-style linear algebra | defer; prefer `fsci-linalg` for SciPy diagnostics |
| `fnp-random` | NumPy-compatible RNG | defer unless evaluation fixtures need NumPy RNG parity |
| `fnp-io` | NPY/NPZ/text I/O | avoid in `ee` default; consider only for explicit import/export |
| `fnp-conformance` | Oracle/conformance harness | do not depend from `ee` |
| `fnp-python` | PyO3 Python extension surface | do not depend from `ee` |

The important low-level crates are small and mostly self-contained:

- `fnp-dtype` depends on `half`.
- `fnp-ndarray` has no direct external dependencies in its Cargo manifest.
- `fnp-runtime` has default features `[]`; optional features are
  `asupersync` and `frankentui`.

For EE, use `fnp-runtime` without its optional features. `ee` already owns its
Asupersync boundary and should not let a science helper crate widen that
runtime surface.

### Useful FrankenNumPy Concepts

`fnp-ndarray` provides deterministic helpers that match EE's needs:

- `broadcast_shape`
- `broadcast_shapes`
- `element_count`
- `fix_unknown_dimension`
- `contiguous_strides`
- `broadcast_strides`

These are good for validating tabular/evaluation tensors, matrix-shaped metric
outputs, and diagram coordinate arrays. They do not require the full NumPy API.

`fnp-runtime` provides a strict/hardened compatibility decision model:

- `RuntimeMode`
- `CompatibilityClass`
- `DecisionAction`
- `DecisionEvent`
- `EvidenceLedger`

That fits EE's evidence-first design, but it should be adapted into EE's own
audit/output contracts rather than exposed directly.

### FrankenNumPy Exclusions

Do not select:

- `fnp-python`: PyO3 plus Python extension behavior is outside the local CLI
  default and would introduce packaging/runtime concerns that EE does not need.
- `fnp-conformance`: it depends on the full FrankenNumPy surface and
  `asupersync = { version = "0.3.1" }` with default features, which is not the
  feature policy EE wants.
- `fnp-io`: `bytemuck` and `flate2` are acceptable in isolation, but NPY/NPZ
  import/export is not needed for the first science analytics slice.
- `fnp-ufunc`: powerful, but broad. Add only when a specific evaluation command
  needs array operations that cannot be expressed with simple vectors or
  `fsci-stats`.

## FrankenSciPy Shape

The top-level workspace contains:

| Crate | Role | EE decision |
| ----- | ---- | ----------- |
| `fsci-runtime` | CASP, policy, evidence, audit primitives | allow behind `science-analytics` |
| `fsci-stats` | Distributions, hypothesis tests, descriptive stats | allow behind `science-analytics` |
| `fsci-linalg` | Matrix solvers, condition-aware solver portfolio | allow later when needed |
| `fsci-cluster` | K-means and hierarchy clustering | allow later when needed |
| `fsci-arrayapi` | Array API facade and audit wrappers | defer; current integration seams are documented as aspirational |
| `fsci-constants` | Scientific constants | defer unless directly needed |
| `fsci-fft` | FFTs | defer |
| `fsci-integrate` | Integration routines | defer |
| `fsci-interpolate` | Interpolation routines | defer |
| `fsci-io` | SciPy-style I/O | defer |
| `fsci-ndimage` | Image/multidimensional filters | defer |
| `fsci-opt` | Optimization | defer |
| `fsci-signal` | Signal processing | defer |
| `fsci-sparse` | Sparse matrices and sparse graph algorithms | defer |
| `fsci-spatial` | KD-tree, distances, spatial algorithms | defer |
| `fsci-special` | Special functions | transitive through `fsci-stats` if needed |
| `fsci-conformance` | Conformance, dashboard, oracle capture | do not depend from `ee` |

The top-level FrankenSciPy workspace has:

```toml
asupersync = { version = "0.3.1", default-features = false, features = ["test-internals"] }
ftui = { version = "0.3.1", default-features = false }
```

That is acceptable for FrankenSciPy's own conformance/testing workspace, but it
is not acceptable as an EE production dependency policy. EE should depend on
leaf crates, not the top-level workspace feature shape.

### Useful FrankenSciPy Concepts

`fsci-runtime` is the best first integration point. It has no direct Asupersync
dependency in its crate manifest and provides:

- `RuntimeMode`
- `AuditLedger`
- `PolicyEvidenceLedger`
- `PolicyController`
- `SolverPortfolio`
- `SolverEvidenceEntry`
- `MatrixConditionState`
- `SolverAction`

These map cleanly to EE diagnostics and can feed explainable status/evaluation
records.

`fsci-stats` is the likely first analytics crate because EE evaluation can use:

- descriptive statistics
- distribution summaries
- bootstrap/permutation-style evaluation
- correlation/rank metrics
- significance checks for retrieval-quality experiments

It depends on `fsci-runtime`, `fsci-special`, and `rand`.

`fsci-linalg` should be added only for matrix-heavy diagnostics. It uses
`nalgebra` and emits CASP/audit information. That is valuable for scientific
quality, but unnecessary for the first analytics slice if we only need scalar
metric summaries.

`fsci-cluster` is attractive for memory-cluster diagnostics because its K-means
implementation accepts plain `Vec<Vec<f64>>` and a seed, but it should wait for
a specific command or evaluation scenario. Clustering can affect UX and must
have deterministic ordering/tie-break rules in EE output.

### FrankenSciPy Exclusions

Do not select:

- `fsci-conformance`: default feature includes the `dashboard` feature and
  optional `ftui`; it also owns oracle capture and fixture regeneration.
- `conformance_dashboard`: useful in FrankenSciPy, wrong for EE default CLI.
- Python oracle capture scripts: these require Python with NumPy/SciPy and
  belong in upstream conformance workflows, not EE's local-first loop.
- `fuzz`: not a library dependency for EE.
- `fsci-sparse` as a graph analytics shortcut: EE graph work must go through
  FrankenNetworkX, not sparse SciPy graph algorithms, unless a later ADR
  explicitly scopes a science-only diagnostic that does not replace graph core.

## Forbidden Dependency Check

Text search over `Cargo.toml` and `Cargo.lock` in both source trees found no
direct mentions of EE's forbidden crates except false-positive substrings in
fuzz target names containing `hyper` as part of scientific terms.

That is not a substitute for a Cargo feature-tree audit. Before any selected
crate is added to EE, run:

```bash
rch exec -- cargo tree -e features
```

Then fail the integration if these appear anywhere in the EE dependency tree:

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

Special attention:

- `fnp-conformance` uses default Asupersync features. Do not add it.
- `fsci-conformance` uses the top-level Asupersync workspace dependency with
  `test-internals`. Do not add it.
- `ftui` is fine for upstream dashboards, but it should not enter EE unless a
  future TUI feature is explicitly designed.

## Proposed Feature Shape

When implementation starts, use feature names that make opt-in cost obvious:

```toml
[features]
science-analytics = [
  "dep:fnp-dtype",
  "dep:fnp-ndarray",
  "dep:fnp-runtime",
  "dep:fsci-runtime",
  "dep:fsci-stats",
]
science-linalg = ["science-analytics", "dep:fsci-linalg"]
science-cluster = ["science-analytics", "dep:fsci-cluster"]
diagram-export = []
```

Recommended dependency intent:

```toml
fnp-dtype = { version = "0.1.0", optional = true }
fnp-ndarray = { version = "0.1.0", optional = true }
fnp-runtime = { version = "0.1.0", default-features = false, optional = true }
fsci-runtime = { version = "0.1.0", optional = true }
fsci-stats = { version = "0.1.0", optional = true }
fsci-linalg = { version = "0.1.0", optional = true }
fsci-cluster = { version = "0.1.0", optional = true }
```

If the crates are consumed by path from `/dp` during early development, keep the
path wiring in a single dependency block and do not duplicate path decisions
inside command modules.

## Minimal EE Science Surface

The first implementation should avoid a broad "scientific mode" and instead
define one narrow service:

```text
evaluation metrics / retrieval runs
  -> ee science summary model
  -> fsci-stats descriptive statistics
  -> optional fsci-runtime audit entries
  -> ee.response.v1 JSON
  -> optional diagram renderer over the same model
```

Suggested first public contract:

```json
{
  "schema": "ee.science.summary.v1",
  "metric": "retrieval_precision_at_k",
  "sampleCount": 12,
  "mean": 0.72,
  "median": 0.75,
  "stdDev": 0.08,
  "confidence": {
    "method": "bootstrap",
    "seed": 7,
    "lower": 0.67,
    "upper": 0.78
  },
  "provenance": []
}
```

That output can later render as Markdown, Mermaid, DOT, or SVG without changing
the canonical JSON data.

## Diagram Guidance

Do not couple diagrams directly to FrankenSciPy.

Recommended split:

- Science crates compute numeric summaries and optional coordinate layouts.
- EE output contracts own the canonical data.
- Renderers convert canonical data to Mermaid/DOT/SVG/Markdown.
- If a renderer is missing, JSON remains available and the command reports
  `diagram_backend_unavailable` as a degraded capability.

SciPy-style plotting helpers are not the right starting point. The local
FrankenSciPy docs track plotting utilities as part of SciPy spatial/cartography
surface, but there is no evidence that EE needs those routines for Phase 0.

## Test Requirements

When this moves from spike to implementation, add tests in this order.

### Dependency Gates

- `cargo tree -e features` through RCH with `science-analytics` disabled.
- `cargo tree -e features` through RCH with `science-analytics` enabled.
- Forbidden-dependency grep for both feature sets.
- A test proving `science-analytics` is not in the default feature set.

### Analytics Contract Tests

- Unit tests for empty samples, one-element samples, NaN/Inf rejection, and
  deterministic ordering.
- Golden JSON for `ee.science.summary.v1`.
- A real-binary command test proving JSON stdout stays clean and diagnostics go
  to stderr.
- Deterministic seed test for any bootstrap, random, or clustering behavior.

### Degraded Mode Tests

- Science feature disabled returns a stable degraded error or capability state.
- Diagram renderer unavailable still produces canonical JSON.
- Python/SciPy unavailable does not affect EE because EE does not call the
  upstream oracle workflow.

## Risks

| Risk | Impact | Mitigation |
| ---- | ------ | ---------- |
| Adding top-level workspaces | Pulls conformance, dashboard, or test-only feature assumptions | Depend only on leaf crates |
| Adding `fnp-python` | Introduces PyO3/Python packaging and runtime concerns | Exclude from EE |
| Adding conformance crates | Pulls broad surfaces and non-EE workflows | Keep oracle/conformance upstream |
| Science feature becomes default | Slower default CLI and larger dependency tree | Keep opt-in feature; add feature-gate test |
| Diagram renderers mutate public contracts | Golden output churn | JSON remains canonical; diagrams are renderers |
| Clustering nondeterminism | Unstable user-visible results | Require explicit seed and deterministic tie-breaks |
| Sparse graph algorithms substitute for EE graph layer | Violates FrankenNetworkX graph requirement | Keep sparse/science graph routines out of core graph subsystem |

## Follow-Up Beads

This spike should inform:

- `EE-171: Add optional ee-science crate behind science-analytics`
- `Gate 10: Optional Science Analytics Readiness`
- Any future diagram renderer bead
- Any future evaluation metrics bead

Recommended next implementation order:

1. Add feature-gated dependency gate tests without adding analytics behavior.
2. Add `ee.science.summary.v1` domain/output structs.
3. Add one deterministic stats summary over existing evaluation fixture data.
4. Add optional renderer views after JSON goldens are stable.

## Closure Notes

This is a docs-only source-research spike. It does not change `Cargo.toml`
because `Cargo.toml` and `src/**` are active/dirty work surfaces for other
agents and because the correct outcome here is a dependency decision, not a
partial implementation.

Acceptance coverage:

- Go/no-go recommendation: go for optional analytics only, no default
  dependency.
- No-Tokio/no-rusqlite behavior: preserved by excluding conformance/top-level
  workspace surfaces and requiring future feature-tree audits.
- Public output: no new command output introduced.
- Tests: concrete future dependency, analytics, and degraded-mode tests listed.
- Verification: Markdown-only checks were run locally; no Cargo command was
  needed for this spike.
