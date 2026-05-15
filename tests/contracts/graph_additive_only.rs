use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ee::core::index::{IndexRebuildOptions, IndexRebuildStatus, rebuild_index};
use ee::db::{
    CreateMemoryInput, CreatePackItemInput, CreatePackRecordInput, CreateWorkspaceInput,
    DbConnection,
};
use ee::models::WorkspaceId;
use serde_json::Value;

type TestResult = Result<(), String>;

const MEMORY_ID: &str = "mem_00000000000000000000000001";
const PACK_ID: &str = "pack_00000000000000000000000001";
const QUERY: &str = "format before release";

#[derive(Debug)]
struct Fixture {
    workspace: PathBuf,
    database: PathBuf,
    index_dir: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self, String> {
        let artifact_parent = artifact_parent()?;
        let artifact_dir = tempfile::Builder::new()
            .prefix("ee-graph-additive-only-")
            .tempdir_in(&artifact_parent)
            .map(tempfile::TempDir::keep)
            .map_err(|error| format!("failed to create additive-only artifact dir: {error}"))?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");

        fs::create_dir_all(&workspace)
            .map_err(|error| format!("failed to create {}: {error}", workspace.display()))?;
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

fn artifact_parent() -> Result<PathBuf, String> {
    let system_tmp = PathBuf::from("/tmp");
    if system_tmp.is_dir() {
        return Ok(system_tmp);
    }

    if let Some(path) = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
    {
        return Ok(path);
    }

    let temp_dir = std::env::temp_dir();
    if temp_dir.is_dir() {
        return Ok(temp_dir);
    }

    let fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("tmp");
    fs::create_dir_all(&fallback).map_err(|error| {
        format!(
            "failed to create fallback temp dir {}: {error}",
            fallback.display()
        )
    })?;
    Ok(fallback)
}

#[test]
fn graph_surfaces_preserve_pre_epic_json_shape_additively() -> TestResult {
    let fixture = Fixture::new()?;
    let baseline = baseline_manifest()?;
    let surfaces = baseline
        .get("surfaces")
        .and_then(Value::as_array)
        .ok_or_else(|| "graph baseline manifest must contain surfaces[]".to_owned())?;

    for surface in surfaces {
        let name = surface
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "baseline surface missing name".to_owned())?;
        let expected = baseline_for_surface(surface)?;
        let actual = current_surface(name, &fixture)?;
        assert_additive_shape(name, "$", &expected, &actual)?;
    }

    Ok(())
}

fn baseline_manifest() -> Result<Value, String> {
    read_snapshot_json(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("snapshots")
            .join("graph_baseline_pre_epic.snap"),
    )
}

fn baseline_for_surface(surface: &Value) -> Result<Value, String> {
    if let Some(snapshot) = surface.get("snapshot").and_then(Value::as_str) {
        return read_snapshot_json(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join("snapshots")
                .join(snapshot),
        );
    }
    surface
        .get("baseline")
        .cloned()
        .ok_or_else(|| "baseline surface must define snapshot or baseline".to_owned())
}

fn read_snapshot_json(path: &Path) -> Result<Value, String> {
    let text =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let body = text
        .splitn(3, "---")
        .nth(2)
        .ok_or_else(|| format!("{} must be an insta-style snapshot", path.display()))?;
    serde_json::from_str(body.trim())
        .map_err(|error| format!("parse JSON body from {}: {error}", path.display()))
}

fn current_surface(name: &str, fixture: &Fixture) -> Result<Value, String> {
    let workspace = fixture.workspace_arg();
    let database = fixture.database_arg();
    let index_dir = fixture.index_dir_arg();
    let args = match name {
        "status" => vec![
            "--json".to_owned(),
            "--workspace".to_owned(),
            workspace,
            "status".to_owned(),
        ],
        "why" => {
            let search_args = vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace.clone(),
                "search".to_owned(),
                QUERY.to_owned(),
                "--database".to_owned(),
                database.clone(),
                "--index-dir".to_owned(),
                index_dir,
            ];
            run_json(&search_args)?;
            vec![
                "--json".to_owned(),
                "--workspace".to_owned(),
                workspace,
                "why".to_owned(),
                MEMORY_ID.to_owned(),
                "--database".to_owned(),
                database,
            ]
        }
        "context" => vec![
            "--json".to_owned(),
            "--workspace".to_owned(),
            workspace,
            "context".to_owned(),
            QUERY.to_owned(),
            "--database".to_owned(),
            database,
            "--index-dir".to_owned(),
            index_dir,
            "--profile".to_owned(),
            "compact".to_owned(),
            "--max-tokens".to_owned(),
            "4000".to_owned(),
            "--candidate-pool".to_owned(),
            "10".to_owned(),
        ],
        "curate" => vec![
            "--json".to_owned(),
            "--workspace".to_owned(),
            workspace,
            "curate".to_owned(),
            "candidates".to_owned(),
            "--all".to_owned(),
            "--database".to_owned(),
            database,
        ],
        "health" => vec![
            "--json".to_owned(),
            "--workspace".to_owned(),
            workspace,
            "health".to_owned(),
            "--robot-insights".to_owned(),
        ],
        other => return Err(format!("unknown graph baseline surface {other:?}")),
    };
    run_json(&args)
}

fn run_json(args: &[String]) -> Result<Value, String> {
    let output = run_ee(args)?;
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

    serde_json::from_str(&stdout)
        .map_err(|error| format!("ee {} stdout must be JSON: {error}", args.join(" ")))
}

fn run_ee(args: &[String]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn assert_additive_shape(
    surface: &str,
    path: &str,
    expected: &Value,
    actual: &Value,
) -> TestResult {
    if expected.is_null() {
        return Ok(());
    }

    let expected_type = json_type(expected);
    let actual_type = json_type(actual);
    if expected_type != actual_type {
        return Err(format!(
            "{surface} changed JSON type at {path}: expected {expected_type}, got {actual_type}"
        ));
    }

    match (expected, actual) {
        (Value::Object(expected_object), Value::Object(actual_object)) => {
            for (key, expected_child) in expected_object {
                let child_path = format!("{path}.{key}");
                let actual_child = actual_object
                    .get(key)
                    .ok_or_else(|| format!("{surface} removed JSON field {child_path}"))?;
                assert_additive_shape(surface, &child_path, expected_child, actual_child)?;
            }
        }
        (Value::Array(expected_items), Value::Array(actual_items)) => {
            if actual_items.len() < expected_items.len() {
                return Err(format!(
                    "{surface} removed populated array entries at {path}: expected at least {}, got {}",
                    expected_items.len(),
                    actual_items.len()
                ));
            }
            for (index, expected_item) in expected_items.iter().enumerate() {
                assert_additive_shape(
                    surface,
                    &format!("{path}[{index}]"),
                    expected_item,
                    &actual_items[index],
                )?;
            }
        }
        _ => {}
    }

    Ok(())
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn seed_workspace(workspace: &Path, database: &Path) -> TestResult {
    let workspace_id = stable_workspace_id(workspace);

    if let Some(parent) = database.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }

    let connection = DbConnection::open_file(database).map_err(|error| error.to_string())?;
    connection.migrate().map_err(|error| error.to_string())?;
    connection
        .insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: workspace.to_string_lossy().into_owned(),
                name: Some("graph-additive-only".to_owned()),
            },
        )
        .map_err(|error| error.to_string())?;
    connection
        .insert_memory(
            MEMORY_ID,
            &CreateMemoryInput {
                workspace_id: workspace_id.clone(),
                level: "procedural".to_owned(),
                kind: "rule".to_owned(),
                content: "Run cargo fmt --check before release.".to_owned(),
                workflow_id: None,
                confidence: 0.92,
                utility: 0.8,
                importance: 0.7,
                provenance_uri: Some("file://AGENTS.md#L164-173".to_owned()),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: Some("project-rule".to_owned()),
                tags: vec!["cargo".to_owned(), "formatting".to_owned()],
                valid_from: None,
                valid_to: None,
            },
        )
        .map_err(|error| error.to_string())?;
    connection
        .insert_pack_record(
            PACK_ID,
            &CreatePackRecordInput {
                workspace_id,
                query: QUERY.to_owned(),
                profile: "compact".to_owned(),
                max_tokens: 4000,
                used_tokens: 8,
                item_count: 1,
                omitted_count: 0,
                pack_hash: "blake3:fixture-pack-hash".to_owned(),
                degraded_json: None,
                created_by: Some("graph-additive-only".to_owned()),
            },
            &[CreatePackItemInput {
                pack_id: PACK_ID.to_owned(),
                memory_id: MEMORY_ID.to_owned(),
                rank: 1,
                section: "procedural_rules".to_owned(),
                estimated_tokens: 8,
                relevance: 0.91,
                utility: 0.8,
                why: "Selected because the memory matches release-formatting work.".to_owned(),
                diversity_key: Some("procedural:rule:cargo".to_owned()),
                provenance_json: r#"{"schema":"ee.pack_item.provenance.v1","entries":[{"uri":"file://AGENTS.md#L164-173","trustClass":"human_explicit","trustSubclass":"project-rule"}]}"#.to_owned(),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: Some("project-rule".to_owned()),
            }],
            &[],
        )
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
    for (target, source) in bytes.iter_mut().zip(hash.as_bytes().iter()) {
        *target = *source;
    }
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
