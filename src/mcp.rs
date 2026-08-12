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
//! - Same response contracts (ee.response.v2)
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

use crate::config::env_registry::{EnvVar, read_or_default as read_env_var_or_default};
use crate::core::agent_docs::AgentDocsTopic;
use crate::models::{ContextProfileName, ProcessExitCode, RedactionLevel};
pub use crate::output::MCP_PROTOCOL_VERSION;
use crate::output::public_schemas;

pub const SUBSYSTEM: &str = "mcp";
pub const MCP_SCHEMA_V1: &str = "ee.mcp.v1";
pub const MCP_SIZE_LIMIT_EXCEEDED_CODE: &str = "size_limit_exceeded";
pub const DEFAULT_MCP_MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
const MIN_MCP_MAX_REQUEST_BYTES: usize = 1024;

fn trace_mcp_top_level(phase: &'static str, elapsed_ms: u64, degraded_codes: &[&str]) {
    tracing::info!(
        workspace_id = "mcp-stdio",
        request_id = "mcp_json_rpc_request",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-3usjw.3"),
        surface = "mcp_top_level",
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        "MCP top-level request checkpoint"
    );
}

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
    PreEditRecall,
    RecordLesson,
    ReviewSession,
}

impl McpPrompt {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "pre-task-context" => Some(Self::PreTaskContext),
            "pre-edit-recall" => Some(Self::PreEditRecall),
            "record-lesson" => Some(Self::RecordLesson),
            "review-session" => Some(Self::ReviewSession),
            _ => None,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::PreTaskContext => "pre-task-context",
            Self::PreEditRecall => "pre-edit-recall",
            Self::RecordLesson => "record-lesson",
            Self::ReviewSession => "review-session",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::PreTaskContext => {
                "Prepare an agent before a task by retrieving a context pack with ee."
            }
            Self::PreEditRecall => "Recall code-anchored memories before editing files.",
            Self::RecordLesson => "Turn a durable lesson into an explicit ee remember workflow.",
            Self::ReviewSession => "Review a prior session and propose curation candidates.",
        }
    }
}

type McpToolSchemaBuilder = fn() -> Value;
type McpToolArgsBuilder = fn(&mut Vec<OsString>, &Value) -> Result<(), String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct McpToolAnnotations {
    read_only: bool,
    destructive: bool,
    idempotent: bool,
    open_world: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct McpToolEffect {
    kind: &'static str,
    write_surface: &'static [&'static str],
    default_dry_run: bool,
    requires_allow_write_when_dry_run_false: bool,
    audit: &'static str,
    redaction: &'static str,
    idempotency: &'static str,
    destructive: bool,
}

#[derive(Debug, Clone, Copy)]
struct McpToolEntry {
    name: &'static str,
    description: &'static str,
    input_schema: McpToolSchemaBuilder,
    annotations: McpToolAnnotations,
    effect: Option<McpToolEffect>,
    args_builder: McpToolArgsBuilder,
}

const READ_ONLY_TOOL_ANNOTATIONS: McpToolAnnotations = McpToolAnnotations {
    read_only: true,
    destructive: false,
    idempotent: true,
    open_world: false,
};

const WRITE_TOOL_ANNOTATIONS: McpToolAnnotations = McpToolAnnotations {
    read_only: false,
    destructive: false,
    idempotent: false,
    open_world: false,
};

const REMEMBER_TOOL_EFFECT: McpToolEffect = McpToolEffect {
    kind: "durable_write",
    write_surface: &["memories", "audit_log", "search_index_jobs"],
    default_dry_run: true,
    requires_allow_write_when_dry_run_false: true,
    audit: "ee remember writes audit_id through the CLI core path",
    redaction: "content is checked by remember policy before storage",
    idempotency: "not_idempotent_for_durable_writes",
    destructive: false,
};

const OUTCOME_TOOL_EFFECT: McpToolEffect = McpToolEffect {
    kind: "durable_write",
    write_surface: &["feedback_events", "audit_log"],
    default_dry_run: true,
    requires_allow_write_when_dry_run_false: true,
    audit: "ee outcome writes audit_id through the CLI core path",
    redaction: "evidenceJson is validated and stored only through the core outcome path",
    idempotency: "eventId enables idempotent retries when content matches",
    destructive: false,
};

const PRIMER_TOOL_EFFECT: McpToolEffect = McpToolEffect {
    kind: "derived_cache_write",
    write_surface: &["primer_cache"],
    default_dry_run: false,
    requires_allow_write_when_dry_run_false: false,
    audit: "ee primer may update rebuildable primer_cache rows through the CLI core path",
    redaction: "primer output is assembled from already-stored memories after policy screening",
    idempotency: "cache entries are deterministic derived artifacts and can be rebuilt",
    destructive: false,
};

const JOURNAL_APPEND_TOOL_EFFECT: McpToolEffect = McpToolEffect {
    kind: "durable_write",
    write_surface: &["journal_entries", "audit_log"],
    default_dry_run: true,
    requires_allow_write_when_dry_run_false: true,
    audit: "ee journal append writes audit evidence through the CLI core path",
    redaction: "policy screening runs before storage and secret-like spans are redacted first",
    idempotency: "single-entry appends are not idempotent; JSONL batches use per-line semantics",
    destructive: false,
};

const DECIDE_RECORD_TOOL_EFFECT: McpToolEffect = McpToolEffect {
    kind: "durable_write",
    write_surface: &["memories", "decision_chains", "audit_log"],
    default_dry_run: true,
    requires_allow_write_when_dry_run_false: true,
    audit: "ee decide record writes decision memory and lifecycle audit rows through the CLI core path",
    redaction: "decision topic, rationale, and options pass through memory policy screening before storage",
    idempotency: "supersedes links converge decision chains; new durable records are otherwise not idempotent",
    destructive: false,
};

const MESH_DISCOVERY_POLICY_TOOL_EFFECT: McpToolEffect = McpToolEffect {
    kind: "policy_write",
    write_surface: &[
        ".ee/discovery_policy.toml",
        ".ee/discovery_*list.toml",
        "audit_log",
    ],
    default_dry_run: false,
    requires_allow_write_when_dry_run_false: true,
    audit: "set/allow/deny operations write mesh.discovery_policy_changed",
    redaction: "node keys are reported in command output but audit details store nodeKeyHash",
    idempotency: "set/allow/deny are deterministic and converge the workspace policy files",
    destructive: false,
};

const TOOL_REGISTRY: &[McpToolEntry] = &[
    McpToolEntry {
        name: "ee_health",
        description: "Run ee health --json and return the response envelope",
        input_schema: workspace_only_tool_schema,
        annotations: READ_ONLY_TOOL_ANNOTATIONS,
        effect: None,
        args_builder: build_health_tool_args,
    },
    McpToolEntry {
        name: "ee_status",
        description: "Run ee status --json and return workspace readiness",
        input_schema: workspace_only_tool_schema,
        annotations: READ_ONLY_TOOL_ANNOTATIONS,
        effect: None,
        args_builder: build_status_tool_args,
    },
    McpToolEntry {
        name: "ee_doctor",
        description: "Run ee doctor --json and return health checks with repair actions",
        input_schema: workspace_only_tool_schema,
        annotations: READ_ONLY_TOOL_ANNOTATIONS,
        effect: None,
        args_builder: build_doctor_tool_args,
    },
    McpToolEntry {
        name: "ee_capabilities",
        description: "Run ee capabilities --json and return feature availability",
        input_schema: workspace_only_tool_schema,
        annotations: READ_ONLY_TOOL_ANNOTATIONS,
        effect: None,
        args_builder: build_capabilities_tool_args,
    },
    McpToolEntry {
        name: "ee_search",
        description: "Run ee search --json over indexed memories and sessions",
        input_schema: search_tool_schema,
        annotations: READ_ONLY_TOOL_ANNOTATIONS,
        effect: None,
        args_builder: build_search_tool_args,
    },
    McpToolEntry {
        name: "ee_context",
        description: "Run ee pack --json to assemble task-specific context",
        input_schema: context_tool_schema,
        annotations: READ_ONLY_TOOL_ANNOTATIONS,
        effect: None,
        args_builder: build_context_tool_args,
    },
    McpToolEntry {
        name: "ee_recall",
        description: "Run ee recall --json for code-anchored memory lookup",
        input_schema: recall_tool_schema,
        annotations: READ_ONLY_TOOL_ANNOTATIONS,
        effect: None,
        args_builder: build_recall_tool_args,
    },
    McpToolEntry {
        name: "ee_ask",
        description: "Run ee ask --json for extractive answers; abstention stays in the response payload",
        input_schema: ask_tool_schema,
        annotations: READ_ONLY_TOOL_ANNOTATIONS,
        effect: None,
        args_builder: build_ask_tool_args,
    },
    McpToolEntry {
        name: "ee_primer",
        description: "Run ee primer --json for workspace primer assembly; may update rebuildable primer cache rows",
        input_schema: primer_tool_schema,
        annotations: READ_ONLY_TOOL_ANNOTATIONS,
        effect: Some(PRIMER_TOOL_EFFECT),
        args_builder: build_primer_tool_args,
    },
    McpToolEntry {
        name: "ee_insights",
        description: "Run ee insights --json for graph-derived insight bundles",
        input_schema: insights_tool_schema,
        annotations: READ_ONLY_TOOL_ANNOTATIONS,
        effect: None,
        args_builder: build_insights_tool_args,
    },
    McpToolEntry {
        name: "ee_proximity",
        description: "Run ee proximity --json for pairwise memory graph proximity",
        input_schema: proximity_tool_schema,
        annotations: READ_ONLY_TOOL_ANNOTATIONS,
        effect: None,
        args_builder: build_proximity_tool_args,
    },
    McpToolEntry {
        name: "ee_pack_dna_explain",
        description: "Run ee pack --explain --json and return only data.pack.packDna",
        input_schema: pack_dna_explain_tool_schema,
        annotations: READ_ONLY_TOOL_ANNOTATIONS,
        effect: None,
        args_builder: build_pack_dna_explain_tool_args,
    },
    McpToolEntry {
        name: "ee_revision_impact",
        description: "Run a dry-run memory revision probe and return only data.impactAnalysis",
        input_schema: revision_impact_tool_schema,
        annotations: READ_ONLY_TOOL_ANNOTATIONS,
        effect: None,
        args_builder: build_revision_impact_tool_args,
    },
    McpToolEntry {
        name: "ee_memory_show",
        description: "Run ee memory show --json for a single memory",
        input_schema: memory_show_tool_schema,
        annotations: READ_ONLY_TOOL_ANNOTATIONS,
        effect: None,
        args_builder: build_memory_show_tool_args,
    },
    McpToolEntry {
        name: "ee_why",
        description: "Run ee why --json to explain memory storage, retrieval, or selection",
        input_schema: why_tool_schema,
        annotations: READ_ONLY_TOOL_ANNOTATIONS,
        effect: None,
        args_builder: build_why_tool_args,
    },
    McpToolEntry {
        name: "ee_remember",
        description: "Gated write tool for ee remember --json; defaults to dry-run and requires allowWrite=true for durable writes",
        input_schema: remember_tool_schema,
        annotations: WRITE_TOOL_ANNOTATIONS,
        effect: Some(REMEMBER_TOOL_EFFECT),
        args_builder: build_remember_tool_args,
    },
    McpToolEntry {
        name: "ee_outcome",
        description: "Gated write tool for ee outcome --json; defaults to dry-run and requires allowWrite=true for durable writes",
        input_schema: outcome_tool_schema,
        annotations: WRITE_TOOL_ANNOTATIONS,
        effect: Some(OUTCOME_TOOL_EFFECT),
        args_builder: build_outcome_tool_args,
    },
    McpToolEntry {
        name: "ee_journal_append",
        description: "Gated write tool for ee journal append --json; durable appends require dryRun=false and allowWrite=true",
        input_schema: journal_append_tool_schema,
        annotations: WRITE_TOOL_ANNOTATIONS,
        effect: Some(JOURNAL_APPEND_TOOL_EFFECT),
        args_builder: build_journal_append_tool_args,
    },
    McpToolEntry {
        name: "ee_decide_record",
        description: "Gated write tool for ee decide record --json; use supersedes to advance an existing decision chain instead of forking topics",
        input_schema: decide_record_tool_schema,
        annotations: WRITE_TOOL_ANNOTATIONS,
        effect: Some(DECIDE_RECORD_TOOL_EFFECT),
        args_builder: build_decide_record_tool_args,
    },
    McpToolEntry {
        name: "ee_decide_list",
        description: "Run ee decide list --json for current decision heads and optional superseded history",
        input_schema: decide_list_tool_schema,
        annotations: READ_ONLY_TOOL_ANNOTATIONS,
        effect: None,
        args_builder: build_decide_list_tool_args,
    },
    McpToolEntry {
        name: "ee_decide_revisit",
        description: "Run ee decide revisit --json for decisions due or nearing their revisit window",
        input_schema: decide_revisit_tool_schema,
        annotations: READ_ONLY_TOOL_ANNOTATIONS,
        effect: None,
        args_builder: build_decide_revisit_tool_args,
    },
    McpToolEntry {
        name: "ee_mesh_discovery_policy",
        description: "Inspect or update ee mesh discovery policy; set/allow/deny require allowWrite=true",
        input_schema: mesh_discovery_policy_tool_schema,
        annotations: WRITE_TOOL_ANNOTATIONS,
        effect: Some(MESH_DISCOVERY_POLICY_TOOL_EFFECT),
        args_builder: build_mesh_discovery_policy_tool_args,
    },
];

fn mcp_tool_entry(name: &str) -> Option<&'static McpToolEntry> {
    TOOL_REGISTRY.iter().find(|tool| tool.name == name)
}

pub fn registered_tool_names() -> impl Iterator<Item = &'static str> {
    TOOL_REGISTRY.iter().map(|tool| tool.name)
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

fn json_rpc_error_with_data(id: Option<Value>, code: i32, message: &str, data: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
            "data": data
        }
    })
}

fn mcp_stdio_byte_limit() -> usize {
    read_env_var_or_default(EnvVar::McpMaxRequestBytes)
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MCP_MAX_REQUEST_BYTES)
        .max(MIN_MCP_MAX_REQUEST_BYTES)
}

fn mcp_size_limit_exceeded_error(
    id: Option<Value>,
    direction: &str,
    actual_bytes: usize,
    max_bytes: usize,
) -> Value {
    json_rpc_error_with_data(
        id,
        -32000,
        MCP_SIZE_LIMIT_EXCEEDED_CODE,
        json!({
            "code": MCP_SIZE_LIMIT_EXCEEDED_CODE,
            "direction": direction,
            "actualBytes": actual_bytes,
            "maxBytes": max_bytes
        }),
    )
}

fn json_rpc_result(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

#[derive(Debug)]
struct LimitedCapture {
    bytes: Vec<u8>,
    max_bytes: usize,
    bytes_seen: usize,
    truncated: bool,
}

impl LimitedCapture {
    fn new(max_bytes: usize) -> Self {
        Self {
            bytes: Vec::new(),
            max_bytes,
            bytes_seen: 0,
            truncated: false,
        }
    }

    fn into_string(self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

impl Write for LimitedCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes_seen = self.bytes_seen.saturating_add(buf.len());
        let remaining = self.max_bytes.saturating_sub(self.bytes.len());
        if remaining == 0 {
            if !buf.is_empty() {
                self.truncated = true;
            }
            return Ok(buf.len());
        }

        let keep = remaining.min(buf.len());
        self.bytes.extend_from_slice(&buf[..keep]);
        if keep < buf.len() {
            self.truncated = true;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct McpCliRunResult {
    exit: ProcessExitCode,
    stdout: String,
    stderr: String,
    stdout_bytes_seen: usize,
    stderr_bytes_seen: usize,
    truncated: bool,
}

/// Process-owned state shared by every request on one MCP stdio session.
///
/// The CLI owner carries advisory-delivery history across tool and resource
/// calls. Standalone helpers construct a fresh owner, while the real stdio
/// loop retains one until shutdown or EOF.
#[derive(Debug, Default)]
struct McpProcess {
    cli: crate::cli::CliProcess,
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
        McpPrompt::PreEditRecall => vec![
            prompt_argument(
                "path",
                "Workspace-relative path or glob to recall against",
                false,
            ),
            prompt_argument("symbol", "Exact symbol name to recall against", false),
            prompt_argument("diff", "Git ref for changed-path recall", false),
            prompt_argument("diffStaged", "Recall against staged paths", false),
            prompt_argument(
                "workspace",
                "Workspace path; defaults to current directory",
                false,
            ),
            prompt_argument("budgetTokens", "Recall token budget", false),
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
                prompt_descriptor(McpPrompt::PreEditRecall),
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
    argument_name(names)?;
    if arguments.is_null() {
        return Ok(None);
    }
    optional_string(arguments, names)
}

fn prompt_required_string<'a>(arguments: &'a Value, names: &[&str]) -> Result<&'a str, String> {
    let name = argument_name(names)?;
    if arguments.is_null() {
        return Err(format!("Missing required prompt argument '{name}'"));
    }
    required_string(arguments, names)
}

fn prompt_optional_bool(arguments: &Value, names: &[&str]) -> Result<bool, String> {
    argument_name(names)?;
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
                "Invalid context profile '{value}'. Expected compact, balanced, grounding, orientation, thorough, or submodular."
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
        "Prepare for this task with ee before editing.\n\nTask:\n{task}\n\nUse the read-only MCP tool `ee_context` or the CLI command below:\n`ee pack {task:?} --workspace {workspace} --profile {profile} --max-tokens {max_tokens} --json`\n\nRead the returned `ee.response.v2` envelope, summarize the highest-confidence procedural rules, relevant failures, decisions, provenance, and degraded capabilities, then proceed with the task. Keep machine JSON separate from human diagnostics."
    ))
}

fn render_pre_edit_recall_prompt(arguments: &Value) -> Result<String, String> {
    let workspace = prompt_optional_string(arguments, &["workspace"])?.unwrap_or(".");
    let path = prompt_optional_string(arguments, &["path"])?;
    let symbol = prompt_optional_string(arguments, &["symbol"])?;
    let diff = prompt_optional_string(arguments, &["diff"])?;
    let diff_staged = prompt_optional_bool(arguments, &["diffStaged", "diff_staged"])?;
    let budget_tokens = optional_u32(arguments, &["budgetTokens", "budget_tokens"])?
        .map_or_else(|| "1200".to_string(), |value| value.to_string());

    let mut selector_args = Vec::new();
    if let Some(path) = path {
        selector_args.push(format!("--path {path:?}"));
    }
    if let Some(symbol) = symbol {
        selector_args.push(format!("--symbol {symbol:?}"));
    }
    if let Some(diff) = diff {
        selector_args.push(format!("--diff {diff:?}"));
    }
    if diff_staged {
        selector_args.push("--diff-staged".to_string());
    }
    let selectors = if selector_args.is_empty() {
        "<add --path, --symbol, --diff, or --diff-staged>".to_string()
    } else {
        selector_args.join(" ")
    };

    Ok(format!(
        "Recall relevant ee memories before editing the selected code surface.\n\nRecommended command:\n`ee recall {selectors} --workspace {workspace} --budget-tokens {budget_tokens} --json`\n\nInspect `data.recall.items[]`, provenance, stale-anchor repair hints, and `degraded[]` before changing files. If no selector was supplied, choose the narrowest path, symbol, or diff selector that matches the edit."
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
        McpPrompt::PreEditRecall => render_pre_edit_recall_prompt(arguments),
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
                    "ee pack",
                    "Assemble a task-specific context pack through ee pack --json"
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

fn workspace_only_tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "workspace": {
                "type": "string",
                "description": "Workspace path (defaults to current directory)"
            }
        }
    })
}

fn search_tool_schema() -> Value {
    json!({
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
    })
}

fn context_tool_schema() -> Value {
    json!({
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
                "enum": ["compact", "balanced", "grounding", "orientation", "thorough", "submodular"]
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
    })
}

fn recall_tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "workspace": {
                "type": "string",
                "description": "Workspace path"
            },
            "path": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Workspace-relative path or fnmatch-style glob selector; repeatable"
            },
            "symbol": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Exact symbol-name selector; repeatable"
            },
            "diff": {
                "type": "string",
                "description": "Recall against changed paths from git diff <REF>"
            },
            "diffStaged": {
                "type": "boolean",
                "description": "Recall against staged changed paths"
            },
            "kind": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Memory kind filters; repeatable"
            },
            "level": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Memory level filters; repeatable"
            },
            "stale": {
                "type": "boolean",
                "description": "Keep only suspect/stale anchored items"
            },
            "budgetTokens": {
                "type": "integer",
                "description": "Token budget for recalled items"
            },
            "cursor": {
                "type": "string",
                "description": "Resume a budget-truncated recall page"
            },
            "database": {
                "type": "string",
                "description": "Database path override"
            }
        }
    })
}

fn ask_tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "question": {
                "type": "string",
                "description": "Question to answer extractively from stored memories"
            },
            "workspace": {
                "type": "string",
                "description": "Workspace path"
            },
            "limitEvidence": {
                "type": "integer",
                "description": "Maximum evidence spans to include"
            },
            "minConfidence": {
                "type": "number",
                "description": "Minimum confidence threshold"
            },
            "requireConfidence": {
                "type": "number",
                "description": "Fail-closed threshold; abstention still appears in the ee error payload"
            },
            "database": {
                "type": "string",
                "description": "Database path override"
            }
        },
        "required": ["question"]
    })
}

fn primer_tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "workspace": {
                "type": "string",
                "description": "Workspace path"
            },
            "tokens": {
                "type": "integer",
                "description": "Token budget for the assembled primer"
            },
            "refresh": {
                "type": "boolean",
                "description": "Force deterministic reassembly instead of using the cache"
            },
            "noPersist": {
                "type": "boolean",
                "description": "Assemble without writing rebuildable primer_cache rows"
            },
            "database": {
                "type": "string",
                "description": "Database path override"
            }
        }
    })
}

fn insights_tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "workspace": {
                "type": "string",
                "description": "Workspace path"
            },
            "section": {
                "type": "string",
                "description": "Optional insight section name"
            },
            "limit": {
                "type": "integer",
                "description": "Maximum section items to return"
            },
            "offset": {
                "type": "integer",
                "description": "Section item offset"
            },
            "explain": {
                "type": "string",
                "description": "Optional memory ID to frame the insights bundle around"
            }
        }
    })
}

fn proximity_tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "memoryIdA": {
                "type": "string",
                "description": "First memory ID"
            },
            "memoryIdB": {
                "type": "string",
                "description": "Second memory ID"
            },
            "workspace": {
                "type": "string",
                "description": "Workspace path"
            },
            "database": {
                "type": "string",
                "description": "Database path override"
            },
            "minWeight": {
                "type": "number",
                "description": "Minimum link weight to include"
            },
            "minConfidence": {
                "type": "number",
                "description": "Minimum link confidence to include"
            },
            "linkLimit": {
                "type": "integer",
                "description": "Maximum memory links to process"
            },
            "includeTombstoned": {
                "type": "boolean",
                "description": "Include tombstoned memory nodes"
            }
        },
        "required": ["memoryIdA", "memoryIdB"]
    })
}

fn pack_dna_explain_tool_schema() -> Value {
    json!({
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
                "description": "Token budget"
            },
            "candidatePool": {
                "type": "integer",
                "description": "Maximum candidate memories before packing"
            },
            "profile": {
                "type": "string",
                "description": "Context profile",
                "enum": ["compact", "balanced", "grounding", "orientation", "thorough", "submodular"]
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
    })
}

fn revision_impact_tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "memoryId": {
                "type": "string",
                "description": "Memory ID to analyze"
            },
            "workspace": {
                "type": "string",
                "description": "Workspace path"
            },
            "database": {
                "type": "string",
                "description": "Database path override"
            }
        },
        "required": ["memoryId"]
    })
}

fn memory_show_tool_schema() -> Value {
    json!({
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
            }
        },
        "required": ["memoryId"]
    })
}

fn why_tool_schema() -> Value {
    json!({
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
    })
}

fn remember_tool_schema() -> Value {
    json!({
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
    })
}

fn outcome_tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "targetId": {
                "type": "string",
                "description": "Target ID to receive feedback"
            },
            "batch": {
                "type": "boolean",
                "description": "Request outcome JSONL batch mode; MCP currently rejects this because tools/call has no stdin stream"
            },
            "pack": {
                "type": "string",
                "description": "Persisted pack ID used with item to resolve the target memory"
            },
            "item": {
                "type": "integer",
                "description": "1-based pack item rank to grade; requires pack"
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
        "required": ["signal"]
    })
}

fn journal_append_tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "text": {
                "type": "string",
                "description": "Observation text to append"
            },
            "workspace": {
                "type": "string",
                "description": "Workspace path"
            },
            "kind": {
                "type": "string",
                "description": "Entry kind: observation, command_failure, surprise, or note"
            },
            "source": {
                "type": "string",
                "description": "Append source: hook, manual, or stdin"
            },
            "cmd": {
                "type": "string",
                "description": "Failing command line for the structured sidecar"
            },
            "exitCode": {
                "type": "integer",
                "description": "Exit code of the failing command"
            },
            "cwd": {
                "type": "string",
                "description": "Working directory of the failing command"
            },
            "path": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Touched paths for the structured sidecar"
            },
            "stderrTail": {
                "type": "string",
                "description": "Trailing stderr excerpt for the structured sidecar"
            },
            "session": {
                "type": "string",
                "description": "Session or run key for later scoped distillation"
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
        "required": ["text"]
    })
}

fn decide_record_tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "topic": {
                "type": "string",
                "description": "Decision topic; normalized to prevent accidental forks"
            },
            "chosen": {
                "type": "string",
                "description": "Chosen option"
            },
            "alternative": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Rejected options; repeatable"
            },
            "rationale": {
                "type": "string",
                "description": "Short rationale explaining why the chosen option won"
            },
            "revisitBy": {
                "type": "string",
                "description": "RFC3339 timestamp or relative interval such as +90d"
            },
            "supersedes": {
                "type": "string",
                "description": "Prior decision memory ID to supersede for the same normalized topic"
            },
            "actor": {
                "type": "string",
                "description": "Actor recorded in lifecycle/audit side effects"
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
        "required": ["topic", "chosen", "alternative", "rationale"]
    })
}

fn decide_list_tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "workspace": {
                "type": "string",
                "description": "Workspace path"
            },
            "about": {
                "type": "string",
                "description": "Case-insensitive substring to match against decision fields"
            },
            "includeSuperseded": {
                "type": "boolean",
                "description": "Include superseded decisions instead of only current heads"
            },
            "limit": {
                "type": "integer",
                "description": "Maximum decisions to return"
            },
            "database": {
                "type": "string",
                "description": "Database path override"
            }
        }
    })
}

fn decide_revisit_tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "workspace": {
                "type": "string",
                "description": "Workspace path"
            },
            "warningDays": {
                "type": "integer",
                "description": "Override the configured near-due warning window in days"
            },
            "limit": {
                "type": "integer",
                "description": "Maximum decisions to return"
            },
            "database": {
                "type": "string",
                "description": "Database path override"
            }
        }
    })
}

fn mesh_discovery_policy_tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "workspace": {
                "type": "string",
                "description": "Workspace path"
            },
            "database": {
                "type": "string",
                "description": "Database path override"
            },
            "operation": {
                "type": "string",
                "description": "Operation to run",
                "enum": ["inspect", "set", "allow", "deny"]
            },
            "discoveryMode": {
                "type": "string",
                "description": "Mode for operation=set",
                "enum": ["service_tag", "auto_admit", "allowlist"]
            },
            "respondMode": {
                "type": "string",
                "description": "Responder mode for operation=set",
                "enum": ["service_tag", "auto_admit", "allowlist"]
            },
            "nodeKey": {
                "type": "string",
                "description": "Tailscale node key for allow/deny operations"
            },
            "explain": {
                "type": "boolean",
                "description": "Include effectiveDecisionPreview"
            },
            "allowWrite": {
                "type": "boolean",
                "description": "Required and true for set/allow/deny"
            }
        }
    })
}

fn mcp_tool_annotations_value(annotations: McpToolAnnotations) -> Value {
    json!({
        "readOnlyHint": annotations.read_only,
        "destructiveHint": annotations.destructive,
        "idempotentHint": annotations.idempotent,
        "openWorldHint": annotations.open_world
    })
}

fn mcp_tool_effect_value(effect: McpToolEffect) -> Value {
    json!({
        "kind": effect.kind,
        "writeSurface": effect.write_surface,
        "defaultDryRun": effect.default_dry_run,
        "requiresAllowWriteWhenDryRunFalse": effect.requires_allow_write_when_dry_run_false,
        "audit": effect.audit,
        "redaction": effect.redaction,
        "idempotency": effect.idempotency,
        "destructive": effect.destructive
    })
}

fn mcp_tool_descriptor(tool: &McpToolEntry) -> Value {
    let mut descriptor = json!({
        "name": tool.name,
        "description": tool.description,
        "inputSchema": (tool.input_schema)(),
        "annotations": mcp_tool_annotations_value(tool.annotations)
    });
    if let Some(effect) = tool.effect {
        if let Value::Object(fields) = &mut descriptor {
            fields.insert("eeEffect".to_string(), mcp_tool_effect_value(effect));
        }
    }
    descriptor
}

fn handle_tools_list(id: Value) -> Value {
    let tools: Vec<Value> = TOOL_REGISTRY.iter().map(mcp_tool_descriptor).collect();
    json_rpc_result(id, json!({ "tools": tools }))
}

fn find_argument<'a>(arguments: &'a Value, names: &[&str]) -> Option<&'a Value> {
    names.iter().find_map(|name| arguments.get(*name))
}

fn argument_name<'name>(names: &[&'name str]) -> Result<&'name str, String> {
    names
        .first()
        .copied()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "MCP argument helper requires at least one argument name".to_owned())
}

fn required_string<'a>(arguments: &'a Value, names: &[&str]) -> Result<&'a str, String> {
    let name = argument_name(names)?;
    let Some(value) = find_argument(arguments, names) else {
        return Err(format!("Missing required argument '{name}'"));
    };
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Argument '{name}' must be a non-empty string"))
}

fn optional_string<'a>(arguments: &'a Value, names: &[&str]) -> Result<Option<&'a str>, String> {
    let name = argument_name(names)?;
    let Some(value) = find_argument(arguments, names) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_str()
        .map(Some)
        .ok_or_else(|| format!("Argument '{name}' must be a string"))
}

fn optional_string_list(arguments: &Value, names: &[&str]) -> Result<Vec<String>, String> {
    let name = argument_name(names)?;
    let Some(value) = find_argument(arguments, names) else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    if let Some(single) = value.as_str() {
        if single.is_empty() {
            return Ok(Vec::new());
        }
        return Ok(vec![single.to_string()]);
    }
    let Some(items) = value.as_array() else {
        return Err(format!(
            "Argument '{name}' must be a string or string array"
        ));
    };
    let mut strings = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let Some(text) = item.as_str() else {
            return Err(format!("Argument '{name}' item {index} must be a string"));
        };
        if text.is_empty() {
            return Err(format!("Argument '{name}' item {index} must be non-empty"));
        }
        strings.push(text.to_string());
    }
    Ok(strings)
}

fn required_string_list(arguments: &Value, names: &[&str]) -> Result<Vec<String>, String> {
    let name = argument_name(names)?;
    let values = optional_string_list(arguments, names)?;
    if values.is_empty() {
        return Err(format!("Missing required argument '{name}'"));
    }
    Ok(values)
}

fn optional_bool(arguments: &Value, names: &[&str]) -> Result<bool, String> {
    let name = argument_name(names)?;
    let Some(value) = find_argument(arguments, names) else {
        return Ok(false);
    };
    if value.is_null() {
        return Ok(false);
    }
    value
        .as_bool()
        .ok_or_else(|| format!("Argument '{name}' must be a boolean"))
}

fn optional_bool_with_default(
    arguments: &Value,
    names: &[&str],
    default: bool,
) -> Result<bool, String> {
    let name = argument_name(names)?;
    let Some(value) = find_argument(arguments, names) else {
        return Ok(default);
    };
    if value.is_null() {
        return Ok(default);
    }
    value
        .as_bool()
        .ok_or_else(|| format!("Argument '{name}' must be a boolean"))
}

fn optional_u32(arguments: &Value, names: &[&str]) -> Result<Option<u32>, String> {
    let name = argument_name(names)?;
    let Some(value) = find_argument(arguments, names) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(raw) = value.as_u64() else {
        return Err(format!("Argument '{name}' must be a non-negative integer"));
    };
    u32::try_from(raw)
        .map(Some)
        .map_err(|_| format!("Argument '{name}' is too large"))
}

fn optional_number_string(arguments: &Value, names: &[&str]) -> Result<Option<String>, String> {
    let name = argument_name(names)?;
    let Some(value) = find_argument(arguments, names) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(raw) = value.as_f64() else {
        return Err(format!("Argument '{name}' must be a number"));
    };
    if !raw.is_finite() {
        return Err(format!("Argument '{name}' must be finite"));
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

fn append_optional_string_list_flag(
    args: &mut Vec<OsString>,
    arguments: &Value,
    names: &[&str],
    flag: &str,
) -> Result<(), String> {
    for value in optional_string_list(arguments, names)? {
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

fn build_health_tool_args(args: &mut Vec<OsString>, _arguments: &Value) -> Result<(), String> {
    push_arg(args, "health");
    Ok(())
}

fn build_status_tool_args(args: &mut Vec<OsString>, _arguments: &Value) -> Result<(), String> {
    push_arg(args, "status");
    Ok(())
}

fn build_doctor_tool_args(args: &mut Vec<OsString>, _arguments: &Value) -> Result<(), String> {
    push_arg(args, "doctor");
    Ok(())
}

fn build_capabilities_tool_args(
    args: &mut Vec<OsString>,
    _arguments: &Value,
) -> Result<(), String> {
    push_arg(args, "capabilities");
    Ok(())
}

fn build_search_tool_args(args: &mut Vec<OsString>, arguments: &Value) -> Result<(), String> {
    push_arg(args, "search");
    push_arg(args, required_string(arguments, &["query"])?);
    if let Some(limit) = optional_u32(arguments, &["limit"])? {
        push_arg(args, "--limit");
        push_arg(args, limit.to_string());
    }
    append_optional_path_flag(args, arguments, &["database"], "--database")?;
    append_optional_path_flag(args, arguments, &["indexDir", "index_dir"], "--index-dir")?;
    if optional_bool(arguments, &["explain"])? {
        push_arg(args, "--explain");
    }
    Ok(())
}

fn build_context_tool_args(args: &mut Vec<OsString>, arguments: &Value) -> Result<(), String> {
    push_arg(args, "pack");
    push_arg(args, required_string(arguments, &["query"])?);
    if let Some(max_tokens) = optional_u32(arguments, &["maxTokens", "max_tokens"])? {
        push_arg(args, "--max-tokens");
        push_arg(args, max_tokens.to_string());
    }
    if let Some(candidate_pool) = optional_u32(arguments, &["candidatePool", "candidate_pool"])? {
        push_arg(args, "--candidate-pool");
        push_arg(args, candidate_pool.to_string());
    }
    if let Some(profile) = optional_string(arguments, &["profile"])? {
        push_arg(args, "--profile");
        push_arg(args, parse_mcp_context_profile(profile)?);
    }
    append_optional_path_flag(args, arguments, &["database"], "--database")?;
    append_optional_path_flag(args, arguments, &["indexDir", "index_dir"], "--index-dir")?;
    Ok(())
}

fn build_recall_tool_args(args: &mut Vec<OsString>, arguments: &Value) -> Result<(), String> {
    push_arg(args, "recall");
    append_optional_string_list_flag(args, arguments, &["path", "paths"], "--path")?;
    append_optional_string_list_flag(args, arguments, &["symbol", "symbols"], "--symbol")?;
    append_optional_string_flag(args, arguments, &["diff"], "--diff")?;
    if optional_bool(arguments, &["diffStaged", "diff_staged"])? {
        push_arg(args, "--diff-staged");
    }
    append_optional_string_list_flag(args, arguments, &["kind", "kinds"], "--kind")?;
    append_optional_string_list_flag(args, arguments, &["level", "levels"], "--level")?;
    if optional_bool(arguments, &["stale"])? {
        push_arg(args, "--stale");
    }
    if let Some(budget_tokens) = optional_u32(arguments, &["budgetTokens", "budget_tokens"])? {
        push_arg(args, "--budget-tokens");
        push_arg(args, budget_tokens.to_string());
    }
    append_optional_string_flag(args, arguments, &["cursor"], "--cursor")?;
    append_optional_path_flag(args, arguments, &["database"], "--database")?;
    Ok(())
}

fn build_ask_tool_args(args: &mut Vec<OsString>, arguments: &Value) -> Result<(), String> {
    push_arg(args, "ask");
    push_arg(args, required_string(arguments, &["question"])?);
    if let Some(limit_evidence) = optional_u32(arguments, &["limitEvidence", "limit_evidence"])? {
        push_arg(args, "--limit-evidence");
        push_arg(args, limit_evidence.to_string());
    }
    append_optional_number_flag(
        args,
        arguments,
        &["minConfidence", "min_confidence"],
        "--min-confidence",
    )?;
    append_optional_number_flag(
        args,
        arguments,
        &["requireConfidence", "require_confidence"],
        "--require-confidence",
    )?;
    append_optional_path_flag(args, arguments, &["database"], "--database")?;
    Ok(())
}

fn build_primer_tool_args(args: &mut Vec<OsString>, arguments: &Value) -> Result<(), String> {
    push_arg(args, "primer");
    if let Some(tokens) = optional_u32(arguments, &["tokens"])? {
        push_arg(args, "--tokens");
        push_arg(args, tokens.to_string());
    }
    if optional_bool(arguments, &["refresh"])? {
        push_arg(args, "--refresh");
    }
    if optional_bool(arguments, &["noPersist", "no_persist"])? {
        push_arg(args, "--no-persist");
    }
    append_optional_path_flag(args, arguments, &["database"], "--database")?;
    Ok(())
}

fn build_insights_tool_args(args: &mut Vec<OsString>, arguments: &Value) -> Result<(), String> {
    push_arg(args, "insights");
    let section = optional_string(arguments, &["section"])?;
    let explain = optional_string(arguments, &["explain"])?;
    if section.is_some() && explain.is_some() {
        return Err("Tool ee_insights accepts either section or explain, not both".to_string());
    }
    if let Some(section) = section {
        push_arg(args, "--section");
        push_arg(args, section);
    }
    if let Some(limit) = optional_u32(arguments, &["limit"])? {
        push_arg(args, "--limit");
        push_arg(args, limit.to_string());
    }
    if let Some(offset) = optional_u32(arguments, &["offset"])? {
        push_arg(args, "--offset");
        push_arg(args, offset.to_string());
    }
    if let Some(explain) = explain {
        push_arg(args, "--explain");
        push_arg(args, explain);
    }
    Ok(())
}

fn build_proximity_tool_args(args: &mut Vec<OsString>, arguments: &Value) -> Result<(), String> {
    push_arg(args, "proximity");
    push_arg(
        args,
        required_string(arguments, &["memoryIdA", "memory_id_a"])?,
    );
    push_arg(
        args,
        required_string(arguments, &["memoryIdB", "memory_id_b"])?,
    );
    append_optional_path_flag(args, arguments, &["database"], "--database")?;
    append_optional_number_flag(
        args,
        arguments,
        &["minWeight", "min_weight"],
        "--min-weight",
    )?;
    append_optional_number_flag(
        args,
        arguments,
        &["minConfidence", "min_confidence"],
        "--min-confidence",
    )?;
    if let Some(limit) = optional_u32(arguments, &["linkLimit", "link_limit"])? {
        push_arg(args, "--link-limit");
        push_arg(args, limit.to_string());
    }
    if optional_bool(arguments, &["includeTombstoned", "include_tombstoned"])? {
        push_arg(args, "--include-tombstoned");
    }
    Ok(())
}

fn build_pack_dna_explain_tool_args(
    args: &mut Vec<OsString>,
    arguments: &Value,
) -> Result<(), String> {
    build_context_tool_args(args, arguments)?;
    push_arg(args, "--explain");
    Ok(())
}

fn build_revision_impact_tool_args(
    args: &mut Vec<OsString>,
    arguments: &Value,
) -> Result<(), String> {
    push_arg(args, "memory");
    push_arg(args, "revise");
    push_arg(
        args,
        required_string(arguments, &["memoryId", "memory_id"])?,
    );
    append_optional_path_flag(args, arguments, &["database"], "--database")?;
    push_arg(args, "--content");
    push_arg(
        args,
        "ee-mcp-revision-impact-probe-00000000000000000000000000000000",
    );
    push_arg(args, "--reason");
    push_arg(args, "mcp revision impact read-only probe");
    push_arg(args, "--dry-run");
    Ok(())
}

fn build_memory_show_tool_args(args: &mut Vec<OsString>, arguments: &Value) -> Result<(), String> {
    push_arg(args, "memory");
    push_arg(args, "show");
    push_arg(
        args,
        required_string(arguments, &["memoryId", "memory_id"])?,
    );
    append_optional_path_flag(args, arguments, &["database"], "--database")?;
    Ok(())
}

fn build_why_tool_args(args: &mut Vec<OsString>, arguments: &Value) -> Result<(), String> {
    push_arg(args, "why");
    push_arg(
        args,
        required_string(arguments, &["memoryId", "memory_id"])?,
    );
    append_optional_path_flag(args, arguments, &["database"], "--database")?;
    if let Some(threshold) =
        optional_number_string(arguments, &["confidenceThreshold", "confidence_threshold"])?
    {
        push_arg(args, "--confidence-threshold");
        push_arg(args, threshold);
    }
    Ok(())
}

fn build_remember_tool_args(args: &mut Vec<OsString>, arguments: &Value) -> Result<(), String> {
    let dry_run = gated_write_dry_run("ee_remember", arguments)?;
    push_arg(args, "remember");
    push_arg(args, required_string(arguments, &["content"])?);
    append_optional_string_flag(args, arguments, &["level"], "--level")?;
    append_optional_string_flag(args, arguments, &["kind"], "--kind")?;
    append_optional_string_flag(args, arguments, &["tags"], "--tags")?;
    append_optional_number_flag(args, arguments, &["confidence"], "--confidence")?;
    append_optional_string_flag(args, arguments, &["source"], "--source")?;
    append_optional_string_flag(
        args,
        arguments,
        &["validFrom", "valid_from"],
        "--valid-from",
    )?;
    append_optional_string_flag(args, arguments, &["validTo", "valid_to"], "--valid-to")?;
    if dry_run {
        push_arg(args, "--dry-run");
    }
    Ok(())
}

fn build_outcome_tool_args(args: &mut Vec<OsString>, arguments: &Value) -> Result<(), String> {
    let dry_run = gated_write_dry_run("ee_outcome", arguments)?;
    if optional_bool(arguments, &["batch"])? {
        return Err(
            "Tool ee_outcome cannot use --batch because MCP tools/call has no stdin stream"
                .to_string(),
        );
    }
    let target_id = optional_string(arguments, &["targetId", "target_id"])?;
    let pack = optional_string(arguments, &["pack"])?;
    let item = optional_u32(arguments, &["item"])?;
    if target_id.is_none() && (pack.is_none() || item.is_none()) {
        return Err("Tool ee_outcome requires targetId or both pack and item".to_string());
    }
    push_arg(args, "outcome");
    if let Some(target_id) = target_id {
        push_arg(args, target_id);
    }
    if let Some(pack) = pack {
        push_arg(args, "--pack");
        push_arg(args, pack);
    }
    if let Some(item) = item {
        push_arg(args, "--item");
        push_arg(args, item.to_string());
    }
    append_optional_string_flag(
        args,
        arguments,
        &["targetType", "target_type"],
        "--target-type",
    )?;
    append_optional_string_flag(
        args,
        arguments,
        &["workspaceId", "workspace_id"],
        "--workspace-id",
    )?;
    push_arg(args, "--signal");
    push_arg(args, required_string(arguments, &["signal"])?);
    append_optional_number_flag(args, arguments, &["weight"], "--weight")?;
    append_optional_string_flag(
        args,
        arguments,
        &["sourceType", "source_type"],
        "--source-type",
    )?;
    append_optional_string_flag(args, arguments, &["sourceId", "source_id"], "--source-id")?;
    append_optional_string_flag(args, arguments, &["reason"], "--reason")?;
    append_optional_string_flag(
        args,
        arguments,
        &["evidenceJson", "evidence_json"],
        "--evidence-json",
    )?;
    append_optional_string_flag(
        args,
        arguments,
        &["sessionId", "session_id"],
        "--session-id",
    )?;
    append_optional_string_flag(args, arguments, &["eventId", "event_id"], "--event-id")?;
    append_optional_string_flag(args, arguments, &["actor"], "--actor")?;
    append_optional_path_flag(args, arguments, &["database"], "--database")?;
    if dry_run {
        push_arg(args, "--dry-run");
    }
    Ok(())
}

fn build_journal_append_tool_args(
    args: &mut Vec<OsString>,
    arguments: &Value,
) -> Result<(), String> {
    let dry_run = gated_write_dry_run("ee_journal_append", arguments)?;
    if dry_run {
        return Err(
            "Tool ee_journal_append dryRun=true requires ee journal append --dry-run support"
                .to_string(),
        );
    }
    push_arg(args, "journal");
    push_arg(args, "append");
    push_arg(args, required_string(arguments, &["text"])?);
    append_optional_string_flag(args, arguments, &["kind"], "--kind")?;
    append_optional_string_flag(args, arguments, &["source"], "--source")?;
    append_optional_string_flag(args, arguments, &["cmd"], "--cmd")?;
    if let Some(exit_code) = optional_number_string(arguments, &["exitCode", "exit_code"])? {
        push_arg(args, "--exit-code");
        push_arg(args, exit_code);
    }
    append_optional_string_flag(args, arguments, &["cwd"], "--cwd")?;
    append_optional_string_list_flag(args, arguments, &["path", "paths"], "--path")?;
    append_optional_string_flag(
        args,
        arguments,
        &["stderrTail", "stderr_tail"],
        "--stderr-tail",
    )?;
    append_optional_string_flag(args, arguments, &["session"], "--session")?;
    append_optional_path_flag(args, arguments, &["database"], "--database")?;
    Ok(())
}

fn build_decide_record_tool_args(
    args: &mut Vec<OsString>,
    arguments: &Value,
) -> Result<(), String> {
    let dry_run = gated_write_dry_run("ee_decide_record", arguments)?;
    push_arg(args, "decide");
    push_arg(args, "record");
    push_arg(args, required_string(arguments, &["topic"])?);
    push_arg(args, "--chosen");
    push_arg(args, required_string(arguments, &["chosen"])?);
    for alternative in required_string_list(arguments, &["alternative", "alternatives"])? {
        push_arg(args, "--alternative");
        push_arg(args, alternative);
    }
    push_arg(args, "--rationale");
    push_arg(args, required_string(arguments, &["rationale"])?);
    append_optional_string_flag(
        args,
        arguments,
        &["revisitBy", "revisit_by"],
        "--revisit-by",
    )?;
    append_optional_string_flag(args, arguments, &["supersedes"], "--supersedes")?;
    append_optional_string_flag(args, arguments, &["actor"], "--actor")?;
    append_optional_path_flag(args, arguments, &["database"], "--database")?;
    if dry_run {
        push_arg(args, "--dry-run");
    }
    Ok(())
}

fn build_decide_list_tool_args(args: &mut Vec<OsString>, arguments: &Value) -> Result<(), String> {
    push_arg(args, "decide");
    push_arg(args, "list");
    append_optional_string_flag(args, arguments, &["about"], "--about")?;
    if optional_bool(arguments, &["includeSuperseded", "include_superseded"])? {
        push_arg(args, "--include-superseded");
    }
    if let Some(limit) = optional_u32(arguments, &["limit"])? {
        push_arg(args, "--limit");
        push_arg(args, limit.to_string());
    }
    append_optional_path_flag(args, arguments, &["database"], "--database")?;
    Ok(())
}

fn build_decide_revisit_tool_args(
    args: &mut Vec<OsString>,
    arguments: &Value,
) -> Result<(), String> {
    push_arg(args, "decide");
    push_arg(args, "revisit");
    if let Some(warning_days) = optional_u32(arguments, &["warningDays", "warning_days"])? {
        push_arg(args, "--warning-days");
        push_arg(args, warning_days.to_string());
    }
    if let Some(limit) = optional_u32(arguments, &["limit"])? {
        push_arg(args, "--limit");
        push_arg(args, limit.to_string());
    }
    append_optional_path_flag(args, arguments, &["database"], "--database")?;
    Ok(())
}

fn build_mesh_discovery_policy_tool_args(
    args: &mut Vec<OsString>,
    arguments: &Value,
) -> Result<(), String> {
    push_arg(args, "mesh");
    push_arg(args, "discovery-policy");
    append_optional_path_flag(args, arguments, &["database"], "--database")?;
    if optional_bool(arguments, &["explain"])? {
        push_arg(args, "--explain");
    }

    let operation = optional_string(arguments, &["operation"])?.unwrap_or("inspect");
    match operation {
        "inspect" => Ok(()),
        "set" => {
            require_mesh_discovery_policy_allow_write(arguments, operation)?;
            push_arg(args, "set");
            append_optional_string_flag(
                args,
                arguments,
                &["discoveryMode", "discovery_mode"],
                "--discovery-mode",
            )?;
            append_optional_string_flag(
                args,
                arguments,
                &["respondMode", "respond_mode"],
                "--respond-mode",
            )?;
            Ok(())
        }
        "allow" | "deny" => {
            require_mesh_discovery_policy_allow_write(arguments, operation)?;
            push_arg(args, operation);
            push_arg(args, required_string(arguments, &["nodeKey", "node_key"])?);
            Ok(())
        }
        other => Err(format!(
            "Invalid mesh discovery policy operation '{other}'. Expected inspect, set, allow, or deny."
        )),
    }
}

fn require_mesh_discovery_policy_allow_write(
    arguments: &Value,
    operation: &str,
) -> Result<(), String> {
    if optional_bool(arguments, &["allowWrite", "allow_write"])? {
        Ok(())
    } else {
        Err(format!(
            "Write tool ee_mesh_discovery_policy operation {operation} requires allowWrite=true"
        ))
    }
}

fn build_cli_args_for_tool(
    tool: &McpToolEntry,
    arguments: &Value,
) -> Result<Vec<OsString>, String> {
    let mut args = Vec::new();
    append_common_json_args(&mut args, arguments)?;
    (tool.args_builder)(&mut args, arguments)?;
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

fn query_parameter_name<'name>(names: &[&'name str]) -> Result<&'name str, String> {
    names
        .first()
        .copied()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "MCP URI query helper requires at least one query parameter name".to_owned())
}

fn required_query_param(query: &[(String, String)], names: &[&str]) -> Result<String, String> {
    let name = query_parameter_name(names)?;
    let Some(value) = query_param(query, names) else {
        return Err(format!("Missing required URI query parameter '{name}'"));
    };
    if value.is_empty() {
        return Err(format!("URI query parameter '{name}' must not be empty"));
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
            push_arg(&mut args, "pack");
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
        }
        _ => return Err(format!("Unknown ee resource URI: {uri}")),
    }

    Ok(args)
}

fn run_cli_tool(process: &mut McpProcess, args: Vec<OsString>) -> McpCliRunResult {
    let max_bytes = mcp_stdio_byte_limit();
    let mut stdout = LimitedCapture::new(max_bytes);
    let mut stderr = LimitedCapture::new(max_bytes);
    let exit = process.cli.run(args, &mut stdout, &mut stderr);
    let stdout_bytes_seen = stdout.bytes_seen;
    let stderr_bytes_seen = stderr.bytes_seen;
    let truncated = stdout.truncated || stderr.truncated;
    McpCliRunResult {
        exit,
        stdout: stdout.into_string(),
        stderr: stderr.into_string(),
        stdout_bytes_seen,
        stderr_bytes_seen,
        truncated,
    }
}

fn redact_mcp_public_diagnostics(value: &str) -> String {
    crate::output::jsonl_export::redact_content(value, RedactionLevel::Standard)
}

fn cli_output_size_limit_error(id: Value, run: &McpCliRunResult) -> Option<Value> {
    if !run.truncated {
        return None;
    }
    let actual_bytes = run.stdout_bytes_seen.max(run.stderr_bytes_seen);
    Some(mcp_size_limit_exceeded_error(
        Some(id),
        "response",
        actual_bytes,
        mcp_stdio_byte_limit(),
    ))
}

fn resource_read_result(
    id: Value,
    uri: &str,
    exit: ProcessExitCode,
    stdout: String,
    stderr: String,
) -> Value {
    let redacted_stderr = redact_mcp_public_diagnostics(&stderr);

    if exit != ProcessExitCode::Success {
        let message = if redacted_stderr.is_empty() {
            format!("Resource read failed with exit code {}", exit as u8)
        } else {
            redacted_stderr
        };
        return json_rpc_error(Some(id), -32603, &message);
    }

    let text = if stdout.is_empty() {
        redacted_stderr.as_str()
    } else {
        stdout.as_str()
    };
    json_rpc_result(
        id,
        json!({
            "contents": [{
                "uri": uri,
                "mimeType": "application/json",
                "text": text
            }]
        }),
    )
}

#[cfg(test)]
fn handle_resources_read(id: Value, params: Option<&Value>) -> Value {
    handle_resources_read_in_process(&mut McpProcess::default(), id, params)
}

fn handle_resources_read_in_process(
    process: &mut McpProcess,
    id: Value,
    params: Option<&Value>,
) -> Value {
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
    let run = run_cli_tool(process, cli_args);
    if let Some(error) = cli_output_size_limit_error(id.clone(), &run) {
        return error;
    }
    resource_read_result(id, uri, run.exit, run.stdout, run.stderr)
}

fn cli_tool_result(id: Value, exit: ProcessExitCode, stdout: String, stderr: String) -> Value {
    let redacted_stderr = redact_mcp_public_diagnostics(&stderr);
    let text = if stdout.is_empty() {
        redacted_stderr.as_str()
    } else {
        stdout.as_str()
    };
    json_rpc_result(
        id,
        json!({
            "content": [{
                "type": "text",
                "text": text
            }],
            "isError": exit != ProcessExitCode::Success,
            "exitCode": exit as u8,
            "stderr": redacted_stderr
        }),
    )
}

fn extract_mcp_tool_payload(tool_name: &str, stdout: &str) -> Result<Option<String>, String> {
    let pointer = match tool_name {
        "ee_pack_dna_explain" => "/data/pack/packDna",
        "ee_revision_impact" => "/data/impactAnalysis",
        _ => return Ok(None),
    };
    let value: Value = serde_json::from_str(stdout).map_err(|error| {
        format!("Tool {tool_name} produced invalid JSON while extracting {pointer}: {error}")
    })?;
    let payload = value
        .pointer(pointer)
        .ok_or_else(|| format!("Tool {tool_name} response missing {pointer}"))?;
    serde_json::to_string(payload)
        .map(|json| Some(format!("{json}\n")))
        .map_err(|error| format!("Tool {tool_name} failed to serialize {pointer}: {error}"))
}

#[cfg(test)]
fn handle_tools_call(id: Value, params: Option<&Value>) -> Value {
    handle_tools_call_in_process(&mut McpProcess::default(), id, params)
}

fn handle_tools_call_in_process(
    process: &mut McpProcess,
    id: Value,
    params: Option<&Value>,
) -> Value {
    let Some(params) = params else {
        return json_rpc_error(Some(id), -32602, "Missing params");
    };

    let tool_name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let Some(tool) = mcp_tool_entry(tool_name) else {
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
    let run = run_cli_tool(process, cli_args);
    if let Some(error) = cli_output_size_limit_error(id.clone(), &run) {
        return error;
    }
    if run.exit == ProcessExitCode::Success {
        match extract_mcp_tool_payload(tool.name, &run.stdout) {
            Ok(Some(payload)) => return cli_tool_result(id, run.exit, payload, run.stderr),
            Ok(None) => {}
            Err(message) => return json_rpc_error(Some(id), -32603, &message),
        }
    }
    cli_tool_result(id, run.exit, run.stdout, run.stderr)
}

fn handle_shutdown(id: Value) -> Value {
    json_rpc_result(id, json!({}))
}

fn is_json_rpc_notification(request: &Value) -> bool {
    // Per JSON-RPC 2.0 §4: "A Notification is a Request object without
    // an 'id' member." Require an object so that bare arrays/scalars do
    // not get treated as notifications when they reach this helper
    // (they are malformed requests and must receive an error reply).
    request.is_object() && request.get("id").is_none()
}

fn is_valid_json_rpc_id(id: &Value) -> bool {
    id.is_string() || id.is_number() || id.is_null()
}

fn json_rpc_error_id(request: &Value) -> Option<Value> {
    request
        .get("id")
        .filter(|id| is_valid_json_rpc_id(id))
        .cloned()
}

fn validate_json_rpc_request(request: &Value) -> Result<&str, Value> {
    if !request.is_object() {
        return Err(json_rpc_error(
            None,
            -32600,
            "Invalid Request: request must be a JSON object",
        ));
    }
    let id = json_rpc_error_id(request);
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(json_rpc_error(
            id,
            -32600,
            "Invalid Request: jsonrpc must be \"2.0\"",
        ));
    }
    if request
        .get("id")
        .is_some_and(|id| !is_valid_json_rpc_id(id))
    {
        return Err(json_rpc_error(
            None,
            -32600,
            "Invalid Request: id must be a string, number, or null",
        ));
    }
    let id = request.get("id").cloned();
    match request.get("method") {
        Some(Value::String(method)) if !method.is_empty() => Ok(method),
        Some(_) => Err(json_rpc_error(
            id,
            -32600,
            "Invalid Request: method must be a non-empty string",
        )),
        None => Err(json_rpc_error(
            id,
            -32600,
            "Invalid Request: method is required",
        )),
    }
}

#[must_use]
pub fn handle_json_rpc_message(request: &Value) -> Option<Value> {
    handle_json_rpc_message_in_process(&mut McpProcess::default(), request)
}

fn handle_json_rpc_message_in_process(process: &mut McpProcess, request: &Value) -> Option<Value> {
    trace_mcp_top_level("input", 0, &[]);
    // Per JSON-RPC 2.0 §4.1: "Notifications ... MUST be processed by
    // the Server without reply or response." This applies even when
    // the notification is otherwise invalid (missing jsonrpc, wrong
    // version, empty method, ...). The pre-validation path returned
    // None for malformed-but-notification-shaped requests; the
    // post-validation path regressed by replying. Determine
    // notification status FIRST so we suppress responses for any
    // object-without-id, regardless of validation outcome. Non-object
    // requests are not notifications and continue to receive the
    // standard invalid-request error reply.
    let is_notification = is_json_rpc_notification(request);
    if let Err(error) = validate_json_rpc_request(request) {
        trace_mcp_top_level("response", 0, &[]);
        return (!is_notification).then_some(error);
    }
    if is_notification {
        trace_mcp_top_level("response", 0, &[]);
        return None;
    }

    trace_mcp_top_level("dispatch", 0, &[]);
    let response = handle_request_in_process(process, request);
    trace_mcp_top_level("response", 0, &[]);
    Some(response)
}

#[cfg(test)]
fn handle_request(request: &Value) -> Value {
    handle_request_in_process(&mut McpProcess::default(), request)
}

fn handle_request_in_process(process: &mut McpProcess, request: &Value) -> Value {
    let id = request.get("id").cloned();
    let method = match validate_json_rpc_request(request) {
        Ok(method) => method,
        Err(error) => return error,
    };
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
            handle_resources_read_in_process(process, id, params)
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
            handle_tools_call_in_process(process, id, params)
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

fn should_stop_stdio_loop_after_response(request: &Value, response: Option<&Value>) -> bool {
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || request.get("id").is_none()
        || request.get("method").and_then(Value::as_str) != Some("shutdown")
    {
        return false;
    }

    response
        .is_some_and(|response| response.get("error").is_none() && response.get("result").is_some())
}

fn read_limited_jsonl_line<R: BufRead>(
    reader: &mut R,
    max_bytes: usize,
) -> Result<Option<StdioLineRead>, String> {
    let mut bytes = Vec::new();
    let mut bytes_seen = 0usize;

    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| format!("stdin read error: {error}"))?;
        if available.is_empty() {
            if bytes_seen == 0 {
                return Ok(None);
            }
            break;
        }

        let (chunk, consume_len, done) =
            if let Some(newline_index) = available.iter().position(|byte| *byte == b'\n') {
                (&available[..newline_index], newline_index + 1, true)
            } else {
                (available, available.len(), false)
            };

        bytes_seen = bytes_seen.saturating_add(chunk.len());
        if bytes_seen <= max_bytes {
            bytes.extend_from_slice(chunk);
        } else if bytes.len() < max_bytes {
            let keep = max_bytes - bytes.len();
            bytes.extend_from_slice(&chunk[..keep]);
        }

        reader.consume(consume_len);
        if done {
            break;
        }
    }

    if bytes_seen > max_bytes {
        return Ok(Some(StdioLineRead::TooLarge(bytes_seen)));
    }

    if bytes.ends_with(b"\r") {
        bytes.pop();
    }
    match String::from_utf8(bytes) {
        Ok(line) => Ok(Some(StdioLineRead::Line(line))),
        Err(error) => Ok(Some(StdioLineRead::InvalidUtf8(error.to_string()))),
    }
}

enum StdioLineRead {
    Line(String),
    TooLarge(usize),
    InvalidUtf8(String),
}

struct StdioLineOutcome {
    response: Option<Value>,
    shutdown: bool,
}

#[cfg(test)]
fn handle_stdio_line(line: &str, max_bytes: usize) -> StdioLineOutcome {
    handle_stdio_line_in_process(&mut McpProcess::default(), line, max_bytes)
}

fn handle_stdio_line_in_process(
    process: &mut McpProcess,
    line: &str,
    max_bytes: usize,
) -> StdioLineOutcome {
    if line.as_bytes().len() > max_bytes {
        return StdioLineOutcome {
            response: Some(mcp_size_limit_exceeded_error(
                None,
                "request",
                line.len(),
                max_bytes,
            )),
            shutdown: false,
        };
    }
    if line.trim().is_empty() {
        return StdioLineOutcome {
            response: None,
            shutdown: false,
        };
    }

    let request: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(error) => {
            return StdioLineOutcome {
                response: Some(json_rpc_error(
                    None,
                    -32700,
                    &format!("Parse error: {error}"),
                )),
                shutdown: false,
            };
        }
    };
    let response = handle_json_rpc_message_in_process(process, &request);
    let shutdown = should_stop_stdio_loop_after_response(&request, response.as_ref());
    StdioLineOutcome { response, shutdown }
}

fn write_json_rpc_response<W: Write>(
    stdout: &mut W,
    response: &Value,
    max_bytes: usize,
) -> Result<(), String> {
    let response_text = response.to_string();
    let output_text = if response_text.len() > max_bytes {
        let id = response.get("id").cloned().filter(|value| !value.is_null());
        let error = mcp_size_limit_exceeded_error(id, "response", response_text.len(), max_bytes);
        let mut error_text = error.to_string();
        if error_text.len() > max_bytes {
            let compact_error =
                mcp_size_limit_exceeded_error(None, "response", response_text.len(), max_bytes);
            error_text = compact_error.to_string();
            if error_text.len() > max_bytes {
                return Err(format!(
                    "MCP size_limit_exceeded response is {} bytes, above configured cap {max_bytes}",
                    error_text.len()
                ));
            }
        }
        error_text
    } else {
        response_text
    };

    writeln!(stdout, "{output_text}").map_err(|error| format!("stdout write error: {error}"))?;
    stdout
        .flush()
        .map_err(|error| format!("stdout flush error: {error}"))
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
    let mut reader = BufReader::new(stdin.lock());
    let max_bytes = mcp_stdio_byte_limit();
    let mut process = McpProcess::default();

    eprintln!("[ee-mcp] Server starting, protocol version {MCP_PROTOCOL_VERSION}");

    while let Some(line_result) = read_limited_jsonl_line(&mut reader, max_bytes)? {
        let line = match line_result {
            StdioLineRead::Line(line) => line,
            StdioLineRead::TooLarge(actual_bytes) => {
                let error = mcp_size_limit_exceeded_error(None, "request", actual_bytes, max_bytes);
                write_json_rpc_response(&mut stdout, &error, max_bytes)?;
                continue;
            }
            StdioLineRead::InvalidUtf8(message) => {
                let error = json_rpc_error(None, -32700, &format!("Parse error: {message}"));
                write_json_rpc_response(&mut stdout, &error, max_bytes)?;
                continue;
            }
        };

        let outcome = handle_stdio_line_in_process(&mut process, &line, max_bytes);
        if let Some(response) = outcome.response {
            write_json_rpc_response(&mut stdout, &response, max_bytes)?;
        }

        if outcome.shutdown {
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
    use proptest::prelude::*;
    use proptest::test_runner::{Config as ProptestConfig, TestCaseError};
    use std::collections::{BTreeMap, BTreeSet};
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

    fn arbitrary_json_key() -> impl Strategy<Value = String> {
        proptest::collection::vec(any::<char>(), 0..16)
            .prop_map(|chars| chars.into_iter().collect())
    }

    fn json_object_value(map: BTreeMap<String, Value>) -> Value {
        Value::Object(map.into_iter().collect())
    }

    fn arbitrary_json_value() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(|number| Value::Number(number.into())),
            proptest::collection::vec(any::<char>(), 0..32)
                .prop_map(|chars| Value::String(chars.into_iter().collect())),
        ];

        leaf.prop_recursive(4, 64, 8, |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 0..8).prop_map(Value::Array),
                proptest::collection::btree_map(arbitrary_json_key(), inner, 0..8)
                    .prop_map(json_object_value),
            ]
        })
    }

    fn without_dispatching_known_mcp_method(mut request: Value) -> Value {
        let Some(object) = request.as_object_mut() else {
            return request;
        };
        if object.get("id").is_none() {
            return request;
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return request;
        };
        if matches!(McpMethod::parse(method), McpMethod::Unknown(_)) {
            return request;
        }

        object.insert(
            "method".to_owned(),
            Value::String(format!("unknown/{method}")),
        );
        request
    }

    #[test]
    fn oversized_stdio_request_returns_size_limit_exceeded_error() -> Result<(), String> {
        let payload = "x".repeat(17 * 1024 * 1024);
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"ee_search","arguments":{{"query":"{payload}"}}}}}}"#
        );
        assert!(request.len() > DEFAULT_MCP_MAX_REQUEST_BYTES);

        let outcome = handle_stdio_line(&request, DEFAULT_MCP_MAX_REQUEST_BYTES);
        assert!(!outcome.shutdown);
        let response = outcome
            .response
            .ok_or_else(|| "oversized request must return an error response".to_string())?;
        let error = response
            .get("error")
            .ok_or_else(|| "oversized request response missing error".to_string())?;
        assert_eq!(error.get("code").and_then(Value::as_i64), Some(-32000));
        assert_eq!(
            error.get("message").and_then(Value::as_str),
            Some(MCP_SIZE_LIMIT_EXCEEDED_CODE)
        );
        let data = error
            .get("data")
            .ok_or_else(|| "oversized request response missing error.data".to_string())?;
        assert_eq!(
            data.get("code").and_then(Value::as_str),
            Some(MCP_SIZE_LIMIT_EXCEEDED_CODE)
        );
        assert_eq!(
            data.get("direction").and_then(Value::as_str),
            Some("request")
        );
        assert!(
            data.get("actualBytes")
                .and_then(Value::as_u64)
                .is_some_and(|actual| actual > DEFAULT_MCP_MAX_REQUEST_BYTES as u64)
        );
        assert_eq!(
            data.get("maxBytes").and_then(Value::as_u64),
            Some(DEFAULT_MCP_MAX_REQUEST_BYTES as u64)
        );
        Ok(())
    }

    #[test]
    fn oversized_response_writer_returns_size_limit_exceeded_error() -> Result<(), String> {
        let response = json_rpc_result(
            json!("big-response"),
            json!({
                "content": "x".repeat(4096)
            }),
        );
        let mut output = Vec::new();
        write_json_rpc_response(&mut output, &response, 1024)?;

        let rendered = String::from_utf8(output).map_err(|error| error.to_string())?;
        let parsed: Value =
            serde_json::from_str(rendered.trim()).map_err(|error| error.to_string())?;
        let error = parsed
            .get("error")
            .ok_or_else(|| "capped response missing error".to_string())?;
        assert_eq!(
            parsed.get("id").and_then(Value::as_str),
            Some("big-response")
        );
        assert_eq!(error.get("code").and_then(Value::as_i64), Some(-32000));
        assert_eq!(
            error
                .get("data")
                .and_then(|data| data.get("direction"))
                .and_then(Value::as_str),
            Some("response")
        );
        assert!(
            rendered.len() <= 1025,
            "rendered capped response should stay within cap plus newline"
        );
        Ok(())
    }

    #[test]
    fn oversized_response_writer_omits_id_when_error_would_exceed_cap() -> Result<(), String> {
        let oversized_id = "request-id-".to_owned() + &"x".repeat(950);
        let response = json_rpc_result(
            json!(oversized_id),
            json!({
                "content": "x".repeat(4096)
            }),
        );
        let mut output = Vec::new();
        write_json_rpc_response(&mut output, &response, 1024)?;

        let rendered = String::from_utf8(output).map_err(|error| error.to_string())?;
        let parsed: Value =
            serde_json::from_str(rendered.trim()).map_err(|error| error.to_string())?;
        assert!(
            parsed.get("id").is_some_and(Value::is_null),
            "oversized fallback should omit an id that would exceed the configured cap: {parsed}"
        );
        assert_eq!(
            parsed
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str),
            Some(MCP_SIZE_LIMIT_EXCEEDED_CODE)
        );
        assert!(
            rendered.len() <= 1025,
            "rendered capped response should stay within cap plus newline"
        );
        Ok(())
    }

    fn expect_error<T>(result: Result<T, String>, expected: &str) -> Result<(), String> {
        match result {
            Ok(_) => Err(format!("expected error: {expected}")),
            Err(error) => {
                assert_eq!(error, expected);
                Ok(())
            }
        }
    }

    fn uri_component_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop::sample::select(vec![
                'a', 'b', 'z', 'A', 'Z', '0', '9', ' ', '+', '%', '&', '=', '/', '?', '.', '-',
                '_', '~', ':', '@',
            ]),
            1..64,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    fn percent_encode_component(value: &str) -> String {
        let mut encoded = String::with_capacity(value.len());
        for byte in value.as_bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                encoded.push(char::from(*byte));
            } else {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                encoded.push('%');
                encoded.push(char::from(HEX[usize::from(byte >> 4)]));
                encoded.push(char::from(HEX[usize::from(byte & 0x0F)]));
            }
        }
        encoded
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
    fn argument_helpers_reject_empty_name_lists_without_panicking() -> Result<(), String> {
        let arguments = json!({
            "query": "prepare release",
            "flag": true,
            "limit": 5,
            "score": 0.75
        });
        let expected = "MCP argument helper requires at least one argument name";

        expect_error(required_string(&arguments, &[]), expected)?;
        expect_error(optional_string(&arguments, &[]), expected)?;
        expect_error(optional_bool(&arguments, &[]), expected)?;
        expect_error(optional_bool_with_default(&arguments, &[], true), expected)?;
        expect_error(optional_u32(&arguments, &[]), expected)?;
        expect_error(optional_number_string(&arguments, &[]), expected)?;
        expect_error(prompt_optional_string(&Value::Null, &[]), expected)?;
        expect_error(prompt_required_string(&Value::Null, &[]), expected)?;
        expect_error(prompt_optional_bool(&Value::Null, &[]), expected)
    }

    #[test]
    fn uri_query_helpers_reject_empty_name_lists_without_panicking() -> Result<(), String> {
        let query = vec![("includeContent".to_owned(), "maybe".to_owned())];
        let expected = "MCP URI query helper requires at least one query parameter name";

        expect_error(required_query_param(&query, &[]), expected)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn resource_context_query_uri_decodes_cli_arguments(
            query in uri_component_strategy(),
            workspace in uri_component_strategy(),
            profile in prop::sample::select(vec!["compact", "balanced", "grounding", "orientation", "thorough", "submodular"]),
        ) {
            let uri = format!(
                "ee://context-packs/by-query?query={}&workspace={}&profile={}",
                percent_encode_component(&query),
                percent_encode_component(&workspace),
                profile,
            );
            let cli_args = build_cli_args_for_resource(&uri).map_err(TestCaseError::fail)?;
            let args = os_args_to_strings(cli_args).map_err(TestCaseError::fail)?;

            prop_assert_eq!(
                args,
                vec![
                    "ee".to_string(),
                    "--json".to_string(),
                    "--workspace".to_string(),
                    workspace,
                    "pack".to_string(),
                    query,
                    "--profile".to_string(),
                    profile.to_string(),
                ]
                );
        }

        #[test]
        fn mcp_json_rpc_handler_is_total_for_arbitrary_json(request in arbitrary_json_value()) {
            // Known-method dispatch is covered by golden and E2E tests. Keep this
            // property at the JSON-RPC validation boundary so generated inputs
            // cannot accidentally run CLI-backed resources or tools.
            let request = without_dispatching_known_mcp_method(request);
            let response = handle_json_rpc_message(&request);

            if request.is_object() && request.get("id").is_none() {
                prop_assert!(
                    response.is_none(),
                    "notification-shaped requests must not receive a response: {request}"
                );
                return Ok(());
            }

            let Some(response) = response else {
                return Ok(());
            };

            prop_assert!(response.is_object(), "response must be an object: {response}");
            prop_assert_eq!(
                response.get("jsonrpc").and_then(Value::as_str),
                Some("2.0")
            );

            let has_result = response.get("result").is_some();
            let has_error = response.get("error").is_some();
            prop_assert_ne!(
                has_result,
                has_error,
                "response must contain exactly one of result or error: {}",
                response
            );

            let id = response
                .get("id")
                .ok_or_else(|| TestCaseError::fail(format!("response missing id: {response}")))?;
            prop_assert!(
                is_valid_json_rpc_id(id),
                "response id must be a valid JSON-RPC id: {response}"
            );

            if let Some(error) = response.get("error") {
                prop_assert!(error.is_object(), "error must be an object: {response}");
                prop_assert!(
                    error.get("code").and_then(Value::as_i64).is_some(),
                    "error must contain integer code: {response}"
                );
                prop_assert!(
                    error.get("message").and_then(Value::as_str).is_some(),
                    "error must contain string message: {response}"
                );
            }
        }
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
        assert!(prompt_names.contains(&"pre-edit-recall"));
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
        assert!(text.contains("ee pack"));
        assert!(text.contains("--max-tokens 3000"));
        Ok(())
    }

    #[test]
    fn handle_prompts_get_pre_edit_recall_renders_selectors() -> Result<(), String> {
        let response = handle_prompts_get(
            json!(1),
            Some(&json!({
                "name": "pre-edit-recall",
                "arguments": {
                    "workspace": ".",
                    "path": "src/mcp.rs",
                    "symbol": "McpToolEntry",
                    "budgetTokens": 800
                }
            })),
        );
        let Some(result) = response.get("result") else {
            return Err("pre-edit-recall response missing result".to_string());
        };
        let Some(messages) = result.get("messages").and_then(Value::as_array) else {
            return Err("pre-edit-recall response missing messages array".to_string());
        };
        let Some(text) = messages
            .first()
            .and_then(|message| message.get("content"))
            .and_then(|content| content.get("text"))
            .and_then(Value::as_str)
        else {
            return Err("pre-edit-recall response missing text".to_string());
        };
        assert!(text.contains("ee recall"));
        assert!(text.contains("--path \"src/mcp.rs\""));
        assert!(text.contains("--symbol \"McpToolEntry\""));
        assert!(text.contains("--budget-tokens 800"));
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
        assert!(resource_uris.contains(&"ee://schemas/ee.response.v2"));
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
        assert!(
            result.get("isError").is_none(),
            "resources/read must not use tool-result isError metadata"
        );
        assert!(
            result.get("exitCode").is_none(),
            "resources/read must not use tool-result exitCode metadata"
        );
        assert!(
            result.get("stderr").is_none(),
            "resources/read must not expose tool-result stderr metadata"
        );

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
            Some("ee.response.v2")
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
    fn resource_read_failure_returns_redacted_json_rpc_error() -> Result<(), String> {
        let raw_stderr =
            "error: failed to read /Users/alice/private/repo/logs/build.log\nNext: inspect it";
        let response = resource_read_result(
            json!(1),
            "ee://workspace/status",
            ProcessExitCode::Storage,
            String::new(),
            raw_stderr.to_owned(),
        );
        assert!(
            response.get("result").is_none(),
            "failed resources/read must not be reported as a successful resource body"
        );
        let Some(message) = response
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
        else {
            return Err("resource response missing error message".to_string());
        };

        assert!(!message.contains("/Users/alice/private/repo"));
        assert!(message.contains("[REDACTED_PATH]"));
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
        assert!(tool_names.contains(&"ee_doctor"));
        assert!(tool_names.contains(&"ee_capabilities"));
        assert!(tool_names.contains(&"ee_insights"));
        assert!(tool_names.contains(&"ee_proximity"));
        assert!(tool_names.contains(&"ee_recall"));
        assert!(tool_names.contains(&"ee_ask"));
        assert!(tool_names.contains(&"ee_primer"));
        assert!(tool_names.contains(&"ee_pack_dna_explain"));
        assert!(tool_names.contains(&"ee_revision_impact"));
        assert!(tool_names.contains(&"ee_memory_show"));
        assert!(tool_names.contains(&"ee_why"));
        assert!(tool_names.contains(&"ee_remember"));
        assert!(tool_names.contains(&"ee_outcome"));
        assert!(tool_names.contains(&"ee_journal_append"));
        assert!(tool_names.contains(&"ee_decide_record"));
        assert!(tool_names.contains(&"ee_decide_list"));
        assert!(tool_names.contains(&"ee_decide_revisit"));

        let Some(remember) = tools
            .iter()
            .find(|tool| tool.get("name").and_then(Value::as_str) == Some("ee_remember"))
        else {
            return Err("ee_remember tool missing".to_string());
        };
        for name in [
            "ee_health",
            "ee_status",
            "ee_doctor",
            "ee_capabilities",
            "ee_search",
            "ee_context",
            "ee_recall",
            "ee_ask",
            "ee_primer",
            "ee_insights",
            "ee_proximity",
            "ee_pack_dna_explain",
            "ee_revision_impact",
            "ee_memory_show",
            "ee_why",
            "ee_decide_list",
            "ee_decide_revisit",
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
        let primer = tools
            .iter()
            .find(|tool| tool.get("name").and_then(Value::as_str) == Some("ee_primer"))
            .ok_or_else(|| "ee_primer tool missing".to_string())?;
        assert_eq!(
            primer
                .get("eeEffect")
                .and_then(|effect| effect.get("kind"))
                .and_then(Value::as_str),
            Some("derived_cache_write")
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
            Some("ee.response.v2")
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
    fn stdio_process_reuses_cli_advisory_state_across_real_search_calls() -> Result<(), String> {
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_text = workspace.path().to_string_lossy().into_owned();
        let mut process = McpProcess::default();
        for args in [
            vec!["ee", "--json", "--workspace", &workspace_text, "init"],
            vec![
                "ee",
                "--json",
                "--workspace",
                &workspace_text,
                "remember",
                "MCP process advisory production-path evidence.",
                "--level",
                "semantic",
                "--kind",
                "fact",
            ],
            vec![
                "ee",
                "--json",
                "--workspace",
                &workspace_text,
                "index",
                "rebuild",
            ],
        ] {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let exit = process.cli.run(
                args.into_iter().map(OsString::from),
                &mut stdout,
                &mut stderr,
            );
            if exit != ProcessExitCode::Success {
                return Err(format!(
                    "MCP search setup failed with exit {}: {}",
                    exit as u8,
                    String::from_utf8_lossy(&stderr)
                ));
            }
        }

        let request = |id: u64, query: &str| {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {
                    "name": "ee_search",
                    "arguments": {
                        "query": query,
                        "workspace": workspace_text.as_str()
                    }
                }
            })
        };
        let first_outcome = handle_stdio_line_in_process(
            &mut process,
            &request(1, "MCP advisory first search").to_string(),
            DEFAULT_MCP_MAX_REQUEST_BYTES,
        );
        let repeated_outcome = handle_stdio_line_in_process(
            &mut process,
            &request(2, "MCP advisory repeated search").to_string(),
            DEFAULT_MCP_MAX_REQUEST_BYTES,
        );
        assert!(!first_outcome.shutdown && !repeated_outcome.shutdown);
        let first_response = first_outcome
            .response
            .ok_or_else(|| "first MCP search response missing".to_owned())?;
        let repeated_response = repeated_outcome
            .response
            .ok_or_else(|| "repeated MCP search response missing".to_owned())?;
        let first: Value = serde_json::from_str(first_tool_text(&first_response)?)
            .map_err(|error| format!("first MCP search returned invalid JSON: {error}"))?;
        let repeated: Value = serde_json::from_str(first_tool_text(&repeated_response)?)
            .map_err(|error| format!("repeated MCP search returned invalid JSON: {error}"))?;

        assert_eq!(
            first
                .pointer("/data/rerank/advisory/code")
                .and_then(Value::as_str),
            Some("rerank_model_unavailable")
        );
        assert_eq!(
            first
                .pointer("/data/rerank/advisorySummary/scope")
                .and_then(Value::as_str),
            Some(crate::core::search::SEARCH_ADVISORY_SCOPE_PROCESS)
        );
        assert!(
            repeated
                .pointer("/data/rerank/advisory")
                .is_some_and(Value::is_null)
        );
        assert_eq!(
            repeated
                .pointer("/data/rerank/advisorySummary/sessionOccurrenceCount")
                .and_then(Value::as_u64),
            Some(2)
        );
        assert_eq!(
            repeated
                .pointer("/data/rerank/advisorySummary/sessionSuppressedCount")
                .and_then(Value::as_u64),
            Some(1)
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
    fn tool_result_redacts_stderr_even_when_stdout_is_json() -> Result<(), String> {
        let stdout = "{\"schema\":\"ee.response.v2\",\"success\":true}".to_string();
        let raw_stderr = "warning: ignored /Users/alice/private/repo/.ee/config.toml".to_string();
        let response = cli_tool_result(
            json!(1),
            ProcessExitCode::Success,
            stdout.clone(),
            raw_stderr,
        );
        let Some(result) = response.get("result") else {
            return Err("tool response missing result".to_string());
        };

        assert_eq!(first_tool_text(&response)?, stdout);
        let Some(stderr) = result.get("stderr").and_then(Value::as_str) else {
            return Err("tool response missing stderr".to_string());
        };
        assert!(!stderr.contains("/Users/alice/private/repo"));
        assert!(stderr.contains("[REDACTED_PATH]"));
        Ok(())
    }

    fn registry_tool(name: &str) -> Result<&'static McpToolEntry, String> {
        mcp_tool_entry(name).ok_or_else(|| format!("missing registry entry for {name}"))
    }

    #[test]
    fn build_cli_args_outcome_defaults_to_dry_run_and_keeps_event_id() -> Result<(), String> {
        let args = os_args_to_strings(build_cli_args_for_tool(
            registry_tool("ee_outcome")?,
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
            registry_tool("ee_remember")?,
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
    fn build_cli_args_graph_tools_route_to_read_only_commands() -> Result<(), String> {
        let insights = os_args_to_strings(build_cli_args_for_tool(
            registry_tool("ee_insights")?,
            &json!({
                "workspace": ".",
                "section": "topMemories",
                "limit": 3
            }),
        )?)?;
        assert_eq!(
            insights,
            vec![
                "ee",
                "--json",
                "--workspace",
                ".",
                "insights",
                "--section",
                "topMemories",
                "--limit",
                "3",
            ]
        );

        let proximity = os_args_to_strings(build_cli_args_for_tool(
            registry_tool("ee_proximity")?,
            &json!({
                "memoryIdA": "mem_a",
                "memoryIdB": "mem_b",
                "minWeight": 0.5,
                "minConfidence": 0.75,
                "linkLimit": 100,
                "includeTombstoned": true
            }),
        )?)?;
        assert_eq!(
            proximity,
            vec![
                "ee",
                "--json",
                "proximity",
                "mem_a",
                "mem_b",
                "--min-weight",
                "0.5",
                "--min-confidence",
                "0.75",
                "--link-limit",
                "100",
                "--include-tombstoned",
            ]
        );

        let pack_dna = os_args_to_strings(build_cli_args_for_tool(
            registry_tool("ee_pack_dna_explain")?,
            &json!({
                "query": "prepare release",
                "maxTokens": 1200,
                "profile": "balanced"
            }),
        )?)?;
        assert_eq!(
            pack_dna,
            vec![
                "ee",
                "--json",
                "pack",
                "prepare release",
                "--max-tokens",
                "1200",
                "--profile",
                "balanced",
                "--explain",
            ]
        );

        let revision_impact = os_args_to_strings(build_cli_args_for_tool(
            registry_tool("ee_revision_impact")?,
            &json!({
                "memoryId": "mem_00000000000000000000000001",
                "database": "/tmp/ee.db"
            }),
        )?)?;
        assert_eq!(
            revision_impact,
            vec![
                "ee",
                "--json",
                "memory",
                "revise",
                "mem_00000000000000000000000001",
                "--database",
                "/tmp/ee.db",
                "--content",
                "ee-mcp-revision-impact-probe-00000000000000000000000000000000",
                "--reason",
                "mcp revision impact read-only probe",
                "--dry-run",
            ]
        );
        Ok(())
    }

    #[test]
    fn build_cli_args_wave_read_tools_route_to_cli_commands() -> Result<(), String> {
        let recall = os_args_to_strings(build_cli_args_for_tool(
            registry_tool("ee_recall")?,
            &json!({
                "workspace": ".",
                "path": ["src/mcp.rs", "tests/mcp_parity.rs"],
                "symbol": "McpToolEntry",
                "kind": ["rule"],
                "budgetTokens": 900,
                "stale": true
            }),
        )?)?;
        assert_eq!(
            recall,
            vec![
                "ee",
                "--json",
                "--workspace",
                ".",
                "recall",
                "--path",
                "src/mcp.rs",
                "--path",
                "tests/mcp_parity.rs",
                "--symbol",
                "McpToolEntry",
                "--kind",
                "rule",
                "--stale",
                "--budget-tokens",
                "900",
            ]
        );

        let ask = os_args_to_strings(build_cli_args_for_tool(
            registry_tool("ee_ask")?,
            &json!({
                "question": "What MCP write tools require allowWrite?",
                "limitEvidence": 4,
                "minConfidence": 0.4,
                "requireConfidence": 0.7,
                "database": "/tmp/ee.db"
            }),
        )?)?;
        assert_eq!(
            ask,
            vec![
                "ee",
                "--json",
                "ask",
                "What MCP write tools require allowWrite?",
                "--limit-evidence",
                "4",
                "--min-confidence",
                "0.4",
                "--require-confidence",
                "0.7",
                "--database",
                "/tmp/ee.db",
            ]
        );

        let primer = os_args_to_strings(build_cli_args_for_tool(
            registry_tool("ee_primer")?,
            &json!({
                "tokens": 700,
                "refresh": true,
                "noPersist": true
            }),
        )?)?;
        assert_eq!(
            primer,
            vec![
                "ee",
                "--json",
                "primer",
                "--tokens",
                "700",
                "--refresh",
                "--no-persist",
            ]
        );
        Ok(())
    }

    #[test]
    fn build_cli_args_decision_tools_route_to_cli_commands() -> Result<(), String> {
        let record = os_args_to_strings(build_cli_args_for_tool(
            registry_tool("ee_decide_record")?,
            &json!({
                "topic": "MCP wave parity",
                "chosen": "hand-written registry entries",
                "alternative": ["auto-generate wrappers", "leave MCP incomplete"],
                "rationale": "MCP schemas must document effects explicitly",
                "revisitBy": "+30d",
                "actor": "mcp-test"
            }),
        )?)?;
        assert_eq!(
            record,
            vec![
                "ee",
                "--json",
                "decide",
                "record",
                "MCP wave parity",
                "--chosen",
                "hand-written registry entries",
                "--alternative",
                "auto-generate wrappers",
                "--alternative",
                "leave MCP incomplete",
                "--rationale",
                "MCP schemas must document effects explicitly",
                "--revisit-by",
                "+30d",
                "--actor",
                "mcp-test",
                "--dry-run",
            ]
        );

        let list = os_args_to_strings(build_cli_args_for_tool(
            registry_tool("ee_decide_list")?,
            &json!({
                "about": "MCP",
                "includeSuperseded": true,
                "limit": 7
            }),
        )?)?;
        assert_eq!(
            list,
            vec![
                "ee",
                "--json",
                "decide",
                "list",
                "--about",
                "MCP",
                "--include-superseded",
                "--limit",
                "7",
            ]
        );

        let revisit = os_args_to_strings(build_cli_args_for_tool(
            registry_tool("ee_decide_revisit")?,
            &json!({
                "warningDays": 14,
                "limit": 5
            }),
        )?)?;
        assert_eq!(
            revisit,
            vec![
                "ee",
                "--json",
                "decide",
                "revisit",
                "--warning-days",
                "14",
                "--limit",
                "5",
            ]
        );
        Ok(())
    }

    #[test]
    fn build_cli_args_write_wave_tools_enforce_gates() -> Result<(), String> {
        let journal_error = build_cli_args_for_tool(
            registry_tool("ee_journal_append")?,
            &json!({
                "text": "Dry-run journal append remains safe by default."
            }),
        )
        .expect_err("journal append default dry-run must not write");
        assert_eq!(
            journal_error,
            "Tool ee_journal_append dryRun=true requires ee journal append --dry-run support"
        );

        let journal = os_args_to_strings(build_cli_args_for_tool(
            registry_tool("ee_journal_append")?,
            &json!({
                "text": "durable only when explicitly allowed",
                "kind": "note",
                "source": "manual",
                "path": ["src/mcp.rs"],
                "dryRun": false,
                "allowWrite": true
            }),
        )?)?;
        assert_eq!(
            journal,
            vec![
                "ee",
                "--json",
                "journal",
                "append",
                "durable only when explicitly allowed",
                "--kind",
                "note",
                "--source",
                "manual",
                "--path",
                "src/mcp.rs",
            ]
        );

        let decide_error = build_cli_args_for_tool(
            registry_tool("ee_decide_record")?,
            &json!({
                "topic": "MCP wave parity",
                "chosen": "record",
                "alternative": ["skip"],
                "rationale": "writes need allowWrite",
                "dryRun": false
            }),
        )
        .expect_err("decide record durable write must require allowWrite");
        assert_eq!(
            decide_error,
            "Write tool ee_decide_record requires allowWrite=true when dryRun=false"
        );

        let outcome = os_args_to_strings(build_cli_args_for_tool(
            registry_tool("ee_outcome")?,
            &json!({
                "pack": "pack_01234567890123456789012345",
                "item": 2,
                "signal": "helpful"
            }),
        )?)?;
        assert!(outcome.contains(&"--pack".to_string()));
        assert!(outcome.contains(&"pack_01234567890123456789012345".to_string()));
        assert!(outcome.contains(&"--item".to_string()));
        assert!(outcome.contains(&"2".to_string()));
        assert!(outcome.contains(&"--dry-run".to_string()));

        let batch_error = build_cli_args_for_tool(
            registry_tool("ee_outcome")?,
            &json!({
                "batch": true,
                "signal": "helpful"
            }),
        )
        .expect_err("outcome batch must not hang on stdin");
        assert_eq!(
            batch_error,
            "Tool ee_outcome cannot use --batch because MCP tools/call has no stdin stream"
        );
        Ok(())
    }

    #[test]
    fn every_write_tool_declares_effect_metadata() {
        for tool in TOOL_REGISTRY {
            if tool.annotations.read_only {
                continue;
            }
            let effect = tool
                .effect
                .unwrap_or_else(|| panic!("write tool {} missing eeEffect metadata", tool.name));
            assert!(
                !effect.write_surface.is_empty(),
                "write tool {} must declare write_surface",
                tool.name
            );
            assert!(
                !effect.audit.trim().is_empty(),
                "write tool {} must declare audit effect text",
                tool.name
            );
            assert!(
                !effect.redaction.trim().is_empty(),
                "write tool {} must declare redaction effect text",
                tool.name
            );
            assert!(
                !effect.idempotency.trim().is_empty(),
                "write tool {} must declare idempotency effect text",
                tool.name
            );
        }
    }

    #[test]
    fn extract_mcp_tool_payload_returns_pack_dna_and_revision_impact() -> Result<(), String> {
        let context_stdout = json!({
            "schema": "ee.response.v2",
            "success": true,
            "data": {
                "pack": {
                    "packDna": {
                        "schema": "ee.context.pack_dna.v1",
                        "query": "prepare release"
                    }
                }
            }
        })
        .to_string();
        let pack_dna = extract_mcp_tool_payload("ee_pack_dna_explain", &context_stdout)?
            .ok_or_else(|| "pack DNA extraction returned None".to_string())?;
        let parsed_pack_dna: Value =
            serde_json::from_str(&pack_dna).map_err(|error| error.to_string())?;
        assert_eq!(
            parsed_pack_dna.get("schema").and_then(Value::as_str),
            Some("ee.context.pack_dna.v1")
        );

        let impact_stdout = json!({
            "schema": "ee.response.v2",
            "success": true,
            "data": {
                "impactAnalysis": {
                    "schema": "ee.memory.impact_analysis.v1",
                    "memoryId": "mem_00000000000000000000000001"
                }
            }
        })
        .to_string();
        let impact = extract_mcp_tool_payload("ee_revision_impact", &impact_stdout)?
            .ok_or_else(|| "revision impact extraction returned None".to_string())?;
        let parsed_impact: Value =
            serde_json::from_str(&impact).map_err(|error| error.to_string())?;
        assert_eq!(
            parsed_impact.get("schema").and_then(Value::as_str),
            Some("ee.memory.impact_analysis.v1")
        );

        assert!(extract_mcp_tool_payload("ee_context", &context_stdout)?.is_none());
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
            registry_tool("ee_context")?,
            &json!({
                "query": "prepare release",
                "maxTokens": u32::MAX,
                "candidatePool": 250,
                "profile": "thorough"
            }),
        )?)?;

        let max_tokens = u32::MAX.to_string();
        assert!(args.contains(&"pack".to_string()));
        assert!(!args.contains(&"context".to_string()));
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
            text.starts_with("{\"schema\":\"ee.error.v2\""),
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
    fn shutdown_notification_is_fire_and_forget_without_stopping_stdio_loop() {
        let outcome = handle_stdio_line(
            r#"{"jsonrpc":"2.0","method":"shutdown"}"#,
            DEFAULT_MCP_MAX_REQUEST_BYTES,
        );

        assert!(outcome.response.is_none());
        assert!(!outcome.shutdown);
    }

    #[test]
    fn valid_shutdown_request_stops_stdio_loop_after_success_response() -> Result<(), String> {
        let outcome = handle_stdio_line(
            r#"{"jsonrpc":"2.0","id":"shutdown-1","method":"shutdown"}"#,
            DEFAULT_MCP_MAX_REQUEST_BYTES,
        );

        assert!(outcome.shutdown);
        let response = outcome
            .response
            .ok_or_else(|| "shutdown request should produce a response".to_string())?;
        assert_eq!(response.get("id"), Some(&json!("shutdown-1")));
        assert!(response.get("result").is_some());
        assert!(response.get("error").is_none());
        Ok(())
    }

    #[test]
    fn invalid_shutdown_request_returns_error_without_stopping_stdio_loop() -> Result<(), String> {
        let outcome = handle_stdio_line(
            r#"{"jsonrpc":"1.0","id":"bad-shutdown","method":"shutdown"}"#,
            DEFAULT_MCP_MAX_REQUEST_BYTES,
        );

        assert!(!outcome.shutdown);
        let response = outcome
            .response
            .ok_or_else(|| "invalid shutdown request should produce an error".to_string())?;
        let Some(error) = response.get("error") else {
            return Err("invalid shutdown response missing error".to_string());
        };
        assert_eq!(response.get("id"), Some(&json!("bad-shutdown")));
        assert_eq!(error.get("code").and_then(Value::as_i64), Some(-32600));
        assert_eq!(
            error.get("message").and_then(Value::as_str),
            Some("Invalid Request: jsonrpc must be \"2.0\"")
        );
        Ok(())
    }

    #[test]
    fn non_object_json_rpc_message_returns_invalid_request() -> Result<(), String> {
        let response = handle_json_rpc_message(&json!([]))
            .ok_or_else(|| "non-object request must produce an error response".to_string())?;
        let Some(error) = response.get("error") else {
            return Err("non-object request response missing error".to_string());
        };

        assert_eq!(response.get("id"), Some(&Value::Null));
        assert_eq!(error.get("code").and_then(Value::as_i64), Some(-32600));
        assert_eq!(
            error.get("message").and_then(Value::as_str),
            Some("Invalid Request: request must be a JSON object")
        );
        Ok(())
    }

    #[test]
    fn missing_or_non_string_method_returns_invalid_request() -> Result<(), String> {
        let cases = [
            (
                json!({"jsonrpc": "2.0", "id": "missing-method"}),
                "Invalid Request: method is required",
            ),
            (
                json!({"jsonrpc": "2.0", "id": "bad-method", "method": []}),
                "Invalid Request: method must be a non-empty string",
            ),
        ];

        for (request, expected_message) in cases {
            let response = handle_json_rpc_message(&request)
                .ok_or_else(|| "invalid request must produce an error response".to_string())?;
            let Some(error) = response.get("error") else {
                return Err(format!(
                    "invalid request response missing error for {request}"
                ));
            };
            assert_eq!(error.get("code").and_then(Value::as_i64), Some(-32600));
            assert_eq!(
                error.get("message").and_then(Value::as_str),
                Some(expected_message)
            );
        }
        Ok(())
    }

    #[test]
    fn missing_or_invalid_jsonrpc_version_returns_invalid_request() -> Result<(), String> {
        for request in [
            json!({"id": "missing-jsonrpc", "method": "initialize"}),
            json!({"jsonrpc": "1.0", "id": "old-jsonrpc", "method": "initialize"}),
            json!({"jsonrpc": 2.0, "id": "numeric-jsonrpc", "method": "initialize"}),
        ] {
            let response = handle_json_rpc_message(&request)
                .ok_or_else(|| "invalid jsonrpc request must produce an error".to_string())?;
            let Some(error) = response.get("error") else {
                return Err(format!(
                    "invalid jsonrpc response missing error for {request}"
                ));
            };
            assert_eq!(error.get("code").and_then(Value::as_i64), Some(-32600));
            assert_eq!(
                error.get("message").and_then(Value::as_str),
                Some("Invalid Request: jsonrpc must be \"2.0\"")
            );
        }
        Ok(())
    }

    #[test]
    fn malformed_notification_does_not_receive_response() -> Result<(), String> {
        // Per JSON-RPC 2.0 §4.1, notifications (Request objects without an
        // `id` member) MUST NOT receive a response, even when otherwise
        // invalid. The pre-validation path silently dropped malformed
        // notifications; when the validate-first path was introduced it
        // regressed by replying. Lock the spec-compliant silence in so the
        // regression cannot recur.
        for request in [
            json!({"method": "initialize"}),
            json!({"jsonrpc": "1.0", "method": "initialize"}),
            json!({"jsonrpc": 2.0, "method": "initialize"}),
            json!({"jsonrpc": "2.0", "method": ""}),
            json!({"jsonrpc": "2.0", "method": []}),
            json!({"jsonrpc": "2.0"}),
            json!({}),
        ] {
            let response = handle_json_rpc_message(&request);
            if let Some(payload) = response {
                return Err(format!(
                    "malformed notification {request} must not produce a response, got {payload}"
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn well_formed_notification_does_not_receive_response() -> Result<(), String> {
        // Well-formed notifications dispatch silently. Pinning this
        // case alongside the malformed-notification regression keeps
        // both spec paths visible at the suite level.
        let request = json!({"jsonrpc": "2.0", "method": "notifications/cancelled"});
        let response = handle_json_rpc_message(&request);
        if let Some(payload) = response {
            return Err(format!(
                "well-formed notification must not produce a response, got {payload}"
            ));
        }
        Ok(())
    }

    #[test]
    fn invalid_json_rpc_id_returns_invalid_request_with_null_id() -> Result<(), String> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": {"nested": "not allowed"},
            "method": "initialize"
        });
        let response = handle_json_rpc_message(&request)
            .ok_or_else(|| "invalid id request must produce an error".to_string())?;
        let Some(error) = response.get("error") else {
            return Err("invalid id response missing error".to_string());
        };

        assert_eq!(response.get("id"), Some(&Value::Null));
        assert_eq!(error.get("code").and_then(Value::as_i64), Some(-32600));
        assert_eq!(
            error.get("message").and_then(Value::as_str),
            Some("Invalid Request: id must be a string, number, or null")
        );
        Ok(())
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

    #[test]
    fn mcp_tool_registry_names_round_trip_through_lookup() {
        for tool in TOOL_REGISTRY {
            let name = tool.name;
            assert!(
                name.starts_with("ee_"),
                "tool name must start with ee_: {name}"
            );
            let parsed = mcp_tool_entry(name);
            assert!(
                parsed.is_some(),
                "mcp_tool_entry failed for canonical name {name}"
            );
            assert_eq!(
                parsed.map(|entry| entry.name),
                Some(tool.name),
                "registry lookup mismatch for {name}"
            );
        }
    }

    #[test]
    fn mcp_tool_registry_names_are_unique_and_exported() {
        use std::collections::BTreeSet;
        let names: BTreeSet<&'static str> = TOOL_REGISTRY.iter().map(|tool| tool.name).collect();
        assert_eq!(
            names.len(),
            TOOL_REGISTRY.len(),
            "TOOL_REGISTRY must contain unique names"
        );
        let exported_names: BTreeSet<&'static str> = registered_tool_names().collect();
        assert_eq!(
            names, exported_names,
            "registered_tool_names() must expose TOOL_REGISTRY exactly"
        );
        for name in &names {
            assert!(
                mcp_tool_entry(name).is_some(),
                "TOOL_REGISTRY name {name} must be lookupable"
            );
        }
    }
}
