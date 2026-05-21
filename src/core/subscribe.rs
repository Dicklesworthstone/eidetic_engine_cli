//! Memory subscription delta producer.
//!
//! This is intentionally backed by the durable audit table first. The
//! lock-free audit MPSC lane can publish the same event shape later without
//! changing the cursor or filter contract.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Instant;

use chrono::{DateTime, TimeDelta, Utc};
use serde::Serialize;
use serde_json::{Value as JsonValue, json};
use sqlmodel_core::Value as SqlValue;

use super::workspace::stable_workspace_id;
use crate::db::{DatabaseConfig, DbConnection, audit_actions};
use crate::models::{DomainError, MemoryKind, MemoryLevel, Tag, TrustClass};

pub const MEMORY_DELTA_SCHEMA_V1: &str = "ee.memory.delta.v1";
pub const SUBSCRIBE_POLL_SCHEMA_V1: &str = "ee.subscribe.poll.v1";
pub const SUBSCRIBE_FILTER_INVALID: &str = "subscribe_filter_invalid";
pub const SUBSCRIBE_CURSOR_STALE: &str = "subscribe_cursor_stale";

const DEFAULT_LIMIT: u32 = 1_000;
const MAX_LIMIT: u32 = 10_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TagMatchMode {
    All,
    Any,
}

impl TagMatchMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Any => "any",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubscribeFilter {
    pub levels: BTreeSet<String>,
    pub kinds: BTreeSet<String>,
    pub tags: BTreeSet<String>,
    pub tag_mode: TagMatchMode,
    pub workspace_ids: BTreeSet<String>,
    pub min_trust_class: Option<TrustClass>,
    pub changed_fields: BTreeSet<String>,
    pub since_ms: Option<i64>,
}

impl Default for SubscribeFilter {
    fn default() -> Self {
        Self {
            levels: BTreeSet::new(),
            kinds: BTreeSet::new(),
            tags: BTreeSet::new(),
            tag_mode: TagMatchMode::All,
            workspace_ids: BTreeSet::new(),
            min_trust_class: None,
            changed_fields: BTreeSet::new(),
            since_ms: None,
        }
    }
}

impl SubscribeFilter {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
            && self.kinds.is_empty()
            && self.tags.is_empty()
            && self.workspace_ids.is_empty()
            && self.min_trust_class.is_none()
            && self.changed_fields.is_empty()
            && self.since_ms.is_none()
    }

    #[must_use]
    pub fn matches_delta(&self, delta: &MemoryDelta, since_cutoff: Option<DateTime<Utc>>) -> bool {
        if !self.levels.is_empty()
            && !delta
                .levels
                .iter()
                .any(|level| self.levels.contains(level.as_str()))
        {
            return false;
        }
        if !self.kinds.is_empty()
            && !delta
                .kinds
                .iter()
                .any(|kind| self.kinds.contains(kind.as_str()))
        {
            return false;
        }
        if !self.tags.is_empty() {
            let event_tags: BTreeSet<&str> = delta.tags.iter().map(String::as_str).collect();
            let tags_match = match self.tag_mode {
                TagMatchMode::All => self
                    .tags
                    .iter()
                    .all(|tag| event_tags.contains(tag.as_str())),
                TagMatchMode::Any => self
                    .tags
                    .iter()
                    .any(|tag| event_tags.contains(tag.as_str())),
            };
            if !tags_match {
                return false;
            }
        }
        if !self.workspace_ids.is_empty() {
            let Some(workspace_id) = delta.workspace_id.as_deref() else {
                return false;
            };
            if !self.workspace_ids.contains(workspace_id) {
                return false;
            }
        }
        if let Some(min_trust_class) = self.min_trust_class {
            let Some(trust_class) = delta.trust_class.as_deref() else {
                return false;
            };
            let Ok(actual) = TrustClass::from_str(trust_class) else {
                return false;
            };
            if trust_rank(actual) < trust_rank(min_trust_class) {
                return false;
            }
        }
        if !self.changed_fields.is_empty() {
            let event_fields: BTreeSet<&str> =
                delta.changed_fields.iter().map(String::as_str).collect();
            if !self
                .changed_fields
                .iter()
                .all(|field| event_fields.contains(field.as_str()))
            {
                return false;
            }
        }
        if let Some(cutoff) = since_cutoff {
            let Ok(timestamp) = DateTime::parse_from_rfc3339(&delta.occurred_at) else {
                return false;
            };
            if timestamp.with_timezone(&Utc) < cutoff {
                return false;
            }
        }
        true
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubscribePollOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
    pub cursor: u64,
    pub filter: SubscribeFilter,
    pub limit: u32,
}

impl<'a> SubscribePollOptions<'a> {
    #[must_use]
    pub fn new(workspace_path: &'a Path) -> Self {
        Self {
            workspace_path,
            database_path: None,
            cursor: 0,
            filter: SubscribeFilter::default(),
            limit: DEFAULT_LIMIT,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDelta {
    pub schema: &'static str,
    pub cursor: u64,
    pub kind: String,
    pub memory_id: String,
    pub levels: Vec<String>,
    pub kinds: Vec<String>,
    pub tags: Vec<String>,
    pub workspace_id: Option<String>,
    pub trust_class: Option<String>,
    pub agent_name: Option<String>,
    pub changed_fields: Vec<String>,
    pub audit_id: String,
    pub occurred_at: String,
}

impl MemoryDelta {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubscribePollReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub database_path: PathBuf,
    pub cursor: u64,
    pub next_cursor: u64,
    pub delta_count: usize,
    pub deltas: Vec<MemoryDelta>,
    pub degraded: Vec<SubscribeDegradation>,
}

impl SubscribePollReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "command": self.command,
            "version": self.version,
            "workspaceId": self.workspace_id,
            "databasePath": self.database_path.to_string_lossy(),
            "cursor": self.cursor,
            "nextCursor": self.next_cursor,
            "deltaCount": self.delta_count,
            "deltas": self.deltas,
            "degraded": self.degraded,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RawAuditDelta {
    cursor: u64,
    audit_id: String,
    workspace_id: Option<String>,
    occurred_at: String,
    actor: Option<String>,
    action: String,
    memory_id: String,
    level: Option<String>,
    kind: Option<String>,
    trust_class: Option<String>,
}

pub fn parse_subscribe_filter(raw: Option<&str>) -> Result<SubscribeFilter, DomainError> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(SubscribeFilter::default());
    };
    let mut filter = SubscribeFilter::default();

    for token in raw
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        let Some((key, value)) = token.split_once('=') else {
            return filter_error(
                format!("Invalid subscribe filter token `{token}`."),
                "Use KEY=value tokens, for example LEVEL=procedural,TAG=release.",
            );
        };
        let key = normalize_key(key);
        let value = value.trim();
        if value.is_empty() {
            return filter_error(
                format!("Subscribe filter `{key}` has an empty value."),
                "Provide a non-empty filter value.",
            );
        }
        match key.as_str() {
            "level" => {
                for item in split_filter_values(value) {
                    let level = MemoryLevel::from_str(&item).map_err(|error| {
                        subscribe_filter_domain_error(
                            format!("Invalid memory level `{item}`: {error}"),
                            "Use working, episodic, semantic, or procedural.",
                        )
                    })?;
                    filter.levels.insert(level.as_str().to_owned());
                }
            }
            "kind" => {
                for item in split_filter_values(value) {
                    let kind = MemoryKind::from_str(&item).map_err(|error| {
                        subscribe_filter_domain_error(
                            format!("Invalid memory kind `{item}`: {error}"),
                            "Use a known memory kind or a valid custom kind identifier.",
                        )
                    })?;
                    filter.kinds.insert(kind.as_str().to_owned());
                }
            }
            "tag" | "tags" => {
                for item in split_filter_values(value) {
                    let tag = Tag::from_str(&item).map_err(|error| {
                        subscribe_filter_domain_error(
                            format!("Invalid tag `{item}`: {error}"),
                            "Use normalized tag identifiers such as release or ci:build.",
                        )
                    })?;
                    filter.tags.insert(tag.as_str().to_owned());
                }
            }
            "tagmode" | "tag_mode" | "tagsmode" | "tags_mode" => {
                filter.tag_mode = match value.to_ascii_lowercase().as_str() {
                    "all" | "and" => TagMatchMode::All,
                    "any" | "or" => TagMatchMode::Any,
                    other => {
                        return filter_error(
                            format!("Invalid tag mode `{other}`."),
                            "Use TAG_MODE=all or TAG_MODE=any.",
                        );
                    }
                };
            }
            "workspaceid" | "workspace_id" => {
                for item in split_filter_values(value) {
                    filter.workspace_ids.insert(item);
                }
            }
            "trustclass" | "trust_class" | "mintrustclass" | "min_trust_class" => {
                let trust_class = TrustClass::from_str(&value.to_ascii_lowercase()).map_err(
                    |error| {
                        subscribe_filter_domain_error(
                            format!("Invalid trust class `{value}`: {error}"),
                            "Use human_explicit, agent_validated, agent_assertion, cass_evidence, or legacy_import.",
                        )
                    },
                )?;
                filter.min_trust_class = Some(trust_class);
            }
            "changed" | "changedfield" | "changedfields" | "changed_field" | "changed_fields" => {
                for item in split_filter_values(value) {
                    filter.changed_fields.insert(normalize_changed_field(&item));
                }
            }
            "sincems" | "since_ms" => {
                let since_ms = value.parse::<i64>().map_err(|error| {
                    subscribe_filter_domain_error(
                        format!("Invalid sinceMs value `{value}`: {error}"),
                        "Use a non-negative integer number of milliseconds.",
                    )
                })?;
                if since_ms < 0 {
                    return filter_error(
                        format!("Invalid sinceMs value `{value}`."),
                        "Use a non-negative integer number of milliseconds.",
                    );
                }
                filter.since_ms = Some(since_ms);
            }
            other => {
                return filter_error(
                    format!("Unknown subscribe filter key `{other}`."),
                    "Supported keys: LEVEL, KIND, TAG, TAG_MODE, WORKSPACE_ID, TRUST_CLASS, CHANGED_FIELDS, SINCE_MS.",
                );
            }
        }
    }

    Ok(filter)
}

pub fn poll_memory_deltas(
    options: &SubscribePollOptions<'_>,
) -> Result<SubscribePollReport, DomainError> {
    let started = Instant::now();
    let workspace_path = resolve_workspace_path(options.workspace_path)?;
    let workspace_id = stable_workspace_id(&workspace_path);
    let database_path = options
        .database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    let limit = options.limit.clamp(1, MAX_LIMIT);
    let mut degraded = Vec::new();

    if !database_path.exists() {
        return Err(DomainError::Storage {
            message: format!("Database not found at {}", database_path.display()),
            repair: Some("Run `ee init --workspace . --json` first.".to_owned()),
        });
    }

    let connection =
        DbConnection::open(DatabaseConfig::file(database_path.clone())).map_err(|error| {
            DomainError::Storage {
                message: format!("Failed to open database: {error}"),
                repair: Some("Run `ee doctor --json` for storage diagnostics.".to_owned()),
            }
        })?;
    connection.migrate().map_err(|error| DomainError::Storage {
        message: format!("Failed to migrate database: {error}"),
        repair: Some("Run `ee migrate run --workspace . --json`.".to_owned()),
    })?;

    let high_watermark = audit_high_watermark(&connection)?;
    if options.cursor > high_watermark {
        degraded.push(SubscribeDegradation {
            code: SUBSCRIBE_CURSOR_STALE.to_owned(),
            severity: "warning".to_owned(),
            message: format!(
                "Requested cursor {} is ahead of the audit high watermark {}.",
                options.cursor, high_watermark
            ),
            repair: "Persist the returned nextCursor before polling again.".to_owned(),
        });
    }

    let rows = connection
        .query(
            "SELECT a.rowid, a.id, a.workspace_id, a.timestamp, a.actor, a.action, a.target_id, \
                    m.level, m.kind, m.trust_class \
             FROM audit_log a \
             LEFT JOIN memories m ON m.id = a.target_id \
             WHERE a.rowid > ?1 \
               AND (a.target_type = 'memory' OR a.action LIKE 'memory.%' OR a.action = ?3) \
               AND a.target_id IS NOT NULL \
             ORDER BY a.rowid ASC \
             LIMIT ?2",
            &[
                cursor_sql_value(options.cursor),
                SqlValue::BigInt(i64::from(limit)),
                SqlValue::Text(audit_actions::TRUST_CLASS_TRANSITION.to_owned()),
            ],
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to read memory audit deltas: {error}"),
            repair: Some("Run `ee doctor --json` for storage diagnostics.".to_owned()),
        })?;

    let since_cutoff = options.filter.since_ms.and_then(|milliseconds| {
        Utc::now().checked_sub_signed(TimeDelta::milliseconds(milliseconds))
    });
    let mut next_cursor = options.cursor.min(high_watermark);
    let mut deltas = Vec::new();
    for row in rows {
        let raw = raw_delta_from_row(&row)?;
        next_cursor = next_cursor.max(raw.cursor);
        let delta = materialize_delta(&connection, raw)?;
        if options.filter.matches_delta(&delta, since_cutoff) {
            deltas.push(delta);
        }
    }

    let degraded_codes: Vec<&str> = degraded.iter().map(|entry| entry.code.as_str()).collect();
    tracing::info!(
        target: "ee::subscribe",
        surface = "subscribe",
        bead_id = "bd-ub249",
        workspace_id,
        request_id = "subscribe_poll",
        cursor = options.cursor,
        deltas_emitted = deltas.len(),
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        degraded_codes = ?degraded_codes,
        "subscribe poll complete"
    );

    Ok(SubscribePollReport {
        schema: SUBSCRIBE_POLL_SCHEMA_V1,
        command: "subscribe poll",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id,
        database_path,
        cursor: options.cursor,
        next_cursor,
        delta_count: deltas.len(),
        deltas,
        degraded,
    })
}

fn raw_delta_from_row(row: &sqlmodel_core::Row) -> Result<RawAuditDelta, DomainError> {
    let cursor = row
        .get(0)
        .and_then(|value| value.as_i64())
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| malformed_row("audit row cursor"))?;
    let audit_id = required_text(row, 1, "audit id")?;
    let workspace_id = optional_text(row, 2);
    let occurred_at = required_text(row, 3, "audit timestamp")?;
    let actor = optional_text(row, 4);
    let action = required_text(row, 5, "audit action")?;
    let memory_id = required_text(row, 6, "memory id")?;
    Ok(RawAuditDelta {
        cursor,
        audit_id,
        workspace_id,
        occurred_at,
        actor,
        action,
        memory_id,
        level: optional_text(row, 7),
        kind: optional_text(row, 8),
        trust_class: optional_text(row, 9),
    })
}

fn materialize_delta(
    connection: &DbConnection,
    raw: RawAuditDelta,
) -> Result<MemoryDelta, DomainError> {
    let tags = memory_tags(connection, &raw.memory_id)?;
    let levels = raw.level.iter().cloned().collect();
    let kinds = raw.kind.iter().cloned().collect();
    Ok(MemoryDelta {
        schema: MEMORY_DELTA_SCHEMA_V1,
        cursor: raw.cursor,
        kind: classify_delta_kind(&raw.action).to_owned(),
        memory_id: raw.memory_id,
        levels,
        kinds,
        tags,
        workspace_id: raw.workspace_id,
        trust_class: raw.trust_class,
        agent_name: raw.actor,
        changed_fields: classify_changed_fields(&raw.action),
        audit_id: raw.audit_id,
        occurred_at: raw.occurred_at,
    })
}

fn memory_tags(connection: &DbConnection, memory_id: &str) -> Result<Vec<String>, DomainError> {
    let rows = connection
        .query(
            "SELECT tag FROM memory_tags WHERE memory_id = ?1 ORDER BY tag ASC",
            &[SqlValue::Text(memory_id.to_owned())],
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to read memory tags: {error}"),
            repair: Some("Run `ee doctor --json` for storage diagnostics.".to_owned()),
        })?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            row.get(0)
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        })
        .collect())
}

fn audit_high_watermark(connection: &DbConnection) -> Result<u64, DomainError> {
    let rows = connection
        .query("SELECT COALESCE(MAX(rowid), 0) FROM audit_log", &[])
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to read audit high watermark: {error}"),
            repair: Some("Run `ee doctor --json` for storage diagnostics.".to_owned()),
        })?;
    Ok(rows
        .first()
        .and_then(|row| row.get(0).and_then(|value| value.as_i64()))
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(0))
}

#[must_use]
pub fn classify_delta_kind(action: &str) -> &'static str {
    match action {
        audit_actions::MEMORY_CREATE => "created",
        audit_actions::MEMORY_LEVEL_TRANSITION => "level_transitioned",
        audit_actions::MEMORY_EXPIRE => "expired",
        audit_actions::MEMORY_TOMBSTONE | audit_actions::MEMORY_DECAY_TOMBSTONE => "tombstoned",
        audit_actions::MEMORY_UNTOMBSTONE
        | audit_actions::MEMORY_REVISE
        | audit_actions::MEMORY_UPDATE
        | audit_actions::MEMORY_TAG_ADD
        | audit_actions::MEMORY_TAG_REMOVE
        | audit_actions::MEMORY_TAG_SET
        | audit_actions::MEMORY_SCORE_DECAY
        | audit_actions::MEMORY_DECAY_DEMOTE
        | audit_actions::MEMORY_BAYES_POSTERIOR_UPDATED
        | audit_actions::OUTCOME_BAYES_UPDATE
        | audit_actions::TRUST_CLASS_TRANSITION => "updated",
        _ if action.contains("level") => "level_transitioned",
        _ if action.contains("expire") => "expired",
        _ if action.contains("tombstone") => "tombstoned",
        _ if action.contains("create") => "created",
        _ => "updated",
    }
}

#[must_use]
pub fn classify_changed_fields(action: &str) -> Vec<String> {
    let fields: &[&str] = match action {
        audit_actions::MEMORY_CREATE => &[
            "content_hash",
            "confidence",
            "utility",
            "level",
            "kind",
            "tags",
        ],
        audit_actions::MEMORY_LEVEL_TRANSITION | audit_actions::MEMORY_DECAY_DEMOTE => &["level"],
        audit_actions::MEMORY_EXPIRE => &["valid_to"],
        audit_actions::MEMORY_TOMBSTONE | audit_actions::MEMORY_DECAY_TOMBSTONE => {
            &["tombstoned_at"]
        }
        audit_actions::MEMORY_UNTOMBSTONE => &["tombstoned_at"],
        audit_actions::MEMORY_TAG_ADD
        | audit_actions::MEMORY_TAG_REMOVE
        | audit_actions::MEMORY_TAG_SET => &["tags"],
        audit_actions::MEMORY_SCORE_DECAY => &["confidence", "utility", "importance"],
        audit_actions::MEMORY_BAYES_POSTERIOR_UPDATED | audit_actions::OUTCOME_BAYES_UPDATE => {
            &["confidence", "trust_class"]
        }
        audit_actions::TRUST_CLASS_TRANSITION => &["trust_class"],
        audit_actions::MEMORY_REVISE | audit_actions::MEMORY_UPDATE => {
            &["content_hash", "confidence", "trust_class"]
        }
        _ => &["content_hash"],
    };
    fields.iter().map(|field| (*field).to_owned()).collect()
}

fn cursor_sql_value(cursor: u64) -> SqlValue {
    SqlValue::BigInt(i64::try_from(cursor).unwrap_or(i64::MAX))
}

fn required_text(
    row: &sqlmodel_core::Row,
    index: usize,
    field: &'static str,
) -> Result<String, DomainError> {
    row.get(index)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .ok_or_else(|| malformed_row(field))
}

fn optional_text(row: &sqlmodel_core::Row, index: usize) -> Option<String> {
    row.get(index)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

fn malformed_row(field: &'static str) -> DomainError {
    DomainError::Storage {
        message: format!("Malformed memory audit delta row: missing {field}."),
        repair: Some("Run `ee doctor --json` for storage diagnostics.".to_owned()),
    }
}

fn resolve_workspace_path(path: &Path) -> Result<PathBuf, DomainError> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) => Err(DomainError::Configuration {
            message: format!("Failed to resolve workspace {}: {error}", path.display()),
            repair: Some("Pass --workspace with an existing ee workspace path.".to_owned()),
        }),
    }
}

fn trust_rank(trust_class: TrustClass) -> u8 {
    match trust_class {
        TrustClass::HumanExplicit => 5,
        TrustClass::AgentValidated => 4,
        TrustClass::AgentAssertion => 3,
        TrustClass::CassEvidence => 2,
        TrustClass::LegacyImport => 1,
    }
}

fn normalize_key(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|ch| *ch != '-' && !ch.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn split_filter_values(value: &str) -> Vec<String> {
    value
        .split(['|', '+', ';'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| item.to_ascii_lowercase())
        .collect()
}

fn normalize_changed_field(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for ch in value.trim().chars() {
        if ch == '-' || ch == ' ' {
            output.push('_');
        } else {
            output.extend(ch.to_lowercase());
        }
    }
    output
}

fn filter_error<T>(message: String, repair: &'static str) -> Result<T, DomainError> {
    Err(subscribe_filter_domain_error(message, repair))
}

fn subscribe_filter_domain_error(message: String, repair: &'static str) -> DomainError {
    DomainError::UsageCodeWithDetails {
        code: SUBSCRIBE_FILTER_INVALID,
        message,
        repair: Some(repair.to_owned()),
        details_json: json!({
            "recovery": [{
                "priority": 1,
                "kind": "command",
                "command": "ee subscribe poll --cursor 0 --filter LEVEL=procedural,TAG=release --json"
            }]
        })
        .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_delta() -> MemoryDelta {
        MemoryDelta {
            schema: MEMORY_DELTA_SCHEMA_V1,
            cursor: 7,
            kind: "created".to_owned(),
            memory_id: "mem_0123456789abcdefghijklmno".to_owned(),
            levels: vec!["procedural".to_owned()],
            kinds: vec!["rule".to_owned()],
            tags: vec!["release".to_owned(), "ci".to_owned()],
            workspace_id: Some("wsp_test".to_owned()),
            trust_class: Some("agent_validated".to_owned()),
            agent_name: Some("agent-a".to_owned()),
            changed_fields: vec!["content_hash".to_owned(), "level".to_owned()],
            audit_id: "audit_0123456789abcdefghijklmnop".to_owned(),
            occurred_at: "2026-05-20T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn parse_subscribe_filter_accepts_taxonomy_tokens() {
        let filter = parse_subscribe_filter(Some(
            "LEVEL=procedural,KIND=rule,TAG=release+ci,TAG_MODE=all,TRUST_CLASS=agent_assertion,CHANGED_FIELDS=level",
        ))
        .expect("filter parses");

        assert!(filter.levels.contains("procedural"));
        assert!(filter.kinds.contains("rule"));
        assert!(filter.tags.contains("release"));
        assert!(filter.tags.contains("ci"));
        assert_eq!(filter.tag_mode, TagMatchMode::All);
        assert_eq!(filter.min_trust_class, Some(TrustClass::AgentAssertion));
        assert!(filter.changed_fields.contains("level"));
    }

    #[test]
    fn filter_excludes_non_matching_level_tag_trust_and_changed_field() {
        let delta = sample_delta();

        let level = parse_subscribe_filter(Some("LEVEL=episodic")).expect("level filter parses");
        assert!(!level.matches_delta(&delta, None));

        let tags = parse_subscribe_filter(Some("TAG=release+missing")).expect("tag filter parses");
        assert!(!tags.matches_delta(&delta, None));

        let trust =
            parse_subscribe_filter(Some("TRUST_CLASS=human_explicit")).expect("trust parses");
        assert!(!trust.matches_delta(&delta, None));

        let changed =
            parse_subscribe_filter(Some("CHANGED_FIELDS=trust_class")).expect("changed parses");
        assert!(!changed.matches_delta(&delta, None));
    }

    #[test]
    fn filter_matches_any_tag_mode() {
        let delta = sample_delta();
        let filter = parse_subscribe_filter(Some("TAG=release+missing,TAG_MODE=any"))
            .expect("filter parses");
        assert!(filter.matches_delta(&delta, None));
    }

    #[test]
    fn change_classification_is_stable() {
        assert_eq!(
            classify_delta_kind(audit_actions::MEMORY_LEVEL_TRANSITION),
            "level_transitioned"
        );
        assert_eq!(classify_delta_kind(audit_actions::MEMORY_EXPIRE), "expired");
        assert_eq!(
            classify_changed_fields(audit_actions::MEMORY_TAG_SET),
            vec!["tags".to_owned()]
        );
    }
}
