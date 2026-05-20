use std::process::{Command, Output};

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
