# Context Delta Apply Guide

Delta payloads add to your prompt; they do not replace the base pack.

`ee context --since <pack-hash> --json` is a JSON-only transport optimization
for long agent sessions. The normal context pack is still the canonical state.
The delta envelope describes how to update a pack that the agent already has in
memory.

## Base Hash

Pass a hash from a prior `ee context --json` response's `data.pack.hash`.
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

Use item ids as keys. Keep the prior pack's item order for unchanged items,
remove ids listed in `items.removed`, merge `items.modified` field changes into
matching items, then append `items.added` in envelope order unless a later pack
ordering field says otherwise.

Pseudo-code:

```text
items_by_id = prior_pack.items keyed by item.id

for id in delta.items.removed:
    delete items_by_id[id]

for change in delta.items.modified:
    item = items_by_id[change.id]
    for field, value in change.fieldChanges:
        if value is [old, new]:
            item[field] = new
        else if value.oldValueOmitted == true:
            item[field] = value.newValue

for item in delta.items.added:
    items_by_id[item.id] = item

reconstructed_pack.items = stable order from prior pack, minus removed ids,
then added items in delta order
reconstructed_pack.hash = delta.data.newPackHash
```

Redaction drift is one-way. If an item became more restricted, the delta may
show only the new redacted value instead of an `[old, new]` pair. Agents must
not infer or reconstruct hidden prior content.

## Response Shapes

There are two valid outcomes:

- Full pack: the ordinary `ee.response.v2` context response. This happens when
  the prior hash is unknown, the requested format does not support deltas, the
  delta would be larger than the full pack, the envelope is oversized, or a
  compute budget is exceeded.
- Delta pack: an `ee.context.delta.v1` envelope with `items.added`,
  `items.removed`, `items.modified`, and `tokenSavings`.

No-op deltas use empty arrays. There is no separate `noChange` response shape.
That keeps agents on a two-shape contract: full pack or delta.

## Format Support

Delta v1 is JSON-only. Markdown, TOON, Mermaid, handoff capsules, and backup
manifests should use full packs. If an agent requests `--since` with an
unsupported renderer, the command should return the full renderer output with
`context_delta_format_unsupported`.

## No Apply Command

`ee` should not add `ee context apply-delta --base <hash> --delta-stdin` for
v1. Sending the base and delta back to the server defeats the byte-saving goal
and creates a second state-management surface. Agents can always re-run
`ee context "<task>" --json` without `--since` to recover the canonical full
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
