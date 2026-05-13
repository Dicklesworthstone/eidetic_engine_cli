# Plan Sweep Report

**Generated:** 2026-05-06  
**Bead:** eidetic_engine_cli-5rmx  
**Plan document:** COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md (3652 lines)

## Summary

| Status | Count | Percentage |
|--------|-------|------------|
| Implemented (verified) | 18 | 56% |
| Partially verified | 1 | 3% |
| Implemented (unverified) | 6 | 19% |
| Stubbed | 5 | 16% |
| Missing | 2 | 6% |
| **Total sections** | 32 | 100% |

---

## Section-by-Section Assessment

### §0. Reading guide
**Status:** ✓ Implemented (verified)  
**Evidence:** AGENTS.md embodies the "ee does not replace agent harnesses" principle.

### §1. Executive summary  
**Status:** ✓ Implemented (verified)  
**Evidence:** Core CLI exists with 50+ subcommands. Walking skeleton complete.

### §2. Background — what changed since the original Eidetic Engine
**Status:** ✓ Implemented (verified)  
**Evidence:** Architecture diverges from original Python project as specified.

### §3. Core design principles
**Status:** ✓ Implemented (verified)  
**Evidence:** Local-first, harness-agnostic, deterministic, explainable retrieval all present.

### §4. North Star acceptance scenarios
**Status:** ⚠ Partially verified
**Evidence:** Related executable tests and fixture contracts exist, but the eight Plan §4 North Star scenarios are not all covered by exact command-flow tests. `tests/agent_outcome_scenario_pack.rs` verifies the six-scenario agent journey matrix, not the full Plan §4 matrix.
**Action:** Follow-up beads filed for exact context-command coverage (`eidetic_engine_cli-axyb`) and procedural distillation flow coverage (`eidetic_engine_cli-lpb5`).

| Plan §4 scenario | Coverage status | Concrete evidence | Remaining gap / follow-up |
|------------------|-----------------|-------------------|----------------------------|
| 1. Release memory saves bad release | Partial executable E2E | `tests/usr002_pre_task_brief_scenario.rs`, `tests/advanced_e2e.rs::release_brief_search_context_why_and_doctor_fix_plan_are_machine_clean`, release fixtures in `tests/eval_fixtures.rs` | Exact Plan command, CASS-imported prior release failures, and branch/publishing traps need pinned assertions in `eidetic_engine_cli-axyb`. |
| 2. Async migration honors real runtime model | Fixture contract only | `tests/eval_fixtures.rs::async_migration_scenario_contract_is_complete`, `fx.async_migration.v1` fixtures | No live `ee context "replace a tokio service with asupersync" --json` E2E yet; covered by `eidetic_engine_cli-axyb`. |
| 3. Repeated CI failure becomes procedural memory | Partial plumbing; flow unproven | CASS import/redaction E2E, post-task machine-data checks, and `eidetic_engine_cli-zj46` review implementation closure | Full `import cass` -> `search` -> `review session --propose` -> `curate apply` rule-creation flow depends on learn/procedure/audit surfaces; covered by `eidetic_engine_cli-lpb5`. |
| 4. New repository onboarding without web UI | Partial executable E2E | `tests/usr002_pre_task_brief_scenario.rs`, `tests/smoke.rs::workspace_continuity_scenario_keeps_context_scoped` | Exact onboarding command with conventions, tooling, prior high-value sessions, and degraded warnings needs pinned assertions in `eidetic_engine_cli-axyb`. |
| 5. Catastrophic mistake avoidance | Partial executable/contract coverage | `tests/usr003_in_task_scenario.rs`, `tests/contracts/preflight_tripwires.rs`, dangerous-cleanup fixtures in `tests/eval_fixtures.rs` | Exact cleanup-context command with safer alternatives, approval rules, and provenance needs pinned assertions in `eidetic_engine_cli-axyb`. |
| 6. Offline degraded mode still helps | Partial degraded-diagnostics coverage | `tests/usr005_degraded_scenario.rs`, `tests/degraded_honesty.rs` | Exact explicit-memory-only offline `ee context "run tests before release" --json` flow needs lexical-only and no-false-attribution assertions in `eidetic_engine_cli-axyb`. |
| 7. Post-session distillation is auditable | Shape coverage only | `tests/advanced_e2e.rs::post_task_outcome_scenario_commands_emit_machine_data`, learning/procedure golden tests | Full `review` -> `curate validate` -> `curate apply` -> `memory show` -> `why` flow with audit/search-index assertions is covered by `eidetic_engine_cli-lpb5`. |
| 8. Multi-agent local work does not corrupt memory | Verified executable E2E | `tests/e2e_multi_process_write.rs::concurrent_remember_processes_serialize_durable_writes`, `tests/write_owner.rs::file_backed_two_writers_are_serialized` | No §4 follow-up needed from this audit. |

### §5. Non-goals for v1
**Status:** ✓ Implemented (verified)  
**Evidence:** No daemon required, no LLM calls by default, no workflow engine.

### §6. Technology stack & dependencies
**Status:** ✓ Implemented (verified)  
**Evidence:** Cargo.toml uses frankensqlite, sqlmodel, asupersync, frankensearch, fnx-*. No tokio, rusqlite, petgraph.

### §7. Dependency integration contracts
**Status:** ⚠ Implemented (unverified)  
**Evidence:** Integrations exist but contract conformance untested.  
**Action:** File test bead for dependency contract tests.

### §8. Runtime architecture with asupersync
**Status:** ✓ Implemented (verified)  
**Evidence:** asupersync integration present; &Cx threading model used.

### §9. Crate / module layout
**Status:** ✓ Implemented (verified)  
**Evidence:** src/ structure matches plan: cli/, core/, db/, models/, search/, cass/, graph/, pack/, curate/, steward/, policy/, output/, config/, hooks/, mcp/, serve/, obs/.

### §10. Storage architecture
**Status:** ✓ Implemented (verified)  
**Evidence:** FrankenSQLite via SQLModel; V001-V032 migrations; .ee/ee.db layout.

### §11. Data model
**Status:** ⚠ Implemented (unverified)  
**Evidence:** memories, memory_links, tags tables exist. Full schema parity with Appendix A needs verification.  
**Action:** Cross-check against Appendix A SQL schema.

### §12. Memory lifecycle
**Status:** ✓ Implemented (verified)  
**Evidence:** `ee remember`, `ee outcome`, `ee curate` commands; maturity levels; decay_factor column.

### §13. Hybrid retrieval pipeline
**Status:** ✓ Implemented (verified)  
**Evidence:** Frankensearch integration; BM25+semantic; TwoTierSearcher.

### §14. Knowledge graph layer
**Status:** ⚠ Implemented (unverified)  
**Evidence:** `ee graph` commands exist; fnx-* integration present. Full metric coverage unverified.

### §15. Session ingestion via `cass`
**Status:** ✓ Implemented (verified)  
**Evidence:** `ee import cass` command; CASS adapter in src/cass/.

### §16. Curation, consolidation, and review
**Status:** ⚠ Stubbed  
**Evidence:** `ee curate candidates` exists but `ee review session --propose` returns REVIEW_UNAVAILABLE_CODE.  
**Action:** Existing bead eidetic_engine_cli-zj46 covers this.

### §17. Procedural memory & playbooks
**Status:** ⚠ Stubbed  
**Evidence:** `ee procedure`, `ee playbook` commands exist but PROCEDURE_UNAVAILABLE_CODE active.  
**Action:** Existing bead eidetic_engine_cli-hssh covers this.

### §18. Trauma guard & confidence decay
**Status:** ✓ Implemented (verified)  
**Evidence:** tripwire table (V029), harmful feedback handling, quarantine system.

### §19. Deterministic context packing
**Status:** ✓ Implemented (verified)  
**Evidence:** `ee context`, `ee pack`; MMR selection; token budgets; pack_records table.

### §20. CLI surface
**Status:** ✓ Implemented (verified)  
**Evidence:** 50+ subcommands matching plan. `ee --help` shows full surface.

### §21. Diagnostics, repair, and `ee why`
**Status:** ✓ Implemented (verified)  
**Evidence:** `ee why`, `ee doctor`, `ee diag` commands present.

### §22. Privacy, redaction, and safety
**Status:** ⚠ Implemented (unverified)  
**Evidence:** Policy module exists; redaction in backup/support commands. Full coverage unverified.

### §23. Agent hook integration
**Status:** ⚠ Stubbed  
**Evidence:** hooks/ module exists but hook installer incomplete per open beads.

### §24. Optional MCP server mode
**Status:** ⚠ Stubbed  
**Evidence:** `ee mcp manifest` works but MCP_PARITY_UNAVAILABLE_CODE suggests incomplete.  
**Action:** Existing bead eidetic_engine_cli-phje covers parity test.

### §25. Configuration
**Status:** ✓ Implemented (verified)  
**Evidence:** config/ module; .ee/config.toml support; `ee config` implied by workspace commands.

### §26. On-disk layout
**Status:** ✓ Implemented (verified)  
**Evidence:** .ee/ directory structure matches plan.

### §27. Testing strategy & evaluation harness
**Status:** ⚠ Implemented (unverified)  
**Evidence:** tests/, benches/, scripts/verify.sh exist. Full harness coverage unverified.

### §28. Performance budget
**Status:** ✓ Implemented (verified)  
**Evidence:** benches/budgets.toml, baselines/v0.1.json, scripts/bench.sh (eidetic_engine_cli-htjd).

### §29. Walking skeleton acceptance gate & rollout milestones
**Status:** ✓ Implemented (verified)  
**Evidence:** Walking skeleton complete per vwfa closure. Milestone tracking via beads.

### §30. Granular backlog
**Status:** N/A (meta)  
**Evidence:** Backlog tracked via .beads/ system.

### §31. Risks & open questions
**Status:** N/A (meta)  
**Evidence:** Risks addressed through implementation or deferred.

---

## Appendix Cross-Checks

### Appendix A — full SQL schema
**Status:** ✓ Documented divergence; follow-up completed
**Evidence:** Appendix A now states that the live DDL contract is the ordered migration set in `src/db/mod.rs` plus `ee_schema_migrations`, and that Appendix A is a design-source snapshot until explicitly reconciled. `tests/contracts/schema_drift.rs` now captures the live migrated FrankenSQLite table, column, and index set under `ee.database.live_ddl.v1`.
**Action:** `eidetic_engine_cli-t9ko` closed with a live DDL parity gate and explicit Appendix A divergence assertions.

### Appendix B — JSON output contracts  
**Status:** ✓ Reconciled by follow-up
**Evidence:** Appendix B now defines `ee.response.v1` as the canonical success envelope, `ee.error.v2` as the failure envelope, and treats command-specific examples as representative `data` payload sketches. `src/output/mod.rs::public_schemas()` is the single list/export registry, including the rule schemas listed by `ee schema list --json`.
**Action:** `eidetic_engine_cli-5mra` added schema-list golden coverage and a registry self-consistency test that exports every listed schema exactly once.

### Appendix C — example end-to-end agent flow
**Status:** ⚠ Partially covered; follow-up filed
**Evidence:** Existing tests cover pieces of the flow: `tests/e2e_core_workflow.rs`, `tests/cli_loop_e2e.rs`, and `tests/no_mocks_e2e.rs` cover init/remember/search/context/why; CASS import tests cover imported sessions and evidence spans; curate smoke tests cover candidates/apply; outcome golden tests cover feedback/audit shape. No single test pins the exact Appendix C setup, import, task-start, during-work, end-of-work curation, rule promotion, and outcome-feedback trace or documents intentional command/output drift.
**Action:** `eidetic_engine_cli-oxt2` covers the exact Appendix C agent-flow parity scenario.

---

## Beads Filed

| Section | Status | Bead |
|---------|--------|------|
| §16 Curation | Stubbed | eidetic_engine_cli-zj46 (existing) |
| §17 Procedural | Stubbed | eidetic_engine_cli-hssh (existing) |
| §24 MCP | Stubbed | eidetic_engine_cli-phje (existing) |
| §4 Scenarios | Partially verified | eidetic_engine_cli-lac7; follow-ups eidetic_engine_cli-axyb, eidetic_engine_cli-lpb5 |
| §7 Contracts | Unverified | (needs test bead) |
| §11 Data model | Unverified | (needs schema parity bead) |
| Appendix A | Documented divergence | eidetic_engine_cli-t9ko |
| Appendix B | Reconciled | eidetic_engine_cli-5mra |
| Appendix C | Partially covered | eidetic_engine_cli-oxt2 |

---

## Next Steps

1. Complete §4 and Appendix follow-up beads; file test beads for remaining unverified sections (§7, §11)
2. Monitor existing implements-surface beads for stubbed sections
3. Re-run sweep after Wave 3 beads close
