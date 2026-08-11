# Dependency Upgrade Log

**Date:** 2026-08-11  |  **Project:** eidetic_engine_cli  |  **Language:** Rust (franken-stack sibling pins + crates.io)

Scope requested: franken-stack pins, especially the new FrankenSQLite release.
This repo's primary dependencies are sibling checkouts pinned by commit in
`franken-stack.lock` and wired through `[patch.crates-io]`; registry versions
matter only where a requirement falls outside the patched version line.
Verification is remote-only (RCH pinned bundles) per repo policy.

## Summary
- **Updated:** unit 1 in flight (frankensqlite + sqlmodel_rust pins)
- **Skipped:** franken_networkx, toon_rust, franken_agent_detection (no consumer-visible movement required by this unit; revisit after unit 1 lands)
- **Needs attention:** 2 (frankensearch fsqlite-0.1 line, asupersync 0.4 major)

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
