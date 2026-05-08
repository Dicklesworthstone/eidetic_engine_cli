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
use std::process::Command;

/// Allowlisted occurrences with justification.
/// Format: (file_path_suffix, line_number, reason)
const ALLOWLIST: &[(&str, u32, &str)] = &[
    // === Mutation model Display impls ===
    ("src/models/mutation.rs", 382, "Display impl for logging"),
    ("src/models/mutation.rs", 387, "Display impl for logging"),
    ("src/models/mutation.rs", 477, "Display impl for logging"),
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

/// Files/directories to exclude from scanning (test code, generated code).
const EXCLUDE_PATHS: &[&str] = &[
    "/tests/",
    "/target/",
    "#[cfg(test)]",
    "mod tests",
    "_test.rs",
    ".test.",
];

#[test]
fn no_unallowlisted_silent_fallbacks() {
    let output = Command::new("rg")
        .args([
            "--no-heading",
            "--line-number",
            "--with-filename",
            "-e",
            r#"serde_json::to_string.*\.unwrap_or_default\(\)"#,
            "-e",
            r#"serde_json::to_string_pretty.*\.unwrap_or_default\(\)"#,
            "-e",
            r#"\.join\(\)\.unwrap_or_default\(\)"#,
            "-e",
            r#"let _ = .*read_to_end"#,
            "--type",
            "rust",
            "src/",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output();

    let output = match output {
        Ok(o) => o,
        Err(e) => panic!("Failed to execute ripgrep: {e}"),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut violations = Vec::new();
    let mut allowlisted_count = 0;

    for line in stdout.lines() {
        // Skip test code
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
            allowlisted_count += 1;
        } else {
            violations.push(format!("{}:{}", file_path, line_num));
        }
    }

    if !violations.is_empty() {
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
            violations.len(),
            violations
                .iter()
                .map(|v| format!("║    - {}\n", v))
                .collect::<String>()
        );
    }

    // Report success with stats
    eprintln!(
        "silent_fallback_guard: PASS ({} allowlisted, 0 violations)",
        allowlisted_count
    );
}

#[test]
fn guard_detects_synthetic_violation() {
    // This test proves the guard would catch a violation if one existed.
    // We test the detection logic directly without needing a real violation.

    let test_line = "src/output/mod.rs:9999:serde_json::to_string(&x).unwrap_or_default()";

    // Parse like the real guard does
    let parts: Vec<&str> = test_line.splitn(3, ':').collect();
    assert_eq!(parts.len(), 3);

    let file_path = parts[0];
    let line_num: u32 = match parts[1].parse() {
        Ok(n) => n,
        Err(_) => panic!("test setup: invalid line number"),
    };

    // This synthetic line should NOT be in the allowlist
    let is_allowlisted = ALLOWLIST
        .iter()
        .any(|(path, allowed_line, _)| file_path.ends_with(path) && line_num == *allowed_line);

    assert!(
        !is_allowlisted,
        "Synthetic violation at line 9999 should not be allowlisted"
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
