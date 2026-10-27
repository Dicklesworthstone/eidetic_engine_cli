//! One writer-owned decision transition, including its structured payload,
//! predecessor, lineage, audit, and durable index obligations.

use super::*;
use crate::core::contradiction_detect::{
    CONFLICT_RESOLUTION_AUDIT_ACTION, CONFLICT_RESOLUTION_AUDIT_SCHEMA, ConflictResolutionOutcome,
    ConflictResolutionPlan, ConflictResolveRequest, ContradictionDetectionConfig,
    PlannedResolutionAction, ResolveVerb, assemble_conflict_surface, plan_conflict_resolution,
};
use crate::core::memory::{
    PreparedRememberTxnWrite, RememberMemoryOptions, finish_prepared_remember_txn_write,
    prepare_remember_txn_write_for_connection, record_prepared_remember_txn_write_in_txn,
    remember_memory,
};
use crate::db::{
    ApplyMemoryLevelTransitionInput, CreateAuditInput, CreateMemoryInput, CreateMemoryLinkInput,
    CreateSearchIndexJobInput, DbError, MemoryLinkSource, SearchIndexJobType, audit_actions,
    generate_audit_id,
};
use crate::models::MemoryId;
use sqlmodel_core::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    Prepared,
    Memory,
    Fields,
    MergeMemory,
    MergeSource,
    Link,
    Predecessor,
    Index,
    ResolutionAction,
    Provenance,
    Completion,
    Report,
}

struct Supersession<'a> {
    from_memory_id: &'a str,
    to_memory_id: &'a str,
    details: JsonValue,
}

#[derive(Debug)]
struct RecordError(DomainError);

impl From<DomainError> for RecordError {
    fn from(error: DomainError) -> Self {
        Self(error)
    }
}

impl From<DbError> for RecordError {
    fn from(_: DbError) -> Self {
        Self(decide_storage_error(
            "Could not commit the complete decision transition; no partial decision was accepted",
        ))
    }
}

pub(super) fn record(options: &DecideRecordOptions<'_>) -> Result<DecideRecordReport, DomainError> {
    record_with_boundary(options, |_, _| Ok(()))
}

pub(super) fn record_resolution(
    options: &DecideRecordOptions<'_>,
    plan: &ConflictResolutionPlan,
) -> Result<ConflictResolutionReport, DomainError> {
    record_resolution_with_boundary(options, plan, |_, _| Ok(()))
}

fn record_resolution_with_boundary(
    options: &DecideRecordOptions<'_>,
    plan: &ConflictResolutionPlan,
    boundary: impl FnMut(Stage, &DbConnection) -> Result<(), DomainError>,
) -> Result<ConflictResolutionReport, DomainError> {
    record_resolution_candidate_with_boundary(options, plan, None, boundary)
}

pub(super) fn record_candidate_resolution(
    options: &DecideRecordOptions<'_>,
    plan: &ConflictResolutionPlan,
    candidate: &crate::db::StoredCurationCandidate,
) -> Result<ConflictResolutionReport, DomainError> {
    record_resolution_candidate_with_boundary(options, plan, Some(candidate), |_, _| Ok(()))
}

fn record_resolution_candidate_with_boundary(
    options: &DecideRecordOptions<'_>,
    plan: &ConflictResolutionPlan,
    candidate: Option<&crate::db::StoredCurationCandidate>,
    boundary: impl FnMut(Stage, &DbConnection) -> Result<(), DomainError>,
) -> Result<ConflictResolutionReport, DomainError> {
    if plan.verb == ResolveVerb::Merge && candidate.is_none() {
        return Err(DomainError::UsageCodeWithDetails {
            code: "conflict_merge_requires_review",
            message: "Merging conflict sources requires an approved curation proposal.".to_owned(),
            repair: Some("Use ee conflict resolve --verb merge --merge-content <TEXT> --propose, then validate and apply its candidate.".to_owned()),
            details_json: "{}".to_owned(),
        });
    }
    let decisions: Vec<_> = plan
        .actions
        .iter()
        .filter_map(|action| match action {
            PlannedResolutionAction::RecordDecision {
                topic,
                chosen,
                alternatives,
                supersedes,
            } => Some((topic, chosen, alternatives, supersedes)),
            _ => None,
        })
        .collect();
    let [(topic, chosen, alternatives, supersedes)] = decisions.as_slice() else {
        return Err(invalid_conflict_plan());
    };
    if options.dry_run
        || options.revisit_by.is_some()
        || options.topic != topic.as_str()
        || options.chosen != chosen.as_str()
        || &options.alternatives != *alternatives
        || options.supersedes != supersedes.as_deref()
    {
        return Err(invalid_conflict_plan());
    }
    // A rationale explains a resolution; it is not a revision of either
    // source, even when one source is itself a decision.
    let mut decision_options = options.clone();
    decision_options.supersedes = None;
    decision_options.actor = options.actor.or(Some("ee conflict resolve"));
    let now = options.now.unwrap_or_else(Utc::now);
    let fields = prepare_decision_fields(
        options.topic,
        options.chosen,
        &options.alternatives,
        options.rationale,
        None,
        None,
        now,
    )?;
    let _ = validated_fields(&fields)?;
    let mut scope = decide_scope(options.workspace_path, options.database_path, false)?;
    let heads = read::load_record_heads(&mut scope, &fields, now)?;
    let mut prior = heads
        .iter()
        .filter(|head| head.normalized_topic == fields.normalized_topic);
    let predecessor = prior.next().map(|head| head.memory_id.clone());
    if prior.next().is_some()
        || predecessor
            .as_deref()
            .is_some_and(|id| id == plan.memory_a || id == plan.memory_b)
    {
        return Err(invalid_conflict_plan());
    }
    // A previously recorded rationale can predate terminal conflict handling.
    // Propose its ordinary decision revision here; the writer rechecks the
    // exact head, topic and hidden-source authority before accepting it.
    decision_options.supersedes = predecessor.as_deref();
    decision_options.now = Some(now);
    record_with_optional_resolution_boundary(&decision_options, Some(plan), candidate, boundary)
}

fn record_with_boundary(
    options: &DecideRecordOptions<'_>,
    boundary: impl FnMut(Stage, &DbConnection) -> Result<(), DomainError>,
) -> Result<DecideRecordReport, DomainError> {
    record_with_optional_resolution_boundary(options, None, None, boundary)
        .map(|report| report.decision)
}

fn record_with_optional_resolution_boundary(
    options: &DecideRecordOptions<'_>,
    resolution: Option<&ConflictResolutionPlan>,
    candidate: Option<&crate::db::StoredCurationCandidate>,
    mut boundary: impl FnMut(Stage, &DbConnection) -> Result<(), DomainError>,
) -> Result<ConflictResolutionReport, DomainError> {
    let mut scope = decide_scope(
        options.workspace_path,
        options.database_path,
        options.dry_run,
    )?;
    let now = options.now.unwrap_or_else(Utc::now);
    let fields = prepare_decision_fields(
        options.topic,
        options.chosen,
        &options.alternatives,
        options.rationale,
        options.revisit_by,
        options.supersedes,
        now,
    )?;
    let typed_fields = validated_fields(&fields)?;
    let tag_csv = decision_tag_csv(&fields.normalized_topic);
    let content = decision_content(&fields);
    let remember_options = RememberMemoryOptions {
        workspace_path: &scope.workspace_path,
        database_path: Some(&scope.database_path),
        content: &content,
        workflow_id: None,
        level: "semantic",
        kind: "decision",
        tags: Some(&tag_csv),
        confidence: 0.85,
        source: None,
        allow_secret_mention: false,
        valid_from: None,
        valid_to: None,
        dry_run: options.dry_run,
        auto_link: false,
        propose_candidates: false,
    };
    if options.dry_run {
        let preview = remember_memory(&remember_options)?;
        let existing = read::load_record_heads(&mut scope, &fields, now)?;
        let predecessor = validate_head(&existing, &fields)?;
        let chain_depth = predecessor
            .map(|item| lineage::successor_depth(item.chain_depth))
            .transpose()?
            .unwrap_or(0);
        return Ok(ConflictResolutionReport {
            decision: DecideRecordReport {
                schema: DECIDE_RECORD_SCHEMA_V1,
                version: env!("CARGO_PKG_VERSION"),
                status: "would_record".to_owned(),
                dry_run: true,
                persisted: false,
                workspace_id: scope.workspace_id,
                database_path: scope.database_path.display().to_string(),
                decision: DecideItem {
                    memory_id: preview.memory_id.to_string(),
                    topic: fields.topic.clone(),
                    normalized_topic: fields.normalized_topic.clone(),
                    chosen: fields.chosen.clone(),
                    alternatives: fields.alternatives.clone(),
                    options: fields.options.clone(),
                    rationale: fields.rationale.clone(),
                    supersedes: fields.supersedes.clone(),
                    chain_depth,
                    revisit_by: fields.revisit_by.clone(),
                    revisit_status: revisit_status(fields.revisit_by.as_deref(), now, None),
                    superseded: false,
                    valid_to: None,
                    created_at: now.to_rfc3339_opts(SecondsFormat::Secs, true),
                },
                superseded: predecessor.map(|item| DecideMemoryRef {
                    memory_id: item.memory_id.clone(),
                    valid_to: item.valid_to.clone(),
                    status: "would_supersede".to_owned(),
                }),
                memory_audit_id: None,
                memory_index_job_id: None,
                link_audit_id: None,
                expire_audit_id: None,
                warnings: Vec::new(),
            },
            results: Vec::new(),
            candidate_audit_id: None,
            candidate_applied_at: None,
        });
    }

    crate::core::ensure_addressed_database_exists(&scope.database_path)?;
    let connection = open_decide_database(&scope.database_path)?;
    // Reuse canonical remember validation, provenance, anchors and audit.
    // Preparation does not insert the decision or publish a derived index.
    let write = prepare_remember_txn_write_for_connection(&connection, &remember_options, false)?;
    boundary(Stage::Prepared, &connection)?;
    let mut report = connection
        .with_transaction_error(|| {
            // This read must be AFTER acquiring the write-owner/BEGIN IMMEDIATE.
            // Neither an earlier preview nor the caller's predecessor is authority
            // to replace a head that another writer has already superseded.
            let bound = crate::core::workspace::ensure_bound_workspace(
                &connection,
                write.workspace_id(),
                &[scope.workspace_path.as_path(), options.workspace_path],
            )?;
            if bound != write.workspace_id() {
                return Err(decide_storage_error(
                    "Decision workspace binding changed before commit",
                )
                .into());
            }
            if let Some(plan) = resolution {
                if let Some(candidate) = candidate {
                    crate::core::conflict_proposal::validate_in_transaction(
                        &connection,
                        candidate,
                        plan,
                        options.rationale,
                    )?;
                }
                validate_conflict_resolution(
                    &connection,
                    write.workspace_id(),
                    plan,
                    options.rationale,
                )?;
            }
            let heads = read::record_heads_in_current_snapshot(
                &connection,
                write.workspace_id(),
                &fields,
                now,
            )?;
            let predecessor = validate_head(&heads, &fields)?;
            record_prepared_remember_txn_write_in_txn(&connection, &write)?;
            boundary(Stage::Memory, &connection)?;
            if !connection.set_memory_typed_fields_json(write.memory_id(), Some(&typed_fields))? {
                return Err(RecordError::from(decide_storage_error(
                    "Decision body is missing from its transaction",
                )));
            }
            boundary(Stage::Fields, &connection)?;
            let stored = connection.get_memory(write.memory_id())?.ok_or_else(|| {
                decide_storage_error("Decision body is missing from its transaction")
            })?;
            let mut report = DecideRecordReport {
                schema: DECIDE_RECORD_SCHEMA_V1,
                version: env!("CARGO_PKG_VERSION"),
                status: "recorded".to_owned(),
                dry_run: false,
                persisted: true,
                workspace_id: write.workspace_id().to_owned(),
                database_path: scope.database_path.display().to_string(),
                // The new row has no predecessor edge until replacement below.
                decision: memory_to_decide_item(&connection, &stored, now, 0)?,
                superseded: None,
                memory_audit_id: Some(memory_audit_id(&connection, &write)?),
                memory_index_job_id: Some(write.index_job_id().to_owned()),
                link_audit_id: None,
                expire_audit_id: None,
                warnings: Vec::new(),
            };
            if let Some(predecessor) = predecessor {
                replace_memory(
                    &connection,
                    &write,
                    Supersession {
                        from_memory_id: write.memory_id(),
                        to_memory_id: &predecessor.memory_id,
                        details: json!({
                            "schema": "ee.decide.supersede.v1",
                            "normalizedTopic": fields.normalized_topic,
                            "fromMemoryId": write.memory_id(),
                            "toMemoryId": predecessor.memory_id,
                            "supersededAt": stored.updated_at,
                        }),
                    },
                    options.actor,
                    &stored.updated_at,
                    &mut report,
                    &mut boundary,
                )?;
            }
            let results = if let Some(plan) = resolution {
                apply_resolution(
                    &connection,
                    &write,
                    plan,
                    candidate,
                    options.actor,
                    &stored.updated_at,
                    &mut report,
                    &mut boundary,
                )?
            } else {
                Vec::new()
            };
            let candidate_audit_id = candidate
                .map(|candidate| {
                    crate::core::conflict_proposal::complete_in_transaction(
                        &connection,
                        candidate,
                        write.memory_id(),
                        results.iter().find_map(|result| {
                            (result.action == "mergeMemories")
                                .then_some(result.created_memory_id.as_deref())
                                .flatten()
                        }),
                        &stored.updated_at,
                        options.actor,
                    )
                })
                .transpose()?;
            // All fallible source-dependent report construction is pre-COMMIT.
            report.decision.chain_depth =
                lineage::chain_depth(&connection, write.workspace_id(), write.memory_id())?;
            boundary(Stage::Report, &connection)?;
            Ok::<_, RecordError>(ConflictResolutionReport {
                decision: report,
                results,
                candidate_audit_id,
                candidate_applied_at: candidate.map(|_| stored.updated_at.clone()),
            })
        })
        .map_err(|error| error.0)?;

    // Indexing and stream append happen only AFTER the entire transition.
    // Retain the ordinary best-effort remember publication behavior, without
    // allowing any derived/reporting failure to erase the source acknowledgement.
    match finish_prepared_remember_txn_write(&connection, write) {
        Ok(finished) if finished.index_status != "indexed" => report.decision.warnings.push(
            "Decision committed; search indexing remains queued, failed, or unverified. Do not repeat the decision write. Inspect memoryIndexJobId or run ee index rebuild for this workspace."
                .to_owned(),
        ),
        Ok(_) => {}
        Err(_) => report.decision.warnings.push(
            "Decision committed, but post-commit follow-up is incomplete. Do not repeat the decision write. Inspect memoryId and run ee doctor --json; the durable index job remains recoverable."
                .to_owned(),
        ),
    }
    Ok(report)
}

fn stale_conflict() -> DomainError {
    DomainError::UsageCodeWithDetails {
        code: "conflict_resolve_stale_surface",
        message: "The conflict pair changed before apply; no resolution was recorded.".to_owned(),
        repair: Some("Run ee conflict list --json and plan against the current pair.".to_owned()),
        details_json: json!({
            "failureModeCode": "conflict_resolve_stale_surface",
            "currentPairs": [],
        })
        .to_string(),
    }
}

fn invalid_conflict_plan() -> DomainError {
    DomainError::Usage {
        message: "Conflict resolution does not match its complete reviewed plan".to_owned(),
        repair: Some("Use ee conflict resolve to create a fresh plan before --apply.".to_owned()),
    }
}

fn validate_conflict_resolution(
    connection: &DbConnection,
    workspace_id: &str,
    plan: &ConflictResolutionPlan,
    reason: &str,
) -> Result<(), RecordError> {
    let ids = [plan.memory_a.as_str(), plan.memory_b.as_str()];
    let seals =
        crate::core::memory_lifecycle::load_memory_seals_for_admission(connection, workspace_id)
            .map_err(|_| decide_storage_error("Could not verify conflict source authority"))?;
    let held =
        crate::core::memory_lifecycle::pending_memory_review_ids(connection, workspace_id, &ids)
            .map_err(|_| decide_storage_error("Could not verify conflict source authority"))?;
    if seals
        .iter()
        .any(|seal| seal.is_sealed() && ids.contains(&seal.memory_id.as_str()))
        || !held.is_empty()
    {
        return Err(conflict_authority_error().into());
    }
    for id in ids {
        let memory = connection.get_memory(id)?.ok_or_else(stale_conflict)?;
        if memory.workspace_id != workspace_id {
            return Err(DomainError::PolicyDenied {
                message: "Both conflict memories must belong to the addressed workspace".to_owned(),
                repair: Some(
                    "Select a conflict from this workspace's ee conflict list.".to_owned(),
                ),
            }
            .into());
        }
        if memory.content == crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT {
            return Err(conflict_authority_error().into());
        }
    }
    let surface = assemble_conflict_surface(connection, ContradictionDetectionConfig::default());
    let (scope_a_tags, scope_b_tags) = if plan.verb == ResolveVerb::ScopeSplit {
        let scope_tags = |id: &str| {
            plan.actions.iter().find_map(|action| match action {
                PlannedResolutionAction::AddTags { memory_id, tags } if memory_id == id => {
                    Some(tags.clone())
                }
                _ => None,
            })
        };
        (
            scope_tags(&plan.memory_a).ok_or_else(invalid_conflict_plan)?,
            scope_tags(&plan.memory_b).ok_or_else(invalid_conflict_plan)?,
        )
    } else {
        (Vec::new(), Vec::new())
    };
    let request = ConflictResolveRequest {
        memory_a: &plan.memory_a,
        memory_b: &plan.memory_b,
        verb: plan.verb,
        keep: plan.keep.as_deref(),
        reason,
        scope_a_tags,
        scope_b_tags,
        merge_content: plan.actions.iter().find_map(|action| match action {
            PlannedResolutionAction::MergeMemories { content } => Some(content.as_str()),
            _ => None,
        }),
    };
    match plan_conflict_resolution(&surface, &request) {
        ConflictResolutionOutcome::Plan(current) if &current == plan => {
            if plan.verb == ResolveVerb::Merge {
                crate::core::conflict_proposal::merge_sources(connection, workspace_id, plan)?;
            }
            Ok(())
        }
        ConflictResolutionOutcome::StaleSurface { .. } => Err(stale_conflict().into()),
        ConflictResolutionOutcome::PolicyDenied { message, repair } => {
            Err(DomainError::PolicyDenied {
                message,
                repair: Some(repair),
            }
            .into())
        }
        _ => Err(invalid_conflict_plan().into()),
    }
}

fn conflict_authority_error() -> DomainError {
    DomainError::PolicyDenied {
        message: "Conflict source authority prevents resolution; no resolution was recorded".to_owned(),
        repair: Some("Review pending feedback or explicitly reveal sealed memories before resolving this pair.".to_owned()),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "one caller-owned resolution transaction"
)]
fn apply_resolution(
    connection: &DbConnection,
    write: &PreparedRememberTxnWrite,
    plan: &ConflictResolutionPlan,
    candidate: Option<&crate::db::StoredCurationCandidate>,
    actor: Option<&str>,
    at: &str,
    report: &mut DecideRecordReport,
    boundary: &mut impl FnMut(Stage, &DbConnection) -> Result<(), DomainError>,
) -> Result<Vec<ConflictResolutionActionResult>, RecordError> {
    let mut details = json!({
        "schema": CONFLICT_RESOLUTION_AUDIT_SCHEMA,
        "conflictId": plan.conflict_id,
        "memoryA": plan.memory_a,
        "memoryB": plan.memory_b,
        "verb": plan.verb.as_str(),
        "resolutionDecisionId": write.memory_id(),
    });
    let mut results = Vec::with_capacity(plan.actions.len());
    for action in &plan.actions {
        let result = match action {
            PlannedResolutionAction::MergeMemories { content } => {
                let candidate = candidate.ok_or_else(invalid_conflict_plan)?;
                let result = merge_memories(
                    connection, write, plan, candidate, content, actor, at, report, boundary,
                )?;
                details["mergedMemoryId"] = json!(result.created_memory_id);
                result
            }
            PlannedResolutionAction::RecordDecision { .. } => {
                let mut audit_ids: Vec<_> = [
                    report.memory_audit_id.clone(),
                    report.link_audit_id.clone(),
                    report.expire_audit_id.clone(),
                ]
                .into_iter()
                .flatten()
                .collect();
                if plan.verb == ResolveVerb::Supersede {
                    let keeper = plan.keep.as_deref().ok_or_else(invalid_conflict_plan)?;
                    let loser = plan.lose.as_deref().ok_or_else(invalid_conflict_plan)?;
                    replace_memory(
                        connection,
                        write,
                        Supersession {
                            from_memory_id: keeper,
                            to_memory_id: loser,
                            details: json!({
                                "schema": CONFLICT_SUPERSESSION_AUDIT_SCHEMA,
                                "conflictId": plan.conflict_id,
                                "resolutionDecisionId": write.memory_id(),
                                "fromMemoryId": keeper,
                                "toMemoryId": loser,
                                "supersededAt": at,
                            }),
                        },
                        actor,
                        at,
                        report,
                        boundary,
                    )?;
                    audit_ids.extend(
                        [report.link_audit_id.clone(), report.expire_audit_id.clone()]
                            .into_iter()
                            .flatten(),
                    );
                }
                ConflictResolutionActionResult {
                    action: "recordDecision".to_owned(),
                    audit_ids,
                    created_memory_id: Some(write.memory_id().to_owned()),
                }
            }
            PlannedResolutionAction::TombstoneMemory { memory_id, reason } => {
                let audit = connection
                    .tombstone_memory_audited_in_current_transaction(
                        memory_id,
                        write.workspace_id(),
                        actor,
                        Some(reason),
                    )?
                    .ok_or_else(stale_conflict)?;
                queue_conflict_source_index(connection, write.workspace_id(), memory_id)?;
                ConflictResolutionActionResult {
                    action: "tombstoneMemory".to_owned(),
                    audit_ids: vec![audit],
                    created_memory_id: None,
                }
            }
            PlannedResolutionAction::AddTags { memory_id, tags } => {
                let current = connection.get_memory_tags(memory_id)?;
                let added: Vec<_> = tags
                    .iter()
                    .filter(|tag| !current.contains(tag))
                    .cloned()
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                let mut audit_ids = Vec::new();
                if !added.is_empty() {
                    audit_ids.push(connection.add_memory_tags_audited_in_current_transaction(
                        memory_id,
                        write.workspace_id(),
                        &added,
                        actor,
                    )?);
                    queue_conflict_source_index(connection, write.workspace_id(), memory_id)?;
                }
                ConflictResolutionActionResult {
                    action: "addTags".to_owned(),
                    audit_ids,
                    created_memory_id: None,
                }
            }
            PlannedResolutionAction::CreateLink { from, to, relation } => {
                let relation =
                    MemoryLinkRelation::parse(relation).ok_or_else(invalid_conflict_plan)?;
                let audit = record_resolution_link(
                    connection,
                    write,
                    CreateMemoryLinkInput {
                        src_memory_id: from.clone(),
                        dst_memory_id: to.clone(),
                        relation,
                        weight: 1.0,
                        confidence: 1.0,
                        directed: false,
                        evidence_count: 1,
                        last_reinforced_at: None,
                        source: MemoryLinkSource::Agent,
                        created_by: actor.map(str::to_owned),
                        metadata_json: None,
                    },
                    &details,
                )?;
                ConflictResolutionActionResult {
                    action: "createLink".to_owned(),
                    audit_ids: audit.into_iter().collect(),
                    created_memory_id: None,
                }
            }
        };
        results.push(result);
        boundary(Stage::ResolutionAction, connection)?;
    }
    let decision = results
        .iter_mut()
        .find(|result| result.action == "recordDecision")
        .ok_or_else(invalid_conflict_plan)?;
    // Rationale provenance is a different relation from decision revision
    // ancestry. Both source rows remain addressable, including rejected history.
    for source in [&plan.memory_a, &plan.memory_b] {
        let audit = record_resolution_link(
            connection,
            write,
            CreateMemoryLinkInput {
                src_memory_id: write.memory_id().to_owned(),
                dst_memory_id: source.clone(),
                relation: MemoryLinkRelation::DerivedFrom,
                weight: 1.0,
                confidence: 1.0,
                directed: true,
                evidence_count: 1,
                last_reinforced_at: None,
                source: MemoryLinkSource::Agent,
                created_by: actor.map(str::to_owned),
                metadata_json: None,
            },
            &details,
        )?;
        decision.audit_ids.extend(audit);
        boundary(Stage::Provenance, connection)?;
    }
    let audit_id = generate_audit_id();
    connection.insert_audit(
        &audit_id,
        &CreateAuditInput {
            workspace_id: Some(write.workspace_id().to_owned()),
            actor: actor.map(str::to_owned),
            action: CONFLICT_RESOLUTION_AUDIT_ACTION.to_owned(),
            target_type: Some("conflict".to_owned()),
            target_id: Some(plan.conflict_id.clone()),
            details: Some(details.to_string()),
        },
    )?;
    decision.audit_ids.push(audit_id);
    boundary(Stage::Completion, connection)?;
    Ok(results)
}

#[allow(clippy::too_many_arguments, reason = "one reviewed merge transaction")]
fn merge_memories(
    connection: &DbConnection,
    write: &PreparedRememberTxnWrite,
    plan: &ConflictResolutionPlan,
    candidate: &crate::db::StoredCurationCandidate,
    content: &str,
    actor: Option<&str>,
    at: &str,
    report: &DecideRecordReport,
    boundary: &mut impl FnMut(Stage, &DbConnection) -> Result<(), DomainError>,
) -> Result<ConflictResolutionActionResult, RecordError> {
    // Re-read complete sources under the same writer fence that creates the
    // output and closes both active lifetimes. Never transplant peer signatures
    // or source trust into a newly authored summary.
    let sources =
        crate::core::conflict_proposal::merge_sources(connection, write.workspace_id(), plan)?;
    let memory_id = MemoryId::now().to_string();
    let confidence = 0.5_f32
        .min(sources.source_a.confidence)
        .min(sources.source_b.confidence);
    let memory = CreateMemoryInput {
        workspace_id: write.workspace_id().to_owned(),
        level: "semantic".to_owned(),
        kind: "summary".to_owned(),
        content: content.to_owned(),
        workflow_id: sources.source_a.workflow_id.clone(),
        confidence,
        utility: 0.5_f32
            .min(sources.source_a.utility)
            .min(sources.source_b.utility),
        importance: 0.5_f32
            .min(sources.source_a.importance)
            .min(sources.source_b.importance),
        provenance_uri: Some(format!("ee://curation-candidate/{}", candidate.id)),
        trust_class: "agent_assertion".to_owned(),
        trust_subclass: crate::core::memory_scope::remember_trust_subclass("conflict_merge"),
        tags: sources.tags,
        valid_from: Some(at.to_owned()),
        valid_to: sources.valid_to,
    };
    connection.insert_memory_with_timestamps(&memory_id, &memory, at, at, &memory_id)?;
    let details = json!({
        "schema": "ee.conflict.merge.v1",
        "candidateId": candidate.id,
        "sourceId": candidate.source_id,
        "conflictId": plan.conflict_id,
        "mergedMemoryId": memory_id,
        "sourceMemoryIds": [&plan.memory_a, &plan.memory_b],
        "resolutionDecisionId": write.memory_id(),
        "trustClass": memory.trust_class,
        "confidence": confidence,
        "validTo": memory.valid_to,
    });
    let creation_audit = generate_audit_id();
    connection.insert_audit(
        &creation_audit,
        &CreateAuditInput {
            workspace_id: Some(write.workspace_id().to_owned()),
            actor: actor.map(str::to_owned),
            action: audit_actions::MEMORY_CREATE.to_owned(),
            target_type: Some("memory".to_owned()),
            target_id: Some(memory_id.clone()),
            details: Some(details.to_string()),
        },
    )?;
    boundary(Stage::MergeMemory, connection)?;
    let mut audit_ids = vec![creation_audit];
    for source in [&plan.memory_a, &plan.memory_b] {
        audit_ids.extend(record_resolution_link(
            connection,
            write,
            CreateMemoryLinkInput {
                src_memory_id: memory_id.clone(),
                dst_memory_id: source.clone(),
                relation: MemoryLinkRelation::DerivedFrom,
                weight: 1.0,
                confidence,
                directed: true,
                evidence_count: 1,
                last_reinforced_at: None,
                source: MemoryLinkSource::Agent,
                created_by: actor.map(str::to_owned),
                metadata_json: None,
            },
            &details,
        )?);
        // The rationale's own revision report must not be overwritten by either
        // source replacement; both source transitions belong to this action.
        let mut replacement = report.clone();
        replacement.link_audit_id = None;
        replacement.expire_audit_id = None;
        replace_memory(
            connection,
            write,
            Supersession {
                from_memory_id: &memory_id,
                to_memory_id: source,
                details: details.clone(),
            },
            actor,
            at,
            &mut replacement,
            boundary,
        )?;
        audit_ids.extend(
            [replacement.link_audit_id, replacement.expire_audit_id]
                .into_iter()
                .flatten(),
        );
        boundary(Stage::MergeSource, connection)?;
    }
    connection.insert_search_index_job(
        &crate::core::conflict_proposal::merge_index_job_id(&memory_id),
        &CreateSearchIndexJobInput {
            workspace_id: write.workspace_id().to_owned(),
            job_type: SearchIndexJobType::SingleDocument,
            document_source: Some("memory".to_owned()),
            document_id: Some(memory_id.clone()),
            documents_total: 1,
        },
    )?;
    boundary(Stage::Index, connection)?;
    Ok(ConflictResolutionActionResult {
        action: "mergeMemories".to_owned(),
        audit_ids,
        created_memory_id: Some(memory_id),
    })
}

fn queue_conflict_source_index(
    connection: &DbConnection,
    workspace_id: &str,
    memory_id: &str,
) -> Result<(), RecordError> {
    let id = format!(
        "sidx_{}",
        MemoryId::now().to_string().trim_start_matches("mem_")
    );
    connection.insert_search_index_job(
        &id,
        &CreateSearchIndexJobInput {
            workspace_id: workspace_id.to_owned(),
            job_type: SearchIndexJobType::SingleDocument,
            document_source: Some("memory".to_owned()),
            document_id: Some(memory_id.to_owned()),
            documents_total: 1,
        },
    )?;
    Ok(())
}

fn record_resolution_link(
    connection: &DbConnection,
    write: &PreparedRememberTxnWrite,
    mut input: CreateMemoryLinkInput,
    details: &JsonValue,
) -> Result<Option<String>, RecordError> {
    let existing = connection.get_memory_link_by_edge(
        &input.src_memory_id,
        &input.dst_memory_id,
        input.relation,
    )?;
    let reverse = if !input.directed && existing.is_none() {
        connection.get_memory_link_by_edge(
            &input.dst_memory_id,
            &input.src_memory_id,
            input.relation,
        )?
    } else {
        None
    };
    if existing.is_some() || reverse.is_some() {
        // Preserve pre-existing link direction and provenance. The canonical
        // resolution audit, not arbitrary link metadata, settles the conflict.
        return Ok(None);
    }
    let id = format!(
        "link_{}",
        MemoryId::now().to_string().trim_start_matches("mem_")
    );
    let mut details = details.clone();
    details["linkId"] = json!(id);
    details["fromMemoryId"] = json!(input.src_memory_id);
    details["toMemoryId"] = json!(input.dst_memory_id);
    details["relation"] = json!(input.relation.as_str());
    input.metadata_json = Some(details.to_string());
    connection.insert_memory_link(&id, &input)?;
    let audit_id = generate_audit_id();
    connection.insert_audit(
        &audit_id,
        &CreateAuditInput {
            workspace_id: Some(write.workspace_id().to_owned()),
            actor: input.created_by,
            action: audit_actions::MEMORY_LINK_CREATE.to_owned(),
            target_type: Some("memory_link".to_owned()),
            target_id: Some(id),
            details: Some(details.to_string()),
        },
    )?;
    Ok(Some(audit_id))
}

fn validated_fields(fields: &DecisionFields) -> Result<String, DomainError> {
    if fields.topic.contains(['\r', '\n']) {
        return Err(DomainError::Usage {
            message: "Decision topic must be a single line so its stored identity is unambiguous"
                .to_owned(),
            repair: Some("Keep the topic on one line; put details in --rationale.".to_owned()),
        });
    }
    // Exact structured fields must not reinsert a secret that remember removed
    // from prose. Screen each decoded element, including multiline strings.
    for value in std::iter::once(fields.topic.as_str())
        .chain(std::iter::once(fields.rationale.as_str()))
        .chain(fields.options.iter().map(String::as_str))
    {
        if crate::policy::screen_external_text_for_ingestion(value).redacted {
            return Err(DomainError::PolicyDenied {
                message: "Decision fields contain sensitive content; no decision was recorded"
                    .to_owned(),
                repair: Some(
                    "Remove sensitive values from the decision fields and retry.".to_owned(),
                ),
            });
        }
    }
    crate::models::memory::canonicalize_typed_memory_fields_json(
        &MemoryKind::Decision,
        &json!({
            "options": fields.options,
            "chosen": fields.chosen,
            "rationale": fields.rationale,
            "supersedes": fields.supersedes,
            "revisit_by": fields.revisit_by,
        })
        .to_string(),
    )
    .map_err(|_| DomainError::Usage {
        message: "Invalid structured decision fields; no decision was recorded".to_owned(),
        repair: Some("Run ee decide record --help.".to_owned()),
    })
}

fn validate_head<'a>(
    heads: &'a [DecideItem],
    fields: &DecisionFields,
) -> Result<Option<&'a DecideItem>, DomainError> {
    let predecessor = fields
        .supersedes
        .as_deref()
        .map(|id| {
            heads
                .iter()
                .find(|item| item.memory_id == id)
                .ok_or_else(|| DomainError::NotFound {
                    resource: "current decision memory".to_owned(),
                    id: id.to_owned(),
                    repair: Some(
                        "Run ee decide list --json and select the current predecessor.".to_owned(),
                    ),
                })
        })
        .transpose()?;
    if let Some(prior) = predecessor
        && prior.normalized_topic != fields.normalized_topic
    {
        return Err(decide_usage_with_details(
            "decision_supersedes_topic_mismatch",
            "Superseded decision topic does not match the new decision topic.",
            json!({"failureModeCode": "decision_supersedes_topic_mismatch", "supersedes": prior.memory_id}),
        ));
    }
    if let Some(prior) = heads.iter().find(|item| {
        item.normalized_topic == fields.normalized_topic
            && fields.supersedes.as_deref() != Some(item.memory_id.as_str())
    }) {
        return Err(decide_usage_with_details(
            "decision_topic_requires_supersedes",
            "A live decision already exists for this normalized topic; replace its current head with --supersedes before retrying.",
            json!({"failureModeCode": "decision_topic_requires_supersedes", "priorMemoryId": prior.memory_id, "normalizedTopic": fields.normalized_topic}),
        ));
    }
    Ok(predecessor)
}

fn memory_audit_id(
    connection: &DbConnection,
    write: &PreparedRememberTxnWrite,
) -> Result<String, RecordError> {
    let rows = connection.query(
        "SELECT id FROM audit_log WHERE workspace_id = ?1 AND action = ?2 AND target_type = 'memory' AND target_id = ?3 ORDER BY id ASC LIMIT 2",
        &[
            Value::Text(write.workspace_id().to_owned()),
            Value::Text(audit_actions::MEMORY_CREATE.to_owned()),
            Value::Text(write.memory_id().to_owned()),
        ],
    )?;
    if rows.len() != 1 {
        return Err(decide_storage_error("Decision must have exactly one creation audit").into());
    }
    rows[0]
        .get(0)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| decide_storage_error("Invalid decision audit identity").into())
}

#[allow(
    clippy::too_many_arguments,
    reason = "all inputs belong to the one decision transition"
)]
fn replace_memory(
    connection: &DbConnection,
    write: &PreparedRememberTxnWrite,
    mut replacement: Supersession<'_>,
    actor: Option<&str>,
    at: &str,
    report: &mut DecideRecordReport,
    boundary: &mut impl FnMut(Stage, &DbConnection) -> Result<(), DomainError>,
) -> Result<(), RecordError> {
    let actor = actor.or(Some("ee decide record")).map(str::to_owned);
    let suffix = MemoryId::now().to_string();
    let suffix = suffix.trim_start_matches("mem_");
    let existing = connection.get_memory_link_by_edge(
        replacement.from_memory_id,
        replacement.to_memory_id,
        MemoryLinkRelation::Supersedes,
    )?;
    if existing.as_ref().is_some_and(|link| !link.directed) {
        return Err(DomainError::Usage {
            message: "An existing supersedes link is undirected; no replacement was recorded"
                .to_owned(),
            repair: Some(
                "Repair the supersedes link's direction before resolving this pair.".to_owned(),
            ),
        }
        .into());
    }
    let link_id = existing
        .as_ref()
        .map_or_else(|| format!("link_{suffix}"), |link| link.id.clone());
    let job_id = format!("sidx_{suffix}");
    let link_audit = existing.is_none().then(generate_audit_id);
    let expire_audit = generate_audit_id();
    replacement.details["linkId"] = json!(link_id);
    let details = replacement.details.to_string();
    if let Some(link_audit) = &link_audit {
        connection.insert_memory_link(
            &link_id,
            &CreateMemoryLinkInput {
                src_memory_id: replacement.from_memory_id.to_owned(),
                dst_memory_id: replacement.to_memory_id.to_owned(),
                relation: MemoryLinkRelation::Supersedes,
                weight: 1.0,
                confidence: 1.0,
                directed: true,
                evidence_count: 1,
                last_reinforced_at: None,
                source: MemoryLinkSource::Agent,
                created_by: actor.clone(),
                metadata_json: Some(details.clone()),
            },
        )?;
        connection.insert_audit(
            link_audit,
            &CreateAuditInput {
                workspace_id: Some(write.workspace_id().to_owned()),
                actor: actor.clone(),
                action: audit_actions::MEMORY_LINK_CREATE.to_owned(),
                target_type: Some("memory_link".to_owned()),
                target_id: Some(link_id),
                details: Some(details.clone()),
            },
        )?;
    }
    boundary(Stage::Link, connection)?;
    let changed_expiry = connection.expire_memory_valid_to(replacement.to_memory_id, at)?;
    if !connection.mark_memory_superseded(replacement.to_memory_id, at)? {
        return Err(decide_storage_error("Superseded memory changed during replacement").into());
    }
    connection.insert_audit(
        &expire_audit,
        &CreateAuditInput {
            workspace_id: Some(write.workspace_id().to_owned()),
            actor: actor.clone(),
            action: audit_actions::MEMORY_EXPIRE.to_owned(),
            target_type: Some("memory".to_owned()),
            target_id: Some(replacement.to_memory_id.to_owned()),
            details: Some(details),
        },
    )?;
    let previous = connection
        .get_memory(replacement.to_memory_id)?
        .ok_or_else(|| decide_storage_error("Superseded memory disappeared"))?;
    if changed_expiry
        && previous.level == "semantic"
        && connection
            .apply_memory_level_transition_in_current_transaction(
                replacement.to_memory_id,
                &ApplyMemoryLevelTransitionInput {
                    workspace_id: write.workspace_id().to_owned(),
                    expected_level: Some("semantic".to_owned()),
                    level: "episodic".to_owned(),
                    updated_at: at.to_owned(),
                    actor,
                    reason: "time_bound_fact".to_owned(),
                    automatic: true,
                    event: "valid_to.set".to_owned(),
                    evidence_refs: vec![at.to_owned(), write.memory_id().to_owned()],
                    source_action: Some(audit_actions::MEMORY_EXPIRE.to_owned()),
                },
            )?
            .is_none()
    {
        return Err(
            decide_storage_error("Superseded memory lifecycle changed during replacement").into(),
        );
    }
    boundary(Stage::Predecessor, connection)?;
    connection.insert_search_index_job(
        &job_id,
        &CreateSearchIndexJobInput {
            workspace_id: write.workspace_id().to_owned(),
            job_type: SearchIndexJobType::SingleDocument,
            document_source: Some("memory".to_owned()),
            document_id: Some(replacement.to_memory_id.to_owned()),
            documents_total: 1,
        },
    )?;
    boundary(Stage::Index, connection)?;
    report.link_audit_id = link_audit;
    report.expire_audit_id = Some(expire_audit);
    report.superseded = Some(DecideMemoryRef {
        memory_id: replacement.to_memory_id.to_owned(),
        valid_to: previous.valid_to,
        status: if changed_expiry {
            "expired"
        } else {
            "already_expired"
        }
        .to_owned(),
    });
    Ok(())
}

#[cfg(test)]
#[path = "decide_atomic_tests.rs"]
mod tests;
