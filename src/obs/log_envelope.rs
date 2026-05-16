//! Stable structured logging and audit JSONL envelopes.
//!
//! The CLI may choose different transports for diagnostics, but the machine
//! row shape is pinned here so tests can validate log and audit streams without
//! depending on a tracing subscriber implementation detail.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const LOG_ENVELOPE_SCHEMA_V1: &str = "ee.log.v1";
pub const AUDIT_EVENT_SCHEMA_V1: &str = "ee.audit.v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditOutcome {
    Success,
    Failure,
    Cancelled,
    DryRun,
    Rollback,
}

impl AuditOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Cancelled => "cancelled",
            Self::DryRun => "dry_run",
            Self::Rollback => "rollback",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LogEnvelope {
    pub schema: String,
    pub ts: String,
    pub level: String,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    pub fields: Map<String, Value>,
}

impl LogEnvelope {
    #[must_use]
    pub fn new(ts: impl Into<String>, level: LogLevel, target: impl Into<String>) -> Self {
        Self {
            schema: LOG_ENVELOPE_SCHEMA_V1.to_owned(),
            ts: ts.into(),
            level: level.as_str().to_owned(),
            target: target.into(),
            span_id: None,
            trace_id: None,
            fields: Map::new(),
        }
    }

    #[must_use]
    pub fn with_span_id(mut self, span_id: impl Into<String>) -> Self {
        self.span_id = Some(span_id.into());
        self
    }

    #[must_use]
    pub fn with_trace_id(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }

    #[must_use]
    pub fn with_field(mut self, key: impl Into<String>, value: Value) -> Self {
        self.fields.insert(key.into(), value);
        self
    }

    pub fn to_json_line(&self) -> Result<String, serde_json::Error> {
        let mut line = serde_json::to_string(self)?;
        line.push('\n');
        Ok(line)
    }

    pub fn write_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let line = self
            .to_json_line()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        writer.write_all(line.as_bytes())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuditEvent {
    pub schema: String,
    pub ts: String,
    pub actor: String,
    pub action: String,
    pub subject: String,
    pub outcome: String,
    pub fields: Map<String, Value>,
}

impl AuditEvent {
    #[must_use]
    pub fn new(
        ts: impl Into<String>,
        actor: impl Into<String>,
        action: impl Into<String>,
        subject: impl Into<String>,
        outcome: AuditOutcome,
    ) -> Self {
        Self {
            schema: AUDIT_EVENT_SCHEMA_V1.to_owned(),
            ts: ts.into(),
            actor: actor.into(),
            action: action.into(),
            subject: subject.into(),
            outcome: outcome.as_str().to_owned(),
            fields: Map::new(),
        }
    }

    #[must_use]
    pub fn with_field(mut self, key: impl Into<String>, value: Value) -> Self {
        self.fields.insert(key.into(), value);
        self
    }

    pub fn to_json_line(&self) -> Result<String, serde_json::Error> {
        let mut line = serde_json::to_string(self)?;
        line.push('\n');
        Ok(line)
    }

    pub fn write_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let line = self
            .to_json_line()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        writer.write_all(line.as_bytes())
    }

    pub fn append_to_path(&self, path: &Path) -> io::Result<()> {
        ensure_append_path_has_no_symlink_components(path)?;
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        self.write_to(&mut file)
    }
}

fn ensure_append_path_has_no_symlink_components(path: &Path) -> io::Result<()> {
    if let Some(symlink_path) = first_existing_symlink_component(path)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing to append audit event to '{}': path traverses symbolic link '{}'",
                path.display(),
                symlink_path.display()
            ),
        ));
    }
    Ok(())
}

fn first_existing_symlink_component(path: &Path) -> io::Result<Option<PathBuf>> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(Some(current)),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "failed to inspect audit append path component '{}': {error}",
                        current.display()
                    ),
                ));
            }
        }
    }
    Ok(None)
}

#[must_use]
pub fn now_rfc3339_nanos() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true)
}

#[cfg(test)]
mod tests {
    use super::{AuditEvent, AuditOutcome, LogEnvelope, LogLevel};
    use serde_json::Value;

    type TestResult = Result<(), String>;

    #[test]
    fn log_level_strings_are_stable() {
        assert_eq!(LogLevel::Trace.as_str(), "trace");
        assert_eq!(LogLevel::Debug.as_str(), "debug");
        assert_eq!(LogLevel::Info.as_str(), "info");
        assert_eq!(LogLevel::Warn.as_str(), "warn");
        assert_eq!(LogLevel::Error.as_str(), "error");
    }

    #[test]
    fn audit_outcome_strings_are_stable() {
        assert_eq!(AuditOutcome::Success.as_str(), "success");
        assert_eq!(AuditOutcome::Failure.as_str(), "failure");
        assert_eq!(AuditOutcome::Cancelled.as_str(), "cancelled");
        assert_eq!(AuditOutcome::DryRun.as_str(), "dry_run");
        assert_eq!(AuditOutcome::Rollback.as_str(), "rollback");
    }

    #[test]
    fn log_envelope_serializes_as_one_json_line() -> TestResult {
        let envelope = LogEnvelope::new("2026-05-06T00:00:00.123456789Z", LogLevel::Info, "ee")
            .with_span_id("span-1")
            .with_trace_id("trace-1")
            .with_field("command", Value::String("status".to_owned()));
        let line = envelope.to_json_line().map_err(|error| error.to_string())?;

        assert!(line.ends_with('\n'));
        assert_eq!(line.lines().count(), 1);
        assert_eq!(
            line.trim_end(),
            r#"{"schema":"ee.log.v1","ts":"2026-05-06T00:00:00.123456789Z","level":"info","target":"ee","span_id":"span-1","trace_id":"trace-1","fields":{"command":"status"}}"#
        );
        Ok(())
    }

    #[test]
    fn audit_event_serializes_as_one_json_line() -> TestResult {
        let event = AuditEvent::new(
            "2026-05-06T00:00:00.123456789Z",
            "agent:SwiftCat",
            "remember",
            "memory:mem_01",
            AuditOutcome::Success,
        )
        .with_field("audit_id", Value::String("audit_01".to_owned()));
        let line = event.to_json_line().map_err(|error| error.to_string())?;

        assert!(line.ends_with('\n'));
        assert_eq!(line.lines().count(), 1);
        assert_eq!(
            line.trim_end(),
            r#"{"schema":"ee.audit.v1","ts":"2026-05-06T00:00:00.123456789Z","actor":"agent:SwiftCat","action":"remember","subject":"memory:mem_01","outcome":"success","fields":{"audit_id":"audit_01"}}"#
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn audit_event_append_rejects_symlinked_path_components() -> TestResult {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let real_dir = tempdir.path().join("real-ee");
        std::fs::create_dir_all(&real_dir).map_err(|error| error.to_string())?;
        let linked_dir = tempdir.path().join(".ee");
        symlink(&real_dir, &linked_dir).map_err(|error| error.to_string())?;

        let event = AuditEvent::new(
            "2026-05-06T00:00:00.123456789Z",
            "agent:SwiftCat",
            "remember",
            "memory:mem_01",
            AuditOutcome::Success,
        );
        let error = match event.append_to_path(&linked_dir.join("audit.jsonl")) {
            Ok(()) => return Err("append should reject symlinked audit parent".to_owned()),
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(
            error.to_string().contains("path traverses symbolic link"),
            "unexpected error: {error}"
        );
        assert!(
            !real_dir.join("audit.jsonl").exists(),
            "audit append must not write through symlinked parent"
        );

        let ee_dir = tempdir.path().join("safe-ee");
        std::fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
        let outside_audit = tempdir.path().join("outside-audit.jsonl");
        std::fs::write(&outside_audit, "").map_err(|error| error.to_string())?;
        let linked_audit = ee_dir.join("audit.jsonl");
        symlink(&outside_audit, &linked_audit).map_err(|error| error.to_string())?;

        let error = match event.append_to_path(&linked_audit) {
            Ok(()) => return Err("append should reject symlinked audit file".to_owned()),
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(
            error.to_string().contains("path traverses symbolic link"),
            "unexpected error: {error}"
        );
        assert!(
            std::fs::read_to_string(&outside_audit)
                .map_err(|error| error.to_string())?
                .is_empty(),
            "audit append must not write through symlinked file"
        );
        Ok(())
    }
}
