//! Source-backed negative feedback admission for positive repair recall.
//!
//! Helpful and harmful observations are independent historical rows. A later
//! helpful upsert cannot erase contrary evidence, and a cached report cannot
//! authorize a repair that has since received a harmful observation. This is
//! a read-time hold on unqualified positive guidance, not rule deprecation or
//! a mutation of feedback history. Scope is the exact workspace/error class.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
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
/// direct-pack evidence admission. Missing, no-longer-admitted, or repeated
/// evidence excerpts cannot consume the window and hide later distinct repairs.
/// Page metadata in canonical order, refilling slots refused by either gate or
/// already represented by the same bounded reader excerpt in this request.
/// The server retains the raw target ordering key; only a bounded link ID crosses
/// the cursor boundary, so an oversized target never has to be hydrated to page.
/// One gate reuses bounded per-session verdicts throughout this source snapshot.
///
/// Diagnostic history and the legacy 128-byte target projection stay unchanged.
/// Only canonical identities supported by a live pack reader earn a slot;
/// opaque metadata and projected empty sentinels remain history, not advice. Total
/// index scans and source interpretation are not constant work or a latency SLO.
#[cfg(test)]
pub(super) fn pack_targets(
    connection: &DbConnection,
    workspace_id: &str,
    fingerprint_key: &str,
    limit: u32,
) -> Result<Vec<String>> {
    pack_selection(connection, workspace_id, fingerprint_key, limit, Utc::now())
        .map(|selection| selection.targets)
}

/// A request-local selection, not another source cache. Keep only the native
/// excerpts that earned a target slot. Reusing them avoids a second hydration
/// and screening pass after selection, within the same caller-owned snapshot.
#[derive(Default)]
pub(super) struct PackSelection {
    pub(super) targets: Vec<String>,
    pub(super) memories: Vec<(String, String)>,
    pub(super) rules: Vec<(String, String)>,
    pub(super) memory_hints: Vec<String>,
}

pub(super) fn pack_selection(
    connection: &DbConnection,
    workspace_id: &str,
    fingerprint_key: &str,
    limit: u32,
    at: DateTime<Utc>,
) -> Result<PackSelection> {
    let mut gate = Gate::new(connection, workspace_id, fingerprint_key);
    let mut diversity = PackEvidenceDiversity::default();
    let mut selection = PackSelection::default();
    let targets = pack_targets_with(connection, workspace_id, fingerprint_key, limit, |ids| {
        // The cursor advances over every raw row, but only canonical typed
        // identities can reach source readers or occupy the usable window.
        // Never trim an alias or let a projected oversized target's empty
        // sentinel consume a slot. The complete report still retains it.
        let typed: Vec<_> = ids
            .iter()
            .copied()
            .filter(|id| canonical_pack_target(id))
            .collect();
        let mut withheld: BTreeSet<_> = ids
            .iter()
            .copied()
            .filter(|id| !canonical_pack_target(id))
            .map(str::to_owned)
            .collect();
        if typed.is_empty() {
            return Ok(withheld);
        }
        withheld.extend(gate.withheld(&typed)?);
        withhold_unusable_pack_evidence(
            connection,
            workspace_id,
            &typed,
            &mut withheld,
            &mut diversity,
        )?;
        selection.admit_native(connection, workspace_id, &typed, &mut withheld, at)?;
        Ok(withheld)
    })?;
    selection.targets = targets;
    Ok(selection)
}

/// Parsing alone may accept another spelling. Only the reader's exact
/// round-tripped identity is useful here; metadata is never an implicit ID.
fn canonical_pack_target(target: &str) -> bool {
    target
        .parse::<EvidenceId>()
        .is_ok_and(|id| id.to_string() == target)
        || target
            .parse::<crate::models::MemoryId>()
            .is_ok_and(|id| id.to_string() == target)
        || target
            .parse::<crate::models::RuleId>()
            .is_ok_and(|id| id.to_string() == target)
}

impl PackSelection {
    fn admit_native(
        &mut self,
        connection: &DbConnection,
        workspace: &str,
        targets: &[&str],
        withheld: &mut BTreeSet<String>,
        at: DateTime<Utc>,
    ) -> Result<()> {
        let mut memories = Vec::new();
        let mut rules = Vec::new();
        for &id in targets {
            // Feedback and current incident-source proof always run first.
            if withheld.contains(id) {
                continue;
            }
            if id
                .parse::<crate::models::MemoryId>()
                .is_ok_and(|parsed| parsed.to_string() == id)
            {
                memories.push(id);
                // Preserve the existing locator-only contract without letting
                // withheld bodies consume the 32 usable-target slots. Hints
                // are independently capped and never grant body authority.
                if self.memory_hints.len() < super::PACK_RECALL_EXCERPT_LIMIT
                    && !self.memory_hints.iter().any(|hint| hint == id)
                {
                    self.memory_hints.push(id.to_owned());
                }
            } else if id
                .parse::<crate::models::RuleId>()
                .is_ok_and(|parsed| parsed.to_string() == id)
            {
                rules.push(id);
            }
        }
        let rows = super::memory::read(connection, workspace, &memories, at)?;
        retain_distinct_native(&memories, rows, &mut self.memories, withheld)?;
        let rows = super::rules::read(connection, workspace, &rules, at)?;
        retain_distinct_native(&rules, rows, &mut self.rules, withheld)
    }
}

/// Equality is the exact final UTF-8 excerpt, not semantic equivalence or a
/// claim about source identities. Keep kinds separate so final evidence,
/// memory and rule precedence remains unchanged. All earlier admission and
/// complete-body screening must succeed before an excerpt reserves a slot.
fn retain_distinct_native(
    selected: &[&str],
    rows: Vec<(String, String)>,
    retained: &mut Vec<(String, String)>,
    withheld: &mut BTreeSet<String>,
) -> Result<()> {
    let mut seen = BTreeSet::new();
    let mut admitted = BTreeSet::new();
    for (id, text) in rows {
        if !selected.contains(&id.as_str())
            || !seen.insert(id.clone())
            || text.is_empty()
            || text.len() > super::PACK_RECALL_EXCERPT_BYTES
        {
            return Err(unavailable());
        }
        if retained.iter().any(|(_, prior)| prior == &text) {
            continue;
        }
        if retained.len() == super::PACK_RECALL_TARGET_LIMIT as usize {
            return Err(unavailable());
        }
        admitted.insert(id.clone());
        retained.push((id, text));
    }
    withheld.extend(
        selected
            .iter()
            .filter(|id| !admitted.contains(**id))
            .map(|id| (*id).to_owned()),
    );
    Ok(())
}

/// Selection-only diversity, not an equivalence claim about source episodes.
/// The final pack reader already emits each bounded excerpt only once. Apply
/// that same equality before the target limit so repeated imports cannot fill
/// all 32 slots with text that contributes only one of the four excerpts.
/// Keep cards separate from ordinary turns to preserve final card preference.
/// At most 32 admitted keys of at most 768 UTF-8 bytes are retained. No full
/// transcript body, cross-request cache, or durable feedback state is kept.
#[derive(Default)]
struct PackEvidenceDiversity {
    excerpts: BTreeSet<(bool, String)>,
}

impl PackEvidenceDiversity {
    fn admit(&mut self, is_card: bool, reader_text: &str) -> bool {
        let excerpt =
            super::bounded_pack_recall_text(reader_text.trim(), super::PACK_RECALL_EXCERPT_BYTES);
        !excerpt.is_empty() && self.excerpts.insert((is_card, excerpt))
    }
}

/// Check the same live admission used by the final error-guided pack reader,
/// inside its existing source snapshot. This does not grant proof authority:
/// the feedback/card gate must run first. Ordinary evidence can become missing,
/// foreign, stale or pack-denied without an explicit harmful-feedback record.
/// Only an admitted row can reserve an excerpt: an unusable earlier copy must
/// never crowd out a later live source containing the same useful explanation.
/// Native memory/rule admission runs next, before targets consume the limit.
fn withhold_unusable_pack_evidence(
    connection: &DbConnection,
    workspace_id: &str,
    targets: &[&str],
    withheld: &mut BTreeSet<String>,
    diversity: &mut PackEvidenceDiversity,
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
    let mut rows = connection
        .get_evidence_spans_with_sessions(&ids)
        .map_err(|_| unavailable())?;
    if rows.len() > selected.len() {
        return Err(unavailable());
    }
    let mut seen = BTreeSet::new();
    for row in &rows {
        if !selected.contains(row.span.id.as_str()) || !seen.insert(row.span.id.clone()) {
            return Err(unavailable());
        }
    }
    // Hydration order is not a sampling policy. Preserve the SQL cursor's
    // canonical target order, including ties that cross page boundaries.
    rows.sort_by(|left, right| left.span.id.cmp(&right.span.id));
    let mut admitted = BTreeSet::new();
    for row in rows {
        if row.is_direct_pack_admitted(workspace_id)
            && diversity.admit(row.span.is_derived_incident_card(), &row.span.reader_text())
        {
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

/// Keep the continuation lower bound outside an OR with the first-page case.
/// Otherwise an index can restart at the beginning of the fingerprint on every
/// refill, making a long rejected prefix quadratic. The server resolves the
/// bounded link cursor to its original target, so oversized targets and NUL
/// aliases keep their exact ordering without crossing the hydration boundary.
/// Retain the link tie-break even though the current schema forbids duplicate
/// helpful targets. The lower bound is inclusive; the final predicate is not.
fn pack_target_page_sql(continuation: bool) -> String {
    let cursor = if continuation {
        " AND h.target_id >= (SELECT c.target_id FROM error_repair_links AS c WHERE c.workspace_id = ?1 AND c.fingerprint_key = ?2 AND c.link_id = ?4) AND (h.target_id > (SELECT c.target_id FROM error_repair_links AS c WHERE c.workspace_id = ?1 AND c.fingerprint_key = ?2 AND c.link_id = ?4) OR h.link_id > ?4)"
    } else {
        ""
    };
    format!(
        "SELECT CASE WHEN length(CAST(h.target_id AS BLOB)) <= 128 THEN h.target_id ELSE '' END, CASE WHEN length(CAST(h.link_id AS BLOB)) <= 128 THEN h.link_id ELSE '' END FROM error_repair_links AS h WHERE h.workspace_id = ?1 AND h.fingerprint_key = ?2 AND h.link_kind = 'repair' AND h.outcome = 'helpful'{cursor} AND NOT EXISTS (SELECT 1 FROM error_repair_links AS n WHERE n.workspace_id = ?1 AND n.fingerprint_key = ?2 AND n.link_kind = 'repair' AND n.outcome = 'harmful' AND n.target_id = h.target_id) ORDER BY h.target_id ASC, h.link_id ASC LIMIT ?3"
    )
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
    // A long rejected history must remain interruptible. Observe the caller's
    // cancellation only; do not mint another request or extend its deadline.
    // The outer pack reader owns (or borrows) the source snapshot and unwinds
    // it on Err. A successful prefix is not an authorized partial answer.
    let request = asupersync::Cx::current();
    let checkpoint = || -> Result<()> {
        if let Some(cx) = &request {
            cx.checkpoint().map_err(|_| unavailable())?;
        }
        Ok(())
    };
    let mut output = Vec::new();
    let mut after = String::new();
    while output.len() < limit as usize {
        checkpoint()?;
        let remaining = limit as usize - output.len();
        let mut parameters = vec![
            Value::Text(workspace_id.to_owned()),
            Value::Text(fingerprint_key.to_owned()),
            Value::BigInt(i64::try_from(remaining).map_err(|_| unavailable())?),
        ];
        if !after.is_empty() {
            parameters.push(Value::Text(after.clone()));
        }
        let rows = connection
            .query(&pack_target_page_sql(!after.is_empty()), &parameters)
            .map_err(|_| unavailable())?;
        checkpoint()?;
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
        drop(rows);
        checkpoint()?;
        let refs: Vec<_> = targets.iter().map(String::as_str).collect();
        let rejected = withheld(&refs)?;
        checkpoint()?;
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
            let text =
                format!("Repair {number} uses the canonical cache key for reads and writes.");
            self.source_text(number, foreign, &text)
        }

        fn source_text(&self, number: u32, foreign: bool, text: &str) -> TestResult<String> {
            let target = id(number);
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

    #[test]
    fn canonical_targets_require_the_exact_reader_identity() {
        let targets = [
            id(1),
            crate::models::MemoryId::from_uuid(uuid::Uuid::from_u128(2)).to_string(),
            crate::models::RuleId::from_uuid(uuid::Uuid::from_u128(3)).to_string(),
        ];
        for target in targets {
            assert!(canonical_pack_target(&target));
            for alias in [
                format!(" {target}"),
                format!("{target} "),
                format!("{target}\0suffix"),
                format!("{target}/child"),
                target.to_ascii_uppercase(),
            ] {
                assert!(!canonical_pack_target(&alias), "{alias:?}");
            }
        }
        for metadata in [
            "",
            "mem_bad",
            "rule_bad",
            "ev_bad",
            "proof_123",
            "artifact_123",
            "0",
        ] {
            assert!(!canonical_pack_target(metadata));
        }
    }

    #[test]
    fn unsupported_history_prefixes_cannot_hide_later_typed_repairs() -> TestResult {
        let fixture = Fixture::new()?;
        let mut targets = Vec::new();
        for number in 0..160 {
            targets.push(if number % 2 == 0 {
                format!("!opaque-{number:04}")
            } else {
                format!("!oversized-{number:04}-{}", "界".repeat(100))
            });
        }
        let mut good = Vec::new();
        for number in 1000..1036 {
            let target = fixture.source(number, false)?;
            targets.push(format!("{target}\0alias"));
            targets.push(target.clone());
            good.push(target);
        }
        fixture.record(targets)?;
        let history = fixture.db.list_error_repair_links(WS, KEY)?;
        let legacy = fixture
            .db
            .list_helpful_error_repair_target_ids(WS, KEY, 32)?;
        assert!(legacy.iter().all(|target| !canonical_pack_target(target)));
        for limit in 0..=32 {
            assert_eq!(
                pack_targets(&fixture.db, WS, KEY, limit)?,
                good[..limit as usize]
            );
        }
        let query = fixture.query()?;
        for number in 1000..1004 {
            assert!(query.contains(&format!("Repair {number} uses")));
        }
        assert_eq!(query.matches("\nprior fix: ").count(), 4);
        assert!(
            !query.contains("opaque-") && !query.contains("oversized-") && !query.contains("alias")
        );
        assert_eq!(fixture.db.list_error_repair_links(WS, KEY)?, history);
        assert_eq!(
            error_recall_report(
                &fixture.db,
                WS,
                &from_rustc(Some("E0277"), "trait bound failed")
            )?
            .helpful_repairs
            .len(),
            history.len()
        );
        Ok(())
    }

    #[test]
    fn unresolvable_metadata_does_not_require_unrelated_source_authority() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.record(
            (0..97)
                .map(|number| format!("!opaque-{number:03}"))
                .collect(),
        )?;
        fixture
            .db
            .execute_raw("ALTER TABLE evidence_spans RENAME TO unavailable_unlinked_evidence")?;
        let selected = pack_selection(&fixture.db, WS, KEY, 32, Utc::now())?;
        assert!(
            selected.targets.is_empty()
                && selected.memories.is_empty()
                && selected.rules.is_empty()
        );
        assert!(selected.memory_hints.is_empty());
        assert_eq!(fixture.db.list_error_repair_links(WS, KEY)?.len(), 97);
        Ok(())
    }

    #[test]
    fn cancelled_refill_discards_provisional_targets_without_releasing_a_borrowed_snapshot()
    -> TestResult {
        let fixture = Fixture::new()?;
        for number in 1..=8 {
            fixture.source(number, false)?;
        }
        fixture.record((1..=8).map(id).collect())?;
        fixture.db.begin_read_snapshot()?;
        let source = &fixture.db;
        let (result, calls) =
            crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| async move {
                let mut calls = 0;
                let result = pack_targets_with(source, WS, KEY, 3, |ids| {
                    calls += 1;
                    cx.cancel_with(asupersync::CancelKind::User, Some("stop repair refill"));
                    Ok(ids.iter().skip(1).map(|id| (*id).to_owned()).collect())
                });
                (result, calls)
            })?;
        assert!(result.is_err());
        assert_eq!(calls, 1);
        assert!(fixture.db.begin_read_snapshot().is_err());
        fixture.db.commit_read_snapshot()?;
        assert_eq!(
            pack_targets(&fixture.db, WS, KEY, 3)?,
            [id(1), id(2), id(3)]
        );
        fixture.db.begin_read_snapshot()?;
        fixture.db.rollback_read_snapshot()?;
        Ok(())
    }

    const REPEATED_REPAIR: &str =
        "The verified repair flushes pending cache writes before reading the canonical key.";

    #[test]
    fn duplicate_evidence_windows_refill_with_distinct_repairs_without_changing_history()
    -> TestResult {
        let fixture = Fixture::new()?;
        for number in 1..=96 {
            fixture.source_text(number, false, REPEATED_REPAIR)?;
        }
        for number in 97..=132 {
            fixture.source(number, false)?;
        }
        fixture.record((1..=132).rev().map(id).collect())?;
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
        let expected = std::iter::once(id(1))
            .chain((97..=127).map(id))
            .collect::<Vec<_>>();
        assert_eq!(pack_targets(&fixture.db, WS, KEY, 32)?, expected);
        let query = fixture.query()?;
        assert_eq!(query.matches(REPEATED_REPAIR).count(), 1);
        assert_eq!(query.matches("\nprior fix: ").count(), 4);
        assert!(query.contains(&format!("helpful_repair:{}", id(1))));
        for number in 97..=99 {
            assert!(query.contains(&format!("Repair {number} uses")));
            assert!(query.contains(&format!("helpful_repair:{}", id(number))));
        }
        for number in 2..=96 {
            assert!(!query.contains(&id(number)));
        }
        assert!(query.len() <= super::super::PACK_RECALL_QUERY_BYTES);
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
        assert_eq!(report.helpful_repairs.len(), 132);
        assert!(report.harmful_repairs.is_empty());
        Ok(())
    }

    #[test]
    fn every_limit_and_duplicate_only_exhaustion_keep_one_canonical_representative() -> TestResult {
        let fixture = Fixture::new()?;
        for number in 1..=41 {
            fixture.source_text(number, false, REPEATED_REPAIR)?;
        }
        fixture.record((1..=41).map(id).collect())?;
        assert_eq!(pack_targets(&fixture.db, WS, KEY, 32)?, vec![id(1)]);
        assert_eq!(fixture.query()?.matches("\nprior fix: ").count(), 1);
        for number in 42..=76 {
            fixture.source(number, false)?;
        }
        fixture.record((42..=76).map(id).collect())?;
        let expected = std::iter::once(id(1))
            .chain((42..=76).map(id))
            .collect::<Vec<_>>();
        for limit in 0..=32 {
            assert_eq!(
                pack_targets(&fixture.db, WS, KEY, limit)?,
                expected[..limit as usize]
            );
        }
        assert!(pack_targets(&fixture.db, WS, KEY, 33).is_err());
        Ok(())
    }

    #[test]
    fn equal_rendered_utf8_prefixes_do_not_hide_later_distinct_guidance() -> TestResult {
        let fixture = Fixture::new()?;
        let prefix = format!("{REPEATED_REPAIR} {}", "🦀".repeat(220));
        for number in 1..=40 {
            let body = format!("{prefix} Source-specific suffix {number} remains in history.");
            fixture.source_text(number, false, &body)?;
        }
        for number in 41..=43 {
            fixture.source(number, false)?;
        }
        fixture.record((1..=43).map(id).collect())?;
        assert_eq!(
            pack_targets(&fixture.db, WS, KEY, 32)?,
            vec![id(1), id(41), id(42), id(43)]
        );
        let query = fixture.query()?;
        assert_eq!(query.matches("\nprior fix: ").count(), 4);
        assert_eq!(query.matches(" [truncated]").count(), 1);
        assert!(query.contains('🦀'));
        assert!(!query.contains("Source-specific suffix"));
        for number in 41..=43 {
            assert!(query.contains(&format!("Repair {number} uses")));
        }
        assert!(query.len() <= super::super::PACK_RECALL_QUERY_BYTES);
        Ok(())
    }

    #[test]
    fn unusable_copies_cannot_reserve_the_excerpt_of_a_later_admitted_repair() -> TestResult {
        let fixture = Fixture::new()?;
        let denied = fixture.source_text(1, false, REPEATED_REPAIR)?;
        let harmful = fixture.source_text(2, false, REPEATED_REPAIR)?;
        fixture.source_text(3, true, REPEATED_REPAIR)?;
        let admitted = fixture.source_text(4, false, REPEATED_REPAIR)?;
        let other = fixture.source(5, false)?;
        fixture.record((1..=5).map(id).collect())?;
        fixture.db.execute_raw(&format!(
            "UPDATE evidence_spans SET pack_eligibility = 'denied' WHERE id = '{denied}'"
        ))?;
        record_error_repair_links(
            &fixture.db,
            WS,
            &from_rustc(Some("E0277"), "trait bound failed"),
            &ErrorRepairLinkRecording {
                harmful_repairs: vec![harmful],
                ..Default::default()
            },
        )?;
        assert_eq!(
            pack_targets(&fixture.db, WS, KEY, 32)?,
            vec![admitted.clone(), other]
        );
        let query = fixture.query()?;
        assert!(query.contains(&format!("helpful_repair:{admitted}")));
        assert_eq!(query.matches("\nprior fix: ").count(), 2);
        Ok(())
    }

    #[test]
    fn exact_excerpt_comparison_preserves_card_priority_case_and_visible_qualifiers() {
        let mut sample = PackEvidenceDiversity::default();
        assert!(!sample.admit(false, " \n"));
        assert!(sample.admit(false, REPEATED_REPAIR));
        assert!(!sample.admit(false, &format!("  {REPEATED_REPAIR}\n")));
        assert!(sample.admit(true, REPEATED_REPAIR));
        assert!(!sample.admit(true, REPEATED_REPAIR));
        assert!(sample.admit(false, &REPEATED_REPAIR.to_ascii_lowercase()));
        assert!(sample.admit(
            false,
            &format!("{REPEATED_REPAIR} Only for validated inputs.")
        ));
        assert!(sample.admit(false, &format!("{REPEATED_REPAIR} Only for legacy inputs.")));
        assert_eq!(sample.excerpts.len(), 5);
        assert!(
            sample
                .excerpts
                .iter()
                .all(|(_, text)| { text.len() <= super::super::PACK_RECALL_EXCERPT_BYTES })
        );
    }

    #[test]
    fn diversity_is_request_local_and_caller_owned_feedback_can_roll_back() -> TestResult {
        let fixture = Fixture::new()?;
        let first = fixture.source_text(1, false, REPEATED_REPAIR)?;
        let second = fixture.source_text(2, false, REPEATED_REPAIR)?;
        fixture.source(3, false)?;
        fixture.record((1..=3).map(id).collect())?;
        let before = fixture.query()?;
        let history = fixture.db.list_error_repair_links(WS, KEY)?;
        let outcome: crate::db::Result<()> = fixture.db.with_transaction(|| {
            record_error_repair_links(
                &fixture.db,
                WS,
                &from_rustc(Some("E0277"), "trait bound failed"),
                &ErrorRepairLinkRecording {
                    harmful_repairs: vec![first.clone()],
                    ..Default::default()
                },
            )?;
            let inside = fixture.query()?;
            assert!(!inside.contains(&first));
            assert!(inside.contains(&format!("helpful_repair:{second}")));
            assert_eq!(inside.matches("\nprior fix: ").count(), 2);
            fixture
                .db
                .execute_raw("INSERT INTO absent_diversity_rollback_test VALUES (1)")?;
            Ok(())
        });
        assert!(outcome.is_err());
        assert_eq!(fixture.db.list_error_repair_links(WS, KEY)?, history);
        assert_eq!(fixture.query()?, before);
        assert_eq!(pack_targets(&fixture.db, WS, KEY, 32)?, vec![first, id(3)]);
        Ok(())
    }
}
