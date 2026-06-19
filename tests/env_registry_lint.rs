#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};

type TestResult = Result<(), String>;

const ALLOWED_RAW_ENV_FILE: &str = "src/config/env_registry.rs";
const RAW_RUNTIME_EE_ENV_PATTERNS: [&str; 4] = [
    r#"std::env::var("EE_"#,
    r#"env::var("EE_"#,
    r#"std::env::var_os("EE_"#,
    r#"env::var_os("EE_"#,
];
const RAW_BUILD_TIME_EE_ENV_PATTERNS: [&str; 2] = [r#"option_env!("EE_"#, r#"env!("EE_"#];
const ALLOWED_BUILD_TIME_EE_ENV_VARS: [&str; 3] =
    ["EE_BUILD_TARGET", "EE_RELEASE_CHANNEL", "EE_TRACE_BEAD_ID"];
const README_WORKSPACE_HYGIENE_ENV_VARS: [&str; 4] = [
    "EE_WORKSPACE_HYGIENE_ALWAYS_REVIEW_PATTERNS",
    "EE_WORKSPACE_HYGIENE_GENERATED_PATTERNS",
    "EE_WORKSPACE_HYGIENE_LOCAL_MACHINE_PATTERNS",
    "EE_WORKSPACE_HYGIENE_SCRATCH_PATTERNS",
];
const README_OBSOLETE_WORKSPACE_HYGIENE_ENV_VARS: [&str; 2] = [
    "EE_WORKSPACE_HYGIENE_SECRET_PATTERNS",
    "EE_WORKSPACE_HYGIENE_IGNORE_PATTERNS",
];

fn collect_rs_files(root: &Path, out: &mut Vec<PathBuf>) -> TestResult {
    for entry in fs::read_dir(root).map_err(|error| format!("read {}: {error}", root.display()))? {
        let entry =
            entry.map_err(|error| format!("read entry under {}: {error}", root.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    Ok(())
}

fn compact_code_line(line: &str) -> String {
    line.split("//")
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn line_has_raw_runtime_ee_env_read(line: &str) -> bool {
    let compact = compact_code_line(line);
    RAW_RUNTIME_EE_ENV_PATTERNS
        .iter()
        .any(|pattern| compact.contains(pattern))
}

fn build_time_ee_var_name(line: &str) -> Option<String> {
    let compact = compact_code_line(line);
    for pattern in RAW_BUILD_TIME_EE_ENV_PATTERNS {
        let Some(pattern_start) = compact.find(pattern) else {
            continue;
        };
        let name_start = pattern_start + pattern.len() - "EE_".len();
        let rest = &compact[name_start..];
        let name_end = rest.find('"')?;
        return Some(rest[..name_end].to_owned());
    }
    None
}

#[test]
fn production_code_uses_env_registry_for_ee_vars() -> TestResult {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("src");
    let mut files = Vec::new();
    collect_rs_files(&src, &mut files)?;

    let mut violations = Vec::new();
    for file in files {
        let relative = file
            .strip_prefix(&root)
            .map_err(|error| format!("strip {}: {error}", file.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        if relative == ALLOWED_RAW_ENV_FILE {
            continue;
        }

        let content = fs::read_to_string(&file)
            .map_err(|error| format!("read {}: {error}", file.display()))?;
        for (line_index, line) in content.lines().enumerate() {
            if line_has_raw_runtime_ee_env_read(line) {
                violations.push(format!("{}:{}", relative, line_index + 1));
            }
            if let Some(var_name) = build_time_ee_var_name(line)
                && !ALLOWED_BUILD_TIME_EE_ENV_VARS.contains(&var_name.as_str())
            {
                violations.push(format!(
                    "{}:{} unregistered build-time env {var_name}",
                    relative,
                    line_index + 1
                ));
            }
        }
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "raw EE_* env reads must use config::env_registry: {}",
            violations.join(", ")
        ))
    }
}

#[test]
fn allowed_build_time_ee_vars_are_documented() -> TestResult {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let docs_path = root.join("docs/env_vars.md");
    let docs = fs::read_to_string(&docs_path)
        .map_err(|error| format!("read {}: {error}", docs_path.display()))?;
    for var_name in ALLOWED_BUILD_TIME_EE_ENV_VARS {
        let needle = format!("`{var_name}`");
        if !docs.contains(&needle) {
            return Err(format!(
                "allowed build-time env var {var_name} must be documented in docs/env_vars.md"
            ));
        }
    }
    Ok(())
}

#[test]
fn raw_runtime_detector_handles_whitespace_and_ignores_line_comments() {
    assert!(line_has_raw_runtime_ee_env_read(
        r#"std::env::var ( "EE_UNREGISTERED" )"#
    ));
    assert!(line_has_raw_runtime_ee_env_read(
        r#"env :: var_os ( "EE_UNREGISTERED" )"#
    ));
    assert!(!line_has_raw_runtime_ee_env_read(
        r#"// std::env::var("EE_DOCUMENTED_ONLY")"#
    ));
}

#[test]
fn build_time_detector_handles_whitespace_and_ignores_line_comments() {
    assert_eq!(
        build_time_ee_var_name(r#"option_env ! ( "EE_TRACE_BEAD_ID" )"#).as_deref(),
        Some("EE_TRACE_BEAD_ID")
    );
    assert_eq!(
        build_time_ee_var_name(r#"env ! ( "EE_NOT_ALLOWED" )"#).as_deref(),
        Some("EE_NOT_ALLOWED")
    );
    assert_eq!(
        build_time_ee_var_name(r#"// option_env!("EE_COMMENT_ONLY")"#),
        None
    );
}

#[test]
fn readme_workspace_hygiene_env_overlays_match_registry() -> TestResult {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let readme_path = root.join("README.md");
    let readme = fs::read_to_string(&readme_path)
        .map_err(|error| format!("read {}: {error}", readme_path.display()))?;
    let row = readme
        .lines()
        .find(|line| line.contains("workspace hygiene classifier overlays"))
        .ok_or_else(|| "README is missing workspace hygiene classifier overlay row".to_string())?;

    for var_name in README_WORKSPACE_HYGIENE_ENV_VARS {
        if !row.contains(var_name) {
            return Err(format!(
                "README workspace hygiene overlay row must document registered env var {var_name}"
            ));
        }
    }
    for var_name in README_OBSOLETE_WORKSPACE_HYGIENE_ENV_VARS {
        if row.contains(var_name) {
            return Err(format!(
                "README workspace hygiene overlay row still documents obsolete env var {var_name}"
            ));
        }
    }
    Ok(())
}
