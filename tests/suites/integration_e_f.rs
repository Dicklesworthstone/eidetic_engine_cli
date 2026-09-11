//! Integration modules E–F. Filter with `cargo test --test integration_e_f <module>::`.

#[path = "../e16_e17_e12_domain_e2e.rs"]
mod e16_e17_e12_domain_e2e;
#[path = "../e2e_agent_sources.rs"]
mod e2e_agent_sources;
#[path = "../e2e_appendix_c_agent_flow.rs"]
mod e2e_appendix_c_agent_flow;
#[path = "../e2e_artifact_manifest_contract.rs"]
mod e2e_artifact_manifest_contract;
#[path = "../e2e_ask_script.rs"]
mod e2e_ask_script;
#[path = "../e2e_capture_suggest_pin.rs"]
mod e2e_capture_suggest_pin;
#[path = "../e2e_cass_import_dry_run_and_since_filter.rs"]
mod e2e_cass_import_dry_run_and_since_filter;
#[path = "../e2e_cass_import_invalid_binary_envelope.rs"]
mod e2e_cass_import_invalid_binary_envelope;
#[path = "../e2e_cass_import_redaction.rs"]
mod e2e_cass_import_redaction;
#[path = "../e2e_cass_import_sessions_invalid_json.rs"]
mod e2e_cass_import_sessions_invalid_json;
#[path = "../e2e_cass_import_since_invalid_duration.rs"]
mod e2e_cass_import_since_invalid_duration;
#[path = "../e2e_cass_import_since_overflow.rs"]
mod e2e_cass_import_since_overflow;
#[path = "../e2e_config_set_invalid_value.rs"]
mod e2e_config_set_invalid_value;
#[path = "../e2e_config_unknown_key.rs"]
mod e2e_config_unknown_key;
#[path = "../e2e_context_show_missing_db.rs"]
mod e2e_context_show_missing_db;
#[path = "../e2e_curate_auto_promote.rs"]
mod e2e_curate_auto_promote;
#[path = "../e2e_curate_candidates_validators.rs"]
mod e2e_curate_candidates_validators;
#[path = "../e2e_curate_disposition.rs"]
mod e2e_curate_disposition;
#[path = "../e2e_curate_propose_derived.rs"]
mod e2e_curate_propose_derived;
#[path = "../e2e_curate_review_actions.rs"]
mod e2e_curate_review_actions;
#[path = "../e2e_curate_tombstone.rs"]
mod e2e_curate_tombstone;
#[path = "../e2e_curate_validate_errors.rs"]
mod e2e_curate_validate_errors;
#[path = "../e2e_db.rs"]
mod e2e_db;
#[path = "../e2e_derived_apply_race_retry.rs"]
mod e2e_derived_apply_race_retry;
#[path = "../e2e_diag_resource_admission.rs"]
mod e2e_diag_resource_admission;
#[path = "../e2e_doctor_concise_default.rs"]
mod e2e_doctor_concise_default;
#[path = "../e2e_doctor_robot_docs.rs"]
mod e2e_doctor_robot_docs;
#[path = "../e2e_doctor_robot_triage.rs"]
mod e2e_doctor_robot_triage;
#[path = "../e2e_economy_pin.rs"]
mod e2e_economy_pin;
#[path = "../e2e_graph_articulation.rs"]
mod e2e_graph_articulation;
#[path = "../e2e_graph_betweenness.rs"]
mod e2e_graph_betweenness;
#[path = "../e2e_graph_centrality.rs"]
mod e2e_graph_centrality;
#[path = "../e2e_graph_centrality_algorithm.rs"]
mod e2e_graph_centrality_algorithm;
#[path = "../e2e_graph_centrality_memory_id_no_match.rs"]
mod e2e_graph_centrality_memory_id_no_match;
#[path = "../e2e_graph_centrality_missing_db.rs"]
mod e2e_graph_centrality_missing_db;
#[path = "../e2e_graph_communities.rs"]
mod e2e_graph_communities;
#[path = "../e2e_graph_explain_link.rs"]
mod e2e_graph_explain_link;
#[path = "../e2e_graph_export.rs"]
mod e2e_graph_export;
#[path = "../e2e_graph_feature_enrichment.rs"]
mod e2e_graph_feature_enrichment;
#[path = "../e2e_graph_hits.rs"]
mod e2e_graph_hits;
#[path = "../e2e_graph_k_core.rs"]
mod e2e_graph_k_core;
#[path = "../e2e_graph_logging_scripts.rs"]
mod e2e_graph_logging_scripts;
#[path = "../e2e_graph_louvain.rs"]
mod e2e_graph_louvain;
#[path = "../e2e_graph_neighborhood_mermaid.rs"]
mod e2e_graph_neighborhood_mermaid;
#[path = "../e2e_graph_neighborhood_validators.rs"]
mod e2e_graph_neighborhood_validators;
#[path = "../e2e_graph_pagerank.rs"]
mod e2e_graph_pagerank;
#[path = "../e2e_graph_pagerank_tombstoned.rs"]
mod e2e_graph_pagerank_tombstoned;
#[path = "../e2e_graph_path.rs"]
mod e2e_graph_path;
#[path = "../e2e_graph_snapshot_refresh_validators.rs"]
mod e2e_graph_snapshot_refresh_validators;
#[path = "../e2e_insights_json_stream_mutex.rs"]
mod e2e_insights_json_stream_mutex;
#[path = "../e2e_jsonl_export_field_validation.rs"]
mod e2e_jsonl_export_field_validation;
#[path = "../e2e_lab_swarm_replay_malformed.rs"]
mod e2e_lab_swarm_replay_malformed;
#[path = "../e2e_lab_swarm_workload_generator.rs"]
mod e2e_lab_swarm_workload_generator;
#[path = "../e2e_mcp_initialize_capabilities.rs"]
mod e2e_mcp_initialize_capabilities;
#[path = "../e2e_mcp_prompts.rs"]
mod e2e_mcp_prompts;
#[path = "../e2e_mcp_prompts_get_errors.rs"]
mod e2e_mcp_prompts_get_errors;
#[path = "../e2e_mcp_request_error_envelopes.rs"]
mod e2e_mcp_request_error_envelopes;
#[path = "../e2e_mcp_resource_templates.rs"]
mod e2e_mcp_resource_templates;
#[path = "../e2e_mcp_resources_list.rs"]
mod e2e_mcp_resources_list;
#[path = "../e2e_mcp_resources_read_errors.rs"]
mod e2e_mcp_resources_read_errors;
#[path = "../e2e_mcp_tools_call_errors.rs"]
mod e2e_mcp_tools_call_errors;
#[path = "../e2e_mcp_top_level.rs"]
mod e2e_mcp_top_level;
#[path = "../e2e_mcp_wave_tools.rs"]
mod e2e_mcp_wave_tools;
#[path = "../e2e_memory_expire.rs"]
mod e2e_memory_expire;
#[path = "../e2e_memory_link_list_and_dry_run.rs"]
mod e2e_memory_link_list_and_dry_run;
#[path = "../e2e_memory_link_semantic.rs"]
mod e2e_memory_link_semantic;
#[path = "../e2e_memory_link_usage.rs"]
mod e2e_memory_link_usage;
#[path = "../e2e_memory_revise_errors.rs"]
mod e2e_memory_revise_errors;
#[path = "../e2e_memory_show_and_history.rs"]
mod e2e_memory_show_and_history;
#[path = "../e2e_memory_tags_validation.rs"]
mod e2e_memory_tags_validation;
#[path = "../e2e_mesh_lane_grant.rs"]
mod e2e_mesh_lane_grant;
#[path = "../e2e_migration_boundary.rs"]
mod e2e_migration_boundary;
#[path = "../e2e_multi_process_write.rs"]
mod e2e_multi_process_write;
#[path = "../e2e_north_star_distillation.rs"]
mod e2e_north_star_distillation;
#[path = "../e2e_outcome_invalid_memory.rs"]
mod e2e_outcome_invalid_memory;
#[path = "../e2e_outcome_invalid_memory_id.rs"]
mod e2e_outcome_invalid_memory_id;
#[path = "../e2e_pack_determinism.rs"]
mod e2e_pack_determinism;
#[path = "../e2e_perf_compare.rs"]
mod e2e_perf_compare;
#[path = "../e2e_plan_recipe.rs"]
mod e2e_plan_recipe;
#[path = "../e2e_plan_recipe_list_category.rs"]
mod e2e_plan_recipe_list_category;
#[path = "../e2e_profile_workflow.rs"]
mod e2e_profile_workflow;
#[path = "../e2e_proximity.rs"]
mod e2e_proximity;
#[path = "../e2e_query_file_validation.rs"]
mod e2e_query_file_validation;
#[path = "../e2e_remember_empty_content.rs"]
mod e2e_remember_empty_content;
#[path = "../e2e_remember_from_git.rs"]
mod e2e_remember_from_git;
#[path = "../e2e_retention_contract.rs"]
mod e2e_retention_contract;
#[path = "../e2e_retrieval_truth_script.rs"]
mod e2e_retrieval_truth_script;
#[path = "../e2e_schema_export_unknown.rs"]
mod e2e_schema_export_unknown;
#[path = "../e2e_subscribe_poll_filter_validators.rs"]
mod e2e_subscribe_poll_filter_validators;
#[path = "../e2e_subscribe_stream_renderer.rs"]
mod e2e_subscribe_stream_renderer;
#[path = "../e2e_swarm_contention_recovery.rs"]
mod e2e_swarm_contention_recovery;
#[path = "../e2e_trauma_guard.rs"]
mod e2e_trauma_guard;
#[path = "../e2e_typed_fields_decide_script.rs"]
mod e2e_typed_fields_decide_script;
#[path = "../e2e_why.rs"]
mod e2e_why;
#[path = "../ee_core_api_no_adapter_logic.rs"]
mod ee_core_api_no_adapter_logic;
#[path = "../effect_contracts.rs"]
mod effect_contracts;
#[path = "../embed_dedup_e2e.rs"]
mod embed_dedup_e2e;
#[path = "../embed_dedup_unit.rs"]
mod embed_dedup_unit;
#[path = "../env_registry_lint.rs"]
mod env_registry_lint;
#[path = "../env_registry_unit.rs"]
mod env_registry_unit;
#[path = "../env_vars_doc_coverage.rs"]
mod env_vars_doc_coverage;
#[path = "../environment_attestation_fixtures.rs"]
mod environment_attestation_fixtures;
#[path = "../eql_query_schema.rs"]
mod eql_query_schema;
#[path = "../error_envelope_v2_unit.rs"]
mod error_envelope_v2_unit;
#[path = "../error_recall_e2e.rs"]
mod error_recall_e2e;
#[path = "../eval_ask_quality_unit.rs"]
mod eval_ask_quality_unit;
#[path = "../eval_fixtures.rs"]
mod eval_fixtures;
#[path = "../eval_report_e2e.rs"]
mod eval_report_e2e;
#[path = "../eval_run_happy_path.rs"]
mod eval_run_happy_path;
#[path = "../exit_code_conformance.rs"]
mod exit_code_conformance;
#[path = "../export_playbook_e2e.rs"]
mod export_playbook_e2e;
#[path = "../failure_mode_catalog_coverage.rs"]
mod failure_mode_catalog_coverage;
#[path = "../failure_mode_impact_runner.rs"]
mod failure_mode_impact_runner;
#[path = "../fake_tailscale_harness.rs"]
mod fake_tailscale_harness;
#[path = "../fanout_rollback.rs"]
mod fanout_rollback;
#[path = "../feature_flag_registry_in_sync.rs"]
mod feature_flag_registry_in_sync;
#[path = "../feedback_gated_properties.rs"]
mod feedback_gated_properties;
#[path = "../field_selector_unit.rs"]
mod field_selector_unit;
#[path = "../flag_precedence.rs"]
mod flag_precedence;
#[path = "../fleet_identifier_guard.rs"]
mod fleet_identifier_guard;
#[path = "../focus_suggest_phase2_e2e.rs"]
mod focus_suggest_phase2_e2e;
#[path = "../focus_suggest_schema.rs"]
mod focus_suggest_schema;
#[path = "../forbidden_deps.rs"]
mod forbidden_deps;
#[path = "../frankensearch_conformance.rs"]
mod frankensearch_conformance;
