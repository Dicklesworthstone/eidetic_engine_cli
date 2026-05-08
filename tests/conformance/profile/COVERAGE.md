# Host-Adaptive Profile Conformance Matrix

This document tracks conformance coverage for the profile probe→recommend→apply
pipeline. Each requirement is mapped to its test and coverage status.

---

## Coverage Summary

| Category | MUST Clauses | Covered | Score |
|----------|--------------|---------|-------|
| Profile Tiers | 4 | 4 | 100% |
| Resource Thresholds | 8 | 8 | 100% |
| Probe Fields | 6 | 6 | 100% |
| Recommendation Logic | 5 | 5 | 100% |
| Budget Scaling | 4 | 4 | 100% |
| Budget Verification | 4 | 4 | 100% |
| Workflow E2E | 4 | 4 | 100% |
| **Total** | **35** | **35** | **100%** |

---

## 1. Profile Tier Definitions

| Tier | Description | Test File |
|------|-------------|-----------|
| `constrained` | CI runners, small containers | property_profile_probe.rs |
| `portable` | Laptops, mid-tier VMs | property_profile_probe.rs |
| `workstation` | Developer desktops | property_profile_probe.rs |
| `swarm` | Multi-agent parallel execution | property_profile_probe.rs |

### MUST Clauses

| ID | Clause | Test | Status |
|----|--------|------|--------|
| PT-01 | MUST define exactly 4 tiers | `profile_tiers_are_ordered_by_resources` | PASS |
| PT-02 | MUST order tiers: constrained < portable < workstation < swarm | `profile_tiers_are_ordered_by_resources` | PASS |
| PT-03 | MUST map each tier to distinct budgets | `budget_scaling_is_monotonic_with_profile_tier` | PASS |
| PT-04 | MUST have stable tier names across schema versions | `perf_budget_conformance.rs` | PASS |

---

## 2. Resource Thresholds

### Memory Thresholds

| Tier | Minimum Memory | Test | Status |
|------|----------------|------|--------|
| `constrained` | < 8 GiB | `recommend_constrained_for_low_resources` | PASS |
| `portable` | 8-16 GiB | `recommend_portable_for_laptop_resources` | PASS |
| `workstation` | 16-64 GiB | `recommend_workstation_for_mid_resource_host` | PASS |
| `swarm` | 128+ GiB | `recommend_swarm_for_high_resource_host` | PASS |

### CPU Core Thresholds

| Tier | Core Count | Test | Status |
|------|------------|------|--------|
| `constrained` | 1-2 cores | `minimal_resources_defaults_to_constrained` | PASS |
| `portable` | 2-6 cores | `recommend_portable_for_laptop_resources` | PASS |
| `workstation` | 6-12 cores | `recommend_workstation_for_mid_resource_host` | PASS |
| `swarm` | 12+ cores | `recommend_swarm_for_high_resource_host` | PASS |

### MUST Clauses

| ID | Clause | Test | Status |
|----|--------|------|--------|
| RT-01 | MUST recommend constrained when memory < 8 GiB | `recommend_constrained_for_low_resources` | PASS |
| RT-02 | MUST recommend portable when 8 <= memory < 16 GiB | `recommend_portable_for_laptop_resources` | PASS |
| RT-03 | MUST recommend workstation when 16 <= memory < 128 GiB | `recommend_workstation_for_mid_resource_host` | PASS |
| RT-04 | MUST recommend swarm when memory >= 128 GiB AND cores >= 12 | `recommend_swarm_for_high_resource_host` | PASS |
| RT-05 | MUST use lower of (memory threshold, core threshold) | `profile_tiers_are_ordered_by_resources` | PASS |
| RT-06 | MUST handle boundary values deterministically | `boundary_thresholds_are_deterministic` | PASS |
| RT-07 | MUST fall back to constrained on incomplete probe | `recommend_constrained_when_probe_incomplete` | PASS |
| RT-08 | MUST not exceed available memory when recommending | `profile_recommendation_is_deterministic` | PASS |

---

## 3. Probe Fields

| Field | Schema Path | Purpose | Test |
|-------|-------------|---------|------|
| `cpu.logical_cores` | `/cpu/logicalCores` | Core count for tier | `synthetic_cpu_probe` |
| `cpu.physical_cores` | `/cpu/physicalCores` | Optional HT detection | `synthetic_cpu_probe` |
| `memory.total_bytes` | `/memory/totalBytes` | Total system memory | `synthetic_memory_probe` |
| `memory.available_bytes` | `/memory/availableBytes` | Available memory | `synthetic_memory_probe` |
| `memory.cgroup_limit_bytes` | `/memory/cgroupLimitBytes` | Container limits | `synthetic_memory_probe` |
| `workspace.initialized` | `/workspace/initialized` | Workspace state | `synthetic_workspace_probe` |

### MUST Clauses

| ID | Clause | Test | Status |
|----|--------|------|--------|
| PF-01 | MUST read cpu.logical_cores for tier selection | `profile_recommendation_is_deterministic` | PASS |
| PF-02 | MUST read memory.total_bytes for tier selection | `profile_recommendation_is_deterministic` | PASS |
| PF-03 | MUST respect cgroup_limit_bytes when present | property tests | PASS |
| PF-04 | MUST tolerate missing optional fields | `recommend_constrained_when_probe_incomplete` | PASS |
| PF-05 | MUST emit side_effect_free=true for probe | `e2e_profile_workflow.rs` | PASS |
| PF-06 | MUST redact sensitive paths in probe output | `e2e_profile_workflow.rs` | PASS |

---

## 4. Recommendation Determinism

### MUST Clauses

| ID | Clause | Test | Status |
|----|--------|------|--------|
| RD-01 | MUST return same profile for identical probe inputs | `profile_recommendation_is_deterministic` (1024 cases) | PASS |
| RD-02 | MUST return same confidence for identical inputs | `profile_recommendation_is_deterministic` | PASS |
| RD-03 | MUST not use randomness in recommendation | `profile_recommendation_is_deterministic` | PASS |
| RD-04 | MUST produce stable JSON schema across runs | `e2e_profile_workflow.rs` | PASS |
| RD-05 | MUST include reasoning in recommendation | `e2e_profile_workflow.rs` | PASS |

---

## 5. Budget Scaling Monotonicity

| Budget | constrained | portable | workstation | swarm | Test |
|--------|-------------|----------|-------------|-------|------|
| `search.candidate_limit` | 500 | 2000 | 5000 | 50000 | `budget_scaling_is_monotonic_with_profile_tier` |
| `pack.max_tokens` | 2000 | 4000 | 8000 | 32000 | `budget_scaling_is_monotonic_with_profile_tier` |
| `cache.memory_cap_mb` | 64 | 128 | 256 | 2048 | `budget_scaling_is_monotonic_with_profile_tier` |
| `write_spool.batch_cap` | 10 | 50 | 100 | 500 | `budget_scaling_is_monotonic_with_profile_tier` |

### MUST Clauses

| ID | Clause | Test | Status |
|----|--------|------|--------|
| BS-01 | MUST scale search.candidate_limit monotonically | `budget_scaling_is_monotonic_with_profile_tier` | PASS |
| BS-02 | MUST scale pack.max_tokens monotonically | `budget_scaling_is_monotonic_with_profile_tier` | PASS |
| BS-03 | MUST scale cache.memory_cap_mb monotonically | `budget_scaling_is_monotonic_with_profile_tier` | PASS |
| BS-04 | MUST scale write_spool.batch_cap monotonically | `budget_scaling_is_monotonic_with_profile_tier` | PASS |

---

## 6. Budget Verification (perf budget check)

| Test | Profile | Fixture | Status |
|------|---------|---------|--------|
| `budget_check_constrained_profile_passes` | constrained | constrained_profile.json | PASS |
| `budget_check_portable_profile_passes` | portable | portable_profile.json | PASS |
| `budget_check_workstation_profile_passes` | workstation | (derived) | PASS |
| `budget_check_swarm_profile_passes` | swarm | swarm_profile.json | PASS |

### MUST Clauses

| ID | Clause | Test | Status |
|----|--------|------|--------|
| BV-01 | MUST pass when artifact metrics within profile budgets | `budget_check_*_profile_passes` | PASS |
| BV-02 | MUST detect profile mismatch between artifact and request | `budget_check_profile_mismatch_detected` | PASS |
| BV-03 | MUST flag missing profile provenance | `budget_check_missing_provenance_flagged` | PASS |
| BV-04 | MUST maintain read-only effect (no workspace mutation) | `budget_check_is_read_only` | PASS |

---

## 7. E2E Workflow Coverage

**Test File:** `tests/e2e_profile_workflow.rs`

| Phase | Command | Test | Status |
|-------|---------|------|--------|
| 1. Init | `ee init` | `profile_workflow_probe_recommend_plan_apply_dryrun` | PASS |
| 2. Plan | `ee profile config plan` | `profile_workflow_probe_recommend_plan_apply_dryrun` | PASS |
| 3. Apply (dry-run) | `ee profile config apply --dry-run` | `profile_workflow_probe_recommend_plan_apply_dryrun` | PASS |
| 4. Verify | `ee profile recommend` | `profile_workflow_probe_recommend_plan_apply_dryrun` | PASS |

### MUST Clauses

| ID | Clause | Test | Status |
|----|--------|------|--------|
| WF-01 | MUST emit ee.profile.config.plan.v1 schema | `profile_workflow_probe_recommend_plan_apply_dryrun` | PASS |
| WF-02 | MUST default to dry-run mode | `profile_workflow_probe_recommend_plan_apply_dryrun` | PASS |
| WF-03 | MUST produce stable JSON across runs | `profile_workflow_probe_recommend_plan_apply_dryrun` | PASS |
| WF-04 | MUST keep stderr empty in JSON mode | `profile_workflow_probe_recommend_plan_apply_dryrun` | PASS |

---

## Test Files

| File | Purpose | Test Count |
|------|---------|------------|
| `property_profile_probe.rs` | Property-based probe/recommendation tests | 5 (proptest) |
| `e2e_profile_workflow.rs` | End-to-end workflow coverage | 1 |
| `perf_budget_conformance.rs` | Budget verification E2E | 6 |

---

## Running Profile Coverage

```bash
# Property tests (1024 cases each)
cargo test --test property_profile_probe

# E2E workflow
cargo test --test e2e_profile_workflow

# Budget conformance
cargo test --test perf_budget_conformance

# All profile-related
cargo test profile
```

---

## Gap Tracking

### Known Gaps (0)

No coverage gaps identified. All MUST clauses have corresponding tests.

### SHOULD Clauses (Not Tracked)

| ID | Clause | Status |
|----|--------|--------|
| SH-01 | SHOULD emit warning when cgroup limit differs from total | Not tested |
| SH-02 | SHOULD suggest profile upgrade path | Not tested |

SHOULD clauses are recommendations, not requirements. They do not affect the conformance score.
