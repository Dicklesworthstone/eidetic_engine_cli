# Gemini CLI - Codebase Investigation & Review Report (Pane 10)

## 1. Investigation Scope

I conducted a deep, methodical, and first-principles architectural review of the Eidetic Engine CLI (`ee`) project. The investigation spanned:

*   **Core Documentation:** `AGENTS.md`, `README.md`, and `docs/trust-model.md`.
*   **Dependency Audits:** Analyzed `Cargo.toml` and the enforcement of the strict "forbidden dependencies" list (preventing `tokio`, `rusqlite`, `petgraph`, etc.).
*   **CLI and Core Pathways:** Traced the "walking-skeleton" CLI commands (`init`, `remember`, `search`, `context`, `why`, `status`) from their entry points in `src/cli/mod.rs` deep into `src/core/`, `src/pack/mod.rs`, `src/search/scoring.rs`, and the DB adapter (`src/db/mod.rs`).
*   **Franken-Stack Subsystems:** Investigated how the Asupersync runtime coordinates with `frankensqlite` (via SQLModel), `frankensearch`, and `franken_networkx`.
*   **Recent Velocity & Epics:** Analyzed the `.beads/issues.jsonl` tracks for epics like `lkw`, `lp4p`, `bikl`, `okfs`, and `mq1k`.
*   **Static Analysis & Background Checks:** Ran `cargo clippy`, `ubs`, and `cargo test` locally and via `rch` to surface low-hanging regressions.

## 2. Bugs Identified and Fixed

During the review, I identified and fixed several systemic logic gaps, race conditions, and test conformance issues:

1.  **Bug:** `okfs.vqu0` DB Race Condition (Missing `ee_advisory_locks` table)
    *   **Finding:** The locking mechanisms (`acquire_advisory_lock`, `is_lock_held`, `cleanup_expired_locks`) assumed the DB migration for `ee_advisory_locks` had successfully completed. Under heavy concurrent load or fresh testing setups, this resulted in fatal SQLite "no such table" panics.
    *   **Fix:** Injected `self.ensure_advisory_locks_table()?` directly at the top of the raw locking functions in `src/db/mod.rs`.
    *   **Locations Modified:** `src/db/mod.rs` (in `acquire_advisory_lock`, `is_lock_held`, `cleanup_expired_locks`), `src/core/index.rs` (in `acquire_index_publish_lock`), and `src/graph/mod.rs` (in `acquire_graph_snapshot_write_owner`).

2.  **Bug:** `okfs.1` Missing Exit Code Conformance Tests
    *   **Finding:** The architecture mathematically mapped internal errors to exit codes 0-8 and 130, but lacked the `tests/exit_code_conformance.rs` drivers to strictly enforce codes 2, 5, 8, and 130, violating the deterministic testing rule.
    *   **Fix:** Added explicit tests for invalid workspaces (Code 2), missing imports (Code 5), future-schema migrations (Code 8), and graceful `SIGINT` termination (Code 130).
    *   **Locations Modified:** `tests/exit_code_conformance.rs`

3.  **Bug:** `okfs.5` Missing Adapter Logic Boundary Enforcement
    *   **Finding:** The mechanical boundary rule states that `src/mcp/` and `src/serve/` must be pure I/O mapping adapters with zero business logic or SQL queries. The automated gate for this was missing.
    *   **Fix:** Created `tests/ee_core_api_no_adapter_logic.rs` that recursively scans adapter directories for forbidden tokens (`DbConnection`, `SELECT`, etc.) to prevent regressions.
    *   **Locations Modified:** Created `tests/ee_core_api_no_adapter_logic.rs`.

4.  **Verification:** `6cjh` (Diversity Key Redundancy Constraint)
    *   **Finding:** Audited `candidate_similarity` in `src/pack/mod.rs`. Verified that the fix correctly uses `FACILITY_LOCATION_DIVERSITY_KEY_SIMILARITY_FLOOR` to clamp similarity for coarse keys (like "formatting") without breaching the `1.0` redundancy cull threshold unless content perfectly matches.

*(Note: Changes were implemented locally; no `git commit` was executed as per agent constraints).*

## 3. Architectural Concerns

*   **Franken-Stack Rigidity:** The total rejection of `tokio` and reliance solely on `Asupersync` provides unparalleled determinism and testing isolation (`LabRuntime`). However, it requires highly bespoke implementations for any network-bound or standard async traits. The current approach is pure and correct for the `ee` requirements, but future contributors must be acutely aware of the `Asupersync` paradigms (`&Cx`, `Outcome`, `Scope`).
*   **Database Contention:** The decision to funnel everything into a single FrankenSQLite file means that the `advisory_locks` mechanism is load-bearing. The fixes applied here harden it, but under heavy multi-agent parallel usage, SQLite write lock timeouts could become a bottleneck. The project relies on the "single-write-owner actor" pattern, which must be strictly maintained.
*   **Aggressive Test Regimes:** The project employs extreme test fidelity (no mocks, full E2E execution). This is incredibly healthy but forces high friction on small changes since every structural change requires updating golden JSON output hashes and provenance validation logic.

## 4. Overall Verdict

**VERDICT: CLEAN**

**Rationale:** The architecture is remarkably coherent and executed with brutal discipline. The `AGENTS.md` constraints are clearly visible in the code structure. The dependencies are immaculate, the command isolation is perfect, and the retrieval/scoring math (`src/search/scoring.rs`) is elegantly deterministic. 

The bugs I found and corrected were largely edge-case initialization races and missing CI conformance tests characteristic of a project rapidly transitioning from MVP to hardened production tool. With the locking logic patched and the adapter boundaries enforced, the core substrate is exceptionally solid.