//! Advisory near-error recall for the complete diagnostic report.
//!
//! Exact lookup and interactive pack expansion do not use this scan. On a
//! code-less exact miss, the complete report scans the workspace's persisted
//! signatures (not logs) and retains at most five neighbors. This is O(N)
//! metadata I/O, not an indexed neighborhood lookup. Similarity is not proof
//! that a repair applies: exact status, repair/proof lists and pack seeds stay
//! unchanged. A future indexed reader can feed the same selector.

use std::collections::BTreeSet;

use crate::core::error_recall::{
    CanonicalDiagnostic, ErrorFingerprint, SIMHASH_TAIL_MAX_DISTANCE, simhash_hamming_distance,
};
use crate::db::{DbConnection, Result, StoredErrorFingerprint};

use super::ErrorRecallReport;

const NEAR_MATCH_LIMIT: usize = 5;

pub(super) fn populate(
    connection: &DbConnection,
    workspace_id: &str,
    canonical: &CanonicalDiagnostic,
    fingerprint: &ErrorFingerprint,
    report: &mut ErrorRecallReport,
) -> Result<()> {
    // Never replace an exact class, or compare distinct structured codes by
    // their often almost identical diagnostic wording.
    if report.exact || !eligible(canonical) || degenerate(fingerprint.stderr_simhash) {
        return Ok(());
    }
    // This existing reader returns only fingerprint metadata. Keep the scan
    // on the complete diagnostic surface, never the bounded pack hot path.
    let candidates = connection.list_error_fingerprints_for_recovery(workspace_id)?;
    report.near = select(fingerprint, workspace_id, candidates);
    Ok(())
}

fn eligible(canonical: &CanonicalDiagnostic) -> bool {
    if canonical.canonical_code.is_some() {
        return false;
    }
    let mut words = BTreeSet::new();
    for token in canonical.message_template.split_whitespace() {
        if token.starts_with("exit_") || token.contains('<') || token.contains('>') {
            continue;
        }
        let word = token.trim_matches(|ch: char| !ch.is_alphanumeric());
        if word.len() < 3
            || !word.chars().any(char::is_alphabetic)
            || matches!(
                word,
                "the"
                    | "and"
                    | "was"
                    | "with"
                    | "error"
                    | "failed"
                    | "failure"
                    | "during"
                    | "after"
                    | "before"
            )
        {
            continue;
        }
        words.insert(word);
        if words.len() == 4 {
            return true;
        }
    }
    false
}

fn degenerate(hash: u128) -> bool {
    hash == 0 || hash == u128::MAX
}

fn hex_field(text: &str, digits: usize) -> bool {
    text.len() == digits && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn candidate_distance(
    query: &ErrorFingerprint,
    workspace_id: &str,
    exact_key: &str,
    candidate: &StoredErrorFingerprint,
) -> Option<u32> {
    if candidate.workspace_id != workspace_id
        || candidate.tool != query.tool.as_str()
        || candidate.canonical_code.is_some()
        || candidate.fingerprint_key == exact_key
    {
        return None;
    }
    let signature = candidate.message_template_signature.strip_prefix("blake3:")?;
    if !hex_field(signature, 64)
        || candidate.fingerprint_key
            != format!("{}:tmpl:{}", candidate.tool, candidate.message_template_signature)
        || !hex_field(&candidate.stderr_simhash, 32)
    {
        return None;
    }
    let hash = u128::from_str_radix(&candidate.stderr_simhash, 16).ok()?;
    if degenerate(hash) {
        return None;
    }
    let distance = simhash_hamming_distance(query.stderr_simhash, hash);
    (distance <= SIMHASH_TAIL_MAX_DISTANCE).then_some(distance)
}

fn select(
    query: &ErrorFingerprint,
    workspace_id: &str,
    candidates: impl IntoIterator<Item = StoredErrorFingerprint>,
) -> Vec<String> {
    if query.canonical_code.is_some() || degenerate(query.stderr_simhash) {
        return Vec::new();
    }
    let exact_key = query.layered_key().key;
    let mut nearest: Vec<(u32, String)> = Vec::with_capacity(NEAR_MATCH_LIMIT + 1);
    for candidate in candidates {
        let Some(distance) = candidate_distance(query, workspace_id, &exact_key, &candidate) else {
            continue;
        };
        // DB keys are unique, but keep selection safe for other future readers
        // and independent of duplicate/input iteration order.
        if let Some(position) = nearest
            .iter()
            .position(|(_, key)| key == &candidate.fingerprint_key)
        {
            if nearest[position].0 <= distance {
                continue;
            }
            nearest.remove(position);
        }
        nearest.push((distance, candidate.fingerprint_key));
        nearest.sort();
        nearest.truncate(NEAR_MATCH_LIMIT);
    }
    nearest.into_iter().map(|(_, key)| key).collect()
}

#[cfg(test)]
mod tests {
    use super::super::{
        ErrorRepairLinkRecording, error_recall_report, pack_error_recall_query_seed,
        record_error_fingerprint, record_error_repair_links, stored_from_fingerprint,
    };
    use super::*;
    use crate::core::error_recall::{DiagnosticTool, from_cargo, from_rustc, from_shell};
    use crate::db::CreateWorkspaceInput;

    const WS: &str = "wsp_01234567890123456789012345";
    const OTHER_WS: &str = "wsp_01234567890123456789012346";
    const HASH: u128 = 0x1234_5678_9abc_def0_fedc_ba98_7654_3210;
    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn query() -> ErrorFingerprint {
        let mut query = ErrorFingerprint::from_canonical(&from_cargo(
            None,
            "connection refused while opening deployment transport socket",
        ));
        query.stderr_simhash = HASH;
        query
    }

    fn row(number: u32, hash: u128) -> StoredErrorFingerprint {
        let mut fingerprint = query();
        fingerprint.message_template_signature = format!("blake3:{number:064x}");
        fingerprint.stderr_simhash = hash;
        stored_from_fingerprint(&fingerprint, WS, "2026-10-10T00:00:00Z")
    }

    #[test]
    fn distance_boundary_and_nearest_order_are_deterministic() {
        let rows = vec![
            row(4, HASH ^ 0x3f),
            row(3, HASH ^ 0x7f),
            row(2, HASH ^ 1),
            row(1, HASH ^ 1),
        ];
        let expected = vec![
            row(1, HASH).fingerprint_key,
            row(2, HASH).fingerprint_key,
            row(4, HASH).fingerprint_key,
        ];
        assert_eq!(select(&query(), WS, rows.clone()), expected);
        assert_eq!(select(&query(), WS, rows.into_iter().rev()), expected);
    }

    #[test]
    fn selection_caps_results_without_cutting_off_later_better_matches() {
        let rows: Vec<_> = (1..=100)
            .rev()
            .map(|number| row(number, HASH ^ 1))
            .collect();
        let expected: Vec<_> = (1..=5)
            .map(|number| row(number, HASH).fingerprint_key)
            .collect();
        assert_eq!(select(&query(), WS, rows), expected);
        let repeated = (0..20).map(|_| row(1, HASH));
        assert_eq!(select(&query(), WS, repeated).len(), 1);
    }

    #[test]
    fn foreign_tools_workspaces_codes_and_exact_keys_are_not_near_matches() {
        let mut foreign_workspace = row(1, HASH);
        foreign_workspace.workspace_id = OTHER_WS.to_owned();
        let mut foreign_tool = row(2, HASH);
        foreign_tool.tool = "rustc".to_owned();
        let mut coded = row(3, HASH);
        coded.canonical_code = Some("E0277".to_owned());
        let exact = stored_from_fingerprint(&query(), WS, "2026-10-10T00:00:00Z");
        assert!(
            select(&query(), WS, [foreign_workspace, foreign_tool, coded, exact]).is_empty()
        );
        let mut coded_query = query();
        coded_query.canonical_code = Some("E0308".to_owned());
        assert!(select(&coded_query, WS, [row(4, HASH)]).is_empty());
    }

    #[test]
    fn malformed_signatures_keys_and_simhashes_are_rejected_without_panicking() {
        for invalid in [
            "",
            "0",
            "+123456789abcdef0123456789abcdef0",
            "éééééééééééééééé",
            "z123456789abcdef0123456789abcdef0",
        ] {
            let mut candidate = row(1, HASH);
            candidate.stderr_simhash = invalid.to_owned();
            assert!(select(&query(), WS, [candidate]).is_empty());
        }
        let mut signature = row(1, HASH);
        signature.message_template_signature = "blake3:invalid".to_owned();
        let mut key = row(2, HASH);
        key.fingerprint_key.push_str("\0different");
        assert!(
            select(&query(), WS, [signature, key, row(3, 0), row(4, u128::MAX)]).is_empty()
        );
    }

    #[test]
    fn low_information_diagnostics_do_not_enter_the_similarity_scan() {
        for text in [
            "",
            "failed",
            "error failed failure",
            "<path> <id> <num> <hex>",
            "socket socket socket socket",
        ] {
            assert!(!eligible(&from_cargo(None, text)));
        }
        assert!(!eligible(&from_shell(1, "")));
        assert!(!eligible(&from_rustc(
            Some("E0277"),
            "connection refused opening deployment transport socket"
        )));
        assert!(eligible(&from_cargo(
            None,
            "connection refused opening deployment transport socket"
        )));
    }

    fn db() -> std::result::Result<DbConnection, Box<dyn std::error::Error>> {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        for (id, path) in [
            (WS, "/tmp/near-error-recall"),
            (OTHER_WS, "/tmp/other-near-error-recall"),
        ] {
            connection.insert_workspace(
                id,
                &CreateWorkspaceInput {
                    path: path.to_owned(),
                    name: None,
                },
            )?;
        }
        Ok(connection)
    }

    fn messages() -> (CanonicalDiagnostic, CanonicalDiagnostic) {
        (
            from_cargo(None, "connection refused while opening deployment transport socket"),
            from_cargo(None, "deployment transport socket opening while refused connection"),
        )
    }

    #[test]
    fn real_stored_signatures_recall_near_without_promoting_repairs_or_pack_hints() -> TestResult {
        let connection = db()?;
        let (previous, current) = messages();
        assert_ne!(previous.layered_key(), current.layered_key());
        // The shared SimHash is token-multiset based; reordered diagnostics
        // have equal signatures but different exact template commitments.
        assert_eq!(previous.simhash_tail(), current.simhash_tail());
        record_error_repair_links(
            &connection,
            WS,
            &previous,
            &ErrorRepairLinkRecording {
                helpful_repairs: vec!["mem_NEAR_REPAIR_MUST_NOT_BE_PROMOTED".to_owned()],
                proof_links: vec!["proof_NEAR_IS_NOT_CURRENT_PROOF".to_owned()],
                ..ErrorRepairLinkRecording::default()
            },
        )?;
        let before = connection.list_error_fingerprints_for_recovery(WS)?.len();
        let report = error_recall_report(&connection, WS, &current)?;
        assert!(!report.exact);
        assert_eq!(report.layer, "message_template");
        assert_eq!(report.near, [previous.layered_key().key]);
        assert!(report.helpful_repairs.is_empty());
        assert!(report.proof_links.is_empty());
        assert!(!report.query_seed().contains("NEAR_REPAIR"));
        let seed = pack_error_recall_query_seed(&connection, WS, &current)?;
        assert!(!seed.contains("helpful_repair:"));
        assert_eq!(connection.list_error_fingerprints_for_recovery(WS)?.len(), before);
        record_error_fingerprint(&connection, WS, &current)?;
        let exact = error_recall_report(&connection, WS, &current)?;
        assert!(exact.exact);
        assert!(exact.near.is_empty());
        Ok(())
    }

    #[test]
    fn complete_reports_never_mix_workspaces_or_structured_error_codes() -> TestResult {
        let connection = db()?;
        let (previous, current) = messages();
        record_error_fingerprint(&connection, OTHER_WS, &previous)?;
        assert!(error_recall_report(&connection, WS, &current)?.near.is_empty());
        let mut coded = previous.clone();
        coded.canonical_code = Some("E0277".to_owned());
        record_error_fingerprint(&connection, WS, &coded)?;
        assert!(error_recall_report(&connection, WS, &current)?.near.is_empty());
        let mut foreign_tool = previous;
        foreign_tool.tool = DiagnosticTool::Rustc;
        record_error_fingerprint(&connection, WS, &foreign_tool)?;
        assert!(error_recall_report(&connection, WS, &current)?.near.is_empty());
        Ok(())
    }
}
