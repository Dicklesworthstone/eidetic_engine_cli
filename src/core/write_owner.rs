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

use std::fmt;

use asupersync::channel::{mpsc, oneshot};
use asupersync::cx::Cx;
use serde::Serialize;

use crate::models::DomainError;

/// Schema for write owner status response.
pub const WRITE_OWNER_STATUS_SCHEMA_V1: &str = "ee.write_owner.status.v1";

/// Schema for write owner busy error.
pub const WRITE_OWNER_BUSY_SCHEMA_V1: &str = "ee.write_owner.busy.v1";

/// Default channel capacity for write requests.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 64;

/// Error code for write owner busy condition.
pub const WRITE_OWNER_BUSY_CODE: &str = "write_owner_busy";

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
            queue_depth: 0, // Would need atomic counter for accurate count
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
