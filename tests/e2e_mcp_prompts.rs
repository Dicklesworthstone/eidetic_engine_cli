//! Real-binary E2E coverage for the MCP `prompts/list` path (bd-4lz5u).
//!
//! `tests/e2e_mcp_top_level.rs` (bd-3u7n5) covers `initialize → tools/list →
//! shutdown` through the real `ee mcp serve-stdio` binary, but stops short
//! of `prompts/list`. The prompts/list path is covered only at the library
//! level (via `handle_json_rpc_message` in `tests/mcp_parity.rs`) and as
//! part of `tests/fixtures/golden/mcp/json_rpc_cases.json` — neither
//! spawns the real binary, and neither pins the per-variant prompt
//! vocabulary (name + description) on its own.
//!
//! This file pins the four published prompt descriptors
//! (`pre-task-context`, `pre-edit-recall`, `record-lesson`, `review-session`) through the
//! real stdio loop, so any rename, description rewording, or
//! required-flag flip on an argument trips a focused, attributable test.
//! That vocabulary is what downstream agent harnesses match on; freezing
//! it here prevents silent surface drift.
//!
//! The shape that must remain stable:
//!   - exactly four prompts, in the order returned by `handle_prompts_list`
//!   - each prompt's `name`, `description`, and `arguments[*].{name,required}`
//!   - `arguments` for each prompt covers the expected named slots

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

struct ExpectedPrompt {
    name: &'static str,
    description: &'static str,
    /// (argument name, required-ness)
    arguments: &'static [(&'static str, bool)],
}

const EXPECTED_PROMPTS: &[ExpectedPrompt] = &[
    ExpectedPrompt {
        name: "pre-task-context",
        description: "Prepare an agent before a task by retrieving a context pack with ee.",
        arguments: &[
            ("task", true),
            ("workspace", false),
            ("profile", false),
            ("maxTokens", false),
        ],
    },
    ExpectedPrompt {
        name: "pre-edit-recall",
        description: "Recall code-anchored memories before editing files.",
        arguments: &[
            ("path", false),
            ("symbol", false),
            ("diff", false),
            ("diffStaged", false),
            ("workspace", false),
            ("budgetTokens", false),
        ],
    },
    ExpectedPrompt {
        name: "record-lesson",
        description: "Turn a durable lesson into an explicit ee remember workflow.",
        arguments: &[
            ("lesson", true),
            ("workspace", false),
            ("level", false),
            ("kind", false),
            ("tags", false),
        ],
    },
    ExpectedPrompt {
        name: "review-session",
        description: "Review a prior session and propose curation candidates.",
        arguments: &[("session", false), ("workspace", false), ("propose", false)],
    },
];

fn assert_prompt_matches(actual: &Value, expected: &ExpectedPrompt) -> TestResult {
    let name = actual["name"]
        .as_str()
        .ok_or_else(|| format!("prompt missing name: {actual}"))?;
    if name != expected.name {
        return Err(format!(
            "prompt name drifted: expected {:?}, got {:?} (full: {actual})",
            expected.name, name
        ));
    }

    let description = actual["description"]
        .as_str()
        .ok_or_else(|| format!("prompt {name} missing description: {actual}"))?;
    if description != expected.description {
        return Err(format!(
            "prompt {name} description drifted: expected {:?}, got {:?}",
            expected.description, description
        ));
    }

    let arguments = actual["arguments"]
        .as_array()
        .ok_or_else(|| format!("prompt {name} arguments must be an array: {actual}"))?;
    if arguments.len() != expected.arguments.len() {
        return Err(format!(
            "prompt {name} argument count drifted: expected {}, got {} (full: {actual})",
            expected.arguments.len(),
            arguments.len()
        ));
    }
    for (index, (argument, (expected_arg_name, expected_required))) in
        arguments.iter().zip(expected.arguments.iter()).enumerate()
    {
        let actual_arg_name = argument["name"]
            .as_str()
            .ok_or_else(|| format!("prompt {name} argument[{index}] missing name: {argument}"))?;
        if actual_arg_name != *expected_arg_name {
            return Err(format!(
                "prompt {name} argument[{index}] name drifted: expected {expected_arg_name:?}, \
                 got {actual_arg_name:?}"
            ));
        }
        let actual_required = argument["required"].as_bool().ok_or_else(|| {
            format!("prompt {name} argument {actual_arg_name} missing required bool: {argument}")
        })?;
        if actual_required != *expected_required {
            return Err(format!(
                "prompt {name} argument {actual_arg_name} required drifted: expected \
                 {expected_required}, got {actual_required}"
            ));
        }
        if argument["description"].as_str().is_none() {
            return Err(format!(
                "prompt {name} argument {actual_arg_name} missing description: {argument}"
            ));
        }
    }

    Ok(())
}

#[test]
fn ee_mcp_serve_stdio_pins_prompts_list_per_variant_vocabulary() -> TestResult {
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
            "clientInfo": { "name": "ee-mcp-prompts-e2e", "version": "0.0.0" }
        }
    });
    writeln!(stdin, "{initialize_request}")
        .map_err(|error| format!("write initialize: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush initialize: {error}"))?;
    let _initialize_response = read_one_response_line(&mut stdout_reader)?;

    let prompts_list_request = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "prompts/list"
    });
    writeln!(stdin, "{prompts_list_request}")
        .map_err(|error| format!("write prompts/list: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush prompts/list: {error}"))?;

    let prompts_list_response = read_one_response_line(&mut stdout_reader)?;
    if prompts_list_response["jsonrpc"].as_str() != Some("2.0") {
        return Err(format!(
            "prompts/list response missing jsonrpc=2.0: {prompts_list_response}"
        ));
    }
    if prompts_list_response["id"].as_u64() != Some(2) {
        return Err(format!(
            "prompts/list id must echo the request id (2): {prompts_list_response}"
        ));
    }
    if !prompts_list_response["error"].is_null() {
        return Err(format!(
            "prompts/list must succeed, got error envelope: {prompts_list_response}"
        ));
    }
    let prompts = prompts_list_response["result"]["prompts"]
        .as_array()
        .ok_or_else(|| {
            format!("prompts/list result.prompts must be an array: {prompts_list_response}")
        })?;
    if prompts.len() != EXPECTED_PROMPTS.len() {
        return Err(format!(
            "prompts/list must publish exactly {} prompts, got {} (full: {prompts_list_response})",
            EXPECTED_PROMPTS.len(),
            prompts.len()
        ));
    }
    for (actual, expected) in prompts.iter().zip(EXPECTED_PROMPTS.iter()) {
        assert_prompt_matches(actual, expected)?;
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
