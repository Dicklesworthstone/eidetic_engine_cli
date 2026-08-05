//! J7 — In-process determinism harness for tie-breaking, pack-hash,
//! and DB inspection response reproduction (bd-17c65.10.7).
//!
//! Companion to `scripts/e2e_overhaul/determinism.sh`. The bash script
//! exercises six surfaces end-to-end across three child invocations;
//! this Rust test focuses narrowly on the determinism invariants that
//! are easiest to regress and most painful to debug after the fact:
//!
//! * **Tie-break by memory_id ascending.** Two memories whose content
//!   produces byte-equal scores must rank by `memory_id` ascending
//!   (lower ULID first), and that order must be byte-stable across
//!   repeated invocations of the same query against the same
//!   workspace.
//! * **Pack-hash reproducibility.** Two `ee context` invocations
//!   against the same workspace + query + budget + profile must
//!   produce identical `data.pack.hash` values.
//! * **DB JSON reproducibility.** Repeated `ee db status` and
//!   `ee db check-integrity` invocations against the same initialized
//!   workspace must emit byte-identical JSON.
//!
//! The test spawns `ee` as a child process so state leaks (per-process
//! caches, in-memory RNGs, wall-clock fields embedded in responses)
//! surface here even though they would not surface in a single-process
//! library-level test. This mirrors the production usage pattern:
//! agents invoke `ee` one shot at a time, never as a daemon.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use frankensearch_core::traits::{RerankDocument, RerankScore, SyncRerank};
use frankensearch_rerank::NativeReranker;
use serde_json::Value;

type TestResult = Result<(), String>;

fn ee_binary() -> &'static str {
    env!("CARGO_BIN_EXE_ee")
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(ee_binary())
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn parse_json(output: &Output, context: &str) -> Result<Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{context}: stdout not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{context}: stdout not JSON: {error}\nstdout: {stdout}"))
}

fn init_workspace(workspace: &Path) -> Result<(), String> {
    let output = run_ee(&["--workspace", workspace.to_str().unwrap(), "init", "--json"])?;
    if !output.status.success() {
        return Err(format!(
            "ee init failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    Ok(())
}

fn remember(workspace: &Path, content: &str) -> Result<String, String> {
    let output = run_ee(&[
        "--workspace",
        workspace.to_str().unwrap(),
        "remember",
        content,
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--json",
    ])?;
    let value = parse_json(&output, "remember")?;
    // Multiple shape variants in flight across the swarm; try each.
    value
        .pointer("/data/memory_id")
        .or_else(|| value.pointer("/data/memoryId"))
        .or_else(|| value.pointer("/data/memory/id"))
        .or_else(|| value.pointer("/data/id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("remember response did not surface a memory id: {value}",))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn tmp_workspace(label: &str) -> Result<PathBuf, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let base = std::env::temp_dir().join(format!(
        "ee-determinism-{}-{}-{}",
        label,
        std::process::id(),
        nonce
    ));
    std::fs::create_dir_all(&base).map_err(|error| format!("create workspace: {error}"))?;
    Ok(base)
}

fn search_result_ids(value: &Value) -> Vec<String> {
    value
        .pointer("/data/results")
        .and_then(Value::as_array)
        .map(|results| {
            results
                .iter()
                .filter_map(|hit| {
                    hit.get("docId")
                        .or_else(|| hit.get("doc_id"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn search_tie_break_stable_across_three_invocations() -> TestResult {
    let workspace = tmp_workspace("tie_break")?;
    init_workspace(&workspace)?;

    // Seed three memories that pre-fusion share the same content shape
    // and therefore land at the same fused RRF score for a search query
    // that matches all three lexically.
    let id_b = remember(&workspace, "Run cargo fmt before release v0.2.")?;
    let id_a = remember(&workspace, "Run cargo fmt before release v0.1.")?;
    let id_c = remember(&workspace, "Run cargo fmt before release v0.3.")?;

    let run_search = || -> Result<Vec<String>, String> {
        let output = run_ee(&[
            "--workspace",
            workspace.to_str().unwrap(),
            "search",
            "cargo fmt before release",
            "--limit",
            "10",
            "--relevance-floor",
            "0",
            "--json",
        ])?;
        let value = parse_json(&output, "search")?;
        Ok(search_result_ids(&value))
    };

    let run1 = run_search()?;
    let run2 = run_search()?;
    let run3 = run_search()?;

    ensure(
        !run1.is_empty(),
        "search must return at least one result".to_owned(),
    )?;
    ensure(run1 == run2, format!("run1 != run2: {run1:?} vs {run2:?}"))?;
    ensure(run2 == run3, format!("run2 != run3: {run2:?} vs {run3:?}"))?;

    // Tie-break direction check: when all three memory IDs appear and
    // share an equal score, lower ULID must rank first. Sort the
    // observed memory IDs in our run by occurrence position and the
    // canonical alphabetical sort and assert they match.
    let mut canonical = vec![id_a.clone(), id_b.clone(), id_c.clone()];
    canonical.sort();
    let observed: Vec<String> = run1
        .iter()
        .filter(|id| id == &&id_a || id == &&id_b || id == &&id_c)
        .cloned()
        .collect();
    if observed.len() == canonical.len() {
        ensure(
            observed == canonical,
            format!(
                "tie-break must rank by memory_id ascending; observed={observed:?} canonical={canonical:?}",
            ),
        )?;
    }
    Ok(())
}

#[test]
fn context_pack_hash_reproduces_across_three_invocations() -> TestResult {
    let workspace = tmp_workspace("pack_hash")?;
    init_workspace(&workspace)?;
    remember(&workspace, "Use cargo fmt before release.")?;
    remember(&workspace, "Database connection pooling guide.")?;
    remember(&workspace, "Migration 0042 added user_email column.")?;

    let run_context = || -> Result<Option<String>, String> {
        let output = run_ee(&[
            "--workspace",
            workspace.to_str().unwrap(),
            "pack",
            "prepare release",
            "--max-tokens",
            "1000",
            "--json",
        ])?;
        let value = parse_json(&output, "context")?;
        Ok(value
            .pointer("/data/pack/hash")
            .and_then(Value::as_str)
            .map(str::to_owned))
    };

    let h1 = run_context()?;
    let h2 = run_context()?;
    let h3 = run_context()?;

    if h1.is_none() || h2.is_none() || h3.is_none() {
        // Some build configurations leave the pack hash null on
        // degraded paths. The bash harness covers this case; here we
        // accept the absence and skip the equality check (the test
        // does not produce a misleading green).
        return Err(format!(
            "context pack hash absent in at least one run: {h1:?} {h2:?} {h3:?}; \
             determinism cannot be asserted",
        ));
    }
    ensure(
        h1 == h2,
        format!("pack hash run1 != run2: {h1:?} vs {h2:?}"),
    )?;
    ensure(
        h2 == h3,
        format!("pack hash run2 != run3: {h2:?} vs {h3:?}"),
    )?;
    Ok(())
}

// bd-1n0np.15.2 — determinism gate for the why-not surface (ee.why_not_selected.v1):
// the counterfactual exclusion report is seeded deterministically (seed 0), so its
// `data` subtree must reproduce byte-identically across repeated invocations.
#[test]
fn why_not_json_reproduces_across_three_invocations() -> TestResult {
    let workspace = tmp_workspace("why_not")?;
    init_workspace(&workspace)?;
    let target = remember(&workspace, "Use cargo fmt before release.")?;
    remember(&workspace, "Database connection pooling guide.")?;
    remember(&workspace, "Migration 0042 added user_email column.")?;

    let run_why_not = || -> Result<String, String> {
        let output = run_ee(&[
            "--workspace",
            workspace.to_str().unwrap(),
            "why-not",
            &target,
            "--task",
            "prepare release",
            "--json",
        ])?;
        let value = parse_json(&output, "why-not")?;
        // Compare the deterministic `data` subtree; the response envelope may
        // carry run-specific timestamps, so canonicalize just `data`.
        let data = value.pointer("/data").cloned().unwrap_or(Value::Null);
        serde_json::to_string(&data).map_err(|error| error.to_string())
    };

    let r1 = run_why_not()?;
    let r2 = run_why_not()?;
    let r3 = run_why_not()?;
    ensure(
        !r1.is_empty() && r1 != "null",
        format!("why-not data absent; determinism cannot be asserted: {r1}"),
    )?;
    ensure(r1 == r2, "why-not data run1 != run2")?;
    ensure(r2 == r3, "why-not data run2 != run3")?;
    Ok(())
}

fn run_ee_stdout(args: &[&str], context: &str) -> Result<String, String> {
    let output = run_ee(args)?;
    if !output.status.success() {
        return Err(format!(
            "{context} failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| format!("{context}: stdout not UTF-8: {error}"))
}

#[test]
fn db_status_json_reproduces_across_three_invocations() -> TestResult {
    let workspace = tmp_workspace("db_status")?;
    init_workspace(&workspace)?;
    let workspace_arg = workspace.to_str().unwrap();

    let run_status = || {
        run_ee_stdout(
            &["--workspace", workspace_arg, "db", "status", "--json"],
            "db status",
        )
    };

    let run1 = run_status()?;
    let run2 = run_status()?;
    let run3 = run_status()?;

    ensure(
        run1 == run2,
        format!("db status run1 != run2:\nrun1={run1}\nrun2={run2}"),
    )?;
    ensure(
        run2 == run3,
        format!("db status run2 != run3:\nrun2={run2}\nrun3={run3}"),
    )?;
    Ok(())
}

#[test]
fn db_check_integrity_json_reproduces_across_three_invocations() -> TestResult {
    let workspace = tmp_workspace("db_check_integrity")?;
    init_workspace(&workspace)?;
    let workspace_arg = workspace.to_str().unwrap();

    let run_check = || {
        run_ee_stdout(
            &[
                "--workspace",
                workspace_arg,
                "db",
                "check-integrity",
                "--json",
            ],
            "db check-integrity",
        )
    };

    let run1 = run_check()?;
    let run2 = run_check()?;
    let run3 = run_check()?;

    ensure(
        run1 == run2,
        format!("db check-integrity run1 != run2:\nrun1={run1}\nrun2={run2}"),
    )?;
    ensure(
        run2 == run3,
        format!("db check-integrity run2 != run3:\nrun2={run2}\nrun3={run3}"),
    )?;
    Ok(())
}

/// Extract the continuation cursor from a governed envelope's
/// `output_truncated_budget` degraded entry.
fn continuation_cursor_of(stdout: &str) -> Result<String, String> {
    let value: Value = serde_json::from_str(stdout)
        .map_err(|error| format!("governed stdout not JSON: {error}"))?;
    value
        .get("degraded")
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries.iter().find_map(|entry| {
                if entry.get("code").and_then(Value::as_str) == Some("output_truncated_budget") {
                    entry
                        .pointer("/details/continuationCursor")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                } else {
                    None
                }
            })
        })
        .ok_or_else(|| "governed page offered no continuation cursor".to_string())
}

/// J7 governor lane (bd-7lvbg.4): governed output — including the
/// MAC'd ee.cursor.v1 continuation token and the stamped
/// meta.tokensEstimated — must be byte-identical across separate
/// child-process invocations, and so must a cursor-resumed second
/// page. A per-process MAC key, RNG, or wall-clock leak in the
/// governor path surfaces here and nowhere else.
#[test]
fn governed_pages_reproduce_across_three_invocations() -> TestResult {
    let workspace = tmp_workspace("governor")?;
    init_workspace(&workspace)?;
    for index in 0..6 {
        remember(
            &workspace,
            &format!("Governor determinism corpus row {index:02}: byte-identity gate filler."),
        )?;
    }
    let workspace_arg = workspace.to_str().unwrap();

    let first_page = || {
        run_ee_stdout(
            &[
                "--workspace",
                workspace_arg,
                "memory",
                "list",
                "--limit",
                "6",
                "--max-output-tokens",
                "700",
                "--json",
            ],
            "governed memory list",
        )
    };
    let run1 = first_page()?;
    let run2 = first_page()?;
    let run3 = first_page()?;
    ensure(
        run1 == run2 && run2 == run3,
        format!(
            "governed first page diverged across processes:\nrun1={run1}\nrun2={run2}\nrun3={run3}"
        ),
    )?;

    let cursor = continuation_cursor_of(&run1)?;
    let resumed_page = || {
        run_ee_stdout(
            &[
                "--workspace",
                workspace_arg,
                "memory",
                "list",
                "--limit",
                "6",
                "--max-output-tokens",
                "700",
                "--cursor",
                &cursor,
                "--json",
            ],
            "resumed memory list",
        )
    };
    let resume1 = resumed_page()?;
    let resume2 = resumed_page()?;
    let resume3 = resumed_page()?;
    ensure(
        resume1 == resume2 && resume2 == resume3,
        format!(
            "cursor-resumed page diverged across processes:\nrun1={resume1}\nrun2={resume2}\nrun3={resume3}"
        ),
    )?;
    ensure(
        resume1
            .find("cursor_invalid")
            .or_else(|| resume1.find("cursor_stale"))
            .is_none(),
        format!("resumed page rejected its own cursor: {resume1}"),
    )?;
    Ok(())
}

const RERANK_MODEL_DIR_ENV: &str = "EE_E2E_RERANK_MODEL_DIR";
const RERANK_REQUIRE_MODEL_ENV: &str = "EE_E2E_RERANK_REQUIRE_MODEL";
const RERANK_QUERY: &str = "bd1nl13 release format checklist cargo clippy";

fn native_reranker_model_dir() -> Result<Option<PathBuf>, String> {
    let Some(model_dir) = std::env::var_os(RERANK_MODEL_DIR_ENV).map(PathBuf::from) else {
        if std::env::var(RERANK_REQUIRE_MODEL_ENV).as_deref() == Ok("1") {
            return Err(format!(
                "{RERANK_REQUIRE_MODEL_ENV}=1 requires {RERANK_MODEL_DIR_ENV} to name an unpacked reranker model directory"
            ));
        }
        eprintln!(
            "[determinism_unit] SKIP native reranker lane: set {RERANK_MODEL_DIR_ENV} to an unpacked model directory; set {RERANK_REQUIRE_MODEL_ENV}=1 to make absence fail closed"
        );
        return Ok(None);
    };

    let tokenizer = model_dir.join("tokenizer.json");
    let primary_weights = model_dir.join("model_f32.safetensors");
    let fallback_weights = model_dir.join("model.safetensors");
    if tokenizer.is_file() && (primary_weights.is_file() || fallback_weights.is_file()) {
        return Ok(Some(model_dir));
    }

    Err(format!(
        "{RERANK_MODEL_DIR_ENV}={} is not loadable: tokenizer.json and model_f32.safetensors (or model.safetensors) are required",
        model_dir.display()
    ))
}

fn rerank_determinism_documents() -> Vec<RerankDocument> {
    [
        (
            "trap",
            "BD1NL13_RERANK_TRAP release release release format format checklist checklist cargo cargo clippy clippy, but this is a noisy lexical trap and not the Rust release policy target.",
        ),
        (
            "target",
            "BD1NL13_RERANK_TARGET The correct Rust release policy says run cargo fmt --check and cargo clippy before publishing.",
        ),
        (
            "noise_one",
            "BD1NL13_RERANK_NOISE_ONE Database migration notes cover index ownership and schema upgrade ordering.",
        ),
        (
            "noise_two",
            "BD1NL13_RERANK_NOISE_TWO Onboarding screenshots and terminal color themes need a design review.",
        ),
        (
            "noise_three",
            "BD1NL13_RERANK_NOISE_THREE Rust ownership and borrowing prevent memory safety errors at compile time.",
        ),
    ]
    .into_iter()
    .map(|(doc_id, text)| RerankDocument {
        doc_id: doc_id.to_owned(),
        text: text.to_owned(),
    })
    .collect()
}

fn descending_rerank_order(scores: &[RerankScore]) -> Vec<String> {
    let mut indices: Vec<usize> = (0..scores.len()).collect();
    indices.sort_by(|left, right| {
        scores[*right]
            .score
            .total_cmp(&scores[*left].score)
            .then_with(|| scores[*left].doc_id.cmp(&scores[*right].doc_id))
    });
    indices
        .into_iter()
        .map(|index| scores[index].doc_id.clone())
        .collect()
}

/// bd-1nl13.13 — exercise the real frankentorch cross-encoder, not a score stub.
///
/// The ordinary model-free lane reports an explicit skip because the 83 MiB
/// artifact is not a source fixture. Cross-platform release lanes set
/// `EE_E2E_RERANK_MODEL_DIR`; setting `EE_E2E_RERANK_REQUIRE_MODEL=1` makes a
/// missing artifact a hard failure. The companion shell harness emits and can
/// compare a content-addressed numerical vector across target platforms.
#[test]
fn native_reranker_scores_and_order_reproduce_across_three_runs() -> TestResult {
    let Some(model_dir) = native_reranker_model_dir()? else {
        return Ok(());
    };
    let reranker = NativeReranker::load(&model_dir)
        .map_err(|error| format!("load NativeReranker from {}: {error}", model_dir.display()))?;
    let documents = rerank_determinism_documents();

    let run = || {
        reranker
            .rerank_sync(RERANK_QUERY, &documents)
            .map_err(|error| format!("native rerank failed: {error}"))
    };
    let run1 = run()?;
    let run2 = run()?;
    let run3 = run()?;

    for (run_index, scores) in [&run1, &run2, &run3].into_iter().enumerate() {
        ensure(
            scores.len() == documents.len(),
            format!(
                "rerank run {} returned {} scores for {} documents",
                run_index + 1,
                scores.len(),
                documents.len()
            ),
        )?;
        for (original_rank, score) in scores.iter().enumerate() {
            ensure(
                score.original_rank == original_rank,
                format!(
                    "rerank run {} changed original_rank for {}: {} != {}",
                    run_index + 1,
                    score.doc_id,
                    score.original_rank,
                    original_rank
                ),
            )?;
            ensure(
                score.score.is_finite() && (0.0..=1.0).contains(&score.score),
                format!(
                    "rerank run {} produced invalid calibrated score for {}: {}",
                    run_index + 1,
                    score.doc_id,
                    score.score
                ),
            )?;
            ensure(
                score.raw_logit.is_some_and(f32::is_finite),
                format!(
                    "rerank run {} produced no finite raw logit for {}",
                    run_index + 1,
                    score.doc_id
                ),
            )?;
        }
    }

    for (index, ((first, second), third)) in run1.iter().zip(&run2).zip(&run3).enumerate() {
        ensure(
            first.doc_id == second.doc_id && second.doc_id == third.doc_id,
            format!("rerank document identity drifted at input rank {index}"),
        )?;
        ensure(
            first.score == second.score && second.score == third.score,
            format!(
                "rerank calibrated score drifted for {}: {} / {} / {}",
                first.doc_id, first.score, second.score, third.score
            ),
        )?;
        ensure(
            first.raw_logit == second.raw_logit && second.raw_logit == third.raw_logit,
            format!(
                "rerank raw logit drifted for {}: {:?} / {:?} / {:?}",
                first.doc_id, first.raw_logit, second.raw_logit, third.raw_logit
            ),
        )?;
    }

    let order1 = descending_rerank_order(&run1);
    let order2 = descending_rerank_order(&run2);
    let order3 = descending_rerank_order(&run3);
    ensure(
        order1 == order2 && order2 == order3,
        format!("rerank order drifted: {order1:?} / {order2:?} / {order3:?}"),
    )?;
    ensure(
        order1.first().map(String::as_str) == Some("target"),
        format!("native reranker did not promote the precise release-policy target: {order1:?}"),
    )
}
