# ADR 0042: Symbol Graph Derived Index

Status: proposed
Date: 2026-05-21
Bead: bd-2xuu7.3

## Context

`ee` increasingly uses code-location provenance to explain why a memory is
useful for an agent editing a particular function, type, module, schema, or CLI
surface. File-level recall is too coarse for large Rust workspaces: a session
note about `src/core/context.rs` may matter only for
`apply_changed_symbol_context_boost`, while another note in the same file may
belong to packing, redaction, or workspace discovery.

The symbol-graph work introduces a read-only layer that extracts Rust symbols,
links memory/CASS evidence spans to those symbols, and lets `ee context` boost
memories tied to changed symbols. The layer must preserve the core project
rules from ADR 0001, ADR 0002, ADR 0005, and ADR 0008:

- `ee` remains a local-first CLI memory substrate, not an IDE, daemon, or agent
  harness.
- FrankenSQLite and SQLModel remain the source of truth; symbol snapshots are
  derived assets.
- CASS remains the raw session source; symbol links attach provenance to
  evidence, not a second session store.
- Graph metrics and symbol projections are rebuildable explanations, not
  authoritative memory state.

## Decision

The symbol graph is a **derived, Rust-first, read-only index** over source
files and evidence spans. It may influence retrieval explanations and ranking,
but it does not mutate memories, does not replace file provenance, and does not
require an editor daemon or LSP server for v1.

The contract has five parts:

1. **Stable symbol records.** A symbol record has a deterministic
   `sym_v1_<hash>` ID, canonical name, namespace, source range, visibility,
   declaration hash, rename fingerprint, parser kind, and normalized path. The
   schema is `docs/schemas/ee.symbol_snapshot.v1.json`.
2. **Rename tolerance is evidence-based.** If an evidence link carries an
   expected symbol ID and the current snapshot no longer contains that ID, the
   resolver may match a current symbol by `renameFingerprint`. That match is
   degraded as `symbol_renamed`; it is not silent.
3. **Evidence links preserve uncertainty.** Memory, CASS evidence, failure,
   rule, and decision spans resolve to `exact_symbol`, `containing_symbol`,
   `file_level`, `stale_span`, `ambiguous`, `renamed_symbol`,
   `deleted_symbol`, or `source_file_missing`. The schema is
   `docs/schemas/ee.symbol_evidence_links.v1.json`.
4. **Context boosting is bounded and explainable.** `ee context` accepts
   `--changed-symbol <selector>` and `--changed-symbols-from-git`. Matching
   memories get a bounded relevance boost and a `symbolBoost` reason string
   that names the changed symbol and link reason.
5. **Staleness is surfaced, not guessed through.** If source files are missing,
   unreadable, too large, stale, or ambiguous, responses use degraded codes such
   as `symbol_index_stale` or `ambiguous_containing_symbols`. Agents must read
   those degraded entries before treating symbol evidence as precise.

## Privacy Boundaries

Symbol snapshots store hashes, ranges, names, and normalized paths. They do not
store source bodies. Evidence links store provenance URI, target range,
resolution, reason, and confidence; they do not copy the referenced code or
session body.

The index is local to the workspace. No cloud service, paid model, editor
plugin, or background watcher is required to build or consume the v1 surface.
If a future integration reads from an IDE or LSP server, it must be optional and
must produce the same schema with the same privacy constraints.

## Current Public Surface

The implemented agent-facing selector surface is:

```bash
ee context "review changed context scoring" \
  --workspace . \
  --changed-symbol apply_changed_symbol_context_boost \
  --json

ee context "review current Rust edits" \
  --workspace . \
  --changed-symbols-from-git \
  --json
```

The schema registry exposes the derived contracts:

```bash
ee schema export ee.symbol_snapshot.v1 --json
ee schema export ee.symbol_evidence_links.v1 --json
```

The durable operator command named in failure-mode fixtures,
`ee symbol snapshot --workspace . --refresh`, is the intended repair command
for a future persistent symbol index. Until that command is exposed, current
context boosting builds the needed Rust snapshot from referenced source files
on demand and reports `symbol_index_stale` when it cannot do so.

## Degraded Codes

The current fixture catalog pins these symbol-specific behaviors:

| Code | Fixture | Meaning |
| --- | --- | --- |
| `symbol_index_stale` | `tests/fixtures/failure_modes/symbol_index_stale.json` | The current symbol evidence may lag the workspace source or cannot be rebuilt from the available spans. |
| `ambiguous_containing_symbols` | `tests/fixtures/failure_modes/ambiguous_containing_symbols.json` | Multiple symbols matched an evidence span with equal specificity, so the resolver refused to guess. |

Additional link-level degradation codes live in
`docs/schemas/ee.symbol_evidence_links.v1.json`, including
`stale_line_span`, `source_file_missing`, `symbol_renamed`, and
`symbol_deleted`.

## Invariants

- Symbol extraction is deterministic for the same normalized source inputs.
- Snapshot hashes do not include volatile timestamps.
- Symbol IDs and link IDs are content/provenance hashes, not database row IDs.
- Source bodies are never embedded in the symbol snapshot or evidence-link
  schema.
- Ambiguous, stale, deleted, renamed, missing, too-large, and unreadable source
  states are visible degraded states.
- Context symbol boosts are ranking hints only; they do not prove a memory is
  correct and do not suppress normal provenance, redaction, freshness, trust, or
  relevance checks.
- Rust is the only implemented extraction target in v1. Multi-language coverage
  requires a later bead and schema-compatible parser identifier.

## Rejected Alternatives

1. **Mandatory IDE/LSP integration.** Rejected because it would make the core
   CLI depend on a separate process and editor state. LSP may become an
   optional adapter later, but the v1 contract is one-shot and local.
2. **Persisting copied source bodies in the index.** Rejected because the
   symbol graph is an explanatory derived asset, not another code store.
3. **Treating rename fingerprints as authoritative.** Rejected because hashes
   can collide semantically when signatures stay similar. Rename matches remain
   degraded evidence.
4. **Multi-language support in the first slice.** Rejected because Rust-first
   extraction already proves the schema and ranking contract without pretending
   to cover TypeScript, Python, or shell.

## Consequences

- Agents can ask for context around a changed Rust function or type without
  needing whole-file recall.
- Operator documentation must point agents at the schemas and degraded fixtures
  before they trust symbol evidence.
- Future persistent index or `ee symbol` CLI work must honor these schemas and
  keep `symbol_index_stale` repair guidance aligned with the fixture catalog.
- Response schema additions from symbol work require normal schema drift review;
  this docs bead adds no new emitted fields.
