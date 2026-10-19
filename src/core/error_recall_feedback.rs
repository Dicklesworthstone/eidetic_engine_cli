//! Source-backed negative feedback admission for positive repair recall.
//!
//! Helpful and harmful observations are independent historical rows. A later
//! helpful upsert cannot erase contrary evidence, and a cached report cannot
//! authorize a repair that has since received a harmful observation. This is
//! a read-time hold on unqualified positive guidance, not rule deprecation or
//! a mutation of feedback history. Scope is the exact workspace/error class.

use std::collections::BTreeSet;

use sqlmodel_core::Value;

use crate::db::{DbConnection, DbError, DbOperation, Result};
use crate::models::EvidenceId;

#[path = "error_recall_revalidate.rs"]
mod revalidate;

pub(super) use revalidate::Gate;

const FEEDBACK_BATCH: usize = 128;

fn unavailable() -> DbError {
    DbError::MalformedRow {
        operation: DbOperation::Query,
        message: "Could not verify repair feedback or source proof; positive guidance withheld"
            .to_owned(),
    }
}

/// Inspect only one already selected batch, inside the caller's source
/// snapshot. Target identity is exact: never trim it or truncate an alias.
/// Return identifiers only; feedback reasons and source text stay private.
pub(super) fn harmful_targets(
    connection: &DbConnection,
    workspace_id: &str,
    fingerprint_key: &str,
    targets: &[&str],
) -> Result<BTreeSet<String>> {
    if targets.len() > FEEDBACK_BATCH || targets.iter().any(|id| id.len() > 128) {
        return Err(unavailable());
    }
    if targets.is_empty() {
        return Ok(BTreeSet::new());
    }
    let selected: BTreeSet<_> = targets.iter().copied().collect();
    let placeholders = (3..3 + selected.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT DISTINCT target_id FROM error_repair_links WHERE workspace_id = ?1 AND fingerprint_key = ?2 AND link_kind = 'repair' AND outcome = 'harmful' AND target_id IN ({placeholders}) ORDER BY target_id ASC"
    );
    let mut parameters = vec![
        Value::Text(workspace_id.to_owned()),
        Value::Text(fingerprint_key.to_owned()),
    ];
    parameters.extend(selected.iter().map(|id| Value::Text((*id).to_owned())));
    let rows = connection
        .query(&sql, &parameters)
        .map_err(|_| unavailable())?;
    let mut harmful = BTreeSet::new();
    for row in rows {
        let Some(id) = row.get(0).and_then(Value::as_str) else {
            return Err(unavailable());
        };
        if !selected.contains(id) || !harmful.insert(id.to_owned()) {
            return Err(unavailable());
        }
    }
    Ok(harmful)
}

/// Select up to the existing target budget after feedback, live proof, and
/// direct-pack evidence admission. Missing or no-longer-admitted evidence
/// cannot consume the window before final hydration and hide later repairs.
/// Page metadata in canonical order, refilling slots refused by either gate.
/// The server retains the raw target ordering key; only a bounded link ID crosses
/// the cursor boundary, so an oversized target never has to be hydrated to page.
/// One gate reuses bounded per-session verdicts throughout this source snapshot.
///
/// Diagnostic history and the legacy 128-byte target projection stay unchanged.
/// Empty sentinels and exact aliases retain their old sampling semantics. Total
/// index scans and source interpretation are not constant work or a latency SLO.
pub(super) fn pack_targets(
    connection: &DbConnection,
    workspace_id: &str,
    fingerprint_key: &str,
    limit: u32,
) -> Result<Vec<String>> {
    let mut gate = Gate::new(connection, workspace_id, fingerprint_key);
    pack_targets_with(connection, workspace_id, fingerprint_key, limit, |ids| {
        let mut withheld = gate.withheld(ids)?;
        withhold_unavailable_pack_evidence(connection, workspace_id, ids, &mut withheld)?;
        Ok(withheld)
    })
}

/// Check the same live admission used by the final error-guided pack reader,
/// inside its existing source snapshot. This does not grant proof authority:
/// the feedback/card gate must run first. Ordinary evidence can become missing,
/// foreign, stale or pack-denied without an explicit harmful-feedback record.
/// Non-evidence targets retain their native reader and locator-only contracts.
fn withhold_unavailable_pack_evidence(
    connection: &DbConnection,
    workspace_id: &str,
    targets: &[&str],
    withheld: &mut BTreeSet<String>,
) -> Result<()> {
    let selected: BTreeSet<_> = targets
        .iter()
        .copied()
        .filter(|target| {
            !withheld.contains(*target)
                && target
                    .parse::<EvidenceId>()
                    .is_ok_and(|id| id.to_string() == *target)
        })
        .collect();
    if selected.is_empty() {
        return Ok(());
    }
    let ids: Vec<_> = selected.iter().copied().collect();
    let rows = connection
        .get_evidence_spans_with_sessions(&ids)
        .map_err(|_| unavailable())?;
    if rows.len() > selected.len() {
        return Err(unavailable());
    }
    let mut seen = BTreeSet::new();
    let mut admitted = BTreeSet::new();
    for row in rows {
        if !selected.contains(row.span.id.as_str()) || !seen.insert(row.span.id.clone()) {
            return Err(unavailable());
        }
        if row.is_direct_pack_admitted(workspace_id) && !row.span.reader_text().trim().is_empty() {
            admitted.insert(row.span.id);
        }
    }
    withheld.extend(
        selected
            .into_iter()
            .filter(|id| !admitted.contains(*id))
            .map(str::to_owned),
    );
    Ok(())
}

fn pack_targets_with(
    connection: &DbConnection,
    workspace_id: &str,
    fingerprint_key: &str,
    limit: u32,
    mut withheld: impl FnMut(&[&str]) -> Result<BTreeSet<String>>,
) -> Result<Vec<String>> {
    if limit > super::PACK_RECALL_TARGET_LIMIT {
        return Err(unavailable());
    }
    let mut output = Vec::new();
    let mut after = String::new();
    while output.len() < limit as usize {
        let remaining = limit as usize - output.len();
        let rows = connection.query(
            "SELECT CASE WHEN length(CAST(h.target_id AS BLOB)) <= 128 THEN h.target_id ELSE '' END, CASE WHEN length(CAST(h.link_id AS BLOB)) <= 128 THEN h.link_id ELSE '' END FROM error_repair_links AS h WHERE h.workspace_id = ?1 AND h.fingerprint_key = ?2 AND h.link_kind = 'repair' AND h.outcome = 'helpful' AND (?4 = '' OR h.target_id > (SELECT c.target_id FROM error_repair_links AS c WHERE c.workspace_id = ?1 AND c.fingerprint_key = ?2 AND c.link_id = ?4) OR (h.target_id = (SELECT c.target_id FROM error_repair_links AS c WHERE c.workspace_id = ?1 AND c.fingerprint_key = ?2 AND c.link_id = ?4) AND h.link_id > ?4)) AND NOT EXISTS (SELECT 1 FROM error_repair_links AS n WHERE n.workspace_id = ?1 AND n.fingerprint_key = ?2 AND n.link_kind = 'repair' AND n.outcome = 'harmful' AND n.target_id = h.target_id) ORDER BY h.target_id ASC, h.link_id ASC LIMIT ?3",
            &[
                Value::Text(workspace_id.to_owned()),
                Value::Text(fingerprint_key.to_owned()),
                Value::BigInt(i64::try_from(remaining).map_err(|_| unavailable())?),
                Value::Text(after.clone()),
            ],
        ).map_err(|_| unavailable())?;
        if rows.is_empty() {
            break;
        }
        if rows.len() > remaining {
            return Err(unavailable());
        }
        let page_complete = rows.len() == remaining;
        let mut targets = Vec::with_capacity(rows.len());
        for row in &rows {
            let target = row
                .get(0)
                .and_then(Value::as_str)
                .filter(|id| id.len() <= 128)
                .ok_or_else(unavailable)?;
            let cursor = row
                .get(1)
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty() && id.len() <= 128)
                .ok_or_else(unavailable)?;
            if cursor == after {
                return Err(unavailable());
            }
            targets.push(target.to_owned());
            after = cursor.to_owned();
        }
        let refs: Vec<_> = targets.iter().map(String::as_str).collect();
        let rejected = withheld(&refs)?;
        if rejected.iter().any(|id| !refs.contains(&id.as_str())) {
            return Err(unavailable());
        }
        output.extend(targets.into_iter().filter(|id| !rejected.contains(id)));
        if !page_complete {
            break;
        }
    }
    Ok(output)
}

#[cfg(test)]
mod live_evidence_tests {
    use super::*;
    use crate::core::error_diagnosis::{
        ErrorRepairLinkRecording, error_recall_report, pack_error_recall_query_seed,
        record_error_repair_links,
    };
    use crate::core::error_recall::from_rustc;
    use crate::db::{
        CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, EvidenceProducerKind,
    };

    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
    const WS: &str = "wsp_01234567890123456789012345";
    const OTHER: &str = "wsp_01234567890123456789012346";
    const KEY: &str = "rustc:E0277";

    struct Fixture {
        db: DbConnection,
        session: String,
        foreign_session: String,
    }

    fn id(number: u32) -> String {
        EvidenceId::from_uuid(uuid::Uuid::from_u128(u128::from(number))).to_string()
    }

    impl Fixture {
        fn new() -> TestResult<Self> {
            let db = DbConnection::open_memory()?;
            db.migrate()?;
            let mut sessions = Vec::new();
            for (number, workspace) in [(800_001, WS), (800_002, OTHER)] {
                db.insert_workspace(
                    workspace,
                    &CreateWorkspaceInput {
                        path: format!("/tmp/live-recall-{number}"),
                        name: None,
                    },
                )?;
                let session =
                    crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
                db.insert_session(
                    &session,
                    &CreateSessionInput {
                        workspace_id: workspace.to_owned(),
                        cass_session_id: format!("/sessions/live-recall-{number}.jsonl"),
                        source_path: None,
                        agent_name: Some("claude_code".to_owned()),
                        model: None,
                        started_at: None,
                        ended_at: None,
                        message_count: 0,
                        token_count: None,
                        content_hash: format!("blake3:{}", blake3::hash(&number.to_le_bytes())),
                        metadata_json: None,
                    },
                )?;
                sessions.push(session);
            }
            Ok(Self {
                db,
                session: sessions[0].clone(),
                foreign_session: sessions[1].clone(),
            })
        }

        fn source(&self, number: u32, foreign: bool) -> TestResult<String> {
            let target = id(number);
            let text =
                format!("Repair {number} uses the canonical cache key for reads and writes.");
            let excerpt = serde_json::json!({"type":"assistant","message":{
                "role":"assistant","content":[{"type":"text","text":text}]
            }})
            .to_string();
            let (workspace, session) = if foreign {
                (OTHER, &self.foreign_session)
            } else {
                (WS, &self.session)
            };
            self.db.insert_evidence_span(
                &target,
                &CreateEvidenceSpanInput {
                    workspace_id: workspace.to_owned(),
                    session_id: session.clone(),
                    memory_id: None,
                    producer_kind: EvidenceProducerKind::CassImport,
                    cass_span_id: format!("{session}:{number}"),
                    span_kind: "message".to_owned(),
                    start_line: number,
                    end_line: number,
                    start_byte: None,
                    end_byte: None,
                    role: Some("assistant".to_owned()),
                    content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes())),
                    excerpt,
                    metadata_json: None,
                    inherited_redaction_classes: Vec::new(),
                },
            )?;
            Ok(target)
        }

        fn record(&self, targets: Vec<String>) -> TestResult {
            record_error_repair_links(
                &self.db,
                WS,
                &from_rustc(Some("E0277"), "trait bound failed"),
                &ErrorRepairLinkRecording {
                    helpful_repairs: targets,
                    ..Default::default()
                },
            )?;
            Ok(())
        }

        fn query(&self) -> crate::db::Result<String> {
            pack_error_recall_query_seed(
                &self.db,
                WS,
                &from_rustc(Some("E0277"), "trait bound failed"),
            )
        }
    }

    #[test]
    fn unavailable_history_prefix_does_not_hide_later_live_repairs() -> TestResult {
        let fixture = Fixture::new()?;
        // Missing, pack-denied, stale-sealed and foreign evidence each occupy
        // a whole former target window; none has harmful feedback.
        for number in 33..=164 {
            let target = fixture.source(number, (97..=128).contains(&number))?;
            if (33..=64).contains(&number) {
                fixture.db.execute_raw(&format!(
                    "UPDATE evidence_spans SET pack_eligibility = 'denied' WHERE id = '{target}'"
                ))?;
            } else if (65..=96).contains(&number) {
                fixture.db.execute_raw(&format!(
                    "UPDATE evidence_spans SET excerpt = 'Changed source without its seal' WHERE id = '{target}'"
                ))?;
            }
        }
        fixture.record((1..=164).map(id).collect())?;
        let history = fixture.db.list_error_repair_links(WS, KEY)?;
        let sources = fixture
            .db
            .list_evidence_spans_for_session(&fixture.session)?;
        assert_eq!(
            fixture
                .db
                .list_helpful_error_repair_target_ids(WS, KEY, 32)?,
            (1..=32).map(id).collect::<Vec<_>>(),
        );
        let expected = (129..=160).map(id).collect::<Vec<_>>();
        assert_eq!(pack_targets(&fixture.db, WS, KEY, 32)?, expected);
        let query = fixture.query()?;
        for number in 129..=132 {
            assert!(query.contains(&format!("Repair {number} uses")));
        }
        assert_eq!(query.matches("\nprior fix: ").count(), 4);
        assert!(query.len() <= super::super::PACK_RECALL_QUERY_BYTES);
        for number in 1..=128 {
            assert!(!query.contains(&id(number)));
        }
        assert_eq!(fixture.query()?, query);
        assert_eq!(fixture.db.list_error_repair_links(WS, KEY)?, history);
        assert_eq!(
            fixture
                .db
                .list_evidence_spans_for_session(&fixture.session)?,
            sources
        );
        let report = error_recall_report(
            &fixture.db,
            WS,
            &from_rustc(Some("E0277"), "trait bound failed"),
        )?;
        assert_eq!(report.helpful_repairs.len(), 164);
        assert!(report.harmful_repairs.is_empty());
        Ok(())
    }

    #[test]
    fn all_target_limits_refill_after_missing_evidence() -> TestResult {
        let fixture = Fixture::new()?;
        for number in 42..=76 {
            fixture.source(number, false)?;
        }
        fixture.record((1..=76).map(id).collect())?;
        let good = (42..=76).map(id).collect::<Vec<_>>();
        for limit in 0..=32 {
            assert_eq!(
                pack_targets(&fixture.db, WS, KEY, limit)?,
                good[..limit as usize]
            );
        }
        assert!(pack_targets(&fixture.db, WS, KEY, 33).is_err());
        Ok(())
    }

    #[test]
    fn exhaustion_returns_metadata_without_inventing_an_admitted_repair() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.record((1..=97).map(id).collect())?;
        assert!(pack_targets(&fixture.db, WS, KEY, 32)?.is_empty());
        let query = fixture.query()?;
        assert!(query.starts_with("error recall known fingerprint:rustc:E0277"));
        assert!(!query.contains("prior fix:"));
        assert!(!query.contains("helpful_repair:"));
        assert_eq!(fixture.db.list_error_repair_links(WS, KEY)?.len(), 97);
        Ok(())
    }

    #[test]
    fn changed_pack_admission_is_observed_without_rewriting_feedback() -> TestResult {
        let fixture = Fixture::new()?;
        let first = fixture.source(1, false)?;
        let next = fixture.source(2, false)?;
        fixture.record(vec![first.clone(), next.clone()])?;
        let history = fixture.db.list_error_repair_links(WS, KEY)?;
        assert_eq!(pack_targets(&fixture.db, WS, KEY, 1)?, vec![first.clone()]);
        fixture.db.execute_raw(&format!(
            "UPDATE evidence_spans SET pack_eligibility = 'denied' WHERE id = '{first}'"
        ))?;
        assert_eq!(pack_targets(&fixture.db, WS, KEY, 1)?, vec![next]);
        assert_eq!(fixture.db.list_error_repair_links(WS, KEY)?, history);
        Ok(())
    }

    #[test]
    fn unavailable_source_authority_fails_without_partial_query_or_leaked_snapshot() -> TestResult {
        let fixture = Fixture::new()?;
        let target = fixture.source(1, false)?;
        fixture.record(vec![target])?;
        assert!(fixture.query()?.contains("prior fix:"));
        fixture
            .db
            .execute_raw("ALTER TABLE evidence_spans RENAME TO unavailable_recall_sources")?;
        let error = fixture
            .query()
            .expect_err("missing authority must not become an empty page");
        assert!(!error.to_string().contains("unavailable_recall_sources"));
        fixture.db.begin_read_snapshot()?;
        fixture.db.rollback_read_snapshot()?;
        Ok(())
    }

    #[test]
    fn uncommitted_admission_changes_remain_owned_by_the_caller() -> TestResult {
        let fixture = Fixture::new()?;
        let first = fixture.source(1, false)?;
        let next = fixture.source(2, false)?;
        fixture.record(vec![first.clone(), next.clone()])?;
        let original = fixture.query()?;
        let result: crate::db::Result<()> = fixture.db.with_transaction(|| {
            fixture.db.execute_raw(&format!(
                "UPDATE evidence_spans SET pack_eligibility = 'denied' WHERE id = '{first}'"
            ))?;
            assert_eq!(pack_targets(&fixture.db, WS, KEY, 1)?, vec![next.clone()]);
            assert!(!fixture.query()?.contains(&first));
            fixture
                .db
                .execute_raw("INSERT INTO absent_admission_rollback_test VALUES (1)")?;
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(pack_targets(&fixture.db, WS, KEY, 1)?, vec![first]);
        assert_eq!(fixture.query()?, original);
        Ok(())
    }
}
