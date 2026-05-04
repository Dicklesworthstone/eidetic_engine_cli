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

/// Side-effect class names shared with the command-boundary matrix.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum SideEffectClass {
    /// No DB, index, cache, or filesystem mutation.
    ReadOnly,
    /// Current command is read-only or degraded; future writes need a new audited class.
    ReadOnlyNow,
    /// Report computed from explicit inputs without durable mutation.
    ReportOnly,
    /// Static reads are allowed; missing or judgment-heavy work degrades or hands off.
    ReadOnlyOrUnavailable,
    /// Append new records, or return an existing record by idempotency key.
    AppendOnly,
    /// Durable mutation in one audited transaction.
    AuditedMutation,
    /// Rebuild only derived, rebuildable assets keyed by source generation.
    DerivedAssetRebuild,
    /// Create or verify a side-path artifact without overwriting source data.
    SidePathArtifact,
    /// Long-running job mutation through a supervised job ledger.
    SupervisedJobs,
    /// Family contains both read-only and mutating subcommands.
    Mixed,
    /// No mutation until the real implementation exists.
    DegradedUnavailable,
    /// Read-only extraction today; future candidate writes require an explicit append path.
    ReportOnlyOrAppend,
    /// Read-only reports today; relation writes require an explicit audited transaction.
    ReportOnlyOrAuditedMutation,
}

impl SideEffectClass {
    /// Stable vocabulary token used in docs, JSON logs, and tests.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "class=read_only",
            Self::ReadOnlyNow => "class=read_only_now",
            Self::ReportOnly => "class=report_only",
            Self::ReadOnlyOrUnavailable => "class=read_only_or_unavailable",
            Self::AppendOnly => "class=append_only",
            Self::AuditedMutation => "class=audited_mutation",
            Self::DerivedAssetRebuild => "class=derived_asset_rebuild",
            Self::SidePathArtifact => "class=side_path_artifact",
            Self::SupervisedJobs => "class=supervised_jobs",
            Self::Mixed => "class=mixed",
            Self::DegradedUnavailable => "class=degraded_unavailable",
            Self::ReportOnlyOrAppend => "class=report_only_or_append",
            Self::ReportOnlyOrAuditedMutation => "class=report_only_or_audited_mutation",
        }
    }

    /// `true` if this class forbids durable mutation.
    #[must_use]
    pub const fn declares_no_durable_mutation(self) -> bool {
        matches!(
            self,
            Self::ReadOnly
                | Self::ReadOnlyNow
                | Self::ReportOnly
                | Self::ReadOnlyOrUnavailable
                | Self::DegradedUnavailable
        )
    }

    /// `true` if this class must carry no-overwrite side-path behavior.
    #[must_use]
    pub const fn requires_no_overwrite_contract(self) -> bool {
        matches!(self, Self::SidePathArtifact)
    }

    /// `true` if this class must carry transaction/audit metadata.
    #[must_use]
    pub const fn requires_audited_transaction_contract(self) -> bool {
        matches!(self, Self::AppendOnly | Self::AuditedMutation)
    }
}

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

/// Cross-cutting mutation contract for a command manifest entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandMutationContract {
    /// Side-effect class from the command-boundary matrix vocabulary.
    pub side_effect_class: SideEffectClass,
    /// Named transaction scope, if the command mutates durable state.
    pub transaction_scope: Option<&'static str>,
    /// Idempotency key or retry posture.
    pub idempotency_key: Option<&'static str>,
    /// Audit surface written by the command, if any.
    pub audit_surface: Option<&'static str>,
    /// Effect on source database generation.
    pub db_generation_effect: &'static str,
    /// Effect on derived index or cache generation.
    pub index_generation_effect: &'static str,
    /// Dry-run or preview behavior, if exposed by this class.
    pub dry_run_behavior: Option<&'static str>,
    /// Recovery, rollback, or degraded behavior.
    pub recovery_behavior: &'static str,
    /// Side-path no-overwrite/no-delete behavior, when applicable.
    pub no_overwrite_behavior: Option<&'static str>,
    /// Degraded/error code returned when the command intentionally abstains.
    pub degraded_code: Option<&'static str>,
}

impl CommandMutationContract {
    #[must_use]
    pub const fn read_only() -> Self {
        Self {
            side_effect_class: SideEffectClass::ReadOnly,
            transaction_scope: None,
            idempotency_key: Some("full command argv and explicit inputs"),
            audit_surface: None,
            db_generation_effect: "none",
            index_generation_effect: "none",
            dry_run_behavior: None,
            recovery_behavior: "no durable changes to recover",
            no_overwrite_behavior: None,
            degraded_code: None,
        }
    }

    #[must_use]
    pub const fn derived_asset_rebuild(
        idempotency_key: &'static str,
        recovery_behavior: &'static str,
    ) -> Self {
        Self {
            side_effect_class: SideEffectClass::DerivedAssetRebuild,
            transaction_scope: Some("derived asset rebuild keyed by source generation"),
            idempotency_key: Some(idempotency_key),
            audit_surface: None,
            db_generation_effect: "source DB generation unchanged",
            index_generation_effect: "derived generation may advance to source generation",
            dry_run_behavior: Some("preview only; no derived files are written"),
            recovery_behavior,
            no_overwrite_behavior: None,
            degraded_code: None,
        }
    }

    #[must_use]
    pub const fn audited_mutation(idempotency_key: &'static str) -> Self {
        Self {
            side_effect_class: SideEffectClass::AuditedMutation,
            transaction_scope: Some("single DB transaction across write surfaces"),
            idempotency_key: Some(idempotency_key),
            audit_surface: Some("audit_log"),
            db_generation_effect: "advances on commit; unchanged on rollback",
            index_generation_effect: "queues or refreshes derived index after commit when applicable",
            dry_run_behavior: Some("no DB rows, audit rows, or derived index jobs are written"),
            recovery_behavior: "transaction rollback leaves no partial durable records",
            no_overwrite_behavior: None,
            degraded_code: None,
        }
    }

    #[must_use]
    pub const fn append_only(idempotency_key: &'static str) -> Self {
        Self {
            side_effect_class: SideEffectClass::AppendOnly,
            transaction_scope: Some("single append transaction across write surfaces"),
            idempotency_key: Some(idempotency_key),
            audit_surface: Some("audit_log"),
            db_generation_effect: "advances only when a new record commits; unchanged when idempotency key matches",
            index_generation_effect: "queues or refreshes derived index after new records commit",
            dry_run_behavior: Some("no DB rows, audit rows, or derived index jobs are written"),
            recovery_behavior: "transaction rollback leaves no partial append records",
            no_overwrite_behavior: None,
            degraded_code: None,
        }
    }

    #[must_use]
    pub const fn side_path_artifact(
        idempotency_key: &'static str,
        no_overwrite_behavior: &'static str,
    ) -> Self {
        Self {
            side_effect_class: SideEffectClass::SidePathArtifact,
            transaction_scope: Some("side-path artifact creation outside source DB mutation"),
            idempotency_key: Some(idempotency_key),
            audit_surface: Some("artifact manifest or audit_log when DB backing exists"),
            db_generation_effect: "source DB generation unchanged unless manifest audit is committed",
            index_generation_effect: "none",
            dry_run_behavior: Some("preview artifact path and manifest only; no files are written"),
            recovery_behavior: "partial side-path output is reported as failed, never deleted by ee, and not treated as a valid artifact",
            no_overwrite_behavior: Some(no_overwrite_behavior),
            degraded_code: None,
        }
    }

    #[must_use]
    pub const fn degraded_unavailable(degraded_code: &'static str) -> Self {
        Self {
            side_effect_class: SideEffectClass::DegradedUnavailable,
            transaction_scope: None,
            idempotency_key: Some("full command argv and explicit inputs"),
            audit_surface: None,
            db_generation_effect: "none",
            index_generation_effect: "none",
            dry_run_behavior: None,
            recovery_behavior: "returns an explicit degraded response without mutation",
            no_overwrite_behavior: None,
            degraded_code: Some(degraded_code),
        }
    }

    #[must_use]
    pub fn declares_no_source_mutation(&self) -> bool {
        matches!(
            self.db_generation_effect,
            "none" | "source DB generation unchanged"
        )
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

/// Runtime class for cancellation and budget behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum RuntimeClass {
    /// Completes without meaningful async checkpoints.
    Immediate,
    /// Bounded local probes or DB reads; cancellation checked around boundaries.
    Bounded,
    /// Potentially long-running work with explicit budget/deadline checkpoints.
    LongRunning,
    /// Multi-stage work with commit/publish boundaries and cleanup policy.
    MultiStage,
    /// Work coordinated through a supervised child/job ledger.
    Supervised,
}

impl RuntimeClass {
    /// Stable vocabulary token used in boundary logs and tests.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Immediate => "immediate",
            Self::Bounded => "bounded",
            Self::LongRunning => "long_running",
            Self::MultiStage => "multi_stage",
            Self::Supervised => "supervised",
        }
    }

    /// `true` when commands in this class need an explicit runtime budget.
    #[must_use]
    pub const fn requires_budget(self) -> bool {
        matches!(
            self,
            Self::LongRunning | Self::MultiStage | Self::Supervised
        )
    }
}

/// Cross-cutting runtime contract for a command manifest entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandRuntimeContract {
    /// Classifies the runtime shape of the command.
    pub runtime_class: RuntimeClass,
    /// Default runtime budget in milliseconds, if the command is budgeted.
    pub default_budget_ms: Option<u64>,
    /// Stable cancellation checkpoints named in boundary logs.
    pub cancellation_points: &'static [&'static str],
    /// Policy for cleanup or audit after partial progress.
    pub partial_progress_policy: &'static str,
    /// Deterministic mapping from runtime outcome to CLI result/log outcome.
    pub outcome_mapping: &'static str,
}

impl CommandRuntimeContract {
    #[must_use]
    pub const fn immediate() -> Self {
        Self {
            runtime_class: RuntimeClass::Immediate,
            default_budget_ms: None,
            cancellation_points: &["before_start"],
            partial_progress_policy: "no durable partial progress is possible",
            outcome_mapping: "success or explicit degraded/error response",
        }
    }

    #[must_use]
    pub const fn bounded_read() -> Self {
        Self {
            runtime_class: RuntimeClass::Bounded,
            default_budget_ms: Some(30_000),
            cancellation_points: &["before_start", "between_bounded_probes"],
            partial_progress_policy: "read-only; no durable partial progress is possible",
            outcome_mapping: "success, degraded, or read-side error with no mutation",
        }
    }

    #[must_use]
    pub const fn long_running_derived() -> Self {
        Self {
            runtime_class: RuntimeClass::LongRunning,
            default_budget_ms: Some(300_000),
            cancellation_points: &["before_start", "source_scan", "before_publish"],
            partial_progress_policy: "derived artifacts publish atomically; failed generations are ignored until a complete publish",
            outcome_mapping: "success, cancelled, budget_exhausted, or index_error",
        }
    }

    #[must_use]
    pub const fn transactional() -> Self {
        Self {
            runtime_class: RuntimeClass::MultiStage,
            default_budget_ms: Some(60_000),
            cancellation_points: &["before_start", "before_transaction", "before_commit"],
            partial_progress_policy: "single transaction rollback leaves no unaudited durable records",
            outcome_mapping: "success, cancelled, budget_exhausted, storage_error, or degraded",
        }
    }

    #[must_use]
    pub const fn side_path_artifact() -> Self {
        Self {
            runtime_class: RuntimeClass::MultiStage,
            default_budget_ms: Some(120_000),
            cancellation_points: &["before_start", "before_artifact_write", "before_manifest"],
            partial_progress_policy: "partial side-path output is reported, never deleted by ee, and is not a valid artifact until manifested",
            outcome_mapping: "success, cancelled, budget_exhausted, storage_error, or degraded",
        }
    }

    #[must_use]
    pub const fn supervised_unavailable() -> Self {
        Self {
            runtime_class: RuntimeClass::Supervised,
            default_budget_ms: Some(300_000),
            cancellation_points: &["before_start", "before_child_spawn", "child_outcome"],
            partial_progress_policy: "supervised jobs must record child failure or cancellation before reporting completion",
            outcome_mapping: "degraded, cancelled, budget_exhausted, or supervised_child_failed",
        }
    }

    #[must_use]
    pub const fn requires_budget(&self) -> bool {
        self.runtime_class.requires_budget()
    }

    pub fn effective_budget_ms(
        &self,
        requested_budget_ms: Option<u64>,
    ) -> Result<Option<u64>, &'static str> {
        match requested_budget_ms {
            Some(0) => Err("runtime budget must be greater than zero"),
            Some(budget) => Ok(Some(budget)),
            None => Ok(self.default_budget_ms),
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
    /// Cross-cutting side-effect and mutation-safety contract.
    pub mutation_contract: CommandMutationContract,
    /// Cross-cutting runtime, cancellation, and budget contract.
    pub runtime_contract: CommandRuntimeContract,
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
            mutation_contract: CommandMutationContract::read_only(),
            runtime_contract: CommandRuntimeContract::bounded_read(),
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
            mutation_contract: CommandMutationContract::derived_asset_rebuild(
                "source DB generation",
                "derived artifacts are rebuildable from FrankenSQLite source records",
            ),
            runtime_contract: CommandRuntimeContract::long_running_derived(),
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
            mutation_contract: CommandMutationContract::audited_mutation(
                "caller-provided key or generated durable record ID",
            ),
            runtime_contract: CommandRuntimeContract::transactional(),
            requires_audit: true,
            description,
        }
    }

    /// Create an append-only durable-write effect entry.
    #[must_use]
    pub fn append_only_write(
        command_path: &'static str,
        db_tables: Vec<&'static str>,
        idempotency_key: &'static str,
        description: &'static str,
    ) -> Self {
        Self {
            command_path,
            default_effect: EffectClass::DurableMemoryWrite,
            dry_run_effect: Some(EffectClass::ReadOnly),
            idempotency: IdempotencyClass::Idempotent,
            write_surfaces: WriteSurfaces {
                db_tables,
                derived_paths: Vec::new(),
                workspace_files: Vec::new(),
            },
            mutation_contract: CommandMutationContract::append_only(idempotency_key),
            runtime_contract: CommandRuntimeContract::transactional(),
            requires_audit: true,
            description,
        }
    }

    /// Create a workspace-file-write effect entry.
    #[must_use]
    pub fn workspace_file_write(
        command_path: &'static str,
        workspace_files: Vec<&'static str>,
        description: &'static str,
    ) -> Self {
        Self {
            command_path,
            default_effect: EffectClass::WorkspaceFileWrite,
            dry_run_effect: Some(EffectClass::ReadOnly),
            idempotency: IdempotencyClass::NonIdempotent,
            write_surfaces: WriteSurfaces {
                db_tables: Vec::new(),
                derived_paths: Vec::new(),
                workspace_files,
            },
            mutation_contract: CommandMutationContract::side_path_artifact(
                "artifact path plus manifest hash",
                "no-overwrite/no-delete: existing output paths block unless the verifier proves the same manifest",
            ),
            runtime_contract: CommandRuntimeContract::side_path_artifact(),
            requires_audit: true,
            description,
        }
    }

    /// Create a config-write effect entry.
    #[must_use]
    pub fn config_write(
        command_path: &'static str,
        workspace_files: Vec<&'static str>,
        idempotency_key: &'static str,
        description: &'static str,
    ) -> Self {
        Self {
            command_path,
            default_effect: EffectClass::ConfigWrite,
            dry_run_effect: Some(EffectClass::ReadOnly),
            idempotency: IdempotencyClass::Idempotent,
            write_surfaces: WriteSurfaces {
                db_tables: vec!["workspace_registry", "audit_log"],
                derived_paths: Vec::new(),
                workspace_files,
            },
            mutation_contract: CommandMutationContract::audited_mutation(idempotency_key),
            runtime_contract: CommandRuntimeContract::transactional(),
            requires_audit: true,
            description,
        }
    }

    /// Create a degraded/unavailable read-only effect entry.
    #[must_use]
    pub const fn degraded_unavailable(
        command_path: &'static str,
        degraded_code: &'static str,
        description: &'static str,
    ) -> Self {
        Self {
            command_path,
            default_effect: EffectClass::ReadOnly,
            dry_run_effect: None,
            idempotency: IdempotencyClass::Idempotent,
            write_surfaces: WriteSurfaces::none(),
            mutation_contract: CommandMutationContract::degraded_unavailable(degraded_code),
            runtime_contract: CommandRuntimeContract::immediate(),
            requires_audit: false,
            description,
        }
    }

    /// Override the default runtime contract for a specific command path.
    #[must_use]
    pub const fn with_runtime_contract(mut self, runtime_contract: CommandRuntimeContract) -> Self {
        self.runtime_contract = runtime_contract;
        self
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

        // Explicitly unavailable commands that must not mutate.
        for entry in Self::degraded_unavailable_commands() {
            entries.insert(entry.command_path, entry);
        }

        // Derived artifact write commands
        for entry in Self::derived_write_commands() {
            entries.insert(entry.command_path, entry);
        }

        // Append-only write commands
        for entry in Self::append_only_write_commands() {
            entries.insert(entry.command_path, entry);
        }

        // Durable write commands
        for entry in Self::durable_write_commands() {
            entries.insert(entry.command_path, entry);
        }

        // Config write commands
        for entry in Self::config_write_commands() {
            entries.insert(entry.command_path, entry);
        }

        // Workspace file write commands
        for entry in Self::workspace_file_write_commands() {
            entries.insert(entry.command_path, entry);
        }

        Self { entries }
    }

    fn read_only_commands() -> Vec<CommandEffect> {
        vec![
            CommandEffect::read_only("agent detect", "Detect installed coding agents"),
            CommandEffect::read_only("agent scan", "Scan agent probe paths"),
            CommandEffect::read_only("agent sources", "List known agent source connectors"),
            CommandEffect::read_only("agent status", "Report local agent inventory status"),
            CommandEffect::read_only("analyze science-status", "Report science readiness"),
            CommandEffect::read_only("agent-docs", "Display agent documentation"),
            CommandEffect::read_only("backup inspect", "Inspect backup manifest"),
            CommandEffect::read_only("backup list", "List backup manifests"),
            CommandEffect::read_only("backup verify", "Verify backup manifest and contents"),
            CommandEffect::read_only("capabilities", "Report feature availability"),
            CommandEffect::read_only("check", "Quick posture summary"),
            CommandEffect::read_only("context", "Assemble context pack (reads only)"),
            CommandEffect::read_only("curate candidates", "List curation candidates"),
            CommandEffect::read_only("curate validate", "Validate curation candidate"),
            CommandEffect::read_only("demo list", "List demo manifests"),
            CommandEffect::read_only("demo verify", "Verify demo artifacts"),
            CommandEffect::read_only("diag claims", "Inspect claim diagnostics"),
            CommandEffect::read_only("diag dependencies", "Inspect dependency diagnostics"),
            CommandEffect::read_only("diag graph", "Inspect graph diagnostics"),
            CommandEffect::read_only("diag integrity", "Inspect storage integrity diagnostics"),
            CommandEffect::read_only("diag quarantine", "Show quarantine status"),
            CommandEffect::read_only("diag streams", "Show streams status"),
            CommandEffect::read_only("doctor", "Run health checks"),
            CommandEffect::read_only("eval list", "List evaluation scenarios"),
            CommandEffect::read_only("eval run", "Run evaluation (reads fixtures)"),
            CommandEffect::read_only(
                "economy prune-plan",
                "Plan economy pruning without mutation",
            ),
            CommandEffect::read_only("graph export", "Export graph projection report"),
            CommandEffect::read_only("graph neighborhood", "Inspect graph neighborhood"),
            CommandEffect::read_only("handoff inspect", "Inspect handoff capsule"),
            CommandEffect::read_only("handoff resume", "Render handoff resume payload"),
            CommandEffect::read_only("artifact inspect", "Inspect artifact metadata"),
            CommandEffect::read_only("artifact list", "List registered artifacts"),
            CommandEffect::read_only("health", "Quick health check"),
            CommandEffect::read_only("help", "Print help"),
            CommandEffect::read_only("index status", "Show index status"),
            CommandEffect::read_only("install check", "Inspect install posture"),
            CommandEffect::read_only("install plan", "Plan install without mutation"),
            CommandEffect::read_only("introspect", "Introspect ee metadata"),
            CommandEffect::read_only("mcp manifest", "Inspect optional MCP adapter manifest"),
            CommandEffect::read_only("memory history", "Show memory revision history"),
            CommandEffect::read_only("memory list", "List memories"),
            CommandEffect::read_only("memory show", "Show memory details"),
            CommandEffect::read_only("model list", "List model registry entries"),
            CommandEffect::read_only("model status", "Inspect model registry status"),
            CommandEffect::read_only("outcome quarantine list", "List feedback quarantine rows"),
            CommandEffect::read_only("plan recipe list", "List static plan recipes"),
            CommandEffect::read_only("plan recipe show", "Show static plan recipe"),
            CommandEffect::read_only("rule list", "List procedural rules"),
            CommandEffect::read_only("rule show", "Show procedural rule"),
            CommandEffect::read_only("schema export", "Export public response schemas"),
            CommandEffect::read_only("schema list", "List response schemas"),
            CommandEffect::read_only("search", "Search memories"),
            CommandEffect::read_only("status", "Report workspace status"),
            CommandEffect::read_only("update", "Plan update without mutation"),
            CommandEffect::read_only("version", "Print version"),
            CommandEffect::read_only("workspace list", "List workspace aliases"),
            CommandEffect::read_only("workspace resolve", "Resolve workspace identity"),
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
                "index reembed",
                vec![".ee/index/embeddings/"],
                "Rebuild semantic embeddings from database records",
            ),
            CommandEffect::derived_write(
                "graph centrality-refresh",
                vec![".ee/graph/"],
                "Refresh derived graph centrality metrics",
            ),
            CommandEffect::derived_write(
                "graph feature-enrichment",
                vec![".ee/graph/"],
                "Refresh derived graph feature enrichments",
            ),
        ]
    }

    fn degraded_unavailable_commands() -> Vec<CommandEffect> {
        vec![
            CommandEffect::degraded_unavailable(
                "audit timeline",
                "audit_log_unavailable",
                "Audit timeline abstains until persisted audit log records exist",
            ),
            CommandEffect::degraded_unavailable(
                "audit show",
                "audit_log_unavailable",
                "Audit operation inspection abstains until persisted audit log records exist",
            ),
            CommandEffect::degraded_unavailable(
                "audit diff",
                "audit_log_unavailable",
                "Audit diff abstains until persisted audit log records exist",
            ),
            CommandEffect::degraded_unavailable(
                "audit verify",
                "audit_log_unavailable",
                "Audit verification abstains until persisted audit hash chains exist",
            ),
            CommandEffect::degraded_unavailable(
                "causal trace",
                "causal_evidence_unavailable",
                "Causal tracing abstains until real evidence ledgers exist",
            ),
            CommandEffect::degraded_unavailable(
                "causal compare",
                "causal_evidence_unavailable",
                "Causal comparison abstains until real evidence ledgers exist",
            ),
            CommandEffect::degraded_unavailable(
                "causal estimate",
                "causal_evidence_unavailable",
                "Causal estimation abstains until real evidence ledgers exist",
            ),
            CommandEffect::degraded_unavailable(
                "causal promote-plan",
                "causal_evidence_unavailable",
                "Causal promotion planning abstains until real evidence ledgers exist",
            ),
            CommandEffect::degraded_unavailable(
                "certificate list",
                "certificate_store_unavailable",
                "Certificate listing abstains until manifest storage exists",
            ),
            CommandEffect::degraded_unavailable(
                "certificate show",
                "certificate_store_unavailable",
                "Certificate inspection abstains until manifest storage exists",
            ),
            CommandEffect::degraded_unavailable(
                "certificate verify",
                "certificate_store_unavailable",
                "Certificate verification abstains until manifest storage exists",
            ),
            CommandEffect::degraded_unavailable(
                "claim list",
                "claim_verification_unavailable",
                "Claim listing abstains until claims manifests are parsed",
            ),
            CommandEffect::degraded_unavailable(
                "claim show",
                "claim_verification_unavailable",
                "Claim inspection abstains until claims manifests are parsed",
            ),
            CommandEffect::degraded_unavailable(
                "claim verify",
                "claim_verification_unavailable",
                "Claim verification abstains until real evidence artifacts exist",
            ),
            CommandEffect::degraded_unavailable(
                "daemon",
                "daemon_jobs_unavailable",
                "Daemon abstains until steward jobs run real handlers",
            )
            .with_runtime_contract(CommandRuntimeContract::supervised_unavailable()),
            CommandEffect::degraded_unavailable(
                "demo run",
                "demo_command_execution_unavailable",
                "Non-dry-run demo execution abstains until audit-ledger artifacts exist",
            ),
            CommandEffect::degraded_unavailable(
                "economy report",
                "economy_metrics_unavailable",
                "Economy reporting abstains until persisted workspace metrics exist",
            ),
            CommandEffect::degraded_unavailable(
                "economy score",
                "economy_metrics_unavailable",
                "Economy scoring abstains until persisted workspace metrics exist",
            ),
            CommandEffect::degraded_unavailable(
                "economy simulate",
                "economy_metrics_unavailable",
                "Economy simulation abstains until persisted workspace metrics exist",
            ),
            CommandEffect::degraded_unavailable(
                "economy prune-plan",
                "economy_metrics_unavailable",
                "Economy prune planning abstains until persisted workspace metrics exist",
            ),
            CommandEffect::degraded_unavailable(
                "diag quarantine",
                "quarantine_trust_state_unavailable",
                "Quarantine diagnostics abstain until persistent source trust state exists",
            ),
            CommandEffect::degraded_unavailable(
                "eval run",
                "eval_fixtures_unavailable",
                "Evaluation run abstains until deterministic fixture registries exist",
            ),
            CommandEffect::degraded_unavailable(
                "eval list",
                "eval_fixtures_unavailable",
                "Evaluation listing abstains until deterministic fixture registries exist",
            ),
            CommandEffect::degraded_unavailable(
                "handoff preview",
                "handoff_unavailable",
                "Handoff preview abstains until redacted continuity evidence exists",
            ),
            CommandEffect::degraded_unavailable(
                "handoff create",
                "handoff_unavailable",
                "Handoff creation abstains until redacted continuity evidence exists",
            ),
            CommandEffect::degraded_unavailable(
                "lab capture",
                "lab_replay_unavailable",
                "Lab capture abstains until stored episodes are implemented",
            ),
            CommandEffect::degraded_unavailable(
                "lab replay",
                "lab_replay_unavailable",
                "Lab replay abstains until evidence-only replay artifacts exist",
            ),
            CommandEffect::degraded_unavailable(
                "lab counterfactual",
                "lab_replay_unavailable",
                "Lab counterfactual analysis abstains until stored episodes exist",
            ),
            CommandEffect::degraded_unavailable(
                "learn agenda",
                "learning_records_unavailable",
                "Learning agenda abstains until persisted observation ledgers exist",
            ),
            CommandEffect::degraded_unavailable(
                "learn uncertainty",
                "learning_records_unavailable",
                "Learning uncertainty abstains until persisted observation ledgers exist",
            ),
            CommandEffect::degraded_unavailable(
                "learn experiment propose",
                "learning_records_unavailable",
                "Experiment proposal abstains until persisted evaluation registries exist",
            ),
            CommandEffect::degraded_unavailable(
                "learn summary",
                "learning_records_unavailable",
                "Learning summary abstains until persisted observation ledgers exist",
            ),
            CommandEffect::degraded_unavailable(
                "memory revise",
                "revision_write_unavailable",
                "Memory revision abstains until immutable revision writes are implemented",
            ),
            CommandEffect::degraded_unavailable(
                "preflight run",
                "preflight_evidence_unavailable",
                "Preflight run abstains until persisted evidence matches exist",
            ),
            CommandEffect::degraded_unavailable(
                "preflight show",
                "preflight_evidence_unavailable",
                "Preflight inspection abstains until stored preflight runs exist",
            ),
            CommandEffect::degraded_unavailable(
                "preflight close",
                "preflight_evidence_unavailable",
                "Preflight close abstains until stored preflight runs exist",
            ),
            CommandEffect::degraded_unavailable(
                "plan goal",
                "plan_decisioning_unavailable",
                "Goal planning abstains until command recipes are mechanical or skill-facing",
            ),
            CommandEffect::degraded_unavailable(
                "plan explain",
                "plan_decisioning_unavailable",
                "Plan explanation abstains until command recipes are mechanical or skill-facing",
            ),
            CommandEffect::degraded_unavailable(
                "procedure propose",
                "procedure_store_unavailable",
                "Procedure proposal abstains until procedure storage exists",
            ),
            CommandEffect::degraded_unavailable(
                "procedure show",
                "procedure_store_unavailable",
                "Procedure inspection abstains until procedure storage exists",
            ),
            CommandEffect::degraded_unavailable(
                "procedure list",
                "procedure_store_unavailable",
                "Procedure listing abstains until procedure storage exists",
            ),
            CommandEffect::degraded_unavailable(
                "procedure export",
                "procedure_store_unavailable",
                "Procedure export abstains until procedure storage exists",
            ),
            CommandEffect::degraded_unavailable(
                "procedure promote",
                "procedure_store_unavailable",
                "Procedure promotion abstains until procedure storage exists",
            ),
            CommandEffect::degraded_unavailable(
                "procedure verify",
                "procedure_store_unavailable",
                "Procedure verification abstains until procedure storage exists",
            ),
            CommandEffect::degraded_unavailable(
                "procedure drift",
                "procedure_store_unavailable",
                "Procedure drift checking abstains until procedure storage exists",
            ),
            CommandEffect::degraded_unavailable(
                "recorder start",
                "recorder_store_unavailable",
                "Recorder start abstains until event storage exists",
            ),
            CommandEffect::degraded_unavailable(
                "recorder event",
                "recorder_store_unavailable",
                "Recorder event abstains until event storage exists",
            ),
            CommandEffect::degraded_unavailable(
                "recorder finish",
                "recorder_store_unavailable",
                "Recorder finish abstains until event storage exists",
            ),
            CommandEffect::degraded_unavailable(
                "recorder tail",
                "recorder_tail_unavailable",
                "Recorder tail abstains until persisted event tailing exists",
            ),
            CommandEffect::degraded_unavailable(
                "recorder import",
                "recorder_tail_unavailable",
                "Recorder import planning abstains until persisted event tailing exists",
            ),
            CommandEffect::degraded_unavailable(
                "rehearse plan",
                "rehearsal_unavailable",
                "Rehearsal planning abstains until real isolated artifacts exist",
            ),
            CommandEffect::degraded_unavailable(
                "rehearse run",
                "rehearsal_unavailable",
                "Rehearsal execution abstains until real isolated artifacts exist",
            ),
            CommandEffect::degraded_unavailable(
                "rehearse inspect",
                "rehearsal_unavailable",
                "Rehearsal inspection abstains until real isolated artifacts exist",
            ),
            CommandEffect::degraded_unavailable(
                "rehearse promote-plan",
                "rehearsal_unavailable",
                "Rehearsal promotion planning abstains until real isolated artifacts exist",
            ),
            CommandEffect::degraded_unavailable(
                "review session",
                "review_evidence_unavailable",
                "Session review abstains until CASS evidence extraction exists",
            ),
            CommandEffect::degraded_unavailable(
                "situation classify",
                "situation_decisioning_unavailable",
                "Situation classification abstains until stored evidence features exist",
            ),
            CommandEffect::degraded_unavailable(
                "situation compare",
                "situation_decisioning_unavailable",
                "Situation comparison abstains until stored evidence features exist",
            ),
            CommandEffect::degraded_unavailable(
                "situation link",
                "situation_decisioning_unavailable",
                "Situation linking abstains until stored evidence features exist",
            ),
            CommandEffect::degraded_unavailable(
                "situation show",
                "situation_decisioning_unavailable",
                "Situation inspection abstains until stored evidence features exist",
            ),
            CommandEffect::degraded_unavailable(
                "situation explain",
                "situation_decisioning_unavailable",
                "Situation explanation abstains until stored evidence features exist",
            ),
            CommandEffect::degraded_unavailable(
                "support bundle",
                "support_bundle_unavailable",
                "Support bundle creation abstains until redacted archives are materialized",
            ),
            CommandEffect::degraded_unavailable(
                "support inspect",
                "support_bundle_unavailable",
                "Support bundle inspection abstains until manifests are verified",
            ),
            CommandEffect::degraded_unavailable(
                "tripwire list",
                "tripwire_store_unavailable",
                "Tripwire listing abstains until persisted tripwire storage exists",
            ),
            CommandEffect::degraded_unavailable(
                "tripwire check",
                "tripwire_store_unavailable",
                "Tripwire checks abstain until explicit event payload evaluation exists",
            ),
        ]
    }

    fn append_only_write_commands() -> Vec<CommandEffect> {
        vec![
            CommandEffect::append_only_write(
                "artifact register",
                vec!["artifacts", "artifact_links", "audit_log"],
                "content hash",
                "Register artifact metadata keyed by content hash",
            ),
            CommandEffect::append_only_write(
                "import cass",
                vec!["memories", "audit_log"],
                "source hash",
                "Import from CASS sessions",
            ),
            CommandEffect::append_only_write(
                "import jsonl",
                vec!["memories", "audit_log"],
                "source hash",
                "Import from JSONL export",
            ),
            CommandEffect::append_only_write(
                "import eidetic-legacy",
                vec!["memories", "audit_log"],
                "source hash",
                "Import from legacy Eidetic export",
            ),
            CommandEffect::append_only_write(
                "pack",
                vec!["context_packs", "pack_items", "audit_log"],
                "pack hash",
                "Persist a context pack keyed by deterministic pack hash",
            ),
        ]
    }

    fn durable_write_commands() -> Vec<CommandEffect> {
        vec![
            CommandEffect::durable_write(
                "curate accept",
                vec!["curation_candidates", "procedural_rules", "audit_log"],
                "Accept a curation candidate",
            ),
            CommandEffect::durable_write(
                "curate apply",
                vec!["curation_candidates", "memories", "audit_log"],
                "Apply a curation candidate",
            ),
            CommandEffect::durable_write(
                "curate disposition",
                vec!["curation_candidates", "audit_log"],
                "Record curation disposition",
            ),
            CommandEffect::durable_write(
                "curate merge",
                vec!["curation_candidates", "memories", "audit_log"],
                "Merge curation candidates",
            ),
            CommandEffect::durable_write(
                "curate reject",
                vec!["curation_candidates", "audit_log"],
                "Reject a curation candidate",
            ),
            CommandEffect::durable_write(
                "curate snooze",
                vec!["curation_candidates", "audit_log"],
                "Snooze a curation candidate",
            ),
            CommandEffect::durable_write(
                "learn close",
                vec!["learning_experiments", "audit_log"],
                "Close a learning experiment",
            ),
            CommandEffect::durable_write(
                "learn experiment run",
                vec!["learning_experiments", "evaluation_reports", "audit_log"],
                "Record a learning experiment run",
            ),
            CommandEffect::durable_write(
                "learn observe",
                vec!["learning_observations", "audit_log"],
                "Record a learning observation",
            ),
            CommandEffect::durable_write(
                "outcome",
                vec!["feedback_events", "audit_log"],
                "Record observed outcome feedback",
            ),
            CommandEffect::durable_write(
                "outcome quarantine release",
                vec!["feedback_quarantine", "audit_log"],
                "Release feedback from quarantine",
            ),
            CommandEffect::durable_write(
                "remember",
                vec!["memories", "memory_tags", "audit_log"],
                "Store a new memory",
            ),
            CommandEffect::durable_write(
                "rule add",
                vec![
                    "procedural_rules",
                    "rule_source_memories",
                    "rule_tags",
                    "audit_log",
                    "search_index_jobs",
                ],
                "Store a procedural rule",
            ),
            CommandEffect::durable_write(
                "rule protect",
                vec!["procedural_rules", "audit_log"],
                "Protect or unprotect a procedural rule",
            ),
        ]
    }

    fn config_write_commands() -> Vec<CommandEffect> {
        vec![
            CommandEffect::config_write(
                "init",
                vec![".ee/", "ee.toml"],
                "workspace root",
                "Initialize workspace-local ee configuration and storage",
            ),
            CommandEffect::config_write(
                "workspace alias",
                vec![".ee/workspaces.toml"],
                "alias name and workspace root",
                "Create or update a workspace alias",
            ),
        ]
    }

    fn workspace_file_write_commands() -> Vec<CommandEffect> {
        vec![
            CommandEffect::workspace_file_write(
                "backup create",
                vec![".ee/backups/<backup-id>/"],
                "Create redacted backup artifacts in the workspace",
            ),
            CommandEffect::workspace_file_write(
                "backup restore",
                vec!["<side-path>/"],
                "Restore backup contents into an explicit side path",
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
        )?;
        ensure(
            EffectClass::WorkspaceFileWrite.as_str(),
            "workspace_file_write",
            "workspace_file_write",
        )
    }

    #[test]
    fn side_effect_class_strings_match_matrix_vocabulary() -> TestResult {
        ensure(
            SideEffectClass::ReadOnly.as_str(),
            "class=read_only",
            "read_only",
        )?;
        ensure(
            SideEffectClass::AppendOnly.as_str(),
            "class=append_only",
            "append_only",
        )?;
        ensure(
            SideEffectClass::ReadOnlyNow.as_str(),
            "class=read_only_now",
            "read_only_now",
        )?;
        ensure(
            SideEffectClass::ReportOnly.as_str(),
            "class=report_only",
            "report_only",
        )?;
        ensure(
            SideEffectClass::ReadOnlyOrUnavailable.as_str(),
            "class=read_only_or_unavailable",
            "read_only_or_unavailable",
        )?;
        ensure(
            SideEffectClass::AuditedMutation.as_str(),
            "class=audited_mutation",
            "audited_mutation",
        )?;
        ensure(
            SideEffectClass::DerivedAssetRebuild.as_str(),
            "class=derived_asset_rebuild",
            "derived_asset_rebuild",
        )?;
        ensure(
            SideEffectClass::SidePathArtifact.as_str(),
            "class=side_path_artifact",
            "side_path_artifact",
        )?;
        ensure(
            SideEffectClass::SupervisedJobs.as_str(),
            "class=supervised_jobs",
            "supervised_jobs",
        )?;
        ensure(SideEffectClass::Mixed.as_str(), "class=mixed", "mixed")?;
        ensure(
            SideEffectClass::DegradedUnavailable.as_str(),
            "class=degraded_unavailable",
            "degraded_unavailable",
        )?;
        ensure(
            SideEffectClass::ReportOnlyOrAppend.as_str(),
            "class=report_only_or_append",
            "report_only_or_append",
        )?;
        ensure(
            SideEffectClass::ReportOnlyOrAuditedMutation.as_str(),
            "class=report_only_or_audited_mutation",
            "report_only_or_audited_mutation",
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
        )?;
        ensure(
            EffectClass::WorkspaceFileWrite.is_mutating(),
            true,
            "workspace_file_write",
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
    fn runtime_class_strings_are_stable() -> TestResult {
        ensure(RuntimeClass::Immediate.as_str(), "immediate", "immediate")?;
        ensure(RuntimeClass::Bounded.as_str(), "bounded", "bounded")?;
        ensure(
            RuntimeClass::LongRunning.as_str(),
            "long_running",
            "long_running",
        )?;
        ensure(
            RuntimeClass::MultiStage.as_str(),
            "multi_stage",
            "multi_stage",
        )?;
        ensure(
            RuntimeClass::Supervised.as_str(),
            "supervised",
            "supervised",
        )
    }

    #[test]
    fn runtime_budget_math_is_deterministic() -> TestResult {
        let long = CommandRuntimeContract::long_running_derived();
        ensure(
            long.requires_budget(),
            true,
            "long-running work requires budget",
        )?;
        ensure(
            long.effective_budget_ms(None),
            Ok(Some(300_000)),
            "default long-running budget",
        )?;
        ensure(
            long.effective_budget_ms(Some(42)),
            Ok(Some(42)),
            "explicit budget overrides default",
        )?;
        ensure(
            long.effective_budget_ms(Some(0)),
            Err("runtime budget must be greater than zero"),
            "zero budget rejected",
        )?;

        let immediate = CommandRuntimeContract::immediate();
        ensure(
            immediate.requires_budget(),
            false,
            "immediate work does not require budget",
        )?;
        ensure(
            immediate.effective_budget_ms(None),
            Ok(None),
            "immediate work is unbounded by default",
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
        ensure(
            effect.runtime_contract.runtime_class,
            RuntimeClass::Bounded,
            "read-only work is bounded",
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
        )?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::AuditedMutation,
            "durable write has audited contract",
        )?;
        ensure(
            effect.mutation_contract.audit_surface,
            Some("audit_log"),
            "durable write names audit surface",
        )?;
        ensure(
            effect.runtime_contract.runtime_class,
            RuntimeClass::MultiStage,
            "durable write has multi-stage runtime",
        )
    }

    #[test]
    fn command_effect_append_only_write_uses_idempotency_key() -> TestResult {
        let effect = CommandEffect::append_only_write(
            "artifact register",
            vec!["artifacts", "audit_log"],
            "content hash",
            "Register artifact",
        );
        ensure(
            effect.default_effect,
            EffectClass::DurableMemoryWrite,
            "append-only writes durable records",
        )?;
        ensure(
            effect.idempotency,
            IdempotencyClass::Idempotent,
            "append-only retries are idempotent",
        )?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::AppendOnly,
            "append-only class",
        )?;
        ensure(
            effect.mutation_contract.idempotency_key,
            Some("content hash"),
            "idempotency key",
        )?;
        ensure(
            effect.runtime_contract.runtime_class,
            RuntimeClass::MultiStage,
            "append-only write has multi-stage runtime",
        )?;
        ensure(effect.requires_audit, true, "append-only requires audit")
    }

    #[test]
    fn command_effect_degraded_unavailable_is_read_only_with_code() -> TestResult {
        let effect = CommandEffect::degraded_unavailable(
            "support bundle",
            "support_bundle_unavailable",
            "Support bundles abstain until real artifacts exist",
        );
        ensure(
            effect.default_effect,
            EffectClass::ReadOnly,
            "degraded unavailable is read-only",
        )?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::DegradedUnavailable,
            "degraded side-effect class",
        )?;
        ensure(
            effect.write_surfaces.is_empty(),
            true,
            "degraded command has no write surfaces",
        )?;
        ensure(
            effect.mutation_contract.degraded_code,
            Some("support_bundle_unavailable"),
            "degraded code is explicit",
        )?;
        ensure(
            effect.runtime_contract.runtime_class,
            RuntimeClass::Immediate,
            "degraded unavailable returns immediately",
        )?;
        ensure(
            effect.requires_audit,
            false,
            "degraded command does not audit",
        )
    }

    #[test]
    fn command_effect_workspace_file_write_uses_side_path_no_delete_contract() -> TestResult {
        let effect = CommandEffect::workspace_file_write(
            "backup restore",
            vec!["<side-path>/"],
            "Restore backup into an explicit side path",
        );

        ensure(
            effect.default_effect,
            EffectClass::WorkspaceFileWrite,
            "workspace write effect",
        )?;
        ensure(
            effect.dry_run_effect,
            Some(EffectClass::ReadOnly),
            "dry-run is read-only",
        )?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::SidePathArtifact,
            "workspace writes are side-path artifacts",
        )?;
        ensure(
            effect.requires_audit,
            true,
            "side-path artifacts require manifest audit",
        )?;
        ensure(
            effect.runtime_contract.runtime_class,
            RuntimeClass::MultiStage,
            "side-path artifacts are multi-stage",
        )?;
        ensure(
            effect
                .mutation_contract
                .no_overwrite_behavior
                .is_some_and(|policy| {
                    policy.contains("no-overwrite") && policy.contains("no-delete")
                }),
            true,
            "side-path policy names no-overwrite and no-delete",
        )?;
        ensure(
            effect
                .mutation_contract
                .recovery_behavior
                .contains("never deleted by ee"),
            true,
            "side-path recovery never deletes partial output",
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
        )?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::DerivedAssetRebuild,
            "derived write uses derived rebuild contract",
        )?;
        ensure(
            effect.mutation_contract.declares_no_source_mutation(),
            true,
            "derived write leaves source DB unchanged",
        )?;
        ensure(
            effect.runtime_contract.runtime_class,
            RuntimeClass::LongRunning,
            "derived writes are long-running",
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
        )?;

        let backup = manifest.get("backup create");
        ensure(backup.is_some(), true, "backup create exists")?;
        ensure(
            backup.map(|e| e.default_effect),
            Some(EffectClass::WorkspaceFileWrite),
            "backup create writes workspace files",
        )
    }

    #[test]
    fn manifest_distinguishes_append_only_imports_from_audited_mutations() -> TestResult {
        let manifest = EffectManifest::build();

        for command in [
            "artifact register",
            "import cass",
            "import jsonl",
            "import eidetic-legacy",
        ] {
            let effect = manifest
                .get(command)
                .ok_or_else(|| format!("{command} not found"))?;
            ensure(
                effect.mutation_contract.side_effect_class,
                SideEffectClass::AppendOnly,
                &format!("{command} is append-only"),
            )?;
            ensure(
                effect.idempotency,
                IdempotencyClass::Idempotent,
                &format!("{command} retries by idempotency key"),
            )?;
        }

        let remember = manifest
            .get("remember")
            .ok_or_else(|| "remember not found".to_owned())?;
        ensure(
            remember.mutation_contract.side_effect_class,
            SideEffectClass::AuditedMutation,
            "remember remains an audited mutation",
        )
    }

    #[test]
    fn manifest_tracks_unavailable_commands_as_non_mutating_degraded_paths() -> TestResult {
        let manifest = EffectManifest::build();

        for (command, code) in [
            ("audit timeline", "audit_log_unavailable"),
            ("demo run", "demo_command_execution_unavailable"),
            ("handoff create", "handoff_unavailable"),
            ("procedure export", "procedure_store_unavailable"),
            ("rehearse run", "rehearsal_unavailable"),
            ("support bundle", "support_bundle_unavailable"),
        ] {
            let effect = manifest
                .get(command)
                .ok_or_else(|| format!("{command} not found"))?;
            ensure(
                effect.default_effect,
                EffectClass::ReadOnly,
                &format!("{command} is read-only while unavailable"),
            )?;
            ensure(
                effect.mutation_contract.side_effect_class,
                SideEffectClass::DegradedUnavailable,
                &format!("{command} uses degraded class"),
            )?;
            ensure(
                effect.write_surfaces.is_empty(),
                true,
                &format!("{command} has no write surfaces while unavailable"),
            )?;
            ensure(
                effect.mutation_contract.degraded_code,
                Some(code),
                &format!("{command} degraded code"),
            )?;
        }

        Ok(())
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
    fn all_mutating_commands_have_s43e_contract_metadata() -> TestResult {
        let manifest = EffectManifest::build();

        for effect in manifest.mutating_commands() {
            let contract = &effect.mutation_contract;
            if contract.transaction_scope.is_none() {
                return Err(format!(
                    "Mutating command '{}' has no transaction scope",
                    effect.command_path
                ));
            }
            if contract.idempotency_key.is_none() {
                return Err(format!(
                    "Mutating command '{}' has no idempotency key",
                    effect.command_path
                ));
            }
            if contract.dry_run_behavior.is_none() {
                return Err(format!(
                    "Mutating command '{}' has no dry-run behavior",
                    effect.command_path
                ));
            }
            if contract.recovery_behavior.is_empty()
                || contract.db_generation_effect.is_empty()
                || contract.index_generation_effect.is_empty()
            {
                return Err(format!(
                    "Mutating command '{}' has incomplete recovery/generation effects",
                    effect.command_path
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn side_path_artifact_commands_name_no_overwrite_behavior() -> TestResult {
        let manifest = EffectManifest::build();

        for effect in manifest.mutating_commands() {
            let contract = &effect.mutation_contract;
            if contract.side_effect_class.requires_no_overwrite_contract() {
                let Some(policy) = contract.no_overwrite_behavior else {
                    return Err(format!(
                        "Side-path command '{}' has no no-overwrite behavior",
                        effect.command_path
                    ));
                };
                if !policy.contains("no-overwrite") || !policy.contains("no-delete") {
                    return Err(format!(
                        "Side-path command '{}' must name no-overwrite and no-delete behavior",
                        effect.command_path
                    ));
                }
                if !contract.recovery_behavior.contains("never deleted by ee") {
                    return Err(format!(
                        "Side-path command '{}' must never delete partial output during recovery",
                        effect.command_path
                    ));
                }
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
        )?;
        ensure(
            EffectClass::DurableMemoryWrite < EffectClass::WorkspaceFileWrite,
            true,
            "durable_memory_write < workspace_file_write",
        )
    }
}
