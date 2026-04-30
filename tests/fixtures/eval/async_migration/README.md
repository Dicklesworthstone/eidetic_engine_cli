# Evaluation Fixture: async_migration

**Fixture ID:** `fx.async_migration.v1`
**Scenario ID:** `usr_pre_migration_context`
**Owning Bead:** `eidetic_engine_cli-h7g3` (EE-248)

## Purpose

This fixture tests that `ee context` surfaces relevant procedural rules and prior
failure evidence before an agent executes async database migration work.

## Agent Journey

A coding agent preparing to run an async database migration should receive:

1. Queue verification rules (check migration_queue health before proceeding)
2. Prior incident context (timeout due to queue congestion)
3. Rollback checkpoint procedures

## Command Sequence

1. `ee init` - Initialize workspace
2. `ee remember` - Store queue verification rule
3. `ee remember` - Store prior migration timeout incident
4. `ee remember` - Store rollback checkpoint rule
5. `ee context "run async database migration"` - Request pre-task context

## Success Signal

> A fresh agent preparing to run an async database migration sees queue
> verification rules, the prior timeout incident, and rollback checkpoint
> procedures before executing migration commands.

## Degraded Branches

- `semantic_disabled` - Lexical fallback preserves success signal
- `graph_snapshot_stale` - Explicit evidence still surfaces with provenance
- `migration_queue_degraded` - Agent receives warning about queue status

## Files

- `scenario.json` - Full scenario contract
- `source_memory.json` - Synthetic memory fixtures
- `README.md` - This documentation
