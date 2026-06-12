//! Structured stderr JSONL progress events (EE-318).
//!
//! Provides types for emitting structured progress events to stderr in JSONL format.
//! These events enable machine-readable progress tracking without polluting stdout.
//!
//! # Design
//!
//! - Progress events go to stderr only, never stdout
//! - Each event is a single JSONL line with a schema identifier
//! - Events are optional and controlled by output mode
//! - Schema: `ee.progress.v1`

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

fn normalized_progress_event_type_token(input: &str) -> String {
    input.trim().to_ascii_lowercase()
}

/// Schema identifier for progress events.
pub const PROGRESS_EVENT_SCHEMA_V1: &str = "ee.progress.v1";

/// Type of progress event.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressEventType {
    /// Operation has started.
    Started,
    /// Operation is running (can include percentage).
    Running,
    /// Operation completed successfully.
    Completed,
    /// Operation failed.
    Failed,
    /// Warning during operation (non-fatal).
    Warning,
    /// Informational message.
    Info,
    /// Debug-level message (only in verbose mode).
    Debug,
}

impl ProgressEventType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Warning => "warning",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }

    #[must_use]
    pub const fn is_error(self) -> bool {
        matches!(self, Self::Failed)
    }
}

impl fmt::Display for ProgressEventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseProgressEventTypeError {
    pub invalid: String,
}

impl fmt::Display for ParseProgressEventTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid progress event type '{}'; expected one of: started, running, completed, failed, warning, info, debug",
            self.invalid
        )
    }
}

impl std::error::Error for ParseProgressEventTypeError {}

impl FromStr for ProgressEventType {
    type Err = ParseProgressEventTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match normalized_progress_event_type_token(s).as_str() {
            "started" => Ok(Self::Started),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "warning" => Ok(Self::Warning),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            _ => Err(ParseProgressEventTypeError {
                invalid: s.to_owned(),
            }),
        }
    }
}

/// A structured progress event for stderr JSONL output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProgressEvent {
    /// Schema identifier.
    pub schema: String,
    /// Event type.
    pub event_type: ProgressEventType,
    /// Operation being tracked.
    pub operation: String,
    /// Human-readable message.
    pub message: String,
    /// Progress percentage (0.0 to 1.0), if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    /// Current item being processed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_item: Option<String>,
    /// Total items to process.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_items: Option<u64>,
    /// Items processed so far.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processed_items: Option<u64>,
    /// Elapsed time in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    /// Timestamp in RFC 3339 format.
    pub timestamp: String,
}

impl ProgressEvent {
    #[must_use]
    pub fn builder() -> ProgressEventBuilder {
        ProgressEventBuilder::default()
    }

    #[must_use]
    pub fn to_jsonl(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| String::new())
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProgressEventBuilder {
    event_type: Option<ProgressEventType>,
    operation: Option<String>,
    message: Option<String>,
    progress: Option<f64>,
    current_item: Option<String>,
    total_items: Option<u64>,
    processed_items: Option<u64>,
    elapsed_ms: Option<u64>,
    timestamp: Option<String>,
}

impl ProgressEventBuilder {
    #[must_use]
    pub fn event_type(mut self, event_type: ProgressEventType) -> Self {
        self.event_type = Some(event_type);
        self
    }

    #[must_use]
    pub fn operation(mut self, operation: impl Into<String>) -> Self {
        self.operation = Some(operation.into());
        self
    }

    #[must_use]
    pub fn message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    #[must_use]
    pub fn progress(mut self, progress: f64) -> Self {
        self.progress = if progress.is_finite() {
            Some(progress.clamp(0.0, 1.0))
        } else {
            None
        };
        self
    }

    #[must_use]
    pub fn current_item(mut self, item: impl Into<String>) -> Self {
        self.current_item = Some(item.into());
        self
    }

    #[must_use]
    pub fn total_items(mut self, total: u64) -> Self {
        self.total_items = Some(total);
        self
    }

    #[must_use]
    pub fn processed_items(mut self, processed: u64) -> Self {
        self.processed_items = Some(processed);
        self
    }

    #[must_use]
    pub fn elapsed_ms(mut self, elapsed: u64) -> Self {
        self.elapsed_ms = Some(elapsed);
        self
    }

    #[must_use]
    pub fn timestamp(mut self, timestamp: impl Into<String>) -> Self {
        self.timestamp = Some(timestamp.into());
        self
    }

    #[must_use]
    pub fn build(self) -> ProgressEvent {
        ProgressEvent {
            schema: PROGRESS_EVENT_SCHEMA_V1.to_owned(),
            event_type: self.event_type.unwrap_or(ProgressEventType::Info),
            operation: self.operation.unwrap_or_default(),
            message: self.message.unwrap_or_default(),
            progress: self.progress,
            current_item: self.current_item,
            total_items: self.total_items,
            processed_items: self.processed_items,
            elapsed_ms: self.elapsed_ms,
            timestamp: self.timestamp.unwrap_or_default(),
        }
    }
}

/// Quick helper to create a started event.
#[must_use]
pub fn progress_started(operation: &str, message: &str) -> ProgressEvent {
    ProgressEvent::builder()
        .event_type(ProgressEventType::Started)
        .operation(operation)
        .message(message)
        .timestamp(chrono::Utc::now().to_rfc3339())
        .build()
}

/// Quick helper to create a completed event.
#[must_use]
pub fn progress_completed(operation: &str, message: &str, elapsed_ms: u64) -> ProgressEvent {
    ProgressEvent::builder()
        .event_type(ProgressEventType::Completed)
        .operation(operation)
        .message(message)
        .elapsed_ms(elapsed_ms)
        .timestamp(chrono::Utc::now().to_rfc3339())
        .build()
}

/// Quick helper to create a failed event.
#[must_use]
pub fn progress_failed(operation: &str, message: &str, elapsed_ms: u64) -> ProgressEvent {
    ProgressEvent::builder()
        .event_type(ProgressEventType::Failed)
        .operation(operation)
        .message(message)
        .elapsed_ms(elapsed_ms)
        .timestamp(chrono::Utc::now().to_rfc3339())
        .build()
}

/// Quick helper to create a running event with progress.
#[must_use]
pub fn progress_running(
    operation: &str,
    message: &str,
    processed: u64,
    total: u64,
) -> ProgressEvent {
    let mut builder = ProgressEvent::builder()
        .event_type(ProgressEventType::Running)
        .operation(operation)
        .message(message)
        .processed_items(processed)
        .total_items(total);
    if total > 0 {
        builder = builder.progress(processed as f64 / total as f64);
    }
    builder.timestamp(chrono::Utc::now().to_rfc3339()).build()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
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

    #[test]
    fn progress_event_type_roundtrip() -> TestResult {
        for event_type in [
            ProgressEventType::Started,
            ProgressEventType::Running,
            ProgressEventType::Completed,
            ProgressEventType::Failed,
            ProgressEventType::Warning,
            ProgressEventType::Info,
            ProgressEventType::Debug,
        ] {
            let s = event_type.as_str();
            let parsed: ProgressEventType = s
                .parse()
                .map_err(|e: ParseProgressEventTypeError| e.to_string())?;
            ensure(parsed, event_type, &format!("roundtrip {s}"))?;
        }
        Ok(())
    }

    #[test]
    fn progress_event_type_accepts_operator_spelling_variants() -> TestResult {
        let trimmed = ProgressEventType::from_str(" Started ")
            .map_err(|error: ParseProgressEventTypeError| error.to_string())?;
        let uppercase = ProgressEventType::from_str("WARNING")
            .map_err(|error: ParseProgressEventTypeError| error.to_string())?;

        ensure(trimmed, ProgressEventType::Started, "trimmed event type")?;
        ensure(
            uppercase,
            ProgressEventType::Warning,
            "uppercase event type",
        )
    }

    #[test]
    fn progress_event_type_display() {
        assert_eq!(ProgressEventType::Started.to_string(), "started");
        assert_eq!(ProgressEventType::Completed.to_string(), "completed");
        assert_eq!(ProgressEventType::Failed.to_string(), "failed");
    }

    #[test]
    fn progress_event_type_is_terminal() {
        assert!(!ProgressEventType::Started.is_terminal());
        assert!(!ProgressEventType::Running.is_terminal());
        assert!(ProgressEventType::Completed.is_terminal());
        assert!(ProgressEventType::Failed.is_terminal());
    }

    #[test]
    fn progress_event_type_is_error() {
        assert!(!ProgressEventType::Started.is_error());
        assert!(!ProgressEventType::Completed.is_error());
        assert!(ProgressEventType::Failed.is_error());
    }

    #[test]
    fn progress_event_builder() {
        let event = ProgressEvent::builder()
            .event_type(ProgressEventType::Running)
            .operation("import")
            .message("Processing records")
            .processed_items(50)
            .total_items(100)
            .progress(0.5)
            .timestamp("2026-04-30T12:00:00Z")
            .build();

        assert_eq!(event.schema, PROGRESS_EVENT_SCHEMA_V1);
        assert_eq!(event.event_type, ProgressEventType::Running);
        assert_eq!(event.operation, "import");
        assert_eq!(event.progress, Some(0.5));
        assert_eq!(event.processed_items, Some(50));
        assert_eq!(event.total_items, Some(100));
    }

    #[test]
    fn progress_event_builder_bounds_progress_percentage() {
        let below = ProgressEvent::builder().progress(-0.25).build();
        let above = ProgressEvent::builder().progress(1.25).build();
        let nan = ProgressEvent::builder().progress(f64::NAN).build();
        let infinite = ProgressEvent::builder().progress(f64::INFINITY).build();

        assert_eq!(below.progress, Some(0.0));
        assert_eq!(above.progress, Some(1.0));
        assert_eq!(nan.progress, None);
        assert_eq!(infinite.progress, None);
    }

    #[test]
    fn progress_running_omits_unknown_percentage_and_clamps_overruns() {
        let unknown_total = progress_running("import", "Counting records", 5, 0);
        let overrun = progress_running("import", "Processing records", 12, 10);

        assert_eq!(unknown_total.progress, None);
        assert_eq!(unknown_total.processed_items, Some(5));
        assert_eq!(unknown_total.total_items, Some(0));
        assert_eq!(overrun.progress, Some(1.0));
    }

    #[test]
    fn progress_event_to_jsonl() {
        let event = ProgressEvent::builder()
            .event_type(ProgressEventType::Started)
            .operation("test")
            .message("Starting test")
            .timestamp("2026-04-30T12:00:00Z")
            .build();

        let jsonl = event.to_jsonl();
        assert!(jsonl.contains(r#""schema":"ee.progress.v1""#));
        assert!(jsonl.contains(r#""event_type":"started""#));
        assert!(jsonl.contains(r#""operation":"test""#));
    }

    #[test]
    fn progress_event_serializes_without_optional_fields() -> TestResult {
        let event = ProgressEvent::builder()
            .event_type(ProgressEventType::Info)
            .operation("info")
            .message("Just info")
            .timestamp("2026-04-30T12:00:00Z")
            .build();

        let json = serde_json::to_string(&event).map_err(|error| error.to_string())?;
        let value: serde_json::Value =
            serde_json::from_str(&json).map_err(|error| error.to_string())?;
        let object = value
            .as_object()
            .ok_or_else(|| format!("event json should be an object: {value}"))?;
        assert!(!object.contains_key("progress"));
        assert!(!object.contains_key("current_item"));
        assert!(!object.contains_key("total_items"));
        Ok(())
    }

    #[test]
    fn parse_invalid_progress_event_type() -> TestResult {
        let result: Result<ProgressEventType, _> = "invalid".parse();
        assert!(result.is_err());
        let err = match result {
            Ok(parsed) => return Err(format!("invalid event type parsed as {parsed:?}")),
            Err(error) => error,
        };
        if err.to_string().contains("invalid progress event type") {
            Ok(())
        } else {
            Err(format!("unexpected parse error: {err}"))
        }
    }
}
