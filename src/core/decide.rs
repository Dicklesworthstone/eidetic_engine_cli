//! Durable decision recording and revisit queries.
//!
//! `ee decide` is a thin use-case layer over the memory source of truth:
//! decisions are ordinary `kind=decision` memories with registry-validated
//! typed fields, optional `supersedes` memory links, and audited lifecycle
//! expiration for replaced heads.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use serde::Serialize;
use serde_json::{Value as JsonValue, json};

use super::memory::{
    ExpireMemoryOptions, MemoryLinkMode, MemoryLinkOptions, RememberMemoryOptions, expire_memory,
    remember_memory, update_memory_link,
};
use super::workspace::{bound_workspace_id_or_hash, stable_workspace_id};
use crate::db::{DbConnection, MemoryLinkRelation, MemoryLinkSource, StoredMemory};
use crate::models::memory::{
    extract_typed_memory_fields_json_with_redactor, typed_memory_fields_from_json,
};
use crate::models::{DomainError, MemoryKind};

pub const DECIDE_RECORD_SCHEMA_V1: &str = "ee.decide.record.v1";
pub const DECIDE_LIST_SCHEMA_V1: &str = "ee.decide.list.v1";
pub const DECIDE_REVISIT_SCHEMA_V1: &str = "ee.decide.revisit.v1";
pub const DEFAULT_REVISIT_WARNING_DAYS: u64 = 14;

const DECISION_TAG: &str = "decision";
const DECISION_TOPIC_TAG_PREFIX: &str = "decision-topic:";
const MAX_DECIDE_LIST_LIMIT: usize = 1_000;

#[derive(Clone, Debug)]
pub struct DecideRecordOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
    pub topic: &'a str,
    pub chosen: &'a str,
    pub alternatives: Vec<String>,
    pub rationale: &'a str,
    pub revisit_by: Option<&'a str>,
    pub supersedes: Option<&'a str>,
    pub dry_run: bool,
    pub actor: Option<&'a str>,
    pub now: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct DecideListOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
    pub about: Option<&'a str>,
    pub include_superseded: bool,
    pub limit: usize,
    pub now: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct DecideRevisitOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
    pub warning_days: Option<u64>,
    pub limit: usize,
    pub now: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecideMemoryRef {
    pub memory_id: String,
    pub valid_to: Option<String>,
    pub status: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecideItem {
    pub memory_id: String,
    pub topic: String,
    pub normalized_topic: String,
    pub chosen: String,
    pub alternatives: Vec<String>,
    pub options: Vec<String>,
    pub rationale: String,
    pub supersedes: Option<String>,
    pub chain_depth: u32,
    pub revisit_by: Option<String>,
    pub revisit_status: String,
    pub superseded: bool,
    pub valid_to: Option<String>,
    pub created_at: String,
}

impl DecideItem {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecideRecordReport {
    pub schema: &'static str,
    pub version: &'static str,
    pub status: String,
    pub dry_run: bool,
    pub persisted: bool,
    pub workspace_id: String,
    pub database_path: String,
    pub decision: DecideItem,
    pub superseded: Option<DecideMemoryRef>,
    pub memory_audit_id: Option<String>,
    pub memory_index_job_id: Option<String>,
    pub link_audit_id: Option<String>,
    pub expire_audit_id: Option<String>,
}

impl DecideRecordReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecideListReport {
    pub schema: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub database_path: String,
    pub about: Option<String>,
    pub include_superseded: bool,
    pub total_count: usize,
    pub returned_count: usize,
    pub truncated: bool,
    pub decisions: Vec<DecideItem>,
}

impl DecideListReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecideRevisitReport {
    pub schema: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub database_path: String,
    pub now: String,
    pub warning_days: u64,
    pub window_end: String,
    pub due_count: usize,
    pub returned_count: usize,
    pub truncated: bool,
    pub decisions: Vec<DecideItem>,
}

impl DecideRevisitReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DecisionFields {
    topic: String,
    normalized_topic: String,
    chosen: String,
    alternatives: Vec<String>,
    options: Vec<String>,
    rationale: String,
    supersedes: Option<String>,
    revisit_by: Option<String>,
}

#[derive(Clone, Debug)]
struct DecideScope {
    workspace_path: PathBuf,
    workspace_id: String,
    database_path: PathBuf,
}

#[must_use]
pub fn configured_revisit_warning_days(workspace_path: &Path) -> u64 {
    crate::config::workspace_config(workspace_path)
        .and_then(|config| config.decide.revisit_warning_days)
        .unwrap_or(DEFAULT_REVISIT_WARNING_DAYS)
}

#[must_use]
pub fn normalize_decision_topic(topic: &str) -> String {
    let stop_words = ["a", "an", "and", "for", "in", "of", "on", "or", "the", "to"];
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in topic.chars() {
        if ch.is_ascii_alphanumeric() {
            current.push(ch.to_ascii_lowercase());
        } else if !current.is_empty() {
            if !stop_words.contains(&current.as_str()) {
                tokens.push(std::mem::take(&mut current));
            }
            current.clear();
        }
    }
    if !current.is_empty() && !stop_words.contains(&current.as_str()) {
        tokens.push(current);
    }
    tokens.join("-")
}

pub fn parse_revisit_by(raw: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, DomainError> {
    let trimmed = raw.trim();
    if let Some(days) = trimmed
        .strip_prefix('+')
        .and_then(|value| value.strip_suffix('d'))
    {
        let days = days.parse::<i64>().map_err(|_| DomainError::Usage {
            message: format!("Invalid relative revisit interval: {raw}"),
            repair: Some(
                "Use an RFC3339 timestamp or a relative day value such as +90d.".to_owned(),
            ),
        })?;
        if !(1..=36_500).contains(&days) {
            return Err(DomainError::Usage {
                message: "Relative revisit interval must be from +1d through +36500d.".to_owned(),
                repair: Some("Use a shorter revisit interval such as +90d.".to_owned()),
            });
        }
        return Ok(now + TimeDelta::days(days));
    }

    DateTime::parse_from_rfc3339(trimmed)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| DomainError::Usage {
            message: format!("Invalid revisit timestamp `{raw}`: {error}"),
            repair: Some(
                "Use an RFC3339 timestamp or a relative day value such as +90d.".to_owned(),
            ),
        })
}

pub fn decide_record(options: &DecideRecordOptions<'_>) -> Result<DecideRecordReport, DomainError> {
    let mut scope = decide_scope(
        options.workspace_path,
        options.database_path,
        options.dry_run,
    )?;
    let now = options.now.unwrap_or_else(Utc::now);
    let fields = prepare_decision_fields(
        options.topic,
        options.chosen,
        &options.alternatives,
        options.rationale,
        options.revisit_by,
        options.supersedes,
        now,
    )?;

    let existing_heads = load_decisions(&mut scope, false, now)?;
    if let Some(supersedes) = options.supersedes {
        let Some(predecessor) = existing_heads
            .iter()
            .find(|item| item.memory_id == supersedes)
            .cloned()
            .or_else(|| {
                load_decision_by_id(&mut scope, supersedes, now)
                    .ok()
                    .flatten()
            })
        else {
            return Err(DomainError::NotFound {
                resource: "decision memory".to_owned(),
                id: supersedes.to_owned(),
                repair: Some("Run ee decide list --include-superseded --json.".to_owned()),
            });
        };
        if predecessor.normalized_topic != fields.normalized_topic {
            return Err(decide_usage_with_details(
                "decision_supersedes_topic_mismatch",
                "Superseded decision topic does not match the new decision topic.",
                json!({
                    "failureModeCode": "decision_supersedes_topic_mismatch",
                    "supersedes": supersedes,
                    "priorTopic": predecessor.topic,
                    "priorNormalizedTopic": predecessor.normalized_topic,
                    "newTopic": fields.topic,
                    "newNormalizedTopic": fields.normalized_topic,
                }),
            ));
        }
    } else if let Some(prior) = existing_heads
        .iter()
        .find(|item| item.normalized_topic == fields.normalized_topic)
    {
        return Err(decide_usage_with_details(
            "decision_topic_requires_supersedes",
            "A live decision already exists for this normalized topic; use --supersedes to replace it.",
            json!({
                "failureModeCode": "decision_topic_requires_supersedes",
                "priorMemoryId": prior.memory_id,
                "topic": fields.topic,
                "normalizedTopic": fields.normalized_topic,
                "suggestedCommand": format!(
                    "ee decide record {:?} --chosen <choice> --alternative <other> --rationale <why> --supersedes {} --json",
                    fields.topic, prior.memory_id
                ),
            }),
        ));
    }

    let tag_csv = decision_tag_csv(&fields.normalized_topic);
    let content = decision_content(&fields);
    let remember = remember_memory(&RememberMemoryOptions {
        workspace_path: &scope.workspace_path,
        database_path: Some(&scope.database_path),
        content: &content,
        workflow_id: None,
        level: "semantic",
        kind: "decision",
        tags: Some(&tag_csv),
        confidence: 0.85,
        source: None,
        allow_secret_mention: false,
        valid_from: None,
        valid_to: None,
        dry_run: options.dry_run,
        auto_link: false,
        propose_candidates: false,
    })?;

    let memory_id = remember.memory_id.to_string();
    let mut decision = DecideItem {
        memory_id: memory_id.clone(),
        topic: fields.topic.clone(),
        normalized_topic: fields.normalized_topic.clone(),
        chosen: fields.chosen.clone(),
        alternatives: fields.alternatives.clone(),
        options: fields.options.clone(),
        rationale: fields.rationale.clone(),
        supersedes: fields.supersedes.clone(),
        chain_depth: 0,
        revisit_by: fields.revisit_by.clone(),
        revisit_status: revisit_status(fields.revisit_by.as_deref(), now, None),
        superseded: false,
        valid_to: None,
        created_at: now.to_rfc3339_opts(SecondsFormat::Secs, true),
    };

    if options.dry_run {
        return Ok(DecideRecordReport {
            schema: DECIDE_RECORD_SCHEMA_V1,
            version: env!("CARGO_PKG_VERSION"),
            status: "would_record".to_owned(),
            dry_run: true,
            persisted: false,
            workspace_id: scope.workspace_id,
            database_path: scope.database_path.display().to_string(),
            decision,
            superseded: options.supersedes.map(|id| DecideMemoryRef {
                memory_id: id.to_owned(),
                valid_to: None,
                status: "would_supersede".to_owned(),
            }),
            memory_audit_id: None,
            memory_index_job_id: None,
            link_audit_id: None,
            expire_audit_id: None,
        });
    }

    store_exact_decision_fields(&scope.database_path, &memory_id, &fields)?;

    let mut superseded = None;
    let mut link_audit_id = None;
    let mut expire_audit_id = None;
    if let Some(target_id) = fields.supersedes.as_deref() {
        let link = update_memory_link(&MemoryLinkOptions {
            workspace_path: &scope.workspace_path,
            database_path: &scope.database_path,
            memory_id: &memory_id,
            mode: MemoryLinkMode::Create {
                target_memory_id: target_id.to_owned(),
                relation: MemoryLinkRelation::Supersedes,
                weight: 1.0,
                confidence: 1.0,
                directed: true,
                evidence_count: 1,
                source: MemoryLinkSource::Agent,
                metadata_json: Some(
                    json!({
                        "schema": "ee.decide.supersede.v1",
                        "normalizedTopic": fields.normalized_topic,
                    })
                    .to_string(),
                ),
            },
            actor: options.actor.or(Some("ee decide record")),
            dry_run: false,
            include_tombstoned: false,
        })?;
        link_audit_id = link.audit_id;

        let reason = format!(
            "superseded by decision {} for topic {}",
            memory_id, fields.normalized_topic
        );
        let expired = expire_memory(&ExpireMemoryOptions {
            workspace_path: &scope.workspace_path,
            database_path: &scope.database_path,
            memory_id: target_id,
            reason: Some(&reason),
            actor: options.actor.or(Some("ee decide record")),
            dry_run: false,
            include_tombstoned: false,
        })?;
        expire_audit_id = expired.audit_id.clone();
        superseded = Some(DecideMemoryRef {
            memory_id: target_id.to_owned(),
            valid_to: expired.valid_to,
            status: expired.status,
        });
    }

    let conn = open_decide_database(&scope.database_path)?;
    decision.chain_depth = supersede_chain_depth(&conn, &memory_id)?;
    if let Some(stored) = conn
        .get_memory(&memory_id)
        .map_err(|error| decide_storage_error(format!("Failed to reload decision: {error}")))?
    {
        decision.created_at = stored.created_at;
        decision.valid_to = stored.valid_to;
    }

    Ok(DecideRecordReport {
        schema: DECIDE_RECORD_SCHEMA_V1,
        version: env!("CARGO_PKG_VERSION"),
        status: "recorded".to_owned(),
        dry_run: false,
        persisted: remember.persisted,
        workspace_id: scope.workspace_id,
        database_path: scope.database_path.display().to_string(),
        decision,
        superseded,
        memory_audit_id: remember.audit_id,
        memory_index_job_id: remember.index_job_id,
        link_audit_id,
        expire_audit_id,
    })
}

pub fn decide_list(options: &DecideListOptions<'_>) -> Result<DecideListReport, DomainError> {
    let mut scope = decide_scope(options.workspace_path, options.database_path, false)?;
    let now = options.now.unwrap_or_else(Utc::now);
    let mut decisions = load_decisions(&mut scope, options.include_superseded, now)?;
    if let Some(about) = options.about.and_then(non_empty_trimmed) {
        let needle = about.to_ascii_lowercase();
        decisions.retain(|item| decision_item_matches_about(item, &needle));
    }
    sort_decisions_for_list(&mut decisions);
    let total_count = decisions.len();
    let limit = normalized_limit(options.limit);
    let truncated = decisions.len() > limit;
    decisions.truncate(limit);
    Ok(DecideListReport {
        schema: DECIDE_LIST_SCHEMA_V1,
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: scope.workspace_id,
        database_path: scope.database_path.display().to_string(),
        about: options.about.and_then(non_empty_trimmed).map(str::to_owned),
        include_superseded: options.include_superseded,
        total_count,
        returned_count: decisions.len(),
        truncated,
        decisions,
    })
}

pub fn decide_revisit(
    options: &DecideRevisitOptions<'_>,
) -> Result<DecideRevisitReport, DomainError> {
    let mut scope = decide_scope(options.workspace_path, options.database_path, false)?;
    let now = options.now.unwrap_or_else(Utc::now);
    let warning_days = options
        .warning_days
        .unwrap_or_else(|| configured_revisit_warning_days(&scope.workspace_path));
    let warning_days_i64 = i64::try_from(warning_days.min(36_500)).unwrap_or(36_500);
    let window_end = now + TimeDelta::days(warning_days_i64);
    let mut decisions = load_decisions(&mut scope, false, now)?;
    decisions.retain(|item| {
        item.revisit_by
            .as_deref()
            .and_then(parse_revisit_timestamp)
            .is_some_and(|revisit_by| revisit_by <= window_end)
    });
    for decision in &mut decisions {
        decision.revisit_status =
            revisit_status(decision.revisit_by.as_deref(), now, Some(window_end));
    }
    sort_decisions_for_revisit(&mut decisions);
    let due_count = decisions.len();
    let limit = normalized_limit(options.limit);
    let truncated = decisions.len() > limit;
    decisions.truncate(limit);
    Ok(DecideRevisitReport {
        schema: DECIDE_REVISIT_SCHEMA_V1,
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: scope.workspace_id,
        database_path: scope.database_path.display().to_string(),
        now: now.to_rfc3339_opts(SecondsFormat::Secs, true),
        warning_days,
        window_end: window_end.to_rfc3339_opts(SecondsFormat::Secs, true),
        due_count,
        returned_count: decisions.len(),
        truncated,
        decisions,
    })
}

fn decide_scope(
    workspace_path: &Path,
    database_path: Option<&Path>,
    dry_run: bool,
) -> Result<DecideScope, DomainError> {
    let workspace_path = resolve_workspace_path(workspace_path, dry_run)?;
    let workspace_id = stable_workspace_id(&workspace_path);
    let database_path = database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    Ok(DecideScope {
        workspace_path,
        workspace_id,
        database_path,
    })
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
        Err(_) if dry_run => Ok(absolute),
        Err(error) => Err(DomainError::Configuration {
            message: format!(
                "Failed to resolve workspace {}: {error}",
                absolute.display()
            ),
            repair: Some("ee init --workspace .".to_owned()),
        }),
    }
}

fn prepare_decision_fields(
    topic: &str,
    chosen: &str,
    alternatives: &[String],
    rationale: &str,
    revisit_by: Option<&str>,
    supersedes: Option<&str>,
    now: DateTime<Utc>,
) -> Result<DecisionFields, DomainError> {
    let topic = require_text("topic", topic)?;
    let normalized_topic = normalize_decision_topic(&topic);
    if normalized_topic.is_empty() {
        return Err(DomainError::Usage {
            message: "Decision topic must contain at least one ASCII letter or digit.".to_owned(),
            repair: Some("Use a concrete topic such as `storage backend for search`.".to_owned()),
        });
    }
    let chosen = require_text("chosen decision", chosen)?;
    let rationale = require_text("rationale", rationale)?;
    let mut options = Vec::new();
    push_unique_option(&mut options, &chosen);
    for alternative in alternatives {
        let alternative = require_text("alternative", alternative)?;
        push_unique_option(&mut options, &alternative);
    }
    if options.len() < 2 {
        return Err(DomainError::Usage {
            message: "A decision record requires at least one alternative besides --chosen."
                .to_owned(),
            repair: Some("Add --alternative <option> for each rejected option.".to_owned()),
        });
    }
    let alternatives = options
        .iter()
        .filter(|option| *option != &chosen)
        .cloned()
        .collect::<Vec<_>>();
    let revisit_by = revisit_by
        .map(|raw| {
            parse_revisit_by(raw, now)
                .map(|timestamp| timestamp.to_rfc3339_opts(SecondsFormat::Secs, true))
        })
        .transpose()?;
    let supersedes = supersedes.and_then(non_empty_trimmed).map(str::to_owned);
    Ok(DecisionFields {
        topic,
        normalized_topic,
        chosen,
        alternatives,
        options,
        rationale,
        supersedes,
        revisit_by,
    })
}

fn require_text(label: &str, value: &str) -> Result<String, DomainError> {
    non_empty_trimmed(value)
        .map(str::to_owned)
        .ok_or_else(|| DomainError::Usage {
            message: format!("Decision {label} cannot be empty."),
            repair: Some("Run ee decide record --help.".to_owned()),
        })
}

fn non_empty_trimmed(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn push_unique_option(options: &mut Vec<String>, value: &str) {
    if !options.iter().any(|existing| existing == value) {
        options.push(value.to_owned());
    }
}

fn decision_content(fields: &DecisionFields) -> String {
    let mut lines = vec![
        format!("Topic: {}", fields.topic),
        format!("Options: {}", fields.options.join(", ")),
        format!("Chosen: {}", fields.chosen),
        format!("Rationale: {}", fields.rationale),
    ];
    if let Some(supersedes) = &fields.supersedes {
        lines.push(format!("Supersedes: {supersedes}"));
    }
    if let Some(revisit_by) = &fields.revisit_by {
        lines.push(format!("Revisit by: {revisit_by}"));
    }
    lines.join("\n")
}

fn decision_tag_csv(normalized_topic: &str) -> String {
    format!("{DECISION_TAG},{}", decision_topic_tag(normalized_topic))
}

fn decision_topic_tag(normalized_topic: &str) -> String {
    let max_topic_bytes = 64 - DECISION_TOPIC_TAG_PREFIX.len();
    if normalized_topic.len() <= max_topic_bytes {
        return format!("{DECISION_TOPIC_TAG_PREFIX}{normalized_topic}");
    }
    let digest = blake3::hash(normalized_topic.as_bytes())
        .to_hex()
        .to_string();
    let prefix_len = max_topic_bytes.saturating_sub(9);
    format!(
        "{}{}-{}",
        DECISION_TOPIC_TAG_PREFIX,
        &normalized_topic[..prefix_len],
        &digest[..8]
    )
}

fn store_exact_decision_fields(
    database_path: &Path,
    memory_id: &str,
    fields: &DecisionFields,
) -> Result<(), DomainError> {
    let conn = open_decide_database(database_path)?;
    let typed_fields = json!({
        "options": fields.options,
        "chosen": fields.chosen,
        "rationale": fields.rationale,
        "supersedes": fields.supersedes,
        "revisit_by": fields.revisit_by,
    })
    .to_string();
    let changed = conn
        .set_memory_typed_fields_json(memory_id, Some(&typed_fields))
        .map_err(|error| {
            decide_storage_error(format!("Failed to store decision typed fields: {error}"))
        })?;
    if changed {
        Ok(())
    } else {
        Err(decide_storage_error(format!(
            "Failed to store decision typed fields for missing memory {memory_id}"
        )))
    }
}

fn load_decision_by_id(
    scope: &mut DecideScope,
    memory_id: &str,
    now: DateTime<Utc>,
) -> Result<Option<DecideItem>, DomainError> {
    if !scope.database_path.exists() {
        return Ok(None);
    }
    let conn = open_decide_database_read_only(&scope.database_path)?;
    scope.workspace_id = bound_workspace_id_or_hash(
        &conn,
        &scope.workspace_id,
        &[scope.workspace_path.as_path()],
    )?;
    let Some(memory) = conn
        .get_memory(memory_id)
        .map_err(|error| decide_storage_error(format!("Failed to query decision: {error}")))?
    else {
        return Ok(None);
    };
    if memory.workspace_id != scope.workspace_id || memory.kind != "decision" {
        return Ok(None);
    }
    memory_to_decide_item(&conn, &memory, now).map(Some)
}

fn load_decisions(
    scope: &mut DecideScope,
    include_superseded: bool,
    now: DateTime<Utc>,
) -> Result<Vec<DecideItem>, DomainError> {
    if !scope.database_path.exists() {
        return Ok(Vec::new());
    }
    let conn = open_decide_database_read_only(&scope.database_path)?;
    scope.workspace_id = bound_workspace_id_or_hash(
        &conn,
        &scope.workspace_id,
        &[scope.workspace_path.as_path()],
    )?;
    let memories = if include_superseded {
        conn.list_memories_for_retrieval(&scope.workspace_id, None, false)
    } else {
        conn.list_memories(&scope.workspace_id, None, false)
    }
    .map_err(|error| decide_storage_error(format!("Failed to list decisions: {error}")))?;

    let mut decisions = Vec::new();
    for memory in memories {
        if memory.kind == "decision" {
            decisions.push(memory_to_decide_item(&conn, &memory, now)?);
        }
    }
    Ok(decisions)
}

fn memory_to_decide_item(
    conn: &DbConnection,
    memory: &StoredMemory,
    now: DateTime<Utc>,
) -> Result<DecideItem, DomainError> {
    let fields = decision_fields_from_memory(conn, memory)?;
    let chain_depth = supersede_chain_depth(conn, &memory.id)?;
    Ok(DecideItem {
        memory_id: memory.id.clone(),
        topic: fields.topic,
        normalized_topic: fields.normalized_topic,
        chosen: fields.chosen,
        alternatives: fields.alternatives,
        options: fields.options,
        rationale: fields.rationale,
        supersedes: fields.supersedes,
        chain_depth,
        revisit_status: revisit_status(fields.revisit_by.as_deref(), now, None),
        revisit_by: fields.revisit_by,
        superseded: memory.valid_to.is_some(),
        valid_to: memory.valid_to.clone(),
        created_at: memory.created_at.clone(),
    })
}

fn decision_fields_from_memory(
    conn: &DbConnection,
    memory: &StoredMemory,
) -> Result<DecisionFields, DomainError> {
    let kind = MemoryKind::Decision;
    let typed_json = conn
        .get_memory_typed_fields_json(&memory.id)
        .map_err(|error| decide_storage_error(format!("Failed to load typed fields: {error}")))?
        .or_else(|| {
            extract_typed_memory_fields_json_with_redactor(&kind, &memory.content, str::to_owned)
                .ok()
                .flatten()
        });
    let fields = typed_json
        .as_deref()
        .map(|raw| typed_memory_fields_from_json(&kind, raw))
        .transpose()
        .map_err(|error| decide_storage_error(format!("Invalid decision typed fields: {error}")))?
        .unwrap_or_default();
    let topic = topic_from_content(&memory.content).unwrap_or_else(|| memory.content.clone());
    let normalized_topic = normalize_decision_topic(&topic);
    let chosen = string_field(&fields, "chosen").unwrap_or_default();
    let options = string_list_field(&fields, "options");
    let alternatives = options
        .iter()
        .filter(|option| **option != chosen)
        .cloned()
        .collect::<Vec<_>>();
    Ok(DecisionFields {
        topic,
        normalized_topic,
        chosen,
        alternatives,
        options,
        rationale: string_field(&fields, "rationale").unwrap_or_default(),
        supersedes: string_field(&fields, "supersedes"),
        revisit_by: string_field(&fields, "revisit_by"),
    })
}

fn topic_from_content(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        line.trim()
            .strip_prefix("Topic:")
            .and_then(non_empty_trimmed)
            .map(str::to_owned)
    })
}

fn string_field(fields: &BTreeMap<String, JsonValue>, name: &str) -> Option<String> {
    fields
        .get(name)
        .and_then(JsonValue::as_str)
        .and_then(non_empty_trimmed)
        .map(str::to_owned)
}

fn string_list_field(fields: &BTreeMap<String, JsonValue>, name: &str) -> Vec<String> {
    fields
        .get(name)
        .and_then(JsonValue::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(JsonValue::as_str)
                .filter_map(non_empty_trimmed)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn supersede_chain_depth(conn: &DbConnection, memory_id: &str) -> Result<u32, DomainError> {
    let mut current = memory_id.to_owned();
    let mut seen = BTreeSet::new();
    let mut depth = 0_u32;
    loop {
        if !seen.insert(current.clone()) || depth >= 64 {
            return Ok(depth);
        }
        let links = conn
            .list_memory_links_for_memory(&current, Some(MemoryLinkRelation::Supersedes))
            .map_err(|error| {
                decide_storage_error(format!("Failed to load supersede chain: {error}"))
            })?;
        let Some(next) = links
            .into_iter()
            .find(|link| link.src_memory_id == current && link.directed)
            .map(|link| link.dst_memory_id)
        else {
            return Ok(depth);
        };
        depth += 1;
        current = next;
    }
}

fn revisit_status(
    revisit_by: Option<&str>,
    now: DateTime<Utc>,
    near_due_window_end: Option<DateTime<Utc>>,
) -> String {
    let Some(revisit_by) = revisit_by.and_then(parse_revisit_timestamp) else {
        return "none".to_owned();
    };
    if revisit_by < now {
        "overdue".to_owned()
    } else if revisit_by == now {
        "due".to_owned()
    } else if near_due_window_end.is_some_and(|window_end| revisit_by <= window_end) {
        "near_due".to_owned()
    } else {
        "future".to_owned()
    }
}

fn parse_revisit_timestamp(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn decision_item_matches_about(item: &DecideItem, needle: &str) -> bool {
    item.topic.to_ascii_lowercase().contains(needle)
        || item.normalized_topic.contains(needle)
        || item.chosen.to_ascii_lowercase().contains(needle)
        || item.rationale.to_ascii_lowercase().contains(needle)
        || item
            .options
            .iter()
            .any(|option| option.to_ascii_lowercase().contains(needle))
}

fn sort_decisions_for_list(decisions: &mut [DecideItem]) {
    decisions.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
}

fn sort_decisions_for_revisit(decisions: &mut [DecideItem]) {
    decisions.sort_by(|left, right| {
        left.revisit_by
            .cmp(&right.revisit_by)
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
}

fn normalized_limit(limit: usize) -> usize {
    if limit == 0 {
        MAX_DECIDE_LIST_LIMIT
    } else {
        limit.min(MAX_DECIDE_LIST_LIMIT)
    }
}

fn open_decide_database(database_path: &Path) -> Result<DbConnection, DomainError> {
    let conn = DbConnection::open_file(database_path).map_err(|error| {
        decide_storage_error(format!(
            "Failed to open database {}: {error}",
            database_path.display()
        ))
    })?;
    conn.migrate()
        .map_err(|error| decide_storage_error(format!("Failed to migrate database: {error}")))?;
    Ok(conn)
}

fn open_decide_database_read_only(database_path: &Path) -> Result<DbConnection, DomainError> {
    DbConnection::open_file_read_only(database_path).map_err(|error| {
        decide_storage_error(format!(
            "Failed to open database {}: {error}",
            database_path.display()
        ))
    })
}

fn decide_storage_error(message: impl Into<String>) -> DomainError {
    DomainError::Storage {
        message: message.into(),
        repair: Some("ee doctor --json".to_owned()),
    }
}

fn decide_usage_with_details(code: &'static str, message: &str, details: JsonValue) -> DomainError {
    DomainError::UsageCodeWithDetails {
        code,
        message: message.to_owned(),
        repair: Some(
            "Run ee decide list --json and retry with --supersedes when replacing a decision."
                .to_owned(),
        ),
        details_json: details.to_string(),
    }
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

    fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
    where
        T: std::fmt::Debug + PartialEq,
    {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn workspace() -> Result<tempfile::TempDir, String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let report = crate::core::init::init_workspace(&crate::core::init::InitOptions {
            workspace_path: temp.path().to_path_buf(),
            dry_run: false,
            repair_plan: false,
            force: false,
            allow_symlink: false,
            skip_boilerplate: true,
        });
        if matches!(report.status, crate::core::init::InitStatus::Failed) {
            return Err(format!(
                "failed to initialize decision fixture: {:?}",
                report.action_errors
            ));
        }
        Ok(temp)
    }

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-15T12:00:00Z")
            .expect("fixed test timestamp parses")
            .with_timezone(&Utc)
    }

    fn record_options<'a>(
        workspace_path: &'a Path,
        topic: &'a str,
        chosen: &'a str,
        alternatives: Vec<String>,
    ) -> DecideRecordOptions<'a> {
        DecideRecordOptions {
            workspace_path,
            database_path: None,
            topic,
            chosen,
            alternatives,
            rationale: "It minimizes moving parts while preserving deterministic output.",
            revisit_by: None,
            supersedes: None,
            dry_run: false,
            actor: Some("test"),
            now: Some(fixed_now()),
        }
    }

    #[test]
    fn normalizes_topic_for_fork_detection() -> TestResult {
        ensure_equal(
            &normalize_decision_topic("The Storage Backend for Search!"),
            &"storage-backend-search".to_owned(),
            "normalized topic",
        )
    }

    #[test]
    fn revisit_by_accepts_relative_days_and_timezone_offsets() -> TestResult {
        let now = fixed_now();
        let relative = parse_revisit_by("+90d", now).map_err(|error| error.to_string())?;
        ensure_equal(
            &relative.to_rfc3339_opts(SecondsFormat::Secs, true),
            &"2026-09-13T12:00:00Z".to_owned(),
            "relative revisit timestamp",
        )?;
        let offset = parse_revisit_by("2026-06-16T09:30:00-04:00", now)
            .map_err(|error| error.to_string())?;
        ensure_equal(
            &offset.to_rfc3339_opts(SecondsFormat::Secs, true),
            &"2026-06-16T13:30:00Z".to_owned(),
            "offset revisit timestamp",
        )
    }

    #[test]
    fn record_refuses_same_topic_fork_without_supersedes() -> TestResult {
        let temp = workspace()?;
        let first = decide_record(&record_options(
            temp.path(),
            "Storage backend for search",
            "frankensearch",
            vec!["custom bm25".to_owned()],
        ))
        .map_err(|error| error.to_string())?;
        ensure(first.persisted, "first decision persists")?;

        let duplicate = decide_record(&record_options(
            temp.path(),
            "the storage backend, for search",
            "custom bm25",
            vec!["frankensearch".to_owned()],
        ))
        .expect_err("same normalized topic without supersedes should fail");
        ensure(
            duplicate.to_string().contains("already exists"),
            format!("unexpected duplicate error: {duplicate}"),
        )
    }

    #[test]
    fn supersede_chain_expires_predecessor_and_lists_head_only() -> TestResult {
        let temp = workspace()?;
        let first = decide_record(&record_options(
            temp.path(),
            "Context pack format",
            "markdown",
            vec!["json".to_owned()],
        ))
        .map_err(|error| error.to_string())?;
        let mut second_options = record_options(
            temp.path(),
            "context pack format",
            "json",
            vec!["markdown".to_owned()],
        );
        second_options.supersedes = Some(&first.decision.memory_id);
        let second = decide_record(&second_options).map_err(|error| error.to_string())?;

        ensure_equal(&second.decision.chain_depth, &1, "chain depth")?;
        ensure(
            second
                .superseded
                .as_ref()
                .and_then(|item| item.valid_to.as_ref())
                .is_some(),
            "predecessor valid_to is set",
        )?;

        let heads = decide_list(&DecideListOptions {
            workspace_path: temp.path(),
            database_path: None,
            about: None,
            include_superseded: false,
            limit: 10,
            now: Some(fixed_now()),
        })
        .map_err(|error| error.to_string())?;
        ensure_equal(&heads.total_count, &1, "head count")?;
        ensure_equal(
            &heads.decisions[0].memory_id,
            &second.decision.memory_id,
            "head id",
        )?;

        let history = decide_list(&DecideListOptions {
            workspace_path: temp.path(),
            database_path: None,
            about: None,
            include_superseded: true,
            limit: 10,
            now: Some(fixed_now()),
        })
        .map_err(|error| error.to_string())?;
        ensure_equal(&history.total_count, &2, "history count")
    }

    #[test]
    fn revisit_lists_due_and_near_due_decisions_deterministically() -> TestResult {
        let temp = workspace()?;
        let mut due = record_options(
            temp.path(),
            "Revisit model choice",
            "small local embedder",
            vec!["remote api".to_owned()],
        );
        due.revisit_by = Some("2026-06-15T10:00:00Z");
        decide_record(&due).map_err(|error| error.to_string())?;

        let mut near = record_options(
            temp.path(),
            "Revisit storage choice",
            "single sqlite db",
            vec!["sharded db".to_owned()],
        );
        near.revisit_by = Some("2026-06-18T12:00:00Z");
        decide_record(&near).map_err(|error| error.to_string())?;

        let mut later = record_options(
            temp.path(),
            "Revisit UI choice",
            "cli first",
            vec!["web first".to_owned()],
        );
        later.revisit_by = Some("2026-07-20T12:00:00Z");
        decide_record(&later).map_err(|error| error.to_string())?;

        let report = decide_revisit(&DecideRevisitOptions {
            workspace_path: temp.path(),
            database_path: None,
            warning_days: Some(7),
            limit: 10,
            now: Some(fixed_now()),
        })
        .map_err(|error| error.to_string())?;
        ensure_equal(&report.due_count, &2, "due count")?;
        ensure_equal(
            &report.decisions[0].topic,
            &"Revisit model choice".to_owned(),
            "oldest due first",
        )?;
        ensure_equal(
            &report.decisions[0].revisit_status,
            &"overdue".to_owned(),
            "overdue status",
        )?;
        ensure_equal(
            &report.decisions[1].topic,
            &"Revisit storage choice".to_owned(),
            "near-due second",
        )?;
        ensure_equal(
            &report.decisions[1].revisit_status,
            &"near_due".to_owned(),
            "near-due status",
        )
    }
}
