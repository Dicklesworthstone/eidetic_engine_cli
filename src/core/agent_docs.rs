use crate::models::{ERROR_SCHEMA_V1, RESPONSE_SCHEMA_V1};

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
        match s {
            "guide" => Some(Self::Guide),
            "commands" => Some(Self::Commands),
            "contracts" => Some(Self::Contracts),
            "schemas" => Some(Self::Schemas),
            "paths" => Some(Self::Paths),
            "env" => Some(Self::Env),
            "exit-codes" => Some(Self::ExitCodes),
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
        content: "ee context \"<task>\" --workspace . --max-tokens 4000 --json",
    },
    GuideSection {
        title: "Machine Output",
        content: "Always use --json, --robot, or --format=json for machine-parseable output. stdout is data, stderr is diagnostics. Check exit codes for error conditions.",
    },
    GuideSection {
        title: "Workspace",
        content: "ee operates on workspaces (directories). Use --workspace to specify the target, or it defaults to the current directory. The .ee/ folder contains the database and indexes.",
    },
    GuideSection {
        title: "Degradation",
        content: "ee degrades gracefully. If semantic search is unavailable, it falls back to lexical. If the database is missing, init creates it. Check ee status --json for capability state.",
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
        env_override: Some("EE_DATABASE"),
    },
    PathEntry {
        name: "index_dir",
        default: "<workspace>/.ee/index/",
        description: "Directory containing search indexes",
        env_override: Some("EE_INDEX_DIR"),
    },
    PathEntry {
        name: "config",
        default: "<workspace>/.ee/config.toml",
        description: "Workspace-specific configuration file",
        env_override: Some("EE_CONFIG"),
    },
    PathEntry {
        name: "global_config",
        default: "~/.config/ee/config.toml",
        description: "Global user configuration file",
        env_override: Some("EE_GLOBAL_CONFIG"),
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

pub const ENV_VARS: &[EnvVarEntry] = &[
    EnvVarEntry {
        name: "EE_WORKSPACE",
        description: "Default workspace path when --workspace is not specified",
        default: Some("."),
        category: "paths",
    },
    EnvVarEntry {
        name: "EE_DATABASE",
        description: "Override default database path",
        default: None,
        category: "paths",
    },
    EnvVarEntry {
        name: "EE_INDEX_DIR",
        description: "Override default index directory",
        default: None,
        category: "paths",
    },
    EnvVarEntry {
        name: "EE_CONFIG",
        description: "Override workspace config file path",
        default: None,
        category: "paths",
    },
    EnvVarEntry {
        name: "EE_GLOBAL_CONFIG",
        description: "Override global config file path",
        default: None,
        category: "paths",
    },
    EnvVarEntry {
        name: "EE_LOG_LEVEL",
        description: "Logging verbosity (error, warn, info, debug, trace)",
        default: Some("warn"),
        category: "diagnostics",
    },
    EnvVarEntry {
        name: "EE_NO_COLOR",
        description: "Disable colored output (also respects NO_COLOR)",
        default: Some("false"),
        category: "output",
    },
    EnvVarEntry {
        name: "NO_COLOR",
        description: "Standard environment variable to disable colors",
        default: None,
        category: "output",
    },
    EnvVarEntry {
        name: "EE_JSON_PRETTY",
        description: "Emit pretty-printed JSON (for debugging only)",
        default: Some("false"),
        category: "output",
    },
];

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
        description: "Structured JSON with ee.response.v1 envelope",
        machine_readable: true,
    },
    FormatEntry {
        name: "toon",
        flag: "--format toon",
        description: "Compact hierarchical key-value notation",
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
        schema: RESPONSE_SCHEMA_V1,
        description: "Standard success response envelope with data payload",
        stability: "stable",
    },
    ContractEntry {
        name: "error",
        schema: ERROR_SCHEMA_V1,
        description: "Standard error response with code, message, and repair hint",
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
        command: "ee context \"fix failing CI tests\" --workspace . --max-tokens 4000 --json",
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
        title: "Fix plan",
        description: "Get actionable repair steps for issues",
        command: "ee doctor --fix-plan --json",
        category: "diagnostics",
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

pub const STATUS_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "database migration is required",
        jq: r#".. | objects | select(.code? == "migration_required")"#,
        next_action: "Run `ee db migrate --workspace . --json` before mutating memory state.",
    },
    FailureBranchEntry {
        condition: "storage or index capability is unavailable",
        jq: r#".data.degraded[]? | select(.code | test("storage|index"))"#,
        next_action: "Use the reported repair field or run `ee doctor --json` for a full repair plan.",
    },
];

pub const DOCTOR_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "one or more checks failed",
        jq: r#".data.checks[]? | select(.status != "ok") | {name, status, code, repair}"#,
        next_action: "Apply failing check repairs in order and rerun `ee doctor --json`.",
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
        jq: r#".data.contracts[]? | select(.schema == "ee.response.v1")"#,
        next_action: "Pin automation to the published schema list and stop if the expected schema is missing.",
    },
    FailureBranchEntry {
        condition: "agent-docs topic is misspelled",
        jq: r#".error | select(.code == "usage") | {message, repair}"#,
        next_action: "Run `ee agent-docs --json` and select a topic from `.data.topics[].name`.",
    },
];

// ============================================================================
// EE-DIST-005: Install/Update Recipe Failure Branches
// ============================================================================

pub const INSTALL_CHECK_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "binary checksum does not match expected",
        jq: r#".data.checks[]? | select(.code == "checksum_mismatch") | {expected: .expected, actual: .actual, repair}"#,
        next_action: "Re-download the binary from the official release URL and verify the checksum before replacing.",
    },
    FailureBranchEntry {
        condition: "binary not found at expected path",
        jq: r#".data.checks[]? | select(.code == "binary_not_found") | {path, repair}"#,
        next_action: "Run `ee install --json` to install the binary or update PATH to include the install directory.",
    },
    FailureBranchEntry {
        condition: "multiple ee binaries found in PATH",
        jq: r#".data.checks[]? | select(.code == "duplicate_binary") | {paths, primary, repair}"#,
        next_action: "Remove or rename duplicate binaries, keeping only the primary installation.",
    },
    FailureBranchEntry {
        condition: "binary version is outdated",
        jq: r#".data.checks[]? | select(.code == "version_stale") | {current: .current, latest: .latest, repair}"#,
        next_action: "Run `ee update --json` to upgrade to the latest version.",
    },
];

pub const UPDATE_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "network unavailable for update check",
        jq: r#".data.degraded[]? | select(.code == "network_unavailable") | {message, offlineAction: .repair}"#,
        next_action: "Defer update until network is available or use `ee update --offline` to apply a cached update.",
    },
    FailureBranchEntry {
        condition: "update download failed",
        jq: r#".error | select(.code == "download_failed") | {url, reason: .message, repair}"#,
        next_action: "Retry the download or manually fetch from the release URL and run `ee update --from-file`.",
    },
    FailureBranchEntry {
        condition: "update would break pinned version",
        jq: r#".data.checks[]? | select(.code == "pinned_version") | {pinnedVersion: .pinned, targetVersion: .target, repair}"#,
        next_action: "Remove the version pin with `ee config unset version-pin` or use `--force` to override.",
    },
    FailureBranchEntry {
        condition: "post-update migration required",
        jq: r#".data.postUpdate[]? | select(.action == "migrate") | {command, reason}"#,
        next_action: "Run the listed migration command before using new features.",
    },
];

pub const PIN_VERSION_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "specified version does not exist",
        jq: r#".error | select(.code == "version_not_found") | {requestedVersion: .details.version, available: .details.availableVersions}"#,
        next_action: "Choose a version from the available list or use `latest` for the most recent stable release.",
    },
    FailureBranchEntry {
        condition: "version pin already set",
        jq: r#".data | select(.existingPin) | {existingPin, requestedPin: .newPin}"#,
        next_action: "Use `--force` to override the existing pin or run `ee config unset version-pin` first.",
    },
];

pub const SUPPORT_BUNDLE_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "bundle creation failed due to permissions",
        jq: r#".error | select(.code == "permission_denied") | {path, reason: .message}"#,
        next_action: "Ensure write permissions for the output directory or specify an alternate path with `--output`.",
    },
    FailureBranchEntry {
        condition: "bundle exceeds size limit",
        jq: r#".data | select(.truncated) | {actualSize: .sizeBytes, limit: .limitBytes, excludedPaths: .excluded}"#,
        next_action: "Use `--max-size` to increase the limit or `--exclude` to remove large artifacts.",
    },
];

pub const AGENT_DOC_RECIPES: &[AgentDocsRecipeEntry] = &[
    AgentDocsRecipeEntry {
        id: "pre-task-context",
        title: "Fetch task context before editing",
        description: "Retrieve a compact, provenance-bearing context pack for the current task.",
        category: "context",
        command: "ee context \"<task>\" --workspace . --max-tokens 4000 --json",
        jq: r#".data.pack.items[]? | {memoryId, section, why}"#,
        success_check: r#".schema == "ee.response.v1" and .success == true"#,
        failure_branches: CONTEXT_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "workspace-health",
        title: "Check workspace health",
        description: "Inspect storage, index, and degraded capability state before relying on memory output.",
        category: "diagnostics",
        command: "ee status --workspace . --json",
        jq: r#"{database: .data.database, index: .data.index, degraded: (.data.degraded // [])}"#,
        success_check: r#".schema == "ee.response.v1" and .success == true"#,
        failure_branches: STATUS_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "repair-plan",
        title: "Collect repair actions",
        description: "Use doctor output as the stable source of repair commands for automation.",
        category: "diagnostics",
        command: "ee doctor --json",
        jq: r#".data.checks[]? | select(.status != "ok") | {name, code, repair}"#,
        success_check: r#".schema == "ee.response.v1" and .success == true"#,
        failure_branches: DOCTOR_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "goal-to-recipe",
        title: "Resolve an agent goal to commands",
        description: "Map a natural-language goal to a deterministic recipe before running a workflow.",
        category: "planning",
        command: "ee plan goal \"<goal>\" --json",
        jq: r#"{recipeId: .data.recipeId, steps: [.data.steps[]?.command], degraded: (.data.degradedBranches // [])}"#,
        success_check: r#".schema == "ee.response.v1" and .success == true"#,
        failure_branches: PLAN_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "contract-discovery",
        title: "Discover machine contracts",
        description: "List stable response contracts before writing or updating agent parsers.",
        category: "contracts",
        command: "ee agent-docs contracts --json",
        jq: r#".data.contracts[] | {name, schema, stability}"#,
        success_check: r#".schema == "ee.response.v1" and .success == true"#,
        failure_branches: CONTRACT_RECIPE_FAILURES,
    },
    // EE-DIST-005: Install/Update/Recovery Recipes
    AgentDocsRecipeEntry {
        id: "install-check",
        title: "Verify ee installation integrity",
        description: "Check binary presence, checksum, version currency, and PATH conflicts before relying on ee.",
        category: "distribution",
        command: "ee install check --json",
        jq: r#"{binaryPath: .data.binaryPath, version: .data.version, checksum: .data.checksumValid, pathConflicts: (.data.duplicates // [])}"#,
        success_check: r#".schema == "ee.response.v1" and .success == true and .data.checksumValid == true"#,
        failure_branches: INSTALL_CHECK_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "update-dry-run",
        title: "Preview update before applying",
        description: "Show what an update would change without modifying the installed binary.",
        category: "distribution",
        command: "ee update --dry-run --json",
        jq: r#"{currentVersion: .data.current, targetVersion: .data.target, changes: .data.changelog, postUpdateActions: (.data.postUpdate // [])}"#,
        success_check: r#".schema == "ee.response.v1" and .success == true"#,
        failure_branches: UPDATE_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "version-pin",
        title: "Pin ee to a specific version",
        description: "Lock the installation to a known version to prevent automatic updates.",
        category: "distribution",
        command: "ee config set version-pin <version> --json",
        jq: r#"{pinnedVersion: .data.version, pinnedAt: .data.pinnedAt, expiresAt: .data.expiresAt}"#,
        success_check: r#".schema == "ee.response.v1" and .success == true"#,
        failure_branches: PIN_VERSION_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "checksum-recovery",
        title: "Recover from checksum mismatch",
        description: "Re-verify and reinstall ee binary when checksum validation fails.",
        category: "distribution",
        command: "ee install --force --verify-checksum --json",
        jq: r#"{reinstalled: .data.installed, newChecksum: .data.checksum, previousChecksum: .data.previousChecksum}"#,
        success_check: r#".schema == "ee.response.v1" and .success == true and .data.checksumValid == true"#,
        failure_branches: INSTALL_CHECK_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "duplicate-binary-fix",
        title: "Resolve duplicate ee binaries in PATH",
        description: "Identify and remove conflicting ee installations when multiple binaries are found.",
        category: "distribution",
        command: "ee install diagnose --json",
        jq: r#"{primaryPath: .data.primary, duplicates: [.data.duplicates[]? | {path, version, recommendation}], repairCommands: .data.repairCommands}"#,
        success_check: r#".schema == "ee.response.v1" and .success == true"#,
        failure_branches: INSTALL_CHECK_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "offline-update-posture",
        title: "Check offline update readiness",
        description: "Verify cached update availability when network is unavailable.",
        category: "distribution",
        command: "ee update --offline --check --json",
        jq: r#"{offlineReady: .data.cachedUpdateAvailable, cachedVersion: .data.cachedVersion, cacheAge: .data.cacheAgeHours, degraded: (.data.degraded // [])}"#,
        success_check: r#".schema == "ee.response.v1" and .success == true"#,
        failure_branches: UPDATE_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "update-failure-bundle",
        title: "Collect support bundle for failed update",
        description: "Gather diagnostic evidence when an install or update fails for support handoff.",
        category: "distribution",
        command: "ee support-bundle --scope update --json",
        jq: r#"{bundlePath: .data.path, sizeBytes: .data.sizeBytes, includes: .data.artifacts, binaryProvenance: .data.provenance}"#,
        success_check: r#".schema == "ee.response.v1" and .success == true"#,
        failure_branches: SUPPORT_BUNDLE_RECIPE_FAILURES,
    },
];

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use super::{
        AGENT_DOC_RECIPES, AgentDocsTopic, CONTRACTS, DEFAULT_PATHS, ENV_VARS, EXAMPLES,
        EXIT_CODES, FIELD_LEVELS, GUIDE_SECTIONS, OUTPUT_FORMATS,
    };

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
        ensure(!ENV_VARS.is_empty(), "env vars exist")?;
        for var in ENV_VARS {
            ensure(!var.name.is_empty(), "env var name non-empty")?;
            ensure(!var.description.is_empty(), "env var description non-empty")?;
        }
        Ok(())
    }

    #[test]
    fn exit_codes_are_sequential() -> TestResult {
        for (i, code) in EXIT_CODES.iter().enumerate() {
            ensure_equal(
                &(code.code as usize),
                &i,
                &format!("exit code {} sequential", i),
            )?;
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
    fn contracts_have_stable_schemas() -> TestResult {
        ensure(!CONTRACTS.is_empty(), "contracts exist")?;
        for contract in CONTRACTS {
            ensure_equal(&contract.stability, &"stable", "contract stability")?;
        }
        Ok(())
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
}
