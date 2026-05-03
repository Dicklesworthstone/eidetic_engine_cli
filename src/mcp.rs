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

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};

use serde_json::{Value, json};

use crate::core::agent_docs::AgentDocsTopic;
use crate::models::{ContextProfileName, ProcessExitCode};
pub use crate::output::MCP_PROTOCOL_VERSION;
use crate::output::public_schemas;

pub const SUBSYSTEM: &str = "mcp";
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
    PromptsList,
    PromptsGet,
    ResourcesList,
    ResourcesRead,
    ResourcesTemplatesList,
    ToolsList,
    ToolsCall,
    NotificationsCancelled,
    Shutdown,
    Unknown(String),
}

impl McpMethod {
    #[must_use]
    pub fn parse(method: &str) -> Self {
        match method {
            "initialize" => Self::Initialize,
            "prompts/list" => Self::PromptsList,
            "prompts/get" => Self::PromptsGet,
            "resources/list" => Self::ResourcesList,
            "resources/read" => Self::ResourcesRead,
            "resources/templates/list" => Self::ResourcesTemplatesList,
            "tools/list" => Self::ToolsList,
            "tools/call" => Self::ToolsCall,
            "notifications/cancelled" => Self::NotificationsCancelled,
            "shutdown" => Self::Shutdown,
            other => Self::Unknown(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpPrompt {
    PreTaskContext,
    RecordLesson,
    ReviewSession,
}

impl McpPrompt {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "pre-task-context" => Some(Self::PreTaskContext),
            "record-lesson" => Some(Self::RecordLesson),
            "review-session" => Some(Self::ReviewSession),
            _ => None,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::PreTaskContext => "pre-task-context",
            Self::RecordLesson => "record-lesson",
            Self::ReviewSession => "review-session",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::PreTaskContext => {
                "Prepare an agent before a task by retrieving an ee context pack."
            }
            Self::RecordLesson => "Turn a durable lesson into an explicit ee remember workflow.",
            Self::ReviewSession => "Review a prior session and propose curation candidates.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpTool {
    Health,
    Status,
    Capabilities,
    Search,
    Context,
    MemoryShow,
    Why,
    Remember,
    Outcome,
}

impl McpTool {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "ee_health" => Some(Self::Health),
            "ee_status" => Some(Self::Status),
            "ee_capabilities" => Some(Self::Capabilities),
            "ee_search" => Some(Self::Search),
            "ee_context" => Some(Self::Context),
            "ee_memory_show" => Some(Self::MemoryShow),
            "ee_why" => Some(Self::Why),
            "ee_remember" => Some(Self::Remember),
            "ee_outcome" => Some(Self::Outcome),
            _ => None,
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
    json_rpc_result(
        id,
        json!({
            "protocolVersion": info.protocol_version,
            "serverInfo": {
                "name": info.name,
                "version": info.version
            },
            "capabilities": {
                "prompts": {},
                "resources": {},
                "tools": {}
            }
        }),
    )
}

fn prompt_argument(name: &str, description: &str, required: bool) -> Value {
    json!({
        "name": name,
        "description": description,
        "required": required
    })
}

fn prompt_descriptor(prompt: McpPrompt) -> Value {
    let arguments = match prompt {
        McpPrompt::PreTaskContext => vec![
            prompt_argument("task", "Task or question to retrieve context for", true),
            prompt_argument(
                "workspace",
                "Workspace path; defaults to current directory",
                false,
            ),
            prompt_argument(
                "profile",
                "Context profile such as balanced or compact",
                false,
            ),
            prompt_argument("maxTokens", "Maximum context token budget", false),
        ],
        McpPrompt::RecordLesson => vec![
            prompt_argument(
                "lesson",
                "Durable rule, fact, failure, or convention to remember",
                true,
            ),
            prompt_argument(
                "workspace",
                "Workspace path; defaults to current directory",
                false,
            ),
            prompt_argument("level", "Memory level; defaults to procedural", false),
            prompt_argument("kind", "Memory kind; defaults to rule", false),
            prompt_argument("tags", "Comma-separated tags to apply", false),
        ],
        McpPrompt::ReviewSession => vec![
            prompt_argument(
                "session",
                "CASS session path or ID; defaults to most recent",
                false,
            ),
            prompt_argument(
                "workspace",
                "Workspace path; defaults to current directory",
                false,
            ),
            prompt_argument("propose", "Whether to ask for curation proposals", false),
        ],
    };
    json!({
        "name": prompt.name(),
        "description": prompt.description(),
        "arguments": arguments
    })
}

fn handle_prompts_list(id: Value) -> Value {
    json_rpc_result(
        id,
        json!({
            "prompts": [
                prompt_descriptor(McpPrompt::PreTaskContext),
                prompt_descriptor(McpPrompt::RecordLesson),
                prompt_descriptor(McpPrompt::ReviewSession)
            ]
        }),
    )
}

fn prompt_arguments<'a>(params: &'a Value, id: &Value) -> Result<&'a Value, Value> {
    let arguments = params.get("arguments").unwrap_or(&Value::Null);
    if !arguments.is_null() && !arguments.is_object() {
        return Err(json_rpc_error(
            Some(id.clone()),
            -32602,
            "Prompt arguments must be an object",
        ));
    }
    Ok(arguments)
}

fn prompt_optional_string<'a>(
    arguments: &'a Value,
    names: &[&str],
) -> Result<Option<&'a str>, String> {
    if arguments.is_null() {
        return Ok(None);
    }
    optional_string(arguments, names)
}

fn prompt_required_string<'a>(arguments: &'a Value, names: &[&str]) -> Result<&'a str, String> {
    if arguments.is_null() {
        return Err(format!("Missing required prompt argument '{}'", names[0]));
    }
    required_string(arguments, names)
}

fn prompt_optional_bool(arguments: &Value, names: &[&str]) -> Result<bool, String> {
    if arguments.is_null() {
        return Ok(false);
    }
    optional_bool(arguments, names)
}

fn parse_mcp_context_profile(value: &str) -> Result<&'static str, String> {
    ContextProfileName::parse(value)
        .map(ContextProfileName::as_str)
        .ok_or_else(|| {
            format!(
                "Invalid context profile '{value}'. Expected compact, balanced, thorough, or submodular."
            )
        })
}

fn render_pre_task_context_prompt(arguments: &Value) -> Result<String, String> {
    let task = prompt_required_string(arguments, &["task"])?;
    let workspace = prompt_optional_string(arguments, &["workspace"])?.unwrap_or(".");
    let profile = prompt_optional_string(arguments, &["profile"])?
        .map(parse_mcp_context_profile)
        .transpose()?
        .unwrap_or("balanced");
    let max_tokens = optional_u32(arguments, &["maxTokens", "max_tokens"])?
        .map_or_else(|| "4000".to_string(), |value| value.to_string());

    Ok(format!(
        "Prepare for this task with ee before editing.\n\nTask:\n{task}\n\nUse the read-only MCP tool `ee_context` or the CLI command below:\n`ee --workspace {workspace} --json context {task:?} --profile {profile} --max-tokens {max_tokens}`\n\nRead the returned `ee.response.v1` envelope, summarize the highest-confidence procedural rules, relevant failures, decisions, provenance, and degraded capabilities, then proceed with the task. Keep machine JSON separate from human diagnostics."
    ))
}

fn render_record_lesson_prompt(arguments: &Value) -> Result<String, String> {
    let lesson = prompt_required_string(arguments, &["lesson"])?;
    let workspace = prompt_optional_string(arguments, &["workspace"])?.unwrap_or(".");
    let level = prompt_optional_string(arguments, &["level"])?.unwrap_or("procedural");
    let kind = prompt_optional_string(arguments, &["kind"])?.unwrap_or("rule");
    let tags = prompt_optional_string(arguments, &["tags"])?.unwrap_or("");

    let tag_instruction = if tags.is_empty() {
        "No tags were provided; add only tags that are directly supported by the evidence."
            .to_string()
    } else {
        format!("Apply these comma-separated tags if they are supported by the evidence: {tags}.")
    };

    Ok(format!(
        "Record a durable ee lesson only if it is supported by evidence from the current work.\n\nLesson:\n{lesson}\n\nRecommended command:\n`ee --workspace {workspace} --json remember --level {level} --kind {kind} {lesson:?}`\n\n{tag_instruction}\nDo not invent provenance. If the lesson came from a failure or decision, include the source session, command, file, or bead in the memory text before recording it."
    ))
}

fn render_review_session_prompt(arguments: &Value) -> Result<String, String> {
    let session = prompt_optional_string(arguments, &["session"])?.unwrap_or("<latest-session>");
    let workspace = prompt_optional_string(arguments, &["workspace"])?.unwrap_or(".");
    let propose = prompt_optional_bool(arguments, &["propose"])?;
    let propose_flag = if propose { " --propose" } else { "" };
    let session_arg = if session == "<latest-session>" {
        String::new()
    } else {
        format!(" {session:?}")
    };

    Ok(format!(
        "Review a prior coding-agent session through ee and convert only evidence-backed lessons into candidates.\n\nSession:\n{session}\n\nRecommended command:\n`ee --workspace {workspace} --json review session{session_arg}{propose_flag}`\n\nInspect the JSON envelope for proposed memories, evidence spans, confidence, degraded capabilities, and next actions. Do not apply or promote curation candidates unless the follow-up command is explicit and auditable."
    ))
}

fn build_prompt_text(prompt: McpPrompt, arguments: &Value) -> Result<String, String> {
    match prompt {
        McpPrompt::PreTaskContext => render_pre_task_context_prompt(arguments),
        McpPrompt::RecordLesson => render_record_lesson_prompt(arguments),
        McpPrompt::ReviewSession => render_review_session_prompt(arguments),
    }
}

fn handle_prompts_get(id: Value, params: Option<&Value>) -> Value {
    let Some(params) = params else {
        return json_rpc_error(Some(id), -32602, "Missing params");
    };
    let prompt_name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let Some(prompt) = McpPrompt::parse(prompt_name) else {
        return json_rpc_error(Some(id), -32602, &format!("Unknown prompt: {prompt_name}"));
    };
    let arguments = match prompt_arguments(params, &id) {
        Ok(arguments) => arguments,
        Err(error) => return error,
    };
    let text = match build_prompt_text(prompt, arguments) {
        Ok(text) => text,
        Err(message) => return json_rpc_error(Some(id), -32602, &message),
    };

    json_rpc_result(
        id,
        json!({
            "description": prompt.description(),
            "messages": [{
                "role": "user",
                "content": {
                    "type": "text",
                    "text": text
                }
            }]
        }),
    )
}

fn mcp_resource(uri: String, name: String, description: String) -> Value {
    json!({
        "uri": uri,
        "name": name,
        "description": description,
        "mimeType": "application/json"
    })
}

fn mcp_resource_template(uri_template: &str, name: &str, description: &str) -> Value {
    json!({
        "uriTemplate": uri_template,
        "name": name,
        "description": description,
        "mimeType": "application/json"
    })
}

fn handle_resources_list(id: Value) -> Value {
    let mut resources = vec![
        mcp_resource(
            "ee://agent-docs".to_string(),
            "ee agent docs".to_string(),
            "Agent-oriented overview of ee commands, contracts, and workflows".to_string(),
        ),
        mcp_resource(
            "ee://schemas".to_string(),
            "ee schema registry".to_string(),
            "List of public ee JSON schemas".to_string(),
        ),
        mcp_resource(
            "ee://workspace/status".to_string(),
            "ee workspace status".to_string(),
            "Current workspace and subsystem readiness from ee status --json".to_string(),
        ),
    ];

    for topic in AgentDocsTopic::all() {
        resources.push(mcp_resource(
            format!("ee://agent-docs/{}", topic.as_str()),
            format!("ee agent docs {}", topic.as_str()),
            topic.description().to_string(),
        ));
    }

    for schema in public_schemas() {
        resources.push(mcp_resource(
            format!("ee://schemas/{}", schema.id),
            format!("ee schema {}", schema.id),
            schema.description.to_string(),
        ));
    }

    json_rpc_result(id, json!({ "resources": resources }))
}

fn handle_resources_templates_list(id: Value) -> Value {
    json_rpc_result(
        id,
        json!({
            "resourceTemplates": [
                mcp_resource_template(
                    "ee://memories/{memoryId}",
                    "ee memory show",
                    "Read a memory record through ee memory show --json"
                ),
                mcp_resource_template(
                    "ee://context-packs/by-query?query={query}",
                    "ee context pack",
                    "Assemble a task-specific context pack through ee context --json"
                ),
                mcp_resource_template(
                    "ee://schemas/{schemaId}",
                    "ee schema export",
                    "Read a public schema definition through ee schema export --json"
                ),
                mcp_resource_template(
                    "ee://agent-docs/{topic}",
                    "ee agent docs topic",
                    "Read an agent docs topic through ee agent-docs --json"
                )
            ]
        }),
    )
}

fn handle_tools_list(id: Value) -> Value {
    json_rpc_result(
        id,
        json!({
            "tools": [
                {
                    "name": "ee_health",
                    "description": "Run ee health --json and return the response envelope",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "workspace": {
                                "type": "string",
                                "description": "Workspace path (defaults to current directory)"
                            }
                        }
                    },
                    "annotations": {
                        "readOnlyHint": true,
                        "destructiveHint": false,
                        "idempotentHint": true,
                        "openWorldHint": false
                    }
                },
                {
                    "name": "ee_status",
                    "description": "Run ee status --json and return workspace readiness",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "workspace": {
                                "type": "string",
                                "description": "Workspace path (defaults to current directory)"
                            }
                        }
                    },
                    "annotations": {
                        "readOnlyHint": true,
                        "destructiveHint": false,
                        "idempotentHint": true,
                        "openWorldHint": false
                    }
                },
                {
                    "name": "ee_capabilities",
                    "description": "Run ee capabilities --json and return feature availability",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "workspace": {
                                "type": "string",
                                "description": "Workspace path (defaults to current directory)"
                            }
                        }
                    },
                    "annotations": {
                        "readOnlyHint": true,
                        "destructiveHint": false,
                        "idempotentHint": true,
                        "openWorldHint": false
                    }
                },
                {
                    "name": "ee_search",
                    "description": "Run ee search --json over indexed memories and sessions",
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
                            },
                            "database": {
                                "type": "string",
                                "description": "Database path override"
                            },
                            "indexDir": {
                                "type": "string",
                                "description": "Index directory override"
                            },
                            "explain": {
                                "type": "boolean",
                                "description": "Include score explanations"
                            }
                        },
                        "required": ["query"]
                    },
                    "annotations": {
                        "readOnlyHint": true,
                        "destructiveHint": false,
                        "idempotentHint": true,
                        "openWorldHint": false
                    }
                },
                {
                    "name": "ee_context",
                    "description": "Run ee context --json to assemble task-specific context",
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
                            },
                            "candidatePool": {
                                "type": "integer",
                                "description": "Maximum candidate memories before packing"
                            },
                            "profile": {
                                "type": "string",
                                "description": "Context profile",
                                "enum": ["compact", "balanced", "thorough", "submodular"]
                            },
                            "database": {
                                "type": "string",
                                "description": "Database path override"
                            },
                            "indexDir": {
                                "type": "string",
                                "description": "Index directory override"
                            }
                        },
                        "required": ["query"]
                    },
                    "annotations": {
                        "readOnlyHint": true,
                        "destructiveHint": false,
                        "idempotentHint": true,
                        "openWorldHint": false
                    }
                },
                {
                    "name": "ee_memory_show",
                    "description": "Run ee memory show --json for a single memory",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "memoryId": {
                                "type": "string",
                                "description": "Memory ID to show"
                            },
                            "workspace": {
                                "type": "string",
                                "description": "Workspace path"
                            },
                            "database": {
                                "type": "string",
                                "description": "Database path override"
                            },
                            "includeTombstoned": {
                                "type": "boolean",
                                "description": "Include tombstoned memories"
                            }
                        },
                        "required": ["memoryId"]
                    },
                    "annotations": {
                        "readOnlyHint": true,
                        "destructiveHint": false,
                        "idempotentHint": true,
                        "openWorldHint": false
                    }
                },
                {
                    "name": "ee_why",
                    "description": "Run ee why --json to explain memory storage, retrieval, or selection",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "memoryId": {
                                "type": "string",
                                "description": "Memory ID to explain"
                            },
                            "workspace": {
                                "type": "string",
                                "description": "Workspace path"
                            },
                            "database": {
                                "type": "string",
                                "description": "Database path override"
                            },
                            "confidenceThreshold": {
                                "type": "number",
                                "description": "Selection confidence threshold"
                            }
                        },
                        "required": ["memoryId"]
                    },
                    "annotations": {
                        "readOnlyHint": true,
                        "destructiveHint": false,
                        "idempotentHint": true,
                        "openWorldHint": false
                    }
                },
                {
                    "name": "ee_remember",
                    "description": "Gated write tool for ee remember --json; defaults to dry-run and requires allowWrite=true for durable writes",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "content": {
                                "type": "string",
                                "description": "Memory content to store"
                            },
                            "workspace": {
                                "type": "string",
                                "description": "Workspace path"
                            },
                            "level": {
                                "type": "string",
                                "description": "Memory level; defaults to episodic"
                            },
                            "kind": {
                                "type": "string",
                                "description": "Memory kind; defaults to fact"
                            },
                            "tags": {
                                "type": "string",
                                "description": "Comma-separated tags"
                            },
                            "confidence": {
                                "type": "number",
                                "description": "Confidence score from 0.0 to 1.0"
                            },
                            "source": {
                                "type": "string",
                                "description": "Source provenance URI"
                            },
                            "validFrom": {
                                "type": "string",
                                "description": "RFC3339 timestamp when this memory becomes applicable"
                            },
                            "validTo": {
                                "type": "string",
                                "description": "RFC3339 timestamp when this memory stops being applicable"
                            },
                            "dryRun": {
                                "type": "boolean",
                                "description": "Validate without writing; defaults to true"
                            },
                            "allowWrite": {
                                "type": "boolean",
                                "description": "Required and true when dryRun is false"
                            }
                        },
                        "required": ["content"]
                    },
                    "annotations": {
                        "readOnlyHint": false,
                        "destructiveHint": false,
                        "idempotentHint": false,
                        "openWorldHint": false
                    },
                    "eeEffect": {
                        "kind": "durable_write",
                        "writeSurface": ["memories", "audit_log", "search_index_jobs"],
                        "defaultDryRun": true,
                        "requiresAllowWriteWhenDryRunFalse": true,
                        "audit": "ee remember writes audit_id through the CLI core path",
                        "redaction": "content is checked by remember policy before storage",
                        "idempotency": "not_idempotent_for_durable_writes",
                        "destructive": false
                    }
                },
                {
                    "name": "ee_outcome",
                    "description": "Gated write tool for ee outcome --json; defaults to dry-run and requires allowWrite=true for durable writes",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "targetId": {
                                "type": "string",
                                "description": "Target ID to receive feedback"
                            },
                            "targetType": {
                                "type": "string",
                                "description": "Target type; defaults to memory"
                            },
                            "workspace": {
                                "type": "string",
                                "description": "Workspace path"
                            },
                            "workspaceId": {
                                "type": "string",
                                "description": "Workspace ID for non-memory targets"
                            },
                            "signal": {
                                "type": "string",
                                "description": "Outcome signal such as helpful, harmful, stale, or neutral"
                            },
                            "weight": {
                                "type": "number",
                                "description": "Optional feedback weight from 0.0 to 10.0"
                            },
                            "sourceType": {
                                "type": "string",
                                "description": "Feedback source type"
                            },
                            "sourceId": {
                                "type": "string",
                                "description": "Source identifier, such as a run or task ID"
                            },
                            "reason": {
                                "type": "string",
                                "description": "Human-readable reason for the feedback"
                            },
                            "evidenceJson": {
                                "type": "string",
                                "description": "JSON evidence payload string"
                            },
                            "sessionId": {
                                "type": "string",
                                "description": "Session ID associated with the observed outcome"
                            },
                            "eventId": {
                                "type": "string",
                                "description": "Caller-supplied feedback event ID for idempotent retries"
                            },
                            "actor": {
                                "type": "string",
                                "description": "Actor recorded in the audit log"
                            },
                            "database": {
                                "type": "string",
                                "description": "Database path override"
                            },
                            "dryRun": {
                                "type": "boolean",
                                "description": "Validate without writing; defaults to true"
                            },
                            "allowWrite": {
                                "type": "boolean",
                                "description": "Required and true when dryRun is false"
                            }
                        },
                        "required": ["targetId", "signal"]
                    },
                    "annotations": {
                        "readOnlyHint": false,
                        "destructiveHint": false,
                        "idempotentHint": false,
                        "openWorldHint": false
                    },
                    "eeEffect": {
                        "kind": "durable_write",
                        "writeSurface": ["feedback_events", "audit_log"],
                        "defaultDryRun": true,
                        "requiresAllowWriteWhenDryRunFalse": true,
                        "audit": "ee outcome writes audit_id through the CLI core path",
                        "redaction": "evidenceJson is validated and stored only through the core outcome path",
                        "idempotency": "eventId enables idempotent retries when content matches",
                        "destructive": false
                    }
                }
            ]
        }),
    )
}

fn find_argument<'a>(arguments: &'a Value, names: &[&str]) -> Option<&'a Value> {
    names.iter().find_map(|name| arguments.get(*name))
}

fn required_string<'a>(arguments: &'a Value, names: &[&str]) -> Result<&'a str, String> {
    let Some(value) = find_argument(arguments, names) else {
        return Err(format!("Missing required argument '{}'", names[0]));
    };
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Argument '{}' must be a non-empty string", names[0]))
}

fn optional_string<'a>(arguments: &'a Value, names: &[&str]) -> Result<Option<&'a str>, String> {
    let Some(value) = find_argument(arguments, names) else {
        return Ok(None);
    };
    value
        .as_str()
        .map(Some)
        .ok_or_else(|| format!("Argument '{}' must be a string", names[0]))
}

fn optional_bool(arguments: &Value, names: &[&str]) -> Result<bool, String> {
    let Some(value) = find_argument(arguments, names) else {
        return Ok(false);
    };
    value
        .as_bool()
        .ok_or_else(|| format!("Argument '{}' must be a boolean", names[0]))
}

fn optional_bool_with_default(
    arguments: &Value,
    names: &[&str],
    default: bool,
) -> Result<bool, String> {
    let Some(value) = find_argument(arguments, names) else {
        return Ok(default);
    };
    value
        .as_bool()
        .ok_or_else(|| format!("Argument '{}' must be a boolean", names[0]))
}

fn optional_u32(arguments: &Value, names: &[&str]) -> Result<Option<u32>, String> {
    let Some(value) = find_argument(arguments, names) else {
        return Ok(None);
    };
    let Some(raw) = value.as_u64() else {
        return Err(format!(
            "Argument '{}' must be a non-negative integer",
            names[0]
        ));
    };
    u32::try_from(raw)
        .map(Some)
        .map_err(|_| format!("Argument '{}' is too large", names[0]))
}

fn optional_number_string(arguments: &Value, names: &[&str]) -> Result<Option<String>, String> {
    let Some(value) = find_argument(arguments, names) else {
        return Ok(None);
    };
    let Some(raw) = value.as_f64() else {
        return Err(format!("Argument '{}' must be a number", names[0]));
    };
    if !raw.is_finite() {
        return Err(format!("Argument '{}' must be finite", names[0]));
    }
    Ok(Some(raw.to_string()))
}

fn push_arg(args: &mut Vec<OsString>, value: impl Into<OsString>) {
    args.push(value.into());
}

fn append_common_json_args(args: &mut Vec<OsString>, arguments: &Value) -> Result<(), String> {
    push_arg(args, "ee");
    push_arg(args, "--json");
    if let Some(workspace) = optional_string(arguments, &["workspace"])? {
        push_arg(args, "--workspace");
        push_arg(args, workspace);
    }
    Ok(())
}

fn append_optional_path_flag(
    args: &mut Vec<OsString>,
    arguments: &Value,
    names: &[&str],
    flag: &str,
) -> Result<(), String> {
    if let Some(value) = optional_string(arguments, names)? {
        push_arg(args, flag);
        push_arg(args, value);
    }
    Ok(())
}

fn append_optional_string_flag(
    args: &mut Vec<OsString>,
    arguments: &Value,
    names: &[&str],
    flag: &str,
) -> Result<(), String> {
    if let Some(value) = optional_string(arguments, names)? {
        push_arg(args, flag);
        push_arg(args, value);
    }
    Ok(())
}

fn append_optional_number_flag(
    args: &mut Vec<OsString>,
    arguments: &Value,
    names: &[&str],
    flag: &str,
) -> Result<(), String> {
    if let Some(value) = optional_number_string(arguments, names)? {
        push_arg(args, flag);
        push_arg(args, value);
    }
    Ok(())
}

fn gated_write_dry_run(tool_name: &str, arguments: &Value) -> Result<bool, String> {
    let dry_run = optional_bool_with_default(arguments, &["dryRun", "dry_run"], true)?;
    let allow_write = optional_bool(arguments, &["allowWrite", "allow_write"])?;
    if !dry_run && !allow_write {
        return Err(format!(
            "Write tool {tool_name} requires allowWrite=true when dryRun=false"
        ));
    }
    Ok(dry_run)
}

fn build_cli_args_for_tool(tool: McpTool, arguments: &Value) -> Result<Vec<OsString>, String> {
    let mut args = Vec::new();
    append_common_json_args(&mut args, arguments)?;

    match tool {
        McpTool::Health => push_arg(&mut args, "health"),
        McpTool::Status => push_arg(&mut args, "status"),
        McpTool::Capabilities => push_arg(&mut args, "capabilities"),
        McpTool::Search => {
            push_arg(&mut args, "search");
            push_arg(&mut args, required_string(arguments, &["query"])?);
            if let Some(limit) = optional_u32(arguments, &["limit"])? {
                push_arg(&mut args, "--limit");
                push_arg(&mut args, limit.to_string());
            }
            append_optional_path_flag(&mut args, arguments, &["database"], "--database")?;
            append_optional_path_flag(
                &mut args,
                arguments,
                &["indexDir", "index_dir"],
                "--index-dir",
            )?;
            if optional_bool(arguments, &["explain"])? {
                push_arg(&mut args, "--explain");
            }
        }
        McpTool::Context => {
            push_arg(&mut args, "context");
            push_arg(&mut args, required_string(arguments, &["query"])?);
            if let Some(max_tokens) = optional_u32(arguments, &["maxTokens", "max_tokens"])? {
                push_arg(&mut args, "--max-tokens");
                push_arg(&mut args, max_tokens.to_string());
            }
            if let Some(candidate_pool) =
                optional_u32(arguments, &["candidatePool", "candidate_pool"])?
            {
                push_arg(&mut args, "--candidate-pool");
                push_arg(&mut args, candidate_pool.to_string());
            }
            if let Some(profile) = optional_string(arguments, &["profile"])? {
                push_arg(&mut args, "--profile");
                push_arg(&mut args, parse_mcp_context_profile(profile)?);
            }
            append_optional_path_flag(&mut args, arguments, &["database"], "--database")?;
            append_optional_path_flag(
                &mut args,
                arguments,
                &["indexDir", "index_dir"],
                "--index-dir",
            )?;
        }
        McpTool::MemoryShow => {
            push_arg(&mut args, "memory");
            push_arg(&mut args, "show");
            push_arg(
                &mut args,
                required_string(arguments, &["memoryId", "memory_id"])?,
            );
            append_optional_path_flag(&mut args, arguments, &["database"], "--database")?;
            if optional_bool(arguments, &["includeTombstoned", "include_tombstoned"])? {
                push_arg(&mut args, "--include-tombstoned");
            }
        }
        McpTool::Why => {
            push_arg(&mut args, "why");
            push_arg(
                &mut args,
                required_string(arguments, &["memoryId", "memory_id"])?,
            );
            append_optional_path_flag(&mut args, arguments, &["database"], "--database")?;
            if let Some(threshold) =
                optional_number_string(arguments, &["confidenceThreshold", "confidence_threshold"])?
            {
                push_arg(&mut args, "--confidence-threshold");
                push_arg(&mut args, threshold);
            }
        }
        McpTool::Remember => {
            let dry_run = gated_write_dry_run("ee_remember", arguments)?;
            push_arg(&mut args, "remember");
            push_arg(&mut args, required_string(arguments, &["content"])?);
            append_optional_string_flag(&mut args, arguments, &["level"], "--level")?;
            append_optional_string_flag(&mut args, arguments, &["kind"], "--kind")?;
            append_optional_string_flag(&mut args, arguments, &["tags"], "--tags")?;
            append_optional_number_flag(&mut args, arguments, &["confidence"], "--confidence")?;
            append_optional_string_flag(&mut args, arguments, &["source"], "--source")?;
            append_optional_string_flag(
                &mut args,
                arguments,
                &["validFrom", "valid_from"],
                "--valid-from",
            )?;
            append_optional_string_flag(
                &mut args,
                arguments,
                &["validTo", "valid_to"],
                "--valid-to",
            )?;
            if dry_run {
                push_arg(&mut args, "--dry-run");
            }
        }
        McpTool::Outcome => {
            let dry_run = gated_write_dry_run("ee_outcome", arguments)?;
            push_arg(&mut args, "outcome");
            push_arg(
                &mut args,
                required_string(arguments, &["targetId", "target_id"])?,
            );
            append_optional_string_flag(
                &mut args,
                arguments,
                &["targetType", "target_type"],
                "--target-type",
            )?;
            append_optional_string_flag(
                &mut args,
                arguments,
                &["workspaceId", "workspace_id"],
                "--workspace-id",
            )?;
            push_arg(&mut args, "--signal");
            push_arg(&mut args, required_string(arguments, &["signal"])?);
            append_optional_number_flag(&mut args, arguments, &["weight"], "--weight")?;
            append_optional_string_flag(
                &mut args,
                arguments,
                &["sourceType", "source_type"],
                "--source-type",
            )?;
            append_optional_string_flag(
                &mut args,
                arguments,
                &["sourceId", "source_id"],
                "--source-id",
            )?;
            append_optional_string_flag(&mut args, arguments, &["reason"], "--reason")?;
            append_optional_string_flag(
                &mut args,
                arguments,
                &["evidenceJson", "evidence_json"],
                "--evidence-json",
            )?;
            append_optional_string_flag(
                &mut args,
                arguments,
                &["sessionId", "session_id"],
                "--session-id",
            )?;
            append_optional_string_flag(
                &mut args,
                arguments,
                &["eventId", "event_id"],
                "--event-id",
            )?;
            append_optional_string_flag(&mut args, arguments, &["actor"], "--actor")?;
            append_optional_path_flag(&mut args, arguments, &["database"], "--database")?;
            if dry_run {
                push_arg(&mut args, "--dry-run");
            }
        }
    }

    Ok(args)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn percent_decode(raw: &str) -> Result<String, String> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        let Some(&byte) = bytes.get(index) else {
            break;
        };
        match byte {
            b'%' => {
                let Some(&high_byte) = bytes.get(index + 1) else {
                    return Err(format!("Invalid percent escape in resource URI: {raw}"));
                };
                let Some(&low_byte) = bytes.get(index + 2) else {
                    return Err(format!("Invalid percent escape in resource URI: {raw}"));
                };
                let Some(high) = hex_value(high_byte) else {
                    return Err(format!("Invalid percent escape in resource URI: {raw}"));
                };
                let Some(low) = hex_value(low_byte) else {
                    return Err(format!("Invalid percent escape in resource URI: {raw}"));
                };
                decoded.push((high << 4) | low);
                index += 3;
            }
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }

    String::from_utf8(decoded)
        .map_err(|error| format!("Resource URI is not valid UTF-8 after decoding: {error}"))
}

type ResourceQueryParams = Vec<(String, String)>;
type ParsedResourceUri<'a> = (&'a str, ResourceQueryParams);

fn parse_resource_uri(uri: &str) -> Result<ParsedResourceUri<'_>, String> {
    let Some(rest) = uri.strip_prefix("ee://") else {
        return Err(format!("Unsupported resource URI '{uri}'; expected ee://"));
    };
    let (path, raw_query) = match rest.split_once('?') {
        Some((path, raw_query)) => (path, Some(raw_query)),
        None => (rest, None),
    };
    if path.is_empty() {
        return Err("Resource URI path must not be empty".to_string());
    }

    let mut query = Vec::new();
    if let Some(raw_query) = raw_query {
        for pair in raw_query.split('&').filter(|pair| !pair.is_empty()) {
            let (raw_key, raw_value) = match pair.split_once('=') {
                Some((key, value)) => (key, value),
                None => (pair, ""),
            };
            query.push((percent_decode(raw_key)?, percent_decode(raw_value)?));
        }
    }

    Ok((path, query))
}

fn query_param<'a>(query: &'a [(String, String)], names: &[&str]) -> Option<&'a str> {
    query
        .iter()
        .find(|(key, _)| names.iter().any(|name| key == name))
        .map(|(_, value)| value.as_str())
}

fn required_query_param(query: &[(String, String)], names: &[&str]) -> Result<String, String> {
    let Some(value) = query_param(query, names) else {
        return Err(format!(
            "Missing required URI query parameter '{}'",
            names[0]
        ));
    };
    if value.is_empty() {
        return Err(format!(
            "URI query parameter '{}' must not be empty",
            names[0]
        ));
    }
    Ok(value.to_string())
}

fn path_tail(path: &str, prefix: &str, label: &str) -> Result<String, String> {
    let Some(raw_tail) = path.strip_prefix(prefix) else {
        return Err(format!("Resource URI does not start with {prefix}"));
    };
    if raw_tail.is_empty() {
        return Err(format!("Resource URI missing {label}"));
    }
    percent_decode(raw_tail)
}

fn append_resource_common_json_args(args: &mut Vec<OsString>, query: &[(String, String)]) {
    push_arg(args, "ee");
    push_arg(args, "--json");
    if let Some(workspace) = query_param(query, &["workspace"]) {
        push_arg(args, "--workspace");
        push_arg(args, workspace);
    }
}

fn append_resource_query_flag(
    args: &mut Vec<OsString>,
    query: &[(String, String)],
    names: &[&str],
    flag: &str,
) {
    if let Some(value) = query_param(query, names) {
        push_arg(args, flag);
        push_arg(args, value);
    }
}

fn append_resource_bool_flag(
    args: &mut Vec<OsString>,
    query: &[(String, String)],
    names: &[&str],
    flag: &str,
) -> Result<(), String> {
    let Some(value) = query_param(query, names) else {
        return Ok(());
    };
    match value {
        "true" => {
            push_arg(args, flag);
            Ok(())
        }
        "false" => Ok(()),
        _ => Err(format!(
            "URI query parameter '{}' must be true or false",
            names[0]
        )),
    }
}

fn build_cli_args_for_resource(uri: &str) -> Result<Vec<OsString>, String> {
    let (path, query) = parse_resource_uri(uri)?;
    let mut args = Vec::new();
    append_resource_common_json_args(&mut args, &query);

    match path {
        "agent-docs" => push_arg(&mut args, "agent-docs"),
        "schemas" => {
            push_arg(&mut args, "schema");
            push_arg(&mut args, "list");
        }
        "workspace/status" => push_arg(&mut args, "status"),
        "context-packs/by-query" => {
            push_arg(&mut args, "context");
            push_arg(&mut args, required_query_param(&query, &["query"])?);
            append_resource_query_flag(
                &mut args,
                &query,
                &["maxTokens", "max_tokens"],
                "--max-tokens",
            );
            append_resource_query_flag(
                &mut args,
                &query,
                &["candidatePool", "candidate_pool"],
                "--candidate-pool",
            );
            append_resource_query_flag(&mut args, &query, &["profile"], "--profile");
            append_resource_query_flag(&mut args, &query, &["database"], "--database");
            append_resource_query_flag(
                &mut args,
                &query,
                &["indexDir", "index_dir"],
                "--index-dir",
            );
        }
        _ if path.starts_with("agent-docs/") => {
            push_arg(&mut args, "agent-docs");
            push_arg(
                &mut args,
                path_tail(path, "agent-docs/", "agent docs topic")?,
            );
        }
        _ if path.starts_with("schemas/") => {
            push_arg(&mut args, "schema");
            push_arg(&mut args, "export");
            push_arg(&mut args, path_tail(path, "schemas/", "schema ID")?);
        }
        _ if path.starts_with("memories/") => {
            push_arg(&mut args, "memory");
            push_arg(&mut args, "show");
            push_arg(&mut args, path_tail(path, "memories/", "memory ID")?);
            append_resource_query_flag(&mut args, &query, &["database"], "--database");
            append_resource_bool_flag(
                &mut args,
                &query,
                &["includeTombstoned", "include_tombstoned"],
                "--include-tombstoned",
            )?;
        }
        _ => return Err(format!("Unknown ee resource URI: {uri}")),
    }

    Ok(args)
}

fn run_cli_tool(args: Vec<OsString>) -> (ProcessExitCode, String, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = crate::cli::run(args, &mut stdout, &mut stderr);
    (
        exit,
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    )
}

fn resource_read_result(
    id: Value,
    uri: &str,
    exit: ProcessExitCode,
    stdout: String,
    stderr: String,
) -> Value {
    let text = if stdout.is_empty() { &stderr } else { &stdout };
    json_rpc_result(
        id,
        json!({
            "contents": [{
                "uri": uri,
                "mimeType": "application/json",
                "text": text
            }],
            "isError": exit != ProcessExitCode::Success,
            "exitCode": exit as u8,
            "stderr": stderr
        }),
    )
}

fn handle_resources_read(id: Value, params: Option<&Value>) -> Value {
    let Some(params) = params else {
        return json_rpc_error(Some(id), -32602, "Missing params");
    };
    let Some(uri) = params.get("uri").and_then(Value::as_str) else {
        return json_rpc_error(Some(id), -32602, "resources/read requires uri");
    };

    let cli_args = match build_cli_args_for_resource(uri) {
        Ok(args) => args,
        Err(message) => return json_rpc_error(Some(id), -32602, &message),
    };
    let (exit, stdout, stderr) = run_cli_tool(cli_args);
    resource_read_result(id, uri, exit, stdout, stderr)
}

fn cli_tool_result(id: Value, exit: ProcessExitCode, stdout: String, stderr: String) -> Value {
    let text = if stdout.is_empty() { &stderr } else { &stdout };
    json_rpc_result(
        id,
        json!({
            "content": [{
                "type": "text",
                "text": text
            }],
            "isError": exit != ProcessExitCode::Success,
            "exitCode": exit as u8,
            "stderr": stderr
        }),
    )
}

fn handle_tools_call(id: Value, params: Option<&Value>) -> Value {
    let Some(params) = params else {
        return json_rpc_error(Some(id), -32602, "Missing params");
    };

    let tool_name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let Some(tool) = McpTool::parse(tool_name) else {
        return json_rpc_error(Some(id), -32601, &format!("Unknown tool: {tool_name}"));
    };

    let arguments = params.get("arguments").unwrap_or(&Value::Null);
    if !arguments.is_null() && !arguments.is_object() {
        return json_rpc_error(Some(id), -32602, "Tool arguments must be an object");
    }
    let empty_arguments = json!({});
    let arguments = if arguments.is_null() {
        &empty_arguments
    } else {
        arguments
    };

    let cli_args = match build_cli_args_for_tool(tool, arguments) {
        Ok(args) => args,
        Err(message) => return json_rpc_error(Some(id), -32602, &message),
    };
    let (exit, stdout, stderr) = run_cli_tool(cli_args);
    cli_tool_result(id, exit, stdout, stderr)
}

fn handle_shutdown(id: Value) -> Value {
    json_rpc_result(id, json!({}))
}

fn is_json_rpc_notification(request: &Value) -> bool {
    request.get("id").is_none()
}

#[must_use]
pub fn handle_json_rpc_message(request: &Value) -> Option<Value> {
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");

    if is_json_rpc_notification(request) && method == "notifications/cancelled" {
        return None;
    }

    Some(handle_request(request))
}

fn handle_request(request: &Value) -> Value {
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params");

    match McpMethod::parse(method) {
        McpMethod::Initialize => {
            let Some(id) = id else {
                return json_rpc_error(None, -32600, "Initialize requires id");
            };
            handle_initialize(id)
        }
        McpMethod::PromptsList => {
            let Some(id) = id else {
                return json_rpc_error(None, -32600, "prompts/list requires id");
            };
            handle_prompts_list(id)
        }
        McpMethod::PromptsGet => {
            let Some(id) = id else {
                return json_rpc_error(None, -32600, "prompts/get requires id");
            };
            handle_prompts_get(id, params)
        }
        McpMethod::ResourcesList => {
            let Some(id) = id else {
                return json_rpc_error(None, -32600, "resources/list requires id");
            };
            handle_resources_list(id)
        }
        McpMethod::ResourcesRead => {
            let Some(id) = id else {
                return json_rpc_error(None, -32600, "resources/read requires id");
            };
            handle_resources_read(id, params)
        }
        McpMethod::ResourcesTemplatesList => {
            let Some(id) = id else {
                return json_rpc_error(None, -32600, "resources/templates/list requires id");
            };
            handle_resources_templates_list(id)
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
        McpMethod::NotificationsCancelled => json_rpc_error(
            id,
            -32600,
            "notifications/cancelled must be sent as a JSON-RPC notification without id",
        ),
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
                stdout
                    .flush()
                    .map_err(|e| format!("stdout flush error: {e}"))?;
                continue;
            }
        };

        if let Some(response) = handle_json_rpc_message(&request) {
            writeln!(stdout, "{response}").map_err(|e| format!("stdout write error: {e}"))?;
            stdout
                .flush()
                .map_err(|e| format!("stdout flush error: {e}"))?;
        }

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
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;

    fn golden_fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("golden")
            .join("mcp")
            .join(name)
    }

    fn load_json_fixture(name: &str) -> Result<Value, String> {
        let path = golden_fixture_path(name);
        let contents = fs::read_to_string(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        serde_json::from_str(&contents)
            .map_err(|error| format!("failed to parse {}: {error}", path.display()))
    }

    fn first_tool_text(response: &Value) -> Result<&str, String> {
        response
            .get("result")
            .and_then(|result| result.get("content"))
            .and_then(Value::as_array)
            .and_then(|content| content.first())
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
            .ok_or_else(|| "tool response missing first text content".to_string())
    }

    fn os_args_to_strings(args: Vec<OsString>) -> Result<Vec<String>, String> {
        args.into_iter()
            .map(|arg| {
                arg.into_string()
                    .map_err(|arg| format!("non-UTF-8 argument: {arg:?}"))
            })
            .collect()
    }

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
        assert_eq!(McpMethod::parse("prompts/list"), McpMethod::PromptsList);
        assert_eq!(McpMethod::parse("prompts/get"), McpMethod::PromptsGet);
        assert_eq!(McpMethod::parse("resources/list"), McpMethod::ResourcesList);
        assert_eq!(McpMethod::parse("resources/read"), McpMethod::ResourcesRead);
        assert_eq!(
            McpMethod::parse("resources/templates/list"),
            McpMethod::ResourcesTemplatesList
        );
        assert_eq!(McpMethod::parse("tools/list"), McpMethod::ToolsList);
        assert_eq!(McpMethod::parse("tools/call"), McpMethod::ToolsCall);
        assert_eq!(
            McpMethod::parse("notifications/cancelled"),
            McpMethod::NotificationsCancelled
        );
        assert_eq!(McpMethod::parse("shutdown"), McpMethod::Shutdown);
        assert!(matches!(McpMethod::parse("unknown"), McpMethod::Unknown(_)));
    }

    #[test]
    fn handle_initialize_returns_server_info() -> Result<(), String> {
        let response = handle_initialize(json!(1));
        let Some(result) = response.get("result") else {
            return Err("initialize response missing result".to_string());
        };

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
        Ok(())
    }

    #[test]
    fn handle_prompts_list_returns_workflow_templates() -> Result<(), String> {
        let response = handle_prompts_list(json!(1));
        let Some(result) = response.get("result") else {
            return Err("prompts/list response missing result".to_string());
        };
        let Some(prompts) = result.get("prompts").and_then(Value::as_array) else {
            return Err("prompts/list response missing prompts array".to_string());
        };

        let prompt_names: Vec<&str> = prompts
            .iter()
            .filter_map(|prompt| prompt.get("name").and_then(Value::as_str))
            .collect();
        assert!(prompt_names.contains(&"pre-task-context"));
        assert!(prompt_names.contains(&"record-lesson"));
        assert!(prompt_names.contains(&"review-session"));
        Ok(())
    }

    #[test]
    fn stdio_json_rpc_golden_fixtures_match_contract() -> Result<(), String> {
        let fixture = load_json_fixture("json_rpc_cases.json")?;
        let Some(cases) = fixture.as_array() else {
            return Err("MCP JSON-RPC fixture root must be an array".to_string());
        };

        for case in cases {
            let Some(name) = case.get("name").and_then(Value::as_str) else {
                return Err("MCP JSON-RPC fixture case missing name".to_string());
            };
            let Some(request) = case.get("request") else {
                return Err(format!("MCP JSON-RPC fixture case {name} missing request"));
            };
            let Some(expected) = case.get("response") else {
                return Err(format!("MCP JSON-RPC fixture case {name} missing response"));
            };

            let actual = handle_json_rpc_message(request);
            if expected.is_null() {
                assert_eq!(
                    actual, None,
                    "MCP JSON-RPC golden fixture {name} should suppress response"
                );
            } else {
                let Some(actual) = actual else {
                    return Err(format!(
                        "MCP JSON-RPC golden fixture {name} suppressed a required response"
                    ));
                };
                assert_eq!(&actual, expected, "MCP JSON-RPC golden fixture {name}");
            }
        }
        Ok(())
    }

    #[test]
    fn initialize_capabilities_match_mcp_manifest() -> Result<(), String> {
        let manifest: Value = serde_json::from_str(&crate::output::render_mcp_manifest_json())
            .map_err(|error| format!("invalid MCP manifest JSON: {error}"))?;
        let initialize = handle_initialize(json!("init"));
        let Some(initialize_result) = initialize.get("result") else {
            return Err("initialize response missing result".to_string());
        };

        assert_eq!(
            initialize_result
                .get("protocolVersion")
                .and_then(Value::as_str),
            manifest
                .get("data")
                .and_then(|data| data.get("protocolVersion"))
                .and_then(Value::as_str)
        );

        let Some(manifest_capabilities) = manifest
            .get("data")
            .and_then(|data| data.get("capabilities"))
            .and_then(Value::as_object)
        else {
            return Err("MCP manifest missing capabilities object".to_string());
        };
        let Some(initialize_capabilities) = initialize_result
            .get("capabilities")
            .and_then(Value::as_object)
        else {
            return Err("initialize response missing capabilities object".to_string());
        };

        for capability in ["tools", "resources", "prompts"] {
            let manifest_enabled = manifest_capabilities
                .get(capability)
                .and_then(Value::as_bool)
                .ok_or_else(|| format!("manifest missing {capability} capability"))?;
            assert_eq!(
                initialize_capabilities.contains_key(capability),
                manifest_enabled,
                "initialize capability {capability} must match MCP manifest"
            );
        }
        Ok(())
    }

    #[test]
    fn schema_resources_match_public_schema_registry() -> Result<(), String> {
        let expected: BTreeSet<&str> = public_schemas().iter().map(|schema| schema.id).collect();
        let response = handle_resources_list(json!("schemas"));
        let Some(resources) = response
            .get("result")
            .and_then(|result| result.get("resources"))
            .and_then(Value::as_array)
        else {
            return Err("resources/list response missing resources array".to_string());
        };

        let actual: BTreeSet<&str> = resources
            .iter()
            .filter_map(|resource| resource.get("uri").and_then(Value::as_str))
            .filter_map(|uri| uri.strip_prefix("ee://schemas/"))
            .collect();

        assert_eq!(
            actual, expected,
            "MCP schema resources must mirror public_schemas() exactly"
        );
        Ok(())
    }

    #[test]
    fn handle_prompts_get_pre_task_context_renders_arguments() -> Result<(), String> {
        let response = handle_prompts_get(
            json!(1),
            Some(&json!({
                "name": "pre-task-context",
                "arguments": {
                    "task": "prepare release",
                    "workspace": ".",
                    "profile": "balanced",
                    "maxTokens": 3000
                }
            })),
        );
        let Some(result) = response.get("result") else {
            return Err("prompts/get response missing result".to_string());
        };
        let Some(messages) = result.get("messages").and_then(Value::as_array) else {
            return Err("prompts/get response missing messages array".to_string());
        };
        let Some(text) = messages
            .first()
            .and_then(|message| message.get("content"))
            .and_then(|content| content.get("text"))
            .and_then(Value::as_str)
        else {
            return Err("prompts/get response missing text".to_string());
        };
        assert!(text.contains("prepare release"));
        assert!(text.contains("ee_context"));
        assert!(text.contains("--max-tokens 3000"));
        Ok(())
    }

    #[test]
    fn handle_prompts_get_pre_task_context_rejects_invalid_profile() -> Result<(), String> {
        let response = handle_prompts_get(
            json!(1),
            Some(&json!({
                "name": "pre-task-context",
                "arguments": {
                    "task": "prepare release",
                    "profile": "release"
                }
            })),
        );
        let Some(error) = response.get("error") else {
            return Err("pre-task-context invalid profile response missing error".to_string());
        };
        assert_eq!(error.get("code").and_then(Value::as_i64), Some(-32602));
        let Some(message) = error.get("message").and_then(Value::as_str) else {
            return Err("pre-task-context invalid profile response missing message".to_string());
        };
        assert!(message.contains("Invalid context profile 'release'"));
        Ok(())
    }

    #[test]
    fn handle_prompts_get_record_lesson_requires_lesson() -> Result<(), String> {
        let response = handle_prompts_get(
            json!(1),
            Some(&json!({
                "name": "record-lesson",
                "arguments": {}
            })),
        );
        let Some(error) = response.get("error") else {
            return Err("record-lesson prompt response missing error".to_string());
        };
        assert_eq!(error.get("code").and_then(Value::as_i64), Some(-32602));
        Ok(())
    }

    #[test]
    fn handle_resources_list_returns_static_resources() -> Result<(), String> {
        let response = handle_resources_list(json!(1));
        let Some(result) = response.get("result") else {
            return Err("resources/list response missing result".to_string());
        };
        let Some(resources) = result.get("resources").and_then(Value::as_array) else {
            return Err("resources/list response missing resources array".to_string());
        };

        let resource_uris: Vec<&str> = resources
            .iter()
            .filter_map(|resource| resource.get("uri").and_then(Value::as_str))
            .collect();
        assert!(resource_uris.contains(&"ee://agent-docs"));
        assert!(resource_uris.contains(&"ee://agent-docs/guide"));
        assert!(resource_uris.contains(&"ee://schemas"));
        assert!(resource_uris.contains(&"ee://schemas/ee.response.v1"));
        assert!(resource_uris.contains(&"ee://workspace/status"));
        Ok(())
    }

    #[test]
    fn handle_resources_templates_list_returns_dynamic_resources() -> Result<(), String> {
        let response = handle_resources_templates_list(json!(1));
        let Some(result) = response.get("result") else {
            return Err("resources/templates/list response missing result".to_string());
        };
        let Some(templates) = result.get("resourceTemplates").and_then(Value::as_array) else {
            return Err(
                "resources/templates/list response missing resourceTemplates array".to_string(),
            );
        };

        let uri_templates: Vec<&str> = templates
            .iter()
            .filter_map(|template| template.get("uriTemplate").and_then(Value::as_str))
            .collect();
        assert!(uri_templates.contains(&"ee://memories/{memoryId}"));
        assert!(uri_templates.contains(&"ee://context-packs/by-query?query={query}"));
        Ok(())
    }

    #[test]
    fn handle_resources_read_agent_docs_routes_to_cli_json() -> Result<(), String> {
        let response =
            handle_resources_read(json!(1), Some(&json!({ "uri": "ee://agent-docs/guide" })));
        let Some(result) = response.get("result") else {
            return Err("resources/read response missing result".to_string());
        };
        assert_eq!(result.get("isError").and_then(Value::as_bool), Some(false));
        assert_eq!(result.get("exitCode").and_then(Value::as_u64), Some(0));

        let Some(contents) = result.get("contents").and_then(Value::as_array) else {
            return Err("resources/read response missing contents".to_string());
        };
        let Some(text) = contents
            .first()
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
        else {
            return Err("resources/read response missing text content".to_string());
        };
        let parsed: Value =
            serde_json::from_str(text).map_err(|error| format!("invalid JSON: {error}"))?;
        assert_eq!(
            parsed.get("schema").and_then(Value::as_str),
            Some("ee.response.v1")
        );
        assert_eq!(
            parsed
                .get("data")
                .and_then(|data| data.get("command"))
                .and_then(Value::as_str),
            Some("agent-docs")
        );
        assert_eq!(
            parsed
                .get("data")
                .and_then(|data| data.get("topic"))
                .and_then(Value::as_str),
            Some("guide")
        );
        Ok(())
    }

    #[test]
    fn handle_resources_read_context_requires_query_param() -> Result<(), String> {
        let response = handle_resources_read(
            json!(1),
            Some(&json!({ "uri": "ee://context-packs/by-query" })),
        );
        let Some(error) = response.get("error") else {
            return Err("context resource response missing error".to_string());
        };
        assert_eq!(error.get("code").and_then(Value::as_i64), Some(-32602));
        Ok(())
    }

    #[test]
    fn handle_tools_list_returns_tool_definitions() -> Result<(), String> {
        let response = handle_tools_list(json!(1));
        let Some(result) = response.get("result") else {
            return Err("tools/list response missing result".to_string());
        };
        let Some(tools) = result.get("tools").and_then(Value::as_array) else {
            return Err("tools/list response missing tools array".to_string());
        };

        assert!(!tools.is_empty());

        let tool_names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        assert!(tool_names.contains(&"ee_search"));
        assert!(tool_names.contains(&"ee_context"));
        assert!(tool_names.contains(&"ee_status"));
        assert!(tool_names.contains(&"ee_health"));
        assert!(tool_names.contains(&"ee_capabilities"));
        assert!(tool_names.contains(&"ee_memory_show"));
        assert!(tool_names.contains(&"ee_why"));
        assert!(tool_names.contains(&"ee_remember"));
        assert!(tool_names.contains(&"ee_outcome"));

        let Some(remember) = tools
            .iter()
            .find(|tool| tool.get("name").and_then(Value::as_str) == Some("ee_remember"))
        else {
            return Err("ee_remember tool missing".to_string());
        };
        for name in [
            "ee_health",
            "ee_status",
            "ee_capabilities",
            "ee_search",
            "ee_context",
            "ee_memory_show",
            "ee_why",
        ] {
            let Some(tool) = tools
                .iter()
                .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
            else {
                return Err(format!("{name} tool missing"));
            };
            let annotations = tool
                .get("annotations")
                .ok_or_else(|| format!("{name} missing annotations"))?;
            assert_eq!(
                annotations.get("readOnlyHint").and_then(Value::as_bool),
                Some(true),
                "{name} must be annotated read-only"
            );
            assert_eq!(
                annotations.get("idempotentHint").and_then(Value::as_bool),
                Some(true),
                "{name} must be annotated idempotent"
            );
            assert_eq!(
                annotations.get("destructiveHint").and_then(Value::as_bool),
                Some(false),
                "{name} must be annotated non-destructive"
            );
        }
        assert_eq!(
            remember
                .get("annotations")
                .and_then(|annotations| annotations.get("readOnlyHint"))
                .and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            remember
                .get("annotations")
                .and_then(|annotations| annotations.get("destructiveHint"))
                .and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            remember
                .get("eeEffect")
                .and_then(|effect| effect.get("defaultDryRun"))
                .and_then(Value::as_bool),
            Some(true)
        );
        Ok(())
    }

    #[test]
    fn handle_tools_call_status_routes_to_cli_json() -> Result<(), String> {
        let response = handle_tools_call(json!(1), Some(&json!({ "name": "ee_status" })));
        let Some(result) = response.get("result") else {
            return Err("status tool response missing result".to_string());
        };
        assert_eq!(result.get("isError").and_then(Value::as_bool), Some(false));
        assert_eq!(result.get("exitCode").and_then(Value::as_u64), Some(0));

        let Some(content) = result.get("content").and_then(Value::as_array) else {
            return Err("status tool response missing content".to_string());
        };
        let Some(text) = content
            .first()
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
        else {
            return Err("status tool response missing text content".to_string());
        };
        let parsed: Value =
            serde_json::from_str(text).map_err(|error| format!("invalid JSON: {error}"))?;
        assert_eq!(
            parsed.get("schema").and_then(Value::as_str),
            Some("ee.response.v1")
        );
        assert_eq!(
            parsed
                .get("data")
                .and_then(|data| data.get("command"))
                .and_then(Value::as_str),
            Some("status")
        );
        Ok(())
    }

    #[test]
    fn handle_tools_call_remember_defaults_to_dry_run() -> Result<(), String> {
        let response = handle_tools_call(
            json!(1),
            Some(&json!({
                "name": "ee_remember",
                "arguments": {
                    "content": "Run cargo fmt --check before release.",
                    "level": "procedural",
                    "kind": "rule",
                    "tags": "cargo,release",
                    "confidence": 0.9
                }
            })),
        );
        let Some(result) = response.get("result") else {
            return Err("remember tool response missing result".to_string());
        };
        assert_eq!(result.get("isError").and_then(Value::as_bool), Some(false));
        assert_eq!(result.get("exitCode").and_then(Value::as_u64), Some(0));

        let parsed: Value = serde_json::from_str(first_tool_text(&response)?)
            .map_err(|error| format!("invalid JSON: {error}"))?;
        assert_eq!(
            parsed
                .get("data")
                .and_then(|data| data.get("command"))
                .and_then(Value::as_str),
            Some("remember")
        );
        assert_eq!(
            parsed
                .get("data")
                .and_then(|data| data.get("dry_run"))
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            parsed
                .get("data")
                .and_then(|data| data.get("persisted"))
                .and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            parsed
                .get("data")
                .and_then(|data| data.get("audit_id"))
                .and_then(Value::as_null),
            Some(())
        );
        assert_eq!(
            parsed
                .get("data")
                .and_then(|data| data.get("redaction_status"))
                .and_then(Value::as_str),
            Some("checked")
        );
        Ok(())
    }

    #[test]
    fn build_cli_args_outcome_defaults_to_dry_run_and_keeps_event_id() -> Result<(), String> {
        let args = os_args_to_strings(build_cli_args_for_tool(
            McpTool::Outcome,
            &json!({
                "targetId": "mem_00000000000000000000000001",
                "signal": "helpful",
                "eventId": "fb_01234567890123456789012345",
                "actor": "mcp-test"
            }),
        )?)?;

        assert!(args.contains(&"outcome".to_string()));
        assert!(args.contains(&"--dry-run".to_string()));
        assert!(args.contains(&"--event-id".to_string()));
        assert!(args.contains(&"fb_01234567890123456789012345".to_string()));
        assert!(args.contains(&"--actor".to_string()));
        assert!(args.contains(&"mcp-test".to_string()));
        Ok(())
    }

    #[test]
    fn build_cli_args_remember_allow_write_removes_dry_run_flag() -> Result<(), String> {
        let args = os_args_to_strings(build_cli_args_for_tool(
            McpTool::Remember,
            &json!({
                "content": "Persist this only with an explicit write gate.",
                "dryRun": false,
                "allowWrite": true
            }),
        )?)?;

        assert!(args.contains(&"remember".to_string()));
        assert!(!args.contains(&"--dry-run".to_string()));
        assert!(!args.contains(&"--allow-write".to_string()));
        Ok(())
    }

    #[test]
    fn handle_tools_call_remember_write_requires_allow_write() -> Result<(), String> {
        let response = handle_tools_call(
            json!(1),
            Some(&json!({
                "name": "ee_remember",
                "arguments": {
                    "content": "Persisted memory requires a gate.",
                    "dryRun": false
                }
            })),
        );
        let Some(error) = response.get("error") else {
            return Err("remember write gate response missing error".to_string());
        };
        assert_eq!(error.get("code").and_then(Value::as_i64), Some(-32602));
        assert_eq!(
            error.get("message").and_then(Value::as_str),
            Some("Write tool ee_remember requires allowWrite=true when dryRun=false")
        );
        Ok(())
    }

    #[test]
    fn handle_tools_call_context_forwards_budget_flags() -> Result<(), String> {
        let args = os_args_to_strings(build_cli_args_for_tool(
            McpTool::Context,
            &json!({
                "query": "prepare release",
                "maxTokens": u32::MAX,
                "candidatePool": 250,
                "profile": "thorough"
            }),
        )?)?;

        let max_tokens = u32::MAX.to_string();
        let max_tokens_index = args
            .iter()
            .position(|arg| arg == "--max-tokens")
            .ok_or_else(|| "context args missing --max-tokens".to_string())?;
        assert_eq!(args.get(max_tokens_index + 1), Some(&max_tokens));

        let candidate_pool = "250".to_string();
        let candidate_pool_index = args
            .iter()
            .position(|arg| arg == "--candidate-pool")
            .ok_or_else(|| "context args missing --candidate-pool".to_string())?;
        assert_eq!(args.get(candidate_pool_index + 1), Some(&candidate_pool));
        assert!(args.contains(&"--profile".to_string()));
        assert!(args.contains(&"thorough".to_string()));
        Ok(())
    }

    #[test]
    fn handle_tools_call_context_rejects_invalid_profile_before_cli() -> Result<(), String> {
        let response = handle_tools_call(
            json!(1),
            Some(&json!({
                "name": "ee_context",
                "arguments": {
                    "query": "prepare release",
                    "profile": "release"
                }
            })),
        );
        let Some(error) = response.get("error") else {
            return Err("context invalid profile response missing error".to_string());
        };
        assert_eq!(error.get("code").and_then(Value::as_i64), Some(-32602));
        let Some(message) = error.get("message").and_then(Value::as_str) else {
            return Err("context invalid profile response missing message".to_string());
        };
        assert!(message.contains("Invalid context profile 'release'"));
        Ok(())
    }

    #[test]
    fn handle_tools_call_context_rejects_budget_overflow_before_cli() -> Result<(), String> {
        let response = handle_tools_call(
            json!(1),
            Some(&json!({
                "name": "ee_context",
                "arguments": {
                    "query": "prepare release",
                    "maxTokens": u64::from(u32::MAX) + 1
                }
            })),
        );
        let Some(error) = response.get("error") else {
            return Err("budget overflow response missing error".to_string());
        };
        assert_eq!(error.get("code").and_then(Value::as_i64), Some(-32602));
        assert_eq!(
            error.get("message").and_then(Value::as_str),
            Some("Argument 'maxTokens' is too large")
        );
        Ok(())
    }

    #[test]
    fn handle_tools_call_context_storage_error_stays_machine_readable() -> Result<(), String> {
        let missing_workspace = format!(
            "/tmp/ee-mcp-missing-workspace-{}-context",
            std::process::id()
        );
        let response = handle_tools_call(
            json!("degraded"),
            Some(&json!({
                "name": "ee_context",
                "arguments": {
                    "query": "prepare release",
                    "workspace": missing_workspace
                }
            })),
        );

        let Some(result) = response.get("result") else {
            return Err("context degraded response missing result".to_string());
        };
        assert_eq!(result.get("isError").and_then(Value::as_bool), Some(true));
        assert_eq!(
            result.get("exitCode").and_then(Value::as_u64),
            Some(u64::from(ProcessExitCode::Storage as u8))
        );
        assert_eq!(result.get("stderr").and_then(Value::as_str), Some(""));

        let text = first_tool_text(&response)?;
        assert!(
            text.starts_with("{\"schema\":\"ee.error.v1\""),
            "context degraded output must stay in the stable error envelope"
        );
        let parsed: Value =
            serde_json::from_str(text).map_err(|error| format!("invalid JSON: {error}"))?;
        let Some(error) = parsed.get("error") else {
            return Err("context degraded envelope missing error object".to_string());
        };
        assert_eq!(error.get("code").and_then(Value::as_str), Some("storage"));
        assert!(
            error
                .get("message")
                .and_then(Value::as_str)
                .is_some_and(|message| message.contains("Database not found")),
            "context degraded error should explain missing database"
        );
        assert!(
            error
                .get("repair")
                .and_then(Value::as_str)
                .is_some_and(|repair| !repair.is_empty()),
            "context degraded error should include a repair hint"
        );
        Ok(())
    }

    #[test]
    fn cancelled_notification_is_fire_and_forget() {
        let request = json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {
                "requestId": "context-1",
                "reason": "caller no longer needs the response"
            }
        });

        assert_eq!(handle_json_rpc_message(&request), None);
    }

    #[test]
    fn cancelled_notification_with_id_returns_protocol_error() -> Result<(), String> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": "bad-cancel",
            "method": "notifications/cancelled",
            "params": {
                "requestId": "context-1"
            }
        });
        let response = handle_request(&request);
        let Some(error) = response.get("error") else {
            return Err("cancelled request with id response missing error".to_string());
        };
        assert_eq!(error.get("code").and_then(Value::as_i64), Some(-32600));
        assert_eq!(
            error.get("message").and_then(Value::as_str),
            Some("notifications/cancelled must be sent as a JSON-RPC notification without id")
        );
        Ok(())
    }

    #[test]
    fn handle_tools_call_outcome_write_requires_allow_write() -> Result<(), String> {
        let response = handle_tools_call(
            json!(1),
            Some(&json!({
                "name": "ee_outcome",
                "arguments": {
                    "targetId": "mem_00000000000000000000000001",
                    "signal": "helpful",
                    "dryRun": false
                }
            })),
        );
        let Some(error) = response.get("error") else {
            return Err("outcome write gate response missing error".to_string());
        };
        assert_eq!(error.get("code").and_then(Value::as_i64), Some(-32602));
        assert_eq!(
            error.get("message").and_then(Value::as_str),
            Some("Write tool ee_outcome requires allowWrite=true when dryRun=false")
        );
        Ok(())
    }

    #[test]
    fn handle_tools_call_search_requires_query() -> Result<(), String> {
        let response = handle_tools_call(json!(1), Some(&json!({ "name": "ee_search" })));
        let Some(error) = response.get("error") else {
            return Err("search tool response missing error".to_string());
        };
        assert_eq!(error.get("code").and_then(Value::as_i64), Some(-32602));
        Ok(())
    }

    #[test]
    fn handle_tools_call_unknown_returns_error() -> Result<(), String> {
        let response = handle_tools_call(json!(1), Some(&json!({ "name": "nonexistent_tool" })));
        let Some(error) = response.get("error") else {
            return Err("unknown tool response missing error".to_string());
        };
        assert_eq!(error.get("code").and_then(Value::as_i64), Some(-32601));
        Ok(())
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
