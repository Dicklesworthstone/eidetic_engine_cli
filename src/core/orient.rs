//! Core helpers for agent orientation.
//!
//! Most `ee orient` assembly is currently performed in the CLI layer because
//! it composes several public commands. This module holds reusable read-only
//! data providers that should not be duplicated by the CLI renderer.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use asupersync::cx::NoCaps;
use asupersync::{CancelReason, Cx};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value as JsonValue, json};

use super::context::{
    ContextPackOptions, ContextPackOutputOptions, ContextPackOutputProfile,
    admit_recent_context_memories,
};
use super::decide::{DecideItem, DecideRevisitOptions, decide_revisit};
use super::search::{
    SearchDedupMode, SearchDegradation, SearchHit, SearchOptions, SearchSourceMode, SearchStatus,
    run_context_search_with_preloaded_memories,
};
use crate::db::{DbConnection, StoredMemory, StoredWorkspace};
#[cfg(test)]
use crate::models::GLOBAL_MEMORY_SCOPE_TAG;
use crate::models::{DomainError, MemoryId, MemoryScope, ProvenanceUri, RedactionLevel};
use crate::pack::{
    ContextPackProfile, PackProvenance, PackResourceProfile, RenderedPackProvenance,
    redact_pack_provenance_text,
};
use crate::runtime::determinism::Deterministic;
use crate::search::SpeedMode;

pub const ORIENT_DECISIONS_SCHEMA_V1: &str = "ee.orient.decisions.v1";
pub const ORIENT_FAST_CONTENT_SCHEMA_V1: &str = "ee.orient.fast_content.v1";
pub const ORIENT_FAST_CONTENT_LIMIT: usize = 5;
pub const ORIENT_FAST_CANDIDATE_POOL_LIMIT: u32 = 100;
pub const ORIENT_FAST_RECENT_UNAVAILABLE_CODE: &str = "orient_fast_recent_unavailable";
pub const ORIENT_FAST_RELEVANT_UNAVAILABLE_CODE: &str = "orient_fast_relevant_unavailable";

/// Maximum child-directory depth scanned by nearby-store discovery.
pub const NEARBY_STORE_CHILD_DEPTH: usize = 3;
/// Maximum nearby stores reported (ranked by document count).
pub const NEARBY_STORE_REPORT_LIMIT: usize = 5;
/// Stores with fewer live memory heads than this remain thin enough to warrant
/// bounded nearby-store discovery. Three live heads are enough to distinguish
/// an intentionally populated store from incidental orientation state while
/// keeping the recovery scan small and predictable.
pub const NEARBY_STORE_THIN_LIVE_MEMORY_THRESHOLD: u64 = 3;
/// Default wall-clock budget for one discovery scan.
pub const NEARBY_STORE_SCAN_BUDGET_MS: u64 = 200;
/// Blocking filesystem/database probes are soft-cancellable. Retain a hard
/// process-local permit until each worker actually exits so repeated timeouts
/// cannot accumulate an unbounded detached tail.
const MAX_CONCURRENT_NEARBY_STORE_SCAN_WORKERS: usize = 2;

static NEARBY_STORE_SCAN_WORKER_LIMITER: std::sync::OnceLock<
    std::sync::Arc<NearbyStoreScanWorkerLimiter>,
> = std::sync::OnceLock::new();

/// Directory names never descended into during nearby-store discovery.
const NEARBY_STORE_SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".venv",
    "venv",
    "node_modules",
    "target",
    "dist",
    "build",
    ".cache",
    ".rch-tmp",
    ".doctor",
];

/// Store directory names that mark a candidate workspace.
const NEARBY_STORE_MARKERS: &[&str] = &[".ee", ".ee-campaign"];

#[derive(Clone, Debug)]
pub struct OrientDecisionOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
    pub warning_days: Option<u64>,
    pub limit: usize,
    pub now: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrientDecisionReport {
    pub schema: &'static str,
    pub due_count: usize,
    pub returned_count: usize,
    pub warning_days: u64,
    pub window_end: String,
    pub decisions: Vec<DecideItem>,
}

impl OrientDecisionReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }
}

#[derive(Clone, Debug)]
pub struct OrientFastContentOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
    pub index_dir: Option<&'a Path>,
    pub task: &'a str,
    pub max_tokens: u32,
    pub candidate_pool: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrientFastContentStrategy {
    pub recent: &'static str,
    pub relevant: &'static str,
    pub section_overlap: &'static str,
    pub recent_limit: usize,
    pub relevant_limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrientFastContentIssue {
    pub component: &'static str,
    pub status: &'static str,
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrientFastContentItem {
    pub id: String,
    pub snippet: String,
    pub created_at: String,
    pub tags: Vec<String>,
    pub provenance: Vec<RenderedPackProvenance>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrientFastContentReport {
    pub schema: &'static str,
    pub posture: &'static str,
    pub strategy: OrientFastContentStrategy,
    pub recent: Vec<OrientFastContentItem>,
    pub relevant: Vec<OrientFastContentItem>,
    pub issues: Vec<OrientFastContentIssue>,
}

impl OrientFastContentReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }
}

/// Assemble bounded fast-mode content without persisting a pack or bypassing
/// context admission. Recent and relevant remain separate by contract, even
/// when the same memory is useful in both sections.
#[must_use]
pub fn orient_fast_content(options: &OrientFastContentOptions<'_>) -> OrientFastContentReport {
    let pack_options = orient_fast_pack_options(options);
    let mut issues = Vec::new();

    let recent = match admit_recent_context_memories(&pack_options, ORIENT_FAST_CONTENT_LIMIT) {
        Ok(items) => items
            .into_iter()
            .map(|admitted| OrientFastContentItem {
                id: admitted.item.memory_id.to_string(),
                snippet: orient_fast_snippet(&admitted.item.content),
                created_at: admitted.created_at,
                tags: orient_fast_public_tags(admitted.tags),
                provenance: admitted.item.rendered_provenance(),
            })
            .collect(),
        Err(error) => {
            issues.push(OrientFastContentIssue {
                component: "recent",
                status: "unavailable",
                code: ORIENT_FAST_RECENT_UNAVAILABLE_CODE.to_owned(),
                severity: "warning".to_owned(),
                message: format!("Recent context admission was unavailable: {error}"),
                repair: Some(
                    "Run `ee doctor --workspace . --json` and inspect the local memory store."
                        .to_owned(),
                ),
            });
            Vec::new()
        }
    };

    let relevant = match orient_fast_relevant_content(options) {
        Ok((items, provider_issues)) => {
            issues.extend(provider_issues);
            items
        }
        Err(issue) => {
            issues.push(issue);
            Vec::new()
        }
    };

    let posture = if issues.is_empty() {
        if recent.is_empty() && relevant.is_empty() {
            "empty"
        } else {
            "ready"
        }
    } else if recent.is_empty() && relevant.is_empty() {
        "unavailable"
    } else {
        "partial"
    };

    OrientFastContentReport {
        schema: ORIENT_FAST_CONTENT_SCHEMA_V1,
        posture,
        strategy: OrientFastContentStrategy {
            recent: "context_admitted_recency_v1",
            relevant: "direct_lexical_admitted_v1",
            section_overlap: "preserved",
            recent_limit: ORIENT_FAST_CONTENT_LIMIT,
            relevant_limit: ORIENT_FAST_CONTENT_LIMIT,
        },
        recent,
        relevant,
        issues,
    }
}

fn orient_fast_pack_options(options: &OrientFastContentOptions<'_>) -> ContextPackOptions {
    ContextPackOptions {
        workspace_path: options.workspace_path.to_path_buf(),
        database_path: options.database_path.map(Path::to_path_buf),
        index_dir: options.index_dir.map(Path::to_path_buf),
        query: options.task.to_owned(),
        speed: SpeedMode::Instant,
        source_mode: SearchSourceMode::LexicalOnly,
        strict_source_mode: false,
        filters: crate::models::QueryFilters::default(),
        profile: Some(ContextPackProfile::Orientation),
        max_tokens: Some(options.max_tokens),
        candidate_pool: Some(options.candidate_pool),
        max_results: Some(ORIENT_FAST_CONTENT_LIMIT as u32),
        include_tombstoned: false,
        as_of: None,
        include_expired: false,
        include_future: false,
        include_stale: false,
        relevance_floor: Some(0.0),
        redaction_level: RedactionLevel::Minimal,
        memory_scope: MemoryScope::Workspace,
        strict_scope: false,
        ppr_weight: Some(0.0),
        changed_symbols: Vec::new(),
        changed_symbols_from_git: false,
        pagination: None,
        coordination_snapshot_path: None,
        coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
        task_lens: None,
        require_fresh_sentinels: false,
        output_options: ContextPackOutputOptions::for_profile(ContextPackOutputProfile::Lean)
            .with_resource_profile(PackResourceProfile::Lean),
        persist_pack: false,
        baseline_write: None,
        no_lod: false,
    }
}

fn orient_fast_relevant_content(
    options: &OrientFastContentOptions<'_>,
) -> Result<(Vec<OrientFastContentItem>, Vec<OrientFastContentIssue>), OrientFastContentIssue> {
    let database_path = options
        .database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| options.workspace_path.join(".ee").join("ee.db"));
    let connection = DbConnection::open_file_read_only(&database_path).map_err(|error| {
        orient_fast_relevant_unavailable(format!(
            "Bounded lexical retrieval could not open the read-only memory store: {error}"
        ))
    })?;
    if connection.needs_migration().map_err(|error| {
        orient_fast_relevant_unavailable(format!(
            "Bounded lexical retrieval could not inspect migration state: {error}"
        ))
    })? {
        return Err(orient_fast_relevant_unavailable(
            "Bounded lexical retrieval requires a database migration.".to_owned(),
        ));
    }

    let candidate_limit = options
        .candidate_pool
        .max(ORIENT_FAST_CONTENT_LIMIT as u32)
        .min(ORIENT_FAST_CANDIDATE_POOL_LIMIT);
    let determinism = Deterministic::from_seed(0);
    let context_search = run_context_search_with_preloaded_memories(
        &SearchOptions {
            workspace_path: options.workspace_path.to_path_buf(),
            database_path: Some(database_path),
            index_dir: options.index_dir.map(Path::to_path_buf),
            query: options.task.to_owned(),
            limit: candidate_limit,
            speed: SpeedMode::Instant,
            explain: false,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: Some(0.0),
            dedup_mode: SearchDedupMode::DocId,
            source_mode: SearchSourceMode::LexicalOnly,
            strict_source_mode: true,
            memory_scope: MemoryScope::Workspace,
            strict_scope: false,
        },
        &connection,
        None,
        &determinism,
        None,
    )
    .map_err(|error| {
        orient_fast_relevant_unavailable(format!(
            "Bounded lexical retrieval was unavailable: {error}"
        ))
    })?;
    let mut issues = context_search
        .report
        .degraded
        .iter()
        .filter_map(orient_fast_relevant_provider_issue)
        .collect::<Vec<_>>();
    if context_search.report.source_mode_applied != SearchSourceMode::LexicalOnly
        || context_search.report.source_mode_fallback
    {
        return Err(orient_fast_relevant_unavailable(format!(
            "Bounded lexical retrieval applied source mode `{}` instead of lexical_only.",
            context_search.report.source_mode_applied.as_str()
        )));
    }
    if matches!(
        context_search.report.status,
        SearchStatus::IndexNotFound | SearchStatus::IndexError
    ) {
        return Err(orient_fast_relevant_unavailable(format!(
            "Bounded lexical retrieval ended with status `{}`.",
            context_search.report.status.as_str()
        )));
    }

    let mut rendered = Vec::with_capacity(ORIENT_FAST_CONTENT_LIMIT);
    let mut freshness_file_cache = crate::core::memory::EvidenceFreshnessFileCache::default();
    for hit in &context_search.report.results {
        if rendered.len() >= ORIENT_FAST_CONTENT_LIMIT {
            break;
        }
        let Some(memory) = context_search.preloaded_memories.get(&hit.doc_id) else {
            issues.push(OrientFastContentIssue {
                component: "relevant",
                status: "metadata_unavailable",
                code: ORIENT_FAST_RELEVANT_UNAVAILABLE_CODE.to_owned(),
                severity: "warning".to_owned(),
                message: format!(
                    "Bounded lexical retrieval omitted admitted memory metadata for {}.",
                    hit.doc_id
                ),
                repair: Some("Run `ee index rebuild --workspace .`.".to_owned()),
            });
            continue;
        };
        // Fast orientation is fail-closed for secret-bearing and sealed
        // bodies. Search has already admitted scope, lifecycle, and source
        // mode; this final screen prevents identifiers or placeholders from
        // becoming a side channel in the compact response.
        if memory.content == crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT
            || !crate::policy::redact_secret_like_content(&memory.content)
                .redacted_reasons
                .is_empty()
        {
            continue;
        }
        let Some(provenance) = orient_fast_provenance(
            memory,
            options.workspace_path,
            &mut freshness_file_cache,
            &mut issues,
        ) else {
            continue;
        };
        rendered.push(OrientFastContentItem {
            id: memory.id.clone(),
            snippet: orient_fast_snippet(&memory.content),
            created_at: memory.created_at.clone(),
            tags: orient_fast_public_tags(orient_fast_search_hit_tags(hit)),
            provenance: vec![provenance],
        });
    }
    Ok((rendered, issues))
}

fn orient_fast_relevant_unavailable(message: String) -> OrientFastContentIssue {
    OrientFastContentIssue {
        component: "relevant",
        status: "unavailable",
        code: ORIENT_FAST_RELEVANT_UNAVAILABLE_CODE.to_owned(),
        severity: "warning".to_owned(),
        message,
        repair: Some(
            "Run `ee index status --workspace . --json`; rebuild the lexical index if needed."
                .to_owned(),
        ),
    }
}

/// Keep only degradations that say the bounded lexical provider or its live
/// admission checks were incomplete. Ranking advisories and expected
/// visibility filters do not describe a provider failure and would make fast
/// orientation noisy without changing whether its content is usable.
fn orient_fast_relevant_provider_issue(
    degradation: &SearchDegradation,
) -> Option<OrientFastContentIssue> {
    matches!(
        degradation.code.as_str(),
        "search_index_stale"
            | "search_index_large_gap"
            | "search_index_degraded"
            | "index_missing"
            | "index_corrupt"
            | "search_unavailable"
            | "lexical_unavailable"
            | "profile_search_limit_capped"
            | "scope_metadata_unavailable"
            | "tombstone_visibility_unavailable"
            | "evidence_live_admission_filtered"
            | "malformed_validity_filtered"
            | "validity_filtered_significant_recall_drop"
    )
    .then(|| OrientFastContentIssue {
        component: "relevant",
        status: "degraded",
        code: degradation.code.clone(),
        severity: degradation.severity.clone(),
        message: degradation.message.clone(),
        repair: degradation.repair.clone(),
    })
}

fn orient_fast_search_hit_tags(hit: &SearchHit) -> Vec<String> {
    let Some(value) = hit
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("tags"))
    else {
        return Vec::new();
    };
    if let Some(tags) = value.as_array() {
        return tags
            .iter()
            .filter_map(JsonValue::as_str)
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(str::to_owned)
            .collect();
    }
    value.as_str().map_or_else(Vec::new, |tags| {
        tags.split(',')
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(str::to_owned)
            .collect()
    })
}

fn orient_fast_provenance(
    memory: &StoredMemory,
    workspace_path: &Path,
    freshness_file_cache: &mut crate::core::memory::EvidenceFreshnessFileCache,
    issues: &mut Vec<OrientFastContentIssue>,
) -> Option<RenderedPackProvenance> {
    let memory_id = MemoryId::from_str(&memory.id).ok()?;
    let uri = match memory.provenance_uri.as_deref() {
        Some(raw) => match ProvenanceUri::from_str(raw) {
            Ok(uri) => uri,
            Err(error) => {
                issues.push(OrientFastContentIssue {
                    component: "relevant",
                    status: "metadata_unavailable",
                    code: "context_invalid_provenance".to_owned(),
                    severity: "low".to_owned(),
                    message: format!("Memory {} has invalid provenance URI: {error}", memory.id),
                    repair: Some(format!("ee memory show {} --json", memory.id)),
                });
                ProvenanceUri::EeMemory(memory_id)
            }
        },
        None => ProvenanceUri::EeMemory(memory_id),
    };
    let freshness = crate::core::memory::assess_memory_evidence_freshness_with_cache(
        memory,
        Some(workspace_path),
        freshness_file_cache,
    );
    if freshness.status.should_report() {
        let code = match freshness.status {
            crate::core::memory::EvidenceFreshnessStatus::MissingSource => {
                "context_evidence_freshness_missing_source"
            }
            crate::core::memory::EvidenceFreshnessStatus::ChangedSource => {
                "context_evidence_freshness_changed_source"
            }
            crate::core::memory::EvidenceFreshnessStatus::UnreachableSource => {
                "context_evidence_freshness_unreachable_source"
            }
            crate::core::memory::EvidenceFreshnessStatus::UnsupportedSource => {
                "context_evidence_freshness_unsupported_source"
            }
            crate::core::memory::EvidenceFreshnessStatus::Fresh => {
                "context_evidence_freshness_fresh"
            }
            crate::core::memory::EvidenceFreshnessStatus::Unknown => {
                "context_evidence_freshness_unknown"
            }
        };
        issues.push(OrientFastContentIssue {
            component: "relevant",
            status: "degraded",
            code: code.to_owned(),
            severity: "low".to_owned(),
            message: format!(
                "Memory {} evidence freshness is {}: {}",
                memory.id,
                freshness.status.as_str(),
                redact_pack_provenance_text(&freshness.detail)
            ),
            repair: freshness.repair.as_deref().map(redact_pack_provenance_text),
        });
    }
    let active_workspace_id = crate::core::curate::stable_workspace_id(workspace_path);
    let note = if memory.workspace_id == active_workspace_id {
        format!(
            "Memory {} selected by bounded direct lexical orientation retrieval; evidenceFreshness={}.",
            memory.id,
            freshness.status.as_str()
        )
    } else {
        format!(
            "Memory {} selected by bounded direct lexical cross_shard_read; origin_workspace_id={}; orientation_workspace_id={}; evidenceFreshness={}.",
            memory.id,
            memory.workspace_id,
            active_workspace_id,
            freshness.status.as_str()
        )
    };
    PackProvenance::new(uri, note)
        .ok()
        .map(|provenance| provenance.rendered())
}

/// Compact fast-mode snippet: at most two trimmed, non-empty lines joined by
/// a single newline, capped at 480 characters, with an ellipsis whenever any
/// content was dropped. Fast items promise a 1-2 line shape in both JSON and
/// human renders (bd-orient-fast-content-iubub).
fn orient_fast_snippet(content: &str) -> String {
    const MAX_CHARS: usize = 480;
    const MAX_LINES: usize = 2;
    let mut lines = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let mut kept = Vec::with_capacity(MAX_LINES);
    for line in lines.by_ref() {
        kept.push(line);
        if kept.len() == MAX_LINES {
            break;
        }
    }
    let dropped_lines = lines.next().is_some();
    let joined = kept.join("\n");
    let joined_chars = joined.chars().count();
    let truncated = joined_chars > MAX_CHARS || dropped_lines;
    let kept_chars = if truncated {
        MAX_CHARS.saturating_sub(1)
    } else {
        MAX_CHARS
    };
    let mut snippet = joined.chars().take(kept_chars).collect::<String>();
    if truncated {
        snippet.push('…');
    }
    snippet
}

/// Route fast-content tags through the shared public-egress text policy so a
/// secret-shaped or otherwise disallowed tag never leaves the process raw.
/// The policy's replacement text is self-describing, so the item shape and
/// schema stay unchanged.
fn orient_fast_public_tags(tags: Vec<String>) -> Vec<String> {
    tags.into_iter()
        .map(|tag| crate::policy::redact_public_replay_text(&tag).content)
        .collect()
}

pub fn orient_decisions(
    options: &OrientDecisionOptions<'_>,
) -> Result<OrientDecisionReport, DomainError> {
    let revisit = decide_revisit(&DecideRevisitOptions {
        workspace_path: options.workspace_path,
        database_path: options.database_path,
        warning_days: options.warning_days,
        limit: options.limit,
        now: options.now,
    })?;
    Ok(OrientDecisionReport {
        schema: ORIENT_DECISIONS_SCHEMA_V1,
        due_count: revisit.due_count,
        returned_count: revisit.returned_count,
        warning_days: revisit.warning_days,
        window_end: revisit.window_end,
        decisions: revisit.decisions,
    })
}

// ============ nearby-store discovery (bd-orient-store-discovery-ft1z5) ============
//
// When the addressed workspace has an empty (or missing) store, agents are
// usually one directory away from the real one — and an empty-store answer
// with no pointer reads as "ee has nothing for you", silently destroying
// ee's value for the session. Discovery scans child directories (bounded
// depth, skip-listed, time-capped) and parents up to the git root for
// populated stores, and consults the machine workspace registry for stores
// outside that local walk, so the orientation output can point at them.

/// One discovered populated store near the addressed workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NearbyStore {
    /// Workspace root that owns the store (the directory containing `.ee`
    /// or `.ee-campaign`).
    pub workspace_root: String,
    /// The store directory itself.
    pub store_dir: String,
    /// Current, non-tombstoned memory heads for this exact workspace. Other
    /// workspace rows in the same database never contribute to this count.
    pub documents: u64,
    /// Newest regular database or WAL file mtime (RFC 3339): the last actual
    /// durable write. The shared-memory sidecar is intentionally excluded.
    pub last_write: Option<String>,
}

/// Bounded discovery result.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct NearbyStoreScan {
    /// Ranked by document count (desc), then path; capped at
    /// [`NEARBY_STORE_REPORT_LIMIT`].
    pub stores: Vec<NearbyStore>,
    /// True when the scan hit its wall-clock budget before covering every
    /// candidate directory.
    pub truncated: bool,
}

#[derive(Debug)]
struct NearbyStoreScanWorkerLimiter {
    active: std::sync::atomic::AtomicUsize,
    cap: usize,
}

impl NearbyStoreScanWorkerLimiter {
    fn new(cap: usize) -> Self {
        Self {
            active: std::sync::atomic::AtomicUsize::new(0),
            cap: cap.max(1),
        }
    }

    fn try_acquire(limiter: &std::sync::Arc<Self>) -> Option<NearbyStoreScanWorkerPermit> {
        use std::sync::atomic::Ordering;

        let mut current = limiter.active.load(Ordering::Acquire);
        loop {
            if current >= limiter.cap {
                return None;
            }
            match limiter.active.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(NearbyStoreScanWorkerPermit {
                        limiter: std::sync::Arc::clone(limiter),
                    });
                }
                Err(actual) => current = actual,
            }
        }
    }

    #[cfg(test)]
    fn active_count(&self) -> usize {
        self.active.load(std::sync::atomic::Ordering::Acquire)
    }
}

#[derive(Debug)]
struct NearbyStoreScanWorkerPermit {
    limiter: std::sync::Arc<NearbyStoreScanWorkerLimiter>,
}

impl Drop for NearbyStoreScanWorkerPermit {
    fn drop(&mut self) {
        let previous = self
            .limiter
            .active
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        debug_assert!(previous > 0, "nearby-store scan worker permit underflow");
    }
}

fn nearby_store_scan_worker_limiter() -> std::sync::Arc<NearbyStoreScanWorkerLimiter> {
    std::sync::Arc::clone(NEARBY_STORE_SCAN_WORKER_LIMITER.get_or_init(|| {
        std::sync::Arc::new(NearbyStoreScanWorkerLimiter::new(
            MAX_CONCURRENT_NEARBY_STORE_SCAN_WORKERS,
        ))
    }))
}

#[derive(Clone, Debug)]
struct NearbyStoreRegistryIdentity {
    workspace_id: String,
    repository_fingerprint: Option<String>,
}

#[derive(Clone, Debug)]
struct NearbyStoreCandidate {
    workspace_root: PathBuf,
    registry_identity: Option<NearbyStoreRegistryIdentity>,
}

enum NearbyStoreScanUpdate {
    Progress(NearbyStoreScan),
    Finished(NearbyStoreScan),
}

/// Source-of-truth population state for one resolved addressed database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddressedStoreState {
    /// The resolved store is missing or has zero memory rows. The path
    /// identifies the exact empty store agents addressed.
    Empty { database: PathBuf },
    /// The resolved store has some durable memory, but not enough to suppress
    /// nearby-store discovery. The exact count is retained for truthful output.
    Thin {
        database: PathBuf,
        live_memories: u64,
    },
    /// The resolved store has enough durable memory to suppress discovery.
    Populated { live_memories: u64 },
    /// A present store could not be inspected safely. Discovery must not call
    /// it empty because an unreadable store is not evidence of zero rows.
    Unavailable,
}

/// Resolve the database addressed by an orientation request.
///
/// The normal `.ee/ee.db` store remains authoritative whenever that path has
/// any filesystem entry, including an unsafe or unreadable one. Falling back
/// in that case would hide a broken addressed store. When the normal database
/// is genuinely absent, an existing `.ee-campaign/ee.db` is the supported
/// campaign-store fallback.
#[must_use]
pub fn resolved_addressed_database_path(workspace_path: &Path) -> PathBuf {
    // Discovery candidates are canonicalized before they are reported. Use
    // the same canonical workspace root for the addressed store so provider
    // reads, discovery suppression, and retarget commands cannot describe
    // different spellings of the same on-disk store (notably `/var` versus
    // `/private/var` on macOS). Marker/database symlinks remain visible in
    // the resulting path and are rejected by `addressed_store_state`.
    let workspace_root = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let default_database = workspace_root.join(".ee").join("ee.db");
    let campaign_database = workspace_root.join(".ee-campaign").join("ee.db");

    match std::fs::symlink_metadata(&default_database) {
        Ok(_) => default_database,
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => default_database,
        Err(_) => match std::fs::symlink_metadata(&campaign_database) {
            Ok(_) => campaign_database,
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => campaign_database,
            Err(_) => default_database,
        },
    }
}

/// Inspect exactly the database and workspace identity addressed by the request.
///
/// `None` from the row-count probe is deliberately unavailable, never empty:
/// unreadable, symlinked, broken, or schema-incompatible stores cannot justify
/// scanning and retargeting away from the caller's selected store.
#[must_use]
pub fn addressed_store_state(workspace_path: &Path, database: &Path) -> AddressedStoreState {
    match std::fs::symlink_metadata(database) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => AddressedStoreState::Empty {
            database: database.to_path_buf(),
        },
        Err(_) => AddressedStoreState::Unavailable,
        Ok(_) if !nearby_store_database_is_safe_regular_file(database) => {
            AddressedStoreState::Unavailable
        }
        Ok(_) => match store_memory_row_count(workspace_path, database) {
            Some(0) => AddressedStoreState::Empty {
                database: database.to_path_buf(),
            },
            Some(live_memories) if live_memories < NEARBY_STORE_THIN_LIVE_MEMORY_THRESHOLD => {
                AddressedStoreState::Thin {
                    database: database.to_path_buf(),
                    live_memories,
                }
            }
            Some(live_memories) => AddressedStoreState::Populated { live_memories },
            None => AddressedStoreState::Unavailable,
        },
    }
}

/// Scan for populated stores near `workspace_path`.
///
/// Read-only and loud about nothing: unreadable directories and stores that
/// fail to open are skipped (permission-denied children must not fail
/// orientation). Only the exact addressed database is excluded; an alternate
/// `.ee`/`.ee-campaign` store at the same workspace root remains discoverable.
#[must_use]
pub fn discover_nearby_stores(
    workspace_path: &Path,
    budget: std::time::Duration,
) -> NearbyStoreScan {
    // Generic callers still address the conventional `.ee` store. Orient is
    // the one surface that resolves `.ee-campaign` as a provider fallback and
    // therefore calls `discover_nearby_stores_for_database` explicitly.
    let workspace_root = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let addressed_database = workspace_root.join(".ee").join("ee.db");
    discover_nearby_stores_with_registry(workspace_path, &addressed_database, budget, None)
}

/// Scan while preserving the exact database already resolved by the caller.
///
/// `ee orient` uses this form so the provider database, the discovery
/// suppression decision, and the excluded candidate are one identity.
#[must_use]
pub fn discover_nearby_stores_for_database(
    workspace_path: &Path,
    addressed_database: &Path,
    budget: std::time::Duration,
) -> NearbyStoreScan {
    discover_nearby_stores_with_registry(workspace_path, addressed_database, budget, None)
}

fn discover_nearby_stores_with_registry(
    workspace_path: &Path,
    addressed_database: &Path,
    budget: std::time::Duration,
    registry_path: Option<&Path>,
) -> NearbyStoreScan {
    let workspace_path = workspace_path.to_path_buf();
    let addressed_database = addressed_database.to_path_buf();
    let registry_path = registry_path.map(Path::to_path_buf);
    run_nearby_store_scan_bounded(budget, move |cx, publish| {
        scan_nearby_stores_with_registry(
            cx,
            &workspace_path,
            &addressed_database,
            budget,
            registry_path.as_deref(),
            publish,
        )
    })
}

/// Keep the caller's discovery latency bounded even when one filesystem or
/// read-only database operation blocks past the cooperative deadline checks.
/// Progress snapshots preserve candidates already proved populated; timeout
/// requests cooperative cancellation and returns the latest partial result.
fn run_nearby_store_scan_bounded<F>(budget: std::time::Duration, scan: F) -> NearbyStoreScan
where
    F: FnOnce(&Cx<NoCaps>, &mut dyn FnMut(&NearbyStoreScan)) -> NearbyStoreScan + Send + 'static,
{
    run_nearby_store_scan_bounded_with_limiter(budget, nearby_store_scan_worker_limiter(), scan)
}

fn run_nearby_store_scan_bounded_with_limiter<F>(
    budget: std::time::Duration,
    worker_limiter: std::sync::Arc<NearbyStoreScanWorkerLimiter>,
    scan: F,
) -> NearbyStoreScan
where
    F: FnOnce(&Cx<NoCaps>, &mut dyn FnMut(&NearbyStoreScan)) -> NearbyStoreScan + Send + 'static,
{
    if budget.is_zero() {
        return NearbyStoreScan {
            stores: Vec::new(),
            truncated: true,
        };
    }

    let Some(worker_permit) = NearbyStoreScanWorkerLimiter::try_acquire(&worker_limiter) else {
        return NearbyStoreScan {
            stores: Vec::new(),
            truncated: true,
        };
    };

    let started = std::time::Instant::now();
    let cancel_cx = Cx::detached_cancel_context();
    let worker_cx = cancel_cx.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("ee-nearby-store-scan".to_owned())
        .spawn(move || {
            let _worker_permit = worker_permit;
            let _ambient_cx = worker_cx.clone().set_current_restricted();
            let progress_sender = sender.clone();
            let mut publish = move |scan: &NearbyStoreScan| {
                let _ = progress_sender.send(NearbyStoreScanUpdate::Progress(scan.clone()));
            };
            let result = scan(&worker_cx, &mut publish);
            let _ = sender.send(NearbyStoreScanUpdate::Finished(result));
        });
    if spawned.is_err() {
        return NearbyStoreScan {
            stores: Vec::new(),
            truncated: true,
        };
    }

    let mut latest = NearbyStoreScan::default();
    loop {
        let remaining = budget.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            cancel_cx.set_cancel_reason(
                CancelReason::timeout().with_message("nearby-store discovery timed out"),
            );
            latest.truncated = true;
            return latest;
        }
        match receiver.recv_timeout(remaining) {
            Ok(NearbyStoreScanUpdate::Progress(progress)) => latest = progress,
            Ok(NearbyStoreScanUpdate::Finished(mut result)) => {
                if started.elapsed() >= budget {
                    result.truncated = true;
                }
                return result;
            }
            Err(
                std::sync::mpsc::RecvTimeoutError::Timeout
                | std::sync::mpsc::RecvTimeoutError::Disconnected,
            ) => {
                cancel_cx.set_cancel_reason(
                    CancelReason::timeout()
                        .with_message("nearby-store discovery worker unavailable"),
                );
                latest.truncated = true;
                return latest;
            }
        }
    }
}

fn scan_nearby_stores_with_registry(
    cx: &Cx<NoCaps>,
    workspace_path: &Path,
    addressed_database: &Path,
    budget: std::time::Duration,
    registry_path: Option<&Path>,
    publish: &mut dyn FnMut(&NearbyStoreScan),
) -> NearbyStoreScan {
    let started = std::time::Instant::now();
    let mut scan = NearbyStoreScan::default();
    let scan_root = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let addressed_database_canonical = addressed_database.canonicalize().ok();
    let mut seen_candidates = BTreeSet::new();
    let mut seen_databases = BTreeSet::new();

    if let Some(candidate) =
        add_nearby_store_candidate(scan_root.clone(), None, &mut seen_candidates)
    {
        inspect_nearby_store_candidate(
            cx,
            &candidate,
            addressed_database,
            addressed_database_canonical.as_deref(),
            started,
            budget,
            &mut seen_databases,
            &mut scan,
            publish,
        );
    }

    // (a) local children, breadth-first and bounded by depth. Local candidates
    // are inspected before the machine registry so a large global registry
    // cannot consume the entire budget ahead of an adjacent workspace.
    let mut frontier = VecDeque::from([(scan_root.clone(), 0_usize)]);
    'frontier: while let Some((dir, depth)) = frontier.pop_front() {
        if nearby_store_scan_should_stop(cx, started, budget, &mut scan) {
            break;
        }
        if depth > 0 {
            if let Some(candidate) =
                add_nearby_store_candidate(dir.clone(), None, &mut seen_candidates)
            {
                inspect_nearby_store_candidate(
                    cx,
                    &candidate,
                    addressed_database,
                    addressed_database_canonical.as_deref(),
                    started,
                    budget,
                    &mut seen_databases,
                    &mut scan,
                    publish,
                );
            }
        }
        if depth >= NEARBY_STORE_CHILD_DEPTH {
            continue;
        }
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        if nearby_store_scan_should_stop(cx, started, budget, &mut scan) {
            break;
        }
        let mut entries = Vec::new();
        for entry in read_dir.flatten() {
            // A single very wide directory must not bypass the advertised
            // wall-clock bound while we enumerate it. Checking only between
            // directories lets one directory with millions of entries turn
            // this read-only recovery hint into an unbounded walk.
            if nearby_store_scan_should_stop(cx, started, budget, &mut scan) {
                break 'frontier;
            }
            entries.push(entry);
        }
        // `read_dir` order is filesystem-dependent. Sort each breadth level's
        // children before enqueueing so both discovery publication and the
        // bounded prefix selected under a time limit are deterministic.
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            if nearby_store_scan_should_stop(cx, started, budget, &mut scan) {
                break 'frontier;
            }
            let path = entry.path();
            // `Path::is_dir` follows symlinks. Discovery is intentionally
            // confined to the addressed workspace tree, so inspect the
            // directory entry itself and never traverse a symlinked tree.
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if nearby_store_scan_should_stop(cx, started, budget, &mut scan) {
                break 'frontier;
            }
            if !file_type.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if NEARBY_STORE_SKIP_DIRS.contains(&name) || NEARBY_STORE_MARKERS.contains(&name) {
                continue;
            }
            frontier.push_back((path, depth + 1));
        }
    }

    // (b) local parents up to (and including) the git root.
    let mut parent = if scan_root.join(".git").exists() {
        None
    } else {
        scan_root.parent()
    };
    while let Some(dir) = parent {
        if nearby_store_scan_should_stop(cx, started, budget, &mut scan) {
            break;
        }
        if let Some(candidate) =
            add_nearby_store_candidate(dir.to_path_buf(), None, &mut seen_candidates)
        {
            inspect_nearby_store_candidate(
                cx,
                &candidate,
                addressed_database,
                addressed_database_canonical.as_deref(),
                started,
                budget,
                &mut seen_databases,
                &mut scan,
                publish,
            );
        }
        if dir.join(".git").exists() {
            break;
        }
        parent = dir.parent();
    }

    // (c) registered workspaces. Registry failures are deliberately quiet:
    // discovery is a best-effort recovery hint, and inability to inspect one
    // candidate must never be reported as evidence that its store is empty.
    if !nearby_store_scan_should_stop(cx, started, budget, &mut scan) {
        if let Ok(registry) = crate::core::workspace::list_workspace_registry(
            &crate::core::workspace::WorkspaceListOptions {
                registry_path: registry_path.map(Path::to_path_buf),
            },
        ) {
            if nearby_store_scan_should_stop(cx, started, budget, &mut scan) {
                return scan;
            }
            for workspace in registry.workspaces {
                if nearby_store_scan_should_stop(cx, started, budget, &mut scan) {
                    break;
                }
                let identity = NearbyStoreRegistryIdentity {
                    workspace_id: workspace.workspace_id,
                    repository_fingerprint: workspace.repository_fingerprint,
                };
                if let Some(candidate) = add_nearby_store_candidate(
                    PathBuf::from(workspace.path),
                    Some(identity),
                    &mut seen_candidates,
                ) {
                    inspect_nearby_store_candidate(
                        cx,
                        &candidate,
                        addressed_database,
                        addressed_database_canonical.as_deref(),
                        started,
                        budget,
                        &mut seen_databases,
                        &mut scan,
                        publish,
                    );
                }
            }
        }
    }

    // Account for the final completed operation too. Without this terminal
    // check, the last candidate comparison or result sort could cross the
    // deadline after the final in-loop check and still report `truncated`
    // as false.
    nearby_store_scan_should_stop(cx, started, budget, &mut scan);
    scan
}

fn nearby_store_scan_should_stop(
    cx: &Cx<NoCaps>,
    started: std::time::Instant,
    budget: std::time::Duration,
    scan: &mut NearbyStoreScan,
) -> bool {
    if cx.checkpoint().is_ok() && started.elapsed() < budget {
        false
    } else {
        scan.truncated = true;
        true
    }
}

fn add_nearby_store_candidate(
    candidate: PathBuf,
    registry_identity: Option<NearbyStoreRegistryIdentity>,
    seen: &mut BTreeSet<PathBuf>,
) -> Option<NearbyStoreCandidate> {
    if crate::core::path_safety::path_has_symlink_component(&candidate).unwrap_or(true) {
        return None;
    }
    let Ok(canonical) = candidate.canonicalize() else {
        return None;
    };
    // Registry metadata is identity evidence for that probe, not permission
    // to suppress a later local-path probe. A stale/mismatched registry row
    // must not hide a valid store found by the bounded filesystem walk.
    if registry_identity.is_none() && !seen.insert(canonical.clone()) {
        return None;
    }
    Some(NearbyStoreCandidate {
        workspace_root: canonical,
        registry_identity,
    })
}

#[allow(clippy::too_many_arguments)]
fn inspect_nearby_store_candidate(
    cx: &Cx<NoCaps>,
    candidate: &NearbyStoreCandidate,
    addressed_database: &Path,
    addressed_database_canonical: Option<&Path>,
    started: std::time::Instant,
    budget: std::time::Duration,
    seen_databases: &mut BTreeSet<PathBuf>,
    scan: &mut NearbyStoreScan,
    publish: &mut dyn FnMut(&NearbyStoreScan),
) {
    for marker in NEARBY_STORE_MARKERS {
        if nearby_store_scan_should_stop(cx, started, budget, scan) {
            return;
        }
        let store_dir = candidate.workspace_root.join(marker);
        let database = store_dir.join("ee.db");
        if nearby_store_is_addressed_database(
            &database,
            addressed_database,
            addressed_database_canonical,
        ) || !nearby_store_database_is_safe_regular_file(&database)
        {
            continue;
        }
        if nearby_store_scan_should_stop(cx, started, budget, scan) {
            return;
        }
        let Some((documents, last_write)) = nearby_store_profile(&database, candidate) else {
            continue;
        };
        if documents == 0 || nearby_store_scan_should_stop(cx, started, budget, scan) {
            continue;
        }
        let database_identity = database.canonicalize().unwrap_or_else(|_| database.clone());
        if !seen_databases.insert(database_identity) {
            continue;
        }
        scan.stores.push(NearbyStore {
            workspace_root: candidate.workspace_root.display().to_string(),
            store_dir: store_dir.display().to_string(),
            documents,
            last_write,
        });
        rank_nearby_stores(scan);
        publish(scan);
    }
}

fn rank_nearby_stores(scan: &mut NearbyStoreScan) {
    scan.stores.sort_by(|left, right| {
        right
            .documents
            .cmp(&left.documents)
            .then_with(|| left.workspace_root.cmp(&right.workspace_root))
            .then_with(|| left.store_dir.cmp(&right.store_dir))
    });
    scan.stores.truncate(NEARBY_STORE_REPORT_LIMIT);
}

fn nearby_store_is_addressed_database(
    candidate: &Path,
    addressed: &Path,
    addressed_canonical: Option<&Path>,
) -> bool {
    candidate == addressed
        || addressed_canonical.is_some_and(|addressed_canonical| {
            candidate
                .canonicalize()
                .is_ok_and(|candidate_canonical| candidate_canonical == addressed_canonical)
        })
}

fn nearby_store_database_is_safe_regular_file(database: &Path) -> bool {
    if crate::core::path_safety::path_has_symlink_component(database).unwrap_or(true) {
        return false;
    }
    std::fs::symlink_metadata(database)
        .map(|metadata| metadata.file_type().is_file())
        .unwrap_or(false)
}

/// Read the source-of-truth live-memory count for the workspace addressed by
/// this store without opening it for writes. `None` means the exact workspace
/// count could not be established and must never be interpreted as empty.
#[must_use]
pub(crate) fn store_memory_row_count(workspace_path: &Path, database: &Path) -> Option<u64> {
    let connection = DbConnection::open_file_read_only(database).ok()?;
    let workspace_root = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let workspace = connection
        .list_workspaces()
        .ok()?
        .into_iter()
        .find(|workspace| nearby_store_workspace_path_matches(workspace, &workspace_root))?;
    connection
        .count_live_memories_for_workspace(&workspace.id)
        .ok()
}

/// Read `(live workspace memory heads, newest db/WAL mtime)` from a candidate
/// store, skipping quietly on any identity or storage failure.
fn nearby_store_profile(
    database: &Path,
    candidate: &NearbyStoreCandidate,
) -> Option<(u64, Option<String>)> {
    let connection = DbConnection::open_file_read_only(database).ok()?;
    let workspace = nearby_store_workspace_identity(&connection, candidate)?;
    let documents = connection
        .count_live_memories_for_workspace(&workspace.id)
        .ok()?;
    let last_write = nearby_store_last_write(database);
    Some((documents, last_write))
}

fn nearby_store_workspace_identity(
    connection: &DbConnection,
    candidate: &NearbyStoreCandidate,
) -> Option<StoredWorkspace> {
    if let Some(expected) = &candidate.registry_identity {
        let workspace = connection.get_workspace(&expected.workspace_id).ok()??;
        if !nearby_store_workspace_path_matches(&workspace, &candidate.workspace_root) {
            return None;
        }
        if let (Some(expected_fingerprint), Some(actual_fingerprint)) = (
            expected.repository_fingerprint.as_deref(),
            workspace.repository_fingerprint.as_deref(),
        ) && expected_fingerprint != actual_fingerprint
        {
            return None;
        }
        return Some(workspace);
    }

    connection
        .list_workspaces()
        .ok()?
        .into_iter()
        .find(|workspace| nearby_store_workspace_path_matches(workspace, &candidate.workspace_root))
}

fn nearby_store_workspace_path_matches(workspace: &StoredWorkspace, candidate_root: &Path) -> bool {
    let stored_path = Path::new(&workspace.path);
    stored_path == candidate_root
        || stored_path
            .canonicalize()
            .is_ok_and(|canonical| canonical == candidate_root)
}

fn nearby_store_last_write(database: &Path) -> Option<String> {
    let mut wal_path = database.as_os_str().to_os_string();
    wal_path.push("-wal");
    let wal_path = std::path::PathBuf::from(wal_path);

    [database, wal_path.as_path()]
        .into_iter()
        .filter_map(|path| {
            let metadata = std::fs::symlink_metadata(path).ok()?;
            if !metadata.file_type().is_file() {
                return None;
            }
            metadata.modified().ok()
        })
        .max()
        .map(|modified| DateTime::<Utc>::from(modified).to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::decide::{DecideRecordOptions, decide_record};
    use crate::core::focus::{FocusScope, FocusSetOptions, set_focus};
    use crate::core::index::{IndexRebuildOptions, IndexRebuildStatus, rebuild_index};
    use crate::core::init::{InitOptions, init_workspace};
    use crate::core::memory::{
        RememberMemoryOptions, RememberOutcome, RememberWriteControls,
        remember_global_memory_with_controls, remember_memory,
    };
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput};
    use crate::models::{MemoryId, WorkspaceId};

    type TestResult = Result<(), String>;

    #[test]
    fn orient_fast_snippet_keeps_the_480_character_contract() {
        let chars_479 = "a".repeat(479);
        let chars_480 = "b".repeat(480);
        let chars_481 = "c".repeat(481);
        assert_eq!(orient_fast_snippet(&chars_479), chars_479);
        assert_eq!(orient_fast_snippet(&chars_480), chars_480);
        let truncated = orient_fast_snippet(&chars_481);
        assert_eq!(truncated.chars().count(), 480);
        assert!(truncated.ends_with('…'));

        let unicode = "λ".repeat(481);
        let unicode_truncated = orient_fast_snippet(&unicode);
        assert_eq!(unicode_truncated.chars().count(), 480);
        assert!(unicode_truncated.ends_with('…'));

        assert_eq!(
            orient_fast_snippet("  first line  \n\n second line \n third line "),
            "first line\nsecond line…"
        );
    }

    fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
    where
        T: std::fmt::Debug + PartialEq,
    {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn ensure(condition: bool, message: String) -> TestResult {
        if condition { Ok(()) } else { Err(message) }
    }

    fn remember_fixture(
        workspace: &Path,
        content: &str,
        tags: &str,
        valid_from: Option<&str>,
    ) -> Result<String, String> {
        if !workspace.join(".ee").join("ee.db").is_file() {
            let init_report = init_workspace(&InitOptions {
                workspace_path: workspace.to_path_buf(),
                dry_run: false,
                repair_plan: false,
                force: false,
                allow_symlink: false,
                skip_boilerplate: true,
            });
            if !init_report.status.is_success() {
                return Err(format!(
                    "initialize fixture store failed: {:?}",
                    init_report.action_errors
                ));
            }
        }

        remember_memory(&RememberMemoryOptions {
            workspace_path: workspace,
            database_path: None,
            content,
            workflow_id: None,
            level: "procedural",
            kind: "rule",
            tags: Some(tags),
            confidence: 0.9,
            source: Some("file://AGENTS.md#L1"),
            valid_from,
            valid_to: None,
            dry_run: false,
            auto_link: false,
            propose_candidates: false,
            allow_secret_mention: false,
        })
        .map(|report| report.memory_id.to_string())
        .map_err(|error| format!("remember fixture failed: {error:?}"))
    }

    fn orient_test_tempdir() -> Result<tempfile::TempDir, String> {
        let canonical_temp_root = std::env::temp_dir()
            .canonicalize()
            .map_err(|error| format!("canonicalize test temp root: {error}"))?;
        tempfile::Builder::new()
            .prefix("ee-orient-test.")
            .tempdir_in(canonical_temp_root)
            .map_err(|error| error.to_string())
    }

    #[test]
    fn orient_fast_content_returns_admitted_recent_and_lexical_items() -> TestResult {
        let temp = orient_test_tempdir()?;
        let workspace = temp.path();
        let positive_id = remember_fixture(
            workspace,
            "Release checksum verification must run before publishing artifacts.",
            "orient-positive,release",
            None,
        )?;
        let tombstoned_id = remember_fixture(
            workspace,
            "Release checksum tombstoned negative must never surface.",
            "orient-negative,tombstoned",
            None,
        )?;
        let future_id = remember_fixture(
            workspace,
            "Release checksum future negative must never surface.",
            "future,orient-negative",
            Some("2099-01-01T00:00:00Z"),
        )?;

        let database_path = workspace.join(".ee").join("ee.db");
        let connection = DbConnection::open_file(&database_path)
            .map_err(|error| format!("open fixture database: {error}"))?;
        connection
            .tombstone_memory(&tombstoned_id)
            .map_err(|error| format!("tombstone fixture memory: {error}"))?;
        let active_workspace = connection
            .get_memory(&positive_id)
            .map_err(|error| format!("load positive fixture: {error}"))?
            .ok_or_else(|| "positive fixture memory missing".to_owned())?
            .workspace_id;

        let path_provenance_id = MemoryId::from_uuid(uuid::Uuid::from_u128(0x50)).to_string();
        connection
            .insert_memory(
                &path_provenance_id,
                &CreateMemoryInput {
                    workspace_id: active_workspace.clone(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Release checksum provenance must be redacted before orientation."
                        .to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.9,
                    importance: 0.9,
                    provenance_uri: Some(
                        "file:///Users/jemanuel/private/orient-proof/AGENTS.md#L7".to_owned(),
                    ),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: vec!["orient-positive".to_owned(), "provenance".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| format!("insert path-provenance fixture: {error}"))?;

        // Eligible memory carrying a secret-shaped tag and multi-line content:
        // the tag must egress redacted by the shared public-replay policy and
        // the snippet must stay in the promised compact 1-2-line shape.
        let leaky_tag = format!("ghp_{}", "a".repeat(36));
        let secret_tag_id = MemoryId::from_uuid(uuid::Uuid::from_u128(0x54)).to_string();
        connection
            .insert_memory(
                &secret_tag_id,
                &CreateMemoryInput {
                    workspace_id: active_workspace.clone(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Release checksum tag hygiene line one.\n\n  Release checksum tag hygiene line two.  \nRelease checksum tag hygiene line three must be dropped."
                        .to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.9,
                    importance: 0.9,
                    provenance_uri: Some("file://AGENTS.md#L9".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: vec!["orient-positive".to_owned(), leaky_tag.clone()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| format!("insert secret-tag fixture: {error}"))?;

        let secret_id = MemoryId::from_uuid(uuid::Uuid::from_u128(0x51)).to_string();
        connection
            .insert_memory(
                &secret_id,
                &CreateMemoryInput {
                    workspace_id: active_workspace,
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "AWS_SECRET_ACCESS_KEY=orient_fast_must_not_emit".to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.9,
                    importance: 0.9,
                    provenance_uri: Some("file://secret-fixture".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: vec!["orient-negative".to_owned(), "secret".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| format!("insert secret fixture: {error}"))?;

        let other_workspace_id = WorkspaceId::from_uuid(uuid::Uuid::from_u128(0x52)).to_string();
        connection
            .insert_workspace(
                &other_workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.join("other-scope").display().to_string(),
                    name: Some("other-scope".to_owned()),
                },
            )
            .map_err(|error| format!("insert out-of-scope workspace: {error}"))?;
        let out_of_scope_id = MemoryId::from_uuid(uuid::Uuid::from_u128(0x53)).to_string();
        connection
            .insert_memory(
                &out_of_scope_id,
                &CreateMemoryInput {
                    workspace_id: other_workspace_id,
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Release checksum out-of-scope negative must never surface."
                        .to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.9,
                    importance: 0.9,
                    provenance_uri: Some("file://other-scope".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: vec!["orient-negative".to_owned(), "scope".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| format!("insert out-of-scope fixture: {error}"))?;

        set_focus(&FocusSetOptions {
            workspace_path: workspace.to_path_buf(),
            memory_ids: vec![
                secret_id.clone(),
                future_id.clone(),
                out_of_scope_id.clone(),
            ],
            focal_memory_id: Some(out_of_scope_id.clone()),
            pinned_memory_ids: vec![secret_id.clone(), future_id.clone()],
            capacity: 7,
            reason: "Plant ineligible passive-focus candidates.".to_owned(),
            provenance: vec!["orient fast admission regression".to_owned()],
            scope: FocusScope::default(),
        })
        .map_err(|error| format!("set focus fixture: {error:?}"))?;

        let index_report = rebuild_index(&IndexRebuildOptions {
            workspace_path: workspace.to_path_buf(),
            database_path: Some(database_path),
            index_dir: None,
            dry_run: false,
        })
        .map_err(|error| format!("rebuild fixture index: {error:?}"))?;
        ensure_equal(
            &index_report.status,
            &IndexRebuildStatus::Success,
            "fixture index status",
        )?;

        let report = orient_fast_content(&OrientFastContentOptions {
            workspace_path: workspace,
            database_path: None,
            index_dir: None,
            task: "verify release checksums",
            max_tokens: 4_000,
            candidate_pool: 100,
        });
        ensure(
            report.posture == "ready",
            format!(
                "fast-content posture: expected \"ready\", got {:?}; issues={:?}",
                report.posture, report.issues
            ),
        )?;
        if report.recent.is_empty() || report.relevant.is_empty() {
            return Err(format!(
                "fast content must return both sections from a populated indexed store: {report:?}"
            ));
        }
        if report.recent.len() > ORIENT_FAST_CONTENT_LIMIT
            || report.relevant.len() > ORIENT_FAST_CONTENT_LIMIT
        {
            return Err(format!(
                "fast sections must stay within the promised {ORIENT_FAST_CONTENT_LIMIT}-item cap: {report:?}"
            ));
        }
        for item in report.recent.iter().chain(&report.relevant) {
            if item.created_at.is_empty() || item.provenance.is_empty() {
                return Err(format!(
                    "item must bind created_at and provenance: {item:?}"
                ));
            }
            if item.snippet.lines().count() > 2 || item.snippet.contains("\n\n") {
                return Err(format!(
                    "snippet must keep the promised compact 1-2-line shape: {item:?}"
                ));
            }
        }
        let tag_hygiene = report
            .recent
            .iter()
            .chain(&report.relevant)
            .find(|item| item.id == secret_tag_id)
            .ok_or_else(|| format!("eligible secret-tag fixture must stay surfaced: {report:?}"))?;
        if !tag_hygiene
            .tags
            .iter()
            .any(|tag| tag == &crate::policy::redaction_placeholder("github_token"))
        {
            return Err(format!(
                "secret-shaped tag must egress through the shared redaction policy: {tag_hygiene:?}"
            ));
        }
        if !tag_hygiene.snippet.starts_with(
            "Release checksum tag hygiene line one.\nRelease checksum tag hygiene line two.",
        ) || !tag_hygiene.snippet.ends_with('…')
        {
            return Err(format!(
                "multi-line content must normalize to two trimmed lines plus ellipsis: {tag_hygiene:?}"
            ));
        }
        let positive_recent = report.recent.iter().any(|item| item.id == positive_id);
        let positive_relevant = report.relevant.iter().any(|item| item.id == positive_id);
        if !positive_recent || !positive_relevant {
            return Err(format!(
                "the admissible positive must remain independently visible in recent and relevant: {report:?}"
            ));
        }
        let positive = report
            .relevant
            .iter()
            .find(|item| item.id == positive_id)
            .ok_or_else(|| "positive relevant item missing".to_owned())?;
        if !positive.snippet.contains("Release checksum verification")
            || !positive.tags.iter().any(|tag| tag == "orient-positive")
        {
            return Err(format!(
                "positive content/tags were not bound: {positive:?}"
            ));
        }
        ensure_equal(
            &positive.provenance,
            &vec![RenderedPackProvenance {
                uri: "file://AGENTS.md#L1".to_owned(),
                scheme: "file".to_owned(),
                label: "AGENTS.md:L1".to_owned(),
                locator: Some("L1".to_owned()),
                note: format!(
                    "Memory {positive_id} selected by bounded direct lexical orientation retrieval; evidenceFreshness=missing_source."
                ),
            }],
            "exact direct-lexical provenance",
        )?;
        let path_provenance = report
            .relevant
            .iter()
            .find(|item| item.id == path_provenance_id)
            .and_then(|item| item.provenance.first())
            .ok_or_else(|| "path-provenance relevant item missing".to_owned())?;
        ensure_equal(
            &path_provenance.uri,
            &"file://[REDACTED_PATH]#L7".to_owned(),
            "absolute provenance URI redaction",
        )?;
        ensure(
            !path_provenance.label.contains("/Users/") && !path_provenance.note.contains("/Users/"),
            format!("rendered provenance leaked an absolute path: {path_provenance:?}"),
        )?;

        let forbidden = [secret_id, tombstoned_id, future_id, out_of_scope_id];
        for forbidden_id in forbidden {
            if report
                .recent
                .iter()
                .chain(&report.relevant)
                .any(|item| item.id == forbidden_id)
            {
                return Err(format!("ineligible memory surfaced: {forbidden_id}"));
            }
        }
        let encoded = serde_json::to_string(&report).map_err(|error| error.to_string())?;
        if encoded.contains("orient_fast_must_not_emit") {
            return Err("secret-shaped content leaked through fast content".to_owned());
        }
        if encoded.contains(&leaky_tag) {
            return Err("secret-shaped tag leaked raw through fast content".to_owned());
        }
        Ok(())
    }

    #[test]
    fn orient_fast_relevant_provider_is_direct_lexical_and_non_persisting() -> TestResult {
        let source = include_str!("orient.rs");
        let provider_start = source
            .find("fn orient_fast_relevant_content(")
            .ok_or_else(|| "orient-fast relevant provider source missing".to_owned())?;
        let provider_end = source[provider_start..]
            .find("fn orient_fast_snippet(")
            .map(|offset| provider_start + offset)
            .ok_or_else(|| "orient-fast provider source boundary missing".to_owned())?;
        let provider = &source[provider_start..provider_end];
        for required in [
            "DbConnection::open_file_read_only",
            "run_context_search_with_preloaded_memories",
            "source_mode: SearchSourceMode::LexicalOnly",
            "strict_source_mode: true",
            ".min(ORIENT_FAST_CANDIDATE_POOL_LIMIT)",
        ] {
            ensure(
                provider.contains(required),
                format!("direct lexical provider lost required guard {required:?}"),
            )?;
        }
        for forbidden in [
            "run_context_pack",
            "SearchSourceMode::Hybrid",
            "SearchSourceMode::SemanticOnly",
            "persist_pack",
        ] {
            ensure(
                !provider.contains(forbidden),
                format!("direct lexical provider contains forbidden path {forbidden:?}"),
            )?;
        }
        ensure(
            provider.contains("&connection,\n        None,\n        &determinism,\n        None,"),
            "direct lexical provider must pass no audit connection and no embedder override"
                .to_owned(),
        )?;
        let search_offset = provider
            .find("let context_search = run_context_search_with_preloaded_memories")
            .ok_or_else(|| "context_search construction missing".to_owned())?;
        let degradation_offset = provider
            .find("context_search\n        .report\n        .degraded")
            .ok_or_else(|| "context_search degradation propagation missing".to_owned())?;
        ensure(
            degradation_offset > search_offset,
            "provider degradations must be read only after context_search is constructed"
                .to_owned(),
        )?;
        Ok(())
    }

    #[test]
    fn orient_fast_missing_store_emits_executable_recent_provider_failure() -> TestResult {
        let temp = orient_test_tempdir()?;
        let workspace = temp.path().join("missing-fast-provider");
        let report = orient_fast_content(&OrientFastContentOptions {
            workspace_path: &workspace,
            database_path: None,
            index_dir: None,
            task: "prove recent provider failure",
            max_tokens: 4_000,
            candidate_pool: 100,
        });

        ensure_equal(&report.posture, &"unavailable", "missing-store posture")?;
        ensure(
            report.recent.is_empty() && report.relevant.is_empty(),
            format!("missing providers must emit no content: {report:?}"),
        )?;
        let recent_issue = report
            .issues
            .iter()
            .find(|issue| issue.code == ORIENT_FAST_RECENT_UNAVAILABLE_CODE)
            .ok_or_else(|| format!("recent provider failure issue missing: {report:?}"))?;
        ensure_equal(&recent_issue.component, &"recent", "component")?;
        ensure(
            recent_issue
                .repair
                .as_deref()
                .is_some_and(|repair| repair.contains("ee doctor --workspace . --json")),
            format!("recent provider failure must carry executable repair: {recent_issue:?}"),
        )
    }

    #[test]
    fn orient_fast_promotes_provider_affecting_validity_degradations() -> TestResult {
        for code in [
            "malformed_validity_filtered",
            "validity_filtered_significant_recall_drop",
        ] {
            let degradation = SearchDegradation {
                code: code.to_owned(),
                severity: "warning".to_owned(),
                message: "validity admission reduced provider output".to_owned(),
                repair: Some("ee doctor --workspace . --json".to_owned()),
            };
            let issue = orient_fast_relevant_provider_issue(&degradation)
                .ok_or_else(|| format!("{code} was not promoted"))?;
            ensure_equal(&issue.code, &code.to_owned(), "promoted code")?;
            ensure_equal(&issue.status, &"degraded", "promoted status")?;
        }
        Ok(())
    }

    #[test]
    fn orient_fast_provenance_keeps_content_with_explicit_canonical_degradation() -> TestResult {
        let workspace = Path::new("/Users/example/private-workspace");
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(0x61)).to_string();
        let mut memory = StoredMemory {
            id: memory_id.clone(),
            workspace_id: crate::core::curate::stable_workspace_id(workspace),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: "Provenance degradation must remain visible.".to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.9,
            importance: 0.9,
            provenance_uri: Some("missing-scheme".to_owned()),
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            provenance_chain_hash: None,
            provenance_chain_hash_version: "v1".to_owned(),
            provenance_verification_status: "unverified".to_owned(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: "2026-08-12T00:00:00Z".to_owned(),
            updated_at: "2026-08-12T00:00:00Z".to_owned(),
            tombstoned_at: None,
            valid_from: None,
            valid_to: None,
        };
        let mut cache = crate::core::memory::EvidenceFreshnessFileCache::default();
        let mut issues = Vec::new();
        let invalid = orient_fast_provenance(&memory, workspace, &mut cache, &mut issues)
            .ok_or_else(|| "invalid provenance must retain an explicit fallback".to_owned())?;
        ensure_equal(&invalid.scheme, &"ee-mem".to_owned(), "fallback scheme")?;
        let expected_repair = format!("ee memory show {memory_id} --json");
        ensure(
            issues.iter().any(|issue| {
                issue.code == "context_invalid_provenance"
                    && issue.message.contains(&memory_id)
                    && issue.repair.as_deref() == Some(expected_repair.as_str())
            }),
            format!("invalid provenance fallback must never be silent: {issues:?}"),
        )?;

        memory.provenance_uri = Some("file://evidence-that-does-not-exist.md".to_owned());
        issues.clear();
        let missing = orient_fast_provenance(&memory, workspace, &mut cache, &mut issues)
            .ok_or_else(|| "missing provenance source must keep the admitted item".to_owned())?;
        ensure_equal(
            &missing.scheme,
            &"file".to_owned(),
            "preserved source scheme",
        )?;
        ensure(
            issues.iter().any(|issue| {
                issue.code == "context_evidence_freshness_missing_source"
                    && issue
                        .message
                        .contains("evidence freshness is missing_source")
                    && issue.message.contains("[REDACTED_PATH]")
                    && issue.repair.as_deref().is_some_and(|repair| {
                        repair.contains("Restore the file or revise the memory provenance URI")
                    })
            }),
            format!("canonical evidence freshness degradation missing: {issues:?}"),
        )
    }

    #[test]
    fn orient_fast_content_hydrates_isolated_global_store() -> TestResult {
        const CHILD_MARKER: &str = "EE_ORIENT_GLOBAL_STORE_TEST_CHILD";
        const TEST_ROOT: &str = "EE_ORIENT_GLOBAL_STORE_TEST_ROOT";

        if std::env::var_os(CHILD_MARKER).is_some() {
            let root = std::env::var_os(TEST_ROOT)
                .map(std::path::PathBuf::from)
                .ok_or_else(|| "isolated global-store child root is missing".to_owned())?;
            return orient_fast_content_hydrates_isolated_global_store_child(&root);
        }

        let temp = orient_test_tempdir()?;
        let output =
            std::process::Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
                .arg("--exact")
                .arg("core::orient::tests::orient_fast_content_hydrates_isolated_global_store")
                .arg("--nocapture")
                .arg("--test-threads=1")
                .env(CHILD_MARKER, "1")
                .env(TEST_ROOT, temp.path())
                .env("XDG_DATA_HOME", temp.path())
                .env_remove("HOME")
                .output()
                .map_err(|error| format!("launch isolated global-store child: {error}"))?;
        if output.status.success() {
            return Ok(());
        }
        Err(format!(
            "isolated global-store child failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    }

    fn orient_fast_content_hydrates_isolated_global_store_child(root: &Path) -> TestResult {
        let paths = crate::core::global_store::default_global_store_paths_from_env()
            .map_err(|error| format!("resolve isolated global store: {error}"))?;
        ensure(
            paths.root.starts_with(root),
            format!(
                "global store escaped isolated root: root={}, store={}",
                root.display(),
                paths.root.display()
            ),
        )?;

        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace)
            .map_err(|error| format!("create isolated workspace: {error}"))?;
        remember_fixture(
            &workspace,
            "Local workspace decoy about garden irrigation.",
            "orient-local-decoy",
            None,
        )?;

        let global_outcome = remember_global_memory_with_controls(
            &RememberMemoryOptions {
                workspace_path: &workspace,
                database_path: None,
                content: "Hermetic quasar checksum sentinel applies across workspaces.",
                workflow_id: None,
                level: "procedural",
                kind: "rule",
                tags: Some("orient-global-proof"),
                confidence: 0.9,
                source: None,
                valid_from: None,
                valid_to: None,
                dry_run: false,
                auto_link: false,
                propose_candidates: false,
                allow_secret_mention: false,
            },
            &RememberWriteControls::default(),
        )
        .map_err(|error| format!("remember isolated global memory: {error:?}"))?;
        let global_id = match global_outcome {
            RememberOutcome::Created(report) => report.memory_id.to_string(),
            other => return Err(format!("global memory was not created: {other:?}")),
        };

        let database_path = workspace.join(".ee").join("ee.db");
        let index_report = rebuild_index(&IndexRebuildOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database_path),
            index_dir: None,
            dry_run: false,
        })
        .map_err(|error| format!("rebuild isolated workspace index: {error:?}"))?;
        ensure_equal(
            &index_report.status,
            &IndexRebuildStatus::Success,
            "isolated workspace index status",
        )?;

        let report = orient_fast_content(&OrientFastContentOptions {
            workspace_path: &workspace,
            database_path: None,
            index_dir: None,
            task: "quasar checksum sentinel",
            max_tokens: 4_000,
            candidate_pool: 100,
        });
        ensure(
            report.posture == "ready" && report.issues.is_empty(),
            format!("isolated global fast content was not ready: {report:?}"),
        )?;
        let global_item = report
            .relevant
            .iter()
            .find(|item| item.id == global_id)
            .ok_or_else(|| format!("global item missing from relevant content: {report:?}"))?;
        ensure(
            global_item
                .snippet
                .contains("Hermetic quasar checksum sentinel"),
            format!("global content missing from item: {global_item:?}"),
        )?;
        ensure(
            global_item
                .tags
                .iter()
                .any(|tag| tag == GLOBAL_MEMORY_SCOPE_TAG),
            format!("global scope tag missing from item: {global_item:?}"),
        )?;
        ensure(
            global_item
                .provenance
                .iter()
                .any(|source| source.note.contains("cross_shard_read")),
            format!("global provenance lane missing from item: {global_item:?}"),
        )?;
        ensure(
            !report.recent.iter().any(|item| item.id == global_id),
            format!("global item leaked into workspace-recency section: {report:?}"),
        )
    }

    #[test]
    fn orient_decisions_reuses_due_decision_query() -> TestResult {
        let temp = orient_test_tempdir()?;
        let init_report = init_workspace(&InitOptions {
            workspace_path: temp.path().to_path_buf(),
            dry_run: false,
            repair_plan: false,
            force: false,
            allow_symlink: false,
            skip_boilerplate: true,
        });
        if !init_report.status.is_success() {
            return Err(format!(
                "initialize decision fixture store failed: {:?}",
                init_report.action_errors
            ));
        }
        let now = DateTime::parse_from_rfc3339("2026-06-15T12:00:00Z")
            .map_err(|error| error.to_string())?
            .with_timezone(&Utc);
        decide_record(&DecideRecordOptions {
            workspace_path: temp.path(),
            database_path: None,
            topic: "Prompt format",
            chosen: "markdown",
            alternatives: vec!["json".to_owned()],
            rationale: "Humans can scan it quickly.",
            revisit_by: Some("2026-06-16T12:00:00Z"),
            supersedes: None,
            dry_run: false,
            actor: Some("test"),
            now: Some(now),
        })
        .map_err(|error| error.to_string())?;

        let report = orient_decisions(&OrientDecisionOptions {
            workspace_path: temp.path(),
            database_path: None,
            warning_days: Some(2),
            limit: 10,
            now: Some(now),
        })
        .map_err(|error| error.to_string())?;
        ensure_equal(&report.schema, &ORIENT_DECISIONS_SCHEMA_V1, "schema")?;
        ensure_equal(&report.due_count, &1, "due count")?;
        ensure_equal(
            &report.decisions[0].topic,
            &"Prompt format".to_owned(),
            "decision topic",
        )
    }

    // ===== nearby-store discovery tests (bd-orient-store-discovery-ft1z5) =====

    fn scan_budget() -> std::time::Duration {
        // Tests use a generous budget so slow CI disks cannot flake the
        // truncation-free assertions.
        std::time::Duration::from_secs(10)
    }

    fn discover_local_stores(workspace: &Path, budget: std::time::Duration) -> NearbyStoreScan {
        let absent_registry = workspace.join("registry-not-present-for-local-scan.db");
        let workspace_root = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf());
        let addressed_database = workspace_root.join(".ee").join("ee.db");
        discover_nearby_stores_with_registry(
            workspace,
            &addressed_database,
            budget,
            Some(&absent_registry),
        )
    }

    #[test]
    fn discovery_finds_populated_child_store() -> TestResult {
        let temp = orient_test_tempdir()?;
        std::fs::create_dir_all(temp.path().join(".git")).map_err(|error| error.to_string())?;
        let child = temp.path().join("campaign");
        std::fs::create_dir_all(&child).map_err(|error| error.to_string())?;
        remember_fixture(&child, "Nearby store rule one.", "nearby", None)?;

        let scan = discover_local_stores(temp.path(), scan_budget());
        ensure_equal(&scan.stores.len(), &1_usize, "one nearby store")?;
        ensure(
            scan.stores[0].workspace_root.ends_with("campaign"),
            format!("workspace root wrong: {:?}", scan.stores[0]),
        )?;
        ensure(
            scan.stores[0].documents >= 1,
            format!("document count wrong: {:?}", scan.stores[0]),
        )?;
        ensure(
            scan.stores[0].last_write.is_some(),
            "last_write must be populated".to_owned(),
        )
    }

    #[test]
    fn discovery_requires_registry_identity_but_local_probe_recovers_same_store() -> TestResult {
        let temp = orient_test_tempdir()?;
        let workspace = temp.path().join("identity-candidate");
        let store_dir = workspace.join(".ee");
        std::fs::create_dir_all(&store_dir).map_err(|error| error.to_string())?;
        let workspace_root = workspace
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let database = store_dir.join("ee.db");
        let workspace_id = WorkspaceId::from_uuid(uuid::Uuid::from_u128(0x71)).to_string();
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace_with_scope(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_root.display().to_string(),
                    name: Some("identity candidate".to_owned()),
                },
                &crate::db::WorkspaceScopeFields {
                    scope_kind: "standalone".to_owned(),
                    repository_root: Some(workspace_root.display().to_string()),
                    repository_fingerprint: Some("repo:actual".to_owned()),
                    subproject_path: None,
                },
            )
            .map_err(|error| error.to_string())?;
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(0x72)).to_string();
        connection
            .insert_memory(
                &memory_id,
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Identity-correct nearby store evidence.".to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.9,
                    importance: 0.9,
                    provenance_uri: None,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: vec!["nearby-identity".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let local = NearbyStoreCandidate {
            workspace_root: workspace_root.clone(),
            registry_identity: None,
        };
        let wrong_workspace = NearbyStoreCandidate {
            workspace_root: workspace_root.clone(),
            registry_identity: Some(NearbyStoreRegistryIdentity {
                workspace_id: WorkspaceId::from_uuid(uuid::Uuid::from_u128(0x73)).to_string(),
                repository_fingerprint: Some("repo:actual".to_owned()),
            }),
        };
        let wrong_repository = NearbyStoreCandidate {
            workspace_root: workspace_root.clone(),
            registry_identity: Some(NearbyStoreRegistryIdentity {
                workspace_id: workspace_id.clone(),
                repository_fingerprint: Some("repo:stale".to_owned()),
            }),
        };
        let matching_registry = NearbyStoreCandidate {
            workspace_root,
            registry_identity: Some(NearbyStoreRegistryIdentity {
                workspace_id,
                repository_fingerprint: Some("repo:actual".to_owned()),
            }),
        };

        let mut seen_candidates = BTreeSet::new();
        ensure(
            add_nearby_store_candidate(
                wrong_repository.workspace_root.clone(),
                wrong_repository.registry_identity.clone(),
                &mut seen_candidates,
            )
            .is_some()
                && add_nearby_store_candidate(
                    local.workspace_root.clone(),
                    None,
                    &mut seen_candidates,
                )
                .is_some(),
            "a stale registry probe must not suppress a later local-path probe".to_owned(),
        )?;

        ensure(
            nearby_store_profile(&database, &wrong_workspace).is_none()
                && nearby_store_profile(&database, &wrong_repository).is_none(),
            "mismatched registry workspace/repository identity must be rejected".to_owned(),
        )?;
        ensure_equal(
            &nearby_store_profile(&database, &matching_registry).map(|profile| profile.0),
            &Some(1_u64),
            "matching registry identity",
        )?;
        ensure_equal(
            &nearby_store_profile(&database, &local).map(|profile| profile.0),
            &Some(1_u64),
            "local path identity remains independently discoverable",
        )
    }

    #[test]
    fn discovery_last_write_uses_newer_wal_and_ignores_shm() -> TestResult {
        let temp = orient_test_tempdir()?;
        let database = temp.path().join("ee.db");
        let wal = temp.path().join("ee.db-wal");
        let shm = temp.path().join("ee.db-shm");
        std::fs::write(&database, b"database fixture").map_err(|error| error.to_string())?;
        std::fs::write(&wal, b"wal fixture").map_err(|error| error.to_string())?;
        std::fs::write(&shm, b"shm fixture").map_err(|error| error.to_string())?;

        let database_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let wal_time = database_time + std::time::Duration::from_secs(60);
        let shm_time = wal_time + std::time::Duration::from_secs(60);
        for (path, modified) in [
            (&database, database_time),
            (&wal, wal_time),
            (&shm, shm_time),
        ] {
            std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .map_err(|error| error.to_string())?
                .set_times(std::fs::FileTimes::new().set_modified(modified))
                .map_err(|error| error.to_string())?;
        }

        let before = [
            std::fs::read(&database).map_err(|error| error.to_string())?,
            std::fs::read(&wal).map_err(|error| error.to_string())?,
            std::fs::read(&shm).map_err(|error| error.to_string())?,
        ];
        let last_write = nearby_store_last_write(&database);
        let after = [
            std::fs::read(&database).map_err(|error| error.to_string())?,
            std::fs::read(&wal).map_err(|error| error.to_string())?,
            std::fs::read(&shm).map_err(|error| error.to_string())?,
        ];

        ensure_equal(
            &last_write,
            &Some(DateTime::<Utc>::from(wal_time).to_rfc3339()),
            "newest durable store write",
        )?;
        ensure_equal(
            &after,
            &before,
            "mtime inspection must not mutate artifacts",
        )
    }

    #[test]
    fn discovery_respects_depth_skip_markers_and_ranks_candidates() -> TestResult {
        let expected_skip_dirs: &[&str] = &[
            ".git",
            ".hg",
            ".svn",
            ".venv",
            "venv",
            "node_modules",
            "target",
            "dist",
            "build",
            ".cache",
            ".rch-tmp",
            ".doctor",
        ];
        ensure_equal(
            &NEARBY_STORE_SKIP_DIRS,
            &expected_skip_dirs,
            "bounded discovery skip list",
        )?;

        let temp = orient_test_tempdir()?;
        std::fs::create_dir_all(temp.path().join(".git")).map_err(|error| error.to_string())?;
        let small = temp.path().join("small");
        let campaign = temp.path().join("campaign-marker");
        let depth_three = temp
            .path()
            .join("nested")
            .join("level-two")
            .join("depth-three");
        let depth_four = depth_three.join("depth-four");
        let skipped = temp.path().join("target").join("skipped-store");
        for workspace in [&small, &campaign, &depth_three, &depth_four, &skipped] {
            std::fs::create_dir_all(workspace).map_err(|error| error.to_string())?;
        }

        remember_fixture(&small, "Small store single rule.", "nearby", None)?;
        for index in 1..=2 {
            remember_fixture(
                &campaign,
                &format!("Campaign-marker store rule {index}."),
                "nearby",
                None,
            )?;
        }
        std::fs::rename(campaign.join(".ee"), campaign.join(".ee-campaign"))
            .map_err(|error| error.to_string())?;
        for index in 1..=3 {
            remember_fixture(
                &depth_three,
                &format!("Depth-three store rule {index}."),
                "nearby",
                None,
            )?;
        }
        for index in 1..=4 {
            remember_fixture(
                &depth_four,
                &format!("Depth-four store rule {index}."),
                "nearby",
                None,
            )?;
        }
        for index in 1..=5 {
            remember_fixture(
                &skipped,
                &format!("Skipped target store rule {index}."),
                "nearby",
                None,
            )?;
        }

        // This test owns traversal, skip, and ranking semantics. Keep it off
        // the outer wall-clock wrapper so cold parallel FrankenSQLite fixture
        // initialization cannot turn a semantic assertion into a deadline
        // assertion. The dedicated zero-budget and blocking-operation tests
        // below independently pin the caller-visible deadline contract.
        let workspace_root = temp
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let addressed_database = workspace_root.join(".ee").join("ee.db");
        let absent_registry = temp.path().join("registry-not-present-for-ranking.db");
        let cx = Cx::detached_cancel_context();
        let mut ignore_progress = |_scan: &NearbyStoreScan| {};
        let scan = scan_nearby_stores_with_registry(
            &cx,
            temp.path(),
            &addressed_database,
            std::time::Duration::MAX,
            Some(&absent_registry),
            &mut ignore_progress,
        );
        ensure_equal(&scan.stores.len(), &3_usize, "three eligible stores")?;
        ensure(
            scan.stores[0].workspace_root.ends_with("depth-three")
                && scan.stores[0].documents == 3
                && scan.stores[1].workspace_root.ends_with("campaign-marker")
                && scan.stores[1].documents == 2
                && scan.stores[1].store_dir.ends_with(".ee-campaign")
                && scan.stores[2].workspace_root.ends_with("small")
                && scan.stores[2].documents == 1,
            format!(
                "eligible stores must rank by document count and include .ee-campaign: {:?}",
                scan.stores
            ),
        )?;
        ensure(
            scan.stores.iter().all(|store| {
                !store.workspace_root.ends_with("depth-four")
                    && !store.workspace_root.ends_with("skipped-store")
            }),
            format!(
                "depth-four and target-contained stores must be excluded: {:?}",
                scan.stores
            ),
        )
    }

    #[test]
    fn discovery_publication_order_is_sorted_fifo_across_siblings_and_depths() -> TestResult {
        let temp = orient_test_tempdir()?;
        std::fs::create_dir_all(temp.path().join(".git")).map_err(|error| error.to_string())?;
        let left = temp.path().join("a-left");
        let left_child = left.join("a-child");
        let right = temp.path().join("z-right");
        let right_child = right.join("z-child");
        for (workspace, content) in [
            (&left, "Left depth-one store."),
            (&left_child, "Left depth-two store."),
            (&right, "Right depth-one store."),
            (&right_child, "Right depth-two store."),
        ] {
            remember_fixture(workspace, content, "nearby", None)?;
        }

        let addressed_database = temp.path().join(".ee").join("ee.db");
        let absent_registry = temp.path().join("absent-fifo-registry.db");
        let cx = Cx::detached_cancel_context();
        let mut seen = BTreeSet::new();
        let mut publication_order = Vec::new();
        let mut publish = |snapshot: &NearbyStoreScan| {
            for store in &snapshot.stores {
                if seen.insert(store.workspace_root.clone()) {
                    publication_order.push(store.workspace_root.clone());
                }
            }
        };
        let scan = scan_nearby_stores_with_registry(
            &cx,
            temp.path(),
            &addressed_database,
            std::time::Duration::MAX,
            Some(&absent_registry),
            &mut publish,
        );

        let expected = [&left, &right, &left_child, &right_child]
            .into_iter()
            .map(|path| {
                path.canonicalize()
                    .map(|path| path.display().to_string())
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        ensure_equal(&scan.stores.len(), &4_usize, "four FIFO candidates")?;
        ensure_equal(
            &publication_order,
            &expected,
            "sorted FIFO publication across two siblings and two depths",
        )
    }

    #[test]
    fn discovery_publishes_local_child_before_registry_candidates() -> TestResult {
        let temp = orient_test_tempdir()?;
        let addressed = temp.path().join("addressed");
        let local = addressed.join("local-child");
        let registered = temp.path().join("registered-remote");
        std::fs::create_dir_all(addressed.join(".git")).map_err(|error| error.to_string())?;
        remember_fixture(
            &local,
            "Local child must be inspected first.",
            "nearby",
            None,
        )?;
        remember_fixture(
            &registered,
            "Registered remote must remain discoverable.",
            "nearby",
            None,
        )?;

        let registered_database = registered.join(".ee").join("ee.db");
        let registered_connection = DbConnection::open_file_read_only(&registered_database)
            .map_err(|error| error.to_string())?;
        let registered_workspace = registered_connection
            .list_workspaces()
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| "registered fixture workspace missing".to_owned())?;
        registered_connection
            .close()
            .map_err(|error| error.to_string())?;

        let registry_path = temp.path().join("workspace-registry.db");
        let registry =
            DbConnection::open_file(&registry_path).map_err(|error| error.to_string())?;
        registry.migrate().map_err(|error| error.to_string())?;
        registry
            .insert_workspace(
                &registered_workspace.id,
                &CreateWorkspaceInput {
                    path: registered_workspace.path,
                    name: Some("registered-remote".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        registry.close().map_err(|error| error.to_string())?;

        let addressed_database = addressed.join(".ee").join("ee.db");
        let cx = Cx::detached_cancel_context();
        let mut first_published = None;
        let mut publish = |snapshot: &NearbyStoreScan| {
            if first_published.is_none() {
                first_published = snapshot
                    .stores
                    .first()
                    .map(|store| store.workspace_root.clone());
            }
        };
        let scan = scan_nearby_stores_with_registry(
            &cx,
            &addressed,
            &addressed_database,
            std::time::Duration::MAX,
            Some(&registry_path),
            &mut publish,
        );

        ensure_equal(
            &scan.stores.len(),
            &2_usize,
            "local and registered candidates",
        )?;
        ensure(
            first_published
                .as_deref()
                .is_some_and(|path| path.ends_with("local-child")),
            format!(
                "local discovery must publish before consulting registry candidates; first={first_published:?}"
            ),
        )
    }

    #[test]
    fn discovery_parent_scan_stops_at_nearest_git_root_and_not_above_workspace_git_root()
    -> TestResult {
        let temp = orient_test_tempdir()?;
        let git_root = temp.path().join("nearest-git-root");
        let workspace = git_root.join("subdir").join("workspace");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        remember_fixture(temp.path(), "Store above Git root.", "nearby", None)?;
        remember_fixture(&git_root, "Store at nearest Git root.", "nearby", None)?;
        std::fs::create_dir_all(git_root.join(".git")).map_err(|error| error.to_string())?;

        let ancestor_scan = discover_local_stores(&workspace, scan_budget());
        ensure_equal(
            &ancestor_scan.stores.len(),
            &1_usize,
            "only nearest Git-root store",
        )?;
        ensure(
            ancestor_scan.stores[0]
                .workspace_root
                .ends_with("nearest-git-root"),
            format!(
                "nearest Git root must be included and ancestors excluded: {:?}",
                ancestor_scan.stores
            ),
        )?;

        std::fs::create_dir_all(workspace.join(".git")).map_err(|error| error.to_string())?;
        let workspace_root_scan = discover_local_stores(&workspace, scan_budget());
        ensure_equal(
            &workspace_root_scan.stores.len(),
            &0_usize,
            "workspace Git root must not scan parents",
        )
    }

    #[test]
    fn discovery_zero_budget_truncates_before_scanning() -> TestResult {
        let temp = orient_test_tempdir()?;
        let child = temp.path().join("populated-child");
        std::fs::create_dir_all(&child).map_err(|error| error.to_string())?;
        remember_fixture(&child, "Unscanned store rule.", "nearby", None)?;

        let scan = discover_local_stores(temp.path(), std::time::Duration::ZERO);
        ensure(scan.truncated, "zero-budget scan must truncate".to_owned())?;
        ensure_equal(
            &scan.stores.len(),
            &0_usize,
            "zero-budget scan must not inspect candidates",
        )
    }

    #[test]
    fn discovery_deadline_bounds_a_blocking_operation_for_the_caller() -> TestResult {
        let started = std::time::Instant::now();
        let budget = std::time::Duration::from_millis(20);
        let limiter = std::sync::Arc::new(NearbyStoreScanWorkerLimiter::new(1));
        let scan = run_nearby_store_scan_bounded_with_limiter(budget, limiter, |_, _| {
            std::thread::sleep(std::time::Duration::from_millis(250));
            NearbyStoreScan {
                stores: vec![NearbyStore {
                    workspace_root: "late".to_owned(),
                    store_dir: "late/.ee".to_owned(),
                    documents: 1,
                    last_write: None,
                }],
                truncated: false,
            }
        });
        let elapsed = started.elapsed();

        ensure(
            elapsed < std::time::Duration::from_millis(200),
            format!(
                "a blocking operation must not hold the caller past the hard deadline; elapsed={elapsed:?}"
            ),
        )?;
        ensure(
            scan.truncated,
            "a timed-out blocking operation must report truncated=true".to_owned(),
        )?;
        ensure_equal(
            &scan.stores,
            &Vec::new(),
            "a result produced after the deadline must be discarded",
        )
    }

    #[test]
    fn discovery_timeout_preserves_partial_progress_and_requests_cancellation() -> TestResult {
        let budget = std::time::Duration::from_millis(30);
        let cancellation_observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_observation = std::sync::Arc::clone(&cancellation_observed);
        let limiter = std::sync::Arc::new(NearbyStoreScanWorkerLimiter::new(1));
        let scan =
            run_nearby_store_scan_bounded_with_limiter(budget, limiter, move |cx, publish| {
                let partial = NearbyStoreScan {
                    stores: vec![NearbyStore {
                        workspace_root: "proved-before-timeout".to_owned(),
                        store_dir: "proved-before-timeout/.ee".to_owned(),
                        documents: 2,
                        last_write: None,
                    }],
                    truncated: false,
                };
                publish(&partial);
                while cx.checkpoint().is_ok() {
                    std::thread::yield_now();
                }
                worker_observation.store(true, std::sync::atomic::Ordering::Release);
                let mut cancelled = partial;
                cancelled.truncated = true;
                cancelled
            });

        for _ in 0..100 {
            if cancellation_observed.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        ensure(
            scan.truncated,
            "partial timeout result must be marked truncated".to_owned(),
        )?;
        ensure_equal(
            &scan.stores.len(),
            &1_usize,
            "candidate proved before timeout survives",
        )?;
        ensure(
            cancellation_observed.load(std::sync::atomic::Ordering::Acquire),
            "worker must observe the Asupersync timeout cancellation".to_owned(),
        )
    }

    #[test]
    fn discovery_worker_permit_remains_held_until_blocking_worker_exits() -> TestResult {
        let limiter = std::sync::Arc::new(NearbyStoreScanWorkerLimiter::new(1));
        let first_limiter = std::sync::Arc::clone(&limiter);
        let first = run_nearby_store_scan_bounded_with_limiter(
            std::time::Duration::from_millis(10),
            first_limiter,
            |_, _| {
                std::thread::sleep(std::time::Duration::from_millis(100));
                NearbyStoreScan::default()
            },
        );
        ensure(first.truncated, "first blocking scan times out".to_owned())?;
        ensure_equal(&limiter.active_count(), &1_usize, "permit retained")?;

        let second_invoked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let second_marker = std::sync::Arc::clone(&second_invoked);
        let second = run_nearby_store_scan_bounded_with_limiter(
            std::time::Duration::from_millis(10),
            std::sync::Arc::clone(&limiter),
            move |_, _| {
                second_marker.store(true, std::sync::atomic::Ordering::Release);
                NearbyStoreScan::default()
            },
        );
        ensure(
            second.truncated,
            "permit refusal is explicitly truncated".to_owned(),
        )?;
        ensure(
            !second_invoked.load(std::sync::atomic::Ordering::Acquire),
            "a refused scan must not spawn another worker".to_owned(),
        )?;

        for _ in 0..150 {
            if limiter.active_count() == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        ensure_equal(&limiter.active_count(), &0_usize, "permit released on exit")
    }

    #[test]
    fn discovery_excludes_own_store_and_empty_dirs() -> TestResult {
        let temp = orient_test_tempdir()?;
        std::fs::create_dir_all(temp.path().join(".git")).map_err(|error| error.to_string())?;
        // The addressed workspace has its own populated store: it must not
        // report itself, and an empty sibling dir contributes nothing.
        remember_fixture(temp.path(), "Own store rule.", "own", None)?;
        std::fs::create_dir_all(temp.path().join("empty_child"))
            .map_err(|error| error.to_string())?;

        let scan = discover_local_stores(temp.path(), scan_budget());
        ensure_equal(&scan.stores.len(), &0_usize, "no nearby stores reported")
    }

    #[test]
    fn addressed_campaign_store_with_rows_is_not_empty() -> TestResult {
        let temp = orient_test_tempdir()?;
        for index in 1..=NEARBY_STORE_THIN_LIVE_MEMORY_THRESHOLD {
            remember_fixture(
                temp.path(),
                &format!("Addressed campaign store durable content {index}."),
                "campaign",
                None,
            )?;
        }
        std::fs::rename(temp.path().join(".ee"), temp.path().join(".ee-campaign"))
            .map_err(|error| error.to_string())?;

        let database = resolved_addressed_database_path(temp.path());
        let expected = temp
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?
            .join(".ee-campaign")
            .join("ee.db");
        ensure_equal(&database, &expected, "resolved addressed campaign database")?;
        ensure_equal(
            &addressed_store_state(temp.path(), &database),
            &AddressedStoreState::Populated {
                live_memories: NEARBY_STORE_THIN_LIVE_MEMORY_THRESHOLD,
            },
            "populated addressed .ee-campaign store",
        )
    }

    #[test]
    fn addressed_store_state_retains_exact_thin_count_and_threshold() -> TestResult {
        let temp = orient_test_tempdir()?;
        let database = temp.path().join(".ee").join("ee.db");
        for expected in 1..NEARBY_STORE_THIN_LIVE_MEMORY_THRESHOLD {
            remember_fixture(
                temp.path(),
                &format!("Thin addressed-store fact {expected}."),
                "thin-store",
                None,
            )?;
            ensure_equal(
                &addressed_store_state(temp.path(), &database),
                &AddressedStoreState::Thin {
                    database: database.clone(),
                    live_memories: expected,
                },
                "exact thin addressed-store count",
            )?;
        }
        remember_fixture(
            temp.path(),
            "Threshold-crossing addressed-store fact.",
            "thin-store",
            None,
        )?;
        ensure_equal(
            &addressed_store_state(temp.path(), &database),
            &AddressedStoreState::Populated {
                live_memories: NEARBY_STORE_THIN_LIVE_MEMORY_THRESHOLD,
            },
            "threshold-populated addressed-store count",
        )
    }

    #[test]
    fn addressed_store_state_counts_explicit_workspace_in_external_database() -> TestResult {
        let temp = orient_test_tempdir()?;
        let workspace = temp.path().join("recorded-workspace");
        let external_store = temp.path().join("external-stores");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&external_store).map_err(|error| error.to_string())?;
        let workspace = workspace
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let database = external_store.join("custom.db");
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = WorkspaceId::from_uuid(uuid::Uuid::from_u128(0x741)).to_string();
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("external custom database".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        for index in 1_u128..=2 {
            connection
                .insert_memory(
                    &MemoryId::from_uuid(uuid::Uuid::from_u128(0x741 + index)).to_string(),
                    &CreateMemoryInput {
                        workspace_id: workspace_id.clone(),
                        level: "procedural".to_owned(),
                        kind: "rule".to_owned(),
                        content: format!("External custom database fact {index}."),
                        workflow_id: None,
                        confidence: 0.9,
                        utility: 0.9,
                        importance: 0.9,
                        provenance_uri: None,
                        trust_class: "human_explicit".to_owned(),
                        trust_subclass: None,
                        tags: vec!["external-store".to_owned()],
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())?;
        }
        connection.close().map_err(|error| error.to_string())?;

        ensure_equal(
            &addressed_store_state(&workspace, &database),
            &AddressedStoreState::Thin {
                database,
                live_memories: 2,
            },
            "explicit workspace identity in external custom database",
        )
    }

    #[test]
    fn addressed_store_state_inspects_only_the_resolved_database() -> TestResult {
        let temp = orient_test_tempdir()?;
        remember_fixture(
            temp.path(),
            "Alternate campaign content must not mask an empty addressed default store.",
            "campaign",
            None,
        )?;
        std::fs::rename(temp.path().join(".ee"), temp.path().join(".ee-campaign"))
            .map_err(|error| error.to_string())?;

        let init_report = init_workspace(&InitOptions {
            workspace_path: temp.path().to_path_buf(),
            dry_run: false,
            repair_plan: false,
            force: false,
            allow_symlink: false,
            skip_boilerplate: true,
        });
        ensure(
            init_report.status.is_success(),
            format!(
                "initialize empty addressed default store failed: {:?}",
                init_report.action_errors
            ),
        )?;

        let database = resolved_addressed_database_path(temp.path());
        let expected = temp
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?
            .join(".ee")
            .join("ee.db");
        ensure_equal(&database, &expected, "resolved addressed default database")?;
        ensure_equal(
            &addressed_store_state(temp.path(), &database),
            &AddressedStoreState::Empty { database: expected },
            "only the resolved addressed database determines emptiness",
        )?;

        let scan = discover_local_stores(temp.path(), scan_budget());
        ensure_equal(
            &scan.stores.len(),
            &1_usize,
            "alternate campaign store at the addressed root remains discoverable",
        )?;
        ensure(
            scan.stores[0].store_dir.ends_with(".ee-campaign") && scan.stores[0].documents == 1,
            format!(
                "empty addressed .ee must report the populated sibling .ee-campaign: {:?}",
                scan.stores
            ),
        )
    }

    #[cfg(unix)]
    #[test]
    fn discovery_skips_permission_denied_children_without_error() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let temp = orient_test_tempdir()?;
        std::fs::create_dir_all(temp.path().join(".git")).map_err(|error| error.to_string())?;
        let open_child = temp.path().join("open");
        let locked = temp.path().join("locked");
        let hidden_store = locked.join("hidden-store");
        std::fs::create_dir_all(&open_child).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&hidden_store).map_err(|error| error.to_string())?;
        remember_fixture(&open_child, "Reachable store rule.", "nearby", None)?;
        remember_fixture(
            &hidden_store,
            "Permission-denied store must not surface.",
            "nearby",
            None,
        )?;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000))
            .map_err(|error| error.to_string())?;

        let permission_probe = std::fs::read_dir(&locked).map(drop);
        let scan = discover_local_stores(temp.path(), scan_budget());
        // Restore permissions so tempdir cleanup succeeds.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))
            .map_err(|error| error.to_string())?;
        ensure(
            matches!(
                permission_probe,
                Err(ref error) if error.kind() == std::io::ErrorKind::PermissionDenied
            ),
            format!("permission fixture did not produce PermissionDenied: {permission_probe:?}"),
        )?;
        ensure_equal(&scan.stores.len(), &1_usize, "reachable store still found")?;
        ensure(
            scan.stores[0].workspace_root.ends_with("open"),
            format!("wrong store surfaced: {:?}", scan.stores),
        )
    }

    #[cfg(unix)]
    #[test]
    fn discovery_does_not_follow_directory_symlinks_outside_workspace() -> TestResult {
        use std::os::unix::fs::symlink;

        let workspace = orient_test_tempdir()?;
        std::fs::create_dir_all(workspace.path().join(".git"))
            .map_err(|error| error.to_string())?;
        let outside = orient_test_tempdir()?;
        remember_fixture(
            outside.path(),
            "External store must not be discovered through a symlink.",
            "nearby",
            None,
        )?;
        symlink(outside.path(), workspace.path().join("linked-outside"))
            .map_err(|error| error.to_string())?;

        let scan = discover_local_stores(workspace.path(), scan_budget());
        ensure_equal(
            &scan.stores.len(),
            &0_usize,
            "symlinked external stores are outside the discovery boundary",
        )
    }
}
