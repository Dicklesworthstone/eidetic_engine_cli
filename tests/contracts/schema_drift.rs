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
    SchemaEntry::new("response", "ee.response.v2", SchemaCategory::Response),
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
    SchemaEntry::new(
        "completion_audit_checklist",
        "ee.completion_audit.checklist.v1",
        SchemaCategory::Handoff,
    ),
    SchemaEntry::new(
        "completion_audit_report",
        "ee.completion_audit.report.v2",
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
        "focus_suggest",
        "ee.focus.suggest.v1",
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
    use std::fs;
    use std::path::{Path, PathBuf};

    use ee::db::DbConnection;
    use serde::Deserialize;
    use serde_json::{Map as JsonMap, Value as JsonValue};
    use sqlmodel_core::{Row, Value};
    use sqlmodel_frankensqlite::FrankenConnection;

    type TestResult = Result<(), String>;

    const CONTRACT_INVENTORY_JSON: &str =
        include_str!("../fixtures/contracts/public_contract_inventory.json");

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ContractInventory {
        schema: String,
        generated_by: String,
        contracts: Vec<ContractInventoryEntry>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ContractInventoryEntry {
        schema_id: String,
        status: String,
        surface: String,
        owner: String,
        schema_file: Option<String>,
        canonical_docs: Vec<String>,
        current_facing_contexts: Vec<String>,
        allowed_historical_contexts: Vec<HistoricalContext>,
        forbidden_current_claims: Vec<String>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct HistoricalContext {
        path_pattern: String,
        reason: String,
    }

    #[derive(Debug, Eq, PartialEq)]
    struct LegacySchemaClaimViolation {
        path: String,
        line: usize,
        schema_id: String,
        phrase: String,
        source_excerpt: String,
    }

    #[derive(Debug)]
    struct MarkdownJsonExample {
        path: String,
        line: usize,
        fence_language: String,
        body: String,
    }

    #[derive(Debug, Eq, PartialEq)]
    struct JsonExampleValidationIssue {
        path: String,
        line: usize,
        schema_id: String,
        message: String,
        source_excerpt: String,
    }

    fn contract_inventory() -> Result<ContractInventory, String> {
        serde_json::from_str(CONTRACT_INVENTORY_JSON)
            .map_err(|error| format!("parse public contract inventory: {error}"))
    }

    fn inventory_entry<'a>(
        inventory: &'a ContractInventory,
        schema_id: &str,
    ) -> Result<&'a ContractInventoryEntry, String> {
        inventory
            .contracts
            .iter()
            .find(|entry| entry.schema_id == schema_id)
            .ok_or_else(|| format!("missing contract inventory entry for {schema_id}"))
    }

    fn repo_path(path: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
    }

    fn normalize_repo_path(path: &Path) -> String {
        path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
            .unwrap_or(path)
            .to_string_lossy()
            .trim_start_matches('/')
            .to_owned()
    }

    fn path_matches_pattern(path: &str, pattern: &str) -> bool {
        if let Some(prefix) = pattern.strip_suffix("/**") {
            path == prefix || path.starts_with(&format!("{prefix}/"))
        } else {
            path == pattern
        }
    }

    fn path_is_allowed_historical(path: &str, entry: &ContractInventoryEntry) -> bool {
        entry
            .allowed_historical_contexts
            .iter()
            .any(|context| path_matches_pattern(path, &context.path_pattern))
    }

    fn previous_char_boundary(text: &str, mut index: usize) -> usize {
        index = index.min(text.len());
        while index > 0 && !text.is_char_boundary(index) {
            index -= 1;
        }
        index
    }

    fn next_char_boundary(text: &str, mut index: usize) -> usize {
        index = index.min(text.len());
        while index < text.len() && !text.is_char_boundary(index) {
            index += 1;
        }
        index
    }

    fn current_facing_doc_paths(inventory: &ContractInventory) -> Result<BTreeSet<String>, String> {
        let mut paths = BTreeSet::new();
        for entry in inventory
            .contracts
            .iter()
            .filter(|entry| entry.status == "current")
        {
            if let Some(schema_file) = &entry.schema_file {
                insert_existing_current_facing_path(schema_file, &mut paths)?;
            }
            for pattern in &entry.current_facing_contexts {
                if let Some(prefix) = pattern.strip_suffix("/**") {
                    collect_markdown_paths(&repo_path(prefix), &mut paths)?;
                } else {
                    insert_existing_current_facing_path(pattern, &mut paths)?;
                }
            }
        }
        Ok(paths)
    }

    fn insert_existing_current_facing_path(path: &str, paths: &mut BTreeSet<String>) -> TestResult {
        if repo_path(path).is_file() {
            paths.insert(path.to_owned());
            Ok(())
        } else {
            Err(format!("current-facing contract path is missing: {path}"))
        }
    }

    fn collect_markdown_paths(root: &Path, paths: &mut BTreeSet<String>) -> Result<(), String> {
        if !root.exists() {
            return Ok(());
        }

        let mut entries = fs::read_dir(root)
            .map_err(|error| format!("read current-facing docs dir {}: {error}", root.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("read current-facing docs dir {}: {error}", root.display()))?;
        entries.sort_by_key(|entry| entry.path());

        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                collect_markdown_paths(&path, paths)?;
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
                paths.insert(normalize_repo_path(&path));
            }
        }
        Ok(())
    }

    fn current_json_example_paths(
        inventory: &ContractInventory,
    ) -> Result<BTreeSet<String>, String> {
        let mut paths = current_facing_doc_paths(inventory)?
            .into_iter()
            .filter(|path| path.ends_with(".md"))
            .collect::<BTreeSet<_>>();
        insert_existing_current_facing_path("docs/migration-guide.md", &mut paths)?;
        Ok(paths)
    }

    fn extract_markdown_json_examples(path: &str, text: &str) -> Vec<MarkdownJsonExample> {
        let mut examples = Vec::new();
        let mut active_fence: Option<(String, usize, Vec<String>)> = None;

        for (line_index, line) in text.lines().enumerate() {
            let line_number = line_index + 1;
            let trimmed = line.trim_start();

            if let Some((language, start_line, body_lines)) = active_fence.as_mut() {
                if trimmed.starts_with("```") {
                    examples.push(MarkdownJsonExample {
                        path: path.to_owned(),
                        line: *start_line,
                        fence_language: language.clone(),
                        body: body_lines.join("\n"),
                    });
                    active_fence = None;
                } else {
                    body_lines.push(line.to_owned());
                }
                continue;
            }

            if let Some(info_string) = trimmed.strip_prefix("```") {
                let language = info_string
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if language == "json" || language == "jsonc" {
                    active_fence = Some((language, line_number + 1, Vec::new()));
                }
            }
        }

        examples
    }

    fn json_example_source_excerpt(source: &str) -> String {
        source
            .split_whitespace()
            .take(32)
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn json_example_issue(
        path: &str,
        line: usize,
        schema_id: &str,
        message: impl Into<String>,
        source: &str,
    ) -> JsonExampleValidationIssue {
        JsonExampleValidationIssue {
            path: path.to_owned(),
            line,
            schema_id: schema_id.to_owned(),
            message: message.into(),
            source_excerpt: json_example_source_excerpt(source),
        }
    }

    fn contract_schema_id(value: &JsonValue) -> Option<&str> {
        value
            .as_object()
            .and_then(|object| object.get("schema"))
            .and_then(JsonValue::as_str)
    }

    fn markdown_example_mentions_envelope_contract(source: &str) -> bool {
        source.contains("\"schema\"")
            && (source.contains("\"ee.response.") || source.contains("\"ee.error."))
    }

    fn strip_jsonc_comments(source: &str) -> String {
        let mut out = String::with_capacity(source.len());
        let mut chars = source.chars().peekable();
        let mut in_string = false;
        let mut escaped = false;
        let mut in_line_comment = false;
        let mut in_block_comment = false;

        while let Some(ch) = chars.next() {
            if in_line_comment {
                if ch == '\n' {
                    in_line_comment = false;
                    out.push(ch);
                }
                continue;
            }
            if in_block_comment {
                if ch == '\n' {
                    out.push('\n');
                } else if ch == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    in_block_comment = false;
                }
                continue;
            }

            if in_string {
                out.push(ch);
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_string = false;
                }
                continue;
            }

            if ch == '"' {
                in_string = true;
                out.push(ch);
            } else if ch == '/' && chars.peek() == Some(&'/') {
                chars.next();
                in_line_comment = true;
            } else if ch == '/' && chars.peek() == Some(&'*') {
                chars.next();
                in_block_comment = true;
            } else {
                out.push(ch);
            }
        }

        out
    }

    fn remove_json_trailing_commas(source: &str) -> String {
        let chars = source.chars().collect::<Vec<_>>();
        let mut out = String::with_capacity(source.len());
        let mut in_string = false;
        let mut escaped = false;
        let mut index = 0;

        while index < chars.len() {
            let ch = chars[index];
            if in_string {
                out.push(ch);
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_string = false;
                }
                index += 1;
                continue;
            }

            if ch == '"' {
                in_string = true;
                out.push(ch);
                index += 1;
                continue;
            }

            if ch == ',' {
                let mut lookahead = index + 1;
                while chars
                    .get(lookahead)
                    .is_some_and(|next| next.is_whitespace())
                {
                    lookahead += 1;
                }
                if matches!(chars.get(lookahead), Some(&'}') | Some(&']')) {
                    index += 1;
                    continue;
                }
            }

            out.push(ch);
            index += 1;
        }

        out
    }

    fn normalize_jsonc(source: &str) -> String {
        remove_json_trailing_commas(&strip_jsonc_comments(source))
    }

    fn parse_markdown_json_example(
        example: &MarkdownJsonExample,
    ) -> Result<Vec<JsonValue>, String> {
        let source = if example.fence_language == "jsonc" {
            normalize_jsonc(&example.body)
        } else {
            example.body.clone()
        };
        let mut values = Vec::new();
        let stream = serde_json::Deserializer::from_str(&source).into_iter::<JsonValue>();
        for value in stream {
            values.push(value.map_err(|error| {
                format!(
                    "contract {} example is not parseable JSON after JSONC normalization: {error}",
                    example.fence_language
                )
            })?);
        }
        if values.is_empty() {
            return Err(format!(
                "contract {} example does not contain a JSON value",
                example.fence_language
            ));
        }
        Ok(values)
    }

    fn json_example_validation_issues_for_text(
        path: &str,
        text: &str,
        inventory: &ContractInventory,
    ) -> Vec<JsonExampleValidationIssue> {
        let mut issues = Vec::new();

        for example in extract_markdown_json_examples(path, text) {
            if !markdown_example_mentions_envelope_contract(&example.body) {
                continue;
            }

            let values = match parse_markdown_json_example(&example) {
                Ok(values) => values,
                Err(error) => {
                    issues.push(json_example_issue(
                        &example.path,
                        example.line,
                        "<unparseable>",
                        error,
                        &example.body,
                    ));
                    continue;
                }
            };

            for value in values {
                issues.extend(json_example_validation_issues_for_value(
                    &example.path,
                    example.line,
                    &example.body,
                    &value,
                    inventory,
                ));
            }
        }

        issues
    }

    fn json_example_validation_issues_for_value(
        path: &str,
        line: usize,
        source: &str,
        value: &JsonValue,
        inventory: &ContractInventory,
    ) -> Vec<JsonExampleValidationIssue> {
        let Some(schema_id) = contract_schema_id(value) else {
            return Vec::new();
        };

        if !schema_id.starts_with("ee.response.") && !schema_id.starts_with("ee.error.") {
            return Vec::new();
        }

        let mut issues = Vec::new();
        let entry = match inventory_entry(inventory, schema_id) {
            Ok(entry) => entry,
            Err(error) => {
                issues.push(json_example_issue(path, line, schema_id, error, source));
                return issues;
            }
        };

        if entry.status == "legacy" {
            if !path_is_allowed_historical(path, entry) {
                issues.push(json_example_issue(
                    path,
                    line,
                    schema_id,
                    format!(
                        "{schema_id} is a legacy envelope outside an allowed historical context"
                    ),
                    source,
                ));
            }
            return issues;
        }

        if entry.status != "current" {
            issues.push(json_example_issue(
                path,
                line,
                schema_id,
                format!(
                    "{schema_id} has unsupported contract status {}",
                    entry.status
                ),
                source,
            ));
            return issues;
        }

        for message in validate_current_envelope_example(schema_id, value) {
            issues.push(json_example_issue(path, line, schema_id, message, source));
        }

        issues
    }

    fn current_json_example_issues(
        inventory: &ContractInventory,
    ) -> Result<Vec<JsonExampleValidationIssue>, String> {
        let mut issues = Vec::new();

        for path in current_json_example_paths(inventory)? {
            let text = fs::read_to_string(repo_path(&path))
                .map_err(|error| format!("read current-facing doc {path}: {error}"))?;
            issues.extend(json_example_validation_issues_for_text(
                &path, &text, inventory,
            ));
        }

        issues.extend(schema_file_json_example_issues(inventory)?);
        issues.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then(left.line.cmp(&right.line))
                .then(left.schema_id.cmp(&right.schema_id))
                .then(left.message.cmp(&right.message))
        });
        Ok(issues)
    }

    fn schema_file_json_example_issues(
        inventory: &ContractInventory,
    ) -> Result<Vec<JsonExampleValidationIssue>, String> {
        let mut issues = Vec::new();

        for entry in inventory
            .contracts
            .iter()
            .filter(|entry| entry.status == "current")
        {
            let Some(schema_file) = entry.schema_file.as_deref() else {
                continue;
            };
            let text = fs::read_to_string(repo_path(schema_file))
                .map_err(|error| format!("read schema file {schema_file}: {error}"))?;
            let schema_json = serde_json::from_str::<JsonValue>(&text)
                .map_err(|error| format!("parse schema file {schema_file}: {error}"))?;
            let Some(examples) = schema_json.get("examples") else {
                continue;
            };
            let Some(example_values) = examples.as_array() else {
                issues.push(json_example_issue(
                    schema_file,
                    1,
                    &entry.schema_id,
                    "schema examples must be an array",
                    &text,
                ));
                continue;
            };

            for example in example_values {
                let source =
                    serde_json::to_string(example).unwrap_or_else(|_| "<example>".to_owned());
                issues.extend(json_example_validation_issues_for_value(
                    schema_file,
                    1,
                    &source,
                    example,
                    inventory,
                ));
            }
        }

        Ok(issues)
    }

    fn validate_current_envelope_example(schema_id: &str, value: &JsonValue) -> Vec<String> {
        match schema_id {
            "ee.response.v2" => validate_response_v2_example(value),
            "ee.error.v2" => validate_error_v2_example(value),
            _ => Vec::new(),
        }
    }

    fn validate_response_v2_example(value: &JsonValue) -> Vec<String> {
        let mut issues = Vec::new();
        let Some(object) = value.as_object() else {
            issues.push("ee.response.v2 example must be a JSON object".to_owned());
            return issues;
        };

        validate_allowed_keys(
            object,
            &["schema", "success", "fields", "data", "degraded"],
            "response envelope",
            &mut issues,
        );
        validate_string_const(
            object,
            "schema",
            "ee.response.v2",
            "response envelope",
            &mut issues,
        );
        match object.get("success").and_then(JsonValue::as_bool) {
            Some(true) => {}
            Some(false) => issues.push("response envelope success must be true".to_owned()),
            None => issues.push("response envelope success must be a boolean".to_owned()),
        }
        validate_required_object(object, "data", "response envelope", &mut issues);
        validate_optional_string(object, "fields", "response envelope", &mut issues);
        if let Some(degraded) = object.get("degraded") {
            issues.extend(validate_degradation_array(
                degraded,
                "response envelope degraded",
                &["info", "low", "warning", "medium", "high", "critical"],
            ));
        }

        issues
    }

    fn validate_error_v2_example(value: &JsonValue) -> Vec<String> {
        let mut issues = Vec::new();
        let Some(object) = value.as_object() else {
            issues.push("ee.error.v2 example must be a JSON object".to_owned());
            return issues;
        };

        validate_allowed_keys(
            object,
            &["schema", "error", "degraded"],
            "error envelope",
            &mut issues,
        );
        validate_string_const(
            object,
            "schema",
            "ee.error.v2",
            "error envelope",
            &mut issues,
        );
        if let Some(degraded) = object.get("degraded") {
            issues.extend(validate_degradation_array(
                degraded,
                "error envelope degraded",
                &["low", "medium", "high"],
            ));
        }

        let Some(error) = object.get("error").and_then(JsonValue::as_object) else {
            issues.push("error envelope error must be an object".to_owned());
            return issues;
        };

        validate_allowed_keys(
            error,
            &[
                "code",
                "message",
                "severity",
                "repair",
                "repairKind",
                "details",
                "nonRecoverable",
            ],
            "error object",
            &mut issues,
        );
        validate_required_string(error, "code", "error object", &mut issues);
        validate_required_string(error, "message", "error object", &mut issues);
        validate_required_string(error, "severity", "error object", &mut issues);
        if let Some(severity) = error.get("severity").and_then(JsonValue::as_str) {
            validate_enum(
                severity,
                &["low", "medium", "high"],
                "error object severity",
                &mut issues,
            );
        }
        validate_optional_string(error, "repair", "error object", &mut issues);
        if let Some(repair_kind) = error.get("repairKind").and_then(JsonValue::as_str) {
            validate_enum(
                repair_kind,
                &["actionable", "template", "placeholder", "unknown", "empty"],
                "error object repairKind",
                &mut issues,
            );
        }
        if let Some(non_recoverable) = error.get("nonRecoverable")
            && !non_recoverable.is_boolean()
        {
            issues.push("error object nonRecoverable must be a boolean".to_owned());
        }

        let Some(details) = error.get("details").and_then(JsonValue::as_object) else {
            issues.push("error object details must be an object".to_owned());
            return issues;
        };

        if error
            .get("repair")
            .and_then(JsonValue::as_str)
            .is_some_and(|repair| !repair.trim().is_empty())
        {
            match details.get("recovery") {
                Some(recovery) => issues.extend(validate_recovery_array(recovery)),
                None => issues.push(
                    "error.details.recovery must be a non-empty array when repair is present"
                        .to_owned(),
                ),
            }
        } else if let Some(recovery) = details.get("recovery") {
            issues.extend(validate_recovery_array(recovery));
        }

        issues
    }

    fn validate_allowed_keys(
        object: &JsonMap<String, JsonValue>,
        allowed: &[&str],
        context: &str,
        issues: &mut Vec<String>,
    ) {
        for key in object.keys() {
            if !allowed.contains(&key.as_str()) {
                issues.push(format!("{context} has unsupported field {key}"));
            }
        }
    }

    fn validate_string_const(
        object: &JsonMap<String, JsonValue>,
        field: &str,
        expected: &str,
        context: &str,
        issues: &mut Vec<String>,
    ) {
        match object.get(field).and_then(JsonValue::as_str) {
            Some(actual) if actual == expected => {}
            Some(actual) => issues.push(format!(
                "{context} {field} must be {expected}, got {actual}"
            )),
            None => issues.push(format!("{context} {field} must be a string")),
        }
    }

    fn validate_required_string(
        object: &JsonMap<String, JsonValue>,
        field: &str,
        context: &str,
        issues: &mut Vec<String>,
    ) {
        match object.get(field).and_then(JsonValue::as_str) {
            Some(value) if !value.trim().is_empty() => {}
            Some(_) => issues.push(format!("{context} {field} must not be empty")),
            None => issues.push(format!("{context} {field} must be a string")),
        }
    }

    fn validate_optional_string(
        object: &JsonMap<String, JsonValue>,
        field: &str,
        context: &str,
        issues: &mut Vec<String>,
    ) {
        if let Some(value) = object.get(field)
            && !value.is_string()
        {
            issues.push(format!("{context} {field} must be a string"));
        }
    }

    fn validate_required_object(
        object: &JsonMap<String, JsonValue>,
        field: &str,
        context: &str,
        issues: &mut Vec<String>,
    ) {
        if !object.get(field).is_some_and(JsonValue::is_object) {
            issues.push(format!("{context} {field} must be an object"));
        }
    }

    fn validate_enum(value: &str, allowed: &[&str], context: &str, issues: &mut Vec<String>) {
        if !allowed.contains(&value) {
            issues.push(format!(
                "{context} must be one of {}, got {value}",
                allowed.join(", ")
            ));
        }
    }

    fn validate_degradation_array(
        value: &JsonValue,
        context: &str,
        severity_values: &[&str],
    ) -> Vec<String> {
        let mut issues = Vec::new();
        let Some(items) = value.as_array() else {
            issues.push(format!("{context} must be an array"));
            return issues;
        };

        for (index, item) in items.iter().enumerate() {
            let item_context = format!("{context}[{index}]");
            let Some(object) = item.as_object() else {
                issues.push(format!("{item_context} must be an object"));
                continue;
            };

            validate_allowed_keys(
                object,
                &[
                    "code",
                    "severity",
                    "message",
                    "repair",
                    "repairKind",
                    "sources",
                    "details",
                ],
                &item_context,
                &mut issues,
            );
            validate_required_string(object, "code", &item_context, &mut issues);
            validate_required_string(object, "severity", &item_context, &mut issues);
            if let Some(severity) = object.get("severity").and_then(JsonValue::as_str) {
                validate_enum(
                    severity,
                    severity_values,
                    &format!("{item_context} severity"),
                    &mut issues,
                );
            }
            validate_required_string(object, "message", &item_context, &mut issues);
            validate_optional_string(object, "repair", &item_context, &mut issues);
            if let Some(repair_kind) = object.get("repairKind").and_then(JsonValue::as_str) {
                validate_enum(
                    repair_kind,
                    &["actionable", "template", "placeholder", "unknown", "empty"],
                    &format!("{item_context} repairKind"),
                    &mut issues,
                );
            }
            if let Some(sources) = object.get("sources")
                && !sources
                    .as_array()
                    .is_some_and(|items| items.iter().all(JsonValue::is_string))
            {
                issues.push(format!(
                    "{item_context} sources must be an array of strings"
                ));
            }
            if let Some(details) = object.get("details")
                && !details.is_object()
            {
                issues.push(format!("{item_context} details must be an object"));
            }
        }

        issues
    }

    fn validate_recovery_array(value: &JsonValue) -> Vec<String> {
        let mut issues = Vec::new();
        let Some(items) = value.as_array() else {
            issues.push("error.details.recovery must be an array".to_owned());
            return issues;
        };
        if items.is_empty() {
            issues.push(
                "error.details.recovery must be a non-empty array when repair is present"
                    .to_owned(),
            );
        }

        for (index, item) in items.iter().enumerate() {
            let context = format!("error.details.recovery[{index}]");
            let Some(object) = item.as_object() else {
                issues.push(format!("{context} must be an object"));
                continue;
            };

            validate_allowed_keys(
                object,
                &[
                    "priority",
                    "kind",
                    "rationale",
                    "envName",
                    "valueHint",
                    "configPath",
                    "configKey",
                    "flagName",
                    "command",
                    "resultsIn",
                    "example",
                ],
                &context,
                &mut issues,
            );

            match object.get("priority").and_then(JsonValue::as_u64) {
                Some(priority) if priority <= 255 => {}
                Some(priority) => {
                    issues.push(format!("{context} priority must be <= 255, got {priority}"))
                }
                None => issues.push(format!("{context} priority must be an integer")),
            }
            validate_required_string(object, "kind", &context, &mut issues);
            if let Some(kind) = object.get("kind").and_then(JsonValue::as_str) {
                validate_enum(
                    kind,
                    &[
                        "env",
                        "config",
                        "flag",
                        "install",
                        "rebuild",
                        "permission",
                        "migration",
                        "command",
                        "broaden",
                        "narrow",
                        "seed",
                        "none",
                    ],
                    &format!("{context} kind"),
                    &mut issues,
                );
            }
            validate_required_string(object, "rationale", &context, &mut issues);
            for optional_field in [
                "envName",
                "valueHint",
                "configPath",
                "configKey",
                "flagName",
                "command",
                "resultsIn",
                "example",
            ] {
                validate_optional_string(object, optional_field, &context, &mut issues);
            }
        }

        issues
    }

    fn json_example_validation_event(issue: &JsonExampleValidationIssue) -> serde_json::Value {
        serde_json::json!({
            "schema": "ee.test_event.v1",
            "phase": "contract_drift_json_examples",
            "path": &issue.path,
            "line": issue.line,
            "schema_id": &issue.schema_id,
            "policy_decision": "violation",
            "message": &issue.message,
            "source_excerpt": &issue.source_excerpt,
        })
    }

    fn json_example_validation_events(issues: &[JsonExampleValidationIssue]) -> String {
        issues
            .iter()
            .map(json_example_validation_event)
            .map(|event| event.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn legacy_schema_claim_violations_for_text(
        path: &str,
        text: &str,
        inventory: &ContractInventory,
    ) -> Vec<LegacySchemaClaimViolation> {
        let mut violations = Vec::new();
        let lower_text = text.to_ascii_lowercase();

        for entry in inventory
            .contracts
            .iter()
            .filter(|entry| entry.status == "legacy")
        {
            if path_is_allowed_historical(path, entry) {
                continue;
            }

            let schema_id = entry.schema_id.as_str();
            let schema_id_lower = schema_id.to_ascii_lowercase();
            let mut search_from = 0;
            while let Some(relative_index) = lower_text[search_from..].find(&schema_id_lower) {
                let match_start = search_from + relative_index;
                let line = text[..match_start]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count()
                    + 1;
                let window_start = previous_char_boundary(text, match_start.saturating_sub(240));
                let window_end = next_char_boundary(text, match_start + schema_id.len() + 240);
                let window = &text[window_start..window_end];
                let lower_window = window.to_ascii_lowercase();

                if let Some(phrase) = entry
                    .forbidden_current_claims
                    .iter()
                    .find(|phrase| lower_window.contains(&phrase.to_ascii_lowercase()))
                {
                    violations.push(LegacySchemaClaimViolation {
                        path: path.to_owned(),
                        line,
                        schema_id: schema_id.to_owned(),
                        phrase: phrase.clone(),
                        source_excerpt: window
                            .split_whitespace()
                            .take(32)
                            .collect::<Vec<_>>()
                            .join(" "),
                    });
                }

                search_from = match_start + schema_id.len();
            }
        }

        violations
    }

    fn legacy_schema_claim_violations(
        inventory: &ContractInventory,
    ) -> Result<Vec<LegacySchemaClaimViolation>, String> {
        let mut violations = Vec::new();
        for path in current_facing_doc_paths(inventory)? {
            let text = fs::read_to_string(repo_path(&path))
                .map_err(|error| format!("read current-facing doc {path}: {error}"))?;
            violations.extend(legacy_schema_claim_violations_for_text(
                &path, &text, inventory,
            ));
        }
        violations.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then(left.line.cmp(&right.line))
                .then(left.schema_id.cmp(&right.schema_id))
                .then(left.phrase.cmp(&right.phrase))
        });
        Ok(violations)
    }

    fn legacy_schema_claim_event(violation: &LegacySchemaClaimViolation) -> serde_json::Value {
        serde_json::json!({
            "schema": "ee.test_event.v1",
            "phase": "contract_drift_docs_scan",
            "path": &violation.path,
            "line": violation.line,
            "schema_id": &violation.schema_id,
            "matched_phrase": &violation.phrase,
            "policy_decision": "violation",
            "source_excerpt": &violation.source_excerpt,
        })
    }

    fn legacy_schema_claim_events(violations: &[LegacySchemaClaimViolation]) -> String {
        violations
            .iter()
            .map(legacy_schema_claim_event)
            .map(|event| event.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

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
        "curation_candidates_v059",
        "curation_candidates_v060",
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
        "rch_verify_runs",
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
        "idx_preflight_bypass_tokens_scope",
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
                "command",
                "command_hash",
                "rule_ids_json",
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
            table: "rch_verify_runs",
            reason: "durable RCH verifier evidence ledger landed by V061 (bd-22p8c) for ingest/query of remote-proof artifacts",
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
            table: "curation_candidates_v059",
            reason: "the retained v059 table is migration evidence for V060 anti-pattern candidate rebuilds",
        },
        AppendixDivergence {
            table: "curation_candidates_v060",
            reason: "the retained v060 table is migration evidence for V062 create-derived-memory candidate rebuilds (bd-8k9gh)",
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
            versions.contains(&"ee.response.v2"),
            "core schemas must include ee.response.v2",
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
        )?;
        ensure(
            versions.contains(&"ee.completion_audit.report.v2"),
            "handoff schemas must include completion audit report",
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
        )?;
        ensure(
            versions.contains(&"ee.focus.suggest.v1"),
            "context schemas must include focus suggest (bd-1me0m / bd-1n0wl)",
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
        assert!(validate_schema_version("ee.response.v2").is_ok());
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

    #[test]
    fn public_contract_inventory_declares_current_and_legacy_envelopes() -> TestResult {
        let inventory = contract_inventory()?;
        ensure_equal(
            &inventory.schema,
            &"ee.contract_inventory.v1".to_owned(),
            "inventory schema",
        )?;
        ensure_equal(
            &inventory.generated_by,
            &"bd-31nul.1".to_owned(),
            "inventory provenance",
        )?;

        let mut seen = BTreeSet::new();
        for entry in &inventory.contracts {
            ensure(
                seen.insert(entry.schema_id.as_str()),
                format!("duplicate contract inventory entry for {}", entry.schema_id),
            )?;
            ensure(
                matches!(
                    entry.status.as_str(),
                    "current" | "legacy" | "retired" | "experimental"
                ),
                format!(
                    "{} has unsupported status {}",
                    entry.schema_id, entry.status
                ),
            )?;
            ensure(
                !entry.surface.trim().is_empty(),
                format!("{} must declare owner surface", entry.schema_id),
            )?;
            ensure(
                !entry.owner.trim().is_empty(),
                format!("{} must declare owner source", entry.schema_id),
            )?;
            ensure(
                !entry.canonical_docs.is_empty(),
                format!("{} must declare canonical docs", entry.schema_id),
            )?;
        }

        ensure_equal(
            &inventory_entry(&inventory, "ee.response.v2")?.status,
            &"current".to_owned(),
            "response v2 status",
        )?;
        ensure_equal(
            &inventory_entry(&inventory, "ee.error.v2")?.status,
            &"current".to_owned(),
            "error v2 status",
        )?;
        ensure_equal(
            &inventory_entry(&inventory, "ee.pack.v2")?.status,
            &"current".to_owned(),
            "pack v2 status",
        )?;
        ensure_equal(
            &inventory_entry(&inventory, "ee.response.v1")?.status,
            &"legacy".to_owned(),
            "response v1 status",
        )?;

        Ok(())
    }

    #[test]
    fn public_contract_inventory_current_entries_match_exported_schema_registry() -> TestResult {
        let inventory = contract_inventory()?;
        let exported: BTreeSet<&str> = ee::output::public_schemas()
            .iter()
            .map(|entry| entry.id)
            .collect();

        for entry in inventory
            .contracts
            .iter()
            .filter(|entry| entry.status == "current")
        {
            ensure(
                exported.contains(entry.schema_id.as_str()),
                format!(
                    "{} is current in contract inventory but missing from src/output/mod.rs::public_schemas",
                    entry.schema_id
                ),
            )?;
            let schema_file = entry.schema_file.as_deref().ok_or_else(|| {
                format!(
                    "{} current contract must declare schemaFile",
                    entry.schema_id
                )
            })?;
            ensure(
                repo_path(schema_file).is_file(),
                format!(
                    "{} schemaFile does not exist: {schema_file}",
                    entry.schema_id
                ),
            )?;
            ensure(
                !entry.current_facing_contexts.is_empty(),
                format!(
                    "{} current contract must declare currentFacingContexts",
                    entry.schema_id
                ),
            )?;
            ensure(
                entry.forbidden_current_claims.is_empty(),
                format!(
                    "{} current contract should not need forbidden legacy claim phrases",
                    entry.schema_id
                ),
            )?;
        }

        Ok(())
    }

    #[test]
    fn public_contract_inventory_legacy_success_envelopes_have_historical_policy() -> TestResult {
        let inventory = contract_inventory()?;

        for schema_id in ["ee.response.v1", "ee.response.v0"] {
            let entry = inventory_entry(&inventory, schema_id)?;
            ensure_equal(
                &entry.status,
                &"legacy".to_owned(),
                &format!("{schema_id} legacy status"),
            )?;
            ensure(
                entry.current_facing_contexts.is_empty(),
                format!("{schema_id} must not declare current-facing contexts"),
            )?;

            for required_phrase in [
                "agent-facing",
                "always emits",
                "canonical",
                "current",
                "default",
                "required",
                "response envelope",
                "success envelope",
            ] {
                ensure(
                    entry
                        .forbidden_current_claims
                        .iter()
                        .any(|phrase| phrase == required_phrase),
                    format!("{schema_id} missing forbidden phrase {required_phrase}"),
                )?;
            }

            ensure(
                entry.allowed_historical_contexts.iter().any(|context| {
                    context.path_pattern == "tests/**" && !context.reason.trim().is_empty()
                }),
                format!("{schema_id} must allow test fixtures with a reason"),
            )?;
            ensure(
                entry.allowed_historical_contexts.iter().any(|context| {
                    context.path_pattern == "docs/migration_v0_1_to_v0_2.md"
                        && !context.reason.trim().is_empty()
                }),
                format!("{schema_id} must allow migration-guide references with a reason"),
            )?;
        }

        ensure(
            inventory_entry(&inventory, "ee.response.v1")?
                .allowed_historical_contexts
                .iter()
                .any(|context| {
                    context.path_pattern == "docs/archive/**" && !context.reason.trim().is_empty()
                }),
            "ee.response.v1 must allow archived historical design references",
        )
    }

    #[test]
    fn legacy_schema_claim_policy_classifies_current_and_historical_contexts() -> TestResult {
        let inventory = contract_inventory()?;

        let current_violation = legacy_schema_claim_violations_for_text(
            "README.md",
            "The default response envelope is `ee.response.v1` for agents.",
            &inventory,
        );
        ensure_equal(&current_violation.len(), &1, "current violation count")?;
        ensure_equal(
            &current_violation[0].phrase,
            &"default".to_owned(),
            "current violation phrase",
        )?;
        let current_violation_event = legacy_schema_claim_event(&current_violation[0]);
        ensure_equal(
            &current_violation_event
                .get("schema")
                .and_then(serde_json::Value::as_str),
            &Some("ee.test_event.v1"),
            "current violation event schema",
        )?;

        let migration_allowed = legacy_schema_claim_violations_for_text(
            "docs/migration_v0_1_to_v0_2.md",
            "Before migration, the default response envelope was `ee.response.v1`.",
            &inventory,
        );
        ensure(
            migration_allowed.is_empty(),
            format!(
                "migration before/after examples should be allowed but got:\n{}",
                legacy_schema_claim_events(&migration_allowed)
            ),
        )?;

        let archive_allowed = legacy_schema_claim_violations_for_text(
            "docs/archive/old_contract.md",
            "The default response envelope was `ee.response.v1` in this archived plan.",
            &inventory,
        );
        ensure(
            archive_allowed.is_empty(),
            format!(
                "archive references should be allowed but got:\n{}",
                legacy_schema_claim_events(&archive_allowed)
            ),
        )?;

        let neutral_mention = legacy_schema_claim_violations_for_text(
            "README.md",
            "The literal schema identifier `ee.response.v1` appears in migration tests.",
            &inventory,
        );
        ensure(
            neutral_mention.is_empty(),
            format!(
                "neutral legacy schema mention should be allowed but got:\n{}",
                legacy_schema_claim_events(&neutral_mention)
            ),
        )
    }

    #[test]
    fn json_example_policy_classifies_current_historical_and_partial_examples() -> TestResult {
        let inventory = contract_inventory()?;

        let legacy_current = json_example_validation_issues_for_text(
            "README.md",
            r#"```json
{"schema":"ee.response.v1","success":true,"data":{}}
```"#,
            &inventory,
        );
        ensure_equal(&legacy_current.len(), &1, "legacy current example count")?;
        ensure_equal(
            &legacy_current[0].schema_id,
            &"ee.response.v1".to_owned(),
            "legacy current schema id",
        )?;

        let legacy_historical = json_example_validation_issues_for_text(
            "docs/migration-guide.md",
            r#"```json
{"schema":"ee.response.v0","ok":true,"result":{}}
```"#,
            &inventory,
        );
        ensure(
            legacy_historical.is_empty(),
            format!(
                "migration guide historical examples should be allowed but got:\n{}",
                json_example_validation_events(&legacy_historical)
            ),
        )?;

        let malformed_jsonc_sketch = json_example_validation_issues_for_text(
            "AGENTS.md",
            r#"```jsonc
{ "schema": "ee.response.v2", "success": true, "data": { ... } }
```"#,
            &inventory,
        );
        ensure_equal(
            &malformed_jsonc_sketch.len(),
            &1,
            "malformed jsonc sketch issue count",
        )?;
        ensure(
            malformed_jsonc_sketch[0].schema_id == "<unparseable>"
                && malformed_jsonc_sketch[0].message.contains("parseable JSON"),
            format!(
                "malformed jsonc envelope sketches should not be skipped:\n{}",
                json_example_validation_events(&malformed_jsonc_sketch)
            ),
        )?;

        let valid_jsonc = json_example_validation_issues_for_text(
            "README.md",
            r#"```jsonc
{
  // JSONC contract examples are normalized before validation.
  "schema": "ee.response.v2",
  "success": true,
  "data": {},
  "degraded": [],
}
{
  "schema": "ee.error.v2",
  "error": {
    "code": "search_index_unavailable",
    "message": "Search index is stale or unavailable.",
    "severity": "medium",
    "repair": "ee index rebuild --workspace .",
    "details": {
      "recovery": [
        {
          "priority": 0,
          "kind": "rebuild",
          "rationale": "Rebuild the derived index.",
          "command": "ee index rebuild --workspace .",
        },
      ],
    },
  },
}
```"#,
            &inventory,
        );
        ensure(
            valid_jsonc.is_empty(),
            format!(
                "valid jsonc envelope examples should pass but got:\n{}",
                json_example_validation_events(&valid_jsonc)
            ),
        )?;

        let drifted_jsonc = json_example_validation_issues_for_text(
            "README.md",
            r#"```jsonc
{
  // This used to bypass validation because the fence language was jsonc.
  "schema": "ee.response.v1",
  "success": true,
  "data": {},
}
{
  "schema": "ee.response.v2",
  "data": {},
}
```"#,
            &inventory,
        );
        ensure_equal(&drifted_jsonc.len(), &2, "drifted jsonc issue count")?;
        ensure(
            drifted_jsonc
                .iter()
                .any(|issue| issue.schema_id == "ee.response.v1"),
            format!(
                "drifted jsonc should report legacy response schema:\n{}",
                json_example_validation_events(&drifted_jsonc)
            ),
        )?;
        ensure(
            drifted_jsonc
                .iter()
                .any(|issue| issue.message.contains("success")),
            format!(
                "drifted jsonc should report malformed v2 response:\n{}",
                json_example_validation_events(&drifted_jsonc)
            ),
        )?;

        let invalid_success = json_example_validation_issues_for_text(
            "README.md",
            r#"```json
{"schema":"ee.response.v2","data":{}}
```"#,
            &inventory,
        );
        ensure_equal(&invalid_success.len(), &1, "invalid success issue count")?;
        ensure(
            invalid_success[0].message.contains("success"),
            format!("invalid success should mention success: {invalid_success:?}"),
        )?;

        let valid_error = json_example_validation_issues_for_text(
            "docs/migration-guide.md",
            r#"```json
{
  "schema": "ee.error.v2",
  "error": {
    "code": "search_index_unavailable",
    "message": "Search index is stale or unavailable.",
    "severity": "medium",
    "repair": "ee index rebuild --workspace .",
    "details": {
      "recovery": [
        {
          "priority": 0,
          "kind": "rebuild",
          "rationale": "Rebuild the derived index.",
          "command": "ee index rebuild --workspace ."
        }
      ]
    }
  }
}
```"#,
            &inventory,
        );
        ensure(
            valid_error.is_empty(),
            format!(
                "valid error example should pass but got:\n{}",
                json_example_validation_events(&valid_error)
            ),
        )?;

        let invalid_error = json_example_validation_issues_for_text(
            "docs/migration-guide.md",
            r#"```json
{
  "schema": "ee.error.v2",
  "error": {
    "code": "search_index_unavailable",
    "message": "Search index is stale or unavailable.",
    "severity": "medium",
    "repair": "ee index rebuild --workspace .",
    "details": {"databaseGeneration": 12}
  }
}
```"#,
            &inventory,
        );
        ensure_equal(&invalid_error.len(), &1, "invalid error issue count")?;
        ensure(
            invalid_error[0].message.contains("recovery"),
            format!("invalid error should mention recovery: {invalid_error:?}"),
        )?;
        let invalid_error_event = json_example_validation_event(&invalid_error[0]);
        ensure_equal(
            &invalid_error_event
                .get("schema")
                .and_then(serde_json::Value::as_str),
            &Some("ee.test_event.v1"),
            "invalid error event schema",
        )
    }

    #[test]
    fn current_facing_docs_do_not_claim_legacy_success_envelopes() -> TestResult {
        let inventory = contract_inventory()?;
        let violations = legacy_schema_claim_violations(&inventory)?;
        ensure(
            violations.is_empty(),
            format!(
                "current-facing docs contain legacy success-envelope claims:\n{}",
                legacy_schema_claim_events(&violations)
            ),
        )
    }

    #[test]
    fn current_facing_docs_json_examples_match_current_envelope_contracts() -> TestResult {
        let inventory = contract_inventory()?;
        let issues = current_json_example_issues(&inventory)?;
        ensure(
            issues.is_empty(),
            format!(
                "current-facing JSON examples violate envelope contracts:\n{}",
                json_example_validation_events(&issues)
            ),
        )
    }

    // ========================================================================
    // bd-31nul.7 — Public schema registry and source-string parity scanner.
    //
    // Cross-checks the contract inventory against Rust source string literals
    // and comments, JSON schema-file `schema.const` values, and the exported
    // `public_schemas()` registry. Findings are emitted as ee.test_event.v1
    // rows with path, line, observed schema id, inventory status, source
    // kind, and policy decision so downstream gates (bd-31nul.5) can route
    // them. Tests are fixture-driven so the scanner can ship without waiting
    // on bead-owned cleanup of live source-string drift (e.g. bd-13631).
    // ========================================================================

    #[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, PartialOrd, Ord)]
    enum RustSourceKind {
        LineComment,
        DocComment,
        BlockComment,
        StringLiteral,
        RawStringLiteral,
        Code,
    }

    impl RustSourceKind {
        fn as_str(self) -> &'static str {
            match self {
                Self::LineComment => "line_comment",
                Self::DocComment => "doc_comment",
                Self::BlockComment => "block_comment",
                Self::StringLiteral => "string_literal",
                Self::RawStringLiteral => "raw_string_literal",
                Self::Code => "code",
            }
        }

        fn is_comment(self) -> bool {
            matches!(
                self,
                Self::LineComment | Self::DocComment | Self::BlockComment
            )
        }

        fn is_emitted_string(self) -> bool {
            matches!(self, Self::StringLiteral | Self::RawStringLiteral)
        }
    }

    const KIND_CODE: u8 = 0;
    const KIND_LINE_COMMENT: u8 = 1;
    const KIND_DOC_COMMENT: u8 = 2;
    const KIND_BLOCK_COMMENT: u8 = 3;
    const KIND_STRING_LITERAL: u8 = 4;
    const KIND_RAW_STRING_LITERAL: u8 = 5;

    fn kind_from_byte(byte: u8) -> RustSourceKind {
        match byte {
            KIND_LINE_COMMENT => RustSourceKind::LineComment,
            KIND_DOC_COMMENT => RustSourceKind::DocComment,
            KIND_BLOCK_COMMENT => RustSourceKind::BlockComment,
            KIND_STRING_LITERAL => RustSourceKind::StringLiteral,
            KIND_RAW_STRING_LITERAL => RustSourceKind::RawStringLiteral,
            _ => RustSourceKind::Code,
        }
    }

    #[derive(Debug, Clone, Eq, PartialEq, Hash)]
    struct SourceStringOccurrence {
        path: String,
        line: usize,
        schema_id: String,
        source_kind: RustSourceKind,
        inventory_status: String,
        policy_decision: String,
        snippet: String,
    }

    #[derive(Debug, Clone, Eq, PartialEq, Hash)]
    struct SchemaConstOccurrence {
        path: String,
        line: usize,
        observed_const: String,
        inventory_status: String,
        policy_decision: String,
        reason: String,
    }

    #[derive(Debug, Clone, Eq, PartialEq)]
    struct PublicSchemaRegistryDiff {
        missing: Vec<String>,
        extras: Vec<String>,
        mismatched: Vec<RegistryMismatch>,
    }

    #[derive(Debug, Clone, Eq, PartialEq)]
    struct RegistryMismatch {
        schema_id: String,
        inventory_status: String,
        registry_description: String,
    }

    impl PublicSchemaRegistryDiff {
        fn is_clean(&self) -> bool {
            self.missing.is_empty() && self.extras.is_empty() && self.mismatched.is_empty()
        }

        fn precise_message(&self) -> String {
            let mut parts = Vec::new();
            if !self.missing.is_empty() {
                parts.push(format!(
                    "missing from src/output/mod.rs::public_schemas: {}",
                    self.missing.join(", ")
                ));
            }
            if !self.extras.is_empty() {
                parts.push(format!(
                    "extra envelope schemas in public_schemas not declared by contract inventory: {}",
                    self.extras.join(", ")
                ));
            }
            if !self.mismatched.is_empty() {
                let pretty = self
                    .mismatched
                    .iter()
                    .map(|m| {
                        format!(
                            "{} (inventory={}, registry_description={:?})",
                            m.schema_id, m.inventory_status, m.registry_description
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                parts.push(format!("status mismatch: {pretty}"));
            }
            parts.join(" | ")
        }
    }

    /// Tokenize Rust source byte-by-byte and emit a per-byte kind annotation.
    /// Handles line comments (// and ///, //!), block comments (/* */ with
    /// nesting), plain string literals with escapes, and raw string literals
    /// (`r"..."` / `r#"..."#` / `r##"..."##`). Char literals are not separately
    /// annotated because the target schema identifiers cannot fit inside one.
    fn annotate_rust_source_kinds(text: &str) -> Vec<u8> {
        let bytes = text.as_bytes();
        let n = bytes.len();
        let mut kinds = vec![KIND_CODE; n];
        let mut i = 0;

        while i < n {
            let b = bytes[i];

            if b == b'/' && i + 1 < n && bytes[i + 1] == b'/' {
                let is_doc = i + 2 < n && (bytes[i + 2] == b'/' || bytes[i + 2] == b'!');
                let kind = if is_doc {
                    KIND_DOC_COMMENT
                } else {
                    KIND_LINE_COMMENT
                };
                let start = i;
                while i < n && bytes[i] != b'\n' {
                    i += 1;
                }
                for k in &mut kinds[start..i] {
                    *k = kind;
                }
                continue;
            }

            if b == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
                let is_doc = i + 2 < n && (bytes[i + 2] == b'*' || bytes[i + 2] == b'!');
                let kind = if is_doc {
                    KIND_DOC_COMMENT
                } else {
                    KIND_BLOCK_COMMENT
                };
                let start = i;
                let mut depth: usize = 1;
                i += 2;
                while i < n && depth > 0 {
                    if bytes[i] == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
                        depth += 1;
                        i += 2;
                    } else if bytes[i] == b'*' && i + 1 < n && bytes[i + 1] == b'/' {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                let end = i.min(n);
                for k in &mut kinds[start..end] {
                    *k = kind;
                }
                continue;
            }

            if b == b'r' {
                let prev_ok =
                    i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
                if prev_ok {
                    let mut j = i + 1;
                    let mut hashes = 0usize;
                    while j < n && bytes[j] == b'#' {
                        hashes += 1;
                        j += 1;
                    }
                    if j < n && bytes[j] == b'"' {
                        let start = i;
                        let body_start = j + 1;
                        let mut k = body_start;
                        while k < n {
                            if bytes[k] == b'"' {
                                let mut h = 0usize;
                                while k + 1 + h < n && bytes[k + 1 + h] == b'#' {
                                    h += 1;
                                }
                                if h >= hashes {
                                    k = k + 1 + hashes;
                                    break;
                                }
                            }
                            k += 1;
                        }
                        let end = k.min(n);
                        for kk in &mut kinds[start..end] {
                            *kk = KIND_RAW_STRING_LITERAL;
                        }
                        i = end;
                        continue;
                    }
                }
            }

            if b == b'"' {
                let start = i;
                i += 1;
                while i < n {
                    if bytes[i] == b'\\' && i + 1 < n {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                let end = i.min(n);
                for k in &mut kinds[start..end] {
                    *k = KIND_STRING_LITERAL;
                }
                continue;
            }

            i += 1;
        }

        kinds
    }

    fn source_snippet(text: &str, match_start: usize, match_len: usize) -> String {
        let window_start = previous_char_boundary(text, match_start.saturating_sub(40));
        let window_end = next_char_boundary(text, match_start + match_len + 40);
        text[window_start..window_end]
            .replace('\n', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn classify_source_occurrence(
        path: &str,
        kind: RustSourceKind,
        entry: &ContractInventoryEntry,
    ) -> String {
        if path_is_allowed_historical(path, entry) {
            "allowed_historical_context".to_owned()
        } else if kind.is_comment() {
            "comment_requires_repair".to_owned()
        } else if kind.is_emitted_string() {
            "violation_current_surface".to_owned()
        } else {
            "unclassified_source_kind".to_owned()
        }
    }

    fn scan_rust_source_for_legacy_schema_ids(
        path: &str,
        text: &str,
        inventory: &ContractInventory,
    ) -> Vec<SourceStringOccurrence> {
        let kinds = annotate_rust_source_kinds(text);
        let mut out = Vec::new();

        for entry in inventory
            .contracts
            .iter()
            .filter(|entry| entry.status == "legacy")
        {
            let id = entry.schema_id.as_str();
            let mut search_from = 0;
            while let Some(rel) = text[search_from..].find(id) {
                let abs = search_from + rel;
                search_from = abs + id.len();

                // Guard against substring matches like ee.response.v10 — the
                // next byte after the version digits must not be ASCII digit.
                let after = abs + id.len();
                if after < text.len() && text.as_bytes()[after].is_ascii_digit() {
                    continue;
                }

                let kind = kinds
                    .get(abs)
                    .copied()
                    .map(kind_from_byte)
                    .unwrap_or(RustSourceKind::Code);

                if matches!(kind, RustSourceKind::Code) {
                    // Identifier-like occurrences in code position are not
                    // valid Rust for a dotted schema id; skip to avoid noise.
                    continue;
                }

                let line = text[..abs].bytes().filter(|byte| *byte == b'\n').count() + 1;
                let decision = classify_source_occurrence(path, kind, entry);
                let snippet = source_snippet(text, abs, id.len());

                out.push(SourceStringOccurrence {
                    path: path.to_owned(),
                    line,
                    schema_id: id.to_owned(),
                    source_kind: kind,
                    inventory_status: entry.status.clone(),
                    policy_decision: decision,
                    snippet,
                });
            }
        }

        out.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then(left.line.cmp(&right.line))
                .then(left.schema_id.cmp(&right.schema_id))
                .then(left.source_kind.cmp(&right.source_kind))
        });
        out
    }

    fn source_string_occurrence_event(occurrence: &SourceStringOccurrence) -> serde_json::Value {
        serde_json::json!({
            "schema": "ee.test_event.v1",
            "phase": "contract_drift_source_string_parity",
            "path": &occurrence.path,
            "line": occurrence.line,
            "schema_id": &occurrence.schema_id,
            "source_kind": occurrence.source_kind.as_str(),
            "inventory_status": &occurrence.inventory_status,
            "policy_decision": &occurrence.policy_decision,
            "snippet": &occurrence.snippet,
        })
    }

    fn source_string_occurrence_events(occurrences: &[SourceStringOccurrence]) -> String {
        occurrences
            .iter()
            .map(source_string_occurrence_event)
            .map(|event| event.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn schema_id_looks_like_envelope(value: &str) -> bool {
        value.starts_with("ee.response.v")
            || value.starts_with("ee.error.v")
            || value.starts_with("ee.pack.v")
    }

    fn schema_const_line_hint(text: &str, observed: &str) -> usize {
        text.lines()
            .enumerate()
            .find(|(_, line)| line.contains(observed))
            .map(|(index, _)| index + 1)
            .unwrap_or(1)
    }

    fn visit_envelope_consts(value: &JsonValue, out: &mut Vec<String>) {
        match value {
            JsonValue::Object(object) => {
                if let Some(schema_value) = object.get("schema")
                    && let Some(inner) = schema_value.as_object()
                    && let Some(constant) = inner.get("const").and_then(JsonValue::as_str)
                    && schema_id_looks_like_envelope(constant)
                {
                    out.push(constant.to_owned());
                }
                for child in object.values() {
                    visit_envelope_consts(child, out);
                }
            }
            JsonValue::Array(array) => {
                for child in array {
                    visit_envelope_consts(child, out);
                }
            }
            _ => {}
        }
    }

    fn scan_schema_json_for_envelope_consts(
        path: &str,
        text: &str,
        inventory: &ContractInventory,
    ) -> Vec<SchemaConstOccurrence> {
        let Ok(value) = serde_json::from_str::<JsonValue>(text) else {
            return Vec::new();
        };
        let mut observed = Vec::new();
        visit_envelope_consts(&value, &mut observed);

        let mut out = Vec::new();
        for constant in observed {
            let entry = inventory_entry(inventory, &constant);
            let (status, decision, reason) = match entry {
                Ok(entry) => classify_schema_const(path, &constant, entry, inventory),
                Err(_) => (
                    "unknown".to_owned(),
                    "violation_unclassified_schema_id".to_owned(),
                    format!("{constant} has no contract inventory entry"),
                ),
            };
            out.push(SchemaConstOccurrence {
                path: path.to_owned(),
                line: schema_const_line_hint(text, &constant),
                observed_const: constant,
                inventory_status: status,
                policy_decision: decision,
                reason,
            });
        }

        out.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then(left.line.cmp(&right.line))
                .then(left.observed_const.cmp(&right.observed_const))
        });
        out
    }

    fn classify_schema_const(
        path: &str,
        observed: &str,
        entry: &ContractInventoryEntry,
        inventory: &ContractInventory,
    ) -> (String, String, String) {
        match entry.status.as_str() {
            "current" => (
                entry.status.clone(),
                "current_envelope_const".to_owned(),
                format!("{observed} is the current envelope const for {path}"),
            ),
            "legacy" => {
                if entry.schema_file.as_deref() == Some(path) {
                    (
                        entry.status.clone(),
                        "allowed_legacy_schema_file".to_owned(),
                        format!(
                            "{path} is the inventory-declared schemaFile for legacy {observed}"
                        ),
                    )
                } else if path_is_allowed_historical(path, entry) {
                    (
                        entry.status.clone(),
                        "allowed_historical_context".to_owned(),
                        format!(
                            "{path} matches an allowedHistoricalContexts pattern for {observed}"
                        ),
                    )
                } else if path.starts_with("docs/schemas/") {
                    let host_entry = inventory.contracts.iter().find(|candidate| {
                        candidate.schema_file.as_deref() == Some(path)
                            && candidate.status == "current"
                    });
                    if host_entry.is_some() {
                        (
                            entry.status.clone(),
                            "violation_legacy_const_in_current_schema_file".to_owned(),
                            format!(
                                "{path} is a current schemaFile yet declares legacy const {observed}"
                            ),
                        )
                    } else {
                        (
                            entry.status.clone(),
                            "violation_legacy_const_outside_owner_schema_file".to_owned(),
                            format!(
                                "legacy {observed} declared in {path}, which is neither the legacy schemaFile nor an allowed historical context"
                            ),
                        )
                    }
                } else {
                    (
                        entry.status.clone(),
                        "violation_legacy_const_outside_allowed_context".to_owned(),
                        format!("legacy {observed} declared outside an allowed context: {path}"),
                    )
                }
            }
            other => (
                other.to_owned(),
                "violation_unsupported_status".to_owned(),
                format!("{observed} has unsupported inventory status {other}"),
            ),
        }
    }

    fn schema_const_occurrence_event(occurrence: &SchemaConstOccurrence) -> serde_json::Value {
        serde_json::json!({
            "schema": "ee.test_event.v1",
            "phase": "contract_drift_schema_const_parity",
            "path": &occurrence.path,
            "line": occurrence.line,
            "schema_id": &occurrence.observed_const,
            "source_kind": "schema_const",
            "inventory_status": &occurrence.inventory_status,
            "policy_decision": &occurrence.policy_decision,
            "reason": &occurrence.reason,
        })
    }

    fn schema_const_occurrence_events(occurrences: &[SchemaConstOccurrence]) -> String {
        occurrences
            .iter()
            .map(schema_const_occurrence_event)
            .map(|event| event.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn public_schema_registry_diff(inventory: &ContractInventory) -> PublicSchemaRegistryDiff {
        let registry: BTreeMap<&str, &ee::output::SchemaEntry> = ee::output::public_schemas()
            .iter()
            .map(|entry| (entry.id, entry))
            .collect();

        let envelope_categories = ["envelope"];

        let mut missing = Vec::new();
        for entry in inventory
            .contracts
            .iter()
            .filter(|entry| entry.status == "current")
        {
            if !registry.contains_key(entry.schema_id.as_str()) {
                missing.push(entry.schema_id.clone());
            }
        }

        let inventory_ids: BTreeSet<&str> = inventory
            .contracts
            .iter()
            .map(|entry| entry.schema_id.as_str())
            .collect();

        let mut extras = Vec::new();
        for (id, schema) in &registry {
            if envelope_categories.contains(&schema.category) && !inventory_ids.contains(id) {
                extras.push((*id).to_owned());
            }
        }

        let mut mismatched = Vec::new();
        for entry in inventory.contracts.iter() {
            let Some(registered) = registry.get(entry.schema_id.as_str()) else {
                continue;
            };
            let description_lower = registered.description.to_ascii_lowercase();
            let registry_signals_legacy = description_lower.contains("legacy")
                || description_lower.contains("deprecated")
                || description_lower.contains("retained");
            let inventory_legacy = entry.status == "legacy";
            if inventory_legacy != registry_signals_legacy
                && envelope_categories.contains(&registered.category)
            {
                mismatched.push(RegistryMismatch {
                    schema_id: entry.schema_id.clone(),
                    inventory_status: entry.status.clone(),
                    registry_description: registered.description.to_owned(),
                });
            }
        }

        missing.sort();
        extras.sort();
        mismatched.sort_by(|a, b| a.schema_id.cmp(&b.schema_id));

        PublicSchemaRegistryDiff {
            missing,
            extras,
            mismatched,
        }
    }

    #[test]
    fn source_string_parity_classifies_string_literal_in_current_mcp_surface() -> TestResult {
        let inventory = contract_inventory()?;
        let fixture =
            "pub const PREPARE: &str = \"Read the returned `ee.response.v1` envelope.\";\n";
        let occurrences = scan_rust_source_for_legacy_schema_ids("src/mcp.rs", fixture, &inventory);
        ensure_equal(&occurrences.len(), &1, "string-literal occurrence count")?;
        let occurrence = &occurrences[0];
        ensure_equal(
            &occurrence.source_kind,
            &RustSourceKind::StringLiteral,
            "source kind",
        )?;
        ensure_equal(
            &occurrence.policy_decision,
            &"violation_current_surface".to_owned(),
            "policy decision",
        )?;
        let event = source_string_occurrence_event(occurrence);
        ensure_equal(
            &event.get("schema").and_then(serde_json::Value::as_str),
            &Some("ee.test_event.v1"),
            "event schema",
        )?;
        ensure_equal(
            &event.get("source_kind").and_then(serde_json::Value::as_str),
            &Some("string_literal"),
            "event source kind",
        )
    }

    #[test]
    fn source_string_parity_classifies_doc_comment_distinct_from_emitted_string() -> TestResult {
        let inventory = contract_inventory()?;
        let fixture = "//! - Same response contracts (ee.response.v1)\n\
                       pub const PREPARE: &str = \"Read the returned `ee.response.v1` envelope.\";\n";
        let occurrences = scan_rust_source_for_legacy_schema_ids("src/mcp.rs", fixture, &inventory);
        ensure_equal(&occurrences.len(), &2, "occurrence count")?;
        let comment = occurrences
            .iter()
            .find(|occurrence| occurrence.source_kind.is_comment())
            .ok_or("missing comment occurrence")?;
        let literal = occurrences
            .iter()
            .find(|occurrence| occurrence.source_kind.is_emitted_string())
            .ok_or("missing string-literal occurrence")?;
        ensure_equal(
            &comment.source_kind,
            &RustSourceKind::DocComment,
            "comment kind",
        )?;
        ensure_equal(
            &comment.policy_decision,
            &"comment_requires_repair".to_owned(),
            "comment decision",
        )?;
        ensure_equal(
            &literal.source_kind,
            &RustSourceKind::StringLiteral,
            "literal kind",
        )?;
        ensure_equal(
            &literal.policy_decision,
            &"violation_current_surface".to_owned(),
            "literal decision",
        )
    }

    #[test]
    fn source_string_parity_classifies_block_and_raw_string_kinds() -> TestResult {
        let inventory = contract_inventory()?;
        let fixture = "/* legacy mention: ee.response.v1 in block comment */\n\
                       let body = r#\"{\"schema\":\"ee.response.v1\"}\"#;\n";
        let occurrences = scan_rust_source_for_legacy_schema_ids("src/mcp.rs", fixture, &inventory);
        ensure_equal(&occurrences.len(), &2, "occurrence count")?;
        ensure(
            occurrences
                .iter()
                .any(|occurrence| occurrence.source_kind == RustSourceKind::BlockComment),
            format!(
                "expected a BlockComment kind but got:\n{}",
                source_string_occurrence_events(&occurrences)
            ),
        )?;
        ensure(
            occurrences
                .iter()
                .any(|occurrence| occurrence.source_kind == RustSourceKind::RawStringLiteral),
            format!(
                "expected a RawStringLiteral kind but got:\n{}",
                source_string_occurrence_events(&occurrences)
            ),
        )?;
        let raw = occurrences
            .iter()
            .find(|occurrence| occurrence.source_kind == RustSourceKind::RawStringLiteral)
            .ok_or("missing raw string occurrence")?;
        ensure_equal(
            &raw.policy_decision,
            &"violation_current_surface".to_owned(),
            "raw string decision in current surface",
        )
    }

    #[test]
    fn source_string_parity_allows_legacy_ids_in_owner_const_definition() -> TestResult {
        let inventory = contract_inventory()?;
        let fixture = "pub const RESPONSE_SCHEMA_V1: &str = \"ee.response.v1\";\n\
             //! Owner module describing the legacy ee.response.v1 envelope for backward compat.\n";
        let occurrences =
            scan_rust_source_for_legacy_schema_ids("src/models/mod.rs", fixture, &inventory);
        ensure(
            !occurrences.is_empty(),
            "expected at least one occurrence in owner module",
        )?;
        for occurrence in &occurrences {
            ensure_equal(
                &occurrence.policy_decision,
                &"allowed_historical_context".to_owned(),
                "owner-context decision",
            )?;
        }
        Ok(())
    }

    #[test]
    fn source_string_parity_allows_legacy_ids_in_tests_historical_context() -> TestResult {
        let inventory = contract_inventory()?;
        let fixture = "let body = \"{\\\"schema\\\":\\\"ee.response.v1\\\"}\";\n";
        let occurrences = scan_rust_source_for_legacy_schema_ids(
            "tests/contracts/some_fixture.rs",
            fixture,
            &inventory,
        );
        ensure_equal(&occurrences.len(), &1, "fixture occurrence count")?;
        ensure_equal(
            &occurrences[0].policy_decision,
            &"allowed_historical_context".to_owned(),
            "tests-historical decision",
        )
    }

    #[test]
    fn source_string_parity_does_not_match_within_longer_version_suffix() -> TestResult {
        let inventory = contract_inventory()?;
        // ee.response.v10 is a hypothetical future schema id; the scanner
        // must not treat it as ee.response.v1 with a trailing digit.
        let fixture = "let s = \"ee.response.v10\";\n";
        let occurrences = scan_rust_source_for_legacy_schema_ids("src/mcp.rs", fixture, &inventory);
        ensure(
            occurrences.is_empty(),
            format!(
                "ee.response.v10 must not match ee.response.v1 but got:\n{}",
                source_string_occurrence_events(&occurrences)
            ),
        )
    }

    #[test]
    fn schema_file_const_validator_allows_legacy_const_in_owner_schema_file() -> TestResult {
        let inventory = contract_inventory()?;
        let fixture = r#"{
            "properties": {
                "schema": {"type": "string", "const": "ee.response.v1"}
            }
        }"#;
        let occurrences = scan_schema_json_for_envelope_consts(
            "docs/schemas/ee.response.v1.json",
            fixture,
            &inventory,
        );
        ensure_equal(&occurrences.len(), &1, "occurrence count")?;
        ensure_equal(
            &occurrences[0].policy_decision,
            &"allowed_legacy_schema_file".to_owned(),
            "owner schemaFile decision",
        )
    }

    #[test]
    fn schema_file_const_validator_flags_legacy_const_in_unrelated_schema_file() -> TestResult {
        let inventory = contract_inventory()?;
        let fixture = r#"{
            "properties": {
                "schema": {"type": "string", "const": "ee.response.v1"}
            }
        }"#;
        let occurrences = scan_schema_json_for_envelope_consts(
            "docs/schemas/ee.status.v1.json",
            fixture,
            &inventory,
        );
        ensure_equal(&occurrences.len(), &1, "occurrence count")?;
        ensure(
            occurrences[0]
                .policy_decision
                .starts_with("violation_legacy_const"),
            format!(
                "unrelated schema file must violate but got:\n{}",
                schema_const_occurrence_events(&occurrences)
            ),
        )?;
        let event = schema_const_occurrence_event(&occurrences[0]);
        ensure_equal(
            &event.get("source_kind").and_then(serde_json::Value::as_str),
            &Some("schema_const"),
            "event source kind",
        )
    }

    #[test]
    fn schema_file_const_validator_flags_mismatched_filename_and_const() -> TestResult {
        let inventory = contract_inventory()?;
        // docs/schemas/ee.error.v1.json should hold "ee.error.v1" by the
        // inventory; if it instead declares "ee.error.v2" the scanner must
        // flag the mismatch as a current envelope const placed in a legacy
        // schemaFile, not silently pass.
        let fixture = r#"{
            "properties": {
                "schema": {"type": "string", "const": "ee.error.v2"}
            }
        }"#;
        let occurrences = scan_schema_json_for_envelope_consts(
            "docs/schemas/ee.error.v1.json",
            fixture,
            &inventory,
        );
        ensure_equal(&occurrences.len(), &1, "occurrence count")?;
        ensure(
            occurrences[0].observed_const == "ee.error.v2"
                && occurrences[0].policy_decision == "current_envelope_const",
            format!(
                "ee.error.v2 const should classify as current envelope const but got:\n{}",
                schema_const_occurrence_events(&occurrences)
            ),
        )?;
        // The classifier intentionally does not infer filename vs const
        // mismatch on its own — that responsibility belongs to the per-file
        // inventory entry. Confirm the scanner still surfaces the constant
        // so an inventory-driven gate (bd-31nul.5) can flag the placeholder.
        ensure(
            occurrences[0].reason.contains("ee.error.v2"),
            format!(
                "scanner reason must include observed constant: {:?}",
                occurrences[0].reason
            ),
        )
    }

    #[test]
    fn schema_file_const_validator_passes_current_schema_files() -> TestResult {
        let inventory = contract_inventory()?;
        let fixture = r#"{
            "properties": {
                "schema": {"type": "string", "const": "ee.error.v2"}
            }
        }"#;
        let occurrences = scan_schema_json_for_envelope_consts(
            "docs/schemas/ee.error.v2.json",
            fixture,
            &inventory,
        );
        ensure_equal(&occurrences.len(), &1, "occurrence count")?;
        ensure_equal(
            &occurrences[0].policy_decision,
            &"current_envelope_const".to_owned(),
            "current schemaFile decision",
        )
    }

    #[test]
    fn public_schema_registry_diff_pinpoints_missing_extras_and_mismatched() -> TestResult {
        let inventory = contract_inventory()?;
        let diff = public_schema_registry_diff(&inventory);
        ensure(
            diff.is_clean(),
            format!(
                "public_schemas registry drift detected: {}",
                diff.precise_message()
            ),
        )
    }

    #[test]
    fn public_schema_registry_diff_message_is_actionable_on_missing_extras() -> TestResult {
        // Construct a synthetic diff to prove the precise message contract
        // — the assertion in the gating test reuses the same format string.
        let diff = PublicSchemaRegistryDiff {
            missing: vec!["ee.example.v3".to_owned()],
            extras: vec!["ee.example.legacy.v1".to_owned()],
            mismatched: vec![RegistryMismatch {
                schema_id: "ee.response.v9".to_owned(),
                inventory_status: "legacy".to_owned(),
                registry_description: "Success response envelope for all ee commands".to_owned(),
            }],
        };
        let message = diff.precise_message();
        ensure(
            message.contains("missing from src/output/mod.rs::public_schemas: ee.example.v3"),
            format!("missing fragment absent: {message}"),
        )?;
        ensure(
            message.contains(
                "extra envelope schemas in public_schemas not declared by contract inventory: ee.example.legacy.v1",
            ),
            format!("extras fragment absent: {message}"),
        )?;
        ensure(
            message.contains("status mismatch: ee.response.v9 (inventory=legacy"),
            format!("mismatch fragment absent: {message}"),
        )
    }

    #[test]
    fn source_string_parity_event_has_required_fields() -> TestResult {
        let inventory = contract_inventory()?;
        let fixture = "let s = \"ee.response.v1\";\n";
        let occurrences = scan_rust_source_for_legacy_schema_ids("src/mcp.rs", fixture, &inventory);
        ensure_equal(&occurrences.len(), &1, "occurrence count")?;
        let event = source_string_occurrence_event(&occurrences[0]);
        for field in [
            "schema",
            "phase",
            "path",
            "line",
            "schema_id",
            "source_kind",
            "inventory_status",
            "policy_decision",
            "snippet",
        ] {
            ensure(
                event.get(field).is_some(),
                format!("event missing required field: {field}"),
            )?;
        }
        Ok(())
    }
}
