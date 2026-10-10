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

use std::collections::BTreeSet;

use chrono::Utc;

use crate::core::error_recall::{CanonicalDiagnostic, ErrorFingerprint, ErrorRepairLinkKind};
use crate::db::{
    CreateErrorRepairLinkInput, DbConnection, Result, StoredErrorFingerprint, StoredErrorRepairLink,
};

#[path = "error_recall_near.rs"]
mod near;

#[path = "error_recall_evidence.rs"]
mod evidence;

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
    /// The observation these links were derived from (for example the
    /// failing evidence span of an imported session), kept for provenance.
    pub evidence_ref: Option<String>,
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

/// Persist repair/proof/outcome links for a canonicalized diagnostic and
/// return the complete stored history for that class. Importers that only
/// need write acknowledgement use the receipt-only path below instead.
/// Link IDs remain deterministic over workspace, fingerprint, kind, target,
/// and outcome, making repeat observations idempotent.
pub fn record_error_repair_links(
    connection: &DbConnection,
    workspace_id: &str,
    canonical: &CanonicalDiagnostic,
    recording: &ErrorRepairLinkRecording,
) -> Result<Vec<StoredErrorRepairLink>> {
    let receipt = persist_error_repair_links(connection, workspace_id, canonical, recording)?;
    connection.list_error_repair_links(workspace_id, &receipt.fingerprint_key)
}

/// Acknowledgement of this batch, independent of prior repair history.
/// Submission counts are not insert counts: an idempotent retry submits the
/// same distinct links without creating new rows. The caller's transaction
/// still owns commit/rollback; receiving a receipt does not commit it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ErrorRepairWriteReceipt {
    pub fingerprint_key: String,
    pub links_submitted: usize,
}

/// Persist only this observation; never reload the fingerprint's history.
/// Repeated occurrences of a common compiler error must not read all earlier
/// sessions' repair/proof links after every upsert. Fingerprint refresh, link
/// normalization, deterministic IDs and storage errors are identical to the
/// complete-history API. Atomic import remains owned by the caller.
pub(crate) fn persist_error_repair_links(
    connection: &DbConnection,
    workspace_id: &str,
    canonical: &CanonicalDiagnostic,
    recording: &ErrorRepairLinkRecording,
) -> Result<ErrorRepairWriteReceipt> {
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
            recording.evidence_ref.as_deref(),
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
            recording.evidence_ref.as_deref(),
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
            recording.evidence_ref.as_deref(),
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
            recording.evidence_ref.as_deref(),
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
    Ok(ErrorRepairWriteReceipt {
        fingerprint_key: stored.fingerprint_key,
        links_submitted: inputs.len(),
    })
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
/// Repair/proof/outcome links describe the exact fingerprint only. On an
/// informative code-less exact miss, `near` lists at most five same-tool,
/// same-workspace signature neighbors, ordered by distance then key. These
/// advisory neighbors never become exact matches or verified repair evidence.
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
        for warning in &self.stale_version_warnings {
            seed.push_str(" stale_version_warning:");
            seed.push_str(warning);
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

/// Build the complete structured recall report without mutating state.
/// Code-less exact misses with sufficient diagnostic content scan stored
/// workspace fingerprint metadata for advisory near matches. That scan is
/// linear in fingerprint history; interactive packing deliberately uses
/// [`pack_error_recall_query_seed`] instead and does not pay for it.
pub fn error_recall_report(
    connection: &DbConnection,
    workspace_id: &str,
    canonical: &CanonicalDiagnostic,
) -> Result<ErrorRecallReport> {
    let fingerprint = ErrorFingerprint::from_canonical(canonical);
    let outcome = diagnose_error(connection, workspace_id, canonical)?;
    let links = connection.list_error_repair_links(workspace_id, &outcome.fingerprint_key)?;
    let mut report = ErrorRecallReport::from_outcome_with_links(&fingerprint, &outcome, &links);
    near::populate(connection, workspace_id, canonical, &fingerprint, &mut report)?;
    Ok(report)
}

/// Maximum linked targets considered by one interactive error-recall query.
const PACK_RECALL_TARGET_LIMIT: u32 = 32;
/// Maximum admitted repair excerpts included in the retrieval query.
const PACK_RECALL_EXCERPT_LIMIT: usize = 4;
const PACK_RECALL_EXCERPT_BYTES: usize = 768;
const PACK_RECALL_METADATA_BYTES: usize = 512;
/// The expansion is bounded before retrieval and its separate token budget.
const PACK_RECALL_QUERY_BYTES: usize = 4096;

/// Build the bounded error-recall expansion used by `ee pack --error-log`.
///
/// A common fingerprint can have repairs from thousands of imported sessions.
/// Interactive packing reads at most 32 helpful targets through the existing
/// fingerprint index, then hydrates only that window. Within it, admitted
/// incident cards precede raw repair turns, with canonical id order breaking
/// ties. This is a deterministic bounded sample, not a history-wide recency
/// ranking. The complete diagnostic report remains available separately via
/// [`error_recall_report`].
///
/// Evidence text crosses the normal live direct-pack admission boundary.
/// Canonical memory ids remain query hints without granting their target any
/// additional admission authority. Proof locators and arbitrary link metadata
/// do not become retrieval text. The final expansion is at most 4096 UTF-8
/// bytes, including at most four distinct repair excerpts.
///
/// # Errors
///
/// Propagates database errors from fingerprint, target, and evidence reads.
pub fn pack_error_recall_query_seed(
    connection: &DbConnection,
    workspace_id: &str,
    canonical: &CanonicalDiagnostic,
) -> Result<String> {
    let fingerprint = ErrorFingerprint::from_canonical(canonical);
    let outcome = diagnose_error(connection, workspace_id, canonical)?;
    let metadata = ErrorRecallReport::from_outcome(&fingerprint, &outcome).query_seed();
    let mut seed = bounded_pack_recall_text(&metadata, PACK_RECALL_METADATA_BYTES);
    let targets = connection.list_helpful_error_repair_target_ids(
        workspace_id,
        &outcome.fingerprint_key,
        PACK_RECALL_TARGET_LIMIT,
    )?;
    let mut evidence_ids = Vec::new();
    let mut memory_ids = Vec::new();
    for target in &targets {
        if target
            .parse::<crate::models::EvidenceId>()
            .is_ok_and(|id| id.to_string() == *target)
        {
            evidence_ids.push(target.as_str());
        } else if target
            .parse::<crate::models::MemoryId>()
            .is_ok_and(|id| id.to_string() == *target)
        {
            memory_ids.push(target.as_str());
        }
    }
    let mut evidence = connection.get_evidence_spans_with_sessions(&evidence_ids)?;
    evidence.retain(|row| row.is_direct_pack_admitted(workspace_id));
    evidence.sort_by(|left, right| {
        right
            .span
            .is_derived_incident_card()
            .cmp(&left.span.is_derived_incident_card())
            .then_with(|| left.span.id.cmp(&right.span.id))
    });
    let mut excerpts = BTreeSet::new();
    for row in evidence {
        if excerpts.len() == PACK_RECALL_EXCERPT_LIMIT {
            break;
        }
        let text = row.span.reader_text();
        let text = bounded_pack_recall_text(text.trim(), PACK_RECALL_EXCERPT_BYTES);
        if text.is_empty() || !excerpts.insert(text.clone()) {
            continue;
        }
        append_pack_recall_fragment(&mut seed, " helpful_repair:", &row.span.id);
        append_pack_recall_fragment(&mut seed, "\nprior fix: ", &text);
    }
    for memory_id in memory_ids.into_iter().take(PACK_RECALL_EXCERPT_LIMIT) {
        append_pack_recall_fragment(&mut seed, " helpful_repair:", memory_id);
    }
    Ok(seed)
}

fn utf8_prefix(text: &str, max_bytes: usize) -> &str {
    let mut end = text.len().min(max_bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn bounded_pack_recall_text(text: &str, max_bytes: usize) -> String {
    const MARKER: &str = " [truncated]";
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let Some(content_bytes) = max_bytes.checked_sub(MARKER.len()) else {
        return String::new();
    };
    let mut bounded = utf8_prefix(text, content_bytes).trim_end().to_owned();
    bounded.push_str(MARKER);
    bounded
}

fn append_pack_recall_fragment(seed: &mut String, prefix: &str, text: &str) {
    let remaining = PACK_RECALL_QUERY_BYTES.saturating_sub(seed.len());
    // Metadata and excerpts are already bounded and visibly marked above.
    // Append whole fragments so an id hint can never become another id.
    if !text.is_empty() && prefix.len().saturating_add(text.len()) <= remaining {
        seed.push_str(prefix);
        seed.push_str(text);
    }
}

/// Imported transcript evidence behind a recall report
/// (bd-reality-core-convergence-1azkt.60): the admitted turns that repaired
/// this error class in earlier sessions, and the spans that verified them.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecalledRepairEvidence {
    pub evidence_id: String,
    /// `incident_card` for a derived failure->fix summary (ADR 0091),
    /// `repair` for a fix turn, `proof` for the run that verified it.
    pub role: &'static str,
    pub provenance_uri: String,
    /// Projected text of a retrieval-admitted repair turn. Proof spans are
    /// tool output, quarantined from retrieval, so only their locator is
    /// shown.
    pub text: Option<String>,
}

/// Longest repair excerpt surfaced by recall, in characters.
const RECALLED_REPAIR_TEXT_CHARS: usize = 400;

/// Resolve imported-evidence targets against live storage in deduplicated
/// 128-ID batches. Repairs cross normal search admission; proof locators
/// require a live session in the same workspace, without exposing proof text.
/// Targets that are not canonical evidence ids remain with their own readers.
/// The complete report is preserved rather than cut to the pack limit.
///
/// # Errors
///
/// Propagates database errors from the evidence lookups.
pub fn recalled_repair_evidence(
    connection: &DbConnection,
    workspace_id: &str,
    report: &ErrorRecallReport,
) -> Result<Vec<RecalledRepairEvidence>> {
    evidence::read(connection, workspace_id, report)
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
    evidence_ref: Option<&str>,
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
        evidence_ref: evidence_ref
            .map(str::trim)
            .filter(|evidence_ref| !evidence_ref.is_empty())
            .map(str::to_string),
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
        ErrorRepairLinkRecording, PACK_RECALL_EXCERPT_LIMIT, PACK_RECALL_QUERY_BYTES,
        PACK_RECALL_TARGET_LIMIT, diagnose_error, error_recall_report,
        pack_error_recall_query_seed, persist_error_repair_links, record_error_fingerprint,
        record_error_repair_links,
    };
    use crate::core::error_recall::from_rustc;
    use crate::db::{
        CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, DbConnection,
        EvidenceProducerKind,
    };

    const WS: &str = "wsp_01234567890123456789012345";
    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

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
                evidence_ref: None,
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
        assert!(
            report
                .query_seed()
                .contains("stale_version_warning:rustc 1.95 repair may be stale")
        );
    }

    fn recall_evidence_id(number: u32) -> String {
        crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(u128::from(number))).to_string()
    }

    fn recall_session(
        connection: &DbConnection,
        workspace_id: &str,
        seed: u128,
    ) -> TestResult<String> {
        let id = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(seed)).to_string();
        connection.insert_session(
            &id,
            &CreateSessionInput {
                workspace_id: workspace_id.to_owned(),
                cass_session_id: format!("/sessions/bounded-recall-{seed}.jsonl"),
                source_path: None,
                agent_name: Some("claude_code".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 0,
                token_count: None,
                content_hash: format!("blake3:{}", blake3::hash(&seed.to_le_bytes()).to_hex()),
                metadata_json: None,
            },
        )?;
        Ok(id)
    }

    fn recall_span(
        connection: &DbConnection,
        workspace_id: &str,
        session_id: &str,
        number: u32,
        text: &str,
        is_card: bool,
    ) -> TestResult<String> {
        let id = recall_evidence_id(number);
        let excerpt = if is_card {
            text.to_owned()
        } else {
            serde_json::json!({"type":"assistant","message":{"role":"assistant","content":[
                {"type":"text","text":text}
            ]}})
            .to_string()
        };
        connection.insert_evidence_span(
            &id,
            &CreateEvidenceSpanInput {
                workspace_id: workspace_id.to_owned(),
                session_id: session_id.to_owned(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("{session_id}:{number}"),
                span_kind: if is_card { "summary" } else { "message" }.to_owned(),
                start_line: number,
                end_line: number,
                start_byte: None,
                end_byte: None,
                role: (!is_card).then(|| "assistant".to_owned()),
                content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
                excerpt,
                metadata_json: None,
                inherited_redaction_classes: Vec::new(),
            },
        )?;
        Ok(id)
    }

    #[test]
    fn pack_recall_bounds_link_history_and_utf8_text_without_truncating_diagnose() -> TestResult {
        let connection = migrated_db_with_workspace();
        let canonical = from_rustc(Some("E0277"), "trait bound is not satisfied");
        let session = recall_session(&connection, WS, 901)?;
        for number in 1..=8 {
            let is_card = matches!(number, 6 | 8);
            let text = if is_card {
                format!(
                    "{}1-5): `cargo test` failed, then passed after a fix.\nSymptom: trait bound not satisfied.\nFix: Card repair {number} updated the cache trait implementation and verified the release test.\nVerified: `cargo test` succeeded afterwards.",
                    crate::core::incident_card::INCIDENT_CARD_PREFIX
                )
            } else {
                format!(
                    "Unicode repair {number} updated the cache configuration: {}",
                    "🦀".repeat(800)
                )
            };
            recall_span(&connection, WS, &session, number, &text, is_card)?;
        }
        let mut targets = (1..=96).map(recall_evidence_id).collect::<Vec<_>>();
        // An arbitrary oversized target must not become query prose. It sorts
        // first, so this also verifies bounded target projection before Rust
        // rejects it as an invalid typed id.
        let untrusted_target = format!("!UNTRUSTED_LINK_TEXT {}", "x".repeat(16_384));
        targets.push(untrusted_target.clone());
        record_error_repair_links(
            &connection,
            WS,
            &canonical,
            &ErrorRepairLinkRecording {
                helpful_repairs: targets,
                harmful_repairs: vec!["UNTRUSTED_HARMFUL_TEXT".to_owned()],
                proof_links: (1000..1096).map(recall_evidence_id).collect(),
                stale_version_warnings: vec!["UNTRUSTED_WARNING_TEXT".repeat(512)],
                ..ErrorRepairLinkRecording::default()
            },
        )?;

        let targets = connection.list_helpful_error_repair_target_ids(
            WS,
            "rustc:E0277",
            PACK_RECALL_TARGET_LIMIT,
        )?;
        assert_eq!(targets.len(), usize::try_from(PACK_RECALL_TARGET_LIMIT)?);
        assert!(targets[0].is_empty());
        assert!(targets.contains(&recall_evidence_id(31)));
        assert!(!targets.contains(&recall_evidence_id(32)));
        assert!(
            connection
                .list_helpful_error_repair_target_ids(WS, "rustc:E0277", 0)?
                .is_empty()
        );

        let seed = pack_error_recall_query_seed(&connection, WS, &canonical)?;
        assert_eq!(
            seed,
            pack_error_recall_query_seed(&connection, WS, &canonical)?
        );
        assert!(seed.starts_with("error recall known fingerprint:rustc:E0277"));
        assert!(
            seed.len() <= PACK_RECALL_QUERY_BYTES,
            "{} bytes",
            seed.len()
        );
        assert_eq!(
            seed.matches("\nprior fix: ").count(),
            PACK_RECALL_EXCERPT_LIMIT
        );
        let card_position = seed.find("Card repair 6").ok_or("first card missing")?;
        let next_card_position = seed.find("Card repair 8").ok_or("second card missing")?;
        let turn_position = seed.find("Unicode repair 1").ok_or("repair turn missing")?;
        assert!(card_position < next_card_position && next_card_position < turn_position);
        assert!(seed.contains("Unicode repair 2"));
        assert!(!seed.contains("Unicode repair 3"));
        assert!(seed.contains('🦀'));
        assert!(
            !seed.contains(&"🦀".repeat(200)),
            "excerpt bytes were not bounded"
        );
        assert_eq!(seed.matches(" [truncated]").count(), 2);
        for forbidden in [
            "UNTRUSTED_LINK_TEXT",
            "UNTRUSTED_HARMFUL_TEXT",
            "UNTRUSTED_WARNING_TEXT",
            &recall_evidence_id(96),
            &recall_evidence_id(1000),
        ] {
            assert!(
                !seed.contains(forbidden),
                "unbounded metadata entered the seed"
            );
        }

        // The interactive bound must not weaken the complete diagnostic API.
        let report = error_recall_report(&connection, WS, &canonical)?;
        assert_eq!(report.helpful_repairs.len(), 97);
        assert!(report.helpful_repairs.contains(&untrusted_target));
        assert_eq!(report.harmful_repairs, ["UNTRUSTED_HARMFUL_TEXT"]);
        assert_eq!(report.proof_links.len(), 96);
        assert_eq!(report.stale_version_warnings.len(), 1);
        let oversized_code = crate::core::error_recall::from_ee_error(
            &"unusually_long_code".repeat(100),
            "Code supplied by a direct library caller.",
        );
        let metadata = pack_error_recall_query_seed(&connection, WS, &oversized_code)?;
        assert!(metadata.len() <= super::PACK_RECALL_METADATA_BYTES);
        assert!(metadata.ends_with(" [truncated]"));
        let mut almost_full = "x".repeat(PACK_RECALL_QUERY_BYTES - 8);
        let before = almost_full.clone();
        super::append_pack_recall_fragment(
            &mut almost_full,
            " helpful_repair:",
            &recall_evidence_id(1),
        );
        assert_eq!(
            almost_full, before,
            "canonical ids must never be sliced to fit"
        );
        Ok(())
    }

    #[test]
    fn pack_recall_hydrates_only_live_admitted_repairs_and_preserves_typed_memory_hints()
    -> TestResult {
        let connection = migrated_db_with_workspace();
        let canonical = from_rustc(Some("E0308"), "mismatched types");
        let session = recall_session(&connection, WS, 902)?;
        let live_text = format!(
            "LIVE_REPAIR_TEXT adjusted the return type and verified the compiler result. {}",
            "🦀".repeat(800)
        );
        let admitted = recall_span(&connection, WS, &session, 1, &live_text, false)?;
        let poisoned = recall_span(
            &connection,
            WS,
            &session,
            2,
            "POISONED_REPAIR_TEXT originally described an admitted compiler fix.",
            false,
        )?;
        connection.execute_raw(&format!(
            "UPDATE evidence_spans SET excerpt = 'UNSCREENED_REPLACEMENT_TEXT' WHERE id = '{poisoned}'"
        ))?;
        let denied = recall_span(
            &connection,
            WS,
            &session,
            3,
            "PACK_DENIED_TEXT describes a repair that policy excludes from packs.",
            false,
        )?;
        connection.execute_raw(&format!(
            "UPDATE evidence_spans SET pack_eligibility = 'denied' WHERE id = '{denied}'"
        ))?;
        const OTHER_WS: &str = "wsp_01234567890123456789012346";
        connection.insert_workspace(
            OTHER_WS,
            &CreateWorkspaceInput {
                path: "/tmp/other-error-recall-test".to_owned(),
                name: None,
            },
        )?;
        let other_session = recall_session(&connection, OTHER_WS, 903)?;
        let foreign = recall_span(
            &connection,
            OTHER_WS,
            &other_session,
            4,
            "FOREIGN_REPAIR_TEXT belongs to a separate workspace.",
            false,
        )?;
        let duplicate = recall_span(&connection, WS, &session, 5, &live_text, false)?;
        let unlinked = recall_span(
            &connection,
            WS,
            &session,
            6,
            "NUL_ALIASED_REPAIR_TEXT must not be reached through a different target id.",
            false,
        )?;
        let nul_alias = format!("{unlinked}\0suffix");
        let manual_memory =
            crate::models::MemoryId::from_uuid(uuid::Uuid::from_u128(904)).to_string();
        record_error_repair_links(
            &connection,
            WS,
            &canonical,
            &ErrorRepairLinkRecording {
                helpful_repairs: vec![
                    admitted.clone(),
                    poisoned.clone(),
                    denied.clone(),
                    foreign.clone(),
                    duplicate,
                    nul_alias.clone(),
                    manual_memory.clone(),
                    "mem_NOT_AN_ID arbitrary instructions".to_owned(),
                ],
                ..ErrorRepairLinkRecording::default()
            },
        )?;
        let seed = pack_error_recall_query_seed(&connection, WS, &canonical)?;
        assert!(seed.contains("LIVE_REPAIR_TEXT"));
        assert!(seed.contains(&format!("helpful_repair:{admitted}")));
        assert!(seed.contains(&format!("helpful_repair:{manual_memory}")));
        assert_eq!(
            seed.matches("\nprior fix: ").count(),
            1,
            "duplicate text was repeated"
        );
        assert_eq!(seed.matches(" [truncated]").count(), 1);
        assert!(
            connection
                .list_helpful_error_repair_target_ids(WS, "rustc:E0308", PACK_RECALL_TARGET_LIMIT)?
                .contains(&nul_alias),
            "bounded target projection must preserve embedded NUL for strict parsing"
        );
        for forbidden in [
            "POISONED_REPAIR_TEXT",
            "UNSCREENED_REPLACEMENT_TEXT",
            "PACK_DENIED_TEXT",
            "FOREIGN_REPAIR_TEXT",
            "arbitrary instructions",
            "NUL_ALIASED_REPAIR_TEXT",
            poisoned.as_str(),
            denied.as_str(),
            foreign.as_str(),
            unlinked.as_str(),
        ] {
            assert!(
                !seed.contains(forbidden),
                "unadmitted target entered the seed: {forbidden}"
            );
        }
        let report = error_recall_report(&connection, WS, &canonical)?;
        assert_eq!(report.helpful_repairs.len(), 8);
        Ok(())
    }

    #[test]
    fn write_receipt_counts_this_batch_without_truncating_the_public_history() -> TestResult {
        let connection = migrated_db_with_workspace();
        let canonical = from_rustc(Some("E0308"), "mismatched types");
        let previous = ErrorRepairLinkRecording {
            helpful_repairs: (0..256).map(|number| format!("mem_prior_{number:04}")).collect(),
            evidence_ref: Some("ev_prior_observation".to_owned()),
            ..ErrorRepairLinkRecording::default()
        };
        let seeded = persist_error_repair_links(&connection, WS, &canonical, &previous)?;
        assert_eq!(seeded.links_submitted, 256);
        let observation = ErrorRepairLinkRecording {
            helpful_repairs: vec![" mem_new ".to_owned(), "mem_new".to_owned(), " ".to_owned()],
            harmful_repairs: vec!["mem_new".to_owned()],
            proof_links: vec!["proof_new".to_owned(), "proof_new".to_owned()],
            stale_version_warnings: vec!["version warning".to_owned()],
            evidence_ref: Some("ev_new_observation".to_owned()),
            created_by: Some("receipt-test".to_owned()),
        };
        for _ in 0..2 {
            let receipt = persist_error_repair_links(&connection, WS, &canonical, &observation)?;
            assert_eq!(receipt.fingerprint_key, "rustc:E0308");
            assert_eq!(receipt.links_submitted, 4);
        }
        let complete = record_error_repair_links(&connection, WS, &canonical, &observation)?;
        assert_eq!(complete.len(), 260);
        assert_eq!(
            complete.iter().filter(|link| link.evidence_ref.as_deref() == Some("ev_prior_observation")).count(),
            256
        );
        assert_eq!(
            complete.iter().filter(|link| link.evidence_ref.as_deref() == Some("ev_new_observation")).count(),
            4
        );
        let report = error_recall_report(&connection, WS, &canonical)?;
        assert_eq!(report.helpful_repairs.len(), 257);
        assert_eq!(report.harmful_repairs, ["mem_new"]);
        assert_eq!(report.proof_links, ["proof_new"]);
        Ok(())
    }

    #[test]
    fn empty_write_receipt_still_records_the_failure_class() -> TestResult {
        let connection = migrated_db_with_workspace();
        let canonical = from_rustc(Some("E0599"), "no method named missing");
        let receipt = persist_error_repair_links(
            &connection,
            WS,
            &canonical,
            &ErrorRepairLinkRecording::default(),
        )?;
        assert_eq!(receipt.links_submitted, 0);
        assert!(diagnose_error(&connection, WS, &canonical)?.is_known());
        assert!(connection.list_error_repair_links(WS, &receipt.fingerprint_key)?.is_empty());
        Ok(())
    }

    #[test]
    fn write_receipt_does_not_commit_its_callers_transaction() -> TestResult {
        let connection = migrated_db_with_workspace();
        let canonical = from_rustc(Some("E0277"), "missing trait implementation");
        let recording = ErrorRepairLinkRecording {
            helpful_repairs: vec!["mem_uncommitted".to_owned()],
            ..ErrorRepairLinkRecording::default()
        };
        let result: crate::db::Result<()> = connection.with_transaction(|| {
            let receipt = persist_error_repair_links(&connection, WS, &canonical, &recording)?;
            assert_eq!(receipt.links_submitted, 1);
            // An actual storage error must roll back both the fingerprint and
            // the links, even though the inner writer already acknowledged them.
            connection.execute_raw("INSERT INTO ee_missing_receipt_test_table VALUES (1)")?;
            Ok(())
        });
        assert!(result.is_err());
        assert!(!diagnose_error(&connection, WS, &canonical)?.is_known());
        assert!(connection.list_error_repair_links(WS, "rustc:E0277")?.is_empty());
        Ok(())
    }
}
