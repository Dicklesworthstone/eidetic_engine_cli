#[path = "contracts/dependency_contract_matrix.rs"]
mod dependency_contract_matrix;

#[path = "contracts/cass_robot.rs"]
mod cass_robot;

#[path = "contracts/cass_subprocess_diagnostics_goldens.rs"]
mod cass_subprocess_diagnostics_goldens;

#[path = "contracts/cass_session_uri_contract.rs"]
mod cass_session_uri_contract;

#[path = "contracts/cass_import_error_repair_hint_contract.rs"]
mod cass_import_error_repair_hint_contract;

#[path = "contracts/cass_error_repair_hint_contract.rs"]
mod cass_error_repair_hint_contract;

#[path = "contracts/cass_error_display_contract.rs"]
mod cass_error_display_contract;

#[path = "contracts/cass_import_options_defaults.rs"]
mod cass_import_options_defaults;

#[path = "contracts/cass_import_error_display_contract.rs"]
mod cass_import_error_display_contract;

#[path = "contracts/cass_import_error_from_conversions.rs"]
mod cass_import_error_from_conversions;

#[path = "contracts/cass_import_parse_summaries.rs"]
mod cass_import_parse_summaries;

#[path = "contracts/cass_session_reference_to_uri.rs"]
mod cass_session_reference_to_uri;

#[path = "contracts/cass_session_info_defaults.rs"]
mod cass_session_info_defaults;

#[path = "contracts/cass_session_info_builders.rs"]
mod cass_session_info_builders;

#[path = "contracts/cass_client_extra_env.rs"]
mod cass_client_extra_env;

#[path = "contracts/cass_import_guidance_from_inventory.rs"]
mod cass_import_guidance_from_inventory;

#[path = "contracts/doctor_integrity_status_severity_canary_as_str.rs"]
mod doctor_integrity_status_severity_canary_as_str;

#[path = "contracts/doctor_mesh_auto_enrollment_check_status.rs"]
mod doctor_mesh_auto_enrollment_check_status;

#[path = "contracts/cass_exit_code_constants.rs"]
mod cass_exit_code_constants;

#[path = "contracts/cass_import_session_status_as_str.rs"]
mod cass_import_session_status_as_str;

#[path = "contracts/cass_invocation_builder.rs"]
mod cass_invocation_builder;

#[path = "contracts/cass_client_command_invocations.rs"]
mod cass_client_command_invocations;

#[path = "contracts/cass_import_session_result_source_path.rs"]
mod cass_import_session_result_source_path;

#[path = "contracts/cass_contract_constants.rs"]
mod cass_contract_constants;

#[path = "contracts/cass_client_defaults.rs"]
mod cass_client_defaults;

#[path = "contracts/cass_schema_id_constants.rs"]
mod cass_schema_id_constants;

#[path = "contracts/cass_error_from_io.rs"]
mod cass_error_from_io;

#[path = "contracts/cass_view_span_defaults.rs"]
mod cass_view_span_defaults;

#[path = "contracts/cass_default_impls.rs"]
mod cass_default_impls;

#[path = "contracts/cass_subsystem_name.rs"]
mod cass_subsystem_name;

#[path = "contracts/cass_error_is_degraded.rs"]
mod cass_error_is_degraded;

#[path = "contracts/cass_health_predicates.rs"]
mod cass_health_predicates;

#[path = "contracts/cass_import_error_subprocess_diagnostics_none.rs"]
mod cass_import_error_subprocess_diagnostics_none;

#[path = "contracts/cass_error_equality.rs"]
mod cass_error_equality;

#[path = "contracts/cass_contract_version_getters.rs"]
mod cass_contract_version_getters;

#[path = "contracts/cass_unavailable_degradation.rs"]
mod cass_unavailable_degradation;

#[path = "contracts/cass_import_guidance_status_as_str.rs"]
mod cass_import_guidance_status_as_str;

#[path = "contracts/agent_inventory_status_as_str.rs"]
mod agent_inventory_status_as_str;

#[path = "contracts/cass_import_root_guidance_fields.rs"]
mod cass_import_root_guidance_fields;

#[path = "contracts/cass_import_guidance_commands_and_message.rs"]
mod cass_import_guidance_commands_and_message;

#[path = "contracts/agent_detect_schema_constants.rs"]
mod agent_detect_schema_constants;

#[path = "contracts/cass_import_guidance_agent_count.rs"]
mod cass_import_guidance_agent_count;

#[path = "contracts/agent_inventory_degradation_fields.rs"]
mod agent_inventory_degradation_fields;

#[path = "contracts/agent_path_rewrite_apply.rs"]
mod agent_path_rewrite_apply;

#[path = "contracts/cass_stdout_decode_fuzz_summary.rs"]
mod cass_stdout_decode_fuzz_summary;

#[path = "contracts/cass_import_report_goldens.rs"]
mod cass_import_report_goldens;

#[path = "conformance/cass_contracts.rs"]
mod cass_contracts;

#[path = "contracts/integration_foundation.rs"]
mod integration_foundation;

#[path = "contracts/sqlmodel_frankensqlite.rs"]
mod sqlmodel_frankensqlite;

#[path = "contracts/frankensearch_local.rs"]
mod frankensearch_local;

#[path = "contracts/asupersync_budget.rs"]
mod asupersync_budget;

#[path = "contracts/asupersync_cancellation.rs"]
mod asupersync_cancellation;

#[path = "contracts/asupersync_quiescence.rs"]
mod asupersync_quiescence;

#[path = "contracts/schema_drift.rs"]
mod schema_drift;

#[path = "contracts/toon_gate12.rs"]
mod toon_gate12;

#[path = "contracts/claims.rs"]
mod claims;

#[path = "contracts/shadow_run.rs"]
mod shadow_run;

#[path = "contracts/repro_packs.rs"]
mod repro_packs;

#[path = "contracts/demo_manifests.rs"]
mod demo_manifests;

#[path = "contracts/cache_admission.rs"]
mod cache_admission;

#[path = "contracts/causal_trace.rs"]
mod causal_trace;

#[path = "contracts/causal_credit.rs"]
mod causal_credit;

#[path = "contracts/submodular_packer.rs"]
mod submodular_packer;

#[path = "contracts/certificates.rs"]
mod certificates;

#[path = "contracts/curation_calibration.rs"]
mod curation_calibration;

#[path = "contracts/lifecycle_automata.rs"]
mod lifecycle_automata;

#[path = "contracts/counterfactual_gate15.rs"]
mod counterfactual_gate15;

#[path = "contracts/recorder_gate17.rs"]
mod recorder_gate17;

#[path = "contracts/recorder_event_spine.rs"]
mod recorder_event_spine;

#[path = "contracts/preflight_tripwires.rs"]
mod preflight_tripwires;

#[path = "contracts/agent_operating_contract_read_only.rs"]
mod agent_operating_contract_read_only;

#[path = "contracts/procedure_gate18.rs"]
mod procedure_gate18;

#[path = "contracts/situation_gate19.rs"]
mod situation_gate19;

#[path = "contracts/economy_gate20.rs"]
mod economy_gate20;

#[path = "contracts/procedure_drift.rs"]
mod procedure_drift;

#[path = "contracts/mermaid_gate11.rs"]
mod mermaid_gate11;

#[path = "contracts/active_learning_gate21.rs"]
mod active_learning_gate21;

#[path = "contracts/agent_status.rs"]
mod agent_status;

#[path = "contracts/franken_agent_detection_default.rs"]
mod franken_agent_detection_default;

#[path = "contracts/fastmcp_rust_adapter.rs"]
mod fastmcp_rust_adapter;

#[path = "contracts/eval_science.rs"]
mod eval_science;

#[path = "contracts/science_analytics.rs"]
mod science_analytics;

#[path = "contracts/retrieval_field_naming.rs"]
mod retrieval_field_naming;

#[path = "contracts/no_silent_fallback.rs"]
mod no_silent_fallback;

#[path = "contracts/canonical_content_field.rs"]
mod canonical_content_field;

#[path = "contracts/schema_canonical_fields.rs"]
mod schema_canonical_fields;

#[path = "contracts/context_pack_dual_render.rs"]
mod context_pack_dual_render;

#[path = "contracts/schema_roundtrip.rs"]
mod schema_roundtrip;

#[path = "contracts/singleflight_key_schema.rs"]
mod singleflight_key_schema;

#[path = "contracts/symbol_snapshot_schema.rs"]
mod symbol_snapshot_schema;

#[path = "contracts/peer_conflict_schema.rs"]
mod peer_conflict_schema;

#[path = "contracts/symbol_graph_artifacts.rs"]
mod symbol_graph_artifacts;

#[path = "contracts/graph_schemas_v1.rs"]
mod graph_schemas_v1;

#[path = "contracts/graph_audit.rs"]
mod graph_audit;

#[path = "contracts/insights_stream.rs"]
mod insights_stream;

#[path = "contracts/graph_additive_only.rs"]
mod graph_additive_only;

#[path = "contracts/graph_config_behavior.rs"]
mod graph_config_behavior;

#[path = "contracts/peer_group_binding_schema.rs"]
mod peer_group_binding_schema;

#[path = "contracts/mesh_peer_policy_schema.rs"]
mod mesh_peer_policy_schema;

#[path = "contracts/tailscale_local_schema.rs"]
mod tailscale_local_schema;

#[path = "contracts/auto_enroll_verify_gate_coverage.rs"]
mod auto_enroll_verify_gate_coverage;

#[path = "contracts/cli_help_completeness.rs"]
mod cli_help_completeness;

#[path = "contracts/audit_event_coverage.rs"]
mod audit_event_coverage;

#[path = "contracts/migrate_command_surface.rs"]
mod migrate_command_surface;

#[path = "contracts/backup_import_roundtrip.rs"]
mod backup_import_roundtrip;

#[path = "contracts/handoff_canonical_schema.rs"]
mod handoff_canonical_schema;

#[path = "contracts/handoff_resume_prompt_fragment.rs"]
mod handoff_resume_prompt_fragment;

#[path = "contracts/workspace_fingerprint_reconciliation.rs"]
mod workspace_fingerprint_reconciliation;

#[path = "contracts/handoff_capsule_roundtrip_determinism.rs"]
mod handoff_capsule_roundtrip_determinism;

#[path = "contracts/context_show_persisted_pack.rs"]
mod context_show_persisted_pack;

#[path = "contracts/c4_rejection_error_details.rs"]
mod c4_rejection_error_details;

#[path = "contracts/handoff_stale_snapshot.rs"]
mod handoff_stale_snapshot;

#[path = "contracts/n15_logical_id_foundation.rs"]
mod n15_logical_id_foundation;

#[path = "contracts/n15_memory_revise_write_path.rs"]
mod n15_memory_revise_write_path;

#[path = "contracts/n15_lab_capture_wal_hold.rs"]
mod n15_lab_capture_wal_hold;

#[path = "contracts/failure_mode_fixtures.rs"]
mod failure_mode_fixtures;

#[path = "contracts/failure_mode_repair_string.rs"]
mod failure_mode_repair_string;

#[path = "contracts/repair_safety_conformance.rs"]
mod repair_safety_conformance;

#[path = "contracts/br_concurrent_read_race.rs"]
mod br_concurrent_read_race;

#[path = "contracts/read_pool_status_schema.rs"]
mod read_pool_status_schema;

#[path = "contracts/mesh_command_modes_contract.rs"]
mod mesh_command_modes_contract;

#[path = "contracts/workspace_git_snapshot_read_only.rs"]
mod workspace_git_snapshot_read_only;

#[path = "contracts/workspace_secret_risk_no_leak.rs"]
mod workspace_secret_risk_no_leak;

#[path = "contracts/hygiene_reason_code_vocabulary.rs"]
mod hygiene_reason_code_vocabulary;

#[path = "contracts/workspace_hygiene_classifier_matrix.rs"]
mod workspace_hygiene_classifier_matrix;

#[path = "contracts/repo_hygiene_root_clutter.rs"]
mod repo_hygiene_root_clutter;

#[path = "contracts/mesh_surrogate_schema.rs"]
mod mesh_surrogate_schema;

#[path = "contracts/mesh_anti_entropy_schema.rs"]
mod mesh_anti_entropy_schema;

#[path = "contracts/curate_peer_evidence_schema.rs"]
mod curate_peer_evidence_schema;

#[path = "contracts/why_not_selected_schema.rs"]
mod why_not_selected_schema;

#[path = "contracts/host_calibration_recommendation_schema.rs"]
mod host_calibration_recommendation_schema;

#[path = "contracts/host_calibration_freshness_contract.rs"]
mod host_calibration_freshness_contract;

#[path = "contracts/agent_workload_trace_schema.rs"]
mod agent_workload_trace_schema;

#[path = "contracts/agent_workload_replay_schema.rs"]
mod agent_workload_replay_schema;

#[path = "contracts/qos_lanes_e2e_contract.rs"]
mod qos_lanes_e2e_contract;

#[path = "contracts/spec_pack_schema.rs"]
mod spec_pack_schema;

#[path = "contracts/prompt_budget_report.rs"]
mod prompt_budget_report;

#[path = "contracts/doctor_undo_replay_e2e.rs"]
mod doctor_undo_replay_e2e;

#[path = "contracts/flight_recorder_e2e.rs"]
mod flight_recorder_e2e;

#[path = "contracts/mesh_lane_grant_preview_schema.rs"]
mod mesh_lane_grant_preview_schema;

#[path = "contracts/mesh_disable_revoke_schemas.rs"]
mod mesh_disable_revoke_schemas;

#[path = "contracts/mesh_auto_status_schema.rs"]
mod mesh_auto_status_schema;

#[path = "contracts/closeout_audit_schema.rs"]
mod closeout_audit_schema;

#[path = "contracts/mcp_parity_required_coverage.rs"]
mod mcp_parity_required_coverage;

#[path = "contracts/graph_hits_perf_budget.rs"]
mod graph_hits_perf_budget;

#[path = "contracts/mesh_off_no_network_e2e.rs"]
mod mesh_off_no_network_e2e;

#[path = "contracts/perf_live_schema.rs"]
mod perf_live_schema;

#[path = "contracts/tracing_paragraph_required.rs"]
mod tracing_paragraph_required;

#[path = "contracts/context_delta_schema_v1.rs"]
mod context_delta_schema_v1;

#[path = "contracts/search_document_schema_v1.rs"]
mod search_document_schema_v1;

#[path = "contracts/context_delta_prior_unknown_repair_pinned.rs"]
mod context_delta_prior_unknown_repair_pinned;

#[path = "contracts/mesh_serve_mcp_degraded_code_catalog.rs"]
mod mesh_serve_mcp_degraded_code_catalog;

#[path = "contracts/mesh_serve_mcp_envelope_mirror_guard.rs"]
mod mesh_serve_mcp_envelope_mirror_guard;

#[path = "contracts/pack_stream_conformance_v1.rs"]
mod pack_stream_conformance_v1;

#[path = "contracts/pack_quality_report_conformance_v1.rs"]
mod pack_quality_report_conformance_v1;

#[path = "contracts/graph_surfaces_conformance_v1.rs"]
mod graph_surfaces_conformance_v1;

#[path = "contracts/swarm_brief_conformance_v1.rs"]
mod swarm_brief_conformance_v1;

#[path = "contracts/curate_outcome_audit_schema_contract.rs"]
mod curate_outcome_audit_schema_contract;

#[path = "contracts/obs_log_envelope_schema_contract.rs"]
mod obs_log_envelope_schema_contract;
