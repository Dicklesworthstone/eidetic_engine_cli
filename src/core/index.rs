use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicU8, AtomicU64, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::core::degraded_aggregation::{DegradationAggregationInput, aggregate_degraded_entries};
use crate::core::profile::{RuntimeProfileReport, runtime_profile_for_workspace};
use crate::db::{
    AcquireLockResult, AdvisoryLockId, CreateSearchIndexJobInput, DbConnection, DbError,
    DbOperation, ModelRegistryUpsertOutcome, SearchIndexJobType, StoredSearchIndexJob,
};
use crate::models::MemoryId;
use crate::models::model_registry::{
    EmbeddingMetadataRecord, EmbeddingPooling, ModelDistanceMetric, ModelProvider,
    ModelRegistryStatus,
};
use crate::models::{
    EMBEDDING_POSTURE_MODE_DETERMINISTIC_HASH, EMBEDDING_POSTURE_MODE_NEURAL_LOCAL,
    EMBEDDING_POSTURE_MODE_NEURAL_LOCAL_PENDING, EMBEDDING_POSTURE_SCHEMA_V1,
};
use crate::search::{
    CanonicalSearchDocument, EmbedderStack, HashEmbedder, IndexBuilder, artifact_to_document,
    memory_to_document_with_context_anchors_and_typed_fields, session_to_document,
};
#[cfg(feature = "lexical-bm25")]
use crate::search::{LexicalSearch, TantivyIndex};
use asupersync::sync::OnceCell as AsyncOnceCell;
use frankensearch::embed::{
    ConsentSource, DownloadConsent, DownloadProgress, ModelDownloader, ModelLifecycle,
    ModelManifest,
};
use frankensearch::{Model2VecEmbedder, ModelCategory, ModelTier, SearchError, VectorIndex};
use sqlmodel_core::Value as SqlValue;

pub const DEFAULT_INDEX_SUBDIR: &str = "index";
const INDEX_METADATA_FILE: &str = "meta.json";
const INDEX_STAGING_PREFIX: &str = ".publish-";
const INDEX_RETAINED_SUFFIX: &str = ".previous";
const VECTOR_INDEX_FAST_FILE: &str = "vector.fast.idx";
const VECTOR_INDEX_QUALITY_FILE: &str = "vector.quality.idx";
const VECTOR_INDEX_FALLBACK_FILE: &str = "vector.idx";
#[cfg(feature = "lexical-bm25")]
const LEXICAL_INDEX_SUBDIR: &str = "lexical";

/// Maximum bytes inspected when reading `<workspace>/.ee/index/meta.json`.
/// Real index metadata is a single tiny JSON object (`generation`,
/// `sourceGeneration`, `lastRebuildAt`, `lastCheckError` — well under 1 KiB in practice);
/// 4 MiB gives many orders of magnitude of headroom while still bounding
/// peer-planted oversize plants on shared multi-agent checkouts.
///
/// Without this cap, a peer-planted or accidentally-inflated meta.json
/// (corrupt write, `cat /dev/urandom > meta.json`, hostile multi-agent
/// checkout) would pin a matching allocation through `fs::read_to_string`
/// on every caller of `get_index_status` — and `get_index_status` fires
/// on at least three production paths:
///   - `ee index status` (`cli/mod.rs:16614`)
///   - `ee status` (`cli/mod.rs:30302`)
///   - `ee capabilities` via `IndexCapabilitySummary::gather`
///     (`core/capabilities.rs:193`)
///
/// Matches the cap and read-shape the parallel hardening pass applied to
/// `src/config/workspace.rs::detect_git_worktree` (c8f33694),
/// `src/core/preflight_guard.rs::read_preflight_rules_file_no_follow`
/// (7f56d89b), and the procedure verification source cap (131fd011).
const INDEX_METADATA_INSPECT_LIMIT: u64 = 4 * 1024 * 1024;
const READ_SURFACE_AUDIT_ACTIONS: [&str; 6] = [
    crate::db::audit_actions::SEARCH_EXECUTED,
    crate::db::audit_actions::SEARCH_RETURNED_MEM,
    crate::db::audit_actions::PACK_ASSEMBLED,
    crate::db::audit_actions::PACK_INCLUDED_MEM,
    crate::db::audit_actions::MEMORY_SHOW,
    crate::db::audit_actions::WHY_INSPECTED,
];

/// Lock TTL for index publish operations (5 minutes).
const INDEX_PUBLISH_LOCK_TTL_SECS: u64 = 300;
const INDEX_PUBLISH_LOCK_RETRY_ATTEMPTS: usize = 200;
pub const INDEX_PUBLISH_LOCK_CONTENTION_CODE: &str = "index_publish_lock_contention";
static INDEX_METADATA_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique holder ID for advisory locks.
fn generate_index_holder_id() -> String {
    let pid = std::process::id();
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("ee-index-{pid}-{ts}")
}

/// Acquire the index publish lock or return an error.
fn acquire_index_publish_lock(
    db: &DbConnection,
    workspace_id: &str,
    holder_id: &str,
) -> Result<(), IndexRebuildError> {
    acquire_index_publish_lock_with_retry(
        db,
        workspace_id,
        holder_id,
        index_publish_lock_retry_attempts(),
        index_publish_lock_retry_delay,
    )
}

fn index_publish_lock_retry_attempts() -> usize {
    crate::config::env_registry::read(
        crate::config::env_registry::EnvVar::IndexPublishLockRetryAttempts,
    )
    .and_then(|raw| raw.parse::<usize>().ok())
    .filter(|attempts| *attempts > 0)
    .unwrap_or(INDEX_PUBLISH_LOCK_RETRY_ATTEMPTS)
}

fn acquire_index_publish_lock_with_retry<F>(
    db: &DbConnection,
    workspace_id: &str,
    holder_id: &str,
    attempts: usize,
    retry_delay: F,
) -> Result<(), IndexRebuildError>
where
    F: Fn(usize) -> Duration,
{
    if let Err(error) = db.ensure_advisory_locks_table() {
        return Err(IndexRebuildError::Database(error));
    }

    let lock_id = AdvisoryLockId::index(workspace_id);
    let attempts = attempts.max(1);
    let mut waited = Duration::ZERO;
    let mut last_holder = None;
    for attempt in 0..attempts {
        match db.acquire_advisory_lock(
            &lock_id,
            holder_id,
            Some(INDEX_PUBLISH_LOCK_TTL_SECS),
            Some("index publish"),
        )? {
            AcquireLockResult::Acquired(_) | AcquireLockResult::Expired { .. } => return Ok(()),
            AcquireLockResult::AlreadyHeld {
                holder_id: other,
                acquired_at,
            } => {
                last_holder = Some((other.clone(), acquired_at.clone()));
                if attempt + 1 < attempts {
                    let delay = retry_delay(attempt);
                    waited += delay;
                    if (attempt + 1) % 10 == 0 {
                        tracing::info!(
                            target: "ee::index",
                            attempt = attempt + 1,
                            attempts,
                            holder_id = %other,
                            acquired_at = %acquired_at,
                            retry_delay_ms = delay.as_millis(),
                            waited_ms = duration_millis_saturating(waited),
                            "waiting for index publish lock"
                        );
                    }
                    if !delay.is_zero() {
                        crate::db::sleep_retry_delay_or_cancel(DbOperation::Execute, delay)
                            .map_err(IndexRebuildError::Database)?;
                    }
                }
            }
        }
    }

    let (other, acquired_at) = last_holder.unwrap_or_else(|| {
        (
            "<unknown holder>".to_owned(),
            "<unknown acquisition time>".to_owned(),
        )
    });
    Err(IndexRebuildError::LockContention(
        IndexPublishLockContention {
            lock_id: lock_id.canonical_key(),
            holder_id: other,
            acquired_at,
            attempts,
            waited_ms: duration_millis_saturating(waited),
        },
    ))
}

fn index_publish_lock_retry_delay(attempt: usize) -> Duration {
    const BASE_DELAY_MS: u64 = 5;
    const MAX_DELAY_MS: u64 = 50;

    let multiplier = 1_u64 << attempt.min(4);
    Duration::from_millis(BASE_DELAY_MS.saturating_mul(multiplier).min(MAX_DELAY_MS))
}

fn duration_millis_saturating(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Release the index publish lock (best-effort, errors are logged but not propagated).
fn release_index_publish_lock(db: &DbConnection, workspace_id: &str, holder_id: &str) {
    let lock_id = AdvisoryLockId::index(workspace_id);
    let _ = db.release_advisory_lock(&lock_id, holder_id);
}

#[derive(Clone, Debug)]
pub struct IndexRebuildOptions {
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
    pub index_dir: Option<PathBuf>,
    pub dry_run: bool,
}

impl IndexRebuildOptions {
    fn resolve_database_path(&self) -> PathBuf {
        self.database_path
            .clone()
            .unwrap_or_else(|| default_workspace_database_path(&self.workspace_path))
    }

    fn resolve_index_dir(&self) -> PathBuf {
        self.index_dir
            .clone()
            .unwrap_or_else(|| default_workspace_index_dir(&self.workspace_path))
    }
}

#[derive(Clone, Debug)]
pub struct IndexRebuildReport {
    pub status: IndexRebuildStatus,
    pub memories_indexed: u32,
    pub sessions_indexed: u32,
    pub artifacts_indexed: u32,
    pub documents_total: u32,
    pub index_dir: PathBuf,
    pub elapsed_ms: f64,
    pub dry_run: bool,
    pub errors: Vec<String>,
    pub runtime_profile: RuntimeProfileReport,
}

#[derive(Clone, Debug)]
pub struct IndexReembedOptions {
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
    pub index_dir: Option<PathBuf>,
    pub dry_run: bool,
}

impl IndexReembedOptions {
    fn resolve_database_path(&self) -> PathBuf {
        self.database_path
            .clone()
            .unwrap_or_else(|| default_workspace_database_path(&self.workspace_path))
    }

    fn resolve_index_dir(&self) -> PathBuf {
        self.index_dir
            .clone()
            .unwrap_or_else(|| default_workspace_index_dir(&self.workspace_path))
    }
}

#[derive(Clone, Debug)]
pub struct IndexReembedReport {
    pub status: IndexReembedStatus,
    pub job_id: Option<String>,
    pub job_status: String,
    pub job_type: String,
    pub document_source: Option<String>,
    pub embedding_scope: String,
    pub embedding: ReembedEmbeddingSummary,
    pub memories_indexed: u32,
    pub sessions_indexed: u32,
    pub artifacts_indexed: u32,
    pub documents_embedded: u32,
    pub documents_total: u32,
    pub index_dir: PathBuf,
    pub elapsed_ms: f64,
    pub dry_run: bool,
    pub idempotency_key: String,
    pub errors: Vec<String>,
    pub runtime_profile: RuntimeProfileReport,
}

#[derive(Clone, Debug)]
pub struct IndexProcessingOptions {
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
    pub index_dir: Option<PathBuf>,
    pub dry_run: bool,
    pub job_limit: Option<u32>,
}

impl IndexProcessingOptions {
    fn resolve_database_path(&self) -> PathBuf {
        self.database_path
            .clone()
            .unwrap_or_else(|| default_workspace_database_path(&self.workspace_path))
    }

    fn resolve_index_dir(&self) -> PathBuf {
        self.index_dir
            .clone()
            .unwrap_or_else(|| default_workspace_index_dir(&self.workspace_path))
    }
}

fn default_workspace_root(workspace_path: &Path) -> PathBuf {
    crate::config::workspace::canonical_workspace_root_or_lexical(workspace_path)
}

fn default_workspace_database_path(workspace_path: &Path) -> PathBuf {
    default_workspace_root(workspace_path)
        .join(".ee")
        .join("ee.db")
}

fn default_workspace_index_dir(workspace_path: &Path) -> PathBuf {
    default_workspace_root(workspace_path)
        .join(".ee")
        .join(DEFAULT_INDEX_SUBDIR)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexProcessingStatus {
    Success,
    DryRun,
    NoPendingJobs,
    PartialFailure,
    Failed,
}

impl IndexProcessingStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::DryRun => "dry_run",
            Self::NoPendingJobs => "no_pending_jobs",
            Self::PartialFailure => "partial_failure",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug)]
pub struct IndexProcessingJobReport {
    pub job_id: String,
    pub job_type: String,
    pub document_source: Option<String>,
    pub document_id: Option<String>,
    pub outcome: String,
    pub processing_mode: String,
    pub fallback_to_full: Option<String>,
    pub documents_total: u32,
    pub documents_indexed: u32,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct IndexProcessingReport {
    pub status: IndexProcessingStatus,
    pub workspace_id: String,
    pub database_path: PathBuf,
    pub index_dir: PathBuf,
    pub pending_jobs: u32,
    pub processed_jobs: u32,
    pub completed_jobs: u32,
    pub failed_jobs: u32,
    pub dry_run: bool,
    pub job_limit: Option<u32>,
    pub elapsed_ms: f64,
    pub jobs: Vec<IndexProcessingJobReport>,
    pub runtime_profile: RuntimeProfileReport,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReembedEmbeddingSummary {
    pub posture: EmbeddingPosture,
    pub fast_model_id: String,
    pub fast_dimension: usize,
    pub quality_model_id: Option<String>,
    pub quality_dimension: Option<usize>,
    pub deterministic: bool,
    pub semantic: bool,
    pub registered_model_count: usize,
    pub available_model_count: usize,
    pub selected_registry_model: Option<ReembedRegistryModelSummary>,
    pub source: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbeddingPosture {
    pub schema: &'static str,
    pub mode: &'static str,
    pub semantic: bool,
    pub source: String,
    pub fast_model_id: String,
    pub fast_dimension: usize,
    pub quality_model_id: Option<String>,
    pub quality_dimension: Option<usize>,
    pub deterministic: bool,
    pub registered_model_count: usize,
    pub available_model_count: usize,
    pub selected_registry_model: Option<EmbeddingPostureRegistryModel>,
    pub vector_coverage: EmbeddingVectorCoverage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbeddingPostureRegistryModel {
    pub id: String,
    pub provider: String,
    pub model_name: String,
    pub status: String,
    pub dimension: u32,
    pub deterministic: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EmbeddingVectorCoverage {
    pub embedded: usize,
    pub total: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReembedRegistryModelSummary {
    pub id: String,
    pub provider: String,
    pub model_name: String,
    pub status: String,
    pub dimension: u32,
    pub deterministic: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexRebuildStatus {
    Success,
    DryRun,
    NoDocuments,
    DatabaseError,
    IndexError,
}

impl IndexRebuildStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::DryRun => "dry_run",
            Self::NoDocuments => "no_documents",
            Self::DatabaseError => "database_error",
            Self::IndexError => "index_error",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexReembedStatus {
    Success,
    DryRun,
    NoDocuments,
    IndexError,
}

impl IndexReembedStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::DryRun => "dry_run",
            Self::NoDocuments => "no_documents",
            Self::IndexError => "index_error",
        }
    }
}

impl IndexRebuildReport {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = String::new();

        match self.status {
            IndexRebuildStatus::DryRun => {
                output.push_str("DRY RUN: Would rebuild search index\n\n");
            }
            IndexRebuildStatus::Success => {
                output.push_str("Search index rebuilt successfully\n\n");
            }
            IndexRebuildStatus::NoDocuments => {
                output.push_str("No documents to index\n\n");
            }
            IndexRebuildStatus::DatabaseError => {
                output.push_str("Database error during index rebuild\n\n");
            }
            IndexRebuildStatus::IndexError => {
                output.push_str("Index error during rebuild\n\n");
            }
        }

        output.push_str(&format!("  Memories: {}\n", self.memories_indexed));
        output.push_str(&format!("  Sessions: {}\n", self.sessions_indexed));
        output.push_str(&format!("  Artifacts: {}\n", self.artifacts_indexed));
        output.push_str(&format!("  Total documents: {}\n", self.documents_total));
        output.push_str(&format!(
            "  Index directory: {}\n",
            self.index_dir.display()
        ));
        output.push_str(&format!("  Elapsed: {:.1}ms\n", self.elapsed_ms));

        if !self.errors.is_empty() {
            output.push_str("\nErrors:\n");
            for error in &self.errors {
                output.push_str(&format!("  - {error}\n"));
            }
        }

        output
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": "index_rebuild",
            "status": self.status.as_str(),
            "memories_indexed": self.memories_indexed,
            "sessions_indexed": self.sessions_indexed,
            "artifacts_indexed": self.artifacts_indexed,
            "documents_total": self.documents_total,
            "index_dir": self.index_dir.to_string_lossy(),
            "elapsed_ms": self.elapsed_ms,
            "dry_run": self.dry_run,
            "profileRuntime": self.runtime_profile.data_json(),
            "errors": self.errors,
        })
    }
}

impl IndexReembedReport {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = String::new();

        match self.status {
            IndexReembedStatus::DryRun => {
                output.push_str("DRY RUN: Would re-embed search index\n\n");
            }
            IndexReembedStatus::Success => {
                output.push_str("Search index re-embedded successfully\n\n");
            }
            IndexReembedStatus::NoDocuments => {
                output.push_str("No documents to re-embed\n\n");
            }
            IndexReembedStatus::IndexError => {
                output.push_str("Index error during re-embedding\n\n");
            }
        }

        output.push_str(&format!("  Job: {}\n", self.job_status));
        if let Some(job_id) = &self.job_id {
            output.push_str(&format!("  Job ID: {job_id}\n"));
        }
        output.push_str(&format!(
            "  Fast embedder: {} ({} dimensions)\n",
            self.embedding.fast_model_id, self.embedding.fast_dimension
        ));
        if let Some(quality_id) = &self.embedding.quality_model_id {
            output.push_str(&format!(
                "  Quality embedder: {} ({} dimensions)\n",
                quality_id,
                self.embedding.quality_dimension.unwrap_or_default()
            ));
        }
        output.push_str(&format!("  Memories: {}\n", self.memories_indexed));
        output.push_str(&format!("  Sessions: {}\n", self.sessions_indexed));
        output.push_str(&format!("  Artifacts: {}\n", self.artifacts_indexed));
        output.push_str(&format!(
            "  Embedded documents: {}/{}\n",
            self.documents_embedded, self.documents_total
        ));
        output.push_str(&format!("  Total documents: {}\n", self.documents_total));
        output.push_str(&format!(
            "  Index directory: {}\n",
            self.index_dir.display()
        ));
        output.push_str(&format!("  Elapsed: {:.1}ms\n", self.elapsed_ms));

        if !self.errors.is_empty() {
            output.push_str("\nErrors:\n");
            for error in &self.errors {
                output.push_str(&format!("  - {error}\n"));
            }
        }

        output
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": "index_reembed",
            "status": self.status.as_str(),
            "job_id": self.job_id,
            "job_status": self.job_status,
            "job_type": self.job_type,
            "document_source": self.document_source,
            "embedding_scope": self.embedding_scope,
            "embedding": self.embedding.data_json(),
            "memories_indexed": self.memories_indexed,
            "sessions_indexed": self.sessions_indexed,
            "artifacts_indexed": self.artifacts_indexed,
            "documents_embedded": self.documents_embedded,
            "documents_total": self.documents_total,
            "index_dir": self.index_dir.to_string_lossy(),
            "elapsed_ms": self.elapsed_ms,
            "dry_run": self.dry_run,
            "idempotency_key": self.idempotency_key,
            "profileRuntime": self.runtime_profile.data_json(),
            "errors": self.errors,
        })
    }
}

impl ReembedEmbeddingSummary {
    #[must_use]
    pub(crate) fn from_posture(posture: EmbeddingPosture) -> Self {
        Self {
            fast_model_id: posture.fast_model_id.clone(),
            fast_dimension: posture.fast_dimension,
            quality_model_id: posture.quality_model_id.clone(),
            quality_dimension: posture.quality_dimension,
            deterministic: posture.deterministic,
            semantic: posture.semantic,
            registered_model_count: posture.registered_model_count,
            available_model_count: posture.available_model_count,
            selected_registry_model: posture
                .selected_registry_model
                .as_ref()
                .map(ReembedRegistryModelSummary::from_posture),
            source: posture.source.clone(),
            posture,
        }
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "posture": self.posture.data_json(),
            "fast_model_id": self.fast_model_id,
            "fast_dimension": self.fast_dimension,
            "quality_model_id": self.quality_model_id,
            "quality_dimension": self.quality_dimension,
            "deterministic": self.deterministic,
            "semantic": self.semantic,
            "registered_model_count": self.registered_model_count,
            "available_model_count": self.available_model_count,
            "selected_registry_model": self.selected_registry_model.as_ref().map(ReembedRegistryModelSummary::data_json),
            "source": self.source,
        })
    }

    #[must_use]
    pub fn documents_embedded(&self) -> u32 {
        u32::try_from(self.posture.vector_coverage.embedded).unwrap_or(u32::MAX)
    }
}

impl EmbeddingPosture {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "mode": self.mode,
            "semantic": self.semantic,
            "source": self.source,
            "fast_model_id": self.fast_model_id,
            "fast_dimension": self.fast_dimension,
            "quality_model_id": self.quality_model_id,
            "quality_dimension": self.quality_dimension,
            "deterministic": self.deterministic,
            "registered_model_count": self.registered_model_count,
            "available_model_count": self.available_model_count,
            "selected_registry_model": self
                .selected_registry_model
                .as_ref()
                .map(EmbeddingPostureRegistryModel::data_json),
            "vector_coverage": self.vector_coverage.data_json(),
        })
    }

    #[must_use]
    pub fn with_vector_coverage(mut self, vector_coverage: EmbeddingVectorCoverage) -> Self {
        self.vector_coverage = vector_coverage;
        self
    }

    #[must_use]
    pub fn semantic_pending(&self) -> bool {
        self.mode == EMBEDDING_POSTURE_MODE_NEURAL_LOCAL_PENDING
    }
}

impl EmbeddingPostureRegistryModel {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "provider": self.provider,
            "model_name": self.model_name,
            "status": self.status,
            "dimension": self.dimension,
            "deterministic": self.deterministic,
        })
    }
}

impl EmbeddingVectorCoverage {
    #[must_use]
    pub const fn new(embedded: usize, total: usize) -> Self {
        Self { embedded, total }
    }

    #[must_use]
    pub fn data_json(self) -> serde_json::Value {
        serde_json::json!({
            "embedded": self.embedded,
            "total": self.total,
        })
    }
}

impl ReembedRegistryModelSummary {
    fn from_posture(model: &EmbeddingPostureRegistryModel) -> Self {
        Self {
            id: model.id.clone(),
            provider: model.provider.clone(),
            model_name: model.model_name.clone(),
            status: model.status.clone(),
            dimension: model.dimension,
            deterministic: model.deterministic,
        }
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "provider": self.provider,
            "model_name": self.model_name,
            "status": self.status,
            "dimension": self.dimension,
            "deterministic": self.deterministic,
        })
    }
}

impl IndexProcessingJobReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "job_id": self.job_id,
            "job_type": self.job_type,
            "document_source": self.document_source,
            "document_id": self.document_id,
            "outcome": self.outcome,
            "processing_mode": self.processing_mode,
            "fallback_to_full": self.fallback_to_full,
            "documents_total": self.documents_total,
            "documents_indexed": self.documents_indexed,
            "error": self.error,
        })
    }
}

impl IndexProcessingReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": "index_process_jobs",
            "status": self.status.as_str(),
            "workspace_id": self.workspace_id,
            "database_path": self.database_path.to_string_lossy(),
            "index_dir": self.index_dir.to_string_lossy(),
            "pending_jobs": self.pending_jobs,
            "processed_jobs": self.processed_jobs,
            "completed_jobs": self.completed_jobs,
            "failed_jobs": self.failed_jobs,
            "dry_run": self.dry_run,
            "job_limit": self.job_limit,
            "elapsed_ms": self.elapsed_ms,
            "profileRuntime": self.runtime_profile.data_json(),
            "jobs": self
                .jobs
                .iter()
                .map(IndexProcessingJobReport::data_json)
                .collect::<Vec<_>>(),
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = String::new();
        match self.status {
            IndexProcessingStatus::DryRun => {
                output.push_str("DRY RUN: Would process search index jobs\n\n");
            }
            IndexProcessingStatus::Success => {
                output.push_str("Search index jobs processed successfully\n\n");
            }
            IndexProcessingStatus::NoPendingJobs => {
                output.push_str("No pending search index jobs\n\n");
            }
            IndexProcessingStatus::PartialFailure => {
                output.push_str("Search index jobs processed with failures\n\n");
            }
            IndexProcessingStatus::Failed => {
                output.push_str("Search index job processing failed\n\n");
            }
        }

        output.push_str(&format!("  Pending jobs: {}\n", self.pending_jobs));
        output.push_str(&format!("  Processed jobs: {}\n", self.processed_jobs));
        output.push_str(&format!("  Completed jobs: {}\n", self.completed_jobs));
        output.push_str(&format!("  Failed jobs: {}\n", self.failed_jobs));
        output.push_str(&format!(
            "  Index directory: {}\n",
            self.index_dir.display()
        ));
        output.push_str(&format!("  Elapsed: {:.1}ms\n", self.elapsed_ms));

        output
    }
}

#[derive(Debug)]
pub struct IndexPublishLockContention {
    pub lock_id: String,
    pub holder_id: String,
    pub acquired_at: String,
    pub attempts: usize,
    pub waited_ms: u64,
}

#[derive(Debug)]
pub enum IndexRebuildError {
    Database(DbError),
    Index(String),
    LockContention(IndexPublishLockContention),
    NoWorkspace,
}

impl IndexRebuildError {
    #[must_use]
    pub fn repair_hint(&self) -> Option<&str> {
        match self {
            Self::Database(_) => Some("ee doctor --fix-plan --json"),
            Self::Index(_) => Some("Check index directory permissions"),
            Self::LockContention(_) => Some(
                "Wait for the active index operation to finish, then retry. Use `ee index status --workspace . --json` to inspect index state.",
            ),
            Self::NoWorkspace => Some("ee init --workspace ."),
        }
    }

    #[must_use]
    pub const fn stable_code(&self) -> Option<&'static str> {
        match self {
            Self::LockContention(_) => Some(INDEX_PUBLISH_LOCK_CONTENTION_CODE),
            Self::Database(_) | Self::Index(_) | Self::NoWorkspace => None,
        }
    }
}

impl std::fmt::Display for IndexRebuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(e) => write!(f, "Database error: {e}"),
            Self::Index(e) => write!(f, "Index error: {e}"),
            Self::LockContention(contention) => write!(
                f,
                "index publish lock contention: lock {} held by {} since {}; exhausted {} attempts after {}ms",
                contention.lock_id,
                contention.holder_id,
                contention.acquired_at,
                contention.attempts,
                contention.waited_ms
            ),
            Self::NoWorkspace => write!(f, "No workspace found"),
        }
    }
}

impl std::error::Error for IndexRebuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(e) => Some(e),
            _ => None,
        }
    }
}

impl From<DbError> for IndexRebuildError {
    fn from(e: DbError) -> Self {
        Self::Database(e)
    }
}

pub fn rebuild_index(
    options: &IndexRebuildOptions,
) -> Result<IndexRebuildReport, IndexRebuildError> {
    let start = Instant::now();
    let database_path = options.resolve_database_path();
    let index_dir = options.resolve_index_dir();
    let runtime_profile = runtime_profile_for_workspace(&options.workspace_path);

    let db = DbConnection::open_file(&database_path)?;
    let workspace_id = get_default_workspace_id(&db)?;
    let (_, _, db_generation) = get_db_stats(&db)?;
    let source_generation = db
        .get_workspace_generation(&workspace_id)?
        .or(db_generation);

    let memories = db.list_memories_for_retrieval_with_global(&workspace_id, None, false)?;
    let sessions = db.list_sessions(&workspace_id)?;
    let artifacts = db.list_artifacts(&workspace_id, None)?;

    let memory_docs = memory_documents_with_anchors(&db, &memories)?;
    let session_docs: Vec<CanonicalSearchDocument> =
        sessions.iter().map(session_to_document).collect();
    let artifact_docs: Vec<CanonicalSearchDocument> =
        artifacts.iter().map(artifact_to_document).collect();

    let (memories_indexed, sessions_indexed, artifacts_indexed, documents_total) =
        checked_document_counts(memory_docs.len(), session_docs.len(), artifact_docs.len())?;

    if options.dry_run {
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        return Ok(IndexRebuildReport {
            status: IndexRebuildStatus::DryRun,
            memories_indexed,
            sessions_indexed,
            artifacts_indexed,
            documents_total,
            index_dir,
            elapsed_ms,
            dry_run: true,
            errors: Vec::new(),
            runtime_profile,
        });
    }

    if documents_total == 0 {
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        return Ok(IndexRebuildReport {
            status: IndexRebuildStatus::NoDocuments,
            memories_indexed: 0,
            sessions_indexed: 0,
            artifacts_indexed: 0,
            documents_total: 0,
            index_dir,
            elapsed_ms,
            dry_run: false,
            errors: Vec::new(),
            runtime_profile,
        });
    }

    // Acquire index publish lock to prevent concurrent publish races.
    let holder_id = generate_index_holder_id();
    acquire_index_publish_lock(&db, &workspace_id, &holder_id)?;

    let result = (|| -> Result<IndexRebuildReport, IndexRebuildError> {
        let _recovery_action = recover_interrupted_publish(&index_dir)?;
        let staging_dir = create_publish_staging_dir(&index_dir)?;

        let indexable_docs: Vec<_> = memory_docs
            .into_iter()
            .chain(session_docs)
            .chain(artifact_docs)
            .map(|doc| doc.into_indexable())
            .collect();

        let stack = default_embedder_stack();
        let registry_stack = stack.clone();

        let build_result = build_index_sync(&staging_dir, stack, indexable_docs);

        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

        match build_result {
            Ok(stats) => {
                ensure_active_embedding_registry_record(&db, &workspace_id, &registry_stack)?;
                let published_generation =
                    source_generation.unwrap_or_else(|| u64::from(documents_total));
                write_index_metadata(&staging_dir, published_generation, documents_total)?;
                publish_staged_index(&index_dir, &staging_dir)?;

                Ok(IndexRebuildReport {
                    status: IndexRebuildStatus::Success,
                    memories_indexed,
                    sessions_indexed,
                    artifacts_indexed,
                    documents_total,
                    index_dir,
                    elapsed_ms,
                    dry_run: false,
                    errors: stats
                        .errors
                        .iter()
                        .map(|(id, e)| format!("{id}: {e}"))
                        .collect(),
                    runtime_profile: runtime_profile.clone(),
                })
            }
            Err(e) => Ok(IndexRebuildReport {
                status: IndexRebuildStatus::IndexError,
                memories_indexed,
                sessions_indexed,
                artifacts_indexed,
                documents_total,
                index_dir,
                elapsed_ms,
                dry_run: false,
                errors: vec![e],
                runtime_profile: runtime_profile.clone(),
            }),
        }
    })();

    release_index_publish_lock(&db, &workspace_id, &holder_id);
    result
}

pub fn reembed_index(
    options: &IndexReembedOptions,
) -> Result<IndexReembedReport, IndexRebuildError> {
    let start = Instant::now();
    let database_path = options.resolve_database_path();
    let index_dir = options.resolve_index_dir();
    let runtime_profile = runtime_profile_for_workspace(&options.workspace_path);

    let db = DbConnection::open_file(&database_path)?;
    let workspace_id = get_default_workspace_id(&db)?;
    let (_, _, db_generation) = get_db_stats(&db)?;
    let source_generation = db
        .get_workspace_generation(&workspace_id)?
        .or(db_generation);

    let memories = db.list_memories_for_retrieval_with_global(&workspace_id, None, false)?;
    let sessions = db.list_sessions(&workspace_id)?;
    let artifacts = db.list_artifacts(&workspace_id, None)?;
    let stack = default_embedder_stack();

    let memory_docs = memory_documents_with_anchors(&db, &memories)?;
    let session_docs: Vec<CanonicalSearchDocument> =
        sessions.iter().map(session_to_document).collect();
    let artifact_docs: Vec<CanonicalSearchDocument> =
        artifacts.iter().map(artifact_to_document).collect();

    let (memories_indexed, sessions_indexed, artifacts_indexed, documents_total) =
        checked_document_counts(memory_docs.len(), session_docs.len(), artifact_docs.len())?;
    let current_vector_coverage =
        embedding_vector_coverage(&index_dir, documents_total, read_fast_vector_record_count);
    let embedding = reembed_embedding_summary(&db, &workspace_id, &stack, current_vector_coverage)?;
    let idempotency_key = reembed_idempotency_key(
        &workspace_id,
        &embedding.fast_model_id,
        embedding.quality_model_id.as_deref(),
        documents_total,
    );

    if options.dry_run {
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        let documents_embedded = embedding.documents_embedded();
        return Ok(IndexReembedReport {
            status: IndexReembedStatus::DryRun,
            job_id: None,
            job_status: "dry_run_not_queued".to_owned(),
            job_type: SearchIndexJobType::FullRebuild.as_str().to_owned(),
            document_source: None,
            embedding_scope: "all_documents".to_owned(),
            embedding,
            memories_indexed,
            sessions_indexed,
            artifacts_indexed,
            documents_embedded,
            documents_total,
            index_dir,
            elapsed_ms,
            dry_run: true,
            idempotency_key,
            errors: Vec::new(),
            runtime_profile,
        });
    }

    let job_id = generate_search_index_job_id();
    let job_input = CreateSearchIndexJobInput {
        workspace_id: workspace_id.clone(),
        job_type: SearchIndexJobType::FullRebuild,
        document_source: None,
        document_id: Some(embedding.fast_model_id.clone()),
        documents_total,
    };
    db.insert_search_index_job(&job_id, &job_input)?;
    if !db.start_search_index_job(&job_id)? {
        return Err(IndexRebuildError::Index(format!(
            "Failed to start re-embedding job {job_id}"
        )));
    }

    if documents_total == 0 {
        db.complete_search_index_job(&job_id, 0)?;
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        return Ok(IndexReembedReport {
            status: IndexReembedStatus::NoDocuments,
            job_id: Some(job_id),
            job_status: "completed".to_owned(),
            job_type: SearchIndexJobType::FullRebuild.as_str().to_owned(),
            document_source: None,
            embedding_scope: "all_documents".to_owned(),
            embedding,
            memories_indexed: 0,
            sessions_indexed: 0,
            artifacts_indexed: 0,
            documents_embedded: 0,
            documents_total: 0,
            index_dir,
            elapsed_ms,
            dry_run: false,
            idempotency_key,
            errors: Vec::new(),
            runtime_profile,
        });
    }

    // Acquire index publish lock to prevent concurrent publish races.
    let holder_id = generate_index_holder_id();
    acquire_index_publish_lock(&db, &workspace_id, &holder_id)?;

    let result = (|| -> Result<IndexReembedReport, IndexRebuildError> {
        let _recovery_action = recover_interrupted_publish(&index_dir)?;
        let staging_dir = create_publish_staging_dir(&index_dir)?;
        let indexable_docs: Vec<_> = memory_docs
            .into_iter()
            .chain(session_docs)
            .chain(artifact_docs)
            .map(|doc| doc.into_indexable())
            .collect();

        let registry_stack = stack.clone();
        let build_result = build_index_sync(&staging_dir, stack, indexable_docs);
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

        match build_result {
            Ok(stats) => {
                ensure_active_embedding_registry_record(&db, &workspace_id, &registry_stack)?;
                db.update_search_index_job_progress(&job_id, documents_total)?;
                let published_generation =
                    source_generation.unwrap_or_else(|| u64::from(documents_total));
                write_index_metadata(&staging_dir, published_generation, documents_total)
                    .and_then(|()| publish_staged_index(&index_dir, &staging_dir))?;
                db.complete_search_index_job(&job_id, documents_total)?;
                let published_coverage = EmbeddingVectorCoverage::new(
                    usize::try_from(documents_total).unwrap_or(usize::MAX),
                    usize::try_from(documents_total).unwrap_or(usize::MAX),
                );
                let published_embedding = ReembedEmbeddingSummary::from_posture(
                    embedding
                        .posture
                        .clone()
                        .with_vector_coverage(published_coverage),
                );
                let documents_embedded = published_embedding.documents_embedded();

                Ok(IndexReembedReport {
                    status: IndexReembedStatus::Success,
                    job_id: Some(job_id),
                    job_status: "completed".to_owned(),
                    job_type: SearchIndexJobType::FullRebuild.as_str().to_owned(),
                    document_source: None,
                    embedding_scope: "all_documents".to_owned(),
                    embedding: published_embedding,
                    memories_indexed,
                    sessions_indexed,
                    artifacts_indexed,
                    documents_embedded,
                    documents_total,
                    index_dir,
                    elapsed_ms,
                    dry_run: false,
                    idempotency_key,
                    errors: stats
                        .errors
                        .iter()
                        .map(|(id, e)| format!("{id}: {e}"))
                        .collect(),
                    runtime_profile: runtime_profile.clone(),
                })
            }
            Err(error) => {
                let primary_error = error;
                let mut errors = vec![primary_error.clone()];
                if let Err(fail_error) = db.fail_search_index_job(&job_id, &primary_error) {
                    errors.push(format!(
                        "failed to mark re-embedding job failed: {fail_error}"
                    ));
                }
                let documents_embedded = embedding.documents_embedded();

                Ok(IndexReembedReport {
                    status: IndexReembedStatus::IndexError,
                    job_id: Some(job_id),
                    job_status: "failed".to_owned(),
                    job_type: SearchIndexJobType::FullRebuild.as_str().to_owned(),
                    document_source: None,
                    embedding_scope: "all_documents".to_owned(),
                    embedding,
                    memories_indexed,
                    sessions_indexed,
                    artifacts_indexed,
                    documents_embedded,
                    documents_total,
                    index_dir,
                    elapsed_ms,
                    dry_run: false,
                    idempotency_key,
                    errors,
                    runtime_profile: runtime_profile.clone(),
                })
            }
        }
    })();

    release_index_publish_lock(&db, &workspace_id, &holder_id);
    result
}

pub fn process_index_jobs(
    options: &IndexProcessingOptions,
) -> Result<IndexProcessingReport, IndexRebuildError> {
    let start = Instant::now();
    let database_path = options.resolve_database_path();
    let index_dir = options.resolve_index_dir();
    let runtime_profile = runtime_profile_for_workspace(&options.workspace_path);
    let (effective_job_limit, _job_limit_capped) =
        runtime_profile.cap_index_job_limit(options.job_limit);

    let db = DbConnection::open_file(&database_path)?;
    let workspace_id = get_default_workspace_id(&db)?;
    let pending_jobs = db.list_pending_search_index_jobs(&workspace_id, effective_job_limit)?;
    let pending_count = u32::try_from(pending_jobs.len()).map_err(|_| {
        IndexRebuildError::Index("Pending search index job count exceeds u32".to_owned())
    })?;

    if options.dry_run {
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        let jobs = pending_jobs
            .iter()
            .map(|job| IndexProcessingJobReport {
                job_id: job.id.clone(),
                job_type: job.job_type.clone(),
                document_source: job.document_source.clone(),
                document_id: job.document_id.clone(),
                outcome: "planned".to_owned(),
                processing_mode: processing_mode_for_job(job).to_owned(),
                fallback_to_full: None,
                documents_total: job.documents_total,
                documents_indexed: job.documents_indexed,
                error: None,
            })
            .collect();
        return Ok(IndexProcessingReport {
            status: IndexProcessingStatus::DryRun,
            workspace_id,
            database_path,
            index_dir,
            pending_jobs: pending_count,
            processed_jobs: 0,
            completed_jobs: 0,
            failed_jobs: 0,
            dry_run: true,
            job_limit: effective_job_limit,
            elapsed_ms,
            jobs,
            runtime_profile,
        });
    }

    if pending_jobs.is_empty() {
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        return Ok(IndexProcessingReport {
            status: IndexProcessingStatus::NoPendingJobs,
            workspace_id,
            database_path,
            index_dir,
            pending_jobs: 0,
            processed_jobs: 0,
            completed_jobs: 0,
            failed_jobs: 0,
            dry_run: false,
            job_limit: effective_job_limit,
            elapsed_ms,
            jobs: Vec::new(),
            runtime_profile,
        });
    }

    let mut jobs = Vec::with_capacity(pending_jobs.len());
    let mut completed_jobs = 0_u32;
    let mut failed_jobs = 0_u32;

    for job in pending_jobs {
        let result = process_one_index_job(&db, &job, &index_dir)?;
        if result.outcome == "failed" {
            failed_jobs = failed_jobs.saturating_add(1);
        } else {
            completed_jobs = completed_jobs.saturating_add(1);
        }
        jobs.push(result);
    }

    let processed_jobs = completed_jobs.saturating_add(failed_jobs);
    let status = match (completed_jobs, failed_jobs) {
        (_, 0) => IndexProcessingStatus::Success,
        (0, _) => IndexProcessingStatus::Failed,
        _ => IndexProcessingStatus::PartialFailure,
    };
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    Ok(IndexProcessingReport {
        status,
        workspace_id,
        database_path,
        index_dir,
        pending_jobs: pending_count,
        processed_jobs,
        completed_jobs,
        failed_jobs,
        dry_run: false,
        job_limit: effective_job_limit,
        elapsed_ms,
        jobs,
        runtime_profile,
    })
}

/// Drain every pending search-index job for one workspace with a SINGLE
/// index rebuild (bd-2efx1). Every job type publishes as a full rebuild
/// of the workspace's indexable documents, so N enqueued jobs are all
/// satisfied by one rebuild executed after the last enqueuing write —
/// the batch-remember lane uses this instead of paying one full rebuild
/// per line. Claimed jobs complete (or fail) together; jobs another
/// worker claimed mid-drain are reported as skipped.
pub(crate) fn process_pending_index_jobs_coalesced(
    db: &DbConnection,
    workspace_id: &str,
    index_dir: &Path,
    job_limit: Option<u32>,
) -> Result<Vec<IndexProcessingJobReport>, IndexRebuildError> {
    const COALESCED_MODE: &str = "coalesced_full_rebuild";
    // bd-d67os.7: mode reported when the coalesced batch applied its K touched
    // docs as a single incremental merge step instead of a full rebuild.
    const COALESCED_INCREMENTAL_MODE: &str = "coalesced_incremental";
    let pending = db.list_pending_search_index_jobs(workspace_id, job_limit)?;
    let mut claimed = Vec::new();
    let mut reports = Vec::new();
    for job in pending {
        if db.start_search_index_job(&job.id)? {
            claimed.push(job);
        } else {
            reports.push(IndexProcessingJobReport {
                job_id: job.id.clone(),
                job_type: job.job_type.clone(),
                document_source: job.document_source.clone(),
                document_id: job.document_id.clone(),
                outcome: "skipped".to_owned(),
                processing_mode: COALESCED_MODE.to_owned(),
                fallback_to_full: None,
                documents_total: job.documents_total,
                documents_indexed: job.documents_indexed,
                error: Some("search index job was not pending".to_owned()),
            });
        }
    }
    if claimed.is_empty() {
        return Ok(reports);
    }

    let (_memories_indexed, _sessions_indexed, documents_total, indexable_docs) =
        collect_workspace_indexable_documents(db, workspace_id)?;
    for job in &claimed {
        db.update_search_index_job_total(&job.id, documents_total)?;
    }

    if documents_total == 0 {
        for job in &claimed {
            db.complete_search_index_job(&job.id, 0)?;
            reports.push(IndexProcessingJobReport {
                job_id: job.id.clone(),
                job_type: job.job_type.clone(),
                document_source: job.document_source.clone(),
                document_id: job.document_id.clone(),
                outcome: "completed_no_documents".to_owned(),
                processing_mode: COALESCED_MODE.to_owned(),
                fallback_to_full: None,
                documents_total: 0,
                documents_indexed: 0,
                error: None,
            });
        }
        return Ok(reports);
    }

    let published_generation = db
        .get_workspace_generation(workspace_id)?
        .or(get_db_stats(db)?.2)
        .unwrap_or_else(|| u64::from(documents_total));

    // bd-d67os.7: when every coalesced job is a simple single-document upsert to
    // a doc that is currently present, and the index already exists, apply the K
    // touched docs as ONE incremental merge step instead of a full rebuild — this
    // is where group-commit (Track B) and incremental intake (Track C) compound.
    // Any FullRebuild job, deleted/missing target, absent index, or incremental
    // error falls back to the proven full-rebuild path, so the published index
    // state is always correct; only the all-upsert case takes the merge path.
    let incremental_batch = coalesced_incremental_batch(&claimed, &indexable_docs);
    let mut processing_mode = COALESCED_MODE.to_owned();
    let mut fallback_to_full = None;
    let holder_id = generate_index_holder_id();
    acquire_index_publish_lock(db, workspace_id, &holder_id)?;
    let incremental_outcome = match incremental_batch {
        Some(documents) => apply_incremental_index_batch_sync(
            index_dir,
            default_embedder_stack(),
            documents,
            published_generation,
            documents_total,
        ),
        None => IncrementalApplyOutcome::FullRebuildRequired,
    };
    let full_rebuild_required = match incremental_outcome {
        IncrementalApplyOutcome::Applied { .. } => {
            processing_mode = COALESCED_INCREMENTAL_MODE.to_owned();
            false
        }
        IncrementalApplyOutcome::Fallback { reason, detail } => {
            tracing::info!(
                target: "ee::index",
                workspace_id = %workspace_id,
                claimed_jobs = claimed.len(),
                fallback_to_full = reason.as_str(),
                detail = %detail,
                "coalesced incremental index intake fell back to full rebuild"
            );
            processing_mode.push_str("_fallback_to_full");
            fallback_to_full = Some(reason.as_str().to_owned());
            true
        }
        IncrementalApplyOutcome::FullRebuildRequired => true,
    };
    let build_result = if full_rebuild_required {
        (|| -> Result<(), String> {
            let _recovery_action =
                recover_interrupted_publish(index_dir).map_err(|error| error.to_string())?;
            let staging_dir =
                create_publish_staging_dir(index_dir).map_err(|error| error.to_string())?;
            build_index_sync(&staging_dir, default_embedder_stack(), indexable_docs).and_then(
                |_stats| {
                    write_index_metadata(&staging_dir, published_generation, documents_total)
                        .and_then(|()| publish_staged_index(index_dir, &staging_dir))
                        .map_err(|error| error.to_string())
                },
            )
        })()
    } else {
        Ok(())
    };
    release_index_publish_lock(db, workspace_id, &holder_id);

    match build_result {
        Ok(()) => {
            for job in &claimed {
                db.update_search_index_job_progress(&job.id, documents_total)?;
                db.complete_search_index_job(&job.id, documents_total)?;
                reports.push(IndexProcessingJobReport {
                    job_id: job.id.clone(),
                    job_type: job.job_type.clone(),
                    document_source: job.document_source.clone(),
                    document_id: job.document_id.clone(),
                    outcome: "completed".to_owned(),
                    processing_mode: processing_mode.clone(),
                    fallback_to_full: fallback_to_full.clone(),
                    documents_total,
                    documents_indexed: documents_total,
                    error: None,
                });
            }
        }
        Err(error) => {
            for job in &claimed {
                let mut error_message = error.clone();
                if let Err(fail_error) = db.fail_search_index_job(&job.id, &error_message) {
                    error_message.push_str("; failed to mark search index job failed: ");
                    error_message.push_str(&fail_error.to_string());
                }
                reports.push(IndexProcessingJobReport {
                    job_id: job.id.clone(),
                    job_type: job.job_type.clone(),
                    document_source: job.document_source.clone(),
                    document_id: job.document_id.clone(),
                    outcome: "failed".to_owned(),
                    processing_mode: processing_mode.clone(),
                    fallback_to_full: fallback_to_full.clone(),
                    documents_total,
                    documents_indexed: 0,
                    error: Some(error_message),
                });
            }
        }
    }
    Ok(reports)
}

pub(crate) fn process_index_job_for_connection(
    db: &DbConnection,
    job_id: &str,
    index_dir: &Path,
) -> Result<IndexProcessingJobReport, IndexRebuildError> {
    let job = db.get_search_index_job(job_id)?.ok_or_else(|| {
        IndexRebuildError::Index(format!("Search index job {job_id} was not found"))
    })?;
    process_one_index_job(db, &job, index_dir)
}

fn process_one_index_job(
    db: &DbConnection,
    job: &StoredSearchIndexJob,
    index_dir: &Path,
) -> Result<IndexProcessingJobReport, IndexRebuildError> {
    let mut processing_mode = processing_mode_for_job(job).to_owned();
    if !db.start_search_index_job(&job.id)? {
        return Ok(IndexProcessingJobReport {
            job_id: job.id.clone(),
            job_type: job.job_type.clone(),
            document_source: job.document_source.clone(),
            document_id: job.document_id.clone(),
            outcome: "skipped".to_owned(),
            processing_mode,
            fallback_to_full: None,
            documents_total: job.documents_total,
            documents_indexed: job.documents_indexed,
            error: Some("search index job was not pending".to_owned()),
        });
    }

    let (_memories_indexed, _sessions_indexed, documents_total, indexable_docs) =
        collect_workspace_indexable_documents(db, &job.workspace_id)?;
    db.update_search_index_job_total(&job.id, documents_total)?;

    let incremental_target = incremental_document_id_for_job(job);
    let incremental_document = incremental_target.and_then(|document_id| {
        indexable_docs
            .iter()
            .find(|document| document.id == document_id)
            .cloned()
    });
    // bd-2qmvp: the single-document incremental path upserts only its own
    // document yet stamps the current MAX workspace generation (see
    // `published_generation` below). Under concurrent single-document writes a
    // sibling memory may already be committed when this job runs — its index
    // job is enqueued in the SAME transaction as the memory write and the
    // (audit-derived) generation bump, so a committed sibling is always visible
    // here as a pending job (race-free detection). An incremental apply would
    // then publish a current-generation-but-INCOMPLETE index, and a concurrent
    // `ee search` would read index_gen == db_gen, see `Ready`, and silently
    // miss the sibling document (the bd-d67os.6 regression). When other index
    // jobs are still pending for this workspace we rebuild the COMPLETE
    // indexable set instead, so the published generation truthfully reflects
    // every committed document. The coalesced batch path already applies all
    // touched documents together and is unaffected.
    let sibling_index_jobs_pending = db
        .list_pending_search_index_jobs(&job.workspace_id, None)?
        .iter()
        .any(|pending| pending.id != job.id);
    let job_is_single_document = matches!(
        job.job_type_enum(),
        Some(SearchIndexJobType::Incremental | SearchIndexJobType::SingleDocument)
    );
    if job_is_single_document && sibling_index_jobs_pending {
        processing_mode.push_str("_sibling_pending_full_rebuild");
    }
    let should_try_incremental = job_is_single_document && !sibling_index_jobs_pending;

    if documents_total == 0 && (!should_try_incremental || incremental_target.is_none()) {
        db.complete_search_index_job(&job.id, 0)?;
        return Ok(IndexProcessingJobReport {
            job_id: job.id.clone(),
            job_type: job.job_type.clone(),
            document_source: job.document_source.clone(),
            document_id: job.document_id.clone(),
            outcome: "completed_no_documents".to_owned(),
            processing_mode,
            fallback_to_full: None,
            documents_total: 0,
            documents_indexed: 0,
            error: None,
        });
    }

    // Publish the index at the database generation (the audit-inclusive
    // max of source-document and audited-mutation counts), matching the
    // full-rebuild path at write_index_metadata above. Writing only
    // `documents_total` here left the incremental index one generation
    // behind db_generation after `ee remember` wrote its audit rows, which
    // falsely tripped `search_index_stale` on the very next search even
    // though the job had already applied synchronously. (agent-UX item 5)
    let published_generation = db
        .get_workspace_generation(&job.workspace_id)?
        .or(get_db_stats(db)?.2)
        .unwrap_or_else(|| u64::from(documents_total));

    // Acquire index publish lock to prevent concurrent publish races.
    let holder_id = generate_index_holder_id();
    acquire_index_publish_lock(db, &job.workspace_id, &holder_id)?;

    let result = (|| -> Result<IndexProcessingJobReport, IndexRebuildError> {
        let _recovery_action = recover_interrupted_publish(index_dir)?;
        let incremental_outcome = if should_try_incremental {
            match incremental_target {
                Some(document_id) => apply_incremental_index_change_sync(
                    index_dir,
                    default_embedder_stack(),
                    document_id,
                    incremental_document.clone(),
                    published_generation,
                    documents_total,
                ),
                None => IncrementalApplyOutcome::Fallback {
                    reason: missing_incremental_target_reason(job),
                    detail: "incremental search index job did not include a document_id".to_owned(),
                },
            }
        } else {
            IncrementalApplyOutcome::FullRebuildRequired
        };

        let fallback_to_full = match incremental_outcome {
            IncrementalApplyOutcome::Applied { documents_indexed } => {
                db.update_search_index_job_progress(&job.id, documents_indexed)?;
                db.complete_search_index_job(&job.id, documents_indexed)?;
                return Ok(IndexProcessingJobReport {
                    job_id: job.id.clone(),
                    job_type: job.job_type.clone(),
                    document_source: job.document_source.clone(),
                    document_id: job.document_id.clone(),
                    outcome: "completed".to_owned(),
                    processing_mode,
                    fallback_to_full: None,
                    documents_total,
                    documents_indexed,
                    error: None,
                });
            }
            IncrementalApplyOutcome::Fallback { reason, detail } => {
                tracing::info!(
                    target: "ee::index",
                    job_id = %job.id,
                    job_type = %job.job_type,
                    document_source = ?job.document_source,
                    document_id = ?job.document_id,
                    fallback_to_full = reason.as_str(),
                    detail = %detail,
                    "incremental index intake fell back to full rebuild"
                );
                processing_mode.push_str("_fallback_to_full");
                if documents_total == 0 && reason == IncrementalFallbackReason::IndexAbsent {
                    db.complete_search_index_job(&job.id, 0)?;
                    return Ok(IndexProcessingJobReport {
                        job_id: job.id.clone(),
                        job_type: job.job_type.clone(),
                        document_source: job.document_source.clone(),
                        document_id: job.document_id.clone(),
                        outcome: "completed_no_documents".to_owned(),
                        processing_mode,
                        fallback_to_full: Some(reason.as_str().to_owned()),
                        documents_total: 0,
                        documents_indexed: 0,
                        error: None,
                    });
                }
                Some(reason.as_str().to_owned())
            }
            IncrementalApplyOutcome::FullRebuildRequired => None,
        };

        let build_result = publish_full_index_generation(
            index_dir,
            indexable_docs,
            published_generation,
            documents_total,
        );

        match build_result {
            Ok(stats) => {
                db.update_search_index_job_progress(&job.id, documents_total)?;
                db.complete_search_index_job(&job.id, documents_total)?;
                let mut errors = stats
                    .errors
                    .iter()
                    .map(|(id, error)| format!("{id}: {error}"))
                    .collect::<Vec<_>>();
                errors.sort();
                Ok(IndexProcessingJobReport {
                    job_id: job.id.clone(),
                    job_type: job.job_type.clone(),
                    document_source: job.document_source.clone(),
                    document_id: job.document_id.clone(),
                    outcome: "completed".to_owned(),
                    processing_mode,
                    fallback_to_full: fallback_to_full.clone(),
                    documents_total,
                    documents_indexed: documents_total,
                    error: if errors.is_empty() {
                        None
                    } else {
                        Some(errors.join("; "))
                    },
                })
            }
            Err(error) => {
                let mut error_message = error;
                if let Err(fail_error) = db.fail_search_index_job(&job.id, &error_message) {
                    error_message.push_str("; failed to mark search index job failed: ");
                    error_message.push_str(&fail_error.to_string());
                }
                Ok(IndexProcessingJobReport {
                    job_id: job.id.clone(),
                    job_type: job.job_type.clone(),
                    document_source: job.document_source.clone(),
                    document_id: job.document_id.clone(),
                    outcome: "failed".to_owned(),
                    processing_mode,
                    fallback_to_full,
                    documents_total,
                    documents_indexed: 0,
                    error: Some(error_message),
                })
            }
        }
    })();

    release_index_publish_lock(db, &job.workspace_id, &holder_id);
    result
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IncrementalFallbackReason {
    IndexAbsent,
    GenerationSkew,
    TierUnavailable,
    ForcedReindex,
    DeltaOverThreshold,
}

impl IncrementalFallbackReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::IndexAbsent => "index_absent",
            Self::GenerationSkew => "generation_skew",
            Self::TierUnavailable => "tier_unavailable",
            Self::ForcedReindex => "forced_reindex",
            Self::DeltaOverThreshold => "delta_over_threshold",
        }
    }
}

#[derive(Debug)]
enum IncrementalApplyOutcome {
    Applied {
        documents_indexed: u32,
    },
    Fallback {
        reason: IncrementalFallbackReason,
        detail: String,
    },
    FullRebuildRequired,
}

#[derive(Debug)]
struct IncrementalFallback {
    reason: IncrementalFallbackReason,
    detail: String,
}

fn incremental_document_id_for_job(job: &StoredSearchIndexJob) -> Option<&str> {
    job.document_id
        .as_deref()
        .map(str::trim)
        .filter(|document_id| !document_id.is_empty())
}

/// bd-d67os.7: the deduplicated set of currently-present documents touched by a
/// coalesced batch, IFF every claimed job is a simple single-document /
/// incremental upsert whose target document currently exists in the corpus.
/// Returns `None` — forcing the proven full-rebuild path — for any full-rebuild
/// job, a deleted/missing target, or an empty batch, so the incremental merge
/// path can never weaken the published index state.
fn coalesced_incremental_batch(
    claimed: &[StoredSearchIndexJob],
    indexable_docs: &[crate::search::IndexableDocument],
) -> Option<Vec<crate::search::IndexableDocument>> {
    let mut documents = Vec::with_capacity(claimed.len());
    let mut seen = std::collections::BTreeSet::new();
    for job in claimed {
        if !matches!(
            job.job_type_enum(),
            Some(SearchIndexJobType::Incremental | SearchIndexJobType::SingleDocument)
        ) {
            return None;
        }
        let document_id = incremental_document_id_for_job(job)?;
        let document = indexable_docs
            .iter()
            .find(|candidate| candidate.id.as_str() == document_id)?;
        if seen.insert(document_id.to_owned()) {
            documents.push(document.clone());
        }
    }
    (!documents.is_empty()).then_some(documents)
}

fn missing_incremental_target_reason(job: &StoredSearchIndexJob) -> IncrementalFallbackReason {
    if job.documents_total > 1 {
        IncrementalFallbackReason::DeltaOverThreshold
    } else {
        IncrementalFallbackReason::ForcedReindex
    }
}

fn incremental_fallback(
    reason: IncrementalFallbackReason,
    detail: impl Into<String>,
) -> IncrementalFallback {
    IncrementalFallback {
        reason,
        detail: detail.into(),
    }
}

fn publish_full_index_generation(
    index_dir: &Path,
    indexable_docs: Vec<crate::search::IndexableDocument>,
    generation: u64,
    documents_total: u32,
) -> Result<BuildStats, String> {
    let staging_dir = create_publish_staging_dir(index_dir).map_err(|error| error.to_string())?;
    build_index_sync(&staging_dir, default_embedder_stack(), indexable_docs).and_then(|stats| {
        write_index_metadata(&staging_dir, generation, documents_total)
            .and_then(|()| publish_staged_index(index_dir, &staging_dir))
            .map_err(|error| error.to_string())?;
        Ok(stats)
    })
}

fn apply_incremental_index_change_sync(
    index_dir: &Path,
    stack: EmbedderStack,
    document_id: &str,
    document: Option<crate::search::IndexableDocument>,
    generation: u64,
    documents_total: u32,
) -> IncrementalApplyOutcome {
    let index_dir_owned = index_dir.to_path_buf();
    let document_id_owned = document_id.to_owned();
    let result_holder: Arc<Mutex<Option<IncrementalApplyOutcome>>> = Arc::new(Mutex::new(None));
    let task_result = Arc::clone(&result_holder);
    let runtime_error_result = Arc::clone(&result_holder);

    let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let runtime_result = crate::core::run_cli_future(async move {
            let cx = asupersync::Cx::for_testing();
            let outcome = match apply_incremental_index_change(
                &cx,
                &index_dir_owned,
                stack,
                &document_id_owned,
                document,
                generation,
                documents_total,
            )
            .await
            {
                Ok(documents_indexed) => IncrementalApplyOutcome::Applied { documents_indexed },
                Err(fallback) => IncrementalApplyOutcome::Fallback {
                    reason: fallback.reason,
                    detail: fallback.detail,
                },
            };
            if let Ok(mut guard) = task_result.lock() {
                *guard = Some(outcome);
            }
        });

        if let Err(error) = runtime_result
            && let Ok(mut guard) = runtime_error_result.lock()
        {
            *guard = Some(IncrementalApplyOutcome::Fallback {
                reason: IncrementalFallbackReason::ForcedReindex,
                detail: format!("incremental index runtime failed: {error}"),
            });
        }
    }));

    if panic_result.is_err() {
        return IncrementalApplyOutcome::Fallback {
            reason: IncrementalFallbackReason::ForcedReindex,
            detail: "incremental index intake panicked".to_owned(),
        };
    }

    result_holder
        .lock()
        .ok()
        .and_then(|mut guard| guard.take())
        .unwrap_or_else(|| IncrementalApplyOutcome::Fallback {
            reason: IncrementalFallbackReason::ForcedReindex,
            detail: "incremental index result was not captured".to_owned(),
        })
}

/// bd-d67os.7: synchronous wrapper for a coalesced-batch incremental merge.
/// Mirrors [`apply_incremental_index_change_sync`] but upserts K documents into
/// the existing index in one publish step; any error/panic degrades to a
/// `Fallback`, which the caller turns into a full rebuild.
fn apply_incremental_index_batch_sync(
    index_dir: &Path,
    stack: EmbedderStack,
    documents: Vec<crate::search::IndexableDocument>,
    generation: u64,
    documents_total: u32,
) -> IncrementalApplyOutcome {
    let index_dir_owned = index_dir.to_path_buf();
    let result_holder: Arc<Mutex<Option<IncrementalApplyOutcome>>> = Arc::new(Mutex::new(None));
    let task_result = Arc::clone(&result_holder);
    let runtime_error_result = Arc::clone(&result_holder);

    let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let runtime_result = crate::core::run_cli_future(async move {
            let cx = asupersync::Cx::for_testing();
            let outcome = match apply_incremental_index_batch(
                &cx,
                &index_dir_owned,
                stack,
                &documents,
                generation,
                documents_total,
            )
            .await
            {
                Ok(documents_indexed) => IncrementalApplyOutcome::Applied { documents_indexed },
                Err(fallback) => IncrementalApplyOutcome::Fallback {
                    reason: fallback.reason,
                    detail: fallback.detail,
                },
            };
            if let Ok(mut guard) = task_result.lock() {
                *guard = Some(outcome);
            }
        });

        if let Err(error) = runtime_result
            && let Ok(mut guard) = runtime_error_result.lock()
        {
            *guard = Some(IncrementalApplyOutcome::Fallback {
                reason: IncrementalFallbackReason::ForcedReindex,
                detail: format!("incremental batch index runtime failed: {error}"),
            });
        }
    }));

    if panic_result.is_err() {
        return IncrementalApplyOutcome::Fallback {
            reason: IncrementalFallbackReason::ForcedReindex,
            detail: "incremental batch index intake panicked".to_owned(),
        };
    }

    result_holder
        .lock()
        .ok()
        .and_then(|mut guard| guard.take())
        .unwrap_or_else(|| IncrementalApplyOutcome::Fallback {
            reason: IncrementalFallbackReason::ForcedReindex,
            detail: "incremental batch index result was not captured".to_owned(),
        })
}

/// bd-d67os.7: upsert every document in a coalesced batch into the existing
/// index, then publish metadata once. Each `upsert_incremental_document` is the
/// same per-document merge the single-write path uses, so the resulting index
/// state matches a full rebuild of the same corpus; metadata-existence is still
/// validated up front so an absent/stale index degrades to a full rebuild.
async fn apply_incremental_index_batch(
    cx: &asupersync::Cx,
    index_dir: &Path,
    stack: EmbedderStack,
    documents: &[crate::search::IndexableDocument],
    generation: u64,
    documents_total: u32,
) -> Result<u32, IncrementalFallback> {
    let max_generation_lag = u64::try_from(documents.len()).unwrap_or(u64::MAX).max(1);
    validate_incremental_index_metadata(index_dir, generation, max_generation_lag)?;
    for document in documents {
        upsert_incremental_document(cx, index_dir, stack.clone(), document).await?;
    }
    write_index_metadata(index_dir, generation, documents_total).map_err(|error| {
        incremental_fallback(
            IncrementalFallbackReason::TierUnavailable,
            format!("failed to write incremental batch index metadata: {error}"),
        )
    })?;
    u32::try_from(documents.len()).map_err(|_| {
        incremental_fallback(
            IncrementalFallbackReason::DeltaOverThreshold,
            "incremental batch document count exceeds u32".to_owned(),
        )
    })
}

async fn apply_incremental_index_change(
    cx: &asupersync::Cx,
    index_dir: &Path,
    stack: EmbedderStack,
    document_id: &str,
    document: Option<crate::search::IndexableDocument>,
    generation: u64,
    documents_total: u32,
) -> Result<u32, IncrementalFallback> {
    validate_incremental_index_metadata(index_dir, generation, 1)?;

    match document {
        Some(document) => {
            upsert_incremental_document(cx, index_dir, stack, &document).await?;
            write_index_metadata(index_dir, generation, documents_total).map_err(|error| {
                incremental_fallback(
                    IncrementalFallbackReason::TierUnavailable,
                    format!("failed to write incremental index metadata: {error}"),
                )
            })?;
            Ok(1)
        }
        None => {
            delete_incremental_document(cx, index_dir, document_id).await?;
            write_index_metadata(index_dir, generation, documents_total).map_err(|error| {
                incremental_fallback(
                    IncrementalFallbackReason::TierUnavailable,
                    format!("failed to write incremental index metadata: {error}"),
                )
            })?;
            Ok(0)
        }
    }
}

fn validate_incremental_index_metadata(
    index_dir: &Path,
    generation: u64,
    max_generation_lag: u64,
) -> Result<(), IncrementalFallback> {
    ensure_index_path_has_no_symlinks(index_dir, "apply incremental index intake").map_err(
        |error| {
            incremental_fallback(
                IncrementalFallbackReason::TierUnavailable,
                error.to_string(),
            )
        },
    )?;
    match std::fs::symlink_metadata(index_dir) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(incremental_fallback(
                IncrementalFallbackReason::TierUnavailable,
                format!(
                    "active index path is not a directory: {}",
                    index_dir.display()
                ),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(incremental_fallback(
                IncrementalFallbackReason::IndexAbsent,
                format!("active index directory is absent: {}", index_dir.display()),
            ));
        }
        Err(error) => {
            return Err(incremental_fallback(
                IncrementalFallbackReason::TierUnavailable,
                format!("failed to inspect active index directory: {error}"),
            ));
        }
    }

    let metadata_path = index_dir.join(INDEX_METADATA_FILE);
    let Some(content) = read_index_metadata_contents(&metadata_path).map_err(|error| {
        incremental_fallback(
            IncrementalFallbackReason::GenerationSkew,
            format!("failed to read index metadata: {error}"),
        )
    })?
    else {
        return Err(incremental_fallback(
            IncrementalFallbackReason::IndexAbsent,
            format!("index metadata is absent: {}", metadata_path.display()),
        ));
    };
    let json: serde_json::Value = serde_json::from_str(&content).map_err(|error| {
        incremental_fallback(
            IncrementalFallbackReason::GenerationSkew,
            format!("failed to parse index metadata: {error}"),
        )
    })?;
    let index_generation = json
        .get("generation")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            incremental_fallback(
                IncrementalFallbackReason::GenerationSkew,
                "index metadata does not contain a numeric generation",
            )
        })?;
    if index_generation > generation {
        return Err(incremental_fallback(
            IncrementalFallbackReason::GenerationSkew,
            format!(
                "index generation {index_generation} is ahead of database generation {generation}"
            ),
        ));
    }
    let generation_lag = generation.saturating_sub(index_generation);
    if generation_lag > max_generation_lag {
        return Err(incremental_fallback(
            IncrementalFallbackReason::GenerationSkew,
            format!(
                "index generation {index_generation} is {generation_lag} generations behind database generation {generation}; max incremental lag is {max_generation_lag}"
            ),
        ));
    }
    Ok(())
}

async fn upsert_incremental_document(
    cx: &asupersync::Cx,
    index_dir: &Path,
    stack: EmbedderStack,
    document: &crate::search::IndexableDocument,
) -> Result<(), IncrementalFallback> {
    let fast_embedder = stack.fast_arc();
    let fast_vector = fast_embedder
        .embed(cx, &document.content)
        .await
        .map_err(|error| {
            incremental_fallback(
                IncrementalFallbackReason::TierUnavailable,
                format!("fast-tier embedding failed: {error}"),
            )
        })?;
    let mut fast_index = open_fast_vector_index(index_dir)?;
    fast_index
        .append(&document.id, &fast_vector)
        .map_err(|error| {
            incremental_fallback(
                IncrementalFallbackReason::TierUnavailable,
                format!("fast-tier vector upsert failed: {error}"),
            )
        })?;
    compact_incremental_vector_index(&mut fast_index, "fast")?;

    if let Some(quality_embedder) = stack.quality_arc() {
        let quality_vector = quality_embedder
            .embed(cx, &document.content)
            .await
            .map_err(|error| {
                incremental_fallback(
                    IncrementalFallbackReason::TierUnavailable,
                    format!("quality-tier embedding failed: {error}"),
                )
            })?;
        let mut quality_index = open_quality_vector_index(index_dir)?.ok_or_else(|| {
            incremental_fallback(
                IncrementalFallbackReason::TierUnavailable,
                "quality-tier vector index is absent for a two-tier embedder stack",
            )
        })?;
        quality_index
            .append(&document.id, &quality_vector)
            .map_err(|error| {
                incremental_fallback(
                    IncrementalFallbackReason::TierUnavailable,
                    format!("quality-tier vector upsert failed: {error}"),
                )
            })?;
        compact_incremental_vector_index(&mut quality_index, "quality")?;
    }

    #[cfg(feature = "lexical-bm25")]
    {
        let lexical = open_lexical_index(index_dir)?;
        lexical
            .index_document(cx, document)
            .await
            .map_err(|error| {
                incremental_fallback(
                    IncrementalFallbackReason::TierUnavailable,
                    format!("lexical upsert failed: {error}"),
                )
            })?;
        lexical.commit(cx).await.map_err(|error| {
            incremental_fallback(
                IncrementalFallbackReason::TierUnavailable,
                format!("lexical commit failed: {error}"),
            )
        })?;
    }

    Ok(())
}

async fn delete_incremental_document(
    cx: &asupersync::Cx,
    index_dir: &Path,
    document_id: &str,
) -> Result<(), IncrementalFallback> {
    let mut fast_index = open_fast_vector_index(index_dir)?;
    if fast_index.soft_delete(document_id).map_err(|error| {
        incremental_fallback(
            IncrementalFallbackReason::TierUnavailable,
            format!("fast-tier vector delete failed: {error}"),
        )
    })? {
        vacuum_incremental_vector_index(&mut fast_index, "fast")?;
    }

    if let Some(mut quality_index) = open_quality_vector_index(index_dir)?
        && quality_index.soft_delete(document_id).map_err(|error| {
            incremental_fallback(
                IncrementalFallbackReason::TierUnavailable,
                format!("quality-tier vector delete failed: {error}"),
            )
        })?
    {
        vacuum_incremental_vector_index(&mut quality_index, "quality")?;
    }

    #[cfg(feature = "lexical-bm25")]
    {
        let lexical = open_lexical_index(index_dir)?;
        lexical
            .delete_document(cx, document_id)
            .await
            .map_err(|error| {
                incremental_fallback(
                    IncrementalFallbackReason::TierUnavailable,
                    format!("lexical delete failed: {error}"),
                )
            })?;
        lexical.commit(cx).await.map_err(|error| {
            incremental_fallback(
                IncrementalFallbackReason::TierUnavailable,
                format!("lexical commit failed: {error}"),
            )
        })?;
    }

    Ok(())
}

fn open_fast_vector_index(index_dir: &Path) -> Result<VectorIndex, IncrementalFallback> {
    let fast_path = index_dir.join(VECTOR_INDEX_FAST_FILE);
    let fallback_path = index_dir.join(VECTOR_INDEX_FALLBACK_FILE);
    let path = if path_is_regular_file_no_follow(&fast_path) {
        fast_path
    } else if path_is_regular_file_no_follow(&fallback_path) {
        fallback_path
    } else {
        return Err(incremental_fallback(
            IncrementalFallbackReason::IndexAbsent,
            format!(
                "no fast vector tier found at {} or {}",
                fast_path.display(),
                fallback_path.display()
            ),
        ));
    };
    VectorIndex::open(&path).map_err(|error| {
        incremental_fallback(
            IncrementalFallbackReason::TierUnavailable,
            format!(
                "failed to open fast vector tier {}: {error}",
                path.display()
            ),
        )
    })
}

fn open_quality_vector_index(index_dir: &Path) -> Result<Option<VectorIndex>, IncrementalFallback> {
    let path = index_dir.join(VECTOR_INDEX_QUALITY_FILE);
    if !path_exists_no_follow(&path) {
        return Ok(None);
    }
    if !path_is_regular_file_no_follow(&path) {
        return Err(incremental_fallback(
            IncrementalFallbackReason::TierUnavailable,
            format!(
                "quality vector tier is not a regular file: {}",
                path.display()
            ),
        ));
    }
    VectorIndex::open(&path).map(Some).map_err(|error| {
        incremental_fallback(
            IncrementalFallbackReason::TierUnavailable,
            format!(
                "failed to open quality vector tier {}: {error}",
                path.display()
            ),
        )
    })
}

fn compact_incremental_vector_index(
    index: &mut VectorIndex,
    tier: &str,
) -> Result<(), IncrementalFallback> {
    if index.wal_record_count() == 0 {
        return Ok(());
    }
    let stats = index.compact().map_err(|error| {
        incremental_fallback(
            IncrementalFallbackReason::TierUnavailable,
            format!("{tier}-tier vector compaction failed: {error}"),
        )
    })?;
    tracing::info!(
        target: "ee::index",
        tier,
        main_records_before = stats.main_records_before,
        wal_records = stats.wal_records,
        total_records_after = stats.total_records_after,
        elapsed_ms = stats.elapsed_ms,
        "incremental vector WAL compacted"
    );
    Ok(())
}

fn vacuum_incremental_vector_index(
    index: &mut VectorIndex,
    tier: &str,
) -> Result<(), IncrementalFallback> {
    let stats = index.vacuum().map_err(|error| {
        incremental_fallback(
            IncrementalFallbackReason::TierUnavailable,
            format!("{tier}-tier vector vacuum failed: {error}"),
        )
    })?;
    tracing::info!(
        target: "ee::index",
        tier,
        records_before = stats.records_before,
        records_after = stats.records_after,
        tombstones_removed = stats.tombstones_removed,
        duration_ms = duration_millis_saturating(stats.duration),
        "incremental vector tombstones vacuumed"
    );
    Ok(())
}

#[cfg(feature = "lexical-bm25")]
fn open_lexical_index(index_dir: &Path) -> Result<TantivyIndex, IncrementalFallback> {
    let lexical_path = index_dir.join(LEXICAL_INDEX_SUBDIR);
    if !path_exists_no_follow(&lexical_path) {
        return Err(incremental_fallback(
            IncrementalFallbackReason::TierUnavailable,
            format!("lexical tier is absent: {}", lexical_path.display()),
        ));
    }
    TantivyIndex::open(&lexical_path).map_err(|error| {
        incremental_fallback(
            IncrementalFallbackReason::TierUnavailable,
            format!(
                "failed to open lexical tier {}: {error}",
                lexical_path.display()
            ),
        )
    })
}

fn collect_workspace_indexable_documents(
    db: &DbConnection,
    workspace_id: &str,
) -> Result<(u32, u32, u32, Vec<crate::search::IndexableDocument>), IndexRebuildError> {
    let memories = db.list_memories_for_retrieval_with_global(workspace_id, None, false)?;
    let sessions = db.list_sessions(workspace_id)?;
    let artifacts = db.list_artifacts(workspace_id, None)?;
    let memory_docs = memory_documents_with_anchors(db, &memories)?;
    let session_docs: Vec<CanonicalSearchDocument> =
        sessions.iter().map(session_to_document).collect();
    let artifact_docs: Vec<CanonicalSearchDocument> =
        artifacts.iter().map(artifact_to_document).collect();
    let (memories_indexed, sessions_indexed, _artifacts_indexed, documents_total) =
        checked_document_counts(memory_docs.len(), session_docs.len(), artifact_docs.len())?;
    let indexable_docs = memory_docs
        .into_iter()
        .chain(session_docs)
        .chain(artifact_docs)
        .map(|doc| doc.into_indexable())
        .collect();
    Ok((
        memories_indexed,
        sessions_indexed,
        documents_total,
        indexable_docs,
    ))
}

fn memory_documents_with_anchors(
    db: &DbConnection,
    memories: &[crate::db::StoredMemory],
) -> Result<Vec<CanonicalSearchDocument>, IndexRebuildError> {
    memories
        .iter()
        .map(|memory| {
            // Index rebuild is the fourth precision anchor-extraction point, after
            // remember, CASS import, and curate apply (all wired through
            // DbConnection::insert_memory). Backfill anchors for memories that have
            // none yet — rows created before the anchor table existed, or through the
            // revision write path, which does not extract at insert time. Memories
            // that already carry anchors keep their original source provenance; this
            // never downgrades a cass_import/curate_apply source to index_rebuild.
            let mut anchors = db.list_memory_anchors(&memory.id)?;
            if anchors.is_empty() {
                db.refresh_memory_anchors_for_memory(&memory.id, &memory.content)?;
                anchors = db.list_memory_anchors(&memory.id)?;
            }
            // ADR 0064: the anchor reverse index is rebuilt alongside the
            // search documents so `ee index rebuild` restores it from
            // scratch and its MAX(generation) advances with the rebuild.
            db.refresh_memory_anchor_index_for_memory(
                &memory.workspace_id,
                &memory.id,
                &memory.content,
            )?;
            let typed_fields_json = db.get_memory_typed_fields_json(&memory.id)?;
            Ok(memory_to_document_with_context_anchors_and_typed_fields(
                memory,
                None,
                &[],
                &anchors,
                typed_fields_json.as_deref(),
            ))
        })
        .collect()
}

fn processing_mode_for_job(job: &StoredSearchIndexJob) -> &'static str {
    match job.job_type_enum() {
        Some(SearchIndexJobType::FullRebuild) => "full_rebuild",
        Some(SearchIndexJobType::Incremental) => "incremental_as_full_rebuild",
        Some(SearchIndexJobType::SingleDocument) => "single_document_as_full_rebuild",
        None => "unknown_as_full_rebuild",
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IndexPublishRecoveryAction {
    ActivePresent,
    RetainedGenerationRestored,
    StagedGenerationPromoted,
    NoRecoverableGeneration,
}

fn recover_interrupted_publish(
    index_dir: &Path,
) -> Result<IndexPublishRecoveryAction, IndexRebuildError> {
    ensure_index_path_has_no_symlinks(index_dir, "recover interrupted index publish")?;

    if path_exists_no_follow(index_dir) {
        return Ok(IndexPublishRecoveryAction::ActivePresent);
    }

    let retained_dir = retained_index_dir(index_dir)?;
    if path_exists_no_follow(&retained_dir) {
        rename_index_dir(
            &retained_dir,
            index_dir,
            "restore retained index generation",
        )?;
        return Ok(IndexPublishRecoveryAction::RetainedGenerationRestored);
    }

    if let Some(staging_dir) = find_complete_staging_dir(index_dir)? {
        rename_index_dir(&staging_dir, index_dir, "promote staged index generation")?;
        return Ok(IndexPublishRecoveryAction::StagedGenerationPromoted);
    }

    Ok(IndexPublishRecoveryAction::NoRecoverableGeneration)
}

fn create_publish_staging_dir(index_dir: &Path) -> Result<PathBuf, IndexRebuildError> {
    let parent = index_parent(index_dir);
    ensure_index_path_has_no_symlinks(parent, "create index parent directory")?;
    std::fs::create_dir_all(parent).map_err(|e| {
        IndexRebuildError::Index(format!("Failed to create index parent directory: {e}"))
    })?;

    let base = index_base_name(index_dir)?;
    let stamp = monotonicish_stamp();
    for sequence in 0_u32..1000 {
        let candidate = parent.join(format!(
            ".{base}{INDEX_STAGING_PREFIX}{stamp}-{sequence:03}"
        ));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(IndexRebuildError::Index(format!(
                    "Failed to create index staging directory: {error}"
                )));
            }
        }
    }

    Err(IndexRebuildError::Index(
        "Failed to allocate a unique index staging directory".to_string(),
    ))
}

fn publish_staged_index(index_dir: &Path, staging_dir: &Path) -> Result<(), IndexRebuildError> {
    ensure_index_path_has_no_symlinks(staging_dir, "publish staged index generation")?;
    ensure_index_path_has_no_symlinks(index_dir, "publish staged index generation")?;

    if !path_exists_no_follow(staging_dir) {
        return Err(IndexRebuildError::Index(format!(
            "Index staging directory does not exist: {}",
            staging_dir.display()
        )));
    }

    let retained_dir = if path_exists_no_follow(index_dir) {
        let retained = allocate_retained_index_dir(index_dir)?;
        rename_index_dir(index_dir, &retained, "retain previous index generation")?;
        Some(retained)
    } else {
        None
    };

    if let Err(error) = rename_index_dir(staging_dir, index_dir, "publish staged index generation")
    {
        if let Some(retained) = retained_dir
            && !path_exists_no_follow(index_dir)
        {
            if let Err(recovery_error) =
                rename_index_dir(&retained, index_dir, "restore previous index generation")
            {
                tracing::error!(%recovery_error, "failed to restore previous index generation after publish failure");
            }
        }
        return Err(error);
    }

    Ok(())
}

fn write_index_metadata(
    index_dir: &Path,
    generation: u64,
    documents_total: u32,
) -> Result<(), IndexRebuildError> {
    let timestamp = current_timestamp_rfc3339();
    let metadata = serde_json::json!({
        "schema": "ee.index_metadata.v1",
        "generation": generation,
        "sourceGeneration": generation,
        "lastRebuildAt": timestamp,
        "documentCount": documents_total,
    });
    let serialized = serde_json::to_vec_pretty(&metadata).map_err(|e| {
        IndexRebuildError::Index(format!("Failed to serialize index metadata: {e}"))
    })?;

    let meta_path = index_dir.join(INDEX_METADATA_FILE);
    ensure_index_path_has_no_symlinks(&meta_path, "write index metadata")?;
    ensure_index_metadata_path_is_regular_or_missing(&meta_path, "write index metadata")?;
    let temp_path = unique_index_metadata_temp_path(&meta_path)?;
    ensure_index_path_has_no_symlinks(&temp_path, "write temporary index metadata")?;
    ensure_index_metadata_temp_path_is_missing(&temp_path, "write temporary index metadata")?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .map_err(|e| {
            IndexRebuildError::Index(format!(
                "Failed to create temporary index metadata {}: {e}",
                temp_path.display()
            ))
        })?;

    use std::io::Write;
    file.write_all(&serialized).map_err(|e| {
        IndexRebuildError::Index(format!("Failed to write temporary index metadata: {e}"))
    })?;

    file.sync_data().map_err(|e| {
        IndexRebuildError::Index(format!("Failed to sync temporary index metadata: {e}"))
    })?;
    drop(file);

    publish_index_metadata_temp_file(&meta_path, &temp_path)
}

fn unique_index_metadata_temp_path(meta_path: &Path) -> Result<PathBuf, IndexRebuildError> {
    let file_name = meta_path.file_name().ok_or_else(|| {
        IndexRebuildError::Index(format!(
            "Failed to build temporary index metadata path for {}: missing file name",
            meta_path.display()
        ))
    })?;
    let counter = INDEX_METADATA_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut temp_name = OsString::from(".");
    temp_name.push(file_name);
    temp_name.push(OsString::from(format!(
        ".{}.{}.{}.tmp",
        std::process::id(),
        monotonicish_stamp(),
        counter
    )));
    Ok(match meta_path.parent() {
        Some(parent) => parent.join(&temp_name),
        None => PathBuf::from(temp_name),
    })
}

fn publish_index_metadata_temp_file(
    meta_path: &Path,
    temp_path: &Path,
) -> Result<(), IndexRebuildError> {
    ensure_index_path_has_no_symlinks(meta_path, "publish index metadata")?;
    ensure_index_metadata_path_is_regular_or_missing(meta_path, "publish index metadata")?;
    ensure_index_path_has_no_symlinks(temp_path, "publish temporary index metadata")?;
    ensure_index_metadata_temp_path_is_regular(temp_path, "publish temporary index metadata")?;
    std::fs::rename(temp_path, meta_path).map_err(|e| {
        IndexRebuildError::Index(format!(
            "Failed to publish index metadata from {} to {}: {e}",
            temp_path.display(),
            meta_path.display()
        ))
    })?;

    Ok(())
}

fn find_complete_staging_dir(index_dir: &Path) -> Result<Option<PathBuf>, IndexRebuildError> {
    let parent = index_parent(index_dir);
    if !path_exists_no_follow(parent) {
        return Ok(None);
    }

    let base = index_base_name(index_dir)?;
    let prefix = format!(".{base}{INDEX_STAGING_PREFIX}");
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(parent).map_err(|e| {
        IndexRebuildError::Index(format!("Failed to inspect index parent directory: {e}"))
    })? {
        let entry = entry.map_err(|e| {
            IndexRebuildError::Index(format!("Failed to inspect index staging entry: {e}"))
        })?;
        if !entry.file_type().map(|ty| ty.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix)
            && path_is_regular_file_no_follow(&entry.path().join(INDEX_METADATA_FILE))
        {
            candidates.push(entry.path());
        }
    }
    candidates.sort();
    Ok(candidates.pop())
}

fn retained_index_dir(index_dir: &Path) -> Result<PathBuf, IndexRebuildError> {
    let parent = index_parent(index_dir);
    let base = index_base_name(index_dir)?;
    Ok(parent.join(format!("{base}{INDEX_RETAINED_SUFFIX}")))
}

fn allocate_retained_index_dir(index_dir: &Path) -> Result<PathBuf, IndexRebuildError> {
    let parent = index_parent(index_dir);
    let base = index_base_name(index_dir)?;
    for sequence in 0_u32..1000 {
        let candidate = if sequence == 0 {
            parent.join(format!("{base}{INDEX_RETAINED_SUFFIX}"))
        } else {
            parent.join(format!("{base}{INDEX_RETAINED_SUFFIX}.{sequence:03}"))
        };
        if !path_exists_no_follow(&candidate) {
            return Ok(candidate);
        }
    }

    Err(IndexRebuildError::Index(
        "Failed to allocate retained index generation directory".to_string(),
    ))
}

fn index_parent(index_dir: &Path) -> &Path {
    index_dir
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn index_base_name(index_dir: &Path) -> Result<String, IndexRebuildError> {
    index_dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            IndexRebuildError::Index(format!(
                "Index directory must have a final path component: {}",
                index_dir.display()
            ))
        })
}

fn rename_index_dir(from: &Path, to: &Path, action: &str) -> Result<(), IndexRebuildError> {
    ensure_index_path_has_no_symlinks(from, action)?;
    ensure_index_path_has_no_symlinks(to, action)?;
    ensure_index_publish_source_is_directory(from, action)?;
    ensure_index_publish_target_is_directory_or_missing(to, action)?;
    std::fs::rename(from, to).map_err(|e| {
        IndexRebuildError::Index(format!(
            "Failed to {action} from {} to {}: {e}",
            from.display(),
            to.display()
        ))
    })
}

fn ensure_index_publish_source_is_directory(
    path: &Path,
    action: &str,
) -> Result<(), IndexRebuildError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => Err(IndexRebuildError::Index(format!(
            "Refusing to {action} because index generation source is not a directory: {}",
            path.display()
        ))),
        Err(error) => Err(IndexRebuildError::Index(format!(
            "Failed to inspect index generation source before {action} at {}: {error}",
            path.display()
        ))),
    }
}

fn ensure_index_publish_target_is_directory_or_missing(
    path: &Path,
    action: &str,
) -> Result<(), IndexRebuildError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => Err(IndexRebuildError::Index(format!(
            "Refusing to {action} because index generation target is not a directory: {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(IndexRebuildError::Index(format!(
            "Failed to inspect index generation target before {action} at {}: {error}",
            path.display()
        ))),
    }
}

fn ensure_index_path_has_no_symlinks(path: &Path, action: &str) -> Result<(), IndexRebuildError> {
    if let Some(component) = first_existing_index_symlink_component(path)? {
        return Err(IndexRebuildError::Index(format!(
            "Refusing to {action} through symlinked index path component: {}",
            component.display()
        )));
    }
    Ok(())
}

fn first_existing_index_symlink_component(
    path: &Path,
) -> Result<Option<PathBuf>, IndexRebuildError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                current.push(component.as_os_str());
                continue;
            }
            std::path::Component::CurDir => continue,
            std::path::Component::ParentDir | std::path::Component::Normal(_) => {
                current.push(component.as_os_str());
            }
        }

        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(Some(current)),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(IndexRebuildError::Index(format!(
                    "Failed to inspect index path component {}: {error}",
                    current.display()
                )));
            }
        }
    }
    Ok(None)
}

fn path_is_regular_file_no_follow(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_file())
        .unwrap_or(false)
}

fn path_exists_no_follow(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn ensure_index_metadata_path_is_regular_or_missing(
    path: &Path,
    action: &str,
) -> Result<(), IndexRebuildError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(IndexRebuildError::Index(format!(
            "Refusing to {action} because index metadata path is not a regular file: {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(IndexRebuildError::Index(format!(
            "Failed to inspect index metadata path {} before {action}: {error}",
            path.display()
        ))),
    }
}

fn ensure_index_metadata_temp_path_is_missing(
    path: &Path,
    action: &str,
) -> Result<(), IndexRebuildError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Err(IndexRebuildError::Index(format!(
            "Refusing to {action} because temporary index metadata already exists: {}",
            path.display()
        ))),
        Ok(_) => Err(IndexRebuildError::Index(format!(
            "Refusing to {action} because temporary index metadata path is not a regular file: {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(IndexRebuildError::Index(format!(
            "Failed to inspect temporary index metadata path {} before {action}: {error}",
            path.display()
        ))),
    }
}

fn ensure_index_metadata_temp_path_is_regular(
    path: &Path,
    action: &str,
) -> Result<(), IndexRebuildError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(IndexRebuildError::Index(format!(
            "Refusing to {action} because temporary index metadata path is not a regular file: {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(IndexRebuildError::Index(format!(
                "Refusing to {action} because temporary index metadata is missing: {}",
                path.display()
            )))
        }
        Err(error) => Err(IndexRebuildError::Index(format!(
            "Failed to inspect temporary index metadata path {} before {action}: {error}",
            path.display()
        ))),
    }
}

fn monotonicish_stamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

fn current_timestamp_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn checked_document_counts(
    memory_count: usize,
    session_count: usize,
    artifact_count: usize,
) -> Result<(u32, u32, u32, u32), IndexRebuildError> {
    let memories_indexed = u32::try_from(memory_count).map_err(|_| {
        IndexRebuildError::Index(format!(
            "Memory document count {memory_count} exceeds the supported maximum."
        ))
    })?;
    let sessions_indexed = u32::try_from(session_count).map_err(|_| {
        IndexRebuildError::Index(format!(
            "Session document count {session_count} exceeds the supported maximum."
        ))
    })?;
    let artifacts_indexed = u32::try_from(artifact_count).map_err(|_| {
        IndexRebuildError::Index(format!(
            "Artifact document count {artifact_count} exceeds the supported maximum."
        ))
    })?;
    let documents_total = memories_indexed
        .checked_add(sessions_indexed)
        .and_then(|count| count.checked_add(artifacts_indexed))
        .ok_or_else(|| {
            IndexRebuildError::Index(
                "Combined document count exceeds the supported maximum.".to_owned(),
            )
        })?;
    Ok((
        memories_indexed,
        sessions_indexed,
        artifacts_indexed,
        documents_total,
    ))
}

fn get_default_workspace_id(db: &DbConnection) -> Result<String, IndexRebuildError> {
    let rows = db.query(
        "SELECT id FROM workspaces ORDER BY created_at DESC LIMIT 1",
        &[],
    )?;

    rows.first()
        .and_then(|row| row.get(0).and_then(|v| v.as_str().map(str::to_string)))
        .ok_or(IndexRebuildError::NoWorkspace)
}

struct BuildStats {
    #[expect(dead_code)]
    doc_count: usize,
    errors: Vec<(String, String)>,
}

fn build_index_sync(
    index_dir: &Path,
    stack: EmbedderStack,
    documents: Vec<crate::search::IndexableDocument>,
) -> Result<BuildStats, String> {
    let index_dir_owned = index_dir.to_path_buf();
    let result_holder: Arc<Mutex<Option<Result<BuildStats, String>>>> = Arc::new(Mutex::new(None));
    let task_result = Arc::clone(&result_holder);
    let runtime_error_result = Arc::clone(&result_holder);

    let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let runtime_result = crate::core::run_cli_future(async move {
            let cx = asupersync::Cx::for_testing();
            let builder = IndexBuilder::new(&index_dir_owned)
                .with_embedder_stack(stack)
                .add_documents(documents);

            let build_result = builder.build(&cx).await;
            let converted = match build_result {
                Ok(stats) => Ok(BuildStats {
                    doc_count: stats.doc_count,
                    errors: stats.errors,
                }),
                Err(e) => Err(format!("Index build failed: {e}")),
            };
            if let Ok(mut guard) = task_result.lock() {
                *guard = Some(converted);
            }
        });

        if let Err(e) = runtime_result
            && let Ok(mut guard) = runtime_error_result.lock()
        {
            *guard = Some(Err(format!("Runtime failed: {e}")));
        }
    }));

    match panic_result {
        Ok(()) => result_holder
            .lock()
            .ok()
            .and_then(|mut guard| guard.take())
            .unwrap_or_else(|| Err("Index build result not captured".to_string())),
        Err(_) => Err("Index build panicked".to_string()),
    }
}

const EE_MODEL_CACHE_SUBDIR: &str = "models";
const EMBEDDING_REGISTRY_FINGERPRINT_SCHEMA: &str = "ee.embedding_registry_fingerprint.v1";
pub(crate) const POTION_MODEL_NAME: &str = "potion-multilingual-128M";
const EE_EMBED_DOWNLOAD_AUTO: &str = "auto";
const EE_EMBED_DOWNLOAD_OFF: &str = "off";
const EE_DOWNLOAD_STATE_PENDING: u8 = 0;
const EE_DOWNLOAD_STATE_READY: u8 = 1;
const EE_DOWNLOAD_STATE_FAILED: u8 = 2;
static DEFAULT_SEARCH_EMBEDDER_STACK: OnceLock<EmbedderStack> = OnceLock::new();

#[derive(Clone, Debug, Eq, PartialEq)]
struct EeEmbedderSettings {
    model_root: PathBuf,
    download_mode: EeEmbedDownloadMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EeEmbedDownloadMode {
    Auto,
    Off,
}

pub(crate) fn default_search_embedder_stack() -> EmbedderStack {
    DEFAULT_SEARCH_EMBEDDER_STACK
        .get_or_init(detect_default_search_embedder_stack)
        .clone()
}

fn detect_default_search_embedder_stack() -> EmbedderStack {
    let settings = default_embedder_settings();
    search_embedder_stack_for_settings(&settings)
}

fn search_embedder_stack_for_settings(settings: &EeEmbedderSettings) -> EmbedderStack {
    tracing::info!(
        target: "ee::index::embedder",
        model_root = %settings.model_root.display(),
        download_mode = ?settings.download_mode,
        "ee embedding model policy resolved"
    );

    if settings.download_mode == EeEmbedDownloadMode::Off {
        // EE_EMBED_DOWNLOAD=off means "never fetch over the network", NOT "never
        // use a model". A host that pre-populated the cache (air-gapped install)
        // or already ran an ee-managed download must still get semantic search.
        // Consult the on-disk model first and only fall back to the
        // deterministic hash embedder when no local semantic model is present.
        // `auto_detect_with` performs local detection only; the network download
        // is driven separately by `EeLazyModel2VecEmbedder`, which we never
        // construct here, so this stays offline. (GH#18: the previous
        // unconditional hash fallback made search report
        // `semantic:false / frankensearch_hash_fallback` even with a valid model
        // on disk, contradicting `ee index reembed`.)
        match EmbedderStack::auto_detect_with(Some(&settings.model_root)) {
            Ok(stack) if stack.fast().is_semantic() => {
                tracing::info!(
                    target: "ee::index::embedder",
                    model_root = %settings.model_root.display(),
                    detected_fast = stack.fast().id(),
                    "EE_EMBED_DOWNLOAD=off; using on-disk semantic model without downloading"
                );
                return stack_with_hash_quality_fallback(stack);
            }
            _ => {
                tracing::info!(
                    target: "ee::index::embedder",
                    model_root = %settings.model_root.display(),
                    "EE_EMBED_DOWNLOAD=off and no on-disk semantic model; using deterministic hash fallback"
                );
                return hash_fallback_embedder_stack();
            }
        }
    }

    match EmbedderStack::auto_detect_with(Some(&settings.model_root)) {
        Ok(stack) if stack.fast().is_semantic() => stack_with_hash_quality_fallback(stack),
        Ok(stack) => {
            tracing::info!(
                target: "ee::index::embedder",
                detected_fast = stack.fast().id(),
                model_root = %settings.model_root.display(),
                "semantic model not present locally; enabling ee-managed first-use download"
            );
            ee_auto_download_embedder_stack(settings.model_root.clone())
        }
        Err(error) => {
            tracing::warn!(
                target: "ee::index::embedder",
                error = %error,
                model_root = %settings.model_root.display(),
                "Frankensearch default embedder auto-detect failed; enabling ee-managed first-use download"
            );
            ee_auto_download_embedder_stack(settings.model_root.clone())
        }
    }
}

fn default_embedder_stack() -> EmbedderStack {
    default_search_embedder_stack()
}

fn stack_with_hash_quality_fallback(stack: EmbedderStack) -> EmbedderStack {
    if stack.quality().is_some() || stack.fast().is_semantic() {
        return stack;
    }
    let fast_embedder = stack.fast_arc();
    let quality_embedder =
        Arc::new(HashEmbedder::default_384()) as Arc<dyn crate::search::Embedder>;
    EmbedderStack::from_parts(fast_embedder, Some(quality_embedder))
}

fn hash_fallback_embedder_stack() -> EmbedderStack {
    let fast_embedder = Arc::new(HashEmbedder::default_256()) as Arc<dyn crate::search::Embedder>;
    let quality_embedder =
        Arc::new(HashEmbedder::default_384()) as Arc<dyn crate::search::Embedder>;
    EmbedderStack::from_parts(fast_embedder, Some(quality_embedder))
}

fn ee_auto_download_embedder_stack(model_root: PathBuf) -> EmbedderStack {
    let fast_embedder =
        Arc::new(EeLazyModel2VecEmbedder::new(model_root)) as Arc<dyn crate::search::Embedder>;
    EmbedderStack::from_parts(fast_embedder, None)
}

fn default_embedder_settings() -> EeEmbedderSettings {
    EeEmbedderSettings {
        model_root: default_embedder_model_root(),
        download_mode: default_embed_download_mode(),
    }
}

pub(crate) fn default_embedder_model_root() -> PathBuf {
    if let Some(model_dir) = configured_embedder_model_root() {
        return model_dir;
    }
    process_ee_data_dir()
        .unwrap_or_else(stable_ee_data_dir_fallback)
        .join(EE_MODEL_CACHE_SUBDIR)
}

fn configured_embedder_model_root() -> Option<PathBuf> {
    crate::config::env_registry::read_os(crate::config::env_registry::EnvVar::EmbedModelDir)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn default_embed_download_mode() -> EeEmbedDownloadMode {
    let raw = crate::config::env_registry::read_or_default(
        crate::config::env_registry::EnvVar::EmbedDownload,
    );
    parse_embed_download_mode(raw.as_deref())
}

fn parse_embed_download_mode(raw: Option<&str>) -> EeEmbedDownloadMode {
    let Some(value) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return EeEmbedDownloadMode::Auto;
    };

    if value.eq_ignore_ascii_case(EE_EMBED_DOWNLOAD_AUTO) {
        return EeEmbedDownloadMode::Auto;
    }
    if value.eq_ignore_ascii_case(EE_EMBED_DOWNLOAD_OFF)
        || value == "0"
        || value.eq_ignore_ascii_case("false")
        || value.eq_ignore_ascii_case("no")
    {
        return EeEmbedDownloadMode::Off;
    }

    tracing::warn!(
        target: "ee::index::embedder",
        value,
        "invalid EE_EMBED_DOWNLOAD value; falling back to auto"
    );
    EeEmbedDownloadMode::Auto
}

/// Hard ceiling on a single bundled-embedding-model download (issue #12).
///
/// The download streams ~531 MiB (potion-128m: a 512 MiB safetensors file plus
/// an 18 MiB tokenizer) over a single HTTP/1.1 connection, driven inline on the
/// CLI's `current_thread` runtime via [`crate::core::run_cli_future`]. A
/// cross-host 302 redirect plus a server-side FIN race in the asupersync H1
/// streaming client can leave that socket parked (CLOSE-WAIT, unread bytes)
/// with the runtime futex-asleep and **no timer pending**, so `block_on` never
/// wakes and `ee model fetch` / `ee index rebuild` / search / init hang
/// FOREVER.
///
/// Wrapping the whole download in a bounded timeout registers a timer, which
/// guarantees the runtime wakes at the deadline and the hang surfaces as a
/// retryable error (here: a graceful fall back to the deterministic hash
/// embedder) instead of an infinite stall. The ceiling is deliberately generous
/// so it can never trip a genuinely slow-but-working download: 1800 s tolerates
/// a sustained ~295 KiB/s across the full 531 MiB — far below any usable link —
/// while still bounding a true deadlock.
///
/// This is the safe, fully ee-side guard. The *precise* fix (a per-read idle
/// timeout that surfaces a stalled connection in seconds with zero
/// false-positive risk) belongs in frankensearch's download client
/// (`crates/frankensearch-embed/src/model_download.rs`, which already builds its
/// `HttpClient` from an `HttpClientConfig` that exposes `.timeout(..)`); that is
/// a separate crate and out of scope here.
pub(crate) const EMBEDDING_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(1800);

struct EeLazyModel2VecEmbedder {
    model_root: PathBuf,
    fallback: Arc<dyn crate::search::Embedder>,
    state: AtomicU8,
    inner: AsyncOnceCell<Arc<dyn crate::search::Embedder>>,
}

impl EeLazyModel2VecEmbedder {
    fn new(model_root: PathBuf) -> Self {
        Self {
            model_root,
            fallback: Arc::new(HashEmbedder::default_256()) as Arc<dyn crate::search::Embedder>,
            state: AtomicU8::new(EE_DOWNLOAD_STATE_PENDING),
            inner: AsyncOnceCell::new(),
        }
    }

    async fn try_load(
        &self,
        cx: &asupersync::Cx,
    ) -> Result<Arc<dyn crate::search::Embedder>, SearchError> {
        let embedder = self
            .inner
            .get_or_try_init(|| async { self.initialize(cx).await })
            .await?;
        self.state.store(EE_DOWNLOAD_STATE_READY, Ordering::Release);
        Ok(Arc::clone(embedder))
    }

    async fn initialize(
        &self,
        cx: &asupersync::Cx,
    ) -> Result<Arc<dyn crate::search::Embedder>, SearchError> {
        let destination = potion_model_destination_dir(&self.model_root);
        if let Ok(embedder) = Model2VecEmbedder::load_with_name(&destination, POTION_MODEL_NAME) {
            return Ok(Arc::new(embedder) as Arc<dyn crate::search::Embedder>);
        }

        let manifest = ModelManifest::potion_128m();
        emit_embedding_download_notice(&destination, manifest.total_size_bytes());
        let reporter = EeModelDownloadReporter::new(POTION_MODEL_NAME);
        let downloader = ModelDownloader::with_defaults();
        let consent = DownloadConsent::granted(ConsentSource::Programmatic);
        let mut lifecycle = ModelLifecycle::new(manifest.clone(), consent);
        let download =
            downloader.download_model(cx, &manifest, &destination, &mut lifecycle, |progress| {
                reporter.report(progress);
            });
        // Bound the streaming download so a stalled connection (issue #12) can
        // never park `block_on` forever: on timeout the download future is
        // dropped (closing the socket) and the embedder degrades to the hash
        // fallback in the caller instead of hanging the whole command.
        let staged = match asupersync::time::TimeoutFuture::after(
            cx.now(),
            EMBEDDING_DOWNLOAD_TIMEOUT,
            download,
        )
        .await
        {
            Ok(result) => result?,
            Err(_elapsed) => {
                return Err(SearchError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "bundled embedding model download exceeded its {}s time limit",
                        EMBEDDING_DOWNLOAD_TIMEOUT.as_secs()
                    ),
                )));
            }
        };
        let backup = manifest.promote_verified_installation(&staged, &destination)?;
        reporter.finish_success();
        tracing::info!(
            target: "ee::index::embedder",
            model = POTION_MODEL_NAME,
            destination = %destination.display(),
            backup = backup.as_ref().map(|path| path.display().to_string()).as_deref().unwrap_or(""),
            "ee-managed embedding model download completed"
        );
        Model2VecEmbedder::load_with_name(&destination, POTION_MODEL_NAME)
            .map(|embedder| Arc::new(embedder) as Arc<dyn crate::search::Embedder>)
    }

    fn mark_failed(&self) {
        self.state
            .store(EE_DOWNLOAD_STATE_FAILED, Ordering::Release);
    }

    fn failed(&self) -> bool {
        self.state.load(Ordering::Acquire) == EE_DOWNLOAD_STATE_FAILED
    }
}

pub(crate) fn embedder_reports_pending_model2vec_download(
    embedder: &dyn crate::search::Embedder,
) -> bool {
    !embedder.is_ready()
        && !embedder.is_semantic()
        && embedder.id() == POTION_MODEL_NAME
        && embedder.category() == ModelCategory::StaticEmbedder
        && embedder.tier() == ModelTier::Fast
}

impl crate::search::Embedder for EeLazyModel2VecEmbedder {
    fn embed<'a>(
        &'a self,
        cx: &'a asupersync::Cx,
        text: &'a str,
    ) -> frankensearch::SearchFuture<'a, Vec<f32>> {
        Box::pin(async move {
            if self.failed() {
                return self.fallback.embed(cx, text).await;
            }
            match self.try_load(cx).await {
                Ok(embedder) => embedder.embed(cx, text).await,
                Err(error) => {
                    self.mark_failed();
                    tracing::warn!(
                        target: "ee::index::embedder",
                        error = %error,
                        model = POTION_MODEL_NAME,
                        "ee-managed embedding model download failed; using deterministic hash fallback for this process"
                    );
                    self.fallback.embed(cx, text).await
                }
            }
        })
    }

    fn embed_batch<'a>(
        &'a self,
        cx: &'a asupersync::Cx,
        texts: &'a [&'a str],
    ) -> frankensearch::SearchFuture<'a, Vec<Vec<f32>>> {
        Box::pin(async move {
            if self.failed() {
                return self.fallback.embed_batch(cx, texts).await;
            }
            match self.try_load(cx).await {
                Ok(embedder) => embedder.embed_batch(cx, texts).await,
                Err(error) => {
                    self.mark_failed();
                    tracing::warn!(
                        target: "ee::index::embedder",
                        error = %error,
                        model = POTION_MODEL_NAME,
                        "ee-managed embedding model download failed; using deterministic hash fallback for this process"
                    );
                    self.fallback.embed_batch(cx, texts).await
                }
            }
        })
    }

    fn dimension(&self) -> usize {
        256
    }

    fn id(&self) -> &str {
        if self.failed() {
            return self.fallback.id();
        }
        POTION_MODEL_NAME
    }

    fn model_name(&self) -> &str {
        if self.failed() {
            return self.fallback.model_name();
        }
        POTION_MODEL_NAME
    }

    fn is_ready(&self) -> bool {
        self.state.load(Ordering::Acquire) == EE_DOWNLOAD_STATE_READY
    }

    fn is_semantic(&self) -> bool {
        self.is_ready()
    }

    fn category(&self) -> ModelCategory {
        if self.failed() {
            return self.fallback.category();
        }
        ModelCategory::StaticEmbedder
    }

    fn tier(&self) -> ModelTier {
        ModelTier::Fast
    }

    fn supports_mrl(&self) -> bool {
        self.is_ready()
    }
}

pub(crate) fn potion_model_destination_dir(model_root: &Path) -> PathBuf {
    if model_root.ends_with(POTION_MODEL_NAME) {
        return model_root.to_path_buf();
    }
    model_root.join(POTION_MODEL_NAME)
}

fn emit_embedding_download_notice(destination: &Path, bytes: u64) {
    eprintln!(
        "ee is downloading the local embedding model {POTION_MODEL_NAME} ({}) once; it will be cached at {}. Set EE_EMBED_DOWNLOAD=off for lexical-only.",
        format_bytes(bytes),
        destination.display()
    );
}

struct EeModelDownloadReporter {
    model_name: &'static str,
    stderr_is_tty: bool,
    last_percent: AtomicU8,
}

impl EeModelDownloadReporter {
    fn new(model_name: &'static str) -> Self {
        Self {
            model_name,
            stderr_is_tty: io::stderr().is_terminal(),
            last_percent: AtomicU8::new(0),
        }
    }

    fn report(&self, progress: &DownloadProgress) {
        if !self.stderr_is_tty {
            return;
        }
        let percent = download_progress_percent(progress);
        let previous = self.last_percent.load(Ordering::Relaxed);
        if percent <= previous {
            return;
        }
        if self
            .last_percent
            .compare_exchange(previous, percent, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            eprint!(
                "\ree model download {model}: {percent:>3}% {downloaded}/{}",
                progress
                    .total_bytes
                    .map_or_else(|| "?".to_owned(), format_bytes),
                model = self.model_name,
                downloaded = format_bytes(progress.bytes_downloaded),
            );
            let _ = io::stderr().flush();
        }
    }

    fn finish_success(&self) {
        if self.stderr_is_tty {
            eprintln!();
        }
    }
}

fn download_progress_percent(progress: &DownloadProgress) -> u8 {
    let files_total = u64::try_from(progress.files_total).unwrap_or(1).max(1);
    let completed = u64::try_from(progress.files_completed)
        .unwrap_or(0)
        .min(files_total);
    let file_share = 100_u64 / files_total;
    let completed_percent = completed.saturating_mul(file_share);
    let current_percent = progress
        .total_bytes
        .filter(|total| *total > 0)
        .map(|total| {
            progress
                .bytes_downloaded
                .min(total)
                .saturating_mul(file_share)
                / total
        })
        .unwrap_or(0);
    u8::try_from((completed_percent + current_percent).min(100)).unwrap_or(100)
}

fn stable_ee_data_dir_fallback() -> PathBuf {
    if cfg!(windows) {
        if let Some(local_app_data) = non_empty_os_env("LOCALAPPDATA") {
            return PathBuf::from(local_app_data).join("ee");
        }
        if let Some(user_profile) = non_empty_os_env("USERPROFILE") {
            return PathBuf::from(user_profile)
                .join("AppData")
                .join("Local")
                .join("ee");
        }
    } else if let Some(home) = non_empty_os_env("HOME") {
        return PathBuf::from(home).join(".local").join("share").join("ee");
    }
    std::env::temp_dir().join("ee")
}

fn non_empty_os_env(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

fn process_ee_data_dir() -> Option<PathBuf> {
    let env = process_env_map();
    if cfg!(windows) {
        return crate::config::path_resolver::resolve_dir_windows_localappdata(&env)
            .ok()
            .map(|root| root.join("ee"));
    }
    crate::config::path_resolver::resolve_dir_unix_xdg(&env, "ee").ok()
}

fn process_env_map() -> BTreeMap<String, OsString> {
    std::env::vars_os()
        .filter_map(|(key, value)| key.into_string().ok().map(|key| (key, value)))
        .collect()
}

fn ensure_active_embedding_registry_record(
    db: &DbConnection,
    workspace_id: &str,
    stack: &EmbedderStack,
) -> Result<(), IndexRebuildError> {
    ensure_loaded_embedding_registry_record(db, workspace_id, stack.fast())
}

pub(crate) fn ensure_loaded_embedding_registry_record(
    db: &DbConnection,
    workspace_id: &str,
    fast_embedder: &dyn crate::search::Embedder,
) -> Result<(), IndexRebuildError> {
    if !fast_embedder.is_semantic() {
        return Ok(());
    }
    let provider = provider_for_embedder(fast_embedder);
    if !fast_embedder.is_ready() {
        tracing::warn!(
            target: "ee::index::embedder",
            provider = provider.as_str(),
            model = fast_embedder.id(),
            "semantic embedder is not loaded yet; deferring Available registry write"
        );
        return Ok(());
    }

    let Some(input) = active_embedding_registry_input(workspace_id, fast_embedder)? else {
        return Ok(());
    };

    match db.upsert_embedding_metadata_record(&generate_model_registry_id(), &input)? {
        ModelRegistryUpsertOutcome::Inserted => {
            tracing::info!(
                target: "ee::index::embedder",
                provider = provider.as_str(),
                model = fast_embedder.id(),
                dimension = input.dimension,
                content_hash = input.content_hash.as_deref().unwrap_or(""),
                "registered active embedding model registry entry"
            );
        }
        ModelRegistryUpsertOutcome::Updated => {
            tracing::info!(
                target: "ee::index::embedder",
                provider = provider.as_str(),
                model = fast_embedder.id(),
                dimension = input.dimension,
                content_hash = input.content_hash.as_deref().unwrap_or(""),
                "reconciled active embedding model registry entry"
            );
        }
        ModelRegistryUpsertOutcome::Unchanged => {}
    }
    Ok(())
}

fn active_embedding_registry_input(
    workspace_id: &str,
    fast_embedder: &dyn crate::search::Embedder,
) -> Result<Option<crate::db::CreateEmbeddingMetadataInput>, IndexRebuildError> {
    if !fast_embedder.is_semantic() || !fast_embedder.is_ready() {
        return Ok(None);
    }

    let provider = provider_for_embedder(fast_embedder);
    let dimension = u32::try_from(fast_embedder.dimension()).map_err(|_| {
        IndexRebuildError::Index(format!(
            "active embedder dimension {} exceeds model registry bounds",
            fast_embedder.dimension()
        ))
    })?;
    let fingerprint = active_embedder_fingerprint(fast_embedder, provider);
    let mut metadata = EmbeddingMetadataRecord::new(dimension, ModelDistanceMetric::Cosine);
    metadata.pooling = EmbeddingPooling::ModelDefault;
    metadata.tokenizer = Some("tokenizer.json".to_owned());
    metadata.model_revision = Some(fingerprint.revision.clone());
    metadata.deterministic = true;

    Ok(Some(crate::db::CreateEmbeddingMetadataInput {
        workspace_id: workspace_id.to_owned(),
        provider,
        model_name: fast_embedder.id().to_owned(),
        dimension,
        distance_metric: ModelDistanceMetric::Cosine,
        status: ModelRegistryStatus::Available,
        version: metadata.model_revision.clone(),
        source_uri: Some(format!(
            "frankensearch://{provider}/{model}",
            provider = provider.as_str(),
            model = fast_embedder.id()
        )),
        content_hash: Some(fingerprint.content_hash),
        metadata,
        last_checked_at: None,
    }))
}

struct ActiveEmbedderFingerprint {
    revision: String,
    content_hash: String,
}

fn active_embedder_fingerprint(
    embedder: &dyn crate::search::Embedder,
    provider: ModelProvider,
) -> ActiveEmbedderFingerprint {
    let manifest = manifest_for_embedder(embedder, provider);
    let content_hash = active_embedder_content_hash(embedder, provider, manifest.as_ref());
    let revision = manifest
        .as_ref()
        .map(|manifest| manifest.revision.trim())
        .filter(|revision| !revision.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| derived_revision_from_content_hash(&content_hash));
    ActiveEmbedderFingerprint {
        revision,
        content_hash,
    }
}

fn active_embedder_content_hash(
    embedder: &dyn crate::search::Embedder,
    provider: ModelProvider,
    manifest: Option<&frankensearch::embed::ModelManifest>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_fingerprint_field(&mut hasher, "schema", EMBEDDING_REGISTRY_FINGERPRINT_SCHEMA);
    hash_fingerprint_field(&mut hasher, "provider", provider.as_str());
    hash_fingerprint_field(&mut hasher, "model_id", embedder.id());
    hash_fingerprint_field(&mut hasher, "model_name", embedder.model_name());
    hash_fingerprint_field(&mut hasher, "dimension", &embedder.dimension().to_string());
    hash_fingerprint_field(&mut hasher, "category", &embedder.category().to_string());
    hash_fingerprint_field(
        &mut hasher,
        "semantic",
        if embedder.is_semantic() {
            "true"
        } else {
            "false"
        },
    );
    hash_fingerprint_field(
        &mut hasher,
        "ready",
        if embedder.is_ready() { "true" } else { "false" },
    );

    if let Some(manifest) = manifest {
        hash_fingerprint_field(&mut hasher, "manifest_id", &manifest.id);
        hash_fingerprint_field(&mut hasher, "manifest_version", &manifest.version);
        hash_fingerprint_field(&mut hasher, "manifest_repo", &manifest.repo);
        hash_fingerprint_field(&mut hasher, "manifest_revision", &manifest.revision);
        hash_fingerprint_field(&mut hasher, "manifest_license", &manifest.license);
        if let Some(dimension) = manifest.dimension {
            hash_fingerprint_field(&mut hasher, "manifest_dimension", &dimension.to_string());
        }
        let mut files = manifest.files.clone();
        files.sort_by(|left, right| left.name.cmp(&right.name));
        for file in files {
            hash_fingerprint_field(&mut hasher, "manifest_file_name", &file.name);
            hash_fingerprint_field(&mut hasher, "manifest_file_sha256", &file.sha256);
            hash_fingerprint_field(&mut hasher, "manifest_file_size", &file.size.to_string());
        }
    }

    format!("blake3:{}", hasher.finalize().to_hex())
}

fn hash_fingerprint_field(hasher: &mut blake3::Hasher, name: &str, value: &str) {
    hasher.update(name.as_bytes());
    hasher.update(&[0]);
    hasher.update(value.as_bytes());
    hasher.update(&[0xff]);
}

fn derived_revision_from_content_hash(content_hash: &str) -> String {
    let digest = content_hash.strip_prefix("blake3:").unwrap_or(content_hash);
    let prefix: String = digest.chars().take(16).collect();
    format!("derived-blake3-{prefix}")
}

fn manifest_for_embedder(
    embedder: &dyn crate::search::Embedder,
    provider: ModelProvider,
) -> Option<frankensearch::embed::ModelManifest> {
    match provider {
        ModelProvider::Model2Vec | ModelProvider::FastEmbed => {
            let normalized_id = normalized_embedder_manifest_key(embedder.id());
            frankensearch::embed::ModelManifest::builtin_catalog()
                .models
                .into_iter()
                .find(|manifest| {
                    manifest.dimension == u32::try_from(embedder.dimension()).ok()
                        && (normalized_embedder_manifest_key(&manifest.id) == normalized_id
                            || normalized_embedder_manifest_key(&manifest.repo)
                                .contains(&normalized_id)
                            || manifest
                                .display_name
                                .as_deref()
                                .is_some_and(|display_name| {
                                    normalized_embedder_manifest_key(display_name)
                                        .contains(&normalized_id)
                                }))
                })
        }
        _ => None,
    }
}

fn normalized_embedder_manifest_key(input: &str) -> String {
    input
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn provider_for_embedder(embedder: &dyn crate::search::Embedder) -> ModelProvider {
    match embedder.category() {
        ModelCategory::HashEmbedder => ModelProvider::Hash,
        ModelCategory::StaticEmbedder => ModelProvider::Model2Vec,
        ModelCategory::TransformerEmbedder => ModelProvider::FastEmbed,
        ModelCategory::ApiEmbedder => ModelProvider::External,
    }
}

fn generate_model_registry_id() -> String {
    let memory_id = MemoryId::now().to_string();
    let payload = memory_id.trim_start_matches("mem_");
    format!("mdl_{payload}")
}

fn reembed_embedding_summary(
    db: &DbConnection,
    workspace_id: &str,
    stack: &EmbedderStack,
    vector_coverage: EmbeddingVectorCoverage,
) -> Result<ReembedEmbeddingSummary, IndexRebuildError> {
    Ok(ReembedEmbeddingSummary::from_posture(
        embedding_posture_from_stack(db, workspace_id, stack, vector_coverage)?,
    ))
}

pub(crate) fn current_embedding_posture(
    db: &DbConnection,
    workspace_id: &str,
    index_dir: &Path,
) -> Result<EmbeddingPosture, DbError> {
    let stack = default_search_embedder_stack();
    let documents_total = current_indexable_document_count(db, workspace_id)?;
    let vector_coverage =
        embedding_vector_coverage(index_dir, documents_total, read_fast_vector_record_count);
    embedding_posture_from_stack(db, workspace_id, &stack, vector_coverage)
}

pub(crate) fn embedding_posture_from_stack(
    db: &DbConnection,
    workspace_id: &str,
    stack: &EmbedderStack,
    vector_coverage: EmbeddingVectorCoverage,
) -> Result<EmbeddingPosture, DbError> {
    let fast_embedder = stack.fast();
    let quality_embedder = stack.quality();
    let records = db.list_embedding_metadata_records(workspace_id)?;
    Ok(embedding_posture_from_records(
        fast_embedder,
        quality_embedder,
        &records,
        vector_coverage,
    ))
}

fn embedding_posture_from_records(
    fast_embedder: &dyn crate::search::Embedder,
    quality_embedder: Option<&dyn crate::search::Embedder>,
    records: &[crate::db::StoredEmbeddingMetadataRecord],
    vector_coverage: EmbeddingVectorCoverage,
) -> EmbeddingPosture {
    let selected_registry_model = records
        .iter()
        .find(|record| record.registry.status.as_str() == "available")
        .map(|record| EmbeddingPostureRegistryModel {
            id: record.registry.id.clone(),
            provider: record.registry.provider.as_str().to_owned(),
            model_name: record.registry.model_name.clone(),
            status: record.registry.status.as_str().to_owned(),
            dimension: record.metadata.dimension,
            deterministic: record.metadata.deterministic,
        });
    let available_model_count = records
        .iter()
        .filter(|record| record.registry.status.as_str() == "available")
        .count();
    let semantic = fast_embedder.is_semantic()
        || quality_embedder.is_some_and(|embedder| embedder.is_semantic());
    let pending_local_download =
        !semantic && embedder_reports_pending_model2vec_download(fast_embedder);
    let source = if semantic && selected_registry_model.is_some() {
        "registry_observed"
    } else if semantic {
        "neural_local"
    } else if pending_local_download {
        "ee_model2vec_download_pending"
    } else {
        "frankensearch_hash_fallback"
    };
    let mode = if semantic {
        EMBEDDING_POSTURE_MODE_NEURAL_LOCAL
    } else if pending_local_download {
        EMBEDDING_POSTURE_MODE_NEURAL_LOCAL_PENDING
    } else {
        EMBEDDING_POSTURE_MODE_DETERMINISTIC_HASH
    };

    EmbeddingPosture {
        schema: EMBEDDING_POSTURE_SCHEMA_V1,
        mode,
        semantic,
        source: source.to_owned(),
        fast_model_id: fast_embedder.id().to_owned(),
        fast_dimension: fast_embedder.dimension(),
        quality_model_id: quality_embedder.map(|embedder| embedder.id().to_owned()),
        quality_dimension: quality_embedder.map(|embedder| embedder.dimension()),
        deterministic: true,
        registered_model_count: records.len(),
        available_model_count,
        selected_registry_model,
        vector_coverage,
    }
}

fn embedding_vector_coverage(
    index_dir: &Path,
    documents_total: u32,
    read_embedded: impl FnOnce(&Path) -> Option<usize>,
) -> EmbeddingVectorCoverage {
    EmbeddingVectorCoverage::new(
        read_embedded(index_dir).unwrap_or(0),
        usize::try_from(documents_total).unwrap_or(usize::MAX),
    )
}

fn read_fast_vector_record_count(index_dir: &Path) -> Option<usize> {
    open_fast_vector_index(index_dir)
        .ok()
        .map(|index| index.record_count())
}

fn current_indexable_document_count(db: &DbConnection, workspace_id: &str) -> Result<u32, DbError> {
    let memories = db.list_memories_for_retrieval_with_global(workspace_id, None, false)?;
    let sessions = db.list_sessions(workspace_id)?;
    let artifacts = db.count_artifacts(workspace_id)?;
    let memory_count = u32::try_from(memories.len()).unwrap_or(u32::MAX);
    let session_count = u32::try_from(sessions.len()).unwrap_or(u32::MAX);
    Ok(memory_count
        .saturating_add(session_count)
        .saturating_add(artifacts))
}

fn reembed_idempotency_key(
    workspace_id: &str,
    fast_model_id: &str,
    quality_model_id: Option<&str>,
    documents_total: u32,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.index_reembed.v1\0");
    hasher.update(workspace_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(fast_model_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(quality_model_id.unwrap_or("").as_bytes());
    hasher.update(b"\0");
    hasher.update(documents_total.to_string().as_bytes());
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn generate_search_index_job_id() -> String {
    let memory_id = MemoryId::now().to_string();
    let payload = memory_id.trim_start_matches("mem_");
    format!("sidx_{payload}")
}

// ============================================================================
// Index Status / Diagnostics (EE-242)
// ============================================================================

/// Options for `ee index status`.
#[derive(Clone, Debug)]
pub struct IndexStatusOptions {
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
    pub index_dir: Option<PathBuf>,
}

impl IndexStatusOptions {
    fn resolve_database_path(&self) -> PathBuf {
        self.database_path
            .clone()
            .unwrap_or_else(|| default_workspace_database_path(&self.workspace_path))
    }

    fn resolve_index_dir(&self) -> PathBuf {
        self.index_dir
            .clone()
            .unwrap_or_else(|| default_workspace_index_dir(&self.workspace_path))
    }
}

/// Options for `ee index vacuum`.
#[derive(Clone, Debug)]
pub struct IndexVacuumOptions {
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
    pub index_dir: Option<PathBuf>,
}

impl IndexVacuumOptions {
    fn resolve_database_path(&self) -> PathBuf {
        self.database_path
            .clone()
            .unwrap_or_else(|| default_workspace_database_path(&self.workspace_path))
    }

    fn resolve_index_dir(&self) -> PathBuf {
        self.index_dir
            .clone()
            .unwrap_or_else(|| default_workspace_index_dir(&self.workspace_path))
    }
}

/// Health classification for the search index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexHealth {
    /// Index exists and is up to date with the database.
    Ready,
    /// Index exists but database has newer records.
    Stale,
    /// Index directory does not exist or is empty.
    Missing,
    /// Index exists but failed integrity checks.
    Corrupt,
}

impl IndexHealth {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Stale => "stale",
            Self::Missing => "missing",
            Self::Corrupt => "corrupt",
        }
    }

    #[must_use]
    pub const fn degradation_code(self) -> Option<&'static str> {
        match self {
            Self::Ready => None,
            Self::Stale => Some("index_stale"),
            Self::Missing => Some("index_missing"),
            Self::Corrupt => Some("index_corrupt"),
        }
    }
}

/// Read-only outcome classification for `ee index vacuum`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexVacuumStatus {
    Ready,
    Preview,
    Missing,
    Stale,
    Locked,
    Corrupt,
}

impl IndexVacuumStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Preview => "preview",
            Self::Missing => "missing",
            Self::Stale => "stale",
            Self::Locked => "locked",
            Self::Corrupt => "corrupt",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexVacuumCandidateKind {
    IncompleteStaging,
    StagedGeneration,
    RetainedGeneration,
}

impl IndexVacuumCandidateKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IncompleteStaging => "incomplete_staging",
            Self::StagedGeneration => "staged_generation",
            Self::RetainedGeneration => "retained_generation",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexPathStats {
    pub path: PathBuf,
    pub exists: bool,
    pub file_count: u32,
    pub directory_count: u32,
    pub size_bytes: u64,
}

impl IndexPathStats {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "path": self.path.to_string_lossy(),
            "exists": self.exists,
            "fileCount": self.file_count,
            "directoryCount": self.directory_count,
            "sizeBytes": self.size_bytes,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexVacuumCandidate {
    pub path: PathBuf,
    pub kind: IndexVacuumCandidateKind,
    pub stats: IndexPathStats,
}

impl IndexVacuumCandidate {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "path": self.path.to_string_lossy(),
            "kind": self.kind.as_str(),
            "plannedAction": "report_reclaimable_derived_asset",
            "requiresExplicitOperatorAction": true,
            "stats": self.stats.data_json(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexVacuumDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: &'static str,
    pub repair: &'static str,
}

impl IndexVacuumDegradation {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code,
            "severity": self.severity,
            "message": self.message,
            "repair": self.repair,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexVacuumLockReport {
    pub held: bool,
    pub lock_id: Option<String>,
    pub holder_id: Option<String>,
    pub acquired_at: Option<String>,
    pub expires_at: Option<String>,
    pub reason: Option<String>,
}

impl IndexVacuumLockReport {
    #[must_use]
    pub fn none() -> Self {
        Self {
            held: false,
            lock_id: None,
            holder_id: None,
            acquired_at: None,
            expires_at: None,
            reason: None,
        }
    }

    #[must_use]
    pub fn for_lock_id(lock_id: String) -> Self {
        Self {
            held: false,
            lock_id: Some(lock_id),
            holder_id: None,
            acquired_at: None,
            expires_at: None,
            reason: None,
        }
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "held": self.held,
            "lockId": self.lock_id,
            "holderId": self.holder_id,
            "acquiredAt": self.acquired_at,
            "expiresAt": self.expires_at,
            "reason": self.reason,
        })
    }
}

/// Preview report for `ee index vacuum`.
#[derive(Clone, Debug)]
pub struct IndexVacuumReport {
    pub status: IndexVacuumStatus,
    pub database_path: PathBuf,
    pub index_dir: PathBuf,
    pub before: IndexPathStats,
    pub after: IndexPathStats,
    pub candidate_count: u32,
    pub reclaimable_bytes: u64,
    pub candidates: Vec<IndexVacuumCandidate>,
    pub degraded: Vec<IndexVacuumDegradation>,
    pub lock: IndexVacuumLockReport,
    pub elapsed_ms: f64,
}

impl IndexVacuumReport {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = String::new();
        output.push_str(&format!(
            "Index vacuum: {} (preview only)\n\n",
            self.status.as_str().to_ascii_uppercase()
        ));
        output.push_str(&format!(
            "  Index directory: {}\n",
            self.index_dir.display()
        ));
        output.push_str(&format!("  Database: {}\n", self.database_path.display()));
        output.push_str("  Mutation allowed: false\n");
        output.push_str(&format!(
            "  Active index files: {}\n",
            self.before.file_count
        ));
        output.push_str(&format!(
            "  Active index size: {}\n",
            format_bytes(self.before.size_bytes)
        ));
        output.push_str(&format!("  Vacuum candidates: {}\n", self.candidate_count));
        output.push_str(&format!(
            "  Reclaimable preview: {}\n",
            format_bytes(self.reclaimable_bytes)
        ));
        output.push_str(&format!("  Lock held: {}\n", self.lock.held));
        output.push_str(&format!("  Elapsed: {:.1}ms\n", self.elapsed_ms));

        if !self.degraded.is_empty() {
            output.push_str("\nDegraded:\n");
            for degraded in &self.degraded {
                output.push_str(&format!(
                    "  - {}: {} Repair: {}\n",
                    degraded.code, degraded.message, degraded.repair
                ));
            }
        }

        if !self.candidates.is_empty() {
            output.push_str("\nCandidates:\n");
            for candidate in &self.candidates {
                output.push_str(&format!(
                    "  - {} {} ({})\n",
                    candidate.kind.as_str(),
                    candidate.path.display(),
                    format_bytes(candidate.stats.size_bytes)
                ));
            }
        }

        output
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let candidates = self
            .candidates
            .iter()
            .map(IndexVacuumCandidate::data_json)
            .collect::<Vec<_>>();
        let degraded = index_vacuum_degraded_data_json(&self.degraded);
        serde_json::json!({
            "command": "index_vacuum",
            "schema": "ee.index.vacuum.v1",
            "status": self.status.as_str(),
            "dryRun": true,
            "previewOnly": true,
            "mutationAllowed": false,
            "databasePath": self.database_path.to_string_lossy(),
            "indexDir": self.index_dir.to_string_lossy(),
            "before": self.before.data_json(),
            "after": self.after.data_json(),
            "candidateCount": self.candidate_count,
            "reclaimableBytes": self.reclaimable_bytes,
            "candidates": candidates,
            "degraded": degraded,
            "lock": self.lock.data_json(),
            "elapsedMs": self.elapsed_ms,
        })
    }
}

fn index_vacuum_degraded_data_json(degraded: &[IndexVacuumDegradation]) -> Vec<serde_json::Value> {
    aggregate_degraded_entries(degraded.iter().map(|entry| {
        DegradationAggregationInput::new(
            "index_vacuum",
            entry.code,
            entry.severity,
            entry.message,
            entry.repair,
        )
    }))
    .into_iter()
    .map(|entry| {
        serde_json::json!({
            "code": entry.code,
            "severity": entry.severity,
            "message": entry.message,
            "repair": entry.repair,
            "sources": entry.sources,
        })
    })
    .collect()
}

/// Diagnostic report for `ee index status`.
#[derive(Clone, Debug)]
pub struct IndexStatusReport {
    pub health: IndexHealth,
    pub index_dir: PathBuf,
    pub database_path: PathBuf,
    pub embedding: Option<EmbeddingPosture>,
    pub index_exists: bool,
    pub index_file_count: u32,
    pub index_size_bytes: u64,
    pub db_memory_count: u32,
    pub db_session_count: u32,
    pub db_generation: Option<u64>,
    pub index_generation: Option<u64>,
    pub last_rebuild_at: Option<String>,
    pub last_check_error: Option<String>,
    pub repair_hint: Option<&'static str>,
    pub elapsed_ms: f64,
}

impl IndexStatusReport {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = String::new();

        let status_line = match self.health {
            IndexHealth::Ready => "Index status: READY\n\n",
            IndexHealth::Stale => "Index status: STALE (rebuild recommended)\n\n",
            IndexHealth::Missing => "Index status: MISSING (rebuild required)\n\n",
            IndexHealth::Corrupt => "Index status: CORRUPT (rebuild required)\n\n",
        };
        output.push_str(status_line);

        output.push_str(&format!(
            "  Index directory: {}\n",
            self.index_dir.display()
        ));
        output.push_str(&format!("  Database: {}\n", self.database_path.display()));
        output.push_str(&format!("  Index exists: {}\n", self.index_exists));

        if self.index_exists {
            output.push_str(&format!("  Index files: {}\n", self.index_file_count));
            output.push_str(&format!(
                "  Index size: {}\n",
                format_bytes(self.index_size_bytes)
            ));
        }

        output.push_str(&format!("  DB memories: {}\n", self.db_memory_count));
        output.push_str(&format!("  DB sessions: {}\n", self.db_session_count));

        if let (Some(db_gen), Some(idx_gen)) = (self.db_generation, self.index_generation) {
            output.push_str(&format!("  DB generation: {db_gen}\n"));
            output.push_str(&format!("  Index generation: {idx_gen}\n"));
        }

        if let Some(ref timestamp) = self.last_rebuild_at {
            output.push_str(&format!("  Last rebuild: {timestamp}\n"));
        }

        if let Some(ref error) = self.last_check_error {
            output.push_str(&format!("  Last check error: {error}\n"));
        }

        output.push_str(&format!("  Elapsed: {:.1}ms\n", self.elapsed_ms));

        if let Some(hint) = self.repair_hint {
            output.push_str(&format!("\nNext:\n  {hint}\n"));
        }

        output
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let degraded = self
            .degraded()
            .into_iter()
            .map(IndexStatusDegradation::data_json)
            .collect::<Vec<_>>();
        serde_json::json!({
            "command": "index_status",
            "health": self.health.as_str(),
            "degradationCode": self.health.degradation_code(),
            "degraded": degraded,
            "indexDir": self.index_dir.to_string_lossy(),
            "databasePath": self.database_path.to_string_lossy(),
            "embedding": self.embedding.as_ref().map(EmbeddingPosture::data_json),
            "indexExists": self.index_exists,
            "indexFileCount": self.index_file_count,
            "indexSizeBytes": self.index_size_bytes,
            "dbMemoryCount": self.db_memory_count,
            "dbSessionCount": self.db_session_count,
            "dbGeneration": self.db_generation,
            "indexGeneration": self.index_generation,
            "lastRebuildAt": self.last_rebuild_at,
            "lastCheckError": self.last_check_error,
            "repairHint": self.repair_hint,
            "elapsedMs": self.elapsed_ms,
        })
    }

    fn degraded(&self) -> Option<IndexStatusDegradation> {
        let repair = self
            .repair_hint
            .unwrap_or("ee index rebuild --workspace .")
            .to_owned();
        match self.health {
            IndexHealth::Ready => None,
            IndexHealth::Stale => Some(IndexStatusDegradation {
                code: "index_stale",
                severity: "high",
                message: "Search index is stale.",
                repair,
            }),
            IndexHealth::Missing => Some(IndexStatusDegradation {
                code: "index_missing",
                severity: "medium",
                message: "Search index is missing.",
                repair,
            }),
            IndexHealth::Corrupt => Some(IndexStatusDegradation {
                code: "index_corrupt",
                severity: "high",
                message: "Search index metadata is corrupt.",
                repair,
            }),
        }
    }
}

struct IndexStatusDegradation {
    code: &'static str,
    severity: &'static str,
    message: &'static str,
    repair: String,
}

impl IndexStatusDegradation {
    fn data_json(self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code,
            "severity": self.severity,
            "message": self.message,
            "repair": self.repair,
        })
    }
}

/// Error from index status check.
#[derive(Debug)]
pub enum IndexStatusError {
    Database(DbError),
    Io(std::io::Error),
}

impl IndexStatusError {
    #[must_use]
    pub fn repair_hint(&self) -> Option<&str> {
        match self {
            Self::Database(_) => Some("ee doctor --json"),
            Self::Io(_) => Some("Check workspace path permissions"),
        }
    }
}

impl std::fmt::Display for IndexStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(e) => write!(f, "Database error: {e}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for IndexStatusError {}

impl From<DbError> for IndexStatusError {
    fn from(e: DbError) -> Self {
        Self::Database(e)
    }
}

impl From<std::io::Error> for IndexStatusError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Get the current status of the search index.
pub fn get_index_status(
    options: &IndexStatusOptions,
) -> Result<IndexStatusReport, IndexStatusError> {
    get_index_status_with_connection(options, None)
}

pub(crate) fn get_index_status_with_connection(
    options: &IndexStatusOptions,
    connection: Option<&DbConnection>,
) -> Result<IndexStatusReport, IndexStatusError> {
    let start = Instant::now();
    let database_path = options.resolve_database_path();
    let index_dir = options.resolve_index_dir();

    // Check index directory
    let (index_exists, index_file_count, index_size_bytes) = inspect_index_dir(&index_dir)?;

    // Fast-path degraded states: when the index is missing/corrupt, we can
    // report health without scanning DB tables for counts/generation.
    let (db_memory_count, db_session_count, db_generation, embedding) = if !index_exists
        || index_file_count == 0
    {
        (0, 0, None, None)
    } else if database_path.exists() {
        let owned_connection;
        let db = if let Some(connection) = connection {
            connection
        } else {
            owned_connection = DbConnection::open_file(&database_path)?;
            &owned_connection
        };
        let (memory_count, session_count, generation) = get_db_stats(db)?;
        let embedding = index_status_embedding_posture(db, &options.workspace_path, &index_dir)?;
        (memory_count, session_count, generation, embedding)
    } else {
        (0, 0, None, None)
    };

    // Read index metadata if available.
    let (index_generation, last_rebuild_at, last_check_error) = read_index_metadata(&index_dir);

    // Determine health
    let health = determine_health(
        index_exists,
        index_file_count,
        db_generation,
        index_generation,
        last_check_error.is_some(),
    );

    let repair_hint = match health {
        IndexHealth::Ready => None,
        IndexHealth::Stale | IndexHealth::Missing | IndexHealth::Corrupt => {
            Some("ee index rebuild --workspace .")
        }
    };

    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    let report = IndexStatusReport {
        health,
        index_dir,
        database_path,
        embedding,
        index_exists,
        index_file_count,
        index_size_bytes,
        db_memory_count,
        db_session_count,
        db_generation,
        index_generation,
        last_rebuild_at,
        last_check_error,
        repair_hint,
        elapsed_ms,
    };
    log_db_generation_observed(&report);
    Ok(report)
}

fn index_status_embedding_posture(
    db: &DbConnection,
    workspace_path: &Path,
    index_dir: &Path,
) -> Result<Option<EmbeddingPosture>, IndexStatusError> {
    let Some(workspace_id) = workspace_id_for_index_status(db, workspace_path)? else {
        return Ok(None);
    };
    Ok(Some(current_embedding_posture(
        db,
        &workspace_id,
        index_dir,
    )?))
}

fn workspace_id_for_index_status(
    db: &DbConnection,
    workspace_path: &Path,
) -> Result<Option<String>, DbError> {
    let canonical_root = default_workspace_root(workspace_path);
    let canonical_key = canonical_root.to_string_lossy();
    if let Some(workspace) = db.get_workspace_by_path(canonical_key.as_ref())? {
        return Ok(Some(workspace.id));
    }

    let lexical_key = workspace_path.to_string_lossy();
    if lexical_key != canonical_key {
        if let Some(workspace) = db.get_workspace_by_path(lexical_key.as_ref())? {
            return Ok(Some(workspace.id));
        }
    }

    Ok(None)
}

fn log_db_generation_observed(report: &IndexStatusReport) {
    crate::obs::log_event(
        crate::obs::TestEvent::new(
            crate::obs::test_id_or("db_generation_observed"),
            crate::obs::EventKind::DbGenerationObserved,
        )
        .with_field(
            "command",
            serde_json::Value::String("index_status".to_owned()),
        )
        .with_field(
            "health",
            serde_json::Value::String(report.health.as_str().to_owned()),
        )
        .with_field(
            "db_generation",
            serde_json::Value::from(report.db_generation),
        )
        .with_field(
            "index_generation",
            serde_json::Value::from(report.index_generation),
        )
        .with_field(
            "index_dir",
            serde_json::Value::String(report.index_dir.to_string_lossy().into_owned()),
        )
        .with_field(
            "database_path",
            serde_json::Value::String(report.database_path.to_string_lossy().into_owned()),
        ),
    );
}

/// Preview search-index vacuum work without mutating the source DB or derived assets.
pub fn get_index_vacuum_report(
    options: &IndexVacuumOptions,
) -> Result<IndexVacuumReport, IndexStatusError> {
    let start = Instant::now();
    let database_path = options.resolve_database_path();
    let index_dir = options.resolve_index_dir();
    let status_report = get_index_status(&IndexStatusOptions {
        workspace_path: options.workspace_path.clone(),
        database_path: Some(database_path.clone()),
        index_dir: Some(index_dir.clone()),
    })?;

    let before = collect_index_path_stats(&index_dir)?;
    let after = before.clone();
    let candidates = discover_index_vacuum_candidates(&index_dir)?;
    let lock = inspect_index_vacuum_lock(&database_path, &options.workspace_path)?;
    let reclaimable_bytes = candidates.iter().fold(0_u64, |total, candidate| {
        total.saturating_add(candidate.stats.size_bytes)
    });
    let candidate_count = u32::try_from(candidates.len()).unwrap_or(u32::MAX);
    let degraded = index_vacuum_degradations(status_report.health, lock.held);
    let status = if lock.held {
        IndexVacuumStatus::Locked
    } else {
        match status_report.health {
            IndexHealth::Ready if candidates.is_empty() => IndexVacuumStatus::Ready,
            IndexHealth::Ready => IndexVacuumStatus::Preview,
            IndexHealth::Missing => IndexVacuumStatus::Missing,
            IndexHealth::Stale => IndexVacuumStatus::Stale,
            IndexHealth::Corrupt => IndexVacuumStatus::Corrupt,
        }
    };

    Ok(IndexVacuumReport {
        status,
        database_path,
        index_dir,
        before,
        after,
        candidate_count,
        reclaimable_bytes,
        candidates,
        degraded,
        lock,
        elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
    })
}

fn index_vacuum_degradations(health: IndexHealth, lock_held: bool) -> Vec<IndexVacuumDegradation> {
    let mut degraded = Vec::new();
    if lock_held {
        degraded.push(IndexVacuumDegradation {
            code: "index_locked",
            severity: "medium",
            message: "An index publish lock is currently held.",
            repair: "Wait for the active index operation to finish, then retry ee index vacuum --workspace . --json.",
        });
    }
    match health {
        IndexHealth::Ready => {}
        IndexHealth::Missing => degraded.push(IndexVacuumDegradation {
            code: "index_missing",
            severity: "medium",
            message: "The derived search index is missing or empty.",
            repair: "ee index rebuild --workspace .",
        }),
        IndexHealth::Stale => degraded.push(IndexVacuumDegradation {
            code: "index_stale",
            severity: "medium",
            message: "The derived search index is behind the FrankenSQLite source generation.",
            repair: "ee index rebuild --workspace .",
        }),
        IndexHealth::Corrupt => degraded.push(IndexVacuumDegradation {
            code: "index_corrupt",
            severity: "high",
            message: "The derived search index metadata failed integrity checks.",
            repair: "ee index rebuild --workspace .",
        }),
    }
    degraded
}

fn collect_index_path_stats(path: &Path) -> Result<IndexPathStats, std::io::Error> {
    let mut stats = IndexPathStats {
        path: path.to_path_buf(),
        exists: path.exists(),
        file_count: 0,
        directory_count: 0,
        size_bytes: 0,
    };
    if !stats.exists {
        return Ok(stats);
    }
    collect_index_path_stats_inner(path, &mut stats)?;
    Ok(stats)
}

fn collect_index_path_stats_inner(
    path: &Path,
    stats: &mut IndexPathStats,
) -> Result<(), std::io::Error> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        stats.directory_count = stats.directory_count.saturating_add(1);
        for entry in std::fs::read_dir(path)? {
            collect_index_path_stats_inner(&entry?.path(), stats)?;
        }
    } else {
        stats.file_count = stats.file_count.saturating_add(1);
        stats.size_bytes = stats.size_bytes.saturating_add(metadata.len());
    }
    Ok(())
}

fn discover_index_vacuum_candidates(
    index_dir: &Path,
) -> Result<Vec<IndexVacuumCandidate>, std::io::Error> {
    let parent = index_parent(index_dir);
    if !parent.exists() {
        return Ok(Vec::new());
    }

    let base = index_vacuum_base_name(index_dir)?;
    let staging_prefix = format!(".{base}{INDEX_STAGING_PREFIX}");
    let retained_prefix = format!("{base}{INDEX_RETAINED_SUFFIX}");
    let mut candidates = Vec::new();

    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        let entry_path = entry.path();
        if entry_path == index_dir {
            continue;
        }
        if !entry.file_type().map(|ty| ty.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let kind = if name.starts_with(&staging_prefix) {
            if entry_path.join(INDEX_METADATA_FILE).is_file() {
                IndexVacuumCandidateKind::StagedGeneration
            } else {
                IndexVacuumCandidateKind::IncompleteStaging
            }
        } else if name == retained_prefix
            || name
                .strip_prefix(&retained_prefix)
                .is_some_and(|suffix| suffix.starts_with('.'))
        {
            IndexVacuumCandidateKind::RetainedGeneration
        } else {
            continue;
        };
        let stats = collect_index_path_stats(&entry_path)?;
        candidates.push(IndexVacuumCandidate {
            path: entry_path,
            kind,
            stats,
        });
    }

    candidates.sort_by(|left, right| {
        left.path
            .to_string_lossy()
            .cmp(&right.path.to_string_lossy())
            .then_with(|| left.kind.as_str().cmp(right.kind.as_str()))
    });
    Ok(candidates)
}

fn index_vacuum_base_name(index_dir: &Path) -> Result<String, std::io::Error> {
    index_dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "index directory must have a final path component: {}",
                    index_dir.display()
                ),
            )
        })
}

fn inspect_index_vacuum_lock(
    database_path: &Path,
    workspace_path: &Path,
) -> Result<IndexVacuumLockReport, IndexStatusError> {
    if !database_path.exists() {
        return Ok(IndexVacuumLockReport::none());
    }
    let db = DbConnection::open_file(database_path)?;
    let Some(workspace_id) = workspace_id_for_index_vacuum(&db, workspace_path)? else {
        return Ok(IndexVacuumLockReport::none());
    };
    let lock_id = AdvisoryLockId::index(&workspace_id);
    let canonical_lock_id = lock_id.canonical_key();
    let table_rows = db.query(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'ee_advisory_locks'",
        &[],
    )?;
    if table_rows.is_empty() {
        return Ok(IndexVacuumLockReport::for_lock_id(canonical_lock_id));
    }

    let rows = db.query(
        "SELECT holder_id, acquired_at, expires_at, reason
         FROM ee_advisory_locks
         WHERE resource_type = ?1 AND resource_id = ?2
         ORDER BY acquired_at DESC, resource_key ASC",
        &[
            SqlValue::Text(lock_id.resource_type().to_owned()),
            SqlValue::Text(lock_id.resource_id().to_owned()),
        ],
    )?;

    for row in rows {
        let expires_at = row
            .get(2)
            .and_then(|value| value.as_str())
            .map(str::to_owned);
        if expires_at.as_deref().is_some_and(index_vacuum_lock_expired) {
            continue;
        }
        return Ok(IndexVacuumLockReport {
            held: true,
            lock_id: Some(canonical_lock_id),
            holder_id: row
                .get(0)
                .and_then(|value| value.as_str())
                .map(str::to_owned),
            acquired_at: row
                .get(1)
                .and_then(|value| value.as_str())
                .map(str::to_owned),
            expires_at,
            reason: row
                .get(3)
                .and_then(|value| value.as_str())
                .map(str::to_owned),
        });
    }

    Ok(IndexVacuumLockReport::for_lock_id(canonical_lock_id))
}

fn workspace_id_for_index_vacuum(
    db: &DbConnection,
    workspace_path: &Path,
) -> Result<Option<String>, DbError> {
    match workspace_id_for_index_status(db, workspace_path) {
        Ok(workspace_id) => Ok(workspace_id),
        Err(error) if db_error_mentions_missing_table(&error, "workspaces") => Ok(None),
        Err(error) => Err(error),
    }
}

fn db_error_mentions_missing_table(error: &DbError, table: &str) -> bool {
    let message = error.to_string();
    message.contains("no such table") && message.contains(table)
}

fn index_vacuum_lock_expired(expires_at: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(expires_at)
        .map(|expires_at| expires_at <= chrono::Utc::now())
        .unwrap_or(false)
}

fn inspect_index_dir(index_dir: &Path) -> Result<(bool, u32, u64), std::io::Error> {
    if !index_dir.exists() {
        return Ok((false, 0, 0));
    }

    let mut file_count = 0_u32;
    let mut total_size = 0_u64;

    for entry in std::fs::read_dir(index_dir)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_file() {
            file_count = file_count.saturating_add(1);
            total_size = total_size.saturating_add(metadata.len());
        }
    }

    Ok((true, file_count, total_size))
}

fn get_db_stats(db: &DbConnection) -> Result<(u32, u32, Option<u64>), DbError> {
    let memory_count = db
        .query("SELECT COUNT(*) FROM memories", &[])?
        .first()
        .and_then(|row| row.get(0).and_then(|v| v.as_i64()))
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);

    let session_count = db
        .query("SELECT COUNT(*) FROM sessions", &[])?
        .first()
        .and_then(|row| row.get(0).and_then(|v| v.as_i64()))
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);

    let artifact_count = db
        .query("SELECT COUNT(*) FROM artifacts", &[])
        .ok()
        .and_then(|rows| {
            rows.first()
                .and_then(|row| row.get(0).and_then(|v| v.as_i64()))
        })
        .and_then(|v| u64::try_from(v).ok())
        .unwrap_or(0);

    let source_document_count = u64::from(memory_count) + u64::from(session_count) + artifact_count;

    // Audit rows track audited updates; source document count covers fixtures and
    // older repository writes that predate full audit coverage. Read-surface
    // audit rows are deliberately excluded: they are access metadata, not
    // search-indexable source mutations, and must not make read-only commands
    // mark the index stale on the next invocation.
    let audit_count = db
        .query(
            "SELECT COUNT(*) FROM audit_log WHERE action NOT IN (?1, ?2, ?3, ?4, ?5, ?6)",
            &READ_SURFACE_AUDIT_ACTIONS
                .iter()
                .map(|action| SqlValue::Text((*action).to_owned()))
                .collect::<Vec<_>>(),
        )
        .ok()
        .and_then(|rows| {
            rows.first()
                .and_then(|row| row.get(0).and_then(|v| v.as_i64()))
        })
        .and_then(|v| u64::try_from(v).ok())
        .unwrap_or(0);

    let workspace_generation = db
        .query(
            "SELECT COALESCE(MAX(generation), 0) FROM workspace_generations",
            &[],
        )
        .ok()
        .and_then(|rows| {
            rows.first()
                .and_then(|row| row.get(0).and_then(|v| v.as_i64()))
        })
        .and_then(|v| u64::try_from(v).ok());

    let generation = workspace_generation.or(Some(source_document_count.max(audit_count)));

    Ok((memory_count, session_count, generation))
}

fn read_index_metadata(index_dir: &Path) -> (Option<u64>, Option<String>, Option<String>) {
    let meta_path = index_dir.join(INDEX_METADATA_FILE);
    let content = match read_index_metadata_contents(&meta_path) {
        Ok(Some(content)) => content,
        Ok(None) => return (None, None, None),
        Err(error) => return (None, None, Some(error)),
    };

    let parsed: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(error) => {
            return (
                None,
                None,
                Some(format!(
                    "failed to parse index metadata '{}': {error}",
                    meta_path.display()
                )),
            );
        }
    };

    if !parsed.is_object() {
        return (
            None,
            None,
            Some(format!(
                "index metadata '{}' must be a JSON object",
                meta_path.display()
            )),
        );
    }

    let generation = parsed
        .get("sourceGeneration")
        .or_else(|| parsed.get("source_generation"))
        .or_else(|| parsed.get("generation"))
        .and_then(|v| v.as_u64());
    let last_rebuild = parsed
        .get("lastRebuildAt")
        .or_else(|| parsed.get("last_rebuild_at"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    (generation, last_rebuild, None)
}

fn read_index_metadata_contents(meta_path: &Path) -> Result<Option<String>, String> {
    if let Some(component) =
        first_existing_index_symlink_component(meta_path).map_err(|error| error.to_string())?
    {
        return Err(format!(
            "index metadata '{}' traverses symlinked path component '{}'",
            meta_path.display(),
            component.display()
        ));
    }

    let metadata = match std::fs::symlink_metadata(meta_path) {
        Ok(metadata) if metadata.file_type().is_file() => metadata,
        Ok(_) => {
            return Err(format!(
                "index metadata '{}' is not a regular file",
                meta_path.display()
            ));
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(error) => {
            return Err(format!(
                "failed to inspect index metadata '{}': {error}",
                meta_path.display()
            ));
        }
    };

    // Reject an obvious oversize at stat time before opening the file
    // at all — see `INDEX_METADATA_INSPECT_LIMIT` for the threat model.
    if metadata.len() > INDEX_METADATA_INSPECT_LIMIT {
        return Err(format!(
            "index metadata '{}' exceeds the {INDEX_METADATA_INSPECT_LIMIT} byte cap (size={size})",
            meta_path.display(),
            size = metadata.len(),
        ));
    }

    // Bound the read itself so a peer-driven TOCTOU growth between the
    // `symlink_metadata` above and the `File::open` here cannot widen
    // peak allocation beyond `LIMIT + 1` bytes. Same shape as
    // `src/config/workspace.rs::detect_git_worktree` (c8f33694) and
    // `src/core/preflight_guard.rs::read_preflight_rules_file_no_follow`
    // (7f56d89b).
    use std::io::Read as _;
    let file = std::fs::File::open(meta_path).map_err(|error| {
        format!(
            "failed to read index metadata '{}': {error}",
            meta_path.display()
        )
    })?;
    let mut bytes = Vec::new();
    file.take(INDEX_METADATA_INSPECT_LIMIT.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| {
            format!(
                "failed to read index metadata '{}': {error}",
                meta_path.display()
            )
        })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > INDEX_METADATA_INSPECT_LIMIT {
        return Err(format!(
            "index metadata '{}' exceeds the {INDEX_METADATA_INSPECT_LIMIT} byte cap during read",
            meta_path.display(),
        ));
    }
    String::from_utf8(bytes).map(Some).map_err(|error| {
        format!(
            "index metadata '{}' is not valid UTF-8: {error}",
            meta_path.display()
        )
    })
}

fn determine_health(
    index_exists: bool,
    index_file_count: u32,
    db_generation: Option<u64>,
    index_generation: Option<u64>,
    metadata_corrupt: bool,
) -> IndexHealth {
    if metadata_corrupt {
        return IndexHealth::Corrupt;
    }

    if !index_exists || index_file_count == 0 {
        return IndexHealth::Missing;
    }

    match (db_generation, index_generation) {
        (Some(db_gen), Some(idx_gen)) if db_gen > idx_gen => IndexHealth::Stale,
        (Some(_), None) => IndexHealth::Stale, // DB has generation but index doesn't
        _ => IndexHealth::Ready,
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{BUNDLED_EMBEDDING_DIMENSION, BUNDLED_EMBEDDING_MODEL_REVISION};
    use crate::core::profile::OperatingProfile;
    use crate::search::Embedder;
    use proptest::prelude::*;
    use proptest::test_runner::Config as ProptestConfig;

    type TestResult = Result<(), String>;

    #[derive(Debug)]
    struct TestSemanticEmbedder {
        id: String,
        dimension: usize,
        ready: bool,
    }

    impl TestSemanticEmbedder {
        fn new(id: &str, dimension: usize) -> Self {
            Self {
                id: id.to_owned(),
                dimension,
                ready: true,
            }
        }

        fn not_ready(id: &str, dimension: usize) -> Self {
            Self {
                id: id.to_owned(),
                dimension,
                ready: false,
            }
        }
    }

    impl crate::search::Embedder for TestSemanticEmbedder {
        fn embed<'a>(
            &'a self,
            _cx: &'a asupersync::Cx,
            _text: &'a str,
        ) -> frankensearch::SearchFuture<'a, Vec<f32>> {
            Box::pin(async move { Ok(vec![0.0; self.dimension]) })
        }

        fn dimension(&self) -> usize {
            self.dimension
        }

        fn id(&self) -> &str {
            &self.id
        }

        fn model_name(&self) -> &str {
            &self.id
        }

        fn is_ready(&self) -> bool {
            self.ready
        }

        fn is_semantic(&self) -> bool {
            true
        }

        fn category(&self) -> ModelCategory {
            ModelCategory::StaticEmbedder
        }
    }

    fn test_runtime_profile() -> RuntimeProfileReport {
        RuntimeProfileReport::for_profile(OperatingProfile::Workstation, "test_fixture")
    }

    fn fixture_hash_embedding_posture() -> EmbeddingPosture {
        EmbeddingPosture {
            schema: EMBEDDING_POSTURE_SCHEMA_V1,
            mode: EMBEDDING_POSTURE_MODE_DETERMINISTIC_HASH,
            semantic: false,
            source: "frankensearch_hash_fallback".to_owned(),
            fast_model_id: "fnv1a-256".to_owned(),
            fast_dimension: 256,
            quality_model_id: Some("fnv1a-384".to_owned()),
            quality_dimension: Some(384),
            deterministic: true,
            registered_model_count: 0,
            available_model_count: 0,
            selected_registry_model: None,
            vector_coverage: EmbeddingVectorCoverage::new(0, 10),
        }
    }

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    #[test]
    fn hash_fallback_stack_keeps_fast_and_quality_hash_tiers() -> TestResult {
        let stack = hash_fallback_embedder_stack();
        ensure(
            !stack.fast().is_semantic(),
            "fast hash tier is non-semantic",
        )?;
        ensure(
            stack.fast().id() == HashEmbedder::default_256().id(),
            "fast tier should be the 256d hash fallback",
        )?;
        let quality = stack
            .quality()
            .ok_or_else(|| "quality hash fallback should be present".to_owned())?;
        ensure(!quality.is_semantic(), "quality hash tier is non-semantic")?;
        ensure(
            quality.id() == HashEmbedder::default_384().id(),
            "quality tier should be the 384d hash fallback",
        )
    }

    #[test]
    fn hash_fallback_embedding_is_byte_identical_for_fixed_input() -> TestResult {
        let embedder = HashEmbedder::default_256();
        let first = crate::core::run_cli_future(async {
            let cx = asupersync::Cx::for_testing();
            embedder.embed(&cx, "RBLX bookings FCF watchlist").await
        })
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;

        let embedder = HashEmbedder::default_256();
        let second = crate::core::run_cli_future(async {
            let cx = asupersync::Cx::for_testing();
            embedder.embed(&cx, "RBLX bookings FCF watchlist").await
        })
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;

        ensure(first == second, "hash embedding vector must be stable")?;
        ensure(first.len() == 256, "hash embedding vector dimension")
    }

    #[test]
    fn semantic_stack_remains_fast_only_without_hash_quality_graft() -> TestResult {
        let stack = stack_with_hash_quality_fallback(EmbedderStack::from_parts(
            Arc::new(TestSemanticEmbedder::new("potion-multilingual-128M", 256))
                as Arc<dyn crate::search::Embedder>,
            None,
        ));

        ensure(stack.fast().is_semantic(), "fast tier should stay semantic")?;
        ensure(
            stack.quality().is_none(),
            "semantic stack must not graft a hash quality tier",
        )
    }

    #[test]
    fn default_search_embedder_stack_reuses_process_cached_arcs() -> TestResult {
        let first = default_search_embedder_stack();
        let second = default_search_embedder_stack();

        ensure(
            Arc::ptr_eq(&first.fast_arc(), &second.fast_arc()),
            "default search embedder stack should reuse the same fast Arc within one process",
        )
    }

    #[test]
    fn stable_embedder_data_dir_fallback_is_not_cwd_relative() -> TestResult {
        let fallback = stable_ee_data_dir_fallback();
        let cwd = std::env::current_dir().map_err(|error| error.to_string())?;

        ensure(
            fallback.is_absolute(),
            "fallback data dir should be absolute",
        )?;
        ensure(
            !fallback.starts_with(&cwd),
            "fallback data dir must not depend on the process cwd",
        )
    }

    #[test]
    fn embed_download_mode_parser_defaults_to_auto_and_accepts_off() -> TestResult {
        ensure(
            parse_embed_download_mode(None) == EeEmbedDownloadMode::Auto,
            "unset EE_EMBED_DOWNLOAD should default to auto",
        )?;
        ensure(
            parse_embed_download_mode(Some("auto")) == EeEmbedDownloadMode::Auto,
            "auto should enable ee-managed first-use download",
        )?;
        ensure(
            parse_embed_download_mode(Some("OFF")) == EeEmbedDownloadMode::Off,
            "off should disable downloads case-insensitively",
        )?;
        ensure(
            parse_embed_download_mode(Some("0")) == EeEmbedDownloadMode::Off,
            "0 should be accepted as an offline opt-out",
        )?;
        ensure(
            parse_embed_download_mode(Some("surprise")) == EeEmbedDownloadMode::Auto,
            "invalid values should fall back to the default auto policy",
        )
    }

    #[test]
    fn embed_model_destination_respects_prepopulated_model_dir() -> TestResult {
        let root = unique_test_dir("embed-model-dir");
        ensure(
            potion_model_destination_dir(&root) == root.join(POTION_MODEL_NAME),
            "plain cache roots should place the model in a named child directory",
        )?;
        let direct_model_dir = root.join(POTION_MODEL_NAME);
        ensure(
            potion_model_destination_dir(&direct_model_dir) == direct_model_dir,
            "pre-populated model directories should be honored directly",
        )
    }

    #[test]
    fn embed_download_off_keeps_hash_fallback_stack() -> TestResult {
        let settings = EeEmbedderSettings {
            model_root: unique_test_dir("embed-download-off"),
            download_mode: EeEmbedDownloadMode::Off,
        };
        let stack = search_embedder_stack_for_settings(&settings);

        ensure(
            !stack.fast().is_semantic(),
            "EE_EMBED_DOWNLOAD=off should keep the deterministic hash fast tier",
        )?;
        ensure(
            stack
                .quality()
                .is_some_and(|quality| !quality.is_semantic()),
            "offline opt-out should retain the hash quality fallback",
        )
    }

    #[test]
    fn embed_download_off_consults_disk_and_never_builds_lazy_download_stub() -> TestResult {
        // GH#18: Off mode must consult the on-disk model (via local-only
        // auto-detect) and, when none is present, use the deterministic hash
        // fallback. It must NEVER hand back the lazy potion download stub, which
        // would fetch over the network on first embed — that would violate the
        // offline opt-out. With no model on disk the fast tier is therefore the
        // hash embedder, not the potion lazy stub.
        let settings = EeEmbedderSettings {
            model_root: unique_test_dir("embed-download-off-no-lazy"),
            download_mode: EeEmbedDownloadMode::Off,
        };
        let stack = search_embedder_stack_for_settings(&settings);

        ensure(
            stack.fast().id() != POTION_MODEL_NAME,
            "off mode must not return the lazy potion download stub",
        )?;
        ensure(
            !stack.fast().is_semantic(),
            "off mode with an empty cache must stay on the deterministic hash fallback",
        )?;
        ensure(
            !stack.fast().is_ready() || stack.fast().category() == ModelCategory::HashEmbedder,
            "off mode fast tier must be the hash embedder, never a download-capable model",
        )
    }

    #[test]
    fn embed_download_auto_uses_lazy_stack_for_empty_cache() -> TestResult {
        let settings = EeEmbedderSettings {
            model_root: unique_test_dir("embed-download-auto-empty"),
            download_mode: EeEmbedDownloadMode::Auto,
        };
        let stack = search_embedder_stack_for_settings(&settings);

        ensure(
            !stack.fast().is_semantic(),
            "auto mode must not claim semantic retrieval before the model is loaded",
        )?;
        ensure(
            stack.fast().id() == POTION_MODEL_NAME,
            "auto mode should select the bundled potion model id",
        )?;
        ensure(
            !stack.fast().is_ready(),
            "lazy semantic tier should not claim ready before first load/download",
        )?;
        ensure(
            stack.quality().is_none(),
            "lazy first-use download tier should not graft a misleading hash quality tier",
        )
    }

    #[test]
    fn lazy_model2vec_embedder_reports_hash_posture_after_failure() -> TestResult {
        let embedder = EeLazyModel2VecEmbedder::new(unique_test_dir("lazy-model-failure"));

        ensure(
            !embedder.is_semantic(),
            "pending lazy model must be download-capable but not semantic-ready",
        )?;
        ensure(
            embedder.id() == POTION_MODEL_NAME,
            "pending lazy model should still disclose the intended bundled model id",
        )?;

        embedder.mark_failed();
        let fallback = HashEmbedder::default_256();
        ensure(
            !embedder.is_semantic(),
            "failed lazy model must remain non-semantic",
        )?;
        ensure(
            embedder.id() == fallback.id(),
            "failed lazy model should disclose the active hash fallback id",
        )?;
        ensure(
            embedder.category() == ModelCategory::HashEmbedder,
            "failed lazy model should disclose hash fallback category",
        )
    }

    #[test]
    fn pending_lazy_model2vec_posture_is_not_hash_fallback() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = crate::testing::wsp("lazypending");
        let workspace_id = workspace_id.as_str();
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-lazy-pending-posture-test".to_owned(),
                    name: Some("lazy pending posture test".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        let lazy = Arc::new(EeLazyModel2VecEmbedder::new(unique_test_dir(
            "lazy-model-pending-posture",
        )));
        let stack =
            EmbedderStack::from_parts(lazy.clone() as Arc<dyn crate::search::Embedder>, None);
        let pending = embedding_posture_from_stack(
            &connection,
            workspace_id,
            &stack,
            EmbeddingVectorCoverage::new(0, 3),
        )
        .map_err(|error| error.to_string())?;

        ensure(!pending.semantic, "pending download is not semantic-ready")?;
        ensure(
            pending.semantic_pending(),
            "pending download should have a distinct posture state",
        )?;
        ensure(
            pending.mode == EMBEDDING_POSTURE_MODE_NEURAL_LOCAL_PENDING,
            "pending download should use neural_local_pending mode",
        )?;
        ensure(
            pending.source == "ee_model2vec_download_pending",
            "pending download should not be reported as hash fallback",
        )?;
        ensure(
            pending.fast_model_id == POTION_MODEL_NAME,
            "pending posture should disclose the intended bundled model id",
        )?;

        lazy.mark_failed();
        let failed = embedding_posture_from_stack(
            &connection,
            workspace_id,
            &stack,
            EmbeddingVectorCoverage::new(0, 3),
        )
        .map_err(|error| error.to_string())?;
        ensure(
            !failed.semantic_pending(),
            "failed load is no longer pending",
        )?;
        ensure(
            failed.mode == EMBEDDING_POSTURE_MODE_DETERMINISTIC_HASH,
            "failed load should become deterministic hash fallback",
        )?;
        ensure(
            failed.source == "frankensearch_hash_fallback",
            "failed load should report hash fallback source",
        )
    }

    #[test]
    fn active_registry_input_requires_ready_semantic_embedder() -> TestResult {
        let stack = EmbedderStack::from_parts(
            Arc::new(TestSemanticEmbedder::not_ready("lazy-potion-test", 256))
                as Arc<dyn crate::search::Embedder>,
            None,
        );

        let input =
            active_embedding_registry_input("wsp_readygate000000000000000000", stack.fast())
                .map_err(|error| error.to_string())?;
        ensure(
            input.is_none(),
            "unloaded semantic embedders must not produce Available registry input",
        )
    }

    #[test]
    fn unavailable_semantic_embedder_does_not_create_available_registry_row() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_lazyfail000000000000000000";
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-lazy-fail-test".to_owned(),
                    name: Some("lazy fail test".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        let stack = EmbedderStack::from_parts(
            Arc::new(TestSemanticEmbedder::not_ready("lazy-potion-test", 256))
                as Arc<dyn crate::search::Embedder>,
            None,
        );

        ensure_active_embedding_registry_record(&connection, workspace_id, &stack)
            .map_err(|error| error.to_string())?;
        let records = connection
            .list_embedding_metadata_records(workspace_id)
            .map_err(|error| error.to_string())?;
        ensure(
            records.is_empty(),
            "lazy-load failure must not leave an Available registry row",
        )?;
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn active_registry_promotes_declared_unavailable_bundled_row() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = crate::testing::wsp("declaredfirst");
        let workspace_id = workspace_id.as_str();
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-declared-first-test".to_owned(),
                    name: Some("declared first test".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        let mut metadata =
            EmbeddingMetadataRecord::new(BUNDLED_EMBEDDING_DIMENSION, ModelDistanceMetric::Cosine);
        metadata.pooling = EmbeddingPooling::ModelDefault;
        metadata.tokenizer = Some("tokenizer.json".to_owned());
        metadata.model_revision = Some(BUNDLED_EMBEDDING_MODEL_REVISION.to_owned());
        metadata.deterministic = true;
        connection
            .insert_embedding_metadata_record(
                &crate::testing::mdl("declaredfirst"),
                &crate::db::CreateEmbeddingMetadataInput {
                    workspace_id: workspace_id.to_owned(),
                    provider: ModelProvider::Model2Vec,
                    model_name: POTION_MODEL_NAME.to_owned(),
                    dimension: BUNDLED_EMBEDDING_DIMENSION,
                    distance_metric: ModelDistanceMetric::Cosine,
                    status: ModelRegistryStatus::Unavailable,
                    version: Some(BUNDLED_EMBEDDING_MODEL_REVISION.to_owned()),
                    source_uri: Some(format!(
                        "frankensearch://{}/{}",
                        ModelProvider::Model2Vec.as_str(),
                        POTION_MODEL_NAME
                    )),
                    content_hash: None,
                    metadata,
                    last_checked_at: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let stack = EmbedderStack::from_parts(
            Arc::new(TestSemanticEmbedder::new(POTION_MODEL_NAME, 256))
                as Arc<dyn crate::search::Embedder>,
            None,
        );
        ensure_active_embedding_registry_record(&connection, workspace_id, &stack)
            .map_err(|error| error.to_string())?;

        let records = connection
            .list_embedding_metadata_records(workspace_id)
            .map_err(|error| error.to_string())?;
        ensure(
            records.len() == 1,
            "promotion should not duplicate registry row",
        )?;
        ensure(
            records[0].registry.status == ModelRegistryStatus::Available,
            "declared row should become Available after loaded semantic proof",
        )?;
        ensure(
            records[0].registry.content_hash.is_some(),
            "promoted row should carry active content hash",
        )?;
        let summary = reembed_embedding_summary(
            &connection,
            workspace_id,
            &stack,
            EmbeddingVectorCoverage::new(1, 1),
        )
        .map_err(|error| error.to_string())?;
        ensure(
            summary.available_model_count == 1 && summary.source == "registry_observed",
            "promoted row should drive registry_observed source",
        )?;
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn active_registry_input_records_derived_revision_and_content_hash() -> TestResult {
        let stack = EmbedderStack::from_parts(
            Arc::new(TestSemanticEmbedder::new("fixture-semantic-model", 256))
                as Arc<dyn crate::search::Embedder>,
            None,
        );

        let input = active_embedding_registry_input("wsp_hash000000000000000000000", stack.fast())
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "ready semantic embedder should produce registry input".to_owned())?;
        let content_hash = input
            .content_hash
            .as_deref()
            .ok_or_else(|| "registry content hash should be present".to_owned())?;
        let version = input
            .version
            .as_deref()
            .ok_or_else(|| "registry version should be present".to_owned())?;

        ensure(
            input.status == ModelRegistryStatus::Available,
            "ready semantic embedder should be Available",
        )?;
        ensure(
            content_hash.starts_with("blake3:") && content_hash.len() == "blake3:".len() + 64,
            "content hash should be a blake3 digest",
        )?;
        ensure(
            Some(version) == input.metadata.model_revision.as_deref(),
            "registry version and metadata model_revision should match",
        )?;
        ensure(
            version.starts_with("derived-blake3-"),
            "non-manifest fixtures should derive revision from content hash",
        )
    }

    #[test]
    fn reembed_summary_reflects_registered_semantic_fast_tier() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_11234567890123456789012345";
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-semantic-default-test".to_owned(),
                    name: Some("semantic default test".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        let stack = stack_with_hash_quality_fallback(EmbedderStack::from_parts(
            Arc::new(TestSemanticEmbedder::new("potion-multilingual-128M", 256))
                as Arc<dyn crate::search::Embedder>,
            None,
        ));
        ensure_active_embedding_registry_record(&connection, workspace_id, &stack)
            .map_err(|error| error.to_string())?;
        let summary = reembed_embedding_summary(
            &connection,
            workspace_id,
            &stack,
            EmbeddingVectorCoverage::new(2, 3),
        )
        .map_err(|error| error.to_string())?;

        ensure(summary.semantic, "summary should report semantic=true")?;
        ensure(
            summary.fast_model_id == "potion-multilingual-128M",
            "summary should use the semantic fast model id",
        )?;
        ensure(
            summary.quality_model_id.is_none(),
            "semantic fast-only stack should not report a hash quality model",
        )?;
        ensure(
            summary.registered_model_count == 1 && summary.available_model_count == 1,
            "semantic fast tier should register one available embedding model",
        )?;
        ensure(
            summary.source == "registry_observed",
            "registered semantic model should use registry_observed source",
        )?;
        ensure(
            summary.posture.schema == EMBEDDING_POSTURE_SCHEMA_V1,
            "summary should carry the shared posture schema",
        )?;
        ensure(
            summary.posture.mode == EMBEDDING_POSTURE_MODE_NEURAL_LOCAL,
            "semantic summary should use neural_local posture mode",
        )?;
        ensure(
            summary.posture.vector_coverage == EmbeddingVectorCoverage::new(2, 3),
            "summary should preserve vector coverage",
        )
    }

    #[test]
    fn embedding_posture_serializer_is_stable_and_schema_pinned() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_21234567890123456789012345";
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-posture-serializer-test".to_owned(),
                    name: Some("posture serializer test".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        let stack = stack_with_hash_quality_fallback(EmbedderStack::from_parts(
            Arc::new(TestSemanticEmbedder::new("potion-multilingual-128M", 256))
                as Arc<dyn crate::search::Embedder>,
            None,
        ));
        ensure_active_embedding_registry_record(&connection, workspace_id, &stack)
            .map_err(|error| error.to_string())?;

        let posture = embedding_posture_from_stack(
            &connection,
            workspace_id,
            &stack,
            EmbeddingVectorCoverage::new(7, 11),
        )
        .map_err(|error| error.to_string())?;
        let json = posture.data_json();

        ensure(
            json == posture.data_json(),
            "posture serializer should be deterministic across calls",
        )?;
        ensure(
            json["schema"] == EMBEDDING_POSTURE_SCHEMA_V1,
            "posture schema should be pinned",
        )?;
        ensure(
            json["mode"] == EMBEDDING_POSTURE_MODE_NEURAL_LOCAL,
            "semantic posture should report neural_local mode",
        )?;
        ensure(json["semantic"] == true, "semantic flag")?;
        ensure(json["source"] == "registry_observed", "registry source")?;
        ensure(
            json["selected_registry_model"]["model_name"] == "potion-multilingual-128M",
            "selected registry model should identify the active semantic model",
        )?;
        ensure(
            json["vector_coverage"] == serde_json::json!({"embedded": 7, "total": 11}),
            "posture should include vector coverage",
        )
    }

    #[test]
    fn index_publish_lock_retry_delay_uses_bounded_backoff() {
        assert_eq!(index_publish_lock_retry_delay(0), Duration::from_millis(5));
        assert_eq!(index_publish_lock_retry_delay(1), Duration::from_millis(10));
        assert_eq!(index_publish_lock_retry_delay(4), Duration::from_millis(50));
        assert_eq!(
            index_publish_lock_retry_delay(100),
            Duration::from_millis(50)
        );
    }

    #[test]
    fn index_publish_lock_exhaustion_reports_stable_contention() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;

        let workspace_id = "wsp_lockretry000000000000000000";
        let lock_id = AdvisoryLockId::index(workspace_id);
        let first_lock = connection
            .acquire_advisory_lock(&lock_id, "agent_existing", Some(300), Some("test lock"))
            .map_err(|error| error.to_string())?;
        ensure(
            matches!(
                first_lock,
                AcquireLockResult::Acquired(_) | AcquireLockResult::Expired { .. }
            ),
            "first lock must be acquired",
        )?;

        let error = match acquire_index_publish_lock_with_retry(
            &connection,
            workspace_id,
            "agent_waiting",
            3,
            |_| Duration::ZERO,
        ) {
            Ok(_) => return Err("held lock should exhaust retries".to_owned()),
            Err(e) => e,
        };

        ensure(
            error.stable_code() == Some(INDEX_PUBLISH_LOCK_CONTENTION_CODE),
            "contention error must expose stable code",
        )?;

        let IndexRebuildError::LockContention(contention) = error else {
            return Err("expected lock contention error".to_owned());
        };
        ensure(
            contention.lock_id == lock_id.canonical_key(),
            "contention lock id",
        )?;
        ensure(
            contention.holder_id == "agent_existing",
            "contention holder id",
        )?;
        ensure(contention.attempts == 3, "contention attempts")?;
        ensure(contention.waited_ms == 0, "contention waited milliseconds")?;
        ensure(
            !contention.acquired_at.is_empty(),
            "contention acquired_at timestamp",
        )
    }

    #[test]
    fn index_rebuild_backfills_anchors_for_unanchored_memory() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                "wsp_anchorbackfill000000000000",
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-anchor-backfill".to_owned(),
                    name: Some("anchor-backfill".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        // The revision write path inserts a memory row without extracting
        // anchors, mirroring rows created before the anchor table existed.
        let memory_id = "mem_anchorbackfill000000000001";
        connection
            .insert_memory_revision(
                memory_id,
                memory_id,
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_anchorbackfill000000000000".to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Run `cargo fmt --check` before touching `src/db/mod.rs`.".to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        ensure(
            connection
                .list_memory_anchors(memory_id)
                .map_err(|error| error.to_string())?
                .is_empty(),
            "revision insert must not extract anchors",
        )?;

        let memories = connection
            .list_memories_for_retrieval_with_global("wsp_anchorbackfill000000000000", None, false)
            .map_err(|error| error.to_string())?;
        let documents = memory_documents_with_anchors(&connection, &memories)
            .map_err(|error| error.to_string())?;
        ensure(documents.len() == 1, "one indexable document expected")?;

        let anchors = connection
            .list_memory_anchors(memory_id)
            .map_err(|error| error.to_string())?;
        ensure(
            anchors
                .iter()
                .any(|anchor| anchor.source == crate::models::MemoryAnchorSource::IndexRebuild),
            "index rebuild must backfill anchors with the index_rebuild source",
        )?;
        ensure(
            anchors
                .iter()
                .any(|anchor| anchor.anchor_kind == crate::models::MemoryAnchorKind::Command),
            "command anchor expected from backfill",
        )?;
        ensure(
            anchors
                .iter()
                .any(|anchor| anchor.anchor_kind == crate::models::MemoryAnchorKind::Path),
            "path anchor expected from backfill",
        )?;
        connection.close().map_err(|error| error.to_string())
    }

    fn unique_test_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ee-index-{label}-{}-{}",
            std::process::id(),
            monotonicish_stamp()
        ))
    }

    fn write_marker(dir: &Path, file: &str, body: &str) -> TestResult {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(file), body).map_err(|e| e.to_string())
    }

    fn seed_reembed_database(workspace: &Path, database: &Path) -> TestResult {
        let parent = database
            .parent()
            .ok_or_else(|| "database path must have parent".to_string())?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let connection = DbConnection::open_file(database).map_err(|e| e.to_string())?;
        connection.migrate().map_err(|e| e.to_string())?;
        connection
            .insert_workspace(
                "wsp_01234567890123456789012345",
                &crate::db::CreateWorkspaceInput {
                    path: workspace.to_string_lossy().into_owned(),
                    name: Some("reembed-test".to_owned()),
                },
            )
            .map_err(|e| e.to_string())?;
        connection
            .insert_memory(
                "mem_01234567890123456789012345",
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_01234567890123456789012345".to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Run cargo fmt --check before release.".to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some("file://AGENTS.md#compiler-checks".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: Some("unit-test".to_owned()),
                    tags: vec!["release".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|e| e.to_string())?;
        connection.close().map_err(|e| e.to_string())
    }

    fn queue_pending_index_job(database: &Path, job_id: &str) -> TestResult {
        let connection = DbConnection::open_file(database).map_err(|e| e.to_string())?;
        connection
            .insert_search_index_job(
                job_id,
                &crate::db::CreateSearchIndexJobInput {
                    workspace_id: "wsp_01234567890123456789012345".to_owned(),
                    job_type: SearchIndexJobType::FullRebuild,
                    document_source: None,
                    document_id: None,
                    documents_total: 0,
                },
            )
            .map_err(|e| e.to_string())?;
        connection.close().map_err(|e| e.to_string())
    }

    fn test_indexable_doc(id: &str, content: &str) -> crate::search::IndexableDocument {
        crate::search::IndexableDocument::new(id, content)
            .with_title(format!("title-{id}"))
            .with_metadata("fixture", "incremental-index")
    }

    fn deterministic_incremental_doc(
        slot: u8,
        term: u8,
        generation: u64,
    ) -> crate::search::IndexableDocument {
        const TERMS: [&str; 8] = [
            "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "theta", "kappa",
        ];
        let id = format!("doc-{slot}");
        let term = TERMS[usize::from(term) % TERMS.len()];
        let content = format!(
            "incremental equivalence release notes common-token slot-{slot} term-{term} generation-{generation}"
        );
        test_indexable_doc(&id, &content)
            .with_metadata("slot", slot.to_string())
            .with_metadata("term", term)
    }

    #[derive(Clone, Debug)]
    enum IncrementalDocOp {
        Upsert { slot: u8, term: u8 },
        Delete { slot: u8 },
    }

    fn incremental_doc_ops() -> impl Strategy<Value = Vec<IncrementalDocOp>> {
        prop::collection::vec(
            prop_oneof![
                (0u8..8, 0u8..16)
                    .prop_map(|(slot, term)| { IncrementalDocOp::Upsert { slot, term } }),
                (0u8..8).prop_map(|slot| IncrementalDocOp::Delete { slot }),
            ],
            1..24,
        )
    }

    #[derive(Debug, Eq, PartialEq)]
    struct VectorSnapshotRow {
        doc_id: String,
        fast_bits: Vec<u32>,
        quality_bits: Option<Vec<u32>>,
    }

    #[derive(Debug, Eq, PartialEq)]
    struct SearchSnapshotRow {
        doc_id: String,
        score: String,
    }

    fn vector_index_snapshot(index_dir: &Path) -> Result<Vec<VectorSnapshotRow>, String> {
        let index =
            frankensearch::TwoTierIndex::open(index_dir, frankensearch::TwoTierConfig::default())
                .map_err(|error| error.to_string())?;
        let mut rows = Vec::new();
        for position in 0..index.doc_count() {
            let doc_id = index
                .doc_id_at(position)
                .map_err(|error| error.to_string())?
                .to_owned();
            let fast = index
                .fast_vector_for_doc_id(&doc_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("missing fast vector for {doc_id}"))?;
            let quality = index
                .quality_vector_for_doc_id(&doc_id)
                .map_err(|error| error.to_string())?;
            rows.push(VectorSnapshotRow {
                doc_id,
                fast_bits: fast.iter().map(|value| value.to_bits()).collect(),
                quality_bits: quality
                    .map(|values| values.iter().map(|value| value.to_bits()).collect()),
            });
        }
        Ok(rows)
    }

    fn search_result_snapshot(
        index_dir: &Path,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchSnapshotRow>, String> {
        let index = Arc::new(
            crate::search::TwoTierIndex::open(index_dir, crate::search::TwoTierConfig::default())
                .map_err(|error| error.to_string())?,
        );
        let fast_embedder =
            Arc::new(HashEmbedder::default_256()) as Arc<dyn crate::search::Embedder>;
        let searcher = crate::search::TwoTierSearcher::new(
            index,
            fast_embedder,
            crate::search::TwoTierConfig::default(),
        );
        let query = query.to_owned();
        crate::core::run_cli_future(async move {
            let cx = asupersync::Cx::for_testing();
            let (results, _) = searcher
                .search_collect(&cx, &query, limit)
                .await
                .map_err(|error| error.to_string())?;
            Ok::<Vec<SearchSnapshotRow>, String>(
                results
                    .into_iter()
                    .map(|result| SearchSnapshotRow {
                        doc_id: result.doc_id.to_string(),
                        score: format!("{:.6}", result.score),
                    })
                    .collect(),
            )
        })
        .map_err(|error| format!("search runtime failed: {error}"))?
    }

    fn ensure_search_results_match_full_rebuild(
        incremental_dir: &Path,
        full_dir: &Path,
    ) -> TestResult {
        for query in [
            "incremental release",
            "common-token notes",
            "alpha",
            "beta",
            "theta",
            "slot-3",
        ] {
            let incremental = search_result_snapshot(incremental_dir, query, 10)?;
            let full = search_result_snapshot(full_dir, query, 10)?;
            ensure(
                incremental == full,
                format!(
                    "incremental search snapshot should match full rebuild for query {query:?}: incremental={incremental:?} full={full:?}"
                ),
            )?;
        }
        Ok(())
    }

    fn assert_incremental_sequence_equivalent_to_full_rebuild(
        ops: &[IncrementalDocOp],
    ) -> TestResult {
        let root = unique_test_dir("incremental-equivalence-property");
        let incremental_dir = root.join("incremental-index");
        let full_dir = root.join("full-index");
        let mut live_docs = BTreeMap::new();
        live_docs.insert(
            "doc-sentinel".to_owned(),
            test_indexable_doc(
                "doc-sentinel",
                "incremental equivalence sentinel release notes common-token",
            ),
        );
        for slot in 0u8..3 {
            let document = deterministic_incremental_doc(slot, slot, 1);
            live_docs.insert(format!("doc-{slot}"), document);
        }

        build_index_sync(
            &incremental_dir,
            hash_fallback_embedder_stack(),
            live_docs.values().cloned().collect(),
        )?;
        let mut generation = 1u64;
        write_index_metadata(&incremental_dir, generation, live_docs.len() as u32)
            .map_err(|error| error.to_string())?;

        for op in ops {
            generation += 1;
            match *op {
                IncrementalDocOp::Upsert { slot, term } => {
                    let id = format!("doc-{slot}");
                    let document = deterministic_incremental_doc(slot, term, generation);
                    live_docs.insert(id.clone(), document.clone());
                    let outcome = apply_incremental_index_change_sync(
                        &incremental_dir,
                        hash_fallback_embedder_stack(),
                        &id,
                        Some(document),
                        generation,
                        live_docs.len() as u32,
                    );
                    ensure(
                        matches!(
                            outcome,
                            IncrementalApplyOutcome::Applied {
                                documents_indexed: 1
                            }
                        ),
                        format!("unexpected upsert outcome for {id}: {outcome:?}"),
                    )?;
                }
                IncrementalDocOp::Delete { slot } => {
                    let id = format!("doc-{slot}");
                    live_docs.remove(&id);
                    let outcome = apply_incremental_index_change_sync(
                        &incremental_dir,
                        hash_fallback_embedder_stack(),
                        &id,
                        None,
                        generation,
                        live_docs.len() as u32,
                    );
                    ensure(
                        matches!(
                            outcome,
                            IncrementalApplyOutcome::Applied {
                                documents_indexed: 0
                            }
                        ),
                        format!("unexpected delete outcome for {id}: {outcome:?}"),
                    )?;
                }
            }
        }

        build_index_sync(
            &full_dir,
            hash_fallback_embedder_stack(),
            live_docs.values().cloned().collect(),
        )?;
        write_index_metadata(&full_dir, generation, live_docs.len() as u32)
            .map_err(|error| error.to_string())?;

        ensure_search_results_match_full_rebuild(&incremental_dir, &full_dir)
    }

    fn read_marker(dir: &Path, file: &str) -> Result<String, String> {
        std::fs::read_to_string(dir.join(file)).map_err(|e| e.to_string())
    }

    #[test]
    fn index_rebuild_status_as_str_is_stable() {
        assert_eq!(IndexRebuildStatus::Success.as_str(), "success");
        assert_eq!(IndexRebuildStatus::DryRun.as_str(), "dry_run");
        assert_eq!(IndexRebuildStatus::NoDocuments.as_str(), "no_documents");
        assert_eq!(IndexRebuildStatus::DatabaseError.as_str(), "database_error");
        assert_eq!(IndexRebuildStatus::IndexError.as_str(), "index_error");
    }

    #[test]
    fn index_rebuild_report_data_json_has_required_fields() {
        let report = IndexRebuildReport {
            status: IndexRebuildStatus::Success,
            memories_indexed: 5,
            sessions_indexed: 3,
            artifacts_indexed: 2,
            documents_total: 10,
            index_dir: PathBuf::from("/tmp/index"),
            elapsed_ms: 123.4,
            dry_run: false,
            errors: Vec::new(),
            runtime_profile: test_runtime_profile(),
        };

        let json = report.data_json();
        assert_eq!(json["command"], "index_rebuild");
        assert_eq!(json["status"], "success");
        assert_eq!(json["memories_indexed"], 5);
        assert_eq!(json["sessions_indexed"], 3);
        assert_eq!(json["artifacts_indexed"], 2);
        assert_eq!(json["documents_total"], 10);
        assert_eq!(json["dry_run"], false);
    }

    #[test]
    fn incremental_fallback_reason_strings_are_stable() {
        assert_eq!(
            IncrementalFallbackReason::IndexAbsent.as_str(),
            "index_absent"
        );
        assert_eq!(
            IncrementalFallbackReason::GenerationSkew.as_str(),
            "generation_skew"
        );
        assert_eq!(
            IncrementalFallbackReason::TierUnavailable.as_str(),
            "tier_unavailable"
        );
        assert_eq!(
            IncrementalFallbackReason::ForcedReindex.as_str(),
            "forced_reindex"
        );
        assert_eq!(
            IncrementalFallbackReason::DeltaOverThreshold.as_str(),
            "delta_over_threshold"
        );
    }

    #[test]
    fn incremental_missing_index_reports_index_absent_fallback() {
        let root = unique_test_dir("incremental-missing-index");
        let index_dir = root.join("index");
        let document = test_indexable_doc("doc-alpha", "alpha content");

        let outcome = apply_incremental_index_change_sync(
            &index_dir,
            default_embedder_stack(),
            "doc-alpha",
            Some(document),
            1,
            1,
        );

        match outcome {
            IncrementalApplyOutcome::Fallback { reason, detail } => {
                assert_eq!(reason, IncrementalFallbackReason::IndexAbsent);
                assert!(detail.contains("active index directory is absent"));
            }
            other => panic!("unexpected incremental outcome: {other:?}"),
        }
    }

    #[test]
    fn incremental_single_document_rejects_preexisting_stale_generation_gap() -> TestResult {
        let root = unique_test_dir("incremental-stale-generation-gap");
        let index_dir = root.join("index");
        build_index_sync(
            &index_dir,
            hash_fallback_embedder_stack(),
            vec![test_indexable_doc("doc-alpha", "alpha content")],
        )?;
        write_index_metadata(&index_dir, 1, 1).map_err(|error| error.to_string())?;

        let outcome = apply_incremental_index_change_sync(
            &index_dir,
            hash_fallback_embedder_stack(),
            "doc-beta",
            Some(test_indexable_doc("doc-beta", "beta content")),
            3,
            2,
        );

        match outcome {
            IncrementalApplyOutcome::Fallback { reason, detail } => {
                ensure(
                    reason == IncrementalFallbackReason::GenerationSkew,
                    format!("expected generation_skew fallback, got {reason:?}: {detail}"),
                )?;
                ensure(
                    detail.contains("2 generations behind database generation 3"),
                    format!("fallback detail should describe stale gap: {detail}"),
                )
            }
            other => Err(format!("unexpected incremental outcome: {other:?}")),
        }
    }

    #[test]
    fn incremental_batch_allows_bounded_generation_lag_for_claimed_documents() -> TestResult {
        let root = unique_test_dir("incremental-batch-generation-lag");
        let index_dir = root.join("index");
        build_index_sync(
            &index_dir,
            hash_fallback_embedder_stack(),
            vec![test_indexable_doc("doc-alpha", "alpha content")],
        )?;
        write_index_metadata(&index_dir, 1, 1).map_err(|error| error.to_string())?;

        let outcome = apply_incremental_index_batch_sync(
            &index_dir,
            hash_fallback_embedder_stack(),
            vec![
                test_indexable_doc("doc-beta", "beta content"),
                test_indexable_doc("doc-gamma", "gamma content"),
            ],
            3,
            3,
        );

        ensure(
            matches!(
                outcome,
                IncrementalApplyOutcome::Applied {
                    documents_indexed: 2
                }
            ),
            format!("unexpected incremental batch outcome: {outcome:?}"),
        )
    }

    #[test]
    fn coalesced_incremental_fallback_reports_generation_skew_reason() -> TestResult {
        let root = unique_test_dir("coalesced-generation-skew-report");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let root = std::fs::canonicalize(&root).map_err(|error| error.to_string())?;
        let index_dir = root.join("index");
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_012345678901234567890123cf";
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: root.to_string_lossy().into_owned(),
                    name: Some("coalesced generation skew report".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        let add_memory_job = |memory_id: &str, job_id: &str, content: &str| -> Result<(), String> {
            connection
                .insert_memory(
                    memory_id,
                    &crate::db::CreateMemoryInput {
                        workspace_id: workspace_id.to_owned(),
                        level: "procedural".to_owned(),
                        kind: "rule".to_owned(),
                        content: content.to_owned(),
                        workflow_id: None,
                        confidence: 0.9,
                        utility: 0.5,
                        importance: 0.5,
                        provenance_uri: Some("test://coalesced-generation-skew".to_owned()),
                        trust_class: "human_explicit".to_owned(),
                        trust_subclass: None,
                        tags: Vec::new(),
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())?;
            connection
                .insert_search_index_job(
                    job_id,
                    &crate::db::CreateSearchIndexJobInput {
                        workspace_id: workspace_id.to_owned(),
                        job_type: crate::db::SearchIndexJobType::SingleDocument,
                        document_source: Some("memory".to_owned()),
                        document_id: Some(memory_id.to_owned()),
                        documents_total: 1,
                    },
                )
                .map_err(|error| error.to_string())
        };

        let seed_memory_id = "mem_012345678901234567890123cs";
        add_memory_job(
            seed_memory_id,
            "sidx_012345678901234567890123cs",
            "seed alpha document for coalesced fallback report",
        )?;
        let seed = process_index_job_for_connection(
            &connection,
            "sidx_012345678901234567890123cs",
            &index_dir,
        )
        .map_err(|error| error.to_string())?;
        ensure(
            seed.outcome == "completed",
            format!("seed index job did not complete: {seed:?}"),
        )?;

        connection
            .add_memory_tags(seed_memory_id, &["retagged".to_owned()])
            .map_err(|error| error.to_string())?;
        add_memory_job(
            "mem_012345678901234567890123cb",
            "sidx_012345678901234567890123cb",
            "beta document added after an unindexed tag generation bump",
        )?;

        let reports =
            process_pending_index_jobs_coalesced(&connection, workspace_id, &index_dir, None)
                .map_err(|error| error.to_string())?;
        ensure(
            reports.len() == 1,
            format!("expected one coalesced report, got {reports:?}"),
        )?;
        let report = &reports[0];
        ensure(
            report.outcome == "completed",
            format!("coalesced fallback rebuild should complete: {report:?}"),
        )?;
        ensure(
            report.fallback_to_full.as_deref()
                == Some(IncrementalFallbackReason::GenerationSkew.as_str()),
            format!("expected generation_skew fallback report, got {report:?}"),
        )?;
        ensure(
            report.processing_mode.contains("fallback_to_full"),
            format!(
                "expected fallback processing mode marker, got {}",
                report.processing_mode
            ),
        )?;
        ensure(
            report.documents_indexed == 2,
            format!(
                "fallback full rebuild should publish both documents, got {}",
                report.documents_indexed
            ),
        )?;

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn single_document_job_full_rebuilds_when_sibling_jobs_pending() -> TestResult {
        // bd-2qmvp: a single-document index job that runs while a sibling
        // memory's index job is still pending must NOT incrementally apply only
        // its own document and then stamp the current MAX workspace generation
        // (that publishes a current-but-incomplete index which a concurrent
        // `ee search` reads as Ready, silently missing the sibling). It must
        // rebuild the COMPLETE indexable set so the published generation is
        // honest about every committed document.
        let root = unique_test_dir("bd2qmvp-sibling-pending");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        // Canonicalize so the index path carries no symlinked components: when
        // this test is RCH-verified from the /Users checkout the worker maps the
        // tree through a symlinked project root, which the index-publish guard
        // (`ensure_index_path_has_no_symlinks`) correctly refuses. Resolving the
        // symlinks here keeps the test portable without weakening that guard.
        let root = std::fs::canonicalize(&root).map_err(|error| error.to_string())?;
        let index_dir = root.join("index");
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_012345678901234567890123ws";
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: root.to_string_lossy().into_owned(),
                    name: Some("bd-2qmvp sibling pending".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        let add_memory_job = |memory_id: &str, job_id: &str, content: &str| -> Result<(), String> {
            connection
                .insert_memory(
                    memory_id,
                    &crate::db::CreateMemoryInput {
                        workspace_id: workspace_id.to_owned(),
                        level: "procedural".to_owned(),
                        kind: "rule".to_owned(),
                        content: content.to_owned(),
                        workflow_id: None,
                        confidence: 0.9,
                        utility: 0.5,
                        importance: 0.5,
                        provenance_uri: Some("test://bd-2qmvp".to_owned()),
                        trust_class: "human_explicit".to_owned(),
                        trust_subclass: None,
                        tags: Vec::new(),
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())?;
            connection
                .insert_search_index_job(
                    job_id,
                    &crate::db::CreateSearchIndexJobInput {
                        workspace_id: workspace_id.to_owned(),
                        job_type: crate::db::SearchIndexJobType::SingleDocument,
                        document_source: Some("memory".to_owned()),
                        document_id: Some(memory_id.to_owned()),
                        documents_total: 1,
                    },
                )
                .map_err(|error| error.to_string())
        };

        // Seed an existing index so the single-document incremental path is a
        // live option; an absent index would fall back to a full rebuild
        // regardless, masking the gate under test.
        add_memory_job(
            "mem_012345678901234567890123ms",
            "sidx_012345678901234567890123js",
            "seed alpha document for sibling rebuild test",
        )?;
        let seed = process_index_job_for_connection(
            &connection,
            "sidx_012345678901234567890123js",
            &index_dir,
        )
        .map_err(|error| error.to_string())?;
        ensure(
            seed.outcome == "completed",
            format!("seed index job did not complete: {seed:?}"),
        )?;

        // Two concurrent writes land (beta, gamma); both their single-document
        // index jobs are now pending, mirroring the swarm remember pattern.
        add_memory_job(
            "mem_012345678901234567890123mb",
            "sidx_012345678901234567890123jb",
            "beta sibling document concurrent write",
        )?;
        add_memory_job(
            "mem_012345678901234567890123mg",
            "sidx_012345678901234567890123jg",
            "gamma sibling document concurrent write",
        )?;

        // Process ONLY beta's job while gamma's job is still pending.
        let beta = process_index_job_for_connection(
            &connection,
            "sidx_012345678901234567890123jb",
            &index_dir,
        )
        .map_err(|error| error.to_string())?;
        ensure(
            beta.outcome == "completed",
            format!("beta index job did not complete: {beta:?}"),
        )?;
        // With the fix the pending gamma job forces a full rebuild of the
        // complete set (seed + beta + gamma == 3 documents). The legacy bug
        // would incrementally apply only beta (documents_indexed == 1) and
        // leave gamma missing under the stamped max generation.
        ensure(
            beta.documents_indexed == 3,
            format!(
                "expected full rebuild of 3 documents when a sibling job is pending, got documents_indexed={} mode={}",
                beta.documents_indexed, beta.processing_mode
            ),
        )?;
        ensure(
            beta.processing_mode
                .contains("sibling_pending_full_rebuild"),
            format!(
                "expected sibling-pending full-rebuild processing mode, got {}",
                beta.processing_mode
            ),
        )?;

        // Draining gamma last (no siblings pending now) completes the queue.
        let gamma = process_index_job_for_connection(
            &connection,
            "sidx_012345678901234567890123jg",
            &index_dir,
        )
        .map_err(|error| error.to_string())?;
        ensure(
            gamma.outcome == "completed",
            format!("gamma index job did not complete: {gamma:?}"),
        )?;

        Ok(())
    }

    #[test]
    fn incremental_upsert_and_delete_match_full_rebuild_vectors_and_results() -> TestResult {
        let root = unique_test_dir("incremental-equivalence");
        let incremental_dir = root.join("incremental-index");
        let full_dir = root.join("full-index");
        let initial_docs = vec![
            test_indexable_doc("doc-alpha", "alpha release workflow"),
            test_indexable_doc("doc-beta", "beta old notes"),
        ];
        let final_docs = vec![
            test_indexable_doc("doc-beta", "beta updated notes"),
            test_indexable_doc("doc-gamma", "gamma new notes"),
        ];

        build_index_sync(
            &incremental_dir,
            hash_fallback_embedder_stack(),
            initial_docs.clone(),
        )?;
        write_index_metadata(&incremental_dir, 1, 2).map_err(|error| error.to_string())?;

        let beta_outcome = apply_incremental_index_change_sync(
            &incremental_dir,
            hash_fallback_embedder_stack(),
            "doc-beta",
            Some(test_indexable_doc("doc-beta", "beta updated notes")),
            2,
            2,
        );
        ensure(
            matches!(
                beta_outcome,
                IncrementalApplyOutcome::Applied {
                    documents_indexed: 1
                }
            ),
            format!("unexpected beta outcome: {beta_outcome:?}"),
        )?;

        let gamma_outcome = apply_incremental_index_change_sync(
            &incremental_dir,
            hash_fallback_embedder_stack(),
            "doc-gamma",
            Some(test_indexable_doc("doc-gamma", "gamma new notes")),
            3,
            3,
        );
        ensure(
            matches!(
                gamma_outcome,
                IncrementalApplyOutcome::Applied {
                    documents_indexed: 1
                }
            ),
            format!("unexpected gamma outcome: {gamma_outcome:?}"),
        )?;

        let alpha_outcome = apply_incremental_index_change_sync(
            &incremental_dir,
            hash_fallback_embedder_stack(),
            "doc-alpha",
            None,
            4,
            2,
        );
        ensure(
            matches!(
                alpha_outcome,
                IncrementalApplyOutcome::Applied {
                    documents_indexed: 0
                }
            ),
            format!("unexpected alpha outcome: {alpha_outcome:?}"),
        )?;

        build_index_sync(&full_dir, hash_fallback_embedder_stack(), final_docs)?;
        write_index_metadata(&full_dir, 4, 2).map_err(|error| error.to_string())?;

        ensure(
            vector_index_snapshot(&incremental_dir)? == vector_index_snapshot(&full_dir)?,
            "incremental vector snapshot should match full rebuild snapshot",
        )?;
        ensure_search_results_match_full_rebuild(&incremental_dir, &full_dir)
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 32,
            ..ProptestConfig::default()
        })]

        #[test]
        fn incremental_random_add_update_delete_ops_match_full_rebuild_results(
            ops in incremental_doc_ops()
        ) {
            let result = assert_incremental_sequence_equivalent_to_full_rebuild(&ops);
            prop_assert!(result.is_ok(), "{}", result.err().unwrap_or_default());
        }
    }

    #[test]
    fn db_stats_generation_tracks_source_documents_without_audit_rows() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                "wsp_01234567890123456789012345",
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-index-generation-test".to_owned(),
                    name: Some("index generation test".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_01234567890123456789012345",
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_01234567890123456789012345".to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Run cargo fmt --check before release.".to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: vec![],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let (_, _, generation) = get_db_stats(&connection).map_err(|error| error.to_string())?;
        ensure(
            generation == Some(1),
            "source generation should include unaudited source documents",
        )?;

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn db_stats_generation_ignores_read_surface_audit_rows() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                "wsp_22222222222222222222222222",
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-index-read-surface-generation-test".to_owned(),
                    name: Some("index read surface generation test".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_22222222222222222222222222",
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_22222222222222222222222222".to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Run cargo fmt --check before release.".to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: vec![],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let (_, _, generation_before) =
            get_db_stats(&connection).map_err(|error| error.to_string())?;
        ensure(
            generation_before == Some(1),
            "baseline generation should track the single source document",
        )?;

        for action in &READ_SURFACE_AUDIT_ACTIONS {
            connection
                .insert_audit(
                    &crate::db::generate_audit_id(),
                    &crate::db::CreateAuditInput {
                        workspace_id: Some("wsp_22222222222222222222222222".to_owned()),
                        actor: None,
                        action: (*action).to_owned(),
                        target_type: Some("memory".to_owned()),
                        target_id: Some("mem_22222222222222222222222222".to_owned()),
                        details: Some(serde_json::json!({"readSurface": true}).to_string()),
                    },
                )
                .map_err(|error| error.to_string())?;
        }

        let (_, _, generation_after) =
            get_db_stats(&connection).map_err(|error| error.to_string())?;
        ensure(
            generation_after == generation_before,
            format!(
                "read-surface audit rows must not bump index generation: before={generation_before:?} after={generation_after:?}",
            ),
        )?;

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn index_reembed_report_data_json_has_required_fields() {
        let report = IndexReembedReport {
            status: IndexReembedStatus::Success,
            job_id: Some("sidx_01234567890123456789012345".to_owned()),
            job_status: "completed".to_owned(),
            job_type: "full_rebuild".to_owned(),
            document_source: None,
            embedding_scope: "all_documents".to_owned(),
            embedding: ReembedEmbeddingSummary::from_posture(fixture_hash_embedding_posture()),
            memories_indexed: 5,
            sessions_indexed: 3,
            artifacts_indexed: 2,
            documents_embedded: 0,
            documents_total: 10,
            index_dir: PathBuf::from("/tmp/index"),
            elapsed_ms: 123.4,
            dry_run: false,
            idempotency_key: "blake3:test".to_owned(),
            errors: Vec::new(),
            runtime_profile: test_runtime_profile(),
        };

        let json = report.data_json();
        assert_eq!(json["command"], "index_reembed");
        assert_eq!(json["status"], "success");
        assert_eq!(json["job_status"], "completed");
        assert_eq!(json["job_type"], "full_rebuild");
        assert_eq!(json["document_source"], serde_json::Value::Null);
        assert_eq!(json["embedding_scope"], "all_documents");
        assert_eq!(json["embedding"]["fast_model_id"], "fnv1a-256");
        assert_eq!(json["embedding"]["quality_model_id"], "fnv1a-384");
        assert_eq!(json["embedding"]["deterministic"], true);
        assert_eq!(
            json["embedding"]["posture"]["schema"],
            EMBEDDING_POSTURE_SCHEMA_V1
        );
        assert_eq!(
            json["embedding"]["posture"]["vector_coverage"],
            serde_json::json!({"embedded": 0, "total": 10})
        );
        assert_eq!(json["memories_indexed"], 5);
        assert_eq!(json["sessions_indexed"], 3);
        assert_eq!(json["artifacts_indexed"], 2);
        assert_eq!(json["documents_embedded"], 0);
        assert_eq!(json["documents_total"], 10);
        assert_eq!(json["dry_run"], false);
    }

    #[test]
    fn index_reembed_dry_run_does_not_queue_job() -> TestResult {
        let root = unique_test_dir("reembed-dry-run");
        let workspace = root.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        seed_reembed_database(&workspace, &database)?;

        let report = reembed_index(&IndexReembedOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database.clone()),
            index_dir: Some(index_dir),
            dry_run: true,
        })
        .map_err(|e| e.to_string())?;

        ensure(
            report.status == IndexReembedStatus::DryRun,
            "dry-run status",
        )?;
        ensure(
            report.job_id.is_none(),
            "dry-run should not allocate job id",
        )?;
        ensure(
            report.job_status == "dry_run_not_queued",
            "dry-run job status",
        )?;
        ensure(report.documents_total == 1, "dry-run document count")?;
        ensure(report.documents_embedded == 0, "dry-run embedded count")?;

        let connection = DbConnection::open_file(database).map_err(|e| e.to_string())?;
        let jobs = connection
            .list_search_index_jobs("wsp_01234567890123456789012345", None)
            .map_err(|e| e.to_string())?;
        ensure(jobs.is_empty(), "dry-run must not queue search index jobs")?;
        let embedding_records = connection
            .list_embedding_metadata_records("wsp_01234567890123456789012345")
            .map_err(|e| e.to_string())?;
        ensure(
            embedding_records.is_empty(),
            "dry-run must not mutate the embedding model registry",
        )?;
        connection.close().map_err(|e| e.to_string())
    }

    #[test]
    fn index_reembed_queues_and_completes_embedding_job() -> TestResult {
        let root = unique_test_dir("reembed-completes-job");
        let workspace = root.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        seed_reembed_database(&workspace, &database)?;

        let report = reembed_index(&IndexReembedOptions {
            workspace_path: workspace,
            database_path: Some(database.clone()),
            index_dir: Some(index_dir.clone()),
            dry_run: false,
        })
        .map_err(|e| e.to_string())?;

        ensure(
            report.status == IndexReembedStatus::Success,
            format!("unexpected status: {:?}", report.status),
        )?;
        ensure(report.job_id.is_some(), "job id should be reported")?;
        ensure(report.job_status == "completed", "job should complete")?;
        ensure(report.document_source.is_none(), "document source")?;
        ensure(
            report.embedding_scope == "all_documents",
            "embedding scope should cover all documents",
        )?;
        ensure(report.documents_total == 1, "document count")?;
        ensure(report.documents_embedded == 1, "embedded document count")?;
        ensure(
            report.embedding.posture.vector_coverage == EmbeddingVectorCoverage::new(1, 1),
            "published vector coverage",
        )?;
        ensure(
            index_dir.join(INDEX_METADATA_FILE).is_file(),
            "reembed should publish index metadata",
        )?;

        let job_id = report
            .job_id
            .ok_or_else(|| "job id should be present".to_string())?;
        let connection = DbConnection::open_file(database).map_err(|e| e.to_string())?;
        let job = connection
            .get_search_index_job(&job_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "stored reembed job should exist".to_string())?;
        ensure(job.status == "completed", "stored job status")?;
        ensure(job.job_type == "full_rebuild", "stored job type")?;
        ensure(job.document_source.is_none(), "stored document source")?;
        ensure(job.documents_total == 1, "stored documents_total")?;
        ensure(job.documents_indexed == 1, "stored documents_indexed")?;
        connection.close().map_err(|e| e.to_string())
    }

    #[test]
    fn index_processing_dry_run_leaves_pending_job_unchanged() -> TestResult {
        let root = unique_test_dir("process-dry-run");
        let workspace = root.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        seed_reembed_database(&workspace, &database)?;
        queue_pending_index_job(&database, "sidx_processdryrun0000000000000")?;

        let report = process_index_jobs(&IndexProcessingOptions {
            workspace_path: workspace,
            database_path: Some(database.clone()),
            index_dir: Some(index_dir.clone()),
            dry_run: true,
            job_limit: Some(1),
        })
        .map_err(|e| e.to_string())?;

        ensure(
            report.status == IndexProcessingStatus::DryRun,
            "processing dry-run status",
        )?;
        ensure(report.pending_jobs == 1, "dry-run pending job count")?;
        ensure(report.processed_jobs == 0, "dry-run processed job count")?;
        ensure(
            !index_dir.join(INDEX_METADATA_FILE).exists(),
            "dry-run must not publish index metadata",
        )?;

        let connection = DbConnection::open_file(database).map_err(|e| e.to_string())?;
        let job = connection
            .get_search_index_job("sidx_processdryrun0000000000000")
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "pending job should exist".to_string())?;
        ensure(job.status == "pending", "dry-run keeps job pending")?;
        ensure(job.started_at.is_none(), "dry-run does not start job")?;
        connection.close().map_err(|e| e.to_string())
    }

    #[test]
    fn index_processing_completes_pending_rebuild_job() -> TestResult {
        let root = unique_test_dir("process-completes-job");
        let workspace = root.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        seed_reembed_database(&workspace, &database)?;
        queue_pending_index_job(&database, "sidx_processcomplete00000000000")?;

        let report = process_index_jobs(&IndexProcessingOptions {
            workspace_path: workspace,
            database_path: Some(database.clone()),
            index_dir: Some(index_dir.clone()),
            dry_run: false,
            job_limit: Some(1),
        })
        .map_err(|e| e.to_string())?;

        ensure(
            report.status == IndexProcessingStatus::Success,
            format!("unexpected processing status: {:?}", report.status),
        )?;
        ensure(report.pending_jobs == 1, "pending job count")?;
        ensure(report.processed_jobs == 1, "processed job count")?;
        ensure(report.completed_jobs == 1, "completed job count")?;
        ensure(report.failed_jobs == 0, "failed job count")?;
        ensure(
            index_dir.join(INDEX_METADATA_FILE).is_file(),
            "processor should publish index metadata",
        )?;

        let connection = DbConnection::open_file(database).map_err(|e| e.to_string())?;
        let job = connection
            .get_search_index_job("sidx_processcomplete00000000000")
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "processed job should exist".to_string())?;
        ensure(job.status == "completed", "stored job status")?;
        ensure(job.documents_total == 1, "stored documents_total")?;
        ensure(job.documents_indexed == 1, "stored documents_indexed")?;
        ensure(job.started_at.is_some(), "stored job started timestamp")?;
        ensure(job.completed_at.is_some(), "stored job completed timestamp")?;
        connection.close().map_err(|e| e.to_string())
    }

    #[test]
    fn index_rebuild_options_resolve_paths() {
        let options = IndexRebuildOptions {
            workspace_path: PathBuf::from("/home/user/project"),
            database_path: None,
            index_dir: None,
            dry_run: false,
        };

        assert_eq!(
            options.resolve_database_path(),
            PathBuf::from("/home/user/project/.ee/ee.db")
        );
        assert_eq!(
            options.resolve_index_dir(),
            PathBuf::from("/home/user/project/.ee/index")
        );
    }

    #[cfg(unix)]
    #[test]
    fn index_default_paths_canonicalize_existing_workspace_root() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = unique_test_dir("canonical-default-paths");
        let target = root.join("real-workspace");
        let alias = root.join("alias-workspace");
        std::fs::create_dir_all(&target).map_err(|error| error.to_string())?;
        symlink(&target, &alias).map_err(|error| error.to_string())?;

        let canonical = target.canonicalize().map_err(|error| error.to_string())?;
        let expected_database = canonical.join(".ee").join("ee.db");
        let expected_index = canonical.join(".ee").join(DEFAULT_INDEX_SUBDIR);

        let rebuild = IndexRebuildOptions {
            workspace_path: alias.clone(),
            database_path: None,
            index_dir: None,
            dry_run: false,
        };
        assert_eq!(rebuild.resolve_database_path(), expected_database);
        assert_eq!(rebuild.resolve_index_dir(), expected_index);

        let reembed = IndexReembedOptions {
            workspace_path: alias.clone(),
            database_path: None,
            index_dir: None,
            dry_run: false,
        };
        assert_eq!(reembed.resolve_database_path(), expected_database);
        assert_eq!(reembed.resolve_index_dir(), expected_index);

        let processing = IndexProcessingOptions {
            workspace_path: alias.clone(),
            database_path: None,
            index_dir: None,
            dry_run: false,
            job_limit: None,
        };
        assert_eq!(processing.resolve_database_path(), expected_database);
        assert_eq!(processing.resolve_index_dir(), expected_index);

        let status = IndexStatusOptions {
            workspace_path: alias.clone(),
            database_path: None,
            index_dir: None,
        };
        assert_eq!(status.resolve_database_path(), expected_database);
        assert_eq!(status.resolve_index_dir(), expected_index);

        let vacuum = IndexVacuumOptions {
            workspace_path: alias,
            database_path: None,
            index_dir: None,
        };
        assert_eq!(vacuum.resolve_database_path(), expected_database);
        assert_eq!(vacuum.resolve_index_dir(), expected_index);

        Ok(())
    }

    #[test]
    fn index_rebuild_options_respect_explicit_paths() {
        let options = IndexRebuildOptions {
            workspace_path: PathBuf::from("/home/user/project"),
            database_path: Some(PathBuf::from("/custom/db.sqlite")),
            index_dir: Some(PathBuf::from("/custom/index")),
            dry_run: true,
        };

        assert_eq!(
            options.resolve_database_path(),
            PathBuf::from("/custom/db.sqlite")
        );
        assert_eq!(options.resolve_index_dir(), PathBuf::from("/custom/index"));
    }

    // ========================================================================
    // Cache Invalidation Tests (EE-259)
    // ========================================================================

    #[test]
    fn cache_invalidation_missing_index_detected() {
        let health = determine_health(false, 0, Some(10), Some(10), false);
        assert_eq!(health, IndexHealth::Missing);
        assert_eq!(health.degradation_code(), Some("index_missing"));
    }

    #[test]
    fn cache_invalidation_empty_index_detected() {
        let health = determine_health(true, 0, Some(10), Some(10), false);
        assert_eq!(health, IndexHealth::Missing);
    }

    #[test]
    fn cache_invalidation_stale_when_db_ahead() {
        let health = determine_health(true, 5, Some(12), Some(9), false);
        assert_eq!(health, IndexHealth::Stale);
        assert_eq!(health.degradation_code(), Some("index_stale"));
    }

    #[test]
    fn cache_invalidation_stale_when_index_has_no_generation() {
        let health = determine_health(true, 5, Some(12), None, false);
        assert_eq!(health, IndexHealth::Stale);
    }

    #[test]
    fn cache_invalidation_corrupt_when_metadata_parse_fails() {
        let health = determine_health(true, 5, Some(12), None, true);
        assert_eq!(health, IndexHealth::Corrupt);
        assert_eq!(health.degradation_code(), Some("index_corrupt"));
    }

    #[test]
    fn cache_invalidation_ready_when_generations_match() {
        let health = determine_health(true, 5, Some(10), Some(10), false);
        assert_eq!(health, IndexHealth::Ready);
        assert_eq!(health.degradation_code(), None);
    }

    #[test]
    fn cache_invalidation_ready_when_index_ahead() {
        let health = determine_health(true, 5, Some(8), Some(10), false);
        assert_eq!(health, IndexHealth::Ready);
    }

    #[test]
    fn cache_invalidation_ready_when_no_generations_tracked() {
        let health = determine_health(true, 5, None, None, false);
        assert_eq!(health, IndexHealth::Ready);
    }

    #[test]
    fn cache_invalidation_ready_when_db_has_no_generation() {
        let health = determine_health(true, 5, None, Some(10), false);
        assert_eq!(health, IndexHealth::Ready);
    }

    #[test]
    fn index_health_strings_are_stable() {
        assert_eq!(IndexHealth::Ready.as_str(), "ready");
        assert_eq!(IndexHealth::Stale.as_str(), "stale");
        assert_eq!(IndexHealth::Missing.as_str(), "missing");
        assert_eq!(IndexHealth::Corrupt.as_str(), "corrupt");
    }

    #[test]
    fn index_health_degradation_codes_are_stable() {
        assert_eq!(IndexHealth::Ready.degradation_code(), None);
        assert_eq!(IndexHealth::Stale.degradation_code(), Some("index_stale"));
        assert_eq!(
            IndexHealth::Missing.degradation_code(),
            Some("index_missing")
        );
        assert_eq!(
            IndexHealth::Corrupt.degradation_code(),
            Some("index_corrupt")
        );
    }

    #[test]
    fn index_status_report_json_includes_generation_fields() {
        let report = IndexStatusReport {
            health: IndexHealth::Stale,
            index_dir: PathBuf::from("/tmp/index"),
            database_path: PathBuf::from("/tmp/ee.db"),
            embedding: Some(fixture_hash_embedding_posture()),
            index_exists: true,
            index_file_count: 3,
            index_size_bytes: 1024,
            db_memory_count: 10,
            db_session_count: 5,
            db_generation: Some(12),
            index_generation: Some(9),
            last_rebuild_at: Some("2026-04-30T12:00:00Z".to_string()),
            last_check_error: None,
            repair_hint: Some("ee index rebuild --workspace ."),
            elapsed_ms: 5.2,
        };

        let json = report.data_json();
        assert_eq!(json["health"], "stale");
        assert_eq!(json["degradationCode"], "index_stale");
        assert_eq!(json["degraded"][0]["code"], "index_stale");
        assert_eq!(json["degraded"][0]["severity"], "high");
        assert_eq!(json["embedding"]["schema"], EMBEDDING_POSTURE_SCHEMA_V1);
        assert_eq!(json["embedding"]["semantic"], false);
        assert_eq!(json["embedding"]["source"], "frankensearch_hash_fallback");
        assert_eq!(
            json["embedding"]["vector_coverage"],
            serde_json::json!({"embedded": 0, "total": 10})
        );
        assert_eq!(json["dbGeneration"], 12);
        assert_eq!(json["indexGeneration"], 9);
        assert_eq!(json["dbMemoryCount"], 10);
        assert_eq!(json["dbSessionCount"], 5);
        assert_eq!(json["repairHint"], "ee index rebuild --workspace .");
    }

    #[test]
    fn index_status_report_human_summary_shows_stale_warning() {
        let report = IndexStatusReport {
            health: IndexHealth::Stale,
            index_dir: PathBuf::from("/tmp/index"),
            database_path: PathBuf::from("/tmp/ee.db"),
            embedding: None,
            index_exists: true,
            index_file_count: 3,
            index_size_bytes: 1024,
            db_memory_count: 10,
            db_session_count: 5,
            db_generation: Some(12),
            index_generation: Some(9),
            last_rebuild_at: None,
            last_check_error: None,
            repair_hint: Some("ee index rebuild --workspace ."),
            elapsed_ms: 5.2,
        };

        let summary = report.human_summary();
        assert!(summary.contains("STALE"));
        assert!(summary.contains("rebuild recommended"));
        assert!(summary.contains("DB generation: 12"));
        assert!(summary.contains("Index generation: 9"));
    }

    #[test]
    fn index_status_embedding_posture_uses_requested_workspace_path() -> TestResult {
        let root = unique_test_dir("status-workspace-selector");
        let workspace_a = root.join("workspace-a");
        let workspace_b = root.join("workspace-b");
        std::fs::create_dir_all(&workspace_a).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&workspace_b).map_err(|error| error.to_string())?;
        let database = root.join("shared-ee.db");
        let index_dir_a = workspace_a.join(".ee").join("index");
        write_marker(&index_dir_a, "marker.bin", "index present")?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_a_id = "wsp_statuspath000000000000000a";
        let workspace_b_id = "wsp_statuspath000000000000000b";
        connection
            .insert_workspace(
                workspace_a_id,
                &crate::db::CreateWorkspaceInput {
                    path: default_workspace_root(&workspace_a)
                        .to_string_lossy()
                        .into_owned(),
                    name: Some("status path workspace a".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                workspace_b_id,
                &crate::db::CreateWorkspaceInput {
                    path: default_workspace_root(&workspace_b)
                        .to_string_lossy()
                        .into_owned(),
                    name: Some("status path workspace b".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        let semantic_b = EmbedderStack::from_parts(
            Arc::new(TestSemanticEmbedder::new("semantic-workspace-b-only", 256))
                as Arc<dyn crate::search::Embedder>,
            None,
        );
        ensure_active_embedding_registry_record(&connection, workspace_b_id, &semantic_b)
            .map_err(|error| error.to_string())?;

        let report = get_index_status_with_connection(
            &IndexStatusOptions {
                workspace_path: workspace_a.clone(),
                database_path: Some(database),
                index_dir: Some(index_dir_a),
            },
            Some(&connection),
        )
        .map_err(|error| error.to_string())?;
        let embedding = report
            .embedding
            .ok_or_else(|| "workspace A should resolve to its own embedding posture".to_owned())?;

        ensure(
            embedding.available_model_count == 0,
            format!("workspace A must not borrow workspace B registry rows: {embedding:?}",),
        )?;
        ensure(
            embedding.selected_registry_model.is_none(),
            "workspace A should not expose workspace B as selected registry model",
        )?;
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn cache_invalidation_boundary_condition_equal_generations() {
        for generation in [0_u64, 1, 100, u64::MAX] {
            let health = determine_health(true, 1, Some(generation), Some(generation), false);
            assert_eq!(
                health,
                IndexHealth::Ready,
                "generation {generation} should be ready"
            );
        }
    }

    #[test]
    fn cache_invalidation_boundary_condition_db_one_ahead() {
        let health = determine_health(true, 1, Some(1), Some(0), false);
        assert_eq!(health, IndexHealth::Stale);
    }

    #[test]
    fn format_bytes_produces_human_readable_sizes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GB");
    }

    #[test]
    fn index_status_options_resolve_defaults() {
        let options = IndexStatusOptions {
            workspace_path: PathBuf::from("/home/user/project"),
            database_path: None,
            index_dir: None,
        };

        assert_eq!(
            options.resolve_database_path(),
            PathBuf::from("/home/user/project/.ee/ee.db")
        );
        assert_eq!(
            options.resolve_index_dir(),
            PathBuf::from("/home/user/project/.ee/index")
        );
    }

    #[test]
    fn index_status_options_respect_overrides() {
        let options = IndexStatusOptions {
            workspace_path: PathBuf::from("/home/user/project"),
            database_path: Some(PathBuf::from("/custom/db.sqlite")),
            index_dir: Some(PathBuf::from("/custom/index")),
        };

        assert_eq!(
            options.resolve_database_path(),
            PathBuf::from("/custom/db.sqlite")
        );
        assert_eq!(options.resolve_index_dir(), PathBuf::from("/custom/index"));
    }

    #[test]
    fn index_rebuild_error_has_repair_hints() {
        let db_err = IndexRebuildError::Database(crate::db::DbError::MalformedRow {
            operation: crate::db::DbOperation::Query,
            message: "test".to_string(),
        });
        assert!(db_err.repair_hint().is_some());

        let idx_err = IndexRebuildError::Index("failed".to_string());
        assert!(idx_err.repair_hint().is_some());

        let ws_err = IndexRebuildError::NoWorkspace;
        assert_eq!(ws_err.repair_hint(), Some("ee init --workspace ."));
    }

    #[test]
    fn index_status_error_has_repair_hints() {
        let db_err = IndexStatusError::Database(crate::db::DbError::MalformedRow {
            operation: crate::db::DbOperation::Query,
            message: "test".to_string(),
        });
        assert_eq!(db_err.repair_hint(), Some("ee doctor --json"));

        let io_err = IndexStatusError::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "test",
        ));
        assert_eq!(
            io_err.repair_hint(),
            Some("Check workspace path permissions")
        );
    }

    #[test]
    fn publish_staged_index_retains_previous_generation() -> TestResult {
        let root = unique_test_dir("publish-retains-previous");
        let index_dir = root.join("index");
        let staging_dir = root.join(".index.publish-test");
        write_marker(&index_dir, "generation.txt", "old")?;
        write_marker(&staging_dir, "generation.txt", "new")?;
        write_index_metadata(&staging_dir, 2, 1).map_err(|e| e.to_string())?;

        publish_staged_index(&index_dir, &staging_dir).map_err(|e| e.to_string())?;

        let retained_dir = root.join("index.previous");
        ensure(
            index_dir.is_dir(),
            "active index should exist after publish",
        )?;
        ensure(
            retained_dir.is_dir(),
            "previous active index should be retained",
        )?;
        ensure(
            !staging_dir.exists(),
            "staging path should have moved into active index",
        )?;
        ensure(
            read_marker(&index_dir, "generation.txt")? == "new",
            "active index should contain staged generation",
        )?;
        ensure(
            read_marker(&retained_dir, "generation.txt")? == "old",
            "retained index should contain previous generation",
        )
    }

    #[test]
    fn publish_staged_index_rejects_non_directory_staging_generation() -> TestResult {
        let root = unique_test_dir("publish-file-staging");
        let index_dir = root.join("index");
        let staging_dir = root.join(".index.publish-test");
        write_marker(&index_dir, "generation.txt", "old")?;
        std::fs::write(&staging_dir, "not a directory").map_err(|error| error.to_string())?;

        let error = match publish_staged_index(&index_dir, &staging_dir) {
            Ok(()) => return Err("unexpected publish success".to_owned()),
            Err(error) => error,
        };

        ensure(
            error.to_string().contains("source is not a directory"),
            format!("unexpected error: {error}"),
        )?;
        ensure(index_dir.is_dir(), "active index should be restored")?;
        ensure(
            read_marker(&index_dir, "generation.txt")? == "old",
            "failed publish should restore previous generation",
        )?;
        ensure(
            staging_dir.is_file(),
            "rejected non-directory staging path should remain for inspection",
        )
    }

    #[cfg(unix)]
    #[test]
    fn create_publish_staging_dir_rejects_symlinked_index_parent() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = unique_test_dir("publish-symlink-parent");
        let real_parent = root.join("real-index-root");
        let linked_parent = root.join("linked-index-root");
        std::fs::create_dir_all(&real_parent).map_err(|error| error.to_string())?;
        symlink(&real_parent, &linked_parent).map_err(|error| error.to_string())?;

        let error = match create_publish_staging_dir(&linked_parent.join("index")) {
            Ok(path) => return Err(format!("unexpected staging dir: {}", path.display())),
            Err(error) => error,
        };

        ensure(
            error.to_string().contains("symlinked index path component"),
            format!("unexpected error: {error}"),
        )?;
        ensure(
            std::fs::read_dir(&real_parent)
                .map_err(|error| error.to_string())?
                .next()
                .is_none(),
            "staging creation must not write through symlinked parent",
        )
    }

    #[cfg(unix)]
    #[test]
    fn publish_staged_index_rejects_symlinked_active_index() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = unique_test_dir("publish-symlink-active");
        let outside = root.join("outside-active");
        let index_dir = root.join("index");
        let staging_dir = root.join(".index.publish-test");
        write_marker(&outside, "generation.txt", "outside")?;
        write_marker(&staging_dir, "generation.txt", "new")?;
        symlink(&outside, &index_dir).map_err(|error| error.to_string())?;

        let error = match publish_staged_index(&index_dir, &staging_dir) {
            Ok(()) => return Err("unexpected publish success".to_owned()),
            Err(error) => error,
        };

        ensure(
            error.to_string().contains("symlinked index path component"),
            format!("unexpected error: {error}"),
        )?;
        ensure(
            read_marker(&outside, "generation.txt")? == "outside",
            "publish must not mutate outside symlink target",
        )?;
        ensure(
            staging_dir.is_dir(),
            "rejected publish should leave staging directory intact",
        )
    }

    #[cfg(unix)]
    #[test]
    fn publish_staged_index_skips_dangling_retained_generation_symlink() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = unique_test_dir("publish-dangling-retained");
        let index_dir = root.join("index");
        let staging_dir = root.join(".index.publish-test");
        let retained_link = root.join("index.previous");
        write_marker(&index_dir, "generation.txt", "old")?;
        write_marker(&staging_dir, "generation.txt", "new")?;
        write_index_metadata(&staging_dir, 2, 1).map_err(|e| e.to_string())?;
        symlink(root.join("missing-retained-target"), &retained_link)
            .map_err(|error| error.to_string())?;

        publish_staged_index(&index_dir, &staging_dir).map_err(|error| error.to_string())?;

        let retained_dir = root.join("index.previous.001");
        ensure(
            index_dir.is_dir(),
            "active index should exist after publish",
        )?;
        ensure(
            retained_dir.is_dir(),
            "dangling retained symlink should force allocation of a later retained path",
        )?;
        ensure(
            std::fs::symlink_metadata(&retained_link)
                .map_err(|error| error.to_string())?
                .file_type()
                .is_symlink(),
            "dangling retained symlink should remain untouched",
        )?;
        ensure(
            read_marker(&index_dir, "generation.txt")? == "new",
            "active index should contain staged generation",
        )?;
        ensure(
            read_marker(&retained_dir, "generation.txt")? == "old",
            "retained index should contain previous generation",
        )
    }

    #[cfg(unix)]
    #[test]
    fn recover_interrupted_publish_rejects_symlinked_retained_generation() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = unique_test_dir("recover-symlink-retained");
        let index_dir = root.join("index");
        let retained_dir = root.join("index.previous");
        let outside = root.join("outside-retained");
        write_marker(&outside, "generation.txt", "outside")?;
        symlink(&outside, &retained_dir).map_err(|error| error.to_string())?;

        let error = match recover_interrupted_publish(&index_dir) {
            Ok(action) => return Err(format!("unexpected recovery action: {action:?}")),
            Err(error) => error,
        };

        ensure(
            error.to_string().contains("symlinked index path component"),
            format!("unexpected error: {error}"),
        )?;
        ensure(
            !index_dir.exists(),
            "recovery must not publish a symlinked retained generation",
        )?;
        ensure(
            read_marker(&outside, "generation.txt")? == "outside",
            "recovery must not mutate outside symlink target",
        )
    }

    #[cfg(unix)]
    #[test]
    fn recover_interrupted_publish_ignores_staging_with_symlinked_metadata() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = unique_test_dir("recover-symlink-metadata");
        let index_dir = root.join("index");
        let staging_dir = root.join(".index.publish-20260501-000");
        let outside_meta = root.join("outside-meta.json");
        std::fs::create_dir_all(&staging_dir).map_err(|error| error.to_string())?;
        std::fs::write(&outside_meta, "{}").map_err(|error| error.to_string())?;
        symlink(&outside_meta, staging_dir.join(INDEX_METADATA_FILE))
            .map_err(|error| error.to_string())?;

        let action = recover_interrupted_publish(&index_dir).map_err(|error| error.to_string())?;

        ensure(
            action == IndexPublishRecoveryAction::NoRecoverableGeneration,
            format!("unexpected recovery action: {action:?}"),
        )?;
        ensure(
            !index_dir.exists(),
            "staging with symlinked metadata should not become active",
        )?;
        ensure(
            staging_dir.is_dir(),
            "rejected staging directory should remain in place",
        )
    }

    #[test]
    fn recover_interrupted_publish_restores_retained_generation() -> TestResult {
        let root = unique_test_dir("recover-retained");
        let index_dir = root.join("index");
        let retained_dir = root.join("index.previous");
        write_marker(&retained_dir, "generation.txt", "old")?;

        let action = recover_interrupted_publish(&index_dir).map_err(|e| e.to_string())?;

        ensure(
            action == IndexPublishRecoveryAction::RetainedGenerationRestored,
            format!("unexpected recovery action: {action:?}"),
        )?;
        ensure(index_dir.is_dir(), "active index should be restored")?;
        ensure(
            !retained_dir.exists(),
            "retained path should have moved back to active index",
        )?;
        ensure(
            read_marker(&index_dir, "generation.txt")? == "old",
            "restored active index should contain retained generation",
        )
    }

    #[test]
    fn recover_interrupted_publish_promotes_complete_staging_generation() -> TestResult {
        let root = unique_test_dir("recover-staging");
        let index_dir = root.join("index");
        let staging_dir = root.join(".index.publish-20260501-000");
        write_marker(&staging_dir, "generation.txt", "new")?;
        write_index_metadata(&staging_dir, 3, 1).map_err(|e| e.to_string())?;

        let action = recover_interrupted_publish(&index_dir).map_err(|e| e.to_string())?;

        ensure(
            action == IndexPublishRecoveryAction::StagedGenerationPromoted,
            format!("unexpected recovery action: {action:?}"),
        )?;
        ensure(index_dir.is_dir(), "complete staging should become active")?;
        ensure(
            !staging_dir.exists(),
            "staging path should have moved into active index",
        )?;
        ensure(
            read_marker(&index_dir, "generation.txt")? == "new",
            "active index should contain completed staged generation",
        )
    }

    #[test]
    fn recover_interrupted_publish_leaves_incomplete_staging_generation() -> TestResult {
        let root = unique_test_dir("recover-incomplete");
        let index_dir = root.join("index");
        let staging_dir = root.join(".index.publish-20260501-000");
        write_marker(&staging_dir, "generation.txt", "partial")?;

        let action = recover_interrupted_publish(&index_dir).map_err(|e| e.to_string())?;

        ensure(
            action == IndexPublishRecoveryAction::NoRecoverableGeneration,
            format!("unexpected recovery action: {action:?}"),
        )?;
        ensure(
            !index_dir.exists(),
            "incomplete staging should not be promoted",
        )?;
        ensure(
            staging_dir.is_dir(),
            "incomplete staging should be left intact",
        )
    }

    #[test]
    fn write_index_metadata_is_read_by_status_metadata_reader() -> TestResult {
        let root = unique_test_dir("metadata-roundtrip");
        let index_dir = root.join("index");
        std::fs::create_dir_all(&index_dir).map_err(|e| e.to_string())?;

        write_index_metadata(&index_dir, 42, 7).map_err(|e| e.to_string())?;
        let (generation, rebuilt_at, check_error) = read_index_metadata(&index_dir);

        ensure(
            generation == Some(42),
            "metadata generation should round-trip",
        )?;
        let metadata_json = std::fs::read_to_string(index_dir.join(INDEX_METADATA_FILE))
            .map_err(|e| e.to_string())?;
        let metadata: serde_json::Value =
            serde_json::from_str(&metadata_json).map_err(|e| e.to_string())?;
        ensure(
            metadata["sourceGeneration"] == serde_json::json!(42),
            "metadata should expose sourceGeneration",
        )?;
        ensure(
            rebuilt_at.is_some(),
            "metadata should include last rebuild timestamp",
        )?;
        ensure(
            check_error.is_none(),
            format!("metadata should not report check error: {check_error:?}"),
        )
    }

    #[test]
    fn write_index_metadata_rejects_non_regular_metadata_path() -> TestResult {
        let root = unique_test_dir("metadata-write-directory");
        let index_dir = root.join("index");
        let metadata_dir = index_dir.join(INDEX_METADATA_FILE);
        std::fs::create_dir_all(&metadata_dir).map_err(|error| error.to_string())?;

        let error = write_index_metadata(&index_dir, 42, 7)
            .map(|()| "unexpected metadata write success".to_owned())
            .expect_err("metadata directory should reject before File::create");

        ensure(
            error.to_string().contains("not a regular file"),
            format!("unexpected metadata write error: {error}"),
        )?;
        ensure(
            metadata_dir.is_dir(),
            "metadata directory must be left untouched",
        )
    }

    #[test]
    fn write_index_metadata_ignores_stale_legacy_temp_without_truncating() -> TestResult {
        let root = unique_test_dir("metadata-write-existing-temp");
        let index_dir = root.join("index");
        std::fs::create_dir_all(&index_dir).map_err(|error| error.to_string())?;
        let metadata_path = index_dir.join(INDEX_METADATA_FILE);
        let temp_path = metadata_path.with_extension("json.tmp");
        std::fs::write(&temp_path, "stale metadata temp").map_err(|error| error.to_string())?;

        write_index_metadata(&index_dir, 42, 7).map_err(|error| error.to_string())?;
        ensure(
            std::fs::read_to_string(&temp_path).map_err(|error| error.to_string())?
                == "stale metadata temp",
            "temporary metadata content must remain untouched",
        )?;
        ensure(
            metadata_path.is_file(),
            "metadata should publish through a unique temporary metadata file",
        )?;
        let (generation, rebuilt_at, check_error) = read_index_metadata(&index_dir);
        ensure(
            generation == Some(42),
            "metadata generation should be readable after stale temp bypass",
        )?;
        ensure(
            rebuilt_at.is_some(),
            "metadata rebuild timestamp should be readable after stale temp bypass",
        )?;
        ensure(
            check_error.is_none(),
            format!("metadata read should not report check error: {check_error:?}"),
        )
    }

    #[cfg(unix)]
    #[test]
    fn index_metadata_publish_rechecks_temp_symlink_before_rename() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = unique_test_dir("metadata-publish-temp-symlink");
        let index_dir = root.join("index");
        std::fs::create_dir_all(&index_dir).map_err(|error| error.to_string())?;
        let metadata_path = index_dir.join(INDEX_METADATA_FILE);
        let temp_path = metadata_path.with_extension("json.tmp");
        let preserved_temp = index_dir.join("meta-preserved.json.tmp");
        let outside = root.join("outside-meta.json");

        std::fs::write(&temp_path, r#"{"generation":7}"#).map_err(|error| error.to_string())?;
        std::fs::rename(&temp_path, &preserved_temp).map_err(|error| error.to_string())?;
        std::fs::write(&outside, "outside").map_err(|error| error.to_string())?;
        symlink(&outside, &temp_path).map_err(|error| error.to_string())?;

        let error = publish_index_metadata_temp_file(&metadata_path, &temp_path)
            .map(|()| "unexpected metadata publish success".to_owned())
            .expect_err("swapped temporary metadata symlink should reject before rename");

        ensure(
            error.to_string().contains("symlinked index path component"),
            format!("unexpected temporary metadata publish error: {error}"),
        )?;
        ensure(
            !metadata_path.exists(),
            "metadata must not be published through swapped temporary symlink",
        )?;
        ensure(
            std::fs::read_to_string(&outside).map_err(|error| error.to_string())? == "outside",
            "publish must not mutate outside symlink target",
        )?;
        ensure(
            std::fs::symlink_metadata(&temp_path)
                .map_err(|error| error.to_string())?
                .file_type()
                .is_symlink(),
            "rejected temporary symlink should remain for inspection",
        )?;
        ensure(
            std::fs::read_to_string(&preserved_temp).map_err(|error| error.to_string())?
                == r#"{"generation":7}"#,
            "original temporary metadata should remain preserved after simulated swap",
        )
    }

    #[test]
    fn index_status_marks_invalid_metadata_as_corrupt() -> TestResult {
        let root = unique_test_dir("metadata-corrupt-status");
        let index_dir = root.join("index");
        std::fs::create_dir_all(&index_dir).map_err(|e| e.to_string())?;
        std::fs::write(index_dir.join("meta.json"), "{ not-json").map_err(|e| e.to_string())?;

        let report = get_index_status(&IndexStatusOptions {
            workspace_path: root.clone(),
            database_path: Some(root.join("missing.db")),
            index_dir: Some(index_dir),
        })
        .map_err(|error| error.to_string())?;

        ensure(
            report.health == IndexHealth::Corrupt,
            format!("invalid metadata should report corrupt health: {report:?}"),
        )?;
        ensure(
            report.last_check_error.as_deref().is_some_and(|error| {
                error.contains("failed to parse index metadata") && error.contains("meta.json")
            }),
            format!(
                "invalid metadata should preserve parse error detail: {:?}",
                report.last_check_error
            ),
        )?;
        ensure(
            report.data_json()["lastCheckError"].as_str().is_some(),
            "status JSON should expose lastCheckError for corrupt metadata",
        )
    }

    #[test]
    fn index_status_rejects_non_regular_metadata_before_read() -> TestResult {
        let root = unique_test_dir("metadata-directory-status");
        let index_dir = root.join("index");
        std::fs::create_dir_all(index_dir.join(INDEX_METADATA_FILE))
            .map_err(|error| error.to_string())?;

        let report = get_index_status(&IndexStatusOptions {
            workspace_path: root.clone(),
            database_path: Some(root.join("missing.db")),
            index_dir: Some(index_dir),
        })
        .map_err(|error| error.to_string())?;

        ensure(
            report.health == IndexHealth::Corrupt,
            format!("non-regular metadata should report corrupt health: {report:?}"),
        )?;
        ensure(
            report.last_check_error.as_deref().is_some_and(|error| {
                error.contains("index metadata")
                    && error.contains("meta.json")
                    && error.contains("not a regular file")
            }),
            format!(
                "non-regular metadata should preserve path-type error: {:?}",
                report.last_check_error
            ),
        )
    }

    #[cfg(unix)]
    #[test]
    fn index_status_rejects_symlinked_metadata_before_read() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = unique_test_dir("metadata-symlink-status");
        let index_dir = root.join("index");
        let outside_meta = root.join("outside-meta.json");
        std::fs::create_dir_all(&index_dir).map_err(|error| error.to_string())?;
        std::fs::write(&outside_meta, r#"{"generation": 99}"#)
            .map_err(|error| error.to_string())?;
        symlink(&outside_meta, index_dir.join(INDEX_METADATA_FILE))
            .map_err(|error| error.to_string())?;

        let report = get_index_status(&IndexStatusOptions {
            workspace_path: root,
            database_path: None,
            index_dir: Some(index_dir),
        })
        .map_err(|error| error.to_string())?;

        ensure(
            report.health == IndexHealth::Corrupt,
            format!("symlinked metadata should report corrupt health: {report:?}"),
        )?;
        ensure(
            report.last_check_error.as_deref().is_some_and(|error| {
                error.contains("index metadata")
                    && error.contains("meta.json")
                    && error.contains("symlinked path component")
            }),
            format!(
                "symlinked metadata should preserve path-type error: {:?}",
                report.last_check_error
            ),
        )
    }

    #[test]
    fn index_vacuum_status_as_str_is_stable() {
        assert_eq!(IndexVacuumStatus::Ready.as_str(), "ready");
        assert_eq!(IndexVacuumStatus::Preview.as_str(), "preview");
        assert_eq!(IndexVacuumStatus::Missing.as_str(), "missing");
        assert_eq!(IndexVacuumStatus::Stale.as_str(), "stale");
        assert_eq!(IndexVacuumStatus::Locked.as_str(), "locked");
        assert_eq!(IndexVacuumStatus::Corrupt.as_str(), "corrupt");
    }

    #[test]
    fn index_vacuum_degraded_entries_are_aggregated() -> TestResult {
        let root = unique_test_dir("vacuum-degraded-aggregation");
        let report = IndexVacuumReport {
            status: IndexVacuumStatus::Stale,
            database_path: root.join(".ee").join("ee.db"),
            index_dir: root.join(".ee").join("index"),
            before: IndexPathStats {
                path: root.join(".ee").join("index"),
                exists: true,
                file_count: 1,
                directory_count: 0,
                size_bytes: 128,
            },
            after: IndexPathStats {
                path: root.join(".ee").join("index"),
                exists: true,
                file_count: 1,
                directory_count: 0,
                size_bytes: 128,
            },
            candidate_count: 0,
            reclaimable_bytes: 0,
            candidates: Vec::new(),
            degraded: vec![
                IndexVacuumDegradation {
                    code: "index_stale",
                    severity: "medium",
                    message: "Search index metadata lags behind the database.",
                    repair: "Run `ee index rebuild --workspace <path>` before vacuuming.",
                },
                IndexVacuumDegradation {
                    code: "index_stale",
                    severity: "high",
                    message: "Search index metadata is stale while a publish lock is held.",
                    repair: "Wait for the lock holder to finish, then rebuild the index.",
                },
            ],
            lock: IndexVacuumLockReport::none(),
            elapsed_ms: 0.0,
        };

        let json = report.data_json();
        let degraded = json["degraded"]
            .as_array()
            .ok_or_else(|| "vacuum degraded array should be present".to_string())?;

        ensure(
            degraded.len() == 1,
            format!("duplicate degraded codes should collapse: {degraded:?}"),
        )?;
        ensure(
            degraded[0]["code"] == "index_stale",
            "aggregate should preserve the degraded code",
        )?;
        ensure(
            degraded[0]["severity"] == "high",
            "aggregate should escalate to the worst severity",
        )?;
        ensure(
            degraded[0]["repair"] == "Wait for the lock holder to finish, then rebuild the index.",
            "aggregate should keep the highest-severity repair hint",
        )?;
        ensure(
            degraded[0]["sources"] == serde_json::json!(["index_vacuum"]),
            "aggregate should expose the index vacuum source label",
        )
    }

    #[test]
    fn index_vacuum_discovers_staging_and_retained_candidates() -> TestResult {
        let root = unique_test_dir("vacuum-candidates");
        let index_dir = root.join("index");
        write_marker(
            &root.join(".index.publish-200-000"),
            "fragment.bin",
            "partial",
        )?;
        write_marker(&root.join(".index.publish-300-000"), "meta.json", "{}")?;
        write_marker(&root.join("index.previous"), "old.bin", "old")?;
        write_marker(&root.join("index.previous.001"), "older.bin", "older")?;
        write_marker(&index_dir, "meta.json", "{}")?;

        let candidates =
            discover_index_vacuum_candidates(&index_dir).map_err(|error| error.to_string())?;

        ensure(candidates.len() == 4, "expected four vacuum candidates")?;
        let kinds = candidates
            .iter()
            .map(|candidate| candidate.kind.as_str())
            .collect::<Vec<_>>();
        ensure(
            kinds.contains(&"incomplete_staging"),
            format!("candidate kinds should include incomplete staging: {kinds:?}"),
        )?;
        ensure(
            kinds.contains(&"staged_generation"),
            format!("candidate kinds should include staged generation: {kinds:?}"),
        )?;
        ensure(
            kinds
                .iter()
                .filter(|kind| **kind == "retained_generation")
                .count()
                == 2,
            format!("candidate kinds should include two retained generations: {kinds:?}"),
        )?;
        ensure(
            candidates
                .iter()
                .all(|candidate| candidate.stats.exists && candidate.stats.file_count > 0),
            "candidate stats should describe existing derived artifacts",
        )
    }

    #[test]
    fn index_vacuum_missing_index_reports_preview_only_degradation() -> TestResult {
        let root = unique_test_dir("vacuum-missing");
        let report = get_index_vacuum_report(&IndexVacuumOptions {
            workspace_path: root.clone(),
            database_path: Some(root.join(".ee").join("ee.db")),
            index_dir: Some(root.join(".ee").join("index")),
        })
        .map_err(|error| error.to_string())?;

        ensure(
            report.status == IndexVacuumStatus::Missing,
            format!("missing index should be reported as missing: {report:?}"),
        )?;
        ensure(!report.before.exists, "missing index before stats")?;
        ensure(!report.after.exists, "missing index after stats")?;
        ensure(
            report.candidate_count == 0,
            "missing index has no candidates",
        )?;

        let json = report.data_json();
        ensure(json["command"] == "index_vacuum", "vacuum command JSON")?;
        ensure(json["dryRun"] == true, "vacuum is always dry-run")?;
        ensure(
            json["mutationAllowed"] == false,
            "vacuum never allows mutation",
        )?;
        ensure(
            json["degraded"][0]["code"] == "index_missing",
            "missing index degradation code",
        )
    }

    #[test]
    fn index_vacuum_reports_active_publish_lock_without_mutation() -> TestResult {
        let root = unique_test_dir("vacuum-lock");
        let database = root.join(".ee").join("ee.db");
        let index_dir = root.join(".ee").join("index");
        let parent = database
            .parent()
            .ok_or_else(|| "database path must have parent".to_string())?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&index_dir).map_err(|error| error.to_string())?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_00000000000000000000000001";
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: root.to_string_lossy().into_owned(),
                    name: Some("vacuum lock test".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        let lock_id = AdvisoryLockId::index(workspace_id);
        let lock = connection
            .acquire_advisory_lock(&lock_id, "agent_holding", Some(300), Some("unit test"))
            .map_err(|error| error.to_string())?;
        ensure(
            matches!(
                lock,
                AcquireLockResult::Acquired(_) | AcquireLockResult::Expired { .. }
            ),
            "test lock should be acquired",
        )?;
        write_index_metadata(&index_dir, 0, 0).map_err(|error| error.to_string())?;

        let report = get_index_vacuum_report(&IndexVacuumOptions {
            workspace_path: root,
            database_path: Some(database),
            index_dir: Some(index_dir),
        })
        .map_err(|error| error.to_string())?;

        ensure(
            report.status == IndexVacuumStatus::Locked,
            format!("held publish lock should report locked status: {report:?}"),
        )?;
        ensure(report.lock.held, "lock report should mark held")?;
        ensure(
            report.lock.holder_id.as_deref() == Some("agent_holding"),
            "lock report should identify holder",
        )?;
        ensure(
            report
                .degraded
                .iter()
                .any(|degraded| degraded.code == "index_locked"),
            "lock degradation should be present",
        )
    }

    #[test]
    fn index_vacuum_lock_resolution_uses_requested_workspace_path() -> TestResult {
        let root = unique_test_dir("vacuum-lock-target-workspace");
        let target_workspace = root.join("target");
        let other_workspace = root.join("other");
        let database = root.join(".ee").join("ee.db");
        let index_dir = target_workspace.join(".ee").join("index");
        let parent = database
            .parent()
            .ok_or_else(|| "database path must have parent".to_string())?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&index_dir).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&other_workspace).map_err(|error| error.to_string())?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let target_workspace_id = crate::testing::wsp("vacuumtarget");
        let target_workspace_id = target_workspace_id.as_str();
        let other_workspace_id = crate::testing::wsp("vacuumother");
        let other_workspace_id = other_workspace_id.as_str();
        connection
            .insert_workspace(
                target_workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: default_workspace_root(&target_workspace)
                        .to_string_lossy()
                        .into_owned(),
                    name: Some("vacuum target workspace".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        std::thread::sleep(std::time::Duration::from_millis(2));
        connection
            .insert_workspace(
                other_workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: default_workspace_root(&other_workspace)
                        .to_string_lossy()
                        .into_owned(),
                    name: Some("newer unrelated workspace".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        let target_lock_id = AdvisoryLockId::index(target_workspace_id);
        let lock = connection
            .acquire_advisory_lock(
                &target_lock_id,
                "target_holder",
                Some(300),
                Some("target workspace lock"),
            )
            .map_err(|error| error.to_string())?;
        ensure(
            matches!(
                lock,
                AcquireLockResult::Acquired(_) | AcquireLockResult::Expired { .. }
            ),
            "target lock should be acquired",
        )?;
        write_index_metadata(&index_dir, 0, 0).map_err(|error| error.to_string())?;

        let report = get_index_vacuum_report(&IndexVacuumOptions {
            workspace_path: target_workspace,
            database_path: Some(database),
            index_dir: Some(index_dir),
        })
        .map_err(|error| error.to_string())?;

        ensure(
            report.status == IndexVacuumStatus::Locked,
            format!(
                "target workspace publish lock should report locked status even when another workspace row is newer: {report:?}"
            ),
        )?;
        ensure(
            report.lock.lock_id.as_deref() == Some(target_lock_id.canonical_key().as_str()),
            "lock report should use the requested workspace lock id",
        )?;
        ensure(
            report.lock.holder_id.as_deref() == Some("target_holder"),
            "lock report should identify the target workspace holder",
        )
    }
}
