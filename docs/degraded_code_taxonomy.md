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

## Usage errors are not degraded codes

Some agent-facing failures are intentionally outside this taxonomy because the
command cannot continue with a degraded result. Typed memory field validation is
one of those surfaces. Unknown fields, wrong field shapes, invalid RFC3339
`revisit_by` values, oversized values, and kind mismatches fail as usage or
validation errors with actionable details. They must not be added to
`data.degraded[]` merely to keep a command successful.

For the typed-field registry and operator surface, see
[`docs/memory-typed-fields.md`](memory-typed-fields.md). For decision fork
refusal and revisit timestamp usage branches, see
[`docs/agent-ux/decide.md`](agent-ux/decide.md).

## Repair Action Risk Classes

Agent-facing recovery actions can include command strings. Those commands are
not automatically safe just because they are concrete. When a recovery or
fallback action names a command, emit or derive the shared repair-action safety
metadata so agents can branch mechanically instead of parsing prose.

| Risk class | Agent may run? | Preflight | Human approval | Beads / Agent Mail mutation | Log privacy |
| --- | --- | --- | --- | --- | --- |
| `read_only_probe` | Yes, when the command uses structured argv or an already-safe shell form. | Not required. | Not required. | Must not mutate local files, Beads, Agent Mail, RCH daemon state, workers, or git history. | Use bounded command metadata only. |
| `idempotent_refresh` | Yes, when the workspace or derived-asset target is explicit. | Not required unless `preflightCommand` is present. | Not required. | Must not mutate Beads or Agent Mail; may rebuild local derived assets only. | Use bounded command metadata and derived-asset evidence codes. |
| `mutating_local_repair` | Yes after reviewing recovery details. | Required when `preflightCommand` is present. | Not required unless also marked by policy. | Must not mutate Beads or Agent Mail; may mutate source-of-truth local state for the selected workspace. | Use bounded command metadata; omit raw database errors and full file listings. |
| `mutating_external_coordination_repair` | Only after coordinating with active agents. | Required when `preflightCommand` is present. | Required when `requiresHumanApproval` is true. | May mutate Beads, Agent Mail, RCH daemon/worker state, or another shared coordination substrate. | Use tracker or coordination metadata only; no raw mail bodies. |
| `approval_required_repair` | No, not autonomously. | Run preflight only after approval if a command is still intended. | Required before execution. | Unknown until reviewed; assume shared state could be affected. | Log only the command shape and review evidence. |
| `destructive_or_irreversible_repair` | No. | Required as part of the explicit approval flow. | Required for the exact command, following `AGENTS.md`. | May delete, rewrite history, destroy state, or otherwise be irreversible. | Log the command and approval evidence only; never include secrets or dumps. |
| `unavailable_or_manual_only` | No command is agent-runnable. | Not applicable. | Required if an operator chooses a manual repair. | Unknown until a human-defined repair exists. | Use `manualStep`, bounded evidence codes, and Beads updates. |

Safety metadata must be redaction-safe: no raw mail bodies, full dirty-file
listings, stack traces, secrets, or unbounded local database errors. Use bounded
evidence codes and preconditions instead.

## Environment Attestation Degraded Codes

`ee diag environment-attestation --workspace . --include-rch --json` reports
readiness blockers as source-authority evidence, not as proof that source
compile/tests passed or failed. Its `degraded[]` entries follow the same
severity vocabulary as other response-time degraded codes. Current attestation
policy assigns `high` to RCH/build-admission proof-environment blockers and
`local_cargo_bypass_detected`; the remaining attestation degraded codes are
`warning`.

The command's recovery actions use the repair-action risk classes above:

| Attestation case | Typical codes | Risk class / handling |
| --- | --- | --- |
| Read-only inspection | `dirty_checkout_observed`, `source_authority_ambiguous`, `stale_binary_suspected`, `missing_required_surface` | `read_only_probe`; structured `argv` such as `git status --short --branch --untracked-files=all`, `ee --version`, or `ee swarm brief --workspace . --include-rch --json` may be run by a harness. |
| Coordination required | `agent_mail_unavailable`, `agent_mail_probe_mismatch`, `reservation_evidence_stale` | `mutating_external_coordination_repair` if the next step sends mail, changes reservations, or touches Beads; the attestation itself is read-only. |
| Beads/BV disagreement | `beads_tracker_stale`, `beads_metadata_only_stale`, `bv_recommendation_stale` | Beads remains authoritative for tracker state; BV is advisory. Mutating Beads repair commands require coordination. |
| Remote proof environment blocked | `rch_worker_topology_blocked`, `rch_source_materialization_blocked`, `rch_remote_required_fallback_prevented`, `rch_verify_client_daemon_version_skew`, `rch_verify_remote_transport_timeout`, `rch_verify_worker_health_threshold_blocked`, `build_admission_blocked` | `read_only_probe` for inspection commands such as `rch status --json`; do not reclassify as `source_failed` and do not substitute local Cargo proof. |
| Local Cargo bypass | `local_cargo_bypass_detected` | `approval_required_repair`; requires human decision because it contradicts remote-only verification policy. |
| Support-bundle redaction unknown | `support_bundle_redaction_unverified` | Verify redaction before attaching bundle evidence; support bundles do not replace a fresh claim gate. |

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
| `bridges` | `ee insights --section bridges` degradation. |
| `causalBottlenecks` | `ee insights --section causalBottlenecks` degradation. |
| `comprehensiveRules` | `ee insights --section comprehensiveRules` degradation. |
| `contradictionClusters` | `ee insights --section contradictionClusters` degradation. |
| `causal_trace` | `ee causal trace` degradation. |
| `causal_estimate` | `ee causal estimate` degradation. |
| `causal_compare` | `ee causal compare` degradation. |
| `causal_promote_plan` | `ee causal promote-plan` degradation. |
| `kCore` | `ee insights --section kCore` degradation. |
| `kTruss` | `ee insights --section kTruss` structural evidence source. |
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
| `audit_lane` | Swarm-X audit-lane enqueue, drain, batch-commit, shutdown, and backpressure degradation. |
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
| `environment_attestation` | `ee diag environment-attestation` source-authority and proof-admission degradation. |
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
| `daemon_status` | `ee daemon status` foreground-supervisor capability degradation. |
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
| `graph_snapshot_prune` | `ee maintenance graph-snapshot-prune` graph-snapshot retention degradation. |
| `graph_witness_prune` | `ee maintenance graph-witnesses-prune` witness-retention degradation. |
| `hits` | `ee graph hits` HITS algorithm degradation. |
| `gomory_hu_proximity` | `ee proximity` Gomory-Hu min-cut proximity degradation. |
| `review_session` | `ee review session` curation proposal degradation. |
| `review_workspace` | `ee review workspace` curation proposal degradation. |
| `host_profile` | Swarm brief host-profile source degradation. |
| `integrity` | `ee diag integrity` database, schema, canary, or provenance-sample degradation. |
| `lab_counterfactual` | `ee lab counterfactual` replay-evidence degradation. |
| `lab_replay` | `ee lab replay` replay-evidence degradation. |
| `learn_cluster` | `ee learn cluster` deterministic clustering degradation. |
| `maintenance` | Generic maintenance response degradation when a narrower command label is unavailable. |
| `maintenance_run` | `ee maintenance run` and `ee job run` maintenance job execution degradation. |
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
| `write_owner_diagnostics` | `ee diag write-owner` queue-busy degradation. |
| `write_spool_diagnostics` | `ee diag write-spool` queue-backpressure degradation. |

When adding a new renderer, prefer the most specific stable section, command,
or algorithm label available. Do not include workspace paths, query text, or
memory bodies in `sources[]`.

## Full code inventory

> Sources of truth: `tests/fixtures/failure_modes/README.md` for the
> agent-facing catalog AND the union of `pub const *_CODE` constants
> + `"code": "..."` JSON literals in `src/`. When either source gains
> a new code, add a row here in the same commit. The
> `tests/degraded_code_taxonomy_consistency_test.rs` enforces this.

### `build_time` (11 codes — surfaced through `ee capabilities`)

| Code | Surface | Feature flag | Notes |
|------|---------|--------------|-------|
| `agent_detection_unavailable` | agent sources, doctor | (binary detection logic) | Reflects compile-time exclusion of agent-detection paths. |
| `diagram_backend_unavailable` | doctor, dependency contract | (mermaid renderer feature) | Mermaid backend not linked. |
| `lexical_unavailable` | search | `frankensearch/lexical` | BM25 arm disabled at build. |
| `mcp_feature_disabled` | mcp manifest, mcp serve-stdio | `mcp` | MCP discovery remains available, but the stdio adapter is disabled in this build. |
| `mcp_unavailable` | doctor, dependency contract | `mcp` | MCP adapter feature off. |
| `runtime_unavailable` | status, doctor | `asupersync` | Runtime feature off (defensive; should never fire in a real build). |
| `search_unimplemented` | status | `frankensearch` core feature | Whole search subsystem disabled. |
| `storage_unimplemented` | status | `fsqlite` core feature | Whole storage subsystem disabled. |
| `toon_unavailable` | status, doctor | TOON renderer feature | TOON format renderer unavailable or explicitly disabled. |

### `mixed` (4 codes — feature + state)

| Code | Surface | Notes |
|------|---------|-------|
| `cass_unavailable` | doctor, import cass | Build-time: `cass` not on PATH at install. Response-time: PATH check fails per call. After E5, presence in capabilities.available[]; per-call resolution failure stays in degraded[]. |
| `embed_model_unavailable` | search, context | Build-time: no dense embedder feature compiled. Response-time: embedder/model load failed or active embedder is `frankensearch_hash_fallback` with `semantic=false` while lexical fallback remains available. |
| `graph_unavailable` | doctor, diag graph | Build-time: `fnx-*` feature. Response-time: snapshot generation failed. Split per E5. |
| `search_unavailable` | status, dependency contract | Build-time: `frankensearch`. Response-time: index manifest missing. Split per E5. |

### `response_time` codes — stay in `degraded[]`

#### Toolchain provenance (3)
| Code | Severity (canonical) | Bead |
|------|----------------------|------|
| `toolchain_hash_unavailable` | info | bd-aunn3.2 |
| `toolchain_probe_timeout` | low | bd-aunn3.2 |
| `toolchain_tool_unresolved` | low | bd-aunn3.2 |

#### External derivation and reflection (22)
| Code | Severity (canonical) | Bead |
|------|----------------------|------|
| `create_derived_replay_ambiguous_audit` | high | bd-3vw03 |
| `create_derived_replay_missing_audit` | high | bd-3vw03 |
| `derived_evidence_already_linked` | medium | bd-1vnvl |
| `derived_invalid_memory_spec` | medium | bd-1vnvl |
| `derived_source_hash_drifted` | medium | bd-1vnvl |
| `derived_source_hash_mismatch` | medium | bd-3vw03 |
| `derived_source_evidence_already_linked` | medium | bd-3vw03 |
| `derived_source_evidence_missing` | medium | bd-3vw03 |
| `derived_source_memory_missing` | medium | bd-3vw03 |
| `derived_source_memory_tombstoned` | medium | bd-3vw03 |
| `derived_source_workspace_mismatch` | medium | bd-1vnvl |
| `derived_sources_invalid` | medium | bd-1vnvl |
| `derived_target_forbidden_for_create` | medium | bd-1vnvl |
| `derived_target_required_for_mutation` | medium | bd-1vnvl |
| `reflect_challenge_invalid` | high | bd-1vnvl |
| `reflect_key_unavailable` | high | bd-1vnvl |
| `reflect_raw_cot_rejected` | high | bd-1vnvl |
| `reflect_request_consumed` | medium | bd-1vnvl |
| `reflect_request_expired` | medium | bd-1vnvl |
| `reflect_result_schema_invalid` | medium | bd-1vnvl |
| `reflect_source_drifted` | medium | bd-1vnvl |
| `reflect_unknown_cited_source` | medium | bd-1vnvl |

Same-candidate create-derived replay is intentionally not a degraded/error code:
when the applied candidate, audit row, and derived memory agree, `curate apply`
returns the existing applied result. Missing, duplicate, or mismatched replay
evidence is classified under the `create_derived_replay_*` conflict codes above.

#### Search and pack quality (57)
| Code | Severity (canonical) | Bead |
|------|----------------------|------|
| `adaptive_backoff_applied` | low | bd-16pwc.2 (SRR5) |
| `conflict_direct` | medium | bd-1zb7k.9 (S8) |
| `conflict_trust_mismatch` | high | bd-1zb7k.9 (S8) |
| `consensus_no_clusters` | low | bd-1zb7k.9 (S8) |
| `agent_profile_cold_start` | info | bd-1prrl.2.5 |
| `certificate_store_unavailable` | medium | bd-79c16 |
| `coordination_source_stale` | low | bd-1zb7k.4 (S3) |
| `coordination_source_unavailable` | medium | bd-1zb7k.4 (S3) |
| `context_evidence_freshness_changed_source` | low | bd-17c65.1.2 (A2) |
| `context_delta_prior_unknown` | low | bd-muovx.5 (M) |
| `context_delta_format_unsupported` | info | bd-muovx.6 (M) |
| `context_delta_larger_than_full` | info | bd-muovx |
| `context_delta_no_baseline` | info | bd-7lvbg.6 (GOV) |
| `context_profile_budget_capped` | info | bd-17c65.2.4 (B7) |
| `context_stream_partial_emission` | warning | bd-17c65.10.18 |
| `cass_prefetch_budget_exceeded` | info | bd-16pwc.2 (SRR5) |
| `duplicates_collapsed` | low | bd-17c65.2.3 (B3) |
| `expired_filtered` | low | bd-17c65.2.8 (B8) |
| `future_validity_filtered` | low | bd-17c65.2.10 (B11) |
| `index_corrupt` | high | bd-17c65.2.1 (B1) |
| `index_missing` | medium | bd-17c65.2.1 (B1) |
| `index_stale` | high | bd-17c65.2.1 (B1) |
| `low_recall_after_floor` | info | bd-17c65.2.1 (B1) |
| `malformed_validity_filtered` | medium | bd-17c65.2.10 (B11) |
| `memory_drift_source_changed` | medium | bd-1z1fd.3 |
| `memory_drift_source_missing` | high | bd-1z1fd.3 |
| `memory_drift_source_unverifiable` | medium | bd-1z1fd.3 |
| `memory_drift_lock_contention` | warning | bd-1xpq9 (DRIFT) |
| `memory_debt_audit_window_partial` | info | bd-3ap2m.2 - `ee curate doctor` scanned a bounded audit window and older read evidence may be outside the inspected rows |
| `mesh_peer_human_explicit_filtered` | medium | bd-29ulx (SRR6.5) |
| `mi_dedup_candidate_proposed` | info | bd-17c65.14.14 (N14) |
| `mi_dedup_threshold_underpowered` | info | bd-17c65.14.14 (N14) |
| `no_relevant_results` | medium | bd-17c65.2.1 (B1) |
| `output_redaction_disabled` | info | bd-17c65.2.9 (B10) |
| `pack_assembly_budget_exceeded` | medium | bd-1zb7k.5 (S4) |
| `pack_assembly_slow` | low | bd-1zb7k.5 (S4) |
| `pack_bin_content_hash_mismatch` | high | bd-17c65.14.1 (N1) |
| `pack_bin_magic_mismatch` | medium | bd-17c65.14.1 (N1) |
| `pack_bin_version_too_new` | medium | bd-17c65.14.1 (N1) |
| `pack_budget_too_small` | warning | bd-3qs2i.2.1 (F2) |
| `pack_concurrent_limit_reached` | low | bd-1zb7k.5 (S4) |
| `swarm_scale_budget_exceeded` | warning | bd-1zb7k.8 (S7) |
| `swarm_scale_nondeterminism` | high | bd-1zb7k.8 (S7) |
| `profile_search_limit_capped` | low | bd-17c65.2.4 (B7) |
| `recent_hours_window_clamped` | warning | bd-1idcb (G) |
| `rerank_model_unavailable` | low | bd-2vq2z.6 |
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
| `search_score_calibration_file_too_large` | warning | bd-1nsk4 |
| `search_score_calibration_rows_corrupt` | warning | bd-3ihl4 |
| `discovery_cache_invalidated_tailnet_changed` | info | bd-36bbk.1.13 |
| `discovery_cache_stale_due_to_workspace_mismatch` | info | bd-36bbk.1.13 |
| `drift_grace_soft_stale_peer_count_high` | warning | bd-36bbk.1.13 |
| `hello_responder_not_running` | medium | bd-36bbk.1.12 |
| `hello_responder_port_in_use` | medium | bd-36bbk.1.12 |
| `hello_responder_no_tailscale_ip` | medium | bd-36bbk.1.12 |
| `hello_responder_crash_loop` | high | bd-36bbk.1.12 |
| `hello_responder_rate_limited_storm` | warning | bd-36bbk.1.12 |
| `host_calibration_contradictory` | medium | bd-1zb7k.12.3.4 (H3.4) |
| `host_calibration_missing` | warning | bd-1zb7k.12.3.4 (H3.4) |
| `host_calibration_partial` | warning | bd-1zb7k.12.3.4 (H3.4) |
| `host_calibration_rch_topology_blocked` | warning | bd-1zb7k.12.3.4 (H3.4) |
| `host_calibration_stale` | warning | bd-1zb7k.12.3.4 (H3.4) |
| `host_calibration_synthetic_only` | warning | bd-1zb7k.12.3.4 (H3.4) |
| `host_calibration_unavailable` | warning | bd-1zb7k.12.3.4 (H3.4) |
| `perf_latency_evidence_missing` | medium | bd-1zb7k.11 (P) |
| `perf_latency_evidence_partial` | warning | bd-1zb7k.11 (P) |
| `task_frame_intersect_empty` | info | bd-1idcb (G) |
| `l2_pack_cache_corruption` | low | (TBD) |
| `l2_pack_cache_unavailable` | low | (TBD) |
| `source_unparsable` | medium | (TBD) |
| `stale_line_span` | warning | (TBD) |
| `symbol_index_stale` | warning | (TBD) |

#### Hook readiness (1)
| Code | Severity (canonical) | Bead |
|------|----------------------|------|
| `git_ahead_unavailable` | warning | bd-2gc7r.3 |

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

#### Storage and runtime state (25)
| Code | Severity | Bead |
|------|----------|------|
| `db_migration_pending` | medium | bd-3usjw.1 (db inspect) |
| `db_wal_stale` | medium | bd-3usjw.1 (db inspect) |
| `wal_growth_exceeds_threshold` | warning | bd-2caru.8 |
| `wal_growth_no_writer` | medium | bd-2caru.8 |
| `shard_chain_mismatch` | high | bd-f6jfs.6 |
| `shard_fanout_catalog_missing` | warning | bd-f6jfs.2 |
| `shard_fanout_home_unavailable` | warning | bd-f6jfs.2 |
| `shard_fanout_root_unsafe` | high | bd-f6jfs.2 |
| `shard_fanout_shard_missing` | warning | bd-f6jfs.2 |
| `shard_fanout_workspace_id_unsafe` | high | bd-f6jfs.2 |
| `shard_fanout_workspace_unavailable` | warning | bd-f6jfs.2 |
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
| `cache_hotset_stale` | medium | (TBD) |
| `hotset_prewarm_no_signals` | low | bd-1zb7k.10.3 (O3) |
| `memory_tier_metadata_stale` | medium | bd-1prrl.6.4 (Swarm-X) |
| `cross_shard_skew_detected` | warning | (TBD) |
| `flight_recorder_directory_unwritable` | medium | (TBD) |
| `shard_attach_failed` | warning | (TBD) |

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

#### Journal capture (4)
| Code | Severity | Bead |
|------|----------|------|
| `journal_disabled` | info | bd-1pi9m.2 — `[journal] enabled = false` config gate; ADR 0062 §7 classifies it build_time/config, but this implementation reads the workspace config per call so the emission varies at response time |
| `journal_entry_truncated` | info | bd-1pi9m.2 — body/sidecar exceeded caps; deterministic truncation applied |
| `journal_redaction_applied` | info | bd-1pi9m.2 — secret classes redacted before storage |
| `distill_no_candidates` | info | bd-1pi9m.3 — ADR 0062 §7: distill scope had entries but nothing met proposal thresholds (honest empty, response time) |

#### Code-anchored recall (4)
| Code | Severity | Bead |
|------|----------|------|
| `anchor_index_empty` | info | bd-u875s.2 — ADR 0064 §5: the anchor reverse index has no rows for this workspace (nothing anchored yet); never a hard error |
| `anchor_index_stale` | low | bd-u875s.2 — ADR 0064 §5: reverse-index generation < DB generation; repair `ee index rebuild --workspace .` |
| `recall_filtered_empty` | info | bd-u875s.2 — ADR 0064 §5: anchored rows matched the surface but `--kind`/`--level`/`--stale` filters removed them all (distinct from empty-index so hook authors can tell the difference) |
| `recall_git_unavailable` | warning | bd-u875s.3 — ADR 0064 §2 `git_unavailable`-family: the read-only `git diff` shell-out behind `--diff`/`--diff-staged` failed; the diff selector degrades to an empty path set and recall continues (never blocks an edit) |

#### Workspace primer (3)
| Code | Severity | Bead |
|------|----------|------|
| `primer_cache_cold` | info | bd-39tzu.2 — ADR 0065 §6: no primer_cache row for the (generation, config, budget, format) key; assembled fresh |
| `primer_graph_unavailable` | info | bd-39tzu.2 — ADR 0065 §6: persisted centrality rows missing/unusable; loadBearing omitted, rules authority factor neutral; repair `ee graph centrality-refresh --workspace .` |
| `primer_budget_floor` | info | bd-39tzu.2 — ADR 0065 §6: proportional shrink hit the rules floor; lower-priority items evicted |

#### AGENTS.md bridge (3)
| Code | Severity | Bead |
|------|----------|------|
| `agentsmd_file_missing` | info | bd-39tzu.4 — ADR 0065 §6: bridge target absent and `--create` not passed; honest file_missing status, never invents a file |
| `agentsmd_markers_missing` | info | bd-39tzu.4 — ADR 0065 §6: file exists without a managed block (import-only file or first export); export appends the block, import parses the whole file |
| `agentsmd_unmanaged_edit_detected` | warning | bd-39tzu.4 — ADR 0065 §6: managed-block hash mismatch (hand-edited); export refuses without `--force-managed-block`, the edit is preserved in the `.ee-backup` sibling |

#### Output-token governor (4)
| Code | Severity | Bead |
|------|----------|------|
| `output_truncated_budget` | info | bd-7lvbg.2 — ADR 0063 §5: trailing whole elements dropped at the schema's declared truncation point to satisfy `--max-output-tokens`; carries `details.droppedCount` + `details.continuationCursor` (`ee.cursor.v1`) |
| `output_budget_unsatisfiable` | medium | bd-7lvbg.2 — ADR 0063 §5: the envelope minimum (or a schema with no declared truncation point) exceeds the ceiling; the response fails closed with a minimal identifying payload |
| `cursor_stale` | low | bd-7lvbg.2 — ADR 0063 §5: cursor `dbGeneration` < current workspace generation; honest pagination requires partitioning one generation's result set; repair: re-run without `--cursor`. Wired (bd-7lvbg.3) on schema list, search, memory list, insights, curate candidates, pack, and audit timeline; a rejected cursor yields an empty page plus this entry, never a restarted page |
| `cursor_invalid` | low | bd-7lvbg.2 — ADR 0063 §5: cursor MAC failure, `paramsHash` mismatch, future generation, dishonest `positionKey`/`droppedCount`, or legacy format (including pre-migration bespoke audit-timeline offset cursors); repair: re-run without `--cursor`. Wired (bd-7lvbg.3) on schema list, search, memory list, insights, curate candidates, pack, and audit timeline; a rejected cursor yields an empty page plus this entry, never a restarted page |

#### Feedback (5)
| Code | Severity | Bead |
|------|----------|------|
| `anti_pattern_proposed` | info | bd-17c65.14.12 (N12) |
| `feedback_health_unavailable` | low | bd-17c65.10.6 (J6) |
| `feedback_protected_rules_unavailable` | medium | bd-17c65.10.6 (J6) |
| `feedback_quarantine_unavailable` | medium | bd-17c65.10.6 (J6) |
| `harmful_burst_quarantine` | warning | bd-3qs2i.3.1 (F3) |

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

#### Preflight + quarantine (12)
| Code | Severity | Bead |
|------|----------|------|
| `agent_contract_source_unavailable` | warning | bd-3d6ko.1 (AOP1) |
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

#### Discoverability + usage (2)
| Code | Severity | Bead |
|------|----------|------|
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

#### Maintenance + steward (18)
| Code | Severity | Bead |
|------|----------|------|
| `cusum_baseline_underpowered` | info | bd-17c65.14.13 (N13) |
| `cusum_regime_change_detected` | warning | bd-17c65.14.13 (N13) |
| `decay_sweep_database_missing` | high | bd-17c65.12.4 (L3) |
| `decay_sweep_database_open_failed` | high | bd-17c65.12.4 (L3) |
| `decay_sweep_database_unresolved` | medium | bd-17c65.12.4 (L3) |
| `decay_sweep_handler_failed` | high | bd-17c65.12.4 (L3) |
| `decay_sweep_item_limit_too_large` | low | bd-17c65.12.4 (L3) |
| `decay_sweep_migration_failed` | high | bd-17c65.12.4 (L3) |
| `decay_sweep_workspace_unresolved` | medium | bd-17c65.12.4 (L3) |
| `learn_decay_config_invalid` | medium | bd-17c65.12.4 (L3) |
| `learn_decay_config_read_failed` | medium | bd-17c65.12.4 (L3) |
| `learn_gaps_no_miss_data` | info | bd-3ap2m.3 (M) |
| `learn_gaps_retention_short` | info | bd-3ap2m.3 (M) |
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

#### Concurrency + write owner (10)
| Code | Severity | Bead |
|------|----------|------|
| `advisory_lock_timeout` | medium | bd-3usjw.57 |
| `audit_backpressure` | warning | bd-wp5ac.1 |
| `audit_lane_shutdown_drain_timeout` | medium | bd-wp5ac.1 |
| `daemon_overloaded` | warning | bd-jnyui — bounded `ee daemon` accept loop refuses excess connections to bound peak RSS amplification |
| `index_publish_lock_contention` | warning | bd-17c65.12.2 (L1) |
| `write_owner_busy` | warning | bd-17c65.12.2 (L1) |
| `write_spool_backpressure` | warning | bd-17c65.12.2 (L1) |
| `write_queue_full` | low | bd-17c65.12.2 (L1) |
| `write_hot_path_cancelled_before_commit` | medium | bd-2lsxf.2.4 (SRR3) |
| `write_hot_path_fsync_failure` | high | bd-2lsxf.2.4 (SRR3) |

#### Other (6)
| Code | Severity | Bead |
|------|----------|------|
| `graph_feature_disabled` | medium | bd-17c65.5.3 (E3) — different from build-time `graph_unavailable`; this is a per-call disable |
| `insights_section_unavailable` | info | bd-113r0, retired by bd-2pos6.4 — historical registered `ee insights` metadata-only builder code |
| `singleflight_follower_timeout` | medium | bd-gni47.3 (SF3) |
| `singleflight_leader_failed` | medium | bd-gni47.3 (SF3) |
| `singleflight_state_poisoned` | high | bd-gni47.3 (SF3) |
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

#### Tailscale peer autodiscovery (5)
| Code | Severity | Bead |
|------|----------|------|
| `tailscale_peer_probe_timeout` | warning | bd-36bbk.1.2 |
| `no_ee_peers_on_tailnet` | info | bd-36bbk.1.2 |
| `tailscale_peer_list_unavailable` | warning | bd-36bbk.1.2 |
| `peer_discovery_workspace_mismatch` | info | bd-36bbk.1.2 |
| `peer_discovery_budget_exhausted` | warning | bd-36bbk.1.2 |

#### Mesh hello negotiation (1)
| Code | Severity | Bead |
|------|----------|------|
| `unsupported_protocol_version` | medium | bd-97rgf.5 (SRR6.27) |

#### Mesh discovery, policy, and body fetch (7)
| Code | Severity | Bead |
|------|----------|------|
| `discovery_policy_no_ee_mesh_tag` | info | bd-36bbk.1.7 |
| `discovery_policy_empty_allowlist` | info | bd-36bbk.1.7 |
| `auto_enrollment_no_eligible_peers` | info | bd-36bbk.1.3 |
| `auto_enrollment_partial_failure` | warning | bd-36bbk.1.3 |
| `auto_enrollment_tailnet_changed` | medium | bd-36bbk.1.3 |
| `auto_enrollment_manual_config_present` | medium | bd-36bbk.1.3 |
| `auto_enrollment_manual_migration_unmatched_peer_set` | info | bd-36bbk.1.3 |
| `auto_enrollment_blocked_by_policy` | medium | bd-36bbk.1 |
| `auto_enrollment_already_complete` | info | bd-36bbk.1 |
| `auto_enrollment_concurrent_attempt` | warning | bd-36bbk.1 |
| `auto_enrollment_audit_failed` | high | bd-36bbk.1 |
| `auto_enrollment_sync_once_failed` | warning | bd-36bbk.1 |
| `auto_enrollment_invalid_override_node_key` | warning | bd-36bbk.1 |
| `auto_enrollment_node_key_changed` | medium | bd-36bbk.1 (SRR6.46.3/.4/.14) |
| `mesh_peer_policy_denied` | high | (TBD) |
| `mesh_body_fetch_denied_by_policy` | medium | bd-nw0v3.1 (SRR6.16) |
| `mesh_remote_body_unavailable` | medium | bd-nw0v3.2 (SRR6.16) |
| `mesh_cached_body_hash_mismatch` | high | bd-nw0v3.3 (SRR6.16) |
| `mesh_secret_export_denied` | high | (TBD) |

#### Mesh foreground sync (3)
| Code | Severity | Bead |
|------|----------|------|
| `mesh_sync_supervisor_backpressure` | info | bd-1ylr3 (SRR6.10) |
| `mesh_sync_supervisor_budget_exhausted` | warning | bd-1ylr3 (SRR6.10) |
| `mesh_sync_supervisor_runtime_error` | warning | bd-1ylr3 (SRR6.10) |
| `mesh_audit_ledger_corrupt` | critical | (TBD) |
| `mesh_audit_ledger_missing` | high | (TBD) |
| `mesh_cursor_repair_required` | critical | (TBD) |
| `mesh_event_quarantined` | high | (TBD) |
| `subscribe_cursor_stale` | warning | (TBD) |

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

#### Graph accretion sentinels (11 — response_time variants of graph_unavailable)
| Code | Severity | Bead |
|------|----------|------|
| `graph_ppr_snapshot_stale` | medium | bd-bife.6 |
| `graph_ppr_empty_seed_set` | low | bd-bife.6 |
| `graph_pack_dna_no_dominator` | low | bd-bife.6 |
| `graph_pack_dna_timeout` | low | bd-1prrl.8.4 |
| `graph_causal_no_evidence` | low | bd-bife.6 |
| `graph_health_no_contradictions` | info | bd-bife.6 |
| `graph_curate_disconnected_graph` | warning | bd-bife.6 |
| `graph_proximity_unreachable` | info | bd-bife.6 |
| `graph_dominance_no_revision_chain` | info | bd-bife.6 |
| `graph_skyline_degenerate_communities` | info | bd-bife.6 |
| `graph_hits_convergence_failure` | warning | bd-bife.6 |

#### Graph NUMA pinning (4 — response_time)

Surfaced under `data.graph.numaPin` (`ee.status.graph.numa_pin.v1`).
The scaffold codes are scaffolded by bd-ldstd and consumed by the wiring
slices under bd-1prrl.3 (swarmx.4); `numa_unavailable_on_macos` is the
macOS platform fallback emitted by the `NumaPinningAdapter` trait
(bd-1prrl.3). See `docs/agent-ux/numa-pin.md`.

| Code | Severity | Bead |
|------|----------|------|
| `numa_pin_disabled` | info | bd-ldstd (swarmx.4 scaffold) |
| `numa_pin_linux_not_implemented` | info | bd-ldstd (swarmx.4 scaffold) |
| `numa_pin_unsupported_platform` | info | bd-ldstd (swarmx.4 scaffold) |
| `numa_unavailable_on_macos` | info | bd-1prrl.3 (macOS platform fallback) |

#### Lexical RAM-tier warmload (4 — response_time)

Surfaced under `data.search.lexicalRamTier`
(`ee.status.search.lexical_ram_tier.v1`). The live V1 contract is
process-local heap warmload rather than OS-level pinning; see
`docs/architecture/lexical-ram-tier.md`.

| Code | Severity | Bead |
|------|----------|------|
| `lexical_hugepages_unavailable` | info | bd-1hvzh (bd-21xbi scaffold) |
| `lexical_ram_tier_heap_warmload` | info | bd-21xbi.2 |
| `lexical_ram_tier_disabled` | info | bd-1hvzh (bd-21xbi scaffold) |
| `lexical_ram_unavailable_on_macos` | info | bd-21xbi.2 |

#### Daemon UDS RPC (9 — response_time)

The `ee daemon` hot-mode UDS RPC skeleton (bd-oja31 / SRR1) emits most of
these codes from the per-connection dispatcher (`src/daemon/server.rs`) and
the `ee daemon stop` CLI handler (`src/cli/mod.rs`). The wire envelope
(`ee.daemon.response.v1`) carries no `repair` field; the CLI client maps
the daemon-side codes onto the canonical `degraded[]` array on fallback.
`daemon_socket_unavailable` is emitted CLI-side with a repair hint. The
bounded-pool `daemon_overloaded` and peer-credential
`daemon_peer_unauthorized` codes are catalogued under their own concurrency
and security rows respectively. `daemon_ann_warmload_not_yet_implemented`
is the historical bd-oja31 context-stub code retained only in the failure-mode
catalog for archived daemon traces and older clients; current
`ee.daemon.context` dispatch executes the canonical pack path instead, and no
production source should declare a `*_NOT_YET_IMPLEMENTED_CODE` sentinel for
the closed daemon hot-mode surface.

| Code | Severity | Bead |
|------|----------|------|
| `daemon_ann_warmload_not_yet_implemented` | medium | bd-oja31 |
| `daemon_handler_panic` | high | bd-b82q4 |
| `daemon_method_unauthorized` | high | bd-3mbao |
| `daemon_request_decode_failed` | medium | bd-oja31 |
| `daemon_request_schema_mismatch` | medium | bd-oja31 |
| `daemon_setsockopt_failed` | high | bd-3pnno (SRR1) |
| `daemon_shutting_down` | medium | bd-36dp2 (SRR1) |
| `daemon_socket_unavailable` | info | bd-oja31 (bd-1feff emission wiring) |
| `daemon_unknown_method` | medium | bd-oja31 |

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

#### Coordination / external tools (32)
| Code | Severity | Bead |
|------|----------|------|
| `agent_mail_unavailable` | medium | bd-2nkbn (Agent Mail resilience) |
| `agent_mail_archive_degraded` | warning | bd-1zb7k.11 (P) |
| `agent_mail_semantic_readiness_failed` | warning | bd-2s48u |
| `agent_status_unavailable` | low | (TBD) |
| `beads_command_timeout` | warning | bd-2z5ly.9.3 (S) |
| `beads_no_output` | warning | bd-2z5ly.9.3 (S) |
| `beads_tracker_metadata_drift` | warning | bd-2glil |
| `beads_tracker_stale` | warning | bd-1zb7k.13.3 (C3) |
| `beads_unavailable` | medium | bd-1zb7k.4 (S3) |
| `bv_command_timeout` | warning | bd-2z5ly.10 (S) |
| `bv_no_output` | warning | bd-2z5ly.10 (S) |
| `bv_unavailable` | medium | bd-1zb7k.4 (S3) |
| `git_unavailable` | warning | bd-1zb7k.4 (S3), bd-1eq3l.11 |
| `git_not_repository` | medium | bd-1eq3l.11 |
| `rch_remote_required_fallback_prevented` | warning | bd-1zb7k.13.4 (C4) |
| `rch_unavailable` | low | bd-1zb7k.5 (S4) |
| `rch_verify_client_daemon_version_skew` | warning | bd-37ugy (RCH) |
| `rch_verify_remote_transport_timeout` | warning | bd-37ugy (RCH) |
| `rch_verify_worker_health_threshold_blocked` | warning | bd-37ugy (RCH) |
| `rch_worker_topology_blocked` | warning | bd-1zb7k.13.4 (C4) |
| `workspace_hygiene_agent_mail_timeout` | warning | bd-1eq3l.11 |
| `workspace_hygiene_agent_mail_unavailable` | warning | bd-1eq3l.11 |
| `workspace_hygiene_beads_content_not_provided` | low | bd-1eq3l.4 |
| `workspace_hygiene_beads_db_divergence_unknown` | low | bd-1eq3l.4 |
| `workspace_hygiene_beads_jsonl_truncated` | warning | bd-1eq3l.4 |
| `workspace_hygiene_beads_parse_error` | medium | bd-1eq3l.11 |
| `workspace_hygiene_beads_reserved` | warning | bd-1eq3l.11 |
| `workspace_hygiene_beads_self_reservation` | info | bd-1eq3l.4 |
| `workspace_hygiene_beads_unavailable` | medium | bd-1eq3l.11 |
| `workspace_hygiene_config_invalid` | medium | bd-1eq3l.11 |
| `workspace_hygiene_output_truncated` | warning | bd-1eq3l.11 |
| `workspace_hygiene_partial_metadata` | warning | bd-1eq3l.11 |
| `workspace_hygiene_secret_scan_skipped` | medium | bd-1eq3l.11 |
| `beads_jsonl_partial_write_transient` | low | (TBD) |

#### Model registry / science (7)
| Code | Severity | Bead |
|------|----------|------|
| `model_registry_empty` | low | bd-17c65.10.6 (J6) |
| `model_registry_no_available_entry` | medium | bd-17c65.10.6 (J6) |
| `rerank_model_corrupt` | high | bd-17c65.14.8 (N8) |
| `rerank_model_missing` | warning | bd-17c65.14.8 (N8) |
| `science_backend_unavailable` | medium | bd-17c65.11.7 (K7) |
| `science_budget_exceeded` | warning | bd-17c65.11.7 (K7) |
| `science_input_too_large` | warning | bd-17c65.11.7 (K7) |
| `science_not_compiled` | medium | bd-17c65.11.7 (K7) |

#### Clustering (2 — distinct from G5 sufficiency signals)
| Code | Severity | Bead |
|------|----------|------|
| `clustering_no_candidates` | info | bd-17c65.7.5 (G5) |
| `clustering_no_embeddings` | info | bd-17c65.7.5 (G5) |

#### Miscellaneous (16)
| Code | Severity | Bead |
|------|----------|------|
| `action_override_not_actionable` | low | (TBD) |
| `advisory_memory` | info | (TBD) — advisory-memory presence marker |
| `degraded_context` | info | bd-17c65.5.2 (E2) — retired tombstone for legacy meta-signal; context emits concrete degraded[] entries instead |
| `dry_run_recommended` | info | (TBD) |
| `fixture_tier_mismatch` | low | (TBD) |
| `heavy_gates_skipped` | info | (TBD) |
| `index_locked` | medium | bd-17c65.10.6 (J6) |
| `lab_counterfactual_multi_swap_unsupported` | medium | bd-17c65.14.15.6 (N15.5) — multi-swap rejected by ADR 0028 |
| `lab_replay_determinism_violation` | high | bd-17c65.14.15.5 (N15.4) — same-query replay hash differs from frozen capture |
| `lab_replay_nondeterministic` | high | bd-17c65.14.15.5 (N15.4) — --verify-determinism replay runs diverged |
| `lab_replay_unavailable` | medium | bd-17c65.14.15.5 (N15.4) — runtime missing-evidence degradation when frozen replay artifacts are absent or untrusted |
| `legacy_memory` | info | (TBD) — legacy import marker |
| `manual_heavy_strategy` | warning | bd-17c65.10.6 (J6) |
| `profile_mismatch` | medium | bd-17c65.10.6 (J6) |
| `profile_missing` | medium | bd-17c65.10.6 (J6) |
| `redaction_pattern_matched` | medium | bd-17c65.11.6 (K6) — emitted per redaction event |
| `redaction_level_invalid` | low | bd-17c65.11.6 (K6) — error envelope; bad --redaction value |
| `redaction_round_trip_marker_preserved` | info | bd-17c65.11.6 (K6) — import surfaces preserved markers |
| `redaction_uncertain` | warning | bd-17c65.11.6 (K6) |
| `derived_asset_corrupt` | high | bd-17c65.12.6 (derived backup assets) |
| `derived_asset_hash_mismatch` | high | bd-1nxz4.2 (content-addressed derived asset store) |
| `derived_asset_schema_mismatch` | high | bd-1nxz4.2 (content-addressed derived asset store) |
| `semantic_dimension_exceeds_budget` | medium | bd-17c65.10.6 (J6) — composes with semantic-model gating |
| `tombstone_visibility_unavailable` | medium | bd-17c65.2.8 (B8) |
| `tripwire_inputs_incomplete` | warning | bd-17c65.10.6 (J6) |
| `unknown_method` | medium | (TBD) |
| `unsupported_artifact_kind` | high | bd-17c65.10.6 (J6) |
| `unsupported_condition` | warning | bd-17c65.10.6 (J6) |
| `unsupported_schema` | high | bd-17c65.10.6 (J6) |
| `windows_appdata_unavailable` | medium | bd-3usjw.68 |
| `workspace_nested_markers` | warning | bd-17c65.12.2 (L1) |
| `workspace_symlink_refused` | medium | bd-2bbtw |
| `ambiguous_containing_symbols` | warning | (TBD) |
| `load_bearing_tombstone_requires_override` | medium | (TBD) |
| `unattributed_compile_blocker` | low | (TBD) |

#### Focus suggest (7 — `response_time`)
| Code | Severity | Bead |
|------|----------|------|
| `workspace_uninitialized` | warning | bd-1idcb (G) — `.ee/ee.db` missing; repair: `ee init --workspace .` |
| `no_recent_evidence` | info | bd-1idcb (G) — no memories within `--recent-hours`; repair: widen window or `ee remember` |
| `task_frame_no_evidence` | warning | bd-1idcb (G) — `--task-frame` resolved a frame whose `evidence_links[]` has no `kind == "memory"` entries; explicit scope honored with empty result |
| `task_frame_unavailable` | warning | bd-1idcb (G) — `--task-frame <id>` could not be resolved; surface falls back to unscoped recent set |
| `graph_pagerank_failed` | warning | bd-1idcb (G) — PageRank algorithm returned an error; centrality contribution forced to zero |
| `graph_empty` | info | bd-1idcb (G) — memory graph projection has no nodes; centrality contribution is zero by construction |
| `graph_projection_failed` | warning | bd-1idcb (G) — `build_memory_graph` returned an error; centrality contribution unavailable |

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
