//! Contract tests for graph-oriented MCP tool registration.

use std::collections::BTreeSet;

use serde_json::{Value, json};

type TestResult = Result<(), String>;

fn tools_list() -> Result<Vec<Value>, String> {
    let request = json!({
        "jsonrpc": "2.0",
        "id": "graph-tools",
        "method": "tools/list"
    });
    let response = ee::mcp::handle_json_rpc_message(&request)
        .ok_or_else(|| "tools/list returned no response".to_string())?;
    response
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| format!("tools/list missing result.tools array: {response}"))
}

fn tool_by_name<'a>(tools: &'a [Value], name: &str) -> Result<&'a Value, String> {
    tools
        .iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
        .ok_or_else(|| format!("tools/list missing {name}"))
}

fn required_fields(tool: &Value) -> BTreeSet<&str> {
    tool.pointer("/inputSchema/required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

#[test]
fn graph_mcp_tools_are_registered_read_only() -> TestResult {
    let tools = tools_list()?;
    for name in [
        "ee_insights",
        "ee_proximity",
        "ee_pack_dna_explain",
        "ee_revision_impact",
    ] {
        let tool = tool_by_name(&tools, name)?;
        let annotations = tool
            .get("annotations")
            .ok_or_else(|| format!("{name} missing annotations"))?;
        if annotations.get("readOnlyHint").and_then(Value::as_bool) != Some(true) {
            return Err(format!("{name} must be read-only"));
        }
        if annotations.get("destructiveHint").and_then(Value::as_bool) != Some(false) {
            return Err(format!("{name} must be non-destructive"));
        }
        if tool.get("eeEffect").is_some() {
            return Err(format!("{name} must not advertise a write effect"));
        }
    }
    Ok(())
}

#[test]
fn graph_mcp_tools_publish_input_schemas() -> TestResult {
    let tools = tools_list()?;
    let expected_required = [
        ("ee_insights", BTreeSet::new()),
        ("ee_proximity", BTreeSet::from(["memoryIdA", "memoryIdB"])),
        ("ee_pack_dna_explain", BTreeSet::from(["query"])),
        ("ee_revision_impact", BTreeSet::from(["memoryId"])),
    ];

    for (name, required) in expected_required {
        let tool = tool_by_name(&tools, name)?;
        if tool.pointer("/inputSchema/type").and_then(Value::as_str) != Some("object") {
            return Err(format!("{name} input schema must be an object schema"));
        }
        if required_fields(tool) != required {
            return Err(format!(
                "{name} required fields mismatch: expected {required:?}, got {:?}",
                required_fields(tool)
            ));
        }
    }
    Ok(())
}
