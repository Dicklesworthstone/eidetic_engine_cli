//! bd-1zb7k.10.2: contract snapshot for `ee.cache.hotset.v1`.
//!
//! Builds an in-process [`HotsetManifest`] from seeded
//! `SearchHotsetEntry` and `PackHotsetEntry` records and pins its JSON
//! shape via insta. This is a structural contract: the snapshot guards
//! against schema drift in the public artifact emitted by the
//! redaction-safe hotset recorder, independent of the inline unit tests
//! that live alongside the module.
//!
//! The structural tests exercise the deterministic builder path. The
//! bd-ty3pl.2 regressions also spawn the public `ee cache hotset-manifest`
//! route against bounded on-disk fixtures so collector reachability is proved
//! independently of private helpers.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ee::cache::hotset::{GenerationGate, HotsetBudget, HotsetManifestBuilder};
use ee::pack::{PackHotsetEntry, PackHotsetEntryKind};
use ee::search::SearchHotsetEntry;
use insta::assert_json_snapshot;
use serde_json::{Value as JsonValue, json};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[cfg(unix)]
fn write_executable_stub(path: &Path, body: &str) -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, format!("#!/bin/sh\nset -eu\n{body}\n"))
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    let mut permissions = fs::metadata(path)
        .map_err(|error| format!("stat {}: {error}", path.display()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .map_err(|error| format!("chmod {}: {error}", path.display()))
}

#[cfg(unix)]
fn hotset_source_authority_snapshot() -> JsonValue {
    let source_kinds = [
        "actionable_queue",
        "agent_mail",
        "beads",
        "bv",
        "git",
        "host_profile",
        "installed_binary",
        "memory_drift",
        "rch",
        "support_bundle",
        "toolchain",
        "workspace_hygiene",
    ];
    json!({
        "schema": "ee.source_authority.snapshot.v1",
        "overall": {"failClosed": false},
        "sources": source_kinds.map(|source_kind| json!({
            "sourceKind": source_kind,
            "state": "ready",
            "authoritative": true,
        })),
    })
}

#[cfg(unix)]
fn prepare_hotset_workspace() -> Result<(tempfile::TempDir, PathBuf, PathBuf), String> {
    let workspace = tempfile::Builder::new()
        .prefix("ee-hotset-public-cli-")
        .tempdir()
        .map_err(|error| format!("create hotset workspace: {error}"))?;
    fs::create_dir_all(workspace.path().join(".beads"))
        .map_err(|error| format!("create .beads fixture directory: {error}"))?;
    fs::create_dir_all(workspace.path().join(".ee"))
        .map_err(|error| format!("create .ee fixture directory: {error}"))?;
    let authority_path = workspace.path().join("authority.json");
    fs::write(
        &authority_path,
        hotset_source_authority_snapshot().to_string(),
    )
    .map_err(|error| format!("write {}: {error}", authority_path.display()))?;

    let git_stub = workspace.path().join("git-stub");
    write_executable_stub(&git_stub, "printf ' M src/planted-private-path.rs\\n'")?;
    let bv_stub = workspace.path().join("bv-stub");
    write_executable_stub(
        &bv_stub,
        "printf '%s\\n' '{\"id\":\"bd-planted-bv\",\"title\":\"planted private bv title\"}'",
    )?;
    Ok((workspace, git_stub, bv_stub))
}

#[cfg(unix)]
fn write_large_beads_tracker(path: &Path) -> TestResult {
    let file = File::create(path).map_err(|error| format!("create {}: {error}", path.display()))?;
    let mut writer = BufWriter::new(file);
    let filler = format!(
        "{{\"id\":\"bd-closed-padding\",\"title\":\"closed padding\",\"status\":\"closed\",\"issue_type\":\"task\",\"padding\":\"{}\"}}\n",
        "x".repeat(896)
    );
    let target_bytes = 8 * 1024 * 1024 + 1;
    let mut written = 0_usize;
    while written <= target_bytes {
        writer
            .write_all(filler.as_bytes())
            .map_err(|error| format!("write {}: {error}", path.display()))?;
        written = written.saturating_add(filler.len());
    }
    for index in 0..200 {
        writeln!(
            writer,
            "{{\"id\":\"bd-open-{index:03}\",\"title\":\"planted private bead title {index:03}\",\"status\":\"open\",\"issue_type\":\"task\",\"priority\":1}}"
        )
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    }
    writer
        .flush()
        .map_err(|error| format!("flush {}: {error}", path.display()))
}

#[cfg(unix)]
fn run_public_hotset_cli(
    workspace: &Path,
    git_stub: &Path,
    bv_stub: &Path,
    authority_path: &Path,
) -> Result<Output, String> {
    let workspace_arg = workspace.to_string_lossy().into_owned();
    let git_arg = git_stub.to_string_lossy().into_owned();
    let bv_arg = bv_stub.to_string_lossy().into_owned();
    let authority_arg = authority_path.to_string_lossy().into_owned();
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args([
            "--json",
            "--workspace",
            workspace_arg.as_str(),
            "cache",
            "hotset-manifest",
            "--probe-timeout-ms",
            "60000",
            "--bv-timeout-ms",
            "60000",
            "--max-signals-per-source",
            "32",
            "--git-program",
            git_arg.as_str(),
            "--bv-program",
            bv_arg.as_str(),
            "--source-authority-snapshot",
            authority_arg.as_str(),
        ])
        .output()
        .map_err(|error| format!("spawn public ee hotset collector: {error}"))
}

#[test]
fn cache_hotset_manifest_json_shape_is_stable() -> TestResult {
    let pack_audit = PackHotsetEntry {
        key: "pack:audit:fixture".to_owned(),
        kind: PackHotsetEntryKind::SelectionAudit,
        section: None,
        generation: 5,
        estimated_bytes: 256,
        hit_count: 4,
        redaction_status: "content_not_stored",
    };

    let manifest =
        HotsetManifestBuilder::new("ws_01HQTSNAPSHOT00000000000", GenerationGate::new(5, 5))
            .with_profile_tier("balanced")
            .with_captured_at("2026-05-19T20:00:00Z")
            .with_budget(HotsetBudget::new(1024, 1_048_576).with_current(3, 768))
            .search_entries([
                SearchHotsetEntry::memory("mem_alpha_______________________", 5, 3),
                SearchHotsetEntry::memory("mem_beta________________________", 5, 1),
                SearchHotsetEntry::query_shape("ee context release", 5, 2)
                    .ok_or_else(|| "query shape should normalize".to_owned())?,
            ])
            .pack_entries([pack_audit])
            .build();

    assert_json_snapshot!("cache_hotset_v1_manifest", manifest.to_json());
    Ok(())
}

#[test]
fn cache_hotset_manifest_emits_degraded_when_stale_entries_rejected() -> TestResult {
    let manifest =
        HotsetManifestBuilder::new("ws_01HQTSNAPSHOT00000000000", GenerationGate::new(10, 10))
            .with_profile_tier("balanced")
            .with_captured_at("2026-05-19T20:00:00Z")
            .with_budget(HotsetBudget::new(1024, 1_048_576))
            .search_entries([
                SearchHotsetEntry::memory("mem_fresh_______________________", 10, 1),
                SearchHotsetEntry::memory("mem_stale_______________________", 4, 1),
            ])
            .build();

    assert_json_snapshot!(
        "cache_hotset_v1_manifest_stale_rejected",
        manifest.to_json()
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn cache_hotset_manifest_real_binary_streams_large_tracker_privately() -> TestResult {
    let (workspace, git_stub, bv_stub) = prepare_hotset_workspace()?;
    let tracker = workspace.path().join(".beads").join("issues.jsonl");
    write_large_beads_tracker(&tracker)?;
    let tracker_len = fs::metadata(&tracker)
        .map_err(|error| format!("stat {}: {error}", tracker.display()))?
        .len();
    ensure(
        tracker_len > 8 * 1024 * 1024,
        "fixture must exceed the removed 8 MiB whole-file limit",
    )?;

    let output = run_public_hotset_cli(
        workspace.path(),
        &git_stub,
        &bv_stub,
        &workspace.path().join("authority.json"),
    )?;
    ensure(
        output.status.success(),
        format!(
            "public hotset collector failed: stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let response: JsonValue = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("public hotset stdout was not JSON: {error}"))?;
    ensure(
        response["schema"] == "ee.response.v2",
        "response schema drift",
    )?;
    ensure(
        response["data"]["schema"] == "ee.cache.hotset_collect.v1",
        "collector schema drift",
    )?;
    let beads = response["data"]["sources"]
        .as_array()
        .and_then(|sources| {
            sources
                .iter()
                .find(|source| source["source"] == "beads_tracker")
        })
        .ok_or_else(|| "public response omitted the Beads source".to_owned())?;
    ensure(
        beads["status"] == "fresh",
        "large Beads source was not fresh",
    )?;
    ensure(
        beads["signalCount"] == 32,
        "large Beads source did not retain the deterministic 32-signal cap",
    )?;
    ensure(
        response["data"]["plan"]["candidateCount"]
            .as_u64()
            .is_some_and(|count| count > 0),
        "healthy authority should admit bounded candidates",
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure(
        !stdout.contains("planted private bead title")
            && !stdout.contains("planted private bv title")
            && !stdout.contains("src/planted-private-path.rs"),
        "public collector leaked planted source text",
    )
}

#[cfg(unix)]
#[test]
fn cache_hotset_manifest_real_binary_missing_authority_abstains() -> TestResult {
    let (workspace, git_stub, bv_stub) = prepare_hotset_workspace()?;
    let tracker = workspace.path().join(".beads").join("issues.jsonl");
    fs::write(
        &tracker,
        b"{\"id\":\"bd-planted-bead\",\"title\":\"planted private bead title\",\"status\":\"open\",\"issue_type\":\"task\",\"priority\":1}\n",
    )
    .map_err(|error| format!("write {}: {error}", tracker.display()))?;
    let missing_authority = workspace.path().join("missing-authority.json");

    let output = run_public_hotset_cli(workspace.path(), &git_stub, &bv_stub, &missing_authority)?;
    ensure(
        output.status.success(),
        format!(
            "authority-unavailable collector failed: stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let response: JsonValue = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("authority-unavailable stdout was not JSON: {error}"))?;
    ensure(
        response["data"]["plan"]["inputSignalCount"] == 0,
        "missing authority must suppress every collected source signal",
    )?;
    ensure(
        response["data"]["plan"]["candidateCount"] == 0,
        "missing authority must produce no prewarm candidates",
    )?;
    ensure(
        response["degraded"].as_array().is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry["code"] == "hotset_source_authority_missing")
        }),
        "missing authority degradation was not public",
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure(
        !stdout.contains("planted private bead title")
            && !stdout.contains("planted private bv title")
            && !stdout.contains("src/planted-private-path.rs"),
        "authority-unavailable response leaked planted source text",
    )
}
