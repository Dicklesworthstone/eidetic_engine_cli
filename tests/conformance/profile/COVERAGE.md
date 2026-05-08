# Host-Adaptive Profile Conformance Matrix

This matrix tracks the implemented profile probe -> recommend -> plan/apply
contract. The source of truth for numeric thresholds and budgets is
`src/core/profile.rs`; this document should not record aspirational values as
passing conformance.

## Coverage Summary

| Category | MUST Clauses | Covered | Score |
|----------|--------------|---------|-------|
| Profile tier identifiers | 4 | 4 | 100% |
| Recommendation thresholds | 9 | 9 | 100% |
| Probe shape and redaction | 10 | 10 | 100% |
| Recommendation determinism | 5 | 5 | 100% |
| Budget defaults and scaling | 9 | 9 | 100% |
| Verification recipes | 6 | 6 | 100% |
| Budget conformance command | 7 | 7 | 100% |
| Workflow and JSON contracts | 7 | 7 | 100% |
| **Total** | **57** | **57** | **100%** |

MUST score: **100%**. SHOULD gaps are tracked separately and do not affect the
MUST score.

## Implemented Thresholds

`recommend_operating_profile` uses `cpu.logical_cores` and
`memory.available_bytes.or(memory.total_bytes)`. A higher tier is selected only
when both the CPU and memory thresholds for that tier are met.

| Profile | CPU threshold | Memory threshold | Fallback boundary |
|---------|---------------|------------------|-------------------|
| `constrained` | `< 2` logical cores or missing CPU | `< 8 GiB` available memory or missing memory | Default/fallback tier |
| `portable` | `>= 2` logical cores | `>= 8 GiB` available memory | Below workstation |
| `workstation` | `>= 6` logical cores | `>= 16 GiB` available memory | Below swarm |
| `swarm` | `>= 12` logical cores | `>= 32 GiB` available memory | Highest tier |

### MUST Clauses

| ID | Clause | Coverage |
|----|--------|----------|
| RT-01 | MUST define `constrained`, `portable`, `workstation`, and `swarm` as the only valid profile IDs. | `OperatingProfile::from_str`; `perf_budget_conformance.rs` profile cases |
| RT-02 | MUST recommend `constrained` below `portable` thresholds. | `recommend_constrained_for_low_resources`; `minimal_resources_defaults_to_constrained` |
| RT-03 | MUST recommend `portable` at `>= 2` cores and `>= 8 GiB` when higher thresholds are not met. | `recommend_portable_for_laptop_resources`; `profile_thresholds_are_deterministic_at_boundaries` |
| RT-04 | MUST recommend `workstation` at `>= 6` cores and `>= 16 GiB` when swarm thresholds are not met. | `recommend_workstation_for_mid_resource_host`; `profile_thresholds_are_deterministic_at_boundaries` |
| RT-05 | MUST recommend `swarm` at `>= 12` cores and `>= 32 GiB`. | `recommend_swarm_for_high_resource_host`; `profile_thresholds_are_deterministic_at_boundaries` |
| RT-06 | MUST use the lower resource dimension because CPU and memory thresholds are conjunctive. | Boundary cases for below-swarm cores, below-swarm memory, below-workstation, and below-portable |
| RT-07 | MUST fall back to `constrained` when CPU and memory probe data are unavailable. | `recommend_constrained_when_probe_incomplete` |
| RT-08 | MUST lower confidence, not silently fail, when recommendation uses incomplete probe data. | `recommend_constrained_when_probe_incomplete` |
| RT-09 | MUST preserve tier ordering as resources increase. | `profile_tiers_are_ordered_by_resources` proptest |

## Probe Fields

| Field | JSON path | Validation |
|-------|-----------|------------|
| `schema` | `/schema` | `json_contract_snapshots__profile_config_*`; `report_serialization_omits_raw_paths_and_env_values` |
| `side_effect_free` | `/sideEffectFree` | `HostResourceProbeReport::gather_for_workspace`; JSON snapshots |
| `redaction` | `/redaction` | `report_serialization_omits_raw_paths_and_env_values`; JSON snapshots |
| `complete` | `/complete` | JSON snapshots; incomplete-probe unit coverage |
| `workspace.initialized` | `/workspace/initialized` | `profile_workflow_probe_recommend_plan_apply_dryrun`; JSON snapshots |
| `cpu.logical_cores` | `/cpu/logicalCores` | E2E plan assertion; threshold unit tests |
| `cpu.physical_cores` | `/cpu/physicalCores` | JSON contract snapshots preserve optional field |
| `memory.total_bytes` | `/memory/totalBytes` | E2E plan assertion; `/proc/meminfo` parser unit tests |
| `memory.available_bytes` | `/memory/availableBytes` | JSON snapshots; `/proc/meminfo` parser unit tests |
| `memory.cgroup_limit_bytes` | `/memory/cgroupLimitBytes` | JSON snapshots preserve optional field |
| `paths[]` | `/paths[]` | `path_probe_labels_are_stable`; `profile_probe_includes_path_filesystem_info` |
| `tools[]` | `/tools[]` | `tool_probe_order_is_stable_and_presence_only`; `profile_probe_includes_tool_availability` |
| `environment` | `/environment` | JSON snapshots preserve presence-only booleans |
| `degraded[]` | `/degraded[]` | `host_probe_degradation_codes_are_stable`; probe construction emits stable degradation codes for missing CPU, memory, or path capacity |

### MUST Clauses

| ID | Clause | Coverage |
|----|--------|----------|
| PF-01 | MUST emit `ee.profile.probe.v1`. | JSON snapshots; serialization unit test |
| PF-02 | MUST mark the probe side-effect-free. | JSON snapshots; report construction |
| PF-03 | MUST avoid raw workspace paths in serialized probe output. | `report_serialization_omits_raw_paths_and_env_values`; `path_probe_labels_are_stable` |
| PF-04 | MUST expose `cpu.logicalCores` for tier selection. | E2E plan assertion; recommendation tests |
| PF-05 | MUST preserve optional `cpu.physicalCores` in the schema. | JSON snapshots |
| PF-06 | MUST expose `memory.totalBytes` and `memory.availableBytes`. | E2E plan assertion; parser unit tests; JSON snapshots |
| PF-07 | MUST preserve optional `memory.cgroupLimitBytes` in the schema. | JSON snapshots |
| PF-08 | MUST expose stable path labels without raw paths. | `path_probe_labels_are_stable`; path E2E |
| PF-09 | MUST expose presence-only tool availability. | `tool_probe_order_is_stable_and_presence_only`; tool E2E |
| PF-10 | MUST expose presence-only environment hints. | JSON snapshots |

## Recommendation Determinism

| ID | Clause | Coverage |
|----|--------|----------|
| RD-01 | MUST return the same profile for identical probe inputs. | `profile_recommendation_is_deterministic` proptest, 1024 cases |
| RD-02 | MUST return the same confidence for identical probe inputs. | `profile_recommendation_is_deterministic` |
| RD-03 | MUST use deterministic threshold boundaries. | `profile_thresholds_are_deterministic_at_boundaries` |
| RD-04 | MUST include human-readable reasons for the selected profile. | JSON snapshots and E2E plan workflow |
| RD-05 | MUST keep profile config plan output stable after host-specific scrubbing. | `fixture_backed_agent_json_contracts_match_snapshots` |

## Budget Defaults And Scaling

| Budget | `constrained` | `portable` | `workstation` | `swarm` | Coverage |
|--------|---------------|------------|---------------|---------|----------|
| `search.candidate_limit` | 48 | 96 | 160 | 240 | Property and inline monotonic tests |
| `search.concurrent_index_readers` | 1 | 2 | 4 | 8 | JSON snapshots; conformance artifacts |
| `pack.max_tokens` | 3000 | 4500 | 6000 | 8000 | Property and inline monotonic tests |
| `pack.max_candidate_memories` | 24 | 48 | 96 | 160 | JSON snapshots; conformance artifacts |
| `cache.memory_cap_mb` | 128 | 512 | 1024 | 2048 | Property and inline monotonic tests |
| `cache.entry_cap` | 512 | 1024 | 4096 | 8192 | JSON snapshots; positivity test |
| `write_spool.batch_cap` | 32 | 64 | 128 | 256 | Budget conformance check coverage |
| `verification.recipe` | `quick` | `workspace` | `workspace` | `full` | Verification recipe unit tests |
| `verification.heavy_strategy` | `manual` | `rch_preferred` | `rch_preferred` | `rch_default` | Verification recipe unit tests |

### MUST Clauses

| ID | Clause | Coverage |
|----|--------|----------|
| BD-01 | MUST assign positive budgets for every profile. | `profile_budgets_are_consistent_across_all_profiles` |
| BD-02 | MUST scale search candidate limits monotonically. | `budget_scaling_is_monotonic_with_profile_tier`; `profile_budgets_scale_with_profile` |
| BD-03 | MUST scale pack token limits monotonically. | `budget_scaling_is_monotonic_with_profile_tier`; `profile_budgets_scale_with_profile` |
| BD-04 | MUST scale cache memory caps monotonically. | `budget_scaling_is_monotonic_with_profile_tier`; `profile_budgets_scale_with_profile` |
| BD-05 | MUST persist planned budget keys in stable TOML output. | `profile_config_plan_reports_exact_toml_without_writing`; JSON snapshots |
| BD-06 | MUST cap runtime search requests by the active profile. | `runtime_profile_caps_context_search_and_pack_budgets` |
| BD-07 | MUST cap runtime pack token requests by the active profile. | `runtime_profile_caps_context_search_and_pack_budgets` |
| BD-08 | MUST cap runtime pack candidate pools by the active profile. | `runtime_profile_caps_context_search_and_pack_budgets` |
| BD-09 | MUST cap index job limits by profile write-spool batch cap. | `runtime_profile_caps_index_jobs_from_write_spool_budget` |

## Verification Recipes

| ID | Clause | Coverage |
|----|--------|----------|
| VR-01 | MUST use the `quick` recipe for `constrained`. | `verification_recipe_constrained_skips_heavy_gates` |
| VR-02 | MUST skip heavy gates on constrained hosts with manual repair commands. | `verification_recipe_constrained_skips_heavy_gates`; `verification_recipe_skipped_gates_have_manual_commands` |
| VR-03 | MUST prefer RCH on `portable` and `workstation`. | `verification_recipe_portable_prefers_rch`; profile budget table |
| VR-04 | MUST include all heavy gates for `swarm`. | `verification_recipe_swarm_includes_all_gates` |
| VR-05 | MUST scale verification timeout from constrained to workstation/swarm. | `verification_recipe_timeout_scales_with_profile` |
| VR-06 | MUST serialize verification recipes with stable camelCase fields and schema. | `verification_recipe_serializes_to_json` |

## Budget Conformance Command

| ID | Clause | Coverage |
|----|--------|----------|
| BC-01 | MUST pass artifacts that match the requested profile. | `budget_check_constrained_profile_passes`; `budget_check_portable_profile_passes`; `budget_check_workstation_profile_passes`; `budget_check_swarm_profile_passes` |
| BC-02 | MUST detect profile mismatches. | `budget_check_profile_mismatch_detected`; `profile_budget_conformance_reports_cache_write_and_recipe_mismatches` |
| BC-03 | MUST report missing profile provenance. | `profile_budget_conformance_reports_missing_profile_provenance` |
| BC-04 | MUST report missing comparable metrics. | `profile_budget_conformance_reports_missing_profile_provenance` |
| BC-05 | MUST distinguish explicit overrides from failures. | `profile_budget_conformance_distinguishes_explicit_overrides` |
| BC-06 | MUST expose read-only command effect. | `budget_check_effect_is_read_only` |
| BC-07 | MUST keep JSON stdout machine-only. | `budget_check_json_stdout_contains_only_machine_data` |

## Workflow And JSON Contracts

| ID | Clause | Coverage |
|----|--------|----------|
| WF-01 | MUST run `ee init` before profile plan/apply in an isolated workspace. | `profile_workflow_probe_recommend_plan_apply_dryrun` |
| WF-02 | MUST emit `ee.profile.config.plan.v1` from plan and dry-run apply. | E2E workflow; JSON snapshots |
| WF-03 | MUST default plan to dry-run behavior. | E2E workflow; `profile_config_plan_reports_exact_toml_without_writing` |
| WF-04 | MUST keep apply `--dry-run` non-mutating. | E2E workflow; `profile_config_apply_dry_run_does_not_write` |
| WF-05 | MUST write config only on non-dry-run apply and make the next plan unchanged. | `profile_config_apply_writes_and_next_plan_is_unchanged` |
| WF-06 | MUST keep JSON diagnostics out of stderr. | `run_json_command`; E2E workflow |
| WF-07 | MUST keep repeated profile plans stable. | `profile_config_plan_is_idempotent` |

## Test Inventory

| File | Role |
|------|------|
| `src/core/profile.rs` inline tests | Probe parsing, redaction, thresholds, budget defaults, config planning/apply, verification recipes |
| `tests/property_profile_probe.rs` | 1024-case recommendation determinism and monotonic budget properties; budget conformance unit coverage |
| `tests/e2e_profile_workflow.rs` | Real-binary init -> plan -> dry-run apply workflow plus tool/path/idempotency probes |
| `tests/perf_budget_conformance.rs` | Real-binary `ee perf budget check` behavior and stdout/effect discipline |
| `tests/json_contract_snapshots.rs` | Stable JSON contract snapshots for profile plan/apply output |
| `tests/snapshots/json_contract_snapshots__profile_config_*.snap` | Golden profile plan/apply response shapes |

## Running Profile Coverage

Use RCH and an isolated target directory in this repository:

```bash
TMPDIR=/data/tmp CARGO_TARGET_DIR=/data/tmp/ee-profile-target rch exec -- cargo test --test property_profile_probe
TMPDIR=/data/tmp CARGO_TARGET_DIR=/data/tmp/ee-profile-target rch exec -- cargo test --test e2e_profile_workflow
TMPDIR=/data/tmp CARGO_TARGET_DIR=/data/tmp/ee-profile-target rch exec -- cargo test --test perf_budget_conformance
TMPDIR=/data/tmp CARGO_TARGET_DIR=/data/tmp/ee-profile-target rch exec -- cargo test --test json_contract_snapshots profile_config
```

## SHOULD Gaps

| ID | Clause | Status |
|----|--------|--------|
| SH-01 | SHOULD add direct tests that `memory.cgroupLimitBytes` constrains recommendation when it differs from total/available memory, if that becomes implemented behavior. | Not a current MUST; recommendation does not use cgroup limits today. |
| SH-02 | SHOULD extend monotonic property tests to every numeric budget field, including write-spool queue cap, retry budget, steward windows, and graph refresh budget. | Partially covered by snapshots, positivity checks, and conformance artifacts. |
