//! Procedural rule creation service (EE-086).
//!
//! `ee rule add` writes to the dedicated procedural rule tables added in
//! EE-084. It keeps direct rule management separate from generic memory
//! capture while preserving the same workspace, audit, dry-run, and index
//! queue conventions used by `ee remember`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::Serialize;

use crate::db::{
    CreateAuditInput, CreateProceduralRuleInput, CreateSearchIndexJobInput, CreateWorkspaceInput,
    DbConnection, SearchIndexJobType, StoredProceduralRule, audit_actions, generate_audit_id,
};
use crate::models::{
    DomainError, MemoryContent, MemoryId, RuleId, RuleMaturity, RuleScope, Tag, TrustClass,
    UnitScore, WorkspaceId,
};

/// Stable schema for `ee rule add` response data.
pub const RULE_ADD_SCHEMA_V1: &str = "ee.rule.add.v1";
/// Stable schema for `ee rule list` response data.
pub const RULE_LIST_SCHEMA_V1: &str = "ee.rule.list.v1";
/// Stable schema for `ee rule show` response data.
pub const RULE_SHOW_SCHEMA_V1: &str = "ee.rule.show.v1";

const MAX_RULE_CONTENT_BYTES: usize = 8192;
const MAX_RULE_LIST_LIMIT: u32 = 1000;

/// Options for creating a procedural rule through `ee rule add`.
#[derive(Clone, Debug)]
pub struct RuleAddOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Rule body.
    pub content: &'a str,
    /// Rule scope.
    pub scope: &'a str,
    /// Optional scope pattern for directory/file-pattern rules.
    pub scope_pattern: Option<&'a str>,
    /// Initial maturity.
    pub maturity: &'a str,
    /// Optional explicit confidence score.
    pub confidence: Option<f32>,
    /// Initial utility score.
    pub utility: f32,
    /// Initial importance score.
    pub importance: f32,
    /// Trust class.
    pub trust_class: &'a str,
    /// Tags, allowing repeated flags and comma-separated values.
    pub tags: &'a [String],
    /// Source memory IDs used as explicit evidence.
    pub source_memory_ids: &'a [String],
    /// Validate and render the write without mutating storage.
    pub dry_run: bool,
    /// Optional audit actor.
    pub actor: Option<&'a str>,
}

/// Options for listing procedural rules through `ee rule list`.
#[derive(Clone, Debug)]
pub struct RuleListOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Optional maturity filter.
    pub maturity: Option<&'a str>,
    /// Optional scope filter.
    pub scope: Option<&'a str>,
    /// Optional tag filter.
    pub tag: Option<&'a str>,
    /// Include tombstoned rules.
    pub include_tombstoned: bool,
    /// Maximum number of rules to return.
    pub limit: u32,
    /// Number of filtered rules to skip.
    pub offset: u32,
}

/// Options for showing one procedural rule through `ee rule show`.
#[derive(Clone, Debug)]
pub struct RuleShowOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Rule ID to retrieve.
    pub rule_id: &'a str,
    /// Include tombstoned rules.
    pub include_tombstoned: bool,
}

/// Result of creating or previewing a procedural rule.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleAddReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub status: String,
    pub rule_id: String,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub content: String,
    pub scope: String,
    pub scope_pattern: Option<String>,
    pub maturity: String,
    pub lifecycle: RuleAddLifecycle,
    pub trust_class: String,
    pub confidence: f32,
    pub utility: f32,
    pub importance: f32,
    pub tags: Vec<String>,
    pub source_memory_ids: Vec<String>,
    pub evidence: RuleAddEvidence,
    pub dry_run: bool,
    pub persisted: bool,
    pub audit_id: Option<String>,
    pub index_job_id: Option<String>,
    pub index_status: String,
    pub redaction_status: String,
    pub degraded: Vec<RuleAddDegradation>,
}

impl RuleAddReport {
    /// Serialize response data without the outer response envelope.
    #[must_use]
    pub fn data_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"rule add","status":"serialization_failed"}}"#,
                RULE_ADD_SCHEMA_V1
            )
        })
    }

    /// Human-readable summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        if self.dry_run {
            format!(
                "DRY RUN: Would add procedural rule ({})\n  Content: {}\n  Evidence: {}\n",
                self.maturity, self.content, self.evidence.status
            )
        } else {
            format!(
                "Added procedural rule: {}\n  ID: {}\n  Audit: {}\n  Index job: {}\n",
                self.content,
                self.rule_id,
                self.audit_id.as_deref().unwrap_or("none"),
                self.index_job_id.as_deref().unwrap_or("none")
            )
        }
    }

    /// Compact TOON-like summary.
    #[must_use]
    pub fn toon_summary(&self) -> String {
        format!(
            "RULE_ADD|status={}|id={}|maturity={}|evidence={}|persisted={}",
            self.status, self.rule_id, self.maturity, self.evidence.status, self.persisted
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleAddLifecycle {
    pub initial_maturity: String,
    pub is_active: bool,
    pub is_terminal: bool,
    pub next_action: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleAddEvidence {
    pub status: String,
    pub source_memory_count: usize,
    pub verified: bool,
    pub requirement: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleAddDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: String,
}

/// Result of listing procedural rules.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleListReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub total_count: usize,
    pub returned_count: usize,
    pub limit: u32,
    pub offset: u32,
    pub truncated: bool,
    pub filter: RuleListFilter,
    pub rules: Vec<RuleSummary>,
    pub degraded: Vec<RuleAddDegradation>,
}

impl RuleListReport {
    /// Serialize response data without the outer response envelope.
    #[must_use]
    pub fn data_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"rule list","status":"serialization_failed"}}"#,
                RULE_LIST_SCHEMA_V1
            )
        })
    }

    /// Human-readable summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = format!("Procedural rules ({} total", self.total_count);
        if self.truncated {
            output.push_str(", showing batch");
        }
        output.push_str(")\n\n");
        if self.rules.is_empty() {
            output.push_str("  No procedural rules found.\n");
            return output;
        }
        for rule in &self.rules {
            output.push_str(&format!(
                "  {} [{}] confidence={:.2}\n",
                rule.id, rule.maturity, rule.confidence
            ));
            output.push_str(&format!("    {}\n", rule.content_preview));
            output.push_str(&format!(
                "    scope={}, tags={}, evidence={}\n\n",
                rule.scope,
                rule.tags.len(),
                rule.evidence.source_memory_count
            ));
        }
        output.push_str("Next:\n  ee rule show <RULE_ID>\n");
        output
    }
}

/// Result of showing one procedural rule.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleShowReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub found: bool,
    pub rule: RuleDetails,
    pub degraded: Vec<RuleAddDegradation>,
}

impl RuleShowReport {
    /// Serialize response data without the outer response envelope.
    #[must_use]
    pub fn data_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"rule show","status":"serialization_failed"}}"#,
                RULE_SHOW_SCHEMA_V1
            )
        })
    }

    /// Human-readable summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let rule = &self.rule;
        let mut output = format!("Procedural rule: {}\n\n", rule.id);
        output.push_str(&format!("  Maturity: {}\n", rule.maturity));
        output.push_str(&format!("  Scope: {}", rule.scope));
        if let Some(pattern) = &rule.scope_pattern {
            output.push_str(&format!(" ({pattern})"));
        }
        output.push('\n');
        output.push_str(&format!("  Content:\n    {}\n", rule.content));
        output.push_str(&format!(
            "  Scores: confidence={:.2}, utility={:.2}, importance={:.2}\n",
            rule.confidence, rule.utility, rule.importance
        ));
        output.push_str(&format!("  Trust: {}\n", rule.trust_class));
        output.push_str(&format!(
            "  Feedback: +{} / -{}\n",
            rule.positive_feedback_count, rule.negative_feedback_count
        ));
        if !rule.tags.is_empty() {
            output.push_str(&format!("  Tags: {}\n", rule.tags.join(", ")));
        }
        if !rule.source_memory_ids.is_empty() {
            output.push_str(&format!(
                "  Source memories: {}\n",
                rule.source_memory_ids.join(", ")
            ));
        }
        output.push_str(&format!("  Created: {}\n", rule.created_at));
        output.push_str(&format!("  Updated: {}\n", rule.updated_at));
        if let Some(ts) = &rule.tombstoned_at {
            output.push_str(&format!("  Tombstoned: {ts}\n"));
        }
        output
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleListFilter {
    pub maturity: Option<String>,
    pub scope: Option<String>,
    pub tag: Option<String>,
    pub include_tombstoned: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleSummary {
    pub id: String,
    pub content_preview: String,
    pub maturity: String,
    pub lifecycle: RuleLifecycle,
    pub scope: String,
    pub scope_pattern: Option<String>,
    pub trust_class: String,
    pub confidence: f32,
    pub utility: f32,
    pub importance: f32,
    pub evidence: RuleEvidence,
    pub tags: Vec<String>,
    pub is_tombstoned: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleDetails {
    pub id: String,
    pub workspace_id: String,
    pub content: String,
    pub confidence: f32,
    pub utility: f32,
    pub importance: f32,
    pub trust_class: String,
    pub scope: String,
    pub scope_pattern: Option<String>,
    pub maturity: String,
    pub lifecycle: RuleLifecycle,
    pub positive_feedback_count: u32,
    pub negative_feedback_count: u32,
    pub last_applied_at: Option<String>,
    pub last_validated_at: Option<String>,
    pub superseded_by: Option<String>,
    pub source_memory_ids: Vec<String>,
    pub tags: Vec<String>,
    pub evidence: RuleEvidence,
    pub created_at: String,
    pub updated_at: String,
    pub tombstoned_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleLifecycle {
    pub maturity: String,
    pub is_active: bool,
    pub is_terminal: bool,
    pub next_action: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleEvidence {
    pub status: String,
    pub source_memory_count: usize,
    pub verified: bool,
    pub requirement: String,
}

#[derive(Clone, Debug)]
struct PreparedRuleAdd {
    rule_id: RuleId,
    workspace_id: String,
    workspace_path: PathBuf,
    database_path: PathBuf,
    content: String,
    scope: RuleScope,
    scope_pattern: Option<String>,
    maturity: RuleMaturity,
    trust_class: TrustClass,
    confidence: f32,
    utility: f32,
    importance: f32,
    tags: Vec<String>,
    source_memory_ids: Vec<String>,
    actor: Option<String>,
}

#[derive(Clone, Debug)]
struct PreparedRuleRead {
    workspace_id: String,
    workspace_path: PathBuf,
    database_path: PathBuf,
}

/// Add a procedural rule or preview the write.
pub fn add_rule(options: &RuleAddOptions<'_>) -> Result<RuleAddReport, DomainError> {
    let prepared = prepare_rule_add(options)?;
    if options.dry_run {
        return Ok(rule_add_report(
            &prepared, "dry_run", false, None, None, false,
        ));
    }

    ensure_database_parent_exists(&prepared.database_path)?;
    let connection =
        DbConnection::open_file(&prepared.database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open database: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;
    connection.migrate().map_err(|error| DomainError::Storage {
        message: format!("Failed to migrate database: {error}"),
        repair: Some("ee doctor".to_string()),
    })?;
    ensure_workspace(
        &connection,
        &prepared.workspace_id,
        &prepared.workspace_path,
    )?;
    verify_source_memories(
        &connection,
        &prepared.workspace_id,
        &prepared.source_memory_ids,
    )?;

    let rule_id = prepared.rule_id.to_string();
    let audit_id = generate_audit_id();
    let index_job_id = generate_search_index_job_id();
    let input = CreateProceduralRuleInput {
        workspace_id: prepared.workspace_id.clone(),
        content: prepared.content.clone(),
        confidence: prepared.confidence,
        utility: prepared.utility,
        importance: prepared.importance,
        trust_class: prepared.trust_class.as_str().to_owned(),
        scope: prepared.scope.as_str().to_owned(),
        scope_pattern: prepared.scope_pattern.clone(),
        maturity: prepared.maturity.as_str().to_owned(),
        source_memory_ids: prepared.source_memory_ids.clone(),
        tags: prepared.tags.clone(),
    };
    let audit_details = rule_add_audit_details(&rule_id, &input);
    let index_input = CreateSearchIndexJobInput {
        workspace_id: prepared.workspace_id.clone(),
        job_type: SearchIndexJobType::SingleDocument,
        document_source: Some("rule".to_owned()),
        document_id: Some(rule_id.clone()),
        documents_total: 1,
    };

    connection
        .with_transaction(|| {
            connection.insert_procedural_rule(&rule_id, &input)?;
            connection.insert_audit(
                &audit_id,
                &CreateAuditInput {
                    workspace_id: Some(input.workspace_id.clone()),
                    actor: prepared
                        .actor
                        .clone()
                        .or_else(|| Some("ee rule add".to_owned())),
                    action: audit_actions::RULE_CREATE.to_owned(),
                    target_type: Some("rule".to_owned()),
                    target_id: Some(rule_id.clone()),
                    details: Some(audit_details.clone()),
                },
            )?;
            connection.insert_search_index_job(&index_job_id, &index_input)
        })
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to store procedural rule: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;

    Ok(rule_add_report(
        &prepared,
        "stored",
        true,
        Some(audit_id),
        Some(index_job_id),
        true,
    ))
}

/// List procedural rules for the selected workspace.
pub fn list_rules(options: &RuleListOptions<'_>) -> Result<RuleListReport, DomainError> {
    let prepared = prepare_rule_read(
        options.workspace_path,
        options.database_path,
        Some("ee rule list --help"),
    )?;
    let maturity = parse_optional_maturity(options.maturity)?;
    let scope = parse_optional_scope(options.scope)?;
    let tag = parse_optional_tag(options.tag)?;
    validate_list_window(options.limit)?;

    let connection = open_existing_database(&prepared.database_path)?;
    let stored = connection
        .list_procedural_rules(
            &prepared.workspace_id,
            maturity.as_deref(),
            scope.as_deref(),
            options.include_tombstoned,
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list procedural rules: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;

    let mut details = Vec::with_capacity(stored.len());
    for rule in stored {
        let detail = load_rule_details(&connection, rule)?;
        if tag
            .as_ref()
            .is_none_or(|required| detail.tags.iter().any(|value| value == required))
        {
            details.push(detail);
        }
    }

    let total_count = details.len();
    let offset = usize::try_from(options.offset).map_err(|_| {
        rule_read_usage_error(
            "rule list offset is too large".to_owned(),
            "ee rule list --help",
        )
    })?;
    let limit = usize::try_from(options.limit).map_err(|_| {
        rule_read_usage_error(
            "rule list limit is too large".to_owned(),
            "ee rule list --help",
        )
    })?;
    let rules = details
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(rule_summary_from_details)
        .collect::<Vec<_>>();
    let returned_count = rules.len();
    let truncated = offset.saturating_add(returned_count) < total_count;

    Ok(RuleListReport {
        schema: RULE_LIST_SCHEMA_V1,
        command: "rule list",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        total_count,
        returned_count,
        limit: options.limit,
        offset: options.offset,
        truncated,
        filter: RuleListFilter {
            maturity,
            scope,
            tag,
            include_tombstoned: options.include_tombstoned,
        },
        rules,
        degraded: Vec::new(),
    })
}

/// Show one procedural rule in the selected workspace.
pub fn show_rule(options: &RuleShowOptions<'_>) -> Result<RuleShowReport, DomainError> {
    let prepared = prepare_rule_read(
        options.workspace_path,
        options.database_path,
        Some("ee rule show <RULE_ID> --json"),
    )?;
    let rule_id = RuleId::from_str(options.rule_id)
        .map_err(|error| {
            rule_read_usage_error(format!("invalid rule ID: {error}"), "ee rule show --help")
        })?
        .to_string();
    let connection = open_existing_database(&prepared.database_path)?;
    let Some(rule) =
        connection
            .get_procedural_rule(&rule_id)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to query procedural rule: {error}"),
                repair: Some("ee doctor".to_owned()),
            })?
    else {
        return Err(rule_not_found(&rule_id));
    };
    if rule.workspace_id != prepared.workspace_id {
        return Err(rule_not_found(&rule_id));
    }
    if rule.tombstoned_at.is_some() && !options.include_tombstoned {
        return Err(rule_not_found(&rule_id));
    }
    let detail = load_rule_details(&connection, rule)?;

    Ok(RuleShowReport {
        schema: RULE_SHOW_SCHEMA_V1,
        command: "rule show",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        found: true,
        rule: detail,
        degraded: Vec::new(),
    })
}

fn prepare_rule_read(
    workspace_path: &Path,
    database_path: Option<&Path>,
    repair: Option<&str>,
) -> Result<PreparedRuleRead, DomainError> {
    let workspace_path =
        resolve_workspace_path(workspace_path, false).map_err(|error| match error {
            DomainError::Configuration { message, .. } => DomainError::Configuration {
                message,
                repair: repair.map(str::to_owned),
            },
            other => other,
        })?;
    let database_path = database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    Ok(PreparedRuleRead {
        workspace_id: stable_workspace_id(&workspace_path),
        workspace_path,
        database_path,
    })
}

fn open_existing_database(database_path: &Path) -> Result<DbConnection, DomainError> {
    if !database_path.exists() {
        return Err(DomainError::Storage {
            message: format!("Database not found at {}", database_path.display()),
            repair: Some("ee init --workspace .".to_owned()),
        });
    }
    DbConnection::open_file(database_path).map_err(|error| DomainError::Storage {
        message: format!("Failed to open database: {error}"),
        repair: Some("ee doctor".to_owned()),
    })
}

fn parse_optional_maturity(raw: Option<&str>) -> Result<Option<String>, DomainError> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            RuleMaturity::from_str(value)
                .map(|maturity| maturity.as_str().to_owned())
                .map_err(|error| rule_read_usage_error(error.to_string(), "ee rule list --help"))
        })
        .transpose()
}

fn parse_optional_scope(raw: Option<&str>) -> Result<Option<String>, DomainError> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            RuleScope::from_str(value)
                .map(|scope| scope.as_str().to_owned())
                .map_err(|error| rule_read_usage_error(error.to_string(), "ee rule list --help"))
        })
        .transpose()
}

fn parse_optional_tag(raw: Option<&str>) -> Result<Option<String>, DomainError> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            Tag::parse(value)
                .map(|tag| tag.to_string())
                .map_err(|error| rule_read_usage_error(error.to_string(), "ee rule list --help"))
        })
        .transpose()
}

fn validate_list_window(limit: u32) -> Result<(), DomainError> {
    if limit == 0 {
        return Err(rule_read_usage_error(
            "rule list --limit must be greater than zero".to_owned(),
            "ee rule list --help",
        ));
    }
    if limit > MAX_RULE_LIST_LIMIT {
        return Err(rule_read_usage_error(
            format!("rule list --limit must be <= {MAX_RULE_LIST_LIMIT}"),
            "ee rule list --help",
        ));
    }
    Ok(())
}

fn load_rule_details(
    connection: &DbConnection,
    stored: StoredProceduralRule,
) -> Result<RuleDetails, DomainError> {
    let tags = connection
        .get_rule_tags(&stored.id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query rule tags: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;
    let source_memory_ids = connection
        .get_rule_source_memory_ids(&stored.id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query rule source memories: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;
    let lifecycle = rule_lifecycle(&stored.maturity, source_memory_ids.len());
    let evidence = rule_evidence(&stored.maturity, source_memory_ids.len());

    Ok(RuleDetails {
        id: stored.id,
        workspace_id: stored.workspace_id,
        content: stored.content,
        confidence: stored.confidence,
        utility: stored.utility,
        importance: stored.importance,
        trust_class: stored.trust_class,
        scope: stored.scope,
        scope_pattern: stored.scope_pattern,
        maturity: stored.maturity,
        lifecycle,
        positive_feedback_count: stored.positive_feedback_count,
        negative_feedback_count: stored.negative_feedback_count,
        last_applied_at: stored.last_applied_at,
        last_validated_at: stored.last_validated_at,
        superseded_by: stored.superseded_by,
        source_memory_ids,
        tags,
        evidence,
        created_at: stored.created_at,
        updated_at: stored.updated_at,
        tombstoned_at: stored.tombstoned_at,
    })
}

fn rule_summary_from_details(details: RuleDetails) -> RuleSummary {
    RuleSummary {
        id: details.id,
        content_preview: truncate_rule_content(&details.content),
        maturity: details.maturity,
        lifecycle: details.lifecycle,
        scope: details.scope,
        scope_pattern: details.scope_pattern,
        trust_class: details.trust_class,
        confidence: details.confidence,
        utility: details.utility,
        importance: details.importance,
        evidence: details.evidence,
        tags: details.tags,
        is_tombstoned: details.tombstoned_at.is_some(),
        created_at: details.created_at,
        updated_at: details.updated_at,
    }
}

fn rule_lifecycle(maturity: &str, source_memory_count: usize) -> RuleLifecycle {
    let parsed = RuleMaturity::from_str(maturity).ok();
    RuleLifecycle {
        maturity: maturity.to_owned(),
        is_active: parsed.is_some_and(RuleMaturity::is_active),
        is_terminal: parsed.is_some_and(RuleMaturity::is_terminal),
        next_action: rule_next_action(parsed, source_memory_count),
    }
}

fn rule_next_action(maturity: Option<RuleMaturity>, source_memory_count: usize) -> String {
    match maturity {
        Some(RuleMaturity::Draft) => "promote to candidate when evidence exists".to_owned(),
        Some(RuleMaturity::Candidate) if source_memory_count == 0 => {
            "attach source memory evidence before validation".to_owned()
        }
        Some(RuleMaturity::Candidate) => "record outcomes or validate evidence".to_owned(),
        Some(RuleMaturity::Validated) => "monitor feedback and decay signals".to_owned(),
        Some(RuleMaturity::Deprecated) => {
            "keep for history; avoid selecting for new context".to_owned()
        }
        Some(RuleMaturity::Superseded) => {
            "follow superseded_by replacement when present".to_owned()
        }
        None => "repair malformed rule maturity".to_owned(),
    }
}

fn rule_evidence(maturity: &str, source_memory_count: usize) -> RuleEvidence {
    let verified = maturity == RuleMaturity::Validated.as_str() && source_memory_count > 0;
    let status = match (source_memory_count, verified) {
        (0, _) => "missing",
        (_, true) => "verified",
        _ => "attached",
    };
    RuleEvidence {
        status: status.to_owned(),
        source_memory_count,
        verified,
        requirement: "validated rules require at least one source memory".to_owned(),
    }
}

fn truncate_rule_content(content: &str) -> String {
    let mut chars = content.chars();
    let preview = chars.by_ref().take(80).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
}

fn rule_not_found(rule_id: &str) -> DomainError {
    DomainError::NotFound {
        resource: "procedural rule".to_owned(),
        id: rule_id.to_owned(),
        repair: Some("ee rule list --json".to_owned()),
    }
}

fn prepare_rule_add(options: &RuleAddOptions<'_>) -> Result<PreparedRuleAdd, DomainError> {
    let workspace_path = resolve_workspace_path(options.workspace_path, options.dry_run)?;
    let database_path = options
        .database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    let content = MemoryContent::parse(options.content)
        .map_err(|error| rule_usage_error(error.to_string()))?
        .as_str()
        .to_owned();
    if content.len() > MAX_RULE_CONTENT_BYTES {
        return Err(rule_usage_error(format!(
            "rule content is too large: {} bytes > {} bytes",
            content.len(),
            MAX_RULE_CONTENT_BYTES
        )));
    }
    validate_rule_policy(&content)?;

    let scope =
        RuleScope::from_str(options.scope).map_err(|error| rule_usage_error(error.to_string()))?;
    let scope_pattern = prepare_scope_pattern(scope, options.scope_pattern)?;
    let maturity = RuleMaturity::from_str(options.maturity)
        .map_err(|error| rule_usage_error(error.to_string()))?;
    if maturity.is_terminal() {
        return Err(rule_usage_error(
            "`ee rule add` creates active rules; use a lifecycle command to deprecate or supersede"
                .to_owned(),
        ));
    }

    let trust_class = TrustClass::from_str(options.trust_class)
        .map_err(|error| rule_usage_error(error.to_string()))?;
    let source_memory_ids = parse_source_memory_ids(options.source_memory_ids)?;
    if maturity == RuleMaturity::Validated && source_memory_ids.is_empty() {
        return Err(rule_usage_error(
            "validated rules require at least one --source-memory evidence ID".to_owned(),
        ));
    }

    let confidence = match options.confidence {
        Some(value) => UnitScore::parse(value)
            .map_err(|error| rule_usage_error(error.to_string()))?
            .into_inner(),
        None if source_memory_ids.is_empty() => trust_class.initial_confidence().min(0.55),
        None => trust_class.initial_confidence(),
    };
    let utility = UnitScore::parse(options.utility)
        .map_err(|error| rule_usage_error(error.to_string()))?
        .into_inner();
    let importance = UnitScore::parse(options.importance)
        .map_err(|error| rule_usage_error(error.to_string()))?
        .into_inner();
    let tags = parse_tags(options.tags)?;
    let actor = options.actor.map(str::trim).and_then(|actor| {
        if actor.is_empty() {
            None
        } else {
            Some(actor.to_owned())
        }
    });

    Ok(PreparedRuleAdd {
        rule_id: RuleId::now(),
        workspace_id: stable_workspace_id(&workspace_path),
        workspace_path,
        database_path,
        content,
        scope,
        scope_pattern,
        maturity,
        trust_class,
        confidence,
        utility,
        importance,
        tags,
        source_memory_ids,
        actor,
    })
}

fn prepare_scope_pattern(
    scope: RuleScope,
    raw: Option<&str>,
) -> Result<Option<String>, DomainError> {
    let pattern = raw.map(str::trim).filter(|value| !value.is_empty());
    if scope.requires_pattern() && pattern.is_none() {
        return Err(rule_usage_error(format!(
            "scope `{}` requires --scope-pattern",
            scope.as_str()
        )));
    }
    if !scope.requires_pattern() && pattern.is_some() {
        return Err(rule_usage_error(format!(
            "scope `{}` does not accept --scope-pattern",
            scope.as_str()
        )));
    }
    Ok(pattern.map(str::to_owned))
}

fn parse_tags(raw_tags: &[String]) -> Result<Vec<String>, DomainError> {
    let mut unique = BTreeSet::new();
    for tag_arg in raw_tags {
        for raw in tag_arg
            .split(',')
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
        {
            let tag = Tag::parse(raw).map_err(|error| rule_usage_error(error.to_string()))?;
            unique.insert(tag.to_string());
        }
    }
    Ok(unique.into_iter().collect())
}

fn parse_source_memory_ids(raw_ids: &[String]) -> Result<Vec<String>, DomainError> {
    let mut unique = BTreeSet::new();
    for id_arg in raw_ids {
        for raw in id_arg.split(',').map(str::trim).filter(|id| !id.is_empty()) {
            let memory_id = MemoryId::from_str(raw)
                .map_err(|error| rule_usage_error(format!("invalid source memory ID: {error}")))?;
            unique.insert(memory_id.to_string());
        }
    }
    Ok(unique.into_iter().collect())
}

fn verify_source_memories(
    connection: &DbConnection,
    workspace_id: &str,
    source_memory_ids: &[String],
) -> Result<(), DomainError> {
    for source_id in source_memory_ids {
        let memory = connection
            .get_memory(source_id)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to query source memory {source_id}: {error}"),
                repair: Some("ee memory show <memory-id> --json".to_owned()),
            })?;
        let Some(memory) = memory else {
            return Err(DomainError::NotFound {
                resource: "source memory".to_owned(),
                id: source_id.clone(),
                repair: Some(
                    "Create or import the evidence memory before adding the rule.".to_owned(),
                ),
            });
        };
        if memory.workspace_id != workspace_id {
            return Err(rule_usage_error(format!(
                "source memory {source_id} belongs to workspace {}, not {}",
                memory.workspace_id, workspace_id
            )));
        }
        if memory.tombstoned_at.is_some() {
            return Err(rule_usage_error(format!(
                "source memory {source_id} is tombstoned and cannot support a new rule"
            )));
        }
    }
    Ok(())
}

fn rule_add_report(
    prepared: &PreparedRuleAdd,
    status: &str,
    persisted: bool,
    audit_id: Option<String>,
    index_job_id: Option<String>,
    verified_evidence: bool,
) -> RuleAddReport {
    let source_memory_count = prepared.source_memory_ids.len();
    let evidence_status = match (source_memory_count, verified_evidence, persisted) {
        (0, _, _) => "missing",
        (_, true, true) => "verified",
        (_, false, false) => "declared_not_verified",
        _ => "declared",
    };
    RuleAddReport {
        schema: RULE_ADD_SCHEMA_V1,
        command: "rule add",
        version: env!("CARGO_PKG_VERSION"),
        status: status.to_owned(),
        rule_id: prepared.rule_id.to_string(),
        workspace_id: prepared.workspace_id.clone(),
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        content: prepared.content.clone(),
        scope: prepared.scope.as_str().to_owned(),
        scope_pattern: prepared.scope_pattern.clone(),
        maturity: prepared.maturity.as_str().to_owned(),
        lifecycle: RuleAddLifecycle {
            initial_maturity: prepared.maturity.as_str().to_owned(),
            is_active: prepared.maturity.is_active(),
            is_terminal: prepared.maturity.is_terminal(),
            next_action: if source_memory_count == 0 {
                "attach evidence with a source memory before promotion".to_owned()
            } else {
                "record outcomes with ee outcome --target-type rule".to_owned()
            },
        },
        trust_class: prepared.trust_class.as_str().to_owned(),
        confidence: prepared.confidence,
        utility: prepared.utility,
        importance: prepared.importance,
        tags: prepared.tags.clone(),
        source_memory_ids: prepared.source_memory_ids.clone(),
        evidence: RuleAddEvidence {
            status: evidence_status.to_owned(),
            source_memory_count,
            verified: verified_evidence,
            requirement: "validated rules require at least one source memory".to_owned(),
        },
        dry_run: !persisted,
        persisted,
        audit_id,
        index_job_id,
        index_status: if persisted {
            "queued".to_owned()
        } else {
            "dry_run_not_queued".to_owned()
        },
        redaction_status: "checked".to_owned(),
        degraded: Vec::new(),
    }
}

fn resolve_workspace_path(path: &Path, dry_run: bool) -> Result<PathBuf, DomainError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };

    match absolute.canonicalize() {
        Ok(canonical) => Ok(canonical),
        Err(_error) if dry_run => Ok(absolute),
        Err(error) => Err(DomainError::Configuration {
            message: format!(
                "Failed to resolve workspace {}: {error}",
                absolute.display()
            ),
            repair: Some("ee init --workspace .".to_owned()),
        }),
    }
}

fn ensure_database_parent_exists(database_path: &Path) -> Result<(), DomainError> {
    let Some(parent) = database_path.parent() else {
        return Ok(());
    };
    if parent.exists() {
        return Ok(());
    }
    Err(DomainError::Storage {
        message: format!("Database directory not found at {}", parent.display()),
        repair: Some("ee init --workspace .".to_owned()),
    })
}

fn ensure_workspace(
    connection: &DbConnection,
    workspace_id: &str,
    workspace_path: &Path,
) -> Result<(), DomainError> {
    let path = workspace_path.to_string_lossy().into_owned();
    if connection
        .get_workspace_by_path(&path)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query workspace: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?
        .is_some()
    {
        return Ok(());
    }

    connection
        .insert_workspace(
            workspace_id,
            &CreateWorkspaceInput {
                path,
                name: workspace_path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
            },
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to register workspace: {error}"),
            repair: Some("ee doctor".to_owned()),
        })
}

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    for (target, source) in bytes.iter_mut().zip(hash.as_bytes()) {
        *target = *source;
    }
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn generate_search_index_job_id() -> String {
    let rule_id = RuleId::now().to_string();
    let payload = rule_id.trim_start_matches("rule_");
    format!("sidx_{payload}")
}

fn rule_add_audit_details(rule_id: &str, input: &CreateProceduralRuleInput) -> String {
    serde_json::json!({
        "schema": "ee.audit.rule_create.v1",
        "command": "ee rule add",
        "ruleId": rule_id,
        "maturity": input.maturity,
        "scope": input.scope,
        "scopePattern": input.scope_pattern,
        "trustClass": input.trust_class,
        "confidence": input.confidence,
        "utility": input.utility,
        "importance": input.importance,
        "tagCount": input.tags.len(),
        "sourceMemoryCount": input.source_memory_ids.len(),
    })
    .to_string()
}

fn rule_usage_error(message: String) -> DomainError {
    DomainError::Usage {
        message,
        repair: Some("ee rule add --help".to_owned()),
    }
}

fn rule_read_usage_error(message: String, repair: &str) -> DomainError {
    DomainError::Usage {
        message,
        repair: Some(repair.to_owned()),
    }
}

const RULE_SECRET_PATTERNS: &[&str] = &[
    "password",
    "secret",
    "api_key",
    "apikey",
    "api-key",
    "token",
    "bearer",
    "authorization",
    "credential",
    "private_key",
    "access_key",
    "secret_key",
    "database_url",
    "connection_string",
    "-----begin",
];

fn validate_rule_policy(content: &str) -> Result<(), DomainError> {
    let lowered = content.to_ascii_lowercase();
    if RULE_SECRET_PATTERNS
        .iter()
        .any(|pattern| lowered.contains(pattern))
    {
        return Err(DomainError::PolicyDenied {
            message: "Refusing to persist rule content that looks like it contains a secret."
                .to_owned(),
            repair: Some(
                "Redact the secret and run `ee rule add` again with only durable guidance."
                    .to_owned(),
            ),
        });
    }
    Ok(())
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

    #[test]
    fn rule_add_dry_run_canonicalizes_tags_and_sources() -> TestResult {
        let source_a = MemoryId::from_uuid(uuid::Uuid::from_u128(2)).to_string();
        let source_b = MemoryId::from_uuid(uuid::Uuid::from_u128(1)).to_string();
        let tags = vec!["Rust,CI".to_owned(), "rust".to_owned()];
        let sources = vec![source_a.clone(), source_b.clone(), source_a.clone()];
        let report = add_rule(&RuleAddOptions {
            workspace_path: Path::new("."),
            database_path: None,
            content: "Run cargo fmt --check before release.",
            scope: "workspace",
            scope_pattern: None,
            maturity: "candidate",
            confidence: None,
            utility: 0.5,
            importance: 0.5,
            trust_class: "human_explicit",
            tags: &tags,
            source_memory_ids: &sources,
            dry_run: true,
            actor: None,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.schema, RULE_ADD_SCHEMA_V1);
        assert_eq!(report.status, "dry_run");
        assert_eq!(report.tags, vec!["ci".to_owned(), "rust".to_owned()]);
        assert_eq!(report.source_memory_ids, vec![source_b, source_a]);
        assert_eq!(report.evidence.status, "declared_not_verified");
        ensure(!report.persisted, "dry-run must not persist")
    }

    #[test]
    fn rule_add_requires_scope_pattern_for_directory_rules() -> TestResult {
        let err = match add_rule(&RuleAddOptions {
            workspace_path: Path::new("."),
            database_path: None,
            content: "Use scoped rule.",
            scope: "directory",
            scope_pattern: None,
            maturity: "candidate",
            confidence: None,
            utility: 0.5,
            importance: 0.5,
            trust_class: "human_explicit",
            tags: &[],
            source_memory_ids: &[],
            dry_run: true,
            actor: None,
        }) {
            Ok(_) => return Err("directory scope without pattern should fail".to_owned()),
            Err(err) => err,
        };

        ensure(
            matches!(err, DomainError::Usage { .. }),
            "expected usage error",
        )
    }

    #[test]
    fn rule_add_rejects_validated_without_evidence() -> TestResult {
        let err = match add_rule(&RuleAddOptions {
            workspace_path: Path::new("."),
            database_path: None,
            content: "A validated rule needs evidence.",
            scope: "workspace",
            scope_pattern: None,
            maturity: "validated",
            confidence: None,
            utility: 0.5,
            importance: 0.5,
            trust_class: "human_explicit",
            tags: &[],
            source_memory_ids: &[],
            dry_run: true,
            actor: None,
        }) {
            Ok(_) => return Err("validated rule without evidence should fail".to_owned()),
            Err(err) => err,
        };

        ensure(
            err.message().contains("validated rules require"),
            "error should mention evidence requirement",
        )
    }

    #[test]
    fn rule_list_rejects_zero_limit_before_database_open() -> TestResult {
        let err = match list_rules(&RuleListOptions {
            workspace_path: Path::new("."),
            database_path: Some(Path::new("/definitely/not/ee.db")),
            maturity: None,
            scope: None,
            tag: None,
            include_tombstoned: false,
            limit: 0,
            offset: 0,
        }) {
            Ok(_) => return Err("zero list limit should fail".to_owned()),
            Err(err) => err,
        };

        ensure(
            err.message().contains("--limit must be greater than zero"),
            "error should mention limit",
        )
    }

    #[test]
    fn rule_show_rejects_invalid_rule_id_before_database_open() -> TestResult {
        let err = match show_rule(&RuleShowOptions {
            workspace_path: Path::new("."),
            database_path: Some(Path::new("/definitely/not/ee.db")),
            rule_id: "mem_00000000000000000000000001",
            include_tombstoned: false,
        }) {
            Ok(_) => return Err("wrong ID prefix should fail".to_owned()),
            Err(err) => err,
        };

        ensure(
            err.message().contains("invalid rule ID"),
            "error should mention invalid rule ID",
        )
    }
}
