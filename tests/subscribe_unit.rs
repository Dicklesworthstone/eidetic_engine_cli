use ee::core::init::{InitOptions, init_workspace};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::core::subscribe::{SubscribePollOptions, parse_subscribe_filter, poll_memory_deltas};

type TestResult = Result<(), String>;

fn init_temp_workspace() -> Result<tempfile::TempDir, String> {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let report = init_workspace(&InitOptions {
        workspace_path: tempdir.path().to_path_buf(),
        dry_run: false,
        repair_plan: false,
        force: false,
        allow_symlink: false,
        skip_boilerplate: true,
    });
    if !report.status.is_success() {
        return Err(format!(
            "init failed with status {}",
            report.status.as_str()
        ));
    }
    Ok(tempdir)
}

fn remember_rule(workspace: &std::path::Path, index: usize, tags: &str) -> Result<(), String> {
    remember_memory(&RememberMemoryOptions {
        workspace_path: workspace,
        database_path: None,
        content: &format!("Subscribe test procedural rule {index}."),
        workflow_id: None,
        level: "procedural",
        kind: "rule",
        tags: Some(tags),
        confidence: 0.8,
        source: None,
        allow_secret_mention: false,
        valid_from: None,
        valid_to: None,
        dry_run: false,
        auto_link: false,
        propose_candidates: false,
    })
    .map(|_| ())
    .map_err(|error| error.to_string())
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn subscribe_poll_advances_cursor_without_replay() -> TestResult {
    let tempdir = init_temp_workspace()?;
    for index in 0..5 {
        remember_rule(tempdir.path(), index, "release,ci")?;
    }

    let filter = parse_subscribe_filter(Some("LEVEL=procedural,TAG=release"))
        .map_err(|error| error.to_string())?;
    let first = poll_memory_deltas(&SubscribePollOptions {
        workspace_path: tempdir.path(),
        database_path: None,
        cursor: 0,
        filter: filter.clone(),
        limit: 100,
    })
    .map_err(|error| error.to_string())?;

    ensure(
        first.delta_count == 5,
        "first poll should return five deltas",
    )?;
    ensure(
        first
            .deltas
            .windows(2)
            .all(|window| window[0].cursor < window[1].cursor),
        "delta cursors must be strictly monotonic",
    )?;

    let second = poll_memory_deltas(&SubscribePollOptions {
        workspace_path: tempdir.path(),
        database_path: None,
        cursor: first.next_cursor,
        filter,
        limit: 100,
    })
    .map_err(|error| error.to_string())?;

    ensure(
        second.delta_count == 0,
        "second poll must not replay deltas",
    )?;
    ensure(
        second.next_cursor == first.next_cursor,
        "empty poll should preserve cursor",
    )
}

#[test]
fn subscribe_filter_excludes_non_matching_tags() -> TestResult {
    let tempdir = init_temp_workspace()?;
    remember_rule(tempdir.path(), 1, "release")?;
    remember_rule(tempdir.path(), 2, "other")?;

    let filter = parse_subscribe_filter(Some("LEVEL=procedural,TAG=release"))
        .map_err(|error| error.to_string())?;
    let report = poll_memory_deltas(&SubscribePollOptions {
        workspace_path: tempdir.path(),
        database_path: None,
        cursor: 0,
        filter,
        limit: 100,
    })
    .map_err(|error| error.to_string())?;

    ensure(report.delta_count == 1, "tag filter should keep one delta")?;
    ensure(
        report.deltas[0].tags == vec!["release".to_string()],
        "matching delta should carry canonical tags",
    )
}
