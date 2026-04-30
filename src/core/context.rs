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
use crate::core::search::{SearchError, SearchOptions, SearchStatus, run_search};
use crate::db::{DbConnection, StoredMemory};
use crate::models::{MemoryId, ProvenanceUri, UnitScore};
use crate::pack::{
    ContextPackProfile, ContextRequest, ContextRequestInput, ContextResponse,
    ContextResponseDegradation, ContextResponseSeverity, PackCandidate, PackCandidateInput,
    PackProvenance, PackSection, TokenBudget, assemble_draft, estimate_tokens_default,
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
    pub profile: Option<ContextPackProfile>,
    pub max_tokens: Option<u32>,
    pub candidate_pool: Option<u32>,
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

    let search_report = run_search(&SearchOptions {
        workspace_path: options.workspace_path.clone(),
        database_path: Some(database_path),
        index_dir: options.index_dir.clone(),
        query: request.query.clone(),
        limit: request.candidate_pool,
    })
    .map_err(ContextPackError::Search)?;

    if search_report.status == SearchStatus::IndexError {
        return Err(ContextPackError::Search(SearchError::Index(
            search_report.errors.join("; "),
        )));
    }

    let mut degraded = Vec::new();
    if search_report.status == SearchStatus::NoResults {
        push_degradation(
            &mut degraded,
            "context_no_results",
            ContextResponseSeverity::Low,
            "Search completed but returned no candidate memories.",
            Some("ee remember --workspace . --level procedural --kind rule \"...\"".to_string()),
        );
    }

    let candidates = candidates_from_search(&connection, &search_report, &mut degraded);
    let budget = match options.max_tokens {
        Some(max_tokens) => TokenBudget::new(max_tokens)
            .map_err(|error| ContextPackError::Pack(error.to_string()))?,
        None => TokenBudget::default_context(),
    };
    let draft = assemble_draft(request.query.clone(), budget, candidates)
        .map_err(|error| ContextPackError::Pack(error.to_string()))?;

    ContextResponse::new(request, draft, degraded)
        .map_err(|error| ContextPackError::Pack(error.to_string()))
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

fn candidate_from_hit(
    connection: &DbConnection,
    hit: &crate::core::search::SearchHit,
    query: &str,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<PackCandidate> {
    let memory_id = match MemoryId::from_str(&hit.doc_id) {
        Ok(id) => id,
        Err(_) => return None,
    };
    let memory = match connection.get_memory(&hit.doc_id) {
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
    let why = format!(
        "Selected for query `{query}` from {} search result with score {:.4} and utility {:.4}.",
        hit.source.as_str(),
        hit.score,
        memory.utility
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

    Some(candidate.with_diversity_key(diversity_key_for_memory(&memory, &tags)))
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

    use super::{AccessLevel, CapabilitySet, CommandContext};
    use crate::config::WorkspaceLocation;
    use crate::core::budget::RequestBudget;

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
}
