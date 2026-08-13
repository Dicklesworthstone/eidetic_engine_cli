#[cfg(test)]
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
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
    DbOperation, EVIDENCE_CANONICAL_PROVENANCE_REVISION, EVIDENCE_SCREENING_VERSION,
    EVIDENCE_SECURITY_POLICY_EPOCH, EvidenceAdmissionReport, ModelRegistryUpsertOutcome,
    SearchIndexJobStatus, SearchIndexJobType, StoredModelRegistryEntry, StoredSearchIndexJob,
};
use crate::models::model_registry::{
    EmbedModelResolution, EmbedModelSource, EmbedRegistryRejectionReason, EmbeddingMetadataRecord,
    EmbeddingPooling, EmbeddingVectorDtype, ModelDistanceMetric, ModelProvider, ModelPurpose,
    ModelRegistryStatus,
};
use crate::models::{CorpusRevision, INDEX_INTAKE_FALLBACK_CORPUS_REVISION_MISMATCH, MemoryId};
use crate::models::{
    EMBEDDING_POSTURE_MODE_DETERMINISTIC_HASH, EMBEDDING_POSTURE_MODE_NEURAL_LOCAL,
    EMBEDDING_POSTURE_MODE_NEURAL_LOCAL_PENDING, EMBEDDING_POSTURE_SCHEMA_V1, EmbedBackend,
};
use crate::search::{
    ARTIFACT_INDEX_PROJECTION_SCHEMA_V1, CanonicalSearchDocument,
    EVIDENCE_INDEX_PROJECTION_SCHEMA_V1, EmbedderStack, HashEmbedder, IndexBuilder,
    MEMORY_INDEX_PROJECTION_SCHEMA_V1, RULE_INDEX_PROJECTION_SCHEMA_V1, RuleIndexProjection,
    SESSION_INDEX_PROJECTION_SCHEMA_V1, artifact_to_document, evidence_span_to_document,
    memory_to_document_with_context_anchors_and_typed_fields, rule_to_document,
    session_to_document,
};
#[cfg(feature = "lexical-bm25")]
use crate::search::{LexicalRead, LexicalWrite, TantivyIndex};
use asupersync::sync::OnceCell as AsyncOnceCell;
use frankensearch::embed::{
    ConsentSource, DownloadConsent, DownloadProgress, ModelDownloader, ModelLifecycle,
    ModelManifest, verify_dir_cached,
};
use frankensearch::{
    Embedder as _, Model2VecEmbedder, ModelCategory, ModelTier, SearchError, VectorIndex,
};
use sqlmodel_core::Value as SqlValue;

pub const DEFAULT_INDEX_SUBDIR: &str = "index";
const INDEX_METADATA_FILE: &str = "meta.json";
pub const INDEX_METADATA_SCHEMA_V2: &str = "ee.index_metadata.v2";
const INDEX_CORPUS_REVISION_DOMAIN: &[u8] = b"ee.index.corpus_revision.v1\0";
const MEMORY_INDEX_ELIGIBILITY_REVISION_V2: &str = "ee.memory_index_eligibility.v2";
const SESSION_INDEX_ELIGIBILITY_REVISION_V1: &str = "ee.session_index_eligibility.v1";
const ARTIFACT_INDEX_ELIGIBILITY_REVISION_V1: &str = "ee.artifact_index_eligibility.v1";
const RULE_INDEX_ELIGIBILITY_REVISION_V1: &str = "ee.rule_index_eligibility.v1";
const EVIDENCE_INDEX_ADMISSION_REVISION_V1: &str = "ee.evidence_index_admission.v1";
const INDEX_STAGING_PREFIX: &str = ".publish-";
const INDEX_REJECTED_PREFIX: &str = ".rejected-";
const INDEX_RETAINED_SUFFIX: &str = ".previous";
const VECTOR_INDEX_FAST_FILE: &str = "vector.fast.idx";
const VECTOR_INDEX_QUALITY_FILE: &str = "vector.quality.idx";
const VECTOR_INDEX_FALLBACK_FILE: &str = "vector.idx";
#[cfg(feature = "lexical-bm25")]
const LEXICAL_INDEX_SUBDIR: &str = "lexical";

/// Maximum bytes inspected when reading `<workspace>/.ee/index/meta.json`.
/// Real index metadata is a single tiny JSON object (`sourceGeneration`,
/// `corpusRevision`, per-kind/per-tier counts, and optional embedder
/// fingerprint — well under 2 KiB in practice); 4 MiB gives many orders of
/// magnitude of headroom while still bounding
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
#[cfg(test)]
const READ_SURFACE_AUDIT_ACTIONS: [&str; 6] = [
    crate::db::audit_actions::SEARCH_EXECUTED,
    crate::db::audit_actions::SEARCH_RETURNED_MEM,
    crate::db::audit_actions::PACK_ASSEMBLED,
    crate::db::audit_actions::PACK_INCLUDED_MEM,
    crate::db::audit_actions::MEMORY_SHOW,
    crate::db::audit_actions::WHY_INSPECTED,
];

/// Lock TTL for index publish operations.
///
/// The bounded production request is five minutes; the lease keeps a second
/// five-minute cleanup margin so a cancellation at the budget edge cannot let
/// another publisher enter while the short masked commit tail is finishing.
const INDEX_PUBLISH_LOCK_TTL_SECS: u64 = 600;
const INDEX_PUBLISH_LOCK_RETRY_ATTEMPTS: usize = 200;
pub const INDEX_PUBLISH_LOCK_CONTENTION_CODE: &str = "index_publish_lock_contention";
static INDEX_METADATA_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
static INDEX_CORPUS_REVISION: OnceLock<CorpusRevision> = OnceLock::new();
#[cfg(test)]
type BeforeIndexPublishHook = Box<dyn FnOnce(&asupersync::Cx)>;
#[cfg(test)]
std::thread_local! {
    static BEFORE_INDEX_PUBLISH_HOOK: RefCell<Option<BeforeIndexPublishHook>> =
        const { RefCell::new(None) };
    static INDEX_PUBLISH_LOCK_RETRY_ATTEMPTS_OVERRIDE: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn install_before_index_publish_hook(hook: impl FnOnce(&asupersync::Cx) + 'static) {
    BEFORE_INDEX_PUBLISH_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_before_index_publish_hook(cx: &asupersync::Cx) {
    BEFORE_INDEX_PUBLISH_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook(cx);
        }
    });
}

#[cfg(not(test))]
fn run_before_index_publish_hook(_cx: &asupersync::Cx) {}

#[cfg(test)]
type AfterIndexPublishHook = Box<dyn FnOnce(&asupersync::Cx)>;
#[cfg(test)]
std::thread_local! {
    static AFTER_INDEX_PUBLISH_HOOK: RefCell<Option<AfterIndexPublishHook>> =
        const { RefCell::new(None) };
}

#[cfg(test)]
fn install_after_index_publish_hook(hook: impl FnOnce(&asupersync::Cx) + 'static) {
    AFTER_INDEX_PUBLISH_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

/// Fired immediately after the staged generation is durably published but
/// before job bookkeeping resumes — the seam for proving a post-publication
/// failure cannot fake job completion or corrupt the just-published index.
#[cfg(test)]
fn run_after_index_publish_hook(cx: &asupersync::Cx) {
    AFTER_INDEX_PUBLISH_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook(cx);
        }
    });
}

#[cfg(not(test))]
fn run_after_index_publish_hook(_cx: &asupersync::Cx) {}

/// Generate a unique holder ID for advisory locks.
fn generate_index_holder_id() -> String {
    let pid = std::process::id();
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("index:{pid}:{ts}")
}

/// Acquire the index publish lock or return an error.
fn acquire_index_publish_lock(
    cx: &asupersync::Cx,
    db: &DbConnection,
    workspace_id: &str,
    holder_id: &str,
) -> Result<(), IndexRebuildError> {
    acquire_index_publish_lock_with_retry(
        cx,
        db,
        workspace_id,
        holder_id,
        index_publish_lock_retry_attempts(),
        index_publish_lock_retry_delay,
    )
}

fn index_publish_lock_retry_attempts() -> usize {
    #[cfg(test)]
    if let Some(attempts) = INDEX_PUBLISH_LOCK_RETRY_ATTEMPTS_OVERRIDE.with(std::cell::Cell::get) {
        return attempts.max(1);
    }

    crate::config::env_registry::read(
        crate::config::env_registry::EnvVar::IndexPublishLockRetryAttempts,
    )
    .and_then(|raw| raw.parse::<usize>().ok())
    .filter(|attempts| *attempts > 0)
    .unwrap_or(INDEX_PUBLISH_LOCK_RETRY_ATTEMPTS)
}

fn acquire_index_publish_lock_with_retry<F>(
    cx: &asupersync::Cx,
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
        index_checkpoint(cx)?;
        let acquisition = match db.acquire_advisory_lock(
            &lock_id,
            holder_id,
            Some(INDEX_PUBLISH_LOCK_TTL_SECS),
            Some("index publish"),
        ) {
            Ok(acquisition) => acquisition,
            Err(error) => {
                if cx.checkpoint().is_err() {
                    return index_checkpoint(cx);
                }
                return Err(IndexRebuildError::Database(error));
            }
        };
        match acquisition {
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
                        let mut remaining = delay;
                        while !remaining.is_zero() {
                            let chunk = remaining.min(Duration::from_millis(5));
                            std::thread::sleep(chunk);
                            remaining = remaining.saturating_sub(chunk);
                            index_checkpoint(cx)?;
                        }
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
    match db.release_advisory_lock(&lock_id, holder_id) {
        Ok(true) => {}
        Ok(false) => tracing::error!(
            target: "ee::index",
            lock_id = %lock_id.canonical_key(),
            holder_id,
            "index publish lock release did not match an active owned lease"
        ),
        Err(error) => tracing::error!(
            target: "ee::index",
            lock_id = %lock_id.canonical_key(),
            holder_id,
            error = %error,
            "failed to release index publish lock"
        ),
    }
}

fn index_checkpoint(cx: &asupersync::Cx) -> Result<(), IndexRebuildError> {
    cx.checkpoint().map_err(|_| {
        IndexRebuildError::Cancelled(cx.cancel_reason().unwrap_or_else(|| {
            crate::core::outcome::attributed_cancel_reason(
                cx,
                asupersync::CancelKind::User,
                "index operation cancelled without a recorded reason",
            )
        }))
    })
}

fn map_index_search_error(
    cx: &asupersync::Cx,
    phase: &str,
    error: SearchError,
) -> IndexRebuildError {
    match error {
        SearchError::Cancelled {
            phase: backend_phase,
            reason: backend_reason,
        } => {
            let reason = cx.cancel_reason().unwrap_or_else(|| {
                let kind = crate::core::outcome::cancel_kind_from_backend_reason(&backend_reason);
                crate::core::outcome::attributed_cancel_reason(
                    cx,
                    kind,
                    format!(
                        "{phase}: Frankensearch cancelled during {backend_phase}: {backend_reason}"
                    ),
                )
            });
            IndexRebuildError::Cancelled(reason)
        }
        error => IndexRebuildError::Index(format!("{phase}: {error}")),
    }
}

/// Owns one advisory index-publish lease and releases it on every exit path.
struct IndexPublishLockOwner<'a> {
    cx: &'a asupersync::Cx,
    db: &'a DbConnection,
    workspace_id: &'a str,
    holder_id: String,
}

impl<'a> IndexPublishLockOwner<'a> {
    fn acquire(
        cx: &'a asupersync::Cx,
        db: &'a DbConnection,
        workspace_id: &'a str,
    ) -> Result<Self, IndexRebuildError> {
        index_checkpoint(cx)?;
        let holder_id = generate_index_holder_id();
        acquire_index_publish_lock(cx, db, workspace_id, &holder_id)?;
        let owner = Self {
            cx,
            db,
            workspace_id,
            holder_id,
        };
        index_checkpoint(cx)?;
        Ok(owner)
    }
}

impl Drop for IndexPublishLockOwner<'_> {
    fn drop(&mut self) {
        self.cx.masked(|| {
            let _ambient = asupersync::Cx::set_current(Some(self.cx.clone()));
            release_index_publish_lock(self.db, self.workspace_id, &self.holder_id);
        });
    }
}

/// Ensures a claimed job cannot remain `running` when control unwinds through
/// an error or cancellation checkpoint.
struct RunningIndexJobFinalizer<'a> {
    cx: &'a asupersync::Cx,
    db: &'a DbConnection,
    job_id: String,
    explicitly_cancelled: bool,
}

impl RunningIndexJobFinalizer<'_> {
    fn mark_cancelled(&mut self) {
        self.explicitly_cancelled = true;
    }
}

impl Drop for RunningIndexJobFinalizer<'_> {
    fn drop(&mut self) {
        let cancelled = self.explicitly_cancelled
            || self.cx.cancel_reason().is_some()
            || self.cx.checkpoint().is_err();
        self.cx.masked(|| {
            let _ambient = asupersync::Cx::set_current(Some(self.cx.clone()));
            let job = match self.db.get_search_index_job(&self.job_id) {
                Ok(Some(job)) => job,
                Ok(None) => {
                    tracing::error!(
                        target: "ee::index",
                        job_id = self.job_id,
                        "failed to finalize index job because its row disappeared"
                    );
                    return;
                }
                Err(error) => {
                    tracing::error!(
                        target: "ee::index",
                        job_id = self.job_id,
                        error = %error,
                        "failed to inspect index job during finalization"
                    );
                    return;
                }
            };
            if job.status_enum() != Some(SearchIndexJobStatus::Running) {
                return;
            }
            let transition = if cancelled {
                self.db.cancel_running_search_index_job(&self.job_id)
            } else {
                self.db.fail_search_index_job(
                    &self.job_id,
                    "index worker exited before recording a terminal job outcome",
                )
            };
            match transition {
                Ok(true) => {}
                Ok(false) => tracing::error!(
                    target: "ee::index",
                    job_id = self.job_id,
                    cancelled,
                    "index job finalization did not change a running row"
                ),
                Err(error) => tracing::error!(
                    target: "ee::index",
                    job_id = self.job_id,
                    cancelled,
                    error = %error,
                    "failed to finalize index job"
                ),
            }
        });
    }
}

fn require_index_job_transition(
    changed: bool,
    job_id: &str,
    transition: &str,
) -> Result<(), IndexRebuildError> {
    if changed {
        Ok(())
    } else {
        Err(IndexRebuildError::Index(format!(
            "search index job {job_id} rejected required transition {transition}"
        )))
    }
}

fn update_running_index_job_total(
    db: &DbConnection,
    job_id: &str,
    documents_total: u32,
) -> Result<(), IndexRebuildError> {
    let changed = db.update_search_index_job_total(job_id, documents_total)?;
    require_index_job_transition(changed, job_id, "running_total_updated")
}

fn commit_running_index_job_success(
    db: &DbConnection,
    job_id: &str,
    documents_total: u32,
) -> Result<(), IndexRebuildError> {
    db.with_transaction_error(|| {
        let progressed = db.update_search_index_job_progress(job_id, documents_total)?;
        require_index_job_transition(progressed, job_id, "running_progress_updated")?;
        let completed = db.complete_search_index_job(job_id, documents_total)?;
        require_index_job_transition(completed, job_id, "running_completed")
    })
}

fn append_failed_index_job_transition(db: &DbConnection, job_id: &str, error_message: &mut String) {
    match db.fail_search_index_job(job_id, error_message) {
        Ok(true) => {}
        Ok(false) => error_message
            .push_str("; failed to mark search index job failed: running row was not updated"),
        Err(error) => {
            error_message.push_str("; failed to mark search index job failed: ");
            error_message.push_str(&error.to_string());
        }
    }
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

/// Exact source-class membership for one complete index corpus.
///
/// The private total is computed with checked arithmetic at construction time,
/// so metadata writers and publication gates cannot accidentally stamp a total
/// that disagrees with the five source classes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IndexDocumentCounts {
    pub memories: u32,
    pub sessions: u32,
    pub artifacts: u32,
    pub rules: u32,
    pub evidence: u32,
    total: u32,
}

impl IndexDocumentCounts {
    fn checked(
        memories: u32,
        sessions: u32,
        artifacts: u32,
        rules: u32,
        evidence: u32,
    ) -> Result<Self, String> {
        let total = memories
            .checked_add(sessions)
            .and_then(|count| count.checked_add(artifacts))
            .and_then(|count| count.checked_add(rules))
            .and_then(|count| count.checked_add(evidence))
            .ok_or_else(|| "combined index document count exceeds u32".to_owned())?;
        Ok(Self {
            memories,
            sessions,
            artifacts,
            rules,
            evidence,
            total,
        })
    }

    #[must_use]
    pub const fn total(self) -> u32 {
        self.total
    }

    #[must_use]
    fn data_json(self) -> serde_json::Value {
        serde_json::json!({
            "memories": self.memories,
            "sessions": self.sessions,
            "artifacts": self.artifacts,
            "rules": self.rules,
            "evidence": self.evidence,
        })
    }

    fn from_metadata(value: &serde_json::Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "documentCounts must be a JSON object".to_owned())?;
        let read_count = |field: &str| -> Result<u32, String> {
            object
                .get(field)
                .and_then(serde_json::Value::as_u64)
                .and_then(|count| u32::try_from(count).ok())
                .ok_or_else(|| format!("documentCounts.{field} must be a u32"))
        };
        Self::checked(
            read_count("memories")?,
            read_count("sessions")?,
            read_count("artifacts")?,
            read_count("rules")?,
            read_count("evidence")?,
        )
    }

    fn memory_only(documents_total: u32) -> Self {
        Self {
            memories: documents_total,
            sessions: 0,
            artifacts: 0,
            rules: 0,
            evidence: 0,
            total: documents_total,
        }
    }
}

impl From<u32> for IndexDocumentCounts {
    fn from(documents_total: u32) -> Self {
        Self::memory_only(documents_total)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct IndexTierDocumentCounts {
    fast: u32,
    quality: Option<u32>,
    lexical: Option<u32>,
}

impl IndexTierDocumentCounts {
    fn from_metadata(value: &serde_json::Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "tierDocumentCounts must be a JSON object".to_owned())?;
        let read_required = |field: &str| -> Result<u32, String> {
            object
                .get(field)
                .and_then(serde_json::Value::as_u64)
                .and_then(|count| u32::try_from(count).ok())
                .ok_or_else(|| format!("tierDocumentCounts.{field} must be a u32"))
        };
        let read_optional = |field: &str| -> Result<Option<u32>, String> {
            match object.get(field) {
                Some(serde_json::Value::Null) | None => Ok(None),
                Some(value) => value
                    .as_u64()
                    .and_then(|count| u32::try_from(count).ok())
                    .map(Some)
                    .ok_or_else(|| format!("tierDocumentCounts.{field} must be a u32 or null")),
            }
        };
        Ok(Self {
            fast: read_required("fast")?,
            quality: read_optional("quality")?,
            lexical: read_optional("lexical")?,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EvidenceAdmissionTotals {
    pub admitted: u32,
    pub quarantined: u32,
    pub denied: u32,
}

impl EvidenceAdmissionTotals {
    #[must_use]
    pub fn from_report(report: &EvidenceAdmissionReport) -> Self {
        report
            .by_producer
            .values()
            .fold(Self::default(), |mut total, counts| {
                total.admitted = total.admitted.saturating_add(counts.admitted);
                total.quarantined = total.quarantined.saturating_add(counts.quarantined);
                total.denied = total.denied.saturating_add(counts.denied);
                total
            })
    }

    #[must_use]
    fn data_json(self) -> serde_json::Value {
        serde_json::json!({
            "admitted": self.admitted,
            "quarantined": self.quarantined,
            "denied": self.denied,
        })
    }

    #[must_use]
    pub fn total(self) -> u32 {
        self.admitted
            .saturating_add(self.quarantined)
            .saturating_add(self.denied)
    }
}

fn hash_index_corpus_component(hasher: &mut blake3::Hasher, name: &str, value: &str) {
    hasher.update(&(name.len() as u64).to_le_bytes());
    hasher.update(name.as_bytes());
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

/// Deterministic compatibility token for the complete workspace search corpus.
///
/// This deliberately excludes row counts, timestamps, generations, and
/// embedder configuration. It changes only when document shape, admitted
/// source classes, or source-specific projection/eligibility/redaction
/// semantics change.
pub fn expected_index_corpus_revision() -> &'static CorpusRevision {
    INDEX_CORPUS_REVISION.get_or_init(|| {
        let mut hasher = blake3::Hasher::new();
        hasher.update(INDEX_CORPUS_REVISION_DOMAIN);
        hash_index_corpus_component(
            &mut hasher,
            "canonical_document_schema",
            crate::search::CANONICAL_DOCUMENT_SCHEMA,
        );
        for source in ["memory", "session", "artifact", "rule", "import"] {
            hash_index_corpus_component(&mut hasher, "admitted_source", source);
        }
        for (source, projection, eligibility) in [
            (
                "memory",
                MEMORY_INDEX_PROJECTION_SCHEMA_V1,
                MEMORY_INDEX_ELIGIBILITY_REVISION_V2,
            ),
            (
                "session",
                SESSION_INDEX_PROJECTION_SCHEMA_V1,
                SESSION_INDEX_ELIGIBILITY_REVISION_V1,
            ),
            (
                "artifact",
                ARTIFACT_INDEX_PROJECTION_SCHEMA_V1,
                ARTIFACT_INDEX_ELIGIBILITY_REVISION_V1,
            ),
            (
                "rule",
                RULE_INDEX_PROJECTION_SCHEMA_V1,
                RULE_INDEX_ELIGIBILITY_REVISION_V1,
            ),
            (
                "import",
                EVIDENCE_INDEX_PROJECTION_SCHEMA_V1,
                EVIDENCE_INDEX_ADMISSION_REVISION_V1,
            ),
        ] {
            hash_index_corpus_component(&mut hasher, &format!("{source}_projection"), projection);
            hash_index_corpus_component(&mut hasher, &format!("{source}_eligibility"), eligibility);
        }
        hash_index_corpus_component(
            &mut hasher,
            "evidence_screening_version",
            &EVIDENCE_SCREENING_VERSION.to_string(),
        );
        hash_index_corpus_component(
            &mut hasher,
            "evidence_security_policy_epoch",
            &EVIDENCE_SECURITY_POLICY_EPOCH.to_string(),
        );
        hash_index_corpus_component(
            &mut hasher,
            "evidence_canonical_provenance_revision",
            &EVIDENCE_CANONICAL_PROVENANCE_REVISION.to_string(),
        );
        CorpusRevision::new(format!("blake3:{}", hasher.finalize().to_hex()))
    })
}

#[derive(Clone, Debug)]
pub struct IndexRebuildReport {
    pub status: IndexRebuildStatus,
    pub memories_indexed: u32,
    pub sessions_indexed: u32,
    pub artifacts_indexed: u32,
    pub rules_indexed: u32,
    pub evidence_indexed: u32,
    pub documents_total: u32,
    pub index_dir: PathBuf,
    pub elapsed_ms: f64,
    pub dry_run: bool,
    pub evidence_admission: EvidenceAdmissionReport,
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
    pub rules_indexed: u32,
    pub evidence_indexed: u32,
    pub documents_embedded: u32,
    pub documents_total: u32,
    pub index_dir: PathBuf,
    pub elapsed_ms: f64,
    pub dry_run: bool,
    pub idempotency_key: String,
    pub evidence_admission: EvidenceAdmissionReport,
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
        output.push_str(&format!("  Rules: {}\n", self.rules_indexed));
        output.push_str(&format!("  Evidence: {}\n", self.evidence_indexed));
        let evidence_totals = EvidenceAdmissionTotals::from_report(&self.evidence_admission);
        output.push_str(&format!(
            "  Evidence admitted/quarantined/denied: {}/{}/{}\n",
            evidence_totals.admitted, evidence_totals.quarantined, evidence_totals.denied
        ));
        output.push_str(&format!(
            "  Evidence admission: {}\n",
            serde_json::to_string(&self.evidence_admission).unwrap_or_else(|_| "{}".to_owned())
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
        let evidence_totals = EvidenceAdmissionTotals::from_report(&self.evidence_admission);
        serde_json::json!({
            "command": "index_rebuild",
            "status": self.status.as_str(),
            "memories_indexed": self.memories_indexed,
            "sessions_indexed": self.sessions_indexed,
            "artifacts_indexed": self.artifacts_indexed,
            "rules_indexed": self.rules_indexed,
            "evidence_indexed": self.evidence_indexed,
            "documents_total": self.documents_total,
            "index_dir": self.index_dir.to_string_lossy(),
            "elapsed_ms": self.elapsed_ms,
            "dry_run": self.dry_run,
            "evidenceAdmission": self.evidence_admission,
            "evidenceAdmissionTotals": evidence_totals.data_json(),
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
        output.push_str(&format!("  Rules: {}\n", self.rules_indexed));
        output.push_str(&format!("  Evidence: {}\n", self.evidence_indexed));
        let evidence_totals = EvidenceAdmissionTotals::from_report(&self.evidence_admission);
        output.push_str(&format!(
            "  Evidence admitted/quarantined/denied: {}/{}/{}\n",
            evidence_totals.admitted, evidence_totals.quarantined, evidence_totals.denied
        ));
        output.push_str(&format!(
            "  Evidence admission: {}\n",
            serde_json::to_string(&self.evidence_admission).unwrap_or_else(|_| "{}".to_owned())
        ));
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
        let evidence_totals = EvidenceAdmissionTotals::from_report(&self.evidence_admission);
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
            "rules_indexed": self.rules_indexed,
            "evidence_indexed": self.evidence_indexed,
            "documents_embedded": self.documents_embedded,
            "documents_total": self.documents_total,
            "index_dir": self.index_dir.to_string_lossy(),
            "elapsed_ms": self.elapsed_ms,
            "dry_run": self.dry_run,
            "idempotency_key": self.idempotency_key,
            "evidenceAdmission": self.evidence_admission,
            "evidenceAdmissionTotals": evidence_totals.data_json(),
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
    pub(crate) fn durable_mutation(&self) -> bool {
        index_processing_jobs_have_durable_mutation(&self.jobs)
    }

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
    Cancelled(asupersync::CancelReason),
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
            Self::Cancelled(_) => None,
            Self::NoWorkspace => Some("ee init --workspace ."),
        }
    }

    #[must_use]
    pub const fn stable_code(&self) -> Option<&'static str> {
        match self {
            Self::LockContention(_) => Some(INDEX_PUBLISH_LOCK_CONTENTION_CODE),
            Self::Database(_) | Self::Index(_) | Self::Cancelled(_) | Self::NoWorkspace => None,
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
            Self::Cancelled(reason) => f.write_str(&crate::core::outcome::cancel_message(reason)),
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
    crate::core::run_cli_with_cx(Duration::from_secs(300), |cx| async move {
        rebuild_index_with_cx(&cx, options).await
    })
    .map_err(|error| IndexRebuildError::Index(format!("Failed to start index runtime: {error}")))?
}

pub async fn rebuild_index_with_cx(
    cx: &asupersync::Cx,
    options: &IndexRebuildOptions,
) -> Result<IndexRebuildReport, IndexRebuildError> {
    index_checkpoint(cx)?;
    let start = Instant::now();
    let database_path = options.resolve_database_path();
    let index_dir = options.resolve_index_dir();
    let runtime_profile = runtime_profile_for_workspace(&options.workspace_path);

    let db = DbConnection::open_file(&database_path)?;
    let workspace_id = resolve_index_workspace_id(&db, &options.workspace_path)?;
    let _publish_lock = if options.dry_run {
        None
    } else {
        Some(IndexPublishLockOwner::acquire(cx, &db, &workspace_id)?)
    };
    let WorkspaceIndexSourceSnapshot {
        generation: source_generation,
        memories_indexed,
        sessions_indexed,
        artifacts_indexed,
        rules_indexed,
        evidence_indexed,
        document_counts,
        documents_total,
        documents: indexable_docs,
        evidence_admission,
        open_job_ids: _,
    } = collect_workspace_index_source_snapshot(&db, &workspace_id)?;
    index_checkpoint(cx)?;

    if options.dry_run {
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        return Ok(IndexRebuildReport {
            status: IndexRebuildStatus::DryRun,
            memories_indexed,
            sessions_indexed,
            artifacts_indexed,
            rules_indexed,
            evidence_indexed,
            documents_total,
            index_dir,
            elapsed_ms,
            dry_run: true,
            evidence_admission,
            errors: Vec::new(),
            runtime_profile,
        });
    }

    let _recovery_action = recover_interrupted_publish(&index_dir)?;
    let registry_stack = workspace_embedder_stack(&db, &workspace_id)?;
    ensure_active_embedding_registry_record(&db, &workspace_id, &registry_stack)?;
    let build_result = publish_full_index_generation_with_stack(
        cx,
        &index_dir,
        registry_stack,
        indexable_docs,
        source_generation,
        document_counts,
        || Ok(()),
    )
    .await;
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    match build_result {
        Ok(stats) => Ok(IndexRebuildReport {
            status: if documents_total == 0 {
                IndexRebuildStatus::NoDocuments
            } else {
                IndexRebuildStatus::Success
            },
            memories_indexed,
            sessions_indexed,
            artifacts_indexed,
            rules_indexed,
            evidence_indexed,
            documents_total,
            index_dir,
            elapsed_ms,
            dry_run: false,
            evidence_admission: evidence_admission.clone(),
            errors: stats
                .errors
                .iter()
                .map(|(id, e)| format!("{id}: {e}"))
                .collect(),
            runtime_profile: runtime_profile.clone(),
        }),
        Err(IndexRebuildError::Cancelled(reason)) => Err(IndexRebuildError::Cancelled(reason)),
        Err(error) => Ok(IndexRebuildReport {
            status: IndexRebuildStatus::IndexError,
            memories_indexed,
            sessions_indexed,
            artifacts_indexed,
            rules_indexed,
            evidence_indexed,
            documents_total,
            index_dir,
            elapsed_ms,
            dry_run: false,
            evidence_admission,
            errors: vec![error.to_string()],
            runtime_profile: runtime_profile.clone(),
        }),
    }
}

pub fn reembed_index(
    options: &IndexReembedOptions,
) -> Result<IndexReembedReport, IndexRebuildError> {
    crate::core::run_cli_with_cx(Duration::from_secs(300), |cx| async move {
        reembed_index_with_cx(&cx, options).await
    })
    .map_err(|error| IndexRebuildError::Index(format!("Failed to start index runtime: {error}")))?
}

pub async fn reembed_index_with_cx(
    cx: &asupersync::Cx,
    options: &IndexReembedOptions,
) -> Result<IndexReembedReport, IndexRebuildError> {
    let db = DbConnection::open_file(&options.resolve_database_path())?;
    let workspace_id = resolve_index_workspace_id(&db, &options.workspace_path)?;
    let stack = workspace_embedder_stack(&db, &workspace_id)?;
    reembed_index_with_cx_and_stack(cx, options, stack).await
}

async fn reembed_index_with_cx_and_stack(
    cx: &asupersync::Cx,
    options: &IndexReembedOptions,
    stack: EmbedderStack,
) -> Result<IndexReembedReport, IndexRebuildError> {
    index_checkpoint(cx)?;
    let start = Instant::now();
    let database_path = options.resolve_database_path();
    let index_dir = options.resolve_index_dir();
    let runtime_profile = runtime_profile_for_workspace(&options.workspace_path);

    let db = DbConnection::open_file(&database_path)?;
    let workspace_id = resolve_index_workspace_id(&db, &options.workspace_path)?;
    let _publish_lock = if options.dry_run {
        None
    } else {
        Some(IndexPublishLockOwner::acquire(cx, &db, &workspace_id)?)
    };
    let WorkspaceIndexSourceSnapshot {
        generation: source_generation,
        memories_indexed,
        sessions_indexed,
        artifacts_indexed,
        rules_indexed,
        evidence_indexed,
        document_counts,
        documents_total,
        documents: indexable_docs,
        evidence_admission,
        open_job_ids: _,
    } = collect_workspace_index_source_snapshot(&db, &workspace_id)?;
    index_checkpoint(cx)?;
    let current_vector_coverage =
        embedding_vector_coverage(&index_dir, documents_total, read_fast_vector_record_count);
    let embedding = reembed_embedding_summary(&db, &workspace_id, &stack, current_vector_coverage)?;
    let idempotency_key = reembed_idempotency_key(
        &workspace_id,
        &embedding.fast_model_id,
        embedding.quality_model_id.as_deref(),
        document_counts,
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
            rules_indexed,
            evidence_indexed,
            documents_embedded,
            documents_total,
            index_dir,
            elapsed_ms,
            dry_run: true,
            idempotency_key,
            evidence_admission,
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
    match db.start_search_index_job(&job_id) {
        Ok(true) => {}
        Ok(false) => {
            let message = format!("Failed to start re-embedding job {job_id}");
            match db.fail_search_index_job(&job_id, &message) {
                Ok(true) => {}
                Ok(false) => tracing::warn!(
                    target: "ee::index::reembed",
                    job_id,
                    "unstartable re-embedding job was not running, so the failure transition was not applied"
                ),
                Err(fail_error) => tracing::error!(
                    target: "ee::index::reembed",
                    job_id,
                    error = %fail_error,
                    "failed to mark unstartable re-embedding job failed"
                ),
            }
            return Err(IndexRebuildError::Index(message));
        }
        Err(error) => {
            let message = error.to_string();
            match db.fail_search_index_job(&job_id, &message) {
                Ok(true) => {}
                Ok(false) => tracing::warn!(
                    target: "ee::index::reembed",
                    job_id,
                    "re-embedding job start failed before a running row was available to fail"
                ),
                Err(fail_error) => tracing::error!(
                    target: "ee::index::reembed",
                    job_id,
                    error = %fail_error,
                    "failed to mark re-embedding job failed after start error"
                ),
            }
            return Err(IndexRebuildError::Database(error));
        }
    }
    let mut job_finalizer = RunningIndexJobFinalizer {
        cx,
        db: &db,
        job_id: job_id.clone(),
        explicitly_cancelled: false,
    };

    let _recovery_action = recover_interrupted_publish(&index_dir)?;
    ensure_active_embedding_registry_record(&db, &workspace_id, &stack)?;
    let embedding = reembed_embedding_summary(&db, &workspace_id, &stack, current_vector_coverage)?;
    let build_result = publish_full_index_generation_with_stack(
        cx,
        &index_dir,
        stack,
        indexable_docs,
        source_generation,
        document_counts,
        || commit_running_index_job_success(&db, &job_id, documents_total),
    )
    .await;
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    match build_result {
        Ok(stats) => {
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
                status: if documents_total == 0 {
                    IndexReembedStatus::NoDocuments
                } else {
                    IndexReembedStatus::Success
                },
                job_id: Some(job_id.clone()),
                job_status: "completed".to_owned(),
                job_type: SearchIndexJobType::FullRebuild.as_str().to_owned(),
                document_source: None,
                embedding_scope: "all_documents".to_owned(),
                embedding: published_embedding,
                memories_indexed,
                sessions_indexed,
                artifacts_indexed,
                rules_indexed,
                evidence_indexed,
                documents_embedded,
                documents_total,
                index_dir,
                elapsed_ms,
                dry_run: false,
                idempotency_key,
                evidence_admission: evidence_admission.clone(),
                errors: stats
                    .errors
                    .iter()
                    .map(|(id, e)| format!("{id}: {e}"))
                    .collect(),
                runtime_profile: runtime_profile.clone(),
            })
        }
        Err(IndexRebuildError::Cancelled(reason)) => {
            job_finalizer.mark_cancelled();
            match db.cancel_running_search_index_job(&job_id) {
                Ok(true) => {}
                Ok(false) => tracing::error!(
                    target: "ee::index::reembed",
                    job_id,
                    "failed to mark cancelled re-embedding job cancelled because its running row was not updated"
                ),
                Err(cancel_error) => tracing::error!(
                    target: "ee::index::reembed",
                    job_id,
                    error = %cancel_error,
                    "failed to mark cancelled re-embedding job cancelled"
                ),
            }
            Err(IndexRebuildError::Cancelled(reason))
        }
        Err(error) => {
            let primary_error = error.to_string();
            let mut errors = vec![primary_error.clone()];
            match db.fail_search_index_job(&job_id, &primary_error) {
                Ok(true) => {}
                Ok(false) => errors.push(
                    "failed to mark re-embedding job failed: running row was not updated"
                        .to_owned(),
                ),
                Err(fail_error) => errors.push(format!(
                    "failed to mark re-embedding job failed: {fail_error}"
                )),
            }
            let documents_embedded = embedding.documents_embedded();

            Ok(IndexReembedReport {
                status: IndexReembedStatus::IndexError,
                job_id: Some(job_id.clone()),
                job_status: "failed".to_owned(),
                job_type: SearchIndexJobType::FullRebuild.as_str().to_owned(),
                document_source: None,
                embedding_scope: "all_documents".to_owned(),
                embedding,
                memories_indexed,
                sessions_indexed,
                artifacts_indexed,
                rules_indexed,
                evidence_indexed,
                documents_embedded,
                documents_total,
                index_dir,
                elapsed_ms,
                dry_run: false,
                idempotency_key,
                evidence_admission,
                errors,
                runtime_profile: runtime_profile.clone(),
            })
        }
    }
}

pub fn process_index_jobs(
    options: &IndexProcessingOptions,
) -> Result<IndexProcessingReport, IndexRebuildError> {
    crate::core::run_cli_with_cx(Duration::from_secs(300), |cx| async move {
        process_index_jobs_with_cx(&cx, options).await
    })
    .map_err(|error| IndexRebuildError::Index(format!("Failed to start index runtime: {error}")))?
}

pub async fn process_index_jobs_with_cx(
    cx: &asupersync::Cx,
    options: &IndexProcessingOptions,
) -> Result<IndexProcessingReport, IndexRebuildError> {
    process_index_jobs_with_drain(cx, options, IndexJobDrain::Ordinary).await
}

/// Process the bounded pending-job set with one source snapshot and one publish.
///
/// Unlike [`process_index_jobs`], this is the production report surface for the
/// steward's `index_coalesce` job. Dry-run reports remain selection-only and
/// retain ordinary per-job planning labels; they do not claim that a coalesced
/// snapshot was built, and they neither claim nor publish any job.
pub(crate) fn process_index_jobs_coalesced(
    options: &IndexProcessingOptions,
) -> Result<IndexProcessingReport, IndexRebuildError> {
    crate::core::run_cli_with_cx(Duration::from_secs(300), |cx| async move {
        process_index_jobs_with_drain(
            &cx,
            options,
            IndexJobDrain::Coalesced {
                max_corpus_documents: None,
            },
        )
        .await
    })
    .map_err(|error| IndexRebuildError::Index(format!("Failed to start index runtime: {error}")))?
}

/// Bounded coalesced processor used by interactive read repair.
///
/// The document ceiling is checked again after the publish lock is held and
/// the authoritative source snapshot has been collected. That closes the
/// race where a writer enlarges the corpus after the read-side preflight but
/// before the coalesced rebuild snapshots its actual inputs.
pub(crate) async fn process_index_jobs_coalesced_with_cx_bounded(
    cx: &asupersync::Cx,
    options: &IndexProcessingOptions,
    max_corpus_documents: u32,
) -> Result<IndexProcessingReport, IndexRebuildError> {
    process_index_jobs_with_drain(
        cx,
        options,
        IndexJobDrain::Coalesced {
            max_corpus_documents: Some(max_corpus_documents),
        },
    )
    .await
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IndexJobDrain {
    Ordinary,
    Coalesced { max_corpus_documents: Option<u32> },
}

async fn process_index_jobs_with_drain(
    cx: &asupersync::Cx,
    options: &IndexProcessingOptions,
    drain: IndexJobDrain,
) -> Result<IndexProcessingReport, IndexRebuildError> {
    index_checkpoint(cx)?;
    let start = Instant::now();
    let database_path = options.resolve_database_path();
    let index_dir = options.resolve_index_dir();
    let runtime_profile = runtime_profile_for_workspace(&options.workspace_path);
    let (effective_job_limit, _job_limit_capped) =
        runtime_profile.cap_index_job_limit(options.job_limit);

    let db = DbConnection::open_file(&database_path)?;
    let workspace_id = resolve_index_workspace_id(&db, &options.workspace_path)?;
    if !options.dry_run {
        requeue_cancelled_search_index_jobs(&db, &workspace_id)?;
    }
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

    let jobs = match drain {
        IndexJobDrain::Ordinary => {
            let mut reports = Vec::with_capacity(pending_jobs.len());
            for job in pending_jobs {
                reports.push(process_one_index_job_with_cx(cx, &db, &job, &index_dir).await?);
            }
            reports
        }
        IndexJobDrain::Coalesced {
            max_corpus_documents,
        } => {
            process_selected_index_jobs_coalesced_with_cx(
                cx,
                &db,
                &workspace_id,
                &index_dir,
                pending_jobs,
                max_corpus_documents,
            )
            .await?
        }
    };
    let (processed_jobs, completed_jobs, failed_jobs, status) =
        summarize_index_processing_jobs(pending_count, &jobs);
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

fn summarize_index_processing_jobs(
    pending_jobs: u32,
    jobs: &[IndexProcessingJobReport],
) -> (u32, u32, u32, IndexProcessingStatus) {
    let reported_jobs = u32::try_from(jobs.len()).unwrap_or(u32::MAX);
    let completed_jobs = jobs
        .iter()
        .filter(|result| result.outcome == "completed")
        .count()
        .try_into()
        .unwrap_or(u32::MAX);
    let non_completed_jobs = jobs
        .iter()
        .filter(|result| result.outcome != "completed")
        .count()
        .try_into()
        .unwrap_or(u32::MAX);
    let unreported_jobs = pending_jobs.saturating_sub(reported_jobs);
    let failed_jobs = non_completed_jobs.saturating_add(unreported_jobs);
    let processed_jobs = completed_jobs.saturating_add(failed_jobs);
    let status = match (pending_jobs, completed_jobs, failed_jobs) {
        (0, 0, 0) => IndexProcessingStatus::NoPendingJobs,
        (_, _, 0) => IndexProcessingStatus::Success,
        (_, 0, _) => IndexProcessingStatus::Failed,
        _ => IndexProcessingStatus::PartialFailure,
    };
    (processed_jobs, completed_jobs, failed_jobs, status)
}

fn index_processing_jobs_have_durable_mutation(jobs: &[IndexProcessingJobReport]) -> bool {
    jobs.iter()
        .any(|job| matches!(job.outcome.as_str(), "completed" | "failed"))
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
    crate::core::run_cli_with_cx(Duration::from_secs(300), |cx| async move {
        process_pending_index_jobs_coalesced_with_cx(&cx, db, workspace_id, index_dir, job_limit)
            .await
    })
    .map_err(|error| IndexRebuildError::Index(format!("Failed to start index runtime: {error}")))?
}

#[cfg(test)]
pub(crate) fn process_pending_index_jobs_coalesced_after_snapshot<F>(
    db: &DbConnection,
    workspace_id: &str,
    index_dir: &Path,
    job_limit: Option<u32>,
    after_snapshot: F,
) -> Result<Vec<IndexProcessingJobReport>, IndexRebuildError>
where
    F: FnOnce() -> Result<(), IndexRebuildError>,
{
    crate::core::run_cli_with_cx(Duration::from_secs(300), |cx| async move {
        process_pending_index_jobs_coalesced_after_snapshot_with_cx(
            &cx,
            db,
            workspace_id,
            index_dir,
            job_limit,
            after_snapshot,
        )
        .await
    })
    .map_err(|error| IndexRebuildError::Index(format!("Failed to start index runtime: {error}")))?
}

#[cfg(test)]
fn process_selected_index_jobs_coalesced(
    db: &DbConnection,
    workspace_id: &str,
    index_dir: &Path,
    selected: Vec<StoredSearchIndexJob>,
) -> Result<Vec<IndexProcessingJobReport>, IndexRebuildError> {
    crate::core::run_cli_with_cx(Duration::from_secs(300), |cx| async move {
        process_selected_index_jobs_coalesced_with_cx(
            &cx,
            db,
            workspace_id,
            index_dir,
            selected,
            None,
        )
        .await
    })
    .map_err(|error| IndexRebuildError::Index(format!("Failed to start index runtime: {error}")))?
}

#[cfg(test)]
fn process_selected_index_jobs_coalesced_bounded(
    db: &DbConnection,
    workspace_id: &str,
    index_dir: &Path,
    selected: Vec<StoredSearchIndexJob>,
    max_corpus_documents: u32,
) -> Result<Vec<IndexProcessingJobReport>, IndexRebuildError> {
    crate::core::run_cli_with_cx(Duration::from_secs(300), |cx| async move {
        process_selected_index_jobs_coalesced_with_cx(
            &cx,
            db,
            workspace_id,
            index_dir,
            selected,
            Some(max_corpus_documents),
        )
        .await
    })
    .map_err(|error| IndexRebuildError::Index(format!("Failed to start index runtime: {error}")))?
}

/// Public retry path for interrupted index jobs: every workflow-emitted
/// processing tick first transitions cancelled/failed jobs and orphaned
/// `running` jobs in the workspace atomically back to `pending` as the SAME
/// logical job (no clone rows or id churn). A live or unprobeable publisher
/// retains ownership of its `running` rows.
fn requeue_cancelled_search_index_jobs(
    db: &DbConnection,
    workspace_id: &str,
) -> Result<u32, IndexRebuildError> {
    let requeued = db.requeue_cancelled_search_index_jobs(workspace_id)?;
    if requeued > 0 {
        tracing::info!(
            target: "ee::index",
            workspace_id,
            requeued,
            "requeued interrupted search index jobs back to pending"
        );
    }
    Ok(requeued)
}

pub(crate) async fn process_pending_index_jobs_coalesced_with_cx(
    cx: &asupersync::Cx,
    db: &DbConnection,
    workspace_id: &str,
    index_dir: &Path,
    job_limit: Option<u32>,
) -> Result<Vec<IndexProcessingJobReport>, IndexRebuildError> {
    requeue_cancelled_search_index_jobs(db, workspace_id)?;
    process_pending_index_jobs_coalesced_after_snapshot_with_cx(
        cx,
        db,
        workspace_id,
        index_dir,
        job_limit,
        || Ok(()),
    )
    .await
}

async fn process_selected_index_jobs_coalesced_with_cx(
    cx: &asupersync::Cx,
    db: &DbConnection,
    workspace_id: &str,
    index_dir: &Path,
    selected: Vec<StoredSearchIndexJob>,
    max_corpus_documents: Option<u32>,
) -> Result<Vec<IndexProcessingJobReport>, IndexRebuildError> {
    process_selected_index_jobs_coalesced_after_snapshot_with_cx(
        cx,
        db,
        workspace_id,
        index_dir,
        selected,
        max_corpus_documents,
        || Ok(()),
    )
    .await
}

async fn process_pending_index_jobs_coalesced_after_snapshot_with_cx<F>(
    cx: &asupersync::Cx,
    db: &DbConnection,
    workspace_id: &str,
    index_dir: &Path,
    job_limit: Option<u32>,
    after_snapshot: F,
) -> Result<Vec<IndexProcessingJobReport>, IndexRebuildError>
where
    F: FnOnce() -> Result<(), IndexRebuildError>,
{
    index_checkpoint(cx)?;
    let selected = db.list_pending_search_index_jobs(workspace_id, job_limit)?;
    process_selected_index_jobs_coalesced_after_snapshot_with_cx(
        cx,
        db,
        workspace_id,
        index_dir,
        selected,
        None,
        after_snapshot,
    )
    .await
}

async fn process_selected_index_jobs_coalesced_after_snapshot_with_cx<F>(
    cx: &asupersync::Cx,
    db: &DbConnection,
    workspace_id: &str,
    index_dir: &Path,
    selected: Vec<StoredSearchIndexJob>,
    max_corpus_documents: Option<u32>,
    after_snapshot: F,
) -> Result<Vec<IndexProcessingJobReport>, IndexRebuildError>
where
    F: FnOnce() -> Result<(), IndexRebuildError>,
{
    index_checkpoint(cx)?;
    const COALESCED_MODE: &str = "coalesced_full_rebuild";
    let mut claimed = Vec::new();
    let mut job_finalizers = Vec::new();
    let mut reports = Vec::new();
    if selected.is_empty() {
        return Ok(reports);
    }

    // Acquire the publication lease before changing any selected job from
    // `pending` to `running`. Publication-lock contention is a transient
    // scheduling condition, not a terminal job failure: claiming first made
    // `RunningIndexJobFinalizer` mark every selected row failed when another
    // process legitimately held the publish lock, leaving no pending work for
    // the next remember-side drain election to recover.
    let _publish_lock = IndexPublishLockOwner::acquire(cx, db, workspace_id)?;
    for job in selected {
        if db.start_search_index_job(&job.id)? {
            job_finalizers.push(RunningIndexJobFinalizer {
                cx,
                db,
                job_id: job.id.clone(),
                explicitly_cancelled: false,
            });
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

    let WorkspaceIndexSourceSnapshot {
        generation: published_generation,
        document_counts,
        documents_total,
        documents: indexable_docs,
        open_job_ids,
        ..
    } = collect_workspace_index_source_snapshot(db, workspace_id)?;
    if max_corpus_documents.is_some_and(|limit| documents_total > limit) {
        let limit = max_corpus_documents.unwrap_or(u32::MAX);
        for finalizer in &mut job_finalizers {
            finalizer.mark_cancelled();
        }
        drop(job_finalizers);
        requeue_cancelled_search_index_jobs(db, workspace_id)?;
        let message = format!(
            "coalesced index repair deferred because the authoritative corpus contains \
             {documents_total} documents, above the interactive limit of {limit}"
        );
        for job in &claimed {
            reports.push(IndexProcessingJobReport {
                job_id: job.id.clone(),
                job_type: job.job_type.clone(),
                document_source: job.document_source.clone(),
                document_id: job.document_id.clone(),
                outcome: "skipped".to_owned(),
                processing_mode: COALESCED_MODE.to_owned(),
                fallback_to_full: None,
                documents_total,
                documents_indexed: 0,
                error: Some(message.clone()),
            });
        }
        return Ok(reports);
    }
    if let Err(error) = after_snapshot() {
        if matches!(&error, IndexRebuildError::Cancelled(_)) {
            for finalizer in &mut job_finalizers {
                finalizer.mark_cancelled();
            }
        }
        return Err(error);
    }
    index_checkpoint(cx)?;
    for job in &claimed {
        update_running_index_job_total(db, &job.id, documents_total)?;
    }

    // Cancellation-aware intake publishes only a complete staged generation.
    // We still probe the former incremental eligibility contract so reports
    // preserve an actionable fallback reason, but never mutate the active fast,
    // quality, or lexical tiers in place.
    let claimed_ids = claimed
        .iter()
        .map(|job| job.id.as_str())
        .collect::<BTreeSet<_>>();
    let uncovered_open_job = open_job_ids
        .iter()
        .any(|job_id| !claimed_ids.contains(job_id.as_str()));
    let incremental_batch = if uncovered_open_job {
        None
    } else {
        coalesced_incremental_batch(&claimed, &indexable_docs)
    };
    let mut processing_mode = COALESCED_MODE.to_owned();
    if uncovered_open_job {
        processing_mode.push_str("_open_sibling_full_rebuild");
    }
    let fallback_to_full = incremental_batch.as_ref().and_then(|documents| {
        let max_generation_lag = u64::try_from(documents.len()).unwrap_or(u64::MAX).max(1);
        validate_incremental_index_metadata(index_dir, published_generation, max_generation_lag)
            .err()
            .map(|fallback| {
                let reason = fallback.reason;
                let detail = fallback.detail;
                tracing::info!(
                    target: "ee::index",
                    workspace_id = %workspace_id,
                    claimed_jobs = claimed.len(),
                    fallback_to_full = reason.as_str(),
                    detail = %detail,
                    "coalesced incremental index intake fell back to full rebuild"
                );
                processing_mode.push_str("_fallback_to_full");
                reason.as_str().to_owned()
            })
    });
    if incremental_batch.is_some() && fallback_to_full.is_none() {
        processing_mode.push_str("_staged_full_rebuild");
    }

    let _recovery_action = recover_interrupted_publish(index_dir)?;
    let stack = workspace_embedder_stack(db, workspace_id)?;
    let build_result = publish_full_index_generation_with_stack(
        cx,
        index_dir,
        stack,
        indexable_docs,
        published_generation,
        document_counts,
        || {
            db.with_transaction_error(|| {
                for job in &claimed {
                    let progressed =
                        db.update_search_index_job_progress(&job.id, documents_total)?;
                    require_index_job_transition(progressed, &job.id, "running_progress_updated")?;
                    let completed = db.complete_search_index_job(&job.id, documents_total)?;
                    require_index_job_transition(completed, &job.id, "running_completed")?;
                }
                Ok(())
            })
        },
    )
    .await;

    match build_result {
        Ok(_) => {
            for job in &claimed {
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
        Err(IndexRebuildError::Cancelled(reason)) => {
            for finalizer in &mut job_finalizers {
                finalizer.mark_cancelled();
            }
            return Err(IndexRebuildError::Cancelled(reason));
        }
        Err(error) => {
            let primary_error = error.to_string();
            for job in &claimed {
                let mut error_message = primary_error.clone();
                append_failed_index_job_transition(db, &job.id, &mut error_message);
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
    crate::core::run_cli_with_cx(Duration::from_secs(300), |cx| async move {
        process_one_index_job_with_cx(&cx, db, job, index_dir).await
    })
    .map_err(|error| IndexRebuildError::Index(format!("Failed to start index runtime: {error}")))?
}

#[cfg(test)]
fn process_one_index_job_after_snapshot<F>(
    db: &DbConnection,
    job: &StoredSearchIndexJob,
    index_dir: &Path,
    after_snapshot: F,
) -> Result<IndexProcessingJobReport, IndexRebuildError>
where
    F: FnOnce() -> Result<(), IndexRebuildError>,
{
    crate::core::run_cli_with_cx(Duration::from_secs(300), |cx| async move {
        process_one_index_job_after_snapshot_with_cx(&cx, db, job, index_dir, after_snapshot).await
    })
    .map_err(|error| IndexRebuildError::Index(format!("Failed to start index runtime: {error}")))?
}

async fn process_one_index_job_with_cx(
    cx: &asupersync::Cx,
    db: &DbConnection,
    job: &StoredSearchIndexJob,
    index_dir: &Path,
) -> Result<IndexProcessingJobReport, IndexRebuildError> {
    process_one_index_job_after_snapshot_with_cx(cx, db, job, index_dir, || Ok(())).await
}

async fn process_one_index_job_after_snapshot_with_cx<F>(
    cx: &asupersync::Cx,
    db: &DbConnection,
    job: &StoredSearchIndexJob,
    index_dir: &Path,
    after_snapshot: F,
) -> Result<IndexProcessingJobReport, IndexRebuildError>
where
    F: FnOnce() -> Result<(), IndexRebuildError>,
{
    index_checkpoint(cx)?;
    let mut processing_mode = processing_mode_for_job(job).to_owned();

    // Keep a job retryable until this worker owns the publication lease. A
    // lock wait that is cancelled or exhausts its bounded retry budget must
    // leave the row `pending`; only failures after admission to the publish
    // critical section are terminalized by `RunningIndexJobFinalizer`.
    let _publish_lock = IndexPublishLockOwner::acquire(cx, db, &job.workspace_id)?;
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
    let mut job_finalizer = RunningIndexJobFinalizer {
        cx,
        db,
        job_id: job.id.clone(),
        explicitly_cancelled: false,
    };

    let WorkspaceIndexSourceSnapshot {
        generation: published_generation,
        document_counts,
        documents_total,
        documents: indexable_docs,
        open_job_ids,
        ..
    } = collect_workspace_index_source_snapshot(db, &job.workspace_id)?;
    if let Err(error) = after_snapshot() {
        if matches!(&error, IndexRebuildError::Cancelled(_)) {
            job_finalizer.mark_cancelled();
        }
        return Err(error);
    }
    index_checkpoint(cx)?;
    update_running_index_job_total(db, &job.id, documents_total)?;

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
    // jobs are still open for this workspace we rebuild the COMPLETE
    // indexable set instead, so the published generation truthfully reflects
    // every committed document. The coalesced path uses the same snapshot and
    // applies incrementally only when every open job belongs to its claim set.
    let sibling_index_jobs_pending = open_job_ids.iter().any(|job_id| job_id != &job.id);
    let job_is_single_document = matches!(
        job.job_type_enum(),
        Some(SearchIndexJobType::Incremental | SearchIndexJobType::SingleDocument)
    );
    if job_is_single_document && sibling_index_jobs_pending {
        processing_mode.push_str("_sibling_pending_full_rebuild");
    }
    if job_is_single_document && !sibling_index_jobs_pending {
        processing_mode.push_str("_staged_full_rebuild");
    }

    // Publish the index at the database generation (the audit-inclusive
    // max of source-document and audited-mutation counts), matching the
    // full-rebuild path at write_index_metadata above. Writing only
    // `documents_total` here left the incremental index one generation
    // behind db_generation after `ee remember` wrote its audit rows, which
    // falsely tripped `search_index_stale` on the very next search even
    // though the job had already applied synchronously. (agent-UX item 5)
    let result = async {
        let _recovery_action = recover_interrupted_publish(index_dir)?;
        let fallback_to_full = None;
        let stack = workspace_embedder_stack(db, &job.workspace_id)?;
        let build_result = publish_full_index_generation_with_stack(
            cx,
            index_dir,
            stack,
            indexable_docs,
            published_generation,
            document_counts,
            || commit_running_index_job_success(db, &job.id, documents_total),
        )
        .await;

        match build_result {
            Ok(stats) => {
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
            Err(IndexRebuildError::Cancelled(reason)) => {
                job_finalizer.mark_cancelled();
                Err(IndexRebuildError::Cancelled(reason))
            }
            Err(error) => {
                let mut error_message = error.to_string();
                append_failed_index_job_transition(db, &job.id, &mut error_message);
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
    }
    .await;

    result
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IncrementalFallbackReason {
    IndexAbsent,
    GenerationSkew,
    CorpusRevisionMismatch,
    TierUnavailable,
    #[cfg(test)]
    ForcedReindex,
    #[cfg(test)]
    DeltaOverThreshold,
}

impl IncrementalFallbackReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::IndexAbsent => "index_absent",
            Self::GenerationSkew => "generation_skew",
            Self::CorpusRevisionMismatch => INDEX_INTAKE_FALLBACK_CORPUS_REVISION_MISMATCH,
            Self::TierUnavailable => "tier_unavailable",
            #[cfg(test)]
            Self::ForcedReindex => "forced_reindex",
            #[cfg(test)]
            Self::DeltaOverThreshold => "delta_over_threshold",
        }
    }
}

#[derive(Debug)]
#[cfg(test)]
enum IncrementalApplyOutcome {
    Applied {
        documents_indexed: u32,
    },
    Fallback {
        reason: IncrementalFallbackReason,
        detail: String,
    },
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

fn incremental_fallback(
    reason: IncrementalFallbackReason,
    detail: impl Into<String>,
) -> IncrementalFallback {
    IncrementalFallback {
        reason,
        detail: detail.into(),
    }
}

async fn publish_full_index_generation_with_stack<F>(
    cx: &asupersync::Cx,
    index_dir: &Path,
    stack: EmbedderStack,
    indexable_docs: Vec<crate::search::IndexableDocument>,
    generation: u64,
    document_counts: IndexDocumentCounts,
    commit_tail: F,
) -> Result<BuildStats, IndexRebuildError>
where
    F: FnOnce() -> Result<(), IndexRebuildError>,
{
    index_checkpoint(cx)?;
    let staging_dir = create_publish_staging_dir(index_dir)?;
    let embedder_fingerprint = embedder_fingerprint_for_index_metadata(&stack);
    let stats = build_index_generation(cx, &staging_dir, stack, indexable_docs).await?;
    let stats = validate_built_generation(&staging_dir, stats, document_counts)
        .map_err(IndexRebuildError::Index)?;
    run_before_index_publish_hook(cx);
    index_checkpoint(cx)?;
    cx.masked(|| {
        write_index_metadata(
            &staging_dir,
            generation,
            document_counts,
            embedder_fingerprint.as_ref(),
        )?;
        publish_staged_index_with_commit(index_dir, &staging_dir, commit_tail)
    })?;
    run_after_index_publish_hook(cx);
    Ok(stats)
}

#[cfg(test)]
fn apply_incremental_index_change_sync(
    index_dir: &Path,
    stack: EmbedderStack,
    document_id: &str,
    document: Option<crate::search::IndexableDocument>,
    generation: u64,
    document_counts: IndexDocumentCounts,
) -> IncrementalApplyOutcome {
    let index_dir_owned = index_dir.to_path_buf();
    let document_id_owned = document_id.to_owned();
    let result_holder: Arc<Mutex<Option<IncrementalApplyOutcome>>> = Arc::new(Mutex::new(None));
    let task_result = Arc::clone(&result_holder);
    let runtime_error_result = Arc::clone(&result_holder);

    let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let runtime_result = crate::core::run_cli_future(async move {
            // Invariant: run_cli_future's block_on installs an ambient runtime Cx.
            #[allow(clippy::expect_used)]
            let cx = asupersync::Cx::current()
                .expect("run_cli_future's block_on installs an ambient runtime Cx");
            let outcome = match apply_incremental_index_change(
                &cx,
                &index_dir_owned,
                stack,
                &document_id_owned,
                document,
                generation,
                document_counts,
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
#[cfg(test)]
fn apply_incremental_index_batch_sync(
    index_dir: &Path,
    stack: EmbedderStack,
    documents: Vec<crate::search::IndexableDocument>,
    generation: u64,
    document_counts: IndexDocumentCounts,
) -> IncrementalApplyOutcome {
    let index_dir_owned = index_dir.to_path_buf();
    let result_holder: Arc<Mutex<Option<IncrementalApplyOutcome>>> = Arc::new(Mutex::new(None));
    let task_result = Arc::clone(&result_holder);
    let runtime_error_result = Arc::clone(&result_holder);

    let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let runtime_result = crate::core::run_cli_future(async move {
            // Invariant: run_cli_future's block_on installs an ambient runtime Cx.
            #[allow(clippy::expect_used)]
            let cx = asupersync::Cx::current()
                .expect("run_cli_future's block_on installs an ambient runtime Cx");
            let outcome = match apply_incremental_index_batch(
                &cx,
                &index_dir_owned,
                stack,
                &documents,
                generation,
                document_counts,
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
#[cfg(test)]
async fn apply_incremental_index_batch(
    cx: &asupersync::Cx,
    index_dir: &Path,
    stack: EmbedderStack,
    documents: &[crate::search::IndexableDocument],
    generation: u64,
    document_counts: IndexDocumentCounts,
) -> Result<u32, IncrementalFallback> {
    let max_generation_lag = u64::try_from(documents.len()).unwrap_or(u64::MAX).max(1);
    validate_incremental_index_metadata(index_dir, generation, max_generation_lag)?;
    let embedder_fingerprint = embedder_fingerprint_for_index_metadata(&stack);
    for document in documents {
        upsert_incremental_document(cx, index_dir, stack.clone(), document).await?;
    }
    verify_published_tier_counts(
        index_dir,
        document_counts.total(),
        stack.quality().is_some(),
    )
    .map_err(|error| incremental_fallback(IncrementalFallbackReason::TierUnavailable, error))?;
    write_index_metadata(
        index_dir,
        generation,
        document_counts,
        embedder_fingerprint.as_ref(),
    )
    .map_err(|error| {
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

#[cfg(test)]
async fn apply_incremental_index_change(
    cx: &asupersync::Cx,
    index_dir: &Path,
    stack: EmbedderStack,
    document_id: &str,
    document: Option<crate::search::IndexableDocument>,
    generation: u64,
    document_counts: IndexDocumentCounts,
) -> Result<u32, IncrementalFallback> {
    validate_incremental_index_metadata(index_dir, generation, 1)?;
    let embedder_fingerprint = embedder_fingerprint_for_index_metadata(&stack);

    match document {
        Some(document) => {
            let has_quality_tier = stack.quality().is_some();
            upsert_incremental_document(cx, index_dir, stack, &document).await?;
            verify_published_tier_counts(index_dir, document_counts.total(), has_quality_tier)
                .map_err(|error| {
                    incremental_fallback(IncrementalFallbackReason::TierUnavailable, error)
                })?;
            write_index_metadata(
                index_dir,
                generation,
                document_counts,
                embedder_fingerprint.as_ref(),
            )
            .map_err(|error| {
                incremental_fallback(
                    IncrementalFallbackReason::TierUnavailable,
                    format!("failed to write incremental index metadata: {error}"),
                )
            })?;
            Ok(1)
        }
        None => {
            delete_incremental_document(cx, index_dir, document_id).await?;
            verify_published_tier_counts(
                index_dir,
                document_counts.total(),
                stack.quality().is_some(),
            )
            .map_err(|error| {
                incremental_fallback(IncrementalFallbackReason::TierUnavailable, error)
            })?;
            write_index_metadata(
                index_dir,
                generation,
                document_counts,
                embedder_fingerprint.as_ref(),
            )
            .map_err(|error| {
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
    let Some(metadata) = parse_index_metadata(index_dir).map_err(|error| {
        incremental_fallback(
            IncrementalFallbackReason::CorpusRevisionMismatch,
            format!("failed to read index metadata: {error}"),
        )
    })?
    else {
        return Err(incremental_fallback(
            IncrementalFallbackReason::IndexAbsent,
            format!("index metadata is absent: {}", metadata_path.display()),
        ));
    };
    if let Some(detail) = index_metadata_compatibility_error(&metadata_path, &metadata) {
        return Err(incremental_fallback(
            IncrementalFallbackReason::CorpusRevisionMismatch,
            detail,
        ));
    }
    let document_count = metadata.document_count.ok_or_else(|| {
        incremental_fallback(
            IncrementalFallbackReason::CorpusRevisionMismatch,
            "index metadata does not contain documentCount",
        )
    })?;
    let expect_quality_tier = metadata
        .tier_document_counts
        .is_some_and(|counts| counts.quality.is_some());
    verify_published_tier_counts(index_dir, document_count, expect_quality_tier)
        .map_err(|error| incremental_fallback(IncrementalFallbackReason::TierUnavailable, error))?;
    let index_generation = metadata.generation.ok_or_else(|| {
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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(all(test, feature = "lexical-bm25"))]
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

struct WorkspaceIndexSourceSnapshot {
    generation: u64,
    memories_indexed: u32,
    sessions_indexed: u32,
    artifacts_indexed: u32,
    rules_indexed: u32,
    evidence_indexed: u32,
    document_counts: IndexDocumentCounts,
    documents_total: u32,
    documents: Vec<crate::search::IndexableDocument>,
    evidence_admission: EvidenceAdmissionReport,
    open_job_ids: BTreeSet<String>,
}

/// Capture one writer-fenced source snapshot for every index publisher.
///
/// Generation is read before any corpus table. The surrounding
/// `BEGIN IMMEDIATE` prevents a source writer from committing midway through
/// the multi-table projection, while still releasing the database before
/// expensive embedding and filesystem publication. A writer that commits
/// after this function returns necessarily advances beyond `generation`, so
/// the just-published manifest is truthfully stale rather than falsely ready.
fn collect_workspace_index_source_snapshot(
    db: &DbConnection,
    workspace_id: &str,
) -> Result<WorkspaceIndexSourceSnapshot, IndexRebuildError> {
    db.with_transaction_error(|| {
        let captured_generation = db.get_workspace_generation(workspace_id)?;
        let memories = db.list_memories_for_retrieval_with_global(workspace_id, None, false)?;
        let artifacts = db.list_artifacts(workspace_id, None)?;
        let memory_docs = memory_documents_with_anchors(db, &memories)?;
        let mut session_docs = Vec::new();
        db.visit_sessions_for_workspace_in_current_snapshot(workspace_id, |session| {
            session_docs.push(session_to_document(&session));
            Ok(())
        })?;
        let artifact_docs: Vec<CanonicalSearchDocument> =
            artifacts.iter().map(artifact_to_document).collect();
        let rule_docs = rule_documents(db, workspace_id)?;
        let evidence_selection = evidence_documents(db, workspace_id)?;
        let evidence_admission = evidence_selection.admission;
        let evidence_docs = evidence_selection.documents;
        let document_counts = checked_document_counts(
            memory_docs.len(),
            session_docs.len(),
            artifact_docs.len(),
            rule_docs.len(),
            evidence_docs.len(),
        )?;
        let memories_indexed = document_counts.memories;
        let sessions_indexed = document_counts.sessions;
        let artifacts_indexed = document_counts.artifacts;
        let rules_indexed = document_counts.rules;
        let evidence_indexed = document_counts.evidence;
        let documents_total = document_counts.total();
        let documents = memory_docs
            .into_iter()
            .chain(session_docs)
            .chain(artifact_docs)
            .chain(rule_docs)
            .chain(evidence_docs)
            .map(CanonicalSearchDocument::into_indexable)
            .collect();
        let open_job_ids = db
            .list_search_index_jobs(workspace_id, None)?
            .into_iter()
            .filter(|job| {
                matches!(
                    job.status_enum(),
                    Some(SearchIndexJobStatus::Pending | SearchIndexJobStatus::Running)
                )
            })
            .map(|job| job.id)
            .collect();
        Ok(WorkspaceIndexSourceSnapshot {
            generation: captured_generation.unwrap_or_else(|| u64::from(documents_total)),
            memories_indexed,
            sessions_indexed,
            artifacts_indexed,
            rules_indexed,
            evidence_indexed,
            document_counts,
            documents_total,
            documents,
            evidence_admission,
            open_job_ids,
        })
    })
}

fn memory_documents_with_anchors(
    db: &DbConnection,
    memories: &[crate::db::StoredMemory],
) -> Result<Vec<CanonicalSearchDocument>, IndexRebuildError> {
    let mut documents = Vec::with_capacity(memories.len());
    for memory in memories {
        if memory_has_seal_sidecar(db, memory)? {
            continue;
        }
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
        documents.push(memory_to_document_with_context_anchors_and_typed_fields(
            memory,
            None,
            &[],
            &anchors,
            typed_fields_json.as_deref(),
        ));
    }
    Ok(documents)
}

/// Sealed eligibility is sidecar state, never a content-string heuristic.
///
/// A verified reveal publishes a new live revision without a seal row, while
/// the superseded placeholder retains its seal as historical evidence. Thus
/// the placeholder row is always excluded and the revealed revision is not.
fn memory_has_seal_sidecar(
    db: &DbConnection,
    memory: &crate::db::StoredMemory,
) -> Result<bool, DbError> {
    if memory.content != crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT {
        return Ok(false);
    }
    Ok(db.get_memory_seal(&memory.id)?.is_some())
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

    if let Some(retained_dir) = find_latest_recoverable_retained_dir(index_dir)? {
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

#[cfg(test)]
fn publish_staged_index(index_dir: &Path, staging_dir: &Path) -> Result<(), IndexRebuildError> {
    publish_staged_index_inner(index_dir, staging_dir).map(|_| ())
}

struct PublishedIndexRollbackGuard<'a> {
    index_dir: &'a Path,
    staging_dir: &'a Path,
    retained_dir: Option<PathBuf>,
    armed: bool,
}

impl PublishedIndexRollbackGuard<'_> {
    fn rollback(&mut self) -> Result<(), IndexRebuildError> {
        let result = rollback_published_index(
            self.index_dir,
            self.staging_dir,
            self.retained_dir.as_deref(),
        );
        self.armed = false;
        result
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PublishedIndexRollbackGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Err(error) = self.rollback() {
            tracing::error!(
                target: "ee::index",
                error = %error,
                index_dir = %self.index_dir.display(),
                "failed to roll back index publication while unwinding commit tail"
            );
        }
    }
}

fn publish_staged_index_with_commit<F>(
    index_dir: &Path,
    staging_dir: &Path,
    commit_tail: F,
) -> Result<(), IndexRebuildError>
where
    F: FnOnce() -> Result<(), IndexRebuildError>,
{
    let retained_dir = publish_staged_index_inner(index_dir, staging_dir)?;
    let mut rollback_guard = PublishedIndexRollbackGuard {
        index_dir,
        staging_dir,
        retained_dir,
        armed: true,
    };
    match commit_tail() {
        Ok(()) => {
            rollback_guard.disarm();
            Ok(())
        }
        Err(primary_error) => {
            if let Err(rollback_error) = rollback_guard.rollback() {
                return Err(IndexRebuildError::Index(format!(
                    "index publication commit failed ({primary_error}); filesystem rollback also failed ({rollback_error})"
                )));
            }
            Err(primary_error)
        }
    }
}

fn publish_staged_index_inner(
    index_dir: &Path,
    staging_dir: &Path,
) -> Result<Option<PathBuf>, IndexRebuildError> {
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
        if let Some(retained) = retained_dir.as_deref()
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

    Ok(retained_dir)
}

fn rollback_published_index(
    index_dir: &Path,
    staging_dir: &Path,
    retained_dir: Option<&Path>,
) -> Result<(), IndexRebuildError> {
    ensure_index_path_has_no_symlinks(index_dir, "roll back published index generation")?;
    ensure_index_path_has_no_symlinks(staging_dir, "roll back published index generation")?;
    if let Some(retained_dir) = retained_dir {
        ensure_index_path_has_no_symlinks(
            retained_dir,
            "restore retained index generation after commit failure",
        )?;
    }

    let mut rollback_errors = Vec::new();
    if path_exists_no_follow(staging_dir) {
        rollback_errors.push(format!(
            "staging path unexpectedly exists: {}",
            staging_dir.display()
        ));
    } else if path_exists_no_follow(index_dir) {
        match allocate_rejected_index_dir(index_dir) {
            Ok(rejected_dir) => {
                if let Err(error) = rename_index_dir(
                    index_dir,
                    &rejected_dir,
                    "quarantine unpublished index generation after commit failure",
                ) {
                    rollback_errors.push(error.to_string());
                }
            }
            Err(error) => rollback_errors.push(error.to_string()),
        }
    } else {
        rollback_errors.push(format!(
            "published index path disappeared before rollback: {}",
            index_dir.display()
        ));
    }

    if let Some(retained_dir) = retained_dir {
        if path_exists_no_follow(index_dir) {
            rollback_errors.push(format!(
                "cannot restore retained generation while active path still exists: {}",
                index_dir.display()
            ));
        } else if !path_exists_no_follow(retained_dir) {
            rollback_errors.push(format!(
                "retained generation disappeared before rollback: {}",
                retained_dir.display()
            ));
        } else if let Err(error) = rename_index_dir(
            retained_dir,
            index_dir,
            "restore retained index generation after commit failure",
        ) {
            rollback_errors.push(error.to_string());
        }
    }

    if rollback_errors.is_empty() {
        Ok(())
    } else {
        Err(IndexRebuildError::Index(rollback_errors.join("; ")))
    }
}

/// GH#19: the embedder fingerprint stamped into `.ee/index/meta.json` at
/// publish time so the model-lifecycle readiness collector
/// (`src/core/model.rs`) can prove "registry + asset + index agree".
///
/// Values mirror `active_embedding_registry_input` exactly: the registry row
/// and the index metadata are written from the same resolved embedder, so the
/// strict `exact_dimension_metric_dtype` readiness rule compares like with
/// like.
#[derive(Clone, Debug, Eq, PartialEq)]
struct IndexEmbedderFingerprint {
    model_id: String,
    model_revision: String,
    model_hash: String,
    dimension: u32,
    distance_metric: &'static str,
    vector_dtype: &'static str,
}

/// Derive the metadata fingerprint for the fast embedder that built the
/// index. Returns `None` for non-semantic (hash fallback) or not-yet-loaded
/// embedders, in which case `meta.json` keeps the fingerprint-less shape and
/// readiness correctly stays lexical.
fn embedder_fingerprint_for_index_metadata(
    stack: &EmbedderStack,
) -> Option<IndexEmbedderFingerprint> {
    let fast_embedder = stack.fast();
    if !fast_embedder.is_semantic() || !fast_embedder.is_ready() {
        return None;
    }
    let dimension = u32::try_from(fast_embedder.dimension()).ok()?;
    let provider = provider_for_embedder(fast_embedder);
    let fingerprint = active_embedder_fingerprint(fast_embedder, provider);
    Some(IndexEmbedderFingerprint {
        model_id: fast_embedder.id().to_owned(),
        model_revision: fingerprint.revision,
        model_hash: fingerprint.content_hash,
        dimension,
        distance_metric: ModelDistanceMetric::Cosine.as_str(),
        vector_dtype: EmbeddingVectorDtype::Float32.as_str(),
    })
}

fn write_index_metadata<C>(
    index_dir: &Path,
    generation: u64,
    document_counts: C,
    embedder_fingerprint: Option<&IndexEmbedderFingerprint>,
) -> Result<(), IndexRebuildError>
where
    C: Into<IndexDocumentCounts>,
{
    let document_counts = document_counts.into();
    let timestamp = current_timestamp_rfc3339();
    let quality_tier_present =
        path_is_regular_file_no_follow(&index_dir.join(VECTOR_INDEX_QUALITY_FILE));
    let mut metadata = serde_json::json!({
        "schema": INDEX_METADATA_SCHEMA_V2,
        "generation": generation,
        "sourceGeneration": generation,
        "corpusRevision": expected_index_corpus_revision().as_str(),
        "evidenceSecurityPolicyEpoch": EVIDENCE_SECURITY_POLICY_EPOCH,
        "lastRebuildAt": timestamp,
        "documentCount": document_counts.total(),
        "documentCounts": document_counts.data_json(),
        "tierDocumentCounts": {
            "fast": document_counts.total(),
            "quality": quality_tier_present.then_some(document_counts.total()),
            "lexical": cfg!(feature = "lexical-bm25").then_some(document_counts.total()),
        },
    });
    if let Some(fingerprint) = embedder_fingerprint
        && let Some(object) = metadata.as_object_mut()
    {
        // GH#19: stamp the active embedder fingerprint so the readiness
        // collector's `ModelLifecycleIndexMetadata` reader finds the fields
        // it already parses (`storedDimension` et al.). These are additive
        // optional keys on `ee.index_metadata.v2`; every reader tolerates
        // their absence.
        object.insert(
            "storedModelId".to_owned(),
            serde_json::json!(fingerprint.model_id),
        );
        object.insert(
            "storedModelRevision".to_owned(),
            serde_json::json!(fingerprint.model_revision),
        );
        object.insert(
            "storedModelHash".to_owned(),
            serde_json::json!(fingerprint.model_hash),
        );
        object.insert(
            "storedDimension".to_owned(),
            serde_json::json!(fingerprint.dimension),
        );
        object.insert(
            "storedDistanceMetric".to_owned(),
            serde_json::json!(fingerprint.distance_metric),
        );
        object.insert(
            "storedVectorDtype".to_owned(),
            serde_json::json!(fingerprint.vector_dtype),
        );
    }
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

/// Stamp a freshly built, memory-only evaluation index with the current
/// corpus and evidence-egress policy.
///
/// Evaluation indexes are assembled directly from typed in-process fixture
/// memories and intentionally bypass the workspace database/rebuild pipeline.
/// They still need canonical metadata before `run_search` can open them:
/// missing security metadata must remain indistinguishable from a pre-policy
/// on-disk index everywhere else.
pub(crate) fn write_memory_eval_index_metadata(
    index_dir: &Path,
    documents_total: u32,
) -> Result<(), IndexRebuildError> {
    write_index_metadata(index_dir, 0, documents_total, None)
}

#[derive(Clone, Debug)]
struct ParsedIndexMetadata {
    schema: Option<String>,
    generation: Option<u64>,
    last_rebuild_at: Option<String>,
    corpus_revision: Option<String>,
    evidence_security_policy_epoch: Option<u64>,
    document_count: Option<u32>,
    document_counts: Option<IndexDocumentCounts>,
    tier_document_counts: Option<IndexTierDocumentCounts>,
}

fn parse_index_metadata(index_dir: &Path) -> Result<Option<ParsedIndexMetadata>, String> {
    let metadata_path = index_dir.join(INDEX_METADATA_FILE);
    let Some(content) = read_index_metadata_contents(&metadata_path)? else {
        return Ok(None);
    };
    let parsed: serde_json::Value = serde_json::from_str(&content).map_err(|error| {
        format!(
            "failed to parse index metadata '{}': {error}",
            metadata_path.display()
        )
    })?;
    let object = parsed.as_object().ok_or_else(|| {
        format!(
            "index metadata '{}' must be a JSON object",
            metadata_path.display()
        )
    })?;
    let document_count = match object.get("documentCount") {
        Some(value) => Some(
            value
                .as_u64()
                .and_then(|count| u32::try_from(count).ok())
                .ok_or_else(|| {
                    format!(
                        "index metadata '{}' documentCount must be a u32",
                        metadata_path.display()
                    )
                })?,
        ),
        None => None,
    };
    let document_counts =
        match object.get("documentCounts") {
            Some(value) => Some(IndexDocumentCounts::from_metadata(value).map_err(|error| {
                format!("index metadata '{}': {error}", metadata_path.display())
            })?),
            None => None,
        };
    let tier_document_counts =
        match object.get("tierDocumentCounts") {
            Some(value) => Some(IndexTierDocumentCounts::from_metadata(value).map_err(
                |error| format!("index metadata '{}': {error}", metadata_path.display()),
            )?),
            None => None,
        };
    Ok(Some(ParsedIndexMetadata {
        schema: object
            .get("schema")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        generation: object
            .get("sourceGeneration")
            .or_else(|| object.get("source_generation"))
            .or_else(|| object.get("generation"))
            .and_then(serde_json::Value::as_u64),
        last_rebuild_at: object
            .get("lastRebuildAt")
            .or_else(|| object.get("last_rebuild_at"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        corpus_revision: object
            .get("corpusRevision")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        evidence_security_policy_epoch: object
            .get("evidenceSecurityPolicyEpoch")
            .and_then(serde_json::Value::as_u64),
        document_count,
        document_counts,
        tier_document_counts,
    }))
}

fn index_metadata_compatibility_error(
    metadata_path: &Path,
    metadata: &ParsedIndexMetadata,
) -> Option<String> {
    let expected_revision = expected_index_corpus_revision().as_str();
    if metadata.corpus_revision.as_deref() != Some(expected_revision) {
        return Some(format!(
            "index metadata '{}' has incompatible corpus revision {:?}; expected {expected_revision} and a full index rebuild is required",
            metadata_path.display(),
            metadata.corpus_revision
        ));
    }
    if metadata.schema.as_deref() != Some(INDEX_METADATA_SCHEMA_V2) {
        return Some(format!(
            "index metadata '{}' uses schema {:?}; current schema is {INDEX_METADATA_SCHEMA_V2} and a full index rebuild is required",
            metadata_path.display(),
            metadata.schema
        ));
    }
    if metadata.evidence_security_policy_epoch != Some(u64::from(EVIDENCE_SECURITY_POLICY_EPOCH)) {
        return Some(format!(
            "index metadata '{}' has incompatible evidence security policy epoch {:?}; current epoch is {} and a full index rebuild is required",
            metadata_path.display(),
            metadata.evidence_security_policy_epoch,
            EVIDENCE_SECURITY_POLICY_EPOCH
        ));
    }
    let Some(document_count) = metadata.document_count else {
        return Some(format!(
            "index metadata '{}' is missing documentCount; a full index rebuild is required",
            metadata_path.display()
        ));
    };
    let Some(document_counts) = metadata.document_counts else {
        return Some(format!(
            "index metadata '{}' is missing documentCounts; a full index rebuild is required",
            metadata_path.display()
        ));
    };
    if document_counts.total() != document_count {
        return Some(format!(
            "index metadata '{}' documentCount {document_count} disagrees with per-kind total {}; a full index rebuild is required",
            metadata_path.display(),
            document_counts.total()
        ));
    }
    let Some(tier_counts) = metadata.tier_document_counts else {
        return Some(format!(
            "index metadata '{}' is missing tierDocumentCounts; a full index rebuild is required",
            metadata_path.display()
        ));
    };
    if tier_counts.fast != document_count
        || tier_counts
            .quality
            .is_some_and(|count| count != document_count)
        || tier_counts
            .lexical
            .is_some_and(|count| count != document_count)
        || (cfg!(feature = "lexical-bm25") && tier_counts.lexical != Some(document_count))
        || (!cfg!(feature = "lexical-bm25") && tier_counts.lexical.is_some())
    {
        return Some(format!(
            "index metadata '{}' tier document counts {:?} disagree with documentCount {document_count}; a full index rebuild is required",
            metadata_path.display(),
            tier_counts
        ));
    }
    None
}

/// Whether an existing derived index was built under the current complete
/// corpus and evidence-egress policy.
///
/// Missing, malformed, oversized, symlinked, or pre-policy metadata all fail
/// closed. Search callers must check this before opening any lexical/vector
/// index bytes because a stale generation can contain content that is no
/// longer admissible.
pub(crate) fn index_corpus_compatibility_is_current(index_dir: &Path) -> bool {
    index_generation_is_recoverable(index_dir)
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
            && index_generation_is_recoverable(&entry.path())
        {
            candidates.push(entry.path());
        }
    }
    candidates.sort();
    Ok(candidates.pop())
}

fn index_generation_is_recoverable(index_dir: &Path) -> bool {
    recoverable_index_generation(index_dir).is_some()
}

fn recoverable_index_generation(index_dir: &Path) -> Option<u64> {
    let Ok(Some(metadata)) = parse_index_metadata(index_dir) else {
        return None;
    };
    if index_metadata_compatibility_error(&index_dir.join(INDEX_METADATA_FILE), &metadata).is_some()
    {
        return None;
    }
    let Some(document_count) = metadata.document_count else {
        return None;
    };
    let Some(tier_counts) = metadata.tier_document_counts else {
        return None;
    };
    verify_published_tier_counts(index_dir, document_count, tier_counts.quality.is_some())
        .ok()
        .map(|()| metadata.generation.unwrap_or(0))
}

fn retained_generation_sequence(name: &str, retained_prefix: &str) -> Option<u32> {
    if name == retained_prefix {
        return Some(0);
    }
    let suffix = name.strip_prefix(retained_prefix)?.strip_prefix('.')?;
    if suffix.len() != 3 || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let sequence = suffix.parse::<u32>().ok()?;
    (sequence > 0).then_some(sequence)
}

fn find_latest_recoverable_retained_dir(
    index_dir: &Path,
) -> Result<Option<PathBuf>, IndexRebuildError> {
    let parent = index_parent(index_dir);
    if !path_exists_no_follow(parent) {
        return Ok(None);
    }
    let retained_prefix = format!("{}{INDEX_RETAINED_SUFFIX}", index_base_name(index_dir)?);
    let entries = std::fs::read_dir(parent).map_err(|error| {
        IndexRebuildError::Index(format!(
            "Failed to inspect retained index generations in '{}': {error}",
            parent.display()
        ))
    })?;
    let mut latest: Option<(u64, u32, PathBuf)> = None;

    for entry in entries {
        let entry = entry.map_err(|error| {
            IndexRebuildError::Index(format!(
                "Failed to inspect a retained index generation in '{}': {error}",
                parent.display()
            ))
        })?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(sequence) = retained_generation_sequence(&name, &retained_prefix) else {
            continue;
        };
        let candidate = entry.path();
        ensure_index_path_has_no_symlinks(
            &candidate,
            "inspect retained index generation for recovery",
        )?;
        if !entry
            .file_type()
            .map_err(|error| {
                IndexRebuildError::Index(format!(
                    "Failed to inspect retained index generation '{}': {error}",
                    candidate.display()
                ))
            })?
            .is_dir()
        {
            continue;
        }
        let Some(generation) = recoverable_index_generation(&candidate) else {
            continue;
        };
        let key = (generation, sequence, candidate);
        if latest.as_ref().is_none_or(|current| &key > current) {
            latest = Some(key);
        }
    }

    Ok(latest.map(|(_, _, path)| path))
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

fn allocate_rejected_index_dir(index_dir: &Path) -> Result<PathBuf, IndexRebuildError> {
    let parent = index_parent(index_dir);
    let base = index_base_name(index_dir)?;
    let stamp = monotonicish_stamp();
    for sequence in 0_u32..1000 {
        let candidate = parent.join(format!(
            ".{base}{INDEX_REJECTED_PREFIX}{stamp}-{sequence:03}"
        ));
        if !path_exists_no_follow(&candidate) {
            return Ok(candidate);
        }
    }

    Err(IndexRebuildError::Index(
        "Failed to allocate rejected index generation directory".to_string(),
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
    // macOS exposes its process temp directory through the root-owned `/var`
    // compatibility symlink even though the backing directory lives below
    // `/private/var`. Treat only that OS-selected prefix as already resolved;
    // every component below the temp root is still inspected without following
    // symlinks, so an attacker-controlled parent inside the temp tree remains
    // rejected.
    let inspected_path = crate::util::path_with_canonical_process_temp_prefix(path);
    let mut current = PathBuf::new();
    for component in inspected_path.components() {
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
    rule_count: usize,
    evidence_count: usize,
) -> Result<IndexDocumentCounts, IndexRebuildError> {
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
    let rules_indexed = u32::try_from(rule_count).map_err(|_| {
        IndexRebuildError::Index(format!(
            "Rule document count {rule_count} exceeds the supported maximum."
        ))
    })?;
    let evidence_indexed = u32::try_from(evidence_count).map_err(|_| {
        IndexRebuildError::Index(format!(
            "Evidence document count {evidence_count} exceeds the supported maximum."
        ))
    })?;
    IndexDocumentCounts::checked(
        memories_indexed,
        sessions_indexed,
        artifacts_indexed,
        rules_indexed,
        evidence_indexed,
    )
    .map_err(IndexRebuildError::Index)
}

/// Collect the indexable procedural-rule documents for a workspace.
///
/// Applied rules are part of the derived search corpus (bd-3h6bz): without
/// them, `document_source=rule` index jobs point at documents that can
/// never exist and the Learn -> Retrieve -> Pack loop dead-ends. Tombstoned
/// rows are excluded by the query; superseded rules are filtered here so
/// only the current head of a supersede chain is retrievable.
fn rule_documents(
    db: &DbConnection,
    workspace_id: &str,
) -> Result<Vec<CanonicalSearchDocument>, IndexRebuildError> {
    let workspace = db.get_workspace(workspace_id)?.ok_or_else(|| {
        IndexRebuildError::Index(format!(
            "Workspace {workspace_id} disappeared while projecting procedural rules."
        ))
    })?;
    let rules = db.list_procedural_rules(workspace_id, None, None, false)?;
    let mut tags_by_rule = db.list_rule_tags_for_workspace(workspace_id)?;
    let mut sources_by_rule = db.list_rule_source_memory_ids_for_workspace(workspace_id)?;
    Ok(rules
        .into_iter()
        .map(|rule| {
            let tags = tags_by_rule.remove(&rule.id).unwrap_or_default();
            let sources = sources_by_rule.remove(&rule.id).unwrap_or_default();
            RuleIndexProjection::new(rule, workspace.path.as_str(), tags, sources)
        })
        .filter(RuleIndexProjection::is_search_indexable)
        .map(|projection| rule_to_document(&projection))
        .collect())
}

/// Collect the indexable imported-evidence documents for a workspace.
///
/// `ee import cass` persists transcript excerpts as `evidence_spans`, but
/// the corpus previously carried only session METADATA documents, so a
/// unique phrase from an imported transcript was undiscoverable by
/// `ee search` (bd-16imy). Every row is positively re-admitted from live
/// storage before projection, then screened again at egress. The listing
/// query orders deterministically by
/// `(session_id, start_line, end_line, id)`.
struct EvidenceDocumentSelection {
    documents: Vec<CanonicalSearchDocument>,
    admission: EvidenceAdmissionReport,
}

fn evidence_documents(
    db: &DbConnection,
    workspace_id: &str,
) -> Result<EvidenceDocumentSelection, IndexRebuildError> {
    let mut documents = Vec::new();
    let scan =
        db.visit_search_admitted_evidence_spans_in_current_snapshot(workspace_id, |span| {
            documents.push(evidence_span_to_document(&span));
            Ok(())
        })?;
    Ok(EvidenceDocumentSelection {
        documents,
        admission: scan.admission,
    })
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

/// GH#20: resolve the workspace row targeted by `--workspace` for the index
/// entry points (`rebuild`, `reembed`, `process-jobs`).
///
/// Looks the requested path up by canonical root first and lexical key second
/// (same contract as `workspace_id_for_index_status`). Only when the path is
/// not registered at all does it fall back to the newest-created workspace
/// row, which preserves the historical single-workspace behavior for
/// databases whose lone workspace row was registered under a different path
/// spelling.
fn resolve_index_workspace_id(
    db: &DbConnection,
    workspace_path: &Path,
) -> Result<String, IndexRebuildError> {
    if let Some(workspace_id) = workspace_id_for_index_status(db, workspace_path)? {
        return Ok(workspace_id);
    }
    get_default_workspace_id(db)
}

#[derive(Debug)]
struct BuildStats {
    source_count: usize,
    doc_count: usize,
    error_count: usize,
    has_quality_index: bool,
    errors: Vec<(String, String)>,
}

/// Build a complete index generation, including the zero-document generation.
///
/// Frankensearch intentionally rejects an empty [`IndexBuilder`] input. An
/// empty source corpus is nevertheless a real derived-asset state: publishing
/// it is what removes the last tombstoned/superseded document and advances the
/// manifest to the captured database generation. The empty path creates the
/// same fast/quality/lexical tiers as a normal build, just without records.
async fn build_index_generation(
    cx: &asupersync::Cx,
    index_dir: &Path,
    stack: EmbedderStack,
    documents: Vec<crate::search::IndexableDocument>,
) -> Result<BuildStats, IndexRebuildError> {
    index_checkpoint(cx)?;
    if documents.is_empty() {
        build_empty_index(cx, index_dir, stack).await
    } else {
        build_index(cx, index_dir, stack, documents).await
    }
}

async fn build_empty_index(
    cx: &asupersync::Cx,
    index_dir: &Path,
    stack: EmbedderStack,
) -> Result<BuildStats, IndexRebuildError> {
    index_checkpoint(cx)?;
    std::fs::create_dir_all(index_dir).map_err(|error| {
        IndexRebuildError::Index(format!(
            "failed to create empty index directory {}: {error}",
            index_dir.display()
        ))
    })?;

    let has_quality_index = stack.quality().is_some();
    let fast_embedder = stack.fast_arc();
    let fast_writer = VectorIndex::create(
        &index_dir.join(VECTOR_INDEX_FAST_FILE),
        fast_embedder.id(),
        fast_embedder.dimension(),
    )
    .map_err(|error| {
        IndexRebuildError::Index(format!("failed to create empty fast vector tier: {error}"))
    })?;
    fast_writer.finish().map_err(|error| {
        IndexRebuildError::Index(format!("failed to finish empty fast vector tier: {error}"))
    })?;
    index_checkpoint(cx)?;

    if let Some(quality_embedder) = stack.quality_arc() {
        let quality_writer = VectorIndex::create(
            &index_dir.join(VECTOR_INDEX_QUALITY_FILE),
            quality_embedder.id(),
            quality_embedder.dimension(),
        )
        .map_err(|error| {
            IndexRebuildError::Index(format!(
                "failed to create empty quality vector tier: {error}"
            ))
        })?;
        quality_writer.finish().map_err(|error| {
            IndexRebuildError::Index(format!(
                "failed to finish empty quality vector tier: {error}"
            ))
        })?;
        index_checkpoint(cx)?;
    }

    #[cfg(feature = "lexical-bm25")]
    {
        let lexical_path = index_dir.join(LEXICAL_INDEX_SUBDIR);
        let lexical = TantivyIndex::create(&lexical_path).map_err(|error| {
            IndexRebuildError::Index(format!("failed to create empty lexical tier: {error}"))
        })?;
        lexical.commit(cx).await.map_err(|error| {
            map_index_search_error(cx, "failed to commit empty lexical tier", error)
        })?;
        index_checkpoint(cx)?;
    }

    Ok(BuildStats {
        source_count: 0,
        doc_count: 0,
        error_count: 0,
        has_quality_index,
        errors: Vec::new(),
    })
}

/// Build the Tantivy lexical tier for a full rebuild.
///
/// frankensearch d117ce1f ("make Quill the lexical feature backend") left
/// `IndexBuilder` without a Tantivy write arm for the `lexical-tantivy`
/// (cass-compat) lane ee stays on, so full rebuilds stopped producing
/// `<index_dir>/lexical`. This replicates the retired
/// `lexical`-without-`quill` builder arm exactly: skip when there are no
/// staged documents (the old arm never created the directory then), admit
/// every source document with per-document failures logged and non-fatal
/// (they surface later as a persisted-count mismatch in
/// `verify_published_tier_counts`, matching the old ignored-receipt
/// semantics), and keep create/commit failures fatal — a half-written
/// lexical index is worse than an absent arm.
#[cfg(feature = "lexical-bm25")]
pub(crate) async fn build_lexical_tier(
    cx: &asupersync::Cx,
    index_dir: &Path,
    documents: &[crate::search::IndexableDocument],
) -> Result<(), IndexRebuildError> {
    index_checkpoint(cx)?;
    if documents.is_empty() {
        return Ok(());
    }
    let lexical_path = index_dir.join(LEXICAL_INDEX_SUBDIR);
    let lexical = TantivyIndex::create(&lexical_path).map_err(|error| {
        IndexRebuildError::Index(format!("failed to create lexical tier: {error}"))
    })?;
    for document in documents {
        match lexical.index_document(cx, document).await {
            Ok(()) => {}
            Err(error @ SearchError::Cancelled { .. }) => {
                return Err(map_index_search_error(
                    cx,
                    "lexical indexing cancelled",
                    error,
                ));
            }
            Err(error) => {
                tracing::warn!(
                    doc_id = %document.id,
                    error = %error,
                    "lexical indexing failed for document"
                );
            }
        }
        index_checkpoint(cx)?;
    }
    lexical
        .commit(cx)
        .await
        .map_err(|error| map_index_search_error(cx, "failed to commit lexical tier", error))?;
    index_checkpoint(cx)
}

async fn build_index(
    cx: &asupersync::Cx,
    index_dir: &Path,
    stack: EmbedderStack,
    documents: Vec<crate::search::IndexableDocument>,
) -> Result<BuildStats, IndexRebuildError> {
    index_checkpoint(cx)?;
    let source_count = documents.len();
    // frankensearch d117ce1f made Quill the backend of its `lexical`
    // feature, so `IndexBuilder` no longer compiles a Tantivy write arm under
    // ee's `lexical-tantivy` lane. Stage the same corpus for ee's Tantivy tier.
    #[cfg(feature = "lexical-bm25")]
    let lexical_documents = documents.clone();
    let builder = IndexBuilder::new(index_dir)
        .with_embedder_stack(stack)
        .add_documents(documents);

    let stats = builder
        .build(cx)
        .await
        .map_err(|error| map_index_search_error(cx, "index build failed", error))?;
    index_checkpoint(cx)?;

    #[cfg(feature = "lexical-bm25")]
    build_lexical_tier(cx, index_dir, &lexical_documents).await?;
    index_checkpoint(cx)?;

    let errors = stats
        .errors
        .into_iter()
        .map(|(id, error)| (id, format!("fast tier: {error}")))
        .collect::<Vec<_>>();
    Ok(BuildStats {
        source_count,
        doc_count: stats.doc_count,
        error_count: stats.error_count,
        has_quality_index: stats.has_quality_index,
        errors,
    })
}

#[cfg(test)]
fn build_index_sync(
    index_dir: &Path,
    stack: EmbedderStack,
    documents: Vec<crate::search::IndexableDocument>,
) -> Result<BuildStats, String> {
    crate::core::run_cli_with_cx(Duration::from_secs(300), |cx| async move {
        build_index(&cx, index_dir, stack, documents).await
    })
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}

#[cfg(test)]
fn build_index_generation_sync(
    index_dir: &Path,
    stack: EmbedderStack,
    documents: Vec<crate::search::IndexableDocument>,
) -> Result<BuildStats, String> {
    crate::core::run_cli_with_cx(Duration::from_secs(300), |cx| async move {
        build_index_generation(&cx, index_dir, stack, documents).await
    })
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}

fn validate_built_generation(
    index_dir: &Path,
    stats: BuildStats,
    document_counts: IndexDocumentCounts,
) -> Result<BuildStats, String> {
    let expected = usize::try_from(document_counts.total()).unwrap_or(usize::MAX);
    let mut violations = stats
        .errors
        .iter()
        .map(|(id, error)| format!("{id}: {error}"))
        .collect::<Vec<_>>();
    if stats.source_count != expected {
        violations.push(format!(
            "source document count mismatch: expected {expected}, build received {}",
            stats.source_count
        ));
    }
    if stats.doc_count != expected {
        violations.push(format!(
            "fast-tier document count mismatch: expected {expected}, built {}",
            stats.doc_count
        ));
    }
    if stats.error_count != stats.errors.len() {
        violations.push(format!(
            "fast-tier error accounting mismatch: build reported {}, preserved {}",
            stats.error_count,
            stats.errors.len()
        ));
    }
    if stats
        .doc_count
        .checked_add(stats.error_count)
        .is_none_or(|accounted| accounted != stats.source_count)
    {
        violations.push(format!(
            "fast-tier source accounting mismatch: received {}, indexed {}, failed {}",
            stats.source_count, stats.doc_count, stats.error_count
        ));
    }
    if violations.is_empty()
        && let Err(error) = verify_published_tier_counts(
            index_dir,
            document_counts.total(),
            stats.has_quality_index,
        )
    {
        violations.push(error);
    }
    if violations.is_empty() {
        Ok(stats)
    } else {
        violations.sort();
        Err(format!(
            "refusing to publish incomplete index generation: {}",
            violations.join("; ")
        ))
    }
}

fn verify_published_tier_counts(
    index_dir: &Path,
    documents_total: u32,
    expect_quality_tier: bool,
) -> Result<(), String> {
    let expected = usize::try_from(documents_total).unwrap_or(usize::MAX);
    let fast = open_fast_vector_index(index_dir).map_err(|fallback| fallback.detail)?;
    if fast.record_count() != expected {
        return Err(format!(
            "fast-tier persisted document count mismatch: expected {expected}, found {}",
            fast.record_count()
        ));
    }

    let quality = open_quality_vector_index(index_dir).map_err(|fallback| fallback.detail)?;
    match (expect_quality_tier, quality) {
        (true, Some(index)) if index.record_count() == expected => {}
        (true, Some(index)) => {
            return Err(format!(
                "quality-tier persisted document count mismatch: expected {expected}, found {}",
                index.record_count()
            ));
        }
        (true, None) => {
            return Err("quality-tier index is missing from a two-tier generation".to_owned());
        }
        (false, Some(_)) => {
            return Err(
                "quality-tier index is present for a generation built without a quality embedder"
                    .to_owned(),
            );
        }
        (false, None) => {}
    }

    #[cfg(feature = "lexical-bm25")]
    {
        let lexical_path = index_dir.join(LEXICAL_INDEX_SUBDIR);
        let lexical = TantivyIndex::open(&lexical_path).map_err(|error| {
            format!(
                "failed to open lexical tier {} for count verification: {error}",
                lexical_path.display()
            )
        })?;
        let actual = LexicalRead::doc_count(&lexical)
            .map_err(|error| format!("failed to read lexical-tier document count: {error}"))?;
        if actual != expected {
            return Err(format!(
                "lexical-tier persisted document count mismatch: expected {expected}, found {actual}"
            ));
        }
    }

    Ok(())
}

const EE_MODEL_CACHE_SUBDIR: &str = "models";
const EE_MODEL2VEC_REGISTRY_SUBDIR: &str = "model2vec";
const EMBEDDING_REGISTRY_FINGERPRINT_SCHEMA: &str = "ee.embedding_registry_fingerprint.v1";
pub(crate) const POTION_MODEL_NAME: &str = "potion-multilingual-128M";
pub(crate) const EMBED_BACKEND_HASH_FALLBACK: &str = EmbedBackend::HashFallback.as_str();
const EE_EMBED_DOWNLOAD_AUTO: &str = "auto";
const EE_EMBED_DOWNLOAD_OFF: &str = "off";
const EE_DOWNLOAD_STATE_PENDING: u8 = 0;
const EE_DOWNLOAD_STATE_READY: u8 = 1;
const EE_DOWNLOAD_STATE_FAILED: u8 = 2;
static DEFAULT_SEARCH_EMBEDDER: OnceLock<DefaultSearchEmbedder> = OnceLock::new();
static REGISTERED_MODEL2VEC_CACHE: OnceLock<RegisteredModel2VecCache> = OnceLock::new();

#[derive(Clone, Debug, Eq, PartialEq)]
struct RegisteredModel2VecIdentity {
    canonical_source: PathBuf,
    content_hash: String,
    dimension: u32,
    distance_metric: &'static str,
}

struct RegisteredModel2VecCacheEntry {
    identity: RegisteredModel2VecIdentity,
    embedder: Arc<dyn crate::search::Embedder>,
}

#[derive(Default)]
struct RegisteredModel2VecCache {
    current: Mutex<Option<RegisteredModel2VecCacheEntry>>,
}

impl RegisteredModel2VecCache {
    fn get_or_try_insert_with(
        &self,
        identity: RegisteredModel2VecIdentity,
        load: impl FnOnce() -> Option<Arc<dyn crate::search::Embedder>>,
    ) -> Option<Arc<dyn crate::search::Embedder>> {
        let mut current = self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = current.as_ref()
            && entry.identity == identity
        {
            return Some(Arc::clone(&entry.embedder));
        }
        let embedder = load()?;
        *current = Some(RegisteredModel2VecCacheEntry {
            identity,
            embedder: Arc::clone(&embedder),
        });
        Some(embedder)
    }
}

struct DefaultSearchEmbedder {
    stack: EmbedderStack,
    lazy_model2vec: Option<Arc<EeLazyModel2VecEmbedder>>,
    model_resolution: EmbedModelResolution,
}

impl DefaultSearchEmbedder {
    fn ready(stack: EmbedderStack, model_resolution: EmbedModelResolution) -> Self {
        Self {
            stack,
            lazy_model2vec: None,
            model_resolution,
        }
    }
}

pub(crate) struct EmbedderPreparation {
    pub(crate) backend: EmbedBackend,
    pub(crate) model_resolution: EmbedModelResolution,
    pub(crate) elapsed: Duration,
    pub(crate) fast_embedder: Arc<dyn crate::search::Embedder>,
}

impl EmbedderPreparation {
    fn new(
        backend: EmbedBackend,
        model_resolution: EmbedModelResolution,
        elapsed: Duration,
        fast_embedder: Arc<dyn crate::search::Embedder>,
    ) -> Self {
        let preparation = Self {
            backend,
            model_resolution,
            elapsed,
            fast_embedder,
        };
        debug_assert!(
            preparation
                .model_resolution
                .is_valid_for_backend(preparation.backend),
            "embedding resolution source/outcome must agree with the executed backend"
        );
        tracing::info!(
            target: "ee::index::embedder",
            backend = preparation.backend.as_str(),
            source = preparation.model_resolution.source.as_str(),
            outcome = preparation.model_resolution.outcome.as_str(),
            "embedding backend resolution completed"
        );
        preparation
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EeEmbedderSettings {
    model_root: PathBuf,
    download_mode: EeEmbedDownloadMode,
    local_source: EmbedModelSource,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EeEmbedDownloadMode {
    Auto,
    Off,
}

pub(crate) fn default_search_embedder_stack() -> EmbedderStack {
    DEFAULT_SEARCH_EMBEDDER
        .get_or_init(detect_default_search_embedder)
        .stack
        .clone()
}

fn detect_default_search_embedder() -> DefaultSearchEmbedder {
    let settings = default_embedder_settings();
    default_search_embedder_for_settings(&settings)
}

#[cfg(test)]
fn search_embedder_stack_for_settings(settings: &EeEmbedderSettings) -> EmbedderStack {
    default_search_embedder_for_settings(settings).stack
}

fn default_search_embedder_for_settings(settings: &EeEmbedderSettings) -> DefaultSearchEmbedder {
    tracing::info!(
        target: "ee::index::embedder",
        download_mode = ?settings.download_mode,
        "ee embedding model policy resolved"
    );

    if settings.download_mode == EeEmbedDownloadMode::Off {
        // EE_EMBED_DOWNLOAD=off means "never fetch over the network", NOT "never
        // use a model". A host that pre-populated the cache (air-gapped install)
        // or already ran an ee-managed download must still get semantic search.
        // Consult the on-disk model first and only fall back to the
        // deterministic hash embedder when no local semantic model is present.
        // Load the frozen local artifact directly instead of invoking general
        // auto-detection. That makes this branch independent of ambient remote
        // provider intent and, by construction, never instantiates either a
        // downloader or a remote embedder. (GH#18: the previous
        // unconditional hash fallback made search report
        // `semantic:false / frankensearch_hash_fallback` even with a valid model
        // on disk, contradicting `ee index reembed`.)
        let model_dir = potion_model_destination_dir(&settings.model_root);
        if verified_potion_model_dir(&model_dir) {
            match Model2VecEmbedder::load(&model_dir) {
                Ok(embedder) => {
                    let fast = Arc::new(embedder) as Arc<dyn crate::search::Embedder>;
                    let stack = EmbedderStack::from_parts(fast, None);
                    tracing::info!(
                        target: "ee::index::embedder",
                        detected_fast = stack.fast().id(),
                        "EE_EMBED_DOWNLOAD=off; using verified on-disk semantic model"
                    );
                    return DefaultSearchEmbedder::ready(
                        stack_with_hash_quality_fallback(stack),
                        EmbedModelResolution::ready(settings.local_source),
                    );
                }
                Err(_) => {
                    tracing::warn!(
                        target: "ee::index::embedder",
                        reason = "verified_local_model_load_failed",
                        "EE_EMBED_DOWNLOAD=off; verified local model could not be loaded"
                    );
                }
            }
        } else {
            tracing::info!(
                target: "ee::index::embedder",
                "EE_EMBED_DOWNLOAD=off and no verified on-disk semantic model; using deterministic hash fallback"
            );
        }
        return DefaultSearchEmbedder::ready(
            hash_fallback_embedder_stack(),
            EmbedModelResolution::deterministic_hash(),
        );
    }

    match EmbedderStack::auto_detect_with(Some(&settings.model_root)) {
        Ok(stack) if stack.fast().is_semantic() => DefaultSearchEmbedder::ready(
            stack_with_hash_quality_fallback(stack),
            EmbedModelResolution::ready(settings.local_source),
        ),
        Ok(stack) => {
            tracing::info!(
                target: "ee::index::embedder",
                detected_fast = stack.fast().id(),
                "semantic model not present locally; enabling ee-managed first-use download"
            );
            ee_auto_download_embedder(settings.model_root.clone())
        }
        Err(_) => {
            tracing::warn!(
                target: "ee::index::embedder",
                reason = "auto_detect_failed",
                "Frankensearch default embedder auto-detect failed; enabling ee-managed first-use download"
            );
            ee_auto_download_embedder(settings.model_root.clone())
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

fn ee_auto_download_embedder(model_root: PathBuf) -> DefaultSearchEmbedder {
    let lazy_model2vec = Arc::new(EeLazyModel2VecEmbedder::new(model_root));
    let fast_embedder = Arc::clone(&lazy_model2vec) as Arc<dyn crate::search::Embedder>;
    DefaultSearchEmbedder {
        stack: EmbedderStack::from_parts(fast_embedder, None),
        lazy_model2vec: Some(lazy_model2vec),
        model_resolution: EmbedModelResolution::ready(EmbedModelSource::Downloaded),
    }
}

fn default_embedder_settings() -> EeEmbedderSettings {
    let configured_model_root = configured_embedder_model_root();
    EeEmbedderSettings {
        model_root: configured_model_root
            .clone()
            .unwrap_or_else(default_embedder_model_root),
        download_mode: default_embed_download_mode(),
        local_source: if configured_model_root.is_some() {
            EmbedModelSource::Configured
        } else {
            EmbedModelSource::Cache
        },
    }
}

pub(crate) fn default_embedder_model_root() -> PathBuf {
    if let Some(model_dir) = configured_embedder_model_root() {
        return model_dir;
    }
    let model_cache_root = process_ee_data_dir()
        .unwrap_or_else(stable_ee_data_dir_fallback)
        .join(EE_MODEL_CACHE_SUBDIR);
    resolve_default_embedder_model_root(&model_cache_root)
}

fn resolve_default_embedder_model_root(model_cache_root: &Path) -> PathBuf {
    resolve_default_embedder_model_root_with(model_cache_root, verified_potion_model_dir)
}

fn resolve_default_embedder_model_root_with(
    model_cache_root: &Path,
    mut is_verified_model_dir: impl FnMut(&Path) -> bool,
) -> PathBuf {
    let registry_root = model_cache_root.join(EE_MODEL2VEC_REGISTRY_SUBDIR);
    let registry_model_dir = potion_model_destination_dir(&registry_root);
    if is_verified_model_dir(&registry_model_dir) {
        return registry_root;
    }
    model_cache_root.to_path_buf()
}

fn registry_source_is_local(source: &str) -> bool {
    !source.trim().is_empty()
        && !source.contains("://")
        && !source.starts_with("urn:")
        && !source.starts_with("model:")
}

fn registered_model2vec_source_path(
    db: &DbConnection,
    entry: &StoredModelRegistryEntry,
) -> Result<PathBuf, EmbedRegistryRejectionReason> {
    let Some(source) = entry
        .source_uri
        .as_deref()
        .map(str::trim)
        .filter(|source| !source.is_empty())
    else {
        return Err(EmbedRegistryRejectionReason::SourceMissing);
    };
    if !registry_source_is_local(source) {
        return Err(EmbedRegistryRejectionReason::SourceNonLocal);
    }
    let path = Path::new(source);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    db.get_workspace(&entry.workspace_id)
        .ok()
        .flatten()
        .map(|workspace| PathBuf::from(workspace.path).join(path))
        .ok_or(EmbedRegistryRejectionReason::SourceWorkspaceMissing)
}

enum RegisteredModel2VecResolution {
    NotRegistered,
    Ready(EmbedderStack),
    Rejected(EmbedModelResolution),
}

struct WorkspaceRegistryEmbedderSelection {
    stack: EmbedderStack,
    model_resolution: EmbedModelResolution,
}

fn workspace_registry_selection_from_resolution(
    resolution: RegisteredModel2VecResolution,
) -> Option<WorkspaceRegistryEmbedderSelection> {
    match resolution {
        RegisteredModel2VecResolution::NotRegistered => None,
        RegisteredModel2VecResolution::Ready(stack) => Some(WorkspaceRegistryEmbedderSelection {
            stack,
            model_resolution: EmbedModelResolution::ready(EmbedModelSource::Registered),
        }),
        RegisteredModel2VecResolution::Rejected(model_resolution) => {
            Some(WorkspaceRegistryEmbedderSelection {
                stack: hash_fallback_embedder_stack(),
                model_resolution,
            })
        }
    }
}

fn workspace_registry_embedder_selection(
    db: &DbConnection,
    workspace_id: &str,
) -> Result<Option<WorkspaceRegistryEmbedderSelection>, DbError> {
    registered_model2vec_resolution(db, workspace_id)
        .map(workspace_registry_selection_from_resolution)
}

fn rejected_registered_model2vec(
    entry: &StoredModelRegistryEntry,
    reason: EmbedRegistryRejectionReason,
) -> RegisteredModel2VecResolution {
    tracing::warn!(
        target: "ee::index::embedder",
        registry_id = entry.id,
        reason = reason.as_str(),
        "rejecting unusable registered Model2Vec entry"
    );
    RegisteredModel2VecResolution::Rejected(EmbedModelResolution::registry_rejected(
        entry.id.clone(),
        reason,
    ))
}

fn model_registry_hash_is_well_formed(value: &str) -> bool {
    value.len() == "blake3:".len() + 64
        && value.starts_with("blake3:")
        && value["blake3:".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
}

fn source_io_rejection(error: &io::Error) -> EmbedRegistryRejectionReason {
    match error.kind() {
        io::ErrorKind::NotFound => EmbedRegistryRejectionReason::SourceNotFound,
        io::ErrorKind::PermissionDenied => EmbedRegistryRejectionReason::SourcePermissionDenied,
        _ => EmbedRegistryRejectionReason::SourceUnreadable,
    }
}

fn registered_model2vec_resolution(
    db: &DbConnection,
    workspace_id: &str,
) -> Result<RegisteredModel2VecResolution, DbError> {
    let mut entries = db
        .list_model_registry_entries(workspace_id)?
        .into_iter()
        .filter(|entry| {
            entry.provider == ModelProvider::Model2Vec && entry.purpose == ModelPurpose::Embedding
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.id.cmp(&right.id));
    let Some(entry) = entries.first() else {
        return Ok(RegisteredModel2VecResolution::NotRegistered);
    };
    if entries.len() != 1 {
        return Ok(rejected_registered_model2vec(
            entry,
            EmbedRegistryRejectionReason::AmbiguousEntries,
        ));
    }
    if entry.model_name != POTION_MODEL_NAME {
        return Ok(rejected_registered_model2vec(
            entry,
            EmbedRegistryRejectionReason::ModelNameMismatch,
        ));
    }
    if entry.status != ModelRegistryStatus::Available {
        return Ok(rejected_registered_model2vec(
            entry,
            EmbedRegistryRejectionReason::StatusNotAvailable,
        ));
    }
    let model_dir = match registered_model2vec_source_path(db, entry) {
        Ok(model_dir) => model_dir,
        Err(reason) => return Ok(rejected_registered_model2vec(entry, reason)),
    };
    let source_metadata = match std::fs::symlink_metadata(&model_dir) {
        Ok(metadata) => metadata,
        Err(error) => {
            return Ok(rejected_registered_model2vec(
                entry,
                source_io_rejection(&error),
            ));
        }
    };
    if source_metadata.file_type().is_symlink() {
        return Ok(rejected_registered_model2vec(
            entry,
            EmbedRegistryRejectionReason::SourceSymlink,
        ));
    }
    if !source_metadata.is_dir() {
        return Ok(rejected_registered_model2vec(
            entry,
            EmbedRegistryRejectionReason::SourceNotDirectory,
        ));
    }
    let canonical_source = match std::fs::canonicalize(&model_dir) {
        Ok(path) => path,
        Err(error) => {
            return Ok(rejected_registered_model2vec(
                entry,
                source_io_rejection(&error),
            ));
        }
    };
    let manifest = ModelManifest::potion_128m();
    for file in &manifest.files {
        let file_path = canonical_source.join(&file.name);
        let file_metadata = match std::fs::symlink_metadata(&file_path) {
            Ok(metadata) => metadata,
            Err(error) => {
                return Ok(rejected_registered_model2vec(
                    entry,
                    source_io_rejection(&error),
                ));
            }
        };
        if file_metadata.file_type().is_symlink() {
            return Ok(rejected_registered_model2vec(
                entry,
                EmbedRegistryRejectionReason::SourceSymlink,
            ));
        }
        if let Err(error) = std::fs::File::open(&file_path) {
            return Ok(rejected_registered_model2vec(
                entry,
                source_io_rejection(&error),
            ));
        }
    }
    if verify_dir_cached(&manifest, &canonical_source).is_err() {
        return Ok(rejected_registered_model2vec(
            entry,
            EmbedRegistryRejectionReason::ManifestVerificationFailed,
        ));
    }
    let Some(content_hash) = entry
        .content_hash
        .as_deref()
        .map(str::trim)
        .filter(|hash| !hash.is_empty())
        .map(str::to_ascii_lowercase)
    else {
        return Ok(rejected_registered_model2vec(
            entry,
            EmbedRegistryRejectionReason::ContentHashMissing,
        ));
    };
    if !model_registry_hash_is_well_formed(&content_hash) {
        return Ok(rejected_registered_model2vec(
            entry,
            EmbedRegistryRejectionReason::ContentHashMalformed,
        ));
    }
    let Some(dimension) = entry.dimension else {
        return Ok(rejected_registered_model2vec(
            entry,
            EmbedRegistryRejectionReason::DimensionMissing,
        ));
    };
    let Some(distance_metric) = entry.distance_metric else {
        return Ok(rejected_registered_model2vec(
            entry,
            EmbedRegistryRejectionReason::DistanceMetricMissing,
        ));
    };
    if distance_metric != ModelDistanceMetric::Cosine {
        return Ok(rejected_registered_model2vec(
            entry,
            EmbedRegistryRejectionReason::DistanceMetricUnsupported,
        ));
    }
    let Some(metadata_json) = entry.metadata_json.as_deref() else {
        return Ok(rejected_registered_model2vec(
            entry,
            EmbedRegistryRejectionReason::MetadataMissing,
        ));
    };
    let metadata = match EmbeddingMetadataRecord::from_json(metadata_json) {
        Ok(metadata) => metadata,
        Err(_) => {
            return Ok(rejected_registered_model2vec(
                entry,
                EmbedRegistryRejectionReason::MetadataMalformed,
            ));
        }
    };
    if metadata.dimension != dimension || metadata.distance_metric != distance_metric {
        return Ok(rejected_registered_model2vec(
            entry,
            EmbedRegistryRejectionReason::MetadataMismatch,
        ));
    }
    let identity = RegisteredModel2VecIdentity {
        canonical_source: canonical_source.clone(),
        content_hash: content_hash.clone(),
        dimension,
        distance_metric: distance_metric.as_str(),
    };
    let fast = REGISTERED_MODEL2VEC_CACHE
        .get_or_init(RegisteredModel2VecCache::default)
        .get_or_try_insert_with(identity, || {
            let Ok(embedder) =
                Model2VecEmbedder::load_with_name(&canonical_source, POTION_MODEL_NAME)
            else {
                return None;
            };
            let fingerprint = active_embedder_fingerprint(&embedder, ModelProvider::Model2Vec);
            let hash_matches = content_hash.eq_ignore_ascii_case(&fingerprint.content_hash);
            let dimension_matches = Some(dimension) == u32::try_from(embedder.dimension()).ok();
            if !hash_matches || !dimension_matches {
                return None;
            }
            Some(Arc::new(embedder) as Arc<dyn crate::search::Embedder>)
        });
    let Some(fast) = fast else {
        let reason = match Model2VecEmbedder::load_with_name(&canonical_source, POTION_MODEL_NAME) {
            Ok(embedder) => {
                let fingerprint = active_embedder_fingerprint(&embedder, ModelProvider::Model2Vec);
                if !content_hash.eq_ignore_ascii_case(&fingerprint.content_hash) {
                    EmbedRegistryRejectionReason::ContentHashMismatch
                } else if Some(dimension) != u32::try_from(embedder.dimension()).ok() {
                    EmbedRegistryRejectionReason::DimensionMismatch
                } else {
                    EmbedRegistryRejectionReason::ModelLoadFailed
                }
            }
            Err(_) => EmbedRegistryRejectionReason::ModelLoadFailed,
        };
        return Ok(rejected_registered_model2vec(entry, reason));
    };
    Ok(RegisteredModel2VecResolution::Ready(
        stack_with_hash_quality_fallback(EmbedderStack::from_parts(fast, None)),
    ))
}

fn workspace_embedder_stack(
    db: &DbConnection,
    workspace_id: &str,
) -> Result<EmbedderStack, DbError> {
    if configured_embedder_model_root().is_none()
        && let Some(selection) = workspace_registry_embedder_selection(db, workspace_id)?
    {
        return Ok(selection.stack);
    }
    Ok(default_embedder_stack())
}

fn verified_potion_model_dir(model_dir: &Path) -> bool {
    verify_dir_cached(&ModelManifest::potion_128m(), model_dir).is_ok()
}

/// Stable, deliberately small backend vocabulary shared by search, pack, and
/// orient. This reports only a backend that has executed in the current
/// process; local model availability alone is not execution evidence.
#[must_use]
pub(crate) fn active_embed_backend() -> EmbedBackend {
    if let Some(selection) = DEFAULT_SEARCH_EMBEDDER.get() {
        return if selection.stack.fast().is_semantic() {
            EmbedBackend::NeuralLocal
        } else {
            EmbedBackend::HashFallback
        };
    }

    // No process-default embedder has executed yet. Reporting a locally
    // discoverable neural model here would describe availability, not the
    // backend that served this response. This distinction is load-bearing for
    // `ee orient --fast`, whose explicit strategy is lexical-only.
    EmbedBackend::HashFallback
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

fn model_initialization_checkpoint(cx: &asupersync::Cx, phase: &str) -> Result<(), SearchError> {
    cx.checkpoint().map_err(|_| SearchError::Cancelled {
        phase: phase.to_owned(),
        reason: cx.cancel_reason().map_or_else(
            || "model initialization cancelled without a recorded reason".to_owned(),
            |reason| reason.to_string(),
        ),
    })
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
        model_initialization_checkpoint(cx, "before local model load")?;
        let destination = potion_model_destination_dir(&self.model_root);
        if let Ok(embedder) = Model2VecEmbedder::load_with_name(&destination, POTION_MODEL_NAME) {
            model_initialization_checkpoint(cx, "after local model load")?;
            return Ok(Arc::new(embedder) as Arc<dyn crate::search::Embedder>);
        }
        model_initialization_checkpoint(cx, "before model download")?;

        let manifest = ModelManifest::potion_128m();
        emit_embedding_download_notice(manifest.total_size_bytes());
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
        model_initialization_checkpoint(cx, "before downloaded model load")?;
        let embedder = Model2VecEmbedder::load_with_name(&destination, POTION_MODEL_NAME)?;
        model_initialization_checkpoint(cx, "after downloaded model load")?;
        Ok(Arc::new(embedder) as Arc<dyn crate::search::Embedder>)
    }

    fn mark_failed(&self) {
        self.state
            .store(EE_DOWNLOAD_STATE_FAILED, Ordering::Release);
    }

    fn record_load_failure(&self, error: &SearchError) -> bool {
        if matches!(error, SearchError::Cancelled { .. }) {
            return false;
        }
        self.mark_failed();
        true
    }

    fn failed(&self) -> bool {
        self.state.load(Ordering::Acquire) == EE_DOWNLOAD_STATE_FAILED
    }
}

fn executed_model_resolution(
    selected: &EmbedModelResolution,
    backend: EmbedBackend,
) -> EmbedModelResolution {
    if backend == EmbedBackend::NeuralLocal {
        EmbedModelResolution::ready(selected.source)
    } else if selected.source == EmbedModelSource::RegistryRejected {
        selected.clone()
    } else {
        EmbedModelResolution::deterministic_hash()
    }
}

/// Resolve and initialize the process-default semantic embedder before a
/// caller acquires a database read snapshot.
///
/// A verified local Model2Vec artifact can take long enough to load that doing
/// so inside a pinned FrankenSQLite snapshot trips the read-pool lifecycle
/// watchdog. First-use download is even slower. Keeping both operations behind
/// this caller-`Cx` seam lets search and pack account for the real cold-start
/// cost while guaranteeing that no snapshot lease is held during model I/O.
pub(crate) async fn prepare_default_search_embedder(
    cx: &asupersync::Cx,
) -> Result<EmbedderPreparation, SearchError> {
    model_initialization_checkpoint(cx, "before default embedder preparation")?;
    let started = Instant::now();
    let selection = DEFAULT_SEARCH_EMBEDDER.get_or_init(detect_default_search_embedder);
    model_initialization_checkpoint(cx, "after default embedder selection")?;

    if let Some(lazy_model2vec) = selection.lazy_model2vec.as_ref()
        && !lazy_model2vec.failed()
        && !lazy_model2vec.is_ready()
        && let Err(error) = lazy_model2vec.try_load(cx).await
    {
        if !lazy_model2vec.record_load_failure(&error) {
            return Err(error);
        }
        tracing::warn!(
            target: "ee::index::embedder",
            error = %error,
            model = POTION_MODEL_NAME,
            "ee-managed embedding model preparation failed; using deterministic hash fallback for this process"
        );
    }

    model_initialization_checkpoint(cx, "after default embedder preparation")?;
    let backend = if selection.stack.fast().is_semantic() {
        EmbedBackend::NeuralLocal
    } else {
        EmbedBackend::HashFallback
    };
    let model_resolution = executed_model_resolution(&selection.model_resolution, backend);
    Ok(EmbedderPreparation::new(
        backend,
        model_resolution,
        started.elapsed(),
        selection.stack.fast_arc(),
    ))
}

pub(crate) async fn prepare_search_embedder_for_workspace(
    cx: &asupersync::Cx,
    workspace_path: &Path,
    database_path: &Path,
) -> Result<EmbedderPreparation, SearchError> {
    let started = Instant::now();
    if configured_embedder_model_root().is_none() && database_path.exists() {
        let db = DbConnection::open_file_read_only(database_path).map_err(|error| {
            SearchError::SubsystemError {
                subsystem: "model registry",
                source: Box::new(error),
            }
        })?;
        if let Some(workspace_id) =
            workspace_id_for_index_status(&db, workspace_path).map_err(|error| {
                SearchError::SubsystemError {
                    subsystem: "model registry",
                    source: Box::new(error),
                }
            })?
        {
            if let Some(selection) = workspace_registry_embedder_selection(&db, &workspace_id)
                .map_err(|error| SearchError::SubsystemError {
                    subsystem: "model registry",
                    source: Box::new(error),
                })?
            {
                let backend = if selection.stack.fast().is_semantic() {
                    EmbedBackend::NeuralLocal
                } else {
                    EmbedBackend::HashFallback
                };
                return Ok(EmbedderPreparation::new(
                    backend,
                    selection.model_resolution,
                    started.elapsed(),
                    selection.stack.fast_arc(),
                ));
            }
        }
    }

    let mut preparation = prepare_default_search_embedder(cx).await?;
    preparation.elapsed = started.elapsed();
    Ok(preparation)
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
                    if !self.record_load_failure(&error) {
                        return Err(error);
                    }
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
                    if !self.record_load_failure(&error) {
                        return Err(error);
                    }
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

fn emit_embedding_download_notice(bytes: u64) {
    eprintln!(
        "ee is downloading the local embedding model {POTION_MODEL_NAME} ({}) once into the private ee model registry. Set EE_EMBED_DOWNLOAD=off to prohibit network downloads; verified registered models remain usable.",
        format_bytes(bytes)
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
    let source_dir = active_embedding_model_source_dir(db, workspace_id, stack.fast())?;
    ensure_loaded_embedding_registry_record(db, workspace_id, stack.fast(), source_dir.as_deref())
}

pub(crate) fn ensure_loaded_embedding_registry_record(
    db: &DbConnection,
    workspace_id: &str,
    fast_embedder: &dyn crate::search::Embedder,
    source_dir: Option<&Path>,
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

    let Some(input) =
        active_embedding_registry_input_with_source(workspace_id, fast_embedder, source_dir)?
    else {
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

fn active_embedding_model_source_dir(
    db: &DbConnection,
    workspace_id: &str,
    fast_embedder: &dyn crate::search::Embedder,
) -> Result<Option<PathBuf>, DbError> {
    if provider_for_embedder(fast_embedder) != ModelProvider::Model2Vec
        || fast_embedder.id() != POTION_MODEL_NAME
    {
        return Ok(None);
    }

    if let Some(model_root) = configured_embedder_model_root()
        && let Some(canonical) =
            canonical_verified_potion_model_dir(&potion_model_destination_dir(&model_root))
    {
        return Ok(Some(canonical));
    }

    if let Some(existing) = db.find_model_registry_entry(
        workspace_id,
        ModelProvider::Model2Vec,
        POTION_MODEL_NAME,
        ModelPurpose::Embedding,
    )? && let Ok(existing_path) = registered_model2vec_source_path(db, &existing)
        && let Some(canonical) = canonical_verified_potion_model_dir(&existing_path)
    {
        return Ok(Some(canonical));
    }

    Ok(canonical_verified_potion_model_dir(
        &potion_model_destination_dir(&default_embedder_model_root()),
    ))
}

fn canonical_verified_potion_model_dir(model_dir: &Path) -> Option<PathBuf> {
    let canonical = std::fs::canonicalize(model_dir).ok()?;
    verified_potion_model_dir(&canonical).then_some(canonical)
}

#[cfg(test)]
fn active_embedding_registry_input(
    workspace_id: &str,
    fast_embedder: &dyn crate::search::Embedder,
) -> Result<Option<crate::db::CreateEmbeddingMetadataInput>, IndexRebuildError> {
    active_embedding_registry_input_with_source(workspace_id, fast_embedder, None)
}

fn active_embedding_registry_input_with_source(
    workspace_id: &str,
    fast_embedder: &dyn crate::search::Embedder,
    source_dir: Option<&Path>,
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
    let source_uri = if provider == ModelProvider::Model2Vec
        && fast_embedder.id() == POTION_MODEL_NAME
        && let Some(source_dir) = source_dir
    {
        let canonical = std::fs::canonicalize(source_dir).map_err(|error| {
            IndexRebuildError::Index(format!(
                "failed to canonicalize loaded Model2Vec directory {}: {error}",
                source_dir.display()
            ))
        })?;
        if !verified_potion_model_dir(&canonical) {
            return Err(IndexRebuildError::Index(format!(
                "loaded Model2Vec directory {} failed pinned manifest verification",
                canonical.display()
            )));
        }
        Some(canonical.to_string_lossy().into_owned())
    } else {
        Some(format!(
            "frankensearch://{provider}/{model}",
            provider = provider.as_str(),
            model = fast_embedder.id()
        ))
    };

    Ok(Some(crate::db::CreateEmbeddingMetadataInput {
        workspace_id: workspace_id.to_owned(),
        provider,
        model_name: fast_embedder.id().to_owned(),
        dimension,
        distance_metric: ModelDistanceMetric::Cosine,
        status: ModelRegistryStatus::Available,
        version: metadata.model_revision.clone(),
        source_uri,
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
    let documents_total = current_indexable_document_count(db, workspace_id)?;
    embedding_posture_for_document_count(db, workspace_id, index_dir, documents_total)
}

fn embedding_posture_for_document_count(
    db: &DbConnection,
    workspace_id: &str,
    index_dir: &Path,
    documents_total: u32,
) -> Result<EmbeddingPosture, DbError> {
    let stack = default_search_embedder_stack();
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

/// GH#19: recover the vector dimension and embedder id recorded in the
/// on-disk fast vector tier (FSVI header).
///
/// Indexes published before the metadata writer stamped the embedder
/// fingerprint have a bare `meta.json` with no `storedDimension`, which left
/// semantic readiness permanently stuck at `unknown`. The FSVI header has
/// carried the true dimension and embedder id all along, so readers can
/// backfill the missing evidence without forcing a rebuild.
pub(crate) fn read_fast_vector_index_fingerprint(index_dir: &Path) -> Option<(u32, String)> {
    let index = open_fast_vector_index(index_dir).ok()?;
    let dimension = u32::try_from(index.dimension()).ok()?;
    Some((dimension, index.embedder_id().to_owned()))
}

fn current_indexable_document_count(db: &DbConnection, workspace_id: &str) -> Result<u32, DbError> {
    current_index_corpus_counts(db, workspace_id).map(|(counts, _)| counts.total())
}

fn current_index_corpus_counts(
    db: &DbConnection,
    workspace_id: &str,
) -> Result<(IndexDocumentCounts, EvidenceAdmissionReport), DbError> {
    let memories = db.list_memories_for_retrieval_with_global(workspace_id, None, false)?;
    let mut indexable_memory_count = 0_usize;
    for memory in &memories {
        if !memory_has_seal_sidecar(db, memory)? {
            indexable_memory_count = indexable_memory_count.saturating_add(1);
        }
    }
    let sessions = db.count_sessions_for_workspace(workspace_id)?;
    let artifacts = db.list_artifacts(workspace_id, None)?;
    let rules = db
        .list_procedural_rules(workspace_id, None, None, false)?
        .into_iter()
        .filter(|rule| {
            rule.tombstoned_at.is_none()
                && rule.superseded_by.is_none()
                && rule.maturity != crate::models::RuleMaturity::Superseded.as_str()
        })
        .count();
    let mut evidence = 0_usize;
    let evidence_scan =
        db.visit_search_admitted_evidence_spans_for_workspace(workspace_id, |_| {
            evidence = evidence.saturating_add(1);
            Ok(())
        })?;
    let to_u32 = |label: &str, count: usize| {
        u32::try_from(count).map_err(|_| DbError::MalformedRow {
            operation: DbOperation::Query,
            message: format!("{label} indexable document count {count} exceeds u32"),
        })
    };
    let counts = IndexDocumentCounts::checked(
        to_u32("memory", indexable_memory_count)?,
        sessions,
        to_u32("artifact", artifacts.len())?,
        to_u32("rule", rules)?,
        to_u32("evidence", evidence)?,
    )
    .map_err(|message| DbError::MalformedRow {
        operation: DbOperation::Query,
        message,
    })?;
    Ok((counts, evidence_scan.admission))
}

fn reembed_idempotency_key(
    workspace_id: &str,
    fast_model_id: &str,
    quality_model_id: Option<&str>,
    document_counts: IndexDocumentCounts,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.index_reembed.v1\0");
    hasher.update(workspace_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(fast_model_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(quality_model_id.unwrap_or("").as_bytes());
    hasher.update(b"\0");
    hasher.update(expected_index_corpus_revision().as_str().as_bytes());
    for count in [
        document_counts.memories,
        document_counts.sessions,
        document_counts.artifacts,
        document_counts.rules,
        document_counts.evidence,
    ] {
        hasher.update(b"\0");
        hasher.update(count.to_string().as_bytes());
    }
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
    pub db_artifact_count: u32,
    pub db_rule_count: u32,
    pub db_evidence_count: u32,
    pub db_evidence_admitted_count: u32,
    pub db_evidence_quarantined_count: u32,
    pub db_evidence_denied_count: u32,
    pub db_generation: Option<u64>,
    pub index_generation: Option<u64>,
    pub expected_corpus_revision: String,
    pub actual_corpus_revision: Option<String>,
    pub index_document_count: Option<u32>,
    pub index_document_counts: Option<IndexDocumentCounts>,
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
        output.push_str(&format!("  DB artifacts: {}\n", self.db_artifact_count));
        output.push_str(&format!("  DB rules: {}\n", self.db_rule_count));
        output.push_str(&format!("  DB evidence: {}\n", self.db_evidence_count));
        output.push_str(&format!(
            "  Evidence admitted/quarantined/denied: {}/{}/{}\n",
            self.db_evidence_admitted_count,
            self.db_evidence_quarantined_count,
            self.db_evidence_denied_count
        ));
        output.push_str(&format!(
            "  Expected corpus revision: {}\n",
            self.expected_corpus_revision
        ));
        output.push_str(&format!(
            "  Actual corpus revision: {}\n",
            self.actual_corpus_revision
                .as_deref()
                .unwrap_or("<missing>")
        ));

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
            "dbArtifactCount": self.db_artifact_count,
            "dbRuleCount": self.db_rule_count,
            "dbEvidenceCount": self.db_evidence_count,
            "dbEvidenceAdmittedCount": self.db_evidence_admitted_count,
            "dbEvidenceQuarantinedCount": self.db_evidence_quarantined_count,
            "dbEvidenceDeniedCount": self.db_evidence_denied_count,
            "dbGeneration": self.db_generation,
            "indexGeneration": self.index_generation,
            "expectedCorpusRevision": self.expected_corpus_revision,
            "actualCorpusRevision": self.actual_corpus_revision,
            "indexDocumentCount": self.index_document_count,
            "indexDocumentCounts": self.index_document_counts.map(IndexDocumentCounts::data_json),
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

    let (db_document_counts, evidence_admission, db_generation, embedding) = if database_path
        .exists()
    {
        let owned_connection;
        let db = if let Some(connection) = connection {
            connection
        } else {
            owned_connection = DbConnection::open_file(&database_path)?;
            &owned_connection
        };
        if let Some(workspace_id) = workspace_id_for_index_status(db, &options.workspace_path)? {
            let (counts, admission, generation) = get_db_stats(db, &workspace_id)?;
            let embedding = Some(embedding_posture_for_document_count(
                db,
                &workspace_id,
                &index_dir,
                counts.total(),
            )?);
            (counts, admission, generation, embedding)
        } else {
            (
                IndexDocumentCounts::default(),
                EvidenceAdmissionReport::default(),
                None,
                None,
            )
        }
    } else {
        (
            IndexDocumentCounts::default(),
            EvidenceAdmissionReport::default(),
            None,
            None,
        )
    };
    let evidence_totals = EvidenceAdmissionTotals::from_report(&evidence_admission);

    // Read index metadata if available.
    let metadata_status = read_index_metadata(&index_dir);
    let last_check_error = metadata_status
        .corruption_error
        .clone()
        .or_else(|| metadata_status.compatibility_error.clone());

    // Determine health
    let health = determine_health(
        index_exists,
        index_file_count,
        db_generation,
        metadata_status.generation,
        metadata_status.present,
        metadata_status.corruption_error.is_some(),
        metadata_status.compatibility_error.is_some(),
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
        db_memory_count: db_document_counts.memories,
        db_session_count: db_document_counts.sessions,
        db_artifact_count: db_document_counts.artifacts,
        db_rule_count: db_document_counts.rules,
        db_evidence_count: evidence_totals.total(),
        db_evidence_admitted_count: evidence_totals.admitted,
        db_evidence_quarantined_count: evidence_totals.quarantined,
        db_evidence_denied_count: evidence_totals.denied,
        db_generation,
        index_generation: metadata_status.generation,
        expected_corpus_revision: expected_index_corpus_revision().to_string(),
        actual_corpus_revision: metadata_status.corpus_revision,
        index_document_count: metadata_status.document_count,
        index_document_counts: metadata_status.document_counts,
        last_rebuild_at: metadata_status.last_rebuild_at,
        last_check_error,
        repair_hint,
        elapsed_ms,
    };
    log_db_generation_observed(&report);
    Ok(report)
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
    let rejected_prefix = format!(".{base}{INDEX_REJECTED_PREFIX}");
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
        } else if name.starts_with(&rejected_prefix) {
            IndexVacuumCandidateKind::StagedGeneration
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

fn get_db_stats(
    db: &DbConnection,
    workspace_id: &str,
) -> Result<(IndexDocumentCounts, EvidenceAdmissionReport, Option<u64>), DbError> {
    let (counts, evidence_admission) = current_index_corpus_counts(db, workspace_id)?;
    let generation = db
        .get_workspace_generation(workspace_id)?
        .or(Some(u64::from(counts.total())));
    Ok((counts, evidence_admission, generation))
}

#[derive(Clone, Debug, Default)]
struct IndexMetadataStatus {
    present: bool,
    generation: Option<u64>,
    last_rebuild_at: Option<String>,
    corpus_revision: Option<String>,
    document_count: Option<u32>,
    document_counts: Option<IndexDocumentCounts>,
    compatibility_error: Option<String>,
    corruption_error: Option<String>,
}

fn read_index_metadata(index_dir: &Path) -> IndexMetadataStatus {
    let meta_path = index_dir.join(INDEX_METADATA_FILE);
    match parse_index_metadata(index_dir) {
        Ok(Some(metadata)) => {
            let compatibility_error =
                index_metadata_compatibility_error(&meta_path, &metadata).or_else(|| {
                    let document_count = metadata.document_count?;
                    let expect_quality_tier = metadata
                        .tier_document_counts
                        .is_some_and(|counts| counts.quality.is_some());
                    verify_published_tier_counts(index_dir, document_count, expect_quality_tier)
                        .err()
                        .map(|error| {
                            format!(
                                "index generation '{}' failed persisted-tier verification: {error}; a full index rebuild is required",
                                index_dir.display()
                            )
                        })
                });
            IndexMetadataStatus {
                present: true,
                generation: metadata.generation,
                last_rebuild_at: metadata.last_rebuild_at.clone(),
                corpus_revision: metadata.corpus_revision.clone(),
                document_count: metadata.document_count,
                document_counts: metadata.document_counts,
                compatibility_error,
                corruption_error: None,
            }
        }
        Ok(None) => IndexMetadataStatus::default(),
        Err(error) => IndexMetadataStatus {
            corruption_error: Some(error),
            ..IndexMetadataStatus::default()
        },
    }
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
    metadata_present: bool,
    metadata_corrupt: bool,
    metadata_incompatible: bool,
) -> IndexHealth {
    if metadata_corrupt {
        return IndexHealth::Corrupt;
    }

    if !index_exists || index_file_count == 0 {
        return IndexHealth::Missing;
    }

    if !metadata_present || metadata_incompatible {
        return IndexHealth::Stale;
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

    #[derive(Debug)]
    struct BackendCancellingEmbedder;

    impl crate::search::Embedder for BackendCancellingEmbedder {
        fn embed<'a>(
            &'a self,
            _cx: &'a asupersync::Cx,
            _text: &'a str,
        ) -> frankensearch::SearchFuture<'a, Vec<f32>> {
            Box::pin(async {
                Err(SearchError::Cancelled {
                    phase: "fast vector embed".to_owned(),
                    reason: "poll quota: backend embedding budget exhausted".to_owned(),
                })
            })
        }

        fn dimension(&self) -> usize {
            256
        }

        fn id(&self) -> &str {
            "cancel-on-embed-test"
        }

        fn model_name(&self) -> &str {
            "cancel-on-embed-test"
        }

        fn is_semantic(&self) -> bool {
            false
        }

        fn category(&self) -> ModelCategory {
            ModelCategory::HashEmbedder
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

    fn rejected_generation_dirs(root: &Path, index_name: &str) -> Result<Vec<PathBuf>, String> {
        let prefix = format!(".{index_name}{INDEX_REJECTED_PREFIX}");
        let mut paths = std::fs::read_dir(root)
            .map_err(|error| error.to_string())?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        paths.sort();
        Ok(paths)
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
    fn registry_rejection_preparation_is_explicit_hash_truth() -> TestResult {
        let stack = hash_fallback_embedder_stack();
        let rejected = EmbedModelResolution::registry_rejected(
            "mdl_rejected_fixture",
            EmbedRegistryRejectionReason::SourceSymlink,
        );
        let executed = executed_model_resolution(&rejected, EmbedBackend::HashFallback);
        ensure(
            executed == rejected,
            "executed hash resolution must retain the exact registry rejection",
        )?;
        let preparation = EmbedderPreparation::new(
            EmbedBackend::HashFallback,
            executed,
            Duration::ZERO,
            stack.fast_arc(),
        );
        ensure(
            preparation.backend == EmbedBackend::HashFallback,
            "rejected registration must execute the hash backend",
        )?;
        ensure(
            preparation.model_resolution.source == EmbedModelSource::RegistryRejected,
            "rejected registration must preserve registry_rejected source truth",
        )?;
        ensure(
            preparation.fast_embedder.id() == HashEmbedder::default_256().id(),
            "rejected registration must serve the real deterministic hash embedder",
        )
    }

    #[test]
    fn workspace_registry_selection_is_order_independent() -> TestResult {
        fn rejected(workspace: &str) -> RegisteredModel2VecResolution {
            RegisteredModel2VecResolution::Rejected(EmbedModelResolution::registry_rejected(
                format!("mdl_{workspace}_rejected"),
                EmbedRegistryRejectionReason::MetadataMalformed,
            ))
        }

        fn ready(workspace: &str) -> RegisteredModel2VecResolution {
            RegisteredModel2VecResolution::Ready(EmbedderStack::from_parts(
                Arc::new(TestSemanticEmbedder::new(
                    &format!("{POTION_MODEL_NAME}-{workspace}"),
                    usize::try_from(BUNDLED_EMBEDDING_DIMENSION)
                        .expect("bundled dimension fits usize"),
                )) as Arc<dyn crate::search::Embedder>,
                None,
            ))
        }

        let rejected_then_ready = [
            workspace_registry_selection_from_resolution(rejected("a")),
            workspace_registry_selection_from_resolution(ready("b")),
        ];
        let ready_then_rejected = [
            workspace_registry_selection_from_resolution(ready("b")),
            workspace_registry_selection_from_resolution(rejected("a")),
        ];

        for rejected_selection in [
            rejected_then_ready[0].as_ref(),
            ready_then_rejected[1].as_ref(),
        ] {
            let selection = rejected_selection
                .ok_or_else(|| "rejected workspace must have an explicit selection".to_owned())?;
            ensure(
                !selection.stack.fast().is_semantic(),
                "rejected workspace must remain hash-backed in either order",
            )?;
            ensure(
                selection.model_resolution.source == EmbedModelSource::RegistryRejected,
                "rejected workspace must retain its own registry rejection",
            )?;
        }
        for ready_selection in [
            rejected_then_ready[1].as_ref(),
            ready_then_rejected[0].as_ref(),
        ] {
            let selection = ready_selection
                .ok_or_else(|| "valid workspace must have an explicit selection".to_owned())?;
            ensure(
                selection.stack.fast().is_semantic(),
                "valid workspace must remain neural in either order",
            )?;
            ensure(
                selection.model_resolution
                    == EmbedModelResolution::ready(EmbedModelSource::Registered),
                "valid workspace must retain registered-model truth",
            )?;
        }

        ensure(
            workspace_registry_selection_from_resolution(
                RegisteredModel2VecResolution::NotRegistered,
            )
            .is_none(),
            "an unregistered workspace must defer to the independent process default",
        )
    }

    #[test]
    fn registry_hash_validation_rejects_malformed_values() {
        assert!(model_registry_hash_is_well_formed(&format!(
            "blake3:{}",
            "a".repeat(64)
        )));
        assert!(!model_registry_hash_is_well_formed(""));
        assert!(!model_registry_hash_is_well_formed("blake3:not-a-digest"));
        assert!(!model_registry_hash_is_well_formed(&format!(
            "sha256:{}",
            "a".repeat(64)
        )));
        assert!(!model_registry_hash_is_well_formed(&format!(
            "blake3:{}g",
            "a".repeat(63)
        )));
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

    fn registered_model2vec_test_identity(content_hash: &str) -> RegisteredModel2VecIdentity {
        RegisteredModel2VecIdentity {
            canonical_source: PathBuf::from("/verified/models/potion-multilingual-128M"),
            content_hash: content_hash.to_owned(),
            dimension: BUNDLED_EMBEDDING_DIMENSION,
            distance_metric: ModelDistanceMetric::Cosine.as_str(),
        }
    }

    fn registered_model2vec_test_embedder(name: &str) -> Arc<dyn crate::search::Embedder> {
        Arc::new(TestSemanticEmbedder::new(
            name,
            usize::try_from(BUNDLED_EMBEDDING_DIMENSION).expect("bundled dimension fits usize"),
        ))
    }

    #[test]
    fn registered_model2vec_cache_reuses_arc_for_exact_identity() -> TestResult {
        let cache = RegisteredModel2VecCache::default();
        let identity = registered_model2vec_test_identity("blake3:identity-a");
        let loads = std::cell::Cell::new(0_u32);
        let first = cache
            .get_or_try_insert_with(identity.clone(), || {
                loads.set(loads.get().saturating_add(1));
                Some(Arc::new(TestSemanticEmbedder::new(
                    POTION_MODEL_NAME,
                    usize::try_from(BUNDLED_EMBEDDING_DIMENSION)
                        .expect("bundled dimension fits usize"),
                )) as Arc<dyn crate::search::Embedder>)
            })
            .ok_or_else(|| "first registered embedder load must succeed".to_owned())?;
        let second = cache
            .get_or_try_insert_with(identity, || {
                loads.set(loads.get().saturating_add(1));
                Some(Arc::new(TestSemanticEmbedder::new(
                    "must-not-load",
                    usize::try_from(BUNDLED_EMBEDDING_DIMENSION)
                        .expect("bundled dimension fits usize"),
                )) as Arc<dyn crate::search::Embedder>)
            })
            .ok_or_else(|| "cached registered embedder lookup must succeed".to_owned())?;

        ensure(loads.get() == 1, "exact identity must load only once")?;
        ensure(
            Arc::ptr_eq(&first, &second),
            "exact registry identity must reuse the same embedder Arc",
        )
    }

    #[test]
    fn registered_model2vec_cache_keys_every_immutable_identity_field() -> TestResult {
        let cache = RegisteredModel2VecCache::default();
        let base = registered_model2vec_test_identity("blake3:identity-a");
        let mut changed_source = base.clone();
        changed_source.canonical_source = PathBuf::from("/verified/models/alternate-potion");
        let mut changed_hash = base.clone();
        changed_hash.content_hash = "blake3:identity-b".to_owned();
        let mut changed_dimension = base.clone();
        changed_dimension.dimension = changed_dimension.dimension.saturating_add(1);
        let mut changed_metric = base.clone();
        changed_metric.distance_metric = ModelDistanceMetric::Dot.as_str();
        let loads = std::cell::Cell::new(0_u32);
        let mut selected = Vec::new();
        for identity in [
            base,
            changed_source,
            changed_hash,
            changed_dimension,
            changed_metric,
        ] {
            let ordinal = loads.get().saturating_add(1);
            let embedder = cache
                .get_or_try_insert_with(identity, || {
                    loads.set(ordinal);
                    Some(Arc::new(TestSemanticEmbedder::new(
                        &format!("registry-identity-{ordinal}"),
                        usize::try_from(BUNDLED_EMBEDDING_DIMENSION)
                            .expect("bundled dimension fits usize"),
                    )) as Arc<dyn crate::search::Embedder>)
                })
                .ok_or_else(|| format!("identity variant {ordinal} must load"))?;
            selected.push(embedder);
        }
        ensure(
            loads.get() == 5,
            "source, hash, dimension, and metric changes must each select a new embedder",
        )?;
        for (left_index, left) in selected.iter().enumerate() {
            for right in &selected[(left_index + 1)..] {
                ensure(
                    !Arc::ptr_eq(left, right),
                    "distinct immutable identities must not share an embedder Arc",
                )?;
            }
        }
        Ok(())
    }

    #[test]
    fn registered_model2vec_cache_replacement_evicts_old_arc() -> TestResult {
        let cache = RegisteredModel2VecCache::default();
        let first_identity = registered_model2vec_test_identity("blake3:identity-a");
        let changed_identity = registered_model2vec_test_identity("blake3:identity-b");
        let loads = std::cell::Cell::new(0_u32);
        let first = cache
            .get_or_try_insert_with(first_identity, || {
                loads.set(loads.get().saturating_add(1));
                Some(registered_model2vec_test_embedder("registry-identity-a"))
            })
            .ok_or_else(|| "first identity load must succeed".to_owned())?;
        let evicted = Arc::downgrade(&first);
        drop(first);
        ensure(
            evicted.upgrade().is_some(),
            "cache must retain the current identity before replacement",
        )?;

        let changed = cache
            .get_or_try_insert_with(changed_identity, || {
                loads.set(loads.get().saturating_add(1));
                Some(registered_model2vec_test_embedder("registry-identity-b"))
            })
            .ok_or_else(|| "changed identity load must succeed".to_owned())?;
        ensure(
            loads.get() == 2,
            "a successful identity change must load exactly one replacement",
        )?;
        ensure(
            evicted.upgrade().is_none(),
            "successful replacement must drop the cache's strong reference to the old embedder",
        )?;
        let current = Arc::downgrade(&changed);
        drop(changed);
        ensure(
            current.upgrade().is_some(),
            "cache must retain exactly the successful replacement",
        )
    }

    #[test]
    fn registered_model2vec_cache_failed_replacement_preserves_current_and_retries() -> TestResult {
        let cache = RegisteredModel2VecCache::default();
        let healthy_identity = registered_model2vec_test_identity("blake3:identity-a");
        let failed_identity = registered_model2vec_test_identity("blake3:identity-failed");
        let loads = std::cell::Cell::new(0_u32);
        let healthy = cache
            .get_or_try_insert_with(healthy_identity.clone(), || {
                loads.set(loads.get().saturating_add(1));
                Some(registered_model2vec_test_embedder("registry-identity-a"))
            })
            .ok_or_else(|| "healthy identity load must succeed".to_owned())?;
        let healthy_weak = Arc::downgrade(&healthy);
        drop(healthy);

        ensure(
            cache
                .get_or_try_insert_with(failed_identity.clone(), || {
                    loads.set(loads.get().saturating_add(1));
                    None
                })
                .is_none(),
            "failed replacement must remain a miss",
        )?;
        ensure(
            loads.get() == 2,
            "failed replacement must run its loader once",
        )?;
        ensure(
            healthy_weak.upgrade().is_some(),
            "failed replacement must preserve the healthy cached embedder",
        )?;

        let preserved = cache
            .get_or_try_insert_with(healthy_identity, || {
                loads.set(loads.get().saturating_add(1));
                Some(registered_model2vec_test_embedder("must-not-reload"))
            })
            .ok_or_else(|| {
                "healthy identity must remain cached after failed replacement".to_owned()
            })?;
        let preserved_from_weak = healthy_weak
            .upgrade()
            .ok_or_else(|| "preserved healthy embedder must remain live".to_owned())?;
        ensure(
            Arc::ptr_eq(&preserved, &preserved_from_weak),
            "failed replacement must leave the exact healthy Arc current",
        )?;
        ensure(
            loads.get() == 2,
            "re-reading the preserved identity must not invoke its loader",
        )?;
        drop(preserved);
        drop(preserved_from_weak);

        cache
            .get_or_try_insert_with(failed_identity, || {
                loads.set(loads.get().saturating_add(1));
                Some(registered_model2vec_test_embedder("registry-retry"))
            })
            .ok_or_else(|| "failed identity must remain retryable".to_owned())?;
        ensure(loads.get() == 3, "failed identity must retry its loader")?;
        ensure(
            healthy_weak.upgrade().is_none(),
            "successful retry must evict the formerly preserved healthy embedder",
        )
    }

    #[test]
    fn registered_model2vec_cache_switch_back_reloads_evicted_identity() -> TestResult {
        let cache = RegisteredModel2VecCache::default();
        let first_identity = registered_model2vec_test_identity("blake3:identity-a");
        let second_identity = registered_model2vec_test_identity("blake3:identity-b");
        let loads = std::cell::Cell::new(0_u32);
        let first = cache
            .get_or_try_insert_with(first_identity.clone(), || {
                loads.set(loads.get().saturating_add(1));
                Some(registered_model2vec_test_embedder(
                    "registry-identity-a-first",
                ))
            })
            .ok_or_else(|| "first identity load must succeed".to_owned())?;
        let first_weak = Arc::downgrade(&first);
        drop(first);

        let second = cache
            .get_or_try_insert_with(second_identity, || {
                loads.set(loads.get().saturating_add(1));
                Some(registered_model2vec_test_embedder("registry-identity-b"))
            })
            .ok_or_else(|| "second identity load must succeed".to_owned())?;
        ensure(
            first_weak.upgrade().is_none(),
            "switching identities must evict the first embedder",
        )?;
        let second_weak = Arc::downgrade(&second);
        drop(second);

        let reloaded_first = cache
            .get_or_try_insert_with(first_identity, || {
                loads.set(loads.get().saturating_add(1));
                Some(registered_model2vec_test_embedder(
                    "registry-identity-a-reloaded",
                ))
            })
            .ok_or_else(|| "switching back must reload the first identity".to_owned())?;
        ensure(loads.get() == 3, "A to B to A must perform three loads")?;
        ensure(
            second_weak.upgrade().is_none(),
            "switching back must evict the second embedder",
        )?;
        ensure(
            first_weak.upgrade().is_none(),
            "reloading an identity must not resurrect its previously evicted Arc",
        )?;
        ensure(
            reloaded_first.model_name() == "registry-identity-a-reloaded",
            "switching back must return the newly loaded embedder",
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
    fn default_embedder_root_prefers_verified_model2vec_registry_layout() -> TestResult {
        let model_cache_root = unique_test_dir("embed-model-registry-root");
        let expected_registry_root = model_cache_root.join(EE_MODEL2VEC_REGISTRY_SUBDIR);
        let expected_model_dir = expected_registry_root.join(POTION_MODEL_NAME);
        let mut inspected = Vec::new();

        let resolved = resolve_default_embedder_model_root_with(&model_cache_root, |candidate| {
            inspected.push(candidate.to_path_buf());
            candidate == expected_model_dir
        });

        ensure(
            inspected == vec![expected_model_dir],
            "resolver should inspect the canonical model2vec registry entry exactly once",
        )?;
        ensure(
            resolved == expected_registry_root,
            "a verified model2vec/<name> registry entry must outrank the legacy cache root",
        )
    }

    #[test]
    fn default_embedder_root_falls_back_when_registry_entry_is_unverified() -> TestResult {
        let model_cache_root = unique_test_dir("embed-model-registry-fallback");
        let resolved = resolve_default_embedder_model_root_with(&model_cache_root, |_| false);

        ensure(
            resolved == model_cache_root,
            "an absent or unverified registry entry must preserve the default cache root",
        )
    }

    #[test]
    fn embed_download_off_keeps_hash_fallback_stack() -> TestResult {
        let settings = EeEmbedderSettings {
            model_root: unique_test_dir("embed-download-off"),
            download_mode: EeEmbedDownloadMode::Off,
            local_source: EmbedModelSource::Cache,
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
        // GH#18: Off mode must consult the frozen on-disk model directly and,
        // when none is present, use the deterministic hash fallback. It must
        // NEVER hand back the lazy potion download stub, which would fetch over
        // the network on first embed. With no model on disk the fast tier is
        // therefore the hash embedder, not the potion lazy stub.
        let settings = EeEmbedderSettings {
            model_root: unique_test_dir("embed-download-off-no-lazy"),
            download_mode: EeEmbedDownloadMode::Off,
            local_source: EmbedModelSource::Cache,
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
            local_source: EmbedModelSource::Cache,
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
    fn lazy_model2vec_cancellation_does_not_poison_retryable_state() -> TestResult {
        let embedder = Arc::new(EeLazyModel2VecEmbedder::new(unique_test_dir(
            "lazy-model-cancelled",
        )));
        let first_embedder = Arc::clone(&embedder);
        let first = crate::core::run_cli_future(async move {
            let cx = asupersync::Cx::for_testing();
            cx.set_cancel_reason(asupersync::CancelReason::user(
                "first caller cancelled model initialization",
            ));
            first_embedder.embed(&cx, "cancellation probe").await
        })
        .map_err(|error| error.to_string())?;
        let first_error = match first {
            Ok(_) => return Err("cancelled model initialization unexpectedly succeeded".to_owned()),
            Err(error) => error,
        };
        ensure(
            matches!(&first_error, SearchError::Cancelled { .. }),
            format!("model initialization cancellation lost its type: {first_error:?}"),
        )?;
        ensure(
            !embedder.failed(),
            "cancelled lazy initialization must remain retryable",
        )?;
        ensure(
            embedder.state.load(Ordering::Acquire) == EE_DOWNLOAD_STATE_PENDING,
            "cancelled lazy initialization must retain pending state for the next caller",
        )?;
        ensure(
            embedder.id() == POTION_MODEL_NAME,
            "cancelled lazy initialization must retain the intended semantic model identity",
        )?;

        let second_embedder = Arc::clone(&embedder);
        let second = crate::core::run_cli_future(async move {
            let cx = asupersync::Cx::for_testing();
            cx.set_cancel_reason(
                asupersync::CancelReason::deadline()
                    .with_message("second caller cancelled model initialization"),
            );
            second_embedder.embed(&cx, "retry cancellation probe").await
        })
        .map_err(|error| error.to_string())?;
        let second_error = match second {
            Ok(_) => {
                return Err(
                    "retried cancelled model initialization unexpectedly succeeded".to_owned(),
                );
            }
            Err(error) => error,
        };
        ensure(
            matches!(
                &second_error,
                SearchError::Cancelled { reason, .. }
                    if reason.contains("second caller cancelled model initialization")
            ),
            format!(
                "AsyncOnceCell cached the first cancellation instead of retrying: {second_error:?}"
            ),
        )?;
        ensure(
            !embedder.failed()
                && embedder.state.load(Ordering::Acquire) == EE_DOWNLOAD_STATE_PENDING,
            "repeated cancellation must leave lazy model initialization retryable",
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

        let cx = asupersync::Cx::for_testing();
        let error = match acquire_index_publish_lock_with_retry(
            &cx,
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
        connection
            .insert_procedural_rule(
                "rule_01234567890123456789012345",
                &crate::db::CreateProceduralRuleInput {
                    workspace_id: "wsp_01234567890123456789012345".to_owned(),
                    content: "Re-embedding must include active procedural rules.".to_owned(),
                    confidence: 0.9,
                    utility: 0.8,
                    importance: 0.9,
                    trust_class: "human_explicit".to_owned(),
                    scope: "workspace".to_owned(),
                    scope_pattern: None,
                    maturity: "candidate".to_owned(),
                    protected: false,
                    source_memory_ids: Vec::new(),
                    tags: vec!["reembed".to_owned()],
                },
            )
            .map_err(|e| e.to_string())?;
        let session_id = "sess_01234567890123456789012345";
        let cass_session_id = "cass-reembed-session";
        connection
            .insert_session(
                session_id,
                &crate::db::CreateSessionInput {
                    workspace_id: "wsp_01234567890123456789012345".to_owned(),
                    cass_session_id: cass_session_id.to_owned(),
                    source_path: Some("/private/reembed-session.jsonl".to_owned()),
                    agent_name: Some("codex".to_owned()),
                    model: Some("gpt-5".to_owned()),
                    started_at: Some("2026-07-30T00:00:00Z".to_owned()),
                    ended_at: Some("2026-07-30T00:01:00Z".to_owned()),
                    message_count: 1,
                    token_count: Some(10),
                    content_hash: format!(
                        "blake3:{}",
                        blake3::hash(cass_session_id.as_bytes()).to_hex()
                    ),
                    metadata_json: Some(r#"{"source":"cass"}"#.to_owned()),
                },
            )
            .map_err(|e| e.to_string())?;
        let evidence_excerpt =
            "The re-embedding run contained this positively screened evidence observation.";
        connection
            .insert_evidence_span(
                "ev_01234567890123456789012345",
                &crate::db::CreateEvidenceSpanInput {
                    workspace_id: "wsp_01234567890123456789012345".to_owned(),
                    session_id: session_id.to_owned(),
                    memory_id: None,
                    producer_kind: crate::db::EvidenceProducerKind::CassImport,
                    cass_span_id: "reembed-span".to_owned(),
                    span_kind: "message".to_owned(),
                    start_line: 1,
                    end_line: 1,
                    start_byte: None,
                    end_byte: None,
                    role: Some("assistant".to_owned()),
                    excerpt: evidence_excerpt.to_owned(),
                    content_hash: format!(
                        "blake3:{}",
                        blake3::hash(evidence_excerpt.as_bytes()).to_hex()
                    ),
                    metadata_json: Some(r#"{"source":"cass"}"#.to_owned()),
                    inherited_redaction_classes: Vec::new(),
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

    fn build_current_test_index(
        index_dir: &Path,
        generation: u64,
        documents: Vec<crate::search::IndexableDocument>,
    ) -> TestResult {
        let documents_total = u32::try_from(documents.len())
            .map_err(|_| "test index document count exceeds u32".to_owned())?;
        let document_counts = IndexDocumentCounts::memory_only(documents_total);
        let stats =
            build_index_generation_sync(index_dir, hash_fallback_embedder_stack(), documents)?;
        validate_built_generation(index_dir, stats, document_counts)?;
        write_index_metadata(index_dir, generation, document_counts, None)
            .map_err(|error| error.to_string())
    }

    fn index_regular_file_snapshot(index_dir: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>, String> {
        let mut snapshot = BTreeMap::new();
        let mut pending = vec![index_dir.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory).map_err(|error| error.to_string())? {
                let entry = entry.map_err(|error| error.to_string())?;
                let file_type = entry.file_type().map_err(|error| error.to_string())?;
                if file_type.is_dir() {
                    pending.push(entry.path());
                } else if file_type.is_file() {
                    let relative = entry
                        .path()
                        .strip_prefix(index_dir)
                        .map_err(|error| error.to_string())?
                        .to_path_buf();
                    snapshot.insert(
                        relative,
                        std::fs::read(entry.path()).map_err(|error| error.to_string())?,
                    );
                }
            }
        }
        Ok(snapshot)
    }

    fn insert_snapshot_test_memory_job(
        connection: &DbConnection,
        workspace_id: &str,
        memory_id: &str,
        job_id: &str,
        content: &str,
    ) -> TestResult {
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
                    provenance_uri: Some("test://index-source-snapshot".to_owned()),
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
                    job_type: SearchIndexJobType::SingleDocument,
                    document_source: Some("memory".to_owned()),
                    document_id: Some(memory_id.to_owned()),
                    documents_total: 1,
                },
            )
            .map_err(|error| error.to_string())
    }

    fn seed_healthy_session_index_case(
        label: &str,
        suffix: &str,
    ) -> Result<(PathBuf, PathBuf, PathBuf, DbConnection, String), String> {
        let root = unique_test_dir(label);
        let workspace = root.join("workspace");
        std::fs::create_dir_all(workspace.join(".ee")).map_err(|error| error.to_string())?;
        let workspace = std::fs::canonicalize(&workspace).map_err(|error| error.to_string())?;
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = format!("wsp_012345678901234567890123{suffix}");
        let memory_id = format!("mem_012345678901234567890123{suffix}");
        let job_id = format!("sidx_012345678901234567890123{suffix}");
        connection
            .insert_workspace(
                &workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: workspace.to_string_lossy().into_owned(),
                    name: Some(format!("healthy session index {suffix}")),
                },
            )
            .map_err(|error| error.to_string())?;
        insert_snapshot_test_memory_job(
            &connection,
            &workspace_id,
            &memory_id,
            &job_id,
            "Healthy baseline memory before the session evidence transaction.",
        )?;
        let baseline = process_index_job_for_connection(&connection, &job_id, &index_dir)
            .map_err(|error| error.to_string())?;
        ensure(
            baseline.outcome == "completed" && baseline.documents_indexed == 1,
            format!("baseline job must publish one healthy document: {baseline:?}"),
        )?;
        let status = get_index_status_with_connection(
            &IndexStatusOptions {
                workspace_path: workspace.clone(),
                database_path: Some(database.clone()),
                index_dir: Some(index_dir.clone()),
            },
            Some(&connection),
        )
        .map_err(|error| error.to_string())?;
        ensure(
            status.health == IndexHealth::Ready
                && status.db_generation == status.index_generation
                && status.index_document_counts == IndexDocumentCounts::checked(1, 0, 0, 0, 0).ok(),
            format!("baseline index must begin Ready with exact counts: {status:?}"),
        )?;
        Ok((workspace, database, index_dir, connection, workspace_id))
    }

    fn session_index_input(workspace_id: &str, suffix: &str) -> crate::db::CreateSessionInput {
        let cass_session_id = format!("cass-session-generation-{suffix}");
        crate::db::CreateSessionInput {
            workspace_id: workspace_id.to_owned(),
            cass_session_id: cass_session_id.clone(),
            source_path: Some(format!("/private/raw-session-{suffix}.jsonl")),
            agent_name: Some("codex".to_owned()),
            model: Some("gpt-5".to_owned()),
            started_at: Some("2026-08-08T01:00:00Z".to_owned()),
            ended_at: Some("2026-08-08T01:01:00Z".to_owned()),
            message_count: 2,
            token_count: Some(24),
            content_hash: format!(
                "blake3:{}",
                blake3::hash(cass_session_id.as_bytes()).to_hex()
            ),
            metadata_json: Some(r#"{"source":"cass"}"#.to_owned()),
        }
    }

    fn admitted_session_evidence_input(
        workspace_id: &str,
        session_id: &str,
        suffix: &str,
        line: u32,
        excerpt: &str,
    ) -> crate::db::CreateEvidenceSpanInput {
        crate::db::CreateEvidenceSpanInput {
            workspace_id: workspace_id.to_owned(),
            session_id: session_id.to_owned(),
            memory_id: None,
            producer_kind: crate::db::EvidenceProducerKind::CassImport,
            cass_span_id: format!("raw-session-{suffix}-span-{line}"),
            span_kind: "message".to_owned(),
            start_line: line,
            end_line: line,
            start_byte: None,
            end_byte: None,
            role: Some("assistant".to_owned()),
            excerpt: excerpt.to_owned(),
            content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
            metadata_json: Some(
                r#"{"source":"cass","rawPath":"/private/never-egress.jsonl"}"#.to_owned(),
            ),
            inherited_redaction_classes: Vec::new(),
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum SessionEvidenceDrain {
        OrdinarySingle,
        LimitedCoalesced,
    }

    fn assert_session_evidence_job_from_healthy_index(
        mode: SessionEvidenceDrain,
        suffix: &str,
    ) -> TestResult {
        let (workspace, database, index_dir, connection, workspace_id) =
            seed_healthy_session_index_case("session-evidence-job", suffix)?;
        let memory_id = format!("mem_012345678901234567890123{suffix}");
        let session_id = format!("sess_012345678901234567890123{suffix}");
        let first_evidence_id = format!("ev_012345678901234567890123{suffix}");
        let second_evidence_id = format!("ev_112345678901234567890123{suffix}");
        let session_job_id = format!("sidx_112345678901234567890123{suffix}");
        let first_excerpt = "Quartz kestrel verification evidence stayed safely searchable.";
        let second_excerpt = "Nimbus lantern provenance evidence stayed safely searchable.";

        connection
            .with_transaction(|| {
                connection
                    .insert_session(&session_id, &session_index_input(&workspace_id, suffix))?;
                connection.insert_evidence_span(
                    &first_evidence_id,
                    &admitted_session_evidence_input(
                        &workspace_id,
                        &session_id,
                        suffix,
                        7,
                        first_excerpt,
                    ),
                )?;
                connection.insert_evidence_span(
                    &second_evidence_id,
                    &admitted_session_evidence_input(
                        &workspace_id,
                        &session_id,
                        suffix,
                        11,
                        second_excerpt,
                    ),
                )?;
                connection.insert_search_index_job(
                    &session_job_id,
                    &crate::db::CreateSearchIndexJobInput {
                        workspace_id: workspace_id.clone(),
                        job_type: SearchIndexJobType::SingleDocument,
                        document_source: Some("session".to_owned()),
                        document_id: Some(session_id.clone()),
                        documents_total: 1,
                    },
                )
            })
            .map_err(|error| error.to_string())?;

        let status_options = IndexStatusOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database.clone()),
            index_dir: Some(index_dir.clone()),
        };
        let stale = get_index_status_with_connection(&status_options, Some(&connection))
            .map_err(|error| error.to_string())?;
        ensure(
            stale.health == IndexHealth::Stale
                && stale
                    .db_generation
                    .zip(stale.index_generation)
                    .is_some_and(|(database, index)| database > index)
                && stale.db_evidence_admitted_count == 2,
            format!("atomic session/evidence commit must make the healthy index stale: {stale:?}"),
        )?;

        let snapshot = collect_workspace_index_source_snapshot(&connection, &workspace_id)
            .map_err(|error| error.to_string())?;
        let admission = EvidenceAdmissionTotals::from_report(&snapshot.evidence_admission);
        ensure(
            snapshot.document_counts == IndexDocumentCounts::checked(1, 1, 0, 0, 2)?
                && admission.admitted == 2
                && admission.quarantined == 0
                && admission.denied == 0,
            format!(
                "source snapshot must have exact kind and admission counts: counts={:?} admission={admission:?}",
                snapshot.document_counts
            ),
        )?;
        let evidence_documents = snapshot
            .documents
            .iter()
            .filter(|document| {
                document
                    .metadata
                    .get("kind")
                    .is_some_and(|kind| kind == "evidence_span")
            })
            .map(|document| (document.id.as_str(), document))
            .collect::<BTreeMap<_, _>>();
        ensure(
            evidence_documents.keys().copied().collect::<BTreeSet<_>>()
                == BTreeSet::from([first_evidence_id.as_str(), second_evidence_id.as_str()]),
            format!(
                "snapshot must contain exactly the admitted evidence IDs: {evidence_documents:?}"
            ),
        )?;
        for (evidence_id, excerpt, line) in [
            (first_evidence_id.as_str(), first_excerpt, 7_u32),
            (second_evidence_id.as_str(), second_excerpt, 11_u32),
        ] {
            let document = evidence_documents
                .get(evidence_id)
                .ok_or_else(|| format!("missing evidence document {evidence_id}"))?;
            let expected_provenance = format!("cass-session://{session_id}#L{line}-{line}");
            let rendered = format!("{} {:?}", document.content, document.metadata);
            ensure(
                document.content == excerpt
                    && document.metadata.get("provenance_uri") == Some(&expected_provenance)
                    && document.metadata.get("kind") == Some(&"evidence_span".to_owned())
                    && !rendered.contains("/private/")
                    && !rendered.contains("raw-session"),
                format!("evidence projection must retain only safe content/provenance: {rendered}"),
            )?;
        }

        let reports = match mode {
            SessionEvidenceDrain::OrdinarySingle => vec![
                process_index_job_for_connection(&connection, &session_job_id, &index_dir)
                    .map_err(|error| error.to_string())?,
            ],
            SessionEvidenceDrain::LimitedCoalesced => process_pending_index_jobs_coalesced(
                &connection,
                &workspace_id,
                &index_dir,
                Some(1),
            )
            .map_err(|error| error.to_string())?,
        };
        ensure(
            reports.len() == 1
                && reports[0].outcome == "completed"
                && reports[0].documents_indexed == 4,
            format!("{mode:?} must publish the complete four-document corpus: {reports:?}"),
        )?;

        let metadata: serde_json::Value = serde_json::from_slice(
            &std::fs::read(index_dir.join(INDEX_METADATA_FILE))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let quality_expected = index_dir
            .join(VECTOR_INDEX_QUALITY_FILE)
            .is_file()
            .then_some(4_u32);
        let lexical_expected = cfg!(feature = "lexical-bm25").then_some(4_u32);
        ensure(
            metadata["documentCount"] == 4
                && metadata["documentCounts"]
                    == serde_json::json!({
                        "memories": 1,
                        "sessions": 1,
                        "artifacts": 0,
                        "rules": 0,
                        "evidence": 2,
                    })
                && metadata["tierDocumentCounts"]["fast"] == 4
                && metadata["tierDocumentCounts"]["quality"] == serde_json::json!(quality_expected)
                && metadata["tierDocumentCounts"]["lexical"] == serde_json::json!(lexical_expected),
            format!("published kind/tier counts must be exact: {metadata}"),
        )?;
        let indexed_ids = vector_index_snapshot(&index_dir)?
            .into_iter()
            .map(|row| row.doc_id)
            .collect::<BTreeSet<_>>();
        ensure(
            indexed_ids
                == BTreeSet::from([
                    memory_id,
                    session_id.clone(),
                    first_evidence_id.clone(),
                    second_evidence_id.clone(),
                ]),
            format!("published index must contain the exact complete corpus: {indexed_ids:?}"),
        )?;

        for (query, evidence_id, excerpt, line) in [
            (
                "Quartz kestrel",
                first_evidence_id.as_str(),
                first_excerpt,
                7_u32,
            ),
            (
                "Nimbus lantern",
                second_evidence_id.as_str(),
                second_excerpt,
                11_u32,
            ),
        ] {
            let hit = published_search_hit(&index_dir, query, evidence_id)?;
            let expected = PublishedSearchHit {
                doc_id: evidence_id.to_owned(),
                content: excerpt.to_owned(),
                provenance_uri: format!("cass-session://{session_id}#L{line}-{line}"),
                kind: "evidence_span".to_owned(),
            };
            ensure(
                hit == expected,
                format!(
                    "published TwoTierSearcher must return the exact hydrated evidence hit for {query:?}: expected={expected:?} actual={hit:?}"
                ),
            )?;
            let rendered = format!("{hit:?}");
            ensure(
                !rendered.contains("/private/") && !rendered.contains("raw-session"),
                format!("published search result leaked raw evidence provenance: {rendered}"),
            )?;
        }

        let ready = get_index_status_with_connection(&status_options, Some(&connection))
            .map_err(|error| error.to_string())?;
        ensure(
            ready.health == IndexHealth::Ready
                && ready.db_generation == ready.index_generation
                && ready.index_document_counts == IndexDocumentCounts::checked(1, 1, 0, 0, 2).ok()
                && ready.db_evidence_admitted_count == 2
                && ready.db_evidence_quarantined_count == 0
                && ready.db_evidence_denied_count == 0,
            format!("drain must restore Ready at the exact DB generation: {ready:?}"),
        )?;

        let before_repeat = index_regular_file_snapshot(&index_dir)?;
        match mode {
            SessionEvidenceDrain::OrdinarySingle => {
                let repeated =
                    process_index_job_for_connection(&connection, &session_job_id, &index_dir)
                        .map_err(|error| error.to_string())?;
                ensure(
                    repeated.outcome == "skipped",
                    format!("repeat ordinary drain must skip completed job: {repeated:?}"),
                )?;
            }
            SessionEvidenceDrain::LimitedCoalesced => {
                let repeated = process_pending_index_jobs_coalesced(
                    &connection,
                    &workspace_id,
                    &index_dir,
                    Some(1),
                )
                .map_err(|error| error.to_string())?;
                ensure(
                    repeated.is_empty(),
                    format!("repeat coalesced drain must find no pending work: {repeated:?}"),
                )?;
            }
        }
        ensure(
            index_regular_file_snapshot(&index_dir)? == before_repeat,
            "idempotent repeat drain must not mutate the published index",
        )?;
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn session_insert_with_zero_admitted_spans_marks_healthy_index_stale() -> TestResult {
        let (workspace, database, index_dir, connection, workspace_id) =
            seed_healthy_session_index_case("session-zero-evidence-stale", "z0")?;
        let session_id = "sess_012345678901234567890123z0";
        let session_job_id = "sidx_112345678901234567890123z0";
        connection
            .with_transaction(|| {
                connection.insert_session(session_id, &session_index_input(&workspace_id, "z0"))?;
                connection.insert_search_index_job(
                    session_job_id,
                    &crate::db::CreateSearchIndexJobInput {
                        workspace_id: workspace_id.clone(),
                        job_type: SearchIndexJobType::SingleDocument,
                        document_source: Some("session".to_owned()),
                        document_id: Some(session_id.to_owned()),
                        documents_total: 1,
                    },
                )
            })
            .map_err(|error| error.to_string())?;

        let status = get_index_status_with_connection(
            &IndexStatusOptions {
                workspace_path: workspace,
                database_path: Some(database),
                index_dir: Some(index_dir),
            },
            Some(&connection),
        )
        .map_err(|error| error.to_string())?;
        ensure(
            status.health == IndexHealth::Stale
                && status
                    .db_generation
                    .zip(status.index_generation)
                    .is_some_and(|(database, index)| database > index)
                && status.db_session_count == 1
                && status.db_evidence_count == 0
                && status.db_evidence_admitted_count == 0
                && status.db_evidence_quarantined_count == 0
                && status.db_evidence_denied_count == 0,
            format!(
                "a session-only corpus change must make a zero-evidence healthy index stale: {status:?}"
            ),
        )?;
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn ordinary_session_job_indexes_atomic_admitted_evidence_without_manual_rebuild() -> TestResult
    {
        assert_session_evidence_job_from_healthy_index(SessionEvidenceDrain::OrdinarySingle, "o1")
    }

    #[test]
    fn limited_coalesced_session_job_indexes_atomic_admitted_evidence_without_manual_rebuild()
    -> TestResult {
        assert_session_evidence_job_from_healthy_index(SessionEvidenceDrain::LimitedCoalesced, "c1")
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

    #[derive(Debug, Eq, PartialEq)]
    struct PublishedSearchHit {
        doc_id: String,
        content: String,
        provenance_uri: String,
        kind: String,
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

    fn published_search_hit(
        index_dir: &Path,
        query: &str,
        expected_doc_id: &str,
    ) -> Result<PublishedSearchHit, String> {
        let index = Arc::new(
            crate::search::TwoTierIndex::open(index_dir, crate::search::TwoTierConfig::default())
                .map_err(|error| error.to_string())?,
        );
        let searcher = crate::search::TwoTierSearcher::new(
            index,
            default_search_embedder_stack().fast_arc(),
            crate::search::TwoTierConfig::default(),
        );
        let query = query.to_owned();
        let expected_doc_id = expected_doc_id.to_owned();
        crate::core::run_cli_future(async move {
            let cx = asupersync::Cx::for_testing();
            let (results, _) = searcher
                .search_collect(&cx, &query, 4)
                .await
                .map_err(|error| error.to_string())?;
            let result = results
                .into_iter()
                .find(|result| result.doc_id == expected_doc_id)
                .ok_or_else(|| {
                    format!(
                        "published TwoTierSearcher did not return expected evidence {expected_doc_id} among four hits for {query:?}"
                    )
                })?;
            let metadata = result
                .metadata
                .as_deref()
                .ok_or_else(|| format!("published search hit {} had no metadata", result.doc_id))?;
            let required = |key: &str| {
                metadata
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        format!(
                            "published search hit {} omitted string metadata {key}",
                            result.doc_id
                        )
                    })
            };
            Ok::<PublishedSearchHit, String>(PublishedSearchHit {
                doc_id: result.doc_id.to_string(),
                content: required("content")?,
                provenance_uri: required("provenance_uri")?,
                kind: required("kind")?,
            })
        })
        .map_err(|error| format!("published search runtime failed: {error}"))?
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
        write_index_metadata(&incremental_dir, generation, live_docs.len() as u32, None)
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
                        IndexDocumentCounts::memory_only(live_docs.len() as u32),
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
                        IndexDocumentCounts::memory_only(live_docs.len() as u32),
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
        write_index_metadata(&full_dir, generation, live_docs.len() as u32, None)
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
            rules_indexed: 1,
            evidence_indexed: 1,
            documents_total: 12,
            index_dir: PathBuf::from("/tmp/index"),
            elapsed_ms: 123.4,
            dry_run: false,
            evidence_admission: EvidenceAdmissionReport::default(),
            errors: Vec::new(),
            runtime_profile: test_runtime_profile(),
        };

        let json = report.data_json();
        assert_eq!(json["command"], "index_rebuild");
        assert_eq!(json["status"], "success");
        assert_eq!(json["memories_indexed"], 5);
        assert_eq!(json["sessions_indexed"], 3);
        assert_eq!(json["artifacts_indexed"], 2);
        assert_eq!(json["rules_indexed"], 1);
        assert_eq!(json["evidence_indexed"], 1);
        assert_eq!(json["documents_total"], 12);
        assert_eq!(
            json["evidenceAdmissionTotals"],
            serde_json::json!({"admitted": 0, "quarantined": 0, "denied": 0})
        );
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
            IncrementalFallbackReason::CorpusRevisionMismatch.as_str(),
            "corpus_revision_mismatch"
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
            IndexDocumentCounts::memory_only(1),
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
    fn incremental_rejects_legacy_metadata_before_mutating_any_tier() -> TestResult {
        let root = unique_test_dir("incremental-legacy-corpus-revision");
        let index_dir = root.join("index");
        build_current_test_index(
            &index_dir,
            2,
            vec![test_indexable_doc("doc-alpha", "alpha content")],
        )?;
        std::fs::write(
            index_dir.join(INDEX_METADATA_FILE),
            r#"{"schema":"ee.index_metadata.v1","generation":2,"sourceGeneration":2,"documentCount":1}"#,
        )
        .map_err(|error| error.to_string())?;
        let before = index_regular_file_snapshot(&index_dir)?;

        let outcome = apply_incremental_index_change_sync(
            &index_dir,
            hash_fallback_embedder_stack(),
            "doc-beta",
            Some(test_indexable_doc("doc-beta", "beta content")),
            2,
            IndexDocumentCounts::memory_only(2),
        );

        match outcome {
            IncrementalApplyOutcome::Fallback { reason, detail } => {
                ensure(
                    reason == IncrementalFallbackReason::CorpusRevisionMismatch,
                    format!("expected corpus revision fallback, got {reason:?}: {detail}"),
                )?;
                ensure(
                    detail.contains(expected_index_corpus_revision().as_str())
                        && detail.contains("full index rebuild is required"),
                    format!("fallback must expose expected revision and repair: {detail}"),
                )?;
            }
            other => return Err(format!("unexpected incremental outcome: {other:?}")),
        }
        ensure(
            index_regular_file_snapshot(&index_dir)? == before,
            "legacy compatibility rejection must occur before any tier or metadata mutation",
        )
    }

    #[test]
    fn incremental_rejects_corrupt_metadata_before_mutating_any_tier() -> TestResult {
        let root = unique_test_dir("incremental-corrupt-corpus-revision");
        let index_dir = root.join("index");
        build_current_test_index(
            &index_dir,
            2,
            vec![test_indexable_doc("doc-alpha", "alpha content")],
        )?;
        std::fs::write(index_dir.join(INDEX_METADATA_FILE), "{not-json")
            .map_err(|error| error.to_string())?;
        let before = index_regular_file_snapshot(&index_dir)?;

        let outcome = apply_incremental_index_change_sync(
            &index_dir,
            hash_fallback_embedder_stack(),
            "doc-beta",
            Some(test_indexable_doc("doc-beta", "beta content")),
            2,
            IndexDocumentCounts::memory_only(2),
        );

        ensure(
            matches!(
                outcome,
                IncrementalApplyOutcome::Fallback {
                    reason: IncrementalFallbackReason::CorpusRevisionMismatch,
                    ..
                }
            ),
            format!("corrupt metadata must force corpus fallback: {outcome:?}"),
        )?;
        ensure(
            index_regular_file_snapshot(&index_dir)? == before,
            "corrupt metadata rejection must occur before any tier mutation",
        )
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
        write_index_metadata(&index_dir, 1, 1, None).map_err(|error| error.to_string())?;

        let outcome = apply_incremental_index_change_sync(
            &index_dir,
            hash_fallback_embedder_stack(),
            "doc-beta",
            Some(test_indexable_doc("doc-beta", "beta content")),
            3,
            IndexDocumentCounts::memory_only(2),
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
        write_index_metadata(&index_dir, 1, 1, None).map_err(|error| error.to_string())?;

        let outcome = apply_incremental_index_batch_sync(
            &index_dir,
            hash_fallback_embedder_stack(),
            vec![
                test_indexable_doc("doc-beta", "beta content"),
                test_indexable_doc("doc-gamma", "gamma content"),
            ],
            3,
            IndexDocumentCounts::memory_only(3),
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
    fn coalesced_legacy_index_rebuilds_complete_rule_and_evidence_corpus() -> TestResult {
        let root = unique_test_dir("coalesced-legacy-complete-corpus");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let root = std::fs::canonicalize(&root).map_err(|error| error.to_string())?;
        let index_dir = root.join("index");
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_012345678901234567890123cr";
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: root.to_string_lossy().into_owned(),
                    name: Some("legacy complete corpus".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        insert_snapshot_test_memory_job(
            &connection,
            workspace_id,
            "mem_012345678901234567890123c0",
            "sidx_012345678901234567890123c0",
            "seed memory before rule and evidence corpus expansion",
        )?;
        let seed = process_index_job_for_connection(
            &connection,
            "sidx_012345678901234567890123c0",
            &index_dir,
        )
        .map_err(|error| error.to_string())?;
        ensure(
            seed.outcome == "completed",
            format!("seed index job did not complete: {seed:?}"),
        )?;

        let rule_id = "rule_012345678901234567890123cr";
        connection
            .insert_procedural_rule(
                rule_id,
                &crate::db::CreateProceduralRuleInput {
                    workspace_id: workspace_id.to_owned(),
                    content: "Corpus revision rule requires a full rebuild before publication."
                        .to_owned(),
                    confidence: 0.9,
                    utility: 0.8,
                    importance: 0.9,
                    trust_class: "human_explicit".to_owned(),
                    scope: "workspace".to_owned(),
                    scope_pattern: None,
                    maturity: "candidate".to_owned(),
                    protected: false,
                    source_memory_ids: Vec::new(),
                    tags: vec!["corpus-revision".to_owned()],
                },
            )
            .map_err(|error| error.to_string())?;

        let session_id = "sess_012345678901234567890123cr";
        let cass_session_id = "cass-corpus-revision-session";
        connection
            .insert_session(
                session_id,
                &crate::db::CreateSessionInput {
                    workspace_id: workspace_id.to_owned(),
                    cass_session_id: cass_session_id.to_owned(),
                    source_path: Some("/private/corpus-revision-session.jsonl".to_owned()),
                    agent_name: Some("codex".to_owned()),
                    model: Some("gpt-5".to_owned()),
                    started_at: Some("2026-07-30T00:00:00Z".to_owned()),
                    ended_at: Some("2026-07-30T00:01:00Z".to_owned()),
                    message_count: 1,
                    token_count: Some(12),
                    content_hash: format!(
                        "blake3:{}",
                        blake3::hash(cass_session_id.as_bytes()).to_hex()
                    ),
                    metadata_json: Some(r#"{"source":"cass"}"#.to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        let evidence_id = "ev_012345678901234567890123cr";
        let evidence_excerpt =
            "The corpus revision experiment observed the admitted evidence row after rebuilding.";
        connection
            .insert_evidence_span(
                evidence_id,
                &crate::db::CreateEvidenceSpanInput {
                    workspace_id: workspace_id.to_owned(),
                    session_id: session_id.to_owned(),
                    memory_id: None,
                    producer_kind: crate::db::EvidenceProducerKind::CassImport,
                    cass_span_id: "corpus-revision-span".to_owned(),
                    span_kind: "message".to_owned(),
                    start_line: 1,
                    end_line: 1,
                    start_byte: None,
                    end_byte: None,
                    role: Some("assistant".to_owned()),
                    excerpt: evidence_excerpt.to_owned(),
                    content_hash: format!(
                        "blake3:{}",
                        blake3::hash(evidence_excerpt.as_bytes()).to_hex()
                    ),
                    metadata_json: Some(r#"{"source":"cass"}"#.to_owned()),
                    inherited_redaction_classes: Vec::new(),
                },
            )
            .map_err(|error| error.to_string())?;
        let denied_evidence_id = "ev_012345678901234567890123cd";
        let denied_evidence_excerpt =
            "Supporting AGENTS evidence is retained but denied direct index membership.";
        connection
            .insert_evidence_span(
                denied_evidence_id,
                &crate::db::CreateEvidenceSpanInput {
                    workspace_id: workspace_id.to_owned(),
                    session_id: session_id.to_owned(),
                    memory_id: None,
                    producer_kind: crate::db::EvidenceProducerKind::AgentsmdImport,
                    cass_span_id: "corpus-revision-denied-span".to_owned(),
                    span_kind: "message".to_owned(),
                    start_line: 2,
                    end_line: 2,
                    start_byte: None,
                    end_byte: None,
                    role: Some("agentsmd_import".to_owned()),
                    excerpt: denied_evidence_excerpt.to_owned(),
                    content_hash: format!(
                        "blake3:{}",
                        blake3::hash(denied_evidence_excerpt.as_bytes()).to_hex()
                    ),
                    metadata_json: Some(r#"{"source":"agentsmd"}"#.to_owned()),
                    inherited_redaction_classes: Vec::new(),
                },
            )
            .map_err(|error| error.to_string())?;

        let other_workspace_id = "wsp_012345678901234567890123co";
        let other_workspace_path = root.join("other-workspace");
        std::fs::create_dir_all(&other_workspace_path).map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                other_workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: other_workspace_path.to_string_lossy().into_owned(),
                    name: Some("other corpus workspace".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        let other_rule_id = "rule_012345678901234567890123co";
        connection
            .insert_procedural_rule(
                other_rule_id,
                &crate::db::CreateProceduralRuleInput {
                    workspace_id: other_workspace_id.to_owned(),
                    content: "Other workspace rule must never cross the corpus fence.".to_owned(),
                    confidence: 0.9,
                    utility: 0.8,
                    importance: 0.9,
                    trust_class: "human_explicit".to_owned(),
                    scope: "workspace".to_owned(),
                    scope_pattern: None,
                    maturity: "candidate".to_owned(),
                    protected: false,
                    source_memory_ids: Vec::new(),
                    tags: vec!["isolation".to_owned()],
                },
            )
            .map_err(|error| error.to_string())?;
        let other_session_id = "sess_012345678901234567890123co";
        connection
            .insert_session(
                other_session_id,
                &crate::db::CreateSessionInput {
                    workspace_id: other_workspace_id.to_owned(),
                    cass_session_id: "cass-other-corpus-session".to_owned(),
                    source_path: Some("/private/other-corpus-session.jsonl".to_owned()),
                    agent_name: Some("codex".to_owned()),
                    model: Some("gpt-5".to_owned()),
                    started_at: Some("2026-07-30T00:00:00Z".to_owned()),
                    ended_at: Some("2026-07-30T00:01:00Z".to_owned()),
                    message_count: 1,
                    token_count: Some(12),
                    content_hash: format!(
                        "blake3:{}",
                        blake3::hash(b"cass-other-corpus-session").to_hex()
                    ),
                    metadata_json: Some(r#"{"source":"cass"}"#.to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        let other_evidence_id = "ev_012345678901234567890123co";
        let other_evidence_excerpt =
            "The isolated workspace recorded a separate corpus evidence observation.";
        connection
            .insert_evidence_span(
                other_evidence_id,
                &crate::db::CreateEvidenceSpanInput {
                    workspace_id: other_workspace_id.to_owned(),
                    session_id: other_session_id.to_owned(),
                    memory_id: None,
                    producer_kind: crate::db::EvidenceProducerKind::CassImport,
                    cass_span_id: "other-corpus-span".to_owned(),
                    span_kind: "message".to_owned(),
                    start_line: 1,
                    end_line: 1,
                    start_byte: None,
                    end_byte: None,
                    role: Some("assistant".to_owned()),
                    excerpt: other_evidence_excerpt.to_owned(),
                    content_hash: format!(
                        "blake3:{}",
                        blake3::hash(other_evidence_excerpt.as_bytes()).to_hex()
                    ),
                    metadata_json: Some(r#"{"source":"cass"}"#.to_owned()),
                    inherited_redaction_classes: Vec::new(),
                },
            )
            .map_err(|error| error.to_string())?;

        for (memory_id, job_id, content) in [
            (
                "mem_012345678901234567890123c1",
                "sidx_012345678901234567890123c1",
                "beta memory for coalesced legacy fallback",
            ),
            (
                "mem_012345678901234567890123c2",
                "sidx_012345678901234567890123c2",
                "gamma memory for coalesced legacy fallback",
            ),
        ] {
            insert_snapshot_test_memory_job(&connection, workspace_id, memory_id, job_id, content)?;
        }

        let live_generation = connection
            .get_workspace_generation(workspace_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "workspace generation missing".to_owned())?;
        std::fs::write(
            index_dir.join(INDEX_METADATA_FILE),
            serde_json::json!({
                "schema": "ee.index_metadata.v1",
                "generation": live_generation,
                "sourceGeneration": live_generation,
                "documentCount": 1,
            })
            .to_string(),
        )
        .map_err(|error| error.to_string())?;

        let reports =
            process_pending_index_jobs_coalesced(&connection, workspace_id, &index_dir, None)
                .map_err(|error| error.to_string())?;
        ensure(
            reports.len() == 2,
            format!("expected one report per coalesced job, got {reports:?}"),
        )?;
        ensure(
            reports.iter().all(|report| {
                report.outcome == "completed"
                    && report.fallback_to_full.as_deref()
                        == Some(IncrementalFallbackReason::CorpusRevisionMismatch.as_str())
                    && report.documents_indexed == 6
            }),
            format!(
                "legacy index must satisfy every claimed job with one complete fallback rebuild: {reports:?}"
            ),
        )?;

        let metadata = read_index_metadata(&index_dir);
        ensure(
            metadata.compatibility_error.is_none()
                && metadata.corruption_error.is_none()
                && metadata.generation == Some(live_generation),
            format!("rebuilt metadata must be current: {metadata:?}"),
        )?;
        ensure(
            metadata.document_counts == IndexDocumentCounts::checked(3, 1, 0, 1, 1).ok(),
            format!(
                "rebuilt per-kind counts must be exact: {:?}",
                metadata.document_counts
            ),
        )?;
        ensure(
            index_generation_is_recoverable(&index_dir),
            "rebuilt generation must pass every metadata and persisted-tier gate",
        )?;
        let indexed_ids = vector_index_snapshot(&index_dir)?
            .into_iter()
            .map(|row| row.doc_id)
            .collect::<BTreeSet<_>>();
        ensure(
            indexed_ids.contains(rule_id) && indexed_ids.contains(evidence_id),
            format!("rebuilt corpus must contain rule and evidence documents: {indexed_ids:?}"),
        )?;
        ensure(
            !indexed_ids.contains(denied_evidence_id),
            format!("denied evidence must not enter any rebuilt tier: {indexed_ids:?}"),
        )?;
        let (_, admission) = current_index_corpus_counts(&connection, workspace_id)
            .map_err(|error| error.to_string())?;
        let admission_totals = EvidenceAdmissionTotals::from_report(&admission);
        ensure(
            admission_totals.admitted == 1 && admission_totals.denied == 1,
            format!("evidence admission totals must remain truthful: {admission_totals:?}"),
        )?;
        ensure(
            !indexed_ids.contains(other_rule_id)
                && !indexed_ids.contains(other_session_id)
                && !indexed_ids.contains(other_evidence_id),
            format!("rebuilt corpus crossed the workspace boundary: {indexed_ids:?}"),
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
    fn single_processor_never_stamps_post_snapshot_commit_current() -> TestResult {
        let root = unique_test_dir("single-source-snapshot-race");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let root = std::fs::canonicalize(&root).map_err(|error| error.to_string())?;
        let index_dir = root.join("index");
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_012345678901234567890123r1";
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: root.to_string_lossy().into_owned(),
                    name: Some("single source snapshot race".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        insert_snapshot_test_memory_job(
            &connection,
            workspace_id,
            "mem_012345678901234567890123r1",
            "sidx_012345678901234567890123r1",
            "snapshot seed alpha",
        )?;
        process_index_job_for_connection(
            &connection,
            "sidx_012345678901234567890123r1",
            &index_dir,
        )
        .map_err(|error| error.to_string())?;

        let beta_job_id = "sidx_012345678901234567890123r2";
        insert_snapshot_test_memory_job(
            &connection,
            workspace_id,
            "mem_012345678901234567890123r2",
            beta_job_id,
            "snapshot beta captured before publication",
        )?;
        let beta_job = connection
            .get_search_index_job(beta_job_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "beta job missing".to_owned())?;
        let gamma_memory_id = "mem_012345678901234567890123r3";
        let gamma_job_id = "sidx_012345678901234567890123r3";
        let beta_report =
            process_one_index_job_after_snapshot(&connection, &beta_job, &index_dir, || {
                insert_snapshot_test_memory_job(
                    &connection,
                    workspace_id,
                    gamma_memory_id,
                    gamma_job_id,
                    "post snapshot gamma unique phrase",
                )
                .map_err(IndexRebuildError::Index)
            })
            .map_err(|error| error.to_string())?;
        ensure(
            beta_report.outcome == "completed",
            format!("beta snapshot publication failed: {beta_report:?}"),
        )?;

        let published_generation = read_index_metadata(&index_dir)
            .generation
            .ok_or_else(|| "published snapshot generation missing".to_owned())?;
        let live_generation = connection
            .get_workspace_generation(workspace_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "live snapshot generation missing".to_owned())?;
        ensure(
            published_generation < live_generation,
            format!(
                "post-snapshot commit must leave an explicitly stale manifest: published={published_generation:?} live={live_generation:?}"
            ),
        )?;
        ensure(
            vector_index_snapshot(&index_dir)?.len() == 2,
            "the captured corpus must contain seed and beta only",
        )?;
        ensure(
            !search_result_snapshot(&index_dir, "post snapshot gamma unique phrase", 10)?
                .iter()
                .any(|row| row.doc_id == gamma_memory_id),
            "gamma must not be mislabeled as represented by the older snapshot",
        )?;

        process_index_job_for_connection(&connection, gamma_job_id, &index_dir)
            .map_err(|error| error.to_string())?;
        ensure(
            read_index_metadata(&index_dir).generation == Some(live_generation),
            "draining the post-snapshot job converges the manifest generation",
        )?;
        ensure(
            search_result_snapshot(&index_dir, "post snapshot gamma unique phrase", 10)?
                .iter()
                .any(|row| row.doc_id == gamma_memory_id),
            "gamma must be searchable after its own job is processed",
        )
    }

    #[test]
    fn limited_coalesced_processor_fences_unclaimed_and_post_snapshot_jobs() -> TestResult {
        let root = unique_test_dir("coalesced-source-snapshot-race");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let root = std::fs::canonicalize(&root).map_err(|error| error.to_string())?;
        let index_dir = root.join("index");
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_012345678901234567890123c1";
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: root.to_string_lossy().into_owned(),
                    name: Some("coalesced source snapshot race".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        insert_snapshot_test_memory_job(
            &connection,
            workspace_id,
            "mem_012345678901234567890123c1",
            "sidx_012345678901234567890123c1",
            "coalesced snapshot seed",
        )?;
        process_index_job_for_connection(
            &connection,
            "sidx_012345678901234567890123c1",
            &index_dir,
        )
        .map_err(|error| error.to_string())?;
        insert_snapshot_test_memory_job(
            &connection,
            workspace_id,
            "mem_012345678901234567890123c2",
            "sidx_012345678901234567890123c2",
            "coalesced beta claimed",
        )?;
        let delta_memory_id = "mem_012345678901234567890123c3";
        insert_snapshot_test_memory_job(
            &connection,
            workspace_id,
            delta_memory_id,
            "sidx_012345678901234567890123c3",
            "coalesced delta unclaimed but captured",
        )?;
        let gamma_memory_id = "mem_012345678901234567890123c4";
        let reports = process_pending_index_jobs_coalesced_after_snapshot(
            &connection,
            workspace_id,
            &index_dir,
            Some(1),
            || {
                insert_snapshot_test_memory_job(
                    &connection,
                    workspace_id,
                    gamma_memory_id,
                    "sidx_012345678901234567890123c4",
                    "coalesced gamma committed after snapshot",
                )
                .map_err(IndexRebuildError::Index)
            },
        )
        .map_err(|error| error.to_string())?;
        ensure(
            reports.len() == 1
                && reports[0]
                    .processing_mode
                    .contains("open_sibling_full_rebuild"),
            format!("limited coalescing must full-rebuild for an unclaimed open job: {reports:?}"),
        )?;
        ensure(
            vector_index_snapshot(&index_dir)?.len() == 3,
            "full snapshot must include the unclaimed delta row",
        )?;
        let stale_generation = read_index_metadata(&index_dir)
            .generation
            .ok_or_else(|| "coalesced published generation missing".to_owned())?;
        let live_generation = connection
            .get_workspace_generation(workspace_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "coalesced live generation missing".to_owned())?;
        ensure(
            stale_generation < live_generation,
            format!(
                "post-snapshot gamma commit must remain visibly stale: published={stale_generation:?} live={live_generation:?}"
            ),
        )?;
        let stale_results =
            search_result_snapshot(&index_dir, "coalesced delta gamma captured", 10)?;
        ensure(
            stale_results
                .iter()
                .any(|row| row.doc_id == delta_memory_id)
                && !stale_results
                    .iter()
                    .any(|row| row.doc_id == gamma_memory_id),
            format!("captured delta and post-snapshot gamma posture drifted: {stale_results:?}"),
        )?;

        let drained =
            process_pending_index_jobs_coalesced(&connection, workspace_id, &index_dir, None)
                .map_err(|error| error.to_string())?;
        ensure(
            drained.iter().all(|report| report.outcome == "completed"),
            format!("remaining coalesced jobs did not converge: {drained:?}"),
        )?;
        ensure(
            read_index_metadata(&index_dir).generation == Some(live_generation),
            "draining remaining jobs must publish the live generation",
        )?;
        ensure(
            search_result_snapshot(&index_dir, "coalesced gamma committed after snapshot", 10)?
                .iter()
                .any(|row| row.doc_id == gamma_memory_id),
            "gamma must be searchable after the bounded queue converges",
        )
    }

    #[test]
    fn coalesced_processor_publishes_empty_generation_after_last_document_tombstone() -> TestResult
    {
        let root = unique_test_dir("coalesced-empty-generation");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let root = std::fs::canonicalize(&root).map_err(|error| error.to_string())?;
        let index_dir = root.join("index");
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_012345678901234567890123e1";
        let memory_id = "mem_012345678901234567890123e1";
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: root.to_string_lossy().into_owned(),
                    name: Some("coalesced empty generation".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        insert_snapshot_test_memory_job(
            &connection,
            workspace_id,
            memory_id,
            "sidx_012345678901234567890123e1",
            "last searchable document before tombstone",
        )?;
        process_index_job_for_connection(
            &connection,
            "sidx_012345678901234567890123e1",
            &index_dir,
        )
        .map_err(|error| error.to_string())?;
        ensure(
            vector_index_snapshot(&index_dir)?.len() == 1,
            "seed generation must contain the live document",
        )?;

        ensure(
            connection
                .tombstone_memory(memory_id)
                .map_err(|error| error.to_string())?,
            "the final live document must be tombstoned",
        )?;
        connection
            .insert_search_index_job(
                "sidx_012345678901234567890123e2",
                &crate::db::CreateSearchIndexJobInput {
                    workspace_id: workspace_id.to_owned(),
                    job_type: SearchIndexJobType::SingleDocument,
                    document_source: Some("memory".to_owned()),
                    document_id: Some(memory_id.to_owned()),
                    documents_total: 0,
                },
            )
            .map_err(|error| error.to_string())?;

        let reports =
            process_pending_index_jobs_coalesced(&connection, workspace_id, &index_dir, None)
                .map_err(|error| error.to_string())?;
        ensure(
            reports.len() == 1
                && reports[0].outcome == "completed"
                && reports[0].documents_total == 0,
            format!("empty-corpus coalesced publication did not complete: {reports:?}"),
        )?;
        let published_generation = read_index_metadata(&index_dir)
            .generation
            .ok_or_else(|| "empty published generation missing".to_owned())?;
        let live_generation = connection
            .get_workspace_generation(workspace_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "empty live generation missing".to_owned())?;
        ensure(
            published_generation == live_generation,
            format!(
                "empty index manifest must represent the tombstone generation: published={published_generation:?} live={live_generation:?}"
            ),
        )?;
        ensure(
            vector_index_snapshot(&index_dir)?.is_empty(),
            "empty generation must remove the final vector document",
        )?;
        ensure(
            search_result_snapshot(&index_dir, "last searchable document", 10)?.is_empty(),
            "empty generation must remove the final lexical document",
        )
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
        write_index_metadata(&incremental_dir, 1, 2, None).map_err(|error| error.to_string())?;

        let beta_outcome = apply_incremental_index_change_sync(
            &incremental_dir,
            hash_fallback_embedder_stack(),
            "doc-beta",
            Some(test_indexable_doc("doc-beta", "beta updated notes")),
            2,
            IndexDocumentCounts::memory_only(2),
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
            IndexDocumentCounts::memory_only(3),
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
            IndexDocumentCounts::memory_only(2),
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
        write_index_metadata(&full_dir, 4, 2, None).map_err(|error| error.to_string())?;

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

        let (_, _, generation) = get_db_stats(&connection, "wsp_01234567890123456789012345")
            .map_err(|error| error.to_string())?;
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

        let (_, _, generation_before) = get_db_stats(&connection, "wsp_22222222222222222222222222")
            .map_err(|error| error.to_string())?;
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

        let (_, _, generation_after) = get_db_stats(&connection, "wsp_22222222222222222222222222")
            .map_err(|error| error.to_string())?;
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
            rules_indexed: 0,
            evidence_indexed: 0,
            documents_embedded: 0,
            documents_total: 10,
            index_dir: PathBuf::from("/tmp/index"),
            elapsed_ms: 123.4,
            dry_run: false,
            idempotency_key: "blake3:test".to_owned(),
            evidence_admission: EvidenceAdmissionReport::default(),
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
        assert_eq!(json["rules_indexed"], 0);
        assert_eq!(json["evidence_indexed"], 0);
        assert_eq!(json["documents_embedded"], 0);
        assert_eq!(json["documents_total"], 10);
        assert_eq!(
            json["evidenceAdmissionTotals"],
            serde_json::json!({"admitted": 0, "quarantined": 0, "denied": 0})
        );
        assert_eq!(json["dry_run"], false);
    }

    #[test]
    fn reembed_idempotency_key_covers_rule_and_evidence_membership() -> TestResult {
        let base = IndexDocumentCounts::checked(2, 1, 0, 0, 0)?;
        let with_rule = IndexDocumentCounts::checked(2, 1, 0, 1, 0)?;
        let with_evidence = IndexDocumentCounts::checked(2, 1, 0, 0, 1)?;
        let first = reembed_idempotency_key("wsp-test", "fast", Some("quality"), base);
        ensure(
            first == reembed_idempotency_key("wsp-test", "fast", Some("quality"), base),
            "identical reembed inputs must have a stable idempotency key",
        )?;
        ensure(
            first != reembed_idempotency_key("wsp-test", "fast", Some("quality"), with_rule)
                && first
                    != reembed_idempotency_key("wsp-test", "fast", Some("quality"), with_evidence),
            "rule and evidence membership changes must invalidate reembed idempotency",
        )
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
        ensure(report.documents_total == 4, "dry-run document count")?;
        ensure(
            report.memories_indexed == 1
                && report.sessions_indexed == 1
                && report.rules_indexed == 1
                && report.evidence_indexed == 1,
            format!("dry-run must count the complete corpus: {report:?}"),
        )?;
        ensure(
            EvidenceAdmissionTotals::from_report(&report.evidence_admission).admitted == 1,
            "dry-run must expose admitted evidence totals",
        )?;
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
            workspace_path: workspace.clone(),
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
        ensure(report.documents_total == 4, "document count")?;
        ensure(report.documents_embedded == 4, "embedded document count")?;
        ensure(
            report.memories_indexed == 1
                && report.sessions_indexed == 1
                && report.rules_indexed == 1
                && report.evidence_indexed == 1,
            format!("reembed must publish the complete corpus: {report:?}"),
        )?;
        ensure(
            EvidenceAdmissionTotals::from_report(&report.evidence_admission).admitted == 1,
            "reembed must expose admitted evidence totals",
        )?;
        ensure(
            report.embedding.posture.vector_coverage == EmbeddingVectorCoverage::new(4, 4),
            "published vector coverage",
        )?;
        ensure(
            index_dir.join(INDEX_METADATA_FILE).is_file(),
            "reembed should publish index metadata",
        )?;

        let job_id = report
            .job_id
            .ok_or_else(|| "job id should be present".to_string())?;
        let connection = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        let job = connection
            .get_search_index_job(&job_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "stored reembed job should exist".to_string())?;
        ensure(job.status == "completed", "stored job status")?;
        ensure(job.job_type == "full_rebuild", "stored job type")?;
        ensure(job.document_source.is_none(), "stored document source")?;
        ensure(job.documents_total == 4, "stored documents_total")?;
        ensure(job.documents_indexed == 4, "stored documents_indexed")?;
        ensure(
            vector_index_snapshot(&index_dir)?.len() == 4,
            "reembed must persist vectors for every source class",
        )?;
        connection.close().map_err(|e| e.to_string())?;

        let status = get_index_status(&IndexStatusOptions {
            workspace_path: workspace,
            database_path: Some(database),
            index_dir: Some(index_dir),
        })
        .map_err(|error| error.to_string())?;
        ensure(
            status.health == IndexHealth::Ready
                && status.db_memory_count == 1
                && status.db_session_count == 1
                && status.db_rule_count == 1
                && status.db_evidence_count == 1
                && status.db_evidence_admitted_count == 1
                && status.db_evidence_quarantined_count == 0
                && status.db_evidence_denied_count == 0
                && status.index_document_count == Some(4)
                && status.index_document_counts == IndexDocumentCounts::checked(1, 1, 0, 1, 1).ok(),
            format!("status must report exact complete-corpus counts: {status:?}"),
        )
    }

    #[test]
    fn index_reembed_report_refreshes_posture_after_registry_write() -> TestResult {
        let root = unique_test_dir("reembed-refreshes-registry-posture");
        let workspace = root.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        seed_reembed_database(&workspace, &database)?;

        let options = IndexReembedOptions {
            workspace_path: workspace,
            database_path: Some(database),
            index_dir: Some(index_dir),
            dry_run: false,
        };
        let stack = EmbedderStack::from_parts(
            Arc::new(TestSemanticEmbedder::new(POTION_MODEL_NAME, 256))
                as Arc<dyn crate::search::Embedder>,
            None,
        );
        let report = crate::core::run_cli_with_cx(Duration::from_secs(300), |cx| async move {
            reembed_index_with_cx_and_stack(&cx, &options, stack).await
        })
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;

        ensure(
            report.status == IndexReembedStatus::Success,
            format!("unexpected semantic reembed status: {:?}", report.status),
        )?;
        ensure(
            report.embedding.semantic,
            "semantic reembed must report semantic=true",
        )?;
        ensure(
            report.embedding.source == "registry_observed",
            format!(
                "semantic reembed must report the registry source written by the same operation: {}",
                report.embedding.source
            ),
        )?;
        ensure(
            report.embedding.registered_model_count == 1
                && report.embedding.available_model_count == 1,
            format!(
                "semantic reembed must report one available registry model: {:?}",
                report.embedding
            ),
        )?;
        ensure(
            report
                .embedding
                .selected_registry_model
                .as_ref()
                .is_some_and(|model| model.model_name == POTION_MODEL_NAME),
            "semantic reembed must identify the selected registry model",
        )?;
        ensure(
            report.embedding.posture.mode == EMBEDDING_POSTURE_MODE_NEURAL_LOCAL,
            "semantic reembed must retain the neural_local posture mode",
        )?;
        ensure(
            report.embedding.posture.vector_coverage == EmbeddingVectorCoverage::new(4, 4),
            "semantic reembed must report published vector coverage",
        )
    }

    fn cancellation_test_embedder_stack() -> EmbedderStack {
        EmbedderStack::from_parts(
            Arc::new(HashEmbedder::default_256()) as Arc<dyn crate::search::Embedder>,
            None,
        )
    }

    fn reembed_index_with_test_stack(
        options: &IndexReembedOptions,
    ) -> Result<IndexReembedReport, IndexRebuildError> {
        crate::core::run_cli_with_cx(Duration::from_secs(300), |cx| async move {
            reembed_index_with_cx_and_stack(&cx, options, cancellation_test_embedder_stack()).await
        })
        .map_err(|error| {
            IndexRebuildError::Index(format!("Failed to start index runtime: {error}"))
        })?
    }

    #[test]
    fn lab_runtime_cancellation_after_build_preserves_active_index_and_job_state() -> TestResult {
        let root = unique_test_dir("reembed-cancel-before-publish");
        let workspace = root.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        seed_reembed_database(&workspace, &database)?;

        reembed_index_with_test_stack(&IndexReembedOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database.clone()),
            index_dir: Some(index_dir.clone()),
            dry_run: false,
        })
        .map_err(|error| format!("build baseline re-embedding index: {error}"))?;
        let baseline = index_regular_file_snapshot(&index_dir)?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_01234567890123456789012346",
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_01234567890123456789012345".to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "This newer row must not appear in a cancelled index generation."
                        .to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some("test://cancel-before-index-publish".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: Some("unit-test".to_owned()),
                    tags: vec!["cancellation".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let expected_message = "caller cancelled after index build before publication";
        install_before_index_publish_hook(move |cx| {
            cx.set_cancel_reason(asupersync::CancelReason::user(expected_message));
        });

        let options = IndexReembedOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database.clone()),
            index_dir: Some(index_dir.clone()),
            dry_run: false,
        };
        let observation: Arc<Mutex<Option<Result<asupersync::CancelReason, String>>>> =
            Arc::new(Mutex::new(None));
        let task_observation = Arc::clone(&observation);
        let mut lab =
            asupersync::LabRuntime::new(asupersync::LabConfig::new(0xEE_90D).max_steps(256));
        let lab_root = lab.state.create_root_region(asupersync::Budget::INFINITE);
        let (task_id, _handle) = lab
            .state
            .create_task(lab_root, asupersync::Budget::INFINITE, async move {
                let result = if let Some(cx) = asupersync::Cx::current() {
                    match reembed_index_with_cx_and_stack(
                        &cx,
                        &options,
                        cancellation_test_embedder_stack(),
                    )
                    .await
                    {
                        Err(IndexRebuildError::Cancelled(reason)) => Ok(reason),
                        Err(error) => Err(format!(
                            "pre-publication cancellation must remain typed, got {error:?}"
                        )),
                        Ok(report) => Err(format!(
                            "cancelled re-embedding unexpectedly published: {report:?}"
                        )),
                    }
                } else {
                    Err("LabRuntime re-embedding task did not install a Cx".to_owned())
                };
                if let Ok(mut slot) = task_observation.lock() {
                    *slot = Some(result);
                }
                asupersync::Outcome::<(), String>::Ok(())
            })
            .map_err(|error| format!("create pre-publication cancellation task: {error}"))?;
        lab.scheduler.lock().schedule(task_id, 0);

        let report = lab.run_until_quiescent_with_report();
        ensure(
            report.quiescent,
            "pre-publication cancellation LabRuntime must quiesce",
        )?;
        ensure(
            report.invariant_violations.is_empty(),
            format!(
                "pre-publication cancellation must preserve LabRuntime invariants: {:?}",
                report.invariant_violations
            ),
        )?;
        let reason = observation
            .lock()
            .map_err(|_| "pre-publication cancellation observation poisoned".to_owned())?
            .take()
            .ok_or_else(|| "pre-publication cancellation observation missing".to_owned())??;
        ensure(
            reason.kind == asupersync::CancelKind::User,
            format!(
                "unexpected pre-publication cancellation kind: {:?}",
                reason.kind
            ),
        )?;
        ensure(
            reason.message.as_deref() == Some(expected_message),
            format!(
                "unexpected pre-publication cancellation message: {:?}",
                reason.message
            ),
        )?;
        ensure(
            index_regular_file_snapshot(&index_dir)? == baseline,
            "cancellation after validation must leave every active index file byte-identical",
        )?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let jobs = connection
            .list_search_index_jobs("wsp_01234567890123456789012345", None)
            .map_err(|error| error.to_string())?;
        let cancelled_jobs = jobs
            .iter()
            .filter(|job| job.status_enum() == Some(SearchIndexJobStatus::Cancelled))
            .collect::<Vec<_>>();
        ensure(
            cancelled_jobs.len() == 1,
            format!("expected exactly one cancelled re-embedding job: {jobs:?}"),
        )?;
        ensure(
            cancelled_jobs[0].completed_at.is_some(),
            "cancelled re-embedding job must have a completion timestamp",
        )?;
        ensure(
            connection
                .list_active_advisory_locks()
                .map_err(|error| error.to_string())?
                .is_empty(),
            "pre-publication cancellation must release the index advisory lock",
        )?;
        connection.close().map_err(|error| error.to_string())
    }

    /// bd-1oep7: a consolidation-apply index job cancelled immediately before
    /// index publication must leave the active generation untouched, report
    /// honest staleness (no false Ready), and converge exactly once when the
    /// next workflow-emitted job is processed.
    #[test]
    fn consolidation_index_job_cancel_before_publish_stays_truthful_and_retry_converges()
    -> TestResult {
        const WORKSPACE_ID: &str = "wsp_consolidx00000000000000001";
        const SURVIVOR_ID: &str = "mem_consolidx00000000000000001";
        const DUPLICATE_ID: &str = "mem_consolidx00000000000000002";
        const BASELINE_JOB_ID: &str = "sidx_consolbase0000000000000001";
        const CANCELLED_JOB_ID: &str = "sidx_consolcancel00000000000002";

        let root = unique_test_dir("consolidation-cancel-before-publish");
        let workspace = root.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        std::fs::create_dir_all(workspace.join(".ee")).map_err(|error| error.to_string())?;

        let insert_memory = |connection: &DbConnection,
                             memory_id: &str,
                             content: &str,
                             confidence: f32|
         -> TestResult {
            connection
                .insert_memory(
                    memory_id,
                    &crate::db::CreateMemoryInput {
                        workspace_id: WORKSPACE_ID.to_owned(),
                        level: "semantic".to_owned(),
                        kind: "fact".to_owned(),
                        content: content.to_owned(),
                        workflow_id: None,
                        confidence,
                        utility: 0.5,
                        importance: 0.5,
                        provenance_uri: Some("test://consolidation-index-cancel".to_owned()),
                        trust_class: "agent_validated".to_owned(),
                        trust_subclass: None,
                        tags: vec!["consolidation".to_owned()],
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())
        };
        let consolidation_job_input = || crate::db::CreateSearchIndexJobInput {
            workspace_id: WORKSPACE_ID.to_owned(),
            job_type: SearchIndexJobType::SingleDocument,
            document_source: Some("memory".to_owned()),
            document_id: Some(DUPLICATE_ID.to_owned()),
            documents_total: 1,
        };
        let processing_options = || IndexProcessingOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database.clone()),
            index_dir: Some(index_dir.clone()),
            dry_run: false,
            job_limit: None,
        };
        let status_options = || IndexStatusOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database.clone()),
            index_dir: Some(index_dir.clone()),
        };

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                WORKSPACE_ID,
                &crate::db::CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("consolidation-index-cancel".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        insert_memory(
            &connection,
            SURVIVOR_ID,
            "Zephyr quill consolidation survivor stays in the index.",
            0.92,
        )?;
        insert_memory(
            &connection,
            DUPLICATE_ID,
            " zephyr   quill consolidation survivor stays in the index. ",
            0.41,
        )?;
        connection
            .insert_search_index_job(
                BASELINE_JOB_ID,
                &crate::db::CreateSearchIndexJobInput {
                    workspace_id: WORKSPACE_ID.to_owned(),
                    job_type: SearchIndexJobType::FullRebuild,
                    document_source: None,
                    document_id: None,
                    documents_total: 0,
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let baseline =
            process_index_jobs(&processing_options()).map_err(|error| error.to_string())?;
        ensure(
            baseline.completed_jobs == 1,
            format!("baseline rebuild must complete: {baseline:?}"),
        )?;
        let baseline_status =
            get_index_status(&status_options()).map_err(|error| error.to_string())?;
        ensure(
            baseline_status
                .index_document_counts
                .as_ref()
                .map(|counts| counts.memories)
                == Some(2),
            format!(
                "baseline index must hold both duplicates: {:?}",
                baseline_status.index_document_counts
            ),
        )?;
        ensure(
            baseline_status.db_generation == baseline_status.index_generation,
            "baseline generation must be truthful before consolidation",
        )?;

        // Consolidation-apply effect: duplicate tombstoned, workflow-emitted
        // single-document job pending.
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        ensure(
            connection
                .tombstone_memory(DUPLICATE_ID)
                .map_err(|error| error.to_string())?,
            "duplicate must tombstone for the consolidation scenario",
        )?;
        connection
            .insert_search_index_job(CANCELLED_JOB_ID, &consolidation_job_input())
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        install_before_index_publish_hook(move |cx| {
            cx.set_cancel_reason(asupersync::CancelReason::user(
                "cancel consolidation index publication",
            ));
        });
        let cancel_options = processing_options();
        let cancelled_run = crate::core::run_cli_future(async move {
            let cx = asupersync::Cx::for_testing();
            process_index_jobs_with_cx(&cx, &cancel_options).await
        })
        .map_err(|error| error.to_string())?;
        ensure(
            matches!(cancelled_run, Err(IndexRebuildError::Cancelled(_))),
            format!("cancel-before-publish must stay typed: {cancelled_run:?}"),
        )?;

        let stale_status =
            get_index_status(&status_options()).map_err(|error| error.to_string())?;
        ensure(
            stale_status
                .index_document_counts
                .as_ref()
                .map(|counts| counts.memories)
                == Some(2),
            format!(
                "cancelled publication must preserve the active generation: {:?}",
                stale_status.index_document_counts
            ),
        )?;
        ensure(
            stale_status.db_generation > stale_status.index_generation,
            format!(
                "cancelled publication must report honest staleness (no false Ready): db={:?} index={:?}",
                stale_status.db_generation, stale_status.index_generation
            ),
        )?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let cancelled_job = connection
            .get_search_index_job(CANCELLED_JOB_ID)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "cancelled consolidation job row missing".to_owned())?;
        ensure(
            cancelled_job.status_enum() == Some(SearchIndexJobStatus::Cancelled),
            format!("cancelled consolidation job must not report done: {cancelled_job:?}"),
        )?;

        connection.close().map_err(|error| error.to_string())?;

        // Retry through the PUBLIC path only: the next workflow-emitted
        // processing tick transitions the cancelled job back to pending as
        // the SAME logical job and converges — no clone rows, no id churn.
        let retry = process_index_jobs(&processing_options()).map_err(|error| error.to_string())?;
        ensure(
            retry.completed_jobs == 1,
            format!("public retry must requeue and process the cancelled job: {retry:?}"),
        )?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let requeued_job = connection
            .get_search_index_job(CANCELLED_JOB_ID)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "requeued job row missing".to_owned())?;
        ensure(
            requeued_job.status_enum() == Some(SearchIndexJobStatus::Completed),
            format!("the same logical job must converge to completed: {requeued_job:?}"),
        )?;
        ensure(
            requeued_job.completed_at.is_some(),
            format!("converged job must carry a completion timestamp: {requeued_job:?}"),
        )?;
        let document_jobs = connection
            .list_search_index_jobs(WORKSPACE_ID, None)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|job| job.document_id.as_deref() == Some(DUPLICATE_ID))
            .count();
        ensure(
            document_jobs == 1,
            format!("requeue must never mint duplicate job rows: {document_jobs}"),
        )?;
        connection.close().map_err(|error| error.to_string())?;
        let converged = get_index_status(&status_options()).map_err(|error| error.to_string())?;
        ensure(
            converged
                .index_document_counts
                .as_ref()
                .map(|counts| counts.memories)
                == Some(1),
            format!(
                "retry must drop the absorbed duplicate exactly once: {:?}",
                converged.index_document_counts
            ),
        )?;
        ensure(
            converged.db_generation == converged.index_generation,
            format!(
                "retry must restore truthful generation: db={:?} index={:?}",
                converged.db_generation, converged.index_generation
            ),
        )?;

        // Idempotency: a further public tick finds nothing to requeue.
        let idle = process_index_jobs(&processing_options()).map_err(|error| error.to_string())?;
        ensure(
            idle.completed_jobs == 0 && idle.pending_jobs == 0,
            format!("requeue must be idempotent once converged: {idle:?}"),
        )?;
        Ok(())
    }

    #[test]
    fn public_index_tick_recovers_orphaned_running_job_without_stealing_live_owner() -> TestResult {
        const WORKSPACE_ID: &str = "wsp_01234567890123456789012345";
        const JOB_ID: &str = "sidx_orphanrunning0000000000000";

        let root = unique_test_dir("orphaned-running-index-job");
        let workspace = root.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        std::fs::create_dir_all(workspace.join(".ee")).map_err(|error| error.to_string())?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                WORKSPACE_ID,
                &crate::db::CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("orphaned-running-index-job".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_search_index_job(
                JOB_ID,
                &crate::db::CreateSearchIndexJobInput {
                    workspace_id: WORKSPACE_ID.to_owned(),
                    job_type: SearchIndexJobType::FullRebuild,
                    document_source: None,
                    document_id: None,
                    documents_total: 0,
                },
            )
            .map_err(|error| error.to_string())?;
        ensure(
            connection
                .start_search_index_job(JOB_ID)
                .map_err(|error| error.to_string())?,
            "fixture job must enter running state",
        )?;

        let lock_id = AdvisoryLockId::index(WORKSPACE_ID);
        let live_holder = generate_index_holder_id();
        ensure(
            connection
                .acquire_advisory_lock(
                    &lock_id,
                    &live_holder,
                    Some(INDEX_PUBLISH_LOCK_TTL_SECS),
                    Some("live owner fixture"),
                )
                .map_err(|error| error.to_string())?
                .is_acquired(),
            "fixture owner must hold the index publication lease",
        )?;

        let options = IndexProcessingOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database.clone()),
            index_dir: Some(index_dir.clone()),
            dry_run: false,
            job_limit: None,
        };
        let protected = process_index_jobs(&options).map_err(|error| error.to_string())?;
        ensure(
            protected.pending_jobs == 0 && protected.completed_jobs == 0,
            format!("a live owner must keep its running job protected: {protected:?}"),
        )?;
        let still_running = connection
            .get_search_index_job(JOB_ID)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "protected running job disappeared".to_owned())?;
        ensure(
            still_running.status_enum() == Some(SearchIndexJobStatus::Running),
            format!("live-owned job must remain running: {still_running:?}"),
        )?;

        ensure(
            connection
                .release_advisory_lock(&lock_id, &live_holder)
                .map_err(|error| error.to_string())?,
            "fixture owner must release its publication lease",
        )?;
        let dead_holder = "index:2147483647:orphaned-worker";
        ensure(
            connection
                .acquire_advisory_lock(
                    &lock_id,
                    dead_holder,
                    Some(INDEX_PUBLISH_LOCK_TTL_SECS),
                    Some("orphaned owner fixture"),
                )
                .map_err(|error| error.to_string())?
                .is_acquired(),
            "fixture must retain the crashed owner's durable lease row",
        )?;
        let recovered = process_index_jobs(&options).map_err(|error| error.to_string())?;
        ensure(
            recovered.pending_jobs == 1 && recovered.completed_jobs == 1,
            format!("the next public tick must recover the orphaned row: {recovered:?}"),
        )?;
        let completed = connection
            .get_search_index_job(JOB_ID)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "recovered index job disappeared".to_owned())?;
        ensure(
            completed.id == JOB_ID
                && completed.status_enum() == Some(SearchIndexJobStatus::Completed),
            format!("recovery must complete the same durable job ID: {completed:?}"),
        )?;
        let jobs = connection
            .list_search_index_jobs(WORKSPACE_ID, None)
            .map_err(|error| error.to_string())?;
        ensure(
            jobs.len() == 1 && jobs[0].id == JOB_ID,
            format!("orphan recovery must not mint a replacement row: {jobs:?}"),
        )?;

        connection.close().map_err(|error| error.to_string())
    }

    /// bd-1oep7: a cancellation injected immediately AFTER the consolidation
    /// index generation is durably published must never fake state in either
    /// direction: the published index stays current and truthful, the job row
    /// lands in a truthful terminal state (done because the work really
    /// finished, or cancelled because bookkeeping aborted — never a leaked
    /// `running` row and never `failed`), and a retry converges with no
    /// duplicate index effects.
    #[test]
    fn consolidation_index_job_cancel_after_publish_stays_truthful_and_retry_converges()
    -> TestResult {
        const WORKSPACE_ID: &str = "wsp_consolpost0000000000000001";
        const SURVIVOR_ID: &str = "mem_consolpost0000000000000001";
        const DUPLICATE_ID: &str = "mem_consolpost0000000000000002";
        const BASELINE_JOB_ID: &str = "sidx_consolpostbase000000000001";
        const CANCELLED_JOB_ID: &str = "sidx_consolpostcancel0000000002";

        let root = unique_test_dir("consolidation-cancel-after-publish");
        let workspace = root.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        std::fs::create_dir_all(workspace.join(".ee")).map_err(|error| error.to_string())?;

        let insert_memory = |connection: &DbConnection,
                             memory_id: &str,
                             content: &str,
                             confidence: f32|
         -> TestResult {
            connection
                .insert_memory(
                    memory_id,
                    &crate::db::CreateMemoryInput {
                        workspace_id: WORKSPACE_ID.to_owned(),
                        level: "semantic".to_owned(),
                        kind: "fact".to_owned(),
                        content: content.to_owned(),
                        workflow_id: None,
                        confidence,
                        utility: 0.5,
                        importance: 0.5,
                        provenance_uri: Some("test://consolidation-index-post".to_owned()),
                        trust_class: "agent_validated".to_owned(),
                        trust_subclass: None,
                        tags: vec!["consolidation".to_owned()],
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())
        };
        let consolidation_job_input = || crate::db::CreateSearchIndexJobInput {
            workspace_id: WORKSPACE_ID.to_owned(),
            job_type: SearchIndexJobType::SingleDocument,
            document_source: Some("memory".to_owned()),
            document_id: Some(DUPLICATE_ID.to_owned()),
            documents_total: 1,
        };
        let processing_options = || IndexProcessingOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database.clone()),
            index_dir: Some(index_dir.clone()),
            dry_run: false,
            job_limit: None,
        };
        let status_options = || IndexStatusOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database.clone()),
            index_dir: Some(index_dir.clone()),
        };

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                WORKSPACE_ID,
                &crate::db::CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("consolidation-index-post-cancel".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        insert_memory(
            &connection,
            SURVIVOR_ID,
            "Zephyr quill post-publish survivor stays in the index.",
            0.92,
        )?;
        insert_memory(
            &connection,
            DUPLICATE_ID,
            " zephyr   quill post-publish survivor stays in the index. ",
            0.41,
        )?;
        connection
            .insert_search_index_job(
                BASELINE_JOB_ID,
                &crate::db::CreateSearchIndexJobInput {
                    workspace_id: WORKSPACE_ID.to_owned(),
                    job_type: SearchIndexJobType::FullRebuild,
                    document_source: None,
                    document_id: None,
                    documents_total: 0,
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let baseline =
            process_index_jobs(&processing_options()).map_err(|error| error.to_string())?;
        ensure(
            baseline.completed_jobs == 1,
            format!("baseline rebuild must complete: {baseline:?}"),
        )?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        ensure(
            connection
                .tombstone_memory(DUPLICATE_ID)
                .map_err(|error| error.to_string())?,
            "duplicate must tombstone for the post-publish scenario",
        )?;
        connection
            .insert_search_index_job(CANCELLED_JOB_ID, &consolidation_job_input())
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        install_after_index_publish_hook(move |cx| {
            cx.set_cancel_reason(asupersync::CancelReason::user(
                "cancel immediately after consolidation index publication",
            ));
        });
        let cancel_options = processing_options();
        let cancelled_run = crate::core::run_cli_future(async move {
            let cx = asupersync::Cx::for_testing();
            process_index_jobs_with_cx(&cx, &cancel_options).await
        })
        .map_err(|error| error.to_string())?;
        ensure(
            matches!(cancelled_run, Ok(_) | Err(IndexRebuildError::Cancelled(_))),
            format!(
                "post-publish cancellation must surface as clean completion or a typed cancellation, never another failure: {cancelled_run:?}"
            ),
        )?;

        // The publication itself is durable and truthful regardless of where
        // the cancellation interrupted bookkeeping.
        let published = get_index_status(&status_options()).map_err(|error| error.to_string())?;
        ensure(
            published
                .index_document_counts
                .as_ref()
                .map(|counts| counts.memories)
                == Some(1),
            format!(
                "post-publish cancellation must keep the published deduplicated generation: {:?}",
                published.index_document_counts
            ),
        )?;
        ensure(
            published.db_generation == published.index_generation,
            format!(
                "post-publish cancellation must leave a truthful current generation: db={:?} index={:?}",
                published.db_generation, published.index_generation
            ),
        )?;

        // The job row must land in a truthful terminal state: done (the work
        // really completed) or cancelled (bookkeeping aborted) — never a
        // leaked running row and never failed.
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let job = connection
            .get_search_index_job(CANCELLED_JOB_ID)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "post-publish consolidation job row missing".to_owned())?;
        let job_status = job.status_enum();
        ensure(
            job_status == Some(SearchIndexJobStatus::Completed)
                || job_status == Some(SearchIndexJobStatus::Cancelled),
            format!("post-publish job must be truthfully terminal: {job:?}"),
        )?;
        ensure(
            job.completed_at.is_some(),
            format!("post-publish terminal job must carry a completion timestamp: {job:?}"),
        )?;
        connection.close().map_err(|error| error.to_string())?;

        // Retry through the PUBLIC path only: the next workflow-emitted tick
        // transitions a cancelled job back to pending as the same logical
        // job (a completed one needs nothing) and converges with zero
        // duplicate index effects.
        let retry = process_index_jobs(&processing_options()).map_err(|error| error.to_string())?;
        let expected_retry_completions =
            u32::from(job_status == Some(SearchIndexJobStatus::Cancelled));
        ensure(
            retry.completed_jobs == expected_retry_completions,
            format!(
                "public retry must process exactly the requeued work (expected {expected_retry_completions}): {retry:?}"
            ),
        )?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let final_job = connection
            .get_search_index_job(CANCELLED_JOB_ID)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "post-publish job row missing after retry".to_owned())?;
        ensure(
            final_job.status_enum() == Some(SearchIndexJobStatus::Completed),
            format!("the same logical job must end completed after retry: {final_job:?}"),
        )?;
        let document_jobs = connection
            .list_search_index_jobs(WORKSPACE_ID, None)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|job| job.document_id.as_deref() == Some(DUPLICATE_ID))
            .count();
        ensure(
            document_jobs == 1,
            format!("retry must never mint duplicate job rows: {document_jobs}"),
        )?;
        connection.close().map_err(|error| error.to_string())?;
        let converged = get_index_status(&status_options()).map_err(|error| error.to_string())?;
        ensure(
            converged
                .index_document_counts
                .as_ref()
                .map(|counts| counts.memories)
                == Some(1),
            format!(
                "post-publish retry must not duplicate index effects: {:?}",
                converged.index_document_counts
            ),
        )?;
        ensure(
            converged.db_generation == converged.index_generation,
            format!(
                "post-publish retry must keep the generation truthful: db={:?} index={:?}",
                converged.db_generation, converged.index_generation
            ),
        )?;
        Ok(())
    }

    /// bd-1oep7: a NON-cancellation failure before index publication must
    /// leave a truthfully `failed` job row with its error message, and the
    /// next PUBLIC processing tick must requeue the same logical job and
    /// converge — failure recovery, not only cancellation recovery.
    #[test]
    fn consolidation_index_job_failure_before_publish_recovers_via_public_retry() -> TestResult {
        const WORKSPACE_ID: &str = "wsp_consolfail0000000000000001";
        const SURVIVOR_ID: &str = "mem_consolfail0000000000000001";
        const DUPLICATE_ID: &str = "mem_consolfail0000000000000002";
        const BASELINE_JOB_ID: &str = "sidx_consolfailbase000000000001";
        const FAILED_JOB_ID: &str = "sidx_consolfailwork000000000002";

        let root = unique_test_dir("consolidation-failure-before-publish");
        let workspace = root.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        std::fs::create_dir_all(workspace.join(".ee")).map_err(|error| error.to_string())?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                WORKSPACE_ID,
                &crate::db::CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("consolidation-failure-before-publish".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        for (memory_id, content, confidence) in [
            (
                SURVIVOR_ID,
                "Zephyr quill failure-recovery survivor stays in the index.",
                0.92_f32,
            ),
            (
                DUPLICATE_ID,
                " zephyr   quill failure-recovery survivor stays in the index. ",
                0.41_f32,
            ),
        ] {
            connection
                .insert_memory(
                    memory_id,
                    &crate::db::CreateMemoryInput {
                        workspace_id: WORKSPACE_ID.to_owned(),
                        level: "semantic".to_owned(),
                        kind: "fact".to_owned(),
                        content: content.to_owned(),
                        workflow_id: None,
                        confidence,
                        utility: 0.5,
                        importance: 0.5,
                        provenance_uri: Some("test://consolidation-failure".to_owned()),
                        trust_class: "agent_validated".to_owned(),
                        trust_subclass: None,
                        tags: vec!["consolidation".to_owned()],
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())?;
        }
        connection
            .insert_search_index_job(
                BASELINE_JOB_ID,
                &crate::db::CreateSearchIndexJobInput {
                    workspace_id: WORKSPACE_ID.to_owned(),
                    job_type: SearchIndexJobType::FullRebuild,
                    document_source: None,
                    document_id: None,
                    documents_total: 0,
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let processing_options = || IndexProcessingOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database.clone()),
            index_dir: Some(index_dir.clone()),
            dry_run: false,
            job_limit: None,
        };
        let baseline =
            process_index_jobs(&processing_options()).map_err(|error| error.to_string())?;
        ensure(
            baseline.completed_jobs == 1,
            format!("failure-recovery baseline rebuild must complete: {baseline:?}"),
        )?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        ensure(
            connection
                .tombstone_memory(DUPLICATE_ID)
                .map_err(|error| error.to_string())?,
            "duplicate must tombstone for the failure-recovery scenario",
        )?;
        connection
            .insert_search_index_job(
                FAILED_JOB_ID,
                &crate::db::CreateSearchIndexJobInput {
                    workspace_id: WORKSPACE_ID.to_owned(),
                    job_type: SearchIndexJobType::SingleDocument,
                    document_source: Some("memory".to_owned()),
                    document_id: Some(DUPLICATE_ID.to_owned()),
                    documents_total: 1,
                },
            )
            .map_err(|error| error.to_string())?;

        // Inject a genuine (non-cancellation) failure after the job claim,
        // before publication.
        let failed_attempt = process_pending_index_jobs_coalesced_after_snapshot(
            &connection,
            WORKSPACE_ID,
            &index_dir,
            None,
            || {
                Err(IndexRebuildError::Index(
                    "injected pre-publish failure".to_owned(),
                ))
            },
        );
        ensure(
            failed_attempt.is_err(),
            format!("injected failure must surface: {failed_attempt:?}"),
        )?;
        let failed_job = connection
            .get_search_index_job(FAILED_JOB_ID)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "failed job row missing".to_owned())?;
        ensure(
            failed_job.status_enum() == Some(SearchIndexJobStatus::Failed),
            format!("pre-publish failure must leave a truthful failed row: {failed_job:?}"),
        )?;
        ensure(
            failed_job.error_message.is_some(),
            format!("failed row must carry its error message: {failed_job:?}"),
        )?;
        connection.close().map_err(|error| error.to_string())?;

        // Public retry: the next workflow-emitted tick requeues the same
        // logical job and converges truthfully.
        let retry = process_index_jobs(&processing_options()).map_err(|error| error.to_string())?;
        ensure(
            retry.completed_jobs == 1,
            format!("public retry must requeue and complete the failed job: {retry:?}"),
        )?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let recovered = connection
            .get_search_index_job(FAILED_JOB_ID)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "recovered job row missing".to_owned())?;
        ensure(
            recovered.status_enum() == Some(SearchIndexJobStatus::Completed),
            format!("the same logical job must recover to completed: {recovered:?}"),
        )?;
        connection.close().map_err(|error| error.to_string())?;
        let status_options = IndexStatusOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database.clone()),
            index_dir: Some(index_dir.clone()),
        };
        let converged = get_index_status(&status_options).map_err(|error| error.to_string())?;
        ensure(
            converged
                .index_document_counts
                .as_ref()
                .map(|counts| counts.memories)
                == Some(1)
                && converged.db_generation == converged.index_generation,
            format!(
                "failure recovery must converge to a truthful index: memories={:?} db={:?} index={:?}",
                converged.index_document_counts,
                converged.db_generation,
                converged.index_generation
            ),
        )?;
        Ok(())
    }

    /// bd-1oep7: two concurrent public processing ticks racing over one
    /// cancelled consolidation job must converge on exactly one completion
    /// of the SAME logical job — the atomic requeue UPDATE plus the publish
    /// lock forbid double-processing and duplicate rows.
    #[test]
    fn consolidation_requeue_public_retry_is_concurrency_safe() -> TestResult {
        const WORKSPACE_ID: &str = "wsp_consolconc0000000000000001";
        const SURVIVOR_ID: &str = "mem_consolconc0000000000000001";
        const DUPLICATE_ID: &str = "mem_consolconc0000000000000002";
        const BASELINE_JOB_ID: &str = "sidx_consolconcbase000000000001";
        const CANCELLED_JOB_ID: &str = "sidx_consolconcwork000000000002";

        let root = unique_test_dir("consolidation-requeue-concurrency");
        let workspace = root.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        std::fs::create_dir_all(workspace.join(".ee")).map_err(|error| error.to_string())?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                WORKSPACE_ID,
                &crate::db::CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("consolidation-requeue-concurrency".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        for (memory_id, content, confidence) in [
            (
                SURVIVOR_ID,
                "Zephyr quill concurrency survivor stays in the index.",
                0.92_f32,
            ),
            (
                DUPLICATE_ID,
                " zephyr   quill concurrency survivor stays in the index. ",
                0.41_f32,
            ),
        ] {
            connection
                .insert_memory(
                    memory_id,
                    &crate::db::CreateMemoryInput {
                        workspace_id: WORKSPACE_ID.to_owned(),
                        level: "semantic".to_owned(),
                        kind: "fact".to_owned(),
                        content: content.to_owned(),
                        workflow_id: None,
                        confidence,
                        utility: 0.5,
                        importance: 0.5,
                        provenance_uri: Some("test://consolidation-concurrency".to_owned()),
                        trust_class: "agent_validated".to_owned(),
                        trust_subclass: None,
                        tags: vec!["consolidation".to_owned()],
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())?;
        }
        connection
            .insert_search_index_job(
                BASELINE_JOB_ID,
                &crate::db::CreateSearchIndexJobInput {
                    workspace_id: WORKSPACE_ID.to_owned(),
                    job_type: SearchIndexJobType::FullRebuild,
                    document_source: None,
                    document_id: None,
                    documents_total: 0,
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let processing_options = || IndexProcessingOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database.clone()),
            index_dir: Some(index_dir.clone()),
            dry_run: false,
            job_limit: None,
        };
        let baseline =
            process_index_jobs(&processing_options()).map_err(|error| error.to_string())?;
        ensure(
            baseline.completed_jobs == 1,
            format!("concurrency baseline rebuild must complete: {baseline:?}"),
        )?;

        // Produce a genuinely cancelled consolidation job via the publish
        // seam, exactly like an interrupted workflow tick.
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        ensure(
            connection
                .tombstone_memory(DUPLICATE_ID)
                .map_err(|error| error.to_string())?,
            "duplicate must tombstone for the concurrency scenario",
        )?;
        connection
            .insert_search_index_job(
                CANCELLED_JOB_ID,
                &crate::db::CreateSearchIndexJobInput {
                    workspace_id: WORKSPACE_ID.to_owned(),
                    job_type: SearchIndexJobType::SingleDocument,
                    document_source: Some("memory".to_owned()),
                    document_id: Some(DUPLICATE_ID.to_owned()),
                    documents_total: 1,
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;
        install_before_index_publish_hook(move |cx| {
            cx.set_cancel_reason(asupersync::CancelReason::user(
                "cancel to seed the concurrency retry race",
            ));
        });
        let seed_options = processing_options();
        let seeded = crate::core::run_cli_future(async move {
            let cx = asupersync::Cx::for_testing();
            process_index_jobs_with_cx(&cx, &seed_options).await
        })
        .map_err(|error| error.to_string())?;
        ensure(
            matches!(seeded, Err(IndexRebuildError::Cancelled(_))),
            format!("concurrency seed must cancel before publication: {seeded:?}"),
        )?;

        // Two public retry ticks race; the atomic requeue + publish lock
        // must yield exactly one completion in total.
        let options_a = processing_options();
        let options_b = processing_options();
        let thread_a = std::thread::spawn(move || process_index_jobs(&options_a));
        let thread_b = std::thread::spawn(move || process_index_jobs(&options_b));
        let result_a = thread_a
            .join()
            .map_err(|_| "retry thread A panicked".to_owned())?
            .map_err(|error| error.to_string())?;
        let result_b = thread_b
            .join()
            .map_err(|_| "retry thread B panicked".to_owned())?
            .map_err(|error| error.to_string())?;
        let total_completed = result_a.completed_jobs + result_b.completed_jobs;
        ensure(
            total_completed == 1,
            format!(
                "concurrent public retries must complete the job exactly once: a={result_a:?} b={result_b:?}"
            ),
        )?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let final_job = connection
            .get_search_index_job(CANCELLED_JOB_ID)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "concurrency job row missing".to_owned())?;
        ensure(
            final_job.status_enum() == Some(SearchIndexJobStatus::Completed),
            format!("the same logical job must end completed exactly once: {final_job:?}"),
        )?;
        let document_jobs = connection
            .list_search_index_jobs(WORKSPACE_ID, None)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|job| job.document_id.as_deref() == Some(DUPLICATE_ID))
            .count();
        ensure(
            document_jobs == 1,
            format!("concurrent retries must never mint duplicate job rows: {document_jobs}"),
        )?;
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn backend_originated_cancellation_marks_live_cx_job_cancelled() -> TestResult {
        let root = unique_test_dir("backend-cancelled-job-finalizer");
        let workspace = root.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        seed_reembed_database(&workspace, &database)?;
        let stack = EmbedderStack::from_parts(
            Arc::new(BackendCancellingEmbedder) as Arc<dyn crate::search::Embedder>,
            None,
        );
        let options = IndexReembedOptions {
            workspace_path: workspace,
            database_path: Some(database.clone()),
            index_dir: Some(index_dir),
            dry_run: false,
        };
        let (result, caller_reason) = crate::core::run_cli_future(async move {
            let cx = asupersync::Cx::for_testing();
            let result = reembed_index_with_cx_and_stack(&cx, &options, stack).await;
            (result, cx.cancel_reason())
        })
        .map_err(|error| error.to_string())?;
        ensure(
            caller_reason.is_none(),
            "backend-only cancellation must leave the caller Cx live",
        )?;
        let reason = match result {
            Err(IndexRebuildError::Cancelled(reason)) => reason,
            Err(error) => return Err(format!("backend cancellation was laundered: {error:?}")),
            Ok(report) => {
                return Err(format!(
                    "backend cancellation unexpectedly returned a report: {report:?}"
                ));
            }
        };
        ensure(
            reason.kind == asupersync::CancelKind::PollQuota,
            format!("backend cancellation lost its structured kind: {reason:?}"),
        )?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let jobs = connection
            .list_search_index_jobs("wsp_01234567890123456789012345", None)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|job| job.job_type_enum() == Some(SearchIndexJobType::FullRebuild))
            .collect::<Vec<_>>();
        ensure(
            jobs.len() == 1,
            format!("production reembed should create one terminal job: {jobs:?}"),
        )?;
        let job = &jobs[0];
        ensure(
            job.status_enum() == Some(SearchIndexJobStatus::Cancelled),
            format!("backend cancellation left false job status: {job:?}"),
        )?;
        ensure(
            job.completed_at.is_some() && job.error_message.is_none(),
            "cancelled production job must be terminal without a failure error",
        )
    }

    #[cfg(unix)]
    #[test]
    fn index_reembed_marks_job_failed_when_recovery_rejects_active_symlink() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = unique_test_dir("reembed-recovery-failure");
        let workspace = root.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        let outside = root.join("outside-index");
        seed_reembed_database(&workspace, &database)?;
        std::fs::create_dir_all(&outside).map_err(|error| error.to_string())?;
        symlink(&outside, &index_dir).map_err(|error| error.to_string())?;

        let error = reembed_index(&IndexReembedOptions {
            workspace_path: workspace,
            database_path: Some(database.clone()),
            index_dir: Some(index_dir),
            dry_run: false,
        })
        .expect_err("symlinked active index must reject re-embedding");
        ensure(
            error.to_string().contains("symlinked index path component"),
            format!("unexpected reembed failure: {error}"),
        )?;

        let connection = DbConnection::open_file(database).map_err(|e| e.to_string())?;
        let jobs = connection
            .list_search_index_jobs("wsp_01234567890123456789012345", None)
            .map_err(|e| e.to_string())?;
        ensure(
            jobs.len() == 1
                && jobs[0].status == SearchIndexJobStatus::Failed.as_str()
                && jobs[0].completed_at.is_some(),
            format!("publication failure must leave one terminal failed job: {jobs:?}"),
        )?;
        connection.close().map_err(|error| error.to_string())
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
    fn coalesced_processing_dry_run_reports_selection_without_coalesced_claim() -> TestResult {
        let root = unique_test_dir("process-coalesced-dry-run");
        let workspace = root.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        seed_reembed_database(&workspace, &database)?;
        queue_pending_index_job(&database, "sidx_coalesceddryrun00000000000")?;

        let report = process_index_jobs_coalesced(&IndexProcessingOptions {
            workspace_path: workspace,
            database_path: Some(database),
            index_dir: Some(index_dir.clone()),
            dry_run: true,
            job_limit: Some(1),
        })
        .map_err(|e| e.to_string())?;
        ensure(
            report.status == IndexProcessingStatus::DryRun,
            "coalesced dry-run status",
        )?;
        ensure(
            report.jobs.len() == 1,
            "coalesced dry-run planned job count",
        )?;
        ensure(
            report.jobs[0].outcome == "planned" && report.jobs[0].processing_mode == "full_rebuild",
            "dry-run must use ordinary selection labels instead of claiming a coalesced build",
        )?;
        ensure(
            !report.durable_mutation(),
            "coalesced dry-run must not claim a durable mutation",
        )?;
        ensure(
            !index_dir.join(INDEX_METADATA_FILE).exists(),
            "coalesced dry-run must not publish index metadata",
        )
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
        ensure(job.documents_total == 4, "stored documents_total")?;
        ensure(job.documents_indexed == 4, "stored documents_indexed")?;
        ensure(job.started_at.is_some(), "stored job started timestamp")?;
        ensure(job.completed_at.is_some(), "stored job completed timestamp")?;
        connection.close().map_err(|e| e.to_string())
    }

    #[test]
    fn coalesced_processing_report_binds_two_jobs_and_then_reports_no_pending() -> TestResult {
        let root = unique_test_dir("process-coalesced-report");
        let workspace = root.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        let first_job_id = "sidx_processcoalesce00000000000";
        let second_job_id = "sidx_processcoalesce10000000000";
        seed_reembed_database(&workspace, &database)?;
        queue_pending_index_job(&database, first_job_id)?;
        queue_pending_index_job(&database, second_job_id)?;

        let options = IndexProcessingOptions {
            workspace_path: workspace,
            database_path: Some(database),
            index_dir: Some(index_dir.clone()),
            dry_run: false,
            job_limit: Some(2),
        };
        let report = process_index_jobs_coalesced(&options).map_err(|e| e.to_string())?;
        ensure(
            report.status == IndexProcessingStatus::Success,
            format!(
                "unexpected coalesced processing status: {:?}",
                report.status
            ),
        )?;
        ensure(report.pending_jobs == 2, "coalesced pending job count")?;
        ensure(report.processed_jobs == 2, "coalesced processed job count")?;
        ensure(report.completed_jobs == 2, "coalesced completed job count")?;
        ensure(report.failed_jobs == 0, "coalesced failed job count")?;
        ensure(
            report.durable_mutation(),
            "completed coalesced jobs are durable mutations",
        )?;
        ensure(report.jobs.len() == 2, "coalesced per-job report count")?;
        for expected_id in [first_job_id, second_job_id] {
            let job = report
                .jobs
                .iter()
                .find(|job| job.job_id == expected_id)
                .ok_or_else(|| format!("coalesced report omitted {expected_id}"))?;
            ensure(job.outcome == "completed", "coalesced job outcome")?;
            ensure(
                job.processing_mode == "coalesced_full_rebuild",
                "coalesced job processing mode",
            )?;
            ensure(job.documents_total == 4, "coalesced document total")?;
            ensure(job.documents_indexed == 4, "coalesced indexed total")?;
        }
        ensure(
            index_dir.join(INDEX_METADATA_FILE).is_file(),
            "coalesced processor should publish index metadata",
        )?;

        let repeated = process_index_jobs_coalesced(&options).map_err(|e| e.to_string())?;
        ensure(
            repeated.status == IndexProcessingStatus::NoPendingJobs,
            format!("repeat should report no pending work: {repeated:?}"),
        )?;
        ensure(repeated.pending_jobs == 0, "repeat pending job count")?;
        ensure(repeated.processed_jobs == 0, "repeat processed job count")?;
        ensure(
            !repeated.durable_mutation(),
            "no-pending repeat is not a durable mutation",
        )?;
        ensure(
            repeated.jobs.is_empty(),
            "repeat per-job report must be empty",
        )
    }

    #[test]
    fn bounded_coalesced_repair_rechecks_corpus_after_preflight_growth() -> TestResult {
        let root = unique_test_dir("bounded-coalesced-post-preflight-growth");
        let workspace = root.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        let job_id = "sidx_boundedcoalesced0000000000";
        seed_reembed_database(&workspace, &database)?;
        queue_pending_index_job(&database, job_id)?;

        let connection = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        let (preflight_counts, _) =
            current_index_corpus_counts(&connection, "wsp_01234567890123456789012345")
                .map_err(|e| e.to_string())?;
        ensure(
            preflight_counts.total() == 4,
            "bounded fixture preflight corpus count",
        )?;
        connection
            .insert_memory(
                "mem_boundedgrowth00000000000000",
                &crate::db::CreateMemoryInput {
                    workspace_id: "wsp_01234567890123456789012345".to_owned(),
                    level: "episodic".to_owned(),
                    kind: "fact".to_owned(),
                    content: "A concurrent writer enlarged the corpus after preflight.".to_owned(),
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some("file://bounded-race-fixture".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: Some("unit-test".to_owned()),
                    tags: vec!["bounded-race".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|e| e.to_string())?;
        let selected = connection
            .list_pending_search_index_jobs("wsp_01234567890123456789012345", Some(1))
            .map_err(|e| e.to_string())?;
        let reports = process_selected_index_jobs_coalesced_bounded(
            &connection,
            "wsp_01234567890123456789012345",
            &index_dir,
            selected,
            preflight_counts.total(),
        )
        .map_err(|e| e.to_string())?;
        ensure(
            reports.len() == 1 && reports[0].outcome == "skipped",
            format!("bounded repair must skip the enlarged corpus: {reports:?}"),
        )?;
        ensure(
            reports[0]
                .error
                .as_deref()
                .is_some_and(|error| error.contains("above the interactive limit of 4")),
            "bounded repair skip reason",
        )?;
        let stored = connection
            .get_search_index_job(job_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "bounded repair job disappeared".to_owned())?;
        ensure(
            stored.status_enum() == Some(SearchIndexJobStatus::Pending),
            "bounded repair must leave deferred work pending",
        )?;
        ensure(
            !index_dir.join(INDEX_METADATA_FILE).exists(),
            "bounded repair must not publish an oversized corpus",
        )?;
        connection.close().map_err(|e| e.to_string())
    }

    #[test]
    fn coalesced_selected_batch_reports_mid_claim_race_as_partial_failure() -> TestResult {
        let root = unique_test_dir("process-coalesced-claim-race");
        let workspace = root.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        let raced_job_id = "sidx_coalescerace00000000000000";
        let completed_job_id = "sidx_coalescerace10000000000000";
        seed_reembed_database(&workspace, &database)?;
        queue_pending_index_job(&database, raced_job_id)?;
        queue_pending_index_job(&database, completed_job_id)?;

        let connection = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        let selected = connection
            .list_pending_search_index_jobs("wsp_01234567890123456789012345", Some(2))
            .map_err(|e| e.to_string())?;
        ensure(selected.len() == 2, "claim-race selected batch size")?;
        ensure(
            connection
                .start_search_index_job(raced_job_id)
                .map_err(|e| e.to_string())?,
            "claim-race fixture must claim the first selected job",
        )?;

        let reports = process_selected_index_jobs_coalesced(
            &connection,
            "wsp_01234567890123456789012345",
            &index_dir,
            selected,
        )
        .map_err(|e| e.to_string())?;
        ensure(reports.len() == 2, "claim-race report count")?;
        let raced = reports
            .iter()
            .find(|job| job.job_id == raced_job_id)
            .ok_or_else(|| "claim-race skipped report missing".to_owned())?;
        ensure(raced.outcome == "skipped", "claim-race skipped outcome")?;
        ensure(
            raced.error.as_deref() == Some("search index job was not pending"),
            "claim-race skipped reason",
        )?;
        let completed = reports
            .iter()
            .find(|job| job.job_id == completed_job_id)
            .ok_or_else(|| "claim-race completed report missing".to_owned())?;
        ensure(
            completed.outcome == "completed",
            "claim-race peer job outcome",
        )?;

        let (processed_jobs, completed_jobs, failed_jobs, status) =
            summarize_index_processing_jobs(2, &reports);
        ensure(processed_jobs == 2, "claim-race accounted job count")?;
        ensure(completed_jobs == 1, "claim-race completed job count")?;
        ensure(failed_jobs == 1, "claim-race failed job count")?;
        ensure(
            status == IndexProcessingStatus::PartialFailure,
            "claim-race summary must not report success",
        )?;
        connection.close().map_err(|e| e.to_string())
    }

    #[test]
    fn publish_lock_contention_leaves_ordinary_and_coalesced_jobs_directly_retryable() -> TestResult
    {
        let root = unique_test_dir("publish-lock-preclaim-contention");
        let workspace = root.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        const WORKSPACE_ID: &str = "wsp_01234567890123456789012345";
        const ORDINARY_JOB_ID: &str = "sidx_preclaimordinary000000000";
        const COALESCED_JOB_ID: &str = "sidx_preclaimcoalesced0000000";
        seed_reembed_database(&workspace, &database)?;
        queue_pending_index_job(&database, ORDINARY_JOB_ID)?;

        let connection = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        let publish_lock = AdvisoryLockId::index(WORKSPACE_ID);
        let fixture_holder = "fixture-held-publish-lock";
        let held = connection
            .acquire_advisory_lock(
                &publish_lock,
                fixture_holder,
                Some(60),
                Some("pre-claim cancellation fixture"),
            )
            .map_err(|e| e.to_string())?;
        ensure(held.is_acquired(), "fixture holds publish lock")?;

        let ordinary = connection
            .get_search_index_job(ORDINARY_JOB_ID)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "ordinary pre-claim job missing".to_owned())?;
        INDEX_PUBLISH_LOCK_RETRY_ATTEMPTS_OVERRIDE.with(|slot| slot.set(Some(1)));
        let ordinary_result = process_one_index_job(&connection, &ordinary, &index_dir);
        ensure(
            matches!(ordinary_result, Err(IndexRebuildError::LockContention(_))),
            format!("ordinary lock contention must remain typed: {ordinary_result:?}"),
        )?;
        let ordinary_after = connection
            .get_search_index_job(ORDINARY_JOB_ID)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "ordinary pre-claim job disappeared".to_owned())?;
        ensure(
            ordinary_after.status_enum() == Some(SearchIndexJobStatus::Pending),
            format!(
                "ordinary publish-lock contention must leave the job retryable: {ordinary_after:?}"
            ),
        )?;

        assert!(
            connection
                .release_advisory_lock(&publish_lock, fixture_holder)
                .map_err(|e| e.to_string())?,
            "release ordinary publish-lock fixture"
        );
        let ordinary_retry = process_one_index_job(&connection, &ordinary, &index_dir)
            .map_err(|e| format!("ordinary direct retry failed: {e}"))?;
        ensure(
            ordinary_retry.outcome == "completed",
            "ordinary direct retry completes the original logical job",
        )?;
        let ordinary_completed = connection
            .get_search_index_job(ORDINARY_JOB_ID)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "ordinary retried job disappeared".to_owned())?;
        ensure(
            ordinary_completed.status_enum() == Some(SearchIndexJobStatus::Completed),
            format!("ordinary direct retry must complete the same row: {ordinary_completed:?}"),
        )?;

        queue_pending_index_job(&database, COALESCED_JOB_ID)?;
        let selected = connection
            .list_pending_search_index_jobs(WORKSPACE_ID, None)
            .map_err(|e| e.to_string())?;
        ensure(selected.len() == 1, "coalesced pre-claim selected set")?;
        let held = connection
            .acquire_advisory_lock(
                &publish_lock,
                fixture_holder,
                Some(60),
                Some("coalesced pre-claim contention fixture"),
            )
            .map_err(|e| e.to_string())?;
        ensure(held.is_acquired(), "fixture re-holds publish lock")?;
        let coalesced_result = process_selected_index_jobs_coalesced(
            &connection,
            WORKSPACE_ID,
            &index_dir,
            selected.clone(),
        );
        ensure(
            matches!(coalesced_result, Err(IndexRebuildError::LockContention(_))),
            format!("coalesced lock contention must remain typed: {coalesced_result:?}"),
        )?;
        let coalesced_after = connection
            .get_search_index_job(COALESCED_JOB_ID)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "coalesced pre-claim job disappeared".to_owned())?;
        ensure(
            coalesced_after.status_enum() == Some(SearchIndexJobStatus::Pending),
            format!(
                "coalesced publish-lock contention must leave the job retryable: {coalesced_after:?}"
            ),
        )?;
        assert!(
            connection
                .release_advisory_lock(&publish_lock, fixture_holder)
                .map_err(|e| e.to_string())?,
            "release coalesced publish-lock fixture"
        );
        let coalesced_retry =
            process_selected_index_jobs_coalesced(&connection, WORKSPACE_ID, &index_dir, selected)
                .map_err(|e| format!("coalesced direct retry failed: {e}"))?;
        ensure(
            coalesced_retry.len() == 1 && coalesced_retry[0].outcome == "completed",
            format!(
                "coalesced direct retry completes the original logical job: {coalesced_retry:?}"
            ),
        )?;
        let coalesced_completed = connection
            .get_search_index_job(COALESCED_JOB_ID)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "coalesced retried job disappeared".to_owned())?;
        ensure(
            coalesced_completed.status_enum() == Some(SearchIndexJobStatus::Completed),
            format!("coalesced direct retry must complete the same row: {coalesced_completed:?}"),
        )?;
        ensure(
            connection
                .list_pending_search_index_jobs(WORKSPACE_ID, None)
                .map_err(|e| e.to_string())?
                .is_empty(),
            "direct retries leave no pending tail",
        )?;
        ensure(
            connection
                .list_search_index_jobs(WORKSPACE_ID, None)
                .map_err(|e| e.to_string())?
                .len()
                == 2,
            "direct retries create no duplicate logical jobs",
        )?;
        INDEX_PUBLISH_LOCK_RETRY_ATTEMPTS_OVERRIDE.with(|slot| slot.set(None));
        connection.close().map_err(|e| e.to_string())
    }

    #[test]
    fn coalesced_selected_batch_all_skipped_fails_without_claiming_mutation() -> TestResult {
        let root = unique_test_dir("process-coalesced-all-skipped");
        let workspace = root.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        let first_job_id = "sidx_coalesceskip00000000000000";
        let second_job_id = "sidx_coalesceskip10000000000000";
        seed_reembed_database(&workspace, &database)?;
        queue_pending_index_job(&database, first_job_id)?;
        queue_pending_index_job(&database, second_job_id)?;

        let connection = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        let selected = connection
            .list_pending_search_index_jobs("wsp_01234567890123456789012345", Some(2))
            .map_err(|e| e.to_string())?;
        ensure(selected.len() == 2, "all-skipped selected batch size")?;
        for job_id in [first_job_id, second_job_id] {
            ensure(
                connection
                    .start_search_index_job(job_id)
                    .map_err(|e| e.to_string())?,
                format!("all-skipped fixture must pre-claim {job_id}"),
            )?;
        }

        let reports = process_selected_index_jobs_coalesced(
            &connection,
            "wsp_01234567890123456789012345",
            &index_dir,
            selected,
        )
        .map_err(|e| e.to_string())?;
        ensure(
            reports.len() == 2 && reports.iter().all(|job| job.outcome == "skipped"),
            format!("all pre-claimed jobs must report skipped: {reports:?}"),
        )?;
        let (processed_jobs, completed_jobs, failed_jobs, status) =
            summarize_index_processing_jobs(2, &reports);
        ensure(processed_jobs == 2, "all-skipped accounted job count")?;
        ensure(completed_jobs == 0, "all-skipped completed job count")?;
        ensure(failed_jobs == 2, "all-skipped failed job count")?;
        ensure(
            status == IndexProcessingStatus::Failed,
            "all-skipped summary must fail",
        )?;
        ensure(
            !index_processing_jobs_have_durable_mutation(&reports),
            "all-skipped batch must not claim a durable mutation",
        )?;
        ensure(
            !index_dir.join(INDEX_METADATA_FILE).exists(),
            "all-skipped batch must not publish an index",
        )?;
        connection.close().map_err(|e| e.to_string())
    }

    #[test]
    fn index_processing_summary_fails_closed_on_unreported_selected_jobs() -> TestResult {
        let (processed_jobs, completed_jobs, failed_jobs, status) =
            summarize_index_processing_jobs(2, &[]);
        ensure(processed_jobs == 2, "unreported jobs accounted count")?;
        ensure(completed_jobs == 0, "unreported jobs completed count")?;
        ensure(failed_jobs == 2, "unreported jobs failed count")?;
        ensure(
            status == IndexProcessingStatus::Failed,
            "unreported selected jobs must fail closed",
        )
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
        let health = determine_health(false, 0, Some(10), Some(10), true, false, false);
        assert_eq!(health, IndexHealth::Missing);
        assert_eq!(health.degradation_code(), Some("index_missing"));
    }

    #[test]
    fn cache_invalidation_empty_index_detected() {
        let health = determine_health(true, 0, Some(10), Some(10), true, false, false);
        assert_eq!(health, IndexHealth::Missing);
    }

    #[test]
    fn cache_invalidation_stale_when_db_ahead() {
        let health = determine_health(true, 5, Some(12), Some(9), true, false, false);
        assert_eq!(health, IndexHealth::Stale);
        assert_eq!(health.degradation_code(), Some("index_stale"));
    }

    #[test]
    fn cache_invalidation_stale_when_index_has_no_generation() {
        let health = determine_health(true, 5, Some(12), None, true, false, false);
        assert_eq!(health, IndexHealth::Stale);
    }

    #[test]
    fn cache_invalidation_corrupt_when_metadata_parse_fails() {
        let health = determine_health(true, 5, Some(12), None, true, true, false);
        assert_eq!(health, IndexHealth::Corrupt);
        assert_eq!(health.degradation_code(), Some("index_corrupt"));
    }

    #[test]
    fn cache_invalidation_ready_when_generations_match() {
        let health = determine_health(true, 5, Some(10), Some(10), true, false, false);
        assert_eq!(health, IndexHealth::Ready);
        assert_eq!(health.degradation_code(), None);
    }

    #[test]
    fn cache_invalidation_ready_when_index_ahead() {
        let health = determine_health(true, 5, Some(8), Some(10), true, false, false);
        assert_eq!(health, IndexHealth::Ready);
    }

    #[test]
    fn cache_invalidation_ready_when_no_generations_tracked() {
        let health = determine_health(true, 5, None, None, true, false, false);
        assert_eq!(health, IndexHealth::Ready);
    }

    #[test]
    fn cache_invalidation_ready_when_db_has_no_generation() {
        let health = determine_health(true, 5, None, Some(10), true, false, false);
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
            db_artifact_count: 2,
            db_rule_count: 3,
            db_evidence_count: 4,
            db_evidence_admitted_count: 4,
            db_evidence_quarantined_count: 1,
            db_evidence_denied_count: 2,
            db_generation: Some(12),
            index_generation: Some(9),
            expected_corpus_revision: "blake3:expected".to_owned(),
            actual_corpus_revision: Some("blake3:legacy".to_owned()),
            index_document_count: Some(20),
            index_document_counts: None,
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
        assert_eq!(json["dbArtifactCount"], 2);
        assert_eq!(json["dbRuleCount"], 3);
        assert_eq!(json["dbEvidenceCount"], 4);
        assert_eq!(json["dbEvidenceAdmittedCount"], 4);
        assert_eq!(json["dbEvidenceQuarantinedCount"], 1);
        assert_eq!(json["dbEvidenceDeniedCount"], 2);
        assert_eq!(json["expectedCorpusRevision"], "blake3:expected");
        assert_eq!(json["actualCorpusRevision"], "blake3:legacy");
        assert_eq!(json["indexDocumentCount"], 20);
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
            db_artifact_count: 2,
            db_rule_count: 3,
            db_evidence_count: 4,
            db_evidence_admitted_count: 4,
            db_evidence_quarantined_count: 1,
            db_evidence_denied_count: 2,
            db_generation: Some(12),
            index_generation: Some(9),
            expected_corpus_revision: "blake3:expected".to_owned(),
            actual_corpus_revision: Some("blake3:legacy".to_owned()),
            index_document_count: Some(20),
            index_document_counts: None,
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
        assert!(summary.contains("DB artifacts: 2"));
        assert!(summary.contains("DB rules: 3"));
        assert!(summary.contains("DB evidence: 4"));
        assert!(summary.contains("Evidence admitted/quarantined/denied: 4/1/2"));
        assert!(summary.contains("Expected corpus revision: blake3:expected"));
        assert!(summary.contains("Actual corpus revision: blake3:legacy"));
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
            let health = determine_health(
                true,
                1,
                Some(generation),
                Some(generation),
                true,
                false,
                false,
            );
            assert_eq!(
                health,
                IndexHealth::Ready,
                "generation {generation} should be ready"
            );
        }
    }

    #[test]
    fn cache_invalidation_boundary_condition_db_one_ahead() {
        let health = determine_health(true, 1, Some(1), Some(0), true, false, false);
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
        write_index_metadata(&staging_dir, 2, 1, None).map_err(|e| e.to_string())?;

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
    fn publish_commit_failure_restores_active_and_preserves_staging_generation() -> TestResult {
        let root = unique_test_dir("publish-commit-rollback");
        let index_dir = root.join("index");
        let staging_dir = root.join(".index.publish-test");
        write_marker(&index_dir, "generation.txt", "old")?;
        write_marker(&staging_dir, "generation.txt", "new")?;
        write_index_metadata(&staging_dir, 2, 1, None).map_err(|error| error.to_string())?;

        let error = match publish_staged_index_with_commit(&index_dir, &staging_dir, || {
            Err(IndexRebuildError::Index(
                "simulated database commit failure".to_owned(),
            ))
        }) {
            Ok(()) => return Err("unexpected publish success".to_owned()),
            Err(error) => error,
        };

        ensure(
            error
                .to_string()
                .contains("simulated database commit failure"),
            format!("unexpected error: {error}"),
        )?;
        ensure(
            index_dir.is_dir(),
            "previous active index should be restored",
        )?;
        ensure(
            read_marker(&index_dir, "generation.txt")? == "old",
            "commit failure must leave the previous generation active",
        )?;
        ensure(
            !staging_dir.exists(),
            "uncommitted generation must leave the recoverable staging namespace",
        )?;
        let rejected = rejected_generation_dirs(&root, "index")?;
        ensure(
            rejected.len() == 1,
            format!("expected one quarantined generation, found {rejected:?}"),
        )?;
        ensure(
            read_marker(&rejected[0], "generation.txt")? == "new",
            "rollback must preserve the quarantined generation for inspection",
        )?;
        ensure(
            !root.join("index.previous").exists(),
            "restored previous generation should no longer occupy the retained path",
        )
    }

    #[test]
    fn publish_commit_panic_rolls_back_before_unwind_escapes() -> TestResult {
        let root = unique_test_dir("publish-commit-panic-rollback");
        let index_dir = root.join("index");
        let staging_dir = root.join(".index.publish-test");
        write_marker(&index_dir, "generation.txt", "old")?;
        write_marker(&staging_dir, "generation.txt", "new")?;
        write_index_metadata(&staging_dir, 2, 1, None).map_err(|error| error.to_string())?;

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = publish_staged_index_with_commit(&index_dir, &staging_dir, || {
                panic!("simulated commit-tail panic")
            });
        }));
        ensure(
            unwind.is_err(),
            "commit-tail panic must escape after rollback",
        )?;
        ensure(
            read_marker(&index_dir, "generation.txt")? == "old",
            "panic rollback must restore the previous active generation",
        )?;
        let rejected = rejected_generation_dirs(&root, "index")?;
        ensure(
            rejected.len() == 1 && read_marker(&rejected[0], "generation.txt")? == "new",
            format!("panic rollback must quarantine the uncommitted generation: {rejected:?}"),
        )?;
        ensure(
            !root.join("index.previous").exists(),
            "panic rollback must consume the retained generation path",
        )
    }

    #[test]
    fn rejected_first_publish_is_not_recovered_as_committed_generation() -> TestResult {
        let root = unique_test_dir("publish-first-commit-rollback");
        let index_dir = root.join("index");
        let staging_dir = root.join(".index.publish-test");
        build_current_test_index(
            &staging_dir,
            2,
            vec![test_indexable_doc("mem-rejected", "uncommitted generation")],
        )?;
        write_marker(&staging_dir, "generation.txt", "rejected")?;

        let error = publish_staged_index_with_commit(&index_dir, &staging_dir, || {
            Err(IndexRebuildError::Index(
                "simulated first database commit failure".to_owned(),
            ))
        })
        .expect_err("first publication commit should fail");
        ensure(
            error
                .to_string()
                .contains("simulated first database commit failure"),
            format!("unexpected error: {error}"),
        )?;
        ensure(
            !index_dir.exists(),
            "failed first publication must leave the active index absent",
        )?;
        ensure(
            !staging_dir.exists(),
            "rejected first publication must leave the recoverable staging namespace",
        )?;
        let rejected = rejected_generation_dirs(&root, "index")?;
        ensure(
            rejected.len() == 1 && read_marker(&rejected[0], "generation.txt")? == "rejected",
            format!("rejected generation was not preserved: {rejected:?}"),
        )?;

        let action = recover_interrupted_publish(&index_dir).map_err(|error| error.to_string())?;
        ensure(
            action == IndexPublishRecoveryAction::NoRecoverableGeneration,
            format!("recovery promoted an uncommitted generation: {action:?}"),
        )?;
        ensure(
            !index_dir.exists() && rejected[0].is_dir(),
            "recovery must keep active absent and preserve the rejected generation",
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
        write_index_metadata(&staging_dir, 2, 1, None).map_err(|e| e.to_string())?;
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
        build_current_test_index(
            &retained_dir,
            2,
            vec![test_indexable_doc("mem-retained", "retained generation")],
        )?;
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
    fn recover_interrupted_publish_restores_newest_retained_generation() -> TestResult {
        let root = unique_test_dir("recover-newest-retained");
        let index_dir = root.join("index");
        let stale_retained = root.join("index.previous");
        let newest_retained = root.join("index.previous.001");
        build_current_test_index(
            &stale_retained,
            41,
            vec![test_indexable_doc("mem-stale", "stale retained generation")],
        )?;
        write_marker(&stale_retained, "generation.txt", "stale")?;
        build_current_test_index(
            &newest_retained,
            42,
            vec![test_indexable_doc(
                "mem-newest",
                "newest retained generation",
            )],
        )?;
        write_marker(&newest_retained, "generation.txt", "newest")?;

        let action = recover_interrupted_publish(&index_dir).map_err(|error| error.to_string())?;

        ensure(
            action == IndexPublishRecoveryAction::RetainedGenerationRestored,
            format!("unexpected recovery action: {action:?}"),
        )?;
        ensure(
            read_marker(&index_dir, "generation.txt")? == "newest",
            "crash recovery must restore the highest source generation",
        )?;
        ensure(
            stale_retained.is_dir(),
            "older retained generations must remain available for vacuum inspection",
        )?;
        ensure(
            !newest_retained.exists(),
            "the selected retained generation should move into the active path",
        )
    }

    #[test]
    fn recover_interrupted_publish_promotes_complete_staging_generation() -> TestResult {
        let root = unique_test_dir("recover-staging");
        let index_dir = root.join("index");
        let staging_dir = root.join(".index.publish-20260501-000");
        build_current_test_index(
            &staging_dir,
            3,
            vec![test_indexable_doc("mem-staged", "staged generation")],
        )?;
        write_marker(&staging_dir, "generation.txt", "new")?;

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
    fn recover_interrupted_publish_refuses_legacy_retained_generation() -> TestResult {
        let root = unique_test_dir("recover-legacy-retained");
        let index_dir = root.join("index");
        let retained_dir = root.join("index.previous");
        build_current_test_index(
            &retained_dir,
            2,
            vec![test_indexable_doc("mem-retained", "retained generation")],
        )?;
        std::fs::write(
            retained_dir.join(INDEX_METADATA_FILE),
            r#"{"schema":"ee.index_metadata.v1","generation":2,"sourceGeneration":2,"documentCount":1}"#,
        )
        .map_err(|error| error.to_string())?;

        for attempt in 0..2 {
            let action =
                recover_interrupted_publish(&index_dir).map_err(|error| error.to_string())?;
            ensure(
                action == IndexPublishRecoveryAction::NoRecoverableGeneration,
                format!(
                    "restart {attempt} unexpectedly recovered legacy retained bytes: {action:?}"
                ),
            )?;
            ensure(
                !index_dir.exists(),
                "legacy retained generation must never become active",
            )?;
            ensure(
                retained_dir.is_dir(),
                "legacy retained generation must remain available for inspection",
            )?;
        }
        Ok(())
    }

    #[test]
    fn recover_interrupted_publish_refuses_wrong_revision_staging_generation() -> TestResult {
        let root = unique_test_dir("recover-wrong-revision-staging");
        let index_dir = root.join("index");
        let staging_dir = root.join(".index.publish-20260501-000");
        build_current_test_index(
            &staging_dir,
            3,
            vec![test_indexable_doc("mem-staged", "staged generation")],
        )?;
        let metadata_path = staging_dir.join(INDEX_METADATA_FILE);
        let mut metadata: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&metadata_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        metadata["corpusRevision"] = serde_json::json!("blake3:wrong");
        std::fs::write(
            &metadata_path,
            serde_json::to_vec_pretty(&metadata).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;

        let action = recover_interrupted_publish(&index_dir).map_err(|error| error.to_string())?;
        ensure(
            action == IndexPublishRecoveryAction::NoRecoverableGeneration,
            format!("wrong-revision staging bytes must not be promoted: {action:?}"),
        )?;
        ensure(
            !index_dir.exists(),
            "wrong-revision staging generation must not become active",
        )?;
        ensure(
            staging_dir.is_dir(),
            "wrong-revision staging generation must remain available for inspection",
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
        build_current_test_index(
            &index_dir,
            42,
            vec![test_indexable_doc("mem-roundtrip", "metadata roundtrip")],
        )?;
        let metadata_status = read_index_metadata(&index_dir);

        ensure(
            metadata_status.generation == Some(42),
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
            metadata_status.last_rebuild_at.is_some(),
            "metadata should include last rebuild timestamp",
        )?;
        ensure(
            metadata_status.compatibility_error.is_none()
                && metadata_status.corruption_error.is_none(),
            format!("metadata should not report check error: {metadata_status:?}"),
        )
    }

    #[test]
    fn corpus_revision_and_metadata_counts_are_deterministic() -> TestResult {
        let first = expected_index_corpus_revision();
        let second = expected_index_corpus_revision();
        ensure(
            std::ptr::eq(first, second),
            "corpus revision must be process-stable",
        )?;
        ensure(
            first
                .as_str()
                .strip_prefix("blake3:")
                .is_some_and(|digest| {
                    digest.len() == 64
                        && digest
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                }),
            format!("corpus revision must be a canonical BLAKE3 token: {first}"),
        )?;

        let index_dir = unique_test_dir("metadata-exact-counts");
        std::fs::create_dir_all(&index_dir).map_err(|error| error.to_string())?;
        let counts = IndexDocumentCounts::checked(2, 3, 5, 7, 11)?;
        write_index_metadata(&index_dir, 29, counts, None).map_err(|error| error.to_string())?;
        let metadata: serde_json::Value = serde_json::from_slice(
            &std::fs::read(index_dir.join(INDEX_METADATA_FILE))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;

        ensure(
            metadata["schema"] == INDEX_METADATA_SCHEMA_V2,
            "metadata schema must be v2",
        )?;
        ensure(
            metadata["corpusRevision"] == first.as_str(),
            "metadata must stamp the deterministic corpus revision",
        )?;
        ensure(
            metadata["evidenceSecurityPolicyEpoch"] == EVIDENCE_SECURITY_POLICY_EPOCH,
            "metadata must stamp the evidence security epoch",
        )?;
        ensure(
            metadata["documentCount"] == 28,
            "metadata total must equal the checked per-kind sum",
        )?;
        ensure(
            metadata["documentCounts"]
                == serde_json::json!({
                    "memories": 2,
                    "sessions": 3,
                    "artifacts": 5,
                    "rules": 7,
                    "evidence": 11,
                }),
            format!("metadata per-kind counts drifted: {metadata}"),
        )?;
        ensure(
            metadata["tierDocumentCounts"]["fast"] == 28
                && metadata["tierDocumentCounts"]["quality"].is_null(),
            "metadata must record exact fast and absent-quality tier counts",
        )?;
        ensure(
            if cfg!(feature = "lexical-bm25") {
                metadata["tierDocumentCounts"]["lexical"] == 28
            } else {
                metadata["tierDocumentCounts"]["lexical"].is_null()
            },
            "metadata lexical count must match the compiled tier posture",
        )
    }

    #[test]
    fn build_validation_rejects_any_partial_generation() -> TestResult {
        let index_dir = unique_test_dir("partial-generation-validation");
        let counts = IndexDocumentCounts::memory_only(2);
        let error = validate_built_generation(
            &index_dir,
            BuildStats {
                source_count: 2,
                doc_count: 1,
                error_count: 1,
                has_quality_index: true,
                errors: vec![("doc-beta".to_owned(), "fast tier: rejected".to_owned())],
            },
            counts,
        )
        .expect_err("partial generation must never be publishable");

        ensure(
            error.contains("refusing to publish incomplete index generation")
                && error.contains("doc-beta")
                && error.contains("fast-tier document count mismatch"),
            format!("partial-generation error must preserve every violation: {error}"),
        )
    }

    #[test]
    fn write_index_metadata_rejects_non_regular_metadata_path() -> TestResult {
        let root = unique_test_dir("metadata-write-directory");
        let index_dir = root.join("index");
        let metadata_dir = index_dir.join(INDEX_METADATA_FILE);
        std::fs::create_dir_all(&metadata_dir).map_err(|error| error.to_string())?;

        let error = write_index_metadata(&index_dir, 42, 7, None)
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
        build_current_test_index(
            &index_dir,
            42,
            vec![test_indexable_doc("mem-stale-temp", "stale temp metadata")],
        )?;
        let metadata_path = index_dir.join(INDEX_METADATA_FILE);
        let temp_path = metadata_path.with_extension("json.tmp");
        std::fs::write(&temp_path, "stale metadata temp").map_err(|error| error.to_string())?;

        write_index_metadata(&index_dir, 42, 1, None).map_err(|error| error.to_string())?;
        ensure(
            std::fs::read_to_string(&temp_path).map_err(|error| error.to_string())?
                == "stale metadata temp",
            "temporary metadata content must remain untouched",
        )?;
        ensure(
            metadata_path.is_file(),
            "metadata should publish through a unique temporary metadata file",
        )?;
        let metadata_status = read_index_metadata(&index_dir);
        ensure(
            metadata_status.generation == Some(42),
            "metadata generation should be readable after stale temp bypass",
        )?;
        ensure(
            metadata_status.last_rebuild_at.is_some(),
            "metadata rebuild timestamp should be readable after stale temp bypass",
        )?;
        ensure(
            metadata_status.compatibility_error.is_none()
                && metadata_status.corruption_error.is_none(),
            format!("metadata read should not report check error: {metadata_status:?}"),
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
    fn index_status_marks_legacy_metadata_as_stale() -> TestResult {
        let root = unique_test_dir("metadata-security-epoch-status");
        let index_dir = root.join("index");
        std::fs::create_dir_all(&index_dir).map_err(|error| error.to_string())?;
        std::fs::write(
            index_dir.join("meta.json"),
            r#"{"schema":"ee.index_metadata.v1","generation":0,"sourceGeneration":0,"documentCount":0}"#,
        )
        .map_err(|error| error.to_string())?;

        let report = get_index_status(&IndexStatusOptions {
            workspace_path: root.clone(),
            database_path: Some(root.join("missing.db")),
            index_dir: Some(index_dir),
        })
        .map_err(|error| error.to_string())?;

        ensure(
            report.health == IndexHealth::Stale,
            format!("legacy metadata must fail closed as stale: {report:?}"),
        )?;
        ensure(
            report.last_check_error.as_deref().is_some_and(|error| {
                error.contains("incompatible corpus revision")
                    && error.contains("full index rebuild is required")
            }),
            format!(
                "status must explain the incompatible corpus revision: {:?}",
                report.last_check_error
            ),
        )?;
        ensure(
            report.actual_corpus_revision.is_none(),
            "legacy metadata must expose a missing actual corpus revision",
        )?;
        ensure(
            report.expected_corpus_revision == expected_index_corpus_revision().as_str(),
            "status must expose the deterministic expected corpus revision",
        )?;
        ensure(
            report.repair_hint == Some("ee index rebuild --workspace ."),
            "status must provide an actionable rebuild repair",
        )
    }

    #[test]
    fn index_status_exposes_wrong_corpus_revision_and_rebuild_repair() -> TestResult {
        let root = unique_test_dir("metadata-wrong-revision-status");
        let index_dir = root.join("index");
        std::fs::create_dir_all(&index_dir).map_err(|error| error.to_string())?;
        write_index_metadata(&index_dir, 0, 0, None).map_err(|error| error.to_string())?;
        let metadata_path = index_dir.join(INDEX_METADATA_FILE);
        let mut metadata: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&metadata_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        metadata["corpusRevision"] = serde_json::json!("blake3:wrong");
        std::fs::write(
            &metadata_path,
            serde_json::to_vec_pretty(&metadata).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;

        let report = get_index_status(&IndexStatusOptions {
            workspace_path: root.clone(),
            database_path: Some(root.join("missing.db")),
            index_dir: Some(index_dir),
        })
        .map_err(|error| error.to_string())?;

        ensure(
            report.health == IndexHealth::Stale
                && report.actual_corpus_revision.as_deref() == Some("blake3:wrong")
                && report.expected_corpus_revision == expected_index_corpus_revision().as_str(),
            format!("status must expose the exact corpus mismatch: {report:?}"),
        )?;
        ensure(
            report
                .last_check_error
                .as_deref()
                .is_some_and(|error| error.contains("blake3:wrong")),
            "status must retain the actual wrong revision in its diagnostic",
        )?;
        ensure(
            report.repair_hint == Some("ee index rebuild --workspace ."),
            "wrong corpus revision must have an explicit full rebuild repair",
        )
    }

    #[test]
    fn index_status_never_reports_current_metadata_with_missing_tiers_ready() -> TestResult {
        let root = unique_test_dir("metadata-missing-tiers-status");
        let index_dir = root.join("index");
        std::fs::create_dir_all(&index_dir).map_err(|error| error.to_string())?;
        write_index_metadata(&index_dir, 0, 0, None).map_err(|error| error.to_string())?;

        let report = get_index_status(&IndexStatusOptions {
            workspace_path: root.clone(),
            database_path: Some(root.join("missing.db")),
            index_dir: Some(index_dir),
        })
        .map_err(|error| error.to_string())?;

        ensure(
            report.health == IndexHealth::Stale,
            format!("metadata without persisted tiers must be stale: {report:?}"),
        )?;
        ensure(
            report.last_check_error.as_deref().is_some_and(|error| {
                error.contains("persisted-tier verification")
                    && error.contains("no fast vector tier found")
                    && error.contains("full index rebuild is required")
            }),
            format!(
                "missing tiers must have a precise diagnostic: {:?}",
                report.last_check_error
            ),
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
        write_index_metadata(&index_dir, 0, 0, None).map_err(|error| error.to_string())?;

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
        write_index_metadata(&index_dir, 0, 0, None).map_err(|error| error.to_string())?;

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
    // ------------------------------------------------------------------
    // GH#19: index metadata embedder fingerprint (writer + FSVI backfill)
    // ------------------------------------------------------------------

    #[test]
    fn write_index_metadata_stamps_semantic_embedder_fingerprint() -> TestResult {
        let index_dir = unique_test_dir("meta-fingerprint-stamp");
        std::fs::create_dir_all(&index_dir).map_err(|e| e.to_string())?;

        let stack = EmbedderStack::from_parts(
            Arc::new(TestSemanticEmbedder::new("fixture-semantic-model", 256))
                as Arc<dyn crate::search::Embedder>,
            None,
        );
        let fingerprint = embedder_fingerprint_for_index_metadata(&stack)
            .ok_or_else(|| "ready semantic embedder must yield a fingerprint".to_owned())?;
        ensure(
            fingerprint.model_id == "fixture-semantic-model",
            "fingerprint model id",
        )?;
        ensure(fingerprint.dimension == 256, "fingerprint dimension")?;
        ensure(
            fingerprint.distance_metric == "cosine",
            "fingerprint distance metric",
        )?;
        ensure(
            fingerprint.vector_dtype == "float32",
            "fingerprint vector dtype",
        )?;
        ensure(
            fingerprint.model_hash.starts_with("blake3:"),
            "fingerprint content hash shape",
        )?;

        write_index_metadata(&index_dir, 7, 3, Some(&fingerprint))
            .map_err(|error| error.to_string())?;
        let raw = std::fs::read_to_string(index_dir.join(INDEX_METADATA_FILE))
            .map_err(|e| e.to_string())?;
        let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;

        ensure(
            parsed["schema"] == INDEX_METADATA_SCHEMA_V2,
            "metadata writer must stamp the current schema",
        )?;
        ensure(parsed["generation"] == 7, "generation")?;
        ensure(parsed["documentCount"] == 3, "document count")?;
        ensure(
            parsed["storedModelId"] == "fixture-semantic-model",
            format!("storedModelId must be stamped: {parsed}"),
        )?;
        ensure(
            parsed["storedDimension"] == 256,
            format!("storedDimension must be stamped: {parsed}"),
        )?;
        ensure(
            parsed["storedDistanceMetric"] == "cosine",
            "storedDistanceMetric must be stamped",
        )?;
        ensure(
            parsed["storedVectorDtype"] == "float32",
            "storedVectorDtype must be stamped",
        )?;
        ensure(
            parsed["storedModelRevision"].is_string(),
            "storedModelRevision must be stamped",
        )?;
        ensure(
            parsed["storedModelHash"]
                .as_str()
                .is_some_and(|hash| hash.starts_with("blake3:")),
            "storedModelHash must be stamped",
        )
    }

    #[test]
    fn embedder_fingerprint_absent_for_hash_fallback_and_unready_embedders() -> TestResult {
        ensure(
            embedder_fingerprint_for_index_metadata(&hash_fallback_embedder_stack()).is_none(),
            "hash fallback stack must not produce a semantic fingerprint",
        )?;
        let not_ready = EmbedderStack::from_parts(
            Arc::new(TestSemanticEmbedder::not_ready(
                "fixture-semantic-model",
                256,
            )) as Arc<dyn crate::search::Embedder>,
            None,
        );
        ensure(
            embedder_fingerprint_for_index_metadata(&not_ready).is_none(),
            "not-ready semantic embedder must not produce a fingerprint",
        )
    }

    #[test]
    fn write_index_metadata_without_fingerprint_omits_embedder_fields() -> TestResult {
        let index_dir = unique_test_dir("meta-fingerprint-absent");
        std::fs::create_dir_all(&index_dir).map_err(|e| e.to_string())?;
        write_index_metadata(&index_dir, 1, 1, None).map_err(|error| error.to_string())?;
        let raw = std::fs::read_to_string(index_dir.join(INDEX_METADATA_FILE))
            .map_err(|e| e.to_string())?;
        let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        ensure(
            parsed.get("storedDimension").is_none(),
            "no fingerprint keys without an embedder fingerprint",
        )?;
        ensure(
            parsed.get("storedModelId").is_none(),
            "no model id without an embedder fingerprint",
        )
    }

    #[test]
    fn read_fast_vector_index_fingerprint_reports_dimension_and_embedder_id() -> TestResult {
        let index_dir = unique_test_dir("fsvi-fingerprint-read");
        let stack = hash_fallback_embedder_stack();
        let expected_id = stack.fast().id().to_owned();
        let expected_dimension = u32::try_from(stack.fast().dimension()).map_err(|_| "dim")?;
        build_index_sync(
            &index_dir,
            stack,
            vec![test_indexable_doc("doc-alpha", "alpha content")],
        )?;

        let (dimension, embedder_id) = read_fast_vector_index_fingerprint(&index_dir)
            .ok_or_else(|| "fast vector fingerprint should be readable".to_owned())?;
        ensure(
            dimension == expected_dimension,
            format!("FSVI dimension: expected {expected_dimension}, got {dimension}"),
        )?;
        ensure(
            embedder_id == expected_id,
            format!("FSVI embedder id: expected {expected_id}, got {embedder_id}"),
        )
    }

    fn seed_lifecycle_backfill_workspace(
        label: &str,
        registry_model_name: &str,
        registry_dimension: u32,
    ) -> Result<(PathBuf, PathBuf), String> {
        let root = unique_test_dir(label);
        let workspace = root.join("workspace");
        std::fs::create_dir_all(workspace.join(".ee")).map_err(|e| e.to_string())?;
        let workspace = workspace.canonicalize().map_err(|e| e.to_string())?;
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");

        let connection = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        connection.migrate().map_err(|e| e.to_string())?;
        connection
            .insert_workspace(
                "wsp_backfill000000000000000000",
                &crate::db::CreateWorkspaceInput {
                    path: workspace.to_string_lossy().into_owned(),
                    name: Some("lifecycle-backfill".to_owned()),
                },
            )
            .map_err(|e| e.to_string())?;
        connection
            .insert_model_registry_entry(
                "mdl_backfill000000000000000000",
                &crate::db::CreateModelRegistryInput {
                    workspace_id: "wsp_backfill000000000000000000".to_owned(),
                    provider: ModelProvider::Hash,
                    model_name: registry_model_name.to_owned(),
                    purpose: crate::models::model_registry::ModelPurpose::Embedding,
                    dimension: Some(registry_dimension),
                    distance_metric: Some(ModelDistanceMetric::Cosine),
                    status: ModelRegistryStatus::Available,
                    version: Some("v1".to_owned()),
                    source_uri: None,
                    content_hash: None,
                    metadata_json: None,
                    last_checked_at: None,
                },
            )
            .map_err(|e| e.to_string())?;
        connection.close().map_err(|e| e.to_string())?;

        // Publish a real fast vector tier plus a LEGACY meta.json (no
        // storedDimension) — the exact on-disk state GH#19 reporters have.
        build_index_sync(
            &index_dir,
            hash_fallback_embedder_stack(),
            vec![test_indexable_doc("doc-alpha", "alpha content")],
        )?;
        write_index_metadata(&index_dir, 1, 1, None).map_err(|error| error.to_string())?;

        Ok((workspace, database))
    }

    #[test]
    fn legacy_meta_without_stored_dimension_backfills_from_fast_vector_header() -> TestResult {
        let stack = hash_fallback_embedder_stack();
        let fast_id = stack.fast().id().to_owned();
        let fast_dimension = u32::try_from(stack.fast().dimension()).map_err(|_| "dim")?;
        let (workspace, database) = seed_lifecycle_backfill_workspace(
            "lifecycle-backfill-match",
            &fast_id,
            fast_dimension,
        )?;

        let report = crate::core::model::build_model_lifecycle_report_for_workspace(
            &workspace,
            Some(&database),
            None,
        )
        .map_err(|error| format!("lifecycle report: {error:?}"))?;
        let index_row = report
            .indexes
            .first()
            .ok_or_else(|| "lifecycle report must include the index row".to_owned())?;

        ensure(
            index_row.stored_dimension == Some(fast_dimension),
            format!(
                "legacy index must backfill storedDimension from the FSVI header: {:?}",
                index_row.stored_dimension
            ),
        )?;
        ensure(
            index_row.stored_model_id.as_deref() == Some(fast_id.as_str()),
            "legacy index must backfill storedModelId from the FSVI header",
        )?;
        ensure(
            index_row.dimension_compatibility.compatible == Some(true),
            format!(
                "backfilled dimension must satisfy the compatibility rule: {:?}",
                index_row.dimension_compatibility
            ),
        )?;
        ensure(
            index_row
                .dimension_compatibility
                .mismatch_reason
                .as_deref()
                .is_none_or(|reason| !reason.contains("does not record a vector dimension")),
            "the stuck 'no vector dimension' reason must be gone",
        )
    }

    #[test]
    fn legacy_meta_backfill_skipped_when_registry_model_differs_from_fsvi() -> TestResult {
        let stack = hash_fallback_embedder_stack();
        let fast_dimension = u32::try_from(stack.fast().dimension()).map_err(|_| "dim")?;
        let (workspace, database) = seed_lifecycle_backfill_workspace(
            "lifecycle-backfill-mismatch",
            "some-other-semantic-model",
            fast_dimension,
        )?;

        let report = crate::core::model::build_model_lifecycle_report_for_workspace(
            &workspace,
            Some(&database),
            None,
        )
        .map_err(|error| format!("lifecycle report: {error:?}"))?;
        let index_row = report
            .indexes
            .first()
            .ok_or_else(|| "lifecycle report must include the index row".to_owned())?;

        ensure(
            index_row.stored_dimension.is_none(),
            format!(
                "an FSVI built by a different embedder must not backfill the registry model's dimension: {:?}",
                index_row.stored_dimension
            ),
        )
    }

    // ------------------------------------------------------------------
    // GH#20: --workspace plumbing for rebuild / reembed / process-jobs
    // ------------------------------------------------------------------

    fn seed_two_workspace_database(root: &Path) -> Result<(PathBuf, PathBuf, PathBuf), String> {
        let workspace_a = root.join("workspace-a");
        let workspace_b = root.join("workspace-b");
        std::fs::create_dir_all(&workspace_a).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&workspace_b).map_err(|e| e.to_string())?;
        let workspace_a = workspace_a.canonicalize().map_err(|e| e.to_string())?;
        let workspace_b = workspace_b.canonicalize().map_err(|e| e.to_string())?;
        let database = workspace_a.join(".ee").join("ee.db");
        std::fs::create_dir_all(database.parent().ok_or("db parent")?)
            .map_err(|e| e.to_string())?;

        let connection = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        connection.migrate().map_err(|e| e.to_string())?;
        connection
            .insert_workspace(
                "wsp_multia0000000000000000000a",
                &crate::db::CreateWorkspaceInput {
                    path: workspace_a.to_string_lossy().into_owned(),
                    name: Some("workspace-a".to_owned()),
                },
            )
            .map_err(|e| e.to_string())?;
        // Guarantee a strictly newer created_at for workspace B so the
        // legacy newest-row fallback would resolve to B, never A.
        std::thread::sleep(Duration::from_millis(5));
        connection
            .insert_workspace(
                "wsp_multib0000000000000000000b",
                &crate::db::CreateWorkspaceInput {
                    path: workspace_b.to_string_lossy().into_owned(),
                    name: Some("workspace-b".to_owned()),
                },
            )
            .map_err(|e| e.to_string())?;

        for (index, workspace_id) in [
            "wsp_multia0000000000000000000a",
            "wsp_multia0000000000000000000a",
            "wsp_multib0000000000000000000b",
        ]
        .iter()
        .enumerate()
        {
            connection
                .insert_memory(
                    &format!("mem_multi{index}00000000000000000000")[..30],
                    &crate::db::CreateMemoryInput {
                        workspace_id: (*workspace_id).to_owned(),
                        level: "procedural".to_owned(),
                        kind: "rule".to_owned(),
                        content: format!("workspace-scoped memory {index}"),
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
                .map_err(|e| e.to_string())?;
        }
        connection.close().map_err(|e| e.to_string())?;
        Ok((workspace_a, workspace_b, database))
    }

    #[test]
    fn resolve_index_workspace_id_prefers_requested_workspace_path() -> TestResult {
        let root = unique_test_dir("resolve-workspace-id");
        let (workspace_a, workspace_b, database) = seed_two_workspace_database(&root)?;
        let connection = DbConnection::open_file(&database).map_err(|e| e.to_string())?;

        let resolved_a =
            resolve_index_workspace_id(&connection, &workspace_a).map_err(|e| e.to_string())?;
        ensure(
            resolved_a == "wsp_multia0000000000000000000a",
            format!("requested workspace A path must resolve to A: {resolved_a}"),
        )?;

        let resolved_b =
            resolve_index_workspace_id(&connection, &workspace_b).map_err(|e| e.to_string())?;
        ensure(
            resolved_b == "wsp_multib0000000000000000000b",
            format!("requested workspace B path must resolve to B: {resolved_b}"),
        )?;

        let unregistered = root.join("never-registered");
        std::fs::create_dir_all(&unregistered).map_err(|e| e.to_string())?;
        let fallback =
            resolve_index_workspace_id(&connection, &unregistered).map_err(|e| e.to_string())?;
        let newest = get_default_workspace_id(&connection).map_err(|e| e.to_string())?;
        ensure(
            fallback == newest,
            "unregistered path must preserve the newest-row fallback",
        )?;
        connection.close().map_err(|e| e.to_string())
    }

    #[test]
    fn index_rebuild_respects_requested_workspace_over_newest_row() -> TestResult {
        let root = unique_test_dir("rebuild-workspace-scope");
        let (workspace_a, _workspace_b, database) = seed_two_workspace_database(&root)?;
        let index_dir = root.join("index-a");

        let report = rebuild_index(&IndexRebuildOptions {
            workspace_path: workspace_a,
            database_path: Some(database),
            index_dir: Some(index_dir),
            dry_run: false,
        })
        .map_err(|e| e.to_string())?;

        ensure(
            report.status == IndexRebuildStatus::Success,
            format!("rebuild status: {:?}", report.status),
        )?;
        ensure(
            report.memories_indexed == 2,
            format!(
                "rebuild --workspace A must index A's 2 memories (newest-row B has 1): {}",
                report.memories_indexed
            ),
        )
    }

    #[test]
    fn index_reembed_respects_requested_workspace_over_newest_row() -> TestResult {
        let root = unique_test_dir("reembed-workspace-scope");
        let (workspace_a, _workspace_b, database) = seed_two_workspace_database(&root)?;
        let index_dir = root.join("index-a");

        let report = reembed_index(&IndexReembedOptions {
            workspace_path: workspace_a,
            database_path: Some(database),
            index_dir: Some(index_dir),
            dry_run: false,
        })
        .map_err(|e| e.to_string())?;

        ensure(
            report.status == IndexReembedStatus::Success,
            format!("reembed status: {:?}", report.status),
        )?;
        ensure(
            report.memories_indexed == 2,
            format!(
                "reembed --workspace A must embed A's 2 memories (newest-row B has 1): {}",
                report.memories_indexed
            ),
        )
    }

    #[test]
    fn process_index_jobs_respects_requested_workspace_over_newest_row() -> TestResult {
        let root = unique_test_dir("process-jobs-workspace-scope");
        let (workspace_a, _workspace_b, database) = seed_two_workspace_database(&root)?;
        let index_dir = root.join("index-a");

        let connection = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        connection
            .insert_search_index_job(
                "sidx_workspacea000000000000000a",
                &crate::db::CreateSearchIndexJobInput {
                    workspace_id: "wsp_multia0000000000000000000a".to_owned(),
                    job_type: SearchIndexJobType::FullRebuild,
                    document_source: None,
                    document_id: None,
                    documents_total: 0,
                },
            )
            .map_err(|e| e.to_string())?;
        connection.close().map_err(|e| e.to_string())?;

        let report = process_index_jobs(&IndexProcessingOptions {
            workspace_path: workspace_a,
            database_path: Some(database),
            index_dir: Some(index_dir),
            dry_run: false,
            job_limit: None,
        })
        .map_err(|e| e.to_string())?;

        ensure(
            report.workspace_id == "wsp_multia0000000000000000000a",
            format!(
                "process-jobs --workspace A must resolve workspace A (was newest-row B before GH#20): {}",
                report.workspace_id
            ),
        )?;
        ensure(
            report.processed_jobs == 1,
            format!("workspace A's pending job must be processed: {report:?}"),
        )
    }
}
