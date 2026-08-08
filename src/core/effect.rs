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
    pub const fn supervised_jobs(
        idempotency_key: &'static str,
        recovery_behavior: &'static str,
    ) -> Self {
        Self {
            side_effect_class: SideEffectClass::SupervisedJobs,
            transaction_scope: Some("supervised steward job ledger"),
            idempotency_key: Some(idempotency_key),
            audit_surface: Some("audit_log"),
            db_generation_effect: "advances only when the configured job applies durable changes",
            index_generation_effect: "unchanged unless the steward job explicitly processes index work",
            dry_run_behavior: Some(
                "runs handler planning and reports candidate changes without committing job mutations",
            ),
            recovery_behavior,
            no_overwrite_behavior: None,
            degraded_code: None,
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
    pub const fn supervised_jobs() -> Self {
        Self {
            runtime_class: RuntimeClass::Supervised,
            default_budget_ms: Some(300_000),
            cancellation_points: &[
                "before_start",
                "before_job_schedule",
                "before_handler",
                "handler_outcome",
            ],
            partial_progress_policy: "supervised jobs report skipped, failed, cancelled, or applied handler work in the runner result",
            outcome_mapping: "success, skipped, failed, cancelled, budget_exhausted, or supervised_child_failed",
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
    /// Whether the command should run through a read-side snapshot lease.
    pub requires_read_snapshot: bool,
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
            requires_read_snapshot: false,
            requires_audit: false,
            description,
        }
    }

    /// Create a DB-backed read-only effect entry that must use a read snapshot.
    #[must_use]
    pub fn read_only_db(command_path: &'static str, description: &'static str) -> Self {
        Self::read_only(command_path, description).with_read_snapshot()
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
            requires_read_snapshot: false,
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
            requires_read_snapshot: false,
            requires_audit: true,
            description,
        }
    }

    /// Create a durable write that also declares companion workspace-file surfaces.
    #[must_use]
    pub fn durable_write_with_workspace_files(
        command_path: &'static str,
        db_tables: Vec<&'static str>,
        workspace_files: Vec<&'static str>,
        description: &'static str,
    ) -> Self {
        let mut effect = Self::durable_write(command_path, db_tables, description);
        effect.write_surfaces.workspace_files = workspace_files;
        effect
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
            requires_read_snapshot: false,
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
            requires_read_snapshot: false,
            requires_audit: true,
            description,
        }
    }

    /// Create an audited external-I/O command entry.
    #[must_use]
    pub fn external_io_write(
        command_path: &'static str,
        db_tables: Vec<&'static str>,
        workspace_files: Vec<&'static str>,
        idempotency_key: &'static str,
        description: &'static str,
    ) -> Self {
        Self {
            command_path,
            default_effect: EffectClass::ExternalIo,
            dry_run_effect: Some(EffectClass::ReadOnly),
            idempotency: IdempotencyClass::NonIdempotent,
            write_surfaces: WriteSurfaces {
                db_tables,
                derived_paths: Vec::new(),
                workspace_files,
            },
            mutation_contract: CommandMutationContract {
                side_effect_class: SideEffectClass::AuditedMutation,
                transaction_scope: Some("one audit ledger append per executed command step"),
                idempotency_key: Some(idempotency_key),
                audit_surface: Some("audit_log"),
                db_generation_effect: "advances audit log for each executed step; unchanged for dry-run or rejected unsafe steps",
                index_generation_effect: "none",
                dry_run_behavior: Some(
                    "parses the manifest and reports planned steps without executing commands or writing evidence",
                ),
                recovery_behavior: "failed steps keep evidence and audit rows; later steps are skipped unless the caller opts into continuing",
                no_overwrite_behavior: Some(
                    "no-overwrite/no-delete: evidence paths use fresh run IDs and ee never deletes partial demo evidence",
                ),
                degraded_code: None,
            },
            runtime_contract: CommandRuntimeContract {
                runtime_class: RuntimeClass::MultiStage,
                default_budget_ms: Some(120_000),
                cancellation_points: &[
                    "before_start",
                    "before_step_execute",
                    "after_step_evidence",
                    "before_audit_commit",
                ],
                partial_progress_policy: "each executed step writes evidence plus one audit row; remaining steps are skipped after the first failure unless explicitly continued",
                outcome_mapping: "success, policy_denied, usage_error, storage_error, or step failure with persisted evidence",
            },
            requires_read_snapshot: false,
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
            requires_read_snapshot: false,
            requires_audit: true,
            description,
        }
    }

    /// Create a config-file-write entry that does not touch the ee database.
    #[must_use]
    pub fn config_file_write(
        command_path: &'static str,
        workspace_files: Vec<&'static str>,
        idempotency_key: &'static str,
        description: &'static str,
    ) -> Self {
        Self {
            command_path,
            default_effect: EffectClass::ConfigWrite,
            dry_run_effect: Some(EffectClass::ReadOnly),
            idempotency: IdempotencyClass::DryRunAvailable,
            write_surfaces: WriteSurfaces {
                db_tables: Vec::new(),
                derived_paths: Vec::new(),
                workspace_files,
            },
            mutation_contract: CommandMutationContract {
                side_effect_class: SideEffectClass::SidePathArtifact,
                transaction_scope: Some("workspace config file update"),
                idempotency_key: Some(idempotency_key),
                audit_surface: None,
                db_generation_effect: "source DB generation unchanged",
                index_generation_effect: "none",
                dry_run_behavior: Some(
                    "--dry-run validates and previews config changes; no files are written",
                ),
                recovery_behavior: "partial config-file output is reported as failed, never deleted by ee, and not treated as committed configuration",
                no_overwrite_behavior: Some(
                    "no-overwrite contract preserves unrelated config keys where possible; no-delete: ee never deletes the config file",
                ),
                degraded_code: None,
            },
            runtime_contract: CommandRuntimeContract::side_path_artifact(),
            requires_read_snapshot: false,
            requires_audit: false,
            description,
        }
    }

    /// Create a harness settings-file write entry that does not touch the ee database.
    #[must_use]
    pub fn harness_hook_settings_write(
        command_path: &'static str,
        workspace_files: Vec<&'static str>,
        idempotency_key: &'static str,
        description: &'static str,
    ) -> Self {
        Self {
            command_path,
            default_effect: EffectClass::ConfigWrite,
            dry_run_effect: Some(EffectClass::ReadOnly),
            idempotency: IdempotencyClass::DryRunAvailable,
            write_surfaces: WriteSurfaces {
                db_tables: Vec::new(),
                derived_paths: Vec::new(),
                workspace_files,
            },
            mutation_contract: CommandMutationContract {
                side_effect_class: SideEffectClass::SidePathArtifact,
                transaction_scope: Some("agent harness settings file update"),
                idempotency_key: Some(idempotency_key),
                audit_surface: None,
                db_generation_effect: "source DB generation unchanged",
                index_generation_effect: "none",
                dry_run_behavior: Some(
                    "--print previews generated harness hooks without writing settings files",
                ),
                recovery_behavior: "failed harness settings writes return storage_error, partial output is never deleted by ee, and backups are left in place for manual restore and --undo",
                no_overwrite_behavior: Some(
                    "no-overwrite/no-delete: preserves unmanaged settings entries; install writes a deterministic backup before changing managed hook entries; ee never deletes harness settings files",
                ),
                degraded_code: None,
            },
            runtime_contract: CommandRuntimeContract::side_path_artifact(),
            requires_read_snapshot: false,
            requires_audit: false,
            description,
        }
    }

    /// Create a certificate key-file write entry that does not touch the ee database.
    #[must_use]
    pub fn certificate_key_file_write(
        command_path: &'static str,
        workspace_files: Vec<&'static str>,
        idempotency_key: &'static str,
        description: &'static str,
    ) -> Self {
        Self {
            command_path,
            default_effect: EffectClass::ConfigWrite,
            dry_run_effect: Some(EffectClass::ReadOnly),
            idempotency: IdempotencyClass::DryRunAvailable,
            write_surfaces: WriteSurfaces {
                db_tables: Vec::new(),
                derived_paths: Vec::new(),
                workspace_files,
            },
            mutation_contract: CommandMutationContract {
                side_effect_class: SideEffectClass::SidePathArtifact,
                transaction_scope: Some("key file create-or-explicit-force overwrite"),
                idempotency_key: Some(idempotency_key),
                audit_surface: None,
                db_generation_effect: "source DB generation unchanged",
                index_generation_effect: "none",
                dry_run_behavior: Some(
                    "--show reads existing key material only; no files are written",
                ),
                recovery_behavior: "partial key-file output is reported as failed, never deleted by ee, and not treated as valid key material",
                no_overwrite_behavior: Some(
                    "no-overwrite by default; --force is an explicit overwrite, and no-delete: ee never deletes key material",
                ),
                degraded_code: None,
            },
            runtime_contract: CommandRuntimeContract::side_path_artifact(),
            requires_read_snapshot: false,
            requires_audit: false,
            description,
        }
    }

    /// Create a workspace state-file write entry.
    #[must_use]
    pub fn workspace_state_write(
        command_path: &'static str,
        workspace_files: Vec<&'static str>,
        idempotency_key: &'static str,
        description: &'static str,
    ) -> Self {
        Self {
            command_path,
            default_effect: EffectClass::WorkspaceFileWrite,
            dry_run_effect: Some(EffectClass::ReadOnly),
            idempotency: IdempotencyClass::DryRunAvailable,
            write_surfaces: WriteSurfaces {
                db_tables: Vec::new(),
                derived_paths: Vec::new(),
                workspace_files,
            },
            mutation_contract: CommandMutationContract {
                side_effect_class: SideEffectClass::AuditedMutation,
                transaction_scope: Some("workspace-local state file update"),
                idempotency_key: Some(idempotency_key),
                audit_surface: Some("workspace state file"),
                db_generation_effect: "source DB generation unchanged",
                index_generation_effect: "none",
                dry_run_behavior: Some(
                    "--dry-run previews the state transition without writing workspace files",
                ),
                recovery_behavior: "failed workspace state writes return storage_error; rerun from the last readable state file",
                no_overwrite_behavior: None,
                degraded_code: None,
            },
            runtime_contract: CommandRuntimeContract::transactional(),
            requires_read_snapshot: false,
            requires_audit: true,
            description,
        }
    }

    /// Create a durable state write backed by a non-audit-log evidence spine.
    #[must_use]
    pub fn durable_state_write(
        command_path: &'static str,
        db_tables: Vec<&'static str>,
        idempotency_key: &'static str,
        audit_surface: &'static str,
        description: &'static str,
    ) -> Self {
        Self {
            command_path,
            default_effect: EffectClass::DurableMemoryWrite,
            dry_run_effect: Some(EffectClass::ReadOnly),
            idempotency: IdempotencyClass::DryRunAvailable,
            write_surfaces: WriteSurfaces {
                db_tables,
                derived_paths: Vec::new(),
                workspace_files: Vec::new(),
            },
            mutation_contract: CommandMutationContract {
                side_effect_class: SideEffectClass::AuditedMutation,
                transaction_scope: Some("single DB transaction across state/evidence rows"),
                idempotency_key: Some(idempotency_key),
                audit_surface: Some(audit_surface),
                db_generation_effect: "advances on commit; unchanged for dry-run or rollback",
                index_generation_effect: "none unless a downstream steward job is queued",
                dry_run_behavior: Some("validates and renders the report without writing DB rows"),
                recovery_behavior: "transaction rollback leaves no partial durable records",
                no_overwrite_behavior: None,
                degraded_code: None,
            },
            runtime_contract: CommandRuntimeContract::transactional(),
            requires_read_snapshot: false,
            requires_audit: true,
            description,
        }
    }

    /// Create the schema-migration effect entry.
    #[must_use]
    pub fn schema_migration_run() -> Self {
        Self {
            command_path: "migrate run",
            default_effect: EffectClass::DurableMemoryWrite,
            dry_run_effect: Some(EffectClass::ReadOnly),
            idempotency: IdempotencyClass::Idempotent,
            write_surfaces: WriteSurfaces {
                db_tables: vec![
                    "ee_schema_migrations",
                    "memories",
                    "search_index_jobs",
                    "audit_log",
                ],
                derived_paths: vec![".ee/index/"],
                workspace_files: Vec::new(),
            },
            mutation_contract: CommandMutationContract {
                side_effect_class: SideEffectClass::AuditedMutation,
                transaction_scope: Some(
                    "ordered schema migrations plus post-migration backfill/index audit",
                ),
                idempotency_key: Some("database path plus compiled migration checksums"),
                audit_surface: Some("ee_schema_migrations and audit_log"),
                db_generation_effect: "advances schema migration state and any migration-owned rows on commit",
                index_generation_effect: "post-migration index rebuild may refresh derived index generation",
                dry_run_behavior: Some(
                    "--dry-run reports pending migrations and backfill/index plans without mutation",
                ),
                recovery_behavior: "migration transaction rollback leaves unapplied versions pending for retry",
                no_overwrite_behavior: None,
                degraded_code: None,
            },
            runtime_contract: CommandRuntimeContract::transactional(),
            requires_read_snapshot: false,
            requires_audit: true,
            description: "Apply pending schema migrations and post-migration repair work",
        }
    }

    /// Create the shard fan-out migration effect entry.
    #[must_use]
    pub fn shard_fanout_migration() -> Self {
        Self {
            command_path: "migrate shard-fanout",
            default_effect: EffectClass::WorkspaceFileWrite,
            dry_run_effect: Some(EffectClass::ReadOnly),
            idempotency: IdempotencyClass::DryRunAvailable,
            write_surfaces: WriteSurfaces {
                db_tables: vec!["shard catalog", "workspace shard databases", "audit_log"],
                derived_paths: Vec::new(),
                workspace_files: vec![
                    "<shards-dir>/catalog.db",
                    "<shards-dir>/<workspace-shard>.db",
                    "<source-db>.pre-shard-fanout",
                ],
            },
            mutation_contract: CommandMutationContract {
                side_effect_class: SideEffectClass::AuditedMutation,
                transaction_scope: Some(
                    "preserve source database, copy workspace rows, then write shard catalog",
                ),
                idempotency_key: Some("source database hash plus shard fan-out plan"),
                audit_surface: Some("shard migration audit rows"),
                db_generation_effect: "source DB generation is preserved; shard catalog and workspace shard DBs advance",
                index_generation_effect: "none",
                dry_run_behavior: Some(
                    "--dry-run reports the shard plan and blockers without writing shard files",
                ),
                recovery_behavior: "preserved source copy and shard hashes let reruns detect already-applied work",
                no_overwrite_behavior: Some(
                    "no-overwrite/no-delete: source database is preserved before copy and existing incompatible shard/catalog hashes block apply",
                ),
                degraded_code: None,
            },
            runtime_contract: CommandRuntimeContract::side_path_artifact(),
            requires_read_snapshot: false,
            requires_audit: true,
            description: "Migrate a monolithic workspace database into per-workspace shard files",
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
            requires_read_snapshot: false,
            requires_audit: false,
            description,
        }
    }

    /// Create a supervised maintenance-job effect entry.
    #[must_use]
    pub fn supervised_job(
        command_path: &'static str,
        db_tables: Vec<&'static str>,
        description: &'static str,
    ) -> Self {
        Self {
            command_path,
            default_effect: EffectClass::DurableMemoryWrite,
            dry_run_effect: Some(EffectClass::ReadOnly),
            idempotency: IdempotencyClass::DryRunAvailable,
            write_surfaces: WriteSurfaces {
                db_tables,
                derived_paths: Vec::new(),
                workspace_files: Vec::new(),
            },
            mutation_contract: CommandMutationContract::supervised_jobs(
                "workspace id plus job type plus unapplied feedback set",
                "job result reports failed/skipped/cancelled work; durable changes are limited to handler-owned audited updates",
            ),
            runtime_contract: CommandRuntimeContract::supervised_jobs(),
            requires_read_snapshot: false,
            requires_audit: true,
            description,
        }
    }

    /// Override the default runtime contract for a specific command path.
    #[must_use]
    pub const fn with_runtime_contract(mut self, runtime_contract: CommandRuntimeContract) -> Self {
        self.runtime_contract = runtime_contract;
        self
    }

    /// Mark this command as requiring a read-side snapshot lease.
    #[must_use]
    pub const fn with_read_snapshot(mut self) -> Self {
        self.requires_read_snapshot = true;
        self
    }

    /// `true` if this read-only command should acquire a read-side snapshot.
    #[must_use]
    pub const fn read_snapshot(&self) -> bool {
        self.requires_read_snapshot
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
    ///
    /// Panics if any `command_path` appears in more than one of the nine
    /// category vectors. The contract is "a command must be classified
    /// in exactly one effect class"; a duplicate would mean the second
    /// category silently overwrites the first via `HashMap::insert`,
    /// and a command's declared effect would depend on the order
    /// `build()` walks the category functions. The mid-task safety
    /// classifier (`is_safe_mid_task`), the doctor capability surface,
    /// and the audit log all key off this manifest, so a silent
    /// miscategorization here would route a mutating command through a
    /// safe-read code path. The duplicate check turns that drift into a
    /// loud, immediate failure at startup.
    #[must_use]
    pub fn build() -> Self {
        let mut entries = HashMap::new();

        // Read-only commands
        for entry in Self::read_only_commands() {
            Self::insert_unique(&mut entries, entry);
        }

        // Explicitly unavailable commands that must not mutate.
        for entry in Self::degraded_unavailable_commands() {
            Self::insert_unique(&mut entries, entry);
        }

        // Derived artifact write commands
        for entry in Self::derived_write_commands() {
            Self::insert_unique(&mut entries, entry);
        }

        // Audited external command execution surfaces
        for entry in Self::external_io_write_commands() {
            Self::insert_unique(&mut entries, entry);
        }

        // Supervised steward jobs
        for entry in Self::supervised_job_commands() {
            Self::insert_unique(&mut entries, entry);
        }

        // Append-only write commands
        for entry in Self::append_only_write_commands() {
            Self::insert_unique(&mut entries, entry);
        }

        // Durable write commands
        for entry in Self::durable_write_commands() {
            Self::insert_unique(&mut entries, entry);
        }

        // Config write commands
        for entry in Self::config_write_commands() {
            Self::insert_unique(&mut entries, entry);
        }

        // Workspace file write commands
        for entry in Self::workspace_file_write_commands() {
            Self::insert_unique(&mut entries, entry);
        }

        Self { entries }
    }

    fn insert_unique(entries: &mut HashMap<&'static str, CommandEffect>, entry: CommandEffect) {
        let path = entry.command_path;
        let previous_class = entries.get(path).map(|prior| prior.default_effect);
        if entries.insert(path, entry).is_some() {
            panic!(
                "EffectManifest::build: duplicate command path `{path}` registered in two \
                 categories (previous default_effect = {previous_class:?}). A command must \
                 appear in exactly one of {{read_only, degraded_unavailable, derived_write, \
                 external_io_write, supervised_job, append_only_write, durable_write, \
                 config_write, workspace_file_write}} — the second registration would \
                 silently overwrite the first and the command's declared effect class \
                 would depend on the build()-walk order."
            );
        }
    }

    fn read_only_commands() -> Vec<CommandEffect> {
        vec![
            CommandEffect::read_only("agent detect", "Detect installed coding agents"),
            CommandEffect::read_only("agent scan", "Scan agent probe paths"),
            CommandEffect::read_only("agent sources", "List known agent source connectors"),
            CommandEffect::read_only("agent status", "Report local agent inventory status"),
            CommandEffect::read_only(
                "analyze clustering",
                "Analyze consolidation clustering posture",
            ),
            CommandEffect::read_only(
                "analyze drift",
                "Analyze drift between evaluation snapshots",
            ),
            CommandEffect::read_only("analyze science-status", "Report science readiness"),
            CommandEffect::read_only("agent-docs", "Display agent documentation"),
            CommandEffect::read_only_db("audit diff", "Show audit log mutations in a time window"),
            CommandEffect::read_only_db("audit show", "Show one audit log row"),
            CommandEffect::read_only_db("audit timeline", "List audit log rows"),
            CommandEffect::read_only_db("audit verify", "Verify audit hash-chain integrity"),
            CommandEffect::read_only_db("backup inspect", "Inspect backup manifest"),
            CommandEffect::read_only_db("backup list", "List backup manifests"),
            CommandEffect::read_only_db("backup verify", "Verify backup manifest and contents"),
            CommandEffect::read_only_db(
                "bootstrap docs",
                "Compile allowlisted workspace docs into dry-run bootstrap candidates (no durable mutation)",
            ),
            CommandEffect::read_only("capabilities", "Report feature availability"),
            CommandEffect::read_only(
                "cache hotset-manifest",
                "Collect a read-only hotset manifest from bounded coordination evidence",
            ),
            CommandEffect::read_only(
                "cache prewarm",
                "Plan explicit cache prewarm admission from a hotset manifest",
            ),
            CommandEffect::read_only_db("certificate list", "List persisted certificate records"),
            CommandEffect::read_only_db(
                "certificate show",
                "Inspect a persisted certificate record",
            ),
            CommandEffect::read_only_db(
                "certificate verify",
                "Verify persisted certificate hash and signature evidence",
            ),
            CommandEffect::read_only(
                "certificate sign",
                "Compute a certificate signature from local key material without persisting it",
            ),
            CommandEffect::read_only_db("causal trace", "Trace persisted causal evidence chains"),
            CommandEffect::read_only_db(
                "causal compare",
                "Compare persisted causal evidence chains and scoped causal evidence",
            ),
            CommandEffect::read_only_db(
                "causal estimate",
                "Estimate causal uplift from persisted or scoped causal evidence",
            ),
            CommandEffect::read_only(
                "regress explain",
                "Build a regression-causality capsule from explicit structured artifacts",
            ),
            CommandEffect::read_only("check", "Quick posture summary"),
            CommandEffect::read_only("claim list", "List executable claims from claims.yaml"),
            CommandEffect::read_only("claim show", "Inspect one executable claim"),
            CommandEffect::read_only(
                "claim verify",
                "Verify executable claim evidence without mutating source records",
            ),
            CommandEffect::read_only_db(
                "capture suggest",
                "Suggest ambient capture candidates from session evidence without durable mutation",
            ),
            CommandEffect::read_only("config get", "Read one merged config key"),
            CommandEffect::read_only("config show", "Show merged config values"),
            CommandEffect::read_only_db(
                "conflict cluster",
                "Cluster persisted contradiction/conflict evidence without mutation",
            ),
            CommandEffect::read_only_db(
                "conflict explain",
                "Explain persisted contradiction/conflict evidence",
            ),
            CommandEffect::read_only_db(
                "conflict list",
                "List persisted contradiction/conflict evidence",
            ),
            CommandEffect::read_only_db("context", "Assemble context pack (reads only)"),
            CommandEffect::read_only_db("context-show", "Show a persisted context pack"),
            CommandEffect::read_only_db(
                "decide list",
                "List durable decision memories and supersede-chain heads",
            ),
            CommandEffect::read_only_db(
                "decide revisit",
                "List due or near-due decision revisit reminders",
            ),
            CommandEffect::read_only_db("orient", "Assemble read-only agent orientation bundle"),
            CommandEffect::read_only_db(
                "orient decisions",
                "Read due decision revisit reminders for orientation output",
            ),
            CommandEffect::read_only("completion", "Generate shell completion scripts"),
            CommandEffect::read_only_db("db status", "Report database status"),
            CommandEffect::read_only_db("db check", "Check database integrity"),
            CommandEffect::read_only_db(
                "db inspect",
                "Inspect rows from one database table without mutation",
            ),
            CommandEffect::read_only_db(
                "db reindex",
                "Preview database-derived index rebuild work",
            ),
            CommandEffect::read_only_db("db migrations", "List database migrations"),
            CommandEffect::read_only_db("curate candidates", "List curation candidates"),
            CommandEffect::read_only_db(
                "curate doctor",
                "Diagnose memory debt from persisted memory state",
            ),
            CommandEffect::read_only_db(
                "health scorecard",
                "Summarize memory-store health from debt, gaps, trust, redundancy, and graph signals",
            ),
            CommandEffect::read_only_db(
                "curate show",
                "Inspect a single curation candidate read-only with apply preview",
            ),
            CommandEffect::read_only_db("curate validate", "Validate curation candidate"),
            CommandEffect::read_only("demo list", "List demo manifests"),
            CommandEffect::read_only_db("demo show", "Show persisted demo audit rows"),
            CommandEffect::read_only_db("demo verify", "Verify demo artifacts"),
            CommandEffect::read_only_db("diag advisory-lock", "Inspect advisory-lock diagnostics"),
            CommandEffect::read_only_db(
                "diag agentsmd-drift",
                "Report AGENTS.md bridge drift: stale export, file-vs-memory contradictions, missing rules",
            ),
            CommandEffect::read_only_db("diag artifacts", "Inspect artifact diagnostics"),
            CommandEffect::read_only_db(
                "diag build-admission",
                "Inspect build-admission diagnostics",
            ),
            CommandEffect::read_only_db("diag causal-edge", "Inspect causal-edge diagnostics"),
            CommandEffect::read_only_db("diag claims", "Inspect claim diagnostics"),
            CommandEffect::read_only_db("diag contention", "Inspect contention diagnostics"),
            CommandEffect::read_only_db(
                "diag curation-candidate",
                "Inspect curation-candidate diagnostics",
            ),
            CommandEffect::read_only_db("diag database-skew", "Inspect database-skew diagnostics"),
            CommandEffect::read_only_db("diag dependencies", "Inspect dependency diagnostics"),
            CommandEffect::read_only_db("diag disk-pressure", "Inspect disk-pressure diagnostics"),
            CommandEffect::read_only_db(
                "diag environment-attestation",
                "Inspect environment attestation diagnostics",
            ),
            CommandEffect::read_only_db("diag graph", "Inspect graph diagnostics"),
            CommandEffect::read_only_db(
                "diag graph-snapshot",
                "Inspect graph-snapshot diagnostics",
            ),
            CommandEffect::read_only_db("diag host-profile", "Inspect host-profile diagnostics"),
            CommandEffect::read_only_db("diag incident", "Inspect incident diagnostics"),
            CommandEffect::read_only_db("diag integrity", "Inspect storage integrity diagnostics"),
            CommandEffect::read_only_db(
                "diag memory-validity",
                "Inspect memory-validity diagnostics",
            ),
            CommandEffect::read_only_db(
                "diag model-registry",
                "Inspect model-registry diagnostics",
            ),
            CommandEffect::read_only_db("diag pack-latest", "Inspect latest pack diagnostics"),
            CommandEffect::read_only(
                "diag resource-admission",
                "Preview a resource admission decision from explicit inputs",
            ),
            CommandEffect::read_only_db("diag plan-cache", "Inspect plan-cache diagnostics"),
            CommandEffect::read_only_db(
                "diag provenance",
                "Inspect live provenance freshness diagnostics",
            ),
            CommandEffect::read_only_db("diag quarantine list", "List quarantine entries"),
            CommandEffect::read_only_db("diag quarantine show", "Show single quarantine entry"),
            CommandEffect::read_only_db("diag search", "Inspect search diagnostics"),
            CommandEffect::read_only(
                "diag store-integrity",
                "Inspect explicit read-fence and write-immune diagnostics",
            ),
            CommandEffect::read_only_db("diag streams", "Show streams status"),
            CommandEffect::read_only(
                "diag toolchain-provenance",
                "Inspect observed toolchain provenance without mutating state",
            ),
            CommandEffect::read_only_db("diag tripwire", "Inspect tripwire diagnostics"),
            CommandEffect::read_only_db("diag write-owner", "Inspect write-owner diagnostics"),
            CommandEffect::read_only_db("diag write-spool", "Inspect write-spool diagnostics"),
            CommandEffect::read_only_db("doctor", "Run health checks"),
            CommandEffect::read_only("eval list", "List evaluation scenarios"),
            CommandEffect::read_only("eval report", "Summarize evaluation fixture reports"),
            CommandEffect::read_only("eval run", "Run evaluation (reads fixtures)"),
            CommandEffect::read_only_db(
                "economy report",
                "Report DB-backed memory economy metrics without mutation",
            ),
            CommandEffect::read_only_db(
                "economy score",
                "Score one persisted memory economy artifact without mutation",
            ),
            CommandEffect::read_only_db(
                "economy simulate",
                "Simulate attention budgets from persisted economy metrics without mutation",
            ),
            CommandEffect::read_only_db(
                "economy prune-plan",
                "Plan report-only memory economy pruning without mutation",
            ),
            CommandEffect::read_only_db(
                "focus explain",
                "Explain passive active-memory focus state",
            ),
            CommandEffect::read_only_db("focus show", "Show passive active-memory focus state"),
            CommandEffect::read_only_db(
                "focus suggest",
                "Suggest focus areas from recent CASS spans and graph centrality (bd-sg5si Phase 1: schema scaffold)",
            ),
            CommandEffect::read_only_db("graph articulation", "List graph articulation points"),
            CommandEffect::read_only_db(
                "graph betweenness",
                "Compute graph betweenness centrality",
            ),
            CommandEffect::read_only_db("graph centrality", "Compute graph centrality metrics"),
            CommandEffect::read_only_db("graph communities", "Compute graph communities"),
            CommandEffect::read_only_db("graph explain-link", "Explain graph link evidence"),
            CommandEffect::read_only_db("graph export", "Export graph projection report"),
            CommandEffect::read_only_db("graph hits", "Compute graph HITS centrality"),
            CommandEffect::read_only_db("graph k-core", "Compute graph k-core decomposition"),
            CommandEffect::read_only_db("graph louvain", "Compute graph Louvain communities"),
            CommandEffect::read_only_db("graph neighborhood", "Inspect graph neighborhood"),
            CommandEffect::read_only_db("graph pagerank", "Compute graph PageRank scores"),
            CommandEffect::read_only_db("graph path", "Find graph shortest path"),
            CommandEffect::read_only(
                "handoff completion-audit",
                "Audit objective completion evidence without mutation",
            ),
            CommandEffect::read_only("handoff inspect", "Inspect handoff capsule"),
            CommandEffect::read_only(
                "handoff preview",
                "Plan handoff capsule contents without writing",
            ),
            CommandEffect::read_only("handoff resume", "Render handoff resume payload"),
            CommandEffect::read_only_db("artifact inspect", "Inspect artifact metadata"),
            CommandEffect::read_only_db("artifact list", "List registered artifacts"),
            CommandEffect::read_only_db(
                "attest memory",
                "Inspect memory attestation inputs and verdicts",
            ),
            CommandEffect::read_only_db(
                "attest pack",
                "Inspect context-pack attestation inputs and verdicts",
            ),
            CommandEffect::read_only_db(
                "attest query",
                "Inspect query attestation inputs and verdicts",
            ),
            CommandEffect::read_only_db("health", "Quick health check"),
            CommandEffect::read_only("help", "Print help"),
            CommandEffect::read_only_db("history", "Show persisted memory history summary"),
            CommandEffect::read_only(
                "hook git-readiness",
                "Inspect local Git hook-chain readiness without mutation",
            ),
            CommandEffect::read_only(
                "hook claude-code",
                "Print Claude Code recall/journal harness hook plan by default",
            ),
            CommandEffect::read_only(
                "hook codex",
                "Print Codex recall/journal harness hook plan by default",
            ),
            CommandEffect::read_only(
                "hook gemini",
                "Report Gemini harness hook support posture without guessing",
            ),
            CommandEffect::read_only(
                "hook status",
                "Inspect managed harness hook posture without mutating settings",
            ),
            CommandEffect::read_only_db(
                "impact",
                "Estimate impact from persisted graph and memory state",
            ),
            CommandEffect::read_only_db("index status", "Show index status"),
            CommandEffect::read_only(
                "index vacuum",
                "Preview reclaimable derived index artifacts without mutation",
            ),
            CommandEffect::read_only_db("insights", "Render persisted insight summaries"),
            CommandEffect::read_only("install check", "Inspect install posture"),
            CommandEffect::read_only("install plan", "Plan install without mutation"),
            CommandEffect::read_only("introspect", "Introspect ee metadata"),
            CommandEffect::read_only_db("job list", "List available steward job types"),
            CommandEffect::read_only_db("job show", "Show steward job row details"),
            CommandEffect::read_only(
                "lab replay",
                "Re-assembles a pack against a previously captured frozen episode and reports whether the captured inputs still produce a matching pack hash (N15.4 / bd-17c65.14.15.5)",
            ),
            CommandEffect::read_only(
                "lab counterfactual",
                "Replays a frozen episode with single-input swaps and surfaces the pack diff between the captured pack and the counterfactual pack (N15.5 / bd-17c65.14.15.6)",
            ),
            CommandEffect::read_only(
                "lab generate-workload",
                "Generate a deterministic workload report without persisting it",
            ),
            CommandEffect::read_only(
                "lab promote-workload",
                "Preview workload promotion and admission without persisting it",
            ),
            CommandEffect::read_only_db(
                "journal list",
                "List append-only journal entries newest-first",
            ),
            CommandEffect::read_only_db(
                "journal show",
                "Show one journal entry with structured sidecar and redaction report",
            ),
            CommandEffect::read_only_db(
                "ask",
                "Deterministic extractive question answering with citations and honest abstention",
            ),
            CommandEffect::read_only_db(
                "recall",
                "Code-anchored reverse lookup from paths, symbols, or a git diff to anchored memories",
            ),
            CommandEffect::read_only_db(
                "similar",
                "Find embedding-native nearest-neighbor memories for a persisted seed memory",
            ),
            CommandEffect::read_only_db(
                "learn agenda",
                "Show learning agenda with prioritized gaps",
            ),
            CommandEffect::read_only_db(
                "learn cluster",
                "Cluster learning evidence without durable mutation",
            ),
            CommandEffect::read_only_db(
                "learn gaps",
                "Mine query-miss demand into learning gap templates",
            ),
            CommandEffect::read_only_db("learn summary", "Show learning summary statistics"),
            CommandEffect::read_only_db("learn uncertainty", "Show uncertainty estimates"),
            CommandEffect::read_only_db("lens explain", "Explain a persisted lens projection"),
            CommandEffect::read_only_db("lens list", "List available persisted lens projections"),
            CommandEffect::read_only_db(
                "maintenance status",
                "Report maintenance job availability",
            ),
            CommandEffect::read_only_db("migrate status", "Report pending schema migrations"),
            CommandEffect::read_only("mcp manifest", "Inspect optional MCP adapter manifest"),
            CommandEffect::read_only("mcp validate", "Validate optional MCP adapter contracts"),
            CommandEffect::read_only_db("memory drift", "Report read-only memory provenance drift"),
            CommandEffect::read_only_db("memory history", "Show memory revision history"),
            CommandEffect::read_only_db("memory list", "List memories"),
            CommandEffect::read_only_db("memory show", "Show memory details"),
            CommandEffect::read_only_db(
                "mesh hello-responder",
                "Inspect mesh hello-responder status",
            ),
            CommandEffect::read_only_db("mesh init", "Preview mesh initialization state"),
            CommandEffect::read_only_db(
                "mesh ledger",
                "Inspect receiver-local mesh import decisions",
            ),
            CommandEffect::read_only_db("mesh peer list", "List mesh peers"),
            CommandEffect::read_only_db("mesh peer show", "Show one mesh peer"),
            CommandEffect::read_only_db(
                "mesh peer unknown-attempt",
                "Inspect unknown mesh-peer admission attempts",
            ),
            CommandEffect::read_only_db("mesh peers", "List mesh peers"),
            CommandEffect::read_only_db(
                "mesh preview-grant",
                "Preview a mesh sharing grant without persisting it",
            ),
            CommandEffect::read_only_db("mesh status", "Inspect mesh status"),
            CommandEffect::read_only_db("model list", "List model registry entries"),
            CommandEffect::read_only_db("model status", "Inspect model registry status"),
            CommandEffect::read_only_db("outcome quarantine list", "List feedback quarantine rows"),
            CommandEffect::read_only_db("pack diff", "Compare persisted pack ledgers"),
            CommandEffect::read_only_db("pack replay", "Inspect persisted pack ledger"),
            CommandEffect::read_only(
                "perf budget check",
                "Check normalized performance artifact budget posture",
            ),
            CommandEffect::read_only(
                "perf compare",
                "Compare normalized performance artifact summaries",
            ),
            CommandEffect::read_only(
                "perf explain-latency",
                "Explain latency stages for a normalized performance artifact",
            ),
            CommandEffect::read_only(
                "perf live",
                "Stream read-only performance snapshots for swarm observability",
            ),
            CommandEffect::read_only(
                "perf prompt-budget",
                "Estimate prompt budget posture without durable mutation",
            ),
            CommandEffect::read_only(
                "perf snapshot",
                "Emit one read-only performance snapshot for swarm observability",
            ),
            CommandEffect::read_only("plan recipe list", "List static plan recipes"),
            CommandEffect::read_only("plan recipe show", "Show static plan recipe"),
            CommandEffect::read_only(
                "preflight show",
                "Read a persisted preflight run from the workspace-local store",
            ),
            CommandEffect::read_only_db(
                "preflight check",
                "Retrieve advisory command-risk memories without granting or revoking execution authority",
            ),
            CommandEffect::read_only_db(
                "preflight guard",
                "Alias for read-only advisory command-risk memory retrieval",
            ),
            CommandEffect::read_only("plan goal", "Recommends recipes for goals"),
            CommandEffect::read_only("plan explain", "Explains recipe selection"),
            CommandEffect::read_only("plan recommend", "Recommends recipes for tasks"),
            CommandEffect::read_only_db("playbook list", "List procedural rules in playbook form"),
            CommandEffect::read_only_db(
                "procedure drift",
                "Inspect procedure maturity and feedback drift signals",
            ),
            CommandEffect::read_only_db(
                "procedure export",
                "Render a persisted procedure artifact",
            ),
            CommandEffect::read_only_db("procedure list", "List persisted procedures"),
            CommandEffect::read_only_db("procedure show", "Show persisted procedure details"),
            CommandEffect::read_only_db(
                "procedure verify",
                "Verify a persisted procedure against evidence sources",
            ),
            CommandEffect::read_only_db("rationale list", "List safe rationale traces"),
            CommandEffect::read_only_db("rationale show", "Show a safe rationale trace"),
            CommandEffect::read_only_db(
                "reflect request-ledger diagnostics",
                "Inspect reflection request ledger diagnostics without exposing secret payloads",
            ),
            CommandEffect::read_only(
                "profile config plan",
                "Plan operating profile configuration without writing files",
            ),
            CommandEffect::read_only_db("proof admit", "Preview proof admission status"),
            CommandEffect::read_only_db("proof status", "Inspect proof status"),
            CommandEffect::read_only_db(
                "proximity",
                "Compute memory proximity from persisted graph state",
            ),
            CommandEffect::read_only(
                "recorder tail",
                "Recorder tail reads persisted recorder events without mutation",
            ),
            CommandEffect::read_only(
                "recorder follow",
                "Recorder follow streams persisted recorder events without mutation",
            ),
            CommandEffect::read_only(
                "recorder import",
                "Recorder import planning is read-only unless explicitly promoted to execution",
            ),
            CommandEffect::read_only(
                "recorder events list",
                "List persisted recorder events without mutation",
            ),
            CommandEffect::read_only(
                "recorder flight replay",
                "Replay a flight-recorder trace without mutating source state",
            ),
            CommandEffect::read_only(
                "rehearse plan",
                "Rehearsal planning validates command specs and estimates side-path artifacts",
            ),
            CommandEffect::read_only(
                "rehearse inspect",
                "Rehearsal inspection reads a prior manifest and verifies hashes",
            ),
            CommandEffect::read_only(
                "rehearse promote-plan",
                "Rehearsal promotion planning reads a manifest and emits a conservative checklist",
            ),
            CommandEffect::read_only(
                "review session",
                "Analyze session evidence spans for curation candidates",
            ),
            CommandEffect::read_only(
                "sandbox diff",
                "Compare sandbox state with workspace state without applying changes",
            ),
            CommandEffect::read_only_db("rule list", "List procedural rules"),
            CommandEffect::read_only_db(
                "rule provenance",
                "Inspect the rule-to-memory provenance ego graph",
            ),
            CommandEffect::read_only_db("rule show", "Show procedural rule"),
            CommandEffect::read_only("schema export", "Export public response schemas"),
            CommandEffect::read_only("schema list", "List response schemas"),
            CommandEffect::read_only_db("search", "Search memories"),
            CommandEffect::read_only_db(
                "sentinel explain",
                "Explain sentinel specifications and prior results",
            ),
            CommandEffect::read_only_db(
                "share preview",
                "Preview outbound mesh sharing without exporting data",
            ),
            CommandEffect::read_only_db("show", "Show a persisted memory or artifact"),
            CommandEffect::read_only(
                "situation classify",
                "Classify task into situation category",
            ),
            CommandEffect::read_only("situation compare", "Compare two situations (dry-run)"),
            CommandEffect::read_only_db("situation explain", "Explain a stored situation"),
            CommandEffect::read_only("situation link", "Plan situation link (dry-run)"),
            CommandEffect::read_only_db("situation show", "Show stored situation details"),
            CommandEffect::read_only_db("status", "Report workspace status"),
            CommandEffect::read_only_db("subscribe poll", "Poll subscription state"),
            CommandEffect::read_only_db(
                "subscribe stream",
                "Stream subscription state without writing durable records",
            ),
            CommandEffect::read_only(
                "support inspect",
                "Verify and inspect a redacted support bundle manifest",
            ),
            CommandEffect::read_only(
                "session-budget plan",
                "Advisory deterministic plan for cheapest useful next command given ledger and posture",
            ),
            CommandEffect::read_only_db("swarm brief", "Report read-only swarm coordination brief"),
            CommandEffect::read_only_db(
                "swarm next-action",
                "Recommend the next swarm action without claiming work",
            ),
            CommandEffect::read_only_db(
                "swarm repair-plan",
                "Render an advisory degraded-stack repair plan without executing repairs",
            ),
            CommandEffect::read_only_db(
                "swarm work-packet",
                "Render a swarm work packet without mutating coordination state",
            ),
            CommandEffect::read_only_db("task-frame show", "Show passive task-frame state"),
            CommandEffect::read_only_db(
                "timeline",
                "Reconstruct read-only memory state for a topic at a historical timestamp",
            ),
            CommandEffect::read_only_db(
                "trust report",
                "Audit confidence calibration and outcome-backed reliability",
            ),
            CommandEffect::read_only_db("tripwire list", "List persisted tripwire rules"),
            CommandEffect::read_only("update", "Plan update without mutation"),
            CommandEffect::read_only_db(
                "verification broker lookup",
                "Look up verification broker state",
            ),
            CommandEffect::read_only_db(
                "verification closeout capsule",
                "Render a verification closeout capsule",
            ),
            CommandEffect::read_only_db(
                "verification closure-guidance",
                "Render verification closure guidance",
            ),
            CommandEffect::read_only_db("verification proofs", "List verification proofs"),
            CommandEffect::read_only_db(
                "verification rch blockers",
                "List RCH verification blockers",
            ),
            CommandEffect::read_only_db("verification rch runs", "List RCH verification runs"),
            CommandEffect::read_only_db(
                "verification rch topology-audit",
                "Audit RCH topology closure for path-dep and crate-graph gaps",
            ),
            CommandEffect::read_only_db(
                "verify broker lookup",
                "Look up verification broker state",
            ),
            CommandEffect::read_only_db(
                "verify closeout capsule",
                "Render a verification closeout capsule",
            ),
            CommandEffect::read_only_db(
                "verify closure-guidance",
                "Render verification closure guidance",
            ),
            CommandEffect::read_only_db("verify proofs", "List verification proofs"),
            CommandEffect::read_only_db("verify rch blockers", "List RCH verification blockers"),
            CommandEffect::read_only_db("verify rch runs", "List RCH verification runs"),
            CommandEffect::read_only_db(
                "verify rch topology-audit",
                "Audit RCH topology closure for path-dep and crate-graph gaps",
            ),
            CommandEffect::read_only("version", "Print version"),
            CommandEffect::read_only_db(
                "workspace hygiene",
                "Inspect workspace hygiene and coordination state",
            ),
            CommandEffect::read_only("workspace list", "List workspace aliases"),
            CommandEffect::read_only("workspace resolve", "Resolve workspace identity"),
            CommandEffect::read_only_db("why", "Explain memory selection"),
            CommandEffect::read_only_db("why-not", "Explain why a memory was not selected"),
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
                "primer",
                vec!["primer_cache (db table)"],
                "Assemble the cached workspace primer; cache rows are derived and rebuildable (--no-persist is read-only)",
            ),
            CommandEffect::derived_write(
                "index reembed",
                vec![".ee/index/embeddings/"],
                "Rebuild semantic embeddings from database records",
            ),
            CommandEffect::derived_write(
                "search --recalibrate-now",
                vec![".ee/search/calibration.jsonl"],
                "Rewrite the derived search score calibration artifact from persisted feedback",
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
            CommandEffect::derived_write(
                "graph snapshot refresh",
                vec![".ee/graph/"],
                "Refresh derived graph snapshots from source database state",
            ),
        ]
    }

    fn degraded_unavailable_commands() -> Vec<CommandEffect> {
        Vec::new()
    }

    fn daemon_command_effect() -> CommandEffect {
        let mut effect = CommandEffect::external_io_write(
            "daemon",
            vec!["memories", "feedback_events", "audit_log"],
            vec![
                "$XDG_RUNTIME_DIR/ee/daemon.sock or ${TMPDIR:-/tmp}/ee-<uid>/daemon.sock",
                ".ee/daemon-jobs.jsonl",
            ],
            "daemon subcommand plus socket path plus workspace plus job type",
            "Run daemon status, foreground steward jobs, background steward scheduling, or UDS hot-mode lifecycle operations",
        );
        effect.idempotency = IdempotencyClass::DryRunAvailable;
        effect.dry_run_effect = Some(EffectClass::WorkspaceFileWrite);
        effect.mutation_contract = CommandMutationContract {
            side_effect_class: SideEffectClass::Mixed,
            transaction_scope: Some("daemon subcommand-specific operation"),
            idempotency_key: Some(
                "daemon subcommand plus socket path plus workspace plus job type",
            ),
            audit_surface: Some(
                "daemon job ledger or audit_log when a selected subcommand mutates",
            ),
            db_generation_effect: "subcommand-specific: status is read-only; foreground jobs may advance handler-owned state",
            index_generation_effect: "subcommand-specific: unchanged unless a selected steward job processes index work",
            dry_run_behavior: Some(
                "foreground --dry-run records planned daemon job rows and reports handler plans without committing handler mutations",
            ),
            recovery_behavior: "foreground and background job rows are persisted and recovered on restart",
            no_overwrite_behavior: Some(
                "daemon socket paths are guarded by same-UID socket checks; stop refuses regular files and stale unauthenticated sockets",
            ),
            degraded_code: None,
        };
        effect.runtime_contract = CommandRuntimeContract {
            runtime_class: RuntimeClass::MultiStage,
            default_budget_ms: Some(300_000),
            cancellation_points: &[
                "before_daemon_mode_dispatch",
                "before_socket_publish_or_probe",
                "before_foreground_job_schedule",
                "before_steward_handler",
                "before_daemon_job_row_commit",
            ],
            partial_progress_policy: "option-specific: status is read-only; foreground jobs persist planned rows before handler execution and terminal rows after completion",
            outcome_mapping: "success, usage_error, storage_error, policy_denied, or supervised job failure",
        };
        effect
    }

    fn external_io_write_commands() -> Vec<CommandEffect> {
        vec![
            Self::daemon_command_effect(),
            CommandEffect::external_io_write(
                "demo run",
                vec!["audit_log"],
                vec![
                    "demo evidence root",
                    "manifest-declared demo artifact paths",
                ],
                "demo id plus manifest hash plus generated run id",
                "Execute safe demo manifest steps with audit ledger rows and evidence artifacts",
            ),
            CommandEffect::external_io_write(
                "lab swarm replay",
                vec!["audit_log"],
                vec![".ee/lab/swarm-replay/"],
                "workload id plus replay host profile plus generated run id",
                "Replay a swarm workload through subprocess execution and write replay evidence artifacts",
            ),
            CommandEffect::external_io_write(
                "mcp serve-stdio",
                Vec::new(),
                vec!["stdio JSON-RPC stream"],
                "process id plus stdio session",
                "Serve the optional MCP stdio adapter over process I/O",
            ),
            CommandEffect::external_io_write(
                "mesh auto-enroll",
                vec!["mesh_peers", "audit_log"],
                vec![
                    ".ee/auto_enroll_overrides.toml",
                    ".ee/discovery_denylist.toml",
                ],
                "tailscale peer set hash plus workspace id",
                "Probe mesh peers and persist reviewed auto-enrollment state",
            ),
            CommandEffect::external_io_write(
                "model fetch",
                vec!["model_registry", "audit_log"],
                vec!["~/.local/share/ee/models/"],
                "model alias plus artifact content hash",
                "Fetch or import a model artifact and update the model registry",
            ),
            CommandEffect::external_io_write(
                "serve",
                Vec::new(),
                vec!["localhost HTTP/SSE listener"],
                "process id plus listener address",
                "Serve the optional localhost adapter",
            ),
        ]
    }

    fn supervised_job_commands() -> Vec<CommandEffect> {
        vec![
            CommandEffect::supervised_job(
                "daemon foreground decay_sweep",
                vec!["memories", "feedback_events", "audit_log"],
                "Run the real score-decay steward handler in a bounded foreground daemon tick",
            ),
            CommandEffect::supervised_job(
                "daemon background",
                vec!["memories", "feedback_events", "audit_log"],
                "Run configured steward handlers on the daemon background scheduler",
            ),
            CommandEffect::supervised_job(
                "daemon foreground non-decay",
                vec!["memories", "feedback_events", "audit_log"],
                "Run real non-decay steward handlers in a bounded foreground daemon tick",
            ),
            CommandEffect::supervised_job(
                "job run",
                vec!["memories", "feedback_events", "audit_log"],
                "Run a steward job directly through the job interface",
            ),
            CommandEffect::supervised_job(
                "maintenance run",
                vec!["memories", "feedback_events", "audit_log"],
                "Run an explicit bounded maintenance job through the steward backend",
            ),
            CommandEffect::supervised_job(
                "maintenance graph-snapshot-prune",
                vec!["graph_snapshots", "audit_log"],
                "Prune expired graph snapshots through a bounded steward job",
            ),
        ]
    }

    fn append_only_write_commands() -> Vec<CommandEffect> {
        let diag_pack_record = CommandEffect::append_only_write(
            "diag pack-record",
            vec!["pack_records", "audit_log"],
            "pack id",
            "Append one audited diagnostic pack record without overwriting an existing ID",
        );
        vec![
            diag_pack_record,
            CommandEffect::append_only_write(
                "db check-integrity",
                vec!["audit_log"],
                "audit row id",
                "Run full database integrity verification and append an audit row",
            ),
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
                "import agentsmd",
                vec![
                    "curation_candidates",
                    "evidence_spans",
                    "sessions",
                    "audit_log",
                ],
                "deterministic candidate id over (file, statement text)",
                "Import rule-like AGENTS.md statements as pending curation candidates",
            ),
            CommandEffect::append_only_write(
                "pack build",
                vec!["context_packs", "pack_items", "audit_log"],
                "pack hash",
                "Persist a context pack keyed by deterministic pack hash",
            ),
            CommandEffect::append_only_write(
                "reflect propose",
                vec!["reflection_request_ledger"],
                "requestHash",
                "Create an external reflection request artifact and non-secret replay ledger row",
            ),
            CommandEffect::append_only_write(
                "mesh import",
                vec!["mesh_peers", "mesh_import_ledger", "search_index_jobs"],
                "origin peer cursor plus event content hash",
                "Import a mesh artifact by replaying idempotent peer events",
            ),
            // Honest classification (bd-6dmhw): the production sync transport
            // is a deliberate no-op until M1 real transport lands
            // (bd-tc-epic-qzk7o.3.x); the command performs no peer network
            // I/O today. Restore external_io_write with the transport.
            CommandEffect::append_only_write(
                "mesh sync",
                vec!["mesh_peers", "mesh_import_ledger", "search_index_jobs"],
                "origin peer cursor plus event content hash",
                "Run one foreground sync cycle over locally available peer state; network transport is deferred",
            ),
            CommandEffect::append_only_write(
                "verification ingest",
                vec!["audit_log"],
                "verification evidence content hash",
                "Ingest verification evidence into the audit ledger",
            ),
            CommandEffect::append_only_write(
                "verification rch ingest",
                vec!["rch_verify_runs"],
                "rch proof command hash plus run id",
                "Ingest RCH verification run evidence",
            ),
            CommandEffect::append_only_write(
                "verification record",
                vec!["audit_log"],
                "verification record content hash",
                "Record verification evidence in the audit ledger",
            ),
            CommandEffect::append_only_write(
                "verify ingest",
                vec!["audit_log"],
                "verification evidence content hash",
                "Ingest verification evidence into the audit ledger",
            ),
            CommandEffect::append_only_write(
                "verify rch ingest",
                vec!["rch_verify_runs"],
                "rch proof command hash plus run id",
                "Ingest RCH verification run evidence",
            ),
            CommandEffect::append_only_write(
                "verify record",
                vec!["audit_log"],
                "verification record content hash",
                "Record verification evidence in the audit ledger",
            ),
        ]
    }

    fn durable_write_commands() -> Vec<CommandEffect> {
        vec![
            CommandEffect::durable_write(
                "diagnose-error",
                vec!["error_fingerprints"],
                "Diagnose a tool error against the fingerprint recall store; --record persists its fingerprint",
            ),
            CommandEffect::durable_write(
                "bootstrap apply",
                vec![
                    "curation_candidates",
                    "memories",
                    "procedural_rules",
                    "rule_source_memories",
                    "rule_tags",
                    "search_index_jobs",
                    "audit_log",
                ],
                "Apply an approved docs bootstrap run through curation (routes through curate apply with audit)",
            ),
            CommandEffect::durable_write(
                "causal promote-plan",
                vec!["curation_candidates", "audit_log"],
                "Plan causal promotion and persist reviewed curation candidates when evidence clears thresholds",
            ),
            CommandEffect::durable_write(
                "curate accept",
                vec!["curation_candidates", "procedural_rules", "audit_log"],
                "Accept a curation candidate",
            ),
            CommandEffect::durable_write(
                "curate auto-promote",
                vec!["memories", "search_index_jobs", "audit_log"],
                "Threshold-based memory level promotion; dry-run by default, --apply routes through memory.level_transition",
            ),
            CommandEffect::durable_write(
                "curate apply",
                vec![
                    "curation_candidates",
                    "memories",
                    "procedural_rules",
                    "rule_source_memories",
                    "rule_tags",
                    "search_index_jobs",
                    "audit_log",
                ],
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
                "curate propose-derived",
                vec!["curation_candidates", "audit_log"],
                "Persist derived curation proposals for explicit review",
            ),
            CommandEffect::durable_write(
                "curate reject",
                vec!["curation_candidates", "audit_log"],
                "Reject a curation candidate",
            ),
            CommandEffect::durable_write(
                "curate retire",
                vec![
                    "curation_candidates",
                    "memories",
                    "search_index_jobs",
                    "audit_log",
                ],
                "Retire an accepted curation artifact through audited memory/index updates",
            ),
            CommandEffect::durable_write(
                "curate snooze",
                vec!["curation_candidates", "audit_log"],
                "Snooze a curation candidate",
            ),
            CommandEffect::durable_write(
                "curate tombstone",
                vec![
                    "curation_candidates",
                    "memories",
                    "search_index_jobs",
                    "audit_log",
                ],
                "Tombstone a curation candidate and related memory state without deleting records",
            ),
            CommandEffect::durable_write(
                "curate untombstone",
                vec![
                    "curation_candidates",
                    "memories",
                    "search_index_jobs",
                    "audit_log",
                ],
                "Restore a tombstoned curation candidate through audited memory/index updates",
            ),
            CommandEffect::durable_write_with_workspace_files(
                "handoff rotate-key",
                vec!["audit_log"],
                vec!["<handoff capsule path>"],
                "Rotate a handoff capsule HMAC key and rewrite the signed capsule body",
            ),
            CommandEffect {
                command_path: "health scorecard --record-snapshot",
                default_effect: EffectClass::DurableMemoryWrite,
                dry_run_effect: Some(EffectClass::ReadOnly),
                idempotency: IdempotencyClass::Idempotent,
                write_surfaces: WriteSurfaces {
                    db_tables: vec!["debt_snapshots"],
                    derived_paths: Vec::new(),
                    workspace_files: Vec::new(),
                },
                mutation_contract: CommandMutationContract {
                    side_effect_class: SideEffectClass::AuditedMutation,
                    transaction_scope: Some("single DB insert-or-ignore for memory debt snapshot"),
                    idempotency_key: Some("workspace id plus snapshot day plus generation"),
                    audit_surface: Some("debt_snapshots"),
                    db_generation_effect: "advances only when a new debt snapshot row commits; unchanged on duplicate",
                    index_generation_effect: "none",
                    dry_run_behavior: Some(
                        "omit --record-snapshot to render the same scorecard without writing debt_snapshots",
                    ),
                    recovery_behavior: "insert-or-ignore leaves at most one complete snapshot row per workspace/day/generation",
                    no_overwrite_behavior: None,
                    degraded_code: None,
                },
                runtime_contract: CommandRuntimeContract::transactional(),
                requires_read_snapshot: false,
                requires_audit: true,
                description: "Record a memory-debt trend snapshot before rendering the health scorecard",
            },
            CommandEffect::durable_write(
                "playbook extract",
                vec!["curation_candidates", "audit_log"],
                "Extract procedural-rule candidates from repeated semantic memories",
            ),
            CommandEffect::durable_write(
                "playbook import",
                vec![
                    "procedural_rules",
                    "rule_source_memories",
                    "rule_tags",
                    "search_index_jobs",
                    "audit_log",
                ],
                "Import portable playbook rules through audited procedural-rule writes",
            ),
            CommandEffect::durable_write(
                "journal append",
                vec!["journal_entries"],
                "Append a redaction-screened observation to the agent journal",
            ),
            CommandEffect::durable_write(
                "journal distill",
                vec![
                    "journal_entries",
                    "curation_candidates",
                    "evidence_spans",
                    "sessions",
                    "audit_log",
                ],
                "Distill journal entries into pending curation candidates; dry-run by default, --apply writes",
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
                "learn experiment propose",
                vec!["curation_candidates", "audit_log"],
                "Persist experiment proposals to curation queue",
            ),
            CommandEffect::durable_write(
                "outcome",
                vec!["feedback_events", "audit_log"],
                "Record observed outcome feedback",
            ),
            CommandEffect::durable_write(
                "procedure promote",
                vec!["procedures", "procedure_events", "audit_log"],
                "Promote a persisted procedure maturity level",
            ),
            CommandEffect::durable_write(
                "procedure propose",
                vec!["procedures", "procedure_events", "audit_log"],
                "Persist a procedure candidate from explicit evidence",
            ),
            CommandEffect::durable_write(
                "procedure retire",
                vec!["procedures", "procedure_events", "audit_log"],
                "Retire a persisted procedure with an audited reason",
            ),
            CommandEffect::durable_write(
                "outcome quarantine release",
                vec!["feedback_quarantine", "audit_log"],
                "Release feedback from quarantine",
            ),
            CommandEffect::durable_write(
                "rationale attach",
                vec!["rationale_traces", "audit_log"],
                "Attach a safe rationale trace with audit provenance",
            ),
            CommandEffect::durable_write(
                "remember",
                vec!["memories", "memory_tags", "audit_log"],
                "Store a new memory with direct or audit-lane-backed audit_log provenance",
            ),
            CommandEffect::durable_write(
                "decide record",
                vec![
                    "memories",
                    "memory_tags",
                    "memory_links",
                    "search_index_jobs",
                    "audit_log",
                ],
                "Record a durable decision memory and optionally supersede the prior decision head",
            ),
            CommandEffect::durable_write(
                "memory revise",
                vec!["memories", "audit_log"],
                "Inserts a new memory row with the same logical_id as the original, sets the prior row's valid_to, and emits a memory.revise audit entry (N15.2 / bd-17c65.14.15.3)",
            ),
            CommandEffect::durable_write(
                "memory expire",
                vec!["memories", "search_index_jobs", "audit_log"],
                "Expire a memory through an audited tombstone without deleting data",
            ),
            CommandEffect::durable_write(
                "memory reveal",
                vec!["memories", "memory_seals", "search_index_jobs", "audit_log"],
                "Verify supplied bytes against a sealed memory's commitment; on match publish the content through the revise path, mark the seal revealed, and audit memory.reveal — a mismatch mutates nothing and audits memory.reveal_failed (bd-sealed-preregistration-memory-b67be)",
            ),
            CommandEffect::durable_write(
                "memory level",
                vec!["memories", "search_index_jobs", "audit_log"],
                "Apply a canonical manual memory-level transition with audit provenance",
            ),
            CommandEffect::durable_write(
                "memory link",
                vec!["memory_links", "audit_log"],
                "List or create explicit memory links with deterministic idempotent audits",
            ),
            CommandEffect::durable_write(
                "memory tags",
                vec!["memory_tags", "search_index_jobs", "audit_log"],
                "List or mutate memory tags with deterministic idempotent audits",
            ),
            CommandEffect::durable_write(
                "link",
                vec!["memory_links", "audit_log"],
                "Create or inspect explicit memory links with audited mutation when requested",
            ),
            CommandEffect::durable_state_write(
                "maintenance wal-checkpoint",
                vec!["database_wal"],
                "database path plus checkpoint mode",
                "database WAL checkpoint",
                "Checkpoint the workspace database WAL without changing logical memory records",
            ),
            CommandEffect::durable_write_with_workspace_files(
                "mesh discovery-policy",
                vec!["audit_log"],
                vec![
                    ".ee/discovery_policy.toml",
                    ".ee/discovery_allowlist.toml",
                    ".ee/discovery_denylist.toml",
                ],
                "Persist mesh discovery policy files and audit the policy change",
            ),
            CommandEffect::durable_write_with_workspace_files(
                "mesh export",
                vec!["audit_log"],
                vec!["<--out path>"],
                "Export authorized mesh material to an explicit side-path artifact",
            ),
            CommandEffect::durable_write(
                "mesh peer add",
                vec!["mesh_peers", "audit_log"],
                "Add a mesh peer with audited policy metadata",
            ),
            CommandEffect::durable_write(
                "mesh peer revoke",
                vec!["mesh_peers", "audit_log"],
                "Revoke a mesh peer with audited policy metadata",
            ),
            CommandEffect::durable_write(
                "mesh peer rotate",
                vec!["mesh_peers", "audit_log"],
                "Rotate mesh peer credentials with audited policy metadata",
            ),
            CommandEffect::durable_write(
                "mesh grant",
                vec!["mesh_lane_grant_states", "audit_log"],
                "Apply an authenticated mesh lane consent grant with generation fencing and audit provenance",
            ),
            CommandEffect::durable_write(
                "mesh revoke-lane",
                vec!["mesh_lane_grant_states", "audit_log"],
                "Narrow mesh lane consent with generation fencing and audit provenance",
            ),
            CommandEffect::durable_write(
                "note",
                vec!["memories", "memory_tags", "audit_log"],
                "Store a note as a memory with optional tags",
            ),
            CommandEffect::durable_write(
                "reflect ingest",
                vec![
                    "reflection_request_ledger",
                    "curation_candidates",
                    "audit_log",
                ],
                "Ingest reflection evidence and propose reviewed curation candidates",
            ),
            CommandEffect::durable_write(
                "review workspace",
                vec!["curation_candidates", "audit_log"],
                "Review workspace evidence and persist curation candidates",
            ),
            CommandEffect::durable_write_with_workspace_files(
                "sandbox apply",
                vec!["memories", "memory_tags", "audit_log"],
                vec![".ee/sandbox/<session>.json"],
                "Apply reviewed sandbox memories and record sandbox session state",
            ),
            CommandEffect::durable_state_write(
                "sentinel check",
                vec!["memory_sentinel_results"],
                "sentinel spec hash plus observed result hash",
                "memory sentinel results",
                "Evaluate sentinel specs and persist checked results",
            ),
            CommandEffect::durable_write(
                "tag",
                vec!["memory_tags", "search_index_jobs", "audit_log"],
                "Add or remove memory tags through audited metadata updates",
            ),
            CommandEffect::durable_write(
                "verification provenance",
                vec!["memories", "curation_candidates", "audit_log"],
                "Verify provenance and persist reviewed revalidation candidates",
            ),
            CommandEffect::durable_write(
                "verify provenance",
                vec!["memories", "curation_candidates", "audit_log"],
                "Verify provenance and persist reviewed revalidation candidates",
            ),
            CommandEffect::durable_state_write(
                "recorder start",
                vec!["recorder_runs"],
                "generated recorder run id",
                "recorder run store",
                "Persist a recorder run start row",
            ),
            CommandEffect::durable_state_write(
                "recorder event",
                vec!["recorder_events"],
                "run id plus next recorder sequence",
                "recorder event spine",
                "Append a redacted recorder event row",
            ),
            CommandEffect::durable_state_write(
                "recorder finish",
                vec!["recorder_runs"],
                "recorder run id",
                "recorder run store",
                "Mark a recorder run finished and persist rolled-up counts",
            ),
            CommandEffect::durable_write(
                "review session --propose",
                vec!["curation_candidates", "audit_log"],
                "Persist session-derived curation candidates",
            ),
            CommandEffect::durable_write(
                "workflow close",
                vec!["memories", "audit_log"],
                "Promote eligible workflow working memories to episodic records",
            ),
            CommandEffect::durable_write(
                "workflow create",
                vec!["memories", "audit_log"],
                "Create workflow working memory and audit provenance",
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
                "rule mark",
                vec!["procedural_rules", "audit_log", "search_index_jobs"],
                "Record lifecycle evidence for a procedural rule",
            ),
            CommandEffect::durable_write(
                "rule protect",
                vec!["procedural_rules", "audit_log"],
                "Protect or unprotect a procedural rule",
            ),
            CommandEffect::durable_write(
                "rule update",
                vec![
                    "procedural_rules",
                    "rule_source_memories",
                    "rule_tags",
                    "audit_log",
                    "search_index_jobs",
                ],
                "Update procedural rule metadata",
            ),
            CommandEffect::durable_write(
                "situation adopt",
                vec!["situation_records"],
                "Adopt task text as a persisted situation record via the idempotent fingerprint",
            ),
            CommandEffect::durable_state_write(
                "tripwire check",
                vec!["tripwires", "tripwire_check_events"],
                "tripwire id plus checked_at plus event payload hash",
                "tripwire check event store",
                "Evaluate a persisted tripwire and record the check event unless --dry-run is used",
            ),
            CommandEffect::durable_state_write(
                "maintenance graph-witnesses-prune",
                vec!["graph_algorithm_witnesses"],
                "workspace id plus retention policy plus witness row identity",
                "graph_algorithm_witnesses",
                "Classify graph algorithm witnesses and delete only rows older than policy TTL that are not tied to active snapshots",
            ),
            CommandEffect::schema_migration_run(),
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
            CommandEffect::config_file_write(
                "profile config apply",
                vec![".ee/config.toml"],
                "workspace profile config path plus requested profile",
                "Apply operating profile configuration to the workspace config file",
            ),
            CommandEffect::config_file_write(
                "config set",
                vec![".ee/config.toml"],
                "workspace config path plus exact config key and scalar value",
                "Set a supported workspace configuration key",
            ),
            CommandEffect::harness_hook_settings_write(
                "hook claude-code --install",
                vec![
                    "~/.claude/settings.json",
                    "~/.claude/settings.json.ee-backup",
                ],
                "Claude Code settings path plus generated managed hook snippets",
                "Install ee-managed Claude Code recall and journal hooks into harness settings",
            ),
            CommandEffect::harness_hook_settings_write(
                "hook claude-code --undo",
                vec![
                    "~/.claude/settings.json",
                    "~/.claude/settings.json.ee-backup",
                ],
                "Claude Code settings backup path",
                "Restore Claude Code harness settings from the deterministic ee backup",
            ),
            CommandEffect::harness_hook_settings_write(
                "hook codex --install",
                vec![".codex/hooks.json", ".codex/hooks.json.ee-backup"],
                "Codex hooks path plus generated managed hook snippets",
                "Install ee-managed Codex recall and journal hooks into harness settings",
            ),
            CommandEffect::harness_hook_settings_write(
                "hook codex --undo",
                vec![".codex/hooks.json", ".codex/hooks.json.ee-backup"],
                "Codex hooks backup path",
                "Restore Codex harness settings from the deterministic ee backup",
            ),
            CommandEffect::certificate_key_file_write(
                "certificate keygen",
                vec!["~/.config/ee/keys/<workspace>.ed25519"],
                "workspace key path plus --show/--force mode",
                "Generate or inspect a local certificate signing key",
            ),
            CommandEffect::config_write(
                "mesh disable",
                vec![".ee/config.toml"],
                "workspace id plus mesh disable reason",
                "Disable mesh synchronization for a workspace",
            ),
            CommandEffect::config_write(
                "mesh reenable",
                vec![".ee/config.toml"],
                "workspace id plus mesh reenable reason",
                "Re-enable mesh synchronization for a workspace",
            ),
        ]
    }

    fn workspace_file_write_commands() -> Vec<CommandEffect> {
        vec![
            CommandEffect::shard_fanout_migration(),
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
            CommandEffect::workspace_file_write(
                "export",
                vec![".ee/backups/<backup-id>/ or <--output-dir>/<backup-id>/"],
                "Export redacted JSONL records as side-path artifacts",
            ),
            CommandEffect::workspace_file_write(
                "export agentsmd",
                vec!["AGENTS.md (or --file target) managed block plus .ee-backup sibling"],
                "Render the primer rules+warnings sections into the AGENTS.md managed block",
            ),
            CommandEffect::workspace_file_write(
                "artifact relocate",
                vec![
                    "artifact relocation destination paths",
                    "artifact relocation manifest path",
                ],
                "Copy preserved artifacts to explicit relocation destinations without deleting originals",
            ),
            CommandEffect::workspace_state_write(
                "coordination evidence ingest",
                vec![".ee/coordination-fallback-evidence.jsonl"],
                "coordination evidence content hash",
                "Append coordination fallback evidence to the workspace-local evidence log",
            ),
            CommandEffect::workspace_state_write(
                "daemon start",
                vec!["$XDG_RUNTIME_DIR/ee/daemon.sock or ${TMPDIR:-/tmp}/ee-<uid>/daemon.sock"],
                "daemon socket path plus process id",
                "Bind the optional UDS RPC socket outside the workspace",
            ),
            CommandEffect::workspace_state_write(
                "daemon stop",
                vec!["$XDG_RUNTIME_DIR/ee/daemon.sock or ${TMPDIR:-/tmp}/ee-<uid>/daemon.sock"],
                "daemon socket path plus process id",
                "Stop dialing the optional UDS RPC socket and update daemon state",
            ),
            CommandEffect::workspace_file_write(
                "lab capture",
                vec![".ee/lab/episodes"],
                "Writes a frozen episode artifact under .ee/lab/episodes/<EPISODE_ID>/ — task input, policy ids, evidence ids, pack hash, repository fingerprint (N15.3 / bd-17c65.14.15.4)",
            ),
            CommandEffect::workspace_file_write(
                "focus add",
                vec![".ee/focus/state.json"],
                "Add explicit memories to passive focus state without eviction",
            ),
            CommandEffect::workspace_file_write(
                "handoff create",
                vec!["<--out path>"],
                "Write a redacted continuity capsule to a user-specified output path",
            ),
            CommandEffect::workspace_file_write(
                "playbook export",
                vec!["<--out path>"],
                "Write portable procedural rules to a no-overwrite playbook artifact",
            ),
            CommandEffect::workspace_state_write(
                "preflight close",
                vec![".ee/preflight_runs.json"],
                "preflight run id",
                "Close a persisted preflight run in the workspace-local run store",
            ),
            CommandEffect::workspace_state_write(
                "preflight run",
                vec![".ee/preflight_runs.json"],
                "generated preflight run id",
                "Persist an evidence-backed preflight run in the workspace-local run store",
            ),
            CommandEffect::workspace_file_write(
                "support bundle",
                vec!["<--out path>/"],
                "Create a redacted support bundle side-path artifact",
            ),
            CommandEffect::workspace_file_write(
                "rehearse run",
                vec![
                    "rehearsal artifact root",
                    "tempfile-backed sandbox workspace",
                ],
                "Rehearsal execution writes side-path sandbox artifacts without mutating the source workspace",
            ),
            CommandEffect::workspace_state_write(
                "recorder flight append",
                vec!["flight recorder trace directory"],
                "flight recorder event hash plus sequence",
                "Append an event to a flight-recorder trace",
            ),
            CommandEffect::workspace_state_write(
                "sandbox curate",
                vec![".ee/sandbox/<session>.json"],
                "sandbox session id plus curation event hash",
                "Update sandbox curation state without applying it to durable memories",
            ),
            CommandEffect::workspace_state_write(
                "sandbox import",
                vec![".ee/sandbox/<session>.json"],
                "sandbox session id plus import source hash",
                "Import memories into sandbox state without applying them to durable storage",
            ),
            CommandEffect::workspace_state_write(
                "sandbox remember",
                vec![".ee/sandbox/<session>.json"],
                "sandbox session id plus memory content hash",
                "Record a sandbox memory without applying it to durable storage",
            ),
            CommandEffect::workspace_file_write(
                "focus clear",
                vec![".ee/focus/state.json"],
                "Clear passive focus state by writing an empty state artifact",
            ),
            CommandEffect::workspace_file_write(
                "focus remove",
                vec![".ee/focus/state.json"],
                "Remove explicit memories from passive focus state",
            ),
            CommandEffect::workspace_file_write(
                "focus set",
                vec![".ee/focus/state.json"],
                "Replace passive focus state from explicit command arguments",
            ),
            CommandEffect::workspace_file_write(
                "task-frame create",
                vec![".ee/task_frames.json"],
                "Create a passive task frame without executing commands",
            ),
            CommandEffect::workspace_file_write(
                "task-frame update",
                vec![".ee/task_frames.json"],
                "Update passive task-frame state without executing commands",
            ),
            CommandEffect::workspace_file_write(
                "task-frame close",
                vec![".ee/task_frames.json"],
                "Close a passive task frame without executing commands",
            ),
            CommandEffect::workspace_file_write(
                "task-frame subgoal add",
                vec![".ee/task_frames.json"],
                "Add a passive task-frame subgoal without executing commands",
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

    fn ensure_category<F>(category: &str, entries: Vec<CommandEffect>, accepts: F) -> TestResult
    where
        F: Fn(&CommandEffect) -> bool,
    {
        for entry in entries {
            if !accepts(&entry) {
                return Err(format!(
                    "{} builder contains `{}` with default_effect={} side_effect_class={}",
                    category,
                    entry.command_path,
                    entry.default_effect.as_str(),
                    entry.mutation_contract.side_effect_class.as_str()
                ));
            }
        }
        Ok(())
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
            "lab replay",
            "lab_replay_unavailable",
            "Lab replay abstains until replay evidence exists",
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
            Some("lab_replay_unavailable"),
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
    fn manifest_build_has_no_duplicate_command_paths() -> TestResult {
        // Pin the no-duplicate invariant directly via the category-
        // vector unions rather than only via `build()` (which already
        // panics on duplicate via `insert_unique`). The vector unions
        // exhibit the same drift surface but with a clearer test-
        // failure message identifying the offending category pair —
        // and the test also fails cleanly in release builds (where
        // `debug_assert!` would be a no-op).
        use std::collections::HashMap;
        let mut origins: HashMap<&'static str, &'static str> = HashMap::new();
        let category_vectors: &[(&'static str, Vec<CommandEffect>)] = &[
            ("read_only", EffectManifest::read_only_commands()),
            (
                "degraded_unavailable",
                EffectManifest::degraded_unavailable_commands(),
            ),
            ("derived_write", EffectManifest::derived_write_commands()),
            (
                "external_io_write",
                EffectManifest::external_io_write_commands(),
            ),
            ("supervised_job", EffectManifest::supervised_job_commands()),
            (
                "append_only_write",
                EffectManifest::append_only_write_commands(),
            ),
            ("durable_write", EffectManifest::durable_write_commands()),
            ("config_write", EffectManifest::config_write_commands()),
            (
                "workspace_file_write",
                EffectManifest::workspace_file_write_commands(),
            ),
        ];
        for (category, entries) in category_vectors {
            // Iterating `&[(K, Vec<V>)]` makes `category: &&'static str`
            // and `entries: &Vec<CommandEffect>`; dereference `category`
            // so the HashMap value type matches and the error message
            // shows the bare category name, not a `&str` debug form.
            let category = *category;
            for entry in entries {
                if let Some(previous_category) = origins.insert(entry.command_path, category) {
                    return Err(format!(
                        "command path `{}` is declared in both `{previous_category}` and `{category}` categories; a command must appear in exactly one",
                        entry.command_path
                    ));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn manifest_category_builders_match_declared_effect_classes() -> TestResult {
        ensure_category("read_only", EffectManifest::read_only_commands(), |entry| {
            entry.default_effect == EffectClass::ReadOnly
                && entry.mutation_contract.side_effect_class != SideEffectClass::DegradedUnavailable
        })?;
        ensure_category(
            "degraded_unavailable",
            EffectManifest::degraded_unavailable_commands(),
            |entry| {
                entry.default_effect == EffectClass::ReadOnly
                    && entry.mutation_contract.side_effect_class
                        == SideEffectClass::DegradedUnavailable
                    && entry.write_surfaces.is_empty()
                    && !entry.requires_audit
                    && entry.mutation_contract.degraded_code.is_some()
            },
        )?;
        ensure_category(
            "derived_write",
            EffectManifest::derived_write_commands(),
            |entry| entry.default_effect == EffectClass::DerivedArtifactWrite,
        )?;
        ensure_category(
            "external_io_write",
            EffectManifest::external_io_write_commands(),
            |entry| entry.default_effect == EffectClass::ExternalIo,
        )?;
        ensure_category(
            "supervised_job",
            EffectManifest::supervised_job_commands(),
            |entry| entry.mutation_contract.side_effect_class == SideEffectClass::SupervisedJobs,
        )?;
        ensure_category(
            "append_only_write",
            EffectManifest::append_only_write_commands(),
            |entry| entry.mutation_contract.side_effect_class == SideEffectClass::AppendOnly,
        )?;
        ensure_category(
            "durable_write",
            EffectManifest::durable_write_commands(),
            |entry| {
                entry.default_effect == EffectClass::DurableMemoryWrite
                    && entry.mutation_contract.side_effect_class == SideEffectClass::AuditedMutation
            },
        )?;
        ensure_category(
            "config_write",
            EffectManifest::config_write_commands(),
            |entry| entry.default_effect == EffectClass::ConfigWrite,
        )?;
        ensure_category(
            "workspace_file_write",
            EffectManifest::workspace_file_write_commands(),
            |entry| entry.default_effect == EffectClass::WorkspaceFileWrite,
        )
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

        let preflight_check = manifest
            .get("preflight check")
            .ok_or_else(|| "preflight check not found".to_owned())?;
        let preflight_guard = manifest
            .get("preflight guard")
            .ok_or_else(|| "preflight guard alias not found".to_owned())?;
        ensure(
            preflight_guard.default_effect,
            preflight_check.default_effect,
            "preflight guard alias matches preflight check effect",
        )?;
        ensure(
            preflight_guard.mutation_contract.side_effect_class,
            preflight_check.mutation_contract.side_effect_class,
            "preflight guard alias matches preflight check mutation class",
        )?;
        ensure(
            preflight_guard.requires_audit,
            false,
            "preflight guard alias is read-only and needs no audit",
        )?;
        ensure(
            preflight_guard.write_surfaces.is_empty(),
            true,
            "preflight guard alias declares no write surfaces",
        )?;

        let backup = manifest.get("backup create");
        ensure(backup.is_some(), true, "backup create exists")?;
        ensure(
            backup.map(|e| e.default_effect),
            Some(EffectClass::WorkspaceFileWrite),
            "backup create writes workspace files",
        )?;

        let decay_sweep = manifest.get("daemon foreground decay_sweep");
        ensure(
            decay_sweep.is_some(),
            true,
            "daemon foreground decay_sweep exists",
        )?;
        ensure(
            decay_sweep.map(|e| e.mutation_contract.side_effect_class),
            Some(SideEffectClass::SupervisedJobs),
            "daemon decay sweep uses supervised job contract",
        )?;
        ensure(
            decay_sweep.map(|e| e.runtime_contract.runtime_class),
            Some(RuntimeClass::Supervised),
            "daemon decay sweep runtime",
        )
    }

    #[test]
    fn manifest_classifies_migrate_command_paths() -> TestResult {
        let manifest = EffectManifest::build();

        let status = manifest
            .get("migrate status")
            .ok_or_else(|| "migrate status not found".to_owned())?;
        ensure(
            status.default_effect,
            EffectClass::ReadOnly,
            "migrate status is read-only",
        )?;
        ensure(
            status.read_snapshot(),
            true,
            "migrate status reads through a DB snapshot",
        )?;

        let run = manifest
            .get("migrate run")
            .ok_or_else(|| "migrate run not found".to_owned())?;
        ensure(
            run.default_effect,
            EffectClass::DurableMemoryWrite,
            "migrate run writes durable schema state",
        )?;
        ensure(
            run.dry_run_effect,
            Some(EffectClass::ReadOnly),
            "migrate run dry-run is read-only",
        )?;
        ensure(
            run.mutation_contract.side_effect_class,
            SideEffectClass::AuditedMutation,
            "migrate run has audited mutation contract",
        )?;
        ensure(
            run.write_surfaces
                .db_tables
                .contains(&"ee_schema_migrations"),
            true,
            "migrate run names schema migration table",
        )?;
        ensure(
            run.write_surfaces.derived_paths.contains(&".ee/index/"),
            true,
            "migrate run names post-migration index rebuild",
        )?;

        let shard = manifest
            .get("migrate shard-fanout")
            .ok_or_else(|| "migrate shard-fanout not found".to_owned())?;
        ensure(
            shard.default_effect,
            EffectClass::WorkspaceFileWrite,
            "migrate shard-fanout writes shard files",
        )?;
        ensure(
            shard.dry_run_effect,
            Some(EffectClass::ReadOnly),
            "migrate shard-fanout dry-run is read-only",
        )?;
        ensure(
            shard.mutation_contract.side_effect_class,
            SideEffectClass::AuditedMutation,
            "migrate shard-fanout has audited mutation contract",
        )?;
        ensure(
            shard
                .write_surfaces
                .workspace_files
                .contains(&"<shards-dir>/catalog.db"),
            true,
            "migrate shard-fanout names shard catalog file",
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
    fn manifest_tracks_lab_replay_as_available_read_only_path() -> TestResult {
        let manifest = EffectManifest::build();

        let command = "lab replay";
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not found"))?;
        ensure(
            effect.default_effect,
            EffectClass::ReadOnly,
            &format!("{command} is read-only"),
        )?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::ReadOnly,
            &format!("{command} uses read-only class"),
        )?;
        ensure(
            effect.write_surfaces.is_empty(),
            true,
            &format!("{command} has no write surfaces"),
        )?;
        ensure(
            effect.mutation_contract.degraded_code,
            None,
            &format!("{command} has no unavailable degraded code"),
        )?;

        Ok(())
    }

    #[test]
    fn manifest_classifies_mesh_lane_mutations_as_audited_durable_writes() -> TestResult {
        let manifest = EffectManifest::build();
        for command in ["mesh grant", "mesh revoke-lane"] {
            let effect = manifest
                .get(command)
                .ok_or_else(|| format!("{command} not found"))?;
            ensure(
                effect.default_effect,
                EffectClass::DurableMemoryWrite,
                &format!("{command} is a durable write"),
            )?;
            ensure(
                effect.requires_audit,
                true,
                &format!("{command} requires audit"),
            )?;
            ensure(
                effect.write_surfaces.db_tables.clone(),
                vec!["mesh_lane_grant_states", "audit_log"],
                &format!("{command} declares exact write surfaces"),
            )?;
        }
        Ok(())
    }

    #[test]
    fn manifest_tracks_demo_run_as_audited_external_io() -> TestResult {
        let manifest = EffectManifest::build();
        let effect = manifest
            .get("demo run")
            .ok_or_else(|| "demo run not found".to_owned())?;

        ensure(
            effect.default_effect,
            EffectClass::ExternalIo,
            "demo run executes manifest commands",
        )?;
        ensure(
            effect.dry_run_effect,
            Some(EffectClass::ReadOnly),
            "demo run --dry-run is read-only",
        )?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::AuditedMutation,
            "demo run writes audit rows",
        )?;
        ensure(
            effect.write_surfaces.db_tables.contains(&"audit_log"),
            true,
            "demo run writes audit_log",
        )?;
        ensure(
            effect.write_surfaces.workspace_files.is_empty(),
            false,
            "demo run names evidence/artifact write surfaces",
        )?;
        ensure(
            effect.mutation_contract.degraded_code,
            None,
            "demo run has no unavailable sentinel",
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
    fn db_backed_read_only_commands_declare_read_snapshot_requirement() -> TestResult {
        let manifest = EffectManifest::build();
        let db_backed = [
            "context",
            "orient",
            "search",
            "why",
            "status",
            "doctor",
            "memory drift",
            "memory list",
            "memory show",
            "pack replay",
            "graph pagerank",
            "db status",
            "audit timeline",
            "curate candidates",
            "diag provenance",
            "trust report",
            "swarm brief",
        ];

        for command in db_backed {
            let effect = manifest
                .get(command)
                .ok_or_else(|| format!("{command} not found"))?;
            ensure(
                effect.default_effect,
                EffectClass::ReadOnly,
                &format!("{command} remains read-only"),
            )?;
            ensure(
                effect.read_snapshot(),
                true,
                &format!("{command} declares read snapshot requirement"),
            )?;
        }

        for command in ["help", "version", "completion"] {
            let effect = manifest
                .get(command)
                .ok_or_else(|| format!("{command} not found"))?;
            ensure(
                effect.read_snapshot(),
                false,
                &format!("{command} does not require a DB read snapshot"),
            )?;
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
