//! Integration modules N–R. Filter with `cargo test --test integration_n_r <module>::`.

#[path = "../neural_default_docs_contract.rs"]
mod neural_default_docs_contract;
#[path = "../no_forbidden_suffixes.rs"]
mod no_forbidden_suffixes;
#[path = "../no_mocks_e2e.rs"]
mod no_mocks_e2e;
#[path = "../no_silent_fallback_e2e.rs"]
mod no_silent_fallback_e2e;
#[path = "../no_silent_fallback_inventory.rs"]
mod no_silent_fallback_inventory;
#[path = "../north_star_context_e2e.rs"]
mod north_star_context_e2e;
#[path = "../output_negative.rs"]
mod output_negative;
#[path = "../pack_adaptive_budget_unit.rs"]
mod pack_adaptive_budget_unit;
#[path = "../pack_baseline_ledger_unit.rs"]
mod pack_baseline_ledger_unit;
#[path = "../pack_binary_format_unit.rs"]
mod pack_binary_format_unit;
#[path = "../pack_budget_too_small_test.rs"]
mod pack_budget_too_small_test;
#[path = "../pack_coordination_parse_unit.rs"]
mod pack_coordination_parse_unit;
#[path = "../pack_opt_out_flags_unit.rs"]
mod pack_opt_out_flags_unit;
#[path = "../pack_provenance_path_redaction_unit.rs"]
mod pack_provenance_path_redaction_unit;
#[path = "../pack_slo_unit.rs"]
mod pack_slo_unit;
#[path = "../package_artifact_leak.rs"]
mod package_artifact_leak;
#[path = "../perf_artifact_contract.rs"]
mod perf_artifact_contract;
#[path = "../perf_bench_envelope_contract.rs"]
mod perf_bench_envelope_contract;
#[path = "../perf_bench_status.rs"]
mod perf_bench_status;
#[path = "../perf_budget_conformance.rs"]
mod perf_budget_conformance;
#[path = "../plan_cache_unit.rs"]
mod plan_cache_unit;
#[path = "../plan_doc_completeness.rs"]
mod plan_doc_completeness;
#[path = "../plan_drift_gate.rs"]
mod plan_drift_gate;
#[path = "../policy_secret_detector_corpora.rs"]
mod policy_secret_detector_corpora;
#[path = "../posture_aggregation_unit.rs"]
mod posture_aggregation_unit;
#[path = "../ppr_context_pack.rs"]
mod ppr_context_pack;
#[path = "../ppr_prefetch_cache_smoke.rs"]
mod ppr_prefetch_cache_smoke;
#[path = "../preflight_destructive_unit.rs"]
mod preflight_destructive_unit;
#[path = "../preflight_guard.rs"]
mod preflight_guard;
#[path = "../preflight_hook_bash.rs"]
mod preflight_hook_bash;
#[path = "../preflight_hook_zsh.rs"]
mod preflight_hook_zsh;
#[path = "../preflight_token_cli.rs"]
mod preflight_token_cli;
#[path = "../preflight_token_lifecycle.rs"]
mod preflight_token_lifecycle;
#[path = "../primer_cli_golden.rs"]
mod primer_cli_golden;
#[path = "../procedure_distillation_skill.rs"]
mod procedure_distillation_skill;
#[path = "../profile_config_golden_e2e.rs"]
mod profile_config_golden_e2e;
#[path = "../proof_check_schema.rs"]
mod proof_check_schema;
#[path = "../proof_verify_core.rs"]
mod proof_verify_core;
#[path = "../property_context_query_metamorphic.rs"]
mod property_context_query_metamorphic;
#[path = "../property_eql_query_parsing.rs"]
mod property_eql_query_parsing;
#[path = "../property_graph_articulation_points.rs"]
mod property_graph_articulation_points;
#[path = "../property_graph_dominance_frontier.rs"]
mod property_graph_dominance_frontier;
#[path = "../property_graph_gomory_hu.rs"]
mod property_graph_gomory_hu;
#[path = "../property_graph_hits.rs"]
mod property_graph_hits;
#[path = "../property_graph_k_truss.rs"]
mod property_graph_k_truss;
#[path = "../property_graph_minhash_rank.rs"]
mod property_graph_minhash_rank;
#[path = "../property_graph_onion_layers.rs"]
mod property_graph_onion_layers;
#[path = "../property_graph_pagerank.rs"]
mod property_graph_pagerank;
#[path = "../property_graph_topological_order.rs"]
mod property_graph_topological_order;
#[path = "../property_graph_transitive_closure.rs"]
mod property_graph_transitive_closure;
#[path = "../property_mesh_frame.rs"]
mod property_mesh_frame;
#[path = "../property_origin_stream.rs"]
mod property_origin_stream;
#[path = "../property_output_governor.rs"]
mod property_output_governor;
#[path = "../property_pack_metamorphic.rs"]
mod property_pack_metamorphic;
#[path = "../property_pack_profile_variation_metamorphic.rs"]
mod property_pack_profile_variation_metamorphic;
#[path = "../property_plan_cache.rs"]
mod property_plan_cache;
#[path = "../property_profile_probe.rs"]
mod property_profile_probe;
#[path = "../property_query_and_pack.rs"]
mod property_query_and_pack;
#[path = "../property_read_pool.rs"]
mod property_read_pool;
#[path = "../property_redaction_idempotence.rs"]
mod property_redaction_idempotence;
#[path = "../property_remember_search_metamorphic.rs"]
mod property_remember_search_metamorphic;
#[path = "../property_response_envelope.rs"]
mod property_response_envelope;
#[path = "../property_shadow_tuning.rs"]
mod property_shadow_tuning;
#[path = "../property_simhash.rs"]
mod property_simhash;
#[path = "../quick_redaction_check.rs"]
mod quick_redaction_check;
#[path = "../radix_ulid_sort_proptest.rs"]
mod radix_ulid_sort_proptest;
#[path = "../randomness_inventory_schema_unit.rs"]
mod randomness_inventory_schema_unit;
#[path = "../rch_compile_blocker_router.rs"]
mod rch_compile_blocker_router;
#[path = "../rch_docs_contract.rs"]
mod rch_docs_contract;
#[path = "../rch_local_cargo_tripwire.rs"]
mod rch_local_cargo_tripwire;
#[path = "../rch_portability_diagnostic.rs"]
mod rch_portability_diagnostic;
#[path = "../rch_recover_verification_contract.rs"]
mod rch_recover_verification_contract;
#[path = "../rch_runbook_docs_lint.rs"]
mod rch_runbook_docs_lint;
#[path = "../rch_verify_contract.rs"]
mod rch_verify_contract;
#[path = "../rch_verify_control_plane.rs"]
mod rch_verify_control_plane;
#[path = "../read_fence_properties.rs"]
mod read_fence_properties;
#[path = "../read_pool_concurrency_e2e.rs"]
mod read_pool_concurrency_e2e;
#[path = "../readme_cli_parity.rs"]
mod readme_cli_parity;
#[path = "../readme_command_reference_in_sync.rs"]
mod readme_command_reference_in_sync;
#[path = "../readme_invariant_harness.rs"]
mod readme_invariant_harness;
#[path = "../readme_invariant_manifest_schema.rs"]
mod readme_invariant_manifest_schema;
#[path = "../readme_perf_coverage_test.rs"]
mod readme_perf_coverage_test;
#[path = "../readme_perf_sync.rs"]
mod readme_perf_sync;
#[path = "../recall_cli_golden.rs"]
mod recall_cli_golden;
#[path = "../recorder_event_spine_contract.rs"]
mod recorder_event_spine_contract;
#[path = "../recorder_persistence.rs"]
mod recorder_persistence;
#[path = "../recorder_tail_follow.rs"]
mod recorder_tail_follow;
#[path = "../redaction_fuzz.rs"]
mod redaction_fuzz;
#[path = "../redaction_levels_doc_consistency_test.rs"]
mod redaction_levels_doc_consistency_test;
#[path = "../redaction_levels_unit.rs"]
mod redaction_levels_unit;
#[path = "../reflection_handshake_contracts.rs"]
mod reflection_handshake_contracts;
#[path = "../release_manifest.rs"]
mod release_manifest;
#[path = "../release_provenance_schema.rs"]
mod release_provenance_schema;
#[path = "../release_provenance_signing_unit.rs"]
mod release_provenance_signing_unit;
#[path = "../remote_embedding_backend_e2e.rs"]
mod remote_embedding_backend_e2e;
#[path = "../renderer_command_capabilities.rs"]
mod renderer_command_capabilities;
#[path = "../rerank_deadlock_regression.rs"]
mod rerank_deadlock_regression;
#[path = "../rerank_model_manifest_smoke.rs"]
mod rerank_model_manifest_smoke;
#[path = "../rerank_posture_contract.rs"]
mod rerank_posture_contract;
#[path = "../resource_admission_queue_pressure_conformance.rs"]
mod resource_admission_queue_pressure_conformance;
#[path = "../resource_admission_report_conformance.rs"]
mod resource_admission_report_conformance;
#[path = "../response_envelope_conformance_matrix.rs"]
mod response_envelope_conformance_matrix;
#[path = "../retrieval_pipeline_monotonic.rs"]
mod retrieval_pipeline_monotonic;
#[path = "../rule_mark_update_e2e.rs"]
mod rule_mark_update_e2e;
#[path = "../rule_provenance_integration.rs"]
mod rule_provenance_integration;
#[path = "../rule_provenance_unit.rs"]
mod rule_provenance_unit;
