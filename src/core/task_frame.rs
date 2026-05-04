//! Durable, non-executing task frames and goal stacks.
//!
//! Task frames are passive state: they record goals, subgoals, blockers, focus
//! links, and evidence IDs for agent continuity. They never execute shell
//! commands, route tools, or mutate workspace source files.

use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::models::DomainError;

pub const TASK_FRAME_SCHEMA_V1: &str = "ee.task_frame.v1";
pub const TASK_FRAME_STORE_SCHEMA_V1: &str = "ee.task_frame.store.v1";
pub const TASK_FRAME_REPORT_SCHEMA_V1: &str = "ee.task_frame.report.v1";
pub const TASK_FRAME_ID_PREFIX: &str = "tf_";
pub const TASK_SUBGOAL_ID_PREFIX: &str = "tg_";
pub const NON_EXECUTING_CONTRACT: &str = "records task state only; never executes shell commands, plans tools, or mutates workspace files";
const REDACTION_PLACEHOLDER: &str = "***REDACTED***";
const SECRET_KEYS: &[&str] = &[
    "api_key",
    "apikey",
    "auth_token",
    "bearer_token",
    "client_secret",
    "database_url",
    "password",
    "passwd",
    "private_key",
    "secret",
    "ssh_key",
    "token",
];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskFrameStatus {
    Draft,
    #[default]
    Open,
    Active,
    Blocked,
    Completed,
    Abandoned,
}

impl TaskFrameStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Open => "open",
            Self::Active => "active",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::Abandoned => "abandoned",
        }
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Abandoned)
    }

    #[must_use]
    pub const fn is_active_candidate(self) -> bool {
        matches!(self, Self::Open | Self::Active | Self::Blocked)
    }

    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }
        match self {
            Self::Draft => matches!(next, Self::Open | Self::Abandoned),
            Self::Open => matches!(
                next,
                Self::Active | Self::Blocked | Self::Completed | Self::Abandoned
            ),
            Self::Active => matches!(
                next,
                Self::Open | Self::Blocked | Self::Completed | Self::Abandoned
            ),
            Self::Blocked => matches!(next, Self::Open | Self::Active | Self::Abandoned),
            Self::Completed | Self::Abandoned => false,
        }
    }
}

impl FromStr for TaskFrameStatus {
    type Err = DomainError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "draft" => Ok(Self::Draft),
            "open" => Ok(Self::Open),
            "active" => Ok(Self::Active),
            "blocked" => Ok(Self::Blocked),
            "completed" => Ok(Self::Completed),
            "abandoned" => Ok(Self::Abandoned),
            _ => Err(DomainError::Usage {
                message: format!("Unknown task-frame status `{raw}`."),
                repair: Some("Use draft|open|active|blocked|completed|abandoned.".to_owned()),
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskEvidenceLink {
    pub kind: String,
    pub id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSubgoal {
    pub id: String,
    pub parent_id: Option<String>,
    pub title: String,
    pub status: TaskFrameStatus,
    pub blockers: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskFrameRecord {
    pub schema: String,
    pub id: String,
    pub workspace_root: String,
    pub root_goal: String,
    pub status: TaskFrameStatus,
    pub actor: String,
    pub source: String,
    pub current_focus: Option<String>,
    pub blockers: Vec<String>,
    pub subgoals: Vec<TaskSubgoal>,
    pub evidence_links: Vec<TaskEvidenceLink>,
    pub suggested_commands: Vec<String>,
    pub redaction_status: String,
    pub non_executing_contract: String,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub close_reason: Option<String>,
}

impl TaskFrameRecord {
    #[must_use]
    pub fn active_subgoal_count(&self) -> usize {
        self.subgoals
            .iter()
            .filter(|subgoal| subgoal.status.is_active_candidate())
            .count()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskFrameStoreDocument {
    pub schema: String,
    pub frames: Vec<TaskFrameRecord>,
}

impl Default for TaskFrameStoreDocument {
    fn default() -> Self {
        Self {
            schema: TASK_FRAME_STORE_SCHEMA_V1.to_owned(),
            frames: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskFrameReport {
    pub schema: String,
    pub command: String,
    pub dry_run: bool,
    pub mutated: bool,
    pub store_path: String,
    pub frame: Option<TaskFrameRecord>,
    pub frames: Vec<TaskFrameRecord>,
    pub selected_subgoal: Option<TaskSubgoal>,
    pub operation: String,
    pub non_executing_contract: String,
}

impl TaskFrameReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|error| {
            serde_json::json!({
                "schema": TASK_FRAME_REPORT_SCHEMA_V1,
                "serializationError": error.to_string(),
            })
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskFrameCreateOptions {
    pub workspace_path: PathBuf,
    pub goal: String,
    pub actor: String,
    pub status: TaskFrameStatus,
    pub current_focus: Option<String>,
    pub blockers: Vec<String>,
    pub evidence_links: Vec<TaskEvidenceLink>,
    pub created_at: Option<String>,
    pub dry_run: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskFrameShowOptions {
    pub workspace_path: PathBuf,
    pub frame_id: Option<String>,
    pub active: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskFrameUpdateOptions {
    pub workspace_path: PathBuf,
    pub frame_id: String,
    pub status: Option<TaskFrameStatus>,
    pub current_focus: Option<String>,
    pub blockers: Vec<String>,
    pub evidence_links: Vec<TaskEvidenceLink>,
    pub updated_at: Option<String>,
    pub dry_run: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskFrameCloseOptions {
    pub workspace_path: PathBuf,
    pub frame_id: String,
    pub status: TaskFrameStatus,
    pub reason: String,
    pub closed_at: Option<String>,
    pub dry_run: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskSubgoalAddOptions {
    pub workspace_path: PathBuf,
    pub frame_id: String,
    pub parent_id: Option<String>,
    pub title: String,
    pub status: TaskFrameStatus,
    pub blockers: Vec<String>,
    pub created_at: Option<String>,
    pub dry_run: bool,
}

#[must_use]
pub fn task_frame_store_path(workspace_path: &Path) -> PathBuf {
    workspace_path.join(".ee").join("task_frames.json")
}

pub fn create_task_frame(options: &TaskFrameCreateOptions) -> Result<TaskFrameReport, DomainError> {
    let workspace_root = workspace_root_string(&options.workspace_path);
    ensure_non_empty("goal", &options.goal)?;
    ensure_non_empty("actor", &options.actor)?;
    if options.status.is_terminal() {
        return Err(DomainError::Usage {
            message: "New task frames must start in draft, open, active, or blocked state."
                .to_owned(),
            repair: Some("Use --status open, then close the frame explicitly.".to_owned()),
        });
    }

    let store_path = task_frame_store_path(&options.workspace_path);
    let mut store = read_store(&store_path)?;
    let now = options.created_at.clone().unwrap_or_else(now_rfc3339);
    let frame_id = stable_id(
        TASK_FRAME_ID_PREFIX,
        &[&workspace_root, &options.goal, &options.actor, &now],
    );
    if store.frames.iter().any(|frame| frame.id == frame_id) {
        return Err(DomainError::Usage {
            message: format!("Task frame already exists: {frame_id}"),
            repair: Some(format!("ee task-frame show {frame_id} --json")),
        });
    }

    let (root_goal, goal_redacted) = redact_task_text(options.goal.trim());
    let (current_focus, focus_redacted) = redact_optional_task_text(options.current_focus.clone());
    let (blockers, blockers_redacted) = redact_task_strings(&options.blockers);
    let redacted = goal_redacted || focus_redacted || blockers_redacted;

    let frame = TaskFrameRecord {
        schema: TASK_FRAME_SCHEMA_V1.to_owned(),
        id: frame_id,
        workspace_root,
        root_goal,
        status: options.status,
        actor: options.actor.trim().to_owned(),
        source: "ee task-frame create".to_owned(),
        current_focus,
        blockers,
        subgoals: Vec::new(),
        evidence_links: normalized_evidence_links(&options.evidence_links),
        suggested_commands: suggested_commands(None),
        redaction_status: redaction_status(redacted),
        non_executing_contract: NON_EXECUTING_CONTRACT.to_owned(),
        created_at: now.clone(),
        updated_at: now,
        closed_at: None,
        close_reason: None,
    };

    if !options.dry_run {
        store.frames.push(frame.clone());
        sort_frames(&mut store.frames);
        write_store(&store_path, &store)?;
    }

    Ok(report(
        "task-frame create",
        options.dry_run,
        !options.dry_run,
        &store_path,
        "create",
        Some(frame),
        Vec::new(),
        None,
    ))
}

pub fn show_task_frame(options: &TaskFrameShowOptions) -> Result<TaskFrameReport, DomainError> {
    let store_path = task_frame_store_path(&options.workspace_path);
    let store = read_store(&store_path)?;
    let frame = match (&options.frame_id, options.active) {
        (Some(frame_id), _) => find_frame(&store.frames, frame_id)?.clone(),
        (None, true) => select_active_frame(&store.frames)?.clone(),
        (None, false) => {
            return Ok(report(
                "task-frame show",
                false,
                false,
                &store_path,
                "list",
                None,
                store.frames,
                None,
            ));
        }
    };

    Ok(report(
        "task-frame show",
        false,
        false,
        &store_path,
        "show",
        Some(frame),
        Vec::new(),
        None,
    ))
}

pub fn update_task_frame(options: &TaskFrameUpdateOptions) -> Result<TaskFrameReport, DomainError> {
    let store_path = task_frame_store_path(&options.workspace_path);
    let mut store = read_store(&store_path)?;
    let index = find_frame_index(&store.frames, &options.frame_id)?;
    let mut frame = store.frames[index].clone();
    let now = options.updated_at.clone().unwrap_or_else(now_rfc3339);

    if let Some(next_status) = options.status {
        validate_transition(frame.status, next_status, "task frame")?;
        frame.status = next_status;
        if next_status.is_terminal() {
            frame.closed_at = Some(now.clone());
        }
    }
    let mut redacted = frame.redaction_status == "redacted";
    if options.current_focus.is_some() {
        let (current_focus, focus_redacted) =
            redact_optional_task_text(options.current_focus.clone());
        frame.current_focus = current_focus;
        redacted |= focus_redacted;
    }
    redacted |= append_unique_task_strings(&mut frame.blockers, &options.blockers);
    append_unique_evidence_links(&mut frame.evidence_links, &options.evidence_links);
    frame.redaction_status = redaction_status(redacted);
    frame.updated_at = now;
    frame.suggested_commands = suggested_commands(Some(&frame.id));

    if !options.dry_run {
        store.frames[index] = frame.clone();
        sort_frames(&mut store.frames);
        write_store(&store_path, &store)?;
    }

    Ok(report(
        "task-frame update",
        options.dry_run,
        !options.dry_run,
        &store_path,
        "update",
        Some(frame),
        Vec::new(),
        None,
    ))
}

pub fn close_task_frame(options: &TaskFrameCloseOptions) -> Result<TaskFrameReport, DomainError> {
    if !matches!(
        options.status,
        TaskFrameStatus::Completed | TaskFrameStatus::Abandoned
    ) {
        return Err(DomainError::Usage {
            message: "Closing a task frame requires completed or abandoned status.".to_owned(),
            repair: Some("Use --status completed or --status abandoned.".to_owned()),
        });
    }
    ensure_non_empty("reason", &options.reason)?;

    let store_path = task_frame_store_path(&options.workspace_path);
    let mut store = read_store(&store_path)?;
    let index = find_frame_index(&store.frames, &options.frame_id)?;
    let mut frame = store.frames[index].clone();
    validate_transition(frame.status, options.status, "task frame")?;
    let now = options.closed_at.clone().unwrap_or_else(now_rfc3339);
    frame.status = options.status;
    frame.updated_at = now.clone();
    frame.closed_at = Some(now);
    let (close_reason, reason_redacted) = redact_task_text(options.reason.trim());
    frame.close_reason = Some(close_reason);
    frame.redaction_status =
        redaction_status(frame.redaction_status == "redacted" || reason_redacted);
    frame.suggested_commands = suggested_commands(Some(&frame.id));

    if !options.dry_run {
        store.frames[index] = frame.clone();
        sort_frames(&mut store.frames);
        write_store(&store_path, &store)?;
    }

    Ok(report(
        "task-frame close",
        options.dry_run,
        !options.dry_run,
        &store_path,
        "close",
        Some(frame),
        Vec::new(),
        None,
    ))
}

pub fn add_task_subgoal(options: &TaskSubgoalAddOptions) -> Result<TaskFrameReport, DomainError> {
    ensure_non_empty("title", &options.title)?;
    if options.status.is_terminal() {
        return Err(DomainError::Usage {
            message: "New subgoals must start in draft, open, active, or blocked state.".to_owned(),
            repair: Some("Create the subgoal first, then close it explicitly.".to_owned()),
        });
    }

    let store_path = task_frame_store_path(&options.workspace_path);
    let mut store = read_store(&store_path)?;
    let index = find_frame_index(&store.frames, &options.frame_id)?;
    let mut frame = store.frames[index].clone();
    if frame.status.is_terminal() {
        return Err(DomainError::Usage {
            message: format!("Cannot add subgoals to terminal task frame `{}`.", frame.id),
            repair: Some("Create a new task frame for follow-up work.".to_owned()),
        });
    }
    if let Some(parent_id) = &options.parent_id {
        if !frame
            .subgoals
            .iter()
            .any(|subgoal| &subgoal.id == parent_id)
        {
            return Err(DomainError::NotFound {
                resource: "task subgoal".to_owned(),
                id: parent_id.clone(),
                repair: Some(format!("ee task-frame show {} --json", frame.id)),
            });
        }
    }

    let now = options.created_at.clone().unwrap_or_else(now_rfc3339);
    let (title, title_redacted) = redact_task_text(options.title.trim());
    let (blockers, blockers_redacted) = redact_task_strings(&options.blockers);
    let subgoal = TaskSubgoal {
        id: stable_id(TASK_SUBGOAL_ID_PREFIX, &[&frame.id, &options.title, &now]),
        parent_id: normalize_optional(options.parent_id.clone()),
        title,
        status: options.status,
        blockers,
        created_at: now.clone(),
        updated_at: now.clone(),
        closed_at: None,
    };
    frame.subgoals.push(subgoal.clone());
    frame.subgoals.sort_by(|left, right| {
        left.parent_id
            .cmp(&right.parent_id)
            .then(left.created_at.cmp(&right.created_at))
            .then(left.id.cmp(&right.id))
    });
    frame.updated_at = now;
    frame.suggested_commands = suggested_commands(Some(&frame.id));
    frame.redaction_status = redaction_status(
        frame.redaction_status == "redacted" || title_redacted || blockers_redacted,
    );

    if !options.dry_run {
        store.frames[index] = frame.clone();
        sort_frames(&mut store.frames);
        write_store(&store_path, &store)?;
    }

    Ok(report(
        "task-frame subgoal add",
        options.dry_run,
        !options.dry_run,
        &store_path,
        "subgoal_add",
        Some(frame),
        Vec::new(),
        Some(subgoal),
    ))
}

fn read_store(store_path: &Path) -> Result<TaskFrameStoreDocument, DomainError> {
    if !store_path.exists() {
        return Ok(TaskFrameStoreDocument::default());
    }
    let text = fs::read_to_string(store_path).map_err(|error| DomainError::Storage {
        message: format!(
            "Failed to read task-frame store `{}`: {error}",
            store_path.display()
        ),
        repair: Some("Check workspace .ee permissions.".to_owned()),
    })?;
    serde_json::from_str(&text).map_err(|error| DomainError::Storage {
        message: format!(
            "Failed to parse task-frame store `{}`: {error}",
            store_path.display()
        ),
        repair: Some("Inspect .ee/task_frames.json for malformed JSON.".to_owned()),
    })
}

fn write_store(store_path: &Path, store: &TaskFrameStoreDocument) -> Result<(), DomainError> {
    if let Some(parent) = store_path.parent() {
        fs::create_dir_all(parent).map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to create task-frame directory `{}`: {error}",
                parent.display()
            ),
            repair: Some("Check workspace .ee permissions.".to_owned()),
        })?;
    }
    let text = serde_json::to_string_pretty(store).map_err(|error| DomainError::Storage {
        message: format!("Failed to serialize task-frame store: {error}"),
        repair: Some("Report this serialization bug.".to_owned()),
    })? + "\n";
    fs::write(store_path, text).map_err(|error| DomainError::Storage {
        message: format!(
            "Failed to write task-frame store `{}`: {error}",
            store_path.display()
        ),
        repair: Some("Check workspace .ee permissions.".to_owned()),
    })
}

#[expect(clippy::too_many_arguments)]
fn report(
    command: &str,
    dry_run: bool,
    mutated: bool,
    store_path: &Path,
    operation: &str,
    frame: Option<TaskFrameRecord>,
    frames: Vec<TaskFrameRecord>,
    selected_subgoal: Option<TaskSubgoal>,
) -> TaskFrameReport {
    TaskFrameReport {
        schema: TASK_FRAME_REPORT_SCHEMA_V1.to_owned(),
        command: command.to_owned(),
        dry_run,
        mutated,
        store_path: store_path.display().to_string(),
        frame,
        frames,
        selected_subgoal,
        operation: operation.to_owned(),
        non_executing_contract: NON_EXECUTING_CONTRACT.to_owned(),
    }
}

fn find_frame<'a>(
    frames: &'a [TaskFrameRecord],
    frame_id: &str,
) -> Result<&'a TaskFrameRecord, DomainError> {
    frames
        .iter()
        .find(|frame| frame.id == frame_id)
        .ok_or_else(|| DomainError::NotFound {
            resource: "task frame".to_owned(),
            id: frame_id.to_owned(),
            repair: Some("ee task-frame show --active --json".to_owned()),
        })
}

fn find_frame_index(frames: &[TaskFrameRecord], frame_id: &str) -> Result<usize, DomainError> {
    frames
        .iter()
        .position(|frame| frame.id == frame_id)
        .ok_or_else(|| DomainError::NotFound {
            resource: "task frame".to_owned(),
            id: frame_id.to_owned(),
            repair: Some("ee task-frame show --json".to_owned()),
        })
}

fn select_active_frame(frames: &[TaskFrameRecord]) -> Result<&TaskFrameRecord, DomainError> {
    let mut active = frames
        .iter()
        .filter(|frame| frame.status.is_active_candidate());
    let first = active.next().ok_or_else(|| DomainError::NotFound {
        resource: "active task frame".to_owned(),
        id: "active".to_owned(),
        repair: Some("ee task-frame create --goal \"...\" --json".to_owned()),
    })?;
    if active.next().is_some() {
        return Err(DomainError::Usage {
            message: "Multiple active task frames exist in this workspace.".to_owned(),
            repair: Some("Pass an explicit FRAME_ID to ee task-frame show.".to_owned()),
        });
    }
    Ok(first)
}

fn validate_transition(
    current: TaskFrameStatus,
    next: TaskFrameStatus,
    label: &str,
) -> Result<(), DomainError> {
    if current.can_transition_to(next) {
        Ok(())
    } else {
        Err(DomainError::PolicyDenied {
            message: format!(
                "Invalid {label} status transition: {} -> {}.",
                current.as_str(),
                next.as_str()
            ),
            repair: Some("Terminal task-frame states cannot be reopened in place.".to_owned()),
        })
    }
}

fn workspace_root_string(workspace_path: &Path) -> String {
    workspace_path.display().to_string()
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn stable_id(prefix: &str, parts: &[&str]) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    let digest = hasher.finalize().to_hex().to_string();
    format!("{prefix}{}", &digest[..26])
}

fn ensure_non_empty(field: &str, value: &str) -> Result<(), DomainError> {
    if value.trim().is_empty() {
        Err(DomainError::Usage {
            message: format!("Task-frame {field} must not be empty."),
            repair: Some(format!("Pass a non-empty --{field} value.")),
        })
    } else {
        Ok(())
    }
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value.and_then(|inner| {
        let trimmed = inner.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    })
}

#[expect(
    dead_code,
    reason = "utility prepared for future subgoal bulk operations"
)]
fn normalized_strings(values: &[String]) -> Vec<String> {
    let mut normalized = values
        .iter()
        .filter_map(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        })
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

#[expect(
    dead_code,
    reason = "utility prepared for future subgoal bulk operations"
)]
fn append_unique_strings(target: &mut Vec<String>, values: &[String]) {
    target.extend(normalized_strings(values));
    target.sort();
    target.dedup();
}

fn redact_task_text(value: &str) -> (String, bool) {
    let trimmed = value.trim();
    let (without_key_values, key_value_redacted) = redact_secret_key_values(trimmed);
    let (without_url_passwords, url_redacted) = redact_url_passwords(&without_key_values);
    (without_url_passwords, key_value_redacted || url_redacted)
}

fn redact_optional_task_text(value: Option<String>) -> (Option<String>, bool) {
    match normalize_optional(value) {
        Some(inner) => {
            let (redacted, changed) = redact_task_text(&inner);
            (Some(redacted), changed)
        }
        None => (None, false),
    }
}

fn redact_task_strings(values: &[String]) -> (Vec<String>, bool) {
    let mut changed = false;
    let mut redacted = values
        .iter()
        .filter_map(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                let (redacted, item_changed) = redact_task_text(trimmed);
                changed |= item_changed;
                Some(redacted)
            }
        })
        .collect::<Vec<_>>();
    redacted.sort();
    redacted.dedup();
    (redacted, changed)
}

fn append_unique_task_strings(target: &mut Vec<String>, values: &[String]) -> bool {
    let (redacted, changed) = redact_task_strings(values);
    target.extend(redacted);
    target.sort();
    target.dedup();
    changed
}

fn redaction_status(redacted: bool) -> String {
    if redacted {
        "redacted".to_owned()
    } else {
        "none".to_owned()
    }
}

fn redact_secret_key_values(input: &str) -> (String, bool) {
    let mut output = input.to_owned();
    let mut changed = false;

    for key in SECRET_KEYS {
        let mut search_start = 0;
        loop {
            let lower = output.to_ascii_lowercase();
            if search_start >= lower.len() {
                break;
            }
            let Some(relative) = lower[search_start..].find(key) else {
                break;
            };
            let key_start = search_start + relative;
            let key_end = key_start + key.len();
            if !is_key_boundary(lower.as_bytes(), key_start, key_end) {
                search_start = key_end;
                continue;
            }

            let Some((value_start, value_end)) = secret_value_range(&output, key_end) else {
                search_start = key_end;
                continue;
            };
            if value_start == value_end {
                search_start = key_end;
                continue;
            }
            output.replace_range(value_start..value_end, REDACTION_PLACEHOLDER);
            changed = true;
            search_start = value_start + REDACTION_PLACEHOLDER.len();
        }
    }

    (output, changed)
}

fn is_key_boundary(bytes: &[u8], start: usize, end: usize) -> bool {
    let before_ok = start == 0
        || bytes
            .get(start.saturating_sub(1))
            .is_none_or(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_');
    let after_ok = bytes
        .get(end)
        .is_none_or(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_');
    before_ok && after_ok
}

fn secret_value_range(input: &str, key_end: usize) -> Option<(usize, usize)> {
    let mut cursor = key_end;
    cursor = skip_ascii_spaces(input, cursor);
    let separator = input.as_bytes().get(cursor).copied()?;
    if !matches!(separator, b'=' | b':') {
        return None;
    }
    cursor += 1;
    cursor = skip_ascii_spaces(input, cursor);
    if cursor >= input.len() {
        return None;
    }

    let quote = input.as_bytes().get(cursor).copied();
    if matches!(quote, Some(b'"' | b'\'')) {
        let quote = quote?;
        let value_start = cursor + 1;
        let value_end = input[value_start..]
            .bytes()
            .position(|byte| byte == quote)
            .map_or(input.len(), |relative| value_start + relative);
        return Some((value_start, value_end));
    }

    let value_end = input[cursor..]
        .char_indices()
        .find_map(|(offset, ch)| {
            if ch.is_whitespace() || matches!(ch, ',' | ';' | '&') {
                Some(cursor + offset)
            } else {
                None
            }
        })
        .unwrap_or(input.len());
    Some((cursor, value_end))
}

fn skip_ascii_spaces(input: &str, mut cursor: usize) -> usize {
    while matches!(input.as_bytes().get(cursor), Some(b' ' | b'\t')) {
        cursor += 1;
    }
    cursor
}

fn redact_url_passwords(input: &str) -> (String, bool) {
    let mut output = input.to_owned();
    let mut changed = false;
    let mut search_start = 0;

    loop {
        if search_start >= output.len() {
            break;
        }
        let lower = output.to_ascii_lowercase();
        let Some(relative_scheme) = lower[search_start..].find("://") else {
            break;
        };
        let scheme_marker = search_start + relative_scheme + 3;
        let segment_end = output[scheme_marker..]
            .char_indices()
            .find_map(|(offset, ch)| ch.is_whitespace().then_some(scheme_marker + offset))
            .unwrap_or(output.len());
        let Some(at_relative) = output[scheme_marker..segment_end].find('@') else {
            search_start = segment_end;
            continue;
        };
        let at_index = scheme_marker + at_relative;
        let Some(colon_relative) = output[scheme_marker..at_index].rfind(':') else {
            search_start = at_index + 1;
            continue;
        };
        let value_start = scheme_marker + colon_relative + 1;
        if value_start < at_index {
            output.replace_range(value_start..at_index, REDACTION_PLACEHOLDER);
            changed = true;
            search_start = value_start + REDACTION_PLACEHOLDER.len();
        } else {
            search_start = at_index + 1;
        }
    }

    (output, changed)
}

fn normalized_evidence_links(values: &[TaskEvidenceLink]) -> Vec<TaskEvidenceLink> {
    let mut normalized = values
        .iter()
        .filter_map(|link| {
            let kind = link.kind.trim();
            let id = link.id.trim();
            if kind.is_empty() || id.is_empty() {
                None
            } else {
                Some(TaskEvidenceLink {
                    kind: kind.to_owned(),
                    id: id.to_owned(),
                })
            }
        })
        .collect::<Vec<_>>();
    normalized.sort_by(|left, right| left.kind.cmp(&right.kind).then(left.id.cmp(&right.id)));
    normalized.dedup();
    normalized
}

fn append_unique_evidence_links(target: &mut Vec<TaskEvidenceLink>, values: &[TaskEvidenceLink]) {
    target.extend(normalized_evidence_links(values));
    target.sort_by(|left, right| left.kind.cmp(&right.kind).then(left.id.cmp(&right.id)));
    target.dedup();
}

fn sort_frames(frames: &mut [TaskFrameRecord]) {
    frames.sort_by(|left, right| {
        left.status
            .as_str()
            .cmp(right.status.as_str())
            .then(left.created_at.cmp(&right.created_at))
            .then(left.id.cmp(&right.id))
    });
}

fn suggested_commands(frame_id: Option<&str>) -> Vec<String> {
    match frame_id {
        Some(id) => vec![
            format!("ee task-frame show {id} --json"),
            format!("ee focus show --json --task-frame-id {id}"),
            format!("ee handoff resume --task-frame-id {id} --json"),
        ],
        None => vec![
            "ee task-frame show --active --json".to_owned(),
            "ee plan recipe list --json".to_owned(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn temp_workspace(name: &str) -> Result<PathBuf, String> {
        let root = std::env::temp_dir().join(format!(
            "ee-task-frame-{name}-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        Ok(root)
    }

    fn create_options(workspace_path: PathBuf) -> TaskFrameCreateOptions {
        TaskFrameCreateOptions {
            workspace_path,
            goal: "Ship task frame support".to_owned(),
            actor: "cod-pane6".to_owned(),
            status: TaskFrameStatus::Open,
            current_focus: Some("wire CLI".to_owned()),
            blockers: vec!["mail unavailable".to_owned()],
            evidence_links: vec![TaskEvidenceLink {
                kind: "bead".to_owned(),
                id: "eidetic_engine_cli-swx.3".to_owned(),
            }],
            created_at: Some("2026-05-04T00:00:00Z".to_owned()),
            dry_run: false,
        }
    }

    #[test]
    fn create_frame_persists_non_executing_record() -> TestResult {
        let workspace = temp_workspace("create")?;
        let report =
            create_task_frame(&create_options(workspace.clone())).map_err(|e| e.message())?;
        let frame = report.frame.ok_or_else(|| "missing frame".to_owned())?;
        assert_eq!(frame.schema, TASK_FRAME_SCHEMA_V1);
        assert_eq!(frame.status, TaskFrameStatus::Open);
        assert!(
            frame
                .non_executing_contract
                .contains("never executes shell commands")
        );
        assert!(task_frame_store_path(&workspace).exists());
        Ok(())
    }

    #[test]
    fn dry_run_does_not_write_store() -> TestResult {
        let workspace = temp_workspace("dry-run")?;
        let mut options = create_options(workspace.clone());
        options.dry_run = true;
        let report = create_task_frame(&options).map_err(|e| e.message())?;
        assert!(report.dry_run);
        assert!(!report.mutated);
        assert!(!task_frame_store_path(&workspace).exists());
        Ok(())
    }

    #[test]
    fn nested_subgoals_preserve_parent_relation() -> TestResult {
        let workspace = temp_workspace("subgoals")?;
        let created =
            create_task_frame(&create_options(workspace.clone())).map_err(|e| e.message())?;
        let frame_id = created.frame.ok_or_else(|| "missing frame".to_owned())?.id;
        let parent = add_task_subgoal(&TaskSubgoalAddOptions {
            workspace_path: workspace.clone(),
            frame_id: frame_id.clone(),
            parent_id: None,
            title: "Define schema".to_owned(),
            status: TaskFrameStatus::Open,
            blockers: Vec::new(),
            created_at: Some("2026-05-04T00:01:00Z".to_owned()),
            dry_run: false,
        })
        .map_err(|e| e.message())?
        .selected_subgoal
        .ok_or_else(|| "missing parent subgoal".to_owned())?;
        let child = add_task_subgoal(&TaskSubgoalAddOptions {
            workspace_path: workspace,
            frame_id,
            parent_id: Some(parent.id.clone()),
            title: "Add stable JSON".to_owned(),
            status: TaskFrameStatus::Blocked,
            blockers: vec!["needs CLI wiring".to_owned()],
            created_at: Some("2026-05-04T00:02:00Z".to_owned()),
            dry_run: false,
        })
        .map_err(|e| e.message())?
        .selected_subgoal
        .ok_or_else(|| "missing child subgoal".to_owned())?;
        assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));
        assert_eq!(child.status, TaskFrameStatus::Blocked);
        Ok(())
    }

    #[test]
    fn terminal_frame_cannot_be_reopened() -> TestResult {
        let workspace = temp_workspace("terminal")?;
        let created =
            create_task_frame(&create_options(workspace.clone())).map_err(|e| e.message())?;
        let frame_id = created.frame.ok_or_else(|| "missing frame".to_owned())?.id;
        close_task_frame(&TaskFrameCloseOptions {
            workspace_path: workspace.clone(),
            frame_id: frame_id.clone(),
            status: TaskFrameStatus::Completed,
            reason: "done".to_owned(),
            closed_at: Some("2026-05-04T00:03:00Z".to_owned()),
            dry_run: false,
        })
        .map_err(|e| e.message())?;
        let error = match update_task_frame(&TaskFrameUpdateOptions {
            workspace_path: workspace,
            frame_id,
            status: Some(TaskFrameStatus::Active),
            current_focus: None,
            blockers: Vec::new(),
            evidence_links: Vec::new(),
            updated_at: Some("2026-05-04T00:04:00Z".to_owned()),
            dry_run: false,
        }) {
            Ok(_) => return Err("terminal transition should be rejected".to_owned()),
            Err(error) => error,
        };
        assert_eq!(error.code(), "policy_denied");
        Ok(())
    }

    #[test]
    fn active_selection_reports_ambiguous_scope() -> TestResult {
        let workspace = temp_workspace("ambiguous")?;
        create_task_frame(&create_options(workspace.clone())).map_err(|e| e.message())?;
        let mut second = create_options(workspace.clone());
        second.goal = "Ship another frame".to_owned();
        second.created_at = Some("2026-05-04T00:05:00Z".to_owned());
        create_task_frame(&second).map_err(|e| e.message())?;
        let error = match show_task_frame(&TaskFrameShowOptions {
            workspace_path: workspace,
            frame_id: None,
            active: true,
        }) {
            Ok(_) => return Err("multiple active frames should be ambiguous".to_owned()),
            Err(error) => error,
        };
        assert_eq!(error.code(), "usage");
        Ok(())
    }

    #[test]
    fn task_frame_redacts_secret_like_values_before_persisting() -> TestResult {
        let workspace = temp_workspace("redaction")?;
        let mut options = create_options(workspace.clone());
        options.goal = "Rotate api_key=sk-live-123 before release".to_owned();
        options.current_focus =
            Some("Check DATABASE_URL=postgres://user:hunter2@example.test/db".to_owned());
        options.blockers = vec![
            "needs password='open-sesame' from operator".to_owned(),
            "token: ghp_secret_token".to_owned(),
        ];
        let report = create_task_frame(&options).map_err(|e| e.message())?;
        let frame = report.frame.ok_or_else(|| "missing frame".to_owned())?;
        let serialized = serde_json::to_string(&frame).map_err(|error| error.to_string())?;

        assert_eq!(frame.redaction_status, "redacted");
        assert!(serialized.contains(REDACTION_PLACEHOLDER));
        assert!(!serialized.contains("sk-live-123"));
        assert!(!serialized.contains("hunter2"));
        assert!(!serialized.contains("open-sesame"));
        assert!(!serialized.contains("ghp_secret_token"));
        assert!(task_frame_store_path(&workspace).exists());
        Ok(())
    }

    #[test]
    fn update_subgoal_and_close_redact_late_secret_inputs() -> TestResult {
        let workspace = temp_workspace("redaction-update")?;
        let created =
            create_task_frame(&create_options(workspace.clone())).map_err(|e| e.message())?;
        let frame_id = created.frame.ok_or_else(|| "missing frame".to_owned())?.id;

        update_task_frame(&TaskFrameUpdateOptions {
            workspace_path: workspace.clone(),
            frame_id: frame_id.clone(),
            status: Some(TaskFrameStatus::Active),
            current_focus: Some("new token=abc123".to_owned()),
            blockers: vec!["secret: hidden".to_owned()],
            evidence_links: Vec::new(),
            updated_at: Some("2026-05-04T00:06:00Z".to_owned()),
            dry_run: false,
        })
        .map_err(|e| e.message())?;
        add_task_subgoal(&TaskSubgoalAddOptions {
            workspace_path: workspace.clone(),
            frame_id: frame_id.clone(),
            parent_id: None,
            title: "remove private_key=abcdef".to_owned(),
            status: TaskFrameStatus::Open,
            blockers: Vec::new(),
            created_at: Some("2026-05-04T00:07:00Z".to_owned()),
            dry_run: false,
        })
        .map_err(|e| e.message())?;
        let closed = close_task_frame(&TaskFrameCloseOptions {
            workspace_path: workspace,
            frame_id,
            status: TaskFrameStatus::Completed,
            reason: "completed after password=done".to_owned(),
            closed_at: Some("2026-05-04T00:08:00Z".to_owned()),
            dry_run: false,
        })
        .map_err(|e| e.message())?;
        let frame = closed.frame.ok_or_else(|| "missing frame".to_owned())?;
        let serialized = serde_json::to_string(&frame).map_err(|error| error.to_string())?;

        assert_eq!(frame.redaction_status, "redacted");
        assert!(serialized.contains(REDACTION_PLACEHOLDER));
        assert!(!serialized.contains("abc123"));
        assert!(!serialized.contains("hidden"));
        assert!(!serialized.contains("abcdef"));
        assert!(!serialized.contains("done"));
        Ok(())
    }
}
