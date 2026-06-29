//! No-mock end-to-end proof for insert-time embedding deduplication.
//!
//! The test runs the public `ee` binary against temporary workspaces, inspects
//! the real FrankenSQLite source-of-truth rows, pins a normalized golden, and
//! writes `ee.test_event.v1` proof events with measured timings.

use std::fmt::Debug;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ee::db::{DbConnection, MemoryLinkRelation};
use ee::search::simhash::simhash_128;
use serde_json::{Value, json};

type TestResult = Result<(), String>;

const GOLDEN: &str = include_str!("golden/embed_dedup_no_mock_e2e.json");
const PERF_FIXTURE: &str = include_str!("fixtures/golden/perf_artifact/embed_dedup_insert.json");
const EXIT_SUCCESS: i32 = 0;
const EXACT_CONTENT: &str = "Agents must route Rust verification through RCH.";
const DUPLICATE_CONTENT: &str = "  Agents must route Rust verification through RCH.  ";
const FALSE_POSITIVE_CONTENT: &str =
    "Offline semantic fixtures should not inherit unrelated release verification embeddings.";

#[derive(Clone, Copy)]
struct DedupEnv<'a> {
    enabled: Option<&'a str>,
    hamming_k: Option<&'a str>,
    cosine_floor: Option<&'a str>,
    agent_name: &'a str,
}

struct TimedOutput {
    output: Output,
    elapsed: Duration,
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn run_ee(workspace: &Path, args: &[&str], env: DedupEnv<'_>) -> Result<TimedOutput, String> {
    let started = Instant::now();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command
        .current_dir(workspace)
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .env_remove("EE_EMBED_DEDUP_ENABLED")
        .env_remove("EE_EMBED_DEDUP_HAMMING_K")
        .env_remove("EE_EMBED_DEDUP_COSINE_FLOOR")
        .env("EE_AGENT_NAME", env.agent_name);
    if let Some(value) = env.enabled {
        command.env("EE_EMBED_DEDUP_ENABLED", value);
    }
    if let Some(value) = env.hamming_k {
        command.env("EE_EMBED_DEDUP_HAMMING_K", value);
    }
    if let Some(value) = env.cosine_floor {
        command.env("EE_EMBED_DEDUP_COSINE_FLOOR", value);
    }

    let output = command
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))?;
    Ok(TimedOutput {
        output,
        elapsed: started.elapsed(),
    })
}

fn assert_success(timed: &TimedOutput, context: &str) -> TestResult {
    ensure_equal(
        &timed.output.status.code(),
        &Some(EXIT_SUCCESS),
        &format!("{context} exit code"),
    )?;
    ensure(
        String::from_utf8_lossy(&timed.output.stderr)
            .trim()
            .is_empty(),
        format!(
            "{context}: stderr should be empty in JSON mode, got {}",
            String::from_utf8_lossy(&timed.output.stderr)
        ),
    )
}

fn stdout_json(output: &Output, context: &str) -> Result<Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{context}: stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{context}: stdout was not JSON: {error}\nstdout: {stdout}"))
}

fn memory_id(json: &Value, context: &str) -> Result<String, String> {
    json.pointer("/data/memory_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{context}: missing /data/memory_id"))
}

fn workspace_id(json: &Value, context: &str) -> Result<String, String> {
    json.pointer("/data/workspace_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{context}: missing /data/workspace_id"))
}

fn degraded_codes(json: &Value) -> Vec<String> {
    json.pointer("/data/degraded")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("code").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn rounded_json_f64(value: Option<f64>) -> Option<f64> {
    value.map(|number| (number * 10_000.0).round() / 10_000.0)
}

fn normalize_remember(json: &Value) -> Value {
    json!({
        "schema": json.get("schema").and_then(Value::as_str),
        "success": json.get("success").and_then(Value::as_bool),
        "command": json.pointer("/data/command").and_then(Value::as_str),
        "memoryId": "<MEMORY_ID>",
        "persisted": json.pointer("/data/persisted").and_then(Value::as_bool),
        "indexStatus": json.pointer("/data/index_status").and_then(Value::as_str),
        "nearDuplicates": normalize_near_duplicates(json),
        "degradedCodes": degraded_codes(json),
    })
}

fn normalize_near_duplicates(json: &Value) -> Value {
    Value::Array(
        json.pointer("/data/near_duplicates")
            .and_then(Value::as_array)
            .map(|duplicates| {
                duplicates
                    .iter()
                    .map(|duplicate| {
                        json!({
                            "memoryId": "<EXISTING_MEMORY_ID>",
                            "similarity": rounded_json_f64(
                                duplicate.get("similarity").and_then(Value::as_f64),
                            ),
                            "threshold": rounded_json_f64(
                                duplicate.get("threshold").and_then(Value::as_f64),
                            ),
                            "hammingDistance": duplicate
                                .get("hammingDistance")
                                .or_else(|| duplicate.get("hamming_distance"))
                                .and_then(Value::as_u64),
                            "source": duplicate.get("source").and_then(Value::as_str),
                            "nextActions": normalize_near_duplicate_next_actions(duplicate),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    )
}

fn normalize_near_duplicate_next_actions(duplicate: &Value) -> Value {
    Value::Array(
        duplicate
            .get("nextActions")
            .or_else(|| duplicate.get("next_actions"))
            .and_then(Value::as_array)
            .map(|actions| {
                actions
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|action| {
                        if action.starts_with("ee memory link <new-memory-id> ") {
                            json!("ee memory link <new-memory-id> <EXISTING_MEMORY_ID>")
                        } else {
                            json!(action)
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    )
}

fn artifact_dir() -> Result<PathBuf, String> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("embed_dedup_e2e");
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create {}: {error}", dir.display()))?;
    Ok(dir)
}

fn unique_event_log_path() -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    Ok(artifact_dir()?.join(format!(
        "embed_dedup_events_{}_{}.jsonl",
        std::process::id(),
        now
    )))
}

fn stable_hash(value: &Value) -> Result<String, String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    Ok(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
}

fn append_test_event(
    log_path: &Path,
    scenario: &str,
    command: &str,
    config: Value,
    decision_counts: Value,
    elapsed: Duration,
    degraded: Vec<String>,
) -> Result<(), String> {
    let hash_payload = json!({
        "scenario": scenario,
        "command": command,
        "config": config,
        "decisionCounts": decision_counts,
        "degraded": degraded,
    });
    let event = json!({
        "schema": "ee.test_event.v1",
        "surface": "embed_dedup",
        "beadId": "bd-1iltv.5",
        "phase": "assert",
        "scenario": scenario,
        "command": command,
        "config": hash_payload["config"],
        "decisionCounts": hash_payload["decisionCounts"],
        "elapsed_ms": elapsed.as_secs_f64() * 1000.0,
        "hash": stable_hash(&hash_payload)?,
        "degraded": hash_payload["degraded"],
    });
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|error| format!("failed to open {}: {error}", log_path.display()))?;
    serde_json::to_writer(&mut file, &event)
        .map_err(|error| format!("failed to write event JSON: {error}"))?;
    file.write_all(b"\n")
        .map_err(|error| format!("failed to write event newline: {error}"))
}

fn remember_args(content: &str) -> [&str; 9] {
    [
        "remember",
        content,
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--no-auto-link",
        "--no-propose-candidates",
        "--json",
    ]
}

fn assert_event_log_shape(path: &Path) -> TestResult {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("failed to read event log {}: {error}", path.display()))?;
    let mut count = 0_usize;
    for (index, line) in text.lines().enumerate() {
        let event: Value = serde_json::from_str(line)
            .map_err(|error| format!("event line {} was not JSON: {error}", index + 1))?;
        ensure_equal(
            &event.get("schema").and_then(Value::as_str),
            &Some("ee.test_event.v1"),
            "event schema",
        )?;
        ensure(
            event.get("command").and_then(Value::as_str).is_some(),
            "event command missing",
        )?;
        ensure(event.get("config").is_some(), "event config missing")?;
        ensure(
            event.get("decisionCounts").is_some(),
            "event decisionCounts missing",
        )?;
        ensure(
            event
                .get("elapsed_ms")
                .and_then(Value::as_f64)
                .is_some_and(|elapsed| elapsed >= 0.0),
            "event elapsed_ms missing",
        )?;
        ensure(
            event
                .get("hash")
                .and_then(Value::as_str)
                .is_some_and(|hash| hash.starts_with("blake3:")),
            "event hash missing",
        )?;
        ensure(event.get("degraded").is_some(), "event degraded missing")?;
        count += 1;
    }
    ensure_equal(&count, &5_usize, "event count")
}

#[test]
fn remember_insert_dedup_real_binary_pins_durable_link_and_perf_events() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let disabled_workspace = tempdir.path().join("disabled");
    let enabled_workspace = tempdir.path().join("enabled");
    fs::create_dir_all(&disabled_workspace).map_err(|error| error.to_string())?;
    fs::create_dir_all(&enabled_workspace).map_err(|error| error.to_string())?;
    let events_path = unique_event_log_path()?;

    let default_env = DedupEnv {
        enabled: None,
        hamming_k: None,
        cosine_floor: None,
        agent_name: "EmbedDedupAgentA",
    };
    let enabled_env = DedupEnv {
        enabled: Some("true"),
        hamming_k: Some("12"),
        cosine_floor: Some("0.97"),
        agent_name: "EmbedDedupAgentB",
    };
    let strict_env = DedupEnv {
        enabled: Some("true"),
        hamming_k: Some("128"),
        cosine_floor: Some("1.0"),
        agent_name: "EmbedDedupAgentC",
    };

    let disabled_init = run_ee(&disabled_workspace, &["init", "--json"], default_env)?;
    assert_success(&disabled_init, "disabled init")?;
    append_test_event(
        &events_path,
        "disabled_default_init",
        "ee init --json",
        json!({"enabled": false}),
        json!({"fresh": 0, "reuse": 0}),
        disabled_init.elapsed,
        Vec::new(),
    )?;

    let disabled_source = run_ee(
        &disabled_workspace,
        &remember_args(EXACT_CONTENT),
        default_env,
    )?;
    assert_success(&disabled_source, "disabled source remember")?;
    let disabled_source_json = stdout_json(&disabled_source.output, "disabled source remember")?;
    append_test_event(
        &events_path,
        "disabled_default_source",
        "ee remember --json",
        json!({"enabled": false}),
        json!({"fresh": 1, "reuse": 0}),
        disabled_source.elapsed,
        degraded_codes(&disabled_source_json),
    )?;

    let disabled_duplicate = run_ee(
        &disabled_workspace,
        &remember_args(EXACT_CONTENT),
        default_env,
    )?;
    assert_success(&disabled_duplicate, "disabled duplicate remember")?;
    let disabled_duplicate_json =
        stdout_json(&disabled_duplicate.output, "disabled duplicate remember")?;
    append_test_event(
        &events_path,
        "disabled_default_duplicate",
        "ee remember --json",
        json!({"enabled": false}),
        json!({"fresh": 1, "reuse": 0}),
        disabled_duplicate.elapsed,
        degraded_codes(&disabled_duplicate_json),
    )?;

    let disabled_workspace_id = workspace_id(&disabled_source_json, "disabled source")?;
    let disabled_connection = DbConnection::open_file(disabled_workspace.join(".ee").join("ee.db"))
        .map_err(|error| error.to_string())?;
    let disabled_links = disabled_connection
        .list_all_memory_links(None)
        .map_err(|error| error.to_string())?;
    let disabled_candidates = disabled_connection
        .list_memory_simhash_candidates(
            &disabled_workspace_id,
            simhash_128(EXACT_CONTENT).to_be_bytes(),
            0,
            10,
        )
        .map_err(|error| error.to_string())?;
    disabled_connection
        .close()
        .map_err(|error| error.to_string())?;
    ensure(
        disabled_links.is_empty(),
        "disabled mode must not create links",
    )?;
    ensure(
        disabled_candidates.is_empty(),
        "disabled mode must not persist content_simhash candidates",
    )?;

    let enabled_init = run_ee(&enabled_workspace, &["init", "--json"], enabled_env)?;
    assert_success(&enabled_init, "enabled init")?;

    let enabled_source = run_ee(
        &enabled_workspace,
        &remember_args(EXACT_CONTENT),
        enabled_env,
    )?;
    assert_success(&enabled_source, "enabled source remember")?;
    let enabled_source_json = stdout_json(&enabled_source.output, "enabled source remember")?;
    let source_id = memory_id(&enabled_source_json, "enabled source")?;
    let enabled_workspace_id = workspace_id(&enabled_source_json, "enabled source")?;

    let enabled_duplicate = run_ee(
        &enabled_workspace,
        &remember_args(DUPLICATE_CONTENT),
        enabled_env,
    )?;
    assert_success(&enabled_duplicate, "enabled duplicate remember")?;
    let enabled_duplicate_json =
        stdout_json(&enabled_duplicate.output, "enabled duplicate remember")?;
    let duplicate_id = memory_id(&enabled_duplicate_json, "enabled duplicate")?;
    let near_duplicates = enabled_duplicate_json
        .pointer("/data/near_duplicates")
        .and_then(Value::as_array)
        .ok_or_else(|| "enabled duplicate remember missing near_duplicates array".to_owned())?;
    ensure_equal(
        &near_duplicates.len(),
        &1_usize,
        "enabled duplicate near duplicate count",
    )?;
    ensure_equal(
        &near_duplicates[0].get("memory_id").and_then(Value::as_str),
        &Some(source_id.as_str()),
        "enabled duplicate near duplicate existing id",
    )?;
    ensure_equal(
        &near_duplicates[0].get("source").and_then(Value::as_str),
        &Some("embedding_reuse"),
        "enabled duplicate near duplicate source",
    )?;
    ensure_equal(
        &near_duplicates[0]
            .get("hammingDistance")
            .and_then(Value::as_u64),
        &Some(0),
        "enabled duplicate near duplicate hamming distance",
    )?;

    let false_positive = run_ee(
        &enabled_workspace,
        &remember_args(FALSE_POSITIVE_CONTENT),
        strict_env,
    )?;
    assert_success(&false_positive, "strict false-positive remember")?;
    let false_positive_json = stdout_json(&false_positive.output, "strict false-positive")?;
    let false_positive_id = memory_id(&false_positive_json, "strict false-positive")?;

    append_test_event(
        &events_path,
        "enabled_exact_duplicate",
        "ee remember --json",
        json!({"enabled": true, "hammingK": 12, "cosineFloor": 0.97}),
        json!({"fresh": 1, "reuse": 1, "persistentEmbeddingReuses": 1}),
        enabled_source.elapsed + enabled_duplicate.elapsed,
        [
            degraded_codes(&enabled_source_json),
            degraded_codes(&enabled_duplicate_json),
        ]
        .concat(),
    )?;
    append_test_event(
        &events_path,
        "strict_false_positive_rejected",
        "ee remember --json",
        json!({"enabled": true, "hammingK": 128, "cosineFloor": 1.0}),
        json!({"fresh": 1, "reuse": 0}),
        false_positive.elapsed,
        degraded_codes(&false_positive_json),
    )?;

    let connection = DbConnection::open_file(enabled_workspace.join(".ee").join("ee.db"))
        .map_err(|error| error.to_string())?;
    let source_candidates = connection
        .list_memory_simhash_candidates(
            &enabled_workspace_id,
            simhash_128(EXACT_CONTENT).to_be_bytes(),
            0,
            10,
        )
        .map_err(|error| error.to_string())?;
    let duplicate_links = connection
        .list_memory_links_for_memory(&duplicate_id, Some(MemoryLinkRelation::Related))
        .map_err(|error| error.to_string())?;
    let false_positive_links = connection
        .list_memory_links_for_memory(&false_positive_id, Some(MemoryLinkRelation::Related))
        .map_err(|error| error.to_string())?;
    connection.close().map_err(|error| error.to_string())?;

    ensure_equal(
        &source_candidates.len(),
        &2_usize,
        "exact SimHash candidate count",
    )?;
    ensure_equal(
        &duplicate_links.len(),
        &1_usize,
        "duplicate dedup link count",
    )?;
    ensure(
        false_positive_links.is_empty(),
        "strict false positive must not create a dedup link",
    )?;

    let link = &duplicate_links[0];
    ensure_equal(&link.src_memory_id, &duplicate_id, "dedup link source id")?;
    ensure_equal(&link.dst_memory_id, &source_id, "dedup link target id")?;
    ensure_equal(&link.relation.as_str(), &"related", "dedup link relation")?;
    ensure_equal(&link.source.as_str(), &"auto", "dedup link source")?;
    ensure(link.directed, "dedup link must be directed")?;
    ensure_equal(&link.evidence_count, &2_u32, "dedup link evidence count")?;
    let metadata: Value = serde_json::from_str(
        link.metadata_json
            .as_deref()
            .ok_or_else(|| "dedup link metadata_json missing".to_owned())?,
    )
    .map_err(|error| error.to_string())?;
    ensure_equal(
        &metadata.get("schema").and_then(Value::as_str),
        &Some("ee.embed_dedup.link.v1"),
        "dedup metadata schema",
    )?;
    ensure_equal(
        &metadata.get("targetMemoryId").and_then(Value::as_str),
        &Some(source_id.as_str()),
        "dedup metadata target",
    )?;

    let normalized = json!({
        "schema": "ee.embed_dedup.no_mock_e2e_golden.v1",
        "beadId": "bd-1iltv.5",
        "disabledDefault": {
            "config": {"enabled": false},
            "rememberResults": [
                normalize_remember(&disabled_source_json),
                normalize_remember(&disabled_duplicate_json),
            ],
            "contentSimhashCandidates": 0,
            "memoryLinks": [],
        },
        "enabledExactDuplicate": {
            "config": {"enabled": true, "hammingK": 12, "cosineFloor": 0.97},
            "rememberResults": [
                normalize_remember(&enabled_source_json),
                normalize_remember(&enabled_duplicate_json),
            ],
            "contentSimhashCandidatesAtHammingZero": 2,
            "dedupLinks": [
                {
                    "srcMemoryId": "<DUPLICATE_MEMORY_ID>",
                    "dstMemoryId": "<SOURCE_MEMORY_ID>",
                    "relation": link.relation,
                    "source": link.source,
                    "directed": link.directed,
                    "evidenceCount": link.evidence_count,
                    "metadata": {
                        "schema": metadata.get("schema").and_then(Value::as_str),
                        "relationship": metadata.get("relationship").and_then(Value::as_str),
                        "targetMemoryId": "<SOURCE_MEMORY_ID>",
                        "hammingDistance": metadata.get("hammingDistance").and_then(Value::as_u64),
                        "cosineSimilarity": rounded_json_f64(metadata.get("cosineSimilarity").and_then(Value::as_f64)),
                        "cosineFloor": rounded_json_f64(metadata.get("cosineFloor").and_then(Value::as_f64)),
                        "decision": metadata.get("decision").and_then(Value::as_str),
                        "reason": metadata.get("reason").and_then(Value::as_str),
                    },
                }
            ],
        },
        "strictFalsePositive": {
            "config": {"enabled": true, "hammingK": 128, "cosineFloor": 1.0},
            "rememberResult": normalize_remember(&false_positive_json),
            "dedupLinks": [],
        },
        "performanceEvidence": {
            "eventSchema": "ee.test_event.v1",
            "eventCount": 5,
            "commandsMeasured": 5,
            "elapsedMs": "<MEASURED>",
            "hashAlgorithm": "blake3",
            "persistentEmbeddingReuses": 1,
            "writeLatencyDeltaMs": "<MEASURED>",
        },
    });
    let expected: Value = serde_json::from_str(GOLDEN)
        .map_err(|error| format!("embed dedup e2e golden must parse: {error}"))?;
    ensure_equal(&normalized, &expected, "normalized embed dedup e2e golden")?;

    let perf_fixture: Value = serde_json::from_str(PERF_FIXTURE)
        .map_err(|error| format!("embed dedup perf fixture must parse: {error}"))?;
    ensure_equal(
        &perf_fixture.get("schema").and_then(Value::as_str),
        &Some("ee.perf.artifact_summary.v1"),
        "perf fixture schema",
    )?;
    ensure_equal(
        &perf_fixture
            .pointer("/metrics/persistent_embedding_reuses/value")
            .and_then(Value::as_f64),
        &Some(1.0),
        "perf fixture persistent reuse count",
    )?;

    assert_event_log_shape(&events_path)
}
