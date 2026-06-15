use chrono::{DateTime, Utc};
use ee::core::decide::{
    DECIDE_LIST_SCHEMA_V1, DECIDE_RECORD_SCHEMA_V1, DECIDE_REVISIT_SCHEMA_V1, DecideListOptions,
    DecideRecordOptions, DecideRevisitOptions, decide_list, decide_record, decide_revisit,
};
use ee::models::ProcessExitCode;
use serde_json::{Value, json};
use std::ffi::OsString;

type TestResult = Result<(), String>;

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn fixed_now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-06-15T12:00:00Z")
        .expect("fixed test timestamp parses")
        .with_timezone(&Utc)
}

fn invoke(args: &[&str]) -> (ProcessExitCode, String, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = ee::cli::run(args.iter().map(OsString::from), &mut stdout, &mut stderr);
    let stdout = String::from_utf8_lossy(&stdout).into_owned();
    let stderr = String::from_utf8_lossy(&stderr).into_owned();
    (exit, stdout, stderr)
}

#[test]
fn decide_record_list_revisit_json_shape_is_stable() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;
    let now = fixed_now();

    let recorded = decide_record(&DecideRecordOptions {
        workspace_path: temp.path(),
        database_path: None,
        topic: "Release verification lane",
        chosen: "rch-only targeted proof",
        alternatives: vec!["local cargo fallback".to_owned()],
        rationale: "Remote verification protects shared Mac disk pressure.",
        revisit_by: Some("+7d"),
        supersedes: None,
        dry_run: false,
        actor: Some("decide workflow test"),
        now: Some(now),
    })
    .map_err(|error| error.to_string())?;
    ensure_equal(&recorded.schema, &DECIDE_RECORD_SCHEMA_V1, "record schema")?;
    ensure_equal(&recorded.status, &"recorded".to_owned(), "record status")?;

    let list = decide_list(&DecideListOptions {
        workspace_path: temp.path(),
        database_path: None,
        about: Some("verification"),
        include_superseded: false,
        limit: 5,
        now: Some(now),
    })
    .map_err(|error| error.to_string())?;
    ensure_equal(&list.schema, &DECIDE_LIST_SCHEMA_V1, "list schema")?;
    ensure_equal(&list.total_count, &1, "list count")?;

    let revisit = decide_revisit(&DecideRevisitOptions {
        workspace_path: temp.path(),
        database_path: None,
        warning_days: Some(7),
        limit: 5,
        now: Some(now),
    })
    .map_err(|error| error.to_string())?;
    ensure_equal(&revisit.schema, &DECIDE_REVISIT_SCHEMA_V1, "revisit schema")?;
    ensure_equal(&revisit.due_count, &1, "revisit count")?;

    let mut golden = list.data_json();
    scrub_dynamic_fields(&mut golden);
    ensure_equal(
        &golden,
        &json!({
            "schema": "ee.decide.list.v1",
            "version": "[VERSION]",
            "workspaceId": "[WORKSPACE]",
            "databasePath": "[DB]",
            "about": "verification",
            "includeSuperseded": false,
            "totalCount": 1,
            "returnedCount": 1,
            "truncated": false,
            "decisions": [{
                "memoryId": "[MEMORY]",
                "topic": "Release verification lane",
                "normalizedTopic": "release-verification-lane",
                "chosen": "rch-only targeted proof",
                "alternatives": ["local cargo fallback"],
                "options": ["rch-only targeted proof", "local cargo fallback"],
                "rationale": "Remote verification protects shared Mac disk pressure.",
                "supersedes": null,
                "chainDepth": 0,
                "revisitBy": "2026-06-22T12:00:00Z",
                "revisitStatus": "future",
                "superseded": false,
                "validTo": null,
                "createdAt": "[TIME]"
            }]
        }),
        "scrubbed decide list golden",
    )
}

fn scrub_dynamic_fields(value: &mut Value) {
    if let Some(object) = value.as_object_mut() {
        if object.contains_key("version") {
            object.insert("version".to_owned(), json!("[VERSION]"));
        }
        if object.contains_key("workspaceId") {
            object.insert("workspaceId".to_owned(), json!("[WORKSPACE]"));
        }
        if object.contains_key("databasePath") {
            object.insert("databasePath".to_owned(), json!("[DB]"));
        }
        if object.contains_key("memoryId") {
            object.insert("memoryId".to_owned(), json!("[MEMORY]"));
        }
        if object.contains_key("createdAt") {
            object.insert("createdAt".to_owned(), json!("[TIME]"));
        }
        for child in object.values_mut() {
            scrub_dynamic_fields(child);
        }
    } else if let Some(array) = value.as_array_mut() {
        for child in array {
            scrub_dynamic_fields(child);
        }
    }
}

#[test]
fn decide_cli_record_list_revisit_exposes_subscribe_query() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = temp.path().to_string_lossy().into_owned();

    let (exit, _stdout, stderr) = invoke(&["ee", "--json", "--workspace", &workspace, "init"]);
    ensure_equal(&exit, &ProcessExitCode::Success, "init exit")?;
    ensure_equal(&stderr, &String::new(), "init stderr")?;

    let (exit, stdout, stderr) = invoke(&[
        "ee",
        "--json",
        "--workspace",
        &workspace,
        "decide",
        "record",
        "Release verification lane",
        "--chosen",
        "rch-only targeted proof",
        "--alternative",
        "local cargo fallback",
        "--rationale",
        "Remote verification protects shared Mac disk pressure.",
        "--revisit-by",
        "+7d",
    ]);
    ensure_equal(&exit, &ProcessExitCode::Success, "decide record exit")?;
    ensure_equal(&stderr, &String::new(), "decide record stderr")?;
    let recorded: Value = serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
    ensure_equal(
        &recorded["data"]["schema"],
        &json!("ee.decide.record.v1"),
        "record data schema",
    )?;
    ensure_equal(
        &recorded["data"]["subscribe"]["filter"],
        &json!("KIND=decision"),
        "record subscribe filter",
    )?;

    let (exit, stdout, stderr) = invoke(&[
        "ee",
        "--json",
        "--workspace",
        &workspace,
        "decide",
        "list",
        "--about",
        "verification",
    ]);
    ensure_equal(&exit, &ProcessExitCode::Success, "decide list exit")?;
    ensure_equal(&stderr, &String::new(), "decide list stderr")?;
    let list: Value = serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
    ensure_equal(
        &list["data"]["returnedCount"],
        &json!(1),
        "list returned count",
    )?;
    ensure_equal(
        &list["data"]["decisions"][0]["normalizedTopic"],
        &json!("release-verification-lane"),
        "list normalized topic",
    )?;

    let (exit, stdout, stderr) = invoke(&[
        "ee",
        "--json",
        "--workspace",
        &workspace,
        "decide",
        "revisit",
        "--warning-days",
        "7",
    ]);
    ensure_equal(&exit, &ProcessExitCode::Success, "decide revisit exit")?;
    ensure_equal(&stderr, &String::new(), "decide revisit stderr")?;
    let revisit: Value = serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
    ensure_equal(&revisit["data"]["dueCount"], &json!(1), "revisit due count")?;
    ensure_equal(
        &revisit["data"]["decisions"][0]["revisitStatus"],
        &json!("near_due"),
        "revisit near-due status",
    )
}

#[test]
fn decide_cli_supersedes_refuses_forks_and_filters_typed_fields() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = temp.path().to_string_lossy().into_owned();

    let (exit, _stdout, stderr) = invoke(&["ee", "--json", "--workspace", &workspace, "init"]);
    ensure_equal(&exit, &ProcessExitCode::Success, "init exit")?;
    ensure_equal(&stderr, &String::new(), "init stderr")?;

    let (exit, stdout, stderr) = invoke(&[
        "ee",
        "--json",
        "--workspace",
        &workspace,
        "decide",
        "record",
        "Storage layer topic",
        "--chosen",
        "FrankenSQLite",
        "--alternative",
        "SQLx",
        "--rationale",
        "Keep the source of truth in FrankenSQLite.",
        "--revisit-by",
        "2026-06-14T12:00:00Z",
    ]);
    ensure_equal(&exit, &ProcessExitCode::Success, "first record exit")?;
    ensure_equal(&stderr, &String::new(), "first record stderr")?;
    let first: Value = serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
    ensure_equal(
        &first["data"]["decision"]["chosen"],
        &json!("FrankenSQLite"),
        "first chosen field",
    )?;
    let first_id = first["data"]["decision"]["memoryId"]
        .as_str()
        .ok_or_else(|| format!("first decision missing memoryId: {first}"))?
        .to_owned();

    let (exit, stdout, _stderr) = invoke(&[
        "ee",
        "--json",
        "--workspace",
        &workspace,
        "decide",
        "record",
        "Storage layer topic",
        "--chosen",
        "SQLx",
        "--alternative",
        "FrankenSQLite",
        "--rationale",
        "A fork should be rejected unless it supersedes the live decision.",
    ]);
    ensure_equal(
        &exit,
        &ProcessExitCode::Usage,
        "same-topic fork exit code",
    )?;
    let fork: Value = serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
    ensure_equal(
        &fork["error"]["code"],
        &json!("decision_topic_requires_supersedes"),
        "same-topic fork error code",
    )?;

    let (exit, stdout, stderr) = invoke(&[
        "ee",
        "--json",
        "--workspace",
        &workspace,
        "decide",
        "record",
        "Storage layer topic",
        "--chosen",
        "RCH remote",
        "--alternative",
        "local Cargo",
        "--rationale",
        "Remote proof keeps build output off the shared Mac checkout.",
        "--supersedes",
        &first_id,
        "--revisit-by",
        "2026-06-14T12:00:00Z",
    ]);
    ensure_equal(&exit, &ProcessExitCode::Success, "superseding record exit")?;
    ensure_equal(&stderr, &String::new(), "superseding record stderr")?;
    let second: Value = serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
    ensure_equal(
        &second["data"]["superseded"]["memoryId"],
        &json!(first_id.as_str()),
        "superseded memory id",
    )?;
    ensure_equal(
        &second["data"]["decision"]["chainDepth"],
        &json!(1),
        "superseding chain depth",
    )?;
    let second_id = second["data"]["decision"]["memoryId"]
        .as_str()
        .ok_or_else(|| format!("second decision missing memoryId: {second}"))?
        .to_owned();

    let (exit, stdout, stderr) = invoke(&[
        "ee",
        "--json",
        "--workspace",
        &workspace,
        "decide",
        "list",
    ]);
    ensure_equal(&exit, &ProcessExitCode::Success, "head list exit")?;
    ensure_equal(&stderr, &String::new(), "head list stderr")?;
    let head_list: Value = serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
    ensure_equal(
        &head_list["data"]["returnedCount"],
        &json!(1),
        "head list returned count",
    )?;
    ensure_equal(
        &head_list["data"]["decisions"][0]["memoryId"],
        &json!(second_id.as_str()),
        "head list returns superseding decision",
    )?;

    let (exit, stdout, stderr) = invoke(&[
        "ee",
        "--json",
        "--workspace",
        &workspace,
        "decide",
        "list",
        "--include-superseded",
    ]);
    ensure_equal(&exit, &ProcessExitCode::Success, "history list exit")?;
    ensure_equal(&stderr, &String::new(), "history list stderr")?;
    let history: Value = serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
    ensure_equal(
        &history["data"]["returnedCount"],
        &json!(2),
        "history list returned count",
    )?;
    let prior = history["data"]["decisions"]
        .as_array()
        .and_then(|decisions| {
            decisions
                .iter()
                .find(|decision| decision["memoryId"] == json!(first_id.as_str()))
        })
        .ok_or_else(|| format!("history missing superseded decision: {history}"))?;
    ensure_equal(
        &prior["superseded"],
        &json!(true),
        "prior decision superseded flag",
    )?;
    ensure_equal(
        &prior["validTo"].is_string(),
        &true,
        "prior decision validTo is set",
    )?;

    let (exit, stdout, stderr) = invoke(&[
        "ee",
        "--json",
        "--workspace",
        &workspace,
        "decide",
        "revisit",
        "--warning-days",
        "30",
    ]);
    ensure_equal(&exit, &ProcessExitCode::Success, "revisit exit")?;
    ensure_equal(&stderr, &String::new(), "revisit stderr")?;
    let revisit: Value = serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
    ensure_equal(&revisit["data"]["dueCount"], &json!(1), "revisit due count")?;
    ensure_equal(
        &revisit["data"]["decisions"][0]["memoryId"],
        &json!(second_id.as_str()),
        "revisit returns live decision",
    )?;
    ensure_equal(
        &revisit["data"]["decisions"][0]["revisitStatus"],
        &json!("overdue"),
        "revisit overdue status",
    )?;

    let (exit, stdout, stderr) = invoke(&[
        "ee",
        "--json",
        "--workspace",
        &workspace,
        "memory",
        "show",
        &second_id,
    ]);
    ensure_equal(&exit, &ProcessExitCode::Success, "memory show exit")?;
    ensure_equal(&stderr, &String::new(), "memory show stderr")?;
    let show: Value = serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
    ensure_equal(
        &show["data"]["memory"]["typedFields"]["chosen"],
        &json!("RCH remote"),
        "memory show chosen typed field",
    )?;
    ensure_equal(
        &show["data"]["memory"]["typedFields"]["supersedes"],
        &json!(first_id.as_str()),
        "memory show supersedes typed field",
    )?;

    let (exit, stdout, stderr) = invoke(&[
        "ee",
        "--json",
        "--workspace",
        &workspace,
        "search",
        "remote proof",
        "--kind",
        "decision",
        "--field",
        "chosen=RCH remote",
    ]);
    ensure_equal(&exit, &ProcessExitCode::Success, "field search exit")?;
    ensure_equal(&stderr, &String::new(), "field search stderr")?;
    let search: Value = serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
    let result_ids = search["data"]["results"]
        .as_array()
        .ok_or_else(|| format!("search results missing array: {search}"))?
        .iter()
        .filter_map(|result| result["memoryId"].as_str().or_else(|| result["docId"].as_str()))
        .collect::<Vec<_>>();
    ensure_equal(
        &result_ids.contains(&second_id.as_str()),
        &true,
        "field search contains superseding decision",
    )?;
    ensure_equal(
        &result_ids.contains(&first_id.as_str()),
        &false,
        "field search omits superseded non-matching decision",
    )
}
