//! Current-source proof shared by historical recall and ordinary answers.
//!
//! A stored card is derived text, not a permanent certificate. Interpret the
//! original transcript once per referenced session, excluding every reserved
//! card row. Neither a helpful link nor absence of one grants proof authority.
//! Callers keep these reads inside their existing source snapshot; this module
//! never starts a transaction, changes history, or invokes a model or index.

use std::collections::{BTreeMap, BTreeSet};

use sqlmodel_core::Value;

#[cfg(test)]
use super::draft_incident_card;
use super::{IncidentCardDraft, IncidentCardSources};
use crate::core::cass_error_recall::session_failure_arcs;
use crate::db::{DbConnection, DbError, DbOperation, Result, StoredEvidenceSpan};
use crate::models::{EvidenceId, SessionId};

// Interpreter-input limits, not a total-query-work or process-RSS promise.
pub(crate) const MAX_SOURCE_ROWS: usize = 4096;
pub(crate) const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
const MAX_CLAIMS: usize = MAX_SOURCE_ROWS * 4;

type CardSignature = [u8; 32];

fn unavailable() -> DbError {
    DbError::MalformedRow {
        operation: DbOperation::Query,
        message: "Could not verify incident sources; derived guidance withheld".to_owned(),
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct Claim {
    pub(crate) failure: String,
    pub(crate) target: String,
    pub(crate) card: bool,
}

#[derive(Default)]
pub(crate) struct SessionProofs {
    pub(crate) claims: BTreeSet<Claim>,
    cards: BTreeMap<String, CardSignature>,
}

/// The reserved locator survives an altered prefix, kind or role. Such a row
/// must not turn into ordinary transcript evidence or feed its own derivation.
pub(crate) fn requires_proof(span: &StoredEvidenceSpan) -> bool {
    span.is_derived_incident_card()
        || span.cass_span_id.starts_with("ee-incident-card:")
        || span.has_screened_upstream_reference(&format!(
            "ee-incident-card:{}:{}",
            super::INCIDENT_CARD_DERIVATION,
            span.id,
        ))
}

/// Equality includes optional source-memory and byte coordinates: extra lineage
/// cannot be smuggled onto a genuine deterministic card ID.
fn matches_draft(card: &StoredEvidenceSpan, draft: &IncidentCardDraft) -> bool {
    card.is_derived_incident_card()
        && card.id == draft.id
        && card.matches_screened_input(&draft.input).unwrap_or(false)
}

/// Commit to the exact admitted row, not just its ID or declared content hash.
/// The source reader still owns live admission. Signatures are transient and
/// never replace a source hash, citation revision, or durable provenance.
fn card_signature(span: &StoredEvidenceSpan) -> Result<CardSignature> {
    let bytes = serde_json::to_vec(&(
        &span.id,
        &span.workspace_id,
        &span.session_id,
        &span.cass_span_id,
        &span.producer_kind,
        &span.span_kind,
        &span.role,
        &span.memory_id,
        span.start_line,
        span.end_line,
        &span.start_byte,
        &span.end_byte,
        &span.excerpt,
        &span.content_hash,
        &span.metadata_json,
    ))
    .map_err(|_| unavailable())?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"ee.incident_card.reader_proof.v1\0");
    hash.update(&bytes);
    Ok(*hash.finalize().as_bytes())
}

/// Compact checks for a caller's already admitted candidate set. This retains
/// IDs and fixed-size commitments, never another copy of the card bodies. Group
/// by session before checking, so interleaved visitor pages cannot thrash a
/// bounded transcript cache. Additional storage scales with selected cards;
/// each referenced session is loaded at most once and released immediately.
#[derive(Default)]
pub(crate) struct CardChecks {
    sessions: BTreeMap<String, BTreeMap<String, CardSignature>>,
    refused: BTreeSet<String>,
}

impl CardChecks {
    pub(crate) fn observe(&mut self, span: &StoredEvidenceSpan) -> Result<()> {
        if !requires_proof(span) {
            return Ok(());
        }
        if !span.is_derived_incident_card()
            || source_bytes(span) > MAX_SOURCE_BYTES
            || !span
                .id
                .parse::<EvidenceId>()
                .is_ok_and(|id| id.to_string() == span.id)
            || !span
                .session_id
                .parse::<SessionId>()
                .is_ok_and(|id| id.to_string() == span.session_id)
        {
            self.refused.insert(span.id.clone());
            return Ok(());
        }
        let signature = card_signature(span)?;
        let cards = self.sessions.entry(span.session_id.clone()).or_default();
        if cards
            .insert(span.id.clone(), signature)
            .is_some_and(|prior| prior != signature)
        {
            self.refused.insert(span.id.clone());
        }
        Ok(())
    }

    /// Missing, foreign, over-budget or unproved sessions refuse only their
    /// cards. Storage failures return an error, never a partially checked set.
    pub(crate) fn withheld(
        self,
        connection: &DbConnection,
        workspace: &str,
    ) -> Result<BTreeSet<String>> {
        self.withheld_with(|session| session_proofs(connection, workspace, None, session))
    }

    /// Feed a bounded reducer only exact, previously admitted candidates whose
    /// complete session proof succeeded. Reuse the rows already hydrated by
    /// proof evaluation instead of retaining candidate bodies during the first
    /// scan, fetching each card separately, or rescanning the whole corpus.
    ///
    /// Observation and this visit must share the caller's source snapshot.
    /// Proof is additional to that caller's admission and egress policy, not a
    /// substitute for either. Callbacks must only build request-local output:
    /// an error in a later session invalidates the entire accumulated result.
    pub(crate) fn visit_verified(
        self,
        connection: &DbConnection,
        workspace: &str,
        visit: impl FnMut(StoredEvidenceSpan) -> Result<()>,
    ) -> Result<()> {
        self.visit_verified_with(
            |session| load_session_proofs(connection, workspace, None, session),
            visit,
        )
    }

    fn visit_verified_with(
        self,
        mut load: impl FnMut(&str) -> Result<LoadedSessionProofs>,
        mut visit: impl FnMut(StoredEvidenceSpan) -> Result<()>,
    ) -> Result<()> {
        for (session, pending) in self.sessions {
            let mut known = load(&session)?;
            for (id, signature) in pending {
                if self.refused.contains(&id) || known.proofs.cards.get(&id) != Some(&signature) {
                    continue;
                }
                // The complete interpreter and claim cap run before the first
                // callback. Unobserved cards can never expand the admitted set.
                visit(known.cards.remove(&id).ok_or_else(unavailable)?)?;
            }
        }
        Ok(())
    }

    fn withheld_with(
        self,
        mut load: impl FnMut(&str) -> Result<SessionProofs>,
    ) -> Result<BTreeSet<String>> {
        let mut refused = self.refused;
        for (session, pending) in self.sessions {
            let known = load(&session)?;
            for (id, signature) in pending {
                if known.cards.get(&id) != Some(&signature) {
                    refused.insert(id);
                }
            }
        }
        Ok(refused)
    }
}

pub(crate) fn source_bytes(span: &StoredEvidenceSpan) -> usize {
    span.excerpt
        .len()
        .saturating_add(span.metadata_json.as_deref().map_or(0, str::len))
}

pub(crate) fn session_proofs(
    connection: &DbConnection,
    workspace: &str,
    fingerprint: Option<&str>,
    session_id: &str,
) -> Result<SessionProofs> {
    load_session_proofs(connection, workspace, fingerprint, session_id).map(|loaded| loaded.proofs)
}

/// Own at most one proof-input-bounded session until its reducer has consumed
/// verified cards. Ordinary proof callers still receive compact commitments
/// only; this is not a transcript cache or a new persistent representation.
#[derive(Default)]
struct LoadedSessionProofs {
    proofs: SessionProofs,
    cards: BTreeMap<String, StoredEvidenceSpan>,
}

fn load_session_proofs(
    connection: &DbConnection,
    workspace: &str,
    fingerprint: Option<&str>,
    session_id: &str,
) -> Result<LoadedSessionProofs> {
    // Count bytes before hydration. Include derived rows in this accounting:
    // they are inspected for exact card identity, never fed back as sources.
    let rows = connection.query(
        "SELECT CAST(COUNT(*) AS TEXT), CAST(COALESCE(SUM(length(CAST(excerpt AS BLOB)) + length(CAST(COALESCE(metadata_json, '') AS BLOB))), 0) AS TEXT) FROM evidence_spans WHERE session_id = ?1",
        &[Value::Text(session_id.to_owned())],
    ).map_err(|_| unavailable())?;
    if rows.len() != 1 {
        return Err(unavailable());
    }
    let count = |index| -> Result<usize> {
        rows[0]
            .get(index)
            .and_then(Value::as_str)
            .and_then(|value| value.parse().ok())
            .ok_or_else(unavailable)
    };
    let row_count = count(0)?;
    let byte_count = count(1)?;
    if row_count == 0 || row_count > MAX_SOURCE_ROWS || byte_count > MAX_SOURCE_BYTES {
        return Ok(LoadedSessionProofs::default());
    }
    let Some(session) = connection
        .get_session(session_id)
        .map_err(|_| unavailable())?
    else {
        return Ok(LoadedSessionProofs::default());
    };
    if session.id != session_id || session.workspace_id != workspace {
        return Ok(LoadedSessionProofs::default());
    }
    let stored = connection
        .list_evidence_spans_for_session(session_id)
        .map_err(|_| unavailable())?;
    if stored.len() != row_count
        || stored
            .iter()
            .fold(0_usize, |sum, span| sum.saturating_add(source_bytes(span)))
            != byte_count
    {
        return Err(unavailable());
    }
    let (cards, sources): (Vec<_>, Vec<_>) = stored.into_iter().partition(requires_proof);
    let cards: BTreeMap<_, _> = cards
        .into_iter()
        .map(|card| (card.id.clone(), card))
        .collect();
    let card_sources = IncidentCardSources::new(&sources);
    let mut proofs = SessionProofs::default();
    for arc in session_failure_arcs(workspace, &session, &sources) {
        if fingerprint.is_some_and(|key| {
            !arc.diagnostics
                .iter()
                .any(|diagnostic| diagnostic.layered_key().key == key)
        }) {
            continue;
        }
        let Some(resolution) = &arc.resolution else {
            continue;
        };
        for target in &resolution.repair_ids {
            proofs.claims.insert(Claim {
                failure: arc.failure_id.clone(),
                target: target.clone(),
                card: false,
            });
        }
        // The complete interpreter still establishes ordinary repair claims.
        // A nonexistent stored card needs no tokenization or facet rendering.
        let card_id = super::incident_card_id(workspace, &arc.failure_id);
        if let Some(card) = cards.get(&card_id)
            && let Some(draft) = card_sources.draft(workspace, session_id, &arc)
            && matches_draft(card, &draft)
        {
            proofs.cards.insert(draft.id.clone(), card_signature(card)?);
            proofs.claims.insert(Claim {
                failure: arc.failure_id,
                target: draft.id,
                card: true,
            });
        }
        if proofs.claims.len() > MAX_CLAIMS {
            return Ok(LoadedSessionProofs::default());
        }
    }
    Ok(LoadedSessionProofs { proofs, cards })
}

#[cfg(test)]
#[path = "incident_card_proof_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
mod reader_visit_tests {
    use super::tests::{Fixture, TestResult, WS};
    use super::*;

    #[test]
    fn source_proofs_without_stored_cards_keep_repair_claims_without_writing_cards() -> TestResult {
        let fixture = Fixture::new_public(true)?;
        let before = fixture
            .db
            .list_evidence_spans_for_session(&fixture.session)?;
        assert!(before.iter().all(|span| !requires_proof(span)));
        let proofs = session_proofs(&fixture.db, WS, None, &fixture.session)?;
        assert!(proofs.cards.is_empty());
        assert!(proofs.claims.contains(&Claim {
            failure: fixture.failure.clone(),
            target: fixture.repair.clone(),
            card: false,
        }));
        let after = fixture
            .db
            .list_evidence_spans_for_session(&fixture.session)?;
        assert_eq!(after.len(), before.len());
        assert!(after.iter().all(|span| !requires_proof(span)));
        Ok(())
    }

    #[test]
    fn verified_visit_agrees_with_withheld_and_keeps_exact_native_card_fields() -> TestResult {
        for valid in [false, true] {
            let fixture = Fixture::new(valid)?;
            let card = if valid {
                fixture.current_card()?
            } else {
                let id = fixture.old_card()?;
                fixture
                    .db
                    .get_evidence_span(&id)?
                    .ok_or("historical card")?
            };
            let mut checks = CardChecks::default();
            checks.observe(&card)?;
            let withheld = checks.withheld(&fixture.db, WS)?;
            let mut checks = CardChecks::default();
            checks.observe(&card)?;
            let mut visited = Vec::new();
            checks.visit_verified(&fixture.db, WS, |span| {
                assert_eq!(card_signature(&span)?, card_signature(&card)?);
                assert_eq!(span.pack_entity_revision(), card.pack_entity_revision());
                assert_eq!(
                    span.canonical_provenance_uri(),
                    card.canonical_provenance_uri()
                );
                visited.push(span.id);
                Ok(())
            })?;
            assert_eq!(visited.contains(&card.id), !withheld.contains(&card.id));
            assert_eq!(visited.len(), usize::from(valid));
        }
        Ok(())
    }

    #[test]
    fn repeated_candidates_load_one_session_and_emit_one_previously_observed_card() -> TestResult {
        let fixture = Fixture::new(true)?;
        let card = fixture.current_card()?;
        let mut checks = CardChecks::default();
        for _ in 0..300 {
            checks.observe(&card)?;
        }
        let mut loads = 0;
        let mut visits = 0;
        checks.visit_verified_with(
            |session| {
                loads += 1;
                load_session_proofs(&fixture.db, WS, None, session)
            },
            |span| {
                visits += 1;
                assert_eq!(span.id, card.id);
                Ok(())
            },
        )?;
        assert_eq!(loads, 1);
        assert_eq!(visits, 1);
        Ok(())
    }

    #[test]
    fn ordinary_candidates_do_not_load_proofs_or_expand_to_unobserved_cards() -> TestResult {
        let fixture = Fixture::new(true)?;
        let _card = fixture.current_card()?;
        let ordinary = fixture
            .db
            .get_evidence_span(&fixture.repair)?
            .ok_or("repair")?;
        let mut checks = CardChecks::default();
        checks.observe(&ordinary)?;
        checks.visit_verified_with(
            |_| panic!("ordinary evidence must not query incident proof"),
            |_| panic!("a genuine but unobserved card cannot expand the candidate set"),
        )?;
        Ok(())
    }

    #[test]
    fn conflicting_observations_never_visit_a_genuine_row_under_a_reused_id() -> TestResult {
        let fixture = Fixture::new(true)?;
        let card = fixture.current_card()?;
        let mut changed = card.clone();
        changed.excerpt.push_str(" Altered repair guidance.");
        for candidates in [
            vec![changed.clone()],
            vec![card.clone(), changed.clone()],
            vec![changed.clone(), card.clone()],
        ] {
            let mut checks = CardChecks::default();
            for candidate in candidates {
                checks.observe(&candidate)?;
            }
            checks.visit_verified(&fixture.db, WS, |_| {
                panic!("a proof for the stored row cannot certify a conflicting observation")
            })?;
        }
        Ok(())
    }

    #[test]
    fn proof_input_refusal_emits_nothing_and_consumer_errors_propagate() -> TestResult {
        let fixture = Fixture::new(true)?;
        let card = fixture.current_card()?;
        let mut checks = CardChecks::default();
        checks.observe(&card)?;
        let mut visits = 0;
        let error = checks
            .visit_verified(&fixture.db, WS, |_| {
                visits += 1;
                Err(unavailable())
            })
            .expect_err("consumer refusal must propagate");
        assert_eq!(visits, 1);
        assert!(!error.to_string().contains(&fixture.session));
        fixture.db.execute_raw(&format!(
            "UPDATE evidence_spans SET metadata_json = '{}' WHERE id = '{}'",
            serde_json::to_string(&"x".repeat(MAX_SOURCE_BYTES))?,
            fixture.failure,
        ))?;
        let mut checks = CardChecks::default();
        checks.observe(&card)?;
        checks.visit_verified(&fixture.db, WS, |_| {
            panic!("over-budget proof inputs cannot publish a prefix of verified cards")
        })?;
        Ok(())
    }

    #[test]
    fn storage_failure_before_proof_never_visits_a_candidate() -> TestResult {
        let fixture = Fixture::new(true)?;
        let card = fixture.current_card()?;
        let mut checks = CardChecks::default();
        checks.observe(&card)?;
        let result = checks.visit_verified_with(
            |_| Err(unavailable()),
            |_| panic!("a failed source read supplies no proof"),
        );
        assert!(result.is_err());
        Ok(())
    }
}
