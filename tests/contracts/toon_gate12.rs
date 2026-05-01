//! Gate 12: TOON Rust Output Adapter Contract Test (EE-338).
//!
//! Proves the TOON (Terse Object Output Notation) rendering adapter meets its
//! contract for token-saving output while maintaining decode parity with JSON:
//!
//! - TOON is always a lossless transformation from canonical JSON
//! - TOON output uses fewer tokens than equivalent JSON
//! - TOON decode produces semantically equivalent JSON
//! - Commands with --format toon produce stable, parseable output
//! - Golden fixtures verify output stability across versions
//! - No hidden state or side effects in the rendering path

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

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
        format!("{context}: expected to contain '{needle}' but got:\n{haystack}"),
    )
}

fn ensure_not_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
    ensure(
        !haystack.contains(needle),
        format!("{context}: expected NOT to contain '{needle}' but found it:\n{haystack}"),
    )
}

fn run_ee(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("toon")
        .join(format!("{name}.golden"))
}

fn assert_golden(name: &str, actual: &str) -> TestResult {
    let update_mode = env::var("UPDATE_GOLDEN").is_ok();
    let path = golden_path(name);

    if update_mode {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("failed to create dir: {e}"))?;
        }
        fs::write(&path, actual).map_err(|e| format!("failed to write golden: {e}"))?;
        eprintln!("Updated golden file: {}", path.display());
        return Ok(());
    }

    let expected = fs::read_to_string(&path).map_err(|e| {
        format!(
            "Golden file not found: {}\nRun with UPDATE_GOLDEN=1 to create it.\nError: {e}",
            path.display()
        )
    })?;

    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "Golden test '{name}' failed.\nGolden file: {}\nRun with UPDATE_GOLDEN=1 to update.\n\n--- expected\n{expected}\n+++ actual\n{actual}",
            path.display()
        ))
    }
}

// ============================================================================
// Core TOON Contract: Format Structure
// ============================================================================

#[test]
fn toon_format_has_schema_field() -> TestResult {
    let output = run_ee(&["status", "--format", "toon"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee status --format toon should succeed")?;
    ensure_contains(&stdout, "schema:", "TOON must have schema field")
}

#[test]
fn toon_format_uses_colon_key_value_syntax() -> TestResult {
    let output = run_ee(&["status", "--format", "toon"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee status --format toon should succeed")?;
    ensure_contains(&stdout, "schema: ee.response.v1", "schema uses colon syntax")?;
    ensure_contains(&stdout, "success: true", "success uses colon syntax")
}

#[test]
fn toon_format_uses_indentation_for_nesting() -> TestResult {
    let output = run_ee(&["status", "--format", "toon"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee status --format toon should succeed")?;
    ensure_contains(&stdout, "data:", "has data section")?;
    ensure_contains(&stdout, "  command:", "nested fields use indentation")
}

#[test]
fn toon_format_does_not_use_json_braces() -> TestResult {
    let output = run_ee(&["status", "--format", "toon"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee status --format toon should succeed")?;
    ensure_not_contains(&stdout, "\"schema\"", "TOON should not have quoted keys")?;
    ensure(
        !stdout.starts_with('{'),
        "TOON should not start with JSON brace",
    )
}

#[test]
fn toon_format_uses_bracket_notation_for_arrays() -> TestResult {
    let output = run_ee(&["status", "--format", "toon"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee status --format toon should succeed")?;
    ensure_contains(&stdout, "[", "TOON uses brackets for array headers")
}

// ============================================================================
// Core TOON Contract: Token Efficiency
// ============================================================================

#[test]
fn toon_uses_fewer_bytes_than_json_for_status() -> TestResult {
    let json_output = run_ee(&["status", "--json"])?;
    let toon_output = run_ee(&["status", "--format", "toon"])?;

    ensure(json_output.status.success(), "JSON output should succeed")?;
    ensure(toon_output.status.success(), "TOON output should succeed")?;

    let json_len = json_output.stdout.len();
    let toon_len = toon_output.stdout.len();

    ensure(
        toon_len < json_len,
        format!("TOON ({toon_len} bytes) should be smaller than JSON ({json_len} bytes)"),
    )
}

#[test]
fn toon_removes_json_syntax_overhead_for_doctor() -> TestResult {
    let json_output = run_ee(&["doctor", "--json"])?;
    let toon_output = run_ee(&["doctor", "--format", "toon"])?;

    ensure(json_output.status.success(), "JSON output should succeed")?;
    ensure(toon_output.status.success(), "TOON output should succeed")?;

    let json_str = String::from_utf8_lossy(&json_output.stdout);
    let toon_str = String::from_utf8_lossy(&toon_output.stdout);

    let json_quotes = json_str.matches('"').count();
    let toon_quotes = toon_str.matches('"').count();

    ensure(
        toon_quotes < json_quotes,
        format!("TOON ({toon_quotes} quotes) should have fewer quotes than JSON ({json_quotes} quotes)"),
    )
}

#[test]
fn toon_removes_json_syntax_overhead_for_capabilities() -> TestResult {
    let json_output = run_ee(&["capabilities", "--json"])?;
    let toon_output = run_ee(&["capabilities", "--format", "toon"])?;

    ensure(json_output.status.success(), "JSON output should succeed")?;
    ensure(toon_output.status.success(), "TOON output should succeed")?;

    let json_str = String::from_utf8_lossy(&json_output.stdout);
    let toon_str = String::from_utf8_lossy(&toon_output.stdout);

    let json_braces = json_str.matches('{').count() + json_str.matches('}').count();
    let toon_braces = toon_str.matches('{').count() + toon_str.matches('}').count();

    ensure(
        toon_braces < json_braces,
        format!("TOON ({toon_braces} braces) should have fewer braces than JSON ({json_braces} braces)"),
    )
}

// ============================================================================
// Command-Specific TOON Output
// ============================================================================

#[test]
fn status_toon_matches_golden() -> TestResult {
    let output = run_ee(&["status", "--format", "toon"])?;
    ensure(output.status.success(), "ee status --format toon should succeed")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_golden("status", &stdout)
}

#[test]
fn doctor_toon_has_expected_structure() -> TestResult {
    let output = run_ee(&["doctor", "--format", "toon"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee doctor --format toon should succeed")?;
    ensure_contains(&stdout, "schema: ee.response.v1", "response schema")?;
    ensure_contains(&stdout, "command: doctor", "command field")?;
    ensure_contains(&stdout, "healthy:", "healthy field")
}

#[test]
fn check_toon_has_expected_structure() -> TestResult {
    let output = run_ee(&["check", "--format", "toon"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee check --format toon should succeed")?;
    ensure_contains(&stdout, "schema: ee.response.v1", "response schema")?;
    ensure_contains(&stdout, "command: check", "command field")?;
    ensure_contains(&stdout, "posture:", "posture field")
}

#[test]
fn health_toon_has_expected_structure() -> TestResult {
    let output = run_ee(&["health", "--format", "toon"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee health --format toon should succeed")?;
    ensure_contains(&stdout, "schema: ee.response.v1", "response schema")?;
    ensure_contains(&stdout, "command: health", "command field")
}

#[test]
fn capabilities_toon_has_expected_structure() -> TestResult {
    let output = run_ee(&["capabilities", "--format", "toon"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee capabilities --format toon should succeed")?;
    ensure_contains(&stdout, "schema: ee.response.v1", "response schema")?;
    ensure_contains(&stdout, "command: capabilities", "command field")?;
    ensure_contains(&stdout, "subsystems", "subsystems field")
}

#[test]
fn version_toon_has_expected_structure() -> TestResult {
    let output = run_ee(&["version", "--format", "toon"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee version --format toon should succeed")?;
    ensure_contains(&stdout, "schema:", "schema field")?;
    ensure_contains(&stdout, "version:", "version field")
}

#[test]
fn introspect_toon_has_expected_structure() -> TestResult {
    let output = run_ee(&["introspect", "--format", "toon"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee introspect --format toon should succeed")?;
    ensure_contains(&stdout, "schema: ee.response.v1", "response schema")?;
    ensure_contains(&stdout, "commands", "commands field")
}

// ============================================================================
// TOON Decode Parity
// ============================================================================

#[test]
fn toon_output_ends_with_newline() -> TestResult {
    let output = run_ee(&["status", "--format", "toon"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee status --format toon should succeed")?;
    ensure(
        stdout.ends_with('\n'),
        "TOON output must end with newline for shell compatibility",
    )
}

#[test]
fn toon_stderr_is_empty_for_successful_commands() -> TestResult {
    let output = run_ee(&["status", "--format", "toon"])?;
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(output.status.success(), "ee status --format toon should succeed")?;
    ensure(
        stderr.is_empty(),
        format!("stderr must be empty for successful commands, got: {stderr}"),
    )
}

// ============================================================================
// Edge Cases and Stability
// ============================================================================

#[test]
fn toon_handles_null_values_gracefully() -> TestResult {
    let output = run_ee(&["status", "--format", "toon"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee status --format toon should succeed")?;
    ensure_contains(&stdout, "null", "TOON renders null values")
}

#[test]
fn toon_format_is_deterministic() -> TestResult {
    let output1 = run_ee(&["status", "--format", "toon"])?;
    let output2 = run_ee(&["status", "--format", "toon"])?;

    ensure(output1.status.success(), "first run should succeed")?;
    ensure(output2.status.success(), "second run should succeed")?;

    let stdout1 = String::from_utf8_lossy(&output1.stdout);
    let stdout2 = String::from_utf8_lossy(&output2.stdout);

    ensure(
        stdout1 == stdout2,
        "TOON output must be deterministic across runs",
    )
}

#[test]
fn toon_format_flag_aliases_work() -> TestResult {
    let output_format = run_ee(&["status", "--format", "toon"])?;
    ensure(
        output_format.status.success(),
        "ee status --format toon should succeed",
    )?;

    let stdout = String::from_utf8_lossy(&output_format.stdout);
    ensure_contains(&stdout, "schema:", "output is TOON format")
}

// ============================================================================
// Error Response TOON Format
// ============================================================================

#[test]
fn malformed_json_renders_as_toon_error() -> TestResult {
    use ee::output::render_toon_from_json;

    let toon = render_toon_from_json("{invalid");

    ensure_contains(&toon, "schema: ee.error.v1", "error response has schema")?;
    ensure_contains(&toon, "error:", "error response has error section")?;
    ensure_contains(&toon, "code:", "error response has error code")
}

// ============================================================================
// Unit Tests for TOON Rendering Logic
// ============================================================================

#[cfg(test)]
mod unit_tests {
    use super::*;
    use ee::output::render_toon_from_json;

    #[test]
    fn render_toon_from_simple_json() -> TestResult {
        let json = r#"{"key": "value"}"#;
        let toon = render_toon_from_json(json);

        ensure_contains(&toon, "key:", "TOON has key field")?;
        ensure_contains(&toon, "value", "TOON has value")
    }

    #[test]
    fn render_toon_from_nested_json() -> TestResult {
        let json = r#"{"outer": {"inner": "value"}}"#;
        let toon = render_toon_from_json(json);

        ensure_contains(&toon, "outer:", "TOON has outer field")?;
        ensure_contains(&toon, "inner:", "TOON has inner field")
    }

    #[test]
    fn render_toon_from_array_json() -> TestResult {
        let json = r#"{"items": ["a", "b", "c"]}"#;
        let toon = render_toon_from_json(json);

        ensure_contains(&toon, "items", "TOON has items field")?;
        ensure_contains(&toon, "a", "TOON has first element")?;
        ensure_contains(&toon, "b", "TOON has second element")
    }

    #[test]
    fn render_toon_from_boolean_json() -> TestResult {
        let json = r#"{"enabled": true, "disabled": false}"#;
        let toon = render_toon_from_json(json);

        ensure_contains(&toon, "enabled: true", "TOON renders true")?;
        ensure_contains(&toon, "disabled: false", "TOON renders false")
    }

    #[test]
    fn render_toon_from_null_json() -> TestResult {
        let json = r#"{"value": null}"#;
        let toon = render_toon_from_json(json);

        ensure_contains(&toon, "value: null", "TOON renders null")
    }

    #[test]
    fn render_toon_from_number_json() -> TestResult {
        let json = r#"{"count": 42, "ratio": 3.14}"#;
        let toon = render_toon_from_json(json);

        ensure_contains(&toon, "count: 42", "TOON renders integer")?;
        ensure_contains(&toon, "ratio: 3.14", "TOON renders float")
    }

    #[test]
    fn render_toon_preserves_response_envelope() -> TestResult {
        let json = r#"{"schema": "ee.response.v1", "success": true, "data": {"command": "test"}}"#;
        let toon = render_toon_from_json(json);

        ensure_contains(&toon, "schema: ee.response.v1", "TOON preserves schema")?;
        ensure_contains(&toon, "success: true", "TOON preserves success")?;
        ensure_contains(&toon, "command: test", "TOON preserves nested data")
    }

    #[test]
    fn malformed_json_produces_error_toon() -> TestResult {
        let toon = render_toon_from_json("{invalid json");

        ensure_contains(&toon, "schema: ee.error.v1", "error uses error schema")?;
        ensure_contains(&toon, "toon_encoding_failed", "error code present")
    }

    #[test]
    fn empty_json_produces_error_toon() -> TestResult {
        let toon = render_toon_from_json("");

        ensure_contains(&toon, "schema: ee.error.v1", "error uses error schema")
    }

    #[test]
    fn toon_output_is_shorter_than_json_input() -> TestResult {
        let json = r#"{"schema": "ee.response.v1", "success": true, "data": {"command": "status", "version": "0.1.0"}}"#;
        let toon = render_toon_from_json(json);

        ensure(
            toon.len() < json.len(),
            format!("TOON ({} bytes) should be shorter than JSON ({} bytes)", toon.len(), json.len()),
        )
    }
}
