//! Integration modules A–D. Filter with `cargo test --test integration_a_d <module>::`.

mod inventory;

#[path = "../adr_0028_docs.rs"]
mod adr_0028_docs;
#[path = "../adr_0029_docs.rs"]
mod adr_0029_docs;
#[path = "../adr_0030_docs.rs"]
mod adr_0030_docs;
#[path = "../adr_0031_docs.rs"]
mod adr_0031_docs;
#[path = "../adr_0032_docs.rs"]
mod adr_0032_docs;
#[path = "../adr_0048_0049_0050_deferred_docs.rs"]
mod adr_0048_0049_0050_deferred_docs;
#[path = "../adr_index_consistency.rs"]
mod adr_index_consistency;
#[path = "../advanced_e2e.rs"]
mod advanced_e2e;
#[path = "../advanced_subsystem_logged_e2e.rs"]
mod advanced_subsystem_logged_e2e;
#[path = "../agent_golden_baselines.rs"]
mod agent_golden_baselines;
#[path = "../agent_outcome_scenario_pack.rs"]
mod agent_outcome_scenario_pack;
#[path = "../agent_triad_inference_acc.rs"]
mod agent_triad_inference_acc;
#[path = "../agents_md_referenced_commands_compile.rs"]
mod agents_md_referenced_commands_compile;
#[path = "../agentsmd_bridge.rs"]
mod agentsmd_bridge;
#[path = "../arena_parity_golden.rs"]
mod arena_parity_golden;
#[path = "../attestation_contracts.rs"]
mod attestation_contracts;
#[path = "../audit_install_pipeline_schema_unit.rs"]
mod audit_install_pipeline_schema_unit;
#[path = "../audit_jsonl_contract.rs"]
mod audit_jsonl_contract;
#[path = "../auto_enroll_documentation_consistency.rs"]
mod auto_enroll_documentation_consistency;
#[path = "../auto_enroll_perf_baseline.rs"]
mod auto_enroll_perf_baseline;
#[path = "../auto_enroll_real_tailscale_self_test.rs"]
mod auto_enroll_real_tailscale_self_test;
#[path = "../bayesian_backfill_unit.rs"]
mod bayesian_backfill_unit;
#[path = "../bayesian_credible_interval_unit.rs"]
mod bayesian_credible_interval_unit;
#[path = "../bayesian_harmful_asymmetry_unit.rs"]
mod bayesian_harmful_asymmetry_unit;
#[path = "../bayesian_posterior_update_unit.rs"]
mod bayesian_posterior_update_unit;
#[path = "../bayesian_trust_class_transitions_unit.rs"]
mod bayesian_trust_class_transitions_unit;
#[path = "../bead_affinity_schema_unit.rs"]
mod bead_affinity_schema_unit;
#[path = "../boundary_fixture_corpus.rs"]
mod boundary_fixture_corpus;
#[path = "../boundary_migration_logging.rs"]
mod boundary_migration_logging;
#[path = "../bridge_staleness_gate.rs"]
mod bridge_staleness_gate;
#[path = "../cache_prewarm_cli_contract.rs"]
mod cache_prewarm_cli_contract;
#[path = "../cache_prewarm_e2e.rs"]
mod cache_prewarm_e2e;
#[path = "../cancellation_graph.rs"]
mod cancellation_graph;
#[path = "../capabilities_contract_e2e.rs"]
mod capabilities_contract_e2e;
#[path = "../cass_import_concurrency.rs"]
mod cass_import_concurrency;
#[path = "../cass_prefetch_integration.rs"]
mod cass_prefetch_integration;
#[path = "../check_contract_e2e.rs"]
mod check_contract_e2e;
#[path = "../cli_arg_hygiene.rs"]
mod cli_arg_hygiene;
#[path = "../cli_completion_regen.rs"]
mod cli_completion_regen;
#[path = "../cli_help_emits.rs"]
mod cli_help_emits;
#[path = "../cli_loop_e2e.rs"]
mod cli_loop_e2e;
#[path = "../cli_no_panic_smoke.rs"]
mod cli_no_panic_smoke;
#[path = "../closeout_audit_runner_unit.rs"]
mod closeout_audit_runner_unit;
#[path = "../closure_lint_harness.rs"]
mod closure_lint_harness;
#[path = "../clustering_coherence_unit.rs"]
mod clustering_coherence_unit;
#[path = "../concurrent_search_lexical_arm_e2e.rs"]
mod concurrent_search_lexical_arm_e2e;
#[path = "../conformal_coverage.rs"]
mod conformal_coverage;
#[path = "../consensus_conflict_unit.rs"]
mod consensus_conflict_unit;
#[path = "../consolidation_maintain_e2e.rs"]
mod consolidation_maintain_e2e;
#[path = "../context_delta_response_degradation_merge.rs"]
mod context_delta_response_degradation_merge;
#[path = "../context_delta_schema_docs.rs"]
mod context_delta_schema_docs;
#[path = "../contract_drift_radar_degraded_taxonomy.rs"]
mod contract_drift_radar_degraded_taxonomy;
#[path = "../contract_drift_radar_schema_source.rs"]
mod contract_drift_radar_schema_source;
#[path = "../contradiction_detect_properties.rs"]
mod contradiction_detect_properties;
#[path = "../coord_watchdog_hung_source.rs"]
mod coord_watchdog_hung_source;
#[path = "../coordination_snapshot_unit.rs"]
mod coordination_snapshot_unit;
#[path = "../corpus_integrity.rs"]
mod corpus_integrity;
#[path = "../curate_auto_promote_safety_contracts.rs"]
mod curate_auto_promote_safety_contracts;
#[path = "../curation_candidates_v062_unit.rs"]
mod curation_candidates_v062_unit;
#[path = "../daemon_bounded_pool_e2e.rs"]
mod daemon_bounded_pool_e2e;
#[path = "../daemon_start_lifecycle.rs"]
mod daemon_start_lifecycle;
#[path = "../daemon_uds_rpc_round_trip.rs"]
mod daemon_uds_rpc_round_trip;
#[path = "../dangling_command_refs.rs"]
mod dangling_command_refs;
#[path = "../db_inspection_integration.rs"]
mod db_inspection_integration;
#[path = "../decay_audit_coverage_unit.rs"]
mod decay_audit_coverage_unit;
#[path = "../decay_compute_unit.rs"]
mod decay_compute_unit;
#[path = "../decay_level_demotion_unit.rs"]
mod decay_level_demotion_unit;
#[path = "../decay_reversibility_unit.rs"]
mod decay_reversibility_unit;
#[path = "../decay_threshold_classification_unit.rs"]
mod decay_threshold_classification_unit;
#[path = "../decide_workflow.rs"]
mod decide_workflow;
#[path = "../degraded_aggregation_emitter_inventory.rs"]
mod degraded_aggregation_emitter_inventory;
#[path = "../degraded_aggregation_worst_case.rs"]
mod degraded_aggregation_worst_case;
#[path = "../degraded_code_taxonomy_consistency_test.rs"]
mod degraded_code_taxonomy_consistency_test;
#[path = "../degraded_codes_doc_coverage.rs"]
mod degraded_codes_doc_coverage;
#[path = "../degraded_honesty.rs"]
mod degraded_honesty;
#[path = "../degraded_no_build_time_codes_unit.rs"]
mod degraded_no_build_time_codes_unit;
#[path = "../demo_env_redaction.rs"]
mod demo_env_redaction;
#[path = "../derived_memory_candidate_contracts.rs"]
mod derived_memory_candidate_contracts;
#[path = "../determinism_capability_token_unit.rs"]
mod determinism_capability_token_unit;
#[path = "../determinism_exemption_audit.rs"]
mod determinism_exemption_audit;
#[path = "../determinism_lint_catches_known_violations.rs"]
mod determinism_lint_catches_known_violations;
#[path = "../determinism_unit.rs"]
mod determinism_unit;
#[path = "../diag_plan_cache_e2e.rs"]
mod diag_plan_cache_e2e;
#[path = "../diagnostics_banner_aliasing_unit.rs"]
mod diagnostics_banner_aliasing_unit;
#[path = "../diagnostics_banner_categorization_unit.rs"]
mod diagnostics_banner_categorization_unit;
#[path = "../diagnostics_banner_emission_unit.rs"]
mod diagnostics_banner_emission_unit;
#[path = "../docs_bootstrap_guards.rs"]
mod docs_bootstrap_guards;
#[path = "../docs_schemas_match_responses.rs"]
mod docs_schemas_match_responses;
#[path = "../doctor_capabilities_golden_e2e.rs"]
mod doctor_capabilities_golden_e2e;
#[path = "../doctor_fixtures_contract.rs"]
mod doctor_fixtures_contract;
#[path = "../doctor_runtime_e2e.rs"]
mod doctor_runtime_e2e;
#[path = "../doctor_safety_harness_wiring.rs"]
mod doctor_safety_harness_wiring;
