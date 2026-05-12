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
const TIE_WORKSPACE_ID: &str = "wsp_searchtie00000000000000001";
const TIE_QUERY: &str = "stable equal ranking tie release check";
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
        workflow_id: None,
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

fn seed_tie_workspace(workspace: &Path, database: &Path) -> TestResult {
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
            TIE_WORKSPACE_ID,
            &CreateWorkspaceInput {
                path: workspace.to_string_lossy().into_owned(),
                name: Some("search-tie-determinism".to_owned()),
            },
        )
        .map_err(|error| error.to_string())?;

    let memory_ids = [
        "mem_00000000000000000000020005",
        "mem_00000000000000000000020002",
        "mem_00000000000000000000020004",
        "mem_00000000000000000000020001",
        "mem_00000000000000000000020003",
    ];

    for id in memory_ids {
        connection
            .insert_memory(
                id,
                &CreateMemoryInput {
                    workspace_id: TIE_WORKSPACE_ID.to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: TIE_QUERY.to_owned(),
                    workflow_id: None,
                    confidence: 0.80,
                    utility: 0.80,
                    importance: 0.80,
                    provenance_uri: Some("fixture://tests/search_deterministic_golden".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: Some("search-tie-determinism".to_owned()),
                    tags: ["release", "tie", "determinism"]
                        .iter()
                        .map(|tag| (*tag).to_owned())
                        .collect(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
    }

    connection
        .execute_raw(
            "UPDATE memories \
             SET created_at = '2026-04-30T12:00:00+00:00', \
                 updated_at = '2026-04-30T12:00:00+00:00' \
             WHERE workspace_id = 'wsp_searchtie00000000000000001'",
        )
        .map_err(|error| error.to_string())?;

    connection.close().map_err(|error| error.to_string())
}

fn rebuild_search_index_with_expected(
    workspace: &Path,
    database: &Path,
    index_dir: &Path,
    expected_documents: u32,
) -> TestResult {
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
    ensure_equal(
        &report.documents_total,
        &expected_documents,
        "indexed document count",
    )
}

fn rebuild_search_index(workspace: &Path, database: &Path, index_dir: &Path) -> TestResult {
    rebuild_search_index_with_expected(workspace, database, index_dir, 3)
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
    run_search_json_for_query(workspace, database, index_dir, QUERY, 3)
}

fn run_search_json_for_query(
    workspace: &Path,
    database: &Path,
    index_dir: &Path,
    query: &str,
    limit: u32,
) -> Result<String, String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--json")
        .arg("--workspace")
        .arg(workspace)
        .arg("search")
        .arg(query)
        .arg("--database")
        .arg(database)
        .arg("--index-dir")
        .arg(index_dir)
        .arg("--limit")
        .arg(limit.to_string())
        .arg("--relevance-floor")
        .arg("0.0")
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

fn search_doc_ids(value: &JsonValue) -> Result<Vec<String>, String> {
    value["data"]["results"]
        .as_array()
        .ok_or_else(|| "search results must be an array".to_owned())?
        .iter()
        .map(|result| {
            result["docId"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "search result docId must be a string".to_owned())
        })
        .collect()
}

fn assert_result_scores_non_increasing(value: &JsonValue) -> TestResult {
    let results = value["data"]["results"]
        .as_array()
        .ok_or_else(|| "search results must be an array".to_owned())?;

    for pair in results.windows(2) {
        let left_score = pair[0]["score"]
            .as_f64()
            .ok_or_else(|| "left tie-fixture search score must be numeric".to_owned())?;
        let right_score = pair[1]["score"]
            .as_f64()
            .ok_or_else(|| "right tie-fixture search score must be numeric".to_owned())?;
        ensure(
            left_score >= right_score,
            format!(
                "tie-fixture search ranking must be non-increasing: {left_score} before {right_score}"
            ),
        )?;
    }

    Ok(())
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
                sort_object_keys(metadata);
            }
        }
    }

    serde_json::to_string_pretty(&value)
        .map(|json| format!("{json}\n"))
        .map_err(|error| format!("failed to canonicalize search JSON: {error}"))
}

fn sort_object_keys(value: &mut JsonValue) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let mut entries = std::mem::take(object).into_iter().collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    for (key, child) in entries {
        object.insert(key, child);
    }
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
            "mem_00000000000000000000010002",
            "mem_00000000000000000000010003",
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
        ensure(
            result["why"].is_string(),
            format!("search result must include top-level why: {result:?}"),
        )?;
        ensure(
            result["provenance"].is_array(),
            format!("search result must include normalized provenance: {result:?}"),
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
    let mut observed_lexical_or_hybrid = 0;
    for result in results {
        let source = result["source"]
            .as_str()
            .ok_or_else(|| "search result source must be a string".to_owned())?;
        if source == "semantic_fast" {
            observed_semantic_fast += 1;
        }
        if matches!(source, "lexical" | "hybrid") {
            observed_lexical_or_hybrid += 1;
        }
        ensure(
            source_counts.get(source_count_key(source)).is_some(),
            format!("sourceCounts must include camelCase key for {source}"),
        )?;
    }
    ensure(
        observed_lexical_or_hybrid > 0,
        "deterministic search golden must include lexical or hybrid evidence",
    )?;
    ensure_equal(
        &source_counts["semanticFast"],
        &serde_json::json!(observed_semantic_fast),
        "semantic fast source count",
    )?;
    let lexical_count = source_counts["lexical"]
        .as_u64()
        .ok_or_else(|| "lexical source count must be numeric".to_owned())?;
    let hybrid_count = source_counts["hybrid"]
        .as_u64()
        .ok_or_else(|| "hybrid source count must be numeric".to_owned())?;
    ensure(
        lexical_count + hybrid_count > 0,
        "search metrics must count lexical or hybrid evidence",
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

#[test]
fn identical_content_search_ties_order_by_memory_id_across_runs() -> TestResult {
    let artifact_dir = unique_artifact_dir("search-equal-score-ties")?;
    let workspace = artifact_dir.join("workspace");
    let database = workspace.join(".ee").join("ee.db");
    let index_dir = workspace.join(".ee").join("index");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;

    seed_tie_workspace(&workspace, &database)?;
    rebuild_search_index_with_expected(&workspace, &database, &index_dir, 5)?;

    let expected_doc_ids = vec![
        "mem_00000000000000000000020001".to_owned(),
        "mem_00000000000000000000020002".to_owned(),
        "mem_00000000000000000000020003".to_owned(),
        "mem_00000000000000000000020004".to_owned(),
        "mem_00000000000000000000020005".to_owned(),
    ];
    let mut first_canonical = None;

    for run in 0..10 {
        let canonical = canonicalize_search_json(&run_search_json_for_query(
            &workspace, &database, &index_dir, TIE_QUERY, 5,
        )?)?;
        let value: JsonValue = serde_json::from_str(&canonical)
            .map_err(|error| format!("run {run}: canonical JSON invalid: {error}"))?;

        ensure_equal(
            &value["data"]["resultCount"],
            &serde_json::json!(5),
            &format!("run {run} search result count"),
        )?;
        assert_result_scores_non_increasing(&value)?;
        ensure_equal(
            &search_doc_ids(&value)?,
            &expected_doc_ids,
            &format!("run {run} equal-score docId ordering"),
        )?;

        if let Some(first) = &first_canonical {
            ensure_equal(
                &canonical,
                first,
                &format!("run {run} canonical search JSON stability"),
            )?;
        } else {
            first_canonical = Some(canonical);
        }
    }

    Ok(())
}
