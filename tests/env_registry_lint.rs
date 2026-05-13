#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};

type TestResult = Result<(), String>;

const ALLOWED_RAW_ENV_FILE: &str = "src/config/env_registry.rs";
const RAW_EE_ENV_PATTERNS: [&str; 4] = [
    r#"std::env::var("EE_"#,
    r#"env::var("EE_"#,
    r#"std::env::var_os("EE_"#,
    r#"env::var_os("EE_"#,
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
            if RAW_EE_ENV_PATTERNS
                .iter()
                .any(|pattern| line.contains(pattern))
            {
                violations.push(format!("{}:{}", relative, line_index + 1));
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
