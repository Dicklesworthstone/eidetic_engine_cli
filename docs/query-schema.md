# Query Schema: `ee.query.v1`

> Versioned request document for context packing and memory retrieval.

---

## Overview

The `ee.query.v1` schema defines how agents and tools submit structured queries to `ee context`, `ee search`, and related commands. JSON is the canonical format; TOON and Markdown are renderers over the same data.

**Implementation substrate**: All query execution uses existing modules:
- **Database filters**: SQLModel + FrankenSQLite for metadata predicates
- **Search**: Frankensearch for text/hybrid retrieval
- **Graph projections**: FrankenNetworkX for neighborhood and link traversal

This schema does **not** introduce custom RRF, BM25, or vector fusion algorithms. Execution must map to the existing module interfaces.

---

## Schema Version

```json
{
  "version": "ee.query.v1"
}
```

The `version` field is required. Unknown versions return `ERR_UNKNOWN_VERSION`.

---

## Core Fields

### Query Text

```json
{
  "version": "ee.query.v1",
  "query": {
    "text": "prepare release checklist",
    "mode": "hybrid"
  }
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `query.text` | string | (required) | Natural language query for retrieval |
| `query.mode` | enum | `"hybrid"` | `"hybrid"`; other modes are recognized but not wired through pack execution |

**Validation**: Empty `query.text` returns `ERR_EMPTY_QUERY`.

### Workspace

```json
{
  "workspace": "/path/to/project"
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `workspace` | string | cwd | Workspace path to query against |

---

## Tag Filtering

Tag filters select memories based on their tags. All tag constraints must be satisfied
for a memory to be included in results.

### Positive Tags (require)

```json
{
  "tags": {
    "require": ["release", "checklist"],
    "requireAny": ["manual", "automated"]
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `tags.require` | string[] | All tags must be present (AND) |
| `tags.requireAny` | string[] | At least one tag must be present (OR) |

### Negative Tags (exclude)

```json
{
  "tags": {
    "exclude": ["deprecated", "draft"]
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `tags.exclude` | string[] | None of these tags may be present |

**Behavior**: Tag filters are **selection filters**, not ranking hints. Memories not matching the filter are excluded entirely from results.

---

## Metadata Filters

Boolean predicates on memory metadata fields.

```json
{
  "filters": {
    "level": {"eq": "procedural"},
    "kind": {"in": ["rule", "warning"]},
    "confidence": {"gte": 0.7},
    "source": {"exists": true}
  }
}
```

### Operators

| Operator | Description | Example |
|----------|-------------|---------|
| `eq` | Equals | `{"level": {"eq": "procedural"}}` |
| `neq` | Not equals | `{"status": {"neq": "tombstoned"}}` |
| `in` | In list | `{"kind": {"in": ["rule", "warning"]}}` |
| `notIn` | Not in list | `{"kind": {"notIn": ["draft"]}}` |
| `gt`, `gte`, `lt`, `lte` | Numeric comparison | `{"confidence": {"gte": 0.7}}` |
| `exists` | Field presence | `{"source": {"exists": true}}` |
| `startsWith`, `endsWith`, `contains` | String matching | `{"id": {"startsWith": "mem_"}}` |

**Validation**: Invalid operator returns `ERR_INVALID_OPERATOR`.

---

## Temporal Filters

> **Status: Implemented** — Temporal fields are applied during pack candidate
> resolution after matching memory rows have been loaded from the database.

### Time Window

```json
{
  "time": {
    "after": "2026-04-01T00:00:00Z",
    "before": "2026-04-30T23:59:59Z"
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `time.after` | ISO 8601 | Memories created at or after this timestamp |
| `time.before` | ISO 8601 | Memories created at or before this timestamp |

### As-Of Query

```json
{
  "asOf": "2026-04-15T12:00:00Z"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `asOf` | ISO 8601 | Point-in-time snapshot; excludes memories created or updated after this timestamp |

**Validation**: Invalid timestamp returns `ERR_INVALID_TIMESTAMP`.

### Temporal Validity

```json
{
  "temporalValidity": {
    "posture": "strict",
    "referenceTime": "2026-04-30T12:00:00Z"
  }
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `temporalValidity.posture` | enum | `"relaxed"` | `"strict"` excludes expired or future-valid memories, `"relaxed"` includes them with a degradation, `"ignore"` bypasses validity filtering |
| `temporalValidity.referenceTime` | ISO 8601 | `asOf`, otherwise now | Time to evaluate `valid_from`/`valid_to` against |

---

## Trust and Redaction Filters

> **Status: Partially Implemented** — Trust filters are applied during pack
> candidate resolution. `redaction.policy: "respect"` is accepted; `"bypass"`
> is denied with a policy error. Redaction category allow-lists are validated
> but not yet used as selection filters.

```json
{
  "trust": {
    "minClass": "human_explicit",
    "excludeClasses": ["agent_inferred"],
    "requirePosture": "authoritative"
  },
  "redaction": {
    "policy": "respect",
    "allowCategories": ["internal"]
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `trust.minClass` | enum | Minimum trust class to include |
| `trust.excludeClasses` | string[] | Trust classes to exclude |
| `trust.requirePosture` | enum | Required trust posture |
| `redaction.policy` | enum | `"respect"` (apply redaction), `"bypass"` (requires elevated permission) |
| `redaction.allowCategories` | string[] | Redaction categories that are acceptable |

**Behavior**: Trust filters are **selection filters**. Memories not meeting trust criteria are excluded.

---

## Graph and Neighborhood Hints

> **Status: Implemented** — Graph fields are accepted as bounded context-pack
> ranking and expansion hints. Missing or stale graph snapshots degrade
> honestly while retrieval continues from the source-of-truth `memory_links`
> table.

```json
{
  "graph": {
    "seedMemories": ["mem_abc123"],
    "traversal": "outbound",
    "maxHops": 2,
    "linkTypes": ["supports", "contradicts"],
    "includeOrphans": false
  }
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `graph.seedMemories` | string[] | - | Start traversal from these memory IDs |
| `graph.traversal` | enum | `"bidirectional"` | `"outbound"`, `"inbound"`, `"bidirectional"` |
| `graph.maxHops` | int | 1 | Maximum link traversal depth; bounded to `0..=8` |
| `graph.linkTypes` | string[] | all | Only traverse these relation types: `supports`, `contradicts`, `derived_from`, `supersedes`, `related`, `co_tag`, `co_mention` |
| `graph.includeOrphans` | bool | true | Include memories with no links |

**Behavior**: Graph hints are **ranking/expansion hints**, not strict filters.
Seed memories and bounded neighbors are boosted or added to the candidate pool.
Traversal uses deterministic ordering and relation filtering. Lexical retrieval
is preserved when graph snapshots are missing or stale; the response includes
degraded codes such as `context_graph_snapshot_missing` or
`context_graph_snapshot_not_current`. Memories outside the graph neighborhood
are excluded only when `includeOrphans: false` is explicitly set. Seeds and
linked neighbors outside the active workspace scope are ignored and reported via
`context_graph_seed_out_of_scope` or `context_graph_workspace_filtered`.

---

## Output Control

### Profile and Format

```json
{
  "output": {
    "profile": "balanced",
    "format": "json",
    "fields": "standard",
    "explain": true
  }
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `output.profile` | enum | `"balanced"` | `"compact"`, `"balanced"`, `"thorough"`, `"submodular"` (`"custom"` not implemented) |
| `output.format` | enum | `"json"` | Hint for renderer selection |
| `output.fields` | enum | `"standard"` | `"minimal"`, `"summary"`, `"standard"`, `"full"` (validated; projection controlled by `--fields` CLI flag) |
| `output.explain` | bool | false | Include scoring/selection explanations (JSON packs include `selectionCertificate` and per-item `why` by default; setting `true` emits an informational degradation confirming this) |

### Budget and Limits

```json
{
  "budget": {
    "maxTokens": 4000,
    "maxResults": 50,
    "candidatePool": 200
  }
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `budget.maxTokens` | int | 8000 | Token budget for context pack |
| `budget.maxResults` | int | 100 | Maximum candidates admitted to pack assembly after retrieval/focus expansion |
| `budget.candidatePool` | int | 200 | Candidate pool size before filtering |

**Validation**: Zero values return `ERR_ZERO_BUDGET`.
When `budget.maxResults` trims candidates, JSON output includes the
`context_query_max_results_applied` degradation so the limit is observable.

### Pagination

> **Status: Not Implemented** — Pagination fields are recognized but return `ERR_UNSUPPORTED_FEATURE`.
> Use CLI flags: `ee search --limit 25 --offset 50`.

```json
{
  "pagination": {
    "cursor": "eyJvZmZzZXQiOjUwfQ==",
    "limit": 25
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `pagination.cursor` | string | Opaque cursor from previous response |
| `pagination.limit` | int | Page size |

**Deterministic ordering**: Results are ordered by `(relevance DESC, memory_id ASC)` to ensure stable pagination.

---

## Evaluation Labels

```json
{
  "eval": {
    "scenarioId": "release-checklist-001",
    "labels": ["release", "checklist"],
    "expectedMemoryIds": ["mem_abc", "mem_def"]
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `eval.scenarioId` | string | Identifier for evaluation scenario |
| `eval.labels` | string[] | Labels for categorizing this query |
| `eval.expectedMemoryIds` | string[] | Expected memories for recall/precision measurement |

**Behavior**: Eval fields do not affect query execution. They are captured in output for offline evaluation.

---

## Extension Policy

### Unknown Fields

```json
{
  "version": "ee.query.v1",
  "query": {"text": "test"},
  "unknownField": "value"
}
```

**Policy**: Unknown fields are **ignored with a warning** in the response's `degraded` array. This allows forward compatibility while signaling that the field had no effect.

### Future Features

Fields that are recognized but not yet implemented return `ERR_UNSUPPORTED_FEATURE` with a message indicating the feature name and expected availability.

---

## Error Codes

| Code | Description |
|------|-------------|
| `ERR_MALFORMED_JSON` | JSON parsing failed |
| `ERR_UNKNOWN_VERSION` | Unrecognized schema version |
| `ERR_UNKNOWN_FIELD` | Unknown field (warning, not fatal) |
| `ERR_EMPTY_QUERY` | Query text is empty or whitespace-only |
| `ERR_INVALID_TIMESTAMP` | Timestamp format invalid (must be ISO 8601) |
| `ERR_INVALID_OPERATOR` | Unknown filter operator |
| `ERR_UNSAFE_PATH` | Path traversal or injection attempt |
| `ERR_INCOMPATIBLE_FIELDS` | Conflicting field combination |
| `ERR_UNSUPPORTED_FEATURE` | Recognized but unimplemented feature |
| `ERR_ZERO_BUDGET` | Token budget or candidate pool is zero |

---

## Examples

> **Note**: Examples marked with ⚠️ use fields that are not yet implemented and will return
> `ERR_UNSUPPORTED_FEATURE`. Use the equivalent CLI flags shown in the Implementation Status table.

### Simple Text Query (working)

```json
{
  "version": "ee.query.v1",
  "query": {"text": "release checklist"}
}
```

### Tag Filtering (working)

```json
{
  "version": "ee.query.v1",
  "query": {"text": "deployment"},
  "tags": {
    "require": ["production"],
    "exclude": ["deprecated"]
  }
}
```

Equivalent CLI flags remain available for ad hoc commands:
`ee context "deployment" --tags production --exclude-tags deprecated`

### Metadata Boolean Filters (working)

```json
{
  "version": "ee.query.v1",
  "query": {"text": "error handling"},
  "filters": {
    "level": {"eq": "procedural"},
    "confidence": {"gte": 0.8}
  }
}
```

### Time/As-Of Query (working)

```json
{
  "version": "ee.query.v1",
  "query": {"text": "API changes"},
  "time": {
    "after": "2026-04-01T00:00:00Z"
  },
  "asOf": "2026-04-15T12:00:00Z"
}
```

### Graph Neighborhood Hints (working)

```json
{
  "version": "ee.query.v1",
  "query": {"text": "authentication"},
  "graph": {
    "seedMemories": ["mem_auth_001"],
    "traversal": "outbound",
    "maxHops": 2
  }
}
```

### Trust Filters (working)

```json
{
  "version": "ee.query.v1",
  "query": {"text": "security policy"},
  "trust": {
    "minClass": "human_explicit"
  },
  "output": {
    "fields": "full"
  }
}
```

### Evaluation Scenario

```json
{
  "version": "ee.query.v1",
  "query": {"text": "test coverage"},
  "eval": {
    "scenarioId": "test-coverage-001",
    "labels": ["testing", "coverage"],
    "expectedMemoryIds": ["mem_test_001", "mem_test_002"]
  }
}
```

### ⚠️ Full Composition (many fields not implemented)

The following shows the full schema, but most fields will error. A working subset is shown below.

```json
{
  "version": "ee.query.v1",
  "workspace": "/data/projects/myproject",
  "query": {
    "text": "prepare release",
    "mode": "hybrid"
  },
  "tags": {                           // Implemented
    "require": ["release"],
    "exclude": ["draft"]
  },
  "filters": {                        // ✓ Implemented
    "level": {"in": ["procedural", "episodic"]},
    "confidence": {"gte": 0.7}
  },
  "time": {                           // Implemented
    "after": "2026-04-01T00:00:00Z"
  },
  "temporalValidity": {               // Implemented
    "posture": "strict"
  },
  "trust": {                          // Implemented
    "minClass": "human_explicit"
  },
  "graph": {                          // Implemented
    "traversal": "bidirectional",
    "maxHops": 1
  },
  "budget": {                         // ✓ Implemented
    "maxTokens": 4000,
    "candidatePool": 200
  },
  "output": {                         // ✓ Partially implemented
    "profile": "balanced",
    "explain": true                   // ⚠️ Not implemented
  }
}
```

### Working Subset

```json
{
  "version": "ee.query.v1",
  "workspace": "/data/projects/myproject",
  "query": {
    "text": "prepare release",
    "mode": "hybrid"
  },
  "filters": {
    "level": {"in": ["procedural", "episodic"]},
    "confidence": {"gte": 0.7}
  },
  "budget": {
    "maxTokens": 4000,
    "candidatePool": 200
  },
  "output": {
    "profile": "balanced"
  }
}
```

---

## Non-Goals

This schema **does not**:

1. Implement custom retrieval algorithms (use Frankensearch)
2. Define arbitrary computation (queries are declarative filters)
3. Support recursive or unbounded graph traversal
4. Allow arbitrary SQL or code execution
5. Bypass redaction without explicit elevated permissions

---

## Implementation Status

> **Note**: Fields marked "Not Implemented" are recognized by the parser but rejected
> at runtime with `ERR_UNSUPPORTED_FEATURE`. Use CLI flags for equivalent functionality
> where available.

| Feature | Status | Notes |
|---------|--------|-------|
| `query.text` | **Implemented** | Core text search |
| `query.mode` | Partial | hybrid accepted; lexical, semantic, and exact return `ERR_UNSUPPORTED_FEATURE` |
| `workspace` | **Implemented** | Workspace path resolution |
| `tags.*` | **Implemented** | require, requireAny, exclude |
| `filters.*` | **Implemented** | eq, neq, in, notIn, gte, lte, exists operators |
| `time.*` | **Implemented** | Inclusive created-at window filtering |
| `asOf` | **Implemented** | Excludes memories created or updated after the snapshot timestamp |
| `temporalValidity` | **Implemented** | strict, relaxed, and ignore postures over `valid_from`/`valid_to` |
| `trust.*` | **Implemented** | minClass, excludeClasses, requirePosture candidate filtering |
| `redaction.*` | Partial | `respect` accepted, `bypass` policy denied; category allow-list filtering pending |
| `graph.*` | **Implemented** | seedMemories, traversal, bounded maxHops, linkTypes, includeOrphans |
| `budget.maxTokens` | **Implemented** | Token budget for context pack |
| `budget.candidatePool` | **Implemented** | Candidate pool size |
| `budget.maxResults` | **Implemented** | Caps candidates admitted to pack assembly |
| `output.profile` | **Implemented** | compact, balanced, thorough, submodular |
| `output.format` | **Implemented** | json, markdown, toon, human, jsonl, compact, hook |
| `output.fields` | Validated | Projection controlled by `--fields` CLI flag |
| `output.explain` | **Implemented** | Accepted; JSON packs already include selection certificates and per-item `why` |
| `pagination.*` | **Implemented** | Cursor-based pagination with deterministic ordering |
| `eval.*` | **Implemented** | Evaluation labels captured in output |

---

## Follow-Up TODOs

### Wiring Query Fields to Execution

These fields are recognized and validated but not wired through to pack/search execution:

- [x] **tags.require, tags.requireAny, tags.exclude** — Memory tag filtering
- [x] **time.after, time.before** — Wire to temporal window filtering
- [x] **asOf** — Point-in-time snapshot queries
- [x] **temporalValidity** — Valid-from/valid-to support (EE-TEMPORAL-VALIDITY-001)
- [x] **trust.minClass, trust.excludeClasses** — Wire to trust class filtering
- [x] **redaction.policy** — Accept respect and policy-deny bypass
- [ ] **redaction.allowCategories** — Wire category allow-list filtering
- [x] **graph.seedMemories, graph.traversal, graph.maxHops** — Bounded graph traversal and ranking hints
- [x] **pagination.cursor, pagination.limit** — Cursor-based pagination
- [x] **budget.maxResults** — Candidate admission limit
- [x] **output.explain** — Explanation metadata is observable in JSON responses

### Infrastructure

- [x] `--query-file` CLI plumbing (implemented)
- [ ] Add JSON Schema export to `ee schema export ee.query.v1`
- [ ] Add golden fixtures for query validation
- [ ] Add property tests for query parsing
