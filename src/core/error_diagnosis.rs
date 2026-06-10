//! bd-1n0np.4.10 — error-recall wiring: the callers the dead library was missing.
//!
//! The pure error-recall library (`core::error_recall`) and the V072
//! `error_fingerprints` store both shipped, but NOTHING called them — the
//! subsystem was unreachable dead code: the store could never be populated or
//! read by any command (review finding bd-1n0np.4.10). This module supplies the
//! two callers ADR-0057 specifies:
//!
//! - [`record_error_fingerprint`] (writer): persist the fingerprint of a
//!   canonicalized diagnostic so recall can later find this error class.
//! - [`diagnose_error`] (reader): recall a prior fingerprint for the EXACT error
//!   class via the layered key (`(tool, canonical_code)` → message-template).
//!
//! Redaction-by-default (ADR-0057): only the fingerprint key + masked signatures
//! (blake3 message-template signature, masked location shape, simhash) are
//! stored, never the raw log. No tool execution — both functions diagnose text
//! they are handed. The `ee diagnose-error` CLI consumes [`diagnose_error`].

use chrono::Utc;

use crate::core::error_recall::{CanonicalDiagnostic, ErrorFingerprint, ErrorRepairLinkKind};
use crate::db::{
    CreateErrorRepairLinkInput, DbConnection, Result, StoredErrorFingerprint, StoredErrorRepairLink,
};

/// Persist (or refresh) the error fingerprint for a canonicalized diagnostic,
/// linking the failing error class into the truth store so recall can later find
/// it (ADR-0057 writer). Returns the stored row. Redaction-safe: stores the
/// fingerprint + masked signatures only, never raw log content.
///
/// # Errors
///
/// Propagates any database error from the underlying upsert.
pub fn record_error_fingerprint(
    connection: &DbConnection,
    workspace_id: &str,
    canonical: &CanonicalDiagnostic,
) -> Result<StoredErrorFingerprint> {
    let fingerprint = ErrorFingerprint::from_canonical(canonical);
    let now = Utc::now().to_rfc3339();
    let stored = stored_from_fingerprint(&fingerprint, workspace_id, &now);
    connection.upsert_error_fingerprint(&stored)?;
    Ok(stored)
}

/// Error-repair links to persist for one diagnosed fingerprint.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ErrorRepairLinkRecording {
    pub helpful_repairs: Vec<String>,
    pub harmful_repairs: Vec<String>,
    pub proof_links: Vec<String>,
    pub stale_version_warnings: Vec<String>,
    pub created_by: Option<String>,
}

impl ErrorRepairLinkRecording {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.helpful_repairs.is_empty()
            && self.harmful_repairs.is_empty()
            && self.proof_links.is_empty()
            && self.stale_version_warnings.is_empty()
    }
}

/// Persist repair/proof/outcome links for a canonicalized diagnostic. The
/// fingerprint row is upserted first so the link table's foreign key is always
/// satisfied. Link IDs are deterministic over workspace, fingerprint, kind,
/// target, and outcome, making repeat observations idempotent.
pub fn record_error_repair_links(
    connection: &DbConnection,
    workspace_id: &str,
    canonical: &CanonicalDiagnostic,
    recording: &ErrorRepairLinkRecording,
) -> Result<Vec<StoredErrorRepairLink>> {
    let stored = record_error_fingerprint(connection, workspace_id, canonical)?;
    let mut links = Vec::new();

    for target_id in &recording.helpful_repairs {
        push_repair_link(
            &mut links,
            workspace_id,
            &stored.fingerprint_key,
            ErrorRepairLinkKind::Repair,
            target_id,
            "helpful",
            None,
            recording.created_by.as_deref(),
        );
    }
    for target_id in &recording.harmful_repairs {
        push_repair_link(
            &mut links,
            workspace_id,
            &stored.fingerprint_key,
            ErrorRepairLinkKind::Repair,
            target_id,
            "harmful",
            None,
            recording.created_by.as_deref(),
        );
    }
    for target_id in &recording.proof_links {
        push_repair_link(
            &mut links,
            workspace_id,
            &stored.fingerprint_key,
            ErrorRepairLinkKind::Proof,
            target_id,
            "unknown",
            None,
            recording.created_by.as_deref(),
        );
    }
    for warning in &recording.stale_version_warnings {
        push_repair_link(
            &mut links,
            workspace_id,
            &stored.fingerprint_key,
            ErrorRepairLinkKind::Outcome,
            warning,
            "unknown",
            Some(warning),
            recording.created_by.as_deref(),
        );
    }

    links.sort_by(|left, right| {
        left.link_kind
            .cmp(&right.link_kind)
            .then_with(|| left.outcome.cmp(&right.outcome))
            .then_with(|| left.target_id.cmp(&right.target_id))
            .then_with(|| left.link_id.cmp(&right.link_id))
    });
    links.dedup_by(|left, right| {
        left.workspace_id == right.workspace_id
            && left.fingerprint_key == right.fingerprint_key
            && left.link_kind == right.link_kind
            && left.target_id == right.target_id
            && left.outcome == right.outcome
    });

    let inputs = links
        .iter()
        .map(|link| CreateErrorRepairLinkInput {
            link_id: link.link_id.clone(),
            workspace_id: link.workspace_id.clone(),
            fingerprint_key: link.fingerprint_key.clone(),
            link_kind: link.link_kind.clone(),
            target_id: link.target_id.clone(),
            outcome: link.outcome.clone(),
            evidence_ref: link.evidence_ref.clone(),
            stale_version_warning: link.stale_version_warning.clone(),
            created_by: link.created_by.clone(),
        })
        .collect::<Vec<_>>();
    connection.upsert_error_repair_links(&inputs)?;
    connection.list_error_repair_links(workspace_id, &stored.fingerprint_key)
}

/// Outcome of diagnosing an error against the fingerprint store (ADR-0057 reader).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ErrorRecallOutcome {
    /// The layered fingerprint key the diagnosis resolved to.
    pub fingerprint_key: String,
    /// Which layer produced the key (`canonical_code` | `message_template` | …).
    pub layer: &'static str,
    /// The recalled fingerprint when this exact error class was seen before.
    pub matched: Option<StoredErrorFingerprint>,
}

impl ErrorRecallOutcome {
    /// Whether a prior fingerprint for this exact error class exists.
    #[must_use]
    pub fn is_known(&self) -> bool {
        self.matched.is_some()
    }
}

/// Agent-facing recall summary for one diagnostic class (ADR 0057 / bd-uafu0).
/// The current implementation supports exact layered-key recall, plus
/// persisted repair/proof/outcome links for the exact fingerprint. `near`
/// remains empty until graph-backed sibling traversal lands.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorRecallReport {
    pub schema: &'static str,
    pub fingerprint_key: String,
    pub layer: &'static str,
    pub exact: bool,
    pub near: Vec<String>,
    pub helpful_repairs: Vec<String>,
    pub harmful_repairs: Vec<String>,
    pub proof_links: Vec<String>,
    pub stale_version_warnings: Vec<String>,
    pub derived_document: String,
}

impl ErrorRecallReport {
    #[must_use]
    pub fn from_outcome(fingerprint: &ErrorFingerprint, outcome: &ErrorRecallOutcome) -> Self {
        Self {
            schema: "ee.error_recall.report.v1",
            fingerprint_key: outcome.fingerprint_key.clone(),
            layer: outcome.layer,
            exact: outcome.is_known(),
            near: Vec::new(),
            helpful_repairs: Vec::new(),
            harmful_repairs: Vec::new(),
            proof_links: Vec::new(),
            stale_version_warnings: Vec::new(),
            derived_document: fingerprint.derived_document_text(),
        }
    }

    #[must_use]
    pub fn from_outcome_with_links(
        fingerprint: &ErrorFingerprint,
        outcome: &ErrorRecallOutcome,
        links: &[StoredErrorRepairLink],
    ) -> Self {
        let mut report = Self::from_outcome(fingerprint, outcome);
        for link in links {
            match (link.link_kind.as_str(), link.outcome.as_str()) {
                ("repair", "helpful") => report.helpful_repairs.push(link.target_id.clone()),
                ("repair", "harmful") => report.harmful_repairs.push(link.target_id.clone()),
                ("proof", _) => report.proof_links.push(link.target_id.clone()),
                _ => {}
            }
            if let Some(warning) = &link.stale_version_warning {
                report.stale_version_warnings.push(warning.clone());
            }
        }
        report.helpful_repairs.sort();
        report.helpful_repairs.dedup();
        report.harmful_repairs.sort();
        report.harmful_repairs.dedup();
        report.proof_links.sort();
        report.proof_links.dedup();
        report.stale_version_warnings.sort();
        report.stale_version_warnings.dedup();
        report
    }

    #[must_use]
    pub fn query_seed(&self) -> String {
        let recall_status = if self.exact { "known" } else { "unseen" };
        let mut seed = format!(
            "error recall {recall_status} fingerprint:{} layer:{} derived:{}",
            self.fingerprint_key, self.layer, self.derived_document
        );
        for repair in &self.helpful_repairs {
            seed.push_str(" helpful_repair:");
            seed.push_str(repair);
        }
        for repair in &self.harmful_repairs {
            seed.push_str(" harmful_repair:");
            seed.push_str(repair);
        }
        for proof in &self.proof_links {
            seed.push_str(" proof:");
            seed.push_str(proof);
        }
        seed
    }
}

/// Diagnose a canonicalized error against the fingerprint store via exact
/// layered-key recall (ADR-0057 reader). Read-only; performs no tool execution
/// and no durable mutation.
///
/// # Errors
///
/// Propagates any database error from the underlying lookup.
pub fn diagnose_error(
    connection: &DbConnection,
    workspace_id: &str,
    canonical: &CanonicalDiagnostic,
) -> Result<ErrorRecallOutcome> {
    let fingerprint = ErrorFingerprint::from_canonical(canonical);
    let key = fingerprint.layered_key();
    let matched = connection.get_error_fingerprint(workspace_id, &key.key)?;
    Ok(ErrorRecallOutcome {
        fingerprint_key: key.key,
        layer: key.layer.as_str(),
        matched,
    })
}

/// Build the structured recall report for a diagnostic without mutating state.
pub fn error_recall_report(
    connection: &DbConnection,
    workspace_id: &str,
    canonical: &CanonicalDiagnostic,
) -> Result<ErrorRecallReport> {
    let fingerprint = ErrorFingerprint::from_canonical(canonical);
    let outcome = diagnose_error(connection, workspace_id, canonical)?;
    let links = connection.list_error_repair_links(workspace_id, &outcome.fingerprint_key)?;
    Ok(ErrorRecallReport::from_outcome_with_links(
        &fingerprint,
        &outcome,
        &links,
    ))
}

fn stable_error_repair_link_id(
    workspace_id: &str,
    fingerprint_key: &str,
    kind: ErrorRepairLinkKind,
    target_id: &str,
    outcome: &str,
) -> String {
    let hash_input = format!(
        "{workspace_id}\0{fingerprint_key}\0{}\0{target_id}\0{outcome}",
        kind.as_str()
    );
    let hash = blake3::hash(hash_input.as_bytes()).to_hex().to_string();
    format!("erl_{}", &hash[..32])
}

fn push_repair_link(
    links: &mut Vec<StoredErrorRepairLink>,
    workspace_id: &str,
    fingerprint_key: &str,
    kind: ErrorRepairLinkKind,
    target_id: &str,
    outcome: &str,
    stale_version_warning: Option<&str>,
    created_by: Option<&str>,
) {
    let target_id = target_id.trim();
    if target_id.is_empty() {
        return;
    }
    let outcome = outcome.trim();
    let outcome = if outcome.is_empty() {
        "unknown"
    } else {
        outcome
    };
    links.push(StoredErrorRepairLink {
        link_id: stable_error_repair_link_id(
            workspace_id,
            fingerprint_key,
            kind,
            target_id,
            outcome,
        ),
        workspace_id: workspace_id.to_string(),
        fingerprint_key: fingerprint_key.to_string(),
        link_kind: kind.as_str().to_string(),
        target_id: target_id.to_string(),
        outcome: outcome.to_string(),
        evidence_ref: None,
        stale_version_warning: stale_version_warning.map(str::to_string),
        created_by: created_by.map(str::to_string),
        created_at: String::new(),
        updated_at: String::new(),
    });
}

/// Project the library [`ErrorFingerprint`] onto the persistable
/// [`StoredErrorFingerprint`] row. `stderr_simhash` is rendered as fixed-width
/// 32-hex (the V072 CHECK), `version_hints` joined (None when empty).
fn stored_from_fingerprint(
    fingerprint: &ErrorFingerprint,
    workspace_id: &str,
    timestamp: &str,
) -> StoredErrorFingerprint {
    StoredErrorFingerprint {
        fingerprint_key: fingerprint.layered_key().key,
        workspace_id: workspace_id.to_string(),
        tool: fingerprint.tool.as_str().to_string(),
        canonical_code: fingerprint.canonical_code.clone(),
        message_template_signature: fingerprint.message_template_signature.clone(),
        location_shape: fingerprint.location_shape.clone(),
        stderr_simhash: format!("{:032x}", fingerprint.stderr_simhash),
        version_hints: (!fingerprint.version_hints.is_empty())
            .then(|| fingerprint.version_hints.join(",")),
        created_at: timestamp.to_string(),
        updated_at: timestamp.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ErrorRepairLinkRecording, diagnose_error, error_recall_report, record_error_fingerprint,
        record_error_repair_links,
    };
    use crate::core::error_recall::from_rustc;
    use crate::db::{CreateWorkspaceInput, DbConnection};

    const WS: &str = "wsp_01234567890123456789012345";

    fn migrated_db_with_workspace() -> DbConnection {
        let connection = DbConnection::open_memory().expect("open in-memory db");
        connection.migrate().expect("migrate");
        connection
            .insert_workspace(
                WS,
                &CreateWorkspaceInput {
                    path: "/tmp/error-recall-test".to_string(),
                    name: None,
                },
            )
            .expect("insert workspace");
        connection
    }

    #[test]
    fn record_then_diagnose_recalls_the_exact_error_class() {
        let connection = migrated_db_with_workspace();
        let canonical = from_rustc(Some("E0277"), "the trait bound `X: Trait` is not satisfied");

        let stored = record_error_fingerprint(&connection, WS, &canonical).expect("record");
        assert_eq!(stored.fingerprint_key, "rustc:E0277");
        assert_eq!(stored.stderr_simhash.len(), 32); // V072 CHECK

        let outcome = diagnose_error(&connection, WS, &canonical).expect("diagnose");
        assert!(outcome.is_known(), "the recorded error class must recall");
        assert_eq!(outcome.fingerprint_key, "rustc:E0277");
        assert_eq!(outcome.layer, "canonical_code");
    }

    #[test]
    fn unseen_error_class_does_not_recall() {
        let connection = migrated_db_with_workspace();
        record_error_fingerprint(
            &connection,
            WS,
            &from_rustc(Some("E0277"), "trait bound not satisfied"),
        )
        .expect("record");

        let other = from_rustc(Some("E0308"), "mismatched types");
        let outcome = diagnose_error(&connection, WS, &other).expect("diagnose");
        assert!(
            !outcome.is_known(),
            "a different error class must not recall"
        );
        assert_eq!(outcome.fingerprint_key, "rustc:E0308");
    }

    #[test]
    fn record_is_idempotent_for_the_same_error_class() {
        let connection = migrated_db_with_workspace();
        let canonical = from_rustc(Some("E0599"), "no method named `foo` found");
        record_error_fingerprint(&connection, WS, &canonical).expect("record 1");
        // Re-recording the same class upserts (ON CONFLICT), never duplicates.
        record_error_fingerprint(&connection, WS, &canonical).expect("record 2");
        assert!(
            diagnose_error(&connection, WS, &canonical)
                .expect("diagnose")
                .is_known()
        );
    }

    #[test]
    fn report_hydrates_persisted_repair_and_proof_links() {
        let connection = migrated_db_with_workspace();
        let canonical = from_rustc(Some("E0277"), "the trait bound `X: Trait` is not satisfied");

        let links = record_error_repair_links(
            &connection,
            WS,
            &canonical,
            &ErrorRepairLinkRecording {
                helpful_repairs: vec!["mem_helpful".to_string()],
                harmful_repairs: vec!["mem_harmful".to_string()],
                proof_links: vec!["rch_pass_1".to_string()],
                stale_version_warnings: vec!["rustc 1.95 repair may be stale".to_string()],
                created_by: Some("test".to_string()),
            },
        )
        .expect("record links");
        assert_eq!(links.len(), 4);

        let report = error_recall_report(&connection, WS, &canonical).expect("report");
        assert!(report.exact);
        assert_eq!(report.helpful_repairs, vec!["mem_helpful"]);
        assert_eq!(report.harmful_repairs, vec!["mem_harmful"]);
        assert_eq!(report.proof_links, vec!["rch_pass_1"]);
        assert_eq!(
            report.stale_version_warnings,
            vec!["rustc 1.95 repair may be stale"]
        );
        assert!(report.query_seed().contains("helpful_repair:mem_helpful"));
        assert!(report.query_seed().contains("proof:rch_pass_1"));
    }
}
