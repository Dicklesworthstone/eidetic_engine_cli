# Provenance Attestation Bundles

Agent-facing companion for the attestation surface introduced under
`bd-1n0np.22`. An **AttestationBundle** is ee's single, canonical,
redaction-safe *chain-of-custody* object for a memory, a pack, or a query. It is
an internal primitive that other surfaces (support-bundle, handoff, pack-replay,
`why`) are meant to be built **on top of** — not another export format.

The design invariant: an attestation attests **local ee evidence custody and
hash consistency, NOT objective truth**. It says "here is exactly what ee holds
about this subject, and here is a deterministic hash over it" — never "this fact
is true."

## Existing Anchors

| Anchor | Role |
| --- | --- |
| `src/models/attestation.rs` | The canonical `AttestationBundle` model (schema `ee.attestation.bundle.v2`) + evidence/redaction/hash manifests and the optional seal block. |
| `src/core/attest.rs` | `core::attest` builders for memory / pack / query bundles. |
| `src/cli/mod.rs` (`ee attest`) | `ee attest memory \| pack \| query --json` surface (schema `ee.attest.v1`). |
| `docs/schemas/ee.attest.v1.json` | JSON contract for the `ee attest` response. |
| `scripts/e2e_attestation.sh` | Real-binary e2e: deterministic redaction-safe bundle + zero secret leakage (`bd-1n0np.22.6`). |

## The command

```bash
ee attest memory <memory-id> --workspace . --json
ee attest pack   <pack-id>   --workspace . --json
ee attest query  "<text>"    --workspace . --json
```

Each emits the `ee.response.v2` envelope around an `ee.attest.v1` payload:

| Field | Meaning |
| --- | --- |
| `bundleHash` | `blake3:…` over the canonical bundle. **Deterministic** for a fixed subject + DB. |
| `subjectKind` | `memory`, `pack`, or `query`. |
| `subjectId` | The memory id, pack id, or query text. |
| `rawTextIncluded` | Whether raw subject text is embedded. Default **false** (hash-only / redaction-safe). |
| `objectiveTruthAttested` | Always false — custody + hashes only, never truth. |
| `trustStatement` | The human-readable scope of the claim. |
| `bundle` | The full `ee.attestation.bundle.v2` manifest (subject, evidence, redaction, hashes, omissions; sealed memories additionally carry `seal` = contentCommitment/sealedAt/revealedAt/revealVerified so commit-before-outcome verifies offline). |

## What an agent can rely on

- **Deterministic.** Two attests of the same subject over the same database
  reproduce the identical `bundleHash`. Use it as a stable content-address for a
  subject's current chain of custody.
- **Redaction-safe by default.** `rawTextIncluded` is false; secrets in a
  memory body do **not** leak into the bundle — the evidence is hash-referenced,
  and any redaction is recorded in `redactionManifest` (never silent). The
  `bd-1n0np.22.6` e2e asserts zero secret leakage.
- **Honest about scope.** `objectiveTruthAttested` is always false and
  `trustStatement` spells out that the bundle attests local custody + hash
  consistency, not the truth of the memory.
- **No omissions hidden.** Anything left out of the bundle is listed in
  `omissions` — the no-silent-loss rule.

## One canonical object across surfaces

The point of the bundle is that **the same `bundleHash` should appear wherever a
surface references a subject's custody** — a support-bundle, a handoff package,
a pack-replay ledger entry, or a `why` explanation. That consumer wiring is
`bd-1n0np.22.3`; until it lands, the surfaces still produce their own evidence
views, and `scripts/e2e_attestation.sh` capability-guards the
"embedded hash == standalone hash" assertion so the gap is visible, never a
false pass. Once 22.3 wires the embedding, that assertion activates and proves
the de-duplication the bundle exists to provide.
