//! JSONL export with redaction support (EE-221).
//!
//! Provides functions for exporting memories and related records to JSONL format
//! with configurable redaction levels. Redaction removes or masks sensitive content
//! before export.

use std::io::{self, Write};

use crate::models::{
    EXPORT_FORMAT_VERSION, ExportAgentRecord, ExportArtifactRecord, ExportAuditRecord,
    ExportFooter, ExportHeader, ExportLinkRecord, ExportMemoryRecord, ExportRecord, ExportScope,
    ExportTagRecord, ExportWorkspaceRecord, RedactionLevel,
};

/// Patterns that indicate sensitive content requiring redaction.
const SECRET_PATTERNS: &[&str] = &[
    "password",
    "secret",
    "api_key",
    "apikey",
    "api-key",
    "token",
    "bearer",
    "authorization",
    "auth",
    "credential",
    "private_key",
    "privatekey",
    "access_key",
    "accesskey",
    "secret_key",
    "secretkey",
    "aws_",
    "gcp_",
    "azure_",
    "database_url",
    "connection_string",
    "-----begin",
    "-----end",
];

/// Placeholder for redacted content.
pub const REDACTED_PLACEHOLDER: &str = "[REDACTED]";

/// Placeholder for redacted paths.
pub const REDACTED_PATH_PLACEHOLDER: &str = "[REDACTED_PATH]";

/// Placeholder for redacted identifiers.
pub const REDACTED_ID_PLACEHOLDER: &str = "[REDACTED_ID]";

/// Check if content contains patterns that suggest secrets.
#[must_use]
pub fn contains_secret_pattern(content: &str) -> bool {
    let lower = content.to_lowercase();
    SECRET_PATTERNS.iter().any(|pat| lower.contains(pat))
}

/// Apply redaction to text content based on redaction level.
#[must_use]
pub fn redact_content(content: &str, level: RedactionLevel) -> String {
    match level {
        RedactionLevel::None => content.to_owned(),
        RedactionLevel::Minimal => {
            if contains_secret_pattern(content) {
                REDACTED_PLACEHOLDER.to_owned()
            } else {
                content.to_owned()
            }
        }
        RedactionLevel::Standard => {
            if contains_secret_pattern(content) {
                REDACTED_PLACEHOLDER.to_owned()
            } else {
                redact_paths_in_content(content)
            }
        }
        RedactionLevel::Full => REDACTED_PLACEHOLDER.to_owned(),
    }
}

/// Redact file paths in content.
fn redact_paths_in_content(content: &str) -> String {
    let mut result = content.to_owned();
    for prefix in ["/home/", "/Users/", "/data/", "C:\\", "D:\\"] {
        if result.contains(prefix) {
            let lines: Vec<&str> = result.lines().collect();
            let redacted_lines: Vec<String> = lines
                .iter()
                .map(|line| {
                    if line.contains(prefix) {
                        let mut output = String::new();
                        let mut chars = line.chars().peekable();
                        while let Some(c) = chars.next() {
                            if line[output.len()..].starts_with(prefix) {
                                output.push_str(REDACTED_PATH_PLACEHOLDER);
                                while let Some(&next) = chars.peek() {
                                    if next.is_whitespace() || next == '"' || next == '\'' {
                                        break;
                                    }
                                    chars.next();
                                }
                            } else {
                                output.push(c);
                            }
                        }
                        output
                    } else {
                        (*line).to_owned()
                    }
                })
                .collect();
            result = redacted_lines.join("\n");
        }
    }
    result
}

/// Redact a path string.
#[must_use]
pub fn redact_path(path: &str, level: RedactionLevel) -> String {
    match level {
        RedactionLevel::None | RedactionLevel::Minimal => path.to_owned(),
        RedactionLevel::Standard | RedactionLevel::Full => {
            if path.starts_with("/home/")
                || path.starts_with("/Users/")
                || path.starts_with("/data/")
                || path.starts_with("C:\\")
                || path.starts_with("D:\\")
            {
                REDACTED_PATH_PLACEHOLDER.to_owned()
            } else {
                path.to_owned()
            }
        }
    }
}

/// Redact an identifier string (memory ID, agent name, etc.).
#[must_use]
pub fn redact_identifier(id: &str, level: RedactionLevel) -> String {
    match level {
        RedactionLevel::None | RedactionLevel::Minimal => id.to_owned(),
        RedactionLevel::Standard => {
            if id.len() > 8 {
                format!("{}...{}", &id[..4], &id[id.len() - 4..])
            } else {
                id.to_owned()
            }
        }
        RedactionLevel::Full => REDACTED_ID_PLACEHOLDER.to_owned(),
    }
}

/// Apply redaction to an export memory record.
#[must_use]
pub fn redact_memory_record(
    mut record: ExportMemoryRecord,
    level: RedactionLevel,
) -> ExportMemoryRecord {
    if level == RedactionLevel::None {
        return record;
    }

    record.content = redact_content(&record.content, level);
    record.redacted = level != RedactionLevel::None;
    record.redaction_reason = Some(format!("redaction_level:{}", level.as_str()));

    if level.redacts_identifiers() {
        record.memory_id = redact_identifier(&record.memory_id, level);
        record.workspace_id = redact_identifier(&record.workspace_id, level);
        if let Some(agent) = record.source_agent.as_ref() {
            record.source_agent = Some(redact_identifier(agent, level));
        }
    }

    if level.redacts_paths() {
        if let Some(uri) = record.provenance_uri.as_ref() {
            record.provenance_uri = Some(redact_path(uri, level));
        }
    }

    record
}

/// Apply redaction to an export artifact record.
#[must_use]
pub fn redact_artifact_record(
    mut record: ExportArtifactRecord,
    level: RedactionLevel,
) -> ExportArtifactRecord {
    if level == RedactionLevel::None {
        return record;
    }

    if let Some(snippet) = record.snippet.as_ref() {
        record.snippet = Some(redact_content(snippet, level));
    }

    if level.redacts_paths() {
        if let Some(path) = record.original_path.as_ref() {
            record.original_path = Some(redact_path(path, level));
        }
        if let Some(path) = record.canonical_path.as_ref() {
            record.canonical_path = Some(redact_path(path, level));
        }
        if let Some(uri) = record.provenance_uri.as_ref() {
            record.provenance_uri = Some(redact_path(uri, level));
        }
    }

    if level.redacts_identifiers() {
        record.artifact_id = redact_identifier(&record.artifact_id, level);
        record.workspace_id = redact_identifier(&record.workspace_id, level);
    }

    if level.redacts_content() {
        record.snippet = None;
        record.metadata = None;
    }

    record
}

/// Apply redaction to an export workspace record.
#[must_use]
pub fn redact_workspace_record(
    mut record: ExportWorkspaceRecord,
    level: RedactionLevel,
) -> ExportWorkspaceRecord {
    if level == RedactionLevel::None {
        return record;
    }

    if level.redacts_paths() {
        record.path = redact_path(&record.path, level);
    }

    if level.redacts_identifiers() {
        record.workspace_id = redact_identifier(&record.workspace_id, level);
        if let Some(name) = record.name.as_ref() {
            record.name = Some(redact_identifier(name, level));
        }
    }

    record
}

/// Apply redaction to an export agent record.
#[must_use]
pub fn redact_agent_record(
    mut record: ExportAgentRecord,
    level: RedactionLevel,
) -> ExportAgentRecord {
    if level == RedactionLevel::None {
        return record;
    }

    if level.redacts_identifiers() {
        record.agent_id = redact_identifier(&record.agent_id, level);
    }

    record
}

/// Apply redaction to an export audit record.
#[must_use]
pub fn redact_audit_record(
    mut record: ExportAuditRecord,
    level: RedactionLevel,
) -> ExportAuditRecord {
    if level == RedactionLevel::None {
        return record;
    }

    if level.redacts_identifiers() {
        record.audit_id = redact_identifier(&record.audit_id, level);
        record.target_id = redact_identifier(&record.target_id, level);
        if let Some(by) = record.performed_by.as_ref() {
            record.performed_by = Some(redact_identifier(by, level));
        }
    }

    if level.redacts_content() {
        record.details = None;
    }

    record
}

/// Apply redaction to any export record.
#[must_use]
pub fn redact_record(record: ExportRecord, level: RedactionLevel) -> ExportRecord {
    match record {
        ExportRecord::Header(h) => ExportRecord::Header(redact_header(h, level)),
        ExportRecord::Memory(m) => ExportRecord::Memory(redact_memory_record(m, level)),
        ExportRecord::Artifact(a) => ExportRecord::Artifact(redact_artifact_record(a, level)),
        ExportRecord::Link(l) => ExportRecord::Link(redact_link_record(l, level)),
        ExportRecord::Tag(t) => ExportRecord::Tag(redact_tag_record(t, level)),
        ExportRecord::Agent(a) => ExportRecord::Agent(redact_agent_record(a, level)),
        ExportRecord::Workspace(w) => ExportRecord::Workspace(redact_workspace_record(w, level)),
        ExportRecord::Audit(a) => ExportRecord::Audit(redact_audit_record(a, level)),
        ExportRecord::Footer(f) => ExportRecord::Footer(f),
    }
}

fn redact_header(mut header: ExportHeader, level: RedactionLevel) -> ExportHeader {
    if level.redacts_paths() {
        if let Some(path) = header.workspace_path.as_ref() {
            header.workspace_path = Some(redact_path(path, level));
        }
    }
    if level.redacts_identifiers() {
        if let Some(id) = header.workspace_id.as_ref() {
            header.workspace_id = Some(redact_identifier(id, level));
        }
        header.export_id = redact_identifier(&header.export_id, level);
        if let Some(host) = header.hostname.as_ref() {
            header.hostname = Some(redact_identifier(host, level));
        }
    }
    header
}

fn redact_link_record(mut record: ExportLinkRecord, level: RedactionLevel) -> ExportLinkRecord {
    if level.redacts_identifiers() {
        record.link_id = redact_identifier(&record.link_id, level);
        record.source_memory_id = redact_identifier(&record.source_memory_id, level);
        record.target_memory_id = redact_identifier(&record.target_memory_id, level);
    }
    if level.redacts_content() {
        record.metadata = None;
    }
    record
}

fn redact_tag_record(mut record: ExportTagRecord, level: RedactionLevel) -> ExportTagRecord {
    if level.redacts_identifiers() {
        record.memory_id = redact_identifier(&record.memory_id, level);
    }
    if level.redacts_content() {
        record.tag = REDACTED_PLACEHOLDER.to_owned();
    }
    record
}

/// JSONL export writer.
pub struct JsonlExporter<W: Write> {
    writer: W,
    redaction_level: RedactionLevel,
    export_scope: ExportScope,
    records_written: u64,
    memory_count: u64,
    artifact_count: u64,
    link_count: u64,
    tag_count: u64,
    audit_count: u64,
}

impl<W: Write> JsonlExporter<W> {
    /// Create a new JSONL exporter.
    pub fn new(writer: W, redaction_level: RedactionLevel, export_scope: ExportScope) -> Self {
        Self {
            writer,
            redaction_level,
            export_scope,
            records_written: 0,
            memory_count: 0,
            artifact_count: 0,
            link_count: 0,
            tag_count: 0,
            audit_count: 0,
        }
    }

    /// Get the redaction level.
    #[must_use]
    pub const fn redaction_level(&self) -> RedactionLevel {
        self.redaction_level
    }

    /// Get the export scope.
    #[must_use]
    pub const fn export_scope(&self) -> ExportScope {
        self.export_scope
    }

    /// Get the number of records written.
    #[must_use]
    pub const fn records_written(&self) -> u64 {
        self.records_written
    }

    /// Write the export header.
    ///
    /// # Errors
    ///
    /// Returns an error if writing fails.
    pub fn write_header(&mut self, mut header: ExportHeader) -> io::Result<()> {
        header.redaction_level = self.redaction_level;
        header.export_scope = self.export_scope;
        header.format_version = EXPORT_FORMAT_VERSION;

        let redacted = redact_header(header, self.redaction_level);
        let json = serde_json::to_string(&redacted)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        writeln!(self.writer, "{json}")?;
        self.records_written += 1;
        Ok(())
    }

    /// Write a memory record.
    ///
    /// # Errors
    ///
    /// Returns an error if writing fails.
    pub fn write_memory(&mut self, record: ExportMemoryRecord) -> io::Result<()> {
        if !self.export_scope.includes_memories() {
            return Ok(());
        }

        let redacted = redact_memory_record(record, self.redaction_level);

        if self.export_scope == ExportScope::MetadataOnly {
            let mut meta_only = redacted;
            meta_only.content = String::new();
            let json = serde_json::to_string(&meta_only)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            writeln!(self.writer, "{json}")?;
        } else {
            let json = serde_json::to_string(&redacted)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            writeln!(self.writer, "{json}")?;
        }

        self.records_written += 1;
        self.memory_count += 1;
        Ok(())
    }

    /// Write an artifact record.
    ///
    /// # Errors
    ///
    /// Returns an error if writing fails.
    pub fn write_artifact(&mut self, record: ExportArtifactRecord) -> io::Result<()> {
        if !self.export_scope.includes_artifacts() {
            return Ok(());
        }

        let redacted = redact_artifact_record(record, self.redaction_level);

        if self.export_scope == ExportScope::MetadataOnly {
            let mut meta_only = redacted;
            meta_only.snippet = None;
            let json = serde_json::to_string(&meta_only)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            writeln!(self.writer, "{json}")?;
        } else {
            let json = serde_json::to_string(&redacted)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            writeln!(self.writer, "{json}")?;
        }

        self.records_written += 1;
        self.artifact_count += 1;
        Ok(())
    }

    /// Write a link record.
    ///
    /// # Errors
    ///
    /// Returns an error if writing fails.
    pub fn write_link(&mut self, record: ExportLinkRecord) -> io::Result<()> {
        if !self.export_scope.includes_links() {
            return Ok(());
        }

        let redacted = redact_link_record(record, self.redaction_level);
        let json = serde_json::to_string(&redacted)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        writeln!(self.writer, "{json}")?;
        self.records_written += 1;
        self.link_count += 1;
        Ok(())
    }

    /// Write a tag record.
    ///
    /// # Errors
    ///
    /// Returns an error if writing fails.
    pub fn write_tag(&mut self, record: ExportTagRecord) -> io::Result<()> {
        if !self.export_scope.includes_memories() {
            return Ok(());
        }

        let redacted = redact_tag_record(record, self.redaction_level);
        let json = serde_json::to_string(&redacted)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        writeln!(self.writer, "{json}")?;
        self.records_written += 1;
        self.tag_count += 1;
        Ok(())
    }

    /// Write an audit record.
    ///
    /// # Errors
    ///
    /// Returns an error if writing fails.
    pub fn write_audit(&mut self, record: ExportAuditRecord) -> io::Result<()> {
        if !self.export_scope.includes_audit() {
            return Ok(());
        }

        let redacted = redact_audit_record(record, self.redaction_level);
        let json = serde_json::to_string(&redacted)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        writeln!(self.writer, "{json}")?;
        self.records_written += 1;
        self.audit_count += 1;
        Ok(())
    }

    /// Write an agent record.
    ///
    /// # Errors
    ///
    /// Returns an error if writing fails.
    pub fn write_agent(&mut self, record: ExportAgentRecord) -> io::Result<()> {
        let redacted = redact_agent_record(record, self.redaction_level);
        let json = serde_json::to_string(&redacted)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        writeln!(self.writer, "{json}")?;
        self.records_written += 1;
        Ok(())
    }

    /// Write a workspace record.
    ///
    /// # Errors
    ///
    /// Returns an error if writing fails.
    pub fn write_workspace(&mut self, record: ExportWorkspaceRecord) -> io::Result<()> {
        let redacted = redact_workspace_record(record, self.redaction_level);
        let json = serde_json::to_string(&redacted)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        writeln!(self.writer, "{json}")?;
        self.records_written += 1;
        Ok(())
    }

    /// Write the export footer and return counts.
    ///
    /// # Errors
    ///
    /// Returns an error if writing fails.
    pub fn write_footer(&mut self, mut footer: ExportFooter) -> io::Result<ExportStats> {
        footer.total_records = self.records_written;
        footer.memory_count = self.memory_count;
        footer.link_count = self.link_count;
        footer.tag_count = self.tag_count;
        footer.audit_count = self.audit_count;
        footer.success = true;

        let json = serde_json::to_string(&footer)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        writeln!(self.writer, "{json}")?;
        self.records_written += 1;

        Ok(ExportStats {
            total_records: self.records_written,
            memory_count: self.memory_count,
            artifact_count: self.artifact_count,
            link_count: self.link_count,
            tag_count: self.tag_count,
            audit_count: self.audit_count,
            redaction_level: self.redaction_level,
            export_scope: self.export_scope,
        })
    }

    /// Flush the writer.
    ///
    /// # Errors
    ///
    /// Returns an error if flushing fails.
    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

/// Statistics about an export operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportStats {
    pub total_records: u64,
    pub memory_count: u64,
    pub artifact_count: u64,
    pub link_count: u64,
    pub tag_count: u64,
    pub audit_count: u64,
    pub redaction_level: RedactionLevel,
    pub export_scope: ExportScope,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::models::{
        EXPORT_ARTIFACT_SCHEMA_V1, EXPORT_MEMORY_SCHEMA_V1, ExportHeader, ExportMemoryRecord,
    };

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn secret_fixture(parts: &[&str]) -> String {
        parts.concat()
    }

    fn secret_assignment(value: &str) -> String {
        format!("{}={value}", secret_fixture(&["api", "_", "key"]))
    }

    #[test]
    fn contains_secret_pattern_detects_secrets() {
        assert!(contains_secret_pattern(&secret_fixture(&[
            "api",
            "_key=abc123"
        ])));
        assert!(contains_secret_pattern(&secret_fixture(&[
            "PASS",
            "WORD: hunter2"
        ])));
        assert!(contains_secret_pattern(&secret_fixture(&[
            "Bearer ", "token123"
        ])));
        assert!(contains_secret_pattern(&secret_fixture(&[
            "AWS", "_SECRET", "_KEY"
        ])));
        assert!(contains_secret_pattern(&secret_fixture(&[
            "-----BEGIN RSA ",
            "PRIVATE ",
            "KEY-----"
        ])));
        assert!(!contains_secret_pattern("just some normal content"));
        assert!(!contains_secret_pattern("public data here"));
    }

    #[test]
    fn redact_content_none_level_preserves() -> TestResult {
        let content = secret_assignment("redaction-fixture");
        let result = redact_content(&content, RedactionLevel::None);
        ensure(result, content, "none level preserves content")
    }

    #[test]
    fn redact_content_minimal_level_redacts_secrets() -> TestResult {
        let sensitive_input = secret_assignment("redaction-fixture");
        let normal = "just normal content";

        ensure(
            redact_content(&sensitive_input, RedactionLevel::Minimal),
            REDACTED_PLACEHOLDER.to_owned(),
            "minimal redacts secrets",
        )?;
        ensure(
            redact_content(normal, RedactionLevel::Minimal),
            normal.to_owned(),
            "minimal preserves normal",
        )
    }

    #[test]
    fn redact_content_full_level_redacts_all() -> TestResult {
        let content = "just normal content";
        ensure(
            redact_content(content, RedactionLevel::Full),
            REDACTED_PLACEHOLDER.to_owned(),
            "full redacts everything",
        )
    }

    #[test]
    fn redact_path_standard_level() -> TestResult {
        ensure(
            redact_path("/home/user/project", RedactionLevel::Standard),
            REDACTED_PATH_PLACEHOLDER.to_owned(),
            "standard redacts home paths",
        )?;
        ensure(
            redact_path("/usr/local/bin", RedactionLevel::Standard),
            "/usr/local/bin".to_owned(),
            "standard preserves system paths",
        )
    }

    #[test]
    fn redact_identifier_standard_level() -> TestResult {
        ensure(
            redact_identifier("mem_abc123xyz456", RedactionLevel::Standard),
            "mem_...z456".to_owned(),
            "standard truncates long IDs",
        )?;
        ensure(
            redact_identifier("short", RedactionLevel::Standard),
            "short".to_owned(),
            "standard preserves short IDs",
        )?;
        ensure(
            redact_identifier("anything", RedactionLevel::Full),
            REDACTED_ID_PLACEHOLDER.to_owned(),
            "full redacts all IDs",
        )
    }

    #[test]
    fn redact_memory_record_minimal() {
        let content = secret_assignment("redaction-fixture");
        let record = ExportMemoryRecord::builder()
            .memory_id("mem-001")
            .workspace_id("ws-123")
            .level("procedural")
            .kind("rule")
            .content(content)
            .created_at("2026-04-30T12:00:00Z")
            .build();

        let redacted = redact_memory_record(record, RedactionLevel::Minimal);

        assert_eq!(redacted.content, REDACTED_PLACEHOLDER);
        assert!(redacted.redacted);
        assert!(redacted.redaction_reason.is_some());
        assert_eq!(redacted.memory_id, "mem-001");
    }

    #[test]
    fn redact_memory_record_standard() {
        let record = ExportMemoryRecord::builder()
            .memory_id("mem-abc123xyz456")
            .workspace_id("ws-def789uvw012")
            .level("procedural")
            .kind("rule")
            .content("normal content")
            .provenance_uri("/home/user/file.txt")
            .created_at("2026-04-30T12:00:00Z")
            .build();

        let redacted = redact_memory_record(record, RedactionLevel::Standard);

        assert_eq!(redacted.memory_id, "mem-...z456");
        assert_eq!(redacted.workspace_id, "ws-d...w012");
        assert_eq!(
            redacted.provenance_uri,
            Some(REDACTED_PATH_PLACEHOLDER.to_owned())
        );
    }

    #[test]
    fn jsonl_exporter_writes_header() {
        let mut output = Vec::new();
        let mut exporter = JsonlExporter::new(&mut output, RedactionLevel::None, ExportScope::All);

        let header = ExportHeader::builder()
            .created_at("2026-04-30T12:00:00Z")
            .ee_version("0.1.0")
            .export_id("test-export")
            .build();

        exporter.write_header(header).expect("write header");

        let written = String::from_utf8(output).expect("valid utf8");
        assert!(written.contains("ee.export.header.v1"));
        assert!(written.ends_with('\n'));
    }

    #[test]
    fn jsonl_exporter_writes_memory() {
        let mut output = Vec::new();

        let memory = ExportMemoryRecord::builder()
            .memory_id("mem-001")
            .workspace_id("ws-123")
            .level("procedural")
            .kind("rule")
            .content("Test content")
            .created_at("2026-04-30T12:00:00Z")
            .build();

        let memory_count = {
            let mut exporter =
                JsonlExporter::new(&mut output, RedactionLevel::None, ExportScope::All);
            exporter.write_memory(memory).expect("write memory");
            exporter.memory_count
        };

        let written = String::from_utf8(output).expect("valid utf8");
        assert!(written.contains("ee.export.memory.v1"));
        assert!(written.contains("Test content"));
        assert_eq!(memory_count, 1);
    }

    #[test]
    fn jsonl_exporter_writes_artifact_with_redaction() -> TestResult {
        let mut output = Vec::new();
        let secret_fixture = format!("api_{}={}", "key", "redaction-fixture");

        let artifact = ExportArtifactRecord::builder()
            .artifact_id("art_01234567890123456789012345")
            .workspace_id("wsp_01234567890123456789012345")
            .source_kind("file")
            .artifact_type("log")
            .canonical_path("/data/projects/example/logs/build.log")
            .content_hash("blake3:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
            .media_type("text/plain")
            .size_bytes(42)
            .redaction_status("checked")
            .snippet(secret_fixture.clone())
            .created_at("2026-04-30T12:00:00Z")
            .updated_at("2026-04-30T12:00:00Z")
            .build();

        let artifact_count = {
            let mut exporter =
                JsonlExporter::new(&mut output, RedactionLevel::Standard, ExportScope::All);
            exporter
                .write_artifact(artifact)
                .map_err(|error| format!("write artifact: {error}"))?;
            exporter.artifact_count
        };

        let written = String::from_utf8(output).map_err(|error| format!("valid utf8: {error}"))?;
        assert!(written.contains(EXPORT_ARTIFACT_SCHEMA_V1));
        assert!(written.contains(REDACTED_PLACEHOLDER));
        assert!(written.contains(REDACTED_PATH_PLACEHOLDER));
        assert!(!written.contains(&secret_fixture));
        ensure(artifact_count, 1, "artifact count")
    }

    #[test]
    fn jsonl_exporter_respects_scope() {
        let mut output = Vec::new();

        let memory = ExportMemoryRecord::builder()
            .memory_id("mem-001")
            .workspace_id("ws-123")
            .level("procedural")
            .kind("rule")
            .content("Test content")
            .created_at("2026-04-30T12:00:00Z")
            .build();

        let memory_count = {
            let mut exporter =
                JsonlExporter::new(&mut output, RedactionLevel::None, ExportScope::Audit);
            exporter.write_memory(memory).expect("write memory");
            exporter.memory_count
        };

        let written = String::from_utf8(output).expect("valid utf8");
        assert!(written.is_empty());
        assert_eq!(memory_count, 0);
    }

    #[test]
    fn jsonl_exporter_metadata_only_strips_content() {
        let mut output = Vec::new();
        let mut exporter =
            JsonlExporter::new(&mut output, RedactionLevel::None, ExportScope::MetadataOnly);

        let memory = ExportMemoryRecord::builder()
            .memory_id("mem-001")
            .workspace_id("ws-123")
            .level("procedural")
            .kind("rule")
            .content("Sensitive content here")
            .created_at("2026-04-30T12:00:00Z")
            .build();

        exporter.write_memory(memory).expect("write memory");

        let written = String::from_utf8(output).expect("valid utf8");
        assert!(written.contains(EXPORT_MEMORY_SCHEMA_V1));
        assert!(!written.contains("Sensitive content here"));
        assert!(written.contains(r#""content":"""#));
    }

    #[test]
    fn jsonl_exporter_applies_redaction() {
        let mut output = Vec::new();
        let mut exporter =
            JsonlExporter::new(&mut output, RedactionLevel::Minimal, ExportScope::All);

        let memory = ExportMemoryRecord::builder()
            .memory_id("mem-001")
            .workspace_id("ws-123")
            .level("procedural")
            .kind("rule")
            .content(secret_assignment("redaction-fixture"))
            .created_at("2026-04-30T12:00:00Z")
            .build();

        exporter.write_memory(memory).expect("write memory");

        let written = String::from_utf8(output).expect("valid utf8");
        assert!(written.contains(REDACTED_PLACEHOLDER));
        assert!(!written.contains("redaction-fixture"));
    }

    #[test]
    fn jsonl_exporter_footer_includes_counts() {
        let mut output = Vec::new();
        let mut exporter = JsonlExporter::new(&mut output, RedactionLevel::None, ExportScope::All);

        let header = ExportHeader::builder()
            .created_at("2026-04-30T12:00:00Z")
            .ee_version("0.1.0")
            .export_id("test-export")
            .build();
        exporter.write_header(header).expect("write header");

        for i in 0..3 {
            let memory = ExportMemoryRecord::builder()
                .memory_id(format!("mem-{i:03}"))
                .workspace_id("ws-123")
                .level("procedural")
                .kind("rule")
                .content(format!("Content {i}"))
                .created_at("2026-04-30T12:00:00Z")
                .build();
            exporter.write_memory(memory).expect("write memory");
        }

        let footer = ExportFooter::builder()
            .export_id("test-export")
            .completed_at("2026-04-30T12:01:00Z")
            .build();
        let stats = exporter.write_footer(footer).expect("write footer");

        assert_eq!(stats.memory_count, 3);
        assert_eq!(stats.total_records, 5);
    }

    #[test]
    fn redact_record_union() -> TestResult {
        let content = secret_assignment("redaction-fixture");
        let memory = ExportRecord::Memory(
            ExportMemoryRecord::builder()
                .memory_id("mem-001")
                .workspace_id("ws-123")
                .level("procedural")
                .kind("rule")
                .content(content)
                .created_at("2026-04-30T12:00:00Z")
                .build(),
        );

        let redacted = redact_record(memory, RedactionLevel::Minimal);

        if let ExportRecord::Memory(m) = redacted {
            return ensure(
                m.content,
                REDACTED_PLACEHOLDER.to_owned(),
                "memory content redacted",
            );
        }

        Err("expected memory variant".to_owned())
    }
}
