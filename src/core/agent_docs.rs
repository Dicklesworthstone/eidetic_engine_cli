use crate::config::EnvVar;
use crate::core::decide::{
    DECIDE_LIST_SCHEMA_V1, DECIDE_RECORD_SCHEMA_V1, DECIDE_REVISIT_SCHEMA_V1,
};
use crate::core::docs_bootstrap::{DOCS_BOOTSTRAP_APPLY_SCHEMA_V1, DOCS_BOOTSTRAP_RUN_SCHEMA_V1};
use crate::core::recall::RECALL_SCHEMA_V1;
use crate::hooks::HARNESS_HOOK_INSTALL_SCHEMA_V1;
use crate::models::memory::TYPED_MEMORY_FIELDS_SCHEMA_V2;
use crate::models::{ERROR_SCHEMA_V2, PACK_SCHEMA_V2, RESPONSE_SCHEMA_V2};

/// One command on the canonical agent-oriented starting path.
///
/// Human root help and the machine-readable `agent-docs` overview both
/// consume this table so their curated command lists cannot drift apart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AgentCoreCommand {
    /// Top-level CLI subcommand name.
    pub name: &'static str,
    /// One-line description rendered in root help.
    pub description: &'static str,
}

/// Canonical agent-oriented commands, ordered for session-start discovery.
pub const AGENT_CORE_COMMANDS: &[AgentCoreCommand] = &[
    AgentCoreCommand {
        name: "orient",
        description: "Start a session with pack, doctor, and workspace-hygiene context",
    },
    AgentCoreCommand {
        name: "init",
        description: "Initialize a workspace with a ready zero-document search index",
    },
    AgentCoreCommand {
        name: "remember",
        description: "Capture an explicit memory",
    },
    AgentCoreCommand {
        name: "search",
        description: "Fine-grained memory retrieval",
    },
    AgentCoreCommand {
        name: "ask",
        description: "Answer a direct question with citations or honest abstention",
    },
    AgentCoreCommand {
        name: "pack",
        description: "Assemble a task-specific context pack",
    },
    AgentCoreCommand {
        name: "lens",
        description: "Inspect reusable task-lens policies",
    },
    AgentCoreCommand {
        name: "why",
        description: "Explain why a memory was stored or selected",
    },
    AgentCoreCommand {
        name: "status",
        description: "Report workspace and subsystem posture",
    },
];

fn normalized_agent_docs_token(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut previous_was_lowercase_or_digit = false;

    for character in value.trim().chars() {
        match character {
            '-' | '_' => {
                if !normalized.ends_with('_') {
                    normalized.push('_');
                }
                previous_was_lowercase_or_digit = false;
            }
            ch if ch.is_ascii_uppercase() => {
                if previous_was_lowercase_or_digit && !normalized.ends_with('_') {
                    normalized.push('_');
                }
                normalized.push(ch.to_ascii_lowercase());
                previous_was_lowercase_or_digit = false;
            }
            ch => {
                normalized.push(ch.to_ascii_lowercase());
                previous_was_lowercase_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
            }
        }
    }

    normalized
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentDocsTopic {
    Guide,
    Commands,
    Contracts,
    Schemas,
    Paths,
    Env,
    ExitCodes,
    Fields,
    Errors,
    Formats,
    Examples,
    Recipes,
}

impl AgentDocsTopic {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Guide => "guide",
            Self::Commands => "commands",
            Self::Contracts => "contracts",
            Self::Schemas => "schemas",
            Self::Paths => "paths",
            Self::Env => "env",
            Self::ExitCodes => "exit-codes",
            Self::Fields => "fields",
            Self::Errors => "errors",
            Self::Formats => "formats",
            Self::Examples => "examples",
            Self::Recipes => "recipes",
        }
    }

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Guide => "Getting started guide for agents using ee",
            Self::Commands => "Complete command reference with arguments and flags",
            Self::Contracts => "Stable JSON/TOON output contracts for agent parsing",
            Self::Schemas => "Available response and error schema definitions",
            Self::Paths => "Default paths for database, indexes, and configuration",
            Self::Env => "Environment variables that affect ee behavior",
            Self::ExitCodes => "Exit code meanings for scripting and error handling",
            Self::Fields => "Field profiles and output verbosity levels",
            Self::Errors => "Error codes, categories, and repair suggestions",
            Self::Formats => "Output format options (json, toon, human, etc.)",
            Self::Examples => "Common workflows and command examples for agents",
            Self::Recipes => "Machine-readable workflows with jq selectors and failure branches",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match normalized_agent_docs_token(s).as_str() {
            "guide" => Some(Self::Guide),
            "commands" => Some(Self::Commands),
            "contracts" => Some(Self::Contracts),
            "schemas" => Some(Self::Schemas),
            "paths" => Some(Self::Paths),
            "env" => Some(Self::Env),
            "exit_codes" => Some(Self::ExitCodes),
            "fields" => Some(Self::Fields),
            "errors" => Some(Self::Errors),
            "formats" => Some(Self::Formats),
            "examples" => Some(Self::Examples),
            "recipes" => Some(Self::Recipes),
            _ => None,
        }
    }

    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Guide,
            Self::Commands,
            Self::Contracts,
            Self::Schemas,
            Self::Paths,
            Self::Env,
            Self::ExitCodes,
            Self::Fields,
            Self::Errors,
            Self::Formats,
            Self::Examples,
            Self::Recipes,
        ]
    }
}

#[derive(Clone, Debug)]
pub struct AgentDocsReport {
    pub version: &'static str,
    pub topic: Option<AgentDocsTopic>,
}

impl AgentDocsReport {
    #[must_use]
    pub fn new(topic: Option<AgentDocsTopic>) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            topic,
        }
    }

    #[must_use]
    pub fn gather(topic: Option<AgentDocsTopic>) -> Self {
        Self::new(topic)
    }
}

#[derive(Clone, Debug)]
pub struct GuideSection {
    pub title: &'static str,
    pub content: &'static str,
}

pub const GUIDE_SECTIONS: &[GuideSection] = &[
    GuideSection {
        title: "Overview",
        content: "ee is a durable, local-first, explainable memory substrate for coding agents. It captures facts, work history, decisions, procedural rules, failures, and evidence; indexes them with hybrid search; and emits compact, explainable context packs.",
    },
    GuideSection {
        title: "Primary Workflow",
        content: "ee pack \"<task>\" --workspace . --max-tokens 4000 --json",
    },
    GuideSection {
        title: "Session Start",
        content: "ee orient \"<task>\" --include-primer --fast --json folds the cached workspace primer (top rules, unresolved warnings, key decisions, load-bearing memories; every line provenance-backed) into one cold-start call. Standalone `ee primer --json` serves the same charter from primer_cache; warm hits are byte-identical and fast enough for SessionStart hooks. Keep AGENTS.md honest with `ee export agentsmd` and audit it with `ee diag agentsmd-drift --json`.",
    },
    GuideSection {
        title: "Task Lenses",
        content: "Use `ee lens list --json` to discover named pack policies, `ee lens explain <id> --json` to inspect effective options and the stable lens hash, and `ee pack \"<task>\" --lens <id> --json` to bind that policy into the persisted pack replay ledger.",
    },
    GuideSection {
        title: "Code-Anchored Recall",
        content: "Run `ee recall --path <path> --workspace . --budget-tokens 400 --format markdown` before editing known files, or `ee recall --diff HEAD --workspace . --json` before reviewing a diff. Recall is a narrow anchor lookup; use `ee search` for free-text discovery and `ee pack` for task context.",
    },
    GuideSection {
        title: "Direct Answers",
        content: "Use `ee ask \"<question>\" --workspace . --json` for narrow, citation-backed extractive answers. Inspect `data.citations[]`, `data.sides[]`, and `data.nearestEvidence[]`; add `--require-confidence <threshold>` when hooks must fail closed instead of accepting an abstention.",
    },
    GuideSection {
        title: "Machine Output",
        content: "Always use --json, --robot, or --format=json for machine-parseable output. stdout is data, stderr is diagnostics. Check exit codes for error conditions.",
    },
    GuideSection {
        title: "Workspace",
        content: "ee operates on workspaces (directories). Use --workspace to specify the target, or it defaults to the current directory. The .ee/ folder contains the database and search indexes; `ee init` publishes a ready zero-document index so retrieval works immediately after the first memory is captured.",
    },
    GuideSection {
        title: "Degradation",
        content: "ee degrades gracefully. If semantic search is unavailable, it falls back to lexical. If the database is missing, init creates it. Check ee status --json for capability state.",
    },
    GuideSection {
        title: "Docs Bootstrap",
        content: "Use `ee bootstrap docs --dry-run --json` to compile allowlisted repository docs into reviewable candidates. Dry-runs never create memories, and apply only materializes or applies candidates through curation after `--approved-only`; inspect parserVersion and degraded[] before trusting a run.",
    },
];

#[derive(Clone, Debug)]
pub struct PathEntry {
    pub name: &'static str,
    pub default: &'static str,
    pub description: &'static str,
    pub env_override: Option<&'static str>,
}

pub const DEFAULT_PATHS: &[PathEntry] = &[
    PathEntry {
        name: "database",
        default: "<workspace>/.ee/ee.db",
        description: "SQLite database storing memories, sessions, and metadata",
        env_override: Some(EnvVar::DatabasePath.name()),
    },
    PathEntry {
        name: "index_dir",
        default: "<workspace>/.ee/index/",
        description: "Directory containing search indexes",
        env_override: Some(EnvVar::IndexDir.name()),
    },
    PathEntry {
        name: "config",
        default: "<workspace>/.ee/config.toml",
        description: "Workspace-specific configuration file",
        env_override: None,
    },
    PathEntry {
        name: "global_config",
        default: "~/.config/ee/config.toml",
        description: "Global user configuration file",
        env_override: None,
    },
    PathEntry {
        name: "lock",
        default: "<workspace>/.ee/ee.lock",
        description: "Advisory write lock file for concurrent access",
        env_override: None,
    },
];

#[derive(Clone, Debug)]
pub struct EnvVarEntry {
    pub name: &'static str,
    pub description: &'static str,
    pub default: Option<&'static str>,
    pub category: &'static str,
}

impl EnvVarEntry {
    #[must_use]
    pub const fn from_env_var(var: EnvVar) -> Self {
        Self {
            name: var.name(),
            description: var.description(),
            default: var.default_value(),
            category: var.category(),
        }
    }
}

#[must_use]
pub fn env_var_entries() -> Vec<EnvVarEntry> {
    EnvVar::all()
        .iter()
        .copied()
        .map(EnvVarEntry::from_env_var)
        .collect()
}

#[derive(Clone, Debug)]
pub struct ExitCodeEntry {
    pub code: u8,
    pub name: &'static str,
    pub description: &'static str,
}

pub const EXIT_CODES: &[ExitCodeEntry] = &[
    ExitCodeEntry {
        code: 0,
        name: "success",
        description: "Command completed successfully",
    },
    ExitCodeEntry {
        code: 1,
        name: "usage",
        description: "Invalid arguments or usage error",
    },
    ExitCodeEntry {
        code: 2,
        name: "configuration",
        description: "Configuration file error or invalid settings",
    },
    ExitCodeEntry {
        code: 3,
        name: "storage",
        description: "Database or storage error",
    },
    ExitCodeEntry {
        code: 4,
        name: "search_index",
        description: "Search index error or index not found",
    },
    ExitCodeEntry {
        code: 5,
        name: "import",
        description: "Import operation failed",
    },
    ExitCodeEntry {
        code: 6,
        name: "degraded",
        description: "Operation could not satisfy required mode",
    },
    ExitCodeEntry {
        code: 7,
        name: "policy",
        description: "Policy denied the operation",
    },
    ExitCodeEntry {
        code: 8,
        name: "migration",
        description: "Database migration required",
    },
    ExitCodeEntry {
        code: 9,
        name: "eval_failure",
        description: "Evaluation completed and found regressions",
    },
    ExitCodeEntry {
        code: 10,
        name: "workspace_store_missing",
        description: "The addressed workspace has no initialized ee store",
    },
    ExitCodeEntry {
        code: 130,
        name: "cancelled",
        description: "Operation was cancelled by the caller, deadline, or runtime budget",
    },
];

#[derive(Clone, Debug)]
pub struct FieldLevelEntry {
    pub name: &'static str,
    pub flag: &'static str,
    pub includes: &'static str,
    pub use_case: &'static str,
}

pub const FIELD_LEVELS: &[FieldLevelEntry] = &[
    FieldLevelEntry {
        name: "minimal",
        flag: "--fields minimal",
        includes: "command, version, status only",
        use_case: "Bare minimum for scripting status checks",
    },
    FieldLevelEntry {
        name: "summary",
        flag: "--fields summary",
        includes: "+ top-level metrics and summary counts",
        use_case: "Quick overview without array details",
    },
    FieldLevelEntry {
        name: "standard",
        flag: "--fields standard",
        includes: "+ arrays with items (default)",
        use_case: "Normal operation with all relevant data",
    },
    FieldLevelEntry {
        name: "full",
        flag: "--fields full",
        includes: "+ provenance, why, repair hints, debug info",
        use_case: "Debugging and detailed analysis",
    },
];

#[derive(Clone, Debug)]
pub struct FormatEntry {
    pub name: &'static str,
    pub flag: &'static str,
    pub description: &'static str,
    pub machine_readable: bool,
}

pub const OUTPUT_FORMATS: &[FormatEntry] = &[
    FormatEntry {
        name: "human",
        flag: "--format human",
        description: "Human-readable text output (default)",
        machine_readable: false,
    },
    FormatEntry {
        name: "json",
        flag: "--format json or --json or -j",
        description: "Structured JSON with ee.response.v2 envelope; size-governable via \
                      --max-output-tokens / EE_MAX_OUTPUT_TOKENS with --cursor resume (ADR 0063)",
        machine_readable: true,
    },
    FormatEntry {
        name: "toon",
        flag: "--format toon",
        description: "Token-efficient hierarchical notation for LLM context; 20-40% fewer tokens than JSON; decode-compatible but not for storage/hooks/MCP",
        machine_readable: false,
    },
    FormatEntry {
        name: "markdown",
        flag: "--format markdown",
        description: "Markdown context output for direct agent prompt inclusion",
        machine_readable: false,
    },
    FormatEntry {
        name: "jsonl",
        flag: "--format jsonl",
        description: "Line-delimited JSON for streaming",
        machine_readable: true,
    },
    FormatEntry {
        name: "compact",
        flag: "--format compact",
        description: "Minimal JSON without whitespace",
        machine_readable: true,
    },
    FormatEntry {
        name: "hook",
        flag: "--format hook",
        description: "Format optimized for hook consumption",
        machine_readable: true,
    },
    FormatEntry {
        name: "mermaid",
        flag: "--format mermaid",
        description: "Mermaid graph projection for commands with diagram output",
        machine_readable: false,
    },
];

#[derive(Clone, Debug)]
pub struct ContractEntry {
    pub name: &'static str,
    pub schema: &'static str,
    pub description: &'static str,
    pub stability: &'static str,
}

pub const CONTRACTS: &[ContractEntry] = &[
    ContractEntry {
        name: "response",
        schema: RESPONSE_SCHEMA_V2,
        description: "Standard success response envelope with data payload",
        stability: "stable",
    },
    ContractEntry {
        name: "pack",
        schema: PACK_SCHEMA_V2,
        description: "Context pack payload; task-lens runs also persist lens id, version, and hash in the pack replay ledger for auditability",
        stability: "stable",
    },
    ContractEntry {
        name: "error",
        schema: ERROR_SCHEMA_V2,
        description: "Standard error response with code, message, and repair hint",
        stability: "stable",
    },
    ContractEntry {
        name: "preflight_guard",
        schema: crate::core::preflight_guard::PREFLIGHT_GUARD_SCHEMA_V1,
        description: "Direct advisory command-risk memory response for ee preflight check/guard; intentionally not wrapped in ee.response.v2, always exits successfully when the report is generated, and never grants or denies shell execution",
        stability: "stable",
    },
    ContractEntry {
        name: "impact",
        schema: crate::core::impact::IMPACT_SCHEMA_V1,
        description: "Impact lookup payload for memories anchored to paths, symbols, commands, env vars, schemas, degraded codes, dependencies, or config keys",
        stability: "stable",
    },
    ContractEntry {
        name: "recall",
        schema: RECALL_SCHEMA_V1,
        description: "Code-anchored recall payload for path, symbol, and git-diff selectors; carried under ee.response.v2 data.recall",
        stability: "stable",
    },
    ContractEntry {
        name: "typed_memory_fields",
        schema: TYPED_MEMORY_FIELDS_SCHEMA_V2,
        description: "Canonical typed sidecar envelope for registry-backed memory kinds; v1 sidecars validate unchanged and canonicalize to v2",
        stability: "stable",
    },
    ContractEntry {
        name: "decide_record",
        schema: DECIDE_RECORD_SCHEMA_V1,
        description: "Decision recording payload for ee decide record; wraps a decision-kind memory plus optional supersede side effects",
        stability: "stable",
    },
    ContractEntry {
        name: "decide_list",
        schema: DECIDE_LIST_SCHEMA_V1,
        description: "Decision log payload for ee decide list; returns current heads by default and superseded history when requested",
        stability: "stable",
    },
    ContractEntry {
        name: "decide_revisit",
        schema: DECIDE_REVISIT_SCHEMA_V1,
        description: "Decision revisit payload for due and near-due decision-kind memories",
        stability: "stable",
    },
    ContractEntry {
        name: "hook_harness_install",
        schema: HARNESS_HOOK_INSTALL_SCHEMA_V1,
        description: "Agent-harness hook generation, install, and undo report for Claude Code, Codex, and capability-gap targets",
        stability: "stable",
    },
    ContractEntry {
        name: "docs_bootstrap_run",
        schema: DOCS_BOOTSTRAP_RUN_SCHEMA_V1,
        description: "Docs bootstrap dry-run payload with allowlisted sources, candidate proposals, parser version, and degraded read/quarantine signals",
        stability: "stable",
    },
    ContractEntry {
        name: "docs_bootstrap_apply",
        schema: DOCS_BOOTSTRAP_APPLY_SCHEMA_V1,
        description: "Docs bootstrap curation apply payload with materialized, approved, skipped, blocked, and durable-mutation counts",
        stability: "stable",
    },
];

#[derive(Clone, Debug)]
pub struct ExampleEntry {
    pub title: &'static str,
    pub description: &'static str,
    pub command: &'static str,
    pub category: &'static str,
}

pub const EXAMPLES: &[ExampleEntry] = &[
    ExampleEntry {
        title: "Pre-task context",
        description: "Get relevant context before starting a task",
        command: "ee pack \"fix failing CI tests\" --workspace . --max-tokens 4000 --json",
        category: "context",
    },
    ExampleEntry {
        title: "Task lens policy",
        description: "Inspect the bugfix lens before applying it to a context pack",
        command: "ee lens explain bugfix --json",
        category: "context",
    },
    ExampleEntry {
        title: "Task-lens context pack",
        description: "Apply a named lens and persist its lens hash in the pack replay ledger",
        command: "ee pack \"debug failing test\" --workspace . --lens bugfix --json",
        category: "context",
    },
    ExampleEntry {
        title: "Store a procedural rule",
        description: "Remember a learned best practice",
        command: "ee remember --level procedural --kind rule \"Run cargo fmt before commit\" --json",
        category: "memory",
    },
    ExampleEntry {
        title: "Search memories",
        description: "Find relevant past context",
        command: "ee search \"authentication error\" --limit 5 --json",
        category: "search",
    },
    ExampleEntry {
        title: "Ask a direct question",
        description: "Return an extractive answer with citations, conflict sides, or calibrated abstention",
        command: "ee ask \"what runtime does this project use\" --workspace . --json",
        category: "search",
    },
    ExampleEntry {
        title: "Store typed failure evidence",
        description: "Record validated typed fields directly without encoding them into prose",
        command: "ee remember \"Prefetch regressed and was reverted.\" --level episodic --kind failure --field family=aggressive-prefetch --field \"cause=cache pollution\" --field reverted-at-sha=9af3c21 --json",
        category: "memory",
    },
    ExampleEntry {
        title: "Search typed memory fields",
        description: "Filter memories by explicitly assigned or body-extracted typed sidecar fields",
        command: "ee search \"prefetch regression\" --kind failure --field family=aggressive-prefetch --json",
        category: "search",
    },
    ExampleEntry {
        title: "Record a decision",
        description: "Create a decision-kind memory with typed fields and fork protection",
        command: "ee decide record \"storage engine\" --chosen FrankenSQLite --alternative rusqlite --rationale \"SQLModel integration and forbidden dependency policy\" --revisit-by +90d --json",
        category: "memory",
    },
    ExampleEntry {
        title: "List decision history",
        description: "Check current decision heads before proposing architecture changes",
        command: "ee decide list --about storage --json",
        category: "memory",
    },
    ExampleEntry {
        title: "Review due decisions",
        description: "Find decisions whose revisit horizon is due or near due",
        command: "ee decide revisit --warning-days 14 --json",
        category: "memory",
    },
    ExampleEntry {
        title: "Impact lookup",
        description: "Find memories attached to a path or typed surface before editing it",
        command: "ee impact src/core/search.rs --workspace . --json",
        category: "context",
    },
    ExampleEntry {
        title: "Recall before editing",
        description: "Fetch memories anchored to the file or diff you are about to touch",
        command: "ee recall --path src/core/search.rs --workspace . --budget-tokens 400 --format markdown",
        category: "context",
    },
    ExampleEntry {
        title: "Preview recall hook install",
        description: "Print a harness hook plan before mutating agent settings",
        command: "ee hook claude-code --print --workspace . --json",
        category: "hooks",
    },
    ExampleEntry {
        title: "Check system health",
        description: "Verify ee is ready to use",
        command: "ee health --json",
        category: "diagnostics",
    },
    ExampleEntry {
        title: "Detailed status",
        description: "Get full capability and degradation info",
        command: "ee status --fields full --json",
        category: "diagnostics",
    },
    ExampleEntry {
        title: "Discover schemas",
        description: "List available response schemas",
        command: "ee schema list --json",
        category: "discovery",
    },
    ExampleEntry {
        title: "Self-introspection",
        description: "Get command/schema/error maps for agent tooling",
        command: "ee introspect --json",
        category: "discovery",
    },
    ExampleEntry {
        title: "Import CASS sessions",
        description: "Import evidence from coding agent session search",
        command: "ee import cass --limit 20 --json",
        category: "import",
    },
    ExampleEntry {
        title: "Docs bootstrap dry-run",
        description: "Compile repository docs into reviewable candidates without creating memories",
        command: "ee bootstrap docs --dry-run --json",
        category: "curation",
    },
    ExampleEntry {
        title: "Fix plan",
        description: "Get actionable repair steps for issues",
        command: "ee doctor --fix-plan --json",
        category: "diagnostics",
    },
    ExampleEntry {
        title: "Contention triage",
        description: "Rank hot-path concurrency bottlenecks (write lock, flock gate, read pool) with copy-paste remediation commands; add --use-daemon for live daemon-side counters",
        command: "ee diag contention --robot-triage --json",
        category: "diagnostics",
    },
    ExampleEntry {
        title: "Inspect command-risk memory",
        description: "Retrieve advisory risk patterns and memories for a command; this example encodes `git status`, and --cmd-base64 or --stdin keeps inspected literals off argv",
        command: "ee preflight check --cmd-base64 Z2l0IHN0YXR1cw== --json",
        category: "safety",
    },
    ExampleEntry {
        title: "Token-efficient status",
        description: "Use TOON format for LLM context windows",
        command: "ee status --format toon",
        category: "formats",
    },
    ExampleEntry {
        title: "TOON context pack",
        description: "Get context with 20-40% fewer tokens than JSON",
        command: "ee pack \"task\" --workspace . --format toon",
        category: "formats",
    },
];

#[derive(Clone, Debug)]
pub struct FailureBranchEntry {
    pub condition: &'static str,
    pub jq: &'static str,
    pub next_action: &'static str,
}

#[derive(Clone, Debug)]
pub struct AgentDocsRecipeEntry {
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub category: &'static str,
    pub command: &'static str,
    pub jq: &'static str,
    pub success_check: &'static str,
    pub failure_branches: &'static [FailureBranchEntry],
}

pub const CONTEXT_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "context pack command returns an error envelope",
        jq: r#".error | {code, message, repair}"#,
        next_action: "Run the repair command when present, then retry with the same workspace and query.",
    },
    FailureBranchEntry {
        condition: "semantic retrieval is degraded",
        jq: r#".data.degraded[]? | select(.code == "semantic_unavailable")"#,
        next_action: "Continue with lexical results when acceptable, or run `ee index reembed --workspace .`.",
    },
];

pub const ASK_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "ask returns a calibrated abstention",
        jq: r#".degraded[]? | select(.code == "no_confident_answer")"#,
        next_action: "Inspect `data.nearestEvidence[]`; add or import grounded source memories when the question should be answerable.",
    },
    FailureBranchEntry {
        condition: "ask finds conflicting evidence",
        jq: r#".degraded[]? | select(.code == "ask_conflicting_evidence")"#,
        next_action: "Read `data.sides[]` and use `ee conflict explain <memory-id> --json` before choosing a side.",
    },
    FailureBranchEntry {
        condition: "fail-closed confidence mode trips",
        jq: r#".error | select(.code == "unsatisfied_degraded_mode") | {message, repair}"#,
        next_action: "Treat exit 6 as a real gate failure; lower the threshold only with an explicit caller policy.",
    },
];

pub const TASK_LENS_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "requested lens is unknown or invalid",
        jq: r#".error | select(.code == "usage") | {message, repair}"#,
        next_action: "Run `ee lens list --json`, then retry with a listed lens id or pass `--no-lens`.",
    },
    FailureBranchEntry {
        condition: "pack replay does not show lens metadata",
        jq: r#".data.replay.ledger.taskLens // null"#,
        next_action: "Ensure the pack was created without `--read-only` or `--no-persist`; persisted packs should carry taskLens id, version, and lensHash in the replay ledger.",
    },
];

pub const STATUS_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "database migration is required",
        jq: r#".. | objects | select(.code? == "migration_required")"#,
        next_action: "Run `ee migrate run --workspace . --json` before mutating memory state.",
    },
    FailureBranchEntry {
        condition: "storage or index capability is unavailable",
        jq: r#".data.degraded[]? | select(.code | test("storage|index"))"#,
        next_action: "Use the reported repair field or run `ee doctor --full --json` for a full repair plan.",
    },
];

pub const RETRIEVAL_TUNING_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "the evidence gate abstained (too few usable outcome triples or margin under threshold)",
        jq: r#".. | objects | select(.code? == "insufficient_outcome_evidence")"#,
        next_action: "Keep the incumbent weights and keep emitting outcomes (`ee outcome <memory-id> --pack <pack-id> --item <n> --signal helpful|harmful`); rerun after more real traffic.",
    },
    FailureBranchEntry {
        condition: "the evaluator ran but could not persist the report",
        jq: r#".. | objects | select(.code? == "shadow_report_not_persisted")"#,
        next_action: "Fix workspace .ee writability (`ee doctor --workspace . --json`) and rerun before any promote.",
    },
];

pub const RESUME_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "the store has no episodic session evidence to resume from",
        jq: r#".degraded[]? | select(.code == "resume_no_session_evidence")"#,
        next_action: "Check data.report.nearbyStores for the populated store (a monorepo subdirectory often owns it), or start capturing session end-state with `ee remember \"<note>\" --level episodic --workspace . --json`.",
    },
    FailureBranchEntry {
        condition: "surfaced items carry stale markers",
        jq: r#"[.. | objects | select(.stale? != null) | {memoryId, supersededBy: .stale.supersededBy}]"#,
        next_action: "Trust the newer memory named in supersededBy; do not act on the flagged note.",
    },
];

pub const GLOBAL_LANE_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "the global lane is disabled or empty for this workspace",
        jq: r#".. | objects | select(.code? == "global_lane_disabled")"#,
        next_action: "Author a first shared rule with `ee remember --global \"<rule>\" --level procedural --kind rule --json`, or enable participation via `ee config set memory.participate true --workspace . --json`.",
    },
    FailureBranchEntry {
        condition: "workspace and global lanes contradict each other",
        jq: r#".. | objects | select(.code? == "global_lane_conflict_deferred")"#,
        next_action: "Review both sides and tombstone or revise the stale lane row; neither side is dropped automatically.",
    },
];

pub const DOCTOR_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "one or more checks failed",
        jq: r#".data.checks[]? | select(.status != "ok") | {name, status, code, repair}"#,
        next_action: "Apply failing check repairs in order and rerun `ee doctor --full --json`.",
    },
    FailureBranchEntry {
        condition: "doctor command itself returns an error envelope",
        jq: r#".error | {code, message, repair}"#,
        next_action: "Treat the error code as the stable branch key and avoid parsing stderr for automation.",
    },
];

pub const PLAN_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "goal cannot be classified confidently",
        jq: r#".data.degradedBranches[]? | select(.condition | test("unknown|ambiguous"))"#,
        next_action: "Run `ee plan recipe list --json` and select a recipe explicitly.",
    },
    FailureBranchEntry {
        condition: "selected recipe includes degraded branches",
        jq: r#".data.degradedBranches[]? | {condition, command, reason}"#,
        next_action: "Resolve the listed precondition before applying the real command sequence.",
    },
];

pub const CONTRACT_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "expected schema is absent",
        jq: r#".data.contracts[]? | select(.schema == "ee.response.v2")"#,
        next_action: "Pin automation to the published schema list and stop if the expected schema is missing.",
    },
    FailureBranchEntry {
        condition: "agent-docs topic is misspelled",
        jq: r#".error | select(.code == "usage") | {message, repair}"#,
        next_action: "Run `ee agent-docs --json` and select a topic from `.data.topics[].name`.",
    },
];

pub const IMPACT_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "impact lookup command returns an error envelope",
        jq: r#".error | {code, message, repair}"#,
        next_action: "Apply the repair command when present, then retry with the same workspace and surface.",
    },
    FailureBranchEntry {
        condition: "no anchored memories are found for the surface",
        jq: r#".data | select((.exactAnchorCount // 0) == 0 and (.fallbackCount // 0) == 0)"#,
        next_action: "Continue with ordinary `ee search` or `ee pack`; absence of impact rows is not evidence that the surface has no relevant history.",
    },
];

pub const RECALL_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "no anchors have been indexed for the workspace",
        jq: r#".degraded[]? | select(.code == "anchor_index_empty")"#,
        next_action: "Continue without injected recall context; create anchored memories naturally, or run `ee index rebuild --workspace .` after memories exist.",
    },
    FailureBranchEntry {
        condition: "anchor reverse index is stale",
        jq: r#".degraded[]? | select(.code == "anchor_index_stale")"#,
        next_action: "Run `ee index rebuild --workspace .`, then retry the same recall selector.",
    },
    FailureBranchEntry {
        condition: "git diff selector could not be read",
        jq: r#".degraded[]? | select(.code == "recall_git_unavailable")"#,
        next_action: "Retry with explicit `--path` selectors; recall must not block the edit path.",
    },
];

pub const TYPED_SEARCH_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "typed field filter is malformed",
        jq: r#".error | select(.code == "usage") | {message, repair}"#,
        next_action: "Pass typed filters as `--field name=value`; keep raw query text in the positional query instead of the field value.",
    },
    FailureBranchEntry {
        condition: "search succeeds but no typed sidecar fields match",
        jq: r#".data | select((.results // []) | length == 0)"#,
        next_action: "Inspect the source memory with `ee memory show <id> --json`; bare memories and unstructured bodies intentionally have no fabricated typed fields.",
    },
];

pub const DECIDE_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "a live decision already exists for the normalized topic",
        jq: r#".error.details | select(.failureModeCode == "decision_topic_requires_supersedes") | {priorMemoryId, suggestedCommand}"#,
        next_action: "Read the prior decision with `ee memory show <priorMemoryId> --json`; replace it only by rerunning record with the suggested --supersedes flag.",
    },
    FailureBranchEntry {
        condition: "the superseded decision has a different normalized topic",
        jq: r#".error.details | select(.failureModeCode == "decision_supersedes_topic_mismatch") | {supersedes, priorNormalizedTopic, newNormalizedTopic}"#,
        next_action: "Use a predecessor from the same topic chain or record a new topic without pretending it replaces the prior one.",
    },
    FailureBranchEntry {
        condition: "a revisit timestamp is malformed",
        jq: r#".error | select(.code == "usage") | {message, repair}"#,
        next_action: "Use RFC3339 or a relative day interval such as `+90d`, then rerun the same command.",
    },
];

pub const DOCS_BOOTSTRAP_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "dry-run reports degraded source handling",
        jq: r#".data.degraded[]? | {code, severity, path, repair}"#,
        next_action: "Treat degraded source reads as bounded input gaps; fix symlinks, permissions, UTF-8, or size limits before applying.",
    },
    FailureBranchEntry {
        condition: "candidate text was quarantined before curation",
        jq: r#".data.curateQuarantine[]? | {code, sourcePath, instructionRisk, rejectedReasons}"#,
        next_action: "Review quarantine rows manually; quarantined docs bootstrap text must not become memory without curation review.",
    },
    FailureBranchEntry {
        condition: "apply is attempted without curation approval",
        jq: r#".error | select(.code == "usage") | {message, repair}"#,
        next_action: "Run `ee bootstrap apply <run-id> --approved-only --json` only after approving curation candidates.",
    },
];

// ============================================================================
// EE-DIST-005: Install/Update Recipe Failure Branches
// ============================================================================

pub const INSTALL_CHECK_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "install directory cannot be written",
        jq: r#".data.findings[]? | select(.code == "install_dir_not_writable") | {message, nextAction}"#,
        next_action: "Choose a writable --install-dir or create the parent directory with appropriate permissions.",
    },
    FailureBranchEntry {
        condition: "multiple or shadowing ee binaries are found in PATH",
        jq: r#".data.findings[]? | select(.code == "duplicate_path_binary" or .code == "current_binary_shadowed") | {message, nextAction}"#,
        next_action: "Remove stale duplicates or make the intended install directory appear first in PATH.",
    },
    FailureBranchEntry {
        condition: "no deterministic update source is configured",
        jq: r#".data.findings[]? | select(.code == "no_update_source_configured" or .code == "offline_no_manifest") | {message, nextAction}"#,
        next_action: "Pass --manifest for deterministic offline install or update planning.",
    },
];

pub const UPDATE_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "manifest is missing in offline update planning",
        jq: r#".data.findings[]? | select(.code == "manifest_missing" or .code == "offline_no_manifest") | {code, message, nextAction}"#,
        next_action: "Pass --manifest pointing at a local release manifest and rerun `ee update --dry-run --offline --json`.",
    },
    FailureBranchEntry {
        condition: "artifact checksum cannot be verified yet",
        jq: r#".data.findings[]? | select(.code == "checksum_verification_pending") | {message, nextAction}"#,
        next_action: "Pass --artifact-root pointing at downloaded release artifacts before treating the plan as apply-ready.",
    },
    FailureBranchEntry {
        condition: "update would downgrade the installed binary",
        jq: r#".data.findings[]? | select(.code == "would_downgrade") | {message, nextAction}"#,
        next_action: "Rerun the install/update plan with an explicit --pin value and --allow-downgrade only when rollback is intentional.",
    },
    FailureBranchEntry {
        condition: "target artifact is not available for this platform",
        jq: r#".data.findings[]? | select(.code == "target_mismatch" or .code == "unsupported_target") | {code, message, nextAction}"#,
        next_action: "Choose a supported --target from the manifest or publish the missing artifact before planning the update.",
    },
];

pub const PIN_VERSION_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "manifest or target artifact is unavailable",
        jq: r#".data.findings[]? | select(.code == "manifest_missing" or .code == "target_mismatch" or .code == "artifact_missing") | {code, message, nextAction}"#,
        next_action: "Pass --manifest and, when verifying artifacts, --artifact-root that contains the release files.",
    },
    FailureBranchEntry {
        condition: "pinned version would downgrade the installed binary",
        jq: r#".data.findings[]? | select(.code == "would_downgrade") | {message, nextAction}"#,
        next_action: "Add --allow-downgrade only when the rollback is intentional and reviewed.",
    },
];

pub const SUPPORT_BUNDLE_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "bundle creation failed due to permissions",
        jq: r#".error | select(.code == "storage" or .code == "configuration") | {message, repair}"#,
        next_action: "Ensure write permissions for the output directory or specify an alternate path with `--out`.",
    },
    FailureBranchEntry {
        condition: "dry-run reports no output path",
        jq: r#".data | select(.dryRun == true) | {filesCollected, totalSizeBytes, outputPath}"#,
        next_action: "Rerun without --dry-run and pass --out <dir> when an actual bundle artifact is needed.",
    },
];

pub const PRIMER_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "primer assembled fresh instead of serving the cache",
        jq: r#".data.primer.degraded[]? // .data.degraded[]? | select(.code == "primer_cache_cold")"#,
        next_action: "Informational: the same call just warmed primer_cache, so the next identical call is a byte-identical hit. Never retry on this code.",
    },
    FailureBranchEntry {
        condition: "loadBearing section is honestly omitted",
        jq: r#".data.primer.degraded[]? // .data.degraded[]? | select(.code == "primer_graph_unavailable")"#,
        next_action: "Run `ee graph centrality-refresh --workspace .` to persist centrality rows; the next assembly includes the loadBearing section.",
    },
];

pub const AGENTSMD_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "managed block was hand-edited since the last export",
        jq: r#".data.degraded[]? | select(.code == "agentsmd_unmanaged_edit_detected")"#,
        next_action: "Review with `ee export agentsmd --workspace . --dry-run`, then re-run with --force-managed-block; the hand edit is preserved in the .ee-backup sibling.",
    },
    FailureBranchEntry {
        condition: "bridge target file is absent",
        jq: r#".data.degraded[]? | select(.code == "agentsmd_file_missing")"#,
        next_action: "Pass --create on export to materialize the file with a fresh managed block; import and drift treat a missing file as an honest empty result.",
    },
];

pub const HOOK_HARNESS_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "requested harness has no writable hook surface",
        jq: r#".data.capabilityGaps[]? | select(.code == "harness_hooks_unsupported")"#,
        next_action: "Use `--print` and wire the snippet manually, or keep recall as an explicit pre-edit command.",
    },
    FailureBranchEntry {
        condition: "undo was requested but no backup exists",
        jq: r#".data.capabilityGaps[]? | select(.code == "harness_backup_missing")"#,
        next_action: "Inspect the target settings path and remove the managed ee hook block manually only after preserving the current file.",
    },
];

pub const MEMORY_HYGIENE_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "doctor report is based on a partial audit window",
        jq: r#".data.degraded[]? | select(.code == "memory_debt_audit_window_partial")"#,
        next_action: "Keep the queue usable but treat older read evidence as unknown; rerun after audit retention or scan limits are widened.",
    },
    FailureBranchEntry {
        condition: "learn gaps has no retained miss demand",
        jq: r#".degraded[]? | select(.code == "learn_gaps_no_miss_data")"#,
        next_action: "Treat this as an honest empty result, then record missing knowledge explicitly when an agent asks an unanswered question.",
    },
    FailureBranchEntry {
        condition: "requested learn-gaps window predates retention",
        jq: r#".degraded[]? | select(.code == "learn_gaps_retention_short")"#,
        next_action: "Increase `[search].query_miss_retention_days` or `EE_QUERY_MISS_RETENTION_DAYS` before the ledger is pruned when longer review windows are required.",
    },
];

pub const JOURNAL_CAPTURE_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "journal capture is disabled by config",
        jq: r#".degraded[]? | select(.code == "journal_disabled")"#,
        next_action: "Respect `[journal].enabled = false`; use explicit `ee remember` only when the operator asks for durable memory.",
    },
    FailureBranchEntry {
        condition: "distillation found entries but no proposals",
        jq: r#".degraded[]? | select(.code == "distill_no_candidates")"#,
        next_action: "Treat this as an honest empty review, then keep journaling until repeated or surprising evidence exists.",
    },
    FailureBranchEntry {
        condition: "database migration is required before journal review",
        jq: r#".error | select(.code == "migration_required") | {message, repair}"#,
        next_action: "Run the reported migration repair before retrying journal append, list, show, or distill.",
    },
];

pub const AGENT_DOC_RECIPES: &[AgentDocsRecipeEntry] = &[
    AgentDocsRecipeEntry {
        id: "local-attestation",
        title: "Attest local provenance for memory or pack",
        description: "Emit a redaction-safe bundle that attests local ee custody and hash consistency, not objective truth.",
        category: "attestation",
        command: "ee attest memory <memory-id> --workspace . --json",
        jq: r#"{subjectKind: .data.subjectKind, bundleHash: .data.bundleHash, objectiveTruthAttested: .data.objectiveTruthAttested, trustStatement: .data.trustStatement}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true and .data.objectiveTruthAttested == false"#,
        failure_branches: CONTEXT_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "pre-task-context",
        title: "Fetch task context before editing",
        description: "Retrieve a compact, provenance-bearing context pack for the current task.",
        category: "context",
        command: "ee pack \"<task>\" --workspace . --max-tokens 4000 --json",
        jq: r#".data.pack.items[]? | {memoryId, section, why}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: CONTEXT_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "task-lens-context",
        title: "Apply an inspectable task lens",
        description: "Use a named, hash-stable pack policy such as bugfix or code-review, then audit the persisted taskLens metadata through pack replay.",
        category: "context",
        command: "ee pack \"<task>\" --workspace . --lens bugfix --json",
        jq: r#"{packHash: .data.pack.hash, request: .data.request, lens: (.data.pack.taskLens // .data.taskLens // null)}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: TASK_LENS_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "impact-before-edit",
        title: "Inspect surface impact before editing",
        description: "Find anchored memories and fallback search hits for a path, symbol, command, env var, schema, degraded code, dependency, or config key.",
        category: "context",
        command: "ee impact <surface> --workspace . --json",
        jq: r#".data.results[]? | {memoryId, matchType, score, preview: .memory.contentPreview}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true and .data.schema == "ee.impact.v1""#,
        failure_branches: IMPACT_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "recall-before-edit",
        title: "Recall anchored memory before editing",
        description: "Fetch path, symbol, or diff-anchored memories before touching code, using a small hook-safe token budget.",
        category: "context",
        command: "ee recall --path <path> --workspace . --budget-tokens 400 --json",
        jq: r#".data.recall.items[]? | {memoryId, freshnessState, kind, level, anchor, repair}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true and .data.recall.schema == "ee.recall.v1""#,
        failure_branches: RECALL_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "typed-memory-search",
        title: "Filter typed memory fields",
        description: "Use explicitly assigned or body-extracted typed memory sidecars to find failures, decisions, commands, or risks without matching raw prose.",
        category: "search",
        command: "ee search \"prefetch regression\" --workspace . --kind failure --field family=aggressive-prefetch --json",
        jq: r#".data.results[]? | {memoryId, kind, typedFields: (.typedFields // .memory.typedFields // {})}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: TYPED_SEARCH_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "decide-before-rearchitecture",
        title: "Stop re-litigating settled decisions",
        description: "Query decision heads before proposing architecture changes; replace a decision only through an explicit supersede chain.",
        category: "memory",
        command: "ee decide list --about <topic> --workspace . --json",
        jq: r#".data.decisions[]? | {memoryId, topic, chosen, supersedes, chainDepth, revisitStatus, superseded}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true and .data.schema == "ee.decide.list.v1""#,
        failure_branches: DECIDE_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "direct-answer",
        title: "Ask for a cited direct answer",
        description: "Use extractive ask when a narrow question needs answer text plus byte-addressed citations, conflict sides, or an honest abstention.",
        category: "search",
        command: "ee ask \"<question>\" --workspace . --json",
        jq: r#"{answer: .data.answerText, confidence: .data.confidence, citations: [.data.citations[]? | {memoryId, span, text}], sides: (.data.sides // []), nearestEvidence: (.data.nearestEvidence // [])}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true and .data.schema == "ee.ask.v1""#,
        failure_branches: ASK_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "monthly-retrieval-tuning",
        title: "Run the monthly retrieval-weight tuning pass",
        description: "Evaluate accumulated outcome evidence offline, read the gate's verdict, and promote a winning fusion-weight overlay explicitly — determinism is preserved because adaptation is an audited config change with byte-identical rollback.",
        category: "maintenance",
        command: "ee shadow run --policy candidate.retrieval.outcome_tuned_weights --workspace . --json",
        jq: r#"{abstained: .data.report.abstained, reason: .data.report.abstentionReason, promotable: .data.report.promotable, winner: .data.report.winner, degraded: (.degraded // [])}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: RETRIEVAL_TUNING_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "session-resume",
        title: "Resume a session — where was I?",
        description: "Start every returning session with the resume bundle instead of a task-less orient: last sessions' end-state newest first, revisit-conditioned decisions, next/queue/blocking-tagged items, and staleness flags on superseded notes.",
        category: "getting-started",
        command: "ee resume --workspace . --json",
        jq: r#"{sessions: [.data.report.sessions[]? | {label, memberCount, newestAt}], openDecisions: [.data.report.openLoops.revisitDecisions[]? | {topic, revisitBy}], queued: [.data.report.openLoops.taggedItems[]? | {memoryId, tags}], staleCount: .data.report.staleCount}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true and .data.report.schema == "ee.resume.v1""#,
        failure_branches: RESUME_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "new-repo-day-one",
        title: "Get day-one context in a brand-new repo",
        description: "In a freshly initialized workspace the user-global lane already carries the user's cross-project rules, so primer/search return provenance-backed guidance before any local memory exists.",
        category: "getting-started",
        command: "ee primer --workspace . --json",
        jq: r#"{rules: [.data.primer.sections[]? | select(.name == "rules") | .items[] | {line, lane: (.provenance[0].source_type // "workspace")}], degraded: (.degraded // [])}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: GLOBAL_LANE_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "workspace-health",
        title: "Check workspace health",
        description: "Inspect storage, index, and degraded capability state before relying on memory output.",
        category: "diagnostics",
        command: "ee status --workspace . --json",
        jq: r#"{database: .data.database, index: .data.index, degraded: (.data.degraded // [])}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: STATUS_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "repair-plan",
        title: "Collect repair actions",
        description: "Use doctor output as the stable source of repair commands for automation.",
        category: "diagnostics",
        command: "ee doctor --full --json",
        jq: r#".data.checks[]? | select(.status != "ok") | {name, code, repair}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: DOCTOR_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "weekly-memory-hygiene",
        title: "Run weekly memory hygiene",
        description: "Review content rot, execute the top curation repairs deliberately, then inspect demand-driven learn gaps for memories to capture.",
        category: "curation",
        command: "ee curate doctor --workspace . --limit 5 --trend --json",
        jq: r#".data | {summary, queue: [.queue[]? | {memoryId, class, severity, command: .suggestedAction.value}], trend}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true and .data.schema == "ee.curate.doctor.v1""#,
        failure_branches: MEMORY_HYGIENE_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "end-of-session-journal-flush",
        title: "End-of-session journal flush",
        description: "Review append-only journal observations at session end and draft evidence-backed curation candidates without mutating memory by default.",
        category: "curation",
        command: "ee journal distill --workspace . --dry-run --json",
        jq: r#"{schema: .data.schema, dryRun: .data.dryRun, proposals: [.data.proposals[]? | {proposalId, action, kind, level, clusterSize}], degraded: (.data.degraded // .degraded // [])}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true and .data.schema == "ee.journal.distill.v1" and .data.dryRun == true"#,
        failure_branches: JOURNAL_CAPTURE_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "grade-pack-item",
        title: "Grade the pack item you just used",
        description: "Attach helpful or harmful outcome feedback to a specific persisted pack item so future ranking learns from real usage.",
        category: "feedback",
        command: "ee outcome --pack <pack-id> --item <n> --signal helpful --reason \"<why>\" --workspace . --json",
        jq: r#"{status: .data.status, targetId: .data.targetId, targetType: .data.targetType, feedback: .data.feedback, degraded: (.data.degraded // .degraded // [])}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: JOURNAL_CAPTURE_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "goal-to-recipe",
        title: "Resolve an agent goal to commands",
        description: "Map a natural-language goal to a deterministic recipe before running a workflow.",
        category: "planning",
        command: "ee plan goal \"<goal>\" --json",
        jq: r#"{recipeId: .data.recipeId, steps: [.data.steps[]?.command], degraded: (.data.degradedBranches // [])}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: PLAN_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "contract-discovery",
        title: "Discover machine contracts",
        description: "List stable response contracts before writing or updating agent parsers.",
        category: "contracts",
        command: "ee agent-docs contracts --json",
        jq: r#".data.contracts[] | {name, schema, stability}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: CONTRACT_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "docs-bootstrap-cold-start",
        title: "Compile repo docs into bootstrap candidates",
        description: "Cold-start memory review from allowlisted docs without creating memories during dry-run.",
        category: "curation",
        command: "ee bootstrap docs --dry-run --json",
        jq: r#"{runId: .data.runId, parserVersion: .data.parserVersion, candidates: (.data.candidates | length), durableMutation: .data.durableMutation, degraded: (.data.degraded // [])}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true and .data.schema == "ee.bootstrap.docs.run.v1" and .data.parserVersion == "docs-bootstrap-v1" and .data.durableMutation == false"#,
        failure_branches: DOCS_BOOTSTRAP_RECIPE_FAILURES,
    },
    // EE-DIST-005: Install/Update/Recovery Recipes
    AgentDocsRecipeEntry {
        id: "install-check",
        title: "Verify ee installation integrity",
        description: "Check binary presence, checksum, version currency, and PATH conflicts before relying on ee.",
        category: "distribution",
        command: "ee install check --json",
        jq: r#"{currentBinary: .data.currentBinary.path, version: .data.version, pathStatus: .data.path.status, findings: [.data.findings[]? | {code, message, nextAction}]}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true and (.data.findings | map(select(.severity == "error")) | length == 0)"#,
        failure_branches: INSTALL_CHECK_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "update-dry-run",
        title: "Preview update before applying",
        description: "Show what an update would change without modifying the installed binary.",
        category: "distribution",
        command: "ee update --dry-run --json",
        jq: r#"{currentVersion: .data.currentVersion, targetVersion: .data.targetVersion, status: .data.status, verification: .data.verification, findings: [.data.findings[]? | {code, message, nextAction}]}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: UPDATE_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "version-pin",
        title: "Pin ee to a specific version",
        description: "Plan an install from a release manifest pinned to a known version.",
        category: "distribution",
        command: "ee install plan --manifest <manifest> --pin <version> --json",
        jq: r#"{currentVersion: .data.currentVersion, targetVersion: .data.targetVersion, pinnedVersion: .data.pinnedVersion, status: .data.status}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: PIN_VERSION_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "checksum-recovery",
        title: "Recover from checksum mismatch",
        description: "Re-plan a verified install when artifact checksum validation fails.",
        category: "distribution",
        command: "ee install plan --manifest <manifest> --artifact-root <artifacts> --json",
        jq: r#"{status: .data.status, checksumStatus: .data.verification.checksumStatus, findings: [.data.findings[]? | {code, message, nextAction}]}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true and (.data.verification.checksumStatus == "verified" or .data.status == "idempotent")"#,
        failure_branches: PIN_VERSION_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "duplicate-binary-fix",
        title: "Resolve duplicate ee binaries in PATH",
        description: "Identify and remove conflicting ee installations when multiple binaries are found.",
        category: "distribution",
        command: "ee install check --json",
        jq: r#"{firstBinary: .data.path.firstBinary, duplicateCount: .data.path.duplicateCount, findings: [.data.findings[]? | {code, message, nextAction}]}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: INSTALL_CHECK_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "offline-update-posture",
        title: "Check offline update readiness",
        description: "Verify cached update availability when network is unavailable.",
        category: "distribution",
        command: "ee update --dry-run --offline --json",
        jq: r#"{status: .data.status, currentVersion: .data.currentVersion, targetVersion: .data.targetVersion, updateSource: .data.verification.manifestStatus, findings: [.data.findings[]? | {code, message, nextAction}]}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: UPDATE_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "update-failure-bundle",
        title: "Collect support bundle for failed update",
        description: "Gather diagnostic evidence when an install or update fails for support handoff.",
        category: "distribution",
        command: "ee support bundle --dry-run --json",
        jq: r#"{outputPath: .data.outputPath, totalSizeBytes: .data.totalSizeBytes, filesCollected: .data.filesCollected, redaction: .data.redactionSummary}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: SUPPORT_BUNDLE_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "cold-session-primer",
        title: "Cold session start with the workspace primer",
        description: "Fold the cached, provenance-backed workspace charter into the first orientation call of a session.",
        category: "context",
        command: "ee orient \"<task>\" --include-primer --fast --json",
        jq: r#"{posture: .data.posture, primerSections: [.data.primer.sections[]? | {name, items: (.items | length)}], cacheHit: .data.primer.cache_hit, degraded: (.data.primer.degraded // [])}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: PRIMER_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "keep-agentsmd-honest",
        title: "Keep AGENTS.md honest with the bridge",
        description: "Audit the managed AGENTS.md block against memory (stale export, contradictions, missing rules) before trusting the file; wire the same pair into CI.",
        category: "context",
        command: "ee diag agentsmd-drift --workspace . --json",
        jq: r#"{stale: .data.managedBlock.stale, hashMatches: .data.managedBlock.hashMatches, contradictions: (.data.contradictions | length), missingRules: (.data.missingRules | length), suggested: .data.suggestedCommands}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true and .data.schema == "ee.agentsmd.drift.v1""#,
        failure_branches: AGENTSMD_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "install-recall-hooks",
        title: "Install recall hooks for an agent harness",
        description: "Preview, install, or undo managed recall hook snippets for Claude Code or Codex while preserving the harness settings file.",
        category: "hooks",
        command: "ee hook claude-code --print --workspace . --json",
        jq: r#"{harness: .data.harness, mode: .data.mode, supported: .data.supported, writtenPaths: .data.writtenPaths, capabilityGaps: .data.capabilityGaps}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true and .data.schema == "ee.hook.harness_install.v1""#,
        failure_branches: HOOK_HARNESS_RECIPE_FAILURES,
    },
];

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use super::{
        AGENT_DOC_RECIPES, ASK_RECIPE_FAILURES, AgentDocsTopic, CONTRACTS, DECIDE_LIST_SCHEMA_V1,
        DECIDE_RECORD_SCHEMA_V1, DECIDE_REVISIT_SCHEMA_V1, DEFAULT_PATHS, EXAMPLES, EXIT_CODES,
        FIELD_LEVELS, GUIDE_SECTIONS, OUTPUT_FORMATS, TASK_LENS_RECIPE_FAILURES, env_var_entries,
    };
    use crate::config::EnvVar;
    use crate::models::ProcessExitCode;

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
    where
        T: Debug + PartialEq,
    {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn topic_all_returns_complete_list() -> TestResult {
        let topics = AgentDocsTopic::all();
        ensure_equal(&topics.len(), &12, "topic count")?;
        ensure_equal(
            &topics.first(),
            &Some(&AgentDocsTopic::Guide),
            "first topic",
        )
    }

    #[test]
    fn topic_round_trip_parse() -> TestResult {
        for topic in AgentDocsTopic::all() {
            let parsed = AgentDocsTopic::parse(topic.as_str());
            ensure_equal(
                &parsed,
                &Some(*topic),
                &format!("round-trip {}", topic.as_str()),
            )?;
        }
        Ok(())
    }

    #[test]
    fn topic_parse_normalizes_cli_values() -> TestResult {
        ensure_equal(
            &AgentDocsTopic::parse(" Exit-Codes "),
            &Some(AgentDocsTopic::ExitCodes),
            "hyphenated topic",
        )?;
        ensure_equal(
            &AgentDocsTopic::parse("exit_codes"),
            &Some(AgentDocsTopic::ExitCodes),
            "underscored topic",
        )?;
        ensure_equal(
            &AgentDocsTopic::parse("ExitCodes"),
            &Some(AgentDocsTopic::ExitCodes),
            "camel-case topic",
        )?;
        ensure_equal(
            &AgentDocsTopic::parse("RECIPES"),
            &Some(AgentDocsTopic::Recipes),
            "uppercase topic",
        )
    }

    #[test]
    fn topic_parse_returns_none_for_unknown() -> TestResult {
        let parsed = AgentDocsTopic::parse("unknown-topic");
        ensure_equal(&parsed, &None, "unknown topic")
    }

    #[test]
    fn guide_sections_are_non_empty() -> TestResult {
        ensure(!GUIDE_SECTIONS.is_empty(), "guide sections exist")?;
        for section in GUIDE_SECTIONS {
            ensure(!section.title.is_empty(), "guide section title non-empty")?;
            ensure(
                !section.content.is_empty(),
                "guide section content non-empty",
            )?;
        }
        Ok(())
    }

    #[test]
    fn default_paths_are_non_empty() -> TestResult {
        ensure(!DEFAULT_PATHS.is_empty(), "default paths exist")?;
        for path in DEFAULT_PATHS {
            ensure(!path.name.is_empty(), "path name non-empty")?;
            ensure(!path.default.is_empty(), "path default non-empty")?;
        }
        Ok(())
    }

    #[test]
    fn env_vars_are_non_empty() -> TestResult {
        let env_vars = env_var_entries();
        ensure(!env_vars.is_empty(), "env vars exist")?;
        for var in &env_vars {
            ensure(!var.name.is_empty(), "env var name non-empty")?;
            ensure(!var.description.is_empty(), "env var description non-empty")?;
        }
        Ok(())
    }

    #[test]
    fn env_docs_match_registry_order() -> TestResult {
        let env_vars = env_var_entries();
        for (entry, var) in env_vars.iter().zip(EnvVar::all()) {
            ensure_equal(&entry.name, &var.name(), "env docs name")?;
            ensure_equal(
                &entry.description,
                &var.description(),
                &format!("env docs description {}", var.name()),
            )?;
            ensure_equal(
                &entry.default,
                &var.default_value(),
                &format!("env docs default {}", var.name()),
            )?;
            ensure_equal(
                &entry.category,
                &var.category(),
                &format!("env docs category {}", var.name()),
            )?;
        }
        ensure_equal(
            &env_vars.len(),
            &EnvVar::all().len(),
            "env docs registry count",
        )
    }

    #[test]
    fn exit_codes_are_unique_and_sorted() -> TestResult {
        for window in EXIT_CODES.windows(2) {
            ensure(
                window[0].code < window[1].code,
                format!(
                    "exit codes must be strictly increasing: {} then {}",
                    window[0].code, window[1].code
                ),
            )?;
        }
        Ok(())
    }

    #[test]
    fn exit_codes_match_process_exit_code_contract() -> TestResult {
        let expected = [
            ("success", ProcessExitCode::Success),
            ("usage", ProcessExitCode::Usage),
            ("configuration", ProcessExitCode::Configuration),
            ("storage", ProcessExitCode::Storage),
            ("search_index", ProcessExitCode::SearchIndex),
            ("import", ProcessExitCode::Import),
            ("degraded", ProcessExitCode::UnsatisfiedDegradedMode),
            ("policy", ProcessExitCode::PolicyDenied),
            ("migration", ProcessExitCode::MigrationRequired),
            ("eval_failure", ProcessExitCode::EvalFailure),
            (
                "workspace_store_missing",
                ProcessExitCode::WorkspaceStoreMissing,
            ),
            ("cancelled", ProcessExitCode::Cancelled),
        ];
        ensure_equal(&EXIT_CODES.len(), &expected.len(), "exit code count")?;
        for (entry, (name, code)) in EXIT_CODES.iter().zip(expected) {
            ensure_equal(&entry.name, &name, "exit code name")?;
            ensure_equal(&entry.code, &(code as u8), "exit code value")?;
        }
        Ok(())
    }

    #[test]
    fn field_levels_are_non_empty() -> TestResult {
        ensure_equal(&FIELD_LEVELS.len(), &4, "field level count")?;
        for level in FIELD_LEVELS {
            ensure(!level.name.is_empty(), "field level name non-empty")?;
        }
        Ok(())
    }

    #[test]
    fn output_formats_are_non_empty() -> TestResult {
        ensure(!OUTPUT_FORMATS.is_empty(), "output formats exist")?;
        for fmt in OUTPUT_FORMATS {
            ensure(!fmt.name.is_empty(), "format name non-empty")?;
        }
        Ok(())
    }

    #[test]
    fn json_output_format_documents_current_response_envelope() -> TestResult {
        let json_format = OUTPUT_FORMATS
            .iter()
            .find(|format| format.name == "json")
            .ok_or_else(|| "json output format is documented".to_string())?;
        let legacy_schema = ["ee", "response", "v1"].join(".");

        ensure(
            json_format.description.contains("ee.response.v2"),
            "json output format documents ee.response.v2",
        )?;
        ensure(
            !json_format.description.contains(&legacy_schema),
            "json output format does not document legacy response schema",
        )
    }

    #[test]
    fn output_formats_cover_global_format_enum() -> TestResult {
        let names = OUTPUT_FORMATS
            .iter()
            .map(|format| format.name)
            .collect::<Vec<_>>();
        ensure_equal(
            &names,
            &vec![
                "human", "json", "toon", "markdown", "jsonl", "compact", "hook", "mermaid",
            ],
            "documented output formats",
        )
    }

    #[test]
    fn contracts_have_stable_schemas() -> TestResult {
        ensure(!CONTRACTS.is_empty(), "contracts exist")?;
        for contract in CONTRACTS {
            ensure_equal(&contract.stability, &"stable", "contract stability")?;
        }
        Ok(())
    }

    #[test]
    fn contracts_catalog_lists_current_response_envelope() -> TestResult {
        let response_contract = CONTRACTS
            .iter()
            .find(|contract| contract.name == "response")
            .ok_or_else(|| "response contract is documented".to_string())?;
        let legacy_schema = ["ee", "response", "v1"].join(".");

        ensure_equal(
            &response_contract.schema,
            &crate::models::RESPONSE_SCHEMA_V2,
            "response contract schema",
        )?;
        ensure(
            response_contract.schema != legacy_schema,
            "response contract must not publish legacy schema",
        )
    }

    #[test]
    fn task_lens_docs_are_registered_for_agents() -> TestResult {
        let guide = GUIDE_SECTIONS
            .iter()
            .find(|section| section.title == "Task Lenses")
            .ok_or_else(|| "task lens guide section is documented".to_string())?;
        ensure(
            guide.content.contains("ee lens list --json")
                && guide.content.contains("ee lens explain <id> --json")
                && guide.content.contains("--lens <id>"),
            "task lens guide points to list, explain, and pack --lens",
        )?;

        let pack_contract = CONTRACTS
            .iter()
            .find(|contract| contract.name == "pack")
            .ok_or_else(|| "pack contract is documented".to_string())?;
        ensure_equal(
            &pack_contract.schema,
            &crate::models::PACK_SCHEMA_V2,
            "pack contract schema",
        )?;
        ensure(
            pack_contract.description.contains("task-lens"),
            "pack contract documents task-lens ledger metadata",
        )?;

        let example = EXAMPLES
            .iter()
            .find(|example| example.title == "Task-lens context pack")
            .ok_or_else(|| "task-lens pack example is documented".to_string())?;
        ensure(
            example.command.contains("--lens bugfix"),
            "task-lens example uses --lens",
        )?;

        let recipe = AGENT_DOC_RECIPES
            .iter()
            .find(|recipe| recipe.id == "task-lens-context")
            .ok_or_else(|| "task lens recipe is documented".to_string())?;
        ensure(
            recipe.command.contains("--lens bugfix"),
            "task lens recipe command uses a named lens",
        )?;
        ensure(
            recipe.description.contains("pack replay"),
            "task lens recipe documents replay auditability",
        )?;
        ensure_equal(
            &recipe.failure_branches.len(),
            &TASK_LENS_RECIPE_FAILURES.len(),
            "task lens recipe carries dedicated failure branch count",
        )?;
        ensure(
            recipe
                .failure_branches
                .iter()
                .any(|branch| branch.next_action.contains("ee lens list --json")),
            "task lens recipe tells agents how to recover unknown lenses",
        )
    }

    #[test]
    fn contracts_catalog_lists_direct_preflight_guard_schema() -> TestResult {
        let preflight_contract = CONTRACTS
            .iter()
            .find(|contract| contract.name == "preflight_guard")
            .ok_or_else(|| "preflight guard contract is documented".to_string())?;

        ensure_equal(
            &preflight_contract.schema,
            &crate::core::preflight_guard::PREFLIGHT_GUARD_SCHEMA_V1,
            "preflight guard direct schema",
        )?;
        ensure(
            preflight_contract.description.contains("not wrapped"),
            "preflight guard docs explain the direct schema exception",
        )
    }

    #[test]
    fn contracts_catalog_lists_impact_schema() -> TestResult {
        let impact_contract = CONTRACTS
            .iter()
            .find(|contract| contract.name == "impact")
            .ok_or_else(|| "impact contract is documented".to_string())?;

        ensure_equal(
            &impact_contract.schema,
            &crate::core::impact::IMPACT_SCHEMA_V1,
            "impact contract schema",
        )?;
        ensure(
            impact_contract.description.contains("paths"),
            "impact docs mention path anchors",
        )
    }

    #[test]
    fn docs_bootstrap_contracts_and_recipe_are_registered() -> TestResult {
        let run_contract = CONTRACTS
            .iter()
            .find(|contract| contract.name == "docs_bootstrap_run")
            .ok_or_else(|| "docs bootstrap run contract is documented".to_string())?;
        ensure_equal(
            &run_contract.schema,
            &crate::core::docs_bootstrap::DOCS_BOOTSTRAP_RUN_SCHEMA_V1,
            "docs bootstrap run schema",
        )?;

        let apply_contract = CONTRACTS
            .iter()
            .find(|contract| contract.name == "docs_bootstrap_apply")
            .ok_or_else(|| "docs bootstrap apply contract is documented".to_string())?;
        ensure_equal(
            &apply_contract.schema,
            &crate::core::docs_bootstrap::DOCS_BOOTSTRAP_APPLY_SCHEMA_V1,
            "docs bootstrap apply schema",
        )?;

        let recipe = AGENT_DOC_RECIPES
            .iter()
            .find(|recipe| recipe.id == "docs-bootstrap-cold-start")
            .ok_or_else(|| "docs bootstrap cold-start recipe is documented".to_string())?;
        ensure(
            recipe.command == "ee bootstrap docs --dry-run --json",
            "docs bootstrap recipe is dry-run",
        )?;
        ensure(
            recipe
                .success_check
                .contains(crate::core::docs_bootstrap::DOCS_BOOTSTRAP_PARSER_VERSION),
            "docs bootstrap recipe checks parser version",
        )?;
        ensure(
            recipe.success_check.contains("durableMutation == false"),
            "docs bootstrap recipe documents candidates-not-memories guarantee",
        )
    }

    #[test]
    fn journal_capture_recipes_are_registered_for_agents() -> TestResult {
        let flush_recipe = AGENT_DOC_RECIPES
            .iter()
            .find(|recipe| recipe.id == "end-of-session-journal-flush")
            .ok_or_else(|| "journal distill recipe is documented".to_string())?;
        ensure(
            flush_recipe.command.contains("ee journal distill")
                && flush_recipe.command.contains("--dry-run")
                && flush_recipe
                    .success_check
                    .contains(crate::core::journal::JOURNAL_DISTILL_SCHEMA_V1),
            "journal flush recipe is dry-run and schema-pinned",
        )?;

        let grade_recipe = AGENT_DOC_RECIPES
            .iter()
            .find(|recipe| recipe.id == "grade-pack-item")
            .ok_or_else(|| "pack-item outcome recipe is documented".to_string())?;
        ensure(
            grade_recipe.command.contains("ee outcome --pack")
                && grade_recipe.command.contains("--item")
                && grade_recipe.command.contains("--signal helpful"),
            "pack-item grade recipe targets a persisted pack item",
        )?;
        ensure(
            flush_recipe
                .failure_branches
                .iter()
                .any(|branch| branch.jq.contains("journal_disabled")),
            "journal recipes document disabled-capture handling",
        )
    }

    #[test]
    fn examples_are_non_empty() -> TestResult {
        ensure(!EXAMPLES.is_empty(), "examples exist")?;
        for example in EXAMPLES {
            ensure(!example.command.is_empty(), "example command non-empty")?;
            ensure(
                example.command.starts_with("ee "),
                "example command starts with ee",
            )?;
        }
        Ok(())
    }

    #[test]
    fn examples_include_preflight_base64_and_stdin_escape_hatches() -> TestResult {
        // Find the example by its stable command surface rather than its
        // human title (retitled to "Inspect command-risk memory" when
        // preflight's advisory-only posture was clarified); the semantic
        // transport assertions below are the real contract.
        let preflight_example = EXAMPLES
            .iter()
            .find(|example| example.command.contains("preflight check"))
            .ok_or_else(|| "preflight command-transport example is documented".to_string())?;

        ensure(
            preflight_example.command.contains("--cmd-base64"),
            "preflight example uses base64 transport",
        )?;
        ensure(
            !preflight_example.command.contains('<'),
            "preflight example avoids shell-redirection-shaped placeholders",
        )?;
        ensure(
            preflight_example.description.contains("--stdin"),
            "preflight example mentions stdin transport",
        )?;
        ensure(
            preflight_example.description.contains("git status"),
            "preflight example names the encoded command",
        )
    }

    #[test]
    fn examples_and_recipes_include_impact_lookup() -> TestResult {
        let impact_example = EXAMPLES
            .iter()
            .find(|example| example.title == "Impact lookup")
            .ok_or_else(|| "impact example is documented".to_string())?;
        ensure(
            impact_example.command.starts_with("ee impact "),
            "impact example uses impact command",
        )?;

        let impact_recipe = AGENT_DOC_RECIPES
            .iter()
            .find(|recipe| recipe.id == "impact-before-edit")
            .ok_or_else(|| "impact recipe is documented".to_string())?;
        ensure(
            impact_recipe.jq.contains("matchType"),
            "impact recipe exposes stable result match type",
        )?;
        ensure(
            impact_recipe
                .success_check
                .contains(crate::core::impact::IMPACT_SCHEMA_V1),
            "impact recipe checks the impact data schema",
        )
    }

    #[test]
    fn ask_docs_are_registered_for_agents() -> TestResult {
        let guide = GUIDE_SECTIONS
            .iter()
            .find(|section| section.title == "Direct Answers")
            .ok_or_else(|| "ask guide section is documented".to_string())?;
        ensure(
            guide.content.contains("ee ask") && guide.content.contains("--require-confidence"),
            "ask guide points to direct answer and fail-closed mode",
        )?;

        let example = EXAMPLES
            .iter()
            .find(|example| example.title == "Ask a direct question")
            .ok_or_else(|| "ask example is documented".to_string())?;
        ensure(
            example.command.starts_with("ee ask ") && example.command.contains("--json"),
            "ask example uses the public JSON surface",
        )?;

        let recipe = AGENT_DOC_RECIPES
            .iter()
            .find(|recipe| recipe.id == "direct-answer")
            .ok_or_else(|| "ask recipe is documented".to_string())?;
        ensure(
            recipe
                .success_check
                .contains(crate::core::ask::ASK_SCHEMA_V1),
            "ask recipe checks ee.ask.v1",
        )?;
        ensure_equal(
            &recipe.failure_branches.len(),
            &ASK_RECIPE_FAILURES.len(),
            "ask recipe carries dedicated failure branch count",
        )?;
        ensure(
            recipe
                .failure_branches
                .iter()
                .any(|branch| branch.jq.contains("no_confident_answer")),
            "ask recipe documents abstention branch",
        )
    }

    #[test]
    fn recipes_include_jq_and_failure_branches() -> TestResult {
        ensure(!AGENT_DOC_RECIPES.is_empty(), "agent recipes exist")?;
        for recipe in AGENT_DOC_RECIPES {
            ensure(!recipe.id.is_empty(), "recipe id non-empty")?;
            ensure(
                recipe.command.starts_with("ee "),
                "recipe command starts with ee",
            )?;
            ensure(!recipe.jq.is_empty(), "recipe jq non-empty")?;
            ensure(
                !recipe.success_check.is_empty(),
                "recipe success check non-empty",
            )?;
            ensure(
                !recipe.failure_branches.is_empty(),
                "recipe failure branches exist",
            )?;
            for branch in recipe.failure_branches {
                ensure(!branch.condition.is_empty(), "failure condition non-empty")?;
                ensure(!branch.jq.is_empty(), "failure jq non-empty")?;
                ensure(
                    !branch.next_action.is_empty(),
                    "failure next action non-empty",
                )?;
            }
        }
        Ok(())
    }

    #[test]
    fn typed_memory_search_flags_are_documented_for_agents() -> TestResult {
        let typed_example = EXAMPLES
            .iter()
            .find(|example| example.title == "Search typed memory fields")
            .ok_or_else(|| "typed memory search example is documented".to_string())?;
        ensure(
            typed_example.command.contains("--kind failure"),
            "typed search example documents --kind",
        )?;
        ensure(
            typed_example.command.contains("--field family="),
            "typed search example documents --field name=value",
        )?;

        let recipe = AGENT_DOC_RECIPES
            .iter()
            .find(|recipe| recipe.id == "typed-memory-search")
            .ok_or_else(|| "typed memory search recipe is documented".to_string())?;
        ensure(
            recipe.description.contains("explicitly assigned")
                && recipe.description.contains("body-extracted"),
            "typed search recipe documents both typed-field capture paths",
        )?;
        ensure(
            recipe.command.contains("--kind failure") && recipe.command.contains("--field family="),
            "typed search recipe command includes kind and field filters",
        )?;
        ensure(
            recipe.jq.contains("typedFields"),
            "typed search recipe extracts typed field output",
        )?;

        let capture = EXAMPLES
            .iter()
            .find(|example| example.title == "Store typed failure evidence")
            .ok_or_else(|| "typed memory capture example is documented".to_string())?;
        ensure(
            capture.command.contains("--field family=")
                && capture.command.contains("--field reverted-at-sha="),
            "typed capture example documents repeatable write assignments",
        )
    }

    #[test]
    fn typed_fields_and_decide_docs_are_registered_for_agents() -> TestResult {
        let typed_contract = CONTRACTS
            .iter()
            .find(|contract| contract.name == "typed_memory_fields")
            .ok_or_else(|| "typed memory fields contract is documented".to_string())?;
        ensure_equal(
            &typed_contract.schema,
            &crate::models::memory::TYPED_MEMORY_FIELDS_SCHEMA_V2,
            "typed memory fields schema",
        )?;
        ensure(
            typed_contract.description.contains("v1 sidecars"),
            "typed contract documents v1 compatibility",
        )?;

        for (name, schema) in [
            ("decide_record", DECIDE_RECORD_SCHEMA_V1),
            ("decide_list", DECIDE_LIST_SCHEMA_V1),
            ("decide_revisit", DECIDE_REVISIT_SCHEMA_V1),
        ] {
            let contract = CONTRACTS
                .iter()
                .find(|contract| contract.name == name)
                .ok_or_else(|| format!("{name} contract is documented"))?;
            ensure_equal(&contract.schema, &schema, "decide contract schema")?;
        }

        let record_example = EXAMPLES
            .iter()
            .find(|example| example.title == "Record a decision")
            .ok_or_else(|| "decide record example is documented".to_string())?;
        ensure(
            record_example.command.starts_with("ee decide record ")
                && record_example.command.contains("--revisit-by +90d"),
            "decide record example uses the public CLI surface",
        )?;

        let recipe = AGENT_DOC_RECIPES
            .iter()
            .find(|recipe| recipe.id == "decide-before-rearchitecture")
            .ok_or_else(|| "decide recipe is documented".to_string())?;
        ensure(
            recipe.command.starts_with("ee decide list --about "),
            "decide recipe checks the decision log",
        )?;
        ensure(
            recipe.success_check.contains(DECIDE_LIST_SCHEMA_V1),
            "decide recipe checks list schema",
        )?;
        ensure(
            recipe
                .failure_branches
                .iter()
                .any(|branch| branch.jq.contains("decision_topic_requires_supersedes")),
            "decide recipe documents fork refusal recovery",
        )
    }

    #[test]
    fn distribution_recipes_document_current_cli_surfaces() -> TestResult {
        let find_recipe = |id: &str| {
            AGENT_DOC_RECIPES
                .iter()
                .find(|recipe| recipe.id == id)
                .ok_or_else(|| format!("recipe {id} exists"))
        };

        ensure_equal(
            &find_recipe("install-check")?.command,
            &"ee install check --json",
            "install check recipe command",
        )?;
        ensure_equal(
            &find_recipe("update-dry-run")?.command,
            &"ee update --dry-run --json",
            "update dry-run recipe command",
        )?;
        ensure_equal(
            &find_recipe("duplicate-binary-fix")?.command,
            &"ee install check --json",
            "duplicate binary recipe command",
        )?;
        ensure_equal(
            &find_recipe("offline-update-posture")?.command,
            &"ee update --dry-run --offline --json",
            "offline update recipe command",
        )?;
        ensure_equal(
            &find_recipe("update-failure-bundle")?.command,
            &"ee support bundle --dry-run --json",
            "support bundle recipe command",
        )?;

        let mut rendered_parts = Vec::new();
        for recipe in AGENT_DOC_RECIPES {
            rendered_parts.push(format!(
                "{}\n{}\n{}\n{}",
                recipe.command, recipe.jq, recipe.success_check, recipe.description
            ));
            for branch in recipe.failure_branches {
                rendered_parts.push(format!(
                    "{}\n{}\n{}",
                    branch.condition, branch.jq, branch.next_action
                ));
            }
        }
        let rendered = rendered_parts.join("\n");
        for obsolete in [
            "install diagnose",
            "support-bundle",
            "update --offline --check",
            "config unset version-pin",
            "checksumValid",
            ".data.current,",
            ".data.target,",
            ".data.postUpdate",
            ".data.duplicates",
        ] {
            ensure(
                !rendered.contains(obsolete),
                format!("agent docs recipes must not advertise obsolete surface `{obsolete}`"),
            )?;
        }
        Ok(())
    }
}
