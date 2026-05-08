# Perf-Forensics Golden Fixture Provenance

This document tracks how each perf-forensics golden fixture was generated,
the schema versions they target, and when to regenerate them.

---

## Generator

**Source:** Rust test code (synthetic, not from reference implementation)  
**Generator File:** `tests/perf_compare_golden.rs`  
**Snapshot Tool:** insta (`assert_json_snapshot!`)  
**Snapshot Location:** `tests/snapshots/perf_compare_golden__*.snap`

Fixtures use hardcoded synthetic values (not live measurements) to ensure
deterministic diffs across CI runs and platforms.

---

## Schema Versions

| Schema | Version | Used By |
|--------|---------|---------|
| `ee.artifact.summary.v1` | v1 | All ArtifactSummary fixtures |
| `ee.compare.result.v1` | v1 | CompareReport output |
| `ee.budget.check.v1` | v1 | Budget constraint checks |
| `ee.support_bundle.manifest.v1` | v1 | Support bundle fixtures |
| `ee.support_bundle.profile_evidence.v1` | v1 | Profile evidence fixtures |

---

## Fixture Catalog

### Benchmark Fixtures (perf_compare_golden.rs)

| Fixture Function | Purpose | Key Metrics |
|------------------|---------|-------------|
| `benchmark_baseline()` | Reference benchmark report | search_elapsed_ms=100, cache_hit_rate=0.85, memory_rss_mb=256 |
| `benchmark_candidate_unchanged()` | Near-identical to baseline | search_elapsed_ms=102, memory_rss_mb=258 |
| `benchmark_candidate_latency_regression()` | 180% latency regression | search_elapsed_ms=280 |
| `benchmark_candidate_memory_regression()` | 3x memory regression | memory_rss_mb=768 |
| `benchmark_candidate_cache_regression()` | Cache hit rate drop | cache_hit_rate=0.35 (from 0.85) |
| `benchmark_candidate_improved()` | All metrics improved | search_elapsed_ms=40, cache_hit_rate=0.92 |
| `benchmark_candidate_missing_metric()` | Missing cache/memory | Tests metric absence handling |

### Fixture Tier Fixtures

| Fixture Function | Purpose | Tier |
|------------------|---------|------|
| `benchmark_baseline_smoke_tier()` | Smoke-tier baseline | smoke |
| `benchmark_candidate_stress_tier()` | Stress-tier candidate | stress |

### Degradation Fixtures

| Fixture Function | Purpose | Degradation |
|------------------|---------|-------------|
| `benchmark_candidate_redaction_uncertain()` | Redacted fields | uncertain_fields: ["request.query"] |
| `benchmark_candidate_unsupported_artifact()` | Unsupported kind | legacy_profiler_dump |

### Profile Fixtures

| Fixture Function | Purpose | Profile |
|------------------|---------|---------|
| `profile_baseline()` | Workstation profile evidence | workstation |
| `profile_candidate_mismatch()` | Profile mismatch detection | swarm |

### Bundle Fixtures

| Fixture Function | Purpose | Hash State |
|------------------|---------|------------|
| `bundle_baseline()` | Valid bundle manifest | content_hash == observed_hash |
| `bundle_tampered()` | Tampered bundle detection | content_hash != observed_hash |

### Write Queue Fixtures

| Fixture Function | Purpose | Key Metrics |
|------------------|---------|-------------|
| `write_queue_baseline()` | Healthy queue | queue_depth=5, backpressure_events=0 |
| `write_queue_regression()` | Queue backpressure | queue_depth=85, backpressure_events=12 |

### Budget Check Fixtures

| Fixture Function | Purpose | Profile |
|------------------|---------|---------|
| `budget_artifact_workstation()` | Budget conformance | workstation |

---

## Static JSON Fixtures (tests/fixtures/golden/perf_artifact/)

| File | Purpose | Schema |
|------|---------|--------|
| `baseline_smoke.json` | Static smoke-tier baseline | ee.artifact.summary.v1 |
| `candidate_regressed.json` | Static regression candidate | ee.artifact.summary.v1 |
| `candidate_unchanged.json` | Static unchanged candidate | ee.artifact.summary.v1 |
| `constrained_profile.json` | Constrained profile evidence | ee.support_bundle.profile_evidence.v1 |
| `portable_profile.json` | Portable profile evidence | ee.support_bundle.profile_evidence.v1 |
| `swarm_profile.json` | Swarm profile evidence | ee.support_bundle.profile_evidence.v1 |
| `summary_benchmark.golden` | Canonical benchmark summary | ee.artifact.summary.v1 |

---

## Support Bundle Fixtures (support_bundle_perf_compare.rs)

**Generator File:** `tests/support_bundle_perf_compare.rs`  
**Snapshot Location:** `tests/snapshots/support_bundle_perf_compare__*.snap`

| Test | Purpose | Profile |
|------|---------|---------|
| `support_bundle_complete_summary` | Full bundle summary | workstation |
| `support_bundle_partial_summary` | Partial bundle handling | workstation |
| `support_bundle_profile_mismatch_compare` | Profile mismatch detection | workstation vs swarm |
| `support_bundle_tampered_hash_summary` | Tampered manifest detection | workstation |

---

## When to Regenerate

Regenerate fixtures when:

1. **Schema version bumps** - Update schema version constants, regenerate snapshots
2. **New metrics added** - Add metric to synthetic fixtures, regenerate
3. **Threshold changes** - Regression/improvement thresholds changed
4. **New artifact kinds** - Add fixture functions for new kinds
5. **Degradation code changes** - Update degradation fixtures

---

## Update Workflow

```bash
# Run tests with snapshot update
INSTA_UPDATE=always cargo test --test perf_compare_golden
INSTA_UPDATE=always cargo test --test support_bundle_perf_compare

# Review all changed snapshots
cargo insta review

# Alternatively, diff manually
git diff tests/snapshots/

# Commit only reviewed .snap files
git add tests/snapshots/perf_compare_golden__*.snap
git add tests/snapshots/support_bundle_perf_compare__*.snap
git commit -m "test(golden): update perf-forensics snapshots for schema vX"
```

---

## Relationship to Other Fixtures

| Related Suite | Location | Relationship |
|---------------|----------|--------------|
| CASS contracts | `tests/conformance/cass_contracts.rs` | Separate schema, no overlap |
| Query-file conformance | `tests/conformance/query_v1_matrix.rs` | Separate schema, no overlap |
| Agent golden baselines | `tests/agent_golden_baselines.rs` | Uses different artifact kinds |

---

## Verification

Fixtures are verified by:

1. `cargo test --test perf_compare_golden` - Snapshot comparison
2. `cargo test --test support_bundle_perf_compare` - Bundle adapter tests
3. `./scripts/verify.sh` - Full verification gate (includes golden tests)

All snapshots must pass for CI to succeed.
