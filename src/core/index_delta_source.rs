//! Publish small deltas without retaining every unchanged canonical body.
//!
//! Job identities and generation equality cannot prove source membership: a
//! global-memory withdrawal, admission change, or projection sidecar can affect
//! documents outside the claimed jobs. Reconcile every source in one database
//! snapshot, retaining only changed bodies and complete digest maps. Every
//! source class is page-read; rule tags and source links are loaded only for
//! the current rule page, not for the entire workspace.
//!
//! Prepare a complete private stage before accepting this path. A refusal then
//! returns to the original collector before its snapshot hook, so fallback
//! captures its own coherent generation, counts, and complete document bodies.

use super::{
    BTreeSet, CanonicalSearchDocument, DbConnection, DbError, DbOperation, DeltaStageError,
    EvidenceAdmissionReport, IncrementalFallback, IndexDocumentCounts, IndexEmbedderFingerprint,
    IndexModelSelection, IndexProcessingJobReport, IndexPublishLockOwner, IndexRebuildError, Path,
    RuleIndexProjection, RunningIndexJobFinalizer, SearchIndexJobType, StoredSearchIndexJob,
    UnpublishedIndexStage, append_failed_index_job_transition, artifact_to_document,
    checked_document_counts, collect_open_index_job_ids, complete_running_index_job_in_txn, delta,
    embedder_fingerprint_for_index_metadata, embedder_reports_pending_model2vec_download,
    evidence_span_to_document, finalize_coalesced_claimed_jobs, index_operation_checkpoint,
    memory_documents_with_anchors_with_cx, path_exists_no_follow,
    persist_index_embedding_registry_input, prepare_index_embedder_stack,
    prepare_index_embedding_registry_input, publish_staged_generation,
    recover_interrupted_publish_for_snapshot, rule_to_document, session_to_document,
    source_snapshot, stage_planned_index_delta, update_running_index_job_total,
    validate_incremental_index_metadata, workspace_index_source_rows_fit,
};

#[cfg(test)]
#[path = "index_delta_source_tests.rs"]
mod tests;

pub(super) const MAX_INTERACTIVE_SOURCE_DOCUMENTS: u32 = 4096;
pub(super) const MAX_INTERACTIVE_SOURCE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_INTERACTIVE_MUTATIONS: u32 = 64;
const MAX_INTERACTIVE_CHANGED_BYTES: usize = 1024 * 1024;
const MAX_INTERACTIVE_GENERATION_BYTES: u64 = 64 * 1024 * 1024;
const MAX_INTERACTIVE_GENERATION_ENTRIES: usize = 512;
const MAX_INTERACTIVE_GENERATION_DEPTH: usize = 16;
const MAX_INTERACTIVE_DIGEST_BYTES: u64 = 2 * 1024 * 1024;
const MAX_INTERACTIVE_METADATA_BYTES: u64 = 64 * 1024;

struct SourceDeltaSnapshot {
    generation: u64,
    document_counts: IndexDocumentCounts,
    evidence_admission: EvidenceAdmissionReport,
    open_job_ids: BTreeSet<String>,
    reconciliation: delta::ReconciledSource,
}

pub(super) struct PreparedDelta {
    generation: u64,
    document_counts: IndexDocumentCounts,
    open_job_ids: BTreeSet<String>,
    stage: UnpublishedIndexStage,
    embedder_fingerprint: Option<IndexEmbedderFingerprint>,
    embedding_registration: Option<crate::db::CreateEmbeddingMetadataInput>,
    model_selection: IndexModelSelection,
}

pub(super) async fn prepare(
    publish_lock: &IndexPublishLockOwner<'_>,
    db: &DbConnection,
    workspace_id: &str,
    index_dir: &Path,
) -> Result<Option<PreparedDelta>, IndexRebuildError> {
    prepare_with_limits(publish_lock, db, workspace_id, index_dir, None, None).await
}

pub(super) async fn prepare_bounded(
    publish_lock: &IndexPublishLockOwner<'_>,
    db: &DbConnection,
    workspace_id: &str,
    index_dir: &Path,
    max_changed_documents: u32,
    claimed_job_count: usize,
) -> Result<Option<PreparedDelta>, IndexRebuildError> {
    // Shared source-allocation admission is available with the lexical tier.
    // Other builds retain the existing small complete-source repair path.
    if !cfg!(feature = "lexical-bm25") {
        publish_lock.checkpoint()?;
        return Ok(None);
    }
    prepare_with_limits(
        publish_lock,
        db,
        workspace_id,
        index_dir,
        Some(delta::SourceReconciliationLimits {
            documents: MAX_INTERACTIVE_SOURCE_DOCUMENTS as usize,
            mutations: max_changed_documents.min(MAX_INTERACTIVE_MUTATIONS) as usize,
            projected_bytes: MAX_INTERACTIVE_SOURCE_BYTES as usize,
            changed_bytes: MAX_INTERACTIVE_CHANGED_BYTES,
        }),
        Some(claimed_job_count.saturating_add(1)),
    )
    .await
}

async fn prepare_with_limits(
    publish_lock: &IndexPublishLockOwner<'_>,
    db: &DbConnection,
    workspace_id: &str,
    index_dir: &Path,
    limits: Option<delta::SourceReconciliationLimits>,
    max_open_job_ids: Option<usize>,
) -> Result<Option<PreparedDelta>, IndexRebuildError> {
    let cx = publish_lock.cx;
    publish_lock.checkpoint()?;
    // Initial builds and recovery of an absent active generation stay on the
    // existing full-source path, without an unnecessary model/source load.
    if !path_exists_no_follow(&index_dir.join(super::INDEX_METADATA_FILE)) {
        return Ok(None);
    }
    // A small delta cannot authorize unbounded native validation, copying,
    // compaction or digest parsing of the previous generation.
    if limits.is_some() && !bounded_generation_fits(cx, index_dir, publish_lock.control)? {
        return Ok(None);
    }
    let generation_hint = db.get_workspace_generation(workspace_id);
    publish_lock.checkpoint()?;
    let generation_hint = generation_hint?.unwrap_or(0);
    if limits.is_none() {
        let recovered = publish_lock
            .with_generation_fence(index_dir, || {
                recover_interrupted_publish_for_snapshot(index_dir, generation_hint)
            })
            .await;
        publish_lock.checkpoint()?;
        recovered?;
    }
    let eligible = if limits.is_some() {
        super::validate_incremental_index_metadata_with_limit(
            index_dir,
            generation_hint,
            u64::MAX,
            MAX_INTERACTIVE_METADATA_BYTES,
        )
    } else {
        validate_incremental_index_metadata(index_dir, generation_hint, u64::MAX)
    };
    publish_lock.checkpoint()?;
    if let Err(refusal) = eligible {
        log_refusal(&refusal);
        return Ok(None);
    }
    let selected = IndexModelSelection::resolve_stack(db, workspace_id);
    publish_lock.checkpoint()?;
    let (model_selection, stack) = selected?;
    if embedder_reports_pending_model2vec_download(stack.fast())
        && workspace_index_source_rows_fit(db, workspace_id, 0)?
    {
        // Let the complete empty-source path clear the generation without
        // acquiring a model for a corpus that has no possible documents.
        return Ok(None);
    }
    publish_lock.checkpoint()?;
    let stack = prepare_index_embedder_stack(cx, stack).await?;
    publish_lock.checkpoint()?;
    let reconciler = match limits {
        Some(limits) => delta::SourceReconciler::read_bounded(
            index_dir,
            &stack,
            limits,
            MAX_INTERACTIVE_DIGEST_BYTES,
        ),
        None => delta::SourceReconciler::read(index_dir, &stack),
    };
    publish_lock.checkpoint()?;
    let reconciler = match reconciler {
        Ok(reconciler) => reconciler,
        Err(refusal) => {
            log_refusal(&refusal);
            return Ok(None);
        }
    };
    let Some(snapshot) = capture_with_control(
        cx,
        db,
        workspace_id,
        reconciler,
        publish_lock.control,
        limits,
        max_open_job_ids,
    )?
    else {
        return Ok(None);
    };
    // Check the actual captured generation as well as the earlier hint. The
    // complete source set, never a queued job or generation count, owns the diff.
    let eligible = if limits.is_some() {
        super::validate_incremental_index_metadata_with_limit(
            index_dir,
            snapshot.generation,
            u64::MAX,
            MAX_INTERACTIVE_METADATA_BYTES,
        )
    } else {
        validate_incremental_index_metadata(index_dir, snapshot.generation, u64::MAX)
    };
    publish_lock.checkpoint()?;
    if let Err(refusal) = eligible {
        log_refusal(&refusal);
        return Ok(None);
    }
    let SourceDeltaSnapshot {
        generation,
        document_counts,
        evidence_admission,
        open_job_ids,
        reconciliation,
    } = snapshot;
    // The publication lease fences cooperating publishers; reject observed
    // external growth as well before staging begins.
    if limits.is_some() && !bounded_generation_fits(cx, index_dir, publish_lock.control)? {
        return Ok(None);
    }
    let staged = stage_planned_index_delta(
        publish_lock,
        index_dir,
        &stack,
        reconciliation.delta,
        reconciliation.digests,
        document_counts,
    )
    .await;
    publish_lock.checkpoint()?;
    match staged {
        Ok((stage, applied)) => {
            tracing::info!(
                target: "ee::index",
                workspace_id,
                documents = document_counts.total(),
                retained_upserts = applied.upserts.len(),
                removals = applied.removals.len(),
                ?evidence_admission,
                "prepared complete index delta from a streamed source snapshot"
            );
            Ok(Some(PreparedDelta {
                generation,
                document_counts,
                open_job_ids,
                stage,
                embedder_fingerprint: embedder_fingerprint_for_index_metadata(&stack),
                model_selection,
                embedding_registration: prepare_index_embedding_registry_input(
                    db,
                    workspace_id,
                    &stack,
                    document_counts.total(),
                )?,
            }))
        }
        Err(DeltaStageError::Cancelled(error)) => Err(error),
        Err(DeltaStageError::Refused(reason)) => {
            tracing::debug!(
                target: "ee::index",
                reason = %reason,
                "streamed delta stage refused; recapturing the complete source for fallback"
            );
            Ok(None)
        }
    }
}

/// Inspect only bounded filesystem metadata before any index deserialization
/// or native backend opens. Logical bytes bound fallback copies and native
/// compaction work; they are not an estimate of allocated disk blocks or RSS.
fn bounded_generation_fits(
    cx: &asupersync::Cx,
    index_dir: &Path,
    control: Option<&super::IndexOperationControl>,
) -> Result<bool, IndexRebuildError> {
    index_operation_checkpoint(cx, control)?;
    let safe_path = super::ensure_index_path_has_no_symlinks(
        index_dir,
        "inspect bounded index delta generation",
    );
    index_operation_checkpoint(cx, control)?;
    if safe_path.is_err() {
        return Ok(false);
    }
    let root = std::fs::symlink_metadata(index_dir);
    index_operation_checkpoint(cx, control)?;
    if !root.is_ok_and(|metadata| metadata.file_type().is_dir()) {
        return Ok(false);
    }
    let mut pending = vec![(index_dir.to_path_buf(), 0_usize)];
    let mut entries_seen = 0_usize;
    let mut total_bytes = 0_u64;
    while let Some((directory, depth)) = pending.pop() {
        let entries = std::fs::read_dir(&directory);
        index_operation_checkpoint(cx, control)?;
        let Ok(entries) = entries else {
            return Ok(false);
        };
        for entry in entries {
            index_operation_checkpoint(cx, control)?;
            let Ok(entry) = entry else {
                return Ok(false);
            };
            entries_seen += 1;
            if entries_seen > MAX_INTERACTIVE_GENERATION_ENTRIES {
                return Ok(false);
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return Ok(false);
            };
            let metadata = std::fs::symlink_metadata(entry.path());
            index_operation_checkpoint(cx, control)?;
            let Ok(metadata) = metadata else {
                return Ok(false);
            };
            if metadata.file_type().is_dir() {
                if depth >= MAX_INTERACTIVE_GENERATION_DEPTH {
                    return Ok(false);
                }
                pending.push((entry.path(), depth + 1));
            } else if metadata.file_type().is_file() {
                let Some(bytes) = total_bytes.checked_add(metadata.len()) else {
                    return Ok(false);
                };
                total_bytes = bytes;
                if total_bytes > MAX_INTERACTIVE_GENERATION_BYTES
                    || (depth == 0
                        && name == delta::DOC_DIGESTS_FILE
                        && metadata.len() > MAX_INTERACTIVE_DIGEST_BYTES)
                    || (depth == 0
                        && name == super::INDEX_METADATA_FILE
                        && metadata.len() > MAX_INTERACTIVE_METADATA_BYTES)
                {
                    return Ok(false);
                }
            } else {
                return Ok(false);
            }
        }
    }
    let parsed = super::parse_index_metadata_with_limit(index_dir, MAX_INTERACTIVE_METADATA_BYTES);
    index_operation_checkpoint(cx, control)?;
    let Ok(Some(metadata)) = parsed else {
        return Ok(false);
    };
    Ok(metadata
        .document_count
        .is_some_and(|count| count <= MAX_INTERACTIVE_SOURCE_DOCUMENTS)
        && metadata.generation.is_some()
        && super::index_metadata_compatibility_error(
            &index_dir.join(super::INDEX_METADATA_FILE),
            &metadata,
        )
        .is_none())
}

/// Read-repair hint without validated corpus admission or source hydration.
/// The publisher repeats its authoritative checks after acquiring the lease.
pub(super) fn bounded_generation_gap(
    cx: &asupersync::Cx,
    db: &DbConnection,
    workspace_id: &str,
    index_dir: &Path,
    max_generation_gap: u64,
) -> Result<Option<u64>, IndexRebuildError> {
    if !cfg!(feature = "lexical-bm25") {
        index_operation_checkpoint(cx, None)?;
        return Ok(None);
    }
    #[cfg(unix)]
    let _generation_lease = {
        let lease = super::IndexGenerationLease::try_read_for_inspection(index_dir);
        index_operation_checkpoint(cx, None)?;
        let Some(lease) = lease? else {
            return Ok(None);
        };
        lease
    };
    if !bounded_generation_fits(cx, index_dir, None)? {
        return Ok(None);
    }
    let parsed = super::parse_index_metadata_with_limit(index_dir, MAX_INTERACTIVE_METADATA_BYTES);
    index_operation_checkpoint(cx, None)?;
    let Ok(Some(metadata)) = parsed else {
        return Ok(None);
    };
    let Some(index_generation) = metadata.generation else {
        return Ok(None);
    };
    source_snapshot::capture(db, || {
        index_operation_checkpoint(cx, None)?;
        #[cfg(feature = "lexical-bm25")]
        let rows_fit = source_snapshot::live_source_rows_fit(
            cx,
            db,
            workspace_id,
            MAX_INTERACTIVE_SOURCE_DOCUMENTS,
        )?;
        #[cfg(not(feature = "lexical-bm25"))]
        let rows_fit = false;
        if !rows_fit {
            return Ok(None);
        }
        let generation = db.get_workspace_generation(workspace_id);
        index_operation_checkpoint(cx, None)?;
        let Some(generation) = generation? else {
            return Ok(None);
        };
        Ok(generation
            .checked_sub(index_generation)
            .filter(|gap| *gap > 0 && *gap <= max_generation_gap))
    })
}

#[cfg(test)]
fn capture(
    cx: &asupersync::Cx,
    db: &DbConnection,
    workspace_id: &str,
    reconciler: delta::SourceReconciler,
) -> Result<Option<SourceDeltaSnapshot>, IndexRebuildError> {
    capture_with_control(cx, db, workspace_id, reconciler, None, None, None)
}

fn capture_with_control(
    cx: &asupersync::Cx,
    db: &DbConnection,
    workspace_id: &str,
    mut reconciler: delta::SourceReconciler,
    control: Option<&super::IndexOperationControl>,
    limits: Option<delta::SourceReconciliationLimits>,
    max_open_job_ids: Option<usize>,
) -> Result<Option<SourceDeltaSnapshot>, IndexRebuildError> {
    let mut refused = None;
    let captured = source_snapshot::capture(db, || {
        index_operation_checkpoint(cx, control)?;
        if limits.is_some() {
            #[cfg(feature = "lexical-bm25")]
            let admitted = source_snapshot::live_source_rows_fit(
                cx,
                db,
                workspace_id,
                MAX_INTERACTIVE_SOURCE_DOCUMENTS,
            )? && source_snapshot::live_source_allocation_fits(
                cx,
                db,
                workspace_id,
                MAX_INTERACTIVE_SOURCE_BYTES,
            )?;
            #[cfg(not(feature = "lexical-bm25"))]
            let admitted = false;
            index_operation_checkpoint(cx, control)?;
            if !admitted {
                refused = Some(super::incremental_fallback(
                    super::IncrementalFallbackReason::DeltaOverThreshold,
                    "complete source exceeds the interactive row or allocation budget",
                ));
                return Err(source_refused().into());
            }
        }
        let generation = db.get_workspace_generation(workspace_id)?;
        let mut counts = [0_usize; 5];
        db.visit_index_memory_batches_in_current_snapshot::<IndexRebuildError>(
            workspace_id,
            |page| {
                index_operation_checkpoint(cx, control)?;
                for document in memory_documents_with_anchors_with_cx(Some(cx), db, page)? {
                    observe(cx, &mut reconciler, &mut refused, document, control)?;
                    counts[0] += 1;
                }
                Ok(())
            },
        )?;
        db.visit_sessions_for_workspace_in_current_snapshot(workspace_id, |session| {
            observe(
                cx,
                &mut reconciler,
                &mut refused,
                session_to_document(&session),
                control,
            )?;
            counts[1] += 1;
            Ok(())
        })?;
        db.visit_index_artifacts_in_current_snapshot::<IndexRebuildError>(
            workspace_id,
            |artifact| {
                observe(
                    cx,
                    &mut reconciler,
                    &mut refused,
                    artifact_to_document(&artifact),
                    control,
                )?;
                counts[2] += 1;
                Ok(())
            },
        )?;
        let workspace = db.get_workspace(workspace_id)?.ok_or_else(|| {
            IndexRebuildError::Index(format!(
                "Workspace {workspace_id} disappeared while projecting procedural rules."
            ))
        })?;
        db.visit_index_rules_in_current_snapshot::<IndexRebuildError>(
            workspace_id,
            |rule, tags, sources| {
                let projection =
                    RuleIndexProjection::new(rule, workspace.path.as_str(), tags, sources);
                if projection.is_search_indexable() {
                    observe(
                        cx,
                        &mut reconciler,
                        &mut refused,
                        rule_to_document(&projection),
                        control,
                    )?;
                    counts[3] += 1;
                }
                Ok(())
            },
        )?;
        let scan = db.visit_index_evidence_in_current_snapshot(workspace_id, |span| {
            observe(
                cx,
                &mut reconciler,
                &mut refused,
                evidence_span_to_document(&span),
                control,
            )?;
            counts[4] += 1;
            Ok(())
        })?;
        index_operation_checkpoint(cx, control)?;
        let document_counts =
            checked_document_counts(counts[0], counts[1], counts[2], counts[3], counts[4])?;
        let open_job_ids = collect_open_index_job_ids(db, workspace_id, max_open_job_ids)?;
        let mut reconciliation = match reconciler.finish() {
            Ok(reconciliation) => reconciliation,
            Err(refusal) => {
                refused = Some(refusal);
                return Err(source_refused().into());
            }
        };
        if usize::try_from(document_counts.total()).ok() != Some(reconciliation.documents_total) {
            return Err(IndexRebuildError::Index(
                "streamed source membership disagrees with its document counts".to_owned(),
            ));
        }
        // Physical paging avoids sorting complete source bodies repeatedly.
        // Restore exactly the full collector's order only for retained changes.
        // The projection writes these fields from source rows; they are not
        // user-supplied index metadata. Memory/session order stays unchanged.
        for (source, stamp) in [
            ("artifact", Some("created_at")),
            ("rule", Some("updated_at")),
            ("import", None),
        ] {
            index_operation_checkpoint(cx, control)?;
            let belongs = |document: &crate::search::IndexableDocument| {
                document
                    .metadata
                    .get("source")
                    .is_some_and(|value| value == source)
            };
            let Some(start) = reconciliation.delta.upserts.iter().position(belongs) else {
                continue;
            };
            let count = reconciliation.delta.upserts[start..]
                .iter()
                .take_while(|document| belongs(document))
                .count();
            let changes = &mut reconciliation.delta.upserts[start..start + count];
            if let Some(field) = stamp {
                if changes
                    .iter()
                    .any(|document| !document.metadata.contains_key(field))
                {
                    return Err(IndexRebuildError::Index(
                        "Streamed source lacks its canonical ordering field".to_owned(),
                    ));
                }
                changes.sort_by(|left, right| {
                    right
                        .metadata
                        .get(field)
                        .cmp(&left.metadata.get(field))
                        .then_with(|| left.id.cmp(&right.id))
                });
            } else {
                changes.sort_by(|left, right| left.id.cmp(&right.id));
            }
        }
        index_operation_checkpoint(cx, control)?;
        Ok(SourceDeltaSnapshot {
            generation: generation.unwrap_or_else(|| u64::from(document_counts.total())),
            document_counts,
            evidence_admission: scan.admission,
            open_job_ids,
            reconciliation,
        })
    });
    // Storage may report interruption as a DB error; retain the request's
    // cancellation before interpreting an intentional collector refusal.
    index_operation_checkpoint(cx, control)?;
    if let Some(refusal) = refused {
        log_refusal(&refusal);
        return Ok(None);
    }
    captured.map(Some)
}

fn observe(
    cx: &asupersync::Cx,
    reconciler: &mut delta::SourceReconciler,
    refused: &mut Option<IncrementalFallback>,
    document: CanonicalSearchDocument,
    control: Option<&super::IndexOperationControl>,
) -> Result<(), DbError> {
    if index_operation_checkpoint(cx, control).is_err() {
        return Err(source_refused());
    }
    reconciler
        .observe(document.into_indexable())
        .map_err(|error| {
            *refused = Some(error);
            source_refused()
        })
}

fn source_refused() -> DbError {
    DbError::MalformedRow {
        operation: DbOperation::Query,
        message: "streamed index delta requested complete source fallback".to_owned(),
    }
}

fn log_refusal(refusal: &IncrementalFallback) {
    tracing::debug!(
        target: "ee::index",
        reason = refusal.reason.as_str(),
        detail = %refusal.detail,
        "streamed delta refused; retaining the complete-source planner"
    );
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn publish<F>(
    publish_lock: &IndexPublishLockOwner<'_>,
    db: &DbConnection,
    index_dir: &Path,
    jobs: &[StoredSearchIndexJob],
    finalizers: &mut [RunningIndexJobFinalizer<'_>],
    prepared: PreparedDelta,
    base_processing_mode: &str,
    after_snapshot: F,
) -> Result<Vec<IndexProcessingJobReport>, IndexRebuildError>
where
    F: FnOnce() -> Result<(), IndexRebuildError>,
{
    let after_capture = after_snapshot();
    if let Err(error) = publish_lock.checkpoint().and(after_capture) {
        finalize_coalesced_claimed_jobs(db, jobs, finalizers, &error);
        return Err(error);
    }
    let documents_total = prepared.document_counts.total();
    for job in jobs {
        let updated = update_running_index_job_total(db, &job.id, documents_total);
        if let Err(error) = publish_lock.checkpoint().and(updated) {
            finalize_coalesced_claimed_jobs(db, jobs, finalizers, &error);
            return Err(error);
        }
    }
    let claimed_ids = jobs
        .iter()
        .map(|job| job.id.as_str())
        .collect::<BTreeSet<_>>();
    let has_sibling = prepared
        .open_job_ids
        .iter()
        .any(|id| !claimed_ids.contains(id.as_str()));
    let mut processing_mode = base_processing_mode.replace("_as_full_rebuild", "");
    if has_sibling {
        if base_processing_mode == "coalesced_full_rebuild" {
            processing_mode.push_str("_open_sibling_full_rebuild");
        } else if jobs.first().is_some_and(|job| {
            matches!(
                job.job_type_enum(),
                Some(SearchIndexJobType::Incremental | SearchIndexJobType::SingleDocument)
            )
        }) {
            processing_mode.push_str("_sibling_pending_full_rebuild");
        }
    }
    processing_mode.push_str("_staged_delta");
    let result = publish_staged_generation(
        publish_lock,
        index_dir,
        prepared.stage,
        prepared.generation,
        prepared.document_counts,
        prepared.embedder_fingerprint.as_ref(),
        || {
            db.with_transaction_error(|| {
                prepared.model_selection.verify(db)?;
                persist_index_embedding_registry_input(
                    db,
                    prepared.embedding_registration.as_ref(),
                )?;
                for job in jobs {
                    complete_running_index_job_in_txn(db, &job.id, documents_total)?;
                }
                Ok(())
            })
        },
    )
    .await;
    let primary_error = match result {
        Ok(()) => None,
        Err(error @ IndexRebuildError::Cancelled(_)) => {
            finalize_coalesced_claimed_jobs(db, jobs, finalizers, &error);
            return Err(error);
        }
        Err(error) => Some(error.to_string()),
    };
    Ok(jobs
        .iter()
        .map(|job| {
            let error = primary_error.as_ref().map(|message| {
                let mut message = message.clone();
                append_failed_index_job_transition(db, &job.id, &mut message);
                message
            });
            let succeeded = error.is_none();
            IndexProcessingJobReport {
                job_id: job.id.clone(),
                job_type: job.job_type.clone(),
                document_source: job.document_source.clone(),
                document_id: job.document_id.clone(),
                outcome: if succeeded { "completed" } else { "failed" }.to_owned(),
                processing_mode: processing_mode.clone(),
                fallback_to_full: None,
                documents_total,
                documents_indexed: if succeeded { documents_total } else { 0 },
                error,
            }
        })
        .collect())
}
