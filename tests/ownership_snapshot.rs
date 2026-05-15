#![forbid(unsafe_code)]

use chrono::{DateTime, Utc};
use ee::core::ownership_snapshot::{
    COMPILE_BLOCKER_ATTRIBUTION_SCHEMA_V1, CompileBlockerAttributionStatus,
    OWNERSHIP_FILE_REPORT_SCHEMA_V1, OWNERSHIP_SNAPSHOT_SCHEMA_V1, OwnershipCandidateSource,
    OwnershipReservationSnapshot, OwnershipSnapshot, RustCompileDiagnostic,
    attribute_compile_blocker, attribute_compile_blocker_diagnostic, ownership_report_for_path,
};
use insta::assert_json_snapshot;
use serde_json::json;

type TestResult<T = ()> = Result<T, String>;

fn fixture_snapshot() -> TestResult<OwnershipSnapshot> {
    let raw = include_str!("fixtures/ownership/ownership_snapshot.json");
    serde_json::from_str(raw).map_err(|error| format!("parse ownership snapshot fixture: {error}"))
}

fn as_of() -> DateTime<Utc> {
    match DateTime::parse_from_rfc3339("2026-05-15T06:30:00Z") {
        Ok(timestamp) => timestamp.with_timezone(&Utc),
        Err(error) => panic!("fixture timestamp must parse: {error}"),
    }
}

#[test]
fn fixture_round_trips_without_raw_coordination_payloads() -> TestResult {
    let snapshot = fixture_snapshot()?;

    if snapshot.schema != OWNERSHIP_SNAPSHOT_SCHEMA_V1 {
        return Err(format!("unexpected schema {}", snapshot.schema));
    }
    let encoded = serde_json::to_string(&snapshot)
        .map_err(|error| format!("serialize ownership snapshot: {error}"))?;
    let decoded: OwnershipSnapshot = serde_json::from_str(&encoded)
        .map_err(|error| format!("deserialize ownership snapshot: {error}"))?;

    if decoded != snapshot {
        return Err("ownership snapshot fixture is not stable across JSON round trip".to_owned());
    }
    for forbidden in ["body_md", "SECRET", "fileContents", "envDump"] {
        if encoded.contains(forbidden) {
            return Err(format!(
                "snapshot leaks forbidden payload marker {forbidden}"
            ));
        }
    }
    Ok(())
}

#[test]
fn fixture_path_report_prefers_current_exact_reservation() -> TestResult {
    let snapshot = fixture_snapshot()?;
    let report = ownership_report_for_path(&snapshot, "src/core/ownership_snapshot.rs", as_of());

    if report.schema != OWNERSHIP_FILE_REPORT_SCHEMA_V1 {
        return Err(format!("unexpected report schema {}", report.schema));
    }
    if report.dirty_status.as_deref() != Some("modified") {
        return Err(format!("unexpected dirty status {:?}", report.dirty_status));
    }
    let Some(first) = report.candidates.first() else {
        return Err("expected ownership candidate".to_owned());
    };
    if first.owner != "NobleBasin"
        || first.source != OwnershipCandidateSource::AgentMailReservation
        || !first.exact_match
        || first.expired
    {
        return Err(format!("unexpected first candidate {first:?}"));
    }
    if !report
        .candidates
        .iter()
        .any(|candidate| candidate.source == OwnershipCandidateSource::BeadsAssignee)
    {
        return Err("expected Beads assignee candidate".to_owned());
    }
    Ok(())
}

#[test]
fn compile_blocker_attribution_prefers_active_reservation_holder() -> TestResult {
    let mut snapshot = fixture_snapshot()?;
    snapshot.reservations.push(OwnershipReservationSnapshot {
        path_pattern: "src/graph/hits.rs".to_owned(),
        holder_agent: "NobleStork".to_owned(),
        exclusive: true,
        expires_at: Some("2026-05-15T10:16:11Z".to_owned()),
        bead_id: Some("bd-1o99v".to_owned()),
        thread_id: Some("bd-1o99v".to_owned()),
        provenance: ee::core::ownership_snapshot::OwnershipProvenance {
            source_kind: "agent_mail_reservation".to_owned(),
            source_id: "reservation-543".to_owned(),
            content_hash: "blake3:reservation543".to_owned(),
        },
    });
    let excerpt = r#"
error[E0603]: constant `HITS_DEFAULT_MAX_ITERATIONS` is private
  --> src/graph/hits.rs:25:22
25 | use fnx_algorithms::{HITS_DEFAULT_MAX_ITERATIONS, HitsCentralityResult, hits_centrality_directed};
"#;

    let report = attribute_compile_blocker(excerpt, &snapshot, as_of());

    if report.schema != COMPILE_BLOCKER_ATTRIBUTION_SCHEMA_V1 {
        return Err(format!("unexpected schema {}", report.schema));
    }
    if report.status != CompileBlockerAttributionStatus::Attributed {
        return Err(format!("unexpected status {:?}", report.status));
    }
    let diagnostic = report
        .diagnostic
        .as_ref()
        .ok_or_else(|| "expected parsed diagnostic".to_owned())?;
    if diagnostic.path != "src/graph/hits.rs"
        || diagnostic.line != Some(25)
        || diagnostic.column != Some(22)
        || diagnostic.error_code.as_deref() != Some("E0603")
    {
        return Err(format!("unexpected diagnostic {diagnostic:?}"));
    }
    let owner = report
        .owner_candidates
        .first()
        .ok_or_else(|| "expected owner candidate".to_owned())?;
    if owner.owner != "NobleStork"
        || owner.source != OwnershipCandidateSource::AgentMailReservation
        || owner.confidence != "high"
        || owner.expires_at.as_deref() != Some("2026-05-15T10:16:11Z")
    {
        return Err(format!("unexpected owner candidate {owner:?}"));
    }
    if report.fallback_code.is_some() || !report.suggested_commands.is_empty() {
        return Err(format!("unexpected fallback guidance {report:?}"));
    }
    Ok(())
}

#[test]
fn compile_blocker_attribution_falls_back_for_unreserved_path() -> TestResult {
    let snapshot = fixture_snapshot()?;
    let excerpt = r#"
error[E0432]: unresolved import `crate::missing`
 --> src/graph/unowned.rs:9:5
"#;

    let report = attribute_compile_blocker(excerpt, &snapshot, as_of());

    if report.status != CompileBlockerAttributionStatus::Unattributed {
        return Err(format!("unexpected status {:?}", report.status));
    }
    if !report.owner_candidates.is_empty() {
        return Err(format!(
            "unowned path should not produce candidates: {:?}",
            report.owner_candidates
        ));
    }
    if report.fallback_code.as_deref() != Some("unattributed_compile_blocker") {
        return Err(format!("unexpected fallback {:?}", report.fallback_code));
    }
    if !report
        .suggested_commands
        .iter()
        .any(|command| command == "bv --robot-file-beads src/graph/unowned.rs")
    {
        return Err(format!(
            "fallback must suggest file-bead lookup, got {:?}",
            report.suggested_commands
        ));
    }
    Ok(())
}

#[test]
fn compile_blocker_attribution_ignores_expired_reservation_for_active_bead() -> TestResult {
    let mut snapshot = fixture_snapshot()?;
    snapshot.reservations.push(OwnershipReservationSnapshot {
        path_pattern: "src/core/ownership_snapshot.rs".to_owned(),
        holder_agent: "ExpiredAgent".to_owned(),
        exclusive: true,
        expires_at: Some("2026-05-15T05:00:00Z".to_owned()),
        bead_id: Some("bd-expired".to_owned()),
        thread_id: Some("bd-expired".to_owned()),
        provenance: ee::core::ownership_snapshot::OwnershipProvenance {
            source_kind: "agent_mail_reservation".to_owned(),
            source_id: "reservation-expired-specific".to_owned(),
            content_hash: "blake3:reservationexpiredspecific".to_owned(),
        },
    });
    let excerpt = r#"
error[E0425]: cannot find value `snapshot` in this scope
 --> src/core/ownership_snapshot.rs:112:9
"#;

    let report = attribute_compile_blocker(excerpt, &snapshot, as_of());

    if report.status != CompileBlockerAttributionStatus::Attributed {
        return Err(format!("unexpected status {:?}", report.status));
    }
    if report
        .owner_candidates
        .iter()
        .any(|candidate| candidate.owner == "ExpiredAgent")
    {
        return Err(format!(
            "expired reservation must be ignored, got {:?}",
            report.owner_candidates
        ));
    }
    if !report.owner_candidates.iter().any(|candidate| {
        candidate.owner == "NobleBasin"
            && candidate.source == OwnershipCandidateSource::BeadsAssignee
    }) {
        return Err(format!(
            "active bead assignee must remain a candidate, got {:?}",
            report.owner_candidates
        ));
    }
    Ok(())
}

#[test]
fn compile_blocker_attribution_accepts_structured_diagnostic() -> TestResult {
    let snapshot = fixture_snapshot()?;
    let diagnostic = RustCompileDiagnostic {
        path: "src/core/ownership_snapshot.rs".to_owned(),
        line: Some(112),
        column: Some(9),
        error_code: Some("E0425".to_owned()),
        message: "cannot find value `snapshot` in this scope".to_owned(),
    };

    let report = attribute_compile_blocker_diagnostic(diagnostic, &snapshot, as_of());

    if report.status != CompileBlockerAttributionStatus::Attributed {
        return Err(format!("unexpected status {:?}", report.status));
    }
    if !report
        .owner_candidates
        .iter()
        .any(|candidate| candidate.owner == "NobleBasin")
    {
        return Err(format!(
            "structured diagnostic should attribute to NobleBasin, got {:?}",
            report.owner_candidates
        ));
    }
    Ok(())
}

#[test]
fn compile_blocker_attribution_pins_golden_ordering_and_repairs() -> TestResult {
    let snapshot = fixture_snapshot()?;
    let attributed_diagnostic = RustCompileDiagnostic {
        path: "src/core/ownership_snapshot.rs".to_owned(),
        line: Some(112),
        column: Some(9),
        error_code: Some("E0425".to_owned()),
        message: "cannot find value `snapshot` in this scope".to_owned(),
    };
    let unattributed_excerpt = r#"
error[E0432]: unresolved import `crate::missing`
 --> src/graph/unowned.rs:9:5
"#;

    let attributed =
        attribute_compile_blocker_diagnostic(attributed_diagnostic, &snapshot, as_of());
    let unattributed = attribute_compile_blocker(unattributed_excerpt, &snapshot, as_of());

    if attributed.status != CompileBlockerAttributionStatus::Attributed {
        return Err(format!(
            "unexpected attributed status {:?}",
            attributed.status
        ));
    }
    let Some(first_owner) = attributed.owner_candidates.first() else {
        return Err("expected attributed owner candidate".to_owned());
    };
    if first_owner.source != OwnershipCandidateSource::AgentMailReservation {
        return Err(format!(
            "reservation candidate must sort first, got {first_owner:?}"
        ));
    }
    if unattributed.status != CompileBlockerAttributionStatus::Unattributed {
        return Err(format!(
            "unexpected unattributed status {:?}",
            unattributed.status
        ));
    }
    if !unattributed
        .suggested_commands
        .iter()
        .any(|command| command == "bv --robot-file-beads src/graph/unowned.rs")
    {
        return Err(format!(
            "unattributed fallback must include file-bead lookup, got {:?}",
            unattributed.suggested_commands
        ));
    }

    let value = json!({
        "attributed": attributed,
        "unattributed": unattributed,
    });
    assert_json_snapshot!("compile_blocker_attribution", value);
    Ok(())
}
