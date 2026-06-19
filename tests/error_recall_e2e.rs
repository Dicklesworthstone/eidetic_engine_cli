//! bd-1n0np.4.10 — real-binary E2E for error-recall subsystem reachability.
//!
//! The review finding (bd-1n0np.4.10) was that the error-recall library
//! (`core::error_recall`) and the V072 `error_fingerprints` store were
//! unreachable dead code: no command could populate or read them. The
//! `ee diagnose-error` command (handler → `core::error_diagnosis` →
//! `core::error_recall` + the store) now makes the subsystem reachable
//! end-to-end. This test exercises that path through the real `ee` binary —
//! state leaks that a library-level test would miss (per-process DB handles,
//! checkpoint behavior, the actual CLI envelope) surface here — and pins two
//! invariants:
//!
//!   * **Populate + recall.** `--record` persists a fingerprint for an error
//!     class and recalls it (`isKnown:true`); a later read-only diagnosis of
//!     the SAME class recalls the prior fingerprint; an unseen class does not
//!     (`isKnown:false`); and without `--record` nothing is persisted.
//!   * **Redaction-by-default (ADR-0057).** The RAW diagnostic text is NEVER
//!     persisted — only the fingerprint key + masked signatures. A unique raw
//!     sentinel token embedded in the message must NOT appear in any on-disk
//!     `.ee/` artifact (this is the bd-1n0np.4.8 raw-log-not-persisted
//!     assertion the original e2e omitted).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

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

fn success_json(output: &Output, context: &str) -> Result<Value, String> {
    if !output.status.success() {
        return Err(format!(
            "{context} failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    let value = parse_json(output, context)?;
    if value.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(format!("{context} did not return success=true: {value}"));
    }
    Ok(value)
}

fn tmp_workspace(label: &str) -> Result<PathBuf, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let base = std::env::temp_dir().join(format!(
        "ee-error-recall-{}-{}-{}",
        label,
        std::process::id(),
        nonce
    ));
    std::fs::create_dir_all(&base).map_err(|error| format!("create workspace: {error}"))?;
    Ok(base)
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

/// Run `ee diagnose-error` and return the `data` subtree of the
/// `ee.diagnose_error.v1` envelope.
fn diagnose_tool(
    workspace: &Path,
    tool: &str,
    code: Option<&str>,
    exit_code: Option<i32>,
    message: &str,
    record: bool,
) -> Result<Value, String> {
    diagnose_tool_with_args(workspace, tool, code, exit_code, message, record, &[])
}

fn diagnose_tool_with_args(
    workspace: &Path,
    tool: &str,
    code: Option<&str>,
    exit_code: Option<i32>,
    message: &str,
    record: bool,
    extra_args: &[(&str, &str)],
) -> Result<Value, String> {
    let workspace_arg = workspace.to_string_lossy().into_owned();
    let mut args = vec![
        "--workspace".to_string(),
        workspace_arg,
        "diagnose-error".to_string(),
        "--tool".to_string(),
        tool.to_string(),
    ];
    if let Some(code) = code {
        args.push("--code".to_string());
        args.push(code.to_string());
    }
    if let Some(exit_code) = exit_code {
        args.push("--exit-code".to_string());
        args.push(exit_code.to_string());
    }
    args.push(message.to_string());
    if record {
        args.push("--record".to_string());
    }
    for (flag, value) in extra_args {
        args.push((*flag).to_string());
        args.push((*value).to_string());
    }
    args.push("--json".to_string());
    let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = run_ee(&arg_refs)?;
    if !output.status.success() {
        return Err(format!(
            "diagnose-error failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    let value = parse_json(&output, "diagnose-error")?;
    // The schema must be the stable contract id.
    if value.pointer("/data/schema").and_then(Value::as_str) != Some("ee.diagnose_error.v1") {
        return Err(format!("unexpected diagnose-error schema: {value}"));
    }
    value
        .pointer("/data")
        .cloned()
        .ok_or_else(|| format!("diagnose-error response had no data: {value}"))
}

fn remember_memory(workspace: &Path, content: &str) -> Result<String, String> {
    let workspace_arg = workspace.to_string_lossy().into_owned();
    let output = run_ee(&[
        "--workspace",
        &workspace_arg,
        "remember",
        content,
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--json",
    ])?;
    let value = success_json(&output, "remember memory")?;
    value["data"]["memory_id"]
        .as_str()
        .or_else(|| value["data"]["public_id"].as_str())
        .or_else(|| value["data"]["id"].as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("remember response missing memory id: {value}"))
}

fn diagnose(workspace: &Path, code: &str, message: &str, record: bool) -> Result<Value, String> {
    diagnose_tool(workspace, "rustc", Some(code), None, message, record)
}

fn diagnose_error_log(workspace: &Path, error_log: &Path, record: bool) -> Result<Value, String> {
    let workspace_arg = workspace.to_string_lossy().into_owned();
    let error_log_arg = error_log.to_string_lossy().into_owned();
    let mut args = vec![
        "--workspace".to_string(),
        workspace_arg,
        "diagnose-error".to_string(),
        "--error-log".to_string(),
        error_log_arg,
    ];
    if record {
        args.push("--record".to_string());
    }
    args.push("--json".to_string());
    let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = run_ee(&arg_refs)?;
    if !output.status.success() {
        return Err(format!(
            "diagnose-error --error-log failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    let value = parse_json(&output, "diagnose-error --error-log")?;
    if value.pointer("/data/schema").and_then(Value::as_str) != Some("ee.diagnose_error.v1") {
        return Err(format!("unexpected diagnose-error schema: {value}"));
    }
    value
        .pointer("/data")
        .cloned()
        .ok_or_else(|| format!("diagnose-error response had no data: {value}"))
}

fn flag(data: &Value, key: &str) -> Result<bool, String> {
    data.get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("missing boolean field `{key}` in {data}"))
}

fn report(data: &Value) -> Result<&Value, String> {
    data.get("report")
        .ok_or_else(|| format!("missing error recall report in {data}"))
}

fn assert_report_shape(data: &Value, code: &str, expect_exact: bool) -> TestResult {
    let report = report(data)?;
    if report.get("schema").and_then(Value::as_str) != Some("ee.error_recall.report.v1") {
        return Err(format!("unexpected report schema: {report}"));
    }
    if report.get("exact").and_then(Value::as_bool) != Some(expect_exact) {
        return Err(format!(
            "report exact flag mismatch; expected {expect_exact}: {report}"
        ));
    }
    let derived = report
        .get("derivedDocument")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing derivedDocument in {report}"))?;
    let expected_parts = [
        "tool:rustc".to_owned(),
        "template:blake3:".to_owned(),
        format!("code:{code}"),
    ];
    for expected in expected_parts {
        if !derived.contains(expected.as_str()) {
            return Err(format!("derivedDocument missing `{expected}`: {derived}"));
        }
    }
    for array_field in [
        "near",
        "helpfulRepairs",
        "harmfulRepairs",
        "proofLinks",
        "staleVersionWarnings",
    ] {
        if !report
            .get(array_field)
            .is_some_and(serde_json::Value::is_array)
        {
            return Err(format!(
                "report field `{array_field}` must be an array: {report}"
            ));
        }
    }
    Ok(())
}

/// Recursively collect regular files under a directory.
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn bytes_contain(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn diagnose_error_records_and_recalls_through_the_real_binary() -> TestResult {
    let workspace = tmp_workspace("recall")?;
    init_workspace(&workspace)?;

    const CODE: &str = "E0277";
    // A token that cannot collide with any hex signature or masked shape, so its
    // presence on disk can only mean the raw message text was persisted.
    const SENTINEL: &str = "zzsentinelrawlog9981qux";
    let message = format!("the trait bound `{SENTINEL}: SomeTrait` is not satisfied");

    // Fresh store: the class is unknown and, without --record, not persisted.
    let fresh = diagnose(&workspace, CODE, &message, false)?;
    if flag(&fresh, "isKnown")? {
        return Err(format!(
            "fresh store must not recall an unseen class: {fresh}"
        ));
    }
    assert_report_shape(&fresh, CODE, false)?;
    if flag(&fresh, "recorded")? {
        return Err(format!(
            "diagnosis without --record must not persist: {fresh}"
        ));
    }

    let helpful_repair_id =
        remember_memory(&workspace, "Fix E0277 by importing the trait into scope.")?;
    let harmful_repair_id = remember_memory(
        &workspace,
        "Do not suppress E0277 by deleting the trait bound from the API.",
    )?;
    let proof_id = "rch-proof-error-recall-e2e";

    // Record the class: it persists and recalls within the same invocation.
    let recorded = diagnose_tool_with_args(
        &workspace,
        "rustc",
        Some(CODE),
        None,
        &message,
        true,
        &[
            ("--helpful-repair", helpful_repair_id.as_str()),
            ("--harmful-repair", harmful_repair_id.as_str()),
            ("--proof-link", proof_id),
        ],
    )?;
    if !flag(&recorded, "recorded")? {
        return Err(format!("--record must persist a fingerprint: {recorded}"));
    }
    if !flag(&recorded, "isKnown")? {
        return Err(format!("the just-recorded class must recall: {recorded}"));
    }
    assert_report_shape(&recorded, CODE, true)?;
    let recorded_report = report(&recorded)?;
    if recorded_report
        .get("helpfulRepairs")
        .and_then(Value::as_array)
        .and_then(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .find(|id| *id == helpful_repair_id.as_str())
        })
        .is_none()
    {
        return Err(format!(
            "recorded report must hydrate helpful repair link {helpful_repair_id}: {recorded_report}"
        ));
    }
    if recorded_report
        .get("harmfulRepairs")
        .and_then(Value::as_array)
        .and_then(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .find(|id| *id == harmful_repair_id.as_str())
        })
        .is_none()
    {
        return Err(format!(
            "recorded report must hydrate harmful repair link {harmful_repair_id}: {recorded_report}"
        ));
    }
    if recorded_report
        .get("proofLinks")
        .and_then(Value::as_array)
        .and_then(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .find(|id| *id == proof_id)
        })
        .is_none()
    {
        return Err(format!(
            "recorded report must hydrate proof link {proof_id}: {recorded_report}"
        ));
    }
    if recorded
        .get("matches")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty)
    {
        return Err(format!(
            "recorded class must emit at least one exact match: {recorded}"
        ));
    }

    // A later read-only diagnosis of the same class recalls the prior fingerprint.
    let again = diagnose(&workspace, CODE, &message, false)?;
    if !flag(&again, "isKnown")? {
        return Err(format!(
            "a recorded class must recall on a later diagnosis: {again}"
        ));
    }
    assert_report_shape(&again, CODE, true)?;
    let again_report = report(&again)?;
    if again_report
        .get("helpfulRepairs")
        .and_then(Value::as_array)
        .and_then(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .find(|id| *id == helpful_repair_id.as_str())
        })
        .is_none()
    {
        return Err(format!(
            "later report must hydrate persisted helpful repair link {helpful_repair_id}: {again_report}"
        ));
    }

    // A different error class must not recall.
    let other = diagnose(&workspace, "E0308", "mismatched types", false)?;
    if flag(&other, "isKnown")? {
        return Err(format!("an unseen class must not recall: {other}"));
    }
    assert_report_shape(&other, "E0308", false)?;

    let error_log_path = workspace.join("rustc-error.json");
    std::fs::write(
        &error_log_path,
        "error[E0277]: the trait bound `FilePathInput: SomeTrait` is not satisfied",
    )
    .map_err(|error| format!("write error log fixture: {error}"))?;
    let from_file = diagnose_error_log(&workspace, &error_log_path, false)?;
    if !flag(&from_file, "isKnown")? {
        return Err(format!(
            "--error-log file path should recall the recorded class: {from_file}"
        ));
    }
    assert_report_shape(&from_file, CODE, true)?;

    // `pack --error-log` should use the same explicit database/workspace routing
    // as normal pack execution. In particular, `pack build --query-file` must
    // honor the query document's workspace before deriving the recall seed.
    let workspace_arg = workspace.to_string_lossy().into_owned();
    let database_arg = workspace
        .join(".ee")
        .join("ee.db")
        .to_string_lossy()
        .into_owned();
    let error_log_arg = error_log_path.to_string_lossy().into_owned();

    let direct_pack = run_ee(&[
        "--workspace",
        &workspace_arg,
        "pack",
        "diagnose a build failure",
        "--database",
        &database_arg,
        "--error-log",
        &error_log_arg,
        "--read-only",
        "--json",
    ])?;
    success_json(&direct_pack, "pack --error-log with explicit database")?;

    let query_file = workspace.join("error-recall-query.json");
    let query_doc = serde_json::json!({
        "version": "ee.query.v1",
        "workspace": workspace_arg,
        "query": {"text": "diagnose a build failure"}
    });
    let query_doc = serde_json::to_vec(&query_doc)
        .map_err(|error| format!("serialize query document: {error}"))?;
    std::fs::write(&query_file, query_doc)
        .map_err(|error| format!("write query document: {error}"))?;
    let query_file_arg = query_file.to_string_lossy().into_owned();
    let query_pack = run_ee(&[
        "pack",
        "build",
        "--query-file",
        &query_file_arg,
        "--database",
        &database_arg,
        "--error-log",
        &error_log_arg,
        "--read-only",
        "--json",
    ])?;
    success_json(
        &query_pack,
        "pack build --query-file --error-log with explicit database",
    )?;

    // Secret-like diagnostic payload must be redacted before recall keys are
    // derived. Otherwise two identical shell failures that differ only in the
    // token value fragment into separate fingerprints and are not recalled.
    const SECRET_A: &str = "sk-proj-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SECRET_B: &str = "sk-proj-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let shell_a = format!("shell failed with OPENAI_API_KEY={SECRET_A}");
    let shell_b = format!("shell failed with OPENAI_API_KEY={SECRET_B}");
    let shell_recorded = diagnose_tool(&workspace, "shell", None, Some(17), &shell_a, true)?;
    if !flag(&shell_recorded, "recorded")? {
        return Err(format!(
            "--record must persist a shell fingerprint: {shell_recorded}"
        ));
    }
    if !flag(&shell_recorded, "isKnown")? {
        return Err(format!(
            "the just-recorded shell class must recall: {shell_recorded}"
        ));
    }
    let shell_again = diagnose_tool(&workspace, "shell", None, Some(17), &shell_b, false)?;
    if !flag(&shell_again, "isKnown")? {
        return Err(format!(
            "redacted-equivalent shell errors must share a recall class: {shell_again}"
        ));
    }

    // RCH diagnostics without an explicit blocker kind are code-less. The CLI
    // must not invent a catch-all canonical code, or unrelated remote failures
    // will falsely recall each other.
    let rch_recorded = diagnose_tool(
        &workspace,
        "rch",
        None,
        None,
        "remote hz1 failed to read /tmp/rch-sync/a/projects/frankensearch/Cargo.toml",
        true,
    )?;
    if rch_recorded.get("layer").and_then(Value::as_str) != Some("message_template") {
        return Err(format!(
            "code-less RCH diagnostics must use message_template layer: {rch_recorded}"
        ));
    }
    let unrelated_rch = diagnose_tool(
        &workspace,
        "rch",
        None,
        None,
        "worker admission failed because no workers passed health",
        false,
    )?;
    if flag(&unrelated_rch, "isKnown")? {
        return Err(format!(
            "unrelated code-less RCH failures must not recall via a fabricated code: {unrelated_rch}"
        ));
    }

    // Redaction-by-default (ADR-0057): the RAW diagnostic text is never
    // persisted. Raw sentinel and secret tokens must be absent from every `.ee/`
    // artifact (DB, WAL/SHM, index) because only fingerprint keys and masked
    // signatures are stored.
    let ee_dir = workspace.join(".ee");
    let mut files = Vec::new();
    collect_files(&ee_dir, &mut files);
    if files.is_empty() {
        return Err(format!("no .ee artifacts found under {}", ee_dir.display()));
    }
    for file in &files {
        let bytes =
            std::fs::read(file).map_err(|error| format!("read {}: {error}", file.display()))?;
        for needle in [SENTINEL, SECRET_A, SECRET_B] {
            if bytes_contain(&bytes, needle.as_bytes()) {
                return Err(format!(
                    "raw diagnostic text leaked: token `{needle}` found in {}",
                    file.display()
                ));
            }
        }
    }

    Ok(())
}
