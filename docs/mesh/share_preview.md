# Mesh Share Preview

Status: proposed
Bead: bd-1ps4c
ADR: docs/adr/0037-optional-mesh-memory.md, docs/adr/0086-team-memory-confederation.md
Payload schema: `ee.mesh.share_preview.v2`

## Purpose

`ee share preview` is the operator review step before widening outbound mesh
sharing. It reports what would be shared with a peer under the current
workspace, command flags, and — as of `ee.mesh.share_preview.v2` — the peer's
**real outbound policy**, without exporting data.

Run it before enabling a peer, materializing a lane grant, or performing an
export:

```bash
ee share preview --peer peer_alpha --workspace . --json
```

The default is metadata-only. Body and embedding lanes are counted as denied
until explicitly requested:

```bash
ee share preview --peer peer_alpha --include-body --workspace . --json
ee share preview --peer peer_alpha --include-embeddings --workspace . --json
```

## Policy-backed verdicts

Each candidate's `policyAction` (`allow`/`deny`) now comes from the same
outbound peer-policy engine that governs `ee mesh export`
(`decide_outbound` over the `[[mesh.peer_policies]]` registry), not from a
simulated "allow". A lane is `allow` only when the operator requested it (the
`--include-*` flag), the peer policy permits that lane from the memory's origin
workspace, and — for the body lane — the content is not secret-like. The
preview therefore predicts what an export to this peer would actually do.

If the target peer has **no resolvable outbound policy** (no matching
`[[mesh.peer_policies]]` entry, or an ambiguous match), the preview fails
closed: every lane denies and the response carries a `share_preview_peer_unknown`
degraded entry (severity `warning`) telling the operator the peer is
unconfigured rather than showing a misleading allow.

## Safety Contract

- The command is always a dry run: `dryRun=true` and `exportPerformed=false`.
- The command is strictly read-only: it never records consent or writes an
  audit row (see [Consent](#consent)).
- Representative examples identify the memory and its stored entity revision,
  but expose neither raw memory bodies nor content-derived hashes.
- The report and its events contain no aggregate preview hash. A preview is
  review evidence, not a stable bearer or content-equality oracle.
- Counts are grouped by memory level, kind, trust class, material lane,
  redaction class, and policy action.
- Exposure estimates split metadata, body, and embedding bytes and reflect the
  real policy verdict (a policy-denied lane contributes zero bytes).
- Denied lanes and redaction classes are listed in `deniedClasses[]`.
- The response emits `preview_generated` and `export_not_performed` events.
- An unconfigured peer yields a `share_preview_peer_unknown` degraded entry.

## Consent

`share preview` is strictly read-only and never records consent. The former
`--record-consent` flag (and its `consentAudit` response block and
`mesh.share.consent` audit row) was removed per ADR 0086 (TC-D14/TC-D15):
recording a consent row from a read-only preview conflated review with
authorization and produced a stable, replayable audit artifact.

Consent to widen sharing is instead expressed by the token-consuming exposure
grant mutation (documented separately), which is the only surface that
authorizes an export. The preview command exists solely to let an operator see
what *would* be shared before invoking that mutation.

## Fields To Inspect

For metadata-only sharing, verify:

- `data.preview.exportableCount` is limited to metadata candidates.
- `data.preview.estimatedBodyBytes == 0`.
- `data.preview.estimatedEmbeddingBytes == 0`.
- `data.preview.deniedClasses[]` includes body and embedding denial classes.

Before granting body sharing, verify:

- `--include-body` was intentional.
- `data.preview.examples[]` contain `memoryId`, `entityRevision`, and redaction
  placeholders only; they contain no content-derived hash.
- `data.preview.deniedClasses[]` does not include
  `redaction_class:body_redacted`; if it does, the raw body lane remains
  denied and only metadata is exportable.
- Authorization is performed only by the separate token-consuming grant
  mutation; no field from this ordinary preview is a reusable authorization
  token.

Embedding sharing is sensitive even without raw bodies. Keep
`--include-embeddings` off unless the peer policy and operator review both allow
semantic leakage.
