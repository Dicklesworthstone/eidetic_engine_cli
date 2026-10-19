//! Recheck positive claims before error-recall text is exposed.
//!
//! Stored helpful links are history, not a permanent proof certificate. Reuse
//! the current session interpreter for the exact failure/error/target tuple.
//! Manual feedback on ordinary notes is not reclassified. Derived cards still
//! require their own current-source proof, irrespective of link authorship.
//! The caller owns one read snapshot spanning this gate and final hydration.
//! Failure owners and unclassified card checks are each grouped by session;
//! no link, card or source row is changed by these reads.

use std::collections::{BTreeMap, BTreeSet};

use sqlmodel_core::Value;

use crate::core::cass_error_recall::CASS_ERROR_RECALL_ACTOR;
use crate::core::incident_card::INCIDENT_CARD_ACTOR;
#[cfg(test)]
use crate::core::incident_card::draft_incident_card;
#[cfg(test)]
use crate::core::incident_card::proof::MAX_SOURCE_BYTES;
use crate::core::incident_card::proof::{self, Claim};
use crate::db::{DbConnection, Result};
use crate::models::EvidenceId;

use super::{FEEDBACK_BATCH, harmful_targets, unavailable};

const MAX_CACHED_SESSIONS: usize = 4;

pub(in crate::core::error_diagnosis) struct Gate<'a> {
    connection: &'a DbConnection,
    workspace: &'a str,
    fingerprint: &'a str,
    // Only verdicts are cached, never transcript bodies. Bounded eviction can
    // cause a later reread; it must not turn a valid claim into a refusal.
    sessions: BTreeMap<String, BTreeSet<Claim>>,
    #[cfg(test)]
    source_reads: usize,
    #[cfg(test)]
    ownership_reads: usize,
}

impl<'a> Gate<'a> {
    pub(in crate::core::error_diagnosis) fn new(
        connection: &'a DbConnection,
        workspace: &'a str,
        fingerprint: &'a str,
    ) -> Self {
        Self {
            connection,
            workspace,
            fingerprint,
            sessions: BTreeMap::new(),
            #[cfg(test)]
            source_reads: 0,
            #[cfg(test)]
            ownership_reads: 0,
        }
    }

    /// Explicit harmful feedback always wins. Other rejected claims are
    /// withheld, not relabeled harmful or erased from the diagnostic history.
    pub(in crate::core::error_diagnosis) fn withheld(
        &mut self,
        targets: &[&str],
    ) -> Result<BTreeSet<String>> {
        let mut withheld =
            harmful_targets(self.connection, self.workspace, self.fingerprint, targets)?;
        let selected: BTreeSet<_> = targets
            .iter()
            .copied()
            .filter(|target| !withheld.contains(*target))
            .collect();
        if selected.is_empty() {
            return Ok(withheld);
        }
        let placeholders = (5..5 + selected.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT target_id, CASE WHEN length(CAST(evidence_ref AS BLOB)) <= 128 THEN evidence_ref ELSE NULL END, CASE WHEN created_by = ?4 THEN 'card' ELSE 'turn' END FROM error_repair_links WHERE workspace_id = ?1 AND fingerprint_key = ?2 AND link_kind = 'repair' AND outcome = 'helpful' AND created_by IN (?3, ?4) AND target_id IN ({placeholders}) ORDER BY target_id ASC, link_id ASC LIMIT {}",
            FEEDBACK_BATCH + 1,
        );
        let mut parameters = vec![
            Value::Text(self.workspace.to_owned()),
            Value::Text(self.fingerprint.to_owned()),
            Value::Text(CASS_ERROR_RECALL_ACTOR.to_owned()),
            Value::Text(INCIDENT_CARD_ACTOR.to_owned()),
        ];
        parameters.extend(selected.iter().map(|id| Value::Text((*id).to_owned())));
        let rows = self
            .connection
            .query(&sql, &parameters)
            .map_err(|_| unavailable())?;
        if rows.len() > FEEDBACK_BATCH {
            return Err(unavailable());
        }
        let mut claims = BTreeSet::new();
        for row in rows {
            let target = row.get(0).and_then(Value::as_str).ok_or_else(unavailable)?;
            if !selected.contains(target) {
                return Err(unavailable());
            }
            let failure = row.get(1).and_then(Value::as_str);
            let card = match row.get(2).and_then(Value::as_str) {
                Some("card") => true,
                Some("turn") => false,
                _ => return Err(unavailable()),
            };
            let Some(failure) = failure.filter(|id| canonical_evidence(id)) else {
                withheld.insert(target.to_owned());
                continue;
            };
            if !canonical_evidence(target) {
                withheld.insert(target.to_owned());
                continue;
            }
            claims.insert(Claim {
                failure: failure.to_owned(),
                target: target.to_owned(),
                card,
            });
        }
        // A malformed twin has already refused this target. Do not spend
        // source I/O on another row that cannot reverse that decision.
        claims.retain(|claim| !withheld.contains(&claim.target));
        let classified: BTreeSet<_> = claims.iter().map(|claim| claim.target.clone()).collect();
        self.verify_batch(claims, &mut withheld)?;

        // Link authorship determines whether a relationship is an automatic
        // claim, not whether its target is a machine-derived document. A
        // manual/unlabeled link, or a supplied report without a stored link,
        // must not turn an unproved card into an ordinary repair note.
        // Already classified targets either passed the full interpreter or
        // were withheld above. Do not hydrate and recheck that normal path.
        let unchecked: Vec<_> = selected
            .into_iter()
            .filter(|target| {
                canonical_evidence(target)
                    && !withheld.contains(*target)
                    && !classified.contains(*target)
            })
            .collect();
        self.withhold_unclassified_cards(&unchecked, &mut withheld)?;
        Ok(withheld)
    }

    /// Inspect only the unclassified evidence subset of this bounded batch.
    /// CardChecks recognizes both the derived row shape and reserved locators;
    /// it commits to every field and uses the complete current source proof.
    /// Ordinary notes are unaffected. The caller's snapshot covers these rows
    /// and the later body hydration, so a verdict cannot certify another row.
    fn withhold_unclassified_cards(
        &self,
        targets: &[&str],
        withheld: &mut BTreeSet<String>,
    ) -> Result<()> {
        if targets.is_empty() {
            return Ok(());
        }
        let selected: BTreeSet<_> = targets.iter().copied().collect();
        let rows = self
            .connection
            .get_evidence_spans_with_sessions(targets)
            .map_err(|_| unavailable())?;
        if rows.len() > selected.len() {
            return Err(unavailable());
        }
        let mut seen = BTreeSet::new();
        let mut checks = proof::CardChecks::default();
        for row in rows {
            let span = &row.span;
            if !selected.contains(span.id.as_str()) || !seen.insert(span.id.clone()) {
                return Err(unavailable());
            }
            if span.workspace_id == self.workspace {
                checks.observe(span).map_err(|_| unavailable())?;
            }
        }
        withheld.extend(
            checks
                .withheld(self.connection, self.workspace)
                .map_err(|_| unavailable())?,
        );
        Ok(())
    }

    /// Resolve at most one batch of exact failure owners without reading their
    /// bodies. Group claims before interpreting sessions: target order can
    /// interleave more source sessions than the cross-batch cache can retain.
    /// Increasing that cache would hide, rather than remove, the repeated work.
    fn verify_batch(
        &mut self,
        claims: BTreeSet<Claim>,
        withheld: &mut BTreeSet<String>,
    ) -> Result<()> {
        if claims.is_empty() {
            return Ok(());
        }
        let failures: BTreeSet<_> = claims.iter().map(|claim| claim.failure.as_str()).collect();
        let owners = self.failure_owners(&failures)?;
        let mut groups = BTreeMap::<String, BTreeSet<Claim>>::new();
        for claim in claims {
            if let Some(session) = owners.get(&claim.failure) {
                groups.entry(session.clone()).or_default().insert(claim);
            } else {
                withheld.insert(claim.target);
            }
        }
        // Consume every warm verdict before a new group can evict it. This
        // preserves useful cache hits even when a missing session sorts first.
        groups.retain(|session, pending| {
            let Some(known) = self.sessions.get(session) else {
                return true;
            };
            withhold_unknown(pending, known, withheld);
            false
        });
        for (session, pending) in groups {
            let known =
                session_claims(self.connection, self.workspace, self.fingerprint, &session)?;
            #[cfg(test)]
            {
                self.source_reads += 1;
            }
            withhold_unknown(&pending, &known, withheld);
            if self.sessions.len() == MAX_CACHED_SESSIONS {
                self.sessions.pop_first();
            }
            self.sessions.insert(session, known);
        }
        Ok(())
    }

    fn failure_owners(&mut self, failures: &BTreeSet<&str>) -> Result<BTreeMap<String, String>> {
        if failures.len() > FEEDBACK_BATCH || failures.iter().any(|id| !canonical_evidence(id)) {
            return Err(unavailable());
        }
        if failures.is_empty() {
            return Ok(BTreeMap::new());
        }
        let placeholders = (2..2 + failures.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT id, CASE WHEN length(CAST(session_id AS BLOB)) <= 128 THEN session_id ELSE '' END FROM evidence_spans WHERE workspace_id = ?1 AND id IN ({placeholders}) ORDER BY id ASC LIMIT {}",
            FEEDBACK_BATCH + 1,
        );
        let mut parameters = vec![Value::Text(self.workspace.to_owned())];
        parameters.extend(failures.iter().map(|id| Value::Text((*id).to_owned())));
        #[cfg(test)]
        {
            self.ownership_reads += 1;
        }
        let rows = self
            .connection
            .query(&sql, &parameters)
            .map_err(|_| unavailable())?;
        if rows.len() > failures.len() {
            return Err(unavailable());
        }
        let mut seen = BTreeSet::new();
        let mut owners = BTreeMap::new();
        for row in rows {
            let failure = row.get(0).and_then(Value::as_str).ok_or_else(unavailable)?;
            let session = row.get(1).and_then(Value::as_str).ok_or_else(unavailable)?;
            if !failures.contains(failure)
                || !seen.insert(failure.to_owned())
                || session.len() > 128
            {
                return Err(unavailable());
            }
            if !session.is_empty() {
                owners.insert(failure.to_owned(), session.to_owned());
            }
        }
        Ok(owners)
    }
}

// Refusals are monotone across all groups, including distinct claims for the
// same target. One convenient source can never restore a contradicted twin.
fn withhold_unknown(
    pending: &BTreeSet<Claim>,
    known: &BTreeSet<Claim>,
    withheld: &mut BTreeSet<String>,
) {
    withheld.extend(
        pending
            .iter()
            .filter(|claim| !known.contains(*claim))
            .map(|claim| claim.target.clone()),
    );
}

fn canonical_evidence(id: &str) -> bool {
    id.parse::<EvidenceId>()
        .is_ok_and(|parsed| parsed.to_string() == id)
}

fn session_claims(
    connection: &DbConnection,
    workspace: &str,
    fingerprint: &str,
    session_id: &str,
) -> Result<BTreeSet<Claim>> {
    proof::session_proofs(connection, workspace, Some(fingerprint), session_id)
        .map(|proofs| proofs.claims)
        .map_err(|_| unavailable())
}

#[cfg(test)]
#[path = "error_recall_revalidate_tests.rs"]
mod tests;

#[cfg(test)]
mod unclassified_card_tests {
    use super::*;
    use crate::core::error_diagnosis::{
        ErrorRepairLinkRecording, error_recall_report, pack_error_recall_query_seed,
        recalled_repair_evidence, record_error_repair_links,
    };
    use crate::core::error_recall::from_rustc;
    use crate::core::incident_card::proof::tests::{Fixture, TestResult, WS};
    use crate::db::{CreateEvidenceSpanInput, EvidenceProducerKind};

    #[test]
    fn unproved_cards_cannot_borrow_manual_or_missing_link_authorship() -> TestResult {
        for actor in [None, Some("operator-review"), Some("older-importer")] {
            let fixture = Fixture::new_public(false)?;
            let card = fixture.old_card()?;
            assert!(
                fixture
                    .db
                    .get_search_admitted_evidence_span(&card, WS)?
                    .is_some()
            );
            let diagnostic = from_rustc(Some("E0308"), "mismatched types");
            record_error_repair_links(
                &fixture.db,
                WS,
                &diagnostic,
                &ErrorRepairLinkRecording {
                    helpful_repairs: vec![card.clone()],
                    proof_links: vec![fixture.proof.clone()],
                    created_by: actor.map(str::to_owned),
                    ..Default::default()
                },
            )?;
            let history = fixture.db.list_error_repair_links(WS, "rustc:E0308")?;
            let report = error_recall_report(&fixture.db, WS, &diagnostic)?;
            assert!(report.helpful_repairs.contains(&card));
            let evidence = recalled_repair_evidence(&fixture.db, WS, &report)?;
            assert_eq!(evidence.len(), 1);
            assert_eq!(evidence[0].role, "proof");
            assert!(evidence[0].text.is_none());
            let query = pack_error_recall_query_seed(&fixture.db, WS, &diagnostic)?;
            assert!(!query.contains("prior fix:"));
            assert!(!query.contains(&card));
            assert_eq!(
                fixture.db.list_error_repair_links(WS, "rustc:E0308")?,
                history
            );
            assert!(fixture.db.get_evidence_span(&card)?.is_some());
        }
        Ok(())
    }

    #[test]
    fn a_supplied_report_without_helpful_rows_still_checks_derived_cards() -> TestResult {
        let fixture = Fixture::new_public(false)?;
        let card = fixture.old_card()?;
        let diagnostic = from_rustc(Some("E0308"), "mismatched types");
        let mut report = error_recall_report(&fixture.db, WS, &diagnostic)?;
        assert!(report.helpful_repairs.is_empty());
        report.helpful_repairs.push(card);
        report.helpful_repairs.push(fixture.repair.clone());
        let evidence = recalled_repair_evidence(&fixture.db, WS, &report)?;
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].evidence_id, fixture.repair);
        assert_eq!(evidence[0].role, "repair");
        Ok(())
    }

    #[test]
    fn proved_manual_cards_still_precede_notes_and_harm_still_wins() -> TestResult {
        let fixture = Fixture::new_public(true)?;
        let card = fixture.current_card()?;
        let diagnostic = from_rustc(Some("E0308"), "mismatched types");
        fixture.db.execute_raw(&format!(
            "UPDATE error_repair_links SET created_by = 'operator-review' WHERE target_id = '{}'",
            card.id,
        ))?;
        let report = error_recall_report(&fixture.db, WS, &diagnostic)?;
        let evidence = recalled_repair_evidence(&fixture.db, WS, &report)?;
        assert_eq!(evidence[0].evidence_id, card.id);
        assert_eq!(evidence[0].role, "incident_card");
        assert_eq!(
            evidence[0].text.as_deref(),
            Some(card.reader_text().as_ref())
        );
        assert!(
            evidence
                .iter()
                .any(|item| item.evidence_id == fixture.repair)
        );
        let query = pack_error_recall_query_seed(&fixture.db, WS, &diagnostic)?;
        assert!(query.contains(&format!("helpful_repair:{}", card.id)));
        record_error_repair_links(
            &fixture.db,
            WS,
            &diagnostic,
            &ErrorRepairLinkRecording {
                harmful_repairs: vec![card.id.clone()],
                ..Default::default()
            },
        )?;
        let evidence = recalled_repair_evidence(&fixture.db, WS, &report)?;
        assert!(evidence.iter().all(|item| item.evidence_id != card.id));
        assert!(
            evidence
                .iter()
                .any(|item| item.evidence_id == fixture.repair)
        );
        Ok(())
    }

    #[test]
    fn rejected_manual_cards_refill_the_pack_window_instead_of_hiding_later_repairs() -> TestResult
    {
        let fixture = Fixture::new_public(false)?;
        let old = fixture.old_card()?;
        let template = fixture.db.get_evidence_span(&old)?.ok_or("old card")?;
        let mut targets = Vec::new();
        for number in 1_u128..=40 {
            let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
            fixture.db.insert_evidence_span(
                &id,
                &CreateEvidenceSpanInput {
                    workspace_id: WS.to_owned(),
                    session_id: fixture.session.clone(),
                    memory_id: None,
                    producer_kind: EvidenceProducerKind::CassImport,
                    cass_span_id: format!("ee-incident-card:incident_card.v1:{id}"),
                    span_kind: "summary".to_owned(),
                    start_line: template.start_line,
                    end_line: template.end_line,
                    start_byte: None,
                    end_byte: None,
                    role: None,
                    content_hash: template.content_hash.clone(),
                    excerpt: template.excerpt.clone(),
                    metadata_json: None,
                    inherited_redaction_classes: Vec::new(),
                },
            )?;
            assert!(id < fixture.repair);
            targets.push(id);
        }
        targets.push(fixture.repair.clone());
        let diagnostic = from_rustc(Some("E0308"), "mismatched types");
        record_error_repair_links(
            &fixture.db,
            WS,
            &diagnostic,
            &ErrorRepairLinkRecording {
                helpful_repairs: targets.clone(),
                created_by: Some("operator-review".to_owned()),
                ..Default::default()
            },
        )?;
        let query = pack_error_recall_query_seed(&fixture.db, WS, &diagnostic)?;
        assert!(query.contains(&format!("helpful_repair:{}", fixture.repair)));
        assert!(query.contains("flushing pending writes"));
        assert_eq!(query.matches("\nprior fix: ").count(), 1);
        for id in &targets[..40] {
            assert!(!query.contains(id));
        }
        assert_eq!(
            error_recall_report(&fixture.db, WS, &diagnostic)?
                .helpful_repairs
                .len(),
            41
        );
        Ok(())
    }

    #[test]
    fn a_manual_card_with_changed_current_sources_is_withheld_from_a_cached_report() -> TestResult {
        let fixture = Fixture::new_public(true)?;
        let card = fixture.current_card()?;
        let diagnostic = from_rustc(Some("E0308"), "mismatched types");
        fixture.db.execute_raw(&format!(
            "UPDATE error_repair_links SET created_by = NULL WHERE target_id = '{}'",
            card.id,
        ))?;
        let report = error_recall_report(&fixture.db, WS, &diagnostic)?;
        assert!(
            recalled_repair_evidence(&fixture.db, WS, &report)?
                .iter()
                .any(|item| item.evidence_id == card.id)
        );
        fixture.db.execute_raw(&format!(
            "UPDATE evidence_spans SET excerpt = 'changed source result' WHERE id = '{}'",
            fixture.proof,
        ))?;
        let history = fixture.db.list_error_repair_links(WS, "rustc:E0308")?;
        let evidence = recalled_repair_evidence(&fixture.db, WS, &report)?;
        assert!(evidence.iter().all(|item| item.role == "proof"));
        assert!(
            !pack_error_recall_query_seed(&fixture.db, WS, &diagnostic)?.contains("prior fix:")
        );
        assert_eq!(
            fixture.db.list_error_repair_links(WS, "rustc:E0308")?,
            history
        );
        assert_eq!(
            fixture
                .db
                .get_evidence_span(&card.id)?
                .ok_or("preserved card")?
                .excerpt,
            card.excerpt
        );
        Ok(())
    }
}
