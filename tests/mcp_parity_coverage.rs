//! Static coverage gate for MCP tool parity tests (bd-3usjw.28).
//!
//! The runtime parity suite proves selected CLI/MCP outputs match. This file
//! proves the suite stays complete when a new MCP tool is registered.

#![cfg(feature = "mcp")]

use std::collections::{BTreeMap, BTreeSet};

type TestResult = Result<(), String>;

const MCP_SOURCE: &str = include_str!("../src/mcp.rs");
const MCP_PARITY_SOURCE: &str = include_str!("mcp_parity.rs");

fn quoted_value(line: &str) -> Option<&str> {
    let start = line.find('"')?;
    let rest = &line[start + 1..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

fn mcp_tool_parse_block() -> Result<&'static str, String> {
    let impl_start = MCP_SOURCE
        .find("impl McpTool")
        .ok_or_else(|| "src/mcp.rs missing impl McpTool block".to_string())?;
    let parse_start = MCP_SOURCE[impl_start..]
        .find("fn parse")
        .map(|offset| impl_start + offset)
        .ok_or_else(|| "src/mcp.rs missing McpTool::parse".to_string())?;
    let parse_end = MCP_SOURCE[parse_start..]
        .find("\n    }\n}\n\nfn json_rpc_error")
        .map(|offset| parse_start + offset)
        .ok_or_else(|| "src/mcp.rs McpTool::parse end marker changed".to_string())?;
    Ok(&MCP_SOURCE[parse_start..parse_end])
}

fn registered_mcp_tools() -> Result<BTreeMap<String, String>, String> {
    let mut tools = BTreeMap::new();
    for line in mcp_tool_parse_block()?.lines() {
        if !line.contains("=> Some(Self::") {
            continue;
        }
        let Some(tool_name) = quoted_value(line) else {
            continue;
        };
        let variant = line
            .split("Some(Self::")
            .nth(1)
            .and_then(|tail| tail.split(')').next())
            .ok_or_else(|| format!("could not parse McpTool variant from line: {line}"))?;
        tools.insert(tool_name.to_string(), variant.to_string());
    }
    if tools.is_empty() {
        return Err("McpTool::parse has no registered tools".to_string());
    }
    Ok(tools)
}

fn parity_declared_tools() -> Result<BTreeSet<String>, String> {
    let start = MCP_PARITY_SOURCE
        .find("const PARITY_TESTED_TOOLS")
        .ok_or_else(|| "tests/mcp_parity.rs missing PARITY_TESTED_TOOLS".to_string())?;
    let end = MCP_PARITY_SOURCE[start..]
        .find("];")
        .map(|offset| start + offset)
        .ok_or_else(|| "tests/mcp_parity.rs PARITY_TESTED_TOOLS end marker changed".to_string())?;
    Ok(MCP_PARITY_SOURCE[start..end]
        .lines()
        .filter_map(quoted_value)
        .filter(|tool| tool.starts_with("ee_"))
        .map(str::to_string)
        .collect())
}

fn parity_invoked_tools() -> BTreeSet<String> {
    MCP_PARITY_SOURCE
        .lines()
        .filter(|line| line.contains("run_mcp_tool_call("))
        .filter_map(quoted_value)
        .filter(|tool| tool.starts_with("ee_"))
        .map(str::to_string)
        .collect()
}

#[test]
fn every_registered_mcp_tool_has_a_parity_invocation() -> TestResult {
    let registered: BTreeSet<String> = registered_mcp_tools()?.into_keys().collect();
    let declared = parity_declared_tools()?;
    let invoked = parity_invoked_tools();

    if registered != declared {
        let missing_from_declared: Vec<_> = registered.difference(&declared).collect();
        let stale_declared: Vec<_> = declared.difference(&registered).collect();
        return Err(format!(
            "PARITY_TESTED_TOOLS drifted from McpTool::parse; missing={missing_from_declared:?}; stale={stale_declared:?}"
        ));
    }

    if registered != invoked {
        let missing_invocations: Vec<_> = registered.difference(&invoked).collect();
        let stale_invocations: Vec<_> = invoked.difference(&registered).collect();
        return Err(format!(
            "MCP parity invocations drifted from McpTool::parse; missing={missing_invocations:?}; stale={stale_invocations:?}"
        ));
    }

    Ok(())
}

#[test]
fn mcp_tool_names_follow_cli_tool_naming_contract() -> TestResult {
    for (tool_name, variant) in registered_mcp_tools()? {
        if !tool_name.starts_with("ee_") {
            return Err(format!(
                "MCP tool {tool_name} for {variant} must start with ee_"
            ));
        }
        if tool_name.contains('-') {
            return Err(format!(
                "MCP tool {tool_name} for {variant} must use underscores, not hyphens"
            ));
        }
    }
    Ok(())
}
