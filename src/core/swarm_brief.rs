//! Read-only coordination snapshot model for swarm preflight briefs.
//!
//! This module owns source collection, normalization, and deterministic advice.
//! Public CLI rendering is wired through `ee swarm brief`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Value, json};

use crate::core::agent_detect::{AgentInventoryStatus, AgentStatusOptions, gather_agent_status};
use crate::core::profile::{HostResourceProbeReport, recommend_operating_profile};
use crate::core::singleflight::singleflight_posture_report;
use crate::policy::redact_secret_like_content;

pub const SWARM_BRIEF_SCHEMA_V1: &str = "ee.swarm.brief.v1";
pub const SWARM_BRIEF_REDACTION_STATUS: &str = "paths_counts_subjects_only_no_content";
pub const SWARM_BRIEF_SUMMARY_SCHEMA_V1: &str = "ee.support_bundle.swarm_brief_summary.v1";
pub const SWARM_BRIEF_SUMMARY_REDACTION_STATUS: &str =
    "counts_hashes_codes_ids_only_no_mail_body_no_raw_queries_no_file_listings";

const GIT_UNAVAILABLE_CODE: &str = "git_unavailable";
const BEADS_UNAVAILABLE_CODE: &str = "beads_unavailable";
const BEADS_TRACKER_STALE_CODE: &str = "beads_tracker_stale";
const BV_UNAVAILABLE_CODE: &str = "bv_unavailable";
const AGENT_MAIL_UNAVAILABLE_CODE: &str = "agent_mail_unavailable";
const RCH_UNAVAILABLE_CODE: &str = "rch_unavailable";
const RCH_WORKER_TOPOLOGY_BLOCKED_CODE: &str = "rch_worker_topology_blocked";
const RCH_REMOTE_REQUIRED_FALLBACK_PREVENTED_CODE: &str = "rch_remote_required_fallback_prevented";
const RCH_POSTURE_REMOTE_READY: &str = "remote_ready";
const RCH_POSTURE_NO_REMOTE_WORKERS: &str = "no_remote_workers";
const RCH_POSTURE_WORKER_UNREACHABLE: &str = "worker_unreachable";
const AGENT_STATUS_UNAVAILABLE_CODE: &str = "agent_status_unavailable";
const MAX_SWARM_BRIEF_SUMMARY_RECOMMENDATIONS: usize = 5;

/// Options used by the internal source collection layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwarmBriefCollectOptions {
    pub workspace: PathBuf,
    pub max_recent_commits: usize,
    pub include_rch: bool,
    pub enabled_sources: BTreeSet<SwarmBriefSourceKind>,
    pub agent_mail_snapshot_path: Option<PathBuf>,
    pub agent_inventory_only_connectors: Option<Vec<String>>,
    pub command_timeout_ms: u64,
}

impl SwarmBriefCollectOptions {
    #[must_use]
    pub fn for_workspace(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            max_recent_commits: 8,
            include_rch: false,
            enabled_sources: default_swarm_brief_sources(),
            agent_mail_snapshot_path: None,
            agent_inventory_only_connectors: None,
            command_timeout_ms: 1_500,
        }
    }
}

#[must_use]
pub fn default_swarm_brief_sources() -> BTreeSet<SwarmBriefSourceKind> {
    [
        SwarmBriefSourceKind::AgentInventory,
        SwarmBriefSourceKind::AgentMail,
        SwarmBriefSourceKind::Beads,
        SwarmBriefSourceKind::Bv,
        SwarmBriefSourceKind::Git,
        SwarmBriefSourceKind::HostProfile,
    ]
    .into_iter()
    .collect()
}

#[must_use]
pub fn all_swarm_brief_sources() -> BTreeSet<SwarmBriefSourceKind> {
    [
        SwarmBriefSourceKind::AgentInventory,
        SwarmBriefSourceKind::AgentMail,
        SwarmBriefSourceKind::Beads,
        SwarmBriefSourceKind::Bv,
        SwarmBriefSourceKind::Git,
        SwarmBriefSourceKind::HostProfile,
        SwarmBriefSourceKind::Rch,
    ]
    .into_iter()
    .collect()
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rch_local_capability: Option<RchLocalCapabilityReport>,
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
            rch_local_capability: None,
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
    Qos,
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
            Self::Qos => "qos",
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

    fn with_freshness(mut self, freshness: SwarmBriefSourceFreshness) -> Self {
        if freshness.state != "current" && self.status == SwarmBriefSourceStatus::Ready {
            self.status = SwarmBriefSourceStatus::Degraded;
        }
        self.freshness = freshness;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependency_cycle_summary: Option<SwarmBriefBeadsDependencyCycleSummary>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefBeadsDependencyCycleSummary {
    pub count: u64,
    pub examples: Vec<Vec<String>>,
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
    pub git_status_buckets: Vec<String>,
    pub reservation_holders: Vec<String>,
    pub related_bead_ids: Vec<String>,
    pub severity: String,
    pub score: u16,
    pub risk_factors: Vec<String>,
    pub evidence: Vec<String>,
    pub suggested_commands: Vec<String>,
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
    #[serde(skip)]
    pub degraded: Vec<SwarmBriefDegradation>,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchCodexHookCapability {
    pub installed: bool,
    pub status: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchWorkerProbeSummary {
    pub healthy_count: u64,
    pub failed_count: u64,
    pub status: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchQueueHealth {
    pub queued_count: u64,
    pub active_count: u64,
    pub slots_available: Option<u64>,
    pub status: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchLocalCapabilityReport {
    pub schema: &'static str,
    pub cli_version: Option<String>,
    pub direct_exec_available: bool,
    pub codex_hook: RchCodexHookCapability,
    pub daemon_status_socket: Option<String>,
    pub status_socket_consistent: Option<bool>,
    pub dry_run_would_offload: Option<bool>,
    pub worker_probe_summary: RchWorkerProbeSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_health: Option<RchQueueHealth>,
    pub remote_only_required: bool,
    pub remote_only_safe: bool,
    pub degraded: Vec<SwarmBriefDegradation>,
    pub recovery: Vec<String>,
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
        timeout_ms: u64,
    ) -> Result<SwarmBriefCommandOutput, SwarmBriefCommandError> {
        let timeout_ms = timeout_ms.max(1);
        let timeout = Duration::from_millis(timeout_ms);
        let mut child = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    SwarmBriefCommandError::Unavailable(format!("{program} was not found on PATH."))
                } else {
                    SwarmBriefCommandError::Unavailable(error.to_string())
                }
            })?;

        let mut stdout_handle = child.stdout.take().ok_or_else(|| {
            SwarmBriefCommandError::Unavailable("Failed to capture stdout pipe".to_string())
        })?;
        let mut stderr_handle = child.stderr.take().ok_or_else(|| {
            SwarmBriefCommandError::Unavailable("Failed to capture stderr pipe".to_string())
        })?;

        let stdout_thread = thread::spawn(move || {
            use std::io::Read;
            let mut buf = Vec::new();
            let _ = (&mut stdout_handle)
                .take(10 * 1024 * 1024)
                .read_to_end(&mut buf);
            buf
        });

        let stderr_thread = thread::spawn(move || {
            use std::io::Read;
            let mut buf = Vec::new();
            let _ = (&mut stderr_handle)
                .take(10 * 1024 * 1024)
                .read_to_end(&mut buf);
            buf
        });

        let started_at = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    let elapsed = started_at.elapsed();
                    if elapsed >= timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        // Reap drain threads even on timeout to prevent resource leak
                        // (detached threads accumulate under repeated timeouts from flaky
                        // external tools like br/bv/cass in swarm scenarios).
                        let _ = stdout_thread.join();
                        let _ = stderr_thread.join();
                        return Err(SwarmBriefCommandError::TimedOut { timeout_ms });
                    }
                    thread::sleep(Duration::from_millis(10).min(timeout.saturating_sub(elapsed)));
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    // Reap drain threads to prevent leak on I/O errors.
                    let _ = stdout_thread.join();
                    let _ = stderr_thread.join();
                    return Err(SwarmBriefCommandError::Unavailable(error.to_string()));
                }
            }
        };

        let stdout_bytes = stdout_thread.join().unwrap_or_default();
        let stderr_bytes = stderr_thread.join().unwrap_or_default();

        let stdout = String::from_utf8(stdout_bytes)
            .unwrap_or_else(|_| String::from_utf8_lossy(b"Invalid UTF-8 stdout").into_owned());
        let stderr = String::from_utf8(stderr_bytes)
            .unwrap_or_else(|_| String::from_utf8_lossy(b"Invalid UTF-8 stderr").into_owned());

        if status.success() {
            Ok(SwarmBriefCommandOutput { stdout, stderr })
        } else {
            Err(SwarmBriefCommandError::Failed {
                status: status.code(),
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
    Rch {
        resource_pressure: Vec<SwarmBriefResourcePressureHint>,
        local_capability: Option<RchLocalCapabilityReport>,
    },
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
        let mut freshness = SwarmBriefSourceFreshness::current();
        let mut degraded = collect_beads_freshness(self.runner, options, &mut freshness);
        let mut bucket_degraded = Vec::new();

        let ready = collect_beads_bucket(
            self.runner,
            options,
            &["ready", "--json"],
            "ready",
            &mut bucket_degraded,
        );
        let blocked = collect_beads_bucket(
            self.runner,
            options,
            &["blocked", "--json"],
            "blocked",
            &mut bucket_degraded,
        );
        let in_progress = collect_beads_bucket(
            self.runner,
            options,
            &["list", "--status", "in_progress", "--json"],
            "in_progress",
            &mut bucket_degraded,
        );
        let deferred = collect_beads_bucket(
            self.runner,
            options,
            &["list", "--status", "deferred", "--json"],
            "deferred",
            &mut bucket_degraded,
        );
        let dependency_cycle_summary =
            collect_beads_dependency_cycles(self.runner, options, &mut degraded);

        if ready.is_empty()
            && blocked.is_empty()
            && in_progress.is_empty()
            && deferred.is_empty()
            && !bucket_degraded.is_empty()
        {
            let primary_degradation = bucket_degraded.remove(0);
            let mut unavailable_degraded = vec![primary_degradation.clone()];
            unavailable_degraded.extend(degraded);
            unavailable_degraded.extend(bucket_degraded);
            return SwarmBriefSourceOutput {
                snapshot: SwarmBriefSourceSnapshot::unavailable(
                    source,
                    provenance,
                    primary_degradation,
                )
                .with_freshness(freshness)
                .with_degraded(unavailable_degraded),
                contribution: SwarmBriefContribution::None,
            };
        }
        degraded.extend(bucket_degraded);

        let summary = SwarmBriefBeadsSummary {
            ready,
            blocked,
            in_progress,
            deferred,
            dependency_cycle_summary,
        };
        let item_count = summary.ready.len()
            + summary.blocked.len()
            + summary.in_progress.len()
            + summary.deferred.len()
            + summary
                .dependency_cycle_summary
                .as_ref()
                .map_or(0, |cycles| cycles.count as usize);
        SwarmBriefSourceOutput {
            snapshot: SwarmBriefSourceSnapshot::ready(source, provenance, item_count)
                .with_freshness(freshness)
                .with_degraded(degraded),
            contribution: SwarmBriefContribution::Beads(summary),
        }
    }
}

fn collect_beads_dependency_cycles<R: SwarmBriefCommandRunner>(
    runner: &R,
    options: &SwarmBriefCollectOptions,
    degraded: &mut Vec<SwarmBriefDegradation>,
) -> Option<SwarmBriefBeadsDependencyCycleSummary> {
    let args = ["dep", "cycles", "--json"];
    match runner.run("br", &args, &options.workspace, options.command_timeout_ms) {
        Ok(output) => match parse_beads_dependency_cycles_json(&output.stdout) {
            Ok(summary) => Some(summary),
            Err(message) => {
                degraded.push(SwarmBriefDegradation::warning(
                    SwarmBriefSourceKind::Beads,
                    BEADS_UNAVAILABLE_CODE,
                    message,
                    Some("br dep cycles --json".to_string()),
                ));
                None
            }
        },
        Err(error) => {
            degraded.push(error.to_degradation(
                SwarmBriefSourceKind::Beads,
                BEADS_UNAVAILABLE_CODE,
                "br dep cycles --json",
            ));
            None
        }
    }
}

fn collect_beads_freshness<R: SwarmBriefCommandRunner>(
    runner: &R,
    options: &SwarmBriefCollectOptions,
    freshness: &mut SwarmBriefSourceFreshness,
) -> Vec<SwarmBriefDegradation> {
    let args = [
        "sync",
        "--status",
        "--json",
        "--no-auto-import",
        "--allow-stale",
    ];
    match runner.run("br", &args, &options.workspace, options.command_timeout_ms) {
        Ok(output) => match parse_beads_sync_status_json(&output.stdout) {
            Ok(status) if status.jsonl_newer || status.db_newer => {
                *freshness = SwarmBriefSourceFreshness {
                    observed_at: status.last_import_time.clone(),
                    age_seconds: None,
                    stale_after_seconds: None,
                    state: "stale",
                };
                let (message, repair) = beads_tracker_stale_message_and_repair(&status);
                vec![SwarmBriefDegradation::warning(
                    SwarmBriefSourceKind::Beads,
                    BEADS_TRACKER_STALE_CODE,
                    message,
                    Some(repair.to_string()),
                )]
            }
            Ok(_) => Vec::new(),
            Err(message) => vec![SwarmBriefDegradation::warning(
                SwarmBriefSourceKind::Beads,
                BEADS_UNAVAILABLE_CODE,
                message,
                Some("br sync --status --json --no-auto-import --allow-stale".to_string()),
            )],
        },
        Err(error) => vec![error.to_degradation(
            SwarmBriefSourceKind::Beads,
            BEADS_UNAVAILABLE_CODE,
            "br sync --status --json --no-auto-import --allow-stale",
        )],
    }
}

fn beads_tracker_stale_message_and_repair(
    status: &BeadsSyncStatus,
) -> (&'static str, &'static str) {
    if status.jsonl_newer && status.db_newer {
        (
            "Beads database and JSONL both report unmerged changes; tracker freshness is ambiguous.",
            "br sync --status --json --no-auto-import --allow-stale",
        )
    } else if status.db_newer {
        (
            "Beads database is newer than JSONL; exported tracker files may lag coordination history.",
            "br sync --flush-only",
        )
    } else {
        (
            "Beads JSONL is newer than the local database; bucket reads may lag coordination history.",
            "br sync --import-only",
        )
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
                    let degraded = snapshot.degraded.clone();
                    SwarmBriefSourceOutput {
                        snapshot: SwarmBriefSourceSnapshot::ready(
                            SwarmBriefSourceKind::AgentMail,
                            provenance,
                            item_count,
                        )
                        .with_degraded(degraded),
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
        if !swarm_brief_source_enabled(options, SwarmBriefSourceKind::Rch) {
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

        let status = self
            .runner
            .run("rch", &args, &options.workspace, options.command_timeout_ms);
        let capability = collect_rch_local_capability_snapshot(
            self.runner,
            options,
            status.as_ref().ok().map(|output| output.stdout.as_str()),
        );

        match status {
            Ok(output) => match parse_rch_status_json(&output.stdout) {
                Ok(hints) => {
                    let item_count = hints.len();
                    SwarmBriefSourceOutput {
                        snapshot: SwarmBriefSourceSnapshot::ready(
                            SwarmBriefSourceKind::Rch,
                            provenance,
                            item_count,
                        ),
                        contribution: SwarmBriefContribution::Rch {
                            resource_pressure: hints,
                            local_capability: capability,
                        },
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
                        contribution: SwarmBriefContribution::Rch {
                            resource_pressure: Vec::new(),
                            local_capability: capability,
                        },
                    }
                }
            },
            Err(error) => {
                let degradation = rch_command_error_to_degradation(&error);
                SwarmBriefSourceOutput {
                    snapshot: SwarmBriefSourceSnapshot::unavailable(
                        SwarmBriefSourceKind::Rch,
                        provenance,
                        degradation,
                    ),
                    contribution: SwarmBriefContribution::Rch {
                        resource_pressure: Vec::new(),
                        local_capability: capability,
                    },
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
    fn collect(&self, options: &SwarmBriefCollectOptions) -> SwarmBriefSourceOutput {
        let provenance = SwarmBriefSourceProvenance::local_probe();
        match gather_agent_status(&AgentStatusOptions {
            only_connectors: options.agent_inventory_only_connectors.clone(),
            ..AgentStatusOptions::default()
        }) {
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
    collect_selected_source(
        &mut report,
        options,
        SwarmBriefSourceKind::Git,
        SwarmBriefSourceProvenance::command("git", &["status", "--short"]),
        || GitSourceAdapter { runner }.collect(options),
    );
    collect_selected_source(
        &mut report,
        options,
        SwarmBriefSourceKind::Beads,
        SwarmBriefSourceProvenance::command("br", &["ready", "--json"]),
        || BeadsSourceAdapter { runner }.collect(options),
    );
    collect_selected_source(
        &mut report,
        options,
        SwarmBriefSourceKind::Bv,
        SwarmBriefSourceProvenance::command("bv", &["--robot-triage", "--robot-triage-by-track"]),
        || BvSourceAdapter { runner }.collect(options),
    );
    collect_selected_source(
        &mut report,
        options,
        SwarmBriefSourceKind::AgentMail,
        SwarmBriefSourceProvenance::local_probe(),
        || AgentMailSnapshotFileAdapter.collect(options),
    );
    collect_selected_source(
        &mut report,
        options,
        SwarmBriefSourceKind::Rch,
        SwarmBriefSourceProvenance::command("rch", &["status", "--json"]),
        || RchSourceAdapter { runner }.collect(options),
    );
    collect_selected_source(
        &mut report,
        options,
        SwarmBriefSourceKind::HostProfile,
        SwarmBriefSourceProvenance::local_probe(),
        || HostProfileSourceAdapter.collect(options),
    );
    collect_selected_source(
        &mut report,
        options,
        SwarmBriefSourceKind::AgentInventory,
        SwarmBriefSourceProvenance::local_probe(),
        || AgentInventorySourceAdapter.collect(options),
    );
    attach_qos_resource_pressure(&mut report, &options.workspace);
    apply_swarm_brief_advice(&mut report);
    report.finalize();
    report
}

fn collect_selected_source<F>(
    report: &mut SwarmBriefReport,
    options: &SwarmBriefCollectOptions,
    source: SwarmBriefSourceKind,
    provenance: SwarmBriefSourceProvenance,
    collect: F,
) where
    F: FnOnce() -> SwarmBriefSourceOutput,
{
    if swarm_brief_source_enabled(options, source) {
        apply_source_output(report, collect());
    } else {
        apply_source_output(report, skipped_source_output(source, provenance));
    }
}

fn swarm_brief_source_enabled(
    options: &SwarmBriefCollectOptions,
    source: SwarmBriefSourceKind,
) -> bool {
    options.enabled_sources.contains(&source)
        || (source == SwarmBriefSourceKind::Rch && options.include_rch)
}

fn skipped_source_output(
    source: SwarmBriefSourceKind,
    provenance: SwarmBriefSourceProvenance,
) -> SwarmBriefSourceOutput {
    SwarmBriefSourceOutput {
        snapshot: SwarmBriefSourceSnapshot {
            source,
            status: SwarmBriefSourceStatus::Skipped,
            freshness: SwarmBriefSourceFreshness::unknown(),
            provenance,
            item_count: 0,
            degraded: Vec::new(),
        },
        contribution: SwarmBriefContribution::None,
    }
}

fn attach_qos_resource_pressure(report: &mut SwarmBriefReport, workspace: &Path) {
    let workspace_identity = workspace.to_string_lossy();
    let now_epoch_ms = current_epoch_ms();
    let summary =
        super::qos::summarize_qos_lane_registry(workspace, &workspace_identity, now_epoch_ms);
    report
        .resource_pressure
        .extend(qos_resource_pressure_hints(&summary));
    report
        .degraded
        .extend(summary.degraded.iter().map(|degradation| {
            SwarmBriefDegradation::warning(
                SwarmBriefSourceKind::Qos,
                degradation.code.clone(),
                degradation.message.clone(),
                Some(degradation.repair.clone()),
            )
        }));
}

#[cfg(test)]
fn attach_qos_summary_for_test(
    report: &mut SwarmBriefReport,
    summary: &super::qos::QosLaneSummary,
) {
    report
        .resource_pressure
        .extend(qos_resource_pressure_hints(summary));
    report
        .degraded
        .extend(summary.degraded.iter().map(|degradation| {
            SwarmBriefDegradation::warning(
                SwarmBriefSourceKind::Qos,
                degradation.code.clone(),
                degradation.message.clone(),
                Some(degradation.repair.clone()),
            )
        }));
}

fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or_default()
}

fn qos_resource_pressure_hints(
    summary: &super::qos::QosLaneSummary,
) -> Vec<SwarmBriefResourcePressureHint> {
    let mut hints = Vec::new();
    if summary.foreground_active_count > 0 {
        hints.push(SwarmBriefResourcePressureHint {
            source: SwarmBriefSourceKind::Qos,
            level: "high".to_string(),
            message: format!(
                "qos foreground pressure active: {} foreground lane(s)",
                summary.foreground_active_count
            ),
        });
    }
    if summary.background_active_count > 0 {
        hints.push(SwarmBriefResourcePressureHint {
            source: SwarmBriefSourceKind::Qos,
            level: "medium".to_string(),
            message: format!(
                "qos background derived work active: {} lane(s)",
                summary.background_active_count
            ),
        });
    }
    if summary.maintenance_active_count > 0 {
        hints.push(SwarmBriefResourcePressureHint {
            source: SwarmBriefSourceKind::Qos,
            level: "medium".to_string(),
            message: format!(
                "qos maintenance work active: {} lane(s)",
                summary.maintenance_active_count
            ),
        });
    }
    if summary.verification_active_count > 0 {
        hints.push(SwarmBriefResourcePressureHint {
            source: SwarmBriefSourceKind::Qos,
            level: "low".to_string(),
            message: format!(
                "qos remote verification active: {} lane(s)",
                summary.verification_active_count
            ),
        });
    }
    if summary.stale_ignored_count > 0 {
        hints.push(SwarmBriefResourcePressureHint {
            source: SwarmBriefSourceKind::Qos,
            level: "low".to_string(),
            message: format!(
                "qos ignored stale lane record(s): {}",
                summary.stale_ignored_count
            ),
        });
    }
    hints.sort();
    hints.dedup();
    hints
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
            report.beads.dependency_cycle_summary = summary.dependency_cycle_summary;
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
        SwarmBriefContribution::Rch {
            resource_pressure,
            local_capability,
        } => {
            report.resource_pressure.extend(resource_pressure);
            if let Some(capability) = local_capability {
                attach_rch_local_capability(report, capability);
            }
        }
        SwarmBriefContribution::HostProfile(summary) => {
            report.host_profile = Some(summary);
        }
        SwarmBriefContribution::AgentInventory(summary) => {
            report.agent_inventory = Some(summary);
        }
    }
}

pub fn attach_rch_local_capability(
    report: &mut SwarmBriefReport,
    capability: RchLocalCapabilityReport,
) {
    let degraded = capability.degraded.clone();
    let status = if capability.remote_only_safe {
        SwarmBriefSourceStatus::Ready
    } else {
        SwarmBriefSourceStatus::Degraded
    };

    match report
        .sources
        .iter_mut()
        .find(|snapshot| snapshot.source == SwarmBriefSourceKind::Rch)
    {
        Some(snapshot) => {
            snapshot.item_count = snapshot.item_count.saturating_add(1);
            snapshot.degraded.extend(degraded.clone());
            snapshot.degraded.sort();
            snapshot.degraded.dedup();
            if !capability.remote_only_safe && snapshot.status == SwarmBriefSourceStatus::Ready {
                snapshot.status = SwarmBriefSourceStatus::Degraded;
            }
        }
        None => {
            report.sources.push(SwarmBriefSourceSnapshot {
                source: SwarmBriefSourceKind::Rch,
                status,
                freshness: SwarmBriefSourceFreshness::current(),
                provenance: SwarmBriefSourceProvenance::local_probe(),
                item_count: 1,
                degraded: degraded.clone(),
            });
        }
    }
    report.degraded.extend(degraded);
    report.degraded.sort();
    report.degraded.dedup();
    report.rch_local_capability = Some(capability);
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

/// Collect a compact redaction-safe summary suitable for support bundles and handoff capsules.
#[must_use]
pub fn collect_swarm_brief_summary(workspace: &Path) -> Value {
    let options = SwarmBriefCollectOptions::for_workspace(workspace);
    let runner = SystemSwarmBriefCommandRunner;
    let report = collect_swarm_brief(&options, &runner);
    summarize_swarm_brief_report(&report)
}

/// Summarize a full brief without exposing raw mail bodies, query text, provenance text, or file lists.
#[must_use]
pub fn summarize_swarm_brief_report(report: &SwarmBriefReport) -> Value {
    let redacted_report = serde_json::to_value(report)
        .map(|value| redact_summary_value(&value))
        .unwrap_or(Value::Null);
    let redacted_report_json = stable_summary_json(&redacted_report);
    let report_hash = blake3_summary_hash(&redacted_report_json);
    let degraded_codes = swarm_brief_degraded_codes(report);
    let source_status_counts = swarm_brief_source_status_counts(report);
    let active_conflict_count = report
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
        .count();

    json!({
        "schema": SWARM_BRIEF_SUMMARY_SCHEMA_V1,
        "sourceSchema": SWARM_BRIEF_SCHEMA_V1,
        "source": "read_only_swarm_brief_report",
        "status": "available",
        "redactionStatus": SWARM_BRIEF_SUMMARY_REDACTION_STATUS,
        "reportHash": report_hash,
        "workspaceHash": blake3_summary_hash(&report.workspace),
        "limits": {
            "maxRecommendations": MAX_SWARM_BRIEF_SUMMARY_RECOMMENDATIONS,
        },
        "counts": {
            "sourceCount": report.sources.len(),
            "dirtyFileCount": report.dirty_files.len(),
            "recentCommitCount": report.recent_commits.len(),
            "readyWorkCount": report.beads.ready.len(),
            "blockedWorkCount": report.beads.blocked.len(),
            "inProgressWorkCount": report.beads.in_progress.len(),
            "deferredWorkCount": report.beads.deferred.len(),
            "activeReservationCount": report.file_reservations.len(),
            "exclusiveReservationCount": report.file_reservations.iter().filter(|reservation| reservation.exclusive).count(),
            "activeConflictCount": active_conflict_count,
            "fileSurfaceRiskCount": report.file_surface_risks.len(),
            "inboxMailboxCount": report.inbox.len(),
            "unreadCount": report.inbox.iter().map(|item| item.unread_count).sum::<u64>(),
            "ackRequiredCount": report.inbox.iter().map(|item| item.ack_required_count).sum::<u64>(),
            "threadCount": report.threads.len(),
            "resourcePressureHintCount": report.resource_pressure.len(),
            "degradedCount": report.degraded.len(),
            "recommendationCount": report.recommendations.len(),
        },
        "bv": {
            "actionableCount": report.bv.as_ref().and_then(|summary| summary.actionable_count),
            "blockedCount": report.bv.as_ref().and_then(|summary| summary.blocked_count),
            "inProgressCount": report.bv.as_ref().and_then(|summary| summary.in_progress_count),
            "trackCount": report.bv.as_ref().and_then(|summary| summary.track_count),
            "topPickIds": report.bv.as_ref().map(|summary| {
                summary.top_picks.iter().take(5).map(|pick| pick.id.clone()).collect::<Vec<_>>()
            }).unwrap_or_default(),
        },
        "sourceStatusCounts": source_status_counts,
        "sourceStatuses": swarm_brief_source_status_summaries(report),
        "resourcePressurePosture": swarm_brief_resource_pressure_posture(report),
        "singleFlight": singleflight_posture_report(),
        "degradedCodes": degraded_codes,
        "fileSurfaceRiskSummary": swarm_brief_file_surface_risk_summary(report),
        "topRecommendations": swarm_brief_summary_recommendations(report),
        "provenance": {
            "underlyingReportHash": report_hash,
            "sideEffectFree": true,
            "rawCommandTextIncluded": false,
            "sourceProvenance": swarm_brief_source_provenance_summaries(report),
        },
        "redaction": {
            "rawMailBodiesIncluded": false,
            "rawQueryTextIncluded": false,
            "rawProvenanceTextIncluded": false,
            "fullFileListingsIncluded": false,
            "recommendationEvidenceIncluded": "hashes_only",
        },
    })
}

/// Render the compact posture as section text for handoff capsules.
#[must_use]
pub fn render_swarm_brief_summary_for_handoff(summary: &Value) -> String {
    let counts = summary.get("counts").unwrap_or(&Value::Null);
    let ready = counts
        .get("readyWorkCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let blocked = counts
        .get("blockedWorkCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let in_progress = counts
        .get("inProgressWorkCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let conflicts = counts
        .get("activeConflictCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let degraded = counts
        .get("degradedCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let pressure = summary
        .get("resourcePressurePosture")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let singleflight = summary.get("singleFlight").unwrap_or(&Value::Null);
    let singleflight_status = singleflight
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let singleflight_active = singleflight
        .get("activeLeaderCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let singleflight_waits = singleflight
        .get("followerWaitCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let singleflight_timeouts = singleflight
        .get("followerTimeoutCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let singleflight_failures = singleflight
        .get("leaderFailureCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let singleflight_reused = singleflight
        .get("reusedResultCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let hash = summary
        .get("reportHash")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let top_recommendations = summary
        .get("topRecommendations")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("id").and_then(Value::as_str))
                .take(3)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut lines = vec![
        format!(
            "Swarm brief summary: ready={ready}, blocked={blocked}, in_progress={in_progress}, active_conflicts={conflicts}, resource_pressure={pressure}, degraded_sources={degraded}."
        ),
        format!(
            "Single-flight posture: status={singleflight_status}, active_leaders={singleflight_active}, follower_waits={singleflight_waits}, follower_timeouts={singleflight_timeouts}, leader_failures={singleflight_failures}, reused_results={singleflight_reused}."
        ),
        format!("Source report hash: {hash}."),
        "Diagnostic posture only; run a fresh live brief before claiming or coordinating work."
            .to_owned(),
    ];
    if !top_recommendations.is_empty() {
        lines.push(format!(
            "Top recommendation ids: {}.",
            top_recommendations.join(", ")
        ));
    }
    lines.join("\n")
}

#[must_use]
pub fn swarm_brief_summary_evidence_id(summary: &Value) -> String {
    let hash = summary
        .get("reportHash")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .trim_start_matches("blake3:");
    let short_hash = hash.get(..12).unwrap_or(hash);
    format!("swarm_brief_summary:{short_hash}")
}

fn swarm_brief_degraded_codes(report: &SwarmBriefReport) -> Vec<String> {
    let mut codes = report
        .degraded
        .iter()
        .map(|degradation| degradation.code.clone())
        .collect::<Vec<_>>();
    codes.sort();
    codes.dedup();
    codes
}

fn swarm_brief_source_status_counts(report: &SwarmBriefReport) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for source in &report.sources {
        *counts.entry(source.status.as_str().to_owned()).or_insert(0) += 1;
    }
    counts
}

fn swarm_brief_source_status_summaries(report: &SwarmBriefReport) -> Vec<Value> {
    report
        .sources
        .iter()
        .map(|source| {
            let mut degraded_codes = source
                .degraded
                .iter()
                .map(|degradation| degradation.code.clone())
                .collect::<Vec<_>>();
            degraded_codes.sort();
            degraded_codes.dedup();
            json!({
                "source": source.source.as_str(),
                "status": source.status.as_str(),
                "itemCount": source.item_count,
                "degradedCodes": degraded_codes,
                "sideEffectFree": source.provenance.side_effect_free,
                "provenanceHash": blake3_summary_hash(
                    &stable_summary_json(
                        &serde_json::to_value(&source.provenance).unwrap_or(Value::Null)
                    )
                ),
                "rawProvenanceIncluded": false,
            })
        })
        .collect()
}

fn swarm_brief_source_provenance_summaries(report: &SwarmBriefReport) -> Vec<Value> {
    report
        .sources
        .iter()
        .map(|source| {
            json!({
                "source": source.source.as_str(),
                "status": source.status.as_str(),
                "itemCount": source.item_count,
                "provenanceHash": blake3_summary_hash(
                    &stable_summary_json(
                        &serde_json::to_value(&source.provenance).unwrap_or(Value::Null)
                    )
                ),
                "commandIncluded": false,
                "sideEffectFree": source.provenance.side_effect_free,
            })
        })
        .collect()
}

fn swarm_brief_resource_pressure_posture(report: &SwarmBriefReport) -> &'static str {
    if report
        .resource_pressure
        .iter()
        .any(|hint| hint.level == "high")
    {
        return "high";
    }
    if report
        .resource_pressure
        .iter()
        .any(|hint| hint.level == "medium")
    {
        return "medium";
    }
    if report.resource_pressure.is_empty() && report.host_profile.is_none() {
        return "unknown";
    }
    "low"
}

fn swarm_brief_file_surface_risk_summary(report: &SwarmBriefReport) -> Value {
    let mut counts_by_severity = BTreeMap::<String, usize>::new();
    let mut counts_by_holder = BTreeMap::<String, usize>::new();
    let mut counts_by_git_status = BTreeMap::<String, usize>::new();
    for risk in &report.file_surface_risks {
        *counts_by_severity.entry(risk.severity.clone()).or_default() += 1;
        for holder in &risk.reservation_holders {
            *counts_by_holder.entry(holder.clone()).or_default() += 1;
        }
        for status in &risk.git_status_buckets {
            *counts_by_git_status.entry(status.clone()).or_default() += 1;
        }
    }

    let mut top_risks = report.file_surface_risks.iter().collect::<Vec<_>>();
    top_risks.sort_by(|left, right| {
        recommendation_severity_rank(&right.severity)
            .cmp(&recommendation_severity_rank(&left.severity))
            .then_with(|| right.score.cmp(&left.score))
            .then_with(|| left.path_pattern.cmp(&right.path_pattern))
    });

    json!({
        "countsBySeverity": counts_by_severity,
        "countsByReservationHolder": counts_by_holder,
        "countsByGitStatus": counts_by_git_status,
        "topRisks": top_risks
            .into_iter()
            .take(MAX_SWARM_BRIEF_SUMMARY_RECOMMENDATIONS)
            .map(|risk| {
                json!({
                    "pathHash": blake3_summary_hash(&risk.path_pattern),
                    "severity": risk.severity.clone(),
                    "score": risk.score,
                    "riskFactors": risk.risk_factors.clone(),
                    "reservationHolders": risk.reservation_holders.clone(),
                    "relatedBeadIds": risk.related_bead_ids.clone(),
                    "suggestedCommandHashes": risk.suggested_commands.iter().map(|value| blake3_summary_hash(value)).collect::<Vec<_>>(),
                    "rawPathIncluded": false,
                    "rawCommandsIncluded": false,
                })
            })
            .collect::<Vec<_>>(),
    })
}

fn swarm_brief_summary_recommendations(report: &SwarmBriefReport) -> Vec<Value> {
    let mut recommendations = report.recommendations.iter().collect::<Vec<_>>();
    recommendations.sort_by(|left, right| {
        recommendation_severity_rank(&right.severity)
            .cmp(&recommendation_severity_rank(&left.severity))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.id.cmp(&right.id))
    });
    recommendations
        .into_iter()
        .take(MAX_SWARM_BRIEF_SUMMARY_RECOMMENDATIONS)
        .map(|recommendation| {
            json!({
                "id": recommendation.id,
                "kind": recommendation.kind,
                "confidence": recommendation.confidence,
                "severity": recommendation.severity,
                "reasonCodes": recommendation.reason_codes,
                "evidenceHashes": recommendation.evidence.iter().map(|value| blake3_summary_hash(value)).collect::<Vec<_>>(),
                "suggestedCommandHashes": recommendation.suggested_commands.iter().map(|value| blake3_summary_hash(value)).collect::<Vec<_>>(),
                "mustNotDoHashes": recommendation.must_not_do.iter().map(|value| blake3_summary_hash(value)).collect::<Vec<_>>(),
                "rawEvidenceIncluded": false,
                "rawCommandsIncluded": false,
            })
        })
        .collect()
}

fn recommendation_severity_rank(severity: &str) -> u8 {
    match severity {
        "critical" => 4,
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

fn redact_summary_value(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(redact_brief_text(text)),
        Value::Array(items) => Value::Array(items.iter().map(redact_summary_value).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), redact_summary_value(value)))
                .collect(),
        ),
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
    }
}

fn stable_summary_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|error| {
        json!({
            "schema": "ee.swarm.brief_summary.serialization_error.v1",
            "message": error.to_string(),
        })
        .to_string()
    })
}

fn blake3_summary_hash(value: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(value.as_bytes());
    format!("blake3:{}", hasher.finalize().to_hex())
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
    git_status_bucket: Option<String>,
    reservation_holder: Option<String>,
    related_bead_id: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SurfaceRiskBuilder {
    score: u16,
    git_status_buckets: BTreeSet<String>,
    reservation_holders: BTreeSet<String>,
    related_bead_ids: BTreeSet<String>,
    risk_factors: BTreeSet<String>,
    evidence: BTreeSet<String>,
}

impl SurfaceRiskBuilder {
    fn add(&mut self, factor: impl Into<String>, evidence: impl Into<String>, score: u16) {
        self.score = self.score.saturating_add(score).min(100);
        self.risk_factors.insert(redact_brief_text(&factor.into()));
        self.evidence.insert(redact_brief_text(&evidence.into()));
    }

    fn add_observation(&mut self, observation: &SurfaceObservation) {
        if let Some(status) = &observation.git_status_bucket {
            self.git_status_buckets.insert(redact_brief_text(status));
        }
        if let Some(holder) = &observation.reservation_holder {
            self.reservation_holders.insert(redact_brief_text(holder));
        }
        if let Some(bead_id) = &observation.related_bead_id {
            self.related_bead_ids.insert(redact_brief_text(bead_id));
        }
    }

    fn build(self, path_pattern: String) -> SwarmBriefFileSurfaceRisk {
        let git_status_buckets = self.git_status_buckets.into_iter().collect::<Vec<_>>();
        let reservation_holders = self.reservation_holders.into_iter().collect::<Vec<_>>();
        let related_bead_ids = self.related_bead_ids.into_iter().collect::<Vec<_>>();
        let suggested_commands = suggested_file_surface_commands(
            &path_pattern,
            &git_status_buckets,
            &reservation_holders,
            &related_bead_ids,
        );
        SwarmBriefFileSurfaceRisk {
            path_pattern,
            git_status_buckets,
            reservation_holders,
            related_bead_ids,
            severity: severity_for_score(self.score).to_string(),
            score: self.score,
            risk_factors: self.risk_factors.into_iter().collect(),
            evidence: self.evidence.into_iter().collect(),
            suggested_commands,
        }
    }
}

fn score_file_surface_risks(report: &SwarmBriefReport) -> Vec<SwarmBriefFileSurfaceRisk> {
    let observations = collect_surface_observations(report);
    let mut risks = BTreeMap::<String, SurfaceRiskBuilder>::new();

    for observation in &observations {
        let risk = risks.entry(observation.pattern.clone()).or_default();
        risk.add(
            observation.factor.clone(),
            observation.evidence.clone(),
            observation.score,
        );
        risk.add_observation(observation);
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
                let risk = risks.entry(pattern).or_default();
                risk.add(factor, evidence, score);
                risk.add_observation(left);
                risk.add_observation(right);
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
            git_status_bucket: Some(file.status.clone()),
            reservation_holder: None,
            related_bead_id: None,
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
            git_status_bucket: None,
            reservation_holder: Some(reservation.holder.clone()),
            related_bead_id: None,
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
                git_status_bucket: None,
                reservation_holder: None,
                related_bead_id: Some(bead.id.clone()),
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
                git_status_bucket: None,
                reservation_holder: None,
                related_bead_id: None,
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

fn suggested_file_surface_commands(
    path_pattern: &str,
    git_status_buckets: &[String],
    reservation_holders: &[String],
    related_bead_ids: &[String],
) -> Vec<String> {
    let mut commands = BTreeSet::new();
    if !git_status_buckets.is_empty() {
        commands.insert(format!("git status --short -- {path_pattern}"));
    }
    if !reservation_holders.is_empty() {
        commands.insert(format!(
            "message {} before editing {path_pattern}",
            reservation_holders.join(",")
        ));
    }
    for bead_id in related_bead_ids.iter().take(3) {
        commands.insert(format!("br show {bead_id} --json"));
    }
    if reservation_holders.is_empty() && related_bead_ids.is_empty() {
        commands.insert("search Agent Mail and Beads before editing this surface".to_string());
    }
    commands.into_iter().collect()
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

    if matches!(
        source_status(report, SwarmBriefSourceKind::Beads),
        Some(SwarmBriefSourceStatus::Ready | SwarmBriefSourceStatus::Degraded)
    ) {
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
            let must_not_do = degraded_recommendation_must_not_do(source, &degradation.code);
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
                must_not_do,
            });
        }
    }
    recommendations
}

fn degraded_recommendation_must_not_do(source: SwarmBriefSourceKind, code: &str) -> Vec<String> {
    let mut must_not_do = vec![format!(
        "Do not treat degraded {} data as complete evidence.",
        source.as_str()
    )];
    if source == SwarmBriefSourceKind::Rch && code == RCH_WORKER_TOPOLOGY_BLOCKED_CODE {
        must_not_do.push(
            "Do not close beads requiring remote Cargo evidence from a topology-blocked RCH attempt; obtain an alternate remote pass or record the blocked posture."
                .to_string(),
        );
    } else if source == SwarmBriefSourceKind::Rch
        && code == RCH_REMOTE_REQUIRED_FALLBACK_PREVENTED_CODE
    {
        must_not_do.push(
            "Do not unset RCH_REQUIRE_REMOTE or count local Cargo output without explicit user approval."
                .to_string(),
        );
    }
    must_not_do
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
            "RCH_VISIBILITY=summary RCH_QUEUE_WHEN_BUSY=1 rch exec -- env CARGO_TARGET_DIR=\"${CARGO_TARGET_DIR:-/Volumes/USBNVME16TB/temp_agent_space/cargo-target}\" cargo check --all-targets".to_string(),
            "RCH_VISIBILITY=summary RCH_QUEUE_WHEN_BUSY=1 rch exec -- env CARGO_TARGET_DIR=\"${CARGO_TARGET_DIR:-/Volumes/USBNVME16TB/temp_agent_space/cargo-target}\" cargo clippy --all-targets -- -D warnings".to_string(),
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
        SwarmBriefSourceKind::Qos => "ee status --json | jq .data.qos",
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
        SwarmBriefSourceKind::Qos => "foreground/background active-lane pressure",
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct BeadsSyncStatus {
    jsonl_newer: bool,
    db_newer: bool,
    last_import_time: Option<String>,
}

fn parse_beads_sync_status_json(input: &str) -> Result<BeadsSyncStatus, String> {
    let value = serde_json::from_str::<Value>(input)
        .map_err(|error| format!("Beads sync status JSON could not be parsed: {error}"))?;
    Ok(BeadsSyncStatus {
        jsonl_newer: value
            .get("jsonl_newer")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        db_newer: value
            .get("db_newer")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        last_import_time: string_field(&value, &["last_import_time", "lastImportTime"]),
    })
}

fn parse_beads_dependency_cycles_json(
    input: &str,
) -> Result<SwarmBriefBeadsDependencyCycleSummary, String> {
    let value = serde_json::from_str::<Value>(input)
        .map_err(|error| format!("Beads dependency cycles JSON could not be parsed: {error}"))?;
    let mut examples = value
        .get("cycles")
        .and_then(Value::as_array)
        .map(|cycles| {
            cycles
                .iter()
                .filter_map(|cycle| {
                    let mut ids = cycle
                        .as_array()?
                        .iter()
                        .filter_map(Value::as_str)
                        .map(redact_brief_text)
                        .collect::<Vec<_>>();
                    ids.retain(|id| !id.is_empty());
                    (!ids.is_empty()).then_some(ids)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    examples.sort();
    examples.dedup();
    let count = value
        .get("count")
        .and_then(Value::as_u64)
        .unwrap_or(examples.len() as u64);
    examples.truncate(3);
    Ok(SwarmBriefBeadsDependencyCycleSummary { count, examples })
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
    let degraded = parse_agent_mail_health_degraded(&value);
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
        degraded,
    })
}

fn parse_agent_mail_health_degraded(value: &Value) -> Vec<SwarmBriefDegradation> {
    let is_coordination_health = value
        .get("schema")
        .and_then(Value::as_str)
        .is_some_and(|schema| schema == "ee.swarm.coordination_health.v1")
        || value.get("fallback_active").is_some();
    if !is_coordination_health {
        return Vec::new();
    }

    let failed_checks = [
        ("mcp_http", "mcp_http_reachable"),
        ("am_agents_list", "am_agents_list_ok"),
        ("am_send_single_recipient", "am_send_single_recipient_ok"),
        ("am_send_multi_recipient", "am_send_multi_recipient_ok"),
    ]
    .into_iter()
    .filter_map(|(label, key)| {
        value
            .get(key)
            .and_then(Value::as_bool)
            .is_some_and(|ok| !ok)
            .then_some(label)
    })
    .collect::<Vec<_>>();
    let fallback_active = value
        .get("fallback_active")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !fallback_active && failed_checks.is_empty() {
        return Vec::new();
    }

    let panic = value.get("observed_panic").and_then(Value::as_str);
    let mut message = if failed_checks.is_empty() {
        "Agent Mail transport health reported fallback mode, so live reservations and unread mail may be incomplete.".to_string()
    } else {
        format!(
            "Agent Mail transport health is degraded; failed checks: {}.",
            failed_checks.join(", ")
        )
    };
    if let Some(panic) = panic.filter(|panic| !panic.is_empty()) {
        message.push_str(" Observed panic: ");
        message.push_str(panic);
        message.push('.');
    }

    vec![SwarmBriefDegradation::warning(
        SwarmBriefSourceKind::AgentMail,
        AGENT_MAIL_UNAVAILABLE_CODE,
        message,
        Some(
            "Run `am doctor repair` or provide a current redacted Agent Mail snapshot.".to_string(),
        ),
    )]
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
    let queue_depth = numeric_field_any(&value, &["queue_depth", "queueDepth", "queued"]);
    let active_builds = numeric_field_any(&value, &["active_builds", "activeBuilds", "running"]);
    let mut hints = Vec::new();
    if let Some(posture) = rch_remote_posture(&value) {
        let level = if posture == RCH_POSTURE_REMOTE_READY {
            "low"
        } else {
            "high"
        };
        hints.push(SwarmBriefResourcePressureHint {
            source: SwarmBriefSourceKind::Rch,
            level: level.to_string(),
            message: format!("rch remote posture: {posture}"),
        });
    }
    if let Some(worker) = rch_selected_worker(&value) {
        hints.push(SwarmBriefResourcePressureHint {
            source: SwarmBriefSourceKind::Rch,
            level: "low".to_string(),
            message: format!("rch selected worker: {}", redact_brief_text(&worker)),
        });
    }
    if let Some(topology_roots) = rch_topology_root_summary(&value) {
        hints.push(SwarmBriefResourcePressureHint {
            source: SwarmBriefSourceKind::Rch,
            level: "low".to_string(),
            message: format!("rch topology roots: {topology_roots}"),
        });
    }
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
            message:
                "rch status did not expose remote posture, topology, queue, or active build counts"
                    .to_string(),
        });
    }
    hints.sort();
    Ok(hints)
}

fn collect_rch_local_capability_snapshot<R: SwarmBriefCommandRunner>(
    runner: &R,
    options: &SwarmBriefCollectOptions,
    status_stdout: Option<&str>,
) -> Option<RchLocalCapabilityReport> {
    let help = run_rch_json_capture(runner, options, &["--help-json"]);
    let hook_status = run_rch_json_capture(
        runner,
        options,
        &["agents", "status", "codex-cli", "--json"],
    );
    let status = status_stdout
        .and_then(|stdout| serde_json::from_str::<Value>(stdout).ok())
        .or_else(|| run_rch_json_capture(runner, options, &["status", "--json"]));
    let queue = run_rch_json_capture(runner, options, &["queue", "--json"]);
    let config = run_rch_json_capture(runner, options, &["config", "show", "--json"]);
    let worker_probe =
        run_rch_json_capture(runner, options, &["workers", "probe", "--all", "--json"]);
    let diagnose = run_rch_json_capture(
        runner,
        options,
        &["diagnose", "--dry-run", "--json", "cargo", "check", "--lib"],
    );

    let snapshot = json!({
        "schema": "ee.rch.local_capability.capture.v1",
        "remoteOnlyRequired": true,
        "captures": {
            "helpJson": help.unwrap_or(Value::Null),
            "hookStatus": hook_status.unwrap_or(Value::Null),
            "status": status.unwrap_or(Value::Null),
            "queue": queue.unwrap_or(Value::Null),
            "config": config.unwrap_or(Value::Null),
            "workerProbe": worker_probe.unwrap_or(Value::Null),
            "diagnose": diagnose.unwrap_or(Value::Null),
        }
    });
    parse_rch_local_capability_snapshot(&snapshot.to_string()).ok()
}

fn run_rch_json_capture<R: SwarmBriefCommandRunner>(
    runner: &R,
    options: &SwarmBriefCollectOptions,
    args: &[&str],
) -> Option<Value> {
    runner
        .run("rch", args, &options.workspace, options.command_timeout_ms)
        .ok()
        .and_then(|output| serde_json::from_str::<Value>(&output.stdout).ok())
}

pub fn parse_rch_local_capability_snapshot(
    input: &str,
) -> Result<RchLocalCapabilityReport, String> {
    let value = serde_json::from_str::<Value>(input)
        .map_err(|error| format!("RCH local capability snapshot could not be parsed: {error}"))?;
    let captures = value.get("captures").unwrap_or(&value);
    let help = captures.get("helpJson").or_else(|| captures.get("help"));
    let hook_status = captures
        .get("hookStatus")
        .or_else(|| captures.get("agentsStatus"));
    let status = captures.get("status").unwrap_or(captures);
    let config = captures.get("config");
    let worker_probe = captures
        .get("workerProbe")
        .or_else(|| captures.get("workersProbe"));
    let queue = captures
        .get("queue")
        .or_else(|| captures.get("queueStatus"));
    let diagnose = captures.get("diagnose");

    let cli_version = help
        .and_then(|help| string_field_any(help, &["version"]))
        .or_else(|| string_field_any(status, &["version"]))
        .or_else(|| {
            status
                .pointer("/data/daemon/version")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            status
                .pointer("/data/daemon/daemon/version")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| string_field(&value, &["cliVersion", "cli_version"]));
    let direct_exec_available = help.is_some_and(rch_help_exposes_exec_command);
    let codex_hook = rch_codex_hook_capability(hook_status);
    let daemon_status_socket_raw = status
        .pointer("/data/daemon/socket_path")
        .or_else(|| status.pointer("/data/daemon/socketPath"))
        .or_else(|| status.pointer("/data/daemon/daemon/socket_path"))
        .or_else(|| status.pointer("/data/daemon/daemon/socketPath"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let config_socket = config
        .and_then(|config| config.pointer("/data/general/socket_path"))
        .or_else(|| config.and_then(|config| config.pointer("/data/general/socketPath")))
        .or_else(|| config.and_then(|config| config.pointer("/general/socket_path")))
        .or_else(|| config.and_then(|config| config.pointer("/general/socketPath")))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let status_socket_consistent = daemon_status_socket_raw
        .as_ref()
        .zip(config_socket.as_ref())
        .map(|(left, right)| left == right);
    let daemon_status_socket = daemon_status_socket_raw
        .as_deref()
        .map(redact_rch_root_label);
    let worker_probe_summary = rch_worker_probe_summary(worker_probe, status);
    let queue_health = queue
        .and_then(rch_queue_health)
        .or_else(|| rch_queue_health(status));
    let dry_run_would_offload = diagnose.and_then(rch_diagnose_would_offload);
    let remote_only_required = value
        .get("remoteOnlyRequired")
        .or_else(|| value.get("remote_only_required"))
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let route_available = direct_exec_available || codex_hook.installed;
    let workers_probe_ready = worker_probe_summary.status == "ready";
    let queue_start_stalled = queue_health
        .as_ref()
        .is_some_and(|health| health.status == "start_stalled");
    let queue_capacity_blocked = queue_health
        .as_ref()
        .is_some_and(|health| health.status == "capacity_blocked");
    let remote_only_safe = route_available
        && workers_probe_ready
        && !queue_start_stalled
        && !queue_capacity_blocked
        && dry_run_would_offload.unwrap_or(true)
        && status_socket_consistent.unwrap_or(true)
        && (!remote_only_required || route_available);
    let mut degraded = Vec::new();
    let mut recovery = Vec::new();

    if remote_only_required && !route_available {
        degraded.push(SwarmBriefDegradation::warning(
            SwarmBriefSourceKind::Rch,
            RCH_REMOTE_REQUIRED_FALLBACK_PREVENTED_CODE,
            "Remote-only Cargo is required, but this shell has neither `rch exec` nor an installed Codex RCH hook.",
            Some("Use a harness with an installed RCH hook, upgrade RCH to expose `rch exec`, or record static-only evidence.".to_string()),
        ));
        recovery.push("do_not_run_plain_cargo_from_this_shell".to_string());
    }
    if worker_probe_summary.failed_count > 0 && worker_probe_summary.healthy_count == 0 {
        degraded.push(SwarmBriefDegradation::warning(
            SwarmBriefSourceKind::Rch,
            RCH_WORKER_TOPOLOGY_BLOCKED_CODE,
            "RCH status and worker probe evidence disagree or all probed workers failed; remote-only verification must fail closed.",
            Some("Run `rch workers probe --all --json` and repair worker SSH/path topology before Cargo verification.".to_string()),
        ));
        recovery.push("repair_rch_worker_probe_failures".to_string());
    }
    if worker_probe_summary.status == "unknown" {
        degraded.push(SwarmBriefDegradation::warning(
            SwarmBriefSourceKind::Rch,
            RCH_UNAVAILABLE_CODE,
            "RCH worker probe did not prove any healthy remote worker; remote-only verification must fail closed.",
            Some("Run `rch workers probe --all --json` before Cargo verification.".to_string()),
        ));
        recovery.push("prove_rch_worker_probe_health".to_string());
    }
    if dry_run_would_offload == Some(false) {
        degraded.push(SwarmBriefDegradation::warning(
            SwarmBriefSourceKind::Rch,
            RCH_REMOTE_REQUIRED_FALLBACK_PREVENTED_CODE,
            "RCH dry-run diagnosis would not offload a sample Cargo check command; remote-only verification must fail closed.",
            Some("Run `rch diagnose --dry-run --json cargo check --lib` and repair the reported classification or daemon condition.".to_string()),
        ));
        recovery.push("repair_rch_dry_run_offload_classification".to_string());
    }
    if status_socket_consistent == Some(false) {
        degraded.push(SwarmBriefDegradation::warning(
            SwarmBriefSourceKind::Rch,
            RCH_UNAVAILABLE_CODE,
            "RCH daemon socket from status does not match configured socket path.",
            Some("Restart the RCH daemon or reconcile the configured socket path.".to_string()),
        ));
        recovery.push("reconcile_rch_socket_path".to_string());
    }
    if queue_start_stalled {
        degraded.push(SwarmBriefDegradation::warning(
            SwarmBriefSourceKind::Rch,
            RCH_REMOTE_REQUIRED_FALLBACK_PREVENTED_CODE,
            "RCH has queued remote builds that should be startable, but no active build is running; remote-only verification must fail closed before the client can time out toward local fallback.",
            Some("Inspect `rch queue --json`, avoid launching more Cargo jobs, and repair or restart RCH scheduling before remote-required verification.".to_string()),
        ));
        recovery.push("repair_rch_queue_scheduler_before_remote_cargo".to_string());
    }
    if queue_capacity_blocked {
        degraded.push(SwarmBriefDegradation::warning(
            SwarmBriefSourceKind::Rch,
            RCH_REMOTE_REQUIRED_FALLBACK_PREVENTED_CODE,
            "RCH has queued remote builds that need more slots than are currently available; remote-only verification must fail closed before the client can time out toward local fallback.",
            Some("Wait for RCH capacity, use fail-fast queue settings, or record static-only evidence instead of launching more Cargo jobs.".to_string()),
        ));
        recovery.push("wait_for_rch_capacity_or_fail_fast_before_remote_cargo".to_string());
    }
    if recovery.is_empty() {
        recovery.push("remote_only_cargo_allowed_from_this_shell".to_string());
    }
    recovery.sort();
    recovery.dedup();
    degraded.sort();

    Ok(RchLocalCapabilityReport {
        schema: "ee.rch.local_capability.v1",
        cli_version,
        direct_exec_available,
        codex_hook,
        daemon_status_socket,
        status_socket_consistent,
        dry_run_would_offload,
        worker_probe_summary,
        queue_health,
        remote_only_required,
        remote_only_safe,
        degraded,
        recovery,
    })
}

fn rch_help_exposes_exec_command(help: &Value) -> bool {
    rch_command_tree_has(help, "exec")
}

fn rch_command_tree_has(value: &Value, target: &str) -> bool {
    if value.as_str().is_some_and(|name| name == target)
        || value
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|name| name == target)
    {
        return true;
    }

    ["commands", "subcommands", "data", "root"]
        .iter()
        .any(|key| match value.get(*key) {
            Some(Value::Array(items)) => {
                items.iter().any(|item| rch_command_tree_has(item, target))
            }
            Some(nested) => rch_command_tree_has(nested, target),
            None => false,
        })
}

fn rch_codex_hook_capability(value: Option<&Value>) -> RchCodexHookCapability {
    let status = value
        .and_then(|value| {
            value
                .pointer("/data/agents")
                .and_then(Value::as_array)
                .or_else(|| value.pointer("/agents").and_then(Value::as_array))
        })
        .and_then(|agents| {
            agents.iter().find_map(|agent| {
                let name = agent
                    .get("agent")
                    .or_else(|| agent.get("kind"))
                    .or_else(|| agent.get("name"))
                    .and_then(Value::as_str)?;
                (name.eq_ignore_ascii_case("CodexCli")
                    || name.eq_ignore_ascii_case("codex-cli")
                    || name.eq_ignore_ascii_case("Codex CLI"))
                .then(|| {
                    agent
                        .get("status")
                        .or_else(|| agent.get("hook_status"))
                        .or_else(|| agent.get("hookStatus"))
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string()
                })
            })
        })
        .or_else(|| {
            value.and_then(|value| {
                let data = value.get("data").unwrap_or(value);
                let name = data
                    .get("kind")
                    .or_else(|| data.get("agent"))
                    .or_else(|| data.get("name"))
                    .and_then(Value::as_str)?;
                (name.eq_ignore_ascii_case("CodexCli")
                    || name.eq_ignore_ascii_case("codex-cli")
                    || name.eq_ignore_ascii_case("Codex CLI"))
                .then(|| {
                    data.get("hook_status")
                        .or_else(|| data.get("hookStatus"))
                        .or_else(|| data.get("status"))
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string()
                })
            })
        })
        .unwrap_or_else(|| "unknown".to_string());
    let installed = status.eq_ignore_ascii_case("installed");
    RchCodexHookCapability { installed, status }
}

fn rch_worker_probe_summary(probe: Option<&Value>, status: &Value) -> RchWorkerProbeSummary {
    let healthy_count = probe
        .and_then(|probe| numeric_field_any(probe, &["healthy", "healthyCount", "workersHealthy"]))
        .or_else(|| {
            probe.and_then(|probe| {
                numeric_field_any(&probe["data"]["summary"], &["healthy", "healthyCount"])
            })
        })
        .or_else(|| numeric_field_any(status, &["workers_healthy", "workersHealthy"]))
        .or_else(|| {
            numeric_field_any(
                &status["data"]["daemon"]["daemon"],
                &["workers_healthy", "workersHealthy"],
            )
        })
        .unwrap_or(0);
    let failed_count = probe
        .and_then(|probe| numeric_field_any(probe, &["failed", "failedCount", "workersFailed"]))
        .or_else(|| {
            probe.and_then(|probe| {
                numeric_field_any(&probe["data"]["summary"], &["failed", "failedCount"])
            })
        })
        .or_else(|| {
            probe.and_then(|probe| {
                numeric_field_any(&probe["data"]["summary"], &["unhealthy", "unhealthyCount"])
            })
        })
        .or_else(|| {
            probe
                .and_then(|probe| probe.pointer("/data/workers").and_then(Value::as_array))
                .or_else(|| {
                    probe.and_then(|probe| probe.pointer("/data/results").and_then(Value::as_array))
                })
                .or_else(|| {
                    probe.and_then(|probe| probe.pointer("/workers").and_then(Value::as_array))
                })
                .map(|workers| {
                    workers
                        .iter()
                        .filter(|worker| !rch_worker_is_ready(worker))
                        .count() as u64
                })
        })
        .unwrap_or(0);
    let status_label = if healthy_count > 0 && failed_count == 0 {
        "ready"
    } else if healthy_count > 0 {
        "degraded"
    } else if failed_count > 0 {
        "blocked"
    } else {
        "unknown"
    };

    RchWorkerProbeSummary {
        healthy_count,
        failed_count,
        status: status_label.to_string(),
    }
}

fn rch_queue_health(status: &Value) -> Option<RchQueueHealth> {
    let queued_count = rch_build_count(status, "queued_builds", "queuedBuilds")
        .or_else(|| numeric_field_any(status, &["queue_depth", "queueDepth"]))?;
    let active_count =
        rch_build_count(status, "active_builds", "activeBuilds").unwrap_or_else(|| {
            numeric_field_any(status, &["active_builds", "activeBuilds", "running"]).unwrap_or(0)
        });
    let slots_available = rch_slots_available(status);
    let first_slots_needed = rch_first_queued_slots_needed(status);
    let startable_now = queued_count > 0
        && active_count == 0
        && slots_available
            .zip(first_slots_needed)
            .is_some_and(|(available, needed)| available >= needed);
    let capacity_blocked = queued_count > 0
        && slots_available
            .zip(first_slots_needed)
            .is_some_and(|(available, needed)| available < needed);
    let status_label = if startable_now {
        "start_stalled"
    } else if capacity_blocked {
        "capacity_blocked"
    } else if queued_count > 0 {
        "queued"
    } else {
        "clear"
    };

    Some(RchQueueHealth {
        queued_count,
        active_count,
        slots_available,
        status: status_label.to_string(),
    })
}

fn rch_build_count(status: &Value, snake_key: &str, camel_key: &str) -> Option<u64> {
    rch_build_array(status, snake_key, camel_key)
        .map(|items| items.len() as u64)
        .or_else(|| numeric_field_any(status, &[snake_key, camel_key]))
}

fn rch_build_array<'a>(
    status: &'a Value,
    snake_key: &str,
    camel_key: &str,
) -> Option<&'a Vec<Value>> {
    status
        .get(snake_key)
        .or_else(|| status.get(camel_key))
        .or_else(|| status.get("data").and_then(|data| data.get(snake_key)))
        .or_else(|| status.get("data").and_then(|data| data.get(camel_key)))
        .or_else(|| {
            status
                .pointer("/data/daemon")
                .and_then(|daemon| daemon.get(snake_key))
        })
        .or_else(|| {
            status
                .pointer("/data/daemon")
                .and_then(|daemon| daemon.get(camel_key))
        })
        .and_then(Value::as_array)
}

fn rch_slots_available(status: &Value) -> Option<u64> {
    numeric_field_any(status, &["slots_available", "slotsAvailable"]).or_else(|| {
        numeric_field_any(
            &status["data"]["daemon"]["daemon"],
            &["slots_available", "slotsAvailable"],
        )
    })
}

fn rch_first_queued_slots_needed(status: &Value) -> Option<u64> {
    rch_build_array(status, "queued_builds", "queuedBuilds").and_then(|items| {
        items
            .first()
            .and_then(|item| numeric_field_any(item, &["slots_needed", "slotsNeeded", "slots"]))
    })
}

fn rch_diagnose_would_offload(value: &Value) -> Option<bool> {
    value
        .pointer("/data/dry_run/would_offload")
        .or_else(|| value.pointer("/data/dryRun/wouldOffload"))
        .or_else(|| value.pointer("/dry_run/would_offload"))
        .or_else(|| value.pointer("/dryRun/wouldOffload"))
        .or_else(|| value.pointer("/data/decision/would_intercept"))
        .or_else(|| value.pointer("/data/decision/wouldIntercept"))
        .and_then(Value::as_bool)
}

fn rch_command_error_to_degradation(error: &SwarmBriefCommandError) -> SwarmBriefDegradation {
    let message = match error {
        SwarmBriefCommandError::Unavailable(message) => message.as_str(),
        SwarmBriefCommandError::Failed { stderr, .. } => stderr.as_str(),
        SwarmBriefCommandError::TimedOut { .. } | SwarmBriefCommandError::InvalidUtf8(_) => "",
    };
    if is_rch_worker_topology_blocked(message) {
        SwarmBriefDegradation::warning(
            SwarmBriefSourceKind::Rch,
            RCH_WORKER_TOPOLOGY_BLOCKED_CODE,
            summarize_rch_topology_blocked_message(message),
            Some(
                "Inspect RCH worker path mapping; remote workers are visible but this workspace cannot be mapped."
                    .to_string(),
            ),
        )
    } else if is_rch_remote_required_fallback_prevented(message) {
        SwarmBriefDegradation::warning(
            SwarmBriefSourceKind::Rch,
            RCH_REMOTE_REQUIRED_FALLBACK_PREVENTED_CODE,
            "RCH_REQUIRE_REMOTE prevented local fallback, so this Cargo gate has no valid remote evidence.",
            Some(
                "Fix remote worker availability or unset the remote-required guard only with explicit approval."
                    .to_string(),
            ),
        )
    } else {
        error.to_degradation(
            SwarmBriefSourceKind::Rch,
            RCH_UNAVAILABLE_CODE,
            "rch status --json",
        )
    }
}

fn is_rch_worker_topology_blocked(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("rch-e327")
        || (lower.contains("worker") && lower.contains("topology"))
        || (lower.contains("worker") && lower.contains("path") && lower.contains("map"))
}

fn is_rch_remote_required_fallback_prevented(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("rch_require_remote")
        || (lower.contains("remote") && lower.contains("required") && lower.contains("fallback"))
}

fn rch_remote_posture(value: &Value) -> Option<&'static str> {
    let status = string_field_any(
        value,
        &[
            "status",
            "state",
            "posture",
            "remoteStatus",
            "remote_status",
        ],
    )
    .map(|status| status.to_ascii_lowercase());
    if let Some(status) = status.as_deref() {
        if status.contains("ready") || status.contains("healthy") {
            return Some(RCH_POSTURE_REMOTE_READY);
        }
        if status.contains("local_only")
            || status.contains("no_remote")
            || status.contains("all_workers_offline")
        {
            return Some(RCH_POSTURE_NO_REMOTE_WORKERS);
        }
        if status.contains("unreachable")
            || status.contains("offline")
            || status.contains("unhealthy")
            || status.contains("blocked")
        {
            return Some(RCH_POSTURE_WORKER_UNREACHABLE);
        }
    }

    if let Some(healthy) = numeric_field_any(
        value,
        &[
            "workers_healthy",
            "workersHealthy",
            "healthyWorkers",
            "remoteWorkersHealthy",
        ],
    ) {
        return Some(if healthy > 0 {
            RCH_POSTURE_REMOTE_READY
        } else {
            RCH_POSTURE_NO_REMOTE_WORKERS
        });
    }

    let workers = rch_workers(value)?;
    if workers.is_empty() {
        Some(RCH_POSTURE_NO_REMOTE_WORKERS)
    } else if workers.iter().any(rch_worker_is_ready) {
        Some(RCH_POSTURE_REMOTE_READY)
    } else if workers.iter().all(rch_worker_is_unreachable) {
        Some(RCH_POSTURE_WORKER_UNREACHABLE)
    } else {
        Some(RCH_POSTURE_NO_REMOTE_WORKERS)
    }
}

fn rch_workers(value: &Value) -> Option<&Vec<Value>> {
    value
        .get("workers")
        .and_then(Value::as_array)
        .or_else(|| value.get("data")?.get("workers").and_then(Value::as_array))
}

fn rch_worker_is_ready(worker: &Value) -> bool {
    string_field(worker, &["status", "state", "health"])
        .map(|status| {
            let status = status.to_ascii_lowercase();
            status.contains("ready") || status.contains("healthy") || status.contains("online")
        })
        .unwrap_or(false)
}

fn rch_worker_is_unreachable(worker: &Value) -> bool {
    string_field(worker, &["status", "state", "health"])
        .map(|status| {
            let status = status.to_ascii_lowercase();
            status.contains("unreachable")
                || status.contains("offline")
                || status.contains("unhealthy")
                || status.contains("down")
        })
        .unwrap_or(false)
}

fn rch_selected_worker(value: &Value) -> Option<String> {
    string_field_any(
        value,
        &[
            "selected_worker",
            "selectedWorker",
            "worker_id",
            "workerId",
            "worker",
        ],
    )
}

fn rch_topology_root_summary(value: &Value) -> Option<String> {
    let canonical = string_field_any(
        value,
        &[
            "canonical_project_root",
            "canonicalProjectRoot",
            "canonical_root",
            "canonicalRoot",
        ],
    );
    let alias = string_field_any(
        value,
        &[
            "alias_project_root",
            "aliasProjectRoot",
            "alias_root",
            "aliasRoot",
        ],
    );
    let mut parts = Vec::new();
    if let Some(canonical) = canonical {
        parts.push(format!(
            "canonical={}",
            redact_rch_root_label(canonical.as_str())
        ));
    }
    if let Some(alias) = alias {
        parts.push(format!("alias={}", redact_rch_root_label(alias.as_str())));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn redact_rch_root_label(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    let label = Path::new(trimmed)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| {
            !value.is_empty()
                && value
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
        })
        .unwrap_or("redacted");
    format!("<path:{label}>")
}

fn summarize_rch_topology_blocked_message(message: &str) -> String {
    let worker = extract_rch_worker_from_message(message)
        .map(|worker| format!("; selected worker: {worker}"))
        .unwrap_or_default();
    format!(
        "RCH-E327 worker topology blocked remote-required verification{worker}; root metadata redacted; remote workers may be visible but this workspace cannot be mapped."
    )
}

fn extract_rch_worker_from_message(message: &str) -> Option<String> {
    message
        .split(|ch: char| ch.is_ascii_whitespace() || matches!(ch, ',' | ';'))
        .find_map(|token| token.strip_prefix("worker="))
        .map(|worker| {
            worker
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
                .collect::<String>()
        })
        .filter(|worker| !worker.is_empty())
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

fn string_field_any(value: &Value, keys: &[&str]) -> Option<String> {
    string_field(value, keys)
        .or_else(|| value.get("data").and_then(|data| string_field(data, keys)))
}

fn numeric_field(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_u64))
}

fn numeric_field_any(value: &Value, keys: &[&str]) -> Option<u64> {
    numeric_field(value, keys)
        .or_else(|| value.get("data").and_then(|data| numeric_field(data, keys)))
}

fn redact_brief_text(input: &str) -> String {
    let secret_redacted = redact_secret_like_content(input).content;
    redact_absolute_path_like_segments(&secret_redacted)
}

fn redact_absolute_path_like_segments(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut offset = 0usize;

    while let Some(relative_start) = input[offset..].find('/') {
        let start = offset + relative_start;
        output.push_str(&input[offset..start]);

        if !is_absolute_path_redaction_start(input, start) {
            output.push('/');
            offset = start + '/'.len_utf8();
            continue;
        }

        let end = input[start..]
            .char_indices()
            .find_map(|(idx, ch)| {
                (idx > 0 && is_absolute_path_redaction_delimiter(ch)).then_some(start + idx)
            })
            .unwrap_or(input.len());
        let candidate = &input[start..end];
        let (path_candidate, trailing_punctuation) = split_absolute_path_candidate(candidate);

        if should_redact_absolute_path_candidate(path_candidate) {
            output.push_str(&format!(
                "[REDACTED_PATH:{}]",
                blake3_summary_hash(path_candidate)
                    .trim_start_matches("blake3:")
                    .get(..12)
                    .unwrap_or("unknown")
            ));
            output.push_str(trailing_punctuation);
        } else {
            output.push_str(candidate);
        }
        offset = end;
    }

    output.push_str(&input[offset..]);
    output
}

fn is_absolute_path_redaction_start(input: &str, start: usize) -> bool {
    if input[start..].starts_with("//") {
        return false;
    }
    let previous = input[..start].chars().next_back();
    let next = input[start + '/'.len_utf8()..].chars().next();
    let previous_allows_path = previous.is_none_or(|ch| {
        ch.is_whitespace() || matches!(ch, '"' | '\'' | '`' | '(' | '[' | '{' | ':' | '=')
    });
    let next_allows_path = next.is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '.');
    previous_allows_path && next_allows_path
}

fn is_absolute_path_redaction_delimiter(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '"' | '\'' | '`' | '<' | '>' | ')' | ']' | '}' | ',' | ';' | '|'
        )
}

fn split_absolute_path_candidate(candidate: &str) -> (&str, &str) {
    let path_end = candidate.trim_end_matches(['.', ':']).len();
    (&candidate[..path_end], &candidate[path_end..])
}

fn should_redact_absolute_path_candidate(candidate: &str) -> bool {
    candidate
        .strip_prefix('/')
        .is_some_and(|without_root| without_root.contains('/'))
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
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use super::*;

    #[derive(Default)]
    struct FakeRunner {
        outputs: BTreeMap<String, Result<SwarmBriefCommandOutput, SwarmBriefCommandError>>,
        calls: RefCell<Vec<String>>,
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

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
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
            self.calls.borrow_mut().push(command_key(program, args));
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
    fn summary_redacts_raw_content_and_hashes_underlying_brief() {
        let raw_secret = format!("{}{}", "api_key=sk-live-", "A".repeat(32));
        let raw_remote_workspace = "/Users/alice/private/repo";
        let mut report = report_with_ready_sources();
        report.beads.ready.push(bead(
            "eidetic_engine_cli-pswb",
            &format!(
                "[swarm-brief] Support bundle handoff {raw_secret} from {raw_remote_workspace}"
            ),
            "ready",
        ));
        report.dirty_files.push(SwarmBriefDirtyFile {
            path: format!("src/core/support_bundle.rs {raw_secret} {raw_remote_workspace}"),
            status: "M".to_string(),
        });
        apply_swarm_brief_advice(&mut report);
        report.finalize();

        let summary = summarize_swarm_brief_report(&report);
        let rendered = stable_summary_json(&summary);
        assert_eq!(
            summary.pointer("/schema"),
            Some(&json!(SWARM_BRIEF_SUMMARY_SCHEMA_V1))
        );
        assert_eq!(
            summary.pointer("/singleFlight/schema"),
            Some(&json!("ee.singleflight.posture.v1"))
        );
        assert!(
            summary
                .pointer("/singleFlight/surfaces/0/surface")
                .and_then(Value::as_str)
                .is_some(),
            "summary must expose redaction-safe single-flight surface posture"
        );
        assert_eq!(
            summary.pointer("/redaction/rawMailBodiesIncluded"),
            Some(&json!(false))
        );
        assert_eq!(
            summary.pointer("/redaction/fullFileListingsIncluded"),
            Some(&json!(false))
        );
        assert!(
            !rendered.contains(&raw_secret),
            "summary must not expose raw secret-like bead titles or file paths"
        );
        assert!(
            !rendered.contains(raw_remote_workspace),
            "summary must not expose raw remote workspace paths"
        );
        assert!(
            !rendered.contains("raw_query") && !rendered.contains("memory_body"),
            "single-flight summary must not expose raw query or memory body labels"
        );
        assert!(
            rendered.contains("[REDACTED_PATH:"),
            "summary should preserve path-presence evidence as a stable redaction marker"
        );
        assert!(
            summary
                .pointer("/reportHash")
                .and_then(Value::as_str)
                .is_some_and(|hash| hash.starts_with("blake3:")),
            "summary must hash the underlying brief"
        );
        assert!(
            summary
                .pointer("/topRecommendations/0/evidenceHashes")
                .and_then(Value::as_array)
                .is_some_and(|hashes| !hashes.is_empty()),
            "summary must expose recommendation evidence as hashes"
        );
        assert_eq!(
            summary.pointer("/fileSurfaceRiskSummary/topRisks/0/rawPathIncluded"),
            Some(&json!(false))
        );
        assert!(
            summary
                .pointer("/fileSurfaceRiskSummary/topRisks/0/pathHash")
                .and_then(Value::as_str)
                .is_some_and(|hash| hash.starts_with("blake3:")),
            "summary must hash high-risk file paths instead of listing them"
        );
    }

    #[test]
    fn handoff_summary_text_mentions_singleflight_posture_without_raw_keys() {
        let mut report = report_with_ready_sources();
        apply_swarm_brief_advice(&mut report);
        report.finalize();

        let summary = summarize_swarm_brief_report(&report);
        let rendered = render_swarm_brief_summary_for_handoff(&summary);

        assert!(
            rendered.contains("Single-flight posture: status="),
            "handoff text must include single-flight aggregate posture"
        );
        assert!(
            !rendered.contains("keyHash")
                && !rendered.contains("queryShapeHash")
                && !rendered.contains("workspaceHash"),
            "handoff text should stay compact and omit raw key-shape field names"
        );
    }

    #[test]
    fn qos_pressure_hints_raise_swarm_brief_resource_posture_without_raw_request() {
        let mut report = report_with_ready_sources();
        let summary = super::super::qos::QosLaneSummary {
            schema: super::super::qos::QOS_ACTIVE_LANE_SUMMARY_SCHEMA_V1.to_string(),
            workspace_hash: "sha256:workspace".to_string(),
            active_records: Vec::new(),
            foreground_active_count: 1,
            background_active_count: 2,
            verification_active_count: 1,
            maintenance_active_count: 1,
            stale_ignored_count: 1,
            degraded: Vec::new(),
        };

        attach_qos_summary_for_test(&mut report, &summary);
        apply_swarm_brief_advice(&mut report);
        report.finalize();

        let summary = summarize_swarm_brief_report(&report);
        assert_eq!(
            summary.pointer("/resourcePressurePosture"),
            Some(&json!("high"))
        );
        assert!(
            report.resource_pressure.iter().any(|hint| {
                hint.source == SwarmBriefSourceKind::Qos
                    && hint.level == "high"
                    && hint.message.contains("foreground pressure")
            }),
            "foreground QoS pressure should become a high resource-pressure hint"
        );
        assert!(
            report.resource_pressure.iter().any(|hint| {
                hint.source == SwarmBriefSourceKind::Qos
                    && hint.level == "medium"
                    && hint.message.contains("background derived work")
            }),
            "background derived QoS work should be visible without raw task content"
        );
        let rendered = stable_summary_json(&summary);
        assert!(
            rendered.contains("\"resourcePressurePosture\":\"high\""),
            "support-bundle swarm summary should expose compact QoS pressure posture"
        );
        assert!(
            !rendered.contains("request_text") && !rendered.contains("summarize private task"),
            "QoS pressure summary must not expose raw request text"
        );
    }

    #[test]
    fn brief_text_redacts_absolute_workspace_paths_without_touching_urls() {
        let raw_remote_workspace = "/Users/alice/private/repo";
        let rendered = redact_brief_text(&format!(
            "blocked origin={raw_remote_workspace}, docs=https://example.test/a/b and alias=remote-beta"
        ));

        assert!(
            !rendered.contains(raw_remote_workspace),
            "raw absolute workspace path should be redacted"
        );
        assert!(
            rendered.contains("[REDACTED_PATH:"),
            "redacted path marker should preserve the presence of a path-like value"
        );
        assert!(
            redact_brief_text("/Users/alice/private/repo.").ends_with("]."),
            "path redaction should preserve trailing sentence punctuation"
        );
        assert!(
            rendered.contains("https://example.test/a/b"),
            "URL paths are not workspace labels and should remain readable"
        );
        assert!(
            rendered.contains("alias=remote-beta"),
            "non-path namespace aliases should remain readable"
        );
    }

    #[test]
    fn summary_hash_changes_when_underlying_brief_changes() {
        let mut first = report_with_ready_sources();
        first.beads.ready.push(bead(
            "eidetic_engine_cli-a111",
            "[swarm-brief] First ready bead",
            "ready",
        ));
        apply_swarm_brief_advice(&mut first);
        first.finalize();

        let mut second = first.clone();
        second.beads.ready.push(bead(
            "eidetic_engine_cli-b222",
            "[swarm-brief] Second ready bead",
            "ready",
        ));
        apply_swarm_brief_advice(&mut second);
        second.finalize();

        let first_hash = summarize_swarm_brief_report(&first)
            .pointer("/reportHash")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let second_hash = summarize_swarm_brief_report(&second)
            .pointer("/reportHash")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        assert_ne!(first_hash, second_hash);
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
    fn advisor_does_not_infer_no_ready_work_when_beads_skipped() {
        let mut report = report_with_ready_sources();
        for source in &mut report.sources {
            if source.source == SwarmBriefSourceKind::Beads {
                source.status = SwarmBriefSourceStatus::Skipped;
            }
        }

        apply_swarm_brief_advice(&mut report);

        assert!(
            report
                .recommendations
                .iter()
                .all(|recommendation| recommendation.id != "rec.work_selection.no_ready_beads")
        );
        assert!(
            report
                .recommendations
                .iter()
                .any(|recommendation| recommendation.id == "rec.degraded.beads.beads_missing")
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
        assert_eq!(risk.reservation_holders, vec!["OtherAgent".to_string()]);
        assert!(
            risk.related_bead_ids
                .contains(&"eidetic_engine_cli-u7r5".to_string())
        );
        assert!(
            risk.suggested_commands
                .iter()
                .any(|command| command.contains("message OtherAgent before editing"))
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
        assert_eq!(risk.git_status_buckets, vec!["M".to_string()]);
        assert!(
            risk.suggested_commands
                .iter()
                .any(|command| command.starts_with("git status --short -- "))
        );
        let rec = recommendation(&report, "rec.candidate.eidetic_engine_cli-u7r5");
        assert!(
            rec.reason_codes
                .contains(&"candidate_blocked_by_surface_conflict".to_string())
        );
    }

    #[test]
    fn summary_counts_file_surface_ownership_risks_without_listing_paths() {
        let mut report = report_with_ready_sources();
        report.dirty_files.push(SwarmBriefDirtyFile {
            path: "src/core/swarm_brief.rs".to_string(),
            status: "M".to_string(),
        });
        report.file_reservations.push(SwarmBriefFileReservation {
            path_pattern: "src/core/swarm_brief.rs".to_string(),
            holder: "OtherAgent".to_string(),
            exclusive: true,
            expires_at: Some("2026-05-09T08:00:00Z".to_string()),
        });

        apply_swarm_brief_advice(&mut report);
        report.finalize();

        let summary = summarize_swarm_brief_report(&report);
        let rendered = stable_summary_json(&summary);
        assert_eq!(
            summary.pointer("/fileSurfaceRiskSummary/countsByReservationHolder/OtherAgent"),
            Some(&json!(1))
        );
        assert_eq!(
            summary.pointer("/fileSurfaceRiskSummary/countsByGitStatus/M"),
            Some(&json!(1))
        );
        assert_eq!(
            summary.pointer("/fileSurfaceRiskSummary/topRisks/0/reservationHolders/0"),
            Some(&json!("OtherAgent"))
        );
        assert!(
            !rendered.contains("src/core/swarm_brief.rs"),
            "support-bundle summary must not include raw file listings"
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
    fn agent_mail_health_snapshot_degrades_transport_fallback() {
        let snapshot = require_ok(
            parse_agent_mail_snapshot_json(
                r#"{
              "schema":"ee.swarm.coordination_health.v1",
              "mcp_http_reachable":false,
              "am_agents_list_ok":true,
              "am_send_single_recipient_ok":true,
              "am_send_multi_recipient_ok":false,
              "observed_panic":"RefCell already borrowed",
              "fallback_active":true
            }"#,
            ),
            "valid Agent Mail health JSON",
        );

        assert_eq!(snapshot.degraded.len(), 1);
        let degradation = &snapshot.degraded[0];
        assert_eq!(degradation.code, AGENT_MAIL_UNAVAILABLE_CODE);
        assert_eq!(degradation.source, SwarmBriefSourceKind::AgentMail);
        assert!(degradation.message.contains("mcp_http"));
        assert!(degradation.message.contains("am_send_multi_recipient"));
        assert!(degradation.message.contains("RefCell already borrowed"));
        let source = SwarmBriefSourceSnapshot::ready(
            SwarmBriefSourceKind::AgentMail,
            SwarmBriefSourceProvenance::local_probe(),
            0,
        )
        .with_degraded(snapshot.degraded);
        assert_eq!(source.status, SwarmBriefSourceStatus::Degraded);
    }

    #[test]
    fn beads_sync_status_jsonl_newer_marks_source_degraded_not_unavailable() {
        let options = SwarmBriefCollectOptions::for_workspace(".");
        let runner = FakeRunner::default()
            .with_output(
                "br",
                &[
                    "sync",
                    "--status",
                    "--json",
                    "--no-auto-import",
                    "--allow-stale",
                ],
                r#"{"jsonl_newer":true,"db_newer":false,"last_import_time":"2026-05-14T05:20:52Z"}"#,
            )
            .with_output(
                "br",
                &["ready", "--json"],
                r#"[{"id":"bd-ready","title":"Ready work","status":"open"}]"#,
            )
            .with_output("br", &["blocked", "--json"], "[]")
            .with_output("br", &["list", "--status", "in_progress", "--json"], "[]")
            .with_output("br", &["list", "--status", "deferred", "--json"], "[]")
            .with_output("br", &["dep", "cycles", "--json"], r#"{"cycles":[],"count":0}"#);

        let output = BeadsSourceAdapter { runner: &runner }.collect(&options);

        assert_eq!(output.snapshot.status, SwarmBriefSourceStatus::Degraded);
        assert_eq!(output.snapshot.freshness.state, "stale");
        assert!(
            output
                .snapshot
                .degraded
                .iter()
                .any(|item| item.code == BEADS_TRACKER_STALE_CODE)
        );
        match output.contribution {
            SwarmBriefContribution::Beads(summary) => assert_eq!(summary.ready.len(), 1),
            other => panic!("expected Beads contribution, got {other:?}"),
        }
    }

    #[test]
    fn beads_sync_status_db_newer_marks_export_pending_not_unavailable() {
        let options = SwarmBriefCollectOptions::for_workspace(".");
        let runner = FakeRunner::default()
            .with_output(
                "br",
                &[
                    "sync",
                    "--status",
                    "--json",
                    "--no-auto-import",
                    "--allow-stale",
                ],
                r#"{"jsonl_newer":false,"db_newer":true,"last_import_time":"2026-05-14T05:20:52Z"}"#,
            )
            .with_output(
                "br",
                &["ready", "--json"],
                r#"[{"id":"bd-ready","title":"Ready work","status":"open"}]"#,
            )
            .with_output("br", &["blocked", "--json"], "[]")
            .with_output("br", &["list", "--status", "in_progress", "--json"], "[]")
            .with_output("br", &["list", "--status", "deferred", "--json"], "[]")
            .with_output("br", &["dep", "cycles", "--json"], r#"{"cycles":[],"count":0}"#);

        let output = BeadsSourceAdapter { runner: &runner }.collect(&options);

        assert_eq!(output.snapshot.status, SwarmBriefSourceStatus::Degraded);
        assert_eq!(output.snapshot.freshness.state, "stale");
        let Some(degradation) = output
            .snapshot
            .degraded
            .iter()
            .find(|item| item.code == BEADS_TRACKER_STALE_CODE)
        else {
            panic!("beads tracker stale degradation");
        };
        assert!(degradation.message.contains("database is newer than JSONL"));
        assert_eq!(degradation.repair.as_deref(), Some("br sync --flush-only"));
        match output.contribution {
            SwarmBriefContribution::Beads(summary) => assert_eq!(summary.ready.len(), 1),
            other => panic!("expected Beads contribution, got {other:?}"),
        }
    }

    #[test]
    fn beads_sync_status_failure_preserves_bucket_results_with_degraded_freshness() {
        let options = SwarmBriefCollectOptions::for_workspace(".");
        let runner = FakeRunner::default()
            .with_output(
                "br",
                &[
                    "sync",
                    "--status",
                    "--json",
                    "--no-auto-import",
                    "--allow-stale",
                ],
                "not-json",
            )
            .with_output(
                "br",
                &["ready", "--json"],
                r#"[{"id":"bd-ready","title":"Ready work","status":"open"}]"#,
            )
            .with_output("br", &["blocked", "--json"], "[]")
            .with_output("br", &["list", "--status", "in_progress", "--json"], "[]")
            .with_output("br", &["list", "--status", "deferred", "--json"], "[]")
            .with_output(
                "br",
                &["dep", "cycles", "--json"],
                r#"{"cycles":[],"count":0}"#,
            );

        let output = BeadsSourceAdapter { runner: &runner }.collect(&options);

        assert_eq!(output.snapshot.status, SwarmBriefSourceStatus::Degraded);
        assert_eq!(output.snapshot.freshness.state, "current");
        assert!(
            output
                .snapshot
                .degraded
                .iter()
                .any(|item| item.code == BEADS_UNAVAILABLE_CODE)
        );
        match output.contribution {
            SwarmBriefContribution::Beads(summary) => assert_eq!(summary.ready.len(), 1),
            other => panic!("expected Beads contribution, got {other:?}"),
        }
    }

    #[test]
    fn beads_dependency_cycles_are_collected_in_summary() {
        let options = SwarmBriefCollectOptions::for_workspace(".");
        let runner = FakeRunner::default()
            .with_output(
                "br",
                &[
                    "sync",
                    "--status",
                    "--json",
                    "--no-auto-import",
                    "--allow-stale",
                ],
                r#"{"jsonl_newer":false,"db_newer":false}"#,
            )
            .with_output(
                "br",
                &["ready", "--json"],
                r#"[{"id":"bd-ready","title":"Ready work","status":"open"}]"#,
            )
            .with_output("br", &["blocked", "--json"], "[]")
            .with_output("br", &["list", "--status", "in_progress", "--json"], "[]")
            .with_output("br", &["list", "--status", "deferred", "--json"], "[]")
            .with_output(
                "br",
                &["dep", "cycles", "--json"],
                r#"{"cycles":[["bd-b","bd-a","bd-b"],["bd-z","bd-y","bd-z"]],"count":2}"#,
            );

        let output = BeadsSourceAdapter { runner: &runner }.collect(&options);

        assert_eq!(output.snapshot.status, SwarmBriefSourceStatus::Ready);
        assert_eq!(output.snapshot.item_count, 3);
        match output.contribution {
            SwarmBriefContribution::Beads(summary) => {
                let cycles =
                    require_some(summary.dependency_cycle_summary, "dependency cycle summary");
                assert_eq!(cycles.count, 2);
                assert_eq!(cycles.examples.len(), 2);
                assert!(cycles.examples.contains(&vec![
                    "bd-b".to_string(),
                    "bd-a".to_string(),
                    "bd-b".to_string()
                ]));
            }
            other => panic!("expected Beads contribution, got {other:?}"),
        }
    }

    #[test]
    fn beads_sync_status_stale_survives_bucket_unavailable() {
        let options = SwarmBriefCollectOptions::for_workspace(".");
        let runner = FakeRunner::default()
            .with_output(
                "br",
                &[
                    "sync",
                    "--status",
                    "--json",
                    "--no-auto-import",
                    "--allow-stale",
                ],
                r#"{"jsonl_newer":true,"db_newer":false,"last_import_time":"2026-05-14T05:20:52Z"}"#,
            )
            .with_error(
                "br",
                &["ready", "--json"],
                SwarmBriefCommandError::Unavailable("br ready failed".to_string()),
            )
            .with_error(
                "br",
                &["blocked", "--json"],
                SwarmBriefCommandError::Unavailable("br blocked failed".to_string()),
            )
            .with_error(
                "br",
                &["list", "--status", "in_progress", "--json"],
                SwarmBriefCommandError::Unavailable("br in_progress failed".to_string()),
            )
            .with_error(
                "br",
                &["list", "--status", "deferred", "--json"],
                SwarmBriefCommandError::Unavailable("br deferred failed".to_string()),
            );

        let output = BeadsSourceAdapter { runner: &runner }.collect(&options);

        assert_eq!(output.snapshot.status, SwarmBriefSourceStatus::Unavailable);
        assert_eq!(output.snapshot.freshness.state, "stale");
        assert!(
            output
                .snapshot
                .degraded
                .iter()
                .any(|item| item.code == BEADS_TRACKER_STALE_CODE)
        );
        assert!(
            output
                .snapshot
                .degraded
                .iter()
                .any(|item| item.code == BEADS_UNAVAILABLE_CODE)
        );
        assert!(matches!(output.contribution, SwarmBriefContribution::None));
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
    fn rch_parser_reports_worker_posture_and_redacted_topology_metadata() {
        let hints = require_ok(
            parse_rch_status_json(
                r#"{
                    "status":"ready",
                    "workersHealthy":3,
                    "selectedWorker":"css",
                    "canonicalProjectRoot":"/Users/jemanuel/projects",
                    "aliasProjectRoot":"/data/projects",
                    "queueDepth":0,
                    "activeBuilds":0
                }"#,
            ),
            "valid rch JSON",
        );
        let messages = hints
            .iter()
            .map(|hint| hint.message.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(messages.contains("rch remote posture: remote_ready"));
        assert!(messages.contains("rch selected worker: css"));
        assert!(
            messages
                .contains("rch topology roots: canonical=<path:projects>, alias=<path:projects>")
        );
        assert!(!messages.contains("/Users/jemanuel"));
        assert!(!messages.contains("/data/projects"));
    }

    #[test]
    fn rch_parser_distinguishes_no_workers_and_unreachable_workers() {
        let no_workers = require_ok(
            parse_rch_status_json(r#"{"workersHealthy":0}"#),
            "no workers rch JSON",
        );
        assert!(no_workers.iter().any(|hint| {
            hint.message == "rch remote posture: no_remote_workers" && hint.level == "high"
        }));

        let unreachable = require_ok(
            parse_rch_status_json(
                r#"{"workers":[{"id":"css","status":"unreachable"},{"id":"gpu","status":"offline"}]}"#,
            ),
            "unreachable workers rch JSON",
        );
        assert!(unreachable.iter().any(|hint| {
            hint.message == "rch remote posture: worker_unreachable" && hint.level == "high"
        }));
    }

    #[test]
    fn rch_local_capability_parses_current_rch_json_shapes() {
        let report = require_ok(
            parse_rch_local_capability_snapshot(
                r#"{
                    "schema":"ee.rch.local_capability.capture.v1",
                    "remoteOnlyRequired":true,
                    "captures":{
                        "helpJson":{
                            "version":"1.0.24",
                            "subcommands":[{"name":"status"},{"name":"exec"}]
                        },
                        "hookStatus":{
                            "data":{
                                "agents":[
                                    {
                                        "kind":"CodexCli",
                                        "name":"Codex CLI",
                                        "hook_status":"Not installed"
                                    }
                                ]
                            }
                        },
                        "status":{
                            "data":{
                                "daemon":{
                                    "daemon":{
                                        "version":"0.1.3",
                                        "socket_path":"/Users/jemanuel/Library/Caches/rch/rch.sock",
                                        "workers_healthy":3
                                    }
                                }
                            }
                        },
                        "config":{
                            "data":{
                                "general":{
                                    "socket_path":"/Users/jemanuel/Library/Caches/rch/rch.sock"
                                }
                            }
                        },
                        "workerProbe":{
                            "data":{
                                "summary":{"healthy":3,"failed":0},
                                "results":[{"id":"css","status":"ok"}]
                            }
                        },
                        "diagnose":{
                            "data":{
                                "dry_run":{"would_offload":true}
                            }
                        }
                    }
                }"#,
            ),
            "current rch JSON shapes",
        );

        assert_eq!(report.cli_version.as_deref(), Some("1.0.24"));
        assert!(report.direct_exec_available);
        assert!(!report.codex_hook.installed);
        assert_eq!(report.codex_hook.status, "Not installed");
        assert_eq!(
            report.daemon_status_socket.as_deref(),
            Some("<path:rch.sock>")
        );
        assert_eq!(report.status_socket_consistent, Some(true));
        assert_eq!(report.dry_run_would_offload, Some(true));
        assert_eq!(report.worker_probe_summary.healthy_count, 3);
        assert_eq!(report.worker_probe_summary.status, "ready");
        assert!(report.remote_only_safe);
        assert!(report.degraded.is_empty());
    }

    #[test]
    fn rch_local_capability_fixture_fails_closed_for_codex_without_remote_route() {
        let fixture =
            include_str!("../../tests/fixtures/swarm/rch_codex_capability_contradiction.json");
        let report = require_ok(
            parse_rch_local_capability_snapshot(fixture),
            "valid rch capability fixture",
        );
        let expected: Value = require_some(
            require_ok(serde_json::from_str::<Value>(fixture), "fixture JSON")
                .pointer("/expected")
                .cloned(),
            "expected block",
        );
        let codes = report
            .degraded
            .iter()
            .map(|degradation| degradation.code.as_str())
            .collect::<BTreeSet<_>>();

        assert!(!report.direct_exec_available);
        assert!(!report.codex_hook.installed);
        assert_eq!(report.codex_hook.status, "Not installed");
        assert_eq!(report.worker_probe_summary.status, "blocked");
        assert_eq!(
            report.daemon_status_socket.as_deref(),
            Some("<path:rch.sock>")
        );
        assert_eq!(
            report.remote_only_safe,
            require_some(
                expected["remoteOnlySafe"].as_bool(),
                "expected remoteOnlySafe"
            )
        );
        for code in require_some(
            expected["degradedCodes"].as_array(),
            "expected degradedCodes",
        ) {
            let code = require_some(code.as_str(), "expected degraded code");
            assert!(
                codes.contains(code),
                "missing expected degraded code {code}"
            );
        }
        for recovery in require_some(expected["recovery"].as_array(), "expected recovery") {
            let recovery = require_some(recovery.as_str(), "expected recovery action");
            assert!(
                report.recovery.contains(&recovery.to_string()),
                "missing expected recovery action {recovery}"
            );
        }
    }

    #[test]
    fn rch_local_capability_allows_remote_only_when_exec_and_workers_are_ready() {
        let report = require_ok(
            parse_rch_local_capability_snapshot(
                r#"{
                    "schema":"ee.rch.local_capability.fixture.v1",
                    "remoteOnlyRequired":true,
                    "captures":{
                        "helpJson":{"commands":[{"name":"status"},{"name":"exec"}]},
                        "hookStatus":{"data":{"agents":[{"agent":"CodexCli","status":"Not installed"}]}},
                        "status":{"data":{"daemon":{"version":"0.2.0","socket_path":"/tmp/rch.sock","workers_healthy":1}}},
                        "config":{"data":{"general":{"socket_path":"/tmp/rch.sock"}}},
                        "workerProbe":{"data":{"healthy":1,"failed":0}},
                        "diagnose":{"data":{"dry_run":{"would_offload":true}}}
                    }
                }"#,
            ),
            "valid safe rch capability fixture",
        );

        assert!(report.direct_exec_available);
        assert!(!report.codex_hook.installed);
        assert_eq!(report.worker_probe_summary.status, "ready");
        assert_eq!(report.status_socket_consistent, Some(true));
        assert_eq!(report.dry_run_would_offload, Some(true));
        assert!(report.remote_only_safe);
        assert!(report.degraded.is_empty());
        assert_eq!(
            report.recovery,
            vec!["remote_only_cargo_allowed_from_this_shell".to_string()]
        );
    }

    #[test]
    fn rch_local_capability_fails_closed_for_startable_queued_builds() {
        let report = require_ok(
            parse_rch_local_capability_snapshot(
                r#"{
                    "schema":"ee.rch.local_capability.fixture.v1",
                    "remoteOnlyRequired":true,
                    "captures":{
                        "helpJson":{"commands":[{"name":"status"},{"name":"exec"}]},
                        "hookStatus":{"data":{"agents":[{"agent":"CodexCli","status":"Not installed"}]}},
                        "status":{
                            "data":{
                                "daemon":{
                                    "daemon":{
                                        "version":"0.1.3",
                                        "socket_path":"/tmp/rch.sock",
                                        "workers_healthy":3,
                                        "slots_available":8
                                    },
                                    "active_builds":[],
                                    "queued_builds":[
                                        {
                                            "id":200,
                                            "command":"env TMPDIR=/tmp cargo test --test cancellation_graph -- --nocapture",
                                            "slots_needed":8,
                                            "estimated_start":"2026-05-15T13:18:07Z"
                                        }
                                    ]
                                }
                            }
                        },
                        "config":{"data":{"general":{"socket_path":"/tmp/rch.sock"}}},
                        "workerProbe":{"data":{"summary":{"healthy":3,"failed":0}}},
                        "diagnose":{"data":{"dry_run":{"would_offload":true}}}
                    }
                }"#,
            ),
            "queued-start RCH capability fixture",
        );
        let queue = require_some(report.queue_health.as_ref(), "queue health");

        assert_eq!(queue.queued_count, 1);
        assert_eq!(queue.active_count, 0);
        assert_eq!(queue.slots_available, Some(8));
        assert_eq!(queue.status, "start_stalled");
        assert!(!report.remote_only_safe);
        assert!(report.degraded.iter().any(|degradation| {
            degradation.code == RCH_REMOTE_REQUIRED_FALLBACK_PREVENTED_CODE
                && degradation.message.contains("queued remote builds")
        }));
        assert!(
            report
                .recovery
                .contains(&"repair_rch_queue_scheduler_before_remote_cargo".to_string())
        );
    }

    #[test]
    fn rch_local_capability_fails_closed_for_capacity_blocked_queue() {
        let report = require_ok(
            parse_rch_local_capability_snapshot(
                r#"{
                    "schema":"ee.rch.local_capability.fixture.v1",
                    "remoteOnlyRequired":true,
                    "captures":{
                        "helpJson":{"commands":[{"name":"status"},{"name":"exec"}]},
                        "hookStatus":{"data":{"agents":[{"agent":"CodexCli","status":"Not installed"}]}},
                        "queue":{
                            "data":{
                                "active_builds":[
                                    {
                                        "id":31,
                                        "command":"env TMPDIR=/tmp cargo build --bin ee"
                                    }
                                ],
                                "queued_builds":[
                                    {
                                        "id":79,
                                        "command":"cargo test --lib health_robot_insights_respects_structural_health_feature_flag -- --nocapture",
                                        "slots_needed":4,
                                        "estimated_start":"2026-05-15T19:48:32Z"
                                    }
                                ],
                                "slots_available":2
                            }
                        },
                        "status":{"data":{"daemon":{"daemon":{"version":"1.0.24","socket_path":"/tmp/rch.sock","workers_healthy":3}}}},
                        "config":{"data":{"general":{"socket_path":"/tmp/rch.sock"}}},
                        "workerProbe":{"data":{"summary":{"healthy":3,"failed":0}}},
                        "diagnose":{"data":{"dry_run":{"would_offload":true}}}
                    }
                }"#,
            ),
            "capacity-blocked RCH queue fixture",
        );
        let queue = require_some(report.queue_health.as_ref(), "queue health");

        assert_eq!(queue.queued_count, 1);
        assert_eq!(queue.active_count, 1);
        assert_eq!(queue.slots_available, Some(2));
        assert_eq!(queue.status, "capacity_blocked");
        assert!(!report.remote_only_safe);
        assert!(report.degraded.iter().any(|degradation| {
            degradation.code == RCH_REMOTE_REQUIRED_FALLBACK_PREVENTED_CODE
                && degradation.message.contains("need more slots")
        }));
        assert!(
            report
                .recovery
                .contains(&"wait_for_rch_capacity_or_fail_fast_before_remote_cargo".to_string())
        );
    }

    #[test]
    fn collector_attaches_live_rch_capability_without_invoking_cargo() {
        let mut options = SwarmBriefCollectOptions::for_workspace(".");
        options.enabled_sources = [SwarmBriefSourceKind::Rch].into_iter().collect();
        options.include_rch = true;
        let runner = FakeRunner::default()
            .with_output(
                "rch",
                &["status", "--json"],
                r#"{
                    "data":{
                        "posture":"remote_ready",
                        "daemon":{
                            "daemon":{
                                "version":"0.1.3",
                                "socket_path":"/tmp/rch.sock",
                                "workers_healthy":2
                            }
                        }
                    }
                }"#,
            )
            .with_output(
                "rch",
                &["--help-json"],
                r#"{"version":"1.0.24","subcommands":[{"name":"exec"},{"name":"status"}]}"#,
            )
            .with_output(
                "rch",
                &["queue", "--json"],
                r#"{"data":{"active_builds":[],"queued_builds":[],"slots_available":2}}"#,
            )
            .with_output(
                "rch",
                &["agents", "status", "codex-cli", "--json"],
                r#"{"data":{"kind":"CodexCli","hook_status":"Not installed"}}"#,
            )
            .with_output(
                "rch",
                &["config", "show", "--json"],
                r#"{"data":{"general":{"socket_path":"/tmp/rch.sock"}}}"#,
            )
            .with_output(
                "rch",
                &["workers", "probe", "--all", "--json"],
                r#"{"data":{"summary":{"healthy":2,"failed":0},"results":[{"id":"csd","status":"ok"}]}}"#,
            )
            .with_output(
                "rch",
                &["diagnose", "--dry-run", "--json", "cargo", "check", "--lib"],
                r#"{"data":{"dry_run":{"would_offload":true}}}"#,
            );

        let report = collect_swarm_brief(&options, &runner);

        let capability = require_some(report.rch_local_capability.as_ref(), "rch local capability");
        assert!(capability.direct_exec_available);
        assert_eq!(capability.dry_run_would_offload, Some(true));
        assert!(capability.remote_only_safe);
        assert_eq!(
            source_status(&report, SwarmBriefSourceKind::Rch),
            Some(SwarmBriefSourceStatus::Ready)
        );
        assert!(
            runner
                .calls()
                .iter()
                .all(|call| !call.starts_with("cargo "))
        );
    }

    #[test]
    fn rch_local_capability_attaches_to_swarm_brief_fail_closed_advice() {
        let fixture =
            include_str!("../../tests/fixtures/swarm/rch_codex_capability_contradiction.json");
        let capability = require_ok(
            parse_rch_local_capability_snapshot(fixture),
            "valid rch capability fixture",
        );
        let mut report = report_with_ready_sources();

        attach_rch_local_capability(&mut report, capability);
        apply_swarm_brief_advice(&mut report);
        report.finalize();

        let local = require_some(
            report.rch_local_capability.as_ref(),
            "rch local capability block",
        );
        assert!(!local.remote_only_safe);
        assert_eq!(
            source_status(&report, SwarmBriefSourceKind::Rch),
            Some(SwarmBriefSourceStatus::Degraded)
        );
        assert!(
            report
                .degraded
                .iter()
                .any(|degradation| degradation.code == RCH_REMOTE_REQUIRED_FALLBACK_PREVENTED_CODE)
        );
        assert!(
            report
                .degraded
                .iter()
                .any(|degradation| degradation.code == RCH_WORKER_TOPOLOGY_BLOCKED_CODE)
        );

        let remote_required = recommendation(
            &report,
            "rec.degraded.rch.rch_remote_required_fallback_prevented",
        );
        assert!(
            remote_required
                .must_not_do
                .iter()
                .any(|item| item.contains("Do not unset RCH_REQUIRE_REMOTE"))
        );

        let worker_topology =
            recommendation(&report, "rec.degraded.rch.rch_worker_topology_blocked");
        assert!(
            worker_topology
                .must_not_do
                .iter()
                .any(|item| item.contains("topology-blocked RCH attempt"))
        );
    }

    #[test]
    fn rch_command_error_maps_e327_to_worker_topology_blocked() {
        let error = SwarmBriefCommandError::Failed {
            status: Some(1),
            stderr:
                "RCH-E327: worker=css path topology could not map /Users/project to /data/project"
                    .to_string(),
        };
        let degradation = rch_command_error_to_degradation(&error);

        assert_eq!(degradation.code, RCH_WORKER_TOPOLOGY_BLOCKED_CODE);
        assert_eq!(degradation.source, SwarmBriefSourceKind::Rch);
        assert!(degradation.message.contains("RCH-E327"));
        assert!(degradation.message.contains("selected worker: css"));
        assert!(degradation.message.contains("root metadata redacted"));
        assert!(!degradation.message.contains("/Users/project"));
        assert!(!degradation.message.contains("/data/project"));
        assert!(
            degradation
                .repair
                .as_deref()
                .is_some_and(|repair| repair.contains("worker path mapping"))
        );
    }

    #[test]
    fn rch_command_error_distinguishes_remote_required_fallback_prevented() {
        let error = SwarmBriefCommandError::Failed {
            status: Some(1),
            stderr: "RCH_REQUIRE_REMOTE is set; remote required fallback prevented local execution"
                .to_string(),
        };
        let degradation = rch_command_error_to_degradation(&error);

        assert_eq!(
            degradation.code,
            RCH_REMOTE_REQUIRED_FALLBACK_PREVENTED_CODE
        );
        assert_eq!(degradation.source, SwarmBriefSourceKind::Rch);
        assert!(degradation.message.contains("no valid remote evidence"));
    }

    #[test]
    fn advisor_blocks_rch_topology_degradation_from_closure_evidence() {
        let mut report = report_with_ready_sources();
        let Some(rch_snapshot) = report
            .sources
            .iter_mut()
            .find(|snapshot| snapshot.source == SwarmBriefSourceKind::Rch)
        else {
            panic!("rch source");
        };
        rch_snapshot.status = SwarmBriefSourceStatus::Unavailable;
        rch_snapshot.degraded = vec![SwarmBriefDegradation::warning(
            SwarmBriefSourceKind::Rch,
            RCH_WORKER_TOPOLOGY_BLOCKED_CODE,
            "RCH-E327 worker topology blocked remote-required verification; root metadata redacted.",
            Some("rch status --json".to_string()),
        )];

        apply_swarm_brief_advice(&mut report);

        let rec = recommendation(&report, "rec.degraded.rch.rch_worker_topology_blocked");
        assert!(
            rec.must_not_do.iter().any(|item| {
                item.contains("Do not close beads requiring remote Cargo evidence")
            })
        );
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
