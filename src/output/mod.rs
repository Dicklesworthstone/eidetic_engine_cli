use std::env;
use std::io::IsTerminal;

use crate::core::doctor::DoctorReport;
use crate::core::status::StatusReport;
use crate::models::{DomainError, ERROR_SCHEMA_V1, RESPONSE_SCHEMA_V1};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Renderer {
    #[default]
    Human,
    Json,
    Toon,
    Jsonl,
    Compact,
    Hook,
}

impl Renderer {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Json => "json",
            Self::Toon => "toon",
            Self::Jsonl => "jsonl",
            Self::Compact => "compact",
            Self::Hook => "hook",
        }
    }

    #[must_use]
    pub const fn is_machine_readable(self) -> bool {
        matches!(self, Self::Json | Self::Jsonl | Self::Compact | Self::Hook)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct OutputContext {
    pub renderer: Renderer,
    pub is_tty: bool,
    pub color_enabled: bool,
}

impl OutputContext {
    #[must_use]
    pub fn detect() -> Self {
        Self::detect_with_hints(false, false, None)
    }

    #[must_use]
    pub fn detect_with_hints(
        json_flag: bool,
        robot_flag: bool,
        format_override: Option<Renderer>,
    ) -> Self {
        let is_tty = std::io::stdout().is_terminal();
        let no_color = env::var("NO_COLOR").is_ok();
        let ee_format = env::var("EE_FORMAT").ok();

        let renderer = if let Some(r) = format_override {
            r
        } else if json_flag || robot_flag {
            Renderer::Json
        } else if let Some(fmt) = ee_format {
            match fmt.to_lowercase().as_str() {
                "json" => Renderer::Json,
                "toon" => Renderer::Toon,
                "jsonl" => Renderer::Jsonl,
                "compact" => Renderer::Compact,
                "hook" => Renderer::Hook,
                _ => Renderer::Human,
            }
        } else {
            Renderer::Human
        };

        let color_enabled = is_tty && !no_color && !renderer.is_machine_readable();

        Self {
            renderer,
            is_tty,
            color_enabled,
        }
    }

    #[must_use]
    pub const fn is_machine_output(&self) -> bool {
        self.renderer.is_machine_readable()
    }
}

/// Severity level for degradation notices in the ee.response.v1 envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DegradationSeverity {
    Low,
    Medium,
    High,
}

impl DegradationSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// A single degradation notice in the ee.response.v1 envelope.
///
/// Degradation notices tell consumers that the response is valid but
/// incomplete or limited in some way. The repair field suggests how to
/// resolve the degradation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Degradation {
    pub code: String,
    pub severity: DegradationSeverity,
    pub message: String,
    pub repair: String,
}

impl Degradation {
    #[must_use]
    pub fn new(
        code: impl Into<String>,
        severity: DegradationSeverity,
        message: impl Into<String>,
        repair: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            severity,
            message: message.into(),
            repair: repair.into(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        let mut b = JsonBuilder::new();
        b.field_str("code", &self.code);
        b.field_str("severity", self.severity.as_str());
        b.field_str("message", &self.message);
        b.field_str("repair", &self.repair);
        b.finish()
    }
}

pub struct JsonBuilder {
    buffer: String,
    first: bool,
}

impl JsonBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: String::from("{"),
            first: true,
        }
    }

    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let mut buffer = String::with_capacity(capacity);
        buffer.push('{');
        Self {
            buffer,
            first: true,
        }
    }

    pub fn field_str(&mut self, key: &str, value: &str) -> &mut Self {
        self.separator();
        self.buffer.push('"');
        self.buffer.push_str(key);
        self.buffer.push_str("\":\"");
        self.buffer.push_str(&escape_json_string(value));
        self.buffer.push('"');
        self
    }

    pub fn field_raw(&mut self, key: &str, raw_json: &str) -> &mut Self {
        self.separator();
        self.buffer.push('"');
        self.buffer.push_str(key);
        self.buffer.push_str("\":");
        self.buffer.push_str(raw_json);
        self
    }

    pub fn field_bool(&mut self, key: &str, value: bool) -> &mut Self {
        self.field_raw(key, if value { "true" } else { "false" })
    }

    pub fn field_u32(&mut self, key: &str, value: u32) -> &mut Self {
        self.separator();
        self.buffer.push('"');
        self.buffer.push_str(key);
        self.buffer.push_str("\":");
        self.buffer.push_str(&value.to_string());
        self
    }

    pub fn field_object<F>(&mut self, key: &str, build: F) -> &mut Self
    where
        F: FnOnce(&mut JsonBuilder),
    {
        let mut nested = JsonBuilder::new();
        build(&mut nested);
        let nested_json = nested.finish();
        self.field_raw(key, &nested_json)
    }

    pub fn field_array_of_objects<T, F>(&mut self, key: &str, items: &[T], build: F) -> &mut Self
    where
        F: Fn(&mut JsonBuilder, &T),
    {
        self.separator();
        self.buffer.push('"');
        self.buffer.push_str(key);
        self.buffer.push_str("\":[");
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.buffer.push(',');
            }
            let mut nested = JsonBuilder::new();
            build(&mut nested, item);
            self.buffer.push_str(&nested.finish());
        }
        self.buffer.push(']');
        self
    }

    fn separator(&mut self) {
        if self.first {
            self.first = false;
        } else {
            self.buffer.push(',');
        }
    }

    #[must_use]
    pub fn finish(mut self) -> String {
        self.buffer.push('}');
        self.buffer
    }
}

impl Default for JsonBuilder {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ResponseEnvelope {
    builder: JsonBuilder,
}

impl ResponseEnvelope {
    #[must_use]
    pub fn success() -> Self {
        let mut builder = JsonBuilder::with_capacity(256);
        builder.field_str("schema", RESPONSE_SCHEMA_V1);
        builder.field_bool("success", true);
        Self { builder }
    }

    #[must_use]
    pub fn failure() -> Self {
        let mut builder = JsonBuilder::with_capacity(256);
        builder.field_str("schema", RESPONSE_SCHEMA_V1);
        builder.field_bool("success", false);
        Self { builder }
    }

    pub fn data<F>(mut self, build: F) -> Self
    where
        F: FnOnce(&mut JsonBuilder),
    {
        self.builder.field_object("data", build);
        self
    }

    pub fn data_raw(mut self, raw_json: &str) -> Self {
        self.builder.field_raw("data", raw_json);
        self
    }

    pub fn degraded_array<T, F>(mut self, items: &[T], build: F) -> Self
    where
        F: Fn(&mut JsonBuilder, &T),
    {
        self.builder
            .field_array_of_objects("degraded", items, build);
        self
    }

    #[must_use]
    pub fn finish(self) -> String {
        self.builder.finish()
    }
}

/// Render a status report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_status_json(report: &StatusReport) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "status");
        d.field_str("version", report.version);
        d.field_object("capabilities", |c| {
            c.field_str("runtime", report.capabilities.runtime.as_str());
            c.field_str("storage", report.capabilities.storage.as_str());
            c.field_str("search", report.capabilities.search.as_str());
        });
        d.field_object("runtime", |r| {
            r.field_str("engine", report.runtime.engine);
            r.field_str("profile", report.runtime.profile);
            r.field_raw("workerThreads", &report.runtime.worker_threads.to_string());
            r.field_str("asyncBoundary", report.runtime.async_boundary);
        });
        d.field_array_of_objects("degraded", &report.degradations, |obj, deg| {
            obj.field_str("code", deg.code);
            obj.field_str("severity", deg.severity);
            obj.field_str("message", deg.message);
            obj.field_str("repair", deg.repair);
        });
    });
    b.finish()
}

/// Render a status report as human-readable text.
#[must_use]
pub fn render_status_human(report: &StatusReport) -> String {
    format!(
        "ee status\n\nstorage: {}\nsearch: {}\nruntime: {} ({} {})\n\nNext:\n  ee status --json\n",
        report.capabilities.storage.as_str(),
        report.capabilities.search.as_str(),
        report.capabilities.runtime.as_str(),
        report.runtime.engine,
        report.runtime.profile
    )
}

/// Render a status report as TOON (Terse Object Output Notation).
#[must_use]
pub fn render_status_toon(report: &StatusReport) -> String {
    render_toon_from_json(&render_status_json(report))
}

/// Render a doctor report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_doctor_json(report: &DoctorReport) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.overall_healthy);
    b.field_object("data", |d| {
        d.field_str("command", "doctor");
        d.field_str("version", report.version);
        d.field_bool("healthy", report.overall_healthy);
        d.field_array_of_objects("checks", &report.checks, |obj, check| {
            obj.field_str("name", check.name);
            obj.field_str("severity", check.severity.as_str());
            obj.field_str("message", &check.message);
            if let Some(code) = check.error_code {
                obj.field_str("errorCode", code.id);
            }
            if let Some(repair) = check.repair {
                obj.field_str("repair", repair);
            }
        });
    });
    b.finish()
}

/// Render a doctor report as human-readable text.
#[must_use]
pub fn render_doctor_human(report: &DoctorReport) -> String {
    let mut output = String::from("ee doctor\n\n");

    for check in &report.checks {
        let icon = match check.severity {
            crate::core::doctor::CheckSeverity::Ok => "✓",
            crate::core::doctor::CheckSeverity::Warning => "⚠",
            crate::core::doctor::CheckSeverity::Error => "✗",
        };
        output.push_str(&format!("{} {}: {}\n", icon, check.name, check.message));
        if let Some(repair) = check.repair {
            output.push_str(&format!("  repair: {}\n", repair));
        }
    }

    if report.overall_healthy {
        output.push_str("\nAll checks passed.\n");
    } else {
        output.push_str("\nSome checks failed. Run suggested repairs to fix issues.\n");
    }

    output
}

/// Render a doctor report as TOON.
#[must_use]
pub fn render_doctor_toon(report: &DoctorReport) -> String {
    render_toon_from_json(&render_doctor_json(report))
}

fn render_toon_from_json(json: &str) -> String {
    toon::json_to_toon(json).unwrap_or_else(|error| {
        let message = escape_toon_quoted_string(&format!("TOON encoding failed: {error}"));
        format!(
            "schema: {ERROR_SCHEMA_V1}\nerror:\n  code: toon_encoding_failed\n  message: \"{message}\"\n"
        )
    })
}

fn escape_toon_quoted_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c => escaped.push(c),
        }
    }
    escaped
}

/// Legacy placeholder for backwards compatibility during transition.
#[must_use]
pub fn status_response_json() -> String {
    render_status_json(&StatusReport::gather())
}

/// Legacy placeholder for backwards compatibility during transition.
#[must_use]
pub fn human_status() -> String {
    render_status_human(&StatusReport::gather())
}

#[must_use]
pub fn help_text() -> &'static str {
    "ee - durable memory substrate for coding agents\n\nUsage:\n  ee status [--json]\n  ee --version\n  ee --help\n"
}

#[must_use]
pub fn schema_json() -> String {
    format!(
        "{{\"schema\":\"{}\",\"success\":true,\"data\":{{\"command\":\"schema\",\"schemas\":{{\"response\":\"{}\",\"error\":\"{}\"}}}}}}",
        RESPONSE_SCHEMA_V1, RESPONSE_SCHEMA_V1, ERROR_SCHEMA_V1
    )
}

#[must_use]
pub fn help_json() -> String {
    format!(
        "{{\"schema\":\"{}\",\"success\":true,\"data\":{{\"command\":\"help\",\"usage\":\"ee [OPTIONS] [COMMAND]\",\"commands\":[\"status\",\"version\",\"help\"],\"globalOptions\":[\"--json\",\"--robot\",\"--schema\",\"--help-json\",\"--agent-docs\"]}}}}",
        RESPONSE_SCHEMA_V1
    )
}

#[must_use]
pub fn agent_docs() -> String {
    format!(
        "{{\"schema\":\"{}\",\"success\":true,\"data\":{{\"command\":\"agent-docs\",\"description\":\"Durable, local-first, explainable memory for coding agents.\",\"primaryWorkflow\":\"ee context \\\"<task>\\\" --workspace . --max-tokens 4000 --json\",\"coreCommands\":[\"init\",\"remember\",\"search\",\"context\",\"why\",\"status\"]}}}}",
        RESPONSE_SCHEMA_V1
    )
}

#[must_use]
pub fn error_response_json(error: &DomainError) -> String {
    let code = error.code();
    let message = escape_json_string(error.message());
    match error.repair() {
        Some(repair) => {
            let repair = escape_json_string(repair);
            format!(
                "{{\"schema\":\"{schema}\",\"error\":{{\"code\":\"{code}\",\"message\":\"{message}\",\"repair\":\"{repair}\"}}}}",
                schema = ERROR_SCHEMA_V1
            )
        }
        None => {
            format!(
                "{{\"schema\":\"{schema}\",\"error\":{{\"code\":\"{code}\",\"message\":\"{message}\"}}}}",
                schema = ERROR_SCHEMA_V1
            )
        }
    }
}

fn escape_json_string(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if c.is_control() => {
                result.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => result.push(c),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{
        Degradation, DegradationSeverity, JsonBuilder, OutputContext, Renderer, ResponseEnvelope,
        error_response_json, escape_json_string, help_text, human_status, render_status_json,
        render_status_toon, status_response_json,
    };
    use crate::core::status::StatusReport;
    use crate::models::DomainError;

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
        ensure(
            haystack.contains(needle),
            format!("{context}: expected output to contain {needle:?}, got {haystack:?}"),
        )
    }

    fn ensure_starts_with(haystack: &str, prefix: &str, context: &str) -> TestResult {
        ensure(
            haystack.starts_with(prefix),
            format!("{context}: expected output to start with {prefix:?}, got {haystack:?}"),
        )
    }

    #[test]
    fn status_json_has_stable_schema_and_degradation_codes() -> TestResult {
        let json = status_response_json();
        ensure_starts_with(&json, "{\"schema\":\"ee.response.v1\"", "status schema")?;
        ensure_contains(&json, "\"success\":true", "status success flag")?;
        ensure_contains(&json, "\"runtime\":\"ready\"", "status runtime capability")?;
        ensure_contains(&json, "\"engine\":\"asupersync\"", "status runtime engine")?;
        ensure_contains(
            &json,
            "\"profile\":\"current_thread\"",
            "status runtime profile",
        )?;
        ensure_contains(
            &json,
            "\"storage_not_implemented\"",
            "status storage degradation",
        )?;
        ensure_contains(
            &json,
            "\"search_not_implemented\"",
            "status search degradation",
        )
    }

    #[test]
    fn human_status_is_not_json() -> TestResult {
        let status = human_status();
        ensure_starts_with(&status, "ee status", "human status heading")?;
        ensure(!status.starts_with('{'), "human status must not be JSON")
    }

    #[test]
    fn help_mentions_supported_skeleton_commands() -> TestResult {
        let help = help_text();
        ensure_contains(help, "ee status [--json]", "help status command")?;
        ensure_contains(help, "ee --version", "help version command")
    }

    #[test]
    fn error_json_has_stable_schema_and_code() -> TestResult {
        let error = DomainError::Usage {
            message: "unrecognized subcommand 'foo'".to_string(),
            repair: Some("ee --help".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "error schema")?;
        ensure_contains(&json, "\"code\":\"usage\"", "error code")?;
        ensure_contains(
            &json,
            "\"message\":\"unrecognized subcommand 'foo'\"",
            "error message",
        )?;
        ensure_contains(&json, "\"repair\":\"ee --help\"", "error repair")
    }

    #[test]
    fn error_json_without_repair_omits_field() -> TestResult {
        let error = DomainError::Storage {
            message: "Database locked".to_string(),
            repair: None,
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "error schema")?;
        ensure_contains(&json, "\"code\":\"storage\"", "error code")?;
        ensure(!json.contains("repair"), "repair field should be absent")
    }

    #[test]
    fn escape_json_handles_special_chars() -> TestResult {
        let escaped = escape_json_string("line1\nline2\ttab\"quote\\backslash");
        ensure_contains(&escaped, "\\n", "newline escape")?;
        ensure_contains(&escaped, "\\t", "tab escape")?;
        ensure_contains(&escaped, "\\\"", "quote escape")?;
        ensure_contains(&escaped, "\\\\", "backslash escape")
    }

    #[test]
    fn json_builder_constructs_simple_object() -> TestResult {
        let mut b = JsonBuilder::new();
        b.field_str("name", "test");
        b.field_bool("active", true);
        b.field_u32("count", 42);
        let json = b.finish();
        ensure_contains(&json, "\"name\":\"test\"", "string field")?;
        ensure_contains(&json, "\"active\":true", "bool field")?;
        ensure_contains(&json, "\"count\":42", "u32 field")?;
        ensure(
            json.starts_with('{') && json.ends_with('}'),
            "valid JSON object",
        )
    }

    #[test]
    fn json_builder_escapes_string_values() -> TestResult {
        let mut b = JsonBuilder::new();
        b.field_str("message", "line1\nline2");
        let json = b.finish();
        ensure_contains(&json, "\"message\":\"line1\\nline2\"", "escaped newline")
    }

    #[test]
    fn json_builder_supports_nested_objects() -> TestResult {
        let mut b = JsonBuilder::new();
        b.field_str("schema", "test.v1");
        b.field_object("data", |obj| {
            obj.field_str("inner", "value");
        });
        let json = b.finish();
        ensure_contains(&json, "\"schema\":\"test.v1\"", "outer field")?;
        ensure_contains(&json, "\"data\":{\"inner\":\"value\"}", "nested object")
    }

    #[test]
    fn json_builder_supports_array_of_objects() -> TestResult {
        let items = vec![("a", 1u32), ("b", 2u32)];
        let mut b = JsonBuilder::new();
        b.field_array_of_objects("items", &items, |obj, (name, val)| {
            obj.field_str("name", name);
            obj.field_u32("value", *val);
        });
        let json = b.finish();
        ensure_contains(&json, "\"items\":[", "array start")?;
        ensure_contains(&json, "{\"name\":\"a\",\"value\":1}", "first item")?;
        ensure_contains(&json, "{\"name\":\"b\",\"value\":2}", "second item")
    }

    #[test]
    fn json_builder_raw_field_allows_prebuilt_json() -> TestResult {
        let mut b = JsonBuilder::new();
        b.field_raw("config", "[1,2,3]");
        let json = b.finish();
        ensure_contains(&json, "\"config\":[1,2,3]", "raw json array")
    }

    #[test]
    fn renderer_wire_names_are_stable() -> TestResult {
        ensure_equal(&Renderer::Human.as_str(), &"human", "human")?;
        ensure_equal(&Renderer::Json.as_str(), &"json", "json")?;
        ensure_equal(&Renderer::Toon.as_str(), &"toon", "toon")?;
        ensure_equal(&Renderer::Jsonl.as_str(), &"jsonl", "jsonl")?;
        ensure_equal(&Renderer::Compact.as_str(), &"compact", "compact")?;
        ensure_equal(&Renderer::Hook.as_str(), &"hook", "hook")
    }

    fn ensure_equal<T: std::fmt::Debug + PartialEq>(
        actual: &T,
        expected: &T,
        ctx: &str,
    ) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn renderer_machine_readable_classification() -> TestResult {
        ensure(
            !Renderer::Human.is_machine_readable(),
            "human is not machine",
        )?;
        ensure(!Renderer::Toon.is_machine_readable(), "toon is not machine")?;
        ensure(Renderer::Json.is_machine_readable(), "json is machine")?;
        ensure(Renderer::Jsonl.is_machine_readable(), "jsonl is machine")?;
        ensure(
            Renderer::Compact.is_machine_readable(),
            "compact is machine",
        )?;
        ensure(Renderer::Hook.is_machine_readable(), "hook is machine")
    }

    #[test]
    fn output_context_json_flag_forces_json() -> TestResult {
        let ctx = OutputContext::detect_with_hints(true, false, None);
        ensure_equal(&ctx.renderer, &Renderer::Json, "json flag")
    }

    #[test]
    fn output_context_robot_flag_forces_json() -> TestResult {
        let ctx = OutputContext::detect_with_hints(false, true, None);
        ensure_equal(&ctx.renderer, &Renderer::Json, "robot flag")
    }

    #[test]
    fn output_context_format_override_takes_precedence() -> TestResult {
        let ctx = OutputContext::detect_with_hints(true, true, Some(Renderer::Toon));
        ensure_equal(&ctx.renderer, &Renderer::Toon, "format override")
    }

    #[test]
    fn response_envelope_success_has_stable_schema() -> TestResult {
        let json = ResponseEnvelope::success()
            .data(|d| {
                d.field_str("command", "test");
            })
            .finish();
        ensure_starts_with(&json, "{\"schema\":\"ee.response.v1\"", "schema")?;
        ensure_contains(&json, "\"success\":true", "success flag")?;
        ensure_contains(&json, "\"data\":{\"command\":\"test\"}", "data object")
    }

    #[test]
    fn response_envelope_failure_has_success_false() -> TestResult {
        let json = ResponseEnvelope::failure()
            .data_raw("{\"error\":\"something\"}")
            .finish();
        ensure_contains(&json, "\"success\":false", "failure flag")?;
        ensure_contains(&json, "\"data\":{\"error\":\"something\"}", "data raw")
    }

    #[test]
    fn response_envelope_degraded_array() -> TestResult {
        let degradations = vec![("code1", "message1")];
        let json = ResponseEnvelope::success()
            .data(|d| {
                d.field_str("status", "ok");
            })
            .degraded_array(&degradations, |obj, (code, msg)| {
                obj.field_str("code", code);
                obj.field_str("message", msg);
            })
            .finish();
        ensure_contains(&json, "\"degraded\":[{", "degraded array start")?;
        ensure_contains(&json, "\"code\":\"code1\"", "degradation code")
    }

    #[test]
    fn degradation_severity_strings_are_stable() -> TestResult {
        ensure_equal(&DegradationSeverity::Low.as_str(), &"low", "low")?;
        ensure_equal(&DegradationSeverity::Medium.as_str(), &"medium", "medium")?;
        ensure_equal(&DegradationSeverity::High.as_str(), &"high", "high")
    }

    #[test]
    fn degradation_to_json_has_stable_structure() -> TestResult {
        let d = Degradation::new(
            "storage_stale",
            DegradationSeverity::Medium,
            "Storage index is stale.",
            "ee index rebuild",
        );
        let json = d.to_json();
        ensure_contains(&json, "\"code\":\"storage_stale\"", "code field")?;
        ensure_contains(&json, "\"severity\":\"medium\"", "severity field")?;
        ensure_contains(
            &json,
            "\"message\":\"Storage index is stale.\"",
            "message field",
        )?;
        ensure_contains(&json, "\"repair\":\"ee index rebuild\"", "repair field")
    }

    // ========================================================================
    // Error JSON Schema Tests (EE-015)
    //
    // These tests verify the ee.error.v1 JSON schema contract for all
    // DomainError variants. Each error type must produce valid JSON with:
    // - schema: "ee.error.v1"
    // - error.code: stable string matching the error variant
    // - error.message: human-readable description
    // - error.repair: optional remediation command (present when provided)
    // ========================================================================

    #[test]
    fn error_schema_usage_has_stable_structure() -> TestResult {
        let error = DomainError::Usage {
            message: "Unknown command 'xyz'.".to_string(),
            repair: Some("ee --help".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"usage\"", "code")?;
        ensure_contains(&json, "\"message\":\"Unknown command 'xyz'.\"", "message")?;
        ensure_contains(&json, "\"repair\":\"ee --help\"", "repair")
    }

    #[test]
    fn error_schema_configuration_has_stable_structure() -> TestResult {
        let error = DomainError::Configuration {
            message: "Invalid config file format.".to_string(),
            repair: Some("ee config validate".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"configuration\"", "code")?;
        ensure_contains(
            &json,
            "\"message\":\"Invalid config file format.\"",
            "message",
        )?;
        ensure_contains(&json, "\"repair\":\"ee config validate\"", "repair")
    }

    #[test]
    fn error_schema_storage_has_stable_structure() -> TestResult {
        let error = DomainError::Storage {
            message: "Database file corrupted.".to_string(),
            repair: Some("ee db repair".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"storage\"", "code")?;
        ensure_contains(&json, "\"message\":\"Database file corrupted.\"", "message")?;
        ensure_contains(&json, "\"repair\":\"ee db repair\"", "repair")
    }

    #[test]
    fn error_schema_search_index_has_stable_structure() -> TestResult {
        let error = DomainError::SearchIndex {
            message: "Index is stale (generation 9, database generation 12).".to_string(),
            repair: Some("ee index rebuild".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"search_index\"", "code")?;
        ensure_contains(&json, "generation 9", "message contains details")?;
        ensure_contains(&json, "\"repair\":\"ee index rebuild\"", "repair")
    }

    #[test]
    fn error_schema_import_has_stable_structure() -> TestResult {
        let error = DomainError::Import {
            message: "CASS session file not found.".to_string(),
            repair: Some("ee import --dry-run".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"import\"", "code")?;
        ensure_contains(
            &json,
            "\"message\":\"CASS session file not found.\"",
            "message",
        )?;
        ensure_contains(&json, "\"repair\":\"ee import --dry-run\"", "repair")
    }

    #[test]
    fn error_schema_unsatisfied_degraded_mode_has_stable_structure() -> TestResult {
        let error = DomainError::UnsatisfiedDegradedMode {
            message: "Semantic search unavailable and --require-semantic was set.".to_string(),
            repair: Some("ee search --lexical-only".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"unsatisfied_degraded_mode\"", "code")?;
        ensure_contains(&json, "--require-semantic", "message contains flag")?;
        ensure_contains(&json, "\"repair\":\"ee search --lexical-only\"", "repair")
    }

    #[test]
    fn error_schema_policy_denied_has_stable_structure() -> TestResult {
        let error = DomainError::PolicyDenied {
            message: "Redaction policy prevents this operation.".to_string(),
            repair: Some("ee policy review".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"policy_denied\"", "code")?;
        ensure_contains(
            &json,
            "\"message\":\"Redaction policy prevents this operation.\"",
            "message",
        )?;
        ensure_contains(&json, "\"repair\":\"ee policy review\"", "repair")
    }

    #[test]
    fn error_schema_migration_required_has_stable_structure() -> TestResult {
        let error = DomainError::MigrationRequired {
            message: "Database schema version 3 requires migration to version 5.".to_string(),
            repair: Some("ee db migrate".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"migration_required\"", "code")?;
        ensure_contains(&json, "version 3", "message contains version")?;
        ensure_contains(&json, "\"repair\":\"ee db migrate\"", "repair")
    }

    #[test]
    fn error_schema_all_codes_are_covered() -> TestResult {
        // Ensure we have tests for all 8 error types
        let codes = [
            "usage",
            "configuration",
            "storage",
            "search_index",
            "import",
            "unsatisfied_degraded_mode",
            "policy_denied",
            "migration_required",
        ];

        // Verify each code produces valid JSON
        let errors = [
            DomainError::Usage {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::Configuration {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::Storage {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::SearchIndex {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::Import {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::UnsatisfiedDegradedMode {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::PolicyDenied {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::MigrationRequired {
                message: "test".to_string(),
                repair: None,
            },
        ];

        for (error, expected_code) in errors.iter().zip(codes.iter()) {
            let json = error_response_json(error);
            ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", expected_code)?;
            ensure_contains(
                &json,
                &format!("\"code\":\"{expected_code}\""),
                expected_code,
            )?;
        }
        Ok(())
    }

    #[test]
    fn error_schema_without_repair_omits_field() -> TestResult {
        // Verify that when repair is None, the field is absent (not null)
        for error in [
            DomainError::Usage {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::Storage {
                message: "test".to_string(),
                repair: None,
            },
        ] {
            let json = error_response_json(&error);
            ensure(
                !json.contains("repair"),
                format!("{}: repair field should be absent when None", error.code()),
            )?;
        }
        Ok(())
    }

    // ========================================================================
    // TOON Output Tests (EE-036)
    //
    // TOON is rendered from the canonical JSON envelope through /dp/toon_rust.
    // These tests prove the public renderer is valid TOON and semantically
    // equivalent to the JSON status output.
    // ========================================================================

    #[test]
    fn toon_status_has_required_fields() -> TestResult {
        let report = StatusReport::gather();
        let toon = render_status_toon(&report);
        ensure_contains(&toon, "schema: ee.response.v1", "toon schema")?;
        ensure_contains(&toon, "success: true", "toon success")?;
        ensure_contains(&toon, "command: status", "toon command")?;
        ensure_contains(&toon, "capabilities:", "toon capabilities section")?;
        ensure_contains(&toon, "runtime:", "toon runtime section")?;
        ensure_contains(&toon, "engine: asupersync", "toon engine")
    }

    #[test]
    fn toon_status_has_degradation_details() -> TestResult {
        let report = StatusReport::gather();
        let toon = render_status_toon(&report);
        ensure_contains(
            &toon,
            "degraded[2]{code,severity,message,repair}:",
            "degradation section",
        )?;
        ensure_contains(&toon, "storage_not_implemented", "storage degradation code")?;
        ensure_contains(&toon, "search_not_implemented", "search degradation code")
    }

    #[test]
    fn json_toon_parity_status_decodes_to_same_json() -> TestResult {
        let report = StatusReport::gather();
        let json = render_status_json(&report);
        let toon = render_status_toon(&report);

        let expected_json = serde_json::from_str::<serde_json::Value>(&json)
            .map_err(|error| format!("status JSON should parse: {error}"))?;
        let expected = serde_json::Value::from(toon::JsonValue::from(expected_json));
        let decoded = toon::try_decode(&toon, None)
            .map_err(|error| format!("status TOON should decode: {error}"))?;
        let actual = serde_json::Value::from(decoded);

        ensure_equal(&actual, &expected, "decoded TOON matches status JSON")
    }

    #[test]
    fn invalid_json_to_toon_returns_stable_error() -> TestResult {
        let toon = super::render_toon_from_json("{not valid json");
        ensure_contains(&toon, "schema: ee.error.v1", "error schema")?;
        ensure_contains(&toon, "code: toon_encoding_failed", "error code")
    }

    const TOON_STATUS_GOLDEN: &str = include_str!("../../tests/fixtures/golden/toon/status.golden");

    #[test]
    fn toon_status_matches_golden() -> TestResult {
        let report = StatusReport::gather();
        let actual = render_status_toon(&report);

        // Normalize both for comparison (trim trailing whitespace)
        let actual_lines: Vec<&str> = actual.lines().collect();
        let golden_lines: Vec<&str> = TOON_STATUS_GOLDEN.lines().collect();

        if actual_lines.len() != golden_lines.len() {
            return Err(format!(
                "line count mismatch: actual={} golden={}",
                actual_lines.len(),
                golden_lines.len()
            ));
        }

        for (i, (actual_line, golden_line)) in
            actual_lines.iter().zip(golden_lines.iter()).enumerate()
        {
            if actual_line.trim_end() != golden_line.trim_end() {
                return Err(format!(
                    "line {} mismatch:\n  actual:  {:?}\n  golden:  {:?}",
                    i + 1,
                    actual_line,
                    golden_line
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn golden_error_fixtures_are_valid_json() -> TestResult {
        let fixtures = [
            include_str!("../../tests/fixtures/golden/error/usage.golden"),
            include_str!("../../tests/fixtures/golden/error/configuration.golden"),
            include_str!("../../tests/fixtures/golden/error/storage.golden"),
            include_str!("../../tests/fixtures/golden/error/search_index.golden"),
            include_str!("../../tests/fixtures/golden/error/import.golden"),
            include_str!("../../tests/fixtures/golden/error/policy_denied.golden"),
            include_str!("../../tests/fixtures/golden/error/migration_required.golden"),
            include_str!("../../tests/fixtures/golden/error/unsatisfied_degraded_mode.golden"),
            include_str!("../../tests/fixtures/golden/error/no_repair.golden"),
        ];

        for (i, fixture) in fixtures.iter().enumerate() {
            let parsed: Result<serde_json::Value, _> = serde_json::from_str(fixture);
            if let Err(e) = parsed {
                return Err(format!("error fixture {} is not valid JSON: {e}", i));
            }
            let value = parsed.unwrap();
            if value.get("schema") != Some(&serde_json::Value::String("ee.error.v1".to_string())) {
                return Err(format!("error fixture {} missing schema", i));
            }
        }
        Ok(())
    }

    #[test]
    fn golden_status_fixtures_are_valid_json() -> TestResult {
        let fixtures = [
            include_str!("../../tests/fixtures/golden/status/status_healthy.golden"),
            include_str!("../../tests/fixtures/golden/status/status_degraded.golden"),
        ];

        for (i, fixture) in fixtures.iter().enumerate() {
            let parsed: Result<serde_json::Value, _> = serde_json::from_str(fixture);
            if let Err(e) = parsed {
                return Err(format!("status fixture {} is not valid JSON: {e}", i));
            }
            let value = parsed.unwrap();
            if value.get("schema") != Some(&serde_json::Value::String("ee.response.v1".to_string()))
            {
                return Err(format!("status fixture {} missing schema", i));
            }
        }
        Ok(())
    }

    #[test]
    fn golden_version_fixture_is_valid_json() -> TestResult {
        let fixture = include_str!("../../tests/fixtures/golden/version/version.golden");
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(fixture);
        if let Err(e) = parsed {
            return Err(format!("version fixture is not valid JSON: {e}"));
        }
        let value = parsed.unwrap();
        if value.get("schema") != Some(&serde_json::Value::String("ee.response.v1".to_string())) {
            return Err("version fixture missing schema".to_string());
        }
        Ok(())
    }

    #[test]
    fn golden_human_fixtures_have_expected_structure() -> TestResult {
        let error_fixture =
            include_str!("../../tests/fixtures/golden/human/error_with_repair.golden");
        ensure_starts_with(error_fixture, "error:", "human error starts with 'error:'")?;
        ensure_contains(error_fixture, "Next:", "human error has Next section")?;

        let success_fixture =
            include_str!("../../tests/fixtures/golden/human/success_with_summary.golden");
        ensure_contains(success_fixture, "Next:", "human success has Next section")?;
        ensure(!success_fixture.starts_with('{'), "human output is not JSON")
    }
}
