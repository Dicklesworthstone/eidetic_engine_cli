//! Black-box incident-card lifecycle (ADR 0091, bd-reality-core-convergence-1azkt.59).
//!
//! A real `ee` binary imports one Claude Code transcript through a stub `cass`
//! (a failing `cargo build`, the turn that explains the fix, a passing
//! `cargo test`), then: the pack carries the derived incident card in place of
//! the raw repair turn, `diagnose-error` recalls the card first, `ee why` names
//! the card's sources, and re-importing the session (the refresh path, with the
//! card present) succeeds and adds nothing.
//! A second fixture keeps two different repairs for one error class and
//! distinct qualified source facts through persisted packing and replay.
#![cfg(unix)]

use super::isolated_ee::isolated_ee_command;
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

type TestResult<T = ()> = Result<T, String>;

const STUB_CASS: &str = r#"#!/bin/sh
case "${1:-}" in
  index) printf '{"success":true,"conversations":1}\n' ;;
  sessions) cat "$CASS_STUB_SESSIONS_JSON" ;;
  view)
    shift; target=1; context=4
    while [ "$#" -gt 0 ]; do
      case "$1" in -n) target="$2"; shift 2 ;; -C) context="$2"; shift 2 ;; --) break ;; *) shift ;; esac
    done
    awk -v target="$target" -v context="$context" '
      BEGIN { printf "{\"lines\":["; separator="" }
      NR >= target-context && NR <= target+context { printf "%s%s", separator, $0; separator="," }
      END { printf "],\"total_lines\":%d}\n", NR }
    ' "$CASS_STUB_VIEW_JSONL" ;;
  *) printf 'unexpected cass stub command: %s\n' "$*" >&2; exit 64 ;;
esac
"#;

struct Fixture {
    _root: tempfile::TempDir,
    root: std::path::PathBuf,
    workspace: std::path::PathBuf,
    cass: std::path::PathBuf,
    sessions: std::path::PathBuf,
    view: std::path::PathBuf,
}

impl Fixture {
    fn new() -> TestResult<Self> {
        let temporary = tempfile::Builder::new()
            .prefix("ee-incident-card-cli-")
            .tempdir()
            .map_err(|error| error.to_string())?;
        let root = temporary
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let workspace = root.join("workspace");
        let bin = root.join("bin");
        for dir in [&workspace, &bin] {
            fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        }
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755))
            .map_err(|error| error.to_string())?;
        let cass = bin.join("cass");
        fs::write(&cass, STUB_CASS).map_err(|error| error.to_string())?;
        fs::set_permissions(&cass, fs::Permissions::from_mode(0o555))
            .map_err(|error| error.to_string())?;

        let transcript = [
            json!({"type": "assistant", "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "cargo build"}}]}}),
            json!({"type": "user", "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "is_error": true,
                 "content": "error[E0277]: the trait bound `Widget: Serialize` is not satisfied\n  --> src/widget.rs:41:9"}]}}),
            json!({"type": "assistant", "message": {"role": "assistant", "content": [
                {"type": "text", "text": "Let me check. The ledger store serializes Widget, so Widget needs to derive Serialize. I added the derive in src/widget.rs."}]}}),
            json!({"type": "assistant", "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_2", "name": "Bash", "input": {"command": "cargo test --lib"}}]}}),
            json!({"type": "user", "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_2", "is_error": false,
                 "content": "test result: ok. 3 passed; 0 failed"}]}}),
        ];
        let session_path = root.join("sessions/claude-session.jsonl");
        fs::create_dir_all(root.join("sessions")).map_err(|error| error.to_string())?;
        let mut session_text = String::new();
        let mut view_text = String::new();
        for (index, record) in transcript.iter().enumerate() {
            session_text.push_str(&record.to_string());
            session_text.push('\n');
            view_text
                .push_str(&json!({"line": index + 1, "content": record.to_string()}).to_string());
            view_text.push('\n');
        }
        fs::write(&session_path, session_text).map_err(|error| error.to_string())?;
        let view = root.join("view.jsonl");
        fs::write(&view, view_text).map_err(|error| error.to_string())?;
        let sessions = root.join("sessions.json");
        fs::write(
            &sessions,
            json!({"sessions": [{
                "path": session_path.to_string_lossy(),
                "agent": "claude_code",
                "workspace": workspace.to_string_lossy(),
                "started_at": "2026-10-01T10:00:00Z",
                "ended_at": "2026-10-01T10:05:00Z",
                "message_count": 5,
                "token_count": 100
            }]})
            .to_string(),
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            _root: temporary,
            root,
            workspace,
            cass,
            sessions,
            view,
        })
    }

    /// Replace authored upstream input before importing it. Both `cass view`
    /// and the source file describe the same complete ordered transcript.
    fn replace_transcript(&self, records: &[Value]) -> TestResult {
        let session_path = self.root.join("sessions/claude-session.jsonl");
        let mut source = String::new();
        let mut view = String::new();
        for (index, record) in records.iter().enumerate() {
            source.push_str(&record.to_string());
            source.push('\n');
            view.push_str(&json!({"line": index + 1, "content": record.to_string()}).to_string());
            view.push('\n');
        }
        fs::write(&session_path, source).map_err(|error| error.to_string())?;
        fs::write(&self.view, view).map_err(|error| error.to_string())?;
        fs::write(
            &self.sessions,
            json!({"sessions": [{
                "path": session_path.to_string_lossy(),
                "agent": "claude_code",
                "workspace": self.workspace.to_string_lossy(),
                "started_at": "2026-10-01T10:00:00Z",
                "ended_at": "2026-10-01T10:10:00Z",
                "message_count": records.len(),
                "token_count": 1000
            }]})
            .to_string(),
        )
        .map_err(|error| error.to_string())
    }

    fn run(&self, args: &[&str]) -> TestResult<Value> {
        let mut command = isolated_ee_command(&self.root.join("isolation"))?;
        let output = command
            .arg("--workspace")
            .arg(&self.workspace)
            .arg("--json")
            .args(args)
            .env("EE_CASS_BINARY", &self.cass)
            .env("CASS_STUB_SESSIONS_JSON", &self.sessions)
            .env("CASS_STUB_VIEW_JSONL", &self.view)
            .current_dir(&self.workspace)
            .output()
            .map_err(|error| error.to_string())?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        // bd-mv9j7: `output.status` already carries the code (Display renders
        // "exit status: 7"), but the drift guard credits it only on a literal
        // `.code()` (verification_drift_guard.rs:2782). Spelled BESIDE the
        // status, not instead of it: `.code()` is None for a signal death while
        // Display still says "signal: 9".
        //
        // THIS COMMENT LIVES ABOVE THE `if`, DELIBERATELY. assertion_window
        // (verification_drift_guard.rs:2652) caps a site at 14 lines from the
        // condition, so a comment inside the block pushes `.code()` out of the
        // window and the guard keeps reporting `[code]` missing. Measured: it
        // did exactly that on the first attempt.
        if !output.status.success() {
            return Err(format!(
                "ee {args:?} failed: {} (code {:?})\nstdout={stdout}\nstderr={}",
                output.status,
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let value: Value = serde_json::from_str(&stdout)
            .map_err(|error| format!("ee {args:?} emitted invalid JSON ({error}): {stdout}"))?;
        if value["success"] != true {
            return Err(format!("ee {args:?} did not succeed: {stdout}"));
        }
        Ok(value["data"].clone())
    }
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn workspace_arg(path: &Path) -> TestResult<&str> {
    path.to_str()
        .ok_or_else(|| "non-UTF-8 workspace".to_owned())
}

#[test]
fn imported_failure_fix_arc_packs_as_one_incident_card() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.run(&["init"])?;
    let workspace = workspace_arg(&fixture.workspace)?;
    let imported = fixture.run(&["import", "cass", "--workspace", workspace, "--limit", "5"])?;
    ensure(imported["sessionsImported"] == 1, format!("{imported}"))?;
    // The two tool calls and two tool results are kept out of search by
    // record kind, and the import says so (bd-reality-core-convergence-1azkt.49).
    ensure(
        imported["evidenceAdmission"]
            == json!({
                "admitted": 1,
                "quarantined": 4,
                "quarantineReasons": {"record_kind:tool_call": 2, "record_kind:tool_result": 2}
            }),
        format!("admission tally: {imported}"),
    )?;
    fixture.run(&["index", "rebuild"])?;

    let pack = fixture.run(&[
        "pack",
        "widget serialize trait bound",
        "--source-mode",
        "lexical_only",
        "--read-only",
        "--max-tokens",
        "2000",
    ])?;
    let items = pack["pack"]["items"]
        .as_array()
        .ok_or_else(|| format!("pack items missing: {pack}"))?;
    let cards = items
        .iter()
        .filter(|item| item["trust"]["subclass"] == "derived_incident_card")
        .collect::<Vec<_>>();
    ensure(cards.len() == 1, format!("exactly one card: {items:?}"))?;
    let card = cards[0];
    let content = card["content"].as_str().unwrap_or_default();
    for facet in [
        "Incident card (derived by ee from lines 1-5): `cargo build` failed, then passed after a fix.",
        "Symptom: error[E0277]: the trait bound `Widget: Serialize` is not satisfied (src/widget.rs:41:9)",
        "Fix: The ledger store serializes Widget, so Widget needs to derive Serialize.",
        "Verified: `cargo test` succeeded afterwards.",
    ] {
        ensure(
            content.contains(facet),
            format!("missing {facet:?} in {content}"),
        )?;
    }
    ensure(
        card["estimatedTokens"]
            .as_u64()
            .is_some_and(|tokens| tokens <= 120),
        format!("card over budget: {card}"),
    )?;
    ensure(
        !items.iter().any(|item| {
            item["content"]
                .as_str()
                .is_some_and(|text| text.starts_with("assistant: Let me check"))
        }),
        format!("the raw repair turn is replaced by its card: {items:?}"),
    )?;
    let card_id = card["evidenceSpanId"]
        .as_str()
        .ok_or_else(|| format!("card id missing: {card}"))?
        .to_owned();

    let recalled = fixture.run(&[
        "diagnose-error",
        "--tool",
        "rustc",
        "error[E0277]: the trait bound `Gadget: Serialize` is not satisfied",
    ])?;
    ensure(
        recalled["repairEvidence"][0]["role"] == "incident_card"
            && recalled["repairEvidence"][0]["evidenceId"] == card_id.as_str(),
        format!("recall leads with the card: {recalled}"),
    )?;

    let why = fixture.run(&["why", &card_id])?;
    let sources = &why["entity"]["details"]["incidentCard"];
    ensure(
        sources["derivation"] == "incident_card.v1"
            && sources["errorClasses"] == json!(["rustc:E0277"])
            && sources["failureSpanId"]
                .as_str()
                .is_some_and(|id| id.starts_with("ev_"))
            && sources["repairSpanIds"]
                .as_array()
                .is_some_and(|ids| ids.len() == 1)
            && sources["proofSpanIds"]
                .as_array()
                .is_some_and(|ids| ids.len() == 1),
        format!("why names the card's sources: {why}"),
    )?;

    // Re-import goes through refresh with the card present.
    let again = fixture.run(&["import", "cass", "--workspace", workspace, "--limit", "5"])?;
    ensure(
        again["sessionsImported"] == 0 && again["sessionsSkipped"] == 1,
        format!("re-import is a no-op: {again}"),
    )
}

#[test]
fn distinct_imported_facts_and_same_class_repairs_survive_packing_and_replay() -> TestResult {
    let fixture = Fixture::new()?;
    let repairs = [
        "The ledger store serializes Widget, so I added the Serialize derive in src/widget.rs.",
        "Widget was accidentally passed to the audit serializer, so I changed src/audit.rs to serialize WidgetSummary instead.",
    ];
    let mut transcript = Vec::new();
    for (index, repair) in repairs.iter().enumerate() {
        let failed_call = format!("build_{index}");
        let passing_call = format!("verify_{index}");
        transcript.extend([
            json!({"type": "assistant", "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": failed_call, "name": "Bash", "input": {"command": "cargo build"}}]}}),
            json!({"type": "user", "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": failed_call, "is_error": true,
                 "content": "error[E0277]: the trait bound `Widget: Serialize` is not satisfied\n  --> src/widget.rs:41:9"}]}}),
            json!({"type": "assistant", "message": {"role": "assistant", "content": [
                {"type": "text", "text": repair}]}}),
            json!({"type": "assistant", "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": passing_call, "name": "Bash", "input": {"command": "cargo test --lib"}}]}}),
            json!({"type": "user", "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": passing_call, "is_error": false,
                 "content": "test result: ok. 3 passed; 0 failed"}]}}),
        ]);
    }
    let shared =
        "Widget serialization migration was checked against the complete persisted replay. "
            .repeat(20);
    let facts = [
        "Widget serialization uses protocol 17 after the compatibility tests pass.".to_owned(),
        "Widget serialization uses protocol 18 after the compatibility tests pass.".to_owned(),
        "Widget serialization must include checksum verification before publishing the final artifact.".to_owned(),
        "Widget serialization must not include checksum verification before publishing the final artifact.".to_owned(),
        "Widget serialization uses cp source.txt destination.txt for the inspected output.".to_owned(),
        "Widget serialization uses cp destination.txt source.txt for the inspected output.".to_owned(),
        format!("{shared}Only use protocol 17 in this case."),
        format!("{shared}Never use protocol 17 in this case."),
    ];
    for fact in facts.iter().chain(std::iter::once(&facts[0])) {
        transcript.push(
            json!({"type": "assistant", "message": {"role": "assistant", "content": [
            {"type": "text", "text": fact}]}}),
        );
    }
    fixture.replace_transcript(&transcript)?;
    fixture.run(&["init"])?;
    let workspace = workspace_arg(&fixture.workspace)?;
    let imported = fixture.run(&["import", "cass", "--workspace", workspace, "--limit", "5"])?;
    ensure(imported["sessionsImported"] == 1, format!("{imported}"))?;
    ensure(
        imported["evidenceAdmission"]["admitted"] == 11
            && imported["evidenceAdmission"]["quarantined"] == 8,
        format!("all authored facts are admitted and tool records stay quarantined: {imported}"),
    )?;
    fixture.run(&["index", "rebuild"])?;
    let as_of = chrono::Utc::now().to_rfc3339();
    let pack_args = [
        "pack",
        "widget",
        "--source-mode",
        "lexical_only",
        "--relevance-floor",
        "0",
        "--candidate-pool",
        "64",
        "--max-tokens",
        "6000",
        "--as-of",
        &as_of,
    ];
    let pack = fixture.run(&pack_args)?;
    let items = pack["pack"]["items"]
        .as_array()
        .ok_or_else(|| format!("pack items missing: {pack}"))?;
    let evidence = items
        .iter()
        .filter(|item| item["entityKind"] == "evidence_span")
        .collect::<Vec<_>>();
    ensure(
        evidence.len() == 10,
        format!("two distinct cards and eight distinct facts: {items:?}"),
    )?;
    let cards = evidence
        .iter()
        .copied()
        .filter(|item| item["trust"]["subclass"] == "derived_incident_card")
        .collect::<Vec<_>>();
    ensure(
        cards.len() == 2,
        format!("same error class must retain both repairs: {cards:?}"),
    )?;
    for repair in &repairs {
        ensure(
            cards.iter().any(|card| {
                card["content"]
                    .as_str()
                    .is_some_and(|text| text.contains(repair))
            }),
            format!("missing observed repair {repair:?}: {cards:?}"),
        )?;
    }
    for fact in &facts {
        let expected = format!("assistant: {fact}");
        ensure(
            evidence
                .iter()
                .filter(|item| item["content"] == expected)
                .count()
                == 1,
            format!("each complete distinct fact must occur exactly once: {expected:?}, {items:?}"),
        )?;
    }
    ensure(
        pack["degraded"].as_array().is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry["code"] == "context_evidence_near_duplicates_collapsed"
                    && entry["message"]
                        .as_str()
                        .is_some_and(|text| text.starts_with("1 imported evidence candidate(s)"))
            })
        }),
        format!("exact duplicate compression remains visible: {pack}"),
    )?;
    ensure(
        pack["pack"]["budget"]["usedTokens"]
            .as_u64()
            .is_some_and(|tokens| tokens < 6000),
        format!("fixture fits the pack budget: {pack}"),
    )?;

    let session_id = evidence[0]["sessionId"]
        .as_str()
        .ok_or_else(|| "evidence session missing".to_owned())?
        .to_owned();
    let connection = ee::db::DbConnection::open_file(&fixture.workspace.join(".ee/ee.db"))
        .map_err(|error| error.to_string())?;
    let stored_workspace = connection
        .get_workspace_by_path(workspace)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "workspace missing".to_owned())?;
    let record = connection
        .get_latest_pack_record_for_query(&stored_workspace.id, "widget")
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "pack was not persisted".to_owned())?;
    let stored_items = connection
        .get_pack_evidence_items(&record.id)
        .map_err(|error| error.to_string())?;
    ensure(
        stored_items.len() == evidence.len(),
        "every selected evidence item is durable",
    )?;
    let source_before = connection
        .list_evidence_spans_for_session(&session_id)
        .map_err(|error| error.to_string())?;
    for item in &evidence {
        let id = item["evidenceSpanId"]
            .as_str()
            .ok_or_else(|| "evidence ID missing".to_owned())?;
        let source = source_before
            .iter()
            .find(|span| span.id == id)
            .ok_or_else(|| "selected source missing".to_owned())?;
        ensure(
            item["entityRevision"] == source.pack_entity_revision()
                && item["startLine"] == source.start_line
                && item["endLine"] == source.end_line,
            format!("selected evidence keeps its source identity: {item}"),
        )?;
        let stored = stored_items
            .iter()
            .find(|stored| stored.evidence_id == id)
            .ok_or_else(|| "persisted evidence item missing".to_owned())?;
        ensure(
            stored.entity_revision == source.pack_entity_revision(),
            "durable evidence revision matches source",
        )?;
    }
    let verified = ee::db::parse_stored_pack_ledger(&record);
    ensure(
        verified.status == ee::db::PackLedgerStatus::Available,
        "pack ledger integrity verifies",
    )?;
    connection.close().map_err(|error| error.to_string())?;

    for card in &cards {
        let id = card["evidenceSpanId"]
            .as_str()
            .ok_or_else(|| "card ID missing".to_owned())?;
        let why = fixture.run(&["why", id])?;
        ensure(
            why["entity"]["details"]["incidentCard"]["errorClasses"] == json!(["rustc:E0277"]),
            format!("both distinct repairs retain the same observed error class: {why}"),
        )?;
    }
    let replay = fixture.run(&["pack", "replay", &record.id])?;
    ensure(
        replay["replay"]["status"] == "available",
        format!("integrity-checked replay: {replay}"),
    )?;
    let selected = replay["replay"]["selectedItems"]
        .as_array()
        .ok_or_else(|| format!("replay selected items missing: {replay}"))?;
    ensure(
        selected.len() == evidence.len(),
        "replay retains every selected source",
    )?;
    for item in &evidence {
        ensure(
            selected.iter().any(|replayed| {
                replayed["evidenceSpanId"] == item["evidenceSpanId"]
                    && replayed["entityRevision"] == item["entityRevision"]
            }),
            format!("replay lost the selected source identity: {item}"),
        )?;
    }
    let repeat = fixture.run(&pack_args)?;
    ensure(
        repeat["pack"]["items"] == pack["pack"]["items"],
        "repeated packing preserves order, contents and provenance",
    )?;
    ensure(
        repeat["pack"]["hash"] == pack["pack"]["hash"],
        "repeated packing preserves the content hash",
    )?;
    let again = fixture.run(&["import", "cass", "--workspace", workspace, "--limit", "5"])?;
    ensure(
        again["sessionsImported"] == 0 && again["sessionsSkipped"] == 1,
        format!("re-import leaves the evidence unchanged: {again}"),
    )?;
    let connection = ee::db::DbConnection::open_file(&fixture.workspace.join(".ee/ee.db"))
        .map_err(|error| error.to_string())?;
    ensure(
        connection
            .list_evidence_spans_for_session(&session_id)
            .map_err(|error| error.to_string())?
            == source_before,
        "packing, replay and repeated import preserve original evidence rows",
    )?;
    ensure(
        connection
            .get_pack_record(&record.id)
            .map_err(|error| error.to_string())?
            == Some(record.clone()),
        "original pack ledger remains unchanged",
    )?;
    connection.close().map_err(|error| error.to_string())?;
    let replay_again = fixture.run(&["pack", "replay", &record.id])?;
    ensure(
        replay_again["replay"] == replay["replay"],
        "historical replay remains exact after repeated packing and import",
    )
}
