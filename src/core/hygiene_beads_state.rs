//! bd-1eq3l.4 narrow slice — Beads metadata state extraction (pure
//! parse + reservation overlay).
//!
//! This slice covers the bd-1eq3l.4 classifications that can be
//! computed WITHOUT shelling out to `bd doctor` / `bd sync --status`:
//!
//! - [`BeadsClassification::BeadsClean`]
//! - [`BeadsClassification::BeadsExportOnly`]
//! - [`BeadsClassification::BeadsConflictOrParseError`]
//! - [`BeadsClassification::BeadsReservedByOtherAgent`]
//!
//! DB-divergence variants are represented by a caller-supplied metadata
//! signal. The classifier remains pure; the caller decides whether that
//! signal came from Beads DB freshness checks, doctor output, or a
//! synthetic test fixture:
//!
//! - [`BeadsClassification::BeadsDbDirtyPendingFlush`]
//! - [`BeadsClassification::BeadsExternalChangesPendingImport`]
//! - [`BeadsClassification::BeadsLikelyCommitReady`]
//!
//! When no metadata signal is available, a dirty export falls back to
//! [`BeadsClassification::BeadsExportOnly`] with the degraded code
//! [`degraded::DB_DIVERGENCE_UNKNOWN`] so downstream callers know the
//! verdict is partial.
//!
//! ## Hard contract
//!
//! - **Pure** — no `std::fs` writes, no process shell-outs, no git
//!   mutations. The caller is responsible for collecting the
//!   `.beads/issues.jsonl` content (bounded) and the Agent Mail
//!   reservation list.
//! - **Reservation priority** — per bd-1eq3l.4 acceptance: "Never
//!   claim Beads metadata is safe to commit while an active exclusive
//!   reservation exists for `.beads/issues.jsonl`." An exclusive
//!   reservation held by another agent overrides every other verdict.
//! - **Truncation safety** — the JSONL content scan stops after
//!   [`BEADS_JSONL_MAX_INSPECT_BYTES`] and emits
//!   [`degraded::JSONL_INSPECTION_TRUNCATED`]; classification of the
//!   inspected prefix is unaffected.

use serde::Serialize;

use crate::core::swarm_brief::WorkspaceGitSnapshot;

/// JSON schema constant for the Beads hygiene state report.
pub const BEADS_HYGIENE_STATE_SCHEMA_V1: &str = "ee.beads_hygiene_state.v1";

/// Canonical repo-relative path for the Beads JSONL export.
pub const BEADS_JSONL_RELATIVE_PATH: &str = ".beads/issues.jsonl";

/// Maximum bytes of `.beads/issues.jsonl` the pure inspector reads.
/// The caller is responsible for honoring this when collecting
/// `jsonl_content`; we re-enforce the bound here so a too-large slice
/// does not cause a runaway scan in this module.
pub const BEADS_JSONL_MAX_INSPECT_BYTES: usize = 8 * 1024 * 1024;

/// Degraded-code constants emitted in [`BeadsHygieneState::degraded_codes`].
/// Stable, used by docs/degraded_code_taxonomy.md + downstream callers.
pub mod degraded {
    /// JSONL was scanned but the inspector saw `len > BEADS_JSONL_MAX_INSPECT_BYTES`.
    pub const JSONL_INSPECTION_TRUNCATED: &str = "workspace_hygiene_beads_jsonl_truncated";
    /// JSONL content was not provided to the inspector. Classifications
    /// that depend on content scanning fall back to the dirty-bit signal
    /// alone.
    pub const CONTENT_NOT_PROVIDED: &str = "workspace_hygiene_beads_content_not_provided";
    /// Caller did not provide a DB/export divergence signal, so a dirty
    /// JSONL posture can only be classified as export-only with this
    /// diagnostic.
    pub const DB_DIVERGENCE_UNKNOWN: &str = "workspace_hygiene_beads_db_divergence_unknown";
    /// A self-agent's own exclusive reservation was filtered out (this
    /// is informational, not a downgrade). Emitted only when the
    /// classifier observed at least one self-reservation while
    /// computing the result.
    pub const SELF_RESERVATION_OBSERVED: &str = "workspace_hygiene_beads_self_reservation";
}

/// Workspace-level Beads metadata classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BeadsClassification {
    BeadsClean,
    BeadsExportOnly,
    BeadsDbDirtyPendingFlush,
    BeadsExternalChangesPendingImport,
    BeadsConflictOrParseError,
    BeadsReservedByOtherAgent,
    BeadsLikelyCommitReady,
}

impl BeadsClassification {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BeadsClean => "beads_clean",
            Self::BeadsExportOnly => "beads_export_only",
            Self::BeadsDbDirtyPendingFlush => "beads_db_dirty_pending_flush",
            Self::BeadsExternalChangesPendingImport => "beads_external_changes_pending_import",
            Self::BeadsConflictOrParseError => "beads_conflict_or_parse_error",
            Self::BeadsReservedByOtherAgent => "beads_reserved_by_other_agent",
            Self::BeadsLikelyCommitReady => "beads_likely_commit_ready",
        }
    }

    /// Stable rank used when two signals conflict. Lower wins. Order:
    /// reservation > conflict/parse > db-divergence > export > clean.
    #[must_use]
    pub const fn safety_rank(self) -> u8 {
        match self {
            Self::BeadsReservedByOtherAgent => 0,
            Self::BeadsConflictOrParseError => 1,
            Self::BeadsDbDirtyPendingFlush => 2,
            Self::BeadsExternalChangesPendingImport => 3,
            Self::BeadsExportOnly => 4,
            Self::BeadsLikelyCommitReady => 5,
            Self::BeadsClean => 6,
        }
    }
}

/// Active Agent Mail reservation overlay for `.beads/issues.jsonl`.
/// The caller is responsible for collecting only reservations whose
/// `path_pattern` matches the JSONL path; this module does not glob.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BeadsReservationHolder {
    pub agent_name: String,
    pub exclusive: bool,
    pub expires_ts_rfc3339: String,
}

/// Echo of the porcelain v2 dirty-bits for `.beads/issues.jsonl`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BeadsJsonlPosture {
    pub present_in_dirty_set: bool,
    pub staged_change: bool,
    pub unstaged_change: bool,
    pub untracked: bool,
    pub entry_kind: Option<String>,
}

/// Metadata/freshness signal supplied by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BeadsMetadataSignal {
    /// Caller could not determine DB/export divergence.
    Unknown,
    /// JSONL changed only because the tracker exported metadata.
    ExportOnly,
    /// Beads DB has local changes that still need JSONL export.
    DbDirtyPendingFlush,
    /// JSONL has changes that the Beads DB still needs to import.
    ExternalChangesPendingImport,
    /// Metadata checks found no divergence and the Beads JSONL change
    /// is ready for the broader staging recommender to evaluate.
    LikelyCommitReady,
}

impl BeadsMetadataSignal {
    #[must_use]
    const fn classification(self, jsonl_is_dirty: bool) -> BeadsClassification {
        match self {
            Self::Unknown => {
                if jsonl_is_dirty {
                    BeadsClassification::BeadsExportOnly
                } else {
                    BeadsClassification::BeadsClean
                }
            }
            Self::ExportOnly => BeadsClassification::BeadsExportOnly,
            Self::DbDirtyPendingFlush => BeadsClassification::BeadsDbDirtyPendingFlush,
            Self::ExternalChangesPendingImport => {
                BeadsClassification::BeadsExternalChangesPendingImport
            }
            Self::LikelyCommitReady => BeadsClassification::BeadsLikelyCommitReady,
        }
    }

    #[must_use]
    const fn needs_unknown_degraded_code(self, jsonl_is_dirty: bool) -> bool {
        matches!(self, Self::Unknown) && jsonl_is_dirty
    }
}

impl BeadsJsonlPosture {
    fn from_snapshot(snapshot: &WorkspaceGitSnapshot) -> Self {
        let entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.path == BEADS_JSONL_RELATIVE_PATH);
        match entry {
            None => Self {
                present_in_dirty_set: false,
                staged_change: false,
                unstaged_change: false,
                untracked: false,
                entry_kind: None,
            },
            Some(entry) => Self {
                present_in_dirty_set: true,
                staged_change: is_significant_status_char(&entry.staged),
                unstaged_change: is_significant_status_char(&entry.unstaged),
                untracked: entry.entry_kind == "untracked",
                entry_kind: Some(entry.entry_kind.clone()),
            },
        }
    }
}

/// Porcelain v2 XY chars: `.`, ` `, and `?` are not interesting
/// indicators of "a delta is staged here." `?` shows up on untracked
/// entries; the `untracked` field is the right place for that signal.
fn is_significant_status_char(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        None => false,
        Some('.') | Some(' ') | Some('?') => false,
        Some(_) => true,
    }
}

/// Top-level state report for `.beads/issues.jsonl`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BeadsHygieneState {
    pub schema: &'static str,
    pub classification: BeadsClassification,
    pub jsonl_posture: BeadsJsonlPosture,
    pub metadata_signal: BeadsMetadataSignal,
    pub conflict_markers_found: bool,
    pub parse_error_line: Option<usize>,
    pub reservation_holders: Vec<BeadsReservationHolder>,
    pub degraded_codes: Vec<&'static str>,
}

/// Inputs bundle for [`classify_beads_state`]. Borrowed because the
/// classifier is pure and does not own any of its data.
#[derive(Clone, Copy, Debug)]
pub struct BeadsHygieneInputs<'a> {
    pub snapshot: &'a WorkspaceGitSnapshot,
    /// Bytes of `.beads/issues.jsonl` capped at
    /// [`BEADS_JSONL_MAX_INSPECT_BYTES`]. `None` means the caller
    /// elected not to read the file (e.g. disk pressure, permission
    /// denied) — classification degrades accordingly.
    pub jsonl_content: Option<&'a [u8]>,
    /// The current calling agent's name. Used to skip self-held
    /// reservations during the reservation-overlay check.
    pub self_agent_name: Option<&'a str>,
    /// Beads DB/export divergence signal collected by the caller.
    pub metadata_signal: BeadsMetadataSignal,
    /// Active reservations (already filtered to the JSONL path by the
    /// caller — this module does not glob).
    pub reservations: &'a [BeadsReservationHolder],
}

/// Compute a Beads hygiene state report from pre-collected inputs.
///
/// Deterministic; same inputs → same byte-stable output (including the
/// `degraded_codes` ordering).
#[must_use]
pub fn classify_beads_state(inputs: BeadsHygieneInputs<'_>) -> BeadsHygieneState {
    let jsonl_posture = BeadsJsonlPosture::from_snapshot(inputs.snapshot);

    // Reservation overlay: filter to OTHER agents holding exclusive
    // leases. Self-reservations are recorded for transparency but do
    // not change the verdict.
    let mut other_agent_reservations: Vec<BeadsReservationHolder> = Vec::new();
    let mut saw_self_reservation = false;
    for holder in inputs.reservations {
        if Some(holder.agent_name.as_str()) == inputs.self_agent_name {
            saw_self_reservation = true;
            continue;
        }
        if holder.exclusive {
            other_agent_reservations.push(holder.clone());
        }
    }
    other_agent_reservations.sort_by(|left, right| left.agent_name.cmp(&right.agent_name));

    let jsonl_is_dirty = jsonl_posture.present_in_dirty_set;
    let (conflict_markers_found, parse_error_line, mut content_degraded) = if jsonl_is_dirty {
        scan_jsonl_content(inputs.jsonl_content)
    } else {
        (false, None, Vec::new())
    };

    // Classification cascade — order matters. The cascade encodes
    // coordination-safety first, then data-integrity, then metadata
    // freshness.
    if !other_agent_reservations.is_empty() {
        // Reservation by another agent wins over every other verdict
        // (per the bead's acceptance criteria: never claim safe to
        // commit while an exclusive reservation is active).
        return BeadsHygieneState {
            schema: BEADS_HYGIENE_STATE_SCHEMA_V1,
            classification: BeadsClassification::BeadsReservedByOtherAgent,
            jsonl_posture,
            metadata_signal: inputs.metadata_signal,
            conflict_markers_found,
            parse_error_line,
            reservation_holders: other_agent_reservations,
            degraded_codes: maybe_with_self_reservation_code(
                content_degraded.clone(),
                saw_self_reservation,
            ),
        };
    }

    if conflict_markers_found || parse_error_line.is_some() {
        return BeadsHygieneState {
            schema: BEADS_HYGIENE_STATE_SCHEMA_V1,
            classification: BeadsClassification::BeadsConflictOrParseError,
            jsonl_posture,
            metadata_signal: inputs.metadata_signal,
            conflict_markers_found,
            parse_error_line,
            reservation_holders: other_agent_reservations,
            degraded_codes: maybe_with_self_reservation_code(
                content_degraded.clone(),
                saw_self_reservation,
            ),
        };
    }

    if inputs
        .metadata_signal
        .needs_unknown_degraded_code(jsonl_is_dirty)
    {
        content_degraded.push(degraded::DB_DIVERGENCE_UNKNOWN);
    }
    content_degraded.sort_unstable();
    content_degraded.dedup();
    BeadsHygieneState {
        schema: BEADS_HYGIENE_STATE_SCHEMA_V1,
        classification: inputs.metadata_signal.classification(jsonl_is_dirty),
        jsonl_posture,
        metadata_signal: inputs.metadata_signal,
        conflict_markers_found,
        parse_error_line,
        reservation_holders: other_agent_reservations,
        degraded_codes: maybe_with_self_reservation_code(content_degraded, saw_self_reservation),
    }
}

fn maybe_with_self_reservation_code(
    mut codes: Vec<&'static str>,
    saw_self: bool,
) -> Vec<&'static str> {
    if saw_self {
        codes.push(degraded::SELF_RESERVATION_OBSERVED);
        codes.sort_unstable();
        codes.dedup();
    }
    codes
}

/// Returns `(conflict_markers_found, parse_error_line, degraded_codes)`.
fn scan_jsonl_content(content: Option<&[u8]>) -> (bool, Option<usize>, Vec<&'static str>) {
    let mut degraded = Vec::new();
    let raw = match content {
        Some(bytes) => bytes,
        None => {
            degraded.push(degraded::CONTENT_NOT_PROVIDED);
            return (false, None, degraded);
        }
    };
    let mut bytes = raw;
    if bytes.len() > BEADS_JSONL_MAX_INSPECT_BYTES {
        bytes = &bytes[..BEADS_JSONL_MAX_INSPECT_BYTES];
        degraded.push(degraded::JSONL_INSPECTION_TRUNCATED);
    }
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => {
            // Non-UTF-8 content in a JSONL export is itself a parse
            // error; report at line 0 (the file as a whole).
            return (false, Some(0), degraded);
        }
    };
    let mut conflict_markers_found = false;
    let mut parse_error_line: Option<usize> = None;
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        if line.starts_with("<<<<<<<") || line.starts_with("=======") || line.starts_with(">>>>>>>")
        {
            conflict_markers_found = true;
            // Keep scanning so we also catch the first parse error if
            // any; the reservation/conflict combination is reported
            // together.
            continue;
        }
        if line.is_empty() {
            continue;
        }
        if parse_error_line.is_some() {
            continue;
        }
        if !looks_like_json_object(line) {
            parse_error_line = Some(line_number);
        }
    }
    (conflict_markers_found, parse_error_line, degraded)
}

/// Strict JSONL line validator for Beads export rows.
fn looks_like_json_object(line: &str) -> bool {
    let trimmed = line.trim();
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(serde_json::Value::Object(_)) => true,
        Ok(_) | Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::swarm_brief::WorkspaceGitStatusEntry;

    fn entry(
        path: &str,
        staged: &str,
        unstaged: &str,
        entry_kind: &str,
    ) -> WorkspaceGitStatusEntry {
        WorkspaceGitStatusEntry {
            path: path.to_owned(),
            original_path: None,
            staged: staged.to_owned(),
            unstaged: unstaged.to_owned(),
            entry_kind: entry_kind.to_owned(),
            submodule_state: None,
            metadata: None,
        }
    }

    fn snapshot(entries: Vec<WorkspaceGitStatusEntry>) -> WorkspaceGitSnapshot {
        WorkspaceGitSnapshot {
            repository_root: "/tmp/beads-repo".to_owned(),
            entries,
        }
    }

    fn reservation(agent: &str, exclusive: bool) -> BeadsReservationHolder {
        BeadsReservationHolder {
            agent_name: agent.to_owned(),
            exclusive,
            expires_ts_rfc3339: "2026-05-17T10:00:00Z".to_owned(),
        }
    }

    fn well_formed_jsonl() -> &'static [u8] {
        // Two minimal Beads-shaped records.
        b"{\"id\":\"bd-foo\",\"title\":\"x\"}\n{\"id\":\"bd-bar\",\"title\":\"y\"}\n"
    }

    #[test]
    fn clean_workspace_classifies_as_beads_clean() {
        let snap = snapshot(Vec::new());
        let state = classify_beads_state(BeadsHygieneInputs {
            snapshot: &snap,
            jsonl_content: None,
            self_agent_name: Some("TopazSpring"),
            metadata_signal: BeadsMetadataSignal::Unknown,
            reservations: &[],
        });
        assert_eq!(state.classification, BeadsClassification::BeadsClean);
        assert!(!state.jsonl_posture.present_in_dirty_set);
        assert!(state.reservation_holders.is_empty());
        assert!(!state.conflict_markers_found);
        assert!(state.parse_error_line.is_none());
    }

    #[test]
    fn dirty_jsonl_without_other_reservation_or_conflict_is_export_only_with_db_divergence_unknown()
    {
        let snap = snapshot(vec![entry(BEADS_JSONL_RELATIVE_PATH, "M", "M", "ordinary")]);
        let state = classify_beads_state(BeadsHygieneInputs {
            snapshot: &snap,
            jsonl_content: Some(well_formed_jsonl()),
            self_agent_name: Some("TopazSpring"),
            metadata_signal: BeadsMetadataSignal::Unknown,
            reservations: &[],
        });
        assert_eq!(state.classification, BeadsClassification::BeadsExportOnly);
        assert!(state.jsonl_posture.present_in_dirty_set);
        assert!(state.jsonl_posture.staged_change);
        assert!(state.jsonl_posture.unstaged_change);
        assert!(
            state
                .degraded_codes
                .contains(&degraded::DB_DIVERGENCE_UNKNOWN),
            "missing db_divergence_unknown in {:?}",
            state.degraded_codes
        );
        assert!(!state.conflict_markers_found);
        assert!(state.parse_error_line.is_none());
    }

    #[test]
    fn untracked_jsonl_is_export_only() {
        let snap = snapshot(vec![entry(
            BEADS_JSONL_RELATIVE_PATH,
            "?",
            "?",
            "untracked",
        )]);
        let state = classify_beads_state(BeadsHygieneInputs {
            snapshot: &snap,
            jsonl_content: Some(well_formed_jsonl()),
            self_agent_name: Some("TopazSpring"),
            metadata_signal: BeadsMetadataSignal::Unknown,
            reservations: &[],
        });
        assert_eq!(state.classification, BeadsClassification::BeadsExportOnly);
        assert!(state.jsonl_posture.untracked);
        assert!(!state.jsonl_posture.staged_change);
        assert!(!state.jsonl_posture.unstaged_change);
    }

    #[test]
    fn db_dirty_signal_classifies_as_pending_flush_even_without_jsonl_dirty() {
        let snap = snapshot(Vec::new());
        let state = classify_beads_state(BeadsHygieneInputs {
            snapshot: &snap,
            jsonl_content: None,
            self_agent_name: Some("TopazSpring"),
            metadata_signal: BeadsMetadataSignal::DbDirtyPendingFlush,
            reservations: &[],
        });
        assert_eq!(
            state.classification,
            BeadsClassification::BeadsDbDirtyPendingFlush
        );
        assert!(!state.jsonl_posture.present_in_dirty_set);
        assert_eq!(
            state.metadata_signal,
            BeadsMetadataSignal::DbDirtyPendingFlush
        );
        assert!(state.degraded_codes.is_empty());
    }

    #[test]
    fn external_import_pending_signal_classifies_as_external_changes_pending_import() {
        let snap = snapshot(vec![entry(BEADS_JSONL_RELATIVE_PATH, ".", "M", "ordinary")]);
        let state = classify_beads_state(BeadsHygieneInputs {
            snapshot: &snap,
            jsonl_content: Some(well_formed_jsonl()),
            self_agent_name: Some("TopazSpring"),
            metadata_signal: BeadsMetadataSignal::ExternalChangesPendingImport,
            reservations: &[],
        });
        assert_eq!(
            state.classification,
            BeadsClassification::BeadsExternalChangesPendingImport
        );
        assert_eq!(
            state.metadata_signal,
            BeadsMetadataSignal::ExternalChangesPendingImport
        );
        assert!(
            !state
                .degraded_codes
                .contains(&degraded::DB_DIVERGENCE_UNKNOWN)
        );
    }

    #[test]
    fn clean_metadata_signal_with_dirty_jsonl_classifies_as_likely_commit_ready() {
        let snap = snapshot(vec![entry(BEADS_JSONL_RELATIVE_PATH, "M", ".", "ordinary")]);
        let state = classify_beads_state(BeadsHygieneInputs {
            snapshot: &snap,
            jsonl_content: Some(well_formed_jsonl()),
            self_agent_name: Some("TopazSpring"),
            metadata_signal: BeadsMetadataSignal::LikelyCommitReady,
            reservations: &[],
        });
        assert_eq!(
            state.classification,
            BeadsClassification::BeadsLikelyCommitReady
        );
        assert!(state.jsonl_posture.staged_change);
        assert!(
            !state
                .degraded_codes
                .contains(&degraded::DB_DIVERGENCE_UNKNOWN)
        );
    }

    #[test]
    fn conflict_markers_classify_as_conflict_or_parse_error() {
        let body = b"{\"id\":\"bd-a\"}\n<<<<<<< HEAD\n{\"id\":\"bd-b\"}\n=======\n{\"id\":\"bd-c\"}\n>>>>>>> theirs\n";
        let snap = snapshot(vec![entry(BEADS_JSONL_RELATIVE_PATH, ".", "M", "ordinary")]);
        let state = classify_beads_state(BeadsHygieneInputs {
            snapshot: &snap,
            jsonl_content: Some(body),
            self_agent_name: Some("TopazSpring"),
            metadata_signal: BeadsMetadataSignal::Unknown,
            reservations: &[],
        });
        assert_eq!(
            state.classification,
            BeadsClassification::BeadsConflictOrParseError
        );
        assert!(state.conflict_markers_found);
    }

    #[test]
    fn invalid_jsonl_line_classifies_as_conflict_or_parse_error_with_line_number() {
        let body = b"{\"id\":\"bd-a\"}\n{not valid json\n{\"id\":\"bd-c\"}\n";
        let snap = snapshot(vec![entry(BEADS_JSONL_RELATIVE_PATH, ".", "M", "ordinary")]);
        let state = classify_beads_state(BeadsHygieneInputs {
            snapshot: &snap,
            jsonl_content: Some(body),
            self_agent_name: Some("TopazSpring"),
            metadata_signal: BeadsMetadataSignal::Unknown,
            reservations: &[],
        });
        assert_eq!(
            state.classification,
            BeadsClassification::BeadsConflictOrParseError
        );
        assert_eq!(state.parse_error_line, Some(2));
        assert!(!state.conflict_markers_found);
    }

    #[test]
    fn exclusive_other_agent_reservation_overrides_export_only() {
        let snap = snapshot(vec![entry(BEADS_JSONL_RELATIVE_PATH, ".", "M", "ordinary")]);
        let holder = reservation("LavenderPeak", true);
        let state = classify_beads_state(BeadsHygieneInputs {
            snapshot: &snap,
            jsonl_content: Some(well_formed_jsonl()),
            self_agent_name: Some("TopazSpring"),
            metadata_signal: BeadsMetadataSignal::Unknown,
            reservations: std::slice::from_ref(&holder),
        });
        assert_eq!(
            state.classification,
            BeadsClassification::BeadsReservedByOtherAgent
        );
        assert_eq!(state.reservation_holders, vec![holder]);
    }

    #[test]
    fn self_agent_reservation_does_not_trigger_reserved_by_other_and_records_diagnostic() {
        let snap = snapshot(vec![entry(BEADS_JSONL_RELATIVE_PATH, ".", "M", "ordinary")]);
        let self_hold = reservation("TopazSpring", true);
        let state = classify_beads_state(BeadsHygieneInputs {
            snapshot: &snap,
            jsonl_content: Some(well_formed_jsonl()),
            self_agent_name: Some("TopazSpring"),
            metadata_signal: BeadsMetadataSignal::Unknown,
            reservations: std::slice::from_ref(&self_hold),
        });
        assert_eq!(state.classification, BeadsClassification::BeadsExportOnly);
        assert!(state.reservation_holders.is_empty());
        assert!(
            state
                .degraded_codes
                .contains(&degraded::SELF_RESERVATION_OBSERVED)
        );
    }

    #[test]
    fn non_exclusive_other_agent_reservation_does_not_block() {
        let snap = snapshot(vec![entry(BEADS_JSONL_RELATIVE_PATH, ".", "M", "ordinary")]);
        let observer = reservation("LavenderPeak", false);
        let state = classify_beads_state(BeadsHygieneInputs {
            snapshot: &snap,
            jsonl_content: Some(well_formed_jsonl()),
            self_agent_name: Some("TopazSpring"),
            metadata_signal: BeadsMetadataSignal::Unknown,
            reservations: std::slice::from_ref(&observer),
        });
        assert_eq!(state.classification, BeadsClassification::BeadsExportOnly);
        assert!(state.reservation_holders.is_empty());
    }

    #[test]
    fn reservation_takes_priority_over_conflict_markers() {
        // The bead's invariant: never claim safe-to-commit while an
        // exclusive reservation is held — even if the content is also
        // corrupt, the reservation classification wins because it's
        // about coordination safety. (Conflict info is still recorded
        // in the row's auxiliary fields.)
        let body = b"<<<<<<< HEAD\nbroken\n>>>>>>> theirs\n";
        let snap = snapshot(vec![entry(BEADS_JSONL_RELATIVE_PATH, ".", "M", "ordinary")]);
        let holder = reservation("LavenderPeak", true);
        let state = classify_beads_state(BeadsHygieneInputs {
            snapshot: &snap,
            jsonl_content: Some(body),
            self_agent_name: Some("TopazSpring"),
            metadata_signal: BeadsMetadataSignal::Unknown,
            reservations: std::slice::from_ref(&holder),
        });
        assert_eq!(
            state.classification,
            BeadsClassification::BeadsReservedByOtherAgent
        );
        assert!(state.conflict_markers_found);
    }

    #[test]
    fn missing_jsonl_content_records_degraded_code_and_falls_back_to_export_only() {
        let snap = snapshot(vec![entry(BEADS_JSONL_RELATIVE_PATH, ".", "M", "ordinary")]);
        let state = classify_beads_state(BeadsHygieneInputs {
            snapshot: &snap,
            jsonl_content: None,
            self_agent_name: Some("TopazSpring"),
            metadata_signal: BeadsMetadataSignal::Unknown,
            reservations: &[],
        });
        assert_eq!(state.classification, BeadsClassification::BeadsExportOnly);
        assert!(
            state
                .degraded_codes
                .contains(&degraded::CONTENT_NOT_PROVIDED)
        );
    }

    #[test]
    fn oversized_jsonl_content_records_truncation_degraded_code() {
        // Build a body that exceeds the inspection cap; the head is
        // well-formed so classification is BeadsExportOnly + truncated.
        let mut body = Vec::with_capacity(BEADS_JSONL_MAX_INSPECT_BYTES + 1024);
        body.extend_from_slice(well_formed_jsonl());
        body.resize(BEADS_JSONL_MAX_INSPECT_BYTES + 1024, b'\n');
        let snap = snapshot(vec![entry(BEADS_JSONL_RELATIVE_PATH, ".", "M", "ordinary")]);
        let state = classify_beads_state(BeadsHygieneInputs {
            snapshot: &snap,
            jsonl_content: Some(&body),
            self_agent_name: Some("TopazSpring"),
            metadata_signal: BeadsMetadataSignal::Unknown,
            reservations: &[],
        });
        assert!(
            state
                .degraded_codes
                .contains(&degraded::JSONL_INSPECTION_TRUNCATED)
        );
    }

    #[test]
    fn classification_safety_rank_is_total_order_with_no_ties() {
        let variants = [
            BeadsClassification::BeadsReservedByOtherAgent,
            BeadsClassification::BeadsConflictOrParseError,
            BeadsClassification::BeadsDbDirtyPendingFlush,
            BeadsClassification::BeadsExternalChangesPendingImport,
            BeadsClassification::BeadsExportOnly,
            BeadsClassification::BeadsLikelyCommitReady,
            BeadsClassification::BeadsClean,
        ];
        let mut ranks: Vec<u8> = variants.iter().map(|v| v.safety_rank()).collect();
        ranks.sort_unstable();
        ranks.dedup();
        assert_eq!(ranks.len(), variants.len(), "safety_rank must be injective");
    }

    #[test]
    fn purity_module_does_not_perform_io_or_shell_out() {
        // Same self-scan style as the hygiene_classifier guard, but
        // built up from prefix/suffix pairs so the test does not
        // self-match its own literal list. Per [[hygiene_classifier]]
        // discipline.
        let source = include_str!("hygiene_beads_state.rs");
        let prefixes = ["std::fs::", "std::process::", "tokio::process"];
        let suffixes_fs = [
            "write",
            "create_dir",
            "remove_file",
            "rename",
            "File::create",
        ];
        let suffixes_proc = ["Command", "Child"];
        for prefix in &prefixes {
            let candidates: &[&str] = if prefix.contains("fs") {
                &suffixes_fs
            } else if prefix.contains("process") || prefix.contains("Command") {
                &suffixes_proc
            } else {
                &[]
            };
            for suffix in candidates {
                let combined = format!("{prefix}{suffix}");
                assert!(
                    !source.contains(&combined),
                    "hygiene_beads_state.rs should not contain `{combined}` — module must remain pure"
                );
            }
        }
    }

    #[test]
    fn looks_like_json_object_accepts_balanced_object_with_nested_braces_and_strings() {
        assert!(looks_like_json_object(
            "{\"id\":\"bd-a\",\"data\":{\"nested\":[1,2,3]}}"
        ));
        assert!(looks_like_json_object("{\"k\":\"with \\\"quotes\\\"\"}"));
        assert!(looks_like_json_object("{}"));
    }

    #[test]
    fn looks_like_json_object_rejects_unbalanced_or_non_object_lines() {
        assert!(!looks_like_json_object("{not valid"));
        assert!(!looks_like_json_object("{not valid}"));
        assert!(!looks_like_json_object("plain text"));
        assert!(!looks_like_json_object("[1,2,3]"));
        assert!(!looks_like_json_object("{\"k\":\"v\""));
        // Unterminated string.
        assert!(!looks_like_json_object("{\"k\":\"x}"));
    }
}
