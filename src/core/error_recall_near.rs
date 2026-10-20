//! Advisory near-error recall for the complete diagnostic report.
//!
//! Exact lookup and interactive pack expansion do not use this scan. On a
//! code-less exact miss, the complete report scans the workspace's persisted
//! same-tool signatures in bounded pages and retains at most five neighbors.
//! This is O(N) signature I/O, not an indexed neighborhood lookup. Unused
//! location/version metadata and other tools' fingerprints are never hydrated.
//! Similarity is not proof
//! that a repair applies: exact status, repair/proof lists and pack seeds stay
//! unchanged. All pages share one source snapshot; no partial answer escapes
//! an unavailable page or cancelled request.

use std::collections::BTreeSet;

use sqlmodel_core::Value;

use crate::core::error_recall::{
    CanonicalDiagnostic, ErrorFingerprint, SIMHASH_TAIL_MAX_DISTANCE, simhash_hamming_distance,
};
#[cfg(test)]
use crate::db::StoredErrorFingerprint;
use crate::db::{DbConnection, DbError, DbOperation, Result};

use super::ErrorRecallReport;

const NEAR_MATCH_LIMIT: usize = 5;
const SIGNATURE_PAGE_SIZE: usize = 128;
const MAX_FINGERPRINT_KEY_BYTES: usize = 128;

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
    report.near = scan_observed(connection, workspace_id, fingerprint, |_| Ok(()))?;
    Ok(())
}

fn unavailable() -> DbError {
    DbError::MalformedRow {
        operation: DbOperation::Query,
        message: "Could not complete near-error signature scan; no partial neighbors returned"
            .to_owned(),
    }
}

/// The existing (workspace, tool, canonical_code) index ends in rowid, so a
/// cursor within one exact prefix avoids offsets or repeated whole-set sorts.
/// Physical row numbers only advance this snapshot's scan; they never enter
/// neighbor ranking, source identity, or persistent feedback. A separate first
/// page includes even i64::MIN rather than assuming rowids start above zero.
fn scan_observed(
    connection: &DbConnection,
    workspace: &str,
    query: &ErrorFingerprint,
    mut after_page: impl FnMut(usize) -> Result<()>,
) -> Result<Vec<String>> {
    let snapshot =
        super::memory::Snapshot::begin_or_borrow(connection).map_err(|_| unavailable())?;
    let request = asupersync::Cx::current();
    let checkpoint = || -> Result<()> {
        if let Some(cx) = &request {
            cx.checkpoint().map_err(|_| unavailable())?;
        }
        Ok(())
    };
    let mut after: Option<i64> = None;
    let mut nearest = Nearest::new(query);
    loop {
        checkpoint()?;
        let cursor = if after.is_some() {
            " AND rowid > ?3"
        } else {
            ""
        };
        // Bound each returned field independently. Oversized or invalid
        // signatures cannot be near matches; returning an empty value lets
        // the shared validator reject them without exposing their payloads.
        // Do not fetch location_shape, version_hints or timestamps at all.
        let sql = format!(
            "SELECT CAST(rowid AS TEXT), CASE WHEN length(CAST(fingerprint_key AS BLOB)) <= {MAX_FINGERPRINT_KEY_BYTES} THEN fingerprint_key ELSE '' END, CASE WHEN length(CAST(message_template_signature AS BLOB)) = 71 THEN message_template_signature ELSE '' END, CASE WHEN length(CAST(stderr_simhash AS BLOB)) = 32 THEN stderr_simhash ELSE '' END FROM error_fingerprints WHERE workspace_id = ?1 AND tool = ?2 AND canonical_code IS NULL{cursor} ORDER BY rowid ASC LIMIT {SIGNATURE_PAGE_SIZE}"
        );
        let mut parameters = vec![
            Value::Text(workspace.to_owned()),
            Value::Text(query.tool.as_str().to_owned()),
        ];
        if let Some(cursor) = after {
            parameters.push(Value::BigInt(cursor));
        }
        let rows = connection
            .query(&sql, &parameters)
            .map_err(|_| unavailable())?;
        checkpoint()?;
        if rows.len() > SIGNATURE_PAGE_SIZE {
            return Err(unavailable());
        }
        let count = rows.len();
        for row in &rows {
            let text = |index| {
                row.get(index)
                    .and_then(Value::as_str)
                    .ok_or_else(unavailable)
            };
            let cursor = text(0)?.parse::<i64>().map_err(|_| unavailable())?;
            if after.is_some_and(|previous| cursor <= previous) {
                return Err(unavailable());
            }
            let key = text(1)?;
            let signature = text(2)?;
            let simhash = text(3)?;
            if key.len() > MAX_FINGERPRINT_KEY_BYTES || signature.len() > 71 || simhash.len() > 32 {
                return Err(unavailable());
            }
            nearest.consider(query, key, signature, simhash);
            after = Some(cursor);
        }
        // Release a page before observing completion or requesting the next.
        drop(rows);
        if count != 0 {
            after_page(count)?;
        }
        checkpoint()?;
        if count < SIGNATURE_PAGE_SIZE {
            break;
        }
    }
    snapshot.finish().map_err(|_| unavailable())?;
    Ok(nearest.finish())
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
    exact_key: &str,
    key: &str,
    template_signature: &str,
    stderr_simhash: &str,
) -> Option<u32> {
    if key == exact_key {
        return None;
    }
    let signature = template_signature.strip_prefix("blake3:")?;
    if !hex_field(signature, 64)
        || key != format!("{}:tmpl:{template_signature}", query.tool.as_str())
        || !hex_field(stderr_simhash, 32)
    {
        return None;
    }
    let hash = u128::from_str_radix(stderr_simhash, 16).ok()?;
    if degenerate(hash) {
        return None;
    }
    let distance = simhash_hamming_distance(query.stderr_simhash, hash);
    (distance <= SIMHASH_TAIL_MAX_DISTANCE).then_some(distance)
}

struct Nearest {
    exact_key: String,
    matches: Vec<(u32, String)>,
}

impl Nearest {
    fn new(query: &ErrorFingerprint) -> Self {
        Self {
            exact_key: query.layered_key().key,
            matches: Vec::with_capacity(NEAR_MATCH_LIMIT + 1),
        }
    }

    fn consider(&mut self, query: &ErrorFingerprint, key: &str, signature: &str, simhash: &str) {
        let Some(distance) = candidate_distance(query, &self.exact_key, key, signature, simhash)
        else {
            return;
        };
        // Keep the original order-independent duplicate policy. Only a better
        // observation replaces the same key, and at most five keys survive.
        if let Some(position) = self.matches.iter().position(|(_, prior)| prior == key) {
            if self.matches[position].0 <= distance {
                return;
            }
            self.matches.remove(position);
        }
        self.matches.push((distance, key.to_owned()));
        self.matches.sort();
        self.matches.truncate(NEAR_MATCH_LIMIT);
    }

    fn finish(self) -> Vec<String> {
        self.matches.into_iter().map(|(_, key)| key).collect()
    }
}

#[cfg(test)]
fn select(
    query: &ErrorFingerprint,
    workspace_id: &str,
    candidates: impl IntoIterator<Item = StoredErrorFingerprint>,
) -> Vec<String> {
    if query.canonical_code.is_some() || degenerate(query.stderr_simhash) {
        return Vec::new();
    }
    let mut nearest = Nearest::new(query);
    for candidate in candidates {
        if candidate.workspace_id != workspace_id
            || candidate.tool != query.tool.as_str()
            || candidate.canonical_code.is_some()
        {
            continue;
        }
        nearest.consider(
            query,
            &candidate.fingerprint_key,
            &candidate.message_template_signature,
            &candidate.stderr_simhash,
        );
    }
    nearest.finish()
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
            select(
                &query(),
                WS,
                [foreign_workspace, foreign_tool, coded, exact]
            )
            .is_empty()
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
        assert!(select(&query(), WS, [signature, key, row(3, 0), row(4, u128::MAX)]).is_empty());
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
            from_cargo(
                None,
                "connection refused while opening deployment transport socket",
            ),
            from_cargo(
                None,
                "deployment transport socket opening while refused connection",
            ),
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
        assert_eq!(
            connection.list_error_fingerprints_for_recovery(WS)?.len(),
            before
        );
        record_error_fingerprint(&connection, WS, &current)?;
        let exact = error_recall_report(&connection, WS, &current)?;
        assert!(exact.exact);
        assert!(exact.near.is_empty());
        Ok(())
    }

    #[test]
    fn paged_scan_matches_complete_selection_and_keeps_late_better_matches() -> TestResult {
        let connection = db()?;
        let query = query();
        let mut candidates = Vec::new();
        // Equal-distance keys arrive in reverse canonical order. Better
        // neighbors also arrive only after the first two complete pages.
        for number in 1..=513_u32 {
            let bits = if number <= 256 { 7 } else { number % 7 };
            let candidate = row(10_000 - number, HASH ^ ((1_u128 << bits) - 1));
            connection.insert_error_fingerprint_for_recovery(&candidate)?;
            candidates.push(candidate);
            if [1, 127, 128, 129, 256, 257, 513].contains(&number) {
                let mut pages = Vec::new();
                let actual = scan_observed(&connection, WS, &query, |count| {
                    pages.push(count);
                    Ok(())
                })?;
                assert_eq!(actual, select(&query, WS, candidates.clone()));
                assert_eq!(pages.iter().sum::<usize>(), number as usize);
                assert!(pages.iter().all(|count| *count <= SIGNATURE_PAGE_SIZE));
                assert!(actual.len() <= NEAR_MATCH_LIMIT);
            }
        }
        let before = connection.list_error_fingerprints_for_recovery(WS)?;
        let expected = select(&query, WS, before.clone());
        assert_eq!(expected.len(), NEAR_MATCH_LIMIT);
        assert_eq!(
            scan_observed(&connection, WS, &query, |_| Ok(()))?,
            expected
        );
        assert_eq!(connection.list_error_fingerprints_for_recovery(WS)?, before);
        Ok(())
    }

    #[test]
    fn paged_scan_includes_negative_zero_and_extreme_physical_rows() -> TestResult {
        let connection = db()?;
        let positions = std::iter::once(i64::MIN)
            .chain(-130_i64..=130)
            .chain(std::iter::once(i64::MAX))
            .collect::<Vec<_>>();
        let mut expected = Vec::new();
        for (index, position) in positions.into_iter().enumerate() {
            let candidate = row(u32::try_from(index + 1)?, HASH ^ (index as u128 % 4));
            connection.insert_error_fingerprint_for_recovery(&candidate)?;
            let position = if position == i64::MIN {
                "(-9223372036854775807 - 1)".to_owned()
            } else {
                position.to_string()
            };
            connection.execute_raw(&format!(
                "UPDATE error_fingerprints SET rowid = {position} WHERE workspace_id = '{WS}' AND fingerprint_key = '{}'",
                candidate.fingerprint_key,
            ))?;
            expected.push(candidate);
        }
        let mut pages = Vec::new();
        let actual = scan_observed(&connection, WS, &query(), |count| {
            pages.push(count);
            Ok(())
        })?;
        assert_eq!(actual, select(&query(), WS, expected));
        assert_eq!(pages, [128, 128, 7]);
        Ok(())
    }

    #[test]
    fn malformed_and_oversized_metadata_cannot_hide_a_later_valid_signature() -> TestResult {
        let connection = db()?;
        for number in 1..=129_u32 {
            let mut invalid = row(number, HASH);
            match number % 3 {
                0 => invalid.fingerprint_key = format!("INVALID_{}", "x".repeat(32_768)),
                1 => invalid.message_template_signature = format!("blake3:{}", "界".repeat(64)),
                _ => invalid.stderr_simhash = "界".repeat(32),
            }
            // Each oversized key must remain distinct in the source store.
            if number % 3 == 0 {
                invalid.fingerprint_key.push_str(&number.to_string());
            }
            connection.insert_error_fingerprint_for_recovery(&invalid)?;
        }
        let mut valid = row(1000, HASH);
        valid.version_hints = Some("UNUSED_VERSION_SENTINEL".repeat(16_384));
        valid.location_shape = Some("UNUSED_LOCATION_SENTINEL".repeat(16_384));
        connection.insert_error_fingerprint_for_recovery(&valid)?;
        let mut foreign = row(1001, HASH);
        foreign.workspace_id = OTHER_WS.to_owned();
        connection.insert_error_fingerprint_for_recovery(&foreign)?;
        let mut coded = row(1002, HASH);
        coded.canonical_code = Some("E0277".to_owned());
        connection.insert_error_fingerprint_for_recovery(&coded)?;
        let mut other_tool = row(1003, HASH);
        other_tool.tool = "shell".to_owned();
        connection.insert_error_fingerprint_for_recovery(&other_tool)?;
        let mut pages = Vec::new();
        let actual = scan_observed(&connection, WS, &query(), |count| {
            pages.push(count);
            Ok(())
        })?;
        assert_eq!(actual, [valid.fingerprint_key]);
        assert_eq!(pages, [128, 2]);
        Ok(())
    }

    #[test]
    fn complete_reports_never_mix_workspaces_or_structured_error_codes() -> TestResult {
        let connection = db()?;
        let (previous, current) = messages();
        record_error_fingerprint(&connection, OTHER_WS, &previous)?;
        assert!(
            error_recall_report(&connection, WS, &current)?
                .near
                .is_empty()
        );
        let mut coded = previous.clone();
        coded.canonical_code = Some("E0277".to_owned());
        record_error_fingerprint(&connection, WS, &coded)?;
        assert!(
            error_recall_report(&connection, WS, &current)?
                .near
                .is_empty()
        );
        let mut foreign_tool = previous;
        foreign_tool.tool = DiagnosticTool::Rustc;
        record_error_fingerprint(&connection, WS, &foreign_tool)?;
        assert!(
            error_recall_report(&connection, WS, &current)?
                .near
                .is_empty()
        );
        Ok(())
    }
}
