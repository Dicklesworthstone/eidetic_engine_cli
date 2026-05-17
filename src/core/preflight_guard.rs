//! Preflight evidence-matched command guard (eidetic_engine_cli-5arc).
//!
//! `ee preflight <command-string>` walks a per-workspace + bundled-default rule
//! registry and emits structured warnings citing the rule that matched. A
//! workspace-side HMAC bypass token (BLAKE3 keyed-hash) can suppress a single
//! match for a single command. Unbypassed matches halt the caller with exit
//! code 7 (PolicyDenied per `AGENTS.md`).
//!
//! This module intentionally has no dependency on the `core::preflight`
//! per-task risk-brief surface; it operates on raw command strings and reuses
//! the deterministic glob matcher shipped by `core::tripwire`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use toml_edit::{DocumentMut, Item};

use crate::core::degraded_aggregation::{DegradationAggregationInput, aggregate_degraded_entries};
use crate::core::tripwire::glob_match;
use crate::db::StoredMemory;
use crate::models::DomainError;

/// Stable schema string for the JSON payload returned by `ee preflight <cmd>`.
pub const PREFLIGHT_GUARD_SCHEMA_V1: &str = "ee.preflight.guard.v1";
pub const NO_RISK_MEMORIES_CODE: &str = "no_risk_memories";
pub const PREFLIGHT_PATTERNS_UNAVAILABLE_CODE: &str = "preflight_patterns_unavailable";

/// Default location for workspace-side rules, relative to the workspace root.
pub const PREFLIGHT_RULES_RELATIVE_PATH: &str = ".ee/preflight_rules.toml";

const TRAUMA_GUARD_PREFLIGHT_SURFACE: &str = "trauma_guard_preflight";

fn elapsed_ms_since(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn preflight_trace_workspace_id(workspace: &Path) -> String {
    let path = workspace.to_string_lossy();
    let digest = blake3::hash(path.as_bytes()).to_hex().to_string();
    format!("wsp_{}", &digest[..16])
}

fn trace_trauma_guard_preflight(
    workspace: &Path,
    phase: &'static str,
    elapsed_ms: u64,
    degraded_codes: &[&str],
) {
    tracing::info!(
        workspace_id = %preflight_trace_workspace_id(workspace),
        request_id = "preflight_guard_request",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-3usjw.6"),
        surface = TRAUMA_GUARD_PREFLIGHT_SURFACE,
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        "trauma guard preflight checkpoint"
    );
}

/// Action the guard takes when a rule matches.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardAction {
    /// Emit a structured warning but allow execution.
    Warn,
    /// Halt with policy-denied exit code unless an authoritative bypass is supplied.
    Halt,
}

impl GuardAction {
    /// Stable lowercase string used in JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Halt => "halt",
        }
    }

    /// Whether this action stops execution by default.
    #[must_use]
    pub const fn stops_execution(self) -> bool {
        matches!(self, Self::Halt)
    }
}

/// Where a guard rule came from. Surfaces in the JSON citation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleSource {
    /// Compiled-in default sourced from `AGENTS.md` invariants.
    Builtin { name: String },
    /// Workspace-side TOML file.
    WorkspaceFile { path: String },
    /// Linked procedural rule (id from `procedural_rules` table).
    ProceduralRule { rule_id: String },
    /// Linked tripwire (id from `tripwires` table).
    Tripwire { tripwire_id: String },
}

impl RuleSource {
    /// Stable kind string for filtering / grouping.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Builtin { .. } => "builtin",
            Self::WorkspaceFile { .. } => "workspace_file",
            Self::ProceduralRule { .. } => "procedural_rule",
            Self::Tripwire { .. } => "tripwire",
        }
    }
}

/// One rule in the registry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreflightGuardRule {
    /// Stable identifier (e.g. `builtin:rm_rf_root`, `workspace:custom_1`).
    pub id: String,
    /// Glob pattern matched against the candidate command string.
    /// Anchored; supports `*`, `?`, and literal characters.
    pub pattern: String,
    /// What to do when the pattern matches.
    pub action: GuardAction,
    /// Human-readable explanation of why this rule exists.
    pub message: String,
    /// Optional citation linking back to the source of this rule.
    pub source: RuleSource,
}

/// Registry holding the merged builtin + workspace rules.
#[derive(Clone, Debug, Default)]
pub struct PreflightGuardRegistry {
    rules: Vec<PreflightGuardRule>,
}

impl PreflightGuardRegistry {
    /// Empty registry (used in tests; production callers should call [`load`]).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a registry containing only the bundled defaults.
    #[must_use]
    pub fn with_builtins() -> Self {
        Self {
            rules: builtin_rules(),
        }
    }

    /// Load builtins, then layer workspace-side rules from
    /// `<workspace>/.ee/preflight_rules.toml` if that file exists. A missing
    /// file is not an error; a malformed file is.
    pub fn load(workspace: &Path) -> Result<Self, DomainError> {
        let mut registry = Self::with_builtins();
        let rules_path = workspace.join(PREFLIGHT_RULES_RELATIVE_PATH);
        validate_preflight_rules_path(&rules_path)?;
        let source_label = rules_path.to_string_lossy().into_owned();
        let body = match fs::read_to_string(&rules_path) {
            Ok(body) => body,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(registry),
            Err(error) => {
                return Err(DomainError::Storage {
                    message: format!("Failed to read {source_label}: {error}"),
                    repair: Some(format!(
                        "Check filesystem permissions on {} or remove the file to fall back to builtins.",
                        source_label
                    )),
                });
            }
        };
        let workspace_rules = parse_workspace_rules(&body, &source_label)?;
        registry.rules.extend(workspace_rules);
        Ok(registry)
    }

    /// Parse a TOML document into a registry (no builtins layered in).
    pub fn from_toml(body: &str, source_label: &str) -> Result<Self, DomainError> {
        Ok(Self {
            rules: parse_workspace_rules(body, source_label)?,
        })
    }

    /// Borrow all rules in stable insertion order.
    #[must_use]
    pub fn rules(&self) -> &[PreflightGuardRule] {
        &self.rules
    }

    /// Replace the rule set; primarily used by tests and external loaders.
    pub fn set_rules(&mut self, rules: Vec<PreflightGuardRule>) {
        self.rules = rules;
    }

    /// Append rules linked from procedural-rule or tripwire records.
    /// Duplicate ids are skipped to keep matches deterministic.
    pub fn extend_from_links<I>(&mut self, linked: I)
    where
        I: IntoIterator<Item = PreflightGuardRule>,
    {
        for rule in linked {
            if !self.rules.iter().any(|existing| existing.id == rule.id) {
                self.rules.push(rule);
            }
        }
    }

    /// Find every rule whose pattern matches the candidate command string.
    /// Order matches the rule order in the registry, which is stable.
    #[must_use]
    pub fn match_command(&self, command: &str) -> Vec<&PreflightGuardRule> {
        self.rules
            .iter()
            .filter(|rule| rule_matches_command(rule, command))
            .collect()
    }
}

fn validate_preflight_rules_path(path: &Path) -> Result<(), DomainError> {
    if let Some(symlink_path) =
        first_existing_symlink_component(path).map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to inspect preflight rule path component {}: {}",
                error.path.display(),
                error.source
            ),
            repair: Some("Fix or remove .ee/preflight_rules.toml.".to_owned()),
        })?
    {
        return Err(DomainError::Configuration {
            message: format!(
                "Refusing to read preflight rule file {} through symlinked path component {}.",
                path.display(),
                symlink_path.display()
            ),
            repair: Some(
                "Replace .ee/preflight_rules.toml with a regular file inside the workspace."
                    .to_owned(),
            ),
        });
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(());
        }
        Err(error) => {
            return Err(DomainError::Storage {
                message: format!(
                    "Failed to inspect preflight rule file {}: {error}",
                    path.display()
                ),
                repair: Some("Fix or remove .ee/preflight_rules.toml.".to_owned()),
            });
        }
    };
    if !metadata.is_file() {
        return Err(DomainError::Configuration {
            message: format!(
                "Preflight rule path is not a regular file: {}",
                path.display()
            ),
            repair: Some("Replace .ee/preflight_rules.toml with a regular TOML file.".to_owned()),
        });
    }
    Ok(())
}

#[derive(Debug)]
struct SymlinkComponentInspectionError {
    path: PathBuf,
    source: std::io::Error,
}

fn first_existing_symlink_component(
    path: &Path,
) -> Result<Option<PathBuf>, SymlinkComponentInspectionError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(Some(current)),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(source) => {
                return Err(SymlinkComponentInspectionError {
                    path: current,
                    source,
                });
            }
        }
    }
    Ok(None)
}

fn rule_matches_command(rule: &PreflightGuardRule, command: &str) -> bool {
    if let RuleSource::Builtin { name } = &rule.source {
        match name.as_str() {
            "rm_rf_root" => return matches_rm_rf_target(command, RmTargetClass::Absolute),
            "rm_rf_home" => return matches_rm_rf_target(command, RmTargetClass::Home),
            "kubectl_mass_delete" => return matches_kubectl_mass_delete(command),
            "drop_table_sql" => return matches_drop_table_sql(command),
            "terraform_destroy" => return matches_terraform_destroy(command),
            "raw_block_device_write" => return matches_raw_block_device_write(command),
            "filesystem_create" => return matches_filesystem_create(command),
            _ => {}
        }
    }
    glob_match(&rule.pattern, command)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RmTargetClass {
    Absolute,
    Home,
}

fn matches_rm_rf_target(command: &str, target_class: RmTargetClass) -> bool {
    shell_command_segments(command)
        .iter()
        .any(|segment| rm_segment_matches_target(segment, target_class))
}

fn rm_segment_matches_target(segment: &[String], target_class: RmTargetClass) -> bool {
    let Some(command_index) = shell_segment_command_index(segment) else {
        return false;
    };
    if segment.get(command_index).is_none_or(|word| word != "rm") {
        return false;
    }

    let mut has_recursive = false;
    let mut has_force = false;
    let mut saw_option_end = false;
    let mut targets = Vec::new();

    for word in segment.iter().skip(command_index + 1) {
        if !saw_option_end && word == "--" {
            saw_option_end = true;
            continue;
        }
        if !saw_option_end && word.starts_with('-') && word != "-" {
            if rm_option_has_recursive(word) {
                has_recursive = true;
            }
            if rm_option_has_force(word) {
                has_force = true;
            }
            continue;
        }
        targets.push(word.as_str());
    }

    has_recursive
        && has_force
        && targets
            .iter()
            .any(|target| rm_target_matches_class(target, target_class))
}

fn shell_segment_command_index(segment: &[String]) -> Option<usize> {
    let mut index = 0;
    while index < segment.len() {
        let word = &segment[index];
        if word == "sudo" {
            index += 1;
            while segment
                .get(index)
                .is_some_and(|candidate| candidate.starts_with('-'))
            {
                index += 1;
            }
            continue;
        }
        if word == "command" || word == "builtin" {
            index += 1;
            continue;
        }
        if word == "env" {
            index += 1;
            while segment
                .get(index)
                .is_some_and(|candidate| looks_like_env_assignment(candidate))
            {
                index += 1;
            }
            continue;
        }
        if looks_like_env_assignment(word) {
            index += 1;
            continue;
        }
        return Some(index);
    }
    None
}

fn looks_like_env_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn rm_option_has_recursive(option: &str) -> bool {
    if option.starts_with("--") {
        option == "--recursive"
    } else {
        option
            .get(1..)
            .unwrap_or("")
            .chars()
            .any(|ch| matches!(ch, 'r' | 'R'))
    }
}

fn rm_option_has_force(option: &str) -> bool {
    if option.starts_with("--") {
        option == "--force"
    } else {
        option.get(1..).unwrap_or("").chars().any(|ch| ch == 'f')
    }
}

fn rm_target_matches_class(target: &str, target_class: RmTargetClass) -> bool {
    match target_class {
        RmTargetClass::Absolute => target.starts_with('/'),
        RmTargetClass::Home => target.starts_with('~'),
    }
}

fn matches_kubectl_mass_delete(command: &str) -> bool {
    shell_command_segments(command).iter().any(|segment| {
        let Some(command_index) = shell_segment_command_index(segment) else {
            return false;
        };
        if segment
            .get(command_index)
            .is_none_or(|word| word != "kubectl")
        {
            return false;
        }
        let args = &segment[command_index + 1..];
        args.iter().any(|arg| arg == "delete")
            && args.iter().any(|arg| arg == "--all")
            && args
                .iter()
                .any(|arg| arg == "--all-namespaces" || arg == "-A")
    })
}

fn matches_drop_table_sql(command: &str) -> bool {
    command.to_ascii_lowercase().contains("drop table")
}

fn matches_terraform_destroy(command: &str) -> bool {
    shell_command_segments(command).iter().any(|segment| {
        let Some(command_index) = shell_segment_command_index(segment) else {
            return false;
        };
        segment
            .get(command_index)
            .is_some_and(|word| word == "terraform")
            && segment
                .iter()
                .skip(command_index + 1)
                .any(|arg| arg == "destroy")
    })
}

fn matches_raw_block_device_write(command: &str) -> bool {
    shell_command_segments(command).iter().any(|segment| {
        let Some(command_index) = shell_segment_command_index(segment) else {
            return false;
        };
        segment.get(command_index).is_some_and(|word| word == "dd")
            && segment
                .iter()
                .skip(command_index + 1)
                .filter_map(|arg| arg.strip_prefix("of="))
                .any(is_block_device_path)
    })
}

fn matches_filesystem_create(command: &str) -> bool {
    shell_command_segments(command).iter().any(|segment| {
        let Some(command_index) = shell_segment_command_index(segment) else {
            return false;
        };
        let Some(command_name) = segment.get(command_index) else {
            return false;
        };
        let mkfs_command = command_name == "mkfs"
            || command_name.starts_with("mkfs.")
            || matches!(command_name.as_str(), "mke2fs" | "mkfs_ext4");
        mkfs_command
            && segment
                .iter()
                .skip(command_index + 1)
                .any(|arg| is_block_device_path(arg))
    })
}

fn is_block_device_path(path: &str) -> bool {
    path.starts_with("/dev/sd")
        || path.starts_with("/dev/xvd")
        || path.starts_with("/dev/vd")
        || path.starts_with("/dev/nvme")
        || path.starts_with("/dev/disk")
        || path.starts_with("/dev/rdisk")
}

fn shell_command_segments(command: &str) -> Vec<Vec<String>> {
    let mut segments = Vec::new();
    let mut current_segment = Vec::new();
    let mut current_word = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;

    for ch in command.chars() {
        if escaped {
            current_word.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(quote_ch) = quote {
            if ch == quote_ch {
                quote = None;
            } else {
                current_word.push(ch);
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            ';' | '|' | '&' => {
                finish_shell_word(&mut current_word, &mut current_segment);
                finish_shell_segment(&mut current_segment, &mut segments);
            }
            ch if ch.is_whitespace() => finish_shell_word(&mut current_word, &mut current_segment),
            _ => current_word.push(ch),
        }
    }

    if escaped {
        current_word.push('\\');
    }
    finish_shell_word(&mut current_word, &mut current_segment);
    finish_shell_segment(&mut current_segment, &mut segments);
    segments
}

fn finish_shell_word(current_word: &mut String, current_segment: &mut Vec<String>) {
    if !current_word.is_empty() {
        current_segment.push(std::mem::take(current_word));
    }
}

fn finish_shell_segment(current_segment: &mut Vec<String>, segments: &mut Vec<Vec<String>>) {
    if !current_segment.is_empty() {
        segments.push(std::mem::take(current_segment));
    }
}

fn parse_workspace_rules(
    body: &str,
    source_label: &str,
) -> Result<Vec<PreflightGuardRule>, DomainError> {
    let document = body
        .parse::<DocumentMut>()
        .map_err(|error| DomainError::Usage {
            message: format!("Failed to parse {source_label}: {error}"),
            repair: Some(format!(
                "Fix the TOML syntax in {source_label} or delete the file."
            )),
        })?;

    let Some(rules_item) = document.get("rules") else {
        return Ok(Vec::new());
    };

    let array = rules_item
        .as_array_of_tables()
        .ok_or_else(|| DomainError::Usage {
            message: format!(
                "{source_label}: expected `[[rules]]` array of tables, got {kind}",
                kind = describe_item(rules_item)
            ),
            repair: Some(
                "Use TOML array-of-tables syntax: each rule starts with `[[rules]]`.".to_owned(),
            ),
        })?;

    let mut rules = Vec::with_capacity(array.len());
    for (index, table) in array.iter().enumerate() {
        let id = table
            .get("id")
            .and_then(Item::as_str)
            .ok_or_else(|| DomainError::Usage {
                message: format!("{source_label}: rule[{index}] missing string `id`"),
                repair: Some("Add an `id = \"...\"` field to each [[rules]] entry.".to_owned()),
            })?;
        let pattern =
            table
                .get("pattern")
                .and_then(Item::as_str)
                .ok_or_else(|| DomainError::Usage {
                    message: format!("{source_label}: rule[{index}] missing string `pattern`"),
                    repair: Some(
                        "Add a `pattern = \"...\"` glob field to each [[rules]] entry.".to_owned(),
                    ),
                })?;
        let action_str = table.get("action").and_then(Item::as_str).unwrap_or("warn");
        let action = match action_str {
            "warn" => GuardAction::Warn,
            "halt" => GuardAction::Halt,
            other => {
                return Err(DomainError::Usage {
                    message: format!("{source_label}: rule[{index}] has invalid action `{other}`"),
                    repair: Some("Use `action = \"warn\"` or `action = \"halt\"`.".to_owned()),
                });
            }
        };
        let message = table
            .get("message")
            .and_then(Item::as_str)
            .unwrap_or(pattern)
            .to_owned();
        rules.push(PreflightGuardRule {
            id: id.to_owned(),
            pattern: pattern.to_owned(),
            action,
            message,
            source: RuleSource::WorkspaceFile {
                path: source_label.to_owned(),
            },
        });
    }
    Ok(rules)
}

fn describe_item(item: &Item) -> &'static str {
    if item.is_table() {
        "table"
    } else if item.is_value() {
        "value"
    } else if item.is_array_of_tables() {
        "array_of_tables"
    } else if item.is_none() {
        "none"
    } else {
        "other"
    }
}

/// Compiled-in defaults sourced from the `AGENTS.md` "Irreversible Git &
/// Filesystem Actions" + RULE 2 invariants. These match destructive command
/// surfaces that have caused real incidents in the past.
fn builtin_rules() -> Vec<PreflightGuardRule> {
    vec![
        PreflightGuardRule {
            id: "builtin:rm_rf_root".to_owned(),
            pattern: "*rm -rf /*".to_owned(),
            action: GuardAction::Halt,
            message: "rm -rf targeting filesystem root is forbidden by AGENTS.md (\"Irreversible Git & Filesystem Actions\").".to_owned(),
            source: RuleSource::Builtin { name: "rm_rf_root".to_owned() },
        },
        PreflightGuardRule {
            id: "builtin:rm_rf_home".to_owned(),
            pattern: "*rm -rf ~*".to_owned(),
            action: GuardAction::Halt,
            message: "rm -rf targeting $HOME is forbidden by AGENTS.md.".to_owned(),
            source: RuleSource::Builtin { name: "rm_rf_home".to_owned() },
        },
        PreflightGuardRule {
            id: "builtin:git_reset_hard".to_owned(),
            pattern: "*git reset --hard*".to_owned(),
            action: GuardAction::Halt,
            message: "git reset --hard is on the AGENTS.md absolutely-forbidden list. Use git stash, git diff, or a backup branch instead.".to_owned(),
            source: RuleSource::Builtin { name: "git_reset_hard".to_owned() },
        },
        PreflightGuardRule {
            id: "builtin:git_clean_fd".to_owned(),
            pattern: "*git clean -fd*".to_owned(),
            action: GuardAction::Halt,
            message: "git clean -fd will delete other agents' uncommitted work and is forbidden by AGENTS.md.".to_owned(),
            source: RuleSource::Builtin { name: "git_clean_fd".to_owned() },
        },
        PreflightGuardRule {
            id: "builtin:git_worktree_add".to_owned(),
            pattern: "*git worktree add*".to_owned(),
            action: GuardAction::Halt,
            message: "git worktree add is forbidden by AGENTS.md RULE 2 (\"NO WORKTREES. EVER.\").".to_owned(),
            source: RuleSource::Builtin { name: "git_worktree_add".to_owned() },
        },
        PreflightGuardRule {
            id: "builtin:git_push_force".to_owned(),
            pattern: "*git push*--force*".to_owned(),
            action: GuardAction::Warn,
            message: "git push --force overwrites upstream history; ensure you have explicit user authorization (AGENTS.md \"Executing actions with care\").".to_owned(),
            source: RuleSource::Builtin { name: "git_push_force".to_owned() },
        },
        PreflightGuardRule {
            id: "builtin:kubectl_mass_delete".to_owned(),
            pattern: "*kubectl delete*--all*".to_owned(),
            action: GuardAction::Halt,
            message: "kubectl mass deletion across namespaces can remove live workloads; require explicit approval before proceeding.".to_owned(),
            source: RuleSource::Builtin { name: "kubectl_mass_delete".to_owned() },
        },
        PreflightGuardRule {
            id: "builtin:drop_table_sql".to_owned(),
            pattern: "*DROP TABLE*".to_owned(),
            action: GuardAction::Halt,
            message: "DROP TABLE is destructive database DDL; require explicit approval and backup evidence before proceeding.".to_owned(),
            source: RuleSource::Builtin { name: "drop_table_sql".to_owned() },
        },
        PreflightGuardRule {
            id: "builtin:terraform_destroy".to_owned(),
            pattern: "*terraform destroy*".to_owned(),
            action: GuardAction::Halt,
            message: "terraform destroy tears down infrastructure and requires explicit approval before proceeding.".to_owned(),
            source: RuleSource::Builtin { name: "terraform_destroy".to_owned() },
        },
        PreflightGuardRule {
            id: "builtin:raw_block_device_write".to_owned(),
            pattern: "*dd *of=/dev/*".to_owned(),
            action: GuardAction::Halt,
            message: "Writing raw bytes to a block device is destructive and requires explicit approval before proceeding.".to_owned(),
            source: RuleSource::Builtin { name: "raw_block_device_write".to_owned() },
        },
        PreflightGuardRule {
            id: "builtin:filesystem_create".to_owned(),
            pattern: "*mkfs* /dev/*".to_owned(),
            action: GuardAction::Halt,
            message: "Creating a filesystem on a block device destroys existing data and requires explicit approval before proceeding.".to_owned(),
            source: RuleSource::Builtin { name: "filesystem_create".to_owned() },
        },
    ]
}

/// Inputs for [`run_preflight_guard`].
#[derive(Clone, Debug)]
pub struct PreflightGuardOptions {
    /// Candidate command string (raw, as the agent would invoke).
    pub command: String,
    /// Workspace path used to locate `.ee/preflight_rules.toml`.
    pub workspace: PathBuf,
    /// Optional one-shot HMAC bypass token (one bypass per token; one token
    /// per `(rule_id, command)` pair).
    pub bypass_tokens: Vec<BypassTokenInput>,
    /// Bypass HMAC secret. When `None`, no token can pass verification.
    pub bypass_secret: Option<Vec<u8>>,
}

/// One caller-provided bypass attempt: token + the rule the caller claims it
/// covers. We require an explicit rule_id so each attempt audits cleanly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BypassTokenInput {
    pub rule_id: String,
    pub token: String,
}

/// One match the guard found, including how it was resolved.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GuardMatch {
    pub rule_id: String,
    pub pattern: String,
    pub action: GuardAction,
    pub message: String,
    pub source: RuleSource,
    /// `bypassed_with_token` if the caller produced a valid token for this
    /// rule+command, `bypass_token_invalid` if a token was supplied but
    /// failed verification, otherwise `enforced`.
    pub resolution: MatchResolution,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PreflightMemoryMatch {
    pub memory_id: String,
    pub kind: String,
    pub content: String,
    pub provenance_uri: Option<String>,
    pub severity: &'static str,
    pub severity_source: &'static str,
    pub score: f64,
    pub matched_terms: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PreflightGuardDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: String,
}

/// Outcome for one rule that matched.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchResolution {
    Enforced,
    BypassedWithToken,
    BypassTokenInvalid,
    BypassSecretMissing,
}

impl MatchResolution {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enforced => "enforced",
            Self::BypassedWithToken => "bypassed_with_token",
            Self::BypassTokenInvalid => "bypass_token_invalid",
            Self::BypassSecretMissing => "bypass_secret_missing",
        }
    }
}

/// Final report from a guard run.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PreflightGuardReport {
    pub schema: String,
    pub command: String,
    pub matches: Vec<GuardMatch>,
    pub matched_memories: Vec<PreflightMemoryMatch>,
    pub degraded: Vec<PreflightGuardDegradation>,
    /// Process exit code: 0 if no enforced match, 7 (PolicyDenied per AGENTS.md
    /// exit-code table) if any match remained enforced after bypass attempts.
    pub exit_code: u32,
    pub checked_at: String,
}

impl PreflightGuardReport {
    /// JSON payload using the stable schema string.
    #[must_use]
    pub fn to_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "command": self.command,
            "exitCode": self.exit_code,
            "checkedAt": self.checked_at,
            "matches": self.matches.iter().map(|m| json!({
                "ruleId": m.rule_id,
                "pattern": m.pattern,
                "action": m.action.as_str(),
                "message": m.message,
                "source": m.source,
                "resolution": m.resolution.as_str(),
            })).collect::<Vec<_>>(),
            "matchedMemories": self.matched_memories,
            "degraded": preflight_guard_degraded_json(&self.degraded),
        })
    }

    /// Human summary suitable for `--no-json`.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(256);
        if self.exit_code == 0 {
            out.push_str("preflight: command passed all guard rules.\n");
        } else {
            out.push_str("preflight: command halted by guard rules (exit 7).\n");
        }
        out.push_str(&format!("  command: {}\n", self.command));
        for m in &self.matches {
            out.push_str(&format!(
                "  - [{action} | {resolution}] {id}: {message}\n",
                action = m.action.as_str(),
                resolution = m.resolution.as_str(),
                id = m.rule_id,
                message = m.message,
            ));
        }
        if !self.matched_memories.is_empty() {
            out.push_str("\nMatched memories:\n");
            for memory in &self.matched_memories {
                out.push_str(&format!(
                    "  - [{} | {} | {:.4}] {}\n",
                    memory.kind, memory.severity, memory.score, memory.memory_id
                ));
            }
        }
        for degraded in &self.degraded {
            out.push_str(&format!(
                "\nDegraded: {} ({})\nNext: {}\n",
                degraded.message, degraded.code, degraded.repair
            ));
        }
        out
    }
}

/// Evaluate the guard for `options.command`, applying any caller-supplied
/// bypass tokens. Returns a stable report; the caller maps `exit_code` onto
/// the process exit value.
#[must_use]
pub fn run_preflight_guard(
    registry: &PreflightGuardRegistry,
    options: &PreflightGuardOptions,
) -> PreflightGuardReport {
    let started = Instant::now();
    trace_trauma_guard_preflight(&options.workspace, "input", 0, &[]);

    let checked_at = chrono::Utc::now().to_rfc3339();
    let matches = registry.match_command(&options.command);

    let mut report_matches = Vec::with_capacity(matches.len());
    let mut any_enforced_halt = false;
    for matched in matches {
        let resolution = resolve_match(matched, options);
        // A halt rule continues to halt unless the bypass actually succeeded.
        // An invalid token, missing secret, or no token at all all leave the
        // policy denial in force.
        if matched.action.stops_execution()
            && !matches!(resolution, MatchResolution::BypassedWithToken)
        {
            any_enforced_halt = true;
        }
        report_matches.push(GuardMatch {
            rule_id: matched.id.clone(),
            pattern: matched.pattern.clone(),
            action: matched.action,
            message: matched.message.clone(),
            source: matched.source.clone(),
            resolution,
        });
    }

    let report = PreflightGuardReport {
        schema: PREFLIGHT_GUARD_SCHEMA_V1.to_owned(),
        command: options.command.clone(),
        exit_code: if any_enforced_halt { 7 } else { 0 },
        checked_at,
        matches: report_matches,
        matched_memories: Vec::new(),
        degraded: Vec::new(),
    };
    let degraded_codes = report
        .degraded
        .iter()
        .map(|degraded| degraded.code)
        .collect::<Vec<_>>();
    trace_trauma_guard_preflight(
        &options.workspace,
        "response",
        elapsed_ms_since(started),
        &degraded_codes,
    );
    report
}

fn preflight_guard_degraded_json(degraded: &[PreflightGuardDegradation]) -> Vec<JsonValue> {
    aggregate_degraded_entries(degraded.iter().map(|entry| {
        DegradationAggregationInput::new(
            "preflight_guard",
            entry.code,
            entry.severity,
            entry.message.clone(),
            entry.repair.clone(),
        )
    }))
    .into_iter()
    .map(|entry| {
        json!({
            "code": entry.code,
            "severity": entry.severity,
            "message": entry.message,
            "repair": entry.repair,
            "sources": entry.sources,
        })
    })
    .collect()
}

pub fn match_trauma_guard_memories(
    command: &str,
    memories: &[StoredMemory],
) -> Vec<PreflightMemoryMatch> {
    let command_terms = trauma_guard_command_terms(command);
    if command_terms.is_empty() {
        return Vec::new();
    }
    let mut matches = memories
        .iter()
        .filter(|memory| trauma_guard_memory_kind(memory.kind.as_str()))
        .filter_map(|memory| trauma_guard_memory_match(memory, &command_terms))
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
    matches
}

#[must_use]
pub fn no_risk_memories_degradation() -> PreflightGuardDegradation {
    PreflightGuardDegradation {
        code: NO_RISK_MEMORIES_CODE,
        severity: "info",
        message: "Destructive command was recognized, but no matching risk, anti-pattern, or failure memories were available.".to_owned(),
        repair: "ee remember --kind risk --severity high <warning>".to_owned(),
    }
}

#[must_use]
pub fn preflight_patterns_unavailable_degradation(
    message: impl Into<String>,
) -> PreflightGuardDegradation {
    PreflightGuardDegradation {
        code: PREFLIGHT_PATTERNS_UNAVAILABLE_CODE,
        severity: "medium",
        message: message.into(),
        repair:
            "Check the workspace preflight rule file or fall back to built-in destructive patterns."
                .to_owned(),
    }
}

fn trauma_guard_memory_match(
    memory: &StoredMemory,
    command_terms: &std::collections::BTreeSet<String>,
) -> Option<PreflightMemoryMatch> {
    let memory_terms = trauma_guard_text_terms(&memory.content);
    let matched_terms = command_terms
        .intersection(&memory_terms)
        .cloned()
        .collect::<Vec<_>>();
    if matched_terms.is_empty() {
        return None;
    }
    let score = matched_terms.len() as f64 / command_terms.len() as f64;
    Some(PreflightMemoryMatch {
        memory_id: memory.id.clone(),
        kind: memory.kind.clone(),
        content: memory.content.clone(),
        provenance_uri: memory.provenance_uri.clone(),
        severity: inferred_trauma_guard_severity(memory.kind.as_str()),
        severity_source: "inferred_from_memory_kind",
        score,
        matched_terms,
    })
}

fn trauma_guard_memory_kind(kind: &str) -> bool {
    matches!(kind, "risk" | "anti-pattern" | "failure")
}

fn inferred_trauma_guard_severity(kind: &str) -> &'static str {
    match kind {
        "risk" | "anti-pattern" => "high",
        "failure" => "medium",
        _ => "info",
    }
}

fn trauma_guard_command_terms(command: &str) -> std::collections::BTreeSet<String> {
    let mut terms = trauma_guard_text_terms(command);
    let lower = command.to_ascii_lowercase();
    if lower.contains("rm") {
        terms.extend(
            ["delete", "remove", "recursive"]
                .into_iter()
                .map(str::to_owned),
        );
    }
    if lower.contains("git reset") {
        terms.extend(["reset", "hard"].into_iter().map(str::to_owned));
    }
    if lower.contains("git clean") {
        terms.extend(["clean", "delete"].into_iter().map(str::to_owned));
    }
    if lower.contains("push") && lower.contains("force") {
        terms.extend(["push", "force", "history"].into_iter().map(str::to_owned));
    }
    terms
}

fn trauma_guard_text_terms(text: &str) -> std::collections::BTreeSet<String> {
    text.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-')
        .map(str::trim)
        .filter(|term| term.len() >= 2)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn resolve_match(rule: &PreflightGuardRule, options: &PreflightGuardOptions) -> MatchResolution {
    let provided_token = options
        .bypass_tokens
        .iter()
        .find(|attempt| attempt.rule_id == rule.id);

    let Some(attempt) = provided_token else {
        return MatchResolution::Enforced;
    };

    let Some(secret) = options.bypass_secret.as_deref() else {
        return MatchResolution::BypassSecretMissing;
    };

    if verify_bypass_token(&attempt.token, &rule.id, &options.command, secret) {
        MatchResolution::BypassedWithToken
    } else {
        MatchResolution::BypassTokenInvalid
    }
}

// ============================================================================
// Bypass tokens (BLAKE3 keyed-hash MAC)
// ============================================================================

/// Schema constant included in token payloads to make tokens unambiguous.
const BYPASS_TOKEN_SCHEMA_TAG: &[u8] = b"ee.preflight.bypass.v1";

/// Issue a bypass token for `(rule_id, command)` using `secret` as the MAC key.
///
/// Tokens are domain-separated: a token issued for rule A cannot bypass rule B,
/// and a token issued for command X cannot bypass command Y. The output is
/// lowercase hex of a 32-byte BLAKE3 keyed hash (cryptographic MAC).
#[must_use]
pub fn issue_bypass_token(rule_id: &str, command: &str, secret: &[u8]) -> String {
    let key = derive_bypass_key(secret);
    let mut hasher = blake3::Hasher::new_keyed(&key);
    hasher.update(BYPASS_TOKEN_SCHEMA_TAG);
    hasher.update(b"\0");
    hasher.update(rule_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(command.as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// Verify `token` was issued for the given `(rule_id, command, secret)` triple.
/// Comparison is constant-time over equal-length inputs.
#[must_use]
pub fn verify_bypass_token(token: &str, rule_id: &str, command: &str, secret: &[u8]) -> bool {
    let expected = issue_bypass_token(rule_id, command, secret);
    constant_time_eq_str(&expected, token)
}

fn derive_bypass_key(secret: &[u8]) -> [u8; 32] {
    // blake3::derive_key gives us a 32-byte MAC key from any-length secret with
    // domain separation; we use a stable context string so a leaked workspace
    // secret can be rotated without invalidating other contexts.
    blake3::derive_key("ee preflight bypass v1", secret)
}

fn constant_time_eq_str(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    //! Inline tests duplicate cases from `tests/preflight_guard.rs`; the
    //! integration test file is the canonical exercise of the public API.
    //! These remain here so the unit-test suite still covers the module
    //! when other crates' broken `#[cfg(test)]` blocks aren't blocking
    //! the lib-test build.
    use super::*;

    fn registry_with_only(rules: Vec<PreflightGuardRule>) -> PreflightGuardRegistry {
        let mut registry = PreflightGuardRegistry::new();
        registry.set_rules(rules);
        registry
    }

    fn rule(id: &str, pattern: &str, action: GuardAction) -> PreflightGuardRule {
        PreflightGuardRule {
            id: id.to_owned(),
            pattern: pattern.to_owned(),
            action,
            message: format!("test rule {id}"),
            source: RuleSource::Builtin {
                name: id.to_owned(),
            },
        }
    }

    fn opts(command: &str) -> PreflightGuardOptions {
        PreflightGuardOptions {
            command: command.to_owned(),
            workspace: PathBuf::from("."),
            bypass_tokens: Vec::new(),
            bypass_secret: None,
        }
    }

    #[test]
    fn no_match_yields_exit_zero() {
        let registry = registry_with_only(vec![rule("r1", "*rm -rf /*", GuardAction::Halt)]);
        let report = run_preflight_guard(&registry, &opts("ls -la"));
        assert_eq!(report.exit_code, 0);
        assert!(report.matches.is_empty());
    }

    #[test]
    fn single_halt_match_exits_seven() {
        let registry = registry_with_only(vec![rule("r1", "*rm -rf /*", GuardAction::Halt)]);
        let report = run_preflight_guard(&registry, &opts("rm -rf /tmp/foo"));
        assert_eq!(report.exit_code, 7);
        assert_eq!(report.matches.len(), 1);
        assert_eq!(report.matches[0].rule_id, "r1");
        assert_eq!(report.matches[0].action, GuardAction::Halt);
        assert_eq!(report.matches[0].resolution, MatchResolution::Enforced);
    }

    #[test]
    fn multiple_rules_all_cited_and_halts_dominate() {
        let registry = registry_with_only(vec![
            rule("r1", "*rm -rf*", GuardAction::Halt),
            rule("r2", "*--no-verify*", GuardAction::Warn),
        ]);
        let report =
            run_preflight_guard(&registry, &opts("git commit --no-verify -m 'rm -rf old'"));
        let ids: Vec<_> = report.matches.iter().map(|m| m.rule_id.as_str()).collect();
        assert_eq!(ids, vec!["r1", "r2"]);
        assert_eq!(report.exit_code, 7); // halt overrides warn
    }

    #[test]
    fn warn_only_match_does_not_halt() {
        let registry = registry_with_only(vec![rule("warn1", "*--no-verify*", GuardAction::Warn)]);
        let report = run_preflight_guard(&registry, &opts("git commit --no-verify"));
        assert_eq!(report.exit_code, 0);
        assert_eq!(report.matches.len(), 1);
        assert_eq!(report.matches[0].action, GuardAction::Warn);
    }

    #[test]
    fn bypass_token_valid_lifts_halt_to_exit_zero() {
        let secret = b"workspace-secret-bytes";
        let command = "rm -rf /tmp/x";
        let token = issue_bypass_token("r1", command, secret);
        let registry = registry_with_only(vec![rule("r1", "*rm -rf*", GuardAction::Halt)]);
        let mut options = opts(command);
        options.bypass_secret = Some(secret.to_vec());
        options.bypass_tokens = vec![BypassTokenInput {
            rule_id: "r1".to_owned(),
            token,
        }];

        let report = run_preflight_guard(&registry, &options);
        assert_eq!(report.exit_code, 0, "valid token suppresses the halt");
        assert_eq!(
            report.matches[0].resolution,
            MatchResolution::BypassedWithToken
        );
    }

    #[test]
    fn bypass_token_invalid_keeps_halt() {
        let secret = b"workspace-secret-bytes";
        let registry = registry_with_only(vec![rule("r1", "*rm -rf*", GuardAction::Halt)]);
        let mut options = opts("rm -rf /tmp/x");
        options.bypass_secret = Some(secret.to_vec());
        options.bypass_tokens = vec![BypassTokenInput {
            rule_id: "r1".to_owned(),
            token: "deadbeef".repeat(8), // wrong token
        }];

        let report = run_preflight_guard(&registry, &options);
        assert_eq!(report.exit_code, 7);
        assert_eq!(
            report.matches[0].resolution,
            MatchResolution::BypassTokenInvalid
        );
    }

    #[test]
    fn bypass_token_for_different_rule_does_not_apply() {
        let secret = b"k";
        let command = "rm -rf /tmp/x";
        let r1_token = issue_bypass_token("r1", command, secret);
        let registry = registry_with_only(vec![rule("r2", "*rm -rf*", GuardAction::Halt)]);
        let mut options = opts(command);
        options.bypass_secret = Some(secret.to_vec());
        options.bypass_tokens = vec![BypassTokenInput {
            rule_id: "r1".to_owned(), // attempting to bypass r1, but r2 matches
            token: r1_token,
        }];

        let report = run_preflight_guard(&registry, &options);
        assert_eq!(report.exit_code, 7);
        assert_eq!(report.matches[0].resolution, MatchResolution::Enforced);
    }

    #[test]
    fn bypass_token_for_different_command_fails_verification() {
        let secret = b"k";
        let token_for_other_command = issue_bypass_token("r1", "rm -rf /etc", secret);
        let registry = registry_with_only(vec![rule("r1", "*rm -rf*", GuardAction::Halt)]);
        let mut options = opts("rm -rf /tmp/x");
        options.bypass_secret = Some(secret.to_vec());
        options.bypass_tokens = vec![BypassTokenInput {
            rule_id: "r1".to_owned(),
            token: token_for_other_command,
        }];

        let report = run_preflight_guard(&registry, &options);
        assert_eq!(report.exit_code, 7);
        assert_eq!(
            report.matches[0].resolution,
            MatchResolution::BypassTokenInvalid
        );
    }

    #[test]
    fn bypass_token_without_secret_is_marked_secret_missing() {
        let registry = registry_with_only(vec![rule("r1", "*rm -rf*", GuardAction::Halt)]);
        let mut options = opts("rm -rf /tmp");
        options.bypass_tokens = vec![BypassTokenInput {
            rule_id: "r1".to_owned(),
            token: "anything".to_owned(),
        }];
        // bypass_secret is None
        let report = run_preflight_guard(&registry, &options);
        assert_eq!(report.exit_code, 7);
        assert_eq!(
            report.matches[0].resolution,
            MatchResolution::BypassSecretMissing
        );
    }

    #[test]
    fn workspace_toml_layered_after_builtins() {
        let toml = r#"
[[rules]]
id = "ws_curl_pipe"
pattern = "*curl*|*sh*"
action = "halt"
message = "Reject curl|sh installers per workspace policy."
"#;
        let registry_result = PreflightGuardRegistry::from_toml(toml, "test.toml");
        assert!(
            registry_result.is_ok(),
            "parse should succeed: {registry_result:?}"
        );
        let registry = if let Ok(registry) = registry_result {
            registry
        } else {
            PreflightGuardRegistry::new()
        };
        let report = run_preflight_guard(
            &registry,
            &opts("curl https://example.com/install.sh | sh -"),
        );
        assert_eq!(report.exit_code, 7);
        assert_eq!(report.matches[0].rule_id, "ws_curl_pipe");
        assert_eq!(
            &report.matches[0].source,
            &RuleSource::WorkspaceFile {
                path: "test.toml".to_owned()
            }
        );
    }

    #[test]
    fn workspace_rules_not_directory_path_is_treated_as_absent() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::write(tempdir.path().join(".ee"), "not a metadata directory\n")
            .map_err(|error| error.to_string())?;

        let registry =
            PreflightGuardRegistry::load(tempdir.path()).map_err(|error| error.to_string())?;
        let mut options = opts("echo ok");
        options.workspace = tempdir.path().to_path_buf();
        let report = run_preflight_guard(&registry, &options);

        assert_eq!(report.exit_code, 0);
        assert!(report.matches.is_empty());
        Ok(())
    }

    #[test]
    fn workspace_toml_missing_id_is_usage_error() {
        let toml = r#"
[[rules]]
pattern = "*foo*"
"#;
        let registry_result = PreflightGuardRegistry::from_toml(toml, "bad.toml");
        assert!(registry_result.is_err(), "should reject missing id");
        let message = if let Err(err) = registry_result {
            err.message()
        } else {
            String::new()
        };
        assert!(message.contains("missing string `id`"), "{message}");
    }

    #[test]
    fn workspace_toml_invalid_action_is_usage_error() {
        let toml = r#"
[[rules]]
id = "x"
pattern = "*foo*"
action = "explode"
"#;
        let registry_result = PreflightGuardRegistry::from_toml(toml, "bad.toml");
        assert!(registry_result.is_err(), "should reject unknown action");
        let message = if let Err(err) = registry_result {
            err.message()
        } else {
            String::new()
        };
        assert!(message.contains("invalid action `explode`"), "{message}");
    }

    #[test]
    fn builtins_block_agents_md_forbidden_actions() {
        let registry = PreflightGuardRegistry::with_builtins();

        for command in [
            "rm -rf /",
            "rm -rf /tmp/work",
            "rm -rf ~/projects",
            "git reset --hard HEAD~3",
            "git clean -fd",
            "git worktree add ../parallel main",
        ] {
            let report = run_preflight_guard(&registry, &opts(command));
            assert_eq!(
                report.exit_code, 7,
                "command `{command}` should be halted by builtins",
            );
            assert!(
                !report.matches.is_empty(),
                "command `{command}` produced no match",
            );
            assert!(
                report
                    .matches
                    .iter()
                    .any(|m| matches!(m.source, RuleSource::Builtin { .. })),
                "command `{command}` did not cite a builtin rule",
            );
        }
    }

    #[test]
    fn builtin_rm_rf_rules_require_command_position() {
        let registry = PreflightGuardRegistry::with_builtins();

        for command in [
            "git log --grep=\"rm -rf /\"",
            "echo do not rm -rf / blindly",
            "confirm -rf /var/cache",
            "rm --force --preserve-root /var/cache",
        ] {
            let report = run_preflight_guard(&registry, &opts(command));
            assert_eq!(report.exit_code, 0, "command `{command}` should pass");
            assert!(report.matches.iter().all(|matched| {
                matched.rule_id != "builtin:rm_rf_root" && matched.rule_id != "builtin:rm_rf_home"
            }));
        }

        for command in [
            "cd /tmp && rm -rf /var/cache",
            "sudo rm -fr /var/cache",
            "sudo -n rm -rf /var/cache",
            "env FOO=bar rm -r -f ~/scratch",
        ] {
            let report = run_preflight_guard(&registry, &opts(command));
            assert_eq!(report.exit_code, 7, "command `{command}` should halt");
            assert!(report.matches.iter().any(|matched| {
                matched.rule_id == "builtin:rm_rf_root" || matched.rule_id == "builtin:rm_rf_home"
            }));
        }
    }

    #[test]
    fn builtin_force_push_warns_but_does_not_halt() {
        let registry = PreflightGuardRegistry::with_builtins();
        let report = run_preflight_guard(&registry, &opts("git push --force origin main"));
        assert_eq!(report.exit_code, 0);
        assert_eq!(report.matches.len(), 1);
        assert_eq!(report.matches[0].action, GuardAction::Warn);
        assert_eq!(report.matches[0].rule_id, "builtin:git_push_force");
    }

    #[test]
    fn issue_then_verify_round_trips() {
        let secret = b"some-secret";
        let token = issue_bypass_token("rule1", "rm -rf /tmp/x", secret);
        assert!(verify_bypass_token(
            &token,
            "rule1",
            "rm -rf /tmp/x",
            secret
        ));
        assert!(!verify_bypass_token(
            &token,
            "rule1",
            "rm -rf /tmp/y",
            secret
        ));
        assert!(!verify_bypass_token(
            &token,
            "rule2",
            "rm -rf /tmp/x",
            secret
        ));
        assert!(!verify_bypass_token(
            &token,
            "rule1",
            "rm -rf /tmp/x",
            b"different-secret"
        ));
    }

    #[test]
    fn json_output_uses_stable_schema() {
        let registry = registry_with_only(vec![rule("r1", "*rm -rf*", GuardAction::Halt)]);
        let report = run_preflight_guard(&registry, &opts("rm -rf /tmp"));
        let json = report.to_json();
        assert_eq!(json["schema"].as_str(), Some(PREFLIGHT_GUARD_SCHEMA_V1));
        assert_eq!(json["exitCode"].as_i64(), Some(7));
        let m0 = &json["matches"][0];
        assert_eq!(m0["ruleId"].as_str(), Some("r1"));
        assert_eq!(m0["action"].as_str(), Some("halt"));
        assert_eq!(m0["resolution"].as_str(), Some("enforced"));
    }

    #[test]
    fn json_output_aggregates_duplicate_degraded_codes() {
        let mut report =
            run_preflight_guard(&PreflightGuardRegistry::with_builtins(), &opts("echo ok"));
        report.degraded = vec![
            PreflightGuardDegradation {
                code: PREFLIGHT_PATTERNS_UNAVAILABLE_CODE,
                severity: "info",
                message: "First pattern catalog warning.".to_owned(),
                repair: "ee preflight check --cmd \"echo ok\" --json".to_owned(),
            },
            PreflightGuardDegradation {
                code: PREFLIGHT_PATTERNS_UNAVAILABLE_CODE,
                severity: "medium",
                message: "Second pattern catalog warning.".to_owned(),
                repair: "Check preflight rule sources.".to_owned(),
            },
        ];

        let json = report.to_json();
        let degraded = json["degraded"]
            .as_array()
            .expect("degraded array should be present");
        assert_eq!(
            degraded.len(),
            1,
            "expected one aggregated degradation, got {degraded:?}",
        );
        assert_eq!(
            degraded[0]["code"].as_str(),
            Some(PREFLIGHT_PATTERNS_UNAVAILABLE_CODE)
        );
        assert_eq!(degraded[0]["severity"].as_str(), Some("medium"));
        assert_eq!(
            degraded[0]["repair"].as_str(),
            Some("Check preflight rule sources.")
        );
        assert_eq!(degraded[0]["sources"][0].as_str(), Some("preflight_guard"));
    }
}
