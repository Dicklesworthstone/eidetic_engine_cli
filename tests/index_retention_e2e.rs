//! Black-box retention bound for retained index generations
//! (bd-reality-core-convergence-1azkt.42).
//!
//! Every publication displaces the live generation into a retained sibling.
//! Before this bound, each `ee remember` left one full index copy behind and
//! nothing ever reclaimed them. These tests drive the real binary in an
//! isolated environment and assert, after every write, that the retained set
//! stays within the bound while the writes keep publishing (the liveness half:
//! a bound satisfied by publications that silently failed proves nothing).

use super::isolated_ee::isolated_ee_command;
use serde_json::Value as JsonValue;
use std::path::{Path, PathBuf};
use std::process::Output;

type TestResult = Result<(), String>;

const RETAINED_LIMIT: usize = 2;

fn unique_root(label: &str) -> Result<PathBuf, String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "ee-index-retention-{label}-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    Ok(root)
}

fn run(root: &Path, workspace: &Path, args: &[&str]) -> Result<Output, String> {
    let mut command = isolated_ee_command(root)?;
    command
        .current_dir(workspace)
        .arg("--workspace")
        .arg(workspace)
        .arg("--json")
        .args(args);
    command
        .output()
        .map_err(|error| format!("failed to run ee {args:?}: {error}"))
}

fn ok_json(output: &Output, context: &str) -> Result<JsonValue, String> {
    if !output.status.success() {
        return Err(format!(
            "{context} failed ({:?}): stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "{context} stdout was not JSON: {error}; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn retained_dirs(store: &Path) -> Result<Vec<PathBuf>, String> {
    let mut retained = Vec::new();
    for entry in std::fs::read_dir(store).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if (name == "index.previous" || name.starts_with("index.previous."))
            && entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_dir()
        {
            retained.push(entry.path());
        }
    }
    retained.sort();
    Ok(retained)
}

fn live_generation(store: &Path) -> Result<u64, String> {
    let raw = std::fs::read_to_string(store.join("index").join("meta.json"))
        .map_err(|error| format!("live index metadata unreadable: {error}"))?;
    let meta: JsonValue = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
    meta.get("generation")
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| format!("live index metadata has no generation: {raw}"))
}

#[test]
fn consecutive_writes_keep_retained_generations_bounded() -> TestResult {
    let root = unique_root("writes")?;
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let store = workspace.join(".ee");
    ok_json(&run(&root, &workspace, &["init"])?, "init")?;

    let mut previous_generation = None;
    let writes = 8;
    for write in 0..writes {
        let content = format!("Retention probe lesson {write}: keep the index bounded per write.");
        ok_json(
            &run(
                &root,
                &workspace,
                &[
                    "remember", &content, "--level", "semantic", "--kind", "fact",
                ],
            )?,
            "remember",
        )?;
        let retained = retained_dirs(&store)?;
        if retained.len() > RETAINED_LIMIT {
            return Err(format!(
                "write {write}: {} retained generations exceed the bound {RETAINED_LIMIT}: {retained:?}",
                retained.len()
            ));
        }
        let generation = live_generation(&store)?;
        if previous_generation.is_some_and(|previous| generation <= previous) {
            return Err(format!(
                "write {write}: live generation {generation} did not advance past {previous_generation:?}; the bound must not be satisfied by publications that stopped"
            ));
        }
        previous_generation = Some(generation);
    }
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn vacuum_apply_reclaims_exactly_the_previewed_generations_once() -> TestResult {
    let root = unique_root("vacuum")?;
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let store = workspace.join(".ee");
    ok_json(&run(&root, &workspace, &["init"])?, "init")?;
    ok_json(
        &run(
            &root,
            &workspace,
            &[
                "remember",
                "Vacuum probe: reclaim beyond the bound.",
                "--kind",
                "fact",
            ],
        )?,
        "remember",
    )?;
    // A store upgraded from an unbounded binary: stale copies with no usable
    // generation, at high sequence numbers.
    for number in 500..505 {
        let stale = store.join(format!("index.previous.{number}"));
        std::fs::create_dir_all(&stale).map_err(|error| error.to_string())?;
        std::fs::write(stale.join("old.bin"), vec![7_u8; 4096])
            .map_err(|error| error.to_string())?;
    }

    let preview = ok_json(&run(&root, &workspace, &["index", "vacuum"])?, "vacuum")?;
    let retention = &preview["data"]["retention"];
    let previewed = retention["reclaimable"]
        .as_array()
        .ok_or("preview has no retention.reclaimable")?
        .iter()
        .filter_map(|entry| entry["path"].as_str().map(str::to_owned))
        .collect::<std::collections::BTreeSet<_>>();
    if previewed.len() < 5 {
        return Err(format!("preview must list the 5 stale copies: {retention}"));
    }
    if preview["data"]["mutationAllowed"] != false {
        return Err("the default vacuum run must stay a preview".to_owned());
    }
    if retained_dirs(&store)?.len() < 5 {
        return Err("preview must not delete anything".to_owned());
    }

    let applied = ok_json(
        &run(&root, &workspace, &["index", "vacuum", "--apply"])?,
        "vacuum --apply",
    )?;
    let reclaimed = applied["data"]["reclaimed"]
        .as_array()
        .ok_or("apply has no reclaimed list")?
        .iter()
        .filter_map(|entry| entry["path"].as_str().map(str::to_owned))
        .collect::<std::collections::BTreeSet<_>>();
    if reclaimed != previewed {
        return Err(format!(
            "apply must reclaim exactly the preview: previewed={previewed:?} reclaimed={reclaimed:?}"
        ));
    }
    if applied["data"]["auditId"]
        .as_str()
        .is_none_or(str::is_empty)
    {
        return Err(format!("apply must record an audit row: {applied}"));
    }
    if retained_dirs(&store)?.len() > RETAINED_LIMIT {
        return Err("apply must leave at most the retention bound".to_owned());
    }
    live_generation(&store)?;

    let again = ok_json(
        &run(&root, &workspace, &["index", "vacuum", "--apply"])?,
        "second vacuum --apply",
    )?;
    if again["data"]["reclaimedCount"] != 0 {
        return Err(format!("a second apply must reclaim nothing: {again}"));
    }
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// bd-reality-core-convergence-1azkt.57: writes publish as staged deltas over
/// the live generation. A delta-built index must answer exactly like a full
/// rebuild of the same corpus: same hits in the same order, same pack.
#[test]
fn delta_published_index_answers_like_a_full_rebuild() -> TestResult {
    let root = unique_root("delta-equivalence")?;
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    ok_json(&run(&root, &workspace, &["init"])?, "init")?;
    let topics = ["release", "cargo", "sqlite", "replay", "clippy", "golden"];
    for (index, topic) in topics.iter().cycle().take(12).enumerate() {
        let content =
            format!("Lesson {index} about {topic}: always verify the {topic} step before merging.");
        ok_json(
            &run(&root, &workspace, &["remember", &content, "--kind", "rule"])?,
            "remember",
        )?;
    }
    let observe = |root: &Path, workspace: &Path| -> Result<Vec<String>, String> {
        let mut observed = Vec::new();
        for query in ["verify release step", "cargo merging", "golden replay"] {
            let search = ok_json(
                &run(
                    root,
                    workspace,
                    &[
                        "search",
                        query,
                        "--limit",
                        "10",
                        "--source-mode",
                        "lexical_only",
                    ],
                )?,
                "search",
            )?;
            let ids = search["data"]["results"]
                .as_array()
                .ok_or("search has no results array")?
                .iter()
                .map(|hit| hit["docId"].as_str().unwrap_or_default().to_owned())
                .collect::<Vec<_>>();
            observed.push(format!("search {query}: {ids:?}"));
            let pack = ok_json(
                &run(
                    root,
                    workspace,
                    &[
                        "pack",
                        query,
                        "--read-only",
                        "--max-tokens",
                        "1500",
                        "--source-mode",
                        "lexical_only",
                    ],
                )?,
                "pack",
            )?;
            observed.push(format!("pack {query}: {}", pack["data"]["pack"]["hash"]));
        }
        Ok(observed)
    };
    let delta_built = observe(&root, &workspace)?;
    ok_json(
        &run(&root, &workspace, &["index", "rebuild"])?,
        "index rebuild",
    )?;
    let rebuilt = observe(&root, &workspace)?;
    if delta_built != rebuilt {
        return Err(format!(
            "delta-built index diverges from a full rebuild:\ndelta={delta_built:#?}\nfull={rebuilt:#?}"
        ));
    }
    if !delta_built.iter().any(|line| line.contains("mem_")) {
        return Err(format!(
            "the probe must observe real hits: {delta_built:#?}"
        ));
    }
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
