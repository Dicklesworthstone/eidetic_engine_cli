//! Workspace registry, resolution, and alias commands.
//!
//! The registry is a local FrankenSQLite database that reuses the existing
//! `workspaces` table. `workspaces.name` is the stable human alias for a
//! workspace path, while the deterministic workspace ID remains path-derived.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::config::{
    EnvVar, WORKSPACE_MARKER, WorkspaceDiagnostic, WorkspaceResolutionMode,
    WorkspaceResolutionRequest, WorkspaceResolutionSource, WorkspaceScope, derive_workspace_scope,
    diagnose_workspace_resolution, read_env_var, resolve_workspace,
};
use crate::core::hygiene_beads_state::{
    BEADS_JSONL_MAX_INSPECT_BYTES, BEADS_JSONL_RELATIVE_PATH, BeadsHygieneInputs,
    BeadsHygieneState, BeadsMetadataSignal, BeadsReservationHolder, classify_beads_state,
};
use crate::core::hygiene_classifier::{
    Bucket, ClassificationRow, HygieneClassifierConfig, Kind, SecretEvidenceLookup,
    classify_workspace_with_config,
};
use crate::core::hygiene_coordination::{
    AgentMailCoordinationInput, HygieneCoordinationOverlay, overlay_coordination_state,
};
use crate::core::swarm_brief::{
    SystemSwarmBriefCommandRunner, WorkspaceGitSnapshot, WorkspaceGitSnapshotOptions,
    collect_workspace_git_snapshot, parse_agent_mail_snapshot_json,
};
use crate::db::{
    CreateAuditInput, CreateWorkspaceInput, DatabaseConfig, DbConnection, StoredWorkspace,
    WorkspaceScopeFields, generate_audit_id,
};
use crate::models::degradation::{
    WORKSPACE_HYGIENE_AGENT_MAIL_UNAVAILABLE_CODE, WORKSPACE_HYGIENE_OUTPUT_TRUNCATED_CODE,
    WORKSPACE_HYGIENE_PARTIAL_METADATA_CODE, WORKSPACE_HYGIENE_SECRET_SCAN_SKIPPED_CODE,
};
use crate::models::{DomainError, WorkspaceId};
use crate::policy::{WORKSPACE_SECRET_RISK_DEFAULT_MAX_SCAN_BYTES, workspace_secret_risk_evidence};
use crate::runtime::determinism::{Deterministic, Seed};

pub const WORKSPACE_REGISTRY_SCHEMA_V1: &str = "ee.workspace.registry.v1";
pub const WORKSPACE_ALIAS_SCHEMA_V1: &str = "ee.workspace.alias.v1";
pub const WORKSPACE_RESOLVE_SCHEMA_V1: &str = "ee.workspace.resolve.v1";
pub const WORKSPACE_HYGIENE_SCHEMA_V1: &str = "ee.workspace_hygiene.v1";
pub const WORKSPACE_REGISTRY_ENV_VAR: &str = EnvVar::WorkspaceRegistry.name();

const WORKSPACE_ALIAS_SET_ACTION: &str = "workspace.alias.set";
const WORKSPACE_ALIAS_CLEAR_ACTION: &str = "workspace.alias.clear";
pub const WORKSPACE_HYGIENE_MAX_PATH_CLASSIFICATIONS: usize = 10_000;
pub const WORKSPACE_HYGIENE_MAX_PATHS_PER_LIST: usize = 10_000;
pub const WORKSPACE_HYGIENE_MAX_PATHS_PER_STAGING_GROUP: usize = 10_000;
pub const WORKSPACE_HYGIENE_SECRET_SCAN_MAX_FILES: usize = 1_000;
pub const WORKSPACE_HYGIENE_SECRET_SCAN_MAX_TOTAL_BYTES: usize = 1_000_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceListOptions {
    pub registry_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceResolveOptions {
    pub workspace_path: Option<PathBuf>,
    pub target: Option<String>,
    pub registry_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceAliasOptions {
    pub workspace_path: Option<PathBuf>,
    pub pick: Option<String>,
    pub alias: Option<String>,
    pub clear: bool,
    pub dry_run: bool,
    pub registry_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceHygieneOptions {
    pub workspace_path: PathBuf,
    pub self_agent_name: Option<String>,
    pub agent_mail_snapshot_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceEntry {
    pub workspace_id: String,
    pub path: String,
    pub alias: Option<String>,
    pub scope_kind: String,
    pub repository_root: Option<String>,
    pub repository_fingerprint: Option<String>,
    pub subproject_path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<StoredWorkspace> for WorkspaceEntry {
    fn from(workspace: StoredWorkspace) -> Self {
        Self {
            workspace_id: workspace.id,
            path: workspace.path,
            alias: workspace.name,
            scope_kind: workspace.scope_kind,
            repository_root: workspace.repository_root,
            repository_fingerprint: workspace.repository_fingerprint,
            subproject_path: workspace.subproject_path,
            created_at: workspace.created_at,
            updated_at: workspace.updated_at,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceListReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub registry_path: String,
    pub registry_exists: bool,
    pub workspaces: Vec<WorkspaceEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAliasReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub status: &'static str,
    pub registry_path: String,
    pub workspace_id: String,
    pub workspace_path: String,
    pub alias: Option<String>,
    pub previous_alias: Option<String>,
    pub scope_kind: String,
    pub repository_root: Option<String>,
    pub repository_fingerprint: Option<String>,
    pub subproject_path: Option<String>,
    pub dry_run: bool,
    pub persisted: bool,
    pub audit_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceResolveReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub source: String,
    pub target: Option<String>,
    pub workspace_id: String,
    pub root: String,
    pub canonical_root: String,
    pub marker_present: bool,
    pub alias: Option<String>,
    pub scope_kind: String,
    pub repository_root: Option<String>,
    pub repository_fingerprint: Option<String>,
    pub subproject_path: Option<String>,
    pub registry_path: String,
    pub diagnostics: Vec<WorkspaceDiagnosticEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceHygieneReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub read_only: bool,
    #[serde(rename = "workspace")]
    pub workspace_path: String,
    #[serde(rename = "gitSummary")]
    pub git_summary: WorkspaceHygieneGitSummary,
    pub repository_root: String,
    pub dirty_path_count: usize,
    pub bucket_counts: Vec<WorkspaceHygieneCount>,
    pub kind_counts: Vec<WorkspaceHygieneCount>,
    #[serde(rename = "stagingRecommendations")]
    pub staging_groups: Vec<WorkspaceHygieneStagingGroup>,
    #[serde(rename = "pathClassifications")]
    pub classifications: Vec<ClassificationRow>,
    #[serde(rename = "doNotCommit")]
    pub do_not_commit: Vec<String>,
    #[serde(rename = "needsHumanReview")]
    pub needs_human_review: Vec<String>,
    #[serde(rename = "outputTruncation")]
    pub output_truncation: WorkspaceHygieneOutputTruncation,
    #[serde(rename = "secretScan")]
    pub secret_scan: WorkspaceHygieneSecretScanReport,
    #[serde(rename = "beadsState")]
    pub beads_state: BeadsHygieneState,
    #[serde(rename = "coordinationState")]
    pub coordination: HygieneCoordinationOverlay,
    #[serde(rename = "degraded")]
    pub degraded_codes: Vec<&'static str>,
    #[serde(rename = "nextActions")]
    pub next_actions: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceHygieneGitSummary {
    pub repository_root: String,
    pub dirty_path_count: usize,
    pub bucket_counts: Vec<WorkspaceHygieneCount>,
    pub kind_counts: Vec<WorkspaceHygieneCount>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceHygieneCount {
    pub name: String,
    pub count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceHygieneStagingGroup {
    pub name: String,
    pub paths: Vec<String>,
    pub path_count: usize,
    pub paths_truncated: bool,
    pub omitted_path_count: usize,
    pub kinds: Vec<String>,
    pub reasons: Vec<String>,
    pub recommendation: &'static str,
    pub read_only: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceHygieneOutputTruncation {
    pub truncated: bool,
    pub max_path_classifications: usize,
    pub max_paths_per_list: usize,
    pub max_paths_per_staging_group: usize,
    pub omitted_path_classifications: usize,
    pub omitted_do_not_commit: usize,
    pub omitted_needs_human_review: usize,
    pub omitted_by_bucket: Vec<WorkspaceHygieneCount>,
    pub omitted_by_kind: Vec<WorkspaceHygieneCount>,
    pub staging_groups: Vec<WorkspaceHygieneStagingGroupTruncation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceHygieneStagingGroupTruncation {
    pub name: String,
    pub omitted_path_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceHygieneSecretScanReport {
    pub read_only: bool,
    pub scanned_file_count: usize,
    pub scanned_byte_count: usize,
    pub skipped_content_scan_count: usize,
    pub max_files: usize,
    pub max_file_bytes: usize,
    pub max_total_bytes: usize,
}

struct WorkspaceHygieneReportInputs<'a> {
    workspace_path: &'a Path,
    snapshot: WorkspaceGitSnapshot,
    classifier_config: &'a HygieneClassifierConfig,
    jsonl_content: Option<&'a [u8]>,
    self_agent_name: Option<&'a str>,
    beads_metadata_signal: BeadsMetadataSignal,
    beads_reservations: &'a [BeadsReservationHolder],
    agent_mail_input: &'a AgentMailCoordinationInput,
    now: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WorkspaceHygieneSecretScanBudget {
    max_files: usize,
    max_file_bytes: usize,
    max_total_bytes: usize,
}

impl Default for WorkspaceHygieneSecretScanBudget {
    fn default() -> Self {
        Self {
            max_files: WORKSPACE_HYGIENE_SECRET_SCAN_MAX_FILES,
            max_file_bytes: WORKSPACE_SECRET_RISK_DEFAULT_MAX_SCAN_BYTES,
            max_total_bytes: WORKSPACE_HYGIENE_SECRET_SCAN_MAX_TOTAL_BYTES,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct WorkspaceHygieneSecretScanSummary {
    scanned_file_count: usize,
    scanned_byte_count: usize,
    skipped_content_scan_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceDiagnosticEntry {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: String,
    pub selected_source: Option<&'static str>,
    pub selected_root: Option<String>,
    pub conflicting_source: Option<&'static str>,
    pub conflicting_root: Option<String>,
    pub marker_roots: Vec<String>,
}

impl From<WorkspaceDiagnostic> for WorkspaceDiagnosticEntry {
    fn from(diagnostic: WorkspaceDiagnostic) -> Self {
        Self {
            code: diagnostic.code,
            severity: diagnostic.severity.as_str(),
            message: diagnostic.message,
            repair: diagnostic.repair,
            selected_source: diagnostic
                .selected_source
                .map(WorkspaceResolutionSource::as_str),
            selected_root: diagnostic
                .selected_root
                .map(|path| path.display().to_string()),
            conflicting_source: diagnostic
                .conflicting_source
                .map(WorkspaceResolutionSource::as_str),
            conflicting_root: diagnostic
                .conflicting_root
                .map(|path| path.display().to_string()),
            marker_roots: diagnostic
                .marker_roots
                .into_iter()
                .map(|path| path.display().to_string())
                .collect(),
        }
    }
}

#[must_use]
pub fn registry_database_path_override(override_path: Option<&Path>) -> PathBuf {
    if let Some(path) = override_path {
        return path.to_path_buf();
    }
    if let Some(path) = read_env_var(EnvVar::WorkspaceRegistry) {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    if let Ok(xdg_data) = env::var("XDG_DATA_HOME") {
        return PathBuf::from(xdg_data).join("ee").join("workspaces.db");
    }
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("ee")
            .join("workspaces.db");
    }
    env::temp_dir().join("ee").join("workspaces.db")
}

#[must_use]
pub fn resolve_workspace_alias_for_cli(raw: &Path) -> Option<PathBuf> {
    if looks_like_path(raw) {
        return None;
    }
    let alias = raw.to_str()?;
    let normalized = normalize_alias(alias).ok()?;
    let registry_path = registry_database_path_override(None);
    let row = find_alias_read_only(&registry_path, &normalized).ok()??;
    Some(PathBuf::from(row.path))
}

pub fn list_workspace_registry(
    options: &WorkspaceListOptions,
) -> Result<WorkspaceListReport, DomainError> {
    let registry_path = registry_database_path_override(options.registry_path.as_deref());
    if !registry_file_exists(&registry_path)? {
        return Ok(WorkspaceListReport {
            schema: WORKSPACE_REGISTRY_SCHEMA_V1,
            command: "workspace list",
            registry_path: registry_path.display().to_string(),
            registry_exists: false,
            workspaces: Vec::new(),
        });
    }

    let conn = open_registry_read_only(&registry_path)?;
    let workspaces = conn
        .list_workspaces()
        .map_err(|error| storage_error("failed to list workspace registry", error))?
        .into_iter()
        .map(WorkspaceEntry::from)
        .collect();

    Ok(WorkspaceListReport {
        schema: WORKSPACE_REGISTRY_SCHEMA_V1,
        command: "workspace list",
        registry_path: registry_path.display().to_string(),
        registry_exists: true,
        workspaces,
    })
}

pub fn resolve_workspace_report(
    options: &WorkspaceResolveOptions,
) -> Result<WorkspaceResolveReport, DomainError> {
    let registry_path = registry_database_path_override(options.registry_path.as_deref());
    if let Some(target) = options.target.as_deref() {
        if !looks_like_path(Path::new(target)) {
            let alias = normalize_alias(target).map_err(alias_usage_error)?;
            let row = find_alias_read_only(&registry_path, &alias)?.ok_or_else(|| {
                DomainError::NotFound {
                    resource: "workspace alias".to_string(),
                    id: alias.clone(),
                    repair: Some("ee workspace list --json".to_string()),
                }
            })?;
            return Ok(resolve_alias_row_report(&registry_path, target, row));
        }

        return resolve_path_report(&registry_path, Some(target), Some(PathBuf::from(target)));
    }

    resolve_path_report(&registry_path, None, options.workspace_path.clone())
}

pub fn build_workspace_hygiene_report(
    options: &WorkspaceHygieneOptions,
) -> Result<WorkspaceHygieneReport, DomainError> {
    let classifier_config = workspace_hygiene_classifier_config()?;
    let snapshot_options = WorkspaceGitSnapshotOptions::for_workspace(&options.workspace_path);
    let snapshot =
        collect_workspace_git_snapshot(&snapshot_options, &SystemSwarmBriefCommandRunner)
            .map_err(workspace_git_error)?;
    let jsonl_content = read_bounded_file(
        &options.workspace_path.join(BEADS_JSONL_RELATIVE_PATH),
        BEADS_JSONL_MAX_INSPECT_BYTES + 1,
    )
    .ok();
    let beads_metadata_signal = detect_beads_metadata_signal(&options.workspace_path);
    let agent_mail_input =
        load_agent_mail_coordination_input(options.agent_mail_snapshot_path.as_deref());

    Ok(build_workspace_hygiene_report_from_inputs(
        WorkspaceHygieneReportInputs {
            workspace_path: &options.workspace_path,
            snapshot,
            classifier_config: &classifier_config,
            jsonl_content: jsonl_content.as_deref(),
            self_agent_name: options.self_agent_name.as_deref(),
            beads_metadata_signal,
            beads_reservations: &[],
            agent_mail_input: &agent_mail_input,
            now: Utc::now(),
        },
    ))
}

fn build_workspace_hygiene_report_from_inputs(
    inputs: WorkspaceHygieneReportInputs<'_>,
) -> WorkspaceHygieneReport {
    let secret_scan_budget = WorkspaceHygieneSecretScanBudget::default();
    let (secret_evidence, secret_scan) = workspace_hygiene_secret_evidence_with_budget(
        inputs.workspace_path,
        &inputs.snapshot,
        secret_scan_budget,
    );
    let classifications_all = classify_workspace_with_config(
        &inputs.snapshot,
        &secret_evidence,
        inputs.classifier_config,
    );
    let beads_state = classify_beads_state(BeadsHygieneInputs {
        snapshot: &inputs.snapshot,
        jsonl_content: inputs.jsonl_content,
        self_agent_name: inputs.self_agent_name,
        metadata_signal: inputs.beads_metadata_signal,
        reservations: inputs.beads_reservations,
    });
    let coordination = overlay_coordination_state(
        &classifications_all,
        inputs.agent_mail_input,
        inputs.now,
        inputs.self_agent_name,
    );

    let bucket_counts = workspace_hygiene_bucket_counts(&classifications_all);
    let kind_counts = workspace_hygiene_kind_counts(&classifications_all);
    let staging_groups_all = workspace_hygiene_staging_groups(&classifications_all, &coordination);
    let do_not_commit_all =
        workspace_hygiene_paths_for_bucket(&classifications_all, Bucket::DoNotCommit);
    let needs_human_review_all =
        workspace_hygiene_paths_for_bucket(&classifications_all, Bucket::NeedsHumanReview);

    let (classifications, omitted_path_classifications) =
        workspace_hygiene_truncate_classifications(&classifications_all);
    let (staging_groups, staging_group_truncations) =
        workspace_hygiene_truncate_staging_groups(staging_groups_all);
    let (do_not_commit, omitted_do_not_commit) =
        workspace_hygiene_truncate_path_list(do_not_commit_all);
    let (needs_human_review, omitted_needs_human_review) =
        workspace_hygiene_truncate_path_list(needs_human_review_all);
    let output_truncation = workspace_hygiene_output_truncation(
        &classifications_all,
        omitted_path_classifications,
        omitted_do_not_commit,
        omitted_needs_human_review,
        staging_group_truncations,
    );
    let secret_scan = workspace_hygiene_secret_scan_report(secret_scan_budget, secret_scan);
    let mut degraded_codes = workspace_hygiene_degraded_codes(&beads_state, &coordination);
    if secret_scan.skipped_content_scan_count > 0 {
        degraded_codes.push(WORKSPACE_HYGIENE_SECRET_SCAN_SKIPPED_CODE);
        degraded_codes.sort_unstable();
        degraded_codes.dedup();
    }
    if output_truncation.truncated
        && !degraded_codes.contains(&WORKSPACE_HYGIENE_OUTPUT_TRUNCATED_CODE)
    {
        degraded_codes.push(WORKSPACE_HYGIENE_OUTPUT_TRUNCATED_CODE);
        degraded_codes.sort_unstable();
        degraded_codes.dedup();
    }
    let next_actions = workspace_hygiene_next_actions(
        &staging_groups,
        &do_not_commit,
        &needs_human_review,
        &degraded_codes,
    );

    WorkspaceHygieneReport {
        schema: WORKSPACE_HYGIENE_SCHEMA_V1,
        command: "workspace hygiene",
        read_only: true,
        workspace_path: inputs.workspace_path.display().to_string(),
        git_summary: WorkspaceHygieneGitSummary {
            repository_root: inputs.snapshot.repository_root.clone(),
            dirty_path_count: classifications_all.len(),
            bucket_counts: bucket_counts.clone(),
            kind_counts: kind_counts.clone(),
        },
        repository_root: inputs.snapshot.repository_root,
        dirty_path_count: classifications_all.len(),
        bucket_counts,
        kind_counts,
        staging_groups,
        classifications,
        do_not_commit,
        needs_human_review,
        output_truncation,
        secret_scan,
        beads_state,
        coordination,
        degraded_codes,
        next_actions,
    }
}

fn load_agent_mail_coordination_input(path: Option<&Path>) -> AgentMailCoordinationInput {
    let Some(path) = path else {
        return AgentMailCoordinationInput::Unavailable;
    };
    let Ok(contents) = read_agent_mail_snapshot(path) else {
        return AgentMailCoordinationInput::Unavailable;
    };
    if agent_mail_snapshot_status_is_timeout(&contents) {
        return AgentMailCoordinationInput::TimedOut;
    }
    let Ok(snapshot) = parse_agent_mail_snapshot_json(&contents) else {
        return AgentMailCoordinationInput::Unavailable;
    };
    if snapshot.degraded.iter().any(|entry| {
        entry.code == "agent_mail_unavailable" && snapshot.file_reservations.is_empty()
    }) {
        return AgentMailCoordinationInput::Unavailable;
    }
    let reservations = snapshot
        .file_reservations
        .into_iter()
        .map(
            |reservation| crate::core::hygiene_coordination::AgentMailReservation {
                path_pattern: reservation.path_pattern,
                holder_agent: reservation.holder,
                exclusive: reservation.exclusive,
                expires_at: reservation.expires_at,
                reservation_id: None,
                bead_id: None,
                thread_id: None,
            },
        )
        .collect();
    let active_agents = parse_active_agents_from_snapshot(&contents);
    AgentMailCoordinationInput::Available {
        reservations,
        active_agents,
    }
}

fn read_agent_mail_snapshot(path: &Path) -> io::Result<String> {
    if let Some(symlink) = first_existing_symlink_component(path)? {
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
    fs::read_to_string(path)
}

fn first_existing_symlink_component(path: &Path) -> io::Result<Option<PathBuf>> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                current.push(component.as_os_str());
                continue;
            }
            Component::CurDir => continue,
            Component::ParentDir | Component::Normal(_) => {
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

fn agent_mail_snapshot_status_is_timeout(contents: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(contents) else {
        return false;
    };
    value
        .get("status")
        .or_else(|| value.get("agentMailStatus"))
        .or_else(|| value.get("agent_mail_status"))
        .and_then(Value::as_str)
        .is_some_and(|status| matches!(status, "timed_out" | "timeout" | "timedOut"))
}

fn parse_active_agents_from_snapshot(
    contents: &str,
) -> Vec<crate::core::hygiene_coordination::ActiveAgent> {
    let Ok(value) = serde_json::from_str::<Value>(contents) else {
        return Vec::new();
    };
    let Some(items) = value
        .get("active_agents")
        .or_else(|| value.get("activeAgents"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut agents = items
        .iter()
        .filter_map(|item| {
            let name = item
                .get("name")
                .or_else(|| item.get("agent_name"))
                .or_else(|| item.get("agentName"))
                .and_then(Value::as_str)?
                .trim();
            if name.is_empty() {
                return None;
            }
            let last_active_at = item
                .get("last_active_at")
                .or_else(|| item.get("lastActiveAt"))
                .or_else(|| item.get("last_active_ts"))
                .or_else(|| item.get("lastActiveTs"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            Some(crate::core::hygiene_coordination::ActiveAgent {
                name: name.to_owned(),
                last_active_at,
            })
        })
        .collect::<Vec<_>>();
    agents.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.last_active_at.cmp(&right.last_active_at))
    });
    agents.dedup();
    agents
}

fn workspace_hygiene_classifier_config() -> Result<HygieneClassifierConfig, DomainError> {
    let generated = read_env_var(EnvVar::WorkspaceHygieneGeneratedPatterns);
    let scratch = read_env_var(EnvVar::WorkspaceHygieneScratchPatterns);
    let local_machine = read_env_var(EnvVar::WorkspaceHygieneLocalMachinePatterns);
    let always_review = read_env_var(EnvVar::WorkspaceHygieneAlwaysReviewPatterns);
    HygieneClassifierConfig::from_raw_pattern_values(
        generated.as_deref(),
        scratch.as_deref(),
        local_machine.as_deref(),
        always_review.as_deref(),
    )
    .map_err(|error| DomainError::Configuration {
        message: format!("invalid workspace hygiene configuration: {error}"),
        repair: Some(format!(
            "Use matcher syntax like `{}=prefix:target/` or clear the invalid variable.",
            EnvVar::WorkspaceHygieneGeneratedPatterns.name()
        )),
    })
}

fn read_bounded_file(path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    let mut buffer = Vec::new();
    file.by_ref()
        .take(u64::try_from(max_bytes).unwrap_or(u64::MAX))
        .read_to_end(&mut buffer)?;
    Ok(buffer)
}

fn workspace_hygiene_secret_evidence_with_budget(
    workspace_path: &Path,
    snapshot: &WorkspaceGitSnapshot,
    budget: WorkspaceHygieneSecretScanBudget,
) -> (SecretEvidenceLookup, WorkspaceHygieneSecretScanSummary) {
    let mut lookup = SecretEvidenceLookup::default();
    let mut summary = WorkspaceHygieneSecretScanSummary::default();

    for entry in &snapshot.entries {
        let Some(metadata) = entry.metadata.as_ref() else {
            continue;
        };
        if !metadata.exists || metadata.file_type != "file" {
            continue;
        }
        let Some(size_bytes) = metadata.size_bytes else {
            summary.skipped_content_scan_count += 1;
            continue;
        };
        let Ok(size_bytes) = usize::try_from(size_bytes) else {
            summary.skipped_content_scan_count += 1;
            continue;
        };
        if metadata.large_file || size_bytes > budget.max_file_bytes {
            summary.skipped_content_scan_count += 1;
            continue;
        }
        if summary.scanned_file_count >= budget.max_files {
            summary.skipped_content_scan_count += 1;
            continue;
        }
        if summary
            .scanned_byte_count
            .checked_add(size_bytes)
            .is_none_or(|total| total > budget.max_total_bytes)
        {
            summary.skipped_content_scan_count += 1;
            continue;
        }
        let Some(full_path) = workspace_hygiene_safe_content_path(workspace_path, &entry.path)
        else {
            summary.skipped_content_scan_count += 1;
            continue;
        };
        let bytes = match read_bounded_file(&full_path, budget.max_file_bytes.saturating_add(1)) {
            Ok(bytes) if bytes.len() <= budget.max_file_bytes => bytes,
            Ok(_) | Err(_) => {
                summary.skipped_content_scan_count += 1;
                continue;
            }
        };
        if summary
            .scanned_byte_count
            .checked_add(bytes.len())
            .is_none_or(|total| total > budget.max_total_bytes)
        {
            summary.skipped_content_scan_count += 1;
            continue;
        }
        summary.scanned_file_count += 1;
        summary.scanned_byte_count += bytes.len();
        let report =
            workspace_secret_risk_evidence(&entry.path, Some(&bytes), budget.max_file_bytes);
        if report.skipped_content_scan {
            summary.skipped_content_scan_count += 1;
        }
        if report.secret_risk {
            lookup.insert(entry.path.clone(), report);
        }
    }

    (lookup, summary)
}

fn workspace_hygiene_safe_content_path(
    workspace_path: &Path,
    relative_path: &str,
) -> Option<PathBuf> {
    let path = Path::new(relative_path);
    if path.components().any(|component| {
        matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir
        )
    }) {
        return None;
    }
    let full_path = workspace_path.join(path);
    match first_existing_symlink_component(&full_path) {
        Ok(None) => Some(full_path),
        Ok(Some(_)) | Err(_) => None,
    }
}

fn workspace_hygiene_secret_scan_report(
    budget: WorkspaceHygieneSecretScanBudget,
    summary: WorkspaceHygieneSecretScanSummary,
) -> WorkspaceHygieneSecretScanReport {
    WorkspaceHygieneSecretScanReport {
        read_only: true,
        scanned_file_count: summary.scanned_file_count,
        scanned_byte_count: summary.scanned_byte_count,
        skipped_content_scan_count: summary.skipped_content_scan_count,
        max_files: budget.max_files,
        max_file_bytes: budget.max_file_bytes,
        max_total_bytes: budget.max_total_bytes,
    }
}

fn detect_beads_metadata_signal(workspace_path: &Path) -> BeadsMetadataSignal {
    let beads_dir = workspace_path.join(".beads");
    let jsonl_modified = modified_at(&beads_dir.join("issues.jsonl"));
    let marker_modified =
        newest_modified_at(&[beads_dir.join("beads.db"), beads_dir.join("last-touched")]);

    match (marker_modified, jsonl_modified) {
        (Some(_), None) => BeadsMetadataSignal::DbDirtyPendingFlush,
        (Some(marker), Some(jsonl)) if marker > jsonl => BeadsMetadataSignal::DbDirtyPendingFlush,
        (Some(marker), Some(jsonl)) if jsonl > marker => {
            BeadsMetadataSignal::ExternalChangesPendingImport
        }
        _ => BeadsMetadataSignal::Unknown,
    }
}

fn newest_modified_at(paths: &[PathBuf]) -> Option<SystemTime> {
    paths.iter().filter_map(|path| modified_at(path)).max()
}

fn modified_at(path: &Path) -> Option<SystemTime> {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

fn workspace_hygiene_bucket_counts(rows: &[ClassificationRow]) -> Vec<WorkspaceHygieneCount> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for row in rows {
        *counts.entry(row.bucket.as_str().to_owned()).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(name, count)| WorkspaceHygieneCount { name, count })
        .collect()
}

fn workspace_hygiene_kind_counts(rows: &[ClassificationRow]) -> Vec<WorkspaceHygieneCount> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for row in rows {
        *counts.entry(row.kind.as_str().to_owned()).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(name, count)| WorkspaceHygieneCount { name, count })
        .collect()
}

fn workspace_hygiene_staging_groups(
    rows: &[ClassificationRow],
    coordination: &HygieneCoordinationOverlay,
) -> Vec<WorkspaceHygieneStagingGroup> {
    let mut groups: BTreeMap<String, WorkspaceHygieneStagingGroupBuilder> = BTreeMap::new();
    let blocked_paths = coordination
        .blocked_by_coordination
        .iter()
        .map(|blocked| blocked.path.as_str())
        .collect::<BTreeSet<_>>();
    for row in rows {
        if row.bucket != Bucket::StageCandidate {
            continue;
        }
        if blocked_paths.contains(row.path.as_str()) {
            continue;
        }
        let group = workspace_hygiene_stage_group_name(row);
        groups.entry(group).or_default().push(row);
    }
    groups
        .into_iter()
        .map(|(name, builder)| builder.into_group(name))
        .collect()
}

fn workspace_hygiene_stage_group_name(row: &ClassificationRow) -> String {
    if row.path.starts_with("tests/fixtures/golden/")
        || row.path.starts_with("tests/fixtures/goldens/")
        || row.path.contains("/golden/")
        || row.path.contains("/goldens/")
    {
        return "goldens".to_owned();
    }
    match row.kind {
        Kind::Source => row
            .suggested_group
            .clone()
            .unwrap_or_else(|| "source".to_owned()),
        Kind::Test => row
            .suggested_group
            .clone()
            .unwrap_or_else(|| "tests".to_owned()),
        Kind::Docs => "docs".to_owned(),
        Kind::BeadsMetadata => "beads_metadata".to_owned(),
        Kind::Generated => "generated".to_owned(),
        Kind::Scratch => "scratch".to_owned(),
        Kind::LocalMachine => "local_machine".to_owned(),
        Kind::SecretRisk => "secret_risk".to_owned(),
        Kind::Binary => "binary".to_owned(),
        Kind::Unknown => "human_review".to_owned(),
    }
}

#[derive(Default)]
struct WorkspaceHygieneStagingGroupBuilder {
    paths: BTreeSet<String>,
    kinds: BTreeSet<String>,
    reasons: BTreeSet<String>,
}

impl WorkspaceHygieneStagingGroupBuilder {
    fn push(&mut self, row: &ClassificationRow) {
        self.paths.insert(row.path.clone());
        self.kinds.insert(row.kind.as_str().to_owned());
        self.reasons
            .extend(row.reasons.iter().map(|reason| (*reason).to_owned()));
    }

    fn into_group(self, name: String) -> WorkspaceHygieneStagingGroup {
        let paths = self.paths.into_iter().collect::<Vec<_>>();
        let path_count = paths.len();
        WorkspaceHygieneStagingGroup {
            name,
            paths,
            path_count,
            paths_truncated: false,
            omitted_path_count: 0,
            kinds: self.kinds.into_iter().collect(),
            reasons: self.reasons.into_iter().collect(),
            recommendation: "review_and_stage_as_one_logical_commit",
            read_only: true,
        }
    }
}

fn workspace_hygiene_paths_for_bucket(rows: &[ClassificationRow], bucket: Bucket) -> Vec<String> {
    let mut paths = BTreeSet::new();
    for row in rows {
        if row.bucket == bucket {
            paths.insert(row.path.clone());
        }
    }
    paths.into_iter().collect()
}

fn workspace_hygiene_truncate_classifications(
    rows: &[ClassificationRow],
) -> (Vec<ClassificationRow>, usize) {
    let omitted = rows
        .len()
        .saturating_sub(WORKSPACE_HYGIENE_MAX_PATH_CLASSIFICATIONS);
    let mut visible = rows
        .iter()
        .take(WORKSPACE_HYGIENE_MAX_PATH_CLASSIFICATIONS)
        .cloned()
        .collect::<Vec<_>>();
    visible.shrink_to_fit();
    (visible, omitted)
}

fn workspace_hygiene_truncate_path_list(paths: Vec<String>) -> (Vec<String>, usize) {
    let original_len = paths.len();
    let omitted = original_len.saturating_sub(WORKSPACE_HYGIENE_MAX_PATHS_PER_LIST);
    if omitted == 0 {
        return (paths, 0);
    }
    (
        paths
            .into_iter()
            .take(WORKSPACE_HYGIENE_MAX_PATHS_PER_LIST)
            .collect(),
        omitted,
    )
}

fn workspace_hygiene_truncate_staging_groups(
    mut groups: Vec<WorkspaceHygieneStagingGroup>,
) -> (
    Vec<WorkspaceHygieneStagingGroup>,
    Vec<WorkspaceHygieneStagingGroupTruncation>,
) {
    let mut truncations = Vec::new();
    for group in &mut groups {
        let original_len = group.paths.len();
        let omitted = original_len.saturating_sub(WORKSPACE_HYGIENE_MAX_PATHS_PER_STAGING_GROUP);
        if omitted == 0 {
            continue;
        }
        group
            .paths
            .truncate(WORKSPACE_HYGIENE_MAX_PATHS_PER_STAGING_GROUP);
        group.paths_truncated = true;
        group.omitted_path_count = omitted;
        group.path_count = original_len;
        truncations.push(WorkspaceHygieneStagingGroupTruncation {
            name: group.name.clone(),
            omitted_path_count: omitted,
        });
    }
    (groups, truncations)
}

fn workspace_hygiene_output_truncation(
    full_rows: &[ClassificationRow],
    omitted_path_classifications: usize,
    omitted_do_not_commit: usize,
    omitted_needs_human_review: usize,
    staging_groups: Vec<WorkspaceHygieneStagingGroupTruncation>,
) -> WorkspaceHygieneOutputTruncation {
    let truncated = omitted_path_classifications > 0
        || omitted_do_not_commit > 0
        || omitted_needs_human_review > 0
        || staging_groups
            .iter()
            .any(|group| group.omitted_path_count > 0);
    let omitted_rows = if omitted_path_classifications == 0 {
        &[][..]
    } else {
        &full_rows[WORKSPACE_HYGIENE_MAX_PATH_CLASSIFICATIONS..]
    };
    WorkspaceHygieneOutputTruncation {
        truncated,
        max_path_classifications: WORKSPACE_HYGIENE_MAX_PATH_CLASSIFICATIONS,
        max_paths_per_list: WORKSPACE_HYGIENE_MAX_PATHS_PER_LIST,
        max_paths_per_staging_group: WORKSPACE_HYGIENE_MAX_PATHS_PER_STAGING_GROUP,
        omitted_path_classifications,
        omitted_do_not_commit,
        omitted_needs_human_review,
        omitted_by_bucket: workspace_hygiene_bucket_counts(omitted_rows),
        omitted_by_kind: workspace_hygiene_kind_counts(omitted_rows),
        staging_groups,
    }
}

fn workspace_hygiene_next_actions(
    staging_groups: &[WorkspaceHygieneStagingGroup],
    do_not_commit: &[String],
    needs_human_review: &[String],
    degraded_codes: &[&'static str],
) -> Vec<String> {
    let mut actions = Vec::new();
    if !staging_groups.is_empty() {
        actions.push(
            "Review stagingRecommendations and commit one logical group at a time.".to_string(),
        );
    }
    if !needs_human_review.is_empty() {
        actions.push("Inspect needsHumanReview paths before staging.".to_string());
    }
    if !do_not_commit.is_empty() {
        actions.push(
            "Leave doNotCommit paths unstaged unless a human explicitly overrides.".to_string(),
        );
    }
    if degraded_codes
        .iter()
        .any(|code| *code == WORKSPACE_HYGIENE_AGENT_MAIL_UNAVAILABLE_CODE)
    {
        actions.push(
            "Refresh Agent Mail reservations before committing coordination-sensitive paths."
                .to_string(),
        );
    }
    if degraded_codes
        .iter()
        .any(|code| *code == WORKSPACE_HYGIENE_OUTPUT_TRUNCATED_CODE)
    {
        actions.push(
            "Narrow the dirty path set or inspect JSON outputTruncation before staging large changes."
                .to_string(),
        );
    }
    actions
}

fn workspace_hygiene_degraded_codes(
    beads_state: &BeadsHygieneState,
    coordination: &HygieneCoordinationOverlay,
) -> Vec<&'static str> {
    let mut codes: BTreeSet<&'static str> = BTreeSet::new();
    codes.insert(WORKSPACE_HYGIENE_PARTIAL_METADATA_CODE);
    codes.extend(beads_state.degraded_codes.iter().copied());
    codes.extend(coordination.degraded_codes.iter().copied());
    codes.into_iter().collect()
}

fn workspace_git_error(error: crate::core::swarm_brief::SwarmBriefCommandError) -> DomainError {
    match error {
        crate::core::swarm_brief::SwarmBriefCommandError::Unavailable(message)
        | crate::core::swarm_brief::SwarmBriefCommandError::InvalidUtf8(message) => {
            DomainError::Configuration {
                message,
                repair: Some("Run `ee workspace hygiene` inside a git checkout.".to_string()),
            }
        }
        crate::core::swarm_brief::SwarmBriefCommandError::Failed { status, stderr } => {
            DomainError::Configuration {
                message: format!(
                    "read-only git status collection failed with status {}: {}",
                    status
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "terminated_by_signal".to_string()),
                    stderr
                ),
                repair: Some(
                    "Run `git status --porcelain=v2 --branch` to inspect the checkout.".to_string(),
                ),
            }
        }
        crate::core::swarm_brief::SwarmBriefCommandError::TimedOut { timeout_ms } => {
            DomainError::Configuration {
                message: format!("read-only git status collection timed out after {timeout_ms} ms"),
                repair: Some("Retry after any long-running git operation finishes.".to_string()),
            }
        }
    }
}

pub fn alias_workspace(
    options: &WorkspaceAliasOptions,
) -> Result<WorkspaceAliasReport, DomainError> {
    if options.clear && options.alias.is_some() {
        return Err(DomainError::Usage {
            message: "--clear cannot be combined with --as or a positional alias".to_string(),
            repair: Some(
                "Use either `ee workspace alias --clear` or `ee workspace alias --as <name>`."
                    .to_string(),
            ),
        });
    }

    let normalized_alias = options
        .alias
        .as_deref()
        .map(normalize_alias)
        .transpose()
        .map_err(alias_usage_error)?;

    if !options.clear && normalized_alias.is_none() {
        return Err(DomainError::Usage {
            message: "workspace alias requires --as <name> or --clear".to_string(),
            repair: Some("ee workspace alias --pick <path-or-id> --as <name>".to_string()),
        });
    }

    let registry_path = registry_database_path_override(options.registry_path.as_deref());
    let target = resolve_alias_target(&registry_path, options)?;
    let previous_alias = target.name.clone();

    if let Some(alias) = normalized_alias.as_deref() {
        ensure_alias_available(&registry_path, alias, &target.id)?;
    }

    if options.dry_run {
        return Ok(WorkspaceAliasReport {
            schema: WORKSPACE_ALIAS_SCHEMA_V1,
            command: "workspace alias",
            status: if options.clear {
                "would_clear"
            } else {
                "would_set"
            },
            registry_path: registry_path.display().to_string(),
            workspace_id: target.id,
            workspace_path: target.path,
            alias: normalized_alias,
            previous_alias,
            scope_kind: target.scope_kind,
            repository_root: target.repository_root,
            repository_fingerprint: target.repository_fingerprint,
            subproject_path: target.subproject_path,
            dry_run: true,
            persisted: false,
            audit_id: None,
        });
    }

    let conn = open_registry_write(&registry_path)?;
    let action = if options.clear {
        WORKSPACE_ALIAS_CLEAR_ACTION
    } else {
        WORKSPACE_ALIAS_SET_ACTION
    };
    let audit_id = generate_audit_id();
    let target_id = target.id.clone();
    let target_path = target.path.clone();
    let target_scope_kind = target.scope_kind.clone();
    let target_repository_root = target.repository_root.clone();
    let target_repository_fingerprint = target.repository_fingerprint.clone();
    let target_subproject_path = target.subproject_path.clone();
    let alias_for_write = normalized_alias.clone();
    let details = serde_json::json!({
        "schema": WORKSPACE_ALIAS_SCHEMA_V1,
        "workspaceId": target_id,
        "workspacePath": target_path,
        "previousAlias": previous_alias,
        "alias": alias_for_write,
        "scopeKind": target_scope_kind,
        "repositoryRoot": target_repository_root,
        "repositoryFingerprint": target_repository_fingerprint,
        "subprojectPath": target_subproject_path,
        "dryRun": false
    })
    .to_string();

    conn.with_transaction(|| {
        upsert_workspace_row(&conn, &target)?;
        conn.update_workspace_name(&target.id, alias_for_write.as_deref())?;
        conn.insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(target.id.clone()),
                actor: Some("ee-cli".to_string()),
                action: action.to_string(),
                target_type: Some("workspace".to_string()),
                target_id: Some(target.id.clone()),
                details: Some(details),
            },
        )?;
        Ok(())
    })
    .map_err(|error| storage_error("failed to persist workspace alias", error))?;

    Ok(WorkspaceAliasReport {
        schema: WORKSPACE_ALIAS_SCHEMA_V1,
        command: "workspace alias",
        status: if options.clear { "cleared" } else { "set" },
        registry_path: registry_path.display().to_string(),
        workspace_id: target.id,
        workspace_path: target.path,
        alias: normalized_alias,
        previous_alias,
        scope_kind: target.scope_kind,
        repository_root: target.repository_root,
        repository_fingerprint: target.repository_fingerprint,
        subproject_path: target.subproject_path,
        dry_run: false,
        persisted: true,
        audit_id: Some(audit_id),
    })
}

fn resolve_path_report(
    registry_path: &Path,
    target: Option<&str>,
    workspace_path: Option<PathBuf>,
) -> Result<WorkspaceResolveReport, DomainError> {
    let request = WorkspaceResolutionRequest::from_process(
        workspace_path,
        WorkspaceResolutionMode::AllowUninitialized,
    )
    .map_err(|error| DomainError::Configuration {
        message: error.to_string(),
        repair: Some("Run from a readable directory or pass --workspace <path>.".to_string()),
    })?;
    let resolution = resolve_workspace(&request).map_err(|error| DomainError::Configuration {
        message: error.to_string(),
        repair: Some("ee init --workspace .".to_string()),
    })?;
    let diagnostics = diagnose_workspace_resolution(&request, &resolution)
        .into_iter()
        .map(WorkspaceDiagnosticEntry::from)
        .collect();
    let alias = find_workspace_alias_read_only(registry_path, &resolution.location.root)?;
    let workspace_id = stable_workspace_id(&resolution.canonical_root);

    Ok(WorkspaceResolveReport {
        schema: WORKSPACE_RESOLVE_SCHEMA_V1,
        command: "workspace resolve",
        source: resolution.source.as_str().to_string(),
        target: target.map(str::to_string),
        workspace_id,
        root: resolution.location.root.display().to_string(),
        canonical_root: resolution.canonical_root.display().to_string(),
        marker_present: resolution.marker_present,
        alias,
        scope_kind: resolution.scope.kind.as_str().to_string(),
        repository_root: resolution
            .scope
            .repository_root
            .as_ref()
            .map(|path| path.display().to_string()),
        repository_fingerprint: resolution.scope.repository_fingerprint.clone(),
        subproject_path: resolution
            .scope
            .subproject_path
            .as_ref()
            .map(|path| path.display().to_string()),
        registry_path: registry_path.display().to_string(),
        diagnostics,
    })
}

fn resolve_alias_row_report(
    registry_path: &Path,
    target: &str,
    row: StoredWorkspace,
) -> WorkspaceResolveReport {
    let root = PathBuf::from(&row.path);
    let canonical_root = canonical_or_lexical(&root);
    WorkspaceResolveReport {
        schema: WORKSPACE_RESOLVE_SCHEMA_V1,
        command: "workspace resolve",
        source: "alias".to_string(),
        target: Some(target.to_string()),
        workspace_id: row.id,
        root: root.display().to_string(),
        canonical_root: canonical_root.display().to_string(),
        marker_present: root.join(WORKSPACE_MARKER).is_dir(),
        alias: row.name,
        scope_kind: row.scope_kind,
        repository_root: row.repository_root,
        repository_fingerprint: row.repository_fingerprint,
        subproject_path: row.subproject_path,
        registry_path: registry_path.display().to_string(),
        diagnostics: Vec::new(),
    }
}

fn resolve_alias_target(
    registry_path: &Path,
    options: &WorkspaceAliasOptions,
) -> Result<StoredWorkspace, DomainError> {
    if let Some(pick) = options.pick.as_deref() {
        if pick.starts_with("wsp_") {
            if let Some(row) = find_workspace_id_read_only(registry_path, pick)? {
                return Ok(row);
            }
            return Err(DomainError::NotFound {
                resource: "workspace".to_string(),
                id: pick.to_string(),
                repair: Some("ee workspace list --json".to_string()),
            });
        }
        return workspace_row_for_path(pick);
    }

    let selected = options
        .workspace_path
        .clone()
        .unwrap_or_else(|| PathBuf::from("."));
    workspace_row_for_path(&selected.display().to_string())
}

fn workspace_row_for_path(raw: &str) -> Result<StoredWorkspace, DomainError> {
    let root = lexical_absolute(
        &env::current_dir().map_err(|error| DomainError::Configuration {
            message: format!("failed to read current directory: {error}"),
            repair: Some("Run from a readable directory or pass --workspace <path>.".to_string()),
        })?,
        Path::new(raw),
    );
    if !root.exists() {
        return Err(DomainError::Configuration {
            message: format!("workspace path does not exist: {}", root.display()),
            repair: Some("Create the directory or pass an existing --workspace path.".to_string()),
        });
    }
    let marker = root.join(WORKSPACE_MARKER);
    if !marker.is_dir() {
        return Err(DomainError::Configuration {
            message: format!("workspace is not initialized: {}", root.display()),
            repair: Some(format!("ee init --workspace {}", root.display())),
        });
    }
    let canonical = canonical_or_lexical(&root);
    let path = canonical.display().to_string();
    let scope = workspace_scope_fields(&derive_workspace_scope(&canonical));
    Ok(StoredWorkspace {
        id: stable_workspace_id(&canonical),
        path,
        name: None,
        scope_kind: scope.scope_kind,
        repository_root: scope.repository_root,
        repository_fingerprint: scope.repository_fingerprint,
        subproject_path: scope.subproject_path,
        created_at: String::new(),
        updated_at: String::new(),
    })
}

fn workspace_scope_fields(scope: &WorkspaceScope) -> WorkspaceScopeFields {
    WorkspaceScopeFields {
        scope_kind: scope.kind.as_str().to_string(),
        repository_root: scope
            .repository_root
            .as_ref()
            .map(|path| path.display().to_string()),
        repository_fingerprint: scope.repository_fingerprint.clone(),
        subproject_path: scope
            .subproject_path
            .as_ref()
            .map(|path| path.display().to_string()),
    }
}

fn upsert_workspace_row(conn: &DbConnection, target: &StoredWorkspace) -> crate::db::Result<()> {
    let scope = WorkspaceScopeFields {
        scope_kind: target.scope_kind.clone(),
        repository_root: target.repository_root.clone(),
        repository_fingerprint: target.repository_fingerprint.clone(),
        subproject_path: target.subproject_path.clone(),
    };
    conn.upsert_workspace_with_scope(
        &target.id,
        &CreateWorkspaceInput {
            path: target.path.clone(),
            name: target.name.clone(),
        },
        &scope,
    )
}

fn ensure_alias_available(
    registry_path: &Path,
    alias: &str,
    workspace_id: &str,
) -> Result<(), DomainError> {
    if let Some(existing) = find_alias_read_only(registry_path, alias)? {
        if existing.id != workspace_id {
            return Err(DomainError::Usage {
                message: format!(
                    "workspace alias `{alias}` already points to {}",
                    existing.path
                ),
                repair: Some(
                    "Choose a different alias or clear the existing workspace alias first."
                        .to_string(),
                ),
            });
        }
    }
    Ok(())
}

fn find_workspace_alias_read_only(
    registry_path: &Path,
    workspace_path: &Path,
) -> Result<Option<String>, DomainError> {
    if !registry_file_exists(registry_path)? {
        return Ok(None);
    }
    let canonical = canonical_or_lexical(workspace_path);
    let path = canonical.display().to_string();
    let conn = open_registry_read_only(registry_path)?;
    Ok(conn
        .get_workspace_by_path(&path)
        .map_err(|error| storage_error("failed to resolve workspace alias", error))?
        .and_then(|row| row.name))
}

fn find_alias_read_only(
    registry_path: &Path,
    alias: &str,
) -> Result<Option<StoredWorkspace>, DomainError> {
    if !registry_file_exists(registry_path)? {
        return Ok(None);
    }
    let conn = open_registry_read_only(registry_path)?;
    let rows = conn
        .list_workspaces()
        .map_err(|error| storage_error("failed to query workspace aliases", error))?;
    Ok(rows
        .into_iter()
        .find(|row| row.name.as_deref() == Some(alias)))
}

fn find_workspace_id_read_only(
    registry_path: &Path,
    workspace_id: &str,
) -> Result<Option<StoredWorkspace>, DomainError> {
    if !registry_file_exists(registry_path)? {
        return Ok(None);
    }
    let conn = open_registry_read_only(registry_path)?;
    conn.get_workspace(workspace_id)
        .map_err(|error| storage_error("failed to query workspace registry", error))
}

fn open_registry_read_only(registry_path: &Path) -> Result<DbConnection, DomainError> {
    if !registry_file_exists(registry_path)? {
        return Err(DomainError::Storage {
            message: format!("workspace registry not found: {}", registry_path.display()),
            repair: Some(
                "Run `ee workspace alias --as <name>` to create the registry.".to_string(),
            ),
        });
    }
    let conn = DbConnection::open(DatabaseConfig::file(registry_path))
        .map_err(|error| storage_error("failed to open workspace registry", error))?;
    if conn
        .needs_migration()
        .map_err(|error| storage_error("failed to inspect workspace registry schema", error))?
    {
        return Err(DomainError::MigrationRequired {
            message: format!(
                "workspace registry requires migration: {}",
                registry_path.display()
            ),
            repair: Some("Run a mutating workspace registry command such as `ee workspace alias --as <name>`.".to_string()),
        });
    }
    Ok(conn)
}

fn open_registry_write(registry_path: &Path) -> Result<DbConnection, DomainError> {
    ensure_registry_path_has_no_symlink_components(registry_path)?;
    if let Some(parent) = registry_path.parent() {
        fs::create_dir_all(parent).map_err(|error| DomainError::Storage {
            message: format!(
                "failed to create workspace registry directory {}: {error}",
                parent.display()
            ),
            repair: Some(
                "Check permissions or set EE_WORKSPACE_REGISTRY to a writable path.".to_string(),
            ),
        })?;
    }
    ensure_registry_path_has_no_symlink_components(registry_path)?;
    ensure_existing_registry_path_is_regular_file(registry_path)?;
    let conn = DbConnection::open(DatabaseConfig::file(registry_path))
        .map_err(|error| storage_error("failed to open workspace registry", error))?;
    conn.migrate()
        .map_err(|error| storage_error("failed to migrate workspace registry", error))?;
    Ok(conn)
}

fn normalize_alias(raw: &str) -> Result<String, String> {
    let alias = raw.trim();
    if alias.is_empty() {
        return Err("workspace alias cannot be empty".to_string());
    }
    if alias == "." || alias == ".." {
        return Err("workspace alias cannot be `.` or `..`".to_string());
    }
    if alias.starts_with('.') {
        return Err("workspace alias cannot start with `.`".to_string());
    }
    if alias.len() > 64 {
        return Err("workspace alias cannot exceed 64 bytes".to_string());
    }
    if alias
        .chars()
        .any(|ch| !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.'))
    {
        return Err(
            "workspace alias may only contain ASCII letters, numbers, dots, dashes, and underscores"
                .to_string(),
        );
    }
    Ok(alias.to_string())
}

fn ensure_registry_path_has_no_symlink_components(path: &Path) -> Result<(), DomainError> {
    match registry_path_has_symlink_component(path) {
        Ok(false) => Ok(()),
        Ok(true) => Err(DomainError::Storage {
            message: format!(
                "refusing workspace registry path with symlink component: {}",
                path.display()
            ),
            repair: Some(
                "Set EE_WORKSPACE_REGISTRY to a non-symlinked path under a trusted directory."
                    .to_string(),
            ),
        }),
        Err(error) => Err(DomainError::Storage {
            message: format!(
                "failed to inspect workspace registry path {}: {error}",
                path.display()
            ),
            repair: Some(
                "Check permissions or set EE_WORKSPACE_REGISTRY to a readable path.".to_string(),
            ),
        }),
    }
}

fn registry_file_exists(path: &Path) -> Result<bool, DomainError> {
    ensure_registry_path_has_no_symlink_components(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(non_regular_registry_path_error(path)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(DomainError::Storage {
            message: format!(
                "failed to inspect workspace registry path {}: {error}",
                path.display()
            ),
            repair: Some(
                "Check permissions or set EE_WORKSPACE_REGISTRY to a readable path.".to_string(),
            ),
        }),
    }
}

fn ensure_existing_registry_path_is_regular_file(path: &Path) -> Result<(), DomainError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(non_regular_registry_path_error(path)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(DomainError::Storage {
            message: format!(
                "failed to inspect workspace registry path {}: {error}",
                path.display()
            ),
            repair: Some(
                "Check permissions or set EE_WORKSPACE_REGISTRY to a readable path.".to_string(),
            ),
        }),
    }
}

fn non_regular_registry_path_error(path: &Path) -> DomainError {
    DomainError::Storage {
        message: format!(
            "workspace registry path is not a regular file: {}",
            path.display()
        ),
        repair: Some(
            "Set EE_WORKSPACE_REGISTRY to a regular database file path or move the directory aside."
                .to_string(),
        ),
    }
}

fn registry_path_has_symlink_component(path: &Path) -> io::Result<bool> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(true),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(false);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

fn alias_usage_error(message: String) -> DomainError {
    DomainError::Usage {
        message,
        repair: Some("Use an alias like `project-main`, `client.api`, or `repo_1`.".to_string()),
    }
}

fn storage_error(context: &str, error: crate::db::DbError) -> DomainError {
    DomainError::Storage {
        message: format!("{context}: {error}"),
        repair: Some(
            "Run `ee doctor --json` and verify the workspace registry database.".to_string(),
        ),
    }
}

fn looks_like_path(path: &Path) -> bool {
    if path.is_absolute() {
        return true;
    }
    let rendered = path.to_string_lossy();
    rendered.starts_with('.')
        || rendered.starts_with('~')
        || rendered.contains('/')
        || rendered.contains('\\')
}

fn canonical_or_lexical(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| lexical_absolute(Path::new("."), path))
}

fn lexical_absolute(base: &Path, path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    normalize_lexical(&joined)
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() && !path.is_absolute() {
                    out.push("..");
                }
            }
            Component::Normal(segment) => out.push(segment),
        }
    }
    out
}

pub(crate) fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    for (target, source) in bytes.iter_mut().zip(hash.as_bytes().iter().copied()) {
        *target = source;
    }
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

#[allow(dead_code, reason = "N4.3 staged token-threaded workspace ID helper")]
pub(crate) fn stable_workspace_id_seeded(
    path: &Path,
    determinism: &mut Deterministic<Seed>,
) -> String {
    let workspace_scope = determinism.child("ulid.workspace");
    let seed_material = format!(
        "{}:{}",
        workspace_scope.seed().as_u64(),
        path.to_string_lossy()
    );
    let mut path_token = Deterministic::from_persistent_seed(seed_material.as_bytes());
    WorkspaceId::from_uuid(path_token.clock().next_uuid_v7()).to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::thread;
    use std::time::Duration;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::core::hygiene_coordination::{ActiveAgent, AgentMailReservation};
    use crate::core::swarm_brief::{
        WorkspaceGitOperationState, WorkspaceGitPathMetadata, WorkspaceGitStatusEntry,
    };

    type TestResult = Result<(), String>;

    fn unique_dir(prefix: &str) -> Result<PathBuf, String> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos();
        Ok(env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id())))
    }

    fn initialized_workspace(prefix: &str) -> Result<PathBuf, String> {
        let root = unique_dir(prefix)?;
        fs::create_dir_all(root.join(WORKSPACE_MARKER)).map_err(|error| error.to_string())?;
        Ok(root)
    }

    fn status_entry(path: &str, staged: &str, unstaged: &str) -> WorkspaceGitStatusEntry {
        WorkspaceGitStatusEntry {
            path: path.to_owned(),
            original_path: None,
            staged: staged.to_owned(),
            unstaged: unstaged.to_owned(),
            entry_kind: "ordinary".to_owned(),
            submodule_state: None,
            metadata: None,
        }
    }

    fn untracked_status_entry(path: &str) -> WorkspaceGitStatusEntry {
        WorkspaceGitStatusEntry {
            path: path.to_owned(),
            original_path: None,
            staged: "?".to_owned(),
            unstaged: "?".to_owned(),
            entry_kind: "untracked".to_owned(),
            submodule_state: None,
            metadata: None,
        }
    }

    fn file_status_entry(
        path: &str,
        staged: &str,
        unstaged: &str,
        size_bytes: u64,
    ) -> WorkspaceGitStatusEntry {
        let mut entry = status_entry(path, staged, unstaged);
        entry.metadata = Some(WorkspaceGitPathMetadata {
            exists: true,
            file_type: "file".to_owned(),
            size_bytes: Some(size_bytes),
            large_file: false,
            skip_reason: None,
        });
        entry
    }

    fn file_untracked_status_entry(path: &str, size_bytes: u64) -> WorkspaceGitStatusEntry {
        let mut entry = untracked_status_entry(path);
        entry.metadata = Some(WorkspaceGitPathMetadata {
            exists: true,
            file_type: "file".to_owned(),
            size_bytes: Some(size_bytes),
            large_file: false,
            skip_reason: None,
        });
        entry
    }

    fn hygiene_snapshot(entries: Vec<WorkspaceGitStatusEntry>) -> WorkspaceGitSnapshot {
        WorkspaceGitSnapshot {
            repository_root: "/repo".to_owned(),
            entries,
            operation_state: WorkspaceGitOperationState::default(),
        }
    }

    fn hygiene_report_from_parts(
        snapshot: WorkspaceGitSnapshot,
        agent_mail_input: &AgentMailCoordinationInput,
        beads_metadata_signal: BeadsMetadataSignal,
        beads_reservations: &[BeadsReservationHolder],
    ) -> WorkspaceHygieneReport {
        build_workspace_hygiene_report_from_inputs(WorkspaceHygieneReportInputs {
            workspace_path: Path::new("/repo"),
            snapshot,
            classifier_config: &HygieneClassifierConfig::default(),
            jsonl_content: Some(b"{\"id\":\"bd-test\",\"title\":\"test\"}\n"),
            self_agent_name: Some("IvoryCondor"),
            beads_metadata_signal,
            beads_reservations,
            agent_mail_input,
            now: DateTime::parse_from_rfc3339("2026-05-18T08:00:00Z")
                .expect("valid test timestamp")
                .with_timezone(&Utc),
        })
    }

    fn hygiene_report_from_workspace_parts(
        workspace_path: &Path,
        snapshot: WorkspaceGitSnapshot,
        agent_mail_input: &AgentMailCoordinationInput,
        beads_metadata_signal: BeadsMetadataSignal,
        beads_reservations: &[BeadsReservationHolder],
    ) -> WorkspaceHygieneReport {
        build_workspace_hygiene_report_from_inputs(WorkspaceHygieneReportInputs {
            workspace_path,
            snapshot,
            classifier_config: &HygieneClassifierConfig::default(),
            jsonl_content: Some(b"{\"id\":\"bd-test\",\"title\":\"test\"}\n"),
            self_agent_name: Some("IvoryCondor"),
            beads_metadata_signal,
            beads_reservations,
            agent_mail_input,
            now: DateTime::parse_from_rfc3339("2026-05-18T08:00:00Z")
                .expect("valid test timestamp")
                .with_timezone(&Utc),
        })
    }

    fn write_file(path: &Path, body: &str) -> TestResult {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(path, body).map_err(|error| error.to_string())
    }

    #[test]
    fn detects_beads_db_pending_flush_when_db_marker_is_newer_than_jsonl() -> TestResult {
        let workspace = unique_dir("ee-beads-db-newer")?;
        let beads_dir = workspace.join(".beads");
        write_file(&beads_dir.join("issues.jsonl"), "{\"id\":\"bd-test\"}\n")?;
        thread::sleep(Duration::from_millis(1_100));
        write_file(&beads_dir.join("beads.db"), "sqlite marker")?;

        assert_eq!(
            detect_beads_metadata_signal(&workspace),
            BeadsMetadataSignal::DbDirtyPendingFlush
        );
        Ok(())
    }

    #[test]
    fn detects_beads_external_import_pending_when_jsonl_is_newer_than_db_marker() -> TestResult {
        let workspace = unique_dir("ee-beads-jsonl-newer")?;
        let beads_dir = workspace.join(".beads");
        write_file(&beads_dir.join("beads.db"), "sqlite marker")?;
        thread::sleep(Duration::from_millis(1_100));
        write_file(&beads_dir.join("issues.jsonl"), "{\"id\":\"bd-test\"}\n")?;

        assert_eq!(
            detect_beads_metadata_signal(&workspace),
            BeadsMetadataSignal::ExternalChangesPendingImport
        );
        Ok(())
    }

    #[test]
    fn loads_agent_mail_snapshot_as_coordination_input() -> TestResult {
        let workspace = unique_dir("ee-agent-mail-snapshot")?;
        let snapshot_path = workspace.join("agent-mail.json");
        write_file(
            &snapshot_path,
            r#"{
              "file_reservations": [
                {
                  "path_pattern": "src/core/workspace.rs",
                  "holder": "OtherAgent",
                  "exclusive": true,
                  "expires_at": "2026-05-18T09:00:00Z"
                }
              ],
              "active_agents": [
                {"name": "OtherAgent", "last_active_at": "2026-05-18T08:45:00Z"}
              ],
              "inbox": [],
              "threads": []
            }"#,
        )?;

        let input = load_agent_mail_coordination_input(Some(&snapshot_path));
        let AgentMailCoordinationInput::Available {
            reservations,
            active_agents,
        } = input
        else {
            return Err("snapshot must load as available Agent Mail input".to_string());
        };
        assert_eq!(reservations.len(), 1);
        assert_eq!(reservations[0].path_pattern, "src/core/workspace.rs");
        assert_eq!(reservations[0].holder_agent, "OtherAgent");
        assert_eq!(active_agents.len(), 1);
        assert_eq!(active_agents[0].name, "OtherAgent");
        Ok(())
    }

    #[test]
    fn alias_validation_rejects_paths() {
        assert!(normalize_alias("client-api").is_ok());
        assert!(normalize_alias("client.api").is_ok());
        assert!(normalize_alias("client/api").is_err());
        assert!(normalize_alias(".").is_err());
        assert!(normalize_alias("..").is_err());
        assert!(normalize_alias("...").is_err());
        assert!(normalize_alias("..foo").is_err());
        assert!(normalize_alias(".bar.").is_err());
        assert!(normalize_alias("foo..").is_ok());
    }

    #[test]
    fn stable_workspace_id_seeded_replays_with_same_seed_and_path() {
        let path = Path::new("/tmp/ee-seeded-workspace");

        let mut first_token = Deterministic::from_seed(44);
        let first = stable_workspace_id_seeded(path, &mut first_token);
        let second = stable_workspace_id_seeded(path, &mut first_token);

        let mut replay_token = Deterministic::from_seed(44);
        assert_eq!(first, stable_workspace_id_seeded(path, &mut replay_token));
        assert_eq!(second, stable_workspace_id_seeded(path, &mut replay_token));

        let mut other_seed = Deterministic::from_seed(45);
        assert_ne!(first, stable_workspace_id_seeded(path, &mut other_seed));

        let mut other_path = Deterministic::from_seed(44);
        assert_ne!(
            first,
            stable_workspace_id_seeded(Path::new("/tmp/ee-other-workspace"), &mut other_path)
        );
        assert!(first.starts_with("wsp_"));
    }

    #[test]
    fn alias_command_registers_and_resolves_workspace() -> TestResult {
        let workspace = initialized_workspace("ee-workspace-alias")?;
        let registry = unique_dir("ee-workspace-registry")?.join("registry.db");
        let report = alias_workspace(&WorkspaceAliasOptions {
            workspace_path: Some(workspace.clone()),
            pick: None,
            alias: Some("client-api".to_string()),
            clear: false,
            dry_run: false,
            registry_path: Some(registry.clone()),
        })
        .map_err(|error| error.message())?;

        assert!(report.persisted);
        assert_eq!(report.alias.as_deref(), Some("client-api"));

        let resolved = resolve_workspace_report(&WorkspaceResolveOptions {
            workspace_path: None,
            target: Some("client-api".to_string()),
            registry_path: Some(registry),
        })
        .map_err(|error| error.message())?;

        assert_eq!(resolved.source, "alias");
        assert_eq!(
            PathBuf::from(resolved.root),
            workspace.canonicalize().unwrap_or(workspace)
        );
        Ok(())
    }

    #[test]
    fn alias_dry_run_does_not_create_registry() -> TestResult {
        let workspace = initialized_workspace("ee-workspace-alias-dry")?;
        let registry = unique_dir("ee-workspace-registry-dry")?.join("registry.db");
        let report = alias_workspace(&WorkspaceAliasOptions {
            workspace_path: Some(workspace),
            pick: None,
            alias: Some("dry-run".to_string()),
            clear: false,
            dry_run: true,
            registry_path: Some(registry.clone()),
        })
        .map_err(|error| error.message())?;

        assert!(!report.persisted);
        assert!(!registry.exists());
        Ok(())
    }

    #[test]
    fn registry_list_reports_missing_registry_without_creating_it() -> TestResult {
        let registry = unique_dir("ee-workspace-registry-missing")?.join("registry.db");
        let report = list_workspace_registry(&WorkspaceListOptions {
            registry_path: Some(registry.clone()),
        })
        .map_err(|error| error.message())?;

        assert!(!report.registry_exists);
        assert!(!registry.exists());
        assert!(report.workspaces.is_empty());
        Ok(())
    }

    #[test]
    fn registry_list_rejects_directory_registry_path() -> TestResult {
        let registry = unique_dir("ee-workspace-registry-dir")?.join("registry.db");
        fs::create_dir_all(&registry).map_err(|error| error.to_string())?;

        let error = match list_workspace_registry(&WorkspaceListOptions {
            registry_path: Some(registry),
        }) {
            Ok(_) => return Err("directory registry path must be rejected".to_string()),
            Err(error) => error,
        };
        assert!(
            error.message().contains("not a regular file"),
            "unexpected error: {}",
            error.message()
        );

        Ok(())
    }

    #[test]
    fn registry_write_rejects_directory_registry_path() -> TestResult {
        let registry = unique_dir("ee-workspace-registry-write-dir")?.join("registry.db");
        fs::create_dir_all(&registry).map_err(|error| error.to_string())?;

        let error = match open_registry_write(&registry) {
            Ok(_) => return Err("directory registry path must be rejected".to_string()),
            Err(error) => error,
        };
        assert!(
            error.message().contains("not a regular file"),
            "unexpected error: {}",
            error.message()
        );

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn registry_write_rejects_symlinked_parent() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside_parent = temp.path().join("outside-registry-parent");
        fs::create_dir_all(&outside_parent).map_err(|error| error.to_string())?;
        let linked_parent = temp.path().join("linked-registry-parent");
        symlink(&outside_parent, &linked_parent).map_err(|error| error.to_string())?;
        let registry = linked_parent.join("registry.db");

        let error = match open_registry_write(&registry) {
            Ok(_) => return Err("symlinked parent must be rejected".to_string()),
            Err(error) => error,
        };
        assert!(
            error.message().contains("symlink component"),
            "unexpected error: {}",
            error.message()
        );
        assert!(
            !outside_parent.join("registry.db").exists(),
            "registry write must not follow a symlinked parent"
        );

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn registry_read_rejects_symlinked_registry_file() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside_registry = temp.path().join("outside-registry.db");
        fs::write(&outside_registry, b"not a trusted registry")
            .map_err(|error| error.to_string())?;
        let registry = temp.path().join("registry.db");
        symlink(&outside_registry, &registry).map_err(|error| error.to_string())?;

        let error = match open_registry_read_only(&registry) {
            Ok(_) => return Err("symlinked registry must be rejected".to_string()),
            Err(error) => error,
        };
        assert!(
            error.message().contains("symlink component"),
            "unexpected error: {}",
            error.message()
        );

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn registry_list_rejects_dangling_symlink_registry_file() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let registry = temp.path().join("registry.db");
        symlink(temp.path().join("missing-outside-registry.db"), &registry)
            .map_err(|error| error.to_string())?;

        let error = match list_workspace_registry(&WorkspaceListOptions {
            registry_path: Some(registry),
        }) {
            Ok(_) => return Err("dangling symlinked registry must be rejected".to_string()),
            Err(error) => error,
        };
        assert!(
            error.message().contains("symlink component"),
            "unexpected error: {}",
            error.message()
        );

        Ok(())
    }

    #[test]
    fn hygiene_report_applies_precollected_agent_mail_reservations() {
        let agent_mail = AgentMailCoordinationInput::Available {
            reservations: vec![AgentMailReservation {
                path_pattern: "src/core/workspace.rs".to_owned(),
                holder_agent: "OtherAgent".to_owned(),
                exclusive: true,
                expires_at: Some("2026-05-18T09:00:00Z".to_owned()),
                reservation_id: Some("reservation-1".to_owned()),
                bead_id: Some("bd-1eq3l.5".to_owned()),
                thread_id: Some("bd-1eq3l.5".to_owned()),
            }],
            active_agents: vec![ActiveAgent {
                name: "OtherAgent".to_owned(),
                last_active_at: Some("2026-05-18T07:55:00Z".to_owned()),
            }],
        };
        let report = hygiene_report_from_parts(
            hygiene_snapshot(vec![status_entry("src/core/workspace.rs", ".", "M")]),
            &agent_mail,
            BeadsMetadataSignal::Unknown,
            &[],
        );

        assert!(report.coordination.agent_mail_available);
        assert_eq!(report.coordination.active_agent_count, 1);
        assert_eq!(report.coordination.blocked_by_coordination.len(), 1);
        assert_eq!(
            report.coordination.blocked_by_coordination[0].path,
            "src/core/workspace.rs"
        );
        assert!(
            report.staging_groups.iter().all(|group| group
                .paths
                .iter()
                .all(|path| path != "src/core/workspace.rs")),
            "coordination-blocked paths must not be suggested as commit-ready"
        );
        assert!(
            !report
                .degraded_codes
                .contains(&WORKSPACE_HYGIENE_AGENT_MAIL_UNAVAILABLE_CODE),
            "available Agent Mail input must not report unavailable degradation"
        );
    }

    #[test]
    fn hygiene_report_applies_beads_metadata_signal_and_reservation_priority() {
        let beads_reservations = vec![BeadsReservationHolder {
            agent_name: "OtherAgent".to_owned(),
            exclusive: true,
            expires_ts_rfc3339: "2026-05-18T09:00:00Z".to_owned(),
        }];
        let report = hygiene_report_from_parts(
            hygiene_snapshot(vec![status_entry(BEADS_JSONL_RELATIVE_PATH, ".", "M")]),
            &AgentMailCoordinationInput::Unavailable,
            BeadsMetadataSignal::DbDirtyPendingFlush,
            &beads_reservations,
        );

        assert_eq!(
            report.beads_state.classification,
            crate::core::hygiene_beads_state::BeadsClassification::BeadsReservedByOtherAgent
        );
        assert_eq!(
            report.beads_state.metadata_signal,
            BeadsMetadataSignal::DbDirtyPendingFlush
        );
        assert_eq!(report.beads_state.reservation_holders.len(), 1);
    }

    #[test]
    fn hygiene_recommendations_split_logical_groups_and_explain_reasons() {
        let agent_mail = AgentMailCoordinationInput::Available {
            reservations: vec![AgentMailReservation {
                path_pattern: "src/core/lib.rs".to_owned(),
                holder_agent: "OtherAgent".to_owned(),
                exclusive: true,
                expires_at: Some("2026-05-18T09:00:00Z".to_owned()),
                reservation_id: Some("reservation-1".to_owned()),
                bead_id: None,
                thread_id: None,
            }],
            active_agents: Vec::new(),
        };
        let report = hygiene_report_from_parts(
            hygiene_snapshot(vec![
                status_entry("src/core/lib.rs", ".", "M"),
                status_entry("src/core/workspace.rs", ".", "M"),
                status_entry("tests/workspace_hygiene.rs", ".", "M"),
                status_entry("tests/fixtures/golden/workspace.json", ".", "M"),
                status_entry("docs/agent-ux/workspace-hygiene.md", ".", "M"),
            ]),
            &agent_mail,
            BeadsMetadataSignal::Unknown,
            &[],
        );

        let group_names = report
            .staging_groups
            .iter()
            .map(|group| group.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(group_names, vec!["docs", "goldens", "source", "tests"]);
        assert!(
            report.staging_groups.iter().all(|group| group.read_only),
            "recommendations must remain read-only"
        );
        assert!(
            report
                .staging_groups
                .iter()
                .all(|group| !group.paths.iter().any(|path| path == "src/core/lib.rs")),
            "coordination-blocked paths must not be stage recommendations"
        );
        let source = report
            .staging_groups
            .iter()
            .find(|group| group.name == "source")
            .expect("source group");
        assert_eq!(source.paths, vec!["src/core/workspace.rs"]);
        assert_eq!(source.path_count, 1);
        assert_eq!(source.kinds, vec!["source"]);
        assert!(source.reasons.contains(&"src_rust_source".to_owned()));
        assert_eq!(
            source.recommendation,
            "review_and_stage_as_one_logical_commit"
        );
    }

    #[test]
    fn hygiene_recommendations_keep_beads_only_metadata_out_of_fast_stage_groups() {
        let report = hygiene_report_from_parts(
            hygiene_snapshot(vec![status_entry(BEADS_JSONL_RELATIVE_PATH, ".", "M")]),
            &AgentMailCoordinationInput::Available {
                reservations: Vec::new(),
                active_agents: Vec::new(),
            },
            BeadsMetadataSignal::Unknown,
            &[],
        );

        assert!(report.staging_groups.is_empty());
        assert!(report.do_not_commit.is_empty());
        assert!(report.needs_human_review.is_empty());
        assert_eq!(report.classifications.len(), 1);
        assert_eq!(report.classifications[0].bucket, Bucket::IgnoreForNow);
        assert_eq!(report.classifications[0].kind, Kind::BeadsMetadata);
        assert!(report.read_only);
    }

    #[test]
    fn hygiene_recommendations_keep_scratch_only_workspaces_out_of_staging() {
        let report = hygiene_report_from_parts(
            hygiene_snapshot(vec![
                untracked_status_entry("drift-report.txt"),
                untracked_status_entry("ubs.json"),
            ]),
            &AgentMailCoordinationInput::Available {
                reservations: Vec::new(),
                active_agents: Vec::new(),
            },
            BeadsMetadataSignal::Unknown,
            &[],
        );

        assert!(report.staging_groups.is_empty());
        assert_eq!(report.do_not_commit, vec!["drift-report.txt", "ubs.json"]);
        assert!(report.needs_human_review.is_empty());
        assert!(
            report
                .classifications
                .iter()
                .all(|row| row.bucket == Bucket::DoNotCommit && row.kind == Kind::Scratch)
        );
        assert!(
            report
                .next_actions
                .iter()
                .any(|action| action.contains("Leave doNotCommit paths unstaged")),
            "scratch-only report should tell agents not to stage scratch paths"
        );
        assert!(report.read_only);
    }

    #[test]
    fn hygiene_recommendations_keep_secret_risk_paths_out_of_staging() {
        let report = hygiene_report_from_parts(
            hygiene_snapshot(vec![
                untracked_status_entry(".env.local"),
                status_entry("configs/secrets.toml", ".", "M"),
                status_entry("src/core/workspace.rs", ".", "M"),
            ]),
            &AgentMailCoordinationInput::Available {
                reservations: Vec::new(),
                active_agents: Vec::new(),
            },
            BeadsMetadataSignal::Unknown,
            &[],
        );

        let recommended_paths = report
            .staging_groups
            .iter()
            .flat_map(|group| group.paths.iter())
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(recommended_paths, vec!["src/core/workspace.rs"]);
        assert_eq!(report.do_not_commit, vec![".env.local"]);
        assert_eq!(report.needs_human_review, vec!["configs/secrets.toml"]);

        let env_row = report
            .classifications
            .iter()
            .find(|row| row.path == ".env.local")
            .expect("env file classification");
        assert_eq!(env_row.kind, Kind::SecretRisk);
        assert_eq!(env_row.bucket, Bucket::DoNotCommit);

        let tracked_secret_row = report
            .classifications
            .iter()
            .find(|row| row.path == "configs/secrets.toml")
            .expect("tracked secret classification");
        assert_eq!(tracked_secret_row.kind, Kind::SecretRisk);
        assert_eq!(tracked_secret_row.bucket, Bucket::NeedsHumanReview);
        assert!(report.read_only);
    }

    #[test]
    fn hygiene_secret_scan_collects_redacted_content_evidence() -> TestResult {
        let workspace = unique_dir("ee-workspace-hygiene-secret-scan")?;
        let raw_value = concat!(
            "sk",
            "-",
            "proj-",
            "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"
        );
        write_file(
            &workspace.join("notes.txt"),
            &format!("ordinary notes\nOPENAI_API_KEY={raw_value}\n"),
        )?;
        let report = hygiene_report_from_workspace_parts(
            &workspace,
            hygiene_snapshot(vec![file_untracked_status_entry(
                "notes.txt",
                u64::try_from(format!("ordinary notes\nOPENAI_API_KEY={raw_value}\n").len())
                    .unwrap_or(u64::MAX),
            )]),
            &AgentMailCoordinationInput::Available {
                reservations: Vec::new(),
                active_agents: Vec::new(),
            },
            BeadsMetadataSignal::Unknown,
            &[],
        );

        let row = report
            .classifications
            .iter()
            .find(|row| row.path == "notes.txt")
            .expect("notes classification");
        assert_eq!(row.kind, Kind::SecretRisk);
        assert_eq!(row.bucket, Bucket::DoNotCommit);
        assert!(row.reasons.contains(&"secret_content_evidence"));
        assert!(!row.redacted_evidence.is_empty());
        assert_eq!(report.secret_scan.scanned_file_count, 1);
        assert_eq!(
            report.secret_scan.max_file_bytes,
            WORKSPACE_SECRET_RISK_DEFAULT_MAX_SCAN_BYTES
        );
        assert_eq!(
            report.secret_scan.max_total_bytes,
            WORKSPACE_HYGIENE_SECRET_SCAN_MAX_TOTAL_BYTES
        );
        let rendered = serde_json::to_string(&report).expect("workspace hygiene JSON");
        assert!(
            !rendered.contains(raw_value),
            "workspace hygiene report must not leak raw content secret"
        );
        assert!(
            !report
                .degraded_codes
                .contains(&WORKSPACE_HYGIENE_SECRET_SCAN_SKIPPED_CODE)
        );
        Ok(())
    }

    #[test]
    fn hygiene_secret_scan_enforces_file_count_and_total_byte_budgets() -> TestResult {
        let workspace = unique_dir("ee-workspace-hygiene-secret-budget")?;
        write_file(&workspace.join("a.txt"), "alpha")?;
        write_file(&workspace.join("b.txt"), "bravo")?;
        write_file(&workspace.join("large.txt"), "0123456789")?;
        let snapshot = hygiene_snapshot(vec![
            file_status_entry("a.txt", ".", "M", 5),
            file_status_entry("b.txt", ".", "M", 5),
            file_status_entry("large.txt", ".", "M", 10),
        ]);

        let (lookup, summary) = workspace_hygiene_secret_evidence_with_budget(
            &workspace,
            &snapshot,
            WorkspaceHygieneSecretScanBudget {
                max_files: 10,
                max_file_bytes: 8,
                max_total_bytes: 5,
            },
        );

        assert!(lookup.is_empty());
        assert_eq!(summary.scanned_file_count, 1);
        assert_eq!(summary.scanned_byte_count, 5);
        assert_eq!(
            summary.skipped_content_scan_count, 2,
            "one path should exceed max_total_bytes and one should exceed max_file_bytes"
        );

        let (_, file_count_summary) = workspace_hygiene_secret_evidence_with_budget(
            &workspace,
            &snapshot,
            WorkspaceHygieneSecretScanBudget {
                max_files: 1,
                max_file_bytes: 8,
                max_total_bytes: 20,
            },
        );
        assert_eq!(file_count_summary.scanned_file_count, 1);
        assert_eq!(file_count_summary.scanned_byte_count, 5);
        assert_eq!(
            file_count_summary.skipped_content_scan_count, 2,
            "one path should hit max_files and one should exceed max_file_bytes"
        );

        let report = workspace_hygiene_secret_scan_report(
            WorkspaceHygieneSecretScanBudget {
                max_files: 1,
                max_file_bytes: 8,
                max_total_bytes: 20,
            },
            file_count_summary,
        );
        assert!(report.read_only);
        assert_eq!(report.max_files, 1);
        assert_eq!(report.max_file_bytes, 8);
        assert_eq!(report.max_total_bytes, 20);
        assert_eq!(report.skipped_content_scan_count, 2);
        Ok(())
    }

    #[test]
    fn hygiene_report_records_10k_path_perf_contract_size_proxy() {
        let entries = (0..10_000)
            .map(|index| status_entry(&format!("src/perf/file_{index:05}.rs"), ".", "M"))
            .collect::<Vec<_>>();
        let started = std::time::Instant::now();
        let report = hygiene_report_from_parts(
            hygiene_snapshot(entries),
            &AgentMailCoordinationInput::Available {
                reservations: Vec::new(),
                active_agents: Vec::new(),
            },
            BeadsMetadataSignal::Unknown,
            &[],
        );
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let serialized = serde_json::to_string(&report).expect("workspace hygiene JSON");

        eprintln!(
            "{}",
            serde_json::json!({
                "schema": "ee.test_event.v1",
                "beadId": "bd-1eq3l.13",
                "surface": "workspace_hygiene",
                "phase": "perf_contract",
                "pathCount": 10_000,
                "elapsedMs": elapsed_ms,
                "serializedBytes": serialized.len(),
                "pathClassificationCount": report.classifications.len(),
                "stagingGroupCount": report.staging_groups.len(),
                "truncated": report.output_truncation.truncated,
            })
        );

        assert_eq!(report.dirty_path_count, 10_000);
        assert_eq!(report.git_summary.dirty_path_count, 10_000);
        assert_eq!(report.classifications.len(), 10_000);
        assert!(
            !report.output_truncation.truncated,
            "10k fixture sits exactly at the default visible cap and must not truncate"
        );
        assert!(
            !report
                .degraded_codes
                .contains(&WORKSPACE_HYGIENE_OUTPUT_TRUNCATED_CODE)
        );
        assert_eq!(report.staging_groups.len(), 1);
        let source_group = &report.staging_groups[0];
        assert_eq!(source_group.name, "source");
        assert_eq!(source_group.path_count, 10_000);
        assert_eq!(source_group.paths.len(), 10_000);
        assert!(!source_group.paths_truncated);
        assert_eq!(source_group.omitted_path_count, 0);
        assert_eq!(source_group.paths[0], "src/perf/file_00000.rs");
        assert_eq!(
            source_group.paths.last().map(String::as_str),
            Some("src/perf/file_09999.rs")
        );
        assert!(
            serialized.len() < 5_000_000,
            "10k workspace hygiene report should stay within the perf-contract size proxy, got {} bytes",
            serialized.len()
        );
    }

    #[test]
    fn hygiene_report_truncates_large_path_arrays_deterministically() {
        let entries = (0..100_050)
            .map(|index| status_entry(&format!("src/generated/file_{index:06}.rs"), ".", "M"))
            .collect::<Vec<_>>();
        let report = hygiene_report_from_parts(
            hygiene_snapshot(entries),
            &AgentMailCoordinationInput::Available {
                reservations: Vec::new(),
                active_agents: Vec::new(),
            },
            BeadsMetadataSignal::Unknown,
            &[],
        );

        assert_eq!(report.dirty_path_count, 100_050);
        assert_eq!(report.git_summary.dirty_path_count, 100_050);
        assert_eq!(
            report.classifications.len(),
            WORKSPACE_HYGIENE_MAX_PATH_CLASSIFICATIONS
        );
        assert!(report.output_truncation.truncated);
        assert_eq!(
            report.output_truncation.omitted_path_classifications,
            100_050 - WORKSPACE_HYGIENE_MAX_PATH_CLASSIFICATIONS
        );
        assert_eq!(report.output_truncation.omitted_do_not_commit, 0);
        assert_eq!(report.output_truncation.omitted_needs_human_review, 0);
        assert_eq!(
            report.output_truncation.omitted_by_bucket,
            vec![WorkspaceHygieneCount {
                name: "stage_candidate".to_owned(),
                count: 100_050 - WORKSPACE_HYGIENE_MAX_PATH_CLASSIFICATIONS,
            }]
        );
        assert_eq!(
            report.output_truncation.omitted_by_kind,
            vec![WorkspaceHygieneCount {
                name: "source".to_owned(),
                count: 100_050 - WORKSPACE_HYGIENE_MAX_PATH_CLASSIFICATIONS,
            }]
        );
        assert!(
            report
                .degraded_codes
                .contains(&WORKSPACE_HYGIENE_OUTPUT_TRUNCATED_CODE)
        );
        assert_eq!(report.staging_groups.len(), 1);
        let source_group = &report.staging_groups[0];
        assert_eq!(source_group.name, "source");
        assert_eq!(source_group.path_count, 100_050);
        assert!(source_group.paths_truncated);
        assert_eq!(
            source_group.paths.len(),
            WORKSPACE_HYGIENE_MAX_PATHS_PER_STAGING_GROUP
        );
        assert_eq!(
            source_group.omitted_path_count,
            100_050 - WORKSPACE_HYGIENE_MAX_PATHS_PER_STAGING_GROUP
        );
        assert_eq!(source_group.paths[0], "src/generated/file_000000.rs");
        assert_eq!(
            source_group
                .paths
                .last()
                .map(String::as_str)
                .expect("last truncated path"),
            "src/generated/file_009999.rs"
        );
        assert_eq!(
            report.output_truncation.staging_groups[0].omitted_path_count,
            100_050 - WORKSPACE_HYGIENE_MAX_PATHS_PER_STAGING_GROUP
        );
        assert!(
            report
                .next_actions
                .iter()
                .any(|action| action.contains("outputTruncation")),
            "truncated reports should point agents at outputTruncation details"
        );

        let serialized = serde_json::to_string(&report).expect("workspace hygiene JSON");
        assert!(
            serialized.len() < 8_000_000,
            "serialized report should stay within the large-report output budget, got {} bytes",
            serialized.len()
        );
        assert!(serialized.contains("\"outputTruncation\""));
        assert!(serialized.contains("\"pathsTruncated\":true"));
        assert!(
            !serialized.contains("src/generated/file_010000.rs"),
            "paths beyond the deterministic visible prefix must be omitted"
        );
    }
}
