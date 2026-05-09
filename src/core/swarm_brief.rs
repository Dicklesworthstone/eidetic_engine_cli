//! Read-only coordination snapshot model for swarm preflight briefs.
//!
//! This module deliberately stops at the internal source/model layer. Public
//! CLI rendering is owned by the follow-on `ee swarm brief` surface.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;
use serde_json::Value;

use crate::core::agent_detect::{AgentInventoryStatus, AgentStatusOptions, gather_agent_status};
use crate::core::profile::{HostResourceProbeReport, recommend_operating_profile};
use crate::policy::redact_secret_like_content;

pub const SWARM_BRIEF_SCHEMA_V1: &str = "ee.swarm.brief.v1";
pub const SWARM_BRIEF_REDACTION_STATUS: &str = "paths_counts_subjects_only_no_content";

const GIT_UNAVAILABLE_CODE: &str = "git_unavailable";
const BEADS_UNAVAILABLE_CODE: &str = "beads_unavailable";
const BV_UNAVAILABLE_CODE: &str = "bv_unavailable";
const AGENT_MAIL_UNAVAILABLE_CODE: &str = "agent_mail_unavailable";
const RCH_UNAVAILABLE_CODE: &str = "rch_unavailable";
const AGENT_STATUS_UNAVAILABLE_CODE: &str = "agent_status_unavailable";

/// Options used by the internal source collection layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwarmBriefCollectOptions {
    pub workspace: PathBuf,
    pub max_recent_commits: usize,
    pub include_rch: bool,
    pub agent_mail_snapshot_path: Option<PathBuf>,
    pub command_timeout_ms: u64,
}

impl SwarmBriefCollectOptions {
    #[must_use]
    pub fn for_workspace(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            max_recent_commits: 8,
            include_rch: false,
            agent_mail_snapshot_path: None,
            command_timeout_ms: 1_500,
        }
    }
}

/// Versioned report assembled from read-only coordination sources.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefReport {
    pub schema: &'static str,
    pub workspace: String,
    pub redaction_status: &'static str,
    pub sources: Vec<SwarmBriefSourceSnapshot>,
    pub dirty_files: Vec<SwarmBriefDirtyFile>,
    pub recent_commits: Vec<SwarmBriefCommit>,
    pub beads: SwarmBriefBeadsSummary,
    pub bv: Option<SwarmBriefBvSummary>,
    pub file_reservations: Vec<SwarmBriefFileReservation>,
    pub file_surface_risks: Vec<SwarmBriefFileSurfaceRisk>,
    pub inbox: Vec<SwarmBriefInboxSummary>,
    pub threads: Vec<SwarmBriefThreadSummary>,
    pub resource_pressure: Vec<SwarmBriefResourcePressureHint>,
    pub host_profile: Option<SwarmBriefHostProfileSummary>,
    pub agent_inventory: Option<SwarmBriefAgentInventorySummary>,
    pub recommendations: Vec<SwarmBriefRecommendation>,
    pub degraded: Vec<SwarmBriefDegradation>,
}

impl SwarmBriefReport {
    #[must_use]
    pub fn empty(workspace: &Path) -> Self {
        Self {
            schema: SWARM_BRIEF_SCHEMA_V1,
            workspace: redact_path_label(workspace),
            redaction_status: SWARM_BRIEF_REDACTION_STATUS,
            sources: Vec::new(),
            dirty_files: Vec::new(),
            recent_commits: Vec::new(),
            beads: SwarmBriefBeadsSummary::default(),
            bv: None,
            file_reservations: Vec::new(),
            file_surface_risks: Vec::new(),
            inbox: Vec::new(),
            threads: Vec::new(),
            resource_pressure: Vec::new(),
            host_profile: None,
            agent_inventory: None,
            recommendations: Vec::new(),
            degraded: Vec::new(),
        }
    }

    pub fn finalize(&mut self) {
        self.sources.sort();
        self.sources
            .dedup_by(|left, right| left.source == right.source);
        self.dirty_files.sort();
        self.dirty_files.dedup();
        self.recent_commits.sort_by(|left, right| {
            right
                .authored_at_epoch_seconds
                .cmp(&left.authored_at_epoch_seconds)
                .then_with(|| left.hash.cmp(&right.hash))
                .then_with(|| left.subject.cmp(&right.subject))
        });
        self.beads.ready.sort();
        self.beads.blocked.sort();
        self.beads.in_progress.sort();
        self.beads.deferred.sort();
        self.file_reservations.sort();
        self.file_reservations.dedup();
        self.file_surface_risks.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.path_pattern.cmp(&right.path_pattern))
                .then_with(|| left.severity.cmp(&right.severity))
        });
        self.file_surface_risks
            .dedup_by(|left, right| left.path_pattern == right.path_pattern);
        self.inbox.sort();
        self.inbox.dedup();
        self.threads.sort();
        self.threads.dedup();
        self.resource_pressure.sort();
        self.resource_pressure.dedup();
        self.recommendations.sort();
        self.recommendations.dedup();
        self.degraded.sort();
        self.degraded.dedup();
    }
}

/// Source identity for the brief.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmBriefSourceKind {
    AgentInventory,
    AgentMail,
    Beads,
    Bv,
    Git,
    HostProfile,
    Rch,
}

impl SwarmBriefSourceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentInventory => "agent_inventory",
            Self::AgentMail => "agent_mail",
            Self::Beads => "beads",
            Self::Bv => "bv",
            Self::Git => "git",
            Self::HostProfile => "host_profile",
            Self::Rch => "rch",
        }
    }
}

impl fmt::Display for SwarmBriefSourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Normalized status of an optional source.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmBriefSourceStatus {
    Ready,
    Degraded,
    Unavailable,
    NotConfigured,
    Skipped,
}

impl SwarmBriefSourceStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Degraded => "degraded",
            Self::Unavailable => "unavailable",
            Self::NotConfigured => "not_configured",
            Self::Skipped => "skipped",
        }
    }
}

/// Freshness metadata for a source snapshot.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefSourceFreshness {
    pub observed_at: Option<String>,
    pub age_seconds: Option<u64>,
    pub stale_after_seconds: Option<u64>,
    pub state: &'static str,
}

impl SwarmBriefSourceFreshness {
    #[must_use]
    pub const fn current() -> Self {
        Self {
            observed_at: None,
            age_seconds: Some(0),
            stale_after_seconds: None,
            state: "current",
        }
    }

    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            observed_at: None,
            age_seconds: None,
            stale_after_seconds: None,
            state: "unknown",
        }
    }
}

/// Redaction-safe source provenance.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefSourceProvenance {
    pub command: Option<String>,
    pub side_effect_free: bool,
    pub redaction: &'static str,
}

impl SwarmBriefSourceProvenance {
    #[must_use]
    pub fn command(program: &str, args: &[&str]) -> Self {
        let command = std::iter::once(program)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            command: Some(command),
            side_effect_free: true,
            redaction: SWARM_BRIEF_REDACTION_STATUS,
        }
    }

    #[must_use]
    pub const fn local_probe() -> Self {
        Self {
            command: None,
            side_effect_free: true,
            redaction: SWARM_BRIEF_REDACTION_STATUS,
        }
    }
}

/// A normalized source snapshot.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefSourceSnapshot {
    pub source: SwarmBriefSourceKind,
    pub status: SwarmBriefSourceStatus,
    pub freshness: SwarmBriefSourceFreshness,
    pub provenance: SwarmBriefSourceProvenance,
    pub item_count: usize,
    pub degraded: Vec<SwarmBriefDegradation>,
}

impl SwarmBriefSourceSnapshot {
    #[must_use]
    pub fn ready(
        source: SwarmBriefSourceKind,
        provenance: SwarmBriefSourceProvenance,
        item_count: usize,
    ) -> Self {
        Self {
            source,
            status: SwarmBriefSourceStatus::Ready,
            freshness: SwarmBriefSourceFreshness::current(),
            provenance,
            item_count,
            degraded: Vec::new(),
        }
    }

    #[must_use]
    pub fn unavailable(
        source: SwarmBriefSourceKind,
        provenance: SwarmBriefSourceProvenance,
        degradation: SwarmBriefDegradation,
    ) -> Self {
        Self {
            source,
            status: SwarmBriefSourceStatus::Unavailable,
            freshness: SwarmBriefSourceFreshness::unknown(),
            provenance,
            item_count: 0,
            degraded: vec![degradation],
        }
    }

    fn with_degraded(mut self, degraded: Vec<SwarmBriefDegradation>) -> Self {
        if !degraded.is_empty() && self.status == SwarmBriefSourceStatus::Ready {
            self.status = SwarmBriefSourceStatus::Degraded;
        }
        self.degraded = degraded;
        self
    }
}

/// Stable degraded-source record.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefDegradation {
    pub code: String,
    pub source: SwarmBriefSourceKind,
    pub severity: &'static str,
    pub message: String,
    pub repair: Option<String>,
}

impl SwarmBriefDegradation {
    #[must_use]
    pub fn warning(
        source: SwarmBriefSourceKind,
        code: impl Into<String>,
        message: impl Into<String>,
        repair: impl Into<Option<String>>,
    ) -> Self {
        Self {
            code: code.into(),
            source,
            severity: "warning",
            message: redact_brief_text(&message.into()),
            repair: repair.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefDirtyFile {
    pub path: String,
    pub status: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefCommit {
    pub hash: String,
    pub authored_at_epoch_seconds: Option<i64>,
    pub subject: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefBeadsSummary {
    pub ready: Vec<SwarmBriefBead>,
    pub blocked: Vec<SwarmBriefBead>,
    pub in_progress: Vec<SwarmBriefBead>,
    pub deferred: Vec<SwarmBriefBead>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefBead {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: Option<i64>,
    pub assignee: Option<String>,
    pub source_bucket: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefBvSummary {
    pub actionable_count: Option<u64>,
    pub blocked_count: Option<u64>,
    pub in_progress_count: Option<u64>,
    pub track_count: Option<u64>,
    pub top_picks: Vec<SwarmBriefBvPick>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefBvPick {
    pub id: String,
    pub title: String,
    pub score_milli: Option<u32>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefFileReservation {
    pub path_pattern: String,
    pub holder: String,
    pub exclusive: bool,
    pub expires_at: Option<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefFileSurfaceRisk {
    pub path_pattern: String,
    pub severity: String,
    pub score: u16,
    pub risk_factors: Vec<String>,
    pub evidence: Vec<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefInboxSummary {
    pub mailbox: String,
    pub unread_count: u64,
    pub ack_required_count: u64,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefThreadSummary {
    pub thread_id: String,
    pub subject: Option<String>,
    pub message_count: Option<u64>,
    pub last_activity_at: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefAgentMailSnapshot {
    pub file_reservations: Vec<SwarmBriefFileReservation>,
    pub inbox: Vec<SwarmBriefInboxSummary>,
    pub threads: Vec<SwarmBriefThreadSummary>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefResourcePressureHint {
    pub source: SwarmBriefSourceKind,
    pub level: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefHostProfileSummary {
    pub recommended_profile: String,
    pub confidence: String,
    pub logical_cores: Option<u32>,
    pub memory_total_bytes: Option<u64>,
    pub memory_available_bytes: Option<u64>,
    pub rch_hint_configured: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefAgentInventorySummary {
    pub status: String,
    pub detected_count: usize,
    pub total_count: usize,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefRecommendation {
    pub id: String,
    pub kind: String,
    pub confidence: String,
    pub severity: String,
    pub reason_codes: Vec<String>,
    pub evidence: Vec<String>,
    pub suggested_commands: Vec<String>,
    pub must_not_do: Vec<String>,
}

/// Command output for read-only source adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwarmBriefCommandOutput {
    pub stdout: String,
    pub stderr: String,
}

/// Error returned by a read-only command runner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SwarmBriefCommandError {
    Unavailable(String),
    Failed { status: Option<i32>, stderr: String },
    TimedOut { timeout_ms: u64 },
    InvalidUtf8(String),
}

impl SwarmBriefCommandError {
    fn to_degradation(
        &self,
        source: SwarmBriefSourceKind,
        code: &'static str,
        repair: &'static str,
    ) -> SwarmBriefDegradation {
        let message = match self {
            Self::Unavailable(message) => message.clone(),
            Self::Failed { status, stderr } => {
                let status = status
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "terminated_by_signal".to_string());
                format!("Read-only source command failed with status {status}: {stderr}")
            }
            Self::TimedOut { timeout_ms } => {
                format!("Read-only source command timed out after {timeout_ms} ms.")
            }
            Self::InvalidUtf8(message) => message.clone(),
        };
        SwarmBriefDegradation::warning(source, code, message, Some(repair.to_string()))
    }
}

/// Read-only command runner abstraction used by external source adapters.
pub trait SwarmBriefCommandRunner {
    fn run(
        &self,
        program: &str,
        args: &[&str],
        cwd: &Path,
        timeout_ms: u64,
    ) -> Result<SwarmBriefCommandOutput, SwarmBriefCommandError>;
}

/// Production command runner. It only accepts explicit program/argument lists.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SystemSwarmBriefCommandRunner;

impl SwarmBriefCommandRunner for SystemSwarmBriefCommandRunner {
    fn run(
        &self,
        program: &str,
        args: &[&str],
        cwd: &Path,
        _timeout_ms: u64,
    ) -> Result<SwarmBriefCommandOutput, SwarmBriefCommandError> {
        let output = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .output()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    SwarmBriefCommandError::Unavailable(format!("{program} was not found on PATH."))
                } else {
                    SwarmBriefCommandError::Unavailable(error.to_string())
                }
            })?;

        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| SwarmBriefCommandError::InvalidUtf8(error.to_string()))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|error| SwarmBriefCommandError::InvalidUtf8(error.to_string()))?;

        if output.status.success() {
            Ok(SwarmBriefCommandOutput { stdout, stderr })
        } else {
            Err(SwarmBriefCommandError::Failed {
                status: output.status.code(),
                stderr,
            })
        }
    }
}

/// Output from one source adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwarmBriefSourceOutput {
    pub snapshot: SwarmBriefSourceSnapshot,
    pub contribution: SwarmBriefContribution,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SwarmBriefContribution {
    None,
    Git {
        dirty_files: Vec<SwarmBriefDirtyFile>,
        recent_commits: Vec<SwarmBriefCommit>,
    },
    Beads(SwarmBriefBeadsSummary),
    Bv(SwarmBriefBvSummary),
    AgentMail {
        file_reservations: Vec<SwarmBriefFileReservation>,
        inbox: Vec<SwarmBriefInboxSummary>,
        threads: Vec<SwarmBriefThreadSummary>,
    },
    Rch(Vec<SwarmBriefResourcePressureHint>),
    HostProfile(SwarmBriefHostProfileSummary),
    AgentInventory(SwarmBriefAgentInventorySummary),
}

/// Source adapter contract. Implementations must be read-only.
pub trait SwarmBriefSourceAdapter {
    fn collect(&self, options: &SwarmBriefCollectOptions) -> SwarmBriefSourceOutput;
}

pub struct GitSourceAdapter<'a, R> {
    pub runner: &'a R,
}

impl<R: SwarmBriefCommandRunner> SwarmBriefSourceAdapter for GitSourceAdapter<'_, R> {
    fn collect(&self, options: &SwarmBriefCollectOptions) -> SwarmBriefSourceOutput {
        let provenance = SwarmBriefSourceProvenance::command(
            "git",
            &["status", "--short", "--branch", "--untracked-files=all"],
        );
        let status = self.runner.run(
            "git",
            &["status", "--short", "--branch", "--untracked-files=all"],
            &options.workspace,
            options.command_timeout_ms,
        );

        let status_output = match status {
            Ok(output) => output,
            Err(error) => {
                let degradation = error.to_degradation(
                    SwarmBriefSourceKind::Git,
                    GIT_UNAVAILABLE_CODE,
                    "Run `git status --short` in the workspace.",
                );
                return SwarmBriefSourceOutput {
                    snapshot: SwarmBriefSourceSnapshot::unavailable(
                        SwarmBriefSourceKind::Git,
                        provenance,
                        degradation,
                    ),
                    contribution: SwarmBriefContribution::None,
                };
            }
        };

        let mut degraded = Vec::new();
        let dirty_files = parse_git_status_short(&status_output.stdout);
        let log_args = [
            "log",
            "-n",
            &options.max_recent_commits.to_string(),
            "--format=%H%x1f%ct%x1f%s",
        ];
        let recent_commits = match self.runner.run(
            "git",
            &log_args,
            &options.workspace,
            options.command_timeout_ms,
        ) {
            Ok(output) => parse_git_log(&output.stdout),
            Err(error) => {
                degraded.push(error.to_degradation(
                    SwarmBriefSourceKind::Git,
                    GIT_UNAVAILABLE_CODE,
                    "Run `git log -n 8 --format=%H%x1f%ct%x1f%s` in the workspace.",
                ));
                Vec::new()
            }
        };

        let item_count = dirty_files.len() + recent_commits.len();
        SwarmBriefSourceOutput {
            snapshot: SwarmBriefSourceSnapshot::ready(
                SwarmBriefSourceKind::Git,
                provenance,
                item_count,
            )
            .with_degraded(degraded),
            contribution: SwarmBriefContribution::Git {
                dirty_files,
                recent_commits,
            },
        }
    }
}

pub struct BeadsSourceAdapter<'a, R> {
    pub runner: &'a R,
}

impl<R: SwarmBriefCommandRunner> SwarmBriefSourceAdapter for BeadsSourceAdapter<'_, R> {
    fn collect(&self, options: &SwarmBriefCollectOptions) -> SwarmBriefSourceOutput {
        let source = SwarmBriefSourceKind::Beads;
        let provenance = SwarmBriefSourceProvenance::command("br", &["ready", "--json"]);
        let mut degraded = Vec::new();

        let ready = collect_beads_bucket(
            self.runner,
            options,
            &["ready", "--json"],
            "ready",
            &mut degraded,
        );
        let blocked = collect_beads_bucket(
            self.runner,
            options,
            &["blocked", "--json"],
            "blocked",
            &mut degraded,
        );
        let in_progress = collect_beads_bucket(
            self.runner,
            options,
            &["list", "--status", "in_progress", "--json"],
            "in_progress",
            &mut degraded,
        );
        let deferred = collect_beads_bucket(
            self.runner,
            options,
            &["list", "--status", "deferred", "--json"],
            "deferred",
            &mut degraded,
        );

        if ready.is_empty()
            && blocked.is_empty()
            && in_progress.is_empty()
            && deferred.is_empty()
            && !degraded.is_empty()
        {
            return SwarmBriefSourceOutput {
                snapshot: SwarmBriefSourceSnapshot::unavailable(
                    source,
                    provenance,
                    degraded.remove(0),
                ),
                contribution: SwarmBriefContribution::None,
            };
        }

        let summary = SwarmBriefBeadsSummary {
            ready,
            blocked,
            in_progress,
            deferred,
        };
        let item_count = summary.ready.len()
            + summary.blocked.len()
            + summary.in_progress.len()
            + summary.deferred.len();
        SwarmBriefSourceOutput {
            snapshot: SwarmBriefSourceSnapshot::ready(source, provenance, item_count)
                .with_degraded(degraded),
            contribution: SwarmBriefContribution::Beads(summary),
        }
    }
}

fn collect_beads_bucket<R: SwarmBriefCommandRunner>(
    runner: &R,
    options: &SwarmBriefCollectOptions,
    args: &[&str],
    bucket: &str,
    degraded: &mut Vec<SwarmBriefDegradation>,
) -> Vec<SwarmBriefBead> {
    match runner.run("br", args, &options.workspace, options.command_timeout_ms) {
        Ok(output) => parse_beads_json(&output.stdout, bucket).unwrap_or_else(|message| {
            degraded.push(SwarmBriefDegradation::warning(
                SwarmBriefSourceKind::Beads,
                BEADS_UNAVAILABLE_CODE,
                message,
                Some("br ready --json".to_string()),
            ));
            Vec::new()
        }),
        Err(error) => {
            degraded.push(error.to_degradation(
                SwarmBriefSourceKind::Beads,
                BEADS_UNAVAILABLE_CODE,
                "br ready --json",
            ));
            Vec::new()
        }
    }
}

pub struct BvSourceAdapter<'a, R> {
    pub runner: &'a R,
}

impl<R: SwarmBriefCommandRunner> SwarmBriefSourceAdapter for BvSourceAdapter<'_, R> {
    fn collect(&self, options: &SwarmBriefCollectOptions) -> SwarmBriefSourceOutput {
        let args = ["--robot-triage", "--robot-triage-by-track"];
        let provenance = SwarmBriefSourceProvenance::command("bv", &args);
        match self
            .runner
            .run("bv", &args, &options.workspace, options.command_timeout_ms)
        {
            Ok(output) => match parse_bv_triage_json(&output.stdout) {
                Ok(summary) => {
                    let item_count = summary.top_picks.len();
                    SwarmBriefSourceOutput {
                        snapshot: SwarmBriefSourceSnapshot::ready(
                            SwarmBriefSourceKind::Bv,
                            provenance,
                            item_count,
                        ),
                        contribution: SwarmBriefContribution::Bv(summary),
                    }
                }
                Err(message) => {
                    let degradation = SwarmBriefDegradation::warning(
                        SwarmBriefSourceKind::Bv,
                        BV_UNAVAILABLE_CODE,
                        message,
                        Some("bv --robot-triage --robot-triage-by-track".to_string()),
                    );
                    SwarmBriefSourceOutput {
                        snapshot: SwarmBriefSourceSnapshot::unavailable(
                            SwarmBriefSourceKind::Bv,
                            provenance,
                            degradation,
                        ),
                        contribution: SwarmBriefContribution::None,
                    }
                }
            },
            Err(error) => {
                let degradation = error.to_degradation(
                    SwarmBriefSourceKind::Bv,
                    BV_UNAVAILABLE_CODE,
                    "bv --robot-triage --robot-triage-by-track",
                );
                SwarmBriefSourceOutput {
                    snapshot: SwarmBriefSourceSnapshot::unavailable(
                        SwarmBriefSourceKind::Bv,
                        provenance,
                        degradation,
                    ),
                    contribution: SwarmBriefContribution::None,
                }
            }
        }
    }
}

pub struct AgentMailSnapshotFileAdapter;

impl SwarmBriefSourceAdapter for AgentMailSnapshotFileAdapter {
    fn collect(&self, options: &SwarmBriefCollectOptions) -> SwarmBriefSourceOutput {
        let provenance = SwarmBriefSourceProvenance::local_probe();
        let Some(path) = &options.agent_mail_snapshot_path else {
            let degradation = SwarmBriefDegradation::warning(
                SwarmBriefSourceKind::AgentMail,
                AGENT_MAIL_UNAVAILABLE_CODE,
                "No redacted Agent Mail snapshot path was configured.",
                Some(
                    "Provide a redacted Agent Mail snapshot path when collecting the brief."
                        .to_string(),
                ),
            );
            return SwarmBriefSourceOutput {
                snapshot: SwarmBriefSourceSnapshot {
                    source: SwarmBriefSourceKind::AgentMail,
                    status: SwarmBriefSourceStatus::NotConfigured,
                    freshness: SwarmBriefSourceFreshness::unknown(),
                    provenance,
                    item_count: 0,
                    degraded: vec![degradation],
                },
                contribution: SwarmBriefContribution::None,
            };
        };

        match std::fs::read_to_string(path) {
            Ok(contents) => match parse_agent_mail_snapshot_json(&contents) {
                Ok(snapshot) => {
                    let item_count = snapshot.file_reservations.len()
                        + snapshot.inbox.len()
                        + snapshot.threads.len();
                    SwarmBriefSourceOutput {
                        snapshot: SwarmBriefSourceSnapshot::ready(
                            SwarmBriefSourceKind::AgentMail,
                            provenance,
                            item_count,
                        ),
                        contribution: SwarmBriefContribution::AgentMail {
                            file_reservations: snapshot.file_reservations,
                            inbox: snapshot.inbox,
                            threads: snapshot.threads,
                        },
                    }
                }
                Err(message) => {
                    let degradation = SwarmBriefDegradation::warning(
                        SwarmBriefSourceKind::AgentMail,
                        AGENT_MAIL_UNAVAILABLE_CODE,
                        message,
                        Some("Regenerate the redacted Agent Mail snapshot.".to_string()),
                    );
                    SwarmBriefSourceOutput {
                        snapshot: SwarmBriefSourceSnapshot::unavailable(
                            SwarmBriefSourceKind::AgentMail,
                            provenance,
                            degradation,
                        ),
                        contribution: SwarmBriefContribution::None,
                    }
                }
            },
            Err(error) => {
                let degradation = SwarmBriefDegradation::warning(
                    SwarmBriefSourceKind::AgentMail,
                    AGENT_MAIL_UNAVAILABLE_CODE,
                    error.to_string(),
                    Some("Check the configured Agent Mail snapshot path.".to_string()),
                );
                SwarmBriefSourceOutput {
                    snapshot: SwarmBriefSourceSnapshot::unavailable(
                        SwarmBriefSourceKind::AgentMail,
                        provenance,
                        degradation,
                    ),
                    contribution: SwarmBriefContribution::None,
                }
            }
        }
    }
}

pub struct RchSourceAdapter<'a, R> {
    pub runner: &'a R,
}

impl<R: SwarmBriefCommandRunner> SwarmBriefSourceAdapter for RchSourceAdapter<'_, R> {
    fn collect(&self, options: &SwarmBriefCollectOptions) -> SwarmBriefSourceOutput {
        let args = ["status", "--json"];
        let provenance = SwarmBriefSourceProvenance::command("rch", &args);
        if !options.include_rch {
            return SwarmBriefSourceOutput {
                snapshot: SwarmBriefSourceSnapshot {
                    source: SwarmBriefSourceKind::Rch,
                    status: SwarmBriefSourceStatus::Skipped,
                    freshness: SwarmBriefSourceFreshness::unknown(),
                    provenance,
                    item_count: 0,
                    degraded: Vec::new(),
                },
                contribution: SwarmBriefContribution::None,
            };
        }

        match self
            .runner
            .run("rch", &args, &options.workspace, options.command_timeout_ms)
        {
            Ok(output) => match parse_rch_status_json(&output.stdout) {
                Ok(hints) => {
                    let item_count = hints.len();
                    SwarmBriefSourceOutput {
                        snapshot: SwarmBriefSourceSnapshot::ready(
                            SwarmBriefSourceKind::Rch,
                            provenance,
                            item_count,
                        ),
                        contribution: SwarmBriefContribution::Rch(hints),
                    }
                }
                Err(message) => {
                    let degradation = SwarmBriefDegradation::warning(
                        SwarmBriefSourceKind::Rch,
                        RCH_UNAVAILABLE_CODE,
                        message,
                        Some("rch status --json".to_string()),
                    );
                    SwarmBriefSourceOutput {
                        snapshot: SwarmBriefSourceSnapshot::unavailable(
                            SwarmBriefSourceKind::Rch,
                            provenance,
                            degradation,
                        ),
                        contribution: SwarmBriefContribution::None,
                    }
                }
            },
            Err(error) => {
                let degradation = error.to_degradation(
                    SwarmBriefSourceKind::Rch,
                    RCH_UNAVAILABLE_CODE,
                    "rch status --json",
                );
                SwarmBriefSourceOutput {
                    snapshot: SwarmBriefSourceSnapshot::unavailable(
                        SwarmBriefSourceKind::Rch,
                        provenance,
                        degradation,
                    ),
                    contribution: SwarmBriefContribution::None,
                }
            }
        }
    }
}

pub struct HostProfileSourceAdapter;

impl SwarmBriefSourceAdapter for HostProfileSourceAdapter {
    fn collect(&self, options: &SwarmBriefCollectOptions) -> SwarmBriefSourceOutput {
        let provenance = SwarmBriefSourceProvenance::local_probe();
        let probe = HostResourceProbeReport::gather_for_workspace(&options.workspace);
        let recommendation = recommend_operating_profile(&probe);
        let summary = SwarmBriefHostProfileSummary {
            recommended_profile: recommendation.recommended.as_str().to_string(),
            confidence: recommendation.confidence.to_string(),
            logical_cores: probe.cpu.logical_cores,
            memory_total_bytes: probe.memory.total_bytes,
            memory_available_bytes: probe.memory.available_bytes,
            rch_hint_configured: probe.environment.rch_hint_configured,
        };
        let degraded = probe
            .degraded
            .iter()
            .map(|item| {
                SwarmBriefDegradation::warning(
                    SwarmBriefSourceKind::HostProfile,
                    item.code,
                    item.message.clone(),
                    Some(item.repair.to_string()),
                )
            })
            .collect::<Vec<_>>();
        SwarmBriefSourceOutput {
            snapshot: SwarmBriefSourceSnapshot::ready(
                SwarmBriefSourceKind::HostProfile,
                provenance,
                1,
            )
            .with_degraded(degraded),
            contribution: SwarmBriefContribution::HostProfile(summary),
        }
    }
}

pub struct AgentInventorySourceAdapter;

impl SwarmBriefSourceAdapter for AgentInventorySourceAdapter {
    fn collect(&self, _options: &SwarmBriefCollectOptions) -> SwarmBriefSourceOutput {
        let provenance = SwarmBriefSourceProvenance::local_probe();
        match gather_agent_status(&AgentStatusOptions::default()) {
            Ok(report) => {
                let summary = SwarmBriefAgentInventorySummary {
                    status: report.status.as_str().to_string(),
                    detected_count: report.summary.detected_count,
                    total_count: report.summary.total_count,
                };
                let status = if report.status == AgentInventoryStatus::Unavailable {
                    SwarmBriefSourceStatus::Unavailable
                } else {
                    SwarmBriefSourceStatus::Ready
                };
                SwarmBriefSourceOutput {
                    snapshot: SwarmBriefSourceSnapshot {
                        source: SwarmBriefSourceKind::AgentInventory,
                        status,
                        freshness: SwarmBriefSourceFreshness::current(),
                        provenance,
                        item_count: summary.detected_count,
                        degraded: Vec::new(),
                    },
                    contribution: SwarmBriefContribution::AgentInventory(summary),
                }
            }
            Err(error) => {
                let degradation = SwarmBriefDegradation::warning(
                    SwarmBriefSourceKind::AgentInventory,
                    AGENT_STATUS_UNAVAILABLE_CODE,
                    error.to_string(),
                    Some("ee agent status --json".to_string()),
                );
                SwarmBriefSourceOutput {
                    snapshot: SwarmBriefSourceSnapshot::unavailable(
                        SwarmBriefSourceKind::AgentInventory,
                        provenance,
                        degradation,
                    ),
                    contribution: SwarmBriefContribution::None,
                }
            }
        }
    }
}

/// Collect a complete internal brief using production source adapters.
///
/// This is intentionally not wired to a public command yet.
#[must_use]
pub fn collect_swarm_brief(
    options: &SwarmBriefCollectOptions,
    runner: &impl SwarmBriefCommandRunner,
) -> SwarmBriefReport {
    let mut report = SwarmBriefReport::empty(&options.workspace);
    apply_source_output(&mut report, GitSourceAdapter { runner }.collect(options));
    apply_source_output(&mut report, BeadsSourceAdapter { runner }.collect(options));
    apply_source_output(&mut report, BvSourceAdapter { runner }.collect(options));
    apply_source_output(&mut report, AgentMailSnapshotFileAdapter.collect(options));
    apply_source_output(&mut report, RchSourceAdapter { runner }.collect(options));
    apply_source_output(&mut report, HostProfileSourceAdapter.collect(options));
    apply_source_output(&mut report, AgentInventorySourceAdapter.collect(options));
    apply_swarm_brief_advice(&mut report);
    report.finalize();
    report
}

fn apply_source_output(report: &mut SwarmBriefReport, output: SwarmBriefSourceOutput) {
    report.degraded.extend(output.snapshot.degraded.clone());
    report.sources.push(output.snapshot);
    match output.contribution {
        SwarmBriefContribution::None => {}
        SwarmBriefContribution::Git {
            dirty_files,
            recent_commits,
        } => {
            report.dirty_files.extend(dirty_files);
            report.recent_commits.extend(recent_commits);
        }
        SwarmBriefContribution::Beads(summary) => {
            report.beads.ready.extend(summary.ready);
            report.beads.blocked.extend(summary.blocked);
            report.beads.in_progress.extend(summary.in_progress);
            report.beads.deferred.extend(summary.deferred);
        }
        SwarmBriefContribution::Bv(summary) => {
            report.bv = Some(summary);
        }
        SwarmBriefContribution::AgentMail {
            file_reservations,
            inbox,
            threads,
        } => {
            report.file_reservations.extend(file_reservations);
            report.inbox.extend(inbox);
            report.threads.extend(threads);
        }
        SwarmBriefContribution::Rch(hints) => report.resource_pressure.extend(hints),
        SwarmBriefContribution::HostProfile(summary) => {
            report.host_profile = Some(summary);
        }
        SwarmBriefContribution::AgentInventory(summary) => {
            report.agent_inventory = Some(summary);
        }
    }
}

/// Derive deterministic, read-only advisory records from collected sources.
///
/// This pass is deliberately pure over the in-memory report. It does not run
/// commands, claim Beads, reserve files, send mail, build code, or update git.
pub fn apply_swarm_brief_advice(report: &mut SwarmBriefReport) {
    let mut pressure = report.resource_pressure.clone();
    pressure.extend(derive_host_pressure_hints(report.host_profile.as_ref()));
    pressure.sort();
    pressure.dedup();
    report.resource_pressure = pressure;

    report.file_surface_risks = score_file_surface_risks(report);
    report.recommendations = recommend_swarm_brief_actions(report);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SurfaceObservationKind {
    Bead,
    Dirty,
    RecentCommit,
    Reservation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SurfaceObservation {
    pattern: String,
    kind: SurfaceObservationKind,
    factor: String,
    evidence: String,
    score: u16,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SurfaceRiskBuilder {
    score: u16,
    risk_factors: BTreeSet<String>,
    evidence: BTreeSet<String>,
}

impl SurfaceRiskBuilder {
    fn add(&mut self, factor: impl Into<String>, evidence: impl Into<String>, score: u16) {
        self.score = self.score.saturating_add(score).min(100);
        self.risk_factors.insert(redact_brief_text(&factor.into()));
        self.evidence.insert(redact_brief_text(&evidence.into()));
    }

    fn build(self, path_pattern: String) -> SwarmBriefFileSurfaceRisk {
        SwarmBriefFileSurfaceRisk {
            path_pattern,
            severity: severity_for_score(self.score).to_string(),
            score: self.score,
            risk_factors: self.risk_factors.into_iter().collect(),
            evidence: self.evidence.into_iter().collect(),
        }
    }
}

fn score_file_surface_risks(report: &SwarmBriefReport) -> Vec<SwarmBriefFileSurfaceRisk> {
    let observations = collect_surface_observations(report);
    let mut risks = BTreeMap::<String, SurfaceRiskBuilder>::new();

    for observation in &observations {
        risks.entry(observation.pattern.clone()).or_default().add(
            observation.factor.clone(),
            observation.evidence.clone(),
            observation.score,
        );
    }

    for (index, left) in observations.iter().enumerate() {
        for right in observations.iter().skip(index + 1) {
            if left.kind == right.kind || !surfaces_overlap(&left.pattern, &right.pattern) {
                continue;
            }
            for (pattern, factor, score) in overlap_risk_factors(left, right) {
                let evidence = format!(
                    "overlap:{}<->{}",
                    observation_label(left),
                    observation_label(right)
                );
                risks
                    .entry(pattern)
                    .or_default()
                    .add(factor, evidence, score);
            }
        }
    }

    let mut output = risks
        .into_iter()
        .map(|(pattern, risk)| risk.build(pattern))
        .collect::<Vec<_>>();
    output.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.path_pattern.cmp(&right.path_pattern))
            .then_with(|| left.severity.cmp(&right.severity))
    });
    output
}

fn collect_surface_observations(report: &SwarmBriefReport) -> Vec<SurfaceObservation> {
    let mut observations = Vec::new();

    for file in &report.dirty_files {
        observations.push(SurfaceObservation {
            pattern: file.path.clone(),
            kind: SurfaceObservationKind::Dirty,
            factor: "dirty_worktree_path".to_string(),
            evidence: format!("git_status:{}:{}", file.status, file.path),
            score: 25,
        });
    }

    for reservation in &report.file_reservations {
        let factor = if reservation.exclusive {
            "active_exclusive_reservation"
        } else {
            "active_shared_reservation"
        };
        observations.push(SurfaceObservation {
            pattern: reservation.path_pattern.clone(),
            kind: SurfaceObservationKind::Reservation,
            factor: factor.to_string(),
            evidence: format!(
                "agent_mail_reservation:{}:{}",
                reservation.holder, reservation.path_pattern
            ),
            score: if reservation.exclusive { 35 } else { 20 },
        });
    }

    for bead in all_swarm_brief_beads(&report.beads) {
        for pattern in likely_surfaces_for_bead(bead) {
            observations.push(SurfaceObservation {
                pattern,
                kind: SurfaceObservationKind::Bead,
                factor: format!("{}_bead_likely_surface", bead.source_bucket),
                evidence: format!("bead:{}:{}:{}", bead.id, bead.source_bucket, bead.title),
                score: 12,
            });
        }
    }

    for commit in &report.recent_commits {
        for pattern in likely_surfaces_for_text(&commit.subject) {
            observations.push(SurfaceObservation {
                pattern,
                kind: SurfaceObservationKind::RecentCommit,
                factor: "recent_commit_likely_surface".to_string(),
                evidence: format!("git_commit:{}:{}", commit.hash, commit.subject),
                score: 5,
            });
        }
    }

    observations.sort_by(|left, right| {
        left.pattern
            .cmp(&right.pattern)
            .then_with(|| left.factor.cmp(&right.factor))
            .then_with(|| left.evidence.cmp(&right.evidence))
    });
    observations
}

fn overlap_risk_factors(
    left: &SurfaceObservation,
    right: &SurfaceObservation,
) -> Vec<(String, String, u16)> {
    let pattern = narrower_surface_pattern(&left.pattern, &right.pattern);
    match (left.kind, right.kind) {
        (SurfaceObservationKind::Dirty, SurfaceObservationKind::Reservation)
        | (SurfaceObservationKind::Reservation, SurfaceObservationKind::Dirty) => {
            vec![(pattern, "dirty_reservation_overlap".to_string(), 25)]
        }
        (SurfaceObservationKind::Bead, SurfaceObservationKind::Reservation)
        | (SurfaceObservationKind::Reservation, SurfaceObservationKind::Bead) => {
            vec![(pattern, "bead_reservation_overlap".to_string(), 20)]
        }
        (SurfaceObservationKind::Bead, SurfaceObservationKind::Dirty)
        | (SurfaceObservationKind::Dirty, SurfaceObservationKind::Bead) => {
            vec![(pattern, "dirty_bead_overlap".to_string(), 15)]
        }
        (SurfaceObservationKind::Bead, SurfaceObservationKind::RecentCommit)
        | (SurfaceObservationKind::RecentCommit, SurfaceObservationKind::Bead) => {
            vec![(pattern, "recent_commit_bead_overlap".to_string(), 5)]
        }
        (SurfaceObservationKind::Reservation, SurfaceObservationKind::RecentCommit)
        | (SurfaceObservationKind::RecentCommit, SurfaceObservationKind::Reservation) => {
            vec![(pattern, "recent_commit_reservation_overlap".to_string(), 5)]
        }
        (SurfaceObservationKind::Dirty, SurfaceObservationKind::RecentCommit)
        | (SurfaceObservationKind::RecentCommit, SurfaceObservationKind::Dirty) => {
            vec![(pattern, "recent_commit_dirty_overlap".to_string(), 5)]
        }
        _ => Vec::new(),
    }
}

fn observation_label(observation: &SurfaceObservation) -> String {
    let kind = match observation.kind {
        SurfaceObservationKind::Bead => "bead",
        SurfaceObservationKind::Dirty => "dirty",
        SurfaceObservationKind::RecentCommit => "recent_commit",
        SurfaceObservationKind::Reservation => "reservation",
    };
    format!("{kind}:{}", observation.pattern)
}

fn recommend_swarm_brief_actions(report: &SwarmBriefReport) -> Vec<SwarmBriefRecommendation> {
    let mut recommendations = Vec::new();
    recommendations.extend(degraded_capability_recommendations(report));
    recommendations.extend(resource_pressure_recommendations(report));
    recommendations.extend(surface_conflict_recommendations(report));

    if report.beads.ready.is_empty() {
        recommendations.push(no_ready_work_recommendation(report));
    } else {
        for bead in &report.beads.ready {
            recommendations.push(ready_bead_recommendation(report, bead));
        }
    }

    for bead in &report.beads.in_progress {
        recommendations.push(in_progress_follow_up_recommendation(report, bead));
    }

    recommendations.sort();
    recommendations.dedup_by(|left, right| left.id == right.id);
    recommendations
}

fn degraded_capability_recommendations(report: &SwarmBriefReport) -> Vec<SwarmBriefRecommendation> {
    let mut recommendations = Vec::new();
    for source in expected_sources() {
        let status = source_status(report, source);
        if status == Some(SwarmBriefSourceStatus::Ready) {
            continue;
        }

        let degradations = report
            .sources
            .iter()
            .find(|snapshot| snapshot.source == source)
            .map(|snapshot| snapshot.degraded.as_slice())
            .unwrap_or(&[]);

        if degradations.is_empty() {
            let code = format!("{}_missing", source.as_str());
            recommendations.push(SwarmBriefRecommendation {
                id: format!("rec.degraded.{}.{}", source.as_str(), code),
                kind: "degraded_capability".to_string(),
                confidence: "high".to_string(),
                severity: degraded_recommendation_severity(source, status),
                reason_codes: vec![
                    code.clone(),
                    format!(
                        "source_status:{}",
                        status.map_or("missing", SwarmBriefSourceStatus::as_str)
                    ),
                ],
                evidence: vec![format!(
                    "could_not_know:{}:{}",
                    source.as_str(),
                    missing_source_knowledge(source)
                )],
                suggested_commands: vec![default_source_repair(source).to_string()],
                must_not_do: vec![format!(
                    "Do not treat missing {} data as empty evidence.",
                    source.as_str()
                )],
            });
            continue;
        }

        for degradation in degradations {
            recommendations.push(SwarmBriefRecommendation {
                id: format!("rec.degraded.{}.{}", source.as_str(), degradation.code),
                kind: "degraded_capability".to_string(),
                confidence: "high".to_string(),
                severity: degraded_recommendation_severity(source, status),
                reason_codes: vec![
                    degradation.code.clone(),
                    format!(
                        "source_status:{}",
                        status.map_or("missing", SwarmBriefSourceStatus::as_str)
                    ),
                ],
                evidence: vec![format!(
                    "could_not_know:{}:{}",
                    source.as_str(),
                    missing_source_knowledge(source)
                )],
                suggested_commands: vec![
                    degradation
                        .repair
                        .clone()
                        .unwrap_or_else(|| default_source_repair(source).to_string()),
                ],
                must_not_do: vec![format!(
                    "Do not treat degraded {} data as complete evidence.",
                    source.as_str()
                )],
            });
        }
    }
    recommendations
}

fn resource_pressure_recommendations(report: &SwarmBriefReport) -> Vec<SwarmBriefRecommendation> {
    let pressured_hints = report
        .resource_pressure
        .iter()
        .filter(|hint| hint.level == "high" || hint.level == "medium")
        .collect::<Vec<_>>();
    let constrained_host = report.host_profile.as_ref().is_some_and(|profile| {
        profile.recommended_profile == "constrained" || profile.recommended_profile == "portable"
    });

    if pressured_hints.is_empty() && !constrained_host {
        return Vec::new();
    }

    let mut reason_codes = BTreeSet::new();
    let mut evidence = BTreeSet::new();
    for hint in pressured_hints {
        reason_codes.insert(format!("resource_pressure_{}", hint.level));
        evidence.insert(format!("{}:{}", hint.source.as_str(), hint.message));
    }
    if let Some(profile) = &report.host_profile
        && constrained_host
    {
        reason_codes.insert("host_profile_prefers_rch_for_heavy_verification".to_string());
        evidence.insert(format!(
            "host_profile:{}:{}",
            profile.recommended_profile, profile.confidence
        ));
    }
    reason_codes.insert("cargo_verification_must_use_rch".to_string());

    vec![SwarmBriefRecommendation {
        id: "rec.resource_pressure.use_rch_for_cargo".to_string(),
        kind: "resource_pressure".to_string(),
        confidence: coordination_confidence(report),
        severity: if reason_codes.contains("resource_pressure_high") {
            "high".to_string()
        } else {
            "medium".to_string()
        },
        reason_codes: reason_codes.into_iter().collect(),
        evidence: evidence.into_iter().collect(),
        suggested_commands: vec![
            "RCH_VISIBILITY=summary RCH_QUEUE_WHEN_BUSY=1 rch exec -- env CARGO_TARGET_DIR=\"${TMPDIR:-/tmp}/rch_target_eidetic_engine_cli\" cargo check --all-targets".to_string(),
            "RCH_VISIBILITY=summary RCH_QUEUE_WHEN_BUSY=1 rch exec -- env CARGO_TARGET_DIR=\"${TMPDIR:-/tmp}/rch_target_eidetic_engine_cli\" cargo clippy --all-targets -- -D warnings".to_string(),
        ],
        must_not_do: vec![
            "Do not run local cargo verification when resource pressure is medium or high."
                .to_string(),
            "Do not clean target directories or temporary build artifacts without explicit permission."
                .to_string(),
        ],
    }]
}

fn surface_conflict_recommendations(report: &SwarmBriefReport) -> Vec<SwarmBriefRecommendation> {
    report
        .file_surface_risks
        .iter()
        .filter(|risk| {
            risk.risk_factors
                .iter()
                .any(|factor| factor.contains("reservation_overlap"))
                || risk
                    .risk_factors
                    .iter()
                    .any(|factor| factor == "active_exclusive_reservation")
        })
        .map(|risk| SwarmBriefRecommendation {
            id: format!("rec.surface_conflict.{}", stable_id_fragment(&risk.path_pattern)),
            kind: "file_surface_conflict".to_string(),
            confidence: coordination_confidence(report),
            severity: risk.severity.clone(),
            reason_codes: risk.risk_factors.clone(),
            evidence: risk.evidence.clone(),
            suggested_commands: vec![
                "Check Agent Mail reservations before editing this surface.".to_string(),
                "Coordinate with the reservation holder or choose a non-overlapping ready bead."
                    .to_string(),
            ],
            must_not_do: vec![
                "Do not edit a surface that overlaps an active exclusive reservation without coordination."
                    .to_string(),
            ],
        })
        .collect()
}

fn no_ready_work_recommendation(report: &SwarmBriefReport) -> SwarmBriefRecommendation {
    let mut evidence = vec![format!("beads.ready:{}", report.beads.ready.len())];
    if let Some(bv) = &report.bv
        && let Some(actionable_count) = bv.actionable_count
    {
        evidence.push(format!("bv.actionable_count:{actionable_count}"));
    }

    SwarmBriefRecommendation {
        id: "rec.work_selection.no_ready_beads".to_string(),
        kind: "work_selection".to_string(),
        confidence: coordination_confidence(report),
        severity: "medium".to_string(),
        reason_codes: vec!["no_ready_work".to_string()],
        evidence,
        suggested_commands: vec![
            "bv --robot-triage".to_string(),
            "br blocked --json".to_string(),
        ],
        must_not_do: vec![
            "Do not claim a blocked bead without resolving its dependencies.".to_string(),
            "Do not infer the project is done from Beads alone when any source is degraded."
                .to_string(),
        ],
    }
}

fn ready_bead_recommendation(
    report: &SwarmBriefReport,
    bead: &SwarmBriefBead,
) -> SwarmBriefRecommendation {
    let surfaces = likely_surfaces_for_bead(bead);
    let related_risks = risks_for_surfaces(&report.file_surface_risks, &surfaces);
    let max_score = related_risks
        .iter()
        .map(|risk| risk.score)
        .max()
        .unwrap_or(0);
    let mut reason_codes = BTreeSet::from(["ready_bead_available".to_string()]);
    let mut evidence = BTreeSet::from([format!(
        "bead:{}:{}:{}",
        bead.id, bead.source_bucket, bead.title
    )]);

    if let Some(priority) = bead.priority {
        evidence.insert(format!("bead_priority:{priority}"));
    }
    if let Some(score) = bv_score_for_bead(report, &bead.id) {
        reason_codes.insert("bv_top_pick".to_string());
        evidence.insert(format!("bv_score_milli:{score}"));
    }
    if surfaces.is_empty() {
        reason_codes.insert("no_likely_file_scope".to_string());
    } else {
        for surface in &surfaces {
            evidence.insert(format!("likely_surface:{surface}"));
        }
    }
    if is_docs_or_tests_bead(bead) {
        reason_codes.insert("docs_test_only_safe_surface".to_string());
    }

    for risk in related_risks {
        evidence.extend(risk.evidence.iter().cloned());
        for factor in &risk.risk_factors {
            reason_codes.insert(factor.clone());
        }
    }

    let conflict = max_score >= 50;
    let severity = if max_score >= 70 {
        "high"
    } else if max_score >= 35 {
        "medium"
    } else {
        "low"
    };
    let mut must_not_do = vec![
        "Do not start editing without an Agent Mail file reservation on the likely surface."
            .to_string(),
        "Do not run local cargo verification; use rch for build and test gates.".to_string(),
    ];
    if conflict {
        must_not_do.push(
            "Do not claim this bead until active reservation conflicts are coordinated."
                .to_string(),
        );
        reason_codes.insert("candidate_blocked_by_surface_conflict".to_string());
    }

    SwarmBriefRecommendation {
        id: format!("rec.candidate.{}", bead.id),
        kind: if conflict {
            "candidate_blocked_by_surface_conflict".to_string()
        } else if is_docs_or_tests_bead(bead) {
            "safe_surface_candidate".to_string()
        } else {
            "candidate_work".to_string()
        },
        confidence: coordination_confidence(report),
        severity: severity.to_string(),
        reason_codes: reason_codes.into_iter().collect(),
        evidence: evidence.into_iter().collect(),
        suggested_commands: vec![
            format!("br show {} --json", bead.id),
            format!("br update {} --status in_progress --json", bead.id),
            "Reserve likely surfaces through Agent Mail before editing.".to_string(),
        ],
        must_not_do,
    }
}

fn in_progress_follow_up_recommendation(
    report: &SwarmBriefReport,
    bead: &SwarmBriefBead,
) -> SwarmBriefRecommendation {
    let mut reason_codes = vec!["in_progress_owner_follow_up".to_string()];
    let mut evidence = vec![format!("bead:{}:{}:{}", bead.id, bead.status, bead.title)];
    if let Some(assignee) = &bead.assignee {
        evidence.push(format!("assignee:{assignee}"));
    } else {
        reason_codes.push("in_progress_without_assignee".to_string());
    }
    if source_status(report, SwarmBriefSourceKind::AgentMail) != Some(SwarmBriefSourceStatus::Ready)
    {
        reason_codes.push("agent_mail_needed_for_owner_freshness".to_string());
    }

    SwarmBriefRecommendation {
        id: format!("rec.in_progress_follow_up.{}", bead.id),
        kind: "stale_in_progress_follow_up".to_string(),
        confidence: coordination_confidence(report),
        severity: "medium".to_string(),
        reason_codes,
        evidence,
        suggested_commands: vec![
            format!("br show {} --json", bead.id),
            format!("Search Agent Mail for thread {}", bead.id),
        ],
        must_not_do: vec![
            "Do not steal or reopen in-progress work without checking the owner/thread first."
                .to_string(),
        ],
    }
}

fn derive_host_pressure_hints(
    profile: Option<&SwarmBriefHostProfileSummary>,
) -> Vec<SwarmBriefResourcePressureHint> {
    let Some(profile) = profile else {
        return Vec::new();
    };
    if profile.recommended_profile != "constrained" && profile.recommended_profile != "portable" {
        return Vec::new();
    }
    vec![SwarmBriefResourcePressureHint {
        source: SwarmBriefSourceKind::HostProfile,
        level: if profile.recommended_profile == "constrained" {
            "high"
        } else {
            "medium"
        }
        .to_string(),
        message: format!(
            "host profile {} recommends RCH for heavy cargo verification",
            profile.recommended_profile
        ),
    }]
}

fn all_swarm_brief_beads(summary: &SwarmBriefBeadsSummary) -> Vec<&SwarmBriefBead> {
    summary
        .ready
        .iter()
        .chain(summary.blocked.iter())
        .chain(summary.in_progress.iter())
        .chain(summary.deferred.iter())
        .collect()
}

fn likely_surfaces_for_bead(bead: &SwarmBriefBead) -> Vec<String> {
    likely_surfaces_for_text(&format!("{} {}", bead.id, bead.title))
}

fn likely_surfaces_for_text(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let mut surfaces = BTreeSet::new();
    if lower.contains("swarm-brief") || lower.contains("swarm brief") {
        surfaces.insert("src/core/swarm_brief.rs".to_string());
    }
    if lower.contains("[cli]") || lower.contains(" cli") || lower.contains("command") {
        surfaces.insert("src/cli/**".to_string());
    }
    if lower.contains("[docs]") || lower.contains("docs") || lower.contains("readme") {
        surfaces.insert("README.md".to_string());
        surfaces.insert("docs/**".to_string());
    }
    if lower.contains("[e2e]")
        || lower.contains("e2e")
        || lower.contains("test")
        || lower.contains("golden")
        || lower.contains("contract")
    {
        surfaces.insert("tests/**".to_string());
    }
    if lower.contains("pack-quality") || lower.contains("eval") {
        surfaces.insert("src/eval/**".to_string());
        surfaces.insert("tests/fixtures/eval/**".to_string());
    }
    if lower.contains("support-bundle") || lower.contains("support bundle") {
        surfaces.insert("src/core/support_bundle.rs".to_string());
    }
    surfaces.into_iter().collect()
}

fn risks_for_surfaces<'a>(
    risks: &'a [SwarmBriefFileSurfaceRisk],
    surfaces: &[String],
) -> Vec<&'a SwarmBriefFileSurfaceRisk> {
    risks
        .iter()
        .filter(|risk| {
            surfaces
                .iter()
                .any(|surface| surfaces_overlap(&risk.path_pattern, surface))
        })
        .collect()
}

fn surfaces_overlap(left: &str, right: &str) -> bool {
    let left = surface_prefix(left);
    let right = surface_prefix(right);
    left == right || left.starts_with(&right) || right.starts_with(&left)
}

fn surface_prefix(pattern: &str) -> String {
    let pattern = pattern.split('*').next().unwrap_or(pattern);
    pattern
        .trim_end_matches("/**")
        .trim_end_matches("/*")
        .trim_end_matches('/')
        .to_string()
}

fn narrower_surface_pattern(left: &str, right: &str) -> String {
    let left_prefix = surface_prefix(left);
    let right_prefix = surface_prefix(right);
    if left_prefix.len() >= right_prefix.len() {
        left.to_string()
    } else {
        right.to_string()
    }
}

fn severity_for_score(score: u16) -> &'static str {
    if score >= 70 {
        "high"
    } else if score >= 35 {
        "medium"
    } else {
        "low"
    }
}

fn expected_sources() -> [SwarmBriefSourceKind; 7] {
    [
        SwarmBriefSourceKind::AgentInventory,
        SwarmBriefSourceKind::AgentMail,
        SwarmBriefSourceKind::Beads,
        SwarmBriefSourceKind::Bv,
        SwarmBriefSourceKind::Git,
        SwarmBriefSourceKind::HostProfile,
        SwarmBriefSourceKind::Rch,
    ]
}

fn source_status(
    report: &SwarmBriefReport,
    source: SwarmBriefSourceKind,
) -> Option<SwarmBriefSourceStatus> {
    report
        .sources
        .iter()
        .find(|snapshot| snapshot.source == source)
        .map(|snapshot| snapshot.status)
}

fn default_source_repair(source: SwarmBriefSourceKind) -> &'static str {
    match source {
        SwarmBriefSourceKind::AgentInventory => "ee agent status --json",
        SwarmBriefSourceKind::AgentMail => {
            "Configure a redacted Agent Mail snapshot path before collecting the brief."
        }
        SwarmBriefSourceKind::Beads => "br ready --json",
        SwarmBriefSourceKind::Bv => "bv --robot-triage --robot-triage-by-track",
        SwarmBriefSourceKind::Git => "git status --short --branch --untracked-files=all",
        SwarmBriefSourceKind::HostProfile => "ee profile probe --json",
        SwarmBriefSourceKind::Rch => "rch status --json",
    }
}

fn missing_source_knowledge(source: SwarmBriefSourceKind) -> &'static str {
    match source {
        SwarmBriefSourceKind::AgentInventory => "active local agent inventory",
        SwarmBriefSourceKind::AgentMail => "active reservations, unread mail, and thread freshness",
        SwarmBriefSourceKind::Beads => "ready, blocked, deferred, and in-progress work",
        SwarmBriefSourceKind::Bv => "critical path and graph-aware priority",
        SwarmBriefSourceKind::Git => "dirty files and recent commit surfaces",
        SwarmBriefSourceKind::HostProfile => "local CPU, memory, and profile pressure",
        SwarmBriefSourceKind::Rch => "remote build queue and active build pressure",
    }
}

fn degraded_recommendation_severity(
    source: SwarmBriefSourceKind,
    status: Option<SwarmBriefSourceStatus>,
) -> String {
    if source == SwarmBriefSourceKind::Git || source == SwarmBriefSourceKind::Beads {
        "high".to_string()
    } else if status == Some(SwarmBriefSourceStatus::Skipped) {
        "low".to_string()
    } else {
        "medium".to_string()
    }
}

fn coordination_confidence(report: &SwarmBriefReport) -> String {
    let critical_degraded = [SwarmBriefSourceKind::Git, SwarmBriefSourceKind::Beads]
        .iter()
        .any(|source| source_status(report, *source) != Some(SwarmBriefSourceStatus::Ready));
    if critical_degraded {
        "low".to_string()
    } else if report
        .sources
        .iter()
        .any(|source| source.status != SwarmBriefSourceStatus::Ready)
    {
        "medium".to_string()
    } else {
        "high".to_string()
    }
}

fn bv_score_for_bead(report: &SwarmBriefReport, bead_id: &str) -> Option<u32> {
    report.bv.as_ref().and_then(|summary| {
        summary
            .top_picks
            .iter()
            .find(|pick| pick.id == bead_id)
            .and_then(|pick| pick.score_milli)
    })
}

fn is_docs_or_tests_bead(bead: &SwarmBriefBead) -> bool {
    let lower = bead.title.to_ascii_lowercase();
    lower.contains("[docs]")
        || lower.contains("[e2e]")
        || lower.contains("docs")
        || lower.contains("test")
        || lower.contains("golden")
        || lower.contains("contract")
}

fn stable_id_fragment(input: &str) -> String {
    let mut output = String::new();
    let mut last_was_separator = false;
    for character in input.chars() {
        if character.is_ascii_alphanumeric() {
            output.push(character.to_ascii_lowercase());
            last_was_separator = false;
        } else if !last_was_separator {
            output.push('_');
            last_was_separator = true;
        }
    }
    let trimmed = output.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "surface".to_string()
    } else {
        trimmed
    }
}

#[must_use]
pub fn parse_git_status_short(input: &str) -> Vec<SwarmBriefDirtyFile> {
    let mut files = input
        .lines()
        .filter(|line| !line.starts_with("## "))
        .filter_map(|line| {
            let status = line.get(..2)?.trim();
            let path = line.get(3..)?.trim();
            if status.is_empty() || path.is_empty() {
                return None;
            }
            let path = path
                .rsplit_once(" -> ")
                .map_or(path, |(_, new_path)| new_path)
                .trim();
            Some(SwarmBriefDirtyFile {
                path: redact_path_label(Path::new(path)),
                status: status.to_string(),
            })
        })
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();
    files
}

#[must_use]
pub fn parse_git_log(input: &str) -> Vec<SwarmBriefCommit> {
    let mut commits = input
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\x1f');
            let hash = parts.next()?.trim();
            let authored_at_epoch_seconds = parts.next()?.trim().parse::<i64>().ok();
            let subject = parts.next()?.trim();
            if hash.is_empty() || subject.is_empty() {
                return None;
            }
            Some(SwarmBriefCommit {
                hash: hash.chars().take(12).collect(),
                authored_at_epoch_seconds,
                subject: redact_brief_text(subject),
            })
        })
        .collect::<Vec<_>>();
    commits.sort_by(|left, right| {
        right
            .authored_at_epoch_seconds
            .cmp(&left.authored_at_epoch_seconds)
            .then_with(|| left.hash.cmp(&right.hash))
            .then_with(|| left.subject.cmp(&right.subject))
    });
    commits.dedup_by(|left, right| left.hash == right.hash);
    commits
}

pub fn parse_beads_json(input: &str, source_bucket: &str) -> Result<Vec<SwarmBriefBead>, String> {
    let value = serde_json::from_str::<Value>(input)
        .map_err(|error| format!("Beads JSON could not be parsed: {error}"))?;
    let array =
        value_array(&value).ok_or_else(|| "Beads JSON did not contain an array.".to_string())?;
    let mut beads = array
        .iter()
        .filter_map(|item| parse_bead_item(item, source_bucket))
        .collect::<Vec<_>>();
    beads.sort();
    beads.dedup_by(|left, right| left.id == right.id && left.source_bucket == right.source_bucket);
    Ok(beads)
}

fn parse_bead_item(item: &Value, source_bucket: &str) -> Option<SwarmBriefBead> {
    let id = string_field(item, &["id", "issue_id"])?;
    let title = string_field(item, &["title"]).unwrap_or_else(|| id.clone());
    let status = string_field(item, &["status"]).unwrap_or_else(|| source_bucket.to_string());
    let priority = item.get("priority").and_then(Value::as_i64);
    let assignee = string_field(item, &["assignee", "assigned_to", "owner"]);
    Some(SwarmBriefBead {
        id: redact_brief_text(&id),
        title: redact_brief_text(&title),
        status: redact_brief_text(&status),
        priority,
        assignee: assignee.map(|value| redact_brief_text(&value)),
        source_bucket: source_bucket.to_string(),
    })
}

pub fn parse_bv_triage_json(input: &str) -> Result<SwarmBriefBvSummary, String> {
    let value = serde_json::from_str::<Value>(input)
        .map_err(|error| format!("BV robot JSON could not be parsed: {error}"))?;
    let quick_ref = value
        .pointer("/triage/quick_ref")
        .or_else(|| value.get("quick_ref"))
        .ok_or_else(|| "BV robot JSON did not contain triage.quick_ref.".to_string())?;
    let picks_value = quick_ref
        .get("top_picks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut top_picks = picks_value
        .iter()
        .filter_map(parse_bv_pick)
        .collect::<Vec<_>>();
    top_picks.sort_by(|left, right| {
        right
            .score_milli
            .cmp(&left.score_milli)
            .then_with(|| left.id.cmp(&right.id))
            .then_with(|| left.title.cmp(&right.title))
    });
    top_picks.dedup_by(|left, right| left.id == right.id);
    Ok(SwarmBriefBvSummary {
        actionable_count: quick_ref.get("actionable_count").and_then(Value::as_u64),
        blocked_count: quick_ref.get("blocked_count").and_then(Value::as_u64),
        in_progress_count: quick_ref.get("in_progress_count").and_then(Value::as_u64),
        track_count: value
            .pointer("/triage/recommendations_by_track")
            .and_then(Value::as_array)
            .map(|items| items.len() as u64),
        top_picks,
    })
}

fn parse_bv_pick(item: &Value) -> Option<SwarmBriefBvPick> {
    let id = string_field(item, &["id"])?;
    let title = string_field(item, &["title"]).unwrap_or_else(|| id.clone());
    let score_milli = item
        .get("score")
        .and_then(Value::as_f64)
        .filter(|score| score.is_finite() && *score >= 0.0)
        .map(|score| (score * 1_000.0).round().clamp(0.0, u32::MAX as f64) as u32);
    Some(SwarmBriefBvPick {
        id: redact_brief_text(&id),
        title: redact_brief_text(&title),
        score_milli,
    })
}

pub fn parse_agent_mail_snapshot_json(input: &str) -> Result<SwarmBriefAgentMailSnapshot, String> {
    let value = serde_json::from_str::<Value>(input)
        .map_err(|error| format!("Agent Mail snapshot JSON could not be parsed: {error}"))?;
    let reservations = value
        .get("file_reservations")
        .or_else(|| value.get("reservations"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(parse_file_reservation)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let inbox = value
        .get("inbox")
        .or_else(|| value.get("mailboxes"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(parse_inbox_summary)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let threads = value
        .get("threads")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(parse_thread_summary)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut reservations = reservations;
    let mut inbox = inbox;
    let mut threads = threads;
    reservations.sort();
    reservations.dedup();
    inbox.sort();
    inbox.dedup();
    threads.sort();
    threads.dedup();
    Ok(SwarmBriefAgentMailSnapshot {
        file_reservations: reservations,
        inbox,
        threads,
    })
}

fn parse_file_reservation(item: &Value) -> Option<SwarmBriefFileReservation> {
    let path_pattern = string_field(item, &["path_pattern", "path", "pattern"])?;
    let holder = string_field(item, &["holder", "agent_name", "agent", "owner"])?;
    Some(SwarmBriefFileReservation {
        path_pattern: redact_path_label(Path::new(&path_pattern)),
        holder: redact_brief_text(&holder),
        exclusive: item
            .get("exclusive")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        expires_at: string_field(item, &["expires_ts", "expires_at"]),
    })
}

fn parse_inbox_summary(item: &Value) -> Option<SwarmBriefInboxSummary> {
    let mailbox = string_field(item, &["mailbox", "agent_name", "agent"])?;
    Some(SwarmBriefInboxSummary {
        mailbox: redact_brief_text(&mailbox),
        unread_count: item
            .get("unread_count")
            .or_else(|| item.get("unread"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        ack_required_count: item
            .get("ack_required_count")
            .or_else(|| item.get("ackRequired"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

fn parse_thread_summary(item: &Value) -> Option<SwarmBriefThreadSummary> {
    let thread_id = string_field(item, &["thread_id", "threadId", "id"])?;
    Some(SwarmBriefThreadSummary {
        thread_id: redact_brief_text(&thread_id),
        subject: string_field(item, &["subject"]).map(|subject| redact_brief_text(&subject)),
        message_count: item
            .get("message_count")
            .or_else(|| item.get("messageCount"))
            .and_then(Value::as_u64),
        last_activity_at: string_field(item, &["last_activity_at", "lastActivityAt"]),
    })
}

pub fn parse_rch_status_json(input: &str) -> Result<Vec<SwarmBriefResourcePressureHint>, String> {
    let value = serde_json::from_str::<Value>(input)
        .map_err(|error| format!("RCH status JSON could not be parsed: {error}"))?;
    let queue_depth = numeric_field(&value, &["queue_depth", "queueDepth", "queued"]);
    let active_builds = numeric_field(&value, &["active_builds", "activeBuilds", "running"]);
    let mut hints = Vec::new();
    if let Some(queue_depth) = queue_depth {
        let level = if queue_depth > 4 {
            "high"
        } else if queue_depth > 0 {
            "medium"
        } else {
            "low"
        };
        hints.push(SwarmBriefResourcePressureHint {
            source: SwarmBriefSourceKind::Rch,
            level: level.to_string(),
            message: format!("rch queue depth: {queue_depth}"),
        });
    }
    if let Some(active_builds) = active_builds {
        let level = if active_builds > 8 {
            "high"
        } else if active_builds > 0 {
            "medium"
        } else {
            "low"
        };
        hints.push(SwarmBriefResourcePressureHint {
            source: SwarmBriefSourceKind::Rch,
            level: level.to_string(),
            message: format!("rch active builds: {active_builds}"),
        });
    }
    if hints.is_empty() {
        hints.push(SwarmBriefResourcePressureHint {
            source: SwarmBriefSourceKind::Rch,
            level: "unknown".to_string(),
            message: "rch status did not expose queue or active build counts".to_string(),
        });
    }
    hints.sort();
    Ok(hints)
}

fn value_array(value: &Value) -> Option<&Vec<Value>> {
    value
        .as_array()
        .or_else(|| value.get("items").and_then(Value::as_array))
        .or_else(|| value.get("issues").and_then(Value::as_array))
        .or_else(|| value.get("result").and_then(Value::as_array))
        .or_else(|| value.get("recommendations").and_then(Value::as_array))
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    })
}

fn numeric_field(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_u64))
}

fn redact_brief_text(input: &str) -> String {
    redact_secret_like_content(input).content
}

fn redact_path_label(path: &Path) -> String {
    let raw = path.display().to_string();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let label = home
        .as_deref()
        .and_then(|home| redact_path_label_with_home(path, home))
        .unwrap_or(raw);
    redact_brief_text(&label)
}

fn redact_path_label_with_home(path: &Path, home: &Path) -> Option<String> {
    let relative = path.strip_prefix(home).ok()?;
    if relative.as_os_str().is_empty() {
        Some("~".to_string())
    } else {
        Some(format!("~/{}", relative.display()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[derive(Default)]
    struct FakeRunner {
        outputs: BTreeMap<String, Result<SwarmBriefCommandOutput, SwarmBriefCommandError>>,
    }

    impl FakeRunner {
        fn with_output(mut self, program: &str, args: &[&str], stdout: &str) -> Self {
            self.outputs.insert(
                command_key(program, args),
                Ok(SwarmBriefCommandOutput {
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                }),
            );
            self
        }

        fn with_error(
            mut self,
            program: &str,
            args: &[&str],
            error: SwarmBriefCommandError,
        ) -> Self {
            self.outputs.insert(command_key(program, args), Err(error));
            self
        }
    }

    impl SwarmBriefCommandRunner for FakeRunner {
        fn run(
            &self,
            program: &str,
            args: &[&str],
            _cwd: &Path,
            _timeout_ms: u64,
        ) -> Result<SwarmBriefCommandOutput, SwarmBriefCommandError> {
            self.outputs
                .get(&command_key(program, args))
                .cloned()
                .unwrap_or_else(|| {
                    Err(SwarmBriefCommandError::Unavailable(format!(
                        "{program} fixture missing"
                    )))
                })
        }
    }

    fn command_key(program: &str, args: &[&str]) -> String {
        std::iter::once(program)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn require_ok<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    fn require_some<T>(option: Option<T>, context: &str) -> T {
        match option {
            Some(value) => value,
            None => panic!("{context}"),
        }
    }

    fn bead(id: &str, title: &str, source_bucket: &str) -> SwarmBriefBead {
        SwarmBriefBead {
            id: id.to_string(),
            title: title.to_string(),
            status: source_bucket.to_string(),
            priority: Some(1),
            assignee: None,
            source_bucket: source_bucket.to_string(),
        }
    }

    fn report_with_ready_sources() -> SwarmBriefReport {
        let mut report = SwarmBriefReport::empty(Path::new("."));
        for source in expected_sources() {
            report.sources.push(SwarmBriefSourceSnapshot::ready(
                source,
                SwarmBriefSourceProvenance::local_probe(),
                0,
            ));
        }
        report
    }

    fn recommendation<'a>(report: &'a SwarmBriefReport, id: &str) -> &'a SwarmBriefRecommendation {
        require_some(
            report
                .recommendations
                .iter()
                .find(|recommendation| recommendation.id == id),
            id,
        )
    }

    #[test]
    fn advisor_reports_no_ready_work() {
        let mut report = report_with_ready_sources();
        report.bv = Some(SwarmBriefBvSummary {
            actionable_count: Some(0),
            blocked_count: Some(2),
            in_progress_count: Some(0),
            track_count: Some(1),
            top_picks: Vec::new(),
        });

        apply_swarm_brief_advice(&mut report);

        let rec = recommendation(&report, "rec.work_selection.no_ready_beads");
        assert_eq!(rec.kind, "work_selection");
        assert!(rec.reason_codes.contains(&"no_ready_work".to_string()));
        assert!(rec.evidence.contains(&"beads.ready:0".to_string()));
        assert!(
            rec.suggested_commands
                .contains(&"bv --robot-triage".to_string())
        );
    }

    #[test]
    fn advisor_recommends_clear_ready_work() {
        let mut report = report_with_ready_sources();
        report.beads.ready.push(bead(
            "eidetic_engine_cli-u7r5",
            "[swarm-brief][advisor] Add non-overlap recommendations",
            "ready",
        ));
        report.bv = Some(SwarmBriefBvSummary {
            actionable_count: Some(1),
            blocked_count: Some(0),
            in_progress_count: Some(0),
            track_count: Some(1),
            top_picks: vec![SwarmBriefBvPick {
                id: "eidetic_engine_cli-u7r5".to_string(),
                title: "top".to_string(),
                score_milli: Some(900),
            }],
        });

        apply_swarm_brief_advice(&mut report);

        let rec = recommendation(&report, "rec.candidate.eidetic_engine_cli-u7r5");
        assert_eq!(rec.kind, "candidate_work");
        assert_eq!(rec.severity, "low");
        assert_eq!(rec.confidence, "high");
        assert!(rec.reason_codes.contains(&"bv_top_pick".to_string()));
        assert!(
            rec.evidence
                .contains(&"likely_surface:src/core/swarm_brief.rs".to_string())
        );
    }

    #[test]
    fn advisor_scores_active_reservation_conflict() {
        let mut report = report_with_ready_sources();
        report.beads.ready.push(bead(
            "eidetic_engine_cli-u7r5",
            "[swarm-brief][advisor] Add recommendations",
            "ready",
        ));
        report.file_reservations.push(SwarmBriefFileReservation {
            path_pattern: "src/core/swarm_brief.rs".to_string(),
            holder: "OtherAgent".to_string(),
            exclusive: true,
            expires_at: Some("2026-05-09T08:00:00Z".to_string()),
        });

        apply_swarm_brief_advice(&mut report);

        let risk = require_some(report.file_surface_risks.first(), "surface risk");
        assert!(
            risk.risk_factors
                .contains(&"active_exclusive_reservation".to_string())
        );
        assert!(
            risk.risk_factors
                .contains(&"bead_reservation_overlap".to_string())
        );
        let rec = recommendation(&report, "rec.candidate.eidetic_engine_cli-u7r5");
        assert_eq!(rec.kind, "candidate_blocked_by_surface_conflict");
        assert!(
            rec.must_not_do
                .iter()
                .any(|item| item.contains("reservation conflicts"))
        );
    }

    #[test]
    fn advisor_scores_dirty_file_overlap() {
        let mut report = report_with_ready_sources();
        report.beads.ready.push(bead(
            "eidetic_engine_cli-u7r5",
            "[swarm-brief][advisor] Add recommendations",
            "ready",
        ));
        report.dirty_files.push(SwarmBriefDirtyFile {
            path: "src/core/swarm_brief.rs".to_string(),
            status: "M".to_string(),
        });

        apply_swarm_brief_advice(&mut report);

        let risk = require_some(report.file_surface_risks.first(), "surface risk");
        assert!(
            risk.risk_factors
                .contains(&"dirty_worktree_path".to_string())
        );
        assert!(
            risk.risk_factors
                .contains(&"dirty_bead_overlap".to_string())
        );
        let rec = recommendation(&report, "rec.candidate.eidetic_engine_cli-u7r5");
        assert!(
            rec.reason_codes
                .contains(&"candidate_blocked_by_surface_conflict".to_string())
        );
    }

    #[test]
    fn advisor_flags_in_progress_owner_follow_up() {
        let mut report = report_with_ready_sources();
        report.beads.in_progress.push(bead(
            "eidetic_engine_cli-mccc",
            "[pack-quality][e2e] Logged no-mock sentinel scenarios",
            "in_progress",
        ));

        apply_swarm_brief_advice(&mut report);

        let rec = recommendation(&report, "rec.in_progress_follow_up.eidetic_engine_cli-mccc");
        assert_eq!(rec.kind, "stale_in_progress_follow_up");
        assert!(
            rec.reason_codes
                .contains(&"in_progress_without_assignee".to_string())
        );
    }

    #[test]
    fn advisor_reports_missing_bv_capability() {
        let mut report = report_with_ready_sources();
        report
            .sources
            .retain(|source| source.source != SwarmBriefSourceKind::Bv);

        apply_swarm_brief_advice(&mut report);

        let rec = recommendation(&report, "rec.degraded.bv.bv_missing");
        assert_eq!(rec.kind, "degraded_capability");
        assert!(
            rec.suggested_commands
                .contains(&"bv --robot-triage --robot-triage-by-track".to_string())
        );
    }

    #[test]
    fn advisor_reports_missing_agent_mail_capability() {
        let mut report = report_with_ready_sources();
        report
            .sources
            .retain(|source| source.source != SwarmBriefSourceKind::AgentMail);

        apply_swarm_brief_advice(&mut report);

        let rec = recommendation(&report, "rec.degraded.agent_mail.agent_mail_missing");
        assert_eq!(rec.kind, "degraded_capability");
        assert!(
            rec.must_not_do
                .contains(&"Do not treat missing agent_mail data as empty evidence.".to_string())
        );
    }

    #[test]
    fn advisor_reports_missing_rch_capability() {
        let mut report = report_with_ready_sources();
        report
            .sources
            .retain(|source| source.source != SwarmBriefSourceKind::Rch);

        apply_swarm_brief_advice(&mut report);

        let rec = recommendation(&report, "rec.degraded.rch.rch_missing");
        assert_eq!(rec.kind, "degraded_capability");
        assert!(
            rec.suggested_commands
                .contains(&"rch status --json".to_string())
        );
    }

    #[test]
    fn advisor_prefers_rch_under_high_pressure_host() {
        let mut report = report_with_ready_sources();
        report.host_profile = Some(SwarmBriefHostProfileSummary {
            recommended_profile: "constrained".to_string(),
            confidence: "high".to_string(),
            logical_cores: Some(1),
            memory_total_bytes: Some(4),
            memory_available_bytes: Some(2),
            rch_hint_configured: true,
        });

        apply_swarm_brief_advice(&mut report);

        let rec = recommendation(&report, "rec.resource_pressure.use_rch_for_cargo");
        assert_eq!(rec.kind, "resource_pressure");
        assert_eq!(rec.severity, "high");
        assert!(
            rec.must_not_do
                .iter()
                .any(|item| item.contains("Do not run local cargo"))
        );
    }

    #[test]
    fn advisor_tie_breaking_is_deterministic() {
        let mut report = report_with_ready_sources();
        report.beads.ready.push(bead(
            "eidetic_engine_cli-zeta",
            "[docs] Document workflow",
            "ready",
        ));
        report.beads.ready.push(bead(
            "eidetic_engine_cli-alpha",
            "[docs] Document workflow",
            "ready",
        ));
        let mut second = report.clone();

        apply_swarm_brief_advice(&mut report);
        apply_swarm_brief_advice(&mut second);

        let ids = report
            .recommendations
            .iter()
            .map(|recommendation| recommendation.id.clone())
            .collect::<Vec<_>>();
        let second_ids = second
            .recommendations
            .iter()
            .map(|recommendation| recommendation.id.clone())
            .collect::<Vec<_>>();

        assert_eq!(ids, second_ids);
        assert!(
            ids.windows(2)
                .all(|window| window[0].as_str() <= window[1].as_str())
        );
    }

    #[test]
    fn git_status_parser_sorts_and_groups_dirty_files() {
        let files = parse_git_status_short(
            "## main...origin/main\n M src/z.rs\n?? src/a.rs\nR  src/old.rs -> src/new.rs\n",
        );

        assert_eq!(
            files,
            vec![
                SwarmBriefDirtyFile {
                    path: "src/a.rs".to_string(),
                    status: "??".to_string(),
                },
                SwarmBriefDirtyFile {
                    path: "src/new.rs".to_string(),
                    status: "R".to_string(),
                },
                SwarmBriefDirtyFile {
                    path: "src/z.rs".to_string(),
                    status: "M".to_string(),
                },
            ]
        );
    }

    #[test]
    fn git_log_parser_redacts_secret_like_subjects_and_sorts_by_time() {
        let commits = parse_git_log(
            "aaaaaaaaaaaaaaaa\x1f10\x1fuse token ghp_abcdefghijklmnopqrstuvwxyz123456\nbbbbbbbbbbbbbbbb\x1f20\x1fnewer commit\n",
        );

        assert_eq!(commits[0].hash, "bbbbbbbbbbbb");
        assert_eq!(commits[1].hash, "aaaaaaaaaaaa");
        assert!(!commits[1].subject.contains("ghp_"));
        assert!(commits[1].subject.contains("[REDACTED"));
    }

    #[test]
    fn path_label_redacts_home_prefix() {
        let path = Path::new("/home/alice/project/src/lib.rs");
        let home = Path::new("/home/alice");

        assert_eq!(
            redact_path_label_with_home(path, home),
            Some("~/project/src/lib.rs".to_string())
        );
    }

    #[test]
    fn beads_parser_accepts_ready_array_and_sorts() {
        let beads = require_ok(
            parse_beads_json(
                r#"[
              {"id":"b2","title":"second","status":"open","priority":2,"assignee":"agent-b"},
              {"id":"b1","title":"first","priority":1}
            ]"#,
                "ready",
            ),
            "valid beads JSON",
        );

        assert_eq!(beads[0].id, "b1");
        assert_eq!(beads[0].status, "ready");
        assert_eq!(beads[1].assignee.as_deref(), Some("agent-b"));
    }

    #[test]
    fn bv_parser_uses_robot_triage_shape_only() {
        let summary = require_ok(
            parse_bv_triage_json(
                r#"{
              "triage": {
                "quick_ref": {
                  "actionable_count": 3,
                  "blocked_count": 12,
                  "in_progress_count": 1,
                  "top_picks": [
                    {"id":"work-2","title":"second","score":0.25},
                    {"id":"work-1","title":"first","score":0.5}
                  ]
                },
                "recommendations_by_track": [
                  {"track_id":"track-A"},
                  {"track_id":"track-B"}
                ]
              }
            }"#,
            ),
            "valid bv JSON",
        );

        assert_eq!(summary.actionable_count, Some(3));
        assert_eq!(summary.track_count, Some(2));
        assert_eq!(summary.top_picks[0].id, "work-1");
        assert_eq!(summary.top_picks[0].score_milli, Some(500));
    }

    #[test]
    fn agent_mail_snapshot_omits_raw_bodies() {
        let snapshot = require_ok(
            parse_agent_mail_snapshot_json(
                r#"{
              "file_reservations": [
                {"path_pattern":"src/core/*.rs","holder":"IndigoBrook","exclusive":true,"expires_ts":"2026-05-09T00:00:00Z"}
              ],
              "inbox": [
                {"mailbox":"IndigoBrook","unread_count":2,"ack_required_count":1,"body_md":"SECRET_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz123456"}
              ],
              "threads": [
                {"thread_id":"eidetic_engine_cli-abwd","subject":"Use token ghp_abcdefghijklmnopqrstuvwxyz123456","message_count":3,"body_md":"raw body"}
              ]
            }"#,
            ),
            "valid mail snapshot",
        );

        let reservations = &snapshot.file_reservations;
        let inbox = &snapshot.inbox;
        let threads = &snapshot.threads;

        assert_eq!(reservations.len(), 1);
        assert_eq!(reservations[0].path_pattern, "src/core/*.rs");
        assert_eq!(inbox[0].unread_count, 2);
        assert_eq!(threads[0].thread_id, "eidetic_engine_cli-abwd");
        let subject = require_some(threads[0].subject.as_ref(), "subject");
        assert!(!subject.contains("ghp_"));
        let json = require_ok(serde_json::to_string(&snapshot), "serialize");
        assert!(!json.contains("SECRET_TOKEN"));
        assert!(!json.contains("body_md"));
        assert!(!json.contains("raw body"));
    }

    #[test]
    fn rch_parser_reports_queue_pressure() {
        let hints = require_ok(
            parse_rch_status_json(r#"{"queueDepth":5,"activeBuilds":2}"#),
            "valid rch JSON",
        );
        let by_message = hints
            .iter()
            .map(|hint| (hint.message.as_str(), hint.level.as_str()))
            .collect::<BTreeMap<_, _>>();

        assert_eq!(by_message["rch active builds: 2"], "medium");
        assert_eq!(by_message["rch queue depth: 5"], "high");
    }

    #[test]
    fn command_error_maps_to_stable_degradation_without_raw_secret() {
        let error = SwarmBriefCommandError::Failed {
            status: Some(1),
            stderr: "token=ghp_abcdefghijklmnopqrstuvwxyz123456".to_string(),
        };
        let degradation = error.to_degradation(
            SwarmBriefSourceKind::Beads,
            BEADS_UNAVAILABLE_CODE,
            "br ready --json",
        );

        assert_eq!(degradation.code, BEADS_UNAVAILABLE_CODE);
        assert!(!degradation.message.contains("ghp_"));
        assert_eq!(degradation.repair.as_deref(), Some("br ready --json"));
    }

    #[test]
    fn collector_degrades_missing_optional_sources_deterministically() {
        let options = SwarmBriefCollectOptions::for_workspace(".");
        let runner = FakeRunner::default()
            .with_output(
                "git",
                &["status", "--short", "--branch", "--untracked-files=all"],
                " M src/core/mod.rs\n",
            )
            .with_output(
                "git",
                &["log", "-n", "8", "--format=%H%x1f%ct%x1f%s"],
                "aaaaaaaaaaaaaaaa\x1f20\x1fcommit subject\n",
            )
            .with_error(
                "br",
                &["ready", "--json"],
                SwarmBriefCommandError::TimedOut { timeout_ms: 1_500 },
            )
            .with_error(
                "br",
                &["blocked", "--json"],
                SwarmBriefCommandError::TimedOut { timeout_ms: 1_500 },
            )
            .with_error(
                "br",
                &["list", "--status", "in_progress", "--json"],
                SwarmBriefCommandError::TimedOut { timeout_ms: 1_500 },
            )
            .with_error(
                "br",
                &["list", "--status", "deferred", "--json"],
                SwarmBriefCommandError::TimedOut { timeout_ms: 1_500 },
            )
            .with_error(
                "bv",
                &["--robot-triage", "--robot-triage-by-track"],
                SwarmBriefCommandError::Unavailable("bv missing".to_string()),
            );

        let report = collect_swarm_brief(&options, &runner);

        assert_eq!(report.schema, SWARM_BRIEF_SCHEMA_V1);
        assert_eq!(report.dirty_files.len(), 1);
        assert!(
            report
                .degraded
                .iter()
                .any(|degraded| degraded.code == BEADS_UNAVAILABLE_CODE)
        );
        assert!(
            report
                .sources
                .iter()
                .any(|source| source.source == SwarmBriefSourceKind::AgentMail
                    && source.status == SwarmBriefSourceStatus::NotConfigured)
        );
    }
}
