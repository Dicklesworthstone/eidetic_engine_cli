# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is ee?

`ee` is a Rust CLI that provides durable, local-first memory for coding agents. It captures facts, decisions, rules, and session evidence; indexes them with hybrid search; and emits compact context packs that agent harnesses (Codex, Claude Code, etc.) consume.

## Build Commands

**Cargo does not run locally in this repo.**
`scripts/check-local-cargo-tripwire.sh` forbids the subcommands `build`,
`check`, `test`, `bench`, `clippy`, `doc`, `run`, `install`, `rustc` and `fix`.
`fmt` is the one exception, so it is also the only local syntax check you have.

```bash
cargo fmt --check                   # The ONLY cargo subcommand allowed locally
./scripts/verify.sh                 # Full verification; dispatches its own cargo stages
```

Everything else is dispatched to the pinned RCH lane:

```bash
# Type check
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD -- \
    cargo check --all-targets
# Lint (warnings = errors)
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD -- \
    cargo clippy --all-targets -- -D warnings
# Run all tests
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD -- \
    cargo test --workspace --all-targets
# A single test
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD -- \
    cargo test test_name -- --exact
```

Write the wrapper out in full on every line. A `RCH="..."` shorthand reads
nicely and breaks both copy-paste of a single line and
`scripts/check-rch-doc-examples.py`, which looks for the literal wrapper
before `cargo` and cannot resolve a shell variable.

Budget the wait from the TARGET DIR, not from the worker. Measured 2026-10-11,
same project and same worker within one hour:

| lane | target dir | crates | build |
|---|---|---|---|
| `cargo` dispatch | `<project>/.rch-target-<worker>-pool-<hash>` (reused) | 278 | 127m33s |
| `rch exec --job` | `/data/projects/<project>/.rch-target` (cold) | 475 | **217m07s** |

A cargo dispatch logs `Rewriting CARGO_TARGET_DIR for remote execution` and
redirects into the worker-scoped pool dir. **A `--job` logs no such line** — it
inherits the worker's own default target dir, which is a different directory.
So a `--job` can never reuse a cargo dispatch's warmth, only a previous
`--job`'s, and `rch exec` has no worker pinning to let you choose. That is why
the same `cargo build --locked --bin ee` has been measured at both 12m49s
(bd-3ghvw, warm) and over 2.9 hours (cold).

Practical consequence: a 20-minute shell e2e suite costs 20 minutes or
**3.6 hours** depending on a directory you never named, so a `--job` that must
build needs `RCH_BUILD_TIMEOUT_SEC`/`RCH_TEST_TIMEOUT_SEC` at 18000 and a
single stage. Two cargo stages in one `--job` do not fit: the second needs the
TEST profile, which is another dependency tier on top of the 217 minutes.

## Architecture

```
src/
├── main.rs              # Entry point
├── lib.rs               # Library surface
├── cli/mod.rs           # CLI handlers (clap commands → core calls)
├── core/                # Business logic
│   ├── effect.rs        # Command effect declarations (read_only, durable_write)
│   ├── search.rs        # Hybrid search via frankensearch
│   ├── context.rs       # Context pack assembly with provenance
│   ├── memory.rs        # Memory storage (remember_memory, revisions, validity)
│   ├── learn.rs         # Procedural rule distillation
│   └── ...
├── db/                  # SQLModel/FrankenSQLite schema and migrations
├── pack/                # Pack binary format, budget classifier, validation
├── search/              # Search index management
├── eval/                # Evaluation runner and fixtures
└── output/              # JSON/Markdown/TOON formatters

tests/
├── golden/              # Surface contract snapshots (*.snap)
├── snapshots/           # JSON contract snapshots (insta)
├── fixtures/            # Test fixtures (eval scenarios, etc.)
│   └── golden/          # Command/schema golden artifacts (316 files)
└── *.rs                 # Integration and E2E tests
```

## Key Conventions

**Forbidden dependencies** — CI fails if these appear anywhere in the tree:
- `tokio`, `async-std`, `smol` — use `asupersync` for runtime
- `rusqlite` — use `fsqlite` via `sqlmodel`
- `petgraph` — use `fnx-*` crates
- `sqlx`, `diesel`, `sea-orm` — use `sqlmodel`
- `hyper`, `axum`, `tower`, `reqwest` — no HTTP stacks in core

**Franken-stack crates** live in `/data/projects/` siblings:
- `asupersync` — runtime (`Cx`, `Scope`, `Outcome`, budgets)
- `fsqlite` + `sqlmodel` — database layer
- `frankensearch` — hybrid BM25 + vector search
- `fnx-*` — graph analytics

**Golden snaps** document surface contracts across three locations:
- `tests/golden/*.snap` — command output and schema contracts
- `tests/snapshots/*.snap` — JSON contract snapshots (context, search, why, status, doctor)
- `tests/fixtures/golden/**/*.golden` — command/schema golden artifacts (316 files)

The closure-lint script requires these for implements-surface beads.

**Effect declarations** in `src/core/effect.rs` classify commands as `read_only`, `durable_write`, or `degraded_unavailable`.

**No worktrees or branches** — all work happens directly on `main`.

## Testing

- Unit tests: inline `#[cfg(test)]` modules
- Golden tests: `tests/golden.rs`, `tests/golden/*.snap`, `tests/snapshots/*.snap`, `tests/fixtures/golden/**`
- E2E: `tests/*_e2e.rs` and `scripts/e2e_*.sh`
- Property tests: `proptest` in `tests/property_*.rs`
- Forbidden deps: `tests/forbidden_deps.rs`

Run `./scripts/verify.sh` before pushing. It is the broadest check available
here, but nothing enforces it and it is **not** congruent with the hosted CI
gates — several of those are disabled, so a green run is necessary, not
sufficient. It does gate rustfmt (Rustfmt Conformance, added in `453291c25`).
