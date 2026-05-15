# README Invariant Manifest Schema

`tests/readme_invariants/manifest.toml` is the source of truth for README
claims that need a test, benchmark, verifier, or explicit follow-up bead.

The manifest is intentionally TOML because this repository already uses TOML
for Cargo metadata, configuration, and benchmark budgets.

## Top-Level Fields

```toml
schema = "ee.readme_invariants.v1"

[scrubber]
denylist_regexes = [
  "(?i)must (be|have) been",
]
```

`schema` is required and must equal `ee.readme_invariants.v1`.

`scrubber.denylist_regexes` is required. These regular expressions suppress
known false positives when the later extraction harness scans README prose for
words such as `must`, `never`, `every`, `deterministic`, or numeric units.

## Invariant Entries

Each invariant uses a TOML table array:

```toml
[[invariant]]
id = "rfm-001-no-silent-mutation"
readme_section = "Design Philosophy"
readme_line_anchor = 192
sentence_hash = "blake3:<64 lowercase hex chars>"
classification = "invariant"
verify = { type = "test", path = "tests/some_test.rs::test_name" }
```

Required fields:

- `id`: stable slug matching `^[a-z0-9][a-z0-9-]*$`.
- `readme_section`: README heading text that owns the claim.
- `readme_line_anchor`: 1-based README line number containing the canonical
  anchor text.
- `sentence_hash`: BLAKE3 hash of the canonical anchor text at
  `readme_line_anchor`, formatted as `blake3:<64 lowercase hex chars>`.
- `classification`: one of `quantitative`, `invariant`, `promise`, or
  `constraint`.
- `verify`: inline table describing how the claim is covered.

Canonical anchor text is the README line at `readme_line_anchor`, trimmed at
both ends and with internal whitespace collapsed to single spaces. For Markdown
table rows, the complete row is the anchor text.

## Verification

`verify.type = "test"` points to an existing test, benchmark, verifier, or
script path. `verify.type = "defer_bead"` points to an open follow-up bead
that owns the missing implementation or coverage.

Examples:

```toml
verify = { type = "test", path = "tests/readme_perf_sync.rs" }
verify = { type = "defer_bead", id = "bd-3usjw.22", defer_until = "2026-08-13" }
```

The schema contract test (`tests/readme_invariant_manifest_schema.rs`)
validates the manifest shape, stable IDs, line anchors, hash format, hash
value, and verification target form. It enforces two drift gates:

- `verify.type = "test"`: the referenced file must exist on disk under
  `tests/` or `scripts/` relative to `CARGO_MANIFEST_DIR`. A manifest
  entry pointing at a deleted test file fails CI.
- `verify.type = "defer_bead"`: the referenced `bd-*` ID must appear in
  `.beads/issues.jsonl` with a non-`closed` status. A `defer_bead` entry
  whose target bead has been closed fails CI — that signals the manifest
  entry should be migrated to a real `test` path (now that coverage
  exists) or repointed at a different open bead.
