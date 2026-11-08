//! Public CLI boundaries for bounded, preview-first outcome harvesting.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::Parser;

use super::{Cli, Command, NormalizedInvocation, OutcomeCommand, ProcessExitCode};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SINCE: &str = "2026-01-01T00:00:00.000000001Z";
const UNTIL: &str = "2026-01-03T00:00:00.000000001Z";

struct HarvestFixture {
    _temporary: tempfile::TempDir,
    workspace: PathBuf,
    database: PathBuf,
    workspace_id: String,
    memory_id: String,
    pack_id: String,
}

#[derive(Debug, PartialEq)]
struct HarvestState {
    memory: crate::db::StoredMemory,
    feedback: Vec<crate::db::StoredFeedbackEvent>,
    candidates: Vec<crate::db::StoredCurationCandidate>,
    evidence: Vec<crate::db::StoredOutcomeEvidence>,
    audits: Vec<crate::db::StoredAuditEntry>,
    index_job_count: usize,
}

impl HarvestFixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        use crate::core::verify::{
            VerificationEvidenceAuthority, VerificationRecordOptions,
            record_validated_verification_evidence,
        };
        use crate::db::{
            CreateMemoryInput, CreatePackItemInput, CreatePackRecordInput, CreateWorkspaceInput,
            DbConnection,
        };
        use sha2::Digest;

        let temporary = tempfile::tempdir()?;
        let workspace = temporary.path().canonicalize()?;
        std::fs::create_dir_all(workspace.join(".ee"))?;
        // Exercise the addressed custom store, including a stored identity
        // independent of the path-derived fallback.
        let database = workspace.join(".ee/harvest.sqlite");
        let workspace_id = crate::models::WorkspaceId::from_uuid(uuid::Uuid::now_v7()).to_string();
        let memory_id = crate::models::MemoryId::from_uuid(uuid::Uuid::now_v7()).to_string();
        let pack_id = crate::models::PackId::from_uuid(uuid::Uuid::now_v7()).to_string();
        let connection = DbConnection::open_file(&database)?;
        connection.migrate()?;
        connection.insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: workspace.to_string_lossy().into_owned(),
                name: Some("harvest CLI".to_owned()),
            },
        )?;
        connection.insert_memory_at(
            &memory_id,
            &CreateMemoryInput {
                workspace_id: workspace_id.clone(),
                level: "episodic".to_owned(),
                kind: "fact".to_owned(),
                content: "Verified the service retry behavior and recovery sequence.".to_owned(),
                workflow_id: None,
                confidence: 0.9,
                utility: 0.8,
                importance: 0.7,
                provenance_uri: Some("manual://harvest-cli".to_owned()),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: None,
                valid_to: None,
            },
            chrono::DateTime::parse_from_rfc3339("2025-12-31T00:00:00Z")?
                .with_timezone(&chrono::Utc),
        )?;
        connection.insert_pack_record_at(
            &pack_id,
            &CreatePackRecordInput {
                task_paths: Vec::new(),
                workspace_id: workspace_id.clone(),
                query: "Verify the service retry behavior".to_owned(),
                profile: "balanced".to_owned(),
                max_tokens: 500,
                used_tokens: 30,
                item_count: 1,
                omitted_count: 0,
                pack_hash: format!("blake3:{}", blake3::hash(b"harvest-cli-pack").to_hex()),
                degraded_json: None,
                created_by: Some("harvest-cli".to_owned()),
            },
            &[CreatePackItemInput {
                pack_id: pack_id.clone(),
                memory_id: memory_id.clone(),
                rank: 1,
                section: "facts".to_owned(),
                estimated_tokens: 30,
                relevance: 0.9,
                utility: 0.8,
                combined_score: Some(0.85),
                attempt_family_multiplicity: None,
                why: "Directly relevant recovery evidence.".to_owned(),
                diversity_key: None,
                provenance_json: "{\"uri\":\"manual://harvest-cli\"}".to_owned(),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
            }],
            &[],
            "2026-01-01T00:00:01Z",
        )?;
        connection.close()?;

        // Parse retained proof fixtures through the trusted ingest boundary;
        // no command is executed by this test or by the harvester.
        for (command, started, finished) in [
            (
                "cargo test --lib harvest_recovery",
                "2026-01-01T01:00:00Z",
                "2026-01-01T01:00:01Z",
            ),
            (
                "cargo test --lib harvest_retry",
                "2026-01-01T01:01:00Z",
                "2026-01-01T01:01:01Z",
            ),
        ] {
            let evidence =
                crate::models::verification_evidence_record_from_rch_verify(&serde_json::json!({
                    "schema": crate::models::RCH_VERIFY_SCHEMA_V1,
                    "command_text": command,
                    "command_hash": format!("{:x}", sha2::Sha256::digest(command.as_bytes())),
                    "command_kind": "cargo_test",
                    "status": "remote_pass",
                    "exit_code": 0,
                    "worker_id": "harvest-fixture-worker",
                    "remote_required": true,
                    "started_at": started,
                    "finished_at": finished,
                }))?;
            record_validated_verification_evidence(
                VerificationRecordOptions {
                    database_path: &database,
                    workspace_path: &workspace,
                    target_type: "pack",
                    target_id: &pack_id,
                    actor: Some("harvest-fixture"),
                    evidence,
                },
                VerificationEvidenceAuthority::ValidatedRchVerify,
            )?;
        }
        Ok(Self {
            _temporary: temporary,
            workspace,
            database,
            workspace_id,
            memory_id,
            pack_id,
        })
    }

    fn state(&self) -> Result<HarvestState, Box<dyn std::error::Error>> {
        let connection = crate::db::DbConnection::open_file_read_only(&self.database)?;
        let state = HarvestState {
            memory: connection
                .get_memory(&self.memory_id)?
                .ok_or("missing source memory")?,
            feedback: connection.list_feedback_events(&self.workspace_id)?,
            candidates: connection.list_curation_candidates(
                &self.workspace_id,
                None,
                None,
                None,
            )?,
            evidence: connection.list_outcome_evidence_for_recovery(&self.workspace_id)?,
            audits: connection.list_audit_entries(Some(&self.workspace_id), None)?,
            index_job_count: connection
                .list_search_index_jobs(&self.workspace_id, None)?
                .len(),
        };
        connection.close()?;
        Ok(state)
    }

    fn run(&self, flags: &[&str]) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let workspace = self
            .workspace
            .to_str()
            .ok_or("workspace path is not UTF-8")?;
        let database = self.database.to_str().ok_or("database path is not UTF-8")?;
        let mut args = vec![
            "ee",
            "--json",
            "--workspace",
            workspace,
            "outcome",
            "harvest",
            "--database",
            database,
            "--since",
            SINCE,
            "--until",
            UNTIL,
        ];
        args.extend_from_slice(flags);
        let (exit, stdout, stderr) = invoke(&args);
        assert_eq!(
            exit,
            ProcessExitCode::Success,
            "{args:?}: {stdout}\n{stderr}"
        );
        assert!(stderr.is_empty(), "{stderr}");
        let response: serde_json::Value = serde_json::from_str(&stdout)?;
        assert_eq!(response["schema"], "ee.response.v2");
        assert_eq!(response["success"], true);
        assert_eq!(response["data"]["schema"], "ee.outcome.harvest.v1");
        assert_eq!(response["data"]["workspaceId"], self.workspace_id);
        let schema: serde_json::Value = serde_json::from_str(
            &crate::output::render_schema_export_json(Some("ee.outcome.harvest.v1")),
        )?;
        assert_exported_fields(&schema, &response["data"])?;
        for proposal in response["data"]["proposals"]
            .as_array()
            .ok_or("proposals missing")?
        {
            assert_exported_fields(&schema["$defs"]["proposal"], proposal)?;
        }
        Ok(response["data"].clone())
    }
}

fn invoke(arguments: &[&str]) -> (ProcessExitCode, String, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = super::run(
        arguments.iter().map(OsString::from),
        &mut stdout,
        &mut stderr,
    );
    (
        exit,
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    )
}

fn assert_exported_fields(schema: &serde_json::Value, payload: &serde_json::Value) -> TestResult {
    let required = schema["required"]
        .as_array()
        .ok_or("schema required fields missing")?;
    let properties = schema["properties"]
        .as_object()
        .ok_or("schema properties missing")?;
    let object = payload.as_object().ok_or("payload must be an object")?;
    for field in required {
        let field = field.as_str().ok_or("required field must be a string")?;
        assert!(
            object.contains_key(field),
            "exported required field {field} is absent"
        );
    }
    for field in object.keys() {
        assert!(
            properties.contains_key(field),
            "emitted field {field} is undocumented"
        );
    }
    Ok(())
}

#[test]
fn harvest_cli_exposes_preview_and_candidate_only_apply_modes() -> TestResult {
    let manifest = crate::core::effect::EffectManifest::build();
    for mode in [None, Some("--dry-run"), Some("--apply")] {
        let mut argv = vec![
            "ee", "outcome", "harvest", "--since", SINCE, "--until", UNTIL,
        ];
        argv.extend(mode);
        let cli = Cli::try_parse_from(&argv)?;
        let os_args = argv.iter().map(OsString::from).collect::<Vec<_>>();
        let invocation = NormalizedInvocation::from_cli(&cli, &os_args);
        let Some(Command::Outcome(parent)) = &cli.command else {
            panic!("expected outcome command");
        };
        let Some(OutcomeCommand::Harvest(harvest)) = &parent.command else {
            panic!("harvest must not be parsed as a feedback target");
        };
        assert_eq!(harvest.apply, mode == Some("--apply"));
        assert_eq!(harvest.dry_run, mode == Some("--dry-run"));
        assert_eq!(harvest.pack_limit, 128);
        assert!(harvest.pack_id.is_none());
        let effect = manifest
            .get(&invocation.command_path)
            .ok_or("harvest must have an effect declaration")?;
        assert_eq!(effect.default_effect.is_mutating(), harvest.apply);
        if harvest.apply {
            assert_eq!(invocation.command_path, "outcome harvest --apply");
            assert!(effect.dry_run_effect.is_none());
            assert!(super::dry_run_refusal_message(&manifest, &invocation.command_path).is_some());
            assert_eq!(
                effect.write_surfaces.db_tables,
                ["curation_candidates", "outcome_evidence_rows", "audit_log"]
            );
            assert!(effect.requires_audit);
        } else {
            assert_eq!(invocation.command_path, "outcome harvest");
            assert!(effect.write_surfaces.db_tables.is_empty());
            assert!(super::dry_run_refusal_message(&manifest, &invocation.command_path).is_none());
        }
    }

    let (exit, stdout, stderr) = invoke(&["ee", "outcome", "harvest", "--help"]);
    assert_eq!(exit, ProcessExitCode::Success, "{stdout}\n{stderr}");
    for flag in [
        "--since",
        "--until",
        "--pack-id",
        "--pack-limit",
        "--dry-run",
        "--apply",
        "--database",
    ] {
        assert!(stdout.contains(flag), "missing help for {flag}: {stdout}");
    }
    let help = stdout.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(help.contains("pending"));
    assert!(help.contains("episodic"));
    assert!(help.contains("does not promote"));
    assert!(
        super::public_cli_command_entries()
            .iter()
            .any(|entry| entry.name == "outcome harvest" && entry.available)
    );
    let recipe = crate::core::agent_docs::AGENT_DOC_RECIPES
        .iter()
        .find(|recipe| recipe.id == "harvest-pack-outcomes")
        .ok_or("harvest must have an agent recipe")?;
    assert!(recipe.command.contains("--dry-run"));
    assert!(recipe.description.contains("pending candidates"));
    assert!(crate::models::schema::is_known_schema(
        "ee.outcome.harvest.v1"
    ));
    let (exit, schema_stdout, stderr) =
        invoke(&["ee", "schema", "export", "ee.outcome.harvest.v1", "--json"]);
    assert_eq!(exit, ProcessExitCode::Success, "{schema_stdout}\n{stderr}");
    let exported: serde_json::Value = serde_json::from_str(&schema_stdout)?;
    let documented: serde_json::Value = serde_json::from_str(include_str!(
        "../../docs/schemas/ee.outcome.harvest.v1.json"
    ))?;
    assert_eq!(exported, documented);
    Ok(())
}

#[test]
fn harvest_cli_rejects_ambiguous_modes_and_invalid_windows_before_creating_a_store() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("unopened-workspace");
    let workspace = workspace.to_str().ok_or("fixture path is not UTF-8")?;
    let cases = [
        (
            vec![
                "outcome",
                "harvest",
                "--since",
                SINCE,
                "--until",
                UNTIL,
                "--apply",
                "--dry-run",
            ],
            "cannot be used with",
        ),
        (
            vec![
                "outcome",
                "--dry-run",
                "harvest",
                "--since",
                SINCE,
                "--until",
                UNTIL,
                "--apply",
            ],
            "does not support --dry-run",
        ),
        (
            vec![
                "outcome", "--signal", "helpful", "harvest", "--since", SINCE, "--until", UNTIL,
            ],
            "harvest cannot be combined",
        ),
        (vec!["outcome", "harvest", "--since", SINCE], "--until"),
        (
            vec![
                "outcome",
                "harvest",
                "--since",
                "not-a-time",
                "--until",
                UNTIL,
            ],
            "since must be an explicit RFC3339",
        ),
        (
            vec!["outcome", "harvest", "--since", UNTIL, "--until", SINCE],
            "since must be strictly earlier",
        ),
        (
            vec!["outcome", "harvest", "--since", SINCE, "--until", SINCE],
            "since must be strictly earlier",
        ),
        (
            vec![
                "outcome",
                "harvest",
                "--since",
                SINCE,
                "--until",
                UNTIL,
                "--pack-limit",
                "0",
            ],
            "pack-limit must be between 1 and 256",
        ),
        (
            vec![
                "outcome",
                "harvest",
                "--since",
                SINCE,
                "--until",
                UNTIL,
                "--pack-limit",
                "257",
            ],
            "pack-limit must be between 1 and 256",
        ),
        (
            vec![
                "outcome",
                "harvest",
                "--since",
                SINCE,
                "--until",
                UNTIL,
                "--pack-id",
                "unknown-pack",
            ],
            "pack-id must be a canonical persisted pack ID",
        ),
    ];
    for (command, expected_message) in cases {
        let mut argv = vec!["ee", "--json", "--workspace", workspace];
        argv.extend(command);
        let (exit, stdout, stderr) = invoke(&argv);
        assert_eq!(exit, ProcessExitCode::Usage, "{argv:?}: {stdout}\n{stderr}");
        assert!(
            format!("{stdout}\n{stderr}").contains(expected_message),
            "{argv:?}: expected {expected_message:?}, got {stdout}\n{stderr}"
        );
        assert!(!std::path::Path::new(workspace).exists());
    }
    Ok(())
}

#[test]
fn harvest_cli_reports_truncation_and_targets_an_older_exact_pack() -> TestResult {
    let fixture = HarvestFixture::new()?;
    let connection = crate::db::DbConnection::open_file(&fixture.database)?;
    let newer_pack_id = crate::models::PackId::from_uuid(uuid::Uuid::now_v7()).to_string();
    connection.insert_pack_record_at(
        &newer_pack_id,
        &crate::db::CreatePackRecordInput {
            task_paths: Vec::new(),
            workspace_id: fixture.workspace_id.clone(),
            query: "A newer pack without corroborating verification".to_owned(),
            profile: "balanced".to_owned(),
            max_tokens: 500,
            used_tokens: 0,
            item_count: 0,
            omitted_count: 0,
            pack_hash: format!(
                "blake3:{}",
                blake3::hash(b"newer-harvest-cli-pack").to_hex()
            ),
            degraded_json: None,
            created_by: Some("harvest-cli".to_owned()),
        },
        &[],
        &[],
        "2026-01-02T00:00:01Z",
    )?;
    connection.close()?;
    let before = fixture.state()?;

    let recent = fixture.run(&["--pack-limit", "1"])?;
    assert_eq!(recent["scannedPacks"], 1);
    assert_eq!(recent["proposedCount"], 0);
    assert_eq!(recent["bounded"], true);
    assert_eq!(recent["coverage"], "recent_packs_truncated");
    assert_eq!(recent["skipped"][0]["packId"], newer_pack_id);

    let targeted = fixture.run(&["--pack-limit", "1", "--pack-id", &fixture.pack_id])?;
    assert_eq!(targeted["scannedPacks"], 1);
    assert_eq!(targeted["proposedCount"], 1);
    assert_eq!(targeted["bounded"], false);
    assert_eq!(targeted["coverage"], "exact_pack");
    assert_eq!(targeted["proposals"][0]["packId"], fixture.pack_id);
    assert_eq!(
        fixture.state()?,
        before,
        "both coverage modes must remain previews"
    );
    Ok(())
}

#[test]
fn harvest_cli_previews_then_saves_and_replays_only_pending_review_candidates() -> TestResult {
    let fixture = HarvestFixture::new()?;
    let original = fixture.state()?;
    assert!(original.candidates.is_empty());
    assert!(original.evidence.is_empty());
    assert!(original.feedback.is_empty());
    assert_eq!(original.index_job_count, 0);
    let preview = fixture.run(&[])?;
    assert_eq!(preview["dryRun"], true);
    assert_eq!(preview["proposedCount"], 1);
    assert_eq!(preview["persistedCount"], 0);
    assert_eq!(preview["proposals"][0]["memoryId"], fixture.memory_id);
    assert_eq!(preview["proposals"][0]["corroboratingCount"], 2);
    assert_eq!(
        fixture.state()?,
        original,
        "default harvest must be read-only"
    );
    assert_eq!(fixture.run(&["--dry-run"])?, preview);
    assert_eq!(
        fixture.state()?,
        original,
        "explicit dry-run must be read-only"
    );

    let applied = fixture.run(&["--apply", "--actor", "harvest-cli-reviewer"])?;
    assert_eq!(applied["dryRun"], false);
    assert_eq!(applied["persistedCount"], 1);
    let persisted = fixture.state()?;
    assert_eq!(persisted.candidates.len(), 1);
    assert_eq!(persisted.candidates[0].status, "pending");
    assert_eq!(persisted.candidates[0].candidate_type, "promote");
    assert_eq!(
        persisted.candidates[0].target_memory_id.as_deref(),
        Some(fixture.memory_id.as_str())
    );
    assert_eq!(persisted.evidence.len(), 2);
    assert!(persisted.audits.len() > original.audits.len());
    assert_eq!(
        persisted.memory, original.memory,
        "harvest must not apply promotion"
    );
    assert_eq!(
        persisted.feedback, original.feedback,
        "harvest must not synthesize explicit feedback"
    );
    assert_eq!(persisted.index_job_count, 0);

    let replay = fixture.run(&["--apply", "--pack-id", &fixture.pack_id])?;
    assert_eq!(replay["persistedCount"], 0);
    assert_eq!(replay["replayedCount"], 1);
    assert_eq!(
        fixture.state()?,
        persisted,
        "an exact receipt replay must not duplicate writes"
    );
    assert!(
        !fixture.workspace.join(".ee/ee.db").exists(),
        "--database must select the requested store"
    );
    Ok(())
}
