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

    /// Explicit harmful feedback always wins. Other rejected CASS claims are
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
