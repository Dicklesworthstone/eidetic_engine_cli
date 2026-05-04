use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use ee::core::index::{IndexRebuildOptions, IndexRebuildStatus, rebuild_index};
use ee::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};
use ee::models::WorkspaceId;
use insta::assert_json_snapshot;
use serde_json::Value;

type TestResult = Result<(), String>;

const MEMORY_ID: &str = "mem_00000000000000000000000001";
const PACK_ID: &str = "pack_00000000000000000000000001";
const QUERY: &str = "format before release";
const FIXTURE_TIMESTAMP: &str = "2026-04-29T12:00:00+00:00";

#[derive(Debug)]
struct JsonContractFixture {
    workspace: PathBuf,
    database: PathBuf,
    index_dir: PathBuf,
}

impl JsonContractFixture {
    fn new() -> Result<Self, String> {
        let artifact_dir = unique_artifact_dir("json-contract-snapshots")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");

        fs::create_dir_all(&workspace).map_err(|error| {
            format!(
                "failed to create fixture workspace {}: {error}",
                workspace.display()
            )
        })?;
        seed_workspace(&workspace, &database)?;
        build_search_index(&workspace, &database, &index_dir)?;

        Ok(Self {
            workspace,
            database,
            index_dir,
        })
    }

    fn workspace_arg(&self) -> String {
        self.workspace.to_string_lossy().into_owned()
    }

    fn database_arg(&self) -> String {
        self.database.to_string_lossy().into_owned()
    }

    fn index_dir_arg(&self) -> String {
        self.index_dir.to_string_lossy().into_owned()
    }
}

fn unique_artifact_dir(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    Ok(std::env::temp_dir()
        .join("ee-insta-golden-artifacts")
        .join(format!("{prefix}-{}-{now}", std::process::id())))
}

fn seed_workspace(workspace: &Path, database: &Path) -> TestResult {
    let workspace_id = stable_workspace_id(workspace);

    if let Some(parent) = database.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create database parent {}: {error}",
                parent.display()
            )
        })?;
    }

    let connection = DbConnection::open_file(database).map_err(|error| error.to_string())?;
    connection.migrate().map_err(|error| error.to_string())?;
    connection
        .insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: workspace.to_string_lossy().into_owned(),
                name: Some("json-contract-snapshots".to_string()),
            },
        )
        .map_err(|error| error.to_string())?;
    connection
        .insert_memory(
            MEMORY_ID,
            &CreateMemoryInput {
                workspace_id: workspace_id.clone(),
                level: "procedural".to_string(),
                kind: "rule".to_string(),
                content: "Run cargo fmt --check before release.".to_string(),
                confidence: 0.92,
                utility: 0.8,
                importance: 0.7,
                provenance_uri: Some("file://AGENTS.md#L164-173".to_string()),
                trust_class: "human_explicit".to_string(),
                trust_subclass: Some("project-rule".to_string()),
                tags: vec!["cargo".to_string(), "formatting".to_string()],
                valid_from: None,
                valid_to: None,
            },
        )
        .map_err(|error| error.to_string())?;
    connection
        .execute_raw(&format!(
            "UPDATE workspaces SET created_at = '{FIXTURE_TIMESTAMP}', updated_at = '{FIXTURE_TIMESTAMP}' WHERE id = '{workspace_id}'"
        ))
        .map_err(|error| error.to_string())?;
    connection
        .execute_raw(&format!(
            "UPDATE memories SET created_at = '{FIXTURE_TIMESTAMP}', updated_at = '{FIXTURE_TIMESTAMP}', provenance_chain_hash = 'blake3:fixture-memory-hash' WHERE id = '{MEMORY_ID}'"
        ))
        .map_err(|error| error.to_string())?;
    connection
        .execute_raw(&format!(
            "INSERT INTO pack_records (id, workspace_id, query, profile, max_tokens, used_tokens, item_count, omitted_count, pack_hash, degraded_json, created_at, created_by) VALUES ('{PACK_ID}', '{workspace_id}', '{QUERY}', 'compact', 4000, 8, 1, 0, 'blake3:fixture-pack-hash', NULL, '2026-04-29T12:01:00+00:00', 'golden-test')"
        ))
        .map_err(|error| error.to_string())?;
    connection
        .execute_raw(&format!(
            "INSERT INTO pack_items (pack_id, memory_id, rank, section, estimated_tokens, relevance, utility, why, diversity_key, provenance_json, trust_class, trust_subclass) VALUES ('{PACK_ID}', '{MEMORY_ID}', 1, 'procedural_rules', 8, 0.91, 0.8, 'Selected because the memory matches release-formatting work.', 'procedural:rule:cargo', '{{\"schema\":\"ee.pack_item.provenance.v1\",\"entries\":[{{\"uri\":\"file://AGENTS.md#L164-173\",\"trustClass\":\"human_explicit\",\"trustSubclass\":\"project-rule\"}}]}}', 'human_explicit', 'project-rule')"
        ))
        .map_err(|error| error.to_string())?;
    connection.close().map_err(|error| error.to_string())
}

fn stable_workspace_id(workspace: &Path) -> String {
    let canonical_workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let hash =
        blake3::hash(format!("workspace:{}", canonical_workspace.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn build_search_index(workspace: &Path, database: &Path, index_dir: &Path) -> TestResult {
    let report = rebuild_index(&IndexRebuildOptions {
        workspace_path: workspace.to_path_buf(),
        database_path: Some(database.to_path_buf()),
        index_dir: Some(index_dir.to_path_buf()),
        dry_run: false,
    })
    .map_err(|error| error.to_string())?;

    if report.status != IndexRebuildStatus::Success {
        return Err(format!(
            "index rebuild failed with status {:?}",
            report.status
        ));
    }
    if report.documents_total != 1 {
        return Err(format!(
            "expected one indexed document, got {}",
            report.documents_total
        ));
    }
    Ok(())
}

fn run_ee(args: &[String]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn run_json_command(fixture: &JsonContractFixture, args: Vec<String>) -> Result<Value, String> {
    let output = run_ee(&args)?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("stdout was not UTF-8 for ee {}: {error}", args.join(" ")))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("stderr was not UTF-8 for ee {}: {error}", args.join(" ")))?;

    if !output.status.success() {
        return Err(format!(
            "ee {} failed with status {:?}; stderr: {stderr}; stdout: {stdout}",
            args.join(" "),
            output.status.code()
        ));
    }
    if !stderr.is_empty() {
        return Err(format!(
            "ee {} must keep JSON diagnostics out of stderr, got: {stderr:?}",
            args.join(" ")
        ));
    }
    if !stdout.ends_with('\n') {
        return Err(format!(
            "ee {} stdout must be newline-terminated JSON, got: {stdout:?}",
            args.join(" ")
        ));
    }

    let mut value: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("ee {} stdout must be JSON: {error}", args.join(" ")))?;
    scrub_json_contract(&mut value, fixture);
    Ok(value)
}

fn scrub_json_contract(value: &mut Value, fixture: &JsonContractFixture) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                scrub_json_contract(child, fixture);
                scrub_value_for_key(key, child);
            }
        }
        Value::Array(items) => {
            for item in items {
                scrub_json_contract(item, fixture);
            }
        }
        Value::String(text) => {
            *text = scrub_string(text, fixture);
        }
        Value::Number(_) | Value::Bool(_) | Value::Null => {}
    }
}

fn scrub_value_for_key(key: &str, value: &mut Value) {
    let normalized = key.to_ascii_lowercase();
    if normalized.contains("hash") || normalized.contains("fingerprint") {
        if value.is_string() {
            *value = Value::String("[HASH]".to_string());
        }
        return;
    }
    if normalized == "packid" || normalized == "pack_id" {
        if value.is_string() {
            *value = Value::String("[PACK_ID]".to_string());
        }
        return;
    }
    if is_timestamp_key(&normalized) {
        if value.is_string() {
            *value = Value::String("[TIMESTAMP]".to_string());
        }
        return;
    }
    if is_elapsed_key(&normalized) && value.is_number() {
        *value = serde_json::json!(0);
        return;
    }
    if normalized.contains("freshness") && value.is_number() {
        *value = serde_json::json!(0);
    }
}

fn is_timestamp_key(key: &str) -> bool {
    matches!(
        key,
        "createdat"
            | "created_at"
            | "updatedat"
            | "updated_at"
            | "verifiedat"
            | "verified_at"
            | "completedat"
            | "completed_at"
            | "startedat"
            | "started_at"
            | "timestamp"
    )
}

fn is_elapsed_key(key: &str) -> bool {
    key.contains("elapsed")
        || key.contains("duration")
        || key.contains("latency")
        || key == "ms"
        || key.ends_with("ms")
}

fn scrub_string(text: &str, fixture: &JsonContractFixture) -> String {
    if text.starts_with("blake3:") || text.starts_with("sha256:") {
        return "[HASH]".to_string();
    }
    if text.starts_with("pack_") {
        return "[PACK_ID]".to_string();
    }
    if looks_like_rfc3339(text) {
        return "[TIMESTAMP]".to_string();
    }

    let database = fixture.database.to_string_lossy();
    let index_dir = fixture.index_dir.to_string_lossy();
    let workspace = fixture.workspace.to_string_lossy();
    text.replace(database.as_ref(), "[DATABASE]")
        .replace(index_dir.as_ref(), "[INDEX]")
        .replace(workspace.as_ref(), "[WORKSPACE]")
        .replace(env!("CARGO_MANIFEST_DIR"), "[REPO]")
}

fn looks_like_rfc3339(text: &str) -> bool {
    text.len() >= 20
        && text.as_bytes().get(4) == Some(&b'-')
        && text.as_bytes().get(7) == Some(&b'-')
        && text.as_bytes().get(10) == Some(&b'T')
        && (text.ends_with('Z') || text.contains("+00:00"))
}

#[test]
fn fixture_backed_agent_json_contracts_match_snapshots() -> TestResult {
    let fixture = JsonContractFixture::new()?;
    let workspace = fixture.workspace_arg();
    let database = fixture.database_arg();
    let index_dir = fixture.index_dir_arg();

    let status = run_json_command(
        &fixture,
        vec![
            "--json".to_string(),
            "--workspace".to_string(),
            workspace.clone(),
            "status".to_string(),
        ],
    )?;
    assert_json_snapshot!("status_json_contract", status);

    let doctor = run_json_command(
        &fixture,
        vec![
            "--json".to_string(),
            "--workspace".to_string(),
            workspace.clone(),
            "doctor".to_string(),
        ],
    )?;
    assert_json_snapshot!("doctor_json_contract", doctor);

    let search = run_json_command(
        &fixture,
        vec![
            "--json".to_string(),
            "--workspace".to_string(),
            workspace.clone(),
            "search".to_string(),
            QUERY.to_string(),
            "--database".to_string(),
            database.clone(),
            "--index-dir".to_string(),
            index_dir.clone(),
        ],
    )?;
    assert_json_snapshot!("search_json_contract", search);

    let why = run_json_command(
        &fixture,
        vec![
            "--json".to_string(),
            "--workspace".to_string(),
            workspace.clone(),
            "why".to_string(),
            MEMORY_ID.to_string(),
            "--database".to_string(),
            database.clone(),
        ],
    )?;
    assert_json_snapshot!("why_json_contract", why);

    let context = run_json_command(
        &fixture,
        vec![
            "--json".to_string(),
            "--workspace".to_string(),
            workspace,
            "context".to_string(),
            QUERY.to_string(),
            "--database".to_string(),
            database,
            "--index-dir".to_string(),
            index_dir,
            "--profile".to_string(),
            "compact".to_string(),
            "--max-tokens".to_string(),
            "4000".to_string(),
            "--candidate-pool".to_string(),
            "10".to_string(),
        ],
    )?;
    assert_json_snapshot!("context_json_contract", context);

    Ok(())
}
