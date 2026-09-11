//! Integration modules S–Z. Filter with `cargo test --test integration_s_z <module>::`.

#[path = "../sandbox_contracts.rs"]
mod sandbox_contracts;
#[path = "../scale_envelope_collectors.rs"]
mod scale_envelope_collectors;
#[path = "../scale_envelope_locality_advisor.rs"]
mod scale_envelope_locality_advisor;
#[path = "../scale_envelope_slo_harness.rs"]
mod scale_envelope_slo_harness;
#[path = "../schema_compat_v0_v1.rs"]
mod schema_compat_v0_v1;
#[path = "../search_deterministic_golden.rs"]
mod search_deterministic_golden;
#[path = "../search_fts5.rs"]
mod search_fts5;
#[path = "../seed_labels_consistency_test.rs"]
mod seed_labels_consistency_test;
#[path = "../selection_audit_renamed_unit.rs"]
mod selection_audit_renamed_unit;
#[path = "../serve_localhost_schema_docs.rs"]
mod serve_localhost_schema_docs;
#[path = "../session_budget_plan_golden.rs"]
mod session_budget_plan_golden;
#[path = "../session_budget_recorder_unit.rs"]
mod session_budget_recorder_unit;
#[path = "../session_budget_schema_unit.rs"]
mod session_budget_schema_unit;
#[path = "../shard_fanout_closeout_matrix.rs"]
mod shard_fanout_closeout_matrix;
#[path = "../shard_fanout_concurrency.rs"]
mod shard_fanout_concurrency;
#[path = "../silent_fallback_guard.rs"]
mod silent_fallback_guard;
#[path = "../situation_persistence.rs"]
mod situation_persistence;
#[path = "../skill_e2e.rs"]
mod skill_e2e;
#[path = "../skill_standards.rs"]
mod skill_standards;
#[path = "../smoke.rs"]
mod smoke;
#[path = "../source_run_fixture_harness.rs"]
mod source_run_fixture_harness;
#[path = "../spec_pack_unit.rs"]
mod spec_pack_unit;
#[path = "../sprt_quarantine_unit.rs"]
mod sprt_quarantine_unit;
#[path = "../steward_manual_runner_regression.rs"]
mod steward_manual_runner_regression;
#[path = "../store_integrity_surface.rs"]
mod store_integrity_surface;
#[path = "../subscribe_e2e.rs"]
mod subscribe_e2e;
#[path = "../subscribe_error_conformance_e2e.rs"]
mod subscribe_error_conformance_e2e;
#[path = "../subscribe_filter_modes_e2e.rs"]
mod subscribe_filter_modes_e2e;
#[path = "../subscribe_filters_e2e.rs"]
mod subscribe_filters_e2e;
#[path = "../subscribe_unit.rs"]
mod subscribe_unit;
#[path = "../support_logging_smoke.rs"]
mod support_logging_smoke;
#[path = "../swarm_coordination_health_unit.rs"]
mod swarm_coordination_health_unit;
#[path = "../swarm_fixture_corpus_unit.rs"]
mod swarm_fixture_corpus_unit;
#[path = "../swarm_scale_determinism_unit.rs"]
mod swarm_scale_determinism_unit;
#[path = "../swarm_scale_workloads.rs"]
mod swarm_scale_workloads;
#[path = "../swarm_schema_lifecycle.rs"]
mod swarm_schema_lifecycle;
#[path = "../swarm_slo_replay_parser_proptest.rs"]
mod swarm_slo_replay_parser_proptest;
#[path = "../symbol_graph_conformance_e2e.rs"]
mod symbol_graph_conformance_e2e;
#[path = "../symbol_graph_manifest_conformance_e2e.rs"]
mod symbol_graph_manifest_conformance_e2e;
#[path = "../tailscale_local_probe.rs"]
mod tailscale_local_probe;
#[path = "../task_lens_golden.rs"]
mod task_lens_golden;
#[path = "../test_event_schema_gate.rs"]
mod test_event_schema_gate;
#[path = "../tombstone_visibility_unit.rs"]
mod tombstone_visibility_unit;
#[path = "../toolchain_provenance_schema_unit.rs"]
mod toolchain_provenance_schema_unit;
#[path = "../trauma_guard_contracts.rs"]
mod trauma_guard_contracts;
#[path = "../trauma_guard_wired.rs"]
mod trauma_guard_wired;
#[path = "../triad_compat_plan_contract.rs"]
mod triad_compat_plan_contract;
#[path = "../tripwire_eval.rs"]
mod tripwire_eval;
#[path = "../trust_freshness_e2e.rs"]
mod trust_freshness_e2e;
#[path = "../typed_fields_registry.rs"]
mod typed_fields_registry;
#[path = "../unsafe_claim_planner_conformance.rs"]
mod unsafe_claim_planner_conformance;
#[path = "../usr002_pre_task_brief_scenario.rs"]
mod usr002_pre_task_brief_scenario;
#[path = "../usr003_in_task_scenario.rs"]
mod usr003_in_task_scenario;
#[path = "../usr005_degraded_scenario.rs"]
mod usr005_degraded_scenario;
#[path = "../usr006_privacy_redaction_backup_scenario.rs"]
mod usr006_privacy_redaction_backup_scenario;
#[path = "../verification_broker_replay_unit.rs"]
mod verification_broker_replay_unit;
#[path = "../verification_closure_guidance_unit.rs"]
mod verification_closure_guidance_unit;
#[path = "../verification_drift_guard.rs"]
mod verification_drift_guard;
#[path = "../verification_evidence_parsers.rs"]
mod verification_evidence_parsers;
#[path = "../verification_evidence_schema_unit.rs"]
mod verification_evidence_schema_unit;
#[path = "../verification_ingestion_unit.rs"]
mod verification_ingestion_unit;
#[path = "../verification_ledger_lookup_unit.rs"]
mod verification_ledger_lookup_unit;
#[path = "../verify_ledger_fixtures_unit.rs"]
mod verify_ledger_fixtures_unit;
#[path = "../vision_coverage_gate.rs"]
mod vision_coverage_gate;
#[path = "../volatile_field_registry_consistency_test.rs"]
mod volatile_field_registry_consistency_test;
#[path = "../walking_skeleton_acceptance.rs"]
mod walking_skeleton_acceptance;
#[path = "../why_conformance.rs"]
mod why_conformance;
#[path = "../why_not_core_e2e.rs"]
mod why_not_core_e2e;
#[path = "../why_not_counterfactual_hints.rs"]
mod why_not_counterfactual_hints;
#[path = "../why_not_exclusions.rs"]
mod why_not_exclusions;
#[path = "../why_renders_credible_interval_unit.rs"]
mod why_renders_credible_interval_unit;
#[path = "../work_packet_beads_source_health_fixture.rs"]
mod work_packet_beads_source_health_fixture;
#[path = "../work_packet_integrity_golden.rs"]
mod work_packet_integrity_golden;
#[path = "../workspace_hygiene_beads_state_e2e.rs"]
mod workspace_hygiene_beads_state_e2e;
#[path = "../workspace_hygiene_cli_parser.rs"]
mod workspace_hygiene_cli_parser;
#[path = "../workspace_hygiene_coordination_e2e.rs"]
mod workspace_hygiene_coordination_e2e;
#[path = "../workspace_hygiene_logged_e2e.rs"]
mod workspace_hygiene_logged_e2e;
#[path = "../workspace_hygiene_public_emission.rs"]
mod workspace_hygiene_public_emission;
#[path = "../workspace_hygiene_recommendations_e2e.rs"]
mod workspace_hygiene_recommendations_e2e;
#[path = "../write_immune_quarantine.rs"]
mod write_immune_quarantine;
#[path = "../write_owner.rs"]
mod write_owner;
