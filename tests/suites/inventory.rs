//! Fail the normal test gate if disabling Cargo autodiscovery hides a root test file.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn suite_modules(source: &str) -> Result<Vec<String>, String> {
    let mut modules = Vec::new();
    let mut lines = source.lines().map(str::trim);
    while let Some(line) = lines.next() {
        if line.is_empty() || line.starts_with("//") || line == "mod inventory;" {
            continue;
        }
        let file = line
            .strip_prefix("#[path = \"../")
            .and_then(|line| line.strip_suffix("\"]"))
            .ok_or_else(|| format!("unexpected suite declaration: {line}"))?;
        // Shared helpers are compiled once by a suite. They do not replace a
        // root test registration, and only direct support/*.rs paths qualify.
        let module_file = file.strip_prefix("support/").unwrap_or(file);
        let name = module_file
            .strip_suffix(".rs")
            .filter(|name| {
                name.chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            })
            .ok_or_else(|| format!("invalid suite module path: {file}"))?;
        let declaration = format!("mod {name};");
        if lines.next() != Some(declaration.as_str()) {
            return Err(format!("{file} must be followed by {declaration}"));
        }
        if module_file == file {
            modules.push(file.to_owned());
        }
    }
    Ok(modules)
}

fn root_files(root: &Path) -> Result<BTreeSet<String>, Box<dyn std::error::Error>> {
    let mut files = BTreeSet::new();
    for entry in root.join("tests").read_dir()? {
        let entry = entry?;
        if entry.file_type()?.is_file() && entry.path().extension().is_some_and(|ext| ext == "rs") {
            files.insert(entry.file_name().into_string().map_err(|name| {
                format!("test file name is not UTF-8: {}", name.to_string_lossy())
            })?);
        }
    }
    Ok(files)
}

fn coverage_errors(files: &BTreeSet<String>, counts: &BTreeMap<String, usize>) -> Vec<String> {
    let mut errors = Vec::new();
    for file in files {
        let count = counts.get(file).copied().unwrap_or(0);
        if count != 1 {
            errors.push(format!(
                "{file}: expected one registered module/target, found {count}"
            ));
        }
    }
    for file in counts.keys() {
        if !files.contains(file) {
            errors.push(format!("registered test file does not exist: {file}"));
        }
    }
    errors
}

#[test]
fn every_root_test_file_is_registered_exactly_once() -> TestResult {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest =
        std::fs::read_to_string(root.join("Cargo.toml"))?.parse::<toml_edit::DocumentMut>()?;
    if manifest["package"]["autotests"].as_bool() != Some(false) {
        return Err("autotests must stay false to avoid hundreds of integration links".into());
    }
    let targets = manifest["test"]
        .as_array_of_tables()
        .ok_or("Cargo.toml is missing explicit integration test targets")?;
    let mut counts = BTreeMap::new();
    let mut registered_suites = BTreeSet::new();
    for target in targets {
        let path = target["path"]
            .as_str()
            .ok_or("test target has no explicit path")?;
        let path = Path::new(path);
        if path.parent() == Some(Path::new("tests/suites")) {
            registered_suites.insert(path.to_path_buf());
            for file in suite_modules(&std::fs::read_to_string(root.join(path))?)? {
                *counts.entry(file).or_insert(0) += 1;
            }
        } else if path.parent() == Some(Path::new("tests")) {
            let file = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("invalid test path")?;
            *counts.entry(file.to_owned()).or_insert(0) += 1;
        }
    }
    // A forgotten Cargo entry must not hide a newly split suite either.
    for entry in root.join("tests/suites").read_dir()? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().ok_or("suite name is not UTF-8")?;
        if name.starts_with("integration_") && name.ends_with(".rs") {
            let path = Path::new("tests/suites").join(name);
            if !registered_suites.contains(&path) {
                return Err(format!("suite is missing from Cargo.toml: {}", path.display()).into());
            }
        }
    }
    let errors = coverage_errors(&root_files(&root)?, &counts);
    if !errors.is_empty() {
        return Err(errors.join("\n").into());
    }
    Ok(())
}

#[test]
fn inventory_rejects_unwired_duplicate_and_stale_files() {
    let files = BTreeSet::from(["new_test.rs".to_owned(), "existing.rs".to_owned()]);
    let counts = BTreeMap::from([("existing.rs".to_owned(), 2), ("stale.rs".to_owned(), 1)]);
    let errors = coverage_errors(&files, &counts);
    assert_eq!(errors.len(), 3);
    assert!(
        errors
            .iter()
            .any(|error| error.contains("new_test.rs") && error.contains("found 0"))
    );
    assert!(
        errors
            .iter()
            .any(|error| error.contains("existing.rs") && error.contains("found 2"))
    );
    assert!(
        errors
            .iter()
            .any(|error| error.contains("stale.rs") && error.contains("does not exist"))
    );
}

#[test]
fn inventory_counts_compiled_declarations_instead_of_comments() -> TestResult {
    let source = "// #[path = \"../omitted.rs\"]\n#[path = \"../present.rs\"]\nmod present;\n";
    assert_eq!(suite_modules(source)?, ["present.rs"]);
    assert!(suite_modules("#[path = \"../omitted.rs\"]\n// mod omitted;\n").is_err());
    assert!(suite_modules("#[path = \"../wrong_name.rs\"]\nmod other;\n").is_err());
    assert!(suite_modules("#[cfg(any())]\n#[path = \"../hidden.rs\"]\nmod hidden;\n").is_err());
    Ok(())
}

#[test]
fn inventory_shared_helpers_cannot_hide_root_test_files() -> TestResult {
    let source = "#[path = \"../support/graph_generator.rs\"]\nmod graph_generator;\n#[path = \"../present.rs\"]\nmod present;\n";
    assert_eq!(suite_modules(source)?, ["present.rs"]);
    assert!(suite_modules("#[path = \"../support/../hidden.rs\"]\nmod hidden;\n").is_err());
    assert!(suite_modules("#[path = \"../support/helper.rs\"]\n// mod helper;\n").is_err());
    let files = BTreeSet::from(["graph_generator.rs".to_owned()]);
    let counts = suite_modules(source)?
        .into_iter()
        .map(|file| (file, 1))
        .collect();
    assert!(
        coverage_errors(&files, &counts)
            .iter()
            .any(|error| error.contains("graph_generator.rs") && error.contains("found 0"))
    );
    Ok(())
}

/// bd-reality-core-convergence-1azkt.5 bullet 7: a `#[test]`-bearing file that
/// two declared targets both `#[path]`-include compiles into BOTH binaries, so
/// its tests run twice under two shard labels.
///
/// WHY `every_root_test_file_is_registered_exactly_once` DOES NOT CATCH THIS.
/// That check reads `tests/*.rs` -- root files -- against the suite
/// registrations. The duplication here happens one level down: a helper such as
/// `tests/contracts/common_spawn.rs` is not a root file and is never registered
/// as a suite module, yet it carried three `#[test]` fns and was included by
/// `tests/contracts.rs` and `tests/contracts/witness_retention_e2e.rs`, both of
/// which are declared `[[test]]` targets.
///
/// MEASURED BEFORE BEING FIXED, by asking the harness rather than reading
/// source: `cargo test --test <t> -- --list` joined across all 45 targets gave
/// 5109 rows and 5106 distinct names. The three duplicates were exactly those
/// three fns -- 3 names x 2 targets -- so the count and the cause agreed.
///
/// This guard is STATIC where the measurement was dynamic, and that is
/// deliberate: the property is a property of the include graph, which is
/// statically decidable, and a dynamic check would have to build 45 test
/// binaries to answer it. The instance that motivated it has already been
/// confirmed by execution.
#[test]
fn no_test_bearing_helper_is_included_by_two_targets() -> TestResult {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = std::fs::read_to_string(root.join("Cargo.toml"))?;

    // Declared [[test]] target roots, taken from Cargo.toml rather than guessed.
    let mut targets: Vec<PathBuf> = Vec::new();
    for line in manifest.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("path = \"") {
            if let Some(path) = rest.strip_suffix("\"") {
                if path.starts_with("tests/") && path.ends_with(".rs") {
                    targets.push(root.join(path));
                }
            }
        }
    }
    assert!(
        targets.len() >= 20,
        "expected the declared [[test]] targets to be readable from Cargo.toml; \
         found {}. A near-zero count means this test parsed nothing, not that \
         the manifest is clean.",
        targets.len()
    );

    // file -> the declared targets whose include graph reaches it
    let mut reached: BTreeMap<PathBuf, BTreeSet<String>> = BTreeMap::new();
    for target in &targets {
        let label = target
            .strip_prefix(root)
            .unwrap_or(target)
            .to_string_lossy()
            .into_owned();
        let mut queue = vec![target.clone()];
        let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
        while let Some(file) = queue.pop() {
            if !seen.insert(file.clone()) {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&file) else {
                continue;
            };
            reached
                .entry(file.clone())
                .or_default()
                .insert(label.clone());
            let dir = file.parent().unwrap_or(root).to_path_buf();
            for line in source.lines().map(str::trim) {
                // `#[path = "..."]` resolves relative to the INCLUDING file's
                // directory, which is why two includers of one helper spell it
                // differently ("contracts/common_spawn.rs" vs "common_spawn.rs")
                // and why a text search for either spelling finds only one.
                if let Some(rest) = line.strip_prefix("#[path = \"") {
                    if let Some(rel) = rest.strip_suffix("\"]") {
                        queue.push(dir.join(rel));
                    }
                }
            }
        }
    }

    let mut offenders = Vec::new();
    for (file, labels) in &reached {
        if labels.len() < 2 {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        let tests = source
            .lines()
            .filter(|line| line.trim_start().starts_with("#[test]"))
            .count();
        if tests > 0 {
            offenders.push(format!(
                "{} carries {tests} #[test] fn(s) and is included by {} targets: {:?}",
                file.strip_prefix(root).unwrap_or(file).display(),
                labels.len(),
                labels
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "these helpers compile their own tests into more than one [[test]] \
         binary, so each test runs twice under two shard labels:\n  {}",
        offenders.join("\n  ")
    );
    Ok(())
}
