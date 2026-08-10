//! Read-only coordination snapshot model for swarm preflight briefs.
//!
//! This module owns source collection, normalization, and deterministic advice.
//! Public CLI rendering is wired through `ee swarm brief`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::agent_detect::{AgentInventoryStatus, AgentStatusOptions, gather_agent_status};
use crate::core::beads_integrity::{
    BeadsIntegrityHealth, compose_integrity_report_from_br_doctor_json,
};
use crate::core::budget_delta_recommender::build_host_calibration_posture;
use crate::core::git_ahead::{
    GIT_AHEAD_LOG_FORMAT, GitAheadLogState, GitAheadSnapshot, summarize_git_ahead,
    summarize_git_ahead_with_log_state,
};
use crate::core::profile::{HostResourceProbeReport, recommend_operating_profile};
use crate::core::query_miss_cluster::{
    KNOWLEDGE_GAP_MIN_CLUSTER_MISSES, MissAuditObservation, cluster_repeated_misses,
};
use crate::core::singleflight::singleflight_posture_report;
use crate::core::support_bundle::{
    TOOLCHAIN_PROVENANCE_REDACTION_STATUS, TOOLCHAIN_PROVENANCE_SCHEMA_V1, ToolchainFreshness,
    ToolchainProvenanceOptions, ToolchainProvenanceReport, ToolchainSourceHint, ToolchainToolId,
    ToolchainToolKind, collect_toolchain_provenance_with_runner,
};
use crate::core::verify::{
    VerificationPostureAdvisoryCounts, VerificationPostureEvidenceHealth,
    VerificationPostureRecoveryAction, VerificationPostureReport, gather_verification_posture,
};
use crate::core::workspace::{
    WorkspaceHygieneOptions, WorkspaceHygieneSwarmBriefSummary,
    build_workspace_hygiene_swarm_brief_summary,
};
use crate::db::{DbConnection, StoredAuditEntry, audit_actions};
use crate::policy::redact_secret_like_content;

pub const SWARM_BRIEF_SCHEMA_V1: &str = "ee.swarm.brief.v1";
pub const SWARM_BRIEF_REDACTION_STATUS: &str = "paths_counts_subjects_only_no_content";
pub const SWARM_BRIEF_SUMMARY_SCHEMA_V1: &str = "ee.support_bundle.swarm_brief_summary.v1";
pub const SWARM_BRIEF_SUMMARY_REDACTION_STATUS: &str =
    "counts_hashes_codes_ids_only_no_mail_body_no_raw_queries_no_file_listings";
pub const SWARM_INCIDENT_SUMMARY_SCHEMA_V1: &str = "ee.support_bundle.swarm_incident_summary.v1";
pub const SWARM_INCIDENT_SUMMARY_REDACTION_STATUS: &str =
    "scenario_ids_status_counts_hashes_only_no_raw_logs_no_mail_bodies_no_commands_no_paths";
pub const SWARM_REPLAY_SUMMARY_SCHEMA_V1: &str = "ee.support_bundle.swarm_replay_summary.v1";
pub const SWARM_REPLAY_SUMMARY_REDACTION_STATUS: &str =
    "workload_run_ids_counts_hashes_only_no_raw_logs_no_commands_no_paths";
pub const SWARM_BRIEF_VERIFICATION_BROKER_SCHEMA_V1: &str =
    "ee.swarm.verification_broker_summary.v1";
pub const MAX_SWARM_INCIDENT_SUMMARY_BYTES: usize = 8192;
pub const MAX_SWARM_REPLAY_SUMMARY_BYTES: usize = 8192;

/// Cap on the byte size of the operator-supplied
/// `--agent-mail-snapshot` JSON file before refusing to read it.
///
/// Redacted Agent Mail snapshots carry paths, counts, and subjects
/// only (per the `paths_counts_subjects_only_no_content` redaction
/// invariant the bd-3nbbe contract pins); they are not full mailbox
/// dumps. 8 MiB is well above any reasonable redacted snapshot
/// (which sits in the kilobytes-to-low-megabytes range) while
/// bounding the allocation an accidentally-aimed-at-a-log-file or
/// adversarial path can demand. bd-1sdr5 / bd-1icct multi-pass-bug-
/// hunting audit pass.
pub const AGENT_MAIL_SNAPSHOT_MAX_BYTES: usize = 8 * 1024 * 1024;
const AGENT_MAIL_SNAPSHOT_SCHEMA_V1: &str = "ee.agent_mail.snapshot.v1";
const AGENT_MAIL_SNAPSHOT_V1_SOURCE_COUNT: usize = 6;
const AGENT_MAIL_SNAPSHOT_STALE_AFTER_SECONDS: u64 = 5 * 60;
const AGENT_MAIL_SNAPSHOT_MAX_FUTURE_SKEW_SECONDS: i64 = 60;

const GIT_UNAVAILABLE_CODE: &str = "git_unavailable";
const BEADS_UNAVAILABLE_CODE: &str = "beads_unavailable";
const BEADS_COMMAND_TIMEOUT_CODE: &str = "beads_command_timeout";
const BEADS_NO_OUTPUT_CODE: &str = "beads_no_output";
const BEADS_TRACKER_METADATA_DRIFT_CODE: &str = "beads_tracker_metadata_drift";
const BEADS_TRACKER_STALE_CODE: &str = "beads_tracker_stale";
const BEADS_READY_ARGS: [&str; 7] = [
    "ready",
    "--limit",
    "0",
    "--json",
    "--no-auto-import",
    "--no-auto-flush",
    "--allow-stale",
];
const BEADS_READY_COMMAND: &str =
    "br ready --limit 0 --json --no-auto-import --no-auto-flush --allow-stale";
const BV_COMMAND_TIMEOUT_CODE: &str = "bv_command_timeout";
const BV_NO_OUTPUT_CODE: &str = "bv_no_output";
const BV_UNAVAILABLE_CODE: &str = "bv_unavailable";
const AGENT_MAIL_UNAVAILABLE_CODE: &str = "agent_mail_unavailable";
const AGENT_MAIL_SEMANTIC_READINESS_FAILED_CODE: &str = "agent_mail_semantic_readiness_failed";
const AGENT_MAIL_HEALTH_PORT: u16 = 8765;
const AGENT_MAIL_HEALTH_PROBE_TIMEOUT_MS: u64 = 75;
pub const AGENT_MAIL_SNAPSHOT_TEMPLATE_AGENT: &str = "<AGENT_NAME>";
pub const AGENT_MAIL_SNAPSHOT_TEMPLATE_PATH: &str = "/private/tmp/ee-agent-mail-snapshot.json";
pub const AGENT_MAIL_SNAPSHOT_PRODUCER_COMMAND: &str = "scripts/agent_mail_snapshot.sh --project . --agent <AGENT_NAME> --json --output /private/tmp/ee-agent-mail-snapshot.json";
pub const DEFAULT_SWARM_SOURCE_COMMAND_TIMEOUT_MS: u64 = 35_000;
const MEMORY_DRIFT_REPORT_UNAVAILABLE_CODE: &str =
    super::memory_drift::MEMORY_DRIFT_REPORT_UNAVAILABLE_CODE;
const MEMORY_DRIFT_REPORT_UNAVAILABLE_MESSAGE_PREFIX: &str =
    "Memory drift report could not be collected read-only before evidence inspection";
const RCH_UNAVAILABLE_CODE: &str = "rch_unavailable";
const RCH_WORKER_TOPOLOGY_BLOCKED_CODE: &str = "rch_worker_topology_blocked";
const RCH_REMOTE_REQUIRED_FALLBACK_PREVENTED_CODE: &str = "rch_remote_required_fallback_prevented";
const RCH_POSTURE_REMOTE_READY: &str = "remote_ready";
const RCH_POSTURE_NO_REMOTE_WORKERS: &str = "no_remote_workers";
const RCH_POSTURE_WORKER_UNREACHABLE: &str = "worker_unreachable";
pub const RCH_WORKER_PRESSURE_SCHEMA_V1: &str = "ee.rch.worker_pressure.v1";
const AGENT_STATUS_UNAVAILABLE_CODE: &str = "agent_status_unavailable";
const MAX_SWARM_BRIEF_SUMMARY_RECOMMENDATIONS: usize = 6;
const MAX_SWARM_INCIDENT_SUMMARY_RECORDS: usize = 8;
const MAX_SWARM_INCIDENT_DEGRADED_CODES: usize = 8;
const MAX_SWARM_INCIDENT_RECOVERY_ACTIONS: usize = 4;
const MAX_SWARM_INCIDENT_ARTIFACT_REFS: usize = 8;
const MAX_SWARM_REPLAY_SUMMARY_RECORDS: usize = 8;
const MAX_SWARM_REPLAY_DEGRADED_CODES: usize = 12;
const MAX_SWARM_REPLAY_ARTIFACT_HASHES: usize = 12;
const MEMORY_DRIFT_SWARM_BRIEF_LIMIT: u32 = 16;
const SWARM_BRIEF_COMMAND_OUTPUT_LIMIT_BYTES: usize = 10 * 1024 * 1024;
const SWARM_BRIEF_COMMAND_PIPE_BUFFER_BYTES: usize = 8192;
const STALLED_BEAD_ACTIVE_WINDOW_SECONDS: i64 = 6 * 60 * 60;
const STALLED_BEAD_QUIET_WINDOW_SECONDS: i64 = 24 * 60 * 60;

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
            command_timeout_ms: DEFAULT_SWARM_SOURCE_COMMAND_TIMEOUT_MS,
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
        SwarmBriefSourceKind::MemoryDrift,
        SwarmBriefSourceKind::Toolchain,
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
        SwarmBriefSourceKind::MemoryDrift,
        SwarmBriefSourceKind::Rch,
        SwarmBriefSourceKind::Toolchain,
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
    #[serde(skip_serializing_if = "WorkspaceGitOperationState::is_clean")]
    pub git_operation_state: WorkspaceGitOperationState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_ahead: Option<GitAheadSnapshot>,
    pub beads: SwarmBriefBeadsSummary,
    pub bv: Option<SwarmBriefBvSummary>,
    pub file_reservations: Vec<SwarmBriefFileReservation>,
    pub file_surface_risks: Vec<SwarmBriefFileSurfaceRisk>,
    pub ready_reservation_pressure: Vec<SwarmBriefReadyReservationPressure>,
    pub stalled_bead_liveness: Vec<SwarmBriefStalledBeadLiveness>,
    /// The identity whose current, workspace-bound Agent Mail snapshot was
    /// collected. Internal-only so downstream claim gates can distinguish
    /// self-owned coordination evidence without changing the public brief
    /// schema or trusting legacy snapshots that lack strict identity proof.
    #[serde(skip)]
    pub agent_mail_agent_name: Option<String>,
    pub agent_mail_agents: Vec<SwarmBriefAgentMailAgent>,
    pub inbox: Vec<SwarmBriefInboxSummary>,
    pub threads: Vec<SwarmBriefThreadSummary>,
    pub resource_pressure: Vec<SwarmBriefResourcePressureHint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rch_local_capability: Option<RchLocalCapabilityReport>,
    pub host_profile: Option<SwarmBriefHostProfileSummary>,
    pub agent_inventory: Option<SwarmBriefAgentInventorySummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_drift: Option<SwarmBriefMemoryDriftSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toolchain_provenance: Option<SwarmBriefToolchainProvenanceSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_broker: Option<SwarmBriefVerificationBrokerSummary>,
    pub recommendations: Vec<SwarmBriefRecommendation>,
    pub degraded: Vec<SwarmBriefDegradation>,
    /// Compact workspace-hygiene posture (bd-1eq3l.6). `None` when the
    /// hygiene summary was not collected for this brief; the collector
    /// itself returns a `status="unavailable"` summary rather than `None`
    /// when the underlying report fails, so a `None` here means "skipped",
    /// not "broken".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_hygiene: Option<WorkspaceHygieneSwarmBriefSummary>,
    /// Knowledge-gap candidates: queries the swarm repeatedly searched and
    /// missed (bd-1n0np.6.4), surfaced from the query-miss audit log. Empty when
    /// the workspace DB is absent or no query hash crossed the repeat threshold.
    /// Advisory/read-only; the query text is redacted (6.3), so each gap is
    /// identified by its opaque hash + repeat count. Omitted from JSON when
    /// empty so briefs without repeated misses keep their existing shape.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub knowledge_gaps: Vec<SwarmBriefKnowledgeGap>,
}

/// Maximum query-miss audit rows scanned when assembling knowledge gaps. Bounds
/// the read on a hot append-only log; the repeat-threshold filter keeps the
/// surfaced set small regardless.
const SWARM_BRIEF_MISS_AUDIT_SCAN_LIMIT: u32 = 5_000;

/// A surfaced knowledge gap (bd-1n0np.6.4): a query hash the swarm repeatedly
/// searched and missed. The query text is redacted at the source (6.3), so the
/// gap is identified by its opaque hash, the repeat count, and the miss reasons.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefKnowledgeGap {
    pub query_hash: String,
    pub miss_count: u32,
    pub reasons: Vec<String>,
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
            git_operation_state: WorkspaceGitOperationState::default(),
            git_ahead: None,
            beads: SwarmBriefBeadsSummary::default(),
            bv: None,
            file_reservations: Vec::new(),
            file_surface_risks: Vec::new(),
            ready_reservation_pressure: Vec::new(),
            stalled_bead_liveness: Vec::new(),
            agent_mail_agent_name: None,
            agent_mail_agents: Vec::new(),
            inbox: Vec::new(),
            threads: Vec::new(),
            resource_pressure: Vec::new(),
            rch_local_capability: None,
            host_profile: None,
            agent_inventory: None,
            memory_drift: None,
            toolchain_provenance: None,
            verification_broker: None,
            recommendations: Vec::new(),
            degraded: Vec::new(),
            workspace_hygiene: None,
            knowledge_gaps: Vec::new(),
        }
    }

    pub fn finalize(&mut self) {
        self.sources.sort();
        self.sources
            .dedup_by(|left, right| left.source == right.source);
        self.dirty_files.sort();
        self.dirty_files.dedup();
        self.git_operation_state.operations.sort();
        self.git_operation_state.operations.dedup();
        self.git_operation_state.autostash_markers.sort();
        self.git_operation_state.autostash_markers.dedup();
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
        self.ready_reservation_pressure.sort();
        self.ready_reservation_pressure.dedup();
        self.stalled_bead_liveness.sort();
        self.stalled_bead_liveness
            .dedup_by(|left, right| left.bead_id == right.bead_id);
        let mut agent_mail_agents = BTreeMap::<String, SwarmBriefAgentMailAgent>::new();
        for agent in std::mem::take(&mut self.agent_mail_agents) {
            match agent_mail_agents.entry(agent.name.clone()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(agent);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    if agent.last_active_at > entry.get().last_active_at {
                        entry.insert(agent);
                    }
                }
            }
        }
        self.agent_mail_agents = agent_mail_agents.into_values().collect();
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
    MemoryDrift,
    Qos,
    Rch,
    Toolchain,
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
            Self::MemoryDrift => "memory_drift",
            Self::Qos => "qos",
            Self::Rch => "rch",
            Self::Toolchain => "toolchain",
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
        if degraded
            .iter()
            .any(SwarmBriefDegradation::affects_source_status)
            && self.status == SwarmBriefSourceStatus::Ready
        {
            self.status = SwarmBriefSourceStatus::Degraded;
        }
        self.degraded = degraded;
        self
    }

    /// Attach evidence findings without demoting a collector that completed
    /// authoritatively. The findings may still block the top-level claim gate;
    /// they do not mean the source itself was unavailable or partial.
    fn with_authoritative_findings(mut self, degraded: Vec<SwarmBriefDegradation>) -> Self {
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
    fn with_severity(
        source: SwarmBriefSourceKind,
        code: impl Into<String>,
        severity: &'static str,
        message: impl Into<String>,
        repair: impl Into<Option<String>>,
    ) -> Self {
        Self {
            code: code.into(),
            source,
            severity,
            message: redact_brief_text(&message.into()),
            repair: repair.into(),
        }
    }

    #[must_use]
    pub fn info(
        source: SwarmBriefSourceKind,
        code: impl Into<String>,
        message: impl Into<String>,
        repair: impl Into<Option<String>>,
    ) -> Self {
        Self {
            code: code.into(),
            source,
            severity: "info",
            message: redact_brief_text(&message.into()),
            repair: repair.into(),
        }
    }

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

    fn affects_source_status(&self) -> bool {
        self.severity != "info"
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Ord, PartialOrd)]
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
    #[serde(skip)]
    pub issue_type: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub latest_comment_at: Option<String>,
    pub comment_count: u64,
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
    pub action_hint: Option<String>,
    pub blocked_by: Vec<String>,
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
pub struct SwarmBriefReadyReservationPressure {
    pub bead_id: String,
    pub title: String,
    pub priority: Option<i64>,
    pub action: String,
    pub severity: String,
    pub likely_surfaces: Vec<String>,
    pub reservation_holders: Vec<String>,
    pub exclusive_reservation_count: usize,
    pub shared_reservation_count: usize,
    pub earliest_expires_at: Option<String>,
    pub max_risk_score: u16,
    pub risk_factors: Vec<String>,
    pub evidence: Vec<String>,
    pub suggested_commands: Vec<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefStalledBeadLiveness {
    pub bead_id: String,
    pub title: String,
    pub assignee: Option<String>,
    pub priority: Option<i64>,
    pub posture: String,
    pub action: String,
    pub severity: String,
    pub last_activity_at: Option<String>,
    pub age_seconds: Option<i64>,
    pub evidence_sources: Vec<String>,
    pub evidence: Vec<String>,
    pub suggested_commands: Vec<String>,
    pub must_not_do: Vec<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefAgentMailAgent {
    pub name: String,
    pub last_active_at: Option<String>,
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
    /// Present only for a strictly validated `ee.agent_mail.snapshot.v1`.
    /// Legacy coordination snapshots remain readable but cannot supply an
    /// authoritative self identity to claim-gate consumers.
    #[serde(skip)]
    pub agent_name: Option<String>,
    pub file_reservations: Vec<SwarmBriefFileReservation>,
    pub agents: Vec<SwarmBriefAgentMailAgent>,
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
    pub host_class: String,
    pub calibration_freshness: String,
    pub target_dir_posture: String,
    pub topology_warnings: Vec<String>,
    pub repair_action_kinds: Vec<String>,
    pub budget_delta_count: usize,
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

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefMemoryDriftSummary {
    pub status: String,
    pub report_mode: String,
    pub total_memories: u32,
    pub current_count: u32,
    pub changed_count: u32,
    pub missing_source_count: u32,
    pub stale_anchor_count: u32,
    pub unverifiable_count: u32,
    pub suppressed_count: u32,
    pub affected_count: u32,
    pub top_affected_memory_ids: Vec<String>,
    pub degraded_codes: Vec<String>,
    pub source_kind_counts: BTreeMap<String, u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefToolchainProvenanceSummary {
    pub schema: String,
    pub redaction_status: String,
    pub workspace_fingerprint: String,
    pub tool_count: usize,
    pub script_hash_count: usize,
    pub critical_blocker_count: usize,
    pub advisory_unknown_count: usize,
    pub tools: Vec<SwarmBriefToolchainToolSummary>,
    pub script_hashes: Vec<SwarmBriefToolchainScriptSummary>,
    pub degraded_codes: Vec<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefToolchainToolSummary {
    pub tool: &'static str,
    pub kind: &'static str,
    pub state: &'static str,
    pub critical: bool,
    pub version: Option<String>,
    pub binary_hash_preview: Option<String>,
    pub source_hint: &'static str,
    pub source_command_id: String,
    pub exit_class: &'static str,
    pub repair: Option<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefToolchainScriptSummary {
    pub script: String,
    pub blake3_preview: String,
    pub tracked: bool,
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
    pub queue_head_slots_needed: Option<u64>,
    pub active_build_max_age_seconds: Option<u64>,
    pub status: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchWorkerPressureReport {
    pub schema: &'static str,
    pub status: String,
    pub worker_count: u64,
    pub usable_worker_count: u64,
    pub blocked_worker_count: u64,
    pub stale_worker_count: u64,
    pub unknown_worker_count: u64,
    pub workers: Vec<RchWorkerPressureObservation>,
}

impl RchWorkerPressureReport {
    #[must_use]
    pub fn not_collected() -> Self {
        Self {
            schema: RCH_WORKER_PRESSURE_SCHEMA_V1,
            status: "not_collected".to_string(),
            worker_count: 0,
            usable_worker_count: 0,
            blocked_worker_count: 0,
            stale_worker_count: 0,
            unknown_worker_count: 0,
            workers: Vec::new(),
        }
    }

    #[must_use]
    pub fn pressure_unknown() -> Self {
        Self {
            schema: RCH_WORKER_PRESSURE_SCHEMA_V1,
            status: "pressure_unknown".to_string(),
            worker_count: 0,
            usable_worker_count: 0,
            blocked_worker_count: 0,
            stale_worker_count: 0,
            unknown_worker_count: 0,
            workers: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchWorkerPressureObservation {
    pub worker_id: String,
    pub pressure_state: String,
    pub confidence: String,
    pub reason_code: String,
    pub free_gb: Option<u64>,
    pub free_ratio_bps: Option<u64>,
    pub telemetry_freshness: String,
    pub admission_impact: String,
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
    pub worker_pressure: RchWorkerPressureReport,
    pub remote_only_required: bool,
    pub remote_only_safe: bool,
    pub degraded: Vec<SwarmBriefDegradation>,
    pub recovery: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefVerificationBrokerSummary {
    pub schema: &'static str,
    pub source_schema: String,
    pub status: String,
    pub record_count: u32,
    pub recent_run_count: u32,
    pub stale_run_count: u32,
    pub unknown_age_count: u32,
    pub recent_reusable_run_count: u32,
    pub in_flight_equivalent_command_count: u32,
    pub advisory_counts: VerificationPostureAdvisoryCounts,
    pub evidence_health: VerificationPostureEvidenceHealth,
    pub recovery_actions: Vec<VerificationPostureRecoveryAction>,
    pub rch_queue_status: String,
    pub rch_slots_available: Option<u64>,
    pub rch_queue_head_slots_needed: Option<u64>,
    pub rch_worker_pressure_status: String,
    pub rch_usable_worker_count: u64,
    pub rch_blocked_worker_count: u64,
    pub raw_logs_included: bool,
    pub raw_mail_bodies_included: bool,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceGitSnapshotOptions {
    pub workspace: PathBuf,
    pub command_timeout_ms: u64,
    pub large_file_threshold_bytes: u64,
}

impl WorkspaceGitSnapshotOptions {
    #[must_use]
    pub fn for_workspace(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            command_timeout_ms: DEFAULT_SWARM_SOURCE_COMMAND_TIMEOUT_MS,
            large_file_threshold_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceGitSnapshot {
    pub repository_root: String,
    pub entries: Vec<WorkspaceGitStatusEntry>,
    #[serde(skip_serializing_if = "WorkspaceGitOperationState::is_clean")]
    pub operation_state: WorkspaceGitOperationState,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceGitStatusEntry {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_path: Option<String>,
    pub staged: String,
    pub unstaged: String,
    pub entry_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submodule_state: Option<WorkspaceGitSubmoduleState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<WorkspaceGitPathMetadata>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceGitSubmoduleState {
    pub raw: String,
    pub commit_changed: bool,
    pub tracked_changes: bool,
    pub untracked_changes: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceGitPathMetadata {
    pub exists: bool,
    pub file_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    pub large_file: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceGitOperationState {
    pub in_progress: bool,
    pub operations: Vec<WorkspaceGitOperationMarker>,
    pub autostash_markers: Vec<WorkspaceGitOperationMarker>,
}

impl WorkspaceGitOperationState {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        !self.in_progress && self.operations.is_empty() && self.autostash_markers.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceGitOperationMarker {
    pub operation: &'static str,
    pub marker_path: &'static str,
    pub marker_type: String,
}

/// Error returned by a read-only command runner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SwarmBriefCommandError {
    Unavailable(String),
    Failed {
        status: Option<i32>,
        stdout: String,
        stderr: String,
    },
    TimedOut {
        timeout_ms: u64,
    },
    InvalidUtf8(String),
}

impl SwarmBriefCommandError {
    fn to_degradation(
        &self,
        source: SwarmBriefSourceKind,
        code: &'static str,
        repair: impl Into<String>,
    ) -> SwarmBriefDegradation {
        let message = match self {
            Self::Unavailable(message) => message.clone(),
            Self::Failed { status, stderr, .. } => {
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
        SwarmBriefDegradation::warning(source, code, message, Some(repair.into()))
    }
}

pub fn collect_workspace_git_snapshot(
    options: &WorkspaceGitSnapshotOptions,
    runner: &impl SwarmBriefCommandRunner,
) -> Result<WorkspaceGitSnapshot, SwarmBriefCommandError> {
    let root_output = runner.run(
        "git",
        &["rev-parse", "--show-toplevel"],
        &options.workspace,
        options.command_timeout_ms,
    )?;
    let repository_root = root_output
        .stdout
        .lines()
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .ok_or_else(|| {
            SwarmBriefCommandError::Unavailable(
                "git rev-parse --show-toplevel returned no repository root".to_string(),
            )
        })?;
    let repository_root_path = PathBuf::from(repository_root);

    let status_output = runner.run(
        "git",
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=all",
        ],
        &repository_root_path,
        options.command_timeout_ms,
    )?;
    let mut entries = parse_workspace_git_status_porcelain_v2(&status_output.stdout);
    attach_workspace_git_metadata(
        &mut entries,
        &repository_root_path,
        options.large_file_threshold_bytes,
    );
    entries.sort();
    entries.dedup();

    Ok(WorkspaceGitSnapshot {
        repository_root: redact_path_label(&repository_root_path),
        entries,
        operation_state: collect_workspace_git_operation_state(&repository_root_path),
    })
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
            read_swarm_brief_pipe_limited(
                &mut stdout_handle,
                SWARM_BRIEF_COMMAND_OUTPUT_LIMIT_BYTES,
            )
        });

        let stderr_thread = thread::spawn(move || {
            read_swarm_brief_pipe_limited(
                &mut stderr_handle,
                SWARM_BRIEF_COMMAND_OUTPUT_LIMIT_BYTES,
            )
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

        let stdout_bytes = join_swarm_brief_pipe_reader(stdout_thread, "stdout")?;
        let stderr_bytes = join_swarm_brief_pipe_reader(stderr_thread, "stderr")?;

        let stdout = String::from_utf8(stdout_bytes)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
        let stderr = String::from_utf8(stderr_bytes)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());

        if status.success() {
            Ok(SwarmBriefCommandOutput { stdout, stderr })
        } else {
            Err(SwarmBriefCommandError::Failed {
                status: status.code(),
                stdout,
                stderr,
            })
        }
    }
}

/// Bridge `SwarmBriefCommandRunner` calls onto the unified
/// `source_run::run_source_command` watchdog (bd-12v87.3).
///
/// This is the harness seam that lets swarm-brief / work-packet / doctor
/// source collectors share the same bounded-subprocess machinery as the
/// rest of `ee` instead of reinventing timeout + pipe-drain logic each
/// place. Wiring the existing call sites onto this adapter is deferred to
/// follow-up integration slices; the seam exists so those slices can land
/// behind a single API change rather than scattering source-run plumbing
/// across every collector.
///
/// `kind` labels every spawned subprocess with its caller class so the
/// emitted `SourceRunEvidence` carries enough provenance for the watchdog
/// to attribute hangs and degraded outcomes back to the source family
/// (Beads, BV, Agent Mail, CASS, RCH, Git, ...).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceRunSwarmBriefRunner {
    kind: crate::core::source_run::SourceRunKind,
}

impl SourceRunSwarmBriefRunner {
    #[must_use]
    pub const fn new(kind: crate::core::source_run::SourceRunKind) -> Self {
        Self { kind }
    }

    fn build_request(
        &self,
        program: &str,
        args: &[&str],
        cwd: &Path,
        timeout_ms: u64,
    ) -> crate::core::source_run::SourceRunRequest {
        let command = crate::core::source_run::SourceRunCommand::new(program)
            .with_args(args.iter().map(|arg| (*arg).to_string()))
            .with_cwd(cwd.to_path_buf());
        let source = crate::core::source_run::SourceRunSource::new(
            self.kind,
            program.to_string(),
            "swarm_brief_command".to_string(),
        );
        // Match `SystemSwarmBriefCommandRunner`'s 10 MiB cap so the
        // adapter is a behavior-compatible drop-in for the existing
        // collectors. Without raising `tail_bytes_max`, source_run's
        // default 8 KiB tail would silently truncate long `git log`
        // / `br ready` outputs that the consuming parsers expect to
        // see in full.
        crate::core::source_run::SourceRunRequest::new(
            source,
            command,
            Duration::from_millis(timeout_ms.max(1)),
        )
        .with_tail_bytes_max(SWARM_BRIEF_COMMAND_OUTPUT_LIMIT_BYTES)
    }
}

/// Translate a `SourceRunEvidence` into the legacy
/// `Result<SwarmBriefCommandOutput, SwarmBriefCommandError>` shape every
/// swarm-brief source adapter already consumes. Capped output tails carry
/// the same semantics as `SystemSwarmBriefCommandRunner` (truncate-and-
/// keep) so consumers do not need to learn a new partial-read contract.
fn translate_source_run_evidence(
    evidence: crate::core::source_run::SourceRunEvidence,
) -> Result<SwarmBriefCommandOutput, SwarmBriefCommandError> {
    use crate::core::source_run::SourceRunStatus;
    let stdout = evidence.output.stdout_tail.clone().unwrap_or_default();
    let stderr = evidence.output.stderr_tail.clone().unwrap_or_default();
    match evidence.status {
        SourceRunStatus::Passed => Ok(SwarmBriefCommandOutput { stdout, stderr }),
        SourceRunStatus::Failed => Err(SwarmBriefCommandError::Failed {
            status: evidence.exit.exit_code,
            stdout,
            stderr,
        }),
        SourceRunStatus::TimedOut => Err(SwarmBriefCommandError::TimedOut {
            timeout_ms: evidence.timing.timeout_ms,
        }),
        SourceRunStatus::SpawnFailed => {
            let detail = stderr.trim();
            let message = if detail.is_empty() {
                format!("{} spawn failed", evidence.source.source_id)
            } else {
                format!("{} spawn failed: {detail}", evidence.source.source_id)
            };
            Err(SwarmBriefCommandError::Unavailable(message))
        }
        SourceRunStatus::ParseFailed
        | SourceRunStatus::StaleSource
        | SourceRunStatus::MalformedStore
        | SourceRunStatus::Blocked => Err(SwarmBriefCommandError::Unavailable(format!(
            "{} returned status {}",
            evidence.source.source_id,
            evidence.status.as_str()
        ))),
    }
}

impl SwarmBriefCommandRunner for SourceRunSwarmBriefRunner {
    fn run(
        &self,
        program: &str,
        args: &[&str],
        cwd: &Path,
        timeout_ms: u64,
    ) -> Result<SwarmBriefCommandOutput, SwarmBriefCommandError> {
        let request = self.build_request(program, args, cwd, timeout_ms);
        let evidence = crate::core::source_run::run_source_command(&request);
        translate_source_run_evidence(evidence)
    }
}

fn join_swarm_brief_pipe_reader(
    handle: thread::JoinHandle<io::Result<Vec<u8>>>,
    stream_name: &str,
) -> Result<Vec<u8>, SwarmBriefCommandError> {
    match handle.join() {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(SwarmBriefCommandError::Unavailable(format!(
            "source command {stream_name} pipe read failed: {error}"
        ))),
        Err(_panic) => Err(SwarmBriefCommandError::Unavailable(format!(
            "source command {stream_name} reader thread panicked"
        ))),
    }
}

fn read_swarm_brief_pipe_limited<R: io::Read>(reader: &mut R, limit: usize) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; SWARM_BRIEF_COMMAND_PIPE_BUFFER_BYTES];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let remaining = limit.saturating_sub(output.len());
        if remaining > 0 {
            output.extend_from_slice(&buffer[..read.min(remaining)]);
        }
    }
    Ok(output)
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
        operation_state: WorkspaceGitOperationState,
        git_ahead: Option<GitAheadSnapshot>,
    },
    Beads(SwarmBriefBeadsSummary),
    Bv(SwarmBriefBvSummary),
    AgentMail {
        agent_name: Option<String>,
        file_reservations: Vec<SwarmBriefFileReservation>,
        agents: Vec<SwarmBriefAgentMailAgent>,
        inbox: Vec<SwarmBriefInboxSummary>,
        threads: Vec<SwarmBriefThreadSummary>,
    },
    Rch {
        resource_pressure: Vec<SwarmBriefResourcePressureHint>,
        local_capability: Option<RchLocalCapabilityReport>,
    },
    HostProfile(SwarmBriefHostProfileSummary),
    AgentInventory(SwarmBriefAgentInventorySummary),
    MemoryDrift(SwarmBriefMemoryDriftSummary),
    Toolchain(SwarmBriefToolchainProvenanceSummary),
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
        let operation_state = collect_workspace_git_operation_state(&options.workspace);
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
        let git_ahead = collect_git_ahead_snapshot(self.runner, options, &mut degraded);

        let item_count = dirty_files.len()
            + recent_commits.len()
            + operation_state.operations.len()
            + operation_state.autostash_markers.len()
            + git_ahead
                .as_ref()
                .map_or(0, |snapshot| 1 + snapshot.commits.len());
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
                operation_state,
                git_ahead,
            },
        }
    }
}

fn collect_git_ahead_snapshot<R: SwarmBriefCommandRunner>(
    runner: &R,
    options: &SwarmBriefCollectOptions,
    degraded: &mut Vec<SwarmBriefDegradation>,
) -> Option<GitAheadSnapshot> {
    let status_args = ["status", "--porcelain=v2", "--branch"];
    let status = match runner.run(
        "git",
        &status_args,
        &options.workspace,
        options.command_timeout_ms,
    ) {
        Ok(output) => output,
        Err(error) => {
            degraded.push(error.to_degradation(
                SwarmBriefSourceKind::Git,
                GIT_UNAVAILABLE_CODE,
                "Run `git status --porcelain=v2 --branch` in the workspace.",
            ));
            return None;
        }
    };

    let status_only = summarize_git_ahead(&status.stdout, Some(""));
    let snapshot = match (status_only.ahead_count, status_only.upstream_ref.as_deref()) {
        (0, _) | (_, None) => status_only,
        (_, Some(upstream)) => {
            let range = format!("{upstream}..HEAD");
            let format_arg = format!("--format={GIT_AHEAD_LOG_FORMAT}");
            let args = ["log".to_string(), range, format_arg];
            let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            match runner.run(
                "git",
                &arg_refs,
                &options.workspace,
                options.command_timeout_ms,
            ) {
                Ok(output) => summarize_git_ahead(&status.stdout, Some(&output.stdout)),
                Err(SwarmBriefCommandError::TimedOut { .. }) => {
                    summarize_git_ahead_with_log_state(&status.stdout, GitAheadLogState::TimedOut)
                }
                Err(SwarmBriefCommandError::Failed { .. }) => {
                    summarize_git_ahead_with_log_state(&status.stdout, GitAheadLogState::Failed)
                }
                Err(
                    SwarmBriefCommandError::Unavailable(_) | SwarmBriefCommandError::InvalidUtf8(_),
                ) => summarize_git_ahead_with_log_state(
                    &status.stdout,
                    GitAheadLogState::Unavailable,
                ),
            }
        }
    };

    degraded.extend(snapshot.degraded.iter().map(|entry| {
        SwarmBriefDegradation::warning(
            SwarmBriefSourceKind::Git,
            entry.code,
            entry.message,
            Some(entry.repair.to_string()),
        )
    }));

    Some(snapshot)
}

pub struct BeadsSourceAdapter<'a, R> {
    pub runner: &'a R,
}

impl<R: SwarmBriefCommandRunner> SwarmBriefSourceAdapter for BeadsSourceAdapter<'_, R> {
    fn collect(&self, options: &SwarmBriefCollectOptions) -> SwarmBriefSourceOutput {
        let source = SwarmBriefSourceKind::Beads;
        let provenance = SwarmBriefSourceProvenance::command("br", &BEADS_READY_ARGS);
        let mut freshness = SwarmBriefSourceFreshness::current();
        let mut degraded = collect_beads_freshness(self.runner, options, &mut freshness);
        let mut bucket_degraded = Vec::new();

        let ready = collect_beads_bucket(
            self.runner,
            options,
            &BEADS_READY_ARGS,
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
        Ok(output) if output.stdout.trim().is_empty() => {
            degraded.push(beads_no_output_degradation("br dep cycles --json"));
            None
        }
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
            degraded.push(beads_command_error_to_degradation(
                &error,
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
        Ok(output) if output.stdout.trim().is_empty() => {
            vec![beads_no_output_degradation(
                "br sync --status --json --no-auto-import --allow-stale",
            )]
        }
        Ok(output) => match parse_beads_sync_status_json(&output.stdout) {
            Ok(status) if status.jsonl_newer || status.db_newer => {
                if beads_sync_status_is_metadata_only_drift(runner, options, &status) {
                    vec![beads_tracker_metadata_drift_degradation()]
                } else {
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
            }
            Ok(_) => Vec::new(),
            Err(message) => vec![SwarmBriefDegradation::warning(
                SwarmBriefSourceKind::Beads,
                BEADS_UNAVAILABLE_CODE,
                message,
                Some("br sync --status --json --no-auto-import --allow-stale".to_string()),
            )],
        },
        Err(error) => vec![beads_command_error_to_degradation(
            &error,
            "br sync --status --json --no-auto-import --allow-stale",
        )],
    }
}

fn beads_sync_status_is_metadata_only_drift<R: SwarmBriefCommandRunner>(
    runner: &R,
    options: &SwarmBriefCollectOptions,
    status: &BeadsSyncStatus,
) -> bool {
    if !(status.jsonl_newer && !status.db_newer && status.dirty_count == Some(0)) {
        return false;
    }
    let args = ["doctor", "--json", "--no-db"];
    let Ok(output) = runner.run("br", &args, &options.workspace, options.command_timeout_ms) else {
        return false;
    };
    compose_integrity_report_from_br_doctor_json(
        &output.stdout,
        ".beads/issues.jsonl",
        ".beads/beads.db",
        true,
    )
    .is_ok_and(|report| {
        report.health == BeadsIntegrityHealth::ExternalChangesPendingImport
            && report.pending_import_count == 0
            && report.dirty_issue_count == 0
            && report.jsonl_parse_error.is_none()
            && report.jsonl_record_count == report.db_record_count
    })
}

fn beads_tracker_metadata_drift_degradation() -> SwarmBriefDegradation {
    SwarmBriefDegradation::warning(
        SwarmBriefSourceKind::Beads,
        BEADS_TRACKER_METADATA_DRIFT_CODE,
        "Beads sync metadata reports JSONL freshness drift, but br doctor reports DB/JSONL content parity and zero dirty issues; br reads are advisory until import-only sync reconciles metadata.",
        Some("br sync --import-only --json".to_string()),
    )
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
        Ok(output) if output.stdout.trim().is_empty() => {
            degraded.push(beads_no_output_degradation(beads_command_repair(args)));
            Vec::new()
        }
        Ok(output) => parse_beads_json(&output.stdout, bucket).unwrap_or_else(|message| {
            degraded.push(SwarmBriefDegradation::warning(
                SwarmBriefSourceKind::Beads,
                BEADS_UNAVAILABLE_CODE,
                message,
                Some(beads_command_repair(args)),
            ));
            Vec::new()
        }),
        Err(error) => {
            degraded.push(beads_command_error_to_degradation(
                &error,
                beads_command_repair(args),
            ));
            Vec::new()
        }
    }
}

fn beads_command_error_to_degradation(
    error: &SwarmBriefCommandError,
    repair: impl Into<String>,
) -> SwarmBriefDegradation {
    let repair = repair.into();
    match error {
        SwarmBriefCommandError::TimedOut { timeout_ms } => SwarmBriefDegradation::warning(
            SwarmBriefSourceKind::Beads,
            BEADS_COMMAND_TIMEOUT_CODE,
            format!(
                "Beads source command timed out after {timeout_ms} ms; stale-safe fallback rows are advisory only."
            ),
            Some(repair),
        ),
        _ => error.to_degradation(SwarmBriefSourceKind::Beads, BEADS_UNAVAILABLE_CODE, repair),
    }
}

fn beads_no_output_degradation(repair: impl Into<String>) -> SwarmBriefDegradation {
    SwarmBriefDegradation::warning(
        SwarmBriefSourceKind::Beads,
        BEADS_NO_OUTPUT_CODE,
        "Beads source command returned no output; stale-safe fallback rows are advisory only."
            .to_owned(),
        Some(repair.into()),
    )
}

fn beads_command_repair(args: &[&str]) -> String {
    format!("br {}", args.join(" "))
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
            Ok(output) if output.stdout.trim().is_empty() => {
                let degradation =
                    bv_no_output_degradation("bv --robot-triage --robot-triage-by-track");
                SwarmBriefSourceOutput {
                    snapshot: SwarmBriefSourceSnapshot::unavailable(
                        SwarmBriefSourceKind::Bv,
                        provenance,
                        degradation,
                    ),
                    contribution: SwarmBriefContribution::None,
                }
            }
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
                let degradation = bv_command_error_to_degradation(
                    &error,
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

fn bv_command_error_to_degradation(
    error: &SwarmBriefCommandError,
    repair: impl Into<String>,
) -> SwarmBriefDegradation {
    let repair = repair.into();
    match error {
        SwarmBriefCommandError::TimedOut { timeout_ms } => SwarmBriefDegradation::warning(
            SwarmBriefSourceKind::Bv,
            BV_COMMAND_TIMEOUT_CODE,
            format!(
                "BV robot source command timed out after {timeout_ms} ms; use bounded retry or stale-safe Beads fallback instead of waiting indefinitely."
            ),
            Some(bv_bounded_retry_repair(&repair)),
        ),
        _ => error.to_degradation(SwarmBriefSourceKind::Bv, BV_UNAVAILABLE_CODE, repair),
    }
}

fn bv_no_output_degradation(repair: impl Into<String>) -> SwarmBriefDegradation {
    let repair = repair.into();
    SwarmBriefDegradation::warning(
        SwarmBriefSourceKind::Bv,
        BV_NO_OUTPUT_CODE,
        "BV robot source command returned no output; use bounded retry or stale-safe Beads fallback instead of waiting indefinitely."
            .to_owned(),
        Some(bv_bounded_retry_repair(&repair)),
    )
}

fn bv_bounded_retry_repair(bv_command: &str) -> String {
    format!(
        "Retry `{bv_command}` with the configured command timeout, or fall back to `{BEADS_READY_COMMAND}`."
    )
}

pub struct AgentMailSnapshotFileAdapter;

impl SwarmBriefSourceAdapter for AgentMailSnapshotFileAdapter {
    fn collect(&self, options: &SwarmBriefCollectOptions) -> SwarmBriefSourceOutput {
        let provenance = SwarmBriefSourceProvenance::local_probe();
        let Some(path) = &options.agent_mail_snapshot_path else {
            let (message, repair) =
                agent_mail_missing_snapshot_degradation_text(probe_agent_mail_health_endpoint());
            let degradation = SwarmBriefDegradation::warning(
                SwarmBriefSourceKind::AgentMail,
                AGENT_MAIL_UNAVAILABLE_CODE,
                message,
                Some(repair),
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

        match read_agent_mail_snapshot_file(path) {
            Ok(contents) => match parse_agent_mail_snapshot_json(&contents) {
                Ok(mut snapshot) => {
                    let decision_now = Utc::now();
                    match validate_agent_mail_snapshot_workspace_binding(
                        &contents,
                        &options.workspace,
                    )
                    .and_then(|()| {
                        agent_mail_snapshot_freshness_assessment(&contents, decision_now)
                    }) {
                        Ok((freshness, freshness_degradation)) => {
                            snapshot.file_reservations.retain(|reservation| {
                                reservation_is_active(reservation, decision_now.timestamp())
                            });
                            let agent_name = if freshness.state == "current" {
                                snapshot.agent_name.take()
                            } else {
                                None
                            };
                            let item_count = snapshot.file_reservations.len()
                                + snapshot.agents.len()
                                + snapshot.inbox.len()
                                + snapshot.threads.len();
                            let mut degraded = snapshot.degraded.clone();
                            degraded.extend(freshness_degradation);
                            SwarmBriefSourceOutput {
                                snapshot: SwarmBriefSourceSnapshot::ready(
                                    SwarmBriefSourceKind::AgentMail,
                                    provenance,
                                    item_count,
                                )
                                .with_degraded(degraded)
                                .with_freshness(freshness),
                                contribution: SwarmBriefContribution::AgentMail {
                                    agent_name,
                                    file_reservations: snapshot.file_reservations,
                                    agents: snapshot.agents,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AgentMailHealthProbe {
    Reachable,
    Unreachable,
}

fn probe_agent_mail_health_endpoint() -> AgentMailHealthProbe {
    let timeout = Duration::from_millis(AGENT_MAIL_HEALTH_PROBE_TIMEOUT_MS);
    let addr = SocketAddr::from(([127, 0, 0, 1], AGENT_MAIL_HEALTH_PORT));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, timeout) else {
        return AgentMailHealthProbe::Unreachable;
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    if stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1:8765\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return AgentMailHealthProbe::Unreachable;
    }
    let mut response_prefix = [0_u8; 16];
    match stream.read(&mut response_prefix) {
        Ok(count) if response_prefix[..count].starts_with(b"HTTP/") => {
            AgentMailHealthProbe::Reachable
        }
        _ => AgentMailHealthProbe::Unreachable,
    }
}

fn agent_mail_missing_snapshot_degradation_text(
    probe: AgentMailHealthProbe,
) -> (&'static str, String) {
    let producer = agent_mail_snapshot_producer_command_template();
    let retry = agent_mail_snapshot_brief_retry_command_template();
    match probe {
        AgentMailHealthProbe::Reachable => (
            "No redacted Agent Mail snapshot path was configured; the local Agent Mail health endpoint at 127.0.0.1:8765 is reachable, but ee swarm brief only consumes explicit redacted snapshots.",
            format!(
                "Generate a read-only redacted Agent Mail snapshot with `{producer}`, then retry with `{retry}`; live MCP tools remain external to ee."
            ),
        ),
        AgentMailHealthProbe::Unreachable => (
            "No redacted Agent Mail snapshot path was configured, and the local Agent Mail health endpoint at 127.0.0.1:8765 was not reachable within the brief probe budget.",
            format!(
                "Start or repair Agent Mail, then generate a read-only redacted Agent Mail snapshot with `{producer}` and retry with `{retry}`."
            ),
        ),
    }
}

#[must_use]
pub const fn agent_mail_snapshot_producer_command_template() -> &'static str {
    AGENT_MAIL_SNAPSHOT_PRODUCER_COMMAND
}

#[must_use]
pub fn agent_mail_snapshot_brief_retry_command_template() -> String {
    format!(
        "ee swarm brief --workspace . --agent-mail-snapshot {AGENT_MAIL_SNAPSHOT_TEMPLATE_PATH} --json"
    )
}

fn read_agent_mail_snapshot_file(path: &Path) -> io::Result<String> {
    if let Some(symlink) = first_existing_snapshot_symlink_component(path)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing to read Agent Mail snapshot through symlink '{}'",
                symlink.display()
            ),
        ));
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "Agent Mail snapshot path '{}' is not a file",
                path.display()
            ),
        ));
    }
    if metadata.len() > AGENT_MAIL_SNAPSHOT_MAX_BYTES as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Agent Mail snapshot '{}' exceeds the {AGENT_MAIL_SNAPSHOT_MAX_BYTES}-byte cap; refusing to read",
                path.display()
            ),
        ));
    }
    // bd-1sdr5: bounded read. Open + take(LIMIT+1) so an oversized
    // file reads LIMIT+1 bytes and we detect the overrun without
    // ever materializing more than ~8 MiB. Mirrors the
    // DEMO_OUTPUT_VERIFY_MAX_BYTES posture in src/cli/mod.rs (commit
    // 06a53349) so the two adversarial-path read sites share a
    // single defensive pattern.
    let read_limit = AGENT_MAIL_SNAPSHOT_MAX_BYTES
        .checked_add(1)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Agent Mail snapshot read cap overflowed usize",
            )
        })?;
    let file = open_agent_mail_snapshot_file_for_read_no_follow(path)?;
    let opened_metadata = file.metadata()?;
    if !opened_metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "Agent Mail snapshot path '{}' is not a regular file after open",
                path.display()
            ),
        ));
    }
    if opened_metadata.len() > AGENT_MAIL_SNAPSHOT_MAX_BYTES as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Agent Mail snapshot '{}' exceeds the {AGENT_MAIL_SNAPSHOT_MAX_BYTES}-byte cap; refusing to read",
                path.display()
            ),
        ));
    }
    let mut bytes = Vec::new();
    file.take(read_limit as u64).read_to_end(&mut bytes)?;
    if bytes.len() > AGENT_MAIL_SNAPSHOT_MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Agent Mail snapshot '{}' exceeds the {AGENT_MAIL_SNAPSHOT_MAX_BYTES}-byte cap; refusing to read",
                path.display()
            ),
        ));
    }
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn open_agent_mail_snapshot_file_for_read_no_follow(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    configure_agent_mail_snapshot_file_open_no_follow(&mut options);
    options.open(path)
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn configure_agent_mail_snapshot_file_open_no_follow(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn configure_agent_mail_snapshot_file_open_no_follow(_options: &mut fs::OpenOptions) {}

fn first_existing_snapshot_symlink_component(path: &Path) -> io::Result<Option<PathBuf>> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                current.push(component.as_os_str());
                continue;
            }
            std::path::Component::CurDir => continue,
            std::path::Component::ParentDir | std::path::Component::Normal(_) => {
                current.push(component.as_os_str());
            }
        }

        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(Some(current)),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    Ok(None)
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
        let host_calibration = build_host_calibration_posture(&probe, recommendation.effective);
        let summary = SwarmBriefHostProfileSummary {
            recommended_profile: recommendation.recommended.as_str().to_string(),
            confidence: recommendation.confidence.to_string(),
            host_class: host_calibration.host_class.as_str().to_string(),
            calibration_freshness: host_calibration.calibration_freshness.as_str().to_string(),
            target_dir_posture: host_calibration.target_dir_posture.to_string(),
            topology_warnings: host_calibration
                .topology_warnings
                .iter()
                .map(|warning| (*warning).to_string())
                .collect(),
            repair_action_kinds: host_calibration
                .repair_actions
                .iter()
                .map(|action| action.kind.to_string())
                .collect(),
            budget_delta_count: host_calibration.budget_deltas.len(),
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

pub struct MemoryDriftSourceAdapter;

impl SwarmBriefSourceAdapter for MemoryDriftSourceAdapter {
    fn collect(&self, options: &SwarmBriefCollectOptions) -> SwarmBriefSourceOutput {
        let source = SwarmBriefSourceKind::MemoryDrift;
        let provenance = SwarmBriefSourceProvenance::local_probe();
        let database_path = memory_drift_database_path(&options.workspace);
        if let Err(error) = validate_memory_drift_database_path(&database_path) {
            let degradation = SwarmBriefDegradation::warning(
                source,
                MEMORY_DRIFT_REPORT_UNAVAILABLE_CODE,
                memory_drift_unavailable_message(&error),
                Some("ee doctor --json".to_string()),
            );
            return SwarmBriefSourceOutput {
                snapshot: SwarmBriefSourceSnapshot::unavailable(source, provenance, degradation),
                contribution: SwarmBriefContribution::None,
            };
        }

        let report_options = super::memory_drift::MemoryDriftReportOptions {
            database_path: &database_path,
            workspace_path: &options.workspace,
            mode: super::memory_drift::MemoryDriftReportMode::RecentPackItems,
            memory_id: None,
            limit: MEMORY_DRIFT_SWARM_BRIEF_LIMIT,
            include_tombstoned: false,
            as_of: None,
        };
        match super::memory_drift::build_memory_drift_report_read_only(&report_options) {
            Ok(report) => {
                let summary = swarm_brief_memory_drift_summary_from_report(&report);
                let degraded = memory_drift_degradations_from_summary(&summary);
                let item_count = usize::try_from(summary.affected_count).unwrap_or(usize::MAX);
                SwarmBriefSourceOutput {
                    snapshot: SwarmBriefSourceSnapshot::ready(source, provenance, item_count)
                        .with_authoritative_findings(degraded),
                    contribution: SwarmBriefContribution::MemoryDrift(summary),
                }
            }
            Err(error) => {
                // Pre-inspection collection failures are not unverifiable
                // memory evidence. Preserve the dedicated contention code and
                // use report-unavailable for every other collector failure.
                let degradation =
                    if super::memory_drift::memory_drift_error_is_lock_contention(&error) {
                        SwarmBriefDegradation::warning(
                            source,
                            super::memory_drift::MEMORY_DRIFT_LOCK_CONTENTION_CODE,
                            super::memory_drift::memory_drift_lock_contention_message(
                                "swarm_brief",
                            ),
                            Some(
                                super::memory_drift::MEMORY_DRIFT_LOCK_CONTENTION_REPAIR
                                    .to_string(),
                            ),
                        )
                    } else {
                        SwarmBriefDegradation::warning(
                            source,
                            MEMORY_DRIFT_REPORT_UNAVAILABLE_CODE,
                            memory_drift_report_unavailable_message(&format!(
                                "report build failed: {error}"
                            )),
                            Some("ee doctor --json".to_string()),
                        )
                    };
                SwarmBriefSourceOutput {
                    snapshot: SwarmBriefSourceSnapshot::unavailable(
                        source,
                        provenance,
                        degradation,
                    ),
                    contribution: SwarmBriefContribution::None,
                }
            }
        }
    }
}

pub struct ToolchainSourceAdapter<'a, R> {
    pub runner: &'a R,
}

impl<R: SwarmBriefCommandRunner> SwarmBriefSourceAdapter for ToolchainSourceAdapter<'_, R> {
    fn collect(&self, options: &SwarmBriefCollectOptions) -> SwarmBriefSourceOutput {
        let mut toolchain_options =
            ToolchainProvenanceOptions::for_workspace(options.workspace.clone());
        toolchain_options.command_timeout_ms = options.command_timeout_ms;
        toolchain_options.agent_mail_snapshot = options.agent_mail_snapshot_path.clone();

        let report = collect_toolchain_provenance_with_runner(&toolchain_options, self.runner);
        let degraded = toolchain_claim_gate_degradations(&report);
        let summary = toolchain_brief_summary_from_report(&report);
        let item_count = summary.tool_count.saturating_add(summary.script_hash_count);

        SwarmBriefSourceOutput {
            snapshot: SwarmBriefSourceSnapshot::ready(
                SwarmBriefSourceKind::Toolchain,
                SwarmBriefSourceProvenance::local_probe(),
                item_count,
            )
            .with_degraded(degraded),
            contribution: SwarmBriefContribution::Toolchain(summary),
        }
    }
}

fn toolchain_brief_summary_from_report(
    report: &ToolchainProvenanceReport,
) -> SwarmBriefToolchainProvenanceSummary {
    let mut tools = report
        .tools
        .iter()
        .map(toolchain_tool_summary)
        .collect::<Vec<_>>();
    tools.sort();
    tools.dedup();

    let mut script_hashes = report
        .script_hashes
        .iter()
        .map(|row| SwarmBriefToolchainScriptSummary {
            script: row.script.clone(),
            blake3_preview: hash_preview(&row.blake3),
            tracked: row.tracked,
        })
        .collect::<Vec<_>>();
    script_hashes.sort();
    script_hashes.dedup();

    let critical_blocker_count = report
        .tools
        .iter()
        .filter(|row| toolchain_claim_gate_degradation_code(row.tool, row.freshness).is_some())
        .count();
    let advisory_unknown_count = report
        .tools
        .iter()
        .filter(|row| {
            !row.tool.critical()
                && row.freshness != ToolchainFreshness::Current
                && toolchain_claim_gate_degradation_code(row.tool, row.freshness).is_none()
        })
        .count();
    let mut degraded_codes = report
        .degraded
        .iter()
        .map(|entry| entry.code.clone())
        .chain(
            report
                .tools
                .iter()
                .flat_map(|row| row.degraded.iter().map(|entry| entry.code.clone())),
        )
        .collect::<Vec<_>>();
    degraded_codes.sort();
    degraded_codes.dedup();

    SwarmBriefToolchainProvenanceSummary {
        schema: TOOLCHAIN_PROVENANCE_SCHEMA_V1.to_owned(),
        redaction_status: TOOLCHAIN_PROVENANCE_REDACTION_STATUS.to_owned(),
        workspace_fingerprint: report.workspace_fingerprint.clone(),
        tool_count: report.tools.len(),
        script_hash_count: report.script_hashes.len(),
        critical_blocker_count,
        advisory_unknown_count,
        tools,
        script_hashes,
        degraded_codes,
    }
}

fn toolchain_tool_summary(
    row: &crate::core::support_bundle::ToolchainToolRow,
) -> SwarmBriefToolchainToolSummary {
    SwarmBriefToolchainToolSummary {
        tool: toolchain_tool_label(row.tool),
        kind: toolchain_tool_kind_label(row.kind),
        state: row.freshness.code(),
        critical: row.tool.critical(),
        version: row.version.clone(),
        binary_hash_preview: row.binary_hash.as_deref().map(hash_preview),
        source_hint: toolchain_source_hint_label(row.source_hint),
        source_command_id: row.probe.command_id.clone(),
        exit_class: row.probe.exit_class.as_str(),
        repair: row.degraded.iter().find_map(|entry| entry.repair.clone()),
    }
}

fn toolchain_claim_gate_degradations(
    report: &ToolchainProvenanceReport,
) -> Vec<SwarmBriefDegradation> {
    let mut degraded = report
        .tools
        .iter()
        .filter_map(|row| {
            let code = toolchain_claim_gate_degradation_code(row.tool, row.freshness)?;
            let source = row.degraded.first();
            Some(SwarmBriefDegradation {
                code: code.to_owned(),
                source: SwarmBriefSourceKind::Toolchain,
                severity: toolchain_claim_gate_degradation_severity(row.tool, row.freshness),
                message: source.map_or_else(
                    || {
                        format!(
                            "{} toolchain state {} is not authoritative for claim-gate work.",
                            toolchain_tool_label(row.tool),
                            row.freshness.code()
                        )
                    },
                    |entry| entry.message.clone(),
                ),
                repair: source
                    .and_then(|entry| entry.repair.clone())
                    .or_else(|| toolchain_claim_gate_repair(row.tool).map(str::to_owned)),
            })
        })
        .collect::<Vec<_>>();
    degraded.sort();
    degraded.dedup();
    degraded
}

fn toolchain_claim_gate_degradation_code(
    tool: ToolchainToolId,
    freshness: ToolchainFreshness,
) -> Option<&'static str> {
    match (tool, freshness) {
        (
            ToolchainToolId::Ee,
            ToolchainFreshness::StaleBinary | ToolchainFreshness::SourceMismatch,
        ) => Some("stale_binary_suspected"),
        (
            ToolchainToolId::Ee,
            ToolchainFreshness::WrapperMissing
            | ToolchainFreshness::CommandTimeout
            | ToolchainFreshness::VersionUnknown
            | ToolchainFreshness::UnsupportedPlatform,
        ) => Some("missing_required_surface"),
        (ToolchainToolId::AgentMail, ToolchainFreshness::HealthCorrupt) => {
            Some("agent_mail_semantic_readiness_failed")
        }
        (
            ToolchainToolId::AgentMail,
            ToolchainFreshness::CommandTimeout
            | ToolchainFreshness::WrapperMissing
            | ToolchainFreshness::VersionUnknown
            | ToolchainFreshness::UnsupportedPlatform,
        ) => Some("agent_mail_unavailable"),
        (ToolchainToolId::Br, ToolchainFreshness::CommandTimeout) => Some("beads_command_timeout"),
        (
            ToolchainToolId::Br,
            ToolchainFreshness::WrapperMissing
            | ToolchainFreshness::VersionUnknown
            | ToolchainFreshness::UnsupportedPlatform,
        ) => Some("beads_unavailable"),
        _ => None,
    }
}

fn toolchain_claim_gate_degradation_severity(
    tool: ToolchainToolId,
    freshness: ToolchainFreshness,
) -> &'static str {
    match (tool, freshness) {
        (ToolchainToolId::AgentMail, ToolchainFreshness::HealthCorrupt) => "high",
        (
            ToolchainToolId::Ee,
            ToolchainFreshness::StaleBinary | ToolchainFreshness::SourceMismatch,
        ) => "medium",
        _ => "warning",
    }
}

fn toolchain_claim_gate_repair(tool: ToolchainToolId) -> Option<&'static str> {
    match tool {
        ToolchainToolId::Ee => Some("ee install check --json --offline"),
        ToolchainToolId::AgentMail => Some(agent_mail_snapshot_producer_command_template()),
        ToolchainToolId::Br => Some("scripts/br_retry.sh actionable --json"),
        _ => None,
    }
}

fn toolchain_tool_label(tool: ToolchainToolId) -> &'static str {
    match tool {
        ToolchainToolId::Ee => "ee",
        ToolchainToolId::Rch => "rch",
        ToolchainToolId::Br => "br",
        ToolchainToolId::Bv => "bv",
        ToolchainToolId::AgentMail => "agent_mail",
        ToolchainToolId::Cass => "cass",
        ToolchainToolId::Git => "git",
        ToolchainToolId::Cargo => "cargo",
    }
}

fn toolchain_tool_kind_label(kind: ToolchainToolKind) -> &'static str {
    match kind {
        ToolchainToolKind::Binary => "binary",
        ToolchainToolKind::Service => "service",
        ToolchainToolKind::ScriptSuite => "script_suite",
    }
}

fn toolchain_source_hint_label(hint: ToolchainSourceHint) -> &'static str {
    match hint {
        ToolchainSourceHint::ReleaseInstall => "release_install",
        ToolchainSourceHint::CargoTarget => "cargo_target",
        ToolchainSourceHint::SystemPackage => "system_package",
        ToolchainSourceHint::Unknown => "unknown",
    }
}

fn hash_preview(value: &str) -> String {
    const MAX_HASH_PREVIEW_CHARS: usize = 24;
    if value.chars().count() <= MAX_HASH_PREVIEW_CHARS {
        return value.to_owned();
    }
    let mut preview = value
        .chars()
        .take(MAX_HASH_PREVIEW_CHARS.saturating_sub(3))
        .collect::<String>();
    preview.push_str("...");
    preview
}

fn memory_drift_database_path(workspace: &Path) -> PathBuf {
    workspace.join(".ee").join("ee.db")
}

fn validate_memory_drift_database_path(database_path: &Path) -> io::Result<()> {
    if let Some(symlink) = first_existing_snapshot_symlink_component(database_path)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing to inspect memory drift database through symlink '{}'",
                redact_path_label(&symlink)
            ),
        ));
    }
    let metadata = fs::symlink_metadata(database_path)?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "memory drift database path '{}' is not a file",
                redact_path_label(database_path)
            ),
        ));
    }
    Ok(())
}

fn memory_drift_unavailable_message(error: &io::Error) -> String {
    let reason = match error.kind() {
        io::ErrorKind::NotFound => {
            "database is missing, so recent pack memory drift posture is unknown".to_string()
        }
        io::ErrorKind::PermissionDenied => {
            format!("database path is unsafe or unreadable: {error}")
        }
        io::ErrorKind::InvalidInput => format!("database path is invalid: {error}"),
        _ => format!("database could not be inspected: {error}"),
    };
    memory_drift_report_unavailable_message(&reason)
}

fn memory_drift_report_unavailable_message(reason: &str) -> String {
    format!(
        "{MEMORY_DRIFT_REPORT_UNAVAILABLE_MESSAGE_PREFIX}: {}",
        redact_brief_text(reason)
    )
}

fn swarm_brief_memory_drift_summary_from_report(
    report: &super::memory_drift::MemoryDriftReport,
) -> SwarmBriefMemoryDriftSummary {
    let mut top_affected_memory_ids = report
        .items
        .iter()
        .filter(|item| item.drift_status != super::memory_drift::MemoryDriftStatus::Current)
        .take(super::memory_drift::MAX_MEMORY_DRIFT_SUPPORT_SUMMARY_ITEMS)
        .map(|item| redact_brief_text(&item.memory_id))
        .collect::<Vec<_>>();
    top_affected_memory_ids.sort();
    top_affected_memory_ids.dedup();

    let degraded_codes = memory_drift_degraded_codes(report);
    let source_kind_counts = memory_drift_source_kind_counts(report);
    let affected_count = report
        .summary
        .changed
        .saturating_add(report.summary.missing_source)
        .saturating_add(report.summary.stale_anchor)
        .saturating_add(report.summary.unverifiable);
    let status = if !degraded_codes.is_empty() {
        "degraded"
    } else if affected_count == 0 {
        "empty_queue"
    } else {
        "available"
    }
    .to_string();

    SwarmBriefMemoryDriftSummary {
        status,
        report_mode: report.mode.as_str().to_string(),
        total_memories: report.summary.total_memories,
        current_count: report.summary.current,
        changed_count: report.summary.changed,
        missing_source_count: report.summary.missing_source,
        stale_anchor_count: report.summary.stale_anchor,
        unverifiable_count: report.summary.unverifiable,
        suppressed_count: report.summary.suppressed,
        affected_count,
        top_affected_memory_ids,
        degraded_codes,
        source_kind_counts,
    }
}

fn memory_drift_degraded_codes(report: &super::memory_drift::MemoryDriftReport) -> Vec<String> {
    let mut codes = report
        .degraded
        .iter()
        .map(|degradation| redact_brief_text(&degradation.code))
        .chain(
            report
                .items
                .iter()
                .filter_map(|item| item.degraded_code.as_deref())
                .map(redact_brief_text),
        )
        .collect::<Vec<_>>();
    codes.sort();
    codes.dedup();
    codes
}

fn memory_drift_source_kind_counts(
    report: &super::memory_drift::MemoryDriftReport,
) -> BTreeMap<String, u32> {
    let mut counts = BTreeMap::new();
    for item in &report.items {
        *counts
            .entry(memory_drift_source_kind(item).to_string())
            .or_default() += 1;
    }
    counts
}

fn memory_drift_source_kind(item: &super::memory_drift::MemoryDriftSelectionHint) -> &'static str {
    let reason = item.top_reason.as_str();
    if reason.starts_with("provenance_chain_") || reason.starts_with("provenance_") {
        "provenance_chain"
    } else if reason.starts_with("pack_item_") {
        "pack_record"
    } else if reason.contains("schema") {
        "schema"
    } else {
        "memory_record"
    }
}

fn memory_drift_degradations_from_summary(
    summary: &SwarmBriefMemoryDriftSummary,
) -> Vec<SwarmBriefDegradation> {
    summary
        .degraded_codes
        .iter()
        .map(|code| {
            let severity = match code.as_str() {
                "memory_drift_source_missing" => "high",
                "memory_drift_source_changed" | "memory_drift_source_unverifiable" => "medium",
                _ => "warning",
            };
            SwarmBriefDegradation::with_severity(
                SwarmBriefSourceKind::MemoryDrift,
                code.clone(),
                severity,
                format!(
                    "Memory drift source reported {code}; affected recent pack item count is {}.",
                    summary.affected_count
                ),
                Some(default_source_repair(SwarmBriefSourceKind::MemoryDrift).to_string()),
            )
        })
        .collect()
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
        SwarmBriefSourceProvenance::command("br", &BEADS_READY_ARGS),
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
    collect_selected_source(
        &mut report,
        options,
        SwarmBriefSourceKind::MemoryDrift,
        SwarmBriefSourceProvenance::local_probe(),
        || MemoryDriftSourceAdapter.collect(options),
    );
    collect_selected_source(
        &mut report,
        options,
        SwarmBriefSourceKind::Toolchain,
        SwarmBriefSourceProvenance::local_probe(),
        || ToolchainSourceAdapter { runner }.collect(options),
    );
    attach_qos_resource_pressure(&mut report, &options.workspace);
    attach_knowledge_gaps(&mut report, &options.workspace);
    // bd-1eq3l.6: embed the compact workspace-hygiene summary so the swarm
    // brief surfaces counts/dirtyPathCount/needsHumanReviewTop/coordination
    // blockers/beadsStateStatus without forcing a separate
    // `ee workspace hygiene --json` shell-out. The summary builder returns
    // an `unavailable`-status payload (not None) on report-build failure,
    // so the field stays serializable in degraded modes.
    let hygiene_options = WorkspaceHygieneOptions {
        workspace_path: options.workspace.clone(),
        self_agent_name: None,
        agent_mail_snapshot_path: None,
    };
    report.workspace_hygiene = Some(build_workspace_hygiene_swarm_brief_summary(
        &hygiene_options,
    ));
    report.verification_broker = Some(swarm_brief_verification_broker_summary(
        gather_verification_posture(Some(&options.workspace)),
        report.rch_local_capability.as_ref(),
    ));
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

/// Parse a query-miss audit row's `details` JSON into a [`MissAuditObservation`].
/// Returns `None` for rows without a usable `queryHash` (defensive: malformed or
/// schema-drifted rows are skipped, never panicked on).
fn parse_miss_audit_observation(entry: &StoredAuditEntry) -> Option<MissAuditObservation> {
    let details = entry.details.as_deref()?;
    let value: Value = serde_json::from_str(details).ok()?;
    let query_hash = value.get("queryHash")?.as_str()?.trim().to_string();
    if query_hash.is_empty() {
        return None;
    }
    let reason = value
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    Some(MissAuditObservation { query_hash, reason })
}

/// Surface knowledge-gap candidates (bd-1n0np.6.4) from the query-miss audit log:
/// read recorded misses, cluster by exact query hash, and keep hashes that
/// crossed the repeat threshold. Read-only and graceful — a missing/unreadable
/// workspace DB leaves `knowledge_gaps` empty rather than failing the brief.
fn attach_knowledge_gaps(report: &mut SwarmBriefReport, workspace: &Path) {
    let database_path = workspace.join(".ee").join("ee.db");
    let Ok(connection) = DbConnection::open_file(&database_path) else {
        return;
    };
    let Ok(entries) = connection.list_audit_by_action(
        audit_actions::SEARCH_MISS_RECORDED,
        Some(SWARM_BRIEF_MISS_AUDIT_SCAN_LIMIT),
    ) else {
        return;
    };
    let observations: Vec<MissAuditObservation> = entries
        .iter()
        .filter_map(parse_miss_audit_observation)
        .collect();
    report.knowledge_gaps =
        cluster_repeated_misses(&observations, KNOWLEDGE_GAP_MIN_CLUSTER_MISSES)
            .into_iter()
            .map(|gap| SwarmBriefKnowledgeGap {
                query_hash: gap.query_hash,
                miss_count: gap.miss_count,
                reasons: gap.reasons,
            })
            .collect();
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

fn rfc3339_epoch_seconds(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc).timestamp())
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
            operation_state,
            git_ahead,
        } => {
            report.dirty_files.extend(dirty_files);
            report.recent_commits.extend(recent_commits);
            report.git_operation_state = operation_state;
            report.git_ahead = git_ahead;
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
            agent_name,
            file_reservations,
            agents,
            inbox,
            threads,
        } => {
            report.agent_mail_agent_name = agent_name;
            report.file_reservations.extend(file_reservations);
            report.agent_mail_agents.extend(agents);
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
        SwarmBriefContribution::MemoryDrift(summary) => {
            report.memory_drift = Some(summary);
        }
        SwarmBriefContribution::Toolchain(summary) => {
            report.toolchain_provenance = Some(summary);
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
    report.ready_reservation_pressure = summarize_ready_reservation_pressure(report);
    report.stalled_bead_liveness = summarize_stalled_bead_liveness(report);
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

    let counts = json!({
        "sourceCount": report.sources.len(),
        "dirtyFileCount": report.dirty_files.len(),
        "recentCommitCount": report.recent_commits.len(),
        "gitOperationInProgress": report.git_operation_state.in_progress,
        "gitOperationMarkerCount": report.git_operation_state.operations.len(),
        "gitAutostashMarkerCount": report.git_operation_state.autostash_markers.len(),
        "gitAheadCount": report.git_ahead.as_ref().map_or(0, |snapshot| snapshot.ahead_count),
        "gitAheadCommitCount": report.git_ahead.as_ref().map_or(0, |snapshot| snapshot.commits.len()),
        "gitAheadPeerOwnedRisk": report.git_ahead.as_ref().is_some_and(|snapshot| snapshot.peer_owned_ahead_risk),
        "readyWorkCount": report.beads.ready.len(),
        "blockedWorkCount": report.beads.blocked.len(),
        "inProgressWorkCount": report.beads.in_progress.len(),
        "deferredWorkCount": report.beads.deferred.len(),
        "activeReservationCount": report.file_reservations.len(),
        "exclusiveReservationCount": report.file_reservations.iter().filter(|reservation| reservation.exclusive).count(),
        "activeConflictCount": active_conflict_count,
        "fileSurfaceRiskCount": report.file_surface_risks.len(),
        "readyReservationPressureCount": report.ready_reservation_pressure.len(),
        "stalledBeadLivenessCount": report.stalled_bead_liveness.len(),
        "agentMailAgentCount": report.agent_mail_agents.len(),
        "inboxMailboxCount": report.inbox.len(),
        "unreadCount": report.inbox.iter().fold(0_u64, |total, item| total.saturating_add(item.unread_count)),
        "ackRequiredCount": report.inbox.iter().fold(0_u64, |total, item| total.saturating_add(item.ack_required_count)),
        "threadCount": report.threads.len(),
        "resourcePressureHintCount": report.resource_pressure.len(),
        "memoryDriftAffectedCount": report.memory_drift.as_ref().map_or(0, |summary| summary.affected_count),
        "memoryDriftTopAffectedCount": report.memory_drift.as_ref().map_or(0, |summary| summary.top_affected_memory_ids.len() as u32),
        "verificationBrokerRecentReusableRunCount": report.verification_broker.as_ref().map_or(0, |summary| summary.recent_reusable_run_count),
        "verificationBrokerKnownBlockerCount": report.verification_broker.as_ref().map_or(0, verification_broker_known_blocker_count),
        "verificationBrokerInFlightCount": report.verification_broker.as_ref().map_or(0, |summary| summary.in_flight_equivalent_command_count),
        "degradedCount": report.degraded.len(),
        "recommendationCount": report.recommendations.len(),
        "symbolRiskPathCount": report.workspace_hygiene.as_ref().and_then(|summary| summary.symbol_risk_summary.as_ref()).map_or(0, |summary| summary.summarized_path_count),
        "symbolRiskHighRiskSymbolCount": report.workspace_hygiene.as_ref().and_then(|summary| summary.symbol_risk_summary.as_ref()).map_or(0, |summary| summary.high_risk_symbol_count),
    });
    let bv = json!({
        "actionableCount": report.bv.as_ref().and_then(|summary| summary.actionable_count),
        "blockedCount": report.bv.as_ref().and_then(|summary| summary.blocked_count),
        "inProgressCount": report.bv.as_ref().and_then(|summary| summary.in_progress_count),
        "trackCount": report.bv.as_ref().and_then(|summary| summary.track_count),
        "topPickIds": report.bv.as_ref().map(|summary| {
            summary.top_picks.iter().take(5).map(|pick| pick.id.clone()).collect::<Vec<_>>()
        }).unwrap_or_default(),
    });
    let provenance = json!({
        "underlyingReportHash": report_hash,
        "sideEffectFree": true,
        "rawCommandTextIncluded": false,
        "sourceProvenance": swarm_brief_source_provenance_summaries(report),
    });
    let redaction = json!({
        "rawMailBodiesIncluded": false,
        "rawQueryTextIncluded": false,
        "rawProvenanceTextIncluded": false,
        "fullFileListingsIncluded": false,
        "rawSymbolNamesIncluded": false,
        "rawAgentNamesIncluded": false,
        "reservationHolderLabelsIncluded": "hashes_only",
        "recommendationEvidenceIncluded": "hashes_only",
    });

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
        "counts": counts,
        "bv": bv,
        "memoryDrift": swarm_brief_memory_drift_summary(report),
        "gitAhead": swarm_brief_git_ahead_summary(report),
        "verificationBroker": swarm_brief_verification_broker_summary_value(report),
        "sourceStatusCounts": source_status_counts,
        "sourceStatuses": swarm_brief_source_status_summaries(report),
        "resourcePressurePosture": swarm_brief_resource_pressure_posture(report),
        "rchWorkerPressure": swarm_brief_rch_worker_pressure_summary(report),
        "singleFlight": singleflight_posture_report(),
        "degradedCodes": degraded_codes,
        "fileSurfaceRiskSummary": swarm_brief_file_surface_risk_summary(report),
        "readyReservationPressureSummary": swarm_brief_ready_reservation_pressure_summary(report),
        "stalledBeadLivenessSummary": swarm_brief_stalled_bead_liveness_summary(report),
        "symbolRiskSummary": swarm_brief_symbol_risk_summary(report),
        "topRecommendations": swarm_brief_summary_recommendations(report),
        "provenance": provenance,
        "redaction": redaction,
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
    let memory_drift = summary.get("memoryDrift").unwrap_or(&Value::Null);
    let memory_drift_status = memory_drift
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let memory_drift_affected = memory_drift
        .get("affectedCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let memory_drift_changed = memory_drift
        .get("changedCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let memory_drift_missing = memory_drift
        .get("missingSourceCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let memory_drift_unverifiable = memory_drift
        .get("unverifiableCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let git_ahead = summary.get("gitAhead").unwrap_or(&Value::Null);
    let git_ahead_status = git_ahead
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let git_ahead_count = git_ahead
        .get("aheadCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let git_ahead_commits = git_ahead
        .get("commitCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let git_ahead_peer_risk = git_ahead
        .get("peerOwnedAheadRisk")
        .and_then(Value::as_bool)
        .unwrap_or(false);
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
    let symbol_risk = summary.get("symbolRiskSummary").unwrap_or(&Value::Null);
    let symbol_risk_status = symbol_risk
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let symbol_risk_dirty_paths = symbol_risk
        .get("dirtyPathCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let symbol_risk_high_risk = symbol_risk
        .get("highRiskSymbolCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let symbol_risk_linked_evidence = symbol_risk
        .get("linkedEvidenceCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let symbol_risk_agent_activity = symbol_risk
        .get("recentAgentActivityCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let verification_broker = summary.get("verificationBroker").unwrap_or(&Value::Null);
    let verification_broker_status = verification_broker
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let verification_reusable = verification_broker
        .get("recentReusableRunCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let verification_known_blockers = verification_broker
        .get("knownBlockerCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let verification_in_flight = verification_broker
        .get("inFlightEquivalentCommandCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let verification_rch_queue = verification_broker
        .get("rchQueueStatus")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let verification_rch_worker_pressure = verification_broker
        .get("rchWorkerPressureStatus")
        .and_then(Value::as_str)
        .unwrap_or("unknown");

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
    if memory_drift_status != "unknown" && memory_drift_affected > 0 {
        lines.push(format!(
            "Memory drift posture: status={memory_drift_status}, affected={memory_drift_affected}, changed={memory_drift_changed}, missing_source={memory_drift_missing}, unverifiable={memory_drift_unverifiable}."
        ));
    }
    if git_ahead_peer_risk {
        lines.push(format!(
            "Push-safety posture: status={git_ahead_status}, ahead={git_ahead_count}, commits={git_ahead_commits}, peer_owned_risk=true; coordinate and inspect git log origin/main..HEAD --oneline --decorate before pushing."
        ));
    }
    if symbol_risk_status != "unknown"
        && symbol_risk_status != "not_collected"
        && (symbol_risk_dirty_paths > 0
            || symbol_risk_high_risk > 0
            || symbol_risk_linked_evidence > 0)
    {
        lines.push(format!(
            "Symbol-risk posture: status={symbol_risk_status}, dirty_paths={symbol_risk_dirty_paths}, high_risk_symbols={symbol_risk_high_risk}, linked_evidence={symbol_risk_linked_evidence}, recent_agent_activity={symbol_risk_agent_activity}, raw_symbol_names_included=false."
        ));
    }
    if verification_broker_status != "unknown"
        && verification_broker_status != "not_collected"
        && (verification_reusable > 0
            || verification_known_blockers > 0
            || verification_in_flight > 0)
    {
        lines.push(format!(
            "Verification broker posture: status={verification_broker_status}, reusable={verification_reusable}, known_blockers={verification_known_blockers}, in_flight={verification_in_flight}, rch_queue={verification_rch_queue}, worker_pressure={verification_rch_worker_pressure}, raw_logs_included=false."
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

fn swarm_brief_verification_broker_summary(
    posture: VerificationPostureReport,
    capability: Option<&RchLocalCapabilityReport>,
) -> SwarmBriefVerificationBrokerSummary {
    let queue_health = capability.and_then(|report| report.queue_health.as_ref());
    let worker_pressure = capability.map(|report| &report.worker_pressure);

    SwarmBriefVerificationBrokerSummary {
        schema: SWARM_BRIEF_VERIFICATION_BROKER_SCHEMA_V1,
        source_schema: posture.schema,
        status: posture.status,
        record_count: posture.record_count,
        recent_run_count: posture.recent_run_count,
        stale_run_count: posture.stale_run_count,
        unknown_age_count: posture.unknown_age_count,
        recent_reusable_run_count: posture.recent_reusable_run_count,
        in_flight_equivalent_command_count: posture.in_flight_equivalent_command_count,
        advisory_counts: posture.advisory_counts,
        evidence_health: posture.evidence_health,
        recovery_actions: posture.recovery_actions,
        rch_queue_status: queue_health
            .map(|queue| queue.status.clone())
            .unwrap_or_else(|| "not_collected".to_string()),
        rch_slots_available: queue_health.and_then(|queue| queue.slots_available),
        rch_queue_head_slots_needed: queue_health.and_then(|queue| queue.queue_head_slots_needed),
        rch_worker_pressure_status: worker_pressure
            .map(|pressure| pressure.status.clone())
            .unwrap_or_else(|| "not_collected".to_string()),
        rch_usable_worker_count: worker_pressure.map_or(0, |pressure| pressure.usable_worker_count),
        rch_blocked_worker_count: worker_pressure
            .map_or(0, |pressure| pressure.blocked_worker_count),
        raw_logs_included: false,
        raw_mail_bodies_included: false,
    }
}

/// Collect redaction-safe replay ledger summaries from swarm replay artifacts.
#[must_use]
pub fn collect_swarm_replay_summary(workspace: &Path) -> Value {
    let artifact_dir = workspace.join(crate::core::lab::SWARM_REPLAY_ARTIFACT_DIR_TAIL);
    let source = json!({
        "kind": "artifact_directory",
        "schema": crate::core::lab::SWARM_REPLAY_RESULT_SCHEMA_V1,
        "pathIncluded": false,
        "pathHash": blake3_summary_hash(&artifact_dir.display().to_string()),
        "supportBundleFile": "swarm_replay_summary.json",
        "resultFile": crate::core::lab::SWARM_REPLAY_RESULT_ARTIFACT_FILE,
    });
    let mut counts = json!({
        "runDirectoryCount": 0,
        "resultArtifactCount": 0,
        "summarizedReplayCount": 0,
        "omittedReplayCount": 0,
        "malformedReplayCount": 0,
    });

    if !artifact_dir.is_dir() {
        return swarm_replay_summary_value(
            "artifact_directory_missing",
            source,
            counts,
            Vec::new(),
        );
    }

    let Ok(entries) = fs::read_dir(&artifact_dir) else {
        return swarm_replay_summary_value(
            "artifact_directory_unreadable",
            source,
            counts,
            Vec::new(),
        );
    };

    let mut result_paths = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            result_paths.push(
                entry
                    .path()
                    .join(crate::core::lab::SWARM_REPLAY_RESULT_ARTIFACT_FILE),
            );
            increment_summary_count(&mut counts, "runDirectoryCount");
        } else if file_type.is_file()
            && entry.file_name().to_str()
                == Some(crate::core::lab::SWARM_REPLAY_RESULT_ARTIFACT_FILE)
        {
            result_paths.push(entry.path());
        }
    }
    result_paths.sort();
    counts["resultArtifactCount"] = json!(result_paths.len());

    let mut replays = Vec::new();
    for path in result_paths {
        if replays.len() >= MAX_SWARM_REPLAY_SUMMARY_RECORDS {
            increment_summary_count(&mut counts, "omittedReplayCount");
            continue;
        }
        match summarize_swarm_replay_result_artifact(&path) {
            Some(summary) => replays.push(summary),
            None => increment_summary_count(&mut counts, "malformedReplayCount"),
        }
    }

    replays.sort_by(|left, right| {
        right
            .get("artifactModifiedEpochMs")
            .and_then(Value::as_u64)
            .cmp(&left.get("artifactModifiedEpochMs").and_then(Value::as_u64))
            .then_with(|| {
                left.get("runId")
                    .and_then(Value::as_str)
                    .cmp(&right.get("runId").and_then(Value::as_str))
            })
    });
    counts["summarizedReplayCount"] = json!(replays.len());

    let status = if replays.is_empty() {
        "no_valid_replay_artifacts"
    } else {
        "available"
    };
    swarm_replay_summary_value(status, source, counts, replays)
}

#[must_use]
pub fn render_swarm_replay_summary_for_handoff(summary: &Value) -> String {
    let status = summary
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let hash = summary
        .get("summaryHash")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let counts = summary.get("counts").unwrap_or(&Value::Null);
    let summarized = counts
        .get("summarizedReplayCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let omitted = counts
        .get("omittedReplayCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let malformed = counts
        .get("malformedReplayCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let latest = summary.get("latestReplay").unwrap_or(&Value::Null);
    let latest_run = latest
        .get("runId")
        .and_then(Value::as_str)
        .unwrap_or("none");
    let latest_status = latest
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("none");
    let proof_level = latest
        .pointer("/proofCapsule/proofLevel")
        .and_then(Value::as_str)
        .unwrap_or("none");
    let rch_status = latest
        .pointer("/proofCapsule/rchStatus")
        .and_then(Value::as_str)
        .unwrap_or("none");
    let degraded_codes = summary
        .get("degradedCodes")
        .and_then(Value::as_array)
        .map(|codes| {
            codes
                .iter()
                .filter_map(Value::as_str)
                .take(MAX_SWARM_REPLAY_DEGRADED_CODES)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut lines = vec![
        format!(
            "Swarm replay summary: status={status}, replays={summarized}, omitted={omitted}, malformed={malformed}."
        ),
        format!(
            "Latest replay: run_id={latest_run}, status={latest_status}, proof_level={proof_level}, rch_status={rch_status}."
        ),
        format!("Replay summary hash: {hash}."),
        "Support-bundle replay evidence only; raw command output, command arguments, host paths, mail bodies, and environment dumps are not embedded.".to_owned(),
    ];
    if !degraded_codes.is_empty() {
        lines.push(format!(
            "Replay degraded codes: {}.",
            degraded_codes.join(", ")
        ));
    }
    lines.push("Inspect the support bundle's swarm_replay_summary.json for compact hashes, then inspect local replay artifacts only in the originating workspace when needed.".to_owned());
    lines.join("\n")
}

#[must_use]
pub fn swarm_replay_summary_evidence_id(summary: &Value) -> String {
    let hash = summary
        .get("summaryHash")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .trim_start_matches("blake3:");
    let short_hash = hash.get(..12).unwrap_or(hash);
    format!("swarm_replay_summary:{short_hash}")
}

/// Hard cap on one persisted `ee.swarm_replay_result.v1` ledger read for
/// support-bundle and handoff summaries.
const SWARM_REPLAY_RESULT_ARTIFACT_MAX_BYTES: u64 = 4 * 1024 * 1024;

fn summarize_swarm_replay_result_artifact(path: &Path) -> Option<Value> {
    use std::io::Read as _;

    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > SWARM_REPLAY_RESULT_ARTIFACT_MAX_BYTES {
        return None;
    }
    let modified_epoch_ms = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or_default();
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(SWARM_REPLAY_RESULT_ARTIFACT_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > SWARM_REPLAY_RESULT_ARTIFACT_MAX_BYTES {
        return None;
    }
    let raw = String::from_utf8(bytes).ok()?;
    let result: Value = serde_json::from_str(&raw).ok()?;
    let (workload_id, run_id, status) = swarm_replay_result_required_shape(&result)?;

    let aggregate = result.get("aggregate").cloned().unwrap_or(Value::Null);
    let host = result
        .get("hostProfileAdmission")
        .cloned()
        .unwrap_or(Value::Null);
    let verification = result.get("verification").cloned().unwrap_or(Value::Null);
    let proof_capsule = verification
        .get("proofCapsule")
        .cloned()
        .unwrap_or(Value::Null);
    let artifact_summary = swarm_replay_artifact_summary(&result);
    let degraded_codes = swarm_replay_degraded_codes(&result);
    let first_failure = swarm_replay_first_failure_summary(&result);

    Some(json!({
        "workloadId": workload_id,
        "runId": run_id,
        "status": status,
        "sideEffectFree": true,
        "artifactModifiedEpochMs": modified_epoch_ms,
        "workloadHash": verification.get("workloadHash").and_then(Value::as_str),
        "replayHash": verification.get("replayHash").and_then(Value::as_str),
        "resultArtifactHash": blake3_summary_hash(&raw),
        "resultPathIncluded": false,
        "resultPathHash": blake3_summary_hash(&path.display().to_string()),
        "hostProfile": {
            "declaredProfile": host.get("declaredProfile").and_then(Value::as_str).unwrap_or("unknown"),
            "requiredClass": host.get("requiredClass").and_then(Value::as_str).unwrap_or("unknown"),
            "observedClass": host.get("observedClass").and_then(Value::as_str).unwrap_or("unknown"),
            "admissionStatus": host.get("status").and_then(Value::as_str).unwrap_or("unknown"),
            "requestedParallelAgents": host.get("requestedParallelAgents").and_then(Value::as_u64).unwrap_or(0),
            "degradedCodes": host.get("degradedCodes").cloned().unwrap_or_else(|| json!([])),
        },
        "aggregate": {
            "commandCount": aggregate.get("commandCount").and_then(Value::as_u64).unwrap_or(0),
            "successCount": aggregate.get("successCount").and_then(Value::as_u64).unwrap_or(0),
            "failureCount": aggregate.get("failureCount").and_then(Value::as_u64).unwrap_or(0),
            "degradedCount": aggregate.get("degradedCount").and_then(Value::as_u64).unwrap_or(0),
            "sloWarningCount": aggregate.get("sloWarningCount").and_then(Value::as_u64).unwrap_or(0),
            "sloFailureCount": aggregate.get("sloFailureCount").and_then(Value::as_u64).unwrap_or(0),
            "firstSloFailureStepId": aggregate.get("firstSloFailureStepId").and_then(Value::as_str),
            "p95Ms": aggregate.get("p95Ms").and_then(Value::as_u64).unwrap_or(0),
            "p99Ms": aggregate.get("p99Ms").and_then(Value::as_u64).unwrap_or(0),
        },
        "firstFailure": first_failure,
        "degradedCodes": degraded_codes,
        "proofCapsule": {
            "schema": proof_capsule.get("schema").and_then(Value::as_str).unwrap_or("unknown"),
            "proofLevel": proof_capsule.get("proofLevel").and_then(Value::as_str).unwrap_or("unknown"),
            "rchRequired": verification.get("rchRequired").and_then(Value::as_bool).unwrap_or(false),
            "rchStatus": verification.get("rchStatus").and_then(Value::as_str).unwrap_or("unknown"),
            "remoteMarkerPresent": proof_capsule.pointer("/rch/remoteMarkerPresent").and_then(Value::as_bool),
            "cargoStarted": proof_capsule.pointer("/rch/cargoStarted").and_then(Value::as_bool),
            "commandHash": proof_capsule.pointer("/rch/commandHash").and_then(Value::as_str),
            "workerIdIncluded": false,
            "workerIdHash": proof_capsule.pointer("/rch/workerId").and_then(Value::as_str).map(blake3_summary_hash),
            "knownBlocker": proof_capsule.get("rch").and_then(|rch| rch.get("knownBlocker")).map(swarm_replay_known_blocker_summary),
            "rawOutputIncluded": proof_capsule.pointer("/rch/rawOutputIncluded").and_then(Value::as_bool).unwrap_or(false),
            "localPathsRedacted": proof_capsule.pointer("/rch/localPathsRedacted").and_then(Value::as_bool).unwrap_or(true),
        },
        "artifacts": artifact_summary,
        "redaction": {
            "rawTaskStringPresent": result.pointer("/redactionStatus/rawTaskStringPresent").and_then(Value::as_bool).unwrap_or(false),
            "rawQueryTextPresent": result.pointer("/redactionStatus/rawQueryTextPresent").and_then(Value::as_bool).unwrap_or(false),
            "rawMemoryBodyPresent": result.pointer("/redactionStatus/rawMemoryBodyPresent").and_then(Value::as_bool).unwrap_or(false),
            "rawMailBodyPresent": result.pointer("/redactionStatus/rawMailBodyPresent").and_then(Value::as_bool).unwrap_or(false),
            "absoluteHostPathPresent": result.pointer("/redactionStatus/absoluteHostPathPresent").and_then(Value::as_bool).unwrap_or(false),
            "secretsPresent": result.pointer("/redactionStatus/secretsPresent").and_then(Value::as_bool).unwrap_or(false),
            "environmentDumpPresent": result.pointer("/redactionStatus/environmentDumpPresent").and_then(Value::as_bool).unwrap_or(false),
            "fullFileListingPresent": result.pointer("/redactionStatus/fullFileListingPresent").and_then(Value::as_bool).unwrap_or(false),
            "rawCommandOutputIncluded": false,
            "commandArgsIncluded": false,
            "artifactPathsIncluded": false,
        },
    }))
}

fn swarm_replay_result_required_shape(result: &Value) -> Option<(&str, &str, &str)> {
    if result.get("schema").and_then(Value::as_str)
        != Some(crate::core::lab::SWARM_REPLAY_RESULT_SCHEMA_V1)
    {
        return None;
    }
    let workload_id = result.get("workloadId").and_then(Value::as_str)?;
    let run_id = result.get("runId").and_then(Value::as_str)?;
    let status = result.get("status").and_then(Value::as_str)?;
    if !is_prefixed_hex_id(workload_id, "swarmwl_")
        || !is_prefixed_hex_id(run_id, "swarmrun_")
        || !matches!(status, "pass" | "fail" | "blocked" | "degraded")
        || result.get("sideEffectFree").and_then(Value::as_bool) != Some(true)
        || result
            .get("hostProfileAdmission")
            .and_then(Value::as_object)
            .is_none()
        || result
            .get("commandResults")
            .and_then(Value::as_array)
            .is_none()
        || result.get("aggregate").and_then(Value::as_object).is_none()
        || result
            .get("redactionStatus")
            .and_then(Value::as_object)
            .is_none()
        || result
            .get("resourceUsage")
            .and_then(Value::as_object)
            .is_none()
        || result.get("firstFailure").is_none()
        || result
            .get("verification")
            .and_then(Value::as_object)
            .is_none()
        || result.get("warnings").and_then(Value::as_array).is_none()
    {
        return None;
    }
    Some((workload_id, run_id, status))
}

fn is_prefixed_hex_id(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        (16..=64).contains(&suffix.len()) && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn swarm_replay_summary_value(
    status: &str,
    source: Value,
    mut counts: Value,
    mut replays: Vec<Value>,
) -> Value {
    loop {
        counts["summarizedReplayCount"] = json!(replays.len());
        let degraded_codes = swarm_replay_summary_degraded_codes(&replays);
        let status_counts = swarm_replay_summary_status_counts(&replays);
        let latest_replay = replays.first().cloned().unwrap_or(Value::Null);
        let mut value = json!({
            "schema": SWARM_REPLAY_SUMMARY_SCHEMA_V1,
            "sourceSchema": crate::core::lab::SWARM_REPLAY_RESULT_SCHEMA_V1,
            "source": source,
            "status": status,
            "redactionStatus": SWARM_REPLAY_SUMMARY_REDACTION_STATUS,
            "limits": {
                "maxReplays": MAX_SWARM_REPLAY_SUMMARY_RECORDS,
                "maxDegradedCodes": MAX_SWARM_REPLAY_DEGRADED_CODES,
                "maxArtifactHashes": MAX_SWARM_REPLAY_ARTIFACT_HASHES,
                "maxResultArtifactBytes": SWARM_REPLAY_RESULT_ARTIFACT_MAX_BYTES,
                "maxSummaryBytes": MAX_SWARM_REPLAY_SUMMARY_BYTES,
            },
            "counts": counts,
            "statusCounts": status_counts,
            "degradedCodes": degraded_codes,
            "latestReplay": latest_replay,
            "replays": replays,
            "redaction": {
                "rawCommandOutputIncluded": false,
                "commandArgsIncluded": false,
                "artifactPathsIncluded": false,
                "hostPathsIncluded": false,
                "mailBodiesIncluded": false,
                "environmentDumpsIncluded": false,
                "workerIdsIncluded": false,
            },
        });
        let summary_hash = blake3_summary_hash(&stable_summary_json(&value));
        value["summaryHash"] = json!(summary_hash);
        let bytes = stable_summary_json(&value).len();
        value["summaryBytes"] = json!(bytes);
        value["withinSizeBudget"] = json!(bytes <= MAX_SWARM_REPLAY_SUMMARY_BYTES);
        if bytes <= MAX_SWARM_REPLAY_SUMMARY_BYTES || replays.is_empty() {
            return value;
        }
        replays.pop();
        increment_summary_count(&mut counts, "omittedReplayCount");
    }
}

fn swarm_replay_artifact_summary(result: &Value) -> Value {
    let mut kind_counts = BTreeMap::<String, u64>::new();
    let mut path_hashes = BTreeSet::<String>::new();
    let mut artifact_count = 0u64;
    if let Some(commands) = result.get("commandResults").and_then(Value::as_array) {
        for command in commands {
            if let Some(artifacts) = command.get("artifactPaths").and_then(Value::as_array) {
                for artifact in artifacts {
                    artifact_count = artifact_count.saturating_add(1);
                    let kind = artifact
                        .get("kind")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    *kind_counts.entry(kind.to_owned()).or_insert(0) += 1;
                    if let Some(hash) = artifact.get("pathHash").and_then(Value::as_str) {
                        path_hashes.insert(hash.to_owned());
                    }
                }
            }
        }
    }
    json!({
        "artifactRefCount": artifact_count,
        "pathIncluded": false,
        "pathHashes": path_hashes
            .into_iter()
            .take(MAX_SWARM_REPLAY_ARTIFACT_HASHES)
            .collect::<Vec<_>>(),
        "kindCounts": kind_counts,
    })
}

fn swarm_replay_known_blocker_summary(value: &Value) -> Value {
    json!({
        "blockerFingerprint": value.get("blockerFingerprint").and_then(Value::as_str),
        "blockerKind": value.get("blockerKind").and_then(Value::as_str),
        "remediationBead": value.get("remediationBead").and_then(Value::as_str),
        "retryAfter": value.get("retryAfter").and_then(Value::as_str),
    })
}

fn swarm_replay_first_failure_summary(result: &Value) -> Value {
    let Some(failure) = result.get("firstFailure") else {
        return Value::Null;
    };
    json!({
        "stepId": failure.get("stepId").and_then(Value::as_str).unwrap_or("unknown"),
        "agentSlot": failure.get("agentSlot").and_then(Value::as_u64).unwrap_or(0),
        "code": failure.get("code").and_then(Value::as_str).unwrap_or("unknown"),
        "severity": failure.get("severity").and_then(Value::as_str).unwrap_or("unknown"),
        "diagnosisIncluded": false,
        "diagnosisHash": failure.get("diagnosis").and_then(Value::as_str).map(blake3_summary_hash),
        "repairHintIncluded": false,
        "repairHintHash": failure.get("repairHint").and_then(Value::as_str).map(blake3_summary_hash),
    })
}

fn swarm_replay_degraded_codes(result: &Value) -> Vec<String> {
    let mut codes = BTreeSet::new();
    insert_string_array_values(
        &mut codes,
        result.pointer("/hostProfileAdmission/degradedCodes"),
    );
    insert_string_array_values(&mut codes, result.get("warnings"));
    insert_string_array_values(
        &mut codes,
        result.pointer("/verification/proofCapsule/rch/degradedCodes"),
    );
    if let Some(code) = result.pointer("/firstFailure/code").and_then(Value::as_str) {
        codes.insert(code.to_owned());
    }
    if let Some(commands) = result.get("commandResults").and_then(Value::as_array) {
        for command in commands {
            insert_string_array_values(&mut codes, command.get("degradedCodes"));
            if let Some(diagnosis) = command.pointer("/slo/diagnosis").and_then(Value::as_str)
                && let Some(code) = diagnosis.split(':').next()
                && code.starts_with("swarm_replay_")
            {
                codes.insert(code.to_owned());
            }
        }
    }
    codes
        .into_iter()
        .take(MAX_SWARM_REPLAY_DEGRADED_CODES)
        .collect()
}

fn insert_string_array_values(codes: &mut BTreeSet<String>, value: Option<&Value>) {
    if let Some(values) = value.and_then(Value::as_array) {
        for value in values {
            if let Some(text) = value.as_str() {
                let code = text.split(':').next().unwrap_or(text).trim();
                if !code.is_empty() {
                    codes.insert(code.to_owned());
                }
            }
        }
    }
}

fn swarm_replay_summary_degraded_codes(replays: &[Value]) -> Vec<String> {
    let mut codes = BTreeSet::new();
    for replay in replays {
        insert_string_array_values(&mut codes, replay.get("degradedCodes"));
    }
    codes
        .into_iter()
        .take(MAX_SWARM_REPLAY_DEGRADED_CODES)
        .collect()
}

fn swarm_replay_summary_status_counts(replays: &[Value]) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for replay in replays {
        let status = replay
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        *counts.entry(status.to_owned()).or_insert(0) += 1;
    }
    counts
}

/// Collect redaction-safe incident replay summaries from committed synthetic fixtures.
#[must_use]
pub fn collect_swarm_incident_summary(workspace: &Path) -> Value {
    let fixture_dir = workspace
        .join("tests")
        .join("fixtures")
        .join("swarm_incidents");
    let mut source = json!({
        "kind": "fixture_directory",
        "schema": "ee.swarm_incident.v1",
        "pathIncluded": false,
        "pathHash": blake3_summary_hash(&fixture_dir.display().to_string()),
        "supportBundleFile": "swarm_incident_summary.json",
    });
    let mut counts = json!({
        "fixtureCount": 0,
        "summarizedIncidentCount": 0,
        "omittedIncidentCount": 0,
        "malformedIncidentCount": 0,
    });

    if !fixture_dir.is_dir() {
        return swarm_incident_summary_value(
            "fixture_directory_missing",
            source,
            counts,
            Vec::new(),
        );
    }

    let Ok(entries) = fs::read_dir(&fixture_dir) else {
        return swarm_incident_summary_value(
            "fixture_directory_unreadable",
            source,
            counts,
            Vec::new(),
        );
    };

    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    paths.sort();
    counts["fixtureCount"] = json!(paths.len());

    let mut incidents = Vec::new();
    for path in paths {
        if incidents.len() >= MAX_SWARM_INCIDENT_SUMMARY_RECORDS {
            increment_summary_count(&mut counts, "omittedIncidentCount");
            continue;
        }
        match summarize_swarm_incident_fixture(&path) {
            Some(summary) => incidents.push(summary),
            None => increment_summary_count(&mut counts, "malformedIncidentCount"),
        }
    }

    incidents.sort_by(|left, right| {
        left.get("scenarioId")
            .and_then(Value::as_str)
            .cmp(&right.get("scenarioId").and_then(Value::as_str))
            .then_with(|| {
                left.get("fixtureHash")
                    .and_then(Value::as_str)
                    .cmp(&right.get("fixtureHash").and_then(Value::as_str))
            })
    });
    counts["summarizedIncidentCount"] = json!(incidents.len());
    if counts
        .get("malformedIncidentCount")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        > 0
    {
        source["malformedFixturesIncluded"] = json!(false);
    }

    let status = if incidents.is_empty() {
        "no_valid_incident_fixtures"
    } else {
        "available"
    };
    swarm_incident_summary_value(status, source, counts, incidents)
}

#[must_use]
pub fn render_swarm_incident_summary_for_handoff(summary: &Value) -> String {
    let status = summary
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let hash = summary
        .get("summaryHash")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let counts = summary.get("counts").unwrap_or(&Value::Null);
    let summarized = counts
        .get("summarizedIncidentCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let omitted = counts
        .get("omittedIncidentCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let malformed = counts
        .get("malformedIncidentCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let scenario_ids = summary
        .get("incidents")
        .and_then(Value::as_array)
        .map(|incidents| {
            incidents
                .iter()
                .filter_map(|incident| incident.get("scenarioId").and_then(Value::as_str))
                .take(4)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let degraded_codes = summary
        .get("degradedCodes")
        .and_then(Value::as_array)
        .map(|codes| {
            codes
                .iter()
                .filter_map(Value::as_str)
                .take(MAX_SWARM_INCIDENT_DEGRADED_CODES)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut lines = vec![
        format!(
            "Swarm incident summary: status={status}, incidents={summarized}, omitted={omitted}, malformed={malformed}."
        ),
        format!("Incident summary hash: {hash}."),
        "Read-only fixture replay evidence; raw logs, mail bodies, commands, command args, and filesystem paths are not embedded.".to_owned(),
    ];
    if !scenario_ids.is_empty() {
        lines.push(format!("Scenario ids: {}.", scenario_ids.join(", ")));
    }
    if !degraded_codes.is_empty() {
        lines.push(format!("Degraded codes: {}.", degraded_codes.join(", ")));
    }
    lines.push("Run `ee diag incident --fixture <path> --json` against a committed fixture for full replay details; do not run live repair actions from this summary.".to_owned());
    lines.join("\n")
}

#[must_use]
pub fn swarm_incident_summary_evidence_id(summary: &Value) -> String {
    let hash = summary
        .get("summaryHash")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .trim_start_matches("blake3:");
    let short_hash = hash.get(..12).unwrap_or(hash);
    format!("swarm_incident_summary:{short_hash}")
}

/// Hard cap on the byte length of one swarm-incident fixture JSON file
/// the summary collector ingests from `<workspace>/tests/fixtures/swarm_incidents/`.
///
/// The summary loop at `collect_swarm_incident_summary` walks the
/// directory, filters to `*.json`, and calls
/// `summarize_swarm_incident_fixture` on each entry. Without a cap, a
/// peer-planted multi-GB `.json` file in that directory (or a runaway
/// fixture writer) would force `fs::read_to_string` to allocate the
/// whole content before `serde_json::from_str` could even start —
/// turning a benign `ee swarm brief` (or any support-bundle path that
/// pulls the swarm-incident summary) into a local OOM.
///
/// The shipped fixtures under `tests/fixtures/swarm_incidents/` are all
/// 3-4 KiB; 1 MiB leaves three orders of magnitude of head-room without
/// leaving the OOM vector open. Same defensive shape as the bounded
/// reads added by Round 1+2 across the workspace-config helpers
/// (`WORKSPACE_CONFIG_MAX_BYTES`, `CURATE_CONFIG_MAX_BYTES`,
/// `MEMORY_SCOPE_CONFIG_MAX_BYTES`) and `read_agent_mail_snapshot`
/// (line 1746) in this same file.
const SWARM_INCIDENT_FIXTURE_MAX_BYTES: u64 = 1024 * 1024;

fn summarize_swarm_incident_fixture(path: &Path) -> Option<Value> {
    use std::io::Read as _;

    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() {
        return None;
    }
    if metadata.len() > SWARM_INCIDENT_FIXTURE_MAX_BYTES {
        // Drop oversize fixtures the same way malformed JSON is dropped
        // upstream: the loop's `None => malformedIncidentCount++` path
        // already accounts for unparseable / unreadable entries, so
        // returning None here keeps the summary counts honest and the
        // caller does not panic on a bad fixture directory.
        return None;
    }
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(SWARM_INCIDENT_FIXTURE_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > SWARM_INCIDENT_FIXTURE_MAX_BYTES {
        // TOCTOU: the fixture grew between the metadata stat and the
        // read. Drop it for the same reason as the metadata pre-check.
        return None;
    }
    let raw = String::from_utf8(bytes).ok()?;
    let fixture: Value = serde_json::from_str(&raw).ok()?;
    if fixture.get("schema").and_then(Value::as_str) != Some("ee.swarm_incident.v1") {
        return None;
    }

    let scenario_id = swarm_incident_fixture_required_shape(&fixture)?;
    let substrate_posture = swarm_incident_substrate_posture(&fixture);
    let status_counts = swarm_incident_status_counts(&substrate_posture);
    let dominant_status = swarm_incident_dominant_status(&substrate_posture);
    let posture = swarm_incident_global_posture(dominant_status);
    let degraded_codes = swarm_incident_degraded_codes(&fixture);
    let recovery_action_summaries = swarm_incident_recovery_action_summaries(&fixture);
    let artifact_refs = swarm_incident_artifact_refs(&fixture);
    let output_core = json!({
        "scenarioId": scenario_id,
        "posture": posture,
        "dominantStatus": dominant_status,
        "substratePosture": substrate_posture,
        "statusCounts": status_counts,
        "degradedCodes": degraded_codes,
        "recoveryActionSummaries": recovery_action_summaries,
        "artifactRefs": artifact_refs,
    });
    let output_hash = blake3_summary_hash(&stable_summary_json(&output_core));
    let fixture_hash = blake3_summary_hash(&stable_summary_json(&redact_summary_value(&fixture)));

    Some(json!({
        "scenarioId": scenario_id,
        "fixedClock": fixture.get("fixedClock").and_then(Value::as_str),
        "purposeIncluded": false,
        "purposeHash": fixture.get("purpose").and_then(Value::as_str).map(blake3_summary_hash),
        "fixtureHash": fixture_hash,
        "outputHash": output_hash,
        "posture": posture,
        "dominantStatus": dominant_status,
        "substratePosture": substrate_posture,
        "statusCounts": status_counts,
        "degradedCodes": degraded_codes,
        "recoveryActionSummaries": recovery_action_summaries,
        "redactionStatus": SWARM_INCIDENT_SUMMARY_REDACTION_STATUS,
        "redaction": {
            "rawLogsIncluded": false,
            "mailBodiesIncluded": false,
            "commandsIncluded": false,
            "commandArgsIncluded": false,
            "filesystemPathsIncluded": false,
            "workerHostnamesIncluded": false,
            "fixturePathsIncluded": false,
            "allowedHostLabelCount": fixture.pointer("/redactionExpectations/allowedHostLabels").and_then(Value::as_array).map_or(0, Vec::len),
            "allowedHostLabelsHash": fixture.pointer("/redactionExpectations/allowedHostLabels").map(|labels| blake3_summary_hash(&stable_summary_json(labels))),
        },
        "provenance": {
            "fixture": {
                "pathIncluded": false,
                "pathHash": blake3_summary_hash(&path.display().to_string()),
                "artifactHash": blake3_summary_hash(&raw),
            },
            "supportBundleFile": "swarm_incident_summary.json",
            "artifactRefs": artifact_refs,
        },
    }))
}

fn swarm_incident_fixture_required_shape(fixture: &Value) -> Option<&str> {
    let scenario_id = fixture.get("scenarioId").and_then(Value::as_str)?;
    if !is_swarm_incident_scenario_id(scenario_id)
        || fixture.get("fixedClock").and_then(Value::as_str).is_none()
        || fixture.get("purpose").and_then(Value::as_str).is_none()
        || fixture
            .get("substrates")
            .and_then(Value::as_object)
            .is_none()
        || fixture
            .get("expectedDegraded")
            .and_then(Value::as_array)
            .is_none()
        || fixture
            .get("expectedRecoveryActions")
            .and_then(Value::as_array)
            .is_none()
        || fixture
            .get("redactionExpectations")
            .and_then(Value::as_object)
            .is_none()
        || fixture
            .get("assertions")
            .and_then(Value::as_object)
            .is_none()
        || fixture.get("artifacts").and_then(Value::as_array).is_none()
    {
        return None;
    }
    Some(scenario_id)
}

fn is_swarm_incident_scenario_id(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

fn swarm_incident_summary_value(
    status: &str,
    source: Value,
    mut counts: Value,
    mut incidents: Vec<Value>,
) -> Value {
    loop {
        counts["summarizedIncidentCount"] = json!(incidents.len());
        let degraded_codes = swarm_incident_summary_degraded_codes(&incidents);
        let status_counts = swarm_incident_summary_status_counts(&incidents);
        let mut value = json!({
            "schema": SWARM_INCIDENT_SUMMARY_SCHEMA_V1,
            "sourceSchema": "ee.swarm_incident.v1",
            "source": source,
            "status": status,
            "redactionStatus": SWARM_INCIDENT_SUMMARY_REDACTION_STATUS,
            "limits": {
                "maxIncidents": MAX_SWARM_INCIDENT_SUMMARY_RECORDS,
                "maxRecoveryActionsPerIncident": MAX_SWARM_INCIDENT_RECOVERY_ACTIONS,
                "maxDegradedCodesPerIncident": MAX_SWARM_INCIDENT_DEGRADED_CODES,
                "maxSummaryBytes": MAX_SWARM_INCIDENT_SUMMARY_BYTES,
            },
            "counts": counts,
            "statusCounts": status_counts,
            "degradedCodes": degraded_codes,
            "incidents": incidents,
            "redaction": {
                "rawLogsIncluded": false,
                "mailBodiesIncluded": false,
                "commandsIncluded": false,
                "commandArgsIncluded": false,
                "filesystemPathsIncluded": false,
                "workerHostnamesIncluded": false,
            },
        });
        let summary_hash = blake3_summary_hash(&stable_summary_json(&value));
        value["summaryHash"] = json!(summary_hash);
        let bytes = stable_summary_json(&value).len();
        value["summaryBytes"] = json!(bytes);
        value["withinSizeBudget"] = json!(bytes <= MAX_SWARM_INCIDENT_SUMMARY_BYTES);
        if bytes <= MAX_SWARM_INCIDENT_SUMMARY_BYTES || incidents.is_empty() {
            return value;
        }
        incidents.pop();
        increment_summary_count(&mut counts, "omittedIncidentCount");
    }
}

fn swarm_incident_substrate_posture(fixture: &Value) -> Value {
    let mut posture = serde_json::Map::new();
    if let Some(substrates) = fixture.get("substrates").and_then(Value::as_object) {
        for name in ["agentMail", "beads", "rch", "disk", "hotPath"] {
            if let Some(status) = substrates
                .get(name)
                .and_then(|substrate| substrate.get("status"))
                .and_then(Value::as_str)
            {
                posture.insert(name.to_owned(), Value::String(status.to_owned()));
            }
        }
    }
    Value::Object(posture)
}

fn swarm_incident_status_counts(substrate_posture: &Value) -> Value {
    let mut counts = BTreeMap::<String, u64>::new();
    if let Some(postures) = substrate_posture.as_object() {
        for status in postures.values().filter_map(Value::as_str) {
            *counts.entry(status.to_owned()).or_insert(0) += 1;
        }
    }
    json!(counts)
}

fn swarm_incident_dominant_status(substrate_posture: &Value) -> &'static str {
    substrate_posture
        .as_object()
        .into_iter()
        .flat_map(|postures| postures.values())
        .filter_map(Value::as_str)
        .max_by_key(|status| swarm_incident_status_rank(status))
        .map(swarm_incident_status_match)
        .unwrap_or("ok")
}

fn swarm_incident_status_rank(status: &str) -> u8 {
    match status {
        "blocked" => 5,
        "unavailable" => 4,
        "stale" => 3,
        "degraded" => 2,
        "ok" => 1,
        "not_applicable" => 0,
        _ => 0,
    }
}

fn swarm_incident_status_match(status: &str) -> &'static str {
    match status {
        "blocked" => "blocked",
        "unavailable" => "unavailable",
        "stale" => "stale",
        "degraded" => "degraded",
        "ok" => "ok",
        "not_applicable" => "not_applicable",
        _ => "unknown",
    }
}

fn swarm_incident_global_posture(dominant_status: &str) -> &'static str {
    match dominant_status {
        "blocked" => "blocked",
        "unavailable" | "stale" | "degraded" => "degraded_recoverable",
        _ => "ok",
    }
}

fn swarm_incident_degraded_codes(fixture: &Value) -> Vec<String> {
    let mut codes = BTreeSet::new();
    if let Some(degraded) = fixture.get("expectedDegraded").and_then(Value::as_array) {
        for item in degraded {
            if let Some(code) = item.get("code").and_then(Value::as_str) {
                codes.insert(code.to_owned());
            }
        }
    }
    if let Some(substrates) = fixture.get("substrates").and_then(Value::as_object) {
        for substrate in substrates.values() {
            if let Some(values) = substrate.get("degradedCodes").and_then(Value::as_array) {
                for value in values {
                    if let Some(code) = value.as_str() {
                        codes.insert(code.to_owned());
                    }
                }
            }
        }
    }
    codes
        .into_iter()
        .take(MAX_SWARM_INCIDENT_DEGRADED_CODES)
        .collect()
}

fn swarm_incident_recovery_action_summaries(fixture: &Value) -> Vec<Value> {
    fixture
        .get("expectedRecoveryActions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_SWARM_INCIDENT_RECOVERY_ACTIONS)
        .map(|action| {
            let summary = action
                .get("summary")
                .and_then(Value::as_str)
                .map(redact_brief_text)
                .map(|text| clamp_summary_text(&text, 180));
            let evidence_hashes = action
                .get("evidence")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(blake3_summary_hash)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            json!({
                "priority": action.get("priority").cloned().unwrap_or(Value::Null),
                "kind": action.get("kind").and_then(Value::as_str).unwrap_or("unknown"),
                "summary": summary,
                "commandPresent": action.get("command").is_some_and(|value| !value.is_null()),
                "commandIncluded": false,
                "manualStepPresent": action.get("manualStep").is_some_and(|value| !value.is_null()),
                "manualStepIncluded": false,
                "destructive": action.get("destructive").and_then(Value::as_bool).unwrap_or(false),
                "preconditionCount": action.get("preconditions").and_then(Value::as_array).map_or(0, Vec::len),
                "evidenceHashes": evidence_hashes,
            })
        })
        .collect()
}

fn swarm_incident_artifact_refs(fixture: &Value) -> Vec<Value> {
    fixture
        .get("artifacts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_SWARM_INCIDENT_ARTIFACT_REFS)
        .map(|artifact| {
            let path = artifact.get("path").and_then(Value::as_str).unwrap_or("");
            json!({
                "kind": artifact.get("kind").and_then(Value::as_str).unwrap_or("unknown"),
                "pathIncluded": false,
                "pathHash": blake3_summary_hash(path),
            })
        })
        .collect()
}

fn swarm_incident_summary_degraded_codes(incidents: &[Value]) -> Vec<String> {
    let mut codes = BTreeSet::new();
    for incident in incidents {
        if let Some(values) = incident.get("degradedCodes").and_then(Value::as_array) {
            for value in values {
                if let Some(code) = value.as_str() {
                    codes.insert(code.to_owned());
                }
            }
        }
    }
    codes.into_iter().collect()
}

fn swarm_incident_summary_status_counts(incidents: &[Value]) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for incident in incidents {
        let posture = incident
            .get("posture")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        *counts.entry(posture.to_owned()).or_insert(0) += 1;
    }
    counts
}

fn increment_summary_count(value: &mut Value, field: &str) {
    value[field] = json!(value.get(field).and_then(Value::as_u64).unwrap_or(0) + 1);
}

fn clamp_summary_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut output = text
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    output.push_str("...");
    output
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

fn swarm_brief_git_ahead_summary(report: &SwarmBriefReport) -> Value {
    let Some(snapshot) = &report.git_ahead else {
        return json!({
            "status": "unknown",
            "available": false,
            "aheadCount": 0,
            "commitCount": 0,
            "peerOwnedAheadRisk": false,
            "degradedCodes": [],
            "rawCommitSubjectsIncluded": false,
        });
    };

    json!({
        "schema": snapshot.schema,
        "status": snapshot.state,
        "available": true,
        "headRef": snapshot.head_ref.as_deref(),
        "upstreamRef": snapshot.upstream_ref.as_deref(),
        "aheadCount": snapshot.ahead_count,
        "behindCount": snapshot.behind_count,
        "commitCount": snapshot.commits.len(),
        "authorCount": snapshot.authors.len(),
        "beadRefCount": snapshot.bead_refs.len(),
        "mixedAuthorAhead": snapshot.mixed_author_ahead,
        "mixedBeadAhead": snapshot.mixed_bead_ahead,
        "ambiguousAhead": snapshot.ambiguous_ahead,
        "peerOwnedAheadRisk": snapshot.peer_owned_ahead_risk,
        "degradedCodes": snapshot.degraded.iter().map(|entry| entry.code).collect::<Vec<_>>(),
        "rawCommitSubjectsIncluded": false,
    })
}

fn swarm_brief_memory_drift_summary(report: &SwarmBriefReport) -> Value {
    let Some(summary) = &report.memory_drift else {
        return json!({
            "status": "unknown",
            "available": false,
            "affectedCount": 0,
            "topAffectedMemoryIds": [],
            "degradedCodes": [],
        });
    };

    json!({
        "status": summary.status.clone(),
        "available": true,
        "reportMode": summary.report_mode.clone(),
        "totalMemories": summary.total_memories,
        "currentCount": summary.current_count,
        "changedCount": summary.changed_count,
        "missingSourceCount": summary.missing_source_count,
        "staleAnchorCount": summary.stale_anchor_count,
        "unverifiableCount": summary.unverifiable_count,
        "suppressedCount": summary.suppressed_count,
        "affectedCount": summary.affected_count,
        "topAffectedMemoryIds": summary.top_affected_memory_ids.clone(),
        "degradedCodes": summary.degraded_codes.clone(),
        "sourceKindCounts": summary.source_kind_counts.clone(),
        "rawSnippetsIncluded": false,
        "rawCommandBodiesIncluded": false,
        "fullListingsIncluded": false,
    })
}

fn swarm_brief_verification_broker_summary_value(report: &SwarmBriefReport) -> Value {
    let Some(summary) = &report.verification_broker else {
        return json!({
            "schema": SWARM_BRIEF_VERIFICATION_BROKER_SCHEMA_V1,
            "status": "not_collected",
            "available": false,
            "recentReusableRunCount": 0,
            "knownBlockerCount": 0,
            "inFlightEquivalentCommandCount": 0,
            "rchQueueStatus": "not_collected",
            "rchWorkerPressureStatus": "not_collected",
            "rawLogsIncluded": false,
            "rawMailBodiesIncluded": false,
        });
    };

    json!({
        "schema": summary.schema,
        "sourceSchema": summary.source_schema,
        "status": summary.status,
        "available": true,
        "recordCount": summary.record_count,
        "recentRunCount": summary.recent_run_count,
        "staleRunCount": summary.stale_run_count,
        "unknownAgeCount": summary.unknown_age_count,
        "recentReusableRunCount": summary.recent_reusable_run_count,
        "knownBlockerCount": verification_broker_known_blocker_count(summary),
        "inFlightEquivalentCommandCount": summary.in_flight_equivalent_command_count,
        "advisoryCounts": summary.advisory_counts,
        "evidenceHealth": summary.evidence_health,
        "recoveryActionKinds": summary.recovery_actions.iter().map(|action| {
            json!({
                "priority": action.priority,
                "kind": &action.kind,
                "commandHash": action.command.as_ref().map(|command| blake3_summary_hash(command)),
                "relatedBeadId": &action.related_bead_id,
            })
        }).collect::<Vec<_>>(),
        "rchQueueStatus": summary.rch_queue_status,
        "rchSlotsAvailable": summary.rch_slots_available,
        "rchQueueHeadSlotsNeeded": summary.rch_queue_head_slots_needed,
        "rchWorkerPressureStatus": summary.rch_worker_pressure_status,
        "rchUsableWorkerCount": summary.rch_usable_worker_count,
        "rchBlockedWorkerCount": summary.rch_blocked_worker_count,
        "rawLogsIncluded": false,
        "rawMailBodiesIncluded": false,
        "rawCommandsIncluded": false,
    })
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

fn swarm_brief_rch_worker_pressure_summary(report: &SwarmBriefReport) -> Value {
    let Some(capability) = &report.rch_local_capability else {
        return json!({
            "schema": RCH_WORKER_PRESSURE_SCHEMA_V1,
            "status": "not_collected",
            "workerCount": 0,
            "usableWorkerCount": 0,
            "blockedWorkerCount": 0,
            "staleWorkerCount": 0,
            "unknownWorkerCount": 0,
            "topWorkers": [],
            "rawPathsIncluded": false,
            "rawCommandsIncluded": false,
        });
    };
    let pressure = &capability.worker_pressure;
    json!({
        "schema": pressure.schema,
        "status": &pressure.status,
        "workerCount": pressure.worker_count,
        "usableWorkerCount": pressure.usable_worker_count,
        "blockedWorkerCount": pressure.blocked_worker_count,
        "staleWorkerCount": pressure.stale_worker_count,
        "unknownWorkerCount": pressure.unknown_worker_count,
        "topWorkers": pressure.workers.iter().take(5).map(|worker| {
            json!({
                "workerId": &worker.worker_id,
                "pressureState": &worker.pressure_state,
                "confidence": &worker.confidence,
                "reasonCode": &worker.reason_code,
                "freeGb": worker.free_gb,
                "freeRatioBps": worker.free_ratio_bps,
                "telemetryFreshness": &worker.telemetry_freshness,
                "admissionImpact": &worker.admission_impact,
            })
        }).collect::<Vec<_>>(),
        "rawPathsIncluded": false,
        "rawCommandsIncluded": false,
    })
}

fn swarm_brief_file_surface_risk_summary(report: &SwarmBriefReport) -> Value {
    let mut counts_by_severity = BTreeMap::<String, usize>::new();
    let mut counts_by_holder = BTreeMap::<String, usize>::new();
    let mut counts_by_git_status = BTreeMap::<String, usize>::new();
    for risk in &report.file_surface_risks {
        *counts_by_severity.entry(risk.severity.clone()).or_default() += 1;
        for holder in &risk.reservation_holders {
            *counts_by_holder
                .entry(blake3_summary_hash(holder))
                .or_default() += 1;
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
                    "reservationHolders": risk.reservation_holders.iter().map(|value| blake3_summary_hash(value)).collect::<Vec<_>>(),
                    "relatedBeadIds": risk.related_bead_ids.clone(),
                    "suggestedCommandHashes": risk.suggested_commands.iter().map(|value| blake3_summary_hash(value)).collect::<Vec<_>>(),
                    "rawPathIncluded": false,
                    "rawCommandsIncluded": false,
                })
            })
            .collect::<Vec<_>>(),
    })
}

fn swarm_brief_ready_reservation_pressure_summary(report: &SwarmBriefReport) -> Value {
    let mut counts_by_action = BTreeMap::<String, usize>::new();
    let mut counts_by_severity = BTreeMap::<String, usize>::new();
    let mut counts_by_holder = BTreeMap::<String, usize>::new();
    for pressure in &report.ready_reservation_pressure {
        *counts_by_action.entry(pressure.action.clone()).or_default() += 1;
        *counts_by_severity
            .entry(pressure.severity.clone())
            .or_default() += 1;
        for holder in &pressure.reservation_holders {
            *counts_by_holder
                .entry(blake3_summary_hash(holder))
                .or_default() += 1;
        }
    }

    let mut top = report.ready_reservation_pressure.iter().collect::<Vec<_>>();
    top.sort_by(|left, right| {
        recommendation_severity_rank(&right.severity)
            .cmp(&recommendation_severity_rank(&left.severity))
            .then_with(|| right.max_risk_score.cmp(&left.max_risk_score))
            .then_with(|| left.bead_id.cmp(&right.bead_id))
    });

    json!({
        "countsByAction": counts_by_action,
        "countsBySeverity": counts_by_severity,
        "countsByReservationHolder": counts_by_holder,
        "topReadyBeads": top
            .into_iter()
            .take(MAX_SWARM_BRIEF_SUMMARY_RECOMMENDATIONS)
            .map(|pressure| {
                json!({
                    "beadId": pressure.bead_id,
                    "titleHash": blake3_summary_hash(&pressure.title),
                    "priority": pressure.priority,
                    "action": pressure.action,
                    "severity": pressure.severity,
                    "reservationHolders": pressure.reservation_holders.iter().map(|value| blake3_summary_hash(value)).collect::<Vec<_>>(),
                    "exclusiveReservationCount": pressure.exclusive_reservation_count,
                    "sharedReservationCount": pressure.shared_reservation_count,
                    "earliestExpiresAt": pressure.earliest_expires_at,
                    "maxRiskScore": pressure.max_risk_score,
                    "riskFactors": pressure.risk_factors,
                    "likelySurfaceHashes": pressure.likely_surfaces.iter().map(|value| blake3_summary_hash(value)).collect::<Vec<_>>(),
                    "suggestedCommandHashes": pressure.suggested_commands.iter().map(|value| blake3_summary_hash(value)).collect::<Vec<_>>(),
                    "rawTitleIncluded": false,
                    "rawSurfacesIncluded": false,
                    "rawCommandsIncluded": false,
                })
            })
            .collect::<Vec<_>>(),
    })
}

fn swarm_brief_stalled_bead_liveness_summary(report: &SwarmBriefReport) -> Value {
    let mut counts_by_posture = BTreeMap::<String, usize>::new();
    let mut counts_by_action = BTreeMap::<String, usize>::new();
    let mut counts_by_severity = BTreeMap::<String, usize>::new();
    for liveness in &report.stalled_bead_liveness {
        *counts_by_posture
            .entry(liveness.posture.clone())
            .or_default() += 1;
        *counts_by_action.entry(liveness.action.clone()).or_default() += 1;
        *counts_by_severity
            .entry(liveness.severity.clone())
            .or_default() += 1;
    }

    let mut top = report.stalled_bead_liveness.iter().collect::<Vec<_>>();
    top.sort_by(|left, right| {
        recommendation_severity_rank(&right.severity)
            .cmp(&recommendation_severity_rank(&left.severity))
            .then_with(|| left.bead_id.cmp(&right.bead_id))
    });

    json!({
        "countsByPosture": counts_by_posture,
        "countsByAction": counts_by_action,
        "countsBySeverity": counts_by_severity,
        "topInProgressBeads": top
            .into_iter()
            .take(MAX_SWARM_BRIEF_SUMMARY_RECOMMENDATIONS)
            .map(|liveness| {
                json!({
                    "beadId": liveness.bead_id,
                    "titleHash": blake3_summary_hash(&liveness.title),
                    "assigneeHash": liveness.assignee.as_ref().map(|value| blake3_summary_hash(value)),
                    "priority": liveness.priority,
                    "posture": liveness.posture,
                    "action": liveness.action,
                    "severity": liveness.severity,
                    "lastActivityAt": liveness.last_activity_at,
                    "ageSeconds": liveness.age_seconds,
                    "evidenceSources": liveness.evidence_sources,
                    "evidenceHashes": liveness.evidence.iter().map(|value| blake3_summary_hash(value)).collect::<Vec<_>>(),
                    "suggestedCommandHashes": liveness.suggested_commands.iter().map(|value| blake3_summary_hash(value)).collect::<Vec<_>>(),
                    "rawTitleIncluded": false,
                    "rawEvidenceIncluded": false,
                    "rawCommandsIncluded": false,
                })
            })
            .collect::<Vec<_>>(),
    })
}

fn swarm_brief_symbol_risk_summary(report: &SwarmBriefReport) -> Value {
    let Some(symbol_risk) = report
        .workspace_hygiene
        .as_ref()
        .and_then(|summary| summary.symbol_risk_summary.as_ref())
    else {
        return json!({
            "schema": "ee.support_bundle.symbol_risk_summary.v1",
            "status": "not_collected",
            "dirtyPathCount": 0,
            "summarizedPathCount": 0,
            "omittedPathCount": 0,
            "touchedSymbolCount": 0,
            "highRiskSymbolCount": 0,
            "linkedEvidenceCount": 0,
            "recentAgentActivityCount": 0,
            "topPaths": [],
            "degradedCodes": [],
            "rawPathsIncluded": false,
            "rawSymbolNamesIncluded": false,
            "rawAgentNamesIncluded": false,
        });
    };

    json!({
        "schema": symbol_risk.schema,
        "status": symbol_risk.status,
        "dirtyPathCount": symbol_risk.dirty_path_count,
        "summarizedPathCount": symbol_risk.summarized_path_count,
        "omittedPathCount": symbol_risk.omitted_path_count,
        "touchedSymbolCount": symbol_risk.touched_symbol_count,
        "highRiskSymbolCount": symbol_risk.high_risk_symbol_count,
        "linkedEvidenceCount": symbol_risk.linked_evidence_count,
        "recentAgentActivityCount": symbol_risk.recent_agent_activity_count,
        "topPaths": symbol_risk.paths.iter().take(MAX_SWARM_BRIEF_SUMMARY_RECOMMENDATIONS).map(|path| {
            json!({
                "pathHash": &path.path_hash,
                "symbolCount": path.symbol_count,
                "highRiskSymbolCount": path.high_risk_symbol_count,
                "linkedEvidenceCount": path.linked_evidence_count,
                "recentAgentActivityCount": path.recent_agent_activity_count,
                "agentNameHashes": &path.agent_name_hashes,
                "evidenceSourceKinds": &path.evidence_source_kinds,
                "symbols": path.symbols.iter().take(5).map(|symbol| {
                    json!({
                        "symbolIdHash": &symbol.symbol_id_hash,
                        "canonicalNameHash": &symbol.canonical_name_hash,
                        "kind": symbol.kind,
                        "visibility": symbol.visibility,
                        "publicSurface": symbol.public_surface,
                        "startLine": symbol.start_line,
                        "endLine": symbol.end_line,
                        "linkedEvidenceCount": symbol.linked_evidence_count,
                        "evidenceSourceKinds": &symbol.evidence_source_kinds,
                    })
                }).collect::<Vec<_>>(),
                "rawPathIncluded": false,
                "rawSymbolNamesIncluded": false,
                "rawAgentNamesIncluded": false,
            })
        }).collect::<Vec<_>>(),
        "degradedCodes": &symbol_risk.degraded_codes,
        "rawPathsIncluded": false,
        "rawSymbolNamesIncluded": false,
        "rawAgentNamesIncluded": false,
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
        "critical" => 5,
        "high" => 4,
        "medium" => 3,
        "warning" => 2,
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

fn summarize_ready_reservation_pressure(
    report: &SwarmBriefReport,
) -> Vec<SwarmBriefReadyReservationPressure> {
    let clear_ready_candidate_available = report
        .beads
        .ready
        .iter()
        .any(|bead| ready_bead_has_clear_reservation_surface(report, bead));
    let mut output = report
        .beads
        .ready
        .iter()
        .filter_map(|bead| {
            ready_bead_reservation_pressure(report, bead, clear_ready_candidate_available)
        })
        .collect::<Vec<_>>();
    output.sort();
    output.dedup_by(|left, right| left.bead_id == right.bead_id);
    output
}

fn ready_bead_has_clear_reservation_surface(
    report: &SwarmBriefReport,
    bead: &SwarmBriefBead,
) -> bool {
    let surfaces = likely_surfaces_for_bead(bead);
    !surfaces.is_empty() && reservations_for_surfaces(report, &surfaces).is_empty()
}

fn ready_bead_reservation_pressure(
    report: &SwarmBriefReport,
    bead: &SwarmBriefBead,
    clear_ready_candidate_available: bool,
) -> Option<SwarmBriefReadyReservationPressure> {
    let likely_surfaces = likely_surfaces_for_bead(bead);
    let reservations = reservations_for_surfaces(report, &likely_surfaces);
    let related_risks = risks_for_surfaces(&report.file_surface_risks, &likely_surfaces);
    if likely_surfaces.is_empty() {
        return Some(unknown_ready_surface_pressure(bead));
    }
    if reservations.is_empty() {
        return None;
    }

    let exclusive_reservation_count = reservations
        .iter()
        .filter(|reservation| reservation.exclusive)
        .count();
    let shared_reservation_count = reservations
        .len()
        .saturating_sub(exclusive_reservation_count);
    let earliest_expires_at = reservations
        .iter()
        .filter_map(|reservation| reservation.expires_at.as_ref())
        .min()
        .cloned();
    let reservation_holders = reservations
        .iter()
        .map(|reservation| reservation.holder.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let max_risk_score = related_risks
        .iter()
        .map(|risk| risk.score)
        .max()
        .unwrap_or_else(|| {
            if exclusive_reservation_count > 0 {
                70
            } else {
                35
            }
        });
    let risk_factors = related_risks
        .iter()
        .flat_map(|risk| risk.risk_factors.iter().cloned())
        .chain(std::iter::once(
            "ready_bead_reservation_pressure".to_string(),
        ))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let evidence = related_risks
        .iter()
        .flat_map(|risk| risk.evidence.iter().cloned())
        .chain(reservations.iter().map(|reservation| {
            format!(
                "ready_reservation:{}:{}:{}",
                bead.id, reservation.holder, reservation.path_pattern
            )
        }))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let action = ready_reservation_pressure_action(
        exclusive_reservation_count,
        shared_reservation_count,
        earliest_expires_at.as_deref(),
        clear_ready_candidate_available,
    );
    let severity = if exclusive_reservation_count > 0 {
        "high"
    } else if shared_reservation_count > 0 {
        "medium"
    } else {
        severity_for_score(max_risk_score)
    };
    let suggested_commands = ready_reservation_pressure_commands(
        &bead.id,
        &action,
        &reservation_holders,
        &likely_surfaces,
    );

    Some(SwarmBriefReadyReservationPressure {
        bead_id: bead.id.clone(),
        title: redact_brief_text(&bead.title),
        priority: bead.priority,
        action,
        severity: severity.to_string(),
        likely_surfaces,
        reservation_holders,
        exclusive_reservation_count,
        shared_reservation_count,
        earliest_expires_at,
        max_risk_score,
        risk_factors,
        evidence,
        suggested_commands,
    })
}

fn unknown_ready_surface_pressure(bead: &SwarmBriefBead) -> SwarmBriefReadyReservationPressure {
    SwarmBriefReadyReservationPressure {
        bead_id: bead.id.clone(),
        title: redact_brief_text(&bead.title),
        priority: bead.priority,
        action: "inspect_full".to_string(),
        severity: "low".to_string(),
        likely_surfaces: Vec::new(),
        reservation_holders: Vec::new(),
        exclusive_reservation_count: 0,
        shared_reservation_count: 0,
        earliest_expires_at: None,
        max_risk_score: 0,
        risk_factors: vec!["ready_bead_surface_unknown".to_string()],
        evidence: vec![format!(
            "bead:{}:{}:{}",
            bead.id, bead.source_bucket, bead.title
        )],
        suggested_commands: vec![
            format!("br show {} --json", bead.id),
            "ee swarm brief --fields full --json".to_string(),
        ],
    }
}

fn summarize_stalled_bead_liveness(
    report: &SwarmBriefReport,
) -> Vec<SwarmBriefStalledBeadLiveness> {
    let now_epoch_seconds = i64::try_from(current_epoch_ms() / 1_000).unwrap_or(i64::MAX);
    let mut output = report
        .beads
        .in_progress
        .iter()
        .map(|bead| stalled_bead_liveness(report, bead, now_epoch_seconds))
        .collect::<Vec<_>>();
    output.sort();
    output.dedup_by(|left, right| left.bead_id == right.bead_id);
    output
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StalledBeadActivitySignal {
    source: String,
    at: Option<String>,
    epoch_seconds: Option<i64>,
    evidence: String,
}

fn stalled_bead_liveness(
    report: &SwarmBriefReport,
    bead: &SwarmBriefBead,
    now_epoch_seconds: i64,
) -> SwarmBriefStalledBeadLiveness {
    let signals = stalled_bead_activity_signals(report, bead, now_epoch_seconds);
    let latest = signals
        .iter()
        .filter_map(|signal| signal.epoch_seconds.map(|epoch| (epoch, signal.at.clone())))
        .max_by(|left, right| left.0.cmp(&right.0));
    let last_activity_at = latest.as_ref().and_then(|(_, at)| at.clone());
    let age_seconds = latest.map(|(epoch, _)| now_epoch_seconds.saturating_sub(epoch).max(0));
    let active_reservation_present = active_reservations_for_bead(report, bead, now_epoch_seconds)
        .into_iter()
        .next()
        .is_some();
    let agent_mail_ready = source_status(report, SwarmBriefSourceKind::AgentMail)
        == Some(SwarmBriefSourceStatus::Ready);
    let git_ready =
        source_status(report, SwarmBriefSourceKind::Git) == Some(SwarmBriefSourceStatus::Ready);
    let blocked_evidence = bead_text_suggests_blocked(&bead.title);
    let human_approval_required = bead_text_suggests_human_approval(&bead.title)
        || bead.status.eq_ignore_ascii_case("deferred");

    let (posture, action, severity) = if human_approval_required {
        ("human_approval_required", "request_human_approval", "high")
    } else if active_reservation_present
        || age_seconds.is_some_and(|age| age <= STALLED_BEAD_ACTIVE_WINDOW_SECONDS)
    {
        ("active", "leave_alone", "low")
    } else if blocked_evidence {
        ("blocked_with_evidence", "inspect_full", "medium")
    } else if age_seconds.is_some_and(|age| age <= STALLED_BEAD_QUIET_WINDOW_SECONDS) {
        ("quiet_but_recent", "message_holder", "low")
    } else if !agent_mail_ready || !git_ready || age_seconds.is_none() {
        ("stale_needs_message", "message_holder", "medium")
    } else {
        ("reclaim_candidate", "reopen_manually", "high")
    };

    let mut evidence_sources = signals
        .iter()
        .map(|signal| signal.source.clone())
        .collect::<BTreeSet<_>>();
    if !agent_mail_ready {
        evidence_sources.insert("agent_mail_degraded".to_string());
    }
    if !git_ready {
        evidence_sources.insert("git_degraded".to_string());
    }

    let mut evidence = signals
        .iter()
        .map(|signal| signal.evidence.clone())
        .collect::<BTreeSet<_>>();
    evidence.insert(format!("bead:{}:{}:{}", bead.id, bead.status, bead.title));
    if let Some(age) = age_seconds {
        evidence.insert(format!("last_activity_age_seconds:{age}"));
    } else {
        evidence.insert("last_activity_age_seconds:unknown".to_string());
    }
    if !agent_mail_ready {
        evidence.insert("source_status:agent_mail:not_ready".to_string());
    }
    if !git_ready {
        evidence.insert("source_status:git:not_ready".to_string());
    }
    if human_approval_required {
        evidence.insert("human_approval_required:true".to_string());
    }

    SwarmBriefStalledBeadLiveness {
        bead_id: bead.id.clone(),
        title: redact_brief_text(&bead.title),
        assignee: bead.assignee.clone(),
        priority: bead.priority,
        posture: posture.to_string(),
        action: action.to_string(),
        severity: severity.to_string(),
        last_activity_at,
        age_seconds,
        evidence_sources: evidence_sources.into_iter().collect(),
        evidence: evidence.into_iter().collect(),
        suggested_commands: stalled_bead_suggested_commands(bead, action),
        must_not_do: stalled_bead_must_not_do(agent_mail_ready, git_ready),
    }
}

fn stalled_bead_activity_signals(
    report: &SwarmBriefReport,
    bead: &SwarmBriefBead,
    now_epoch_seconds: i64,
) -> Vec<StalledBeadActivitySignal> {
    let mut signals = Vec::new();
    if let Some(updated_at) = &bead.updated_at {
        signals.push(StalledBeadActivitySignal {
            source: "beads_updated_at".to_string(),
            at: Some(updated_at.clone()),
            epoch_seconds: rfc3339_epoch_seconds(updated_at),
            evidence: format!("beads_updated_at:{}:{}", bead.id, updated_at),
        });
    }
    if let Some(comment_at) = &bead.latest_comment_at {
        signals.push(StalledBeadActivitySignal {
            source: "beads_comment".to_string(),
            at: Some(comment_at.clone()),
            epoch_seconds: rfc3339_epoch_seconds(comment_at),
            evidence: format!("beads_latest_comment_at:{}:{}", bead.id, comment_at),
        });
    }
    if bead.comment_count > 0 {
        signals.push(StalledBeadActivitySignal {
            source: "beads_comment".to_string(),
            at: None,
            epoch_seconds: None,
            evidence: format!("beads_comment_count:{}:{}", bead.id, bead.comment_count),
        });
    }

    for thread in matching_threads_for_bead(report, bead) {
        signals.push(StalledBeadActivitySignal {
            source: "agent_mail_thread".to_string(),
            at: thread.last_activity_at.clone(),
            epoch_seconds: thread
                .last_activity_at
                .as_deref()
                .and_then(rfc3339_epoch_seconds),
            evidence: format!(
                "agent_mail_thread:{}:{}",
                bead.id,
                thread
                    .last_activity_at
                    .clone()
                    .unwrap_or_else(|| "activity_unknown".to_string())
            ),
        });
    }

    for agent in matching_agent_mail_agents_for_bead(report, bead) {
        signals.push(StalledBeadActivitySignal {
            source: "agent_mail_agent".to_string(),
            at: agent.last_active_at.clone(),
            epoch_seconds: agent
                .last_active_at
                .as_deref()
                .and_then(rfc3339_epoch_seconds),
            evidence: format!(
                "agent_mail_agent_last_active:{}:{}",
                bead.id,
                agent
                    .last_active_at
                    .clone()
                    .unwrap_or_else(|| "activity_unknown".to_string())
            ),
        });
    }

    for commit in report
        .recent_commits
        .iter()
        .filter(|commit| text_mentions_bead_id(&commit.subject, &bead.id))
    {
        signals.push(StalledBeadActivitySignal {
            source: "git_commit".to_string(),
            at: None,
            epoch_seconds: commit.authored_at_epoch_seconds,
            evidence: format!("git_commit_mentions:{}:{}", bead.id, commit.hash),
        });
    }

    for reservation in matching_reservations_for_bead(report, bead) {
        let active = reservation_is_active(reservation, now_epoch_seconds);
        signals.push(StalledBeadActivitySignal {
            source: "agent_mail_reservation".to_string(),
            at: reservation.expires_at.clone(),
            epoch_seconds: reservation
                .expires_at
                .as_deref()
                .and_then(rfc3339_epoch_seconds),
            evidence: format!(
                "agent_mail_reservation:{}:{}:{}",
                bead.id,
                reservation.holder,
                if active {
                    "active"
                } else {
                    "expired_or_unknown"
                }
            ),
        });
    }

    signals.sort_by(|left, right| {
        left.source
            .cmp(&right.source)
            .then_with(|| left.at.cmp(&right.at))
            .then_with(|| left.evidence.cmp(&right.evidence))
    });
    signals.dedup_by(|left, right| left.evidence == right.evidence);
    signals
}

fn matching_threads_for_bead<'a>(
    report: &'a SwarmBriefReport,
    bead: &SwarmBriefBead,
) -> Vec<&'a SwarmBriefThreadSummary> {
    let mut threads = report
        .threads
        .iter()
        .filter(|thread| {
            text_mentions_bead_id(&thread.thread_id, &bead.id)
                || thread
                    .subject
                    .as_ref()
                    .is_some_and(|subject| text_mentions_bead_id(subject, &bead.id))
        })
        .collect::<Vec<_>>();
    threads.sort();
    threads.dedup();
    threads
}

fn matching_agent_mail_agents_for_bead<'a>(
    report: &'a SwarmBriefReport,
    bead: &SwarmBriefBead,
) -> Vec<&'a SwarmBriefAgentMailAgent> {
    let Some(assignee) = bead.assignee.as_ref() else {
        return Vec::new();
    };
    let mut agents = report
        .agent_mail_agents
        .iter()
        .filter(|agent| agent.name == *assignee)
        .collect::<Vec<_>>();
    agents.sort();
    agents.dedup();
    agents
}

fn matching_reservations_for_bead<'a>(
    report: &'a SwarmBriefReport,
    bead: &SwarmBriefBead,
) -> Vec<&'a SwarmBriefFileReservation> {
    let likely_surfaces = likely_surfaces_for_bead(bead);
    let mut reservations = report
        .file_reservations
        .iter()
        .filter(|reservation| {
            bead.assignee
                .as_ref()
                .is_some_and(|assignee| reservation.holder == *assignee)
                || likely_surfaces
                    .iter()
                    .any(|surface| surfaces_overlap(&reservation.path_pattern, surface))
        })
        .collect::<Vec<_>>();
    reservations.sort();
    reservations.dedup();
    reservations
}

fn active_reservations_for_bead<'a>(
    report: &'a SwarmBriefReport,
    bead: &SwarmBriefBead,
    now_epoch_seconds: i64,
) -> Vec<&'a SwarmBriefFileReservation> {
    matching_reservations_for_bead(report, bead)
        .into_iter()
        .filter(|reservation| reservation_is_active(reservation, now_epoch_seconds))
        .collect()
}

fn reservation_is_active(reservation: &SwarmBriefFileReservation, now_epoch_seconds: i64) -> bool {
    reservation
        .expires_at
        .as_deref()
        .and_then(rfc3339_epoch_seconds)
        .is_none_or(|expires_at| expires_at > now_epoch_seconds)
}

fn stalled_bead_suggested_commands(bead: &SwarmBriefBead, action: &str) -> Vec<String> {
    let mut commands = BTreeSet::from([
        format!("br show {} --json", bead.id),
        format!("Search Agent Mail for thread {}", bead.id),
    ]);
    if let Some(assignee) = &bead.assignee {
        commands.insert(format!("message {assignee} before reclaiming {}", bead.id));
    }
    match action {
        "reopen_manually" => {
            commands.insert(format!("br update {} --status open --json", bead.id));
        }
        "request_human_approval" => {
            commands.insert("Ask the human/operator for explicit approval before reopening or deleting anything.".to_string());
        }
        "inspect_full" => {
            commands.insert("ee --fields full swarm brief --workspace . --json".to_string());
        }
        _ => {}
    }
    commands.into_iter().collect()
}

fn stalled_bead_must_not_do(agent_mail_ready: bool, git_ready: bool) -> Vec<String> {
    let mut must_not_do = vec![
        "Do not auto-reopen in-progress work from swarm brief output.".to_string(),
        "Do not force-release reservations from liveness guidance alone.".to_string(),
        "Do not treat deferred or human-approval work as abandoned.".to_string(),
    ];
    if !agent_mail_ready {
        must_not_do.push("Do not treat missing Agent Mail data as inactivity proof.".to_string());
    }
    if !git_ready {
        must_not_do.push("Do not treat missing git history as inactivity proof.".to_string());
    }
    must_not_do
}

fn bead_text_suggests_blocked(text: &str) -> bool {
    let normalized = text.to_ascii_lowercase();
    ["blocked", "waiting", "stalled", "needs approval"]
        .iter()
        .any(|needle| normalized.contains(needle))
}

fn bead_text_suggests_human_approval(text: &str) -> bool {
    let normalized = text.to_ascii_lowercase();
    [
        "human approval",
        "approval required",
        "deletion approval",
        "delete approval",
        "do not delete",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn text_mentions_bead_id(text: &str, bead_id: &str) -> bool {
    text.contains(bead_id)
}

fn reservations_for_surfaces<'a>(
    report: &'a SwarmBriefReport,
    surfaces: &[String],
) -> Vec<&'a SwarmBriefFileReservation> {
    let mut reservations = report
        .file_reservations
        .iter()
        .filter(|reservation| {
            surfaces
                .iter()
                .any(|surface| surfaces_overlap(&reservation.path_pattern, surface))
        })
        .collect::<Vec<_>>();
    reservations.sort();
    reservations.dedup_by(|left, right| {
        left.path_pattern == right.path_pattern
            && left.holder == right.holder
            && left.exclusive == right.exclusive
    });
    reservations
}

fn ready_reservation_pressure_action(
    exclusive_reservation_count: usize,
    shared_reservation_count: usize,
    earliest_expires_at: Option<&str>,
    clear_ready_candidate_available: bool,
) -> String {
    if exclusive_reservation_count > 0 && clear_ready_candidate_available {
        "choose_another"
    } else if exclusive_reservation_count > 0 && earliest_expires_at.is_some() {
        "wait"
    } else if exclusive_reservation_count > 0 || shared_reservation_count > 0 {
        "message_holder"
    } else {
        "inspect_full"
    }
    .to_string()
}

fn ready_reservation_pressure_commands(
    bead_id: &str,
    action: &str,
    reservation_holders: &[String],
    likely_surfaces: &[String],
) -> Vec<String> {
    let mut commands = BTreeSet::from([format!("br show {bead_id} --json")]);
    match action {
        "choose_another" => {
            commands.insert(BEADS_READY_COMMAND.to_string());
        }
        "message_holder" | "wait" => {
            if reservation_holders.is_empty() {
                commands.insert("Inspect Agent Mail reservations before editing.".to_string());
            } else {
                let surface = likely_surfaces
                    .first()
                    .map(String::as_str)
                    .unwrap_or("the likely surface");
                commands.insert(format!(
                    "message {} before editing {surface}",
                    reservation_holders.join(",")
                ));
            }
        }
        _ => {
            commands.insert("ee swarm brief --fields full --json".to_string());
        }
    }
    commands.into_iter().collect()
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
    recommendations.extend(git_operation_state_recommendations(report));
    recommendations.extend(git_ahead_recommendations(report));
    recommendations.extend(surface_conflict_recommendations(report));
    recommendations.extend(memory_drift_recommendations(report));
    recommendations.extend(verification_broker_recommendations(report));

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

fn memory_drift_recommendations(report: &SwarmBriefReport) -> Vec<SwarmBriefRecommendation> {
    let Some(summary) = &report.memory_drift else {
        return Vec::new();
    };
    if summary.affected_count == 0 {
        return Vec::new();
    }

    let mut reason_codes = BTreeSet::from(["memory_drift_queue_non_empty".to_string()]);
    reason_codes.extend(summary.degraded_codes.iter().cloned());
    if summary.changed_count > 0 {
        reason_codes.insert("memory_drift_changed_sources_present".to_string());
    }
    if summary.missing_source_count > 0 {
        reason_codes.insert("memory_drift_missing_sources_present".to_string());
    }
    if summary.stale_anchor_count > 0 {
        reason_codes.insert("memory_drift_stale_anchors_present".to_string());
    }
    if summary.unverifiable_count > 0 {
        reason_codes.insert("memory_drift_unverifiable_sources_present".to_string());
    }

    let mut evidence = BTreeSet::from([
        format!("memory_drift_affected:{}", summary.affected_count),
        format!("memory_drift_changed:{}", summary.changed_count),
        format!(
            "memory_drift_missing_source:{}",
            summary.missing_source_count
        ),
        format!("memory_drift_stale_anchor:{}", summary.stale_anchor_count),
        format!("memory_drift_unverifiable:{}", summary.unverifiable_count),
    ]);
    for memory_id in &summary.top_affected_memory_ids {
        evidence.insert(format!("memory_drift_top_affected:{memory_id}"));
    }

    let severity = if summary.missing_source_count > 0 {
        "high"
    } else if summary.changed_count > 0 || summary.unverifiable_count > 0 {
        "medium"
    } else {
        "low"
    };

    vec![SwarmBriefRecommendation {
        id: "rec.memory_drift.revalidate_recent_pack_items".to_string(),
        kind: "memory_drift_revalidation".to_string(),
        confidence: coordination_confidence(report),
        severity: severity.to_string(),
        reason_codes: reason_codes.into_iter().collect(),
        evidence: evidence.into_iter().collect(),
        suggested_commands: vec![
            default_source_repair(SwarmBriefSourceKind::MemoryDrift).to_string(),
            "ee why --help".to_string(),
        ],
        must_not_do: vec![
            "Do not rely on stale memory provenance without revalidation.".to_string(),
            "Do not include raw source snippets or command output bodies in swarm brief summaries."
                .to_string(),
        ],
    }]
}

fn verification_broker_recommendations(report: &SwarmBriefReport) -> Vec<SwarmBriefRecommendation> {
    let Some(summary) = &report.verification_broker else {
        return Vec::new();
    };

    let mut recommendations = Vec::new();
    if summary.recent_reusable_run_count > 0 {
        let mut reason_codes = verification_broker_base_reason_codes(summary);
        reason_codes.insert("verification_recent_reusable_run".to_string());
        reason_codes.extend(verification_broker_rch_pressure_reason_codes(summary));

        let severity = if verification_broker_rch_pressure_present(summary) {
            "medium"
        } else {
            "low"
        };

        recommendations.push(SwarmBriefRecommendation {
            id: "rec.verification_broker.reuse_recent_evidence".to_string(),
            kind: "verification_reuse".to_string(),
            confidence: coordination_confidence(report),
            severity: severity.to_string(),
            reason_codes: reason_codes.into_iter().collect(),
            evidence: verification_broker_evidence(summary),
            suggested_commands: vec![
                "ee verify broker lookup --runs-jsonl <j1.jsonl> --command-hash <hash> --source-hash <hash> --execution-substrate rch --json".to_string(),
                "ee verify closeout capsule --runs-jsonl <j1.jsonl> --run-id <run-id> --json".to_string(),
            ],
            must_not_do: vec![
                "Do not spend a fresh RCH slot before checking reusable verification evidence."
                    .to_string(),
                "Do not paste raw stdout/stderr or mail bodies into swarm brief evidence."
                    .to_string(),
            ],
        });
    }

    let known_blockers = verification_broker_known_blocker_count(summary);
    if known_blockers > 0 {
        let mut reason_codes = verification_broker_base_reason_codes(summary);
        reason_codes.insert("verification_known_blocker_present".to_string());
        if summary.advisory_counts.remote_failed > 0 {
            reason_codes.insert("verification_remote_failed".to_string());
        }
        if summary.advisory_counts.topology_blocked > 0 {
            reason_codes.insert("verification_topology_blocked".to_string());
        }
        if summary.advisory_counts.local_disallowed > 0 {
            reason_codes.insert("verification_local_disallowed".to_string());
        }
        reason_codes.extend(verification_broker_rch_pressure_reason_codes(summary));

        let mut evidence = verification_broker_evidence(summary);
        evidence.push(format!("verification_known_blockers:{known_blockers}"));
        evidence.sort();
        evidence.dedup();

        recommendations.push(SwarmBriefRecommendation {
            id: "rec.verification_broker.inspect_known_blocker".to_string(),
            kind: "verification_known_blocker".to_string(),
            confidence: coordination_confidence(report),
            severity: "high".to_string(),
            reason_codes: reason_codes.into_iter().collect(),
            evidence,
            suggested_commands: vec![
                "ee verify broker lookup --runs-jsonl <j1.jsonl> --command-hash <hash> --source-hash <hash> --execution-substrate rch --json".to_string(),
                "ee swarm brief --include-rch --json".to_string(),
            ],
            must_not_do: vec![
                "Do not launch broad RCH verification until the known blocker is inspected or coordinated.".to_string(),
                "Do not count local Cargo fallback as remote proof.".to_string(),
            ],
        });
    }

    if summary.in_flight_equivalent_command_count > 0 {
        let mut reason_codes = verification_broker_base_reason_codes(summary);
        reason_codes.insert("verification_in_flight_equivalent_command".to_string());

        recommendations.push(SwarmBriefRecommendation {
            id: "rec.verification_broker.wait_for_in_flight_run".to_string(),
            kind: "verification_in_flight".to_string(),
            confidence: coordination_confidence(report),
            severity: "medium".to_string(),
            reason_codes: reason_codes.into_iter().collect(),
            evidence: verification_broker_evidence(summary),
            suggested_commands: vec![
                "br list --status in_progress --json".to_string(),
                "Search Agent Mail for the active verification owner before duplicating the run."
                    .to_string(),
            ],
            must_not_do: vec![
                "Do not start a duplicate remote-required Cargo gate while equivalent verification is in flight.".to_string(),
            ],
        });
    }

    recommendations
}

fn verification_broker_known_blocker_count(summary: &SwarmBriefVerificationBrokerSummary) -> u32 {
    summary
        .advisory_counts
        .remote_failed
        .saturating_add(summary.advisory_counts.local_disallowed)
        .saturating_add(summary.advisory_counts.topology_blocked)
}

fn verification_broker_base_reason_codes(
    summary: &SwarmBriefVerificationBrokerSummary,
) -> BTreeSet<String> {
    BTreeSet::from([
        "verification_broker_posture_available".to_string(),
        format!("verification_broker_status:{}", summary.status),
    ])
}

fn verification_broker_rch_pressure_reason_codes(
    summary: &SwarmBriefVerificationBrokerSummary,
) -> BTreeSet<String> {
    let mut reason_codes = BTreeSet::new();
    match summary.rch_queue_status.as_str() {
        "capacity_blocked" => {
            reason_codes.insert("rch_queue_capacity_blocked".to_string());
        }
        "start_stalled" => {
            reason_codes.insert("rch_queue_start_stalled".to_string());
        }
        "queued" => {
            reason_codes.insert("rch_queue_non_empty".to_string());
        }
        _ => {}
    }
    if summary.rch_worker_pressure_status != "not_collected"
        && summary.rch_worker_pressure_status != "clear"
        && summary.rch_worker_pressure_status != "healthy"
    {
        reason_codes.insert(format!(
            "rch_worker_pressure:{}",
            summary.rch_worker_pressure_status
        ));
    }
    reason_codes
}

fn verification_broker_rch_pressure_present(summary: &SwarmBriefVerificationBrokerSummary) -> bool {
    !verification_broker_rch_pressure_reason_codes(summary).is_empty()
}

fn verification_broker_evidence(summary: &SwarmBriefVerificationBrokerSummary) -> Vec<String> {
    let mut evidence = BTreeSet::from([
        format!("verification_records:{}", summary.record_count),
        format!(
            "verification_recent_reusable_runs:{}",
            summary.recent_reusable_run_count
        ),
        format!(
            "verification_in_flight:{}",
            summary.in_flight_equivalent_command_count
        ),
        format!(
            "verification_remote_failed:{}",
            summary.advisory_counts.remote_failed
        ),
        format!(
            "verification_topology_blocked:{}",
            summary.advisory_counts.topology_blocked
        ),
        format!(
            "verification_local_disallowed:{}",
            summary.advisory_counts.local_disallowed
        ),
        format!("rch_queue_status:{}", summary.rch_queue_status),
        format!(
            "rch_worker_pressure_status:{}",
            summary.rch_worker_pressure_status
        ),
    ]);
    if let Some(slots_available) = summary.rch_slots_available {
        evidence.insert(format!("rch_slots_available:{slots_available}"));
    }
    if let Some(slots_needed) = summary.rch_queue_head_slots_needed {
        evidence.insert(format!("rch_queue_head_slots_needed:{slots_needed}"));
    }
    evidence.into_iter().collect()
}

fn git_ahead_recommendations(report: &SwarmBriefReport) -> Vec<SwarmBriefRecommendation> {
    let Some(snapshot) = &report.git_ahead else {
        return Vec::new();
    };
    if !snapshot.peer_owned_ahead_risk {
        return Vec::new();
    }

    let mut reason_codes = BTreeSet::from([
        "git_ahead_peer_owned_risk".to_string(),
        format!("git_ahead_state:{}", snapshot.state),
    ]);
    if snapshot.mixed_author_ahead {
        reason_codes.insert("git_ahead_mixed_author".to_string());
    }
    if snapshot.mixed_bead_ahead {
        reason_codes.insert("git_ahead_mixed_bead".to_string());
    }
    if snapshot.ambiguous_ahead {
        reason_codes.insert("git_ahead_ambiguous".to_string());
    }
    reason_codes.extend(
        snapshot
            .degraded
            .iter()
            .map(|entry| format!("git_ahead_degraded:{}", entry.code)),
    );

    vec![SwarmBriefRecommendation {
        id: "rec.git.coordinate_mixed_owner_ahead".to_string(),
        kind: "push_safety".to_string(),
        confidence: coordination_confidence(report),
        severity: git_ahead_recommendation_severity(snapshot).to_string(),
        reason_codes: reason_codes.into_iter().collect(),
        evidence: git_ahead_recommendation_evidence(snapshot),
        suggested_commands: vec![
            "Coordinate with peers before pushing mixed-owner, mixed-bead, or ambiguous ahead commits.".to_string(),
            "git log origin/main..HEAD --oneline --decorate".to_string(),
        ],
        must_not_do: vec![
            "Do not automatically push when ahead commits may include peer-owned work.".to_string(),
            "Do not rewrite, rebase, reset, or squash ahead commits to make the warning disappear.".to_string(),
        ],
    }]
}

fn git_ahead_recommendation_severity(snapshot: &GitAheadSnapshot) -> &'static str {
    if snapshot.mixed_author_ahead || snapshot.mixed_bead_ahead {
        "high"
    } else {
        "medium"
    }
}

fn git_ahead_recommendation_evidence(snapshot: &GitAheadSnapshot) -> Vec<String> {
    let mut evidence = BTreeSet::from([
        format!("git_ahead_state:{}", snapshot.state),
        format!("git_ahead_count:{}", snapshot.ahead_count),
        format!("git_ahead_commit_count:{}", snapshot.commits.len()),
        format!("git_ahead_author_count:{}", snapshot.authors.len()),
        format!("git_ahead_bead_ref_count:{}", snapshot.bead_refs.len()),
    ]);
    if let Some(upstream) = snapshot.upstream_ref.as_deref() {
        evidence.insert(format!("git_ahead_upstream:{upstream}"));
    }
    if snapshot.mixed_author_ahead {
        evidence.insert("git_ahead_mixed_author:true".to_string());
    }
    if snapshot.mixed_bead_ahead {
        evidence.insert("git_ahead_mixed_bead:true".to_string());
    }
    if snapshot.ambiguous_ahead {
        evidence.insert("git_ahead_ambiguous:true".to_string());
    }
    evidence.extend(
        snapshot
            .degraded
            .iter()
            .map(|entry| format!("git_ahead_degraded:{}", entry.code)),
    );
    evidence.into_iter().collect()
}

fn git_operation_state_recommendations(report: &SwarmBriefReport) -> Vec<SwarmBriefRecommendation> {
    if report.git_operation_state.is_clean() {
        return Vec::new();
    }

    let mut reason_codes = BTreeSet::from(["git_operation_in_progress".to_string()]);
    let mut evidence = BTreeSet::new();
    for marker in &report.git_operation_state.operations {
        reason_codes.insert(format!("git_operation:{}", marker.operation));
        evidence.insert(format!(
            "git_operation_marker:{}:{}:{}",
            marker.operation, marker.marker_path, marker.marker_type
        ));
    }
    for marker in &report.git_operation_state.autostash_markers {
        reason_codes.insert("git_autostash_marker_present".to_string());
        evidence.insert(format!(
            "git_autostash_marker:{}:{}",
            marker.marker_path, marker.marker_type
        ));
    }

    vec![SwarmBriefRecommendation {
        id: "rec.git.operation_in_progress".to_string(),
        kind: "git_operation_state".to_string(),
        confidence: "high".to_string(),
        severity: if report.git_operation_state.autostash_markers.is_empty() {
            "high".to_string()
        } else {
            "critical".to_string()
        },
        reason_codes: reason_codes.into_iter().collect(),
        evidence: evidence.into_iter().collect(),
        suggested_commands: vec![
            "git status".to_string(),
            "Ask the human/operator for explicit Git operation recovery direction.".to_string(),
        ],
        must_not_do: vec![
            "Do not run git rebase --continue, --abort, or --quit without explicit operator direction."
                .to_string(),
            "Do not apply, pop, drop, or create a stash while an operation/autostash marker is present."
                .to_string(),
            "Do not stage or commit over unresolved Git operation metadata without coordination."
                .to_string(),
        ],
    }]
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
    } else if source == SwarmBriefSourceKind::Bv
        && matches!(code, BV_COMMAND_TIMEOUT_CODE | BV_NO_OUTPUT_CODE)
    {
        must_not_do.push(
            "Do not wait on raw bv --robot-* commands without an explicit timeout; use ee swarm brief/work-packet or a bounded retry."
                .to_string(),
        );
        must_not_do.push(
            "Do not use BV copy-paste claim guidance while graph-triage liveness is degraded; require direct br evidence and a safe claim gate."
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
    if lower.contains("[docs]")
        || lower.contains("docs")
        || lower.contains("readme")
        || lower.contains("document")
    {
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

fn expected_sources() -> [SwarmBriefSourceKind; 9] {
    [
        SwarmBriefSourceKind::AgentInventory,
        SwarmBriefSourceKind::AgentMail,
        SwarmBriefSourceKind::Beads,
        SwarmBriefSourceKind::Bv,
        SwarmBriefSourceKind::Git,
        SwarmBriefSourceKind::HostProfile,
        SwarmBriefSourceKind::MemoryDrift,
        SwarmBriefSourceKind::Rch,
        SwarmBriefSourceKind::Toolchain,
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
        SwarmBriefSourceKind::Beads => BEADS_READY_COMMAND,
        SwarmBriefSourceKind::Bv => "bv --robot-triage --robot-triage-by-track",
        SwarmBriefSourceKind::Git => "git status --short --branch --untracked-files=all",
        SwarmBriefSourceKind::HostProfile => "ee profile probe --json",
        SwarmBriefSourceKind::MemoryDrift => "ee memory drift --mode recent-pack-items --json",
        SwarmBriefSourceKind::Qos => "ee status --json | jq .data.qos",
        SwarmBriefSourceKind::Rch => "rch status --json",
        SwarmBriefSourceKind::Toolchain => "ee diag toolchain-provenance --json",
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
        SwarmBriefSourceKind::MemoryDrift => "recent pack memory drift posture",
        SwarmBriefSourceKind::Qos => "foreground/background active-lane pressure",
        SwarmBriefSourceKind::Rch => "remote build queue and active build pressure",
        SwarmBriefSourceKind::Toolchain => "local toolchain provenance and freshness",
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
pub fn parse_workspace_git_status_porcelain_v2(input: &str) -> Vec<WorkspaceGitStatusEntry> {
    let mut entries = input
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(parse_workspace_git_status_porcelain_v2_line)
        .collect::<Vec<_>>();
    entries.sort();
    entries.dedup();
    entries
}

fn parse_workspace_git_status_porcelain_v2_line(line: &str) -> Option<WorkspaceGitStatusEntry> {
    if let Some(rest) = line.strip_prefix("1 ") {
        let mut parts = rest.splitn(8, ' ');
        let xy = parts.next()?;
        let submodule = parts.next()?;
        let _head_mode = parts.next()?;
        let _index_mode = parts.next()?;
        let _worktree_mode = parts.next()?;
        let _head_hash = parts.next()?;
        let _index_hash = parts.next()?;
        let path = normalize_workspace_git_path(parts.next()?)?;
        return workspace_git_status_entry("ordinary", xy, submodule, path, None);
    }

    if let Some(rest) = line.strip_prefix("2 ") {
        let mut parts = rest.splitn(9, ' ');
        let xy = parts.next()?;
        let submodule = parts.next()?;
        let _head_mode = parts.next()?;
        let _index_mode = parts.next()?;
        let _worktree_mode = parts.next()?;
        let _head_hash = parts.next()?;
        let _index_hash = parts.next()?;
        let _rename_score = parts.next()?;
        let paths = parts.next()?;
        let (path, original_path) = paths
            .split_once('\t')
            .map_or((paths, None), |(destination, source)| {
                (destination, Some(source))
            });
        let path = normalize_workspace_git_path(path)?;
        let original_path = original_path.and_then(normalize_workspace_git_path);
        return workspace_git_status_entry("renamed_or_copied", xy, submodule, path, original_path);
    }

    if let Some(rest) = line.strip_prefix("u ") {
        let mut parts = rest.splitn(10, ' ');
        let xy = parts.next()?;
        let submodule = parts.next()?;
        let _stage1_mode = parts.next()?;
        let _stage2_mode = parts.next()?;
        let _stage3_mode = parts.next()?;
        let _worktree_mode = parts.next()?;
        let _stage1_hash = parts.next()?;
        let _stage2_hash = parts.next()?;
        let _stage3_hash = parts.next()?;
        let path = normalize_workspace_git_path(parts.next()?)?;
        return workspace_git_status_entry("unmerged", xy, submodule, path, None);
    }

    if let Some(path) = line.strip_prefix("? ") {
        let path = normalize_workspace_git_path(path)?;
        return workspace_git_status_entry("untracked", "??", "N...", path, None);
    }

    None
}

fn workspace_git_status_entry(
    entry_kind: &str,
    xy: &str,
    submodule: &str,
    path: String,
    original_path: Option<String>,
) -> Option<WorkspaceGitStatusEntry> {
    let mut chars = xy.chars();
    let staged = chars.next()?.to_string();
    let unstaged = chars.next()?.to_string();
    Some(WorkspaceGitStatusEntry {
        path,
        original_path,
        staged,
        unstaged,
        entry_kind: entry_kind.to_string(),
        submodule_state: workspace_git_submodule_state(submodule),
        metadata: None,
    })
}

fn workspace_git_submodule_state(raw: &str) -> Option<WorkspaceGitSubmoduleState> {
    if raw == "N..." || raw.is_empty() {
        return None;
    }
    let mut chars = raw.chars();
    if chars.next()? != 'S' {
        return None;
    }
    let commit = chars.next();
    let tracked = chars.next();
    let untracked = chars.next();
    Some(WorkspaceGitSubmoduleState {
        raw: raw.to_string(),
        commit_changed: commit == Some('C'),
        tracked_changes: tracked == Some('M'),
        untracked_changes: untracked == Some('U'),
    })
}

fn normalize_workspace_git_path(path: &str) -> Option<String> {
    let unquoted = unquote_git_path(path)?;
    let path = Path::new(&unquoted);
    if path.is_absolute() {
        return None;
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }
    Some(redact_path_label(path))
}

fn unquote_git_path(path: &str) -> Option<String> {
    let quoted = path
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'));
    let Some(quoted) = quoted else {
        return (!path.is_empty()).then(|| path.to_string());
    };

    let mut unquoted = Vec::with_capacity(quoted.len());
    let mut chars = quoted.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            let mut encoded = [0; 4];
            unquoted.extend_from_slice(ch.encode_utf8(&mut encoded).as_bytes());
            continue;
        }
        let escaped = chars.next()?;
        match escaped {
            '\\' => unquoted.push(b'\\'),
            '"' => unquoted.push(b'"'),
            'n' => unquoted.push(b'\n'),
            'r' => unquoted.push(b'\r'),
            't' => unquoted.push(b'\t'),
            'a' => unquoted.push(0x07),
            'b' => unquoted.push(0x08),
            'f' => unquoted.push(0x0c),
            'v' => unquoted.push(0x0b),
            '0'..='7' => {
                let mut value = escaped.to_digit(8)?;
                for _ in 0..2 {
                    let Some(next) = chars.clone().next() else {
                        break;
                    };
                    let Some(digit) = next.to_digit(8) else {
                        break;
                    };
                    chars.next();
                    value = (value * 8) + digit;
                }
                let byte = u8::try_from(value).ok()?;
                unquoted.push(byte);
            }
            _ => return None,
        }
    }
    if unquoted.is_empty() {
        return None;
    }
    String::from_utf8(unquoted).ok()
}

fn attach_workspace_git_metadata(
    entries: &mut [WorkspaceGitStatusEntry],
    repository_root: &Path,
    large_file_threshold_bytes: u64,
) {
    for entry in entries {
        entry.metadata = Some(workspace_git_path_metadata(
            repository_root,
            &entry.path,
            large_file_threshold_bytes,
        ));
    }
}

fn workspace_git_path_metadata(
    repository_root: &Path,
    path: &str,
    large_file_threshold_bytes: u64,
) -> WorkspaceGitPathMetadata {
    let full_path = repository_root.join(path);
    let metadata = match fs::symlink_metadata(&full_path) {
        Ok(metadata) => metadata,
        Err(_) => {
            return WorkspaceGitPathMetadata {
                exists: false,
                file_type: "missing".to_string(),
                size_bytes: None,
                large_file: false,
                skip_reason: Some("metadata_unavailable".to_string()),
            };
        }
    };
    let file_type = metadata.file_type();
    let kind = if file_type.is_symlink() {
        "symlink"
    } else if metadata.is_dir() {
        "directory"
    } else if metadata.is_file() {
        "file"
    } else {
        "other"
    };
    let size_bytes = metadata.is_file().then_some(metadata.len());
    let large_file = size_bytes.is_some_and(|bytes| bytes > large_file_threshold_bytes);
    WorkspaceGitPathMetadata {
        exists: true,
        file_type: kind.to_string(),
        size_bytes: if large_file { None } else { size_bytes },
        large_file,
        skip_reason: large_file.then(|| "large_file_metadata_only".to_string()),
    }
}

#[must_use]
pub fn collect_workspace_git_operation_state(repository_root: &Path) -> WorkspaceGitOperationState {
    let git_dir = repository_root.join(".git");
    let mut operations = [
        ("rebase", "rebase-merge"),
        ("rebase", "rebase-apply"),
        ("merge", "MERGE_HEAD"),
        ("cherry_pick", "CHERRY_PICK_HEAD"),
        ("revert", "REVERT_HEAD"),
        ("bisect", "BISECT_LOG"),
    ]
    .into_iter()
    .filter_map(|(operation, marker_path)| git_operation_marker(&git_dir, operation, marker_path))
    .collect::<Vec<_>>();
    operations.sort();
    operations.dedup();

    let mut autostash_markers = [
        ("autostash", "rebase-merge/autostash"),
        ("autostash", "rebase-apply/autostash"),
        ("autostash", "MERGE_AUTOSTASH"),
    ]
    .into_iter()
    .filter_map(|(operation, marker_path)| git_operation_marker(&git_dir, operation, marker_path))
    .collect::<Vec<_>>();
    autostash_markers.sort();
    autostash_markers.dedup();

    WorkspaceGitOperationState {
        in_progress: !operations.is_empty(),
        operations,
        autostash_markers,
    }
}

fn git_operation_marker(
    git_dir: &Path,
    operation: &'static str,
    marker_path: &'static str,
) -> Option<WorkspaceGitOperationMarker> {
    let metadata = fs::symlink_metadata(git_dir.join(marker_path)).ok()?;
    let file_type = metadata.file_type();
    let marker_type = if file_type.is_symlink() {
        "symlink"
    } else if metadata.is_dir() {
        "directory"
    } else if metadata.is_file() {
        "file"
    } else {
        "other"
    };

    Some(WorkspaceGitOperationMarker {
        operation,
        marker_path,
        marker_type: marker_type.to_string(),
    })
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
    dirty_count: Option<u64>,
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
        dirty_count: value
            .get("dirty_count")
            .or_else(|| value.get("dirtyCount"))
            .and_then(Value::as_u64),
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
    let issue_type = string_field(item, &["issue_type", "issueType"]);
    let comment_count = item
        .get("comment_count")
        .or_else(|| item.get("commentCount"))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| {
            item.get("comments")
                .and_then(Value::as_array)
                .map_or(0, |comments| comments.len() as u64)
        });
    Some(SwarmBriefBead {
        id: redact_brief_text(&id),
        title: redact_brief_text(&title),
        status: redact_brief_text(&status),
        priority,
        assignee: assignee.map(|value| redact_brief_text(&value)),
        issue_type: issue_type.map(|value| redact_brief_text(&value)),
        created_at: string_field(item, &["created_at", "createdAt"]),
        updated_at: string_field(item, &["updated_at", "updatedAt"]),
        latest_comment_at: latest_bead_comment_timestamp(item),
        comment_count,
        source_bucket: source_bucket.to_string(),
    })
}

fn latest_bead_comment_timestamp(item: &Value) -> Option<String> {
    let comments = item.get("comments").and_then(Value::as_array)?;
    comments
        .iter()
        .filter_map(|comment| {
            string_field(
                comment,
                &["created_at", "createdAt", "updated_at", "updatedAt"],
            )
        })
        .filter_map(|timestamp| rfc3339_epoch_seconds(&timestamp).map(|epoch| (epoch, timestamp)))
        .max_by(|left, right| left.0.cmp(&right.0))
        .map(|(_, timestamp)| timestamp)
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
    if let Some(recommendations) = value
        .pointer("/triage/recommendations")
        .or_else(|| value.get("recommendations"))
        .and_then(Value::as_array)
    {
        top_picks.extend(recommendations.iter().filter_map(parse_bv_pick));
    }
    top_picks.sort_by(|left, right| {
        right
            .score_milli
            .cmp(&left.score_milli)
            .then_with(|| right.blocked_by.len().cmp(&left.blocked_by.len()))
            .then_with(|| right.action_hint.is_some().cmp(&left.action_hint.is_some()))
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
        action_hint: string_field(item, &["action", "action_hint", "actionHint"])
            .map(|value| redact_brief_text(&value)),
        blocked_by: string_array_field(item, &["blocked_by", "blockedBy"]),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentMailSnapshotV1Wire {
    schema: String,
    generated_at: String,
    project_key: String,
    agent_name: String,
    redaction_status: String,
    producer_status: String,
    source_commands: Vec<String>,
    command_statuses: Vec<AgentMailSnapshotV1CommandStatus>,
    fallback_active: bool,
    am_agents_list_ok: bool,
    health_level: Option<String>,
    semantic_readiness: Option<AgentMailSnapshotV1SemanticReadiness>,
    durability_state: Option<String>,
    recovery: Option<AgentMailSnapshotV1Recovery>,
    summary: AgentMailSnapshotV1Summary,
    degraded: Vec<AgentMailSnapshotV1Degradation>,
    file_reservations: Vec<AgentMailSnapshotV1FileReservation>,
    agents: Vec<AgentMailSnapshotV1Agent>,
    inbox: Vec<AgentMailSnapshotV1Inbox>,
    threads: Vec<AgentMailSnapshotV1Thread>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentMailSnapshotV1CommandStatus {
    command: String,
    ok: bool,
    exit_code: Option<i64>,
    timed_out: bool,
    error_class: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentMailSnapshotV1SemanticReadiness {
    status: String,
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentMailSnapshotV1Recovery {
    mode: String,
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentMailSnapshotV1Summary {
    agent_count: u64,
    file_reservation_count: u64,
    inbox_mailbox_count: u64,
    thread_count: u64,
    source_command_count: u64,
    degraded_count: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentMailSnapshotV1Degradation {
    code: String,
    severity: String,
    source: String,
    command: String,
    error_class: Option<String>,
    exit_code: Option<i64>,
    timed_out: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentMailSnapshotV1FileReservation {
    path_pattern: String,
    holder: String,
    exclusive: bool,
    expires_ts: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentMailSnapshotV1Agent {
    name: String,
    last_active_ts: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentMailSnapshotV1Inbox {
    mailbox: String,
    unread_count: u64,
    ack_required_count: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentMailSnapshotV1Thread {
    thread_id: String,
    message_count: u64,
    subject: Option<String>,
    last_activity_at: Option<String>,
}

fn agent_mail_snapshot_v1_require_fields(
    value: &Value,
    required: &[&str],
    context: &str,
) -> Result<(), String> {
    let object = value.as_object().ok_or_else(|| {
        format!("declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} {context} must be an object")
    })?;
    if let Some(missing) = required.iter().find(|field| !object.contains_key(**field)) {
        return Err(format!(
            "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} {context} is missing required field {missing}"
        ));
    }
    Ok(())
}

fn agent_mail_snapshot_v1_reject_explicit_null(
    value: &Value,
    fields: &[&str],
    context: &str,
) -> Result<(), String> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    if let Some(field) = fields
        .iter()
        .find(|field| object.get(**field).is_some_and(Value::is_null))
    {
        return Err(format!(
            "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} {context}.{field} cannot be null"
        ));
    }
    Ok(())
}

fn agent_mail_snapshot_v1_nonempty(value: &str) -> bool {
    !value.trim().is_empty()
}

fn agent_mail_snapshot_v1_timestamp_is_valid(value: &str) -> bool {
    DateTime::parse_from_rfc3339(value).is_ok()
}

fn agent_mail_snapshot_project_key_is_valid(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn normalize_agent_mail_workspace_identity(value: &str, windows: bool) -> String {
    if !windows {
        return value.to_owned();
    }

    let mut normalized = value.replace('\\', "/");
    let folded = normalized.to_ascii_uppercase();
    if folded.starts_with("//?/UNC/") {
        normalized = format!("//{}", &normalized[8..]);
    } else if folded.starts_with("//?/") {
        normalized = normalized[4..].to_owned();
    }
    if normalized.as_bytes().get(1) == Some(&b':')
        && normalized
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
    {
        let drive_letter = normalized[0..1].to_ascii_lowercase();
        normalized.replace_range(0..1, &drive_letter);
    }
    normalized
}

pub(crate) fn agent_mail_snapshot_project_key_for_workspace(
    workspace: &Path,
) -> Result<String, String> {
    let canonical = fs::canonicalize(workspace).map_err(|error| {
        format!(
            "Agent Mail snapshot workspace binding could not canonicalize the requested workspace: {error}"
        )
    })?;
    let canonical = canonical.to_str().ok_or_else(|| {
        "Agent Mail snapshot workspace binding requires a valid UTF-8 canonical workspace path"
            .to_owned()
    })?;
    let identity = normalize_agent_mail_workspace_identity(canonical, cfg!(windows));
    let digest = crate::models::release::sha256_hex(identity.as_bytes());
    Ok(format!("sha256:{digest}"))
}

fn validate_agent_mail_snapshot_workspace_binding(
    input: &str,
    workspace: &Path,
) -> Result<(), String> {
    let value = serde_json::from_str::<Value>(input)
        .map_err(|error| format!("Agent Mail snapshot JSON could not be parsed: {error}"))?;
    if value.get("schema").and_then(Value::as_str) != Some(AGENT_MAIL_SNAPSHOT_SCHEMA_V1) {
        return Ok(());
    }
    let actual = value
        .get("project_key")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!("declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} project_key is missing")
        })?;
    let expected = agent_mail_snapshot_project_key_for_workspace(workspace)?;
    if actual != expected {
        return Err(format!(
            "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} project_key does not match the requested workspace"
        ));
    }
    Ok(())
}

fn agent_mail_snapshot_freshness_assessment(
    input: &str,
    now: DateTime<Utc>,
) -> Result<(SwarmBriefSourceFreshness, Option<SwarmBriefDegradation>), String> {
    let value = serde_json::from_str::<Value>(input)
        .map_err(|error| format!("Agent Mail snapshot JSON could not be parsed: {error}"))?;
    let declared_v1 =
        value.get("schema").and_then(Value::as_str) == Some(AGENT_MAIL_SNAPSHOT_SCHEMA_V1);
    if !declared_v1 {
        return Ok((
            SwarmBriefSourceFreshness::unknown(),
            Some(SwarmBriefDegradation::warning(
                SwarmBriefSourceKind::AgentMail,
                AGENT_MAIL_UNAVAILABLE_CODE,
                "Agent Mail snapshot does not declare ee.agent_mail.snapshot.v1 freshness evidence; legacy rows remain visible but are not authoritative for claims."
                    .to_owned(),
                Some("Regenerate the redacted Agent Mail snapshot with the shipped producer.".to_owned()),
            )),
        ));
    }

    let observed_raw = value
        .get("generated_at")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!("declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} generated_at is missing")
        })?;
    let observed = DateTime::parse_from_rfc3339(observed_raw)
        .map_err(|error| {
            format!(
                "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} generated_at is not RFC 3339: {error}"
            )
        })?
        .with_timezone(&Utc);
    let future_seconds = observed.signed_duration_since(now).num_seconds();
    if future_seconds > AGENT_MAIL_SNAPSHOT_MAX_FUTURE_SKEW_SECONDS {
        return Err(format!(
            "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} generated_at is {future_seconds} seconds in the future"
        ));
    }
    let age_seconds =
        u64::try_from(now.signed_duration_since(observed).num_seconds().max(0)).unwrap_or(u64::MAX);
    let stale = age_seconds > AGENT_MAIL_SNAPSHOT_STALE_AFTER_SECONDS;
    let freshness = SwarmBriefSourceFreshness {
        observed_at: Some(observed_raw.to_owned()),
        age_seconds: Some(age_seconds),
        stale_after_seconds: Some(AGENT_MAIL_SNAPSHOT_STALE_AFTER_SECONDS),
        state: if stale { "stale" } else { "current" },
    };
    let degradation = stale.then(|| {
        SwarmBriefDegradation::warning(
            SwarmBriefSourceKind::AgentMail,
            AGENT_MAIL_UNAVAILABLE_CODE,
            format!(
                "Agent Mail snapshot is stale: generated_at is {age_seconds} seconds old, beyond the {AGENT_MAIL_SNAPSHOT_STALE_AFTER_SECONDS}-second claim-evidence horizon."
            ),
            Some("Regenerate the redacted Agent Mail snapshot immediately before retrying the claim gate.".to_owned()),
        )
    });
    Ok((freshness, degradation))
}

pub(crate) fn validate_current_agent_mail_snapshot_for_workspace(
    input: &str,
    workspace: &Path,
    now: DateTime<Utc>,
) -> Result<(), String> {
    validate_agent_mail_snapshot_workspace_binding(input, workspace)?;
    let (freshness, degradation) = agent_mail_snapshot_freshness_assessment(input, now)?;
    if let Some(degradation) = degradation {
        return Err(degradation.message);
    }
    if freshness.state != "current" {
        return Err("Agent Mail snapshot does not carry current claim evidence".to_owned());
    }
    Ok(())
}

fn agent_mail_snapshot_v1_shell_quote(value: &str) -> String {
    let shell_safe = value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'_' | b'@' | b'%' | b'+' | b'=' | b':' | b',' | b'.' | b'/' | b'-'
            )
    });
    if shell_safe {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}

fn agent_mail_snapshot_v1_cli_command_prefix<'a>(
    index: usize,
    command: &'a str,
    agent_name: &str,
) -> Option<&'a str> {
    let quoted_agent = agent_mail_snapshot_v1_shell_quote(agent_name);
    let static_suffix = match index {
        0 => " agents list --project '<workspace>' --json".to_owned(),
        1 => " robot reservations --project '<workspace>' --all --format json".to_owned(),
        3 => format!(" status --project '<workspace>' --agent {quoted_agent} --json"),
        _ => String::new(),
    };
    if index != 2 {
        return command
            .strip_suffix(&static_suffix)
            .filter(|prefix| !prefix.is_empty());
    }

    let marker = format!(" mail inbox --project '<workspace>' --agent {quoted_agent} --limit ");
    let (prefix, limit_and_suffix) = command.rsplit_once(&marker)?;
    let limit = limit_and_suffix.strip_suffix(" --json")?;
    (!prefix.is_empty() && !limit.is_empty() && limit.bytes().all(|byte| byte.is_ascii_digit()))
        .then_some(prefix)
}

fn validate_declared_agent_mail_snapshot_v1(value: &Value) -> Result<(), String> {
    const ROOT_REQUIRED: &[&str] = &[
        "schema",
        "generated_at",
        "project_key",
        "agent_name",
        "redaction_status",
        "producer_status",
        "source_commands",
        "command_statuses",
        "fallback_active",
        "am_agents_list_ok",
        "summary",
        "degraded",
        "file_reservations",
        "agents",
        "inbox",
        "threads",
    ];
    const COMMAND_STATUS_REQUIRED: &[&str] =
        &["command", "ok", "exit_code", "timed_out", "error_class"];
    const SUMMARY_REQUIRED: &[&str] = &[
        "agent_count",
        "file_reservation_count",
        "inbox_mailbox_count",
        "thread_count",
        "source_command_count",
        "degraded_count",
    ];
    const DEGRADATION_REQUIRED: &[&str] = &[
        "code",
        "severity",
        "source",
        "command",
        "error_class",
        "exit_code",
        "timed_out",
    ];
    const RESERVATION_REQUIRED: &[&str] = &["path_pattern", "holder", "exclusive"];
    const AGENT_REQUIRED: &[&str] = &["name"];
    const INBOX_REQUIRED: &[&str] = &["mailbox", "unread_count", "ack_required_count"];
    const THREAD_REQUIRED: &[&str] = &["thread_id", "message_count"];

    agent_mail_snapshot_v1_require_fields(value, ROOT_REQUIRED, "root")?;
    agent_mail_snapshot_v1_reject_explicit_null(
        value,
        &[
            "health_level",
            "semantic_readiness",
            "durability_state",
            "recovery",
        ],
        "root",
    )?;
    for (field, required) in [
        ("command_statuses", COMMAND_STATUS_REQUIRED),
        ("degraded", DEGRADATION_REQUIRED),
        ("file_reservations", RESERVATION_REQUIRED),
        ("agents", AGENT_REQUIRED),
        ("inbox", INBOX_REQUIRED),
        ("threads", THREAD_REQUIRED),
    ] {
        let items = value.get(field).and_then(Value::as_array).ok_or_else(|| {
            format!("declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} {field} must be an array")
        })?;
        for (index, item) in items.iter().enumerate() {
            agent_mail_snapshot_v1_require_fields(item, required, &format!("{field}[{index}]"))?;
            let optional_fields: &[&str] = match field {
                "file_reservations" => &["expires_ts"],
                "agents" => &["last_active_ts"],
                "threads" => &["subject", "last_activity_at"],
                _ => &[],
            };
            agent_mail_snapshot_v1_reject_explicit_null(
                item,
                optional_fields,
                &format!("{field}[{index}]"),
            )?;
        }
    }
    agent_mail_snapshot_v1_require_fields(
        value
            .get("summary")
            .ok_or_else(|| "Agent Mail snapshot summary disappeared".to_owned())?,
        SUMMARY_REQUIRED,
        "summary",
    )?;
    if let Some(recovery) = value.get("recovery") {
        agent_mail_snapshot_v1_require_fields(recovery, &["mode", "reason"], "recovery")?;
    }
    if let Some(semantic) = value.get("semantic_readiness") {
        agent_mail_snapshot_v1_reject_explicit_null(semantic, &["reason"], "semantic_readiness")?;
    }

    let wire: AgentMailSnapshotV1Wire = serde_json::from_value(value.clone()).map_err(|error| {
        format!("declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} failed strict validation: {error}")
    })?;
    if wire.schema != AGENT_MAIL_SNAPSHOT_SCHEMA_V1 {
        return Err(format!(
            "declared Agent Mail snapshot schema changed during validation: {}",
            wire.schema
        ));
    }
    if !agent_mail_snapshot_v1_nonempty(&wire.generated_at)
        || !agent_mail_snapshot_v1_nonempty(&wire.project_key)
        || !agent_mail_snapshot_v1_nonempty(&wire.agent_name)
        || wire.agent_name.contains('\n')
        || wire.agent_name.contains('\r')
    {
        return Err(format!(
            "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} identity fields must be non-empty"
        ));
    }
    if !agent_mail_snapshot_project_key_is_valid(&wire.project_key) {
        return Err(format!(
            "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} project_key is not a canonical SHA-256 workspace binding"
        ));
    }
    DateTime::parse_from_rfc3339(&wire.generated_at).map_err(|error| {
        format!("declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} generated_at is not RFC 3339: {error}")
    })?;
    if wire.redaction_status != SWARM_BRIEF_REDACTION_STATUS {
        return Err(format!(
            "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} redaction_status is not authoritative"
        ));
    }
    if wire.source_commands.len() != AGENT_MAIL_SNAPSHOT_V1_SOURCE_COUNT
        || wire.command_statuses.len() != AGENT_MAIL_SNAPSHOT_V1_SOURCE_COUNT
    {
        return Err(format!(
            "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} requires {AGENT_MAIL_SNAPSHOT_V1_SOURCE_COUNT} source commands and matching statuses"
        ));
    }
    if wire.source_commands.iter().any(|command| {
        !agent_mail_snapshot_v1_nonempty(command)
            || command.contains('\n')
            || command.contains('\r')
    }) || wire.source_commands.iter().collect::<BTreeSet<_>>().len()
        != wire.source_commands.len()
    {
        return Err(format!(
            "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} source commands must be non-empty and unique"
        ));
    }
    let cli_prefixes = wire
        .source_commands
        .iter()
        .take(4)
        .enumerate()
        .map(|(index, command)| {
            agent_mail_snapshot_v1_cli_command_prefix(index, command, &wire.agent_name)
        })
        .collect::<Option<Vec<_>>>();
    let cli_prefixes_are_consistent = cli_prefixes.as_ref().is_some_and(|prefixes| {
        prefixes.len() == 4 && prefixes.iter().all(|prefix| *prefix == prefixes[0])
    });
    if !cli_prefixes_are_consistent
        || wire.source_commands[4] != "agent-mail-health http://127.0.0.1:8765/health"
        || wire.source_commands[5] != "agent-mail-health http://127.0.0.1:8765/health/durability"
    {
        return Err(format!(
            "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} source command identities or ordering drifted"
        ));
    }

    let mut failed_commands = BTreeSet::new();
    for (index, (command, status)) in wire
        .source_commands
        .iter()
        .zip(wire.command_statuses.iter())
        .enumerate()
    {
        if status.command != *command {
            return Err(format!(
                "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} command status order does not match source_commands"
            ));
        }
        if !status.ok {
            failed_commands.insert(status.command.as_str());
        }
        if status.ok && (status.timed_out || status.error_class.is_some()) {
            return Err(format!(
                "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} successful command status carries failure metadata"
            ));
        }
        let expected_success_exit_code = if index < 4 { 0 } else { 200 };
        if status.ok && status.exit_code != Some(expected_success_exit_code) {
            return Err(format!(
                "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} successful command status carries an impossible exit code"
            ));
        }
        if !status.ok && !status.timed_out && status.error_class.is_none() {
            return Err(format!(
                "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} failed command status omitted failure metadata"
            ));
        }
        if index == 4
            && !status.ok
            && (wire.health_level.is_some() || wire.semantic_readiness.is_some())
        {
            return Err(format!(
                "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} failed readiness probe carries health output"
            ));
        }
        if status
            .error_class
            .as_deref()
            .is_some_and(|error_class| !agent_mail_snapshot_v1_nonempty(error_class))
        {
            return Err(format!(
                "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} error_class must be null or non-empty"
            ));
        }
        let _ = status.exit_code;
    }
    let health_requires_fallback = matches!(wire.health_level.as_deref(), Some("yellow" | "red"))
        || wire
            .semantic_readiness
            .as_ref()
            .is_some_and(|semantic| semantic.status != "pass")
        || wire.recovery.is_some()
        || wire
            .durability_state
            .as_deref()
            .is_some_and(|state| state != "ok");
    let expected_fallback = !failed_commands.is_empty() || health_requires_fallback;
    if wire.fallback_active != expected_fallback
        || wire.producer_status != if expected_fallback { "degraded" } else { "ok" }
    {
        return Err(format!(
            "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} producer/fallback posture contradicts command statuses"
        ));
    }
    if wire.am_agents_list_ok != wire.command_statuses[0].ok {
        return Err(format!(
            "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} am_agents_list_ok contradicts its command status"
        ));
    }
    if (!wire.command_statuses[0].ok && !wire.agents.is_empty())
        || (!wire.command_statuses[1].ok && !wire.file_reservations.is_empty())
        || (!wire.command_statuses[2].ok && !wire.threads.is_empty())
        || (!wire.command_statuses[3].ok && !wire.inbox.is_empty())
    {
        return Err(format!(
            "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} failed source command carries normalized rows"
        ));
    }
    if wire.command_statuses[3].ok
        && (wire.inbox.len() != 1 || wire.inbox[0].mailbox != wire.agent_name)
    {
        return Err(format!(
            "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} successful status probe must carry exactly its agent mailbox"
        ));
    }

    let degraded_commands = wire
        .degraded
        .iter()
        .map(|degradation| degradation.command.as_str())
        .collect::<BTreeSet<_>>();
    if degraded_commands != failed_commands || wire.degraded.len() != failed_commands.len() {
        return Err(format!(
            "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} degraded entries do not match failed command statuses"
        ));
    }
    for degradation in &wire.degraded {
        if !agent_mail_snapshot_v1_nonempty(&degradation.code)
            || !agent_mail_snapshot_v1_nonempty(&degradation.source)
            || crate::models::DegradationSeverity::parse(&degradation.severity)
                .is_none_or(|parsed| parsed.as_str() != degradation.severity)
            || degradation
                .error_class
                .as_deref()
                .is_some_and(|error_class| !agent_mail_snapshot_v1_nonempty(error_class))
        {
            return Err(format!(
                "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} contains malformed degradation metadata"
            ));
        }
        let status = wire
            .command_statuses
            .iter()
            .find(|status| status.command == degradation.command)
            .ok_or_else(|| {
                format!(
                    "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} degradation has no command status"
                )
            })?;
        if status.ok
            || status.exit_code != degradation.exit_code
            || status.timed_out != degradation.timed_out
            || status.error_class != degradation.error_class
        {
            return Err(format!(
                "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} degradation contradicts its command status"
            ));
        }
    }

    let count = |value: usize, label: &str| {
        u64::try_from(value).map_err(|_| {
            format!("declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} {label} count overflowed u64")
        })
    };
    if wire.summary.agent_count != count(wire.agents.len(), "agent")?
        || wire.summary.file_reservation_count
            != count(wire.file_reservations.len(), "reservation")?
        || wire.summary.inbox_mailbox_count != count(wire.inbox.len(), "inbox")?
        || wire.summary.thread_count != count(wire.threads.len(), "thread")?
        || wire.summary.source_command_count != count(wire.source_commands.len(), "source command")?
        || wire.summary.degraded_count != count(wire.degraded.len(), "degraded")?
    {
        return Err(format!(
            "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} summary counts contradict normalized arrays"
        ));
    }

    for reservation in &wire.file_reservations {
        if !agent_mail_snapshot_v1_nonempty(&reservation.path_pattern)
            || !agent_mail_snapshot_v1_nonempty(&reservation.holder)
            || reservation
                .expires_ts
                .as_deref()
                .is_some_and(|timestamp| !agent_mail_snapshot_v1_timestamp_is_valid(timestamp))
        {
            return Err(format!(
                "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} contains malformed reservation metadata"
            ));
        }
        let _ = reservation.exclusive;
    }
    let distinct_agent_names = wire
        .agents
        .iter()
        .map(|agent| agent.name.as_str())
        .collect::<BTreeSet<_>>();
    if distinct_agent_names.len() != wire.agents.len()
        || wire.agents.iter().any(|agent| {
            !agent_mail_snapshot_v1_nonempty(&agent.name)
                || agent
                    .last_active_ts
                    .as_deref()
                    .is_some_and(|timestamp| !agent_mail_snapshot_v1_timestamp_is_valid(timestamp))
        })
        || wire.inbox.iter().any(|mailbox| {
            let _ = (mailbox.unread_count, mailbox.ack_required_count);
            !agent_mail_snapshot_v1_nonempty(&mailbox.mailbox)
        })
        || wire.threads.iter().any(|thread| {
            let _ = thread.message_count;
            !agent_mail_snapshot_v1_nonempty(&thread.thread_id)
                || thread
                    .subject
                    .as_deref()
                    .is_some_and(|subject| subject.contains('\n') || subject.contains('\r'))
                || thread
                    .last_activity_at
                    .as_deref()
                    .is_some_and(|timestamp| !agent_mail_snapshot_v1_timestamp_is_valid(timestamp))
        })
    {
        return Err(format!(
            "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} contains malformed coordination rows"
        ));
    }
    if wire
        .health_level
        .as_deref()
        .is_some_and(|level| !matches!(level, "green" | "yellow" | "red"))
        || wire.semantic_readiness.as_ref().is_some_and(|semantic| {
            !matches!(semantic.status.as_str(), "pass" | "fail" | "unknown")
                || (semantic.status != "fail" && semantic.reason.is_some())
                || (semantic.status == "fail" && semantic.reason.is_none())
                || semantic.reason.as_deref().is_some_and(|reason| {
                    !matches!(
                        reason,
                        "malformed_sqlite"
                            | "archive_corruption"
                            | "index_rebuild_required"
                            | "permission_denied"
                            | "unknown"
                    )
                })
        })
        || wire.durability_state.as_deref().is_some_and(|state| {
            !matches!(
                state,
                "ok" | "corrupt" | "repair_required" | "unknown_recovery"
            )
        })
        || wire.recovery.as_ref().is_some_and(|recovery| {
            !matches!(
                recovery.mode.as_str(),
                "corrupt" | "repair_required" | "unknown_recovery"
            ) || !matches!(
                recovery.reason.as_str(),
                "archive_corruption"
                    | "storage_recovery_required"
                    | "permission_denied"
                    | "unknown"
            )
        })
    {
        return Err(format!(
            "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} health posture is malformed"
        ));
    }
    if wire.command_statuses[4].ok && wire.health_level.is_none() {
        return Err(format!(
            "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} successful readiness probe omitted health_level"
        ));
    }
    if wire.command_statuses[5].ok && wire.durability_state.is_none() {
        return Err(format!(
            "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} successful durability probe omitted durability_state"
        ));
    }
    match (wire.durability_state.as_deref(), wire.recovery.as_ref()) {
        (Some("ok"), None) | (None, None) => {}
        (Some(state), Some(recovery)) if state != "ok" && recovery.mode == state => {}
        _ => {
            return Err(format!(
                "declared {AGENT_MAIL_SNAPSHOT_SCHEMA_V1} durability_state and recovery posture contradict"
            ));
        }
    }

    Ok(())
}

pub fn parse_agent_mail_snapshot_json(input: &str) -> Result<SwarmBriefAgentMailSnapshot, String> {
    let value = serde_json::from_str::<Value>(input)
        .map_err(|error| format!("Agent Mail snapshot JSON could not be parsed: {error}"))?;
    let declared_v1 =
        value.get("schema").and_then(Value::as_str) == Some(AGENT_MAIL_SNAPSHOT_SCHEMA_V1);
    if declared_v1
        || value.get("producer_status").is_some()
        || value.get("source_commands").is_some()
        || value.get("command_statuses").is_some()
    {
        validate_declared_agent_mail_snapshot_v1(&value)?;
    }
    let agent_name = if declared_v1 {
        value
            .get("agent_name")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    } else {
        None
    };
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
    let agents = value
        .get("agents")
        .or_else(|| value.get("agent_inventory"))
        .or_else(|| value.get("agentInventory"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(parse_agent_mail_agent_summary)
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
    let mut agents_by_name = BTreeMap::<String, SwarmBriefAgentMailAgent>::new();
    for agent in agents {
        match agents_by_name.entry(agent.name.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(agent);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                if agent.last_active_at > entry.get().last_active_at {
                    entry.insert(agent);
                }
            }
        }
    }
    let agents = agents_by_name.into_values().collect();
    inbox.sort();
    inbox.dedup();
    threads.sort();
    threads.dedup();
    Ok(SwarmBriefAgentMailSnapshot {
        agent_name,
        file_reservations: reservations,
        agents,
        inbox,
        threads,
        degraded,
    })
}

fn parse_agent_mail_health_degraded(value: &Value) -> Vec<SwarmBriefDegradation> {
    let semantic_readiness_degradation = parse_agent_mail_semantic_readiness_degradation(value);
    let recovery_degradation = parse_agent_mail_recovery_degradation(value);
    let is_coordination_health = value
        .get("schema")
        .and_then(Value::as_str)
        .is_some_and(|schema| schema == "ee.swarm.coordination_health.v1")
        || value.get("fallback_active").is_some()
        || semantic_readiness_degradation.is_some()
        || recovery_degradation.is_some();
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
    let mut degraded = semantic_readiness_degradation
        .into_iter()
        .chain(recovery_degradation)
        .collect::<Vec<_>>();
    if failed_checks.is_empty() && !degraded.is_empty() {
        return degraded;
    }
    if !fallback_active && failed_checks.is_empty() {
        return degraded;
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

    degraded.push(SwarmBriefDegradation::warning(
        SwarmBriefSourceKind::AgentMail,
        AGENT_MAIL_UNAVAILABLE_CODE,
        message,
        Some(
            "Run `am doctor repair` or provide a current redacted Agent Mail snapshot.".to_string(),
        ),
    ));
    degraded
}

fn parse_agent_mail_recovery_degradation(value: &Value) -> Option<SwarmBriefDegradation> {
    let recovery = value
        .get("recovery")
        .or_else(|| value.get("recoveryStatus"));
    let (mode, reason) = match recovery {
        Some(Value::Object(recovery)) => {
            let mode = recovery
                .get("mode")
                .or_else(|| recovery.get("status"))
                .and_then(Value::as_str)
                .and_then(agent_mail_recovery_mode_class)?;
            let reason = agent_mail_recovery_reason_class(mode, Some(recovery), value);
            (mode, reason)
        }
        _ => {
            let durability_state = value
                .get("durability_state")
                .or_else(|| value.get("durabilityState"))
                .and_then(Value::as_str)
                .and_then(agent_mail_recovery_mode_class)?;
            let reason = agent_mail_recovery_reason_class(durability_state, None, value);
            (durability_state, reason)
        }
    };
    let health_level = value
        .get("healthLevel")
        .or_else(|| value.get("health_level"))
        .and_then(Value::as_str)
        .and_then(agent_mail_health_level_class);
    let health_fragment = health_level
        .map(|level| format!(" with healthLevel={level}"))
        .unwrap_or_default();
    let semantic_fragment = agent_mail_semantic_status_class(value)
        .map(|status| format!(", semanticStatus={status}"))
        .unwrap_or_default();
    let message = format!(
        "Agent Mail recovery posture is degraded{health_fragment} (mode={mode}, reason={reason}{semantic_fragment}); reservation and inbox reads are not authoritative."
    );

    Some(SwarmBriefDegradation::warning(
        SwarmBriefSourceKind::AgentMail,
        AGENT_MAIL_UNAVAILABLE_CODE,
        message,
        Some(
            "Repair Agent Mail storage and provide a current redacted Agent Mail snapshot after recovery completes."
                .to_string(),
        ),
    ))
}

fn agent_mail_semantic_status_class(value: &Value) -> Option<&'static str> {
    let semantic = value
        .get("semantic_readiness")
        .or_else(|| value.get("semanticReadiness"))?;
    let status = semantic
        .as_str()
        .or_else(|| semantic.get("status").and_then(Value::as_str))?;
    match status.to_ascii_lowercase().as_str() {
        "ok" | "pass" => Some("pass"),
        "fail" => Some("fail"),
        "unknown" => Some("unknown"),
        _ => None,
    }
}

fn parse_agent_mail_semantic_readiness_degradation(value: &Value) -> Option<SwarmBriefDegradation> {
    let semantic = value
        .get("semantic_readiness")
        .or_else(|| value.get("semanticReadiness"))?;
    let status = semantic
        .as_str()
        .or_else(|| semantic.get("status").and_then(Value::as_str))?;
    if !status.eq_ignore_ascii_case("fail") {
        return None;
    }

    let reason = semantic
        .get("reason")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("semantic_readiness_reason")
                .and_then(Value::as_str)
        })
        .or_else(|| value.get("semanticReadinessReason").and_then(Value::as_str));
    let reason_class = agent_mail_semantic_readiness_reason_class(reason);
    let health_level = value
        .get("healthLevel")
        .or_else(|| value.get("health_level"))
        .and_then(Value::as_str)
        .and_then(agent_mail_health_level_class);
    let health_fragment = health_level
        .map(|level| format!(" with healthLevel={level}"))
        .unwrap_or_default();
    let message = format!(
        "Agent Mail semantic readiness failed{health_fragment} ({reason_class}); reservation and inbox reads are not authoritative."
    );

    Some(SwarmBriefDegradation::warning(
        SwarmBriefSourceKind::AgentMail,
        AGENT_MAIL_SEMANTIC_READINESS_FAILED_CODE,
        message,
        Some(
            "Repair Agent Mail storage and re-run the work-packet collector after semantic readiness passes."
                .to_string(),
        ),
    ))
}

fn agent_mail_recovery_mode_class(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "" | "ok" | "none" | "normal" | "clean" | "idle" => None,
        "corrupt" => Some("corrupt"),
        "repair" | "repair_required" | "repairing" | "recover" | "recovering"
        | "recovery_required" | "restore" | "restoring" | "reconstruct" => Some("repair_required"),
        _ => Some("unknown_recovery"),
    }
}

fn agent_mail_recovery_reason_class(
    mode: &str,
    recovery: Option<&serde_json::Map<String, Value>>,
    value: &Value,
) -> &'static str {
    if mode == "corrupt" {
        return "archive_corruption";
    }
    let mut text = String::new();
    if let Some(recovery) = recovery {
        for key in [
            "reason",
            "next_action",
            "nextAction",
            "detail",
            "message",
            "bundle_path",
            "bundlePath",
        ] {
            if let Some(fragment) = recovery.get(key).and_then(Value::as_str) {
                text.push(' ');
                text.push_str(fragment);
            }
        }
    }
    for key in ["detail", "message", "status"] {
        if let Some(fragment) = value.get(key).and_then(Value::as_str) {
            text.push(' ');
            text.push_str(fragment);
        }
    }
    let normalized = text.to_ascii_lowercase();
    if normalized.contains("doctor repair")
        || normalized.contains("restore")
        || normalized.contains("reconstruct")
    {
        "storage_recovery_required"
    } else if normalized.contains("permission denied") || normalized.contains("access denied") {
        "permission_denied"
    } else {
        "unknown"
    }
}

fn agent_mail_health_level_class(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "green" => Some("green"),
        "yellow" => Some("yellow"),
        "red" => Some("red"),
        _ => None,
    }
}

fn agent_mail_semantic_readiness_reason_class(value: Option<&str>) -> &'static str {
    let Some(value) = value else {
        return "unknown";
    };
    let normalized = value.to_ascii_lowercase();
    if normalized == "malformed_sqlite"
        || (normalized.contains("sqlite") && normalized.contains("malformed"))
        || normalized.contains("database disk image is malformed")
    {
        "malformed_sqlite"
    } else if normalized == "archive_corruption"
        || (normalized.contains("archive")
            && (normalized.contains("corrupt")
                || normalized.contains("parse")
                || normalized.contains("jsonl")))
    {
        "archive_corruption"
    } else if normalized == "index_rebuild_required"
        || (normalized.contains("index")
            && (normalized.contains("rebuild")
                || normalized.contains("missing")
                || normalized.contains("stale")))
    {
        "index_rebuild_required"
    } else if normalized == "permission_denied"
        || normalized.contains("permission denied")
        || normalized.contains("access denied")
    {
        "permission_denied"
    } else {
        "unknown"
    }
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

fn parse_agent_mail_agent_summary(item: &Value) -> Option<SwarmBriefAgentMailAgent> {
    let name = string_field(item, &["name", "agent_name", "agent", "mailbox"])?;
    Some(SwarmBriefAgentMailAgent {
        name: redact_brief_text(&name),
        last_active_at: string_field(
            item,
            &[
                "last_active_at",
                "lastActiveAt",
                "last_active_ts",
                "lastActiveTs",
            ],
        ),
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
    let pressure = rch_worker_pressure_report(&value, None);
    if pressure.worker_count > 0 {
        let level = match pressure.status.as_str() {
            "healthy_but_pressure_blocked" | "pressure_policy_denied" => "high",
            "telemetry_stale" | "pressure_degraded" => "medium",
            "pressure_unknown" => "unknown",
            _ => "low",
        };
        hints.push(SwarmBriefResourcePressureHint {
            source: SwarmBriefSourceKind::Rch,
            level: level.to_string(),
            message: format!("rch worker pressure posture: {}", pressure.status),
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
    let worker_pressure = rch_worker_pressure_report(status, worker_probe);
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
    let worker_pressure_blocked = matches!(
        worker_pressure.status.as_str(),
        "healthy_but_pressure_blocked" | "pressure_policy_denied"
    ) || (worker_pressure.worker_count > 0
        && worker_pressure.usable_worker_count == 0
        && worker_pressure.blocked_worker_count > 0);
    let remote_only_safe = route_available
        && workers_probe_ready
        && !queue_start_stalled
        && !queue_capacity_blocked
        && !worker_pressure_blocked
        && dry_run_would_offload.unwrap_or(true)
        && status_socket_consistent.unwrap_or(true);
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
    if worker_pressure_blocked {
        degraded.push(SwarmBriefDegradation::warning(
            SwarmBriefSourceKind::Rch,
            RCH_REMOTE_REQUIRED_FALLBACK_PREVENTED_CODE,
            format!(
                "RCH worker pressure posture is {}; remote-only verification must fail closed before launching a doomed Cargo job.",
                worker_pressure.status
            ),
            Some("Reuse an active RCH known-blocker proof or wait for operator-approved worker disk-pressure remediation; ee must not delete or mutate worker files.".to_string()),
        ));
        recovery.push("reuse_rch_known_blocker_or_wait_for_worker_pressure_recovery".to_string());
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
        worker_pressure,
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
    let active_build_max_age_seconds = rch_active_build_max_age_seconds(status);
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
        queue_head_slots_needed: first_slots_needed,
        active_build_max_age_seconds,
        status: status_label.to_string(),
    })
}

pub fn parse_rch_worker_pressure_report(input: &str) -> Result<RchWorkerPressureReport, String> {
    let status = serde_json::from_str::<Value>(input)
        .map_err(|error| format!("RCH status JSON could not be parsed: {error}"))?;
    Ok(rch_worker_pressure_report(&status, None))
}

fn rch_worker_pressure_report(status: &Value, probe: Option<&Value>) -> RchWorkerPressureReport {
    let mut workers = rch_worker_pressure_observations(status);
    if workers.is_empty()
        && let Some(probe) = probe
    {
        workers = rch_worker_pressure_observations(probe);
    }
    workers.sort();
    workers.dedup_by(|left, right| left.worker_id == right.worker_id);

    let worker_count = workers.len() as u64;
    let usable_worker_count = workers
        .iter()
        .filter(|worker| worker.admission_impact == "usable")
        .count() as u64;
    let blocked_worker_count = workers
        .iter()
        .filter(|worker| worker.admission_impact == "blocked")
        .count() as u64;
    let stale_worker_count = workers
        .iter()
        .filter(|worker| worker.telemetry_freshness == "stale")
        .count() as u64;
    let unknown_worker_count = workers
        .iter()
        .filter(|worker| worker.pressure_state == "unknown")
        .count() as u64;
    let any_healthy_blocked = workers.iter().any(|worker| {
        worker.admission_impact == "blocked" && worker.reason_code.contains("disk_pressure")
    });
    let any_policy_denied = workers
        .iter()
        .any(|worker| worker.reason_code.contains("policy_denied"));
    let status_label = if worker_count == 0 {
        "pressure_unknown"
    } else if any_policy_denied {
        "pressure_policy_denied"
    } else if unknown_worker_count == worker_count {
        "pressure_unknown"
    } else if any_healthy_blocked && usable_worker_count == 0 {
        "healthy_but_pressure_blocked"
    } else if stale_worker_count == worker_count {
        "telemetry_stale"
    } else if blocked_worker_count > 0 || stale_worker_count > 0 {
        "pressure_degraded"
    } else {
        "pressure_clear"
    };

    RchWorkerPressureReport {
        schema: RCH_WORKER_PRESSURE_SCHEMA_V1,
        status: status_label.to_string(),
        worker_count,
        usable_worker_count,
        blocked_worker_count,
        stale_worker_count,
        unknown_worker_count,
        workers,
    }
}

fn rch_worker_pressure_observations(value: &Value) -> Vec<RchWorkerPressureObservation> {
    rch_workers(value)
        .map(|workers| {
            workers
                .iter()
                .enumerate()
                .map(|(index, worker)| rch_worker_pressure_observation(index, worker))
                .collect()
        })
        .unwrap_or_default()
}

fn rch_worker_pressure_observation(index: usize, worker: &Value) -> RchWorkerPressureObservation {
    let worker_id = string_field(
        worker,
        &["id", "worker_id", "workerId", "name", "alias", "host"],
    )
    .map(|id| redact_brief_text(&id))
    .unwrap_or_else(|| format!("worker_{index}"));
    let explicit_pressure = string_field(
        worker,
        &[
            "disk_pressure",
            "diskPressure",
            "pressure",
            "pressure_state",
            "pressureState",
            "resource_pressure",
            "resourcePressure",
        ],
    );
    let explicit_admission = string_field(
        worker,
        &[
            "admission",
            "admissionImpact",
            "admission_impact",
            "admission_state",
            "admissionState",
            "buildAdmission",
            "build_admission",
        ],
    );
    let explicit_reason = string_field(
        worker,
        &[
            "reason_code",
            "reasonCode",
            "reason",
            "message",
            "admissionReason",
            "admission_reason",
        ],
    );
    let free_gb = numeric_field(
        worker,
        &[
            "free_gb",
            "freeGb",
            "disk_free_gb",
            "diskFreeGb",
            "available_gb",
            "availableGb",
        ],
    )
    .or_else(|| {
        numeric_field(
            worker,
            &[
                "free_bytes",
                "freeBytes",
                "disk_free_bytes",
                "diskFreeBytes",
                "available_bytes",
                "availableBytes",
            ],
        )
        .map(|bytes| bytes / 1_000_000_000)
    });
    let free_ratio_bps = ratio_bps_field(
        worker,
        &[
            "free_ratio",
            "freeRatio",
            "disk_free_ratio",
            "diskFreeRatio",
            "available_ratio",
            "availableRatio",
        ],
        false,
    )
    .or_else(|| {
        ratio_bps_field(
            worker,
            &[
                "free_percent",
                "freePercent",
                "available_percent",
                "availablePercent",
            ],
            true,
        )
    });
    let telemetry_freshness = rch_worker_telemetry_freshness(worker);
    let pressure_state = normalize_rch_pressure_state(
        explicit_pressure.as_deref(),
        free_gb,
        free_ratio_bps,
        telemetry_freshness.as_str(),
    );
    let admission_impact =
        normalize_rch_admission_impact(explicit_admission.as_deref(), pressure_state.as_str());
    let reason_code = if explicit_admission
        .as_deref()
        .is_some_and(is_rch_policy_denied_text)
    {
        "pressure_policy_denied".to_string()
    } else {
        normalize_rch_pressure_reason(
            explicit_reason.as_deref(),
            pressure_state.as_str(),
            admission_impact.as_str(),
        )
    };
    let confidence = rch_worker_pressure_confidence(
        explicit_pressure.as_deref(),
        explicit_admission.as_deref(),
        free_gb,
        free_ratio_bps,
        pressure_state.as_str(),
    );

    RchWorkerPressureObservation {
        worker_id,
        pressure_state,
        confidence,
        reason_code,
        free_gb,
        free_ratio_bps,
        telemetry_freshness,
        admission_impact,
    }
}

fn normalize_rch_pressure_state(
    explicit: Option<&str>,
    free_gb: Option<u64>,
    free_ratio_bps: Option<u64>,
    freshness: &str,
) -> String {
    if freshness == "stale" {
        return "stale".to_string();
    }
    if let Some(value) = explicit {
        let lower = value.to_ascii_lowercase();
        if lower.contains("critical")
            || lower.contains("full")
            || lower.contains("blocked")
            || lower.contains("enospc")
        {
            return "critical".to_string();
        }
        if lower.contains("warn") || lower.contains("pressure") || lower.contains("low") {
            return "warning".to_string();
        }
        if lower.contains("clear")
            || lower.contains("ok")
            || lower.contains("healthy")
            || lower.contains("normal")
            || lower.contains("nominal")
        {
            return "clear".to_string();
        }
        if lower.contains("stale") {
            return "stale".to_string();
        }
    }
    if free_ratio_bps.is_some_and(|ratio| ratio < 500) || free_gb.is_some_and(|gb| gb < 2) {
        return "critical".to_string();
    }
    if free_ratio_bps.is_some_and(|ratio| ratio < 1_000) || free_gb.is_some_and(|gb| gb < 10) {
        return "warning".to_string();
    }
    if free_ratio_bps.is_some() || free_gb.is_some() {
        return "clear".to_string();
    }
    "unknown".to_string()
}

fn normalize_rch_admission_impact(explicit: Option<&str>, pressure_state: &str) -> String {
    if let Some(value) = explicit {
        let lower = value.to_ascii_lowercase();
        if is_rch_policy_denied_text(value)
            || lower.contains("deny")
            || lower.contains("blocked")
            || lower.contains("refuse")
            || lower.contains("not_admitted")
        {
            return "blocked".to_string();
        }
        if lower.contains("degraded")
            || lower.contains("warn")
            || lower.contains("limited")
            || lower.contains("throttle")
        {
            return "degraded".to_string();
        }
        if lower.contains("allow") || lower.contains("admit") || lower.contains("usable") {
            return "usable".to_string();
        }
    }
    match pressure_state {
        "critical" => "blocked",
        "warning" | "stale" => "degraded",
        "clear" => "usable",
        _ => "unknown",
    }
    .to_string()
}

fn normalize_rch_pressure_reason(
    explicit: Option<&str>,
    pressure_state: &str,
    admission_impact: &str,
) -> String {
    if let Some(value) = explicit {
        if is_rch_policy_denied_text(value) {
            return "pressure_policy_denied".to_string();
        }
        let lower = value.to_ascii_lowercase();
        if lower.contains("disk") && lower.contains("pressure") {
            return format!("disk_pressure_{pressure_state}");
        }
        if lower.contains("enospc") || lower.contains("no space left") {
            return "disk_pressure_critical".to_string();
        }
    }
    if admission_impact == "blocked" && pressure_state == "critical" {
        "disk_pressure_critical"
    } else if pressure_state == "warning" {
        "disk_pressure_warning"
    } else if pressure_state == "stale" {
        "telemetry_stale"
    } else if pressure_state == "clear" {
        "pressure_clear"
    } else {
        "no_pressure_telemetry"
    }
    .to_string()
}

fn is_rch_policy_denied_text(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("policy") && (lower.contains("deny") || lower.contains("denied"))
}

fn rch_worker_pressure_confidence(
    explicit_pressure: Option<&str>,
    explicit_admission: Option<&str>,
    free_gb: Option<u64>,
    free_ratio_bps: Option<u64>,
    pressure_state: &str,
) -> String {
    if explicit_pressure.is_some() || explicit_admission.is_some() {
        return "high".to_string();
    }
    if free_gb.is_some() || free_ratio_bps.is_some() {
        return "medium".to_string();
    }
    if pressure_state == "unknown" {
        "low"
    } else {
        "medium"
    }
    .to_string()
}

fn rch_worker_telemetry_freshness(worker: &Value) -> String {
    if let Some(freshness) = string_field(
        worker,
        &[
            "telemetry_freshness",
            "telemetryFreshness",
            "freshness",
            "telemetry_state",
            "telemetryState",
        ],
    ) {
        let lower = freshness.to_ascii_lowercase();
        if lower.contains("stale") || lower.contains("expired") {
            return "stale".to_string();
        }
        if lower.contains("current") || lower.contains("fresh") {
            return "current".to_string();
        }
    }
    if string_field(
        worker,
        &[
            "observed_at",
            "observedAt",
            "last_seen",
            "lastSeen",
            "updated_at",
            "updatedAt",
        ],
    )
    .is_some()
    {
        "current".to_string()
    } else {
        "unknown".to_string()
    }
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

fn rch_active_build_max_age_seconds(status: &Value) -> Option<u64> {
    let active_build_age_keys = [
        "active_build_max_age_seconds",
        "activeBuildMaxAgeSeconds",
        "max_active_build_age_seconds",
        "maxActiveBuildAgeSeconds",
    ];
    numeric_field_any(status, &active_build_age_keys).or_else(|| {
        rch_build_array(status, "active_builds", "activeBuilds")
            .and_then(|items| items.iter().filter_map(rch_active_build_age_seconds).max())
    })
}

fn rch_active_build_age_seconds(build: &Value) -> Option<u64> {
    numeric_field_any(
        build,
        &[
            "detector_build_age_secs",
            "detectorBuildAgeSecs",
            "detector_build_age_seconds",
            "detectorBuildAgeSeconds",
            "age_secs",
            "ageSecs",
            "age_seconds",
            "ageSeconds",
            "duration_secs",
            "durationSecs",
            "duration_seconds",
            "durationSeconds",
        ],
    )
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
        .or_else(|| {
            value
                .pointer("/data/daemon/workers")
                .and_then(Value::as_array)
        })
        .or_else(|| {
            value
                .pointer("/data/daemon/daemon/workers")
                .and_then(Value::as_array)
        })
        .or_else(|| value.pointer("/data/results").and_then(Value::as_array))
}

fn rch_worker_is_ready(worker: &Value) -> bool {
    string_field(worker, &["status", "state", "health"])
        .map(|status| {
            let status = status.trim().to_ascii_lowercase();
            status.contains("ready")
                || status.contains("healthy")
                || status.contains("online")
                || status == "ok"
        })
        .unwrap_or(false)
}

fn rch_worker_is_unreachable(worker: &Value) -> bool {
    string_field(worker, &["status", "state", "health"])
        .map(|status| {
            let status = status.trim().to_ascii_lowercase();
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

fn string_array_field(value: &Value, keys: &[&str]) -> Vec<String> {
    let mut values = keys
        .iter()
        .find_map(|key| value.get(*key).and_then(Value::as_array))
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(redact_brief_text)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    values.sort();
    values.dedup();
    values
}

fn string_field_any(value: &Value, keys: &[&str]) -> Option<String> {
    string_field(value, keys)
        .or_else(|| value.get("data").and_then(|data| string_field(data, keys)))
}

fn numeric_field(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        let value = value.get(*key)?;
        value
            .as_u64()
            .or_else(|| value.as_i64().and_then(|number| number.try_into().ok()))
            .or_else(|| value.as_str()?.trim().parse::<u64>().ok())
    })
}

fn numeric_field_any(value: &Value, keys: &[&str]) -> Option<u64> {
    numeric_field(value, keys)
        .or_else(|| value.get("data").and_then(|data| numeric_field(data, keys)))
}

fn ratio_bps_field(value: &Value, keys: &[&str], percent_units: bool) -> Option<u64> {
    keys.iter().find_map(|key| {
        let raw = value.get(*key)?;
        let numeric = raw.as_f64().or_else(|| {
            raw.as_str()
                .and_then(|text| text.trim().parse::<f64>().ok())
        })?;
        if !numeric.is_finite() || numeric.is_sign_negative() {
            return None;
        }
        let basis_points = if percent_units || numeric > 1.0 {
            numeric * 100.0
        } else {
            numeric * 10_000.0
        };
        Some(basis_points.round() as u64)
    })
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
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use super::*;
    use crate::testing::{TestResult, ensure_equal};

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

    struct DrainCountingReader {
        remaining: usize,
        chunk_size: usize,
        consumed: Rc<Cell<usize>>,
    }

    impl io::Read for DrainCountingReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Ok(0);
            }
            let read = self.remaining.min(self.chunk_size).min(buf.len());
            buf[..read].fill(b'x');
            self.remaining -= read;
            self.consumed.set(self.consumed.get() + read);
            Ok(read)
        }
    }

    struct FailingReader;

    impl io::Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("synthetic pipe failure"))
        }
    }

    #[test]
    fn swarm_brief_pipe_limit_drains_after_retained_cap() {
        let consumed = Rc::new(Cell::new(0));
        let mut reader = DrainCountingReader {
            remaining: 32,
            chunk_size: 5,
            consumed: Rc::clone(&consumed),
        };

        let output = read_swarm_brief_pipe_limited(&mut reader, 7).expect("pipe drains");

        assert_eq!(output, b"xxxxxxx");
        assert_eq!(consumed.get(), 32);
    }

    #[test]
    fn swarm_brief_pipe_reader_reports_read_errors() {
        let mut reader = FailingReader;

        let error = read_swarm_brief_pipe_limited(&mut reader, 7)
            .expect_err("pipe read failures must not become empty output");

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(error.to_string().contains("synthetic pipe failure"));
    }

    #[test]
    fn swarm_brief_pipe_reader_thread_panic_is_unavailable() {
        let handle = thread::spawn(|| -> io::Result<Vec<u8>> {
            panic!("synthetic pipe reader panic");
        });

        let error = join_swarm_brief_pipe_reader(handle, "stdout")
            .expect_err("reader thread panics must not become empty output");

        match error {
            SwarmBriefCommandError::Unavailable(message) => {
                assert!(message.contains("stdout reader thread panicked"));
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    fn bead(id: &str, title: &str, source_bucket: &str) -> SwarmBriefBead {
        SwarmBriefBead {
            id: id.to_string(),
            title: title.to_string(),
            status: source_bucket.to_string(),
            priority: Some(1),
            assignee: None,
            issue_type: None,
            created_at: None,
            updated_at: None,
            latest_comment_at: None,
            comment_count: 0,
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

    fn verification_broker_summary_fixture() -> SwarmBriefVerificationBrokerSummary {
        SwarmBriefVerificationBrokerSummary {
            schema: SWARM_BRIEF_VERIFICATION_BROKER_SCHEMA_V1,
            source_schema: "ee.verification.posture.v1".to_string(),
            status: "ok".to_string(),
            record_count: 3,
            recent_run_count: 2,
            stale_run_count: 1,
            unknown_age_count: 0,
            recent_reusable_run_count: 0,
            in_flight_equivalent_command_count: 0,
            advisory_counts: VerificationPostureAdvisoryCounts::default(),
            evidence_health: VerificationPostureEvidenceHealth {
                ledger_available: true,
                status: "healthy".to_string(),
                malformed_timestamp_count: 0,
                missing_artifact_manifest_count: 0,
                local_disallowed_count: 0,
                topology_blocked_count: 0,
                issue_count: 0,
                reason: None,
            },
            recovery_actions: Vec::new(),
            rch_queue_status: "clear".to_string(),
            rch_slots_available: Some(4),
            rch_queue_head_slots_needed: None,
            rch_worker_pressure_status: "healthy".to_string(),
            rch_usable_worker_count: 2,
            rch_blocked_worker_count: 0,
            raw_logs_included: false,
            raw_mail_bodies_included: false,
        }
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
    fn likely_surfaces_map_document_terms_to_docs_scope() {
        for title in [
            "Document the swarm claim gate",
            "Improve coordination documentation",
        ] {
            let surfaces = likely_surfaces_for_text(title);
            assert!(surfaces.contains(&"README.md".to_owned()), "{title}");
            assert!(surfaces.contains(&"docs/**".to_owned()), "{title}");
        }
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
    fn swarm_incident_summary_counts_schema_shaped_invalid_fixtures_as_malformed() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let fixture_dir = tempdir
            .path()
            .join("tests")
            .join("fixtures")
            .join("swarm_incidents");
        fs::create_dir_all(&fixture_dir).map_err(|error| error.to_string())?;

        fs::write(
            fixture_dir.join("schema_only.json"),
            r#"{"schema":"ee.swarm_incident.v1"}"#,
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            fixture_dir.join("bad_scenario.json"),
            stable_summary_json(&json!({
                "schema": "ee.swarm_incident.v1",
                "scenarioId": "Bad-Scenario",
                "fixedClock": "2026-06-11T00:00:00Z",
                "purpose": "invalid identifier should not become support evidence",
                "substrates": {},
                "expectedDegraded": [],
                "expectedRecoveryActions": [],
                "redactionExpectations": {},
                "assertions": {},
                "artifacts": []
            })),
        )
        .map_err(|error| error.to_string())?;

        let summary = collect_swarm_incident_summary(tempdir.path());
        assert_eq!(
            summary.pointer("/status"),
            Some(&json!("no_valid_incident_fixtures"))
        );
        assert_eq!(summary.pointer("/counts/fixtureCount"), Some(&json!(2)));
        assert_eq!(
            summary.pointer("/counts/summarizedIncidentCount"),
            Some(&json!(0))
        );
        assert_eq!(
            summary.pointer("/counts/malformedIncidentCount"),
            Some(&json!(2))
        );
        assert!(
            !stable_summary_json(&summary).contains(r#""scenarioId":"unknown""#),
            "invalid fixtures must not be summarized under a synthetic scenario id"
        );
        Ok(())
    }

    #[test]
    fn swarm_replay_summary_counts_schema_shaped_invalid_results_as_malformed() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let replay_root = tempdir
            .path()
            .join(crate::core::lab::SWARM_REPLAY_ARTIFACT_DIR_TAIL);
        let schema_only_dir = replay_root.join("schema_only");
        let bad_id_dir = replay_root.join("bad_id");
        fs::create_dir_all(&schema_only_dir).map_err(|error| error.to_string())?;
        fs::create_dir_all(&bad_id_dir).map_err(|error| error.to_string())?;

        fs::write(
            schema_only_dir.join(crate::core::lab::SWARM_REPLAY_RESULT_ARTIFACT_FILE),
            stable_summary_json(&json!({
                "schema": crate::core::lab::SWARM_REPLAY_RESULT_SCHEMA_V1
            })),
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            bad_id_dir.join(crate::core::lab::SWARM_REPLAY_RESULT_ARTIFACT_FILE),
            stable_summary_json(&json!({
                "schema": crate::core::lab::SWARM_REPLAY_RESULT_SCHEMA_V1,
                "workloadId": "not_a_swarm_workload_id",
                "runId": "not_a_swarm_run_id",
                "sideEffectFree": true,
                "status": "pass",
                "hostProfileAdmission": {},
                "commandResults": [],
                "aggregate": {},
                "redactionStatus": {},
                "resourceUsage": {},
                "firstFailure": null,
                "verification": {},
                "warnings": []
            })),
        )
        .map_err(|error| error.to_string())?;

        let summary = collect_swarm_replay_summary(tempdir.path());
        assert_eq!(
            summary.pointer("/status"),
            Some(&json!("no_valid_replay_artifacts"))
        );
        assert_eq!(
            summary.pointer("/counts/runDirectoryCount"),
            Some(&json!(2))
        );
        assert_eq!(
            summary.pointer("/counts/resultArtifactCount"),
            Some(&json!(2))
        );
        assert_eq!(
            summary.pointer("/counts/summarizedReplayCount"),
            Some(&json!(0))
        );
        assert_eq!(
            summary.pointer("/counts/malformedReplayCount"),
            Some(&json!(2))
        );
        let encoded = stable_summary_json(&summary);
        assert!(
            !encoded.contains(r#""workloadId":"unknown""#)
                && !encoded.contains(r#""runId":"unknown""#),
            "invalid replay results must not be summarized under synthetic identifiers"
        );
        Ok(())
    }

    #[test]
    fn verification_broker_recommendation_reuses_recent_evidence_under_rch_pressure() {
        let mut report = report_with_ready_sources();
        let mut broker = verification_broker_summary_fixture();
        broker.recent_reusable_run_count = 2;
        broker.rch_queue_status = "capacity_blocked".to_string();
        broker.rch_slots_available = Some(1);
        broker.rch_queue_head_slots_needed = Some(4);
        broker.rch_worker_pressure_status = "pressure_degraded".to_string();
        report.verification_broker = Some(broker);

        apply_swarm_brief_advice(&mut report);
        report.finalize();

        let rec = recommendation(&report, "rec.verification_broker.reuse_recent_evidence");
        assert_eq!(rec.kind, "verification_reuse");
        assert_eq!(rec.severity, "medium");
        assert!(
            rec.reason_codes
                .contains(&"verification_recent_reusable_run".to_string())
        );
        assert!(
            rec.reason_codes
                .contains(&"rch_queue_capacity_blocked".to_string())
        );
        assert!(
            rec.suggested_commands
                .iter()
                .any(|command| command.contains("ee verify broker lookup"))
        );
        assert!(
            rec.must_not_do
                .iter()
                .any(|warning| warning.contains("fresh RCH slot"))
        );

        let summary = summarize_swarm_brief_report(&report);
        assert_eq!(
            summary.pointer("/verificationBroker/recentReusableRunCount"),
            Some(&json!(2))
        );
        assert_eq!(
            summary.pointer("/verificationBroker/rawLogsIncluded"),
            Some(&json!(false))
        );
        assert_eq!(
            summary.pointer("/counts/verificationBrokerRecentReusableRunCount"),
            Some(&json!(2))
        );

        let rendered = render_swarm_brief_summary_for_handoff(&summary);
        assert!(rendered.contains("Verification broker posture:"));
        assert!(
            !rendered.contains("stdout") && !rendered.contains("stderr"),
            "handoff text must stay redaction-safe"
        );
    }

    #[test]
    fn verification_broker_recommendation_surfaces_known_blockers() {
        let mut report = report_with_ready_sources();
        let mut broker = verification_broker_summary_fixture();
        broker.status = "blocked".to_string();
        broker.advisory_counts.remote_failed = 1;
        broker.advisory_counts.local_disallowed = 1;
        broker.advisory_counts.topology_blocked = 1;
        broker.evidence_health.topology_blocked_count = 1;
        broker.evidence_health.issue_count = 2;
        broker.evidence_health.status = "blocked".to_string();
        broker.rch_worker_pressure_status = "topology_blocked".to_string();
        report.verification_broker = Some(broker);

        apply_swarm_brief_advice(&mut report);
        report.finalize();

        let rec = recommendation(&report, "rec.verification_broker.inspect_known_blocker");
        assert_eq!(rec.kind, "verification_known_blocker");
        assert_eq!(rec.severity, "high");
        assert!(
            rec.reason_codes
                .contains(&"verification_known_blocker_present".to_string())
        );
        assert!(
            rec.reason_codes
                .contains(&"verification_topology_blocked".to_string())
        );
        assert!(
            rec.must_not_do
                .iter()
                .any(|warning| warning.contains("local Cargo fallback"))
        );

        let summary = summarize_swarm_brief_report(&report);
        assert_eq!(
            summary.pointer("/verificationBroker/knownBlockerCount"),
            Some(&json!(3))
        );
        assert_eq!(
            summary.pointer("/counts/verificationBrokerKnownBlockerCount"),
            Some(&json!(3))
        );
    }

    #[test]
    fn verification_broker_recommendation_waits_for_in_flight_equivalent_run() {
        let mut report = report_with_ready_sources();
        let mut broker = verification_broker_summary_fixture();
        broker.status = "initializing".to_string();
        broker.in_flight_equivalent_command_count = 1;
        broker.advisory_counts.remote_in_flight = 1;
        report.verification_broker = Some(broker);

        apply_swarm_brief_advice(&mut report);
        report.finalize();

        let rec = recommendation(&report, "rec.verification_broker.wait_for_in_flight_run");
        assert_eq!(rec.kind, "verification_in_flight");
        assert!(
            rec.reason_codes
                .contains(&"verification_in_flight_equivalent_command".to_string())
        );
        assert!(
            rec.must_not_do
                .iter()
                .any(|warning| warning.contains("duplicate remote-required Cargo gate"))
        );
    }

    #[test]
    fn verification_broker_summary_handles_no_evidence_and_missing_rch_without_raw_payloads() {
        let mut report = report_with_ready_sources();
        report.verification_broker = Some(swarm_brief_verification_broker_summary(
            VerificationPostureReport::from_records(chrono::Utc::now(), &[]),
            None,
        ));

        apply_swarm_brief_advice(&mut report);
        report.finalize();

        assert!(
            report
                .recommendations
                .iter()
                .all(|recommendation| !recommendation.id.starts_with("rec.verification_broker."))
        );

        let summary = summarize_swarm_brief_report(&report);
        assert_eq!(
            summary.pointer("/verificationBroker/status"),
            Some(&json!("no_evidence"))
        );
        assert_eq!(
            summary.pointer("/verificationBroker/rchQueueStatus"),
            Some(&json!("not_collected"))
        );
        assert_eq!(
            summary.pointer("/verificationBroker/rawLogsIncluded"),
            Some(&json!(false))
        );
        assert_eq!(
            summary.pointer("/verificationBroker/rawMailBodiesIncluded"),
            Some(&json!(false))
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
                action_hint: None,
                blocked_by: Vec::new(),
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

        let pressure = require_some(report.ready_reservation_pressure.first(), "ready pressure");
        assert_eq!(pressure.bead_id, "eidetic_engine_cli-u7r5");
        assert_eq!(pressure.action, "wait");
        assert_eq!(pressure.severity, "high");
        assert_eq!(pressure.exclusive_reservation_count, 1);
        assert_eq!(pressure.shared_reservation_count, 0);
        assert_eq!(
            pressure.earliest_expires_at.as_deref(),
            Some("2026-05-09T08:00:00Z")
        );
        assert!(
            pressure
                .likely_surfaces
                .contains(&"src/core/swarm_brief.rs".to_string())
        );
        assert_eq!(pressure.reservation_holders, vec!["OtherAgent".to_string()]);
        assert!(
            pressure
                .risk_factors
                .contains(&"ready_bead_reservation_pressure".to_string())
        );

        report.finalize();
        let summary = summarize_swarm_brief_report(&report);
        assert_eq!(
            summary.pointer("/counts/readyReservationPressureCount"),
            Some(&json!(1))
        );
        assert_eq!(
            summary.pointer("/readyReservationPressureSummary/countsByAction/wait"),
            Some(&json!(1))
        );
        assert_eq!(
            summary.pointer("/readyReservationPressureSummary/topReadyBeads/0/action"),
            Some(&json!("wait"))
        );
        assert_eq!(
            summary.pointer("/readyReservationPressureSummary/topReadyBeads/0/rawSurfacesIncluded"),
            Some(&json!(false))
        );
        let holder_hash = blake3_summary_hash("OtherAgent");
        assert_eq!(
            summary.pointer(&format!(
                "/readyReservationPressureSummary/countsByReservationHolder/{holder_hash}"
            )),
            Some(&json!(1))
        );
        assert_eq!(
            summary
                .pointer("/readyReservationPressureSummary/topReadyBeads/0/reservationHolders/0"),
            Some(&json!(holder_hash))
        );
        assert!(
            summary
                .pointer("/readyReservationPressureSummary/topReadyBeads/0/likelySurfaceHashes/0")
                .and_then(Value::as_str)
                .is_some_and(|hash| hash.starts_with("blake3:")),
            "summary should hash likely surfaces"
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
    fn ready_reservation_pressure_chooses_alternate_clear_ready_work() {
        let mut report = report_with_ready_sources();
        report.beads.ready.push(bead(
            "eidetic_engine_cli-u7r5",
            "[swarm-brief][advisor] Add recommendations",
            "ready",
        ));
        report.beads.ready.push(bead(
            "eidetic_engine_cli-docs",
            "[docs] Update swarm runbook wording",
            "ready",
        ));
        report.file_reservations.push(SwarmBriefFileReservation {
            path_pattern: "src/core/swarm_brief.rs".to_string(),
            holder: "OtherAgent".to_string(),
            exclusive: true,
            expires_at: Some("2026-05-09T08:00:00Z".to_string()),
        });

        apply_swarm_brief_advice(&mut report);

        let pressure = require_some(
            report
                .ready_reservation_pressure
                .iter()
                .find(|item| item.bead_id == "eidetic_engine_cli-u7r5"),
            "conflicted ready pressure",
        );
        assert_eq!(pressure.action, "choose_another");
        assert!(
            pressure
                .suggested_commands
                .contains(&BEADS_READY_COMMAND.to_string())
        );
        assert!(
            report
                .ready_reservation_pressure
                .iter()
                .all(|item| item.bead_id != "eidetic_engine_cli-docs"),
            "clear ready bead should not be reported as pressured"
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
        let holder_hash = blake3_summary_hash("OtherAgent");
        assert_eq!(
            summary.pointer(&format!(
                "/fileSurfaceRiskSummary/countsByReservationHolder/{holder_hash}"
            )),
            Some(&json!(1))
        );
        assert_eq!(
            summary.pointer("/fileSurfaceRiskSummary/countsByGitStatus/M"),
            Some(&json!(1))
        );
        assert_eq!(
            summary.pointer("/fileSurfaceRiskSummary/topRisks/0/reservationHolders/0"),
            Some(&json!(holder_hash))
        );
        assert!(
            !rendered.contains("src/core/swarm_brief.rs"),
            "support-bundle summary must not include raw file listings"
        );
        assert!(
            !rendered.contains("OtherAgent"),
            "support-bundle summary must not include raw reservation holder labels"
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
    fn beads_parser_captures_fractional_activity_timestamps() {
        let beads = parse_beads_json(
            r#"[
              {
                "id": "bd-fractional",
                "title": "fractional timestamp fixture",
                "status": "in_progress",
                "issue_type": "bug",
                "priority": 2,
                "assignee": "BlueLake",
                "created_at": "2026-05-01T10:00:00.111111Z",
                "updated_at": "2026-05-01T11:00:00.222222Z",
                "comments": [
                  {"created_at": "2026-05-01T12:00:00.333333Z"},
                  {"created_at": "2026-05-01T13:00:00.444444Z"}
                ]
              }
            ]"#,
            "in_progress",
        )
        .expect("fractional Beads JSON parses");

        let bead = require_some(beads.first(), "fractional bead");
        assert_eq!(
            bead.created_at.as_deref(),
            Some("2026-05-01T10:00:00.111111Z")
        );
        assert_eq!(
            bead.updated_at.as_deref(),
            Some("2026-05-01T11:00:00.222222Z")
        );
        assert_eq!(
            bead.latest_comment_at.as_deref(),
            Some("2026-05-01T13:00:00.444444Z")
        );
        assert_eq!(bead.comment_count, 2);
        assert_eq!(bead.issue_type.as_deref(), Some("bug"));
        assert!(
            rfc3339_epoch_seconds("2026-05-01T13:00:00.444444Z").is_some(),
            "fractional RFC3339 timestamps must parse structurally"
        );
    }

    #[test]
    fn beads_parser_accepts_camel_case_issue_type_for_internal_routing() {
        let beads = parse_beads_json(
            r#"[
              {
                "id": "bd-epic",
                "title": "Epic wrapper",
                "status": "open",
                "issueType": "epic"
              }
            ]"#,
            "ready",
        )
        .expect("Beads JSON parses");

        let bead = require_some(beads.first(), "camel-case issue type bead");
        assert_eq!(bead.issue_type.as_deref(), Some("epic"));

        let rendered = serde_json::to_value(bead).expect("bead serializes");
        assert!(
            rendered.get("issueType").is_none(),
            "issue_type is internal routing metadata, not a swarm brief contract expansion"
        );
    }

    #[test]
    fn liveness_marks_old_unowned_in_progress_as_reclaim_candidate() {
        let mut report = report_with_ready_sources();
        let mut stale = bead(
            "bd-stale",
            "[swarm-brief] Old abandoned in-progress work",
            "in_progress",
        );
        stale.updated_at = Some("2000-01-01T00:00:00.123456Z".to_string());
        report.beads.in_progress.push(stale);

        apply_swarm_brief_advice(&mut report);

        let liveness = require_some(
            report
                .stalled_bead_liveness
                .iter()
                .find(|item| item.bead_id == "bd-stale"),
            "stale liveness",
        );
        assert_eq!(liveness.posture, "reclaim_candidate");
        assert_eq!(liveness.action, "reopen_manually");
        assert_eq!(liveness.severity, "high");
        assert!(
            liveness
                .suggested_commands
                .contains(&"br update bd-stale --status open --json".to_string())
        );
        assert!(
            liveness.must_not_do.contains(
                &"Do not auto-reopen in-progress work from swarm brief output.".to_string()
            )
        );
    }

    #[test]
    fn liveness_treats_recent_assignee_mail_activity_as_active_work() {
        let mut report = report_with_ready_sources();
        let mut stale = bead(
            "bd-active-agent",
            "[swarm-brief] Old in-progress with recently active assignee",
            "in_progress",
        );
        stale.assignee = Some("BlueLake".to_string());
        stale.updated_at = Some("2000-01-01T00:00:00Z".to_string());
        report.beads.in_progress.push(stale);
        report.agent_mail_agents.push(SwarmBriefAgentMailAgent {
            name: "BlueLake".to_string(),
            last_active_at: Some("9999-01-01T00:00:00Z".to_string()),
        });

        apply_swarm_brief_advice(&mut report);

        let liveness = require_some(
            report
                .stalled_bead_liveness
                .iter()
                .find(|item| item.bead_id == "bd-active-agent"),
            "active agent liveness",
        );
        assert_eq!(liveness.posture, "active");
        assert_eq!(liveness.action, "leave_alone");
        assert!(
            liveness
                .evidence_sources
                .contains(&"agent_mail_agent".to_string())
        );
        assert!(
            !liveness
                .suggested_commands
                .iter()
                .any(|command| command.contains("--status open")),
            "recent agent roster activity must suppress reopen guidance"
        );
    }

    #[test]
    fn liveness_marks_quiet_but_recent_after_active_window() {
        let report = report_with_ready_sources();
        let mut quiet = bead(
            "bd-quiet",
            "[swarm-brief] Quiet but recently updated in-progress work",
            "in_progress",
        );
        quiet.assignee = Some("QuietAgent".to_string());
        quiet.updated_at = Some("2026-05-01T00:00:00.123456Z".to_string());
        let activity_epoch =
            rfc3339_epoch_seconds("2026-05-01T00:00:00.123456Z").expect("quiet timestamp parses");
        let now_epoch = activity_epoch + STALLED_BEAD_ACTIVE_WINDOW_SECONDS + 60;

        let liveness = stalled_bead_liveness(&report, &quiet, now_epoch);

        assert_eq!(liveness.posture, "quiet_but_recent");
        assert_eq!(liveness.action, "message_holder");
        assert_eq!(liveness.severity, "low");
        assert_eq!(
            liveness.age_seconds,
            Some(STALLED_BEAD_ACTIVE_WINDOW_SECONDS + 60)
        );
        assert!(
            liveness
                .evidence_sources
                .contains(&"beads_updated_at".to_string())
        );
        assert!(
            !liveness
                .suggested_commands
                .iter()
                .any(|command| command.contains("--status open")),
            "quiet but recent work must not get reopen guidance"
        );
    }

    #[test]
    fn liveness_does_not_reclaim_when_agent_mail_is_degraded() {
        let mut report = report_with_ready_sources();
        for source in &mut report.sources {
            if source.source == SwarmBriefSourceKind::AgentMail {
                source.status = SwarmBriefSourceStatus::Unavailable;
            }
        }
        let mut stale = bead(
            "bd-stale-mail",
            "[swarm-brief] Old in-progress with missing mail source",
            "in_progress",
        );
        stale.updated_at = Some("2000-01-01T00:00:00Z".to_string());
        report.beads.in_progress.push(stale);

        apply_swarm_brief_advice(&mut report);

        let liveness = require_some(
            report
                .stalled_bead_liveness
                .iter()
                .find(|item| item.bead_id == "bd-stale-mail"),
            "degraded-mail liveness",
        );
        assert_eq!(liveness.posture, "stale_needs_message");
        assert_eq!(liveness.action, "message_holder");
        assert!(
            liveness
                .evidence
                .contains(&"source_status:agent_mail:not_ready".to_string())
        );
        assert!(
            liveness
                .must_not_do
                .contains(&"Do not treat missing Agent Mail data as inactivity proof.".to_string())
        );
    }

    #[test]
    fn liveness_treats_active_reservation_as_active_work() {
        let mut report = report_with_ready_sources();
        let mut owned = bead(
            "bd-active",
            "[swarm-brief] Active owner on swarm brief core",
            "in_progress",
        );
        owned.assignee = Some("BlueLake".to_string());
        owned.updated_at = Some("2000-01-01T00:00:00Z".to_string());
        report.beads.in_progress.push(owned);
        report.file_reservations.push(SwarmBriefFileReservation {
            path_pattern: "src/core/swarm_brief.rs".to_string(),
            holder: "BlueLake".to_string(),
            exclusive: true,
            expires_at: Some("9999-01-01T00:00:00Z".to_string()),
        });

        apply_swarm_brief_advice(&mut report);

        let liveness = require_some(
            report
                .stalled_bead_liveness
                .iter()
                .find(|item| item.bead_id == "bd-active"),
            "active liveness",
        );
        assert_eq!(liveness.posture, "active");
        assert_eq!(liveness.action, "leave_alone");
        assert!(
            liveness
                .evidence_sources
                .contains(&"agent_mail_reservation".to_string())
        );
    }

    #[test]
    fn liveness_treats_recent_git_commit_as_active_work() {
        let mut report = report_with_ready_sources();
        let mut stale = bead(
            "bd-git-active",
            "[swarm-brief] Old in-progress with fresh git activity",
            "in_progress",
        );
        stale.updated_at = Some("2000-01-01T00:00:00Z".to_string());
        let now_epoch =
            rfc3339_epoch_seconds("2026-05-01T12:00:00Z").expect("now timestamp parses");
        report.recent_commits.push(SwarmBriefCommit {
            hash: "abc123".to_string(),
            authored_at_epoch_seconds: Some(now_epoch - 60),
            subject: "finish bd-git-active liveness proof".to_string(),
        });

        let liveness = stalled_bead_liveness(&report, &stale, now_epoch);

        assert_eq!(liveness.posture, "active");
        assert_eq!(liveness.action, "leave_alone");
        assert!(
            liveness
                .evidence_sources
                .contains(&"git_commit".to_string())
        );
        assert!(
            liveness
                .evidence
                .contains(&"git_commit_mentions:bd-git-active:abc123".to_string())
        );
        assert!(
            !liveness
                .suggested_commands
                .iter()
                .any(|command| command.contains("--status open")),
            "recent git activity must suppress reopen guidance"
        );
    }

    #[test]
    fn liveness_keeps_human_approval_blockers_out_of_reclaim_candidates() {
        let mut report = report_with_ready_sources();
        let mut blocker = bead(
            "bd-human",
            "[cleanup] deletion approval required before removing snapshots",
            "in_progress",
        );
        blocker.updated_at = Some("2000-01-01T00:00:00Z".to_string());
        report.beads.in_progress.push(blocker);

        apply_swarm_brief_advice(&mut report);

        let liveness = require_some(
            report
                .stalled_bead_liveness
                .iter()
                .find(|item| item.bead_id == "bd-human"),
            "human approval liveness",
        );
        assert_eq!(liveness.posture, "human_approval_required");
        assert_eq!(liveness.action, "request_human_approval");
        assert!(
            !liveness
                .suggested_commands
                .iter()
                .any(|command| command.contains("--status open")),
            "human approval blockers must not get reopen guidance"
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
    fn advisor_blocks_commit_actions_during_git_operation_state() {
        let mut report = report_with_ready_sources();
        report.git_operation_state = WorkspaceGitOperationState {
            in_progress: true,
            operations: vec![WorkspaceGitOperationMarker {
                operation: "rebase",
                marker_path: "rebase-merge",
                marker_type: "directory".to_string(),
            }],
            autostash_markers: vec![WorkspaceGitOperationMarker {
                operation: "autostash",
                marker_path: "rebase-merge/autostash",
                marker_type: "file".to_string(),
            }],
        };

        apply_swarm_brief_advice(&mut report);

        let rec = recommendation(&report, "rec.git.operation_in_progress");
        assert_eq!(rec.kind, "git_operation_state");
        assert_eq!(rec.severity, "critical");
        assert!(
            rec.reason_codes
                .contains(&"git_operation:rebase".to_string())
        );
        assert!(
            rec.reason_codes
                .contains(&"git_autostash_marker_present".to_string())
        );
        assert!(
            rec.must_not_do
                .iter()
                .any(|item| item.contains("Do not stage or commit"))
        );
        assert!(rec.suggested_commands.contains(&"git status".to_string()));
    }

    #[test]
    fn advisor_recommends_coordination_for_mixed_owner_ahead_commits() {
        let mut report = report_with_ready_sources();
        report.git_ahead = Some(summarize_git_ahead(
            "# branch.head main\n# branch.upstream origin/main\n# branch.ab +2 -0\n",
            Some(concat!(
                "aaaaaaaaaaaaaaaa\x1fCodex\x1ffix: parser (bd-2gc7r.1)\n",
                "bbbbbbbbbbbbbbbb\x1fPeerAgent\x1ftest: fixture (bd-peer.2)\n",
            )),
        ));

        apply_swarm_brief_advice(&mut report);

        let rec = recommendation(&report, "rec.git.coordinate_mixed_owner_ahead");
        assert_eq!(rec.kind, "push_safety");
        assert_eq!(rec.severity, "high");
        assert!(
            rec.reason_codes
                .contains(&"git_ahead_mixed_author".to_string())
        );
        assert!(
            rec.suggested_commands
                .contains(&"git log origin/main..HEAD --oneline --decorate".to_string())
        );
        assert!(
            rec.must_not_do
                .iter()
                .any(|item| item.contains("automatically push"))
        );

        let summary = summarize_swarm_brief_report(&report);
        assert_eq!(
            summary.pointer("/gitAhead/peerOwnedAheadRisk"),
            Some(&json!(true))
        );
        let rendered = render_swarm_brief_summary_for_handoff(&summary);
        assert!(rendered.contains("Push-safety posture:"));
        assert!(rendered.contains("coordinate and inspect git log origin/main..HEAD"));
        assert!(!rendered.contains("fix: parser"));
    }

    #[test]
    fn advisor_stays_quiet_for_clean_and_single_owner_ahead() {
        for snapshot in [
            summarize_git_ahead(
                "# branch.head main\n# branch.upstream origin/main\n# branch.ab +0 -0\n",
                Some(""),
            ),
            summarize_git_ahead(
                "# branch.head main\n# branch.upstream origin/main\n# branch.ab +1 -0\n",
                Some("aaaaaaaaaaaaaaaa\x1fCodex\x1ffix: parser (bd-2gc7r.1)\n"),
            ),
            summarize_git_ahead(
                "# branch.head main\n# branch.upstream origin/main\n# branch.ab +1 -0\n",
                Some(
                    "bbbbbbbbbbbbbbbb\x1fCodex\x1fchore(beads): file review findings from R1 cod_4 pass\n",
                ),
            ),
        ] {
            let mut report = report_with_ready_sources();
            report.git_ahead = Some(snapshot);

            apply_swarm_brief_advice(&mut report);

            assert!(
                report
                    .recommendations
                    .iter()
                    .all(|recommendation| recommendation.id
                        != "rec.git.coordinate_mixed_owner_ahead")
            );
        }
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
    fn advisor_recommends_memory_drift_revalidation_without_raw_content() {
        let mut report = report_with_ready_sources();
        let mut source_kind_counts = BTreeMap::new();
        source_kind_counts.insert("provenance_chain".to_string(), 2);
        source_kind_counts.insert("pack_record".to_string(), 1);
        report.memory_drift = Some(SwarmBriefMemoryDriftSummary {
            status: "degraded".to_string(),
            report_mode: "recent_pack_items".to_string(),
            total_memories: 4,
            current_count: 1,
            changed_count: 1,
            missing_source_count: 1,
            stale_anchor_count: 1,
            unverifiable_count: 0,
            suppressed_count: 0,
            affected_count: 3,
            top_affected_memory_ids: vec![
                "mem_changed".to_string(),
                "mem_missing".to_string(),
                "mem_stale".to_string(),
            ],
            degraded_codes: vec![
                "memory_drift_source_changed".to_string(),
                "memory_drift_source_missing".to_string(),
            ],
            source_kind_counts,
        });
        let degradation = SwarmBriefDegradation::warning(
            SwarmBriefSourceKind::MemoryDrift,
            "memory_drift_source_missing",
            "missing provenance for recent pack item",
            Some("ee memory drift --mode recent-pack-items --json".to_string()),
        );
        let source = require_some(
            report
                .sources
                .iter_mut()
                .find(|source| source.source == SwarmBriefSourceKind::MemoryDrift),
            "memory drift source",
        );
        source.status = SwarmBriefSourceStatus::Degraded;
        source.item_count = 3;
        source.degraded = vec![degradation.clone()];
        report.degraded.push(degradation);

        apply_swarm_brief_advice(&mut report);
        report.finalize();

        let rec = recommendation(&report, "rec.memory_drift.revalidate_recent_pack_items");
        assert_eq!(rec.kind, "memory_drift_revalidation");
        assert_eq!(rec.severity, "high");
        assert!(
            rec.reason_codes
                .contains(&"memory_drift_queue_non_empty".to_string())
        );
        assert!(
            rec.reason_codes
                .contains(&"memory_drift_source_missing".to_string())
        );
        assert!(
            rec.evidence
                .contains(&"memory_drift_affected:3".to_string())
        );
        assert!(
            rec.suggested_commands
                .contains(&"ee memory drift --mode recent-pack-items --json".to_string())
        );
        assert!(
            rec.must_not_do
                .iter()
                .any(|item| item.contains("without revalidation"))
        );

        let summary = summarize_swarm_brief_report(&report);
        assert_eq!(
            summary.pointer("/memoryDrift/affectedCount"),
            Some(&json!(3))
        );
        assert_eq!(
            summary.pointer("/counts/memoryDriftAffectedCount"),
            Some(&json!(3))
        );
        let rendered = stable_summary_json(&summary);
        assert!(
            !rendered.contains("missing provenance for recent pack item"),
            "support-bundle swarm summary must not include raw degradation messages"
        );
        assert_eq!(
            summary.pointer("/memoryDrift/rawSnippetsIncluded"),
            Some(&json!(false))
        );
        assert_eq!(
            summary.pointer("/memoryDrift/rawCommandBodiesIncluded"),
            Some(&json!(false))
        );
    }

    #[test]
    fn successful_memory_drift_findings_preserve_source_authority_and_top_level_blocker() {
        let degradation = SwarmBriefDegradation::with_severity(
            SwarmBriefSourceKind::MemoryDrift,
            "memory_drift_source_unverifiable",
            "medium",
            "A recent pack selected a superseded memory revision.",
            Some("ee memory drift --mode recent-pack-items --json".to_owned()),
        );
        let source = SwarmBriefSourceSnapshot::ready(
            SwarmBriefSourceKind::MemoryDrift,
            SwarmBriefSourceProvenance::local_probe(),
            1,
        )
        .with_authoritative_findings(vec![degradation.clone()]);
        assert_eq!(source.status, SwarmBriefSourceStatus::Ready);

        let mut report = SwarmBriefReport::empty(Path::new("/tmp/project"));
        report.sources.push(source);
        report.degraded.push(degradation);
        report.finalize();

        let memory_source = report
            .sources
            .iter()
            .find(|source| source.source == SwarmBriefSourceKind::MemoryDrift)
            .expect("memory-drift source remains present");
        assert_eq!(memory_source.status, SwarmBriefSourceStatus::Ready);
        assert!(
            memory_source
                .degraded
                .iter()
                .any(|item| { item.code == "memory_drift_source_unverifiable" })
        );
        assert!(report.degraded.iter().any(|item| {
            item.source == SwarmBriefSourceKind::MemoryDrift
                && item.code == "memory_drift_source_unverifiable"
        }));
    }

    #[test]
    fn summary_and_handoff_text_surface_symbol_risk_without_raw_names() {
        let mut report = report_with_ready_sources();
        report.workspace_hygiene =
            Some(crate::core::workspace::WorkspaceHygieneSwarmBriefSummary {
                schema: crate::core::workspace::WORKSPACE_HYGIENE_SWARM_BRIEF_SUMMARY_SCHEMA_V1,
                status: "available",
                dirty_path_count: 1,
                bucket_counts: Vec::new(),
                kind_counts: Vec::new(),
                needs_human_review_top: Vec::new(),
                needs_human_review_total: 0,
                needs_human_review_truncated: false,
                coordination_blocker_count: 0,
                coordination_blocker_patterns: Vec::new(),
                beads_state_status: "beads_clean",
                command_hint: crate::core::workspace::WORKSPACE_HYGIENE_SWARM_BRIEF_COMMAND_HINT,
                degraded_codes: Vec::new(),
                symbol_risk_summary: Some(
                    crate::core::workspace::WorkspaceHygieneSymbolRiskSummary {
                        schema: crate::core::workspace::WORKSPACE_HYGIENE_SYMBOL_RISK_SCHEMA_V1,
                        status: "available",
                        dirty_path_count: 1,
                        summarized_path_count: 1,
                        omitted_path_count: 0,
                        touched_symbol_count: 1,
                        high_risk_symbol_count: 1,
                        linked_evidence_count: 1,
                        recent_agent_activity_count: 1,
                        paths: vec![crate::core::workspace::WorkspaceHygieneSymbolRiskPath {
                            path: "src/core/private_symbol.rs".to_owned(),
                            path_hash: blake3_summary_hash("src/core/private_symbol.rs"),
                            symbol_count: 1,
                            high_risk_symbol_count: 1,
                            linked_evidence_count: 1,
                            recent_agent_activity_count: 1,
                            symbols: vec![
                                crate::core::workspace::WorkspaceHygieneSymbolRiskSymbol {
                                    symbol_id_hash: blake3_summary_hash("sym_private_symbol"),
                                    canonical_name_hash: blake3_summary_hash("private_symbol"),
                                    kind: "function",
                                    visibility: "public",
                                    public_surface: true,
                                    start_line: 7,
                                    end_line: 9,
                                    linked_evidence_count: 1,
                                    evidence_source_kinds: vec!["failure".to_owned()],
                                },
                            ],
                            agent_name_hashes: vec![blake3_summary_hash("LavenderHollow")],
                            evidence_source_kinds: vec!["failure".to_owned()],
                        }],
                        degraded_codes: vec!["symbol_evidence_links_unavailable".to_owned()],
                    },
                ),
            });

        let summary = summarize_swarm_brief_report(&report);
        assert_eq!(
            summary.pointer("/symbolRiskSummary/highRiskSymbolCount"),
            Some(&json!(1))
        );
        assert_eq!(
            summary.pointer("/symbolRiskSummary/topPaths/0/rawPathIncluded"),
            Some(&json!(false))
        );
        assert_eq!(
            summary.pointer("/counts/symbolRiskHighRiskSymbolCount"),
            Some(&json!(1))
        );

        let rendered = stable_summary_json(&summary);
        assert!(
            !rendered.contains("src/core/private_symbol.rs")
                && !rendered.contains("private_symbol")
                && !rendered.contains("LavenderHollow"),
            "support-bundle swarm summary must not leak raw paths, symbols, or agent names"
        );

        let handoff_text = render_swarm_brief_summary_for_handoff(&summary);
        assert!(
            handoff_text.contains("Symbol-risk posture: status=available"),
            "handoff text should include compact symbol-risk posture"
        );
        assert!(
            !handoff_text.contains("private_symbol") && !handoff_text.contains("LavenderHollow"),
            "handoff text must not include raw symbol or agent labels"
        );
    }

    #[test]
    fn advisor_reports_unavailable_memory_drift_report_as_unknown_evidence() {
        let mut report = report_with_ready_sources();
        let source = require_some(
            report
                .sources
                .iter_mut()
                .find(|source| source.source == SwarmBriefSourceKind::MemoryDrift),
            "memory drift source",
        );
        source.status = SwarmBriefSourceStatus::Unavailable;
        source.degraded = vec![SwarmBriefDegradation::warning(
            SwarmBriefSourceKind::MemoryDrift,
            MEMORY_DRIFT_REPORT_UNAVAILABLE_CODE,
            memory_drift_report_unavailable_message("database is missing"),
            Some("ee doctor --json".to_string()),
        )];

        apply_swarm_brief_advice(&mut report);

        let rec = recommendation(
            &report,
            "rec.degraded.memory_drift.memory_drift_report_unavailable",
        );
        assert_eq!(rec.kind, "degraded_capability");
        assert!(
            rec.evidence.contains(
                &"could_not_know:memory_drift:recent pack memory drift posture".to_string()
            )
        );
        assert!(
            rec.must_not_do
                .iter()
                .any(|item| item.contains("degraded memory_drift data"))
        );
    }

    #[test]
    fn memory_drift_adapter_reports_missing_database_without_raw_path() {
        let tempdir = tempfile::tempdir().expect("temp workspace");
        let options = SwarmBriefCollectOptions::for_workspace(tempdir.path());

        let output = MemoryDriftSourceAdapter.collect(&options);

        assert_eq!(output.snapshot.source, SwarmBriefSourceKind::MemoryDrift);
        assert_eq!(output.snapshot.status, SwarmBriefSourceStatus::Unavailable);
        assert_eq!(
            output
                .snapshot
                .degraded
                .first()
                .map(|item| item.code.as_str()),
            Some(MEMORY_DRIFT_REPORT_UNAVAILABLE_CODE)
        );
        assert_eq!(
            output.snapshot.degraded.first().map(|item| item.severity),
            Some("warning")
        );
        assert!(output.snapshot.degraded.first().is_some_and(|item| {
            item.message
                .starts_with(MEMORY_DRIFT_REPORT_UNAVAILABLE_MESSAGE_PREFIX)
        }));
        assert_eq!(
            output
                .snapshot
                .degraded
                .first()
                .and_then(|item| item.repair.as_deref()),
            Some("ee doctor --json")
        );
        assert!(matches!(output.contribution, SwarmBriefContribution::None));
        let rendered = stable_summary_json(
            &serde_json::to_value(&output.snapshot).expect("snapshot should serialize"),
        );
        assert!(
            !rendered.contains(tempdir.path().to_string_lossy().as_ref()),
            "missing database degradation must not expose raw temporary workspace path"
        );
    }

    #[test]
    fn advisor_prefers_rch_under_high_pressure_host() {
        let mut report = report_with_ready_sources();
        report.host_profile = Some(SwarmBriefHostProfileSummary {
            recommended_profile: "constrained".to_string(),
            confidence: "high".to_string(),
            host_class: "constrained".to_string(),
            calibration_freshness: "fresh".to_string(),
            target_dir_posture: "shared".to_string(),
            topology_warnings: Vec::new(),
            repair_action_kinds: Vec::new(),
            budget_delta_count: 5,
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
    fn summary_recommendations_follow_canonical_severity_order() {
        let mut report = report_with_ready_sources();
        report.recommendations = ["info", "low", "warning", "medium", "high", "critical"]
            .into_iter()
            .map(|severity| SwarmBriefRecommendation {
                id: format!("rec.severity.{severity}"),
                kind: "severity_order".to_string(),
                confidence: "high".to_string(),
                severity: severity.to_string(),
                reason_codes: vec![format!("severity_{severity}")],
                evidence: Vec::new(),
                suggested_commands: Vec::new(),
                must_not_do: Vec::new(),
            })
            .collect();

        let summary = summarize_swarm_brief_report(&report);
        let ids = summary
            .pointer("/topRecommendations")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("topRecommendations must be an array"))
            .iter()
            .map(|entry| {
                entry
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("recommendation entry must have an id"))
            })
            .collect::<Vec<_>>();

        assert_eq!(
            ids,
            vec![
                "rec.severity.critical",
                "rec.severity.high",
                "rec.severity.medium",
                "rec.severity.warning",
                "rec.severity.low",
                "rec.severity.info",
            ],
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
    fn git_source_adapter_collects_ahead_snapshot_without_mutating_git_state() {
        let options = SwarmBriefCollectOptions::for_workspace(".");
        let runner = FakeRunner::default()
            .with_output(
                "git",
                &["status", "--short", "--branch", "--untracked-files=all"],
                "## main...origin/main [ahead 2]\n M src/lib.rs\n",
            )
            .with_output(
                "git",
                &["log", "-n", "8", "--format=%H%x1f%ct%x1f%s"],
                "cccccccccccccccc\x1f1778352000\x1ffix: recent subject\n",
            )
            .with_output(
                "git",
                &["status", "--porcelain=v2", "--branch"],
                "# branch.head main\n# branch.upstream origin/main\n# branch.ab +2 -0\n",
            )
            .with_output(
                "git",
                &["log", "origin/main..HEAD", "--format=%H%x1f%an%x1f%s"],
                concat!(
                    "aaaaaaaaaaaaaaaa\x1fCodex\x1ffix: parser (bd-2gc7r.1)\n",
                    "bbbbbbbbbbbbbbbb\x1fPeerAgent\x1ftest: fixture (bd-peer.2)\n",
                ),
            );

        let output = GitSourceAdapter { runner: &runner }.collect(&options);

        assert_eq!(output.snapshot.status, SwarmBriefSourceStatus::Ready);
        match output.contribution {
            SwarmBriefContribution::Git {
                git_ahead: Some(snapshot),
                ..
            } => {
                assert_eq!(snapshot.ahead_count, 2);
                assert!(snapshot.peer_owned_ahead_risk);
                assert!(snapshot.mixed_author_ahead);
            }
            other => panic!("expected git contribution with ahead snapshot, got {other:?}"),
        }
        assert_eq!(
            runner.calls(),
            vec![
                "git status --short --branch --untracked-files=all".to_string(),
                "git log -n 8 --format=%H%x1f%ct%x1f%s".to_string(),
                "git status --porcelain=v2 --branch".to_string(),
                "git log origin/main..HEAD --format=%H%x1f%an%x1f%s".to_string(),
            ]
        );
    }

    #[test]
    fn git_source_adapter_marks_missing_upstream_as_degraded_without_log_probe() {
        let options = SwarmBriefCollectOptions::for_workspace(".");
        let runner = FakeRunner::default()
            .with_output(
                "git",
                &["status", "--short", "--branch", "--untracked-files=all"],
                "## main\n",
            )
            .with_output("git", &["log", "-n", "8", "--format=%H%x1f%ct%x1f%s"], "")
            .with_output(
                "git",
                &["status", "--porcelain=v2", "--branch"],
                "# branch.head main\n# branch.ab +0 -0\n",
            );

        let output = GitSourceAdapter { runner: &runner }.collect(&options);

        assert_eq!(output.snapshot.status, SwarmBriefSourceStatus::Degraded);
        assert!(
            output
                .snapshot
                .degraded
                .iter()
                .any(|entry| entry.code == "git_ahead_no_upstream")
        );
        match output.contribution {
            SwarmBriefContribution::Git {
                git_ahead: Some(snapshot),
                ..
            } => {
                assert_eq!(snapshot.state, "no_upstream");
                assert!(!snapshot.peer_owned_ahead_risk);
            }
            other => {
                panic!("expected git contribution with degraded ahead snapshot, got {other:?}")
            }
        }
        assert_eq!(
            runner.calls(),
            vec![
                "git status --short --branch --untracked-files=all".to_string(),
                "git log -n 8 --format=%H%x1f%ct%x1f%s".to_string(),
                "git status --porcelain=v2 --branch".to_string(),
            ]
        );
    }

    #[test]
    fn workspace_git_porcelain_v2_parser_preserves_states_and_renames() {
        let entries = parse_workspace_git_status_porcelain_v2(concat!(
            "# branch.head main\n",
            "1 .M N... 100644 100644 100644 abc def src/z.rs\n",
            "? src/a.rs\n",
            "2 R. N... 100644 100644 100644 abc def R100 src/new.rs\tsrc/old.rs\n",
            "u UU N... 100644 100644 100644 100644 aaa bbb ccc src/conflict.rs\n",
            "! ignored.log\n",
        ));

        assert_eq!(
            entries
                .iter()
                .map(|entry| (
                    entry.path.as_str(),
                    entry.original_path.as_deref(),
                    entry.staged.as_str(),
                    entry.unstaged.as_str(),
                    entry.entry_kind.as_str(),
                ))
                .collect::<Vec<_>>(),
            vec![
                ("src/a.rs", None, "?", "?", "untracked"),
                ("src/conflict.rs", None, "U", "U", "unmerged"),
                (
                    "src/new.rs",
                    Some("src/old.rs"),
                    "R",
                    ".",
                    "renamed_or_copied"
                ),
                ("src/z.rs", None, ".", "M", "ordinary"),
            ]
        );
    }

    #[test]
    fn workspace_git_porcelain_v2_parser_handles_boundary_states_deterministically() {
        assert!(parse_workspace_git_status_porcelain_v2("# branch.head main\n").is_empty());

        let entries = parse_workspace_git_status_porcelain_v2(concat!(
            "1 MM N... 100644 100644 100644 abc def src/both.rs\n",
            "1 D. N... 100644 000000 000000 abc 000 deleted/staged.rs\n",
            "1 .D N... 100644 100644 000000 abc def deleted/worktree.rs\n",
            "? src/both.rs\n",
            "1 MM N... 100644 100644 100644 abc def ../outside.rs\n",
            "1 MM N... 100644 100644 100644 abc def /abs/outside.rs\n",
        ));

        assert_eq!(
            entries
                .iter()
                .map(|entry| (
                    entry.path.as_str(),
                    entry.staged.as_str(),
                    entry.unstaged.as_str(),
                    entry.entry_kind.as_str(),
                ))
                .collect::<Vec<_>>(),
            vec![
                ("deleted/staged.rs", "D", ".", "ordinary"),
                ("deleted/worktree.rs", ".", "D", "ordinary"),
                ("src/both.rs", "?", "?", "untracked"),
                ("src/both.rs", "M", "M", "ordinary"),
            ]
        );
    }

    #[test]
    fn workspace_git_porcelain_v2_parser_unquotes_git_escaped_paths() {
        let entries = parse_workspace_git_status_porcelain_v2(concat!(
            "1 .M N... 100644 100644 100644 abc def \"src/quote\\\"name.rs\"\n",
            "? \"scratch/tab\\011name.txt\"\n",
            "? \"scratch/bel\\aname.txt\"\n",
            "? \"scratch/vtab\\vname.txt\"\n",
            "2 R. N... 100644 100644 100644 abc def R100 \"src/new\\\\name.rs\"\t\"src/old\\\\name.rs\"\n",
        ));

        assert_eq!(
            entries
                .iter()
                .map(|entry| (
                    entry.path.as_str(),
                    entry.original_path.as_deref(),
                    entry.staged.as_str(),
                    entry.unstaged.as_str(),
                    entry.entry_kind.as_str(),
                ))
                .collect::<Vec<_>>(),
            vec![
                ("scratch/bel\u{0007}name.txt", None, "?", "?", "untracked"),
                ("scratch/tab\tname.txt", None, "?", "?", "untracked"),
                ("scratch/vtab\u{000b}name.txt", None, "?", "?", "untracked"),
                (
                    "src/new\\name.rs",
                    Some("src/old\\name.rs"),
                    "R",
                    ".",
                    "renamed_or_copied"
                ),
                ("src/quote\"name.rs", None, ".", "M", "ordinary"),
            ]
        );
    }

    #[test]
    fn workspace_git_porcelain_v2_parser_decodes_multibyte_octal_paths() {
        let entries = parse_workspace_git_status_porcelain_v2(concat!(
            "1 .M N... 100644 100644 100644 abc def \"src/caf\\303\\251.rs\"\n",
            "? \"scratch/snowman-\\342\\230\\203.txt\"\n",
            "2 R. N... 100644 100644 100644 abc def R100 \"renamed/ni\\303\\261o.rs\"\t\"old/ni\\303\\261o.rs\"\n",
        ));

        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.path.as_str(), entry.original_path.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("renamed/ni\u{00f1}o.rs", Some("old/ni\u{00f1}o.rs")),
                ("scratch/snowman-\u{2603}.txt", None),
                ("src/caf\u{00e9}.rs", None),
            ]
        );
    }

    #[test]
    fn workspace_git_porcelain_v2_parser_rejects_malformed_quoted_paths() {
        let entries = parse_workspace_git_status_porcelain_v2(concat!(
            "1 .M N... 100644 100644 100644 abc def \"src/bad\\q.rs\"\n",
            "1 .M N... 100644 100644 100644 abc def \"src/bad\\303.rs\"\n",
            "? \"\"\n",
            "? \"../escaped-outside.rs\"\n",
        ));

        assert!(entries.is_empty());
    }

    #[test]
    fn workspace_git_porcelain_v2_parser_preserves_submodule_state() {
        let entries = parse_workspace_git_status_porcelain_v2(concat!(
            "1 .M S..U 160000 160000 160000 abc def vendor/untracked-submodule\n",
            "2 R. SCM. 160000 160000 160000 abc def R100 vendor/new\tvendor/old\n",
            "u UU S.M. 160000 160000 160000 160000 aaa bbb ccc vendor/conflict\n",
            "? ordinary-untracked.txt\n",
        ));

        let states = entries
            .iter()
            .map(|entry| {
                (
                    entry.path.as_str(),
                    entry.submodule_state.as_ref().map(|state| {
                        (
                            state.raw.as_str(),
                            state.commit_changed,
                            state.tracked_changes,
                            state.untracked_changes,
                        )
                    }),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            states,
            vec![
                ("ordinary-untracked.txt", None),
                ("vendor/conflict", Some(("S.M.", false, true, false))),
                ("vendor/new", Some(("SCM.", true, true, false))),
                (
                    "vendor/untracked-submodule",
                    Some(("S..U", false, false, true))
                ),
            ]
        );
    }

    #[test]
    fn workspace_git_snapshot_provider_uses_only_read_only_git_status_commands() {
        let workspace = Path::new("/repo/subdir");
        let runner = FakeRunner::default()
            .with_output("git", &["rev-parse", "--show-toplevel"], "/repo\n")
            .with_output(
                "git",
                &[
                    "status",
                    "--porcelain=v2",
                    "--branch",
                    "--untracked-files=all",
                ],
                "? scratch.txt\n1 M. N... 100644 100644 100644 abc def src/lib.rs\n",
            );
        let mut options = WorkspaceGitSnapshotOptions::for_workspace(workspace);
        options.large_file_threshold_bytes = 8;

        let snapshot = match collect_workspace_git_snapshot(&options, &runner) {
            Ok(snapshot) => snapshot,
            Err(error) => panic!("workspace git snapshot should parse: {error:?}"),
        };

        assert_eq!(snapshot.repository_root, "/repo");
        assert_eq!(
            snapshot
                .entries
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            vec!["scratch.txt", "src/lib.rs"]
        );
        assert!(
            snapshot
                .entries
                .iter()
                .all(|entry| entry.metadata.as_ref().is_some_and(|metadata| {
                    !metadata.exists
                        && metadata.skip_reason.as_deref() == Some("metadata_unavailable")
                }))
        );
        assert_eq!(
            runner.calls(),
            vec![
                "git rev-parse --show-toplevel".to_string(),
                "git status --porcelain=v2 --branch --untracked-files=all".to_string(),
            ]
        );
    }

    #[test]
    fn workspace_git_snapshot_reports_operation_state_from_metadata_only() {
        let temp = tempfile::Builder::new()
            .prefix("ee-workspace-git-operation-state-")
            .tempdir()
            .expect("create temp repo root");
        let workspace = temp.path();
        std::fs::create_dir_all(workspace.join(".git/rebase-merge"))
            .expect("create synthetic rebase metadata");
        std::fs::write(
            workspace.join(".git/rebase-merge/autostash"),
            "fake-secret-oid\n",
        )
        .expect("write synthetic autostash marker");
        std::fs::write(workspace.join(".git/CHERRY_PICK_HEAD"), "fake-secret-oid\n")
            .expect("write synthetic cherry-pick marker");
        let autostash_before = std::fs::read(workspace.join(".git/rebase-merge/autostash"))
            .expect("read synthetic autostash marker before snapshot");
        let cherry_pick_before = std::fs::read(workspace.join(".git/CHERRY_PICK_HEAD"))
            .expect("read synthetic cherry-pick marker before snapshot");

        let workspace_label = workspace.display().to_string();
        let runner = FakeRunner::default()
            .with_output("git", &["rev-parse", "--show-toplevel"], &workspace_label)
            .with_output(
                "git",
                &[
                    "status",
                    "--porcelain=v2",
                    "--branch",
                    "--untracked-files=all",
                ],
                "",
            );
        let snapshot = collect_workspace_git_snapshot(
            &WorkspaceGitSnapshotOptions::for_workspace(workspace),
            &runner,
        )
        .expect("workspace git snapshot should collect synthetic operation state");

        assert_eq!(
            std::fs::read(workspace.join(".git/rebase-merge/autostash"))
                .expect("read synthetic autostash marker after snapshot"),
            autostash_before
        );
        assert_eq!(
            std::fs::read(workspace.join(".git/CHERRY_PICK_HEAD"))
                .expect("read synthetic cherry-pick marker after snapshot"),
            cherry_pick_before
        );
        assert!(snapshot.operation_state.in_progress);
        assert_eq!(
            snapshot
                .operation_state
                .operations
                .iter()
                .map(|marker| (
                    marker.operation,
                    marker.marker_path,
                    marker.marker_type.as_str()
                ))
                .collect::<Vec<_>>(),
            vec![
                ("cherry_pick", "CHERRY_PICK_HEAD", "file"),
                ("rebase", "rebase-merge", "directory"),
            ]
        );
        assert_eq!(
            snapshot
                .operation_state
                .autostash_markers
                .iter()
                .map(|marker| (
                    marker.operation,
                    marker.marker_path,
                    marker.marker_type.as_str()
                ))
                .collect::<Vec<_>>(),
            vec![("autostash", "rebase-merge/autostash", "file")]
        );
        let serialized =
            serde_json::to_value(&snapshot.operation_state).expect("operation state serializes");
        assert!(
            !serialized.to_string().contains("fake-secret-oid"),
            "operation state must not expose marker file contents"
        );
    }

    #[test]
    fn workspace_git_metadata_classifies_existing_source_file_without_following_content() {
        let metadata =
            workspace_git_path_metadata(Path::new("."), "src/core/swarm_brief.rs", u64::MAX);

        assert!(metadata.exists);
        assert_eq!(metadata.file_type, "file");
        assert!(metadata.size_bytes.is_some_and(|bytes| bytes > 0));
        assert!(!metadata.large_file);
        assert_eq!(metadata.skip_reason, None);
    }

    #[test]
    fn workspace_git_metadata_marks_large_files_without_reporting_size() {
        let metadata = workspace_git_path_metadata(Path::new("."), "src/core/swarm_brief.rs", 1);

        assert!(metadata.exists);
        assert_eq!(metadata.file_type, "file");
        assert_eq!(metadata.size_bytes, None);
        assert!(metadata.large_file);
        assert_eq!(
            metadata.skip_reason.as_deref(),
            Some("large_file_metadata_only")
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
                    {"id":"work-2","title":"second","score":0.25}
                  ]
                },
                "recommendations": [
                  {
                    "id":"work-1",
                    "title":"first",
                    "score":0.5,
                    "action":"Work on bd-parent first",
                    "blocked_by":["bd-parent","bd-parent"]
                  }
                ],
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
        assert_eq!(
            summary.top_picks[0].action_hint.as_deref(),
            Some("Work on bd-parent first")
        );
        assert_eq!(summary.top_picks[0].blocked_by, vec!["bd-parent"]);
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
              "agents": [
                {"name":"IndigoBrook","last_active_ts":"2026-05-09T01:00:00.123456Z","body_md":"SECRET_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz123456"}
              ],
              "threads": [
                {"thread_id":"eidetic_engine_cli-abwd","subject":"Use token ghp_abcdefghijklmnopqrstuvwxyz123456","message_count":3,"body_md":"raw body"}
              ]
            }"#,
            ),
            "valid mail snapshot",
        );

        let reservations = &snapshot.file_reservations;
        let agents = &snapshot.agents;
        let inbox = &snapshot.inbox;
        let threads = &snapshot.threads;

        assert_eq!(snapshot.agent_name, None);
        assert_eq!(reservations.len(), 1);
        assert_eq!(reservations[0].path_pattern, "src/core/*.rs");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "IndigoBrook");
        assert_eq!(
            agents[0].last_active_at.as_deref(),
            Some("2026-05-09T01:00:00.123456Z")
        );
        assert_eq!(inbox[0].unread_count, 2);
        assert_eq!(threads[0].thread_id, "eidetic_engine_cli-abwd");
        let subject = require_some(threads[0].subject.as_ref(), "subject");
        assert!(!subject.contains("ghp_"));
        let json = require_ok(serde_json::to_string(&snapshot), "serialize");
        assert!(!json.contains("SECRET_TOKEN"));
        assert!(!json.contains("body_md"));
        assert!(!json.contains("raw body"));
    }

    fn declared_agent_mail_snapshot_v1_example() -> Value {
        let schema: Value = serde_json::from_str(include_str!(
            "../../docs/schemas/swarm/ee.agent_mail.snapshot.v1.json"
        ))
        .expect("Agent Mail snapshot schema remains valid JSON");
        schema
            .pointer("/examples/0")
            .cloned()
            .expect("Agent Mail snapshot schema keeps a declared-v1 example")
    }

    #[test]
    fn agent_mail_declared_v1_schema_example_passes_strict_parser() {
        let example = declared_agent_mail_snapshot_v1_example();
        let encoded = serde_json::to_string(&example).expect("example serializes");
        let snapshot = require_ok(
            parse_agent_mail_snapshot_json(&encoded),
            "strict declared-v1 Agent Mail snapshot",
        );

        assert_eq!(snapshot.agent_name.as_deref(), Some("BeigeHollow"));
        assert_eq!(snapshot.agents.len(), 1);
        assert_eq!(snapshot.file_reservations.len(), 1);
        assert_eq!(snapshot.inbox.len(), 1);
        assert_eq!(snapshot.threads.len(), 1);
        assert!(snapshot.degraded.is_empty());
    }

    #[test]
    fn agent_mail_workspace_identity_normalization_is_platform_stable() {
        assert_eq!(
            normalize_agent_mail_workspace_identity("/Users/Example/repo", false),
            "/Users/Example/repo"
        );
        assert_eq!(
            normalize_agent_mail_workspace_identity(r"\\?\C:\Users\Example\repo", true),
            "c:/Users/Example/repo"
        );
        assert_eq!(
            normalize_agent_mail_workspace_identity(r"\\?\UNC\server\share\repo", true),
            "//server/share/repo"
        );
    }

    #[test]
    fn agent_mail_declared_v1_accepts_independent_ack_and_unread_counts() {
        let mut example = declared_agent_mail_snapshot_v1_example();
        example["inbox"][0]["unread_count"] = json!(0);
        example["inbox"][0]["ack_required_count"] = json!(1);
        let encoded = serde_json::to_string(&example).expect("count example serializes");

        let snapshot = require_ok(
            parse_agent_mail_snapshot_json(&encoded),
            "independent Agent Mail status counts",
        );

        assert_eq!(snapshot.inbox[0].unread_count, 0);
        assert_eq!(snapshot.inbox[0].ack_required_count, 1);
    }

    #[test]
    fn agent_mail_declared_v1_health_posture_controls_fallback_consistently() {
        let mut degraded = declared_agent_mail_snapshot_v1_example();
        degraded["health_level"] = json!("yellow");
        degraded["fallback_active"] = json!(true);
        degraded["producer_status"] = json!("degraded");
        let encoded = serde_json::to_string(&degraded).expect("degraded example serializes");
        let parsed = require_ok(
            parse_agent_mail_snapshot_json(&encoded),
            "health-degraded Agent Mail snapshot",
        );
        assert!(
            parsed
                .degraded
                .iter()
                .any(|item| item.code == AGENT_MAIL_UNAVAILABLE_CODE)
        );

        let mut contradictory = declared_agent_mail_snapshot_v1_example();
        contradictory["health_level"] = json!("red");
        let encoded =
            serde_json::to_string(&contradictory).expect("contradictory example serializes");
        let error = parse_agent_mail_snapshot_json(&encoded)
            .expect_err("red health with fallback=false must fail closed");
        assert!(error.contains("producer/fallback posture contradicts"));
    }

    #[test]
    fn agent_mail_declared_v1_rejects_incomplete_or_contradictory_evidence() {
        let mut cases = Vec::new();

        let mut missing_status = declared_agent_mail_snapshot_v1_example();
        missing_status["command_statuses"]
            .as_array_mut()
            .expect("command statuses array")
            .pop();
        cases.push(("missing_command_status", missing_status));

        let mut mismatched_command = declared_agent_mail_snapshot_v1_example();
        mismatched_command["command_statuses"][0]["command"] =
            json!("am agents list --project <different-workspace> --json");
        cases.push(("mismatched_command_status", mismatched_command));

        let mut wrong_source_identity = declared_agent_mail_snapshot_v1_example();
        wrong_source_identity["source_commands"][0] =
            json!("am status --project '<workspace>' --agent BeigeHollow --json");
        let wrong_command = wrong_source_identity["source_commands"][0].clone();
        wrong_source_identity["command_statuses"][0]["command"] = wrong_command;
        cases.push(("wrong_source_identity", wrong_source_identity));

        let mut false_success = declared_agent_mail_snapshot_v1_example();
        false_success["command_statuses"][3]["ok"] = json!(false);
        false_success["command_statuses"][3]["error_class"] = json!("invalid_response");
        cases.push(("failed_status_without_degradation", false_success));

        let mut impossible_success_exit = declared_agent_mail_snapshot_v1_example();
        impossible_success_exit["command_statuses"][0]["exit_code"] = json!(1);
        cases.push(("impossible_success_exit", impossible_success_exit));

        let mut failure_without_metadata = declared_agent_mail_snapshot_v1_example();
        failure_without_metadata["command_statuses"][0]["ok"] = json!(false);
        cases.push(("failure_without_metadata", failure_without_metadata));

        let mut failed_readiness_with_output = declared_agent_mail_snapshot_v1_example();
        failed_readiness_with_output["command_statuses"][4]["ok"] = json!(false);
        cases.push(("failed_readiness_with_output", failed_readiness_with_output));

        let mut missing_health_level = declared_agent_mail_snapshot_v1_example();
        missing_health_level
            .as_object_mut()
            .expect("snapshot object")
            .remove("health_level");
        cases.push(("successful_readiness_without_health", missing_health_level));

        let mut missing_durability_state = declared_agent_mail_snapshot_v1_example();
        missing_durability_state
            .as_object_mut()
            .expect("snapshot object")
            .remove("durability_state");
        cases.push((
            "successful_durability_without_state",
            missing_durability_state,
        ));

        let mut explicit_null_timestamp = declared_agent_mail_snapshot_v1_example();
        explicit_null_timestamp["agents"][0]["last_active_ts"] = Value::Null;
        cases.push(("explicit_null_timestamp", explicit_null_timestamp));

        let mut pass_with_failure_reason = declared_agent_mail_snapshot_v1_example();
        pass_with_failure_reason["semantic_readiness"]["reason"] = json!("unknown");
        cases.push(("pass_with_failure_reason", pass_with_failure_reason));

        let mut wrong_summary = declared_agent_mail_snapshot_v1_example();
        wrong_summary["summary"]["source_command_count"] = json!(5);
        cases.push(("wrong_source_count", wrong_summary));

        let mut duplicate_agent = declared_agent_mail_snapshot_v1_example();
        let duplicate = duplicate_agent["agents"][0].clone();
        duplicate_agent["agents"]
            .as_array_mut()
            .expect("agents array")
            .push(duplicate);
        duplicate_agent["summary"]["agent_count"] = json!(2);
        cases.push(("duplicate_agent_identity", duplicate_agent));

        let mut mismatched_mailbox = declared_agent_mail_snapshot_v1_example();
        mismatched_mailbox["inbox"][0]["mailbox"] = json!("OtherAgent");
        cases.push(("status_mailbox_mismatch", mismatched_mailbox));

        let mut mismatched_command_agent = declared_agent_mail_snapshot_v1_example();
        mismatched_command_agent["source_commands"][2] =
            json!("am mail inbox --project '<workspace>' --agent OtherAgent --limit 20 --json");
        mismatched_command_agent["command_statuses"][2]["command"] =
            mismatched_command_agent["source_commands"][2].clone();
        cases.push(("source_command_agent_mismatch", mismatched_command_agent));

        let mut mismatched_command_binary = declared_agent_mail_snapshot_v1_example();
        mismatched_command_binary["source_commands"][3] =
            json!("other-am status --project '<workspace>' --agent BeigeHollow --json");
        mismatched_command_binary["command_statuses"][3]["command"] =
            mismatched_command_binary["source_commands"][3].clone();
        cases.push(("source_command_binary_mismatch", mismatched_command_binary));

        let mut non_schema_agent = declared_agent_mail_snapshot_v1_example();
        non_schema_agent["agents"][0]["lastActiveAt"] =
            non_schema_agent["agents"][0]["last_active_ts"].clone();
        cases.push(("non_schema_agent_alias", non_schema_agent));

        for (name, value) in cases {
            let encoded = serde_json::to_string(&value).expect("malformed case serializes");
            let error = parse_agent_mail_snapshot_json(&encoded)
                .expect_err("declared-v1 incomplete evidence must fail closed");
            assert!(
                error.contains(AGENT_MAIL_SNAPSHOT_SCHEMA_V1),
                "{name}: error must identify the strict declared-v1 boundary: {error}"
            );
        }
    }

    #[test]
    fn agent_mail_declared_v1_freshness_boundary_and_future_skew_fail_closed() {
        let now = DateTime::parse_from_rfc3339("2030-01-08T00:00:00Z")
            .expect("fixed now parses")
            .with_timezone(&Utc);
        let cases = [
            (
                "2030-01-07T23:55:00Z",
                "current",
                AGENT_MAIL_SNAPSHOT_STALE_AFTER_SECONDS,
            ),
            (
                "2030-01-07T23:54:59Z",
                "stale",
                AGENT_MAIL_SNAPSHOT_STALE_AFTER_SECONDS + 1,
            ),
        ];
        for (generated_at, expected_state, expected_age) in cases {
            let mut value = declared_agent_mail_snapshot_v1_example();
            value["generated_at"] = json!(generated_at);
            let encoded = serde_json::to_string(&value).expect("freshness case serializes");
            let (freshness, degradation) = require_ok(
                agent_mail_snapshot_freshness_assessment(&encoded, now),
                "Agent Mail freshness assessment",
            );
            assert_eq!(freshness.state, expected_state);
            assert_eq!(freshness.age_seconds, Some(expected_age));
            assert_eq!(degradation.is_some(), expected_state == "stale");
        }

        let mut future = declared_agent_mail_snapshot_v1_example();
        future["generated_at"] = json!("2030-01-08T00:01:01Z");
        let encoded = serde_json::to_string(&future).expect("future case serializes");
        let error = agent_mail_snapshot_freshness_assessment(&encoded, now)
            .expect_err("future snapshot beyond skew budget must fail closed");
        assert!(error.contains("61 seconds in the future"));
    }

    #[test]
    fn agent_mail_snapshot_adapter_uses_generated_at_not_file_mtime() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let project_key = agent_mail_snapshot_project_key_for_workspace(tempdir.path())?;
        let cases = [
            ("fresh", Utc::now().to_rfc3339(), "current", false),
            ("stale", "2000-01-01T00:00:00Z".to_owned(), "stale", true),
        ];

        for (name, generated_at, expected_freshness, expected_degraded) in cases {
            let mut value = declared_agent_mail_snapshot_v1_example();
            value["generated_at"] = json!(generated_at);
            value["project_key"] = json!(&project_key);
            let path = tempdir.path().join(format!("{name}-agent-mail.json"));
            fs::write(
                &path,
                serde_json::to_vec(&value).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            let mut options = SwarmBriefCollectOptions::for_workspace(tempdir.path());
            options.agent_mail_snapshot_path = Some(path);

            let output = AgentMailSnapshotFileAdapter.collect(&options);

            assert_eq!(output.snapshot.freshness.state, expected_freshness);
            assert_eq!(
                output.snapshot.status == SwarmBriefSourceStatus::Degraded,
                expected_degraded
            );
            assert_eq!(
                output
                    .snapshot
                    .degraded
                    .iter()
                    .any(|item| item.code == AGENT_MAIL_UNAVAILABLE_CODE),
                expected_degraded
            );
            match &output.contribution {
                SwarmBriefContribution::AgentMail { agent_name, .. } => {
                    assert_eq!(
                        agent_name.as_deref(),
                        (expected_freshness == "current").then_some("BeigeHollow")
                    );
                }
                other => panic!("expected Agent Mail contribution, got {other:?}"),
            }
        }
        Ok(())
    }

    #[test]
    fn agent_mail_identity_survives_authoritative_contribution_and_report_projection()
    -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let project_key = agent_mail_snapshot_project_key_for_workspace(tempdir.path())?;
        let mut value = declared_agent_mail_snapshot_v1_example();
        value["generated_at"] = json!(Utc::now().to_rfc3339());
        value["project_key"] = json!(project_key);
        let path = tempdir.path().join("identity-agent-mail.json");
        fs::write(
            &path,
            serde_json::to_vec(&value).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let mut options = SwarmBriefCollectOptions::for_workspace(tempdir.path());
        options.agent_mail_snapshot_path = Some(path);

        let output = AgentMailSnapshotFileAdapter.collect(&options);

        assert_eq!(output.snapshot.status, SwarmBriefSourceStatus::Ready);
        assert_eq!(output.snapshot.freshness.state, "current");
        match &output.contribution {
            SwarmBriefContribution::AgentMail { agent_name, .. } => {
                assert_eq!(agent_name.as_deref(), Some("BeigeHollow"));
            }
            other => return Err(format!("expected Agent Mail contribution, got {other:?}")),
        }

        let mut report = SwarmBriefReport::empty(tempdir.path());
        apply_source_output(&mut report, output);
        assert_eq!(report.agent_mail_agent_name.as_deref(), Some("BeigeHollow"));
        let serialized = serde_json::to_value(&report).map_err(|error| error.to_string())?;
        assert!(
            serialized.get("agentMailAgentName").is_none(),
            "authoritative self identity must stay internal to the brief"
        );
        Ok(())
    }

    #[test]
    fn agent_mail_snapshot_adapter_drops_expired_reservations_before_surface_projection()
    -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let project_key = agent_mail_snapshot_project_key_for_workspace(tempdir.path())?;
        let mut value = declared_agent_mail_snapshot_v1_example();
        value["generated_at"] = json!(Utc::now().to_rfc3339());
        value["project_key"] = json!(project_key);
        value["file_reservations"][0]["expires_ts"] = json!("2000-01-01T00:00:00Z");
        let path = tempdir.path().join("expired-reservation-agent-mail.json");
        fs::write(
            &path,
            serde_json::to_vec(&value).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let mut options = SwarmBriefCollectOptions::for_workspace(tempdir.path());
        options.agent_mail_snapshot_path = Some(path);

        let output = AgentMailSnapshotFileAdapter.collect(&options);

        assert_eq!(output.snapshot.status, SwarmBriefSourceStatus::Ready);
        assert_eq!(output.snapshot.item_count, 3);
        match output.contribution {
            SwarmBriefContribution::AgentMail {
                file_reservations, ..
            } => assert!(
                file_reservations.is_empty(),
                "expired reservations must not become active surface risk"
            ),
            other => return Err(format!("expected Agent Mail contribution, got {other:?}")),
        }
        Ok(())
    }

    #[test]
    fn agent_mail_snapshot_adapter_rejects_other_workspace_binding() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let mut value = declared_agent_mail_snapshot_v1_example();
        value["generated_at"] = json!(Utc::now().to_rfc3339());
        let path = tempdir.path().join("other-workspace-agent-mail.json");
        fs::write(
            &path,
            serde_json::to_vec(&value).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let mut options = SwarmBriefCollectOptions::for_workspace(tempdir.path());
        options.agent_mail_snapshot_path = Some(path);

        let output = AgentMailSnapshotFileAdapter.collect(&options);

        assert_eq!(output.snapshot.status, SwarmBriefSourceStatus::Unavailable);
        assert!(output.snapshot.degraded.iter().any(|item| {
            item.code == AGENT_MAIL_UNAVAILABLE_CODE
                && item
                    .message
                    .contains("does not match the requested workspace")
        }));
        assert!(matches!(output.contribution, SwarmBriefContribution::None));
        Ok(())
    }

    #[test]
    fn agent_mail_missing_snapshot_mentions_reachable_health_bridge() {
        let (message, repair) =
            agent_mail_missing_snapshot_degradation_text(AgentMailHealthProbe::Reachable);
        assert!(message.contains("health endpoint"));
        assert!(message.contains("reachable"));
        assert!(message.contains("redacted snapshots"));
        assert!(repair.contains("read-only redacted Agent Mail snapshot"));
        assert!(!repair.contains("scripts/swarm_coordination_health.sh"));
        assert!(repair.contains("--agent-mail-snapshot"));
    }

    #[test]
    fn agent_mail_missing_snapshot_mentions_unreachable_health_bridge() {
        let (message, repair) =
            agent_mail_missing_snapshot_degradation_text(AgentMailHealthProbe::Unreachable);
        assert!(message.contains("not reachable"));
        assert!(message.contains("127.0.0.1:8765"));
        assert!(repair.contains("Start or repair Agent Mail"));
        assert!(repair.contains("read-only redacted Agent Mail snapshot"));
        assert!(!repair.contains("scripts/swarm_coordination_health.sh"));
        assert!(repair.contains("--agent-mail-snapshot"));
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
    fn agent_mail_health_snapshot_degrades_semantic_readiness_failure_without_raw_storage_leaks() {
        let snapshot = require_ok(
            parse_agent_mail_snapshot_json(
                r#"{
              "schema":"ee.swarm.coordination_health.v1",
              "healthLevel":"green",
              "semantic_readiness":{
                "status":"fail",
                "reason":"database disk image is malformed at page 283 in /Users/example/.local/share/mcp_agent_mail/mail.db"
              }
            }"#,
            ),
            "valid Agent Mail semantic-readiness health JSON",
        );

        assert_eq!(snapshot.degraded.len(), 1);
        let degradation = &snapshot.degraded[0];
        assert_eq!(degradation.code, AGENT_MAIL_SEMANTIC_READINESS_FAILED_CODE);
        assert_eq!(degradation.source, SwarmBriefSourceKind::AgentMail);
        assert!(degradation.message.contains("healthLevel=green"));
        assert!(degradation.message.contains("malformed_sqlite"));
        assert!(!degradation.message.contains("/Users/"));
        assert!(!degradation.message.contains("page 283"));
        assert!(!degradation.message.contains("mail.db"));
    }

    #[test]
    fn agent_mail_health_snapshot_degrades_recovery_corrupt_without_raw_storage_leaks() {
        let snapshot = require_ok(
            parse_agent_mail_snapshot_json(
                r#"{
              "schema":"ee.swarm.coordination_health.v1",
              "health_level":"green",
              "semantic_readiness":{
                "status":"ok"
              },
              "recovery":{
                "mode":"corrupt",
                "next_action":"Run am doctor repair --yes or restore from /Users/example/.local/share/mcp_agent_mail/storage.sqlite3 after B-tree page 283 failed",
                "bundle_path":"/Users/example/.local/share/mcp_agent_mail/doctor/forensics/storage.sqlite3/reconstruct-20260602_030410_115"
              }
            }"#,
            ),
            "valid Agent Mail recovery-corrupt health JSON",
        );

        assert_eq!(snapshot.degraded.len(), 1);
        let degradation = &snapshot.degraded[0];
        assert_eq!(degradation.code, AGENT_MAIL_UNAVAILABLE_CODE);
        assert_eq!(degradation.source, SwarmBriefSourceKind::AgentMail);
        assert!(degradation.message.contains("healthLevel=green"));
        assert!(degradation.message.contains("mode=corrupt"));
        assert!(degradation.message.contains("archive_corruption"));
        assert!(!degradation.message.contains("/Users/"));
        assert!(!degradation.message.contains("storage.sqlite3"));
        assert!(!degradation.message.contains("B-tree"));
        assert!(!degradation.message.contains("page 283"));
        assert!(
            !degradation
                .message
                .contains("reconstruct-20260602_030410_115")
        );
    }

    #[test]
    fn agent_mail_health_snapshot_preserves_explicit_repair_required_mode() {
        let snapshot = require_ok(
            parse_agent_mail_snapshot_json(
                r#"{
              "schema":"ee.swarm.coordination_health.v1",
              "health_level":"yellow",
              "semantic_readiness":{
                "status":"ok"
              },
              "recovery":{
                "mode":"repair_required"
              }
            }"#,
            ),
            "valid Agent Mail repair-required health JSON",
        );

        assert_eq!(snapshot.degraded.len(), 1);
        let degradation = &snapshot.degraded[0];
        assert_eq!(degradation.code, AGENT_MAIL_UNAVAILABLE_CODE);
        assert_eq!(degradation.source, SwarmBriefSourceKind::AgentMail);
        assert!(degradation.message.contains("healthLevel=yellow"));
        assert!(degradation.message.contains("mode=repair_required"));
        assert!(!degradation.message.contains("mode=unknown_recovery"));
    }

    #[test]
    fn agent_mail_health_snapshot_degrades_durability_corrupt_without_semantic_failure() {
        let snapshot = require_ok(
            parse_agent_mail_snapshot_json(
                r#"{
              "schema":"ee.swarm.coordination_health.v1",
              "status":"degraded",
              "durability_state":"corrupt",
              "database_path":"storage.sqlite3",
              "detail":"open /Users/example/.local/share/mcp_agent_mail/storage.sqlite3 failed at B-tree page 283"
            }"#,
            ),
            "valid Agent Mail durability-corrupt health JSON",
        );

        assert_eq!(snapshot.degraded.len(), 1);
        let degradation = &snapshot.degraded[0];
        assert_eq!(degradation.code, AGENT_MAIL_UNAVAILABLE_CODE);
        assert_eq!(degradation.source, SwarmBriefSourceKind::AgentMail);
        assert!(degradation.message.contains("mode=corrupt"));
        assert!(degradation.message.contains("archive_corruption"));
        assert!(!degradation.message.contains("/Users/"));
        assert!(!degradation.message.contains("storage.sqlite3"));
        assert!(!degradation.message.contains("B-tree"));
        assert!(!degradation.message.contains("page 283"));
    }

    #[cfg(unix)]
    #[test]
    fn agent_mail_snapshot_adapter_refuses_symlinked_snapshot_file() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside_snapshot = outside.path().join("agent-mail.json");
        fs::write(
            &outside_snapshot,
            r#"{"file_reservations":[],"inbox":[],"threads":[]}"#,
        )
        .map_err(|error| error.to_string())?;
        let snapshot_path = tempdir.path().join("agent-mail.json");
        std::os::unix::fs::symlink(&outside_snapshot, &snapshot_path)
            .map_err(|error| error.to_string())?;

        let mut options = SwarmBriefCollectOptions::for_workspace(tempdir.path());
        options.agent_mail_snapshot_path = Some(snapshot_path);
        let output = AgentMailSnapshotFileAdapter.collect(&options);

        assert_eq!(output.snapshot.status, SwarmBriefSourceStatus::Unavailable);
        assert!(
            output
                .snapshot
                .degraded
                .iter()
                .any(|item| item.code == AGENT_MAIL_UNAVAILABLE_CODE
                    && item.message.contains("symlink")),
            "expected symlink degradation, got {:?}",
            output.snapshot.degraded
        );
        assert!(matches!(output.contribution, SwarmBriefContribution::None));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn swarm_brief_agent_mail_snapshot_final_open_rejects_symlink_leaf() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside_snapshot = outside.path().join("agent-mail.json");
        fs::write(
            &outside_snapshot,
            r#"{"file_reservations":[],"inbox":[],"threads":[]}"#,
        )
        .map_err(|error| error.to_string())?;
        let snapshot_path = tempdir.path().join("agent-mail.json");
        std::os::unix::fs::symlink(&outside_snapshot, &snapshot_path)
            .map_err(|error| error.to_string())?;

        let error = match open_agent_mail_snapshot_file_for_read_no_follow(&snapshot_path) {
            Ok(_) => return Err("symlinked Agent Mail snapshot unexpectedly opened".to_string()),
            Err(error) => error,
        };

        assert_ne!(
            error.kind(),
            io::ErrorKind::NotFound,
            "O_NOFOLLOW rejection should not be masked as a missing snapshot"
        );
        let outside_contents =
            fs::read_to_string(&outside_snapshot).map_err(|error| error.to_string())?;
        assert!(outside_contents.contains("file_reservations"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn agent_mail_snapshot_adapter_refuses_symlinked_snapshot_parent() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside = tempfile::tempdir().map_err(|error| error.to_string())?;
        fs::write(
            outside.path().join("agent-mail.json"),
            r#"{"file_reservations":[],"inbox":[],"threads":[]}"#,
        )
        .map_err(|error| error.to_string())?;
        let snapshot_parent = tempdir.path().join("mail-snapshot");
        std::os::unix::fs::symlink(outside.path(), &snapshot_parent)
            .map_err(|error| error.to_string())?;

        let mut options = SwarmBriefCollectOptions::for_workspace(tempdir.path());
        options.agent_mail_snapshot_path = Some(snapshot_parent.join("agent-mail.json"));
        let output = AgentMailSnapshotFileAdapter.collect(&options);

        assert_eq!(output.snapshot.status, SwarmBriefSourceStatus::Unavailable);
        assert!(
            output
                .snapshot
                .degraded
                .iter()
                .any(|item| item.code == AGENT_MAIL_UNAVAILABLE_CODE
                    && item.message.contains("symlink")),
            "expected symlink parent degradation, got {:?}",
            output.snapshot.degraded
        );
        assert!(matches!(output.contribution, SwarmBriefContribution::None));
        Ok(())
    }

    #[test]
    fn agent_mail_snapshot_adapter_refuses_oversized_snapshot_file() -> Result<(), String> {
        // bd-1sdr5: regression — an operator-supplied snapshot path
        // larger than AGENT_MAIL_SNAPSHOT_MAX_BYTES (8 MiB) must
        // degrade rather than allocate the whole file. Mirrors the
        // symlink-refusal posture above.
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let snapshot_path = tempdir.path().join("agent-mail.json");

        // Write AGENT_MAIL_SNAPSHOT_MAX_BYTES + 1 bytes so the cap
        // trips with a deterministic single-byte overrun. Content
        // doesn't matter — the cap fires before any parse path.
        let mut oversized = vec![b'x'; AGENT_MAIL_SNAPSHOT_MAX_BYTES + 1];
        oversized[0] = b'{';
        let last_byte = oversized.len() - 1;
        oversized[last_byte] = b'}';
        fs::write(&snapshot_path, &oversized).map_err(|error| error.to_string())?;

        let mut options = SwarmBriefCollectOptions::for_workspace(tempdir.path());
        options.agent_mail_snapshot_path = Some(snapshot_path);
        let output = AgentMailSnapshotFileAdapter.collect(&options);

        assert_eq!(output.snapshot.status, SwarmBriefSourceStatus::Unavailable);
        assert!(
            output
                .snapshot
                .degraded
                .iter()
                .any(|item| item.code == AGENT_MAIL_UNAVAILABLE_CODE
                    && item.message.contains("exceeds")
                    && item.message.contains("byte cap")),
            "expected byte-cap degradation, got {:?}",
            output.snapshot.degraded
        );
        assert!(matches!(output.contribution, SwarmBriefContribution::None));
        Ok(())
    }

    #[test]
    fn beads_ready_collection_is_unbounded_sorted_and_deduplicated() {
        let options = SwarmBriefCollectOptions::for_workspace(".");
        let mut ready_rows = (0..25)
            .rev()
            .map(|index| {
                json!({
                    "id": format!("bd-ready-{index:02}"),
                    "title": format!("Ready work {index:02}"),
                    "status": "open"
                })
            })
            .collect::<Vec<_>>();
        ready_rows.push(ready_rows[0].clone());
        let ready_json = serde_json::to_string(&ready_rows).expect("ready rows serialize");
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
            .with_output("br", &BEADS_READY_ARGS, &ready_json)
            .with_output("br", &["blocked", "--json"], "[]")
            .with_output("br", &["list", "--status", "in_progress", "--json"], "[]")
            .with_output("br", &["list", "--status", "deferred", "--json"], "[]")
            .with_output(
                "br",
                &["dep", "cycles", "--json"],
                r#"{"cycles":[],"count":0}"#,
            );

        let output = BeadsSourceAdapter { runner: &runner }.collect(&options);

        assert_eq!(output.snapshot.item_count, 25);
        assert_eq!(
            output.snapshot.provenance.command.as_deref(),
            Some("br ready --limit 0 --json --no-auto-import --no-auto-flush --allow-stale")
        );
        match output.contribution {
            SwarmBriefContribution::Beads(summary) => {
                let ids = summary
                    .ready
                    .iter()
                    .map(|bead| bead.id.clone())
                    .collect::<Vec<_>>();
                let expected = (0..25)
                    .map(|index| format!("bd-ready-{index:02}"))
                    .collect::<Vec<_>>();
                assert_eq!(ids, expected);
            }
            other => panic!("expected Beads contribution, got {other:?}"),
        }
        let calls = runner.calls();
        assert!(calls.contains(
            &"br ready --limit 0 --json --no-auto-import --no-auto-flush --allow-stale".to_owned()
        ));
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("br ready"))
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec![BEADS_READY_COMMAND]
        );
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
                &BEADS_READY_ARGS,
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
    fn beads_sync_status_metadata_only_jsonl_newer_keeps_source_ready() {
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
                r#"{"dirty_count":0,"jsonl_newer":true,"db_newer":false,"last_import_time":"2026-06-04T19:42:30+00:00"}"#,
            )
            .with_output(
                "br",
                &["doctor", "--json", "--no-db"],
                r#"{
  "ok": true,
  "checks": [
    {"name":"jsonl.merge_artifacts","status":"ok","details":{"files":[]}},
    {"name":"jsonl.parse","status":"ok","message":"Parsed 3347 records","details":{"records":3347}},
    {"name":"counts.db_vs_jsonl","status":"ok","message":"Both have 3347 records","details":{"db":3347,"jsonl":3347}},
    {"name":"sync.metadata","status":"ok","message":"External changes pending import","details":{"dirty_issues":0,"last_import":"2026-06-04T19:42:30+00:00","last_export":"2026-06-04T19:42:30+00:00","jsonl_hash":"e49435f610df6319"}}
  ]
}"#,
            )
            .with_output(
                "br",
                &BEADS_READY_ARGS,
                r#"[{"id":"bd-ready","title":"Ready work","status":"open"}]"#,
            )
            .with_output("br", &["blocked", "--json"], "[]")
            .with_output("br", &["list", "--status", "in_progress", "--json"], "[]")
            .with_output("br", &["list", "--status", "deferred", "--json"], "[]")
            .with_output("br", &["dep", "cycles", "--json"], r#"{"cycles":[],"count":0}"#);

        let output = BeadsSourceAdapter { runner: &runner }.collect(&options);

        assert_eq!(output.snapshot.status, SwarmBriefSourceStatus::Degraded);
        assert_eq!(output.snapshot.freshness.state, "current");
        assert!(
            output
                .snapshot
                .degraded
                .iter()
                .all(|item| item.code != BEADS_TRACKER_STALE_CODE),
            "metadata-only drift must not emit beads_tracker_stale: {:?}",
            output.snapshot.degraded
        );
        let metadata_drift = output
            .snapshot
            .degraded
            .iter()
            .find(|item| item.code == BEADS_TRACKER_METADATA_DRIFT_CODE)
            .expect("metadata-only drift should emit a warning diagnostic");
        assert_eq!(metadata_drift.severity, "warning");
        assert!(metadata_drift.message.contains("br reads are advisory"));
        assert_eq!(
            metadata_drift.repair.as_deref(),
            Some("br sync --import-only --json")
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
                &BEADS_READY_ARGS,
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
                &BEADS_READY_ARGS,
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
                &BEADS_READY_ARGS,
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
                &BEADS_READY_ARGS,
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
            parse_rch_status_json(r#"{"queueDepth":" 5 ","activeBuilds":"2"}"#),
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
            parse_rch_status_json(r#"{"workersHealthy":"0"}"#),
            "no workers rch JSON",
        );
        assert!(no_workers.iter().any(|hint| {
            hint.message == "rch remote posture: no_remote_workers" && hint.level == "high"
        }));

        let ready = require_ok(
            parse_rch_status_json(r#"{"workers":[{"id":"css","status":" OK "}]}"#),
            "ready workers rch JSON",
        );
        assert!(ready.iter().any(|hint| {
            hint.message == "rch remote posture: remote_ready" && hint.level == "low"
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
    fn rch_worker_pressure_blocks_all_healthy_critical_workers() {
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
                                    "daemon":{"version":"1.0.17","socket_path":"/tmp/rch.sock","workers_healthy":2},
                                    "workers":[
                                        {
                                            "id":"vmi-a",
                                            "status":"healthy",
                                            "diskPressure":"critical",
                                            "admissionImpact":"blocked",
                                            "reasonCode":"disk_pressure_critical",
                                            "freeGb":1,
                                            "freeRatio":0.03,
                                            "telemetryFreshness":"current"
                                        },
                                        {
                                            "id":"vmi-b",
                                            "status":"healthy",
                                            "diskPressure":"critical",
                                            "admissionImpact":"blocked",
                                            "reasonCode":"disk_pressure_critical",
                                            "freeGb":0,
                                            "freeRatio":0.01,
                                            "telemetryFreshness":"current"
                                        }
                                    ]
                                }
                            }
                        },
                        "config":{"data":{"general":{"socket_path":"/tmp/rch.sock"}}},
                        "workerProbe":{"data":{"summary":{"healthy":2,"failed":0}}},
                        "diagnose":{"data":{"dry_run":{"would_offload":true}}}
                    }
                }"#,
            ),
            "all workers pressure-blocked fixture",
        );

        assert_eq!(report.worker_pressure.schema, RCH_WORKER_PRESSURE_SCHEMA_V1);
        assert_eq!(
            report.worker_pressure.status,
            "healthy_but_pressure_blocked"
        );
        assert_eq!(report.worker_pressure.worker_count, 2);
        assert_eq!(report.worker_pressure.usable_worker_count, 0);
        assert_eq!(report.worker_pressure.blocked_worker_count, 2);
        assert_eq!(report.worker_pressure.workers[0].free_ratio_bps, Some(300));
        assert!(!report.remote_only_safe);
        assert!(report.degraded.iter().any(|degradation| {
            degradation.code == RCH_REMOTE_REQUIRED_FALLBACK_PREVENTED_CODE
                && degradation.message.contains("worker pressure posture")
        }));
        assert!(
            report.recovery.contains(
                &"reuse_rch_known_blocker_or_wait_for_worker_pressure_recovery".to_string()
            )
        );

        let hints = require_ok(
            parse_rch_status_json(
                r#"{"data":{"daemon":{"workers":[{"id":"vmi-a","status":"healthy","diskPressure":"critical","admissionImpact":"blocked","reasonCode":"disk_pressure_critical"}]}}}"#,
            ),
            "pressure status hints",
        );
        assert!(hints.iter().any(|hint| {
            hint.level == "high"
                && hint.message == "rch worker pressure posture: healthy_but_pressure_blocked"
        }));
    }

    #[test]
    fn rch_worker_pressure_keeps_one_usable_worker_available() {
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
                                    "daemon":{"version":"1.0.17","socket_path":"/tmp/rch.sock","workers_healthy":2},
                                    "workers":[
                                        {"id":"vmi-a","status":"healthy","diskPressure":"critical","admissionImpact":"blocked","freeGb":1},
                                        {"id":"vmi-b","status":"healthy","diskPressure":"clear","admissionImpact":"usable","freeGb":44,"freeRatio":0.42}
                                    ]
                                }
                            }
                        },
                        "config":{"data":{"general":{"socket_path":"/tmp/rch.sock"}}},
                        "workerProbe":{"data":{"summary":{"healthy":2,"failed":0}}},
                        "diagnose":{"data":{"dry_run":{"would_offload":true}}}
                    }
                }"#,
            ),
            "one usable worker fixture",
        );

        assert_eq!(report.worker_pressure.status, "pressure_degraded");
        assert_eq!(report.worker_pressure.usable_worker_count, 1);
        assert_eq!(report.worker_pressure.blocked_worker_count, 1);
        assert!(report.remote_only_safe);
        assert!(report.degraded.is_empty());
    }

    #[test]
    fn rch_worker_pressure_distinguishes_stale_and_missing_telemetry() {
        let stale = rch_worker_pressure_report(
            &serde_json::json!({
                "data":{
                    "daemon":{
                        "workers":[
                            {"id":"vmi-a","status":"healthy","telemetryFreshness":"stale","freeGb":30},
                            {"id":"vmi-b","status":"healthy","telemetryFreshness":"stale","freeRatio":0.30}
                        ]
                    }
                }
            }),
            None,
        );
        assert_eq!(stale.status, "telemetry_stale");
        assert_eq!(stale.stale_worker_count, 2);
        assert_eq!(stale.blocked_worker_count, 0);

        let missing = rch_worker_pressure_report(
            &serde_json::json!({
                "data":{
                    "daemon":{
                        "workers":[
                            {"id":"vmi-a","status":"healthy"},
                            {"id":"vmi-b","status":"healthy"}
                        ]
                    }
                }
            }),
            None,
        );
        assert_eq!(missing.status, "pressure_unknown");
        assert_eq!(missing.worker_count, 2);
        assert_eq!(missing.unknown_worker_count, 2);
        assert!(missing.workers.iter().all(|worker| {
            worker.reason_code == "no_pressure_telemetry" && worker.admission_impact == "unknown"
        }));

        let policy_denied = rch_worker_pressure_report(
            &serde_json::json!({
                "data":{
                    "daemon":{
                        "workers":[
                            {"id":"vmi-a","status":"healthy","admissionImpact":"policy_denied"}
                        ]
                    }
                }
            }),
            None,
        );
        assert_eq!(policy_denied.status, "pressure_policy_denied");
        assert_eq!(policy_denied.blocked_worker_count, 1);
        assert_eq!(
            policy_denied.workers[0].reason_code,
            "pressure_policy_denied"
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
        assert_eq!(queue.queue_head_slots_needed, Some(8));
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
                                        "command":"env TMPDIR=/tmp cargo build --bin ee",
                                        "detector_build_age_secs":79200
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
        assert_eq!(queue.queue_head_slots_needed, Some(4));
        assert_eq!(queue.active_build_max_age_seconds, Some(79_200));
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
            stdout: String::new(),
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
            stdout: String::new(),
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
    fn advisor_blocks_raw_bv_claim_guidance_on_liveness_degradation() {
        for (code, message) in [
            (
                BV_COMMAND_TIMEOUT_CODE,
                "BV robot source command timed out after 1500 ms.",
            ),
            (
                BV_NO_OUTPUT_CODE,
                "BV robot source command returned no output.",
            ),
        ] {
            let mut report = report_with_ready_sources();
            let Some(bv_snapshot) = report
                .sources
                .iter_mut()
                .find(|snapshot| snapshot.source == SwarmBriefSourceKind::Bv)
            else {
                panic!("bv source");
            };
            bv_snapshot.status = SwarmBriefSourceStatus::Unavailable;
            bv_snapshot.degraded = vec![SwarmBriefDegradation::warning(
                SwarmBriefSourceKind::Bv,
                code,
                message,
                Some(format!(
                    "Retry `bv --robot-triage --robot-triage-by-track` with the configured command timeout, or fall back to `{BEADS_READY_COMMAND}`."
                )),
            )];

            apply_swarm_brief_advice(&mut report);

            let rec = recommendation(&report, &format!("rec.degraded.bv.{code}"));
            assert!(rec.must_not_do.iter().any(|item| {
                item.contains(
                    "Do not wait on raw bv --robot-* commands without an explicit timeout",
                )
            }));
            assert!(
                rec.must_not_do
                    .iter()
                    .any(|item| item.contains("Do not use BV copy-paste claim guidance"))
            );
        }
    }

    #[test]
    fn command_error_maps_to_stable_degradation_without_raw_secret() {
        let error = SwarmBriefCommandError::Failed {
            status: Some(1),
            stdout: String::new(),
            stderr: "token=ghp_abcdefghijklmnopqrstuvwxyz123456".to_string(),
        };
        let degradation = error.to_degradation(
            SwarmBriefSourceKind::Beads,
            BEADS_UNAVAILABLE_CODE,
            BEADS_READY_COMMAND,
        );

        assert_eq!(degradation.code, BEADS_UNAVAILABLE_CODE);
        assert!(!degradation.message.contains("ghp_"));
        assert_eq!(degradation.repair.as_deref(), Some(BEADS_READY_COMMAND));
    }

    #[test]
    fn beads_timeout_uses_specific_source_health_code() {
        let error = SwarmBriefCommandError::TimedOut { timeout_ms: 1_500 };
        let degradation = beads_command_error_to_degradation(&error, BEADS_READY_COMMAND);

        assert_eq!(degradation.code, BEADS_COMMAND_TIMEOUT_CODE);
        assert_eq!(degradation.source, SwarmBriefSourceKind::Beads);
        assert!(degradation.message.contains("timed out after 1500 ms"));
        assert!(degradation.message.contains("advisory only"));
        assert_eq!(degradation.repair.as_deref(), Some(BEADS_READY_COMMAND));
    }

    #[test]
    fn beads_empty_stdout_uses_specific_source_health_code() {
        let options = SwarmBriefCollectOptions::for_workspace(".");
        let runner = FakeRunner::default().with_output(
            "br",
            &["list", "--status", "in_progress", "--json"],
            "",
        );
        let mut degraded = Vec::new();

        let beads = collect_beads_bucket(
            &runner,
            &options,
            &["list", "--status", "in_progress", "--json"],
            "in_progress",
            &mut degraded,
        );

        assert!(beads.is_empty());
        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, BEADS_NO_OUTPUT_CODE);
        assert_eq!(
            degraded[0].repair.as_deref(),
            Some("br list --status in_progress --json")
        );
        assert!(degraded[0].message.contains("no output"));
        assert!(degraded[0].message.contains("advisory only"));
    }

    #[test]
    fn bv_timeout_uses_specific_source_health_code() {
        let error = SwarmBriefCommandError::TimedOut { timeout_ms: 1_500 };
        let degradation =
            bv_command_error_to_degradation(&error, "bv --robot-triage --robot-triage-by-track");

        assert_eq!(degradation.code, BV_COMMAND_TIMEOUT_CODE);
        assert_eq!(degradation.source, SwarmBriefSourceKind::Bv);
        assert!(degradation.message.contains("timed out after 1500 ms"));
        assert!(degradation.message.contains("waiting indefinitely"));
        let repair = degradation.repair.as_deref().unwrap_or_default();
        assert!(repair.contains("bv --robot-triage --robot-triage-by-track"));
        assert!(repair.contains(BEADS_READY_COMMAND));
    }

    #[test]
    fn bv_empty_stdout_uses_specific_source_health_code() {
        let options = SwarmBriefCollectOptions::for_workspace(".");
        let runner = FakeRunner::default().with_output(
            "bv",
            &["--robot-triage", "--robot-triage-by-track"],
            "",
        );

        let output = BvSourceAdapter { runner: &runner }.collect(&options);

        assert_eq!(output.snapshot.source, SwarmBriefSourceKind::Bv);
        assert_eq!(output.snapshot.status, SwarmBriefSourceStatus::Unavailable);
        assert_eq!(output.snapshot.degraded.len(), 1);
        assert_eq!(output.snapshot.degraded[0].code, BV_NO_OUTPUT_CODE);
        assert!(output.snapshot.degraded[0].message.contains("no output"));
        let repair = output.snapshot.degraded[0]
            .repair
            .as_deref()
            .unwrap_or_default();
        assert!(repair.contains("bv --robot-triage --robot-triage-by-track"));
        assert!(repair.contains(BEADS_READY_COMMAND));
        match output.contribution {
            SwarmBriefContribution::None => {}
            _ => panic!("empty bv stdout must not contribute a healthy summary"),
        }
    }

    #[test]
    fn swarm_source_timeout_defaults_allow_live_bv_triage_budget() -> TestResult {
        let options = SwarmBriefCollectOptions::for_workspace(".");
        ensure_equal(
            &options.command_timeout_ms,
            &DEFAULT_SWARM_SOURCE_COMMAND_TIMEOUT_MS,
            "swarm brief command timeout default",
        )?;
        let git_options = WorkspaceGitSnapshotOptions::for_workspace(".");
        ensure_equal(
            &git_options.command_timeout_ms,
            &DEFAULT_SWARM_SOURCE_COMMAND_TIMEOUT_MS,
            "workspace git command timeout default",
        )
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
                &BEADS_READY_ARGS,
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
                .any(|degraded| degraded.code == BEADS_COMMAND_TIMEOUT_CODE)
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

#[cfg(test)]
mod source_run_adapter_tests {
    //! bd-12v87.3 — focused tests for the `SourceRunSwarmBriefRunner`
    //! adapter that bridges `SwarmBriefCommandRunner` calls onto the
    //! shared `source_run` watchdog (bd-12v87.2). Each test injects a
    //! `SourceRunExecutor` outcome and checks that the
    //! `SwarmBriefCommandError`/`SwarmBriefCommandOutput` translation
    //! preserves the contract the existing collectors already consume.
    //!
    //! These tests exercise the seam directly (via
    //! `run_source_command_with`) rather than spawning real
    //! subprocesses; the `SystemSwarmBriefCommandRunner` integration
    //! tests above still cover the spawn-and-drain path against the
    //! filesystem so we are not double-counting.
    use super::*;
    use crate::core::source_run::{
        SourceRunExecution, SourceRunExecutor, SourceRunKind, SourceRunPipeCapture,
        SourceRunRequest, SystemSourceRunClock, run_source_command_with,
    };
    use std::path::PathBuf;
    use std::time::Duration;

    fn pipe(text: &str) -> SourceRunPipeCapture {
        SourceRunPipeCapture::from_bytes(text.as_bytes(), SWARM_BRIEF_COMMAND_OUTPUT_LIMIT_BYTES)
    }

    struct FixedExecutor(SourceRunExecution);

    impl SourceRunExecutor for FixedExecutor {
        fn execute(&self, _request: &SourceRunRequest) -> SourceRunExecution {
            self.0.clone()
        }
    }

    fn run_through_adapter(
        program: &str,
        args: &[&str],
        timeout_ms: u64,
        execution: SourceRunExecution,
    ) -> Result<SwarmBriefCommandOutput, SwarmBriefCommandError> {
        let runner = SourceRunSwarmBriefRunner::new(SourceRunKind::Beads);
        let request = runner.build_request(program, args, &PathBuf::from("."), timeout_ms);
        let evidence =
            run_source_command_with(&request, &FixedExecutor(execution), &SystemSourceRunClock);
        translate_source_run_evidence(evidence)
    }

    #[test]
    fn passed_execution_returns_stdout_and_stderr() {
        let result = run_through_adapter(
            "br",
            &BEADS_READY_ARGS,
            1_500,
            SourceRunExecution::Completed {
                exit_code: Some(0),
                signal: None,
                stdout: pipe("[\n  {\"id\": \"bd-1\"}\n]\n"),
                stderr: pipe(""),
                elapsed: Duration::from_millis(12),
            },
        );
        let output = result.expect("passed execution must translate to Ok");
        assert!(output.stdout.contains("bd-1"));
        assert!(output.stderr.is_empty());
    }

    #[test]
    fn nonzero_exit_translates_to_failed_with_exit_code() {
        let result = run_through_adapter(
            "br",
            &BEADS_READY_ARGS,
            1_500,
            SourceRunExecution::Completed {
                exit_code: Some(2),
                signal: None,
                stdout: pipe(""),
                stderr: pipe("br: storage error\n"),
                elapsed: Duration::from_millis(8),
            },
        );
        match result {
            Err(SwarmBriefCommandError::Failed {
                status,
                stdout,
                stderr,
            }) => {
                assert_eq!(status, Some(2));
                assert!(stdout.is_empty());
                assert!(stderr.contains("storage error"));
            }
            other => panic!("expected Failed; got {other:?}"),
        }
    }

    #[test]
    fn timed_out_execution_propagates_timeout_ms() {
        let result = run_through_adapter(
            "br",
            &BEADS_READY_ARGS,
            1_500,
            SourceRunExecution::TimedOut {
                exit_code: None,
                signal: None,
                stdout: pipe(""),
                stderr: pipe(""),
                elapsed: Duration::from_millis(1_500),
                killed_own_child: true,
            },
        );
        match result {
            Err(SwarmBriefCommandError::TimedOut { timeout_ms }) => {
                assert_eq!(timeout_ms, 1_500);
            }
            other => panic!("expected TimedOut; got {other:?}"),
        }
    }

    #[test]
    fn spawn_failure_translates_to_unavailable() {
        let result = run_through_adapter(
            "br",
            &BEADS_READY_ARGS,
            1_500,
            SourceRunExecution::SpawnFailed {
                error: "No such file or directory".to_string(),
                elapsed: Duration::from_millis(1),
            },
        );
        match result {
            Err(SwarmBriefCommandError::Unavailable(message)) => {
                assert!(message.contains("spawn failed"), "got {message}");
                assert!(
                    message.contains("No such file or directory"),
                    "got {message}"
                );
            }
            other => panic!("expected Unavailable; got {other:?}"),
        }
    }

    #[test]
    fn adapter_uses_swarm_brief_output_byte_cap_not_default_tail() {
        // Regression guard: source_run's DEFAULT_TAIL_BYTES_MAX (8 KiB)
        // would silently truncate `git log` / broad `br ready` payloads
        // that the existing parsers expect to see in full. The adapter
        // raises tail_bytes_max to SWARM_BRIEF_COMMAND_OUTPUT_LIMIT_BYTES
        // (10 MiB) at build_request time.
        let runner = SourceRunSwarmBriefRunner::new(SourceRunKind::Beads);
        let request = runner.build_request("br", &BEADS_READY_ARGS, &PathBuf::from("."), 1_500);
        assert_eq!(
            request.tail_bytes_max,
            SWARM_BRIEF_COMMAND_OUTPUT_LIMIT_BYTES
        );
    }
}
