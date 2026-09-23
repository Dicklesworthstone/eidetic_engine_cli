//! Real-binary failure/repair learning from admitted CASS evidence.
//! Seeds the real database, then uses public CLI commands for proposal, preview,
//! validation, application and replay. This is not an external CASS importer test.
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use ee::core::curate::{CurateApplyOptions, apply_curation_candidate};
use ee::db::{
    CreateAuditInput, CreateEvidenceSpanInput, CreateMemoryInput, CreateSessionInput, DbConnection,
    EvidenceProducerKind, EvidenceSpanMemoryAttachResult, MemoryLinkRelation, audit_actions,
    generate_audit_id,
};
use ee::models::{EvidenceId, MemoryId, SessionId};
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

const LATER_FAILURE: &str =
    "Failure arc: rewriting evidence ownership would violate the source-provenance policy.";
const LATER_REPAIR: &str =
    "Fix: preserve the original evidence owner and audit explicit learning decisions.";

struct MultiEpisodeFixture {
    _temporary: tempfile::TempDir,
    workspace: PathBuf,
    workspace_id: String,
    evidence_id: String,
    // Rule A, anti-pattern A, rule B, anti-pattern B. Never depend on ranking.
    candidates: Vec<Value>,
}

impl MultiEpisodeFixture {
    fn new() -> TestResult<Self> {
        let temporary = tempfile::Builder::new()
            .prefix("ee-multiple-episodes-")
            .tempdir()?;
        let workspace = temporary.path().canonicalize()?;
        run(&workspace, &["init"])?;
        let connection = DbConnection::open_file(&workspace.join(".ee/ee.db"))?;
        let workspace_id = connection
            .get_workspace_by_path(workspace.to_str().ok_or("non-UTF8 workspace")?)?
            .ok_or("workspace missing")?
            .id;
        let session_id = SessionId::from_uuid(uuid::Uuid::from_u128(0x91b_0001)).to_string();
        let evidence_id = EvidenceId::from_uuid(uuid::Uuid::from_u128(0x91b_0002)).to_string();
        let messages = [FAILURE, REPAIR, LATER_FAILURE, LATER_REPAIR];
        let transcript = workspace.join("multiple-episodes.jsonl");
        let mut transcript_text = String::new();
        for message in messages {
            transcript_text.push_str(&json!({"role":"assistant","content":message}).to_string());
            transcript_text.push('\n');
        }
        fs::write(&transcript, &transcript_text)?;
        connection.insert_session(
            &session_id,
            &CreateSessionInput {
                workspace_id: workspace_id.clone(),
                cass_session_id: "multiple-episodes-cli".to_owned(),
                source_path: Some(transcript.to_string_lossy().into_owned()),
                agent_name: Some("fixture".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 4,
                token_count: None,
                content_hash: format!(
                    "blake3:{}",
                    blake3::hash(transcript_text.as_bytes()).to_hex()
                ),
                metadata_json: Some(r#"{"source":"cass","schema":"cass.session.v1"}"#.to_owned()),
            },
        )?;
        let excerpt = messages.join("\n");
        connection.insert_evidence_span(
            &evidence_id,
            &CreateEvidenceSpanInput {
                workspace_id: workspace_id.clone(),
                session_id: session_id.clone(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: "multiple-episodes-window".to_owned(),
                span_kind: "message".to_owned(),
                start_line: 1,
                end_line: 4,
                start_byte: None,
                end_byte: None,
                role: Some("assistant".to_owned()),
                content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
                excerpt,
                metadata_json: Some(
                    r#"{"source":"cass","schema":"cass.evidence_span.v1"}"#.to_owned(),
                ),
                inherited_redaction_classes: Vec::new(),
            },
        )?;
        connection.close()?;
        let proposed = run(
            &workspace,
            &[
                "review",
                "session",
                &session_id,
                "--propose",
                "--limit",
                "4",
                "--min-confidence",
                "0.8",
            ],
        )?;
        let proposed = proposed["candidates"]
            .as_array()
            .ok_or("candidates missing")?;
        assert_eq!(proposed.len(), 4, "{proposed:?}");
        let mut candidates = Vec::new();
        for failure in [FAILURE, LATER_FAILURE] {
            for kind in ["session_arc_rule", "session_arc_anti_pattern"] {
                candidates.push(
                    proposed
                        .iter()
                        .find(|candidate| {
                            candidate["candidateKind"] == kind
                                && candidate["sessionArc"]["failureSpan"]["excerpt"] == failure
                        })
                        .ok_or("missing episode role")?
                        .clone(),
                );
            }
        }
        for (index, candidate) in candidates.iter().enumerate() {
            assert_eq!(
                candidate["sessionArc"]["linkedCandidateId"],
                candidates[index ^ 1]["candidateId"]
            );
        }
        Ok(Self {
            _temporary: temporary,
            workspace,
            workspace_id,
            evidence_id,
            candidates,
        })
    }

    fn connection(&self) -> TestResult<DbConnection> {
        Ok(DbConnection::open_file(&self.workspace.join(".ee/ee.db"))?)
    }

    fn candidate_id(&self, index: usize) -> TestResult<&str> {
        Ok(self.candidates[index]["candidateId"]
            .as_str()
            .ok_or("candidate ID missing")?)
    }

    fn validate(&self, index: usize) -> TestResult {
        let validated = run(
            &self.workspace,
            &[
                "curate",
                "validate",
                self.candidate_id(index)?,
                "--actor",
                "ArcCli",
            ],
        )?;
        assert_eq!(
            validated["validation"]["decision"], "approved",
            "{validated}"
        );
        Ok(())
    }

    fn apply(&self, index: usize) -> TestResult<String> {
        let applied = run(
            &self.workspace,
            &[
                "curate",
                "apply",
                self.candidate_id(index)?,
                "--actor",
                "ArcCli",
            ],
        )?;
        assert_eq!(applied["application"]["status"], "applied", "{applied}");
        Ok(applied["application"]["createdMemoryId"]
            .as_str()
            .ok_or("created memory missing")?
            .to_owned())
    }

    fn accept(&self, index: usize) -> TestResult<String> {
        self.validate(index)?;
        self.apply(index)
    }

    // The public core returns blocked decisions without depending on the CLI's
    // error exit-code convention. The preview still traverses the real binary.
    fn assert_blocked_without_writes(&self, index: usize, code: &str) -> TestResult {
        let connection = self.connection()?;
        let before_audits = connection.list_audit_entries(Some(&self.workspace_id), None)?;
        let before_memories = connection
            .list_memories(&self.workspace_id, None, true)?
            .len();
        let before_jobs = connection
            .list_search_index_jobs(&self.workspace_id, None)?
            .len();
        let before_source = connection
            .get_evidence_span(&self.evidence_id)?
            .ok_or("source missing")?;
        let before_candidate = connection
            .get_curation_candidate(&self.workspace_id, self.candidate_id(index)?)?
            .ok_or("candidate missing")?;
        assert_eq!(
            before_candidate.status, "approved",
            "exercise apply-time revalidation"
        );
        let shown = run(
            &self.workspace,
            &["curate", "show", self.candidate_id(index)?],
        )?;
        assert_eq!(shown["durableMutation"], false);
        assert_eq!(shown["plannedApplication"]["status"], "blocked", "{shown}");
        for dry_run in [true, false] {
            let report = apply_curation_candidate(&CurateApplyOptions {
                workspace_path: &self.workspace,
                database_path: None,
                candidate_id: self.candidate_id(index)?,
                actor: Some("ArcCli"),
                dry_run,
                allow_tombstone_load_bearing: false,
            })
            .map_err(|error| error.message())?;
            assert_eq!(
                report.application.status, "blocked",
                "{:?}",
                report.application.errors
            );
            assert!(!report.mutation.persisted);
            assert!(
                report
                    .application
                    .errors
                    .iter()
                    .any(|issue| issue.code == code)
            );
        }
        assert_eq!(
            connection.list_audit_entries(Some(&self.workspace_id), None)?,
            before_audits
        );
        assert_eq!(
            connection
                .list_memories(&self.workspace_id, None, true)?
                .len(),
            before_memories
        );
        assert_eq!(
            connection
                .list_search_index_jobs(&self.workspace_id, None)?
                .len(),
            before_jobs
        );
        let after_source = connection
            .get_evidence_span(&self.evidence_id)?
            .ok_or("source missing")?;
        assert_eq!(after_source.memory_id, before_source.memory_id);
        assert_eq!(after_source.content_hash, before_source.content_hash);
        assert_eq!(
            connection
                .get_curation_candidate(&self.workspace_id, self.candidate_id(index)?)?
                .ok_or("candidate missing")?
                .status,
            before_candidate.status
        );
        connection.close()?;
        Ok(())
    }
}

fn exercise_multiple_episodes(order: [usize; 4]) -> TestResult {
    let fixture = MultiEpisodeFixture::new()?;
    let source = fixture
        .connection()?
        .get_evidence_span(&fixture.evidence_id)?
        .ok_or("source missing")?;
    let mut memories: [Option<String>; 4] = std::array::from_fn(|_| None);
    for (step, index) in order.into_iter().enumerate() {
        fixture.validate(index)?;
        let connection = fixture.connection()?;
        let before = connection.list_audit_entries(Some(&fixture.workspace_id), None)?;
        let shown = run(
            &fixture.workspace,
            &["curate", "show", fixture.candidate_id(index)?],
        )?;
        assert_eq!(shown["durableMutation"], false);
        let plan = &shown["plannedApplication"];
        assert_eq!(plan["status"], "ready", "{shown}");
        let attachments = plan["plannedEvidenceAttachments"]
            .as_array()
            .ok_or("attachments missing")?;
        if step == 0 {
            assert_eq!(attachments.len(), 1);
            assert_eq!(attachments[0]["evidenceSpanId"], fixture.evidence_id);
            assert!(plan.get("sharedEvidenceSpans").is_none());
        } else {
            assert!(
                attachments.is_empty(),
                "do not advertise source reassignment"
            );
            let shared = plan["sharedEvidenceSpans"]
                .as_array()
                .ok_or("shared source missing")?;
            assert_eq!(shared.len(), 1);
            assert_eq!(shared[0]["evidenceSpanId"], fixture.evidence_id);
            assert_eq!(shared[0]["contentHash"], source.content_hash);
            assert_eq!(
                shared[0]["ownerMemoryId"].as_str(),
                memories[order[0]].as_deref()
            );
        }
        if let Some(peer) = &memories[index ^ 1] {
            let link = &plan["plannedSessionArcLink"];
            let peer_endpoint = if index % 2 == 0 {
                "dstMemoryId"
            } else {
                "srcMemoryId"
            };
            assert_eq!(link[peer_endpoint].as_str(), Some(peer.as_str()));
            assert_eq!(link["relation"], "related");
            assert_eq!(link["directed"], false);
        } else {
            assert!(
                plan.get("plannedSessionArcLink").is_none(),
                "no cross-episode link: {shown}"
            );
        }
        let preview = run(
            &fixture.workspace,
            &[
                "curate",
                "apply",
                fixture.candidate_id(index)?,
                "--dry-run",
                "--actor",
                "ArcCli",
            ],
        )?;
        assert_eq!(preview["application"]["status"], "would_apply", "{preview}");
        assert_eq!(preview["mutation"]["persisted"], false);
        assert_eq!(
            connection.list_audit_entries(Some(&fixture.workspace_id), None)?,
            before
        );
        assert_eq!(
            connection
                .list_memories(&fixture.workspace_id, None, false)?
                .len(),
            step
        );
        memories[index] = Some(fixture.apply(index)?);
        let owner = memories[order[0]].as_deref().ok_or("first owner missing")?;
        let actual_source = connection
            .get_evidence_span(&fixture.evidence_id)?
            .ok_or("source missing")?;
        assert_eq!(actual_source.memory_id.as_deref(), Some(owner));
        assert_eq!(actual_source.content_hash, source.content_hash);
        assert_eq!(actual_source.start_line, source.start_line);
        assert_eq!(actual_source.end_line, source.end_line);
        for (candidate_index, memory) in memories.iter().enumerate() {
            let candidate = connection
                .get_curation_candidate(
                    &fixture.workspace_id,
                    fixture.candidate_id(candidate_index)?,
                )?
                .ok_or("candidate missing")?;
            assert_eq!(
                candidate.status,
                if memory.is_some() {
                    "applied"
                } else {
                    "pending"
                }
            );
            let Some(memory) = memory else {
                continue;
            };
            let links = connection
                .list_memory_links_for_memory(memory, Some(MemoryLinkRelation::Related))?;
            if memories[candidate_index ^ 1].is_some() {
                assert_eq!(links.len(), 1);
                let rule = candidate_index & !1;
                assert_eq!(
                    Some(links[0].src_memory_id.as_str()),
                    memories[rule].as_deref()
                );
                assert_eq!(
                    Some(links[0].dst_memory_id.as_str()),
                    memories[rule + 1].as_deref()
                );
                assert!(!links[0].directed);
                let details: Value = serde_json::from_str(
                    links[0]
                        .metadata_json
                        .as_deref()
                        .ok_or("link metadata missing")?,
                )?;
                assert_eq!(
                    details["ruleCandidateId"].as_str(),
                    Some(fixture.candidate_id(rule)?)
                );
                assert_eq!(
                    details["antiPatternCandidateId"].as_str(),
                    Some(fixture.candidate_id(rule + 1)?)
                );
                assert_eq!(
                    connection
                        .list_audit_by_target("memory_link", &links[0].id, None)?
                        .len(),
                    1
                );
            } else {
                assert!(
                    links.is_empty(),
                    "unaccepted peers must not become graph edges"
                );
            }
        }
        let memory = memories[index].as_deref().ok_or("created memory missing")?;
        let creation = connection
            .list_audit_by_target("memory", memory, None)?
            .into_iter()
            .filter(|audit| audit.action == audit_actions::MEMORY_CREATE)
            .collect::<Vec<_>>();
        assert_eq!(creation.len(), 1);
        let details: Value = serde_json::from_str(
            creation[0]
                .details
                .as_deref()
                .ok_or("creation details missing")?,
        )?;
        assert_eq!(
            details["sourceRefs"]
                .as_array()
                .ok_or("source refs missing")?
                .len(),
            1
        );
        assert_eq!(details["sourceRefs"][0]["id"], fixture.evidence_id);
        assert_eq!(details["sourceRefs"][0]["contentHash"], source.content_hash);
        assert_eq!(
            details["producerPayload"]["sessionArc"],
            fixture.candidates[index]["sessionArc"]
        );
        let audits = connection.list_audit_entries(Some(&fixture.workspace_id), None)?;
        let replay = run(
            &fixture.workspace,
            &[
                "curate",
                "apply",
                fixture.candidate_id(index)?,
                "--actor",
                "ArcCli",
            ],
        )?;
        assert_eq!(replay["application"]["status"], "already_applied");
        assert_eq!(
            connection.list_audit_entries(Some(&fixture.workspace_id), None)?,
            audits
        );
        assert_eq!(
            connection
                .list_memories(&fixture.workspace_id, None, false)?
                .len(),
            step + 1
        );
        assert_eq!(
            connection
                .list_search_index_jobs(&fixture.workspace_id, None)?
                .iter()
                .filter(|job| job.document_id.as_deref() == Some(memory))
                .count(),
            1
        );
        connection.close()?;
    }
    Ok(())
}

#[test]
fn public_cli_learns_all_episodes_in_source_order() -> TestResult {
    exercise_multiple_episodes([0, 1, 2, 3])
}

#[test]
fn public_cli_learns_all_episodes_in_reverse_order() -> TestResult {
    exercise_multiple_episodes([3, 2, 1, 0])
}

#[test]
fn public_cli_interleaves_rule_first_episodes_without_cross_linking() -> TestResult {
    exercise_multiple_episodes([2, 0, 1, 3])
}

#[test]
fn public_cli_interleaves_anti_patterns_without_cross_linking() -> TestResult {
    exercise_multiple_episodes([1, 3, 2, 0])
}

#[test]
fn public_cli_keeps_rejected_peers_rejected_when_sharing_a_window() -> TestResult {
    let fixture = MultiEpisodeFixture::new()?;
    let owner = fixture.accept(0)?;
    run(
        &fixture.workspace,
        &[
            "curate",
            "reject",
            fixture.candidate_id(3)?,
            "--reason",
            "Keep only the rule",
            "--actor",
            "ArcCli",
        ],
    )?;
    let later = fixture.accept(2)?;
    let connection = fixture.connection()?;
    assert_eq!(
        connection
            .list_memories(&fixture.workspace_id, None, false)?
            .len(),
        2
    );
    for (index, status) in [(3, "rejected"), (1, "pending")] {
        assert_eq!(
            connection
                .get_curation_candidate(&fixture.workspace_id, fixture.candidate_id(index)?)?
                .ok_or("candidate missing")?
                .status,
            status
        );
    }
    for memory in [&owner, &later] {
        assert!(
            connection
                .list_memory_links_for_memory(memory, None)?
                .is_empty()
        );
    }
    assert_eq!(
        connection
            .get_evidence_span(&fixture.evidence_id)?
            .ok_or("source missing")?
            .memory_id
            .as_deref(),
        Some(owner.as_str())
    );
    connection.close()?;
    Ok(())
}

#[test]
fn shared_window_owner_retirement_blocks_previously_approved_later_episode() -> TestResult {
    let fixture = MultiEpisodeFixture::new()?;
    let owner = fixture.accept(0)?;
    fixture.validate(2)?;
    let connection = fixture.connection()?;
    assert!(connection.tombstone_memory(&owner)?);
    connection.close()?;
    fixture.assert_blocked_without_writes(2, "session_arc_pair_invalid")
}

#[test]
fn shared_window_ambiguous_owner_audit_blocks_previously_approved_later_episode() -> TestResult {
    let fixture = MultiEpisodeFixture::new()?;
    let owner = fixture.accept(0)?;
    fixture.validate(2)?;
    let connection = fixture.connection()?;
    let original = connection
        .list_audit_by_target("memory", &owner, None)?
        .into_iter()
        .find(|audit| audit.action == audit_actions::MEMORY_CREATE)
        .ok_or("owner creation missing")?;
    connection.insert_audit(
        &generate_audit_id(),
        &CreateAuditInput {
            workspace_id: original.workspace_id,
            actor: Some("CorruptionFixture".to_owned()),
            action: original.action,
            target_type: original.target_type,
            target_id: original.target_id,
            details: original.details,
        },
    )?;
    connection.close()?;
    fixture.assert_blocked_without_writes(2, "session_arc_pair_invalid")
}

#[test]
fn shared_window_source_hash_drift_blocks_previously_approved_later_episode() -> TestResult {
    let fixture = MultiEpisodeFixture::new()?;
    fixture.accept(0)?;
    fixture.validate(2)?;
    let connection = fixture.connection()?;
    // IDs are generated by EvidenceId, not user-controlled SQL. Simulate
    // source corruption between validation and apply on this disposable store.
    connection.execute_raw(&format!(
        "UPDATE evidence_spans SET content_hash = 'blake3:{}' WHERE id = '{}'",
        blake3::hash(b"different evidence").to_hex(),
        fixture.evidence_id
    ))?;
    connection.close()?;
    fixture.assert_blocked_without_writes(2, "session_arc_pair_invalid")
}

#[test]
fn shared_window_forged_creation_does_not_turn_approval_into_application() -> TestResult {
    let fixture = MultiEpisodeFixture::new()?;
    fixture.validate(0)?;
    fixture.validate(2)?;
    let connection = fixture.connection()?;
    let candidate = connection
        .get_curation_candidate(&fixture.workspace_id, fixture.candidate_id(0)?)?
        .ok_or("candidate missing")?;
    let owner = MemoryId::from_uuid(uuid::Uuid::from_u128(0x91b_0003)).to_string();
    connection.insert_memory(
        &owner,
        &CreateMemoryInput {
            workspace_id: fixture.workspace_id.clone(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: candidate
                .proposed_content
                .clone()
                .ok_or("content missing")?,
            workflow_id: None,
            confidence: 0.9,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: None,
            trust_class: "agent_assertion".to_owned(),
            trust_subclass: None,
            tags: Vec::new(),
            valid_from: None,
            valid_to: None,
        },
    )?;
    let metadata: Value = serde_json::from_str(
        candidate
            .derivation_metadata_json
            .as_deref()
            .ok_or("metadata missing")?,
    )?;
    let refs: Value = serde_json::from_str(
        candidate
            .derivation_source_refs_json
            .as_deref()
            .ok_or("source refs missing")?,
    )?;
    connection.insert_audit(
        &generate_audit_id(),
        &CreateAuditInput {
            workspace_id: Some(fixture.workspace_id.clone()),
            actor: Some("CorruptionFixture".to_owned()),
            action: audit_actions::MEMORY_CREATE.to_owned(),
            target_type: Some("memory".to_owned()),
            target_id: Some(owner.clone()),
            details: Some(
                json!({
                    "schema": "ee.audit.derived_memory_created.v1",
                    "candidateId": candidate.id,
                    "createdMemoryId": owner,
                    "producer": "review_session",
                    "producerPayload": metadata["producer"]["producerPayload"],
                    "sourceRefs": refs,
                })
                .to_string(),
            ),
        },
    )?;
    let source = connection
        .get_evidence_span(&fixture.evidence_id)?
        .ok_or("source missing")?;
    assert_eq!(
        connection.attach_evidence_span_to_memory_if_unlinked(
            &fixture.workspace_id,
            &fixture.evidence_id,
            &source.content_hash,
            &owner
        )?,
        EvidenceSpanMemoryAttachResult::Attached
    );
    connection.close()?;
    fixture.assert_blocked_without_writes(2, "session_arc_pair_invalid")
}

#[test]
fn public_cli_learns_all_structured_cross_window_episodes_and_applies_them_independently()
-> TestResult {
    let temporary = tempfile::Builder::new()
        .prefix("ee-sequence-learning-")
        .tempdir()?;
    let workspace = temporary.path().canonicalize()?;
    run(&workspace, &["init"])?;
    let database = workspace.join(".ee/ee.db");
    let db = DbConnection::open_file(&database)?;
    let workspace_id = db
        .get_workspace_by_path(workspace.to_str().ok_or("non-UTF8 workspace")?)?
        .ok_or("workspace missing")?
        .id;
    let session_id = SessionId::from_uuid(uuid::Uuid::from_u128(0x91c_0001)).to_string();
    let messages = [
        "cargo test failed because the cache identity was stale.",
        "Fixed the cache identity and cargo test passed.",
        "cargo test failed because the fixture path was absent.",
        "Fixed the fixture path and cargo test passed.",
    ];
    let records: Vec<String> = messages.iter().enumerate().map(|(index, message)| {
        json!({"type":"assistant", "message":{"role":"assistant", "content":message},
            "metadata":{"topic":if index % 2 == 0 {"rustfmt"} else {"clippy"}, "content":"not-lesson-material"}}).to_string()
    }).collect();
    let transcript = workspace.join("structured-episodes.jsonl");
    let transcript_text = format!("{}\n", records.join("\n"));
    fs::write(&transcript, &transcript_text)?;
    db.insert_session(
        &session_id,
        &CreateSessionInput {
            workspace_id: workspace_id.clone(),
            cass_session_id: "structured-episodes".into(),
            source_path: Some(transcript.to_string_lossy().into_owned()),
            agent_name: Some("fixture".into()),
            model: None,
            started_at: None,
            ended_at: None,
            message_count: 4,
            token_count: None,
            content_hash: format!(
                "blake3:{}",
                blake3::hash(transcript_text.as_bytes()).to_hex()
            ),
            metadata_json: Some(r#"{"source":"cass","schema":"cass.session.v1"}"#.into()),
        },
    )?;
    let mut evidence_ids = Vec::new();
    for (index, record) in records.iter().enumerate() {
        let id =
            EvidenceId::from_uuid(uuid::Uuid::from_u128(0x91c_1000 + index as u128)).to_string();
        let line = u32::try_from(index + 1)?;
        db.insert_evidence_span(
            &id,
            &CreateEvidenceSpanInput {
                workspace_id: workspace_id.clone(),
                session_id: session_id.clone(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("sequence-{index}"),
                span_kind: "message".into(),
                start_line: line,
                end_line: line,
                start_byte: None,
                end_byte: None,
                role: Some("assistant".into()),
                excerpt: record.clone(),
                content_hash: format!("blake3:{}", blake3::hash(record.as_bytes()).to_hex()),
                metadata_json: Some(r#"{"source":"cass","schema":"cass.evidence_span.v1"}"#.into()),
                inherited_redaction_classes: Vec::new(),
            },
        )?;
        evidence_ids.push(id);
    }
    db.close()?;
    let proposed = run(
        &workspace,
        &[
            "review",
            "session",
            &session_id,
            "--propose",
            "--limit",
            "8",
            "--min-confidence",
            "0.8",
        ],
    )?;
    let candidates: Vec<&Value> = proposed["candidates"]
        .as_array()
        .ok_or("candidates missing")?
        .iter()
        .filter(|candidate| candidate["sessionArc"].is_object())
        .collect();
    assert_eq!(
        candidates.len(),
        4,
        "both episodes must be usable: {proposed}"
    );
    let mut memory_ids = Vec::new();
    // Apply the second episode first. It must not approve the first one, and
    // reconstructing from its two raw source records must reproduce its IDs.
    for line in [3, 1] {
        let pair: Vec<_> = candidates
            .iter()
            .copied()
            .filter(|candidate| candidate["sessionArc"]["failureSpan"]["startLine"] == line)
            .collect();
        assert_eq!(pair.len(), 2, "missing episode at line {line}: {proposed}");
        for kind in ["session_arc_rule", "session_arc_anti_pattern"] {
            let candidate = pair
                .iter()
                .copied()
                .find(|candidate| candidate["candidateKind"] == kind)
                .ok_or("pair member missing")?;
            let id = candidate["candidateId"]
                .as_str()
                .ok_or("candidate ID missing")?;
            assert!(
                !candidate["proposedContent"]
                    .as_str()
                    .ok_or("content missing")?
                    .contains("not-lesson-material")
            );
            let validated = run(
                &workspace,
                &["curate", "validate", id, "--actor", "SequenceCli"],
            )?;
            assert_eq!(
                validated["validation"]["decision"], "approved",
                "{validated}"
            );
            let applied = run(
                &workspace,
                &["curate", "apply", id, "--actor", "SequenceCli"],
            )?;
            assert_eq!(applied["application"]["status"], "applied", "{applied}");
            memory_ids.push(
                applied["application"]["createdMemoryId"]
                    .as_str()
                    .ok_or("memory missing")?
                    .to_owned(),
            );
            let replay = run(
                &workspace,
                &["curate", "apply", id, "--actor", "SequenceCli"],
            )?;
            assert_eq!(replay["application"]["status"], "already_applied");
        }
        let db = DbConnection::open_file(&database)?;
        assert_eq!(
            db.list_memories(&workspace_id, None, false)?.len(),
            memory_ids.len()
        );
        if line == 3 {
            for candidate in &candidates {
                if candidate["sessionArc"]["failureSpan"]["startLine"] == 1 {
                    let stored = db
                        .get_curation_candidate(
                            &workspace_id,
                            candidate["candidateId"]
                                .as_str()
                                .ok_or("candidate missing")?,
                        )?
                        .ok_or("candidate lost")?;
                    assert_ne!(
                        stored.status, "applied",
                        "later episode must not accept its predecessor"
                    );
                }
            }
        }
        db.close()?;
    }
    let db = DbConnection::open_file(&database)?;
    for (index, id) in evidence_ids.iter().enumerate() {
        let source = db.get_evidence_span(id)?.ok_or("source lost")?;
        assert_eq!(
            source.excerpt, records[index],
            "learning must not rewrite source JSON"
        );
        assert_eq!(
            source.content_hash,
            format!(
                "blake3:{}",
                blake3::hash(records[index].as_bytes()).to_hex()
            )
        );
        let owner = if index < 2 {
            &memory_ids[2]
        } else {
            &memory_ids[0]
        };
        assert_eq!(source.memory_id.as_ref(), Some(owner));
    }
    for pair in memory_ids.as_chunks::<2>().0 {
        let links = db.list_memory_links_for_memory(&pair[0], Some(MemoryLinkRelation::Related))?;
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].src_memory_id, pair[0]);
        assert_eq!(links[0].dst_memory_id, pair[1]);
        assert_eq!(
            db.list_audit_by_target("memory_link", &links[0].id, None)?
                .len(),
            1
        );
    }
    db.close()?;
    Ok(())
}
