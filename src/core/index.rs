use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::core::degraded_aggregation::{DegradationAggregationInput, aggregate_degraded_entries};
use crate::core::profile::{RuntimeProfileReport, runtime_profile_for_workspace};
use crate::db::{
    AcquireLockResult, AdvisoryLockId, CreateSearchIndexJobInput, DbConnection, DbError,
    SearchIndexJobType, StoredSearchIndexJob,
};
use crate::models::MemoryId;
use crate::search::{
    CanonicalSearchDocument, Embedder, EmbedderStack, HashEmbedder, IndexBuilder,
    artifact_to_document, memory_to_document, session_to_document,
};
use sqlmodel_core::Value as SqlValue;

pub const DEFAULT_INDEX_SUBDIR: &str = "index";
const INDEX_METADATA_FILE: &str = "meta.json";
const INDEX_STAGING_PREFIX: &str = ".publish-";
const INDEX_RETAINED_SUFFIX: &str = ".previous";
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
                        std::thread::sleep(delay);
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
            .unwrap_or_else(|| self.workspace_path.join(".ee").join("ee.db"))
    }

    fn resolve_index_dir(&self) -> PathBuf {
        self.index_dir
            .clone()
            .unwrap_or_else(|| self.workspace_path.join(".ee").join(DEFAULT_INDEX_SUBDIR))
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
            .unwrap_or_else(|| self.workspace_path.join(".ee").join("ee.db"))
    }

    fn resolve_index_dir(&self) -> PathBuf {
        self.index_dir
            .clone()
            .unwrap_or_else(|| self.workspace_path.join(".ee").join(DEFAULT_INDEX_SUBDIR))
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
            .unwrap_or_else(|| self.workspace_path.join(".ee").join("ee.db"))
    }

    fn resolve_index_dir(&self) -> PathBuf {
        self.index_dir
            .clone()
            .unwrap_or_else(|| self.workspace_path.join(".ee").join(DEFAULT_INDEX_SUBDIR))
    }
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
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
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
}

impl ReembedRegistryModelSummary {
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
    let (_, _, db_generation) = get_db_stats(&db)?;

    let workspace_id = get_default_workspace_id(&db)?;

    let memories = db.list_memories_for_retrieval(&workspace_id, None, false)?;
    let sessions = db.list_sessions(&workspace_id)?;
    let artifacts = db.list_artifacts(&workspace_id, None)?;

    let memory_docs: Vec<CanonicalSearchDocument> =
        memories.iter().map(memory_to_document).collect();
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

        let build_result = build_index_sync(&staging_dir, stack, indexable_docs);

        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

        match build_result {
            Ok(stats) => {
                let published_generation =
                    db_generation.unwrap_or_else(|| u64::from(documents_total));
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

    let memories = db.list_memories_for_retrieval(&workspace_id, None, false)?;
    let sessions = db.list_sessions(&workspace_id)?;
    let artifacts = db.list_artifacts(&workspace_id, None)?;
    let embedding = reembed_embedding_summary(&db, &workspace_id)?;

    let memory_docs: Vec<CanonicalSearchDocument> =
        memories.iter().map(memory_to_document).collect();
    let session_docs: Vec<CanonicalSearchDocument> =
        sessions.iter().map(session_to_document).collect();
    let artifact_docs: Vec<CanonicalSearchDocument> =
        artifacts.iter().map(artifact_to_document).collect();

    let (memories_indexed, sessions_indexed, artifacts_indexed, documents_total) =
        checked_document_counts(memory_docs.len(), session_docs.len(), artifact_docs.len())?;
    let idempotency_key = reembed_idempotency_key(
        &workspace_id,
        &embedding.fast_model_id,
        embedding.quality_model_id.as_deref(),
        documents_total,
    );

    if options.dry_run {
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
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

        let build_result = build_index_sync(&staging_dir, default_embedder_stack(), indexable_docs);
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

        match build_result {
            Ok(stats) => {
                db.update_search_index_job_progress(&job_id, documents_total)?;
                write_index_metadata(&staging_dir, u64::from(documents_total), documents_total)
                    .and_then(|()| publish_staged_index(&index_dir, &staging_dir))?;
                db.complete_search_index_job(&job_id, documents_total)?;

                Ok(IndexReembedReport {
                    status: IndexReembedStatus::Success,
                    job_id: Some(job_id),
                    job_status: "completed".to_owned(),
                    job_type: SearchIndexJobType::FullRebuild.as_str().to_owned(),
                    document_source: None,
                    embedding_scope: "all_documents".to_owned(),
                    embedding,
                    memories_indexed,
                    sessions_indexed,
                    artifacts_indexed,
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
    let processing_mode = processing_mode_for_job(job).to_owned();
    if !db.start_search_index_job(&job.id)? {
        return Ok(IndexProcessingJobReport {
            job_id: job.id.clone(),
            job_type: job.job_type.clone(),
            document_source: job.document_source.clone(),
            document_id: job.document_id.clone(),
            outcome: "skipped".to_owned(),
            processing_mode,
            documents_total: job.documents_total,
            documents_indexed: job.documents_indexed,
            error: Some("search index job was not pending".to_owned()),
        });
    }

    let (_memories_indexed, _sessions_indexed, documents_total, indexable_docs) =
        collect_workspace_indexable_documents(db, &job.workspace_id)?;
    db.update_search_index_job_total(&job.id, documents_total)?;

    if documents_total == 0 {
        db.complete_search_index_job(&job.id, 0)?;
        return Ok(IndexProcessingJobReport {
            job_id: job.id.clone(),
            job_type: job.job_type.clone(),
            document_source: job.document_source.clone(),
            document_id: job.document_id.clone(),
            outcome: "completed_no_documents".to_owned(),
            processing_mode,
            documents_total: 0,
            documents_indexed: 0,
            error: None,
        });
    }

    // Acquire index publish lock to prevent concurrent publish races.
    let holder_id = generate_index_holder_id();
    acquire_index_publish_lock(db, &job.workspace_id, &holder_id)?;

    let result = (|| -> Result<IndexProcessingJobReport, IndexRebuildError> {
        let _recovery_action = recover_interrupted_publish(index_dir)?;
        let staging_dir = create_publish_staging_dir(index_dir)?;
        let build_result = build_index_sync(&staging_dir, default_embedder_stack(), indexable_docs)
            .and_then(|stats| {
                write_index_metadata(&staging_dir, u64::from(documents_total), documents_total)
                    .and_then(|()| publish_staged_index(index_dir, &staging_dir))
                    .map_err(|error| error.to_string())?;
                Ok(stats)
            });

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

fn collect_workspace_indexable_documents(
    db: &DbConnection,
    workspace_id: &str,
) -> Result<(u32, u32, u32, Vec<crate::search::IndexableDocument>), IndexRebuildError> {
    let memories = db.list_memories_for_retrieval(workspace_id, None, false)?;
    let sessions = db.list_sessions(workspace_id)?;
    let artifacts = db.list_artifacts(workspace_id, None)?;
    let memory_docs: Vec<CanonicalSearchDocument> =
        memories.iter().map(memory_to_document).collect();
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

    if index_dir.exists() {
        return Ok(IndexPublishRecoveryAction::ActivePresent);
    }

    let retained_dir = retained_index_dir(index_dir)?;
    if retained_dir.exists() {
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

    if !staging_dir.exists() {
        return Err(IndexRebuildError::Index(format!(
            "Index staging directory does not exist: {}",
            staging_dir.display()
        )));
    }

    let retained_dir = if index_dir.exists() {
        let retained = allocate_retained_index_dir(index_dir)?;
        rename_index_dir(index_dir, &retained, "retain previous index generation")?;
        Some(retained)
    } else {
        None
    };

    if let Err(error) = rename_index_dir(staging_dir, index_dir, "publish staged index generation")
    {
        if let Some(retained) = retained_dir
            && !index_dir.exists()
        {
            let _ = rename_index_dir(&retained, index_dir, "restore previous index generation");
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
        "lastRebuildAt": timestamp,
        "documentCount": documents_total,
    });
    let serialized = serde_json::to_vec_pretty(&metadata).map_err(|e| {
        IndexRebuildError::Index(format!("Failed to serialize index metadata: {e}"))
    })?;

    let meta_path = index_dir.join(INDEX_METADATA_FILE);
    ensure_index_path_has_no_symlinks(&meta_path, "write index metadata")?;
    let mut file = std::fs::File::create(&meta_path).map_err(|e| {
        IndexRebuildError::Index(format!("Failed to open index metadata for writing: {e}"))
    })?;

    use std::io::Write;
    file.write_all(&serialized)
        .map_err(|e| IndexRebuildError::Index(format!("Failed to write index metadata: {e}")))?;

    file.sync_data()
        .map_err(|e| IndexRebuildError::Index(format!("Failed to sync index metadata: {e}")))?;

    Ok(())
}

fn find_complete_staging_dir(index_dir: &Path) -> Result<Option<PathBuf>, IndexRebuildError> {
    let parent = index_parent(index_dir);
    if !parent.exists() {
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
        if !candidate.exists() {
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
    std::fs::rename(from, to).map_err(|e| {
        IndexRebuildError::Index(format!(
            "Failed to {action} from {} to {}: {e}",
            from.display(),
            to.display()
        ))
    })
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

fn default_embedder_stack() -> EmbedderStack {
    let fast_embedder = Arc::new(HashEmbedder::default_256()) as Arc<dyn crate::search::Embedder>;
    let quality_embedder =
        Arc::new(HashEmbedder::default_384()) as Arc<dyn crate::search::Embedder>;
    EmbedderStack::from_parts(fast_embedder, Some(quality_embedder))
}

fn reembed_embedding_summary(
    db: &DbConnection,
    workspace_id: &str,
) -> Result<ReembedEmbeddingSummary, IndexRebuildError> {
    let fast_embedder = HashEmbedder::default_256();
    let quality_embedder = HashEmbedder::default_384();
    let records = db.list_embedding_metadata_records(workspace_id)?;
    let selected_registry_model = records
        .iter()
        .find(|record| record.registry.status.as_str() == "available")
        .map(|record| ReembedRegistryModelSummary {
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
    let source = if selected_registry_model.is_some() {
        "registry_observed"
    } else {
        "frankensearch_hash_fallback"
    };

    Ok(ReembedEmbeddingSummary {
        fast_model_id: fast_embedder.id().to_owned(),
        fast_dimension: fast_embedder.dimension(),
        quality_model_id: Some(quality_embedder.id().to_owned()),
        quality_dimension: Some(quality_embedder.dimension()),
        deterministic: true,
        semantic: fast_embedder.is_semantic() || quality_embedder.is_semantic(),
        registered_model_count: records.len(),
        available_model_count,
        selected_registry_model,
        source: source.to_owned(),
    })
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
            .unwrap_or_else(|| self.workspace_path.join(".ee").join("ee.db"))
    }

    fn resolve_index_dir(&self) -> PathBuf {
        self.index_dir
            .clone()
            .unwrap_or_else(|| self.workspace_path.join(".ee").join(DEFAULT_INDEX_SUBDIR))
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
            .unwrap_or_else(|| self.workspace_path.join(".ee").join("ee.db"))
    }

    fn resolve_index_dir(&self) -> PathBuf {
        self.index_dir
            .clone()
            .unwrap_or_else(|| self.workspace_path.join(".ee").join(DEFAULT_INDEX_SUBDIR))
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
    let start = Instant::now();
    let database_path = options.resolve_database_path();
    let index_dir = options.resolve_index_dir();

    // Check index directory
    let (index_exists, index_file_count, index_size_bytes) = inspect_index_dir(&index_dir)?;

    // Fast-path degraded states: when the index is missing/corrupt, we can
    // report health without scanning DB tables for counts/generation.
    let (db_memory_count, db_session_count, db_generation) =
        if !index_exists || index_file_count == 0 {
            (0, 0, None)
        } else if database_path.exists() {
            let db = DbConnection::open_file(&database_path)?;
            get_db_stats(&db)?
        } else {
            (0, 0, None)
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
    let lock = inspect_index_vacuum_lock(&database_path)?;
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
) -> Result<IndexVacuumLockReport, IndexStatusError> {
    if !database_path.exists() {
        return Ok(IndexVacuumLockReport::none());
    }
    let db = DbConnection::open_file(database_path)?;
    let Some(workspace_id) = latest_workspace_id_for_vacuum(&db)? else {
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

fn latest_workspace_id_for_vacuum(db: &DbConnection) -> Result<Option<String>, DbError> {
    match db.query(
        "SELECT id FROM workspaces ORDER BY created_at DESC LIMIT 1",
        &[],
    ) {
        Ok(rows) => Ok(rows
            .first()
            .and_then(|row| row.get(0).and_then(|value| value.as_str()))
            .map(str::to_owned)),
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

    let generation = Some(source_document_count.max(audit_count));

    Ok((memory_count, session_count, generation))
}

fn read_index_metadata(index_dir: &Path) -> (Option<u64>, Option<String>, Option<String>) {
    let meta_path = index_dir.join("meta.json");
    if !meta_path.exists() {
        return (None, None, None);
    }

    let content = match std::fs::read_to_string(&meta_path) {
        Ok(c) => c,
        Err(error) => {
            return (
                None,
                None,
                Some(format!(
                    "failed to read index metadata '{}': {error}",
                    meta_path.display()
                )),
            );
        }
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

    let generation = parsed.get("generation").and_then(|v| v.as_u64());
    let last_rebuild = parsed
        .get("lastRebuildAt")
        .or_else(|| parsed.get("last_rebuild_at"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    (generation, last_rebuild, None)
}

fn determine_health(
    index_exists: bool,
    index_file_count: u32,
    db_generation: Option<u64>,
    index_generation: Option<u64>,
    metadata_corrupt: bool,
) -> IndexHealth {
    if !index_exists || index_file_count == 0 {
        return IndexHealth::Missing;
    }

    if metadata_corrupt {
        return IndexHealth::Corrupt;
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
    use crate::core::profile::OperatingProfile;

    type TestResult = Result<(), String>;

    fn test_runtime_profile() -> RuntimeProfileReport {
        RuntimeProfileReport::for_profile(OperatingProfile::Workstation, "test_fixture")
    }

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
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
            embedding: ReembedEmbeddingSummary {
                fast_model_id: "fnv1a-256".to_owned(),
                fast_dimension: 256,
                quality_model_id: Some("fnv1a-384".to_owned()),
                quality_dimension: Some(384),
                deterministic: true,
                semantic: false,
                registered_model_count: 0,
                available_model_count: 0,
                selected_registry_model: None,
                source: "frankensearch_hash_fallback".to_owned(),
            },
            memories_indexed: 5,
            sessions_indexed: 3,
            artifacts_indexed: 2,
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
        assert_eq!(json["memories_indexed"], 5);
        assert_eq!(json["sessions_indexed"], 3);
        assert_eq!(json["artifacts_indexed"], 2);
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

        let connection = DbConnection::open_file(database).map_err(|e| e.to_string())?;
        let jobs = connection
            .list_search_index_jobs("wsp_01234567890123456789012345", None)
            .map_err(|e| e.to_string())?;
        ensure(jobs.is_empty(), "dry-run must not queue search index jobs")?;
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
}
