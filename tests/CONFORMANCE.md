# Conformance Test Documentation

This directory contains conformance test suites that verify `ee` behavior against
documented specifications. Each suite validates that implementation matches the
schema contracts defined in `docs/`.

---

## Query File Conformance: `ee.query.v1`

**Test File:** `conformance/query_v1_matrix.rs`  
**Schema Doc:** `docs/query-schema.md`

### Coverage Matrix

| Category | Count | Description |
|----------|-------|-------------|
| Implemented Features | 16 | Working features that should succeed |
| Unimplemented Features | 2 | Features returning `ERR_UNSUPPORTED_FEATURE` |
| Error Cases | 10 | Invalid inputs returning appropriate error codes |
| Combination Tests | 3 | Multiple features used together |
| Determinism Tests | 1 | Same input produces identical output |
| Edge Cases | 5 | Boundary conditions and Unicode handling |

### Implemented Features (Should Succeed)

| Test | Feature | Schema Reference |
|------|---------|------------------|
| `matrix_simple_text_query` | Basic query.text | docs/query-schema.md:34-50 |
| `matrix_tags_require_only` | tags.require (AND) | docs/query-schema.md:72-86 |
| `matrix_tags_require_any` | tags.requireAny (OR) | docs/query-schema.md:79-86 |
| `matrix_tags_exclude` | tags.exclude | docs/query-schema.md:90-100 |
| `matrix_tags_combined_filters` | All tag filters together | docs/query-schema.md:69-102 |
| `matrix_output_profile_balanced` | output.profile | docs/query-schema.md:265-267 |
| `matrix_output_explain_true` | output.explain | docs/query-schema.md:270 |
| `matrix_budget_max_tokens` | budget.maxTokens | docs/query-schema.md:286 |
| `matrix_budget_max_results` | budget.maxResults | docs/query-schema.md:287 |
| `matrix_query_mode_hybrid` | query.mode | docs/query-schema.md:49 |
| `matrix_time_after_filters_created_at` | time.after | docs/query-schema.md:139-156 |
| `matrix_time_before_accepts_open_ended_future_window` | time.before | docs/query-schema.md:139-156 |
| `matrix_as_of_future_snapshot_succeeds` | asOf | docs/query-schema.md:159-168 |
| `matrix_temporal_validity_strict_succeeds` | temporalValidity | docs/query-schema.md:171-186 |
| `matrix_trust_min_class_succeeds` | trust.minClass | docs/query-schema.md:192-217 |
| `matrix_redaction_respect_succeeds` | redaction.policy=respect | docs/query-schema.md:192-217 |

### Unimplemented Features (ERR_UNSUPPORTED_FEATURE)

| Test | Feature | Schema Reference | Blocking Bead |
|------|---------|------------------|---------------|
| `matrix_unsupported_graph` | graph | docs/query-schema.md:221-246 | eidetic_engine_cli-bzwu |
| `matrix_unsupported_pagination` | pagination | docs/query-schema.md:294-313 | eidetic_engine_cli-4x80 |

### Error Cases

| Test | Error Code | Trigger |
|------|------------|---------|
| `matrix_error_malformed_json` | ERR_MALFORMED_JSON | Invalid JSON syntax |
| `matrix_error_unknown_version` | ERR_UNKNOWN_VERSION | Unrecognized schema version |
| `matrix_error_empty_query_text` | ERR_EMPTY_QUERY | Empty string query |
| `matrix_error_whitespace_query_text` | ERR_EMPTY_QUERY | Whitespace-only query |
| `matrix_error_invalid_timestamp_format` | ERR_INVALID_TIMESTAMP | Non-ISO8601 timestamp |
| `matrix_error_zero_budget_max_tokens` | ERR_ZERO_BUDGET | maxTokens = 0 |
| `matrix_error_query_file_not_found` | ERR_QUERY_FILE_NOT_FOUND | Nonexistent file path |
| `matrix_error_tags_wrong_type_array` | ERR_MALFORMED_JSON | tags as array (not object) |
| `matrix_error_tags_wrong_type_string` | ERR_MALFORMED_JSON | tags as string (not object) |

### Determinism Guarantee

The `matrix_deterministic_output` test verifies that:
- Same workspace + same query file = identical item IDs
- Pack ordering is stable across runs
- No non-deterministic data leaks into output

This is required by AGENTS.md: "Given the same database, indexes, config, and query,
JSON output must be stable."

### Running the Matrix

```bash
# Run full conformance matrix
cargo test --test query_v1_matrix

# Run specific section
cargo test --test query_v1_matrix matrix_tags
cargo test --test query_v1_matrix matrix_error
cargo test --test query_v1_matrix matrix_unsupported

# With output for debugging
cargo test --test query_v1_matrix -- --nocapture
```

### When Features Are Implemented

When an `ERR_UNSUPPORTED_FEATURE` test transitions to working:

1. Move the test from Section 2 to Section 1
2. Change assertion from `assert_error_envelope` to `assert_response_envelope`
3. Add value assertions for the feature's behavior
4. Update this table to reflect the new status
5. Close the corresponding blocking bead

---

## CASS Contracts Conformance

**Test File:** `conformance/cass_contracts.rs`  
**Schema Doc:** CASS robot/JSON output contracts

See `conformance/DISCREPANCIES.md` for known gaps.

---

## Adding New Conformance Suites

1. Create `conformance/<schema>_matrix.rs`
2. Add entry to this file documenting coverage
3. Link to schema documentation
4. Track blocking beads for unimplemented features
5. Include determinism test if schema promises stability
