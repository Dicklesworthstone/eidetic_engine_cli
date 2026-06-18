# ADR 0074: Embedding Model Lifecycle Readiness

Status: proposed
Date: 2026-06-14
Bead: bd-1iupc.1 (epic bd-1iupc, 2026-06 idea-wizard wave)

## Context

`ee` already has three separate views of semantic retrieval readiness:

- the model registry records configured local model rows and stable embedding
  metadata (`ee.model_registry.v1`, `ee.embedding.metadata.v1`, and
  `ee.semantic_model_admissibility.v1`);
- Frankensearch owns model selection and hybrid retrieval execution
  (ADR 0016), including lexical fallback when embeddings cannot run;
- index status reports whether the derived search index is missing, stale, or
  corrupt.

Those surfaces are individually useful, but the next model-lifecycle work needs
one machine contract that can answer: "Can semantic retrieval use this model
and this index right now, and if not, why?" The answer must distinguish a cold
but valid model from missing assets, corrupt assets, dimension mismatches,
indexes built with a stale model, unsupported build features, and honest
lexical fallback.

ADR 0080 makes the normal default model concrete for this lifecycle contract:
`potion-multilingual-128M` via Frankensearch Model2Vec. The lifecycle report
still observes rather than selects models, but default-install docs and doctor
messages should treat that model as the expected local semantic path.

## Decision

### 1. Add `ee.model_lifecycle.v1`

The normative schema is
[`docs/schemas/ee.model_lifecycle.v1.json`](../schemas/ee.model_lifecycle.v1.json).
It is an observation contract for a read-only collector, not a new runtime
selection engine. The collector bead (`bd-1iupc.2`) will populate it from:

- model registry rows and embedding metadata;
- Frankensearch capability/admissibility observations;
- semantic index metadata such as stored dimension, model revision/hash, and
  last rebuild timestamp.

The top-level report contains `semanticReadiness`, `models[]`, `indexes[]`,
and `degraded[]`. It uses the standard redaction posture
`paths_workspace_relative_or_hashed_no_content`; raw model paths are never part
of the contract.

### 2. Lifecycle states are closed for v1

Every model row, index row, and readiness summary uses the same closed state
enum:

`available`, `cold`, `warming`, `missing`, `corrupt`, `dimension_mismatch`,
`stale_index_model`, `lexical_fallback`, `unsupported_feature`, `unknown`.

State meanings:

| State | Meaning |
|---|---|
| `available` | The model/index pair is usable for semantic retrieval now. |
| `cold` | Assets and metadata are valid, but the model has not been warmed or loaded. |
| `warming` | A load, warmup, or rebuild is in progress, so readiness is not final. |
| `missing` | A required model asset, registry row, manifest, or index asset is absent. |
| `corrupt` | An asset exists but fails checksum, manifest, decode, or index integrity checks. |
| `dimension_mismatch` | Model embedding dimensions or compatible vector metadata do not match the index. |
| `stale_index_model` | The index was built with a different model hash, revision, dimension, or metric. |
| `lexical_fallback` | Semantic retrieval cannot run, but lexical retrieval can still answer honestly. |
| `unsupported_feature` | The build or platform lacks a required semantic capability. |
| `unknown` | Evidence was insufficient; consumers must not infer readiness. |

### 3. Provenance is asset-level and redaction-safe

Each model row has an `assetProvenance` object with `sourceKind`,
`sourceUri`, `registryEntryId`, `modelRevision`, `contentHash`, `assetHash`,
`manifestHash`, `checkedAt`, and `provenanceComplete`.

Hashes use the existing `blake3:<64-hex>` shape. `sourceUri` is either `null`,
workspace-relative, or `hashed:<blake3-12>` for host-private paths. The schema
does not allow absolute paths. Provenance answers what was observed, not which
model Frankensearch should choose.

### 4. Dimension compatibility is explicit

Dimension compatibility is a first-class object on readiness, model rows, and
index rows:

- `expectedDimension`: selected/current model dimension;
- `actualDimension`: observed asset or query embedder dimension;
- `indexDimension`: stored semantic index dimension;
- `distanceMetric` and `vectorDtype`: the compatibility-relevant vector
  metadata;
- `compatible`: the final boolean, or `null` when evidence is incomplete;
- `rule`: one of `exact_dimension_metric_dtype`, `lexical_no_dimension`,
  `unsupported_feature`, or `unknown`;
- `mismatchReason` and `repair`: bounded machine-readable repair context.

The v1 rule is deliberately strict: semantic readiness requires matching
dimension, distance metric, and vector dtype between the selected model and the
semantic index. Unknown evidence degrades to lexical fallback or blocked
readiness; it never silently passes.

### 5. Degraded vocabulary maps state to existing surfaces

`ee.model_lifecycle.v1` defines a bounded degraded-code enum covering the
closed lifecycle states and the existing model/index vocabulary:

- lifecycle-specific: `model_lifecycle_cold`,
  `model_lifecycle_warming`, `model_asset_missing`, `model_asset_corrupt`,
  `model_dimension_mismatch`, `stale_index_model`, `lexical_fallback`,
  `unsupported_feature`, `model_lifecycle_unknown`;
- existing registry/search/index codes: `model_registry_empty`,
  `model_registry_no_available_entry`, `semantic_model_unavailable`,
  `semantic_dimension_exceeds_budget`, `index_missing`, `index_corrupt`,
  `index_stale`, `search_index_degraded`.

The follow-on collector may include several degraded entries, but must choose
one lifecycle state per model/index/readiness row.

## Composition Rules

- **Model registry remains source of truth for local model rows.** The
  lifecycle report consumes registry fields; it does not create a second model
  registry or infer model identity from arbitrary paths. The ee-managed bundled
  row for `potion-multilingual-128M` is the expected default row when the
  neural-local path is healthy.
- **Frankensearch still owns model choice and vector search.** This ADR does
  not add an `ee` embedding trait, custom vector store, custom BM25/RRF, or
  model selection config. It only records whether the selected/observed
  Frankensearch model and the derived index are compatible.
- **Indexes remain derived assets.** A stale or corrupt index is repaired by
  rebuild/reembed commands, not by mutating model registry rows.
- **Lexical fallback is a valid readiness state.** It is not success for
  semantic retrieval, but it is a usable search posture and must be reported
  explicitly.
- **Unknown never means available.** Missing evidence must produce `unknown`,
  `unsupported_feature`, or `lexical_fallback`, with a degraded entry.

## Consequences

- `bd-1iupc.2` can add a read-only collector without inventing state names or
  data shape.
- `bd-1iupc.3` can decide when an index rebuild is required by comparing model
  provenance and dimension compatibility fields.
- `ee doctor`, `ee status`, and search readiness can share one compact
  vocabulary instead of separately describing the same failure modes.
- Consumers get redaction-safe evidence and repair hints without depending on
  Frankensearch internals.

## Rejected Alternatives

- **Expose embedding model names in `ee` config:** rejected by ADR 0016; model
  selection belongs to Frankensearch.
- **Add an `ee` vector-store readiness layer:** duplicates Frankensearch and
  violates the derived-index boundary.
- **Use only existing degraded codes:** too coarse for downstream rebuild
  logic; `missing`, `corrupt`, `dimension_mismatch`, and
  `stale_index_model` need distinct machine states.
- **Treat lexical fallback as `available`:** hides semantic unavailability and
  prevents honest recall-readiness reporting.

## Verification

- `docs/schemas/ee.model_lifecycle.v1.json` pins the v1 report shape, closed
  lifecycle state enum, provenance fields, dimension compatibility rules, and
  degraded-code vocabulary.
- `tests/model_lifecycle_schema_unit.rs` validates the schema identity,
  required field sets, state enum, redaction rules, compatibility fields, and a
  representative lexical-fallback sample.
- Follow-on beads implement collectors and live command surfaces against this
  contract; until then `x-ee-status.shipped = false`.
