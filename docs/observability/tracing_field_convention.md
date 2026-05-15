# Tracing Field Convention

`ee` traces should make a request easy to follow across CLI dispatch, core
services, persistence, and response rendering. New Part II surfaces use the
fields below whenever the value is available.

## Required fields

| Field | Meaning | Required when |
| --- | --- | --- |
| `workspace_id` | Stable workspace identifier used by the command. | The command has resolved a workspace. |
| `request_id` | Per-invocation ULID assigned at CLI entry. | The command is handling a user or agent request. |
| `bead_id` | Bead that introduced the code path. | Debug builds or verification runs set `EE_TRACE_BEAD_ID`. |
| `surface` | Stable surface name such as `db_inspect` or `trauma_guard`. | Every new Part II surface. |
| `phase` | Current phase: `input`, `dispatch`, `dependency_check`, `persistence`, or `response`. | Every span or event that reports progress through a surface. |
| `elapsed_ms` | Wall-clock duration in milliseconds. | Exit events and measured sub-operations. |
| `degraded_codes` | Sorted degraded-code list emitted by the response. | Any response emits non-empty `degraded[]`. |

Use snake_case field names in tracing calls. JSON response fields may keep their
schema-specific casing, but trace events use this table so log queries do not
need per-surface aliases.

## Bead Requirements

Each Part II `implements-surface:*` bead should include a `TRACING:` paragraph
that names the fields its implementation will emit. Example:

```text
TRACING: surface=trauma_guard, phases=input|dispatch|persistence|response,
fields=workspace_id,request_id,bead_id,surface,phase,elapsed_ms,degraded_codes.
```

The paragraph is a contract, not decoration. If a surface intentionally cannot
emit one of the common fields, the paragraph should say why.

## Source Pattern

Prefer structured tracing over string-only log messages:

```rust
tracing::info!(
    workspace_id = %workspace_id,
    request_id = %request_id,
    bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("unassigned"),
    surface = "db_inspect",
    phase = "response",
    elapsed_ms = elapsed.as_millis() as u64,
    degraded_codes = ?degraded_codes,
    "surface completed"
);
```

Use `#[tracing::instrument(...)]` or explicit `tracing::info!`/`debug!` events
where that keeps the code clearer. Avoid putting the field names only inside the
message string; the checker looks for structured field identifiers in source.

## Checker

Run:

```bash
scripts/check-tracing-fields.sh --json
```

The checker is build-independent. It reads `.beads/issues.jsonl`, finds Part II
`implements-surface:*` beads, verifies that they declare `TRACING:`, and checks
declared Rust source surfaces for tracing evidence when those files exist. It
does not write to Beads or edit source files.

## Retrofit Strategy

The first audit after this convention landed reported 46 audited Part II beads
and 70 violations. Most violations are missing `TRACING:` paragraphs in Beads;
the remaining violations are Rust `FILE SURFACE` paths that do not yet show
structured tracing field evidence.

Retire the backlog in this order:

1. Add `TRACING:` paragraphs to tracker descriptions for all Part II
   `implements-surface:*` beads. This is safe tracker-only work and makes the
   expected surface name explicit before source edits begin.
2. Prioritize open or in-progress runtime surfaces over docs-only or release
   process beads, because runtime surfaces are where source tracing can prevent
   future debugging gaps.
3. For every Rust `FILE SURFACE` violation, add structured tracing in the
   implementation or test helper that owns the public response path. Do not add
   placeholder fields to unrelated code just to satisfy the grep gate.
4. Re-run the checker with an explicit external report path when working on this
   Mac:

   ```bash
   EE_TRACING_FIELD_REPORT=/Volumes/USBNVME16TB/temp_agent_space/tracing-field-report.json \
     scripts/check-tracing-fields.sh --json
   ```

5. Close `bd-3usjw.58` only when the checker reports zero violations and the
   final Beads comment includes the report path or copied summary counts.
