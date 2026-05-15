use std::process::{Command, Output};

type TestResult = Result<(), String>;

fn ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee: {error}"))
}

fn parse_stdout(output: &Output) -> Result<serde_json::Value, String> {
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("stdout should be JSON: {error}; stdout={:?}", output.stdout))
}

fn ensure_success(output: &Output, context: &str) -> TestResult {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{context}: expected success, status={:?}, stdout={}, stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

#[test]
fn preflight_bypass_token_cli_is_one_shot_and_redacts_list_output() -> TestResult {
    let workspace = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace_path = workspace.path().to_string_lossy().into_owned();

    let init = ee(&["--workspace", &workspace_path, "--json", "init"])?;
    ensure_success(&init, "init")?;

    let issued = ee(&[
        "--workspace",
        &workspace_path,
        "--json",
        "preflight",
        "issue-bypass-token",
        "--reason",
        "approve one destructive test command",
    ])?;
    ensure_success(&issued, "issue token")?;
    let issued_json = parse_stdout(&issued)?;
    let token = issued_json["data"]["report"]["token"]
        .as_str()
        .ok_or_else(|| "issued token should be present".to_owned())?
        .to_owned();
    let token_hash_prefix = issued_json["data"]["report"]["token_hash_prefix"]
        .as_str()
        .ok_or_else(|| "issued token hash prefix should be present".to_owned())?
        .to_owned();

    let first_use = ee(&[
        "--workspace",
        &workspace_path,
        "--json",
        "preflight",
        "check",
        "--cmd",
        "rm -rf /",
        "--override-token",
        &token,
    ])?;
    ensure_success(&first_use, "first use")?;
    let first_use_json = parse_stdout(&first_use)?;
    assert_eq!(first_use_json["schema"], "ee.preflight.guard.v1");
    assert_eq!(first_use_json["exitCode"], 0);
    assert_eq!(
        first_use_json["matches"][0]["resolution"],
        "bypassed_with_token"
    );

    let second_use = ee(&[
        "--workspace",
        &workspace_path,
        "--json",
        "preflight",
        "check",
        "--cmd",
        "rm -rf /",
        "--override-token",
        &token,
    ])?;
    assert_eq!(second_use.status.code(), Some(6));
    let second_use_json = parse_stdout(&second_use)?;
    assert_eq!(second_use_json["schema"], "ee.error.v2");
    assert_eq!(second_use_json["error"]["code"], "bypass_token_exhausted");

    let listed = ee(&[
        "--workspace",
        &workspace_path,
        "--json",
        "preflight",
        "list-bypass-tokens",
    ])?;
    ensure_success(&listed, "list tokens")?;
    let listed_stdout = String::from_utf8_lossy(&listed.stdout);
    if listed_stdout.contains(&token) {
        return Err("list-bypass-tokens output leaked raw token".to_owned());
    }
    let listed_json = parse_stdout(&listed)?;
    assert_eq!(listed_json["data"]["report"]["tokens"][0]["used_count"], 1);
    assert_eq!(
        listed_json["data"]["report"]["tokens"][0]["token_hash_prefix"],
        token_hash_prefix
    );

    let revocation_issued = ee(&[
        "--workspace",
        &workspace_path,
        "--json",
        "preflight",
        "issue-bypass-token",
        "--reason",
        "approve then revoke",
    ])?;
    ensure_success(&revocation_issued, "issue token for revocation")?;
    let revocation_json = parse_stdout(&revocation_issued)?;
    let revocation_token = revocation_json["data"]["report"]["token"]
        .as_str()
        .ok_or_else(|| "revocation token should be present".to_owned())?;

    let revoked = ee(&[
        "--workspace",
        &workspace_path,
        "--json",
        "preflight",
        "revoke-bypass-token",
        "--token",
        revocation_token,
    ])?;
    ensure_success(&revoked, "revoke token")?;
    let revoked_json = parse_stdout(&revoked)?;
    assert_eq!(
        revoked_json["data"]["report"]["token_hash_prefix"],
        revocation_json["data"]["report"]["token_hash_prefix"]
    );

    Ok(())
}

#[test]
fn override_token_records_bypass_audit_with_blocking_memory_provenance() -> TestResult {
    let workspace = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace_path = workspace.path().to_string_lossy().into_owned();
    let risk_content =
        "Prior incident: rm -rf /tmp/work recursively removed another agent workspace.";

    let init = ee(&["--workspace", &workspace_path, "--json", "init"])?;
    ensure_success(&init, "init")?;

    let remembered = ee(&[
        "--workspace",
        &workspace_path,
        "remember",
        risk_content,
        "--level",
        "procedural",
        "--kind",
        "risk",
        "--source",
        "cass-session://incident-rm-rf#L1-L3",
        "--no-auto-link",
        "--no-propose-candidates",
        "--json",
    ])?;
    ensure_success(&remembered, "remember risk memory")?;
    let remembered_json = parse_stdout(&remembered)?;
    let memory_id = remembered_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "remember response should include memory_id".to_owned())?
        .to_owned();

    let issued = ee(&[
        "--workspace",
        &workspace_path,
        "--json",
        "preflight",
        "issue-bypass-token",
        "--reason",
        "approve one destructive test command",
    ])?;
    ensure_success(&issued, "issue token")?;
    let issued_json = parse_stdout(&issued)?;
    let token = issued_json["data"]["report"]["token"]
        .as_str()
        .ok_or_else(|| "issued token should be present".to_owned())?
        .to_owned();
    let token_hash_prefix = issued_json["data"]["report"]["token_hash_prefix"]
        .as_str()
        .ok_or_else(|| "issued token hash prefix should be present".to_owned())?
        .to_owned();

    let bypassed = ee(&[
        "--workspace",
        &workspace_path,
        "--json",
        "preflight",
        "check",
        "--cmd",
        "rm -rf /tmp/work",
        "--override-token",
        &token,
    ])?;
    ensure_success(&bypassed, "override-token preflight")?;
    let bypassed_json = parse_stdout(&bypassed)?;
    assert_eq!(bypassed_json["schema"], "ee.preflight.guard.v1");
    assert_eq!(bypassed_json["exitCode"], 0);
    assert_eq!(
        bypassed_json["matches"][0]["resolution"],
        "bypassed_with_token"
    );
    assert_eq!(bypassed_json["matchedMemories"][0]["memory_id"], memory_id);

    let audit = ee(&[
        "--workspace",
        &workspace_path,
        "--json",
        "audit",
        "timeline",
    ])?;
    ensure_success(&audit, "audit timeline")?;
    let audit_stdout = String::from_utf8_lossy(&audit.stdout);
    if audit_stdout.contains(&token) {
        return Err("audit timeline leaked raw override token".to_owned());
    }
    let audit_json = parse_stdout(&audit)?;
    let entries = audit_json["entries"]
        .as_array()
        .ok_or_else(|| "audit timeline should include entries".to_owned())?;
    let bypass_audit = entries
        .iter()
        .find(|entry| entry["mutation_kind"] == "preflight.bypass")
        .ok_or_else(|| format!("audit timeline missing preflight.bypass entry: {audit_json}"))?;
    let details = if let Some(details) = bypass_audit["details"].as_str() {
        serde_json::from_str::<serde_json::Value>(details)
            .map_err(|error| format!("preflight.bypass details should be JSON: {error}"))?
    } else {
        bypass_audit["details"].clone()
    };
    assert_eq!(details["schema"], "ee.preflight.bypass.v1");
    assert_eq!(details["token_hash_prefix"], token_hash_prefix);
    assert_eq!(details["command"], "rm -rf /tmp/work");
    assert_eq!(details["matched_memory_ids"][0], memory_id);
    assert_eq!(details["matched_memories"][0]["memory_id"], memory_id);

    Ok(())
}
