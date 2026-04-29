//! EE-012 forbidden-dependency audit.
//!
//! AGENTS.md `Forbidden Dependencies (Hard Rule, Audited By CI)` requires
//! the resolved dependency tree to exclude `tokio`, `tokio-util`,
//! `async-std`, `smol`, `rusqlite`, `sqlx`, `diesel`, `sea-orm`, `petgraph`,
//! `hyper`, `axum`, `tower`, and `reqwest`. This integration test fails if
//! any of those crate names appears in the resolved cargo tree under the
//! default feature set or under `--all-features`.
//!
//! The test shells out to `cargo tree --prefix none --edges normal,build,dev`
//! and matches the first whitespace-separated token of each non-empty line
//! against the forbidden list. The test is deterministic and offline as
//! long as the local cargo cache already has the manifest's resolved
//! dependencies; it does not perform new network resolution.

use std::collections::BTreeSet;
use std::process::Command;

const FORBIDDEN_CRATES: &[&str] = &[
    "tokio",
    "tokio-util",
    "async-std",
    "smol",
    "rusqlite",
    "sqlx",
    "diesel",
    "sea-orm",
    "petgraph",
    "hyper",
    "axum",
    "tower",
    "reqwest",
];

fn manifest_path() -> String {
    format!("{}/Cargo.toml", env!("CARGO_MANIFEST_DIR"))
}

fn run_cargo_tree(extra: &[&str]) -> String {
    let mut args: Vec<&str> = vec![
        "tree",
        "--edges",
        "normal,build,dev",
        "--prefix",
        "none",
        "--manifest-path",
    ];
    let manifest = manifest_path();
    args.push(manifest.as_str());
    args.extend_from_slice(extra);

    let output = match Command::new(env!("CARGO")).args(&args).output() {
        Ok(value) => value,
        Err(error) => panic!("failed to invoke `cargo tree`: {error}"),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        panic!("`cargo tree` returned non-zero exit code\nstdout:\n{stdout}\nstderr:\n{stderr}");
    }

    match String::from_utf8(output.stdout) {
        Ok(text) => text,
        Err(error) => panic!("`cargo tree` produced non-UTF-8 output: {error}"),
    }
}

fn forbidden_hits(tree_output: &str) -> BTreeSet<&'static str> {
    let mut hits: BTreeSet<&'static str> = BTreeSet::new();
    for line in tree_output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let name = match trimmed.split_whitespace().next() {
            Some(value) => value,
            None => continue,
        };
        for forbidden in FORBIDDEN_CRATES {
            if name == *forbidden {
                hits.insert(*forbidden);
            }
        }
    }
    hits
}

fn fail_with_hits(scenario: &str, hits: BTreeSet<&'static str>) -> ! {
    let names: Vec<&str> = hits.into_iter().collect();
    panic!(
        "Forbidden dependencies present in {scenario} feature tree: {}.\n\n\
         Fix: remove the dependency, or quarantine it behind an explicit feature \
         that is disabled by default. See AGENTS.md \
         `Forbidden Dependencies (Hard Rule, Audited By CI)` for the canonical \
         list and rationale.",
        names.join(", ")
    );
}

#[test]
fn default_feature_tree_excludes_forbidden_crates() {
    let tree = run_cargo_tree(&[]);
    let hits = forbidden_hits(&tree);
    if !hits.is_empty() {
        fail_with_hits("default", hits);
    }
}

#[test]
fn all_features_tree_excludes_forbidden_crates() {
    let tree = run_cargo_tree(&["--all-features"]);
    let hits = forbidden_hits(&tree);
    if !hits.is_empty() {
        fail_with_hits("--all-features", hits);
    }
}

#[test]
fn no_default_features_tree_excludes_forbidden_crates() {
    let tree = run_cargo_tree(&["--no-default-features"]);
    let hits = forbidden_hits(&tree);
    if !hits.is_empty() {
        fail_with_hits("--no-default-features", hits);
    }
}

#[cfg(test)]
mod self_tests {
    use super::{FORBIDDEN_CRATES, forbidden_hits};

    #[test]
    fn detects_each_forbidden_crate_when_present() {
        for forbidden in FORBIDDEN_CRATES {
            let synthetic = format!("ee v0.1.0\n{forbidden} v1.0.0\nserde v1.0.0\n");
            let hits = forbidden_hits(&synthetic);
            assert!(
                hits.contains(forbidden),
                "scanner failed to detect `{forbidden}` in synthetic tree"
            );
        }
    }

    #[test]
    fn ignores_unrelated_crates() {
        let synthetic = "ee v0.1.0\nserde v1.0.0\nclap v4.5.0\nthiserror v1.0.0\n";
        let hits = forbidden_hits(synthetic);
        assert!(
            hits.is_empty(),
            "scanner produced false positives: {hits:?}"
        );
    }

    #[test]
    fn ignores_empty_and_whitespace_lines() {
        let synthetic = "\n   \nee v0.1.0\n\n";
        let hits = forbidden_hits(synthetic);
        assert!(hits.is_empty());
    }

    #[test]
    fn matches_exact_crate_name_not_substring() {
        // A crate named `tokio-foo` would share the prefix but is not on the
        // forbidden list; only the exact crate name should match.
        let synthetic = "ee v0.1.0\ntokio-foo v0.1.0\nrusqlite-clone v0.1.0\n";
        let hits = forbidden_hits(synthetic);
        assert!(hits.is_empty(), "false positives: {hits:?}");
    }
}
