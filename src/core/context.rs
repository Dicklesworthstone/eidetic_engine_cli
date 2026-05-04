//! Capability-narrowed command context.
//!
//! Every command handler accepts a [`CommandContext`] that bundles
//! - the active [`WorkspaceLocation`] (produced by EE-023),
//! - the per-request [`RequestBudget`] (EE-010), and
//! - a [`CapabilitySet`] naming which subsystems the handler may touch
//!   and at what [`AccessLevel`].
//!
//! Narrowing is element-wise `min` against a mask, so capabilities can
//! only contract — never widen — as control flows from the CLI entry
//! point down into subsystems. The narrowing law (`narrow(a, mask) ≤ a`
//! on every axis, with `≤` ordered as `None < Read < Write`) is the
//! load-bearing invariant: a downstream handler that holds a `Read`
//! capability for `db` cannot accidentally execute a write because the
//! narrow operation never produces a higher level than the input.
//!
//! EE-011 (this bead) ships only the type and its math. The wiring
//! that constructs a `CommandContext` from CLI arguments + workspace
//! discovery + a default capability set per command lives in EE-005 /
//! EE-018. The mapping from a capability denial to a stable
//! `degraded[]` code (e.g. `policy_capability_denied`) belongs to
//! EE-006 / EE-016. Strict scope: this module must not depend on any
//! of those landing first.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::config::WorkspaceLocation;
use crate::core::budget::RequestBudget;
use crate::core::focus::{focus_state_hash, focus_state_path, read_active_focus_state};
use crate::core::search::{SearchError, SearchOptions, SearchStatus, run_search};
use crate::db::{
    CreatePackItemInput, CreatePackOmissionInput, CreatePackRecordInput, DbConnection, StoredMemory,
};
use crate::models::{MemoryId, PackId, ProvenanceUri, TrustClass, UnitScore};
use crate::pack::{
    ContextPackProfile, ContextRequest, ContextRequestInput, ContextResponse,
    ContextResponseDegradation, ContextResponseSeverity, PackCandidate, PackCandidateInput,
    PackProvenance, PackSection, PackTrustSignal, TokenBudget, assemble_draft_with_profile,
    estimate_tokens_default, pack_item_provenance_json,
};

/// Per-subsystem permission level. `None < Read < Write` under the
/// derived `Ord`, which is what the narrowing law relies on.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
#[repr(u8)]
pub enum AccessLevel {
    /// The handler may not touch the subsystem at all.
    #[default]
    None = 0,
    /// The handler may observe state without mutating it.
    Read = 1,
    /// The handler may mutate the subsystem.
    Write = 2,
}

impl AccessLevel {
    /// Stable string representation suitable for log fields and future
    /// JSON renderers.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Read => "read",
            Self::Write => "write",
        }
    }

    /// `true` if at least `Read`.
    #[must_use]
    pub const fn allows_read(self) -> bool {
        matches!(self, Self::Read | Self::Write)
    }

    /// `true` if `Write`.
    #[must_use]
    pub const fn allows_write(self) -> bool {
        matches!(self, Self::Write)
    }

    /// Element-wise lattice meet (`min`) usable in `const` context.
    /// `Ord` derive would cover this for non-`const` callers, but
    /// narrowing math runs inside `const fn`s where `Ord::min` is not
    /// yet stable.
    #[must_use]
    pub const fn min_const(a: Self, b: Self) -> Self {
        if (a as u8) <= (b as u8) { a } else { b }
    }
}

/// Per-subsystem permission map. Each slot is independent; narrowing
/// a single dimension does not affect the others.
///
/// Adding a new subsystem here is a deliberate edit: every consumer
/// pattern-matches on the named slots, and the schema-drift gate
/// (EE-SCHEMA-DRIFT-001) will eventually pin the variant order.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CapabilitySet {
    /// FrankenSQLite source-of-truth database access.
    pub db: AccessLevel,
    /// Frankensearch / FTS5 lexical and vector indexes.
    pub search_index: AccessLevel,
    /// FrankenNetworkX graph snapshot artefacts.
    pub graph_snapshot: AccessLevel,
    /// `cass` subprocess invocation rights.
    pub cass_subprocess: AccessLevel,
    /// Workspace filesystem access beyond the database file.
    pub filesystem: AccessLevel,
    /// Outbound network access (off by default; only adapters may
    /// hold any non-`None` value here).
    pub network: AccessLevel,
    /// Append-only audit log writes. Reads are gated by `db`.
    pub audit_log: AccessLevel,
}

impl CapabilitySet {
    /// All subsystems set to [`AccessLevel::None`]. Useful as a
    /// starting point when explicitly opting in to capabilities.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            db: AccessLevel::None,
            search_index: AccessLevel::None,
            graph_snapshot: AccessLevel::None,
            cass_subprocess: AccessLevel::None,
            filesystem: AccessLevel::None,
            network: AccessLevel::None,
            audit_log: AccessLevel::None,
        }
    }

    /// All subsystems set to [`AccessLevel::Read`]. Suitable as the
    /// starting capability set for read-only commands such as
    /// `ee status`, `ee search`, `ee why`, `ee context`.
    #[must_use]
    pub const fn read_only() -> Self {
        Self {
            db: AccessLevel::Read,
            search_index: AccessLevel::Read,
            graph_snapshot: AccessLevel::Read,
            cass_subprocess: AccessLevel::Read,
            filesystem: AccessLevel::Read,
            network: AccessLevel::None,
            audit_log: AccessLevel::Read,
        }
    }

    /// Every subsystem set to [`AccessLevel::Write`] except `network`,
    /// which stays `None` because v1 is local-first and outbound
    /// network is opt-in per adapter (see README §Local First).
    #[must_use]
    pub const fn full_local() -> Self {
        Self {
            db: AccessLevel::Write,
            search_index: AccessLevel::Write,
            graph_snapshot: AccessLevel::Write,
            cass_subprocess: AccessLevel::Write,
            filesystem: AccessLevel::Write,
            network: AccessLevel::None,
            audit_log: AccessLevel::Write,
        }
    }

    /// Element-wise narrow against `mask`. Each slot becomes
    /// `min(self.slot, mask.slot)`.
    ///
    /// The narrowing law: for every slot `s`,
    /// `self.narrow(mask).s ≤ self.s` and
    /// `self.narrow(mask).s ≤ mask.s`. Repeated narrowing therefore
    /// never widens.
    #[must_use]
    pub const fn narrow(self, mask: Self) -> Self {
        Self {
            db: AccessLevel::min_const(self.db, mask.db),
            search_index: AccessLevel::min_const(self.search_index, mask.search_index),
            graph_snapshot: AccessLevel::min_const(self.graph_snapshot, mask.graph_snapshot),
            cass_subprocess: AccessLevel::min_const(self.cass_subprocess, mask.cass_subprocess),
            filesystem: AccessLevel::min_const(self.filesystem, mask.filesystem),
            network: AccessLevel::min_const(self.network, mask.network),
            audit_log: AccessLevel::min_const(self.audit_log, mask.audit_log),
        }
    }
}

/// Bundle threaded through every command handler.
///
/// Ownership is `Clone` rather than `Copy` because [`WorkspaceLocation`]
/// owns `PathBuf`s. Cloning is cheap relative to a command's actual work
/// and keeps narrowing free of borrow gymnastics.
#[derive(Clone, Debug)]
pub struct CommandContext {
    workspace: WorkspaceLocation,
    budget: RequestBudget,
    capabilities: CapabilitySet,
}

impl CommandContext {
    /// Build a new context. The CLI entry point constructs one of
    /// these from the resolved workspace, the parsed CLI flags, and
    /// the per-command capability default.
    #[must_use]
    pub const fn new(
        workspace: WorkspaceLocation,
        budget: RequestBudget,
        capabilities: CapabilitySet,
    ) -> Self {
        Self {
            workspace,
            budget,
            capabilities,
        }
    }

    /// The active workspace location.
    #[must_use]
    pub const fn workspace(&self) -> &WorkspaceLocation {
        &self.workspace
    }

    /// Convenience accessor for the workspace root directory.
    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        self.workspace.root.as_path()
    }

    /// The per-request budget. Read-only access for handlers that
    /// only need to consult deadlines; mutating access goes through
    /// [`Self::budget_mut`].
    #[must_use]
    pub const fn budget(&self) -> &RequestBudget {
        &self.budget
    }

    /// Mutable access to the per-request budget so handlers can
    /// record consumption (`record_tokens`, `record_io_bytes`, etc.).
    #[must_use]
    pub const fn budget_mut(&mut self) -> &mut RequestBudget {
        &mut self.budget
    }

    /// The current capability set.
    #[must_use]
    pub const fn capabilities(&self) -> CapabilitySet {
        self.capabilities
    }

    /// Return a clone whose capability set is the element-wise `min`
    /// of `self.capabilities` and `mask`. Workspace and budget pass
    /// through unchanged so cancellation / deadline state is
    /// preserved across narrowing.
    #[must_use]
    pub fn with_narrowed_capabilities(&self, mask: CapabilitySet) -> Self {
        Self {
            workspace: self.workspace.clone(),
            budget: self.budget,
            capabilities: self.capabilities.narrow(mask),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ContextPackOptions {
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
    pub index_dir: Option<PathBuf>,
    pub query: String,
    pub filters: crate::models::QueryFilters,
    pub profile: Option<ContextPackProfile>,
    pub max_tokens: Option<u32>,
    pub candidate_pool: Option<u32>,
    pub filters: crate::models::QueryFilters,
}

#[derive(Debug)]
pub enum ContextPackError {
    Storage(String),
    Search(SearchError),
    Pack(String),
}

impl ContextPackError {
    #[must_use]
    pub fn repair_hint(&self) -> Option<&str> {
        match self {
            Self::Storage(_) => Some("ee init --workspace ."),
            Self::Search(error) => error.repair_hint(),
            Self::Pack(_) => Some("ee context --help"),
        }
    }
}

impl std::fmt::Display for ContextPackError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(message) | Self::Pack(message) => formatter.write_str(message),
            Self::Search(error) => std::fmt::Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for ContextPackError {}

pub fn run_context_pack(options: &ContextPackOptions) -> Result<ContextResponse, ContextPackError> {
    let request = ContextRequest::new(ContextRequestInput {
        query: options.query.clone(),
        profile: options.profile,
        max_tokens: options.max_tokens,
        candidate_pool: options.candidate_pool,
        sections: Vec::new(),
    })
    .map_err(|error| ContextPackError::Pack(error.to_string()))?;

    let database_path = options
        .database_path
        .clone()
        .unwrap_or_else(|| options.workspace_path.join(".ee").join("ee.db"));
    if !database_path.exists() {
        return Err(ContextPackError::Storage(format!(
            "Database not found at {}",
            database_path.display()
        )));
    }

    let connection = DbConnection::open_file(&database_path)
        .map_err(|error| ContextPackError::Storage(format!("Failed to open database: {error}")))?;

    let mut search_report = run_search(&SearchOptions {
        workspace_path: options.workspace_path.clone(),
        database_path: Some(database_path),
        index_dir: options.index_dir.clone(),
        query: request.query.clone(),
        limit: request.candidate_pool,
        explain: false,
    })
    .map_err(ContextPackError::Search)?;

    if search_report.status == SearchStatus::IndexError {
        return Err(ContextPackError::Search(SearchError::Index(
            search_report.errors.join("; "),
        )));
    }

    let mut degraded = Vec::new();

    // Apply query filters to search results
    if !options.filters.is_empty() {
        let pre_filter_count = search_report.results.len();
        search_report.results.retain(|hit| {
            options.filters.matches(hit.metadata.as_ref())
        });
        let filtered_count = pre_filter_count - search_report.results.len();
        if filtered_count > 0 {
            push_degradation(
                &mut degraded,
                "context_filtered_results",
                ContextResponseSeverity::Low,
                format!(
                    "{} of {} search results excluded by query filters.",
                    filtered_count, pre_filter_count
                ),
                None,
            );
        }
    }
    if search_report.status == SearchStatus::NoResults {
        push_degradation(
            &mut degraded,
            "context_no_results",
            ContextResponseSeverity::Low,
            "Search completed but returned no candidate memories.",
            Some("ee remember --workspace . --level procedural --kind rule \"...\"".to_string()),
        );
    }

    let mut candidates = candidates_from_search(&connection, &search_report, &mut degraded);
    match read_active_focus_state(&options.workspace_path) {
        Ok(Some(focus_state)) => {
            candidates.extend(focus_candidates_from_state(
                &connection,
                &options.workspace_path,
                &focus_state,
                &mut degraded,
            ));
        }
        Ok(None) => {}
        Err(error) => push_degradation(
            &mut degraded,
            "context_focus_state_unavailable",
            ContextResponseSeverity::Low,
            format!("Passive focus state could not be read: {}", error.message()),
            Some("ee focus show --json".to_string()),
        ),
    }
    let budget = match options.max_tokens {
        Some(max_tokens) => TokenBudget::new(max_tokens)
            .map_err(|error| ContextPackError::Pack(error.to_string()))?,
        None => TokenBudget::default_context(),
    };
    let mut draft =
        assemble_draft_with_profile(request.profile, request.query.clone(), budget, candidates)
            .map_err(|error| ContextPackError::Pack(error.to_string()))?;

    draft.hash = Some(compute_pack_hash(&request, &draft, &degraded));

    let mut response_degraded = degraded.clone();

    if let Err(persist_error) = persist_pack_record(
        &connection,
        &options.workspace_path,
        &request,
        &draft,
        &degraded,
    ) {
        push_degradation(
            &mut response_degraded,
            "context_pack_persist_failed",
            ContextResponseSeverity::Medium,
            format!("Pack assembled but persistence failed: {persist_error}"),
            Some("ee status --json".to_string()),
        );
    }

    ContextResponse::new(request, draft, response_degraded)
        .map_err(|error| ContextPackError::Pack(error.to_string()))
}

fn persist_pack_record(
    connection: &DbConnection,
    workspace_path: &Path,
    request: &ContextRequest,
    draft: &crate::pack::PackDraft,
    degraded: &[ContextResponseDegradation],
) -> Result<(), String> {
    let workspace = connection
        .get_workspace_by_path(&workspace_path.display().to_string())
        .map_err(|e| format!("workspace lookup failed: {e}"))?
        .ok_or_else(|| "workspace not found".to_string())?;

    let pack_id = PackId::now();
    let pack_hash = draft
        .hash
        .clone()
        .unwrap_or_else(|| compute_pack_hash(request, draft, degraded));

    let degraded_json = if degraded.is_empty() {
        None
    } else {
        serde_json::to_string(
            &degraded
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "code": d.code,
                        "severity": d.severity.as_str(),
                        "message": d.message,
                        "repair": d.repair,
                    })
                })
                .collect::<Vec<_>>(),
        )
        .ok()
    };

    let input = CreatePackRecordInput {
        workspace_id: workspace.id.clone(),
        query: request.query.clone(),
        profile: request.profile.as_str().to_string(),
        max_tokens: request.budget.max_tokens(),
        used_tokens: draft.used_tokens,
        item_count: draft.items.len() as u32,
        omitted_count: draft.omitted.len() as u32,
        pack_hash,
        degraded_json,
        created_by: Some("ee context".to_string()),
    };

    let items: Vec<CreatePackItemInput> = draft
        .items
        .iter()
        .map(|item| CreatePackItemInput {
            pack_id: pack_id.to_string(),
            memory_id: item.memory_id.to_string(),
            rank: item.rank,
            section: item.section.as_str().to_string(),
            estimated_tokens: item.estimated_tokens,
            relevance: item.relevance.into_inner(),
            utility: item.utility.into_inner(),
            why: item.why.clone(),
            diversity_key: item.diversity_key.clone(),
            provenance_json: pack_item_provenance_json(&item.provenance),
            trust_class: item.trust.class.as_str().to_string(),
            trust_subclass: item.trust.subclass.clone(),
        })
        .collect();

    let omissions: Vec<CreatePackOmissionInput> = draft
        .omitted
        .iter()
        .map(|omission| CreatePackOmissionInput {
            pack_id: pack_id.to_string(),
            memory_id: omission.memory_id.to_string(),
            estimated_tokens: omission.estimated_tokens,
            reason: omission.reason.as_str().to_string(),
        })
        .collect();

    connection
        .insert_pack_record(&pack_id.to_string(), &input, &items, &omissions)
        .map_err(|e| format!("insert failed: {e}"))
}

fn compute_pack_hash(
    request: &ContextRequest,
    draft: &crate::pack::PackDraft,
    degraded: &[ContextResponseDegradation],
) -> String {
    use blake3::Hasher;
    let mut hasher = Hasher::new();
    hasher.update(request.query.as_bytes());
    hasher.update(request.profile.as_str().as_bytes());
    hasher.update(&request.budget.max_tokens().to_le_bytes());
    hasher.update(&draft.used_tokens.to_le_bytes());
    for item in &draft.items {
        hasher.update(item.memory_id.to_string().as_bytes());
        hasher.update(&item.rank.to_le_bytes());
        hasher.update(item.section.as_str().as_bytes());
        hasher.update(item.content.as_bytes());
        hasher.update(&item.estimated_tokens.to_le_bytes());
        hasher.update(&item.relevance.into_inner().to_le_bytes());
        hasher.update(&item.utility.into_inner().to_le_bytes());
        hasher.update(item.why.as_bytes());
        for provenance in &item.provenance {
            hasher.update(provenance.uri.to_string().as_bytes());
            hasher.update(provenance.note.as_bytes());
        }
        if let Some(diversity_key) = &item.diversity_key {
            hasher.update(diversity_key.as_bytes());
        }
        hasher.update(item.trust.class.as_str().as_bytes());
        if let Some(subclass) = &item.trust.subclass {
            hasher.update(subclass.as_bytes());
        }
    }
    for omission in &draft.omitted {
        hasher.update(omission.memory_id.to_string().as_bytes());
        hasher.update(&omission.estimated_tokens.to_le_bytes());
        hasher.update(omission.reason.as_str().as_bytes());
    }
    for degradation in degraded {
        hasher.update(degradation.code.as_bytes());
        hasher.update(degradation.severity.as_str().as_bytes());
        hasher.update(degradation.message.as_bytes());
        if let Some(repair) = &degradation.repair {
            hasher.update(repair.as_bytes());
        }
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn candidates_from_search(
    connection: &DbConnection,
    search_report: &crate::core::search::SearchReport,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Vec<PackCandidate> {
    let mut candidates = Vec::new();
    for hit in &search_report.results {
        match candidate_from_hit(connection, hit, &search_report.query, degraded) {
            Some(candidate) => candidates.push(candidate),
            None => push_degradation(
                degraded,
                "context_candidate_skipped",
                ContextResponseSeverity::Low,
                format!(
                    "Search hit {} could not be converted into a pack candidate.",
                    hit.doc_id
                ),
                Some("ee index rebuild --workspace .".to_string()),
            ),
        }
    }
    candidates
}

fn focus_candidates_from_state(
    connection: &DbConnection,
    workspace_path: &Path,
    focus_state: &crate::models::FocusState,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Vec<PackCandidate> {
    let mut candidates = Vec::new();
    let focus_hash = focus_state_hash(focus_state);
    let storage_path = focus_state_path(workspace_path).display().to_string();
    for item in &focus_state.items {
        match focus_candidate_from_item(
            connection,
            item,
            focus_state,
            &focus_hash,
            &storage_path,
            degraded,
        ) {
            Some(candidate) => candidates.push(candidate),
            None => push_degradation(
                degraded,
                "context_focus_candidate_skipped",
                ContextResponseSeverity::Low,
                format!(
                    "Focused memory {} could not be converted into a pack candidate.",
                    item.memory_id
                ),
                Some(format!("ee focus remove {} --json", item.memory_id)),
            ),
        }
    }
    candidates
}

fn focus_candidate_from_item(
    connection: &DbConnection,
    item: &crate::models::FocusItem,
    focus_state: &crate::models::FocusState,
    focus_hash: &str,
    storage_path: &str,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<PackCandidate> {
    let memory = match connection.get_memory(&item.memory_id.to_string()) {
        Ok(Some(memory)) if memory.tombstoned_at.is_none() => memory,
        Ok(Some(_)) => {
            push_degradation(
                degraded,
                "context_focus_tombstoned_memory",
                ContextResponseSeverity::Low,
                format!(
                    "Focused memory {} is tombstoned and was excluded from context.",
                    item.memory_id
                ),
                Some(format!("ee focus remove {} --json", item.memory_id)),
            );
            return None;
        }
        Ok(None) => {
            push_degradation(
                degraded,
                "context_focus_missing_memory",
                ContextResponseSeverity::Low,
                format!(
                    "Focused memory {} is missing and was excluded from context.",
                    item.memory_id
                ),
                Some(format!("ee focus remove {} --json", item.memory_id)),
            );
            return None;
        }
        Err(error) => {
            push_degradation(
                degraded,
                "context_focus_memory_lookup_unavailable",
                ContextResponseSeverity::Low,
                format!(
                    "Focused memory {} could not be loaded: {error}",
                    item.memory_id
                ),
                Some("ee status --json".to_string()),
            );
            return None;
        }
    };
    let tags = connection
        .get_memory_tags(&memory.id)
        .unwrap_or_else(|_| Vec::new());
    let mut provenance = Vec::new();
    if let Some(memory_provenance) = provenance_for_memory(&memory, item.memory_id, degraded) {
        provenance.push(memory_provenance);
    }
    if let Ok(focus_provenance) = PackProvenance::new(
        ProvenanceUri::File {
            path: storage_path.to_owned(),
            span: None,
        },
        format!(
            "Passive focus state {focus_hash} included memory {}; reason={}; provenance={}",
            item.memory_id,
            item.reason,
            item.provenance.join(",")
        ),
    ) {
        provenance.push(focus_provenance);
    }
    let relevance = focus_relevance(item, focus_state)?;
    let utility = unit_score(memory.utility.max(0.75))?;
    let why = focus_candidate_why(item, focus_state, focus_hash);
    let candidate = PackCandidate::new(PackCandidateInput {
        memory_id: item.memory_id,
        section: section_for_memory(&memory),
        content: memory.content.clone(),
        estimated_tokens: estimate_tokens_default(&memory.content),
        relevance,
        utility,
        provenance,
        why,
    })
    .ok()?;

    Some(
        candidate
            .with_diversity_key(diversity_key_for_memory(&memory, &tags))
            .with_trust_signal(trust_signal_for_memory(&memory, item.memory_id, degraded)),
    )
}

fn focus_relevance(
    item: &crate::models::FocusItem,
    focus_state: &crate::models::FocusState,
) -> Option<UnitScore> {
    let value = if focus_state.focal_memory_id == Some(item.memory_id) {
        1.0
    } else if item.pinned {
        0.97
    } else {
        0.94
    };
    unit_score(value)
}

fn focus_candidate_why(
    item: &crate::models::FocusItem,
    focus_state: &crate::models::FocusState,
    focus_hash: &str,
) -> String {
    format!(
        "Selected as passive active-memory input: focus_state_hash={focus_hash}; focal={}; pinned={}; capacity={}; reason={}; provenance={}; source=ee_focus_state; no hidden mutation or agent-plan inference occurred.",
        focus_state.focal_memory_id == Some(item.memory_id),
        item.pinned,
        focus_state.capacity,
        item.reason,
        item.provenance.join(",")
    )
}

fn candidate_from_hit(
    connection: &DbConnection,
    hit: &crate::core::search::SearchHit,
    query: &str,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<PackCandidate> {
    let (memory_id, artifact_id) = match MemoryId::from_str(&hit.doc_id) {
        Ok(id) => (id, None),
        Err(_) => artifact_linked_memory_id(connection, hit, degraded)?,
    };
    let memory = match connection.get_memory(&memory_id.to_string()) {
        Ok(Some(memory)) if memory.tombstoned_at.is_none() => memory,
        Ok(_) | Err(_) => return None,
    };
    let tags = match connection.get_memory_tags(&memory.id) {
        Ok(tags) => tags,
        Err(error) => {
            push_degradation(
                degraded,
                "context_memory_tags_unavailable",
                ContextResponseSeverity::Low,
                format!("Tags for memory {} could not be loaded: {error}", memory.id),
                Some(format!("ee memory show {} --json", memory.id)),
            );
            Vec::new()
        }
    };
    let provenance = provenance_for_memory(&memory, memory_id, degraded)?;
    let relevance = unit_score(hit.score)?;
    let utility = unit_score(memory.utility)?;
    let content = memory.content.clone();
    let why = candidate_selection_why(
        query,
        hit.source.as_str(),
        hit.score,
        memory.utility,
        artifact_id.as_deref(),
    );
    let candidate = PackCandidate::new(PackCandidateInput {
        memory_id,
        section: section_for_memory(&memory),
        content,
        estimated_tokens: estimate_tokens_default(&memory.content),
        relevance,
        utility,
        provenance: vec![provenance],
        why,
    })
    .ok()?;

    Some(
        candidate
            .with_diversity_key(diversity_key_for_memory(&memory, &tags))
            .with_trust_signal(trust_signal_for_memory(&memory, memory_id, degraded)),
    )
}

fn candidate_selection_why(
    query: &str,
    search_source: &str,
    search_score: f32,
    memory_utility: f32,
    artifact_id: Option<&str>,
) -> String {
    let (source_reference, utility_field) = match artifact_id {
        Some(artifact_id) => (
            format!(
                "source=registered_artifact artifact_id={artifact_id} search_source={search_source}"
            ),
            "linked_memory.utility",
        ),
        None => (
            format!("source=memory search_source={search_source}"),
            "memory.utility",
        ),
    };

    format!(
        "Deterministic retrieval explanation for query `{query}`: {source_reference}; score_components=[relevance=unit_score(search_hit.score) with search_hit.score={search_score:.4}, utility=unit_score({utility_field}) with {utility_field}={memory_utility:.4}]; formula=unit_score(field)=clamp(field, 0.0, 1.0) for finite fields, otherwise 0.0; inputs are stored memory/link fields and the explicit search hit, not agent reasoning."
    )
}

fn artifact_linked_memory_id(
    connection: &DbConnection,
    hit: &crate::core::search::SearchHit,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<(MemoryId, Option<String>)> {
    let has_artifact_metadata = hit
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("source"))
        .and_then(serde_json::Value::as_str)
        == Some("artifact");
    if !has_artifact_metadata && !is_registered_artifact_hit(connection, &hit.doc_id, degraded) {
        return None;
    }

    let links = match connection.list_artifact_links(&hit.doc_id) {
        Ok(links) => links,
        Err(error) => {
            push_degradation(
                degraded,
                "context_artifact_links_unavailable",
                ContextResponseSeverity::Low,
                format!(
                    "Artifact links for {} could not be loaded: {error}",
                    hit.doc_id
                ),
                Some(format!("ee artifact inspect {} --json", hit.doc_id)),
            );
            return None;
        }
    };

    for link in links {
        if link.target_type != "memory" {
            continue;
        }
        match MemoryId::from_str(&link.target_id) {
            Ok(memory_id) => return Some((memory_id, Some(hit.doc_id.clone()))),
            Err(error) => push_degradation(
                degraded,
                "context_artifact_memory_link_invalid",
                ContextResponseSeverity::Low,
                format!(
                    "Artifact {} links to invalid memory id `{}`: {error}",
                    hit.doc_id, link.target_id
                ),
                Some(format!("ee artifact inspect {} --json", hit.doc_id)),
            ),
        }
    }

    push_degradation(
        degraded,
        "context_artifact_unlinked",
        ContextResponseSeverity::Low,
        format!(
            "Artifact {} matched search but has no valid memory link for context packing.",
            hit.doc_id
        ),
        Some("ee artifact register <path> --link-memory <memory-id> --json".to_string()),
    );
    None
}

fn is_registered_artifact_hit(
    connection: &DbConnection,
    artifact_id: &str,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> bool {
    if !is_registry_artifact_id(artifact_id) {
        return false;
    }

    match connection.get_artifact(artifact_id) {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(error) => {
            push_degradation(
                degraded,
                "context_artifact_lookup_unavailable",
                ContextResponseSeverity::Low,
                format!("Artifact {artifact_id} could not be loaded: {error}"),
                Some(format!("ee artifact inspect {artifact_id} --json")),
            );
            false
        }
    }
}

fn is_registry_artifact_id(value: &str) -> bool {
    value.len() == 30
        && value.starts_with("art_")
        && value.strip_prefix("art_").is_some_and(|suffix| {
            suffix
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
}

fn trust_signal_for_memory(
    memory: &StoredMemory,
    memory_id: MemoryId,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> PackTrustSignal {
    let trust_class = match TrustClass::from_str(&memory.trust_class) {
        Ok(class) => class,
        Err(error) => {
            push_degradation(
                degraded,
                "context_invalid_trust_class",
                ContextResponseSeverity::Medium,
                format!(
                    "Memory {} has invalid trust class `{}`: {error}",
                    memory.id, memory.trust_class
                ),
                Some(format!("ee memory show {memory_id} --json")),
            );
            TrustClass::AgentAssertion
        }
    };
    PackTrustSignal::new(trust_class, memory.trust_subclass.clone())
}

fn provenance_for_memory(
    memory: &StoredMemory,
    memory_id: MemoryId,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<PackProvenance> {
    let uri = match memory.provenance_uri.as_deref() {
        Some(raw) => match ProvenanceUri::from_str(raw) {
            Ok(uri) => uri,
            Err(error) => {
                push_degradation(
                    degraded,
                    "context_invalid_provenance",
                    ContextResponseSeverity::Low,
                    format!("Memory {} has invalid provenance URI: {error}", memory.id),
                    Some(format!("ee memory show {} --json", memory.id)),
                );
                ProvenanceUri::EeMemory(memory_id)
            }
        },
        None => ProvenanceUri::EeMemory(memory_id),
    };
    PackProvenance::new(
        uri,
        format!("Memory {} selected for context pack", memory.id),
    )
    .ok()
}

fn section_for_memory(memory: &StoredMemory) -> PackSection {
    match (memory.level.as_str(), memory.kind.as_str()) {
        ("procedural", _) | (_, "rule" | "convention" | "playbook-step") => {
            PackSection::ProceduralRules
        }
        (_, "decision") => PackSection::Decisions,
        (_, "failure" | "anti-pattern" | "risk") => PackSection::Failures,
        ("episodic", _) => PackSection::Evidence,
        _ => PackSection::Artifacts,
    }
}

fn diversity_key_for_memory(memory: &StoredMemory, tags: &[String]) -> String {
    let tag = tags.first().map_or("untagged", String::as_str);
    format!("{}:{}:{}", memory.level, memory.kind, tag)
}

fn unit_score(value: f32) -> Option<UnitScore> {
    let bounded = if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    };
    UnitScore::parse(bounded).ok()
}

fn push_degradation(
    degraded: &mut Vec<ContextResponseDegradation>,
    code: &str,
    severity: ContextResponseSeverity,
    message: impl Into<String>,
    repair: Option<String>,
) {
    if let Ok(entry) = ContextResponseDegradation::new(code, severity, message, repair) {
        degraded.push(entry);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        AccessLevel, CapabilitySet, CommandContext, candidate_selection_why, focus_candidate_why,
        focus_relevance, unit_score,
    };
    use crate::config::WorkspaceLocation;
    use crate::core::budget::RequestBudget;
    use crate::models::{FocusItem, FocusState, MemoryId, WorkspaceId};

    fn workspace_at(root: &str) -> WorkspaceLocation {
        WorkspaceLocation::new(PathBuf::from(root))
    }

    fn ctx(caps: CapabilitySet) -> CommandContext {
        CommandContext::new(
            workspace_at("/tmp/ee-test-workspace"),
            RequestBudget::unbounded(),
            caps,
        )
    }

    #[test]
    fn access_level_default_is_none() {
        assert_eq!(AccessLevel::default(), AccessLevel::None);
    }

    #[test]
    fn access_level_ordering_is_none_lt_read_lt_write() {
        assert!(AccessLevel::None < AccessLevel::Read);
        assert!(AccessLevel::Read < AccessLevel::Write);
        assert!(AccessLevel::None < AccessLevel::Write);
    }

    #[test]
    fn access_level_strings_are_stable() {
        assert_eq!(AccessLevel::None.as_str(), "none");
        assert_eq!(AccessLevel::Read.as_str(), "read");
        assert_eq!(AccessLevel::Write.as_str(), "write");
    }

    #[test]
    fn access_level_allows_read_and_write_predicates() {
        assert!(!AccessLevel::None.allows_read());
        assert!(!AccessLevel::None.allows_write());
        assert!(AccessLevel::Read.allows_read());
        assert!(!AccessLevel::Read.allows_write());
        assert!(AccessLevel::Write.allows_read());
        assert!(AccessLevel::Write.allows_write());
    }

    #[test]
    fn access_level_min_const_returns_lesser() {
        assert_eq!(
            AccessLevel::min_const(AccessLevel::None, AccessLevel::Write),
            AccessLevel::None,
        );
        assert_eq!(
            AccessLevel::min_const(AccessLevel::Read, AccessLevel::Write),
            AccessLevel::Read,
        );
        assert_eq!(
            AccessLevel::min_const(AccessLevel::Read, AccessLevel::Read),
            AccessLevel::Read,
        );
    }

    #[test]
    fn capability_set_constructors_are_consistent() {
        let n = CapabilitySet::none();
        assert_eq!(n.db, AccessLevel::None);
        assert_eq!(n.network, AccessLevel::None);

        let r = CapabilitySet::read_only();
        assert_eq!(r.db, AccessLevel::Read);
        assert_eq!(r.search_index, AccessLevel::Read);
        assert_eq!(r.graph_snapshot, AccessLevel::Read);
        assert_eq!(r.cass_subprocess, AccessLevel::Read);
        assert_eq!(r.filesystem, AccessLevel::Read);
        assert_eq!(r.audit_log, AccessLevel::Read);
        // Network stays None even in read_only because v1 is
        // local-first and outbound network is opt-in per adapter.
        assert_eq!(r.network, AccessLevel::None);

        let f = CapabilitySet::full_local();
        assert_eq!(f.db, AccessLevel::Write);
        assert_eq!(f.search_index, AccessLevel::Write);
        assert_eq!(f.graph_snapshot, AccessLevel::Write);
        assert_eq!(f.cass_subprocess, AccessLevel::Write);
        assert_eq!(f.filesystem, AccessLevel::Write);
        assert_eq!(f.audit_log, AccessLevel::Write);
        assert_eq!(f.network, AccessLevel::None);
    }

    #[test]
    fn narrow_against_full_returns_self() {
        // full_local has Write everywhere except network; narrowing a
        // read_only set against it must leave the read_only set
        // unchanged because every slot of read_only is already <= the
        // matching full_local slot.
        let r = CapabilitySet::read_only();
        assert_eq!(r.narrow(CapabilitySet::full_local()), r);
    }

    #[test]
    fn narrow_against_none_zeroes_every_slot() {
        let f = CapabilitySet::full_local();
        assert_eq!(f.narrow(CapabilitySet::none()), CapabilitySet::none());
    }

    #[test]
    fn narrow_with_mixed_mask_is_elementwise_min() {
        let original = CapabilitySet {
            db: AccessLevel::Write,
            search_index: AccessLevel::Write,
            graph_snapshot: AccessLevel::Write,
            cass_subprocess: AccessLevel::Write,
            filesystem: AccessLevel::Write,
            network: AccessLevel::Write,
            audit_log: AccessLevel::Write,
        };
        let mask = CapabilitySet {
            db: AccessLevel::Read,
            search_index: AccessLevel::None,
            graph_snapshot: AccessLevel::Write,
            cass_subprocess: AccessLevel::Read,
            filesystem: AccessLevel::None,
            network: AccessLevel::None,
            audit_log: AccessLevel::Write,
        };
        let narrowed = original.narrow(mask);
        assert_eq!(narrowed.db, AccessLevel::Read);
        assert_eq!(narrowed.search_index, AccessLevel::None);
        assert_eq!(narrowed.graph_snapshot, AccessLevel::Write);
        assert_eq!(narrowed.cass_subprocess, AccessLevel::Read);
        assert_eq!(narrowed.filesystem, AccessLevel::None);
        assert_eq!(narrowed.network, AccessLevel::None);
        assert_eq!(narrowed.audit_log, AccessLevel::Write);
    }

    #[test]
    fn narrow_is_monotone_and_never_widens() {
        // Repeated narrowing is monotone non-increasing on every axis.
        let starting = CapabilitySet::full_local();
        let mask_a = CapabilitySet::read_only();
        let mask_b = CapabilitySet {
            db: AccessLevel::None,
            ..CapabilitySet::read_only()
        };
        let once = starting.narrow(mask_a);
        let twice = once.narrow(mask_b);

        // Sanity: once is read_only because full_local was at or above
        // read_only on every slot.
        assert_eq!(once, mask_a);
        // After narrowing again with mask_b (which zeros db), the db
        // axis must drop and no other axis may widen.
        assert!(twice.db <= once.db);
        assert!(twice.search_index <= once.search_index);
        assert!(twice.graph_snapshot <= once.graph_snapshot);
        assert!(twice.cass_subprocess <= once.cass_subprocess);
        assert!(twice.filesystem <= once.filesystem);
        assert!(twice.network <= once.network);
        assert!(twice.audit_log <= once.audit_log);
        assert_eq!(twice.db, AccessLevel::None);
    }

    #[test]
    fn narrow_property_holds_for_a_curated_corpus() {
        // Property restated as a deterministic table so the test runs
        // without a property-test crate dependency. Each row is
        // (initial, mask); for every row, narrow(initial, mask).slot
        // <= initial.slot && narrow(initial, mask).slot <= mask.slot.
        let levels = [AccessLevel::None, AccessLevel::Read, AccessLevel::Write];
        for db_a in levels {
            for db_b in levels {
                for fs_a in levels {
                    for fs_b in levels {
                        let initial = CapabilitySet {
                            db: db_a,
                            filesystem: fs_a,
                            ..CapabilitySet::full_local()
                        };
                        let mask = CapabilitySet {
                            db: db_b,
                            filesystem: fs_b,
                            ..CapabilitySet::full_local()
                        };
                        let narrowed = initial.narrow(mask);
                        assert!(narrowed.db <= initial.db);
                        assert!(narrowed.db <= mask.db);
                        assert!(narrowed.filesystem <= initial.filesystem);
                        assert!(narrowed.filesystem <= mask.filesystem);
                    }
                }
            }
        }
    }

    #[test]
    fn command_context_exposes_workspace_and_budget() {
        let context = ctx(CapabilitySet::read_only());
        assert_eq!(
            context.workspace_root(),
            PathBuf::from("/tmp/ee-test-workspace")
        );
        assert!(context.budget().remaining_wall_clock().is_none());
        assert_eq!(context.capabilities(), CapabilitySet::read_only());
    }

    #[test]
    fn budget_mut_lets_handlers_record_consumption() {
        let mut context = ctx(CapabilitySet::read_only());
        context.budget_mut().record_tokens(42);
        context.budget_mut().record_io_bytes(1024);
        assert_eq!(context.budget().tokens_used(), 42);
        assert_eq!(context.budget().io_used_bytes(), 1024);
    }

    #[test]
    fn candidate_selection_why_names_direct_source_fields() {
        let why = candidate_selection_why("prepare release", "lexical", 0.812_34, 0.456_78, None);

        assert!(why.contains(
            "Deterministic retrieval explanation for query `prepare release`: source=memory search_source=lexical"
        ));
        assert!(why.contains("relevance=unit_score(search_hit.score)"));
        assert!(why.contains("search_hit.score=0.8123"));
        assert!(why.contains("utility=unit_score(memory.utility)"));
        assert!(why.contains("memory.utility=0.4568"));
        assert!(
            why.contains(
                "unit_score(field)=clamp(field, 0.0, 1.0) for finite fields, otherwise 0.0"
            )
        );
    }

    #[test]
    fn candidate_selection_why_names_artifact_link_source_fields() {
        let why = candidate_selection_why(
            "prepare release",
            "hybrid",
            0.912_34,
            0.556_78,
            Some("art_0123456789abcdef01234567"),
        );

        assert!(why.contains(
            "source=registered_artifact artifact_id=art_0123456789abcdef01234567 search_source=hybrid"
        ));
        assert!(why.contains("relevance=unit_score(search_hit.score)"));
        assert!(why.contains("search_hit.score=0.9123"));
        assert!(why.contains("utility=unit_score(linked_memory.utility)"));
        assert!(why.contains("linked_memory.utility=0.5568"));
    }

    #[test]
    fn candidate_selection_why_declares_deterministic_inputs() {
        let why = candidate_selection_why("prepare release", "lexical", 0.812_34, 0.456_78, None);

        assert!(
            why.contains("inputs are stored memory/link fields and the explicit search hit"),
            "{why}"
        );
        assert!(why.contains("not agent reasoning"), "{why}");

        let lower = why.to_ascii_lowercase();
        for forbidden in [
            "believes",
            "understands",
            "intends",
            "inferred intent",
            "story",
        ] {
            assert!(
                !lower.contains(forbidden),
                "explanation used qualitative reasoning term `{forbidden}`: {why}"
            );
        }
    }

    #[test]
    fn focus_candidate_why_declares_passive_context_influence() -> Result<(), String> {
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(44));
        let mut state = FocusState::new(
            WorkspaceId::from_uuid(uuid::Uuid::from_u128(1)),
            3,
            "2026-05-04T00:00:00Z",
        )
        .map_err(|error| error.to_string())?
        .with_focal_memory_id(memory_id);
        let item = FocusItem::new(
            memory_id,
            "Resume the failing test context.",
            "2026-05-04T00:00:00Z",
        )
        .map_err(|error| error.to_string())?
        .pinned(true)
        .with_provenance("ee focus set");
        state = state
            .with_item(item.clone())
            .map_err(|error| error.to_string())?;

        let why = focus_candidate_why(&item, &state, "blake3:test");
        assert!(why.contains("focus_state_hash=blake3:test"), "{why}");
        assert!(why.contains("focal=true"), "{why}");
        assert!(why.contains("pinned=true"), "{why}");
        assert!(why.contains("source=ee_focus_state"), "{why}");
        assert!(why.contains("no hidden mutation"), "{why}");
        assert!(why.contains("agent-plan inference"), "{why}");

        let relevance = focus_relevance(&item, &state).map(|score| score.into_inner());
        assert_eq!(relevance, Some(1.0));
        Ok(())
    }

    #[test]
    fn unit_score_clamps_non_finite_and_bounds() {
        assert!(
            matches!(unit_score(-0.25), Some(score) if (score.into_inner() - 0.0).abs() <= f32::EPSILON)
        );
        assert!(
            matches!(unit_score(0.50), Some(score) if (score.into_inner() - 0.50).abs() <= f32::EPSILON)
        );
        assert!(
            matches!(unit_score(1.25), Some(score) if (score.into_inner() - 1.0).abs() <= f32::EPSILON)
        );
        assert!(
            matches!(unit_score(f32::NAN), Some(score) if (score.into_inner() - 0.0).abs() <= f32::EPSILON)
        );
        assert!(
            matches!(unit_score(f32::INFINITY), Some(score) if (score.into_inner() - 0.0).abs() <= f32::EPSILON)
        );
    }

    #[test]
    fn with_narrowed_capabilities_preserves_workspace_and_budget() {
        let mut context = ctx(CapabilitySet::full_local());
        context.budget_mut().record_tokens(7);
        let narrowed = context.with_narrowed_capabilities(CapabilitySet::read_only());

        // Capabilities narrowed.
        assert_eq!(narrowed.capabilities().db, AccessLevel::Read);
        assert_eq!(narrowed.capabilities().filesystem, AccessLevel::Read);
        // Workspace identity preserved.
        assert_eq!(narrowed.workspace_root(), context.workspace_root());
        // Budget state preserved (tokens recorded before narrow are
        // still recorded after narrow).
        assert_eq!(narrowed.budget().tokens_used(), 7);
    }

    #[test]
    fn with_narrowed_capabilities_composes() {
        let context = ctx(CapabilitySet::full_local());
        let mask_a = CapabilitySet::read_only();
        let mask_b = CapabilitySet {
            db: AccessLevel::None,
            ..CapabilitySet::read_only()
        };
        // narrow(narrow(c, mask_a), mask_b) == narrow(c, narrow(mask_a, mask_b))
        let chained = context
            .with_narrowed_capabilities(mask_a)
            .with_narrowed_capabilities(mask_b);
        let combined = context.with_narrowed_capabilities(mask_a.narrow(mask_b));
        assert_eq!(chained.capabilities(), combined.capabilities());
    }

    #[test]
    fn pack_hash_includes_content_provenance_and_degradation() -> Result<(), String> {
        use super::{ContextResponseDegradation, ContextResponseSeverity, compute_pack_hash};
        use crate::models::{ProvenanceUri, TrustClass};
        use crate::pack::{
            ContextRequest, PackDraft, PackDraftItem, PackGuaranteeStatus, PackOmission,
            PackOmissionReason, PackProvenance, PackSection, PackSelectionCertificate,
            PackSelectionObjective, PackTrustSignal, TokenBudget,
        };

        let request =
            ContextRequest::from_query("test query").map_err(|error| error.to_string())?;

        let mem_a = MemoryId::from_uuid(uuid::Uuid::from_u128(1));
        let mem_b = MemoryId::from_uuid(uuid::Uuid::from_u128(2));
        let mem_c = MemoryId::from_uuid(uuid::Uuid::from_u128(3));
        let mem_d = MemoryId::from_uuid(uuid::Uuid::from_u128(4));
        let budget = TokenBudget::default_context();

        let base_item = PackDraftItem {
            rank: 1,
            memory_id: mem_a,
            section: PackSection::ProceduralRules,
            content: "original content".to_string(),
            estimated_tokens: 10,
            relevance: crate::models::UnitScore::parse(0.8).map_err(|error| error.to_string())?,
            utility: crate::models::UnitScore::parse(0.7).map_err(|error| error.to_string())?,
            provenance: vec![
                PackProvenance::new(ProvenanceUri::EeMemory(mem_b), "source note")
                    .map_err(|error| error.to_string())?,
            ],
            why: "test explanation".to_string(),
            diversity_key: None,
            trust: PackTrustSignal::new(TrustClass::AgentAssertion, None),
        };

        let base_draft = PackDraft {
            query: "test query".to_string(),
            budget,
            used_tokens: 10,
            items: vec![base_item.clone()],
            omitted: vec![],
            selection_certificate: PackSelectionCertificate {
                certificate_id: None,
                profile: request.profile,
                objective: PackSelectionObjective::MmrRedundancy,
                algorithm: "test_deterministic_selection",
                guarantee: "test certificate only",
                guarantee_status: PackGuaranteeStatus::Conditional,
                candidate_count: 1,
                selected_count: 1,
                omitted_count: 0,
                budget_limit: budget.max_tokens(),
                budget_used: 10,
                total_objective_value: 1.0,
                monotone: false,
                submodular: false,
                selected_items: Vec::new(),
                rejected_frontier: Vec::new(),
                steps: Vec::new(),
            },
            hash: None,
        };

        let base_degraded: Vec<ContextResponseDegradation> = vec![];

        let hash_base = compute_pack_hash(&request, &base_draft, &base_degraded);

        // Different content produces different hash.
        let mut draft_content = base_draft.clone();
        draft_content.items[0].content = "different content".to_string();
        let hash_content = compute_pack_hash(&request, &draft_content, &base_degraded);
        assert_ne!(hash_base, hash_content, "content change must alter hash");

        // Different provenance produces different hash.
        let mut draft_provenance = base_draft.clone();
        draft_provenance.items[0].provenance = vec![
            PackProvenance::new(ProvenanceUri::EeMemory(mem_c), "different source")
                .map_err(|error| error.to_string())?,
        ];
        let hash_provenance = compute_pack_hash(&request, &draft_provenance, &base_degraded);
        assert_ne!(
            hash_base, hash_provenance,
            "provenance change must alter hash"
        );

        // Different why explanation produces different hash.
        let mut draft_why = base_draft.clone();
        draft_why.items[0].why = "different explanation".to_string();
        let hash_why = compute_pack_hash(&request, &draft_why, &base_degraded);
        assert_ne!(hash_base, hash_why, "why change must alter hash");

        // Different trust signal produces different hash.
        let mut draft_trust = base_draft.clone();
        draft_trust.items[0].trust =
            PackTrustSignal::new(TrustClass::AgentValidated, Some("verified".to_string()));
        let hash_trust = compute_pack_hash(&request, &draft_trust, &base_degraded);
        assert_ne!(hash_base, hash_trust, "trust change must alter hash");

        // Different omissions produce different hash.
        let mut draft_omission = base_draft.clone();
        draft_omission.omitted = vec![PackOmission {
            memory_id: mem_d,
            estimated_tokens: 50,
            reason: PackOmissionReason::TokenBudgetExceeded,
        }];
        let hash_omission = compute_pack_hash(&request, &draft_omission, &base_degraded);
        assert_ne!(hash_base, hash_omission, "omission change must alter hash");

        // Different degradations produce different hash.
        let degraded_with_issue = vec![ContextResponseDegradation {
            code: "test_degradation".to_string(),
            severity: ContextResponseSeverity::Medium,
            message: "Something degraded".to_string(),
            repair: Some("ee fix something".to_string()),
        }];
        let hash_degraded = compute_pack_hash(&request, &base_draft, &degraded_with_issue);
        assert_ne!(
            hash_base, hash_degraded,
            "degradation change must alter hash"
        );

        // Same inputs produce same hash (determinism check).
        let hash_repeat = compute_pack_hash(&request, &base_draft, &base_degraded);
        assert_eq!(hash_base, hash_repeat, "same inputs must produce same hash");
        Ok(())
    }

    #[test]
    fn persist_pack_record_preserves_item_provenance_and_trust() -> Result<(), String> {
        use std::path::Path;
        use std::str::FromStr;

        use super::{compute_pack_hash, persist_pack_record};
        use crate::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};
        use crate::models::{ProvenanceUri, TrustClass, UnitScore};
        use crate::pack::{
            ContextRequest, PackCandidate, PackCandidateInput, PackProvenance, PackSection,
            PackTrustSignal, TokenBudget, assemble_draft, pack_item_provenance_json,
        };

        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_01234567890123456789088888";
        let workspace_path = "/tmp/ee-context-persist-signals";
        connection
            .insert_workspace(
                workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.to_string(),
                    name: Some("context persist signals".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;

        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(88));
        connection
            .insert_memory(
                &memory_id.to_string(),
                &CreateMemoryInput {
                    workspace_id: workspace_id.to_string(),
                    level: "procedural".to_string(),
                    kind: "rule".to_string(),
                    content: "Run cargo fmt before release.".to_string(),
                    confidence: 0.9,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: Some("file://AGENTS.md#L42".to_string()),
                    trust_class: TrustClass::AgentValidated.as_str().to_string(),
                    trust_subclass: Some("reviewed".to_string()),
                    tags: vec!["release".to_string()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let provenance = vec![
            PackProvenance::new(
                ProvenanceUri::from_str("file://AGENTS.md#L42")
                    .map_err(|error| error.to_string())?,
                "project rule source",
            )
            .map_err(|error| error.to_string())?,
            PackProvenance::new(
                ProvenanceUri::from_str("cass-session://session-a#L20-22")
                    .map_err(|error| error.to_string())?,
                "session confirmation",
            )
            .map_err(|error| error.to_string())?,
        ];
        let candidate = PackCandidate::new(PackCandidateInput {
            memory_id,
            section: PackSection::ProceduralRules,
            content: "Run cargo fmt before release.".to_string(),
            estimated_tokens: 9,
            relevance: UnitScore::parse(0.95).map_err(|error| error.to_string())?,
            utility: UnitScore::parse(0.8).map_err(|error| error.to_string())?,
            provenance: provenance.clone(),
            why: "Selected because the task is release formatting.".to_string(),
        })
        .map_err(|error| error.to_string())?
        .with_trust_signal(PackTrustSignal::new(
            TrustClass::AgentValidated,
            Some("reviewed".to_string()),
        ));
        let request =
            ContextRequest::from_query("prepare release").map_err(|error| error.to_string())?;
        let mut draft = assemble_draft(
            "prepare release",
            TokenBudget::default_context(),
            [candidate],
        )
        .map_err(|error| error.to_string())?;
        draft.hash = Some(compute_pack_hash(&request, &draft, &[]));

        persist_pack_record(
            &connection,
            Path::new(workspace_path),
            &request,
            &draft,
            &[],
        )?;

        let history = connection
            .list_pack_records_for_memory(&memory_id.to_string(), 10)
            .map_err(|error| error.to_string())?;
        assert_eq!(history.len(), 1);
        let stored_item = &history[0].1;
        assert_eq!(
            stored_item.provenance_json,
            pack_item_provenance_json(&provenance)
        );
        assert_eq!(stored_item.trust_class, "agent_validated");
        assert_eq!(stored_item.trust_subclass.as_deref(), Some("reviewed"));

        connection.close().map_err(|error| error.to_string())?;
        Ok(())
    }
}
