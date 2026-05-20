use std::process::{Command, Output};

use serde_json::json;

type TestResult = Result<(), String>;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn expect_success(output: &Output, label: &str) -> TestResult {
    ensure(
        output.status.success(),
        format!(
            "{label} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

fn stdout_json(output: &Output, label: &str) -> Result<serde_json::Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{label}: stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{label}: stdout was not JSON: {error}\n{stdout}"))
}

fn log_event(phase: &str, payload: serde_json::Value) {
    eprintln!(
        "{}",
        json!({
            "schema": "ee.test_event.v1",
            "kind": "subscribe_filter_e2e",
            "phase": phase,
            "payload": payload,
        })
    );
}

fn remember(workspace: &str, content: &str, tags: &str) -> TestResult {
    let output = run_ee(&[
        "--workspace",
        workspace,
        "remember",
        content,
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--tags",
        tags,
        "--json",
    ])?;
    expect_success(&output, "remember")
}

fn poll_release_deltas(
    workspace: &str,
    cursor: u64,
    limit: u32,
) -> Result<serde_json::Value, String> {
    let cursor = cursor.to_string();
    let limit = limit.to_string();
    let output = run_ee(&[
        "--workspace",
        workspace,
        "subscribe",
        "poll",
        "--cursor",
        &cursor,
        "--limit",
        &limit,
        "--filter",
        "LEVEL=procedural,TAG=release,CHANGED_FIELDS=tags",
        "--json",
    ])?;
    expect_success(&output, "subscribe poll")?;
    stdout_json(&output, "subscribe poll")
}

#[test]
fn subscribe_poll_advances_cursor_across_filtered_audit_window() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    log_event("setup", json!({ "workspace": workspace }));
    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    expect_success(&init, "init")?;

    remember(
        &workspace,
        "Subscribe filter e2e release alpha should be searchable.",
        "release,subscribe",
    )?;
    remember(
        &workspace,
        "Subscribe filter e2e incident beta should not match release polls.",
        "incident,subscribe",
    )?;
    remember(
        &workspace,
        "Subscribe filter e2e release gamma should arrive after a skipped row.",
        "release,subscribe",
    )?;

    let search = run_ee(&[
        "--workspace",
        &workspace,
        "search",
        "Subscribe filter e2e release alpha",
        "--json",
    ])?;
    expect_success(&search, "search")?;
    let search_json = stdout_json(&search, "search")?;
    ensure(
        search_json["success"] == json!(true),
        "search should exercise the real index path successfully",
    )?;

    log_event("act_first_poll", json!({ "cursor": 0, "limit": 2 }));
    let first_json = poll_release_deltas(&workspace, 0, 2)?;
    let first_data = &first_json["data"];
    ensure(
        first_data["deltaCount"] == json!(1),
        format!("first poll should return only the first release delta: {first_data}"),
    )?;
    let first_next_cursor = first_data["nextCursor"]
        .as_u64()
        .ok_or_else(|| "first nextCursor must be u64".to_string())?;
    let first_delta_cursor = first_data["deltas"][0]["cursor"]
        .as_u64()
        .ok_or_else(|| "first delta cursor must be u64".to_string())?;
    ensure(
        first_next_cursor > first_delta_cursor,
        format!(
            "filtered window should advance past the skipped non-release row: next={first_next_cursor} delta={first_delta_cursor}"
        ),
    )?;
    log_event(
        "assert_first_poll",
        json!({
            "deltaCount": first_data["deltaCount"],
            "deltaCursor": first_delta_cursor,
            "nextCursor": first_next_cursor,
        }),
    );

    log_event(
        "act_second_poll",
        json!({ "cursor": first_next_cursor, "limit": 2 }),
    );
    let second_json = poll_release_deltas(&workspace, first_next_cursor, 2)?;
    let second_data = &second_json["data"];
    ensure(
        second_data["deltaCount"] == json!(1),
        format!("second poll should return the later release delta: {second_data}"),
    )?;
    let second_delta_cursor = second_data["deltas"][0]["cursor"]
        .as_u64()
        .ok_or_else(|| "second delta cursor must be u64".to_string())?;
    ensure(
        second_delta_cursor > first_next_cursor,
        format!(
            "second release delta should occur after the first poll window: second={second_delta_cursor} first_next={first_next_cursor}"
        ),
    )?;

    let second_next_cursor = second_data["nextCursor"]
        .as_u64()
        .ok_or_else(|| "second nextCursor must be u64".to_string())?;
    let third_json = poll_release_deltas(&workspace, second_next_cursor, 2)?;
    ensure(
        third_json["data"]["deltaCount"] == json!(0),
        format!(
            "third poll should not replay release deltas: {}",
            third_json["data"]
        ),
    )?;
    log_event(
        "pass",
        json!({
            "firstNextCursor": first_next_cursor,
            "secondDeltaCursor": second_delta_cursor,
            "secondNextCursor": second_next_cursor,
        }),
    );

    Ok(())
}
