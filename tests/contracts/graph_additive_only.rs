#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

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
        assert_additive_shape(name, "$", &expected, &actual)
            .map_err(|error| format!("{error}\n{}", baseline_provenance_note(surface)))?;
    }

    Ok(())
}

/// Where this surface's baseline came from, appended to every shape failure.
///
/// Three surfaces borrow a snapshot written by `tests/json_contract_snapshots.rs`
/// rather than captured from the command they are compared against. That is two
/// producers, and a borrowed fixture can legitimately declare a field the live
/// command never emits -- `$.data.degraded[embed_model_unavailable]` is exactly
/// that: a literal at json_contract_snapshots.rs:1209, emitted live only when
/// EE_EMBED_MODEL_PATH points at a missing path, which this fixture never sets.
///
/// Without this note the failure reads as a REMOVAL, and three panes spent a
/// cycle hunting for the commit that stopped emitting a code nothing had
/// stopped emitting. The comparison is unchanged; only the diagnosis is.
fn baseline_provenance_note(surface: &Value) -> String {
    let command = surface
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("<undeclared>");
    match surface.get("snapshot").and_then(Value::as_str) {
        Some(snapshot) => format!(
            "baseline provenance: BORROWED from `tests/snapshots/{snapshot}`, which is written by \
             another test, not captured from `{command}`. A field present in that fixture and \
             absent here may never have been emitted by this surface at all -- check whether the \
             live command can produce it before treating this as a removal."
        ),
        None => format!(
            "baseline provenance: inline in the manifest, declared for `{command}`. If this \
             surface's output legitimately changed, the manifest entry is what needs updating."
        ),
    }
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
            "pack".to_owned(),
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
    graph_payload_for_baseline(name, run_json(&args)?)
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

fn graph_payload_for_baseline(surface: &str, actual: Value) -> Result<Value, String> {
    if surface != "health" {
        return Ok(actual);
    }

    if actual.get("schema").and_then(Value::as_str) != Some("ee.response.v2") {
        let schema = actual.get("schema").and_then(Value::as_str);
        return Err(format!(
            "health graph baseline response schema must be ee.response.v2, got {schema:?}"
        ));
    }

    if actual.get("success").and_then(Value::as_bool) != Some(true) {
        return Err("health graph baseline response envelope must be successful".to_owned());
    }

    let data = actual
        .get("data")
        .ok_or_else(|| "health graph baseline response envelope missing data".to_owned())?;
    let inner_schema = data.get("schema").and_then(Value::as_str);
    if inner_schema != Some("ee.health.structural.v1") {
        return Err(format!(
            "health graph baseline response data.schema must be ee.health.structural.v1, got {inner_schema:?}"
        ));
    }

    Ok(actual)
}

#[test]
fn graph_payload_for_baseline_requires_health_response_v2() -> TestResult {
    let payload = serde_json::json!({
        "schema": "ee.health.structural.v1",
        "summary": {
            "critical": 0,
            "warning": 1
        }
    });
    let response = serde_json::json!({
        "schema": "ee.response.v2",
        "success": true,
        "data": payload,
        "degraded": []
    });

    let normalized = graph_payload_for_baseline("health", response)?;
    assert_eq!(
        normalized.get("schema").and_then(Value::as_str),
        Some("ee.response.v2")
    );
    assert_eq!(
        normalized
            .pointer("/data/summary")
            .and_then(Value::as_object)
            .map(|summary| summary.len()),
        Some(2)
    );
    Ok(())
}

#[test]
fn graph_payload_for_baseline_rejects_health_envelope_schema_drift() {
    let response = serde_json::json!({
        "schema": "ee.response.v2",
        "success": true,
        "data": {
            "schema": "ee.health.other.v1"
        },
        "degraded": []
    });

    let error = graph_payload_for_baseline("health", response)
        .expect_err("health graph payload schema drift must fail clearly");
    assert!(error.contains("ee.health.structural.v1"));
}

#[test]
fn graph_payload_for_baseline_rejects_bare_health_payload() {
    let bare_payload = serde_json::json!({
        "schema": "ee.health.structural.v1",
        "summary": {
            "critical": 0,
            "warning": 1
        }
    });

    let error = graph_payload_for_baseline("health", bare_payload)
        .expect_err("old bare health structural payload must fail the public-surface baseline");
    assert!(error.contains("ee.response.v2"));
}

fn run_ee(args: &[String]) -> Result<Output, String> {
    crate::common_spawn::serialized_real_ee(args)
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
                // Entries carrying a stable identity are matched BY it, never by
                // position. `degraded[]` is the motivating case: a NEW degradation
                // code appearing ahead of an existing one shifts every later entry,
                // and a positional comparison then reports the shifted entry's
                // fields as REMOVED -- inside a check whose entire purpose is to
                // permit additions. bv27 read exactly that way:
                //
                //   context removed JSON field $.data.degraded[0].details
                //
                // with every entry still present, `embed_model_unavailable` still
                // mapped to its Rebuild recovery action, and `details` still being
                // built for it. Nothing had been removed; something had been added
                // in front.
                //
                // Identity matching is strictly STRONGER than positional, not
                // weaker: an entry that is genuinely gone still fails, because its
                // identity is absent from `actual`. Only the false positive is
                // removed, and the path in the message names the entry rather than
                // an index that shifts.
                let (child_path, actual_child) = match entry_identity(expected_item) {
                    Some(identity) => {
                        let Some(found) = actual_items.iter().find(|item| {
                            entry_identity(item).as_deref() == Some(identity.as_str())
                        }) else {
                            return Err(format!(
                                "{surface} removed array entry {path}[{identity}]"
                            ));
                        };
                        (format!("{path}[{identity}]"), found)
                    }
                    None => (format!("{path}[{index}]"), &actual_items[index]),
                };
                assert_additive_shape(surface, &child_path, expected_item, actual_child)?;
            }
        }
        _ => {} // Primitive values (bool/string/number) are compared only by JSON
                // type. A true->false or "ok"->"degraded" flip is in scope for
                // insta contract snapshots, not this additive-shape gate.
    }

    Ok(())
}

/// A stable identity for an array entry, when it has one.
///
/// Degradation entries are identified by `code`; other identified records on
/// these surfaces use `id` or `name`. An entry with none of those is an ordered
/// value (a string, a number, a positional tuple) and stays positional, so this
/// only changes behaviour for arrays whose elements are addressable records.
/// The negative the identity relaxation depends on.
///
/// Matching identified entries by identity instead of position RELAXES a
/// comparison inside an additive-only checker, which is the diff shape that
/// deserves the most distrust. The argument that it is nevertheless strictly
/// stronger rests entirely on one property: an entry that is genuinely gone
/// must still fail. That property cannot be established by the reasoning that
/// motivated the change, so it is pinned here instead of asserted in a commit
/// message.
///
/// Both directions are exercised, because either alone is satisfiable by a
/// checker that is simply wrong in the other direction.
#[test]
fn additive_shape_accepts_a_prepended_entry_but_still_rejects_a_removed_one() -> TestResult {
    let expected = serde_json::json!({
        "degraded": [
            {"code": "embed_model_unavailable", "details": {"recovery": []}},
            {"code": "context_evidence_freshness_missing_source"}
        ]
    });

    // POSITIVE: an addition that displaces index 0. This is the bv27 symptom --
    // positional matching reported it as
    // `context removed JSON field $.data.degraded[0].details` with every entry
    // still present.
    let with_prepended_entry = serde_json::json!({
        "degraded": [
            {"code": "neural_local_unconfirmed"},
            {"code": "embed_model_unavailable", "details": {"recovery": []}},
            {"code": "context_evidence_freshness_missing_source"}
        ]
    });
    assert_additive_shape("context", "$", &expected, &with_prepended_entry)?;

    // NEGATIVE: a genuine removal, at the SAME array length so the length guard
    // above cannot be what catches it. Only identity matching can.
    let with_entry_replaced = serde_json::json!({
        "degraded": [
            {"code": "neural_local_unconfirmed"},
            {"code": "context_evidence_freshness_missing_source"}
        ]
    });
    let Err(message) = assert_additive_shape("context", "$", &expected, &with_entry_replaced)
    else {
        return Err(
            "removing an identified entry must fail the additive-shape check, but it passed"
                .to_owned(),
        );
    };
    if !message.contains("embed_model_unavailable") {
        return Err(format!(
            "the removal failure must name the missing entry; got: {message}"
        ));
    }
    Ok(())
}

/// Values are not this contract. `qos.registryHealthy` flipping true -> false
/// (the motivating example on bd-9qvos) must PASS here. This gate compares
/// live CLI output to a frozen borrowed snapshot; generations, timestamps,
/// and QoS flags are volatile across those two producers. Pinning values
/// would turn an additive-shape check into an exact-match against
/// `json_contract_snapshots`. Value coverage lives in
/// `fixture_backed_agent_json_contracts_match_snapshots` (status/why/context
/// insta), `read_pool_status_schema` (qos types), `health_structural.snap`,
/// and `curate_candidates_after_seed_matches_snapshot`.
///
/// The planted negative is the key removal: same object, still a bool-typed
/// sibling, but the field is gone. That must still fail.
#[test]
fn additive_shape_permits_boolean_value_flips_and_still_rejects_key_removal() -> TestResult {
    let expected = serde_json::json!({
        "qos": { "registryHealthy": true },
        "summary": { "status": "ok" }
    });
    let flipped = serde_json::json!({
        "qos": { "registryHealthy": false },
        "summary": { "status": "degraded" }
    });
    assert_additive_shape("status", "$", &expected, &flipped)?;

    let removed = serde_json::json!({
        "qos": {},
        "summary": { "status": "ok" }
    });
    let Err(message) = assert_additive_shape("status", "$", &expected, &removed) else {
        return Err(
            "removing qos.registryHealthy must fail the additive-shape check, but it passed"
                .to_owned(),
        );
    };
    if !message.contains("qos.registryHealthy") {
        return Err(format!(
            "the removal failure must name qos.registryHealthy; got: {message}"
        ));
    }
    Ok(())
}

/// Numbers count as identities too. An earlier revision accepted only strings,
/// so an entry keyed `"id": 7` silently fell back to positional matching and
/// kept the exact false-positive this function exists to remove -- quietly,
/// because the fallback is the old behaviour and nothing fails.
fn entry_identity(value: &Value) -> Option<String> {
    let object = value.as_object()?;
    ["code", "id", "name"]
        .into_iter()
        .find_map(|key| match object.get(key) {
            Some(Value::String(text)) => Some(text.clone()),
            Some(Value::Number(number)) => Some(number.to_string()),
            _ => None,
        })
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
                pack_hash: format!(
                    "blake3:{}",
                    blake3::hash(b"graph additive-only fixture pack").to_hex()
                ),
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
                combined_score: None,
                attempt_family_multiplicity: None,
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
