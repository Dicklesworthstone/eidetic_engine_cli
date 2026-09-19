//! Read-only context packs must survive loss of the live generation when a
//! compatible, previously published generation is still retained.
//!
//! This module is included by the existing concurrent-search test target so
//! `autotests = false` cannot leave these tests unregistered.

use super::{TestResult, collect_all, degraded_codes, result_ids, run_ee, spawn_all};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy)]
enum Fault {
    MissingDirectory,
    CorruptVector,
    MissingLexical,
}

fn file_inventory(root: &Path) -> Result<BTreeMap<PathBuf, Option<Vec<u8>>>, String> {
    let mut result = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let key = path
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .to_path_buf();
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.is_dir() {
            result.insert(key, None);
            for entry in std::fs::read_dir(&path).map_err(|error| error.to_string())? {
                pending.push(entry.map_err(|error| error.to_string())?.path());
            }
        } else if metadata.is_file() {
            result.insert(
                key,
                Some(std::fs::read(&path).map_err(|error| error.to_string())?),
            );
        } else {
            return Err("unexpected special entry in the generation fixture".to_owned());
        }
    }
    Ok(result)
}

fn copy_generation(source: &Path, destination: &Path) -> TestResult {
    // The source was just published by the real CLI, not synthesized metadata.
    // create_dir and writes are fault-fixture setup, never the read path.
    std::fs::create_dir(destination).map_err(|error| error.to_string())?;
    for (relative, bytes) in file_inventory(source)? {
        if relative.as_os_str().is_empty() {
            continue;
        }
        let path = destination.join(relative);
        match bytes {
            Some(bytes) => std::fs::write(path, bytes).map_err(|error| error.to_string())?,
            None => std::fs::create_dir(path).map_err(|error| error.to_string())?,
        }
    }
    Ok(())
}

fn selected_ids(pack: &serde_json::Value) -> Result<Vec<String>, String> {
    pack.pointer("/data/pack/items")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "read-only pack has no item array".to_owned())?
        .iter()
        .map(|item| {
            item.get("memoryId")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| "manual-memory pack item has no memoryId".to_owned())
        })
        .collect()
}

struct RecoveryFixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    data_home: PathBuf,
    expected: Vec<String>,
}

fn initialized_fixture() -> Result<RecoveryFixture, String> {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let physical = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let workspace = physical.join("workspace");
    let data_home = physical.join("home");
    std::fs::create_dir(&workspace).map_err(|error| error.to_string())?;
    std::fs::create_dir(&data_home).map_err(|error| error.to_string())?;
    run_ee(&workspace, &data_home, &["init", "--json"])?;
    run_ee(
        &workspace,
        &data_home,
        &[
            "remember",
            "Before release run retainedquartzprotocol to verify snapshot continuity.",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--json",
        ],
    )?;
    run_ee(&workspace, &data_home, &["index", "rebuild", "--json"])?;
    let baseline = run_ee(
        &workspace,
        &data_home,
        &[
            "search",
            "retainedquartzprotocol",
            "--source-mode",
            "lexical_only",
            "--json",
        ],
    )?;
    let expected = result_ids(&baseline);
    assert_eq!(
        expected.len(),
        1,
        "the unique source memory must be retrievable"
    );
    Ok(RecoveryFixture {
        _root: root,
        workspace,
        data_home,
        expected,
    })
}

fn exercise_read_only_recovery(fault: Fault) -> TestResult {
    let RecoveryFixture {
        _root,
        workspace,
        data_home,
        expected,
    } = initialized_fixture()?;
    let live = workspace.join(".ee/index");
    let retained = workspace.join(".ee/index.previous.998");
    assert!(!retained.exists());
    // The parent/lease inode stays in place. Simulate loss after a real
    // publication by moving that generation into the recognized retained set.
    std::fs::rename(&live, &retained).map_err(|error| error.to_string())?;
    match fault {
        Fault::MissingDirectory => {}
        Fault::CorruptVector => {
            copy_generation(&retained, &live)?;
            std::fs::write(live.join("vector.fast.idx"), b"injected corrupt vector")
                .map_err(|error| error.to_string())?;
        }
        Fault::MissingLexical => {
            copy_generation(&retained, &live)?;
            std::fs::rename(live.join("lexical"), live.join("saved-lexical-fixture"))
                .map_err(|error| error.to_string())?;
            std::fs::create_dir(live.join("lexical")).map_err(|error| error.to_string())?;
        }
    }
    let retained_before = file_inventory(&retained)?;
    let live_before = live.exists().then(|| file_inventory(&live)).transpose()?;

    // No pack has run on this query before fault injection: a previously
    // cached context response must not serve as a substitute for real recovery.
    let args = [
        "pack",
        "retainedquartzprotocol snapshot continuity before release",
        "--read-only",
        "--source-mode",
        "lexical_only",
        "--max-tokens",
        "1500",
        "--json",
    ];
    let responses = collect_all(spawn_all(&workspace, &data_home, &args, 4)?, &args)?;
    let mut hashes = BTreeSet::new();
    let mut orders = BTreeSet::new();
    assert_eq!(responses.len(), 4);
    for response in responses {
        let ids = selected_ids(&response)?;
        assert!(
            ids.contains(&expected[0]),
            "the retained source memory must be packed"
        );
        assert!(
            !degraded_codes(&response).contains("context_lexical_fallback"),
            "a database scan is not proof of retained-index recovery"
        );
        let hash = response
            .pointer("/data/pack/hash")
            .and_then(serde_json::Value::as_str)
            .filter(|hash| !hash.is_empty())
            .ok_or_else(|| "read-only pack has no nonempty hash".to_owned())?;
        hashes.insert(hash.to_owned());
        orders.insert(ids);
    }
    assert_eq!(
        hashes.len(),
        1,
        "concurrent recovery packs must hash identically"
    );
    assert_eq!(
        orders.len(),
        1,
        "concurrent recovery packs must order identically"
    );
    assert_eq!(file_inventory(&retained)?, retained_before);
    assert_eq!(
        live.exists().then(|| file_inventory(&live)).transpose()?,
        live_before
    );
    Ok(())
}

#[test]
fn read_only_packs_recover_a_missing_live_directory_concurrently() -> TestResult {
    exercise_read_only_recovery(Fault::MissingDirectory)
}

#[test]
fn read_only_packs_recover_corrupt_live_vector_tiers_concurrently() -> TestResult {
    exercise_read_only_recovery(Fault::CorruptVector)
}

#[test]
fn read_only_packs_recover_missing_live_lexical_tiers_concurrently() -> TestResult {
    exercise_read_only_recovery(Fault::MissingLexical)
}

#[test]
fn read_only_pack_keeps_database_fallback_when_no_published_index_exists() -> TestResult {
    let fixture = initialized_fixture()?;
    let parent = fixture
        .workspace
        .parent()
        .ok_or("workspace fixture has no parent")?
        .join("empty-index-parent");
    std::fs::create_dir(&parent).map_err(|error| error.to_string())?;
    let missing = parent.join("index");
    let missing_arg = missing.to_str().ok_or("fixture path is not UTF-8")?;
    let before = file_inventory(&parent)?;
    let response = run_ee(
        &fixture.workspace,
        &fixture.data_home,
        &[
            "pack",
            "retainedquartzprotocol snapshot continuity",
            "--read-only",
            "--source-mode",
            "lexical_only",
            "--max-tokens",
            "1500",
            "--index-dir",
            missing_arg,
            "--json",
        ],
    )?;
    assert!(selected_ids(&response)?.contains(&fixture.expected[0]));
    assert!(degraded_codes(&response).contains("context_lexical_fallback"));
    assert_eq!(file_inventory(&parent)?, before);
    assert!(!missing.exists(), "fallback must not initialize an index");
    Ok(())
}
