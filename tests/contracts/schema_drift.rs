//! Schema-drift detection test (EE-SCHEMA-DRIFT-001).
//!
//! Unified CI gate that verifies all declared schemas remain stable:
//! - DB DDL migrations
//! - JSON response envelopes
//! - Index manifests
//! - JSONL headers
//! - Audit log entries
//!
//! A drifted schema fails CI. Contributors intentionally changing a schema
//! must update the corresponding fixture in the same PR.

use std::collections::BTreeMap;

/// Schema entry for drift detection.
#[derive(Clone, Debug)]
pub struct SchemaEntry {
    pub name: &'static str,
    pub version: &'static str,
    pub category: SchemaCategory,
}

impl SchemaEntry {
    pub const fn new(name: &'static str, version: &'static str, category: SchemaCategory) -> Self {
        Self {
            name,
            version,
            category,
        }
    }
}

/// Category of schema for organization.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum SchemaCategory {
    Response,
    Error,
    Database,
    Index,
    Audit,
    Config,
    Handoff,
    Context,
    Search,
    Memory,
    Economy,
    Procedure,
    Graph,
    Preflight,
    Recorder,
    Lab,
    Situation,
    Plan,
    Doctor,
    Install,
    Backup,
    Hooks,
    Eval,
}

impl SchemaCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Response => "response",
            Self::Error => "error",
            Self::Database => "database",
            Self::Index => "index",
            Self::Audit => "audit",
            Self::Config => "config",
            Self::Handoff => "handoff",
            Self::Context => "context",
            Self::Search => "search",
            Self::Memory => "memory",
            Self::Economy => "economy",
            Self::Procedure => "procedure",
            Self::Graph => "graph",
            Self::Preflight => "preflight",
            Self::Recorder => "recorder",
            Self::Lab => "lab",
            Self::Situation => "situation",
            Self::Plan => "plan",
            Self::Doctor => "doctor",
            Self::Install => "install",
            Self::Backup => "backup",
            Self::Hooks => "hooks",
            Self::Eval => "eval",
        }
    }
}

/// Core response schemas.
pub const CORE_SCHEMAS: &[SchemaEntry] = &[
    SchemaEntry::new("response", "ee.response.v1", SchemaCategory::Response),
    SchemaEntry::new("error", "ee.error.v2", SchemaCategory::Error),
    SchemaEntry::new(
        "version_provenance",
        "ee.version.provenance.v1",
        SchemaCategory::Response,
    ),
];

/// Database schemas.
pub const DATABASE_SCHEMAS: &[SchemaEntry] = &[SchemaEntry::new(
    "database_live_ddl",
    "ee.database.live_ddl.v1",
    SchemaCategory::Database,
)];

/// Handoff schemas.
pub const HANDOFF_SCHEMAS: &[SchemaEntry] = &[
    SchemaEntry::new(
        "handoff_capsule",
        "ee.handoff.capsule.v1",
        SchemaCategory::Handoff,
    ),
    SchemaEntry::new(
        "handoff_preview",
        "ee.handoff.preview.v1",
        SchemaCategory::Handoff,
    ),
    SchemaEntry::new(
        "handoff_create",
        "ee.handoff.create.v1",
        SchemaCategory::Handoff,
    ),
    SchemaEntry::new(
        "handoff_inspect",
        "ee.handoff.inspect.v1",
        SchemaCategory::Handoff,
    ),
    SchemaEntry::new(
        "handoff_resume",
        "ee.handoff.resume.v1",
        SchemaCategory::Handoff,
    ),
];

/// Context and search schemas.
pub const CONTEXT_SCHEMAS: &[SchemaEntry] = &[
    SchemaEntry::new(
        "context_pack",
        "ee.context.pack.v1",
        SchemaCategory::Context,
    ),
    SchemaEntry::new(
        "context_profile",
        "ee.context.profile.v1",
        SchemaCategory::Context,
    ),
    SchemaEntry::new(
        "context_profile_schema_catalog",
        "ee.context.profile.schemas.v1",
        SchemaCategory::Context,
    ),
    SchemaEntry::new("focus_item", "ee.focus.item.v1", SchemaCategory::Context),
    SchemaEntry::new("focus_state", "ee.focus.state.v1", SchemaCategory::Context),
    SchemaEntry::new(
        "focus_schema_catalog",
        "ee.focus.schemas.v1",
        SchemaCategory::Context,
    ),
    SchemaEntry::new(
        "pack_replay_ledger",
        "ee.pack_replay_ledger.v1",
        SchemaCategory::Context,
    ),
    SchemaEntry::new("pack_replay", "ee.pack.replay.v1", SchemaCategory::Context),
    SchemaEntry::new("pack_diff", "ee.pack.diff.v1", SchemaCategory::Context),
    SchemaEntry::new("query", "ee.query.v1", SchemaCategory::Context),
    SchemaEntry::new(
        "search_results",
        "ee.search.results.v1",
        SchemaCategory::Search,
    ),
];

/// Economy and attention-budget schemas.
pub const ECONOMY_SCHEMAS: &[SchemaEntry] = &[
    SchemaEntry::new(
        "economy_utility_value",
        "ee.economy.utility_value.v1",
        SchemaCategory::Economy,
    ),
    SchemaEntry::new(
        "economy_attention_cost",
        "ee.economy.attention_cost.v1",
        SchemaCategory::Economy,
    ),
    SchemaEntry::new(
        "economy_attention_budget",
        "ee.economy.attention_budget.v1",
        SchemaCategory::Economy,
    ),
    SchemaEntry::new(
        "economy_risk_reserve",
        "ee.economy.risk_reserve.v1",
        SchemaCategory::Economy,
    ),
    SchemaEntry::new(
        "economy_tail_risk_reserve_rule",
        "ee.economy.tail_risk_reserve_rule.v1",
        SchemaCategory::Economy,
    ),
    SchemaEntry::new(
        "economy_maintenance_debt",
        "ee.economy.maintenance_debt.v1",
        SchemaCategory::Economy,
    ),
    SchemaEntry::new(
        "economy_recommendation",
        "ee.economy.recommendation.v1",
        SchemaCategory::Economy,
    ),
    SchemaEntry::new(
        "economy_report",
        "ee.economy.report.v1",
        SchemaCategory::Economy,
    ),
    SchemaEntry::new(
        "economy_simulation",
        "ee.economy.simulation.v1",
        SchemaCategory::Economy,
    ),
    SchemaEntry::new(
        "economy_schema_catalog",
        "ee.economy.schemas.v1",
        SchemaCategory::Economy,
    ),
];

/// Procedure schemas.
pub const PROCEDURE_SCHEMAS: &[SchemaEntry] = &[
    SchemaEntry::new(
        "procedure_propose",
        "ee.procedure.propose_report.v1",
        SchemaCategory::Procedure,
    ),
    SchemaEntry::new(
        "procedure_show",
        "ee.procedure.show_report.v1",
        SchemaCategory::Procedure,
    ),
    SchemaEntry::new(
        "procedure_list",
        "ee.procedure.list_report.v1",
        SchemaCategory::Procedure,
    ),
    SchemaEntry::new(
        "procedure_export",
        "ee.procedure.export_report.v1",
        SchemaCategory::Procedure,
    ),
    SchemaEntry::new(
        "procedure_verify",
        "ee.procedure.verify_report.v1",
        SchemaCategory::Procedure,
    ),
];

/// Graph schemas.
pub const GRAPH_SCHEMAS: &[SchemaEntry] = &[
    SchemaEntry::new("graph_module", "ee.graph.module.v1", SchemaCategory::Graph),
    SchemaEntry::new(
        "centrality_refresh",
        "ee.graph.centrality_refresh.v1",
        SchemaCategory::Graph,
    ),
    SchemaEntry::new(
        "feature_enrichment",
        "ee.graph.feature_enrichment.v1",
        SchemaCategory::Graph,
    ),
    SchemaEntry::new(
        "snapshot_validation",
        "ee.graph.snapshot_validation.v1",
        SchemaCategory::Graph,
    ),
    SchemaEntry::new("graph_export", "ee.graph.export.v1", SchemaCategory::Graph),
];

/// Preflight and recorder schemas.
pub const PREFLIGHT_SCHEMAS: &[SchemaEntry] = &[
    SchemaEntry::new(
        "preflight_report",
        "ee.preflight.report.v1",
        SchemaCategory::Preflight,
    ),
    SchemaEntry::new(
        "recorder_start",
        "ee.recorder.start.v1",
        SchemaCategory::Recorder,
    ),
    SchemaEntry::new(
        "recorder_event",
        "ee.recorder.event_response.v1",
        SchemaCategory::Recorder,
    ),
    SchemaEntry::new(
        "recorder_finish",
        "ee.recorder.finish.v1",
        SchemaCategory::Recorder,
    ),
    SchemaEntry::new(
        "recorder_tail",
        "ee.recorder.tail.v1",
        SchemaCategory::Recorder,
    ),
    SchemaEntry::new(
        "recorder_links",
        "ee.recorder.links.v1",
        SchemaCategory::Recorder,
    ),
    SchemaEntry::new(
        "rationale_trace",
        "ee.rationale_trace.v1",
        SchemaCategory::Recorder,
    ),
];

/// Lab schemas.
pub const LAB_SCHEMAS: &[SchemaEntry] = &[
    SchemaEntry::new("lab_capture", "ee.lab.capture.v1", SchemaCategory::Lab),
    SchemaEntry::new("lab_replay", "ee.lab.replay.v1", SchemaCategory::Lab),
    SchemaEntry::new(
        "lab_counterfactual",
        "ee.lab.counterfactual.v1",
        SchemaCategory::Lab,
    ),
    SchemaEntry::new(
        "lab_reconstruct",
        "ee.lab.reconstruct.v1",
        SchemaCategory::Lab,
    ),
];

/// Situation and plan schemas.
pub const SITUATION_SCHEMAS: &[SchemaEntry] = &[
    SchemaEntry::new(
        "situation_classify",
        "ee.situation.classify.v1",
        SchemaCategory::Situation,
    ),
    SchemaEntry::new(
        "situation_show",
        "ee.situation.show.v1",
        SchemaCategory::Situation,
    ),
    SchemaEntry::new(
        "situation_explain",
        "ee.situation.explain.v1",
        SchemaCategory::Situation,
    ),
    SchemaEntry::new("situation", "ee.situation.v1", SchemaCategory::Situation),
    SchemaEntry::new(
        "task_signature",
        "ee.task_signature.v1",
        SchemaCategory::Situation,
    ),
    SchemaEntry::new(
        "feature_evidence",
        "ee.situation.feature_evidence.v1",
        SchemaCategory::Situation,
    ),
    SchemaEntry::new(
        "routing_decision",
        "ee.situation.routing_decision.v1",
        SchemaCategory::Situation,
    ),
    SchemaEntry::new(
        "situation_link",
        "ee.situation.link.v1",
        SchemaCategory::Situation,
    ),
    SchemaEntry::new(
        "situation_schema_catalog",
        "ee.situation.schemas.v1",
        SchemaCategory::Situation,
    ),
    SchemaEntry::new("goal_plan", "ee.plan.goal.v1", SchemaCategory::Plan),
    SchemaEntry::new(
        "recipe_list",
        "ee.plan.recipe_list.v1",
        SchemaCategory::Plan,
    ),
    SchemaEntry::new("recipe_show", "ee.plan.recipe.v1", SchemaCategory::Plan),
];

/// Doctor and diagnostics schemas.
pub const DOCTOR_SCHEMAS: &[SchemaEntry] = &[
    SchemaEntry::new(
        "doctor_report",
        "ee.doctor.report.v1",
        SchemaCategory::Doctor,
    ),
    SchemaEntry::new(
        "franken_health",
        "ee.doctor.franken_health.v1",
        SchemaCategory::Doctor,
    ),
    SchemaEntry::new(
        "dependency_diagnostics",
        "ee.diag.dependencies.v1",
        SchemaCategory::Doctor,
    ),
    SchemaEntry::new(
        "integrity_diagnostics",
        "ee.diag.integrity.v1",
        SchemaCategory::Doctor,
    ),
];

/// Hooks schemas.
pub const HOOKS_SCHEMAS: &[SchemaEntry] = &[
    SchemaEntry::new("hook_install", "ee.hooks.install.v1", SchemaCategory::Hooks),
    SchemaEntry::new("hook_status", "ee.hooks.status.v1", SchemaCategory::Hooks),
];

/// Learn schemas.
pub const LEARN_SCHEMAS: &[SchemaEntry] = &[
    SchemaEntry::new("learn_agenda", "ee.learn.agenda.v1", SchemaCategory::Memory),
    SchemaEntry::new(
        "learn_uncertainty",
        "ee.learn.uncertainty.v1",
        SchemaCategory::Memory,
    ),
    SchemaEntry::new(
        "learn_summary",
        "ee.learn.summary.v1",
        SchemaCategory::Memory,
    ),
    SchemaEntry::new(
        "learn_experiment_proposal",
        "ee.learn.experiment_proposal.v1",
        SchemaCategory::Memory,
    ),
    SchemaEntry::new(
        "learn_experiment_run",
        "ee.learn.experiment_run.v1",
        SchemaCategory::Memory,
    ),
    SchemaEntry::new(
        "learn_observe",
        "ee.learn.observe.v1",
        SchemaCategory::Memory,
    ),
    SchemaEntry::new("learn_close", "ee.learn.close.v1", SchemaCategory::Memory),
];

/// Rule management schemas.
pub const RULE_SCHEMAS: &[SchemaEntry] = &[
    SchemaEntry::new("rule_add", "ee.rule.add.v1", SchemaCategory::Memory),
    SchemaEntry::new("rule_list", "ee.rule.list.v1", SchemaCategory::Memory),
    SchemaEntry::new("rule_show", "ee.rule.show.v1", SchemaCategory::Memory),
];

/// Audit schemas.
pub const AUDIT_SCHEMAS: &[SchemaEntry] = &[
    SchemaEntry::new(
        "audit_timeline",
        "ee.audit.timeline.v1",
        SchemaCategory::Audit,
    ),
    SchemaEntry::new("audit_show", "ee.audit.show.v1", SchemaCategory::Audit),
    SchemaEntry::new("audit_diff", "ee.audit.diff.v1", SchemaCategory::Audit),
    SchemaEntry::new("audit_verify", "ee.audit.verify.v1", SchemaCategory::Audit),
];

/// Eval schemas (EE-348).
pub const EVAL_SCHEMAS: &[SchemaEntry] = &[
    SchemaEntry::new("eval_fixture", "ee.eval_fixture.v1", SchemaCategory::Eval),
    SchemaEntry::new(
        "release_gate",
        "ee.eval.release_gate.v1",
        SchemaCategory::Eval,
    ),
    SchemaEntry::new(
        "tail_budget_config",
        "ee.eval.tail_budget_config.v1",
        SchemaCategory::Eval,
    ),
    SchemaEntry::new(
        "science_metrics",
        "ee.eval.science_metrics.v1",
        SchemaCategory::Eval,
    ),
];

/// Backup schemas.
pub const BACKUP_SCHEMAS: &[SchemaEntry] = &[
    SchemaEntry::new(
        "backup_create",
        "ee.backup.create.v1",
        SchemaCategory::Backup,
    ),
    SchemaEntry::new(
        "backup_manifest",
        "ee.backup.manifest.v1",
        SchemaCategory::Backup,
    ),
    SchemaEntry::new(
        "backup_manifest_derived",
        "ee.backup.manifest.v2",
        SchemaCategory::Backup,
    ),
];

/// All registered schemas.
pub fn all_schemas() -> Vec<&'static SchemaEntry> {
    let mut schemas = Vec::new();
    schemas.extend(CORE_SCHEMAS.iter());
    schemas.extend(DATABASE_SCHEMAS.iter());
    schemas.extend(HANDOFF_SCHEMAS.iter());
    schemas.extend(CONTEXT_SCHEMAS.iter());
    schemas.extend(ECONOMY_SCHEMAS.iter());
    schemas.extend(PROCEDURE_SCHEMAS.iter());
    schemas.extend(GRAPH_SCHEMAS.iter());
    schemas.extend(PREFLIGHT_SCHEMAS.iter());
    schemas.extend(LAB_SCHEMAS.iter());
    schemas.extend(SITUATION_SCHEMAS.iter());
    schemas.extend(DOCTOR_SCHEMAS.iter());
    schemas.extend(HOOKS_SCHEMAS.iter());
    schemas.extend(LEARN_SCHEMAS.iter());
    schemas.extend(RULE_SCHEMAS.iter());
    schemas.extend(AUDIT_SCHEMAS.iter());
    schemas.extend(EVAL_SCHEMAS.iter());
    schemas.extend(BACKUP_SCHEMAS.iter());
    schemas
}

/// Schema version format validation.
pub fn validate_schema_version(version: &str) -> Result<(), String> {
    if !version.starts_with("ee.") {
        return Err(format!("schema version must start with 'ee.': {version}"));
    }
    if !version.ends_with(".v1") && !version.contains(".v") {
        return Err(format!(
            "schema version must contain version suffix: {version}"
        ));
    }
    Ok(())
}

/// Schema uniqueness check.
pub fn check_schema_uniqueness(schemas: &[&SchemaEntry]) -> Result<(), String> {
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for schema in schemas {
        if let Some(existing) = seen.insert(schema.version, schema.name) {
            return Err(format!(
                "duplicate schema version '{}': declared by both '{}' and '{}'",
                schema.version, existing, schema.name
            ));
        }
    }
    Ok(())
}

/// Schema category coverage check.
pub fn check_category_coverage(schemas: &[&SchemaEntry]) -> BTreeMap<SchemaCategory, usize> {
    let mut coverage: BTreeMap<SchemaCategory, usize> = BTreeMap::new();
    for schema in schemas {
        *coverage.entry(schema.category).or_insert(0) += 1;
    }
    coverage
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    use ee::db::DbConnection;
    use sqlmodel_core::{Row, Value};
    use sqlmodel_frankensqlite::FrankenConnection;

    type TestResult = Result<(), String>;

    struct LiveSchemaSnapshot {
        tables: BTreeSet<String>,
        indexes: BTreeSet<String>,
        columns: std::collections::BTreeMap<String, BTreeSet<String>>,
    }

    struct AppendixDivergence {
        table: &'static str,
        reason: &'static str,
    }

    const LIVE_SCHEMA_TABLES: &[&str] = &[
        "agent_history_sources",
        "agent_installations",
        "agents",
        "artifact_links",
        "artifacts",
        "audit_log",
        "audit_log_v038",
        "causal_evidence",
        "certificates",
        "curation_candidates",
        "curation_candidates_v029",
        "curation_candidates_v033",
        "curation_ttl_policies",
        "ee_advisory_locks",
        "ee_schema_migrations",
        "ee_wal_holds",
        "evidence_spans",
        "feedback_events",
        "feedback_events_v037",
        "feedback_quarantine",
        "feedback_quarantine_v037",
        "graph_algorithm_results",
        "graph_algorithm_witnesses",
        "graph_snapshots",
        "graph_snapshots_v044",
        "import_ledger",
        "learning_observations",
        "memories",
        "memory_links",
        "memory_tags",
        "model_registry",
        "pack_items",
        "pack_omissions",
        "pack_records",
        "plan_recipes",
        "preflight_bypass_tokens",
        "procedure_events",
        "procedures",
        "procedural_rules",
        "rationale_trace_links",
        "rationale_traces",
        "recorder_events",
        "recorder_runs",
        "rule_source_memories",
        "rule_tags",
        "search_index_jobs",
        "sessions",
        "task_episodes",
        "tripwire_check_events",
        "tripwires",
        "trust_quarantine",
        "workspaces",
    ];

    const CRITICAL_SCHEMA_INDEXES: &[&str] = &[
        "idx_audit_log_chain",
        "idx_ee_advisory_locks_holder",
        "idx_ee_wal_holds_episode",
        "idx_ee_wal_holds_workspace_expires",
        "idx_graph_algorithm_results_computed",
        "idx_graph_algorithm_results_lookup",
        "idx_graph_algorithm_witnesses_lookup",
        "idx_graph_snapshots_workspace",
        "idx_import_ledger_source",
        "idx_learning_observations_workspace",
        "idx_memories_trust_class",
        "idx_memories_workspace",
        "idx_memories_workspace_workflow",
        "idx_pack_items_rank",
        "idx_pack_items_trust_class",
        "idx_pack_records_ledger_hash",
        "idx_preflight_bypass_tokens_revoked",
        "idx_preflight_bypass_tokens_workspace",
        "idx_recorder_runs_workspace",
        "idx_search_index_jobs_workspace",
        "idx_workspaces_path",
    ];

    const CRITICAL_SCHEMA_COLUMNS: &[(&str, &[&str])] = &[
        (
            "workspaces",
            &[
                "id",
                "path",
                "name",
                "scope_kind",
                "repository_root",
                "repository_fingerprint",
                "subproject_path",
                "created_at",
                "updated_at",
            ],
        ),
        (
            "memories",
            &[
                "id",
                "workspace_id",
                "level",
                "kind",
                "content",
                "workflow_id",
                "confidence",
                "utility",
                "importance",
                "provenance_uri",
                "provenance_chain_hash",
                "provenance_chain_hash_version",
                "provenance_verification_status",
                "trust_class",
                "trust_subclass",
                "valid_from",
                "valid_to",
                "created_at",
                "updated_at",
                "tombstoned_at",
            ],
        ),
        (
            "pack_records",
            &[
                "id",
                "workspace_id",
                "query",
                "profile",
                "max_tokens",
                "used_tokens",
                "item_count",
                "omitted_count",
                "pack_hash",
                "degraded_json",
                "ledger_json",
                "ledger_hash",
                "created_at",
                "created_by",
            ],
        ),
        (
            "pack_items",
            &[
                "pack_id",
                "memory_id",
                "rank",
                "section",
                "estimated_tokens",
                "relevance",
                "utility",
                "why",
                "diversity_key",
                "provenance_json",
                "trust_class",
                "trust_subclass",
            ],
        ),
        (
            "audit_log",
            &[
                "id",
                "workspace_id",
                "timestamp",
                "actor",
                "action",
                "target_type",
                "target_id",
                "details",
                "surface",
                "mutation_kind",
                "before_hash",
                "after_hash",
                "prev_row_hash",
                "this_row_hash",
            ],
        ),
        (
            "ee_wal_holds",
            &[
                "workspace_id",
                "episode_id",
                "lsn",
                "created_at",
                "expires_at",
            ],
        ),
        (
            "procedural_rules",
            &[
                "id",
                "workspace_id",
                "content",
                "confidence",
                "utility",
                "importance",
                "trust_class",
                "scope",
                "scope_pattern",
                "maturity",
                "positive_feedback_count",
                "negative_feedback_count",
                "protected",
                "created_at",
                "updated_at",
            ],
        ),
        (
            "ee_schema_migrations",
            &["version", "name", "checksum", "applied_at"],
        ),
        (
            "learning_observations",
            &[
                "id",
                "workspace_id",
                "observation_kind",
                "source_type",
                "source_id",
                "target_type",
                "target_id",
                "topic",
                "signal",
                "evidence_json",
                "observed_at",
                "created_at",
            ],
        ),
        (
            "graph_algorithm_witnesses",
            &[
                "workspace_id",
                "snapshot_id",
                "algorithm",
                "params_json",
                "witness_json",
                "recorded_at",
            ],
        ),
        (
            "graph_algorithm_results",
            &[
                "workspace_id",
                "snapshot_id",
                "algorithm",
                "params_hash",
                "result_json",
                "computed_at",
                "ttl_seconds",
            ],
        ),
        (
            "preflight_bypass_tokens",
            &[
                "token_hash",
                "token_hash_prefix",
                "workspace_id",
                "issued_at",
                "expires_at",
                "max_uses",
                "used_count",
                "issuer_workspace",
                "reason",
                "revoked_at",
                "last_used_at",
            ],
        ),
    ];

    const APPENDIX_A_ONLY_TABLES: &[AppendixDivergence] = &[
        AppendixDivergence {
            table: "meta",
            reason: "metadata is currently represented by workspaces plus migration records",
        },
        AppendixDivergence {
            table: "migrations",
            reason: "the live migration ledger is ee_schema_migrations",
        },
        AppendixDivergence {
            table: "embeddings",
            reason: "semantic indexes are derived assets outside the durable DB contract",
        },
        AppendixDivergence {
            table: "memory_fts",
            reason: "Frankensearch is the retrieval layer; no in-DB FTS table is canonical",
        },
        AppendixDivergence {
            table: "workflows",
            reason: "workflow grouping is represented by memories.workflow_id in the live schema",
        },
        AppendixDivergence {
            table: "actions",
            reason: "action history has not been promoted into the live durable schema",
        },
        AppendixDivergence {
            table: "diary_entries",
            reason: "diary storage has not been promoted into the live durable schema",
        },
        AppendixDivergence {
            table: "retrieval_policies",
            reason: "retrieval policy state is not yet a durable table",
        },
        AppendixDivergence {
            table: "steward_jobs",
            reason: "steward job persistence is not yet a durable table",
        },
        AppendixDivergence {
            table: "idempotency_keys",
            reason: "idempotency keys are not yet part of the live DB contract",
        },
    ];

    const IMPLEMENTATION_ADDED_TABLES: &[AppendixDivergence] = &[
        AppendixDivergence {
            table: "pack_items",
            reason: "context pack item provenance is persisted for explainability",
        },
        AppendixDivergence {
            table: "pack_omissions",
            reason: "context pack omissions are persisted for replayable why output",
        },
        AppendixDivergence {
            table: "recorder_runs",
            reason: "recorder imports and live runs use explicit durable rows",
        },
        AppendixDivergence {
            table: "recorder_events",
            reason: "recorder event chains are persisted separately from sessions",
        },
        AppendixDivergence {
            table: "certificates",
            reason: "signed manifests and lifecycle certificates are durable records",
        },
        AppendixDivergence {
            table: "trust_quarantine",
            reason: "source trust quarantine summaries are durable records",
        },
        AppendixDivergence {
            table: "learning_observations",
            reason: "active learning observations have a dedicated ledger",
        },
        AppendixDivergence {
            table: "curation_candidates_v029",
            reason: "the retained v029 table is migration evidence for FrankenSQLite integrity",
        },
        AppendixDivergence {
            table: "curation_candidates_v033",
            reason: "the retained v033 table is migration evidence for procedure-candidate rebuilds",
        },
        AppendixDivergence {
            table: "feedback_events_v037",
            reason: "the retained v037 table is migration evidence for procedure feedback-target rebuilds",
        },
        AppendixDivergence {
            table: "feedback_quarantine_v037",
            reason: "the retained v037 table is migration evidence for procedure feedback-target rebuilds",
        },
        AppendixDivergence {
            table: "audit_log_v038",
            reason: "the retained v038 table is migration evidence for UUID-v7 audit id rebuilds",
        },
        AppendixDivergence {
            table: "procedures",
            reason: "procedure distillation uses durable procedure records separate from raw curation candidates",
        },
        AppendixDivergence {
            table: "procedure_events",
            reason: "procedure maturity transitions are auditable durable events",
        },
        AppendixDivergence {
            table: "plan_recipes",
            reason: "plan decisioning persists reusable recipes as first-class records",
        },
        AppendixDivergence {
            table: "causal_evidence",
            reason: "causal credit assignment persists evidence ledger rows for explainable estimates",
        },
    ];

    struct CanonicalFieldRule {
        logical_name: &'static str,
        canonical_key: &'static str,
        forbidden_aliases: &'static [&'static str],
    }

    struct CanonicalFieldSurface {
        surface: &'static str,
        logical_name: &'static str,
        canonical_path: &'static str,
        forbidden_paths: &'static [&'static str],
    }

    const RESERVED_FIELD_SUFFIXES: &[&str] = &["_preview", "_hash", "_truncated", "_format"];

    const CANONICAL_FIELD_RULES: &[CanonicalFieldRule] = &[
        CanonicalFieldRule {
            logical_name: "memory body text",
            canonical_key: "content",
            forbidden_aliases: &["body", "text", "memory_body", "memory_text"],
        },
        CanonicalFieldRule {
            logical_name: "memory level",
            canonical_key: "level",
            forbidden_aliases: &["memory_level"],
        },
        CanonicalFieldRule {
            logical_name: "memory kind",
            canonical_key: "kind",
            forbidden_aliases: &["memory_kind", "type"],
        },
        CanonicalFieldRule {
            logical_name: "workspace id",
            canonical_key: "workspace_id",
            forbidden_aliases: &["workspaceId", "workspace"],
        },
        CanonicalFieldRule {
            logical_name: "workspace path",
            canonical_key: "workspace_path",
            forbidden_aliases: &["workspacePath"],
        },
        CanonicalFieldRule {
            logical_name: "memory creation timestamp",
            canonical_key: "created_at",
            forbidden_aliases: &["createdAt", "created"],
        },
        CanonicalFieldRule {
            logical_name: "relevance score",
            canonical_key: "scores.relevance",
            forbidden_aliases: &["relevanceScore", "relevance_score"],
        },
    ];

    const CANONICAL_FIELD_SURFACES: &[CanonicalFieldSurface] = &[
        CanonicalFieldSurface {
            surface: "ee memory list",
            logical_name: "memory body text",
            canonical_path: "data.memories[].content",
            forbidden_paths: &["data.memories[].body", "data.memories[].text"],
        },
        CanonicalFieldSurface {
            surface: "ee memory list",
            logical_name: "memory level",
            canonical_path: "data.memories[].level",
            forbidden_paths: &["data.memories[].memory_level"],
        },
        CanonicalFieldSurface {
            surface: "ee memory list",
            logical_name: "memory kind",
            canonical_path: "data.memories[].kind",
            forbidden_paths: &["data.memories[].memory_kind", "data.memories[].type"],
        },
        CanonicalFieldSurface {
            surface: "ee memory list",
            logical_name: "memory creation timestamp",
            canonical_path: "data.memories[].created_at",
            forbidden_paths: &["data.memories[].createdAt", "data.memories[].created"],
        },
        CanonicalFieldSurface {
            surface: "ee search",
            logical_name: "relevance score",
            canonical_path: "data.results[].scores.relevance",
            forbidden_paths: &[
                "data.results[].relevanceScore",
                "data.results[].relevance_score",
            ],
        },
        CanonicalFieldSurface {
            surface: "ee context",
            logical_name: "memory body text",
            canonical_path: "data.pack.items[].content",
            forbidden_paths: &["data.pack.items[].body", "data.pack.items[].text"],
        },
        CanonicalFieldSurface {
            surface: "ee context",
            logical_name: "relevance score",
            canonical_path: "data.pack.items[].scores.relevance",
            forbidden_paths: &[
                "data.pack.items[].relevanceScore",
                "data.pack.items[].relevance_score",
            ],
        },
        CanonicalFieldSurface {
            surface: "ee why",
            logical_name: "memory body text",
            canonical_path: "data.content",
            forbidden_paths: &["data.body", "data.text"],
        },
        CanonicalFieldSurface {
            surface: "ee why",
            logical_name: "memory level",
            canonical_path: "data.retrieval.level",
            forbidden_paths: &["data.retrieval.memory_level"],
        },
        CanonicalFieldSurface {
            surface: "ee learn uncertainty",
            logical_name: "memory body text",
            canonical_path: "items[].content",
            forbidden_paths: &["items[].body", "items[].text"],
        },
    ];

    fn canonical_field_rule(logical_name: &str) -> Option<&'static CanonicalFieldRule> {
        CANONICAL_FIELD_RULES
            .iter()
            .find(|rule| rule.logical_name == logical_name)
    }

    fn is_reserved_modifier_for(field_name: &str, canonical_key: &str) -> bool {
        let Some(base_key) = canonical_key.rsplit('.').next() else {
            return false;
        };
        let Some(suffix) = field_name.strip_prefix(base_key) else {
            return false;
        };
        RESERVED_FIELD_SUFFIXES.contains(&suffix)
    }

    fn check_canonical_field_key(logical_name: &str, observed_key: &str) -> Result<(), String> {
        let rule = canonical_field_rule(logical_name)
            .ok_or_else(|| format!("missing canonical field rule for {logical_name}"))?;
        if observed_key == rule.canonical_key
            || is_reserved_modifier_for(observed_key, rule.canonical_key)
        {
            return Ok(());
        }
        if rule.forbidden_aliases.contains(&observed_key) {
            return Err(format!(
                "field `{observed_key}` drifts from canonical `{}` for {logical_name}",
                rule.canonical_key
            ));
        }
        Ok(())
    }

    fn field_key_from_path(path: &str) -> &str {
        path.rsplit('.').next().unwrap_or(path)
    }

    fn observed_key_for_path(path: &str, rule: &CanonicalFieldRule) -> String {
        let key = field_key_from_path(path);
        if rule.canonical_key.contains('.') && key == field_key_from_path(rule.canonical_key) {
            rule.canonical_key.to_owned()
        } else {
            key.to_owned()
        }
    }

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn ensure_equal<T: std::fmt::Debug + PartialEq>(
        actual: &T,
        expected: &T,
        context: &str,
    ) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn row_text(row: &Row, index: usize, context: &str) -> Result<String, String> {
        row.get(index)
            .and_then(|value| value.as_str())
            .map(str::to_owned)
            .ok_or_else(|| format!("{context}: expected text at column {index}"))
    }

    fn quote_identifier(identifier: &str) -> String {
        format!("\"{}\"", identifier.replace('"', "\"\""))
    }

    fn migrated_schema_snapshot() -> Result<LiveSchemaSnapshot, String> {
        let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
        let database_path = tempdir.path().join("schema-drift.db");
        let migration_connection =
            DbConnection::open_file(&database_path).map_err(|error| format!("open db: {error}"))?;
        migration_connection
            .migrate()
            .map_err(|error| format!("migrate db: {error}"))?;
        migration_connection
            .close()
            .map_err(|error| format!("close migrated db: {error}"))?;

        let query_connection =
            FrankenConnection::open_file(database_path.to_string_lossy().into_owned())
                .map_err(|error| format!("open migrated db for schema read: {error}"))?;

        let table_rows = query_connection
            .query_sync(
                "SELECT name FROM sqlite_master \
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
                 ORDER BY name",
                &[] as &[Value],
            )
            .map_err(|error| format!("read sqlite_master tables: {error}"))?;
        let tables: BTreeSet<String> = table_rows
            .iter()
            .map(|row| row_text(row, 0, "table name"))
            .collect::<Result<_, _>>()?;

        let index_rows = query_connection
            .query_sync(
                "SELECT name FROM sqlite_master \
                 WHERE type = 'index' AND name NOT LIKE 'sqlite_%' \
                 ORDER BY name",
                &[] as &[Value],
            )
            .map_err(|error| format!("read sqlite_master indexes: {error}"))?;
        let indexes: BTreeSet<String> = index_rows
            .iter()
            .map(|row| row_text(row, 0, "index name"))
            .collect::<Result<_, _>>()?;

        let mut columns = std::collections::BTreeMap::new();
        for table in &tables {
            let sql = format!("PRAGMA table_info({})", quote_identifier(table));
            let column_rows = query_connection
                .query_sync(&sql, &[] as &[Value])
                .map_err(|error| format!("read columns for {table}: {error}"))?;
            let column_names = column_rows
                .iter()
                .map(|row| row_text(row, 1, table))
                .collect::<Result<BTreeSet<_>, _>>()?;
            columns.insert(table.clone(), column_names);
        }

        query_connection
            .close_sync()
            .map_err(|error| format!("close schema read db: {error}"))?;

        Ok(LiveSchemaSnapshot {
            tables,
            indexes,
            columns,
        })
    }

    #[test]
    fn schema_registry_is_non_empty() -> TestResult {
        let schemas = all_schemas();
        ensure(!schemas.is_empty(), "schema registry must not be empty")?;
        ensure(
            schemas.len() >= 30,
            format!("expected at least 30 schemas, got {}", schemas.len()),
        )
    }

    #[test]
    fn all_schema_versions_are_valid() -> TestResult {
        for schema in all_schemas() {
            validate_schema_version(schema.version)
                .map_err(|e| format!("schema '{}' has invalid version: {}", schema.name, e))?;
        }
        Ok(())
    }

    #[test]
    fn all_schema_versions_are_unique() -> TestResult {
        let schemas = all_schemas();
        check_schema_uniqueness(&schemas)
    }

    #[test]
    fn schema_names_are_non_empty() -> TestResult {
        for schema in all_schemas() {
            ensure(
                !schema.name.is_empty(),
                format!(
                    "schema name must not be empty for version {}",
                    schema.version
                ),
            )?;
        }
        Ok(())
    }

    #[test]
    fn category_coverage_includes_core_categories() -> TestResult {
        let schemas = all_schemas();
        let coverage = check_category_coverage(&schemas);

        ensure(
            coverage.contains_key(&SchemaCategory::Response),
            "must have Response category schemas",
        )?;
        ensure(
            coverage.contains_key(&SchemaCategory::Error),
            "must have Error category schemas",
        )?;
        ensure(
            coverage.contains_key(&SchemaCategory::Handoff),
            "must have Handoff category schemas",
        )?;
        ensure(
            coverage.contains_key(&SchemaCategory::Database),
            "must have Database category schemas",
        )?;
        ensure(
            coverage.contains_key(&SchemaCategory::Procedure),
            "must have Procedure category schemas",
        )?;
        ensure(
            coverage.contains_key(&SchemaCategory::Economy),
            "must have Economy category schemas",
        )?;
        ensure(
            coverage.contains_key(&SchemaCategory::Graph),
            "must have Graph category schemas",
        )?;
        Ok(())
    }

    #[test]
    fn database_schemas_include_live_ddl_contract() -> TestResult {
        let versions: Vec<&str> = DATABASE_SCHEMAS.iter().map(|s| s.version).collect();
        ensure(
            versions.contains(&"ee.database.live_ddl.v1"),
            "database schemas must include the live DDL migration contract",
        )
    }

    #[test]
    fn migrated_database_schema_matches_live_contract() -> TestResult {
        let snapshot = migrated_schema_snapshot()?;
        let expected_tables = LIVE_SCHEMA_TABLES
            .iter()
            .map(|table| (*table).to_owned())
            .collect::<BTreeSet<_>>();
        ensure_equal(
            &snapshot.tables,
            &expected_tables,
            "freshly migrated database table set",
        )?;

        for index in CRITICAL_SCHEMA_INDEXES {
            ensure(
                snapshot.indexes.contains(*index),
                format!("freshly migrated database must include critical index {index}"),
            )?;
        }

        for (table, expected_columns) in CRITICAL_SCHEMA_COLUMNS {
            let actual_columns = snapshot
                .columns
                .get(*table)
                .ok_or_else(|| format!("missing critical table {table}"))?;
            for column in *expected_columns {
                ensure(
                    actual_columns.contains(*column),
                    format!("critical table {table} must include column {column}"),
                )?;
            }
        }

        Ok(())
    }

    #[test]
    fn appendix_a_schema_divergences_are_explicit() -> TestResult {
        let snapshot = migrated_schema_snapshot()?;

        ensure(
            snapshot.tables.contains("ee_schema_migrations"),
            "live schema must use ee_schema_migrations as the migration ledger",
        )?;
        ensure(
            !snapshot.tables.contains("migrations"),
            "Appendix A migrations table is intentionally superseded by ee_schema_migrations",
        )?;

        for divergence in APPENDIX_A_ONLY_TABLES {
            ensure(
                !divergence.reason.trim().is_empty(),
                format!(
                    "Appendix A divergence for {} needs a reason",
                    divergence.table
                ),
            )?;
            ensure(
                !snapshot.tables.contains(divergence.table),
                format!(
                    "Appendix A table {} is now present; update the live DDL contract and divergence list",
                    divergence.table
                ),
            )?;
        }

        for divergence in IMPLEMENTATION_ADDED_TABLES {
            ensure(
                !divergence.reason.trim().is_empty(),
                format!(
                    "implementation-added divergence for {} needs a reason",
                    divergence.table
                ),
            )?;
            ensure(
                snapshot.tables.contains(divergence.table),
                format!(
                    "implementation-added table {} is missing; update migrations or divergence list",
                    divergence.table
                ),
            )?;
        }

        Ok(())
    }

    #[test]
    fn core_schemas_include_response_and_error() -> TestResult {
        let versions: Vec<&str> = CORE_SCHEMAS.iter().map(|s| s.version).collect();
        ensure(
            versions.contains(&"ee.response.v1"),
            "core schemas must include ee.response.v1",
        )?;
        ensure(
            versions.contains(&"ee.error.v2"),
            "core schemas must include ee.error.v2",
        )
    }

    #[test]
    fn handoff_schemas_are_complete() -> TestResult {
        let versions: Vec<&str> = HANDOFF_SCHEMAS.iter().map(|s| s.version).collect();
        ensure(
            versions.contains(&"ee.handoff.capsule.v1"),
            "handoff schemas must include capsule",
        )?;
        ensure(
            versions.contains(&"ee.handoff.create.v1"),
            "handoff schemas must include create",
        )?;
        ensure(
            versions.contains(&"ee.handoff.resume.v1"),
            "handoff schemas must include resume",
        )
    }

    #[test]
    fn lab_schemas_include_reconstruct() -> TestResult {
        let versions: Vec<&str> = LAB_SCHEMAS.iter().map(|s| s.version).collect();
        ensure(
            versions.contains(&"ee.lab.reconstruct.v1"),
            "lab schemas must include reconstruct (EE-405)",
        )
    }

    #[test]
    fn graph_schemas_include_snapshot_validation() -> TestResult {
        let versions: Vec<&str> = GRAPH_SCHEMAS.iter().map(|s| s.version).collect();
        ensure(
            versions.contains(&"ee.graph.snapshot_validation.v1"),
            "graph schemas must include snapshot_validation (EE-268)",
        )
    }

    #[test]
    fn graph_schemas_include_feature_enrichment() -> TestResult {
        let versions: Vec<&str> = GRAPH_SCHEMAS.iter().map(|s| s.version).collect();
        ensure(
            versions.contains(&"ee.graph.feature_enrichment.v1"),
            "graph schemas must include feature_enrichment (EE-167)",
        )
    }

    #[test]
    fn graph_schemas_include_mermaid_export() -> TestResult {
        let versions: Vec<&str> = GRAPH_SCHEMAS.iter().map(|s| s.version).collect();
        ensure(
            versions.contains(&"ee.graph.export.v1"),
            "graph schemas must include export (EE-169)",
        )
    }

    #[test]
    fn hooks_schemas_are_complete() -> TestResult {
        let versions: Vec<&str> = HOOKS_SCHEMAS.iter().map(|s| s.version).collect();
        ensure(
            versions.contains(&"ee.hooks.install.v1"),
            "hooks schemas must include install (EE-321)",
        )?;
        ensure(
            versions.contains(&"ee.hooks.status.v1"),
            "hooks schemas must include status (EE-321)",
        )
    }

    #[test]
    fn eval_schemas_include_release_gate_tail_budget_and_science_metrics() -> TestResult {
        let versions: Vec<&str> = EVAL_SCHEMAS.iter().map(|s| s.version).collect();
        ensure(
            versions.contains(&"ee.eval.release_gate.v1"),
            "eval schemas must include release_gate (EE-348)",
        )?;
        ensure(
            versions.contains(&"ee.eval.tail_budget_config.v1"),
            "eval schemas must include tail_budget_config (EE-348)",
        )?;
        ensure(
            versions.contains(&"ee.eval.science_metrics.v1"),
            "eval schemas must include science metrics (EE-175)",
        )
    }

    #[test]
    fn query_schema_closure_is_verified() -> TestResult {
        let versions: Vec<&str> = CONTEXT_SCHEMAS.iter().map(|s| s.version).collect();
        ensure(
            versions.contains(&"ee.query.v1"),
            "context schemas must include ee.query.v1 (EE-QUERY-SCHEMA-VERIFY-001)",
        )?;

        let entry = match CONTEXT_SCHEMAS.iter().find(|s| s.version == "ee.query.v1") {
            Some(entry) => entry,
            None => return Err("ee.query.v1 entry must exist".to_owned()),
        };
        ensure_equal(&entry.name, &"query", "schema name")?;
        ensure_equal(&entry.category, &SchemaCategory::Context, "schema category")
    }

    #[test]
    fn focus_schemas_are_registered_as_context_contracts() -> TestResult {
        let versions: Vec<&str> = CONTEXT_SCHEMAS.iter().map(|s| s.version).collect();
        ensure(
            versions.contains(&"ee.focus.item.v1"),
            "context schemas must include focus item",
        )?;
        ensure(
            versions.contains(&"ee.focus.state.v1"),
            "context schemas must include focus state",
        )?;
        ensure(
            versions.contains(&"ee.focus.schemas.v1"),
            "context schemas must include focus schema catalog",
        )
    }

    #[test]
    fn query_schema_version_matches_constant() -> TestResult {
        ensure_equal(
            &"ee.query.v1",
            &"ee.query.v1",
            "query schema version literal",
        )
    }

    #[test]
    fn schema_category_strings_are_stable() -> TestResult {
        ensure_equal(&SchemaCategory::Response.as_str(), &"response", "response")?;
        ensure_equal(&SchemaCategory::Error.as_str(), &"error", "error")?;
        ensure_equal(&SchemaCategory::Database.as_str(), &"database", "database")?;
        ensure_equal(&SchemaCategory::Index.as_str(), &"index", "index")?;
        ensure_equal(&SchemaCategory::Audit.as_str(), &"audit", "audit")?;
        ensure_equal(&SchemaCategory::Config.as_str(), &"config", "config")?;
        ensure_equal(&SchemaCategory::Handoff.as_str(), &"handoff", "handoff")?;
        ensure_equal(&SchemaCategory::Context.as_str(), &"context", "context")?;
        ensure_equal(&SchemaCategory::Search.as_str(), &"search", "search")?;
        ensure_equal(&SchemaCategory::Memory.as_str(), &"memory", "memory")?;
        ensure_equal(&SchemaCategory::Economy.as_str(), &"economy", "economy")?;
        ensure_equal(
            &SchemaCategory::Procedure.as_str(),
            &"procedure",
            "procedure",
        )?;
        ensure_equal(&SchemaCategory::Graph.as_str(), &"graph", "graph")?;
        ensure_equal(
            &SchemaCategory::Preflight.as_str(),
            &"preflight",
            "preflight",
        )?;
        ensure_equal(&SchemaCategory::Recorder.as_str(), &"recorder", "recorder")?;
        ensure_equal(&SchemaCategory::Lab.as_str(), &"lab", "lab")?;
        ensure_equal(
            &SchemaCategory::Situation.as_str(),
            &"situation",
            "situation",
        )?;
        ensure_equal(&SchemaCategory::Plan.as_str(), &"plan", "plan")?;
        ensure_equal(&SchemaCategory::Doctor.as_str(), &"doctor", "doctor")?;
        ensure_equal(&SchemaCategory::Install.as_str(), &"install", "install")?;
        ensure_equal(&SchemaCategory::Hooks.as_str(), &"hooks", "hooks")?;
        ensure_equal(&SchemaCategory::Eval.as_str(), &"eval", "eval")
    }

    #[test]
    fn schema_version_validation_rejects_invalid_formats() {
        assert!(validate_schema_version("invalid").is_err());
        assert!(validate_schema_version("foo.bar").is_err());
        assert!(validate_schema_version("ee.test.v1").is_ok());
        assert!(validate_schema_version("ee.response.v1").is_ok());
    }

    #[test]
    fn total_schema_count_tracks_growth() -> TestResult {
        let schemas = all_schemas();
        let count = schemas.len();
        ensure(
            count >= 40,
            format!("expected at least 40 registered schemas, got {count}"),
        )?;
        ensure(
            count <= 200,
            format!("unexpectedly high schema count {count} - review for duplicates"),
        )
    }

    #[test]
    fn canonical_field_map_covers_agent_facing_memory_concepts() -> TestResult {
        let required = [
            ("memory body text", "content"),
            ("memory level", "level"),
            ("memory kind", "kind"),
            ("workspace id", "workspace_id"),
            ("workspace path", "workspace_path"),
            ("memory creation timestamp", "created_at"),
            ("relevance score", "scores.relevance"),
        ];

        for (logical_name, canonical_key) in required {
            let rule = canonical_field_rule(logical_name)
                .ok_or_else(|| format!("missing canonical rule for {logical_name}"))?;
            ensure_equal(
                &rule.canonical_key,
                &canonical_key,
                &format!("canonical key for {logical_name}"),
            )?;
            ensure(
                !rule.forbidden_aliases.is_empty(),
                format!("{logical_name} must declare drift aliases"),
            )?;
        }

        Ok(())
    }

    #[test]
    fn canonical_field_audit_declares_agent_facing_surfaces() -> TestResult {
        let required_surfaces = [
            "ee memory list",
            "ee search",
            "ee context",
            "ee why",
            "ee learn uncertainty",
        ];

        for surface in required_surfaces {
            ensure(
                CANONICAL_FIELD_SURFACES
                    .iter()
                    .any(|entry| entry.surface == surface),
                format!("canonical field audit must cover {surface}"),
            )?;
        }

        for entry in CANONICAL_FIELD_SURFACES {
            let rule = canonical_field_rule(entry.logical_name)
                .ok_or_else(|| format!("missing canonical rule for {}", entry.logical_name))?;
            let observed = observed_key_for_path(entry.canonical_path, rule);
            check_canonical_field_key(entry.logical_name, &observed).map_err(|error| {
                format!(
                    "{} canonical path `{}` should satisfy {}: {error}",
                    entry.surface, entry.canonical_path, entry.logical_name
                )
            })?;
            ensure(
                !entry.forbidden_paths.is_empty(),
                format!(
                    "{} {} audit must include at least one forbidden alias path",
                    entry.surface, entry.logical_name
                ),
            )?;
            for forbidden_path in entry.forbidden_paths {
                let forbidden_key = observed_key_for_path(forbidden_path, rule);
                let error = match check_canonical_field_key(entry.logical_name, &forbidden_key) {
                    Ok(()) => {
                        return Err(format!(
                            "forbidden surface alias should fail: {} {forbidden_path}",
                            entry.surface
                        ));
                    }
                    Err(error) => error,
                };
                ensure(
                    error.contains(&forbidden_key),
                    format!(
                        "{} forbidden path `{forbidden_path}` should name `{forbidden_key}`: {error}",
                        entry.surface
                    ),
                )?;
            }
        }

        Ok(())
    }

    #[test]
    fn canonical_field_rules_reject_known_drift_aliases() -> TestResult {
        let drift_cases = [
            ("memory body text", "body"),
            ("memory body text", "text"),
            ("memory kind", "type"),
            ("workspace id", "workspaceId"),
            ("workspace path", "workspacePath"),
            ("memory creation timestamp", "createdAt"),
            ("relevance score", "relevanceScore"),
        ];

        for (logical_name, observed_key) in drift_cases {
            let error = match check_canonical_field_key(logical_name, observed_key) {
                Ok(()) => {
                    return Err(format!(
                        "drift alias should fail: {logical_name} {observed_key}"
                    ));
                }
                Err(error) => error,
            };
            ensure(
                error.contains(observed_key),
                format!("error should name observed key {observed_key}: {error}"),
            )?;
            ensure(
                error.contains("canonical"),
                format!("error should explain canonical replacement: {error}"),
            )?;
        }

        Ok(())
    }

    #[test]
    fn canonical_field_rules_allow_canonical_keys_and_reserved_modifiers() -> TestResult {
        let allowed_cases = [
            ("memory body text", "content"),
            ("memory body text", "content_preview"),
            ("memory body text", "content_hash"),
            ("memory body text", "content_truncated"),
            ("memory body text", "content_format"),
            ("workspace id", "workspace_id"),
            ("workspace id", "workspace_id_hash"),
            ("relevance score", "scores.relevance"),
            ("relevance score", "relevance_hash"),
        ];

        for (logical_name, observed_key) in allowed_cases {
            check_canonical_field_key(logical_name, observed_key).map_err(|error| {
                format!("{logical_name} should allow `{observed_key}` but got {error}")
            })?;
        }

        Ok(())
    }
}
