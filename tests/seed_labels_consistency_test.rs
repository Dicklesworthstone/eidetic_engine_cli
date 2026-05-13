//! Contract tests for the N4 deterministic child-seed label registry.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use ee::core::determinism::{
    SEED_LABEL_REGISTRY, SEED_LABELS, assert_label_registered, is_label_registered,
    seed_label_definition,
};

type TestResult<T = ()> = Result<T, String>;

const DOC: &str = include_str!("../docs/determinism_seed_labels.md");
const DOC_PATH: &str = "docs/determinism_seed_labels.md";

#[test]
fn code_registry_and_docs_have_identical_seed_labels() {
    let code_labels = sorted_strings(SEED_LABELS.iter().copied());
    let doc_labels = sorted_strings(seed_labels_from_doc(DOC).into_iter());

    assert_eq!(code_labels, doc_labels);
}

#[test]
fn seed_label_registry_has_complete_metadata() {
    assert_eq!(SEED_LABELS.len(), SEED_LABEL_REGISTRY.len());

    for label in SEED_LABELS {
        let definition = seed_label_definition(label)
            .unwrap_or_else(|| panic!("missing registry metadata for `{label}`"));
        assert_eq!(definition.label, *label);
        assert!(
            !definition.producer_call_site.trim().is_empty(),
            "producer call site missing for `{label}`"
        );
        assert!(
            !definition.consumer.trim().is_empty(),
            "consumer missing for `{label}`"
        );
        assert!(DOC.contains(definition.producer_call_site));
        assert!(DOC.contains(definition.consumer));
    }
}

#[test]
fn label_assertion_helper_accepts_registered_labels() {
    for label in SEED_LABELS {
        assert!(is_label_registered(label));
        assert_label_registered(label);
    }

    assert!(!is_label_registered("unregistered.label"));
}

#[test]
fn production_child_calls_use_registered_literal_labels() -> TestResult {
    let mut violations = Vec::new();

    for path in rust_files_under(Path::new("src"))? {
        let content =
            fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        for (line_index, line) in content.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with("//!")
            {
                continue;
            }

            for label in child_call_literals(line) {
                if !is_label_registered(&label) {
                    violations.push(format!("{}:{} `{label}`", path.display(), line_index + 1));
                }
            }
        }
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "unregistered Deterministic::child labels:\n{}",
            violations.join("\n")
        ))
    }
}

fn seed_labels_from_doc(doc: &str) -> Vec<&str> {
    doc.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if !trimmed.starts_with("| `") {
                return None;
            }
            let rest = trimmed.strip_prefix("| `")?;
            let (label, _) = rest.split_once('`')?;
            Some(label)
        })
        .collect()
}

fn sorted_strings<'a>(labels: impl Iterator<Item = &'a str>) -> Vec<String> {
    labels
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn child_call_literals(line: &str) -> Vec<String> {
    let mut labels = Vec::new();
    let mut rest = line;

    while let Some(start) = rest.find(".child(\"") {
        let after_start = &rest[start + ".child(\"".len()..];
        let Some(end) = after_start.find('"') else {
            break;
        };
        labels.push(after_start[..end].to_owned());
        rest = &after_start[end + 1..];
    }

    labels
}

fn rust_files_under(root: &Path) -> TestResult<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_rust_files(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_rust_files(path: &Path, files: &mut Vec<PathBuf>) -> TestResult {
    let entries = fs::read_dir(path).map_err(|error| format!("{}: {error}", path.display()))?;

    for entry in entries {
        let entry = entry.map_err(|error| format!("{}: {error}", path.display()))?;
        let entry_path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("{}: {error}", entry_path.display()))?;
        if file_type.is_dir() {
            collect_rust_files(&entry_path, files)?;
        } else if entry_path
            .extension()
            .is_some_and(|extension| extension == "rs")
        {
            files.push(entry_path);
        }
    }

    Ok(())
}

#[test]
fn doc_path_is_stable_for_bead_fj481() {
    assert_eq!(DOC_PATH, "docs/determinism_seed_labels.md");
}
