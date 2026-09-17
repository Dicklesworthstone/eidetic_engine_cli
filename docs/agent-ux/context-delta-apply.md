# Context Delta Apply Guide

Delta payloads add to your prompt; they do not replace the base pack.

`ee pack --since <pack-hash> --json` is the machine-apply transport
optimization for long agent sessions. `--format markdown` is also supported as
a prompt-ready delta document for agents that do not need to reconstruct the
pack object. The normal context pack is still the canonical state. The JSON
delta envelope describes how to update a pack that the agent already has in
memory.

## Base Hash

Pass a hash from a prior `ee pack --json` response's `data.pack.hash`,
or pass the literal `last` (bd-7lvbg.6) to resolve this agent's most
recent recorded baseline from the per-agent ledger: every persisted
pack records a (EE_AGENT_NAME, optional `--task-key`) baseline row
automatically, so cross-session bookkeeping of pack hashes is no longer
the agent's job. `--since last` needs `EE_AGENT_NAME`; when no baseline
resolves the command falls back to the full pack with the
`context_delta_no_baseline` degraded entry (info). `--no-baseline-write`
skips the recording; `--read-only` / `--no-persist` never record. See
[`docs/migrations/pack-baselines.md`](../migrations/pack-baselines.md)
for ledger semantics (cap, audited eviction, GC cascade).

`--format markdown` with `--since` emits a markdown delta document
(added items in full, changed items as field lines, removed items as
one-line id stubs) instead of the JSON envelope. Retained, unchanged items
are not re-emitted; a move or complete replacement may re-emit an item's
body under the same ID. Markdown output is not an `ee.response.v2` envelope,
so `--max-output-tokens` does not govern it; the delta's size follows
the pack's own `--max-tokens` budget rules. Other non-JSON formats
still fall back to the full pack with
`context_delta_format_unsupported`.

The server verifies that the hash names a pack record emitted by `ee` for the
same workspace. Locally computed hashes, hashes from another workspace, and
evicted records are rejected as delta bases. In those cases the command returns
the full pack and includes a degraded entry such as
`context_delta_prior_unknown`.

The server never chains deltas. Each request compares exactly two pack records:
the verified prior pack and the freshly assembled new pack. The new pack hash is
the same hash a no-`--since` context request would return for the same database,
indexes, config, query, and flags.

## Prompt Budgeting

Agents must account for the base pack and the delta payload together until they
replace their local base.

Example:

```text
base pack P1 in prompt: 3500 tokens
delta D1 appended now: 200 tokens
effective prompt cost: 3700 tokens
```

Use `data.tokenSavings.netPackTokens` as the logical size of the reconstructed
new pack. That value can shrink when items are removed even though the delta
payload itself still costs prompt tokens.

## Applying A Delta

First validate the envelope and its association with the local baseline. It
must be a successful `ee.context.delta.v2` JSON delta with no fallback reason,
not an ordinary full response or a markdown document. Check `priorPackHash`,
workspace isolation and policy compatibility before using item operations.
A server-verification boolean is not authentication of an untrusted sender.

Use item IDs as keys, but preserve their sequence. Remove IDs listed in
`items.removed`, merge `items.modified` field changes into retained items,
then append `items.added` in **envelope order**, not lexical ID order. The
snapshot wire shape is `{id, fields: {...}}`: modifications apply inside the
`fields` object, not to the snapshot's own `id` or wrapper.

An ID may appear in both `removed` and `added`. This means **remove the old
item completely, then append the supplied replacement**. Never cancel out
these operations or merge the replacement with the old fields. This is how
v2 expresses moves and field deletion without extending the schema. An ID in
`modified` must not also be removed or added.

For example, changing `[a, b, c]` to `[b, c, a]` removes `a` and re-adds it at
the end. Inserting an item before existing items may require removing and
re-adding the suffix. The kernel retains the longest new prefix representable
as a subsequence of the old pack; ordinary field updates, removals and
append-only changes remain compact.

An ordinary `[old, new]` pair assigns `new`, including an explicit JSON null.
It does not delete a field. When a field disappears, the kernel emits a full
item replacement instead. V2 represents both an absent old field and an old
null as null, so consumers can compare old values but cannot distinguish those
two old states from the pair alone.

Validate all operations before installing the result. Reject blank or duplicate
IDs, unknown removal/modification targets, additions that overwrite retained
items, conflicting operations, and mismatched old field values. Work on a copy
so a late failure cannot leave the caller's context half-updated.

Pseudo-code, after validating the envelope and all operation preconditions:

```text
items = deep copy of prior_snapshot.items, preserving order
remove every item whose id occurs in delta.data.items.removed

for change in delta.data.items.modified:
    item = the retained item with id == change.id
    for field, value in change.fieldChanges:
        if value is [old, new]:
            # Old-value equality was checked before any mutation.
            item.fields[field] = new
        else if value.oldValueOmitted == true:
            item.fields[field] = value.newValue

append complete delta.data.items.added snapshots in their envelope order

# Install only after the entire operation succeeds.
reconstructed_snapshot.items = items
reconstructed_snapshot.packHash = delta.data.newPackHash
```

Redaction drift is one-way. If an item became more restricted, the delta may
show only the new redacted value instead of an `[old, new]` pair. Agents must
not infer or reconstruct hidden prior content. Redacted changes require
`oldValueOmitted=true`; they do not compare or recover the omitted old value.
Deleting a field through replacement likewise does not re-emit its old value.

## Rust Client Application

The existing types in `ee::core::context_delta` support local application:

```rust
// In-process callers with a typed envelope and matching baseline snapshot:
let next = delta.apply_to_snapshot(&prior)?;

// Wire consumers can deserialize data.items into ContextDeltaItems and apply
// it after independently validating the enclosing envelope and baseline:
let next_items = item_delta.apply_to_items(&prior_items)?;
```

`ContextDeltaItems::apply_to_items` validates the complete operation set and
returns a fresh ordered item vector. `ContextDeltaEnvelope::apply_to_snapshot`
also checks the schema/success/fallback/format/chaining markers, baseline hash,
baseline generation, presence of the new generation, and agreement between the
declared prior/new feature-flag hashes. Neither method mutates its input.
Missing generation metadata is rejected by the envelope method, not invented.
A supplied generation value of zero remains zero; it does not prove that a
caller collected a real database generation.

These methods reconstruct the **item snapshot projection**, not every field
of the canonical context response. The new hash is copied from the envelope;
it cannot be recomputed from this projection alone. The returned snapshot is
not marked as a server-verified ledger record, even when the prior snapshot
or envelope carries that marker. Workspace and policy isolation still belong
to the caller because `ContextDeltaPackSnapshot` does not store those values.
The methods neither authenticate network input nor widen source eligibility.

## Response Shapes

There are two valid outcomes:

- Full pack: the ordinary `ee.response.v2` context response. This happens when
  the prior hash is unknown, the requested format does not support deltas, the
  delta would be larger than the full pack, the envelope is oversized, or a
  compute budget is exceeded.
- Delta pack: an `ee.context.delta.v2` envelope with `items.added`,
  `items.removed`, `items.modified`, and `tokenSavings`.

`serverDecision.computedFromServerVerifiedPackRecord` is `true` only when the
CLI resolved the prior hash through an Available, centrally verified persisted
ledger. Public API snapshots created directly by callers emit `false`.

No-op deltas use empty arrays. There is no separate `noChange` response shape.
That keeps agents on a two-shape contract: full pack or delta.

## Format Support

Delta v2 supports JSON envelopes and markdown delta documents. Use JSON when a
consumer needs `items.added`, `items.removed`, `items.modified`, and
`tokenSavings` as structured fields. Use markdown when the delta is being
appended directly to an agent prompt. TOON, Mermaid, handoff capsules, backup
manifests, and other renderers should use full packs. If an agent requests
`--since` with an unsupported renderer, the command should return the full
renderer output with `context_delta_format_unsupported`.

## No Apply Command

`ee` should not add `ee pack apply-delta --base <hash> --delta-stdin` for
v2. Sending the base and delta back to the server defeats the byte-saving goal
and creates a second state-management surface. The Rust helpers above run
locally without creating a command or endpoint. Agents can always re-run
`ee pack "<task>" --json` without `--since` to recover the canonical full
pack.

## Retention

Pack-record retention controls how often old hashes are still usable as delta
bases. Aggressive retention settings make `context_delta_prior_unknown` more
common. A practical operating default is to keep at least the last 24 hours of
pack records or the last 100 records, whichever is larger; changing that
default is outside the delta schema contract.

## Transport

The delta envelope inherits the same trust boundary as normal local CLI output.
Do not pipe full packs or deltas across an untrusted network channel without an
external transport security layer.
