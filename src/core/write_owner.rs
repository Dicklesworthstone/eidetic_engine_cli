//! Single-write-owner actor for serialized database writes (ADR-0013).
//!
//! All durable writes flow through a single-writer actor to prevent SQLITE_BUSY
//! races between concurrent `ee` invocations. Write requests are submitted to a
//! bounded channel and processed serially in FIFO order.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────┐     ┌─────────────┐     ┌─────────────┐
//! │  Caller 1   │     │  Caller 2   │     │  Caller N   │
//! └──────┬──────┘     └──────┬──────┘     └──────┬──────┘
//!        │                   │                   │
//!        │ submit(request)   │                   │
//!        ▼                   ▼                   ▼
//! ┌──────────────────────────────────────────────────────┐
//! │                   MPSC Channel                        │
//! │              (bounded, FIFO order)                    │
//! └──────────────────────────┬───────────────────────────┘
//!                            │
//!                            ▼
//!                    ┌───────────────┐
//!                    │  WriteOwner   │
//!                    │  (single Rx)  │
//!                    └───────┬───────┘
//!                            │
//!                            ▼
//!                    ┌───────────────┐
//!                    │   Database    │
//!                    │   (serial)    │
//!                    └───────────────┘
//! ```
//!
//! # Cancel Safety
//!
//! Uses asupersync's two-phase reserve/commit pattern:
//! - If cancelled during reserve: request is not queued
//! - If cancelled after reserve: permit drop aborts cleanly
//! - Response arrives via oneshot channel

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use asupersync::channel::{mpsc, oneshot};
use asupersync::cx::Cx;
use serde::Serialize;

use crate::models::DomainError;

/// Schema for write owner status response.
pub const WRITE_OWNER_STATUS_SCHEMA_V1: &str = "ee.write_owner.status.v1";

/// Schema for write owner busy error.
pub const WRITE_OWNER_BUSY_SCHEMA_V1: &str = "ee.write_owner.busy.v1";

/// Schema for write spool status response.
pub const WRITE_SPOOL_STATUS_SCHEMA_V1: &str = "ee.write_spool.status.v1";

/// Schema for write spool backpressure errors.
pub const WRITE_SPOOL_BACKPRESSURE_SCHEMA_V1: &str = "ee.write_spool.backpressure.v1";

/// Schema for the durable write-spool crash-recovery state marker.
pub const WRITE_SPOOL_RECOVERY_STATE_SCHEMA_V1: &str = "ee.write_spool.recovery_state.v1";

/// Relative path to the durable write-spool crash-recovery state marker.
pub const WRITE_SPOOL_RECOVERY_STATE_PATH: &str = ".ee/write-spool/recovery-state.json";

const WRITE_SPOOL_RECOVERY_STATE_CLEAN: &str = "clean";
const WRITE_SPOOL_RECOVERY_STATE_REPLAY_REQUIRED: &str = "uncommitted_write_replay_required";

/// Default channel capacity for write requests.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 64;

/// Default maximum pending entries in the durable write spool.
pub const DEFAULT_SPOOL_MAX_PENDING: usize = 512;

/// Default maximum entries coalesced into one durable batch.
pub const DEFAULT_SPOOL_MAX_BATCH_SIZE: usize = 32;

/// Default maximum payload bytes waiting in the write spool.
pub const DEFAULT_SPOOL_MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;

/// Default queue age budget before callers receive backpressure.
pub const DEFAULT_SPOOL_QUEUE_TIMEOUT_MS: u64 = 30_000;

/// Error code for write owner busy condition.
pub const WRITE_OWNER_BUSY_CODE: &str = "write_owner_busy";

/// Error code for write spool backpressure.
pub const WRITE_SPOOL_BACKPRESSURE_CODE: &str = "write_spool_backpressure";

/// User-facing alias for queue-depth write spool backpressure (L1).
pub const WRITE_QUEUE_FULL_CODE: &str = "write_queue_full";

/// SRR3 fake-runner degraded code for writes cancelled before commit.
pub const WRITE_HOT_PATH_CANCELLED_BEFORE_COMMIT_CODE: &str =
    "write_hot_path_cancelled_before_commit";

/// SRR3 fake-runner degraded code for modeled fsync failures.
pub const WRITE_HOT_PATH_FSYNC_FAILURE_CODE: &str = "write_hot_path_fsync_failure";

/// Return the workspace-relative recovery state path.
#[must_use]
pub fn write_spool_recovery_state_path(workspace_path: &Path) -> PathBuf {
    workspace_path.join(WRITE_SPOOL_RECOVERY_STATE_PATH)
}

/// Mark the workspace as having an interrupted write that requires replay.
pub fn mark_write_replay_required(workspace_path: &Path) -> std::io::Result<()> {
    write_recovery_state(workspace_path, WRITE_SPOOL_RECOVERY_STATE_REPLAY_REQUIRED)
}

/// Mark the workspace write-spool recovery state as clean.
pub fn mark_write_replay_clean(workspace_path: &Path) -> std::io::Result<()> {
    write_recovery_state(workspace_path, WRITE_SPOOL_RECOVERY_STATE_CLEAN)
}

/// Returns true when the workspace has an interrupted write requiring replay.
#[must_use]
pub fn workspace_write_replay_required(workspace_path: &Path) -> bool {
    let path = write_spool_recovery_state_path(workspace_path);
    if recovery_state_path_has_symlink_component(&path).unwrap_or(true) {
        return false;
    }
    let Ok(metadata) = fs::symlink_metadata(&path) else {
        return false;
    };
    if !metadata.file_type().is_file() {
        return false;
    }
    let Ok(raw) = fs::read_to_string(path) else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|value| {
            value
                .get("state")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .as_deref()
        == Some(WRITE_SPOOL_RECOVERY_STATE_REPLAY_REQUIRED)
}

fn write_recovery_state(workspace_path: &Path, state: &str) -> std::io::Result<()> {
    let path = write_spool_recovery_state_path(workspace_path);
    ensure_recovery_state_path_has_no_symlink_components(&path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    ensure_recovery_state_path_has_no_symlink_components(&path)?;
    ensure_recovery_state_final_path_is_regular_or_missing(&path)?;
    let payload = format!(
        "{{\"schema\":\"{WRITE_SPOOL_RECOVERY_STATE_SCHEMA_V1}\",\"state\":\"{state}\"}}\n"
    );

    let mut temp_path = path.clone();
    temp_path.set_extension("tmp");
    ensure_recovery_state_path_has_no_symlink_components(&temp_path)?;
    ensure_recovery_state_temp_path_is_regular_or_missing(&temp_path)?;

    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        file.write_all(payload.as_bytes())?;
        file.sync_data()?;
    }

    fs::rename(temp_path, path)?;

    // Attempt to sync the parent directory to persist the rename
    if let Some(parent) = write_spool_recovery_state_path(workspace_path).parent() {
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_data();
        }
    }

    Ok(())
}

fn ensure_recovery_state_temp_path_is_regular_or_missing(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "write-spool recovery temp path already exists: {}",
                path.display()
            ),
        )),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "write-spool recovery temp path is not a file: {}",
                path.display()
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn ensure_recovery_state_final_path_is_regular_or_missing(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "write-spool recovery state path is not a file: {}",
                path.display()
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn ensure_recovery_state_path_has_no_symlink_components(path: &Path) -> io::Result<()> {
    if recovery_state_path_has_symlink_component(path)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing write-spool recovery state path with symlink component: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn recovery_state_path_has_symlink_component(path: &Path) -> io::Result<bool> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(true),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(false);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

/// A request to perform a write operation.
#[derive(Debug)]
pub struct WriteRequest {
    /// The write operation to perform.
    pub operation: WriteOperation,
    /// Oneshot sender for the result.
    pub response_tx: oneshot::Sender<WriteResult>,
    /// Arrival timestamp for fairness tracking.
    pub arrived_at: std::time::Instant,
}

/// Types of write operations that flow through the owner.
#[derive(Clone, Debug)]
pub enum WriteOperation {
    /// Create a new memory.
    MemoryCreate {
        workspace_id: String,
        content: String,
        level: String,
        kind: String,
        tags: Vec<String>,
    },
    /// Create a memory link.
    LinkCreate {
        workspace_id: String,
        source_id: String,
        target_id: String,
        relation: String,
    },
    /// Record feedback outcome.
    OutcomeRecord {
        workspace_id: String,
        memory_id: String,
        outcome_type: String,
        details: Option<String>,
    },
    /// Generic write for extensibility.
    Custom {
        operation_type: String,
        payload: serde_json::Value,
    },
}

impl WriteOperation {
    /// Returns a human-readable operation type string.
    #[must_use]
    pub fn operation_type(&self) -> &'static str {
        match self {
            Self::MemoryCreate { .. } => "memory_create",
            Self::LinkCreate { .. } => "link_create",
            Self::OutcomeRecord { .. } => "outcome_record",
            Self::Custom { .. } => "custom",
        }
    }
}

/// Result of a write operation.
#[derive(Clone, Debug)]
pub enum WriteResult {
    /// Operation succeeded with optional ID of created entity.
    Success { entity_id: Option<String> },
    /// Operation failed with domain error.
    Failed { error: DomainError },
    /// Write owner is shutting down.
    Shutdown,
}

impl WriteResult {
    /// Returns true if the operation succeeded.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }

    /// Returns the entity ID if present.
    #[must_use]
    pub fn entity_id(&self) -> Option<&str> {
        match self {
            Self::Success { entity_id } => entity_id.as_deref(),
            _ => None,
        }
    }
}

/// Status of the write owner actor.
#[derive(Clone, Debug, Serialize)]
pub struct WriteOwnerStatus {
    /// Schema identifier.
    pub schema: &'static str,
    /// Whether the actor is running.
    pub running: bool,
    /// Number of pending requests in the queue.
    pub queue_depth: usize,
    /// Total requests processed since start.
    pub total_processed: u64,
    /// Average wait time in milliseconds (rolling).
    pub avg_wait_ms: f64,
    /// Maximum wait time observed in milliseconds.
    pub max_wait_ms: u64,
}

impl Default for WriteOwnerStatus {
    fn default() -> Self {
        Self {
            schema: WRITE_OWNER_STATUS_SCHEMA_V1,
            running: false,
            queue_depth: 0,
            total_processed: 0,
            avg_wait_ms: 0.0,
            max_wait_ms: 0,
        }
    }
}

/// Handle for submitting write requests to the owner.
#[derive(Clone)]
pub struct WriteHandle {
    tx: mpsc::Sender<WriteRequest>,
}

impl WriteHandle {
    /// Submit a write request and wait for the result.
    ///
    /// Returns `Err` if the channel is disconnected or the operation times out.
    pub async fn submit(
        &self,
        cx: &Cx,
        operation: WriteOperation,
    ) -> Result<WriteResult, DomainError> {
        let (response_tx, mut response_rx) = oneshot::channel();
        let request = WriteRequest {
            operation,
            response_tx,
            arrived_at: std::time::Instant::now(),
        };

        // Phase 1: Reserve a slot in the channel
        let permit = self
            .tx
            .reserve(cx)
            .await
            .map_err(|e| DomainError::Storage {
                message: format!("write owner channel error: {e}"),
                repair: Some("ee diag locks --json".into()),
            })?;

        // Phase 2: Commit the request
        permit.try_send(request).map_err(|e| DomainError::Storage {
            message: format!("write owner disconnected: {e}"),
            repair: Some("Restart the write owner actor".into()),
        })?;

        // Wait for response
        response_rx
            .recv(cx)
            .await
            .map_err(|_| DomainError::Storage {
                message: "write owner response channel closed".into(),
                repair: Some("Restart the write owner actor".into()),
            })
    }

    /// Try to submit a write request without blocking.
    ///
    /// Returns `None` if the channel is full or disconnected.
    pub fn try_submit(&self, operation: WriteOperation) -> Option<oneshot::Receiver<WriteResult>> {
        let (response_tx, response_rx) = oneshot::channel();
        let request = WriteRequest {
            operation,
            response_tx,
            arrived_at: std::time::Instant::now(),
        };

        match self.tx.try_send(request) {
            Ok(()) => Some(response_rx),
            Err(_) => None,
        }
    }
}

impl fmt::Debug for WriteHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WriteHandle")
            .field("connected", &!self.tx.is_closed())
            .finish()
    }
}

/// The single-write-owner actor.
///
/// Receives write requests from multiple producers and processes them serially.
pub struct WriteOwner {
    rx: mpsc::Receiver<WriteRequest>,
    stats: WriteOwnerStats,
}

/// Internal statistics for the write owner.
#[derive(Default)]
struct WriteOwnerStats {
    total_processed: u64,
    total_wait_ms: u64,
    max_wait_ms: u64,
}

impl WriteOwner {
    /// Create a new write owner with the given channel capacity.
    ///
    /// Returns the owner and a clonable handle for submitting requests.
    #[must_use]
    pub fn new(capacity: usize) -> (Self, WriteHandle) {
        let (tx, rx) = mpsc::channel(capacity);
        let owner = Self {
            rx,
            stats: WriteOwnerStats::default(),
        };
        let handle = WriteHandle { tx };
        (owner, handle)
    }

    /// Create a new write owner with default capacity.
    #[must_use]
    pub fn with_default_capacity() -> (Self, WriteHandle) {
        Self::new(DEFAULT_CHANNEL_CAPACITY)
    }

    /// Run the write owner actor loop.
    ///
    /// This method processes requests until the channel is closed or cancelled.
    /// The `process` callback is invoked for each operation.
    pub async fn run<F>(mut self, cx: &Cx, mut process: F)
    where
        F: FnMut(WriteOperation) -> WriteResult,
    {
        while let Ok(request) = self.rx.recv(cx).await {
            let wait_ms = request.arrived_at.elapsed().as_millis() as u64;
            self.stats.total_processed += 1;
            self.stats.total_wait_ms += wait_ms;
            if wait_ms > self.stats.max_wait_ms {
                self.stats.max_wait_ms = wait_ms;
            }

            let result = process(request.operation);

            // Send response (ignore if receiver dropped)
            let _ = request.response_tx.send(cx, result);
        }
    }

    /// Get current status of the write owner.
    #[must_use]
    pub fn status(&self) -> WriteOwnerStatus {
        let avg_wait_ms = if self.stats.total_processed > 0 {
            self.stats.total_wait_ms as f64 / self.stats.total_processed as f64
        } else {
            0.0
        };

        WriteOwnerStatus {
            schema: WRITE_OWNER_STATUS_SCHEMA_V1,
            running: true,
            queue_depth: self.rx.len(),
            total_processed: self.stats.total_processed,
            avg_wait_ms,
            max_wait_ms: self.stats.max_wait_ms,
        }
    }
}

/// Error returned when the write owner is busy.
#[derive(Clone, Debug, Serialize)]
pub struct WriteOwnerBusyError {
    /// Schema identifier.
    pub schema: &'static str,
    /// Error code.
    pub code: &'static str,
    /// Human-readable message.
    pub message: String,
    /// Current queue depth.
    pub queue_depth: usize,
    /// Suggested repair action.
    pub repair: &'static str,
}

impl WriteOwnerBusyError {
    /// Create a new busy error with the given queue depth.
    #[must_use]
    pub fn new(queue_depth: usize) -> Self {
        Self {
            schema: WRITE_OWNER_BUSY_SCHEMA_V1,
            code: WRITE_OWNER_BUSY_CODE,
            message: format!(
                "Write owner is busy with {queue_depth} pending requests. Try again later."
            ),
            queue_depth,
            repair: "ee diag locks --json",
        }
    }
}

impl fmt::Display for WriteOwnerBusyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for WriteOwnerBusyError {}

/// Configuration for the batched write spool.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteSpoolConfig {
    /// Maximum number of writes waiting for the owner.
    pub max_pending: usize,
    /// Maximum writes in one coalesced batch.
    pub max_batch_size: usize,
    /// Maximum payload bytes waiting for the owner.
    pub max_pending_bytes: usize,
    /// Maximum permitted age for the oldest queued write.
    pub max_queue_age_ms: u64,
}

impl Default for WriteSpoolConfig {
    fn default() -> Self {
        Self {
            max_pending: DEFAULT_SPOOL_MAX_PENDING,
            max_batch_size: DEFAULT_SPOOL_MAX_BATCH_SIZE,
            max_pending_bytes: DEFAULT_SPOOL_MAX_PENDING_BYTES,
            max_queue_age_ms: DEFAULT_SPOOL_QUEUE_TIMEOUT_MS,
        }
    }
}

impl WriteSpoolConfig {
    /// Create a test-friendly config with explicit limits.
    #[must_use]
    pub const fn new(
        max_pending: usize,
        max_batch_size: usize,
        max_pending_bytes: usize,
        max_queue_age_ms: u64,
    ) -> Self {
        Self {
            max_pending,
            max_batch_size,
            max_pending_bytes,
            max_queue_age_ms,
        }
    }

    fn effective_batch_size(&self) -> usize {
        self.max_batch_size.max(1)
    }
}

/// Durable write categories accepted by the spool.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteSpoolIntentKind {
    /// `ee remember` memory write.
    Remember,
    /// `ee outcome` feedback write.
    Outcome,
    /// CASS/import checkpoint or imported row write.
    Import,
    /// Recorder event or transcript write.
    Recorder,
}

impl WriteSpoolIntentKind {
    /// Stable machine string for JSON, audit rows, and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Remember => "remember",
            Self::Outcome => "outcome",
            Self::Import => "import",
            Self::Recorder => "recorder",
        }
    }

    /// Default durability class for this write category.
    #[must_use]
    pub const fn default_durability(self) -> WriteSpoolDurability {
        match self {
            Self::Import => WriteSpoolDurability::Immediate,
            Self::Remember | Self::Outcome | Self::Recorder => WriteSpoolDurability::Batched,
        }
    }
}

/// Whether a write may be coalesced with matching writes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteSpoolDurability {
    /// May share a transaction with matching writes.
    Batched,
    /// Must become its own durable batch boundary.
    Immediate,
}

impl WriteSpoolDurability {
    /// Stable machine string for JSON and audit rows.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Batched => "batched",
            Self::Immediate => "immediate",
        }
    }
}

/// Write request accepted by the batched spool.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteSpoolIntent {
    /// Idempotency key supplied by the caller.
    pub idempotency_key: String,
    /// Workspace this write mutates.
    pub workspace_id: String,
    /// Write category.
    pub kind: WriteSpoolIntentKind,
    /// Durability and batching behavior.
    pub durability: WriteSpoolDurability,
    /// Approximate serialized payload size for budget accounting.
    pub payload_bytes: usize,
    /// Stable audit subject written alongside the batch boundary.
    pub audit_subject: String,
}

impl WriteSpoolIntent {
    /// Build a write intent with the default durability for its kind.
    #[must_use]
    pub fn new(
        kind: WriteSpoolIntentKind,
        workspace_id: impl Into<String>,
        idempotency_key: impl Into<String>,
        payload_bytes: usize,
    ) -> Self {
        let idempotency_key = idempotency_key.into();
        Self {
            idempotency_key: idempotency_key.clone(),
            workspace_id: workspace_id.into(),
            kind,
            durability: kind.default_durability(),
            payload_bytes,
            audit_subject: format!("{}:{idempotency_key}", kind.as_str()),
        }
    }

    /// Force immediate durability for a write that normally batches.
    #[must_use]
    pub const fn immediate(mut self) -> Self {
        self.durability = WriteSpoolDurability::Immediate;
        self
    }

    /// Force batched durability for a write that normally commits alone.
    #[must_use]
    pub const fn batched(mut self) -> Self {
        self.durability = WriteSpoolDurability::Batched;
        self
    }

    /// Override the audit subject used in batch metadata.
    #[must_use]
    pub fn with_audit_subject(mut self, audit_subject: impl Into<String>) -> Self {
        self.audit_subject = audit_subject.into();
        self
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct WriteSpoolBatchKey {
    workspace_id: String,
    kind: WriteSpoolIntentKind,
    durability: WriteSpoolDurability,
}

/// Durable state for a spooled write after crash recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteSpoolRecordStatus {
    /// Accepted by the spool but not durably committed.
    Pending,
    /// Committed by the write owner.
    Committed,
    /// Cancelled before commit.
    Cancelled,
    /// Failed during commit.
    Failed,
}

impl WriteSpoolRecordStatus {
    /// Stable machine string for JSON and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Committed => "committed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    const fn is_terminal(self) -> bool {
        matches!(self, Self::Committed | Self::Cancelled | Self::Failed)
    }
}

/// Persistent recovery record for one spooled write.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteSpoolRecord {
    /// Monotonic request ID assigned by the spool.
    pub request_id: u64,
    /// Caller-supplied idempotency key.
    pub idempotency_key: String,
    /// Workspace this write mutates.
    pub workspace_id: String,
    /// Write category.
    pub kind: WriteSpoolIntentKind,
    /// Durability and batching behavior.
    pub durability: WriteSpoolDurability,
    /// Current durable state.
    pub status: WriteSpoolRecordStatus,
    /// Batch ID assigned when the write owner drains the record.
    pub batch_id: Option<u64>,
    /// Virtual or wall-clock enqueue time in milliseconds.
    pub enqueued_at_ms: u64,
    /// Terminal timestamp when committed, cancelled, or failed.
    pub terminal_at_ms: Option<u64>,
    /// Approximate serialized payload size.
    pub payload_bytes: usize,
    /// Stable audit subject emitted with the batch.
    pub audit_subject: String,
    /// Failure message when status is failed.
    pub failure: Option<String>,
}

impl WriteSpoolRecord {
    fn from_intent(request_id: u64, intent: WriteSpoolIntent, enqueued_at_ms: u64) -> Self {
        Self {
            request_id,
            idempotency_key: intent.idempotency_key,
            workspace_id: intent.workspace_id,
            kind: intent.kind,
            durability: intent.durability,
            status: WriteSpoolRecordStatus::Pending,
            batch_id: None,
            enqueued_at_ms,
            terminal_at_ms: None,
            payload_bytes: intent.payload_bytes,
            audit_subject: intent.audit_subject,
            failure: None,
        }
    }

    fn batch_key(&self) -> WriteSpoolBatchKey {
        WriteSpoolBatchKey {
            workspace_id: self.workspace_id.clone(),
            kind: self.kind,
            durability: self.durability,
        }
    }
}

/// Ticket returned by enqueue, including idempotent duplicate detection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteSpoolTicket {
    /// Monotonic request ID assigned to this idempotency key.
    pub request_id: u64,
    /// Caller-supplied idempotency key.
    pub idempotency_key: String,
    /// True when enqueue reused an existing idempotency key.
    pub duplicate: bool,
    /// Current state of the existing or new record.
    pub status: WriteSpoolRecordStatus,
}

/// Batch boundary handed to the single write owner.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteSpoolBatch {
    /// Monotonic batch ID.
    pub batch_id: u64,
    /// Workspace shared by every row in this batch.
    pub workspace_id: String,
    /// Write category shared by every row in this batch.
    pub kind: WriteSpoolIntentKind,
    /// Durability class for this boundary.
    pub durability: WriteSpoolDurability,
    /// Request IDs included in FIFO order.
    pub request_ids: Vec<u64>,
    /// Audit subjects included in FIFO order.
    pub audit_subjects: Vec<String>,
    /// Stable audit row ID for this batch boundary.
    pub audit_row_id: String,
    /// Stable job row ID for this batch boundary.
    pub job_row_id: String,
}

impl WriteSpoolBatch {
    /// Number of write rows in this batch.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.request_ids.len()
    }
}

/// Reason a caller hit write-spool backpressure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteSpoolBackpressureReason {
    /// Queue depth exceeded configured budget.
    QueueDepth,
    /// Pending payload bytes exceeded configured budget.
    PendingBytes,
    /// Oldest queued write exceeded age budget.
    QueueTimeout,
}

impl WriteSpoolBackpressureReason {
    /// Stable machine string for JSON and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::QueueDepth => "queue_depth",
            Self::PendingBytes => "pending_bytes",
            Self::QueueTimeout => "queue_timeout",
        }
    }
}

/// JSON-serializable error returned when the spool refuses more writes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteSpoolBackpressureError {
    /// Schema identifier.
    pub schema: &'static str,
    /// Error code.
    pub code: &'static str,
    /// Machine-readable budget reason.
    pub reason: WriteSpoolBackpressureReason,
    /// Human-readable message.
    pub message: String,
    /// Current queue depth.
    pub queue_depth: usize,
    /// Queue depth limit.
    pub max_pending: usize,
    /// Current pending payload bytes.
    pub pending_bytes: usize,
    /// Pending payload byte limit.
    pub max_pending_bytes: usize,
    /// Age of the oldest pending write, if any.
    pub oldest_queued_age_ms: Option<u64>,
    /// Suggested repair command.
    pub repair: &'static str,
    /// Suggested next diagnostic command.
    pub next: &'static str,
}

impl WriteSpoolBackpressureError {
    fn new(
        reason: WriteSpoolBackpressureReason,
        status: &WriteSpoolStatus,
        config: &WriteSpoolConfig,
    ) -> Self {
        let message = match reason {
            WriteSpoolBackpressureReason::QueueDepth => format!(
                "Write spool queue depth {} exceeded the configured limit {}.",
                status.queue_depth, config.max_pending
            ),
            WriteSpoolBackpressureReason::PendingBytes => format!(
                "Write spool has {} pending bytes, exceeding the configured limit {}.",
                status.pending_bytes, config.max_pending_bytes
            ),
            WriteSpoolBackpressureReason::QueueTimeout => format!(
                "Write spool oldest queued write is {} ms old, exceeding the configured limit {} ms.",
                status.oldest_queued_age_ms.unwrap_or(0),
                config.max_queue_age_ms
            ),
        };

        Self {
            schema: WRITE_SPOOL_BACKPRESSURE_SCHEMA_V1,
            code: WRITE_SPOOL_BACKPRESSURE_CODE,
            reason,
            message,
            queue_depth: status.queue_depth,
            max_pending: config.max_pending,
            pending_bytes: status.pending_bytes,
            max_pending_bytes: config.max_pending_bytes,
            oldest_queued_age_ms: status.oldest_queued_age_ms,
            repair: "ee daemon status --json",
            next: "ee support-bundle create --include write-queue --json",
        }
    }
}

impl fmt::Display for WriteSpoolBackpressureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for WriteSpoolBackpressureError {}

type WriteSpoolBackpressureResult<T> = Result<T, Box<WriteSpoolBackpressureError>>;

/// Last failed write metadata for status/support bundles.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteSpoolFailure {
    /// Failed request ID.
    pub request_id: u64,
    /// Failed idempotency key.
    pub idempotency_key: String,
    /// Failure message.
    pub message: String,
    /// Failure timestamp in milliseconds.
    pub failed_at_ms: u64,
}

/// Status exposed by `status` and support-bundle diagnostics.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteSpoolStatus {
    /// Schema identifier.
    pub schema: &'static str,
    /// Number of records waiting to be drained.
    pub queue_depth: usize,
    /// Approximate queued payload bytes.
    pub pending_bytes: usize,
    /// Age of the oldest queued write.
    pub oldest_queued_age_ms: Option<u64>,
    /// Queue depth limit.
    pub max_pending: usize,
    /// Pending payload byte limit.
    pub max_pending_bytes: usize,
    /// Queue age limit.
    pub max_queue_age_ms: u64,
    /// Total unique writes accepted.
    pub total_enqueued: u64,
    /// Total rows committed.
    pub total_committed: u64,
    /// Total rows cancelled.
    pub total_cancelled: u64,
    /// Total rows failed.
    pub total_failed: u64,
    /// Total batches emitted to the write owner.
    pub total_batches: u64,
    /// Size of the most recent batch.
    pub last_batch_size: usize,
    /// Largest batch emitted since start.
    pub max_batch_size_observed: usize,
    /// Committed rows per second since the spool started.
    pub rows_per_sec: f64,
    /// Most recent failure, if any.
    pub last_failure: Option<WriteSpoolFailure>,
}

/// Deterministic batched write spool for daemon/write-owner mode.
#[derive(Clone, Debug)]
pub struct WriteSpool {
    config: WriteSpoolConfig,
    next_request_id: u64,
    next_batch_id: u64,
    started_at_ms: u64,
    pending_order: VecDeque<u64>,
    records: Vec<WriteSpoolRecord>,
    idempotency: HashMap<String, u64>,
    pending_bytes: usize,
    stats: WriteSpoolStats,
}

#[derive(Clone, Debug, Default)]
struct WriteSpoolStats {
    total_enqueued: u64,
    total_committed: u64,
    total_cancelled: u64,
    total_failed: u64,
    total_batches: u64,
    last_batch_size: usize,
    max_batch_size_observed: usize,
    last_failure: Option<WriteSpoolFailure>,
}

impl WriteSpool {
    /// Create an empty spool.
    #[must_use]
    pub fn new(config: WriteSpoolConfig, started_at_ms: u64) -> Self {
        Self {
            config,
            next_request_id: 1,
            next_batch_id: 1,
            started_at_ms,
            pending_order: VecDeque::new(),
            records: Vec::new(),
            idempotency: HashMap::new(),
            pending_bytes: 0,
            stats: WriteSpoolStats::default(),
        }
    }

    /// Rebuild in-memory queue state from persisted recovery records.
    #[must_use]
    pub fn from_recovery_records(
        config: WriteSpoolConfig,
        started_at_ms: u64,
        records: Vec<WriteSpoolRecord>,
    ) -> Self {
        let mut pending_order = VecDeque::new();
        let mut idempotency = HashMap::new();
        let mut pending_bytes = 0usize;
        let mut stats = WriteSpoolStats::default();
        let mut next_request_id = 1u64;
        let mut next_batch_id = 1u64;

        for record in &records {
            next_request_id = next_request_id.max(record.request_id.saturating_add(1));
            if let Some(batch_id) = record.batch_id {
                next_batch_id = next_batch_id.max(batch_id.saturating_add(1));
            }
            idempotency.insert(record.idempotency_key.clone(), record.request_id);

            match record.status {
                WriteSpoolRecordStatus::Pending => {
                    pending_order.push_back(record.request_id);
                    pending_bytes = pending_bytes.saturating_add(record.payload_bytes);
                    stats.total_enqueued = stats.total_enqueued.saturating_add(1);
                }
                WriteSpoolRecordStatus::Committed => {
                    stats.total_enqueued = stats.total_enqueued.saturating_add(1);
                    stats.total_committed = stats.total_committed.saturating_add(1);
                }
                WriteSpoolRecordStatus::Cancelled => {
                    stats.total_enqueued = stats.total_enqueued.saturating_add(1);
                    stats.total_cancelled = stats.total_cancelled.saturating_add(1);
                }
                WriteSpoolRecordStatus::Failed => {
                    stats.total_enqueued = stats.total_enqueued.saturating_add(1);
                    stats.total_failed = stats.total_failed.saturating_add(1);
                    if let (Some(message), Some(failed_at_ms)) =
                        (&record.failure, record.terminal_at_ms)
                    {
                        stats.last_failure = Some(WriteSpoolFailure {
                            request_id: record.request_id,
                            idempotency_key: record.idempotency_key.clone(),
                            message: message.clone(),
                            failed_at_ms,
                        });
                    }
                }
            }
        }

        Self {
            config,
            next_request_id,
            next_batch_id,
            started_at_ms,
            pending_order,
            records,
            idempotency,
            pending_bytes,
            stats,
        }
    }

    /// Enqueue a write intent or return the existing idempotency ticket.
    pub fn enqueue(
        &mut self,
        intent: WriteSpoolIntent,
        now_ms: u64,
    ) -> WriteSpoolBackpressureResult<WriteSpoolTicket> {
        if let Some(request_id) = self.idempotency.get(&intent.idempotency_key).copied() {
            if let Some(record) = self.record(request_id) {
                return Ok(WriteSpoolTicket {
                    request_id,
                    idempotency_key: record.idempotency_key.clone(),
                    duplicate: true,
                    status: record.status,
                });
            }
            self.idempotency.remove(&intent.idempotency_key);
        }

        self.ensure_accepting(intent.payload_bytes, now_ms)?;

        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);

        let record = WriteSpoolRecord::from_intent(request_id, intent, now_ms);
        self.pending_bytes = self.pending_bytes.saturating_add(record.payload_bytes);
        self.pending_order.push_back(request_id);
        self.idempotency
            .insert(record.idempotency_key.clone(), request_id);
        self.stats.total_enqueued = self.stats.total_enqueued.saturating_add(1);

        let ticket = WriteSpoolTicket {
            request_id,
            idempotency_key: record.idempotency_key.clone(),
            duplicate: false,
            status: record.status,
        };
        self.records.push(record);
        Ok(ticket)
    }

    /// Drain the next FIFO-compatible batch.
    #[must_use]
    pub fn next_batch(&mut self) -> Option<WriteSpoolBatch> {
        let (first_id, first) = loop {
            let first_id = self.pending_order.pop_front()?;
            if let Some(record) = self.record(first_id) {
                break (first_id, record.clone());
            }
        };
        let key = first.batch_key();
        let mut selected = vec![first_id];

        if key.durability == WriteSpoolDurability::Batched {
            let mut retained = VecDeque::with_capacity(self.pending_order.len());
            while let Some(request_id) = self.pending_order.pop_front() {
                let should_batch = selected.len() < self.config.effective_batch_size()
                    && self
                        .record(request_id)
                        .is_some_and(|record| record.batch_key() == key);
                if should_batch {
                    selected.push(request_id);
                } else {
                    retained.push_back(request_id);
                }
            }
            self.pending_order = retained;
        }

        let batch_id = self.next_batch_id;
        self.next_batch_id = self.next_batch_id.saturating_add(1);

        let mut audit_subjects = Vec::with_capacity(selected.len());
        let mut request_ids = Vec::with_capacity(selected.len());
        for request_id in &selected {
            let (payload_bytes, audit_subject) = {
                let Some(record) = self.record_mut(*request_id) else {
                    continue;
                };
                record.batch_id = Some(batch_id);
                (record.payload_bytes, record.audit_subject.clone())
            };
            self.pending_bytes = self.pending_bytes.saturating_sub(payload_bytes);
            request_ids.push(*request_id);
            audit_subjects.push(audit_subject);
        }
        if request_ids.is_empty() {
            return None;
        }

        self.stats.total_batches = self.stats.total_batches.saturating_add(1);
        self.stats.last_batch_size = request_ids.len();
        self.stats.max_batch_size_observed =
            self.stats.max_batch_size_observed.max(request_ids.len());

        Some(WriteSpoolBatch {
            batch_id,
            workspace_id: key.workspace_id,
            kind: key.kind,
            durability: key.durability,
            request_ids,
            audit_subjects,
            audit_row_id: format!("audit_batch_{batch_id:016}"),
            job_row_id: format!("job_batch_{batch_id:016}"),
        })
    }

    /// Mark every pending record in the batch committed.
    pub fn mark_batch_committed(&mut self, batch_id: u64, now_ms: u64) -> usize {
        let mut committed = 0usize;
        for record in &mut self.records {
            if record.batch_id == Some(batch_id) && record.status == WriteSpoolRecordStatus::Pending
            {
                record.status = WriteSpoolRecordStatus::Committed;
                record.terminal_at_ms = Some(now_ms);
                committed += 1;
            }
        }
        self.stats.total_committed = self.stats.total_committed.saturating_add(committed as u64);
        committed
    }

    /// Mark every pending record in the batch failed.
    pub fn mark_batch_failed(
        &mut self,
        batch_id: u64,
        now_ms: u64,
        message: impl Into<String>,
    ) -> usize {
        let message = message.into();
        let mut failed = 0usize;
        let mut last_failure = None;
        for record in &mut self.records {
            if record.batch_id == Some(batch_id) && record.status == WriteSpoolRecordStatus::Pending
            {
                record.status = WriteSpoolRecordStatus::Failed;
                record.terminal_at_ms = Some(now_ms);
                record.failure = Some(message.clone());
                failed += 1;
                last_failure = Some(WriteSpoolFailure {
                    request_id: record.request_id,
                    idempotency_key: record.idempotency_key.clone(),
                    message: message.clone(),
                    failed_at_ms: now_ms,
                });
            }
        }
        self.stats.total_failed = self.stats.total_failed.saturating_add(failed as u64);
        if last_failure.is_some() {
            self.stats.last_failure = last_failure;
        }
        failed
    }

    /// Cancel a pending record by request ID.
    pub fn cancel_pending(&mut self, request_id: u64, now_ms: u64) -> bool {
        let Some(index) = self.records.iter().position(|r| r.request_id == request_id) else {
            return false;
        };
        if self.records[index].status.is_terminal() {
            return false;
        }

        self.pending_order
            .retain(|queued_id| *queued_id != request_id);
        if self.records[index].batch_id.is_none() {
            self.pending_bytes = self
                .pending_bytes
                .saturating_sub(self.records[index].payload_bytes);
        }
        self.records[index].status = WriteSpoolRecordStatus::Cancelled;
        self.records[index].terminal_at_ms = Some(now_ms);
        self.stats.total_cancelled = self.stats.total_cancelled.saturating_add(1);
        true
    }

    /// Return stable recovery records for persistence or support bundles.
    #[must_use]
    pub fn recovery_records(&self) -> Vec<WriteSpoolRecord> {
        let mut records = self.records.clone();
        records.sort_by_key(|record| record.request_id);
        records
    }

    /// Current status for `ee status` and support bundles.
    #[must_use]
    pub fn status(&self, now_ms: u64) -> WriteSpoolStatus {
        let elapsed_ms = now_ms.saturating_sub(self.started_at_ms);
        let rows_per_sec = if elapsed_ms == 0 {
            0.0
        } else {
            self.stats.total_committed as f64 / (elapsed_ms as f64 / 1_000.0)
        };

        WriteSpoolStatus {
            schema: WRITE_SPOOL_STATUS_SCHEMA_V1,
            queue_depth: self.pending_order.len(),
            pending_bytes: self.pending_bytes,
            oldest_queued_age_ms: self.oldest_queued_age_ms(now_ms),
            max_pending: self.config.max_pending,
            max_pending_bytes: self.config.max_pending_bytes,
            max_queue_age_ms: self.config.max_queue_age_ms,
            total_enqueued: self.stats.total_enqueued,
            total_committed: self.stats.total_committed,
            total_cancelled: self.stats.total_cancelled,
            total_failed: self.stats.total_failed,
            total_batches: self.stats.total_batches,
            last_batch_size: self.stats.last_batch_size,
            max_batch_size_observed: self.stats.max_batch_size_observed,
            rows_per_sec,
            last_failure: self.stats.last_failure.clone(),
        }
    }

    /// Look up a record by request ID.
    #[must_use]
    pub fn record(&self, request_id: u64) -> Option<&WriteSpoolRecord> {
        self.records
            .iter()
            .find(|record| record.request_id == request_id)
    }

    fn record_mut(&mut self, request_id: u64) -> Option<&mut WriteSpoolRecord> {
        self.records
            .iter_mut()
            .find(|record| record.request_id == request_id)
    }

    fn ensure_accepting(
        &self,
        additional_bytes: usize,
        now_ms: u64,
    ) -> WriteSpoolBackpressureResult<()> {
        let status = self.status(now_ms);
        if status.queue_depth >= self.config.max_pending {
            return Err(Box::new(WriteSpoolBackpressureError::new(
                WriteSpoolBackpressureReason::QueueDepth,
                &status,
                &self.config,
            )));
        }
        if self.pending_bytes.saturating_add(additional_bytes) > self.config.max_pending_bytes {
            return Err(Box::new(WriteSpoolBackpressureError::new(
                WriteSpoolBackpressureReason::PendingBytes,
                &status,
                &self.config,
            )));
        }
        if status
            .oldest_queued_age_ms
            .is_some_and(|age_ms| age_ms > self.config.max_queue_age_ms)
        {
            return Err(Box::new(WriteSpoolBackpressureError::new(
                WriteSpoolBackpressureReason::QueueTimeout,
                &status,
                &self.config,
            )));
        }
        Ok(())
    }

    fn oldest_queued_age_ms(&self, now_ms: u64) -> Option<u64> {
        self.pending_order
            .front()
            .and_then(|request_id| self.record(*request_id))
            .map(|record| now_ms.saturating_sub(record.enqueued_at_ms))
    }
}

#[cfg(test)]
// Write-owner tests use expect for fixture-only assertions around queued intents.
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use proptest::test_runner::{Config as ProptestConfig, TestCaseError};
    use std::collections::{BTreeMap, BTreeSet, VecDeque};

    #[derive(Clone, Debug)]
    struct ScheduledSpoolWrite {
        producer_id: u8,
        kind: WriteSpoolIntentKind,
        payload_bytes: usize,
        cancel_before_drain: bool,
    }

    fn scheduled_spool_write_strategy() -> impl Strategy<Value = ScheduledSpoolWrite> {
        (0_u8..8, 0_u8..4, 1_usize..512, proptest::bool::ANY).prop_map(
            |(producer_id, kind_index, payload_bytes, cancel_before_drain)| {
                let kind = match kind_index {
                    0 => WriteSpoolIntentKind::Remember,
                    1 => WriteSpoolIntentKind::Outcome,
                    2 => WriteSpoolIntentKind::Import,
                    _ => WriteSpoolIntentKind::Recorder,
                };
                ScheduledSpoolWrite {
                    producer_id,
                    kind,
                    payload_bytes,
                    cancel_before_drain,
                }
            },
        )
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Srr3CancellationPoint {
        None,
        BeforeEnqueue,
        AfterEnqueueBeforeCommit,
        DuringBatchAssembly,
        AfterCommit,
    }

    #[derive(Clone, Debug)]
    struct Srr3ScheduledWrite {
        producer_id: u8,
        kind: WriteSpoolIntentKind,
        payload_bytes: usize,
        cancellation_point: Srr3CancellationPoint,
    }

    #[derive(Clone, Debug)]
    struct Srr3PropertySchedule {
        max_batch_size: usize,
        writes: Vec<Srr3ScheduledWrite>,
        fsync_failure_batches: BTreeSet<u64>,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct Srr3WriteMetadata {
        producer_id: u8,
        producer_sequence: u16,
        kind: WriteSpoolIntentKind,
        payload_bytes: usize,
        cancellation_point: Srr3CancellationPoint,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct Srr3DurableRow {
        request_id: u64,
        producer_id: u8,
        producer_sequence: u16,
        batch_id: u64,
        kind: WriteSpoolIntentKind,
        payload_bytes: usize,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct Srr3ModeledResult {
        durable_rows: Vec<Srr3DurableRow>,
        audit_chain_hashes: Vec<String>,
        published_snapshots: Vec<u64>,
    }

    #[derive(Clone, Debug)]
    struct Srr3ReferenceRecord {
        request_id: u64,
        metadata: Srr3WriteMetadata,
        durability: WriteSpoolDurability,
        audit_subject: String,
        status: WriteSpoolRecordStatus,
        batch_id: Option<u64>,
    }

    impl Srr3ReferenceRecord {
        fn kind(&self) -> WriteSpoolIntentKind {
            self.metadata.kind
        }
    }

    fn srr3_property_schedule_strategy() -> impl Strategy<Value = Srr3PropertySchedule> {
        (1_u8..=32, 0_usize..=1000, 1_usize..=64).prop_flat_map(
            |(producer_count, write_count, max_batch_size)| {
                let write = (0_u8..producer_count, 0_u8..4, 1_usize..256, 0_u8..5).prop_map(
                    |(producer_id, kind_index, payload_bytes, cancellation_index)| {
                        let kind = match kind_index {
                            0 => WriteSpoolIntentKind::Remember,
                            1 => WriteSpoolIntentKind::Outcome,
                            2 => WriteSpoolIntentKind::Import,
                            _ => WriteSpoolIntentKind::Recorder,
                        };
                        let cancellation_point = match cancellation_index {
                            0 => Srr3CancellationPoint::None,
                            1 => Srr3CancellationPoint::BeforeEnqueue,
                            2 => Srr3CancellationPoint::AfterEnqueueBeforeCommit,
                            3 => Srr3CancellationPoint::DuringBatchAssembly,
                            _ => Srr3CancellationPoint::AfterCommit,
                        };
                        Srr3ScheduledWrite {
                            producer_id,
                            kind,
                            payload_bytes,
                            cancellation_point,
                        }
                    },
                );

                (
                    prop::collection::vec(write, write_count),
                    prop::collection::btree_set(1_u64..=1000, 0..32),
                )
                    .prop_map(move |(writes, fsync_failure_batches)| {
                        Srr3PropertySchedule {
                            max_batch_size,
                            writes,
                            fsync_failure_batches,
                        }
                    })
            },
        )
    }

    fn srr3_intent(
        kind: WriteSpoolIntentKind,
        producer_id: u8,
        producer_sequence: u16,
        payload_bytes: usize,
    ) -> WriteSpoolIntent {
        WriteSpoolIntent::new(
            kind,
            "workspace",
            format!("p{producer_id:02}-s{producer_sequence:04}"),
            payload_bytes,
        )
    }

    fn srr3_audit_chain_hash(
        previous: &str,
        batch_id: u64,
        outcome: &str,
        request_ids: &[u64],
        audit_subjects: &[String],
    ) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(previous.as_bytes());
        hasher.update(&batch_id.to_be_bytes());
        hasher.update(outcome.as_bytes());
        for request_id in request_ids {
            hasher.update(&request_id.to_be_bytes());
        }
        for audit_subject in audit_subjects {
            hasher.update(audit_subject.as_bytes());
            hasher.update(b"\0");
        }
        hasher.finalize().to_hex().to_string()
    }

    fn srr3_outcome(
        failed: bool,
        committed_count: usize,
        cancelled_count: usize,
        row_count: usize,
    ) -> &'static str {
        if failed {
            "failed"
        } else if committed_count > 0 {
            "committed"
        } else if cancelled_count == row_count {
            "cancelled"
        } else {
            "empty"
        }
    }

    fn srr3_batch_key_matches(
        record: &Srr3ReferenceRecord,
        workspace_id: &str,
        kind: WriteSpoolIntentKind,
        durability: WriteSpoolDurability,
    ) -> bool {
        record.status == WriteSpoolRecordStatus::Pending
            && record.kind() == kind
            && record.durability == durability
            && workspace_id == "workspace"
    }

    fn srr3_durable_rows_from_reference(records: &[Srr3ReferenceRecord]) -> Vec<Srr3DurableRow> {
        records
            .iter()
            .filter(|record| record.status == WriteSpoolRecordStatus::Committed)
            .filter_map(|record| {
                Some(Srr3DurableRow {
                    request_id: record.request_id,
                    producer_id: record.metadata.producer_id,
                    producer_sequence: record.metadata.producer_sequence,
                    batch_id: record.batch_id?,
                    kind: record.metadata.kind,
                    payload_bytes: record.metadata.payload_bytes,
                })
            })
            .collect()
    }

    fn srr3_durable_rows_from_spool(
        records: &[WriteSpoolRecord],
        metadata: &BTreeMap<u64, Srr3WriteMetadata>,
    ) -> Vec<Srr3DurableRow> {
        records
            .iter()
            .filter(|record| record.status == WriteSpoolRecordStatus::Committed)
            .filter_map(|record| {
                let metadata = metadata.get(&record.request_id)?;
                Some(Srr3DurableRow {
                    request_id: record.request_id,
                    producer_id: metadata.producer_id,
                    producer_sequence: metadata.producer_sequence,
                    batch_id: record.batch_id?,
                    kind: metadata.kind,
                    payload_bytes: metadata.payload_bytes,
                })
            })
            .collect()
    }

    fn interpret_srr3_reference(schedule: &Srr3PropertySchedule) -> Srr3ModeledResult {
        let mut producer_sequences = BTreeMap::<u8, u16>::new();
        let mut records = Vec::<Srr3ReferenceRecord>::new();
        let mut pending_order = VecDeque::<u64>::new();
        let mut next_request_id = 1_u64;

        for write in &schedule.writes {
            let sequence = producer_sequences.entry(write.producer_id).or_default();
            let producer_sequence = *sequence;
            *sequence = sequence.saturating_add(1);

            if write.cancellation_point == Srr3CancellationPoint::BeforeEnqueue {
                continue;
            }

            let intent = srr3_intent(
                write.kind,
                write.producer_id,
                producer_sequence,
                write.payload_bytes,
            );
            let request_id = next_request_id;
            next_request_id = next_request_id.saturating_add(1);
            let metadata = Srr3WriteMetadata {
                producer_id: write.producer_id,
                producer_sequence,
                kind: write.kind,
                payload_bytes: write.payload_bytes,
                cancellation_point: write.cancellation_point,
            };
            records.push(Srr3ReferenceRecord {
                request_id,
                metadata,
                durability: intent.durability,
                audit_subject: intent.audit_subject,
                status: WriteSpoolRecordStatus::Pending,
                batch_id: None,
            });
            pending_order.push_back(request_id);

            if write.cancellation_point == Srr3CancellationPoint::AfterEnqueueBeforeCommit {
                pending_order.retain(|queued_id| *queued_id != request_id);
                if let Some(record) = records
                    .iter_mut()
                    .find(|record| record.request_id == request_id)
                {
                    record.status = WriteSpoolRecordStatus::Cancelled;
                }
            }
        }

        let mut next_batch_id = 1_u64;
        let mut audit_chain_hashes = Vec::<String>::new();
        let mut previous_audit_hash = String::new();
        let mut published_snapshots = Vec::<u64>::new();
        let mut snapshot_generation = 0_u64;

        while let Some(first_id) = pending_order.pop_front() {
            let Some(first) = records
                .iter()
                .find(|record| {
                    record.request_id == first_id
                        && record.status == WriteSpoolRecordStatus::Pending
                })
                .cloned()
            else {
                continue;
            };

            let mut selected = vec![first_id];
            if first.durability == WriteSpoolDurability::Batched {
                let mut retained = VecDeque::with_capacity(pending_order.len());
                while let Some(request_id) = pending_order.pop_front() {
                    let should_batch = selected.len() < schedule.max_batch_size.max(1)
                        && records
                            .iter()
                            .find(|record| record.request_id == request_id)
                            .is_some_and(|record| {
                                srr3_batch_key_matches(
                                    record,
                                    "workspace",
                                    first.kind(),
                                    first.durability,
                                )
                            });
                    if should_batch {
                        selected.push(request_id);
                    } else {
                        retained.push_back(request_id);
                    }
                }
                pending_order = retained;
            }

            let batch_id = next_batch_id;
            next_batch_id = next_batch_id.saturating_add(1);
            let mut audit_subjects = Vec::with_capacity(selected.len());
            for request_id in &selected {
                if let Some(record) = records
                    .iter_mut()
                    .find(|record| record.request_id == *request_id)
                {
                    record.batch_id = Some(batch_id);
                    audit_subjects.push(record.audit_subject.clone());
                    if record.metadata.cancellation_point
                        == Srr3CancellationPoint::DuringBatchAssembly
                    {
                        record.status = WriteSpoolRecordStatus::Cancelled;
                    }
                }
            }

            let failed = schedule.fsync_failure_batches.contains(&batch_id)
                && selected.iter().any(|request_id| {
                    records.iter().any(|record| {
                        record.request_id == *request_id
                            && record.status == WriteSpoolRecordStatus::Pending
                    })
                });
            let mut committed_count = 0_usize;
            let mut cancelled_count = 0_usize;
            for request_id in &selected {
                let Some(record) = records
                    .iter_mut()
                    .find(|record| record.request_id == *request_id)
                else {
                    continue;
                };
                match record.status {
                    WriteSpoolRecordStatus::Pending if failed => {
                        record.status = WriteSpoolRecordStatus::Failed;
                    }
                    WriteSpoolRecordStatus::Pending => {
                        record.status = WriteSpoolRecordStatus::Committed;
                        committed_count = committed_count.saturating_add(1);
                    }
                    WriteSpoolRecordStatus::Cancelled => {
                        cancelled_count = cancelled_count.saturating_add(1);
                    }
                    WriteSpoolRecordStatus::Committed | WriteSpoolRecordStatus::Failed => {}
                }
            }

            let outcome = srr3_outcome(failed, committed_count, cancelled_count, selected.len());
            previous_audit_hash = srr3_audit_chain_hash(
                &previous_audit_hash,
                batch_id,
                outcome,
                &selected,
                &audit_subjects,
            );
            audit_chain_hashes.push(previous_audit_hash.clone());

            if committed_count > 0 {
                snapshot_generation = snapshot_generation.saturating_add(1);
                published_snapshots.push(snapshot_generation);
            }
        }

        Srr3ModeledResult {
            durable_rows: srr3_durable_rows_from_reference(&records),
            audit_chain_hashes,
            published_snapshots,
        }
    }

    fn interpret_srr3_write_spool(schedule: &Srr3PropertySchedule) -> Srr3ModeledResult {
        let mut spool = WriteSpool::new(
            WriteSpoolConfig::new(
                schedule.writes.len().max(1),
                schedule.max_batch_size,
                usize::MAX / 4,
                30_000,
            ),
            0,
        );
        let mut producer_sequences = BTreeMap::<u8, u16>::new();
        let mut metadata = BTreeMap::<u64, Srr3WriteMetadata>::new();

        for (arrival_index, write) in schedule.writes.iter().enumerate() {
            let sequence = producer_sequences.entry(write.producer_id).or_default();
            let producer_sequence = *sequence;
            *sequence = sequence.saturating_add(1);

            if write.cancellation_point == Srr3CancellationPoint::BeforeEnqueue {
                continue;
            }

            let ticket = spool
                .enqueue(
                    srr3_intent(
                        write.kind,
                        write.producer_id,
                        producer_sequence,
                        write.payload_bytes,
                    ),
                    u64::try_from(arrival_index).unwrap_or(u64::MAX),
                )
                .expect("generated SRR3 schedule should fit configured spool budgets");
            metadata.insert(
                ticket.request_id,
                Srr3WriteMetadata {
                    producer_id: write.producer_id,
                    producer_sequence,
                    kind: write.kind,
                    payload_bytes: write.payload_bytes,
                    cancellation_point: write.cancellation_point,
                },
            );

            if write.cancellation_point == Srr3CancellationPoint::AfterEnqueueBeforeCommit {
                assert!(spool.cancel_pending(
                    ticket.request_id,
                    u64::try_from(arrival_index.saturating_add(1_000)).unwrap_or(u64::MAX),
                ));
            }
        }

        let mut audit_chain_hashes = Vec::<String>::new();
        let mut previous_audit_hash = String::new();
        let mut published_snapshots = Vec::<u64>::new();
        let mut snapshot_generation = 0_u64;

        while let Some(batch) = spool.next_batch() {
            for request_id in &batch.request_ids {
                if metadata.get(request_id).is_some_and(|metadata| {
                    metadata.cancellation_point == Srr3CancellationPoint::DuringBatchAssembly
                }) {
                    assert!(spool.cancel_pending(*request_id, 20_000 + batch.batch_id));
                }
            }

            let failed = schedule.fsync_failure_batches.contains(&batch.batch_id)
                && batch.request_ids.iter().any(|request_id| {
                    spool
                        .record(*request_id)
                        .is_some_and(|record| record.status == WriteSpoolRecordStatus::Pending)
                });
            let committed_count = if failed {
                let _failed_count =
                    spool.mark_batch_failed(batch.batch_id, 30_000 + batch.batch_id, "fsync");
                0
            } else {
                spool.mark_batch_committed(batch.batch_id, 30_000 + batch.batch_id)
            };
            let cancelled_count = batch
                .request_ids
                .iter()
                .filter(|request_id| {
                    spool
                        .record(**request_id)
                        .is_some_and(|record| record.status == WriteSpoolRecordStatus::Cancelled)
                })
                .count();

            for request_id in &batch.request_ids {
                if metadata.get(request_id).is_some_and(|metadata| {
                    metadata.cancellation_point == Srr3CancellationPoint::AfterCommit
                }) {
                    let _ = spool.cancel_pending(*request_id, 40_000 + batch.batch_id);
                }
            }

            let outcome = srr3_outcome(
                failed,
                committed_count,
                cancelled_count,
                batch.request_ids.len(),
            );
            previous_audit_hash = srr3_audit_chain_hash(
                &previous_audit_hash,
                batch.batch_id,
                outcome,
                &batch.request_ids,
                &batch.audit_subjects,
            );
            audit_chain_hashes.push(previous_audit_hash.clone());

            if committed_count > 0 {
                snapshot_generation = snapshot_generation.saturating_add(1);
                published_snapshots.push(snapshot_generation);
            }
        }

        Srr3ModeledResult {
            durable_rows: srr3_durable_rows_from_spool(&spool.recovery_records(), &metadata),
            audit_chain_hashes,
            published_snapshots,
        }
    }

    fn srr3_failure_context(
        schedule: &Srr3PropertySchedule,
        expected: &Srr3ModeledResult,
        actual: &Srr3ModeledResult,
    ) -> String {
        format!(
            "schedule writes={} max_batch_size={} fsync_failures={:?}\nexpected={:#?}\nactual={:#?}",
            schedule.writes.len(),
            schedule.max_batch_size,
            schedule.fsync_failure_batches,
            expected,
            actual
        )
    }

    const SRR3_DUPLICATE_SEQUENCE_FAILURE: &str = "duplicate_producer_sequence";
    const SRR3_AUDIT_CHAIN_DISCONTINUITY_FAILURE: &str = "audit_chain_discontinuity";
    const SRR3_DURABLE_ROWS_MISMATCH_FAILURE: &str = "durable_rows_mismatch";

    fn srr3_cancellation_point_as_str(point: Srr3CancellationPoint) -> &'static str {
        match point {
            Srr3CancellationPoint::None => "none",
            Srr3CancellationPoint::BeforeEnqueue => "before_enqueue",
            Srr3CancellationPoint::AfterEnqueueBeforeCommit => "after_enqueue_before_commit",
            Srr3CancellationPoint::DuringBatchAssembly => "during_batch_assembly",
            Srr3CancellationPoint::AfterCommit => "after_commit",
        }
    }

    fn srr3_schedule_hash(schedule: &Srr3PropertySchedule) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(
            &u64::try_from(schedule.max_batch_size)
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        for write in &schedule.writes {
            hasher.update(&[write.producer_id]);
            hasher.update(write.kind.as_str().as_bytes());
            hasher.update(
                &u64::try_from(write.payload_bytes)
                    .unwrap_or(u64::MAX)
                    .to_be_bytes(),
            );
            hasher.update(srr3_cancellation_point_as_str(write.cancellation_point).as_bytes());
        }
        for batch_id in &schedule.fsync_failure_batches {
            hasher.update(&batch_id.to_be_bytes());
        }
        hasher.finalize().to_hex().to_string()
    }

    fn srr3_audit_chain_digest(result: &Srr3ModeledResult) -> String {
        let mut hasher = blake3::Hasher::new();
        for chain_hash in &result.audit_chain_hashes {
            hasher.update(chain_hash.as_bytes());
            hasher.update(b"\0");
        }
        hasher.finalize().to_hex().to_string()
    }

    fn srr3_accepted_count(schedule: &Srr3PropertySchedule) -> usize {
        schedule
            .writes
            .iter()
            .filter(|write| write.cancellation_point != Srr3CancellationPoint::BeforeEnqueue)
            .count()
    }

    fn srr3_rejected_count(schedule: &Srr3PropertySchedule, result: &Srr3ModeledResult) -> usize {
        schedule
            .writes
            .len()
            .saturating_sub(result.durable_rows.len())
    }

    fn srr3_cancelled_before_commit_sequences(
        schedule: &Srr3PropertySchedule,
    ) -> BTreeSet<(u8, u16)> {
        let mut producer_sequences = BTreeMap::<u8, u16>::new();
        let mut cancelled = BTreeSet::new();
        for write in &schedule.writes {
            let sequence = producer_sequences.entry(write.producer_id).or_default();
            let producer_sequence = *sequence;
            *sequence = sequence.saturating_add(1);
            if matches!(
                write.cancellation_point,
                Srr3CancellationPoint::AfterEnqueueBeforeCommit
                    | Srr3CancellationPoint::DuringBatchAssembly
            ) {
                cancelled.insert((write.producer_id, producer_sequence));
            }
        }
        cancelled
    }

    fn srr3_first_failure(
        schedule: &Srr3PropertySchedule,
        expected: &Srr3ModeledResult,
        observed: &Srr3ModeledResult,
    ) -> Option<&'static str> {
        let mut durable_sequences = BTreeSet::new();
        for row in &observed.durable_rows {
            if !durable_sequences.insert((row.producer_id, row.producer_sequence)) {
                return Some(SRR3_DUPLICATE_SEQUENCE_FAILURE);
            }
        }

        let cancelled = srr3_cancelled_before_commit_sequences(schedule);
        if observed
            .durable_rows
            .iter()
            .any(|row| cancelled.contains(&(row.producer_id, row.producer_sequence)))
        {
            return Some(WRITE_HOT_PATH_CANCELLED_BEFORE_COMMIT_CODE);
        }

        if !schedule.fsync_failure_batches.is_empty()
            && observed.published_snapshots != expected.published_snapshots
        {
            return Some(WRITE_HOT_PATH_FSYNC_FAILURE_CODE);
        }

        if observed.audit_chain_hashes != expected.audit_chain_hashes {
            return Some(SRR3_AUDIT_CHAIN_DISCONTINUITY_FAILURE);
        }

        if observed.durable_rows != expected.durable_rows {
            if !schedule.fsync_failure_batches.is_empty() {
                return Some(WRITE_HOT_PATH_FSYNC_FAILURE_CODE);
            }
            return Some(SRR3_DURABLE_ROWS_MISMATCH_FAILURE);
        }

        None
    }

    fn srr3_fake_runner_event_line(
        schedule: &Srr3PropertySchedule,
        observed: &Srr3ModeledResult,
    ) -> String {
        let expected = interpret_srr3_reference(schedule);
        let first_failure = srr3_first_failure(schedule, &expected, observed).unwrap_or("none");
        let event = serde_json::json!({
            "schema": "ee.test_event.v1",
            "ts": "1970-01-01T00:00:00Z",
            "test_id": format!("srr3_fake_runner:blake3:{}", srr3_schedule_hash(schedule)),
            "kind": "note",
            "fields": {
                "scheduleHash": format!("blake3:{}", srr3_schedule_hash(schedule)),
                "acceptedCount": srr3_accepted_count(schedule),
                "rejectedCount": srr3_rejected_count(schedule, observed),
                "batchCount": observed.audit_chain_hashes.len(),
                "firstFailure": first_failure,
                "auditChainDigest": format!("blake3:{}", srr3_audit_chain_digest(observed)),
            }
        });
        serde_json::to_string(&event).expect("SRR3 fake-runner event should serialize")
    }

    fn srr3_fake_runner_first_failure(event_line: &str) -> Result<String, String> {
        let value: serde_json::Value =
            serde_json::from_str(event_line).map_err(|error| error.to_string())?;
        value
            .pointer("/fields/firstFailure")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "missing firstFailure field".to_string())
    }

    fn srr3_fake_runner_cancellation_schedule() -> Srr3PropertySchedule {
        Srr3PropertySchedule {
            max_batch_size: 4,
            writes: vec![
                Srr3ScheduledWrite {
                    producer_id: 0,
                    kind: WriteSpoolIntentKind::Remember,
                    payload_bytes: 64,
                    cancellation_point: Srr3CancellationPoint::AfterEnqueueBeforeCommit,
                },
                Srr3ScheduledWrite {
                    producer_id: 0,
                    kind: WriteSpoolIntentKind::Remember,
                    payload_bytes: 64,
                    cancellation_point: Srr3CancellationPoint::None,
                },
            ],
            fsync_failure_batches: BTreeSet::new(),
        }
    }

    fn srr3_fake_runner_fsync_schedule() -> Srr3PropertySchedule {
        Srr3PropertySchedule {
            max_batch_size: 1,
            writes: vec![
                Srr3ScheduledWrite {
                    producer_id: 0,
                    kind: WriteSpoolIntentKind::Remember,
                    payload_bytes: 64,
                    cancellation_point: Srr3CancellationPoint::None,
                },
                Srr3ScheduledWrite {
                    producer_id: 1,
                    kind: WriteSpoolIntentKind::Remember,
                    payload_bytes: 64,
                    cancellation_point: Srr3CancellationPoint::None,
                },
            ],
            fsync_failure_batches: BTreeSet::from([2]),
        }
    }

    fn next_snapshot_generation(current: u64, batch_committed: bool) -> u64 {
        if batch_committed {
            current.saturating_add(1)
        } else {
            current
        }
    }

    fn assert_write_spool_schedule_invariants(
        schedule: &[ScheduledSpoolWrite],
    ) -> Result<(), TestCaseError> {
        let mut spool = WriteSpool::new(WriteSpoolConfig::new(256, 8, 1_000_000, 30_000), 0);
        let mut producer_sequences = BTreeMap::<u8, u16>::new();
        let mut producer_request_ids = BTreeMap::<u8, Vec<u64>>::new();
        let mut cancelled_request_ids = BTreeSet::<u64>::new();

        for (arrival_index, write) in schedule.iter().enumerate() {
            let sequence = producer_sequences.entry(write.producer_id).or_default();
            let idempotency_key = format!("p{}-s{sequence}", write.producer_id);
            *sequence = sequence.saturating_add(1);

            let ticket = spool
                .enqueue(
                    WriteSpoolIntent::new(
                        write.kind,
                        "workspace",
                        idempotency_key,
                        write.payload_bytes,
                    ),
                    u64::try_from(arrival_index)
                        .map_err(|error| TestCaseError::fail(error.to_string()))?,
                )
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            producer_request_ids
                .entry(write.producer_id)
                .or_default()
                .push(ticket.request_id);

            if write.cancel_before_drain {
                let cancelled = spool.cancel_pending(
                    ticket.request_id,
                    u64::try_from(arrival_index.saturating_add(1_000))
                        .map_err(|error| TestCaseError::fail(error.to_string()))?,
                );
                prop_assert!(cancelled, "scheduled cancellation should succeed");
                cancelled_request_ids.insert(ticket.request_id);
            }
        }

        let mut committed_request_ids = BTreeSet::<u64>::new();
        let mut failed_request_ids = BTreeSet::<u64>::new();
        let mut batch_ids = BTreeSet::<u64>::new();
        let mut snapshot_generations = Vec::<u64>::new();
        let mut snapshot_generation = 0_u64;

        while let Some(batch) = spool.next_batch() {
            let mut sorted_request_ids = batch.request_ids.clone();
            sorted_request_ids.sort_unstable();
            prop_assert_eq!(
                &batch.request_ids,
                &sorted_request_ids,
                "batch request IDs must stay in deterministic FIFO order"
            );
            let expected_audit_row_id = format!("audit_batch_{:016}", batch.batch_id);
            prop_assert_eq!(batch.audit_row_id.as_str(), expected_audit_row_id);
            let expected_job_row_id = format!("job_batch_{:016}", batch.batch_id);
            prop_assert_eq!(batch.job_row_id.as_str(), expected_job_row_id);
            prop_assert!(batch_ids.insert(batch.batch_id));

            for request_id in &batch.request_ids {
                prop_assert!(
                    !cancelled_request_ids.contains(request_id),
                    "cancelled request must not appear in a durable batch"
                );
                let record = spool
                    .record(*request_id)
                    .ok_or_else(|| TestCaseError::fail(format!("missing record {request_id}")))?;
                prop_assert_eq!(record.batch_id, Some(batch.batch_id));
                prop_assert_eq!(record.workspace_id.as_str(), batch.workspace_id.as_str());
                prop_assert_eq!(record.kind, batch.kind);
                prop_assert_eq!(record.durability, batch.durability);
            }

            if batch.batch_id % 7 == 0 {
                let failed =
                    spool.mark_batch_failed(batch.batch_id, 10_000 + batch.batch_id, "fsync");
                prop_assert_eq!(failed, batch.request_ids.len());
                failed_request_ids.extend(batch.request_ids.iter().copied());
                snapshot_generation = next_snapshot_generation(snapshot_generation, false);
            } else {
                let committed = spool.mark_batch_committed(batch.batch_id, 10_000 + batch.batch_id);
                prop_assert_eq!(committed, batch.request_ids.len());
                committed_request_ids.extend(batch.request_ids.iter().copied());
                snapshot_generation = next_snapshot_generation(snapshot_generation, true);
                snapshot_generations.push(snapshot_generation);
            }
        }

        let records = spool.recovery_records();
        prop_assert_eq!(records.len(), schedule.len());
        for (index, record) in records.iter().enumerate() {
            prop_assert_eq!(
                record.request_id,
                u64::try_from(index.saturating_add(1))
                    .map_err(|error| TestCaseError::fail(error.to_string()))?
            );
        }

        for request_ids in producer_request_ids.values() {
            for adjacent in request_ids.windows(2) {
                prop_assert!(
                    adjacent[0] < adjacent[1],
                    "producer request IDs must preserve per-producer FIFO order"
                );
            }
        }

        for record in &records {
            match record.status {
                WriteSpoolRecordStatus::Committed => {
                    prop_assert!(committed_request_ids.contains(&record.request_id));
                    prop_assert!(record.batch_id.is_some());
                }
                WriteSpoolRecordStatus::Failed => {
                    prop_assert!(failed_request_ids.contains(&record.request_id));
                    prop_assert!(record.batch_id.is_some());
                    prop_assert_eq!(record.failure.as_deref(), Some("fsync"));
                }
                WriteSpoolRecordStatus::Cancelled => {
                    prop_assert!(cancelled_request_ids.contains(&record.request_id));
                    prop_assert_eq!(record.batch_id, None);
                }
                WriteSpoolRecordStatus::Pending => {
                    prop_assert!(false, "all non-cancelled records should be drained");
                }
            }
        }

        for expected_batch_id in 1..=u64::try_from(batch_ids.len())
            .map_err(|error| TestCaseError::fail(error.to_string()))?
        {
            prop_assert!(
                batch_ids.contains(&expected_batch_id),
                "batch audit chain must not have holes"
            );
        }
        for adjacent in snapshot_generations.windows(2) {
            prop_assert!(
                adjacent[0] < adjacent[1],
                "snapshot generations must be monotone after committed batches"
            );
        }
        prop_assert_eq!(spool.status(20_000).queue_depth, 0);
        Ok(())
    }

    #[test]
    fn write_operation_type_strings() {
        let op = WriteOperation::MemoryCreate {
            workspace_id: "ws".into(),
            content: "test".into(),
            level: "semantic".into(),
            kind: "note".into(),
            tags: vec![],
        };
        assert_eq!(op.operation_type(), "memory_create");

        let op = WriteOperation::LinkCreate {
            workspace_id: "ws".into(),
            source_id: "src".into(),
            target_id: "tgt".into(),
            relation: "supports".into(),
        };
        assert_eq!(op.operation_type(), "link_create");

        let op = WriteOperation::OutcomeRecord {
            workspace_id: "ws".into(),
            memory_id: "mem".into(),
            outcome_type: "positive".into(),
            details: None,
        };
        assert_eq!(op.operation_type(), "outcome_record");

        let op = WriteOperation::Custom {
            operation_type: "test".into(),
            payload: serde_json::json!({}),
        };
        assert_eq!(op.operation_type(), "custom");
    }

    #[test]
    fn write_result_accessors() {
        let success = WriteResult::Success {
            entity_id: Some("id-123".into()),
        };
        assert!(success.is_success());
        assert_eq!(success.entity_id(), Some("id-123"));

        let failed = WriteResult::Failed {
            error: DomainError::Storage {
                message: "test error".to_string(),
                repair: None,
            },
        };
        assert!(!failed.is_success());
        assert_eq!(failed.entity_id(), None);

        let shutdown = WriteResult::Shutdown;
        assert!(!shutdown.is_success());
        assert_eq!(shutdown.entity_id(), None);
    }

    #[test]
    fn write_owner_busy_error_format() {
        let err = WriteOwnerBusyError::new(5);
        assert_eq!(err.code, WRITE_OWNER_BUSY_CODE);
        assert!(err.message.contains("5 pending"));
        assert_eq!(err.repair, "ee diag locks --json");
    }

    #[test]
    fn write_owner_status_default() {
        let status = WriteOwnerStatus::default();
        assert!(!status.running);
        assert_eq!(status.queue_depth, 0);
        assert_eq!(status.total_processed, 0);
        assert_eq!(status.avg_wait_ms, 0.0);
        assert_eq!(status.max_wait_ms, 0);
    }

    #[test]
    fn write_owner_status_reports_enqueued_requests() -> Result<(), String> {
        let (owner, handle) = WriteOwner::new(4);
        assert_eq!(owner.status().queue_depth, 0);

        let _first_response = handle
            .try_submit(WriteOperation::Custom {
                operation_type: "first".to_string(),
                payload: serde_json::json!({}),
            })
            .ok_or_else(|| "first write request should enqueue".to_string())?;
        assert_eq!(owner.status().queue_depth, 1);

        let _second_response = handle
            .try_submit(WriteOperation::Custom {
                operation_type: "second".to_string(),
                payload: serde_json::json!({}),
            })
            .ok_or_else(|| "second write request should enqueue".to_string())?;
        assert_eq!(owner.status().queue_depth, 2);

        Ok(())
    }

    #[test]
    fn write_spool_deduplicates_idempotency_keys() -> Result<(), String> {
        let mut spool = WriteSpool::new(WriteSpoolConfig::default(), 0);
        let first = spool
            .enqueue(
                WriteSpoolIntent::new(WriteSpoolIntentKind::Remember, "workspace", "idem-1", 128),
                10,
            )
            .map_err(|error| error.to_string())?;
        let duplicate = spool
            .enqueue(
                WriteSpoolIntent::new(WriteSpoolIntentKind::Remember, "workspace", "idem-1", 128),
                11,
            )
            .map_err(|error| error.to_string())?;

        assert_eq!(first.request_id, duplicate.request_id);
        assert!(!first.duplicate);
        assert!(duplicate.duplicate);
        assert_eq!(spool.status(11).queue_depth, 1);
        assert_eq!(spool.recovery_records().len(), 1);
        Ok(())
    }

    #[test]
    fn write_spool_recovery_state_marks_replay_required_and_clean() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;

        mark_write_replay_required(temp.path()).map_err(|error| error.to_string())?;
        assert!(
            workspace_write_replay_required(temp.path()),
            "replay marker should report required"
        );

        mark_write_replay_clean(temp.path()).map_err(|error| error.to_string())?;
        assert!(
            !workspace_write_replay_required(temp.path()),
            "clean marker should clear replay requirement"
        );

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn write_spool_recovery_state_rejects_symlinked_spool_parent() -> Result<(), String> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).map_err(|error| error.to_string())?;
        let ee_dir = temp.path().join(".ee");
        fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
        symlink(&outside, ee_dir.join("write-spool")).map_err(|error| error.to_string())?;

        let error = mark_write_replay_required(temp.path())
            .expect_err("symlinked write-spool parent must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(
            !outside.join("recovery-state.json").exists(),
            "recovery marker must not be written through symlinked write-spool parent"
        );

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn workspace_write_replay_required_ignores_symlinked_marker_file() -> Result<(), String> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let spool_dir = temp.path().join(".ee").join("write-spool");
        fs::create_dir_all(&spool_dir).map_err(|error| error.to_string())?;
        let outside_marker = temp.path().join("outside-recovery-state.json");
        fs::write(
            &outside_marker,
            format!(
                "{{\"schema\":\"{WRITE_SPOOL_RECOVERY_STATE_SCHEMA_V1}\",\"state\":\"{WRITE_SPOOL_RECOVERY_STATE_REPLAY_REQUIRED}\"}}\n"
            ),
        )
        .map_err(|error| error.to_string())?;
        symlink(
            &outside_marker,
            write_spool_recovery_state_path(temp.path()),
        )
        .map_err(|error| error.to_string())?;

        assert!(
            !workspace_write_replay_required(temp.path()),
            "status must not trust a symlinked recovery marker file"
        );

        Ok(())
    }

    #[test]
    fn workspace_write_replay_required_ignores_marker_directory() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        fs::create_dir_all(write_spool_recovery_state_path(temp.path()))
            .map_err(|error| error.to_string())?;

        assert!(
            !workspace_write_replay_required(temp.path()),
            "status must not trust a non-regular recovery marker path"
        );

        Ok(())
    }

    #[test]
    fn write_spool_recovery_state_rejects_non_regular_marker_before_temp_write()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let marker_path = write_spool_recovery_state_path(temp.path());
        fs::create_dir_all(&marker_path).map_err(|error| error.to_string())?;
        let mut temp_path = marker_path.clone();
        temp_path.set_extension("tmp");

        let error = mark_write_replay_required(temp.path())
            .expect_err("non-regular recovery marker path should be rejected");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(
            error.to_string().contains("not a file"),
            "error should explain the non-file marker path"
        );
        assert!(
            fs::symlink_metadata(&marker_path)
                .map_err(|error| error.to_string())?
                .file_type()
                .is_dir(),
            "recovery marker path must remain a directory"
        );
        assert!(
            !temp_path.exists(),
            "temp recovery marker must not be written after final path preflight fails"
        );

        Ok(())
    }

    #[test]
    fn write_spool_recovery_state_rejects_existing_temp_without_truncating() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let marker_path = write_spool_recovery_state_path(temp.path());
        let parent = marker_path
            .parent()
            .ok_or_else(|| "marker parent missing".to_owned())?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let mut temp_path = marker_path.clone();
        temp_path.set_extension("tmp");
        fs::write(&temp_path, "keep me").map_err(|error| error.to_string())?;

        let error =
            mark_write_replay_required(temp.path()).expect_err("existing temp should be rejected");

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(
            error.to_string().contains("already exists"),
            "error should explain the stale temp path"
        );
        assert_eq!(
            fs::read_to_string(&temp_path).map_err(|error| error.to_string())?,
            "keep me",
            "recovery temp path must not be truncated"
        );
        assert!(
            !marker_path.exists(),
            "final recovery marker must not be written after temp collision"
        );

        Ok(())
    }

    #[test]
    fn write_spool_recovery_state_rejects_non_regular_temp_before_write() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let marker_path = write_spool_recovery_state_path(temp.path());
        let parent = marker_path
            .parent()
            .ok_or_else(|| "marker parent missing".to_owned())?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let mut temp_path = marker_path.clone();
        temp_path.set_extension("tmp");
        fs::create_dir_all(&temp_path).map_err(|error| error.to_string())?;

        let error = mark_write_replay_required(temp.path())
            .expect_err("non-regular temp marker path should be rejected");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(
            error.to_string().contains("temp path is not a file"),
            "error should explain the non-file temp path"
        );
        assert!(
            fs::symlink_metadata(&temp_path)
                .map_err(|error| error.to_string())?
                .file_type()
                .is_dir(),
            "recovery temp path must remain a directory"
        );
        assert!(
            !marker_path.exists(),
            "final recovery marker must not be written after temp path preflight fails"
        );

        Ok(())
    }

    #[test]
    fn write_spool_batches_eligible_writes_and_isolates_immediate_imports() -> Result<(), String> {
        let mut spool = WriteSpool::new(WriteSpoolConfig::new(8, 4, 4096, 30_000), 0);
        for index in 0..3 {
            spool
                .enqueue(
                    WriteSpoolIntent::new(
                        WriteSpoolIntentKind::Remember,
                        "workspace",
                        format!("remember-{index}"),
                        100,
                    ),
                    index,
                )
                .map_err(|error| error.to_string())?;
        }
        let import = spool
            .enqueue(
                WriteSpoolIntent::new(WriteSpoolIntentKind::Import, "workspace", "import-0", 100),
                4,
            )
            .map_err(|error| error.to_string())?;

        let remember_batch = spool
            .next_batch()
            .ok_or_else(|| "expected remember batch".to_string())?;
        assert_eq!(remember_batch.kind, WriteSpoolIntentKind::Remember);
        assert_eq!(remember_batch.durability, WriteSpoolDurability::Batched);
        assert_eq!(remember_batch.row_count(), 3);
        assert_eq!(remember_batch.audit_row_id, "audit_batch_0000000000000001");
        assert_eq!(remember_batch.job_row_id, "job_batch_0000000000000001");

        let import_batch = spool
            .next_batch()
            .ok_or_else(|| "expected immediate import batch".to_string())?;
        assert_eq!(import_batch.request_ids, vec![import.request_id]);
        assert_eq!(import_batch.kind, WriteSpoolIntentKind::Import);
        assert_eq!(import_batch.durability, WriteSpoolDurability::Immediate);
        assert_eq!(import_batch.row_count(), 1);
        assert_eq!(spool.status(5).queue_depth, 0);
        Ok(())
    }

    #[test]
    fn write_spool_backpressure_reports_json_contract() -> Result<(), String> {
        let mut spool = WriteSpool::new(WriteSpoolConfig::new(1, 4, 4096, 30_000), 0);
        spool
            .enqueue(
                WriteSpoolIntent::new(WriteSpoolIntentKind::Outcome, "workspace", "outcome-0", 10),
                0,
            )
            .map_err(|error| error.to_string())?;

        let err = spool
            .enqueue(
                WriteSpoolIntent::new(WriteSpoolIntentKind::Outcome, "workspace", "outcome-1", 10),
                1,
            )
            .expect_err("second write should hit depth backpressure");
        assert_eq!(err.schema, WRITE_SPOOL_BACKPRESSURE_SCHEMA_V1);
        assert_eq!(err.code, WRITE_SPOOL_BACKPRESSURE_CODE);
        assert_eq!(err.reason, WriteSpoolBackpressureReason::QueueDepth);
        assert_eq!(err.queue_depth, 1);
        assert_eq!(err.repair, "ee daemon status --json");
        assert_eq!(
            err.next,
            "ee support-bundle create --include write-queue --json"
        );

        let json = serde_json::to_value(&err).map_err(|error| error.to_string())?;
        assert_eq!(json["reason"], "queue_depth");
        assert_eq!(json["oldestQueuedAgeMs"], 1);
        Ok(())
    }

    #[test]
    fn write_spool_recovery_distinguishes_pending_committed_cancelled_failed() -> Result<(), String>
    {
        let mut spool = WriteSpool::new(WriteSpoolConfig::new(8, 2, 4096, 30_000), 0);
        let pending = spool
            .enqueue(
                WriteSpoolIntent::new(WriteSpoolIntentKind::Recorder, "workspace", "pending", 10),
                0,
            )
            .map_err(|error| error.to_string())?;
        let committed = spool
            .enqueue(
                WriteSpoolIntent::new(WriteSpoolIntentKind::Remember, "workspace", "committed", 10),
                1,
            )
            .map_err(|error| error.to_string())?;
        let cancelled = spool
            .enqueue(
                WriteSpoolIntent::new(WriteSpoolIntentKind::Outcome, "workspace", "cancelled", 10),
                2,
            )
            .map_err(|error| error.to_string())?;
        let failed = spool
            .enqueue(
                WriteSpoolIntent::new(WriteSpoolIntentKind::Import, "workspace", "failed", 10),
                3,
            )
            .map_err(|error| error.to_string())?;

        assert!(spool.cancel_pending(cancelled.request_id, 4));

        let first_batch = spool
            .next_batch()
            .ok_or_else(|| "expected first batch".to_string())?;
        assert_eq!(first_batch.request_ids, vec![pending.request_id]);

        let committed_batch = spool
            .next_batch()
            .ok_or_else(|| "expected committed batch".to_string())?;
        assert_eq!(committed_batch.request_ids, vec![committed.request_id]);
        assert_eq!(spool.mark_batch_committed(committed_batch.batch_id, 5), 1);

        let failed_batch = spool
            .next_batch()
            .ok_or_else(|| "expected failed batch".to_string())?;
        assert_eq!(failed_batch.request_ids, vec![failed.request_id]);
        assert_eq!(
            spool.mark_batch_failed(failed_batch.batch_id, 6, "disk full"),
            1
        );

        let recovered = WriteSpool::from_recovery_records(
            WriteSpoolConfig::new(8, 2, 4096, 30_000),
            0,
            spool.recovery_records(),
        );
        assert_eq!(
            recovered.record(pending.request_id).map(|r| r.status),
            Some(WriteSpoolRecordStatus::Pending)
        );
        assert_eq!(
            recovered.record(committed.request_id).map(|r| r.status),
            Some(WriteSpoolRecordStatus::Committed)
        );
        assert_eq!(
            recovered.record(cancelled.request_id).map(|r| r.status),
            Some(WriteSpoolRecordStatus::Cancelled)
        );
        assert_eq!(
            recovered.record(failed.request_id).map(|r| r.status),
            Some(WriteSpoolRecordStatus::Failed)
        );
        assert_eq!(recovered.status(7).queue_depth, 1);
        Ok(())
    }

    #[test]
    fn write_spool_status_reports_metrics_for_support_bundle() -> Result<(), String> {
        let mut spool = WriteSpool::new(WriteSpoolConfig::new(8, 8, 4096, 30_000), 1_000);
        for index in 0..4 {
            spool
                .enqueue(
                    WriteSpoolIntent::new(
                        WriteSpoolIntentKind::Remember,
                        "workspace",
                        format!("metric-{index}"),
                        25,
                    ),
                    1_000 + index,
                )
                .map_err(|error| error.to_string())?;
        }
        let batch = spool
            .next_batch()
            .ok_or_else(|| "expected metrics batch".to_string())?;
        assert_eq!(spool.mark_batch_committed(batch.batch_id, 2_000), 4);

        let status = spool.status(3_000);
        assert_eq!(status.schema, WRITE_SPOOL_STATUS_SCHEMA_V1);
        assert_eq!(status.queue_depth, 0);
        assert_eq!(status.total_enqueued, 4);
        assert_eq!(status.total_committed, 4);
        assert_eq!(status.total_batches, 1);
        assert_eq!(status.last_batch_size, 4);
        assert_eq!(status.max_batch_size_observed, 4);
        assert_eq!(status.rows_per_sec, 2.0);
        assert_eq!(status.last_failure, None);
        Ok(())
    }

    #[test]
    fn write_spool_lab_runtime_cancellation_is_recoverable() -> Result<(), String> {
        let runtime = asupersync::LabRuntime::new(asupersync::LabConfig::new(42));
        let now_ms = runtime.now().as_nanos() / 1_000_000;
        let mut spool = WriteSpool::new(WriteSpoolConfig::new(4, 2, 1024, 10_000), now_ms);
        let ticket = spool
            .enqueue(
                WriteSpoolIntent::new(WriteSpoolIntentKind::Remember, "workspace", "cancel", 32),
                now_ms,
            )
            .map_err(|error| error.to_string())?;
        assert!(spool.cancel_pending(ticket.request_id, now_ms + 1));

        let recovered = WriteSpool::from_recovery_records(
            WriteSpoolConfig::new(4, 2, 1024, 10_000),
            now_ms,
            spool.recovery_records(),
        );
        assert_eq!(
            recovered.record(ticket.request_id).map(|r| r.status),
            Some(WriteSpoolRecordStatus::Cancelled)
        );
        assert_eq!(recovered.status(now_ms + 2).total_cancelled, 1);
        Ok(())
    }

    #[test]
    fn write_spool_lab_runtime_queue_timeout_backpressure() -> Result<(), String> {
        let mut runtime = asupersync::LabRuntime::new(asupersync::LabConfig::new(43));
        let mut spool = WriteSpool::new(WriteSpoolConfig::new(4, 2, 1024, 5), 0);
        spool
            .enqueue(
                WriteSpoolIntent::new(WriteSpoolIntentKind::Recorder, "workspace", "stale", 32),
                0,
            )
            .map_err(|error| error.to_string())?;

        runtime.advance_time(6_000_000);
        let now_ms = runtime.now().as_nanos() / 1_000_000;
        let err = spool
            .enqueue(
                WriteSpoolIntent::new(WriteSpoolIntentKind::Recorder, "workspace", "blocked", 32),
                now_ms,
            )
            .expect_err("stale queue should apply timeout backpressure");
        assert_eq!(err.reason, WriteSpoolBackpressureReason::QueueTimeout);
        assert_eq!(err.oldest_queued_age_ms, Some(6));
        Ok(())
    }

    #[test]
    fn write_spool_lab_runtime_pending_bytes_backpressure() -> Result<(), String> {
        let runtime = asupersync::LabRuntime::new(asupersync::LabConfig::new(44));
        let now_ms = runtime.now().as_nanos() / 1_000_000;
        let mut spool = WriteSpool::new(WriteSpoolConfig::new(4, 2, 64, 10_000), now_ms);
        spool
            .enqueue(
                WriteSpoolIntent::new(WriteSpoolIntentKind::Remember, "workspace", "fits", 48),
                now_ms,
            )
            .map_err(|error| error.to_string())?;

        let err = spool
            .enqueue(
                WriteSpoolIntent::new(WriteSpoolIntentKind::Remember, "workspace", "too-big", 32),
                now_ms,
            )
            .expect_err("payload budget should apply bytes backpressure");
        assert_eq!(err.reason, WriteSpoolBackpressureReason::PendingBytes);
        assert_eq!(err.pending_bytes, 48);
        assert_eq!(err.max_pending_bytes, 64);
        Ok(())
    }

    #[test]
    fn write_spool_invariant_single_writer_happy() -> Result<(), String> {
        let mut spool = WriteSpool::new(WriteSpoolConfig::new(8, 8, 4096, 30_000), 0);
        let mut request_ids = Vec::new();
        for sequence in 0..3 {
            let ticket = spool
                .enqueue(
                    WriteSpoolIntent::new(
                        WriteSpoolIntentKind::Remember,
                        "workspace",
                        format!("writer-0-seq-{sequence}"),
                        64,
                    ),
                    sequence,
                )
                .map_err(|error| error.to_string())?;
            request_ids.push(ticket.request_id);
        }

        let batch = spool
            .next_batch()
            .ok_or_else(|| "expected a single writer batch".to_string())?;
        assert_eq!(batch.request_ids, request_ids);
        assert_eq!(batch.audit_row_id, "audit_batch_0000000000000001");
        assert_eq!(spool.mark_batch_committed(batch.batch_id, 10), 3);
        assert_eq!(spool.status(11).queue_depth, 0);
        Ok(())
    }

    #[test]
    fn write_spool_invariant_fsync_failure_propagation_model() -> Result<(), String> {
        let mut spool = WriteSpool::new(WriteSpoolConfig::new(8, 8, 4096, 30_000), 0);
        let ticket = spool
            .enqueue(
                WriteSpoolIntent::new(WriteSpoolIntentKind::Remember, "workspace", "fsync", 64),
                0,
            )
            .map_err(|error| error.to_string())?;

        let batch = spool
            .next_batch()
            .ok_or_else(|| "expected fsync-failure batch".to_string())?;
        assert_eq!(
            spool.mark_batch_failed(batch.batch_id, 5, "simulated fsync failure"),
            1
        );
        let record = spool
            .record(ticket.request_id)
            .ok_or_else(|| "failed record missing".to_string())?;
        assert_eq!(record.status, WriteSpoolRecordStatus::Failed);
        assert_eq!(record.failure.as_deref(), Some("simulated fsync failure"));
        assert_eq!(spool.status(6).total_failed, 1);
        Ok(())
    }

    #[test]
    fn write_spool_invariant_snapshot_generation_monotone() {
        let mut generation = 0_u64;
        let outcomes = [true, true, false, true, false, true];
        let mut observed = Vec::new();

        for committed in outcomes {
            generation = next_snapshot_generation(generation, committed);
            if committed {
                observed.push(generation);
            }
        }

        assert_eq!(observed, vec![1, 2, 3, 4]);
    }

    #[test]
    fn srr3_fake_runner_emits_sanitized_test_event_line() -> Result<(), String> {
        let schedule = srr3_fake_runner_fsync_schedule();
        let observed = interpret_srr3_write_spool(&schedule);
        let event_line = srr3_fake_runner_event_line(&schedule, &observed);
        let event: serde_json::Value =
            serde_json::from_str(&event_line).map_err(|error| error.to_string())?;

        assert_eq!(event["schema"], "ee.test_event.v1");
        assert_eq!(event["kind"], "note");
        assert_eq!(event["fields"]["firstFailure"], "none");
        assert_eq!(event["fields"]["acceptedCount"], serde_json::json!(2));
        assert_eq!(event["fields"]["rejectedCount"], serde_json::json!(1));
        assert_eq!(event["fields"]["batchCount"], serde_json::json!(2));
        assert!(
            event["fields"]["scheduleHash"]
                .as_str()
                .is_some_and(|value| value.starts_with("blake3:"))
        );
        assert!(
            event["fields"]["auditChainDigest"]
                .as_str()
                .is_some_and(|value| value.starts_with("blake3:"))
        );
        assert!(
            !event_line.contains("workspace") && !event_line.contains("p00"),
            "fake-runner event must expose sanitized hashes/counts, not raw write subjects"
        );
        Ok(())
    }

    #[test]
    fn srr3_fake_runner_rejects_duplicate_sequence_regression() -> Result<(), String> {
        let schedule = srr3_fake_runner_fsync_schedule();
        let mut observed = interpret_srr3_write_spool(&schedule);
        let duplicate = observed
            .durable_rows
            .first()
            .cloned()
            .ok_or_else(|| "expected at least one durable row".to_string())?;
        observed.durable_rows.push(duplicate);

        let event_line = srr3_fake_runner_event_line(&schedule, &observed);
        assert_eq!(
            srr3_fake_runner_first_failure(&event_line)?,
            SRR3_DUPLICATE_SEQUENCE_FAILURE
        );
        Ok(())
    }

    #[test]
    fn srr3_fake_runner_rejects_missing_cancellation_event() -> Result<(), String> {
        let schedule = srr3_fake_runner_cancellation_schedule();
        let mut observed = interpret_srr3_write_spool(&schedule);
        observed.durable_rows.push(Srr3DurableRow {
            request_id: 1,
            producer_id: 0,
            producer_sequence: 0,
            batch_id: 1,
            kind: WriteSpoolIntentKind::Remember,
            payload_bytes: 64,
        });

        let event_line = srr3_fake_runner_event_line(&schedule, &observed);
        assert_eq!(
            srr3_fake_runner_first_failure(&event_line)?,
            WRITE_HOT_PATH_CANCELLED_BEFORE_COMMIT_CODE
        );
        assert!(
            event_line.contains("scheduleHash"),
            "cancellation diagnostic must carry sanitized schedule evidence"
        );
        Ok(())
    }

    #[test]
    fn srr3_fake_runner_rejects_partial_fsync_publication() -> Result<(), String> {
        let schedule = srr3_fake_runner_fsync_schedule();
        let mut observed = interpret_srr3_write_spool(&schedule);
        observed.published_snapshots.push(2);

        let event_line = srr3_fake_runner_event_line(&schedule, &observed);
        assert_eq!(
            srr3_fake_runner_first_failure(&event_line)?,
            WRITE_HOT_PATH_FSYNC_FAILURE_CODE
        );
        assert!(
            event_line.contains("auditChainDigest"),
            "fsync diagnostic must carry sanitized audit-chain evidence"
        );
        Ok(())
    }

    #[test]
    fn srr3_fake_runner_rejects_durable_row_from_failed_fsync_batch() -> Result<(), String> {
        let schedule = srr3_fake_runner_fsync_schedule();
        let mut observed = interpret_srr3_write_spool(&schedule);
        observed.durable_rows.push(Srr3DurableRow {
            request_id: 2,
            producer_id: 1,
            producer_sequence: 0,
            batch_id: 2,
            kind: WriteSpoolIntentKind::Remember,
            payload_bytes: 64,
        });

        let event_line = srr3_fake_runner_event_line(&schedule, &observed);
        assert_eq!(
            srr3_fake_runner_first_failure(&event_line)?,
            WRITE_HOT_PATH_FSYNC_FAILURE_CODE
        );
        assert!(
            event_line.contains("auditChainDigest"),
            "fsync diagnostic must carry sanitized audit-chain evidence"
        );
        Ok(())
    }

    #[test]
    fn srr3_fake_runner_rejects_audit_chain_discontinuity() -> Result<(), String> {
        let schedule = srr3_fake_runner_fsync_schedule();
        let mut observed = interpret_srr3_write_spool(&schedule);
        let first_hash = observed
            .audit_chain_hashes
            .first_mut()
            .ok_or_else(|| "expected at least one audit hash".to_string())?;
        *first_hash = "tampered".to_string();

        let event_line = srr3_fake_runner_event_line(&schedule, &observed);
        assert_eq!(
            srr3_fake_runner_first_failure(&event_line)?,
            SRR3_AUDIT_CHAIN_DISCONTINUITY_FAILURE
        );
        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn write_spool_group_commit_preserves_order_audit_and_snapshot_invariants(
            schedule in prop::collection::vec(scheduled_spool_write_strategy(), 0..64),
        ) {
            assert_write_spool_schedule_invariants(&schedule)?;
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(16))]

        #[test]
        fn srr3_property_generators_match_reference_interpreter(
            schedule in srr3_property_schedule_strategy(),
        ) {
            let expected = interpret_srr3_reference(&schedule);
            let actual = interpret_srr3_write_spool(&schedule);
            prop_assert_eq!(
                &actual,
                &expected,
                "{}",
                srr3_failure_context(&schedule, &expected, &actual)
            );
            prop_assert!(
                actual.published_snapshots.windows(2).all(|window| window[0] < window[1]),
                "published snapshots must be monotone: {}",
                srr3_failure_context(&schedule, &expected, &actual)
            );
        }
    }
}
