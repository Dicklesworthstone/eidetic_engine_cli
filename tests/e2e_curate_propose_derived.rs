//! bd-kxm0c: real-binary pin test for `ee curate propose-derived`.
//!
//! Exercises the new explicit propose-derived CLI surface end-to-end:
//!
//! * `--dry-run` builds a canonical create_derived_memory candidate
//!   package without mutating the database (target_memory_id=null,
//!   source_refs sorted by (kind, id), nextCommands point to the
//!   ordinary curate validate/apply path).
//! * Non-dry-run inserts a pending candidate visible via
//!   `ee curate candidates --status pending --json`.
//! * Repeating the same proposal is idempotent: the second invocation
//!   does NOT insert a duplicate row, but still returns the existing
//!   candidate id so the caller can chain validate/apply.
//! * Missing source ids surface a documented DomainError::Usage repair.
//! * Citing a non-existent source memory surfaces DomainError::NotFound
//!   with a recovery hint pointing at `ee memory show`.

#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn run_copyable_ee_command(command: &str) -> Result<Output, String> {
    let ee_path = PathBuf::from(env!("CARGO_BIN_EXE_ee"));
    let ee_dir = ee_path
        .parent()
        .ok_or_else(|| format!("ee binary path has no parent: {}", ee_path.display()))?;
    let mut paths = vec![ee_dir.to_path_buf()];
    if let Some(current_path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&current_path));
    }
    let path = std::env::join_paths(paths).map_err(|error| error.to_string())?;

    Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("PATH", path)
        .output()
        .map_err(|error| format!("failed to run copied command `{command}`: {error}"))
}

fn unique_workspace(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-curate-propose-derived-pin")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

fn init_workspace(workspace_arg: &str) -> TestResult {
    let init = run_ee(&["--workspace", workspace_arg, "--json", "init"])?;
    ensure(
        init.status.success(),
        format!(
            "ee init must succeed; stderr: {}",
            String::from_utf8_lossy(&init.stderr)
        ),
    )
}

fn remember(workspace_arg: &str, content: &str) -> Result<String, String> {
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "remember",
        "--level",
        "semantic",
        "--kind",
        "fact",
        content,
    ])?;
    if !output.status.success() {
        return Err(format!(
            "remember failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    let parsed: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    parsed["data"]["public_id"]
        .as_str()
        .or_else(|| parsed["data"]["memory_id"].as_str())
        .or_else(|| parsed["data"]["id"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "remember response missing memory id: {}",
                serde_json::to_string(&parsed).unwrap_or_default()
            )
        })
}

fn propose_derived(
    workspace_arg: &str,
    sources: &[(&str, &str)],
    content: &str,
    extra: &[&str],
) -> Result<(Output, Value), String> {
    let mut args: Vec<String> = vec![
        "--workspace".to_owned(),
        workspace_arg.to_owned(),
        "--json".to_owned(),
        "curate".to_owned(),
        "propose-derived".to_owned(),
        "--level".to_owned(),
        "semantic".to_owned(),
        "--kind".to_owned(),
        "insight".to_owned(),
        "--content".to_owned(),
        content.to_owned(),
        "--producer-kind".to_owned(),
        "e2e_test".to_owned(),
    ];
    for (kind, id) in sources {
        match *kind {
            "memory" => {
                args.push("--source-memory".to_owned());
                args.push((*id).to_owned());
            }
            "evidence_span" => {
                args.push("--source-evidence-span".to_owned());
                args.push((*id).to_owned());
            }
            other => {
                return Err(format!("unsupported source kind in test helper: {other}"));
            }
        }
    }
    for arg in extra {
        args.push((*arg).to_owned());
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = run_ee(&arg_refs)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("propose-derived stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn assert_envelope_shape(
    data: &Value,
    expected_persisted: bool,
    expected_dry_run: bool,
) -> TestResult {
    ensure(
        data["schema"].as_str() == Some("ee.curate.propose_derived.v1"),
        format!("data.schema must be ee.curate.propose_derived.v1; got {data}"),
    )?;
    ensure(
        data["command"].as_str() == Some("curate propose-derived"),
        format!("data.command must be curate propose-derived; got {data}"),
    )?;
    ensure(
        data["candidateType"].as_str() == Some("create_derived_memory"),
        format!("candidateType must be create_derived_memory; got {data}"),
    )?;
    ensure(
        data["targetMemoryId"].is_null(),
        format!("targetMemoryId must be null for create-derived candidates; got {data}"),
    )?;
    ensure(
        data["persisted"] == Value::Bool(expected_persisted),
        format!("persisted must be {expected_persisted}; got {data}"),
    )?;
    ensure(
        data["dryRun"] == Value::Bool(expected_dry_run),
        format!("dryRun must be {expected_dry_run}; got {data}"),
    )?;
    ensure(
        data["candidateId"]
            .as_str()
            .is_some_and(|id| id.starts_with("curate_")),
        format!("candidateId must start with curate_; got {data}"),
    )?;
    ensure(
        data.get("nextActions").is_none(),
        format!("propose-derived must expose nextCommands, not nextActions; got {data}"),
    )?;
    let next_commands = data["nextCommands"]
        .as_array()
        .ok_or_else(|| format!("nextCommands must be an array; got {data}"))?;
    let candidate_id = data["candidateId"].as_str().unwrap_or("");
    ensure(
        next_commands.iter().any(|next| {
            next.as_str().is_some_and(|s| {
                s.starts_with("ee curate validate ")
                    && s.contains(candidate_id)
                    && s.contains("--workspace ")
                    && s.contains("--json")
            })
        }),
        format!(
            "nextCommands must include copyable `ee curate validate <id>`; got {next_commands:?}"
        ),
    )?;
    ensure(
        next_commands.iter().any(|next| {
            next.as_str().is_some_and(|s| {
                s.starts_with("ee curate apply ")
                    && s.contains(candidate_id)
                    && s.contains("--workspace ")
                    && s.contains("--json")
            })
        }),
        format!("nextCommands must include copyable `ee curate apply <id>`; got {next_commands:?}"),
    )?;
    Ok(())
}

fn next_command(data: &Value, prefix: &str) -> Result<String, String> {
    data["nextCommands"]
        .as_array()
        .ok_or_else(|| format!("nextCommands must be an array; got {data}"))?
        .iter()
        .filter_map(Value::as_str)
        .find(|command| command.starts_with(prefix))
        .map(str::to_owned)
        .ok_or_else(|| format!("nextCommands missing prefix `{prefix}`; got {data}"))
}

#[test]
fn curate_propose_derived_dry_run_does_not_mutate_database() -> TestResult {
    let workspace = unique_workspace("dry-run")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let source = remember(&workspace_arg, "Source memory for dry-run propose-derived.")?;

    let (output, parsed) = propose_derived(
        &workspace_arg,
        &[("memory", &source)],
        "Derived insight from dry-run pin test.",
        &["--dry-run"],
    )?;
    ensure(
        output.status.success(),
        format!(
            "dry-run propose-derived must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = &parsed["data"];
    assert_envelope_shape(data, false, true)?;
    let source_refs = data["sourceRefs"]
        .as_array()
        .ok_or_else(|| format!("sourceRefs must be an array; got {data}"))?;
    ensure(
        source_refs.len() == 1,
        format!("sourceRefs must contain one entry; got {source_refs:?}"),
    )?;
    ensure(
        source_refs[0]["kind"].as_str() == Some("memory"),
        format!("source kind must be memory; got {source_refs:?}"),
    )?;
    ensure(
        source_refs[0]["id"].as_str() == Some(source.as_str()),
        format!("source id must echo provided memory id; got {source_refs:?}"),
    )?;
    let content_hash = source_refs[0]["contentHash"]
        .as_str()
        .ok_or_else(|| format!("contentHash must be a string; got {source_refs:?}"))?;
    ensure(
        content_hash.starts_with("blake3:") && content_hash.len() == 71,
        format!("contentHash must be canonical blake3:<64hex>; got {content_hash}"),
    )?;

    // Database must not carry the candidate after a dry-run.
    let candidate_id = data["candidateId"].as_str().unwrap_or("").to_owned();
    let listing = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "curate",
        "candidates",
        "--status",
        "pending",
        "--all",
    ])?;
    ensure(
        listing.status.success(),
        format!(
            "curate candidates must succeed; stderr: {}",
            String::from_utf8_lossy(&listing.stderr)
        ),
    )?;
    let listing_text = String::from_utf8_lossy(&listing.stdout).into_owned();
    ensure(
        !listing_text.contains(&candidate_id),
        format!(
            "dry-run candidate {candidate_id} must NOT appear in curate candidates listing; got: {listing_text}"
        ),
    )?;
    Ok(())
}

#[test]
fn curate_propose_derived_inserts_pending_candidate_and_is_idempotent() -> TestResult {
    let workspace = unique_workspace("insert")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let source = remember(
        &workspace_arg,
        "Source memory for non-dry-run propose-derived.",
    )?;

    let (first_output, first_parsed) = propose_derived(
        &workspace_arg,
        &[("memory", &source)],
        "Derived insight from non-dry-run pin test.",
        &[],
    )?;
    ensure(
        first_output.status.success(),
        format!(
            "first propose-derived must succeed; stderr: {}",
            String::from_utf8_lossy(&first_output.stderr)
        ),
    )?;
    let first_data = &first_parsed["data"];
    assert_envelope_shape(first_data, true, false)?;
    let candidate_id = first_data["candidateId"]
        .as_str()
        .ok_or_else(|| "first candidate id missing".to_owned())?
        .to_owned();

    // Listing should now include the pending candidate.
    let listing = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "curate",
        "candidates",
        "--status",
        "pending",
    ])?;
    ensure(
        listing.status.success(),
        format!(
            "curate candidates must succeed; stderr: {}",
            String::from_utf8_lossy(&listing.stderr)
        ),
    )?;
    let listing_text = String::from_utf8_lossy(&listing.stdout).into_owned();
    ensure(
        listing_text.contains(&candidate_id),
        format!(
            "pending listing must include freshly proposed candidate {candidate_id}; got: {listing_text}"
        ),
    )?;

    // Re-running the exact same proposal must be idempotent: same
    // candidate id, persisted=false (already exists), no new row.
    let (second_output, second_parsed) = propose_derived(
        &workspace_arg,
        &[("memory", &source)],
        "Derived insight from non-dry-run pin test.",
        &[],
    )?;
    ensure(
        second_output.status.success(),
        "second propose-derived must succeed".to_owned(),
    )?;
    let second_data = &second_parsed["data"];
    ensure(
        second_data["candidateId"].as_str() == Some(candidate_id.as_str()),
        format!(
            "second invocation must reuse the candidate id; got {} vs {candidate_id}",
            second_data["candidateId"]
        ),
    )?;
    ensure(
        second_data["persisted"] == Value::Bool(false),
        format!("second invocation must report persisted=false; got {second_data}"),
    )?;
    ensure(
        second_data["durableMutation"] == Value::Bool(false),
        format!("second invocation must report durableMutation=false; got {second_data}"),
    )?;

    let validate_command = next_command(first_data, "ee curate validate ")?;
    let validate = run_copyable_ee_command(&validate_command)?;
    ensure(
        validate.status.success(),
        format!(
            "copied validate command must succeed; command={validate_command}; stdout={} stderr={}",
            String::from_utf8_lossy(&validate.stdout),
            String::from_utf8_lossy(&validate.stderr)
        ),
    )?;
    let validate_json: Value =
        serde_json::from_slice(&validate.stdout).map_err(|error| error.to_string())?;
    ensure(
        validate_json["data"]["nextAction"]
            .as_str()
            .is_some_and(|next| next.contains("ee curate apply")),
        format!("validate nextAction must point at apply; got {validate_json}"),
    )?;

    let apply_command = next_command(first_data, "ee curate apply ")?;
    let apply = run_copyable_ee_command(&apply_command)?;
    ensure(
        apply.status.success(),
        format!(
            "copied apply command must succeed; command={apply_command}; stdout={} stderr={}",
            String::from_utf8_lossy(&apply.stdout),
            String::from_utf8_lossy(&apply.stderr)
        ),
    )?;
    let apply_json: Value =
        serde_json::from_slice(&apply.stdout).map_err(|error| error.to_string())?;
    let created_memory_id = apply_json["data"]["application"]["createdMemoryId"]
        .as_str()
        .ok_or_else(|| format!("apply output must expose createdMemoryId; got {apply_json}"))?;
    let why_command = apply_json["data"]["nextAction"]
        .as_str()
        .ok_or_else(|| format!("apply output must expose nextAction; got {apply_json}"))?;
    ensure(
        why_command.starts_with("ee why ") && why_command.contains(created_memory_id),
        format!("apply nextAction must be a copyable why command; got {why_command}"),
    )?;

    let why = run_copyable_ee_command(why_command)?;
    ensure(
        why.status.success(),
        format!(
            "copied why command must succeed; command={why_command}; stdout={} stderr={}",
            String::from_utf8_lossy(&why.stdout),
            String::from_utf8_lossy(&why.stderr)
        ),
    )?;
    let why_json: Value = serde_json::from_slice(&why.stdout).map_err(|error| error.to_string())?;
    ensure(
        why_json["data"]["memoryId"].as_str() == Some(created_memory_id),
        format!("why output must describe created memory {created_memory_id}; got {why_json}"),
    )?;
    Ok(())
}

#[test]
fn curate_propose_derived_rejects_missing_sources_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-empty-sources")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "curate",
        "propose-derived",
        "--level",
        "semantic",
        "--kind",
        "insight",
        "--content",
        "Bare content without sources.",
    ])?;
    ensure(
        !output.status.success(),
        format!(
            "propose-derived without sources must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("error stdout must be JSON: {error}"))?;
    let message = parsed["error"]["message"].as_str().unwrap_or("");
    ensure(
        message.contains("at least one")
            && (message.contains("--source-memory") || message.contains("--source-evidence-span")),
        format!("usage message must explain that at least one source is required; got {message}"),
    )?;
    Ok(())
}

#[test]
fn curate_propose_derived_rejects_unknown_source_memory_with_recovery() -> TestResult {
    let workspace = unique_workspace("usage-unknown")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = propose_derived(
        &workspace_arg,
        &[("memory", "mem_does_not_exist_in_workspace")],
        "Should never persist.",
        &[],
    )?;
    ensure(
        !output.status.success(),
        format!(
            "propose-derived with unknown source must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let error = &parsed["error"];
    let message = error["message"].as_str().unwrap_or("");
    ensure(
        message.contains("mem_does_not_exist_in_workspace"),
        format!("error message must name the missing memory id; got {message}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or("");
    ensure(
        repair.contains("ee memory show"),
        format!("error repair must point at `ee memory show`; got {repair}"),
    )?;
    Ok(())
}
