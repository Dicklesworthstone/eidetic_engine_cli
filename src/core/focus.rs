//! Passive active-memory focus workflow (EE-FOCUS-001).
//!
//! Focus state is explicit, bounded, and passive. Mutating commands write a
//! workspace-local state artifact; read commands report and explain that state
//! without deciding what the agent should do next.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};

use crate::core::degraded_aggregation::{DegradationAggregationInput, aggregate_degraded_entries};
use crate::db::DbConnection;
use crate::models::{
    DomainError, FOCUS_ITEM_SCHEMA_V1, FOCUS_STATE_SCHEMA_V1, FocusItem, FocusState,
    FocusValidationError, MemoryId, WorkspaceId,
};

pub const FOCUS_COMMAND_SCHEMA_V1: &str = "ee.focus.command.v1";
pub const DEFAULT_FOCUS_CAPACITY: usize = 7;
pub const FOCUS_STATE_RELATIVE_PATH: &str = ".ee/focus/state.json";

const UNSET_FOCUS_TIMESTAMP: &str = "1970-01-01T00:00:00Z";

/// Cached focus state to avoid re-reading unchanged files on every context request.
struct FocusCacheEntry {
    path: PathBuf,
    mtime: SystemTime,
    state: FocusState,
}

static FOCUS_CACHE: OnceLock<Mutex<Option<FocusCacheEntry>>> = OnceLock::new();

fn get_focus_cache() -> &'static Mutex<Option<FocusCacheEntry>> {
    FOCUS_CACHE.get_or_init(|| Mutex::new(None))
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FocusScope {
    pub task_frame_id: Option<String>,
    pub recorder_run_id: Option<String>,
    pub handoff_id: Option<String>,
    pub profile: Option<String>,
}

impl FocusScope {
    fn apply_to_state(&self, state: &mut FocusState) {
        if let Some(task_frame_id) = &self.task_frame_id {
            state.task_frame_id = Some(task_frame_id.clone());
        }
        if let Some(recorder_run_id) = &self.recorder_run_id {
            state.recorder_run_id = Some(recorder_run_id.clone());
        }
        if let Some(handoff_id) = &self.handoff_id {
            state.handoff_id = Some(handoff_id.clone());
        }
        if let Some(profile) = &self.profile {
            state.profile = Some(profile.clone());
        }
    }

    fn apply_exact_to_state(&self, state: &mut FocusState) {
        state.task_frame_id = self.task_frame_id.clone();
        state.recorder_run_id = self.recorder_run_id.clone();
        state.handoff_id = self.handoff_id.clone();
        state.profile = self.profile.clone();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FocusShowOptions {
    pub workspace_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FocusSetOptions {
    pub workspace_path: PathBuf,
    pub memory_ids: Vec<String>,
    pub focal_memory_id: Option<String>,
    pub pinned_memory_ids: Vec<String>,
    pub capacity: usize,
    pub reason: String,
    pub provenance: Vec<String>,
    pub scope: FocusScope,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FocusAddOptions {
    pub workspace_path: PathBuf,
    pub memory_ids: Vec<String>,
    pub focal_memory_id: Option<String>,
    pub pinned_memory_ids: Vec<String>,
    pub capacity: Option<usize>,
    pub reason: String,
    pub provenance: Vec<String>,
    pub scope: FocusScope,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FocusRemoveOptions {
    pub workspace_path: PathBuf,
    pub memory_ids: Vec<String>,
    pub provenance: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FocusClearOptions {
    pub workspace_path: PathBuf,
    pub capacity: Option<usize>,
    pub provenance: Vec<String>,
    pub scope: FocusScope,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FocusExplainOptions {
    pub workspace_path: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FocusMemoryAvailability {
    Present,
    Tombstoned,
}

impl FocusMemoryAvailability {
    const fn as_status(self) -> FocusMemoryStatusKind {
        match self {
            Self::Present => FocusMemoryStatusKind::Present,
            Self::Tombstoned => FocusMemoryStatusKind::Tombstoned,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FocusMemoryStatusKind {
    Present,
    Missing,
    Tombstoned,
    Unverified,
}

impl FocusMemoryStatusKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Missing => "missing",
            Self::Tombstoned => "tombstoned",
            Self::Unverified => "unverified",
        }
    }

    const fn is_unusable(self) -> bool {
        matches!(self, Self::Missing | Self::Tombstoned)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FocusMemoryStatus {
    pub memory_id: String,
    pub status: FocusMemoryStatusKind,
    pub reason: String,
}

impl FocusMemoryStatus {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "memoryId": self.memory_id,
            "status": self.status.as_str(),
            "reason": self.reason,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FocusExplanation {
    pub code: String,
    pub memory_id: Option<String>,
    pub message: String,
}

impl FocusExplanation {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            memory_id: None,
            message: message.into(),
        }
    }

    fn for_memory(
        code: impl Into<String>,
        memory_id: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            memory_id: Some(memory_id.into()),
            message: message.into(),
        }
    }

    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "code": self.code,
            "memoryId": self.memory_id,
            "message": self.message,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FocusDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: Option<String>,
}

impl FocusDegradation {
    fn low(
        code: impl Into<String>,
        message: impl Into<String>,
        repair: impl Into<Option<String>>,
    ) -> Self {
        Self {
            code: code.into(),
            severity: "low".to_owned(),
            message: message.into(),
            repair: repair.into(),
        }
    }

    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "code": self.code,
            "severity": self.severity,
            "message": self.message,
            "repair": self.repair,
        })
    }
}

fn focus_degraded_data_json(degraded: &[FocusDegradation]) -> Vec<JsonValue> {
    aggregate_degraded_entries(degraded.iter().map(|entry| {
        DegradationAggregationInput::new(
            "focus",
            entry.code.clone(),
            entry.severity.clone(),
            entry.message.clone(),
            entry.repair.clone().unwrap_or_default(),
        )
    }))
    .into_iter()
    .map(|entry| {
        json!({
            "code": entry.code,
            "severity": entry.severity,
            "message": entry.message,
            "repair": entry.repair,
            "sources": entry.sources,
        })
    })
    .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FocusReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_path: PathBuf,
    pub storage_path: PathBuf,
    pub state: FocusState,
    pub state_hash: String,
    pub before_state_hash: Option<String>,
    pub after_state_hash: Option<String>,
    pub mutated: bool,
    pub mutation_kind: &'static str,
    pub memory_statuses: Vec<FocusMemoryStatus>,
    pub explanations: Vec<FocusExplanation>,
    pub degraded: Vec<FocusDegradation>,
}

impl FocusReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        let missing_memory_ids = self
            .memory_statuses
            .iter()
            .filter(|status| status.status == FocusMemoryStatusKind::Missing)
            .map(|status| status.memory_id.clone())
            .collect::<Vec<_>>();
        let stale_memory_ids = self
            .memory_statuses
            .iter()
            .filter(|status| status.status == FocusMemoryStatusKind::Tombstoned)
            .map(|status| status.memory_id.clone())
            .collect::<Vec<_>>();
        let active_memory_ids = self
            .state
            .items
            .iter()
            .map(|item| item.memory_id.to_string())
            .collect::<Vec<_>>();

        json!({
            "schema": self.schema,
            "command": self.command,
            "version": self.version,
            "workspacePath": self.workspace_path.display().to_string(),
            "storagePath": self.storage_path.display().to_string(),
            "stateHash": self.state_hash,
            "beforeStateHash": self.before_state_hash,
            "afterStateHash": self.after_state_hash,
            "mutated": self.mutated,
            "mutationKind": self.mutation_kind,
            "activeMemoryIds": active_memory_ids,
            "missingMemoryIds": missing_memory_ids,
            "staleMemoryIds": stale_memory_ids,
            "focusState": self.state.data_json(),
            "memoryStatuses": self
                .memory_statuses
                .iter()
                .map(FocusMemoryStatus::data_json)
                .collect::<Vec<_>>(),
            "explanations": self
                .explanations
                .iter()
                .map(FocusExplanation::data_json)
                .collect::<Vec<_>>(),
            "degraded": focus_degraded_data_json(&self.degraded),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredFocusItem {
    schema: String,
    memory_id: String,
    pinned: bool,
    reason: String,
    provenance: Vec<String>,
    added_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredFocusState {
    schema: String,
    workspace_id: String,
    task_frame_id: Option<String>,
    recorder_run_id: Option<String>,
    handoff_id: Option<String>,
    profile: Option<String>,
    capacity: usize,
    focal_memory_id: Option<String>,
    items: Vec<StoredFocusItem>,
    updated_at: String,
    provenance: Vec<String>,
}

#[derive(Clone, Debug)]
struct LoadedFocus {
    workspace_path: PathBuf,
    storage_path: PathBuf,
    state: FocusState,
    degraded: Vec<FocusDegradation>,
}

pub fn show_focus(options: &FocusShowOptions) -> Result<FocusReport, DomainError> {
    let loaded = load_focus(options.workspace_path.as_path())?;
    let memory_statuses = memory_statuses_for_workspace(&loaded.state, &loaded.workspace_path);
    let mut degraded = loaded.degraded.clone();
    degraded.extend(memory_status_degradations(&memory_statuses));
    Ok(report(FocusReportInput {
        command: "focus show",
        loaded,
        mutated: false,
        mutation_kind: "read_only",
        before_state_hash: None,
        after_state_hash: None,
        memory_statuses,
        explanations: vec![FocusExplanation::new(
            "focus_read_only",
            "Displayed passive focus state without writing workspace files.",
        )],
        degraded,
    }))
}

pub fn set_focus(options: &FocusSetOptions) -> Result<FocusReport, DomainError> {
    ensure_capacity(options.capacity)?;
    let loaded = load_focus(options.workspace_path.as_path())?;
    let before_hash = state_hash(&loaded.state);
    let workspace_id = stable_workspace_id(&loaded.workspace_path);
    let now = Utc::now().to_rfc3339();
    let parsed_memory_ids = parse_memory_ids(&options.memory_ids)?;
    let focal_memory_id = parse_optional_memory_id(options.focal_memory_id.as_deref())?;
    let pinned_memory_ids = parse_memory_id_set(&options.pinned_memory_ids)?;

    validate_requested_memory_ids(&loaded.workspace_path, &parsed_memory_ids)?;

    let mut state = FocusState::new(workspace_id, options.capacity, now.clone())
        .map_err(focus_validation_error)?;
    options.scope.apply_exact_to_state(&mut state);
    state.provenance = command_provenance("ee focus set", &options.provenance);

    for memory_id in parsed_memory_ids {
        let mut item = FocusItem::new(memory_id, options.reason.clone(), now.clone())
            .map_err(focus_validation_error)?
            .pinned(pinned_memory_ids.contains(&memory_id));
        for provenance in &state.provenance {
            item = item.with_provenance(provenance.clone());
        }
        state = state.with_item(item).map_err(focus_validation_error)?;
    }
    state.focal_memory_id = focal_memory_id;
    canonicalize_state(&mut state);
    state.validate().map_err(focus_validation_error)?;

    write_focus_state(&loaded.storage_path, &state)?;
    let memory_statuses = memory_statuses_for_workspace(&state, &loaded.workspace_path);
    let after_hash = state_hash(&state);
    Ok(report(FocusReportInput {
        command: "focus set",
        loaded: LoadedFocus { state, ..loaded },
        mutated: true,
        mutation_kind: "replace_state",
        before_state_hash: Some(before_hash),
        after_state_hash: Some(after_hash),
        memory_statuses,
        explanations: vec![FocusExplanation::new(
            "focus_set_explicit",
            "Replaced the passive focus set from explicit command arguments.",
        )],
        degraded: Vec::new(),
    }))
}

pub fn add_focus(options: &FocusAddOptions) -> Result<FocusReport, DomainError> {
    let loaded = load_focus(options.workspace_path.as_path())?;
    let before_hash = state_hash(&loaded.state);
    let now = Utc::now().to_rfc3339();
    let parsed_memory_ids = parse_memory_ids(&options.memory_ids)?;
    let focal_memory_id = parse_optional_memory_id(options.focal_memory_id.as_deref())?;
    let pinned_memory_ids = parse_memory_id_set(&options.pinned_memory_ids)?;

    validate_requested_memory_ids(&loaded.workspace_path, &parsed_memory_ids)?;

    let mut state = loaded.state.clone();
    if let Some(capacity) = options.capacity {
        ensure_capacity(capacity)?;
        state.capacity = capacity;
    }
    options.scope.apply_to_state(&mut state);

    let provenance = command_provenance("ee focus add", &options.provenance);
    append_unique(&mut state.provenance, &provenance);
    state.updated_at = now.clone();

    let mut explanations = Vec::new();
    let mut changed = false;
    for memory_id in parsed_memory_ids {
        if state.contains_memory(memory_id) {
            explanations.push(FocusExplanation::for_memory(
                "focus_add_already_present",
                memory_id.to_string(),
                "Memory was already present in the focus set; it was not duplicated.",
            ));
            continue;
        }
        if state.items.len().saturating_add(1) > state.capacity {
            return Err(DomainError::Usage {
                message: format!(
                    "Cannot add {memory_id}: focus capacity {} would be exceeded.",
                    state.capacity
                ),
                repair: Some(
                    "Use ee focus set --capacity <N> or remove another memory first.".to_owned(),
                ),
            });
        }
        let mut item = FocusItem::new(memory_id, options.reason.clone(), now.clone())
            .map_err(focus_validation_error)?
            .pinned(pinned_memory_ids.contains(&memory_id));
        for provenance_entry in &provenance {
            item = item.with_provenance(provenance_entry.clone());
        }
        state = state.with_item(item).map_err(focus_validation_error)?;
        changed = true;
    }

    if let Some(focal) = focal_memory_id {
        state.focal_memory_id = Some(focal);
    }
    canonicalize_state(&mut state);
    state.validate().map_err(focus_validation_error)?;
    let after_hash = state_hash(&state);
    if changed || before_hash != after_hash {
        write_focus_state(&loaded.storage_path, &state)?;
    }
    if explanations.is_empty() {
        explanations.push(FocusExplanation::new(
            "focus_add_explicit",
            "Added explicit memories to the passive focus set without evicting existing entries.",
        ));
    }
    let memory_statuses = memory_statuses_for_workspace(&state, &loaded.workspace_path);
    Ok(report(FocusReportInput {
        command: "focus add",
        loaded: LoadedFocus { state, ..loaded },
        mutated: changed || before_hash != after_hash,
        mutation_kind: "add_items",
        before_state_hash: Some(before_hash),
        after_state_hash: Some(after_hash),
        memory_statuses,
        explanations,
        degraded: Vec::new(),
    }))
}

pub fn remove_focus(options: &FocusRemoveOptions) -> Result<FocusReport, DomainError> {
    let loaded = load_focus(options.workspace_path.as_path())?;
    let before_hash = state_hash(&loaded.state);
    let remove_ids = parse_memory_id_set(&options.memory_ids)?;
    let mut state = loaded.state.clone();
    let before_count = state.items.len();
    let mut explanations = Vec::new();

    state.items.retain(|item| {
        let remove = remove_ids.contains(&item.memory_id);
        if remove {
            explanations.push(FocusExplanation::for_memory(
                "focus_remove_explicit",
                item.memory_id.to_string(),
                "Removed by explicit focus command.",
            ));
        }
        !remove
    });
    for memory_id in &remove_ids {
        if !loaded.state.contains_memory(*memory_id) {
            explanations.push(FocusExplanation::for_memory(
                "focus_remove_not_present",
                memory_id.to_string(),
                "Memory was not present in the focus set.",
            ));
        }
    }
    if state
        .focal_memory_id
        .is_some_and(|memory_id| remove_ids.contains(&memory_id))
    {
        state.focal_memory_id = None;
        explanations.push(FocusExplanation::new(
            "focus_focal_removed",
            "Removed focal memory from the focus set, so focalMemoryId was cleared.",
        ));
    }

    let changed =
        state.items.len() != before_count || state.focal_memory_id != loaded.state.focal_memory_id;
    if changed {
        state.updated_at = Utc::now().to_rfc3339();
        let provenance = command_provenance("ee focus remove", &options.provenance);
        append_unique(&mut state.provenance, &provenance);
        canonicalize_state(&mut state);
        state.validate().map_err(focus_validation_error)?;
    }
    let after_hash = state_hash(&state);
    if changed {
        write_focus_state(&loaded.storage_path, &state)?;
    }
    let memory_statuses = memory_statuses_for_workspace(&state, &loaded.workspace_path);
    Ok(report(FocusReportInput {
        command: "focus remove",
        loaded: LoadedFocus { state, ..loaded },
        mutated: changed,
        mutation_kind: "remove_items",
        before_state_hash: Some(before_hash),
        after_state_hash: Some(after_hash),
        memory_statuses,
        explanations,
        degraded: Vec::new(),
    }))
}

pub fn clear_focus(options: &FocusClearOptions) -> Result<FocusReport, DomainError> {
    let loaded = load_focus(options.workspace_path.as_path())?;
    let before_hash = state_hash(&loaded.state);
    let capacity = options.capacity.unwrap_or(loaded.state.capacity);
    ensure_capacity(capacity)?;
    let mut state = FocusState::new(
        stable_workspace_id(&loaded.workspace_path),
        capacity,
        Utc::now().to_rfc3339(),
    )
    .map_err(focus_validation_error)?;
    options.scope.apply_exact_to_state(&mut state);
    state.provenance = command_provenance("ee focus clear", &options.provenance);
    write_focus_state(&loaded.storage_path, &state)?;
    let after_hash = state_hash(&state);
    Ok(report(FocusReportInput {
        command: "focus clear",
        loaded: LoadedFocus { state, ..loaded },
        mutated: true,
        mutation_kind: "clear_state",
        before_state_hash: Some(before_hash),
        after_state_hash: Some(after_hash),
        memory_statuses: Vec::new(),
        explanations: vec![FocusExplanation::new(
            "focus_clear_explicit",
            "Cleared passive focus state by writing an empty state artifact; no files were deleted.",
        )],
        degraded: Vec::new(),
    }))
}

pub fn explain_focus(options: &FocusExplainOptions) -> Result<FocusReport, DomainError> {
    let loaded = load_focus(options.workspace_path.as_path())?;
    let memory_statuses = memory_statuses_for_workspace(&loaded.state, &loaded.workspace_path);
    let mut degraded = loaded.degraded.clone();
    degraded.extend(memory_status_degradations(&memory_statuses));
    let mut explanations = vec![FocusExplanation::new(
        "focus_passive_boundary",
        "Focus state records active memory IDs and provenance only; it does not infer hidden attention or execute a plan.",
    )];
    explanations.extend(loaded.state.items.iter().map(|item| {
        let focal = loaded.state.focal_memory_id == Some(item.memory_id);
        let pin = if item.pinned { "pinned" } else { "unpinned" };
        let focal_text = if focal { " focal" } else { "" };
        FocusExplanation::for_memory(
            "focus_item_included",
            item.memory_id.to_string(),
            format!("{pin}{focal_text} memory included because: {}", item.reason),
        )
    }));
    if loaded.state.items.is_empty() {
        explanations.push(FocusExplanation::new(
            "focus_empty",
            "No active memories are currently focused for this workspace.",
        ));
    }
    Ok(report(FocusReportInput {
        command: "focus explain",
        loaded,
        mutated: false,
        mutation_kind: "read_only",
        before_state_hash: None,
        after_state_hash: None,
        memory_statuses,
        explanations,
        degraded,
    }))
}

/// Read the active focus state if the workspace has one.
///
/// Uses an mtime-based cache to avoid re-reading and re-parsing the focus
/// state file on every context request when it hasn't changed.
///
/// # Errors
///
/// Returns a storage/configuration error if the state artifact exists but
/// cannot be read or parsed.
pub fn read_active_focus_state(workspace_path: &Path) -> Result<Option<FocusState>, DomainError> {
    let workspace_path = normalize_workspace_path(workspace_path);
    let storage_path = focus_state_path(&workspace_path);

    let Some(metadata) = focus_state_metadata_for_read(&storage_path)? else {
        return Ok(None);
    };

    let mtime = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);

    let cache = get_focus_cache();
    if let Ok(guard) = cache.lock() {
        if let Some(entry) = guard.as_ref() {
            if entry.path == storage_path && entry.mtime == mtime {
                return Ok(Some(entry.state.clone()));
            }
        }
    }

    let state = read_focus_state(&storage_path)?;

    if let Ok(mut guard) = cache.lock() {
        *guard = Some(FocusCacheEntry {
            path: storage_path,
            mtime,
            state: state.clone(),
        });
    }

    Ok(Some(state))
}

#[must_use]
pub fn focus_state_path(workspace_path: &Path) -> PathBuf {
    workspace_path.join(FOCUS_STATE_RELATIVE_PATH)
}

#[must_use]
pub fn focus_state_hash(state: &FocusState) -> String {
    state_hash(state)
}

#[must_use]
pub fn focus_memory_statuses_from_lookup(
    state: &FocusState,
    lookup: &BTreeMap<String, FocusMemoryAvailability>,
    lookup_complete: bool,
) -> Vec<FocusMemoryStatus> {
    let mut statuses = state
        .items
        .iter()
        .map(|item| {
            let memory_id = item.memory_id.to_string();
            let status = match lookup.get(&memory_id) {
                Some(availability) => availability.as_status(),
                None if lookup_complete => FocusMemoryStatusKind::Missing,
                None => FocusMemoryStatusKind::Unverified,
            };
            FocusMemoryStatus {
                memory_id,
                status,
                reason: status_reason(status),
            }
        })
        .collect::<Vec<_>>();
    statuses.sort_by(|left, right| left.memory_id.cmp(&right.memory_id));
    statuses
}

fn load_focus(workspace_path: &Path) -> Result<LoadedFocus, DomainError> {
    let workspace_path = normalize_workspace_path(workspace_path);
    let storage_path = focus_state_path(&workspace_path);
    let (state, degraded) = match focus_state_metadata_for_read(&storage_path)? {
        Some(_) => (read_focus_state(&storage_path)?, Vec::new()),
        None => (
            empty_focus_state(&workspace_path, DEFAULT_FOCUS_CAPACITY)?,
            vec![FocusDegradation::low(
                "focus_state_absent",
                "No focus state artifact exists yet; reporting an empty passive state.",
                Some("ee focus set <memory-id> --json".to_owned()),
            )],
        ),
    };
    Ok(LoadedFocus {
        workspace_path,
        storage_path,
        state,
        degraded,
    })
}

fn read_focus_state(path: &Path) -> Result<FocusState, DomainError> {
    let Some(_) = focus_state_metadata_for_read(path)? else {
        return Err(DomainError::Storage {
            message: format!("Focus state {} is missing.", path.display()),
            repair: Some("Run ee focus set <memory-id> --json.".to_owned()),
        });
    };
    let raw = fs::read_to_string(path).map_err(|error| DomainError::Storage {
        message: format!("Failed to read focus state {}: {error}", path.display()),
        repair: Some("Check workspace .ee/focus permissions.".to_owned()),
    })?;
    let stored: StoredFocusState =
        serde_json::from_str(&raw).map_err(|error| DomainError::Storage {
            message: format!("Failed to parse focus state {}: {error}", path.display()),
            repair: Some(
                "Run ee focus clear --json to replace the malformed focus state.".to_owned(),
            ),
        })?;
    stored_focus_state_to_domain(stored)
}

fn focus_state_metadata_for_read(path: &Path) -> Result<Option<std::fs::Metadata>, DomainError> {
    ensure_no_symlink_components(path, "read")?;
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(error) => {
            return Err(DomainError::Storage {
                message: format!("Failed to stat focus state {}: {error}", path.display()),
                repair: Some("Check workspace .ee/focus permissions.".to_owned()),
            });
        }
    };
    if !metadata.file_type().is_file() {
        return Err(DomainError::Storage {
            message: format!(
                "Refusing to read focus state {} because it is not a regular file.",
                path.display()
            ),
            repair: Some("Replace .ee/focus/state.json with a regular JSON file.".to_owned()),
        });
    }
    Ok(Some(metadata))
}

fn write_focus_state(path: &Path, state: &FocusState) -> Result<(), DomainError> {
    ensure_no_symlink_components(path, "write")?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to create focus state directory {}: {error}",
                parent.display()
            ),
            repair: Some("Check workspace .ee permissions.".to_owned()),
        })?;
    }
    ensure_no_symlink_components(path, "write")?;
    ensure_focus_state_final_path_for_write(path)?;
    let mut body =
        serde_json::to_string_pretty(&state.data_json()).map_err(|error| DomainError::Storage {
            message: format!("Failed to serialize focus state: {error}"),
            repair: Some("Report the focus serialization failure.".to_owned()),
        })?;
    body.push('\n');
    let temp_path = path.with_extension("json.tmp");
    ensure_no_symlink_components(&temp_path, "write")?;
    ensure_focus_state_temp_path_for_write(&temp_path)?;
    write_focus_state_temp_file(&temp_path, &body)?;
    publish_focus_state_temp_file(path, &temp_path)?;

    // Update cache with new state to avoid stale reads
    if let Ok(mtime) = fs::symlink_metadata(path).and_then(|m| m.modified()) {
        if let Ok(mut guard) = get_focus_cache().lock() {
            *guard = Some(FocusCacheEntry {
                path: path.to_path_buf(),
                mtime,
                state: state.clone(),
            });
        }
    }

    Ok(())
}

fn publish_focus_state_temp_file(path: &Path, temp_path: &Path) -> Result<(), DomainError> {
    ensure_no_symlink_components(path, "write")?;
    ensure_focus_state_final_path_for_write(path)?;
    fs::rename(temp_path, path).map_err(|error| DomainError::Storage {
        message: format!("Failed to publish focus state {}: {error}", path.display()),
        repair: Some("Check workspace .ee/focus permissions.".to_owned()),
    })
}

fn ensure_focus_state_final_path_for_write(path: &Path) -> Result<(), DomainError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(DomainError::Storage {
            message: format!(
                "Refusing to write focus state {} because it is not a regular file.",
                path.display()
            ),
            repair: Some("Replace .ee/focus/state.json with a regular JSON file.".to_owned()),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DomainError::Storage {
            message: format!(
                "Failed to stat focus state {} before write: {error}",
                path.display()
            ),
            repair: Some("Check workspace .ee/focus permissions.".to_owned()),
        }),
    }
}

fn ensure_focus_state_temp_path_for_write(path: &Path) -> Result<(), DomainError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Err(DomainError::Storage {
            message: format!(
                "Refusing to write focus state temp file {} because it already exists.",
                path.display()
            ),
            repair: Some("Remove stale .ee/focus/state.json.tmp and retry.".to_owned()),
        }),
        Ok(_) => Err(DomainError::Storage {
            message: format!(
                "Refusing to write focus state temp file {} because it is not a regular file.",
                path.display()
            ),
            repair: Some("Replace .ee/focus/state.json.tmp with a regular file.".to_owned()),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DomainError::Storage {
            message: format!(
                "Failed to stat focus state temp file {} before write: {error}",
                path.display()
            ),
            repair: Some("Check workspace .ee/focus permissions.".to_owned()),
        }),
    }
}

fn write_focus_state_temp_file(path: &Path, body: &str) -> Result<(), DomainError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to create focus state temp file {}: {error}",
                path.display()
            ),
            repair: Some("Check workspace .ee/focus permissions.".to_owned()),
        })?;
    file.write_all(body.as_bytes())
        .map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to write focus state temp file {}: {error}",
                path.display()
            ),
            repair: Some("Check workspace .ee/focus permissions.".to_owned()),
        })?;
    file.sync_all().map_err(|error| DomainError::Storage {
        message: format!(
            "Failed to sync focus state temp file {}: {error}",
            path.display()
        ),
        repair: Some("Check workspace .ee/focus permissions.".to_owned()),
    })
}

fn ensure_no_symlink_components(path: &Path, operation: &'static str) -> Result<(), DomainError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(DomainError::Storage {
                    message: format!(
                        "Refusing to {operation} focus state `{}` through symlinked path component `{}`.",
                        path.display(),
                        current.display()
                    ),
                    repair: Some(
                        "Replace the symlink with a real workspace .ee/focus path before retrying."
                            .to_owned(),
                    ),
                });
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(());
            }
            Err(error) => {
                return Err(DomainError::Storage {
                    message: format!(
                        "Failed to inspect focus state path component `{}` before {operation}: {error}",
                        current.display()
                    ),
                    repair: Some("Check workspace .ee/focus permissions.".to_owned()),
                });
            }
        }
    }
    Ok(())
}

fn stored_focus_state_to_domain(stored: StoredFocusState) -> Result<FocusState, DomainError> {
    if stored.schema != FOCUS_STATE_SCHEMA_V1 {
        return Err(DomainError::Storage {
            message: format!("Unsupported focus state schema `{}`.", stored.schema),
            repair: Some("Run ee focus clear --json to reset focus state.".to_owned()),
        });
    }
    let workspace_id =
        WorkspaceId::from_str(&stored.workspace_id).map_err(|error| DomainError::Storage {
            message: format!(
                "Invalid focus workspaceId `{}`: {error}",
                stored.workspace_id
            ),
            repair: Some("Run ee focus clear --json to reset focus state.".to_owned()),
        })?;
    let mut state = FocusState::new(workspace_id, stored.capacity, stored.updated_at)
        .map_err(focus_validation_error)?;
    state.task_frame_id = stored.task_frame_id;
    state.recorder_run_id = stored.recorder_run_id;
    state.handoff_id = stored.handoff_id;
    state.profile = stored.profile;
    state.focal_memory_id = stored
        .focal_memory_id
        .as_deref()
        .map(parse_memory_id)
        .transpose()?;
    state.provenance = normalize_string_list(stored.provenance);

    for stored_item in stored.items {
        if stored_item.schema != FOCUS_ITEM_SCHEMA_V1 {
            return Err(DomainError::Storage {
                message: format!("Unsupported focus item schema `{}`.", stored_item.schema),
                repair: Some("Run ee focus clear --json to reset focus state.".to_owned()),
            });
        }
        let mut item = FocusItem::new(
            parse_memory_id(&stored_item.memory_id)?,
            stored_item.reason,
            stored_item.added_at,
        )
        .map_err(focus_validation_error)?
        .pinned(stored_item.pinned);
        for provenance in stored_item.provenance {
            item = item.with_provenance(provenance);
        }
        state = state.with_item(item).map_err(focus_validation_error)?;
    }
    canonicalize_state(&mut state);
    state.validate().map_err(focus_validation_error)?;
    Ok(state)
}

fn empty_focus_state(workspace_path: &Path, capacity: usize) -> Result<FocusState, DomainError> {
    FocusState::new(
        stable_workspace_id(workspace_path),
        capacity,
        UNSET_FOCUS_TIMESTAMP,
    )
    .map_err(focus_validation_error)
}

fn normalize_workspace_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn stable_workspace_id(workspace_path: &Path) -> WorkspaceId {
    let hash = blake3::hash(format!("workspace:{}", workspace_path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes))
}

fn parse_memory_ids(raw_ids: &[String]) -> Result<Vec<MemoryId>, DomainError> {
    let mut parsed = Vec::with_capacity(raw_ids.len());
    for raw in raw_ids {
        parsed.push(parse_memory_id(raw)?);
    }
    Ok(parsed)
}

fn parse_memory_id_set(raw_ids: &[String]) -> Result<BTreeSet<MemoryId>, DomainError> {
    let mut parsed = BTreeSet::new();
    for raw in raw_ids {
        parsed.insert(parse_memory_id(raw)?);
    }
    Ok(parsed)
}

fn parse_optional_memory_id(raw: Option<&str>) -> Result<Option<MemoryId>, DomainError> {
    raw.map(parse_memory_id).transpose()
}

fn parse_memory_id(raw: &str) -> Result<MemoryId, DomainError> {
    MemoryId::from_str(raw).map_err(|error| DomainError::Usage {
        message: format!("Invalid memory ID `{raw}`: {error}"),
        repair: Some("Use an ID returned by ee remember, ee search, or ee memory list.".to_owned()),
    })
}

fn ensure_capacity(capacity: usize) -> Result<(), DomainError> {
    if capacity == 0 {
        return Err(DomainError::Usage {
            message: "Focus capacity must be greater than zero.".to_owned(),
            repair: Some("Use --capacity 1 or higher.".to_owned()),
        });
    }
    Ok(())
}

fn validate_requested_memory_ids(
    workspace_path: &Path,
    memory_ids: &[MemoryId],
) -> Result<(), DomainError> {
    let Some(lookup) = memory_lookup_if_database_exists(workspace_path, memory_ids) else {
        return Ok(());
    };
    let state = state_for_status_check(workspace_path, memory_ids)?;
    let statuses = focus_memory_statuses_from_lookup(&state, &lookup, true);
    let unusable = statuses
        .iter()
        .filter(|status| status.status.is_unusable())
        .map(|status| format!("{} ({})", status.memory_id, status.status.as_str()))
        .collect::<Vec<_>>();
    if unusable.is_empty() {
        Ok(())
    } else {
        Err(DomainError::Usage {
            message: format!("Focus memory IDs are not active: {}.", unusable.join(", ")),
            repair: Some("Use ee memory list --json or remove stale focus entries.".to_owned()),
        })
    }
}

fn state_for_status_check(
    workspace_path: &Path,
    memory_ids: &[MemoryId],
) -> Result<FocusState, DomainError> {
    let mut state = empty_focus_state(workspace_path, memory_ids.len().max(1))?;
    for memory_id in memory_ids {
        state = state
            .with_item(
                FocusItem::new(*memory_id, "Validation probe.", UNSET_FOCUS_TIMESTAMP)
                    .map_err(focus_validation_error)?,
            )
            .map_err(focus_validation_error)?;
    }
    Ok(state)
}

fn memory_statuses_for_workspace(
    state: &FocusState,
    workspace_path: &Path,
) -> Vec<FocusMemoryStatus> {
    let memory_ids = state
        .items
        .iter()
        .map(|item| item.memory_id)
        .collect::<Vec<_>>();
    match memory_lookup_if_database_exists(workspace_path, &memory_ids) {
        Some(lookup) => focus_memory_statuses_from_lookup(state, &lookup, true),
        None => focus_memory_statuses_from_lookup(state, &BTreeMap::new(), false),
    }
}

fn memory_lookup_if_database_exists(
    workspace_path: &Path,
    memory_ids: &[MemoryId],
) -> Option<BTreeMap<String, FocusMemoryAvailability>> {
    let database_path = workspace_path.join(".ee").join("ee.db");
    ensure_no_symlink_components(&database_path, "read").ok()?;
    if fs::symlink_metadata(&database_path).is_err() {
        return None;
    }
    let Ok(connection) = DbConnection::open_file(database_path) else {
        return None;
    };
    let mut lookup = BTreeMap::new();
    for memory_id in memory_ids {
        if let Ok(Some(memory)) = connection.get_memory(&memory_id.to_string()) {
            let availability = if memory.tombstoned_at.is_some() {
                FocusMemoryAvailability::Tombstoned
            } else {
                FocusMemoryAvailability::Present
            };
            lookup.insert(memory_id.to_string(), availability);
        }
    }
    Some(lookup)
}

fn status_reason(status: FocusMemoryStatusKind) -> String {
    match status {
        FocusMemoryStatusKind::Present => "Memory exists and is not tombstoned.".to_owned(),
        FocusMemoryStatusKind::Missing => "Memory ID is not present in the database.".to_owned(),
        FocusMemoryStatusKind::Tombstoned => {
            "Memory exists but is tombstoned and should not influence context.".to_owned()
        }
        FocusMemoryStatusKind::Unverified => {
            "No initialized database was available for memory ID verification.".to_owned()
        }
    }
}

fn memory_status_degradations(statuses: &[FocusMemoryStatus]) -> Vec<FocusDegradation> {
    let mut degraded = Vec::new();
    if statuses
        .iter()
        .any(|status| status.status == FocusMemoryStatusKind::Missing)
    {
        degraded.push(FocusDegradation::low(
            "focus_missing_memory",
            "Focus state references memory IDs that are missing from the database.",
            Some("ee focus remove <memory-id> --json".to_owned()),
        ));
    }
    if statuses
        .iter()
        .any(|status| status.status == FocusMemoryStatusKind::Tombstoned)
    {
        degraded.push(FocusDegradation::low(
            "focus_tombstoned_memory",
            "Focus state references tombstoned memories that will not be used for context.",
            Some("ee focus remove <memory-id> --json".to_owned()),
        ));
    }
    if statuses
        .iter()
        .any(|status| status.status == FocusMemoryStatusKind::Unverified)
    {
        degraded.push(FocusDegradation::low(
            "focus_memory_verification_unavailable",
            "Focus memory IDs could not be verified because no workspace database was available.",
            Some("ee init --workspace .".to_owned()),
        ));
    }
    degraded
}

fn command_provenance(command: &'static str, explicit: &[String]) -> Vec<String> {
    let mut provenance = normalize_string_list(explicit.to_vec());
    if provenance.is_empty() {
        provenance.push(command.to_owned());
    }
    provenance
}

fn normalize_string_list(values: Vec<String>) -> Vec<String> {
    let mut normalized = values
        .into_iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn append_unique(target: &mut Vec<String>, additions: &[String]) {
    target.extend(additions.iter().cloned());
    target.sort();
    target.dedup();
}

fn canonicalize_state(state: &mut FocusState) {
    for item in &mut state.items {
        item.provenance.sort();
        item.provenance.dedup();
    }
    state.items.sort_by_key(|item| item.memory_id);
    state.provenance.sort();
    state.provenance.dedup();
}

fn state_hash(state: &FocusState) -> String {
    let serialized = serde_json::to_string(&state.data_json()).unwrap_or_else(|_| "{}".to_owned());
    format!("blake3:{}", blake3::hash(serialized.as_bytes()).to_hex())
}

struct FocusReportInput {
    command: &'static str,
    loaded: LoadedFocus,
    mutated: bool,
    mutation_kind: &'static str,
    before_state_hash: Option<String>,
    after_state_hash: Option<String>,
    memory_statuses: Vec<FocusMemoryStatus>,
    explanations: Vec<FocusExplanation>,
    degraded: Vec<FocusDegradation>,
}

fn report(input: FocusReportInput) -> FocusReport {
    let state_hash = state_hash(&input.loaded.state);
    FocusReport {
        schema: FOCUS_COMMAND_SCHEMA_V1,
        command: input.command,
        version: env!("CARGO_PKG_VERSION"),
        workspace_path: input.loaded.workspace_path,
        storage_path: input.loaded.storage_path,
        state: input.loaded.state,
        state_hash,
        before_state_hash: input.before_state_hash,
        after_state_hash: input.after_state_hash,
        mutated: input.mutated,
        mutation_kind: input.mutation_kind,
        memory_statuses: input.memory_statuses,
        explanations: input.explanations,
        degraded: input.degraded,
    }
}

fn focus_validation_error(error: FocusValidationError) -> DomainError {
    DomainError::Usage {
        message: error.to_string(),
        repair: Some(format!(
            "Fix focus input that triggered `{}`.",
            error.code()
        )),
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn memory_id(seed: u128) -> MemoryId {
        MemoryId::from_uuid(Uuid::from_u128(seed))
    }

    fn workspace_id(seed: u128) -> WorkspaceId {
        WorkspaceId::from_uuid(Uuid::from_u128(seed))
    }

    fn focus_state(ids: &[MemoryId], capacity: usize) -> Result<FocusState, DomainError> {
        let mut state = FocusState::new(workspace_id(1), capacity, "2026-05-04T00:00:00Z")
            .map_err(focus_validation_error)?;
        for id in ids {
            state = state
                .with_item(
                    FocusItem::new(*id, "test reason", "2026-05-04T00:00:00Z")
                        .map_err(focus_validation_error)?,
                )
                .map_err(focus_validation_error)?;
        }
        Ok(state)
    }

    #[test]
    fn show_empty_state_does_not_write_storage() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let options = FocusShowOptions {
            workspace_path: dir.path().to_path_buf(),
        };
        let report = show_focus(&options).map_err(|error| error.message())?;
        ensure(report.mutated, false, "show mutation")?;
        ensure(report.state.item_count(), 0, "empty item count")?;
        ensure(
            report.storage_path.exists(),
            false,
            "show must not create focus state file",
        )
    }

    #[test]
    fn set_focus_writes_bounded_state_and_explainable_hashes() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first = memory_id(10).to_string();
        let second = memory_id(11).to_string();
        let report = set_focus(&FocusSetOptions {
            workspace_path: dir.path().to_path_buf(),
            memory_ids: vec![second.clone(), first.clone()],
            focal_memory_id: Some(first.clone()),
            pinned_memory_ids: vec![first.clone()],
            capacity: 2,
            reason: "test focus".to_owned(),
            provenance: vec!["test".to_owned()],
            scope: FocusScope {
                task_frame_id: Some("task-a".to_owned()),
                recorder_run_id: Some("run-a".to_owned()),
                handoff_id: None,
                profile: Some("resume".to_owned()),
            },
        })
        .map_err(|error| error.message())?;

        ensure(report.mutated, true, "set mutation")?;
        ensure(report.state.item_count(), 2, "item count")?;
        ensure(
            report.state.focal_memory_id.map(|id| id.to_string()),
            Some(first),
            "focal id",
        )?;
        ensure(report.state.pinned_count(), 1, "pinned count")?;
        ensure(report.storage_path.exists(), true, "state file exists")?;
        ensure(
            report.before_state_hash.is_some(),
            true,
            "before hash present",
        )?;
        ensure(
            report.after_state_hash.is_some(),
            true,
            "after hash present",
        )
    }

    #[test]
    fn add_focus_refuses_capacity_overflow_without_eviction() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first = memory_id(20).to_string();
        set_focus(&FocusSetOptions {
            workspace_path: dir.path().to_path_buf(),
            memory_ids: vec![first],
            focal_memory_id: None,
            pinned_memory_ids: Vec::new(),
            capacity: 1,
            reason: "seed".to_owned(),
            provenance: Vec::new(),
            scope: FocusScope::default(),
        })
        .map_err(|error| error.message())?;

        let overflow = add_focus(&FocusAddOptions {
            workspace_path: dir.path().to_path_buf(),
            memory_ids: vec![memory_id(21).to_string()],
            focal_memory_id: None,
            pinned_memory_ids: Vec::new(),
            capacity: None,
            reason: "overflow".to_owned(),
            provenance: Vec::new(),
            scope: FocusScope::default(),
        });
        ensure(
            overflow.map_err(|error| error.message()).is_err(),
            true,
            "overflow refused",
        )
    }

    #[test]
    fn add_focus_can_make_new_memory_focal_after_insert() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first = memory_id(24).to_string();
        let second = memory_id(25).to_string();
        set_focus(&FocusSetOptions {
            workspace_path: dir.path().to_path_buf(),
            memory_ids: vec![first],
            focal_memory_id: None,
            pinned_memory_ids: Vec::new(),
            capacity: 2,
            reason: "seed".to_owned(),
            provenance: Vec::new(),
            scope: FocusScope::default(),
        })
        .map_err(|error| error.message())?;

        let report = add_focus(&FocusAddOptions {
            workspace_path: dir.path().to_path_buf(),
            memory_ids: vec![second.clone()],
            focal_memory_id: Some(second.clone()),
            pinned_memory_ids: Vec::new(),
            capacity: None,
            reason: "new focal".to_owned(),
            provenance: Vec::new(),
            scope: FocusScope::default(),
        })
        .map_err(|error| error.message())?;

        ensure(
            report.state.focal_memory_id.map(|id| id.to_string()),
            Some(second),
            "new focal",
        )
    }

    #[test]
    fn remove_focus_clears_focal_when_removed() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first = memory_id(30).to_string();
        set_focus(&FocusSetOptions {
            workspace_path: dir.path().to_path_buf(),
            memory_ids: vec![first.clone()],
            focal_memory_id: Some(first.clone()),
            pinned_memory_ids: Vec::new(),
            capacity: 2,
            reason: "seed".to_owned(),
            provenance: Vec::new(),
            scope: FocusScope::default(),
        })
        .map_err(|error| error.message())?;
        let report = remove_focus(&FocusRemoveOptions {
            workspace_path: dir.path().to_path_buf(),
            memory_ids: vec![first],
            provenance: Vec::new(),
        })
        .map_err(|error| error.message())?;
        ensure(report.state.item_count(), 0, "item count")?;
        ensure(report.state.focal_memory_id, None, "focal cleared")
    }

    #[test]
    fn clear_focus_overwrites_empty_state_without_deleting_file() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        set_focus(&FocusSetOptions {
            workspace_path: dir.path().to_path_buf(),
            memory_ids: vec![memory_id(40).to_string()],
            focal_memory_id: None,
            pinned_memory_ids: Vec::new(),
            capacity: 2,
            reason: "seed".to_owned(),
            provenance: Vec::new(),
            scope: FocusScope::default(),
        })
        .map_err(|error| error.message())?;
        let report = clear_focus(&FocusClearOptions {
            workspace_path: dir.path().to_path_buf(),
            capacity: Some(3),
            provenance: Vec::new(),
            scope: FocusScope::default(),
        })
        .map_err(|error| error.message())?;
        ensure(
            report.storage_path.exists(),
            true,
            "state file still exists",
        )?;
        ensure(report.state.item_count(), 0, "cleared item count")?;
        ensure(report.state.capacity, 3, "new capacity")
    }

    #[cfg(unix)]
    #[test]
    fn set_focus_rejects_symlinked_metadata_parent() -> TestResult {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = dir.path().join("workspace");
        let real_metadata = dir.path().join("real-ee");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&real_metadata).map_err(|error| error.to_string())?;
        symlink(&real_metadata, workspace.join(".ee")).map_err(|error| error.to_string())?;

        let result = set_focus(&FocusSetOptions {
            workspace_path: workspace,
            memory_ids: vec![memory_id(45).to_string()],
            focal_memory_id: None,
            pinned_memory_ids: Vec::new(),
            capacity: 2,
            reason: "symlink guard".to_owned(),
            provenance: Vec::new(),
            scope: FocusScope::default(),
        });
        let error = result.expect_err("symlinked .ee parent should be rejected");
        ensure(
            error.message().contains("symlinked path component"),
            true,
            "symlinked .ee error message",
        )?;
        ensure(
            real_metadata.join("focus").join("state.json").exists(),
            false,
            "focus write must not follow symlinked .ee parent",
        )
    }

    #[cfg(unix)]
    #[test]
    fn show_focus_rejects_symlinked_state_file() -> TestResult {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = dir.path().join("workspace");
        let focus_dir = workspace.join(".ee").join("focus");
        std::fs::create_dir_all(&focus_dir).map_err(|error| error.to_string())?;

        let outside_state = dir.path().join("outside-focus.json");
        let state = focus_state(&[memory_id(46)], 2).map_err(|error| error.message())?;
        let mut body =
            serde_json::to_string_pretty(&state.data_json()).map_err(|error| error.to_string())?;
        body.push('\n');
        std::fs::write(&outside_state, body).map_err(|error| error.to_string())?;
        symlink(&outside_state, focus_dir.join("state.json")).map_err(|error| error.to_string())?;

        let result = show_focus(&FocusShowOptions {
            workspace_path: workspace,
        });
        let error = result.expect_err("symlinked focus state should be rejected");
        ensure(
            error.message().contains("symlinked path component"),
            true,
            "symlinked state error message",
        )
    }

    #[test]
    fn show_focus_rejects_state_directory_before_read() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir_all(focus_state_path(dir.path())).map_err(|error| error.to_string())?;

        let result = show_focus(&FocusShowOptions {
            workspace_path: dir.path().to_path_buf(),
        });
        let error = result.expect_err("focus state directory should be rejected");
        ensure(error.code(), "storage", "state directory error code")?;
        ensure(
            error.message().contains("not a regular file"),
            true,
            "state directory error message",
        )
    }

    #[test]
    fn active_focus_rejects_state_directory_before_cache_read() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir_all(focus_state_path(dir.path())).map_err(|error| error.to_string())?;

        let result = read_active_focus_state(dir.path());
        let error = result.expect_err("active focus state directory should be rejected");
        ensure(error.code(), "storage", "active state directory error code")?;
        ensure(
            error.message().contains("not a regular file"),
            true,
            "active state directory error message",
        )
    }

    #[test]
    fn write_focus_state_rejects_non_regular_final_path() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let state_path = focus_state_path(dir.path());
        std::fs::create_dir_all(&state_path).map_err(|error| error.to_string())?;
        let state = focus_state(&[memory_id(47)], 2).map_err(|error| error.message())?;

        let result = write_focus_state(&state_path, &state);
        let error = result.expect_err("directory focus state should be rejected on write");
        ensure(
            error.message().contains("not a regular file"),
            true,
            "non-regular write error message",
        )?;
        ensure(
            state_path.is_dir(),
            true,
            "non-regular focus state remains a directory",
        )
    }

    #[test]
    fn write_focus_state_rejects_existing_regular_temp_file_without_truncating() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let state_path = focus_state_path(dir.path());
        let temp_path = state_path.with_extension("json.tmp");
        let state = focus_state(&[memory_id(48)], 2).map_err(|error| error.message())?;
        std::fs::create_dir_all(temp_path.parent().expect("focus temp parent"))
            .map_err(|error| error.to_string())?;
        std::fs::write(&temp_path, "stale focus temp").map_err(|error| error.to_string())?;

        let result = write_focus_state(&state_path, &state);
        let error = result.expect_err("existing regular temp state should reject focus write");
        ensure(
            error.message().contains("already exists"),
            true,
            "existing temp error message",
        )?;
        ensure(
            std::fs::read_to_string(&temp_path).map_err(|error| error.to_string())?,
            "stale focus temp".to_owned(),
            "existing temp content remains unchanged",
        )?;
        ensure(
            state_path.exists(),
            false,
            "final focus state must not be published when temp exists",
        )
    }

    #[cfg(unix)]
    #[test]
    fn publish_focus_state_rechecks_final_symlink_before_rename() -> TestResult {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let state_path = focus_state_path(dir.path());
        let temp_path = state_path.with_extension("json.tmp");
        let outside_state = dir.path().join("outside-focus-state.json");
        let state = focus_state(&[memory_id(49)], 2).map_err(|error| error.message())?;
        let mut body =
            serde_json::to_string_pretty(&state.data_json()).map_err(|error| error.to_string())?;
        body.push('\n');
        std::fs::create_dir_all(temp_path.parent().expect("focus temp parent"))
            .map_err(|error| error.to_string())?;
        write_focus_state_temp_file(&temp_path, &body).map_err(|error| error.message())?;
        std::fs::write(&outside_state, "outside sentinel").map_err(|error| error.to_string())?;
        symlink(&outside_state, &state_path).map_err(|error| error.to_string())?;

        let error = publish_focus_state_temp_file(&state_path, &temp_path)
            .expect_err("final symlink should reject focus state publish");

        ensure(
            error.message().contains("symlinked path component")
                || error.message().contains("not a regular file"),
            true,
            "final symlink publish error",
        )?;
        ensure(
            std::fs::read_to_string(&outside_state).map_err(|error| error.to_string())?,
            "outside sentinel".to_owned(),
            "outside focus state remains unchanged",
        )?;
        ensure(
            std::fs::symlink_metadata(&temp_path)
                .map_err(|error| error.to_string())?
                .file_type()
                .is_file(),
            true,
            "temp focus state remains available after publish rejection",
        )
    }

    #[test]
    fn lookup_statuses_cover_missing_tombstoned_and_unverified() -> TestResult {
        let present = memory_id(50);
        let tombstoned = memory_id(51);
        let missing = memory_id(52);
        let state = focus_state(&[present, tombstoned, missing], 3).map_err(|e| e.message())?;
        let lookup = BTreeMap::from([
            (present.to_string(), FocusMemoryAvailability::Present),
            (tombstoned.to_string(), FocusMemoryAvailability::Tombstoned),
        ]);
        let statuses = focus_memory_statuses_from_lookup(&state, &lookup, true);
        ensure(
            statuses
                .iter()
                .find(|status| status.memory_id == missing.to_string())
                .map(|status| status.status),
            Some(FocusMemoryStatusKind::Missing),
            "missing status",
        )?;
        ensure(
            statuses
                .iter()
                .find(|status| status.memory_id == tombstoned.to_string())
                .map(|status| status.status),
            Some(FocusMemoryStatusKind::Tombstoned),
            "tombstoned status",
        )?;
        let unverified = focus_memory_statuses_from_lookup(&state, &BTreeMap::new(), false);
        ensure(
            unverified
                .iter()
                .all(|status| status.status == FocusMemoryStatusKind::Unverified),
            true,
            "unverified statuses",
        )
    }

    #[cfg(unix)]
    #[test]
    fn memory_lookup_refuses_symlinked_database_file() -> TestResult {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = dir.path().join("workspace");
        let ee_dir = workspace.join(".ee");
        std::fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
        let outside_db = dir.path().join("outside-ee.db");
        std::fs::write(&outside_db, b"").map_err(|error| error.to_string())?;
        symlink(&outside_db, ee_dir.join("ee.db")).map_err(|error| error.to_string())?;

        let lookup = memory_lookup_if_database_exists(&workspace, &[memory_id(60)]);
        ensure(
            lookup.is_none(),
            true,
            "symlinked focus memory DB lookup is refused",
        )
    }

    #[test]
    fn focus_report_degraded_entries_are_aggregated() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let mut report = show_focus(&FocusShowOptions {
            workspace_path: dir.path().to_path_buf(),
        })
        .map_err(|error| error.message())?;
        report.degraded = vec![
            FocusDegradation::low(
                "focus_missing_memory",
                "Focus state references memory IDs that are missing from the database.",
                Some("ee focus remove <memory-id> --json".to_owned()),
            ),
            FocusDegradation {
                code: "focus_missing_memory".to_owned(),
                severity: "medium".to_owned(),
                message: "Multiple focused memories are missing from the database.".to_owned(),
                repair: Some("ee focus clear --json".to_owned()),
            },
        ];

        let data = report.data_json();
        let degraded = data["degraded"]
            .as_array()
            .ok_or_else(|| "expected degraded array".to_owned())?;
        ensure(degraded.len(), 1, "aggregated degraded count")?;
        ensure(
            degraded[0]["code"].as_str(),
            Some("focus_missing_memory"),
            "aggregated code",
        )?;
        ensure(
            degraded[0]["severity"].as_str(),
            Some("medium"),
            "severity escalates",
        )?;
        ensure(
            degraded[0]["repair"].as_str(),
            Some("ee focus clear --json"),
            "repair follows highest severity",
        )?;
        ensure(
            degraded[0]["sources"].clone(),
            json!(["focus"]),
            "focus source label",
        )
    }
}
