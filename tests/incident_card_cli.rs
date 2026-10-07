//! Black-box incident-card lifecycle (ADR 0091, bd-reality-core-convergence-1azkt.59).
//!
//! A real `ee` binary imports one Claude Code transcript through a stub `cass`
//! (a failing `cargo build`, the turn that explains the fix, a passing
//! `cargo test`), then: the pack carries the derived incident card in place of
//! the raw repair turn, `diagnose-error` recalls the card first, `ee why` names
//! the card's sources, and re-importing the session (the refresh path, with the
//! card present) succeeds and adds nothing.
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
        if !output.status.success() {
            return Err(format!(
                "ee {args:?} failed: {}\nstdout={stdout}\nstderr={}",
                output.status,
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
