//! Operation audit timeline and inspection.
//!
//! Audit commands are read-only projections over the persisted `audit_log`
//! table. Mutating commands append rows through `ee-db`; this module only
//! lists, shows, diffs, and verifies those rows.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};

use crate::db::{DbConnection, StoredAuditEntry, compute_audit_row_hash};
use crate::models::DomainError;

/// Schema for audit timeline response.
pub const AUDIT_TIMELINE_SCHEMA_V1: &str = "ee.audit.timeline.v1";

/// Schema for audit show response.
pub const AUDIT_SHOW_SCHEMA_V1: &str = "ee.audit.show.v1";

/// Schema for audit diff response.
pub const AUDIT_DIFF_SCHEMA_V1: &str = "ee.audit.diff.v1";

/// Schema for audit verify response.
pub const AUDIT_VERIFY_SCHEMA_V1: &str = "ee.audit.verify.v1";

/// Options for listing the audit timeline.
#[derive(Clone, Debug, Default)]
pub struct AuditTimelineOptions {
    pub workspace: PathBuf,
    pub database_path: Option<PathBuf>,
    pub since: Option<String>,
    pub surface: Option<String>,
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
    pub target_type: Option<String>,
    pub target_id: Option<String>,
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
        serde_json::to_string(self).unwrap_or_default()
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
        serde_json::to_string(self).unwrap_or_default()
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
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Verification issue found while walking the chain.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationIssue {
    pub code: String,
    pub audit_id: Option<String>,
    pub message: String,
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
}

impl AuditVerifyReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// List persisted operations in chronological order.
pub fn list_timeline(options: &AuditTimelineOptions) -> Result<AuditTimelineReport, DomainError> {
    let entries = load_entries(&options.workspace, options.database_path.as_deref())?;
    let since = parse_optional_instant(options.since.as_deref(), "since")?;
    let offset = parse_cursor(options.cursor.as_deref())?;
    let filtered = filter_entries(entries, since, None, options.surface.as_deref())?;
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
    let filtered = filter_entries(entries, Some(from), Some(to), None)?;
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

fn verify_entries(
    entries: &[StoredAuditEntry],
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> Result<AuditVerifyReport, DomainError> {
    let mut ordered = entries.to_vec();
    sort_entries_chronological(&mut ordered);
    let filtered = filter_entries(ordered, since, until, None)?;
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
    })
}

fn push_first_issue(
    issues: &mut Vec<VerificationIssue>,
    first_break: &mut Option<String>,
    code: &str,
    audit_id: String,
    message: String,
) {
    if first_break.is_none() {
        *first_break = Some(audit_id.clone());
    }
    issues.push(VerificationIssue {
        code: code.to_owned(),
        audit_id: Some(audit_id),
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
) -> Result<Vec<StoredAuditEntry>, DomainError> {
    let surface = surface.map(str::trim).filter(|value| !value.is_empty());
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
        filtered.push(entry);
    }

    Ok(filtered)
}

fn sort_entries_chronological(entries: &mut [StoredAuditEntry]) {
    entries.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.id.cmp(&right.id))
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
            target_type: entry.target_type,
            target_id: entry.target_id,
            details: entry
                .details
                .as_deref()
                .and_then(|details| serde_json::from_str(details).ok()),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::db::{CreateAuditInput, CreateMemoryInput, CreateWorkspaceInput};

    use super::*;

    type TestResult = Result<(), String>;

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
                    details: Some(format!(r#"{{"action":"{action}","target":"{target_id}"}}"#)),
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
