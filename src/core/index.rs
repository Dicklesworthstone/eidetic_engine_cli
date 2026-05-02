use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::db::{
    CreateSearchIndexJobInput, DbConnection, DbError, SearchIndexJobType, StoredSearchIndexJob,
};
use crate::models::MemoryId;
use crate::search::{
    CanonicalSearchDocument, Embedder, EmbedderStack, HashEmbedder, IndexBuilder,
    artifact_to_document, memory_to_document, session_to_document,
};

pub const DEFAULT_INDEX_SUBDIR: &str = "index";
const INDEX_METADATA_FILE: &str = "meta.json";
const INDEX_STAGING_PREFIX: &str = ".publish-";
const INDEX_RETAINED_SUFFIX: &str = ".previous";

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
pub enum IndexRebuildError {
    Database(DbError),
    Index(String),
    NoWorkspace,
}

impl IndexRebuildError {
    #[must_use]
    pub fn repair_hint(&self) -> Option<&str> {
        match self {
            Self::Database(_) => Some("ee doctor --fix-plan --json"),
            Self::Index(_) => Some("Check index directory permissions"),
            Self::NoWorkspace => Some("ee init --workspace ."),
        }
    }
}

impl std::fmt::Display for IndexRebuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(e) => write!(f, "Database error: {e}"),
            Self::Index(e) => write!(f, "Index error: {e}"),
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

    let db = DbConnection::open_file(&database_path)?;
    let (_, _, db_generation) = get_db_stats(&db)?;

    let workspace_id = get_default_workspace_id(&db)?;

    let memories = db.list_memories(&workspace_id, None, false)?;
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
        });
    }

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
            let published_generation = db_generation.unwrap_or_else(|| u64::from(documents_total));
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
        }),
    }
}

pub fn reembed_index(
    options: &IndexReembedOptions,
) -> Result<IndexReembedReport, IndexRebuildError> {
    let start = Instant::now();
    let database_path = options.resolve_database_path();
    let index_dir = options.resolve_index_dir();

    let db = DbConnection::open_file(&database_path)?;
    let workspace_id = get_default_workspace_id(&db)?;

    let memories = db.list_memories(&workspace_id, None, false)?;
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
        });
    }

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
            })
        }
    }
}

pub fn process_index_jobs(
    options: &IndexProcessingOptions,
) -> Result<IndexProcessingReport, IndexRebuildError> {
    let start = Instant::now();
    let database_path = options.resolve_database_path();
    let index_dir = options.resolve_index_dir();

    let db = DbConnection::open_file(&database_path)?;
    let workspace_id = get_default_workspace_id(&db)?;
    let pending_jobs = db.list_pending_search_index_jobs(&workspace_id, options.job_limit)?;
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
            job_limit: options.job_limit,
            elapsed_ms,
            jobs,
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
            job_limit: options.job_limit,
            elapsed_ms,
            jobs: Vec::new(),
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
        job_limit: options.job_limit,
        elapsed_ms,
        jobs,
    })
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
}

fn collect_workspace_indexable_documents(
    db: &DbConnection,
    workspace_id: &str,
) -> Result<(u32, u32, u32, Vec<crate::search::IndexableDocument>), IndexRebuildError> {
    let memories = db.list_memories(workspace_id, None, false)?;
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
    std::fs::write(index_dir.join(INDEX_METADATA_FILE), serialized)
        .map_err(|e| IndexRebuildError::Index(format!("Failed to write index metadata: {e}")))
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
        if name.starts_with(&prefix) && entry.path().join(INDEX_METADATA_FILE).is_file() {
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
    std::fs::rename(from, to).map_err(|e| {
        IndexRebuildError::Index(format!(
            "Failed to {action} from {} to {}: {e}",
            from.display(),
            to.display()
        ))
    })
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

        output.push_str(&format!("  Elapsed: {:.1}ms\n", self.elapsed_ms));

        if let Some(hint) = self.repair_hint {
            output.push_str(&format!("\nNext:\n  {hint}\n"));
        }

        output
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": "index_status",
            "health": self.health.as_str(),
            "degradationCode": self.health.degradation_code(),
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
            "repairHint": self.repair_hint,
            "elapsedMs": self.elapsed_ms,
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

    // Read index metadata if available
    let (index_generation, last_rebuild_at) = read_index_metadata(&index_dir);

    // Determine health
    let health = determine_health(
        index_exists,
        index_file_count,
        db_generation,
        index_generation,
    );

    let repair_hint = match health {
        IndexHealth::Ready => None,
        IndexHealth::Stale | IndexHealth::Missing | IndexHealth::Corrupt => {
            Some("ee index rebuild --workspace .")
        }
    };

    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    Ok(IndexStatusReport {
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
        repair_hint,
        elapsed_ms,
    })
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
        .unwrap_or(0) as u32;

    let session_count = db
        .query("SELECT COUNT(*) FROM sessions", &[])?
        .first()
        .and_then(|row| row.get(0).and_then(|v| v.as_i64()))
        .unwrap_or(0) as u32;

    // Check for a generation marker in search_index_jobs or a dedicated table
    let generation = db
        .query(
            "SELECT MAX(id) FROM search_index_jobs WHERE status = 'completed'",
            &[],
        )
        .ok()
        .and_then(|rows| {
            rows.first()
                .and_then(|row| row.get(0).and_then(|v| v.as_i64()))
        })
        .map(|v| v as u64);

    Ok((memory_count, session_count, generation))
}

fn read_index_metadata(index_dir: &Path) -> (Option<u64>, Option<String>) {
    let meta_path = index_dir.join("meta.json");
    if !meta_path.exists() {
        return (None, None);
    }

    let content = match std::fs::read_to_string(&meta_path) {
        Ok(c) => c,
        Err(_) => return (None, None),
    };

    let parsed: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return (None, None),
    };

    let generation = parsed.get("generation").and_then(|v| v.as_u64());
    let last_rebuild = parsed
        .get("lastRebuildAt")
        .or_else(|| parsed.get("last_rebuild_at"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    (generation, last_rebuild)
}

fn determine_health(
    index_exists: bool,
    index_file_count: u32,
    db_generation: Option<u64>,
    index_generation: Option<u64>,
) -> IndexHealth {
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

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
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
        let health = determine_health(false, 0, Some(10), Some(10));
        assert_eq!(health, IndexHealth::Missing);
        assert_eq!(health.degradation_code(), Some("index_missing"));
    }

    #[test]
    fn cache_invalidation_empty_index_detected() {
        let health = determine_health(true, 0, Some(10), Some(10));
        assert_eq!(health, IndexHealth::Missing);
    }

    #[test]
    fn cache_invalidation_stale_when_db_ahead() {
        let health = determine_health(true, 5, Some(12), Some(9));
        assert_eq!(health, IndexHealth::Stale);
        assert_eq!(health.degradation_code(), Some("index_stale"));
    }

    #[test]
    fn cache_invalidation_stale_when_index_has_no_generation() {
        let health = determine_health(true, 5, Some(12), None);
        assert_eq!(health, IndexHealth::Stale);
    }

    #[test]
    fn cache_invalidation_ready_when_generations_match() {
        let health = determine_health(true, 5, Some(10), Some(10));
        assert_eq!(health, IndexHealth::Ready);
        assert_eq!(health.degradation_code(), None);
    }

    #[test]
    fn cache_invalidation_ready_when_index_ahead() {
        let health = determine_health(true, 5, Some(8), Some(10));
        assert_eq!(health, IndexHealth::Ready);
    }

    #[test]
    fn cache_invalidation_ready_when_no_generations_tracked() {
        let health = determine_health(true, 5, None, None);
        assert_eq!(health, IndexHealth::Ready);
    }

    #[test]
    fn cache_invalidation_ready_when_db_has_no_generation() {
        let health = determine_health(true, 5, None, Some(10));
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
            repair_hint: Some("ee index rebuild --workspace ."),
            elapsed_ms: 5.2,
        };

        let json = report.data_json();
        assert_eq!(json["health"], "stale");
        assert_eq!(json["degradationCode"], "index_stale");
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
            let health = determine_health(true, 1, Some(generation), Some(generation));
            assert_eq!(
                health,
                IndexHealth::Ready,
                "generation {generation} should be ready"
            );
        }
    }

    #[test]
    fn cache_invalidation_boundary_condition_db_one_ahead() {
        let health = determine_health(true, 1, Some(1), Some(0));
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
        let (generation, rebuilt_at) = read_index_metadata(&index_dir);

        ensure(
            generation == Some(42),
            "metadata generation should round-trip",
        )?;
        ensure(
            rebuilt_at.is_some(),
            "metadata should include last rebuild timestamp",
        )
    }
}
