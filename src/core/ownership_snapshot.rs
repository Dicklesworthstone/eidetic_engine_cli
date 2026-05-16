//! Deterministic ownership snapshots for agent coordination surfaces.
//!
//! The model is intentionally pure: callers provide already-collected Agent
//! Mail reservations, Beads ownership records, and git dirty-file buckets. This
//! module only normalizes and ranks those records, so it never reaches into
//! live coordination services and never stores mail bodies, raw environment
//! dumps, or file contents.

use std::cmp::Ordering;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::tripwire::glob_match;

pub const OWNERSHIP_SNAPSHOT_SCHEMA_V1: &str = "ee.ownership_snapshot.v1";
pub const OWNERSHIP_FILE_REPORT_SCHEMA_V1: &str = "ee.ownership_file_report.v1";
pub const COMPILE_BLOCKER_ATTRIBUTION_SCHEMA_V1: &str = "ee.compile_blocker_attribution.v1";
pub const UNATTRIBUTED_COMPILE_BLOCKER_CODE: &str = "unattributed_compile_blocker";

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipProvenance {
    pub source_kind: String,
    pub source_id: String,
    pub content_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipSnapshot {
    pub schema: String,
    pub generated_at: String,
    pub reservations: Vec<OwnershipReservationSnapshot>,
    pub beads: Vec<BeadOwnershipRecord>,
    pub dirty_files: Vec<DirtyFileRecord>,
}

impl OwnershipSnapshot {
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.reservations.sort_by(compare_reservations);
        self.beads.sort_by(compare_beads);
        self.dirty_files.sort_by(compare_dirty_files);
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipReservationSnapshot {
    pub path_pattern: String,
    pub holder_agent: String,
    pub exclusive: bool,
    pub expires_at: Option<String>,
    pub bead_id: Option<String>,
    pub thread_id: Option<String>,
    pub provenance: OwnershipProvenance,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeadOwnershipRecord {
    pub bead_id: String,
    pub title: String,
    pub status: String,
    pub assignee: Option<String>,
    pub labels: Vec<String>,
    pub file_patterns: Vec<String>,
    pub provenance: OwnershipProvenance,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirtyFileRecord {
    pub path: String,
    pub status: String,
    pub provenance: OwnershipProvenance,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnershipCandidateSource {
    AgentMailReservation,
    BeadsAssignee,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipCandidate {
    pub owner: String,
    pub source: OwnershipCandidateSource,
    pub path_pattern: String,
    pub bead_id: Option<String>,
    pub thread_id: Option<String>,
    pub expires_at: Option<String>,
    pub exclusive: bool,
    pub expired: bool,
    pub exact_match: bool,
    pub pattern_specificity: usize,
    pub provenance: OwnershipProvenance,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOwnershipReport {
    pub schema: String,
    pub path: String,
    pub dirty_status: Option<String>,
    pub conflict: bool,
    pub candidates: Vec<OwnershipCandidate>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RustCompileDiagnostic {
    pub path: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub error_code: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompileBlockerAttributionStatus {
    Attributed,
    Unattributed,
    Unparsed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompileBlockerOwnerCandidate {
    pub owner: String,
    pub source: OwnershipCandidateSource,
    pub confidence: String,
    pub evidence: Vec<String>,
    pub expires_at: Option<String>,
    pub bead_id: Option<String>,
    pub thread_id: Option<String>,
    pub recommended_action: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompileBlockerAttributionReport {
    pub schema: String,
    pub status: CompileBlockerAttributionStatus,
    pub diagnostic: Option<RustCompileDiagnostic>,
    pub owner_candidates: Vec<CompileBlockerOwnerCandidate>,
    pub fallback_code: Option<String>,
    pub suggested_commands: Vec<String>,
}

#[must_use]
pub fn ownership_report_for_path(
    snapshot: &OwnershipSnapshot,
    path: &str,
    as_of: DateTime<Utc>,
) -> FileOwnershipReport {
    let mut candidates = ownership_candidates_for_path(snapshot, path, as_of);
    candidates.sort_by(compare_candidates);
    let dirty_status = dirty_status_for_path(snapshot, path);
    let conflict = has_active_owner_conflict(&candidates);

    FileOwnershipReport {
        schema: OWNERSHIP_FILE_REPORT_SCHEMA_V1.to_owned(),
        path: path.to_owned(),
        dirty_status,
        conflict,
        candidates,
    }
}

#[must_use]
pub fn dirty_file_reports(
    snapshot: &OwnershipSnapshot,
    as_of: DateTime<Utc>,
) -> Vec<FileOwnershipReport> {
    let mut paths = snapshot
        .dirty_files
        .iter()
        .map(|record| record.path.as_str())
        .collect::<Vec<_>>();
    paths.sort_unstable();
    paths.dedup();
    paths
        .into_iter()
        .map(|path| ownership_report_for_path(snapshot, path, as_of))
        .collect()
}

/// Attribute a concise Rust compiler diagnostic excerpt to likely file owners.
///
/// This helper is deliberately pure: callers pass a short failure excerpt and a
/// pre-collected ownership snapshot. It does not launch Cargo/RCH, read Beads,
/// query Agent Mail, inspect git state, or store raw logs.
#[must_use]
pub fn attribute_compile_blocker(
    diagnostic_excerpt: &str,
    snapshot: &OwnershipSnapshot,
    as_of: DateTime<Utc>,
) -> CompileBlockerAttributionReport {
    let Some(diagnostic) = parse_first_rust_compile_diagnostic(diagnostic_excerpt) else {
        return CompileBlockerAttributionReport {
            schema: COMPILE_BLOCKER_ATTRIBUTION_SCHEMA_V1.to_owned(),
            status: CompileBlockerAttributionStatus::Unparsed,
            diagnostic: None,
            owner_candidates: Vec::new(),
            fallback_code: Some(UNATTRIBUTED_COMPILE_BLOCKER_CODE.to_owned()),
            suggested_commands: vec![
                "rerun with a concise Rust compiler diagnostic excerpt".to_owned(),
            ],
        };
    };

    attribute_compile_blocker_diagnostic(diagnostic, snapshot, as_of)
}

#[must_use]
pub fn attribute_compile_blocker_diagnostic(
    diagnostic: RustCompileDiagnostic,
    snapshot: &OwnershipSnapshot,
    as_of: DateTime<Utc>,
) -> CompileBlockerAttributionReport {
    let candidates = ownership_candidates_for_path(snapshot, &diagnostic.path, as_of)
        .into_iter()
        .filter(|candidate| !candidate.expired)
        .map(|candidate| compile_blocker_owner_candidate(&candidate, &diagnostic))
        .collect::<Vec<_>>();

    if candidates.is_empty() {
        let diagnostic_path = diagnostic.path.clone();
        return CompileBlockerAttributionReport {
            schema: COMPILE_BLOCKER_ATTRIBUTION_SCHEMA_V1.to_owned(),
            status: CompileBlockerAttributionStatus::Unattributed,
            diagnostic: Some(diagnostic),
            owner_candidates: Vec::new(),
            fallback_code: Some(UNATTRIBUTED_COMPILE_BLOCKER_CODE.to_owned()),
            suggested_commands: vec![
                "br list --status in_progress --json".to_owned(),
                format!("bv --robot-file-beads {diagnostic_path}"),
                "search Agent Mail for the source path before editing".to_owned(),
            ],
        };
    }

    CompileBlockerAttributionReport {
        schema: COMPILE_BLOCKER_ATTRIBUTION_SCHEMA_V1.to_owned(),
        status: CompileBlockerAttributionStatus::Attributed,
        diagnostic: Some(diagnostic),
        owner_candidates: candidates,
        fallback_code: None,
        suggested_commands: Vec::new(),
    }
}

#[must_use]
pub fn ownership_candidates_for_path(
    snapshot: &OwnershipSnapshot,
    path: &str,
    as_of: DateTime<Utc>,
) -> Vec<OwnershipCandidate> {
    let mut candidates = Vec::new();

    for reservation in &snapshot.reservations {
        if !pattern_matches_path(&reservation.path_pattern, path) {
            continue;
        }
        candidates.push(OwnershipCandidate {
            owner: reservation.holder_agent.clone(),
            source: OwnershipCandidateSource::AgentMailReservation,
            path_pattern: reservation.path_pattern.clone(),
            bead_id: reservation.bead_id.clone(),
            thread_id: reservation.thread_id.clone(),
            expires_at: reservation.expires_at.clone(),
            exclusive: reservation.exclusive,
            expired: reservation_is_expired_at(reservation, as_of),
            exact_match: reservation.path_pattern == path,
            pattern_specificity: path_pattern_specificity(&reservation.path_pattern),
            provenance: reservation.provenance.clone(),
        });
    }

    for bead in &snapshot.beads {
        let Some(owner) = bead.assignee.as_ref() else {
            continue;
        };
        if !bead_status_is_active(&bead.status) {
            continue;
        }
        for pattern in &bead.file_patterns {
            if !pattern_matches_path(pattern, path) {
                continue;
            }
            candidates.push(OwnershipCandidate {
                owner: owner.clone(),
                source: OwnershipCandidateSource::BeadsAssignee,
                path_pattern: pattern.clone(),
                bead_id: Some(bead.bead_id.clone()),
                thread_id: None,
                expires_at: None,
                exclusive: false,
                expired: false,
                exact_match: pattern == path,
                pattern_specificity: path_pattern_specificity(pattern),
                provenance: bead.provenance.clone(),
            });
        }
    }

    candidates.sort_by(compare_candidates);
    candidates
}

#[must_use]
pub fn pattern_matches_path(pattern: &str, path: &str) -> bool {
    if pattern == path {
        return true;
    }
    // Normalize ** (and degenerate *** etc.) to *.
    // This makes common recursive reservation patterns from Agent Mail
    // ("src/**", "src/graph/**") and Beads work correctly with the minimal
    // byte-level glob matcher (which already crosses '/' for *).
    // We do not change the glob language itself (used by tripwire preflight)
    // because that must stay small and deterministic.
    let mut normalized = pattern.replace("**", "*");
    // A second replace handles degenerate cases like "src/***" (becomes "src/**" then "src/*").
    // This is sufficient for any realistic file path patterns.
    if normalized.contains("**") {
        normalized = normalized.replace("**", "*");
    }
    glob_match(&normalized, path)
}

#[must_use]
pub fn path_pattern_specificity(pattern: &str) -> usize {
    pattern
        .chars()
        .filter(|ch| !matches!(ch, '*' | '?' | '[' | ']' | '{' | '}' | ','))
        .count()
}

#[must_use]
pub fn reservation_is_expired_at(
    reservation: &OwnershipReservationSnapshot,
    as_of: DateTime<Utc>,
) -> bool {
    reservation
        .expires_at
        .as_deref()
        .and_then(parse_rfc3339_utc)
        .is_some_and(|expires_at| expires_at <= as_of)
}

#[must_use]
pub fn bead_status_is_active(status: &str) -> bool {
    !matches!(
        status,
        "closed" | "done" | "resolved" | "cancelled" | "canceled"
    )
}

#[must_use]
pub fn parse_first_rust_compile_diagnostic(excerpt: &str) -> Option<RustCompileDiagnostic> {
    let mut current_error_code = None;
    let mut current_message = None;

    for line in excerpt.lines() {
        let trimmed = strip_ansi_codes(line).trim().to_owned();
        if trimmed.starts_with("error") {
            let (code, message) = parse_rust_error_header(&trimmed);
            current_error_code = code;
            current_message = message;
            continue;
        }

        let Some(location) = trimmed.strip_prefix("-->").map(str::trim) else {
            continue;
        };
        let Some((path, line, column)) = parse_rust_location(location) else {
            continue;
        };
        return Some(RustCompileDiagnostic {
            path,
            line,
            column,
            error_code: current_error_code.clone(),
            message: current_message
                .clone()
                .unwrap_or_else(|| "Rust compiler diagnostic".to_owned()),
        });
    }

    None
}

fn dirty_status_for_path(snapshot: &OwnershipSnapshot, path: &str) -> Option<String> {
    snapshot
        .dirty_files
        .iter()
        .find(|record| record.path == path)
        .map(|record| record.status.clone())
}

fn parse_rfc3339_utc(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .ok()
}

fn compile_blocker_owner_candidate(
    candidate: &OwnershipCandidate,
    diagnostic: &RustCompileDiagnostic,
) -> CompileBlockerOwnerCandidate {
    let confidence = compile_blocker_confidence(candidate);
    let evidence = vec![
        format!("diagnostic_path={}", diagnostic.path),
        format!("matched_pattern={}", candidate.path_pattern),
        format!("source={:?}", candidate.source),
    ];
    CompileBlockerOwnerCandidate {
        owner: candidate.owner.clone(),
        source: candidate.source.clone(),
        confidence: confidence.to_owned(),
        evidence,
        expires_at: candidate.expires_at.clone(),
        bead_id: candidate.bead_id.clone(),
        thread_id: candidate.thread_id.clone(),
        recommended_action: recommended_compile_blocker_action(candidate),
    }
}

fn compile_blocker_confidence(candidate: &OwnershipCandidate) -> &'static str {
    match (
        &candidate.source,
        candidate.exact_match,
        candidate.exclusive,
        candidate.expired,
    ) {
        (_, _, _, true) => "low",
        (OwnershipCandidateSource::AgentMailReservation, true, true, false) => "high",
        (OwnershipCandidateSource::AgentMailReservation, _, true, false) => "medium",
        (OwnershipCandidateSource::BeadsAssignee, true, _, false) => "medium",
        _ => "low",
    }
}

fn recommended_compile_blocker_action(candidate: &OwnershipCandidate) -> String {
    match &candidate.source {
        OwnershipCandidateSource::AgentMailReservation => {
            if let Some(thread_id) = &candidate.thread_id {
                format!(
                    "message {} in Agent Mail thread {}",
                    candidate.owner, thread_id
                )
            } else {
                format!(
                    "message {} before editing {}",
                    candidate.owner, candidate.path_pattern
                )
            }
        }
        OwnershipCandidateSource::BeadsAssignee => {
            if let Some(bead_id) = &candidate.bead_id {
                format!("coordinate on Bead {bead_id} before editing")
            } else {
                format!("coordinate with {} before editing", candidate.owner)
            }
        }
    }
}

fn parse_rust_error_header(header: &str) -> (Option<String>, Option<String>) {
    let Some((prefix, message)) = header.split_once(':') else {
        return (None, Some(header.to_owned()));
    };
    let code = prefix
        .split_once('[')
        .and_then(|(_, rest)| rest.split_once(']').map(|(code, _)| code.to_owned()));
    (code, Some(message.trim().to_owned()))
}

fn parse_rust_location(location: &str) -> Option<(String, Option<u32>, Option<u32>)> {
    let location = location.split_whitespace().next()?;
    let mut parts = location.rsplitn(3, ':').collect::<Vec<_>>();
    if parts.len() < 3 {
        return Some((location.to_owned(), None, None));
    }
    let column = parts[0].parse::<u32>().ok();
    let line = parts[1].parse::<u32>().ok();
    parts.reverse();
    Some((parts[0].to_owned(), line, column))
}

fn strip_ansi_codes(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for code_ch in chars.by_ref() {
                if code_ch.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        output.push(ch);
    }
    output
}

fn has_active_owner_conflict(candidates: &[OwnershipCandidate]) -> bool {
    let active = candidates
        .iter()
        .filter(|candidate| !candidate.expired)
        .collect::<Vec<_>>();
    if active.len() < 2 {
        return false;
    }

    // Conflict = multiple distinct active owners on the same path.
    // This is the correct safety signal for agent coordination (swarm brief,
    // status file-surface risks, compile blocker attribution, etc.).
    //
    // Previous logic only flagged when an *exclusive* claim existed. That was
    // too narrow once advisory (non-exclusive) Beads file_patterns were added.
    // Two agents with overlapping non-exclusive claims can still cause
    // concurrent unsafe edits → lost work.
    //
    // We deliberately err on the side of reporting conflict (better to force
    // coordination than to silently allow races). A future refinement can
    // distinguish "exclusive conflict" (high severity) vs "advisory overlap"
    // (medium severity) if the data model needs it.
    let distinct_owners: std::collections::BTreeSet<_> = active.iter().map(|c| &c.owner).collect();
    distinct_owners.len() > 1
}

fn compare_candidates(left: &OwnershipCandidate, right: &OwnershipCandidate) -> Ordering {
    right
        .exact_match
        .cmp(&left.exact_match)
        .then_with(|| left.expired.cmp(&right.expired))
        .then_with(|| right.pattern_specificity.cmp(&left.pattern_specificity))
        .then_with(|| right.exclusive.cmp(&left.exclusive))
        .then_with(|| {
            candidate_source_rank(&left.source).cmp(&candidate_source_rank(&right.source))
        })
        .then_with(|| left.owner.cmp(&right.owner))
        .then_with(|| left.path_pattern.cmp(&right.path_pattern))
        .then_with(|| left.bead_id.cmp(&right.bead_id))
        .then_with(|| left.thread_id.cmp(&right.thread_id))
}

fn candidate_source_rank(source: &OwnershipCandidateSource) -> u8 {
    match source {
        OwnershipCandidateSource::AgentMailReservation => 0,
        OwnershipCandidateSource::BeadsAssignee => 1,
    }
}

fn compare_reservations(
    left: &OwnershipReservationSnapshot,
    right: &OwnershipReservationSnapshot,
) -> Ordering {
    left.path_pattern
        .cmp(&right.path_pattern)
        .then_with(|| left.holder_agent.cmp(&right.holder_agent))
        .then_with(|| right.exclusive.cmp(&left.exclusive))
        .then_with(|| left.expires_at.cmp(&right.expires_at))
        .then_with(|| left.bead_id.cmp(&right.bead_id))
        .then_with(|| left.thread_id.cmp(&right.thread_id))
        .then_with(|| left.provenance.cmp(&right.provenance))
}

fn compare_beads(left: &BeadOwnershipRecord, right: &BeadOwnershipRecord) -> Ordering {
    left.bead_id
        .cmp(&right.bead_id)
        .then_with(|| left.assignee.cmp(&right.assignee))
        .then_with(|| left.status.cmp(&right.status))
        .then_with(|| left.title.cmp(&right.title))
        .then_with(|| left.labels.cmp(&right.labels))
        .then_with(|| left.file_patterns.cmp(&right.file_patterns))
        .then_with(|| left.provenance.cmp(&right.provenance))
}

fn compare_dirty_files(left: &DirtyFileRecord, right: &DirtyFileRecord) -> Ordering {
    left.path
        .cmp(&right.path)
        .then_with(|| left.status.cmp(&right.status))
        .then_with(|| left.provenance.cmp(&right.provenance))
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};

    use super::*;

    fn as_of() -> DateTime<Utc> {
        match DateTime::parse_from_rfc3339("2026-05-15T06:30:00Z") {
            Ok(timestamp) => timestamp.with_timezone(&Utc),
            Err(error) => panic!("fixture timestamp must parse: {error}"),
        }
    }

    fn provenance(source_id: &str) -> OwnershipProvenance {
        OwnershipProvenance {
            source_kind: "fixture".to_owned(),
            source_id: source_id.to_owned(),
            content_hash: format!("blake3:{source_id}"),
        }
    }

    fn sample_snapshot() -> OwnershipSnapshot {
        OwnershipSnapshot {
            schema: OWNERSHIP_SNAPSHOT_SCHEMA_V1.to_owned(),
            generated_at: "2026-05-15T06:20:00Z".to_owned(),
            reservations: vec![
                OwnershipReservationSnapshot {
                    path_pattern: "src/core/ownership_snapshot.rs".to_owned(),
                    holder_agent: "NobleBasin".to_owned(),
                    exclusive: true,
                    expires_at: Some("2026-05-15T08:23:03Z".to_owned()),
                    bead_id: Some("bd-1zb7k.16.1".to_owned()),
                    thread_id: Some("bd-1zb7k.16.1".to_owned()),
                    provenance: provenance("reservation-1"),
                },
                OwnershipReservationSnapshot {
                    path_pattern: "src/core/*.rs".to_owned(),
                    holder_agent: "OtherAgent".to_owned(),
                    exclusive: false,
                    expires_at: Some("2026-05-15T05:00:00Z".to_owned()),
                    bead_id: Some("bd-expired".to_owned()),
                    thread_id: Some("bd-expired".to_owned()),
                    provenance: provenance("reservation-2"),
                },
            ],
            beads: vec![BeadOwnershipRecord {
                bead_id: "bd-1zb7k.16.1".to_owned(),
                title: "Ownership snapshot model".to_owned(),
                status: "in_progress".to_owned(),
                assignee: Some("NobleBasin".to_owned()),
                labels: vec!["coordination".to_owned()],
                file_patterns: vec!["src/core/ownership*.rs".to_owned()],
                provenance: provenance("bead-1"),
            }],
            dirty_files: vec![DirtyFileRecord {
                path: "src/core/ownership_snapshot.rs".to_owned(),
                status: "modified".to_owned(),
                provenance: provenance("dirty-1"),
            }],
        }
    }

    #[test]
    fn path_report_ranks_exact_current_reservation_before_globs_and_expired_records() {
        let report = ownership_report_for_path(
            &sample_snapshot(),
            "src/core/ownership_snapshot.rs",
            as_of(),
        );

        assert_eq!(report.schema, OWNERSHIP_FILE_REPORT_SCHEMA_V1);
        assert_eq!(report.dirty_status.as_deref(), Some("modified"));
        assert!(!report.conflict);
        assert_eq!(report.candidates.len(), 3);
        assert_eq!(report.candidates[0].owner, "NobleBasin");
        assert_eq!(
            report.candidates[0].source,
            OwnershipCandidateSource::AgentMailReservation
        );
        assert!(report.candidates[0].exact_match);
        assert!(!report.candidates[0].expired);
        assert_eq!(
            report.candidates[1].source,
            OwnershipCandidateSource::BeadsAssignee
        );
        assert_eq!(report.candidates[2].owner, "OtherAgent");
        assert!(report.candidates[2].expired);
    }

    #[test]
    fn closed_beads_do_not_create_current_ownership_candidates() {
        let mut snapshot = sample_snapshot();
        snapshot.beads[0].status = "closed".to_owned();

        let candidates =
            ownership_candidates_for_path(&snapshot, "src/core/ownership_snapshot.rs", as_of());

        assert_eq!(candidates.len(), 2);
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.source == OwnershipCandidateSource::AgentMailReservation)
        );
    }

    #[test]
    fn dirty_file_reports_are_sorted_by_path() {
        let mut snapshot = sample_snapshot();
        snapshot.dirty_files.push(DirtyFileRecord {
            path: "README.md".to_owned(),
            status: "modified".to_owned(),
            provenance: provenance("dirty-2"),
        });

        let reports = dirty_file_reports(&snapshot, as_of());

        assert_eq!(reports[0].path, "README.md");
        assert_eq!(reports[1].path, "src/core/ownership_snapshot.rs");
    }

    #[test]
    fn snapshot_normalization_is_deterministic() {
        let mut snapshot = sample_snapshot();
        snapshot.reservations.reverse();
        snapshot.beads.push(BeadOwnershipRecord {
            bead_id: "bd-alpha".to_owned(),
            title: "Earlier bead".to_owned(),
            status: "open".to_owned(),
            assignee: Some("AlphaAgent".to_owned()),
            labels: Vec::new(),
            file_patterns: vec!["README.md".to_owned()],
            provenance: provenance("bead-alpha"),
        });
        snapshot.dirty_files.push(DirtyFileRecord {
            path: "README.md".to_owned(),
            status: "modified".to_owned(),
            provenance: provenance("dirty-2"),
        });

        let normalized = snapshot.normalized();

        assert_eq!(normalized.beads[0].bead_id, "bd-1zb7k.16.1");
        assert_eq!(normalized.beads[1].bead_id, "bd-alpha");
        assert_eq!(normalized.dirty_files[0].path, "README.md");
        assert_eq!(normalized.reservations[0].path_pattern, "src/core/*.rs");
    }

    // bd-iez98 — coverage for the generalized has_active_owner_conflict.
    //
    // The function previously only flagged conflict when at least one
    // active candidate carried `exclusive: true`. After the bd-iez98 fix,
    // it flags conflict whenever 2+ DISTINCT active (non-expired) owners
    // claim the same path, regardless of exclusivity, because two agents
    // with overlapping advisory claims can still race unsafe edits.
    //
    // These tests cover the happy / edge / error paths AGENTS.md L300-302
    // requires alongside the implementation (and also satisfy bd-3usjw.62
    // Rule 7's inline-test obligation for the file).

    fn candidate(owner: &str, exclusive: bool, expired: bool) -> OwnershipCandidate {
        OwnershipCandidate {
            owner: owner.to_owned(),
            source: OwnershipCandidateSource::AgentMailReservation,
            path_pattern: "src/core/example.rs".to_owned(),
            bead_id: Some("bd-iez98".to_owned()),
            thread_id: Some("bd-iez98".to_owned()),
            expires_at: Some("2026-05-15T08:23:03Z".to_owned()),
            exclusive,
            expired,
            exact_match: true,
            pattern_specificity: 100,
            provenance: provenance(&format!("candidate-{owner}")),
        }
    }

    #[test]
    fn has_active_owner_conflict_single_owner_is_not_a_conflict() {
        let only_one = vec![candidate("Solo", false, false)];
        assert!(!has_active_owner_conflict(&only_one));

        let only_exclusive = vec![candidate("Solo", true, false)];
        assert!(
            !has_active_owner_conflict(&only_exclusive),
            "a single exclusive owner is the normal case and must not flag conflict"
        );
    }

    #[test]
    fn has_active_owner_conflict_flags_two_distinct_advisory_owners() {
        // The regression bd-iez98 fixes: previously this returned `false`
        // because no candidate was exclusive. The post-fix expectation is
        // that two distinct advisory owners on the same path is enough to
        // demand explicit coordination.
        let advisory_overlap = vec![
            candidate("Alpha", false, false),
            candidate("Beta", false, false),
        ];
        assert!(
            has_active_owner_conflict(&advisory_overlap),
            "2+ distinct advisory owners on the same path must be flagged as conflict"
        );
    }

    #[test]
    fn has_active_owner_conflict_ignores_expired_candidates() {
        // An expired non-exclusive lease and a current advisory lease from
        // different agents must NOT flag conflict: the expired one is no
        // longer load-bearing for coordination.
        let one_expired = vec![
            candidate("Alpha", false, false),
            candidate("Beta", false, true),
        ];
        assert!(
            !has_active_owner_conflict(&one_expired),
            "expired candidates must be filtered out before counting distinct active owners"
        );

        // Even when both are expired, no conflict.
        let both_expired = vec![
            candidate("Alpha", true, true),
            candidate("Beta", true, true),
        ];
        assert!(!has_active_owner_conflict(&both_expired));
    }

    #[test]
    fn has_active_owner_conflict_treats_repeated_owner_as_single_holder() {
        // Two candidates from the SAME owner (e.g. one Agent Mail
        // reservation + one Beads assignee record) must not flag conflict.
        // The whole point of distinct-owner counting is to ignore the
        // collision between an agent's parallel coordination signals.
        let mut beads = candidate("Alpha", false, false);
        beads.source = OwnershipCandidateSource::BeadsAssignee;
        let same_owner_two_sources = vec![candidate("Alpha", true, false), beads];
        assert!(
            !has_active_owner_conflict(&same_owner_two_sources),
            "the same owner appearing twice (across sources) is one holder, not a conflict"
        );
    }

    #[test]
    fn has_active_owner_conflict_flags_mixed_exclusive_and_advisory_across_owners() {
        // The post-fix behavior is symmetric: any 2+ distinct active
        // owners flag conflict regardless of exclusivity mix. An exclusive
        // claim by Alpha plus an advisory claim by Beta is still a race
        // surface that the swarm brief should highlight.
        let mixed = vec![
            candidate("Alpha", true, false),
            candidate("Beta", false, false),
        ];
        assert!(
            has_active_owner_conflict(&mixed),
            "mixed exclusive+advisory across distinct active owners must flag conflict"
        );
    }

    #[test]
    fn has_active_owner_conflict_empty_input_is_not_a_conflict() {
        // Boundary: an empty candidate list (no reservations, no assignees)
        // is unambiguously not a conflict.
        let empty: Vec<OwnershipCandidate> = Vec::new();
        assert!(!has_active_owner_conflict(&empty));
    }
}
