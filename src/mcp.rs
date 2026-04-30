//! MCP (Model Context Protocol) adapter for ee (EE-211).
//!
//! This module provides a synchronous MCP stdio server that exposes ee commands
//! as MCP tools. It does NOT use rust-mcp-sdk because that crate requires Tokio.
//! Instead, this implements the MCP JSON-RPC 2.0 protocol directly over stdio.
//!
//! # Architecture
//!
//! MCP is a thin adapter layer over the same core services used by the CLI:
//! - No business logic duplication
//! - Same response contracts (ee.response.v1)
//! - Same error codes and degradation paths
//!
//! # Protocol
//!
//! MCP uses JSON-RPC 2.0 over stdio:
//! - One JSON object per line (JSONL)
//! - Server reads from stdin, writes to stdout
//! - Diagnostics go to stderr

use std::io::{BufRead, BufReader, Write};

use serde_json::{Value, json};

pub const SUBSYSTEM: &str = "mcp";
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
pub const MCP_SCHEMA_V1: &str = "ee.mcp.v1";

#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerInfo {
    pub name: &'static str,
    pub version: &'static str,
    pub protocol_version: &'static str,
}

impl McpServerInfo {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            name: "ee",
            version: env!("CARGO_PKG_VERSION"),
            protocol_version: MCP_PROTOCOL_VERSION,
        }
    }
}

impl Default for McpServerInfo {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpMethod {
    Initialize,
    ToolsList,
    ToolsCall,
    Shutdown,
    Unknown(String),
}

impl McpMethod {
    #[must_use]
    pub fn parse(method: &str) -> Self {
        match method {
            "initialize" => Self::Initialize,
            "tools/list" => Self::ToolsList,
            "tools/call" => Self::ToolsCall,
            "shutdown" => Self::Shutdown,
            other => Self::Unknown(other.to_string()),
        }
    }
}

fn json_rpc_error(id: Option<Value>, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

fn json_rpc_result(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

fn handle_initialize(id: Value) -> Value {
    let info = McpServerInfo::new();
    json_rpc_result(id, json!({
        "protocolVersion": info.protocol_version,
        "serverInfo": {
            "name": info.name,
            "version": info.version
        },
        "capabilities": {
            "tools": {}
        }
    }))
}

fn handle_tools_list(id: Value) -> Value {
    json_rpc_result(id, json!({
        "tools": [
            {
                "name": "ee_search",
                "description": "Search indexed memories and sessions",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Search query"
                        },
                        "workspace": {
                            "type": "string",
                            "description": "Workspace path (defaults to current directory)"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum results (default 10)"
                        }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "ee_context",
                "description": "Assemble task-specific context pack from relevant memories",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Task description"
                        },
                        "workspace": {
                            "type": "string",
                            "description": "Workspace path"
                        },
                        "max_tokens": {
                            "type": "integer",
                            "description": "Token budget (default 4000)"
                        }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "ee_status",
                "description": "Report workspace and subsystem readiness",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "workspace": {
                            "type": "string",
                            "description": "Workspace path"
                        }
                    }
                }
            },
            {
                "name": "ee_remember",
                "description": "Store a new memory",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "content": {
                            "type": "string",
                            "description": "Memory content"
                        },
                        "workspace": {
                            "type": "string",
                            "description": "Workspace path"
                        },
                        "level": {
                            "type": "string",
                            "description": "Memory level: episodic, semantic, procedural",
                            "enum": ["episodic", "semantic", "procedural"]
                        },
                        "kind": {
                            "type": "string",
                            "description": "Memory kind: fact, decision, outcome, lesson, rule, task, artifact, session"
                        }
                    },
                    "required": ["content"]
                }
            }
        ]
    }))
}

fn handle_tools_call(id: Value, params: Option<&Value>) -> Value {
    let Some(params) = params else {
        return json_rpc_error(Some(id), -32602, "Missing params");
    };

    let tool_name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let _arguments = params.get("arguments");

    match tool_name {
        "ee_search" | "ee_context" | "ee_status" | "ee_remember" => {
            json_rpc_result(id, json!({
                "content": [{
                    "type": "text",
                    "text": format!("Tool '{}' is registered but core services are not wired yet. See EE-211.", tool_name)
                }],
                "isError": false
            }))
        }
        _ => json_rpc_error(
            Some(id),
            -32601,
            &format!("Unknown tool: {tool_name}"),
        ),
    }
}

fn handle_shutdown(id: Value) -> Value {
    json_rpc_result(id, json!({}))
}

fn handle_request(request: &Value) -> Value {
    let id = request.get("id").cloned();
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("");
    let params = request.get("params");

    match McpMethod::parse(method) {
        McpMethod::Initialize => {
            let Some(id) = id else {
                return json_rpc_error(None, -32600, "Initialize requires id");
            };
            handle_initialize(id)
        }
        McpMethod::ToolsList => {
            let Some(id) = id else {
                return json_rpc_error(None, -32600, "tools/list requires id");
            };
            handle_tools_list(id)
        }
        McpMethod::ToolsCall => {
            let Some(id) = id else {
                return json_rpc_error(None, -32600, "tools/call requires id");
            };
            handle_tools_call(id, params)
        }
        McpMethod::Shutdown => {
            let Some(id) = id else {
                return json_rpc_error(None, -32600, "shutdown requires id");
            };
            handle_shutdown(id)
        }
        McpMethod::Unknown(m) => json_rpc_error(id, -32601, &format!("Unknown method: {m}")),
    }
}

/// Run the MCP stdio server.
///
/// Reads JSON-RPC requests from stdin (one per line) and writes responses to stdout.
/// Diagnostics go to stderr. Returns when stdin closes or shutdown is received.
///
/// # Errors
///
/// Returns an error string if the server encounters a fatal I/O error.
pub fn run_stdio_server() -> Result<(), String> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let reader = BufReader::new(stdin.lock());

    eprintln!("[ee-mcp] Server starting, protocol version {MCP_PROTOCOL_VERSION}");

    for line_result in reader.lines() {
        let line = line_result.map_err(|e| format!("stdin read error: {e}"))?;

        if line.trim().is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let error = json_rpc_error(None, -32700, &format!("Parse error: {e}"));
                writeln!(stdout, "{error}").map_err(|e| format!("stdout write error: {e}"))?;
                stdout.flush().map_err(|e| format!("stdout flush error: {e}"))?;
                continue;
            }
        };

        let response = handle_request(&request);
        writeln!(stdout, "{response}").map_err(|e| format!("stdout write error: {e}"))?;
        stdout.flush().map_err(|e| format!("stdout flush error: {e}"))?;

        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        if method == "shutdown" {
            eprintln!("[ee-mcp] Shutdown requested");
            break;
        }
    }

    eprintln!("[ee-mcp] Server stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_server_info_has_correct_values() {
        let info = McpServerInfo::new();
        assert_eq!(info.name, "ee");
        assert!(!info.version.is_empty());
        assert_eq!(info.protocol_version, MCP_PROTOCOL_VERSION);
    }

    #[test]
    fn mcp_method_parse_recognizes_standard_methods() {
        assert_eq!(McpMethod::parse("initialize"), McpMethod::Initialize);
        assert_eq!(McpMethod::parse("tools/list"), McpMethod::ToolsList);
        assert_eq!(McpMethod::parse("tools/call"), McpMethod::ToolsCall);
        assert_eq!(McpMethod::parse("shutdown"), McpMethod::Shutdown);
        assert!(matches!(
            McpMethod::parse("unknown"),
            McpMethod::Unknown(_)
        ));
    }

    #[test]
    fn handle_initialize_returns_server_info() {
        let response = handle_initialize(json!(1));
        let result = response.get("result").expect("result");

        assert_eq!(
            result.get("protocolVersion").and_then(Value::as_str),
            Some(MCP_PROTOCOL_VERSION)
        );
        assert_eq!(
            result
                .get("serverInfo")
                .and_then(|s| s.get("name"))
                .and_then(Value::as_str),
            Some("ee")
        );
    }

    #[test]
    fn handle_tools_list_returns_tool_definitions() {
        let response = handle_tools_list(json!(1));
        let result = response.get("result").expect("result");
        let tools = result.get("tools").and_then(Value::as_array).expect("tools array");

        assert!(!tools.is_empty());

        let tool_names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        assert!(tool_names.contains(&"ee_search"));
        assert!(tool_names.contains(&"ee_context"));
        assert!(tool_names.contains(&"ee_status"));
        assert!(tool_names.contains(&"ee_remember"));
    }

    #[test]
    fn handle_tools_call_unknown_returns_error() {
        let response = handle_tools_call(
            json!(1),
            Some(&json!({ "name": "nonexistent_tool" })),
        );
        let error = response.get("error").expect("error");
        assert_eq!(error.get("code").and_then(Value::as_i64), Some(-32601));
    }

    #[test]
    fn handle_request_routes_correctly() {
        let init_req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        });
        let response = handle_request(&init_req);
        assert!(response.get("result").is_some());

        let unknown_req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "unknown/method"
        });
        let response = handle_request(&unknown_req);
        assert!(response.get("error").is_some());
    }
}
