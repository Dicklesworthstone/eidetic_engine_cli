//! Static coverage gate for MCP tool parity tests (bd-3usjw.28).
//!
//! The runtime parity suite proves selected CLI/MCP outputs match. This file
//! proves the suite stays complete when a new MCP tool is registered.

#![cfg(feature = "mcp")]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value as JsonValue;

type TestResult = Result<(), String>;

const MCP_SOURCE: &str = include_str!("../src/mcp.rs");
const MCP_PARITY_SOURCE: &str = include_str!("mcp_parity.rs");

fn quoted_value(line: &str) -> Option<&str> {
    let start = line.find('"')?;
    let rest = &line[start + 1..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

fn quoted_values(line: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('"') {
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('"') else {
            break;
        };
        values.push(after_start[..end].to_string());
        rest = &after_start[end + 1..];
    }
    values
}

fn mcp_tool_registry_block() -> Result<&'static str, String> {
    let registry_start = MCP_SOURCE
        .find("const TOOL_REGISTRY")
        .ok_or_else(|| "src/mcp.rs missing TOOL_REGISTRY".to_string())?;
    let registry_end = MCP_SOURCE[registry_start..]
        .find("\n];\n\nfn mcp_tool_entry")
        .map(|offset| registry_start + offset)
        .ok_or_else(|| "src/mcp.rs TOOL_REGISTRY end marker changed".to_string())?;
    Ok(&MCP_SOURCE[registry_start..registry_end])
}

fn registered_mcp_tools() -> Result<BTreeMap<String, String>, String> {
    let mut tools = BTreeMap::new();
    for line in mcp_tool_registry_block()?.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("name: ") {
            continue;
        }
        let Some(tool_name) = quoted_value(trimmed) else {
            continue;
        };
        tools.insert(
            tool_name.to_string(),
            tool_name.trim_start_matches("ee_").to_string(),
        );
    }
    if tools.is_empty() {
        return Err("TOOL_REGISTRY has no registered tools".to_string());
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

fn parity_invoked_tools() -> Result<BTreeSet<String>, String> {
    let mut tools: BTreeSet<String> = MCP_PARITY_SOURCE
        .lines()
        .filter(|line| line.contains("run_mcp_tool_call("))
        .filter_map(quoted_value)
        .filter(|tool| tool.starts_with("ee_"))
        .map(str::to_string)
        .collect();

    for line in MCP_PARITY_SOURCE
        .lines()
        .filter(|line| line.contains("load_parity_fixture("))
    {
        let values = quoted_values(line);
        if values.len() < 2 {
            return Err(format!("could not parse load_parity_fixture line: {line}"));
        }
        let surface = &values[0];
        let name = &values[1];
        let path = parity_fixture_root()
            .join(surface)
            .join("inputs")
            .join(format!("{name}.json"));
        let contents = fs::read_to_string(&path)
            .map_err(|e| format!("failed to read {}: {e}", path_for_message(&path)))?;
        let fixture: JsonValue = serde_json::from_str(&contents)
            .map_err(|e| format!("failed to parse {}: {e}", path_for_message(&path)))?;
        tools.insert(validate_fixture_shape(&fixture, surface, &path)?);
    }

    Ok(tools)
}

fn parity_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("mcp_parity")
}

fn path_for_message(path: &Path) -> String {
    path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
        .unwrap_or(path)
        .display()
        .to_string()
}

fn fixture_string<'a>(fixture: &'a JsonValue, field: &str, path: &Path) -> Result<&'a str, String> {
    fixture
        .get(field)
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("{} missing string field {field}", path_for_message(path)))
}

fn validate_fixture_shape(
    fixture: &JsonValue,
    surface: &str,
    path: &Path,
) -> Result<String, String> {
    let fixture_surface = fixture_string(fixture, "surface", path)?;
    if fixture_surface != surface {
        return Err(format!(
            "{} has surface={fixture_surface:?}, expected {surface:?}",
            path_for_message(path)
        ));
    }

    let tool = fixture_string(fixture, "mcpTool", path)?;
    if !tool.starts_with("ee_") {
        return Err(format!(
            "{} has MCP tool {tool:?}; expected ee_* tool name",
            path_for_message(path)
        ));
    }

    let cli_args = fixture
        .get("cliArgs")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| format!("{} missing cliArgs array", path_for_message(path)))?;
    if cli_args.is_empty() || !cli_args.iter().all(JsonValue::is_string) {
        return Err(format!(
            "{} cliArgs must be a non-empty string array",
            path_for_message(path)
        ));
    }

    if !fixture
        .get("mcpArguments")
        .is_some_and(JsonValue::is_object)
    {
        return Err(format!(
            "{} missing mcpArguments object",
            path_for_message(path)
        ));
    }

    Ok(tool.to_string())
}

fn parity_fixture_tools() -> Result<BTreeMap<String, Vec<String>>, String> {
    let root = parity_fixture_root();
    let mut tools: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let surfaces = fs::read_dir(&root)
        .map_err(|e| format!("failed to read {}: {e}", path_for_message(&root)))?;

    for surface_entry in surfaces {
        let surface_entry = surface_entry.map_err(|e| e.to_string())?;
        if !surface_entry
            .file_type()
            .map_err(|e| e.to_string())?
            .is_dir()
        {
            continue;
        }
        let surface = surface_entry
            .file_name()
            .into_string()
            .map_err(|name| format!("non-UTF-8 fixture surface name: {name:?}"))?;
        let inputs_dir = surface_entry.path().join("inputs");
        if !inputs_dir.is_dir() {
            continue;
        }

        for fixture_entry in fs::read_dir(&inputs_dir)
            .map_err(|e| format!("failed to read {}: {e}", path_for_message(&inputs_dir)))?
        {
            let fixture_entry = fixture_entry.map_err(|e| e.to_string())?;
            let path = fixture_entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let contents = fs::read_to_string(&path)
                .map_err(|e| format!("failed to read {}: {e}", path_for_message(&path)))?;
            let fixture: JsonValue = serde_json::from_str(&contents)
                .map_err(|e| format!("failed to parse {}: {e}", path_for_message(&path)))?;
            let tool = validate_fixture_shape(&fixture, &surface, &path)?;
            tools.entry(tool).or_default().push(path_for_message(&path));
        }
    }

    for paths in tools.values_mut() {
        paths.sort();
    }
    Ok(tools)
}

#[test]
fn every_registered_mcp_tool_has_a_parity_invocation() -> TestResult {
    let registered: BTreeSet<String> = registered_mcp_tools()?.into_keys().collect();
    let declared = parity_declared_tools()?;
    let invoked = parity_invoked_tools()?;

    if registered != declared {
        let missing_from_declared: Vec<_> = registered.difference(&declared).collect();
        let stale_declared: Vec<_> = declared.difference(&registered).collect();
        return Err(format!(
            "PARITY_TESTED_TOOLS drifted from TOOL_REGISTRY; missing={missing_from_declared:?}; stale={stale_declared:?}"
        ));
    }

    if registered != invoked {
        let missing_invocations: Vec<_> = registered.difference(&invoked).collect();
        let stale_invocations: Vec<_> = invoked.difference(&registered).collect();
        return Err(format!(
            "MCP parity invocations drifted from TOOL_REGISTRY; missing={missing_invocations:?}; stale={stale_invocations:?}"
        ));
    }

    Ok(())
}

#[test]
fn every_parity_tested_tool_has_an_input_fixture() -> TestResult {
    let declared = parity_declared_tools()?;
    let fixture_tools = parity_fixture_tools()?;
    let tools_with_fixtures: BTreeSet<String> = fixture_tools.keys().cloned().collect();

    if declared != tools_with_fixtures {
        let missing_fixtures: Vec<_> = declared.difference(&tools_with_fixtures).collect();
        let stale_fixtures: Vec<_> = tools_with_fixtures.difference(&declared).collect();
        return Err(format!(
            "MCP parity fixture corpus drifted from PARITY_TESTED_TOOLS; missing={missing_fixtures:?}; stale={stale_fixtures:?}"
        ));
    }

    for tool in declared {
        let paths = fixture_tools
            .get(&tool)
            .ok_or_else(|| format!("{tool} missing fixture path after corpus scan"))?;
        if paths.is_empty() {
            return Err(format!(
                "{tool} must have at least one parity input fixture"
            ));
        }
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
