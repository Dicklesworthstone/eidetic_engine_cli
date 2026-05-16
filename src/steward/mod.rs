//! Steward subsystem for maintenance jobs and lifecycle management.
//!
//! The steward manages background maintenance tasks like index rebuilds,
//! decay sweeps, curation reviews, and health checks. It operates in
//! CLI-first mode without requiring a daemon.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use asupersync::runtime::yield_now::yield_now;
use asupersync::time::sleep as asupersync_sleep;
use asupersync::{CancelReason, Cx, Outcome};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use fnx_classes::Graph;
use fnx_runtime::CompatibilityMode;
#[cfg(unix)]
use rustix::fs::{FlockOperation, flock};
#[cfg(unix)]
use rustix::io::Errno;
use serde_json::{Value as JsonValue, json};

use crate::db::{
    AcquireLockResult, AdvisoryLockId, ApplyMemoryDecayDemotionInput, ApplyMemoryScoreUpdateInput,
    CreateCurationCandidateInput, DbConnection, FeedbackCounts, GraphSnapshotPruneCandidate,
    GraphSnapshotType, StoredAuditEntry, StoredFeedbackEvent, StoredMemory, StoredMemoryLink,
    audit_actions, feedback_scoring,
};
use crate::graph::decay::{StructuralDecayMultiplier, compute_structural_decay_adjustment};
use crate::policy::{
    MEMORY_DECAY_SOURCE, MemoryDecayAction, MemoryDecayEvaluation, MemoryDecayHalfLives,
    MemoryDecaySettings, MemoryDecayThresholds, evaluate_memory_decay_with_settings,
    memory_decay_freshness_score,
};

pub const SUBSYSTEM: &str = "steward";

/// Schema identifier for job ledger reports.
pub const JOB_LEDGER_SCHEMA_V1: &str = "ee.steward.job_ledger.v1";

/// Schema identifier for individual job records.
pub const JOB_RECORD_SCHEMA_V1: &str = "ee.steward.job.v1";

/// Schema identifier for maintenance job run command payloads.
pub const MAINTENANCE_RUN_SCHEMA_V1: &str = "ee.maintenance.run.v1";

/// Schema identifier for maintenance status command payloads.
pub const MAINTENANCE_STATUS_SCHEMA_V1: &str = "ee.maintenance.status.v1";

/// Schema identifier for maintenance job list command payloads.
pub const MAINTENANCE_JOB_LIST_SCHEMA_V1: &str = "ee.maintenance.job_list.v1";

/// Schema identifier for maintenance job show command payloads.
pub const MAINTENANCE_JOB_SHOW_SCHEMA_V1: &str = "ee.maintenance.job_show.v1";

/// Schema identifier for persisted maintenance job history rows.
pub const MAINTENANCE_JOB_ROW_SCHEMA_V1: &str = "ee.maintenance.job_row.v1";

pub const MAINTENANCE_JOB_LOCK_SCHEMA_V1: &str = "ee.steward.maintenance_job_lock.v1";
pub const MAINTENANCE_JOB_LOCK_BUSY_CODE: &str = "maintenance_job_lock_busy";
pub const GRAPH_SNAPSHOT_PRUNE_JOB_SCHEMA_V1: &str = "ee.graph.snapshot_prune.v1";
const GRAPH_SNAPSHOT_PRUNE_DEFAULT_LIMIT: u32 = 10_000;
const GRAPH_SNAPSHOT_PRUNE_RETENTION_DAYS: i64 = 7;
const GRAPH_SNAPSHOT_PRUNE_LOCK_TTL_SECS: u64 = 300;
const GRAPH_SNAPSHOT_PRUNE_LOCK_REASON: &str = "graph snapshot prune";

static MAINTENANCE_JOB_PROCESS_GATES: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();

struct GraphSnapshotPruneLockOwner<'a> {
    conn: &'a DbConnection,
    lock_ids: Vec<AdvisoryLockId>,
    holder_id: String,
    ttl_secs: u64,
}

struct GraphSnapshotPruneDetailsInput<'a> {
    workspace_id: &'a str,
    graph_type: GraphSnapshotType,
    dry_run: bool,
    retention_days: i64,
    cutoff_timestamp: &'a str,
    candidates: &'a [GraphSnapshotPruneCandidate],
    pruned_count: u64,
    pruned_bytes: u64,
    lock_acquired: bool,
    lock_holder_id: Option<&'a str>,
}

impl Drop for GraphSnapshotPruneLockOwner<'_> {
    fn drop(&mut self) {
        for lock_id in self.lock_ids.iter().rev() {
            if let Err(error) = self.conn.release_advisory_lock(lock_id, &self.holder_id) {
                tracing::warn!(
                    target: "ee::steward",
                    resource_type = lock_id.resource_type(),
                    resource_id = lock_id.resource_id(),
                    holder_id = self.holder_id.as_str(),
                    lock_ttl_secs = self.ttl_secs,
                    error = %error,
                    "graph snapshot prune advisory lock release failed"
                );
            }
        }
    }
}

#[derive(Debug)]
enum GraphSnapshotPruneLockError {
    Busy {
        resource_type: String,
        resource_id: String,
        holder_id: String,
        acquired_at: String,
    },
    Storage {
        resource_type: String,
        resource_id: String,
        message: String,
    },
}

impl GraphSnapshotPruneLockError {
    const fn code(&self) -> &'static str {
        match self {
            Self::Busy { .. } => "graph_snapshot_prune_lock_busy",
            Self::Storage { .. } => "graph_snapshot_prune_lock_failed",
        }
    }

    fn message(&self) -> String {
        match self {
            Self::Busy {
                resource_type,
                resource_id,
                holder_id,
                acquired_at,
            } => format!(
                "Graph snapshot prune lock {resource_type}:{resource_id} is held by {holder_id} since {acquired_at}"
            ),
            Self::Storage {
                resource_type,
                resource_id,
                message,
            } => format!(
                "Failed to acquire graph snapshot prune lock {resource_type}:{resource_id}: {message}"
            ),
        }
    }
}

#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

fn maintenance_job_process_gates() -> &'static Mutex<BTreeSet<PathBuf>> {
    MAINTENANCE_JOB_PROCESS_GATES.get_or_init(|| Mutex::new(BTreeSet::new()))
}

fn try_acquire_maintenance_job_process_gate(
    lock_path: &Path,
) -> Result<PathBuf, MaintenanceJobLockError> {
    let mut active_paths = maintenance_job_process_gates()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let lock_path = lock_path.to_path_buf();
    if !active_paths.insert(lock_path.clone()) {
        return Err(MaintenanceJobLockError::Busy {
            path: lock_path,
            message: "Another maintenance job is already running in this process.".to_owned(),
        });
    }
    Ok(lock_path)
}

fn release_maintenance_job_process_gate(lock_path: &Path) {
    let mut active_paths = maintenance_job_process_gates()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    active_paths.remove(lock_path);
}

/// Process and file guard for one active maintenance job trigger.
#[derive(Debug)]
pub struct MaintenanceJobLock {
    path: PathBuf,
    holder_id: String,
    _file: File,
    process_gate_path: PathBuf,
}

impl MaintenanceJobLock {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn holder_id(&self) -> &str {
        &self.holder_id
    }
}

impl Drop for MaintenanceJobLock {
    fn drop(&mut self) {
        release_maintenance_job_process_gate(&self.process_gate_path);
    }
}

/// Error returned when the cooperative maintenance job lock cannot be acquired.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MaintenanceJobLockError {
    Busy { path: PathBuf, message: String },
    OpenFailed { path: PathBuf, message: String },
}

impl MaintenanceJobLockError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Busy { .. } => MAINTENANCE_JOB_LOCK_BUSY_CODE,
            Self::OpenFailed { .. } => "maintenance_job_lock_open_failed",
        }
    }

    #[must_use]
    pub const fn severity(&self) -> &'static str {
        match self {
            Self::Busy { .. } => "medium",
            Self::OpenFailed { .. } => "high",
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Busy { message, .. } | Self::OpenFailed { message, .. } => message,
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Busy { path, .. } | Self::OpenFailed { path, .. } => path,
        }
    }

    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": MAINTENANCE_JOB_LOCK_SCHEMA_V1,
            "code": self.code(),
            "severity": self.severity(),
            "message": self.message(),
            "path": self.path().display().to_string(),
            "repair": "Wait for the active maintenance job to finish, then rerun ee job list --json or ee job run <kind> --json.",
        })
    }
}

/// Try to acquire the cooperative lock for manual and daemon maintenance work.
///
/// The lock file is persistent and unlocked by closing the file descriptor; it
/// is intentionally not deleted after a run.
pub fn try_acquire_maintenance_job_lock(
    workspace_path: &Path,
    holder_id: &str,
) -> Result<MaintenanceJobLock, MaintenanceJobLockError> {
    let lock_path = workspace_path.join(".ee").join("maintenance-job.lock");
    ensure_maintenance_job_lock_path_is_not_symlink(&lock_path)?;
    let process_gate_path = try_acquire_maintenance_job_process_gate(&lock_path)?;

    let Some(parent) = lock_path.parent() else {
        release_maintenance_job_process_gate(&process_gate_path);
        return Err(MaintenanceJobLockError::OpenFailed {
            path: lock_path,
            message: "Could not resolve maintenance job lock parent directory.".to_owned(),
        });
    };
    if let Err(error) = fs::create_dir_all(parent) {
        release_maintenance_job_process_gate(&process_gate_path);
        return Err(MaintenanceJobLockError::OpenFailed {
            path: lock_path.clone(),
            message: format!("Failed to create maintenance job lock directory: {error}"),
        });
    }
    if let Err(error) = ensure_maintenance_job_lock_path_is_not_symlink(&lock_path) {
        release_maintenance_job_process_gate(&process_gate_path);
        return Err(error);
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| {
            release_maintenance_job_process_gate(&process_gate_path);
            MaintenanceJobLockError::OpenFailed {
                path: lock_path.clone(),
                message: format!("Failed to open maintenance job lock: {error}"),
            }
        })?;

    #[cfg(unix)]
    if let Err(error) = flock(&file, FlockOperation::NonBlockingLockExclusive) {
        if error == Errno::WOULDBLOCK || error == Errno::AGAIN {
            release_maintenance_job_process_gate(&process_gate_path);
            return Err(MaintenanceJobLockError::Busy {
                path: lock_path,
                message: "Another maintenance job holds the workspace lock.".to_owned(),
            });
        }
        release_maintenance_job_process_gate(&process_gate_path);
        return Err(MaintenanceJobLockError::OpenFailed {
            path: lock_path,
            message: format!("Failed to acquire maintenance job lock: {error}"),
        });
    }

    Ok(MaintenanceJobLock {
        path: lock_path,
        holder_id: holder_id.to_owned(),
        _file: file,
        process_gate_path,
    })
}

fn ensure_maintenance_job_lock_path_is_not_symlink(
    lock_path: &Path,
) -> Result<(), MaintenanceJobLockError> {
    if let Some(symlink_path) = first_existing_symlink_component(lock_path).map_err(|error| {
        MaintenanceJobLockError::OpenFailed {
            path: lock_path.to_path_buf(),
            message: format!(
                "Failed to inspect maintenance job lock path component '{}': {}",
                error.path.display(),
                error.source
            ),
        }
    })? {
        return Err(MaintenanceJobLockError::OpenFailed {
            path: lock_path.to_path_buf(),
            message: format!(
                "Refusing to open maintenance job lock '{}': path traverses symbolic link '{}'",
                lock_path.display(),
                symlink_path.display()
            ),
        });
    }
    Ok(())
}

#[derive(Debug)]
struct SymlinkComponentInspectionError {
    path: PathBuf,
    source: std::io::Error,
}

fn first_existing_symlink_component(
    path: &Path,
) -> Result<Option<PathBuf>, SymlinkComponentInspectionError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(Some(current)),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(source) => {
                return Err(SymlinkComponentInspectionError {
                    path: current,
                    source,
                });
            }
        }
    }
    Ok(None)
}

// ============================================================================
// EE-200: Job Ledger
// ============================================================================

/// Types of maintenance jobs the steward can execute.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum JobType {
    /// Rebuild search indexes from source of truth.
    IndexRebuild,
    /// Process queued incremental search index jobs.
    IndexCoalesce,
    /// Apply time-based decay to memory confidence.
    DecaySweep,
    /// Detect duplicate memories and create consolidation review candidates.
    ConsolidationPass,
    /// Process pending curation candidates.
    CurationReview,
    /// Inspect quarantined harmful feedback rows.
    QuarantineSweep,
    /// Run health checks and generate diagnostics.
    HealthCheck,
    /// Prune derived caches without deleting source-of-truth data.
    CachePruning,
    /// Prune archived graph snapshot derived rows after retention.
    GraphSnapshotPrune,
    /// Compact and optimize storage.
    StorageCompact,
    /// Refresh graph link prediction inputs.
    LinkPredictionRefresh,
    /// Refresh graph centrality metrics.
    CentralityRefresh,
    /// Validate data integrity.
    IntegrityCheck,
    /// Export backup snapshot.
    BackupExport,
    /// Clean up expired or orphaned data.
    GarbageCollection,
    /// Custom job type for extensions.
    Custom,
}

impl JobType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IndexRebuild => "index_rebuild",
            Self::IndexCoalesce => "index_coalesce",
            Self::DecaySweep => "decay_sweep",
            Self::ConsolidationPass => "consolidation_pass",
            Self::CurationReview => "curation_review",
            Self::QuarantineSweep => "quarantine_sweep",
            Self::HealthCheck => "health_check",
            Self::CachePruning => "cache_pruning",
            Self::GraphSnapshotPrune => "graph_snapshot_prune",
            Self::StorageCompact => "storage_compact",
            Self::LinkPredictionRefresh => "link_prediction_refresh",
            Self::CentralityRefresh => "centrality_refresh",
            Self::IntegrityCheck => "integrity_check",
            Self::BackupExport => "backup_export",
            Self::GarbageCollection => "garbage_collection",
            Self::Custom => "custom",
        }
    }

    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::IndexRebuild,
            Self::IndexCoalesce,
            Self::DecaySweep,
            Self::ConsolidationPass,
            Self::CurationReview,
            Self::QuarantineSweep,
            Self::HealthCheck,
            Self::CachePruning,
            Self::GraphSnapshotPrune,
            Self::StorageCompact,
            Self::LinkPredictionRefresh,
            Self::CentralityRefresh,
            Self::IntegrityCheck,
            Self::BackupExport,
            Self::GarbageCollection,
            Self::Custom,
        ]
    }

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::IndexRebuild => "Rebuild search indexes from source of truth",
            Self::IndexCoalesce => "Process queued incremental search index jobs",
            Self::DecaySweep => "Apply time-based decay to memory confidence",
            Self::ConsolidationPass => {
                "Detect duplicate memories and create consolidation review candidates"
            }
            Self::CurationReview => "Process pending curation candidates",
            Self::QuarantineSweep => "Inspect quarantined harmful feedback rows",
            Self::HealthCheck => "Run health checks and generate diagnostics",
            Self::CachePruning => "Prune derived caches without deleting source-of-truth data",
            Self::GraphSnapshotPrune => {
                "Prune archived graph snapshot derived rows after retention"
            }
            Self::StorageCompact => "Compact and optimize storage",
            Self::LinkPredictionRefresh => "Refresh graph link prediction inputs",
            Self::CentralityRefresh => "Refresh graph centrality metrics",
            Self::IntegrityCheck => "Validate data integrity",
            Self::BackupExport => "Export backup snapshot",
            Self::GarbageCollection => "Clean up expired or orphaned data",
            Self::Custom => "Custom job type",
        }
    }
}

impl fmt::Display for JobType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error when parsing an invalid job type string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseJobTypeError {
    input: String,
}

impl fmt::Display for ParseJobTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown job type '{}'", self.input)
    }
}

impl std::error::Error for ParseJobTypeError {}

impl FromStr for JobType {
    type Err = ParseJobTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "index_rebuild" => Ok(Self::IndexRebuild),
            "index_coalesce" => Ok(Self::IndexCoalesce),
            "decay_sweep" => Ok(Self::DecaySweep),
            "consolidation_pass" => Ok(Self::ConsolidationPass),
            "curation_review" => Ok(Self::CurationReview),
            "quarantine_sweep" => Ok(Self::QuarantineSweep),
            "health_check" => Ok(Self::HealthCheck),
            "cache_pruning" => Ok(Self::CachePruning),
            "graph_snapshot_prune" => Ok(Self::GraphSnapshotPrune),
            "storage_compact" => Ok(Self::StorageCompact),
            "link_prediction_refresh" => Ok(Self::LinkPredictionRefresh),
            "centrality_refresh" => Ok(Self::CentralityRefresh),
            "integrity_check" => Ok(Self::IntegrityCheck),
            "backup_export" => Ok(Self::BackupExport),
            "garbage_collection" => Ok(Self::GarbageCollection),
            "custom" => Ok(Self::Custom),
            _ => Err(ParseJobTypeError {
                input: s.to_owned(),
            }),
        }
    }
}

/// Status of a maintenance job.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum JobStatus {
    /// Job is queued but not yet started.
    Pending,
    /// Job is currently executing.
    Running,
    /// Job completed successfully.
    Completed,
    /// Job failed with an error.
    Failed,
    /// Job was cancelled before completion.
    Cancelled,
    /// Job was skipped (preconditions not met).
    Skipped,
}

impl JobStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
        }
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Skipped
        )
    }

    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Completed | Self::Skipped)
    }
}

impl fmt::Display for JobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error when parsing an invalid job status string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseJobStatusError {
    input: String,
}

impl fmt::Display for ParseJobStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown job status '{}'", self.input)
    }
}

impl std::error::Error for ParseJobStatusError {}

impl FromStr for JobStatus {
    type Err = ParseJobStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "skipped" => Ok(Self::Skipped),
            _ => Err(ParseJobStatusError {
                input: s.to_owned(),
            }),
        }
    }
}

/// Priority level for job scheduling.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum JobPriority {
    /// Background task, run when idle.
    Low,
    /// Normal priority.
    #[default]
    Normal,
    /// Higher priority, run before normal jobs.
    High,
    /// Critical, run immediately.
    Critical,
}

impl JobPriority {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Normal => "normal",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }

    #[must_use]
    pub const fn numeric(self) -> u8 {
        match self {
            Self::Low => 1,
            Self::Normal => 2,
            Self::High => 3,
            Self::Critical => 4,
        }
    }
}

impl fmt::Display for JobPriority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single maintenance job record.
#[derive(Clone, Debug)]
pub struct Job {
    /// Unique job identifier.
    pub id: String,
    /// Type of job.
    pub job_type: JobType,
    /// Current status.
    pub status: JobStatus,
    /// Job priority.
    pub priority: JobPriority,
    /// When the job was created/queued.
    pub created_at: String,
    /// When the job started executing.
    pub started_at: Option<String>,
    /// When the job completed (success or failure).
    pub completed_at: Option<String>,
    /// Duration in milliseconds (if completed).
    pub duration_ms: Option<u64>,
    /// Error message (if failed).
    pub error: Option<String>,
    /// Additional context or parameters.
    pub context: Option<String>,
    /// Number of items processed (if applicable).
    pub items_processed: Option<u64>,
}

impl Job {
    /// Create a new pending job.
    #[must_use]
    pub fn new(id: impl Into<String>, job_type: JobType, created_at: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            job_type,
            status: JobStatus::Pending,
            priority: JobPriority::Normal,
            created_at: created_at.into(),
            started_at: None,
            completed_at: None,
            duration_ms: None,
            error: None,
            context: None,
            items_processed: None,
        }
    }

    /// Set job priority.
    #[must_use]
    pub fn with_priority(mut self, priority: JobPriority) -> Self {
        self.priority = priority;
        self
    }

    /// Set job context.
    #[must_use]
    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }

    /// Mark job as started.
    pub fn start(&mut self, started_at: impl Into<String>) {
        self.status = JobStatus::Running;
        self.started_at = Some(started_at.into());
    }

    /// Mark job as completed successfully.
    pub fn complete(&mut self, completed_at: impl Into<String>, items_processed: Option<u64>) {
        self.status = JobStatus::Completed;
        self.completed_at = Some(completed_at.into());
        self.items_processed = items_processed;
        self.calculate_duration();
    }

    /// Mark job as failed.
    pub fn fail(&mut self, completed_at: impl Into<String>, error: impl Into<String>) {
        self.status = JobStatus::Failed;
        self.completed_at = Some(completed_at.into());
        self.error = Some(error.into());
        self.calculate_duration();
    }

    /// Mark job as cancelled.
    pub fn cancel(&mut self, completed_at: impl Into<String>) {
        self.status = JobStatus::Cancelled;
        self.completed_at = Some(completed_at.into());
        self.calculate_duration();
    }

    /// Mark job as skipped.
    pub fn skip(&mut self, completed_at: impl Into<String>, reason: impl Into<String>) {
        self.status = JobStatus::Skipped;
        self.completed_at = Some(completed_at.into());
        self.context = Some(reason.into());
    }

    fn calculate_duration(&mut self) {
        let (Some(started), Some(completed)) = (&self.started_at, &self.completed_at) else {
            return;
        };

        let Ok(start_dt) = DateTime::parse_from_rfc3339(started) else {
            return;
        };
        let Ok(end_dt) = DateTime::parse_from_rfc3339(completed) else {
            return;
        };

        let duration = end_dt.signed_duration_since(start_dt);
        let millis = duration.num_milliseconds();
        if millis >= 0 {
            self.duration_ms = Some(millis as u64);
        }
    }

    /// Render job as JSON.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        let mut obj = json!({
            "id": self.id,
            "jobType": self.job_type.as_str(),
            "status": self.status.as_str(),
            "priority": self.priority.as_str(),
            "createdAt": self.created_at,
        });

        if let Some(obj_map) = obj.as_object_mut() {
            if let Some(ref started) = self.started_at {
                obj_map.insert("startedAt".to_string(), json!(started));
            }
            if let Some(ref completed) = self.completed_at {
                obj_map.insert("completedAt".to_string(), json!(completed));
            }
            if let Some(duration) = self.duration_ms {
                obj_map.insert("durationMs".to_string(), json!(duration));
            }
            if let Some(ref error) = self.error {
                obj_map.insert("error".to_string(), json!(error));
            }
            if let Some(ref context) = self.context {
                obj_map.insert("context".to_string(), json!(context));
            }
            if let Some(items) = self.items_processed {
                obj_map.insert("itemsProcessed".to_string(), json!(items));
            }
        }

        obj
    }
}

/// Statistics about jobs in the ledger.
#[derive(Clone, Debug, Default)]
pub struct JobStatistics {
    pub total: u32,
    pub pending: u32,
    pub running: u32,
    pub completed: u32,
    pub failed: u32,
    pub cancelled: u32,
    pub skipped: u32,
}

impl JobStatistics {
    fn add_job(&mut self, job: &Job) {
        self.total += 1;
        match job.status {
            JobStatus::Pending => self.pending += 1,
            JobStatus::Running => self.running += 1,
            JobStatus::Completed => self.completed += 1,
            JobStatus::Failed => self.failed += 1,
            JobStatus::Cancelled => self.cancelled += 1,
            JobStatus::Skipped => self.skipped += 1,
        }
    }
}

/// The job ledger tracks all maintenance jobs.
#[derive(Clone, Debug, Default)]
pub struct JobLedger {
    jobs: BTreeMap<String, Job>,
    next_id: u64,
}

impl JobLedger {
    /// Create an empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Generate the next job ID.
    pub fn next_job_id(&mut self) -> String {
        self.next_id += 1;
        format!("job-{:06}", self.next_id)
    }

    /// Add a job to the ledger.
    pub fn add_job(&mut self, job: Job) {
        self.jobs.insert(job.id.clone(), job);
    }

    /// Get a job by ID.
    #[must_use]
    pub fn get_job(&self, id: &str) -> Option<&Job> {
        self.jobs.get(id)
    }

    /// Get a mutable job by ID.
    pub fn get_job_mut(&mut self, id: &str) -> Option<&mut Job> {
        self.jobs.get_mut(id)
    }

    /// List all jobs.
    #[must_use]
    pub fn list_jobs(&self) -> Vec<&Job> {
        self.jobs.values().collect()
    }

    /// List jobs by status.
    #[must_use]
    pub fn list_by_status(&self, status: JobStatus) -> Vec<&Job> {
        self.jobs.values().filter(|j| j.status == status).collect()
    }

    /// List jobs by type.
    #[must_use]
    pub fn list_by_type(&self, job_type: JobType) -> Vec<&Job> {
        self.jobs
            .values()
            .filter(|j| j.job_type == job_type)
            .collect()
    }

    /// Get pending jobs sorted by priority (highest first).
    #[must_use]
    pub fn pending_by_priority(&self) -> Vec<&Job> {
        let mut pending: Vec<_> = self.list_by_status(JobStatus::Pending);
        pending.sort_by_key(|job| Reverse(job.priority.numeric()));
        pending
    }

    /// Count jobs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    /// Check if ledger is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// Calculate statistics.
    #[must_use]
    pub fn statistics(&self) -> JobStatistics {
        let mut stats = JobStatistics::default();
        for job in self.jobs.values() {
            stats.add_job(job);
        }
        stats
    }

    /// Render ledger as JSON report.
    #[must_use]
    pub fn report_json(&self) -> JsonValue {
        let stats = self.statistics();
        json!({
            "schema": JOB_LEDGER_SCHEMA_V1,
            "command": "steward jobs",
            "statistics": {
                "total": stats.total,
                "pending": stats.pending,
                "running": stats.running,
                "completed": stats.completed,
                "failed": stats.failed,
                "cancelled": stats.cancelled,
                "skipped": stats.skipped,
            },
            "jobs": self.jobs.values().map(Job::data_json).collect::<Vec<_>>(),
        })
    }

    /// Render human-readable summary.
    #[must_use]
    pub fn report_human(&self) -> String {
        let stats = self.statistics();
        let mut out = String::with_capacity(512);

        out.push_str("Job Ledger\n");
        out.push_str("==========\n\n");
        out.push_str(&format!("Total jobs: {}\n", stats.total));
        out.push_str(&format!("  Pending:   {}\n", stats.pending));
        out.push_str(&format!("  Running:   {}\n", stats.running));
        out.push_str(&format!("  Completed: {}\n", stats.completed));
        out.push_str(&format!("  Failed:    {}\n", stats.failed));
        out.push_str(&format!("  Cancelled: {}\n", stats.cancelled));
        out.push_str(&format!("  Skipped:   {}\n\n", stats.skipped));

        if !self.jobs.is_empty() {
            out.push_str("Jobs:\n");
            for job in self.jobs.values() {
                out.push_str(&format!(
                    "  {} [{}] {} ({})\n",
                    job.id,
                    job.status.as_str(),
                    job.job_type.as_str(),
                    job.priority.as_str()
                ));
            }
        }

        out.push_str("\nNext:\n  ee status --json\n");
        out
    }
}

/// Create a new job and add it to the ledger.
pub fn create_job(
    ledger: &mut JobLedger,
    job_type: JobType,
    priority: JobPriority,
    created_at: impl Into<String>,
    context: Option<String>,
) -> String {
    let id = ledger.next_job_id();
    let mut job = Job::new(&id, job_type, created_at).with_priority(priority);
    if let Some(ctx) = context {
        job = job.with_context(ctx);
    }
    ledger.add_job(job);
    id
}

// ============================================================================
// EE-202: Job Budget Model
// ============================================================================

/// Schema identifier for job budget reports.
pub const JOB_BUDGET_SCHEMA_V1: &str = "ee.steward.job_budget.v1";

/// Resource type that can be budgeted.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ResourceType {
    /// Wall-clock time in milliseconds.
    TimeMs,
    /// Number of items to process.
    Items,
    /// Memory usage in bytes.
    MemoryBytes,
    /// CPU time in milliseconds.
    CpuMs,
    /// I/O operations.
    IoOps,
    /// Network bytes transferred.
    NetworkBytes,
}

impl ResourceType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TimeMs => "time_ms",
            Self::Items => "items",
            Self::MemoryBytes => "memory_bytes",
            Self::CpuMs => "cpu_ms",
            Self::IoOps => "io_ops",
            Self::NetworkBytes => "network_bytes",
        }
    }

    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::TimeMs,
            Self::Items,
            Self::MemoryBytes,
            Self::CpuMs,
            Self::IoOps,
            Self::NetworkBytes,
        ]
    }

    #[must_use]
    pub const fn unit(self) -> &'static str {
        match self {
            Self::TimeMs | Self::CpuMs => "ms",
            Self::Items | Self::IoOps => "count",
            Self::MemoryBytes | Self::NetworkBytes => "bytes",
        }
    }
}

impl fmt::Display for ResourceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single resource budget constraint.
#[derive(Clone, Debug)]
pub struct ResourceBudget {
    /// Type of resource being budgeted.
    pub resource: ResourceType,
    /// Maximum allowed value.
    pub limit: u64,
    /// Behavior when limit is exceeded.
    pub on_exceed: BudgetExceedAction,
}

impl ResourceBudget {
    /// Create a new resource budget.
    #[must_use]
    pub const fn new(resource: ResourceType, limit: u64, on_exceed: BudgetExceedAction) -> Self {
        Self {
            resource,
            limit,
            on_exceed,
        }
    }

    /// Create a hard time limit.
    #[must_use]
    pub const fn time_limit_ms(limit: u64) -> Self {
        Self::new(ResourceType::TimeMs, limit, BudgetExceedAction::Cancel)
    }

    /// Create a soft time limit (warn only).
    #[must_use]
    pub const fn time_soft_limit_ms(limit: u64) -> Self {
        Self::new(ResourceType::TimeMs, limit, BudgetExceedAction::Warn)
    }

    /// Create an item count limit.
    #[must_use]
    pub const fn item_limit(limit: u64) -> Self {
        Self::new(ResourceType::Items, limit, BudgetExceedAction::Cancel)
    }

    /// Render as JSON.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "resource": self.resource.as_str(),
            "limit": self.limit,
            "unit": self.resource.unit(),
            "onExceed": self.on_exceed.as_str(),
        })
    }
}

/// Action to take when a budget limit is exceeded.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum BudgetExceedAction {
    /// Log a warning and continue.
    Warn,
    /// Cancel the job immediately.
    #[default]
    Cancel,
    /// Throttle/slow down execution.
    Throttle,
    /// Checkpoint and pause.
    Checkpoint,
}

impl BudgetExceedAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Cancel => "cancel",
            Self::Throttle => "throttle",
            Self::Checkpoint => "checkpoint",
        }
    }
}

impl fmt::Display for BudgetExceedAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Current consumption of a resource.
#[derive(Clone, Debug, Default)]
pub struct ResourceConsumption {
    /// Amount consumed.
    pub consumed: u64,
    /// Peak usage (for memory).
    pub peak: u64,
}

impl ResourceConsumption {
    /// Add to consumption.
    pub fn add(&mut self, amount: u64) {
        self.consumed = self.consumed.saturating_add(amount);
        self.peak = self.peak.max(self.consumed);
    }

    /// Check if a limit is exceeded.
    #[must_use]
    pub const fn exceeds(&self, limit: u64) -> bool {
        self.consumed > limit
    }

    /// Calculate percentage of limit used.
    #[must_use]
    pub fn percent_of(&self, limit: u64) -> f64 {
        if limit == 0 {
            if self.consumed == 0 { 0.0 } else { 100.0 }
        } else {
            (self.consumed as f64 / limit as f64) * 100.0
        }
    }
}

/// Budget state for an active job.
#[derive(Clone, Debug)]
pub struct JobBudgetState {
    /// Job ID this budget applies to.
    pub job_id: String,
    /// Budget constraints.
    pub budgets: Vec<ResourceBudget>,
    /// Current consumption per resource type.
    pub consumption: BTreeMap<ResourceType, ResourceConsumption>,
    /// Warnings issued for approaching limits.
    pub warnings: Vec<BudgetWarning>,
    /// When tracking started.
    pub started_at: String,
}

impl JobBudgetState {
    /// Create a new budget state for a job.
    #[must_use]
    pub fn new(job_id: impl Into<String>, started_at: impl Into<String>) -> Self {
        Self {
            job_id: job_id.into(),
            budgets: Vec::new(),
            consumption: BTreeMap::new(),
            warnings: Vec::new(),
            started_at: started_at.into(),
        }
    }

    /// Add a budget constraint.
    pub fn add_budget(&mut self, budget: ResourceBudget) {
        self.budgets.push(budget);
    }

    /// Record consumption of a resource.
    pub fn record(&mut self, resource: ResourceType, amount: u64) {
        self.consumption.entry(resource).or_default().add(amount);
    }

    /// Check all budgets and return any exceeded actions.
    #[must_use]
    pub fn check_budgets(&self) -> Vec<BudgetViolation> {
        let mut violations = Vec::new();

        for budget in &self.budgets {
            if let Some(consumption) = self.consumption.get(&budget.resource) {
                if consumption.exceeds(budget.limit) {
                    violations.push(BudgetViolation {
                        resource: budget.resource,
                        limit: budget.limit,
                        consumed: consumption.consumed,
                        action: budget.on_exceed,
                    });
                }
            }
        }

        violations
    }

    /// Check if any hard limit is exceeded (requires cancellation).
    #[must_use]
    pub fn should_cancel(&self) -> bool {
        self.check_budgets()
            .iter()
            .any(|v| v.action == BudgetExceedAction::Cancel)
    }

    /// Get remaining budget for a resource (None if no budget).
    #[must_use]
    pub fn remaining(&self, resource: ResourceType) -> Option<u64> {
        self.budgets
            .iter()
            .find(|b| b.resource == resource)
            .map(|b| {
                let consumed = self.consumption.get(&resource).map_or(0, |c| c.consumed);
                b.limit.saturating_sub(consumed)
            })
    }

    /// Generate a summary report.
    #[must_use]
    pub fn summary(&self) -> BudgetSummary {
        let mut resources = Vec::new();

        for budget in &self.budgets {
            let consumption = self
                .consumption
                .get(&budget.resource)
                .cloned()
                .unwrap_or_default();

            resources.push(ResourceSummary {
                resource: budget.resource,
                limit: budget.limit,
                consumed: consumption.consumed,
                remaining: budget.limit.saturating_sub(consumption.consumed),
                percent_used: consumption.percent_of(budget.limit),
                exceeded: consumption.exceeds(budget.limit),
            });
        }

        BudgetSummary {
            job_id: self.job_id.clone(),
            started_at: self.started_at.clone(),
            resources,
            violations: self.check_budgets(),
            warning_count: self.warnings.len(),
        }
    }

    /// Render as JSON.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        let summary = self.summary();
        json!({
            "schema": JOB_BUDGET_SCHEMA_V1,
            "jobId": summary.job_id,
            "startedAt": summary.started_at,
            "resources": summary.resources.iter().map(|r| json!({
                "resource": r.resource.as_str(),
                "limit": r.limit,
                "consumed": r.consumed,
                "remaining": r.remaining,
                "percentUsed": format!("{:.1}", r.percent_used),
                "exceeded": r.exceeded,
            })).collect::<Vec<_>>(),
            "violations": summary.violations.iter().map(|v| json!({
                "resource": v.resource.as_str(),
                "limit": v.limit,
                "consumed": v.consumed,
                "action": v.action.as_str(),
            })).collect::<Vec<_>>(),
            "warningCount": summary.warning_count,
        })
    }
}

/// A budget violation record.
#[derive(Clone, Debug)]
pub struct BudgetViolation {
    /// Resource that exceeded budget.
    pub resource: ResourceType,
    /// The limit that was set.
    pub limit: u64,
    /// Amount actually consumed.
    pub consumed: u64,
    /// Action to take.
    pub action: BudgetExceedAction,
}

/// A warning issued when approaching a limit.
#[derive(Clone, Debug)]
pub struct BudgetWarning {
    /// Resource approaching limit.
    pub resource: ResourceType,
    /// Threshold that triggered warning (percentage).
    pub threshold_percent: u8,
    /// When the warning was issued.
    pub issued_at: String,
}

/// Summary of budget usage for a resource.
#[derive(Clone, Debug)]
pub struct ResourceSummary {
    pub resource: ResourceType,
    pub limit: u64,
    pub consumed: u64,
    pub remaining: u64,
    pub percent_used: f64,
    pub exceeded: bool,
}

/// Summary of all budget usage for a job.
#[derive(Clone, Debug)]
pub struct BudgetSummary {
    pub job_id: String,
    pub started_at: String,
    pub resources: Vec<ResourceSummary>,
    pub violations: Vec<BudgetViolation>,
    pub warning_count: usize,
}

impl BudgetSummary {
    /// Check if any budget was exceeded.
    #[must_use]
    pub fn has_violations(&self) -> bool {
        !self.violations.is_empty()
    }

    /// Human-readable report.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(512);

        out.push_str(&format!("Budget Summary: {}\n", self.job_id));
        out.push_str(&format!("Started: {}\n\n", self.started_at));

        if self.resources.is_empty() {
            out.push_str("No budgets configured.\n");
        } else {
            out.push_str("Resources:\n");
            for r in &self.resources {
                let status = if r.exceeded { "EXCEEDED" } else { "ok" };
                out.push_str(&format!(
                    "  {}: {}/{} ({:.1}%) [{}]\n",
                    r.resource.as_str(),
                    r.consumed,
                    r.limit,
                    r.percent_used,
                    status
                ));
            }
        }

        if !self.violations.is_empty() {
            out.push_str("\nViolations:\n");
            for v in &self.violations {
                out.push_str(&format!(
                    "  {} exceeded: {}/{} -> {}\n",
                    v.resource.as_str(),
                    v.consumed,
                    v.limit,
                    v.action.as_str()
                ));
            }
        }

        out
    }
}

/// Default budgets for different job types.
#[must_use]
pub fn default_budgets_for_job_type(job_type: JobType) -> Vec<ResourceBudget> {
    match job_type {
        JobType::IndexRebuild => vec![
            ResourceBudget::time_limit_ms(300_000), // 5 minutes
            ResourceBudget::item_limit(100_000),
        ],
        JobType::IndexCoalesce => vec![
            ResourceBudget::time_limit_ms(120_000), // 2 minutes
            ResourceBudget::item_limit(10_000),
        ],
        JobType::DecaySweep => vec![
            ResourceBudget::time_limit_ms(60_000), // 1 minute
            ResourceBudget::item_limit(10_000),
        ],
        JobType::ConsolidationPass => vec![
            ResourceBudget::time_limit_ms(120_000), // 2 minutes
            ResourceBudget::item_limit(10_000),
        ],
        JobType::CurationReview => vec![
            ResourceBudget::time_limit_ms(120_000), // 2 minutes
            ResourceBudget::item_limit(100),
        ],
        JobType::QuarantineSweep => vec![
            ResourceBudget::time_limit_ms(60_000), // 1 minute
            ResourceBudget::item_limit(10_000),
        ],
        JobType::HealthCheck => vec![
            ResourceBudget::time_soft_limit_ms(10_000), // 10 seconds soft
        ],
        JobType::CachePruning => vec![
            ResourceBudget::time_limit_ms(60_000), // 1 minute
            ResourceBudget::item_limit(10_000),
        ],
        JobType::GraphSnapshotPrune => vec![
            ResourceBudget::time_limit_ms(60_000), // 1 minute
            ResourceBudget::item_limit(10_000),
        ],
        JobType::StorageCompact => vec![
            ResourceBudget::time_limit_ms(600_000), // 10 minutes
        ],
        JobType::LinkPredictionRefresh => vec![
            ResourceBudget::time_limit_ms(180_000), // 3 minutes
            ResourceBudget::item_limit(100_000),
        ],
        JobType::CentralityRefresh => vec![
            ResourceBudget::time_limit_ms(180_000), // 3 minutes
            ResourceBudget::item_limit(100_000),
        ],
        JobType::IntegrityCheck => vec![
            ResourceBudget::time_limit_ms(300_000), // 5 minutes
        ],
        JobType::BackupExport => vec![
            ResourceBudget::time_limit_ms(600_000), // 10 minutes
        ],
        JobType::GarbageCollection => vec![
            ResourceBudget::time_limit_ms(60_000), // 1 minute
            ResourceBudget::item_limit(1000),
        ],
        JobType::Custom => vec![
            ResourceBudget::time_soft_limit_ms(60_000), // 1 minute soft default
        ],
    }
}

/// Create a budget state for a job with default budgets.
#[must_use]
pub fn create_job_budget(
    job_id: impl Into<String>,
    job_type: JobType,
    started_at: impl Into<String>,
) -> JobBudgetState {
    let mut state = JobBudgetState::new(job_id, started_at);
    for budget in default_budgets_for_job_type(job_type) {
        state.add_budget(budget);
    }
    state
}

/// Create a custom budget state.
#[must_use]
pub fn create_custom_budget(
    job_id: impl Into<String>,
    started_at: impl Into<String>,
    budgets: Vec<ResourceBudget>,
) -> JobBudgetState {
    let mut state = JobBudgetState::new(job_id, started_at);
    for budget in budgets {
        state.add_budget(budget);
    }
    state
}

// ============================================================================
// EE-206: Score Decay Job
// ============================================================================

/// Schema identifier for score decay job reports.
pub const SCORE_DECAY_JOB_SCHEMA_V1: &str = "ee.steward.score_decay.v1";

/// Schema identifier for steward-managed graph centrality refresh reports.
pub const GRAPH_CENTRALITY_JOB_SCHEMA_V1: &str = "ee.steward.graph_centrality_refresh.v1";

const GRAPH_CENTRALITY_ERROR_SCHEMA_V1: &str = "ee.steward.graph_centrality_refresh.error.v1";

/// Default age after which a memory becomes eligible for time-based decay.
pub const DEFAULT_SCORE_DECAY_STALE_AFTER_DAYS: u32 = 30;

/// Default interval for each staleness decay step.
pub const DEFAULT_SCORE_DECAY_INTERVAL_DAYS: u32 = 30;

/// Default minimum confidence delta before a score update is persisted.
pub const DEFAULT_SCORE_DECAY_MIN_DELTA: f32 = 0.0001;

/// Options for the explicit score decay maintenance job.
#[derive(Clone, Debug)]
pub struct ScoreDecayJobOptions {
    pub workspace_id: String,
    pub as_of: Option<String>,
    pub item_limit: Option<u32>,
    pub stale_after_days: u32,
    pub decay_interval_days: u32,
    pub min_delta: f32,
    pub include_decay_actions: bool,
    pub structural_decay: bool,
    pub decay_thresholds: MemoryDecayThresholds,
    pub decay_half_lives: MemoryDecayHalfLives,
    pub dry_run: bool,
    pub actor: Option<String>,
}

impl ScoreDecayJobOptions {
    #[must_use]
    pub fn new(workspace_id: impl Into<String>) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            as_of: None,
            item_limit: None,
            stale_after_days: DEFAULT_SCORE_DECAY_STALE_AFTER_DAYS,
            decay_interval_days: DEFAULT_SCORE_DECAY_INTERVAL_DAYS,
            min_delta: DEFAULT_SCORE_DECAY_MIN_DELTA,
            include_decay_actions: false,
            structural_decay: true,
            decay_thresholds: MemoryDecayThresholds::default(),
            decay_half_lives: MemoryDecayHalfLives::default(),
            dry_run: false,
            actor: None,
        }
    }
}

/// One memory score considered by the decay job.
#[derive(Clone, Debug, PartialEq)]
pub struct ScoreDecayMemoryChange {
    pub memory_id: String,
    pub old_confidence: f32,
    pub new_confidence: f32,
    pub delta: f32,
    pub age_days: u32,
    pub stale_periods: u32,
    pub freshness: f32,
    pub lifecycle_score: f32,
    pub utility: f32,
    pub half_life_days: f32,
    pub decay_action: MemoryDecayAction,
    pub old_level: String,
    pub new_level: String,
    pub old_importance: f32,
    pub new_importance: f32,
    pub demote_threshold: f32,
    pub forget_threshold: f32,
    pub feedback_total_count: u32,
    pub feedback_event_ids: Vec<String>,
    pub structural_adjustment: Option<ScoreDecayStructuralAdjustment>,
    pub applied: bool,
    pub audit_id: Option<String>,
    pub lifecycle_audit_id: Option<String>,
}

/// Structural multiplier applied to one memory's age-based decay.
#[derive(Clone, Debug, PartialEq)]
pub struct ScoreDecayStructuralAdjustment {
    pub memory_id: String,
    pub onion_layer: Option<usize>,
    pub max_layer: usize,
    pub is_articulation_point: bool,
    pub base_decay: f32,
    pub structural_multiplier: f32,
    pub adjusted_decay: f32,
    pub rationale: String,
}

impl ScoreDecayStructuralAdjustment {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "memoryId": self.memory_id,
            "onionLayer": self.onion_layer,
            "maxLayer": self.max_layer,
            "isArticulationPoint": self.is_articulation_point,
            "baseDecay": score_json(self.base_decay),
            "structuralMultiplier": score_json(self.structural_multiplier),
            "adjustedDecay": score_json(self.adjusted_decay),
            "rationale": self.rationale,
        })
    }
}

impl ScoreDecayMemoryChange {
    #[must_use]
    pub fn confidence_changed(&self, min_delta: f32) -> bool {
        self.delta.abs() >= min_delta
    }

    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        let mut value = json!({
            "memoryId": self.memory_id,
            "oldConfidence": score_json(self.old_confidence),
            "newConfidence": score_json(self.new_confidence),
            "delta": score_json(self.delta),
            "ageDays": self.age_days,
            "stalePeriods": self.stale_periods,
            "freshness": score_json(self.freshness),
            "lifecycleScore": score_json(self.lifecycle_score),
            "utility": score_json(self.utility),
            "halfLifeDays": score_json(self.half_life_days),
            "decayAction": self.decay_action.as_str(),
            "oldLevel": self.old_level,
            "newLevel": self.new_level,
            "oldImportance": score_json(self.old_importance),
            "newImportance": score_json(self.new_importance),
            "demoteThreshold": score_json(self.demote_threshold),
            "forgetThreshold": score_json(self.forget_threshold),
            "feedbackTotalCount": self.feedback_total_count,
            "feedbackEventIds": self.feedback_event_ids,
            "applied": self.applied,
            "auditId": self.audit_id,
            "lifecycleAuditId": self.lifecycle_audit_id,
        });
        if let Some(adjustment) = &self.structural_adjustment {
            value["structuralAdjustment"] = adjustment.data_json();
        }
        value
    }
}

/// Report produced by the score decay job.
#[derive(Clone, Debug, PartialEq)]
pub struct ScoreDecayJobReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub workspace_id: String,
    pub as_of: String,
    pub dry_run: bool,
    pub durable_mutation: bool,
    pub scanned_count: usize,
    pub changed_count: usize,
    pub applied_count: usize,
    pub skipped_count: usize,
    pub demoted_count: usize,
    pub tombstoned_count: usize,
    pub stale_after_days: u32,
    pub decay_interval_days: u32,
    pub min_delta: f32,
    pub include_decay_actions: bool,
    pub structural_decay: bool,
    pub decay_thresholds: MemoryDecayThresholds,
    pub decay_half_lives: MemoryDecayHalfLives,
    pub changes: Vec<ScoreDecayMemoryChange>,
}

impl ScoreDecayJobReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        let structural_adjustments = self
            .changes
            .iter()
            .filter_map(|change| change.structural_adjustment.as_ref())
            .map(ScoreDecayStructuralAdjustment::data_json)
            .collect::<Vec<_>>();
        let mut decay = json!({
            "schema": MEMORY_DECAY_SOURCE,
            "enabled": self.include_decay_actions,
            "memoriesDemoted": self.demoted_count,
            "memoriesTombstoned": self.tombstoned_count,
            "halfLivesApplied": self.include_decay_actions,
            "halfLifeDays": {
                "working": score_json(self.decay_half_lives.working),
                "episodicEvent": score_json(self.decay_half_lives.episodic_event),
                "episodicFailure": score_json(self.decay_half_lives.episodic_failure),
                "semanticFact": score_json(self.decay_half_lives.semantic_fact),
                "proceduralRule": score_json(self.decay_half_lives.procedural_rule),
                "default": score_json(self.decay_half_lives.default),
            },
            "thresholdDemote": score_json(self.decay_thresholds.demote),
            "thresholdForget": score_json(self.decay_thresholds.forget),
            "dryRun": self.dry_run,
        });
        if self.include_decay_actions && self.structural_decay {
            decay["structuralDecayEnabled"] = json!(true);
            decay["structuralAdjustments"] = JsonValue::Array(structural_adjustments);
        }

        json!({
            "schema": self.schema,
            "command": self.command,
            "workspaceId": self.workspace_id,
            "asOf": self.as_of,
            "dryRun": self.dry_run,
            "durableMutation": self.durable_mutation,
            "policy": {
                "staleAfterDays": self.stale_after_days,
                "decayIntervalDays": self.decay_interval_days,
                "minDelta": score_json(self.min_delta),
            },
            "decay": decay,
            "summary": {
                "scannedCount": self.scanned_count,
                "changedCount": self.changed_count,
                "appliedCount": self.applied_count,
                "skippedCount": self.skipped_count,
                "demotedCount": self.demoted_count,
                "tombstonedCount": self.tombstoned_count,
            },
            "changes": self
                .changes
                .iter()
                .map(ScoreDecayMemoryChange::data_json)
                .collect::<Vec<_>>(),
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = String::new();
        output.push_str("Score Decay Job\n");
        output.push_str("================\n\n");
        output.push_str(&format!("Workspace: {}\n", self.workspace_id));
        output.push_str(&format!("As of:     {}\n", self.as_of));
        output.push_str(&format!("Dry run:   {}\n\n", self.dry_run));
        output.push_str("Summary:\n");
        output.push_str(&format!("  Scanned: {}\n", self.scanned_count));
        output.push_str(&format!("  Changed: {}\n", self.changed_count));
        output.push_str(&format!("  Applied: {}\n", self.applied_count));
        output.push_str(&format!("  Skipped: {}\n", self.skipped_count));
        output.push_str(&format!("  Demoted: {}\n", self.demoted_count));
        output.push_str(&format!("  Tombstoned: {}\n", self.tombstoned_count));
        if !self.changes.is_empty() {
            output.push_str("\nChanges:\n");
            for change in &self.changes {
                output.push_str(&format!(
                    "  {}: {:.4} -> {:.4} ({:.4})\n",
                    change.memory_id, change.old_confidence, change.new_confidence, change.delta
                ));
            }
        }
        output
    }
}

/// Run the explicit score decay maintenance job over active memories.
///
/// The job is report-only in dry-run mode. In mutating mode it applies bounded
/// confidence decreases and records an audit entry for each changed memory.
/// Feedback events that contributed to an applied decrease are marked applied in
/// the same transaction so rerunning the job is idempotent for those signals.
pub fn run_score_decay_job(
    conn: &DbConnection,
    options: &ScoreDecayJobOptions,
) -> Result<ScoreDecayJobReport, String> {
    validate_score_decay_options(options)?;
    let as_of = options
        .as_of
        .clone()
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    let as_of_timestamp = parse_score_decay_timestamp(&as_of, "as_of")?;

    let mut memories = conn
        .list_memories(&options.workspace_id, None, false)
        .map_err(|error| format!("Failed to list memories for score decay: {error}"))?;
    if let Some(limit) = options.item_limit {
        memories.truncate(
            usize::try_from(limit)
                .map_err(|_| "Score decay item limit exceeds platform usize".to_owned())?,
        );
    }
    let access_times = if options.include_decay_actions {
        latest_memory_access_timestamps(conn, &options.workspace_id)?
    } else {
        BTreeMap::new()
    };
    let structural_adjustments = if options.include_decay_actions && options.structural_decay {
        structural_decay_adjustments(conn, &memories, &access_times, &as_of_timestamp, options)?
    } else {
        BTreeMap::new()
    };

    let mut scanned_count = 0usize;
    let mut changes = Vec::new();

    for memory in memories {
        scanned_count = scanned_count.saturating_add(1);
        let feedback_events = conn
            .list_feedback_events_for_target("memory", &memory.id)
            .map_err(|error| {
                format!(
                    "Failed to list feedback events for memory {}: {error}",
                    memory.id
                )
            })?
            .into_iter()
            .filter(|event| event.applied_at.is_none())
            .collect::<Vec<_>>();
        let feedback_counts = feedback_counts_from_events(&feedback_events);
        let Some(mut change) = score_decay_change_for_memory(
            &memory,
            &feedback_counts,
            &feedback_events,
            &as_of_timestamp,
            access_times.get(&memory.id).copied(),
            structural_adjustments.get(&memory.id).cloned(),
            options,
        )?
        else {
            continue;
        };

        if !options.dry_run {
            if change.confidence_changed(options.min_delta) {
                let details = score_decay_audit_details(&change, &as_of);
                change.audit_id = conn
                    .apply_memory_score_update_audited(
                        &memory.id,
                        &ApplyMemoryScoreUpdateInput {
                            workspace_id: options.workspace_id.clone(),
                            confidence: change.new_confidence,
                            utility: memory.utility,
                            importance: memory.importance,
                            updated_at: as_of.clone(),
                            actor: options.actor.clone(),
                            details,
                            feedback_event_ids: change.feedback_event_ids.clone(),
                        },
                    )
                    .map_err(|error| {
                        format!(
                            "Failed to apply score decay for memory {}: {error}",
                            memory.id
                        )
                    })?;
            }
            if options.include_decay_actions {
                apply_decay_lifecycle_action(conn, &memory, &mut change, options, &as_of)?;
            }
            change.applied = change.audit_id.is_some() || change.lifecycle_audit_id.is_some();
        }

        changes.push(change);
    }

    let applied_count = changes.iter().filter(|change| change.applied).count();
    let demoted_count = changes
        .iter()
        .filter(|change| change.decay_action == MemoryDecayAction::Demote)
        .count();
    let tombstoned_count = changes
        .iter()
        .filter(|change| change.decay_action == MemoryDecayAction::Tombstone)
        .count();
    let changed_count = changes.len();
    let skipped_count = scanned_count.saturating_sub(changed_count);

    Ok(ScoreDecayJobReport {
        schema: SCORE_DECAY_JOB_SCHEMA_V1,
        command: "steward score-decay",
        workspace_id: options.workspace_id.clone(),
        as_of,
        dry_run: options.dry_run,
        durable_mutation: applied_count > 0,
        scanned_count,
        changed_count,
        applied_count,
        skipped_count,
        demoted_count,
        tombstoned_count,
        stale_after_days: options.stale_after_days,
        decay_interval_days: options.decay_interval_days,
        min_delta: options.min_delta,
        include_decay_actions: options.include_decay_actions,
        structural_decay: options.structural_decay,
        decay_thresholds: options.decay_thresholds,
        decay_half_lives: options.decay_half_lives,
        changes,
    })
}

fn validate_score_decay_options(options: &ScoreDecayJobOptions) -> Result<(), String> {
    if options.workspace_id.trim().is_empty() {
        return Err("Score decay workspace_id must not be empty".to_owned());
    }
    if options.decay_interval_days == 0 {
        return Err("Score decay interval must be at least one day".to_owned());
    }
    if !options.min_delta.is_finite() || options.min_delta < 0.0 {
        return Err("Score decay min_delta must be a finite non-negative number".to_owned());
    }
    if !options.decay_half_lives.is_valid() {
        return Err("Score decay half-lives must be finite positive numbers".to_owned());
    }
    Ok(())
}

fn structural_decay_adjustments(
    conn: &DbConnection,
    memories: &[StoredMemory],
    access_times: &BTreeMap<String, DateTime<Utc>>,
    as_of: &DateTime<Utc>,
    options: &ScoreDecayJobOptions,
) -> Result<BTreeMap<String, ScoreDecayStructuralAdjustment>, String> {
    let memory_ids = memories
        .iter()
        .map(|memory| memory.id.clone())
        .collect::<BTreeSet<_>>();
    let links = conn
        .list_all_memory_links(None)
        .map_err(|error| format!("Failed to list memory links for structural decay: {error}"))?;
    let graph = structural_decay_graph(&memory_ids, &links);
    let mut adjustments = BTreeMap::new();
    for memory in memories {
        let reference =
            memory_decay_reference_time(memory, access_times.get(&memory.id).copied(), as_of)?;
        let half_life_days = f64::from(
            options
                .decay_half_lives
                .for_memory(&memory.level, &memory.kind),
        );
        let age_days =
            as_of.signed_duration_since(reference).num_seconds().max(0) as f64 / 86_400.0;
        let base_decay = 1.0 - memory_decay_freshness_score(age_days, half_life_days);
        let structural = compute_structural_decay_adjustment(&graph, &memory.id);
        adjustments.insert(
            memory.id.clone(),
            score_decay_structural_adjustment(&memory.id, base_decay, structural),
        );
    }
    Ok(adjustments)
}

fn structural_decay_graph(memory_ids: &BTreeSet<String>, links: &[StoredMemoryLink]) -> Graph {
    let mut graph = Graph::new(CompatibilityMode::Strict);
    for memory_id in memory_ids {
        graph.add_node(memory_id);
    }
    for link in links {
        if !crate::graph::memory_link_mesh_metadata_visible(link.metadata_json.as_deref()) {
            continue;
        }
        if !memory_ids.contains(&link.src_memory_id) || !memory_ids.contains(&link.dst_memory_id) {
            continue;
        }
        graph.add_node(&link.src_memory_id);
        graph.add_node(&link.dst_memory_id);
        let _ = graph
            .extend_edges_unrecorded([(link.src_memory_id.as_str(), link.dst_memory_id.as_str())]);
    }
    graph
}

fn score_decay_structural_adjustment(
    memory_id: &str,
    base_decay: f32,
    structural: StructuralDecayMultiplier,
) -> ScoreDecayStructuralAdjustment {
    let structural_multiplier = round_score(structural.structural_multiplier as f32);
    let adjusted_decay = round_score((base_decay * structural_multiplier).clamp(0.0, 1.0));
    ScoreDecayStructuralAdjustment {
        memory_id: memory_id.to_owned(),
        onion_layer: structural.onion_layer,
        max_layer: structural.max_layer,
        is_articulation_point: structural.is_articulation_point,
        base_decay: round_score(base_decay),
        structural_multiplier,
        adjusted_decay,
        rationale: structural.rationale,
    }
}

fn structurally_adjusted_reference_time(
    reference: DateTime<Utc>,
    as_of: DateTime<Utc>,
    adjustment: &ScoreDecayStructuralAdjustment,
) -> Result<DateTime<Utc>, String> {
    let age_seconds = as_of.signed_duration_since(reference).num_seconds().max(0);
    let adjusted_seconds = (age_seconds as f64 * f64::from(adjustment.structural_multiplier))
        .round()
        .clamp(0.0, i64::MAX as f64) as i64;
    as_of
        .checked_sub_signed(ChronoDuration::seconds(adjusted_seconds))
        .ok_or_else(|| "Structural decay adjusted timestamp is out of range".to_owned())
}

fn score_decay_change_for_memory(
    memory: &StoredMemory,
    feedback_counts: &FeedbackCounts,
    feedback_events: &[StoredFeedbackEvent],
    as_of: &DateTime<Utc>,
    last_accessed_at: Option<DateTime<Utc>>,
    structural_adjustment: Option<ScoreDecayStructuralAdjustment>,
    options: &ScoreDecayJobOptions,
) -> Result<Option<ScoreDecayMemoryChange>, String> {
    let age_days = score_age_days(&memory.updated_at, as_of)?;
    let stale_periods = score_decay_stale_periods(age_days, options);
    let new_confidence =
        decayed_confidence(memory.confidence, feedback_counts, age_days, stale_periods);
    let old_confidence = round_score(memory.confidence);
    let new_confidence = round_score(new_confidence);
    let delta = round_score(new_confidence - old_confidence);
    let decay_evaluation = if options.include_decay_actions {
        let reference = memory_decay_reference_time(memory, last_accessed_at, as_of)?;
        let reference = structural_adjustment
            .as_ref()
            .map_or(Ok(reference), |adjustment| {
                structurally_adjusted_reference_time(reference, *as_of, adjustment)
            })?;
        evaluate_memory_decay_with_settings(
            memory,
            reference,
            *as_of,
            MemoryDecaySettings {
                thresholds: options.decay_thresholds,
                half_lives: options.decay_half_lives,
            },
        )
    } else {
        MemoryDecayEvaluation {
            action: MemoryDecayAction::Preserve,
            freshness: 1.0,
            lifecycle_score: round_score(memory.confidence * memory.utility),
            half_life_days: 0.0,
            age_days,
            previous_level: memory.level.clone(),
            new_level: memory.level.clone(),
            previous_importance: round_score(memory.importance),
            new_importance: round_score(memory.importance),
            demote_threshold: options.decay_thresholds.demote,
            forget_threshold: options.decay_thresholds.forget,
        }
    };

    if delta.abs() < options.min_delta && decay_evaluation.action == MemoryDecayAction::Preserve {
        return Ok(None);
    }

    Ok(Some(ScoreDecayMemoryChange {
        memory_id: memory.id.clone(),
        old_confidence,
        new_confidence,
        delta,
        age_days,
        stale_periods,
        freshness: decay_evaluation.freshness,
        lifecycle_score: decay_evaluation.lifecycle_score,
        utility: round_score(memory.utility),
        half_life_days: decay_evaluation.half_life_days,
        decay_action: decay_evaluation.action,
        old_level: decay_evaluation.previous_level,
        new_level: decay_evaluation.new_level,
        old_importance: decay_evaluation.previous_importance,
        new_importance: decay_evaluation.new_importance,
        demote_threshold: decay_evaluation.demote_threshold,
        forget_threshold: decay_evaluation.forget_threshold,
        feedback_total_count: feedback_counts.total_count(),
        feedback_event_ids: feedback_events
            .iter()
            .filter(|event| score_decay_consumes_feedback_event(event))
            .map(|event| event.id.clone())
            .collect::<Vec<_>>(),
        structural_adjustment,
        applied: false,
        audit_id: None,
        lifecycle_audit_id: None,
    }))
}

fn latest_memory_access_timestamps(
    conn: &DbConnection,
    workspace_id: &str,
) -> Result<BTreeMap<String, DateTime<Utc>>, String> {
    let entries = conn
        .list_audit_entries(Some(workspace_id), None)
        .map_err(|error| format!("Failed to list memory access audit rows: {error}"))?;
    Ok(memory_access_timestamp_map(&entries))
}

fn memory_access_timestamp_map(entries: &[StoredAuditEntry]) -> BTreeMap<String, DateTime<Utc>> {
    let mut timestamps = BTreeMap::new();
    for entry in entries {
        if !is_memory_access_audit_action(&entry.action) {
            continue;
        }
        if entry.target_type.as_deref() != Some("memory") {
            continue;
        }
        let Some(memory_id) = entry.target_id.as_deref() else {
            continue;
        };
        let Some(timestamp) = parse_score_decay_timestamp(&entry.timestamp, "audit.timestamp").ok()
        else {
            continue;
        };
        timestamps
            .entry(memory_id.to_owned())
            .and_modify(|existing| {
                if timestamp > *existing {
                    *existing = timestamp;
                }
            })
            .or_insert(timestamp);
    }
    timestamps
}

fn is_memory_access_audit_action(action: &str) -> bool {
    matches!(
        action,
        audit_actions::SEARCH_RETURNED_MEM
            | audit_actions::PACK_INCLUDED_MEM
            | audit_actions::MEMORY_SHOW
            | audit_actions::WHY_INSPECTED
    )
}

fn memory_decay_reference_time(
    memory: &StoredMemory,
    last_accessed_at: Option<DateTime<Utc>>,
    as_of: &DateTime<Utc>,
) -> Result<DateTime<Utc>, String> {
    let updated_at = parse_score_decay_timestamp(&memory.updated_at, "memory.updated_at")?;
    let created_at = parse_score_decay_timestamp(&memory.created_at, "memory.created_at")?;
    let reference = [Some(updated_at), Some(created_at), last_accessed_at]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(*as_of);
    Ok(reference.min(*as_of))
}

fn apply_decay_lifecycle_action(
    conn: &DbConnection,
    memory: &StoredMemory,
    change: &mut ScoreDecayMemoryChange,
    options: &ScoreDecayJobOptions,
    as_of: &str,
) -> Result<(), String> {
    let details = decay_lifecycle_audit_details(change, as_of);
    match change.decay_action {
        MemoryDecayAction::Preserve => Ok(()),
        MemoryDecayAction::Demote => {
            change.lifecycle_audit_id = conn
                .apply_memory_decay_demotion_audited(
                    &memory.id,
                    &ApplyMemoryDecayDemotionInput {
                        workspace_id: options.workspace_id.clone(),
                        level: change.new_level.clone(),
                        importance: change.new_importance,
                        updated_at: as_of.to_owned(),
                        actor: options.actor.clone(),
                        details,
                    },
                )
                .map_err(|error| {
                    format!(
                        "Failed to apply decay demotion for memory {}: {error}",
                        memory.id
                    )
                })?;
            if change.lifecycle_audit_id.is_some() {
                tracing::info!(
                    target: "ee::learn::decay",
                    memory_id = %memory.id,
                    freshness = change.freshness,
                    confidence = change.new_confidence,
                    utility = memory.utility,
                    action = change.decay_action.as_str(),
                    "memory demoted by decay"
                );
            }
            Ok(())
        }
        MemoryDecayAction::Tombstone => {
            change.lifecycle_audit_id = conn
                .tombstone_memory_decay_audited(
                    &memory.id,
                    &options.workspace_id,
                    options.actor.as_deref(),
                    &details,
                )
                .map_err(|error| {
                    format!(
                        "Failed to apply decay tombstone for memory {}: {error}",
                        memory.id
                    )
                })?;
            if change.lifecycle_audit_id.is_some() {
                tracing::info!(
                    target: "ee::learn::decay",
                    memory_id = %memory.id,
                    freshness = change.freshness,
                    confidence = change.new_confidence,
                    utility = memory.utility,
                    action = change.decay_action.as_str(),
                    "memory tombstoned by decay"
                );
            }
            Ok(())
        }
    }
}

fn decayed_confidence(
    current_confidence: f32,
    feedback_counts: &FeedbackCounts,
    age_days: u32,
    stale_periods: u32,
) -> f32 {
    let stale_factor = feedback_scoring::STALENESS_DECAY_RATE
        .powi(i32::try_from(stale_periods).unwrap_or(i32::MAX));
    let time_decayed = (current_confidence * stale_factor).clamp(
        feedback_scoring::CONFIDENCE_FLOOR,
        feedback_scoring::CONFIDENCE_CEILING,
    );
    feedback_counts
        .apply_to_confidence_at_age(time_decayed, age_days)
        .min(current_confidence)
        .clamp(
            feedback_scoring::CONFIDENCE_FLOOR,
            feedback_scoring::CONFIDENCE_CEILING,
        )
}

fn score_decay_stale_periods(age_days: u32, options: &ScoreDecayJobOptions) -> u32 {
    if age_days < options.stale_after_days {
        return 0;
    }
    age_days
        .saturating_sub(options.stale_after_days)
        .checked_div(options.decay_interval_days)
        .unwrap_or(0)
        .saturating_add(1)
}

fn score_age_days(updated_at: &str, as_of: &DateTime<Utc>) -> Result<u32, String> {
    let updated_at = parse_score_decay_timestamp(updated_at, "memory.updated_at")?;
    let seconds = as_of.signed_duration_since(updated_at).num_seconds().max(0);
    u32::try_from(seconds / 86_400)
        .map_err(|_| "Score decay age exceeds supported u32 day range".to_owned())
}

fn parse_score_decay_timestamp(raw: &str, field: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(raw)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|error| format!("Invalid score decay {field} timestamp: {error}"))
}

fn feedback_counts_from_events(events: &[StoredFeedbackEvent]) -> FeedbackCounts {
    let mut counts = FeedbackCounts::default();
    for event in events {
        match event.signal.as_str() {
            "positive" | "helpful" | "confirmation" => {
                counts.positive_weight += event.weight;
                counts.positive_count = counts.positive_count.saturating_add(1);
            }
            "negative" | "harmful" | "contradiction" | "inaccurate" => {
                counts.negative_weight += event.weight;
                counts.negative_count = counts.negative_count.saturating_add(1);
            }
            "stale" | "outdated" => {
                counts.decay_weight += event.weight;
                counts.decay_count = counts.decay_count.saturating_add(1);
            }
            _ => {
                counts.neutral_weight += event.weight;
                counts.neutral_count = counts.neutral_count.saturating_add(1);
            }
        }
    }
    counts
}

fn score_decay_consumes_feedback_event(event: &StoredFeedbackEvent) -> bool {
    matches!(
        event.signal.as_str(),
        "negative" | "harmful" | "contradiction" | "inaccurate" | "stale" | "outdated"
    )
}

fn score_decay_audit_details(change: &ScoreDecayMemoryChange, as_of: &str) -> String {
    json!({
        "schema": "ee.audit.memory_score_decay.v1",
        "command": "steward score-decay",
        "memoryId": change.memory_id,
        "asOf": as_of,
        "oldConfidence": score_json(change.old_confidence),
        "newConfidence": score_json(change.new_confidence),
        "delta": score_json(change.delta),
        "ageDays": change.age_days,
        "stalePeriods": change.stale_periods,
        "feedbackTotalCount": change.feedback_total_count,
        "feedbackEventIds": change.feedback_event_ids,
    })
    .to_string()
}

fn decay_lifecycle_audit_details(change: &ScoreDecayMemoryChange, as_of: &str) -> String {
    json!({
        "schema": "ee.audit.memory_decay_lifecycle.v1",
        "command": "steward score-decay",
        "memoryId": change.memory_id,
        "asOf": as_of,
        "action": change.decay_action.as_str(),
        "reason": if change.decay_action == MemoryDecayAction::Tombstone {
            "auto_forgetting"
        } else {
            "auto_decay_demotion"
        },
        "previousLevel": change.old_level,
        "newLevel": change.new_level,
        "previousImportance": score_json(change.old_importance),
        "newImportance": score_json(change.new_importance),
        "freshness": score_json(change.freshness),
        "confidence": score_json(change.new_confidence),
        "utility": score_json(change.utility),
        "lifecycleScore": score_json(change.lifecycle_score),
        "halfLifeDays": score_json(change.half_life_days),
        "ageDays": change.age_days,
        "demoteThreshold": score_json(change.demote_threshold),
        "forgetThreshold": score_json(change.forget_threshold),
    })
    .to_string()
}

fn round_score(value: f32) -> f32 {
    if value.is_finite() {
        (value * 1_000_000.0).round() / 1_000_000.0
    } else {
        feedback_scoring::CONFIDENCE_FLOOR
    }
}

fn score_json(value: f32) -> JsonValue {
    serde_json::Number::from_f64(f64::from(round_score(value)))
        .map_or(JsonValue::Null, JsonValue::Number)
}

// ============================================================================
// EE-203: Manual Steward Runner
// ============================================================================

/// Schema identifier for runner reports.
pub const RUNNER_REPORT_SCHEMA_V1: &str = "ee.steward.runner_report.v1";

/// Outcome of running a job.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RunOutcome {
    /// Job completed successfully.
    Success,
    /// Job failed with an error.
    Failed,
    /// Job was cancelled (budget exceeded or manual).
    Cancelled,
    /// Job was skipped (preconditions not met).
    Skipped,
    /// Job timed out.
    TimedOut,
}

impl RunOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
            Self::TimedOut => "timed_out",
        }
    }

    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Success | Self::Skipped)
    }
}

impl fmt::Display for RunOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

type JobWorkResult = (RunOutcome, Option<u64>, Option<String>, Option<JsonValue>);

/// Options for the manual runner.
#[derive(Clone, Debug, Default)]
pub struct RunnerOptions {
    /// Maximum time budget in milliseconds (overrides job default).
    pub time_limit_ms: Option<u64>,
    /// Maximum items to process (overrides job default).
    pub item_limit: Option<u64>,
    /// Workspace path used to resolve the default database and workspace row.
    pub workspace_path: Option<PathBuf>,
    /// Explicit database path for maintenance handlers.
    pub database_path: Option<PathBuf>,
    /// Explicit workspace ID for handlers that need durable workspace scope.
    pub workspace_id: Option<String>,
    /// Deterministic timestamp used by maintenance handlers that support it.
    pub as_of: Option<String>,
    /// Actor recorded on durable maintenance audit rows.
    pub actor: Option<String>,
    /// Whether to perform a dry run (report what would happen).
    pub dry_run: bool,
    /// Whether decay_sweep should apply lifecycle demotion/tombstone decisions.
    pub include_decay_actions: bool,
    /// Whether decay_sweep should adjust lifecycle decay by graph structure.
    pub structural_decay: bool,
    /// Resolved decay settings from defaults and workspace config.
    pub decay_settings: MemoryDecaySettings,
    /// Whether to continue on non-fatal errors.
    pub continue_on_error: bool,
    /// Verbose diagnostics.
    pub verbose: bool,
}

impl RunnerOptions {
    #[must_use]
    pub fn new() -> Self {
        Self {
            structural_decay: true,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_time_limit(mut self, ms: u64) -> Self {
        self.time_limit_ms = Some(ms);
        self
    }

    #[must_use]
    pub fn with_item_limit(mut self, limit: u64) -> Self {
        self.item_limit = Some(limit);
        self
    }

    #[must_use]
    pub fn with_workspace_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.workspace_path = Some(path.into());
        self
    }

    #[must_use]
    pub fn with_database_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.database_path = Some(path.into());
        self
    }

    #[must_use]
    pub fn with_workspace_id(mut self, workspace_id: impl Into<String>) -> Self {
        self.workspace_id = Some(workspace_id.into());
        self
    }

    #[must_use]
    pub fn with_as_of(mut self, as_of: impl Into<String>) -> Self {
        self.as_of = Some(as_of.into());
        self
    }

    #[must_use]
    pub fn with_actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = Some(actor.into());
        self
    }

    #[must_use]
    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    #[must_use]
    pub fn with_include_decay_actions(mut self, include_decay_actions: bool) -> Self {
        self.include_decay_actions = include_decay_actions;
        self
    }

    #[must_use]
    pub fn with_structural_decay(mut self, structural_decay: bool) -> Self {
        self.structural_decay = structural_decay;
        self
    }

    #[must_use]
    pub fn with_decay_settings(mut self, decay_settings: MemoryDecaySettings) -> Self {
        self.decay_settings = decay_settings;
        self
    }

    #[must_use]
    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }
}

/// Result of running a single job.
#[derive(Clone, Debug)]
pub struct JobRunResult {
    /// Job that was run.
    pub job_id: String,
    /// Job type.
    pub job_type: JobType,
    /// Outcome of the run.
    pub outcome: RunOutcome,
    /// Duration in milliseconds.
    pub duration_ms: u64,
    /// Items processed (if applicable).
    pub items_processed: Option<u64>,
    /// Error message (if failed).
    pub error: Option<String>,
    /// Budget state at completion.
    pub budget_summary: Option<BudgetSummary>,
    /// Real handler details, when a maintenance handler ran or abstained.
    pub details: Option<JsonValue>,
    /// Whether this was a dry run.
    pub dry_run: bool,
}

impl JobRunResult {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        let mut obj = json!({
            "jobId": self.job_id,
            "jobType": self.job_type.as_str(),
            "outcome": self.outcome.as_str(),
            "durationMs": self.duration_ms,
            "dryRun": self.dry_run,
        });

        if let Some(obj_map) = obj.as_object_mut() {
            if let Some(items) = self.items_processed {
                obj_map.insert("itemsProcessed".to_string(), json!(items));
            }
            if let Some(ref error) = self.error {
                obj_map.insert("error".to_string(), json!(error));
            }
            if let Some(ref summary) = self.budget_summary {
                obj_map.insert(
                    "budgetUsed".to_string(),
                    json!({
                        "violations": summary.violations.len(),
                        "warningCount": summary.warning_count,
                    }),
                );
            }
            if let Some(ref details) = self.details {
                obj_map.insert("details".to_string(), details.clone());
            }
        }

        obj
    }
}

/// Report from running multiple jobs.
#[derive(Clone, Debug)]
pub struct RunnerReport {
    /// Results for each job run.
    pub results: Vec<JobRunResult>,
    /// Total duration in milliseconds.
    pub total_duration_ms: u64,
    /// Jobs that succeeded.
    pub succeeded: u32,
    /// Jobs that failed.
    pub failed: u32,
    /// Jobs that were skipped.
    pub skipped: u32,
    /// Whether the run was cancelled.
    pub was_cancelled: bool,
    /// When the run started.
    pub started_at: String,
    /// When the run completed.
    pub completed_at: String,
}

impl RunnerReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": RUNNER_REPORT_SCHEMA_V1,
            "command": "steward run",
            "startedAt": self.started_at,
            "completedAt": self.completed_at,
            "totalDurationMs": self.total_duration_ms,
            "summary": {
                "total": self.results.len(),
                "succeeded": self.succeeded,
                "failed": self.failed,
                "skipped": self.skipped,
                "wasCancelled": self.was_cancelled,
            },
            "results": self.results.iter().map(JobRunResult::data_json).collect::<Vec<_>>(),
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(512);

        out.push_str("Steward Run Report\n");
        out.push_str("==================\n\n");
        out.push_str(&format!("Started:   {}\n", self.started_at));
        out.push_str(&format!("Completed: {}\n", self.completed_at));
        out.push_str(&format!("Duration:  {} ms\n\n", self.total_duration_ms));

        out.push_str("Summary:\n");
        out.push_str(&format!("  Total:     {}\n", self.results.len()));
        out.push_str(&format!("  Succeeded: {}\n", self.succeeded));
        out.push_str(&format!("  Failed:    {}\n", self.failed));
        out.push_str(&format!("  Skipped:   {}\n", self.skipped));

        if self.was_cancelled {
            out.push_str("\n[Run was cancelled]\n");
        }

        if !self.results.is_empty() {
            out.push_str("\nJobs:\n");
            for result in &self.results {
                let status = result.outcome.as_str();
                let duration = result.duration_ms;
                out.push_str(&format!(
                    "  {} [{}] {} ({} ms)\n",
                    result.job_id, status, result.job_type, duration
                ));
                if let Some(ref error) = result.error {
                    out.push_str(&format!("    Error: {error}\n"));
                }
            }
        }

        out.push_str("\nNext:\n  ee status --json\n");
        out
    }

    #[must_use]
    pub fn all_succeeded(&self) -> bool {
        self.failed == 0 && !self.was_cancelled
    }
}

/// The manual steward runner executes jobs synchronously in CLI mode.
#[derive(Clone, Debug)]
pub struct ManualRunner {
    options: RunnerOptions,
    ledger: JobLedger,
}

impl ManualRunner {
    /// Create a new manual runner.
    #[must_use]
    pub fn new(options: RunnerOptions) -> Self {
        Self {
            options,
            ledger: JobLedger::new(),
        }
    }

    /// Create a runner with an existing ledger.
    #[must_use]
    pub fn with_ledger(options: RunnerOptions, ledger: JobLedger) -> Self {
        Self { options, ledger }
    }

    /// Get the runner options.
    #[must_use]
    pub fn options(&self) -> &RunnerOptions {
        &self.options
    }

    /// Get the job ledger.
    #[must_use]
    pub fn ledger(&self) -> &JobLedger {
        &self.ledger
    }

    /// Get a mutable reference to the ledger.
    pub fn ledger_mut(&mut self) -> &mut JobLedger {
        &mut self.ledger
    }

    /// Schedule a job for execution.
    pub fn schedule(
        &mut self,
        job_type: JobType,
        priority: JobPriority,
        context: Option<String>,
    ) -> String {
        let timestamp = chrono::Utc::now().to_rfc3339();
        create_job(&mut self.ledger, job_type, priority, timestamp, context)
    }

    /// Run a single job by ID.
    pub fn run_job(&mut self, job_id: &str, now: &str) -> Option<JobRunResult> {
        let job = self.ledger.get_job_mut(job_id)?;
        let job_type = job.job_type;

        if job.status.is_terminal() {
            return Some(JobRunResult {
                job_id: job_id.to_owned(),
                job_type,
                outcome: RunOutcome::Skipped,
                duration_ms: 0,
                items_processed: None,
                error: Some("Job already completed".to_owned()),
                budget_summary: None,
                details: None,
                dry_run: self.options.dry_run,
            });
        }

        job.start(now);

        let mut budget = create_job_budget(job_id, job_type, now);
        if let Some(time_limit) = self.options.time_limit_ms {
            budget.add_budget(ResourceBudget::time_limit_ms(time_limit));
        }
        if let Some(item_limit) = self.options.item_limit {
            budget.add_budget(ResourceBudget::item_limit(item_limit));
        }

        let (outcome, items, error, details) = self.execute_job_work(job_type, &mut budget);

        let completion_time = chrono::Utc::now().to_rfc3339();
        let job = self.ledger.get_job_mut(job_id)?;

        match outcome {
            RunOutcome::Success => job.complete(&completion_time, items),
            RunOutcome::Failed => job.fail(
                &completion_time,
                error.as_deref().unwrap_or("unknown error"),
            ),
            RunOutcome::Cancelled => job.cancel(&completion_time),
            RunOutcome::Skipped => {
                job.skip(&completion_time, error.as_deref().unwrap_or("skipped"))
            }
            RunOutcome::TimedOut => job.fail(&completion_time, "timed out"),
        }

        Some(JobRunResult {
            job_id: job_id.to_owned(),
            job_type,
            outcome,
            duration_ms: job.duration_ms.unwrap_or(0),
            items_processed: items,
            error,
            budget_summary: Some(budget.summary()),
            details,
            dry_run: self.options.dry_run,
        })
    }

    fn execute_job_work(
        &self,
        job_type: JobType,
        budget: &mut JobBudgetState,
    ) -> (RunOutcome, Option<u64>, Option<String>, Option<JsonValue>) {
        match job_type {
            JobType::IndexRebuild => self.execute_index_rebuild(budget),
            JobType::IndexCoalesce => self.execute_index_coalesce(budget),
            JobType::DecaySweep => self.execute_decay_sweep(budget),
            JobType::ConsolidationPass => self.execute_consolidation_pass(budget),
            JobType::CurationReview => self.execute_curation_review(budget),
            JobType::QuarantineSweep => self.execute_quarantine_sweep(budget),
            JobType::HealthCheck => self.execute_health_check(budget),
            JobType::CachePruning => self.execute_cache_pruning(budget),
            JobType::GraphSnapshotPrune => self.execute_graph_snapshot_prune(budget),
            JobType::StorageCompact => self.execute_storage_compact(budget),
            JobType::LinkPredictionRefresh => self.execute_graph_centrality_refresh(budget),
            JobType::CentralityRefresh => self.execute_graph_centrality_refresh(budget),
            JobType::IntegrityCheck => self.execute_integrity_check(budget),
            JobType::BackupExport => self.execute_backup_export(budget),
            JobType::GarbageCollection => self.execute_garbage_collection(budget),
            JobType::Custom => self.execute_custom_job(),
        }
    }

    fn execute_index_rebuild(
        &self,
        budget: &mut JobBudgetState,
    ) -> (RunOutcome, Option<u64>, Option<String>, Option<JsonValue>) {
        let Some(database_path) = self.resolve_database_path() else {
            let message = "Index rebuild requires a database path or workspace path with .ee/ee.db"
                .to_owned();
            return steward_job_failure(
                "ee.steward.index_rebuild.error.v1",
                "index_rebuild_database_unresolved",
                message,
                self.options.dry_run,
                None,
                "ee init --workspace .",
            );
        };
        if !database_path.exists() {
            let message = format!(
                "Index rebuild database does not exist: {}",
                database_path.display()
            );
            return steward_job_failure(
                "ee.steward.index_rebuild.error.v1",
                "index_rebuild_database_missing",
                message,
                self.options.dry_run,
                Some(&database_path),
                "ee init --workspace .",
            );
        }

        let started = Instant::now();
        let workspace_path = self.normalized_workspace_path();
        let mut preflight_options = crate::core::index::IndexRebuildOptions {
            workspace_path: workspace_path.clone(),
            database_path: Some(database_path.clone()),
            index_dir: None,
            dry_run: true,
        };
        let preflight = match crate::core::index::rebuild_index(&preflight_options) {
            Ok(report) => report,
            Err(error) => {
                let message = format!("Index rebuild preflight failed: {error}");
                return steward_job_failure(
                    "ee.steward.index_rebuild.error.v1",
                    "index_rebuild_preflight_failed",
                    message,
                    true,
                    Some(&database_path),
                    "ee doctor --json",
                );
            }
        };

        let planned_documents = u64::from(preflight.documents_total);
        budget.record(ResourceType::Items, planned_documents);
        let preflight_elapsed_ms = millis_to_u64(started.elapsed());
        budget.record(ResourceType::TimeMs, preflight_elapsed_ms);

        if budget_cancels_before_mutation(budget) {
            return (
                RunOutcome::Cancelled,
                Some(planned_documents),
                Some("Budget exceeded before durable index rebuild".to_owned()),
                Some(index_rebuild_job_details(
                    &database_path,
                    &preflight,
                    None,
                    self.options.dry_run,
                    false,
                )),
            );
        }

        if self.options.dry_run {
            return (
                RunOutcome::Success,
                Some(planned_documents),
                None,
                Some(index_rebuild_job_details(
                    &database_path,
                    &preflight,
                    None,
                    true,
                    false,
                )),
            );
        }

        preflight_options.dry_run = false;
        let report = match crate::core::index::rebuild_index(&preflight_options) {
            Ok(report) => report,
            Err(error) => {
                let message = format!("Index rebuild failed: {error}");
                return steward_job_failure(
                    "ee.steward.index_rebuild.error.v1",
                    "index_rebuild_failed",
                    message,
                    false,
                    Some(&database_path),
                    "ee index rebuild --workspace . --json",
                );
            }
        };
        let total_elapsed_ms = millis_to_u64(started.elapsed());
        budget.record(
            ResourceType::TimeMs,
            total_elapsed_ms.saturating_sub(preflight_elapsed_ms),
        );
        let durable_mutation = matches!(
            report.status,
            crate::core::index::IndexRebuildStatus::Success
        );
        let outcome = if matches!(
            report.status,
            crate::core::index::IndexRebuildStatus::Success
                | crate::core::index::IndexRebuildStatus::NoDocuments
        ) {
            RunOutcome::Success
        } else {
            RunOutcome::Failed
        };
        let error = if outcome == RunOutcome::Failed {
            Some(format!(
                "Index rebuild ended with status {}",
                report.status.as_str()
            ))
        } else {
            None
        };

        (
            outcome,
            Some(u64::from(report.documents_total)),
            error,
            Some(index_rebuild_job_details(
                &database_path,
                &preflight,
                Some(&report),
                false,
                durable_mutation,
            )),
        )
    }

    fn execute_index_coalesce(
        &self,
        budget: &mut JobBudgetState,
    ) -> (RunOutcome, Option<u64>, Option<String>, Option<JsonValue>) {
        let Some(database_path) = self.resolve_database_path() else {
            let message =
                "Index coalesce requires a database path or workspace path with .ee/ee.db"
                    .to_owned();
            return steward_job_failure(
                "ee.steward.index_coalesce.error.v1",
                "index_coalesce_database_unresolved",
                message,
                self.options.dry_run,
                None,
                "ee init --workspace .",
            );
        };
        if !database_path.exists() {
            let message = format!(
                "Index coalesce database does not exist: {}",
                database_path.display()
            );
            return steward_job_failure(
                "ee.steward.index_coalesce.error.v1",
                "index_coalesce_database_missing",
                message,
                self.options.dry_run,
                Some(&database_path),
                "ee init --workspace .",
            );
        }

        let started = Instant::now();
        let workspace_path = self.normalized_workspace_path();
        let job_limit = match self.options.item_limit {
            Some(limit) => match u32::try_from(limit) {
                Ok(limit) => Some(limit),
                Err(_) => {
                    let message = "Index coalesce job limit exceeds u32".to_owned();
                    return steward_job_failure(
                        "ee.steward.index_coalesce.error.v1",
                        "index_coalesce_job_limit_too_large",
                        message,
                        self.options.dry_run,
                        Some(&database_path),
                        "Use a smaller --item-limit.",
                    );
                }
            },
            None => None,
        };
        let mut options = crate::core::index::IndexProcessingOptions {
            workspace_path,
            database_path: Some(database_path.clone()),
            index_dir: None,
            dry_run: true,
            job_limit,
        };
        let preflight = match crate::core::index::process_index_jobs(&options) {
            Ok(report) => report,
            Err(error) => {
                let message = format!("Index coalesce preflight failed: {error}");
                return steward_job_failure(
                    "ee.steward.index_coalesce.error.v1",
                    "index_coalesce_preflight_failed",
                    message,
                    true,
                    Some(&database_path),
                    "ee doctor --json",
                );
            }
        };

        budget.record(ResourceType::Items, u64::from(preflight.pending_jobs));
        let preflight_elapsed_ms = millis_to_u64(started.elapsed());
        budget.record(ResourceType::TimeMs, preflight_elapsed_ms);

        if budget_cancels_before_mutation(budget) {
            return (
                RunOutcome::Cancelled,
                Some(u64::from(preflight.pending_jobs)),
                Some("Budget exceeded before durable index job processing".to_owned()),
                Some(index_coalesce_job_details(
                    &preflight,
                    None,
                    self.options.dry_run,
                    false,
                )),
            );
        }

        if self.options.dry_run {
            return (
                RunOutcome::Success,
                Some(u64::from(preflight.pending_jobs)),
                None,
                Some(index_coalesce_job_details(&preflight, None, true, false)),
            );
        }

        options.dry_run = false;
        let report = match crate::core::index::process_index_jobs(&options) {
            Ok(report) => report,
            Err(error) => {
                let message = format!("Index coalesce failed: {error}");
                return steward_job_failure(
                    "ee.steward.index_coalesce.error.v1",
                    "index_coalesce_failed",
                    message,
                    false,
                    Some(&database_path),
                    "ee index process --workspace . --json",
                );
            }
        };
        let total_elapsed_ms = millis_to_u64(started.elapsed());
        budget.record(
            ResourceType::TimeMs,
            total_elapsed_ms.saturating_sub(preflight_elapsed_ms),
        );
        let outcome = if matches!(
            report.status,
            crate::core::index::IndexProcessingStatus::Success
                | crate::core::index::IndexProcessingStatus::NoPendingJobs
        ) {
            RunOutcome::Success
        } else {
            RunOutcome::Failed
        };
        let error = if outcome == RunOutcome::Failed {
            Some(format!(
                "Index coalesce ended with status {}",
                report.status.as_str()
            ))
        } else {
            None
        };

        (
            outcome,
            Some(u64::from(report.processed_jobs)),
            error,
            Some(index_coalesce_job_details(
                &preflight,
                Some(&report),
                false,
                report.processed_jobs > 0,
            )),
        )
    }

    fn execute_decay_sweep(
        &self,
        budget: &mut JobBudgetState,
    ) -> (RunOutcome, Option<u64>, Option<String>, Option<JsonValue>) {
        let Some(database_path) = self.resolve_database_path() else {
            let message =
                "Decay sweep requires a database path or workspace path with .ee/ee.db".to_owned();
            return (
                RunOutcome::Failed,
                None,
                Some(message.clone()),
                Some(json!({
                    "schema": "ee.steward.decay_sweep.error.v1",
                    "code": "decay_sweep_database_unresolved",
                    "severity": "medium",
                    "message": message,
                    "repair": "ee init --workspace .",
                })),
            );
        };

        let started = std::time::Instant::now();
        if !database_path.exists() {
            let message = format!(
                "Decay sweep database does not exist: {}",
                database_path.display()
            );
            return (
                RunOutcome::Failed,
                None,
                Some(message.clone()),
                Some(json!({
                    "schema": "ee.steward.decay_sweep.error.v1",
                    "code": "decay_sweep_database_missing",
                    "severity": "medium",
                    "databasePath": database_path.display().to_string(),
                    "dryRun": self.options.dry_run,
                    "durableMutation": false,
                    "message": message,
                    "repair": "ee init --workspace .",
                })),
            );
        }

        let connection = match if self.options.dry_run {
            DbConnection::open_schema_only(&database_path)
        } else {
            DbConnection::open_file(&database_path)
        } {
            Ok(connection) => connection,
            Err(error) => {
                let message = format!(
                    "Failed to open maintenance database {}: {error}",
                    database_path.display()
                );
                return (
                    RunOutcome::Failed,
                    None,
                    Some(message.clone()),
                    Some(json!({
                        "schema": "ee.steward.decay_sweep.error.v1",
                        "code": "decay_sweep_database_open_failed",
                        "severity": "high",
                        "databasePath": database_path.display().to_string(),
                        "message": message,
                        "repair": "ee init --workspace .",
                    })),
                );
            }
        };
        if !self.options.dry_run {
            if let Err(error) = connection.migrate() {
                let message = format!("Failed to migrate maintenance database: {error}");
                return (
                    RunOutcome::Failed,
                    None,
                    Some(message.clone()),
                    Some(json!({
                        "schema": "ee.steward.decay_sweep.error.v1",
                        "code": "decay_sweep_migration_failed",
                        "severity": "high",
                        "databasePath": database_path.display().to_string(),
                        "message": message,
                        "repair": "ee doctor --json",
                    })),
                );
            }
        }

        let workspace_id = match self.resolve_workspace_id(&connection) {
            Ok(workspace_id) => workspace_id,
            Err(message) => {
                return (
                    RunOutcome::Failed,
                    None,
                    Some(message.clone()),
                    Some(json!({
                        "schema": "ee.steward.decay_sweep.error.v1",
                        "code": "decay_sweep_workspace_unresolved",
                        "severity": "medium",
                        "databasePath": database_path.display().to_string(),
                        "message": message,
                        "repair": "ee remember --workspace . --json",
                    })),
                );
            }
        };

        let item_limit = match self.options.item_limit {
            Some(limit) => match u32::try_from(limit) {
                Ok(limit) => Some(limit),
                Err(_) => {
                    let message = "Decay sweep item limit exceeds u32".to_owned();
                    return (
                        RunOutcome::Failed,
                        None,
                        Some(message.clone()),
                        Some(json!({
                            "schema": "ee.steward.decay_sweep.error.v1",
                            "code": "decay_sweep_item_limit_too_large",
                            "severity": "medium",
                            "message": message,
                        })),
                    );
                }
            },
            None => None,
        };

        let mut options = ScoreDecayJobOptions::new(workspace_id);
        options.as_of = self.options.as_of.clone();
        options.item_limit = item_limit;
        options.dry_run = self.options.dry_run;
        options.include_decay_actions = self.options.include_decay_actions;
        options.structural_decay = self.options.structural_decay;
        options.decay_thresholds = self.options.decay_settings.thresholds;
        options.decay_half_lives = self.options.decay_settings.half_lives;
        options.actor = self
            .options
            .actor
            .clone()
            .or_else(|| Some("ee-steward".to_owned()));

        let mut preflight_options = options.clone();
        preflight_options.dry_run = true;
        let preflight_report = match run_score_decay_job(&connection, &preflight_options) {
            Ok(report) => report,
            Err(message) => {
                return (
                    RunOutcome::Failed,
                    None,
                    Some(message.clone()),
                    Some(json!({
                        "schema": "ee.steward.decay_sweep.error.v1",
                        "code": "decay_sweep_handler_failed",
                        "severity": "high",
                        "message": message,
                    })),
                );
            }
        };

        let scanned_count = usize_to_u64(preflight_report.scanned_count);
        budget.record(ResourceType::Items, scanned_count);
        let preflight_elapsed_ms = millis_to_u64(started.elapsed());
        budget.record(ResourceType::TimeMs, preflight_elapsed_ms);

        if budget_times_out_before_mutation(budget) {
            return (
                RunOutcome::TimedOut,
                Some(scanned_count),
                Some("Timed out before durable decay mutations".to_owned()),
                Some(preflight_report.data_json()),
            );
        }

        if budget_cancels_before_mutation(budget) {
            return (
                RunOutcome::Cancelled,
                Some(scanned_count),
                Some("Budget exceeded before durable decay mutations".to_owned()),
                Some(preflight_report.data_json()),
            );
        }

        if options.dry_run {
            return (
                RunOutcome::Success,
                Some(scanned_count),
                None,
                Some(preflight_report.data_json()),
            );
        }

        let report = match run_score_decay_job(&connection, &options) {
            Ok(report) => report,
            Err(message) => {
                return (
                    RunOutcome::Failed,
                    None,
                    Some(message.clone()),
                    Some(json!({
                        "schema": "ee.steward.decay_sweep.error.v1",
                        "code": "decay_sweep_handler_failed",
                        "severity": "high",
                        "message": message,
                    })),
                );
            }
        };
        let total_elapsed_ms = millis_to_u64(started.elapsed());
        budget.record(
            ResourceType::TimeMs,
            total_elapsed_ms.saturating_sub(preflight_elapsed_ms),
        );

        (
            RunOutcome::Success,
            Some(scanned_count),
            None,
            Some(report.data_json()),
        )
    }

    fn execute_graph_centrality_refresh(
        &self,
        budget: &mut JobBudgetState,
    ) -> (RunOutcome, Option<u64>, Option<String>, Option<JsonValue>) {
        let Some(database_path) = self.resolve_database_path() else {
            let message = "Graph centrality refresh requires a database path or workspace path with .ee/ee.db".to_owned();
            return graph_centrality_failure(
                "graph_centrality_database_unresolved",
                message,
                self.options.dry_run,
                None,
            );
        };

        if self.options.dry_run && !database_path.exists() {
            let message = format!(
                "Dry-run graph centrality database does not exist: {}",
                database_path.display()
            );
            return graph_centrality_failure(
                "graph_centrality_database_missing",
                message,
                true,
                Some(&database_path),
            );
        }

        let started = Instant::now();
        let connection = match if self.options.dry_run {
            DbConnection::open_schema_only(&database_path)
        } else {
            DbConnection::open_file(&database_path)
        } {
            Ok(connection) => connection,
            Err(error) => {
                let message = format!(
                    "Failed to open graph centrality database {}: {error}",
                    database_path.display()
                );
                return graph_centrality_failure(
                    "graph_centrality_database_open_failed",
                    message,
                    self.options.dry_run,
                    Some(&database_path),
                );
            }
        };

        if !self.options.dry_run {
            if let Err(error) = connection.migrate() {
                let message = format!("Failed to migrate graph centrality database: {error}");
                return graph_centrality_failure(
                    "graph_centrality_migration_failed",
                    message,
                    false,
                    Some(&database_path),
                );
            }
        }

        let workspace_id = match self.resolve_workspace_id(&connection) {
            Ok(workspace_id) => workspace_id,
            Err(message) => {
                return graph_centrality_failure(
                    "graph_centrality_workspace_unresolved",
                    message,
                    self.options.dry_run,
                    Some(&database_path),
                );
            }
        };
        let link_limit = match self.graph_link_limit() {
            Ok(link_limit) => link_limit,
            Err(message) => {
                return graph_centrality_failure(
                    "graph_centrality_link_limit_too_large",
                    message,
                    self.options.dry_run,
                    Some(&database_path),
                );
            }
        };

        let mut refresh_options = crate::graph::CentralityRefreshOptions {
            dry_run: true,
            min_weight: None,
            min_confidence: None,
            link_limit,
        };
        let preflight = match crate::graph::refresh_graph_snapshot(
            &connection,
            &workspace_id,
            &refresh_options,
        ) {
            Ok(report) => report,
            Err(error) => {
                let message = format!("Graph centrality dry-run preflight failed: {error}");
                return graph_centrality_failure(
                    "graph_centrality_preflight_failed",
                    message,
                    true,
                    Some(&database_path),
                );
            }
        };

        let preflight_edge_count = usize_to_u64(preflight.centrality.edge_count);
        budget.record(ResourceType::Items, preflight_edge_count);
        let preflight_elapsed_ms = millis_to_u64(started.elapsed());
        budget.record(ResourceType::TimeMs, preflight_elapsed_ms);

        if budget_cancels_before_mutation(budget) {
            return (
                RunOutcome::Cancelled,
                Some(preflight_edge_count),
                Some("Budget exceeded before durable graph snapshot mutation".to_owned()),
                Some(graph_centrality_job_details(
                    &workspace_id,
                    &preflight,
                    None,
                    self.options.dry_run,
                    false,
                )),
            );
        }

        if self.options.dry_run {
            return (
                RunOutcome::Success,
                Some(preflight_edge_count),
                None,
                Some(graph_centrality_job_details(
                    &workspace_id,
                    &preflight,
                    None,
                    self.options.dry_run,
                    false,
                )),
            );
        }

        refresh_options.dry_run = false;
        let report = match crate::graph::refresh_graph_snapshot(
            &connection,
            &workspace_id,
            &refresh_options,
        ) {
            Ok(report) => report,
            Err(error) => {
                let message = format!("Graph centrality refresh failed: {error}");
                return graph_centrality_failure(
                    "graph_centrality_refresh_failed",
                    message,
                    false,
                    Some(&database_path),
                );
            }
        };
        let total_elapsed_ms = millis_to_u64(started.elapsed());
        budget.record(
            ResourceType::TimeMs,
            total_elapsed_ms.saturating_sub(preflight_elapsed_ms),
        );
        let durable_mutation = report.snapshot.is_some();

        (
            RunOutcome::Success,
            Some(usize_to_u64(report.centrality.edge_count)),
            None,
            Some(graph_centrality_job_details(
                &workspace_id,
                &preflight,
                Some(&report),
                self.options.dry_run,
                durable_mutation,
            )),
        )
    }

    fn execute_consolidation_pass(
        &self,
        budget: &mut JobBudgetState,
    ) -> (RunOutcome, Option<u64>, Option<String>, Option<JsonValue>) {
        let opened = match self.open_workspace_database_for_job(
            "ee.steward.consolidation_pass.error.v1",
            "consolidation_pass",
            "ee init --workspace .",
        ) {
            Ok(opened) => opened,
            Err(result) => return result,
        };

        let memories = match opened
            .connection
            .list_memories(&opened.workspace_id, None, false)
        {
            Ok(memories) => memories,
            Err(error) => {
                let message = format!("Failed to list memories for consolidation: {error}");
                return steward_job_failure(
                    "ee.steward.consolidation_pass.error.v1",
                    "consolidation_pass_memory_query_failed",
                    message,
                    self.options.dry_run,
                    Some(&opened.database_path),
                    "ee doctor --json",
                );
            }
        };

        let plan = plan_consolidation_candidates(&opened.workspace_id, &memories);
        budget.record(ResourceType::Items, usize_to_u64(memories.len()));
        budget.record(ResourceType::TimeMs, 0);

        if budget_cancels_before_mutation(budget) {
            return (
                RunOutcome::Cancelled,
                Some(usize_to_u64(memories.len())),
                Some("Budget exceeded before durable consolidation candidates".to_owned()),
                Some(consolidation_pass_details(
                    &opened,
                    &plan,
                    0,
                    0,
                    self.options.dry_run,
                    false,
                )),
            );
        }

        if self.options.dry_run {
            return (
                RunOutcome::Success,
                Some(usize_to_u64(plan.len())),
                None,
                Some(consolidation_pass_details(
                    &opened, &plan, 0, 0, true, false,
                )),
            );
        }

        let mut inserted = 0_u64;
        let mut already_pending = 0_u64;
        for candidate in &plan {
            let existing = match opened.connection.list_curation_candidates(
                &opened.workspace_id,
                Some("consolidation"),
                Some("pending"),
                Some(&candidate.target_memory_id),
            ) {
                Ok(existing) => existing,
                Err(error) => {
                    let message =
                        format!("Failed to inspect pending consolidation candidates: {error}");
                    return steward_job_failure(
                        "ee.steward.consolidation_pass.error.v1",
                        "consolidation_pass_candidate_query_failed",
                        message,
                        false,
                        Some(&opened.database_path),
                        "ee doctor --json",
                    );
                }
            };
            if !existing.is_empty() {
                already_pending = already_pending.saturating_add(1);
                continue;
            }

            let input = CreateCurationCandidateInput {
                workspace_id: opened.workspace_id.clone(),
                candidate_type: "consolidation".to_owned(),
                target_memory_id: candidate.target_memory_id.clone(),
                proposed_content: Some(candidate.proposed_content.clone()),
                proposed_confidence: Some(candidate.proposed_confidence),
                proposed_trust_class: None,
                source_type: "steward.consolidation_pass".to_owned(),
                source_id: Some(candidate.source_memory_id.clone()),
                reason: candidate.reason.clone(),
                confidence: 0.82,
                status: Some("pending".to_owned()),
                created_at: self.options.as_of.clone(),
                ttl_expires_at: None,
            };
            if let Err(error) = opened
                .connection
                .insert_curation_candidate(&candidate.candidate_id, &input)
            {
                let message = format!(
                    "Failed to insert consolidation candidate {}: {error}",
                    candidate.candidate_id
                );
                return steward_job_failure(
                    "ee.steward.consolidation_pass.error.v1",
                    "consolidation_pass_candidate_insert_failed",
                    message,
                    false,
                    Some(&opened.database_path),
                    "ee doctor --json",
                );
            }
            inserted = inserted.saturating_add(1);
        }

        (
            RunOutcome::Success,
            Some(usize_to_u64(plan.len())),
            None,
            Some(consolidation_pass_details(
                &opened,
                &plan,
                inserted,
                already_pending,
                false,
                inserted > 0,
            )),
        )
    }

    fn execute_curation_review(
        &self,
        budget: &mut JobBudgetState,
    ) -> (RunOutcome, Option<u64>, Option<String>, Option<JsonValue>) {
        let opened = match self.open_workspace_database_for_job(
            "ee.steward.curation_review.error.v1",
            "curation_review",
            "ee curate status --json",
        ) {
            Ok(opened) => opened,
            Err(result) => return result,
        };
        let candidates = match opened.connection.list_curation_candidates(
            &opened.workspace_id,
            None,
            Some("pending"),
            None,
        ) {
            Ok(candidates) => candidates,
            Err(error) => {
                let message = format!("Failed to list pending curation candidates: {error}");
                return steward_job_failure(
                    "ee.steward.curation_review.error.v1",
                    "curation_review_query_failed",
                    message,
                    self.options.dry_run,
                    Some(&opened.database_path),
                    "ee doctor --json",
                );
            }
        };
        budget.record(ResourceType::Items, usize_to_u64(candidates.len()));

        (
            RunOutcome::Success,
            Some(usize_to_u64(candidates.len())),
            None,
            Some(json!({
                "schema": "ee.steward.curation_review.v1",
                "jobType": JobType::CurationReview.as_str(),
                "workspaceId": opened.workspace_id,
                "databasePath": opened.database_path.display().to_string(),
                "pendingCandidates": candidates.len(),
                "candidateIds": candidates
                    .iter()
                    .map(|candidate| candidate.id.as_str())
                    .collect::<Vec<_>>(),
                "dryRun": self.options.dry_run,
                "durableMutation": false,
            })),
        )
    }

    fn execute_quarantine_sweep(
        &self,
        budget: &mut JobBudgetState,
    ) -> (RunOutcome, Option<u64>, Option<String>, Option<JsonValue>) {
        let opened = match self.open_workspace_database_for_job(
            "ee.steward.quarantine_sweep.error.v1",
            "quarantine_sweep",
            "ee outcome quarantine list --json",
        ) {
            Ok(opened) => opened,
            Err(result) => return result,
        };
        let pending = match opened
            .connection
            .list_feedback_quarantine(&opened.workspace_id, Some("pending"))
        {
            Ok(rows) => rows,
            Err(error) => {
                let message = format!("Failed to list pending feedback quarantine rows: {error}");
                return steward_job_failure(
                    "ee.steward.quarantine_sweep.error.v1",
                    "quarantine_sweep_query_failed",
                    message,
                    self.options.dry_run,
                    Some(&opened.database_path),
                    "ee doctor --json",
                );
            }
        };
        budget.record(ResourceType::Items, usize_to_u64(pending.len()));

        let mut by_source = BTreeMap::<String, u64>::new();
        for row in &pending {
            *by_source.entry(row.source_id.clone()).or_default() += 1;
        }

        (
            RunOutcome::Success,
            Some(usize_to_u64(pending.len())),
            None,
            Some(json!({
                "schema": "ee.steward.quarantine_sweep.v1",
                "jobType": JobType::QuarantineSweep.as_str(),
                "workspaceId": opened.workspace_id,
                "databasePath": opened.database_path.display().to_string(),
                "pendingRows": pending.len(),
                "pendingBySource": by_source,
                "rowIds": pending
                    .iter()
                    .map(|row| row.id.as_str())
                    .collect::<Vec<_>>(),
                "dryRun": self.options.dry_run,
                "durableMutation": false,
            })),
        )
    }

    fn execute_health_check(
        &self,
        budget: &mut JobBudgetState,
    ) -> (RunOutcome, Option<u64>, Option<String>, Option<JsonValue>) {
        let started = Instant::now();
        let Some(database_path) = self.resolve_database_path() else {
            return (
                RunOutcome::Success,
                Some(0),
                None,
                Some(json!({
                    "schema": "ee.steward.health_check.v1",
                    "jobType": JobType::HealthCheck.as_str(),
                    "storageStatus": "unresolved",
                    "workspace": self.normalized_workspace_path().display().to_string(),
                    "checks": [],
                    "dryRun": self.options.dry_run,
                    "durableMutation": false,
                })),
            );
        };
        if !database_path.exists() {
            return (
                RunOutcome::Success,
                Some(0),
                None,
                Some(json!({
                    "schema": "ee.steward.health_check.v1",
                    "jobType": JobType::HealthCheck.as_str(),
                    "storageStatus": "missing",
                    "databasePath": database_path.display().to_string(),
                    "checks": [],
                    "dryRun": self.options.dry_run,
                    "durableMutation": false,
                })),
            );
        }
        let connection = match DbConnection::open_file(&database_path) {
            Ok(connection) => connection,
            Err(error) => {
                let message = format!("Failed to open health-check database: {error}");
                return steward_job_failure(
                    "ee.steward.health_check.error.v1",
                    "health_check_database_open_failed",
                    message,
                    self.options.dry_run,
                    Some(&database_path),
                    "ee doctor --json",
                );
            }
        };
        let workspaces = match connection.list_workspaces() {
            Ok(workspaces) => workspaces,
            Err(error) => {
                let message = format!("Failed to inspect workspace rows: {error}");
                return steward_job_failure(
                    "ee.steward.health_check.error.v1",
                    "health_check_workspace_query_failed",
                    message,
                    self.options.dry_run,
                    Some(&database_path),
                    "ee doctor --json",
                );
            }
        };
        budget.record(ResourceType::TimeMs, millis_to_u64(started.elapsed()));

        (
            RunOutcome::Success,
            Some(usize_to_u64(workspaces.len())),
            None,
            Some(json!({
                "schema": "ee.steward.health_check.v1",
                "jobType": JobType::HealthCheck.as_str(),
                "storageStatus": "ready",
                "databasePath": database_path.display().to_string(),
                "workspaceRows": workspaces.len(),
                "checks": [
                    {
                        "name": "database_open",
                        "status": "ok"
                    },
                    {
                        "name": "workspace_rows",
                        "status": "ok",
                        "count": workspaces.len()
                    }
                ],
                "dryRun": self.options.dry_run,
                "durableMutation": false,
            })),
        )
    }

    fn execute_cache_pruning(
        &self,
        budget: &mut JobBudgetState,
    ) -> (RunOutcome, Option<u64>, Option<String>, Option<JsonValue>) {
        let workspace_path = self.normalized_workspace_path();
        let cache_path = workspace_path.join(".ee").join("cache");
        let exists = cache_path.exists();
        budget.record(ResourceType::Items, 0);

        (
            RunOutcome::Success,
            Some(0),
            None,
            Some(json!({
                "schema": "ee.steward.cache_pruning.v1",
                "jobType": JobType::CachePruning.as_str(),
                "workspace": workspace_path.display().to_string(),
                "cachePath": cache_path.display().to_string(),
                "cacheExists": exists,
                "filesDeleted": 0,
                "durableMutation": false,
                "policy": "No cache files are deleted without explicit file-deletion approval.",
                "dryRun": self.options.dry_run,
            })),
        )
    }

    fn execute_graph_snapshot_prune(
        &self,
        budget: &mut JobBudgetState,
    ) -> (RunOutcome, Option<u64>, Option<String>, Option<JsonValue>) {
        let opened = match self.open_workspace_database_for_job(
            GRAPH_SNAPSHOT_PRUNE_JOB_SCHEMA_V1,
            "graph_snapshot_prune",
            "ee maintenance graph-snapshot-prune --workspace . --dry-run --json",
        ) {
            Ok(opened) => opened,
            Err(result) => return result,
        };
        let graph_type = GraphSnapshotType::MemoryLinks;
        let lock_owner = if self.options.dry_run {
            None
        } else {
            match acquire_graph_snapshot_prune_lock_owner(
                &opened.connection,
                &opened.workspace_id,
                graph_type,
            ) {
                Ok(owner) => Some(owner),
                Err(error) => {
                    return steward_job_failure(
                        GRAPH_SNAPSHOT_PRUNE_JOB_SCHEMA_V1,
                        error.code(),
                        error.message(),
                        false,
                        Some(&opened.database_path),
                        "Retry after the current graph snapshot refresh or prune operation exits.",
                    );
                }
            }
        };
        let limit = match self.options.item_limit {
            Some(limit) => match u32::try_from(limit) {
                Ok(limit) => limit,
                Err(_) => {
                    return steward_job_failure(
                        GRAPH_SNAPSHOT_PRUNE_JOB_SCHEMA_V1,
                        "graph_snapshot_prune_limit_too_large",
                        "Graph snapshot prune item limit exceeds u32".to_owned(),
                        self.options.dry_run,
                        Some(&opened.database_path),
                        "Use --item-limit with a value at or below 4294967295.",
                    );
                }
            },
            None => GRAPH_SNAPSHOT_PRUNE_DEFAULT_LIMIT,
        };
        let retention_days = GRAPH_SNAPSHOT_PRUNE_RETENTION_DAYS;
        let cutoff_timestamp = (Utc::now() - ChronoDuration::days(retention_days))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let candidates = match opened
            .connection
            .list_archived_graph_snapshot_prune_candidates(
                &opened.workspace_id,
                graph_type,
                &cutoff_timestamp,
                limit,
            ) {
            Ok(candidates) => candidates,
            Err(error) => {
                let message = format!("Failed to list graph snapshot prune candidates: {error}");
                return steward_job_failure(
                    GRAPH_SNAPSHOT_PRUNE_JOB_SCHEMA_V1,
                    "graph_snapshot_prune_candidate_query_failed",
                    message,
                    self.options.dry_run,
                    Some(&opened.database_path),
                    "ee doctor --json",
                );
            }
        };
        let candidate_count = usize_to_u64(candidates.len());
        budget.record(ResourceType::Items, candidate_count);
        if budget_cancels_before_mutation(budget) {
            return (
                RunOutcome::Cancelled,
                Some(candidate_count),
                Some("Budget exceeded before graph snapshot pruning".to_owned()),
                Some(Self::graph_snapshot_prune_details(
                    GraphSnapshotPruneDetailsInput {
                        workspace_id: &opened.workspace_id,
                        graph_type,
                        dry_run: self.options.dry_run,
                        retention_days,
                        cutoff_timestamp: &cutoff_timestamp,
                        candidates: &candidates,
                        pruned_count: 0,
                        pruned_bytes: 0,
                        lock_acquired: lock_owner.is_some(),
                        lock_holder_id: lock_owner.as_ref().map(|owner| owner.holder_id.as_str()),
                    },
                )),
            );
        }
        let pruned_count = if self.options.dry_run || candidates.is_empty() {
            0
        } else {
            match opened.connection.prune_archived_graph_snapshots(
                &opened.workspace_id,
                graph_type,
                &cutoff_timestamp,
                limit,
            ) {
                Ok(count) => count,
                Err(error) => {
                    let message = format!("Failed to prune archived graph snapshots: {error}");
                    return steward_job_failure(
                        GRAPH_SNAPSHOT_PRUNE_JOB_SCHEMA_V1,
                        "graph_snapshot_prune_delete_failed",
                        message,
                        false,
                        Some(&opened.database_path),
                        "ee doctor --json",
                    );
                }
            }
        };
        let pruned_bytes = candidates
            .iter()
            .take(usize::try_from(pruned_count).unwrap_or(usize::MAX))
            .fold(0_u64, |total, candidate| {
                total.saturating_add(candidate.metrics_bytes)
            });

        (
            RunOutcome::Success,
            Some(candidate_count),
            None,
            Some(Self::graph_snapshot_prune_details(
                GraphSnapshotPruneDetailsInput {
                    workspace_id: &opened.workspace_id,
                    graph_type,
                    dry_run: self.options.dry_run,
                    retention_days,
                    cutoff_timestamp: &cutoff_timestamp,
                    candidates: &candidates,
                    pruned_count,
                    pruned_bytes,
                    lock_acquired: lock_owner.is_some(),
                    lock_holder_id: lock_owner.as_ref().map(|owner| owner.holder_id.as_str()),
                },
            )),
        )
    }

    fn graph_snapshot_prune_details(input: GraphSnapshotPruneDetailsInput<'_>) -> JsonValue {
        let candidate_bytes = input.candidates.iter().fold(0_u64, |total, candidate| {
            total.saturating_add(candidate.metrics_bytes)
        });
        let candidates_json = input
            .candidates
            .iter()
            .map(|candidate| {
                json!({
                    "snapshotId": candidate.snapshot.id.as_str(),
                    "snapshotVersion": candidate.snapshot.snapshot_version,
                    "createdAt": candidate.snapshot.created_at.as_str(),
                    "metricsBytes": candidate.metrics_bytes,
                    "contentHash": candidate.snapshot.content_hash.as_str(),
                })
            })
            .collect::<Vec<_>>();
        let lock = json!({
            "resourceType": "graph_snapshot_prune",
            "resourceId": format!("{}:{}", input.workspace_id, input.graph_type.as_str()),
            "ttlSeconds": GRAPH_SNAPSHOT_PRUNE_LOCK_TTL_SECS,
            "acquired": input.lock_acquired,
            "holderId": input.lock_holder_id,
        });
        json!({
            "schema": GRAPH_SNAPSHOT_PRUNE_JOB_SCHEMA_V1,
            "command": "maintenance graph-snapshot-prune",
            "workspaceId": input.workspace_id,
            "graphType": input.graph_type.as_str(),
            "dryRun": input.dry_run,
            "retentionDays": input.retention_days,
            "cutoffTimestamp": input.cutoff_timestamp,
            "candidateCount": input.candidates.len(),
            "prunedCount": input.pruned_count,
            "candidateBytes": candidate_bytes,
            "prunedBytes": input.pruned_bytes,
            "oldestRetainedAt": null,
            "candidates": candidates_json,
            "lock": lock,
            "degraded": [],
        })
    }

    fn execute_storage_compact(
        &self,
        budget: &mut JobBudgetState,
    ) -> (RunOutcome, Option<u64>, Option<String>, Option<JsonValue>) {
        let opened = match self.open_workspace_database_for_job(
            "ee.steward.storage_compact.error.v1",
            "storage_compact",
            "ee doctor --json",
        ) {
            Ok(opened) => opened,
            Err(result) => return result,
        };
        let workspaces = match opened.connection.list_workspaces() {
            Ok(workspaces) => workspaces,
            Err(error) => {
                let message = format!("Failed to inspect database before storage compact: {error}");
                return steward_job_failure(
                    "ee.steward.storage_compact.error.v1",
                    "storage_compact_preflight_failed",
                    message,
                    self.options.dry_run,
                    Some(&opened.database_path),
                    "ee doctor --json",
                );
            }
        };
        budget.record(ResourceType::Items, usize_to_u64(workspaces.len()));

        (
            RunOutcome::Success,
            Some(usize_to_u64(workspaces.len())),
            None,
            Some(json!({
                "schema": "ee.steward.storage_compact.v1",
                "jobType": JobType::StorageCompact.as_str(),
                "workspaceId": opened.workspace_id,
                "databasePath": opened.database_path.display().to_string(),
                "operation": "preflight_only",
                "durableMutation": false,
                "reason": "Storage compaction is held to read-only diagnostics until the DB layer exposes an audited vacuum/optimize operation.",
                "dryRun": self.options.dry_run,
            })),
        )
    }

    fn execute_integrity_check(
        &self,
        budget: &mut JobBudgetState,
    ) -> (RunOutcome, Option<u64>, Option<String>, Option<JsonValue>) {
        let opened = match self.open_workspace_database_for_job(
            "ee.steward.integrity_check.error.v1",
            "integrity_check",
            "ee doctor --json",
        ) {
            Ok(opened) => opened,
            Err(result) => return result,
        };
        let memories = match opened
            .connection
            .list_memories(&opened.workspace_id, None, false)
        {
            Ok(memories) => memories,
            Err(error) => {
                let message = format!("Failed to inspect memories for integrity check: {error}");
                return steward_job_failure(
                    "ee.steward.integrity_check.error.v1",
                    "integrity_check_memory_query_failed",
                    message,
                    self.options.dry_run,
                    Some(&opened.database_path),
                    "ee doctor --json",
                );
            }
        };
        budget.record(ResourceType::Items, usize_to_u64(memories.len()));

        (
            RunOutcome::Success,
            Some(usize_to_u64(memories.len())),
            None,
            Some(json!({
                "schema": "ee.steward.integrity_check.v1",
                "jobType": JobType::IntegrityCheck.as_str(),
                "workspaceId": opened.workspace_id,
                "databasePath": opened.database_path.display().to_string(),
                "checkedMemories": memories.len(),
                "issues": [],
                "dryRun": self.options.dry_run,
                "durableMutation": false,
            })),
        )
    }

    fn execute_backup_export(
        &self,
        budget: &mut JobBudgetState,
    ) -> (RunOutcome, Option<u64>, Option<String>, Option<JsonValue>) {
        let Some(database_path) = self.resolve_database_path() else {
            let message = "Backup export requires a database path or workspace path with .ee/ee.db"
                .to_owned();
            return steward_job_failure(
                "ee.steward.backup_export.error.v1",
                "backup_export_database_unresolved",
                message,
                self.options.dry_run,
                None,
                "ee init --workspace .",
            );
        };
        let database_exists = if database_path.exists() { 1_u64 } else { 0 };
        budget.record(ResourceType::Items, database_exists);
        (
            RunOutcome::Success,
            Some(database_exists),
            None,
            Some(json!({
                "schema": "ee.steward.backup_export.v1",
                "jobType": JobType::BackupExport.as_str(),
                "databasePath": database_path.display().to_string(),
                "databaseExists": database_exists == 1,
                "operation": "planned",
                "durableMutation": false,
                "reason": "Backup export is exposed through dedicated backup commands; daemon records readiness without copying files.",
                "dryRun": self.options.dry_run,
            })),
        )
    }

    fn execute_garbage_collection(
        &self,
        budget: &mut JobBudgetState,
    ) -> (RunOutcome, Option<u64>, Option<String>, Option<JsonValue>) {
        budget.record(ResourceType::Items, 0);
        (
            RunOutcome::Success,
            Some(0),
            None,
            Some(json!({
                "schema": "ee.steward.garbage_collection.v1",
                "jobType": JobType::GarbageCollection.as_str(),
                "itemsCollected": 0,
                "durableMutation": false,
                "policy": "Garbage collection never deletes files or rows without an audited subsystem-specific operation.",
                "dryRun": self.options.dry_run,
            })),
        )
    }

    fn execute_custom_job(&self) -> (RunOutcome, Option<u64>, Option<String>, Option<JsonValue>) {
        (
            RunOutcome::Skipped,
            Some(0),
            Some("Custom steward jobs require an explicit extension handler.".to_owned()),
            Some(json!({
                "schema": "ee.steward.custom_job.v1",
                "jobType": JobType::Custom.as_str(),
                "outcome": "skipped",
                "durableMutation": false,
            })),
        )
    }

    fn resolve_database_path(&self) -> Option<PathBuf> {
        self.options.database_path.clone().or_else(|| {
            self.options.workspace_path.as_ref().map(|workspace| {
                normalize_runner_workspace_path(workspace)
                    .join(".ee")
                    .join("ee.db")
            })
        })
    }

    fn resolve_workspace_id(&self, connection: &DbConnection) -> Result<String, String> {
        if let Some(workspace_id) = self.options.workspace_id.as_ref() {
            return Ok(workspace_id.clone());
        }

        if let Some(workspace_path) = self.options.workspace_path.as_ref() {
            let workspace_path = normalize_runner_workspace_path(workspace_path);
            let path = workspace_path.to_string_lossy().into_owned();
            let workspace = connection
                .get_workspace_by_path(&path)
                .map_err(|error| format!("Failed to query workspace path {path}: {error}"))?;
            if let Some(workspace) = workspace {
                return Ok(workspace.id);
            }
            return Ok(stable_runner_workspace_id(&workspace_path));
        }

        let workspaces = connection
            .list_workspaces()
            .map_err(|error| format!("Failed to list workspaces: {error}"))?;
        if let [workspace] = workspaces.as_slice() {
            return Ok(workspace.id.clone());
        }

        Err("Could not resolve a unique workspace row for decay sweep".to_owned())
    }

    fn graph_link_limit(&self) -> Result<Option<u32>, String> {
        match self.options.item_limit {
            Some(limit) => u32::try_from(limit)
                .map(Some)
                .map_err(|_| "Graph centrality link limit exceeds u32".to_owned()),
            None => Ok(None),
        }
    }

    fn normalized_workspace_path(&self) -> PathBuf {
        self.options.workspace_path.as_ref().map_or_else(
            || normalize_runner_workspace_path(Path::new(".")),
            |workspace| normalize_runner_workspace_path(workspace),
        )
    }

    fn open_workspace_database_for_job(
        &self,
        schema: &'static str,
        code_prefix: &'static str,
        repair: &'static str,
    ) -> Result<OpenedWorkspaceDatabase, JobWorkResult> {
        let Some(database_path) = self.resolve_database_path() else {
            let message = format!(
                "{} requires a database path or workspace path with .ee/ee.db",
                code_prefix
            );
            return Err(steward_job_failure(
                schema,
                &format!("{code_prefix}_database_unresolved"),
                message,
                self.options.dry_run,
                None,
                repair,
            ));
        };
        if self.options.dry_run && !database_path.exists() {
            let message = format!(
                "Dry-run {} database does not exist: {}",
                code_prefix,
                database_path.display()
            );
            return Err(steward_job_failure(
                schema,
                &format!("{code_prefix}_database_missing"),
                message,
                true,
                Some(&database_path),
                "ee init --workspace .",
            ));
        }

        let connection = match if self.options.dry_run {
            DbConnection::open_schema_only(&database_path)
        } else {
            DbConnection::open_file(&database_path)
        } {
            Ok(connection) => connection,
            Err(error) => {
                let message = format!(
                    "Failed to open {} database {}: {error}",
                    code_prefix,
                    database_path.display()
                );
                return Err(steward_job_failure(
                    schema,
                    &format!("{code_prefix}_database_open_failed"),
                    message,
                    self.options.dry_run,
                    Some(&database_path),
                    "ee init --workspace .",
                ));
            }
        };
        if !self.options.dry_run {
            if let Err(error) = connection.migrate() {
                let message = format!("Failed to migrate {code_prefix} database: {error}");
                return Err(steward_job_failure(
                    schema,
                    &format!("{code_prefix}_migration_failed"),
                    message,
                    false,
                    Some(&database_path),
                    "ee doctor --json",
                ));
            }
        }

        let workspace_id = match self.resolve_workspace_id(&connection) {
            Ok(workspace_id) => workspace_id,
            Err(message) => {
                return Err(steward_job_failure(
                    schema,
                    &format!("{code_prefix}_workspace_unresolved"),
                    message,
                    self.options.dry_run,
                    Some(&database_path),
                    repair,
                ));
            }
        };

        Ok(OpenedWorkspaceDatabase {
            connection,
            database_path,
            workspace_id,
        })
    }

    /// Run all pending jobs in priority order.
    pub fn run_pending(&mut self) -> RunnerReport {
        let started_at = chrono::Utc::now().to_rfc3339();
        let mut results = Vec::new();
        let mut succeeded = 0u32;
        let mut failed = 0u32;
        let mut skipped = 0u32;
        let mut was_cancelled = false;

        let pending_ids: Vec<String> = self
            .ledger
            .pending_by_priority()
            .iter()
            .map(|j| j.id.clone())
            .collect();

        for job_id in pending_ids {
            let now = chrono::Utc::now().to_rfc3339();
            if let Some(result) = self.run_job(&job_id, &now) {
                match result.outcome {
                    RunOutcome::Success => succeeded += 1,
                    RunOutcome::Failed | RunOutcome::TimedOut => failed += 1,
                    RunOutcome::Skipped => skipped += 1,
                    RunOutcome::Cancelled => {
                        was_cancelled = true;
                        failed += 1;
                    }
                }

                let should_stop = result.outcome == RunOutcome::Cancelled
                    || (result.outcome == RunOutcome::Failed && !self.options.continue_on_error);

                results.push(result);

                if should_stop {
                    break;
                }
            }
        }

        let completed_at = chrono::Utc::now().to_rfc3339();
        let total_duration_ms: u64 = results.iter().map(|r| r.duration_ms).sum();

        RunnerReport {
            results,
            total_duration_ms,
            succeeded,
            failed,
            skipped,
            was_cancelled,
            started_at,
            completed_at,
        }
    }

    /// Run a specific job type.
    pub fn run_job_type(&mut self, job_type: JobType, context: Option<String>) -> JobRunResult {
        let timestamp = chrono::Utc::now().to_rfc3339();
        let job_id = self.schedule(job_type, JobPriority::Normal, context);
        self.run_job(&job_id, &timestamp).unwrap_or(JobRunResult {
            job_id,
            job_type,
            outcome: RunOutcome::Failed,
            duration_ms: 0,
            items_processed: None,
            error: Some("Failed to execute job".to_owned()),
            budget_summary: None,
            details: None,
            dry_run: self.options.dry_run,
        })
    }
}

fn normalize_runner_workspace_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    absolute.canonicalize().unwrap_or(absolute)
}

fn stable_runner_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    crate::models::WorkspaceId::from_uuid(uuid::Uuid::from_bytes(blake3_uuid_bytes(&hash)))
        .to_string()
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn usize_to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn blake3_uuid_bytes(hash: &blake3::Hash) -> [u8; 16] {
    let mut bytes = [0_u8; 16];
    if let Some(prefix) = hash.as_bytes().get(..16) {
        bytes.copy_from_slice(prefix);
    }
    bytes
}

fn millis_to_u64(elapsed: std::time::Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

fn budget_cancels_before_mutation(budget: &JobBudgetState) -> bool {
    budget.should_cancel()
        || budget
            .budgets
            .iter()
            .any(|limit| limit.on_exceed == BudgetExceedAction::Cancel && limit.limit == 0)
}

fn budget_times_out_before_mutation(budget: &JobBudgetState) -> bool {
    budget.check_budgets().iter().any(|violation| {
        violation.resource == ResourceType::TimeMs && violation.action == BudgetExceedAction::Cancel
    }) || budget.budgets.iter().any(|limit| {
        limit.resource == ResourceType::TimeMs
            && limit.on_exceed == BudgetExceedAction::Cancel
            && limit.limit == 0
    })
}

struct OpenedWorkspaceDatabase {
    connection: DbConnection,
    database_path: PathBuf,
    workspace_id: String,
}

fn graph_snapshot_prune_holder_id() -> String {
    let nonce = Utc::now().timestamp_nanos_opt().map_or_else(
        || Utc::now().timestamp_micros().to_string(),
        |value| value.to_string(),
    );
    format!("ee-graph-snapshot-prune-{}-{nonce}", std::process::id())
}

fn graph_snapshot_prune_lock_id(
    workspace_id: &str,
    graph_type: GraphSnapshotType,
) -> AdvisoryLockId {
    AdvisoryLockId::new(
        "graph_snapshot_prune",
        format!("{workspace_id}:{}", graph_type.as_str()),
    )
}

fn graph_snapshot_refresh_lock_id(
    workspace_id: &str,
    graph_type: GraphSnapshotType,
) -> AdvisoryLockId {
    AdvisoryLockId::new(
        "graph_snapshot",
        format!("{workspace_id}:{}", graph_type.as_str()),
    )
}

fn acquire_graph_snapshot_prune_lock_owner<'a>(
    conn: &'a DbConnection,
    workspace_id: &str,
    graph_type: GraphSnapshotType,
) -> Result<GraphSnapshotPruneLockOwner<'a>, GraphSnapshotPruneLockError> {
    if let Err(error) = conn.ensure_advisory_locks_table() {
        return Err(GraphSnapshotPruneLockError::Storage {
            resource_type: "graph_snapshot_prune".to_owned(),
            resource_id: format!("{workspace_id}:{}", graph_type.as_str()),
            message: error.to_string(),
        });
    }

    let holder_id = graph_snapshot_prune_holder_id();
    let mut owner = GraphSnapshotPruneLockOwner {
        conn,
        lock_ids: Vec::new(),
        holder_id,
        ttl_secs: GRAPH_SNAPSHOT_PRUNE_LOCK_TTL_SECS,
    };

    for lock_id in [
        graph_snapshot_prune_lock_id(workspace_id, graph_type),
        graph_snapshot_refresh_lock_id(workspace_id, graph_type),
    ] {
        match conn.acquire_advisory_lock(
            &lock_id,
            &owner.holder_id,
            Some(GRAPH_SNAPSHOT_PRUNE_LOCK_TTL_SECS),
            Some(GRAPH_SNAPSHOT_PRUNE_LOCK_REASON),
        ) {
            Ok(AcquireLockResult::Acquired(_)) | Ok(AcquireLockResult::Expired { .. }) => {
                owner.lock_ids.push(lock_id);
            }
            Ok(AcquireLockResult::AlreadyHeld {
                holder_id,
                acquired_at,
            }) => {
                return Err(GraphSnapshotPruneLockError::Busy {
                    resource_type: lock_id.resource_type().to_owned(),
                    resource_id: lock_id.resource_id().to_owned(),
                    holder_id,
                    acquired_at,
                });
            }
            Err(error) => {
                return Err(GraphSnapshotPruneLockError::Storage {
                    resource_type: lock_id.resource_type().to_owned(),
                    resource_id: lock_id.resource_id().to_owned(),
                    message: error.to_string(),
                });
            }
        }
    }

    Ok(owner)
}

#[derive(Clone, Debug)]
struct ConsolidationCandidatePlan {
    candidate_id: String,
    source_memory_id: String,
    target_memory_id: String,
    proposed_content: String,
    proposed_confidence: f32,
    reason: String,
}

fn steward_job_failure(
    schema: &'static str,
    code: &str,
    message: String,
    dry_run: bool,
    database_path: Option<&Path>,
    repair: &'static str,
) -> (RunOutcome, Option<u64>, Option<String>, Option<JsonValue>) {
    (
        RunOutcome::Failed,
        None,
        Some(message.clone()),
        Some(json!({
            "schema": schema,
            "code": code,
            "databasePath": database_path.map(|path| path.display().to_string()),
            "dryRun": dry_run,
            "durableMutation": false,
            "message": message,
            "repair": repair,
        })),
    )
}

fn index_rebuild_job_details(
    database_path: &Path,
    preflight: &crate::core::index::IndexRebuildReport,
    report: Option<&crate::core::index::IndexRebuildReport>,
    dry_run: bool,
    durable_mutation: bool,
) -> JsonValue {
    json!({
        "schema": "ee.steward.index_rebuild.v1",
        "jobType": JobType::IndexRebuild.as_str(),
        "databasePath": database_path.display().to_string(),
        "preflight": preflight.data_json(),
        "result": report.map(crate::core::index::IndexRebuildReport::data_json),
        "dryRun": dry_run,
        "durableMutation": durable_mutation,
    })
}

fn index_coalesce_job_details(
    preflight: &crate::core::index::IndexProcessingReport,
    report: Option<&crate::core::index::IndexProcessingReport>,
    dry_run: bool,
    durable_mutation: bool,
) -> JsonValue {
    json!({
        "schema": "ee.steward.index_coalesce.v1",
        "jobType": JobType::IndexCoalesce.as_str(),
        "preflight": preflight.data_json(),
        "result": report.map(crate::core::index::IndexProcessingReport::data_json),
        "dryRun": dry_run,
        "durableMutation": durable_mutation,
    })
}

fn normalize_memory_content_for_consolidation(content: &str) -> String {
    content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_ascii_lowercase()
}

fn plan_consolidation_candidates(
    workspace_id: &str,
    memories: &[StoredMemory],
) -> Vec<ConsolidationCandidatePlan> {
    let mut grouped = BTreeMap::<(String, String, String), Vec<&StoredMemory>>::new();
    for memory in memories {
        let normalized = normalize_memory_content_for_consolidation(&memory.content);
        if normalized.is_empty() {
            continue;
        }
        grouped
            .entry((memory.level.clone(), memory.kind.clone(), normalized))
            .or_default()
            .push(memory);
    }

    let mut plans = Vec::new();
    for ((level, kind, normalized), group) in grouped {
        if group.len() < 2 {
            continue;
        }
        let Some(source) = group.first().copied() else {
            continue;
        };
        for target in group.iter().skip(1) {
            let candidate_id =
                stable_consolidation_candidate_id(workspace_id, &source.id, &target.id);
            plans.push(ConsolidationCandidatePlan {
                candidate_id,
                source_memory_id: source.id.clone(),
                target_memory_id: target.id.clone(),
                proposed_content: source.content.clone(),
                proposed_confidence: source.confidence.max(target.confidence),
                reason: format!(
                    "Duplicate {level}/{kind} memory content normalized to {:?}; consolidate {} into {}.",
                    normalized, target.id, source.id
                ),
            });
        }
    }
    plans
}

fn stable_consolidation_candidate_id(
    workspace_id: &str,
    source_memory_id: &str,
    target_memory_id: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"steward.consolidation.v1\0");
    hasher.update(workspace_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(source_memory_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(target_memory_id.as_bytes());
    let hash = hasher.finalize();
    let candidate =
        crate::models::CandidateId::from_uuid(uuid::Uuid::from_bytes(blake3_uuid_bytes(&hash)));
    format!(
        "curate_{}",
        candidate.to_string().trim_start_matches("cand_")
    )
}

fn consolidation_pass_details(
    opened: &OpenedWorkspaceDatabase,
    plan: &[ConsolidationCandidatePlan],
    inserted: u64,
    already_pending: u64,
    dry_run: bool,
    durable_mutation: bool,
) -> JsonValue {
    json!({
        "schema": "ee.steward.consolidation_pass.v1",
        "jobType": JobType::ConsolidationPass.as_str(),
        "workspaceId": opened.workspace_id,
        "databasePath": opened.database_path.display().to_string(),
        "plannedCandidates": plan.len(),
        "insertedCandidates": inserted,
        "alreadyPendingCandidates": already_pending,
        "candidateIds": plan
            .iter()
            .map(|candidate| candidate.candidate_id.as_str())
            .collect::<Vec<_>>(),
        "dryRun": dry_run,
        "durableMutation": durable_mutation,
    })
}

fn graph_centrality_failure(
    code: &'static str,
    message: String,
    dry_run: bool,
    database_path: Option<&Path>,
) -> (RunOutcome, Option<u64>, Option<String>, Option<JsonValue>) {
    (
        RunOutcome::Failed,
        None,
        Some(message.clone()),
        Some(graph_centrality_error_details(
            code,
            message,
            dry_run,
            database_path,
        )),
    )
}

fn graph_centrality_error_details(
    code: &'static str,
    message: String,
    dry_run: bool,
    database_path: Option<&Path>,
) -> JsonValue {
    let mut details = json!({
        "schema": GRAPH_CENTRALITY_ERROR_SCHEMA_V1,
        "code": code,
        "command": "steward graph-centrality-refresh",
        "dryRun": dry_run,
        "durableMutation": false,
        "message": message,
        "repair": "ee init --workspace . && ee daemon --foreground --once --job centrality_refresh --dry-run --json",
    });
    if let (Some(object), Some(path)) = (details.as_object_mut(), database_path) {
        object.insert(
            "databasePath".to_owned(),
            JsonValue::String(path.display().to_string()),
        );
    }
    details
}

fn graph_centrality_job_details(
    workspace_id: &str,
    preflight: &crate::graph::GraphRefreshJobReport,
    result: Option<&crate::graph::GraphRefreshJobReport>,
    dry_run: bool,
    durable_mutation: bool,
) -> JsonValue {
    json!({
        "schema": GRAPH_CENTRALITY_JOB_SCHEMA_V1,
        "command": "steward graph-centrality-refresh",
        "workspaceId": workspace_id,
        "dryRun": dry_run,
        "durableMutation": durable_mutation,
        "sideEffectClass": if durable_mutation { "derived_graph_snapshot" } else { "report_only" },
        "preflight": preflight.data_json(),
        "result": result.map(crate::graph::GraphRefreshJobReport::data_json),
    })
}

// ============================================================================
// EE-207: Foreground Daemon Mode
// ============================================================================

/// Schema identifier for foreground daemon reports.
pub const DAEMON_FOREGROUND_SCHEMA_V1: &str = "ee.steward.daemon_foreground.v1";

/// Default number of daemon ticks for bounded foreground runs.
pub const DEFAULT_DAEMON_FOREGROUND_TICK_LIMIT: u32 = 1;

/// Default delay between foreground daemon ticks.
pub const DEFAULT_DAEMON_FOREGROUND_INTERVAL_MS: u64 = 1_000;

/// Maximum number of reports retained by a bounded foreground daemon run.
pub const MAX_DAEMON_FOREGROUND_TICK_LIMIT: u32 = 1_000;

/// Maximum delay between foreground daemon ticks.
pub const MAX_DAEMON_FOREGROUND_INTERVAL_MS: u64 = 60_000;

const DAEMON_FOREGROUND_SLEEP_SLICE_MS: u64 = 250;

/// Options for running the optional daemon in the foreground.
#[derive(Clone, Debug)]
pub struct DaemonForegroundOptions {
    pub workspace: String,
    pub tick_limit: u32,
    pub interval_ms: u64,
    pub dry_run: bool,
    pub job_types: Vec<JobType>,
    pub runner_options: RunnerOptions,
}

impl DaemonForegroundOptions {
    #[must_use]
    pub fn new(workspace: impl Into<String>) -> Self {
        Self {
            workspace: workspace.into(),
            tick_limit: DEFAULT_DAEMON_FOREGROUND_TICK_LIMIT,
            interval_ms: DEFAULT_DAEMON_FOREGROUND_INTERVAL_MS,
            dry_run: false,
            job_types: vec![JobType::DecaySweep],
            runner_options: RunnerOptions::new(),
        }
    }
}

/// One foreground daemon scheduler tick.
#[derive(Clone, Debug)]
pub struct DaemonForegroundTick {
    pub tick: u32,
    pub started_at: String,
    pub completed_at: String,
    pub report: RunnerReport,
}

impl DaemonForegroundTick {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "tick": self.tick,
            "startedAt": self.started_at,
            "completedAt": self.completed_at,
            "runner": self.report.data_json(),
        })
    }
}

/// Report from a bounded foreground daemon run.
#[derive(Clone, Debug)]
pub struct DaemonForegroundReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub mode: &'static str,
    pub workspace: String,
    pub daemonized: bool,
    pub supervisor: &'static str,
    pub started_at: String,
    pub completed_at: String,
    pub tick_limit: u32,
    pub interval_ms: u64,
    pub dry_run: bool,
    pub job_types: Vec<JobType>,
    pub ticks: Vec<DaemonForegroundTick>,
}

impl DaemonForegroundReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "command": self.command,
            "mode": self.mode,
            "workspace": self.workspace,
            "daemonized": self.daemonized,
            "supervisor": self.supervisor,
            "startedAt": self.started_at,
            "completedAt": self.completed_at,
            "requestedTickLimit": self.tick_limit,
            "intervalMs": self.interval_ms,
            "dryRun": self.dry_run,
            "jobTypes": self
                .job_types
                .iter()
                .map(|job_type| job_type.as_str())
                .collect::<Vec<_>>(),
            "summary": {
                "tickCount": self.ticks.len(),
                "jobsRun": self.jobs_run(),
                "succeeded": self.succeeded_count(),
                "failed": self.failed_count(),
                "skipped": self.skipped_count(),
                "wasCancelled": self.was_cancelled(),
            },
            "ticks": self
                .ticks
                .iter()
                .map(DaemonForegroundTick::data_json)
                .collect::<Vec<_>>(),
            "capabilityGap": {
                "code": "daemon_background_mode_unimplemented",
                "capabilitiesCommand": "ee capabilities --json"
            },
            "degraded": [],
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = String::new();
        output.push_str("ee daemon foreground\n");
        output.push_str("====================\n\n");
        output.push_str(&format!("Workspace:  {}\n", self.workspace));
        output.push_str(&format!("Started:    {}\n", self.started_at));
        output.push_str(&format!("Completed:  {}\n", self.completed_at));
        output.push_str(&format!("Ticks:      {}\n", self.ticks.len()));
        output.push_str(&format!("Jobs run:   {}\n", self.jobs_run()));
        output.push_str(&format!("Succeeded:  {}\n", self.succeeded_count()));
        output.push_str(&format!("Failed:     {}\n", self.failed_count()));
        output.push_str(&format!("Skipped:    {}\n", self.skipped_count()));
        output.push_str(&format!("Dry run:    {}\n", self.dry_run));
        output.push_str("\nMode:\n  Foreground, bounded, current process.\n");
        output.push_str("\nNext:\n  ee daemon --foreground --once --json\n");
        output
    }

    #[must_use]
    pub fn jobs_run(&self) -> usize {
        self.ticks
            .iter()
            .map(|tick| tick.report.results.len())
            .sum()
    }

    #[must_use]
    pub fn succeeded_count(&self) -> u32 {
        self.ticks.iter().map(|tick| tick.report.succeeded).sum()
    }

    #[must_use]
    pub fn failed_count(&self) -> u32 {
        self.ticks.iter().map(|tick| tick.report.failed).sum()
    }

    #[must_use]
    pub fn skipped_count(&self) -> u32 {
        self.ticks.iter().map(|tick| tick.report.skipped).sum()
    }

    #[must_use]
    pub fn was_cancelled(&self) -> bool {
        self.ticks.iter().any(|tick| tick.report.was_cancelled)
    }
}

/// Run a bounded foreground daemon loop in the current process through the
/// production Asupersync runtime.
pub fn run_daemon_foreground(
    options: &DaemonForegroundOptions,
) -> Result<DaemonForegroundReport, String> {
    let runtime = crate::core::build_cli_runtime()
        .map_err(|error| format!("Failed to build Asupersync daemon runtime: {error}"))?;
    let task_options = options.clone();
    let join = runtime
        .handle()
        .try_spawn(async move {
            let Some(cx) = Cx::current() else {
                return Outcome::Err(
                    "Asupersync daemon task started without an ambient Cx".to_owned(),
                );
            };
            run_daemon_foreground_supervised(&cx, &task_options).await
        })
        .map_err(|error| format!("Failed to spawn Asupersync daemon supervisor: {error}"))?;
    match runtime.block_on(join) {
        Outcome::Ok(report) => Ok(report),
        Outcome::Err(message) => Err(message),
        Outcome::Cancelled(reason) => Err(format!(
            "Daemon supervisor cancelled: {}",
            crate::core::outcome::cancel_message(&reason)
        )),
        Outcome::Panicked(payload) => Err(format!("Daemon supervisor panicked: {payload}")),
    }
}

/// Run a bounded foreground daemon loop under an explicit Asupersync context.
///
/// The loop is intentionally foreground-only, but each tick is checkpointed so
/// LabRuntime and production cancellation propagate before scheduling and
/// before every durable maintenance handler.
pub async fn run_daemon_foreground_supervised(
    cx: &Cx,
    options: &DaemonForegroundOptions,
) -> Outcome<DaemonForegroundReport, String> {
    if let Err(message) = validate_daemon_foreground_options(options) {
        return Outcome::Err(message);
    }

    if let Some(cancelled) = daemon_checkpoint(cx) {
        return cancelled;
    }
    let tick_capacity = match usize::try_from(options.tick_limit) {
        Ok(capacity) => capacity,
        Err(_) => {
            return Outcome::Err(
                "Daemon foreground tick limit does not fit this platform".to_owned(),
            );
        }
    };
    let started_at = chrono::Utc::now().to_rfc3339();
    let mut ticks = Vec::with_capacity(tick_capacity);

    for tick in 1..=options.tick_limit {
        if let Some(cancelled) = daemon_checkpoint(cx) {
            return cancelled;
        }
        let tick_started_at = chrono::Utc::now().to_rfc3339();
        let mut runner_options = options.runner_options.clone();
        runner_options.dry_run = options.dry_run;
        if runner_options.workspace_path.is_none() {
            runner_options.workspace_path = Some(PathBuf::from(&options.workspace));
        }
        let mut runner = ManualRunner::new(runner_options);

        for job_type in &options.job_types {
            runner.schedule(
                *job_type,
                JobPriority::Normal,
                Some(format!("daemon foreground tick {tick}")),
            );
        }

        if let Some(cancelled) = daemon_checkpoint(cx) {
            return cancelled;
        }
        let report = runner.run_pending();
        ticks.push(DaemonForegroundTick {
            tick,
            started_at: tick_started_at,
            completed_at: report.completed_at.clone(),
            report,
        });

        if tick < options.tick_limit {
            if let Some(cancelled) = sleep_daemon_foreground_interval(cx, options.interval_ms).await
            {
                return cancelled;
            }
        }
    }

    Outcome::Ok(DaemonForegroundReport {
        schema: DAEMON_FOREGROUND_SCHEMA_V1,
        command: "daemon",
        mode: "foreground",
        workspace: options.workspace.clone(),
        daemonized: false,
        supervisor: "asupersync_foreground",
        started_at,
        completed_at: chrono::Utc::now().to_rfc3339(),
        tick_limit: options.tick_limit,
        interval_ms: options.interval_ms,
        dry_run: options.dry_run,
        job_types: options.job_types.clone(),
        ticks,
    })
}

fn daemon_checkpoint<T>(cx: &Cx) -> Option<Outcome<T, String>> {
    if cx.checkpoint().is_ok() {
        return None;
    }
    Some(Outcome::Cancelled(
        cx.cancel_reason()
            .unwrap_or_else(CancelReason::parent_cancelled),
    ))
}

fn validate_daemon_foreground_options(options: &DaemonForegroundOptions) -> Result<(), String> {
    if options.workspace.trim().is_empty() {
        return Err("Daemon workspace must not be empty".to_owned());
    }
    if options.tick_limit == 0 {
        return Err("Daemon foreground tick limit must be at least one".to_owned());
    }
    if options.tick_limit > MAX_DAEMON_FOREGROUND_TICK_LIMIT {
        return Err(format!(
            "Daemon foreground tick limit must be no greater than {MAX_DAEMON_FOREGROUND_TICK_LIMIT}"
        ));
    }
    if options.interval_ms > MAX_DAEMON_FOREGROUND_INTERVAL_MS {
        return Err(format!(
            "Daemon foreground interval must be no greater than {MAX_DAEMON_FOREGROUND_INTERVAL_MS} ms"
        ));
    }
    if options.job_types.is_empty() {
        return Err("Daemon foreground mode requires at least one steward job type".to_owned());
    }
    Ok(())
}

async fn sleep_daemon_foreground_interval(
    cx: &Cx,
    interval_ms: u64,
) -> Option<Outcome<DaemonForegroundReport, String>> {
    if let Some(cancelled) = daemon_checkpoint(cx) {
        return Some(cancelled);
    }
    if interval_ms == 0 {
        yield_now().await;
        return daemon_checkpoint(cx);
    }

    let mut remaining_ms = interval_ms;
    while remaining_ms > 0 {
        if let Some(cancelled) = daemon_checkpoint(cx) {
            return Some(cancelled);
        }
        let slice_ms = remaining_ms.min(DAEMON_FOREGROUND_SLEEP_SLICE_MS);
        asupersync_sleep(cx.now(), Duration::from_millis(slice_ms)).await;
        remaining_ms = remaining_ms.saturating_sub(slice_ms);
    }

    daemon_checkpoint(cx)
}

// ============================================================================
// EE-244: Job Diagnostic Output
// ============================================================================

/// Schema identifier for job diagnostic reports.
pub const JOB_DIAGNOSTIC_SCHEMA_V1: &str = "ee.steward.job_diagnostic.v1";

/// Diagnostic severity level.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum DiagnosticSeverity {
    /// Informational observation.
    Info,
    /// Warning that may need attention.
    Warning,
    /// Error requiring action.
    Error,
}

impl DiagnosticSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

impl fmt::Display for DiagnosticSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single diagnostic observation about a job.
#[derive(Clone, Debug)]
pub struct JobDiagnostic {
    /// Diagnostic code for machine consumption.
    pub code: String,
    /// Severity level.
    pub severity: DiagnosticSeverity,
    /// Human-readable message.
    pub message: String,
    /// Suggested action to resolve (if applicable).
    pub suggestion: Option<String>,
    /// Related job ID (if specific to a job).
    pub job_id: Option<String>,
}

impl JobDiagnostic {
    /// Create a new diagnostic.
    #[must_use]
    pub fn new(
        code: impl Into<String>,
        severity: DiagnosticSeverity,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            severity,
            message: message.into(),
            suggestion: None,
            job_id: None,
        }
    }

    /// Add a suggestion.
    #[must_use]
    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }

    /// Associate with a job.
    #[must_use]
    pub fn for_job(mut self, job_id: impl Into<String>) -> Self {
        self.job_id = Some(job_id.into());
        self
    }

    /// Render as JSON value.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        let mut obj = json!({
            "code": self.code,
            "severity": self.severity.as_str(),
            "message": self.message,
        });
        if let Some(obj_map) = obj.as_object_mut() {
            if let Some(ref suggestion) = self.suggestion {
                obj_map.insert("suggestion".to_string(), json!(suggestion));
            }
            if let Some(ref job_id) = self.job_id {
                obj_map.insert("jobId".to_string(), json!(job_id));
            }
        }
        obj
    }
}

/// Diagnostic report for jobs in the ledger.
#[derive(Clone, Debug)]
pub struct JobDiagnosticReport {
    /// Schema identifier.
    pub schema: &'static str,
    /// List of diagnostics.
    pub diagnostics: Vec<JobDiagnostic>,
    /// Overall health status.
    pub health: HealthStatus,
    /// Summary statistics.
    pub summary: DiagnosticSummary,
}

/// Health status of the job system.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HealthStatus {
    /// All good, no issues.
    Healthy,
    /// Minor issues, mostly operational.
    Degraded,
    /// Significant issues requiring attention.
    Unhealthy,
}

impl HealthStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Unhealthy => "unhealthy",
        }
    }
}

impl fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Summary of diagnostic findings.
#[derive(Clone, Debug, Default)]
pub struct DiagnosticSummary {
    /// Number of info-level diagnostics.
    pub info_count: u32,
    /// Number of warnings.
    pub warning_count: u32,
    /// Number of errors.
    pub error_count: u32,
    /// Total jobs analyzed.
    pub jobs_analyzed: u32,
    /// Jobs with issues.
    pub jobs_with_issues: u32,
}

impl JobDiagnosticReport {
    /// Create a new diagnostic report.
    #[must_use]
    pub fn new(diagnostics: Vec<JobDiagnostic>) -> Self {
        let mut summary = DiagnosticSummary::default();
        let mut jobs_with_issues = std::collections::HashSet::new();

        for diag in &diagnostics {
            match diag.severity {
                DiagnosticSeverity::Info => summary.info_count += 1,
                DiagnosticSeverity::Warning => summary.warning_count += 1,
                DiagnosticSeverity::Error => summary.error_count += 1,
            }
            if let Some(ref job_id) = diag.job_id {
                if diag.severity != DiagnosticSeverity::Info {
                    jobs_with_issues.insert(job_id.clone());
                }
            }
        }
        summary.jobs_with_issues = usize_to_u32(jobs_with_issues.len());

        let health = if summary.error_count > 0 {
            HealthStatus::Unhealthy
        } else if summary.warning_count > 0 {
            HealthStatus::Degraded
        } else {
            HealthStatus::Healthy
        };

        Self {
            schema: JOB_DIAGNOSTIC_SCHEMA_V1,
            diagnostics,
            health,
            summary,
        }
    }

    /// Render as JSON.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "command": "steward diag",
            "health": self.health.as_str(),
            "summary": {
                "infoCount": self.summary.info_count,
                "warningCount": self.summary.warning_count,
                "errorCount": self.summary.error_count,
                "jobsAnalyzed": self.summary.jobs_analyzed,
                "jobsWithIssues": self.summary.jobs_with_issues,
            },
            "diagnostics": self.diagnostics.iter().map(JobDiagnostic::data_json).collect::<Vec<_>>(),
        })
    }

    /// Render as human-readable string.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(512);

        out.push_str("Job Diagnostics\n");
        out.push_str("===============\n\n");
        out.push_str(&format!("Health: {}\n\n", self.health));
        out.push_str("Summary:\n");
        out.push_str(&format!("  Info:     {}\n", self.summary.info_count));
        out.push_str(&format!("  Warnings: {}\n", self.summary.warning_count));
        out.push_str(&format!("  Errors:   {}\n\n", self.summary.error_count));

        if !self.diagnostics.is_empty() {
            out.push_str("Findings:\n");
            for diag in &self.diagnostics {
                let prefix = match diag.severity {
                    DiagnosticSeverity::Info => "  [INFO]",
                    DiagnosticSeverity::Warning => "  [WARN]",
                    DiagnosticSeverity::Error => "  [ERR!]",
                };
                out.push_str(&format!("{} {}: {}\n", prefix, diag.code, diag.message));
                if let Some(ref suggestion) = diag.suggestion {
                    out.push_str(&format!("         -> {suggestion}\n"));
                }
            }
        }

        out.push_str("\nNext:\n  ee doctor --fix-plan --json\n");
        out
    }
}

/// Generate diagnostics for a job ledger.
#[must_use]
pub fn diagnose_ledger(ledger: &JobLedger) -> JobDiagnosticReport {
    let mut diagnostics = Vec::new();
    let stats = ledger.statistics();

    // Check for stuck running jobs
    for job in ledger.list_by_status(JobStatus::Running) {
        diagnostics.push(
            JobDiagnostic::new(
                "STEWARD_JOB_RUNNING",
                DiagnosticSeverity::Warning,
                format!("Job {} is still running", job.id),
            )
            .with_suggestion("Check if the job is progressing or needs cancellation")
            .for_job(&job.id),
        );
    }

    // Check for failed jobs
    for job in ledger.list_by_status(JobStatus::Failed) {
        let msg = job.error.as_deref().unwrap_or("Unknown error");
        diagnostics.push(
            JobDiagnostic::new(
                "STEWARD_JOB_FAILED",
                DiagnosticSeverity::Error,
                format!("Job {} failed: {}", job.id, msg),
            )
            .with_suggestion("Review error, then inspect the next daemon tick with `ee daemon --once --dry-run --json`")
            .for_job(&job.id),
        );
    }

    // Check for high pending count
    if stats.pending > 10 {
        diagnostics.push(
            JobDiagnostic::new(
                "STEWARD_HIGH_PENDING",
                DiagnosticSeverity::Warning,
                format!(
                    "{} jobs pending - backlog may need attention",
                    stats.pending
                ),
            )
            .with_suggestion("Run `ee daemon --once --dry-run --json` to inspect pending jobs"),
        );
    }

    // Check for empty ledger
    if stats.total == 0 {
        diagnostics.push(JobDiagnostic::new(
            "STEWARD_LEDGER_EMPTY",
            DiagnosticSeverity::Info,
            "No jobs in ledger",
        ));
    }

    // Overall health observation
    let success_rate = if stats.total > 0 {
        (stats.completed as f64 / stats.total as f64) * 100.0
    } else {
        100.0
    };

    if success_rate < 80.0 && stats.total >= 5 {
        diagnostics.push(
            JobDiagnostic::new(
                "STEWARD_LOW_SUCCESS_RATE",
                DiagnosticSeverity::Warning,
                format!("Job success rate is {:.1}%", success_rate),
            )
            .with_suggestion("Investigate failed jobs to improve reliability"),
        );
    }

    let mut report = JobDiagnosticReport::new(diagnostics);
    report.summary.jobs_analyzed = stats.total;
    report
}

/// Generate diagnostics for a single job.
#[must_use]
pub fn diagnose_job(job: &Job) -> Vec<JobDiagnostic> {
    let mut diagnostics = Vec::new();

    match job.status {
        JobStatus::Failed => {
            let msg = job.error.as_deref().unwrap_or("Unknown error");
            diagnostics.push(
                JobDiagnostic::new(
                    "JOB_FAILED",
                    DiagnosticSeverity::Error,
                    format!("Job failed: {msg}"),
                )
                .for_job(&job.id),
            );
        }
        JobStatus::Running => {
            diagnostics.push(
                JobDiagnostic::new(
                    "JOB_RUNNING",
                    DiagnosticSeverity::Info,
                    "Job is currently running",
                )
                .for_job(&job.id),
            );
        }
        JobStatus::Cancelled => {
            diagnostics.push(
                JobDiagnostic::new(
                    "JOB_CANCELLED",
                    DiagnosticSeverity::Warning,
                    "Job was cancelled",
                )
                .for_job(&job.id),
            );
        }
        _ => {}
    }

    // Check for long duration
    if let Some(duration) = job.duration_ms {
        if duration > 60_000 {
            diagnostics.push(
                JobDiagnostic::new(
                    "JOB_SLOW",
                    DiagnosticSeverity::Info,
                    format!("Job took {}ms (over 1 minute)", duration),
                )
                .for_job(&job.id),
            );
        }
    }

    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        CreateFeedbackEventInput, CreateMemoryInput, CreateMemoryLinkInput, CreateWorkspaceInput,
        DbConnection, MemoryLinkRelation, MemoryLinkSource, audit_actions,
    };
    use asupersync::runtime::JoinError;
    use asupersync::{Budget, CancelReason, Cx, LabConfig, LabRuntime, Outcome};
    use std::sync::{Arc, Mutex as StdMutex};

    type TestResult = Result<(), String>;

    const SCORE_WORKSPACE_ID: &str = "wsp_scoredecay0000000000000000";
    const SCORE_MEMORY_A: &str = "mem_scoredecay0000000000000001";
    const SCORE_MEMORY_B: &str = "mem_scoredecay0000000000000002";

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[cfg(unix)]
    #[test]
    fn maintenance_job_lock_rejects_symlinked_path_components() -> TestResult {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let real_ee = tempdir.path().join("real-ee");
        fs::create_dir_all(&real_ee).map_err(|error| error.to_string())?;
        symlink(&real_ee, tempdir.path().join(".ee")).map_err(|error| error.to_string())?;

        let error = match try_acquire_maintenance_job_lock(tempdir.path(), "test-holder") {
            Ok(lock) => return Err(format!("symlinked .ee lock should fail, got {lock:?}")),
            Err(error) => error,
        };
        ensure(
            error.code(),
            "maintenance_job_lock_open_failed",
            "symlinked .ee error code",
        )?;
        ensure(
            error.message().contains("path traverses symbolic link"),
            true,
            "symlinked .ee error message",
        )?;
        ensure(
            real_ee.join("maintenance-job.lock").exists(),
            false,
            "lock must not be written through symlinked .ee",
        )?;

        let safe_workspace = tempdir.path().join("safe-workspace");
        let safe_ee = safe_workspace.join(".ee");
        fs::create_dir_all(&safe_ee).map_err(|error| error.to_string())?;
        let outside_lock = tempdir.path().join("outside-maintenance-job.lock");
        fs::write(&outside_lock, "").map_err(|error| error.to_string())?;
        symlink(&outside_lock, safe_ee.join("maintenance-job.lock"))
            .map_err(|error| error.to_string())?;

        let error = match try_acquire_maintenance_job_lock(&safe_workspace, "test-holder") {
            Ok(lock) => return Err(format!("symlinked lock target should fail, got {lock:?}")),
            Err(error) => error,
        };
        ensure(
            error.code(),
            "maintenance_job_lock_open_failed",
            "symlinked lock error code",
        )?;
        ensure(
            error.message().contains("path traverses symbolic link"),
            true,
            "symlinked lock error message",
        )?;
        ensure(
            fs::read_to_string(&outside_lock).map_err(|error| error.to_string())?,
            String::new(),
            "lock must not write through symlinked target",
        )
    }

    fn open_score_decay_db() -> Result<DbConnection, String> {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                SCORE_WORKSPACE_ID,
                &CreateWorkspaceInput {
                    path: "/tmp/ee-score-decay".to_owned(),
                    name: Some("score-decay".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        Ok(connection)
    }

    fn insert_score_memory(
        connection: &DbConnection,
        memory_id: &str,
        confidence: f32,
    ) -> Result<(), String> {
        connection
            .insert_memory(
                memory_id,
                &CreateMemoryInput {
                    workspace_id: SCORE_WORKSPACE_ID.to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: format!("score decay fixture {memory_id}"),
                    workflow_id: None,
                    confidence,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some("test://score-decay".to_owned()),
                    trust_class: "agent_validated".to_owned(),
                    trust_subclass: None,
                    tags: vec!["decay".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())
    }

    fn set_score_memory_timestamp(
        connection: &DbConnection,
        memory_id: &str,
        timestamp: &str,
    ) -> Result<(), String> {
        connection
            .execute_raw(&format!(
                "UPDATE memories SET created_at = '{timestamp}', updated_at = '{timestamp}' WHERE id = '{memory_id}'"
            ))
            .map_err(|error| error.to_string())
    }

    fn insert_score_feedback(
        connection: &DbConnection,
        event_id: &str,
        memory_id: &str,
        signal: &str,
        weight: f32,
    ) -> Result<(), String> {
        connection
            .insert_feedback_event(
                event_id,
                &CreateFeedbackEventInput {
                    workspace_id: SCORE_WORKSPACE_ID.to_owned(),
                    target_type: "memory".to_owned(),
                    target_id: memory_id.to_owned(),
                    signal: signal.to_owned(),
                    weight,
                    source_type: "outcome_observed".to_owned(),
                    source_id: Some("test-run".to_owned()),
                    reason: Some("score decay fixture".to_owned()),
                    evidence_json: Some(r#"{"redacted":true}"#.to_owned()),
                    session_id: None,
                },
            )
            .map_err(|error| error.to_string())
    }

    fn insert_score_memory_link(
        connection: &DbConnection,
        link_id: &str,
        src_memory_id: &str,
        dst_memory_id: &str,
    ) -> Result<(), String> {
        connection
            .insert_memory_link(
                link_id,
                &CreateMemoryLinkInput {
                    src_memory_id: src_memory_id.to_owned(),
                    dst_memory_id: dst_memory_id.to_owned(),
                    relation: MemoryLinkRelation::Supports,
                    weight: 1.0,
                    confidence: 1.0,
                    directed: false,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: MemoryLinkSource::Agent,
                    created_by: Some("score-decay-test".to_owned()),
                    metadata_json: None,
                },
            )
            .map_err(|error| error.to_string())
    }

    fn stored_score_memory_link(
        link_id: &str,
        src_memory_id: &str,
        dst_memory_id: &str,
        metadata_json: Option<String>,
    ) -> StoredMemoryLink {
        StoredMemoryLink {
            id: link_id.to_owned(),
            src_memory_id: src_memory_id.to_owned(),
            dst_memory_id: dst_memory_id.to_owned(),
            relation: MemoryLinkRelation::Supports.as_str().to_owned(),
            weight: 1.0,
            confidence: 1.0,
            directed: false,
            evidence_count: 1,
            last_reinforced_at: None,
            source: MemoryLinkSource::Agent.as_str().to_owned(),
            created_at: "2026-05-16T00:00:00Z".to_owned(),
            created_by: Some("score-decay-test".to_owned()),
            metadata_json,
        }
    }

    fn denied_mesh_score_link_metadata() -> String {
        json!({
            "mesh": {
                "workspaceScopeDecision": "deny",
                "materialLane": "graphSignal",
                "cachedMaterialId": "mesh_steward_denied",
                "originWorkspaceId": "wsp_remote_private",
                "originWorkspaceLabel": "/Users/alice/private/repo",
                "producerPeerId": "peer_builder_one",
                "producerPeerLabel": "/Users/alice/private/peer-agent",
                "importDecisionId": "mesh_steward_decision_denied",
                "trustLane": "quarantined",
                "redactionPosture": "metadata_only"
            }
        })
        .to_string()
    }

    #[test]
    fn subsystem_name_is_stable() {
        assert_eq!(subsystem_name(), "steward");
    }

    #[test]
    fn job_type_roundtrip() -> TestResult {
        for job_type in JobType::all() {
            let s = job_type.as_str();
            let parsed: JobType = s.parse().map_err(|e: ParseJobTypeError| e.to_string())?;
            ensure(parsed, *job_type, &format!("roundtrip {s}"))?;
        }
        Ok(())
    }

    #[test]
    fn job_type_display() {
        assert_eq!(JobType::IndexRebuild.to_string(), "index_rebuild");
        assert_eq!(JobType::DecaySweep.to_string(), "decay_sweep");
        assert_eq!(
            JobType::GraphSnapshotPrune.to_string(),
            "graph_snapshot_prune"
        );
    }

    #[test]
    fn job_status_roundtrip() -> TestResult {
        for status in [
            JobStatus::Pending,
            JobStatus::Running,
            JobStatus::Completed,
            JobStatus::Failed,
            JobStatus::Cancelled,
            JobStatus::Skipped,
        ] {
            let s = status.as_str();
            let parsed: JobStatus = s.parse().map_err(|e: ParseJobStatusError| e.to_string())?;
            ensure(parsed, status, &format!("roundtrip {s}"))?;
        }
        Ok(())
    }

    #[test]
    fn job_status_is_terminal() {
        assert!(!JobStatus::Pending.is_terminal());
        assert!(!JobStatus::Running.is_terminal());
        assert!(JobStatus::Completed.is_terminal());
        assert!(JobStatus::Failed.is_terminal());
        assert!(JobStatus::Cancelled.is_terminal());
        assert!(JobStatus::Skipped.is_terminal());
    }

    #[test]
    fn job_status_is_success() {
        assert!(!JobStatus::Pending.is_success());
        assert!(!JobStatus::Running.is_success());
        assert!(JobStatus::Completed.is_success());
        assert!(!JobStatus::Failed.is_success());
        assert!(!JobStatus::Cancelled.is_success());
        assert!(JobStatus::Skipped.is_success());
    }

    #[test]
    fn job_lifecycle() {
        let mut job = Job::new("job-001", JobType::IndexRebuild, "2026-04-30T12:00:00Z");
        assert_eq!(job.status, JobStatus::Pending);

        job.start("2026-04-30T12:00:01Z");
        assert_eq!(job.status, JobStatus::Running);
        assert!(job.started_at.is_some());

        job.complete("2026-04-30T12:00:05Z", Some(100));
        assert_eq!(job.status, JobStatus::Completed);
        assert!(job.completed_at.is_some());
        assert_eq!(job.items_processed, Some(100));
    }

    #[test]
    fn job_failure() {
        let mut job = Job::new("job-002", JobType::DecaySweep, "2026-04-30T12:00:00Z");
        job.start("2026-04-30T12:00:01Z");
        job.fail("2026-04-30T12:00:02Z", "Database connection lost");

        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(job.error, Some("Database connection lost".to_owned()));
    }

    #[test]
    fn job_cancellation() {
        let mut job = Job::new("job-003", JobType::HealthCheck, "2026-04-30T12:00:00Z");
        job.cancel("2026-04-30T12:00:01Z");

        assert_eq!(job.status, JobStatus::Cancelled);
        assert!(job.completed_at.is_some());
    }

    #[test]
    fn job_json_has_required_fields() {
        let job = Job::new("job-004", JobType::StorageCompact, "2026-04-30T12:00:00Z")
            .with_priority(JobPriority::High)
            .with_context("manual trigger");
        let json = job.data_json();

        assert_eq!(json["id"], "job-004");
        assert_eq!(json["jobType"], "storage_compact");
        assert_eq!(json["status"], "pending");
        assert_eq!(json["priority"], "high");
        assert_eq!(json["context"], "manual trigger");
    }

    #[test]
    fn ledger_add_and_get() {
        let mut ledger = JobLedger::new();
        let job = Job::new("job-001", JobType::IndexRebuild, "2026-04-30T12:00:00Z");
        ledger.add_job(job);

        assert_eq!(ledger.len(), 1);
        assert!(ledger.get_job("job-001").is_some());
        assert!(ledger.get_job("job-999").is_none());
    }

    #[test]
    fn ledger_list_by_status() {
        let mut ledger = JobLedger::new();

        let mut job1 = Job::new("job-001", JobType::IndexRebuild, "2026-04-30T12:00:00Z");
        job1.start("2026-04-30T12:00:01Z");
        ledger.add_job(job1);

        let job2 = Job::new("job-002", JobType::DecaySweep, "2026-04-30T12:00:00Z");
        ledger.add_job(job2);

        assert_eq!(ledger.list_by_status(JobStatus::Running).len(), 1);
        assert_eq!(ledger.list_by_status(JobStatus::Pending).len(), 1);
        assert_eq!(ledger.list_by_status(JobStatus::Completed).len(), 0);
    }

    #[test]
    fn ledger_pending_by_priority() {
        let mut ledger = JobLedger::new();

        let job1 = Job::new("job-001", JobType::IndexRebuild, "2026-04-30T12:00:00Z")
            .with_priority(JobPriority::Low);
        let job2 = Job::new("job-002", JobType::HealthCheck, "2026-04-30T12:00:01Z")
            .with_priority(JobPriority::Critical);
        let job3 = Job::new("job-003", JobType::DecaySweep, "2026-04-30T12:00:02Z")
            .with_priority(JobPriority::Normal);

        ledger.add_job(job1);
        ledger.add_job(job2);
        ledger.add_job(job3);

        let pending = ledger.pending_by_priority();
        assert_eq!(pending.len(), 3);
        assert_eq!(pending[0].id, "job-002"); // Critical first
        assert_eq!(pending[1].id, "job-003"); // Normal second
        assert_eq!(pending[2].id, "job-001"); // Low last
    }

    #[test]
    fn ledger_statistics() {
        let mut ledger = JobLedger::new();

        let mut job1 = Job::new("job-001", JobType::IndexRebuild, "2026-04-30T12:00:00Z");
        job1.start("2026-04-30T12:00:01Z");
        job1.complete("2026-04-30T12:00:05Z", Some(50));

        let mut job2 = Job::new("job-002", JobType::DecaySweep, "2026-04-30T12:00:00Z");
        job2.start("2026-04-30T12:00:01Z");
        job2.fail("2026-04-30T12:00:02Z", "error");

        let job3 = Job::new("job-003", JobType::HealthCheck, "2026-04-30T12:00:00Z");

        ledger.add_job(job1);
        ledger.add_job(job2);
        ledger.add_job(job3);

        let stats = ledger.statistics();
        assert_eq!(stats.total, 3);
        assert_eq!(stats.completed, 1);
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.pending, 1);
    }

    #[test]
    fn ledger_report_json_has_schema() {
        let ledger = JobLedger::new();
        let json = ledger.report_json();

        assert_eq!(json["schema"], JOB_LEDGER_SCHEMA_V1);
        assert_eq!(json["command"], "steward jobs");
        assert!(json["statistics"].is_object());
        assert!(json["jobs"].is_array());
    }

    #[test]
    fn create_job_generates_id() {
        let mut ledger = JobLedger::new();

        let id1 = create_job(
            &mut ledger,
            JobType::IndexRebuild,
            JobPriority::Normal,
            "2026-04-30T12:00:00Z",
            None,
        );
        let id2 = create_job(
            &mut ledger,
            JobType::DecaySweep,
            JobPriority::High,
            "2026-04-30T12:00:01Z",
            Some("context".to_owned()),
        );

        assert_eq!(id1, "job-000001");
        assert_eq!(id2, "job-000002");
        assert_eq!(ledger.len(), 2);
    }

    #[test]
    fn job_priority_ordering() {
        assert!(JobPriority::Critical > JobPriority::High);
        assert!(JobPriority::High > JobPriority::Normal);
        assert!(JobPriority::Normal > JobPriority::Low);
    }

    // ========================================================================
    // EE-202: Job Budget Model Tests
    // ========================================================================

    #[test]
    fn resource_type_as_str_roundtrip() {
        for rt in ResourceType::all() {
            let s = rt.as_str();
            assert!(!s.is_empty(), "resource type should have a string form");
        }
    }

    #[test]
    fn resource_type_has_unit() {
        assert_eq!(ResourceType::TimeMs.unit(), "ms");
        assert_eq!(ResourceType::Items.unit(), "count");
        assert_eq!(ResourceType::MemoryBytes.unit(), "bytes");
    }

    #[test]
    fn resource_budget_time_limit() {
        let budget = ResourceBudget::time_limit_ms(5000);
        assert_eq!(budget.resource, ResourceType::TimeMs);
        assert_eq!(budget.limit, 5000);
        assert_eq!(budget.on_exceed, BudgetExceedAction::Cancel);
    }

    #[test]
    fn resource_budget_soft_limit() {
        let budget = ResourceBudget::time_soft_limit_ms(5000);
        assert_eq!(budget.on_exceed, BudgetExceedAction::Warn);
    }

    #[test]
    fn resource_consumption_add() {
        let mut c = ResourceConsumption::default();
        c.add(100);
        assert_eq!(c.consumed, 100);
        assert_eq!(c.peak, 100);

        c.add(50);
        assert_eq!(c.consumed, 150);
        assert_eq!(c.peak, 150);
    }

    #[test]
    fn resource_consumption_exceeds() {
        let mut c = ResourceConsumption::default();
        c.add(100);
        assert!(!c.exceeds(100));
        assert!(c.exceeds(99));
    }

    #[test]
    fn resource_consumption_percent() {
        let mut c = ResourceConsumption::default();
        c.add(50);
        assert!((c.percent_of(100) - 50.0).abs() < 0.01);
        assert!((c.percent_of(200) - 25.0).abs() < 0.01);
    }

    #[test]
    fn job_budget_state_record_and_check() {
        let mut state = JobBudgetState::new("job-001", "2026-04-30T12:00:00Z");
        state.add_budget(ResourceBudget::time_limit_ms(1000));
        state.add_budget(ResourceBudget::item_limit(100));

        state.record(ResourceType::TimeMs, 500);
        state.record(ResourceType::Items, 50);

        assert!(!state.should_cancel());

        state.record(ResourceType::TimeMs, 600); // Now at 1100, exceeds 1000

        assert!(state.should_cancel());
        let violations = state.check_budgets();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].resource, ResourceType::TimeMs);
    }

    #[test]
    fn job_budget_state_remaining() {
        let mut state = JobBudgetState::new("job-002", "2026-04-30T12:00:00Z");
        state.add_budget(ResourceBudget::item_limit(100));

        assert_eq!(state.remaining(ResourceType::Items), Some(100));

        state.record(ResourceType::Items, 30);
        assert_eq!(state.remaining(ResourceType::Items), Some(70));

        assert!(state.remaining(ResourceType::TimeMs).is_none());
    }

    #[test]
    fn job_budget_summary() {
        let mut state = JobBudgetState::new("job-003", "2026-04-30T12:00:00Z");
        state.add_budget(ResourceBudget::time_limit_ms(1000));
        state.record(ResourceType::TimeMs, 250);

        let summary = state.summary();
        assert_eq!(summary.job_id, "job-003");
        assert_eq!(summary.resources.len(), 1);
        assert_eq!(summary.resources[0].consumed, 250);
        assert_eq!(summary.resources[0].remaining, 750);
        assert!(!summary.has_violations());
    }

    #[test]
    fn job_budget_summary_with_violation() {
        let mut state = JobBudgetState::new("job-004", "2026-04-30T12:00:00Z");
        state.add_budget(ResourceBudget::item_limit(50));
        state.record(ResourceType::Items, 100);

        let summary = state.summary();
        assert!(summary.has_violations());
        assert_eq!(summary.violations.len(), 1);
        assert!(summary.resources[0].exceeded);
    }

    #[test]
    fn job_budget_json_has_schema() {
        let state = JobBudgetState::new("job-005", "2026-04-30T12:00:00Z");
        let json = state.data_json();

        assert_eq!(json["schema"], JOB_BUDGET_SCHEMA_V1);
        assert_eq!(json["jobId"], "job-005");
        assert!(json["resources"].is_array());
    }

    #[test]
    fn default_budgets_for_index_rebuild() {
        let budgets = default_budgets_for_job_type(JobType::IndexRebuild);
        assert!(!budgets.is_empty());

        let time_budget = budgets.iter().find(|b| b.resource == ResourceType::TimeMs);
        assert!(time_budget.is_some());
    }

    #[test]
    fn default_budgets_vary_by_job_type() {
        let rebuild = default_budgets_for_job_type(JobType::IndexRebuild);
        let health = default_budgets_for_job_type(JobType::HealthCheck);

        let rebuild_time = rebuild
            .iter()
            .find(|b| b.resource == ResourceType::TimeMs)
            .map(|b| b.limit);
        let health_time = health
            .iter()
            .find(|b| b.resource == ResourceType::TimeMs)
            .map(|b| b.limit);

        assert_ne!(rebuild_time, health_time);
    }

    #[test]
    fn default_budgets_for_graph_centrality_bound_time_and_links() -> TestResult {
        let budgets = default_budgets_for_job_type(JobType::CentralityRefresh);

        ensure(
            budgets
                .iter()
                .any(|budget| budget.resource == ResourceType::TimeMs && budget.limit == 180_000),
            true,
            "graph centrality time budget",
        )?;
        ensure(
            budgets
                .iter()
                .any(|budget| budget.resource == ResourceType::Items && budget.limit == 100_000),
            true,
            "graph centrality link budget",
        )
    }

    #[test]
    fn default_budgets_for_graph_snapshot_prune_bound_time_and_rows() -> TestResult {
        let budgets = default_budgets_for_job_type(JobType::GraphSnapshotPrune);

        ensure(
            budgets
                .iter()
                .any(|budget| budget.resource == ResourceType::TimeMs && budget.limit == 60_000),
            true,
            "graph snapshot prune time budget",
        )?;
        ensure(
            budgets
                .iter()
                .any(|budget| budget.resource == ResourceType::Items && budget.limit == 10_000),
            true,
            "graph snapshot prune row budget",
        )
    }

    #[test]
    fn create_job_budget_uses_defaults() {
        let state = create_job_budget("job-006", JobType::DecaySweep, "2026-04-30T12:00:00Z");
        assert!(!state.budgets.is_empty());
    }

    #[test]
    fn create_custom_budget_uses_provided() {
        let custom = vec![
            ResourceBudget::time_limit_ms(999),
            ResourceBudget::item_limit(42),
        ];
        let state = create_custom_budget("job-007", "2026-04-30T12:00:00Z", custom);

        assert_eq!(state.budgets.len(), 2);
        assert_eq!(state.budgets[0].limit, 999);
        assert_eq!(state.budgets[1].limit, 42);
    }

    #[test]
    fn budget_human_summary_format() {
        let mut state = JobBudgetState::new("job-008", "2026-04-30T12:00:00Z");
        state.add_budget(ResourceBudget::time_limit_ms(1000));
        state.record(ResourceType::TimeMs, 500);

        let summary = state.summary();
        let human = summary.human_summary();

        assert!(human.contains("job-008"));
        assert!(human.contains("time_ms"));
        assert!(human.contains("500/1000"));
    }

    #[test]
    fn budget_exceed_action_display() {
        assert_eq!(BudgetExceedAction::Cancel.to_string(), "cancel");
        assert_eq!(BudgetExceedAction::Warn.to_string(), "warn");
        assert_eq!(BudgetExceedAction::Throttle.to_string(), "throttle");
        assert_eq!(BudgetExceedAction::Checkpoint.to_string(), "checkpoint");
    }

    // ========================================================================
    // EE-203: Manual Runner Tests
    // ========================================================================

    #[test]
    fn run_outcome_display() {
        assert_eq!(RunOutcome::Success.to_string(), "success");
        assert_eq!(RunOutcome::Failed.to_string(), "failed");
        assert_eq!(RunOutcome::Cancelled.to_string(), "cancelled");
        assert_eq!(RunOutcome::Skipped.to_string(), "skipped");
        assert_eq!(RunOutcome::TimedOut.to_string(), "timed_out");
    }

    #[test]
    fn run_outcome_is_success() {
        assert!(RunOutcome::Success.is_success());
        assert!(RunOutcome::Skipped.is_success());
        assert!(!RunOutcome::Failed.is_success());
        assert!(!RunOutcome::Cancelled.is_success());
        assert!(!RunOutcome::TimedOut.is_success());
    }

    #[test]
    fn runner_options_defaults() {
        let opts = RunnerOptions::new();
        assert!(!opts.dry_run);
        assert!(!opts.verbose);
        assert!(!opts.continue_on_error);
        assert!(opts.time_limit_ms.is_none());
        assert!(opts.item_limit.is_none());
        assert!(opts.workspace_path.is_none());
        assert!(opts.database_path.is_none());
        assert!(opts.workspace_id.is_none());
        assert!(opts.as_of.is_none());
        assert!(opts.actor.is_none());
        assert!(opts.structural_decay);
        assert_eq!(opts.decay_settings, MemoryDecaySettings::default());
    }

    #[test]
    fn runner_options_builder() {
        let decay_settings = MemoryDecaySettings {
            thresholds: MemoryDecayThresholds {
                demote: 0.08,
                forget: 0.02,
            },
            half_lives: MemoryDecayHalfLives {
                procedural_rule: 730.0,
                ..MemoryDecayHalfLives::default()
            },
        };
        let opts = RunnerOptions::new()
            .with_dry_run(true)
            .with_verbose(true)
            .with_time_limit(5000)
            .with_item_limit(100)
            .with_workspace_path("/tmp/ee-runner")
            .with_database_path("/tmp/ee-runner/.ee/ee.db")
            .with_workspace_id(SCORE_WORKSPACE_ID)
            .with_as_of("2099-01-01T00:00:00Z")
            .with_actor("runner-test")
            .with_structural_decay(false)
            .with_decay_settings(decay_settings);

        assert!(opts.dry_run);
        assert!(opts.verbose);
        assert_eq!(opts.time_limit_ms, Some(5000));
        assert_eq!(opts.item_limit, Some(100));
        assert_eq!(opts.workspace_path, Some(PathBuf::from("/tmp/ee-runner")));
        assert_eq!(
            opts.database_path,
            Some(PathBuf::from("/tmp/ee-runner/.ee/ee.db"))
        );
        assert_eq!(opts.workspace_id, Some(SCORE_WORKSPACE_ID.to_owned()));
        assert_eq!(opts.as_of, Some("2099-01-01T00:00:00Z".to_owned()));
        assert_eq!(opts.actor, Some("runner-test".to_owned()));
        assert!(!opts.structural_decay);
        assert_eq!(opts.decay_settings, decay_settings);
    }

    #[test]
    fn decay_sweep_empty_workspace_path_is_successful_noop() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = tempdir.path().join(".ee").join("ee.db");
        std::fs::create_dir_all(
            database_path
                .parent()
                .ok_or_else(|| "database parent missing".to_owned())?,
        )
        .map_err(|error| error.to_string())?;
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        drop(connection);

        let options = RunnerOptions::new()
            .with_workspace_path(tempdir.path())
            .with_database_path(&database_path)
            .with_dry_run(true);
        let mut runner = ManualRunner::new(options);
        let result = runner.run_job_type(JobType::DecaySweep, None);

        ensure(result.outcome, RunOutcome::Success, "outcome")?;
        ensure(result.items_processed, Some(0), "items processed")?;
        ensure(result.error, None, "error")?;
        ensure(
            result
                .details
                .as_ref()
                .and_then(|details| details["schema"].as_str())
                .unwrap_or_default(),
            SCORE_DECAY_JOB_SCHEMA_V1,
            "details schema",
        )
    }

    // ========================================================================
    // EE-206: Score Decay Job Tests
    // ========================================================================

    #[test]
    fn score_decay_job_dry_run_does_not_mutate_memory_or_feedback() -> TestResult {
        let connection = open_score_decay_db()?;
        insert_score_memory(&connection, SCORE_MEMORY_A, 0.8)?;
        insert_score_feedback(
            &connection,
            "fb_decaydry000000000000000001",
            SCORE_MEMORY_A,
            "harmful",
            1.0,
        )?;
        insert_score_feedback(
            &connection,
            "fb_decaydry000000000000000002",
            SCORE_MEMORY_A,
            "harmful",
            1.0,
        )?;

        let mut options = ScoreDecayJobOptions::new(SCORE_WORKSPACE_ID);
        options.as_of = Some("2099-01-01T00:00:00Z".to_owned());
        options.dry_run = true;
        let report = run_score_decay_job(&connection, &options)?;

        ensure(report.schema, SCORE_DECAY_JOB_SCHEMA_V1, "schema")?;
        ensure(report.dry_run, true, "dry run flag")?;
        ensure(report.durable_mutation, false, "dry run mutation flag")?;
        ensure(report.scanned_count, 1, "scanned count")?;
        ensure(report.changed_count, 1, "changed count")?;
        ensure(report.applied_count, 0, "applied count")?;
        ensure(report.changes[0].applied, false, "change not applied")?;
        ensure(
            report.changes[0].new_confidence < report.changes[0].old_confidence,
            true,
            "dry run reports decrease",
        )?;

        let memory = connection
            .get_memory(SCORE_MEMORY_A)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "memory missing".to_owned())?;
        ensure((memory.confidence - 0.8).abs() < 0.0001, true, "unchanged")?;
        let events = connection
            .list_feedback_events_for_target("memory", SCORE_MEMORY_A)
            .map_err(|error| error.to_string())?;
        ensure(
            events.iter().all(|event| event.applied_at.is_none()),
            true,
            "feedback remains unapplied",
        )
    }

    #[test]
    fn score_decay_job_applies_negative_feedback_and_is_idempotent() -> TestResult {
        let connection = open_score_decay_db()?;
        insert_score_memory(&connection, SCORE_MEMORY_A, 0.8)?;
        insert_score_feedback(
            &connection,
            "fb_decayapply0000000000000001",
            SCORE_MEMORY_A,
            "harmful",
            1.0,
        )?;
        insert_score_feedback(
            &connection,
            "fb_decayapply0000000000000002",
            SCORE_MEMORY_A,
            "harmful",
            1.0,
        )?;

        let mut options = ScoreDecayJobOptions::new(SCORE_WORKSPACE_ID);
        options.as_of = Some("2099-01-01T00:00:00Z".to_owned());
        options.actor = Some("score-decay-test".to_owned());
        let first = run_score_decay_job(&connection, &options)?;

        ensure(first.changed_count, 1, "first changed")?;
        ensure(first.applied_count, 1, "first applied")?;
        ensure(first.durable_mutation, true, "durable mutation")?;
        ensure(first.changes[0].audit_id.is_some(), true, "audit id")?;

        let memory = connection
            .get_memory(SCORE_MEMORY_A)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "memory missing".to_owned())?;
        ensure(
            memory.confidence < 0.8,
            true,
            "confidence decreased after job",
        )?;
        ensure(
            memory.provenance_verification_status,
            "unverified".to_owned(),
            "score update invalidates provenance verification",
        )?;
        let audit = connection
            .list_audit_by_target("memory", SCORE_MEMORY_A, None)
            .map_err(|error| error.to_string())?;
        ensure(audit.len(), 1, "audit count")?;
        ensure(
            audit[0].action.as_str(),
            audit_actions::MEMORY_SCORE_DECAY,
            "audit action",
        )?;
        let events = connection
            .list_feedback_events_for_target("memory", SCORE_MEMORY_A)
            .map_err(|error| error.to_string())?;
        ensure(
            events.iter().all(|event| event.applied_at.is_some()),
            true,
            "feedback marked applied",
        )?;

        let second = run_score_decay_job(&connection, &options)?;
        ensure(second.changed_count, 0, "second changed count")?;
        ensure(second.applied_count, 0, "second applied count")
    }

    #[test]
    fn score_decay_job_include_decay_dry_run_reports_demote_without_mutation() -> TestResult {
        let connection = open_score_decay_db()?;
        insert_score_memory(&connection, SCORE_MEMORY_A, 0.8)?;
        set_score_memory_timestamp(&connection, SCORE_MEMORY_A, "2026-01-01T00:00:00Z")?;

        let mut options = ScoreDecayJobOptions::new(SCORE_WORKSPACE_ID);
        options.as_of = Some("2030-01-01T00:00:00Z".to_owned());
        options.include_decay_actions = true;
        options.dry_run = true;
        let report = run_score_decay_job(&connection, &options)?;

        ensure(report.changed_count, 1, "changed count")?;
        ensure(report.demoted_count, 1, "planned demotion count")?;
        ensure(report.tombstoned_count, 0, "planned tombstone count")?;
        ensure(
            report.changes[0].decay_action,
            MemoryDecayAction::Demote,
            "decay action",
        )?;
        ensure(
            report.changes[0].new_level.as_str(),
            "semantic",
            "demoted level",
        )?;
        let memory = connection
            .get_memory(SCORE_MEMORY_A)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "memory missing".to_owned())?;
        ensure(memory.level.as_str(), "procedural", "dry run keeps level")?;
        let audits = connection
            .list_audit_by_target("memory", SCORE_MEMORY_A, None)
            .map_err(|error| error.to_string())?;
        ensure(audits.is_empty(), true, "dry run writes no audit rows")
    }

    #[test]
    fn score_decay_structural_decay_protects_articulation_memory() -> TestResult {
        const SCORE_MEMORY_C: &str = "mem_scoredecay0000000000000003";
        const SCORE_MEMORY_D: &str = "mem_scoredecay0000000000000004";
        let connection = open_score_decay_db()?;
        insert_score_memory(&connection, SCORE_MEMORY_A, 0.8)?;
        insert_score_memory(&connection, SCORE_MEMORY_B, 0.8)?;
        insert_score_memory(&connection, SCORE_MEMORY_C, 0.8)?;
        insert_score_memory(&connection, SCORE_MEMORY_D, 0.8)?;
        for memory_id in [
            SCORE_MEMORY_A,
            SCORE_MEMORY_B,
            SCORE_MEMORY_C,
            SCORE_MEMORY_D,
        ] {
            set_score_memory_timestamp(&connection, memory_id, "2026-01-01T00:00:00Z")?;
        }
        insert_score_memory_link(
            &connection,
            "link_00000000000000000000000051",
            SCORE_MEMORY_A,
            SCORE_MEMORY_B,
        )?;
        insert_score_memory_link(
            &connection,
            "link_00000000000000000000000052",
            SCORE_MEMORY_B,
            SCORE_MEMORY_C,
        )?;
        insert_score_memory_link(
            &connection,
            "link_00000000000000000000000053",
            SCORE_MEMORY_A,
            SCORE_MEMORY_C,
        )?;
        insert_score_memory_link(
            &connection,
            "link_00000000000000000000000054",
            SCORE_MEMORY_A,
            SCORE_MEMORY_D,
        )?;

        let mut legacy = ScoreDecayJobOptions::new(SCORE_WORKSPACE_ID);
        legacy.as_of = Some("2029-01-01T00:00:00Z".to_owned());
        legacy.include_decay_actions = true;
        legacy.structural_decay = false;
        legacy.dry_run = true;
        let legacy_report = run_score_decay_job(&connection, &legacy)?;

        let mut structural = legacy.clone();
        structural.structural_decay = true;
        let structural_report = run_score_decay_job(&connection, &structural)?;

        let legacy_bridge = legacy_report
            .changes
            .iter()
            .find(|change| change.memory_id == SCORE_MEMORY_A)
            .ok_or_else(|| "legacy bridge change missing".to_owned())?;
        let structural_bridge = structural_report
            .changes
            .iter()
            .find(|change| change.memory_id == SCORE_MEMORY_A)
            .ok_or_else(|| "structural bridge change missing".to_owned())?;

        ensure(
            legacy_bridge.decay_action,
            MemoryDecayAction::Demote,
            "legacy bridge demotes",
        )?;
        ensure(
            structural_bridge.decay_action,
            MemoryDecayAction::Preserve,
            "structural bridge preserves",
        )?;
        let adjustment = structural_bridge
            .structural_adjustment
            .as_ref()
            .ok_or_else(|| "structural adjustment missing".to_owned())?;
        ensure(
            adjustment.is_articulation_point,
            true,
            "bridge articulation",
        )?;
        ensure(
            adjustment.structural_multiplier < 1.0,
            true,
            "bridge multiplier protects",
        )?;
        ensure(
            structural_report.data_json()["decay"]["structuralAdjustments"].is_array(),
            true,
            "structural adjustments json",
        )
    }

    #[test]
    fn score_decay_structural_graph_ignores_denied_mesh_links() -> TestResult {
        let memory_ids = [
            SCORE_MEMORY_A.to_owned(),
            SCORE_MEMORY_B.to_owned(),
            "mem_scoredecay0000000000000003".to_owned(),
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        let allowed_links = vec![
            stored_score_memory_link(
                "link_00000000000000000000000151",
                SCORE_MEMORY_A,
                SCORE_MEMORY_B,
                None,
            ),
            stored_score_memory_link(
                "link_00000000000000000000000152",
                SCORE_MEMORY_B,
                "mem_scoredecay0000000000000003",
                None,
            ),
        ];
        let denied_links = vec![
            allowed_links[0].clone(),
            stored_score_memory_link(
                "link_00000000000000000000000153",
                SCORE_MEMORY_B,
                "mem_scoredecay0000000000000003",
                Some(denied_mesh_score_link_metadata()),
            ),
        ];

        let allowed_graph = structural_decay_graph(&memory_ids, &allowed_links);
        let denied_graph = structural_decay_graph(&memory_ids, &denied_links);

        ensure(
            compute_structural_decay_adjustment(&allowed_graph, SCORE_MEMORY_B)
                .is_articulation_point,
            true,
            "visible local link can make middle memory structural",
        )?;
        ensure(
            compute_structural_decay_adjustment(&denied_graph, SCORE_MEMORY_B)
                .is_articulation_point,
            false,
            "denied mesh link is ignored for structural decay",
        )
    }

    #[test]
    fn score_decay_job_uses_configured_decay_half_life_and_thresholds() -> TestResult {
        let connection = open_score_decay_db()?;
        insert_score_memory(&connection, SCORE_MEMORY_A, 0.8)?;
        set_score_memory_timestamp(&connection, SCORE_MEMORY_A, "2026-01-01T00:00:00Z")?;

        let mut options = ScoreDecayJobOptions::new(SCORE_WORKSPACE_ID);
        options.as_of = Some("2026-01-11T00:00:00Z".to_owned());
        options.include_decay_actions = true;
        options.dry_run = true;
        options.decay_thresholds = MemoryDecayThresholds {
            demote: 0.3,
            forget: 0.0001,
        };
        options.decay_half_lives = MemoryDecayHalfLives {
            procedural_rule: 1.0,
            ..MemoryDecayHalfLives::default()
        };

        let report = run_score_decay_job(&connection, &options)?;

        ensure(report.demoted_count, 1, "configured demotion count")?;
        ensure(report.tombstoned_count, 0, "configured tombstone count")?;
        ensure(
            report.changes[0].half_life_days,
            1.0,
            "configured procedural half-life",
        )?;
        ensure(
            report.changes[0].demote_threshold,
            0.3,
            "configured demote threshold",
        )?;
        ensure(
            report.data_json()["decay"]["halfLifeDays"]["proceduralRule"].as_f64(),
            Some(1.0),
            "configured half-life JSON",
        )
    }

    #[test]
    fn score_decay_job_include_decay_tombstones_and_audits() -> TestResult {
        let connection = open_score_decay_db()?;
        insert_score_memory(&connection, SCORE_MEMORY_A, 0.8)?;
        set_score_memory_timestamp(&connection, SCORE_MEMORY_A, "2026-01-01T00:00:00Z")?;

        let mut options = ScoreDecayJobOptions::new(SCORE_WORKSPACE_ID);
        options.as_of = Some("2099-01-01T00:00:00Z".to_owned());
        options.include_decay_actions = true;
        options.actor = Some("decay-lifecycle-test".to_owned());
        let report = run_score_decay_job(&connection, &options)?;

        ensure(report.tombstoned_count, 1, "tombstone count")?;
        ensure(report.applied_count, 1, "applied count")?;
        ensure(
            report.changes[0].decay_action,
            MemoryDecayAction::Tombstone,
            "decay action",
        )?;
        ensure(
            report.changes[0].lifecycle_audit_id.is_some(),
            true,
            "lifecycle audit id",
        )?;
        let memory = connection
            .get_memory(SCORE_MEMORY_A)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "memory missing".to_owned())?;
        ensure(memory.tombstoned_at.is_some(), true, "memory tombstoned")?;
        let audits = connection
            .list_audit_by_target("memory", SCORE_MEMORY_A, None)
            .map_err(|error| error.to_string())?;
        ensure(
            audits
                .iter()
                .any(|entry| entry.action == audit_actions::MEMORY_DECAY_TOMBSTONE),
            true,
            "decay tombstone audit row",
        )
    }

    #[test]
    fn score_decay_job_keeps_helpful_and_neutral_feedback_pending() -> TestResult {
        let connection = open_score_decay_db()?;
        insert_score_memory(&connection, SCORE_MEMORY_A, 0.8)?;
        insert_score_feedback(
            &connection,
            "fb_decaymixed0000000000000001",
            SCORE_MEMORY_A,
            "harmful",
            1.0,
        )?;
        insert_score_feedback(
            &connection,
            "fb_decaymixed0000000000000002",
            SCORE_MEMORY_A,
            "stale",
            1.0,
        )?;
        insert_score_feedback(
            &connection,
            "fb_decaymixed0000000000000003",
            SCORE_MEMORY_A,
            "helpful",
            1.0,
        )?;
        insert_score_feedback(
            &connection,
            "fb_decaymixed0000000000000004",
            SCORE_MEMORY_A,
            "neutral",
            1.0,
        )?;

        let mut options = ScoreDecayJobOptions::new(SCORE_WORKSPACE_ID);
        options.as_of = Some("2099-01-01T00:00:00Z".to_owned());
        let report = run_score_decay_job(&connection, &options)?;

        ensure(report.changed_count, 1, "changed count")?;
        ensure(report.applied_count, 1, "applied count")?;
        ensure(
            report.changes[0].feedback_total_count,
            4,
            "all pending feedback considered",
        )?;
        ensure(
            report.changes[0]
                .feedback_event_ids
                .contains(&"fb_decaymixed0000000000000001".to_owned()),
            true,
            "harmful feedback consumed",
        )?;
        ensure(
            report.changes[0]
                .feedback_event_ids
                .contains(&"fb_decaymixed0000000000000002".to_owned()),
            true,
            "stale feedback consumed",
        )?;
        ensure(
            report.changes[0]
                .feedback_event_ids
                .contains(&"fb_decaymixed0000000000000003".to_owned()),
            false,
            "helpful feedback not consumed",
        )?;
        ensure(
            report.changes[0]
                .feedback_event_ids
                .contains(&"fb_decaymixed0000000000000004".to_owned()),
            false,
            "neutral feedback not consumed",
        )?;

        let events = connection
            .list_feedback_events_for_target("memory", SCORE_MEMORY_A)
            .map_err(|error| error.to_string())?;
        let event_applied = |event_id: &str| -> Result<bool, String> {
            events
                .iter()
                .find(|event| event.id == event_id)
                .map(|event| event.applied_at.is_some())
                .ok_or_else(|| format!("feedback event missing: {event_id}"))
        };

        ensure(
            event_applied("fb_decaymixed0000000000000001")?,
            true,
            "harmful feedback applied",
        )?;
        ensure(
            event_applied("fb_decaymixed0000000000000002")?,
            true,
            "stale feedback applied",
        )?;
        ensure(
            event_applied("fb_decaymixed0000000000000003")?,
            false,
            "helpful feedback remains pending",
        )?;
        ensure(
            event_applied("fb_decaymixed0000000000000004")?,
            false,
            "neutral feedback remains pending",
        )
    }

    #[test]
    fn score_decay_job_decays_stale_memory_without_feedback() -> TestResult {
        let connection = open_score_decay_db()?;
        insert_score_memory(&connection, SCORE_MEMORY_B, 0.6)?;

        let mut options = ScoreDecayJobOptions::new(SCORE_WORKSPACE_ID);
        options.as_of = Some("2099-01-01T00:00:00Z".to_owned());
        options.item_limit = Some(1);
        let report = run_score_decay_job(&connection, &options)?;

        ensure(report.scanned_count, 1, "scanned count")?;
        ensure(report.changed_count, 1, "changed count")?;
        ensure(report.changes[0].feedback_total_count, 0, "feedback count")?;
        ensure(
            report.changes[0].stale_periods > 0,
            true,
            "stale periods present",
        )?;
        ensure(
            report.changes[0].new_confidence < 0.6,
            true,
            "stale memory decayed",
        )
    }

    #[test]
    fn manual_runner_decay_sweep_uses_real_score_decay_handler() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join("ee.db");
        {
            let connection =
                DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
            connection.migrate().map_err(|error| error.to_string())?;
            connection
                .insert_workspace(
                    SCORE_WORKSPACE_ID,
                    &CreateWorkspaceInput {
                        path: temp.path().to_string_lossy().into_owned(),
                        name: Some("score-decay-runner".to_owned()),
                    },
                )
                .map_err(|error| error.to_string())?;
            insert_score_memory(&connection, SCORE_MEMORY_A, 0.8)?;
            insert_score_feedback(
                &connection,
                "fb_runreal0000000000000000001",
                SCORE_MEMORY_A,
                "harmful",
                1.0,
            )?;
            connection.close().map_err(|error| error.to_string())?;
        }

        let opts = RunnerOptions::new()
            .with_database_path(database_path.clone())
            .with_workspace_id(SCORE_WORKSPACE_ID)
            .with_as_of("2099-01-01T00:00:00Z")
            .with_actor("manual-runner-test");
        let mut runner = ManualRunner::new(opts);
        let result = runner.run_job_type(JobType::DecaySweep, Some("manual test".to_owned()));

        assert_eq!(result.job_type, JobType::DecaySweep);
        assert_eq!(result.outcome, RunOutcome::Success);
        assert_eq!(result.items_processed, Some(1));
        let details = result
            .details
            .ok_or_else(|| "score decay runner details missing".to_owned())?;
        ensure(
            details["schema"].as_str(),
            Some(SCORE_DECAY_JOB_SCHEMA_V1),
            "details schema",
        )?;
        ensure(
            details["summary"]["appliedCount"].as_u64(),
            Some(1),
            "applied count",
        )?;

        let connection =
            DbConnection::open_file(database_path).map_err(|error| error.to_string())?;
        let memory = connection
            .get_memory(SCORE_MEMORY_A)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "memory missing after runner".to_owned())?;
        ensure(
            memory.confidence < 0.8,
            true,
            "runner applied confidence decay",
        )
    }

    #[test]
    fn manual_runner_decay_sweep_zero_time_budget_times_out_before_mutation() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join("ee.db");
        {
            let connection =
                DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
            connection.migrate().map_err(|error| error.to_string())?;
            connection
                .insert_workspace(
                    SCORE_WORKSPACE_ID,
                    &CreateWorkspaceInput {
                        path: temp.path().to_string_lossy().into_owned(),
                        name: Some("score-decay-zero-budget".to_owned()),
                    },
                )
                .map_err(|error| error.to_string())?;
            insert_score_memory(&connection, SCORE_MEMORY_A, 0.8)?;
            insert_score_feedback(
                &connection,
                "fb_runbudget00000000000000001",
                SCORE_MEMORY_A,
                "harmful",
                1.0,
            )?;
            connection.close().map_err(|error| error.to_string())?;
        }

        let opts = RunnerOptions::new()
            .with_database_path(database_path.clone())
            .with_workspace_id(SCORE_WORKSPACE_ID)
            .with_as_of("2099-01-01T00:00:00Z")
            .with_time_limit(0)
            .with_actor("zero-budget-test");
        let mut runner = ManualRunner::new(opts);
        let result = runner.run_job_type(JobType::DecaySweep, Some("zero budget".to_owned()));

        ensure(result.job_type, JobType::DecaySweep, "job type")?;
        ensure(result.outcome, RunOutcome::TimedOut, "zero-budget outcome")?;
        ensure(result.items_processed, Some(1), "preflight scanned count")?;
        ensure(
            result.error.as_deref().is_some_and(|message| {
                message.contains("Timed out before durable decay mutations")
            }),
            true,
            "timeout reason",
        )?;
        let details = result
            .details
            .ok_or_else(|| "score decay preflight details missing".to_owned())?;
        ensure(
            details["summary"]["appliedCount"].as_u64(),
            Some(0),
            "preflight applied count",
        )?;
        ensure(
            details["durableMutation"].as_bool(),
            Some(false),
            "preflight durable mutation",
        )?;

        let connection =
            DbConnection::open_file(database_path).map_err(|error| error.to_string())?;
        let memory = connection
            .get_memory(SCORE_MEMORY_A)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "memory missing after zero-budget runner".to_owned())?;
        ensure((memory.confidence - 0.8).abs() < 0.0001, true, "unchanged")?;
        let events = connection
            .list_feedback_events_for_target("memory", SCORE_MEMORY_A)
            .map_err(|error| error.to_string())?;
        ensure(
            events.iter().all(|event| event.applied_at.is_none()),
            true,
            "feedback remains unapplied",
        )?;
        let audit = connection
            .list_audit_by_target("memory", SCORE_MEMORY_A, None)
            .map_err(|error| error.to_string())?;
        ensure(audit.is_empty(), true, "no score-decay audit rows")
    }

    #[test]
    fn manual_runner_decay_sweep_dry_run_missing_db_does_not_create_file() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join("missing-ee.db");
        ensure(database_path.exists(), false, "database initially absent")?;

        let opts = RunnerOptions::new()
            .with_database_path(database_path.clone())
            .with_workspace_id(SCORE_WORKSPACE_ID)
            .with_dry_run(true);
        let mut runner = ManualRunner::new(opts);
        let result =
            runner.run_job_type(JobType::DecaySweep, Some("dry-run missing db".to_owned()));

        ensure(result.job_type, JobType::DecaySweep, "job type")?;
        ensure(
            result.outcome,
            RunOutcome::Failed,
            "dry-run missing db outcome",
        )?;
        ensure(
            database_path.exists(),
            false,
            "dry-run must not create db file",
        )?;
        let details = result
            .details
            .ok_or_else(|| "missing-db dry-run details missing".to_owned())?;
        ensure(
            details["code"].as_str(),
            Some("decay_sweep_database_missing"),
            "missing db code",
        )?;
        ensure(details["dryRun"].as_bool(), Some(true), "dry-run detail")?;
        ensure(
            details["durableMutation"].as_bool(),
            Some(false),
            "durable mutation detail",
        )
    }

    #[test]
    fn manual_runner_graph_centrality_dry_run_uses_budgeted_handler() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join("ee.db");
        {
            let connection =
                DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
            connection.migrate().map_err(|error| error.to_string())?;
            connection
                .insert_workspace(
                    SCORE_WORKSPACE_ID,
                    &CreateWorkspaceInput {
                        path: temp.path().to_string_lossy().into_owned(),
                        name: Some("graph-centrality-runner".to_owned()),
                    },
                )
                .map_err(|error| error.to_string())?;
            connection.close().map_err(|error| error.to_string())?;
        }

        let opts = RunnerOptions::new()
            .with_database_path(database_path)
            .with_workspace_id(SCORE_WORKSPACE_ID)
            .with_dry_run(true)
            .with_item_limit(7);
        let mut runner = ManualRunner::new(opts);
        let result =
            runner.run_job_type(JobType::CentralityRefresh, Some("graph dry-run".to_owned()));

        ensure(result.job_type, JobType::CentralityRefresh, "job type")?;
        ensure(result.outcome, RunOutcome::Success, "outcome")?;
        ensure(result.error, None, "error")?;
        let details = result
            .details
            .ok_or_else(|| "graph centrality details missing".to_owned())?;
        ensure(
            details["schema"].as_str(),
            Some(GRAPH_CENTRALITY_JOB_SCHEMA_V1),
            "details schema",
        )?;
        ensure(details["dryRun"].as_bool(), Some(true), "details dry-run")?;
        ensure(
            details["durableMutation"].as_bool(),
            Some(false),
            "no durable mutation",
        )?;
        ensure(
            details["preflight"]["command"].as_str(),
            Some("graph centrality refresh"),
            "preflight command",
        )?;
        ensure(details["result"].is_null(), true, "dry-run result absent")?;
        let budget = result
            .budget_summary
            .ok_or_else(|| "budget summary missing".to_owned())?;
        ensure(
            budget
                .resources
                .iter()
                .any(|resource| resource.resource == ResourceType::Items && resource.limit == 7),
            true,
            "runner link limit budget",
        )
    }

    #[test]
    fn manual_runner_graph_centrality_zero_budget_cancels_before_mutation() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join("ee.db");
        {
            let connection =
                DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
            connection.migrate().map_err(|error| error.to_string())?;
            connection
                .insert_workspace(
                    SCORE_WORKSPACE_ID,
                    &CreateWorkspaceInput {
                        path: temp.path().to_string_lossy().into_owned(),
                        name: Some("graph-zero-budget".to_owned()),
                    },
                )
                .map_err(|error| error.to_string())?;
            connection.close().map_err(|error| error.to_string())?;
        }

        let opts = RunnerOptions::new()
            .with_database_path(database_path)
            .with_workspace_id(SCORE_WORKSPACE_ID)
            .with_time_limit(0);
        let mut runner = ManualRunner::new(opts);
        let result = runner.run_job_type(
            JobType::CentralityRefresh,
            Some("zero graph budget".to_owned()),
        );

        ensure(result.job_type, JobType::CentralityRefresh, "job type")?;
        ensure(result.outcome, RunOutcome::Cancelled, "outcome")?;
        ensure(
            result
                .error
                .as_deref()
                .is_some_and(|message| message.contains("before durable graph snapshot mutation")),
            true,
            "cancel reason",
        )?;
        let details = result
            .details
            .ok_or_else(|| "graph budget details missing".to_owned())?;
        ensure(
            details["dryRun"].as_bool(),
            Some(false),
            "non-dry-run cancellation must report dry-run false",
        )?;
        ensure(
            details["durableMutation"].as_bool(),
            Some(false),
            "cancelled before mutation",
        )?;
        ensure(
            details["result"].is_null(),
            true,
            "cancelled before non-dry-run result",
        )
    }

    #[test]
    fn manual_runner_graph_centrality_dry_run_missing_db_does_not_create_file() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join("missing-graph-ee.db");
        ensure(database_path.exists(), false, "database initially absent")?;

        let opts = RunnerOptions::new()
            .with_database_path(database_path.clone())
            .with_workspace_id(SCORE_WORKSPACE_ID)
            .with_dry_run(true);
        let mut runner = ManualRunner::new(opts);
        let result = runner.run_job_type(
            JobType::CentralityRefresh,
            Some("dry-run missing graph db".to_owned()),
        );

        ensure(result.job_type, JobType::CentralityRefresh, "job type")?;
        ensure(result.outcome, RunOutcome::Failed, "missing db outcome")?;
        ensure(database_path.exists(), false, "dry-run must not create db")?;
        let details = result
            .details
            .ok_or_else(|| "missing-db graph details missing".to_owned())?;
        ensure(
            details["schema"].as_str(),
            Some(GRAPH_CENTRALITY_ERROR_SCHEMA_V1),
            "error schema",
        )?;
        ensure(
            details["code"].as_str(),
            Some("graph_centrality_database_missing"),
            "error code",
        )?;
        ensure(details["dryRun"].as_bool(), Some(true), "dry-run detail")?;
        ensure(
            details["durableMutation"].as_bool(),
            Some(false),
            "durable mutation detail",
        )
    }

    // ========================================================================
    // EE-207: Foreground Daemon Tests
    // ========================================================================

    #[test]
    fn daemon_foreground_once_runs_configured_job() -> TestResult {
        let mut options = DaemonForegroundOptions::new("/tmp/ee-daemon");
        options.interval_ms = 0;
        options.job_types = vec![JobType::HealthCheck];

        let report = run_daemon_foreground(&options)?;

        ensure(report.schema, DAEMON_FOREGROUND_SCHEMA_V1, "schema")?;
        ensure(report.command, "daemon", "command")?;
        ensure(report.mode, "foreground", "mode")?;
        ensure(report.daemonized, false, "daemonized")?;
        ensure(report.ticks.len(), 1, "tick count")?;
        ensure(report.jobs_run(), 1, "jobs run")?;
        ensure(report.succeeded_count(), 1, "succeeded")?;
        ensure(report.failed_count(), 0, "failed")?;
        ensure(report.skipped_count(), 0, "skipped")?;
        let json = report.data_json();
        ensure(
            json["summary"]["tickCount"].as_u64(),
            Some(1),
            "json tick count",
        )?;
        ensure(
            json["jobTypes"][0].as_str(),
            Some("health_check"),
            "json job type",
        )
    }

    #[test]
    fn daemon_foreground_lab_runtime_schedule_is_deterministic() -> TestResult {
        let mut lab = LabRuntime::new(LabConfig::new(207));
        let root = lab.state.create_root_region(Budget::INFINITE);
        let mut options = DaemonForegroundOptions::new("/tmp/ee-daemon-lab");
        options.interval_ms = 0;
        options.tick_limit = 2;
        options.job_types = vec![JobType::HealthCheck];

        let (task_id, mut handle) = lab
            .state
            .create_task(root, Budget::INFINITE, async move {
                let Some(cx) = Cx::current() else {
                    return Outcome::Err("LabRuntime task should install Cx".to_owned());
                };
                run_daemon_foreground_supervised(&cx, &options).await
            })
            .map_err(|error| error.to_string())?;
        lab.scheduler.lock().schedule(task_id, 0);
        lab.run_until_quiescent();

        let outcome = handle
            .try_join()
            .map_err(|error| format!("daemon lab join failed: {error}"))?
            .ok_or_else(|| "daemon lab task did not finish".to_owned())?;
        let Outcome::Ok(report) = outcome else {
            return Err(format!("daemon lab outcome was not ok: {outcome:?}"));
        };
        ensure(report.ticks.len(), 2, "tick count")?;
        ensure(report.jobs_run(), 2, "jobs run")?;
        ensure(report.succeeded_count(), 2, "success count")?;

        let golden = json!({
            "schema": "ee.steward.daemon_schedule_golden.v1",
            "supervisor": "asupersync_foreground",
            "tickCount": 2,
            "jobsRun": 2,
            "succeeded": 2,
            "failed": 0,
            "jobTypes": ["health_check"],
            "ticks": [
                {
                    "tick": 1,
                    "jobs": [
                        {
                            "id": "job-000001",
                            "jobType": "health_check",
                            "outcome": "success"
                        }
                    ]
                },
                {
                    "tick": 2,
                    "jobs": [
                        {
                            "id": "job-000001",
                            "jobType": "health_check",
                            "outcome": "success"
                        }
                    ]
                }
            ]
        });
        let actual = json!({
            "schema": "ee.steward.daemon_schedule_golden.v1",
            "supervisor": report.supervisor,
            "tickCount": report.ticks.len(),
            "jobsRun": report.jobs_run(),
            "succeeded": report.succeeded_count(),
            "failed": report.failed_count(),
            "jobTypes": report
                .job_types
                .iter()
                .map(|job_type| job_type.as_str())
                .collect::<Vec<_>>(),
            "ticks": report
                .ticks
                .iter()
                .map(|tick| {
                    json!({
                        "tick": tick.tick,
                        "jobs": tick
                            .report
                            .results
                            .iter()
                            .map(|result| {
                                json!({
                                    "id": result.job_id,
                                    "jobType": result.job_type.as_str(),
                                    "outcome": result.outcome.as_str(),
                                })
                            })
                            .collect::<Vec<_>>(),
                    })
                })
                .collect::<Vec<_>>(),
        });
        ensure(actual, golden, "daemon schedule golden")
    }

    #[test]
    fn daemon_foreground_lab_runtime_observes_cancellation_between_ticks() -> TestResult {
        let mut lab = LabRuntime::new(LabConfig::new(208));
        let root = lab.state.create_root_region(Budget::INFINITE);
        let mut options = DaemonForegroundOptions::new("/tmp/ee-daemon-cancel-lab");
        options.interval_ms = 1_000;
        options.tick_limit = 2;
        options.job_types = vec![JobType::HealthCheck];
        let observed_cx: Arc<StdMutex<Option<Cx>>> = Arc::new(StdMutex::new(None));
        let observed_cx_for_task = Arc::clone(&observed_cx);

        let (task_id, mut handle) = lab
            .state
            .create_task(root, Budget::INFINITE, async move {
                let Some(cx) = Cx::current() else {
                    return Outcome::Err("LabRuntime task should install Cx".to_owned());
                };
                {
                    let Ok(mut slot) = observed_cx_for_task.lock() else {
                        return Outcome::Err("daemon cancellation Cx slot poisoned".to_owned());
                    };
                    *slot = Some(cx.clone());
                }
                run_daemon_foreground_supervised(&cx, &options).await
            })
            .map_err(|error| error.to_string())?;
        lab.scheduler.lock().schedule(task_id, 0);
        lab.run_until_idle();
        ensure(
            handle.is_finished(),
            false,
            "task should wait between ticks",
        )?;

        let reason = CancelReason::user("daemon cancellation test");
        for (cancelled_task, priority) in lab.state.cancel_request(root, &reason, None) {
            lab.scheduler.lock().schedule(cancelled_task, priority);
        }
        lab.scheduler.lock().schedule(task_id, 0);
        lab.advance_time(1_000_000_000);
        lab.run_until_quiescent();

        let cancellation_reason = match handle.try_join() {
            Ok(Some(Outcome::Cancelled(reason))) => reason,
            Err(JoinError::Cancelled(reason))
                if reason.message.as_deref() == Some("join channel closed") =>
            {
                observed_cx
                    .lock()
                    .map_err(|_| "daemon cancellation Cx slot poisoned".to_owned())?
                    .as_ref()
                    .and_then(Cx::cancel_reason)
                    .ok_or_else(|| "daemon cancellation reason missing from Cx".to_owned())?
            }
            Err(JoinError::Cancelled(reason)) => reason,
            Ok(Some(other)) => {
                return Err(format!(
                    "daemon cancellation outcome was not cancelled: {other:?}"
                ));
            }
            Ok(None) => return Err("daemon cancellation task did not finish".to_owned()),
            Err(error) => return Err(format!("daemon cancellation join failed: {error}")),
        };

        {
            let golden = json!({
                "schema": "ee.steward.daemon_cancellation_golden.v1",
                "outcome": "cancelled",
                "reason": "daemon cancellation test",
            });
            let actual = json!({
                "schema": "ee.steward.daemon_cancellation_golden.v1",
                "outcome": "cancelled",
                "reason": cancellation_reason.message.as_deref(),
            });
            ensure(actual, golden, "daemon cancellation golden")
        }
    }

    #[test]
    fn daemon_foreground_rejects_zero_tick_limit() -> TestResult {
        let mut options = DaemonForegroundOptions::new("/tmp/ee-daemon");
        options.tick_limit = 0;

        ensure(
            run_daemon_foreground(&options).is_err(),
            true,
            "zero tick limit rejected",
        )
    }

    #[test]
    fn daemon_foreground_accepts_configured_safety_limits() -> TestResult {
        let mut options = DaemonForegroundOptions::new("/tmp/ee-daemon");
        options.tick_limit = MAX_DAEMON_FOREGROUND_TICK_LIMIT;
        options.interval_ms = MAX_DAEMON_FOREGROUND_INTERVAL_MS;

        validate_daemon_foreground_options(&options)
    }

    #[test]
    fn daemon_foreground_rejects_excessive_tick_limit() -> TestResult {
        let mut options = DaemonForegroundOptions::new("/tmp/ee-daemon");
        options.tick_limit = MAX_DAEMON_FOREGROUND_TICK_LIMIT + 1;

        let Err(error) = run_daemon_foreground(&options) else {
            return Err("excessive daemon tick limit accepted".to_owned());
        };
        if error.contains(&MAX_DAEMON_FOREGROUND_TICK_LIMIT.to_string()) {
            Ok(())
        } else {
            Err(format!("tick limit error omitted configured cap: {error}"))
        }
    }

    #[test]
    fn daemon_foreground_rejects_excessive_interval() -> TestResult {
        let mut options = DaemonForegroundOptions::new("/tmp/ee-daemon");
        options.interval_ms = MAX_DAEMON_FOREGROUND_INTERVAL_MS + 1;

        let Err(error) = run_daemon_foreground(&options) else {
            return Err("excessive daemon interval accepted".to_owned());
        };
        if error.contains(&MAX_DAEMON_FOREGROUND_INTERVAL_MS.to_string()) {
            Ok(())
        } else {
            Err(format!("interval error omitted configured cap: {error}"))
        }
    }

    #[test]
    fn manual_runner_schedule_and_run() -> TestResult {
        let opts = RunnerOptions::new();
        let mut runner = ManualRunner::new(opts);

        let job_id = runner.schedule(JobType::HealthCheck, JobPriority::Normal, None);
        assert!(!job_id.is_empty());

        let result = runner.run_job(&job_id, "2026-04-30T12:00:00Z");
        assert!(result.is_some());

        let result = result.ok_or_else(|| "manual runner result missing".to_string())?;
        assert_eq!(result.outcome, RunOutcome::Success);
        assert!(!result.dry_run);
        assert_eq!(result.items_processed, Some(0));
        let details = result
            .details
            .ok_or_else(|| "health-check details missing".to_owned())?;
        ensure(
            details["schema"].as_str(),
            Some("ee.steward.health_check.v1"),
            "health-check details schema",
        )?;
        ensure(
            details["jobType"].as_str(),
            Some("health_check"),
            "health-check job type",
        )
    }

    #[test]
    fn manual_runner_dry_run() -> TestResult {
        let opts = RunnerOptions::new().with_dry_run(true);
        let mut runner = ManualRunner::new(opts);

        let job_id = runner.schedule(JobType::HealthCheck, JobPriority::High, None);
        let result = runner
            .run_job(&job_id, "2026-04-30T12:00:00Z")
            .ok_or_else(|| "manual runner dry-run result missing".to_string())?;

        assert_eq!(result.outcome, RunOutcome::Success);
        assert!(result.dry_run);
        assert_eq!(result.items_processed, Some(0));
        let details = result
            .details
            .ok_or_else(|| "dry-run health-check details missing".to_owned())?;
        ensure(
            details["schema"].as_str(),
            Some("ee.steward.health_check.v1"),
            "dry-run health-check details schema",
        )
    }

    #[test]
    fn manual_runner_run_pending() {
        let opts = RunnerOptions::new();
        let mut runner = ManualRunner::new(opts);

        runner.schedule(JobType::HealthCheck, JobPriority::Low, None);
        runner.schedule(JobType::CachePruning, JobPriority::High, None);

        let report = runner.run_pending();

        assert_eq!(report.results.len(), 2);
        assert_eq!(report.succeeded, 2);
        assert_eq!(report.failed, 0);
        assert_eq!(report.skipped, 0);
        assert!(!report.was_cancelled);
    }

    #[test]
    fn manual_runner_run_job_type() {
        let opts = RunnerOptions::new();
        let mut runner = ManualRunner::new(opts);

        let result = runner.run_job_type(JobType::CachePruning, Some("manual test".to_owned()));

        assert_eq!(result.job_type, JobType::CachePruning);
        assert_eq!(result.outcome, RunOutcome::Success);
    }

    #[test]
    fn manual_runner_graph_snapshot_prune_uses_db_backed_report() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join("ee.db");
        {
            let connection =
                DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
            connection.migrate().map_err(|error| error.to_string())?;
            connection
                .insert_workspace(
                    SCORE_WORKSPACE_ID,
                    &CreateWorkspaceInput {
                        path: temp.path().to_string_lossy().into_owned(),
                        name: Some("graph-snapshot-prune-runner".to_owned()),
                    },
                )
                .map_err(|error| error.to_string())?;
            connection.close().map_err(|error| error.to_string())?;
        }
        let opts = RunnerOptions::new()
            .with_dry_run(true)
            .with_database_path(database_path)
            .with_workspace_id(SCORE_WORKSPACE_ID);
        let mut runner = ManualRunner::new(opts);

        let result = runner.run_job_type(
            JobType::GraphSnapshotPrune,
            Some("graph snapshot prune dry run".to_owned()),
        );

        ensure(result.job_type, JobType::GraphSnapshotPrune, "job type")?;
        ensure(result.outcome, RunOutcome::Success, "outcome")?;
        ensure(result.items_processed, Some(0), "items processed")?;
        let details = result
            .details
            .ok_or_else(|| "graph snapshot prune details missing".to_owned())?;
        ensure(
            details["schema"].as_str(),
            Some(GRAPH_SNAPSHOT_PRUNE_JOB_SCHEMA_V1),
            "schema",
        )?;
        ensure(
            details["workspaceId"].as_str(),
            Some(SCORE_WORKSPACE_ID),
            "workspace id",
        )?;
        ensure(
            details["lock"]["resourceType"].as_str(),
            Some("graph_snapshot_prune"),
            "lock resource type",
        )?;
        ensure(
            details["lock"]["acquired"].as_bool(),
            Some(false),
            "dry-run does not acquire advisory locks",
        )?;
        ensure(
            details["lock"]["holderId"].is_null(),
            true,
            "dry-run lock holder id",
        )?;
        ensure(
            details["degraded"].as_array().map(Vec::is_empty),
            Some(true),
            "no taxonomy-only degradation",
        )?;
        ensure(
            details["candidateCount"].as_u64(),
            Some(0),
            "candidate count",
        )?;

        Ok(())
    }

    #[test]
    fn manual_runner_graph_snapshot_prune_acquires_and_releases_locks() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join("ee.db");
        {
            let connection =
                DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
            connection.migrate().map_err(|error| error.to_string())?;
            connection
                .insert_workspace(
                    SCORE_WORKSPACE_ID,
                    &CreateWorkspaceInput {
                        path: temp.path().to_string_lossy().into_owned(),
                        name: Some("graph-snapshot-prune-locks".to_owned()),
                    },
                )
                .map_err(|error| error.to_string())?;
            connection.close().map_err(|error| error.to_string())?;
        }
        let opts = RunnerOptions::new()
            .with_database_path(database_path.clone())
            .with_workspace_id(SCORE_WORKSPACE_ID);
        let mut runner = ManualRunner::new(opts);

        let result = runner.run_job_type(
            JobType::GraphSnapshotPrune,
            Some("graph snapshot prune lock acquisition".to_owned()),
        );

        ensure(result.outcome, RunOutcome::Success, "outcome")?;
        let details = result
            .details
            .ok_or_else(|| "graph snapshot prune details missing".to_owned())?;
        ensure(
            details["lock"]["acquired"].as_bool(),
            Some(true),
            "lock acquired",
        )?;
        ensure(
            details["lock"]["holderId"].as_str().is_some(),
            true,
            "lock holder id",
        )?;

        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        ensure(
            connection
                .is_lock_held(&graph_snapshot_prune_lock_id(
                    SCORE_WORKSPACE_ID,
                    GraphSnapshotType::MemoryLinks,
                ))
                .map_err(|error| error.to_string())?
                .is_none(),
            true,
            "prune lock should release after the runner returns",
        )?;
        ensure(
            connection
                .is_lock_held(&graph_snapshot_refresh_lock_id(
                    SCORE_WORKSPACE_ID,
                    GraphSnapshotType::MemoryLinks,
                ))
                .map_err(|error| error.to_string())?
                .is_none(),
            true,
            "refresh-conflict lock should release after the runner returns",
        )?;
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn manual_runner_graph_snapshot_prune_conflicts_with_refresh_lock() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join("ee.db");
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                SCORE_WORKSPACE_ID,
                &CreateWorkspaceInput {
                    path: temp.path().to_string_lossy().into_owned(),
                    name: Some("graph-snapshot-prune-conflict".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        let refresh_lock_id =
            graph_snapshot_refresh_lock_id(SCORE_WORKSPACE_ID, GraphSnapshotType::MemoryLinks);
        match connection
            .acquire_advisory_lock(
                &refresh_lock_id,
                "refresh-holder",
                Some(GRAPH_SNAPSHOT_PRUNE_LOCK_TTL_SECS),
                Some("test refresh lock"),
            )
            .map_err(|error| error.to_string())?
        {
            AcquireLockResult::Acquired(_) | AcquireLockResult::Expired { .. } => {}
            AcquireLockResult::AlreadyHeld { holder_id, .. } => {
                return Err(format!(
                    "test refresh lock unexpectedly held by {holder_id}"
                ));
            }
        }

        let opts = RunnerOptions::new()
            .with_database_path(database_path.clone())
            .with_workspace_id(SCORE_WORKSPACE_ID);
        let mut runner = ManualRunner::new(opts);

        let result = runner.run_job_type(
            JobType::GraphSnapshotPrune,
            Some("graph snapshot prune lock conflict".to_owned()),
        );

        ensure(result.outcome, RunOutcome::Failed, "outcome")?;
        let details = result
            .details
            .ok_or_else(|| "graph snapshot prune error details missing".to_owned())?;
        ensure(
            details["code"].as_str(),
            Some("graph_snapshot_prune_lock_busy"),
            "lock conflict code",
        )?;
        ensure(
            details["message"]
                .as_str()
                .is_some_and(|message| message.contains("graph_snapshot:")),
            true,
            "lock conflict identifies refresh lock",
        )?;
        ensure(
            connection
                .is_lock_held(&graph_snapshot_prune_lock_id(
                    SCORE_WORKSPACE_ID,
                    GraphSnapshotType::MemoryLinks,
                ))
                .map_err(|error| error.to_string())?
                .is_none(),
            true,
            "prune lock should release when refresh lock acquisition fails",
        )?;
        ensure(
            connection
                .is_lock_held(&refresh_lock_id)
                .map_err(|error| error.to_string())?
                .is_some(),
            true,
            "pre-existing refresh lock should remain held",
        )?;
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn job_run_result_json() {
        let result = JobRunResult {
            job_id: "job-test".to_owned(),
            job_type: JobType::HealthCheck,
            outcome: RunOutcome::Success,
            duration_ms: 42,
            items_processed: Some(1),
            error: None,
            budget_summary: None,
            details: Some(json!({"schema": "test.details.v1"})),
            dry_run: false,
        };

        let json = result.data_json();
        assert_eq!(json["jobId"], "job-test");
        assert_eq!(json["outcome"], "success");
        assert_eq!(json["durationMs"], 42);
        assert_eq!(json["details"]["schema"], "test.details.v1");
    }

    #[test]
    fn runner_report_json_has_schema() {
        let report = RunnerReport {
            results: vec![],
            total_duration_ms: 0,
            succeeded: 0,
            failed: 0,
            skipped: 0,
            was_cancelled: false,
            started_at: "2026-04-30T12:00:00Z".to_owned(),
            completed_at: "2026-04-30T12:00:01Z".to_owned(),
        };

        let json = report.data_json();
        assert_eq!(json["schema"], RUNNER_REPORT_SCHEMA_V1);
        assert_eq!(json["command"], "steward run");
    }

    #[test]
    fn runner_report_all_succeeded() {
        let report_success = RunnerReport {
            results: vec![],
            total_duration_ms: 0,
            succeeded: 2,
            failed: 0,
            skipped: 0,
            was_cancelled: false,
            started_at: "2026-04-30T12:00:00Z".to_owned(),
            completed_at: "2026-04-30T12:00:01Z".to_owned(),
        };
        assert!(report_success.all_succeeded());

        let report_fail = RunnerReport {
            results: vec![],
            total_duration_ms: 0,
            succeeded: 1,
            failed: 1,
            skipped: 0,
            was_cancelled: false,
            started_at: "2026-04-30T12:00:00Z".to_owned(),
            completed_at: "2026-04-30T12:00:01Z".to_owned(),
        };
        assert!(!report_fail.all_succeeded());
    }

    #[test]
    fn runner_report_human_summary() {
        let report = RunnerReport {
            results: vec![JobRunResult {
                job_id: "job-001".to_owned(),
                job_type: JobType::HealthCheck,
                outcome: RunOutcome::Success,
                duration_ms: 10,
                items_processed: Some(1),
                error: None,
                budget_summary: None,
                details: None,
                dry_run: false,
            }],
            total_duration_ms: 10,
            succeeded: 1,
            failed: 0,
            skipped: 0,
            was_cancelled: false,
            started_at: "2026-04-30T12:00:00Z".to_owned(),
            completed_at: "2026-04-30T12:00:01Z".to_owned(),
        };

        let human = report.human_summary();
        assert!(human.contains("Steward Run Report"));
        assert!(human.contains("job-001"));
        assert!(human.contains("success"));
    }

    #[test]
    fn manual_runner_skip_completed_job() -> TestResult {
        let opts = RunnerOptions::new();
        let mut runner = ManualRunner::new(opts);

        let job_id = runner.schedule(JobType::HealthCheck, JobPriority::Normal, None);

        // Run once
        let _ = runner.run_job(&job_id, "2026-04-30T12:00:00Z");

        // Try to run again - should skip
        let result = runner
            .run_job(&job_id, "2026-04-30T12:00:01Z")
            .ok_or_else(|| "manual runner skipped result missing".to_string())?;
        assert_eq!(result.outcome, RunOutcome::Skipped);
        assert!(result.error.is_some());
        Ok(())
    }

    // ========================================================================
    // EE-244: Job Diagnostic Output Tests
    // ========================================================================

    #[test]
    fn job_diagnostic_schema_is_stable() -> TestResult {
        ensure(
            JOB_DIAGNOSTIC_SCHEMA_V1,
            "ee.steward.job_diagnostic.v1",
            "diagnostic schema constant",
        )
    }

    #[test]
    fn diagnostic_severity_as_str() -> TestResult {
        ensure(DiagnosticSeverity::Info.as_str(), "info", "info")?;
        ensure(DiagnosticSeverity::Warning.as_str(), "warning", "warning")?;
        ensure(DiagnosticSeverity::Error.as_str(), "error", "error")
    }

    #[test]
    fn health_status_as_str() -> TestResult {
        ensure(HealthStatus::Healthy.as_str(), "healthy", "healthy")?;
        ensure(HealthStatus::Degraded.as_str(), "degraded", "degraded")?;
        ensure(HealthStatus::Unhealthy.as_str(), "unhealthy", "unhealthy")
    }

    #[test]
    fn job_diagnostic_data_json() {
        let diag = JobDiagnostic::new("TEST_CODE", DiagnosticSeverity::Warning, "Test message")
            .with_suggestion("Do something")
            .for_job("job-001");

        let json = diag.data_json();

        assert_eq!(json["code"], "TEST_CODE");
        assert_eq!(json["severity"], "warning");
        assert_eq!(json["message"], "Test message");
        assert_eq!(json["suggestion"], "Do something");
        assert_eq!(json["jobId"], "job-001");
    }

    #[test]
    fn diagnose_empty_ledger() {
        let ledger = JobLedger::new();
        let report = diagnose_ledger(&ledger);

        assert_eq!(report.health, HealthStatus::Healthy);
        assert_eq!(report.summary.jobs_analyzed, 0);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == "STEWARD_LEDGER_EMPTY")
        );
    }

    #[test]
    fn diagnose_ledger_with_failed_job() {
        let mut ledger = JobLedger::new();
        let mut job = Job::new("job-001", JobType::HealthCheck, "2026-04-30T12:00:00Z");
        job.fail("2026-04-30T12:00:01Z", "Test failure");
        ledger.add_job(job);

        let report = diagnose_ledger(&ledger);

        assert_eq!(report.health, HealthStatus::Unhealthy);
        assert_eq!(report.summary.error_count, 1);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == "STEWARD_JOB_FAILED")
        );
    }

    #[test]
    fn diagnose_ledger_with_running_job() {
        let mut ledger = JobLedger::new();
        let mut job = Job::new("job-001", JobType::HealthCheck, "2026-04-30T12:00:00Z");
        job.start("2026-04-30T12:00:00Z");
        ledger.add_job(job);

        let report = diagnose_ledger(&ledger);

        assert_eq!(report.health, HealthStatus::Degraded);
        assert_eq!(report.summary.warning_count, 1);
    }

    #[test]
    fn diagnostic_report_json_has_required_fields() {
        let ledger = JobLedger::new();
        let report = diagnose_ledger(&ledger);
        let json = report.data_json();

        assert_eq!(json["schema"], JOB_DIAGNOSTIC_SCHEMA_V1);
        assert_eq!(json["command"], "steward diag");
        assert!(json["health"].is_string());
        assert!(json["summary"].is_object());
        assert!(json["diagnostics"].is_array());
    }

    #[test]
    fn diagnostic_report_human_summary() {
        let ledger = JobLedger::new();
        let report = diagnose_ledger(&ledger);
        let human = report.human_summary();

        assert!(human.contains("Job Diagnostics"));
        assert!(human.contains("Health:"));
        assert!(human.contains("Summary:"));
    }

    #[test]
    fn job_duration_calculated_from_timestamps() -> TestResult {
        // Bug: eidetic_engine_cli-s048 - was returning hardcoded zero
        let mut job = Job::new("job-dur-001", JobType::HealthCheck, "2026-04-30T12:00:00Z");
        job.start("2026-04-30T12:00:01.000Z");
        job.complete("2026-04-30T12:00:03.500Z", Some(10));

        ensure(job.duration_ms, Some(2500), "duration should be 2500ms")?;

        let json = job.data_json();
        ensure(
            json["durationMs"].as_u64(),
            Some(2500),
            "JSON durationMs should be 2500",
        )
    }

    #[test]
    fn job_duration_handles_failed_job() -> TestResult {
        let mut job = Job::new("job-dur-002", JobType::IndexRebuild, "2026-04-30T10:00:00Z");
        job.start("2026-04-30T10:00:00.100Z");
        job.fail("2026-04-30T10:00:05.600Z", "index corruption");

        ensure(
            job.duration_ms,
            Some(5500),
            "failed job duration should be 5500ms",
        )
    }

    #[test]
    fn job_duration_none_without_start() {
        let mut job = Job::new("job-dur-003", JobType::DecaySweep, "2026-04-30T08:00:00Z");
        // Complete without starting (edge case)
        job.completed_at = Some("2026-04-30T08:00:10Z".to_string());
        job.calculate_duration();

        assert!(
            job.duration_ms.is_none(),
            "duration should be None without started_at"
        );
    }
}
