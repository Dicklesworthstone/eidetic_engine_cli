use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ee::cass::{CassClient, CassContract, REQUIRED_API_VERSION, REQUIRED_CONTRACT_VERSION};
use serde_json::{Map, Value};

type TestResult = Result<(), String>;

const FIXTURE_ROOT: &str = "tests/fixtures/cass/v1";
const REQUIRED_FEATURES: &[&str] = &[
    "api_version_command",
    "expand_command",
    "field_selection",
    "json_output",
    "request_id",
    "robot_meta",
    "status_command",
    "timeout",
    "view_command",
];
const REQUIRED_CONNECTORS: &[&str] = &["claude_code", "codex"];

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn fixture(name: &str) -> Result<Value, String> {
    let path = repo_path(&format!("{FIXTURE_ROOT}/{name}"));
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("{} is not valid JSON: {error}", path.display()))
}

fn object<'a>(value: &'a Value, context: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{context} must be a JSON object"))
}

fn array<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a Vec<Value>, String> {
    object
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("`{key}` must be an array"))
}

fn string<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("`{key}` must be a string"))
}

fn number(object: &Map<String, Value>, key: &str) -> Result<u64, String> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("`{key}` must be an unsigned integer"))
}

fn boolean(object: &Map<String, Value>, key: &str) -> Result<bool, String> {
    object
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("`{key}` must be a boolean"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_all_present(
    present: &BTreeSet<&str>,
    required: &[&str],
    description: &str,
) -> TestResult {
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|item| !present.contains(item))
        .collect();
    ensure(
        missing.is_empty(),
        format!("CASS capabilities missing {description}: {missing:?}"),
    )
}

fn ensure_args(invocation_args: &[std::ffi::OsString], expected: &[&str]) -> TestResult {
    let actual: Result<Vec<&str>, String> = invocation_args
        .iter()
        .map(|arg| {
            arg.to_str()
                .ok_or_else(|| format!("non-UTF-8 invocation arg: {arg:?}"))
        })
        .collect();
    let actual = actual?;
    ensure(
        actual == expected,
        format!("expected args {expected:?}, got {actual:?}"),
    )
}

#[test]
fn cass_v1_capabilities_declare_the_robot_surfaces_ee_consumes() -> TestResult {
    let capabilities = fixture("capabilities.json")?;
    let root = object(&capabilities, "capabilities")?;

    ensure(
        number(root, "api_version")? == u64::from(REQUIRED_API_VERSION),
        "capabilities api_version must match ee's required version",
    )?;
    ensure(
        string(root, "contract_version")? == REQUIRED_CONTRACT_VERSION,
        "capabilities contract_version must match ee's required version",
    )?;

    let features: BTreeSet<&str> = array(root, "features")?
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| "capabilities feature must be a string".to_string())
        })
        .collect::<Result<_, _>>()?;
    ensure_all_present(&features, REQUIRED_FEATURES, "required features")?;
    let connectors: BTreeSet<&str> = array(root, "connectors")?
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| "capabilities connector must be a string".to_string())
        })
        .collect::<Result<_, _>>()?;
    ensure_all_present(&connectors, REQUIRED_CONNECTORS, "harness connectors")?;

    let contract = CassContract::new(
        string(root, "crate_version")?,
        u32::try_from(number(root, "api_version")?)
            .map_err(|error| format!("api_version is out of range: {error}"))?,
        string(root, "contract_version")?,
        features.iter().copied(),
    );
    contract
        .ensure_compatible()
        .map_err(|error| error.to_string())?;
    ensure(
        contract.missing_required_capabilities().is_empty(),
        format!(
            "required capability set drifted: {:?}",
            contract.missing_required_capabilities()
        ),
    )
}

#[test]
fn unknown_cass_schema_versions_fail_with_adapter_schema_mismatch() -> TestResult {
    let future = fixture("api_version.future.json")?;
    let root = object(&future, "future api-version")?;
    let contract = CassContract::new(
        string(root, "crate_version")?,
        u32::try_from(number(root, "api_version")?)
            .map_err(|error| format!("api_version is out of range: {error}"))?,
        string(root, "contract_version")?,
        ["json_output"],
    );

    let error = match contract.ensure_compatible() {
        Ok(()) => return Err("future CASS schema must not be accepted".to_string()),
        Err(error) => error,
    };
    ensure(
        error.kind_str() == "external_adapter_schema_mismatch",
        format!("unexpected error kind `{}`", error.kind_str()),
    )?;
    ensure(
        error.repair_hint() == Some("upgrade cass to a compatible contract version"),
        "schema mismatch should carry a repair hint",
    )
}

#[test]
fn cass_search_view_and_expand_fixtures_match_robot_vocabulary() -> TestResult {
    let search = fixture("search_robot.json")?;
    let search_root = object(&search, "search robot")?;
    ensure(
        string(search_root, "query")? == "format before release",
        "query",
    )?;
    ensure(number(search_root, "limit")? == 2, "limit")?;
    ensure(!boolean(search_root, "hits_clamped")?, "hits_clamped")?;
    ensure(
        string(search_root, "request_id")? == "ee-gate6-search-001",
        "request_id",
    )?;
    let hit_value = array(search_root, "hits")?
        .first()
        .ok_or_else(|| "search fixture must include at least one hit".to_string())?;
    let hit = object(hit_value, "hits[0]")?;
    ensure(
        string(hit, "source_path")?.ends_with("session-a.jsonl"),
        "hit source_path",
    )?;
    ensure(number(hit, "line_number")? == 42, "hit line_number")?;
    ensure(string(hit, "agent")? == "codex", "hit agent")?;
    let meta = object(
        search_root
            .get("_meta")
            .ok_or_else(|| "search fixture missing _meta".to_string())?,
        "_meta",
    )?;
    ensure(number(meta, "max_tokens")? == 200, "max_tokens")?;
    ensure(number(meta, "timeout_ms")? == 30_000, "timeout_ms")?;

    let view = fixture("view.json")?;
    let view_root = object(&view, "view")?;
    ensure(number(view_root, "target_line")? == 2, "view target_line")?;
    ensure(number(view_root, "context")? == 1, "view context")?;
    let view_lines = array(view_root, "lines")?;
    ensure(view_lines.len() == 3, "view fixture line count")?;
    let highlighted_value = view_lines
        .get(1)
        .ok_or_else(|| "view fixture must include a highlighted target line".to_string())?;
    let highlighted = object(highlighted_value, "view lines[1]")?;
    ensure(
        boolean(highlighted, "highlighted")?,
        "view target highlighted",
    )?;

    let expand = fixture("expand.json")?;
    let messages = expand
        .as_array()
        .ok_or_else(|| "expand fixture must be a JSON array".to_string())?;
    ensure(messages.len() == 3, "expand message count")?;
    let target_value = messages
        .get(1)
        .ok_or_else(|| "expand fixture must include a target message".to_string())?;
    let target = object(target_value, "expand[1]")?;
    ensure(boolean(target, "is_target")?, "expand target flag")?;
    ensure(string(target, "role")? == "assistant", "expand target role")
}

#[test]
fn cass_invocations_are_noninteractive_budgeted_and_reapable() -> TestResult {
    let client = CassClient::new_default().with_timeout(Duration::from_secs(30));
    let preflight = client.preflight_invocations();
    ensure(preflight.len() == 3, "preflight invocation count")?;
    let api_version = preflight
        .first()
        .ok_or_else(|| "missing api-version preflight invocation".to_string())?;
    let capabilities = preflight
        .get(1)
        .ok_or_else(|| "missing capabilities preflight invocation".to_string())?;
    ensure_args(api_version.args(), &["api-version", "--json"])?;
    ensure_args(capabilities.args(), &["capabilities", "--json"])?;

    let search = client.search_invocation("format before release", "ee-gate6-search-001", 2, 200);
    ensure_args(
        search.args(),
        &[
            "search",
            "format before release",
            "--robot",
            "--robot-meta",
            "--fields",
            "minimal",
            "--limit",
            "2",
            "--max-tokens",
            "200",
            "--timeout",
            "30000",
            "--request-id",
            "ee-gate6-search-001",
        ],
    )?;
    ensure(
        search.timeout() == Some(Duration::from_secs(30)),
        "search timeout",
    )?;

    let view = client.view_invocation("/workspace/session-a.jsonl", 42, 1);
    ensure_args(
        view.args(),
        &[
            "view",
            "/workspace/session-a.jsonl",
            "-n",
            "42",
            "-C",
            "1",
            "--json",
        ],
    )?;
    ensure(
        view.timeout() == Some(Duration::from_secs(30)),
        "view timeout",
    )?;

    let expand = client.expand_invocation("/workspace/session-a.jsonl", 42, 1);
    ensure_args(
        expand.args(),
        &[
            "expand",
            "/workspace/session-a.jsonl",
            "-n",
            "42",
            "-C",
            "1",
            "--json",
        ],
    )?;
    ensure(
        expand.timeout() == Some(Duration::from_secs(30)),
        "expand timeout",
    )
}

#[test]
fn tiny_fixture_session_set_is_idempotency_ready() -> TestResult {
    let sessions = fixture("sessions.json")?;
    let root = object(&sessions, "sessions")?;
    let list = array(root, "sessions")?;
    ensure(list.len() == 1, "fixture must stay tiny and deterministic")?;
    let session_value = list
        .first()
        .ok_or_else(|| "sessions fixture must include one session".to_string())?;
    let session = object(session_value, "sessions[0]")?;
    ensure(
        string(session, "path")? == "/workspace/session-a.jsonl",
        "session path",
    )?;
    ensure(string(session, "agent")? == "codex", "session agent")?;
    ensure(number(session, "message_count")? == 2, "message count")?;
    ensure(
        !string(session, "content_hash")?.trim().is_empty(),
        "content hash",
    )?;

    let client = CassClient::new_default().with_timeout(Duration::from_secs(30));
    let invocation = client.sessions_invocation(Path::new("/workspace"), 1);
    ensure_args(
        invocation.args(),
        &[
            "sessions",
            "--workspace",
            "/workspace",
            "--json",
            "--limit",
            "1",
        ],
    )
}
