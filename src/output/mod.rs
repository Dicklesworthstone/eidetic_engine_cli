use std::env;
use std::io::IsTerminal;

use crate::core;
use crate::models::{CapabilityStatus, DomainError, ERROR_SCHEMA_V1, RESPONSE_SCHEMA_V1};

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

#[must_use]
pub fn status_response_json() -> String {
    let storage = CapabilityStatus::Unimplemented.as_str();
    let search = CapabilityStatus::Unimplemented.as_str();
    let runtime = CapabilityStatus::Ready.as_str();
    let runtime_status = core::runtime_status();

    format!(
        "{{\"schema\":\"{schema}\",\"success\":true,\"data\":{{\"command\":\"status\",\"version\":\"{version}\",\"capabilities\":{{\"runtime\":\"{runtime}\",\"storage\":\"{storage}\",\"search\":\"{search}\"}},\"runtime\":{{\"engine\":\"{engine}\",\"profile\":\"{profile}\",\"workerThreads\":{worker_threads},\"asyncBoundary\":\"{async_boundary}\"}},\"degraded\":[{{\"code\":\"storage_not_implemented\",\"severity\":\"medium\",\"message\":\"Storage subsystem is not wired yet.\",\"repair\":\"Implement EE-040 through EE-044.\"}},{{\"code\":\"search_not_implemented\",\"severity\":\"medium\",\"message\":\"Search subsystem is not wired yet.\",\"repair\":\"Implement EE-120 and dependent search beads.\"}}]}}}}",
        schema = RESPONSE_SCHEMA_V1,
        version = env!("CARGO_PKG_VERSION"),
        runtime = runtime,
        storage = storage,
        search = search,
        engine = runtime_status.engine,
        profile = runtime_status.profile.as_str(),
        worker_threads = runtime_status.worker_threads(),
        async_boundary = runtime_status.async_boundary
    )
}

#[must_use]
pub fn human_status() -> &'static str {
    "ee status\n\nstorage: unimplemented\nsearch: unimplemented\nruntime: ready (asupersync current_thread)\n\nNext:\n  ee status --json\n"
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
        error_response_json, escape_json_string, help_text, human_status, status_response_json,
    };
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
        ensure_starts_with(status, "ee status", "human status heading")?;
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
}
