//! Publish small deltas without retaining every unchanged canonical body.
//!
//! Job identities and generation equality cannot prove source membership: a
//! global-memory withdrawal, admission change, or projection sidecar can affect
//! documents outside the claimed jobs. Reconcile every source in one database
//! snapshot, retaining only changed bodies and complete digest maps. Memory,
//! session and evidence reads are page-bounded; artifacts and rules still use
//! their existing list APIs, released after each class is projected.
//!
//! Prepare a complete private stage before accepting this path. A refusal then
//! returns to the original collector before its snapshot hook, so fallback
//! captures its own coherent generation, counts, and complete document bodies.

use super::{
    BTreeSet, CanonicalSearchDocument, DbConnection, DbError, DbOperation, DeltaStageError,
    EvidenceAdmissionReport, IncrementalFallback, IndexDocumentCounts, IndexEmbedderFingerprint,
    IndexProcessingJobReport, IndexPublishLockOwner, IndexRebuildError, Path,
    RunningIndexJobFinalizer, SearchIndexJobStatus, SearchIndexJobType, StoredSearchIndexJob,
    UnpublishedIndexStage, append_failed_index_job_transition, artifact_to_document,
    checked_document_counts, delta, embedder_fingerprint_for_index_metadata,
    evidence_span_to_document, finalize_coalesced_claimed_jobs, index_checkpoint,
    memory_documents_with_anchors_with_cx, path_exists_no_follow, publish_staged_generation,
    recover_interrupted_publish_for_snapshot, require_index_job_transition, rule_documents,
    session_to_document, source_snapshot, stage_planned_index_delta,
    update_running_index_job_total, validate_incremental_index_metadata, workspace_embedder_stack,
};

#[cfg(test)]
#[path = "index_delta_source_tests.rs"]
mod tests;

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
}

pub(super) async fn prepare(
    publish_lock: &IndexPublishLockOwner<'_>,
    db: &DbConnection,
    workspace_id: &str,
    index_dir: &Path,
) -> Result<Option<PreparedDelta>, IndexRebuildError> {
    let cx = publish_lock.cx;
    index_checkpoint(cx)?;
    // Initial builds and recovery of an absent active generation stay on the
    // existing full-source path, without an unnecessary model/source load.
    if !path_exists_no_follow(&index_dir.join(super::INDEX_METADATA_FILE)) {
        return Ok(None);
    }
    let generation_hint = db.get_workspace_generation(workspace_id);
    index_checkpoint(cx)?;
    let generation_hint = generation_hint?.unwrap_or(0);
    let recovered = publish_lock
        .with_generation_fence(index_dir, || {
            recover_interrupted_publish_for_snapshot(index_dir, generation_hint)
        })
        .await;
    index_checkpoint(cx)?;
    recovered?;
    let eligible = validate_incremental_index_metadata(index_dir, generation_hint, u64::MAX);
    index_checkpoint(cx)?;
    if let Err(refusal) = eligible {
        log_refusal(&refusal);
        return Ok(None);
    }
    let stack = workspace_embedder_stack(db, workspace_id);
    index_checkpoint(cx)?;
    let (stack, _) = stack?;
    let reconciler = delta::SourceReconciler::read(index_dir, &stack);
    index_checkpoint(cx)?;
    let reconciler = match reconciler {
        Ok(reconciler) => reconciler,
        Err(refusal) => {
            log_refusal(&refusal);
            return Ok(None);
        }
    };
    let Some(snapshot) = capture(cx, db, workspace_id, reconciler)? else {
        return Ok(None);
    };
    // Check the actual captured generation as well as the earlier hint. The
    // complete source set, never a queued job or generation count, owns the diff.
    let eligible = validate_incremental_index_metadata(index_dir, snapshot.generation, u64::MAX);
    index_checkpoint(cx)?;
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
    let staged = stage_planned_index_delta(
        cx,
        index_dir,
        &stack,
        reconciliation.delta,
        reconciliation.digests,
        document_counts,
    )
    .await;
    index_checkpoint(cx)?;
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

fn capture(
    cx: &asupersync::Cx,
    db: &DbConnection,
    workspace_id: &str,
    mut reconciler: delta::SourceReconciler,
) -> Result<Option<SourceDeltaSnapshot>, IndexRebuildError> {
    let mut refused = None;
    let captured = source_snapshot::capture(db, || {
        index_checkpoint(cx)?;
        let generation = db.get_workspace_generation(workspace_id)?;
        let mut counts = [0_usize; 5];
        db.visit_index_memory_batches_in_current_snapshot::<IndexRebuildError>(
            workspace_id,
            |page| {
                index_checkpoint(cx)?;
                for document in memory_documents_with_anchors_with_cx(Some(cx), db, page)? {
                    observe(cx, &mut reconciler, &mut refused, document)?;
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
            )?;
            counts[1] += 1;
            Ok(())
        })?;
        for artifact in db.list_artifacts(workspace_id, None)? {
            observe(
                cx,
                &mut reconciler,
                &mut refused,
                artifact_to_document(&artifact),
            )?;
            counts[2] += 1;
        }
        for document in rule_documents(db, workspace_id)? {
            observe(cx, &mut reconciler, &mut refused, document)?;
            counts[3] += 1;
        }
        let scan = db.visit_index_evidence_in_current_snapshot(workspace_id, |span| {
            observe(
                cx,
                &mut reconciler,
                &mut refused,
                evidence_span_to_document(&span),
            )?;
            counts[4] += 1;
            Ok(())
        })?;
        index_checkpoint(cx)?;
        let document_counts =
            checked_document_counts(counts[0], counts[1], counts[2], counts[3], counts[4])?;
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
        // The evidence visitor uses physical pagination. Match the full
        // collector's canonical evidence order without retaining unchanged
        // evidence bodies: all evidence is the final source class here.
        if let Some(first_evidence) = reconciliation.delta.upserts.iter().position(|document| {
            document
                .metadata
                .get("source")
                .is_some_and(|source| source == "import")
        }) {
            reconciliation.delta.upserts[first_evidence..]
                .sort_by(|left, right| left.id.cmp(&right.id));
        }
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
    index_checkpoint(cx)?;
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
) -> Result<(), DbError> {
    if index_checkpoint(cx).is_err() {
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
    let cx = publish_lock.cx;
    let after_capture = after_snapshot();
    if let Err(error) = index_checkpoint(cx).and(after_capture) {
        finalize_coalesced_claimed_jobs(db, jobs, finalizers, &error);
        return Err(error);
    }
    let documents_total = prepared.document_counts.total();
    for job in jobs {
        let updated = update_running_index_job_total(db, &job.id, documents_total);
        if let Err(error) = index_checkpoint(cx).and(updated) {
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
                for job in jobs {
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
