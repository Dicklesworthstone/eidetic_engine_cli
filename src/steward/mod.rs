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
        Self { resource, limit, on_exceed }
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
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BudgetExceedAction {
    /// Log a warning and continue.
    Warn,
    /// Cancel the job immediately.
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

impl Default for BudgetExceedAction {
    fn default() -> Self {
        Self::Cancel
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
        self.consumption
            .entry(resource)
            .or_default()
            .add(amount);
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
                let consumed = self.consumption
                    .get(&resource)
                    .map_or(0, |c| c.consumed);
                b.limit.saturating_sub(consumed)
            })
    }

    /// Generate a summary report.
    #[must_use]
    pub fn summary(&self) -> BudgetSummary {
        let mut resources = Vec::new();

        for budget in &self.budgets {
            let consumption = self.consumption
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
        JobType::DecaySweep => vec![
            ResourceBudget::time_limit_ms(60_000), // 1 minute
            ResourceBudget::item_limit(10_000),
        ],
        JobType::CurationReview => vec![
            ResourceBudget::time_limit_ms(120_000), // 2 minutes
            ResourceBudget::item_limit(100),
        ],
        JobType::HealthCheck => vec![
            ResourceBudget::time_soft_limit_ms(10_000), // 10 seconds soft
        ],
        JobType::StorageCompact => vec![
            ResourceBudget::time_limit_ms(600_000), // 10 minutes
        ],
        JobType::CentralityRefresh => vec![
            ResourceBudget::time_limit_ms(180_000), // 3 minutes
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

/// Options for the manual runner.
#[derive(Clone, Debug)]
pub struct RunnerOptions {
    /// Maximum time budget in milliseconds (overrides job default).
    pub time_limit_ms: Option<u64>,
    /// Maximum items to process (overrides job default).
    pub item_limit: Option<u64>,
    /// Whether to perform a dry run (report what would happen).
    pub dry_run: bool,
    /// Whether to continue on non-fatal errors.
    pub continue_on_error: bool,
    /// Verbose diagnostics.
    pub verbose: bool,
}

impl Default for RunnerOptions {
    fn default() -> Self {
        Self {
            time_limit_ms: None,
            item_limit: None,
            dry_run: false,
            continue_on_error: false,
            verbose: false,
        }
    }
}

impl RunnerOptions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
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

        if let Some(items) = self.items_processed {
            obj["itemsProcessed"] = json!(items);
        }
        if let Some(ref error) = self.error {
            obj["error"] = json!(error);
        }
        if let Some(ref summary) = self.budget_summary {
            obj["budgetUsed"] = json!({
                "violations": summary.violations.len(),
                "warningCount": summary.warning_count,
            });
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

        out.push_str("\nNext:\n  ee steward jobs --json\n");
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
    pub fn schedule(&mut self, job_type: JobType, priority: JobPriority, context: Option<String>) -> String {
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
                dry_run: self.options.dry_run,
            });
        }

        if self.options.dry_run {
            return Some(JobRunResult {
                job_id: job_id.to_owned(),
                job_type,
                outcome: RunOutcome::Success,
                duration_ms: 0,
                items_processed: Some(0),
                error: None,
                budget_summary: None,
                dry_run: true,
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

        let (outcome, items, error) = self.execute_job_work(job_type, &mut budget);

        let completion_time = chrono::Utc::now().to_rfc3339();
        let job = self.ledger.get_job_mut(job_id)?;

        match outcome {
            RunOutcome::Success => job.complete(&completion_time, items),
            RunOutcome::Failed => job.fail(&completion_time, error.as_deref().unwrap_or("unknown error")),
            RunOutcome::Cancelled => job.cancel(&completion_time),
            RunOutcome::Skipped => job.skip(&completion_time, error.as_deref().unwrap_or("skipped")),
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
            dry_run: false,
        })
    }

    fn execute_job_work(
        &self,
        job_type: JobType,
        budget: &mut JobBudgetState,
    ) -> (RunOutcome, Option<u64>, Option<String>) {
        let simulated_items: u64 = match job_type {
            JobType::IndexRebuild => 100,
            JobType::DecaySweep => 50,
            JobType::CurationReview => 10,
            JobType::HealthCheck => 1,
            JobType::StorageCompact => 1,
            JobType::CentralityRefresh => 25,
            JobType::IntegrityCheck => 75,
            JobType::BackupExport => 1,
            JobType::GarbageCollection => 30,
            JobType::Custom => 5,
        };

        budget.record(ResourceType::Items, simulated_items);
        budget.record(ResourceType::TimeMs, 100);

        if budget.should_cancel() {
            return (
                RunOutcome::Cancelled,
                Some(simulated_items),
                Some("Budget exceeded".to_owned()),
            );
        }

        (RunOutcome::Success, Some(simulated_items), None)
    }

    /// Run all pending jobs in priority order.
    pub fn run_pending(&mut self) -> RunnerReport {
        let started_at = chrono::Utc::now().to_rfc3339();
        let mut results = Vec::new();
        let mut succeeded = 0u32;
        let mut failed = 0u32;
        let mut skipped = 0u32;
        let mut was_cancelled = false;

        let pending_ids: Vec<String> = self.ledger
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
            dry_run: self.options.dry_run,
        })
    }
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
        if let Some(ref suggestion) = self.suggestion {
            obj["suggestion"] = json!(suggestion);
        }
        if let Some(ref job_id) = self.job_id {
            obj["jobId"] = json!(job_id);
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
        summary.jobs_with_issues = jobs_with_issues.len() as u32;

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
        out.push_str(&format!("Summary:\n"));
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

        out.push_str("\nNext:\n  ee steward diag --json\n");
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
        let msg = job
            .error
            .as_deref()
            .unwrap_or("Unknown error");
        diagnostics.push(
            JobDiagnostic::new(
                "STEWARD_JOB_FAILED",
                DiagnosticSeverity::Error,
                format!("Job {} failed: {}", job.id, msg),
            )
            .with_suggestion("Review error and retry with `ee steward run`")
            .for_job(&job.id),
        );
    }

    // Check for high pending count
    if stats.pending > 10 {
        diagnostics.push(
            JobDiagnostic::new(
                "STEWARD_HIGH_PENDING",
                DiagnosticSeverity::Warning,
                format!("{} jobs pending - backlog may need attention", stats.pending),
            )
            .with_suggestion("Run `ee steward run --all` to process pending jobs"),
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
        assert_eq!(summary.resources[0].exceeded, true);
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
    }

    #[test]
    fn runner_options_builder() {
        let opts = RunnerOptions::new()
            .with_dry_run(true)
            .with_verbose(true)
            .with_time_limit(5000)
            .with_item_limit(100);

        assert!(opts.dry_run);
        assert!(opts.verbose);
        assert_eq!(opts.time_limit_ms, Some(5000));
        assert_eq!(opts.item_limit, Some(100));
    }

    #[test]
    fn manual_runner_schedule_and_run() {
        let opts = RunnerOptions::new();
        let mut runner = ManualRunner::new(opts);

        let job_id = runner.schedule(JobType::HealthCheck, JobPriority::Normal, None);
        assert!(!job_id.is_empty());

        let result = runner.run_job(&job_id, "2026-04-30T12:00:00Z");
        assert!(result.is_some());

        let result = result.unwrap();
        assert_eq!(result.outcome, RunOutcome::Success);
        assert!(!result.dry_run);
    }

    #[test]
    fn manual_runner_dry_run() {
        let opts = RunnerOptions::new().with_dry_run(true);
        let mut runner = ManualRunner::new(opts);

        let job_id = runner.schedule(JobType::DecaySweep, JobPriority::High, None);
        let result = runner.run_job(&job_id, "2026-04-30T12:00:00Z").unwrap();

        assert_eq!(result.outcome, RunOutcome::Success);
        assert!(result.dry_run);
        assert_eq!(result.duration_ms, 0);
    }

    #[test]
    fn manual_runner_run_pending() {
        let opts = RunnerOptions::new();
        let mut runner = ManualRunner::new(opts);

        runner.schedule(JobType::HealthCheck, JobPriority::Low, None);
        runner.schedule(JobType::DecaySweep, JobPriority::High, None);

        let report = runner.run_pending();

        assert_eq!(report.results.len(), 2);
        assert_eq!(report.succeeded, 2);
        assert_eq!(report.failed, 0);
        assert!(!report.was_cancelled);
    }

    #[test]
    fn manual_runner_run_job_type() {
        let opts = RunnerOptions::new();
        let mut runner = ManualRunner::new(opts);

        let result = runner.run_job_type(JobType::IntegrityCheck, Some("manual test".to_owned()));

        assert_eq!(result.job_type, JobType::IntegrityCheck);
        assert_eq!(result.outcome, RunOutcome::Success);
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
            dry_run: false,
        };

        let json = result.data_json();
        assert_eq!(json["jobId"], "job-test");
        assert_eq!(json["outcome"], "success");
        assert_eq!(json["durationMs"], 42);
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
    fn manual_runner_skip_completed_job() {
        let opts = RunnerOptions::new();
        let mut runner = ManualRunner::new(opts);

        let job_id = runner.schedule(JobType::HealthCheck, JobPriority::Normal, None);

        // Run once
        let _ = runner.run_job(&job_id, "2026-04-30T12:00:00Z");

        // Try to run again - should skip
        let result = runner.run_job(&job_id, "2026-04-30T12:00:01Z").unwrap();
        assert_eq!(result.outcome, RunOutcome::Skipped);
        assert!(result.error.is_some());
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
        assert!(report
            .diagnostics
            .iter()
            .any(|d| d.code == "STEWARD_LEDGER_EMPTY"));
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
        assert!(report
            .diagnostics
            .iter()
            .any(|d| d.code == "STEWARD_JOB_FAILED"));
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
}
