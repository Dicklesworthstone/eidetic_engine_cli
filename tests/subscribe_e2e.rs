use std::process::{Command, Output};
use std::time::Instant;

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

fn stdout_json(output: &Output, label: &str) -> Result<serde_json::Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{label}: stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{label}: stdout was not JSON: {error}\n{stdout}"))
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

fn log_event(suite: &str, test: &str, phase: &str, event: &str, data: serde_json::Value) {
    eprintln!(
        "{}",
        serde_json::json!({
            "schema": "ee.test_event.v1",
            "suite": suite,
            "test": test,
            "phase": phase,
            "event": event,
            "data": data,
        })
    );
}

fn remember(workspace: &str, index: usize, tags: &str) -> TestResult {
    let content = format!("Subscribe e2e procedural rule {index}.");
    let output = run_ee(&[
        "--workspace",
        workspace,
        "remember",
        &content,
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

#[test]
fn subscribe_poll_and_stream_replay_memory_deltas() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    expect_success(&init, "init")?;

    for index in 0..5 {
        remember(&workspace, index, "release,subscribe")?;
    }

    let first = run_ee(&[
        "--workspace",
        &workspace,
        "subscribe",
        "poll",
        "--cursor",
        "0",
        "--filter",
        "LEVEL=procedural,TAG=release",
        "--json",
    ])?;
    expect_success(&first, "subscribe poll")?;
    let first_json = stdout_json(&first, "subscribe poll")?;
    ensure(
        first_json["data"]["deltaCount"] == serde_json::json!(5),
        "poll should return five matching deltas",
    )?;
    let next_cursor = first_json["data"]["nextCursor"]
        .as_u64()
        .ok_or_else(|| "poll nextCursor must be u64".to_string())?;

    let second = run_ee(&[
        "--workspace",
        &workspace,
        "subscribe",
        "poll",
        "--cursor",
        &next_cursor.to_string(),
        "--filter",
        "LEVEL=procedural,TAG=release",
        "--json",
    ])?;
    expect_success(&second, "subscribe second poll")?;
    let second_json = stdout_json(&second, "subscribe second poll")?;
    ensure(
        second_json["data"]["deltaCount"] == serde_json::json!(0),
        "second poll should not replay deltas",
    )?;

    let stream = run_ee(&[
        "--workspace",
        &workspace,
        "subscribe",
        "stream",
        "--cursor",
        "0",
        "--filter",
        "LEVEL=procedural,TAG=release",
        "--max-events",
        "5",
        "--json",
    ])?;
    expect_success(&stream, "subscribe stream")?;
    let stdout = String::from_utf8(stream.stdout).map_err(|error| error.to_string())?;
    let mut cursors = Vec::new();
    for line in stdout.lines() {
        let event: serde_json::Value =
            serde_json::from_str(line).map_err(|error| format!("stream line JSON: {error}"))?;
        ensure(
            event["schema"] == serde_json::json!("ee.memory.delta.v1"),
            "stream event schema",
        )?;
        cursors.push(
            event["cursor"]
                .as_u64()
                .ok_or_else(|| "stream event cursor must be u64".to_string())?,
        );
    }
    ensure(cursors.len() == 5, "stream should emit five events")?;
    ensure(
        cursors.windows(2).all(|window| window[0] < window[1]),
        "stream cursors must be strictly monotonic",
    )
}

#[test]
fn subscribe_poll_filters_and_paginates_real_audit_deltas() -> TestResult {
    let suite = "subscribe_e2e";
    let test = "subscribe_poll_filters_and_paginates_real_audit_deltas";
    let started = Instant::now();
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    log_event(
        suite,
        test,
        "setup",
        "workspace_created",
        serde_json::json!({ "workspace": workspace }),
    );
    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    expect_success(&init, "init")?;

    for (index, tags) in ["release,ci", "audit,ops", "release,audit"]
        .iter()
        .enumerate()
    {
        remember(&workspace, index, tags)?;
        log_event(
            suite,
            test,
            "setup",
            "memory_seeded",
            serde_json::json!({ "index": index, "tags": tags }),
        );
    }

    let all_match_first_page = run_ee(&[
        "--workspace",
        &workspace,
        "subscribe",
        "poll",
        "--cursor",
        "0",
        "--filter",
        "LEVEL=procedural,KIND=rule,TAG=release+audit,TAG_MODE=any,CHANGED_FIELDS=tags",
        "--limit",
        "2",
        "--json",
    ])?;
    expect_success(&all_match_first_page, "subscribe paginated poll")?;
    let first_page = stdout_json(&all_match_first_page, "subscribe paginated poll")?;
    log_event(
        suite,
        test,
        "act",
        "first_page",
        serde_json::json!({
            "deltaCount": first_page["data"]["deltaCount"],
            "nextCursor": first_page["data"]["nextCursor"],
        }),
    );
    ensure(
        first_page["data"]["deltaCount"] == serde_json::json!(2),
        "first page should inspect two matching real audit rows",
    )?;

    let first_page_deltas = first_page["data"]["deltas"]
        .as_array()
        .ok_or_else(|| "first page deltas should be an array".to_string())?;
    for delta in first_page_deltas {
        ensure(
            delta["schema"] == serde_json::json!("ee.memory.delta.v1"),
            "delta schema should be stable",
        )?;
        ensure(
            delta["kind"] == serde_json::json!("created"),
            "remembered memories should surface created deltas",
        )?;
        let changed_fields = delta["changedFields"]
            .as_array()
            .ok_or_else(|| "changedFields should be an array".to_string())?;
        ensure(
            changed_fields
                .iter()
                .any(|field| field == &serde_json::json!("tags")),
            "created deltas should expose tags as a changed field",
        )?;
    }

    let next_cursor = first_page["data"]["nextCursor"]
        .as_u64()
        .ok_or_else(|| "first page nextCursor must be u64".to_string())?;
    let all_match_second_page = run_ee(&[
        "--workspace",
        &workspace,
        "subscribe",
        "poll",
        "--cursor",
        &next_cursor.to_string(),
        "--filter",
        "LEVEL=procedural,KIND=rule,TAG=release+audit,TAG_MODE=any,CHANGED_FIELDS=tags",
        "--limit",
        "2",
        "--json",
    ])?;
    expect_success(&all_match_second_page, "subscribe paginated second poll")?;
    let second_page = stdout_json(&all_match_second_page, "subscribe paginated second poll")?;
    log_event(
        suite,
        test,
        "act",
        "second_page",
        serde_json::json!({
            "deltaCount": second_page["data"]["deltaCount"],
            "nextCursor": second_page["data"]["nextCursor"],
        }),
    );
    ensure(
        second_page["data"]["deltaCount"] == serde_json::json!(1),
        "second page should continue from nextCursor without replay",
    )?;

    let all_tags = run_ee(&[
        "--workspace",
        &workspace,
        "subscribe",
        "poll",
        "--cursor",
        "0",
        "--filter",
        "TAG=release+audit,TAG_MODE=all",
        "--json",
    ])?;
    expect_success(&all_tags, "subscribe all-tags poll")?;
    let all_tags_json = stdout_json(&all_tags, "subscribe all-tags poll")?;
    log_event(
        suite,
        test,
        "assert",
        "all_tags_filter",
        serde_json::json!({
            "deltaCount": all_tags_json["data"]["deltaCount"],
            "elapsedMs": started.elapsed().as_millis(),
        }),
    );
    ensure(
        all_tags_json["data"]["deltaCount"] == serde_json::json!(1),
        "TAG_MODE=all should keep only the memory carrying both tags",
    )
}

#[test]
fn subscribe_poll_reports_stale_cursor_with_real_database() -> TestResult {
    let suite = "subscribe_e2e";
    let test = "subscribe_poll_reports_stale_cursor_with_real_database";
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    log_event(
        suite,
        test,
        "setup",
        "workspace_created",
        serde_json::json!({ "workspace": workspace }),
    );
    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    expect_success(&init, "init")?;
    remember(&workspace, 0, "release,subscribe")?;

    let stale = run_ee(&[
        "--workspace",
        &workspace,
        "subscribe",
        "poll",
        "--cursor",
        "999999",
        "--filter",
        "LEVEL=procedural",
        "--json",
    ])?;
    expect_success(&stale, "subscribe stale cursor poll")?;
    let stale_json = stdout_json(&stale, "subscribe stale cursor poll")?;
    log_event(
        suite,
        test,
        "assert",
        "stale_cursor_degradation",
        serde_json::json!({
            "deltaCount": stale_json["data"]["deltaCount"],
            "nextCursor": stale_json["data"]["nextCursor"],
            "degraded": stale_json["data"]["degraded"],
        }),
    );

    ensure(
        stale_json["data"]["deltaCount"] == serde_json::json!(0),
        "stale cursor should not replay existing audit deltas",
    )?;
    let degraded = stale_json["data"]["degraded"]
        .as_array()
        .ok_or_else(|| "degraded should be an array".to_string())?;
    let top_level_degraded = stale_json["degraded"]
        .as_array()
        .ok_or_else(|| "top-level degraded should be an array".to_string())?;
    ensure(
        degraded
            .iter()
            .any(|entry| entry["code"] == serde_json::json!("subscribe_cursor_stale")),
        "stale cursor should emit subscribe_cursor_stale degradation",
    )?;
    ensure(
        top_level_degraded
            .iter()
            .any(|entry| entry["code"] == serde_json::json!("subscribe_cursor_stale")),
        "stale cursor degradation should be mirrored at the response top level",
    )
}
