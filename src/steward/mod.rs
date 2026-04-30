//! Steward subsystem for maintenance jobs and lifecycle management.
//!
//! The steward manages background maintenance tasks like index rebuilds,
//! decay sweeps, curation reviews, and health checks. It operates in
//! CLI-first mode without requiring a daemon.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde_json::{Value as JsonValue, json};

pub const SUBSYSTEM: &str = "steward";

/// Schema identifier for job ledger reports.
pub const JOB_LEDGER_SCHEMA_V1: &str = "ee.steward.job_ledger.v1";

/// Schema identifier for individual job records.
pub const JOB_RECORD_SCHEMA_V1: &str = "ee.steward.job.v1";

#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

// ============================================================================
// EE-200: Job Ledger
// ============================================================================

/// Types of maintenance jobs the steward can execute.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum JobType {
    /// Rebuild search indexes from source of truth.
    IndexRebuild,
    /// Apply time-based decay to memory confidence.
    DecaySweep,
    /// Process pending curation candidates.
    CurationReview,
    /// Run health checks and generate diagnostics.
    HealthCheck,
    /// Compact and optimize storage.
    StorageCompact,
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
            Self::DecaySweep => "decay_sweep",
            Self::CurationReview => "curation_review",
            Self::HealthCheck => "health_check",
            Self::StorageCompact => "storage_compact",
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
            Self::DecaySweep,
            Self::CurationReview,
            Self::HealthCheck,
            Self::StorageCompact,
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
            Self::DecaySweep => "Apply time-based decay to memory confidence",
            Self::CurationReview => "Process pending curation candidates",
            Self::HealthCheck => "Run health checks and generate diagnostics",
            Self::StorageCompact => "Compact and optimize storage",
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
            "decay_sweep" => Ok(Self::DecaySweep),
            "curation_review" => Ok(Self::CurationReview),
            "health_check" => Ok(Self::HealthCheck),
            "storage_compact" => Ok(Self::StorageCompact),
            "centrality_refresh" => Ok(Self::CentralityRefresh),
            "integrity_check" => Ok(Self::IntegrityCheck),
            "backup_export" => Ok(Self::BackupExport),
            "garbage_collection" => Ok(Self::GarbageCollection),
            "custom" => Ok(Self::Custom),
            _ => Err(ParseJobTypeError { input: s.to_owned() }),
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
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled | Self::Skipped)
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
            _ => Err(ParseJobStatusError { input: s.to_owned() }),
        }
    }
}

/// Priority level for job scheduling.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum JobPriority {
    /// Background task, run when idle.
    Low,
    /// Normal priority.
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

impl Default for JobPriority {
    fn default() -> Self {
        Self::Normal
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
        // In a real implementation, parse timestamps and calculate
        // For now, this is a placeholder
        self.duration_ms = Some(0);
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

        if let Some(ref started) = self.started_at {
            obj["startedAt"] = json!(started);
        }
        if let Some(ref completed) = self.completed_at {
            obj["completedAt"] = json!(completed);
        }
        if let Some(duration) = self.duration_ms {
            obj["durationMs"] = json!(duration);
        }
        if let Some(ref error) = self.error {
            obj["error"] = json!(error);
        }
        if let Some(ref context) = self.context {
            obj["context"] = json!(context);
        }
        if let Some(items) = self.items_processed {
            obj["itemsProcessed"] = json!(items);
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
        self.jobs.values().filter(|j| j.job_type == job_type).collect()
    }

    /// Get pending jobs sorted by priority (highest first).
    #[must_use]
    pub fn pending_by_priority(&self) -> Vec<&Job> {
        let mut pending: Vec<_> = self.list_by_status(JobStatus::Pending);
        pending.sort_by(|a, b| b.priority.numeric().cmp(&a.priority.numeric()));
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

        out.push_str("\nNext:\n  ee steward jobs --json\n");
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

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
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
}
