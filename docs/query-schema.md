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
| `query.mode` | enum | `"hybrid"` | `"hybrid"`, `"lexical"`, `"semantic"`, `"exact"` |

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
| `time.after` | ISO 8601 | Memories created after this timestamp |
| `time.before` | ISO 8601 | Memories created before this timestamp |

### As-Of Query

```json
{
  "asOf": "2026-04-15T12:00:00Z"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `asOf` | ISO 8601 | Point-in-time snapshot (excludes later updates) |

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
| `temporalValidity.posture` | enum | `"relaxed"` | `"strict"` (exclude expired), `"relaxed"` (include with warning), `"ignore"` |
| `temporalValidity.referenceTime` | ISO 8601 | now | Time to evaluate validity against |

---

## Trust and Redaction Filters

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
| `graph.maxHops` | int | 1 | Maximum link traversal depth |
| `graph.linkTypes` | string[] | all | Only traverse these link types |
| `graph.includeOrphans` | bool | true | Include memories with no links |

**Behavior**: Graph hints are **ranking/expansion hints**, not strict filters. They influence which memories are considered and how they score, but do not silently exclude memories unless `includeOrphans: false` is explicitly set.

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
| `output.profile` | enum | `"balanced"` | `"compact"`, `"balanced"`, `"wide"`, `"custom"` |
| `output.format` | enum | `"json"` | Hint for renderer selection |
| `output.fields` | enum | `"standard"` | `"minimal"`, `"summary"`, `"standard"`, `"full"` |
| `output.explain` | bool | false | Include scoring/selection explanations |

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
| `budget.maxResults` | int | 100 | Maximum memories to return |
| `budget.candidatePool` | int | 200 | Candidate pool size before filtering |

**Validation**: Zero values return `ERR_ZERO_BUDGET`.

### Pagination

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

### Simple Text Query

```json
{
  "version": "ee.query.v1",
  "query": {"text": "release checklist"}
}
```

### Tag Filtering

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

### Metadata Boolean Filters

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

### Time/As-Of Query

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

### Graph Neighborhood Hints

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

### Trust Filters with Explain

```json
{
  "version": "ee.query.v1",
  "query": {"text": "security policy"},
  "trust": {
    "minClass": "human_explicit"
  },
  "output": {
    "explain": true,
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

### Full Composition

```json
{
  "version": "ee.query.v1",
  "workspace": "/data/projects/myproject",
  "query": {
    "text": "prepare release",
    "mode": "hybrid"
  },
  "tags": {
    "require": ["release"],
    "exclude": ["draft"]
  },
  "filters": {
    "level": {"in": ["procedural", "episodic"]},
    "confidence": {"gte": 0.7}
  },
  "time": {
    "after": "2026-04-01T00:00:00Z"
  },
  "temporalValidity": {
    "posture": "strict"
  },
  "trust": {
    "minClass": "human_explicit"
  },
  "graph": {
    "traversal": "bidirectional",
    "maxHops": 1
  },
  "budget": {
    "maxTokens": 4000,
    "candidatePool": 200
  },
  "output": {
    "profile": "balanced",
    "explain": true
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

| Feature | Status | Notes |
|---------|--------|-------|
| `query.text` | Implemented | Core text search |
| `tags.*` | Implemented | Tag filtering |
| `filters.*` | Partial | Basic operators only |
| `time.*` | Implemented | Temporal filtering |
| `asOf` | Planned | Point-in-time queries |
| `temporalValidity` | Planned | EE-TEMPORAL-VALIDITY-001 |
| `trust.*` | Partial | Basic trust filtering |
| `graph.*` | Planned | Graph traversal hints |
| `budget.*` | Implemented | Token budgets |
| `output.*` | Implemented | Output control |
| `pagination.*` | Partial | Basic pagination |
| `eval.*` | Implemented | Evaluation labels |

---

## Follow-Up TODOs

- [ ] **EE-QUERY-FILE-001**: Implement `--query-file` CLI plumbing
- [ ] **EE-TEMPORAL-VALIDITY-001**: Add `valid_from`/`valid_to` support
- [ ] Add JSON Schema export to `ee schema export ee.query.v1`
- [ ] Add golden fixtures for query validation
- [ ] Add property tests for query parsing
