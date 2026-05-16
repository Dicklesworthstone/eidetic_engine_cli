//! bd-1prrl.1.5: streaming context frame order, snapshot, and batch hash parity.
//!
//! The first test uses a deterministic in-memory `ContextResponse` fixture so
//! the stream envelope shape is golden-snapshot stable. The second test drives
//! the real `ee` binary to prove the CLI stream path preserves batch pack hash
//! and frame ordering for an actual workspace.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use ee::models::{MemoryId, ProvenanceUri, TrustClass, UnitScore};
use ee::output::streaming::{ContextStreamFrameOptions, context_response_stream_frames};
use ee::pack::{
    ContextRequest, ContextResponse, PackCandidate, PackCandidateInput, PackProvenance,
    PackSection, PackTrustSignal, TokenBudget, assemble_draft,
};
use insta::assert_json_snapshot;
use serde_json::{Map, Value, json};
use uuid::Uuid;

type TestResult = Result<(), String>;

const QUERY: &str = "stream release guardrail";
const REAL_STREAM_QUERIES: &[&str] = &[
    "stream release guardrail",
    "stream trailer hash",
    "partial stream terminal frame",
    "item frame rank sequence",
    "batch canonical selection source",
];

fn memory_id(seed: u128) -> MemoryId {
    MemoryId::from_uuid(Uuid::from_u128(seed))
}

fn unit(value: f32) -> UnitScore {
    UnitScore::parse(value).expect("unit score in range")
}

fn provenance(uri: &str) -> PackProvenance {
    PackProvenance::new(
        ProvenanceUri::from_str(uri).expect("provenance URI parses"),
        "stream fixture",
    )
    .expect("pack provenance constructs")
}

fn candidate(seed: u128, content: &str, relevance: f32, section: PackSection) -> PackCandidate {
    PackCandidate::new(PackCandidateInput {
        memory_id: memory_id(seed),
        section,
        content: content.to_owned(),
        estimated_tokens: 12,
        relevance: unit(relevance),
        utility: unit(0.7),
        provenance: vec![provenance("file://tests/context_stream.rs")],
        why: "stream fixture item selected for context emission".to_owned(),
    })
    .expect("candidate constructs")
    .with_trust_signal(PackTrustSignal::new(
        TrustClass::HumanExplicit,
        Some("stream-fixture".to_owned()),
    ))
}

fn fixture_response() -> ContextResponse {
    let request = ContextRequest::from_query(QUERY).expect("request query accepts");
    let budget = TokenBudget::new(600).expect("budget accepts 600");
    let mut draft = assemble_draft(
        &request.query,
        budget,
        vec![
            candidate(
                0x11,
                "Use stream frames when an agent can consume context incrementally.",
                0.91,
                PackSection::ProceduralRules,
            ),
            candidate(
                0x12,
                "Trailer hash must match the non-streaming context pack hash.",
                0.83,
                PackSection::Decisions,
            ),
            candidate(
                0x13,
                "Partial streams are not complete packs until a terminal frame arrives.",
                0.72,
                PackSection::Failures,
            ),
        ],
    )
    .expect("draft assembles");
    draft.hash = Some("blake3:context-stream-fixture-pack".to_owned());
    ContextResponse::new(request, draft, Vec::new()).expect("context response constructs")
}

fn stream_options() -> ContextStreamFrameOptions {
    ContextStreamFrameOptions::new(
        "pack_stream_fixture",
        "workspace_fixture",
        "request_fixture",
        "2026-05-16T00:00:00Z",
        "2026-05-16T00:00:01Z",
    )
}

fn canonical_json(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(canonical_json).collect()),
        Value::Object(object) => {
            let mut entries: Vec<_> = object.into_iter().collect();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            let mut canonical = Map::new();
            for (key, value) in entries {
                canonical.insert(key, canonical_json(value));
            }
            Value::Object(canonical)
        }
        scalar => scalar,
    }
}

fn snapshot_stream_frames(value: Value) {
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path("snapshots");
    settings.set_prepend_module_to_snapshot(false);
    settings.bind(|| {
        assert_json_snapshot!("context_stream", canonical_json(value));
    });
}

fn snapshot_contract_frame(frame: &Value) -> Value {
    match frame.get("kind").and_then(Value::as_str) {
        Some("header") => json!({
            "schema": frame["schema"],
            "kind": frame["kind"],
            "packId": frame["packId"],
            "query": frame["query"],
            "canonicalKeyHash": frame["canonicalKeyHash"],
        }),
        Some("item") => json!({
            "schema": frame["schema"],
            "kind": frame["kind"],
            "packId": frame["packId"],
            "seq": frame["seq"],
            "rank": frame["rank"],
            "memoryId": frame["memoryId"],
            "section": frame["section"],
            "content": frame["content"],
            "estimatedTokens": frame["estimatedTokens"],
            "scores": {
                "relevance": frame["scores"]["relevance"],
                "utility": frame["scores"]["utility"],
            },
            "why": frame["why"],
        }),
        Some("trailer") => json!({
            "schema": frame["schema"],
            "kind": frame["kind"],
            "packId": frame["packId"],
            "packHash": frame["packHash"],
            "totalItems": frame["totalItems"],
            "usedTokens": frame["usedTokens"],
            "skippedTotal": frame["skippedTotal"],
            "degraded": frame["degraded"],
        }),
        _ => frame.clone(),
    }
}

#[test]
fn stream_adapter_frames_match_golden_shape() -> TestResult {
    let frames = context_response_stream_frames(&fixture_response(), stream_options())
        .map_err(|error| error.to_string())?;
    let values = frames
        .iter()
        .map(|frame| serde_json::to_value(frame).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;

    snapshot_stream_frames(Value::Array(
        values.iter().map(snapshot_contract_frame).collect(),
    ));
    Ok(())
}

fn workspace_dir() -> Result<PathBuf, String> {
    let mut root = std::env::var("EE_E2E_TMPDIR")
        .or_else(|_| std::env::var("TMPDIR"))
        .unwrap_or_else(|_| "/tmp".to_owned());
    if root.starts_with("/Volumes/") {
        root = "/tmp".to_owned();
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock before UNIX epoch: {error}"))?
        .as_nanos();
    let path = PathBuf::from(format!(
        "{}/ee-context-stream-{}-{nanos}",
        root.trim_end_matches('/'),
        std::process::id()
    ));
    fs::create_dir_all(&path).map_err(|error| {
        format!(
            "failed to create retained workspace {}: {error}",
            path.display()
        )
    })?;
    Ok(path)
}

fn run_ee(workspace: &PathBuf, args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .arg("--workspace")
        .arg(workspace)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn ensure_success(output: &Output, label: &str) -> TestResult {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{label}: ee exited {:?}; stdout: {}; stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).trim_end(),
            String::from_utf8_lossy(&output.stderr).trim_end()
        ))
    }
}

fn stdout_json(output: &Output, label: &str) -> Result<Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{label}: stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{label}: stdout was not JSON: {error}\nstdout: {stdout}"))
}

fn stdout_stream(output: &Output, label: &str) -> Result<Vec<Value>, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{label}: stdout was not UTF-8: {error}"))?;
    stdout
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).map_err(|error| {
                format!("{label}: stream line was not JSON: {error}\nline: {line}")
            })
        })
        .collect()
}

fn seed_workspace(workspace: &PathBuf) -> Result<(), String> {
    ensure_success(&run_ee(workspace, &["init", "--json"])?, "init")?;
    for (kind, content) in [
        (
            "rule",
            "Use streaming context frames when agents need incremental release guardrails.",
        ),
        (
            "decision",
            "The stream trailer pack hash must equal the batch context pack hash.",
        ),
        (
            "failure",
            "A partial context stream without a terminal frame must not be treated as a complete pack.",
        ),
        (
            "rule",
            "Item frames must preserve rank and sequence ordering from the batch pack.",
        ),
        (
            "decision",
            "The batch context response remains the canonical selection source until direct streaming lands.",
        ),
    ] {
        let output = run_ee(
            workspace,
            &[
                "remember",
                "--level",
                "procedural",
                "--kind",
                kind,
                content,
                "--json",
            ],
        )?;
        ensure_success(&output, "remember")?;
    }
    Ok(())
}

#[test]
fn real_context_stream_preserves_batch_hash_and_order() -> TestResult {
    let workspace = workspace_dir()?;
    seed_workspace(&workspace)?;

    for query in REAL_STREAM_QUERIES {
        assert_real_stream_matches_batch(&workspace, query)?;
    }

    Ok(())
}

fn assert_real_stream_matches_batch(workspace: &PathBuf, query: &str) -> TestResult {
    let batch = run_ee(
        workspace,
        &["--format", "json", "context", query, "--max-tokens", "900"],
    )?;
    ensure_success(&batch, &format!("batch context {query}"))?;
    let batch_json = stdout_json(&batch, &format!("batch context {query}"))?;
    let batch_hash = batch_json
        .pointer("/data/pack/hash")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("batch context {query} missing /data/pack/hash"))?;
    let batch_items = batch_json
        .pointer("/data/pack/items")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("batch context {query} missing /data/pack/items"))?;
    if batch_items.is_empty() {
        return Err(format!(
            "batch context returned no pack items for stream fixture query {query}"
        ));
    }

    for stream_format in ["json", "jsonl"] {
        let stream = run_ee(
            workspace,
            &[
                "--format",
                stream_format,
                "context",
                query,
                "--max-tokens",
                "900",
                "--stream",
            ],
        )?;
        let label = format!("stream context {stream_format} {query}");
        ensure_success(&stream, &label)?;
        let frames = stdout_stream(&stream, &label)?;
        if frames.len() != batch_items.len() + 2 {
            return Err(format!(
                "stream {stream_format} for {query} should emit header + items + trailer; got {} frames for {} batch items",
                frames.len(),
                batch_items.len()
            ));
        }

        if frames
            .first()
            .and_then(|frame| frame.get("kind"))
            .and_then(Value::as_str)
            != Some("header")
        {
            return Err(format!(
                "first {stream_format} stream frame for {query} must be header"
            ));
        }
        let trailer = frames
            .last()
            .ok_or_else(|| format!("{stream_format} stream for {query} emitted no frames"))?;
        if trailer.get("kind").and_then(Value::as_str) != Some("trailer") {
            return Err(format!(
                "last {stream_format} stream frame for {query} must be trailer"
            ));
        }
        if trailer.get("packHash").and_then(Value::as_str) != Some(batch_hash) {
            return Err(format!(
                "stream {stream_format} trailer hash for {query} must match batch hash: {:?} != {batch_hash}",
                trailer.get("packHash")
            ));
        }
        if trailer.get("totalItems").and_then(Value::as_u64) != Some(batch_items.len() as u64) {
            return Err(format!(
                "stream {stream_format} trailer totalItems for {query} must match batch item count"
            ));
        }

        for (index, frame) in frames[1..frames.len() - 1].iter().enumerate() {
            let expected_rank = (index + 1) as u64;
            let expected_seq = index as u64;
            if frame.get("kind").and_then(Value::as_str) != Some("item") {
                return Err(format!(
                    "frame {index} between {stream_format} header/trailer for {query} must be an item"
                ));
            }
            if frame.get("seq").and_then(Value::as_u64) != Some(expected_seq) {
                return Err(format!(
                    "{stream_format} item frame {index} for {query} has non-monotone seq"
                ));
            }
            if frame.get("rank").and_then(Value::as_u64) != Some(expected_rank) {
                return Err(format!(
                    "{stream_format} item frame {index} for {query} has non-monotone rank"
                ));
            }
            let stream_id = frame
                .get("memoryId")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    format!("{stream_format} item frame {index} for {query} missing memoryId")
                })?;
            let batch_id = batch_items[index]
                .get("memoryId")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("batch item {index} for {query} missing memoryId"))?;
            if stream_id != batch_id {
                return Err(format!(
                    "stream {stream_format} item {index} for {query} memoryId drifted from batch order: {stream_id} != {batch_id}"
                ));
            }
        }
    }

    Ok(())
}

#[test]
fn partial_stream_without_terminal_is_rejected_by_validator() -> TestResult {
    let mut frames = context_response_stream_frames(&fixture_response(), stream_options())
        .map_err(|error| error.to_string())?;
    frames.pop();
    let mut validator = ee::output::streaming::StreamSequenceValidator::new();
    for frame in &frames {
        validator
            .observe(frame)
            .map_err(|error| format!("partial prefix should be valid until finish: {error}"))?;
    }
    let error = validator
        .finish()
        .expect_err("partial stream without trailer must not be complete");
    if !matches!(
        error,
        ee::output::streaming::StreamValidationError::MissingTerminal
    ) {
        return Err(format!(
            "partial stream failed with unexpected error: {error}"
        ));
    }
    Ok(())
}
