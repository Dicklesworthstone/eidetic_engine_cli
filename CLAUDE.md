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
    cargo check --locked --all-targets
# Lint (warnings = errors)
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD -- \
    cargo clippy --locked --all-targets -- -D warnings
# Run all tests (the NON-BENCH gate, which is what verify.sh runs; benches go
# through ./scripts/verify.sh --include-bench)
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD -- \
    cargo test --locked --workspace --lib --bins --tests --examples
# A single test
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD -- \
    cargo test --locked test_name -- --exact
```

**`--pinned-franken-stack` refuses any cargo command without `--locked`.** It
exits 1 before materializing anything, with `"status":
"source_state_refused"`, `blocking_codes:
["rch_verify_franken_stack_locked_required"]` and no `test result:` line —
`elapsed_ms` is 0, so the round trip is cheap but it buys you nothing. The
examples above carried no `--locked` until 2026-10-13 and every one of them
was refused as written; `scripts/check-rch-doc-examples.py` could not catch it
because it only checks that the wrapper precedes `cargo`. It now also requires
`--locked` on pinned examples.

**A pinned refusal almost always means one stale row in `franken-stack.lock`,
not a problem with your checkouts.** Each locked revision's own manifest must
declare the version `Cargo.toml` pins. The report carries BOTH readings per
repository — a live-checkout reading and a `materialized` block — and only the
materialized one has `matches`; the live numbers are informational and do NOT
block. On 2026-10-13 one stale row (`franken_agent_detection` locked at a
commit declaring 0.3.3 against a `=0.3.8` pin) refused every pinned dispatch,
and the live readings correlated so neatly with the symptom that they looked
causal. They were not: the live checkouts are still drifted today and the lane
is green. The repair text now names the offending `repo/package locked=
expected=` rows, so read that rather than inferring.

**`RCH_VERIFY_FRANKEN_STACK_PREFLIGHT=0` disables the whole franken-stack
preflight** (it defaults to `1`). It is a blunt escape hatch, not a fix: it
skips provenance verification entirely rather than scoping it, so a dispatch
run under it carries no attestation that the dependency sources match the lock.
Use it only to establish that the preflight is what is blocking you, then
repair the lock row and drop the flag. Never use it to obtain a verdict you
intend to cite as proof.

Write the wrapper out in full on every line. A `RCH="..."` shorthand reads
nicely and breaks both copy-paste of a single line and
`scripts/check-rch-doc-examples.py`, which looks for the literal wrapper
before `cargo` and cannot resolve a shell variable.

Budget the wait pessimistically: **the same build has been measured at 32
minutes and at 217 minutes, and you cannot choose which you get.** All
2026-10-11, same project:

| lane | target dir | crates | build |
|---|---|---|---|
| `cargo` dispatch | `<project>/.rch-target-<worker>-pool-<hash>` | 278 | 127m33s |
| `rch exec --job` | `/data/projects/<project>/.rch-target` | 475 | 217m07s |
| `rch exec --job` | same path, different worker | 475 | **31m57s** |

Two independent effects, and it is worth not confusing them:

**Which directory** — a cargo dispatch logs `Rewriting CARGO_TARGET_DIR for
remote execution` and redirects into a worker-scoped pool dir; a `--job` logs
no such line and inherits the worker's own default dir. So a `--job` never
reuses a cargo dispatch's warmth, only a previous `--job`'s. That is a real
difference in WORK — 278 crates against 475.

**That rewrite DISCARDS the configured `target-dir` and can exhaust the
internal disk.** `.cargo/config.toml` sends builds to
`/Volumes/USBNVME16TB/temp_agent_space/cargo-target`, but the pool dir it
rewrites to is `<project>/.rch-target-…`, i.e. on whichever volume holds the
project. Measured 2026-10-13: a `cargo test --locked --lib` dispatch compiled
496 crates over 30.7 minutes and then died with

    error: failed to write file <project>/.rch-target-<worker>-pool-<hash>/debug/
      incremental/…/dep-graph.part.bin: No space left on device (os error 28)

with 2.3 TiB free on the configured volume and 10 GiB free on `/`. It was not
too big for the machine; it was pointed at the wrong volume. Confirmed by the
log line itself: present once in the cargo dispatch, absent in the `--job`
rerun, which formed no local pool dir. **For a long build prefer
`RCH_REQUIRE_REMOTE=1 rch exec --job -- cargo <subcommand> --locked …`, which
never writes artifacts to this machine.** Keep `RCH_REQUIRE_REMOTE=1`: a bare
`rch exec` may fall back to a LOCAL cargo build, which lands straight back on
the volume that just filled up.
The failure surfaces as exit 101 with NO `test result:` line — 101 is cargo's
tests-failed code, so reading the exit code alone reports a compile-space
failure as a failing test. See bd-to1fr.

**Which worker** — and this dominates the clock. Rows 2 and 3 are the same
lane, the same directory path and the same 475 crates, 6.8x apart, on workers
whose rated speeds differ by 9%. Do not attribute a slow build to a cold
directory without comparing crate COUNTS; if two runs compiled the same number
of crates, the directory was not the variable.

Treat the slow case as the budget: `RCH_BUILD_TIMEOUT_SEC`/`RCH_TEST_TIMEOUT_SEC`
at 18000, and **one cargo stage per `--job`** — a second `cargo test` stage needs
the TEST profile, another dependency tier on top of whatever the first stage cost.

**`rch exec` DOES have worker pinning: `--worker <ID>`**, "Execute on this exact
worker; overrides environment/project preferences. Refuses local execution or a
different worker when unavailable." This line said it did not until 2026-10-14,
and that cost three dispatches: the scheduler re-picked one box that was
refusing every job with `worker source namespace initialization refused (status
73)` while reporting healthy with free slots, and a blind retry loop picked the
same box again. Pin past a bad worker rather than retrying into it. Pinning also
buys the warm target dir, since a `--job` reuses that worker's own directory.

Pinning is not a capacity escape. A pinned request that cannot be admitted is
refused whole — `[RCH-I001] requested worker set [...] refused (selection error:
queue_timeout)`, exit 103 — because `--worker` declines to fall back. So pin to a
worker you have just seen idle in `rch status`, and keep the retry for the 103.
Admission is project-identity-keyed and capacity-blind (bd-0u3i2), so a free
fleet does not imply an admissible one.

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
