# Agent Guide: Streaming Context Packs

`ee context --stream --format json` emits newline-delimited JSON frames using
the `ee.pack.stream.v1` schema. The stream is an agent-facing transport for the
same deterministic pack that `ee context --json` returns in one batch.

Use streaming when first-use latency matters more than waiting for the complete
pack envelope. Do not use it as a different ranking mode. The item order,
selected memories, and trailer `packHash` must match the non-streaming context
pack for the same workspace, database generation, index generation, config,
query, profile, memory scope, and token budget.

## Frame Order

Consumers must parse one JSON object per line and handle frames by `kind`:

```text
header
item...
trailer
```

Failure paths emit either:

```text
header
item...
error
```

or, when cancellation is detected after some output:

```text
header
item...
cancelled
```

The header frame is emitted before pack assembly work that can block on graph
or search state. Item frames are emitted in the same rank order as the batch
pack. The trailer is authoritative for `packHash`, final degraded signals,
quality, selection audit, and optional Pack DNA.

## Minimal Consumer

```bash
ee context "prepare release" --workspace . --stream --format json |
  while IFS= read -r line; do
    kind=$(printf '%s\n' "$line" | jq -r '.kind')
    case "$kind" in
      header)  printf 'pack %s started\n' "$(printf '%s\n' "$line" | jq -r '.packId')" ;;
      item)    printf '%s\n' "$line" | jq -r '.content' ;;
      trailer) printf 'hash %s\n' "$(printf '%s\n' "$line" | jq -r '.packHash')" ;;
      error|cancelled) printf '%s\n' "$line" | jq -r '.error.message' >&2; exit 1 ;;
    esac
  done
```

Production harnesses should avoid shell parsing and decode each line against
`docs/schemas/ee.pack.stream.v1.json`.

## Determinism Rules

- Treat `packId` and timestamps as stream metadata, not pack identity.
- Compare trailer `packHash` with batch `data.pack.hash` when validating a
  fixture.
- Preserve item order exactly as emitted; do not sort by score client-side.
- A stream with the same deterministic inputs must emit the same item sequence
  as batch mode.
- Unknown `kind` values are a schema violation for `ee.pack.stream.v1`.

## Error Handling

If a command fails before the header can be emitted, `ee` may return the normal
`ee.error.v2` envelope and a non-zero exit code. After a header is emitted, the
stream must end with `trailer`, `error`, or `cancelled`.

On `error` or `cancelled`, consumers should keep any already-emitted items only
as partial context. Do not cache a partial stream as a complete pack. Use
`emittedItems` and `degraded[]` to explain what was usable before failure.

## Pack DNA

With `--explain`, item frames still stream in rank order. Pack DNA depends on
the final selection and appears only in the trailer. A missing `packDna` field
does not make the stream invalid; inspect `degraded[]` for graph explanation
gaps.

## Implementation Expectations

The implementation should flush stdout after every frame. This matters for
agent harnesses that begin prompt construction as soon as the first item
arrives.

Required tracing fields for implementation and e2e logs:

- `workspace_id`
- `request_id`
- `surface=context_stream`
- `phase=input|dependency_check|dispatch|item_emit|trailer|error`
- `elapsed_ms`
- `degraded_codes`

## Validation Checklist

- Validate each emitted line against `ee.pack.stream.v1`.
- Assert exactly one header and exactly one terminal frame.
- Assert every item `seq` starts at 0 and increments by 1.
- Assert every item `rank` starts at 1 and increments by 1.
- Assert trailer `totalItems` equals emitted item count.
- Assert trailer `packHash` equals the batch pack hash for the same fixture.
- Assert error and cancelled streams are never accepted as complete packs.
