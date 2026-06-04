//! Read-only environment attestation collector.
//!
//! This module adapts existing swarm-brief source probes into the
//! `ee.environment_attestation.v1` payload shape. It intentionally owns no CLI
//! formatting and runs no Cargo/build/test command itself.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::core::swarm_brief::{
    SwarmBriefCollectOptions, SwarmBriefCommandRunner, SwarmBriefDegradation, SwarmBriefReport,
    SwarmBriefSourceKind, SwarmBriefSourceSnapshot, SwarmBriefSourceStatus, collect_swarm_brief,
};

pub const ENVIRONMENT_ATTESTATION_SCHEMA_V1: &str = "ee.environment_attestation.v1";
pub const ENVIRONMENT_ATTESTATION_REDACTION_STATUS: &str =
    "counts_ids_statuses_path_patterns_command_templates_no_mail_body_no_file_content";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentAttestationReport {
    pub schema: &'static str,
    pub attestation_id: String,
    pub workspace: String,
    pub generated_at: DateTime<Utc>,
    pub redaction_status: &'static str,
    pub summary: EnvironmentAttestationSummary,
    pub source_authority: Vec<EnvironmentAttestationSourceAuthorityEntry>,
    pub verdict: EnvironmentAttestationVerdict,
    pub evidence_refs: Vec<String>,
    pub recovery_actions: Vec<EnvironmentAttestationRecoveryAction>,
    pub degraded: Vec<EnvironmentAttestationDegradation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentAttestationSummary {
    pub safe_to_claim: bool,
    pub remote_verification_admitted: Option<bool>,
    pub source_test_verdict: EnvironmentAttestationSourceTestVerdict,
    pub environment_verdict: EnvironmentAttestationVerdict,
    pub local_cargo_fallback_observed: bool,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentAttestationVerdict {
    SafeToClaim,
    CoordinateBeforeClaim,
    UnsafeDueToConflict,
    RemoteVerificationAdmitted,
    ProofEnvironmentBlocked,
    SourceAuthorityAmbiguous,
    StaleBinarySuspected,
    TrackerStale,
    LocalCargoBypassDetected,
    UnknownInsufficientEvidence,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentAttestationSourceTestVerdict {
    NotEvaluated,
    SourceNotTested,
    SourcePassed,
    SourceFailed,
    EnvironmentBlockedBeforeSource,
    StaleSource,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentAttestationSourceKind {
    InstalledBinary,
    SourceTree,
    BeadsTracker,
    BvRecommendation,
    AgentMailMcp,
    AgentMailProbe,
    Rch,
    RchSourceMaterialization,
    BuildAdmission,
    LocalCargoTripwire,
    HostProfile,
    ClaimGate,
    FileReservations,
    SupportBundleRedaction,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentAttestationAuthority {
    Authoritative,
    Advisory,
    Degraded,
    Stale,
    Unavailable,
    Contradicted,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentAttestationSourceStatus {
    NotCollected,
    Ok,
    Stale,
    Unavailable,
    Degraded,
    Blocked,
    Contradicted,
    Ambiguous,
    LocalOnly,
    RemoteReady,
    RemoteBlocked,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentAttestationFreshness {
    Current,
    Stale,
    Unknown,
    NotApplicable,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentAttestationDegradedCode {
    AgentMailUnavailable,
    AgentMailProbeMismatch,
    BeadsTrackerStale,
    BeadsMetadataOnlyStale,
    BvRecommendationStale,
    RchUnavailable,
    RchWorkerTopologyBlocked,
    RchSourceMaterializationBlocked,
    RchRemoteRequiredFallbackPrevented,
    StaleBinarySuspected,
    SourceAuthorityAmbiguous,
    LocalCargoBypassDetected,
    DirtyCheckoutObserved,
    BuildAdmissionBlocked,
    SupportBundleRedactionUnverified,
    ReservationEvidenceStale,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentAttestationSourceAuthorityEntry {
    pub source: EnvironmentAttestationSourceKind,
    pub authority: EnvironmentAttestationAuthority,
    pub status: EnvironmentAttestationSourceStatus,
    pub freshness: EnvironmentAttestationFreshness,
    pub observed_at: Option<String>,
    pub summary: String,
    pub evidence_refs: Vec<String>,
    pub metrics: Vec<EnvironmentAttestationMetric>,
    pub degraded_codes: Vec<EnvironmentAttestationDegradedCode>,
    pub recovery_actions: Vec<EnvironmentAttestationRecoveryAction>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentAttestationMetric {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentAttestationRecoveryAction {
    pub priority: u8,
    pub kind: EnvironmentAttestationRecoveryKind,
    pub command: Option<EnvironmentAttestationCommandAction>,
    pub mutates_state: bool,
    pub required_substrate: EnvironmentAttestationSubstrate,
    pub rationale: String,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentAttestationRecoveryKind {
    Inspect,
    Coordinate,
    Sync,
    Rebuild,
    RerunRemote,
    RepairEnvironment,
    VerifyRedaction,
    HumanDecision,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentAttestationSubstrate {
    AgentMail,
    Beads,
    Bv,
    Ee,
    Git,
    Human,
    Rch,
    StaticLocal,
    None,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentAttestationCommandAction {
    pub display_command: String,
    pub argv: Vec<String>,
    pub shell_required: bool,
    pub copy_safety: EnvironmentAttestationCommandCopySafety,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentAttestationCommandCopySafety {
    SafeStructuredArgv,
    DisplayOnly,
    ShellRequiredReview,
    ForbiddenUntilHumanApproval,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct EnvironmentAttestationDegradation {
    pub code: EnvironmentAttestationDegradedCode,
    pub severity: &'static str,
    pub message: String,
    pub repair: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnvironmentAttestationInputs<'a> {
    pub generated_at: DateTime<Utc>,
    pub local_cargo_process_scan: Option<&'a Value>,
}

impl EnvironmentAttestationInputs<'_> {
    #[must_use]
    pub fn generated_now() -> Self {
        Self {
            generated_at: Utc::now(),
            local_cargo_process_scan: None,
        }
    }
}

#[must_use]
pub fn collect_environment_attestation(
    options: &SwarmBriefCollectOptions,
    runner: &impl SwarmBriefCommandRunner,
) -> EnvironmentAttestationReport {
    let report = collect_swarm_brief(options, runner);
    let local_cargo_process_scan =
        crate::core::support_bundle::local_cargo_tripwire_process_scan_json(&options.workspace);
    environment_attestation_from_swarm_brief_with_inputs(
        &report,
        EnvironmentAttestationInputs {
            generated_at: Utc::now(),
            local_cargo_process_scan: Some(&local_cargo_process_scan),
        },
    )
}

#[must_use]
pub fn environment_attestation_from_swarm_brief(
    report: &SwarmBriefReport,
    generated_at: DateTime<Utc>,
) -> EnvironmentAttestationReport {
    environment_attestation_from_swarm_brief_with_inputs(
        report,
        EnvironmentAttestationInputs {
            generated_at,
            local_cargo_process_scan: None,
        },
    )
}

#[must_use]
pub fn environment_attestation_from_swarm_brief_with_inputs(
    report: &SwarmBriefReport,
    inputs: EnvironmentAttestationInputs<'_>,
) -> EnvironmentAttestationReport {
    let mut entries = source_authority_entries(report);
    if let Some(process_scan) = inputs.local_cargo_process_scan {
        entries.push(local_cargo_tripwire_entry(process_scan));
    }
    if let Some(entry) = build_admission_entry(report) {
        entries.push(entry);
    }
    if let Some(entry) = file_reservation_entry(report) {
        entries.push(entry);
    }
    entries.sort_by(|left, right| {
        left.source
            .cmp(&right.source)
            .then_with(|| left.summary.cmp(&right.summary))
    });
    entries.dedup_by(|left, right| left.source == right.source);

    let summary = attestation_summary(&entries);
    let verdict = summary.environment_verdict;
    let evidence_refs = collect_evidence_refs(&entries);
    let recovery_actions = collect_recovery_actions(&entries);
    let degraded = collect_degraded_entries(&entries);
    let mut attestation = EnvironmentAttestationReport {
        schema: ENVIRONMENT_ATTESTATION_SCHEMA_V1,
        attestation_id: String::new(),
        workspace: report.workspace.clone(),
        generated_at: inputs.generated_at,
        redaction_status: ENVIRONMENT_ATTESTATION_REDACTION_STATUS,
        summary,
        source_authority: entries,
        verdict,
        evidence_refs,
        recovery_actions,
        degraded,
    };
    attestation.attestation_id = attestation_id(&attestation);
    attestation
}

fn source_authority_entries(
    report: &SwarmBriefReport,
) -> Vec<EnvironmentAttestationSourceAuthorityEntry> {
    let mut entries: Vec<_> = report
        .sources
        .iter()
        .filter_map(|snapshot| source_entry_from_snapshot(report, snapshot))
        .collect();
    if report.sources.is_empty() {
        entries.push(not_collected_entry(
            EnvironmentAttestationSourceKind::ClaimGate,
            "No swarm brief sources were collected.",
            "ee swarm brief --workspace . --include-rch --json",
        ));
    }
    entries
}

fn source_entry_from_snapshot(
    report: &SwarmBriefReport,
    snapshot: &SwarmBriefSourceSnapshot,
) -> Option<EnvironmentAttestationSourceAuthorityEntry> {
    let source = match snapshot.source {
        SwarmBriefSourceKind::AgentInventory
        | SwarmBriefSourceKind::MemoryDrift
        | SwarmBriefSourceKind::Qos => return None,
        SwarmBriefSourceKind::AgentMail => EnvironmentAttestationSourceKind::AgentMailProbe,
        SwarmBriefSourceKind::Beads => EnvironmentAttestationSourceKind::BeadsTracker,
        SwarmBriefSourceKind::Bv => EnvironmentAttestationSourceKind::BvRecommendation,
        SwarmBriefSourceKind::Git => EnvironmentAttestationSourceKind::SourceTree,
        SwarmBriefSourceKind::HostProfile => EnvironmentAttestationSourceKind::HostProfile,
        SwarmBriefSourceKind::Rch => EnvironmentAttestationSourceKind::Rch,
    };
    let degraded_codes = snapshot_degraded_codes(snapshot);
    let mut entry = EnvironmentAttestationSourceAuthorityEntry {
        source,
        authority: snapshot_authority(source, snapshot, &degraded_codes),
        status: snapshot_status(source, snapshot, &degraded_codes),
        freshness: freshness_from_swarm(snapshot.freshness.state),
        observed_at: snapshot.freshness.observed_at.clone(),
        summary: source_summary(report, source, snapshot),
        evidence_refs: source_evidence_refs(source),
        metrics: source_metrics(report, source, snapshot),
        degraded_codes,
        recovery_actions: snapshot_recovery_actions(snapshot),
    };
    if source == EnvironmentAttestationSourceKind::SourceTree && !report.dirty_files.is_empty() {
        insert_degraded_code(
            &mut entry.degraded_codes,
            EnvironmentAttestationDegradedCode::DirtyCheckoutObserved,
        );
        entry.status = EnvironmentAttestationSourceStatus::Degraded;
        entry.authority = EnvironmentAttestationAuthority::Degraded;
        entry
            .recovery_actions
            .push(EnvironmentAttestationRecoveryAction {
                priority: 0,
                kind: EnvironmentAttestationRecoveryKind::Coordinate,
                command: command_action("git status --short --branch --untracked-files=all"),
                mutates_state: false,
                required_substrate: EnvironmentAttestationSubstrate::Git,
                rationale:
                    "Inspect dirty path counts and coordinate before claiming overlapping work."
                        .to_owned(),
            });
    }
    entry.recovery_actions.sort();
    entry.recovery_actions.dedup();
    Some(entry)
}

fn source_summary(
    report: &SwarmBriefReport,
    source: EnvironmentAttestationSourceKind,
    snapshot: &SwarmBriefSourceSnapshot,
) -> String {
    match source {
        EnvironmentAttestationSourceKind::SourceTree => format!(
            "Git source collected with {} dirty path(s) and {} recent commit(s).",
            report.dirty_files.len(),
            report.recent_commits.len()
        ),
        EnvironmentAttestationSourceKind::BeadsTracker => format!(
            "Beads source collected {} ready, {} blocked, {} in-progress, and {} deferred item(s).",
            report.beads.ready.len(),
            report.beads.blocked.len(),
            report.beads.in_progress.len(),
            report.beads.deferred.len()
        ),
        EnvironmentAttestationSourceKind::BvRecommendation => {
            let pick_count = report
                .bv
                .as_ref()
                .map_or(0, |summary| summary.top_picks.len());
            format!("BV source collected {pick_count} top pick(s).")
        }
        EnvironmentAttestationSourceKind::AgentMailProbe => format!(
            "Agent Mail source collected {} reservation(s), {} inbox summary row(s), and {} thread summary row(s).",
            report.file_reservations.len(),
            report.inbox.len(),
            report.threads.len()
        ),
        EnvironmentAttestationSourceKind::Rch => {
            let posture =
                report
                    .rch_local_capability
                    .as_ref()
                    .map_or("not_collected", |capability| {
                        if capability.remote_only_safe {
                            "remote_only_safe"
                        } else if capability.remote_only_required {
                            "remote_required_not_safe"
                        } else {
                            "remote_not_required"
                        }
                    });
            format!(
                "RCH source status={} posture={posture}.",
                snapshot.status.as_str()
            )
        }
        EnvironmentAttestationSourceKind::HostProfile => report.host_profile.as_ref().map_or_else(
            || "Host profile source collected no detailed profile.".to_owned(),
            |summary| {
                format!(
                    "Host profile recommends {} with target-dir posture {}.",
                    summary.recommended_profile, summary.target_dir_posture
                )
            },
        ),
        _ => format!(
            "Source {} collected {} item(s).",
            source.as_str(),
            snapshot.item_count
        ),
    }
}

fn source_metrics(
    report: &SwarmBriefReport,
    source: EnvironmentAttestationSourceKind,
    snapshot: &SwarmBriefSourceSnapshot,
) -> Vec<EnvironmentAttestationMetric> {
    let mut metrics = vec![metric("item_count", snapshot.item_count)];
    match source {
        EnvironmentAttestationSourceKind::SourceTree => {
            metrics.push(metric("dirty_path_count", report.dirty_files.len()));
            metrics.push(metric("recent_commit_count", report.recent_commits.len()));
        }
        EnvironmentAttestationSourceKind::BeadsTracker => {
            metrics.push(metric("ready_count", report.beads.ready.len()));
            metrics.push(metric("blocked_count", report.beads.blocked.len()));
            metrics.push(metric("in_progress_count", report.beads.in_progress.len()));
            metrics.push(metric("deferred_count", report.beads.deferred.len()));
        }
        EnvironmentAttestationSourceKind::AgentMailProbe => {
            metrics.push(metric("reservation_count", report.file_reservations.len()));
            metrics.push(metric("inbox_summary_count", report.inbox.len()));
            metrics.push(metric("thread_summary_count", report.threads.len()));
        }
        EnvironmentAttestationSourceKind::Rch => {
            if let Some(capability) = &report.rch_local_capability {
                metrics.push(metric_bool(
                    "remote_only_required",
                    capability.remote_only_required,
                ));
                metrics.push(metric_bool("remote_only_safe", capability.remote_only_safe));
                metrics.push(metric(
                    "usable_worker_count",
                    capability.worker_pressure.usable_worker_count,
                ));
                metrics.push(metric(
                    "blocked_worker_count",
                    capability.worker_pressure.blocked_worker_count,
                ));
            }
        }
        EnvironmentAttestationSourceKind::HostProfile => {
            if let Some(summary) = &report.host_profile {
                metrics.push(metric_string(
                    "target_dir_posture",
                    summary.target_dir_posture.clone(),
                ));
                metrics.push(metric_bool(
                    "rch_hint_configured",
                    summary.rch_hint_configured,
                ));
            }
        }
        _ => {}
    }
    metrics.sort();
    metrics.dedup();
    metrics
}

fn local_cargo_tripwire_entry(process_scan: &Value) -> EnvironmentAttestationSourceAuthorityEntry {
    let status_text = process_scan
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unavailable");
    let count = process_scan
        .get("count")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let bypass_detected = status_text == "bypass_detected"
        || process_scan
            .get("detectedLocalBuilds")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty());
    let mut degraded_codes = Vec::new();
    let (authority, status, summary) = if bypass_detected {
        degraded_codes.push(EnvironmentAttestationDegradedCode::LocalCargoBypassDetected);
        (
            EnvironmentAttestationAuthority::Contradicted,
            EnvironmentAttestationSourceStatus::Blocked,
            format!(
                "Local Cargo process scan detected {count} disallowed local build process(es)."
            ),
        )
    } else if status_text == "unavailable" {
        degraded_codes.push(EnvironmentAttestationDegradedCode::SourceAuthorityAmbiguous);
        (
            EnvironmentAttestationAuthority::Unavailable,
            EnvironmentAttestationSourceStatus::Unavailable,
            "Local Cargo process scan was unavailable.".to_owned(),
        )
    } else {
        (
            EnvironmentAttestationAuthority::Authoritative,
            EnvironmentAttestationSourceStatus::Ok,
            "Local Cargo process scan found no disallowed local build process.".to_owned(),
        )
    };
    EnvironmentAttestationSourceAuthorityEntry {
        source: EnvironmentAttestationSourceKind::LocalCargoTripwire,
        authority,
        status,
        freshness: EnvironmentAttestationFreshness::Current,
        observed_at: None,
        summary,
        evidence_refs: vec!["tripwire://local-cargo-process-scan".to_owned()],
        metrics: vec![metric("detected_local_build_count", count)],
        degraded_codes,
        recovery_actions: local_cargo_recovery_actions(bypass_detected, status_text),
    }
}

fn local_cargo_recovery_actions(
    bypass_detected: bool,
    status_text: &str,
) -> Vec<EnvironmentAttestationRecoveryAction> {
    if bypass_detected {
        vec![EnvironmentAttestationRecoveryAction {
            priority: 0,
            kind: EnvironmentAttestationRecoveryKind::HumanDecision,
            command: None,
            mutates_state: false,
            required_substrate: EnvironmentAttestationSubstrate::Human,
            rationale: "Stop and resolve the local Cargo bypass before treating verification as remote-only proof."
                .to_owned(),
        }]
    } else if status_text == "unavailable" {
        vec![EnvironmentAttestationRecoveryAction {
            priority: 1,
            kind: EnvironmentAttestationRecoveryKind::Inspect,
            command: command_action(
                "scripts/check-local-cargo-tripwire.sh --probe-processes --json",
            ),
            mutates_state: false,
            required_substrate: EnvironmentAttestationSubstrate::StaticLocal,
            rationale: "Inspect why the read-only local Cargo process scan was unavailable."
                .to_owned(),
        }]
    } else {
        Vec::new()
    }
}

fn build_admission_entry(
    report: &SwarmBriefReport,
) -> Option<EnvironmentAttestationSourceAuthorityEntry> {
    let capability = report.rch_local_capability.as_ref()?;
    let mut degraded_codes = Vec::new();
    let (authority, status, summary) = if capability.remote_only_safe {
        (
            EnvironmentAttestationAuthority::Authoritative,
            EnvironmentAttestationSourceStatus::RemoteReady,
            "Build admission permits remote-only Cargo verification from this shell.".to_owned(),
        )
    } else {
        degraded_codes.push(EnvironmentAttestationDegradedCode::BuildAdmissionBlocked);
        (
            EnvironmentAttestationAuthority::Degraded,
            EnvironmentAttestationSourceStatus::Blocked,
            "Build admission does not permit remote-only Cargo verification from this shell."
                .to_owned(),
        )
    };
    let recovery_actions = if capability.remote_only_safe {
        Vec::new()
    } else {
        vec![EnvironmentAttestationRecoveryAction {
            priority: 0,
            kind: EnvironmentAttestationRecoveryKind::RepairEnvironment,
            command: command_action("rch status --json"),
            mutates_state: false,
            required_substrate: EnvironmentAttestationSubstrate::Rch,
            rationale: "Repair RCH readiness before launching Cargo verification.".to_owned(),
        }]
    };
    Some(EnvironmentAttestationSourceAuthorityEntry {
        source: EnvironmentAttestationSourceKind::BuildAdmission,
        authority,
        status,
        freshness: EnvironmentAttestationFreshness::Current,
        observed_at: None,
        summary,
        evidence_refs: vec!["swarm-brief://rch-local-capability".to_owned()],
        metrics: vec![
            metric_bool("remote_only_required", capability.remote_only_required),
            metric_bool("remote_only_safe", capability.remote_only_safe),
        ],
        degraded_codes,
        recovery_actions,
    })
}

fn file_reservation_entry(
    report: &SwarmBriefReport,
) -> Option<EnvironmentAttestationSourceAuthorityEntry> {
    if report.file_reservations.is_empty()
        && source_status(report, SwarmBriefSourceKind::AgentMail).is_none()
    {
        return None;
    }
    let exclusive_count = report
        .file_reservations
        .iter()
        .filter(|reservation| reservation.exclusive)
        .count();
    let mut degraded_codes = Vec::new();
    let (authority, status) = if exclusive_count > 0 {
        degraded_codes.push(EnvironmentAttestationDegradedCode::ReservationEvidenceStale);
        (
            EnvironmentAttestationAuthority::Advisory,
            EnvironmentAttestationSourceStatus::Blocked,
        )
    } else {
        (
            EnvironmentAttestationAuthority::Authoritative,
            EnvironmentAttestationSourceStatus::Ok,
        )
    };
    let recovery_actions = if exclusive_count > 0 {
        vec![EnvironmentAttestationRecoveryAction {
            priority: 0,
            kind: EnvironmentAttestationRecoveryKind::Coordinate,
            command: None,
            mutates_state: false,
            required_substrate: EnvironmentAttestationSubstrate::AgentMail,
            rationale: "Coordinate active file reservations before claiming overlapping surfaces."
                .to_owned(),
        }]
    } else {
        Vec::new()
    };
    Some(EnvironmentAttestationSourceAuthorityEntry {
        source: EnvironmentAttestationSourceKind::FileReservations,
        authority,
        status,
        freshness: EnvironmentAttestationFreshness::Current,
        observed_at: None,
        summary: format!(
            "File reservation source reports {} active reservation(s), {} exclusive.",
            report.file_reservations.len(),
            exclusive_count
        ),
        evidence_refs: vec!["agent-mail://file-reservations".to_owned()],
        metrics: vec![
            metric("active_reservation_count", report.file_reservations.len()),
            metric("exclusive_reservation_count", exclusive_count),
        ],
        degraded_codes,
        recovery_actions,
    })
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

fn snapshot_degraded_codes(
    snapshot: &SwarmBriefSourceSnapshot,
) -> Vec<EnvironmentAttestationDegradedCode> {
    let mut codes = Vec::new();
    for degraded in &snapshot.degraded {
        insert_degraded_code(&mut codes, map_degraded_code(&degraded.code));
    }
    codes.sort();
    codes.dedup();
    codes
}

fn map_degraded_code(code: &str) -> EnvironmentAttestationDegradedCode {
    match code {
        "agent_mail_unavailable" => EnvironmentAttestationDegradedCode::AgentMailUnavailable,
        "agent_mail_semantic_readiness_failed" | "agent_mail_probe_mismatch" => {
            EnvironmentAttestationDegradedCode::AgentMailProbeMismatch
        }
        "beads_tracker_stale" => EnvironmentAttestationDegradedCode::BeadsTrackerStale,
        "beads_tracker_metadata_drift" | "beads_metadata_only_stale" => {
            EnvironmentAttestationDegradedCode::BeadsMetadataOnlyStale
        }
        "bv_command_timeout" | "bv_no_output" | "bv_unavailable" | "bv_recommendation_stale" => {
            EnvironmentAttestationDegradedCode::BvRecommendationStale
        }
        "rch_unavailable" => EnvironmentAttestationDegradedCode::RchUnavailable,
        "rch_worker_topology_blocked" => {
            EnvironmentAttestationDegradedCode::RchWorkerTopologyBlocked
        }
        "rch_source_materialization_blocked" => {
            EnvironmentAttestationDegradedCode::RchSourceMaterializationBlocked
        }
        "rch_remote_required_fallback_prevented" => {
            EnvironmentAttestationDegradedCode::RchRemoteRequiredFallbackPrevented
        }
        "stale_binary_suspected" => EnvironmentAttestationDegradedCode::StaleBinarySuspected,
        "local_cargo_bypass_detected" => {
            EnvironmentAttestationDegradedCode::LocalCargoBypassDetected
        }
        "dirty_checkout_observed" => EnvironmentAttestationDegradedCode::DirtyCheckoutObserved,
        "build_admission_blocked" => EnvironmentAttestationDegradedCode::BuildAdmissionBlocked,
        "support_bundle_redaction_unverified" => {
            EnvironmentAttestationDegradedCode::SupportBundleRedactionUnverified
        }
        "reservation_evidence_stale" => {
            EnvironmentAttestationDegradedCode::ReservationEvidenceStale
        }
        _ => EnvironmentAttestationDegradedCode::SourceAuthorityAmbiguous,
    }
}

fn snapshot_authority(
    source: EnvironmentAttestationSourceKind,
    snapshot: &SwarmBriefSourceSnapshot,
    degraded_codes: &[EnvironmentAttestationDegradedCode],
) -> EnvironmentAttestationAuthority {
    if degraded_codes.contains(&EnvironmentAttestationDegradedCode::BeadsMetadataOnlyStale) {
        return EnvironmentAttestationAuthority::Authoritative;
    }
    if degraded_codes.contains(&EnvironmentAttestationDegradedCode::BeadsTrackerStale) {
        return EnvironmentAttestationAuthority::Stale;
    }
    if degraded_codes.contains(&EnvironmentAttestationDegradedCode::RchWorkerTopologyBlocked)
        || degraded_codes
            .contains(&EnvironmentAttestationDegradedCode::RchRemoteRequiredFallbackPrevented)
        || degraded_codes
            .contains(&EnvironmentAttestationDegradedCode::RchSourceMaterializationBlocked)
    {
        return EnvironmentAttestationAuthority::Degraded;
    }
    match snapshot.status {
        SwarmBriefSourceStatus::Ready => match source {
            EnvironmentAttestationSourceKind::BvRecommendation
            | EnvironmentAttestationSourceKind::HostProfile => {
                EnvironmentAttestationAuthority::Advisory
            }
            _ => EnvironmentAttestationAuthority::Authoritative,
        },
        SwarmBriefSourceStatus::Degraded => EnvironmentAttestationAuthority::Degraded,
        SwarmBriefSourceStatus::Unavailable | SwarmBriefSourceStatus::NotConfigured => {
            EnvironmentAttestationAuthority::Unavailable
        }
        SwarmBriefSourceStatus::Skipped => EnvironmentAttestationAuthority::Unavailable,
    }
}

fn snapshot_status(
    source: EnvironmentAttestationSourceKind,
    snapshot: &SwarmBriefSourceSnapshot,
    degraded_codes: &[EnvironmentAttestationDegradedCode],
) -> EnvironmentAttestationSourceStatus {
    if degraded_codes.contains(&EnvironmentAttestationDegradedCode::BeadsMetadataOnlyStale)
        || degraded_codes.contains(&EnvironmentAttestationDegradedCode::BeadsTrackerStale)
    {
        return EnvironmentAttestationSourceStatus::Stale;
    }
    if degraded_codes.contains(&EnvironmentAttestationDegradedCode::RchWorkerTopologyBlocked)
        || degraded_codes
            .contains(&EnvironmentAttestationDegradedCode::RchRemoteRequiredFallbackPrevented)
        || degraded_codes
            .contains(&EnvironmentAttestationDegradedCode::RchSourceMaterializationBlocked)
    {
        return EnvironmentAttestationSourceStatus::RemoteBlocked;
    }
    match snapshot.status {
        SwarmBriefSourceStatus::Ready => {
            if source == EnvironmentAttestationSourceKind::Rch {
                EnvironmentAttestationSourceStatus::RemoteReady
            } else if snapshot.freshness.state == "stale" {
                EnvironmentAttestationSourceStatus::Stale
            } else {
                EnvironmentAttestationSourceStatus::Ok
            }
        }
        SwarmBriefSourceStatus::Degraded => EnvironmentAttestationSourceStatus::Degraded,
        SwarmBriefSourceStatus::Unavailable | SwarmBriefSourceStatus::NotConfigured => {
            EnvironmentAttestationSourceStatus::Unavailable
        }
        SwarmBriefSourceStatus::Skipped => EnvironmentAttestationSourceStatus::NotCollected,
    }
}

fn freshness_from_swarm(state: &str) -> EnvironmentAttestationFreshness {
    match state {
        "current" => EnvironmentAttestationFreshness::Current,
        "stale" => EnvironmentAttestationFreshness::Stale,
        "not_applicable" => EnvironmentAttestationFreshness::NotApplicable,
        _ => EnvironmentAttestationFreshness::Unknown,
    }
}

fn snapshot_recovery_actions(
    snapshot: &SwarmBriefSourceSnapshot,
) -> Vec<EnvironmentAttestationRecoveryAction> {
    let mut actions: Vec<_> = snapshot
        .degraded
        .iter()
        .enumerate()
        .map(|(index, degraded)| recovery_action_for_degradation(index as u8, degraded))
        .collect();
    actions.sort();
    actions.dedup();
    actions
}

fn recovery_action_for_degradation(
    priority: u8,
    degraded: &SwarmBriefDegradation,
) -> EnvironmentAttestationRecoveryAction {
    match map_degraded_code(&degraded.code) {
        EnvironmentAttestationDegradedCode::BeadsMetadataOnlyStale => {
            EnvironmentAttestationRecoveryAction {
                priority,
                kind: EnvironmentAttestationRecoveryKind::Sync,
                command: command_action("br sync --flush-only --json"),
                mutates_state: true,
                required_substrate: EnvironmentAttestationSubstrate::Beads,
                rationale:
                    "Refresh Beads export metadata while preserving metadata-only read authority."
                        .to_owned(),
            }
        }
        EnvironmentAttestationDegradedCode::BeadsTrackerStale => {
            EnvironmentAttestationRecoveryAction {
                priority,
                kind: EnvironmentAttestationRecoveryKind::Sync,
                command: command_action("br sync --import-only"),
                mutates_state: true,
                required_substrate: EnvironmentAttestationSubstrate::Beads,
                rationale: "Import pending tracker records before using Beads as claim authority."
                    .to_owned(),
            }
        }
        EnvironmentAttestationDegradedCode::RchWorkerTopologyBlocked
        | EnvironmentAttestationDegradedCode::RchSourceMaterializationBlocked
        | EnvironmentAttestationDegradedCode::RchRemoteRequiredFallbackPrevented
        | EnvironmentAttestationDegradedCode::RchUnavailable => {
            EnvironmentAttestationRecoveryAction {
                priority,
                kind: EnvironmentAttestationRecoveryKind::RepairEnvironment,
                command: command_action("rch status --json"),
                mutates_state: false,
                required_substrate: EnvironmentAttestationSubstrate::Rch,
                rationale:
                    "Repair RCH readiness before treating remote Cargo verification as source evidence."
                        .to_owned(),
            }
        }
        EnvironmentAttestationDegradedCode::AgentMailUnavailable
        | EnvironmentAttestationDegradedCode::AgentMailProbeMismatch => {
            EnvironmentAttestationRecoveryAction {
                priority,
                kind: EnvironmentAttestationRecoveryKind::Coordinate,
                command: None,
                mutates_state: false,
                required_substrate: EnvironmentAttestationSubstrate::AgentMail,
                rationale: "Repair or refresh Agent Mail evidence before treating coordination as empty."
                    .to_owned(),
            }
        }
        EnvironmentAttestationDegradedCode::BvRecommendationStale => {
            EnvironmentAttestationRecoveryAction {
                priority,
                kind: EnvironmentAttestationRecoveryKind::Inspect,
                command: command_action("br --no-auto-import --allow-stale ready --json"),
                mutates_state: false,
                required_substrate: EnvironmentAttestationSubstrate::Beads,
                rationale: "Use bounded Beads fallback when BV recommendation evidence is stale."
                    .to_owned(),
            }
        }
        EnvironmentAttestationDegradedCode::DirtyCheckoutObserved
        | EnvironmentAttestationDegradedCode::ReservationEvidenceStale
        | EnvironmentAttestationDegradedCode::SourceAuthorityAmbiguous
        | EnvironmentAttestationDegradedCode::StaleBinarySuspected
        | EnvironmentAttestationDegradedCode::BuildAdmissionBlocked
        | EnvironmentAttestationDegradedCode::SupportBundleRedactionUnverified
        | EnvironmentAttestationDegradedCode::LocalCargoBypassDetected => {
            EnvironmentAttestationRecoveryAction {
                priority,
                kind: EnvironmentAttestationRecoveryKind::Inspect,
                command: degraded.repair.as_deref().and_then(command_action),
                mutates_state: false,
                required_substrate: EnvironmentAttestationSubstrate::StaticLocal,
                rationale: degraded.message.clone(),
            }
        }
    }
}

fn attestation_summary(
    entries: &[EnvironmentAttestationSourceAuthorityEntry],
) -> EnvironmentAttestationSummary {
    let codes = all_degraded_codes(entries);
    let local_cargo_fallback_observed =
        codes.contains(&EnvironmentAttestationDegradedCode::LocalCargoBypassDetected);
    let remote_environment_blocked = codes
        .contains(&EnvironmentAttestationDegradedCode::RchWorkerTopologyBlocked)
        || codes.contains(&EnvironmentAttestationDegradedCode::RchSourceMaterializationBlocked)
        || codes.contains(&EnvironmentAttestationDegradedCode::RchRemoteRequiredFallbackPrevented)
        || codes.contains(&EnvironmentAttestationDegradedCode::BuildAdmissionBlocked);
    let remote_verification_admitted = remote_verification_admitted(entries);
    let environment_verdict = if local_cargo_fallback_observed {
        EnvironmentAttestationVerdict::LocalCargoBypassDetected
    } else if remote_environment_blocked {
        EnvironmentAttestationVerdict::ProofEnvironmentBlocked
    } else if codes.contains(&EnvironmentAttestationDegradedCode::BeadsTrackerStale) {
        EnvironmentAttestationVerdict::TrackerStale
    } else if codes.contains(&EnvironmentAttestationDegradedCode::ReservationEvidenceStale) {
        EnvironmentAttestationVerdict::UnsafeDueToConflict
    } else if codes.contains(&EnvironmentAttestationDegradedCode::StaleBinarySuspected) {
        EnvironmentAttestationVerdict::StaleBinarySuspected
    } else if codes.contains(&EnvironmentAttestationDegradedCode::DirtyCheckoutObserved)
        || codes.contains(&EnvironmentAttestationDegradedCode::BeadsMetadataOnlyStale)
        || codes.contains(&EnvironmentAttestationDegradedCode::BvRecommendationStale)
        || codes.contains(&EnvironmentAttestationDegradedCode::AgentMailUnavailable)
        || codes.contains(&EnvironmentAttestationDegradedCode::AgentMailProbeMismatch)
    {
        EnvironmentAttestationVerdict::CoordinateBeforeClaim
    } else if codes.contains(&EnvironmentAttestationDegradedCode::SourceAuthorityAmbiguous) {
        EnvironmentAttestationVerdict::SourceAuthorityAmbiguous
    } else if remote_verification_admitted == Some(true) {
        EnvironmentAttestationVerdict::RemoteVerificationAdmitted
    } else if entries.is_empty() {
        EnvironmentAttestationVerdict::UnknownInsufficientEvidence
    } else {
        EnvironmentAttestationVerdict::SafeToClaim
    };
    EnvironmentAttestationSummary {
        safe_to_claim: environment_verdict == EnvironmentAttestationVerdict::SafeToClaim
            || environment_verdict == EnvironmentAttestationVerdict::RemoteVerificationAdmitted,
        remote_verification_admitted,
        source_test_verdict: if remote_environment_blocked {
            EnvironmentAttestationSourceTestVerdict::EnvironmentBlockedBeforeSource
        } else {
            EnvironmentAttestationSourceTestVerdict::NotEvaluated
        },
        environment_verdict,
        local_cargo_fallback_observed,
    }
}

fn all_degraded_codes(
    entries: &[EnvironmentAttestationSourceAuthorityEntry],
) -> BTreeSet<EnvironmentAttestationDegradedCode> {
    entries
        .iter()
        .flat_map(|entry| entry.degraded_codes.iter().copied())
        .collect()
}

fn remote_verification_admitted(
    entries: &[EnvironmentAttestationSourceAuthorityEntry],
) -> Option<bool> {
    let build = entries
        .iter()
        .find(|entry| entry.source == EnvironmentAttestationSourceKind::BuildAdmission);
    if let Some(entry) = build {
        return Some(entry.status == EnvironmentAttestationSourceStatus::RemoteReady);
    }
    let rch = entries
        .iter()
        .find(|entry| entry.source == EnvironmentAttestationSourceKind::Rch)?;
    match rch.status {
        EnvironmentAttestationSourceStatus::RemoteReady => Some(true),
        EnvironmentAttestationSourceStatus::RemoteBlocked
        | EnvironmentAttestationSourceStatus::Blocked
        | EnvironmentAttestationSourceStatus::Unavailable => Some(false),
        _ => None,
    }
}

fn collect_evidence_refs(entries: &[EnvironmentAttestationSourceAuthorityEntry]) -> Vec<String> {
    let mut refs: Vec<_> = entries
        .iter()
        .flat_map(|entry| entry.evidence_refs.iter().cloned())
        .collect();
    refs.sort();
    refs.dedup();
    refs
}

fn collect_recovery_actions(
    entries: &[EnvironmentAttestationSourceAuthorityEntry],
) -> Vec<EnvironmentAttestationRecoveryAction> {
    let mut actions: Vec<_> = entries
        .iter()
        .flat_map(|entry| entry.recovery_actions.iter().cloned())
        .collect();
    actions.sort();
    actions.dedup();
    actions
}

fn collect_degraded_entries(
    entries: &[EnvironmentAttestationSourceAuthorityEntry],
) -> Vec<EnvironmentAttestationDegradation> {
    let mut degraded = Vec::new();
    for entry in entries {
        for code in &entry.degraded_codes {
            degraded.push(EnvironmentAttestationDegradation {
                code: *code,
                severity: severity_for_degraded_code(*code),
                message: degradation_message(*code),
                repair: repair_for_degraded_code(*code),
            });
        }
    }
    degraded.sort();
    degraded.dedup();
    degraded
}

fn severity_for_degraded_code(code: EnvironmentAttestationDegradedCode) -> &'static str {
    match code {
        EnvironmentAttestationDegradedCode::LocalCargoBypassDetected
        | EnvironmentAttestationDegradedCode::RchWorkerTopologyBlocked
        | EnvironmentAttestationDegradedCode::RchSourceMaterializationBlocked
        | EnvironmentAttestationDegradedCode::RchRemoteRequiredFallbackPrevented
        | EnvironmentAttestationDegradedCode::BuildAdmissionBlocked => "high",
        _ => "warning",
    }
}

fn degradation_message(code: EnvironmentAttestationDegradedCode) -> String {
    match code {
        EnvironmentAttestationDegradedCode::AgentMailUnavailable => {
            "Agent Mail evidence was unavailable; do not treat coordination as empty."
        }
        EnvironmentAttestationDegradedCode::AgentMailProbeMismatch => {
            "Agent Mail probe and semantic readiness evidence disagreed."
        }
        EnvironmentAttestationDegradedCode::BeadsTrackerStale => {
            "Beads tracker content may be stale relative to JSONL."
        }
        EnvironmentAttestationDegradedCode::BeadsMetadataOnlyStale => {
            "Beads tracker metadata is stale while content remains synchronized."
        }
        EnvironmentAttestationDegradedCode::BvRecommendationStale => {
            "BV recommendation evidence was stale or unavailable."
        }
        EnvironmentAttestationDegradedCode::RchUnavailable => {
            "RCH status evidence was unavailable."
        }
        EnvironmentAttestationDegradedCode::RchWorkerTopologyBlocked => {
            "Remote verification was blocked before Cargo by RCH topology."
        }
        EnvironmentAttestationDegradedCode::RchSourceMaterializationBlocked => {
            "Remote verification source materialization was blocked before Cargo."
        }
        EnvironmentAttestationDegradedCode::RchRemoteRequiredFallbackPrevented => {
            "RCH remote-required mode prevented invalid local fallback."
        }
        EnvironmentAttestationDegradedCode::StaleBinarySuspected => {
            "Installed binary surface does not match current source contract."
        }
        EnvironmentAttestationDegradedCode::SourceAuthorityAmbiguous => {
            "At least one source could not be mapped to authoritative evidence."
        }
        EnvironmentAttestationDegradedCode::LocalCargoBypassDetected => {
            "Local Cargo process scan detected a local verification bypass."
        }
        EnvironmentAttestationDegradedCode::DirtyCheckoutObserved => {
            "Dirty checkout paths were observed."
        }
        EnvironmentAttestationDegradedCode::BuildAdmissionBlocked => {
            "Build admission blocked remote-only Cargo verification."
        }
        EnvironmentAttestationDegradedCode::SupportBundleRedactionUnverified => {
            "Support bundle redaction posture was not verified."
        }
        EnvironmentAttestationDegradedCode::ReservationEvidenceStale => {
            "Active file reservation evidence requires coordination."
        }
    }
    .to_owned()
}

fn repair_for_degraded_code(code: EnvironmentAttestationDegradedCode) -> Option<String> {
    match code {
        EnvironmentAttestationDegradedCode::BeadsTrackerStale => {
            Some("br sync --import-only".to_owned())
        }
        EnvironmentAttestationDegradedCode::BeadsMetadataOnlyStale => {
            Some("br sync --flush-only --json".to_owned())
        }
        EnvironmentAttestationDegradedCode::BvRecommendationStale => {
            Some("br --no-auto-import --allow-stale ready --json".to_owned())
        }
        EnvironmentAttestationDegradedCode::RchUnavailable
        | EnvironmentAttestationDegradedCode::RchWorkerTopologyBlocked
        | EnvironmentAttestationDegradedCode::RchSourceMaterializationBlocked
        | EnvironmentAttestationDegradedCode::RchRemoteRequiredFallbackPrevented
        | EnvironmentAttestationDegradedCode::BuildAdmissionBlocked => {
            Some("rch status --json".to_owned())
        }
        EnvironmentAttestationDegradedCode::DirtyCheckoutObserved => {
            Some("git status --short --branch --untracked-files=all".to_owned())
        }
        EnvironmentAttestationDegradedCode::AgentMailUnavailable
        | EnvironmentAttestationDegradedCode::AgentMailProbeMismatch
        | EnvironmentAttestationDegradedCode::StaleBinarySuspected
        | EnvironmentAttestationDegradedCode::SourceAuthorityAmbiguous
        | EnvironmentAttestationDegradedCode::LocalCargoBypassDetected
        | EnvironmentAttestationDegradedCode::SupportBundleRedactionUnverified
        | EnvironmentAttestationDegradedCode::ReservationEvidenceStale => None,
    }
}

fn source_evidence_refs(source: EnvironmentAttestationSourceKind) -> Vec<String> {
    vec![format!("swarm-brief://source/{}", source.as_str())]
}

fn not_collected_entry(
    source: EnvironmentAttestationSourceKind,
    summary: &str,
    inspect_command: &str,
) -> EnvironmentAttestationSourceAuthorityEntry {
    EnvironmentAttestationSourceAuthorityEntry {
        source,
        authority: EnvironmentAttestationAuthority::Unavailable,
        status: EnvironmentAttestationSourceStatus::NotCollected,
        freshness: EnvironmentAttestationFreshness::Unknown,
        observed_at: None,
        summary: summary.to_owned(),
        evidence_refs: Vec::new(),
        metrics: Vec::new(),
        degraded_codes: vec![EnvironmentAttestationDegradedCode::SourceAuthorityAmbiguous],
        recovery_actions: vec![EnvironmentAttestationRecoveryAction {
            priority: 0,
            kind: EnvironmentAttestationRecoveryKind::Inspect,
            command: command_action(inspect_command),
            mutates_state: false,
            required_substrate: EnvironmentAttestationSubstrate::Ee,
            rationale: "Collect source-authority inputs before using the attestation.".to_owned(),
        }],
    }
}

fn insert_degraded_code(
    codes: &mut Vec<EnvironmentAttestationDegradedCode>,
    code: EnvironmentAttestationDegradedCode,
) {
    if !codes.contains(&code) {
        codes.push(code);
    }
}

fn command_action(command: &str) -> Option<EnvironmentAttestationCommandAction> {
    let argv = command
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if argv.is_empty() {
        None
    } else {
        Some(EnvironmentAttestationCommandAction {
            display_command: command.to_owned(),
            argv,
            shell_required: false,
            copy_safety: EnvironmentAttestationCommandCopySafety::SafeStructuredArgv,
        })
    }
}

fn metric(name: &str, value: impl ToString) -> EnvironmentAttestationMetric {
    EnvironmentAttestationMetric {
        name: name.to_owned(),
        value: value.to_string(),
    }
}

fn metric_bool(name: &str, value: bool) -> EnvironmentAttestationMetric {
    metric_string(name, value.to_string())
}

fn metric_string(name: &str, value: String) -> EnvironmentAttestationMetric {
    EnvironmentAttestationMetric {
        name: name.to_owned(),
        value,
    }
}

impl EnvironmentAttestationSourceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InstalledBinary => "installed_binary",
            Self::SourceTree => "source_tree",
            Self::BeadsTracker => "beads_tracker",
            Self::BvRecommendation => "bv_recommendation",
            Self::AgentMailMcp => "agent_mail_mcp",
            Self::AgentMailProbe => "agent_mail_probe",
            Self::Rch => "rch",
            Self::RchSourceMaterialization => "rch_source_materialization",
            Self::BuildAdmission => "build_admission",
            Self::LocalCargoTripwire => "local_cargo_tripwire",
            Self::HostProfile => "host_profile",
            Self::ClaimGate => "claim_gate",
            Self::FileReservations => "file_reservations",
            Self::SupportBundleRedaction => "support_bundle_redaction",
        }
    }
}

fn attestation_id(attestation: &EnvironmentAttestationReport) -> String {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Signature<'a> {
        workspace: &'a str,
        summary: &'a EnvironmentAttestationSummary,
        source_authority: &'a [EnvironmentAttestationSourceAuthorityEntry],
        verdict: EnvironmentAttestationVerdict,
        evidence_refs: &'a [String],
        recovery_actions: &'a [EnvironmentAttestationRecoveryAction],
        degraded: &'a [EnvironmentAttestationDegradation],
    }

    let signature = Signature {
        workspace: &attestation.workspace,
        summary: &attestation.summary,
        source_authority: &attestation.source_authority,
        verdict: attestation.verdict,
        evidence_refs: &attestation.evidence_refs,
        recovery_actions: &attestation.recovery_actions,
        degraded: &attestation.degraded,
    };
    let bytes = serde_json::to_vec(&signature)
        .unwrap_or_else(|_| b"environment_attestation_signature_error".to_vec());
    let hash = blake3::hash(&bytes);
    let hex = hash.to_hex();
    format!("environment_attestation_{}", &hex.as_str()[..24])
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::*;
    use crate::core::swarm_brief::{
        SwarmBriefBead, SwarmBriefDirtyFile, SwarmBriefFileReservation, SwarmBriefSourceFreshness,
        SwarmBriefSourceProvenance,
    };

    fn fixed_time() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 4, 20, 0, 0)
            .single()
            .unwrap_or_else(Utc::now)
    }

    fn report_with_sources(sources: Vec<SwarmBriefSourceSnapshot>) -> SwarmBriefReport {
        let mut report = SwarmBriefReport::empty(Path::new("."));
        report.sources = sources;
        report.finalize();
        report
    }

    fn ready_source(source: SwarmBriefSourceKind) -> SwarmBriefSourceSnapshot {
        SwarmBriefSourceSnapshot::ready(source, SwarmBriefSourceProvenance::local_probe(), 1)
    }

    fn degraded_source(
        source: SwarmBriefSourceKind,
        code: &str,
        message: &str,
    ) -> SwarmBriefSourceSnapshot {
        SwarmBriefSourceSnapshot {
            source,
            status: SwarmBriefSourceStatus::Degraded,
            freshness: SwarmBriefSourceFreshness::current(),
            provenance: SwarmBriefSourceProvenance::local_probe(),
            item_count: 0,
            degraded: vec![SwarmBriefDegradation::warning(
                source,
                code,
                message,
                Some("inspect".to_owned()),
            )],
        }
    }

    fn source_kinds(
        report: &EnvironmentAttestationReport,
    ) -> Vec<EnvironmentAttestationSourceKind> {
        report
            .source_authority
            .iter()
            .map(|entry| entry.source)
            .collect()
    }

    fn entry(
        report: &EnvironmentAttestationReport,
        source: EnvironmentAttestationSourceKind,
    ) -> &EnvironmentAttestationSourceAuthorityEntry {
        match report
            .source_authority
            .iter()
            .find(|entry| entry.source == source)
        {
            Some(entry) => entry,
            None => panic!("missing source entry {source:?}"),
        }
    }

    #[test]
    fn attestation_orders_sources_and_marks_dirty_checkout_for_coordination() {
        let mut brief = report_with_sources(vec![
            ready_source(SwarmBriefSourceKind::Bv),
            ready_source(SwarmBriefSourceKind::Git),
            ready_source(SwarmBriefSourceKind::Beads),
        ]);
        brief.dirty_files.push(SwarmBriefDirtyFile {
            path: "-".to_owned(),
            status: "??".to_owned(),
        });
        let attestation = environment_attestation_from_swarm_brief(&brief, fixed_time());

        assert_eq!(
            source_kinds(&attestation),
            vec![
                EnvironmentAttestationSourceKind::SourceTree,
                EnvironmentAttestationSourceKind::BeadsTracker,
                EnvironmentAttestationSourceKind::BvRecommendation,
            ]
        );
        let source_tree = entry(&attestation, EnvironmentAttestationSourceKind::SourceTree);
        assert_eq!(
            source_tree.status,
            EnvironmentAttestationSourceStatus::Degraded
        );
        assert!(
            source_tree
                .degraded_codes
                .contains(&EnvironmentAttestationDegradedCode::DirtyCheckoutObserved)
        );
        assert_eq!(
            attestation.verdict,
            EnvironmentAttestationVerdict::CoordinateBeforeClaim
        );
    }

    #[test]
    fn metadata_only_beads_drift_keeps_authority_and_precise_repair() {
        let brief = report_with_sources(vec![SwarmBriefSourceSnapshot {
            source: SwarmBriefSourceKind::Beads,
            status: SwarmBriefSourceStatus::Ready,
            freshness: SwarmBriefSourceFreshness::current(),
            provenance: SwarmBriefSourceProvenance::command("br", &["sync", "--status", "--json"]),
            item_count: 6,
            degraded: vec![SwarmBriefDegradation::info(
                SwarmBriefSourceKind::Beads,
                "beads_tracker_metadata_drift",
                "metadata-only drift",
                Some("br sync --flush-only --json".to_owned()),
            )],
        }]);
        let attestation = environment_attestation_from_swarm_brief(&brief, fixed_time());
        let beads = entry(&attestation, EnvironmentAttestationSourceKind::BeadsTracker);

        assert_eq!(
            beads.authority,
            EnvironmentAttestationAuthority::Authoritative
        );
        assert_eq!(beads.status, EnvironmentAttestationSourceStatus::Stale);
        assert_eq!(
            beads.degraded_codes,
            vec![EnvironmentAttestationDegradedCode::BeadsMetadataOnlyStale]
        );
        assert!(beads.recovery_actions.iter().any(|action| {
            action
                .command
                .as_ref()
                .is_some_and(|command| command.display_command == "br sync --flush-only --json")
        }));
    }

    #[test]
    fn rch_topology_blocker_is_environment_blocked_not_source_failure() {
        let brief = report_with_sources(vec![degraded_source(
            SwarmBriefSourceKind::Rch,
            "rch_worker_topology_blocked",
            "RCH-E327 blocked before Cargo",
        )]);
        let attestation = environment_attestation_from_swarm_brief(&brief, fixed_time());
        let rch = entry(&attestation, EnvironmentAttestationSourceKind::Rch);

        assert_eq!(
            rch.status,
            EnvironmentAttestationSourceStatus::RemoteBlocked
        );
        assert_eq!(
            attestation.summary.source_test_verdict,
            EnvironmentAttestationSourceTestVerdict::EnvironmentBlockedBeforeSource
        );
        assert_eq!(
            attestation.verdict,
            EnvironmentAttestationVerdict::ProofEnvironmentBlocked
        );
        assert_eq!(
            attestation.summary.remote_verification_admitted,
            Some(false)
        );
    }

    #[test]
    fn local_cargo_process_scan_detects_bypass_without_mutating_repair_action() {
        let brief = report_with_sources(vec![ready_source(SwarmBriefSourceKind::Git)]);
        let process_scan = json!({
            "schema": "ee.rch_local_cargo_tripwire.v1",
            "mode": "probe_processes",
            "status": "bypass_detected",
            "count": 1,
            "detectedLocalBuilds": [{"kind": "cargo"}],
            "evidence": [{"kind": "active_process_scan", "result": "bypass_detected"}]
        });
        let attestation = environment_attestation_from_swarm_brief_with_inputs(
            &brief,
            EnvironmentAttestationInputs {
                generated_at: fixed_time(),
                local_cargo_process_scan: Some(&process_scan),
            },
        );
        let tripwire = entry(
            &attestation,
            EnvironmentAttestationSourceKind::LocalCargoTripwire,
        );

        assert_eq!(tripwire.status, EnvironmentAttestationSourceStatus::Blocked);
        assert_eq!(
            attestation.verdict,
            EnvironmentAttestationVerdict::LocalCargoBypassDetected
        );
        assert!(attestation.summary.local_cargo_fallback_observed);
        assert!(
            tripwire
                .recovery_actions
                .iter()
                .all(|action| !action.mutates_state)
        );
    }

    #[test]
    fn stale_bv_and_agent_mail_sources_normalize_to_schema_codes() {
        let brief = report_with_sources(vec![
            degraded_source(
                SwarmBriefSourceKind::Bv,
                "bv_command_timeout",
                "BV timed out",
            ),
            degraded_source(
                SwarmBriefSourceKind::AgentMail,
                "agent_mail_unavailable",
                "Agent Mail unavailable",
            ),
        ]);
        let attestation = environment_attestation_from_swarm_brief(&brief, fixed_time());
        let bv = entry(
            &attestation,
            EnvironmentAttestationSourceKind::BvRecommendation,
        );
        let mail = entry(
            &attestation,
            EnvironmentAttestationSourceKind::AgentMailProbe,
        );

        assert_eq!(
            bv.degraded_codes,
            vec![EnvironmentAttestationDegradedCode::BvRecommendationStale]
        );
        assert_eq!(
            mail.degraded_codes,
            vec![EnvironmentAttestationDegradedCode::AgentMailUnavailable]
        );
        assert_eq!(
            attestation.verdict,
            EnvironmentAttestationVerdict::CoordinateBeforeClaim
        );
    }

    #[test]
    fn file_reservation_entry_blocks_conflicting_claims_deterministically() {
        let mut brief = report_with_sources(vec![ready_source(SwarmBriefSourceKind::AgentMail)]);
        brief.file_reservations.push(SwarmBriefFileReservation {
            path_pattern: "src/core/*.rs".to_owned(),
            holder: "RedactedAgent".to_owned(),
            exclusive: true,
            expires_at: Some("2026-06-04T22:00:00Z".to_owned()),
        });
        brief.beads.ready.push(SwarmBriefBead {
            id: "bd-example".to_owned(),
            title: "example".to_owned(),
            status: "open".to_owned(),
            priority: Some(1),
            assignee: None,
            issue_type: Some("task".to_owned()),
            created_at: None,
            updated_at: None,
            latest_comment_at: None,
            comment_count: 0,
            source_bucket: "ready".to_owned(),
        });
        let attestation = environment_attestation_from_swarm_brief(&brief, fixed_time());
        let reservations = entry(
            &attestation,
            EnvironmentAttestationSourceKind::FileReservations,
        );

        assert_eq!(
            reservations.degraded_codes,
            vec![EnvironmentAttestationDegradedCode::ReservationEvidenceStale]
        );
        assert_eq!(
            attestation.verdict,
            EnvironmentAttestationVerdict::UnsafeDueToConflict
        );
    }
}
