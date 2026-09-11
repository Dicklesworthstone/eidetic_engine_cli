//! Integration modules G–M. Filter with `cargo test --test integration_g_m <module>::`.

#[path = "../g4_health_structural_e2e.rs"]
mod g4_health_structural_e2e;
#[path = "../g5_curate_decay_e2e.rs"]
mod g5_curate_decay_e2e;
#[path = "../gap_honesty_contracts.rs"]
mod gap_honesty_contracts;
#[path = "../gates.rs"]
mod gates;
#[path = "../global_store_e2e.rs"]
mod global_store_e2e;
#[path = "../golden.rs"]
mod golden;
#[path = "../graph_determinism.rs"]
mod graph_determinism;
#[path = "../graph_migrations.rs"]
mod graph_migrations;
#[path = "../graph_neighborhood_smoke.rs"]
mod graph_neighborhood_smoke;
#[path = "../graph_run_with_budget_cancellation.rs"]
mod graph_run_with_budget_cancellation;
#[path = "../graph_scale_degradation.rs"]
mod graph_scale_degradation;
#[path = "../graph_telemetry_integration.rs"]
mod graph_telemetry_integration;
#[path = "../graph_test_policy.rs"]
mod graph_test_policy;
#[path = "../handoff_capsule_roundtrip.rs"]
mod handoff_capsule_roundtrip;
#[path = "../handoff_no_mocks_e2e.rs"]
mod handoff_no_mocks_e2e;
#[path = "../harness_conformance_golden.rs"]
mod harness_conformance_golden;
#[path = "../harness_conformance_schema_unit.rs"]
mod harness_conformance_schema_unit;
#[path = "../harness_conformance_simulator.rs"]
mod harness_conformance_simulator;
#[path = "../hotset_manifest_contract.rs"]
mod hotset_manifest_contract;
#[path = "../hotset_prewarm_e2e.rs"]
mod hotset_prewarm_e2e;
#[path = "../index_vacuum_e2e.rs"]
mod index_vacuum_e2e;
#[path = "../influence_function_why.rs"]
mod influence_function_why;
#[path = "../install_freshness_contract.rs"]
mod install_freshness_contract;
#[path = "../install_path_smoke.rs"]
mod install_path_smoke;
#[path = "../install_workflows.rs"]
mod install_workflows;
#[path = "../journal_capture_contract.rs"]
mod journal_capture_contract;
#[path = "../journal_capture_property.rs"]
mod journal_capture_property;
#[path = "../l2_pack_cache_perf_gate.rs"]
mod l2_pack_cache_perf_gate;
#[path = "../lexical_ram_tier_p99_proof_self_test.rs"]
mod lexical_ram_tier_p99_proof_self_test;
#[path = "../lifecycle_docs.rs"]
mod lifecycle_docs;
#[path = "../lod_packing_e2e.rs"]
mod lod_packing_e2e;
#[path = "../log_envelope_contract.rs"]
mod log_envelope_contract;
#[path = "../markdown_mermaid_share_renderer_unit.rs"]
mod markdown_mermaid_share_renderer_unit;
#[path = "../markdown_render_roundtrip.rs"]
mod markdown_render_roundtrip;
#[path = "../mcp_graph_tools.rs"]
mod mcp_graph_tools;
#[path = "../mcp_parity_coverage.rs"]
mod mcp_parity_coverage;
#[path = "../mechanical_boundary_inventory.rs"]
mod mechanical_boundary_inventory;
#[path = "../memory_composition_scenario.rs"]
mod memory_composition_scenario;
#[path = "../memory_debt_curate_doctor.rs"]
mod memory_debt_curate_doctor;
#[path = "../memory_drift_no_mock_e2e.rs"]
mod memory_drift_no_mock_e2e;
#[path = "../memory_expire_tags_e2e.rs"]
mod memory_expire_tags_e2e;
#[path = "../memory_level_lifecycle_unit.rs"]
mod memory_level_lifecycle_unit;
#[path = "../memory_link_e2e.rs"]
mod memory_link_e2e;
#[path = "../memory_seal_e2e.rs"]
mod memory_seal_e2e;
#[path = "../memory_sentinel_contract.rs"]
mod memory_sentinel_contract;
#[path = "../mesh_anti_entropy_model.rs"]
mod mesh_anti_entropy_model;
#[path = "../mesh_anti_entropy_protocol.rs"]
mod mesh_anti_entropy_protocol;
#[path = "../mesh_audit_forensics.rs"]
mod mesh_audit_forensics;
#[path = "../mesh_auto_enrollment_safety_audit.rs"]
mod mesh_auto_enrollment_safety_audit;
#[path = "../mesh_auto_status_golden_e2e.rs"]
mod mesh_auto_status_golden_e2e;
#[path = "../mesh_cache.rs"]
mod mesh_cache;
#[path = "../mesh_command_modes.rs"]
mod mesh_command_modes;
#[path = "../mesh_discovery_policy_audit.rs"]
mod mesh_discovery_policy_audit;
#[path = "../mesh_e2e_logging_contract.rs"]
mod mesh_e2e_logging_contract;
#[path = "../mesh_emergency_disable_e2e.rs"]
mod mesh_emergency_disable_e2e;
#[path = "../mesh_event_schema_contract.rs"]
mod mesh_event_schema_contract;
#[path = "../mesh_foreground_cli.rs"]
mod mesh_foreground_cli;
#[path = "../mesh_hello_responder_audit.rs"]
mod mesh_hello_responder_audit;
#[path = "../mesh_identity_change_guard_audit.rs"]
mod mesh_identity_change_guard_audit;
#[path = "../mesh_key_store.rs"]
mod mesh_key_store;
#[path = "../mesh_local_two_node_demo.rs"]
mod mesh_local_two_node_demo;
#[path = "../mesh_off_no_network.rs"]
mod mesh_off_no_network;
#[path = "../mesh_peer_enrollment.rs"]
mod mesh_peer_enrollment;
#[path = "../mesh_peer_freshness.rs"]
mod mesh_peer_freshness;
#[path = "../mesh_privacy_redaction_authorization.rs"]
mod mesh_privacy_redaction_authorization;
#[path = "../mesh_remote_evidence.rs"]
mod mesh_remote_evidence;
#[path = "../mesh_replay_convergence.rs"]
mod mesh_replay_convergence;
#[path = "../mesh_status_golden_e2e.rs"]
mod mesh_status_golden_e2e;
#[path = "../mesh_surrogate_audit.rs"]
mod mesh_surrogate_audit;
#[path = "../mesh_sync_once_real_tailscale_self_test.rs"]
mod mesh_sync_once_real_tailscale_self_test;
#[path = "../mesh_tailnet_change_audit.rs"]
mod mesh_tailnet_change_audit;
#[path = "../mesh_two_tier_budget.rs"]
mod mesh_two_tier_budget;
#[path = "../mesh_workspace_scope_no_leak.rs"]
mod mesh_workspace_scope_no_leak;
#[path = "../migration_guide.rs"]
mod migration_guide;
#[path = "../migration_no_v1_references.rs"]
mod migration_no_v1_references;
#[path = "../migration_v0_2_unit.rs"]
mod migration_v0_2_unit;
#[path = "../model_lifecycle_readiness_integration.rs"]
mod model_lifecycle_readiness_integration;
#[path = "../model_lifecycle_schema_unit.rs"]
mod model_lifecycle_schema_unit;
#[path = "../model_status_contract.rs"]
mod model_status_contract;
