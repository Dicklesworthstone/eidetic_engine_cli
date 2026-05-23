# Context Pack Compression Manifests

This document defines the contract for zstd-compressed pack artifacts before
the hot pack write paths change. It covers L2 cache entries, binary pack
frames, and future compressed pack-record ledger sidecars.

Compression is a derived storage or transport optimization. It never changes
pack selection, redaction, replay, or existing canonical hash meaning.

## Schema

The manifest schema is `ee.pack.compression_manifest.v1` at
`docs/schemas/ee.pack.compression_manifest.v1.json`.

Every compressed artifact has one manifest. The manifest records:

- `manifestId`: `packcm_` plus the BLAKE3 hash of the manifest identity object.
- `dictionary.dictionaryId`: `zstd_dict_` plus the BLAKE3 hash of raw dictionary
  bytes.
- `dictionary.dictionarySourceHash`: BLAKE3 over the deterministic training
  corpus manifest.
- `codec`, `codecVersion`, and `compressionLevel`.
- `artifact.uncompressedByteHash` and `artifact.compressedByteHash`.
- `createdAt`, `corpusWindow`, compatibility flags, storage location, fallback
  behavior, and redaction status.

`createdAt` is audit metadata only. It is required in the manifest record, but
it is excluded from `manifestId` and from every pack, binary, and ledger hash.

## Canonical Hashes

Compression must preserve the existing hash contracts:

- `data.pack.hash` and persisted `pack_records.pack_hash` are hashes of the
  existing uncompressed pack content components: request, output options,
  selected and omitted item data, provenance, trust, degradations, rendered
  text when included, and snapshot or coordination inputs when they affect
  output. Compression fields never enter this hash.
- `pack_records.ledger_hash` is the hash of the uncompressed redaction-safe
  replay ledger JSON.
- `ee.pack.bin.v1` `content_hash` is the BLAKE3 hash of the uncompressed
  canonical JSON footer embedded in the binary frame.
- `artifact.uncompressedByteHash` hashes the exact bytes existing readers
  consume.
- `artifact.compressedByteHash` hashes the compressed byte stream and is only
  an integrity check for the compressed representation.

A compressed cache hit or replay read is valid only if decompressing produces
the recorded `uncompressedByteHash`. If the artifact also carries a pack hash,
ledger hash, or binary content hash, that value must still match the
uncompressed bytes.

## Manifest Identity

`manifestId` is computed from stable JSON bytes for a manifest identity object.
The identity object is the manifest without:

- `manifestId`
- `createdAt`
- `storage`
- `operationalNotes`

Object keys are sorted by UTF-8 lexical order. Arrays whose order is semantic,
such as `storage.dbReference.columns`, preserve schema order. Arrays that model
sets must be sorted lexicographically before hashing. Hash strings use lowercase
hex with the `blake3:` prefix unless the field itself defines an ID prefix such
as `packcm_` or `zstd_dict_`.

The identity object includes the artifact hashes. That means two compressed
artifacts built with the same dictionary but over different uncompressed bytes
have different `manifestId` values and the same `dictionaryId`.

## Storage Decision

The authoritative manifest is a sidecar file:

```text
.ee/derived/pack-compression/manifests/<manifestId>.json
```

Pack DB rows and L2 cache entries may store nullable references to
`manifestId`, `dictionaryId`, and `artifact.kind`, but the sidecar is the
canonical manifest payload. This keeps pack-record migrations small and lets
rollback ignore compression metadata without rewriting historical pack records.

Publish uses the existing derived-asset pattern: write a temp file, fsync it,
rename into place, then fsync the parent directory. Readers must treat absent
or unreadable sidecars as a cache miss or an uncompressed fallback, not as pack
selection failure.

Migration and rollback rules:

- New DB metadata must be nullable and additive.
- Existing `pack_records.pack_hash` and `pack_records.ledger_hash` remain
  unchanged.
- Rollback ignores the manifest reference and reads the uncompressed source.
- A failed compression write must not roll back or mutate the pack record.
- Stale or corrupt manifests are not trusted for replay; the uncompressed pack
  or ledger remains authoritative.

## L2 Cache References

Compressed L2 entries reference the manifest but do not change the canonical
key contract from `docs/configuration/cache.md`.

The canonical L2 key is still the hash of all inputs that can affect emitted
JSON. Compression settings, dictionary IDs, and sidecar paths are not allowed
to make a semantically different pack look like a different context answer. On
read, the cache implementation may choose any valid compressed representation
for that key as long as decompression verifies `artifact.uncompressedByteHash`
and the emitted JSON is byte-identical to a fresh uncompressed response.

## Fallback Codes

The child implementation beads must add failure-mode fixtures and taxonomy rows
before these codes are emitted by Rust code:

| Code | Severity | Behavior |
| --- | --- | --- |
| `pack_compression_manifest_missing` | `low` | Read the uncompressed source or miss the cache. |
| `pack_compression_manifest_corrupt` | `warning` | Ignore the manifest and read the uncompressed source. |
| `pack_compression_dictionary_missing` | `low` | Read the uncompressed source; selected items are unchanged. |
| `pack_compression_dictionary_stale` | `low` | Miss the cache and reassemble with the current corpus window. |
| `pack_compression_hash_mismatch` | `high` | Reject the compressed artifact and use uncompressed recovery if available. |
| `pack_compression_codec_unsupported` | `warning` | Read the uncompressed source or ask for a newer reader. |

These are response-time degradations. They describe a storage or transport
shortcut failing; they must not imply changed selection.

## Child Bead Handoff

`bd-1prrl.5.2` should train dictionaries and prove deterministic
`dictionaryId` and `dictionarySourceHash` generation from the ordered corpus
window.

`bd-1prrl.5.3` should wire compressed L2 reads and writes so cache hits emit
the same JSON bytes as a fresh response for the same canonical key.

`bd-1prrl.5.4` should add compressed pack-record ledger sidecars while keeping
`pack_records.ledger_json` and `pack_records.ledger_hash` replay-compatible.

All implementation beads need RCH-only Cargo proof once Rust code lands.
