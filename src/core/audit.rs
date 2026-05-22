//! Operation audit timeline and inspection.
//!
//! Audit commands are read-only projections over the persisted `audit_log`
//! table. Mutating commands append rows through `ee-db`; this module only
//! lists, shows, diffs, and verifies those rows.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};

use crate::core::why::{DedupLinkEvidence, find_embed_dedup_link};
use crate::db::{DbConnection, StoredAuditEntry, compute_audit_row_hash};
use crate::models::{DomainError, ProducerMetadata};

/// Schema for audit timeline response.
pub const AUDIT_TIMELINE_SCHEMA_V1: &str = "ee.audit.timeline.v1";

/// Schema for audit show response.
pub const AUDIT_SHOW_SCHEMA_V1: &str = "ee.audit.show.v1";

/// Schema for audit diff response.
pub const AUDIT_DIFF_SCHEMA_V1: &str = "ee.audit.diff.v1";

/// Schema for audit verify response.
pub const AUDIT_VERIFY_SCHEMA_V1: &str = "ee.audit.verify.v1";

/// Degraded-code emitted when one shard-local audit chain is broken.
pub const SHARD_CHAIN_MISMATCH_CODE: &str = "shard_chain_mismatch";

/// Read-only audit-side dedup-link projection helper (bd-1iltv.3).
///
/// Wraps [`crate::core::why::find_embed_dedup_link`] so audit consumers
/// can embed dedupLink evidence into an audit-render or support-bundle
/// surface without duplicating the JSON-parsing path. Returns `None`
/// when the audit entry does not target a memory or when no
/// `memory_links` row with schema `ee.embed_dedup.link.v1` exists for
/// the targeted memory — the same honest-degradation contract pinned
/// by the why surface tests.
///
/// Only audit entries whose `target_type` resolves to a memory row are
/// considered: that keeps the lookup cheap and avoids issuing a
/// memory_links query for every audit row (e.g. workspace creates,
/// pack persists) that has no dedup-link semantics at all.
#[must_use]
pub fn audit_entry_dedup_link(
    conn: &DbConnection,
    entry: &AuditTimelineEntry,
) -> Option<DedupLinkEvidence> {
    if entry.target_type.as_deref() != Some("memory") {
        return None;
    }
    let memory_id = entry.target_id.as_deref()?;
    if memory_id.is_empty() {
        return None;
    }
    find_embed_dedup_link(conn, memory_id)
}

/// Options for listing the audit timeline.
#[derive(Clone, Debug, Default)]
pub struct AuditTimelineOptions {
    pub workspace: PathBuf,
    pub database_path: Option<PathBuf>,
    pub since: Option<String>,
    pub surface: Option<String>,
    pub action: Option<String>,
    pub limit: u32,
    pub cursor: Option<String>,
}

/// Options for showing one audit row.
#[derive(Clone, Debug, Default)]
pub struct AuditShowOptions {
    pub workspace: PathBuf,
    pub database_path: Option<PathBuf>,
    pub audit_id: String,
}

/// Options for showing audit rows between two timestamps.
#[derive(Clone, Debug, Default)]
pub struct AuditDiffOptions {
    pub workspace: PathBuf,
    pub database_path: Option<PathBuf>,
    pub from: String,
    pub to: String,
}

/// Options for verifying audit integrity.
#[derive(Clone, Debug, Default)]
pub struct AuditVerifyOptions {
    pub workspace: PathBuf,
    pub database_path: Option<PathBuf>,
    pub since: Option<String>,
    pub until: Option<String>,
}

/// Audit rows read from one shard database.
#[derive(Clone, Debug, Default)]
pub struct AuditShardEntries {
    pub shard_id: String,
    pub entries: Vec<StoredAuditEntry>,
}

/// Options for merging multiple shard-local audit chains into one timeline.
#[derive(Clone, Debug, Default)]
pub struct ShardedAuditTimelineOptions {
    pub shards: Vec<AuditShardEntries>,
    pub since: Option<String>,
    pub surface: Option<String>,
    pub action: Option<String>,
    pub limit: u32,
    pub cursor: Option<String>,
}

/// Options for verifying multiple shard-local audit chains.
#[derive(Clone, Debug, Default)]
pub struct ShardedAuditVerifyOptions {
    pub shards: Vec<AuditShardEntries>,
    pub since: Option<String>,
    pub until: Option<String>,
}

/// Summary of a persisted audit row.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditTimelineEntry {
    pub id: String,
    pub timestamp: String,
    pub actor: Option<String>,
    pub surface: String,
    pub mutation_kind: String,
    pub before_hash: Option<String>,
    pub after_hash: Option<String>,
    pub prev_row_hash: Option<String>,
    pub this_row_hash: Option<String>,
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_id: Option<String>,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub producer: ProducerMetadata,
    pub details: Option<JsonValue>,
}

/// Pagination metadata for timeline.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TimelinePagination {
    pub total_count: u32,
    pub returned_count: u32,
    pub has_more: bool,
    pub next_cursor: Option<String>,
}

/// Report from listing the audit timeline.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditTimelineReport {
    pub schema: String,
    pub entries: Vec<AuditTimelineEntry>,
    pub pagination: TimelinePagination,
}

impl AuditTimelineReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }
}

/// Linked target snapshot included by `ee audit show`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkedSnapshot {
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub found: bool,
    pub snapshot_hash: Option<String>,
    pub snapshot: Option<JsonValue>,
}

/// Report from showing an audit row.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditShowReport {
    pub schema: String,
    pub row: AuditTimelineEntry,
    pub linked_snapshot: LinkedSnapshot,
    pub hash_chain_valid: bool,
}

impl AuditShowReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }
}

/// Report from showing audit mutations in a time window.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditDiffReport {
    pub schema: String,
    pub from: String,
    pub to: String,
    pub entries: Vec<AuditTimelineEntry>,
    pub row_count: u32,
}

impl AuditDiffReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }
}

/// Verification issue found while walking the chain.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationIssue {
    pub code: String,
    pub audit_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_id: Option<String>,
    pub message: String,
}

/// Per-shard verification summary embedded in sharded audit reports.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditShardVerifyReport {
    pub shard_id: String,
    pub integrity_ok: bool,
    pub rows: u32,
    pub last_hash: Option<String>,
    pub first_break: Option<String>,
    pub issues: Vec<VerificationIssue>,
}

/// Report from verifying audit integrity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditVerifyReport {
    pub schema: String,
    pub integrity_ok: bool,
    pub rows: u32,
    pub last_hash: Option<String>,
    pub first_break: Option<String>,
    pub issues: Vec<VerificationIssue>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub shard_count: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub broken_shard_count: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shards: Vec<AuditShardVerifyReport>,
}

impl AuditVerifyReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }
}

/// List persisted operations in chronological order.
pub fn list_timeline(options: &AuditTimelineOptions) -> Result<AuditTimelineReport, DomainError> {
    let entries = load_entries(&options.workspace, options.database_path.as_deref())?;
    let since = parse_optional_instant(options.since.as_deref(), "since")?;
    let offset = parse_cursor(options.cursor.as_deref())?;
    let filtered = filter_entries(
        entries,
        since,
        None,
        options.surface.as_deref(),
        options.action.as_deref(),
    )?;
    let total_count = u32::try_from(filtered.len()).unwrap_or(u32::MAX);
    let limit = usize::try_from(options.limit.max(1)).unwrap_or(usize::MAX);
    let page: Vec<_> = filtered.into_iter().skip(offset).take(limit).collect();
    let next_offset = offset.saturating_add(page.len());
    let has_more = next_offset < usize::try_from(total_count).unwrap_or(usize::MAX);

    Ok(AuditTimelineReport {
        schema: AUDIT_TIMELINE_SCHEMA_V1.to_owned(),
        pagination: TimelinePagination {
            total_count,
            returned_count: u32::try_from(page.len()).unwrap_or(u32::MAX),
            has_more,
            next_cursor: has_more.then(|| next_offset.to_string()),
        },
        entries: page.into_iter().map(AuditTimelineEntry::from).collect(),
    })
}

/// Merge persisted operations from shard-local audit chains.
pub fn list_sharded_timeline(
    options: &ShardedAuditTimelineOptions,
) -> Result<AuditTimelineReport, DomainError> {
    let since = parse_optional_instant(options.since.as_deref(), "since")?;
    let offset = parse_cursor(options.cursor.as_deref())?;
    let mut entries = sharded_entries(options.shards.as_slice());
    sort_sharded_entries_chronological(&mut entries);
    let filtered = filter_sharded_entries(
        entries,
        since,
        None,
        options.surface.as_deref(),
        options.action.as_deref(),
    )?;
    let total_count = u32::try_from(filtered.len()).unwrap_or(u32::MAX);
    let limit = usize::try_from(options.limit.max(1)).unwrap_or(usize::MAX);
    let page: Vec<_> = filtered.into_iter().skip(offset).take(limit).collect();
    let next_offset = offset.saturating_add(page.len());
    let has_more = next_offset < usize::try_from(total_count).unwrap_or(usize::MAX);

    Ok(AuditTimelineReport {
        schema: AUDIT_TIMELINE_SCHEMA_V1.to_owned(),
        pagination: TimelinePagination {
            total_count,
            returned_count: u32::try_from(page.len()).unwrap_or(u32::MAX),
            has_more,
            next_cursor: has_more.then(|| next_offset.to_string()),
        },
        entries: page
            .into_iter()
            .map(|entry| AuditTimelineEntry::from_sharded(entry.entry, Some(entry.shard_id)))
            .collect(),
    })
}

/// Show one persisted audit row and a snapshot of its linked target when known.
pub fn show_operation(options: &AuditShowOptions) -> Result<AuditShowReport, DomainError> {
    let database_path =
        resolved_database_path(&options.workspace, options.database_path.as_deref());
    let connection = open_database(&database_path)?;
    let row = connection
        .get_audit(&options.audit_id)
        .map_err(|error| storage_error("Failed to load audit row", error))?
        .ok_or_else(|| DomainError::NotFound {
            resource: "audit row".to_owned(),
            id: options.audit_id.clone(),
            repair: Some("Run `ee audit timeline --json` to list audit row IDs.".to_owned()),
        })?;
    let linked_snapshot = linked_snapshot(&connection, &row)?;
    let hash_chain_valid = verify_entries(
        &connection
            .list_audit_entries(None, None)
            .map_err(|error| storage_error("Failed to list audit rows", error))?,
        None,
        None,
    )?
    .integrity_ok;

    Ok(AuditShowReport {
        schema: AUDIT_SHOW_SCHEMA_V1.to_owned(),
        row: AuditTimelineEntry::from(row),
        linked_snapshot,
        hash_chain_valid,
    })
}

/// Show audit rows between two RFC 3339 timestamps.
pub fn show_diff(options: &AuditDiffOptions) -> Result<AuditDiffReport, DomainError> {
    let from = parse_required_instant(&options.from, "from")?;
    let to = parse_required_instant(&options.to, "to")?;
    if from > to {
        return Err(DomainError::Usage {
            message: "audit diff requires FROM to be earlier than or equal to TO".to_owned(),
            repair: Some(
                "Use `ee audit diff 2026-05-01T00:00:00Z 2026-05-02T00:00:00Z --json`.".to_owned(),
            ),
        });
    }

    let entries = load_entries(&options.workspace, options.database_path.as_deref())?;
    let filtered = filter_entries(entries, Some(from), Some(to), None, None)?;
    let row_count = u32::try_from(filtered.len()).unwrap_or(u32::MAX);

    Ok(AuditDiffReport {
        schema: AUDIT_DIFF_SCHEMA_V1.to_owned(),
        from: options.from.clone(),
        to: options.to.clone(),
        entries: filtered.into_iter().map(AuditTimelineEntry::from).collect(),
        row_count,
    })
}

/// Verify audit hash-chain integrity for all rows or an optional time window.
pub fn verify_audit(options: &AuditVerifyOptions) -> Result<AuditVerifyReport, DomainError> {
    let since = parse_optional_instant(options.since.as_deref(), "since")?;
    let until = parse_optional_instant(options.until.as_deref(), "until")?;
    if let (Some(since), Some(until)) = (since, until) {
        if since > until {
            return Err(DomainError::Usage {
                message: "audit verify requires --since to be earlier than or equal to --until"
                    .to_owned(),
                repair: Some("Use `ee audit verify --since 2026-05-01T00:00:00Z --until 2026-05-02T00:00:00Z --json`.".to_owned()),
            });
        }
    }

    let database_path =
        resolved_database_path(&options.workspace, options.database_path.as_deref());
    let connection = open_database(&database_path)?;
    let entries = connection
        .list_audit_entries(None, None)
        .map_err(|error| storage_error("Failed to list audit rows", error))?;

    verify_entries(&entries, since, until)
}

/// Verify each shard-local audit chain independently.
pub fn verify_sharded_audit(
    options: &ShardedAuditVerifyOptions,
) -> Result<AuditVerifyReport, DomainError> {
    let since = parse_optional_instant(options.since.as_deref(), "since")?;
    let until = parse_optional_instant(options.until.as_deref(), "until")?;
    if let (Some(since), Some(until)) = (since, until) {
        if since > until {
            return Err(DomainError::Usage {
                message: "audit verify requires --since to be earlier than or equal to --until"
                    .to_owned(),
                repair: Some("Use `ee audit verify --since 2026-05-01T00:00:00Z --until 2026-05-02T00:00:00Z --json`.".to_owned()),
            });
        }
    }

    let mut shards = options.shards.clone();
    shards.sort_by(|left, right| left.shard_id.cmp(&right.shard_id));

    let mut shard_reports = Vec::with_capacity(shards.len());
    let mut aggregate_issues = Vec::new();
    let mut first_break = None;
    let mut rows = 0_u32;

    for shard in shards {
        let report =
            verify_entries_with_shard(&shard.entries, since, until, Some(shard.shard_id.as_str()))?;
        rows = rows.saturating_add(report.rows);
        if !report.integrity_ok {
            if first_break.is_none() {
                first_break = report.first_break.clone();
            }
            aggregate_issues.push(VerificationIssue {
                code: SHARD_CHAIN_MISMATCH_CODE.to_owned(),
                audit_id: report.first_break.clone(),
                shard_id: Some(shard.shard_id.clone()),
                message: match report.first_break.as_deref() {
                    Some(audit_id) => format!(
                        "shard {} audit chain mismatch at row {audit_id}; inspect shards[] for row-level issues",
                        shard.shard_id
                    ),
                    None => format!(
                        "shard {} audit chain mismatch; inspect shards[] for row-level issues",
                        shard.shard_id
                    ),
                },
            });
        }
        shard_reports.push(AuditShardVerifyReport {
            shard_id: shard.shard_id,
            integrity_ok: report.integrity_ok,
            rows: report.rows,
            last_hash: report.last_hash,
            first_break: report.first_break,
            issues: report.issues,
        });
    }

    let broken_shard_count = u32::try_from(
        shard_reports
            .iter()
            .filter(|report| !report.integrity_ok)
            .count(),
    )
    .unwrap_or(u32::MAX);

    Ok(AuditVerifyReport {
        schema: AUDIT_VERIFY_SCHEMA_V1.to_owned(),
        integrity_ok: broken_shard_count == 0,
        rows,
        last_hash: None,
        first_break,
        issues: aggregate_issues,
        shard_count: u32::try_from(shard_reports.len()).unwrap_or(u32::MAX),
        broken_shard_count,
        shards: shard_reports,
    })
}

fn verify_entries(
    entries: &[StoredAuditEntry],
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> Result<AuditVerifyReport, DomainError> {
    verify_entries_with_shard(entries, since, until, None)
}

fn verify_entries_with_shard(
    entries: &[StoredAuditEntry],
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    shard_id: Option<&str>,
) -> Result<AuditVerifyReport, DomainError> {
    let mut ordered = entries.to_vec();
    sort_entries_chronological(&mut ordered);
    let filtered = filter_entries(ordered, since, until, None, None)?;
    let mut expected_prev_hash = if since.is_some() {
        filtered
            .first()
            .and_then(|entry| entry.prev_row_hash.clone())
    } else {
        None
    };
    let mut issues = Vec::new();
    let mut first_break = None;
    let mut last_hash = None;

    for entry in &filtered {
        if entry.prev_row_hash != expected_prev_hash {
            push_first_issue(
                &mut issues,
                &mut first_break,
                "prev_hash_mismatch",
                entry.id.clone(),
                shard_id,
                format!(
                    "row {} points to {:?}, expected {:?}",
                    entry.id, entry.prev_row_hash, expected_prev_hash
                ),
            );
        }

        match &entry.this_row_hash {
            Some(stored_hash) => {
                let computed = compute_audit_row_hash(entry);
                if stored_hash != &computed {
                    push_first_issue(
                        &mut issues,
                        &mut first_break,
                        "row_hash_mismatch",
                        entry.id.clone(),
                        shard_id,
                        format!(
                            "row {} hash mismatch: stored {}, recomputed {}",
                            entry.id, stored_hash, computed
                        ),
                    );
                }
                expected_prev_hash = Some(stored_hash.clone());
                last_hash = Some(stored_hash.clone());
            }
            None => {
                push_first_issue(
                    &mut issues,
                    &mut first_break,
                    "missing_row_hash",
                    entry.id.clone(),
                    shard_id,
                    format!("row {} is missing this_row_hash", entry.id),
                );
                expected_prev_hash = None;
                last_hash = None;
            }
        }
    }

    Ok(AuditVerifyReport {
        schema: AUDIT_VERIFY_SCHEMA_V1.to_owned(),
        integrity_ok: issues.is_empty(),
        rows: u32::try_from(filtered.len()).unwrap_or(u32::MAX),
        last_hash,
        first_break,
        issues,
        shard_count: 0,
        broken_shard_count: 0,
        shards: vec![],
    })
}

fn push_first_issue(
    issues: &mut Vec<VerificationIssue>,
    first_break: &mut Option<String>,
    code: &str,
    audit_id: String,
    shard_id: Option<&str>,
    message: String,
) {
    if first_break.is_none() {
        *first_break = Some(audit_id.clone());
    }
    issues.push(VerificationIssue {
        code: code.to_owned(),
        audit_id: Some(audit_id),
        shard_id: shard_id.map(str::to_owned),
        message,
    });
}

fn load_entries(
    workspace: &Path,
    database_path: Option<&Path>,
) -> Result<Vec<StoredAuditEntry>, DomainError> {
    let database_path = resolved_database_path(workspace, database_path);
    let connection = open_database(&database_path)?;
    let mut entries = connection
        .list_audit_entries(None, None)
        .map_err(|error| storage_error("Failed to list audit rows", error))?;
    sort_entries_chronological(&mut entries);
    Ok(entries)
}

fn filter_entries(
    entries: Vec<StoredAuditEntry>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    surface: Option<&str>,
    action: Option<&str>,
) -> Result<Vec<StoredAuditEntry>, DomainError> {
    let surface = surface.map(str::trim).filter(|value| !value.is_empty());
    let action = action.map(str::trim).filter(|value| !value.is_empty());
    let mut filtered = Vec::new();

    for entry in entries {
        let timestamp = parse_required_instant(&entry.timestamp, "audit_log.timestamp")?;
        if since.is_some_and(|bound| timestamp < bound) {
            continue;
        }
        if until.is_some_and(|bound| timestamp > bound) {
            continue;
        }
        if surface.is_some_and(|wanted| entry.surface != wanted) {
            continue;
        }
        if action.is_some_and(|wanted| !audit_action_filter_matches(&entry.action, wanted)) {
            continue;
        }
        filtered.push(entry);
    }

    Ok(filtered)
}

fn audit_action_filter_matches(action: &str, wanted: &str) -> bool {
    if wanted == "*" {
        return true;
    }
    if let Some(prefix) = wanted.strip_suffix('*') {
        return action.starts_with(prefix);
    }
    action == wanted
}

#[derive(Clone, Debug)]
struct ShardedStoredAuditEntry {
    shard_id: String,
    entry: StoredAuditEntry,
}

fn sharded_entries(shards: &[AuditShardEntries]) -> Vec<ShardedStoredAuditEntry> {
    let mut entries = Vec::new();
    for shard in shards {
        entries.extend(
            shard
                .entries
                .iter()
                .cloned()
                .map(|entry| ShardedStoredAuditEntry {
                    shard_id: shard.shard_id.clone(),
                    entry,
                }),
        );
    }
    entries
}

fn filter_sharded_entries(
    entries: Vec<ShardedStoredAuditEntry>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    surface: Option<&str>,
    action: Option<&str>,
) -> Result<Vec<ShardedStoredAuditEntry>, DomainError> {
    let surface = surface.map(str::trim).filter(|value| !value.is_empty());
    let action = action.map(str::trim).filter(|value| !value.is_empty());
    let mut filtered = Vec::new();

    for entry in entries {
        let timestamp = parse_required_instant(&entry.entry.timestamp, "audit_log.timestamp")?;
        if since.is_some_and(|bound| timestamp < bound) {
            continue;
        }
        if until.is_some_and(|bound| timestamp > bound) {
            continue;
        }
        if surface.is_some_and(|wanted| entry.entry.surface != wanted) {
            continue;
        }
        if action.is_some_and(|wanted| !audit_action_filter_matches(&entry.entry.action, wanted)) {
            continue;
        }
        filtered.push(entry);
    }

    Ok(filtered)
}

fn sort_entries_chronological(entries: &mut Vec<StoredAuditEntry>) {
    entries.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.workspace_id.cmp(&right.workspace_id))
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn sort_sharded_entries_chronological(entries: &mut [ShardedStoredAuditEntry]) {
    entries.sort_by(|left, right| {
        left.entry
            .timestamp
            .cmp(&right.entry.timestamp)
            .then_with(|| left.entry.workspace_id.cmp(&right.entry.workspace_id))
            .then_with(|| left.shard_id.cmp(&right.shard_id))
            .then_with(|| left.entry.id.cmp(&right.entry.id))
    });
}

fn linked_snapshot(
    connection: &DbConnection,
    entry: &StoredAuditEntry,
) -> Result<LinkedSnapshot, DomainError> {
    let target_type = entry.target_type.clone();
    let target_id = entry.target_id.clone();
    let Some(target_id_ref) = target_id.as_deref() else {
        return Ok(LinkedSnapshot {
            target_type,
            target_id,
            found: false,
            snapshot_hash: None,
            snapshot: None,
        });
    };

    match target_type.as_deref() {
        Some("memory") => match connection
            .get_memory(target_id_ref)
            .map_err(|error| storage_error("Failed to load linked memory snapshot", error))?
        {
            Some(memory) => {
                let snapshot = json!({
                    "id": memory.id,
                    "workspace_id": memory.workspace_id,
                    "level": memory.level,
                    "kind": memory.kind,
                    "confidence": memory.confidence,
                    "trust_class": memory.trust_class,
                    "tombstoned_at": memory.tombstoned_at,
                });
                Ok(LinkedSnapshot {
                    target_type,
                    target_id,
                    found: true,
                    snapshot_hash: Some(hash_json("memory", &snapshot)),
                    snapshot: Some(snapshot),
                })
            }
            None => Ok(LinkedSnapshot {
                target_type,
                target_id,
                found: false,
                snapshot_hash: None,
                snapshot: None,
            }),
        },
        Some("rule") | Some("procedural_rule") => match connection
            .get_procedural_rule(target_id_ref)
            .map_err(|error| storage_error("Failed to load linked rule snapshot", error))?
        {
            Some(rule) => {
                let snapshot = json!({
                    "id": rule.id,
                    "workspace_id": rule.workspace_id,
                    "confidence": rule.confidence,
                    "trust_class": rule.trust_class,
                    "scope": rule.scope,
                    "maturity": rule.maturity,
                    "protected": rule.protected,
                    "tombstoned_at": rule.tombstoned_at,
                });
                Ok(LinkedSnapshot {
                    target_type,
                    target_id,
                    found: true,
                    snapshot_hash: Some(hash_json("rule", &snapshot)),
                    snapshot: Some(snapshot),
                })
            }
            None => Ok(LinkedSnapshot {
                target_type,
                target_id,
                found: false,
                snapshot_hash: None,
                snapshot: None,
            }),
        },
        _ => Ok(LinkedSnapshot {
            target_type,
            target_id,
            found: false,
            snapshot_hash: None,
            snapshot: None,
        }),
    }
}

fn hash_json(prefix: &str, value: &JsonValue) -> String {
    format!(
        "blake3:{}",
        blake3::hash(format!("{prefix}:{value}").as_bytes()).to_hex()
    )
}

fn open_database(database_path: &Path) -> Result<DbConnection, DomainError> {
    DbConnection::open_file(database_path)
        .map_err(|error| storage_error("Failed to open database", error))
}

fn resolved_database_path(workspace: &Path, database_path: Option<&Path>) -> PathBuf {
    database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace.join(".ee").join("ee.db"))
}

fn parse_cursor(cursor: Option<&str>) -> Result<usize, DomainError> {
    let Some(raw) = cursor else {
        return Ok(0);
    };
    raw.parse::<usize>().map_err(|_| DomainError::Usage {
        message: format!("Invalid audit timeline cursor `{raw}`: expected a non-negative offset"),
        repair: Some(
            "Use the `next_cursor` value returned by the previous timeline response.".to_owned(),
        ),
    })
}

fn parse_optional_instant(
    value: Option<&str>,
    field: &str,
) -> Result<Option<DateTime<Utc>>, DomainError> {
    value
        .map(|raw| parse_required_instant(raw, field))
        .transpose()
}

fn parse_required_instant(value: &str, field: &str) -> Result<DateTime<Utc>, DomainError> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|error| DomainError::Usage {
            message: format!("{field} must be an RFC 3339 timestamp: {error}"),
            repair: Some("Use timestamps such as 2026-05-01T00:00:00Z.".to_owned()),
        })
}

fn storage_error(context: &str, error: crate::db::DbError) -> DomainError {
    DomainError::Storage {
        message: format!("{context}: {error}"),
        repair: Some("Run `ee doctor --json` and verify the workspace database.".to_owned()),
    }
}

impl From<StoredAuditEntry> for AuditTimelineEntry {
    fn from(entry: StoredAuditEntry) -> Self {
        Self::from_sharded(entry, None)
    }
}

impl AuditTimelineEntry {
    fn from_sharded(entry: StoredAuditEntry, shard_id: Option<String>) -> Self {
        let producer =
            ProducerMetadata::audit_actor(entry.actor.as_deref(), Some(&entry.timestamp));
        Self {
            id: entry.id,
            timestamp: entry.timestamp,
            actor: entry.actor,
            surface: entry.surface,
            mutation_kind: entry.mutation_kind,
            before_hash: entry.before_hash,
            after_hash: entry.after_hash,
            prev_row_hash: entry.prev_row_hash,
            this_row_hash: entry.this_row_hash,
            workspace_id: entry.workspace_id,
            shard_id,
            target_type: entry.target_type,
            target_id: entry.target_id,
            producer,
            details: entry
                .details
                .as_deref()
                .and_then(|details| serde_json::from_str(details).ok()),
        }
    }
}

const fn is_zero(value: &u32) -> bool {
    *value == 0
}

#[cfg(test)]
// Audit tests use expect only for deterministic fixture setup failures.
#[allow(clippy::expect_used)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::db::{CreateAuditInput, CreateMemoryInput, CreateWorkspaceInput};

    use super::*;

    type TestResult = Result<(), String>;

    fn audit_entry_for_sort(id: &str, timestamp: &str) -> StoredAuditEntry {
        StoredAuditEntry {
            id: id.to_owned(),
            workspace_id: Some("wsp_01234567890123456789012345".to_owned()),
            timestamp: timestamp.to_owned(),
            actor: Some("agent-a".to_owned()),
            action: "memory.create".to_owned(),
            target_type: Some("memory".to_owned()),
            target_id: Some("mem_00000000000000000000000001".to_owned()),
            details: None,
            surface: "memory".to_owned(),
            mutation_kind: "memory.create".to_owned(),
            before_hash: None,
            after_hash: None,
            prev_row_hash: None,
            this_row_hash: None,
        }
    }

    #[test]
    fn audit_timeline_same_timestamp_ties_use_radix_audit_id_order() {
        let lower = "audit_00000000000000000000000001";
        let higher = "audit_00000000000000000000000002";
        let later = "audit_00000000000000000000000000";
        let mut entries = vec![
            audit_entry_for_sort(higher, "2026-05-19T08:00:00Z"),
            audit_entry_for_sort(later, "2026-05-19T08:00:01Z"),
            audit_entry_for_sort(lower, "2026-05-19T08:00:00Z"),
        ];

        sort_entries_chronological(&mut entries);

        let ids = entries
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![lower, higher, later]);
    }

    fn shard_timeline_entry(id: &str, timestamp: &str, workspace_id: &str) -> StoredAuditEntry {
        let mut entry = audit_entry_for_sort(id, timestamp);
        entry.workspace_id = Some(workspace_id.to_owned());
        entry
    }

    fn hashed_chain_entry(
        id: &str,
        timestamp: &str,
        workspace_id: &str,
        prev_row_hash: Option<String>,
    ) -> StoredAuditEntry {
        let mut entry = audit_entry_for_sort(id, timestamp);
        entry.workspace_id = Some(workspace_id.to_owned());
        entry.prev_row_hash = prev_row_hash;
        entry.this_row_hash = Some(compute_audit_row_hash(&entry));
        entry
    }

    #[test]
    fn sharded_timeline_orders_by_timestamp_workspace_shard_and_audit_id() -> TestResult {
        let report = list_sharded_timeline(&ShardedAuditTimelineOptions {
            shards: vec![
                AuditShardEntries {
                    shard_id: "shard_b".to_owned(),
                    entries: vec![
                        shard_timeline_entry(
                            "audit_00000000000000000000000001",
                            "2026-05-19T08:00:00Z",
                            "wsp_b",
                        ),
                        shard_timeline_entry(
                            "audit_00000000000000000000000004",
                            "2026-05-19T08:00:01Z",
                            "wsp_a",
                        ),
                    ],
                },
                AuditShardEntries {
                    shard_id: "shard_a".to_owned(),
                    entries: vec![
                        shard_timeline_entry(
                            "audit_00000000000000000000000003",
                            "2026-05-19T08:00:00Z",
                            "wsp_a",
                        ),
                        shard_timeline_entry(
                            "audit_00000000000000000000000002",
                            "2026-05-19T08:00:00Z",
                            "wsp_a",
                        ),
                    ],
                },
            ],
            limit: 20,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        let keys = report
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.timestamp.as_str(),
                    entry.workspace_id.as_deref(),
                    entry.shard_id.as_deref(),
                    entry.id.as_str(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            keys,
            vec![
                (
                    "2026-05-19T08:00:00Z",
                    Some("wsp_a"),
                    Some("shard_a"),
                    "audit_00000000000000000000000002",
                ),
                (
                    "2026-05-19T08:00:00Z",
                    Some("wsp_a"),
                    Some("shard_a"),
                    "audit_00000000000000000000000003",
                ),
                (
                    "2026-05-19T08:00:00Z",
                    Some("wsp_b"),
                    Some("shard_b"),
                    "audit_00000000000000000000000001",
                ),
                (
                    "2026-05-19T08:00:01Z",
                    Some("wsp_a"),
                    Some("shard_b"),
                    "audit_00000000000000000000000004",
                ),
            ]
        );
        assert_eq!(report.pagination.total_count, 4);
        Ok(())
    }

    #[test]
    fn sharded_verify_reports_one_broken_shard_without_poisoning_other_shards() -> TestResult {
        let alpha_first = hashed_chain_entry(
            "audit_00000000000000000000000001",
            "2026-05-19T08:00:00Z",
            "wsp_alpha",
            None,
        );
        let alpha_second = hashed_chain_entry(
            "audit_00000000000000000000000002",
            "2026-05-19T08:00:01Z",
            "wsp_alpha",
            alpha_first.this_row_hash.clone(),
        );
        let beta_first = hashed_chain_entry(
            "audit_00000000000000000000000003",
            "2026-05-19T08:00:00Z",
            "wsp_beta",
            None,
        );
        let beta_second = hashed_chain_entry(
            "audit_00000000000000000000000004",
            "2026-05-19T08:00:01Z",
            "wsp_beta",
            Some("blake3:not-the-beta-tip".to_owned()),
        );

        let report = verify_sharded_audit(&ShardedAuditVerifyOptions {
            shards: vec![
                AuditShardEntries {
                    shard_id: "shard_beta".to_owned(),
                    entries: vec![beta_first, beta_second],
                },
                AuditShardEntries {
                    shard_id: "shard_alpha".to_owned(),
                    entries: vec![alpha_first, alpha_second],
                },
            ],
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        assert!(!report.integrity_ok);
        assert_eq!(report.rows, 4);
        assert_eq!(report.last_hash, None);
        assert_eq!(report.shard_count, 2);
        assert_eq!(report.broken_shard_count, 1);
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].code, SHARD_CHAIN_MISMATCH_CODE);
        assert_eq!(report.issues[0].shard_id.as_deref(), Some("shard_beta"));
        assert_eq!(
            report.issues[0].audit_id.as_deref(),
            Some("audit_00000000000000000000000004")
        );

        let alpha = report
            .shards
            .iter()
            .find(|shard| shard.shard_id == "shard_alpha")
            .ok_or_else(|| "missing shard_alpha report".to_owned())?;
        let beta = report
            .shards
            .iter()
            .find(|shard| shard.shard_id == "shard_beta")
            .ok_or_else(|| "missing shard_beta report".to_owned())?;
        assert!(alpha.integrity_ok);
        assert!(alpha.issues.is_empty());
        assert!(!beta.integrity_ok);
        assert_eq!(beta.issues.len(), 1);
        assert_eq!(beta.issues[0].code, "prev_hash_mismatch");
        assert_eq!(beta.issues[0].shard_id.as_deref(), Some("shard_beta"));
        Ok(())
    }

    fn fixture_workspace(name: &str) -> Result<PathBuf, String> {
        let root = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("system clock before UNIX_EPOCH: {error}"))?
            .as_nanos();
        let path = root
            .join("ee-test-artifacts")
            .join("audit")
            .join(format!("{name}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(path.join(".ee"))
            .map_err(|error| format!("failed to create {}: {error}", path.display()))?;
        Ok(path)
    }

    fn seed_entry(
        connection: &DbConnection,
        id: &str,
        actor: &str,
        action: &str,
        target_type: &str,
        target_id: &str,
    ) -> Result<(), String> {
        connection
            .insert_audit(
                id,
                &CreateAuditInput {
                    workspace_id: Some("wsp_01234567890123456789012345".to_owned()),
                    actor: Some(actor.to_owned()),
                    action: action.to_owned(),
                    target_type: Some(target_type.to_owned()),
                    target_id: Some(target_id.to_owned()),
                    details: Some(
                        serde_json::json!({
                            "action": action,
                            "target": target_id,
                        })
                        .to_string(),
                    ),
                },
            )
            .map_err(|error| error.to_string())
    }

    fn seeded_workspace(name: &str) -> Result<PathBuf, String> {
        let workspace = fixture_workspace(name)?;
        let database = workspace.join(".ee").join("ee.db");
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                "wsp_01234567890123456789012345",
                &CreateWorkspaceInput {
                    path: workspace.to_string_lossy().into_owned(),
                    name: Some("audit-test".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_00000000000000000000000001",
                &CreateMemoryInput {
                    workspace_id: "wsp_01234567890123456789012345".to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Run cargo fmt --check before release.".to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: Some("file://AGENTS.md".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: Some("test".to_owned()),
                    tags: vec![],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        seed_entry(
            &connection,
            "audit_00000000000000000000000001",
            "agent-a",
            "memory.create",
            "memory",
            "mem_00000000000000000000000001",
        )?;
        seed_entry(
            &connection,
            "audit_00000000000000000000000002",
            "agent-b",
            "rule.protect",
            "rule",
            "rule_missing0000000000000000001",
        )?;
        connection.close().map_err(|error| error.to_string())?;
        Ok(workspace)
    }

    #[test]
    fn timeline_empty_log_is_valid_json_shape() -> TestResult {
        let workspace = fixture_workspace("empty")?;
        let database = workspace.join(".ee").join("ee.db");
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let report = list_timeline(&AuditTimelineOptions {
            workspace,
            limit: 20,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.schema, AUDIT_TIMELINE_SCHEMA_V1);
        assert!(report.entries.is_empty());
        assert_eq!(report.pagination.total_count, 0);
        Ok(())
    }

    #[test]
    fn timeline_filters_by_surface_and_paginates() -> TestResult {
        let workspace = seeded_workspace("surface")?;
        let report = list_timeline(&AuditTimelineOptions {
            workspace,
            surface: Some("memory".to_owned()),
            limit: 1,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].surface, "memory");
        assert_eq!(report.entries[0].actor.as_deref(), Some("agent-a"));
        assert_eq!(report.pagination.total_count, 1);
        Ok(())
    }

    #[test]
    fn timeline_filters_by_action_namespace_glob() -> TestResult {
        let workspace = seeded_workspace("action-glob")?;
        let database = workspace.join(".ee").join("ee.db");
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        seed_entry(
            &connection,
            "audit_00000000000000000000000003",
            "agent-g",
            "graph.algorithm.result_cached",
            "graph_algorithm_witness",
            "witness_000000000000000000000001",
        )?;
        connection.close().map_err(|error| error.to_string())?;

        let report = list_timeline(&AuditTimelineOptions {
            workspace,
            action: Some("graph.*".to_owned()),
            limit: 20,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.entries.len(), 1);
        assert_eq!(
            report.entries[0].mutation_kind,
            "graph.algorithm.result_cached"
        );
        assert_eq!(report.entries[0].surface, "graph_algorithm_witness");
        assert_eq!(report.entries[0].actor.as_deref(), Some("agent-g"));
        assert_eq!(report.pagination.total_count, 1);
        Ok(())
    }

    #[test]
    fn show_returns_linked_memory_snapshot() -> TestResult {
        let workspace = seeded_workspace("show")?;
        let report = show_operation(&AuditShowOptions {
            workspace,
            audit_id: "audit_00000000000000000000000001".to_owned(),
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.schema, AUDIT_SHOW_SCHEMA_V1);
        assert!(report.hash_chain_valid);
        assert!(report.linked_snapshot.found);
        assert_eq!(
            report.linked_snapshot.target_id.as_deref(),
            Some("mem_00000000000000000000000001")
        );
        Ok(())
    }

    #[test]
    fn diff_filters_by_time_window() -> TestResult {
        let workspace = seeded_workspace("diff")?;
        let report = show_diff(&AuditDiffOptions {
            workspace,
            from: "2000-01-01T00:00:00Z".to_owned(),
            to: "2999-01-01T00:00:00Z".to_owned(),
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.schema, AUDIT_DIFF_SCHEMA_V1);
        assert_eq!(report.row_count, 2);
        assert_eq!(report.entries[0].id, "audit_00000000000000000000000001");
        Ok(())
    }

    #[test]
    fn verify_detects_tampered_row() -> TestResult {
        let workspace = seeded_workspace("tamper")?;
        let database = workspace.join(".ee").join("ee.db");
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        // V036 (eidetic_engine_cli-is96) installs an append-only trigger on
        // audit_log that blocks UPDATEs at the engine. To exercise the
        // post-hoc detection layer we have to bypass the trigger first —
        // an attacker who managed the same would leave a forensically
        // visible DROP TRIGGER in the schema, but the chain hash check
        // below still catches the underlying row tamper.
        connection
            .execute_raw("DROP TRIGGER IF EXISTS audit_log_no_update")
            .map_err(|error| error.to_string())?;
        connection
            .execute_raw(
                "UPDATE audit_log SET actor = 'tampered-agent' WHERE id = 'audit_00000000000000000000000002'",
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let report = verify_audit(&AuditVerifyOptions {
            workspace,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        assert!(!report.integrity_ok);
        assert_eq!(
            report.first_break.as_deref(),
            Some("audit_00000000000000000000000002")
        );
        Ok(())
    }

    /// V036 / eidetic_engine_cli-is96 — append-only trigger on audit_log
    /// blocks raw UPDATE attempts before they touch the row.
    #[test]
    fn append_only_trigger_blocks_audit_log_update() -> TestResult {
        let workspace = seeded_workspace("trigger-update")?;
        let database = workspace.join(".ee").join("ee.db");
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;

        let outcome = connection.execute_raw(
            "UPDATE audit_log SET actor = 'tampered-agent' WHERE id = 'audit_00000000000000000000000001'",
        );

        connection.close().map_err(|error| error.to_string())?;

        let error = outcome.expect_err("trigger should reject UPDATE on audit_log");
        let message = error.to_string().to_lowercase();
        assert!(
            message.contains("audit_log") && message.contains("append-only"),
            "trigger error should mention audit_log + append-only, got: {error}"
        );
        Ok(())
    }

    /// V036 / eidetic_engine_cli-is96 — append-only trigger on audit_log
    /// blocks raw DELETE attempts.
    #[test]
    fn append_only_trigger_blocks_audit_log_delete() -> TestResult {
        let workspace = seeded_workspace("trigger-delete")?;
        let database = workspace.join(".ee").join("ee.db");
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;

        let outcome = connection
            .execute_raw("DELETE FROM audit_log WHERE id = 'audit_00000000000000000000000001'");

        connection.close().map_err(|error| error.to_string())?;

        let error = outcome.expect_err("trigger should reject DELETE on audit_log");
        let message = error.to_string().to_lowercase();
        assert!(
            message.contains("audit_log") && message.contains("append-only"),
            "trigger error should mention audit_log + append-only, got: {error}"
        );
        Ok(())
    }

    /// V036 / eidetic_engine_cli-is96 — the trigger's WHEN clause must
    /// permit the workspaces ON DELETE SET NULL foreign-key action so that
    /// deleting a workspace doesn't cascade into a trigger abort. The
    /// chain hash will report a break afterward (because workspace_id
    /// participates in the row hash and the cascade flips it to NULL),
    /// but that is a pre-existing design tension between V001's FK and
    /// V033's hash chain — not a regression introduced by V036.
    #[test]
    fn append_only_trigger_allows_workspace_set_null_cascade() -> TestResult {
        let workspace = seeded_workspace("trigger-cascade")?;
        let database = workspace.join(".ee").join("ee.db");
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;

        // Workspaces FK on audit_log is ON DELETE SET NULL, so deleting
        // the workspace performs an UPDATE on audit_log.workspace_id.
        // Without the WHEN-clause carve-out this would trip the trigger.
        connection
            .execute_raw("DELETE FROM workspaces WHERE id = 'wsp_01234567890123456789012345'")
            .map_err(|error| {
                format!("workspace delete must succeed despite append-only trigger: {error}")
            })?;

        connection.close().map_err(|error| error.to_string())?;

        // Audit log rows should still exist; the cascade should not have
        // deleted them.
        let report = verify_audit(&AuditVerifyOptions {
            workspace,
            ..Default::default()
        })
        .map_err(|error| error.message())?;
        assert_eq!(
            report.rows, 2,
            "audit rows preserved after FK SET NULL cascade"
        );
        Ok(())
    }

    #[test]
    fn verify_empty_log_is_integrity_ok() -> TestResult {
        let workspace = fixture_workspace("verify-empty")?;
        let database = workspace.join(".ee").join("ee.db");
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let report = verify_audit(&AuditVerifyOptions {
            workspace,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        assert!(report.integrity_ok);
        assert_eq!(report.rows, 0);
        assert_eq!(report.last_hash, None);
        Ok(())
    }
}
