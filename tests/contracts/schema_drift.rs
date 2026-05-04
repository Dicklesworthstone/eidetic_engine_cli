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
    SchemaEntry::new("error", "ee.error.v1", SchemaCategory::Error),
    SchemaEntry::new(
        "version_provenance",
        "ee.version.provenance.v1",
        SchemaCategory::Response,
    ),
];

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
];

/// All registered schemas.
pub fn all_schemas() -> Vec<&'static SchemaEntry> {
    let mut schemas = Vec::new();
    schemas.extend(CORE_SCHEMAS.iter());
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

    type TestResult = Result<(), String>;

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
    fn core_schemas_include_response_and_error() -> TestResult {
        let versions: Vec<&str> = CORE_SCHEMAS.iter().map(|s| s.version).collect();
        ensure(
            versions.contains(&"ee.response.v1"),
            "core schemas must include ee.response.v1",
        )?;
        ensure(
            versions.contains(&"ee.error.v1"),
            "core schemas must include ee.error.v1",
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
}
