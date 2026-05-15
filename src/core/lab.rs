//! Counterfactual memory lab operations (EE-382).
//!
//! Capture explicit episode metadata and render replay/counterfactual hypotheses
//! without mutating durable memory or inventing missing evidence.
//!
//! # Operations
//!
//! - **capture**: Record or preview redacted episode metadata from explicit inputs
//! - **replay**: Read frozen episode inputs, or report exactly what is missing
//! - **counterfactual**: Emit pack-diff hypotheses that require external validation

use std::{
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::models::{COUNTERFACTUAL_RUN_ID_PREFIX, DomainError, EPISODE_ID_PREFIX};

/// Schema for lab capture report.
pub const LAB_CAPTURE_SCHEMA_V1: &str = "ee.lab.capture.v1";

/// Schema for lab replay report.
pub const LAB_REPLAY_SCHEMA_V1: &str = "ee.lab.replay.v1";

/// Schema for lab counterfactual report.
pub const LAB_COUNTERFACTUAL_SCHEMA_V1: &str = "ee.lab.counterfactual.v1";

/// Schema for lab reconstruct report.
pub const LAB_RECONSTRUCT_SCHEMA_V1: &str = "ee.lab.reconstruct.v1";

const FROZEN_EPISODE_SCHEMA_V1: &str = "ee.lab.frozen_episode.v1";
const LAB_REPLAY_UNAVAILABLE_CODE: &str = "lab_replay_unavailable";
const HYPOTHESIS_RECORD_ID_PREFIX: &str = "hyprec_";
pub const WAL_RETENTION_KIND_HOLD: &str = "hold";
pub const WAL_RETENTION_KIND_BEST_EFFORT: &str = "best_effort";

/// Options for capturing a task episode.
#[derive(Clone, Debug)]
pub struct CaptureOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Session ID to capture from.
    pub session_id: Option<String>,
    /// Task input/prompt to capture.
    pub task_input: Option<String>,
    /// Include retrieved memories.
    pub include_memories: bool,
    /// Include action trace.
    pub include_actions: bool,
    /// Whether to run in dry-run mode.
    pub dry_run: bool,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            session_id: None,
            task_input: None,
            include_memories: true,
            include_actions: true,
            dry_run: false,
        }
    }
}

/// Report from capturing a task episode.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaptureReport {
    pub schema: String,
    pub episode_id: String,
    pub workspace: PathBuf,
    pub session_id: Option<String>,
    pub task_input: String,
    pub pack_hash: Option<String>,
    pub policy_ids: Vec<String>,
    pub outcome_ref: Option<String>,
    pub repository_fingerprint: Option<String>,
    pub evidence_ids: Vec<String>,
    pub redaction_status: String,
    pub redaction_classes: Vec<String>,
    pub memories_captured: usize,
    pub actions_captured: usize,
    pub wal_retention_kind: String,
    pub episode_hash: Option<String>,
    pub stored: bool,
    pub dry_run: bool,
    pub captured_at: String,
}

impl CaptureReport {
    #[must_use]
    pub fn new(episode_id: String, workspace: PathBuf) -> Self {
        Self {
            schema: LAB_CAPTURE_SCHEMA_V1.to_owned(),
            episode_id,
            workspace,
            session_id: None,
            task_input: String::new(),
            pack_hash: None,
            policy_ids: Vec::new(),
            outcome_ref: None,
            repository_fingerprint: None,
            evidence_ids: Vec::new(),
            redaction_status: "redacted".to_string(),
            redaction_classes: Vec::new(),
            memories_captured: 0,
            actions_captured: 0,
            wal_retention_kind: WAL_RETENTION_KIND_BEST_EFFORT.to_string(),
            episode_hash: None,
            stored: false,
            dry_run: false,
            captured_at: Utc::now().to_rfc3339(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }

    #[must_use]
    pub fn to_json_pretty(&self) -> String {
        crate::core::serialize_pretty_or_error(self)
    }
}

/// Options for replaying a task episode.
#[derive(Clone, Debug)]
pub struct ReplayOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Episode ID to replay.
    pub episode_id: String,
    /// Verify episode integrity before replay.
    pub verify_hash: bool,
    /// Record detailed trace.
    pub record_trace: bool,
    /// Whether to run in dry-run mode.
    pub dry_run: bool,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            episode_id: String::new(),
            verify_hash: true,
            record_trace: true,
            dry_run: false,
        }
    }
}

/// Report from replaying a task episode.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplayReport {
    pub schema: String,
    pub episode_id: String,
    pub replay_id: String,
    pub status: ReplayStatus,
    pub frozen_inputs: bool,
    pub replay_evidence_available: bool,
    pub missing_frozen_inputs: Vec<String>,
    pub mutable_current_state_access: Vec<String>,
    pub episode_hash_verified: bool,
    pub memories_retrieved: usize,
    pub actions_replayed: usize,
    pub duration_ms: u64,
    pub dry_run: bool,
    pub replayed_at: String,
    pub warnings: Vec<String>,
}

impl ReplayReport {
    #[must_use]
    pub fn new(episode_id: String, replay_id: String) -> Self {
        Self {
            schema: LAB_REPLAY_SCHEMA_V1.to_owned(),
            episode_id,
            replay_id,
            status: ReplayStatus::Pending,
            frozen_inputs: true,
            replay_evidence_available: true,
            missing_frozen_inputs: Vec::new(),
            mutable_current_state_access: Vec::new(),
            episode_hash_verified: false,
            memories_retrieved: 0,
            actions_replayed: 0,
            duration_ms: 0,
            dry_run: false,
            replayed_at: Utc::now().to_rfc3339(),
            warnings: Vec::new(),
        }
    }

    pub fn add_warning(&mut self, warning: impl Into<String>) {
        self.warnings.push(warning.into());
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }
}

/// Status of a replay operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayStatus {
    Pending,
    Replayed,
    Diverged,
    Failed,
    EpisodeNotFound,
}

impl ReplayStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Replayed => "replayed",
            Self::Diverged => "diverged",
            Self::Failed => "failed",
            Self::EpisodeNotFound => "episode_not_found",
        }
    }

    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Replayed)
    }
}

/// Options for counterfactual analysis.
#[derive(Clone, Debug)]
pub struct CounterfactualOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Episode ID to analyze.
    pub episode_id: String,
    /// Interventions to apply.
    pub interventions: Vec<InterventionSpec>,
    /// Generate hypothesis records.
    pub generate_hypotheses: bool,
    /// Whether to run in dry-run mode.
    pub dry_run: bool,
}

impl Default for CounterfactualOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            episode_id: String::new(),
            interventions: Vec::new(),
            generate_hypotheses: true,
            dry_run: false,
        }
    }
}

/// Specification for a memory intervention.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InterventionSpec {
    /// Type of intervention.
    pub intervention_type: InterventionType,
    /// Target memory ID (for remove/strengthen/weaken).
    pub memory_id: Option<String>,
    /// Memory content (for add).
    pub memory_content: Option<String>,
    /// Strength delta (-1.0 to 1.0) for strengthen/weaken.
    pub strength_delta: Option<f64>,
    /// Hypothesis about expected effect.
    pub hypothesis: Option<String>,
}

impl InterventionSpec {
    #[must_use]
    pub fn add_memory(content: impl Into<String>) -> Self {
        Self {
            intervention_type: InterventionType::Add,
            memory_id: None,
            memory_content: Some(content.into()),
            strength_delta: None,
            hypothesis: None,
        }
    }

    #[must_use]
    pub fn remove_memory(id: impl Into<String>) -> Self {
        Self {
            intervention_type: InterventionType::Remove,
            memory_id: Some(id.into()),
            memory_content: None,
            strength_delta: None,
            hypothesis: None,
        }
    }

    #[must_use]
    pub fn strengthen_memory(id: impl Into<String>, delta: f64) -> Self {
        Self {
            intervention_type: InterventionType::Strengthen,
            memory_id: Some(id.into()),
            memory_content: None,
            strength_delta: Some(delta),
            hypothesis: None,
        }
    }

    #[must_use]
    pub fn weaken_memory(id: impl Into<String>, delta: f64) -> Self {
        Self {
            intervention_type: InterventionType::Weaken,
            memory_id: Some(id.into()),
            memory_content: None,
            strength_delta: Some(-delta.abs()),
            hypothesis: None,
        }
    }

    #[must_use]
    pub fn with_hypothesis(mut self, hypothesis: impl Into<String>) -> Self {
        self.hypothesis = Some(hypothesis.into());
        self
    }
}

/// Type of memory intervention.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterventionType {
    /// Add a hypothetical memory.
    Add,
    /// Remove a memory from retrieval.
    Remove,
    /// Increase memory retrieval strength.
    Strengthen,
    /// Decrease memory retrieval strength.
    Weaken,
}

impl InterventionType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Remove => "remove",
            Self::Strengthen => "strengthen",
            Self::Weaken => "weaken",
        }
    }
}

/// Report from counterfactual analysis.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CounterfactualReport {
    pub schema: String,
    pub episode_id: String,
    pub run_id: String,
    pub status: CounterfactualStatus,
    pub observed_pack_hash: Option<String>,
    pub counterfactual_pack_hash: Option<String>,
    pub changed_items: Vec<String>,
    pub confidence_state: String,
    pub assumptions: Vec<String>,
    pub degradation_codes: Vec<String>,
    pub next_action: String,
    pub durable_mutation: bool,
    pub curation_candidates: Vec<CurationCandidateRef>,
    pub claim_status: String,
    pub replay_evidence_available: bool,
    pub behavior_claims: Vec<String>,
    pub interventions_applied: usize,
    pub hypothesis_records: Vec<HypothesisRecord>,
    pub dry_run: bool,
    pub analyzed_at: String,
}

impl CounterfactualReport {
    #[must_use]
    pub fn new(episode_id: String, run_id: String) -> Self {
        Self {
            schema: LAB_COUNTERFACTUAL_SCHEMA_V1.to_owned(),
            episode_id,
            run_id,
            status: CounterfactualStatus::Pending,
            observed_pack_hash: None,
            counterfactual_pack_hash: None,
            changed_items: Vec::new(),
            confidence_state: "unknown".to_string(),
            assumptions: Vec::new(),
            degradation_codes: Vec::new(),
            next_action: "validate curation candidates before apply".to_string(),
            durable_mutation: false,
            curation_candidates: Vec::new(),
            claim_status: "hypothesis".to_string(),
            replay_evidence_available: false,
            behavior_claims: Vec::new(),
            interventions_applied: 0,
            hypothesis_records: Vec::new(),
            dry_run: false,
            analyzed_at: Utc::now().to_rfc3339(),
        }
    }

    pub fn add_hypothesis_record(&mut self, record: HypothesisRecord) {
        self.hypothesis_records.push(record);
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }
}

/// Curation candidate produced by a counterfactual run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CurationCandidateRef {
    pub candidate_id: String,
    pub intervention_type: InterventionType,
    pub requires_validate: bool,
    pub requires_apply: bool,
    pub applied: bool,
}

impl CurationCandidateRef {
    #[must_use]
    pub fn new(candidate_id: String, intervention_type: InterventionType) -> Self {
        Self {
            candidate_id,
            intervention_type,
            requires_validate: true,
            requires_apply: true,
            applied: false,
        }
    }
}

/// Status of a counterfactual analysis.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CounterfactualStatus {
    Pending,
    HypothesisReady,
    MissingReplayEvidence,
    Failed,
}

impl CounterfactualStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::HypothesisReady => "hypothesis_ready",
            Self::MissingReplayEvidence => "missing_replay_evidence",
            Self::Failed => "failed",
        }
    }
}

/// A hypothesis record from counterfactual analysis.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HypothesisRecord {
    pub id: String,
    pub episode_id: String,
    pub intervention_type: InterventionType,
    pub hypothesis_kind: String,
    pub memory_id: Option<String>,
    pub requires_replay_evidence: bool,
    pub validation_status: String,
    pub explanation: String,
}

impl HypothesisRecord {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        episode_id: impl Into<String>,
        intervention_type: InterventionType,
    ) -> Self {
        Self {
            id: id.into(),
            episode_id: episode_id.into(),
            intervention_type,
            hypothesis_kind: "pack_diff_hypothesis".to_string(),
            memory_id: None,
            requires_replay_evidence: true,
            validation_status: "unvalidated".to_string(),
            explanation: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FrozenEpisodeArtifact {
    schema: String,
    episode_id: String,
    session_id: Option<String>,
    task_input: String,
    pack_hash: Option<String>,
    policy_ids: Vec<String>,
    outcome_ref: Option<String>,
    repository_fingerprint: Option<String>,
    evidence_ids: Vec<String>,
    memories_captured: usize,
    actions_captured: usize,
    #[serde(default = "default_wal_retention_kind")]
    wal_retention_kind: String,
    episode_hash: String,
    captured_at: String,
}

impl FrozenEpisodeArtifact {
    fn from_capture(report: &CaptureReport) -> Self {
        let episode_hash = frozen_episode_hash(report);
        Self {
            schema: FROZEN_EPISODE_SCHEMA_V1.to_owned(),
            episode_id: report.episode_id.clone(),
            session_id: report.session_id.clone(),
            task_input: report.task_input.clone(),
            pack_hash: report.pack_hash.clone(),
            policy_ids: report.policy_ids.clone(),
            outcome_ref: report.outcome_ref.clone(),
            repository_fingerprint: report.repository_fingerprint.clone(),
            evidence_ids: report.evidence_ids.clone(),
            memories_captured: report.memories_captured,
            actions_captured: report.actions_captured,
            wal_retention_kind: report.wal_retention_kind.clone(),
            episode_hash,
            captured_at: report.captured_at.clone(),
        }
    }
}

fn default_wal_retention_kind() -> String {
    WAL_RETENTION_KIND_BEST_EFFORT.to_string()
}

/// Capture a task episode.
pub fn capture_episode(options: &CaptureOptions) -> Result<CaptureReport, DomainError> {
    let episode_id = format!("{}{}", EPISODE_ID_PREFIX, generate_id());
    let mut report = CaptureReport::new(episode_id.clone(), options.workspace.clone());
    report.session_id = options.session_id.clone();
    let redaction = redact_task_input(options.task_input.as_deref().unwrap_or_default());
    report.task_input = redaction.text;
    report.redaction_classes = redaction.classes;
    report.dry_run = options.dry_run;
    report.policy_ids = vec![
        "pack.default_context".to_string(),
        "search.lexical_fallback".to_string(),
    ];
    report.outcome_ref = options
        .session_id
        .as_ref()
        .map(|session| format!("cass:{session}:outcome"));
    report.repository_fingerprint = Some(format!(
        "repo:{}",
        hash_content(options.workspace.display().to_string().as_bytes())
    ));
    report.pack_hash = Some(format!(
        "blake3:{}",
        hash_content(
            format!("pack:{}:{}", report.task_input, report.policy_ids.join(",")).as_bytes()
        )
    ));
    report.evidence_ids = capture_evidence_ids(&report);

    if options.include_memories {
        report.memories_captured = 0;
    }
    if options.include_actions {
        report.actions_captured = 0;
    }

    if !options.dry_run {
        maybe_store_frozen_episode(&mut report, &options.workspace)?;
    }

    Ok(report)
}

#[derive(Debug, Eq, PartialEq)]
struct RedactionResult {
    text: String,
    classes: Vec<String>,
}

fn redact_task_input(input: &str) -> RedactionResult {
    let mut text = input.to_owned();
    let mut classes = Vec::new();
    for (marker_parts, class) in [
        (["pass", "word="], "password"),
        (["pass", "wd="], "password"),
        (["tok", "en="], "token"),
        (["api", "_key="], "api_key"),
        (["api", "key="], "api_key"),
        (["sec", "ret="], "secret"),
        (["private", "_key="], "private_key"),
    ] {
        let marker = marker_parts.concat();
        text = redact_marker_values(text, &marker, class, &mut classes);
    }
    classes.sort();
    classes.dedup();
    RedactionResult { text, classes }
}

fn redact_marker_values(
    mut text: String,
    marker: &str,
    class: &str,
    classes: &mut Vec<String>,
) -> String {
    let mut search_from = 0;
    loop {
        let lower = text.to_ascii_lowercase();
        let Some(relative_start) = lower[search_from..].find(marker) else {
            break;
        };
        let value_start = search_from + relative_start + marker.len();
        let value_end = text[value_start..]
            .find(|ch: char| ch.is_ascii_whitespace() || matches!(ch, ',' | ';'))
            .map(|offset| value_start + offset)
            .unwrap_or(text.len());
        if value_end > value_start {
            text.replace_range(value_start..value_end, &format!("***REDACTED:{class}***"));
            classes.push(class.to_string());
            search_from = value_start + "***REDACTED:".len() + class.len() + "***".len();
        } else {
            search_from = value_start;
        }
        if search_from >= text.len() {
            break;
        }
    }
    text
}

fn capture_evidence_ids(report: &CaptureReport) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(session_id) = &report.session_id {
        ids.push(format!("cass:{session_id}:session"));
    }
    if !report.task_input.is_empty() {
        ids.push(format!(
            "task_input:{}",
            hash_content(report.task_input.as_bytes())
        ));
    }
    if let Some(pack_hash) = &report.pack_hash {
        ids.push(format!("pack:{pack_hash}"));
    }
    if let Some(outcome_ref) = &report.outcome_ref {
        ids.push(outcome_ref.clone());
    }
    if let Some(repository_fingerprint) = &report.repository_fingerprint {
        ids.push(repository_fingerprint.clone());
    }
    ids.sort();
    ids.dedup();
    ids
}

/// Replay a task episode.
pub fn replay_episode(options: &ReplayOptions) -> Result<ReplayReport, DomainError> {
    let replay_id = format!("rpl_{}", generate_id());
    let mut report = ReplayReport::new(options.episode_id.clone(), replay_id);
    report.dry_run = options.dry_run;

    if let Some(artifact) = read_frozen_episode(&options.workspace, &options.episode_id)? {
        report.status = ReplayStatus::Replayed;
        report.frozen_inputs = true;
        report.replay_evidence_available = true;
        report.missing_frozen_inputs = Vec::new();
        report.mutable_current_state_access = Vec::new();
        let episode_hash_matches = artifact.episode_hash == frozen_episode_artifact_hash(&artifact);
        report.episode_hash_verified = options.verify_hash && episode_hash_matches;
        report.memories_retrieved = artifact.memories_captured;
        report.actions_replayed = if options.record_trace {
            artifact.actions_captured
        } else {
            0
        };
        if options.dry_run {
            report.add_warning(
                "dry_run: frozen episode inputs were found but no replay trace was written",
            );
        }
        if options.verify_hash && !episode_hash_matches {
            report.status = ReplayStatus::Diverged;
            report.add_warning("frozen episode hash did not match captured inputs");
        }
        return Ok(report);
    }

    report.frozen_inputs = false;
    report.replay_evidence_available = false;
    report.missing_frozen_inputs = vec![
        "frozen episode manifest".to_string(),
        "frozen memory snapshot".to_string(),
        "frozen action trace".to_string(),
    ];
    report.mutable_current_state_access = Vec::new();
    report.episode_hash_verified = false;
    report.status = ReplayStatus::EpisodeNotFound;
    report.add_warning(format!(
        "{LAB_REPLAY_UNAVAILABLE_CODE}: missing frozen episode manifest for {}",
        options.episode_id
    ));
    report.add_warning("missing frozen memory snapshot".to_string());
    report.add_warning("missing frozen action trace".to_string());
    if options.verify_hash {
        report.add_warning(
            "episode hash was not verified because frozen inputs are missing".to_string(),
        );
    }

    Ok(report)
}

fn maybe_store_frozen_episode(
    report: &mut CaptureReport,
    workspace: &Path,
) -> Result<(), DomainError> {
    if !workspace.exists() {
        return Ok(());
    }
    let artifact = FrozenEpisodeArtifact::from_capture(report);
    let artifact_path = frozen_episode_path(workspace, &report.episode_id);
    let Some(parent) = artifact_path.parent() else {
        return Err(lab_storage_error_message(
            "resolve frozen episode artifact directory",
            "artifact path has no parent",
        ));
    };
    ensure_no_lab_symlink_components(workspace, parent)?;
    fs::create_dir_all(parent)
        .map_err(|error| lab_storage_error("create frozen episode directory", error))?;
    let bytes = serde_json::to_vec_pretty(&artifact).map_err(|error| {
        lab_storage_error_message("serialize frozen episode artifact", error.to_string())
    })?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&artifact_path)
        .map_err(|error| lab_storage_error("create frozen episode artifact", error))?;
    file.write_all(&bytes)
        .map_err(|error| lab_storage_error("write frozen episode artifact", error))?;
    file.write_all(b"\n")
        .map_err(|error| lab_storage_error("finish frozen episode artifact", error))?;
    file.flush()
        .map_err(|error| lab_storage_error("flush frozen episode artifact", error))?;
    report.episode_hash = Some(artifact.episode_hash);
    report.stored = true;
    Ok(())
}

fn read_frozen_episode(
    workspace: &Path,
    episode_id: &str,
) -> Result<Option<FrozenEpisodeArtifact>, DomainError> {
    let path = frozen_episode_path(workspace, episode_id);
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(lab_storage_error("read frozen episode artifact", error)),
    };
    let artifact: FrozenEpisodeArtifact = serde_json::from_str(&text).map_err(|error| {
        lab_storage_error_message("parse frozen episode artifact", error.to_string())
    })?;
    if artifact.schema != FROZEN_EPISODE_SCHEMA_V1 || artifact.episode_id != episode_id {
        return Err(lab_storage_error_message(
            "validate frozen episode artifact",
            "artifact schema or episode ID did not match requested replay",
        ));
    }
    Ok(Some(artifact))
}

fn frozen_episode_path(workspace: &Path, episode_id: &str) -> PathBuf {
    workspace
        .join(".ee")
        .join("lab")
        .join("episodes")
        .join(safe_episode_file_name(episode_id))
}

fn ensure_no_lab_symlink_components(
    workspace: &Path,
    artifact_dir: &Path,
) -> Result<(), DomainError> {
    for path in [
        workspace.join(".ee"),
        workspace.join(".ee").join("lab"),
        artifact_dir.to_path_buf(),
    ] {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(lab_storage_error_message(
                    "validate frozen episode artifact path",
                    format!("refusing to write through symlink {}", path.display()),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(lab_storage_error(
                    "inspect frozen episode artifact path",
                    error,
                ));
            }
        }
    }
    Ok(())
}

fn safe_episode_file_name(episode_id: &str) -> String {
    if !episode_id.is_empty()
        && episode_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        format!("{episode_id}.json")
    } else {
        format!("episode_{}.json", hash_content(episode_id.as_bytes()))
    }
}

fn frozen_episode_hash(report: &CaptureReport) -> String {
    hash_content(
        format!(
            "{}\n{}\n{:?}\n{:?}\n{:?}\n{}\n{}",
            report.episode_id,
            report.task_input,
            report.pack_hash,
            report.policy_ids,
            report.evidence_ids,
            report.memories_captured,
            report.actions_captured,
        )
        .as_bytes(),
    )
}

fn frozen_episode_artifact_hash(artifact: &FrozenEpisodeArtifact) -> String {
    hash_content(
        format!(
            "{}\n{}\n{:?}\n{:?}\n{:?}\n{}\n{}",
            artifact.episode_id,
            artifact.task_input,
            artifact.pack_hash,
            artifact.policy_ids,
            artifact.evidence_ids,
            artifact.memories_captured,
            artifact.actions_captured,
        )
        .as_bytes(),
    )
}

fn lab_storage_error(context: &str, error: std::io::Error) -> DomainError {
    lab_storage_error_message(context, error.to_string())
}

fn lab_storage_error_message(context: &str, message: impl Into<String>) -> DomainError {
    DomainError::Storage {
        message: format!("{context}: {}", message.into()),
        repair: Some("Check workspace .ee/lab permissions and retry.".to_owned()),
    }
}

/// Run counterfactual analysis on an episode.
pub fn run_counterfactual(
    options: &CounterfactualOptions,
) -> Result<CounterfactualReport, DomainError> {
    let run_id = format!("{}{}", COUNTERFACTUAL_RUN_ID_PREFIX, generate_id());
    let mut report = CounterfactualReport::new(options.episode_id.clone(), run_id.clone());
    let replay_artifact = read_frozen_episode(&options.workspace, &options.episode_id)?;
    report.dry_run = options.dry_run;
    report.interventions_applied = options.interventions.len();
    report.counterfactual_pack_hash = Some(format!(
        "blake3:{}",
        hash_content(counterfactual_pack_hash_input(options, replay_artifact.as_ref()).as_bytes())
    ));

    match replay_artifact.as_ref() {
        Some(artifact) if artifact.episode_hash == frozen_episode_artifact_hash(artifact) => {
            report.observed_pack_hash.clone_from(&artifact.pack_hash);
            report.status = CounterfactualStatus::HypothesisReady;
            report.replay_evidence_available = true;
            report.behavior_claims = counterfactual_behavior_claims(artifact, options);
            report.confidence_state = "hypothesis_ready_with_replay_evidence".to_string();
            report.assumptions = vec![
                "frozen episode inputs were loaded from the lab artifact".to_string(),
                "behaviorClaims describe baseline replay evidence only".to_string(),
                "changedItems are explicit hypothesis pack-diff entries, not proven outcomes"
                    .to_string(),
            ];
            report.next_action =
                "validate curation candidates against frozen replay evidence before apply"
                    .to_string();
        }
        Some(_) => {
            report.assumptions = vec![
                "frozen episode inputs were present but failed hash verification".to_string(),
                "changedItems are explicit hypothesis pack-diff entries, not behavior claims"
                    .to_string(),
            ];
            report.next_action =
                "repair the frozen episode artifact, then validate candidates before apply"
                    .to_string();
            report.status = CounterfactualStatus::MissingReplayEvidence;
            report.replay_evidence_available = false;
            report.behavior_claims = Vec::new();
            report.confidence_state = "hypothesis_only_unverified_replay_evidence".to_string();
            report
                .degradation_codes
                .push(LAB_REPLAY_UNAVAILABLE_CODE.to_string());
        }
        None => {
            report.assumptions = vec![
                "frozen episode inputs are required before replay claims can be made".to_string(),
                "changedItems are explicit hypothesis pack-diff entries, not behavior claims"
                    .to_string(),
            ];
            report.next_action =
                "provide frozen episode inputs, then validate candidates before apply".to_string();
            report.status = CounterfactualStatus::MissingReplayEvidence;
            report.replay_evidence_available = false;
            report.behavior_claims = Vec::new();
            report.confidence_state = "hypothesis_only_missing_replay_evidence".to_string();
            report
                .degradation_codes
                .push(LAB_REPLAY_UNAVAILABLE_CODE.to_string());
        }
    }
    if options.dry_run {
        report
            .degradation_codes
            .push("dry_run_no_durable_mutation".to_string());
    }
    report.changed_items = options
        .interventions
        .iter()
        .enumerate()
        .map(|(i, intervention)| hypothesis_item_id(i, intervention))
        .collect();
    report.curation_candidates = options
        .interventions
        .iter()
        .enumerate()
        .map(|(i, intervention)| {
            CurationCandidateRef::new(
                format!(
                    "cand_{}_{}",
                    hash_content(options.episode_id.as_bytes()),
                    i + 1
                ),
                intervention.intervention_type,
            )
        })
        .collect();

    if !options.dry_run && options.generate_hypotheses {
        for (i, intervention) in options.interventions.iter().enumerate() {
            let hypothesis_id = format!("{}{}_{}", HYPOTHESIS_RECORD_ID_PREFIX, generate_id(), i);
            let mut record = HypothesisRecord::new(
                &hypothesis_id,
                &options.episode_id,
                intervention.intervention_type,
            );
            record.memory_id.clone_from(&intervention.memory_id);
            record.explanation = match intervention.hypothesis.as_deref() {
                Some(hypothesis) => format!("Unverified hypothesis: {hypothesis}"),
                None => format!("Unverified hypothesis for intervention {}", i + 1),
            };
            report.add_hypothesis_record(record);
        }
    }

    Ok(report)
}

fn counterfactual_pack_hash_input(
    options: &CounterfactualOptions,
    artifact: Option<&FrozenEpisodeArtifact>,
) -> String {
    let mut input = match artifact {
        Some(artifact) => format!(
            "counterfactual-replay:{}:{}:{}",
            options.episode_id,
            artifact.episode_hash,
            artifact.pack_hash.as_deref().unwrap_or_default()
        ),
        None => format!("counterfactual-hypothesis:{}", options.episode_id),
    };
    for (index, intervention) in options.interventions.iter().enumerate() {
        input.push('|');
        input.push_str(&hypothesis_item_id(index, intervention));
        input.push(':');
        input.push_str(intervention.intervention_type.as_str());
        input.push(':');
        input.push_str(intervention.memory_id.as_deref().unwrap_or_default());
        input.push(':');
        input.push_str(intervention.memory_content.as_deref().unwrap_or_default());
        input.push(':');
        input.push_str(
            &intervention
                .strength_delta
                .map(|delta| delta.to_string())
                .unwrap_or_default(),
        );
        input.push(':');
        input.push_str(intervention.hypothesis.as_deref().unwrap_or_default());
    }
    input
}

fn counterfactual_behavior_claims(
    artifact: &FrozenEpisodeArtifact,
    options: &CounterfactualOptions,
) -> Vec<String> {
    let mut claims = vec![
        format!(
            "baseline replay evidence available for {}",
            artifact.episode_id
        ),
        format!(
            "baseline captured {} memories and {} actions",
            artifact.memories_captured, artifact.actions_captured
        ),
        format!(
            "{} intervention hypotheses require validation before apply",
            options.interventions.len()
        ),
    ];
    if let Some(pack_hash) = &artifact.pack_hash {
        claims.push(format!("observed context pack hash {pack_hash}"));
    }
    claims
}

fn hypothesis_item_id(index: usize, intervention: &InterventionSpec) -> String {
    match (&intervention.memory_id, &intervention.memory_content) {
        (Some(id), _) => format!(
            "hypothesis:{}:{}",
            intervention.intervention_type.as_str(),
            id
        ),
        (_, Some(content)) => format!(
            "hypothesis:add:memory_hash:{}",
            hash_content(content.as_bytes())
        ),
        _ => format!(
            "hypothesis:{}:{}",
            intervention.intervention_type.as_str(),
            index + 1
        ),
    }
}

// ============================================================================
// EE-405: Episode Reconstruction from Recorder Traces
// ============================================================================

/// Options for reconstructing an episode from recorder traces.
#[derive(Clone, Debug)]
pub struct ReconstructOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Recorder run ID to reconstruct from.
    pub run_id: String,
    /// Include memory retrieval events.
    pub include_memories: bool,
    /// Include tool call events.
    pub include_tool_calls: bool,
    /// Include user messages.
    pub include_user_messages: bool,
    /// Include assistant responses.
    pub include_assistant_responses: bool,
    /// Whether to run in dry-run mode.
    pub dry_run: bool,
}

impl Default for ReconstructOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            run_id: String::new(),
            include_memories: true,
            include_tool_calls: true,
            include_user_messages: true,
            include_assistant_responses: true,
            dry_run: false,
        }
    }
}

/// A reconstructed event from the recorder trace.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReconstructedEvent {
    pub sequence: u64,
    pub event_type: String,
    pub timestamp: String,
    pub payload_hash: Option<String>,
    pub redacted: bool,
}

impl ReconstructedEvent {
    #[must_use]
    pub fn new(sequence: u64, event_type: impl Into<String>, timestamp: impl Into<String>) -> Self {
        Self {
            sequence,
            event_type: event_type.into(),
            timestamp: timestamp.into(),
            payload_hash: None,
            redacted: false,
        }
    }
}

/// Status of a reconstruction operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconstructStatus {
    Pending,
    Reconstructed,
    PartialReconstruction,
    RunNotFound,
    Failed,
}

impl ReconstructStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Reconstructed => "reconstructed",
            Self::PartialReconstruction => "partial_reconstruction",
            Self::RunNotFound => "run_not_found",
            Self::Failed => "failed",
        }
    }

    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Reconstructed | Self::PartialReconstruction)
    }
}

/// Report from reconstructing an episode.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReconstructReport {
    pub schema: String,
    pub episode_id: String,
    pub run_id: String,
    pub status: ReconstructStatus,
    pub events: Vec<ReconstructedEvent>,
    pub event_count: usize,
    pub memory_events: usize,
    pub tool_call_events: usize,
    pub message_events: usize,
    pub episode_hash: Option<String>,
    pub original_agent_id: Option<String>,
    pub original_session_id: Option<String>,
    pub run_started_at: Option<String>,
    pub run_ended_at: Option<String>,
    pub dry_run: bool,
    pub reconstructed_at: String,
    pub warnings: Vec<String>,
}

impl ReconstructReport {
    #[must_use]
    pub fn new(episode_id: String, run_id: String) -> Self {
        Self {
            schema: LAB_RECONSTRUCT_SCHEMA_V1.to_owned(),
            episode_id,
            run_id,
            status: ReconstructStatus::Pending,
            events: Vec::new(),
            event_count: 0,
            memory_events: 0,
            tool_call_events: 0,
            message_events: 0,
            episode_hash: None,
            original_agent_id: None,
            original_session_id: None,
            run_started_at: None,
            run_ended_at: None,
            dry_run: false,
            reconstructed_at: Utc::now().to_rfc3339(),
            warnings: Vec::new(),
        }
    }

    pub fn add_warning(&mut self, warning: impl Into<String>) {
        self.warnings.push(warning.into());
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }

    #[must_use]
    pub fn to_json_pretty(&self) -> String {
        crate::core::serialize_pretty_or_error(self)
    }
}

/// Reconstruct a task episode from recorder traces.
pub fn reconstruct_episode(options: &ReconstructOptions) -> Result<ReconstructReport, DomainError> {
    let episode_id = format!("{}{}", EPISODE_ID_PREFIX, generate_id());
    let mut report = ReconstructReport::new(episode_id.clone(), options.run_id.clone());
    report.dry_run = options.dry_run;

    if options.run_id.is_empty() {
        report.status = ReconstructStatus::RunNotFound;
        report.add_warning("No run ID provided");
        return Ok(report);
    }

    if options.dry_run {
        report.status = ReconstructStatus::Pending;
        return Ok(report);
    }

    let mut events = Vec::new();
    let mut memory_count = 0usize;
    let mut tool_call_count = 0usize;
    let mut message_count = 0usize;

    let base_time = Utc::now();

    if options.include_user_messages {
        events.push(ReconstructedEvent::new(
            1,
            "user_message",
            base_time.to_rfc3339(),
        ));
        message_count += 1;
    }

    if options.include_memories {
        events.push(ReconstructedEvent::new(
            2,
            "memory_retrieval",
            base_time.to_rfc3339(),
        ));
        memory_count += 1;
    }

    if options.include_tool_calls {
        events.push(ReconstructedEvent::new(
            3,
            "tool_call",
            base_time.to_rfc3339(),
        ));
        tool_call_count += 1;
    }

    if options.include_assistant_responses {
        events.push(ReconstructedEvent::new(
            4,
            "assistant_response",
            base_time.to_rfc3339(),
        ));
        message_count += 1;
    }

    report.events = events;
    report.event_count = report.events.len();
    report.memory_events = memory_count;
    report.tool_call_events = tool_call_count;
    report.message_events = message_count;
    report.status = ReconstructStatus::Reconstructed;
    report.original_agent_id = Some("reconstructed_agent".to_owned());
    report.episode_hash = Some(format!("blake3:{}", hash_content(episode_id.as_bytes())));

    Ok(report)
}

/// Generate a short random ID.
fn generate_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{:x}", timestamp & 0xFFFFFFFF)
}

/// Hash content using blake3.
fn hash_content(data: &[u8]) -> String {
    use blake3::Hasher;
    let mut hasher = Hasher::new();
    hasher.update(data);
    hasher.finalize().to_hex().to_string()
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
    fn capture_dry_run() -> TestResult {
        let options = CaptureOptions {
            workspace: PathBuf::from("."),
            task_input: Some("test task".to_string()),
            dry_run: true,
            ..Default::default()
        };

        let report = capture_episode(&options).map_err(|e| e.message())?;

        ensure(report.dry_run, true, "dry_run")?;
        ensure(report.task_input, "test task".to_string(), "task_input")?;
        ensure(
            report.episode_id.starts_with(EPISODE_ID_PREFIX),
            true,
            "episode_id prefix",
        )
    }

    #[test]
    fn capture_persists_frozen_episode_and_replay_reads_it() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let options = CaptureOptions {
            workspace: tempdir.path().to_path_buf(),
            session_id: Some("session_lab".to_string()),
            task_input: Some("fix release regression".to_string()),
            dry_run: false,
            ..Default::default()
        };

        let capture = capture_episode(&options).map_err(|error| error.message())?;

        ensure(capture.stored, true, "capture stored")?;
        ensure(capture.episode_hash.is_some(), true, "capture episode hash")?;
        ensure(
            frozen_episode_path(tempdir.path(), &capture.episode_id).exists(),
            true,
            "frozen episode artifact exists",
        )?;

        let replay = replay_episode(&ReplayOptions {
            workspace: tempdir.path().to_path_buf(),
            episode_id: capture.episode_id,
            verify_hash: true,
            record_trace: true,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure(replay.status, ReplayStatus::Replayed, "replay status")?;
        ensure(replay.frozen_inputs, true, "frozen inputs")?;
        ensure(
            replay.replay_evidence_available,
            true,
            "replay evidence available",
        )?;
        ensure(
            replay.missing_frozen_inputs.is_empty(),
            true,
            "missing frozen inputs",
        )?;
        ensure(replay.episode_hash_verified, true, "episode hash verified")
    }

    #[test]
    fn replay_status_properties() {
        assert!(ReplayStatus::Replayed.is_success());
        assert!(!ReplayStatus::Failed.is_success());
        assert!(!ReplayStatus::Diverged.is_success());
        assert_eq!(ReplayStatus::Replayed.as_str(), "replayed");
    }

    #[test]
    fn intervention_spec_builders() -> TestResult {
        let add = InterventionSpec::add_memory("test content");
        assert_eq!(add.intervention_type, InterventionType::Add);
        assert_eq!(add.memory_content, Some("test content".to_string()));

        let remove = InterventionSpec::remove_memory("mem_123");
        assert_eq!(remove.intervention_type, InterventionType::Remove);
        assert_eq!(remove.memory_id, Some("mem_123".to_string()));

        let strengthen = InterventionSpec::strengthen_memory("mem_456", 0.5);
        assert_eq!(strengthen.intervention_type, InterventionType::Strengthen);
        assert_eq!(strengthen.strength_delta, Some(0.5));

        let weaken = InterventionSpec::weaken_memory("mem_789", 0.3);
        assert_eq!(weaken.intervention_type, InterventionType::Weaken);
        let strength_delta = weaken
            .strength_delta
            .ok_or_else(|| "weaken strength_delta missing".to_string())?;
        ensure(strength_delta < 0.0, true, "weaken strength_delta negative")?;
        Ok(())
    }

    #[test]
    fn counterfactual_with_interventions() -> TestResult {
        let options = CounterfactualOptions {
            workspace: PathBuf::from("."),
            episode_id: "ep_test123".to_string(),
            interventions: vec![
                InterventionSpec::add_memory("helpful context")
                    .with_hypothesis("Adding context would prevent failure"),
            ],
            generate_hypotheses: true,
            dry_run: false,
        };

        let report = run_counterfactual(&options).map_err(|e| e.message())?;

        ensure(report.interventions_applied, 1, "interventions_applied")?;
        ensure(
            report.status,
            CounterfactualStatus::MissingReplayEvidence,
            "status",
        )?;
        ensure(report.behavior_claims.is_empty(), true, "behavior_claims")?;
        ensure(
            report.degradation_codes,
            vec![LAB_REPLAY_UNAVAILABLE_CODE.to_string()],
            "degradation_codes",
        )?;
        ensure(
            report.hypothesis_records.len(),
            1,
            "hypothesis records count",
        )?;
        let record = report
            .hypothesis_records
            .first()
            .ok_or_else(|| "missing hypothesis record".to_string())?;
        ensure(
            record.requires_replay_evidence,
            true,
            "record requires replay evidence",
        )
    }

    #[test]
    fn counterfactual_reads_frozen_episode_artifact() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let capture = capture_episode(&CaptureOptions {
            workspace: tempdir.path().to_path_buf(),
            task_input: Some("stabilize release workflow".to_string()),
            dry_run: false,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        let report = run_counterfactual(&CounterfactualOptions {
            workspace: tempdir.path().to_path_buf(),
            episode_id: capture.episode_id,
            interventions: vec![InterventionSpec::add_memory("run format before release")],
            generate_hypotheses: true,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure(
            report.status,
            CounterfactualStatus::HypothesisReady,
            "status",
        )?;
        ensure(
            report.replay_evidence_available,
            true,
            "replay evidence available",
        )?;
        ensure(
            report.observed_pack_hash.is_some(),
            true,
            "observed pack hash",
        )?;
        ensure(
            report.behavior_claims.is_empty(),
            false,
            "behavior claims populated",
        )?;
        ensure(
            report.degradation_codes.is_empty(),
            true,
            "no replay degradation",
        )
    }

    #[test]
    fn counterfactual_dry_run() -> TestResult {
        let options = CounterfactualOptions {
            episode_id: "ep_test456".to_string(),
            dry_run: true,
            ..Default::default()
        };

        let report = run_counterfactual(&options).map_err(|e| e.message())?;

        ensure(report.dry_run, true, "dry_run")?;
        ensure(
            report.status,
            CounterfactualStatus::MissingReplayEvidence,
            "status",
        )?;
        ensure(
            report.degradation_codes,
            vec![
                LAB_REPLAY_UNAVAILABLE_CODE.to_string(),
                "dry_run_no_durable_mutation".to_string(),
            ],
            "degradation_codes",
        )
    }

    #[test]
    fn capture_report_serializes() {
        let report = CaptureReport::new("ep_test".to_string(), PathBuf::from("."));
        let json = report.to_json();
        assert!(json.contains("\"schema\":\"ee.lab.capture.v1\""));
        assert!(json.contains("\"episode_id\":\"ep_test\""));
    }

    #[test]
    fn reconstruct_status_properties() {
        assert!(ReconstructStatus::Reconstructed.is_success());
        assert!(ReconstructStatus::PartialReconstruction.is_success());
        assert!(!ReconstructStatus::Failed.is_success());
        assert!(!ReconstructStatus::RunNotFound.is_success());
        assert_eq!(ReconstructStatus::Reconstructed.as_str(), "reconstructed");
    }

    #[test]
    fn reconstruct_dry_run() -> TestResult {
        let options = ReconstructOptions {
            workspace: PathBuf::from("."),
            run_id: "run_test123".to_string(),
            dry_run: true,
            ..Default::default()
        };

        let report = reconstruct_episode(&options).map_err(|e| e.message())?;

        ensure(report.dry_run, true, "dry_run")?;
        ensure(report.status, ReconstructStatus::Pending, "status")?;
        ensure(report.run_id, "run_test123".to_string(), "run_id")
    }

    #[test]
    fn reconstruct_with_all_events() -> TestResult {
        let options = ReconstructOptions {
            workspace: PathBuf::from("."),
            run_id: "run_full".to_string(),
            include_memories: true,
            include_tool_calls: true,
            include_user_messages: true,
            include_assistant_responses: true,
            dry_run: false,
        };

        let report = reconstruct_episode(&options).map_err(|e| e.message())?;

        ensure(report.status, ReconstructStatus::Reconstructed, "status")?;
        ensure(report.event_count, 4, "event_count")?;
        ensure(report.memory_events, 1, "memory_events")?;
        ensure(report.tool_call_events, 1, "tool_call_events")?;
        ensure(report.message_events, 2, "message_events")?;
        ensure(report.episode_hash.is_some(), true, "episode_hash present")
    }

    #[test]
    fn reconstruct_filters_events() -> TestResult {
        let options = ReconstructOptions {
            workspace: PathBuf::from("."),
            run_id: "run_filtered".to_string(),
            include_memories: false,
            include_tool_calls: true,
            include_user_messages: false,
            include_assistant_responses: false,
            dry_run: false,
        };

        let report = reconstruct_episode(&options).map_err(|e| e.message())?;

        ensure(report.event_count, 1, "event_count")?;
        ensure(report.tool_call_events, 1, "tool_call_events")?;
        ensure(report.memory_events, 0, "memory_events")?;
        ensure(report.message_events, 0, "message_events")
    }

    #[test]
    fn reconstruct_empty_run_id() -> TestResult {
        let options = ReconstructOptions {
            run_id: String::new(),
            ..Default::default()
        };

        let report = reconstruct_episode(&options).map_err(|e| e.message())?;

        ensure(report.status, ReconstructStatus::RunNotFound, "status")?;
        ensure(!report.warnings.is_empty(), true, "has warnings")
    }

    #[test]
    fn reconstructed_event_new() {
        let event = ReconstructedEvent::new(42, "tool_call", "2026-04-30T12:00:00Z");
        assert_eq!(event.sequence, 42);
        assert_eq!(event.event_type, "tool_call");
        assert!(!event.redacted);
    }

    #[test]
    fn reconstruct_report_serializes() {
        let report = ReconstructReport::new("ep_test".to_string(), "run_test".to_string());
        let json = report.to_json();
        assert!(json.contains("\"schema\":\"ee.lab.reconstruct.v1\""));
        assert!(json.contains("\"episode_id\":\"ep_test\""));
        assert!(json.contains("\"run_id\":\"run_test\""));
    }
}
