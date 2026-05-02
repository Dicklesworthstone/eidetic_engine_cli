#[cfg(feature = "mcp")]
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

#[cfg(feature = "mcp")]
use serde_json::json;
use serde_json::{Map, Value};

type TestResult = Result<(), String>;

const CARGO_TOML_PATH: &str = "Cargo.toml";
const DEPENDENCY_MATRIX_PATH: &str =
    "tests/fixtures/golden/dependencies/contract_matrix.json.golden";
#[cfg(feature = "mcp")]
const MCP_JSON_RPC_CASES_PATH: &str = "tests/fixtures/golden/mcp/json_rpc_cases.json";
const FORBIDDEN_CRATES: &[&str] = &[
    "tokio",
    "tokio-util",
    "async-std",
    "smol",
    "rusqlite",
    "sqlx",
    "diesel",
    "sea-orm",
    "petgraph",
    "hyper",
    "axum",
    "tower",
    "reqwest",
];

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read_text(relative: &str) -> Result<String, String> {
    let path = repo_path(relative);
    fs::read_to_string(&path).map_err(|error| format!("failed to read {}: {error}", path.display()))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
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

fn boolean(object: &Map<String, Value>, key: &str) -> Result<bool, String> {
    object
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("`{key}` must be a boolean"))
}

fn load_json(relative: &str) -> Result<Value, String> {
    let text = read_text(relative)?;
    serde_json::from_str(&text).map_err(|error| format!("{relative} is invalid JSON: {error}"))
}

fn find_dependency_entry<'a>(
    matrix: &'a Value,
    name: &str,
) -> Result<&'a Map<String, Value>, String> {
    let root = object(matrix, "dependency matrix root")?;
    for (index, entry) in array(root, "entries")?.iter().enumerate() {
        let entry = object(entry, &format!("entries[{index}]"))?;
        if string(entry, "name")? == name {
            return Ok(entry);
        }
    }
    Err(format!("dependency matrix is missing `{name}`"))
}

#[test]
fn fastmcp_dependency_remains_explicitly_gated_after_no_go_spike() -> TestResult {
    let cargo_toml = read_text(CARGO_TOML_PATH)?;
    let uncommented_lines = cargo_toml
        .lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();

    ensure(
        uncommented_lines.contains(&"mcp = []"),
        "mcp feature must stay an empty adapter gate until FastMCP has a clean dependency audit",
    )?;
    ensure(
        !uncommented_lines
            .iter()
            .any(|line| line.contains("rust-mcp-sdk") || line.contains("fastmcp")),
        "Cargo.toml must not link FastMCP/rust-mcp-sdk while the no-go spike is active",
    )?;

    for forbidden in FORBIDDEN_CRATES {
        let dependency_prefix = format!("{forbidden} =");
        let feature_ref = format!("dep:{forbidden}");
        ensure(
            !uncommented_lines
                .iter()
                .any(|line| line.starts_with(&dependency_prefix) || line.contains(&feature_ref)),
            format!("Cargo.toml must not directly enable forbidden crate `{forbidden}`"),
        )?;
    }

    let matrix = load_json(DEPENDENCY_MATRIX_PATH)?;
    let fastmcp = find_dependency_entry(&matrix, "fastmcp-rust")?;
    ensure(
        string(fastmcp, "status")? == "planned_not_linked",
        "fastmcp-rust must remain planned_not_linked until a clean stdio adapter profile exists",
    )?;
    ensure(
        !boolean(fastmcp, "enabled_by_default")?,
        "fastmcp-rust must not be enabled by default",
    )?;
    ensure(
        string(fastmcp, "degradation_code")? == "mcp_unavailable",
        "FastMCP gate must expose the stable MCP degradation code",
    )?;

    let optional_profiles = array(fastmcp, "optional_feature_profiles")?;
    let Some(mcp_profile) = optional_profiles.first().and_then(Value::as_object) else {
        return Err("fastmcp-rust must record an mcp optional profile".to_string());
    };
    ensure(
        string(mcp_profile, "status")? == "blocked_until_dependency_audit",
        "FastMCP optional profile must stay blocked by dependency audit",
    )?;
    ensure(
        string(fastmcp, "release_pin_decision")?.contains("Do not link"),
        "FastMCP release decision must forbid linking before audit evidence exists",
    )
}

#[cfg(feature = "mcp")]
mod enabled_mcp {
    use super::*;

    fn rpc(request: Value) -> Option<Value> {
        ee::mcp::handle_json_rpc_message(&request)
    }

    fn response(request: Value, context: &str) -> Result<Value, String> {
        rpc(request).ok_or_else(|| format!("{context} unexpectedly suppressed its response"))
    }

    fn result<'a>(response: &'a Value, context: &str) -> Result<&'a Map<String, Value>, String> {
        let result = response
            .get("result")
            .ok_or_else(|| format!("{context} missing result"))?;
        object(result, context)
    }

    fn find_item<'a>(
        items: &'a [Value],
        field: &str,
        expected: &str,
        context: &str,
    ) -> Result<&'a Map<String, Value>, String> {
        for (index, item) in items.iter().enumerate() {
            let item = object(item, &format!("{context}[{index}]"))?;
            if item.get(field).and_then(Value::as_str) == Some(expected) {
                return Ok(item);
            }
        }
        Err(format!("{context} missing {field}={expected}"))
    }

    fn first_tool_text<'a>(response: &'a Value, context: &str) -> Result<&'a str, String> {
        response
            .get("result")
            .and_then(|result| result.get("content"))
            .and_then(Value::as_array)
            .and_then(|content| content.first())
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{context} missing first text content"))
    }

    fn assert_annotation(
        tool: &Map<String, Value>,
        key: &str,
        expected: bool,
        context: &str,
    ) -> TestResult {
        let annotations = tool
            .get("annotations")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("{context} missing annotations"))?;
        ensure(
            annotations.get(key).and_then(Value::as_bool) == Some(expected),
            format!("{context}.{key} must be {expected}"),
        )
    }

    fn assert_property(tool: &Map<String, Value>, property: &str, context: &str) -> TestResult {
        ensure(
            tool.get("inputSchema")
                .and_then(|schema| schema.get("properties"))
                .and_then(|properties| properties.get(property))
                .is_some(),
            format!("{context} missing input schema property `{property}`"),
        )
    }

    #[test]
    fn stdio_json_rpc_golden_cases_are_adapter_contracts() -> TestResult {
        let fixture = load_json(MCP_JSON_RPC_CASES_PATH)?;
        let cases = fixture
            .as_array()
            .ok_or_else(|| "MCP JSON-RPC golden root must be an array".to_string())?;

        let mut names = BTreeSet::new();
        for (index, case) in cases.iter().enumerate() {
            let case = object(case, &format!("cases[{index}]"))?;
            let name = string(case, "name")?;
            ensure(
                names.insert(name.to_owned()),
                format!("duplicate MCP golden case `{name}`"),
            )?;
            let request = case
                .get("request")
                .ok_or_else(|| format!("MCP golden case `{name}` missing request"))?;
            let expected = case
                .get("response")
                .ok_or_else(|| format!("MCP golden case `{name}` missing response"))?;
            let actual = ee::mcp::handle_json_rpc_message(request);
            if expected.is_null() {
                ensure(
                    actual.is_none(),
                    format!("MCP golden case `{name}` should suppress its response"),
                )?;
            } else {
                let actual = actual.ok_or_else(|| {
                    format!("MCP golden case `{name}` suppressed a required response")
                })?;
                ensure(
                    &actual == expected,
                    format!("MCP golden case `{name}` no longer matches the adapter"),
                )?;
            }
        }

        for required in [
            "initialize",
            "prompts_list",
            "resources_templates_list",
            "record_lesson_prompt",
            "cancelled_notification_suppresses_response",
            "cancelled_notification_with_id_error",
            "remember_write_gate_error",
        ] {
            ensure(
                names.contains(required),
                format!("MCP JSON-RPC golden fixture missing `{required}`"),
            )?;
        }
        Ok(())
    }

    #[test]
    fn stdio_surface_passes_gate_methods_and_read_only_annotations() -> TestResult {
        let initialize = response(
            json!({"jsonrpc": "2.0", "id": "init", "method": "initialize"}),
            "initialize",
        )?;
        let initialize_result = result(&initialize, "initialize result")?;
        ensure(
            string(initialize_result, "protocolVersion")? == ee::mcp::MCP_PROTOCOL_VERSION,
            "initialize protocol version must match the public MCP version",
        )?;
        let capabilities = initialize_result
            .get("capabilities")
            .and_then(Value::as_object)
            .ok_or_else(|| "initialize missing capabilities".to_string())?;
        for capability in ["tools", "resources", "prompts"] {
            ensure(
                capabilities.contains_key(capability),
                format!("initialize missing `{capability}` capability"),
            )?;
        }

        let tools_list = response(
            json!({"jsonrpc": "2.0", "id": "tools", "method": "tools/list"}),
            "tools/list",
        )?;
        let tools_result = result(&tools_list, "tools/list result")?;
        let tools = array(tools_result, "tools")?;
        for (name, required_property) in [
            ("ee_health", "workspace"),
            ("ee_status", "workspace"),
            ("ee_capabilities", "workspace"),
            ("ee_search", "query"),
            ("ee_context", "query"),
            ("ee_memory_show", "memoryId"),
            ("ee_why", "memoryId"),
        ] {
            let tool = find_item(tools, "name", name, "tools/list tools")?;
            assert_annotation(tool, "readOnlyHint", true, name)?;
            assert_annotation(tool, "idempotentHint", true, name)?;
            assert_annotation(tool, "destructiveHint", false, name)?;
            assert_property(tool, required_property, name)?;
        }
        for name in ["ee_remember", "ee_outcome"] {
            let tool = find_item(tools, "name", name, "tools/list tools")?;
            assert_annotation(tool, "readOnlyHint", false, name)?;
            assert_annotation(tool, "destructiveHint", false, name)?;
            let effect = tool
                .get("eeEffect")
                .and_then(Value::as_object)
                .ok_or_else(|| format!("{name} missing eeEffect"))?;
            ensure(
                boolean(effect, "defaultDryRun")?,
                format!("{name} must default to dry-run"),
            )?;
            ensure(
                boolean(effect, "requiresAllowWriteWhenDryRunFalse")?,
                format!("{name} must require allowWrite for durable writes"),
            )?;
        }

        let resources_list = response(
            json!({"jsonrpc": "2.0", "id": "resources", "method": "resources/list"}),
            "resources/list",
        )?;
        let resources_result = result(&resources_list, "resources/list result")?;
        let resources = array(resources_result, "resources")?;
        for uri in [
            "ee://agent-docs",
            "ee://schemas",
            "ee://schemas/ee.response.v1",
            "ee://workspace/status",
        ] {
            find_item(resources, "uri", uri, "resources/list resources")?;
        }

        let prompts_list = response(
            json!({"jsonrpc": "2.0", "id": "prompts", "method": "prompts/list"}),
            "prompts/list",
        )?;
        let prompts_result = result(&prompts_list, "prompts/list result")?;
        let prompts = array(prompts_result, "prompts")?;
        for name in ["pre-task-context", "record-lesson", "review-session"] {
            find_item(prompts, "name", name, "prompts/list prompts")?;
        }

        let status_call = response(
            json!({
                "jsonrpc": "2.0",
                "id": "status",
                "method": "tools/call",
                "params": {"name": "ee_status"}
            }),
            "tools/call ee_status",
        )?;
        let status_text = first_tool_text(&status_call, "ee_status")?;
        let status_json: Value = serde_json::from_str(status_text)
            .map_err(|error| format!("ee_status returned invalid JSON: {error}"))?;
        ensure(
            status_json.get("schema").and_then(Value::as_str) == Some("ee.response.v1"),
            "ee_status must delegate to the stable CLI response envelope",
        )?;

        let budget_error = response(
            json!({
                "jsonrpc": "2.0",
                "id": "budget",
                "method": "tools/call",
                "params": {
                    "name": "ee_context",
                    "arguments": {"query": "prepare release", "maxTokens": 4294967296u64}
                }
            }),
            "tools/call ee_context budget overflow",
        )?;
        let error = budget_error
            .get("error")
            .and_then(Value::as_object)
            .ok_or_else(|| "budget overflow must return a JSON-RPC error".to_string())?;
        ensure(
            error.get("code").and_then(Value::as_i64) == Some(-32602),
            "budget overflow must be an invalid-params MCP error",
        )?;
        ensure(
            error.get("message").and_then(Value::as_str)
                == Some("Argument 'maxTokens' is too large"),
            "budget overflow must explain the rejected field",
        )?;

        ensure(
            rpc(json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": {"requestId": "status"}
            }))
            .is_none(),
            "notifications/cancelled must be a fire-and-forget notification",
        )
    }
}
