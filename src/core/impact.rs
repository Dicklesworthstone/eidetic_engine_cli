//! Read-only impact lookup for code and command surfaces.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::core::memory_scope::MemoryScopeContext;
use crate::core::search::{
    SearchDedupMode, SearchOptions, SearchReport, SearchSourceMode,
    run_search_with_read_connection, search_degraded_data_json,
};
use crate::db::{DbConnection, DbError, StoredMemory};
use crate::models::{
    CreateMemoryAnchorInput, MemoryAnchorKind, MemoryAnchorSource, MemoryScope, MemoryScopeStats,
    StoredMemoryAnchor, memory_tags_include_global_scope,
};
use crate::search::SpeedMode;

pub const IMPACT_SCHEMA_V1: &str = "ee.impact.v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImpactFallbackStatus {
    SkippedLimitFilled,
    Searched,
    Unavailable,
}

impl ImpactFallbackStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SkippedLimitFilled => "skipped_limit_filled",
            Self::Searched => "searched",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImpactSurfaceQuery {
    pub kind: MemoryAnchorKind,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImpactOptions {
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
    pub index_dir: Option<PathBuf>,
    pub surface: ImpactSurfaceQuery,
    pub limit: u32,
    pub speed: SpeedMode,
    pub source_mode: SearchSourceMode,
    pub strict_source_mode: bool,
    pub memory_scope: MemoryScope,
    pub strict_scope: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImpactResolvedSurface {
    pub schema: &'static str,
    pub kind: MemoryAnchorKind,
    pub anchor_value_hash: String,
    pub redacted_anchor_value: String,
}

#[derive(Clone, Debug)]
pub struct ImpactReport {
    pub schema: &'static str,
    pub surface: ImpactResolvedSurface,
    pub requested_limit: u32,
    pub results: Vec<ImpactResult>,
    pub exact_anchor_count: usize,
    pub fallback_count: usize,
    pub fallback_status: ImpactFallbackStatus,
    pub fallback_report: Option<SearchReport>,
    pub scope_stats: MemoryScopeStats,
    pub elapsed_ms: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImpactResult {
    pub rank: usize,
    pub memory_id: String,
    pub match_type: ImpactMatchType,
    pub score: f32,
    pub memory: ImpactMemorySummary,
    pub anchor: Option<ImpactAnchorSummary>,
    pub fallback_hit: Option<serde_json::Value>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImpactMatchType {
    ExactAnchor,
    SearchFallback,
}

impl ImpactMatchType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExactAnchor => "exact_anchor",
            Self::SearchFallback => "search_fallback",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImpactMemorySummary {
    pub level: String,
    pub kind: String,
    pub trust_class: String,
    pub trust_subclass: Option<String>,
    pub confidence: f32,
    pub utility: f32,
    pub importance: f32,
    pub content_preview: String,
    pub provenance_uri: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImpactAnchorSummary {
    pub kind: MemoryAnchorKind,
    pub anchor_value_hash: String,
    pub redacted_anchor_value: String,
    pub confidence: f32,
    pub source: String,
    pub freshness_state: String,
    pub generation: i64,
    pub captured_span_hash: String,
}

#[derive(Debug)]
pub enum ImpactError {
    InvalidSurface { kind: MemoryAnchorKind },
    Storage(DbError),
}

impl fmt::Display for ImpactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSurface { kind } => write!(
                formatter,
                "Surface value could not be normalized as a {} anchor.",
                kind.as_str()
            ),
            Self::Storage(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for ImpactError {}

impl From<DbError> for ImpactError {
    fn from(error: DbError) -> Self {
        Self::Storage(error)
    }
}

impl ImpactReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let fallback = self.fallback_report.as_ref();
        serde_json::json!({
            "schema": self.schema,
            "command": "impact",
            "surface": self.surface.data_json(),
            "request": {
                "limit": self.requested_limit,
            },
            "phases": {
                "exactAnchor": {
                    "status": "ok",
                    "resultCount": self.exact_anchor_count,
                },
                "searchFallback": {
                    "status": self.fallback_status.as_str(),
                    "resultCount": self.fallback_count,
                },
                "graphNeighbors": {
                    "status": "not_available",
                    "resultCount": 0,
                    "reason": "anchor_graph_projection_not_wired_for_impact_yet",
                },
            },
            "scopeStats": self.scope_stats.data_json(),
            "results": self.results.iter().map(ImpactResult::data_json).collect::<Vec<_>>(),
            "resultCount": self.results.len(),
            "elapsedMs": self.elapsed_ms,
            "fallbackSearch": fallback.map(|report| {
                serde_json::json!({
                    "status": report.status.as_str(),
                    "query": report.query,
                    "resultCount": report.results.len(),
                    "degraded": search_degraded_data_json("impact.search_fallback", &report.degraded),
                })
            }),
            "degraded": fallback
                .map(|report| search_degraded_data_json("impact.search_fallback", &report.degraded))
                .unwrap_or_default(),
        })
    }
}

impl ImpactResolvedSurface {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "kind": self.kind.as_str(),
            "anchorValueHash": self.anchor_value_hash,
            "redactedValue": self.redacted_anchor_value,
        })
    }
}

impl ImpactResult {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "rank": self.rank,
            "memoryId": self.memory_id,
            "matchType": self.match_type.as_str(),
            "score": self.score,
            "memory": self.memory.data_json(),
        });
        if let Some(object) = value.as_object_mut() {
            if let Some(anchor) = &self.anchor {
                object.insert("anchor".to_owned(), anchor.data_json());
            }
            if let Some(hit) = &self.fallback_hit {
                object.insert("fallbackHit".to_owned(), hit.clone());
            }
        }
        value
    }
}

impl ImpactMemorySummary {
    #[must_use]
    pub fn from_memory(memory: &StoredMemory) -> Self {
        Self {
            level: memory.level.clone(),
            kind: memory.kind.clone(),
            trust_class: memory.trust_class.clone(),
            trust_subclass: memory.trust_subclass.clone(),
            confidence: memory.confidence,
            utility: memory.utility,
            importance: memory.importance,
            content_preview: impact_content_preview(&memory.content),
            provenance_uri: memory.provenance_uri.clone(),
            created_at: memory.created_at.clone(),
            updated_at: memory.updated_at.clone(),
        }
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "level": self.level,
            "kind": self.kind,
            "trustClass": self.trust_class,
            "trustSubclass": self.trust_subclass,
            "confidence": self.confidence,
            "utility": self.utility,
            "importance": self.importance,
            "contentPreview": self.content_preview,
            "provenanceUri": self.provenance_uri,
            "createdAt": self.created_at,
            "updatedAt": self.updated_at,
        })
    }
}

impl ImpactAnchorSummary {
    #[must_use]
    pub fn from_anchor(anchor: &StoredMemoryAnchor) -> Self {
        Self {
            kind: anchor.anchor_kind,
            anchor_value_hash: anchor.anchor_value_hash.clone(),
            redacted_anchor_value: anchor.redacted_anchor_value.clone(),
            confidence: anchor.confidence,
            source: anchor.source.as_str().to_owned(),
            freshness_state: anchor.freshness_state.as_str().to_owned(),
            generation: anchor.generation,
            captured_span_hash: anchor.captured_span_hash.clone(),
        }
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": self.kind.as_str(),
            "anchorValueHash": self.anchor_value_hash,
            "redactedValue": self.redacted_anchor_value,
            "confidence": self.confidence,
            "source": self.source,
            "freshnessState": self.freshness_state,
            "generation": self.generation,
            "capturedSpanHash": self.captured_span_hash,
        })
    }
}

pub fn run_impact(options: &ImpactOptions) -> Result<ImpactReport, ImpactError> {
    let started = Instant::now();
    let workspace_root = default_workspace_root(&options.workspace_path);
    // Query surfaces consult the filesystem so a real workspace file anchors
    // even when it is outside the lexical repo-path allowlist (GH#14). This
    // keeps `ee impact --path` in agreement with `ee recall --path`, which
    // already accepts any workspace-relative path selector.
    let query_anchor = CreateMemoryAnchorInput::from_query_surface(
        "mem_impactquery000000000000000000",
        options.surface.kind,
        &options.surface.value,
        1.0,
        MemoryAnchorSource::Explicit,
        "impact.query",
        0,
        Some(workspace_root.as_path()),
    )
    .ok_or(ImpactError::InvalidSurface {
        kind: options.surface.kind,
    })?;
    let surface = ImpactResolvedSurface {
        schema: IMPACT_SCHEMA_V1,
        kind: query_anchor.anchor_kind,
        anchor_value_hash: query_anchor.anchor_value_hash.clone(),
        redacted_anchor_value: query_anchor.redacted_anchor_value.clone(),
    };

    let database_path = options
        .database_path
        .clone()
        .unwrap_or_else(|| default_workspace_database_path(&options.workspace_path));
    let connection = DbConnection::open_file(&database_path)?;
    let workspace_id = resolve_workspace_id(&connection, &workspace_root)?;
    let scope_context = MemoryScopeContext::for_workspace(
        &workspace_root,
        options.memory_scope,
        options.strict_scope,
    );
    let mut scope_stats = scope_context.stats();
    let exact_anchors = connection
        .query_memory_anchors(query_anchor.anchor_kind, &query_anchor.anchor_value_hash)?;
    let mut exact_results = exact_impact_results(
        &connection,
        &workspace_id,
        &scope_context,
        &mut scope_stats,
        &exact_anchors,
        options.limit,
    )?;
    let exact_anchor_count = exact_results.len();

    let mut fallback_report = None;
    let mut fallback_count = 0_usize;
    let mut fallback_status = ImpactFallbackStatus::SkippedLimitFilled;
    let mut seen_memory_ids: BTreeSet<String> = exact_results
        .iter()
        .map(|result| result.memory_id.clone())
        .collect();
    let limit = usize::try_from(options.limit).unwrap_or(usize::MAX);
    if exact_results.len() < limit {
        let fallback_limit = options.limit.saturating_sub(exact_results.len() as u32);
        let search_options = SearchOptions {
            workspace_path: workspace_root.clone(),
            database_path: options.database_path.clone(),
            index_dir: options.index_dir.clone(),
            query: options.surface.value.clone(),
            limit: fallback_limit.max(1),
            speed: options.speed,
            explain: false,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: None,
            dedup_mode: SearchDedupMode::DocId,
            source_mode: options.source_mode,
            strict_source_mode: options.strict_source_mode,
            memory_scope: options.memory_scope,
            strict_scope: options.strict_scope,
        };
        match run_search_with_read_connection(&search_options, &connection) {
            Ok(report) => {
                fallback_status = ImpactFallbackStatus::Searched;
                append_fallback_results(
                    &connection,
                    &report,
                    &mut seen_memory_ids,
                    limit,
                    &mut exact_results,
                    &mut fallback_count,
                )?;
                scope_stats.merge(&report.scope_stats);
                fallback_report = Some(report);
            }
            Err(_) => {
                fallback_status = ImpactFallbackStatus::Unavailable;
            }
        }
    }

    for (index, result) in exact_results.iter_mut().enumerate() {
        result.rank = index + 1;
    }

    Ok(ImpactReport {
        schema: IMPACT_SCHEMA_V1,
        surface,
        requested_limit: options.limit,
        results: exact_results,
        exact_anchor_count,
        fallback_count,
        fallback_status,
        fallback_report,
        scope_stats,
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
    })
}

fn exact_impact_results(
    connection: &DbConnection,
    workspace_id: &str,
    scope_context: &MemoryScopeContext,
    scope_stats: &mut MemoryScopeStats,
    anchors: &[StoredMemoryAnchor],
    limit: u32,
) -> Result<Vec<ImpactResult>, ImpactError> {
    let mut grouped = BTreeMap::<String, Vec<StoredMemoryAnchor>>::new();
    for anchor in anchors {
        grouped
            .entry(anchor.memory_id.clone())
            .or_default()
            .push(anchor.clone());
    }

    let mut results = Vec::new();
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    if limit == 0 {
        return Ok(results);
    }
    for (memory_id, mut memory_anchors) in grouped {
        let Some(memory) = connection.get_memory(&memory_id)? else {
            scope_stats.record_candidate_id(false, Some(&memory_id));
            continue;
        };
        if memory.tombstoned_at.is_some() {
            scope_stats.record_candidate_id(false, Some(&memory_id));
            continue;
        }
        let tags = connection.get_memory_tags(&memory_id)?;
        let workspace_candidate =
            memory.workspace_id == workspace_id || memory_tags_include_global_scope(&tags);
        let in_scope =
            workspace_candidate && scope_context.memory_in_scope_with_tags(&memory, &tags);
        scope_stats.record_candidate_id(in_scope, Some(&memory_id));
        if !in_scope {
            continue;
        }
        memory_anchors.sort_by(|left, right| {
            left.anchor_kind
                .cmp(&right.anchor_kind)
                .then_with(|| left.anchor_value_hash.cmp(&right.anchor_value_hash))
                .then_with(|| left.memory_id.cmp(&right.memory_id))
        });
        let anchor = memory_anchors.first().cloned();
        results.push(ImpactResult {
            rank: 0,
            memory_id,
            match_type: ImpactMatchType::ExactAnchor,
            score: anchor
                .as_ref()
                .map_or(1.0, |stored| stored.confidence.clamp(0.0, 1.0)),
            memory: ImpactMemorySummary::from_memory(&memory),
            anchor: anchor.as_ref().map(ImpactAnchorSummary::from_anchor),
            fallback_hit: None,
        });
        if results.len() >= limit {
            break;
        }
    }
    Ok(results)
}

fn append_fallback_results(
    connection: &DbConnection,
    report: &SearchReport,
    seen_memory_ids: &mut BTreeSet<String>,
    limit: usize,
    results: &mut Vec<ImpactResult>,
    fallback_count: &mut usize,
) -> Result<(), ImpactError> {
    let search_data = report.data_json();
    let rendered_hits = search_data
        .get("results")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let rendered_by_memory_id = rendered_hits
        .into_iter()
        .filter_map(|hit| {
            let memory_id = hit
                .get("memoryId")
                .or_else(|| hit.get("docId"))
                .and_then(serde_json::Value::as_str)?
                .to_owned();
            Some((memory_id, hit))
        })
        .collect::<BTreeMap<_, _>>();

    for hit in &report.results {
        if results.len() >= limit {
            break;
        }
        let memory_id = hit.doc_id.as_str();
        if !memory_id.starts_with("mem_") || !seen_memory_ids.insert(memory_id.to_owned()) {
            continue;
        }
        let Some(memory) = connection.get_memory(memory_id)? else {
            continue;
        };
        results.push(ImpactResult {
            rank: 0,
            memory_id: memory_id.to_owned(),
            match_type: ImpactMatchType::SearchFallback,
            score: hit.relevance_score(),
            memory: ImpactMemorySummary::from_memory(&memory),
            anchor: None,
            fallback_hit: rendered_by_memory_id.get(memory_id).cloned(),
        });
        *fallback_count = fallback_count.saturating_add(1);
    }
    Ok(())
}

fn default_workspace_root(workspace_path: &Path) -> PathBuf {
    crate::config::workspace::canonical_workspace_root_or_lexical(workspace_path)
}

fn default_workspace_database_path(workspace_path: &Path) -> PathBuf {
    default_workspace_root(workspace_path)
        .join(".ee")
        .join("ee.db")
}

fn resolve_workspace_id(
    connection: &DbConnection,
    workspace_path: &Path,
) -> Result<String, DbError> {
    let requested = crate::core::curate::stable_workspace_id(workspace_path);
    if let Ok(Some(workspace)) = crate::core::workspace::select_existing_workspace_row(
        connection,
        &requested,
        &[workspace_path],
    ) {
        return Ok(workspace.id);
    }
    Ok(connection
        .get_workspace_by_path(&workspace_path.to_string_lossy())?
        .map(|workspace| workspace.id)
        .unwrap_or(requested))
}

fn impact_content_preview(content: &str) -> String {
    const MAX_CHARS: usize = 240;
    let collapsed = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_CHARS {
        return collapsed;
    }
    let mut preview = collapsed.chars().take(MAX_CHARS).collect::<String>();
    preview.push('…');
    preview
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput};
    use crate::models::{MemoryAnchorKind, MemoryScope};

    type TestResult = Result<(), String>;

    fn seed_anchor_database(
        connection: &DbConnection,
        workspace: &Path,
    ) -> Result<(String, String), DbError> {
        connection.migrate()?;
        let workspace_id = crate::core::curate::stable_workspace_id(workspace);
        connection.insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: workspace.to_string_lossy().to_string(),
                name: Some("impact-test".to_owned()),
            },
        )?;
        let memory_id = "mem_30000000000000000000000001".to_owned();
        connection.insert_memory(
            &memory_id,
            &CreateMemoryInput {
                workspace_id: workspace_id.clone(),
                level: "procedural".to_owned(),
                kind: "rule".to_owned(),
                content: "Before editing `src/core/impact.rs`, run the anchor impact query."
                    .to_owned(),
                workflow_id: None,
                confidence: 0.9,
                utility: 0.8,
                importance: 0.7,
                provenance_uri: Some("test://impact".to_owned()),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: Some("test".to_owned()),
                tags: Vec::new(),
                valid_from: None,
                valid_to: None,
            },
        )?;
        Ok((workspace_id, memory_id))
    }

    #[test]
    fn impact_exact_anchor_results_are_first_and_redacted() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let db_path = workspace.join("impact.ee.db");
        let connection = DbConnection::open_file(&db_path).map_err(|error| error.to_string())?;
        let (_workspace_id, memory_id) =
            seed_anchor_database(&connection, &workspace).map_err(|error| error.to_string())?;

        let report = run_impact(&ImpactOptions {
            workspace_path: workspace,
            database_path: Some(db_path),
            index_dir: None,
            surface: ImpactSurfaceQuery {
                kind: MemoryAnchorKind::Path,
                value: "src/core/impact.rs".to_owned(),
            },
            limit: 1,
            speed: SpeedMode::Instant,
            source_mode: SearchSourceMode::LexicalOnly,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
        })
        .map_err(|error| error.to_string())?;

        assert_eq!(report.results.len(), 1);
        assert_eq!(report.results[0].memory_id, memory_id);
        assert_eq!(report.results[0].match_type, ImpactMatchType::ExactAnchor);
        assert_eq!(
            report.fallback_status,
            ImpactFallbackStatus::SkippedLimitFilled
        );
        let data = report.data_json();
        assert_eq!(data["schema"], IMPACT_SCHEMA_V1);
        assert_eq!(data["surface"]["kind"], "path");
        assert!(
            data["surface"]["anchorValueHash"]
                .as_str()
                .unwrap_or_default()
                .starts_with("blake3:")
        );
        assert_eq!(data["results"][0]["matchType"], "exact_anchor");
        let anchor_block = data["results"][0]["anchor"].to_string();
        assert!(!anchor_block.contains("src/core/impact.rs"));
        Ok(())
    }

    #[test]
    fn impact_zero_limit_returns_no_exact_anchor_results() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let db_path = workspace.join("impact.ee.db");
        let connection = DbConnection::open_file(&db_path).map_err(|error| error.to_string())?;
        seed_anchor_database(&connection, &workspace).map_err(|error| error.to_string())?;

        let report = run_impact(&ImpactOptions {
            workspace_path: workspace,
            database_path: Some(db_path),
            index_dir: None,
            surface: ImpactSurfaceQuery {
                kind: MemoryAnchorKind::Path,
                value: "src/core/impact.rs".to_owned(),
            },
            limit: 0,
            speed: SpeedMode::Instant,
            source_mode: SearchSourceMode::LexicalOnly,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
        })
        .map_err(|error| error.to_string())?;

        assert!(report.results.is_empty());
        assert_eq!(report.exact_anchor_count, 0);
        assert_eq!(
            report.fallback_status,
            ImpactFallbackStatus::SkippedLimitFilled
        );
        assert_eq!(report.data_json()["resultCount"], 0);
        Ok(())
    }

    #[test]
    fn impact_rejects_invalid_surface_values() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let report = run_impact(&ImpactOptions {
            workspace_path: tempdir.path().to_path_buf(),
            database_path: Some(tempdir.path().join("missing.ee.db")),
            index_dir: None,
            surface: ImpactSurfaceQuery {
                kind: MemoryAnchorKind::EnvVar,
                value: "not_an_ee_variable".to_owned(),
            },
            limit: 5,
            speed: SpeedMode::Instant,
            source_mode: SearchSourceMode::LexicalOnly,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
        });
        assert!(matches!(
            report,
            Err(ImpactError::InvalidSurface {
                kind: MemoryAnchorKind::EnvVar
            })
        ));
    }
}
