use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn taxonomy_section_codes(doc: &str, heading: &str) -> Result<BTreeSet<String>, String> {
    let section = doc
        .split(heading)
        .nth(1)
        .and_then(|section| section.split("### `").next())
        .ok_or_else(|| format!("taxonomy section {heading:?} not found"))?;
    let codes = section
        .lines()
        .filter_map(|line| {
            line.split('`').nth(1).filter(|token| {
                token
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                    && token.len() > 3
            })
        })
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();

    if codes.is_empty() {
        Err(format!("no codes found in taxonomy section {heading:?}"))
    } else {
        Ok(codes)
    }
}

fn collect_json_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(root).map_err(|error| format!("read {}: {error}", root.display()))? {
        let entry = entry.map_err(|error| format!("read {} entry: {error}", root.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_json_files(&path, files)?;
        } else if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension, "golden" | "json"))
        {
            files.push(path);
        }
    }
    Ok(())
}

fn collect_degraded_code_violations(
    value: &serde_json::Value,
    build_time_codes: &BTreeSet<String>,
    path: &Path,
    pointer: &str,
    violations: &mut Vec<String>,
) {
    match value {
        serde_json::Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_degraded_code_violations(
                    item,
                    build_time_codes,
                    path,
                    &format!("{pointer}/{index}"),
                    violations,
                );
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::Array(degraded)) = map.get("degraded") {
                for (index, entry) in degraded.iter().enumerate() {
                    if let Some(code) = entry.get("code").and_then(serde_json::Value::as_str)
                        && build_time_codes.contains(code)
                    {
                        violations.push(format!(
                            "{}:{pointer}/degraded/{index} emits build-time code {code}",
                            path.display()
                        ));
                    }
                }
            }
            for (key, child) in map {
                collect_degraded_code_violations(
                    child,
                    build_time_codes,
                    path,
                    &format!("{pointer}/{key}"),
                    violations,
                );
            }
        }
        _ => {}
    }
}

#[test]
fn degraded_arrays_do_not_emit_build_time_codes() -> TestResult {
    let repo = repo_root();
    let taxonomy = fs::read_to_string(repo.join("docs/degraded_code_taxonomy.md"))
        .map_err(|error| format!("read degraded taxonomy: {error}"))?;
    let build_time_codes = taxonomy_section_codes(&taxonomy, "### `build_time`")?;

    let mut files = Vec::new();
    collect_json_files(&repo.join("tests/fixtures/golden"), &mut files)?;

    let mut violations = Vec::new();
    let mut parsed_json_count = 0usize;
    for path in files {
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        parsed_json_count += 1;
        collect_degraded_code_violations(&value, &build_time_codes, &path, "", &mut violations);
    }

    if parsed_json_count == 0 {
        Err("no JSON golden fixtures were parsed".to_owned())
    } else if violations.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "build-time degraded codes appeared in response degraded[] arrays:\n{}",
            violations.join("\n")
        ))
    }
}
