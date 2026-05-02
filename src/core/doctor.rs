//! Doctor command handler (EE-025, EE-241).
//!
//! Performs health checks on workspace subsystems and returns a structured
//! report with issues and repair suggestions.
//!
//! The `--fix-plan` flag (EE-241) outputs a structured repair plan that
//! agents can execute step-by-step.

use std::path::PathBuf;

use crate::core::agent_detect::{AgentInventoryReport, AgentInventoryStatus};
use crate::db::{
    CreateMemoryInput, DbConnection, ForeignKeyCheckResult, IntegrityCheckResult,
    ProvenanceSampleVerificationReport,
};
use crate::models::TrustClass;
use crate::models::error_codes::{self, ErrorCode};

pub const DEPENDENCY_DIAGNOSTICS_SCHEMA_V1: &str = "ee.diag.dependencies.v1";
pub const FRANKEN_HEALTH_SCHEMA_V1: &str = "ee.doctor.franken_health.v1";
pub const INTEGRITY_DIAGNOSTICS_SCHEMA_V1: &str = "ee.diag.integrity.v1";
pub const DEPENDENCY_MATRIX_REVISION: u32 = 1;
pub const DEPENDENCY_MATRIX_SOURCE_BEAD: &str = "eidetic_engine_cli-ilcq";
pub const DEPENDENCY_MATRIX_SOURCE_PLAN_ITEM: &str = "EE-307";
pub const DEPENDENCY_MATRIX_DEFAULT_FEATURE_PROFILE: &str = "default";
pub const INTEGRITY_CANARY_MEMORY_ID: &str = "mem_integritycanary00000000000";
const INTEGRITY_CANARY_CONTENT: &str = "EE integrity canary memory. Safe to ignore; verifies memory table write/read/provenance chain.";

pub const FORBIDDEN_CRATES: &[&str] = &[
    "tokio",
    "tokio-util",
    "async-std",
    "smol",
    "rusqlite",
    "sqlx",
    "diesel",
    "sea-orm",
    "petgraph",
    "hyper",
    "axum",
    "tower",
    "reqwest",
];

/// Severity of a doctor check issue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckSeverity {
    Ok,
    Warning,
    Error,
}

impl CheckSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }

    #[must_use]
    pub const fn is_healthy(self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// Result of a single health check.
#[derive(Clone, Debug)]
pub struct CheckResult {
    pub name: &'static str,
    pub severity: CheckSeverity,
    pub message: String,
    pub error_code: Option<ErrorCode>,
    pub repair: Option<&'static str>,
}

impl CheckResult {
    #[must_use]
    pub fn ok(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            severity: CheckSeverity::Ok,
            message: message.into(),
            error_code: None,
            repair: None,
        }
    }

    #[must_use]
    pub fn warning(name: &'static str, message: impl Into<String>, error_code: ErrorCode) -> Self {
        Self {
            name,
            severity: CheckSeverity::Warning,
            message: message.into(),
            error_code: Some(error_code),
            repair: error_code.default_repair,
        }
    }

    #[must_use]
    pub fn error(name: &'static str, message: impl Into<String>, error_code: ErrorCode) -> Self {
        Self {
            name,
            severity: CheckSeverity::Error,
            message: message.into(),
            error_code: Some(error_code),
            repair: error_code.default_repair,
        }
    }
}

/// Full doctor report.
#[derive(Clone, Debug)]
pub struct DoctorReport {
    pub version: &'static str,
    pub overall_healthy: bool,
    pub checks: Vec<CheckResult>,
}

impl DoctorReport {
    /// Run all health checks and return a report.
    #[must_use]
    pub fn gather() -> Self {
        let checks = vec![
            check_runtime(),
            check_workspace(),
            check_database(),
            check_search_index(),
            check_cass(),
        ];

        let overall_healthy = checks.iter().all(|c| c.severity.is_healthy());

        Self {
            version: env!("CARGO_PKG_VERSION"),
            overall_healthy,
            checks,
        }
    }

    /// Convert the doctor report into a structured fix plan.
    #[must_use]
    pub fn to_fix_plan(&self) -> FixPlan {
        self.to_fix_plan_with_agent_inventory(&AgentInventoryReport::not_inspected())
    }

    /// Convert the doctor report into a structured fix plan with optional
    /// agent-root guidance for CASS import dry runs.
    #[must_use]
    pub fn to_fix_plan_with_agent_inventory(
        &self,
        agent_inventory: &AgentInventoryReport,
    ) -> FixPlan {
        let steps: Vec<FixStep> = self
            .checks
            .iter()
            .filter(|c| !c.severity.is_healthy() && c.repair.is_some())
            .enumerate()
            .map(|(idx, check)| FixStep {
                order: idx + 1,
                subsystem: check.name,
                severity: check.severity,
                issue: check.message.clone(),
                error_code: check.error_code,
                command: check.repair.unwrap_or_default(),
            })
            .collect();

        let total_issues = self
            .checks
            .iter()
            .filter(|c| !c.severity.is_healthy())
            .count();
        let fixable_issues = steps.len();

        FixPlan {
            version: self.version,
            total_issues,
            fixable_issues,
            steps,
            cass_import_guidance: CassImportGuidance::from_agent_inventory(agent_inventory),
        }
    }
}

/// A structured repair plan generated from doctor checks.
#[derive(Clone, Debug)]
pub struct FixPlan {
    pub version: &'static str,
    pub total_issues: usize,
    pub fixable_issues: usize,
    pub steps: Vec<FixStep>,
    pub cass_import_guidance: CassImportGuidance,
}

impl FixPlan {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// A single repair step in a fix plan.
#[derive(Clone, Debug)]
pub struct FixStep {
    pub order: usize,
    pub subsystem: &'static str,
    pub severity: CheckSeverity,
    pub issue: String,
    pub error_code: Option<ErrorCode>,
    pub command: &'static str,
}

/// CASS import guidance status derived from agent detection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CassImportGuidanceStatus {
    AgentRootsDetected,
    NoAgentRootsDetected,
    NotInspected,
    Unavailable,
}

impl CassImportGuidanceStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentRootsDetected => "agent_roots_detected",
            Self::NoAgentRootsDetected => "no_agent_roots_detected",
            Self::NotInspected => "not_inspected",
            Self::Unavailable => "unavailable",
        }
    }
}

/// One detected local agent source root relevant to CASS import review.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CassImportRootGuidance {
    pub connector: String,
    pub root_path: String,
    pub guidance: String,
}

/// Agent-root guidance shown by `ee doctor --fix-plan`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CassImportGuidance {
    pub status: CassImportGuidanceStatus,
    pub detected_agent_count: usize,
    pub detected_root_count: usize,
    pub roots: Vec<CassImportRootGuidance>,
    pub suggested_commands: Vec<String>,
    pub message: String,
}

impl CassImportGuidance {
    #[must_use]
    pub fn from_agent_inventory(agent_inventory: &AgentInventoryReport) -> Self {
        let mut roots: Vec<CassImportRootGuidance> = agent_inventory
            .installed_agents
            .iter()
            .filter(|agent| agent.detected)
            .flat_map(|agent| {
                agent.root_paths.iter().map(|root_path| CassImportRootGuidance {
                    connector: agent.slug.clone(),
                    root_path: root_path.clone(),
                    guidance: format!(
                        "Review CASS dry-run coverage for {connector} history rooted at {root_path}.",
                        connector = agent.slug
                    ),
                })
            })
            .collect();
        roots.sort_by(|left, right| {
            left.connector
                .cmp(&right.connector)
                .then(left.root_path.cmp(&right.root_path))
        });

        let status = match agent_inventory.status {
            AgentInventoryStatus::Ready if roots.is_empty() => {
                CassImportGuidanceStatus::NoAgentRootsDetected
            }
            AgentInventoryStatus::Ready => CassImportGuidanceStatus::AgentRootsDetected,
            AgentInventoryStatus::Empty => CassImportGuidanceStatus::NoAgentRootsDetected,
            AgentInventoryStatus::NotInspected => CassImportGuidanceStatus::NotInspected,
            AgentInventoryStatus::Unavailable => CassImportGuidanceStatus::Unavailable,
        };

        let detected_root_count = roots.len();
        let suggested_commands = match status {
            CassImportGuidanceStatus::AgentRootsDetected => vec![
                "ee agent status --json".to_string(),
                "ee import cass --dry-run --json".to_string(),
                "ee import cass --json".to_string(),
            ],
            CassImportGuidanceStatus::NoAgentRootsDetected => vec![
                "ee agent scan --existing-only --json".to_string(),
                "ee import cass --dry-run --json".to_string(),
            ],
            CassImportGuidanceStatus::NotInspected => vec![
                "ee agent status --json".to_string(),
                "ee agent scan --existing-only --json".to_string(),
                "ee import cass --dry-run --json".to_string(),
            ],
            CassImportGuidanceStatus::Unavailable => vec![
                "ee agent sources --json".to_string(),
                "ee import cass --dry-run --json".to_string(),
            ],
        };

        let message = match status {
            CassImportGuidanceStatus::AgentRootsDetected => format!(
                "Detected {detected_root_count} local agent source root(s); run a CASS dry-run before importing evidence."
            ),
            CassImportGuidanceStatus::NoAgentRootsDetected => {
                "No local agent source roots were detected; CASS import can still report available sessions.".to_string()
            }
            CassImportGuidanceStatus::NotInspected => {
                "Agent source roots were not inspected for this fix plan; run agent status for root-level guidance.".to_string()
            }
            CassImportGuidanceStatus::Unavailable => {
                "Agent source root detection is unavailable; use the static source catalog and CASS dry-run output.".to_string()
            }
        };

        Self {
            status,
            detected_agent_count: agent_inventory.summary.detected_count,
            detected_root_count,
            roots,
            suggested_commands,
            message,
        }
    }
}

/// Options for `ee diag integrity`.
#[derive(Clone, Debug)]
pub struct IntegrityDiagnosticsOptions {
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
    pub workspace_id: String,
    pub sample_size: u32,
    pub create_canary: bool,
    pub dry_run: bool,
}

impl IntegrityDiagnosticsOptions {
    #[must_use]
    pub fn resolved_database_path(&self) -> PathBuf {
        self.database_path
            .clone()
            .unwrap_or_else(|| self.workspace_path.join(".ee").join("ee.db"))
    }
}

/// Overall integrity diagnostic posture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegrityDiagnosticsStatus {
    Ok,
    Degraded,
    Failed,
}

impl IntegrityDiagnosticsStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Degraded => "degraded",
            Self::Failed => "failed",
        }
    }
}

/// Severity for an integrity diagnostic check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegrityDiagnosticSeverity {
    Ok,
    Warning,
    Error,
}

impl IntegrityDiagnosticSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// A single integrity check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntegrityDiagnosticCheck {
    pub name: &'static str,
    pub severity: IntegrityDiagnosticSeverity,
    pub message: String,
    pub repair: Option<&'static str>,
}

impl IntegrityDiagnosticCheck {
    #[must_use]
    pub fn ok(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            severity: IntegrityDiagnosticSeverity::Ok,
            message: message.into(),
            repair: None,
        }
    }

    #[must_use]
    pub fn warning(
        name: &'static str,
        message: impl Into<String>,
        repair: Option<&'static str>,
    ) -> Self {
        Self {
            name,
            severity: IntegrityDiagnosticSeverity::Warning,
            message: message.into(),
            repair,
        }
    }

    #[must_use]
    pub fn error(
        name: &'static str,
        message: impl Into<String>,
        repair: Option<&'static str>,
    ) -> Self {
        Self {
            name,
            severity: IntegrityDiagnosticSeverity::Error,
            message: message.into(),
            repair,
        }
    }
}

/// Explicit canary-memory mutation posture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegrityCanaryStatus {
    NotRequested,
    DryRun,
    Created,
    AlreadyExists,
    Skipped,
    Failed,
}

impl IntegrityCanaryStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRequested => "not_requested",
            Self::DryRun => "dry_run",
            Self::Created => "created",
            Self::AlreadyExists => "already_exists",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }
}

/// Canary-memory creation result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntegrityCanaryReport {
    pub requested: bool,
    pub dry_run: bool,
    pub memory_id: &'static str,
    pub status: IntegrityCanaryStatus,
    pub message: String,
    pub repair: Option<&'static str>,
}

impl IntegrityCanaryReport {
    #[must_use]
    pub fn not_requested() -> Self {
        Self {
            requested: false,
            dry_run: false,
            memory_id: INTEGRITY_CANARY_MEMORY_ID,
            status: IntegrityCanaryStatus::NotRequested,
            message: "Canary memory creation was not requested.".to_string(),
            repair: Some("Use `ee diag integrity --create-canary --json` to write one."),
        }
    }
}

/// Non-fatal integrity diagnostic degradation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntegrityDiagnosticDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: Option<&'static str>,
}

/// Full `ee diag integrity` report.
#[derive(Clone, Debug)]
pub struct IntegrityDiagnosticsReport {
    pub version: &'static str,
    pub schema: &'static str,
    pub status: IntegrityDiagnosticsStatus,
    pub workspace_id: String,
    pub database_path: PathBuf,
    pub sample_size: u32,
    pub checks: Vec<IntegrityDiagnosticCheck>,
    pub provenance_sample: Option<ProvenanceSampleVerificationReport>,
    pub canary: IntegrityCanaryReport,
    pub degraded: Vec<IntegrityDiagnosticDegradation>,
}

impl IntegrityDiagnosticsReport {
    #[must_use]
    pub fn gather(options: &IntegrityDiagnosticsOptions) -> Self {
        let database_path = options.resolved_database_path();
        let mut checks = Vec::new();
        let mut degraded = Vec::new();
        let mut provenance_sample = None;

        if !database_path.exists() {
            checks.push(IntegrityDiagnosticCheck::warning(
                "database_exists",
                format!("Database not found at {}.", database_path.display()),
                Some("ee init --workspace ."),
            ));
            degraded.push(IntegrityDiagnosticDegradation {
                code: "integrity_database_missing",
                severity: "medium",
                message: "Integrity checks require an initialized ee database.".to_string(),
                repair: Some("ee init --workspace ."),
            });
            let canary = canary_for_missing_database(options);
            return Self::finalize(
                options,
                database_path,
                checks,
                provenance_sample,
                canary,
                degraded,
            );
        }

        checks.push(IntegrityDiagnosticCheck::ok(
            "database_exists",
            format!("Database found at {}.", database_path.display()),
        ));

        let connection = match DbConnection::open_file(&database_path) {
            Ok(connection) => connection,
            Err(error) => {
                checks.push(IntegrityDiagnosticCheck::error(
                    "database_open",
                    format!("Failed to open database: {error}"),
                    Some("ee doctor --json"),
                ));
                degraded.push(IntegrityDiagnosticDegradation {
                    code: "integrity_database_open_failed",
                    severity: "high",
                    message: "The database exists but could not be opened.".to_string(),
                    repair: Some("ee doctor --json"),
                });
                let canary = canary_for_open_failure(options);
                return Self::finalize(
                    options,
                    database_path,
                    checks,
                    provenance_sample,
                    canary,
                    degraded,
                );
            }
        };

        checks.push(IntegrityDiagnosticCheck::ok(
            "database_open",
            "Database opened through FrankenSQLite/SQLModel.",
        ));

        checks.push(check_sqlite_integrity(&connection));
        checks.push(check_foreign_keys(&connection));

        match connection.needs_migration() {
            Ok(false) => checks.push(IntegrityDiagnosticCheck::ok(
                "schema_current",
                "Database schema is current.",
            )),
            Ok(true) => {
                checks.push(IntegrityDiagnosticCheck::warning(
                    "schema_current",
                    "Database schema has pending migrations.",
                    Some("ee init --workspace ."),
                ));
                degraded.push(IntegrityDiagnosticDegradation {
                    code: "integrity_schema_migration_required",
                    severity: "medium",
                    message: "Integrity diagnostics require the current ee schema before sampling provenance or writing the canary."
                        .to_string(),
                    repair: Some("ee init --workspace ."),
                });
                let canary = canary_for_pending_migration(options);
                return Self::finalize(
                    options,
                    database_path,
                    checks,
                    provenance_sample,
                    canary,
                    degraded,
                );
            }
            Err(error) => {
                checks.push(IntegrityDiagnosticCheck::warning(
                    "schema_current",
                    format!("Could not inspect migration state: {error}"),
                    Some("ee doctor --json"),
                ));
                degraded.push(IntegrityDiagnosticDegradation {
                    code: "integrity_schema_check_unavailable",
                    severity: "medium",
                    message: "The database migration state could not be inspected.".to_string(),
                    repair: Some("ee doctor --json"),
                });
                let canary = canary_for_pending_migration(options);
                return Self::finalize(
                    options,
                    database_path,
                    checks,
                    provenance_sample,
                    canary,
                    degraded,
                );
            }
        }

        match connection
            .inspect_sampled_memory_provenance(&options.workspace_id, options.sample_size)
        {
            Ok(report) => {
                checks.push(check_provenance_sample(&report));
                provenance_sample = Some(report);
            }
            Err(error) => {
                checks.push(IntegrityDiagnosticCheck::warning(
                    "provenance_sample",
                    format!("Could not inspect sampled provenance chains: {error}"),
                    Some("ee diag integrity --json"),
                ));
                degraded.push(IntegrityDiagnosticDegradation {
                    code: "integrity_provenance_sample_unavailable",
                    severity: "medium",
                    message: "The provenance-chain sample could not be inspected.".to_string(),
                    repair: Some("ee diag integrity --json"),
                });
            }
        }

        let canary = maybe_create_canary(&connection, options);
        if canary.status == IntegrityCanaryStatus::Failed {
            checks.push(IntegrityDiagnosticCheck::warning(
                "canary_memory",
                canary.message.clone(),
                canary.repair,
            ));
        }

        Self::finalize(
            options,
            database_path,
            checks,
            provenance_sample,
            canary,
            degraded,
        )
    }

    #[must_use]
    pub fn success(&self) -> bool {
        self.status != IntegrityDiagnosticsStatus::Failed
    }

    fn finalize(
        options: &IntegrityDiagnosticsOptions,
        database_path: PathBuf,
        checks: Vec<IntegrityDiagnosticCheck>,
        provenance_sample: Option<ProvenanceSampleVerificationReport>,
        canary: IntegrityCanaryReport,
        degraded: Vec<IntegrityDiagnosticDegradation>,
    ) -> Self {
        let status = if checks
            .iter()
            .any(|check| check.severity == IntegrityDiagnosticSeverity::Error)
        {
            IntegrityDiagnosticsStatus::Failed
        } else if !degraded.is_empty()
            || checks
                .iter()
                .any(|check| check.severity == IntegrityDiagnosticSeverity::Warning)
        {
            IntegrityDiagnosticsStatus::Degraded
        } else {
            IntegrityDiagnosticsStatus::Ok
        };

        Self {
            version: env!("CARGO_PKG_VERSION"),
            schema: INTEGRITY_DIAGNOSTICS_SCHEMA_V1,
            status,
            workspace_id: options.workspace_id.clone(),
            database_path,
            sample_size: options.sample_size,
            checks,
            provenance_sample,
            canary,
            degraded,
        }
    }
}

fn canary_for_missing_database(options: &IntegrityDiagnosticsOptions) -> IntegrityCanaryReport {
    if !options.create_canary {
        return IntegrityCanaryReport::not_requested();
    }

    if options.dry_run {
        return IntegrityCanaryReport {
            requested: true,
            dry_run: true,
            memory_id: INTEGRITY_CANARY_MEMORY_ID,
            status: IntegrityCanaryStatus::DryRun,
            message: "Would create the integrity canary after the database exists.".to_string(),
            repair: Some("ee init --workspace ."),
        };
    }

    IntegrityCanaryReport {
        requested: true,
        dry_run: false,
        memory_id: INTEGRITY_CANARY_MEMORY_ID,
        status: IntegrityCanaryStatus::Skipped,
        message: "Canary creation skipped because the database is missing.".to_string(),
        repair: Some("ee init --workspace ."),
    }
}

fn canary_for_open_failure(options: &IntegrityDiagnosticsOptions) -> IntegrityCanaryReport {
    if !options.create_canary {
        return IntegrityCanaryReport::not_requested();
    }

    IntegrityCanaryReport {
        requested: true,
        dry_run: options.dry_run,
        memory_id: INTEGRITY_CANARY_MEMORY_ID,
        status: IntegrityCanaryStatus::Skipped,
        message: "Canary creation skipped because the database could not be opened.".to_string(),
        repair: Some("ee doctor --json"),
    }
}

fn canary_for_pending_migration(options: &IntegrityDiagnosticsOptions) -> IntegrityCanaryReport {
    if !options.create_canary {
        return IntegrityCanaryReport::not_requested();
    }

    IntegrityCanaryReport {
        requested: true,
        dry_run: options.dry_run,
        memory_id: INTEGRITY_CANARY_MEMORY_ID,
        status: IntegrityCanaryStatus::Skipped,
        message: "Canary creation skipped until database migrations are current.".to_string(),
        repair: Some("ee init --workspace ."),
    }
}

fn check_sqlite_integrity(connection: &DbConnection) -> IntegrityDiagnosticCheck {
    match connection.check_integrity() {
        Ok(IntegrityCheckResult { passed: true, .. }) => {
            IntegrityDiagnosticCheck::ok("sqlite_integrity", "SQLite integrity_check returned ok.")
        }
        Ok(IntegrityCheckResult { issues, .. }) => IntegrityDiagnosticCheck::error(
            "sqlite_integrity",
            format!("SQLite integrity_check reported {} issue(s).", issues.len()),
            Some("Restore from backup or inspect with sqlite integrity_check."),
        ),
        Err(error) => IntegrityDiagnosticCheck::error(
            "sqlite_integrity",
            format!("Failed to run SQLite integrity_check: {error}"),
            Some("ee doctor --json"),
        ),
    }
}

fn check_foreign_keys(connection: &DbConnection) -> IntegrityDiagnosticCheck {
    match connection.check_foreign_keys() {
        Ok(ForeignKeyCheckResult { passed: true, .. }) => IntegrityDiagnosticCheck::ok(
            "foreign_keys",
            "SQLite foreign_key_check returned no violations.",
        ),
        Ok(ForeignKeyCheckResult { violations, .. }) => IntegrityDiagnosticCheck::error(
            "foreign_keys",
            format!(
                "SQLite foreign_key_check reported {} violation(s).",
                violations.len()
            ),
            Some("Inspect foreign_key_check output before further writes."),
        ),
        Err(error) => IntegrityDiagnosticCheck::error(
            "foreign_keys",
            format!("Failed to run SQLite foreign_key_check: {error}"),
            Some("ee doctor --json"),
        ),
    }
}

fn check_provenance_sample(
    report: &ProvenanceSampleVerificationReport,
) -> IntegrityDiagnosticCheck {
    if report.is_clean() {
        IntegrityDiagnosticCheck::ok(
            "provenance_sample",
            format!(
                "Sampled {} memory provenance chain(s); all matched.",
                report.checked_count
            ),
        )
    } else {
        IntegrityDiagnosticCheck::warning(
            "provenance_sample",
            format!(
                "Sampled provenance found {} missing and {} mismatched chain hash(es).",
                report.missing_count, report.mismatch_count
            ),
            Some("ee diag integrity --create-canary --json"),
        )
    }
}

fn maybe_create_canary(
    connection: &DbConnection,
    options: &IntegrityDiagnosticsOptions,
) -> IntegrityCanaryReport {
    if !options.create_canary {
        return IntegrityCanaryReport::not_requested();
    }

    if options.dry_run {
        return IntegrityCanaryReport {
            requested: true,
            dry_run: true,
            memory_id: INTEGRITY_CANARY_MEMORY_ID,
            status: IntegrityCanaryStatus::DryRun,
            message: "Would create the integrity canary memory.".to_string(),
            repair: None,
        };
    }

    match connection.get_memory(INTEGRITY_CANARY_MEMORY_ID) {
        Ok(Some(_)) => IntegrityCanaryReport {
            requested: true,
            dry_run: false,
            memory_id: INTEGRITY_CANARY_MEMORY_ID,
            status: IntegrityCanaryStatus::AlreadyExists,
            message: "Integrity canary memory already exists.".to_string(),
            repair: None,
        },
        Ok(None) => insert_canary_memory(connection, &options.workspace_id),
        Err(error) => IntegrityCanaryReport {
            requested: true,
            dry_run: false,
            memory_id: INTEGRITY_CANARY_MEMORY_ID,
            status: IntegrityCanaryStatus::Failed,
            message: format!("Could not check for existing canary memory: {error}"),
            repair: Some("ee diag integrity --json"),
        },
    }
}

fn insert_canary_memory(connection: &DbConnection, workspace_id: &str) -> IntegrityCanaryReport {
    let input = CreateMemoryInput {
        workspace_id: workspace_id.to_string(),
        level: "semantic".to_string(),
        kind: "fact".to_string(),
        content: INTEGRITY_CANARY_CONTENT.to_string(),
        confidence: TrustClass::AgentAssertion.initial_confidence(),
        utility: 0.0,
        importance: 0.0,
        provenance_uri: Some("ee://diag/integrity/canary/v1".to_string()),
        trust_class: TrustClass::AgentAssertion.as_str().to_string(),
        trust_subclass: Some("integrity-canary".to_string()),
        tags: vec!["ee-canary".to_string(), "integrity".to_string()],
        valid_from: None,
        valid_to: None,
    };

    match connection.insert_memory(INTEGRITY_CANARY_MEMORY_ID, &input) {
        Ok(()) => IntegrityCanaryReport {
            requested: true,
            dry_run: false,
            memory_id: INTEGRITY_CANARY_MEMORY_ID,
            status: IntegrityCanaryStatus::Created,
            message: "Created integrity canary memory.".to_string(),
            repair: None,
        },
        Err(error) => IntegrityCanaryReport {
            requested: true,
            dry_run: false,
            memory_id: INTEGRITY_CANARY_MEMORY_ID,
            status: IntegrityCanaryStatus::Failed,
            message: format!("Failed to create integrity canary memory: {error}"),
            repair: Some("ee diag integrity --json"),
        },
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DependencySource {
    pub kind: &'static str,
    pub version: &'static str,
    pub path: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DependencyFeatureProfile {
    pub default_features: bool,
    pub features: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DependencyOptionalFeatureProfile {
    pub name: &'static str,
    pub features: &'static [&'static str],
    pub status: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DependencyBlockedFeature {
    pub name: &'static str,
    pub forbidden_crates: &'static [&'static str],
    pub action: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DependencyContractEntry {
    pub name: &'static str,
    pub kind: &'static str,
    pub owning_surface: &'static str,
    pub status: &'static str,
    pub enabled_by_default: bool,
    pub source: DependencySource,
    pub default_feature_profile: DependencyFeatureProfile,
    pub optional_feature_profiles: &'static [DependencyOptionalFeatureProfile],
    pub blocked_features: &'static [DependencyBlockedFeature],
    pub forbidden_transitive_dependencies: &'static [&'static str],
    pub minimum_smoke_test: &'static str,
    pub degradation_code: &'static str,
    pub status_fields: &'static [&'static str],
    pub diagnostic_command: &'static str,
    pub release_pin_decision: &'static str,
}

impl DependencyContractEntry {
    #[must_use]
    pub fn has_default_forbidden_transitives(self) -> bool {
        self.enabled_by_default
            && self
                .forbidden_transitive_dependencies
                .iter()
                .any(|candidate| FORBIDDEN_CRATES.contains(candidate))
    }

    #[must_use]
    pub fn readiness(self) -> &'static str {
        match (self.status, self.enabled_by_default) {
            ("accepted_default", true) | ("accepted_external", true) => "ready",
            ("optional_feature_gated", false) => "feature_gated",
            ("planned_not_linked", false) => "not_linked",
            _ => "review_required",
        }
    }

    #[must_use]
    pub fn is_franken_health_dependency(self) -> bool {
        matches!(
            self.name,
            "asupersync"
                | "frankensqlite"
                | "sqlmodel_rust"
                | "frankensearch"
                | "franken_networkx"
                | "franken_agent_detection"
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DependencyDriftPolicy {
    pub cargo_update_dry_run: &'static str,
    pub fail_conditions: &'static [&'static str],
    pub runtime_diagnostic_owner: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyDiagnosticsSummary {
    pub total_dependencies: usize,
    pub accepted_default_count: usize,
    pub accepted_external_count: usize,
    pub optional_feature_gated_count: usize,
    pub planned_not_linked_count: usize,
    pub default_enabled_count: usize,
    pub forbidden_default_hit_count: usize,
    pub blocked_feature_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyDiagnosticsReport {
    pub version: &'static str,
    pub schema: &'static str,
    pub matrix_revision: u32,
    pub source_bead: &'static str,
    pub source_plan_item: &'static str,
    pub default_feature_profile: &'static str,
    pub forbidden_crates: &'static [&'static str],
    pub entries: &'static [DependencyContractEntry],
    pub drift_policy: DependencyDriftPolicy,
    pub summary: DependencyDiagnosticsSummary,
}

impl DependencyDiagnosticsReport {
    #[must_use]
    pub fn gather() -> Self {
        let entries = DEPENDENCY_CONTRACT_ENTRIES;
        Self {
            version: env!("CARGO_PKG_VERSION"),
            schema: DEPENDENCY_DIAGNOSTICS_SCHEMA_V1,
            matrix_revision: DEPENDENCY_MATRIX_REVISION,
            source_bead: DEPENDENCY_MATRIX_SOURCE_BEAD,
            source_plan_item: DEPENDENCY_MATRIX_SOURCE_PLAN_ITEM,
            default_feature_profile: DEPENDENCY_MATRIX_DEFAULT_FEATURE_PROFILE,
            forbidden_crates: FORBIDDEN_CRATES,
            entries,
            drift_policy: DEPENDENCY_DRIFT_POLICY,
            summary: DependencyDiagnosticsSummary::from_entries(entries),
        }
    }
}

impl DependencyDiagnosticsSummary {
    #[must_use]
    pub fn from_entries(entries: &[DependencyContractEntry]) -> Self {
        Self {
            total_dependencies: entries.len(),
            accepted_default_count: count_status(entries, "accepted_default"),
            accepted_external_count: count_status(entries, "accepted_external"),
            optional_feature_gated_count: count_status(entries, "optional_feature_gated"),
            planned_not_linked_count: count_status(entries, "planned_not_linked"),
            default_enabled_count: entries
                .iter()
                .filter(|entry| entry.enabled_by_default)
                .count(),
            forbidden_default_hit_count: entries
                .iter()
                .filter(|entry| entry.has_default_forbidden_transitives())
                .count(),
            blocked_feature_count: entries
                .iter()
                .map(|entry| entry.blocked_features.len())
                .sum(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrankenHealthSummary {
    pub total_dependencies: usize,
    pub ready_count: usize,
    pub feature_gated_count: usize,
    pub not_linked_count: usize,
    pub default_enabled_count: usize,
    pub local_source_count: usize,
    pub forbidden_default_hit_count: usize,
    pub blocked_feature_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrankenDependencyHealth {
    pub name: &'static str,
    pub owning_surface: &'static str,
    pub status: &'static str,
    pub readiness: &'static str,
    pub enabled_by_default: bool,
    pub source: DependencySource,
    pub default_feature_profile: DependencyFeatureProfile,
    pub blocked_features: &'static [DependencyBlockedFeature],
    pub forbidden_transitive_dependencies: &'static [&'static str],
    pub degradation_code: &'static str,
    pub diagnostic_command: &'static str,
    pub minimum_smoke_test: &'static str,
    pub release_pin_decision: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrankenHealthReport {
    pub version: &'static str,
    pub schema: &'static str,
    pub healthy: bool,
    pub summary: FrankenHealthSummary,
    pub dependencies: Vec<FrankenDependencyHealth>,
}

impl FrankenHealthReport {
    #[must_use]
    pub fn gather() -> Self {
        let dependencies: Vec<FrankenDependencyHealth> = DEPENDENCY_CONTRACT_ENTRIES
            .iter()
            .copied()
            .filter(|entry| entry.is_franken_health_dependency())
            .map(FrankenDependencyHealth::from_entry)
            .collect();
        let summary = FrankenHealthSummary::from_dependencies(&dependencies);
        let healthy = summary.forbidden_default_hit_count == 0 && summary.not_linked_count == 0;

        Self {
            version: env!("CARGO_PKG_VERSION"),
            schema: FRANKEN_HEALTH_SCHEMA_V1,
            healthy,
            summary,
            dependencies,
        }
    }
}

impl FrankenDependencyHealth {
    #[must_use]
    pub fn from_entry(entry: DependencyContractEntry) -> Self {
        Self {
            name: entry.name,
            owning_surface: entry.owning_surface,
            status: entry.status,
            readiness: entry.readiness(),
            enabled_by_default: entry.enabled_by_default,
            source: entry.source,
            default_feature_profile: entry.default_feature_profile,
            blocked_features: entry.blocked_features,
            forbidden_transitive_dependencies: entry.forbidden_transitive_dependencies,
            degradation_code: entry.degradation_code,
            diagnostic_command: entry.diagnostic_command,
            minimum_smoke_test: entry.minimum_smoke_test,
            release_pin_decision: entry.release_pin_decision,
        }
    }
}

impl FrankenHealthSummary {
    #[must_use]
    pub fn from_dependencies(dependencies: &[FrankenDependencyHealth]) -> Self {
        Self {
            total_dependencies: dependencies.len(),
            ready_count: dependencies
                .iter()
                .filter(|dependency| dependency.readiness == "ready")
                .count(),
            feature_gated_count: dependencies
                .iter()
                .filter(|dependency| dependency.readiness == "feature_gated")
                .count(),
            not_linked_count: dependencies
                .iter()
                .filter(|dependency| dependency.readiness == "not_linked")
                .count(),
            default_enabled_count: dependencies
                .iter()
                .filter(|dependency| dependency.enabled_by_default)
                .count(),
            local_source_count: dependencies
                .iter()
                .filter(|dependency| {
                    matches!(dependency.source.kind, "path_dependency" | "path_patch")
                })
                .count(),
            forbidden_default_hit_count: dependencies
                .iter()
                .filter(|dependency| {
                    dependency.enabled_by_default
                        && dependency
                            .forbidden_transitive_dependencies
                            .iter()
                            .any(|candidate| FORBIDDEN_CRATES.contains(candidate))
                })
                .count(),
            blocked_feature_count: dependencies
                .iter()
                .map(|dependency| dependency.blocked_features.len())
                .sum(),
        }
    }
}

fn check_runtime() -> CheckResult {
    CheckResult::ok("runtime", "Asupersync runtime is available.")
}

fn check_workspace() -> CheckResult {
    CheckResult::warning(
        "workspace",
        "No workspace specified. Use --workspace or run from a workspace directory.",
        error_codes::WORKSPACE_NOT_SPECIFIED,
    )
}

fn check_database() -> CheckResult {
    CheckResult::warning(
        "database",
        "Database subsystem is not yet implemented.",
        error_codes::DATABASE_NOT_FOUND,
    )
}

fn check_search_index() -> CheckResult {
    CheckResult::warning(
        "search_index",
        "Search index subsystem is not yet implemented.",
        error_codes::INDEX_NOT_FOUND,
    )
}

fn check_cass() -> CheckResult {
    CheckResult::ok("cass", "CASS binary discovery is available.")
}

fn count_status(entries: &[DependencyContractEntry], status: &str) -> usize {
    entries
        .iter()
        .filter(|entry| entry.status == status)
        .count()
}

pub const DEPENDENCY_DRIFT_POLICY: DependencyDriftPolicy = DependencyDriftPolicy {
    cargo_update_dry_run: "advisory_only",
    fail_conditions: &[
        "introduces_forbidden_crate",
        "duplicates_franken_stack_family",
        "invalidates_accepted_feature_profile",
    ],
    runtime_diagnostic_owner: "EE-308",
};

pub const DEPENDENCY_CONTRACT_ENTRIES: &[DependencyContractEntry] = &[
    DependencyContractEntry {
        name: "asupersync",
        kind: "rust_crate",
        owning_surface: "ee-runtime",
        status: "accepted_default",
        enabled_by_default: true,
        source: DependencySource {
            kind: "registry",
            version: "0.3.1",
            path: "/dp/asupersync/asupersync",
        },
        default_feature_profile: DependencyFeatureProfile {
            default_features: false,
            features: &["tracing-integration"],
        },
        optional_feature_profiles: &[DependencyOptionalFeatureProfile {
            name: "deterministic-tests",
            features: &["deterministic-mode"],
            status: "test_only",
        }],
        blocked_features: &[DependencyBlockedFeature {
            name: "sqlite",
            forbidden_crates: &["rusqlite"],
            action: "do_not_enable_in_ee",
        }],
        forbidden_transitive_dependencies: &[],
        minimum_smoke_test: "runtime_status_reports_asupersync_engine",
        degradation_code: "runtime_unavailable",
        status_fields: &[
            "runtime.engine",
            "runtime.profile",
            "runtime.async_boundary",
        ],
        diagnostic_command: "ee status --json",
        release_pin_decision: "Registry version 0.3.1 is accepted; /dp/asupersync remains the local source reference for API checks.",
    },
    DependencyContractEntry {
        name: "frankensqlite",
        kind: "rust_crate_family",
        owning_surface: "ee-db",
        status: "accepted_default",
        enabled_by_default: true,
        source: DependencySource {
            kind: "path_patch",
            version: "0.1.2",
            path: "/data/projects/frankensqlite",
        },
        default_feature_profile: DependencyFeatureProfile {
            default_features: true,
            features: &["json", "fts5"],
        },
        optional_feature_profiles: &[DependencyOptionalFeatureProfile {
            name: "extended-sqlite-extensions",
            features: &["rtree", "session", "icu", "misc"],
            status: "not_in_default_profile",
        }],
        blocked_features: &[],
        forbidden_transitive_dependencies: &[],
        minimum_smoke_test: "default_feature_tree_excludes_forbidden_crates and migration tests",
        degradation_code: "storage_unavailable",
        status_fields: &["capabilities.storage", "degraded[].code"],
        diagnostic_command: "ee doctor --json",
        release_pin_decision: "Local path patches are accepted only for development; release must record a registry pin or ADR-backed local source policy.",
    },
    DependencyContractEntry {
        name: "sqlmodel_rust",
        kind: "rust_crate_family",
        owning_surface: "ee-db",
        status: "accepted_default",
        enabled_by_default: true,
        source: DependencySource {
            kind: "path_dependency",
            version: "0.2.2",
            path: "/data/projects/sqlmodel_rust",
        },
        default_feature_profile: DependencyFeatureProfile {
            default_features: true,
            features: &["sqlmodel-core", "sqlmodel-frankensqlite"],
        },
        optional_feature_profiles: &[],
        blocked_features: &[DependencyBlockedFeature {
            name: "c-sqlite-tests",
            forbidden_crates: &["rusqlite"],
            action: "parity_only_do_not_enable_in_ee",
        }],
        forbidden_transitive_dependencies: &[],
        minimum_smoke_test: "migration_sequence_is_contiguous and repository tests",
        degradation_code: "storage_unavailable",
        status_fields: &["capabilities.storage", "database.schema_version"],
        diagnostic_command: "ee db status --json",
        release_pin_decision: "Local path dependencies are accepted only for development; release must record a registry pin or ADR-backed local source policy.",
    },
    DependencyContractEntry {
        name: "frankensearch",
        kind: "rust_crate_family",
        owning_surface: "ee-search",
        status: "accepted_default",
        enabled_by_default: true,
        source: DependencySource {
            kind: "path_dependency",
            version: "0.3.0",
            path: "/data/projects/frankensearch",
        },
        default_feature_profile: DependencyFeatureProfile {
            default_features: false,
            features: &["hash", "storage", "model2vec", "lexical", "fts5"],
        },
        optional_feature_profiles: &[],
        blocked_features: &[
            DependencyBlockedFeature {
                name: "fastembed",
                forbidden_crates: &["tokio", "tokio-util", "hyper", "tower", "reqwest"],
                action: "block_embed_quality_until_upstream_has_clean_local_profile",
            },
            DependencyBlockedFeature {
                name: "download_api",
                forbidden_crates: &["reqwest"],
                action: "no_network_stack_in_core",
            },
        ],
        forbidden_transitive_dependencies: &[],
        minimum_smoke_test: "search/index smoke tests with deterministic hash embeddings",
        degradation_code: "search_unavailable",
        status_fields: &["capabilities.search", "index.generation", "degraded[].code"],
        diagnostic_command: "ee index status --json",
        release_pin_decision: "Local path dependencies are accepted only for development; release must record a registry pin or ADR-backed local source policy.",
    },
    DependencyContractEntry {
        name: "franken_networkx",
        kind: "rust_crate_family",
        owning_surface: "ee-graph",
        status: "optional_feature_gated",
        enabled_by_default: false,
        source: DependencySource {
            kind: "path_dependency",
            version: "0.1.0",
            path: "/data/projects/franken_networkx",
        },
        default_feature_profile: DependencyFeatureProfile {
            default_features: false,
            features: &[],
        },
        optional_feature_profiles: &[DependencyOptionalFeatureProfile {
            name: "graph",
            features: &[
                "fnx-runtime/asupersync-integration",
                "fnx-classes",
                "fnx-algorithms",
            ],
            status: "accepted_optional",
        }],
        blocked_features: &[DependencyBlockedFeature {
            name: "ftui-integration",
            forbidden_crates: &[],
            action: "not_part_of_ee_graph_contract",
        }],
        forbidden_transitive_dependencies: &[],
        minimum_smoke_test: "graph projection and centrality tests",
        degradation_code: "graph_unavailable",
        status_fields: &["capabilities.graph", "graph.snapshot_generation"],
        diagnostic_command: "ee graph status --json",
        release_pin_decision: "Local path dependencies are accepted only for development; release must record a registry pin or ADR-backed local source policy.",
    },
    DependencyContractEntry {
        name: "coding_agent_session_search",
        kind: "external_process",
        owning_surface: "ee-cass",
        status: "accepted_external",
        enabled_by_default: true,
        source: DependencySource {
            kind: "external_binary",
            version: "0.4.1",
            path: "/dp/coding_agent_session_search",
        },
        default_feature_profile: DependencyFeatureProfile {
            default_features: false,
            features: &["cass --robot", "cass --json"],
        },
        optional_feature_profiles: &[],
        blocked_features: &[DependencyBlockedFeature {
            name: "interactive-output",
            forbidden_crates: &[],
            action: "never_parse_bare_cass_output",
        }],
        forbidden_transitive_dependencies: &[],
        minimum_smoke_test: "CASS fixture parsing for capabilities, health, and API version",
        degradation_code: "cass_unavailable",
        status_fields: &["capabilities.cass", "degraded[].code"],
        diagnostic_command: "ee import cass --dry-run --json",
        release_pin_decision: "External process contract is accepted; no Rust crate is linked into ee.",
    },
    DependencyContractEntry {
        name: "toon_rust",
        kind: "rust_crate",
        owning_surface: "ee-output",
        status: "accepted_default",
        enabled_by_default: true,
        source: DependencySource {
            kind: "path_dependency",
            version: "0.2.2",
            path: "/data/projects/toon_rust",
        },
        default_feature_profile: DependencyFeatureProfile {
            default_features: false,
            features: &[],
        },
        optional_feature_profiles: &[],
        blocked_features: &[],
        forbidden_transitive_dependencies: &[],
        minimum_smoke_test: "TOON renderer round-trip and golden parity tests",
        degradation_code: "toon_unavailable",
        status_fields: &["capabilities.output.toon"],
        diagnostic_command: "ee status --json",
        release_pin_decision: "Local path dependency is accepted only for development; release must record a registry pin or ADR-backed local source policy.",
    },
    DependencyContractEntry {
        name: "franken_mermaid",
        kind: "planned_rust_crate",
        owning_surface: "ee-diagram",
        status: "planned_not_linked",
        enabled_by_default: false,
        source: DependencySource {
            kind: "not_linked",
            version: "unresolved",
            path: "/dp/franken_mermaid",
        },
        default_feature_profile: DependencyFeatureProfile {
            default_features: false,
            features: &[],
        },
        optional_feature_profiles: &[DependencyOptionalFeatureProfile {
            name: "franken-mermaid-adapter",
            features: &["diagram-validation"],
            status: "blocked_until_repository_api_and_dependency_audit",
        }],
        blocked_features: &[DependencyBlockedFeature {
            name: "browser-or-network-renderer",
            forbidden_crates: &["tokio", "hyper", "axum", "tower", "reqwest"],
            action: "plain_mermaid_text_remains_the_default_until_adapter_tree_is_clean",
        }],
        forbidden_transitive_dependencies: &[],
        minimum_smoke_test: "Gate 11 Mermaid goldens plus future FrankenMermaid adapter cargo-tree audit",
        degradation_code: "diagram_backend_unavailable",
        status_fields: &["capabilities.output.diagram", "degraded[].code"],
        diagnostic_command: "ee doctor --json",
        release_pin_decision: "Do not link before /dp/franken_mermaid exists, its API is audited, and a forbidden-dependency cargo-tree gate passes.",
    },
    DependencyContractEntry {
        name: "franken_agent_detection",
        kind: "rust_crate",
        owning_surface: "ee-agent-detect",
        status: "accepted_default",
        enabled_by_default: true,
        source: DependencySource {
            kind: "path_dependency",
            version: "0.1.3",
            path: "/data/projects/franken_agent_detection",
        },
        default_feature_profile: DependencyFeatureProfile {
            default_features: false,
            features: &[],
        },
        optional_feature_profiles: &[],
        blocked_features: &[DependencyBlockedFeature {
            name: "connector-backed-scans",
            forbidden_crates: &[],
            action: "requires_privacy_and_dependency_gates_before_default_use",
        }],
        forbidden_transitive_dependencies: &[],
        minimum_smoke_test: "agent detection fixture tests with root overrides",
        degradation_code: "agent_detection_unavailable",
        status_fields: &["capabilities.agent_detection"],
        diagnostic_command: "ee agent sources --json",
        release_pin_decision: "Local path dependency is accepted only for development; release must record a registry pin or ADR-backed local source policy.",
    },
    DependencyContractEntry {
        name: "fastmcp-rust",
        kind: "planned_rust_crate",
        owning_surface: "ee-mcp",
        status: "planned_not_linked",
        enabled_by_default: false,
        source: DependencySource {
            kind: "not_linked",
            version: "unresolved",
            path: "/dp/fastmcp-rust",
        },
        default_feature_profile: DependencyFeatureProfile {
            default_features: false,
            features: &[],
        },
        optional_feature_profiles: &[DependencyOptionalFeatureProfile {
            name: "mcp",
            features: &["stdio"],
            status: "blocked_until_dependency_audit",
        }],
        blocked_features: &[],
        forbidden_transitive_dependencies: &[],
        minimum_smoke_test: "MCP stdio initialize/tools/resources golden tests",
        degradation_code: "mcp_unavailable",
        status_fields: &["capabilities.mcp"],
        diagnostic_command: "ee doctor --json",
        release_pin_decision: "Do not link before a clean feature-tree audit and ADR-backed release pin.",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;
    const TEST_WORKSPACE_ID: &str = "wsp_01234567890123456789012345";

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn doctor_report_gather_returns_checks() -> TestResult {
        let report = DoctorReport::gather();

        ensure(
            report.checks.len() >= 5,
            true,
            "should have at least 5 checks",
        )?;

        let runtime = report.checks.iter().find(|c| c.name == "runtime");
        ensure(runtime.is_some(), true, "runtime check exists")?;
        ensure(
            runtime.map(|c| c.severity),
            Some(CheckSeverity::Ok),
            "runtime is ok",
        )?;

        Ok(())
    }

    #[test]
    fn doctor_report_overall_healthy_reflects_all_checks() -> TestResult {
        let report = DoctorReport::gather();

        let has_issues = report.checks.iter().any(|c| !c.severity.is_healthy());

        ensure(
            report.overall_healthy,
            !has_issues,
            "overall_healthy matches check status",
        )
    }

    #[test]
    fn check_result_ok_has_no_error_code() -> TestResult {
        let check = CheckResult::ok("test", "All good");
        ensure(check.error_code.is_none(), true, "ok has no error code")?;
        ensure(check.repair.is_none(), true, "ok has no repair")
    }

    #[test]
    fn check_result_warning_has_error_code_and_repair() -> TestResult {
        let check = CheckResult::warning("test", "Issue found", error_codes::DATABASE_NOT_FOUND);
        ensure(check.error_code.is_some(), true, "warning has error code")?;
        ensure(check.repair.is_some(), true, "warning has repair from code")
    }

    #[test]
    fn check_severity_strings_are_stable() -> TestResult {
        ensure(CheckSeverity::Ok.as_str(), "ok", "ok")?;
        ensure(CheckSeverity::Warning.as_str(), "warning", "warning")?;
        ensure(CheckSeverity::Error.as_str(), "error", "error")
    }

    #[test]
    fn fix_plan_contains_only_fixable_issues() -> TestResult {
        let report = DoctorReport::gather();
        let plan = report.to_fix_plan();

        for step in &plan.steps {
            ensure(!step.command.is_empty(), true, "step has a command")?;
            ensure(
                step.severity != CheckSeverity::Ok,
                true,
                "step is not an ok check",
            )?;
        }

        Ok(())
    }

    #[test]
    fn fix_plan_steps_are_ordered() -> TestResult {
        let report = DoctorReport::gather();
        let plan = report.to_fix_plan();

        for (idx, step) in plan.steps.iter().enumerate() {
            ensure(step.order, idx + 1, "step order is sequential")?;
        }

        Ok(())
    }

    #[test]
    fn fix_plan_counts_match() -> TestResult {
        let report = DoctorReport::gather();
        let plan = report.to_fix_plan();

        let unhealthy_count = report
            .checks
            .iter()
            .filter(|c| !c.severity.is_healthy())
            .count();
        ensure(plan.total_issues, unhealthy_count, "total_issues matches")?;

        let fixable_count = report
            .checks
            .iter()
            .filter(|c| !c.severity.is_healthy() && c.repair.is_some())
            .count();
        ensure(plan.fixable_issues, fixable_count, "fixable_issues matches")?;
        ensure(plan.steps.len(), fixable_count, "steps count matches")?;

        Ok(())
    }

    #[test]
    fn fix_plan_is_empty_when_all_healthy() -> TestResult {
        let report = DoctorReport {
            version: "0.1.0",
            overall_healthy: true,
            checks: vec![
                CheckResult::ok("test1", "All good"),
                CheckResult::ok("test2", "Also good"),
            ],
        };
        let plan = report.to_fix_plan();

        ensure(plan.is_empty(), true, "plan is empty when all healthy")?;
        ensure(plan.total_issues, 0, "no total issues")?;
        ensure(plan.fixable_issues, 0, "no fixable issues")
    }

    #[test]
    fn fix_plan_default_guidance_defers_agent_root_inspection() -> TestResult {
        let report = DoctorReport {
            version: "0.1.0",
            overall_healthy: true,
            checks: vec![],
        };
        let plan = report.to_fix_plan();

        ensure(
            plan.cass_import_guidance.status,
            CassImportGuidanceStatus::NotInspected,
            "default guidance is deferred",
        )?;
        ensure(
            plan.cass_import_guidance.detected_root_count,
            0,
            "deferred guidance has no roots",
        )?;
        ensure(
            plan.cass_import_guidance
                .suggested_commands
                .contains(&"ee agent status --json".to_string()),
            true,
            "deferred guidance suggests agent status",
        )
    }

    #[test]
    fn fix_plan_agent_inventory_guidance_uses_detected_roots() -> TestResult {
        let inventory = AgentInventoryReport::from_detection(
            crate::core::agent_detect::detect_fixture_agents()
                .map_err(|error| error.to_string())?,
        );
        let report = DoctorReport {
            version: "0.1.0",
            overall_healthy: false,
            checks: vec![CheckResult::warning(
                "cass",
                "CASS import dry-run recommended.",
                error_codes::AGENT_SOURCE_NOT_IMPORTED,
            )],
        };
        let plan = report.to_fix_plan_with_agent_inventory(&inventory);

        ensure(
            plan.cass_import_guidance.status,
            CassImportGuidanceStatus::AgentRootsDetected,
            "fixture roots detected",
        )?;
        ensure(
            plan.cass_import_guidance.detected_root_count >= 4,
            true,
            "fixture detected roots counted",
        )?;
        ensure(
            plan.cass_import_guidance
                .roots
                .iter()
                .any(|root| root.connector == "codex"),
            true,
            "codex fixture root present",
        )?;
        ensure(
            plan.cass_import_guidance
                .suggested_commands
                .contains(&"ee import cass --dry-run --json".to_string()),
            true,
            "CASS dry-run command suggested",
        )
    }

    #[test]
    fn integrity_diagnostics_missing_database_degrades_without_creating_file() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join("missing-ee.db");

        let report = IntegrityDiagnosticsReport::gather(&IntegrityDiagnosticsOptions {
            workspace_path: temp.path().to_path_buf(),
            database_path: Some(database_path.clone()),
            workspace_id: "default".to_string(),
            sample_size: 8,
            create_canary: false,
            dry_run: false,
        });

        ensure(
            report.status,
            IntegrityDiagnosticsStatus::Degraded,
            "missing db degrades",
        )?;
        ensure(database_path.exists(), false, "missing db was not created")?;
        ensure(
            report.canary.status,
            IntegrityCanaryStatus::NotRequested,
            "canary not requested",
        )?;
        ensure(
            report
                .degraded
                .iter()
                .any(|entry| entry.code == "integrity_database_missing"),
            true,
            "missing database degradation present",
        )
    }

    #[test]
    fn integrity_diagnostics_canary_dry_run_does_not_write_memory() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join("ee.db");
        let connection = crate::db::DbConnection::open_file(&database_path)
            .map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                TEST_WORKSPACE_ID,
                &crate::db::CreateWorkspaceInput {
                    path: temp.path().to_string_lossy().into_owned(),
                    name: Some("integrity-test".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let report = IntegrityDiagnosticsReport::gather(&IntegrityDiagnosticsOptions {
            workspace_path: temp.path().to_path_buf(),
            database_path: Some(database_path.clone()),
            workspace_id: TEST_WORKSPACE_ID.to_string(),
            sample_size: 8,
            create_canary: true,
            dry_run: true,
        });

        ensure(
            report.canary.status,
            IntegrityCanaryStatus::DryRun,
            "dry-run canary status",
        )?;

        let connection = crate::db::DbConnection::open_file(&database_path)
            .map_err(|error| error.to_string())?;
        ensure(
            connection
                .get_memory(INTEGRITY_CANARY_MEMORY_ID)
                .map_err(|error| error.to_string())?
                .is_none(),
            true,
            "dry run did not write canary",
        )
    }

    #[test]
    fn integrity_diagnostics_create_canary_is_idempotent() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join("ee.db");
        let connection = crate::db::DbConnection::open_file(&database_path)
            .map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                TEST_WORKSPACE_ID,
                &crate::db::CreateWorkspaceInput {
                    path: temp.path().to_string_lossy().into_owned(),
                    name: Some("integrity-test".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let options = IntegrityDiagnosticsOptions {
            workspace_path: temp.path().to_path_buf(),
            database_path: Some(database_path.clone()),
            workspace_id: TEST_WORKSPACE_ID.to_string(),
            sample_size: 8,
            create_canary: true,
            dry_run: false,
        };

        let first = IntegrityDiagnosticsReport::gather(&options);
        ensure(
            first.canary.status,
            IntegrityCanaryStatus::Created,
            "first run creates canary",
        )?;

        let second = IntegrityDiagnosticsReport::gather(&options);
        ensure(
            second.canary.status,
            IntegrityCanaryStatus::AlreadyExists,
            "second run is idempotent",
        )?;

        let connection = crate::db::DbConnection::open_file(&database_path)
            .map_err(|error| error.to_string())?;
        let memory = connection
            .get_memory(INTEGRITY_CANARY_MEMORY_ID)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "canary memory should exist".to_string())?;
        ensure(
            memory.trust_class,
            TrustClass::AgentAssertion.as_str().to_string(),
            "canary trust class",
        )
    }

    #[test]
    fn integrity_diagnostics_unmigrated_database_skips_canary() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join("ee.db");
        let connection = crate::db::DbConnection::open_file(&database_path)
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let report = IntegrityDiagnosticsReport::gather(&IntegrityDiagnosticsOptions {
            workspace_path: temp.path().to_path_buf(),
            database_path: Some(database_path),
            workspace_id: "default".to_string(),
            sample_size: 8,
            create_canary: true,
            dry_run: false,
        });

        ensure(
            report.status,
            IntegrityDiagnosticsStatus::Degraded,
            "unmigrated db degrades",
        )?;
        ensure(
            report.canary.status,
            IntegrityCanaryStatus::Skipped,
            "unmigrated db skips canary",
        )?;
        ensure(
            report
                .degraded
                .iter()
                .any(|entry| entry.code == "integrity_schema_migration_required"),
            true,
            "schema migration degradation present",
        )
    }

    #[test]
    fn dependency_diagnostics_report_summarizes_matrix() -> TestResult {
        let report = DependencyDiagnosticsReport::gather();

        ensure(
            report.schema,
            DEPENDENCY_DIAGNOSTICS_SCHEMA_V1,
            "dependency schema",
        )?;
        ensure(
            report.source_bead,
            DEPENDENCY_MATRIX_SOURCE_BEAD,
            "source bead",
        )?;
        ensure(report.entries.len(), 10, "matrix row count")?;
        ensure(
            report.summary.total_dependencies,
            10,
            "summary total dependencies",
        )?;
        ensure(
            report.summary.accepted_default_count,
            6,
            "accepted default count",
        )?;
        ensure(
            report.summary.forbidden_default_hit_count,
            0,
            "default forbidden hit count",
        )?;
        ensure(
            report.summary.blocked_feature_count,
            8,
            "blocked feature count",
        )?;

        Ok(())
    }

    #[test]
    fn dependency_diagnostics_rows_keep_required_entries() -> TestResult {
        let report = DependencyDiagnosticsReport::gather();

        for expected in [
            "asupersync",
            "frankensqlite",
            "sqlmodel_rust",
            "frankensearch",
            "franken_networkx",
            "coding_agent_session_search",
            "toon_rust",
            "franken_mermaid",
            "franken_agent_detection",
            "fastmcp-rust",
        ] {
            ensure(
                report.entries.iter().any(|entry| entry.name == expected),
                true,
                &format!("dependency row {expected} exists"),
            )?;
        }

        Ok(())
    }

    #[test]
    fn default_dependency_rows_have_no_forbidden_hits() -> TestResult {
        let report = DependencyDiagnosticsReport::gather();

        for entry in report
            .entries
            .iter()
            .filter(|entry| entry.enabled_by_default)
        {
            ensure(
                entry.has_default_forbidden_transitives(),
                false,
                &format!("{} has no forbidden default transitives", entry.name),
            )?;
        }

        Ok(())
    }

    #[test]
    fn franken_health_report_tracks_default_and_feature_gated_stack() -> TestResult {
        let report = FrankenHealthReport::gather();

        ensure(report.schema, FRANKEN_HEALTH_SCHEMA_V1, "franken schema")?;
        ensure(report.healthy, true, "franken health")?;
        ensure(
            report.summary.total_dependencies,
            6,
            "franken dependency count",
        )?;
        ensure(report.summary.ready_count, 5, "ready count")?;
        ensure(report.summary.feature_gated_count, 1, "feature gated count")?;
        ensure(report.summary.not_linked_count, 0, "not linked count")?;
        ensure(
            report.summary.forbidden_default_hit_count,
            0,
            "forbidden default hits",
        )?;

        let graph = report
            .dependencies
            .iter()
            .find(|dependency| dependency.name == "franken_networkx")
            .ok_or_else(|| "franken_networkx health row missing".to_string())?;
        ensure(graph.readiness, "feature_gated", "graph readiness")
    }
}
