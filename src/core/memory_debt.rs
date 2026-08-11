//! Memory-debt diagnostics for `ee curate doctor`.
//!
//! The doctor is deliberately read-mostly: it derives debt from persisted
//! memories, anchors, links, feedback, pack ledgers, audit rows, and prior
//! snapshots. The steward-only snapshot path writes a compact trend row.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use serde_json::{Value as JsonValue, json};

use crate::core::degraded_honesty::{RepairCommandKind, classify_repair_command};
use crate::core::workspace;
use crate::db::{
    CreateDebtSnapshotInput, DbConnection, MemoryLinkRelation, StoredAuditEntry,
    StoredDebtSnapshot, StoredFeedbackEvent, StoredMemory, StoredMemoryLink,
    pack_ledger_core_array, parse_stored_pack_ledger,
};
use crate::models::{DomainError, StoredMemoryAnchor};
use crate::policy::{
    MemoryDecayAction, MemoryDecayHalfLives, MemoryDecaySettings, MemoryDecayThresholds,
    evaluate_memory_decay_with_settings,
};

/// Stable schema for `ee curate doctor` response data.
pub const MEMORY_DEBT_DOCTOR_SCHEMA_V1: &str = "ee.curate.doctor.v1";
/// Stable schema for memory-debt trend snapshots.
pub const MEMORY_DEBT_TREND_SCHEMA_V1: &str = "ee.curate.debt_trend.v1";
/// Degraded code emitted when the bounded audit scan may have hidden older reads.
pub const MEMORY_DEBT_AUDIT_WINDOW_PARTIAL_CODE: &str = "memory_debt_audit_window_partial";

const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 500;
const DEFAULT_AUDIT_SCAN_LIMIT: u32 = 2_048;
const RECENT_PACK_RECORD_LIMIT: u32 = 512;
const RECENT_PACK_ITEM_LIMIT: usize = 5_000;
const TREND_LIMIT: u32 = 30;
const NEVER_RETRIEVED_WINDOW_DAYS: i64 = 60;
const CONTRADICTION_WINDOW_DAYS: i64 = 14;
const DECAY_PROJECTION_HORIZON_DAYS: i64 = 14;

/// Options for `ee curate doctor`.
#[derive(Clone, Debug)]
pub struct MemoryDebtDoctorOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Optional debt class filter.
    pub class_filter: Option<&'a str>,
    /// Maximum queue items to return.
    pub limit: u32,
    /// Include prior steward snapshots.
    pub trend: bool,
    /// Optional frozen clock for deterministic tests.
    pub now_rfc3339: Option<&'a str>,
    /// Test-only override for the bounded audit scan.
    pub audit_scan_limit: Option<u32>,
}

impl<'a> MemoryDebtDoctorOptions<'a> {
    /// Build default doctor options for a workspace path.
    #[must_use]
    pub fn new(workspace_path: &'a Path) -> Self {
        Self {
            workspace_path,
            database_path: None,
            class_filter: None,
            limit: DEFAULT_LIMIT,
            trend: false,
            now_rfc3339: None,
            audit_scan_limit: None,
        }
    }
}

/// Options for the steward `memory_debt_snapshot` job.
#[derive(Clone, Debug)]
pub struct MemoryDebtSnapshotOptions<'a> {
    /// Workspace root selected by the runner.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Optional frozen clock for deterministic runs.
    pub now_rfc3339: Option<&'a str>,
    /// Preview without inserting the snapshot row.
    pub dry_run: bool,
    /// Optional scan cap override from steward budget.
    pub limit: Option<u32>,
}

/// One supported debt class.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum MemoryDebtClass {
    StaleAnchor,
    ContradictedUnresolved,
    NeverRetrieved,
    Orphan,
    LowTrustHighRank,
    DecayImminentHighUtility,
}

impl MemoryDebtClass {
    /// Stable snake-case token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StaleAnchor => "stale_anchor",
            Self::ContradictedUnresolved => "contradicted_unresolved",
            Self::NeverRetrieved => "never_retrieved",
            Self::Orphan => "orphan",
            Self::LowTrustHighRank => "low_trust_high_rank",
            Self::DecayImminentHighUtility => "decay_imminent_high_utility",
        }
    }

    /// Parse class filters accepted by the CLI.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let normalized = raw.trim().to_ascii_lowercase().replace(['-', '.'], "_");
        Some(match normalized.as_str() {
            "stale_anchor" | "stale_anchors" => Self::StaleAnchor,
            "contradicted_unresolved" | "conflict" | "conflicts" => Self::ContradictedUnresolved,
            "never_retrieved" | "unretrieved" => Self::NeverRetrieved,
            "orphan" | "orphans" => Self::Orphan,
            "low_trust_high_rank" | "low_trust" => Self::LowTrustHighRank,
            "decay_imminent_high_utility" | "decay_imminent" => Self::DecayImminentHighUtility,
            _ => return None,
        })
    }

    /// All classes in stable display order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::StaleAnchor,
            Self::ContradictedUnresolved,
            Self::NeverRetrieved,
            Self::Orphan,
            Self::LowTrustHighRank,
            Self::DecayImminentHighUtility,
        ]
    }
}

/// One suggested next action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryDebtSuggestedAction {
    pub command: String,
    pub classifier: &'static str,
}

impl MemoryDebtSuggestedAction {
    #[must_use]
    fn data_json(&self) -> JsonValue {
        json!({
            "command": self.command,
            "classifier": self.classifier,
        })
    }
}

/// One debt evidence pointer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryDebtEvidence {
    pub kind: String,
    pub value: String,
}

impl MemoryDebtEvidence {
    #[must_use]
    fn new(kind: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            value: value.into(),
        }
    }

    #[must_use]
    fn data_json(&self) -> JsonValue {
        json!({
            "kind": self.kind,
            "value": self.value,
        })
    }
}

/// One debt queue item.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryDebtItem {
    pub memory_id: String,
    pub class: MemoryDebtClass,
    pub score: f32,
    pub severity: &'static str,
    pub reason: String,
    pub evidence: Vec<MemoryDebtEvidence>,
    pub suggested_action: MemoryDebtSuggestedAction,
    pub memory_level: String,
    pub memory_kind: String,
    pub confidence: f32,
    pub utility: f32,
    pub importance: f32,
    pub trust_class: String,
    pub updated_at: String,
}

impl MemoryDebtItem {
    #[must_use]
    fn data_json(&self) -> JsonValue {
        json!({
            "memoryId": self.memory_id,
            "class": self.class.as_str(),
            "score": self.score,
            "severity": self.severity,
            "reason": self.reason,
            "evidence": self.evidence.iter().map(MemoryDebtEvidence::data_json).collect::<Vec<_>>(),
            "suggestedAction": self.suggested_action.data_json(),
            "memory": {
                "level": self.memory_level,
                "kind": self.memory_kind,
                "confidence": self.confidence,
                "utility": self.utility,
                "importance": self.importance,
                "trustClass": self.trust_class,
                "updatedAt": self.updated_at,
            },
        })
    }
}

/// One degraded report entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryDebtDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: String,
}

impl MemoryDebtDegradation {
    #[must_use]
    fn data_json(&self) -> JsonValue {
        json!({
            "code": self.code,
            "severity": self.severity,
            "message": self.message,
            "repair": self.repair,
        })
    }
}

/// Summary block for the debt queue.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryDebtSummary {
    pub memory_count: usize,
    pub item_count: usize,
    pub returned_count: usize,
    pub limit: u32,
    pub truncated: bool,
    pub total_score: f32,
    pub class_counts: BTreeMap<String, usize>,
}

impl MemoryDebtSummary {
    #[must_use]
    fn data_json(&self) -> JsonValue {
        json!({
            "memoryCount": self.memory_count,
            "itemCount": self.item_count,
            "returnedCount": self.returned_count,
            "limit": self.limit,
            "truncated": self.truncated,
            "totalScore": self.total_score,
            "classCounts": self.class_counts,
        })
    }
}

/// Trend block included by `--trend`.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryDebtTrend {
    pub schema: &'static str,
    pub snapshots: Vec<MemoryDebtTrendSnapshot>,
}

impl MemoryDebtTrend {
    #[must_use]
    fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "snapshots": self.snapshots.iter().map(MemoryDebtTrendSnapshot::data_json).collect::<Vec<_>>(),
        })
    }
}

/// Compact trend row for output.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryDebtTrendSnapshot {
    pub snapshot_day: String,
    pub generation: u64,
    pub report_hash: String,
    pub item_count: u64,
    pub total_score: f32,
    pub created_at: String,
}

impl MemoryDebtTrendSnapshot {
    #[must_use]
    fn from_stored(stored: &StoredDebtSnapshot) -> Self {
        Self {
            snapshot_day: stored.snapshot_day.clone(),
            generation: stored.generation,
            report_hash: stored.report_hash.clone(),
            item_count: stored.item_count,
            total_score: finite_score(stored.total_score),
            created_at: stored.created_at.clone(),
        }
    }

    #[must_use]
    fn data_json(&self) -> JsonValue {
        json!({
            "snapshotDay": self.snapshot_day,
            "generation": self.generation,
            "reportHash": self.report_hash,
            "itemCount": self.item_count,
            "totalScore": self.total_score,
            "createdAt": self.created_at,
        })
    }
}

/// Full doctor report.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryDebtReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub generated_at: String,
    pub filter_class: Option<String>,
    pub summary: MemoryDebtSummary,
    pub queue: Vec<MemoryDebtItem>,
    pub degraded: Vec<MemoryDebtDegradation>,
    pub trend: Option<MemoryDebtTrend>,
    pub next_actions: Vec<String>,
}

impl MemoryDebtReport {
    /// Render stable response data.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "command": self.command,
            "version": self.version,
            "workspaceId": self.workspace_id,
            "workspacePath": self.workspace_path,
            "databasePath": self.database_path,
            "generatedAt": self.generated_at,
            "filter": {
                "class": self.filter_class,
            },
            "summary": self.summary.data_json(),
            "queue": self.queue.iter().map(MemoryDebtItem::data_json).collect::<Vec<_>>(),
            "degraded": self.degraded.iter().map(MemoryDebtDegradation::data_json).collect::<Vec<_>>(),
            "trend": self.trend.as_ref().map(MemoryDebtTrend::data_json),
            "nextActions": self.next_actions,
        })
    }

    /// Human summary for non-JSON renderers.
    #[must_use]
    pub fn human_output(&self) -> String {
        let mut out = format!(
            "Memory Debt Doctor\n\n  workspace: {}\n  queue: {} of {} item(s){}\n  totalScore: {:.3}\n",
            self.workspace_path,
            self.summary.returned_count,
            self.summary.item_count,
            if self.summary.truncated {
                " (truncated)"
            } else {
                ""
            },
            self.summary.total_score,
        );
        if !self.summary.class_counts.is_empty() {
            out.push_str("\nClasses:\n");
            for (class, count) in &self.summary.class_counts {
                out.push_str(&format!("  {class}: {count}\n"));
            }
        }
        if !self.queue.is_empty() {
            out.push_str("\nQueue:\n");
            for item in &self.queue {
                out.push_str(&format!(
                    "  {:.3} {} {} - {}\n",
                    item.score,
                    item.class.as_str(),
                    item.memory_id,
                    item.reason
                ));
            }
        }
        out
    }
}

/// Result of running the steward snapshot job.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryDebtSnapshotReport {
    pub schema: &'static str,
    pub status: String,
    pub dry_run: bool,
    pub inserted: bool,
    pub workspace_id: String,
    pub snapshot_day: String,
    pub generation: u64,
    pub report_hash: String,
    pub item_count: usize,
    pub total_score: f32,
}

impl MemoryDebtSnapshotReport {
    /// Render steward details.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "status": self.status,
            "dryRun": self.dry_run,
            "inserted": self.inserted,
            "workspaceId": self.workspace_id,
            "snapshotDay": self.snapshot_day,
            "generation": self.generation,
            "reportHash": self.report_hash,
            "itemCount": self.item_count,
            "totalScore": self.total_score,
        })
    }
}

#[derive(Clone, Debug)]
struct PreparedMemoryDebt {
    workspace_id: String,
    workspace_path: PathBuf,
    database_path: PathBuf,
}

#[derive(Clone, Debug, Default)]
struct MemoryDebtSignals {
    link_count: u32,
    contradiction_count: u32,
    supersedes_count: u32,
    positive_feedback: f32,
    negative_feedback: f32,
    best_pack_rank: Option<u32>,
    last_read_at: Option<DateTime<Utc>>,
}

/// Run `ee curate doctor`.
pub fn run_memory_debt_doctor(
    options: &MemoryDebtDoctorOptions<'_>,
) -> Result<MemoryDebtReport, DomainError> {
    let prepared = prepare_memory_debt(options.workspace_path, options.database_path)?;
    let class_filter = parse_class_filter(options.class_filter)?;
    let limit = options.limit.min(MAX_LIMIT);
    let now = parse_now(options.now_rfc3339)?;
    let connection = open_existing_database(&prepared.database_path)?;
    build_memory_debt_report(
        &connection,
        &prepared,
        class_filter,
        limit,
        options.trend,
        now,
        options.audit_scan_limit.unwrap_or(DEFAULT_AUDIT_SCAN_LIMIT),
    )
}

/// Run the steward snapshot job.
pub fn run_memory_debt_snapshot(
    options: &MemoryDebtSnapshotOptions<'_>,
) -> Result<MemoryDebtSnapshotReport, DomainError> {
    let prepared = prepare_memory_debt(options.workspace_path, options.database_path)?;
    let now = parse_now(options.now_rfc3339)?;
    let connection = open_existing_database(&prepared.database_path)?;
    let report = build_memory_debt_report(
        &connection,
        &prepared,
        None,
        options.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT),
        false,
        now,
        DEFAULT_AUDIT_SCAN_LIMIT,
    )?;
    let generation = connection
        .get_workspace_generation(&prepared.workspace_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to read workspace generation: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?
        .unwrap_or(0);
    let snapshot_day = report.generated_at.chars().take(10).collect::<String>();
    let report_json = report.data_json();
    let report_text = report_json.to_string();
    let report_hash = format!("blake3:{}", blake3::hash(report_text.as_bytes()).to_hex());
    let inserted = if options.dry_run {
        false
    } else {
        connection
            .insert_debt_snapshot(&CreateDebtSnapshotInput {
                workspace_id: prepared.workspace_id.clone(),
                snapshot_day: snapshot_day.clone(),
                generation,
                report_hash: report_hash.clone(),
                report_json: report_text,
                item_count: report.summary.item_count as u64,
                total_score: report.summary.total_score,
                created_at: report.generated_at.clone(),
            })
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to insert memory debt snapshot: {error}"),
                repair: Some("ee steward run --job memory_debt_snapshot --workspace .".to_owned()),
            })?
    };
    Ok(MemoryDebtSnapshotReport {
        schema: MEMORY_DEBT_TREND_SCHEMA_V1,
        status: if options.dry_run {
            "preview".to_owned()
        } else if inserted {
            "inserted".to_owned()
        } else {
            "already_recorded".to_owned()
        },
        dry_run: options.dry_run,
        inserted,
        workspace_id: prepared.workspace_id,
        snapshot_day,
        generation,
        report_hash,
        item_count: report.summary.item_count,
        total_score: report.summary.total_score,
    })
}

fn build_memory_debt_report(
    connection: &DbConnection,
    prepared: &PreparedMemoryDebt,
    class_filter: Option<MemoryDebtClass>,
    limit: u32,
    include_trend: bool,
    now: DateTime<Utc>,
    audit_scan_limit: u32,
) -> Result<MemoryDebtReport, DomainError> {
    let memories = connection
        .list_memories(&prepared.workspace_id, None, false)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list memories for memory debt doctor: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    let mut signals = memories
        .iter()
        .map(|memory| (memory.id.clone(), MemoryDebtSignals::default()))
        .collect::<BTreeMap<_, _>>();

    ingest_links(connection, &memories, &mut signals)?;
    ingest_feedback(connection, &prepared.workspace_id, &mut signals)?;
    ingest_pack_reads(connection, &prepared.workspace_id, now, &mut signals)?;
    let audit_partial = ingest_audit_reads(
        connection,
        &prepared.workspace_id,
        audit_scan_limit,
        &mut signals,
    )?;
    let anchor_state = ingest_anchor_state(connection, &memories)?;

    let mut queue = Vec::new();
    for memory in &memories {
        let memory_signals = signals.get(&memory.id).cloned().unwrap_or_default();
        let anchors = anchor_state.get(&memory.id).cloned().unwrap_or_default();
        detect_stale_anchor(memory, &anchors, class_filter, &mut queue);
        detect_contradicted(memory, &memory_signals, class_filter, now, &mut queue);
        detect_never_retrieved(memory, &memory_signals, class_filter, now, &mut queue);
        detect_orphan(memory, &memory_signals, class_filter, &mut queue);
        detect_low_trust_high_rank(memory, &memory_signals, class_filter, &mut queue);
        detect_decay_imminent(memory, class_filter, now, &mut queue);
    }

    sort_debt_queue(&mut queue);
    let total_count = queue.len();
    let mut class_counts = BTreeMap::new();
    for item in &queue {
        *class_counts
            .entry(item.class.as_str().to_owned())
            .or_insert(0) += 1;
    }
    for class in MemoryDebtClass::all() {
        class_counts.entry(class.as_str().to_owned()).or_insert(0);
    }
    // bd-3qagn: total_score must reflect the FULL debt set so it stays
    // consistent with item_count/class_counts (both full-queue) and so a trend
    // snapshot's totalScore shifts only when corpus debt changes, not when the
    // --limit changes. Compute it BEFORE truncating the queue.
    let total_score = round_score(queue.iter().map(|item| f64::from(item.score)).sum());
    let returned_count = total_count.min(limit as usize);
    let truncated = total_count > returned_count;
    queue.truncate(returned_count);

    let mut degraded = Vec::new();
    if audit_partial {
        degraded.push(MemoryDebtDegradation {
            code: MEMORY_DEBT_AUDIT_WINDOW_PARTIAL_CODE.to_owned(),
            severity: "info".to_owned(),
            message: format!(
                "Memory debt doctor inspected the newest {audit_scan_limit} audit rows; older read evidence may be outside the window."
            ),
            repair: "ee curate doctor --workspace . --json".to_owned(),
        });
    }

    let trend = if include_trend {
        Some(load_trend(connection, &prepared.workspace_id)?)
    } else {
        None
    };
    Ok(MemoryDebtReport {
        schema: MEMORY_DEBT_DOCTOR_SCHEMA_V1,
        command: "curate doctor",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: prepared.workspace_id.clone(),
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        generated_at: now.to_rfc3339_opts(SecondsFormat::Secs, true),
        filter_class: class_filter.map(|class| class.as_str().to_owned()),
        summary: MemoryDebtSummary {
            memory_count: memories.len(),
            item_count: total_count,
            returned_count,
            limit,
            truncated,
            total_score,
            class_counts,
        },
        queue,
        degraded,
        trend,
        next_actions: vec![
            "ee curate doctor --workspace . --trend --json".to_owned(),
            "ee steward run --job memory_debt_snapshot --workspace . --json".to_owned(),
        ],
    })
}

fn ingest_links(
    connection: &DbConnection,
    memories: &[StoredMemory],
    signals: &mut BTreeMap<String, MemoryDebtSignals>,
) -> Result<(), DomainError> {
    let mut seen = BTreeSet::new();
    for chunk in memories.chunks(200) {
        let ids = chunk
            .iter()
            .map(|memory| memory.id.as_str())
            .collect::<Vec<_>>();
        let links = connection
            .list_memory_links_for_memories(&ids, None)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to list memory links for memory debt doctor: {error}"),
                repair: Some("ee doctor --json".to_owned()),
            })?;
        for link in links {
            if !seen.insert(link.id.clone()) {
                continue;
            }
            apply_link_signal(&link, signals);
        }
    }
    Ok(())
}

fn apply_link_signal(link: &StoredMemoryLink, signals: &mut BTreeMap<String, MemoryDebtSignals>) {
    for memory_id in [&link.src_memory_id, &link.dst_memory_id] {
        if let Some(signal) = signals.get_mut(memory_id) {
            signal.link_count = signal.link_count.saturating_add(1);
            match link.relation_enum() {
                Some(MemoryLinkRelation::Contradicts) => {
                    signal.contradiction_count = signal.contradiction_count.saturating_add(1);
                }
                Some(MemoryLinkRelation::Supersedes) => {
                    signal.supersedes_count = signal.supersedes_count.saturating_add(1);
                }
                _ => {}
            }
        }
    }
}

fn ingest_feedback(
    connection: &DbConnection,
    workspace_id: &str,
    signals: &mut BTreeMap<String, MemoryDebtSignals>,
) -> Result<(), DomainError> {
    let feedback = connection
        .list_feedback_events(workspace_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list feedback for memory debt doctor: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    for event in feedback {
        apply_feedback_signal(&event, signals);
    }
    Ok(())
}

fn apply_feedback_signal(
    event: &StoredFeedbackEvent,
    signals: &mut BTreeMap<String, MemoryDebtSignals>,
) {
    if event.target_type != "memory" {
        return;
    }
    let Some(signal) = signals.get_mut(&event.target_id) else {
        return;
    };
    let weight = finite_score(event.weight);
    match event.signal.as_str() {
        "helpful" | "confirmation" | "positive" | "accurate" | "success" => {
            signal.positive_feedback += weight;
        }
        "harmful" | "contradiction" | "negative" | "inaccurate" | "outdated" | "stale"
        | "failure" => {
            signal.negative_feedback += weight;
        }
        _ => {}
    }
}

fn ingest_pack_reads(
    connection: &DbConnection,
    workspace_id: &str,
    now: DateTime<Utc>,
    signals: &mut BTreeMap<String, MemoryDebtSignals>,
) -> Result<(), DomainError> {
    let pack_ids = connection
        .list_recent_pack_record_ids_for_workspace(
            workspace_id,
            RECENT_PACK_RECORD_LIMIT.saturating_add(1),
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list pack history for memory debt doctor: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    let mut admitted_items = 0_usize;
    let record_cap_exhausted = pack_ids.len() > RECENT_PACK_RECORD_LIMIT as usize;
    for pack_id in pack_ids.into_iter().take(RECENT_PACK_RECORD_LIMIT as usize) {
        if admitted_items >= RECENT_PACK_ITEM_LIMIT {
            break;
        }
        let record = connection
            .get_pack_record(&pack_id)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to load pack history for memory debt doctor: {error}"),
                repair: Some("ee doctor --json".to_owned()),
            })?
            .ok_or_else(|| DomainError::Storage {
                message: "Pack history changed during memory debt inspection.".to_owned(),
                repair: Some("ee doctor --json".to_owned()),
            })?;
        if record.workspace_id != workspace_id {
            return Err(DomainError::Storage {
                message: "Pack history changed workspace during memory debt inspection.".to_owned(),
                repair: Some("ee doctor --json".to_owned()),
            });
        }
        let parsed = parse_stored_pack_ledger(&record);
        let ledger = parsed.available_ledger().ok_or_else(|| DomainError::Storage {
            message: format!(
                "Pack selection evidence is unavailable for memory debt inspection (status {}).",
                parsed.status.as_str()
            ),
            repair: Some("ee doctor --json".to_owned()),
        })?;
        let read_at = ledger
            .get("createdAt")
            .and_then(JsonValue::as_str)
            .and_then(parse_rfc3339)
            .ok_or_else(|| DomainError::Storage {
                message: "Verified pack selection evidence contained an invalid timestamp."
                    .to_owned(),
                repair: Some("ee doctor --json".to_owned()),
            })?;
        if read_at > now {
            return Err(DomainError::Storage {
                message: "Verified pack selection evidence is later than the report clock."
                    .to_owned(),
                repair: Some("Check the system clock, then run `ee doctor --json`.".to_owned()),
            });
        }
        let items = pack_ledger_core_array(ledger, "selectedItems").ok_or_else(|| {
            DomainError::Storage {
                message: "Verified pack selection evidence omitted selectedItems.".to_owned(),
                repair: Some("ee doctor --json".to_owned()),
            }
        })?;
        for item in items {
            if admitted_items >= RECENT_PACK_ITEM_LIMIT {
                break;
            }
            admitted_items = admitted_items.saturating_add(1);
            let Some(memory_id) = item.get("memoryId").and_then(JsonValue::as_str) else {
                return Err(DomainError::Storage {
                    message: "Verified pack selection evidence omitted a memory id.".to_owned(),
                    repair: Some("ee doctor --json".to_owned()),
                });
            };
            let Some(rank) = item
                .get("rank")
                .and_then(JsonValue::as_u64)
                .and_then(|rank| u32::try_from(rank).ok())
            else {
                return Err(DomainError::Storage {
                    message: "Verified pack selection evidence contained an invalid rank."
                        .to_owned(),
                    repair: Some("ee doctor --json".to_owned()),
                });
            };
            let Some(signal) = signals.get_mut(memory_id) else {
                continue;
            };
            signal.best_pack_rank = match signal.best_pack_rank {
                Some(existing) => Some(existing.min(rank)),
                None => Some(rank),
            };
            signal.last_read_at = newer_time(signal.last_read_at, read_at);
        }
    }
    if admitted_items < RECENT_PACK_ITEM_LIMIT && record_cap_exhausted {
        return Err(DomainError::Storage {
            message: "Pack history exceeded the bounded memory debt inspection window.".to_owned(),
            repair: Some(
                "Narrow the workspace or inspect pack history before retrying.".to_owned(),
            ),
        });
    }
    Ok(())
}

fn ingest_audit_reads(
    connection: &DbConnection,
    workspace_id: &str,
    audit_scan_limit: u32,
    signals: &mut BTreeMap<String, MemoryDebtSignals>,
) -> Result<bool, DomainError> {
    let rows = connection
        .list_audit_entries(Some(workspace_id), Some(audit_scan_limit))
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list audit history for memory debt doctor: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    for row in &rows {
        apply_audit_read_signal(row, signals);
    }
    Ok(rows.len() == audit_scan_limit as usize && audit_scan_limit > 0)
}

fn apply_audit_read_signal(
    row: &StoredAuditEntry,
    signals: &mut BTreeMap<String, MemoryDebtSignals>,
) {
    let Some(target_type) = row.target_type.as_deref() else {
        return;
    };
    if target_type != "memory" {
        return;
    }
    if matches!(
        row.action.as_str(),
        "memory.create" | "memory.update" | "memory.revise"
    ) {
        return;
    }
    let Some(target_id) = row.target_id.as_deref() else {
        return;
    };
    let Some(timestamp) = parse_rfc3339(&row.timestamp) else {
        return;
    };
    if let Some(signal) = signals.get_mut(target_id) {
        signal.last_read_at = newer_time(signal.last_read_at, timestamp);
    }
}

fn ingest_anchor_state(
    connection: &DbConnection,
    memories: &[StoredMemory],
) -> Result<BTreeMap<String, Vec<StoredMemoryAnchor>>, DomainError> {
    let mut by_memory = BTreeMap::new();
    for memory in memories {
        let anchors = connection
            .list_memory_anchors(&memory.id)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to list memory anchors for {}: {error}", memory.id),
                repair: Some("ee doctor --json".to_owned()),
            })?;
        by_memory.insert(memory.id.clone(), anchors);
    }
    Ok(by_memory)
}

fn detect_stale_anchor(
    memory: &StoredMemory,
    anchors: &[StoredMemoryAnchor],
    class_filter: Option<MemoryDebtClass>,
    queue: &mut Vec<MemoryDebtItem>,
) {
    if !class_allowed(class_filter, MemoryDebtClass::StaleAnchor) {
        return;
    }
    let stale = anchors
        .iter()
        .filter(|anchor| anchor.freshness_state.rank() > 0)
        .collect::<Vec<_>>();
    if stale.is_empty() {
        return;
    }
    let max_rank = stale
        .iter()
        .map(|anchor| anchor.freshness_state.rank())
        .max()
        .unwrap_or(1);
    let score = debt_score(0.48 + f64::from(max_rank) * 0.16, memory, 0.0);
    let evidence = stale
        .iter()
        .take(5)
        .map(|anchor| {
            MemoryDebtEvidence::new(
                "anchor",
                format!(
                    "{}:{}:{}",
                    anchor.anchor_kind.as_str(),
                    anchor.redacted_anchor_value,
                    anchor.freshness_state.as_str()
                ),
            )
        })
        .collect::<Vec<_>>();
    queue.push(debt_item(
        memory,
        MemoryDebtClass::StaleAnchor,
        score,
        format!("{} anchored surface(s) are suspect or stale", stale.len()),
        evidence,
    ));
}

fn detect_contradicted(
    memory: &StoredMemory,
    signals: &MemoryDebtSignals,
    class_filter: Option<MemoryDebtClass>,
    now: DateTime<Utc>,
    queue: &mut Vec<MemoryDebtItem>,
) {
    if !class_allowed(class_filter, MemoryDebtClass::ContradictedUnresolved)
        || signals.contradiction_count == 0
        || signals.supersedes_count > 0
    {
        return;
    }
    let age_days = parse_rfc3339(&memory.updated_at)
        .map(|updated| now.signed_duration_since(updated).num_days())
        .unwrap_or_default();
    if age_days < CONTRADICTION_WINDOW_DAYS {
        return;
    }
    let score = debt_score(
        0.74 + f64::from(signals.contradiction_count.min(5)) * 0.04,
        memory,
        f64::from(signals.negative_feedback.min(3.0)) * 0.04,
    );
    queue.push(debt_item(
        memory,
        MemoryDebtClass::ContradictedUnresolved,
        score,
        "contradiction links exist without a superseding resolution".to_owned(),
        vec![
            MemoryDebtEvidence::new(
                "contradictionCount",
                signals.contradiction_count.to_string(),
            ),
            MemoryDebtEvidence::new("ageDays", age_days.to_string()),
        ],
    ));
}

fn detect_never_retrieved(
    memory: &StoredMemory,
    signals: &MemoryDebtSignals,
    class_filter: Option<MemoryDebtClass>,
    now: DateTime<Utc>,
    queue: &mut Vec<MemoryDebtItem>,
) {
    if !class_allowed(class_filter, MemoryDebtClass::NeverRetrieved)
        || signals.last_read_at.is_some()
    {
        return;
    }
    let Some(created_at) = parse_rfc3339(&memory.created_at) else {
        return;
    };
    let age_days = now.signed_duration_since(created_at).num_days();
    if age_days < NEVER_RETRIEVED_WINDOW_DAYS {
        return;
    }
    let score = debt_score(
        0.36 + ((age_days - NEVER_RETRIEVED_WINDOW_DAYS) as f64 / 365.0).min(0.25),
        memory,
        0.0,
    );
    queue.push(debt_item(
        memory,
        MemoryDebtClass::NeverRetrieved,
        score,
        format!("no persisted read evidence in the last {NEVER_RETRIEVED_WINDOW_DAYS} days"),
        vec![MemoryDebtEvidence::new("ageDays", age_days.to_string())],
    ));
}

fn detect_orphan(
    memory: &StoredMemory,
    signals: &MemoryDebtSignals,
    class_filter: Option<MemoryDebtClass>,
    queue: &mut Vec<MemoryDebtItem>,
) {
    if !class_allowed(class_filter, MemoryDebtClass::Orphan) || signals.link_count > 0 {
        return;
    }
    let score = debt_score(0.32, memory, 0.0);
    queue.push(debt_item(
        memory,
        MemoryDebtClass::Orphan,
        score,
        "memory has no persisted memory_links edges".to_owned(),
        vec![MemoryDebtEvidence::new("linkCount", "0")],
    ));
}

fn detect_low_trust_high_rank(
    memory: &StoredMemory,
    signals: &MemoryDebtSignals,
    class_filter: Option<MemoryDebtClass>,
    queue: &mut Vec<MemoryDebtItem>,
) {
    if !class_allowed(class_filter, MemoryDebtClass::LowTrustHighRank) {
        return;
    }
    let Some(rank) = signals.best_pack_rank else {
        return;
    };
    if rank > 10 || !is_low_trust(memory, signals) {
        return;
    }
    let rank_boost = (11_u32.saturating_sub(rank) as f64) * 0.025;
    let score = debt_score(
        0.62 + rank_boost,
        memory,
        f64::from(signals.negative_feedback.min(3.0)) * 0.05,
    );
    queue.push(debt_item(
        memory,
        MemoryDebtClass::LowTrustHighRank,
        score,
        "low-trust memory appeared near the top of a persisted pack".to_owned(),
        vec![
            MemoryDebtEvidence::new("bestPackRank", rank.to_string()),
            MemoryDebtEvidence::new(
                "negativeFeedback",
                format!("{:.3}", signals.negative_feedback),
            ),
        ],
    ));
}

fn detect_decay_imminent(
    memory: &StoredMemory,
    class_filter: Option<MemoryDebtClass>,
    now: DateTime<Utc>,
    queue: &mut Vec<MemoryDebtItem>,
) {
    if !class_allowed(class_filter, MemoryDebtClass::DecayImminentHighUtility)
        || memory.utility < 0.6
    {
        return;
    }
    let Some(reference) =
        parse_rfc3339(&memory.updated_at).or_else(|| parse_rfc3339(&memory.created_at))
    else {
        return;
    };
    let projected = now + ChronoDuration::days(DECAY_PROJECTION_HORIZON_DAYS);
    let evaluation = evaluate_memory_decay_with_settings(
        memory,
        reference,
        projected,
        MemoryDecaySettings {
            thresholds: MemoryDecayThresholds::default(),
            half_lives: MemoryDecayHalfLives::default(),
        },
    );
    if evaluation.action == MemoryDecayAction::Preserve {
        return;
    }
    let score = debt_score(
        0.66,
        memory,
        f64::from(1.0 - evaluation.lifecycle_score) * 0.2,
    );
    queue.push(debt_item(
        memory,
        MemoryDebtClass::DecayImminentHighUtility,
        score,
        format!(
            "high-utility memory projects to {} within {DECAY_PROJECTION_HORIZON_DAYS} days",
            evaluation.action.as_str()
        ),
        vec![
            MemoryDebtEvidence::new("projectedAction", evaluation.action.as_str()),
            MemoryDebtEvidence::new(
                "projectedLifecycleScore",
                format!("{:.3}", evaluation.lifecycle_score),
            ),
        ],
    ));
}

fn load_trend(
    connection: &DbConnection,
    workspace_id: &str,
) -> Result<MemoryDebtTrend, DomainError> {
    let snapshots = connection
        .list_debt_snapshots(workspace_id, TREND_LIMIT)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list memory debt snapshots: {error}"),
            repair: Some("ee steward run --job memory_debt_snapshot --workspace .".to_owned()),
        })?;
    Ok(MemoryDebtTrend {
        schema: MEMORY_DEBT_TREND_SCHEMA_V1,
        snapshots: snapshots
            .iter()
            .map(MemoryDebtTrendSnapshot::from_stored)
            .collect(),
    })
}

fn debt_item(
    memory: &StoredMemory,
    class: MemoryDebtClass,
    score: f32,
    reason: String,
    evidence: Vec<MemoryDebtEvidence>,
) -> MemoryDebtItem {
    MemoryDebtItem {
        memory_id: memory.id.clone(),
        class,
        score,
        severity: severity_for_score(score),
        reason,
        evidence,
        suggested_action: suggested_action(class, &memory.id),
        memory_level: memory.level.clone(),
        memory_kind: memory.kind.clone(),
        confidence: finite_score(memory.confidence),
        utility: finite_score(memory.utility),
        importance: finite_score(memory.importance),
        trust_class: memory.trust_class.clone(),
        updated_at: memory.updated_at.clone(),
    }
}

fn suggested_action(class: MemoryDebtClass, memory_id: &str) -> MemoryDebtSuggestedAction {
    let command = match class {
        MemoryDebtClass::StaleAnchor => "ee recall --stale --workspace . --json".to_owned(),
        MemoryDebtClass::ContradictedUnresolved => {
            format!("ee memory show {memory_id} --workspace . --json")
        }
        MemoryDebtClass::NeverRetrieved => {
            format!("ee memory show {memory_id} --workspace . --json")
        }
        MemoryDebtClass::Orphan => format!("ee memory show {memory_id} --workspace . --json"),
        MemoryDebtClass::LowTrustHighRank => {
            format!("ee outcome {memory_id} --signal inaccurate --workspace . --json")
        }
        MemoryDebtClass::DecayImminentHighUtility => {
            "ee curate disposition --workspace . --json".to_owned()
        }
    };
    MemoryDebtSuggestedAction {
        classifier: repair_kind_name(classify_repair_command(&command)),
        command,
    }
}

fn repair_kind_name(kind: RepairCommandKind) -> &'static str {
    match kind {
        RepairCommandKind::Empty => "Empty",
        RepairCommandKind::Unknown => "Unknown",
        RepairCommandKind::Placeholder => "Placeholder",
        RepairCommandKind::Template => "Template",
        RepairCommandKind::Actionable => "Actionable",
    }
}

fn is_low_trust(memory: &StoredMemory, signals: &MemoryDebtSignals) -> bool {
    let trust = memory.trust_class.to_ascii_lowercase();
    memory.confidence < 0.55
        || trust.contains("low")
        || trust.contains("untrusted")
        || trust.contains("quarantine")
        || signals.negative_feedback > signals.positive_feedback
}

fn debt_score(base: f64, memory: &StoredMemory, extra: f64) -> f32 {
    round_score(
        base + f64::from(finite_score(memory.utility)) * 0.12
            + f64::from(finite_score(memory.importance)) * 0.10
            + f64::from(1.0 - finite_score(memory.confidence)) * 0.08
            + extra,
    )
}

fn sort_debt_queue(queue: &mut [MemoryDebtItem]) {
    queue.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.class.cmp(&right.class))
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
}

fn severity_for_score(score: f32) -> &'static str {
    if score >= 0.85 {
        "high"
    } else if score >= 0.65 {
        "medium"
    } else if score >= 0.45 {
        "warning"
    } else {
        "low"
    }
}

fn class_allowed(filter: Option<MemoryDebtClass>, class: MemoryDebtClass) -> bool {
    filter.is_none_or(|selected| selected == class)
}

fn parse_class_filter(raw: Option<&str>) -> Result<Option<MemoryDebtClass>, DomainError> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            MemoryDebtClass::parse(value).ok_or_else(|| DomainError::Usage {
                message: format!("unknown memory debt class `{value}`"),
                repair: Some(format!(
                    "use one of: {}",
                    MemoryDebtClass::all()
                        .iter()
                        .map(|class| class.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            })
        })
        .transpose()
}

fn parse_now(raw: Option<&str>) -> Result<DateTime<Utc>, DomainError> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            DateTime::parse_from_rfc3339(value)
                .map(|timestamp| timestamp.with_timezone(&Utc))
                .map_err(|error| DomainError::Usage {
                    message: format!("--now must be an RFC3339 timestamp: {error}"),
                    repair: Some("use --now 2026-06-15T00:00:00Z".to_owned()),
                })
        })
        .transpose()
        .map(|value| value.unwrap_or_else(Utc::now))
}

fn parse_rfc3339(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .ok()
}

fn newer_time(current: Option<DateTime<Utc>>, candidate: DateTime<Utc>) -> Option<DateTime<Utc>> {
    Some(match current {
        Some(current) => current.max(candidate),
        None => candidate,
    })
}

fn prepare_memory_debt(
    workspace_path: &Path,
    database_path: Option<&Path>,
) -> Result<PreparedMemoryDebt, DomainError> {
    let workspace_path = resolve_workspace_path(workspace_path)?;
    let database_path = database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    Ok(PreparedMemoryDebt {
        workspace_id: workspace::stable_workspace_id(&workspace_path),
        workspace_path,
        database_path,
    })
}

fn resolve_workspace_path(path: &Path) -> Result<PathBuf, DomainError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    absolute
        .canonicalize()
        .map_err(|error| DomainError::Configuration {
            message: format!(
                "Failed to resolve workspace {}: {error}",
                absolute.display()
            ),
            repair: Some("ee init --workspace .".to_owned()),
        })
}

fn open_existing_database(database_path: &Path) -> Result<DbConnection, DomainError> {
    if !database_path.exists() {
        return Err(crate::core::storeless_workspace_error(database_path));
    }
    let connection =
        DbConnection::open_file(database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open database: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    connection
        .migrate()
        .map_err(|error| DomainError::MigrationRequired {
            message: format!("Failed to migrate memory debt database: {error}"),
            repair: Some("ee migrate run --workspace . --json".to_owned()),
        })?;
    Ok(connection)
}

fn finite_score(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn round_score(value: f64) -> f32 {
    if !value.is_finite() {
        return 0.0;
    }
    ((value.clamp(0.0, 1.0) * 1000.0).round() / 1000.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(id: &str, confidence: f32, utility: f32, importance: f32) -> StoredMemory {
        StoredMemory {
            id: id.to_owned(),
            workspace_id: "ws_test".to_owned(),
            level: "semantic".to_owned(),
            kind: "fact".to_owned(),
            content: "test memory".to_owned(),
            workflow_id: None,
            confidence,
            utility,
            importance,
            provenance_uri: None,
            trust_class: "untrusted".to_owned(),
            trust_subclass: None,
            provenance_chain_hash: None,
            provenance_chain_hash_version: "v".to_owned(),
            provenance_verification_status: "unverified".to_owned(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            updated_at: "2026-01-01T00:00:00Z".to_owned(),
            tombstoned_at: None,
            valid_from: Some("2026-01-01T00:00:00Z".to_owned()),
            valid_to: None,
        }
    }

    #[test]
    fn memory_debt_sort_is_score_then_class_then_id() {
        let first = memory("mem_b", 0.4, 0.8, 0.7);
        let second = memory("mem_a", 0.4, 0.8, 0.7);
        let mut queue = vec![
            debt_item(
                &first,
                MemoryDebtClass::Orphan,
                0.5,
                "first".to_owned(),
                Vec::new(),
            ),
            debt_item(
                &second,
                MemoryDebtClass::ContradictedUnresolved,
                0.5,
                "second".to_owned(),
                Vec::new(),
            ),
            debt_item(
                &first,
                MemoryDebtClass::StaleAnchor,
                0.9,
                "third".to_owned(),
                Vec::new(),
            ),
        ];
        sort_debt_queue(&mut queue);
        assert_eq!(queue[0].class, MemoryDebtClass::StaleAnchor);
        assert_eq!(queue[1].class, MemoryDebtClass::ContradictedUnresolved);
        assert_eq!(queue[2].class, MemoryDebtClass::Orphan);
    }

    #[test]
    fn memory_debt_suggested_actions_are_actionable() {
        for class in MemoryDebtClass::all() {
            let action = suggested_action(*class, "mem_01234567890123456789012345");
            assert_eq!(action.classifier, "Actionable", "{}", action.command);
        }
    }

    #[test]
    fn memory_debt_class_parser_accepts_aliases() {
        assert_eq!(
            MemoryDebtClass::parse("decay-imminent"),
            Some(MemoryDebtClass::DecayImminentHighUtility)
        );
        assert_eq!(
            MemoryDebtClass::parse("low_trust"),
            Some(MemoryDebtClass::LowTrustHighRank)
        );
        assert_eq!(MemoryDebtClass::parse("bogus"), None);
    }

    // ---- age-gated detector battery (bd-3ap2m.4): clock-injected ---------

    fn fixed_now() -> DateTime<Utc> {
        parse_rfc3339("2026-08-10T00:00:00Z").expect("fixed test clock")
    }

    fn memory_at(id: &str, created_at: &str, updated_at: &str) -> StoredMemory {
        let mut planted = memory(id, 0.7, 0.5, 0.5);
        planted.created_at = created_at.to_owned();
        planted.updated_at = updated_at.to_owned();
        planted
    }

    #[test]
    fn contradicted_unresolved_needs_age_and_no_supersede() {
        let old = memory_at("mem_old", "2026-07-01T00:00:00Z", "2026-07-01T00:00:00Z");
        let mut signals = MemoryDebtSignals {
            contradiction_count: 2,
            ..MemoryDebtSignals::default()
        };

        let mut queue = Vec::new();
        detect_contradicted(&old, &signals, None, fixed_now(), &mut queue);
        assert_eq!(queue.len(), 1, "40-day-old contradiction must flag");
        assert_eq!(queue[0].class, MemoryDebtClass::ContradictedUnresolved);

        // Inside the 14-day window: not yet debt.
        let fresh = memory_at("mem_new", "2026-08-05T00:00:00Z", "2026-08-05T00:00:00Z");
        let mut queue = Vec::new();
        detect_contradicted(&fresh, &signals, None, fixed_now(), &mut queue);
        assert!(queue.is_empty(), "5-day-old contradiction is not debt yet");

        // A supersede resolution clears the class regardless of age.
        signals.supersedes_count = 1;
        let mut queue = Vec::new();
        detect_contradicted(&old, &signals, None, fixed_now(), &mut queue);
        assert!(queue.is_empty(), "supersede resolution clears the class");
    }

    #[test]
    fn never_retrieved_needs_window_and_no_read_evidence() {
        let old = memory_at("mem_old", "2026-05-01T00:00:00Z", "2026-05-01T00:00:00Z");
        let mut queue = Vec::new();
        detect_never_retrieved(
            &old,
            &MemoryDebtSignals::default(),
            None,
            fixed_now(),
            &mut queue,
        );
        assert_eq!(queue.len(), 1, "101-day-old unread memory must flag");

        let young = memory_at("mem_you", "2026-07-15T00:00:00Z", "2026-07-15T00:00:00Z");
        let mut queue = Vec::new();
        detect_never_retrieved(
            &young,
            &MemoryDebtSignals::default(),
            None,
            fixed_now(),
            &mut queue,
        );
        assert!(queue.is_empty(), "26-day-old memory is inside the window");

        let read_signals = MemoryDebtSignals {
            last_read_at: Some(fixed_now()),
            ..MemoryDebtSignals::default()
        };
        let mut queue = Vec::new();
        detect_never_retrieved(&old, &read_signals, None, fixed_now(), &mut queue);
        assert!(queue.is_empty(), "read evidence clears the class");
    }

    #[test]
    fn decay_imminent_gates_on_utility_and_flags_ancient_high_utility() {
        let mut ancient = memory_at("mem_anc", "2020-01-01T00:00:00Z", "2020-01-01T00:00:00Z");
        ancient.utility = 0.9;
        ancient.confidence = 0.4;
        let mut queue = Vec::new();
        detect_decay_imminent(&ancient, None, fixed_now(), &mut queue);
        assert_eq!(
            queue.len(),
            1,
            "six-year-old high-utility memory projects past preserve"
        );
        assert_eq!(queue[0].class, MemoryDebtClass::DecayImminentHighUtility);

        let mut low_utility = memory_at("mem_low", "2020-01-01T00:00:00Z", "2020-01-01T00:00:00Z");
        low_utility.utility = 0.3;
        let mut queue = Vec::new();
        detect_decay_imminent(&low_utility, None, fixed_now(), &mut queue);
        assert!(queue.is_empty(), "utility < 0.6 never enters this class");
    }

    #[test]
    fn stale_anchor_flags_only_nonfresh_anchors() {
        use crate::models::{MemoryAnchorFreshnessState, MemoryAnchorKind, MemoryAnchorSource};
        let anchor = |state: MemoryAnchorFreshnessState| StoredMemoryAnchor {
            memory_id: "mem_a".to_owned(),
            anchor_kind: MemoryAnchorKind::Symbol,
            anchor_value_hash: "hash".to_owned(),
            redacted_anchor_value: "core::thing".to_owned(),
            confidence: 0.9,
            source: MemoryAnchorSource::Remember,
            provenance: "test".to_owned(),
            captured_span_hash: "span".to_owned(),
            freshness_state: state,
            generation: 1,
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            updated_at: "2026-01-01T00:00:00Z".to_owned(),
        };
        let subject = memory("mem_a", 0.7, 0.5, 0.5);

        let mut queue = Vec::new();
        detect_stale_anchor(
            &subject,
            &[anchor(MemoryAnchorFreshnessState::Stale)],
            None,
            &mut queue,
        );
        assert_eq!(queue.len(), 1, "stale anchor must flag");
        assert_eq!(queue[0].class, MemoryDebtClass::StaleAnchor);

        let mut queue = Vec::new();
        detect_stale_anchor(
            &subject,
            &[anchor(MemoryAnchorFreshnessState::Current)],
            None,
            &mut queue,
        );
        assert!(queue.is_empty(), "current anchors are not debt");
    }

    #[test]
    fn detector_battery_is_insertion_order_stable() {
        // The bd-3ap2m.4 permutation property: running the per-memory
        // detector battery in any input order and then sorting yields the
        // identical queue.
        let corpus = [
            memory_at("mem_01", "2026-05-01T00:00:00Z", "2026-05-01T00:00:00Z"),
            memory_at("mem_02", "2026-04-01T00:00:00Z", "2026-04-01T00:00:00Z"),
            memory_at("mem_03", "2026-03-01T00:00:00Z", "2026-03-01T00:00:00Z"),
        ];
        let signals = MemoryDebtSignals {
            contradiction_count: 1,
            ..MemoryDebtSignals::default()
        };
        let run = |order: &[usize]| {
            let mut queue = Vec::new();
            for index in order {
                let subject = &corpus[*index];
                detect_contradicted(subject, &signals, None, fixed_now(), &mut queue);
                detect_never_retrieved(subject, &signals, None, fixed_now(), &mut queue);
                detect_orphan(subject, &signals, None, &mut queue);
            }
            sort_debt_queue(&mut queue);
            queue
        };
        let forward = run(&[0, 1, 2]);
        let reversed = run(&[2, 0, 1]);
        assert!(!forward.is_empty(), "battery found planted debt");
        assert_eq!(forward, reversed, "insertion order changed the report");
    }
}
