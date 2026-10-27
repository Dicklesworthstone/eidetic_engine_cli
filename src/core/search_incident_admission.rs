//! Current-source incident proof before search ranking and query assistance.
//!
//! Live admission is necessary but does not certify an old repair summary.
//! Rehydrate only requested native identities inside one source snapshot and
//! reuse the shared failure/repair/retry interpreter. Neither indexed labels,
//! historical query flags nor a valid proof can expand source admission.

use std::collections::{BTreeMap, BTreeSet};

use super::memory_revisions::RevisionReadSnapshot;
use super::{DbConnection, SearchDegradation, SearchHit, SearchOptions};
use crate::core::incident_card::proof::{CardChecks, requires_proof};
use crate::core::search::{
    MeshQueryVisibility, canonical_evidence_search_metadata, mesh_query_visibility,
};
use crate::db::{DbError, DbOperation};
use crate::models::EvidenceId;

const PAGE_SIZE: usize = 128;
const UNAVAILABLE: &str = "incident_card_proof_unavailable";
const FILTERED: &str = "incident_card_source_filtered";

struct EvidenceProjection {
    metadata: serde_json::Value,
    revision: String,
    content_hash: String,
}

#[derive(Default)]
struct Projections {
    evidence: BTreeMap<String, EvidenceProjection>,
    proof_unavailable: bool,
}

fn native_evidence(hit: &SearchHit) -> bool {
    // Match the existing live-evidence gate. A changed indexed kind or source
    // label must not turn a native evidence identity into an ordinary document.
    hit.doc_id.starts_with("ev_")
}

fn source_unavailable() -> DbError {
    DbError::MalformedRow {
        operation: DbOperation::Query,
        message: "Could not verify current incident-card source authority".to_owned(),
    }
}

fn read_projections(
    options: &SearchOptions,
    ids: &BTreeSet<&str>,
    connection: &DbConnection,
) -> Result<Projections, DbError> {
    let Some(workspace) = crate::core::workspace::addressed_workspace_row(
        connection,
        &options.workspace_path,
        &options.resolve_database_path(),
    )
    .map_err(|_| source_unavailable())?
    else {
        return Ok(Projections::default());
    };
    let canonical_ids = ids
        .iter()
        .copied()
        .filter(|id| {
            id.parse::<EvidenceId>()
                .is_ok_and(|parsed| parsed.to_string() == *id)
        })
        .collect::<Vec<_>>();
    let mut result = Projections::default();
    let mut checks = CardChecks::default();
    let mut card_ids = BTreeSet::new();
    // Bound transient source bodies to one candidate page. Across pages keep
    // public projections and card commitments, not full transcript bodies.
    for page in canonical_ids.chunks(PAGE_SIZE) {
        for row in connection.get_evidence_spans_with_sessions(page)? {
            if !page.contains(&row.span.id.as_str()) {
                return Err(source_unavailable());
            }
            if !row.is_search_admitted(&workspace.id) {
                continue;
            }
            let span = row.span;
            if requires_proof(&span) {
                card_ids.insert(span.id.clone());
                checks.observe(&span)?;
            }
            let projection = EvidenceProjection {
                metadata: canonical_evidence_search_metadata(&span),
                revision: span.pack_entity_revision(),
                content_hash: span.content_hash.clone(),
            };
            if result.evidence.insert(span.id, projection).is_some() {
                return Err(source_unavailable());
            }
        }
    }
    let refused = match checks.withheld(connection, &workspace.id) {
        Ok(refused) => refused,
        Err(_) => {
            // A failed session proof cannot release a partially checked set.
            // Ordinary admitted transcript evidence does not depend on it.
            result.proof_unavailable = true;
            card_ids
        }
    };
    result.evidence.retain(|id, _| !refused.contains(id));
    Ok(result)
}

fn load_projections(
    options: &SearchOptions,
    ids: &BTreeSet<&str>,
    read_connection: Option<&DbConnection>,
) -> Result<Projections, DbError> {
    if let Some(connection) = read_connection {
        let snapshot = RevisionReadSnapshot::begin_or_borrow(connection)?;
        let result = read_projections(options, ids, connection)?;
        snapshot.finish()?;
        return Ok(result);
    }
    let connection = DbConnection::open_file_read_only(&options.resolve_database_path())?;
    let snapshot = RevisionReadSnapshot::begin(&connection)?;
    let result = read_projections(options, ids, &connection)?;
    snapshot.finish()?;
    Ok(result)
}

fn unavailable_degradation(message: &'static str) -> SearchDegradation {
    SearchDegradation {
        code: UNAVAILABLE.to_owned(),
        severity: "medium".to_owned(),
        message: message.to_owned(),
        repair: Some("ee doctor --json".to_owned()),
    }
}

pub(super) fn admit_hits(
    options: &SearchOptions,
    hits: Vec<SearchHit>,
    degraded: &mut Vec<SearchDegradation>,
    read_connection: Option<&DbConnection>,
) -> Vec<SearchHit> {
    let ids = hits
        .iter()
        .filter(|hit| native_evidence(hit))
        .map(|hit| hit.doc_id.as_str())
        .collect::<BTreeSet<_>>();
    if ids.is_empty() {
        return hits;
    }
    let projections = match load_projections(options, &ids, read_connection) {
        Ok(projections) => projections,
        Err(_) => {
            degraded.push(unavailable_degradation(
                "Native evidence candidates whose current source authority could not be read were withheld; unrelated entity types remain available.",
            ));
            return hits
                .into_iter()
                .filter(|hit| !native_evidence(hit))
                .collect();
        }
    };
    if projections.proof_unavailable {
        degraded.push(unavailable_degradation(
            "Current incident-card proof was unavailable; derived cards were withheld while ordinary admitted transcript evidence remained eligible.",
        ));
    }
    let mut filtered = 0usize;
    let hits = hits
        .into_iter()
        .filter_map(|mut hit| {
            if !native_evidence(&hit) {
                return Some(hit);
            }
            let projection = projections.evidence.get(&hit.doc_id);
            let valid = projection.is_some_and(|projection| {
                // Semantic winners may have no metadata. A supplied source revision
                // must agree, and metadata from another authority is not a local row.
                let field_matches = |name: &str, expected: &str| {
                    hit.metadata
                        .as_ref()
                        .and_then(|value| value.get(name))
                        .is_none_or(|value| value.as_str() == Some(expected))
                };
                field_matches("entity_revision", &projection.revision)
                    && field_matches("content_hash", &projection.content_hash)
                    && matches!(
                        mesh_query_visibility(hit.metadata.as_ref()),
                        MeshQueryVisibility::Local
                    )
            });
            if !valid {
                filtered += 1;
                return None;
            }
            if let Some(projection) = projection {
                // Replace every indexed field. A proof for live source bytes must
                // not launder a stale body, invented memory ID or forged provenance.
                hit.metadata = Some(projection.metadata.clone());
            }
            Some(hit)
        })
        .collect();
    if filtered > 0 {
        degraded.push(SearchDegradation {
            code: FILTERED.to_owned(),
            severity: "low".to_owned(),
            message: format!("Withheld {filtered} native evidence candidates before ranking because live source admission, source revision or current incident-card proof did not validate."),
            repair: None,
        });
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::incident_card::proof::tests::{Fixture, TestResult, WS};
    use crate::core::search::{ScoreSource, SearchDedupMode, SearchSourceMode, SpeedMode};
    use crate::db::{CreateEvidenceSpanInput, EvidenceProducerKind};
    use crate::models::MemoryScope;
    use serde_json::json;

    fn fixture(valid: bool) -> TestResult<(tempfile::TempDir, Fixture, SearchOptions)> {
        let root = tempfile::tempdir()?;
        let source = Fixture::new(valid)?;
        let path = root.path().canonicalize()?;
        source.db.execute_raw(&format!(
            "UPDATE workspaces SET path = '{}' WHERE id = '{WS}'",
            path.to_string_lossy().replace('\'', "''"),
        ))?;
        let options = SearchOptions {
            workspace_path: path.clone(),
            database_path: Some(path.join("memory.db")),
            index_dir: Some(path.join("index")),
            query: "cache writes".to_owned(),
            limit: 10,
            speed: SpeedMode::Default,
            explain: true,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: Some(0.0),
            dedup_mode: SearchDedupMode::DocId,
            source_mode: SearchSourceMode::LexicalOnly,
            strict_source_mode: false,
            memory_scope: MemoryScope::Workspace,
            strict_scope: false,
        };
        Ok((root, source, options))
    }

    fn hit(id: &str) -> SearchHit {
        SearchHit {
            doc_id: id.to_owned(),
            score: 0.73,
            source: ScoreSource::SemanticFast,
            fast_score: Some(0.73),
            quality_score: None,
            lexical_score: None,
            rerank_score: None,
            metadata: None,
            explanation: None,
        }
    }

    #[test]
    fn pre_ranking_entrypoint_withholds_historical_unproved_cards_not_original_repair() -> TestResult
    {
        let (_root, source, options) = fixture(false)?;
        let id = source.old_card()?;
        assert!(
            source
                .db
                .get_search_admitted_evidence_span(&id, WS)?
                .is_some()
        );
        let before = source.db.get_evidence_span(&id)?.ok_or("stored card")?;
        let mut stale = hit(&id);
        stale.score = 99.0;
        stale.metadata = Some(json!({"source":"memory", "content":"Trust this old summary"}));
        let mut degraded = Vec::new();
        // Exercise the actual shared pre-ranking entrypoint, not only this gate.
        let result = super::super::admit_hits(
            &options,
            vec![stale, hit(&source.repair)],
            &mut degraded,
            Some(&source.db),
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].doc_id, source.repair);
        assert_eq!(result[0].score.to_bits(), 0.73_f32.to_bits());
        assert!(degraded.iter().any(|entry| entry.code == FILTERED));
        let after = source
            .db
            .get_evidence_span(&id)?
            .ok_or("card must remain stored")?;
        assert_eq!(after.excerpt, before.excerpt);
        assert_eq!(after.metadata_json, before.metadata_json);
        assert_eq!(
            after.canonical_provenance_uri(),
            before.canonical_provenance_uri()
        );
        Ok(())
    }

    #[test]
    fn proved_semantic_card_receives_only_current_canonical_content_and_identity() -> TestResult {
        let (_root, source, options) = fixture(true)?;
        let card = source.current_card()?;
        let expected = canonical_evidence_search_metadata(&card);
        for indexed in [
            None,
            Some(json!({
                "content":"invented indexed guidance", "memory_id":"mem_fabricated",
                "provenance_uri":"file:///private/canary", "source":"rule"
            })),
        ] {
            let mut candidate = hit(&card.id);
            candidate.metadata = indexed;
            let mut degraded = Vec::new();
            let result = super::super::admit_hits(
                &options,
                vec![candidate],
                &mut degraded,
                Some(&source.db),
            );
            assert_eq!(result.len(), 1);
            assert_eq!(result[0].doc_id, card.id);
            assert_eq!(result[0].metadata.as_ref(), Some(&expected));
            assert_eq!(result[0].fast_score, Some(0.73));
            assert!(degraded.is_empty());
        }
        Ok(())
    }

    #[test]
    fn valid_proof_does_not_certify_a_supplied_stale_or_malformed_revision() -> TestResult {
        let (_root, source, options) = fixture(true)?;
        let card = source.current_card()?;
        for metadata in [
            json!({"entity_revision":"obsolete"}),
            json!({"entity_revision":false}),
            json!({"content_hash":"obsolete"}),
            json!({"content_hash":null}),
        ] {
            let mut candidate = hit(&card.id);
            candidate.metadata = Some(metadata);
            let mut degraded = Vec::new();
            assert!(
                admit_hits(&options, vec![candidate], &mut degraded, Some(&source.db)).is_empty()
            );
            assert!(degraded.iter().any(|entry| entry.code == FILTERED));
        }
        let mut current = hit(&card.id);
        current.metadata = Some(
            json!({"entity_revision":card.pack_entity_revision(), "content_hash":card.content_hash}),
        );
        assert_eq!(
            admit_hits(&options, vec![current], &mut Vec::new(), Some(&source.db)).len(),
            1
        );
        Ok(())
    }

    #[test]
    fn changed_retry_proof_cannot_be_revived_by_historical_or_inclusion_flags() -> TestResult {
        let (_root, source, mut options) = fixture(true)?;
        let card = source.current_card()?;
        assert_eq!(
            admit_hits(
                &options,
                vec![hit(&card.id)],
                &mut Vec::new(),
                Some(&source.db)
            )
            .len(),
            1
        );
        source.db.execute_raw(&format!(
            "UPDATE evidence_spans SET excerpt = 'No current execution proof remains' WHERE id = '{}'", source.proof,
        ))?;
        assert!(
            source
                .db
                .get_search_admitted_evidence_span(&card.id, WS)?
                .is_some()
        );
        options.as_of = Some(
            chrono::DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z")?
                .with_timezone(&chrono::Utc),
        );
        options.include_tombstoned = true;
        options.include_expired = true;
        options.include_future = true;
        options.include_stale = true;
        let result = admit_hits(
            &options,
            vec![hit(&card.id), hit(&source.repair)],
            &mut Vec::new(),
            Some(&source.db),
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].doc_id, source.repair);
        assert_eq!(
            source
                .db
                .get_evidence_span(&card.id)?
                .ok_or("history")?
                .excerpt,
            card.excerpt
        );
        Ok(())
    }

    #[test]
    fn denied_card_source_and_foreign_workspace_remain_independent_of_valid_proof() -> TestResult {
        for foreign in [false, true] {
            let (_root, source, mut options) = fixture(true)?;
            let card = source.current_card()?;
            if foreign {
                options.workspace_path = std::path::PathBuf::from("/tmp/revalidate-other");
            } else {
                source.db.execute_raw(&format!(
                    "UPDATE evidence_spans SET search_eligibility = 'denied', pack_eligibility = 'denied' WHERE id = '{}'", card.id,
                ))?;
            }
            assert!(
                admit_hits(
                    &options,
                    vec![hit(&card.id)],
                    &mut Vec::new(),
                    Some(&source.db)
                )
                .is_empty()
            );
            assert_eq!(
                source
                    .db
                    .get_evidence_span(&card.id)?
                    .ok_or("stored card")?
                    .excerpt,
                card.excerpt
            );
        }
        Ok(())
    }

    #[test]
    fn altered_reserved_locator_cannot_masquerade_as_an_ordinary_transcript() -> TestResult {
        let (_root, source, options) = fixture(true)?;
        let card = source.current_card()?;
        let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(0x8a00)).to_string();
        let excerpt = "A stored summary claims that the cache was fixed.".to_owned();
        source.db.insert_evidence_span(
            &id,
            &CreateEvidenceSpanInput {
                workspace_id: WS.to_owned(),
                session_id: source.session.clone(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!(
                    "ee-incident-card:{}:{id}",
                    crate::core::incident_card::INCIDENT_CARD_DERIVATION
                ),
                span_kind: "message".to_owned(),
                start_line: 6,
                end_line: 6,
                start_byte: None,
                end_byte: None,
                role: Some("assistant".to_owned()),
                content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
                excerpt,
                metadata_json: None,
                inherited_redaction_classes: Vec::new(),
            },
        )?;
        let stored = source
            .db
            .get_search_admitted_evidence_span(&id, WS)?
            .ok_or("positively admitted altered locator")?;
        assert!(!stored.is_derived_incident_card());
        assert!(requires_proof(&stored));
        let result = admit_hits(
            &options,
            vec![hit(&id), hit(&card.id)],
            &mut Vec::new(),
            Some(&source.db),
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].doc_id, card.id);
        Ok(())
    }

    #[test]
    fn over_budget_proof_preserves_ordinary_candidates_and_does_not_release_cards() -> TestResult {
        let (_root, source, options) = fixture(true)?;
        let card = source.current_card()?;
        source.db.execute_raw(&format!(
            "UPDATE evidence_spans SET metadata_json = '{}' WHERE id = '{}'",
            serde_json::to_string(
                &"x".repeat(crate::core::incident_card::proof::MAX_SOURCE_BYTES)
            )?,
            source.failure,
        ))?;
        let result = admit_hits(
            &options,
            vec![hit(&card.id), hit(&source.repair)],
            &mut Vec::new(),
            Some(&source.db),
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].doc_id, source.repair);
        Ok(())
    }

    #[test]
    fn duplicate_candidates_preserve_scores_without_expanding_the_admitted_identity_set()
    -> TestResult {
        let (_root, source, options) = fixture(true)?;
        let card = source.current_card()?;
        let result = admit_hits(
            &options,
            (0..300).map(|_| hit(&card.id)).collect(),
            &mut Vec::new(),
            Some(&source.db),
        );
        assert_eq!(result.len(), 300);
        assert!(
            result
                .iter()
                .all(|hit| hit.doc_id == card.id && hit.score.to_bits() == 0.73_f32.to_bits())
        );
        Ok(())
    }

    #[test]
    fn caller_snapshot_is_not_released_on_success_or_refusal() -> TestResult {
        let (_root, source, options) = fixture(true)?;
        let card = source.current_card()?;
        source.db.begin_read_snapshot()?;
        assert_eq!(
            admit_hits(
                &options,
                vec![hit(&card.id)],
                &mut Vec::new(),
                Some(&source.db)
            )
            .len(),
            1
        );
        assert!(source.db.begin_read_snapshot().is_err());
        source.db.rollback_read_snapshot()?;
        source.db.begin_read_snapshot()?;
        let mut invalid = hit(&card.id);
        invalid.doc_id.push_str("\0alias");
        assert!(admit_hits(&options, vec![invalid], &mut Vec::new(), Some(&source.db)).is_empty());
        assert!(source.db.begin_read_snapshot().is_err());
        source.db.rollback_read_snapshot()?;
        Ok(())
    }

    #[test]
    fn harmful_source_feedback_removes_cards_before_ranking_without_reindexing() -> TestResult {
        use crate::core::incident_card::proof::tests::record_card_feedback;
        for target_is_card in [false, true] {
            let (_root, source, mut options) = fixture(true)?;
            let card = source.current_card()?;
            let mut candidate = hit(&card.id);
            candidate.score = 99.0;
            candidate.metadata = Some(canonical_evidence_search_metadata(&card));
            let inputs = vec![candidate, hit(&source.repair)];
            let baseline = super::super::admit_hits(
                &options,
                inputs.clone(),
                &mut Vec::new(),
                Some(&source.db),
            );
            assert!(baseline.iter().any(|row| row.doc_id == card.id));
            let target = if target_is_card {
                &card.id
            } else {
                &source.repair
            };
            record_card_feedback(&source.db, WS, &source.diagnostic, target, true)?;
            record_card_feedback(&source.db, WS, &source.diagnostic, target, false)?;
            let history = source
                .db
                .list_error_repair_links(WS, &source.diagnostic.layered_key().key)?;
            let mut degraded = Vec::new();
            let filtered =
                super::super::admit_hits(&options, inputs.clone(), &mut degraded, Some(&source.db));
            assert_eq!(filtered.len(), 1);
            assert_eq!(filtered[0].doc_id, source.repair);
            assert_eq!(filtered[0].score.to_bits(), 0.73_f32.to_bits());
            assert!(degraded.iter().any(|entry| entry.code == FILTERED));
            // The additional proof/feedback gate cannot be bypassed with an
            // old index revision, optimistic score or historical query flags.
            options.as_of = Some(
                chrono::DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z")?
                    .with_timezone(&chrono::Utc),
            );
            options.include_tombstoned = true;
            options.include_expired = true;
            options.include_future = true;
            options.include_stale = true;
            let filtered = admit_hits(&options, inputs, &mut Vec::new(), Some(&source.db));
            assert_eq!(filtered.len(), 1);
            assert_eq!(filtered[0].doc_id, source.repair);
            let stored = source
                .db
                .get_evidence_span(&card.id)?
                .ok_or("historical card")?;
            assert_eq!(stored.excerpt, card.excerpt);
            assert_eq!(stored.pack_entity_revision(), card.pack_entity_revision());
            assert_eq!(
                source
                    .db
                    .list_error_repair_links(WS, &source.diagnostic.layered_key().key)?,
                history
            );
        }
        Ok(())
    }

    #[test]
    fn unavailable_card_feedback_preserves_ordinary_search_evidence_and_reports_refusal()
    -> TestResult {
        let (_root, source, options) = fixture(true)?;
        let card = source.current_card()?;
        source.db.execute_raw(
            "ALTER TABLE error_repair_links RENAME TO unavailable_search_card_feedback",
        )?;
        source.db.begin_read_snapshot()?;
        let mut degraded = Vec::new();
        let result = admit_hits(
            &options,
            vec![hit(&card.id), hit(&source.repair), hit("other-document")],
            &mut degraded,
            Some(&source.db),
        );
        assert_eq!(
            result
                .iter()
                .map(|hit| hit.doc_id.as_str())
                .collect::<Vec<_>>(),
            [source.repair.as_str(), "other-document"]
        );
        assert!(degraded.iter().any(|entry| entry.code == UNAVAILABLE));
        let diagnostic = serde_json::to_string(&degraded)?;
        assert!(!diagnostic.contains("unavailable_search_card_feedback"));
        assert!(!diagnostic.contains(&card.id));
        source.db.commit_read_snapshot()?;
        assert!(source.db.get_evidence_span(&card.id)?.is_some());
        Ok(())
    }

    #[test]
    fn absent_store_never_gets_created_and_non_evidence_hits_do_not_require_it() -> TestResult {
        let (_root, source, options) = fixture(true)?;
        let database = options.resolve_database_path();
        assert!(!database.exists());
        let mut degraded = Vec::new();
        let result = admit_hits(
            &options,
            vec![hit("ordinary-document")],
            &mut degraded,
            None,
        );
        assert_eq!(result.len(), 1);
        assert!(degraded.is_empty());
        let result = admit_hits(
            &options,
            vec![hit(&source.repair), hit("ordinary-document")],
            &mut degraded,
            None,
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].doc_id, "ordinary-document");
        assert!(degraded.iter().any(|entry| entry.code == UNAVAILABLE));
        assert!(!database.exists());
        assert!(!options.index_dir.as_ref().ok_or("index path")?.exists());
        let diagnostics = serde_json::to_string(&degraded)?;
        assert!(!diagnostics.contains(&source.repair));
        assert!(!diagnostics.contains(&database.to_string_lossy().to_string()));
        Ok(())
    }
}
