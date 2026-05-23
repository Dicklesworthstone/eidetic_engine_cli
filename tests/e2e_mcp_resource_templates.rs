//! Real-binary E2E coverage for the MCP `resources/templates/list` path (bd-2wlym).
//!
//! Companion to `tests/e2e_mcp_top_level.rs` (bd-3u7n5; tools/list) and
//! `tests/e2e_mcp_prompts.rs` (bd-4lz5u; prompts/list). Prior coverage of
//! `resources/templates/list` lived only at the library level inside
//! `tests/fixtures/golden/mcp/json_rpc_cases.json` (which drives
//! `handle_json_rpc_message`) — there was no real-binary E2E pinning the
//! published URI-template vocabulary.
//!
//! The four templates published by `handle_resources_templates_list`
//! (`ee://memories/{memoryId}`, `ee://context-packs/by-query?query={query}`,
//! `ee://schemas/{schemaId}`, `ee://agent-docs/{topic}`) are stable static
//! patterns: they do not depend on the `public_schemas` v1↔v2 transition
//! that bd-13631 is normalising, so this test stays decoupled from that
//! work while still asserting the per-template name + description +
//! mimeType vocabulary downstream agents bind to.

#![cfg(feature = "mcp")]
#![allow(clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

type TestResult = Result<(), String>;

fn read_one_response_line(
    reader: &mut BufReader<std::process::ChildStdout>,
) -> Result<Value, String> {
    let mut line = String::new();
    let bytes = reader
        .read_line(&mut line)
        .map_err(|error| format!("read mcp response line: {error}"))?;
    if bytes == 0 {
        return Err("mcp stdio closed before delivering a response".to_owned());
    }
    serde_json::from_str(line.trim())
        .map_err(|error| format!("mcp response not valid JSON: {error}; line={line:?}"))
}

struct ExpectedTemplate {
    uri_template: &'static str,
    name: &'static str,
    description: &'static str,
}

const EXPECTED_TEMPLATES: &[ExpectedTemplate] = &[
    ExpectedTemplate {
        uri_template: "ee://memories/{memoryId}",
        name: "ee memory show",
        description: "Read a memory record through ee memory show --json",
    },
    ExpectedTemplate {
        uri_template: "ee://context-packs/by-query?query={query}",
        name: "ee context pack",
        description: "Assemble a task-specific context pack through ee context --json",
    },
    ExpectedTemplate {
        uri_template: "ee://schemas/{schemaId}",
        name: "ee schema export",
        description: "Read a public schema definition through ee schema export --json",
    },
    ExpectedTemplate {
        uri_template: "ee://agent-docs/{topic}",
        name: "ee agent docs topic",
        description: "Read an agent docs topic through ee agent-docs --json",
    },
];

fn assert_template_matches(actual: &Value, expected: &ExpectedTemplate) -> TestResult {
    let uri_template = actual["uriTemplate"]
        .as_str()
        .ok_or_else(|| format!("template missing uriTemplate: {actual}"))?;
    if uri_template != expected.uri_template {
        return Err(format!(
            "template uriTemplate drifted: expected {:?}, got {:?} (full: {actual})",
            expected.uri_template, uri_template
        ));
    }

    let name = actual["name"]
        .as_str()
        .ok_or_else(|| format!("template {uri_template} missing name: {actual}"))?;
    if name != expected.name {
        return Err(format!(
            "template {uri_template} name drifted: expected {:?}, got {:?}",
            expected.name, name
        ));
    }

    let description = actual["description"]
        .as_str()
        .ok_or_else(|| format!("template {uri_template} missing description: {actual}"))?;
    if description != expected.description {
        return Err(format!(
            "template {uri_template} description drifted: expected {:?}, got {:?}",
            expected.description, description
        ));
    }

    let mime_type = actual["mimeType"]
        .as_str()
        .ok_or_else(|| format!("template {uri_template} missing mimeType: {actual}"))?;
    if mime_type != "application/json" {
        return Err(format!(
            "template {uri_template} mimeType drifted: expected \"application/json\", got {mime_type:?}"
        ));
    }

    Ok(())
}

#[test]
fn ee_mcp_serve_stdio_pins_resources_templates_list_per_template_vocabulary() -> TestResult {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["mcp", "serve-stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawn ee mcp serve-stdio: {error}"))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "ee mcp serve-stdio stdin was not piped".to_owned())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ee mcp serve-stdio stdout was not piped".to_owned())?;
    let mut stdout_reader = BufReader::new(stdout);

    let initialize_request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": ee::mcp::MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "ee-mcp-resource-templates-e2e", "version": "0.0.0" }
        }
    });
    writeln!(stdin, "{initialize_request}")
        .map_err(|error| format!("write initialize: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush initialize: {error}"))?;
    let _initialize_response = read_one_response_line(&mut stdout_reader)?;

    let templates_list_request = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "resources/templates/list"
    });
    writeln!(stdin, "{templates_list_request}")
        .map_err(|error| format!("write resources/templates/list: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush resources/templates/list: {error}"))?;

    let templates_response = read_one_response_line(&mut stdout_reader)?;
    if templates_response["jsonrpc"].as_str() != Some("2.0") {
        return Err(format!(
            "resources/templates/list response missing jsonrpc=2.0: {templates_response}"
        ));
    }
    if templates_response["id"].as_u64() != Some(2) {
        return Err(format!(
            "resources/templates/list id must echo request id (2): {templates_response}"
        ));
    }
    if !templates_response["error"].is_null() {
        return Err(format!(
            "resources/templates/list must succeed, got error envelope: {templates_response}"
        ));
    }
    let templates = templates_response["result"]["resourceTemplates"]
        .as_array()
        .ok_or_else(|| {
            format!(
                "resources/templates/list result.resourceTemplates must be an array: {templates_response}"
            )
        })?;
    if templates.len() != EXPECTED_TEMPLATES.len() {
        return Err(format!(
            "resources/templates/list must publish exactly {} templates, got {} (full: {templates_response})",
            EXPECTED_TEMPLATES.len(),
            templates.len()
        ));
    }
    for (actual, expected) in templates.iter().zip(EXPECTED_TEMPLATES.iter()) {
        assert_template_matches(actual, expected)?;
    }

    let shutdown_request = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "shutdown"
    });
    writeln!(stdin, "{shutdown_request}").map_err(|error| format!("write shutdown: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush shutdown: {error}"))?;
    let _shutdown_response = read_one_response_line(&mut stdout_reader)?;
    drop(stdin);

    let mut trailing_stdout = String::new();
    stdout_reader
        .read_to_string(&mut trailing_stdout)
        .map_err(|error| format!("read trailing stdout: {error}"))?;
    if !trailing_stdout.trim().is_empty() {
        return Err(format!(
            "ee mcp serve-stdio emitted trailing stdout after shutdown: {trailing_stdout:?}"
        ));
    }

    let exit_status = child
        .wait()
        .map_err(|error| format!("wait for ee mcp serve-stdio child: {error}"))?;
    if !exit_status.success() {
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        return Err(format!(
            "ee mcp serve-stdio must exit cleanly after shutdown: status={exit_status}, \
             stderr={stderr}"
        ));
    }

    Ok(())
}
