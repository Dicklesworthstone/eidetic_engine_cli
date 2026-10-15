//! Recheck CASS-derived positive claims before error-recall text is exposed.
//!
//! Stored helpful links are history, not a permanent proof certificate. Reuse
//! the current session interpreter for the exact failure/error/target tuple.
//! Manual feedback is not reclassified; no link, card or source row is changed.
//! The caller owns one read snapshot spanning this gate and final hydration.

use std::collections::{BTreeMap, BTreeSet};

use sqlmodel_core::Value;

use crate::core::cass_error_recall::{CASS_ERROR_RECALL_ACTOR, session_failure_arcs};
use crate::core::incident_card::{INCIDENT_CARD_ACTOR, draft_incident_card};
use crate::db::{DbConnection, Result, StoredEvidenceSpan};
use crate::models::EvidenceId;

use super::{FEEDBACK_BATCH, harmful_targets, unavailable};

// These bound interpreter input, not total query work or process RSS. Large
// or malformed source sessions cannot establish positive advice in this lane.
const MAX_SOURCE_ROWS: usize = 4096;
const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
const MAX_CACHED_SESSIONS: usize = 4;
const MAX_CLAIMS: usize = MAX_SOURCE_ROWS * 4;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Claim {
    failure: String,
    target: String,
    card: bool,
}

pub(in crate::core::error_diagnosis) struct Gate<'a> {
    connection: &'a DbConnection,
    workspace: &'a str,
    fingerprint: &'a str,
    // Only verdicts are cached, never transcript bodies. Bounded eviction can
    // cause a later reread; it must not turn a valid claim into a refusal.
    sessions: BTreeMap<String, BTreeSet<Claim>>,
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
        }
    }

    /// Explicit harmful feedback always wins. Other rejected CASS claims are
    /// withheld, not relabeled harmful or erased from the diagnostic history.
    pub(in crate::core::error_diagnosis) fn withheld(
        &mut self,
        targets: &[&str],
    ) -> Result<BTreeSet<String>> {
        let mut withheld = harmful_targets(
            self.connection,
            self.workspace,
            self.fingerprint,
            targets,
        )?;
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
        let rows = self.connection.query(&sql, &parameters).map_err(|_| unavailable())?;
        if rows.len() > FEEDBACK_BATCH {
            return Err(unavailable());
        }
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
            let claim = Claim { failure: failure.to_owned(), target: target.to_owned(), card };
            if !self.verified(&claim)? {
                // Monotone across repeated source rows: one contradictory
                // observation cannot be repaired by a convenient later twin.
                withheld.insert(target.to_owned());
            }
        }
        Ok(withheld)
    }

    fn verified(&mut self, claim: &Claim) -> Result<bool> {
        // Resolve ownership without loading a potentially large failing body.
        let rows = self.connection.query(
            "SELECT CASE WHEN length(CAST(session_id AS BLOB)) <= 128 THEN session_id ELSE '' END FROM evidence_spans WHERE workspace_id = ?1 AND id = ?2",
            &[Value::Text(self.workspace.to_owned()), Value::Text(claim.failure.clone())],
        ).map_err(|_| unavailable())?;
        if rows.len() > 1 {
            return Err(unavailable());
        }
        let Some(row) = rows.first() else { return Ok(false); };
        let session = row.get(0).and_then(Value::as_str).ok_or_else(unavailable)?;
        if session.is_empty() {
            return Ok(false);
        }
        if !self.sessions.contains_key(session) {
            let claims = session_claims(self.connection, self.workspace, self.fingerprint, session)?;
            if self.sessions.len() == MAX_CACHED_SESSIONS {
                self.sessions.pop_first();
            }
            self.sessions.insert(session.to_owned(), claims);
        }
        Ok(self.sessions.get(session).is_some_and(|claims| claims.contains(claim)))
    }
}

fn canonical_evidence(id: &str) -> bool {
    id.parse::<EvidenceId>().is_ok_and(|parsed| parsed.to_string() == id)
}

fn source_bytes(span: &StoredEvidenceSpan) -> usize {
    span.excerpt.len().saturating_add(span.metadata_json.as_deref().map_or(0, str::len))
}

fn session_claims(
    connection: &DbConnection,
    workspace: &str,
    fingerprint: &str,
    session_id: &str,
) -> Result<BTreeSet<Claim>> {
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
        rows[0].get(index).and_then(Value::as_str)
            .and_then(|value| value.parse().ok()).ok_or_else(unavailable)
    };
    let row_count = count(0)?;
    let byte_count = count(1)?;
    if row_count == 0 || row_count > MAX_SOURCE_ROWS || byte_count > MAX_SOURCE_BYTES {
        return Ok(BTreeSet::new());
    }
    let Some(session) = connection.get_session(session_id)? else {
        return Ok(BTreeSet::new());
    };
    if session.id != session_id || session.workspace_id != workspace {
        return Ok(BTreeSet::new());
    }
    let stored = connection.list_evidence_spans_for_session(session_id)?;
    if stored.len() != row_count
        || stored.iter().fold(0_usize, |sum, span| sum.saturating_add(source_bytes(span))) != byte_count
    {
        return Err(unavailable());
    }
    let (cards, sources): (Vec<_>, Vec<_>) = stored.into_iter()
        .partition(StoredEvidenceSpan::is_derived_incident_card);
    let cards: BTreeMap<_, _> = cards.into_iter().map(|card| (card.id.clone(), card)).collect();
    let mut claims = BTreeSet::new();
    for arc in session_failure_arcs(workspace, &session, &sources) {
        if !arc.diagnostics.iter().any(|diagnostic| diagnostic.layered_key().key == fingerprint) {
            continue;
        }
        let Some(resolution) = &arc.resolution else { continue; };
        for target in &resolution.repair_ids {
            claims.insert(Claim { failure: arc.failure_id.clone(), target: target.clone(), card: false });
        }
        if let Some(draft) = draft_incident_card(workspace, session_id, &sources, &arc)
            && cards.get(&draft.id).is_some_and(|card| {
                // Same deterministic ID does not prove that old card text
                // describes today's valid arc. Never rewrite it on a read.
                card.workspace_id == workspace && card.session_id == session_id
                    && card.cass_span_id == draft.input.cass_span_id
                    && card.start_line == draft.input.start_line
                    && card.end_line == draft.input.end_line
                    && card.excerpt == draft.input.excerpt
                    && card.content_hash == draft.input.content_hash
                    && card.metadata_json == draft.input.metadata_json
            })
        {
            claims.insert(Claim { failure: arc.failure_id, target: draft.id, card: true });
        }
        if claims.len() > MAX_CLAIMS {
            return Ok(BTreeSet::new());
        }
    }
    Ok(claims)
}

#[cfg(test)]
#[path = "error_recall_revalidate_tests.rs"]
mod tests;
