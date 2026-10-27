# Code-Anchored Recall

`ee recall` answers the narrow pre-edit question: "I am about to touch this
path, symbol, or diff; what stored memories are anchored to it?" It is not a
replacement for free-text retrieval or task packing.

| Need | Use |
|---|---|
| Known path, symbol, or changed-file set before editing | `ee recall` |
| Open-ended question with text terms | `ee search` |
| Task-wide context with provenance and explanations | `ee pack` |
| Full surface impact plus fallback search hits | `ee impact` |

Run recall before the first edit to a known surface:

```bash
ee recall --path src/db/mod.rs --workspace . --budget-tokens 400 --format markdown
```

For a diff-oriented change, use the read-only git selector:

```bash
ee recall --diff HEAD --workspace . --budget-tokens 400 --json
ee recall --diff-staged --workspace . --budget-tokens 400 --json
```

Selectors compose as OR within the surface set and deduplicate by memory id.
`--path <glob>` matches normalized workspace-relative paths with
case-sensitive fnmatch-style semantics. `--symbol <name>` is an exact symbol
match. `--kind`, `--level`, and `--stale` filter the anchored result set before
ranking. Use `--cursor` only with the continuation cursor returned by a
budget-truncated response.

`--budget-tokens` must be greater than zero. If a positive budget is still too
small to fit the next ranked recall item, the response returns an empty page with
`output_budget_unsatisfiable` and no continuation cursor; raise the budget or
omit the flag rather than retrying the same cursor sequence.

## Create Anchored Memories

During capture, the supported explicit inline forms are
`ee-anchor:path:<path>` and `ee-anchor:symbol:<symbol>`; `anchor:path:<path>`
and `anchor:symbol:<symbol>` are accepted aliases. Tokens are separated by
whitespace. Code fragments can also supply paths and qualified symbols, but
explicit tokens make the intended relationship unambiguous.
Explicit paths support any project layout: `ee-anchor:path:app/routes.py`,
`anchor:path:lib/parser.ts`, and
`ee-anchor:path:.github/workflows/check.yml` all retain their relative path
identity. They can name historical files that no longer exist.

This example creates both anchors and verifies a positive path recall in an
initialized workspace:

```bash
ee init --workspace . --json
ee remember 'Review storage changes. ee-anchor:path:src/db/mod.rs ee-anchor:symbol:DbConnection::open_memory' \
  --level procedural --kind rule --workspace . --json
ee recall --path src/db/mod.rs --workspace . --json \
  | jq -e '.data.recall.items | length > 0'
ee recall --symbol DbConnection::open_memory --workspace . --json
```

Inline tokens become part of the memory body. For an existing memory, use
the additive operation below to preserve its original text and provenance.

## Attach Anchors to an Existing Memory

`ee memory anchors <memory-id>` lists a memory's code anchors. Add repeatable
`--path` and `--symbol` flags to attach anchors, or add `--dry-run` to preview
the attachment. Both the memory and the command must address the same
workspace; use the same `--database` override throughout when addressing a
copied store.

The following example starts with an ordinary memory that has no code
references, previews its attachment, applies it, and checks the exact memory
ID returned by path recall:

```bash
memory_id="$(ee remember 'Check both durable writes before returning success.' \
  --level procedural --kind rule --source manual://storage-review \
  --workspace . --json | jq -r '.data.memoryId')"

ee memory anchors "$memory_id" --workspace . --json
ee memory anchors "$memory_id" --path ./src/db/mod.rs \
  --symbol 'DbConnection::open_memory()' --dry-run --workspace . --json
ee memory anchors "$memory_id" --path ./src/db/mod.rs \
  --symbol 'DbConnection::open_memory()' --workspace . --json

ee recall --path src/db/mod.rs --workspace . --json \
  | jq -e --arg id "$memory_id" '.data.recall.items | any(.memoryId == $id)'
```

Attachment is additive and idempotent. It does not replace the body, change
the memory's source URI, revise its provenance chain, or reset its timestamps.
The anchor operation has its own audit record and schedules the existing
search-index maintenance path. The durable anchor and recall projection are
written together, so the successfully attached anchor is available to recall
immediately. Repeating the same normalized attachment returns `unchanged`
without adding duplicate anchors. Later index rebuilds retain the attachment.

Paths use the same case-sensitive workspace-relative identity as recall:
`./src/db/mod.rs` normalizes to `src/db/mod.rs`. Attachment accepts concrete
paths, not recall globs. Absolute paths, parent traversal, symlink escapes
outside that workspace, malformed symbols, and secret-bearing values are
rejected. Any project layout is accepted, including `app/`, `lib/`, and
`.github/`; a historical path may name a file that no longer exists. Symbols
are exact, case-sensitive names; a trailing `()` is normalized away. File
existence is not proof of an anchor's meaning: the caller supplies the
relationship, and attachment does not read or index source-file contents.

Reports use `ee.memory.anchors.v1` under `data`. `status` is `listed`,
`would_attach`, `attached`, or `unchanged`; `dryRun`, `persisted`, `changed`,
and `addedAnchorCount` describe the result. `anchors[]` always contains typed
hashes and redacted display values. Its optional `locator` field shows the
exact normalized path or symbol when the same public redaction screen used
by recall admits it, so a preview identifies the relationship being added.
The human renderer shows this locator too. Unsafe or unavailable locators
are omitted rather than replaced with a misleading path; their hashes remain
available. Raw memory bodies and source provenance are not included.
`auditId` and `indexJobId` identify the durable operation when one was needed.

## Preview and Backfill Existing Memories

`ee index backfill-anchors` proposes missing code anchors from eligible
memories in the selected workspace. It defaults to a read-only preview;
`--dry-run` makes that explicit and `--apply` opts into mutation.

```bash
ee index backfill-anchors --workspace . --limit 100 --dry-run --json
ee index backfill-anchors --workspace . --limit 100 --apply --json
ee recall --path src/db/mod.rs --workspace . --json
```

Backfill reuses explicit inline tokens and code-fragment extraction, and can
recognize ordinary repository paths mentioned in prose. Explicit path tokens
support any project layout. Unmarked paths use conservative repository-path
patterns or must name an existing file inside the addressed workspace: an
unmarked `app/routes.py` is eligible when that file exists, while an explicit
`ee-anchor:path:app/retired.py` can recover a historical locator. Backfill
does not infer symbols from arbitrary prose, reinterpret a provenance URI as
a code path, or scan source files. Existing anchors remain intact. Retired or
non-visible memories are skipped. Stored text is screened before extraction,
and candidate locators outside the workspace or unsafe under redaction policy
are excluded.
Each accepted attachment follows the same atomic,
audited, idempotent path as explicit attachment and preserves the memory's
content and source provenance.

The default `--limit 100` bounds **scanned memories**, including memories with
no proposed anchors. The `ee.index.backfill_anchors.v1` response contains
`scannedMemoryCount`, `proposedMemoryCount`, `proposedAnchorCount`,
`appliedMemoryCount`, `appliedAnchorCount`, `skippedMemoryCount`, and redacted
per-memory `proposals[]`. If `nextAfter` is not null, continue with
`--after <nextAfter>` even when this page proposed no changes. Preview and
apply a page with the same `--after` boundary before advancing to the next
page. Apply rechecks the current stored memories; preview again if the store
changed substantially between review and application.

Backfill accepts at most **256 distinct screened code-anchor candidates per
memory**. A memory above that limit is skipped in full in both preview and
apply; no subset is attached automatically. The optional `skippedMemories[]`
report identifies it with `reason: "anchor_limit_exceeded"`,
`candidateAnchorCount`, and `anchorLimit`. It contributes to scanned/skipped
counts, and `nextAfter` still advances normally. Other eligible memories in
the page can proceed. Attach selected, reviewed locators from an oversized
memory explicitly with `ee memory anchors`.

Review each proposed locator alongside its memory ID before applying a page:

```bash
ee index backfill-anchors --workspace . --limit 100 --dry-run --json \
  | jq '.data.proposals[] | {memoryId, anchors: [.anchors[] | {anchorKind, locator, anchorValueHash}]}'
```

Use `ee memory show <memory-id> --workspace . --json` to inspect a proposed
memory through the normal public reader. If only some relationships are
appropriate, attach those exact locators with `ee memory anchors` instead
of applying the entire backfill page.

If no eligible references exist, supply known anchors with `ee memory
anchors`. A rebuild can restore an anchor projection from durable source
records; it cannot supply missing source knowledge.

## Backup and Portable Export

Explicit and backfilled bindings are durable source state. Workspace backups
and portable `ee export` packages retain them, and restore/import validates
and restores them alongside the owning memory. The optional `code_anchors`
field in a JSONL memory record carries each binding's `kind`, normalized
`value`, `source`, and anchor `provenance`; old records without that field
remain valid. This anchor provenance is separate from the memory's original
source URI and provenance chain.

A complete export refuses a redaction level that would change a locator or
its anchor provenance, instead of emitting an unusable placeholder and
claiming the binding is recoverable. That includes maximum content redaction
such as `paranoid`. Metadata-only exports omit code locators intentionally.
The import path rejects malformed, duplicate, secret-bearing, or sealed
source bindings before writing them. Review the export's chosen redaction
policy for its destination; no anchor is inferred from a missing JSONL field.

## Hook Install

Hook commands default to printing a managed plan. Use `--print` first, then
install only after reviewing the target path and generated snippets.

```bash
ee hook claude-code --print --workspace . --json
ee hook claude-code --install --workspace . --json
ee hook claude-code --undo --workspace . --json

ee hook codex --print --workspace . --json
ee hook codex --install --workspace . --json
ee hook codex --undo --workspace . --json
```

The install report uses `ee.hook.harness_install.v1`. It includes the harness,
mode, written paths, backup path, snippets, plan entries, and capability gaps.
The managed snippets inject a small recall block before edits and capture
failure context after commands when the harness supports those events. Unsupported
targets return capability gaps; keep recall as an explicit pre-edit command for
those harnesses.

## Latency

Recall sits on the pre-edit path, so it must stay cheap. ADR 0064 sets a warm
core target of less than 30 ms on the mac-m3-pro fixture class and a hook budget
around 400 output tokens. Hook integrations must fail open: if recall is slow,
returns an error envelope, or cannot read the requested diff, the edit proceeds
without injected recall text and the degraded code is left in the report.

## Stale Anchors

The reverse lookup table is `memory_anchor_index`. It is a derived,
rebuildable asset populated from the same anchor extraction used by search
documents and from durable explicit/backfilled code-anchor sources. The
durable source of truth remains the memory and anchor records.

Use stale-only recall to inspect anchors that need attention:

```bash
ee recall --stale --path src/db/mod.rs --workspace . --json
```

Repair stale or missing derived rows **when source anchors exist** with the
index rebuild path:

```bash
ee index rebuild --workspace . --json
ee recall --path src/db/mod.rs --workspace . --json
```

If the store has only non-code anchors such as `config_key`, or has no
anchors, use attachment or backfill first. Repeating a successful rebuild on
that store does not create the missing path/symbol relationship. A missing
projection with existing code anchors is reported as `anchor_index_stale`;
an absence of source code anchors is `anchor_index_empty` with a backfill
preview repair. Search-index staleness after score decay is a separate
maintenance issue: anchor capture uses the same indexing queue and does not
change search-generation or decay semantics.

If a command exits 8 with `migration_required`, the database schema is older
than the binary. Apply migrations before relying on recall:

```bash
ee migrate status --workspace . --json
ee migrate run --workspace . --json
```

Dropping `memory_anchor_index` is not data loss, but do not repair it by
manual SQL. Use `ee migrate run` for schema drift and `ee index rebuild` for
derived-index drift so generation, audit, and repair posture stay coherent.

## Contracts

`ee recall` returns the standard `ee.response.v2` envelope. The recall payload
is under `data.recall` and declares `schema: "ee.recall.v1"`.

```bash
ee schema export ee.recall.v1 --json
ee schema export ee.memory.anchors.v1 --json
ee schema export ee.index.backfill_anchors.v1 --json
ee schema export ee.hook.harness_install.v1 --json
```

Budget truncation uses the shared governor code `output_truncated_budget`, not
a recall-specific code. The continuation cursor is a recall cursor carried in
the degraded entry details and echoed by the recall payload.

## Degraded Codes

| Code | Meaning | Agent action |
|---|---|---|
| `anchor_index_empty` | No indexed or stored path/symbol anchors are available in this workspace; other anchor kinds do not count | Preview `ee index backfill-anchors --workspace . --dry-run --json`, or attach known anchors with `ee memory anchors <memory-id> --path <path>` |
| `anchor_index_stale` | Reverse-index generation is behind DB generation, or source code anchors exist but their projection is empty | Run `ee index rebuild --workspace .` |
| `recall_filtered_empty` | Anchors matched, but filters removed all rows | Relax `--kind`, `--level`, or `--stale` |
| `recall_git_unavailable` | Read-only git diff failed | Retry with explicit `--path` selectors |
| `output_truncated_budget` | Whole recall items were dropped to honor the token budget | Re-run with `--cursor` or raise `--budget-tokens` |
