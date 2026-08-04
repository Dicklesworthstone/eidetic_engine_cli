# Typed Memory Fields

Typed memory fields are the machine-readable sidecar for memory kinds that
agents need to filter, inspect, or reason about without parsing prose. The
sidecar can be populated from explicit `--field NAME=VALUE` assignments or by
extracting labeled markers already present in the body. Explicit assignments
win when both sources name the same registry field.

The canonical sidecar schema is `ee.memory.typed_fields.v2`:

```json
{
  "schema": "ee.memory.typed_fields.v2",
  "kind": "decision",
  "fields": {
    "options": ["FrankenSQLite", "rusqlite"],
    "chosen": "FrankenSQLite",
    "rationale": "SQLModel integration and forbidden dependency policy",
    "revisit_by": "2026-09-15T00:00:00Z"
  }
}
```

Valid v1 sidecars remain accepted. Canonicalization rewrites accepted input to
the v2 envelope, so old failure sidecars and fixtures do not need a data
migration.

## Registry

| Kind | Field | Shape | Indexed | Extraction markers |
|---|---|---|---|---|
| `failure` | `cause` | text | yes | `cause-`, `Cause:`, `Cause=`, `Root cause:` |
| `failure` | `regression_surface` | text | no | `regression-`, `Regression:`, `Regression surface:`, `Lost on:` |
| `failure` | `reverted_at_sha` | text | no | `reverted-at-`, `Reverted at SHA`, `Reverted at` |
| `failure` | `family` | text | yes | `family-`, `Family:`, `Family=` |
| `decision` | `options` | text list | no | `Options:`, `Options=` |
| `decision` | `chosen` | text | yes | `Chosen:`, `Chosen=`, `Decision:`, `Selected:` |
| `decision` | `rationale` | text | no | `Rationale:`, `Because:`, `Why:` |
| `decision` | `supersedes` | text | yes | `Supersedes:`, `Supersedes=` |
| `decision` | `revisit_by` | RFC3339 timestamp | no | `Revisit by:`, `revisit_by:`, `revisit-by:` |
| `command` | `command` | text | yes | `Command:`, `Cmd:`, or the first command-looking backtick segment |
| `command` | `when_to_use` | text | no | `When to use:`, `Use when:`, `When:` |
| `command` | `exit_meaning` | text | no | `Exit meaning:`, `Exit codes:`, `Exit code:` |
| `rule` | `condition` | text | yes | `Condition:`, `Condition=`, `When:`, `If:` |
| `rule` | `action` | text | no | `Action:`, `Action=`, `Then:`, `Do:` |
| `rule` | `exceptions` | text list | no | `Exceptions:`, `Except:` |
| `convention` | `scope` | text | yes | `Scope:`, `Scope=`, `Applies to:`, `Where:` |
| `convention` | `pattern` | text | no | `Pattern:`, `Pattern=`, `Convention:`, `Style:` |
| `risk` | `trigger` | text | no | `Trigger:`, `Trigger=`, `When:` |
| `risk` | `blast_radius` | text | no | `Blast radius:`, `Impact:`, `Risk:` |
| `risk` | `safer_alternative` | text | no | `Safer alternative:`, `Safer:`, `Mitigation:`, `Instead:` |
| `anti-pattern` | `trigger` | text | no | Same as `risk.trigger` |
| `anti-pattern` | `blast_radius` | text | no | Same as `risk.blast_radius` |
| `anti-pattern` | `safer_alternative` | text | no | Same as `risk.safer_alternative` |

Kinds without a registry entry (`fact`, `playbook-step`, custom kinds) do not
get fabricated typed fields.

## Bounds

| Bound | Value |
|---|---:|
| Fields per sidecar | 8 |
| Bytes per string value | 4096 |
| Items per text-list field | 8 |
| Raw typed-field JSON bytes | 32768 |

`MAX_TYPED_MEMORY_FIELDS` was raised from 4 to 8 in v2 because decision memories
now need five fields (`options`, `chosen`, `rationale`, `supersedes`,
`revisit_by`). The headroom is bounded; user-defined field names are still out
of scope.

## Explicit Capture

`ee remember` and `ee note` accept repeatable exact assignments:

```bash
ee remember "Remote verification won the storage decision." \
  --kind decision \
  --field "chosen=RCH remote" \
  --field "options=local Cargo" \
  --field "options=RCH remote" \
  --field "rationale=avoid local build artifacts" \
  --json
```

Field names use the same normalization as search: `revisit-by` and
`revisit_by` both persist as `revisit_by`. The first `=` separates the name
from the value, so additional `=` characters remain part of the value. `~` and
`^` are search-only operators and are rejected on writes.

Repeat a list-valued field (`options`, `exceptions`) to append items in command
order. Assigning a scalar field more than once is an error. Explicit values
override values extracted from body labels; other extracted fields remain.
Dry runs validate and return the canonical field map at
`data.typedFields` without writing it.

For `ee remember --batch --stdin`, put a `fields` object on each JSONL row.
Scalar registry fields take strings and list fields take arrays of strings:

```json
{"content":"Remote verification decision.","kind":"decision","fields":{"chosen":"RCH remote","options":["local Cargo","RCH remote"]}}
```

Command-level `--field` is rejected in batch mode so fields cannot be silently
applied to every row. `--field` is also rejected with `--reinforce`, because a
reinforced write preserves the surviving memory rather than implicitly
mutating its sidecar. Idempotency identity includes the canonical explicit
fields, so reusing a key with changed fields is a conflict.

## Search Filters

`ee search` accepts `--kind <kind>` plus repeatable typed field filters:

| Syntax | Meaning |
|---|---|
| `--field name=value` | exact match |
| `--field name~value` | contains match |
| `--field name^value` | prefix match |

The first operator character after the field name wins, so literal `=`, `~`, or
`^` characters may appear in the value without escaping. Field names must be
registry identifiers.

Indexed fields can narrow the search-document metadata path. Non-indexed fields
remain valid filters, but they are checked against the stored memory sidecar
after retrieval.

## Usage Errors

Typed-field validation is not a `degraded[]` condition. Invalid input is a usage
or validation error because the command cannot honestly continue with a
different field contract. The common cases are:

| Case | Outcome |
|---|---|
| Field name is not valid for the kind | error includes the offending field and valid names |
| Assignment omits `=` or uses `~` / `^` | error explains that writes require `NAME=VALUE` |
| Scalar field is assigned more than once | error identifies the duplicate field |
| Field has the wrong JSON type | error names the expected shape |
| RFC3339 field cannot parse | error names the timestamp field and parse reason |
| Text/list/JSON exceeds a bound | error includes actual size and limit |
| Envelope `kind` does not match the memory kind | error reports expected and actual kind |

## Capture Examples

Failure:

```bash
ee remember "Tried page-cache prefetch; tail latency regressed and the change was reverted." \
  --kind failure \
  --level episodic \
  --field family=aggressive-prefetch \
  --field "cause=cache pollution" \
  --field reverted-at-sha=9af3c21 \
  --json
```

Decision:

```bash
ee decide record "storage engine" \
  --chosen FrankenSQLite \
  --alternative rusqlite \
  --rationale "SQLModel integration and forbidden dependency policy" \
  --revisit-by +90d \
  --json
```

Rule:

```bash
ee remember "Condition: Rust source changed. Action: run remote cargo fmt --check before close. Exceptions: docs-only change." \
  --kind rule \
  --level procedural \
  --json
```

Inspect the sidecar with:

```bash
ee memory show <memory-id> --json
```

Machine readers should read `data.memory.typedFields` when present. Absence of
`typedFields` means no explicit assignments or extractable body labels produced
fields, or the kind is not registry-backed.
