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

fn diagnose(workspace: &Path, code: &str, message: &str, record: bool) -> Result<Value, String> {
    diagnose_tool(workspace, "rustc", Some(code), None, message, record)
}

fn flag(data: &Value, key: &str) -> Result<bool, String> {
    data.get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("missing boolean field `{key}` in {data}"))
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
    if flag(&fresh, "recorded")? {
        return Err(format!(
            "diagnosis without --record must not persist: {fresh}"
        ));
    }

    // Record the class: it persists and recalls within the same invocation.
    let recorded = diagnose(&workspace, CODE, &message, true)?;
    if !flag(&recorded, "recorded")? {
        return Err(format!("--record must persist a fingerprint: {recorded}"));
    }
    if !flag(&recorded, "isKnown")? {
        return Err(format!("the just-recorded class must recall: {recorded}"));
    }

    // A later read-only diagnosis of the same class recalls the prior fingerprint.
    let again = diagnose(&workspace, CODE, &message, false)?;
    if !flag(&again, "isKnown")? {
        return Err(format!(
            "a recorded class must recall on a later diagnosis: {again}"
        ));
    }

    // A different error class must not recall.
    let other = diagnose(&workspace, "E0308", "mismatched types", false)?;
    if flag(&other, "isKnown")? {
        return Err(format!("an unseen class must not recall: {other}"));
    }

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
