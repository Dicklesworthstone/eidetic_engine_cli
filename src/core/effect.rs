//! Command effect manifest (EE-TST-009).
//!
//! Defines a first-class taxonomy of what each public command may do,
//! enabling agents to mechanically choose safe commands and proving
//! that read-only commands stay read-only.
//!
//! Effect classes:
//! - `ReadOnly` — no durable mutation, safe to call mid-task
//! - `DerivedArtifactWrite` — writes rebuildable indexes/caches
//! - `DurableMemoryWrite` — writes memories, audit records
//! - `WorkspaceFileWrite` — writes workspace files beyond DB
//! - `ConfigWrite` — modifies configuration
//! - `ExternalIo` — network or subprocess I/O
//!
//! The manifest maps each command path to its default effect, dry-run
//! effect, allowed write surfaces, and idempotency posture.

use std::collections::HashMap;

/// Effect class describing what a command may mutate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum EffectClass {
    /// No durable mutation. Safe to call at any time.
    ReadOnly,
    /// Writes derived artifacts (indexes, caches) that can be rebuilt.
    DerivedArtifactWrite,
    /// Writes durable memory records, audit log, or user-visible state.
    DurableMemoryWrite,
    /// Writes files in the workspace beyond the database.
    WorkspaceFileWrite,
    /// Modifies configuration (ee.toml, workspace config).
    ConfigWrite,
    /// Performs external I/O (network, subprocess).
    ExternalIo,
}

impl EffectClass {
    /// Stable string for JSON serialization and logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::DerivedArtifactWrite => "derived_artifact_write",
            Self::DurableMemoryWrite => "durable_memory_write",
            Self::WorkspaceFileWrite => "workspace_file_write",
            Self::ConfigWrite => "config_write",
            Self::ExternalIo => "external_io",
        }
    }

    /// `true` if this effect class mutates durable user-visible state.
    #[must_use]
    pub const fn is_mutating(self) -> bool {
        !matches!(self, Self::ReadOnly)
    }

    /// `true` if mutations are rebuildable (indexes, caches).
    #[must_use]
    pub const fn is_derived(self) -> bool {
        matches!(self, Self::DerivedArtifactWrite)
    }
}

/// Idempotency behavior of a command.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum IdempotencyClass {
    /// Running twice produces the same observable outcome.
    Idempotent,
    /// Running twice may produce different outcomes (e.g., new memory IDs).
    NonIdempotent,
    /// Command supports `--dry-run` to preview without mutation.
    DryRunAvailable,
}

impl IdempotencyClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idempotent => "idempotent",
            Self::NonIdempotent => "non_idempotent",
            Self::DryRunAvailable => "dry_run_available",
        }
    }
}

/// Allowed write surfaces for a command.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WriteSurfaces {
    /// Database tables the command may write.
    pub db_tables: Vec<&'static str>,
    /// Derived artifact paths (relative to workspace).
    pub derived_paths: Vec<&'static str>,
    /// Workspace file patterns the command may write.
    pub workspace_files: Vec<&'static str>,
}

impl WriteSurfaces {
    #[must_use]
    pub const fn none() -> Self {
        Self {
            db_tables: Vec::new(),
            derived_paths: Vec::new(),
            workspace_files: Vec::new(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.db_tables.is_empty()
            && self.derived_paths.is_empty()
            && self.workspace_files.is_empty()
    }
}

/// Effect manifest entry for a single command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandEffect {
    /// Command path (e.g., "status", "memory list", "index rebuild").
    pub command_path: &'static str,
    /// Default effect class when run normally.
    pub default_effect: EffectClass,
    /// Effect class when run with `--dry-run` (if supported).
    pub dry_run_effect: Option<EffectClass>,
    /// Idempotency behavior.
    pub idempotency: IdempotencyClass,
    /// Surfaces the command may write.
    pub write_surfaces: WriteSurfaces,
    /// Whether command requires audit log write.
    pub requires_audit: bool,
    /// Human-readable description of the effect.
    pub description: &'static str,
}

impl CommandEffect {
    /// Create a read-only effect entry.
    #[must_use]
    pub const fn read_only(command_path: &'static str, description: &'static str) -> Self {
        Self {
            command_path,
            default_effect: EffectClass::ReadOnly,
            dry_run_effect: None,
            idempotency: IdempotencyClass::Idempotent,
            write_surfaces: WriteSurfaces::none(),
            requires_audit: false,
            description,
        }
    }

    /// Create a derived-artifact-write effect entry.
    #[must_use]
    pub fn derived_write(
        command_path: &'static str,
        derived_paths: Vec<&'static str>,
        description: &'static str,
    ) -> Self {
        Self {
            command_path,
            default_effect: EffectClass::DerivedArtifactWrite,
            dry_run_effect: Some(EffectClass::ReadOnly),
            idempotency: IdempotencyClass::Idempotent,
            write_surfaces: WriteSurfaces {
                db_tables: Vec::new(),
                derived_paths,
                workspace_files: Vec::new(),
            },
            requires_audit: false,
            description,
        }
    }

    /// Create a durable-memory-write effect entry.
    #[must_use]
    pub fn durable_write(
        command_path: &'static str,
        db_tables: Vec<&'static str>,
        description: &'static str,
    ) -> Self {
        Self {
            command_path,
            default_effect: EffectClass::DurableMemoryWrite,
            dry_run_effect: Some(EffectClass::ReadOnly),
            idempotency: IdempotencyClass::NonIdempotent,
            write_surfaces: WriteSurfaces {
                db_tables,
                derived_paths: Vec::new(),
                workspace_files: Vec::new(),
            },
            requires_audit: true,
            description,
        }
    }

    /// `true` if running this command is safe mid-task (no durable mutation).
    #[must_use]
    pub const fn is_safe_mid_task(&self) -> bool {
        matches!(self.default_effect, EffectClass::ReadOnly)
    }
}

/// The complete command effect manifest.
#[derive(Clone, Debug)]
pub struct EffectManifest {
    entries: HashMap<&'static str, CommandEffect>,
}

impl EffectManifest {
    /// Build the manifest from the canonical command list.
    #[must_use]
    pub fn build() -> Self {
        let mut entries = HashMap::new();

        // Read-only commands
        for entry in Self::read_only_commands() {
            entries.insert(entry.command_path, entry);
        }

        // Derived artifact write commands
        for entry in Self::derived_write_commands() {
            entries.insert(entry.command_path, entry);
        }

        // Durable write commands
        for entry in Self::durable_write_commands() {
            entries.insert(entry.command_path, entry);
        }

        Self { entries }
    }

    fn read_only_commands() -> Vec<CommandEffect> {
        vec![
            CommandEffect::read_only("agent-docs", "Display agent documentation"),
            CommandEffect::read_only("capabilities", "Report feature availability"),
            CommandEffect::read_only("check", "Quick posture summary"),
            CommandEffect::read_only("context", "Assemble context pack (reads only)"),
            CommandEffect::read_only("diag quarantine", "Show quarantine status"),
            CommandEffect::read_only("diag streams", "Show streams status"),
            CommandEffect::read_only("diag trust", "Show trust decay status"),
            CommandEffect::read_only("doctor", "Run health checks"),
            CommandEffect::read_only("eval list", "List evaluation scenarios"),
            CommandEffect::read_only("eval run", "Run evaluation (reads fixtures)"),
            CommandEffect::read_only("health", "Quick health check"),
            CommandEffect::read_only("help", "Print help"),
            CommandEffect::read_only("import list", "List import sources"),
            CommandEffect::read_only("index status", "Show index status"),
            CommandEffect::read_only("install check", "Inspect install posture"),
            CommandEffect::read_only("install plan", "Plan install without mutation"),
            CommandEffect::read_only("introspect", "Introspect ee metadata"),
            CommandEffect::read_only("memory history", "Show memory revision history"),
            CommandEffect::read_only("memory list", "List memories"),
            CommandEffect::read_only("memory show", "Show memory details"),
            CommandEffect::read_only("memory show-link", "Show memory link"),
            CommandEffect::read_only("memory show-links", "List memory links"),
            CommandEffect::read_only("schema list", "List response schemas"),
            CommandEffect::read_only("schema show", "Show schema definition"),
            CommandEffect::read_only("search", "Search memories"),
            CommandEffect::read_only("status", "Report workspace status"),
            CommandEffect::read_only("update", "Plan update without mutation"),
            CommandEffect::read_only("version", "Print version"),
            CommandEffect::read_only("why", "Explain memory selection"),
        ]
    }

    fn derived_write_commands() -> Vec<CommandEffect> {
        vec![
            CommandEffect::derived_write(
                "index rebuild",
                vec![".ee/index/"],
                "Rebuild search indexes from database",
            ),
            CommandEffect::derived_write(
                "index refresh",
                vec![".ee/index/"],
                "Refresh stale search indexes",
            ),
        ]
    }

    fn durable_write_commands() -> Vec<CommandEffect> {
        vec![
            CommandEffect::durable_write(
                "import cass",
                vec!["memories", "audit_log"],
                "Import from CASS sessions",
            ),
            CommandEffect::durable_write(
                "outcome",
                vec!["feedback_events", "audit_log"],
                "Record observed outcome feedback",
            ),
            CommandEffect::durable_write(
                "remember",
                vec!["memories", "memory_tags", "audit_log"],
                "Store a new memory",
            ),
        ]
    }

    /// Get the effect entry for a command path.
    #[must_use]
    pub fn get(&self, command_path: &str) -> Option<&CommandEffect> {
        self.entries.get(command_path)
    }

    /// All command paths in the manifest.
    #[must_use]
    pub fn command_paths(&self) -> Vec<&'static str> {
        let mut paths: Vec<_> = self.entries.keys().copied().collect();
        paths.sort_unstable();
        paths
    }

    /// Commands that are safe to call mid-task (read-only).
    #[must_use]
    pub fn safe_mid_task_commands(&self) -> Vec<&CommandEffect> {
        self.entries
            .values()
            .filter(|e| e.is_safe_mid_task())
            .collect()
    }

    /// Commands that perform durable mutations.
    #[must_use]
    pub fn mutating_commands(&self) -> Vec<&CommandEffect> {
        self.entries
            .values()
            .filter(|e| e.default_effect.is_mutating())
            .collect()
    }

    /// Number of commands in the manifest.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if the manifest is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for EffectManifest {
    fn default() -> Self {
        Self::build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn ensure_at_least<T: std::fmt::Debug + PartialOrd>(
        actual: T,
        minimum: T,
        ctx: &str,
    ) -> TestResult {
        if actual >= minimum {
            Ok(())
        } else {
            Err(format!(
                "{ctx}: expected at least {minimum:?}, got {actual:?}"
            ))
        }
    }

    #[test]
    fn effect_class_strings_are_stable() -> TestResult {
        ensure(EffectClass::ReadOnly.as_str(), "read_only", "read_only")?;
        ensure(
            EffectClass::DerivedArtifactWrite.as_str(),
            "derived_artifact_write",
            "derived_artifact_write",
        )?;
        ensure(
            EffectClass::DurableMemoryWrite.as_str(),
            "durable_memory_write",
            "durable_memory_write",
        )
    }

    #[test]
    fn effect_class_is_mutating_classifies_correctly() -> TestResult {
        ensure(EffectClass::ReadOnly.is_mutating(), false, "read_only")?;
        ensure(
            EffectClass::DerivedArtifactWrite.is_mutating(),
            true,
            "derived_artifact_write",
        )?;
        ensure(
            EffectClass::DurableMemoryWrite.is_mutating(),
            true,
            "durable_memory_write",
        )
    }

    #[test]
    fn idempotency_strings_are_stable() -> TestResult {
        ensure(
            IdempotencyClass::Idempotent.as_str(),
            "idempotent",
            "idempotent",
        )?;
        ensure(
            IdempotencyClass::NonIdempotent.as_str(),
            "non_idempotent",
            "non_idempotent",
        )?;
        ensure(
            IdempotencyClass::DryRunAvailable.as_str(),
            "dry_run_available",
            "dry_run_available",
        )
    }

    #[test]
    fn command_effect_read_only_is_safe_mid_task() -> TestResult {
        let effect = CommandEffect::read_only("status", "Report status");
        ensure(
            effect.is_safe_mid_task(),
            true,
            "read_only is safe mid-task",
        )?;
        ensure(
            effect.default_effect,
            EffectClass::ReadOnly,
            "default effect",
        )?;
        ensure(effect.requires_audit, false, "no audit required")
    }

    #[test]
    fn command_effect_durable_write_requires_audit() -> TestResult {
        let effect = CommandEffect::durable_write("remember", vec!["memories"], "Store memory");
        ensure(
            effect.is_safe_mid_task(),
            false,
            "durable write not safe mid-task",
        )?;
        ensure(
            effect.default_effect,
            EffectClass::DurableMemoryWrite,
            "default effect",
        )?;
        ensure(effect.requires_audit, true, "audit required")?;
        ensure(
            effect.dry_run_effect,
            Some(EffectClass::ReadOnly),
            "dry_run reduces to read_only",
        )
    }

    #[test]
    fn command_effect_derived_write_is_idempotent() -> TestResult {
        let effect =
            CommandEffect::derived_write("index rebuild", vec![".ee/index/"], "Rebuild indexes");
        ensure(
            effect.idempotency,
            IdempotencyClass::Idempotent,
            "derived write is idempotent",
        )?;
        ensure(
            effect.default_effect,
            EffectClass::DerivedArtifactWrite,
            "default effect",
        )
    }

    #[test]
    fn manifest_build_includes_all_command_classes() -> TestResult {
        let manifest = EffectManifest::build();

        ensure_at_least(manifest.len(), 20, "at least 20 commands")?;

        let safe = manifest.safe_mid_task_commands();
        ensure_at_least(safe.len(), 15, "at least 15 safe commands")?;

        let mutating = manifest.mutating_commands();
        ensure_at_least(mutating.len(), 2, "at least 2 mutating commands")
    }

    #[test]
    fn manifest_get_returns_correct_entry() -> TestResult {
        let manifest = EffectManifest::build();

        let status = manifest.get("status");
        ensure(status.is_some(), true, "status exists")?;
        ensure(
            status.map(|e| e.default_effect),
            Some(EffectClass::ReadOnly),
            "status is read_only",
        )?;

        let remember = manifest.get("remember");
        ensure(remember.is_some(), true, "remember exists")?;
        ensure(
            remember.map(|e| e.default_effect),
            Some(EffectClass::DurableMemoryWrite),
            "remember is durable_memory_write",
        )?;

        let outcome = manifest.get("outcome");
        ensure(outcome.is_some(), true, "outcome exists")?;
        ensure(
            outcome.map(|e| e.write_surfaces.db_tables.clone()),
            Some(vec!["feedback_events", "audit_log"]),
            "outcome writes feedback and audit",
        )
    }

    #[test]
    fn manifest_command_paths_are_sorted() -> TestResult {
        let manifest = EffectManifest::build();
        let paths = manifest.command_paths();

        let mut sorted = paths.clone();
        sorted.sort_unstable();
        ensure(paths, sorted, "paths are sorted")
    }

    #[test]
    fn write_surfaces_none_is_empty() -> TestResult {
        let surfaces = WriteSurfaces::none();
        ensure(surfaces.is_empty(), true, "none is empty")
    }

    #[test]
    fn remember_command_writes_to_memory_tables() -> TestResult {
        let manifest = EffectManifest::build();
        let remember = manifest
            .get("remember")
            .ok_or_else(|| "remember not found".to_string())?;

        let has_memories = remember.write_surfaces.db_tables.contains(&"memories");
        ensure(has_memories, true, "writes to memories table")?;

        let has_audit = remember.write_surfaces.db_tables.contains(&"audit_log");
        ensure(has_audit, true, "writes to audit_log")
    }

    #[test]
    fn index_rebuild_writes_to_index_path() -> TestResult {
        let manifest = EffectManifest::build();
        let rebuild = manifest
            .get("index rebuild")
            .ok_or_else(|| "index rebuild not found".to_string())?;

        let has_index = rebuild.write_surfaces.derived_paths.contains(&".ee/index/");
        ensure(has_index, true, "writes to .ee/index/")
    }

    // ========================================================================
    // No-Mutation Contract Tests
    // ========================================================================

    #[test]
    fn all_read_only_commands_have_empty_write_surfaces() -> TestResult {
        let manifest = EffectManifest::build();

        for effect in manifest.safe_mid_task_commands() {
            if !effect.write_surfaces.is_empty() {
                return Err(format!(
                    "Read-only command '{}' has non-empty write surfaces",
                    effect.command_path
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn all_read_only_commands_do_not_require_audit() -> TestResult {
        let manifest = EffectManifest::build();

        for effect in manifest.safe_mid_task_commands() {
            if effect.requires_audit {
                return Err(format!(
                    "Read-only command '{}' requires audit, but should not",
                    effect.command_path
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn all_durable_write_commands_require_audit() -> TestResult {
        let manifest = EffectManifest::build();

        for effect in manifest.entries.values() {
            if effect.default_effect == EffectClass::DurableMemoryWrite && !effect.requires_audit {
                return Err(format!(
                    "Durable-write command '{}' does not require audit, but should",
                    effect.command_path
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn all_mutating_commands_have_dry_run_option() -> TestResult {
        let manifest = EffectManifest::build();

        for effect in manifest.mutating_commands() {
            if effect.dry_run_effect.is_none() {
                return Err(format!(
                    "Mutating command '{}' has no dry_run option",
                    effect.command_path
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn effect_class_ordering_is_monotone() -> TestResult {
        // ReadOnly < DerivedArtifactWrite < DurableMemoryWrite < ...
        ensure(
            EffectClass::ReadOnly < EffectClass::DerivedArtifactWrite,
            true,
            "read_only < derived_artifact_write",
        )?;
        ensure(
            EffectClass::DerivedArtifactWrite < EffectClass::DurableMemoryWrite,
            true,
            "derived_artifact_write < durable_memory_write",
        )
    }
}
