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
    RuleIndexProjection, RunningIndexJobFinalizer, SearchIndexJobStatus, SearchIndexJobType,
    StoredSearchIndexJob, UnpublishedIndexStage, append_failed_index_job_transition,
    artifact_to_document, checked_document_counts, complete_running_index_job_in_txn, delta,
    embedder_fingerprint_for_index_metadata, embedder_reports_pending_model2vec_download,
    evidence_span_to_document, finalize_coalesced_claimed_jobs, index_checkpoint,
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
    let selected = IndexModelSelection::resolve_stack(db, workspace_id);
    index_checkpoint(cx)?;
    let (model_selection, stack) = selected?;
    if embedder_reports_pending_model2vec_download(stack.fast())
        && workspace_index_source_rows_fit(db, workspace_id, 0)?
    {
        // Let the complete empty-source path clear the generation without
        // acquiring a model for a corpus that has no possible documents.
        return Ok(None);
    }
    let stack = prepare_index_embedder_stack(cx, stack).await?;
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
        db.visit_index_artifacts_in_current_snapshot::<IndexRebuildError>(
            workspace_id,
            |artifact| {
                observe(
                    cx,
                    &mut reconciler,
                    &mut refused,
                    artifact_to_document(&artifact),
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
        // Physical paging avoids sorting complete source bodies repeatedly.
        // Restore exactly the full collector's order only for retained changes.
        // The projection writes these fields from source rows; they are not
        // user-supplied index metadata. Memory/session order stays unchanged.
        for (source, stamp) in [
            ("artifact", Some("created_at")),
            ("rule", Some("updated_at")),
            ("import", None),
        ] {
            index_checkpoint(cx)?;
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
        index_checkpoint(cx)?;
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
