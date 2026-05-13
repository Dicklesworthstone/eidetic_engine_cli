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

## Full code inventory (100 codes)

> Sources of truth: `tests/fixtures/failure_modes/README.md` for the
> agent-facing catalog AND the union of `pub const *_CODE` constants
> + `"code": "..."` JSON literals in `src/`. When either source gains
> a new code, add a row here in the same commit. The
> `tests/degraded_code_taxonomy_consistency_test.rs` enforces this.

### `build_time` (9 codes — migration targets for E5)

| Code | Surface | Feature flag | Notes |
|------|---------|--------------|-------|
| `agent_detection_unavailable` | agent sources, doctor | (binary detection logic) | Reflects compile-time exclusion of agent-detection paths. |
| `daemon_background_mode_unimplemented` | serve | (daemon background-mode build) | Background daemon mode not built; foreground still works. |
| `diagram_backend_unavailable` | doctor, dependency contract | (mermaid renderer feature) | Mermaid backend not linked. |
| `lexical_unavailable` | search | `frankensearch/lexical` | BM25 arm disabled at build. |
| `mcp_unavailable` | doctor, dependency contract | `mcp` | MCP adapter feature off. |
| `runtime_unavailable` | status, doctor | `asupersync` | Runtime feature off (defensive; should never fire in a real build). |
| `search_unimplemented` | status | `frankensearch` core feature | Whole search subsystem disabled. |
| `storage_unimplemented` | status | `fsqlite` core feature | Whole storage subsystem disabled. |
| `toon_unavailable` | status, doctor | TOON renderer feature | TOON format renderer not linked. |

### `mixed` (3 codes — feature + state)

| Code | Surface | Notes |
|------|---------|-------|
| `cass_unavailable` | doctor, import cass | Build-time: `cass` not on PATH at install. Response-time: PATH check fails per call. After E5, presence in capabilities.available[]; per-call resolution failure stays in degraded[]. |
| `graph_unavailable` | doctor, diag graph | Build-time: `fnx-*` feature. Response-time: snapshot generation failed. Split per E5. |
| `search_unavailable` | doctor, dependency contract | Build-time: `frankensearch`. Response-time: index manifest missing. Split per E5. |

### `response_time` (88 codes — stay in `degraded[]`)

#### Search and pack quality (15)
| Code | Severity (canonical) | Bead |
|------|----------------------|------|
| `context_evidence_freshness_changed_source` | info | bd-17c65.1.2 (A2) |
| `context_profile_budget_capped` | info | bd-17c65.2.4 (B7) |
| `duplicates_collapsed` | low | bd-17c65.2.3 (B3) |
| `expired_filtered` | low | bd-17c65.2.8 (B8) |
| `future_validity_filtered` | low | bd-17c65.2.10 (B11) |
| `index_corrupt` | high | bd-17c65.2.1 (B1) |
| `index_missing` | medium | bd-17c65.2.1 (B1) |
| `index_stale` | high | bd-17c65.2.1 (B1) |
| `low_recall_after_floor` | info | bd-17c65.2.1 (B1) |
| `malformed_validity_filtered` | medium | bd-17c65.2.10 (B11) |
| `no_relevant_results` | medium | bd-17c65.2.1 (B1) |
| `profile_search_limit_capped` | low | bd-17c65.2.4 (B7) |
| `source_mode_fallback` | warning | bd-17c65.2.6 (B6) |
| `stale_validity_filtered` | low | bd-17c65.2.10 (B11) |
| `tombstoned_filtered` | low | bd-17c65.2.8 (B8) |
| `tombstoned_in_results` | low | bd-17c65.2.8 (B8) |
| `validity_filtered_significant_recall_drop` | warning | bd-17c65.2.10 (B11) |
| `weak_query_recall` | low | bd-17c65.2.5 (B5) |
| `search_index_stale` | medium | bd-17c65.2.1 (B1) |
| `search_index_degraded` | medium | bd-17c65.10.6 (J6) |

#### Storage and runtime state (8)
| Code | Severity | Bead |
|------|----------|------|
| `search_not_inspected` | low | bd-17c65.10.6 (J6) |
| `search_not_ready` | medium | bd-17c65.10.6 (J6) |
| `search_waiting_for_storage` | medium | bd-17c65.10.6 (J6) |
| `storage_degraded` | medium | bd-17c65.10.6 (J6) |
| `storage_not_inspected` | low | bd-17c65.10.6 (J6) |
| `storage_not_initialized` | medium | bd-17c65.10.6 (J6) |
| `storage_not_ready` | medium | bd-17c65.10.6 (J6) |
| `memory_health_unavailable` | low | bd-17c65.10.6 (J6) |

#### Policy and detector (3)
| Code | Severity | Bead |
|------|----------|------|
| `policy_bypass_used` | info | bd-17c65.3.2 (C2) |
| `policy_secret_detected_with_offsets` | medium | bd-17c65.3.4 (C4) |
| `policy_tag_rejected_with_details` | low | bd-17c65.3.4 (C4) |

#### Learn / curate (12)
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
| `remember_auto_link_failed` | low | bd-17c65.7.3 (G3) |
| `remember_link_suggestion_failed` | low | bd-17c65.7.3 (G3) |

#### Feedback (3)
| Code | Severity | Bead |
|------|----------|------|
| `feedback_health_unavailable` | low | bd-17c65.10.6 (J6) |
| `feedback_protected_rules_unavailable` | medium | bd-17c65.10.6 (J6) |
| `feedback_quarantine_unavailable` | medium | bd-17c65.10.6 (J6) |

#### Why / pack inspection (4)
| Code | Severity | Bead |
|------|----------|------|
| `graph_memory_not_in_snapshot` | low | bd-17c65.10.6 (J6) |
| `graph_query_relative_features_unavailable` | low | bd-17c65.10.6 (J6) |
| `verification_evidence_not_found` | low | bd-1zb7k.3 (S2) |
| `why_pack_selection_unavailable` | low | bd-17c65.10.6 (J6) |
| `why_result_target_unsupported_source` | medium | bd-17c65.10.6 (J6) |

#### Preflight + quarantine (3)
| Code | Severity | Bead |
|------|----------|------|
| `preflight_evidence_stale` | warning | bd-17c65.10.6 (J6) |
| `preflight_evidence_unavailable` | medium | bd-17c65.10.6 (J6) |
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

#### Maintenance + steward (10)
| Code | Severity | Bead |
|------|----------|------|
| `decay_sweep_database_missing` | high | bd-17c65.12.4 (L3) |
| `decay_sweep_database_open_failed` | high | bd-17c65.12.4 (L3) |
| `decay_sweep_database_unresolved` | medium | bd-17c65.12.4 (L3) |
| `decay_sweep_handler_failed` | high | bd-17c65.12.4 (L3) |
| `decay_sweep_item_limit_too_large` | low | bd-17c65.12.4 (L3) |
| `decay_sweep_migration_failed` | high | bd-17c65.12.4 (L3) |
| `decay_sweep_workspace_unresolved` | medium | bd-17c65.12.4 (L3) |
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

#### Concurrency + write owner (3)
| Code | Severity | Bead |
|------|----------|------|
| `index_publish_lock_contention` | warning | bd-17c65.12.2 (L1) |
| `write_owner_busy` | warning | bd-17c65.12.2 (L1) |
| `write_spool_backpressure` | warning | bd-17c65.12.2 (L1) |

#### Other (3)
| Code | Severity | Bead |
|------|----------|------|
| `graph_feature_disabled` | medium | bd-17c65.5.3 (E3) — different from build-time `graph_unavailable`; this is a per-call disable |
| `situation_decisioning_unavailable` | medium | (TBD) |
| `test_degraded` | info | testing harness (synthetic; not emitted in production paths) |

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

#### Graph snapshot (4 — response_time variants of graph_unavailable)
| Code | Severity | Bead |
|------|----------|------|
| `graph_snapshot_missing` | medium | bd-17c65.5.3 (E3) |
| `graph_snapshot_stale` | medium | bd-17c65.5.3 (E3) |
| `graph_snapshot_scores_unavailable` | low | bd-17c65.5.3 (E3) |
| `graph_snapshot_topology_unavailable` | low | bd-17c65.5.3 (E3) |
| `graph_snapshot_unusable` | high | bd-17c65.5.3 (E3) |

#### Integrity / schema (7)
| Code | Severity | Bead |
|------|----------|------|
| `integrity_database_missing` | high | bd-17c65.12.2 (L1) |
| `integrity_database_open_failed` | high | bd-17c65.12.2 (L1) |
| `integrity_provenance_sample_unavailable` | low | bd-17c65.12.2 (L1) |
| `integrity_reference_check_unavailable` | medium | bd-17c65.12.2 (L1) |
| `integrity_reference_issues` | medium | bd-17c65.12.2 (L1) |
| `integrity_schema_check_unavailable` | medium | bd-17c65.12.2 (L1) |
| `integrity_schema_migration_required` | high | bd-17c65.12.5 (L4) |
| `stale_schema_version` | high | bd-17c65.12.5 (L4) |
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

#### Coordination / external tools (6)
| Code | Severity | Bead |
|------|----------|------|
| `agent_mail_unavailable` | medium | bd-2nkbn (Agent Mail resilience) |
| `agent_status_unavailable` | low | (TBD) |
| `beads_unavailable` | medium | bd-1zb7k.4 (S3) |
| `bv_unavailable` | medium | bd-1zb7k.4 (S3) |
| `git_unavailable` | medium | bd-1zb7k.4 (S3) |
| `rch_unavailable` | low | bd-1zb7k.5 (S4) |

#### Model registry / science (5)
| Code | Severity | Bead |
|------|----------|------|
| `model_registry_empty` | medium | (TBD) |
| `model_registry_no_available_entry` | high | (TBD) |
| `science_backend_unavailable` | medium | bd-17c65.11.7 (K7) |
| `science_budget_exceeded` | warning | bd-17c65.11.7 (K7) |
| `science_input_too_large` | warning | bd-17c65.11.7 (K7) |
| `science_not_compiled` | medium | bd-17c65.11.7 (K7) |

#### Clustering (2 — distinct from G5 sufficiency signals)
| Code | Severity | Bead |
|------|----------|------|
| `clustering_no_candidates` | info | bd-17c65.7.5 (G5) |
| `clustering_no_embeddings` | info | bd-17c65.7.5 (G5) |

#### Miscellaneous (11)
| Code | Severity | Bead |
|------|----------|------|
| `action_override_not_actionable` | low | (TBD) |
| `advisory_memory` | info | (TBD) — advisory-memory presence marker |
| `degraded_context` | info | (TBD) — meta-signal for any non-empty degraded[] |
| `dry_run_recommended` | info | (TBD) |
| `fixture_tier_mismatch` | low | (TBD) |
| `heavy_gates_skipped` | info | (TBD) |
| `index_locked` | warning | bd-17c65.12.2 (L1) |
| `lab_replay_unavailable` | medium | bd-17c65.14.15.5 (N15.4) — slated for retirement once N15 lands |
| `legacy_memory` | info | (TBD) — legacy import marker |
| `manual_heavy_strategy` | info | (TBD) |
| `mcp_feature_disabled` | medium | bd-17c65.10.6 (J6) — distinct from build-time `mcp_unavailable`; this is a per-call disable |
| `profile_mismatch` | medium | (TBD) |
| `profile_missing` | low | (TBD) |
| `redaction_uncertain` | warning | bd-17c65.11.6 (K6) |
| `semantic_dimension_exceeds_budget` | warning | (TBD) — composes with semantic-model gating |
| `tombstone_visibility_unavailable` | medium | bd-17c65.2.8 (B8) |
| `tripwire_inputs_incomplete` | low | (TBD) |
| `unknown_method` | medium | (TBD) |
| `unsupported_artifact_kind` | medium | (TBD) |
| `unsupported_condition` | medium | (TBD) |
| `unsupported_schema` | medium | (TBD) |
| `workspace_nested_markers` | warning | bd-17c65.12.2 (L1) |

#### Mixed: storage_unavailable
| Code | Severity | Bead |
|------|----------|------|
| `storage_unavailable` | medium | bd-17c65.10.6 (J6) — also classified in mixed table above; appears as response_time when storage feature is built |

## Capabilities surface (post-E5 target)

When E5 (`bd-17c65.5.5`) lands, the build-time and mixed codes move to:

```json
"capabilities": {
  "unimplemented": [
    {
      "code": "lexical_unavailable",
      "feature_flag": "frankensearch/lexical",
      "tracking_bead": "bd-17c65.5.5",
      "user_message": "Lexical BM25 search is disabled in this build."
    },
    { /* ... other 7 build_time codes ... */ }
  ],
  "available": [
    { "name": "semantic_search", "feature_flag": "frankensearch/model2vec" },
    { "name": "graph_compute",  "feature_flag": "fnx-runtime" },
    /* ... presence markers including the mixed-code build-time half ... */
  ]
}
```

Until E5 lands, `build_time` and `mixed` codes still appear in
`data.degraded[]`. After E5, only `response_time` codes (and the
response-time half of mixed) appear in `data.degraded[]`.

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
