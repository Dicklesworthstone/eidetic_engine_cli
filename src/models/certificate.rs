//! Certificate domain models (EE-340).
//!
//! Certificates are typed verification artifacts that prove a computation
//! or decision was performed correctly. They make "alien artifact math"
//! inspectable and auditable rather than opaque.
//!
//! Certificate types:
//! - Pack: context pack assembly verification
//! - Curation: curation decision verification
//! - TailRisk: risk assessment bounds
//! - PrivacyBudget: privacy budget consumption
//! - Lifecycle: lifecycle event verification

use std::fmt;
use std::str::FromStr;

/// Schema version for certificate JSON output.
pub const CERTIFICATE_SCHEMA_V1: &str = "ee.certificate.v1";

/// Certificate type discriminator.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CertificateKind {
    /// Context pack assembly certificate.
    Pack,
    /// Curation decision certificate.
    Curation,
    /// Tail-risk assessment certificate.
    TailRisk,
    /// Privacy budget consumption certificate.
    PrivacyBudget,
    /// Lifecycle event certificate.
    Lifecycle,
}

impl CertificateKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pack => "pack",
            Self::Curation => "curation",
            Self::TailRisk => "tail_risk",
            Self::PrivacyBudget => "privacy_budget",
            Self::Lifecycle => "lifecycle",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Pack,
            Self::Curation,
            Self::TailRisk,
            Self::PrivacyBudget,
            Self::Lifecycle,
        ]
    }
}

impl fmt::Display for CertificateKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error when parsing an invalid certificate kind string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseCertificateKindError {
    input: String,
}

impl ParseCertificateKindError {
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for ParseCertificateKindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown certificate kind `{}`; expected one of pack, curation, tail_risk, privacy_budget, lifecycle",
            self.input
        )
    }
}

impl std::error::Error for ParseCertificateKindError {}

impl FromStr for CertificateKind {
    type Err = ParseCertificateKindError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "pack" => Ok(Self::Pack),
            "curation" => Ok(Self::Curation),
            "tail_risk" => Ok(Self::TailRisk),
            "privacy_budget" => Ok(Self::PrivacyBudget),
            "lifecycle" => Ok(Self::Lifecycle),
            _ => Err(ParseCertificateKindError {
                input: input.to_owned(),
            }),
        }
    }
}

/// Certificate verification status.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CertificateStatus {
    /// Certificate is valid and verified.
    Valid,
    /// Certificate is pending verification.
    Pending,
    /// Certificate verification failed.
    Invalid,
    /// Certificate has expired.
    Expired,
    /// Certificate was revoked.
    Revoked,
}

impl CertificateStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Pending => "pending",
            Self::Invalid => "invalid",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Valid,
            Self::Pending,
            Self::Invalid,
            Self::Expired,
            Self::Revoked,
        ]
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Invalid | Self::Expired | Self::Revoked)
    }

    #[must_use]
    pub const fn is_usable(self) -> bool {
        matches!(self, Self::Valid)
    }
}

impl fmt::Display for CertificateStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error when parsing an invalid certificate status string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseCertificateStatusError {
    input: String,
}

impl ParseCertificateStatusError {
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for ParseCertificateStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown certificate status `{}`; expected one of valid, pending, invalid, expired, revoked",
            self.input
        )
    }
}

impl std::error::Error for ParseCertificateStatusError {}

impl FromStr for CertificateStatus {
    type Err = ParseCertificateStatusError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "valid" => Ok(Self::Valid),
            "pending" => Ok(Self::Pending),
            "invalid" => Ok(Self::Invalid),
            "expired" => Ok(Self::Expired),
            "revoked" => Ok(Self::Revoked),
            _ => Err(ParseCertificateStatusError {
                input: input.to_owned(),
            }),
        }
    }
}

/// Pack certificate payload - proves context pack assembly correctness.
#[derive(Clone, Debug, PartialEq)]
pub struct PackCertificate {
    /// Hash of the assembled pack content.
    pub pack_hash: String,
    /// Query that produced this pack.
    pub query: String,
    /// Token budget used.
    pub budget_used: u32,
    /// Token budget limit.
    pub budget_limit: u32,
    /// Number of items included.
    pub item_count: u32,
    /// Number of items omitted.
    pub omitted_count: u32,
    /// Section quotas were satisfied.
    pub quotas_satisfied: bool,
    /// Redundancy control was applied.
    pub redundancy_applied: bool,
    /// All items have provenance.
    pub provenance_complete: bool,
}

impl PackCertificate {
    /// Check if the pack certificate indicates a valid assembly.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.budget_used <= self.budget_limit && self.quotas_satisfied && self.provenance_complete
    }
}

/// Curation certificate payload - proves curation decision correctness.
#[derive(Clone, Debug, PartialEq)]
pub struct CurationCertificate {
    /// ID of the curation candidate.
    pub candidate_id: String,
    /// Type of curation action.
    pub action_type: String,
    /// Confidence score of the decision.
    pub confidence: f64,
    /// Minimum confidence threshold.
    pub threshold: f64,
    /// Evidence count supporting the decision.
    pub evidence_count: u32,
    /// Feedback events considered.
    pub feedback_count: u32,
    /// Human review required.
    pub requires_review: bool,
    /// Decision is reversible.
    pub reversible: bool,
}

impl CurationCertificate {
    /// Check if the curation meets the confidence threshold.
    #[must_use]
    pub fn meets_threshold(&self) -> bool {
        self.confidence >= self.threshold
    }

    /// Check if the curation has supporting evidence.
    #[must_use]
    pub const fn has_evidence(&self) -> bool {
        self.evidence_count > 0 || self.feedback_count > 0
    }
}

/// Tail-risk certificate payload - proves risk assessment bounds.
#[derive(Clone, Debug, PartialEq)]
pub struct TailRiskCertificate {
    /// Risk metric name.
    pub metric: String,
    /// Observed value.
    pub observed: f64,
    /// Threshold that triggers concern.
    pub threshold: f64,
    /// Confidence level (e.g., 0.95 for 95%).
    pub confidence_level: f64,
    /// Upper bound of the risk estimate.
    pub upper_bound: f64,
    /// Whether risk exceeds acceptable bounds.
    pub exceeds_bounds: bool,
    /// Recommended action if bounds exceeded.
    pub recommended_action: Option<String>,
}

impl TailRiskCertificate {
    /// Check if the risk is within acceptable bounds.
    #[must_use]
    pub const fn is_acceptable(&self) -> bool {
        !self.exceeds_bounds
    }

    /// Get the margin to threshold (positive = safe, negative = exceeded).
    #[must_use]
    pub fn margin(&self) -> f64 {
        self.threshold - self.observed
    }
}

/// Privacy budget certificate payload - proves privacy budget consumption.
#[derive(Clone, Debug, PartialEq)]
pub struct PrivacyBudgetCertificate {
    /// Budget category (e.g., "aggregation", "export").
    pub category: String,
    /// Budget consumed in this operation.
    pub consumed: f64,
    /// Total budget consumed so far.
    pub total_consumed: f64,
    /// Maximum allowed budget.
    pub budget_limit: f64,
    /// Remaining budget.
    pub remaining: f64,
    /// Whether operation was allowed.
    pub operation_allowed: bool,
    /// Reset timestamp if applicable.
    pub resets_at: Option<String>,
}

impl PrivacyBudgetCertificate {
    /// Check if budget is exhausted.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.remaining <= 0.0
    }

    /// Get utilization percentage (0.0 to 1.0+).
    #[must_use]
    pub fn utilization(&self) -> f64 {
        if self.budget_limit > 0.0 {
            self.total_consumed / self.budget_limit
        } else {
            0.0
        }
    }
}

// ============================================================================
// Shareable Aggregate Reports (EE-349)
// ============================================================================

/// Kind of shareable aggregate report.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ShareableAggregateKind {
    /// Count of items matching criteria.
    Count,
    /// Sum of numeric values.
    Sum,
    /// Mean/average value.
    Mean,
    /// Median value.
    Median,
    /// Standard deviation.
    StdDev,
    /// Histogram/distribution.
    Histogram,
    /// Percentile value.
    Percentile,
    /// Top-k items (with k-anonymity).
    TopK,
}

impl ShareableAggregateKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Count => "count",
            Self::Sum => "sum",
            Self::Mean => "mean",
            Self::Median => "median",
            Self::StdDev => "std_dev",
            Self::Histogram => "histogram",
            Self::Percentile => "percentile",
            Self::TopK => "top_k",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 8] {
        [
            Self::Count,
            Self::Sum,
            Self::Mean,
            Self::Median,
            Self::StdDev,
            Self::Histogram,
            Self::Percentile,
            Self::TopK,
        ]
    }

    #[must_use]
    pub const fn sensitivity_class(self) -> &'static str {
        match self {
            Self::Count => "bounded",
            Self::Sum | Self::Mean => "unbounded",
            Self::Median | Self::Percentile => "bounded",
            Self::StdDev => "unbounded",
            Self::Histogram => "bounded",
            Self::TopK => "k_anonymous",
        }
    }
}

impl fmt::Display for ShareableAggregateKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error when parsing an invalid shareable aggregate kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseShareableAggregateKindError {
    input: String,
}

impl ParseShareableAggregateKindError {
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for ParseShareableAggregateKindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown shareable aggregate kind `{}`; expected one of count, sum, mean, median, std_dev, histogram, percentile, top_k",
            self.input
        )
    }
}

impl std::error::Error for ParseShareableAggregateKindError {}

impl FromStr for ShareableAggregateKind {
    type Err = ParseShareableAggregateKindError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "count" => Ok(Self::Count),
            "sum" => Ok(Self::Sum),
            "mean" => Ok(Self::Mean),
            "median" => Ok(Self::Median),
            "std_dev" => Ok(Self::StdDev),
            "histogram" => Ok(Self::Histogram),
            "percentile" => Ok(Self::Percentile),
            "top_k" => Ok(Self::TopK),
            _ => Err(ParseShareableAggregateKindError {
                input: input.to_owned(),
            }),
        }
    }
}

/// Constraints for sharing aggregate reports.
#[derive(Clone, Debug, PartialEq)]
pub struct PrivacyBudgetShareConstraint {
    /// Minimum k for k-anonymity (records per group).
    pub k_anonymity_threshold: u32,
    /// Maximum epsilon for differential privacy.
    pub max_epsilon: f64,
    /// Maximum delta for differential privacy.
    pub max_delta: f64,
    /// Required noise mechanism (laplace, gaussian, exponential).
    pub noise_mechanism: String,
    /// Minimum sample size for statistical validity.
    pub min_sample_size: u32,
}

impl PrivacyBudgetShareConstraint {
    /// Default constraints for shareable aggregates.
    #[must_use]
    pub fn default_safe() -> Self {
        Self {
            k_anonymity_threshold: 5,
            max_epsilon: 1.0,
            max_delta: 1e-5,
            noise_mechanism: "laplace".to_string(),
            min_sample_size: 10,
        }
    }

    /// Strict constraints for high-sensitivity data.
    #[must_use]
    pub fn strict() -> Self {
        Self {
            k_anonymity_threshold: 10,
            max_epsilon: 0.1,
            max_delta: 1e-7,
            noise_mechanism: "gaussian".to_string(),
            min_sample_size: 50,
        }
    }

    /// Check if epsilon is within bounds.
    #[must_use]
    pub fn epsilon_valid(&self, epsilon: f64) -> bool {
        epsilon > 0.0 && epsilon <= self.max_epsilon
    }

    /// Check if delta is within bounds.
    #[must_use]
    pub fn delta_valid(&self, delta: f64) -> bool {
        delta > 0.0 && delta <= self.max_delta
    }
}

impl Default for PrivacyBudgetShareConstraint {
    fn default() -> Self {
        Self::default_safe()
    }
}

/// A shareable aggregate report with privacy guarantees.
#[derive(Clone, Debug, PartialEq)]
pub struct ShareableAggregateReport {
    /// Unique report identifier.
    pub report_id: String,
    /// Kind of aggregate computed.
    pub aggregate_kind: ShareableAggregateKind,
    /// The aggregate value (noised if differential privacy applied).
    pub value: f64,
    /// Original sample size before aggregation.
    pub sample_size: u32,
    /// Epsilon consumed for this report.
    pub epsilon_consumed: f64,
    /// Delta consumed for this report.
    pub delta_consumed: f64,
    /// Noise scale applied (if any).
    pub noise_scale: f64,
    /// Sensitivity bound used.
    pub sensitivity: f64,
    /// Whether the report meets k-anonymity requirements.
    pub k_anonymity_satisfied: bool,
    /// Whether the report is safe to share externally.
    pub shareable: bool,
    /// Reason if not shareable.
    pub share_denial_reason: Option<String>,
    /// Timestamp when report was generated.
    pub generated_at: String,
}

impl ShareableAggregateReport {
    /// Check if the report has valid privacy parameters.
    #[must_use]
    pub fn privacy_valid(&self) -> bool {
        self.epsilon_consumed > 0.0 && self.delta_consumed >= 0.0 && self.noise_scale >= 0.0
    }

    /// Check if report meets constraint requirements.
    #[must_use]
    pub fn meets_constraints(&self, constraint: &PrivacyBudgetShareConstraint) -> bool {
        self.k_anonymity_satisfied
            && constraint.epsilon_valid(self.epsilon_consumed)
            && constraint.delta_valid(self.delta_consumed)
            && self.sample_size >= constraint.min_sample_size
    }
}

/// Certificate proving an aggregate report is safe to share.
#[derive(Clone, Debug, PartialEq)]
pub struct PrivacyBudgetShareCertificate {
    /// Budget certificate showing consumption.
    pub budget: PrivacyBudgetCertificate,
    /// The aggregate report being certified.
    pub report: ShareableAggregateReport,
    /// Constraints used for validation.
    pub constraints: PrivacyBudgetShareConstraint,
    /// Whether certificate approves sharing.
    pub share_approved: bool,
    /// Validation checks performed.
    pub validations: Vec<ShareValidationCheck>,
    /// Certificate generation timestamp.
    pub certified_at: String,
}

impl PrivacyBudgetShareCertificate {
    /// Check if all validations passed.
    #[must_use]
    pub fn all_validations_passed(&self) -> bool {
        self.validations.iter().all(|v| v.passed)
    }

    /// Get failed validations.
    #[must_use]
    pub fn failed_validations(&self) -> Vec<&ShareValidationCheck> {
        self.validations.iter().filter(|v| !v.passed).collect()
    }

    /// Count of passed validations.
    #[must_use]
    pub fn passed_count(&self) -> usize {
        self.validations.iter().filter(|v| v.passed).count()
    }

    /// Count of total validations.
    #[must_use]
    pub fn total_count(&self) -> usize {
        self.validations.len()
    }
}

/// A single validation check in a share certificate.
#[derive(Clone, Debug, PartialEq)]
pub struct ShareValidationCheck {
    /// Check identifier.
    pub check_id: String,
    /// Human-readable check name.
    pub name: String,
    /// Whether check passed.
    pub passed: bool,
    /// Actual value observed.
    pub actual_value: String,
    /// Threshold or expected value.
    pub threshold: String,
    /// Explanation of result.
    pub explanation: String,
}

impl ShareValidationCheck {
    /// Create a passing check.
    #[must_use]
    pub fn pass(
        check_id: impl Into<String>,
        name: impl Into<String>,
        actual: impl Into<String>,
        threshold: impl Into<String>,
    ) -> Self {
        Self {
            check_id: check_id.into(),
            name: name.into(),
            passed: true,
            actual_value: actual.into(),
            threshold: threshold.into(),
            explanation: "Check passed".to_string(),
        }
    }

    /// Create a failing check.
    #[must_use]
    pub fn fail(
        check_id: impl Into<String>,
        name: impl Into<String>,
        actual: impl Into<String>,
        threshold: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            check_id: check_id.into(),
            name: name.into(),
            passed: false,
            actual_value: actual.into(),
            threshold: threshold.into(),
            explanation: reason.into(),
        }
    }
}

/// Lifecycle event type for lifecycle certificates.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LifecycleEvent {
    /// Import operation completed.
    Import,
    /// Index was published.
    IndexPublish,
    /// Hook was executed.
    HookExecution,
    /// Backup was created.
    Backup,
    /// Daemon shutdown.
    Shutdown,
    /// Migration completed.
    Migration,
    /// Maintenance job completed.
    Maintenance,
}

impl LifecycleEvent {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::IndexPublish => "index_publish",
            Self::HookExecution => "hook_execution",
            Self::Backup => "backup",
            Self::Shutdown => "shutdown",
            Self::Migration => "migration",
            Self::Maintenance => "maintenance",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 7] {
        [
            Self::Import,
            Self::IndexPublish,
            Self::HookExecution,
            Self::Backup,
            Self::Shutdown,
            Self::Migration,
            Self::Maintenance,
        ]
    }
}

impl fmt::Display for LifecycleEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error when parsing an invalid lifecycle event string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseLifecycleEventError {
    input: String,
}

impl ParseLifecycleEventError {
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for ParseLifecycleEventError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown lifecycle event `{}`; expected one of import, index_publish, hook_execution, backup, shutdown, migration, maintenance",
            self.input
        )
    }
}

impl std::error::Error for ParseLifecycleEventError {}

impl FromStr for LifecycleEvent {
    type Err = ParseLifecycleEventError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "import" => Ok(Self::Import),
            "index_publish" => Ok(Self::IndexPublish),
            "hook_execution" => Ok(Self::HookExecution),
            "backup" => Ok(Self::Backup),
            "shutdown" => Ok(Self::Shutdown),
            "migration" => Ok(Self::Migration),
            "maintenance" => Ok(Self::Maintenance),
            _ => Err(ParseLifecycleEventError {
                input: input.to_owned(),
            }),
        }
    }
}

/// Lifecycle certificate payload - proves lifecycle event completion.
#[derive(Clone, Debug, PartialEq)]
pub struct LifecycleCertificate {
    /// Type of lifecycle event.
    pub event: LifecycleEvent,
    /// Start timestamp.
    pub started_at: String,
    /// Completion timestamp.
    pub completed_at: String,
    /// Duration in milliseconds.
    pub duration_ms: u64,
    /// Whether the event succeeded.
    pub success: bool,
    /// Items processed (if applicable).
    pub items_processed: Option<u32>,
    /// Error message if failed.
    pub error: Option<String>,
    /// Idempotency key for replay detection.
    pub idempotency_key: Option<String>,
}

impl LifecycleCertificate {
    /// Check if the lifecycle event completed successfully.
    #[must_use]
    pub const fn is_successful(&self) -> bool {
        self.success
    }

    /// Check if this certificate can be used for replay detection.
    #[must_use]
    pub fn has_idempotency_key(&self) -> bool {
        self.idempotency_key.is_some()
    }
}

/// Certificate envelope that wraps any certificate type.
#[derive(Clone, Debug, PartialEq)]
pub struct Certificate {
    /// Unique certificate ID.
    pub id: String,
    /// Certificate kind discriminator.
    pub kind: CertificateKind,
    /// Certificate status.
    pub status: CertificateStatus,
    /// Workspace this certificate belongs to.
    pub workspace_id: String,
    /// When the certificate was issued.
    pub issued_at: String,
    /// When the certificate expires (if applicable).
    pub expires_at: Option<String>,
    /// Hash of the certificate payload for integrity.
    pub payload_hash: String,
    /// Decision-plane tracking metadata (EE-364).
    /// Links the certificate to the policy, decision, and trace that produced it.
    pub decision_metadata: super::decision::DecisionPlaneMetadata,
}

impl Certificate {
    /// Check if the certificate is currently usable.
    #[must_use]
    pub const fn is_usable(&self) -> bool {
        self.status.is_usable()
    }

    /// Check if the certificate has expired based on status.
    #[must_use]
    pub const fn is_expired(&self) -> bool {
        matches!(self.status, CertificateStatus::Expired)
    }
}

// ============================================================================
// Lifecycle Automaton Models (EE-350)
// ============================================================================

/// Schema version for lifecycle automaton certificates.
pub const LIFECYCLE_AUTOMATON_SCHEMA_V1: &str = "ee.lifecycle.automaton.v1";

/// State of a lifecycle automaton.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AutomatonState {
    /// Initial state before the process starts.
    Idle,
    /// Process is initializing.
    Initializing,
    /// Process is running.
    Running,
    /// Process is waiting for external input.
    Waiting,
    /// Process completed successfully.
    Completed,
    /// Process failed.
    Failed,
    /// Process was cancelled.
    Cancelled,
    /// Process is in cleanup/rollback.
    Rollback,
}

impl AutomatonState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Initializing => "initializing",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Rollback => "rollback",
        }
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(
            self,
            Self::Initializing | Self::Running | Self::Waiting | Self::Rollback
        )
    }

    #[must_use]
    pub const fn all() -> [Self; 8] {
        [
            Self::Idle,
            Self::Initializing,
            Self::Running,
            Self::Waiting,
            Self::Completed,
            Self::Failed,
            Self::Cancelled,
            Self::Rollback,
        ]
    }
}

impl fmt::Display for AutomatonState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A transition in the lifecycle automaton.
#[derive(Clone, Debug, PartialEq)]
pub struct AutomatonTransition {
    /// Source state.
    pub from: AutomatonState,
    /// Target state.
    pub to: AutomatonState,
    /// Transition trigger/event name.
    pub trigger: String,
    /// Timestamp of transition.
    pub timestamp: String,
    /// Optional transition metadata.
    pub metadata: Option<String>,
}

impl AutomatonTransition {
    #[must_use]
    pub fn new(from: AutomatonState, to: AutomatonState, trigger: impl Into<String>) -> Self {
        Self {
            from,
            to,
            trigger: trigger.into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            metadata: None,
        }
    }

    #[must_use]
    pub fn with_metadata(mut self, metadata: impl Into<String>) -> Self {
        self.metadata = Some(metadata.into());
        self
    }
}

/// Import lifecycle automaton certificate (EE-350).
#[derive(Clone, Debug, PartialEq)]
pub struct ImportAutomatonCertificate {
    /// Source type (cass, legacy, manual).
    pub source_type: String,
    /// Source path or identifier.
    pub source_id: String,
    /// Current automaton state.
    pub state: AutomatonState,
    /// State transition history.
    pub transitions: Vec<AutomatonTransition>,
    /// Sessions imported.
    pub sessions_imported: u32,
    /// Memories extracted.
    pub memories_extracted: u32,
    /// Items skipped.
    pub items_skipped: u32,
    /// Validation passed.
    pub validation_passed: bool,
    /// Idempotency fingerprint.
    pub idempotency_fingerprint: Option<String>,
}

impl ImportAutomatonCertificate {
    #[must_use]
    pub fn new(source_type: impl Into<String>, source_id: impl Into<String>) -> Self {
        Self {
            source_type: source_type.into(),
            source_id: source_id.into(),
            state: AutomatonState::Idle,
            transitions: Vec::new(),
            sessions_imported: 0,
            memories_extracted: 0,
            items_skipped: 0,
            validation_passed: false,
            idempotency_fingerprint: None,
        }
    }

    #[must_use]
    pub const fn is_complete(&self) -> bool {
        matches!(self.state, AutomatonState::Completed)
    }

    #[must_use]
    pub const fn is_successful(&self) -> bool {
        self.is_complete() && self.validation_passed
    }

    pub fn record_transition(&mut self, to: AutomatonState, trigger: impl Into<String>) {
        let transition = AutomatonTransition::new(self.state, to, trigger);
        self.transitions.push(transition);
        self.state = to;
    }
}

/// Index publish lifecycle automaton certificate (EE-350).
#[derive(Clone, Debug, PartialEq)]
pub struct IndexPublishAutomatonCertificate {
    /// Index type (fts5, vector, hybrid).
    pub index_type: String,
    /// Database generation before publish.
    pub db_generation_before: u64,
    /// Database generation after publish.
    pub db_generation_after: u64,
    /// Current automaton state.
    pub state: AutomatonState,
    /// State transition history.
    pub transitions: Vec<AutomatonTransition>,
    /// Documents indexed.
    pub documents_indexed: u32,
    /// Documents removed.
    pub documents_removed: u32,
    /// Index size in bytes.
    pub index_size_bytes: u64,
    /// Consistency check passed.
    pub consistency_check: bool,
    /// Publish timestamp.
    pub published_at: Option<String>,
}

impl IndexPublishAutomatonCertificate {
    #[must_use]
    pub fn new(index_type: impl Into<String>) -> Self {
        Self {
            index_type: index_type.into(),
            db_generation_before: 0,
            db_generation_after: 0,
            state: AutomatonState::Idle,
            transitions: Vec::new(),
            documents_indexed: 0,
            documents_removed: 0,
            index_size_bytes: 0,
            consistency_check: false,
            published_at: None,
        }
    }

    #[must_use]
    pub const fn is_complete(&self) -> bool {
        matches!(self.state, AutomatonState::Completed)
    }

    #[must_use]
    pub const fn generations_match(&self) -> bool {
        self.db_generation_after >= self.db_generation_before
    }

    pub fn record_transition(&mut self, to: AutomatonState, trigger: impl Into<String>) {
        let transition = AutomatonTransition::new(self.state, to, trigger);
        self.transitions.push(transition);
        self.state = to;
    }
}

/// Hook execution lifecycle automaton certificate (EE-350).
#[derive(Clone, Debug, PartialEq)]
pub struct HookAutomatonCertificate {
    /// Hook name/identifier.
    pub hook_name: String,
    /// Hook type (pre, post, on_error).
    pub hook_type: String,
    /// Triggering event.
    pub trigger_event: String,
    /// Current automaton state.
    pub state: AutomatonState,
    /// State transition history.
    pub transitions: Vec<AutomatonTransition>,
    /// Exit code if executed.
    pub exit_code: Option<i32>,
    /// Execution duration in milliseconds.
    pub duration_ms: u64,
    /// Output captured (truncated).
    pub output_summary: Option<String>,
    /// Hook was skipped.
    pub skipped: bool,
    /// Skip reason if skipped.
    pub skip_reason: Option<String>,
}

impl HookAutomatonCertificate {
    #[must_use]
    pub fn new(hook_name: impl Into<String>, hook_type: impl Into<String>) -> Self {
        Self {
            hook_name: hook_name.into(),
            hook_type: hook_type.into(),
            trigger_event: String::new(),
            state: AutomatonState::Idle,
            transitions: Vec::new(),
            exit_code: None,
            duration_ms: 0,
            output_summary: None,
            skipped: false,
            skip_reason: None,
        }
    }

    #[must_use]
    pub fn is_successful(&self) -> bool {
        matches!(self.state, AutomatonState::Completed)
            && self.exit_code.is_some_and(|code| code == 0)
    }

    #[must_use]
    pub const fn was_skipped(&self) -> bool {
        self.skipped
    }

    pub fn record_transition(&mut self, to: AutomatonState, trigger: impl Into<String>) {
        let transition = AutomatonTransition::new(self.state, to, trigger);
        self.transitions.push(transition);
        self.state = to;
    }
}

/// Backup lifecycle automaton certificate (EE-350).
#[derive(Clone, Debug, PartialEq)]
pub struct BackupAutomatonCertificate {
    /// Backup type (full, incremental, snapshot).
    pub backup_type: String,
    /// Backup destination path.
    pub destination: String,
    /// Current automaton state.
    pub state: AutomatonState,
    /// State transition history.
    pub transitions: Vec<AutomatonTransition>,
    /// Files backed up.
    pub files_count: u32,
    /// Total size in bytes.
    pub total_bytes: u64,
    /// Checksum of backup archive.
    pub checksum: Option<String>,
    /// Verification passed.
    pub verified: bool,
    /// Retention policy applied.
    pub retention_applied: bool,
    /// Old backups pruned.
    pub pruned_count: u32,
}

impl BackupAutomatonCertificate {
    #[must_use]
    pub fn new(backup_type: impl Into<String>, destination: impl Into<String>) -> Self {
        Self {
            backup_type: backup_type.into(),
            destination: destination.into(),
            state: AutomatonState::Idle,
            transitions: Vec::new(),
            files_count: 0,
            total_bytes: 0,
            checksum: None,
            verified: false,
            retention_applied: false,
            pruned_count: 0,
        }
    }

    #[must_use]
    pub const fn is_complete(&self) -> bool {
        matches!(self.state, AutomatonState::Completed)
    }

    #[must_use]
    pub const fn is_verified(&self) -> bool {
        self.is_complete() && self.verified
    }

    pub fn record_transition(&mut self, to: AutomatonState, trigger: impl Into<String>) {
        let transition = AutomatonTransition::new(self.state, to, trigger);
        self.transitions.push(transition);
        self.state = to;
    }
}

/// Shutdown lifecycle automaton certificate (EE-350).
#[derive(Clone, Debug, PartialEq)]
pub struct ShutdownAutomatonCertificate {
    /// Shutdown type (graceful, immediate, restart).
    pub shutdown_type: String,
    /// Shutdown reason.
    pub reason: String,
    /// Current automaton state.
    pub state: AutomatonState,
    /// State transition history.
    pub transitions: Vec<AutomatonTransition>,
    /// Pending operations at shutdown start.
    pub pending_operations: u32,
    /// Operations completed before shutdown.
    pub operations_completed: u32,
    /// Operations cancelled.
    pub operations_cancelled: u32,
    /// Cleanup tasks run.
    pub cleanup_tasks_run: u32,
    /// Final state persisted.
    pub state_persisted: bool,
    /// Connections closed cleanly.
    pub connections_closed: bool,
}

impl ShutdownAutomatonCertificate {
    #[must_use]
    pub fn new(shutdown_type: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            shutdown_type: shutdown_type.into(),
            reason: reason.into(),
            state: AutomatonState::Idle,
            transitions: Vec::new(),
            pending_operations: 0,
            operations_completed: 0,
            operations_cancelled: 0,
            cleanup_tasks_run: 0,
            state_persisted: false,
            connections_closed: false,
        }
    }

    #[must_use]
    pub const fn is_complete(&self) -> bool {
        matches!(self.state, AutomatonState::Completed)
    }

    #[must_use]
    pub const fn is_clean(&self) -> bool {
        self.is_complete() && self.state_persisted && self.connections_closed
    }

    #[must_use]
    pub const fn had_data_loss(&self) -> bool {
        self.operations_cancelled > 0 && !self.state_persisted
    }

    pub fn record_transition(&mut self, to: AutomatonState, trigger: impl Into<String>) {
        let transition = AutomatonTransition::new(self.state, to, trigger);
        self.transitions.push(transition);
        self.state = to;
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{
        CERTIFICATE_SCHEMA_V1, Certificate, CertificateKind, CertificateStatus,
        CurationCertificate, LifecycleCertificate, LifecycleEvent, PackCertificate,
        ParseCertificateKindError, ParseCertificateStatusError, ParseLifecycleEventError,
        ParseShareableAggregateKindError, PrivacyBudgetCertificate, PrivacyBudgetShareCertificate,
        PrivacyBudgetShareConstraint, ShareValidationCheck, ShareableAggregateKind,
        ShareableAggregateReport, TailRiskCertificate,
    };
    use crate::models::DecisionPlaneMetadata;

    type TestResult = Result<(), String>;

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
        ctx: &str,
    ) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn certificate_schema_is_stable() -> TestResult {
        ensure_equal(
            &CERTIFICATE_SCHEMA_V1,
            &"ee.certificate.v1",
            "schema version",
        )
    }

    #[test]
    fn certificate_kind_round_trip_for_every_variant() -> TestResult {
        for kind in CertificateKind::all() {
            let rendered = kind.to_string();
            let parsed = CertificateKind::from_str(&rendered)
                .map_err(|e| format!("kind {kind:?} failed to round-trip: {e}"))?;
            ensure_equal(&parsed, &kind, &format!("round-trip for {kind:?}"))?;
        }
        Ok(())
    }

    #[test]
    fn certificate_kind_rejects_unknown_input() -> TestResult {
        let err = CertificateKind::from_str("unknown_kind");
        ensure(
            matches!(err, Err(ParseCertificateKindError { .. })),
            "should reject unknown kind",
        )
    }

    #[test]
    fn certificate_status_round_trip_for_every_variant() -> TestResult {
        for status in CertificateStatus::all() {
            let rendered = status.to_string();
            let parsed = CertificateStatus::from_str(&rendered)
                .map_err(|e| format!("status {status:?} failed to round-trip: {e}"))?;
            ensure_equal(&parsed, &status, &format!("round-trip for {status:?}"))?;
        }
        Ok(())
    }

    #[test]
    fn certificate_status_rejects_unknown_input() -> TestResult {
        let err = CertificateStatus::from_str("unknown_status");
        ensure(
            matches!(err, Err(ParseCertificateStatusError { .. })),
            "should reject unknown status",
        )
    }

    #[test]
    fn certificate_status_terminal_and_usable() -> TestResult {
        ensure(
            !CertificateStatus::Valid.is_terminal(),
            "valid is not terminal",
        )?;
        ensure(
            !CertificateStatus::Pending.is_terminal(),
            "pending is not terminal",
        )?;
        ensure(
            CertificateStatus::Invalid.is_terminal(),
            "invalid is terminal",
        )?;
        ensure(
            CertificateStatus::Expired.is_terminal(),
            "expired is terminal",
        )?;
        ensure(
            CertificateStatus::Revoked.is_terminal(),
            "revoked is terminal",
        )?;

        ensure(CertificateStatus::Valid.is_usable(), "valid is usable")?;
        ensure(
            !CertificateStatus::Pending.is_usable(),
            "pending is not usable",
        )?;
        ensure(
            !CertificateStatus::Invalid.is_usable(),
            "invalid is not usable",
        )
    }

    #[test]
    fn lifecycle_event_round_trip_for_every_variant() -> TestResult {
        for event in LifecycleEvent::all() {
            let rendered = event.to_string();
            let parsed = LifecycleEvent::from_str(&rendered)
                .map_err(|e| format!("event {event:?} failed to round-trip: {e}"))?;
            ensure_equal(&parsed, &event, &format!("round-trip for {event:?}"))?;
        }
        Ok(())
    }

    #[test]
    fn lifecycle_event_rejects_unknown_input() -> TestResult {
        let err = LifecycleEvent::from_str("unknown_event");
        ensure(
            matches!(err, Err(ParseLifecycleEventError { .. })),
            "should reject unknown event",
        )
    }

    #[test]
    fn pack_certificate_validity_checks() -> TestResult {
        let valid = PackCertificate {
            pack_hash: "hash123".to_string(),
            query: "test query".to_string(),
            budget_used: 100,
            budget_limit: 200,
            item_count: 5,
            omitted_count: 2,
            quotas_satisfied: true,
            redundancy_applied: true,
            provenance_complete: true,
        };
        ensure(valid.is_valid(), "should be valid")?;

        let over_budget = PackCertificate {
            budget_used: 300,
            budget_limit: 200,
            ..valid.clone()
        };
        ensure(!over_budget.is_valid(), "over budget should be invalid")?;

        let no_provenance = PackCertificate {
            provenance_complete: false,
            ..valid.clone()
        };
        ensure(
            !no_provenance.is_valid(),
            "missing provenance should be invalid",
        )?;

        let quotas_failed = PackCertificate {
            quotas_satisfied: false,
            ..valid
        };
        ensure(!quotas_failed.is_valid(), "failed quotas should be invalid")
    }

    #[test]
    fn curation_certificate_threshold_checks() -> TestResult {
        let cert = CurationCertificate {
            candidate_id: "curate_123".to_string(),
            action_type: "promote".to_string(),
            confidence: 0.8,
            threshold: 0.7,
            evidence_count: 3,
            feedback_count: 5,
            requires_review: false,
            reversible: true,
        };
        ensure(cert.meets_threshold(), "0.8 >= 0.7 should meet threshold")?;
        ensure(cert.has_evidence(), "should have evidence")?;

        let below_threshold = CurationCertificate {
            confidence: 0.5,
            ..cert.clone()
        };
        ensure(
            !below_threshold.meets_threshold(),
            "0.5 < 0.7 should not meet threshold",
        )?;

        let no_evidence = CurationCertificate {
            evidence_count: 0,
            feedback_count: 0,
            ..cert
        };
        ensure(!no_evidence.has_evidence(), "should not have evidence")
    }

    #[test]
    fn tail_risk_certificate_bounds_checks() -> TestResult {
        let acceptable = TailRiskCertificate {
            metric: "latency_p99".to_string(),
            observed: 50.0,
            threshold: 100.0,
            confidence_level: 0.95,
            upper_bound: 75.0,
            exceeds_bounds: false,
            recommended_action: None,
        };
        ensure(acceptable.is_acceptable(), "should be acceptable")?;
        ensure(acceptable.margin() > 0.0, "margin should be positive")?;

        let exceeded = TailRiskCertificate {
            observed: 120.0,
            exceeds_bounds: true,
            recommended_action: Some("scale up".to_string()),
            ..acceptable
        };
        ensure(!exceeded.is_acceptable(), "should not be acceptable")?;
        ensure(exceeded.margin() < 0.0, "margin should be negative")
    }

    #[test]
    fn privacy_budget_certificate_utilization() -> TestResult {
        let cert = PrivacyBudgetCertificate {
            category: "aggregation".to_string(),
            consumed: 0.1,
            total_consumed: 0.5,
            budget_limit: 1.0,
            remaining: 0.5,
            operation_allowed: true,
            resets_at: None,
        };
        ensure(!cert.is_exhausted(), "should not be exhausted")?;
        ensure_equal(&cert.utilization(), &0.5, "utilization should be 50%")?;

        let exhausted = PrivacyBudgetCertificate {
            remaining: 0.0,
            total_consumed: 1.0,
            operation_allowed: false,
            ..cert
        };
        ensure(exhausted.is_exhausted(), "should be exhausted")?;
        ensure_equal(&exhausted.utilization(), &1.0, "utilization should be 100%")
    }

    #[test]
    fn shareable_aggregate_kind_round_trip_for_every_variant() -> TestResult {
        for kind in ShareableAggregateKind::all() {
            let rendered = kind.to_string();
            let parsed = ShareableAggregateKind::from_str(&rendered)
                .map_err(|e| format!("kind {kind:?} failed to round-trip: {e}"))?;
            ensure_equal(&parsed, &kind, &format!("round-trip for {kind:?}"))?;
        }
        Ok(())
    }

    #[test]
    fn shareable_aggregate_kind_rejects_unknown_input() -> TestResult {
        let err = ShareableAggregateKind::from_str("unknown_aggregate");
        ensure(
            matches!(err, Err(ParseShareableAggregateKindError { .. })),
            "should reject unknown aggregate kind",
        )
    }

    #[test]
    fn shareable_aggregate_kind_sensitivity_classes() -> TestResult {
        ensure_equal(
            &ShareableAggregateKind::Count.sensitivity_class(),
            &"bounded",
            "count sensitivity",
        )?;
        ensure_equal(
            &ShareableAggregateKind::Sum.sensitivity_class(),
            &"unbounded",
            "sum sensitivity",
        )?;
        ensure_equal(
            &ShareableAggregateKind::TopK.sensitivity_class(),
            &"k_anonymous",
            "top_k sensitivity",
        )
    }

    #[test]
    fn privacy_budget_share_constraint_defaults() -> TestResult {
        let safe = PrivacyBudgetShareConstraint::default_safe();
        ensure_equal(&safe.k_anonymity_threshold, &5, "default k-anonymity")?;
        ensure_equal(&safe.max_epsilon, &1.0, "default max epsilon")?;
        ensure(safe.epsilon_valid(0.5), "0.5 epsilon should be valid")?;
        ensure(!safe.epsilon_valid(1.5), "1.5 epsilon should be invalid")?;

        let strict = PrivacyBudgetShareConstraint::strict();
        ensure_equal(&strict.k_anonymity_threshold, &10, "strict k-anonymity")?;
        ensure_equal(&strict.max_epsilon, &0.1, "strict max epsilon")?;
        ensure(
            !strict.epsilon_valid(0.5),
            "0.5 epsilon should be invalid for strict",
        )
    }

    #[test]
    fn shareable_aggregate_report_privacy_validation() -> TestResult {
        let report = ShareableAggregateReport {
            report_id: "rpt_001".to_string(),
            aggregate_kind: ShareableAggregateKind::Mean,
            value: 42.5,
            sample_size: 100,
            epsilon_consumed: 0.1,
            delta_consumed: 1e-6,
            noise_scale: 0.5,
            sensitivity: 1.0,
            k_anonymity_satisfied: true,
            shareable: true,
            share_denial_reason: None,
            generated_at: "2026-04-30T12:00:00Z".to_string(),
        };

        ensure(
            report.privacy_valid(),
            "report should have valid privacy params",
        )?;
        ensure(
            report.meets_constraints(&PrivacyBudgetShareConstraint::default_safe()),
            "report should meet default constraints",
        )?;

        let invalid = ShareableAggregateReport {
            epsilon_consumed: 0.0,
            ..report.clone()
        };
        ensure(!invalid.privacy_valid(), "zero epsilon should be invalid")?;

        let below_min_sample = ShareableAggregateReport {
            sample_size: 5,
            ..report
        };
        ensure(
            !below_min_sample.meets_constraints(&PrivacyBudgetShareConstraint::default_safe()),
            "below min sample should fail constraints",
        )
    }

    #[test]
    fn privacy_budget_share_certificate_validation_checks() -> TestResult {
        let budget = PrivacyBudgetCertificate {
            category: "aggregation".to_string(),
            consumed: 0.1,
            total_consumed: 0.5,
            budget_limit: 1.0,
            remaining: 0.5,
            operation_allowed: true,
            resets_at: None,
        };

        let report = ShareableAggregateReport {
            report_id: "rpt_001".to_string(),
            aggregate_kind: ShareableAggregateKind::Count,
            value: 150.0,
            sample_size: 200,
            epsilon_consumed: 0.1,
            delta_consumed: 1e-6,
            noise_scale: 1.0,
            sensitivity: 1.0,
            k_anonymity_satisfied: true,
            shareable: true,
            share_denial_reason: None,
            generated_at: "2026-04-30T12:00:00Z".to_string(),
        };

        let cert = PrivacyBudgetShareCertificate {
            budget,
            report,
            constraints: PrivacyBudgetShareConstraint::default_safe(),
            share_approved: true,
            validations: vec![
                ShareValidationCheck::pass("k_anon", "K-Anonymity", "true", "5"),
                ShareValidationCheck::pass("epsilon", "Epsilon Budget", "0.1", "1.0"),
                ShareValidationCheck::pass("sample", "Sample Size", "200", "10"),
            ],
            certified_at: "2026-04-30T12:00:00Z".to_string(),
        };

        ensure(cert.all_validations_passed(), "all validations should pass")?;
        ensure_equal(&cert.passed_count(), &3, "passed count")?;
        ensure_equal(&cert.total_count(), &3, "total count")?;
        ensure(
            cert.failed_validations().is_empty(),
            "no failed validations",
        )?;

        let with_failure = PrivacyBudgetShareCertificate {
            share_approved: false,
            validations: vec![
                ShareValidationCheck::pass("k_anon", "K-Anonymity", "true", "5"),
                ShareValidationCheck::fail(
                    "epsilon",
                    "Epsilon Budget",
                    "1.5",
                    "1.0",
                    "Epsilon exceeds maximum allowed",
                ),
            ],
            ..cert
        };

        ensure(
            !with_failure.all_validations_passed(),
            "should have failures",
        )?;
        ensure_equal(
            &with_failure.passed_count(),
            &1,
            "passed count with failure",
        )?;
        ensure_equal(
            &with_failure.failed_validations().len(),
            &1,
            "failed validation count",
        )
    }

    #[test]
    fn lifecycle_certificate_success_checks() -> TestResult {
        let successful = LifecycleCertificate {
            event: LifecycleEvent::Import,
            started_at: "2026-04-29T12:00:00Z".to_string(),
            completed_at: "2026-04-29T12:01:00Z".to_string(),
            duration_ms: 60000,
            success: true,
            items_processed: Some(100),
            error: None,
            idempotency_key: Some("import-abc123".to_string()),
        };
        ensure(successful.is_successful(), "should be successful")?;
        ensure(
            successful.has_idempotency_key(),
            "should have idempotency key",
        )?;

        let failed = LifecycleCertificate {
            success: false,
            error: Some("connection timeout".to_string()),
            ..successful.clone()
        };
        ensure(!failed.is_successful(), "should not be successful")?;

        let no_key = LifecycleCertificate {
            idempotency_key: None,
            ..successful
        };
        ensure(
            !no_key.has_idempotency_key(),
            "should not have idempotency key",
        )
    }

    #[test]
    fn certificate_envelope_usability() -> TestResult {
        let valid = Certificate {
            id: "cert_123".to_string(),
            kind: CertificateKind::Pack,
            status: CertificateStatus::Valid,
            workspace_id: "wsp_456".to_string(),
            issued_at: "2026-04-29T12:00:00Z".to_string(),
            expires_at: None,
            payload_hash: "hash789".to_string(),
            decision_metadata: DecisionPlaneMetadata::empty(),
        };
        ensure(valid.is_usable(), "valid cert should be usable")?;
        ensure(!valid.is_expired(), "valid cert should not be expired")?;

        let expired = Certificate {
            status: CertificateStatus::Expired,
            ..valid
        };
        ensure(!expired.is_usable(), "expired cert should not be usable")?;
        ensure(
            expired.is_expired(),
            "expired cert should report as expired",
        )
    }

    // ========================================================================
    // Lifecycle Automaton Tests (EE-350)
    // ========================================================================

    #[test]
    fn automaton_state_terminal_and_active() -> TestResult {
        use super::AutomatonState;

        ensure(!AutomatonState::Idle.is_terminal(), "idle is not terminal")?;
        ensure(!AutomatonState::Idle.is_active(), "idle is not active")?;

        ensure(AutomatonState::Running.is_active(), "running is active")?;
        ensure(
            !AutomatonState::Running.is_terminal(),
            "running is not terminal",
        )?;

        ensure(
            AutomatonState::Completed.is_terminal(),
            "completed is terminal",
        )?;
        ensure(AutomatonState::Failed.is_terminal(), "failed is terminal")?;
        ensure(
            AutomatonState::Cancelled.is_terminal(),
            "cancelled is terminal",
        )?;

        ensure(
            !AutomatonState::Completed.is_active(),
            "completed is not active",
        )?;
        Ok(())
    }

    #[test]
    fn automaton_state_round_trip() -> TestResult {
        use super::AutomatonState;

        for state in AutomatonState::all() {
            let rendered = state.to_string();
            ensure(
                !rendered.is_empty(),
                format!("state {:?} should have string", state),
            )?;
        }
        Ok(())
    }

    #[test]
    fn import_automaton_certificate_transitions() -> TestResult {
        use super::{AutomatonState, ImportAutomatonCertificate};

        let mut cert = ImportAutomatonCertificate::new("cass", "/path/to/sessions");
        ensure_equal(&cert.state, &AutomatonState::Idle, "initial state")?;
        ensure(!cert.is_complete(), "should not be complete initially")?;

        cert.record_transition(AutomatonState::Initializing, "start_import");
        ensure_equal(&cert.state, &AutomatonState::Initializing, "after init")?;
        ensure_equal(&cert.transitions.len(), &1, "one transition")?;

        cert.record_transition(AutomatonState::Running, "begin_scan");
        cert.sessions_imported = 10;
        cert.memories_extracted = 50;

        cert.validation_passed = true;
        cert.record_transition(AutomatonState::Completed, "finish");
        ensure(cert.is_complete(), "should be complete")?;
        ensure(cert.is_successful(), "should be successful")
    }

    #[test]
    fn index_publish_automaton_certificate_generations() -> TestResult {
        use super::IndexPublishAutomatonCertificate;

        let mut cert = IndexPublishAutomatonCertificate::new("fts5");
        cert.db_generation_before = 10;
        cert.db_generation_after = 12;
        ensure(
            cert.generations_match(),
            "generations should match when after >= before",
        )?;

        cert.db_generation_after = 8;
        ensure(
            !cert.generations_match(),
            "should not match when after < before",
        )
    }

    #[test]
    fn hook_automaton_certificate_success_check() -> TestResult {
        use super::{AutomatonState, HookAutomatonCertificate};

        let mut cert = HookAutomatonCertificate::new("pre_commit", "pre");
        cert.trigger_event = "commit".to_owned();
        cert.record_transition(AutomatonState::Running, "execute");
        cert.exit_code = Some(0);
        cert.record_transition(AutomatonState::Completed, "finish");
        ensure(cert.is_successful(), "exit 0 + completed = successful")?;

        let mut failed_cert = HookAutomatonCertificate::new("pre_commit", "pre");
        failed_cert.record_transition(AutomatonState::Completed, "finish");
        failed_cert.exit_code = Some(1);
        ensure(!failed_cert.is_successful(), "exit 1 = not successful")
    }

    #[test]
    fn backup_automaton_certificate_verification() -> TestResult {
        use super::{AutomatonState, BackupAutomatonCertificate};

        let mut cert = BackupAutomatonCertificate::new("full", "/backups/daily");
        cert.files_count = 100;
        cert.total_bytes = 1024 * 1024;
        cert.checksum = Some("abc123".to_owned());
        cert.verified = true;
        cert.record_transition(AutomatonState::Completed, "finish");

        ensure(cert.is_complete(), "should be complete")?;
        ensure(cert.is_verified(), "should be verified")
    }

    #[test]
    fn shutdown_automaton_certificate_clean_check() -> TestResult {
        use super::{AutomatonState, ShutdownAutomatonCertificate};

        let mut cert = ShutdownAutomatonCertificate::new("graceful", "user_request");
        cert.pending_operations = 5;
        cert.operations_completed = 5;
        cert.cleanup_tasks_run = 3;
        cert.state_persisted = true;
        cert.connections_closed = true;
        cert.record_transition(AutomatonState::Completed, "shutdown_complete");

        ensure(cert.is_complete(), "should be complete")?;
        ensure(cert.is_clean(), "should be clean")?;
        ensure(!cert.had_data_loss(), "should not have data loss")?;

        let mut dirty = ShutdownAutomatonCertificate::new("immediate", "crash");
        dirty.operations_cancelled = 3;
        dirty.state_persisted = false;
        ensure(dirty.had_data_loss(), "should have data loss")
    }
}
