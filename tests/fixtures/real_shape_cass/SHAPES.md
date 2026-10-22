# Real-shape CASS oracle corpus — provenance of THIS committed tree

bd-reality-core-convergence-1azkt.43, deliverable 2. The field-by-field record
tables live beside the generator, in
[`scripts/real_shape_corpus/SHAPES.md`](../../../scripts/real_shape_corpus/SHAPES.md),
deliberately not copied here: the tables and the renderer must move together, and
a second copy is the kind that drifts. What this file records is the provenance of
the bytes actually committed under this directory.

## Exactly how to reproduce this tree

    python3 scripts/real_shape_corpus/generate.py \
        --out tests/fixtures/real_shape_cass \
        --target-records 4000 --digest

Byte-identical for the same arguments: ids come from a seeded RNG and timestamps
from a fixed epoch, never from the clock. Verified by generating twice, to two
different destinations, and comparing:

    tree digest   7bcedd76fcc6c3e9913fa5d30efe54ffccd43eeea72299d56ce581ff65753faa

## What is here

| | |
| --- | --- |
| sessions | 308 (14 authored core + seeded recombinations) |
| records | 4009 |
| labels | 66 |
| files | 310 (308 `.jsonl` + `manifest.json` + `judgments.json`) |
| on disk | 5.84 MiB |

## Upstream shapes this mirrors

Pinned in `manifest.json` rather than only in prose, so a reader can assert on
them:

| key | value |
| --- | --- |
| `schema` | `ee.real_shape_cass.manifest.v1` |
| `claude_code_version` | 2.0.14 |
| `codex_cli_version` | 0.46.0 |
| `seed` | 43 |
| `target_records` | 4000 |
| `total_records` | 4009 |

## Checks run against this tree before it was committed

- `generate.validate(<this dir>)` — **0 problems**. It walks every record and
  checks it against the envelope table, so the fixture cannot quietly drift into
  the "clean" one-line shapes this corpus exists to replace.
- The private-fleet-identifier predicate from `tests/fleet_identifier_guard.rs`,
  replicated over all 310 files: **0 hits** for any of its 14 banned host words,
  for `vmi<digits>` host names, and for routable IPv4 addresses. The corpus is
  authored, never copied from a private session, and this is the check that says
  so rather than the claim.

Note that the corpus DELIBERATELY contains secret-shaped strings and
instruction-like text, because redaction and prompt-injection quarantine are
among the properties measured against it. Those are authored decoys; they are
not real credentials.

## Why the scale variant is the committed form

The authored core alone is 14 sessions and 123 records. The failure this corpus
was built to measure — retrieval and pack latency going from about 1.2 s to
about 11 s — appeared at roughly 4,900 spans, so a 123-record tree cannot
reproduce it and a measuring stick that cannot reach the regime is not one.
`scripts/real_shape_corpus/SHAPES.md` described scale variants as "never
committed" until 2026-10-22; that line was written while nothing at all was
committed, and it is corrected there.

Regenerating the core alone (`--out <dir>` with no `--target-records`) remains
the right way to review the authored CONTENT, which is 14 reviewable sessions
rather than 308 recombined ones.
