//! Parser-focused coverage for redacted pack coordination snapshots.

use ee::models::RESPONSE_SCHEMA_V1;
use ee::pack::{
    COORDINATION_SNAPSHOT_SCHEMA_V1, DEFAULT_COORDINATION_STALE_AFTER_MS, PackCoordinationEntry,
    PackCoordinationSnapshot, PackCoordinationSource,
};
use serde_json::json;

type TestResult<T = ()> = Result<T, String>;

fn parse_snapshot(
    value: serde_json::Value,
    stale_after_ms: u64,
) -> TestResult<PackCoordinationSnapshot> {
    PackCoordinationSnapshot::from_json_str(&value.to_string(), stale_after_ms)
}

fn expect_parse_error(value: serde_json::Value) -> TestResult<String> {
    match PackCoordinationSnapshot::from_json_str(
        &value.to_string(),
        DEFAULT_COORDINATION_STALE_AFTER_MS,
    ) {
        Ok(snapshot) => Err(format!("expected parse error, got snapshot: {snapshot:?}")),
        Err(error) => Ok(error),
    }
}

fn source_by_id<'a>(
    snapshot: &'a PackCoordinationSnapshot,
    source_id: &str,
) -> TestResult<&'a PackCoordinationSource> {
    snapshot
        .sources
        .iter()
        .find(|source| source.source_id == source_id)
        .ok_or_else(|| format!("missing coordination source {source_id}"))
}

fn entry_by_id<'a>(
    source: &'a PackCoordinationSource,
    entry_id: &str,
) -> TestResult<&'a PackCoordinationEntry> {
    source
        .entries
        .iter()
        .find(|entry| entry.id == entry_id)
        .ok_or_else(|| format!("missing coordination entry {entry_id}"))
}

#[test]
fn coordination_snapshot_parser_rejects_bad_schema_and_missing_sources() -> TestResult {
    let bad_schema = expect_parse_error(json!({
        "schema": "ee.coordination_snapshot.v999",
        "sources": []
    }))?;
    if !bad_schema.contains("Unsupported coordination snapshot schema") {
        return Err(format!(
            "bad schema error should explain schema mismatch: {bad_schema}"
        ));
    }
    if !bad_schema.contains(COORDINATION_SNAPSHOT_SCHEMA_V1) {
        return Err(format!(
            "bad schema error should name expected schema: {bad_schema}"
        ));
    }

    let missing_sources = expect_parse_error(json!({
        "schema": COORDINATION_SNAPSHOT_SCHEMA_V1,
        "scope": "workspace"
    }))?;
    if !missing_sources.contains("missing sources[]") {
        return Err(format!(
            "missing sources error should name sources[]: {missing_sources}"
        ));
    }

    Ok(())
}

#[test]
fn coordination_snapshot_parser_unwraps_response_v1_and_accepts_camelcase_aliases() -> TestResult {
    let snapshot = parse_snapshot(
        json!({
            "schema": RESPONSE_SCHEMA_V1,
            "success": true,
            "data": {
                "schema": COORDINATION_SNAPSHOT_SCHEMA_V1,
                "capturedAt": "2026-05-23T03:30:00Z",
                "scope": "workspace",
                "sources": [{
                    "source": "beads_ready",
                    "sourceId": "br ready --json",
                    "freshnessMs": 123,
                    "lastSyncedAt": "2026-05-23T03:29:59Z",
                    "entries": [{
                        "beadId": "bd-active",
                        "status": "in_progress",
                        "title": "Active parser bead",
                        "provenance": ["br://ready", "bv://triage"]
                    }]
                }]
            }
        }),
        DEFAULT_COORDINATION_STALE_AFTER_MS,
    )?;

    if snapshot.schema != COORDINATION_SNAPSHOT_SCHEMA_V1 {
        return Err(format!("unexpected snapshot schema: {}", snapshot.schema));
    }
    if snapshot.captured_at.as_deref() != Some("2026-05-23T03:30:00Z") {
        return Err(format!("capturedAt alias was not parsed: {snapshot:?}"));
    }
    if snapshot.summary.in_progress_bead_count != 1 {
        return Err(format!(
            "expected one in-progress bead, got {}",
            snapshot.summary.in_progress_bead_count
        ));
    }
    if snapshot.notable_entries().len() != 1 {
        return Err(format!(
            "expected one notable in-progress entry, got {}",
            snapshot.notable_entries().len()
        ));
    }

    let source = source_by_id(&snapshot, "br ready --json")?;
    if source.kind != "beads_ready" || source.freshness_ms != Some(123) {
        return Err(format!(
            "source aliases were not parsed correctly: {source:?}"
        ));
    }
    let entry = entry_by_id(source, "bd-active")?;
    if entry.kind != "bead" {
        return Err(format!(
            "beads source should infer bead entry kind: {entry:?}"
        ));
    }
    if entry.summary != "Active parser bead" {
        return Err(format!("title alias should become summary: {entry:?}"));
    }
    if entry.provenance != ["br://ready", "bv://triage"] {
        return Err(format!("array provenance was not preserved: {entry:?}"));
    }

    Ok(())
}

#[test]
fn coordination_snapshot_parser_marks_all_stale_signal_variants() -> TestResult {
    let stale_after_ms = 5_000;
    let snapshot = parse_snapshot(
        json!({
            "schema": COORDINATION_SNAPSHOT_SCHEMA_V1,
            "sources": [
                {"kind": "agent_mail", "id": "explicit", "stale": true},
                {"kind": "agent_mail", "id": "status", "status": "stale"},
                {"kind": "agent_mail", "id": "age_ms", "freshness": {"age_ms": 5_001}},
                {"kind": "agent_mail", "id": "age_seconds", "freshness": {"ageSeconds": 6}},
                {"kind": "agent_mail", "id": "fresh", "freshness": {"age_seconds": 4}}
            ]
        }),
        stale_after_ms,
    )?;

    if snapshot.freshness.status != "stale" {
        return Err(format!("overall freshness should be stale: {snapshot:?}"));
    }
    if snapshot.summary.stale_source_count != 4 {
        return Err(format!(
            "expected four stale sources, got {}",
            snapshot.summary.stale_source_count
        ));
    }

    for source_id in ["explicit", "status", "age_ms", "age_seconds"] {
        let source = source_by_id(&snapshot, source_id)?;
        if !source.stale {
            return Err(format!("{source_id} should be stale: {source:?}"));
        }
    }
    let fresh = source_by_id(&snapshot, "fresh")?;
    if fresh.stale || fresh.status != "fresh" || fresh.freshness_ms != Some(4_000) {
        return Err(format!(
            "fresh source should remain fresh with seconds converted: {fresh:?}"
        ));
    }

    Ok(())
}

#[test]
fn coordination_snapshot_parser_infers_entries_summaries_severity_and_provenance() -> TestResult {
    let snapshot = parse_snapshot(
        json!({
            "schema": COORDINATION_SNAPSHOT_SCHEMA_V1,
            "sources": [
                {
                    "kind": "file_reservation_snapshot",
                    "source_id": "mail-reservations",
                    "entries": [
                        {
                            "pathPattern": "src/pack/**",
                            "holder": "BlueLake",
                            "exclusive": true,
                            "conflict": true
                        },
                        {
                            "path_pattern": "README.md",
                            "agent_name": "GreenField",
                            "exclusive": false,
                            "provenance": "agent-mail://reservation/2"
                        },
                        {
                            "path": "docs/**",
                            "exclusive": true
                        }
                    ]
                },
                {
                    "kind": "mail_threads",
                    "id": "mail",
                    "entries": [{
                        "threadId": "bd-vqant",
                        "subject": "Parser test thread",
                        "provenance": ["agent-mail://thread/bd-vqant"]
                    }]
                }
            ]
        }),
        DEFAULT_COORDINATION_STALE_AFTER_MS,
    )?;

    if snapshot.summary.active_reservation_count != 3 {
        return Err(format!(
            "expected three inferred file reservations, got {}",
            snapshot.summary.active_reservation_count
        ));
    }
    if snapshot.summary.active_conflict_count != 1 {
        return Err(format!(
            "expected one active conflict, got {}",
            snapshot.summary.active_conflict_count
        ));
    }
    if snapshot.active_conflict_entries().len() != 1 {
        return Err(format!(
            "expected one active conflict entry, got {}",
            snapshot.active_conflict_entries().len()
        ));
    }

    let reservations = source_by_id(&snapshot, "mail-reservations")?;
    let exclusive_with_holder = entry_by_id(reservations, "src/pack/**")?;
    if exclusive_with_holder.kind != "file_reservation" {
        return Err(format!(
            "reservation source should infer file_reservation kind: {exclusive_with_holder:?}"
        ));
    }
    if exclusive_with_holder.severity != "warning" {
        return Err(format!(
            "conflict without severity should default warning: {exclusive_with_holder:?}"
        ));
    }
    if exclusive_with_holder.summary != "exclusive reservation on src/pack/** held by BlueLake" {
        return Err(format!(
            "exclusive holder summary drifted: {exclusive_with_holder:?}"
        ));
    }
    if exclusive_with_holder.provenance != ["mail-reservations"] {
        return Err(format!(
            "missing provenance should default to source id: {exclusive_with_holder:?}"
        ));
    }

    let shared_with_holder = entry_by_id(reservations, "README.md")?;
    if shared_with_holder.summary != "reservation on README.md held by GreenField" {
        return Err(format!(
            "shared holder summary drifted: {shared_with_holder:?}"
        ));
    }
    if shared_with_holder.provenance != ["agent-mail://reservation/2"] {
        return Err(format!(
            "string provenance should become one item: {shared_with_holder:?}"
        ));
    }

    let exclusive_without_holder = entry_by_id(reservations, "docs/**")?;
    if exclusive_without_holder.summary != "exclusive reservation on docs/**" {
        return Err(format!(
            "exclusive no-holder summary drifted: {exclusive_without_holder:?}"
        ));
    }

    let mail = source_by_id(&snapshot, "mail")?;
    let thread = entry_by_id(mail, "bd-vqant")?;
    if thread.kind != "agent_mail_thread" || thread.summary != "Parser test thread" {
        return Err(format!("mail thread inference drifted: {thread:?}"));
    }

    Ok(())
}
