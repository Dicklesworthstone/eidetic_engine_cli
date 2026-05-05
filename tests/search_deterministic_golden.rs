//! Golden coverage for deterministic `ee search --json` ranking.
//!
//! The fixture uses fixed database IDs and timestamps, rebuilds a real
//! Frankensearch index with the hash embedder fallback, and compares the
//! canonicalized CLI JSON output byte-for-byte against a committed golden file.

use ee::core::index::{IndexRebuildOptions, IndexRebuildStatus, rebuild_index};
use ee::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};
use ee::search::{Embedder, HashEmbedder};
use serde_json::Value as JsonValue;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;

const WORKSPACE_ID: &str = "wsp_searchgolden00000000000001";
const QUERY: &str = "cargo test before release";
const GOLDEN_CATEGORY: &str = "agent";
const GOLDEN_NAME: &str = "search_deterministic_ranking.json";

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn ensure_json_number_close(
    actual: &JsonValue,
    expected: &JsonValue,
    epsilon: f64,
    context: &str,
) -> TestResult {
    let actual = actual
        .as_f64()
        .ok_or_else(|| format!("{context}: actual value is not numeric: {actual:?}"))?;
    let expected = expected
        .as_f64()
        .ok_or_else(|| format!("{context}: expected value is not numeric: {expected:?}"))?;

    if (actual - expected).abs() <= epsilon {
        Ok(())
    } else {
        Err(format!(
            "{context}: expected {expected} within {epsilon}, got {actual}"
        ))
    }
}

fn source_count_key(source: &str) -> &str {
    match source {
        "semantic_fast" => "semanticFast",
        "semantic_quality" => "semanticQuality",
        other => other,
    }
}

fn target_root() -> PathBuf {
    env::var_os("CARGO_TARGET_TMPDIR")
        .or_else(|| env::var_os("CARGO_TARGET_DIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"))
}

fn unique_artifact_dir(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let dir = target_root()
        .join("ee-search-golden-artifacts")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create artifact dir {}: {error}", dir.display()))?;
    Ok(dir)
}

fn golden_path(category: &str, name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join(category)
        .join(format!("{name}.golden"))
}

fn actual_path(name: &str) -> Result<PathBuf, String> {
    let dir = target_root().join("ee-search-golden-actual");
    fs::create_dir_all(&dir).map_err(|error| {
        format!(
            "failed to create golden actual dir {}: {error}",
            dir.display()
        )
    })?;
    Ok(dir.join(format!("{name}.actual")))
}

fn assert_golden(category: &str, name: &str, actual: &str) -> TestResult {
    let golden = golden_path(category, name);
    let expected = fs::read_to_string(&golden).map_err(|error| {
        let actual_output = actual_path(name)
            .map(|path| {
                let _ = fs::write(&path, actual);
                path.display().to_string()
            })
            .unwrap_or_else(|write_error| format!("unavailable ({write_error})"));
        format!(
            "golden file missing: {}\nactual output written to: {actual_output}\nreview and commit this approved fixture\nerror: {error}\n\n{actual}",
            golden.display()
        )
    })?;

    if actual == expected {
        return Ok(());
    }

    let actual_output = actual_path(name)?;
    fs::write(&actual_output, actual).map_err(|error| {
        format!(
            "failed to write actual golden output {}: {error}",
            actual_output.display()
        )
    })?;

    Err(format!(
        "golden mismatch for {category}/{name}\nexpected: {}\nactual: {}\n\n{}",
        golden.display(),
        actual_output.display(),
        line_diff(&expected, actual)
    ))
}

fn line_diff(expected: &str, actual: &str) -> String {
    let expected_lines = expected.lines().collect::<Vec<_>>();
    let actual_lines = actual.lines().collect::<Vec<_>>();
    let max = expected_lines.len().max(actual_lines.len());
    let mut output = String::from("--- expected\n+++ actual\n");

    for index in 0..max {
        match (expected_lines.get(index), actual_lines.get(index)) {
            (Some(left), Some(right)) if left == right => {}
            (Some(left), Some(right)) => {
                output.push_str(&format!("@@ line {} @@\n", index + 1));
                output.push_str(&format!("-{left}\n"));
                output.push_str(&format!("+{right}\n"));
            }
            (Some(left), None) => {
                output.push_str(&format!("@@ line {} @@\n", index + 1));
                output.push_str(&format!("-{left}\n"));
            }
            (None, Some(right)) => {
                output.push_str(&format!("@@ line {} @@\n", index + 1));
                output.push_str(&format!("+{right}\n"));
            }
            (None, None) => {}
        }
    }

    output
}

fn memory_input(
    level: &str,
    kind: &str,
    content: &str,
    confidence: f32,
    utility: f32,
    importance: f32,
    tags: &[&str],
) -> CreateMemoryInput {
    CreateMemoryInput {
        workspace_id: WORKSPACE_ID.to_owned(),
        level: level.to_owned(),
        kind: kind.to_owned(),
        content: content.to_owned(),
        confidence,
        utility,
        importance,
        provenance_uri: Some("fixture://tests/search_deterministic_golden".to_owned()),
        trust_class: "human_explicit".to_owned(),
        trust_subclass: Some("search-ranking-golden".to_owned()),
        tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
        valid_from: None,
        valid_to: None,
    }
}

fn seed_search_workspace(workspace: &Path, database: &Path) -> TestResult {
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
            WORKSPACE_ID,
            &CreateWorkspaceInput {
                path: workspace.to_string_lossy().into_owned(),
                name: Some("search-deterministic-golden".to_owned()),
            },
        )
        .map_err(|error| error.to_string())?;

    let memories = [
        (
            "mem_00000000000000000000010001",
            "procedural",
            "rule",
            "Before release, run cargo fmt --check and cargo test to verify formatting.",
            0.95,
            0.90,
            0.85,
            ["cargo", "release", "testing"].as_slice(),
            "2026-04-30T10:00:00+00:00",
        ),
        (
            "mem_00000000000000000000010002",
            "procedural",
            "rule",
            "Before pushing to main, run cargo test and inspect failing integration logs.",
            0.88,
            0.82,
            0.77,
            ["cargo", "main", "testing"].as_slice(),
            "2026-04-30T10:01:00+00:00",
        ),
        (
            "mem_00000000000000000000010003",
            "episodic",
            "fact",
            "A prior release failed because formatting checks were skipped before packaging.",
            0.70,
            0.62,
            0.60,
            ["release", "incident", "formatting"].as_slice(),
            "2026-04-30T10:02:00+00:00",
        ),
    ];

    for (id, level, kind, content, confidence, utility, importance, tags, timestamp) in memories {
        connection
            .insert_memory(
                id,
                &memory_input(level, kind, content, confidence, utility, importance, tags),
            )
            .map_err(|error| error.to_string())?;
        connection
            .execute_raw(&format!(
                "UPDATE memories SET created_at = '{timestamp}', updated_at = '{timestamp}' WHERE id = '{id}'"
            ))
            .map_err(|error| error.to_string())?;
    }

    connection.close().map_err(|error| error.to_string())
}

fn rebuild_search_index(workspace: &Path, database: &Path, index_dir: &Path) -> TestResult {
    let report = rebuild_index(&IndexRebuildOptions {
        workspace_path: workspace.to_path_buf(),
        database_path: Some(database.to_path_buf()),
        index_dir: Some(index_dir.to_path_buf()),
        dry_run: false,
    })
    .map_err(|error| error.to_string())?;

    ensure_equal(
        &report.status,
        &IndexRebuildStatus::Success,
        "index rebuild status",
    )?;
    ensure_equal(&report.documents_total, &3, "indexed document count")
}

fn assert_hash_embedder_determinism() -> TestResult {
    let fast = HashEmbedder::default_256();
    let quality = HashEmbedder::default_384();
    ensure_equal(&fast.id(), &"fnv1a-256", "fast hash embedder id")?;
    ensure_equal(&quality.id(), &"fnv1a-384", "quality hash embedder id")?;

    let first = fast.embed_sync(QUERY);
    let second = fast.embed_sync(QUERY);
    ensure_equal(&first, &second, "hash embedder output determinism")
}

fn run_search_json(workspace: &Path, database: &Path, index_dir: &Path) -> Result<String, String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--json")
        .arg("--workspace")
        .arg(workspace)
        .arg("search")
        .arg(QUERY)
        .arg("--database")
        .arg(database)
        .arg("--index-dir")
        .arg(index_dir)
        .arg("--limit")
        .arg("3")
        .arg("--explain")
        .output()
        .map_err(|error| format!("failed to run ee search --json: {error}"))?;

    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("search stdout was not UTF-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("search stderr was not UTF-8: {error}"))?;

    ensure(
        output.status.success(),
        format!("search --json should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        format!("search --json stderr must be empty, got: {stderr:?}"),
    )?;
    ensure(
        stdout.starts_with('{'),
        format!("search stdout must start with JSON data, got: {stdout:?}"),
    )?;
    ensure(
        stdout.ends_with('\n'),
        format!("search stdout must end with newline, got: {stdout:?}"),
    )?;
    ensure(
        !stdout.contains("\u{1b}["),
        "search JSON stdout must not contain ANSI escapes",
    )?;

    Ok(stdout)
}

fn canonicalize_search_json(stdout: &str) -> Result<String, String> {
    let mut value: JsonValue =
        serde_json::from_str(stdout).map_err(|error| format!("stdout must be JSON: {error}"))?;
    value["data"]["elapsedMs"] = serde_json::json!(0.0);
    value["data"]["metrics"]["elapsedMs"] = serde_json::json!(0.0);

    if let Some(results) = value["data"]["results"].as_array_mut() {
        for result in results {
            if let Some(metadata) = result.get_mut("metadata") {
                metadata["workspace"] = serde_json::json!("[WORKSPACE]");
            }
        }
    }

    serde_json::to_string_pretty(&value)
        .map(|json| format!("{json}\n"))
        .map_err(|error| format!("failed to canonicalize search JSON: {error}"))
}

fn assert_search_contract(value: &JsonValue) -> TestResult {
    ensure_equal(
        &value["schema"],
        &serde_json::json!("ee.response.v1"),
        "search response schema",
    )?;
    ensure_equal(
        &value["success"],
        &serde_json::json!(true),
        "search success",
    )?;
    ensure_equal(
        &value["data"]["command"],
        &serde_json::json!("search"),
        "search command",
    )?;
    ensure_equal(
        &value["data"]["query"],
        &serde_json::json!(QUERY),
        "search query",
    )?;
    ensure_equal(
        &value["data"]["resultCount"],
        &serde_json::json!(3),
        "search result count",
    )?;
    ensure_equal(
        &value["data"]["metrics"]["requestedLimit"],
        &serde_json::json!(3),
        "requested limit",
    )?;
    ensure_equal(
        &value["data"]["metrics"]["returnedCount"],
        &serde_json::json!(3),
        "returned count",
    )?;
    ensure_equal(
        &value["data"]["metrics"]["errorCount"],
        &serde_json::json!(0),
        "search error count",
    )?;

    let results = value["data"]["results"]
        .as_array()
        .ok_or_else(|| "search results must be an array".to_owned())?;
    let doc_ids = results
        .iter()
        .map(|result| {
            result["docId"]
                .as_str()
                .ok_or_else(|| "search result docId must be a string".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    ensure_equal(
        &doc_ids,
        &vec![
            "mem_00000000000000000000010001",
            "mem_00000000000000000000010003",
            "mem_00000000000000000000010002",
        ],
        "deterministic search ranking",
    )?;

    for result in results {
        ensure(
            result["score"].is_number(),
            format!("search result score must be numeric: {result:?}"),
        )?;
        ensure(
            result["source"].is_string(),
            format!("search result source must be a string: {result:?}"),
        )?;
        ensure(
            result["explanation"]["factors"].is_array(),
            format!("search result must include explanation factors: {result:?}"),
        )?;
    }

    for pair in results.windows(2) {
        let left_score = pair[0]["score"]
            .as_f64()
            .ok_or_else(|| "left search score must be numeric".to_owned())?;
        let right_score = pair[1]["score"]
            .as_f64()
            .ok_or_else(|| "right search score must be numeric".to_owned())?;
        ensure(
            left_score >= right_score,
            format!("search ranking must be non-increasing: {left_score} before {right_score}"),
        )?;
    }

    let source_counts = &value["data"]["metrics"]["sourceCounts"];
    let mut observed_semantic_fast = 0;
    for result in results {
        let source = result["source"]
            .as_str()
            .ok_or_else(|| "search result source must be a string".to_owned())?;
        if source == "semantic_fast" {
            observed_semantic_fast += 1;
        }
        ensure(
            source_counts.get(source_count_key(source)).is_some(),
            format!("sourceCounts must include camelCase key for {source}"),
        )?;
    }
    ensure_equal(
        &source_counts["semanticFast"],
        &serde_json::json!(observed_semantic_fast),
        "semantic fast source count",
    )?;
    ensure_json_number_close(
        &value["data"]["metrics"]["scoreDistribution"]["top"],
        &results[0]["score"],
        0.000_001,
        "score distribution top tracks first result",
    )?;
    ensure_json_number_close(
        &value["data"]["metrics"]["scoreDistribution"]["max"],
        &results[0]["score"],
        0.000_001,
        "score distribution max tracks first result",
    )?;
    ensure_json_number_close(
        &value["data"]["metrics"]["scoreDistribution"]["min"],
        &results[2]["score"],
        0.000_001,
        "score distribution min tracks last result",
    )?;

    Ok(())
}

#[test]
fn search_json_deterministic_ranking_matches_golden() -> TestResult {
    assert_hash_embedder_determinism()?;

    let artifact_dir = unique_artifact_dir("search-deterministic-ranking")?;
    let workspace = artifact_dir.join("workspace");
    let database = workspace.join(".ee").join("ee.db");
    let index_dir = workspace.join(".ee").join("index");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;

    seed_search_workspace(&workspace, &database)?;
    rebuild_search_index(&workspace, &database, &index_dir)?;

    let first = canonicalize_search_json(&run_search_json(&workspace, &database, &index_dir)?)?;
    let second = canonicalize_search_json(&run_search_json(&workspace, &database, &index_dir)?)?;
    ensure_equal(&first, &second, "canonical search JSON rerun stability")?;

    let value: JsonValue =
        serde_json::from_str(&first).map_err(|error| format!("canonical JSON invalid: {error}"))?;
    assert_search_contract(&value)?;
    assert_golden(GOLDEN_CATEGORY, GOLDEN_NAME, &first)
}
