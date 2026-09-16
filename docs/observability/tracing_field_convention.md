# Tracing Field Convention

`ee` traces should make a request easy to follow across CLI dispatch, core
services, persistence, and response rendering. New Part II surfaces use the
fields below whenever the value is available.

## Required fields

| Field | Meaning | Required when |
| --- | --- | --- |
| `workspace_id` | Stable workspace identifier used by the command. | The command has resolved a workspace. |
| `request_id` | Per-invocation ULID assigned at CLI entry. | The command is handling a user or agent request. |
| `bead_id` | Bead that introduced the code path. | Debug or verification builds may set compile-time `EE_TRACE_BEAD_ID` before compilation. |
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
    elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
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

The Rust source check is **per bead, not per file**. `FILE SURFACE:` is a change
manifest — new leaf modules, extended dispatch files, schemas, tests, docs — so
the checker considers only production (`src/**`) Rust paths and is satisfied when
at least one of them carries the evidence. Two exclusions are deliberate:

- `tests/**` and `benches/**` are never required to carry tracing. A benchmark
  has no request and no workspace, so it cannot supply `workspace_id`,
  `request_id`, or `elapsed_ms`; instrumenting one would measure the harness.
- A bead that declares no production Rust path is a docs, CI, or test-harness
  surface with no runtime boundary to instrument, and is satisfied vacuously.

This matches how the crate actually emits tracing: roughly 55 of 338 `src` files
use `tracing::`/`#[instrument]`, concentrated at dispatch boundaries
(`src/cli/mod.rs`, `src/core/{context,search,memory,outcome,why,status}.rs`,
`src/steward/mod.rs`, …) rather than in pure leaf helpers. Modules such as
`src/util/radix_ulid_sort.rs` and `src/core/influence.rs` document that they are
storage-independent and hot-path pure; threading a request context into them to
satisfy a grep would destroy the contract they advertise.

A production surface with no tracing evidence anywhere in its declared files
still fails. `scripts/check-tracing-fields.sh --self-test` pins all three
behaviours, including that last anti-regression case, so the rule cannot decay
into a rubber stamp.

## What counts as evidence

"Tracing evidence" means **an emitted event carries the convention fields as
keys** — not that the file mentions them. The checker blanks comments and
string literals, finds each `tracing::{info,warn,error,debug,trace}!`, span
macro, and `#[instrument(...)]`, matches its parentheses, and reads the field
keys out of that body (including the `%field` / `?field` sigil forms and the
`field` shorthand). A file passes when **one** event carries at least
`MIN_EVENT_REQUIRED_FIELDS` (3) of the 7.

This replaced a substring test that asked whether the file contained the
literal `tracing::` plus three of the field names anywhere in its bytes — which
a doc comment satisfies. The counterexample that motivated the change
(bd-ti1zt): `src/steward/mod.rs` passed, while every event it actually emits
carries `memory_id` / `freshness` / `confidence` / `reason` and not one
convention field. `src/cli/mod.rs` (9 events) and `src/core/context.rs` (24
events) passed the same way and carry zero convention keys between them.

So a green result now means "some event on this surface is shaped like the
convention", which is a real if partial claim. It still does **not** mean every
event is conformant, nor that the emitted values are correct. Weight it
accordingly in a release or batch verdict.

`--self-test` carries `src/mentions.rs`, an eight-line reduction of the
`steward/mod.rs` counterexample: prose naming all seven fields, a `tracing::`
call emitting none of them. It must fail. If that fixture ever passes, the
predicate has regressed to a substring check.

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
3. For a Rust `FILE SURFACE` violation, add structured tracing in the
   implementation that owns the public response path — the dispatch boundary,
   not whichever leaf module the bead happened to create. Do not add
   placeholder fields to unrelated code just to satisfy the grep gate, and do
   not instrument a pure helper, a test, or a benchmark to turn a number green.
   If the checker is demanding evidence from a file that structurally cannot
   carry a request context, the gate is wrong and the gate is what should
   change.
4. Re-run the checker with an explicit external report path when working on this
   Mac:

   ```bash
   EE_TRACING_FIELD_REPORT=/Volumes/USBNVME16TB/temp_agent_space/tracing-field-report.json \
     scripts/check-tracing-fields.sh --json
   ```

5. Close `bd-3usjw.58` only when the checker reports zero violations and the
   final Beads comment includes the report path or copied summary counts.
