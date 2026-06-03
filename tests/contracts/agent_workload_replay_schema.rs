//! bd-1zb7k.19.2: contract coverage for deterministic replay of
//! redacted `ee.agent_workload_trace.v1` rows.
//!
//! The replay surface must stay side-effect-free and content-redacted:
//! it consumes only committed JSONL fixtures and emits a stable
//! `ee.agent_workload_replay.v1` aggregate without live Agent Mail,
//! Beads, RCH, network, raw queries, raw memory bodies, or trace
//! timestamps in its deterministic hash/report payload.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use ee::core::lab::{
    AGENT_WORKLOAD_REPLAY_SCHEMA_V1, AgentWorkloadReplayOptions, replay_agent_workload_trace,
};
use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_PATH: &str = "docs/schemas/ee.agent_workload_replay.v1.json";
const SCHEMA_ID: &str = "https://eidetic-engine/schemas/ee.agent_workload_replay.v1.json";
const FIXTURE_PATH: &str = "tests/fixtures/agent_workloads/redacted_trace_minimal.jsonl";
const DOC_PATH: &str = "docs/agent-ux/workload-replay.md";

const REQUIRED_TOP_LEVEL: &[&str] = &[
    "schema",
    "sideEffectFree",
    "command",
    "playback",
    "trace",
    "commandCounts",
    "schemasObserved",
    "degradedCodeDeltas",
    "byteTokenDeltas",
    "latency",
    "cachePosture",
    "duplicateWorkCoalescing",
    "replayHash",
    "determinism",
    "fixturePromotion",
    "warnings",
];

const CLOSED_OBJECT_DEFS: &[&str] = &[
    "playback",
    "traceSummary",
    "commandCount",
    "schemaCount",
    "degradedCodeDelta",
    "byteTokenDeltas",
    "latency",
    "cachePosture",
    "duplicateWorkCoalescing",
    "determinism",
    "fixturePromotion",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn read_json(path: &str) -> Result<Value, String> {
    let full_path = repo_root().join(path);
    let text = fs::read_to_string(&full_path)
        .map_err(|error| format!("read {}: {error}", full_path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", full_path.display()))
}

fn read_text(path: &str) -> Result<String, String> {
    let full_path = repo_root().join(path);
    fs::read_to_string(&full_path).map_err(|error| format!("read {}: {error}", full_path.display()))
}

fn collect_strings(value: &Value, ctx: &str) -> Result<Vec<String>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("{ctx}: expected array, got {value}"))?
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{ctx}: non-string entry {entry}"))
        })
        .collect()
}

fn collect_string_set(value: &Value, ctx: &str) -> Result<BTreeSet<String>, String> {
    Ok(collect_strings(value, ctx)?.into_iter().collect())
}

fn replay_fixture(agent_count: u16, verify_determinism: bool) -> Result<Value, String> {
    let report = replay_agent_workload_trace(&AgentWorkloadReplayOptions {
        trace_path: repo_root().join(FIXTURE_PATH),
        agent_count,
        verify_determinism,
    })
    .map_err(|error| error.message())?;
    serde_json::from_str(&report.to_json()).map_err(|error| format!("parse replay JSON: {error}"))
}

#[test]
fn agent_workload_replay_v1_schema_pins_side_effect_free_report_shape() -> TestResult {
    let schema = read_json(SCHEMA_PATH)?;
    ensure(
        schema["$id"] == SCHEMA_ID,
        format!("expected schema id {SCHEMA_ID}; got {}", schema["$id"]),
    )?;
    ensure(
        schema["properties"]["schema"]["const"] == AGENT_WORKLOAD_REPLAY_SCHEMA_V1,
        "schema const must match AGENT_WORKLOAD_REPLAY_SCHEMA_V1",
    )?;
    ensure(
        schema["properties"]["sideEffectFree"]["const"] == Value::Bool(true),
        "replay report schema must be side-effect-free by construction",
    )?;
    ensure(
        schema["properties"]["command"]["const"] == "lab replay workload",
        "command const must name the workload replay surface",
    )?;

    let actual = collect_string_set(&schema["required"], "top-level required")?;
    let expected = REQUIRED_TOP_LEVEL
        .iter()
        .map(|field| (*field).to_owned())
        .collect::<BTreeSet<_>>();
    ensure(
        actual == expected,
        format!(
            "REQUIRED_TOP_LEVEL drifted from schema required array\nexpected={expected:?}\nactual={actual:?}"
        ),
    )?;
    ensure(
        schema["$defs"]["blake3Hash"]["pattern"]
            .as_str()
            .is_some_and(|pattern| pattern.contains("blake3:")),
        "replay hashes must be explicit blake3-prefixed strings",
    )
}

#[test]
fn agent_workload_replay_v1_closes_all_nested_object_shapes() -> TestResult {
    let schema = read_json(SCHEMA_PATH)?;
    ensure(
        schema["additionalProperties"] == Value::Bool(false),
        "replay schema top-level object must be closed with additionalProperties=false",
    )?;

    for def_name in CLOSED_OBJECT_DEFS {
        let def = &schema["$defs"][def_name];
        ensure(
            def["type"] == "object",
            format!("$defs.{def_name}.type must be object; got: {}", def["type"]),
        )?;
        ensure(
            def["additionalProperties"] == Value::Bool(false),
            format!(
                "$defs.{def_name} must be closed with additionalProperties=false; got: {}",
                def["additionalProperties"]
            ),
        )?;

        let required = collect_strings(
            &def["required"],
            &format!("agent workload replay $defs.{def_name}.required"),
        )?;
        let properties = def["properties"]
            .as_object()
            .ok_or_else(|| format!("$defs.{def_name}.properties must be an object"))?;
        for field in &required {
            ensure(
                properties.contains_key(field),
                format!(
                    "$defs.{def_name}.required includes `{field}` but properties are {:?}",
                    properties.keys().collect::<Vec<_>>()
                ),
            )?;
        }
    }
    Ok(())
}

#[test]
fn agent_workload_replay_fixture_is_deterministic_and_scaled() -> TestResult {
    let replay = replay_fixture(64, true)?;
    ensure(
        replay["schema"] == AGENT_WORKLOAD_REPLAY_SCHEMA_V1,
        "replay JSON schema mismatch",
    )?;
    ensure(
        replay["sideEffectFree"] == Value::Bool(true),
        "sideEffectFree",
    )?;
    ensure(replay["playback"]["activeAgents"] == 64, "activeAgents")?;
    ensure(
        replay["playback"]["syntheticOperations"] == 256,
        "synthetic operation count",
    )?;
    ensure(replay["trace"]["rowCount"] == 4, "rowCount")?;
    ensure(
        replay["fixturePromotion"]["sanitizedFixtureHash"] == replay["trace"]["traceHash"],
        "fixturePromotion must carry the sanitized trace hash",
    )?;

    let commands = replay["commandCounts"]
        .as_array()
        .ok_or_else(|| "commandCounts must be an array".to_owned())?;
    for command in ["context", "search", "why", "status"] {
        ensure(
            commands
                .iter()
                .any(|entry| entry["command"] == command && entry["count"].as_u64() == Some(64)),
            format!("missing scaled command count for {command}: {commands:?}"),
        )?;
    }
    let degraded = replay["degradedCodeDeltas"]
        .as_array()
        .ok_or_else(|| "degradedCodeDeltas must be an array".to_owned())?;
    ensure(
        degraded
            .iter()
            .any(|entry| entry["code"] == "index_stale" && entry["observedCount"] == 128),
        format!("missing index_stale degraded delta: {degraded:?}"),
    )?;

    let hashes = replay["determinism"]["replayHashes"]
        .as_array()
        .ok_or_else(|| "determinism.replayHashes must be an array".to_owned())?;
    ensure(hashes.len() == 3, "determinism must run three hash passes")?;
    ensure(
        replay["determinism"]["allIdentical"] == Value::Bool(true),
        "determinism hashes must be byte-identical",
    )?;
    ensure(
        hashes.iter().all(|hash| hash == &hashes[0]),
        format!("determinism hashes differ: {hashes:?}"),
    )
}

#[test]
fn agent_workload_replay_json_omits_raw_trace_content_and_volatiles() -> TestResult {
    let replay = replay_fixture(64, true)?;
    let rendered = serde_json::to_string(&replay).map_err(|error| error.to_string())?;
    for forbidden in [
        "rawTaskStringPresent",
        "rawQueryTextPresent",
        "rawMemoryBodyPresent",
        "rawMailBodyPresent",
        "secretsPresent",
        "environmentDumpPresent",
        "fullFileListingPresent",
        "recordedAt",
        "retentionPosture",
    ] {
        ensure(
            !rendered.contains(forbidden),
            format!("replay report must not leak trace-only field `{forbidden}`"),
        )?;
    }
    Ok(())
}

#[test]
fn workload_replay_docs_pin_no_live_service_contract() -> TestResult {
    let doc = read_text(DOC_PATH)?;
    for expected in [
        "ee lab replay --trace <trace.jsonl> --agents 64 --json",
        "does not call Agent Mail, Beads, RCH",
        "workspace database, search indexes, or external services",
        "Raw task strings, query text, memory bodies",
        "fixturePromotion.perfBudgetKey",
    ] {
        ensure(
            doc.contains(expected),
            format!("{DOC_PATH} missing expected workload replay contract text: {expected}"),
        )?;
    }
    Ok(())
}
