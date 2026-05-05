//! EE-okfs5: Adapter business-logic boundary audit.
//!
//! Enforces that MCP and serve modules strictly act as adapters.
//! They must not contain direct business logic, SQL queries, direct ORM usage,
//! or low-level search indexing code. All heavy lifting must be delegated to
//! `src/core/`.

use std::path::Path;

type TestResult = Result<(), String>;

const FORBIDDEN_PATTERNS: &[&str] = &[
    "DbConnection::",
    "frankensqlite",
    "sqlmodel",
    "frankensearch",
    "SELECT ",
    "INSERT INTO ",
    "UPDATE ",
    "DELETE FROM ",
    "ee_schema_migrations",
];

const ADAPTER_PATHS: &[&str] = &["src/mcp.rs", "src/serve.rs", "src/mcp", "src/serve"];

fn scan_path(path: &Path, findings: &mut Vec<(String, usize, String)>) {
    if path.is_dir() {
        scan_directory(path, findings);
    } else if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("rs") {
        scan_file(path, findings);
    }
}

fn scan_directory(dir: &Path, findings: &mut Vec<(String, usize, String)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();

        if path.is_dir() {
            scan_directory(&path, findings);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            scan_file(&path, findings);
        }
    }
}

fn scan_file(path: &Path, findings: &mut Vec<(String, usize, String)>) {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return,
    };

    let path_str = path.display().to_string();

    for (line_num, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with("*") {
            continue;
        }

        for pattern in FORBIDDEN_PATTERNS {
            if line.contains(pattern) {
                findings.push((path_str.clone(), line_num + 1, pattern.to_string()));
            }
        }
    }
}

#[test]
fn adapters_contain_no_business_logic() -> TestResult {
    let root_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut findings = Vec::new();

    for adapter_path in ADAPTER_PATHS {
        scan_path(&root_dir.join(adapter_path), &mut findings);
    }

    if !findings.is_empty() {
        let mut report = String::from(
            "Business logic patterns found in adapter source code (src/mcp.rs, src/serve.rs, src/mcp/, or src/serve/):\n\n\
             The architecture requires adapters to only map I/O. They must not contain direct \
             SQL queries, database connections, or search indexing logic. Delegate to src/core/.\n\n",
        );

        for (path, line, pattern) in &findings {
            report.push_str(&format!("  {path}:{line} - matched: {pattern}\n"));
        }

        return Err(report);
    }

    Ok(())
}
