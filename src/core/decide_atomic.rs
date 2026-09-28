//! One writer-owned decision transition, including its structured payload,
//! predecessor, lineage, audit, and durable index obligations.

use super::*;
use crate::core::memory::{
    PreparedRememberTxnWrite, RememberMemoryOptions, finish_prepared_remember_txn_write,
    prepare_remember_txn_write_for_connection, record_prepared_remember_txn_write_in_txn,
    remember_memory,
};
use crate::db::{
    ApplyMemoryLevelTransitionInput, CreateAuditInput, CreateMemoryLinkInput,
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
    Link,
    Predecessor,
    Index,
    Report,
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

fn record_with_boundary(
    options: &DecideRecordOptions<'_>,
    mut boundary: impl FnMut(Stage, &DbConnection) -> Result<(), DomainError>,
) -> Result<DecideRecordReport, DomainError> {
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
        return Ok(DecideRecordReport {
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
                replace_predecessor(
                    &connection,
                    &write,
                    predecessor,
                    &fields,
                    options.actor,
                    &stored.updated_at,
                    &mut report,
                    &mut boundary,
                )?;
            }
            // All fallible source-dependent report construction is pre-COMMIT.
            report.decision.chain_depth =
                lineage::chain_depth(&connection, write.workspace_id(), write.memory_id())?;
            boundary(Stage::Report, &connection)?;
            Ok::<_, RecordError>(report)
        })
        .map_err(|error| error.0)?;

    // Indexing and stream append happen only AFTER the entire transition.
    // Retain the ordinary best-effort remember publication behavior, without
    // allowing any derived/reporting failure to erase the source acknowledgement.
    match finish_prepared_remember_txn_write(&connection, write) {
        Ok(finished) if finished.index_status != "indexed" => report.warnings.push(
            "Decision committed; search indexing remains queued, failed, or unverified. Do not repeat the decision write. Inspect memoryIndexJobId or run ee index rebuild for this workspace."
                .to_owned(),
        ),
        Ok(_) => {}
        Err(_) => report.warnings.push(
            "Decision committed, but post-commit follow-up is incomplete. Do not repeat the decision write. Inspect memoryId and run ee doctor --json; the durable index job remains recoverable."
                .to_owned(),
        ),
    }
    Ok(report)
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
fn replace_predecessor(
    connection: &DbConnection,
    write: &PreparedRememberTxnWrite,
    predecessor: &DecideItem,
    fields: &DecisionFields,
    actor: Option<&str>,
    at: &str,
    report: &mut DecideRecordReport,
    boundary: &mut impl FnMut(Stage, &DbConnection) -> Result<(), DomainError>,
) -> Result<(), RecordError> {
    let actor = actor.or(Some("ee decide record")).map(str::to_owned);
    let suffix = MemoryId::now().to_string();
    let suffix = suffix.trim_start_matches("mem_");
    let link_id = format!("link_{suffix}");
    let job_id = format!("sidx_{suffix}");
    let link_audit = generate_audit_id();
    let expire_audit = generate_audit_id();
    let details = json!({
        "schema": "ee.decide.supersede.v1",
        "normalizedTopic": fields.normalized_topic,
        "fromMemoryId": write.memory_id(),
        "toMemoryId": predecessor.memory_id,
        "supersededAt": at,
    })
    .to_string();
    connection.insert_memory_link(
        &link_id,
        &CreateMemoryLinkInput {
            src_memory_id: write.memory_id().to_owned(),
            dst_memory_id: predecessor.memory_id.clone(),
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
        &link_audit,
        &CreateAuditInput {
            workspace_id: Some(write.workspace_id().to_owned()),
            actor: actor.clone(),
            action: audit_actions::MEMORY_LINK_CREATE.to_owned(),
            target_type: Some("memory_link".to_owned()),
            target_id: Some(link_id),
            details: Some(details.clone()),
        },
    )?;
    boundary(Stage::Link, connection)?;
    let changed_expiry = connection.expire_memory_valid_to(&predecessor.memory_id, at)?;
    if !connection.mark_memory_superseded(&predecessor.memory_id, at)? {
        return Err(decide_storage_error("Decision predecessor changed during replacement").into());
    }
    connection.insert_audit(
        &expire_audit,
        &CreateAuditInput {
            workspace_id: Some(write.workspace_id().to_owned()),
            actor: actor.clone(),
            action: audit_actions::MEMORY_EXPIRE.to_owned(),
            target_type: Some("memory".to_owned()),
            target_id: Some(predecessor.memory_id.clone()),
            details: Some(details),
        },
    )?;
    let previous = connection
        .get_memory(&predecessor.memory_id)?
        .ok_or_else(|| decide_storage_error("Decision predecessor disappeared"))?;
    if changed_expiry
        && previous.level == "semantic"
        && connection
            .apply_memory_level_transition_in_current_transaction(
                &predecessor.memory_id,
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
        return Err(decide_storage_error(
            "Decision predecessor lifecycle changed during replacement",
        )
        .into());
    }
    boundary(Stage::Predecessor, connection)?;
    connection.insert_search_index_job(
        &job_id,
        &CreateSearchIndexJobInput {
            workspace_id: write.workspace_id().to_owned(),
            job_type: SearchIndexJobType::SingleDocument,
            document_source: Some("memory".to_owned()),
            document_id: Some(predecessor.memory_id.clone()),
            documents_total: 1,
        },
    )?;
    boundary(Stage::Index, connection)?;
    report.link_audit_id = Some(link_audit);
    report.expire_audit_id = Some(expire_audit);
    report.superseded = Some(DecideMemoryRef {
        memory_id: predecessor.memory_id.clone(),
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
