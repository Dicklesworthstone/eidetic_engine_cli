# Agent Ergonomics E2E Conventions

The F1-F5 agent-ergonomics beads use one shared shell contract:

```bash
WORKSPACE="$(mktemp -d -t ee-agent-e2e-XXXXXX)" \
  scripts/e2e_lib/e2e_pack_budget_too_small.sh
```

Every script sources `scripts/e2e_lib/agent_ergonomics_lib.sh`. The library
requires `WORKSPACE`, defaults `EE_BIN=ee`, creates a retained `LOG_DIR`, emits
`ee.test_event.v1` JSONL through `scripts/lib/e2e_logger.sh`, and exposes:

- `log_step "<label>"`
- `log_run "<label>" <argv...>`
- `assert_jq "<json>" "<jq-filter>" "<expected>" "<label>"`
- `assert_contains "<text>" "<needle>" "<label>"`
- `finalize`, installed as the `EXIT` trap

The driver `scripts/e2e_lib/run_agent_ergonomics_e2e.sh` is wired into
`scripts/verify.sh` after Basic E2E and before Advanced E2E. It runs these
scripts in deterministic order once they exist:

- `e2e_curate_reject_with_reason.sh`
- `e2e_pack_budget_too_small.sh`
- `e2e_harmful_burst_quarantine.sh`
- `e2e_embed_model_unavailable.sh`
- `e2e_rule_validation_counter.sh`

Missing future scripts are recorded as skips so the infrastructure bead can
land before the feature e2es. Set `AGENT_ERGONOMICS_E2E_REQUIRE_ALL=1` when the
F1-F5 scripts are expected to be present. The wrapper fails on any script
failure, on non-executable present scripts, or when the suite exceeds
`AGENT_ERGONOMICS_E2E_TOTAL_BUDGET_SECONDS` (default `300`).

## Rust Tracing

F1-F5 source instrumentation should use stable targets shaped as:

```rust
tracing::info!(
    target: "ee::<subsystem>::<event_name>",
    reason_present = reason.is_some(),
    reason_len = reason.map(str::len).unwrap_or(0),
    "curate transition recorded",
);
```

Info-level fields must not include raw user-provided strings. Prefer length,
presence, bounded enums, stable IDs, or hashes. Integration tests that assert a
span should set `EE_LOG_JSON=1`, capture stderr in `LOG_DIR`, and assert one
targeted event rather than snapshotting the entire log stream.

## Failure-Mode Fixtures

New degraded codes need a fixture at
`tests/fixtures/failure_modes/<code>.json` in the same change as the source
emission. Keep the fixture scenario minimal and deterministic, then classify
the code in `docs/degraded_code_taxonomy.md`. The e2e script should assert the
public response envelope, the degraded code, the ordered recovery actions, and
the absence of unrelated degraded codes.
