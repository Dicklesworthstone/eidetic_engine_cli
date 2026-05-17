# Degraded code taxonomy

> **What this file is:** the canonical classification of every `code`
> that can appear in a response `data.degraded[]` array. Each code is
> categorized as `build_time`, `response_time`, or `mixed` so the E5
> bead can move build-time codes to `capabilities.unimplemented[]` and
> the K3 auto-generated catalog can group entries correctly.
>
> **Bead:** [`bd-fptj3`](../README.md) — referenced by E5
> ([bd-17c65.5.5](../README.md)) and K3 ([bd-17c65.11.3](../README.md)).
>
> **How to update:** if you add a new degraded code in `src/`, also add
> a row here. `tests/degraded_code_taxonomy_consistency_test.rs` (when
> it lands) walks `src/` for emission sites and fails CI on orphans.

## Categories

A degraded entry's `code` is classified into one of three buckets:

- **`build_time`** — the emission decision is determined when the
  binary is built. Does NOT vary per call against the same binary.
  Typically tied to a Cargo feature flag. These belong in
  `capabilities.unimplemented[]` (a top-level surface that an agent
  reads ONCE per session) rather than in per-response `degraded[]`
  (which agents must re-parse on every call). Migration target for
  E5 / `bd-17c65.5.5`.

- **`response_time`** — the emission decision depends on workspace
  state, query input, or runtime conditions. Two consecutive calls
  with different inputs against the same binary may produce different
  emissions. These STAY in `degraded[]`.

- **`mixed`** — the emission is gated on BOTH a build-time feature
  flag AND a response-time condition. The presence marker lives in
  `capabilities.available[]`; the response-time aspect lives in
  `degraded[]` when the feature is built. After E5 lands, the
  build-time half is reported once (via capabilities); the
  response-time half stays per-call.

## Categorization rules (canonical)

1. Suffix `_unimplemented` always means `build_time` (the feature
   wasn't compiled in).
2. Suffix `_unavailable` is ambiguous — read the code to determine
   build-time vs response-time. Most are response-time (the resource
   couldn't be located at call time); a few are build-time (the
   binary was compiled without the dependency).
3. Suffix `_not_ready`, `_not_inspected`, `_waiting_for_*` is
   ALWAYS `response_time` (state-dependent initialization signals).
4. Suffix `_degraded` is ALWAYS `response_time` (the subsystem ran
   but couldn't deliver full quality).
5. Suffix `_failed` is ALWAYS `response_time` (a call attempt failed
   at runtime).
6. Suffix `_filtered`, `_collapsed`, `_capped` is ALWAYS
   `response_time` (data-dependent).

## Aggregation source labels

Renderer-level aggregation collapses repeated degraded codes into one entry
with `sources[]` naming the emitters. Source labels are stable snake-case
surface or algorithm names, not user-facing prose.

Current conventions:

| Label | Use |
|-------|-----|
| `insights` | Whole-bundle `ee insights` degraded signals that are not owned by one section. |
| `hubs` | `ee insights --section hubs` HITS profile degradation. |
| `authorities` | `ee insights --section authorities` HITS profile degradation. |
| `causalBottlenecks` | `ee insights --section causalBottlenecks` degradation. |
| `causal_trace` | `ee causal trace` degradation. |
| `causal_estimate` | `ee causal estimate` degradation. |
| `causal_compare` | `ee causal compare` degradation. |
| `causal_promote_plan` | `ee causal promote-plan` degradation. |
| `knowledgeSkyline` | `ee insights --section knowledgeSkyline` degradation. |
| `loadBearingMemories` | `ee insights --section loadBearingMemories` degradation. |
| `revisionFrontiers` | `ee insights --section revisionFrontiers` degradation. |
| `context` | General `ee context` response degradation without a narrower subsystem owner. |
| `pack` | Context pack assembly, advisory, consensus, or conflict degradation. |
| `index_vacuum` | `ee index vacuum` derived-index preview and lock-state degradation. |
| `model_status` | `ee model status` registry posture degradation. |
| `model_list` | `ee model list` registry posture degradation. |
| `pack_dna` | `ee context --explain` Pack DNA graph-explanation degradation. |
| `perf_artifact_summary` | Normalized perf artifact summary degradation. |
| `perf_budget_check` | `ee perf budget check` artifact-budget degradation. |
| `perf_compare` | `ee perf compare` artifact comparison degradation. |
| `pack_coordination` | Context-pack embedded coordination snapshot degradation. |
| `preflight_guard` | `ee preflight check --cmd` command-guard degradation. |
| `preflight_run` | `ee preflight run` risk-evidence degradation. |
| `preflight_show` | `ee preflight show` persisted-run or risk-evidence degradation. |
| `profile_budget_conformance` | `ee perf budget check` profile-budget conformance degradation. |
| `profile_host_probe` | Host profile probe resource-inspection degradation. |
| `profile_verification_recipe` | Profile-derived verification recipe degradation. |
| `playbook_export` | `ee playbook export` portable-rule export degradation. |
| `playbook_extract` | `ee playbook extract` candidate extraction degradation. |
| `playbook_import` | `ee playbook import` portable-rule import degradation. |
| `playbook_list` | `ee playbook list` portable-rule listing degradation. |
| `request` | Request parsing, compatibility, or ignored-query-field degradation. |
| `rule_add` | `ee rule add` procedural-rule creation degradation. |
| `rule_list` | `ee rule list` procedural-rule listing degradation. |
| `rule_mark` | `ee rule mark` lifecycle evidence degradation. |
| `rule_protect` | `ee rule protect` mutation degradation. |
| `rule_show` | `ee rule show` procedural-rule read degradation. |
| `rule_update` | `ee rule update` mutation degradation. |
| `search` | Search, index, recall, filtering, or visibility degradation carried into context output. |
| `status` | Top-level `ee status` posture, capability, or subsystem degradation. |
| `skyline` | `ee status --skyline` structural skyline degradation. |
| `tripwire_check` | `ee tripwire check` deterministic condition-evaluation degradation. |
| `agent_detection` | Agent inventory and agent-status detection degradation. |
| `agent_mail` | Swarm brief Agent Mail source degradation. |
| `artifact_register` | `ee artifact register` artifact metadata, redaction, or indexing degradation. |
| `backup_create` | `ee backup create` export, redaction, index, or graph-cache degradation. |
| `backup_export` | Legacy `ee export` backup JSONL export degradation. |
| `backup_inspect` | `ee backup inspect` manifest or artifact-inspection degradation. |
| `backup_list` | `ee backup list` backup-root or manifest-listing degradation. |
| `backup_manifest` | Persisted backup manifest degradation records. |
| `backup_restore` | `ee backup restore` import, side-path, or derived-asset degradation. |
| `beads` | Swarm brief Beads source degradation. |
| `build` | Binary version and build-provenance degradation. |
| `build_admission` | `ee diag build-admission` disk-pressure and external-build-root admission degradation. |
| `bv` | Swarm brief BV source degradation. |
| `cluster_coherence` | `ee learn cluster` deterministic cluster-coherence degradation. |
| `curate_apply` | `ee curate apply` candidate-application degradation. |
| `curate_candidates` | `ee curate candidates` queue listing degradation. |
| `curate_disposition` | `ee curate disposition` TTL-disposition degradation. |
| `curate_review` | `ee curate accept/reject/snooze/merge` review-lifecycle degradation. |
| `curate_retire` | `ee curate retire` candidate-retirement degradation. |
| `curate_tombstone` | `ee curate tombstone` memory-tombstone degradation. |
| `curate_untombstone` | `ee curate untombstone` memory-restoration degradation. |
| `curate_validate` | `ee curate validate` candidate-validation degradation. |
| `db_status` | `ee db status` migration or sidecar-file degradation. |
| `dependency_contract` | `ee diag dependencies` dependency-contract degradation. |
| `qos_registry` | QoS active-lane registry read or integrity degradation. |
| `economy_prune` | `ee economy prune-plan` memory-economy recommendation degradation. |
| `economy_report` | `ee economy report` memory-economy metric degradation. |
| `economy_score` | `ee economy score` single-artifact economy degradation. |
| `economy_simulation` | `ee economy simulate` attention-budget simulation degradation. |
| `focus` | `ee focus` passive focus-state degradation. |
| `git` | Swarm brief Git source degradation. |
| `graph_centrality_read` | `ee graph centrality` persisted centrality read degradation. |
| `graph_dominance` | `ee why` revision-dominance impact analysis degradation. |
| `graph_export` | `ee graph export` graph snapshot export degradation. |
| `graph_feature_enrichment` | `ee graph feature-enrichment` graph-derived scoring degradation. |
| `hits` | `ee graph hits` HITS algorithm degradation. |
| `gomory_hu_proximity` | `ee proximity` Gomory-Hu min-cut proximity degradation. |
| `review_session` | `ee review session` curation proposal degradation. |
| `review_workspace` | `ee review workspace` curation proposal degradation. |
| `host_profile` | Swarm brief host-profile source degradation. |
| `integrity` | `ee diag integrity` database, schema, canary, or provenance-sample degradation. |
| `lab_counterfactual` | `ee lab counterfactual` replay-evidence degradation. |
| `lab_replay` | `ee lab replay` replay-evidence degradation. |
| `learn_cluster` | `ee learn cluster` deterministic clustering degradation. |
| `quarantine` | `ee diag quarantine` trust or feedback quarantine degradation. |
| `rch` | Swarm brief RCH source degradation. |
| `structural_health` | `ee health structural` graph-health degradation. |
| `science_status` | `ee analyze science-status` availability degradation. |
| `science_drift` | `ee analyze drift` science/evaluation drift degradation. |
| `science_clustering` | `ee analyze clustering` candidate clustering degradation. |
| `situation_classify` | `ee situation classify` deterministic heuristic-routing degradation. |
| `tailscale_status` | Nested `ee status` mesh/Tailscale local-probe degradation. |
| `why` | Top-level `ee why` memory explanation degradation. |
| `why_graph_retrieval` | `ee why` graph-retrieval feature degradation. |
| `why_revision_lineage` | `ee why` revision-lineage sentinel degradation. |

When adding a new renderer, prefer the most specific stable section, command,
or algorithm label available. Do not include workspace paths, query text, or
memory bodies in `sources[]`.

## Full code inventory

> Sources of truth: `tests/fixtures/failure_modes/README.md` for the
> agent-facing catalog AND the union of `pub const *_CODE` constants
> + `"code": "..."` JSON literals in `src/`. When either source gains
> a new code, add a row here in the same commit. The
> `tests/degraded_code_taxonomy_consistency_test.rs` enforces this.

### `build_time` (10 codes — surfaced through `ee capabilities`)

| Code | Surface | Feature flag | Notes |
|------|---------|--------------|-------|
| `agent_detection_unavailable` | agent sources, doctor | (binary detection logic) | Reflects compile-time exclusion of agent-detection paths. |
| `daemon_background_mode_unimplemented` | serve | (daemon background-mode build) | Background daemon mode not built; foreground still works. |
| `diagram_backend_unavailable` | doctor, dependency contract | (mermaid renderer feature) | Mermaid backend not linked. |
| `lexical_unavailable` | search | `frankensearch/lexical` | BM25 arm disabled at build. |
| `mcp_feature_disabled` | mcp manifest | `mcp` | MCP manifest remains available, but the stdio adapter is disabled in this build. |
| `mcp_unavailable` | doctor, dependency contract | `mcp` | MCP adapter feature off. |
| `runtime_unavailable` | status, doctor | `asupersync` | Runtime feature off (defensive; should never fire in a real build). |
| `search_unimplemented` | status | `frankensearch` core feature | Whole search subsystem disabled. |
| `storage_unimplemented` | status | `fsqlite` core feature | Whole storage subsystem disabled. |
| `toon_unavailable` | status, doctor | TOON renderer feature | TOON format renderer unavailable or explicitly disabled. |

### `mixed` (3 codes — feature + state)

| Code | Surface | Notes |
|------|---------|-------|
| `cass_unavailable` | doctor, import cass | Build-time: `cass` not on PATH at install. Response-time: PATH check fails per call. After E5, presence in capabilities.available[]; per-call resolution failure stays in degraded[]. |
| `graph_unavailable` | doctor, diag graph | Build-time: `fnx-*` feature. Response-time: snapshot generation failed. Split per E5. |
| `search_unavailable` | status, dependency contract | Build-time: `frankensearch`. Response-time: index manifest missing. Split per E5. |

### `response_time` codes — stay in `degraded[]`

#### Search and pack quality (40)
| Code | Severity (canonical) | Bead |
|------|----------------------|------|
| `conflict_direct` | medium | bd-1zb7k.9 (S8) |
| `conflict_trust_mismatch` | high | bd-1zb7k.9 (S8) |
| `consensus_no_clusters` | low | bd-1zb7k.9 (S8) |
| `agent_profile_cold_start` | info | bd-1prrl.2.5 |
| `coordination_source_stale` | low | bd-1zb7k.4 (S3) |
| `coordination_source_unavailable` | medium | bd-1zb7k.4 (S3) |
| `context_evidence_freshness_changed_source` | low | bd-17c65.1.2 (A2) |
| `context_profile_budget_capped` | info | bd-17c65.2.4 (B7) |
| `duplicates_collapsed` | low | bd-17c65.2.3 (B3) |
| `expired_filtered` | low | bd-17c65.2.8 (B8) |
| `future_validity_filtered` | low | bd-17c65.2.10 (B11) |
| `index_corrupt` | high | bd-17c65.2.1 (B1) |
| `index_missing` | medium | bd-17c65.2.1 (B1) |
| `index_stale` | high | bd-17c65.2.1 (B1) |
| `low_recall_after_floor` | info | bd-17c65.2.1 (B1) |
| `malformed_validity_filtered` | medium | bd-17c65.2.10 (B11) |
| `mesh_peer_human_explicit_filtered` | medium | bd-29ulx (SRR6.5) |
| `no_relevant_results` | medium | bd-17c65.2.1 (B1) |
| `output_redaction_disabled` | info | bd-17c65.2.9 (B10) |
| `pack_assembly_budget_exceeded` | medium | bd-1zb7k.5 (S4) |
| `pack_assembly_slow` | low | bd-1zb7k.5 (S4) |
| `pack_concurrent_limit_reached` | low | bd-1zb7k.5 (S4) |
| `swarm_scale_budget_exceeded` | warning | bd-1zb7k.8 (S7) |
| `swarm_scale_nondeterminism` | high | bd-1zb7k.8 (S7) |
| `profile_search_limit_capped` | low | bd-17c65.2.4 (B7) |
| `scope_agent_unavailable` | warning | bd-17c65.10.6 (J6) |
| `scope_excluded_evidence` | low | bd-17c65.10.6 (J6) |
| `scope_metadata_unavailable` | medium | bd-17c65.10.6 (J6) |
| `scope_strict_excluded_evidence` | medium | bd-17c65.10.6 (J6) |
| `source_mode_fallback` | warning | bd-17c65.2.6 (B6) |
| `stale_validity_filtered` | low | bd-17c65.2.10 (B11) |
| `tombstoned_filtered` | low | bd-17c65.2.8 (B8) |
| `tombstoned_in_results` | low | bd-17c65.2.8 (B8) |
| `validity_filtered_significant_recall_drop` | warning | bd-17c65.2.10 (B11) |
| `weak_query_recall` | low | bd-17c65.2.5 (B5) |
| `search_index_stale` | medium | bd-17c65.2.1 (B1) |
| `search_index_degraded` | medium | bd-17c65.10.6 (J6) |
| `conformal_calibration_insufficient` | warning | bd-17c65.14.2 (N2) |
| `perf_latency_evidence_missing` | medium | bd-1zb7k.11 (P) |
| `perf_latency_evidence_partial` | warning | bd-1zb7k.11 (P) |

#### Disk pressure and build admission (4)
| Code | Severity | Bead |
|------|----------|------|
| `artifact_destination_not_external` | warning | bd-1zb7k.11.4 (P4) |
| `build_admission_denied` | medium | bd-1zb7k.11.4 (P4) |
| `cargo_target_not_external` | warning | bd-1zb7k.11.4 (P4) |
| `tmpdir_not_external` | warning | bd-1zb7k.11.4 (P4) |

#### Swarm coordination and QoS (1)
| Code | Severity | Bead |
|------|----------|------|
| `qos_registry_unavailable` | medium | bd-1zb7k.20.2 |

#### Storage and runtime state (16)
| Code | Severity | Bead |
|------|----------|------|
| `db_migration_pending` | medium | bd-3usjw.1 (db inspect) |
| `db_wal_stale` | medium | bd-3usjw.1 (db inspect) |
| `read_pool_acquire_timeout` | medium | bd-2caru.7 |
| `read_pool_undersized` | low | bd-2caru.7 |
| `search_not_inspected` | low | bd-17c65.10.6 (J6) |
| `search_not_ready` | medium | bd-17c65.10.6 (J6) |
| `search_waiting_for_storage` | medium | bd-17c65.10.6 (J6) |
| `storage_degraded` | medium | bd-17c65.10.6 (J6) |
| `storage_not_inspected` | low | bd-17c65.10.6 (J6) |
| `storage_not_initialized` | medium | bd-17c65.10.6 (J6) |
| `storage_not_ready` | medium | bd-17c65.10.6 (J6) |
| `memory_health_unavailable` | low | bd-17c65.10.6 (J6) |
| `snapshot_pin_expired` | medium | bd-2caru.6 |
| `snapshot_pin_force_released` | medium | bd-2caru.6 |
| `snapshot_release_failed` | medium | bd-2caru.6 |
| `wal_holds_orphaned` | high | bd-17c65.12.6 (derived backup assets) |

#### Policy and detector (3)
| Code | Severity | Bead |
|------|----------|------|
| `policy_bypass_used` | info | bd-17c65.3.2 (C2) |
| `policy_secret_detected_with_offsets` | medium | bd-17c65.3.4 (C4) |
| `policy_tag_rejected_with_details` | low | bd-17c65.3.4 (C4) |

#### Learn / curate (16)
| Code | Severity | Bead |
|------|----------|------|
| `auto_propose_deferred_to_maintenance` | info | bd-17c65.7.3 (G3) |
| `auto_propose_failed` | low | bd-17c65.7.3 (G3) |
| `auto_propose_search_neighbor_lookup_failed` | info | bd-17c65.7.3 (G3) |
| `auto_propose_skipped_existing_rule_covers` | info | bd-17c65.7.3 (G3) |
| `auto_propose_skipped_too_few_neighbors` | info | bd-17c65.7.3 (G3) |
| `cass_evidence_not_available` | low | bd-17c65.7.4 (G4) |
| `curation_harmful_candidate_escalated` | high | bd-17c65.7.4 (G4) |
| `curation_health_unavailable` | low | bd-17c65.10.6 (J6) |
| `curation_ttl_blocked` | medium | bd-17c65.7.4 (G4) |
| `curation_ttl_policy_missing` | medium | bd-17c65.7.4 (G4) |
| `curation_ttl_policy_unavailable` | medium | bd-17c65.10.6 (J6) |
| `level_transition_concurrent_conflict` | medium | bd-17c65.7.8 (G9) |
| `level_transition_requires_evidence` | medium | bd-17c65.7.8 (G9) |
| `level_transition_tombstoned_rejected` | medium | bd-17c65.7.8 (G9) |
| `auto_link_disabled` | info | bd-17c65.7.6 (G7) — workflow-less honest-unimplemented marker |
| `remember_auto_link_failed` | low | bd-17c65.7.3 (G3) |
| `remember_link_suggestion_failed` | low | bd-17c65.7.3 (G3) |

#### Feedback (3)
| Code | Severity | Bead |
|------|----------|------|
| `feedback_health_unavailable` | low | bd-17c65.10.6 (J6) |
| `feedback_protected_rules_unavailable` | medium | bd-17c65.10.6 (J6) |
| `feedback_quarantine_unavailable` | medium | bd-17c65.10.6 (J6) |

#### Why / pack inspection and proof verification (6)
| Code | Severity | Bead |
|------|----------|------|
| `graph_memory_not_in_snapshot` | low | bd-17c65.10.6 (J6) |
| `graph_query_relative_features_unavailable` | low | bd-17c65.10.6 (J6) |
| `proof_tool_missing` | info | bd-nnfq4 (SRR2) |
| `proof_violation_detected` | high | bd-nnfq4 (SRR2) |
| `verification_evidence_not_found` | low | bd-1zb7k.3 (S2) |
| `why_pack_selection_unavailable` | low | bd-17c65.10.6 (J6) |
| `why_result_target_unsupported_source` | medium | bd-17c65.10.6 (J6) |

#### Preflight + quarantine (11)
| Code | Severity | Bead |
|------|----------|------|
| `bypass_rate_limit_exceeded` | high | bd-3usjw.6.1 |
| `bypass_token_exhausted` | high | bd-3usjw.6.1 |
| `bypass_token_expired` | medium | bd-3usjw.6.1 |
| `bypass_token_invalid` | high | bd-3usjw.6.1 |
| `bypass_token_revoked` | high | bd-3usjw.6.1 |
| `no_risk_memories` | info | bd-3usjw.6 |
| `preflight_evidence_stale` | warning | bd-17c65.10.6 (J6) |
| `preflight_evidence_unavailable` | medium | bd-17c65.10.6 (J6) |
| `preflight_patterns_unavailable` | medium | bd-3usjw.6 |
| `quarantine_database_missing` | medium | bd-17c65.10.6 (J6) |
| `quarantine_workspace_unavailable` | medium | bd-17c65.10.6 (J6) |

#### Discoverability + usage (3)
| Code | Severity | Bead |
|------|----------|------|
| `deprecated_alias` | info | bd-17c65.6.7 (F7) |
| `usage_conflicting_presets` | low | bd-17c65.4.5 (D5) |
| `usage_unknown_field` | low | bd-17c65.4.5 (D5) |

#### Curate validation gates (6)
| Code | Severity | Bead |
|------|----------|------|
| `candidate_too_generic` | medium | bd-17c65.7.4 (G4 — curate validation) |
| `clustering_insufficient_data` | info | bd-17c65.7.5 (G5) |
| `clustering_threshold_too_strict` | low | bd-17c65.7.5 (G5) |
| `duplicate_rule_exact` | medium | bd-17c65.7.4 (G4) |
| `duplicate_rule_near` | low | bd-17c65.7.4 (G4) |
| `duplicate_rule_insufficient_signal` | low | bd-17c65.7.4 (G4) |
| `review_queue_invalid_transition` | medium | bd-17c65.7.4 (G4) |

#### Maintenance + steward (14)
| Code | Severity | Bead |
|------|----------|------|
| `decay_sweep_database_missing` | high | bd-17c65.12.4 (L3) |
| `decay_sweep_database_open_failed` | high | bd-17c65.12.4 (L3) |
| `decay_sweep_database_unresolved` | medium | bd-17c65.12.4 (L3) |
| `decay_sweep_handler_failed` | high | bd-17c65.12.4 (L3) |
| `decay_sweep_item_limit_too_large` | low | bd-17c65.12.4 (L3) |
| `decay_sweep_migration_failed` | high | bd-17c65.12.4 (L3) |
| `decay_sweep_workspace_unresolved` | medium | bd-17c65.12.4 (L3) |
| `learn_decay_config_invalid` | medium | bd-17c65.12.4 (L3) |
| `learn_decay_config_read_failed` | medium | bd-17c65.12.4 (L3) |
| `maintenance_job_history_read_failed` | medium | bd-17c65.10.6 (J6) |
| `maintenance_job_history_write_failed` | medium | bd-17c65.10.6 (J6) |
| `maintenance_job_lock_busy` | warning | bd-17c65.10.6 (J6) |
| `maintenance_job_not_found` | medium | bd-17c65.10.6 (J6) |
| `maintenance_job_since_invalid` | low | bd-17c65.10.6 (J6) |

#### Schema + integrity (3)
| Code | Severity | Bead |
|------|----------|------|
| `migration_drift` | high | bd-17c65.12.5 (L4) |
| `serialization_failed` | medium | bd-17c65.10.6 (J6) |
| `trust_promotion_evidence_rejected` | medium | bd-17c65.7.4 (G4) |

#### Concurrency + write owner (7)
| Code | Severity | Bead |
|------|----------|------|
| `advisory_lock_timeout` | medium | bd-3usjw.57 |
| `index_publish_lock_contention` | warning | bd-17c65.12.2 (L1) |
| `write_owner_busy` | warning | bd-17c65.12.2 (L1) |
| `write_spool_backpressure` | warning | bd-17c65.12.2 (L1) |
| `write_queue_full` | low | bd-17c65.12.2 (L1) |
| `write_hot_path_cancelled_before_commit` | medium | bd-2lsxf.2.4 (SRR3) |
| `write_hot_path_fsync_failure` | high | bd-2lsxf.2.4 (SRR3) |

#### Other (7)
| Code | Severity | Bead |
|------|----------|------|
| `graph_feature_disabled` | medium | bd-17c65.5.3 (E3) — different from build-time `graph_unavailable`; this is a per-call disable |
| `serve_unavailable_v1` | low | bd-3usjw.4 |
| `singleflight_follower_timeout` | medium | bd-gni47.3 (SF3) |
| `singleflight_leader_failed` | medium | bd-gni47.3 (SF3) |
| `singleflight_state_poisoned` | high | bd-gni47.3 (SF3) |
| `situation_decisioning_unavailable` | medium | (TBD) |
| `test_degraded` | info | testing harness (synthetic; not emitted in production paths) |

#### Tailscale local probe (7)
| Code | Severity | Bead |
|------|----------|------|
| `tailscale_binary_inauthentic` | high | bd-36bbk.1.1 |
| `tailscale_daemon_unreachable` | warning | bd-36bbk.1.1 |
| `tailscale_not_authenticated` | warning | bd-36bbk.1.1 |
| `tailscale_not_installed` | warning | bd-36bbk.1.1 |
| `tailscale_probe_timeout` | warning | bd-36bbk.1.1 |
| `tailscale_probe_unavailable` | info | bd-36bbk.1.1 |
| `tailscale_shields_up` | warning | bd-36bbk.1.1 |

#### Causal lab (13)
| Code | Severity | Bead |
|------|----------|------|
| `causal_chain_id_required` | low | bd-17c65.14.3 (N3) |
| `causal_chain_not_found` | medium | bd-17c65.14.3 (N3) |
| `causal_chain_pair_required` | low | bd-17c65.14.3 (N3) |
| `causal_comparison_evidence_unavailable` | medium | bd-17c65.14.3 (N3) |
| `causal_confounders_unavailable` | medium | bd-17c65.14.3 (N3) |
| `causal_database_migration_failed` | high | bd-17c65.14.3 (N3) |
| `causal_database_missing` | high | bd-17c65.14.3 (N3) |
| `causal_database_open_failed` | high | bd-17c65.14.3 (N3) |
| `causal_evidence_table_missing` | medium | bd-17c65.14.3 (N3) |
| `causal_evidence_unavailable` | medium | bd-17c65.14.3 (N3) |
| `causal_failure_id_required` | low | bd-17c65.14.3 (N3) |
| `causal_insufficient_chains` | low | bd-17c65.14.3 (N3) |
| `causal_ledger_empty` | info | bd-17c65.14.3 (N3) |
| `causal_no_matching_chains` | info | bd-17c65.14.3 (N3) |
| `causal_sample_underpowered` | warning | bd-17c65.14.3 (N3) |
| `causal_trace_store_failed` | high | bd-17c65.14.3 (N3) |
| `causal_workspace_id_required` | low | bd-17c65.14.3 (N3) |
| `conditional_independence` | info | bd-17c65.14.3 (N3) — assumption-check signal |
| `no_confounders` | info | bd-17c65.14.3 (N3) |
| `no_filters` | info | bd-17c65.14.3 (N3) |
| `no_sources` | info | bd-17c65.14.3 (N3) |
| `proper_randomization` | info | bd-17c65.14.3 (N3) |

#### Drift / metric analysis (6)
| Code | Severity | Bead |
|------|----------|------|
| `drift_analysis_unavailable` | medium | (TBD) |
| `drift_no_comparable_metrics` | low | (TBD) |
| `drift_no_evaluation_snapshots` | info | (TBD) |
| `metric_missing` | low | bd-17c65.10.6 (J6) |
| `missing_metric` | low | bd-17c65.10.6 (J6) |
| `replay_fidelity` | info | bd-17c65.14.15.5 (N15.4) |
| `stable_unit` | info | bd-17c65.14.3 (N3) — replay verification |

#### Graph snapshot (6 — response_time variants of graph_unavailable)
| Code | Severity | Bead |
|------|----------|------|
| `graph_algorithm_unavailable` | medium | bd-3usjw.2 |
| `graph_snapshot_missing` | medium | bd-17c65.5.3 (E3) |
| `graph_snapshot_stale` | medium | bd-17c65.5.3 (E3) |
| `graph_snapshot_scores_unavailable` | low | bd-17c65.5.3 (E3) |
| `graph_snapshot_topology_unavailable` | low | bd-17c65.5.3 (E3) |
| `graph_snapshot_unusable` | high | bd-17c65.5.3 (E3) |

#### Graph accretion sentinels (10 — response_time variants of graph_unavailable)
| Code | Severity | Bead |
|------|----------|------|
| `graph_ppr_snapshot_stale` | medium | bd-bife.6 |
| `graph_ppr_empty_seed_set` | low | bd-bife.6 |
| `graph_pack_dna_no_dominator` | low | bd-bife.6 |
| `graph_causal_no_evidence` | low | bd-bife.6 |
| `graph_health_no_contradictions` | info | bd-bife.6 |
| `graph_curate_disconnected_graph` | warning | bd-bife.6 |
| `graph_proximity_unreachable` | info | bd-bife.6 |
| `graph_dominance_no_revision_chain` | info | bd-bife.6 |
| `graph_skyline_degenerate_communities` | info | bd-bife.6 |
| `graph_hits_convergence_failure` | warning | bd-bife.6 |

#### Integrity / schema (15)
| Code | Severity | Bead |
|------|----------|------|
| `handoff_capsule_machine_mismatch` | high | bd-17c65.13.6 (M5) |
| `handoff_capsule_tampered` | high | bd-17c65.13.6 (M5) |
| `handoff_hmac_missing` | high | bd-17c65.13.6 (M5) |
| `handoff_hmac_skipped` | high | bd-17c65.13.6 (M5) |
| `handoff_snapshot_stale` | medium | bd-17c65.13.5 (M4) |
| `integrity_database_missing` | high | bd-17c65.12.2 (L1) |
| `integrity_database_open_failed` | high | bd-17c65.12.2 (L1) |
| `integrity_provenance_sample_unavailable` | low | bd-17c65.12.2 (L1) |
| `integrity_reference_check_unavailable` | medium | bd-17c65.12.2 (L1) |
| `integrity_reference_issues` | medium | bd-17c65.12.2 (L1) |
| `integrity_schema_check_unavailable` | medium | bd-17c65.12.2 (L1) |
| `integrity_schema_migration_required` | high | bd-17c65.12.5 (L4) |
| `stale_schema_version` | high | bd-17c65.12.5 (L4) |
| `strict_mode_no_salt_file` | high | bd-17c65.13.6 (M5) |
| `tampered_hash` | critical | bd-17c65.13.6 (M5) |

#### Maintenance jobs (5)
| Code | Severity | Bead |
|------|----------|------|
| `maintenance_job_cancelled` | info | bd-17c65.10.6 (J6) |
| `maintenance_job_failed` | high | bd-17c65.10.6 (J6) |
| `maintenance_job_lock_open_failed` | medium | bd-17c65.10.6 (J6) |
| `maintenance_job_skipped` | info | bd-17c65.10.6 (J6) |
| `maintenance_job_timed_out` | medium | bd-17c65.10.6 (J6) |

#### Quarantine + trust (4)
| Code | Severity | Bead |
|------|----------|------|
| `quarantine_database_unreadable` | medium | bd-17c65.10.6 (J6) |
| `quarantine_feedback_events_unreadable` | medium | bd-17c65.10.6 (J6) |
| `quarantine_rows_unreadable` | medium | bd-17c65.10.6 (J6) |
| `trust_quarantine_rows_unreadable` | medium | bd-17c65.10.6 (J6) |

#### Coordination / external tools (20)
| Code | Severity | Bead |
|------|----------|------|
| `agent_mail_unavailable` | medium | bd-2nkbn (Agent Mail resilience) |
| `agent_mail_archive_degraded` | warning | bd-1zb7k.11 (P) |
| `agent_status_unavailable` | low | (TBD) |
| `beads_tracker_stale` | warning | bd-1zb7k.13.3 (C3) |
| `beads_unavailable` | medium | bd-1zb7k.4 (S3) |
| `bv_unavailable` | medium | bd-1zb7k.4 (S3) |
| `git_unavailable` | warning | bd-1zb7k.4 (S3), bd-1eq3l.11 |
| `git_not_repository` | medium | bd-1eq3l.11 |
| `rch_remote_required_fallback_prevented` | warning | bd-1zb7k.13.4 (C4) |
| `rch_unavailable` | low | bd-1zb7k.5 (S4) |
| `rch_worker_topology_blocked` | warning | bd-1zb7k.13.4 (C4) |
| `workspace_hygiene_agent_mail_timeout` | warning | bd-1eq3l.11 |
| `workspace_hygiene_agent_mail_unavailable` | warning | bd-1eq3l.11 |
| `workspace_hygiene_beads_parse_error` | medium | bd-1eq3l.11 |
| `workspace_hygiene_beads_reserved` | warning | bd-1eq3l.11 |
| `workspace_hygiene_beads_unavailable` | medium | bd-1eq3l.11 |
| `workspace_hygiene_config_invalid` | medium | bd-1eq3l.11 |
| `workspace_hygiene_output_truncated` | warning | bd-1eq3l.11 |
| `workspace_hygiene_partial_metadata` | warning | bd-1eq3l.11 |
| `workspace_hygiene_secret_scan_skipped` | medium | bd-1eq3l.11 |

#### Model registry / science (5)
| Code | Severity | Bead |
|------|----------|------|
| `model_registry_empty` | low | bd-17c65.10.6 (J6) |
| `model_registry_no_available_entry` | medium | bd-17c65.10.6 (J6) |
| `science_backend_unavailable` | medium | bd-17c65.11.7 (K7) |
| `science_budget_exceeded` | warning | bd-17c65.11.7 (K7) |
| `science_input_too_large` | warning | bd-17c65.11.7 (K7) |
| `science_not_compiled` | medium | bd-17c65.11.7 (K7) |

#### Clustering (2 — distinct from G5 sufficiency signals)
| Code | Severity | Bead |
|------|----------|------|
| `clustering_no_candidates` | info | bd-17c65.7.5 (G5) |
| `clustering_no_embeddings` | info | bd-17c65.7.5 (G5) |

#### Miscellaneous (15)
| Code | Severity | Bead |
|------|----------|------|
| `action_override_not_actionable` | low | (TBD) |
| `advisory_memory` | info | (TBD) — advisory-memory presence marker |
| `degraded_context` | info | bd-17c65.5.2 (E2) — retired tombstone for legacy meta-signal; context emits concrete degraded[] entries instead |
| `dry_run_recommended` | info | (TBD) |
| `fixture_tier_mismatch` | low | (TBD) |
| `heavy_gates_skipped` | info | (TBD) |
| `index_locked` | medium | bd-17c65.10.6 (J6) |
| `lab_replay_unavailable` | medium | bd-17c65.14.15.5 (N15.4) — slated for retirement once N15 lands |
| `legacy_memory` | info | (TBD) — legacy import marker |
| `manual_heavy_strategy` | warning | bd-17c65.10.6 (J6) |
| `profile_mismatch` | medium | (TBD) |
| `profile_missing` | low | (TBD) |
| `redaction_pattern_matched` | medium | bd-17c65.11.6 (K6) — emitted per redaction event |
| `redaction_level_invalid` | low | bd-17c65.11.6 (K6) — error envelope; bad --redaction value |
| `redaction_round_trip_marker_preserved` | info | bd-17c65.11.6 (K6) — import surfaces preserved markers |
| `redaction_uncertain` | warning | bd-17c65.11.6 (K6) |
| `derived_asset_corrupt` | high | bd-17c65.12.6 (derived backup assets) |
| `semantic_dimension_exceeds_budget` | warning | (TBD) — composes with semantic-model gating |
| `tombstone_visibility_unavailable` | medium | bd-17c65.2.8 (B8) |
| `tripwire_inputs_incomplete` | low | (TBD) |
| `unknown_method` | medium | (TBD) |
| `unsupported_artifact_kind` | medium | (TBD) |
| `unsupported_condition` | medium | (TBD) |
| `unsupported_schema` | medium | (TBD) |
| `windows_appdata_unavailable` | medium | bd-3usjw.68 |
| `workspace_nested_markers` | warning | bd-17c65.12.2 (L1) |

#### Mixed: storage_unavailable
| Code | Severity | Bead |
|------|----------|------|
| `storage_unavailable` | high | bd-17c65.10.6 (J6) — also classified in mixed table above; appears as response_time when storage feature is built |

## Capabilities surface

Build-time gaps are reported once by `ee capabilities --json`:

```json
"unimplemented": [
  {
    "code": "lexical_unavailable",
    "featureFlag": "lexical-bm25",
    "trackingBead": "bd-17c65.5.5",
    "userMessage": "BM25 lexical search is disabled in this build."
  },
  { "...": "other build_time codes" }
]
```

Only `response_time` codes, plus the response-time half of `mixed`
codes, belong in response-local `data.degraded[]` arrays.

## Severity vocabulary (canonical; 6 tiers)

Per `tests/fixtures/failure_modes/SCHEMA.md` v1, severity values are
ordered: `info < low < warning < medium < high < critical`.

- **`info`** — purely informational; no action needed.
- **`low`** — informational; agent may want to read more.
- **`warning`** — degraded behavior; non-blocking but may affect quality.
- **`medium`** — response was affected; suggest repair.
- **`high`** — response is unreliable; strongly suggest repair.
- **`critical`** — unrecoverable; operator action required.

A code's severity is documented in `tests/fixtures/failure_modes/<code>.json`
and asserted by the J6 catalog validator.

## Test plan (deferred to a sibling bead)

`tests/degraded_code_taxonomy_consistency_test.rs` (NOT yet authored):

1. Every code emitted in `src/` (grep `"code": "..."` + `pub const ..._CODE`) appears in this taxonomy.
2. Every code in this taxonomy is emitted in `src/` (no orphans).
3. Severity values in this doc match `tests/fixtures/failure_modes/<code>.json` exactly.
4. After E5: no `build_time` code appears in any `degraded[]` test fixture.

When this test lands, it will be a sibling sub-bead of `bd-fptj3` (or
folded into the J6 catalog validator).
