//! No-silent-fallback conformance guard (EE-sos5.5).
//!
//! Production code that feeds machine-facing data paths must not silently
//! erase failure evidence by defaulting to empty values. This test scans
//! for high-risk patterns and fails if any unallowlisted occurrence is found.
//!
//! ## Policy
//!
//! The following patterns are FORBIDDEN in machine-facing data paths:
//!
//! 1. `serde_json::to_string(...).unwrap_or_default()` — serialization failure
//!    produces empty string instead of error
//! 2. `thread.join().unwrap_or_default()` — thread panic produces empty result
//! 3. `let _ = ...read_to_end(...)` — I/O error silently discarded
//!
//! ## Allowlist
//!
//! Safe patterns are documented in the ALLOWLIST constant with file path,
//! pattern, and justification. To add a new allowlist entry:
//!
//! 1. Verify the pattern is in a human-display-only or optional-metadata path
//! 2. Add an entry with file:line, pattern, and reason
//! 3. Add a regression test proving the allowlisted path handles failure safely
//!
//! ## Repair Pattern
//!
//! Instead of `serde_json::to_string(x).unwrap_or_default()`, use:
//!
//! ```ignore
//! serde_json::to_string(x).map_err(|e| MyError::Serialization(e))?
//! // or for display-only paths:
//! serde_json::to_string(x).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}")
//! ```

use std::collections::HashSet;
use std::fs;
use std::process::Command;

/// Allowlisted occurrences with justification.
/// Format: (file_path_suffix, line_number, reason)
const ALLOWLIST: &[(&str, u32, &str)] = &[
    // === Existing production inventory entries ===
    (
        "src/cli/mesh.rs",
        769,
        "PENDING-FIX: Mesh response JSON serialization tracked by no-silent-fallback inventory",
    ),
    (
        "src/cli/mod.rs",
        27844,
        "PENDING-FIX: CLI error envelope serialization tracked by no-silent-fallback inventory",
    ),
    (
        "src/cli/mod.rs",
        28682,
        "PENDING-FIX: CLI JSON response serialization tracked by no-silent-fallback inventory",
    ),
    (
        "src/cli/mod.rs",
        29007,
        "PENDING-FIX: CLI JSON response serialization tracked by no-silent-fallback inventory",
    ),
    (
        "src/cli/mod.rs",
        29386,
        "PENDING-FIX: CLI JSON response serialization tracked by no-silent-fallback inventory",
    ),
    (
        "src/cli/mod.rs",
        29709,
        "PENDING-FIX: CLI JSON response serialization tracked by no-silent-fallback inventory",
    ),
    (
        "src/cli/mod.rs",
        29955,
        "PENDING-FIX: CLI JSON response serialization tracked by no-silent-fallback inventory",
    ),
    (
        "src/cli/mod.rs",
        30400,
        "PENDING-FIX: CLI JSON response serialization tracked by no-silent-fallback inventory",
    ),
    (
        "src/cli/mod.rs",
        30486,
        "PENDING-FIX: CLI JSON response serialization tracked by no-silent-fallback inventory",
    ),
    (
        "src/cli/mod.rs",
        30599,
        "PENDING-FIX: CLI JSON response serialization tracked by no-silent-fallback inventory",
    ),
    (
        "src/cli/mod.rs",
        30728,
        "PENDING-FIX: CLI JSON response serialization tracked by no-silent-fallback inventory",
    ),
    (
        "src/core/rehearse.rs",
        1404,
        "PENDING-FIX: Captured stderr thread join tracked by no-silent-fallback inventory",
    ),
    (
        "src/core/verify.rs",
        1588,
        "PENDING-FIX: Captured stdout thread join tracked by no-silent-fallback inventory",
    ),
    (
        "src/core/verify.rs",
        1589,
        "PENDING-FIX: Captured stderr thread join tracked by no-silent-fallback inventory",
    ),
    // === Mutation model Display impls ===
    ("src/models/mutation.rs", 416, "Display impl for logging"),
    ("src/models/mutation.rs", 421, "Display impl for logging"),
    ("src/models/mutation.rs", 511, "Display impl for logging"),
    // === Progress model Display ===
    (
        "src/models/progress.rs",
        146,
        "Display impl for progress updates",
    ),
    // === Hooks installer (pending fix: sos5.3) ===
    (
        "src/hooks/installer.rs",
        164,
        "PENDING-FIX: Hook config serialization - tracked by sos5.3",
    ),
    (
        "src/hooks/installer.rs",
        645,
        "PENDING-FIX: Hook manifest serialization - tracked by sos5.3",
    ),
    // === Output module renderers (pending fix: sos5.3) ===
    (
        "src/output/mod.rs",
        7166,
        "PENDING-FIX: Search report render - tracked by sos5.3",
    ),
    (
        "src/output/mod.rs",
        7208,
        "PENDING-FIX: Search report render - tracked by sos5.3",
    ),
    (
        "src/output/mod.rs",
        7258,
        "PENDING-FIX: Search report render - tracked by sos5.3",
    ),
    (
        "src/output/mod.rs",
        7315,
        "PENDING-FIX: Search report render - tracked by sos5.3",
    ),
    (
        "src/output/mod.rs",
        7361,
        "PENDING-FIX: Search report render - tracked by sos5.3",
    ),
    (
        "src/output/mod.rs",
        7408,
        "PENDING-FIX: Search report render - tracked by sos5.3",
    ),
    (
        "src/output/mod.rs",
        8147,
        "PENDING-FIX: Report render - tracked by sos5.3",
    ),
    (
        "src/output/mod.rs",
        8198,
        "PENDING-FIX: Report render - tracked by sos5.3",
    ),
];

const SCAN_ROOTS: &[&str] = &["src/", "tests/"];

const SILENT_FALLBACK_PATTERNS: &[&str] = &[
    r#"serde_json::to_string.*\.unwrap_or_default\(\)"#,
    r#"serde_json::to_string_pretty.*\.unwrap_or_default\(\)"#,
    r#"\.join\(\)\.unwrap_or_default\(\)"#,
    r#"let _ = .*read_to_end"#,
];

/// Files/directories to exclude from scanning (generated code and guard fixtures).
const EXCLUDE_PATHS: &[&str] = &[
    "/target/",
    "#[cfg(test)]",
    "mod tests",
    "tests/contracts/no_silent_fallback.rs",
    "tests/no_silent_fallback_inventory.rs",
    "tests/silent_fallback_guard.rs",
];

#[derive(Debug, Default, Eq, PartialEq)]
struct ScanClassification {
    violations: Vec<String>,
    allowlisted_count: usize,
}

fn scan_silent_fallbacks() -> String {
    let mut args = vec!["--no-heading", "--line-number", "--with-filename"];
    for pattern in SILENT_FALLBACK_PATTERNS {
        args.push("-e");
        args.push(pattern);
    }
    args.push("--type");
    args.push("rust");
    args.extend(SCAN_ROOTS);

    let output = Command::new("rg")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output();

    let output = match output {
        Ok(o) => o,
        Err(e) => panic!("Failed to execute ripgrep: {e}"),
    };

    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn classify_silent_fallback_scan(stdout: &str) -> ScanClassification {
    let mut result = ScanClassification::default();

    for line in stdout.lines() {
        // Skip generated code and policy fixtures; tests are otherwise scanned.
        if EXCLUDE_PATHS.iter().any(|ex| line.contains(ex)) {
            continue;
        }

        // Parse "file:line:content" format
        let parts: Vec<&str> = line.splitn(3, ':').collect();
        if parts.len() < 2 {
            continue;
        }

        let file_path = parts[0];
        let line_num: u32 = match parts[1].parse() {
            Ok(n) => n,
            Err(_) => continue,
        };

        // Check if allowlisted
        let is_allowlisted = ALLOWLIST
            .iter()
            .any(|(path, allowed_line, _)| file_path.ends_with(path) && line_num == *allowed_line);

        if is_allowlisted {
            result.allowlisted_count += 1;
        } else {
            result
                .violations
                .push(format!("{}:{}", file_path, line_num));
        }
    }

    result
}

#[test]
fn no_unallowlisted_silent_fallbacks() {
    let stdout = scan_silent_fallbacks();
    let classification = classify_silent_fallback_scan(&stdout);

    if !classification.violations.is_empty() {
        panic!(
            "\n\
            ╔══════════════════════════════════════════════════════════════════╗\n\
            ║  NO-SILENT-FALLBACK GUARD FAILED                                 ║\n\
            ╠══════════════════════════════════════════════════════════════════╣\n\
            ║  Found {} unallowlisted silent-fallback pattern(s).              \n\
            ║                                                                  \n\
            ║  VIOLATIONS:                                                     \n\
            {}║                                                                  \n\
            ║  REPAIR OPTIONS:                                                 \n\
            ║  1. Return Result<String, Error> instead of unwrap_or_default    \n\
            ║  2. Use unwrap_or_else with explicit error JSON                  \n\
            ║  3. Add to ALLOWLIST with justification if display-only          \n\
            ║                                                                  \n\
            ║  See docs/silent-fallback-inventory.md for policy details.       \n\
            ╚══════════════════════════════════════════════════════════════════╝\n",
            classification.violations.len(),
            classification
                .violations
                .iter()
                .map(|v| format!("║    - {}\n", v))
                .collect::<String>()
        );
    }

    // Report success with stats
    eprintln!(
        "silent_fallback_guard: PASS ({} allowlisted, 0 violations)",
        classification.allowlisted_count
    );
}

#[test]
fn guard_detects_synthetic_violation() {
    // This test proves the guard would catch a violation if one existed.
    // We test the detection logic directly without needing a real violation.

    let test_line = "src/output/mod.rs:9999:serde_json::to_string(&x).unwrap_or_default()";
    let classification = classify_silent_fallback_scan(test_line);

    assert_eq!(
        classification.violations,
        vec!["src/output/mod.rs:9999"],
        "synthetic violation should not be allowlisted"
    );
}

#[test]
fn guard_detects_synthetic_tests_resident_violation() {
    let fixture = include_str!("fixtures/silent_fallback_guard/tests_scope_violation.fixture");
    let classification = classify_silent_fallback_scan(fixture);

    assert_eq!(
        classification.violations,
        vec!["tests/synthetic_silent_fallback_scope.rs:7"],
        "tests/ resident silent fallback patterns must be visible to the guard"
    );
}

#[test]
fn memory_drift_no_mock_e2e_hashes_have_no_silent_json_fallbacks() {
    let path = "tests/memory_drift_no_mock_e2e.rs";
    let source = fs::read_to_string(format!("{}/{}", env!("CARGO_MANIFEST_DIR"), path))
        .unwrap_or_else(|error| panic!("failed to read {path}: {error}"));
    let violations = source
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            (line.contains("serde_json::to_string") && line.contains("unwrap_or_default()"))
                .then_some(format!("{path}:{}", index + 1))
        })
        .collect::<Vec<_>>();

    assert!(
        violations.is_empty(),
        "memory_drift_no_mock_e2e.rs must fail structurally on JSON serialization errors; found silent fallback(s): {}",
        violations.join(", ")
    );
}

#[test]
fn allowlist_entries_have_justification() {
    for (path, line, reason) in ALLOWLIST {
        assert!(
            !reason.is_empty(),
            "Allowlist entry {}:{} missing justification",
            path,
            line
        );
        assert!(
            reason.len() >= 10,
            "Allowlist entry {}:{} has insufficient justification: '{}'",
            path,
            line,
            reason
        );
    }
}

#[test]
fn allowlist_entries_are_unique() {
    let mut seen = HashSet::new();
    for (path, line, _) in ALLOWLIST {
        let key = format!("{}:{}", path, line);
        assert!(
            seen.insert(key.clone()),
            "Duplicate allowlist entry: {}",
            key
        );
    }
}
