//! Recheck CASS-derived positive claims before error-recall text is exposed.
//!
//! Stored helpful links are history, not a permanent proof certificate. Reuse
//! the current session interpreter for the exact failure/error/target tuple.
//! Manual feedback is not reclassified; no link, card or source row is changed.
//! The caller owns one read snapshot spanning this gate and final hydration.
//! Failure owners are batched; each referenced session is interpreted at most
//! once per batch even when target ordering interleaves many source sessions.

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
            claims.insert(Claim { failure: failure.to_owned(), target: target.to_owned(), card });
        }
        // A malformed twin has already refused this target. Do not spend
        // source I/O on another row that cannot reverse that decision.
        claims.retain(|claim| !withheld.contains(&claim.target));
        self.verify_batch(claims, &mut withheld)?;
        Ok(withheld)
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
            let known = session_claims(self.connection, self.workspace, self.fingerprint, &session)?;
            #[cfg(test)]
            { self.source_reads += 1; }
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
        { self.ownership_reads += 1; }
        let rows = self.connection.query(&sql, &parameters).map_err(|_| unavailable())?;
        if rows.len() > failures.len() {
            return Err(unavailable());
        }
        let mut seen = BTreeSet::new();
        let mut owners = BTreeMap::new();
        for row in rows {
            let failure = row.get(0).and_then(Value::as_str).ok_or_else(unavailable)?;
            let session = row.get(1).and_then(Value::as_str).ok_or_else(unavailable)?;
            if !failures.contains(failure) || !seen.insert(failure.to_owned()) || session.len() > 128 {
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
    withheld.extend(pending.iter()
        .filter(|claim| !known.contains(*claim))
        .map(|claim| claim.target.clone()));
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
    let Some(session) = connection.get_session(session_id).map_err(|_| unavailable())? else {
        return Ok(BTreeSet::new());
    };
    if session.id != session_id || session.workspace_id != workspace {
        return Ok(BTreeSet::new());
    }
    let stored = connection.list_evidence_spans_for_session(session_id).map_err(|_| unavailable())?;
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
