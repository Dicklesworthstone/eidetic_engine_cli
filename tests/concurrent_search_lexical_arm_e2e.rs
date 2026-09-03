//! bd-reality-core-convergence-1azkt.23: concurrent read-only `ee search` and
//! `ee pack` processes must all keep the lexical arm and agree byte-for-byte.
//!
//! Before the read-only Tantivy open landed, every search process opened the
//! lexical index through a constructor that also built an `IndexWriter`, which
//! takes Tantivy's exclusive `.tantivy-writer.lock`. A second concurrent
//! process failed that open, `lexical_search_available()` returned false, and
//! hybrid retrieval silently fell back to semantic-only ranking
//! (`source_mode_fallback`) or, in hash-fallback environments, to zero results.
//! Six concurrent read-only packs produced two or three distinct pack hashes on
//! the public v0.14.4 binary (2026-09-02 probe).
//!
//! This test is environment-agnostic about the embedder: with the pinned
//! Model2Vec model present the arms fuse (`rrf_fused`); without it the
//! deterministic hash fallback runs. Either way, every concurrent process must
//! report the same applied source mode, the same non-empty ordered result ids,
//! no lexical-loss degradation, and the same pack hash.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};

type TestResult = Result<(), String>;

const LEXICAL_LOSS_CODES: [&str; 3] = [
    "source_mode_fallback",
    "lexical_unavailable",
    "search_unavailable",
];

fn ee_command(workspace: &Path, data_home: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .env_remove("EE_DATABASE_PATH")
        .env_remove("EE_INDEX_DIR")
        // Keep the user-global lane and any model download out of the test.
        .env("HOME", data_home)
        .env("XDG_DATA_HOME", data_home)
        .env("XDG_CONFIG_HOME", data_home)
        .env("EE_EMBED_DOWNLOAD", "off")
        .env("EE_NO_COLOR", "1")
        .arg("--workspace")
        .arg(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn run_ee(workspace: &Path, data_home: &Path, args: &[&str]) -> Result<serde_json::Value, String> {
    let output = ee_command(workspace, data_home, args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))?;
    parse_success(&output, args)
}

fn parse_success(output: &Output, args: &[&str]) -> Result<serde_json::Value, String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(format!(
            "ee {} exited {:?}\nstdout: {stdout}\nstderr: {stderr}",
            args.join(" "),
            output.status.code()
        ));
    }
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).map_err(|error| {
        format!(
            "ee {} stdout was not JSON: {error}\n{stdout}",
            args.join(" ")
        )
    })?;
    if value["success"] != serde_json::Value::Bool(true) {
        return Err(format!(
            "ee {} did not report success: {}",
            args.join(" "),
            serde_json::to_string(&value).unwrap_or_default()
        ));
    }
    Ok(value)
}

fn degraded_codes(value: &serde_json::Value) -> BTreeSet<String> {
    value["degraded"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry["code"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn result_ids(value: &serde_json::Value) -> Vec<String> {
    value["data"]["results"]
        .as_array()
        .map(|results| {
            results
                .iter()
                .filter_map(|result| {
                    result["id"]
                        .as_str()
                        .or_else(|| result["memoryId"].as_str())
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn spawn_all(
    workspace: &Path,
    data_home: &Path,
    args: &[&str],
    count: usize,
) -> Result<Vec<Child>, String> {
    (0..count)
        .map(|_| {
            ee_command(workspace, data_home, args)
                .spawn()
                .map_err(|error| format!("failed to spawn ee {}: {error}", args.join(" ")))
        })
        .collect()
}

fn collect_all(children: Vec<Child>, args: &[&str]) -> Result<Vec<serde_json::Value>, String> {
    children
        .into_iter()
        .map(|child| {
            let output = child
                .wait_with_output()
                .map_err(|error| format!("failed to wait for ee {}: {error}", args.join(" ")))?;
            parse_success(&output, args)
        })
        .collect()
}

#[test]
fn concurrent_searches_keep_the_lexical_arm_and_agree_on_order() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().join("workspace");
    let data_home = tempdir.path().join("home");
    std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&data_home).map_err(|error| error.to_string())?;

    run_ee(&workspace, &data_home, &["init", "--json"])?;
    let rules = [
        "Run cargo fmt --check before every release tag.",
        "Release verification must go through the remote RCH lane, never local cargo.",
        "Clippy nursery and pedantic lints are errors in CI; fix them before a release.",
        "Never publish a release without a SHA-256 checksum for every asset.",
        "The release workflow triggers on a version tag pushed to main.",
        "Backups must be verified before a release restore drill.",
        "Search index generation must equal DB generation before release smoke tests.",
        "Frontend CSS tweaks are unrelated to release verification.",
    ];
    for rule in rules {
        run_ee(
            &workspace,
            &data_home,
            &[
                "remember",
                rule,
                "--level",
                "procedural",
                "--kind",
                "rule",
                "--json",
            ],
        )?;
    }
    run_ee(&workspace, &data_home, &["index", "rebuild", "--json"])?;

    // Serial baseline: what one uncontended process reports.
    let search_args = [
        "search",
        "release verification remote lane",
        "--limit",
        "5",
        "--json",
    ];
    let baseline = run_ee(&workspace, &data_home, &search_args)?;
    let baseline_ids = result_ids(&baseline);
    if baseline_ids.is_empty() {
        return Err(format!(
            "serial baseline search returned no results: {}",
            serde_json::to_string(&baseline).unwrap_or_default()
        ));
    }
    let baseline_mode = baseline["data"]["metrics"]["sourceModeApplied"].clone();
    let baseline_codes = degraded_codes(&baseline);
    for code in LEXICAL_LOSS_CODES {
        if baseline_codes.contains(code) {
            return Err(format!(
                "serial baseline already lost the lexical arm ({code}); the fixture is broken: {:?}",
                baseline_codes
            ));
        }
    }

    // Eight processes open the same published generation at once. Every one
    // of them must serve it; none may fail the lexical open and degrade.
    let children = spawn_all(&workspace, &data_home, &search_args, 8)?;
    let responses = collect_all(children, &search_args)?;
    for (index, response) in responses.iter().enumerate() {
        let codes = degraded_codes(response);
        for code in LEXICAL_LOSS_CODES {
            if codes.contains(code) {
                return Err(format!(
                    "concurrent search #{index} lost the lexical arm ({code}); degraded={codes:?}"
                ));
            }
        }
        let mode = &response["data"]["metrics"]["sourceModeApplied"];
        if mode != &baseline_mode {
            return Err(format!(
                "concurrent search #{index} applied source mode {mode} but the serial baseline applied {baseline_mode}"
            ));
        }
        let ids = result_ids(response);
        if ids != baseline_ids {
            return Err(format!(
                "concurrent search #{index} ranked {ids:?} but the serial baseline ranked {baseline_ids:?}"
            ));
        }
    }

    // Read-only packs over the same state must hash identically.
    let pack_args = [
        "pack",
        "prepare release",
        "--read-only",
        "--max-tokens",
        "1500",
        "--json",
    ];
    let baseline_pack = run_ee(&workspace, &data_home, &pack_args)?;
    let baseline_hash = baseline_pack["data"]["pack"]["hash"]
        .as_str()
        .ok_or("serial baseline pack carried no data.pack.hash")?
        .to_owned();
    let children = spawn_all(&workspace, &data_home, &pack_args, 6)?;
    let packs = collect_all(children, &pack_args)?;
    for (index, pack) in packs.iter().enumerate() {
        let hash = pack["data"]["pack"]["hash"].as_str().unwrap_or_default();
        if hash != baseline_hash {
            return Err(format!(
                "concurrent pack #{index} hashed {hash} but the serial baseline hashed {baseline_hash}; degraded={:?}",
                degraded_codes(pack)
            ));
        }
        let codes = degraded_codes(pack);
        for code in LEXICAL_LOSS_CODES {
            if codes.contains(code) {
                return Err(format!(
                    "concurrent pack #{index} lost the lexical arm ({code}); degraded={codes:?}"
                ));
            }
        }
    }
    Ok(())
}
