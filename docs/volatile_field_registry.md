# Volatile Field Registry

This registry is the single source for fields that determinism checks may strip
before comparing machine-facing JSON produced from the same workspace state.

The Rust registry lives in `src/obs/volatile_fields.rs` as
`VOLATILE_FIELD_NAMES`. The J7 determinism harness mirrors the same list in
`scripts/e2e_overhaul/determinism.sh`. Additions must update this document,
the Rust constant, and the shell list together.

| Field path | Reason for volatility | Introduced in version | Notes |
|---|---|---|---|
| `generatedAt` / `generated_at` | Wall-clock timestamp | v0.1 | RFC 3339 report timestamp. |
| `last_accessed` / `last_accessed_at` | Per-read update | v0.1 | Access signals for memory freshness and decay. |
| `last_seen_at` | Per-read or per-observation update | v0.1 | Agent, workspace, and discovery observations may refresh this field. |
| `last_used_at` | Per-read update | v0.1 | Usage freshness signal. |
| `audit_ts` | Per-write timestamp | v0.1 | Audit chain event time. |
| `elapsedMs` / `elapsed_ms` | Wall-clock elapsed time | v0.1 | Performance-only measurement. |
| `startedAt` / `started_at` | Wall-clock start time | v0.1 | Maintenance jobs and long-running operations. |
| `endedAt` / `ended_at` | Wall-clock end time | v0.1 | Maintenance jobs and long-running operations. |
| `ts` / `timestamp` | Generic wall-clock timestamp | v0.1 | Log envelopes and event records. |
| `runIndex` / `run_index` | Measurement run ordinal | v0.1 | Perf gates compare stable payloads across repeated invocations. |
| `ee_binary_hash` | Per-build artifact hash | v0.1 | Included in run summaries and status-like diagnostics. |
| `databasePath` / `workspacePath` | Machine-dependent absolute path | v0.1 | Canonicalized but environment-dependent. |
| `indexDir` | Machine-dependent absolute path | v0.1 | Rebuildable derived asset location. |

The registry is intentionally field-name based, not JSON-pointer based. These
fields may appear at multiple nesting depths across command responses, golden
fixtures, and E2E support logs.
