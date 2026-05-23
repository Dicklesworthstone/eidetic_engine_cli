//! Real-binary E2E coverage for the MCP `resources/list` path (bd-2fthw).
//!
//! Companion to `tests/e2e_mcp_resource_templates.rs` (bd-2wlym;
//! resources/templates/list), `tests/e2e_mcp_prompts.rs` (bd-4lz5u;
//! prompts/list), `tests/e2e_mcp_prompts_get_errors.rs` (bd-2j9z3;
//! prompts/get error vocabulary), and `tests/e2e_mcp_top_level.rs`
//! (bd-3u7n5; initialize + tools/list + shutdown). Prior coverage of
//! `resources/list` lived only at the library level inside
//! `tests/fixtures/golden/mcp/json_rpc_cases.json` (which drives
//! `handle_json_rpc_message`) — there was no real-binary E2E pinning
//! the per-resource uri/name/description vocabulary that downstream
//! agents bind to.
//!
//! The fixed top-level resources (`ee://agent-docs`, `ee://schemas`,
//! `ee://workspace/status`) and the `ee://agent-docs/{topic}` block
//! (12 topics, `AgentDocsTopic::all()` order) are stable static
//! patterns. The `ee://schemas/{id}` block is sanity-pinned against
//! `ee::output::public_schemas()` so any drift between the registry
//! and the MCP resources/list response fails with a precise diff
//! identifying the offending uri.

#![cfg(feature = "mcp")]
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
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

struct ExpectedResource {
    uri: &'static str,
    name: &'static str,
    description: &'static str,
}

const EXPECTED_FIXED_RESOURCES: &[ExpectedResource] = &[
    ExpectedResource {
        uri: "ee://agent-docs",
        name: "ee agent docs",
        description: "Agent-oriented overview of ee commands, contracts, and workflows",
    },
    ExpectedResource {
        uri: "ee://schemas",
        name: "ee schema registry",
        description: "List of public ee JSON schemas",
    },
    ExpectedResource {
        uri: "ee://workspace/status",
        name: "ee workspace status",
        description: "Current workspace and subsystem readiness from ee status --json",
    },
];

fn assert_resource_matches(actual: &Value, expected: &ExpectedResource) -> TestResult {
    let uri = actual["uri"]
        .as_str()
        .ok_or_else(|| format!("resource missing uri: {actual}"))?;
    if uri != expected.uri {
        return Err(format!(
            "resource uri drifted: expected {:?}, got {:?} (full: {actual})",
            expected.uri, uri
        ));
    }

    let name = actual["name"]
        .as_str()
        .ok_or_else(|| format!("resource {uri} missing name: {actual}"))?;
    if name != expected.name {
        return Err(format!(
            "resource {uri} name drifted: expected {:?}, got {:?}",
            expected.name, name
        ));
    }

    let description = actual["description"]
        .as_str()
        .ok_or_else(|| format!("resource {uri} missing description: {actual}"))?;
    if description != expected.description {
        return Err(format!(
            "resource {uri} description drifted: expected {:?}, got {:?}",
            expected.description, description
        ));
    }

    Ok(())
}

fn expected_agent_docs_topic_resources() -> Vec<ExpectedResource> {
    ee::core::agent_docs::AgentDocsTopic::all()
        .iter()
        .map(|topic| ExpectedResource {
            uri: Box::leak(format!("ee://agent-docs/{}", topic.as_str()).into_boxed_str()),
            name: Box::leak(format!("ee agent docs {}", topic.as_str()).into_boxed_str()),
            description: topic.description(),
        })
        .collect()
}

fn expected_schema_resources() -> Vec<ExpectedResource> {
    ee::output::public_schemas()
        .iter()
        .map(|schema| ExpectedResource {
            uri: Box::leak(format!("ee://schemas/{}", schema.id).into_boxed_str()),
            name: Box::leak(format!("ee schema {}", schema.id).into_boxed_str()),
            description: schema.description,
        })
        .collect()
}

#[test]
fn ee_mcp_serve_stdio_pins_resources_list_per_resource_vocabulary() -> TestResult {
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
            "clientInfo": { "name": "ee-mcp-resources-list-e2e", "version": "0.0.0" }
        }
    });
    writeln!(stdin, "{initialize_request}")
        .map_err(|error| format!("write initialize: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush initialize: {error}"))?;
    let _initialize_response = read_one_response_line(&mut stdout_reader)?;

    let resources_list_request = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "resources/list"
    });
    writeln!(stdin, "{resources_list_request}")
        .map_err(|error| format!("write resources/list: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush resources/list: {error}"))?;

    let resources_response = read_one_response_line(&mut stdout_reader)?;
    if resources_response["jsonrpc"].as_str() != Some("2.0") {
        return Err(format!(
            "resources/list response missing jsonrpc=2.0: {resources_response}"
        ));
    }
    if resources_response["id"].as_u64() != Some(2) {
        return Err(format!(
            "resources/list id must echo request id (2): {resources_response}"
        ));
    }
    if !resources_response["error"].is_null() {
        return Err(format!(
            "resources/list must succeed, got error envelope: {resources_response}"
        ));
    }

    let resources = resources_response["result"]["resources"]
        .as_array()
        .ok_or_else(|| {
            format!("resources/list result.resources must be an array: {resources_response}")
        })?;

    let agent_docs_topics = expected_agent_docs_topic_resources();
    let schema_resources = expected_schema_resources();
    let expected_total =
        EXPECTED_FIXED_RESOURCES.len() + agent_docs_topics.len() + schema_resources.len();
    if resources.len() != expected_total {
        return Err(format!(
            "resources/list must publish exactly {expected_total} resources \
             ({} fixed + {} agent-docs topics + {} schemas), got {} (full: {resources_response})",
            EXPECTED_FIXED_RESOURCES.len(),
            agent_docs_topics.len(),
            schema_resources.len(),
            resources.len(),
        ));
    }

    let mut iter = resources.iter();
    for expected in EXPECTED_FIXED_RESOURCES {
        let actual = iter
            .next()
            .ok_or_else(|| format!("missing fixed resource {}", expected.uri))?;
        assert_resource_matches(actual, expected)?;
    }
    for expected in &agent_docs_topics {
        let actual = iter
            .next()
            .ok_or_else(|| format!("missing agent-docs topic resource {}", expected.uri))?;
        assert_resource_matches(actual, expected)?;
    }

    let remaining: Vec<&Value> = iter.collect();
    if remaining.len() != schema_resources.len() {
        return Err(format!(
            "expected {} schema resources after fixed and agent-docs blocks, got {}",
            schema_resources.len(),
            remaining.len(),
        ));
    }

    let mut emitted_schema_uris: BTreeMap<&str, &Value> = BTreeMap::new();
    for resource in &remaining {
        let uri = resource["uri"]
            .as_str()
            .ok_or_else(|| format!("schema resource missing uri: {resource}"))?;
        if !uri.starts_with("ee://schemas/") {
            return Err(format!(
                "schema-block resource has unexpected uri prefix: {uri} (full: {resource})"
            ));
        }
        if emitted_schema_uris.insert(uri, resource).is_some() {
            return Err(format!("duplicate schema resource emitted: {uri}"));
        }
    }

    for expected in &schema_resources {
        let actual = emitted_schema_uris.get(expected.uri).ok_or_else(|| {
            format!(
                "public_schemas() declares {} but resources/list did not emit it",
                expected.uri
            )
        })?;
        assert_resource_matches(actual, expected)?;
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
