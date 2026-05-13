use std::fs;
use std::path::{Path, PathBuf};

type TestResult = Result<(), String>;

const FORBIDDEN: &[&str] = &[
    concat!("ee.error", ".v1"),
    concat!("error_envelope", "_v1"),
    concat!("ErrorEnvelope", "V1"),
];

fn visit_text_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|error| format!("read {}: {error}", dir.display()))? {
        let entry = entry.map_err(|error| format!("read {} entry: {error}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            visit_text_files(&path, files)?;
        } else if matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("json" | "md" | "py" | "rs" | "sh" | "snap")
        ) {
            files.push(path);
        }
    }
    Ok(())
}

#[test]
fn repository_text_contracts_do_not_emit_error_v1() -> TestResult {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dirname in ["src", "tests", "docs", "scripts"] {
        visit_text_files(&root.join(dirname), &mut files)?;
    }

    let mut failures = Vec::new();
    for path in files {
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        for needle in FORBIDDEN {
            if text.contains(needle) {
                failures.push(format!("{} contains {needle}", path.display()));
            }
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}
