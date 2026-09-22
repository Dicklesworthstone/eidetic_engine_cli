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
        let stem = file
            .strip_suffix(".rs")
            .filter(|stem| {
                stem.split('/').all(|part| {
                    !part.is_empty()
                        && part
                            .chars()
                            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                })
            })
            .ok_or_else(|| format!("invalid suite module path: {file}"))?;
        // Every declaration must still be followed by a `mod <ident>;` line.
        let declaration = lines
            .next()
            .ok_or_else(|| format!("{file} must be followed by a mod declaration"))?;
        let declared = declaration
            .strip_prefix("mod ")
            .and_then(|name| name.strip_suffix(';'))
            .filter(|name| {
                !name.is_empty()
                    && name
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            })
            .ok_or_else(|| format!("{file} must be followed by a mod declaration"))?;
        // A path with a directory component is a shared helper compiled once by
        // a suite. It does not replace a root test registration, and the suite
        // names it: `agent_mail_fixture/snapshot_v1.rs` is declared as
        // `agent_mail_snapshot_v1` so two workspace-hygiene modules can share
        // one copy without tripping clippy::duplicate_mod. Root files keep the
        // strict stem match, because that is what keeps the coverage counts
        // keyed to real file names -- relaxing it there would let a renamed
        // module silently stop covering its file.
        if stem.contains('/') {
            continue;
        }
        if declared != stem {
            return Err(format!("{file} must be followed by mod {stem};"));
        }
        modules.push(file.to_owned());
    }
    Ok(modules)
}

/// The files a source declares as modules through `#[path = "..."]`, as written.
///
/// An include counts only when the attribute is followed by its `mod` line
/// (`pub` and `pub(crate)` allowed, with `#[allow]`/`#[expect]` lint attributes
/// in between). A commented-out declaration, or a path attribute separated from
/// its `mod` by anything else, is not an include.
fn path_includes(source: &str) -> Vec<String> {
    let mut includes = Vec::new();
    let mut lines = source
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    while let Some(line) = lines.next() {
        let Some(file) = line
            .strip_prefix("#[path = \"")
            .and_then(|rest| rest.strip_suffix("\"]"))
        else {
            continue;
        };
        let declaration =
            lines.find(|next| !(next.starts_with("#[allow(") || next.starts_with("#[expect(")));
        let declares_module = declaration.is_some_and(|next| {
            let next = next
                .strip_prefix("pub(crate) ")
                .or_else(|| next.strip_prefix("pub "))
                .unwrap_or(next);
            next.starts_with("mod ") && next.ends_with(';')
        });
        if declares_module {
            includes.push(file.to_owned());
        }
    }
    includes
}

/// Resolve `.` and `..` without touching the filesystem.
fn normalize(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normal.pop();
            }
            other => normal.push(other),
        }
    }
    normal
}

/// Root test files reached through a `#[path]` include nested below a
/// registered entry point, counted once per including file.
///
/// `suite_modules` sees only a suite's direct declarations. A root file that
/// another test module includes -- `snapshot_index_recovery_e2e.rs` by
/// `concurrent_search_lexical_arm_e2e.rs`, `mcp_capture_git.rs` by the
/// `mcp_parity` target -- compiles into that parent's binary, yet this count
/// used to miss it and report both as unregistered (bd-tsrq7). Edges out of
/// suite files are skipped because `suite_modules` already counted them. Paths
/// resolve against the including file's directory, as rustc resolves them.
fn nested_root_includes(
    targets: &[PathBuf],
    suites: &BTreeSet<PathBuf>,
    read: impl Fn(&Path) -> Option<String>,
) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    let mut queue = targets.to_vec();
    let mut seen = BTreeSet::new();
    while let Some(file) = queue.pop() {
        if !seen.insert(file.clone()) {
            continue;
        }
        let Some(source) = read(&file) else {
            continue;
        };
        let dir = file.parent().unwrap_or(Path::new("")).to_path_buf();
        for include in path_includes(&source) {
            let resolved = normalize(&dir.join(include));
            let is_root_file = resolved.parent() == Some(Path::new("tests"))
                && resolved.extension().is_some_and(|ext| ext == "rs");
            if is_root_file && !suites.contains(&file) {
                if let Some(name) = resolved.file_name().and_then(|name| name.to_str()) {
                    *counts.entry(name.to_owned()).or_insert(0) += 1;
                }
            }
            queue.push(resolved);
        }
    }
    counts
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
    let mut target_paths = Vec::new();
    for target in targets {
        let path = target["path"]
            .as_str()
            .ok_or("test target has no explicit path")?;
        let path = Path::new(path);
        target_paths.push(path.to_path_buf());
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
    // A root file included one level further down is registered too: it
    // compiles into its parent's binary. Before this was counted, the check
    // reported tests/mcp_capture_git.rs and tests/snapshot_index_recovery_e2e.rs
    // as unregistered (bd-tsrq7), and "register them" would have compiled each
    // twice. An unreached root file still fails here, and so does one reached
    // twice.
    let nested = nested_root_includes(&target_paths, &registered_suites, |path| {
        std::fs::read_to_string(root.join(path)).ok()
    });
    for (file, count) in nested {
        *counts.entry(file).or_insert(0) += count;
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

#[test]
fn inventory_counts_root_files_included_below_a_registered_module() {
    let sources = BTreeMap::from([
        (
            "tests/suites/integration_x.rs",
            "#[path = \"../parent.rs\"]\nmod parent;\n",
        ),
        (
            "tests/parent.rs",
            "#[path = \"child_e2e.rs\"]\nmod child;\n\
             // #[path = \"commented.rs\"]\n// mod commented;\n\
             #[path = \"detached.rs\"]\nfn not_a_module() {}\n",
        ),
        (
            "tests/target_like.rs",
            "#[path = \"helpers/deep.rs\"]\n#[allow(dead_code)]\npub mod deep;\n",
        ),
        (
            "tests/helpers/deep.rs",
            "#[path = \"../grandchild.rs\"]\nmod grandchild;\n",
        ),
    ]);
    let targets = [
        PathBuf::from("tests/suites/integration_x.rs"),
        PathBuf::from("tests/target_like.rs"),
    ];
    let suites = BTreeSet::from([PathBuf::from("tests/suites/integration_x.rs")]);
    let read = |path: &Path| {
        path.to_str()
            .and_then(|key| sources.get(key))
            .map(|s| s.to_string())
    };

    // The suite's own edge to parent.rs belongs to suite_modules, so only the
    // nested includes are counted here -- and not the commented or detached ones.
    let nested = nested_root_includes(&targets, &suites, read);
    assert_eq!(
        nested,
        BTreeMap::from([
            ("child_e2e.rs".to_owned(), 1),
            ("grandchild.rs".to_owned(), 1)
        ])
    );

    // Composed with the direct counts, every reached file is covered once and
    // an unreached file is still reported.
    let mut counts = BTreeMap::from([
        ("parent.rs".to_owned(), 1),
        ("target_like.rs".to_owned(), 1),
    ]);
    counts.extend(nested);
    let files = BTreeSet::from([
        "parent.rs".to_owned(),
        "child_e2e.rs".to_owned(),
        "target_like.rs".to_owned(),
        "grandchild.rs".to_owned(),
        "orphan.rs".to_owned(),
    ]);
    assert_eq!(
        coverage_errors(&files, &counts),
        ["orphan.rs: expected one registered module/target, found 0"]
    );

    // Two includers of one root file compile it into two binaries.
    let mut twice = sources.clone();
    twice.insert(
        "tests/helpers/deep.rs",
        "#[path = \"../grandchild.rs\"]\nmod grandchild;\n#[path = \"../child_e2e.rs\"]\nmod child;\n",
    );
    let read_twice = |path: &Path| {
        path.to_str()
            .and_then(|key| twice.get(key))
            .map(|s| s.to_string())
    };
    let nested = nested_root_includes(&targets, &suites, read_twice);
    assert_eq!(nested.get("child_e2e.rs"), Some(&2));
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
