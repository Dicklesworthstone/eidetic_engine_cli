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

use crate::core::error_recall::{CanonicalDiagnostic, ErrorFingerprint};
use crate::db::{DbConnection, Result, StoredErrorFingerprint};

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
    use super::{diagnose_error, record_error_fingerprint};
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
}
