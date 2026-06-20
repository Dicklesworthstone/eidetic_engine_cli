# Mesh Share Preview

Status: proposed
Bead: bd-1ps4c
ADR: docs/adr/0037-optional-mesh-memory.md

## Purpose

`ee share preview` is the operator review step before widening outbound mesh
sharing. It reports what would be shared with a peer under the current
workspace and command flags without exporting data.

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

## Safety Contract

- The command is always a dry run: `dryRun=true` and `exportPerformed=false`.
- Representative examples are redacted and include content hashes, not raw
  memory bodies.
- Counts are grouped by memory level, kind, trust class, material lane,
  redaction class, and policy action.
- Exposure estimates split metadata, body, and embedding bytes.
- Denied lanes and redaction classes are listed in `deniedClasses[]`.
- The response emits `preview_generated` and `export_not_performed` events.

## Consent Audit

When an operator has reviewed the preview, record that consent without exporting
anything:

```bash
ee share preview --peer peer_alpha --include-body --record-consent --workspace . --json
```

This writes a local audit row with action `mesh.share.consent` and returns a
`consentAudit` block containing the audit id, preview hash, reason, and
`exportAfterConsent=false`. A later export command must still perform its own
policy check; the preview command does not export data.

## Fields To Inspect

For metadata-only sharing, verify:

- `data.preview.exportableCount` is limited to metadata candidates.
- `data.preview.estimatedBodyBytes == 0`.
- `data.preview.estimatedEmbeddingBytes == 0`.
- `data.preview.deniedClasses[]` includes body and embedding denial classes.

Before granting body sharing, verify:

- `--include-body` was intentional.
- `data.preview.examples[]` contain redaction placeholders only.
- `data.preview.deniedClasses[]` does not include
  `redaction_class:body_redacted`; if it does, the raw body lane remains
  denied and only metadata is exportable.
- `data.previewHash` is captured in the consent audit row.

Embedding sharing is sensitive even without raw bodies. Keep
`--include-embeddings` off unless the peer policy and operator review both allow
semantic leakage.
