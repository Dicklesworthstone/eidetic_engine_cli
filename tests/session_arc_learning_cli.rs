//! Real-binary failure/repair learning from admitted CASS evidence.
//! Seeds the real database, then uses public CLI commands for proposal, preview,
//! validation, application and replay. This is not an external CASS importer test.
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::Command;

use ee::db::{
    CreateEvidenceSpanInput, CreateSessionInput, DbConnection, EvidenceProducerKind,
    MemoryLinkRelation, audit_actions,
};
use ee::models::{EvidenceId, SessionId};
use serde_json::{Value, json};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const FAILURE: &str = "Failure arc: storing silently would violate the no-loop-takeover policy.";
const REPAIR: &str = "Fix: require accept/reject commands and audit every accepted capture.";

fn run(workspace: &Path, args: &[&str]) -> TestResult<Value> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--workspace")
        .arg(workspace)
        .arg("--json")
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .env("EE_EMBED_DOWNLOAD", "off")
        .env("NO_COLOR", "1")
        .env("XDG_DATA_HOME", workspace.join("xdg-data"))
        .env("XDG_CONFIG_HOME", workspace.join("xdg-config"))
        .env("XDG_CACHE_HOME", workspace.join("xdg-cache"))
        .current_dir(workspace)
        .output()?;
    let event = json!({"schema":"ee.test_event.v1", "testId":"session_arc_learning_cli", "kind":"command_finish",
        "command":"ee", "args":args, "exitCode":output.status.code(),
        "stdout":String::from_utf8_lossy(&output.stdout), "stderr":String::from_utf8_lossy(&output.stderr)});
    writeln!(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(workspace.join("commands.jsonl"))?,
        "{event}"
    )?;
    assert!(output.status.success(), "{event}");
    let result: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(result["schema"], "ee.response.v2", "{event}");
    assert_eq!(result["success"], true, "{event}");
    Ok(result["data"].clone())
}

fn exercise(single_window: bool) -> TestResult {
    // Normalize only the trusted temporary root, not product descendant paths.
    let temporary = tempfile::Builder::new()
        .prefix("ee-session-arc-cli-")
        .tempdir()?;
    let workspace = temporary.path().canonicalize()?;
    run(&workspace, &["init"])?;
    let database = workspace.join(".ee/ee.db");
    let connection = DbConnection::open_file(&database)?;
    let workspace_id = connection
        .get_workspace_by_path(workspace.to_str().ok_or("non-UTF8 workspace")?)?
        .ok_or("initialized workspace missing")?
        .id;
    let session_id = SessionId::from_uuid(uuid::Uuid::from_u128(0x91a_0001)).to_string();
    let transcript = workspace.join("failed-to-fixed.jsonl");
    let combined = format!("{FAILURE}\n{REPAIR}");
    let distinct = [
        "The capture hook cargo test failed with red error output.",
        "Fixed the capture hook and cargo test passed green.",
    ];
    let messages = if single_window {
        [FAILURE, REPAIR]
    } else {
        distinct
    };
    fs::write(
        &transcript,
        format!(
            "{}\n{}\n",
            json!({"role":"assistant","content":messages[0]}),
            json!({"role":"user","content":messages[1]})
        ),
    )?;
    connection.insert_session(
        &session_id,
        &CreateSessionInput {
            workspace_id: workspace_id.clone(),
            cass_session_id: "session-arc-cli".to_owned(),
            source_path: Some(transcript.to_string_lossy().into_owned()),
            agent_name: Some("fixture".to_owned()),
            model: None,
            started_at: None,
            ended_at: None,
            message_count: 2,
            token_count: None,
            content_hash: format!("blake3:{}", blake3::hash(&fs::read(&transcript)?).to_hex()),
            metadata_json: Some(r#"{"source":"cass","schema":"cass.session.v1"}"#.to_owned()),
        },
    )?;
    let excerpts: Vec<&str> = if single_window {
        vec![&combined]
    } else {
        // Separate evidence windows share the same concrete test-command topic.
        distinct.to_vec()
    };
    let mut evidence_ids = Vec::new();
    for (index, excerpt) in excerpts.iter().enumerate() {
        let id =
            EvidenceId::from_uuid(uuid::Uuid::from_u128(0x91a_1000 + index as u128)).to_string();
        let line = u32::try_from(index + 1)?;
        connection.insert_evidence_span(
            &id,
            &CreateEvidenceSpanInput {
                workspace_id: workspace_id.clone(),
                session_id: session_id.clone(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("cli-window-{index}"),
                span_kind: "message".to_owned(),
                start_line: line,
                end_line: if single_window { 2 } else { line },
                start_byte: None,
                end_byte: None,
                role: Some("assistant".to_owned()),
                excerpt: (*excerpt).to_owned(),
                content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
                metadata_json: Some(
                    r#"{"source":"cass","schema":"cass.evidence_span.v1"}"#.to_owned(),
                ),
                inherited_redaction_classes: Vec::new(),
            },
        )?;
        evidence_ids.push(id);
    }
    connection.close()?;
    let preview = run(
        &workspace,
        &[
            "review",
            "session",
            &session_id,
            "--dry-run",
            "--limit",
            "2",
            "--min-confidence",
            "0.8",
        ],
    )?;
    assert_eq!(preview["candidateCount"], 2, "{preview}");
    assert_eq!(preview["durableMutation"], false);
    let too_small = run(
        &workspace,
        &[
            "review",
            "session",
            &session_id,
            "--dry-run",
            "--limit",
            "1",
            "--min-confidence",
            "0.8",
        ],
    )?;
    assert_eq!(
        too_small["candidateCount"], 0,
        "do not split a learning pair: {too_small}"
    );
    let proposed = run(
        &workspace,
        &[
            "review",
            "session",
            &session_id,
            "--propose",
            "--limit",
            "2",
            "--min-confidence",
            "0.8",
        ],
    )?;
    let candidates = proposed["candidates"]
        .as_array()
        .ok_or("candidates missing")?;
    assert_eq!(candidates.len(), 2);
    assert_eq!(proposed["durableMutation"], true);
    let anti = candidates
        .iter()
        .find(|c| c["candidateKind"] == "session_arc_anti_pattern")
        .ok_or("anti-pattern missing")?;
    let rule = candidates
        .iter()
        .find(|c| c["candidateKind"] == "session_arc_rule")
        .ok_or("rule missing")?;
    assert_eq!(anti["sessionArc"]["linkedCandidateId"], rule["candidateId"]);
    assert_eq!(rule["sessionArc"]["linkedCandidateId"], anti["candidateId"]);
    assert_eq!(rule["sessionArc"]["arcId"], anti["sessionArc"]["arcId"]);
    let mut memories: Vec<String> = Vec::new();
    for (index, candidate) in [rule, anti].iter().enumerate() {
        let id = candidate["candidateId"]
            .as_str()
            .ok_or("candidate ID missing")?;
        let validated = run(&workspace, &["curate", "validate", id, "--actor", "ArcCli"])?;
        assert_eq!(
            validated["validation"]["decision"], "approved",
            "{validated}"
        );
        if index == 1 {
            let shown = run(&workspace, &["curate", "show", id])?;
            let plan = &shown["plannedApplication"];
            assert_eq!(shown["durableMutation"], false);
            assert_eq!(plan["plannedEvidenceAttachments"], json!([]));
            assert_eq!(
                plan["sharedEvidenceSpans"]
                    .as_array()
                    .ok_or("shared sources missing")?
                    .len(),
                evidence_ids.len()
            );
            assert_eq!(plan["plannedSessionArcLink"]["srcMemoryId"], memories[0]);
            assert_eq!(plan["plannedSessionArcLink"]["relation"], "related");
        }
        let applied = run(&workspace, &["curate", "apply", id, "--actor", "ArcCli"])?;
        assert_eq!(applied["application"]["status"], "applied", "{applied}");
        memories.push(
            applied["application"]["createdMemoryId"]
                .as_str()
                .ok_or("created ID missing")?
                .to_owned(),
        );
        let replay = run(&workspace, &["curate", "apply", id, "--actor", "ArcCli"])?;
        assert_eq!(replay["application"]["status"], "already_applied");
    }
    let connection = DbConnection::open_file(&database)?;
    assert_eq!(
        connection.list_memories(&workspace_id, None, false)?.len(),
        2
    );
    assert_eq!(
        connection
            .get_memory(&memories[0])?
            .ok_or("rule missing")?
            .kind,
        "rule"
    );
    assert_eq!(
        connection
            .get_memory(&memories[1])?
            .ok_or("anti-pattern missing")?
            .kind,
        "anti-pattern"
    );
    let links =
        connection.list_memory_links_for_memory(&memories[0], Some(MemoryLinkRelation::Related))?;
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].src_memory_id, memories[0]);
    assert_eq!(links[0].dst_memory_id, memories[1]);
    assert!(!links[0].directed);
    let audits = connection.list_audit_by_target("memory_link", &links[0].id, None)?;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].action, audit_actions::MEMORY_LINK_CREATE);
    assert_eq!(audits[0].actor.as_deref(), Some("ArcCli"));
    for id in evidence_ids {
        assert_eq!(
            connection
                .get_evidence_span(&id)?
                .ok_or("source disappeared")?
                .memory_id
                .as_ref(),
            Some(&memories[0])
        );
    }
    connection.close()?;
    Ok(())
}

#[test]
fn public_cli_applies_same_window_failure_repair_pair() -> TestResult {
    exercise(true)
}

#[test]
fn public_cli_applies_distinct_failure_repair_windows() -> TestResult {
    exercise(false)
}
