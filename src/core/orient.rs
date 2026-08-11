//! Core helpers for agent orientation.
//!
//! Most `ee orient` assembly is currently performed in the CLI layer because
//! it composes several public commands. This module holds reusable read-only
//! data providers that should not be duplicated by the CLI renderer.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value as JsonValue, json};

use super::context::{
    ContextPackOptions, ContextPackOutputOptions, ContextPackOutputProfile,
    admit_recent_context_memories, run_context_pack,
};
use super::decide::{DecideItem, DecideRevisitOptions, decide_revisit};
use super::search::SearchSourceMode;
use crate::db::DbConnection;
use crate::models::{DomainError, GLOBAL_MEMORY_SCOPE_TAG, MemoryScope, RedactionLevel};
use crate::pack::{ContextPackProfile, PackResourceProfile, RenderedPackProvenance};
use crate::search::SpeedMode;

pub const ORIENT_DECISIONS_SCHEMA_V1: &str = "ee.orient.decisions.v1";
pub const ORIENT_FAST_CONTENT_SCHEMA_V1: &str = "ee.orient.fast_content.v1";
pub const ORIENT_FAST_CONTENT_LIMIT: usize = 5;

/// Maximum child-directory depth scanned by nearby-store discovery.
pub const NEARBY_STORE_CHILD_DEPTH: usize = 3;
/// Maximum nearby stores reported (ranked by document count).
pub const NEARBY_STORE_REPORT_LIMIT: usize = 5;
/// Default wall-clock budget for one discovery scan.
pub const NEARBY_STORE_SCAN_BUDGET_MS: u64 = 200;

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
    pub message: String,
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
                tags: admitted.tags,
                provenance: admitted.item.rendered_provenance(),
            })
            .collect(),
        Err(error) => {
            issues.push(OrientFastContentIssue {
                component: "recent",
                status: "unavailable",
                message: format!("Recent context admission was unavailable: {error}"),
            });
            Vec::new()
        }
    };

    let relevant = match run_context_pack(&pack_options) {
        Ok(response) => {
            match orient_fast_items_from_pack(&pack_options, response.data.pack.items) {
                Ok(items) => items,
                Err(message) => {
                    issues.push(OrientFastContentIssue {
                        component: "relevant",
                        status: "metadata_unavailable",
                        message,
                    });
                    Vec::new()
                }
            }
        }
        Err(error) => {
            issues.push(OrientFastContentIssue {
                component: "relevant",
                status: "unavailable",
                message: format!("Lexical context retrieval was unavailable: {error}"),
            });
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
            relevant: "context_pack_lexical_only_v1",
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

fn orient_fast_items_from_pack(
    options: &ContextPackOptions,
    items: Vec<crate::pack::PackDraftItem>,
) -> Result<Vec<OrientFastContentItem>, String> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let database_path = options
        .database_path
        .clone()
        .unwrap_or_else(|| options.workspace_path.join(".ee").join("ee.db"));
    let connection = DbConnection::open_file_read_only(&database_path)
        .map_err(|error| format!("Relevant memory metadata could not be opened: {error}"))?;
    let memory_ids = items
        .iter()
        .map(|item| item.memory_id.to_string())
        .collect::<Vec<_>>();
    let memory_refs = memory_ids.iter().map(String::as_str).collect::<Vec<_>>();
    let mut memories = connection
        .get_memories_batch(&memory_refs)
        .map_err(|error| format!("Relevant memory metadata could not be loaded: {error}"))?;
    let mut tags = connection
        .get_memory_tags_batch(&memory_refs)
        .map_err(|error| format!("Relevant memory tags could not be loaded: {error}"))?;

    let missing_ids = memory_ids
        .iter()
        .filter(|id| !memories.contains_key(id.as_str()))
        .map(String::as_str)
        .collect::<Vec<_>>();
    if !missing_ids.is_empty()
        && let Ok(paths) = crate::core::global_store::default_global_store_paths_from_env()
        && paths.database_path.is_file()
    {
        let global_connection =
            DbConnection::open_file_read_only(&paths.database_path).map_err(|error| {
                format!("Relevant global memory metadata could not be opened: {error}")
            })?;
        if global_connection.needs_migration().map_err(|error| {
            format!("Relevant global memory migration state could not be inspected: {error}")
        })? {
            return Err("Relevant global memory metadata requires a database migration".to_owned());
        }
        let global_memories =
            global_connection
                .get_memories_batch(&missing_ids)
                .map_err(|error| {
                    format!("Relevant global memory metadata could not be loaded: {error}")
                })?;
        let mut global_tags = global_connection
            .get_memory_tags_batch(&missing_ids)
            .map_err(|error| format!("Relevant global memory tags could not be loaded: {error}"))?;
        for id in global_memories.keys() {
            let item_tags = global_tags.entry(id.clone()).or_default();
            if !item_tags.iter().any(|tag| tag == GLOBAL_MEMORY_SCOPE_TAG) {
                item_tags.push(GLOBAL_MEMORY_SCOPE_TAG.to_owned());
            }
        }
        memories.extend(global_memories);
        tags.extend(global_tags);
    }

    let mut rendered = Vec::with_capacity(items.len());
    for item in items {
        // Secret-shaped pack items are never useful orientation output. The
        // pack path has already replaced their content, and dropping them here
        // also prevents their identifiers from leaking into fast output.
        if !item.redactions.is_empty() {
            continue;
        }
        let id = item.memory_id.to_string();
        let memory = memories.get(&id).ok_or_else(|| {
            format!("Relevant memory metadata was missing for admitted item {id}")
        })?;
        rendered.push(OrientFastContentItem {
            id: id.clone(),
            snippet: orient_fast_snippet(&item.content),
            created_at: memory.created_at.clone(),
            tags: tags.get(&id).cloned().unwrap_or_else(Vec::new),
            provenance: item.rendered_provenance(),
        });
    }
    Ok(rendered)
}

fn orient_fast_snippet(content: &str) -> String {
    const MAX_CHARS: usize = 480;
    let mut chars = content.chars();
    let mut snippet = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        snippet.push('…');
    }
    snippet
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
// populated stores, so the orientation output can point at them.

/// One discovered populated store near the addressed workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NearbyStore {
    /// Workspace root that owns the store (the directory containing `.ee`
    /// or `.ee-campaign`).
    pub workspace_root: String,
    /// The store directory itself.
    pub store_dir: String,
    /// Total memory rows in the store (all workspaces, tombstones included —
    /// a presence signal, not a curation metric).
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

/// Scan for populated stores near `workspace_path`.
///
/// Read-only and loud about nothing: unreadable directories and stores that
/// fail to open are skipped (permission-denied children must not fail
/// orientation). The addressed workspace's own store is excluded.
#[must_use]
pub fn discover_nearby_stores(
    workspace_path: &Path,
    budget: std::time::Duration,
) -> NearbyStoreScan {
    let started = std::time::Instant::now();
    let mut scan = NearbyStoreScan::default();
    let own_roots: Vec<std::path::PathBuf> = workspace_path
        .canonicalize()
        .map(|canonical| vec![canonical])
        .unwrap_or_else(|_| vec![workspace_path.to_path_buf()]);

    let mut candidates: Vec<std::path::PathBuf> = Vec::new();

    // (a) children, breadth-first, bounded depth.
    let mut frontier = vec![(workspace_path.to_path_buf(), 0_usize)];
    while let Some((dir, depth)) = frontier.pop() {
        if started.elapsed() >= budget {
            scan.truncated = true;
            break;
        }
        if depth > 0 {
            candidates.push(dir.clone());
        }
        if depth >= NEARBY_STORE_CHILD_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // A single very wide directory must not bypass the advertised
            // wall-clock bound while we enumerate it. Checking only between
            // directories lets one directory with millions of entries turn
            // this read-only recovery hint into an unbounded walk.
            if started.elapsed() >= budget {
                scan.truncated = true;
                frontier.clear();
                break;
            }
            let path = entry.path();
            // `Path::is_dir` follows symlinks. Discovery is intentionally
            // confined to the addressed workspace tree, so inspect the
            // directory entry itself and never traverse a symlinked tree.
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if NEARBY_STORE_SKIP_DIRS.contains(&name) || NEARBY_STORE_MARKERS.contains(&name) {
                continue;
            }
            frontier.push((path, depth + 1));
        }
    }

    // (b) parents up to (and including) the git root.
    let mut parent = if workspace_path.join(".git").exists() {
        None
    } else {
        workspace_path.parent()
    };
    while let Some(dir) = parent {
        if started.elapsed() >= budget {
            scan.truncated = true;
            break;
        }
        candidates.push(dir.to_path_buf());
        if dir.join(".git").exists() {
            break;
        }
        parent = dir.parent();
    }

    for candidate in candidates {
        if started.elapsed() >= budget {
            scan.truncated = true;
            break;
        }
        let canonical = candidate
            .canonicalize()
            .unwrap_or_else(|_| candidate.clone());
        if own_roots.contains(&canonical) {
            continue;
        }
        for marker in NEARBY_STORE_MARKERS {
            let store_dir = candidate.join(marker);
            let database = store_dir.join("ee.db");
            if !database.is_file() {
                continue;
            }
            let Some(profile) = nearby_store_profile(&database) else {
                continue;
            };
            let (documents, last_write) = profile;
            if documents == 0 {
                continue;
            }
            scan.stores.push(NearbyStore {
                workspace_root: candidate.display().to_string(),
                store_dir: store_dir.display().to_string(),
                documents,
                last_write,
            });
        }
    }

    scan.stores.sort_by(|left, right| {
        right
            .documents
            .cmp(&left.documents)
            .then_with(|| left.workspace_root.cmp(&right.workspace_root))
    });
    scan.stores.truncate(NEARBY_STORE_REPORT_LIMIT);
    scan
}

/// Read the source-of-truth memory row count from a store without opening it
/// for writes. `None` means the count could not be established and must never
/// be interpreted as an empty store.
#[must_use]
pub(crate) fn store_memory_row_count(database: &Path) -> Option<u64> {
    let connection = DbConnection::open_file_read_only(database).ok()?;
    let documents = connection.count_table_rows("memories").ok()?;
    u64::try_from(documents).ok()
}

/// Read `(memory rows, newest db/WAL mtime)` from a candidate store, skipping
/// quietly on any failure.
fn nearby_store_profile(database: &Path) -> Option<(u64, Option<String>)> {
    let documents = store_memory_row_count(database)?;
    let last_write = nearby_store_last_write(database);
    Some((documents, last_write))
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

    #[test]
    fn orient_fast_content_returns_admitted_recent_and_lexical_items() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
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
        for item in report.recent.iter().chain(&report.relevant) {
            if item.created_at.is_empty() || item.provenance.is_empty() {
                return Err(format!(
                    "item must bind created_at and provenance: {item:?}"
                ));
            }
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
        Ok(())
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

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
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
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;
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

    #[test]
    fn discovery_finds_populated_child_store() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir_all(temp.path().join(".git")).map_err(|error| error.to_string())?;
        let child = temp.path().join("campaign");
        std::fs::create_dir_all(&child).map_err(|error| error.to_string())?;
        remember_fixture(&child, "Nearby store rule one.", "nearby", None)?;

        let scan = discover_nearby_stores(temp.path(), scan_budget());
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
    fn discovery_last_write_uses_newer_wal_and_ignores_shm() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
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

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
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

        let scan = discover_nearby_stores(temp.path(), scan_budget());
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
    fn discovery_parent_scan_stops_at_nearest_git_root_and_not_above_workspace_git_root()
    -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let git_root = temp.path().join("nearest-git-root");
        let workspace = git_root.join("subdir").join("workspace");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        remember_fixture(temp.path(), "Store above Git root.", "nearby", None)?;
        remember_fixture(&git_root, "Store at nearest Git root.", "nearby", None)?;
        std::fs::create_dir_all(git_root.join(".git")).map_err(|error| error.to_string())?;

        let ancestor_scan = discover_nearby_stores(&workspace, scan_budget());
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
        let workspace_root_scan = discover_nearby_stores(&workspace, scan_budget());
        ensure_equal(
            &workspace_root_scan.stores.len(),
            &0_usize,
            "workspace Git root must not scan parents",
        )
    }

    #[test]
    fn discovery_zero_budget_truncates_before_scanning() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let child = temp.path().join("populated-child");
        std::fs::create_dir_all(&child).map_err(|error| error.to_string())?;
        remember_fixture(&child, "Unscanned store rule.", "nearby", None)?;

        let scan = discover_nearby_stores(temp.path(), std::time::Duration::ZERO);
        ensure(scan.truncated, "zero-budget scan must truncate".to_owned())?;
        ensure_equal(
            &scan.stores.len(),
            &0_usize,
            "zero-budget scan must not inspect candidates",
        )
    }

    #[test]
    fn discovery_excludes_own_store_and_empty_dirs() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir_all(temp.path().join(".git")).map_err(|error| error.to_string())?;
        // The addressed workspace has its own populated store: it must not
        // report itself, and an empty sibling dir contributes nothing.
        remember_fixture(temp.path(), "Own store rule.", "own", None)?;
        std::fs::create_dir_all(temp.path().join("empty_child"))
            .map_err(|error| error.to_string())?;

        let scan = discover_nearby_stores(temp.path(), scan_budget());
        ensure_equal(&scan.stores.len(), &0_usize, "no nearby stores reported")
    }

    #[cfg(unix)]
    #[test]
    fn discovery_skips_permission_denied_children_without_error() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir_all(temp.path().join(".git")).map_err(|error| error.to_string())?;
        let open_child = temp.path().join("open");
        let locked = temp.path().join("locked");
        std::fs::create_dir_all(&open_child).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&locked).map_err(|error| error.to_string())?;
        remember_fixture(&open_child, "Reachable store rule.", "nearby", None)?;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000))
            .map_err(|error| error.to_string())?;

        let scan = discover_nearby_stores(temp.path(), scan_budget());
        // Restore permissions so tempdir cleanup succeeds.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))
            .map_err(|error| error.to_string())?;
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

        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir_all(workspace.path().join(".git"))
            .map_err(|error| error.to_string())?;
        let outside = tempfile::tempdir().map_err(|error| error.to_string())?;
        remember_fixture(
            outside.path(),
            "External store must not be discovered through a symlink.",
            "nearby",
            None,
        )?;
        symlink(outside.path(), workspace.path().join("linked-outside"))
            .map_err(|error| error.to_string())?;

        let scan = discover_nearby_stores(workspace.path(), scan_budget());
        ensure_equal(
            &scan.stores.len(),
            &0_usize,
            "symlinked external stores are outside the discovery boundary",
        )
    }
}
