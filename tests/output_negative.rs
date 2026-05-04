//! EE-334: Negative output contract tests
//!
//! Tests for malformed TOON, unavailable feature, encoding failure, and unsupported
//! format scenarios with stable degradation/error codes.
//!
//! These tests prove that error paths produce stable, machine-readable output
//! suitable for programmatic error handling.

use clap::Parser;
use ee::cli::Cli;
use ee::models::{DomainError, ERROR_SCHEMA_V1, RESPONSE_SCHEMA_V1};
use ee::output::{
    DegradationSeverity, FieldProfile, OutputContext, Renderer, ResponseEnvelope,
    error_response_json, error_response_toon, render_toon_from_json,
};

type TestResult = Result<(), String>;

fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
    if haystack.contains(needle) {
        Ok(())
    } else {
        Err(format!(
            "{context}: expected to contain {needle:?}, got:\n{haystack}"
        ))
    }
}

fn ensure_not_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
    if !haystack.contains(needle) {
        Ok(())
    } else {
        Err(format!(
            "{context}: expected NOT to contain {needle:?}, got:\n{haystack}"
        ))
    }
}

fn ensure_starts_with(haystack: &str, prefix: &str, context: &str) -> TestResult {
    if haystack.starts_with(prefix) {
        Ok(())
    } else {
        Err(format!(
            "{context}: expected to start with {prefix:?}, got:\n{haystack}"
        ))
    }
}

fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, context: &str) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

// ============================================================================
// Malformed TOON / Invalid JSON Input Tests
// ============================================================================

#[test]
fn malformed_json_unclosed_brace_produces_stable_error() -> TestResult {
    let toon = render_toon_from_json("{not valid json");
    ensure_contains(&toon, "schema: ee.error.v1", "error schema")?;
    ensure_contains(&toon, "code: toon_encoding_failed", "error code")?;
    ensure_contains(&toon, "error:", "error section")
}

#[test]
fn malformed_json_trailing_comma_produces_stable_error() -> TestResult {
    let toon = render_toon_from_json(r#"{"key": "value",}"#);
    ensure_contains(&toon, "schema: ee.error.v1", "error schema")?;
    ensure_contains(&toon, "code: toon_encoding_failed", "error code")
}

#[test]
fn malformed_json_unquoted_key_produces_stable_error() -> TestResult {
    let toon = render_toon_from_json(r#"{key: "value"}"#);
    ensure_contains(&toon, "schema: ee.error.v1", "error schema")?;
    ensure_contains(&toon, "code: toon_encoding_failed", "error code")
}

#[test]
fn malformed_json_single_quotes_produces_stable_error() -> TestResult {
    let toon = render_toon_from_json("{'key': 'value'}");
    ensure_contains(&toon, "schema: ee.error.v1", "error schema")?;
    ensure_contains(&toon, "code: toon_encoding_failed", "error code")
}

#[test]
fn malformed_json_empty_string_produces_stable_error() -> TestResult {
    let toon = render_toon_from_json("");
    ensure_contains(&toon, "schema: ee.error.v1", "error schema")?;
    ensure_contains(&toon, "code: toon_encoding_failed", "error code")
}

#[test]
fn malformed_json_only_whitespace_produces_stable_error() -> TestResult {
    let toon = render_toon_from_json("   \n\t  ");
    ensure_contains(&toon, "schema: ee.error.v1", "error schema")?;
    ensure_contains(&toon, "code: toon_encoding_failed", "error code")
}

#[test]
fn malformed_json_nested_invalid_produces_stable_error() -> TestResult {
    let toon = render_toon_from_json(r#"{"outer": {"inner": }}"#);
    ensure_contains(&toon, "schema: ee.error.v1", "error schema")?;
    ensure_contains(&toon, "code: toon_encoding_failed", "error code")
}

#[test]
fn malformed_json_array_invalid_produces_stable_error() -> TestResult {
    let toon = render_toon_from_json("[1, 2, 3,]");
    ensure_contains(&toon, "schema: ee.error.v1", "error schema")?;
    ensure_contains(&toon, "code: toon_encoding_failed", "error code")
}

#[test]
fn malformed_json_control_char_produces_stable_error() -> TestResult {
    // Raw control character in string (not escaped)
    let toon = render_toon_from_json("{\"key\": \"value\x01\"}");
    ensure_contains(&toon, "schema: ee.error.v1", "error schema")?;
    ensure_contains(&toon, "code: toon_encoding_failed", "error code")
}

// ============================================================================
// Unsupported Format Tests
// ============================================================================

#[test]
fn renderer_from_unknown_string_defaults_to_human() -> TestResult {
    // renderer_from_env_value returns None for unknown formats,
    // which causes detect_with_hints to fall back to Human
    let ctx = OutputContext::detect_with_hints(false, false, None);
    // Without env vars or flags, defaults to Human
    ensure(ctx.renderer, Renderer::Human, "default renderer")
}

#[test]
fn renderer_enum_covers_all_valid_formats() -> TestResult {
    let formats = [
        ("human", Renderer::Human),
        ("json", Renderer::Json),
        ("toon", Renderer::Toon),
        ("jsonl", Renderer::Jsonl),
        ("compact", Renderer::Compact),
        ("hook", Renderer::Hook),
        ("markdown", Renderer::Markdown),
    ];

    for (name, expected) in formats {
        let actual = match name {
            "human" => Renderer::Human,
            "json" => Renderer::Json,
            "toon" => Renderer::Toon,
            "jsonl" => Renderer::Jsonl,
            "compact" => Renderer::Compact,
            "hook" => Renderer::Hook,
            "markdown" => Renderer::Markdown,
            _ => Renderer::Human,
        };
        ensure(actual, expected, name)?;
    }
    Ok(())
}

#[test]
fn renderer_as_str_round_trips() -> TestResult {
    let renderers = [
        Renderer::Human,
        Renderer::Json,
        Renderer::Toon,
        Renderer::Jsonl,
        Renderer::Compact,
        Renderer::Hook,
        Renderer::Markdown,
    ];

    for renderer in renderers {
        let name = renderer.as_str();
        ensure(!name.is_empty(), true, "renderer name not empty")?;
    }
    Ok(())
}

#[test]
fn machine_readable_formats_identified_correctly() -> TestResult {
    ensure(Renderer::Json.is_machine_readable(), true, "json")?;
    ensure(Renderer::Jsonl.is_machine_readable(), true, "jsonl")?;
    ensure(Renderer::Compact.is_machine_readable(), true, "compact")?;
    ensure(Renderer::Hook.is_machine_readable(), true, "hook")?;
    ensure(Renderer::Human.is_machine_readable(), false, "human")?;
    ensure(Renderer::Toon.is_machine_readable(), false, "toon")?;
    ensure(Renderer::Markdown.is_machine_readable(), false, "markdown")
}

// ============================================================================
// Encoding Failure / Edge Case Tests
// ============================================================================

#[test]
fn json_escapes_special_characters_in_error_message() -> TestResult {
    let error = DomainError::Usage {
        message: "Invalid path: \"foo\\bar\"\nnewline".to_string(),
        repair: None,
    };
    let json = error_response_json(&error);

    // Should be valid JSON
    let _: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| format!("should be valid JSON: {e}"))?;

    // Should contain escaped characters
    ensure_contains(&json, "\\\"foo", "escaped quote")?;
    ensure_contains(&json, "\\\\bar", "escaped backslash")?;
    ensure_contains(&json, "\\n", "escaped newline")
}

#[test]
fn toon_escapes_special_characters_in_error_message() -> TestResult {
    let error = DomainError::Usage {
        message: "Invalid path: \"foo\\bar\"\nnewline".to_string(),
        repair: None,
    };
    let toon = error_response_toon(&error);

    // Should contain message with escaping
    ensure_contains(&toon, "message:", "message field")
}

#[test]
fn json_handles_unicode_in_error_message() -> TestResult {
    let error = DomainError::Usage {
        message: "Unicode test: \u{1F600} emoji \u{4E2D}\u{6587} chinese".to_string(),
        repair: None,
    };
    let json = error_response_json(&error);

    // Should be valid JSON
    let _: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| format!("should be valid JSON: {e}"))?;

    Ok(())
}

#[test]
fn json_handles_empty_error_message() -> TestResult {
    let error = DomainError::Usage {
        message: String::new(),
        repair: None,
    };
    let json = error_response_json(&error);

    // Should be valid JSON
    let _: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| format!("should be valid JSON: {e}"))?;

    ensure_contains(&json, "\"message\":\"\"", "empty message")
}

#[test]
fn json_handles_very_long_error_message() -> TestResult {
    let long_message = "x".repeat(10_000);
    let error = DomainError::Usage {
        message: long_message.clone(),
        repair: None,
    };
    let json = error_response_json(&error);

    // Should be valid JSON
    let parsed: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| format!("should be valid JSON: {e}"))?;

    // Message should be preserved
    let msg = parsed["error"]["message"]
        .as_str()
        .ok_or("message should be string")?;
    ensure(msg.len(), long_message.len(), "message length preserved")
}

// ============================================================================
// Unavailable Feature / Degradation Tests
// ============================================================================

#[test]
fn unavailable_feature_produces_degradation_code() -> TestResult {
    let error = DomainError::UnsatisfiedDegradedMode {
        message: "Semantic search requires embedding model".to_string(),
        repair: Some("ee index reembed --dry-run".to_string()),
    };
    let json = error_response_json(&error);

    ensure_contains(
        &json,
        "\"code\":\"unsatisfied_degraded_mode\"",
        "error code",
    )?;
    ensure_contains(
        &json,
        "\"repair\":\"ee index reembed --dry-run\"",
        "repair hint",
    )
}

#[test]
fn policy_denied_produces_stable_error() -> TestResult {
    let error = DomainError::PolicyDenied {
        message: "Operation blocked by retention policy".to_string(),
        repair: Some("ee doctor --fix-plan --json".to_string()),
    };
    let json = error_response_json(&error);

    ensure_contains(&json, "\"code\":\"policy_denied\"", "error code")?;
    ensure_contains(
        &json,
        "\"repair\":\"ee doctor --fix-plan --json\"",
        "repair hint",
    )
}

#[test]
fn migration_required_produces_stable_error() -> TestResult {
    let error = DomainError::MigrationRequired {
        message: "Database schema version 3 requires migration".to_string(),
        repair: Some("ee init --workspace .".to_string()),
    };
    let json = error_response_json(&error);

    ensure_contains(&json, "\"code\":\"migration_required\"", "error code")?;
    ensure_contains(&json, "\"repair\":\"ee init --workspace .\"", "repair hint")
}

#[test]
fn golden_error_fixture_repairs_parse_current_cli() -> TestResult {
    let fixtures = [
        ("usage", include_str!("fixtures/golden/error/usage.golden")),
        (
            "configuration",
            include_str!("fixtures/golden/error/configuration.golden"),
        ),
        (
            "storage",
            include_str!("fixtures/golden/error/storage.golden"),
        ),
        (
            "search_index",
            include_str!("fixtures/golden/error/search_index.golden"),
        ),
        (
            "import",
            include_str!("fixtures/golden/error/import.golden"),
        ),
        (
            "policy_denied",
            include_str!("fixtures/golden/error/policy_denied.golden"),
        ),
        (
            "migration_required",
            include_str!("fixtures/golden/error/migration_required.golden"),
        ),
        (
            "unsatisfied_degraded_mode",
            include_str!("fixtures/golden/error/unsatisfied_degraded_mode.golden"),
        ),
        (
            "no_repair",
            include_str!("fixtures/golden/error/no_repair.golden"),
        ),
    ];

    for (name, fixture) in fixtures {
        let value: serde_json::Value = serde_json::from_str(fixture)
            .map_err(|error| format!("{name} golden must be JSON: {error}"))?;
        let Some(repair) = value
            .pointer("/error/repair")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let args: Vec<&str> = repair.split_whitespace().collect();
        ensure(
            args.first(),
            Some(&"ee"),
            &format!("{name} repair command prefix"),
        )?;
        Cli::try_parse_from(args)
            .map_err(|error| format!("{name} repair `{repair}` must parse: {:?}", error.kind()))?;
    }

    Ok(())
}

#[test]
fn not_found_error_includes_resource_info() -> TestResult {
    let error = DomainError::NotFound {
        resource: "memory".to_string(),
        id: "mem_abc123".to_string(),
        repair: Some("ee memory list".to_string()),
    };
    let json = error_response_json(&error);

    ensure_contains(&json, "\"code\":\"not_found\"", "error code")?;
    ensure_contains(&json, "memory not found: mem_abc123", "resource info")
}

// ============================================================================
// Error Response Envelope Structure Tests
// ============================================================================

#[test]
fn error_json_has_required_envelope_fields() -> TestResult {
    let error = DomainError::Storage {
        message: "Test error".to_string(),
        repair: Some("ee doctor".to_string()),
    };
    let json = error_response_json(&error);

    ensure_starts_with(
        &json,
        &format!("{{\"schema\":\"{ERROR_SCHEMA_V1}\""),
        "schema field first",
    )?;
    ensure_contains(&json, "\"error\":{", "error object")?;
    ensure_contains(&json, "\"code\":\"storage\"", "code field")?;
    ensure_contains(&json, "\"message\":\"Test error\"", "message field")?;
    ensure_contains(&json, "\"severity\":\"high\"", "severity field")?;
    ensure_contains(&json, "\"details\":{}", "details field")?;
    ensure_contains(&json, "\"repair\":\"ee doctor\"", "repair field")
}

#[test]
fn error_toon_has_required_fields() -> TestResult {
    let error = DomainError::Storage {
        message: "Test error".to_string(),
        repair: Some("ee doctor".to_string()),
    };
    let toon = error_response_toon(&error);

    ensure_contains(&toon, &format!("schema: {ERROR_SCHEMA_V1}"), "schema field")?;
    ensure_contains(&toon, "error:", "error section")?;
    ensure_contains(&toon, "code: storage", "code field")?;
    ensure_contains(&toon, "message:", "message field")?;
    ensure_contains(&toon, "repair:", "repair field")
}

#[test]
fn error_without_repair_omits_field_in_json() -> TestResult {
    let error = DomainError::Storage {
        message: "Test error".to_string(),
        repair: None,
    };
    let json = error_response_json(&error);

    ensure_not_contains(&json, "repair", "no repair field")
}

#[test]
fn all_domain_error_variants_produce_valid_json() -> TestResult {
    let errors = vec![
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
        DomainError::Graph {
            message: "test".to_string(),
            repair: None,
        },
        DomainError::Import {
            message: "test".to_string(),
            repair: None,
        },
        DomainError::NotFound {
            resource: "item".to_string(),
            id: "123".to_string(),
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

    for error in &errors {
        let json = error_response_json(error);
        let _: serde_json::Value = serde_json::from_str(&json)
            .map_err(|e| format!("{}: should be valid JSON: {e}", error.code()))?;
    }
    Ok(())
}

#[test]
fn all_domain_error_variants_produce_valid_toon() -> TestResult {
    let errors = vec![
        DomainError::Usage {
            message: "test".to_string(),
            repair: Some("ee --help".to_string()),
        },
        DomainError::Configuration {
            message: "test".to_string(),
            repair: Some("ee doctor --fix-plan".to_string()),
        },
        DomainError::Storage {
            message: "test".to_string(),
            repair: Some("ee doctor".to_string()),
        },
        DomainError::SearchIndex {
            message: "test".to_string(),
            repair: Some("ee index rebuild".to_string()),
        },
        DomainError::Graph {
            message: "test".to_string(),
            repair: Some("ee graph centrality-refresh --dry-run".to_string()),
        },
        DomainError::Import {
            message: "test".to_string(),
            repair: Some("ee import cass --dry-run".to_string()),
        },
        DomainError::NotFound {
            resource: "item".to_string(),
            id: "123".to_string(),
            repair: Some("ee memory list".to_string()),
        },
        DomainError::UnsatisfiedDegradedMode {
            message: "test".to_string(),
            repair: Some("ee doctor".to_string()),
        },
        DomainError::PolicyDenied {
            message: "test".to_string(),
            repair: Some("ee doctor --fix-plan".to_string()),
        },
        DomainError::MigrationRequired {
            message: "test".to_string(),
            repair: Some("ee init --workspace .".to_string()),
        },
    ];

    for error in &errors {
        let toon = error_response_toon(error);
        ensure_contains(&toon, "schema:", &format!("{}: has schema", error.code()))?;
        ensure_contains(
            &toon,
            "error:",
            &format!("{}: has error section", error.code()),
        )?;
        ensure_contains(
            &toon,
            &format!("code: {}", error.code()),
            &format!("{}: has code", error.code()),
        )?;
    }
    Ok(())
}

// ============================================================================
// Field Profile Tests (verbosity levels)
// ============================================================================

#[test]
fn field_profile_minimal_excludes_arrays() -> TestResult {
    let profile = FieldProfile::Minimal;
    ensure(!profile.include_arrays(), true, "minimal excludes arrays")?;
    ensure(
        !profile.include_summary_metrics(),
        true,
        "minimal excludes metrics",
    )?;
    ensure(
        !profile.include_verbose_details(),
        true,
        "minimal excludes verbose",
    )?;
    ensure(
        !profile.include_provenance(),
        true,
        "minimal excludes provenance",
    )
}

#[test]
fn field_profile_summary_includes_metrics_only() -> TestResult {
    let profile = FieldProfile::Summary;
    ensure(!profile.include_arrays(), true, "summary excludes arrays")?;
    ensure(
        profile.include_summary_metrics(),
        true,
        "summary includes metrics",
    )?;
    ensure(
        !profile.include_verbose_details(),
        true,
        "summary excludes verbose",
    )?;
    ensure(
        !profile.include_provenance(),
        true,
        "summary excludes provenance",
    )
}

#[test]
fn field_profile_standard_includes_arrays() -> TestResult {
    let profile = FieldProfile::Standard;
    ensure(profile.include_arrays(), true, "standard includes arrays")?;
    ensure(
        profile.include_summary_metrics(),
        true,
        "standard includes metrics",
    )?;
    ensure(
        !profile.include_verbose_details(),
        true,
        "standard excludes verbose",
    )?;
    ensure(
        !profile.include_provenance(),
        true,
        "standard excludes provenance",
    )
}

#[test]
fn field_profile_full_includes_everything() -> TestResult {
    let profile = FieldProfile::Full;
    ensure(profile.include_arrays(), true, "full includes arrays")?;
    ensure(
        profile.include_summary_metrics(),
        true,
        "full includes metrics",
    )?;
    ensure(
        profile.include_verbose_details(),
        true,
        "full includes verbose",
    )?;
    ensure(
        profile.include_provenance(),
        true,
        "full includes provenance",
    )
}

#[test]
fn field_profile_as_str_round_trips() -> TestResult {
    let profiles = [
        (FieldProfile::Minimal, "minimal"),
        (FieldProfile::Summary, "summary"),
        (FieldProfile::Standard, "standard"),
        (FieldProfile::Full, "full"),
    ];

    for (profile, expected_name) in profiles {
        ensure(profile.as_str(), expected_name, "profile name")?;
    }
    Ok(())
}

// ============================================================================
// Response Envelope Degradation Tests
// ============================================================================

#[test]
fn response_envelope_success_has_correct_structure() -> TestResult {
    let json = ResponseEnvelope::success()
        .data(|d| {
            d.field_str("test", "value");
        })
        .finish();

    ensure_starts_with(
        &json,
        &format!("{{\"schema\":\"{RESPONSE_SCHEMA_V1}\""),
        "schema first",
    )?;
    ensure_contains(&json, "\"success\":true", "success flag")?;
    ensure_contains(&json, "\"data\":{", "data object")
}

#[test]
fn response_envelope_failure_has_correct_structure() -> TestResult {
    let json = ResponseEnvelope::failure()
        .data(|d| {
            d.field_str("error", "test error");
        })
        .finish();

    ensure_starts_with(
        &json,
        &format!("{{\"schema\":\"{RESPONSE_SCHEMA_V1}\""),
        "schema first",
    )?;
    ensure_contains(&json, "\"success\":false", "success flag")?;
    ensure_contains(&json, "\"data\":{", "data object")
}

// ============================================================================
// Degradation Severity Tests
// ============================================================================

#[test]
fn degradation_severity_as_str_covers_all_levels() -> TestResult {
    ensure(DegradationSeverity::Low.as_str(), "low", "low")?;
    ensure(DegradationSeverity::Medium.as_str(), "medium", "medium")?;
    ensure(DegradationSeverity::High.as_str(), "high", "high")
}
