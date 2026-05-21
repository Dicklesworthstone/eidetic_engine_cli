# Agent Onboarding: Symbol Graph

The symbol graph is a Rust-first, derived index that helps `ee` connect
memories and CASS evidence to functions, types, modules, CLI handlers, and
schema constants. It is an explanation and ranking aid for agents working on
code. It is not an IDE integration, not an LSP daemon, and not a second source
of truth.

Use it when the task is tied to a changed Rust symbol and file-level context is
too broad.

## Current Workflow

Use explicit selectors when you know the function, type, or schema constant:

```bash
ee context "review changed context scoring" \
  --workspace . \
  --changed-symbol apply_changed_symbol_context_boost \
  --json
```

Use git-derived selectors when the working tree already contains the relevant
Rust edits:

```bash
ee context "find memories related to my current Rust edits" \
  --workspace . \
  --changed-symbols-from-git \
  --json
```

In JSON output, inspect pack item `why` strings for `symbolBoost`. A boosted
item names the changed symbol and why its evidence span matched. Treat the
boost as a retrieval hint; still read ordinary provenance, freshness, trust,
redaction, and degraded entries before relying on the memory.

## Schemas

The symbol graph contracts are registered schemas:

```bash
ee schema export ee.symbol_snapshot.v1 --json
ee schema export ee.symbol_evidence_links.v1 --json
```

`ee.symbol_snapshot.v1` records Rust source paths, source hashes, stable symbol
IDs, canonical names, ranges, declaration hashes, rename fingerprints, and
degraded extraction states. It does not store source bodies.

`ee.symbol_evidence_links.v1` records links from memory/CASS/failure/rule or
decision evidence to symbols. Each link includes a resolution and reason such
as `exact_symbol`, `containing_symbol`, `renamed_symbol`, `stale_span`, or
`ambiguous`.

## Operator Notes

Current `ee context --changed-symbol` behavior builds the needed Rust symbol
snapshot from referenced source files on demand. There is no required daemon,
watcher, or editor service.

If a response contains `symbol_index_stale`, read the message before retrying.
Common causes are missing file-span provenance, unreadable Rust sources, stale
line ranges, or source files that are too large for the extractor budget. The
failure-mode fixture pins the future durable repair command:

```bash
ee symbol snapshot --workspace . --refresh
```

Until that persistent command is exposed, the practical recovery is to make the
source tree and file-span provenance readable, then rerun the context command
with the same selector:

```bash
ee context "review changed context scoring" \
  --workspace . \
  --changed-symbol apply_changed_symbol_context_boost \
  --json
```

If `ambiguous_containing_symbols` appears, do not guess which symbol owns the
evidence. Narrow the evidence span or use a more specific selector.

## Degraded Fixtures

The relevant fixture files are:

- `tests/fixtures/failure_modes/symbol_index_stale.json`
- `tests/fixtures/failure_modes/ambiguous_containing_symbols.json`

The fixture catalog also documents setup commands and expected message
substrings in `docs/degraded_codes.md`.

## Privacy And Scope

Symbol snapshots store normalized paths, hashes, names, ranges, and parser
metadata. They do not copy source bodies into context packs. Evidence links
store provenance URIs and ranges, not full session content.

Rust is the implemented extraction target for v1. Do not assume TypeScript,
Python, shell, or editor/LSP coverage unless a later schema and parser
identifier explicitly say so.

## Migration Note

This docs slice adds no response fields. Existing consumers can ignore
symbol-graph metadata unless they opt into `--changed-symbol` or
`--changed-symbols-from-git`. If future work adds persistent `ee symbol`
commands or new response fields, that work must update the schema registry,
the degraded fixture catalog, and the migration notes in the same commit.
