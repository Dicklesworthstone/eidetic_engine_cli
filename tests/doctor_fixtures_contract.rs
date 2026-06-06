#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_root() -> PathBuf {
    repo_root().join("tests/doctor_fixtures")
}

fn manifest() -> Value {
    let path = fixture_root().join("manifest.json");
    let text = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("failed to read {}: {error}", path.display());
    });
    serde_json::from_str(&text).unwrap_or_else(|error| {
        panic!("failed to parse {}: {error}", path.display());
    })
}

fn manifest_ids() -> BTreeSet<String> {
    manifest()["fixtures"]
        .as_array()
        .expect("fixtures array")
        .iter()
        .map(|entry| entry["id"].as_str().expect("fixture id").to_owned())
        .collect()
}

fn scored_p0_p1_fms() -> BTreeSet<String> {
    let path = repo_root().join("doctor_workspace/failure_mode_scores.jsonl");
    let text = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("failed to read {}: {error}", path.display());
    });
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let value: Value = serde_json::from_str(line).expect("failure mode score json");
            let severity = value["severity"].as_str().expect("severity");
            matches!(severity, "P0" | "P1")
                .then(|| value["fm_id"].as_str().expect("fm_id").to_owned())
        })
        .collect()
}

#[test]
fn doctor_fixtures_manifest_covers_scored_p0_p1_fms() {
    let expected = scored_p0_p1_fms();
    let actual = manifest_ids();
    assert_eq!(actual, expected);
}

#[test]
fn doctor_fixtures_have_triplet_files_and_metadata() {
    let manifest = manifest();
    let fixtures = manifest["fixtures"].as_array().expect("fixtures array");
    for fixture in fixtures {
        let id = fixture["id"].as_str().expect("id");
        let severity = fixture["severity"].as_str().expect("severity");
        let subsystem = fixture["subsystem"].as_str().expect("subsystem");
        let spec = repo_root().join(fixture["spec"].as_str().expect("spec"));
        assert!(spec.is_file(), "missing spec for {id}: {}", spec.display());

        let dir = fixture_root().join(id);
        assert!(dir.is_dir(), "missing fixture dir {}", dir.display());
        for name in ["README.md", "corrupt.sh", "assert.sh"] {
            assert!(dir.join(name).is_file(), "missing {id}/{name}");
        }

        let readme = fs::read_to_string(dir.join("README.md")).expect("readme");
        assert!(readme.contains(id), "README must name {id}");
        assert!(
            readme.contains(severity),
            "README must name severity for {id}"
        );
        assert!(
            readme.contains(subsystem),
            "README must name subsystem for {id}"
        );
        assert!(
            readme.contains("doctor_workspace/analysis/repair_specs/"),
            "README must cite repair spec for {id}"
        );
    }
}

#[test]
fn doctor_fixtures_scripts_are_non_destructive_and_no_local_cargo() {
    let forbidden = [
        "rm ",
        "rm\t",
        "rm -",
        "git reset",
        "git checkout",
        "git stash",
        "git worktree",
        "cargo ",
        "rustc ",
        "rustdoc ",
    ];
    for entry in walk_fixture_scripts(&fixture_root()) {
        let text = fs::read_to_string(&entry).expect("script read");
        for needle in forbidden {
            assert!(
                !text.contains(needle),
                "{} contains forbidden token {needle:?}",
                entry.display()
            );
        }
    }
}

#[test]
fn doctor_fixtures_manifest_has_unique_dirs_by_subsystem() {
    let manifest = manifest();
    let mut by_subsystem: BTreeMap<String, usize> = BTreeMap::new();
    for fixture in manifest["fixtures"].as_array().expect("fixtures array") {
        let subsystem = fixture["subsystem"].as_str().expect("subsystem");
        *by_subsystem.entry(subsystem.to_owned()).or_default() += 1;
    }
    for subsystem in [
        "agent_coordination",
        "cass_integration",
        "graph_subsystem",
        "policy_safety",
        "schema_migrations",
        "search_indexes",
        "state_files",
        "workspace_config",
    ] {
        assert!(
            by_subsystem.contains_key(subsystem),
            "missing subsystem coverage for {subsystem}"
        );
    }
}

fn walk_fixture_scripts(root: &Path) -> Vec<PathBuf> {
    let mut scripts = Vec::new();
    scripts.push(root.join("lib.sh"));
    scripts.push(root.join("run_all.sh"));
    for id in manifest_ids() {
        scripts.push(root.join(&id).join("corrupt.sh"));
        scripts.push(root.join(id).join("assert.sh"));
    }
    scripts
}
