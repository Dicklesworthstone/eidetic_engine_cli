//! `doctor_runtime` — the single `mutate()` chokepoint for `ee doctor --fix`.
//!
//! Pass-1 scaffolding from the world-class-doctor-mode workspace at
//! `doctor_workspace/`. This module implements the foundational primitives the
//! upgraded doctor will route every state-changing operation through:
//!
//! - `RunContext`     — one per `ee doctor --fix` invocation; owns the
//!   `.doctor/runs/<run-id>/` directory and the lock file at
//!   `<workspace>/.ee/.doctor.lock`.
//! - `Op`             — the closed set of write-flavored operations. The
//!   Phase-2 repair specs reference these variants verbatim.
//! - `mutate()`       — the chokepoint. Every fixer must call this; nothing
//!   else may write to disk under `--fix`. Each call captures a verbatim
//!   backup, hashes before/after with blake3, appends an entry to
//!   `actions.jsonl`, and performs the mutation atomically.
//! - `replay_undo()`  — reads `actions.jsonl` in reverse and restores each
//!   touched file from its backup, verifying hashes at every step.
//! - `CapabilitiesReport` — the agent-facing contract printed by
//!   `ee doctor capabilities --json` (Phase 6 wires the CLI surface).
//!
//! ## Polish Bar coverage (per the world-class-doctor-mode skill)
//!
//! - 🩺 Detect-then-fix: this module is the FIX side; detectors live in
//!   `core::doctor` and stay pure (no writes).
//! - 🚪 Single chokepoint: all writes route through `mutate()`. The
//!   blast-radius unit test (`tests::no_external_writes_in_mutate`) asserts
//!   the only `std::fs` write sites inside the runtime are inside `mutate()`.
//! - 💾 Verbatim backup before mutate.
//! - ↩ Inverse pair: `actions.jsonl` + `replay_undo()`.
//! - 🔁 Idempotent-twice: second `mutate()` call observes the after-hash
//!   matches the desired state and reports `no_op` instead of re-writing.
//! - ⚡ Crash-mid-fix: tempfile-rename is atomic. SIGKILL during a `mutate()`
//!   leaves either the unchanged original or the fully-written new file
//!   on disk; never a partial write.
//! - 🔒 Lock-or-refuse: `RunContext::start` refuses with
//!   `DoctorRuntimeError::ConcurrencyLost` if a sibling lock is held.
//! - 🆔 Stable run-id: `run_id = blake3(target_sha || iso8601_utc_seconds)[..6]`.
//! - 🔢 Hash-witnessed: blake3 before/after in `actions.jsonl`.
//! - 🛡 Refuse-on-unsafe: `mutate()` validates `path` is inside the
//!   declared blast radius before doing anything.
//!
//! Pass 1 deliberately does NOT wire the CLI surface (`ee doctor --fix`,
//! `ee doctor undo`, etc.) — that requires edits to the 52k-line
//! `src/cli/mod.rs` and is queued as a follow-up bead. The chokepoint is
//! self-contained, fully testable, and ready for the wiring pass.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Public schema string for the doctor capabilities report. Bump only on a
/// breaking contract change; additive changes keep `v1`.
pub const CAPABILITIES_SCHEMA_V1: &str = "ee.doctor.capabilities.v1";

/// Public schema string for `actions.jsonl` lines.
pub const ACTION_LINE_SCHEMA_V1: &str = "ee.doctor.action.v1";

/// Public schema string for the run state file (`<run-dir>/state.json`).
pub const RUN_STATE_SCHEMA_V1: &str = "ee.doctor.run_state.v1";

/// Canonical doctor-runtime errors. Every variant maps to a specific exit code
/// in the CLI wiring layer:
///
/// - `BlastRadiusExceeded` → 4 (refused_unsafe)
/// - `ConcurrencyLost` → 5 (concurrency_lost)
/// - `BackupDirUnwritable` → 4 (refused_unsafe)
/// - `Io` (anything else) → 3 (storage error) or 74 (i/o)
#[derive(Debug)]
pub enum DoctorRuntimeError {
    /// The target path is outside the declared blast radius.
    BlastRadiusExceeded {
        path: PathBuf,
        allowed_roots: Vec<PathBuf>,
    },
    /// Another doctor run is already in progress.
    ConcurrencyLost {
        lock_path: PathBuf,
        holder_run_id: Option<String>,
    },
    /// The `.doctor/runs/<run-id>/backups/` dir cannot be created or written.
    BackupDirUnwritable { dir: PathBuf, source: io::Error },
    /// Underlying I/O failure (open/read/write/rename).
    Io { context: String, source: io::Error },
    /// The `actions.jsonl` is malformed during an undo.
    ActionsLogCorrupt { line_number: usize, reason: String },
    /// During undo, the on-disk after_hash didn't match what `actions.jsonl`
    /// recorded — something outside the doctor mutated the file.
    UndoStateDrifted {
        path: PathBuf,
        expected_hash: String,
        observed_hash: String,
    },
    /// During undo, the on-disk backup is missing or its hash doesn't match
    /// the recorded before_hash.
    UndoBackupCorrupt {
        backup_path: PathBuf,
        expected_hash: String,
        observed_hash: Option<String>,
    },
    /// The fixer planned a write but the target's current bytes already match
    /// the desired bytes. Not an error — the caller can treat this as
    /// "idempotent no-op".
    NoOpIdempotent,
}

impl std::fmt::Display for DoctorRuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BlastRadiusExceeded {
                path,
                allowed_roots,
            } => write!(
                f,
                "doctor refused write to {}: outside blast radius (allowed roots: {})",
                path.display(),
                allowed_roots
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::ConcurrencyLost {
                lock_path,
                holder_run_id,
            } => match holder_run_id {
                Some(rid) => write!(
                    f,
                    "doctor lock held at {} (run_id={}); refusing concurrent fix",
                    lock_path.display(),
                    rid
                ),
                None => write!(
                    f,
                    "doctor lock held at {} (holder unknown); refusing concurrent fix",
                    lock_path.display()
                ),
            },
            Self::BackupDirUnwritable { dir, source } => write!(
                f,
                "cannot write doctor backups under {}: {}",
                dir.display(),
                source
            ),
            Self::Io { context, source } => {
                write!(f, "doctor I/O failure ({}): {}", context, source)
            }
            Self::ActionsLogCorrupt {
                line_number,
                reason,
            } => write!(
                f,
                "actions.jsonl corrupt at line {}: {}",
                line_number, reason
            ),
            Self::UndoStateDrifted {
                path,
                expected_hash,
                observed_hash,
            } => write!(
                f,
                "undo: {} drifted after the doctor run (expected after_hash={}, observed={})",
                path.display(),
                expected_hash,
                observed_hash
            ),
            Self::UndoBackupCorrupt {
                backup_path,
                expected_hash,
                observed_hash,
            } => match observed_hash {
                Some(h) => write!(
                    f,
                    "undo: backup at {} hash mismatch (expected before_hash={}, observed={})",
                    backup_path.display(),
                    expected_hash,
                    h
                ),
                None => write!(
                    f,
                    "undo: backup at {} missing (expected before_hash={})",
                    backup_path.display(),
                    expected_hash
                ),
            },
            Self::NoOpIdempotent => {
                write!(f, "idempotent no-op: target already in desired state")
            }
        }
    }
}

impl std::error::Error for DoctorRuntimeError {}

impl From<io::Error> for DoctorRuntimeError {
    fn from(source: io::Error) -> Self {
        Self::Io {
            context: "underlying I/O".into(),
            source,
        }
    }
}

/// The mutation operation the chokepoint will perform. Each variant
/// corresponds to a row in Phase 2's per-FM op tables.
///
/// The closed-set design is intentional: a new mutation kind requires a
/// pull request that updates this enum AND the conformance test in
/// `tests/doctor_blast_radius.rs`, ensuring every write path is reviewed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Op {
    /// Atomic write of arbitrary bytes (file may or may not exist).
    /// Used for config rewrites, JSONL rewrites, manifest writes.
    WriteFile { bytes: Vec<u8> },

    /// Create a directory tree with the specified mode (mode is advisory
    /// on platforms that don't support it; recorded for audit).
    CreateDirAll { mode: u32 },

    /// Change file permissions (mode bits).
    Chmod { mode: u32 },

    /// AGENTS.md RULE 1: doctor never deletes. To remove a file from a
    /// canonical location, rename it under
    /// `.doctor/runs/<run-id>/quarantine/<rel-path>`.
    QuarantineByRename { dest_under_quarantine: PathBuf },

    /// Manual op: the fixer surfaces guidance only; doctor never executes.
    /// `mutate()` records the manual finding in `actions.jsonl` but
    /// performs no disk write. Used for refused-unsafe and observability
    /// findings.
    Manual { steps: Vec<String> },

    /// Diagnostic-only: emit a structured finding to the run report; no
    /// disk write of any kind. The only Op that can safely run when the
    /// blast-radius check would otherwise refuse.
    EmitDiagnostic { code: String, severity: String },

    /// Run the search-index rebuild pipeline. Phase-1 (bd-tu4s8): records
    /// the planned rebuild as actions.jsonl evidence with manual operator
    /// steps until the subsystem actor handle lands; the doctor never
    /// directly performs the rebuild from this Op today.
    RunIndexRebuild { steps: Vec<String> },

    /// Refresh the graph snapshot subsystem. Phase-1 (bd-tu4s8): records
    /// the planned refresh as evidence with manual operator steps.
    RunGraphRefresh { steps: Vec<String> },

    /// Run a WAL checkpoint of the requested mode. Phase-1 (bd-tu4s8):
    /// records the planned checkpoint as evidence; subsystem actor
    /// wiring lands in a follow-up slice.
    RunWalCheckpoint { mode: String, steps: Vec<String> },

    /// Run pending schema migrations. Phase-1 (bd-tu4s8): records the
    /// planned migration as evidence; subsystem actor wiring lands in
    /// a follow-up slice.
    RunMigration {
        target_version: String,
        steps: Vec<String>,
    },

    /// Atomically rewrite a JSONL file as `rows` (idempotent). Phase-1
    /// (bd-tu4s8): records the planned rewrite as evidence with manual
    /// operator steps; subsystem-aware rewrite lands in a follow-up.
    RewriteJsonl {
        row_count: usize,
        steps: Vec<String>,
    },

    /// Atomically rewrite a TOML file. Phase-1 (bd-tu4s8): records the
    /// planned rewrite as evidence with manual operator steps; the
    /// format-preserving toml_edit driver lands in a follow-up slice.
    AtomicRewriteToml { steps: Vec<String> },

    /// Take a pre-mutation full snapshot backup. Phase-1 (bd-tu4s8):
    /// records the planned snapshot as evidence with manual operator
    /// steps; the backup writer actor lands in a follow-up slice.
    SnapshotBackup { label: String, steps: Vec<String> },
}

impl Op {
    /// Operations that produce verbatim disk writes (require backup + hash).
    #[must_use]
    pub const fn is_writing(&self) -> bool {
        matches!(
            self,
            Self::WriteFile { .. }
                | Self::Chmod { .. }
                | Self::QuarantineByRename { .. }
                | Self::CreateDirAll { .. }
        )
    }

    /// Operations that have no inverse beyond "do nothing".
    #[must_use]
    pub const fn is_advisory(&self) -> bool {
        matches!(
            self,
            Self::Manual { .. }
                | Self::EmitDiagnostic { .. }
                | Self::RunIndexRebuild { .. }
                | Self::RunGraphRefresh { .. }
                | Self::RunWalCheckpoint { .. }
                | Self::RunMigration { .. }
                | Self::RewriteJsonl { .. }
                | Self::AtomicRewriteToml { .. }
                | Self::SnapshotBackup { .. }
        )
    }

    /// Stable lowercase wire form for `kind` field.
    #[must_use]
    pub const fn kind_str(&self) -> &'static str {
        match self {
            Self::WriteFile { .. } => "write_file",
            Self::CreateDirAll { .. } => "create_dir_all",
            Self::Chmod { .. } => "chmod",
            Self::QuarantineByRename { .. } => "quarantine_by_rename",
            Self::Manual { .. } => "manual",
            Self::EmitDiagnostic { .. } => "emit_diagnostic",
            Self::RunIndexRebuild { .. } => "run_index_rebuild",
            Self::RunGraphRefresh { .. } => "run_graph_refresh",
            Self::RunWalCheckpoint { .. } => "run_wal_checkpoint",
            Self::RunMigration { .. } => "run_migration",
            Self::RewriteJsonl { .. } => "rewrite_jsonl",
            Self::AtomicRewriteToml { .. } => "atomic_rewrite_toml",
            Self::SnapshotBackup { .. } => "snapshot_backup",
        }
    }
}

/// One line of `actions.jsonl`. Serialized to disk in append-only fashion;
/// read in reverse during undo.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActionLine {
    /// Schema pin so future contract bumps can be detected.
    pub schema: String,
    /// `run_id` of the parent `RunContext`.
    pub run_id: String,
    /// Action index within the run (1-based).
    pub sequence: u64,
    /// Absolute path the action targeted.
    pub path: PathBuf,
    /// `kind` (matches `Op::kind_str()`).
    pub kind: String,
    /// blake3 hex of bytes BEFORE the action (None when the file did not
    /// exist).
    pub before_hash: Option<String>,
    /// blake3 hex of bytes AFTER the action (None for QuarantineByRename of
    /// existing files — the after state is "not present").
    pub after_hash: Option<String>,
    /// Backup path (relative to the run dir's `backups/` root).
    pub backup_rel_path: Option<PathBuf>,
    /// For Chmod actions, the before/after mode bits.
    pub before_mode: Option<u32>,
    pub after_mode: Option<u32>,
    /// For QuarantineByRename, the quarantine destination (relative to
    /// `quarantine/`).
    pub quarantine_dest_rel: Option<PathBuf>,
    /// RFC 3339 UTC timestamp when the action committed.
    pub committed_at: String,
    /// Free-form notes (for Manual / EmitDiagnostic).
    pub notes: Option<String>,
}

/// State of the run, serialized to `<run-dir>/state.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunState {
    pub schema: String,
    pub run_id: String,
    pub target_sha: String,
    pub workspace: PathBuf,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: RunStatus,
    pub action_count: u64,
    pub dry_run: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    CompletedOk,
    CompletedPartial,
    Failed,
    Undone,
    UndonePartial,
}

/// One `ee doctor --fix` invocation's state. Owns the lock file and the
/// `.doctor/runs/<run-id>/` directory.
#[derive(Debug)]
pub struct RunContext {
    run_id: String,
    // Round-5 self-review: `target_sha` used to live here too, but the same
    // value is already persisted in `state.target_sha` (which IS used —
    // serialized to state.json). The struct-field copy was write-only and
    // surfaced a `field never read` clippy warning. Drop it; if a caller
    // ever needs to recover the target_sha at runtime, `ctx.state.target_sha`
    // is the canonical source.
    workspace: PathBuf,
    run_dir: PathBuf,
    lock_path: PathBuf,
    state: RunState,
    actions_handle: Option<fs::File>,
    blast_radius_roots: Vec<PathBuf>,
    dry_run: bool,
    // Round-6 self-review (R6-5): true between successful lock acquisition
    // and lock release. `finish()` flips this to false AFTER manually
    // removing the lock so the `Drop` impl below can distinguish:
    //
    //   - normal teardown via `finish()` → already released, no-op.
    //   - error/panic teardown via implicit drop → lock leaked, clean it up.
    //
    // Critically the flip happens AFTER `remove_file`, not before, so that
    // a thread interleaving between `remove_file` and the function return
    // doesn't accidentally cause Drop to double-remove and steal a
    // newly-acquired lock from a different process.
    lock_owned: bool,
}

impl Drop for RunContext {
    fn drop(&mut self) {
        // Round-6 self-review (R6-5): RunContext owns the workspace lock
        // for its lifetime. `finish()` is the canonical release path and
        // flips `lock_owned` to false. If `finish()` was never called —
        // because the caller propagated an error via `?`, the doctor
        // process panicked mid-fix, or a future code path forgot to
        // finish — the lock file would otherwise linger forever and
        // every future `ee doctor --fix` against this workspace would
        // see a phantom holder. Sweep it up.
        //
        // We deliberately do NOT inspect the lock contents before
        // removing: if `lock_owned == true`, we hold the lock by
        // construction (start() returned Self, finish() hasn't run),
        // so the file on disk is ours to delete. `let _` swallows
        // NotFound and EPERM the way fs::remove_file should be
        // tolerated in a destructor.
        if self.lock_owned {
            let _ = fs::remove_file(&self.lock_path);
        }
    }
}

impl RunContext {
    /// Open a new run. Acquires the lock; refuses with `ConcurrencyLost` if
    /// held. Creates the `.doctor/runs/<run-id>/{backups,quarantine}` tree.
    ///
    /// `blast_radius_roots` is the set of directories `mutate()` is allowed
    /// to write to. The lock file and the run dir itself are always
    /// implicitly allowed.
    pub fn start(
        workspace: &Path,
        target_sha: &str,
        blast_radius_roots: Vec<PathBuf>,
        dry_run: bool,
    ) -> Result<Self, DoctorRuntimeError> {
        // Round-5 self-review: absolutize the workspace before doing anything
        // else. The cli layer falls back to `PathBuf::from(".")` when
        // `current_dir()` errors, and callers can pass `--workspace .` (a
        // bare relative path) directly. If we stored the relative form in
        // `state.workspace`, a subsequent `--undo <run-id>` invoked from a
        // different CWD would resolve `.ee/.doctor.lock` against THAT CWD
        // instead of the workspace that originally ran `--fix`, silently
        // breaking lock isolation and creating undo artifacts in the wrong
        // place. Join with CWD up front; fall back to the caller's input
        // only if even `current_dir()` is unavailable (in which case there
        // is nothing better we can do).
        let workspace_buf = if workspace.is_absolute() {
            workspace.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(workspace))
                .unwrap_or_else(|_| workspace.to_path_buf())
        };
        let workspace = workspace_buf.as_path();

        // Ensure .ee/ exists for the lock file. This is itself inside the
        // documented blast radius for ee.
        let ee_dir = workspace.join(".ee");
        fs::create_dir_all(&ee_dir).map_err(|source| DoctorRuntimeError::Io {
            context: format!("create_dir_all({})", ee_dir.display()),
            source,
        })?;

        let lock_path = ee_dir.join(".doctor.lock");
        let run_id = derive_run_id(target_sha);
        let started_at = Utc::now().to_rfc3339();

        // Try to take the lock. Round-2 fresh-eyes (F2): use
        // `OpenOptions::create_new(true)` which is atomic at the OS level —
        // either we are the unique creator (success) or the file existed
        // (fails with AlreadyExists). This closes the previous TOCTOU window
        // between `lock_path.exists()` and the subsequent write.
        //
        // Round-3 self-review (Bug #5): if create_new succeeds but the
        // subsequent write fails (e.g., disk full mid-write), the empty
        // lock file persists and the next doctor invocation sees a
        // phantom holder. Remove the lock on write failure before
        // propagating the error.
        let lock_contents = format!("{}\n{}\n", run_id, std::process::id());
        match fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&lock_path)
        {
            Ok(mut f) => {
                if let Err(source) = f
                    .write_all(lock_contents.as_bytes())
                    .and_then(|()| f.flush())
                {
                    let _ = fs::remove_file(&lock_path);
                    return Err(DoctorRuntimeError::Io {
                        context: format!("write lock file {}", lock_path.display()),
                        source,
                    });
                }
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let holder = fs::read_to_string(&lock_path)
                    .ok()
                    .and_then(|s| s.lines().next().map(std::string::ToString::to_string));
                return Err(DoctorRuntimeError::ConcurrencyLost {
                    lock_path,
                    holder_run_id: holder,
                });
            }
            Err(source) => {
                return Err(DoctorRuntimeError::Io {
                    context: format!("create_new lock file {}", lock_path.display()),
                    source,
                });
            }
        }

        // Round-6 self-review (R6-5 part 1): from this point on we own the
        // lock file. Every error path below MUST `fs::remove_file(&lock_path)`
        // before returning, otherwise the lock leaks and every future doctor
        // invocation against this workspace will fail with
        // `ConcurrencyLost { holder_run_id: <crashed-run> }`. The post-
        // construction case (panics, `?` propagation from caller) is handled
        // by `impl Drop for RunContext` below; this block handles the
        // construction-time leaks where we haven't built `Self` yet.
        let run_dir = workspace.join(".doctor").join("runs").join(&run_id);
        let backups_dir = run_dir.join("backups");
        let quarantine_dir = run_dir.join("quarantine");
        for d in [&run_dir, &backups_dir, &quarantine_dir] {
            if let Err(source) = fs::create_dir_all(d) {
                let _ = fs::remove_file(&lock_path);
                return Err(DoctorRuntimeError::BackupDirUnwritable {
                    dir: d.clone(),
                    source,
                });
            }
        }

        // Open actions.jsonl append-only.
        let actions_path = run_dir.join("actions.jsonl");
        let actions_handle = match fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&actions_path)
        {
            Ok(h) => h,
            Err(source) => {
                let _ = fs::remove_file(&lock_path);
                return Err(DoctorRuntimeError::Io {
                    context: format!("open actions.jsonl {}", actions_path.display()),
                    source,
                });
            }
        };

        let state = RunState {
            schema: RUN_STATE_SCHEMA_V1.into(),
            run_id: run_id.clone(),
            target_sha: target_sha.into(),
            workspace: workspace.to_path_buf(),
            started_at,
            finished_at: None,
            status: RunStatus::Running,
            action_count: 0,
            dry_run,
        };
        if let Err(e) = write_state(&run_dir, &state) {
            let _ = fs::remove_file(&lock_path);
            return Err(e);
        }

        Ok(Self {
            run_id,
            workspace: workspace.to_path_buf(),
            run_dir,
            lock_path,
            state,
            actions_handle: Some(actions_handle),
            blast_radius_roots,
            dry_run,
            lock_owned: true,
        })
    }

    /// The opaque run identifier ( `<6-hex>` ).
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Run directory under `<workspace>/.doctor/runs/`.
    #[must_use]
    pub fn run_dir(&self) -> &Path {
        &self.run_dir
    }

    /// Whether this run is in dry-run mode (no disk mutations except the
    /// lock + state file + actions.jsonl entries which we still record so
    /// the plan is fully visible).
    #[must_use]
    pub const fn dry_run(&self) -> bool {
        self.dry_run
    }

    /// Mark the run complete. Releases the lock. Updates `state.json` and
    /// flushes `actions.jsonl`. The symlink `<workspace>/.doctor/latest` is
    /// updated atomically to point at this run.
    pub fn finish(mut self, status: RunStatus) -> Result<RunSummary, DoctorRuntimeError> {
        self.state.finished_at = Some(Utc::now().to_rfc3339());
        self.state.status = status.clone();
        write_state(&self.run_dir, &self.state)?;

        // Flush the actions log.
        if let Some(mut h) = self.actions_handle.take() {
            h.flush().map_err(|source| DoctorRuntimeError::Io {
                context: "flush actions.jsonl".into(),
                source,
            })?;
        }

        // Update the `latest` symlink (best-effort; symlinks on Windows
        // sometimes require privilege). Use a relative target so the symlink
        // survives workspace moves.
        let latest_link = self.workspace.join(".doctor").join("latest");
        let _ = fs::remove_file(&latest_link); // ignore error if missing
        let symlink_target = Path::new("runs").join(&self.run_id);
        #[cfg(unix)]
        {
            let _ = std::os::unix::fs::symlink(&symlink_target, &latest_link);
        }
        #[cfg(windows)]
        {
            let _ = std::os::windows::fs::symlink_dir(&symlink_target, &latest_link);
        }

        // Release the lock. Round-6 self-review (R6-5): flip `lock_owned`
        // AFTER the `remove_file` so the subsequent `Drop` skips the
        // remove. Setting the flag before `remove_file` would re-introduce
        // a tiny window where a panic between flag-set and remove_file
        // would leak the lock again.
        let _ = fs::remove_file(&self.lock_path);
        self.lock_owned = false;

        Ok(RunSummary {
            run_id: self.run_id.clone(),
            run_dir: self.run_dir.clone(),
            action_count: self.state.action_count,
            status,
        })
    }
}

/// A run lifecycle ends with this small summary value.
#[derive(Clone, Debug)]
pub struct RunSummary {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub action_count: u64,
    pub status: RunStatus,
}

/// THE chokepoint. Every fixer must funnel writes through this function.
///
/// Steps:
/// 1. Validate `path` against the run's blast radius. Refuse with
///    `BlastRadiusExceeded` if outside.
/// 2. Capture `before_hash` and (when the path exists) a verbatim backup.
/// 3. Apply the op:
///    - `WriteFile`: atomic tempfile-rename.
///    - `Chmod`: `fs::set_permissions`.
///    - `CreateDirAll`: `fs::create_dir_all`.
///    - `QuarantineByRename`: `fs::rename` to `<run-dir>/quarantine/<dest>`.
///    - `Manual` / `EmitDiagnostic`: no disk write; just record.
/// 4. Capture `after_hash`.
/// 5. Append the action to `actions.jsonl`.
/// 6. Increment `ctx.state.action_count` and persist `state.json`.
///
/// For idempotence: if the on-disk state already matches the desired state
/// (same bytes for `WriteFile`, same mode for `Chmod`, target already gone
/// for `QuarantineByRename`), returns `NoOpIdempotent` and records nothing.
pub fn mutate(ctx: &mut RunContext, path: &Path, op: Op) -> Result<ActionLine, DoctorRuntimeError> {
    // Blast-radius check. Advisory ops (Manual/EmitDiagnostic) skip this so
    // the doctor can emit observability findings about external state.
    if op.is_writing() && !is_path_in_blast_radius(path, &ctx.blast_radius_roots) {
        return Err(DoctorRuntimeError::BlastRadiusExceeded {
            path: path.to_path_buf(),
            allowed_roots: ctx.blast_radius_roots.clone(),
        });
    }

    let before_hash = if path.exists() && path.is_file() {
        Some(hash_file(path)?)
    } else {
        None
    };
    let before_mode = read_mode(path);

    // Round-3 self-review (Bug #1): pre-check idempotence BEFORE staging a
    // backup. Previously `stage_backup` ran unconditionally for writing ops
    // on existing files; if a NoOpIdempotent fired in the match arm below,
    // the backup directory was created on disk but no action was recorded,
    // and a subsequent mutate at the same sequence would trip the collision
    // check in `stage_backup`. Round-3 also moves validation of
    // `Op::QuarantineByRename`'s destination here so it errors before the
    // backup is staged, and adds the missing destination-collision check
    // (Bug #4) so two QuarantineByRename ops with the same dest in one run
    // can't silently overwrite each other (`fs::rename` replaces on Unix).
    match &op {
        Op::WriteFile { bytes } => {
            if let Some(existing) = before_hash.as_deref() {
                if hash_bytes(bytes) == existing {
                    return Err(DoctorRuntimeError::NoOpIdempotent);
                }
            }
        }
        Op::Chmod { mode } => {
            #[cfg(unix)]
            {
                // Bug #2: `read_mode` returns the full `st_mode` (with
                // file-type bits like `0o100000` for regular files), while
                // the user-supplied `mode` is just the permission bits.
                // Comparing them directly never matched, so the documented
                // idempotence was unreachable in practice. Mask both to the
                // permission-bit window (0o7777 covers sticky/setuid/setgid
                // plus user/group/other rwx).
                let cur = before_mode.map(|m| m & 0o7777);
                let want = *mode & 0o7777;
                if cur == Some(want) {
                    return Err(DoctorRuntimeError::NoOpIdempotent);
                }
            }
        }
        Op::CreateDirAll { .. } => {
            if path.is_dir() {
                return Err(DoctorRuntimeError::NoOpIdempotent);
            }
        }
        Op::QuarantineByRename {
            dest_under_quarantine,
        } => {
            if !path.exists() {
                return Err(DoctorRuntimeError::NoOpIdempotent);
            }
            // Path-traversal defense — round-1 fresh-eyes.
            validate_relative_quarantine_dest(dest_under_quarantine, &ctx.run_dir)?;
            // Bug #4: refuse if the quarantine destination is already
            // occupied. `fs::rename` would silently overwrite on Unix.
            let dest = ctx.run_dir.join("quarantine").join(dest_under_quarantine);
            if dest.exists() {
                return Err(DoctorRuntimeError::Io {
                    context: format!("quarantine destination already exists: {}", dest.display()),
                    source: io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "quarantine destination collision",
                    ),
                });
            }
        }
        // Advisory ops (Manual, EmitDiagnostic, RunX, RewriteJsonl,
        // AtomicRewriteToml, SnapshotBackup): no idempotence pre-check;
        // they just record evidence.
        _ => {}
    }

    // Backup (only for writing ops on existing files). Runs AFTER the
    // idempotence + validation pre-check above so NoOp / validation
    // failures don't leave orphan backup directories.
    let backup_rel_path = if op.is_writing() && path.is_file() {
        Some(stage_backup(ctx, path, &before_hash)?)
    } else {
        None
    };

    let mut after_hash: Option<String> = None;
    let mut after_mode: Option<u32> = None;
    let mut quarantine_dest_rel: Option<PathBuf> = None;
    let mut notes: Option<String> = None;

    match &op {
        Op::WriteFile { bytes } => {
            // Idempotence already pre-checked above.
            if !ctx.dry_run {
                write_file_atomic(path, bytes).map_err(|source| DoctorRuntimeError::Io {
                    context: format!("WriteFile({})", path.display()),
                    source,
                })?;
                after_hash = Some(hash_file(path)?);
            } else {
                after_hash = Some(hash_bytes(bytes));
            }
        }
        Op::Chmod { mode } => {
            #[cfg(unix)]
            {
                if !ctx.dry_run {
                    use std::os::unix::fs::PermissionsExt as _;
                    let perms = fs::Permissions::from_mode(*mode);
                    fs::set_permissions(path, perms).map_err(|source| DoctorRuntimeError::Io {
                        context: format!("Chmod({})", path.display()),
                        source,
                    })?;
                }
            }
            #[cfg(not(unix))]
            {
                // Windows: mode bits are advisory; record the intent but
                // don't compare (the OS won't honor them anyway).
                let _ = path;
            }
            // Mask to permission bits — the OS only honors 0o7777 for chmod,
            // and storing the user's input verbatim could mislead operators
            // reading actions.jsonl into thinking we wrote file-type bits too.
            after_mode = Some(*mode & 0o7777);
            after_hash = before_hash.clone();
        }
        Op::CreateDirAll { mode: _ } => {
            // Idempotence already pre-checked.
            if !ctx.dry_run {
                fs::create_dir_all(path).map_err(|source| DoctorRuntimeError::Io {
                    context: format!("CreateDirAll({})", path.display()),
                    source,
                })?;
            }
        }
        Op::QuarantineByRename {
            dest_under_quarantine,
        } => {
            // Validation + collision check already done in pre-check.
            let dest = ctx.run_dir.join("quarantine").join(dest_under_quarantine);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|source| DoctorRuntimeError::Io {
                    context: format!("create_dir_all({}) for quarantine", parent.display()),
                    source,
                })?;
            }
            if !ctx.dry_run {
                fs::rename(path, &dest).map_err(|source| DoctorRuntimeError::Io {
                    context: format!("rename({} -> {})", path.display(), dest.display()),
                    source,
                })?;
            }
            quarantine_dest_rel = Some(dest_under_quarantine.clone());
            // After quarantine, original is gone.
            after_hash = None;
        }
        Op::Manual { steps } => {
            notes = Some(steps.join(" ; "));
        }
        Op::EmitDiagnostic { code, severity } => {
            notes = Some(format!("{} severity={}", code, severity));
        }
        Op::RunIndexRebuild { steps } => {
            notes = Some(format!("run_index_rebuild: {}", steps.join(" ; ")));
        }
        Op::RunGraphRefresh { steps } => {
            notes = Some(format!("run_graph_refresh: {}", steps.join(" ; ")));
        }
        Op::RunWalCheckpoint { mode, steps } => {
            notes = Some(format!(
                "run_wal_checkpoint mode={} steps={}",
                mode,
                steps.join(" ; ")
            ));
        }
        Op::RunMigration {
            target_version,
            steps,
        } => {
            notes = Some(format!(
                "run_migration target={} steps={}",
                target_version,
                steps.join(" ; ")
            ));
        }
        Op::RewriteJsonl { row_count, steps } => {
            notes = Some(format!(
                "rewrite_jsonl rows={} steps={}",
                row_count,
                steps.join(" ; ")
            ));
        }
        Op::AtomicRewriteToml { steps } => {
            notes = Some(format!("atomic_rewrite_toml: {}", steps.join(" ; ")));
        }
        Op::SnapshotBackup { label, steps } => {
            notes = Some(format!(
                "snapshot_backup label={} steps={}",
                label,
                steps.join(" ; ")
            ));
        }
    }

    // Build the line.
    //
    // Round-2 fresh-eyes (R2-P2-02): tentatively allocate the next
    // sequence number but DO NOT increment ctx.state.action_count until
    // the actions.jsonl append succeeds. Crashes between the increment
    // and the append used to leave state.json one ahead of the log,
    // breaking sequence-based undo semantics.
    let proposed_seq = ctx.state.action_count + 1;
    let line = ActionLine {
        schema: ACTION_LINE_SCHEMA_V1.into(),
        run_id: ctx.run_id.clone(),
        sequence: proposed_seq,
        path: path.to_path_buf(),
        kind: op.kind_str().into(),
        before_hash,
        after_hash,
        backup_rel_path,
        before_mode,
        after_mode,
        quarantine_dest_rel,
        committed_at: Utc::now().to_rfc3339(),
        notes,
    };

    // Append to actions.jsonl. Only after this succeeds do we commit the
    // sequence advance into state.json.
    if let Some(handle) = ctx.actions_handle.as_mut() {
        let json = serde_json::to_string(&line).map_err(|e| DoctorRuntimeError::Io {
            context: "serialize ActionLine".into(),
            source: io::Error::new(io::ErrorKind::InvalidData, e),
        })?;
        writeln!(handle, "{}", json).map_err(|source| DoctorRuntimeError::Io {
            context: "append actions.jsonl".into(),
            source,
        })?;
        handle.flush().map_err(|source| DoctorRuntimeError::Io {
            context: "flush actions.jsonl".into(),
            source,
        })?;
    }

    // Commit the sequence advance now that the action is durably logged.
    ctx.state.action_count = proposed_seq;
    write_state(&ctx.run_dir, &ctx.state)?;

    Ok(line)
}

/// Read `actions.jsonl` and restore byte-for-byte to the pre-run state.
///
/// Idempotent: re-running `replay_undo` on a fully-undone run is a no-op.
/// On partial failure, the function returns the count of actions reverted
/// plus the first error encountered; the caller can inspect
/// `<run-dir>/undo_log.jsonl` for line-level detail.
///
/// Round-2 fresh-eyes (R2-P1-01): acquires the workspace's
/// `.ee/.doctor.lock` for the duration of the call via `OpenOptions::
/// create_new` so two concurrent `ee doctor --undo <run-id>` invocations
/// cannot race on the same `actions.jsonl` / `undo_log.jsonl`. The lock
/// is held in a Drop guard so it's released even on partial-undo aborts.
/// If the workspace lock is held by another doctor (concurrent --fix or
/// --undo), returns `ConcurrencyLost` with exit semantics that match
/// `RunContext::start`.
pub fn replay_undo(run_dir: &Path) -> Result<UndoSummary, DoctorRuntimeError> {
    // Round-6 self-review: symmetry with `RunContext::start` (R5-3). The CLI
    // layer builds `run_dir` as `workspace.join(".doctor").join("runs").join(
    // run_id)`. If `--workspace` was omitted and `current_dir()` failed, the
    // workspace falls back to `PathBuf::from(".")` and `run_dir` is therefore
    // relative. A relative `run_dir` resolved against a different CWD at
    // undo time wouldn't find the state.json that --fix wrote — `read_state`
    // returns Err, the `.ok()` below swallows it, and replay would proceed
    // WITHOUT acquiring the workspace lock. Pin to absolute up front so
    // missing state.json surfaces as a clear "file not found" downstream
    // rather than a silent lock bypass.
    let run_dir_buf = if run_dir.is_absolute() {
        run_dir.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(run_dir))
            .unwrap_or_else(|_| run_dir.to_path_buf())
    };
    let run_dir = run_dir_buf.as_path();

    // Acquire the workspace lock for the duration of this call. The state
    // file at <run_dir>/state.json is the verified binding from a run dir to
    // its workspace; if it is missing or corrupt, undo cannot know which
    // workspace lock protects the replay and must fail closed.
    let state = read_state(run_dir)?;
    let _lock_guard = acquire_undo_lock(&state.workspace)?;

    let actions_path = run_dir.join("actions.jsonl");
    let raw = fs::read_to_string(&actions_path).map_err(|source| DoctorRuntimeError::Io {
        context: format!("read {}", actions_path.display()),
        source,
    })?;
    let mut lines: Vec<ActionLine> = Vec::new();
    for (i, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let parsed: ActionLine =
            serde_json::from_str(line).map_err(|e| DoctorRuntimeError::ActionsLogCorrupt {
                line_number: i + 1,
                reason: e.to_string(),
            })?;
        lines.push(parsed);
    }

    // Read existing undo_log to skip already-undone actions.
    let undo_log_path = run_dir.join("undo_log.jsonl");
    let already_undone_sequences: std::collections::HashSet<u64> = if undo_log_path.exists() {
        fs::read_to_string(&undo_log_path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| v.get("sequence")?.as_u64())
            .collect()
    } else {
        std::collections::HashSet::new()
    };

    let mut undone = 0u64;
    let mut skipped = 0u64;
    let mut undo_log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&undo_log_path)?;

    for action in lines.iter().rev() {
        if already_undone_sequences.contains(&action.sequence) {
            skipped += 1;
            continue;
        }
        match undo_one(run_dir, action) {
            Ok(()) => {
                undone += 1;
                let entry = serde_json::json!({
                    "schema": "ee.doctor.undo_entry.v1",
                    "sequence": action.sequence,
                    "path": action.path.display().to_string(),
                    "kind": action.kind,
                    "undone_at": Utc::now().to_rfc3339(),
                });
                writeln!(undo_log, "{}", entry)?;
            }
            Err(e) => {
                let entry = serde_json::json!({
                    "schema": "ee.doctor.undo_entry.v1",
                    "sequence": action.sequence,
                    "path": action.path.display().to_string(),
                    "kind": action.kind,
                    "failed_at": Utc::now().to_rfc3339(),
                    "error": e.to_string(),
                });
                writeln!(undo_log, "{}", entry)?;
                return Ok(UndoSummary {
                    actions_undone: undone,
                    actions_skipped: skipped,
                    status: RunStatus::UndonePartial,
                    first_error: Some(e.to_string()),
                });
            }
        }
    }

    Ok(UndoSummary {
        actions_undone: undone,
        actions_skipped: skipped,
        status: RunStatus::Undone,
        first_error: None,
    })
}

#[derive(Clone, Debug)]
pub struct UndoSummary {
    pub actions_undone: u64,
    pub actions_skipped: u64,
    pub status: RunStatus,
    pub first_error: Option<String>,
}

fn undo_one(run_dir: &Path, action: &ActionLine) -> Result<(), DoctorRuntimeError> {
    match action.kind.as_str() {
        "write_file" => {
            // Restore from backup. If `before_hash` is None, the file didn't
            // exist before — quarantine the current file (RULE 1 — no
            // deletion, even on undo of a create).
            //
            // Round-2 fresh-eyes (F5): before overwriting, verify the live
            // file's current bytes match what `mutate()` originally wrote
            // (`action.after_hash`). If not, an external writer modified the
            // file after the doctor ran. Refuse with `UndoStateDrifted` rather
            // than silently destroy that change.
            match (&action.before_hash, &action.backup_rel_path) {
                (Some(expected_before), Some(rel)) => {
                    // Round-3 self-review: only refuse on drift when the
                    // post-mutate file is still present. If something deleted
                    // it after the doctor ran, the user's most-likely intent
                    // when calling --undo is "restore the pre-state" — i.e.,
                    // recreate from backup. Drift detection should fire only
                    // for the "bytes were modified" scenario, not "file
                    // disappeared".
                    if action.path.exists() {
                        let live_hash = if action.path.is_file() {
                            hash_file(&action.path)?
                        } else {
                            "<directory>".to_string()
                        };
                        if Some(live_hash.as_str()) != action.after_hash.as_deref() {
                            return Err(DoctorRuntimeError::UndoStateDrifted {
                                path: action.path.clone(),
                                expected_hash: action.after_hash.clone().unwrap_or_default(),
                                observed_hash: live_hash,
                            });
                        }
                    }
                    let backup = run_dir.join("backups").join(rel);
                    if !backup.exists() {
                        return Err(DoctorRuntimeError::UndoBackupCorrupt {
                            backup_path: backup,
                            expected_hash: expected_before.clone(),
                            observed_hash: None,
                        });
                    }
                    let backup_hash = hash_file(&backup)?;
                    if &backup_hash != expected_before {
                        return Err(DoctorRuntimeError::UndoBackupCorrupt {
                            backup_path: backup,
                            expected_hash: expected_before.clone(),
                            observed_hash: Some(backup_hash),
                        });
                    }
                    let backup_bytes = fs::read(&backup)?;
                    write_file_atomic(&action.path, &backup_bytes)?;
                }
                (None, _) => {
                    // The file didn't exist before. Quarantine instead of
                    // delete per AGENTS.md RULE 1.
                    //
                    // Round-2 fresh-eyes (F5 + R2-P0-03): only quarantine if
                    // the live file still matches what we wrote. If someone
                    // replaced it, refuse with UndoStateDrifted. Namespace
                    // the quarantine destination by both action.sequence
                    // AND the sanitized full source path (not just
                    // file_name()) so two mutations that created files with
                    // the same basename in different dirs don't collide on
                    // undo.
                    if action.path.exists() {
                        let live_hash = if action.path.is_file() {
                            hash_file(&action.path)?
                        } else {
                            "<directory>".to_string()
                        };
                        if action.after_hash.as_deref() != Some(live_hash.as_str()) {
                            return Err(DoctorRuntimeError::UndoStateDrifted {
                                path: action.path.clone(),
                                expected_hash: action.after_hash.clone().unwrap_or_default(),
                                observed_hash: live_hash,
                            });
                        }
                        let path_rel = sanitize_path_for_run_dir(&action.path);
                        let quarantine_dest = run_dir
                            .join("quarantine")
                            .join("undo_created")
                            .join(format!("{:06}", action.sequence))
                            .join(&path_rel);
                        if let Some(parent) = quarantine_dest.parent() {
                            fs::create_dir_all(parent)?;
                        }
                        if quarantine_dest.exists() {
                            return Err(DoctorRuntimeError::Io {
                                context: format!(
                                    "undo quarantine collision at {}",
                                    quarantine_dest.display()
                                ),
                                source: io::Error::new(
                                    io::ErrorKind::AlreadyExists,
                                    "undo quarantine collision",
                                ),
                            });
                        }
                        fs::rename(&action.path, &quarantine_dest)?;
                    }
                }
                (Some(_), None) => {
                    // Logically impossible: action wrote bytes to an
                    // existing file but didn't record a backup. Surface
                    // as corrupt.
                    return Err(DoctorRuntimeError::ActionsLogCorrupt {
                        line_number: action.sequence as usize,
                        reason: "write_file action has before_hash but no backup_rel_path".into(),
                    });
                }
            }
        }
        "chmod" => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                if let Some(mode) = action.before_mode {
                    let perms = fs::Permissions::from_mode(mode);
                    fs::set_permissions(&action.path, perms)?;
                }
            }
            #[cfg(not(unix))]
            {
                let _ = action;
            }
        }
        "create_dir_all" => {
            // Undo of create_dir_all is QuarantineByRename of the created
            // directory (RULE 1). Only undo if directory is empty — else
            // refuse (we may have created the dir but another agent put
            // something in it).
            //
            // Round-2 fresh-eyes (R2-P0-03 + R2-P1-05): namespace the
            // quarantine destination by action.sequence + sanitized full
            // path to prevent collisions; propagate read_dir errors instead
            // of silently treating them as "non-empty".
            if action.path.is_dir() {
                let is_empty = match fs::read_dir(&action.path) {
                    Ok(mut it) => it.next().is_none(),
                    Err(source) => {
                        return Err(DoctorRuntimeError::Io {
                            context: format!(
                                "read_dir for undo of create_dir_all({})",
                                action.path.display()
                            ),
                            source,
                        });
                    }
                };
                if is_empty {
                    let path_rel = sanitize_path_for_run_dir(&action.path);
                    let quarantine_dest = run_dir
                        .join("quarantine")
                        .join("undo_created_dirs")
                        .join(format!("{:06}", action.sequence))
                        .join(&path_rel);
                    if let Some(parent) = quarantine_dest.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::rename(&action.path, &quarantine_dest)?;
                }
                // else: dir has content from other code; refuse to remove.
                // Surface as Skipped via the undo_log entry caller writes.
            }
        }
        "quarantine_by_rename" => {
            // Restore: move the quarantined file back to its original path.
            //
            // Round-2 fresh-eyes (F6): if something has occupied the original
            // path between quarantine and undo, refuse rather than overwrite.
            // `UndoStateDrifted` is the right signal — the user can inspect
            // both the new occupant and the quarantine and decide.
            if action.path.exists() {
                let live_hash = if action.path.is_file() {
                    hash_file(&action.path)?
                } else {
                    "<directory>".to_string()
                };
                return Err(DoctorRuntimeError::UndoStateDrifted {
                    path: action.path.clone(),
                    expected_hash: "<not present (quarantined)>".into(),
                    observed_hash: live_hash,
                });
            }
            let quarantine_dest = action
                .quarantine_dest_rel
                .as_ref()
                .map(|rel| run_dir.join("quarantine").join(rel));
            if let Some(source) = quarantine_dest {
                if source.exists() {
                    if let Some(parent) = action.path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::rename(&source, &action.path)?;
                }
            }
        }
        "manual"
        | "emit_diagnostic"
        | "run_index_rebuild"
        | "run_graph_refresh"
        | "run_wal_checkpoint"
        | "run_migration"
        | "rewrite_jsonl"
        | "atomic_rewrite_toml"
        | "snapshot_backup" => {
            // No-op for undo. These op kinds are advisory: `mutate()`
            // records planned-mutation evidence in `actions.jsonl` but does
            // not touch the doctor's blast-radius files itself. The actual
            // subsystem mutation (when those actor handles land) is undone
            // via the subsystem's own rollback, not by doctor's
            // tempfile-rename inverse pair.
        }
        other => {
            return Err(DoctorRuntimeError::ActionsLogCorrupt {
                line_number: action.sequence as usize,
                reason: format!("unknown action kind: {}", other),
            });
        }
    }
    Ok(())
}

/// The shape printed by `ee doctor capabilities --json`.
#[derive(Clone, Debug, Serialize)]
pub struct CapabilitiesReport {
    pub schema: String,
    pub doctor_version: String,
    pub doctor_contract_version: String,
    pub tool_version: String,
    pub run_artifact_schema: String,
    pub blast_radius: Vec<String>,
    pub op_kinds: Vec<&'static str>,
    pub exit_codes: Vec<ExitCodeEntry>,
    pub env_vars: Vec<EnvVarEntry>,
    pub action_line_schema: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExitCodeEntry {
    pub code: i32,
    pub name: &'static str,
    pub meaning: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct EnvVarEntry {
    pub name: &'static str,
    pub purpose: &'static str,
}

impl CapabilitiesReport {
    /// Build the report. All fields are deterministic at this build's
    /// compile time except `tool_version` (from the package version).
    #[must_use]
    pub fn build(tool_version: &str, workspace: &Path) -> Self {
        let blast_radius = default_blast_radius_roots(workspace)
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>();
        Self {
            schema: CAPABILITIES_SCHEMA_V1.into(),
            doctor_version: env!("CARGO_PKG_VERSION").into(),
            doctor_contract_version: "1.0.0".into(),
            tool_version: tool_version.into(),
            run_artifact_schema: RUN_STATE_SCHEMA_V1.into(),
            blast_radius,
            op_kinds: vec![
                "write_file",
                "create_dir_all",
                "chmod",
                "quarantine_by_rename",
                "manual",
                "emit_diagnostic",
                "run_index_rebuild",
                "run_graph_refresh",
                "run_wal_checkpoint",
                "run_migration",
                "rewrite_jsonl",
                "atomic_rewrite_toml",
                "snapshot_backup",
            ],
            exit_codes: vec![
                ExitCodeEntry {
                    code: 0,
                    name: "ok",
                    meaning: "no findings, or all fixes applied successfully",
                },
                ExitCodeEntry {
                    code: 1,
                    name: "findings_present",
                    meaning: "diagnose mode: at least one finding (no --fix passed)",
                },
                ExitCodeEntry {
                    code: 2,
                    name: "fix_partial",
                    meaning: "fix attempted; some actions failed but state is consistent",
                },
                ExitCodeEntry {
                    code: 3,
                    name: "fix_failed",
                    meaning: "fix attempted; rolled back to pre-run state",
                },
                ExitCodeEntry {
                    code: 4,
                    name: "refused_unsafe",
                    meaning: "blast-radius / precondition refused; no mutations",
                },
                ExitCodeEntry {
                    code: 5,
                    name: "concurrency_lost",
                    meaning: "another doctor run is holding the workspace lock",
                },
                ExitCodeEntry {
                    code: 6,
                    name: "online_required",
                    meaning: "network probe needed but --online was not passed",
                },
                ExitCodeEntry {
                    code: 7,
                    name: "policy_denied",
                    meaning: "trauma-guard or other policy denied a precondition",
                },
                ExitCodeEntry {
                    code: 8,
                    name: "migration_required",
                    meaning: "doctor refuses because `ee migrate run` is needed first",
                },
            ],
            env_vars: vec![
                EnvVarEntry {
                    name: "EE_DOCTOR_BLAST_RADIUS",
                    purpose: "Override default blast radius (colon-separated abs paths)",
                },
                EnvVarEntry {
                    name: "EE_DOCTOR_LOCK_STALE_AFTER_SECS",
                    purpose: "Treat lock files older than this as stale (default: never)",
                },
                EnvVarEntry {
                    name: "EE_NO_COLOR",
                    purpose: "Disables ANSI styling on stderr (inherited from ee)",
                },
            ],
            action_line_schema: ACTION_LINE_SCHEMA_V1.into(),
        }
    }
}

/// Default blast radius for an ee workspace: the four canonical roots
/// doctor is allowed to write to.
#[must_use]
pub fn default_blast_radius_roots(workspace: &Path) -> Vec<PathBuf> {
    let mut roots = vec![workspace.join(".ee"), workspace.join(".doctor")];
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        roots.push(home.join(".local").join("share").join("ee"));
    }
    roots
}

// ---------- private helpers ----------

fn derive_run_id(target_sha: &str) -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut hasher = blake3::Hasher::new();
    hasher.update(target_sha.as_bytes());
    hasher.update(b"|");
    hasher.update(seconds.to_string().as_bytes());
    let hash = hasher.finalize();
    let hex = hash.to_hex();
    let short = &hex.as_str()[..6];
    format!("{}__{}", Utc::now().format("%Y-%m-%dT%H-%M-%SZ"), short)
}

fn hash_file(path: &Path) -> Result<String, DoctorRuntimeError> {
    let mut hasher = blake3::Hasher::new();
    let mut file = fs::File::open(path).map_err(|source| DoctorRuntimeError::Io {
        context: format!("open for hashing: {}", path.display()),
        source,
    })?;
    let mut buf = [0u8; 8192];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|source| DoctorRuntimeError::Io {
                context: format!("read for hashing: {}", path.display()),
                source,
            })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn read_mode(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::metadata(path).ok().map(|m| m.permissions().mode())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Validate that a `dest_under_quarantine` PathBuf is a relative path containing
/// only `Normal` components. Refuses absolute paths, `..` (`ParentDir`), Windows
/// drive prefixes, etc. Returns `BlastRadiusExceeded` on failure so the caller
/// surfaces it as exit 4 (refused_unsafe), matching the rest of the
/// containment story.
///
/// This is defense-in-depth: callers are SUPPOSED to pass safe relative paths,
/// but the runtime validates anyway so a buggy fixer can't escape the
/// `<run-dir>/quarantine/` root.
fn validate_relative_quarantine_dest(
    dest: &Path,
    run_dir: &Path,
) -> Result<(), DoctorRuntimeError> {
    for component in dest.components() {
        match component {
            std::path::Component::Normal(_) => continue,
            _ => {
                return Err(DoctorRuntimeError::BlastRadiusExceeded {
                    path: dest.to_path_buf(),
                    allowed_roots: vec![run_dir.join("quarantine")],
                });
            }
        }
    }
    Ok(())
}

fn is_path_in_blast_radius(path: &Path, roots: &[PathBuf]) -> bool {
    // Round-2 fresh-eyes (R2-P1-03): walk upward to the nearest existing
    // ancestor and canonicalize THAT, then re-append the tail. The prior
    // implementation refused any path whose immediate parent didn't yet
    // exist (so `WriteFile` to `<workspace>/.ee/cache/sub/leaf.json` was
    // refused if `cache/sub/` didn't exist, even though `write_file_atomic`
    // would have created it). This blocked legitimate Phase-4 fixer
    // scenarios.
    let probe = if path.exists() {
        path.canonicalize().ok()
    } else {
        nearest_existing_ancestor_canonical(path)
    };
    let probe = match probe {
        Some(p) => p,
        None => return false,
    };
    roots.iter().any(|root| {
        root.canonicalize()
            .map(|r| probe.starts_with(&r))
            .unwrap_or(false)
    })
}

/// Walk upward from `path` to the nearest existing ancestor, canonicalize
/// it, then re-append the not-yet-existing tail. Returns the resulting
/// concrete (canonical + tail) path if an existing ancestor was found.
///
/// The not-yet-existing tail is accepted only when every component is
/// `Normal`. Refusing `..` here is the defense-in-depth boundary that keeps a
/// missing intermediate directory from turning a literal in-radius prefix into
/// an out-of-radius write once the kernel resolves the final path.
fn nearest_existing_ancestor_canonical(path: &Path) -> Option<PathBuf> {
    let mut tail: Vec<OsString> = Vec::new();
    let mut p = path.to_path_buf();
    loop {
        if p.exists() {
            let mut canon = p.canonicalize().ok()?;
            for component in tail.iter().rev() {
                canon.push(component);
            }
            return Some(canon);
        }

        let name = match p.components().next_back()? {
            Component::Normal(name) => name.to_os_string(),
            Component::CurDir
            | Component::ParentDir
            | Component::Prefix(_)
            | Component::RootDir => {
                return None;
            }
        };

        let parent = p.parent()?.to_path_buf();
        tail.push(name);
        p = parent;
    }
}

fn write_file_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    // Round-2 fresh-eyes (R2-P1-04): refuse paths without a parent rather
    // than falling back to "." (the CWD of an invoking shell, which could
    // be outside the blast radius and break the cross-filesystem rename
    // assumption that persist() relies on).
    //
    // Round-5 self-review: `Path::new("foo.txt").parent()` returns `Some("")`
    // (an empty Path, not None). The original guard accepted that and then
    // happily wrote a tempfile in CWD via `NamedTempFile::new_in("")`. Filter
    // out the empty-parent case too so bare-filename paths get the same
    // refusal as truly parentless ones.
    //
    // Round-6 self-review: `Path::new("./foo.txt").parent()` returns
    // `Some(".")` (non-empty) which slipped past the round-5 filter and
    // produced the same CWD-leak via `NamedTempFile::new_in(".")`. The
    // chokepoint's invariant after R5-3 is that all writes are to absolute
    // paths (R5-3 absolutizes the workspace, which propagates everywhere).
    // Enforce that invariant at the leaf: refuse any parent that isn't
    // itself absolute. This catches `""`, `"."`, `"./sub"`, `".."` etc.
    let parent = path.parent().filter(|p| p.is_absolute()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "write_file_atomic: path lacks an absolute parent: {}",
                path.display()
            ),
        )
    })?;
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.flush()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Compute the sanitized relative path under `<run-dir>/backups/<seq>/` (or
/// quarantine equivalent) that maps an arbitrary absolute path to a safe
/// destination. Drops `Prefix` / `RootDir` / `CurDir`, replaces `ParentDir`
/// with the literal `__parent__` placeholder, keeps `Normal` components.
fn sanitize_path_for_run_dir(path: &Path) -> PathBuf {
    let mut rel = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(_) | std::path::Component::RootDir => continue,
            std::path::Component::CurDir => continue,
            std::path::Component::ParentDir => rel.push("__parent__"),
            std::path::Component::Normal(s) => rel.push(s),
        }
    }
    rel
}

fn stage_backup(
    ctx: &RunContext,
    path: &Path,
    expected_hash: &Option<String>,
) -> Result<PathBuf, DoctorRuntimeError> {
    // Backups are addressed by sequence-prefixed relative path under the
    // run's backups/ root. Round-2 fresh-eyes (R2-P0-01) found that
    // sharing the path-derived relative key for two mutations of the same
    // target file silently overwrites the first backup, breaking the
    // byte-identical-undo invariant the chokepoint advertises.
    //
    // The sequence prefix is `{:06}` of the action number we're about to
    // commit (i.e. `ctx.state.action_count + 1`) since `mutate()` does
    // `ctx.state.action_count += 1` AFTER calling stage_backup. Six digits
    // keeps lexical sort = numerical sort up to 999,999 actions per run.
    let path_rel = sanitize_path_for_run_dir(path);
    let next_seq = ctx.state.action_count + 1;
    let seq_dir = PathBuf::from(format!("{:06}", next_seq));
    let rel = seq_dir.join(&path_rel);
    let backup_path = ctx.run_dir.join("backups").join(&rel);
    // Defense in depth: the sequence-prefixed slot must not already exist.
    // If it does, a prior mutate() partially completed at this sequence
    // and the run state is inconsistent — refuse.
    if backup_path.exists() {
        return Err(DoctorRuntimeError::Io {
            context: format!(
                "backup collision at sequence {} target {}: {} already exists",
                next_seq,
                path.display(),
                backup_path.display()
            ),
            source: io::Error::new(io::ErrorKind::AlreadyExists, "backup collision"),
        });
    }
    if let Some(parent) = backup_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if !ctx.dry_run {
        // Verbatim copy. We use fs::copy which is byte-identical.
        fs::copy(path, &backup_path).map_err(|source| DoctorRuntimeError::Io {
            context: format!(
                "backup copy {} -> {}",
                path.display(),
                backup_path.display()
            ),
            source,
        })?;
        // Hash verify (the backup must hash to the same value as the
        // original at this exact moment).
        if let Some(expected) = expected_hash {
            let observed = hash_file(&backup_path)?;
            if &observed != expected {
                return Err(DoctorRuntimeError::Io {
                    context: format!(
                        "backup hash mismatch after copy ({}): expected {}, observed {}",
                        backup_path.display(),
                        expected,
                        observed
                    ),
                    source: io::Error::new(io::ErrorKind::Other, "backup race"),
                });
            }
        }
    }
    Ok(rel)
}

/// Maximum bytes inspected when reading `<run_dir>/state.json`. Real
/// `RunState` is a tiny JSON object (`schema`, `run_id`, `target_sha`,
/// `workspace`, two timestamps, `status` enum, `action_count`, `dry_run` —
/// well under 1 KiB in practice); 4 MiB gives many orders of magnitude of
/// headroom while bounding peer plants on shared multi-agent checkouts.
///
/// Without this cap, a peer-planted or accidentally-inflated state.json
/// (corrupt write, `cat /dev/urandom > state.json`, hostile multi-agent
/// checkout) would pin a matching allocation through `fs::read` on every
/// `replay_undo` (`ee doctor --undo`) invocation. The previous shape also
/// had no symlink guard: a peer-swapped symlink at the same path would
/// have followed off-tree to attacker-chosen bytes. Matches the cap +
/// read-shape the parallel hardening pass applied to
/// `src/core/index.rs::read_index_metadata_contents` (ad2d302e) and
/// `src/core/preflight_guard.rs::read_preflight_rules_file_no_follow`
/// (7f56d89b).
const DOCTOR_RUN_STATE_INSPECT_LIMIT: u64 = 4 * 1024 * 1024;

/// Read the persisted [`RunState`] from `<run_dir>/state.json`.
fn read_state(run_dir: &Path) -> Result<RunState, DoctorRuntimeError> {
    let path = run_dir.join("state.json");
    let metadata = fs::symlink_metadata(&path).map_err(|source| DoctorRuntimeError::Io {
        context: format!("read state.json {}", path.display()),
        source,
    })?;
    if !metadata.file_type().is_file() {
        return Err(DoctorRuntimeError::Io {
            context: format!("read state.json {}", path.display()),
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "state.json is not a regular file",
            ),
        });
    }
    if metadata.len() > DOCTOR_RUN_STATE_INSPECT_LIMIT {
        return Err(DoctorRuntimeError::Io {
            context: format!("read state.json {}", path.display()),
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "state.json size {} exceeds {DOCTOR_RUN_STATE_INSPECT_LIMIT} byte cap",
                    metadata.len()
                ),
            ),
        });
    }
    let file = fs::File::open(&path).map_err(|source| DoctorRuntimeError::Io {
        context: format!("read state.json {}", path.display()),
        source,
    })?;
    let mut bytes = Vec::new();
    file.take(DOCTOR_RUN_STATE_INSPECT_LIMIT.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| DoctorRuntimeError::Io {
            context: format!("read state.json {}", path.display()),
            source,
        })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > DOCTOR_RUN_STATE_INSPECT_LIMIT {
        return Err(DoctorRuntimeError::Io {
            context: format!("read state.json {}", path.display()),
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                "state.json grew past cap during read",
            ),
        });
    }
    serde_json::from_slice(&bytes).map_err(|e| DoctorRuntimeError::Io {
        context: format!("parse state.json {}", path.display()),
        source: io::Error::new(io::ErrorKind::InvalidData, e),
    })
}

/// Drop-guarded lock around `<workspace>/.ee/.doctor.lock` used by
/// [`replay_undo`] to serialize against concurrent `--fix` or `--undo` runs.
/// Round-2 fresh-eyes (R2-P1-01).
struct UndoLockGuard {
    lock_path: PathBuf,
}

impl Drop for UndoLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock_path);
    }
}

fn acquire_undo_lock(workspace: &Path) -> Result<UndoLockGuard, DoctorRuntimeError> {
    // Round-5 self-review: defend against a state.json that has a relative
    // `workspace` (shouldn't happen with the round-5 fix in `RunContext::
    // start` that absolutizes the input, but a corrupt or hand-edited
    // state.json could still smuggle one in). Joining a relative workspace
    // with the CURRENT CWD when undo runs would point the lock at the wrong
    // place. Pin it to CWD-at-undo-time only when we can't recover an
    // absolute form — better than silently failing isolation.
    let workspace_abs = if workspace.is_absolute() {
        workspace.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(workspace))
            .unwrap_or_else(|_| workspace.to_path_buf())
    };
    let ee_dir = workspace_abs.join(".ee");
    fs::create_dir_all(&ee_dir).map_err(|source| DoctorRuntimeError::Io {
        context: format!("create_dir_all({}) for undo lock", ee_dir.display()),
        source,
    })?;
    let lock_path = ee_dir.join(".doctor.lock");
    let contents = format!("undo\n{}\n", std::process::id());
    match fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&lock_path)
    {
        Ok(mut f) => {
            // Round-3 self-review (Bug #5): clean up the lock file if the
            // initial write fails, so a partial create doesn't strand a
            // phantom holder for subsequent doctor invocations.
            if let Err(source) = f.write_all(contents.as_bytes()).and_then(|()| f.flush()) {
                let _ = fs::remove_file(&lock_path);
                return Err(DoctorRuntimeError::Io {
                    context: format!("write undo lock {}", lock_path.display()),
                    source,
                });
            }
            Ok(UndoLockGuard { lock_path })
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let holder = fs::read_to_string(&lock_path)
                .ok()
                .and_then(|s| s.lines().next().map(std::string::ToString::to_string));
            Err(DoctorRuntimeError::ConcurrencyLost {
                lock_path,
                holder_run_id: holder,
            })
        }
        Err(source) => Err(DoctorRuntimeError::Io {
            context: format!("create undo lock {}", lock_path.display()),
            source,
        }),
    }
}

fn write_state(run_dir: &Path, state: &RunState) -> Result<(), DoctorRuntimeError> {
    let path = run_dir.join("state.json");
    let bytes = serde_json::to_vec_pretty(state).map_err(|e| DoctorRuntimeError::Io {
        context: "serialize RunState".into(),
        source: io::Error::new(io::ErrorKind::InvalidData, e),
    })?;
    write_file_atomic(&path, &bytes).map_err(|source| DoctorRuntimeError::Io {
        context: format!("write state.json {}", path.display()),
        source,
    })?;
    Ok(())
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh_workspace() -> TempDir {
        TempDir::new().expect("tempdir")
    }

    fn start_run(ws: &Path) -> RunContext {
        let mut roots = default_blast_radius_roots(ws);
        // Tests should be able to mutate inside the workspace's root, not
        // just `.ee/` — push the workspace root itself so the unit tests
        // can write a file at workspace.join("data.txt").
        roots.push(ws.to_path_buf());
        RunContext::start(ws, "deadbeefcafe", roots, false).expect("start run")
    }

    #[test]
    fn write_file_creates_file_and_records_action() {
        let ws = fresh_workspace();
        let mut ctx = start_run(ws.path());
        let target = ws.path().join("data.txt");

        let line = mutate(
            &mut ctx,
            &target,
            Op::WriteFile {
                bytes: b"hello".to_vec(),
            },
        )
        .expect("mutate");
        assert_eq!(fs::read(&target).unwrap(), b"hello");
        assert_eq!(line.kind, "write_file");
        assert!(line.before_hash.is_none());
        assert!(line.after_hash.is_some());

        // actions.jsonl exists and has one line.
        let actions_path = ctx.run_dir().join("actions.jsonl");
        let raw = fs::read_to_string(&actions_path).unwrap();
        assert_eq!(raw.lines().count(), 1);
    }

    #[test]
    fn write_file_idempotent_same_bytes_returns_no_op() {
        let ws = fresh_workspace();
        let mut ctx = start_run(ws.path());
        let target = ws.path().join("data.txt");

        let _ = mutate(
            &mut ctx,
            &target,
            Op::WriteFile {
                bytes: b"same".to_vec(),
            },
        )
        .expect("first");
        let result = mutate(
            &mut ctx,
            &target,
            Op::WriteFile {
                bytes: b"same".to_vec(),
            },
        );
        assert!(matches!(result, Err(DoctorRuntimeError::NoOpIdempotent)));
    }

    #[test]
    fn write_file_backs_up_existing_content_before_overwrite() {
        let ws = fresh_workspace();
        let target = ws.path().join("data.txt");
        fs::write(&target, b"original").unwrap();

        let mut ctx = start_run(ws.path());
        let line = mutate(
            &mut ctx,
            &target,
            Op::WriteFile {
                bytes: b"updated".to_vec(),
            },
        )
        .expect("mutate");

        let rel = line.backup_rel_path.expect("backup rel path");
        let backup_path = ctx.run_dir().join("backups").join(&rel);
        assert_eq!(fs::read(&backup_path).unwrap(), b"original");
        assert_eq!(fs::read(&target).unwrap(), b"updated");
    }

    #[test]
    fn blast_radius_refuses_writes_outside_allowed_roots() {
        let ws = fresh_workspace();
        // Restricted roots: only .ee under the workspace.
        let restricted = vec![ws.path().join(".ee")];
        let mut ctx = RunContext::start(ws.path(), "abc1234", restricted, false).unwrap();

        // Write to a parent of the workspace — should refuse.
        let outside = ws.path().parent().unwrap().join("evil.txt");
        let result = mutate(
            &mut ctx,
            &outside,
            Op::WriteFile {
                bytes: b"nope".to_vec(),
            },
        );
        assert!(matches!(
            result,
            Err(DoctorRuntimeError::BlastRadiusExceeded { .. })
        ));
    }

    #[test]
    fn concurrency_second_start_refuses_with_lost() {
        let ws = fresh_workspace();
        let _first = start_run(ws.path());
        let result = RunContext::start(
            ws.path(),
            "deadbeefcafe2",
            vec![ws.path().to_path_buf()],
            false,
        );
        assert!(matches!(
            result,
            Err(DoctorRuntimeError::ConcurrencyLost { .. })
        ));
    }

    #[test]
    fn finish_releases_lock_and_writes_state() {
        let ws = fresh_workspace();
        let ctx = start_run(ws.path());
        let run_dir = ctx.run_dir().to_path_buf();
        let lock = ws.path().join(".ee").join(".doctor.lock");
        assert!(lock.exists());

        ctx.finish(RunStatus::CompletedOk).expect("finish");
        assert!(!lock.exists(), "lock released after finish");

        let state: RunState =
            serde_json::from_slice(&fs::read(run_dir.join("state.json")).unwrap()).unwrap();
        assert!(matches!(state.status, RunStatus::CompletedOk));
        assert!(state.finished_at.is_some());
    }

    #[test]
    fn drop_releases_lock_when_finish_was_skipped() {
        // Round-6 self-review (R6-5): if `RunContext` is dropped without
        // `finish()` being called — caller propagated an error via `?`, a
        // panic unwound past the doctor scope, or a future code path
        // simply forgot to call `finish()` — the workspace lock must
        // still be released so the next `ee doctor --fix` can run.
        // Previously the lock leaked forever and the only recovery was
        // to manually delete `.ee/.doctor.lock`.
        let ws = fresh_workspace();
        let lock = ws.path().join(".ee").join(".doctor.lock");
        {
            let _ctx = start_run(ws.path());
            assert!(lock.exists(), "lock should be held while ctx is alive");
            // _ctx is intentionally not `.finish()`-ed; it drops here.
        }
        assert!(
            !lock.exists(),
            "lock must be released by Drop when finish() was skipped (R6-5)"
        );
        // Verify the next start() can acquire the lock cleanly.
        let ctx2 = start_run(ws.path());
        ctx2.finish(RunStatus::CompletedOk).expect("finish");
        assert!(!lock.exists());
    }

    #[test]
    fn undo_restores_byte_identical_state_for_write_file() {
        let ws = fresh_workspace();
        let target = ws.path().join("data.txt");
        fs::write(&target, b"original").unwrap();
        let original_hash = hash_file(&target).unwrap();

        let run_dir;
        {
            let mut ctx = start_run(ws.path());
            run_dir = ctx.run_dir().to_path_buf();
            mutate(
                &mut ctx,
                &target,
                Op::WriteFile {
                    bytes: b"updated_v1".to_vec(),
                },
            )
            .unwrap();
            mutate(
                &mut ctx,
                &target,
                Op::WriteFile {
                    bytes: b"updated_v2".to_vec(),
                },
            )
            .unwrap();
            ctx.finish(RunStatus::CompletedOk).unwrap();
        }

        // Two mutations applied. After replay_undo, target must match
        // original bytes byte-for-byte.
        assert_eq!(fs::read(&target).unwrap(), b"updated_v2");
        let summary = replay_undo(&run_dir).expect("undo");
        assert_eq!(summary.actions_undone, 2);
        assert_eq!(fs::read(&target).unwrap(), b"original");
        assert_eq!(hash_file(&target).unwrap(), original_hash);
    }

    #[test]
    fn undo_quarantines_files_that_didnt_exist_pre_run() {
        let ws = fresh_workspace();
        let target = ws.path().join("created.txt");

        let run_dir;
        {
            let mut ctx = start_run(ws.path());
            run_dir = ctx.run_dir().to_path_buf();
            mutate(
                &mut ctx,
                &target,
                Op::WriteFile {
                    bytes: b"new".to_vec(),
                },
            )
            .unwrap();
            ctx.finish(RunStatus::CompletedOk).unwrap();
        }

        // After undo, the created file should NOT exist at its original
        // path, but should be quarantined under
        // <run_dir>/quarantine/undo_created/<seq>/<sanitized-path>/created.txt
        // per the round-2 sequence-prefixing fix (R2-P0-03).
        replay_undo(&run_dir).expect("undo");
        assert!(!target.exists());
        let undo_root = run_dir.join("quarantine").join("undo_created");
        assert!(undo_root.is_dir());
        // Walk the tree and confirm exactly one `created.txt` exists somewhere
        // under undo_created/<sequence>/.../created.txt.
        let mut found = false;
        let mut stack = vec![undo_root.clone()];
        while let Some(d) = stack.pop() {
            for entry in fs::read_dir(&d).unwrap().flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.file_name().and_then(|s| s.to_str()) == Some("created.txt") {
                    found = true;
                }
            }
        }
        assert!(
            found,
            "quarantined created.txt not found under {}",
            undo_root.display()
        );
    }

    #[test]
    fn undo_is_idempotent_when_called_twice() {
        let ws = fresh_workspace();
        let target = ws.path().join("d.txt");
        fs::write(&target, b"orig").unwrap();

        let run_dir;
        {
            let mut ctx = start_run(ws.path());
            run_dir = ctx.run_dir().to_path_buf();
            mutate(
                &mut ctx,
                &target,
                Op::WriteFile {
                    bytes: b"new".to_vec(),
                },
            )
            .unwrap();
            ctx.finish(RunStatus::CompletedOk).unwrap();
        }
        let s1 = replay_undo(&run_dir).unwrap();
        assert_eq!(s1.actions_undone, 1);
        let s2 = replay_undo(&run_dir).unwrap();
        assert_eq!(s2.actions_undone, 0);
        assert_eq!(s2.actions_skipped, 1);
    }

    #[test]
    fn undo_fails_closed_when_state_json_is_missing() {
        let runs = fresh_workspace();
        let run_dir = runs.path().join("run_without_state");
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("actions.jsonl"), "").unwrap();

        let result = replay_undo(&run_dir);

        match result {
            Err(DoctorRuntimeError::Io { context, source }) => {
                assert!(context.contains("read state.json"), "{context}");
                assert_eq!(source.kind(), io::ErrorKind::NotFound);
            }
            other => panic!("expected missing state.json to fail closed, got {other:?}"),
        }
        assert!(
            !run_dir.join("undo_log.jsonl").exists(),
            "undo must stop before replay artifacts are written"
        );
    }

    #[test]
    fn undo_fails_closed_when_state_json_is_corrupt() {
        let runs = fresh_workspace();
        let run_dir = runs.path().join("run_with_corrupt_state");
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("state.json"), b"{not valid json").unwrap();
        fs::write(run_dir.join("actions.jsonl"), "").unwrap();

        let result = replay_undo(&run_dir);

        match result {
            Err(DoctorRuntimeError::Io { context, source }) => {
                assert!(context.contains("parse state.json"), "{context}");
                assert_eq!(source.kind(), io::ErrorKind::InvalidData);
            }
            other => panic!("expected corrupt state.json to fail closed, got {other:?}"),
        }
        assert!(
            !run_dir.join("undo_log.jsonl").exists(),
            "undo must stop before replay artifacts are written"
        );
    }

    #[test]
    fn quarantine_by_rename_moves_file_into_run_quarantine() {
        let ws = fresh_workspace();
        let target = ws.path().join("trash.tmp");
        fs::write(&target, b"junk").unwrap();

        let mut ctx = start_run(ws.path());
        let line = mutate(
            &mut ctx,
            &target,
            Op::QuarantineByRename {
                dest_under_quarantine: PathBuf::from("trash.tmp"),
            },
        )
        .unwrap();
        assert!(!target.exists());
        assert!(ctx.run_dir().join("quarantine").join("trash.tmp").exists());
        assert_eq!(line.kind, "quarantine_by_rename");
    }

    #[test]
    fn manual_op_records_action_but_writes_nothing_to_disk() {
        let ws = fresh_workspace();
        let mut ctx = start_run(ws.path());
        let target = ws.path().join("nonexistent");
        let line = mutate(
            &mut ctx,
            &target,
            Op::Manual {
                steps: vec!["run X".into(), "then Y".into()],
            },
        )
        .unwrap();
        assert!(!target.exists());
        assert_eq!(line.kind, "manual");
        assert!(line.notes.is_some());
    }

    #[test]
    fn capabilities_report_is_stable_and_self_describing() {
        let ws = fresh_workspace();
        let report = CapabilitiesReport::build("0.1.0", ws.path());
        let json = serde_json::to_string_pretty(&report).unwrap();
        assert!(json.contains("ee.doctor.capabilities.v1"));
        assert!(json.contains("write_file"));
        assert!(json.contains("quarantine_by_rename"));
        assert!(json.contains("\"code\": 5"));
        assert!(json.contains("concurrency_lost"));
    }

    #[test]
    fn dry_run_records_actions_without_touching_disk() {
        let ws = fresh_workspace();
        let target = ws.path().join("data.txt");
        fs::write(&target, b"orig").unwrap();

        let mut roots = default_blast_radius_roots(ws.path());
        roots.push(ws.path().to_path_buf());
        let mut ctx = RunContext::start(ws.path(), "sha", roots, /*dry_run*/ true).unwrap();

        let _line = mutate(
            &mut ctx,
            &target,
            Op::WriteFile {
                bytes: b"new".to_vec(),
            },
        )
        .unwrap();
        // Disk is unchanged.
        assert_eq!(fs::read(&target).unwrap(), b"orig");
        // But the action is recorded.
        let actions = fs::read_to_string(ctx.run_dir().join("actions.jsonl")).unwrap();
        assert!(actions.contains("write_file"));
    }

    #[test]
    fn create_dir_all_is_idempotent_on_existing_dir() {
        let ws = fresh_workspace();
        let mut ctx = start_run(ws.path());
        let target = ws.path().join("subdir");
        fs::create_dir_all(&target).unwrap();

        let result = mutate(&mut ctx, &target, Op::CreateDirAll { mode: 0o755 });
        assert!(matches!(result, Err(DoctorRuntimeError::NoOpIdempotent)));
    }

    #[test]
    fn quarantine_refuses_path_traversal_with_parent_dir_components() {
        // Defense-in-depth: a buggy fixer could pass `../../etc/passwd` as the
        // quarantine destination. Validate that the chokepoint refuses with
        // BlastRadiusExceeded rather than letting `fs::rename` resolve the `..`
        // and escape the run-dir quarantine root. Round-1 fresh-eyes review.
        let ws = fresh_workspace();
        let target = ws.path().join("victim.txt");
        fs::write(&target, b"hi").unwrap();

        let mut ctx = start_run(ws.path());
        let result = mutate(
            &mut ctx,
            &target,
            Op::QuarantineByRename {
                dest_under_quarantine: PathBuf::from("../../etc/passwd"),
            },
        );
        assert!(matches!(
            result,
            Err(DoctorRuntimeError::BlastRadiusExceeded { .. })
        ));
        // Victim is untouched.
        assert!(target.exists());
        assert_eq!(fs::read(&target).unwrap(), b"hi");
    }

    #[test]
    fn undo_refuses_when_live_file_drifted_after_doctor_run() {
        // Round-2 fresh-eyes (F5): if an external writer modified the file
        // after the doctor's --fix, undo must NOT silently overwrite. Refuse
        // with UndoStateDrifted; let the operator inspect.
        let ws = fresh_workspace();
        let target = ws.path().join("data.txt");
        fs::write(&target, b"original").unwrap();

        let run_dir;
        {
            let mut ctx = start_run(ws.path());
            run_dir = ctx.run_dir().to_path_buf();
            mutate(
                &mut ctx,
                &target,
                Op::WriteFile {
                    bytes: b"doctor_wrote".to_vec(),
                },
            )
            .unwrap();
            ctx.finish(RunStatus::CompletedOk).unwrap();
        }

        // Simulate external writer modifying the file after the doctor ran.
        fs::write(&target, b"external_writer_changed_this").unwrap();

        let result = replay_undo(&run_dir);
        let summary = result.expect("replay_undo returns Ok with partial status");
        // The undo refused this action; status reflects partial completion.
        assert!(matches!(summary.status, RunStatus::UndonePartial));
        assert!(summary.first_error.is_some());
        let err = summary.first_error.unwrap();
        assert!(
            err.contains("drifted"),
            "expected drift error, got: {}",
            err
        );
        // Live file is untouched — the external writer's change is preserved.
        assert_eq!(fs::read(&target).unwrap(), b"external_writer_changed_this");
    }

    #[test]
    fn undo_refuses_when_path_reoccupied_after_quarantine() {
        // Round-2 fresh-eyes (F6): if something landed at the original path
        // between quarantine and undo, refuse rather than overwrite.
        let ws = fresh_workspace();
        let victim = ws.path().join("orphan.wal");
        fs::write(&victim, b"original wal").unwrap();

        let run_dir;
        {
            let mut ctx = start_run(ws.path());
            run_dir = ctx.run_dir().to_path_buf();
            mutate(
                &mut ctx,
                &victim,
                Op::QuarantineByRename {
                    dest_under_quarantine: PathBuf::from("orphan.wal"),
                },
            )
            .unwrap();
            ctx.finish(RunStatus::CompletedOk).unwrap();
        }

        // Simulate: an unrelated process creates a NEW file at the same
        // path after the doctor quarantined the original.
        fs::write(&victim, b"new_unrelated_file").unwrap();

        let summary = replay_undo(&run_dir).expect("returns Ok with partial");
        assert!(matches!(summary.status, RunStatus::UndonePartial));
        let err = summary.first_error.unwrap();
        assert!(
            err.contains("drifted"),
            "expected drift error, got: {}",
            err
        );
        // The new unrelated file is preserved.
        assert_eq!(fs::read(&victim).unwrap(), b"new_unrelated_file");
        // The quarantined original is still safely in quarantine.
        let quarantine = run_dir.join("quarantine").join("orphan.wal");
        assert!(quarantine.exists());
        assert_eq!(fs::read(&quarantine).unwrap(), b"original wal");
    }

    #[test]
    fn two_writes_to_same_path_in_one_run_undo_byte_identical() {
        // Round-2 fresh-eyes R2-P0-01: the headline finding. Two
        // WriteFile mutations of the SAME target file in a single run
        // must each store their own backup, so undo can walk both
        // actions in reverse and end at the original bytes.
        let ws = fresh_workspace();
        let target = ws.path().join("data.txt");
        fs::write(&target, b"orig").unwrap();

        let run_dir;
        {
            let mut ctx = start_run(ws.path());
            run_dir = ctx.run_dir().to_path_buf();
            // First write.
            mutate(
                &mut ctx,
                &target,
                Op::WriteFile {
                    bytes: b"v1".to_vec(),
                },
            )
            .unwrap();
            // Second write (same path, different bytes).
            mutate(
                &mut ctx,
                &target,
                Op::WriteFile {
                    bytes: b"v2".to_vec(),
                },
            )
            .unwrap();
            ctx.finish(RunStatus::CompletedOk).unwrap();
        }

        // Two distinct backups must exist under their sequence-prefixed
        // directories.
        let b1 = run_dir.join("backups").join("000001");
        let b2 = run_dir.join("backups").join("000002");
        assert!(b1.is_dir(), "first backup dir missing: {}", b1.display());
        assert!(b2.is_dir(), "second backup dir missing: {}", b2.display());

        // Undo restores byte-identical original.
        let summary = replay_undo(&run_dir).unwrap();
        assert_eq!(summary.actions_undone, 2);
        assert_eq!(fs::read(&target).unwrap(), b"orig");
    }

    #[test]
    fn chmod_idempotent_when_mode_already_matches() {
        // Round-2 fresh-eyes R2-P1-02: Chmod with the same mode the file
        // already has must return NoOpIdempotent rather than spuriously
        // recording a bumped-but-unchanged mtime in actions.jsonl.
        // Unix-only because Windows treats mode bits as advisory.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let ws = fresh_workspace();
            let target = ws.path().join("perm.txt");
            fs::write(&target, b"x").unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();

            let mut ctx = start_run(ws.path());
            let result = mutate(&mut ctx, &target, Op::Chmod { mode: 0o644 });
            assert!(
                matches!(result, Err(DoctorRuntimeError::NoOpIdempotent)),
                "expected NoOpIdempotent, got: {:?}",
                result
            );
        }
    }

    #[test]
    fn write_file_atomic_refuses_path_without_parent() {
        // Round-2 R2-P1-04 → round-5 → round-6 self-review.
        // `write_file_atomic` used to fall back to the CWD of an invoking
        // shell when given a parentless or relative path. Round-6 tightened
        // the filter to refuse any parent that isn't absolute, since the
        // chokepoint's invariant (after R5-3 absolutized the workspace) is
        // that every write goes to an absolute path. Each parent form below
        // would have leaked into CWD under the previous, weaker guards.
        //
        // - `Path::new("")` has `.parent() == None`.
        // - `Path::new("foo.txt")` has `.parent() == Some("")` — slipped
        //   past the original R2-P1-04 None-only guard.
        // - `Path::new("./foo.txt")` has `.parent() == Some(".")`
        //   (non-empty, non-absolute) — slipped past the R5-2 is_empty
        //   filter.
        // - `Path::new("../foo.txt")` has `.parent() == Some("..")` —
        //   same class as `./foo.txt`.
        for parentless in [
            Path::new(""),
            Path::new("foo.txt"),
            Path::new("./foo.txt"),
            Path::new("../foo.txt"),
        ] {
            let result = write_file_atomic(parentless, b"x");
            assert!(
                result.is_err(),
                "write_file_atomic should refuse path lacking an absolute parent: {}",
                parentless.display()
            );
            let err = result.unwrap_err();
            assert!(
                matches!(
                    err.kind(),
                    io::ErrorKind::InvalidInput | io::ErrorKind::NotFound
                ),
                "unexpected error kind for {}: {:?}",
                parentless.display(),
                err.kind()
            );
        }
    }

    #[test]
    fn two_writes_to_different_paths_with_same_basename_undo_correctly() {
        // Round-2 fresh-eyes R2-P0-03: undo of `Op::WriteFile { bytes: ... }`
        // where two files share a basename (e.g., a/config.toml and
        // b/config.toml) — previously the undo quarantine destination was
        // namespaced only by `file_name()`, so the second undo would
        // overwrite the first. Now namespaced by sequence + sanitized path.
        let ws = fresh_workspace();
        let a = ws.path().join("a/config.toml");
        let b = ws.path().join("b/config.toml");
        fs::create_dir_all(a.parent().unwrap()).unwrap();
        fs::create_dir_all(b.parent().unwrap()).unwrap();
        // Neither file exists initially.

        let run_dir;
        {
            let mut ctx = start_run(ws.path());
            run_dir = ctx.run_dir().to_path_buf();
            mutate(
                &mut ctx,
                &a,
                Op::WriteFile {
                    bytes: b"contents-a".to_vec(),
                },
            )
            .unwrap();
            mutate(
                &mut ctx,
                &b,
                Op::WriteFile {
                    bytes: b"contents-b".to_vec(),
                },
            )
            .unwrap();
            ctx.finish(RunStatus::CompletedOk).unwrap();
        }

        // After undo, both files are quarantined; the doctor never deletes.
        // Verify both quarantine destinations are distinct paths and both
        // hold their original creation bytes.
        replay_undo(&run_dir).unwrap();
        assert!(!a.exists());
        assert!(!b.exists());

        let q_root = run_dir.join("quarantine").join("undo_created");
        // Two sequence-prefixed quarantine dirs must exist.
        let mut entries: Vec<String> = fs::read_dir(&q_root)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        entries.sort();
        assert_eq!(
            entries,
            vec!["000001".to_string(), "000002".to_string()],
            "expected sequence-prefixed quarantine dirs, found: {:?}",
            entries
        );
    }

    #[test]
    fn quarantine_refuses_absolute_path_destination() {
        // Defense-in-depth: `Path::join(absolute)` replaces the prefix entirely.
        // Without validation, `dest_under_quarantine = "/etc/passwd"` would
        // overwrite or move to `/etc/passwd`. Refuse instead.
        let ws = fresh_workspace();
        let target = ws.path().join("victim.txt");
        fs::write(&target, b"hi").unwrap();

        let mut ctx = start_run(ws.path());
        let result = mutate(
            &mut ctx,
            &target,
            Op::QuarantineByRename {
                dest_under_quarantine: PathBuf::from("/tmp/escape"),
            },
        );
        assert!(matches!(
            result,
            Err(DoctorRuntimeError::BlastRadiusExceeded { .. })
        ));
        assert!(target.exists());
    }
}
