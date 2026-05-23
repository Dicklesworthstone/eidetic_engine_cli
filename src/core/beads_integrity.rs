//! Beads tracker integrity diagnostics (bd-2z5ly.9).
//!
//! Pure data model + classifier for the swarm work-packet's "Beads
//! integrity" section. The packet collector layer is responsible for
//! invoking `br doctor --json` (or reading the JSONL/DB directly) and
//! feeding the results into [`compose_integrity_report`]; this module
//! never spawns subprocesses, opens databases, or mutates `.beads`.
//!
//! ## Why not parse `.beads` ourselves?
//!
//! The bd-2z5ly.9 spec is explicit: "do not implement a second Beads
//! parser if an existing collector can provide the data". The collector
//! invariant for this module is therefore:
//!
//! - Inputs are *already-collected*, redacted summaries (record counts,
//!   merge-artifact path patterns, the first malformed JSONL line,
//!   etc.).
//! - Output is a deterministic [`BeadsIntegrityReport`] whose
//!   serialization is byte-stable for the same inputs.
//!
//! ## Health states
//!
//! - [`BeadsIntegrityHealth::Ok`] — JSONL parses, DB and JSONL agree,
//!   no merge artifacts, no pending import.
//! - [`BeadsIntegrityHealth::JsonlParseError`] — at least one line in
//!   `.beads/issues.jsonl` failed to parse; the packet must downgrade
//!   candidate safety because normal `br` reads are not authoritative.
//! - [`BeadsIntegrityHealth::DbJsonlCountMismatch`] — the SQLite store
//!   and JSONL export disagree on record count. Usually caused by a
//!   half-finished import or peer-staged JSONL the local DB has not
//!   yet absorbed.
//! - [`BeadsIntegrityHealth::ExternalChangesPendingImport`] — JSONL has
//!   more rows than the DB (a peer pushed work the local agent has not
//!   imported yet). A specific case of mismatch worth surfacing
//!   separately so the collector can suggest `br sync` rather than
//!   `br doctor --repair`.
//! - [`BeadsIntegrityHealth::MergeArtifactsWarn`] — merge conflict
//!   artifacts (`.orig`, `.rej`, `.merge_artifact*`) are sitting next
//!   to `issues.jsonl`. JSONL may parse, but a recent merge may not
//!   have settled.
//!
//! When more than one condition is true at once the *most severe* one
//! is reported (parse error > count mismatch > merge warn). Pending
//! import is reported only when the DB has fewer rows than the JSONL
//! and the JSONL parses cleanly; otherwise it folds into the mismatch
//! state.

use serde::Serialize;

/// Maximum length (bytes) of the redacted malformed-line excerpt
/// included in the report.
pub const MAX_EXCERPT_LEN: usize = 240;

/// Maximum number of merge-artifact path patterns to retain in the
/// report. Long lists indicate a deeper merge mess that should be
/// summarized by count, not enumerated in the packet.
pub const MAX_MERGE_ARTIFACTS: usize = 8;

/// Top-level health classification of the Beads tracker.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BeadsIntegrityHealth {
    /// JSONL parses, DB and JSONL agree, no merge artifacts, no
    /// pending import.
    Ok,
    /// Merge-conflict artifacts exist next to `issues.jsonl`. JSONL
    /// may still parse, but a recent merge may not have settled.
    MergeArtifactsWarn,
    /// JSONL contains more rows than the DB; the agent should
    /// `br sync` (or `br --no-auto-import --allow-stale` for
    /// read-only inspection) before claiming work.
    ExternalChangesPendingImport,
    /// SQLite store and JSONL disagree on record count for reasons
    /// other than pending import (e.g. DB has rows JSONL does not).
    DbJsonlCountMismatch,
    /// At least one line in `.beads/issues.jsonl` failed to parse.
    /// Normal `br` reads are not authoritative until the bad line is
    /// inspected.
    JsonlParseError,
}

impl BeadsIntegrityHealth {
    /// Stable severity ordering used internally to pick the worst
    /// state when several conditions hold at once. Larger = worse.
    const fn severity_rank(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::MergeArtifactsWarn => 1,
            Self::ExternalChangesPendingImport => 2,
            Self::DbJsonlCountMismatch => 3,
            Self::JsonlParseError => 4,
        }
    }

    /// Whether the work-packet must refuse to recommend candidate
    /// claims while the tracker is in this state. Parse errors and
    /// mismatches make `br ready` non-authoritative; merge artifacts
    /// and pending import are warnings only.
    #[must_use]
    pub const fn requires_candidate_downgrade(self) -> bool {
        matches!(self, Self::JsonlParseError | Self::DbJsonlCountMismatch)
    }

    /// Short, agent-facing recovery hint. Returns `None` for
    /// [`BeadsIntegrityHealth::Ok`] because no action is needed.
    #[must_use]
    pub const fn recovery_hint(self) -> Option<&'static str> {
        match self {
            Self::Ok => None,
            Self::MergeArtifactsWarn => {
                Some("Inspect or remove merge artifacts in .beads/ before relying on br reads.")
            }
            Self::ExternalChangesPendingImport => Some(
                "Run br sync (or br --no-auto-import --allow-stale for read-only inspection) \
                 before claiming work.",
            ),
            Self::DbJsonlCountMismatch => {
                Some("Run br doctor --json and reconcile DB and JSONL before claiming work.")
            }
            Self::JsonlParseError => Some(
                "Inspect the first invalid JSONL line and run br doctor --json before any \
                 br update / br claim.",
            ),
        }
    }
}

/// Location of the first malformed JSONL row.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonlParseError {
    /// 1-indexed line number of the first malformed row.
    pub line: u64,
    /// 1-indexed column of the JSON parse error, when the upstream
    /// parser reports it. `None` when the column is unknown (for
    /// example a truncated final line).
    pub column: Option<u64>,
    /// Redacted excerpt (UTF-8, truncated to [`MAX_EXCERPT_LEN`]
    /// bytes) of the offending line. The collector is responsible
    /// for redacting secrets; this struct only truncates length.
    pub excerpt: String,
}

/// Inputs for [`compose_integrity_report`].
///
/// Each field is the *already-collected* result of a single read-only
/// inspection. The collector layer is responsible for invoking
/// `br doctor --json`, reading file metadata, etc.; this struct is
/// the deterministic data interface between the collector and the
/// packet integrity section.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BeadsIntegrityInputs<'a> {
    /// Project-relative, already-redacted path to the JSONL file
    /// (e.g. `".beads/issues.jsonl"`).
    pub jsonl_path: &'a str,
    /// Project-relative, already-redacted path to the SQLite store
    /// (e.g. `".beads/beads.db"`).
    pub db_path: &'a str,
    /// Total parseable rows in the JSONL file.
    pub jsonl_record_count: u64,
    /// Total rows in the SQLite store.
    pub db_record_count: u64,
    /// Whether `br` is configured to auto-import JSONL on read.
    /// Affects pending-import semantics.
    pub auto_import_enabled: bool,
    /// Project-relative paths to merge-conflict artifacts found
    /// alongside `issues.jsonl`. Already-redacted by the collector.
    pub merge_artifact_paths: &'a [String],
    /// First malformed JSONL row, when the collector observed one.
    pub jsonl_parse_error: Option<JsonlParseError>,
}

/// Deterministic, redacted Beads tracker integrity report.
///
/// Field ordering is stable and reflects the serialized JSON: top-
/// level health first, then file identifiers, then counts, then any
/// parse-error / merge-artifact evidence, then the agent-facing
/// recovery hint.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BeadsIntegrityReport {
    pub health: BeadsIntegrityHealth,
    pub jsonl_path: String,
    pub db_path: String,
    pub jsonl_record_count: u64,
    pub db_record_count: u64,
    pub pending_import_count: u64,
    pub merge_artifact_paths: Vec<String>,
    pub merge_artifact_count: u64,
    pub jsonl_parse_error: Option<JsonlParseError>,
    pub requires_candidate_downgrade: bool,
    pub recovery_hint: Option<&'static str>,
}

/// Compose a deterministic [`BeadsIntegrityReport`] from the
/// already-collected inputs.
///
/// Pure function: no IO, no clock reads, no allocations beyond what's
/// in the inputs. Calling this twice with equal inputs produces equal
/// reports and byte-identical JSON.
///
/// Severity selection rules:
/// 1. `jsonl_parse_error.is_some()` → [`BeadsIntegrityHealth::JsonlParseError`].
/// 2. Otherwise, if DB has rows JSONL does not (db > jsonl) or the
///    record counts differ for any other reason while auto-import is
///    disabled → [`BeadsIntegrityHealth::DbJsonlCountMismatch`].
/// 3. Otherwise, if JSONL has more rows than the DB →
///    [`BeadsIntegrityHealth::ExternalChangesPendingImport`].
/// 4. Otherwise, if any merge artifacts are present →
///    [`BeadsIntegrityHealth::MergeArtifactsWarn`].
/// 5. Otherwise → [`BeadsIntegrityHealth::Ok`].
#[must_use]
pub fn compose_integrity_report(inputs: BeadsIntegrityInputs<'_>) -> BeadsIntegrityReport {
    let parse_error = inputs.jsonl_parse_error.as_ref().map(truncate_parse_error);
    let pending_import_count = inputs
        .jsonl_record_count
        .saturating_sub(inputs.db_record_count);
    let merge_artifact_count = u64::try_from(inputs.merge_artifact_paths.len()).unwrap_or(u64::MAX);

    let health = classify_health(
        parse_error.is_some(),
        inputs.jsonl_record_count,
        inputs.db_record_count,
        inputs.auto_import_enabled,
        merge_artifact_count > 0,
    );

    let mut merge_artifact_paths: Vec<String> = inputs
        .merge_artifact_paths
        .iter()
        .take(MAX_MERGE_ARTIFACTS)
        .cloned()
        .collect();
    merge_artifact_paths.sort();

    BeadsIntegrityReport {
        health,
        jsonl_path: inputs.jsonl_path.to_owned(),
        db_path: inputs.db_path.to_owned(),
        jsonl_record_count: inputs.jsonl_record_count,
        db_record_count: inputs.db_record_count,
        pending_import_count,
        merge_artifact_paths,
        merge_artifact_count,
        jsonl_parse_error: parse_error,
        requires_candidate_downgrade: health.requires_candidate_downgrade(),
        recovery_hint: health.recovery_hint(),
    }
}

/// Pick the most severe [`BeadsIntegrityHealth`] state implied by
/// the input signals. Exposed for tests and the packet collector;
/// `compose_integrity_report` uses this internally.
#[must_use]
pub fn classify_health(
    has_parse_error: bool,
    jsonl_count: u64,
    db_count: u64,
    auto_import_enabled: bool,
    has_merge_artifacts: bool,
) -> BeadsIntegrityHealth {
    let mut candidate = BeadsIntegrityHealth::Ok;
    let mut promote = |state: BeadsIntegrityHealth| {
        if state.severity_rank() > candidate.severity_rank() {
            candidate = state;
        }
    };

    if has_merge_artifacts {
        promote(BeadsIntegrityHealth::MergeArtifactsWarn);
    }
    if jsonl_count != db_count {
        if jsonl_count > db_count && auto_import_enabled && !has_parse_error {
            promote(BeadsIntegrityHealth::ExternalChangesPendingImport);
        } else {
            promote(BeadsIntegrityHealth::DbJsonlCountMismatch);
        }
    }
    if has_parse_error {
        promote(BeadsIntegrityHealth::JsonlParseError);
    }

    candidate
}

fn truncate_parse_error(err: &JsonlParseError) -> JsonlParseError {
    JsonlParseError {
        line: err.line,
        column: err.column,
        excerpt: truncate_utf8(&err.excerpt, MAX_EXCERPT_LEN),
    }
}

/// Truncate `s` to at most `max_bytes` bytes, preserving UTF-8
/// boundaries. Returns the input unchanged when it is already short
/// enough.
fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn ensure_equal<T: std::fmt::Debug + PartialEq>(
        actual: &T,
        expected: &T,
        ctx: &str,
    ) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn base_inputs<'a>(
        merge_artifacts: &'a [String],
        parse_error: Option<JsonlParseError>,
    ) -> BeadsIntegrityInputs<'a> {
        BeadsIntegrityInputs {
            jsonl_path: ".beads/issues.jsonl",
            db_path: ".beads/beads.db",
            jsonl_record_count: 100,
            db_record_count: 100,
            auto_import_enabled: true,
            merge_artifact_paths: merge_artifacts,
            jsonl_parse_error: parse_error,
        }
    }

    #[test]
    fn ok_state_has_no_recovery_hint_and_no_downgrade() -> TestResult {
        let report = compose_integrity_report(base_inputs(&[], None));
        ensure_equal(&report.health, &BeadsIntegrityHealth::Ok, "ok health")?;
        ensure_equal(
            &report.requires_candidate_downgrade,
            &false,
            "ok must not downgrade",
        )?;
        ensure_equal(&report.recovery_hint, &None, "ok has no hint")?;
        ensure_equal(&report.pending_import_count, &0, "ok pending=0")
    }

    #[test]
    fn merge_artifacts_promote_to_merge_artifacts_warn() -> TestResult {
        let artifacts = vec![
            ".beads/issues.jsonl.orig".to_owned(),
            ".beads/issues.jsonl.rej".to_owned(),
        ];
        let report = compose_integrity_report(base_inputs(&artifacts, None));
        ensure_equal(
            &report.health,
            &BeadsIntegrityHealth::MergeArtifactsWarn,
            "merge warn",
        )?;
        ensure_equal(
            &report.requires_candidate_downgrade,
            &false,
            "merge warn must not downgrade candidate safety",
        )?;
        ensure(
            report.recovery_hint.is_some(),
            "merge warn must offer a recovery hint",
        )?;
        ensure_equal(&report.merge_artifact_count, &2, "artifact count")
    }

    #[test]
    fn jsonl_more_than_db_with_auto_import_is_pending_import() -> TestResult {
        let inputs = BeadsIntegrityInputs {
            jsonl_record_count: 105,
            db_record_count: 100,
            auto_import_enabled: true,
            ..base_inputs(&[], None)
        };
        let report = compose_integrity_report(inputs);
        ensure_equal(
            &report.health,
            &BeadsIntegrityHealth::ExternalChangesPendingImport,
            "pending import",
        )?;
        ensure_equal(
            &report.requires_candidate_downgrade,
            &false,
            "pending import is a warning, not a hard downgrade",
        )?;
        ensure_equal(&report.pending_import_count, &5, "pending=5")
    }

    #[test]
    fn db_more_than_jsonl_is_count_mismatch() -> TestResult {
        let inputs = BeadsIntegrityInputs {
            jsonl_record_count: 100,
            db_record_count: 110,
            auto_import_enabled: true,
            ..base_inputs(&[], None)
        };
        let report = compose_integrity_report(inputs);
        ensure_equal(
            &report.health,
            &BeadsIntegrityHealth::DbJsonlCountMismatch,
            "db>jsonl -> mismatch",
        )?;
        ensure_equal(
            &report.requires_candidate_downgrade,
            &true,
            "mismatch must downgrade candidate safety",
        )?;
        ensure_equal(
            &report.pending_import_count,
            &0,
            "saturating_sub clamps pending at 0 when db > jsonl",
        )
    }

    #[test]
    fn jsonl_more_than_db_without_auto_import_is_count_mismatch() -> TestResult {
        // Without auto-import, JSONL drift is not "pending import" — it
        // is a real DB/JSONL mismatch the agent has to resolve manually.
        let inputs = BeadsIntegrityInputs {
            jsonl_record_count: 105,
            db_record_count: 100,
            auto_import_enabled: false,
            ..base_inputs(&[], None)
        };
        let report = compose_integrity_report(inputs);
        ensure_equal(
            &report.health,
            &BeadsIntegrityHealth::DbJsonlCountMismatch,
            "auto_import=false escalates to mismatch",
        )
    }

    #[test]
    fn jsonl_parse_error_overrides_other_states() -> TestResult {
        // Even with a count mismatch and merge artifacts, a parse error
        // is the worst signal and must dominate.
        let artifacts = vec![".beads/issues.jsonl.orig".to_owned()];
        let parse_error = JsonlParseError {
            line: 2701,
            column: Some(42),
            excerpt: "{\"id\":\"bd-bad\",\"type\":truncated".to_owned(),
        };
        let inputs = BeadsIntegrityInputs {
            jsonl_record_count: 2700,
            db_record_count: 2702,
            auto_import_enabled: true,
            ..base_inputs(&artifacts, Some(parse_error.clone()))
        };
        let report = compose_integrity_report(inputs);
        ensure_equal(
            &report.health,
            &BeadsIntegrityHealth::JsonlParseError,
            "parse error wins",
        )?;
        ensure_equal(
            &report.requires_candidate_downgrade,
            &true,
            "parse error must downgrade candidate safety",
        )?;
        ensure_equal(
            &report.jsonl_parse_error,
            &Some(parse_error),
            "parse error round-trips",
        )
    }

    #[test]
    fn parse_error_excerpt_is_truncated_to_max_len() -> TestResult {
        let huge = "x".repeat(MAX_EXCERPT_LEN + 500);
        let parse_error = JsonlParseError {
            line: 1,
            column: None,
            excerpt: huge,
        };
        let report = compose_integrity_report(BeadsIntegrityInputs {
            jsonl_parse_error: Some(parse_error),
            ..base_inputs(&[], None)
        });
        let stored = report
            .jsonl_parse_error
            .as_ref()
            .map(|e| e.excerpt.len())
            .unwrap_or_default();
        ensure(
            stored <= MAX_EXCERPT_LEN,
            format!("excerpt must be <= {MAX_EXCERPT_LEN}, got {stored}"),
        )
    }

    #[test]
    fn merge_artifact_list_is_sorted_and_bounded() -> TestResult {
        let artifacts: Vec<String> = (0..MAX_MERGE_ARTIFACTS + 4)
            .rev()
            .map(|i| format!(".beads/issues.jsonl.orig.{i:03}"))
            .collect();
        let report = compose_integrity_report(base_inputs(&artifacts, None));
        ensure_equal(
            &report.merge_artifact_paths.len(),
            &MAX_MERGE_ARTIFACTS,
            "bounded",
        )?;
        let sorted: Vec<String> = {
            let mut s = report.merge_artifact_paths.clone();
            s.sort();
            s
        };
        ensure_equal(&report.merge_artifact_paths, &sorted, "sorted")?;
        ensure_equal(
            &report.merge_artifact_count,
            &(MAX_MERGE_ARTIFACTS as u64 + 4),
            "count reflects the full input, not the truncated list",
        )
    }

    #[test]
    fn report_serialization_is_byte_stable_across_runs() -> TestResult {
        let inputs = BeadsIntegrityInputs {
            jsonl_record_count: 100,
            db_record_count: 99,
            auto_import_enabled: false,
            ..base_inputs(&[".beads/issues.jsonl.orig".to_owned()], None)
        };
        let first = serde_json::to_string(&compose_integrity_report(inputs.clone()))
            .map_err(|e| format!("serialize first: {e}"))?;
        let second = serde_json::to_string(&compose_integrity_report(inputs))
            .map_err(|e| format!("serialize second: {e}"))?;
        ensure_equal(&first, &second, "deterministic serialization")
    }

    #[test]
    fn truncate_utf8_respects_codepoint_boundaries() -> TestResult {
        // Each "🦀" is 4 bytes; truncating at byte 6 would split the
        // second crab without boundary handling.
        let s = "🦀🦀🦀";
        let truncated = truncate_utf8(s, 6);
        ensure(
            truncated.len() <= 6,
            format!("truncated len {} must be <= 6", truncated.len()),
        )?;
        ensure(
            truncated.is_char_boundary(truncated.len()),
            "truncated must end on a UTF-8 boundary",
        )?;
        ensure_equal(&truncated.as_str(), &"🦀", "first crab survives")
    }

    #[test]
    fn classify_health_severity_ordering_is_stable() -> TestResult {
        use BeadsIntegrityHealth::{
            DbJsonlCountMismatch, ExternalChangesPendingImport, JsonlParseError,
            MergeArtifactsWarn, Ok as HealthOk,
        };
        let ranks = [
            HealthOk.severity_rank(),
            MergeArtifactsWarn.severity_rank(),
            ExternalChangesPendingImport.severity_rank(),
            DbJsonlCountMismatch.severity_rank(),
            JsonlParseError.severity_rank(),
        ];
        ensure(
            ranks.windows(2).all(|w| w[0] < w[1]),
            format!("severity ranks must be strictly increasing: {ranks:?}"),
        )
    }
}
