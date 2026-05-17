# Failure-mode fixture catalog

Catalog of degraded-emission fixtures. Each `*.json` documents one
`degraded[]` code with its surface, severity, trigger scenario, and
expected emission shape.

See [`SCHEMA.md`](./SCHEMA.md) for the fixture format. The contract test
at [`tests/contracts/failure_mode_fixtures.rs`](../../contracts/failure_mode_fixtures.rs)
walks this directory and asserts every fixture is well-formed and that
every documented `code` corresponds to a real string in `src/`.

## Seed catalog (J6 / bd-17c65.10.6)

These are the highest-emission codes the system raises today. Per-epic
PRs that introduce new codes are expected to land their own fixture here
in the same commit, keeping the catalog complete by construction.

| Code | Surface | Severity | Bead |
|---|---|---|---|
| `no_relevant_results` | search | medium | bd-17c65.2.1 (B1) |
| `weak_query_recall` | search | low | bd-17c65.2.5 (B5) |
| `lexical_unavailable` | search | warning | bd-17c65.2.6 (B6) |
| `source_mode_fallback` | search | warning | bd-17c65.2.6 (B6) |
| `low_recall_after_floor` | search | info | bd-17c65.2.1 (B1) |
| `duplicates_collapsed` | search | low | bd-17c65.2.3 (B3) |
| `index_stale` | index status | high | bd-17c65.2.1 (B1) |
| `search_index_stale` | search, context | medium | bd-17c65.2.1 (B1) |
| `index_missing` | search, context | medium | bd-17c65.2.1 (B1) |
| `index_corrupt` | search, context | high | bd-17c65.2.1 (B1) |
| `tombstoned_in_results` | search | low | bd-17c65.2.8 (B8) |
| `tombstoned_filtered` | search | low | bd-17c65.2.8 (B8) |
| `expired_filtered` | search | low | bd-17c65.2.8 (B8) |
| `future_validity_filtered` | search | low | bd-17c65.10.6 (J6) |
| `stale_validity_filtered` | search | low | bd-17c65.10.6 (J6) |
| `malformed_validity_filtered` | search | medium | bd-17c65.10.6 (J6) |
| `validity_filtered_significant_recall_drop` | search, context | info | bd-17c65.10.6 (J6) |
| `output_redaction_disabled` | search, context | info | bd-17c65.2.9 (B10) |
| `redaction_level_invalid` | export, handoff create, context, support bundle | low | bd-17c65.11.6 (K6) |
| `redaction_pattern_matched` | export, handoff create, context, support bundle | medium | bd-17c65.11.6 (K6) |
| `redaction_round_trip_marker_preserved` | import jsonl | info | bd-17c65.11.6 (K6) |
| `handoff_snapshot_stale` | handoff resume | medium | bd-17c65.13.5 (M4) |
| `profile_search_limit_capped` | search, diag search | low | bd-17c65.2.4 (B7) |
| `context_profile_budget_capped` | context | low | bd-17c65.10.6 (J6) |
| `context_evidence_freshness_changed_source` | context | low | bd-17c65.1.2 (A2) |
| `policy_bypass_used` | remember, note | info | bd-17c65.3.2 (C2) |
| `policy_tag_rejected_with_details` | remember, note | low | bd-17c65.3.4 (C4) |
| `policy_secret_detected_with_offsets` | remember, note | medium | bd-17c65.3.4 (C4) |
| `runtime_unavailable` | status, doctor | high | bd-17c65.10.6 (J6) |
| `storage_unavailable` | status, dependency contract | high | bd-17c65.10.6 (J6) |
| `search_unavailable` | status, dependency contract | medium | bd-17c65.10.6 (J6) |
| `graph_unavailable` | diag dependencies, dependency contract | medium | bd-17c65.10.6 (J6) |
| `cass_unavailable` | doctor, import cass | medium | bd-17c65.10.6 (J6) |
| `toon_unavailable` | status, doctor | medium | bd-17c65.10.6 (J6) |
| `diagram_backend_unavailable` | diag dependencies, dependency contract | medium | bd-17c65.10.6 (J6) |
| `agent_detection_unavailable` | agent status, doctor | medium | bd-17c65.10.6 (J6) |
| `mcp_unavailable` | diag dependencies, dependency contract | medium | bd-17c65.10.6 (J6) |
| `storage_not_inspected` | status | low | bd-17c65.10.6 (J6) |
| `storage_not_initialized` | status | medium | bd-17c65.10.6 (J6) |
| `storage_degraded` | status, health | medium | bd-17c65.10.6 (J6) |
| `storage_not_ready` | health | medium | bd-17c65.10.6 (J6) |
| `storage_unimplemented` | status | high | bd-17c65.10.6 (J6) |
| `snapshot_pin_expired` | context, status | medium | bd-2caru.6 |
| `snapshot_release_failed` | context, status | medium | bd-2caru.6 |
| `snapshot_pin_force_released` | status, workspace close | medium | bd-2caru.6 |
| `search_not_inspected` | status | low | bd-17c65.10.6 (J6) |
| `search_waiting_for_storage` | status | medium | bd-17c65.10.6 (J6) |
| `search_index_degraded` | status, health | medium | bd-17c65.10.6 (J6) |
| `search_not_ready` | health | medium | bd-17c65.10.6 (J6) |
| `search_unimplemented` | status | high | bd-17c65.10.6 (J6) |
| `memory_health_unavailable` | status | low | bd-17c65.10.6 (J6) |
| `curation_health_unavailable` | status | low | bd-17c65.10.6 (J6) |
| `curation_ttl_policy_unavailable` | status | medium | bd-17c65.10.6 (J6) |
| `feedback_health_unavailable` | status | low | bd-17c65.10.6 (J6) |
| `feedback_quarantine_unavailable` | status | medium | bd-17c65.10.6 (J6) |
| `feedback_protected_rules_unavailable` | status | medium | bd-17c65.10.6 (J6) |
| `deprecated_alias` | remember, context | low | bd-17c65.10.6 (J6) |
| `usage_unknown_field` | global fields | low | bd-17c65.4.5 (D5) |
| `usage_conflicting_presets` | global fields | low | bd-17c65.4.5 (D5) |
| `auto_propose_skipped_too_few_neighbors` | remember | info | bd-17c65.7.3 (G3) |
| `auto_propose_search_neighbor_lookup_failed` | remember | info | bd-17c65.7.3 (G3) |
| `auto_propose_skipped_existing_rule_covers` | remember | info | bd-17c65.7.3 (G3) |
| `auto_propose_deferred_to_maintenance` | remember | info | bd-17c65.7.3 (G3) |
| `auto_propose_failed` | remember | low | bd-17c65.7.3 (G3) |
| `auto_link_disabled` | remember | info | bd-17c65.7.6 (G7) |
| `remember_auto_link_failed` | remember | low | bd-17c65.7.3 (G3) |
| `remember_link_suggestion_failed` | remember | low | bd-17c65.7.3 (G3) |
| `cass_evidence_not_available` | review workspace | low | bd-17c65.7.4 (G4) |
| `curation_ttl_policy_missing` | curate disposition | medium | bd-17c65.7.4 (G4) |
| `curation_harmful_candidate_escalated` | curate disposition, status | high | bd-17c65.7.4 (G4) |
| `curation_ttl_blocked` | status, curate disposition | medium | bd-17c65.7.4 (G4) |
| `level_transition_tombstoned_rejected` | workflow close, curate apply, curate tombstone | medium | bd-17c65.7.8 (G9) |
| `level_transition_requires_evidence` | curate apply, workflow close | medium | bd-17c65.7.8 (G9) |
| `level_transition_concurrent_conflict` | workflow close, job run | medium | bd-17c65.7.8 (G9) |
| `verification_evidence_not_found` | why | low | bd-1zb7k.3 (S2) |
| `proof_tool_missing` | verify proofs | info | bd-nnfq4 (SRR2) |
| `proof_violation_detected` | verify proofs | high | bd-nnfq4 (SRR2) |
| `consensus_no_clusters` | context | low | bd-1zb7k.9 (S8) |
| `conflict_direct` | context | medium | bd-1zb7k.9 (S8) |
| `conflict_trust_mismatch` | context | high | bd-1zb7k.9 (S8) |
| `coordination_source_stale` | context, pack | low | bd-1zb7k.4 (S3) |
| `coordination_source_unavailable` | context, pack | medium | bd-1zb7k.4 (S3) |
| `qos_registry_unavailable` | qos registry, status, doctor, swarm brief | medium | bd-1zb7k.20.2 |
| `why_pack_selection_unavailable` | why | low | bd-17c65.10.6 (J6) |
| `why_result_target_unsupported_source` | why | medium | bd-17c65.10.6 (J6) |
| `graph_memory_not_in_snapshot` | why | low | bd-17c65.10.6 (J6) |
| `graph_query_relative_features_unavailable` | why | low | bd-17c65.10.6 (J6) |
| `db_migration_pending` | db status | medium | bd-3usjw.1 |
| `db_wal_stale` | db status | medium | bd-3usjw.1 |
| `no_risk_memories` | preflight check | info | bd-3usjw.6 |
| `preflight_evidence_unavailable` | preflight | medium | bd-17c65.10.6 (J6) |
| `preflight_evidence_stale` | preflight | warning | bd-17c65.10.6 (J6) |
| `preflight_patterns_unavailable` | preflight check | medium | bd-3usjw.6 |
| `quarantine_workspace_unavailable` | quarantine, status | medium | bd-17c65.10.6 (J6) |
| `quarantine_database_missing` | quarantine, status | medium | bd-17c65.10.6 (J6) |
| `quarantine_database_unreadable` | quarantine, status | medium | bd-17c65.10.6 (J6) |
| `quarantine_feedback_events_unreadable` | quarantine, status | medium | bd-17c65.10.6 (J6) |
| `quarantine_rows_unreadable` | quarantine, status | medium | bd-17c65.10.6 (J6) |
| `trust_quarantine_rows_unreadable` | quarantine, status | medium | bd-17c65.10.6 (J6) |
| `model_registry_empty` | model status, model list | low | bd-17c65.10.6 (J6) |
| `model_registry_no_available_entry` | model status, model list | medium | bd-17c65.10.6 (J6) |
| `heavy_gates_skipped` | profile config plan | info | bd-17c65.10.6 (J6) |
| `manual_heavy_strategy` | profile config plan | warning | bd-17c65.10.6 (J6) |
| `index_locked` | index vacuum | medium | bd-17c65.10.6 (J6) |
| `integrity_database_missing` | diag integrity | medium | bd-17c65.10.6 (J6) |
| `integrity_database_open_failed` | diag integrity | high | bd-17c65.10.6 (J6) |
| `integrity_reference_issues` | diag integrity | medium | bd-17c65.10.6 (J6) |
| `integrity_reference_check_unavailable` | diag integrity | medium | bd-17c65.10.6 (J6) |
| `integrity_schema_migration_required` | diag integrity | medium | bd-17c65.10.6 (J6) |
| `integrity_schema_check_unavailable` | diag integrity | medium | bd-17c65.10.6 (J6) |
| `integrity_provenance_sample_unavailable` | diag integrity | medium | bd-17c65.10.6 (J6) |
| `tripwire_inputs_incomplete` | tripwire check, preflight | warning | bd-17c65.10.6 (J6) |
| `unsupported_condition` | tripwire check, preflight | warning | bd-17c65.10.6 (J6) |
| `tombstone_visibility_unavailable` | search | medium | bd-17c65.10.6 (J6) |
| `semantic_dimension_exceeds_budget` | semantic model admissibility, search | medium | bd-17c65.10.6 (J6) |
| `unsupported_schema` | import jsonl, perf compare | high | bd-17c65.10.6 (J6) |
| `stale_schema_version` | certificate verify, perf compare | medium | bd-17c65.10.6 (J6) |
| `tampered_hash` | perf compare, certificate verify | high | bd-17c65.10.6 (J6) |
| `unsupported_artifact_kind` | perf compare | high | bd-17c65.10.6 (J6) |
| `redaction_uncertain` | perf compare, support bundle | medium | bd-17c65.10.6 (J6) |
| `profile_mismatch` | perf compare, perf budget check | medium | bd-17c65.10.6 (J6) |
| `profile_missing` | perf budget check | medium | bd-17c65.10.6 (J6) |
| `missing_metric` | perf artifact summary, perf budget check | medium | bd-17c65.10.6 (J6) |
| `metric_missing` | perf compare | medium | bd-17c65.10.6 (J6) |
| `fixture_tier_mismatch` | perf compare | medium | bd-17c65.10.6 (J6) |
| `no_filters` | causal commands | warning | bd-17c65.10.6 (J6) |
| `no_sources` | causal compare | warning | bd-17c65.10.6 (J6) |
| `causal_sample_underpowered` | causal estimate, causal promote-plan | warning | bd-17c65.10.6 (J6) |
| `causal_confounders_unavailable` | causal estimate | warning | bd-17c65.10.6 (J6) |
| `causal_comparison_evidence_unavailable` | causal compare | warning | bd-17c65.10.6 (J6) |
| `unknown_method` | causal compare, causal promote-plan | warning | bd-17c65.10.6 (J6) |
| `stable_unit` | causal estimate | info | bd-17c65.10.6 (J6) |
| `no_confounders` | causal estimate | info | bd-17c65.10.6 (J6) |
| `conditional_independence` | causal estimate | info | bd-17c65.10.6 (J6) |
| `replay_fidelity` | causal estimate | info | bd-17c65.10.6 (J6) |
| `proper_randomization` | causal estimate | info | bd-17c65.10.6 (J6) |
| `advisory_memory` | context | medium | bd-17c65.10.6 (J6) |
| `legacy_memory` | context | high | bd-17c65.10.6 (J6) |
| `degraded_context` | context | info | bd-17c65.5.2 (E2, retired tombstone) |
| `daemon_background_mode_unimplemented` | daemon | low | bd-17c65.10.6 (J6) |
| `decay_sweep_database_unresolved` | job run | medium | bd-17c65.10.6 (J6) |
| `decay_sweep_database_missing` | job run | medium | bd-17c65.10.6 (J6) |
| `decay_sweep_database_open_failed` | job run | high | bd-17c65.10.6 (J6) |
| `decay_sweep_migration_failed` | job run | high | bd-17c65.10.6 (J6) |
| `decay_sweep_workspace_unresolved` | job run | medium | bd-17c65.10.6 (J6) |
| `decay_sweep_item_limit_too_large` | job run | medium | bd-17c65.10.6 (J6) |
| `decay_sweep_handler_failed` | job run | high | bd-17c65.10.6 (J6) |
| `learn_decay_config_invalid` | maintenance run | medium | bd-17c65.10.6 (J6) |
| `learn_decay_config_read_failed` | maintenance run | medium | bd-17c65.10.6 (J6) |
| `graph_feature_disabled` | graph, graph feature-enrichment | medium | bd-17c65.10.6 (J6) |
| `graph_algorithm_unavailable` | graph centrality | medium | bd-3usjw.2 |
| `graph_snapshot_missing` | graph export, graph feature-enrichment | medium | bd-17c65.10.6 (J6) |
| `graph_snapshot_stale` | graph export, graph feature-enrichment | medium | bd-17c65.10.6 (J6) |
| `graph_snapshot_unusable` | graph export, graph feature-enrichment | medium | bd-17c65.10.6 (J6) |
| `graph_snapshot_topology_unavailable` | graph export | medium | bd-17c65.10.6 (J6) |
| `graph_ppr_snapshot_stale` | context | medium | bd-bife.6 |
| `graph_ppr_empty_seed_set` | context | low | bd-bife.6 |
| `graph_pack_dna_no_dominator` | context | low | bd-bife.6 |
| `graph_causal_no_evidence` | why | low | bd-bife.6 |
| `graph_health_no_contradictions` | health, insights | info | bd-bife.6 |
| `graph_curate_disconnected_graph` | curate | warning | bd-bife.6 |
| `graph_proximity_unreachable` | proximity | info | bd-bife.6 |
| `graph_dominance_no_revision_chain` | memory revise | info | bd-bife.6 |
| `graph_skyline_degenerate_communities` | status, insights | info | bd-bife.6 |
| `graph_hits_convergence_failure` | context, why | warning | bd-bife.6 |
| `maintenance_job_history_write_failed` | job run | high | bd-17c65.10.6 (J6) |
| `maintenance_job_since_invalid` | job list | medium | bd-17c65.10.6 (J6) |
| `maintenance_job_history_read_failed` | job list, job show | medium | bd-17c65.10.6 (J6) |
| `maintenance_job_not_found` | job show | medium | bd-17c65.10.6 (J6) |
| `mcp_feature_disabled` | mcp manifest | low | bd-17c65.10.6 (J6) |
| `workspace_nested_markers` | workspace discovery, status | warning | bd-17c65.10.6 (J6) |
| `causal_ledger_empty` | causal estimate, causal compare | warning | bd-17c65.10.6 (J6) |
| `causal_evidence_unavailable` | causal commands | warning | bd-17c65.10.6 (J6) |
| `causal_workspace_id_required` | causal trace | warning | bd-17c65.10.6 (J6) |
| `causal_database_missing` | causal trace | warning | bd-17c65.10.6 (J6) |
| `causal_database_open_failed` | causal trace | warning | bd-17c65.10.6 (J6) |
| `causal_database_migration_failed` | causal trace | warning | bd-17c65.10.6 (J6) |
| `causal_trace_store_failed` | causal trace | warning | bd-17c65.10.6 (J6) |
| `causal_failure_id_required` | causal trace | warning | bd-17c65.10.6 (J6) |
| `causal_evidence_table_missing` | causal commands | warning | bd-17c65.10.6 (J6) |
| `causal_chain_id_required` | causal estimate, causal promote-plan | warning | bd-17c65.10.6 (J6) |
| `causal_chain_not_found` | causal estimate, causal compare | warning | bd-17c65.10.6 (J6) |
| `causal_no_matching_chains` | causal estimate | info | bd-17c65.10.6 (J6) |
| `causal_chain_pair_required` | causal compare | warning | bd-17c65.10.6 (J6) |
| `causal_insufficient_chains` | causal compare | info | bd-17c65.10.6 (J6) |
| `action_override_not_actionable` | causal promote-plan | warning | bd-17c65.10.6 (J6) |
| `dry_run_recommended` | causal promote-plan | info | bd-17c65.10.6 (J6) |
| `graph_snapshot_scores_unavailable` | graph feature-enrichment | medium | bd-17c65.10.6 (J6) |
| `index_publish_lock_contention` | index rebuild, index publish | medium | bd-17c65.10.6 (J6) |
| `lab_replay_unavailable` | lab capture, lab replay, lab counterfactual | medium | bd-17c65.10.6 (J6) |
| `git_unavailable` | swarm brief | warning | bd-17c65.10.6 (J6) |
| `beads_unavailable` | swarm brief | warning | bd-17c65.10.6 (J6) |
| `beads_tracker_stale` | swarm brief | warning | bd-1zb7k.13.3 (C3) |
| `bv_unavailable` | swarm brief | warning | bd-17c65.10.6 (J6) |
| `agent_mail_unavailable` | swarm brief | warning | bd-17c65.10.6 (J6) |
| `rch_remote_required_fallback_prevented` | swarm brief | warning | bd-1zb7k.13.4 (C4) |
| `rch_unavailable` | swarm brief | warning | bd-17c65.10.6 (J6) |
| `rch_worker_topology_blocked` | swarm brief | warning | bd-1zb7k.13.4 (C4) |
| `agent_status_unavailable` | swarm brief | warning | bd-17c65.10.6 (J6) |
| `singleflight_follower_timeout` | graph feature-enrichment | medium | bd-gni47.3 (SF) |
| `singleflight_leader_failed` | graph feature-enrichment | medium | bd-gni47.3 (SF) |
| `singleflight_state_poisoned` | graph feature-enrichment | high | bd-gni47.3 (SF) |
| `swarm_scale_budget_exceeded` | swarm-scale benchmark | warning | bd-1zb7k.8 (S7) |
| `swarm_scale_nondeterminism` | swarm-scale benchmark | high | bd-1zb7k.8 (S7) |
| `write_owner_busy` | write owner | medium | bd-17c65.10.6 (J6) |
| `write_spool_backpressure` | write spool | medium | bd-17c65.10.6 (J6) |
| `write_queue_full` | write spool | low | bd-17c65.12.2 (L1) |
| `write_hot_path_cancelled_before_commit` | write hot path fake runner | medium | bd-2lsxf.2.4 (SRR3) |
| `write_hot_path_fsync_failure` | write hot path fake runner | high | bd-2lsxf.2.4 (SRR3) |
| `situation_decisioning_unavailable` | situation classify | warning | bd-17c65.10.6 (J6) |
| `clustering_insufficient_data` | learn cluster, curate candidates | warning | bd-17c65.10.6 (J6) |
| `clustering_threshold_too_strict` | learn cluster, curate candidates | warning | bd-17c65.10.6 (J6) |
| `science_not_compiled` | science status, analyze drift, analyze clustering | high | bd-17c65.10.6 (J6) |
| `science_backend_unavailable` | science status | high | bd-17c65.10.6 (J6) |
| `science_input_too_large` | science analytics | medium | bd-17c65.10.6 (J6) |
| `science_budget_exceeded` | science analytics | medium | bd-17c65.10.6 (J6) |
| `scope_agent_unavailable` | search, context | warning | bd-17c65.10.6 (J6) |
| `scope_excluded_evidence` | search, context | low | bd-17c65.10.6 (J6) |
| `scope_metadata_unavailable` | search, context | medium | bd-17c65.10.6 (J6) |
| `scope_strict_excluded_evidence` | search, context | medium | bd-17c65.10.6 (J6) |
| `drift_analysis_unavailable` | analyze drift | high | bd-17c65.10.6 (J6) |
| `drift_no_evaluation_snapshots` | analyze drift | high | bd-17c65.10.6 (J6) |
| `drift_no_comparable_metrics` | analyze drift | medium | bd-17c65.10.6 (J6) |
| `clustering_no_candidates` | analyze clustering | medium | bd-17c65.10.6 (J6) |
| `clustering_no_embeddings` | analyze clustering | medium | bd-17c65.10.6 (J6) |
| `maintenance_job_cancelled` | job run | medium | bd-17c65.10.6 (J6) |
| `maintenance_job_timed_out` | job run | medium | bd-17c65.10.6 (J6) |
| `maintenance_job_failed` | job run | medium | bd-17c65.10.6 (J6) |
| `maintenance_job_skipped` | job run | medium | bd-17c65.10.6 (J6) |
| `maintenance_job_lock_busy` | job run | medium | bd-17c65.10.6 (J6) |
| `maintenance_job_lock_open_failed` | job run | high | bd-17c65.10.6 (J6) |

## Adding a fixture

1. Implement the new degraded emission in `src/`.
2. Drop a fixture here named `<code>.json` matching the schema.
3. `cargo test --test contracts failure_mode_fixtures_validate_catalog`
   to confirm structural validity + cross-reference against `src/`.
4. Add a row to the table above.

## What this catalog is NOT

* It is not a replacement for end-to-end exercise of each degraded
  emission. Per-epic e2e drivers under
  `scripts/e2e_overhaul/` (search_honesty.sh, etc.) and the J6 catalog
  driver at `scripts/e2e_overhaul/failure_modes.sh` run the real binary
  and assert each code fires when expected. The catalog is the static
  reference; the e2e drivers are the executable proof.
* It is not authoritative for message text. Production messages embed
  runtime values (floor, query, counts). Fixtures use
  `message_contains` substrings to stay robust under templating.

Bead: `bd-17c65.10.6` (J6).
