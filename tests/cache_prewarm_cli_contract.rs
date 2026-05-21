//! bd-1zb7k.10.3: CLI contract coverage for `ee cache prewarm`.

use std::ffi::OsString;
use std::fs;
use std::path::Path;

use ee::cache::hotset::{GenerationGate, HotsetBudget, HotsetManifestBuilder};
use ee::models::ProcessExitCode;
use ee::pack::{PackHotsetEntry, PackHotsetEntryKind, PackSection};
use ee::search::SearchHotsetEntry;
use serde_json::Value;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn write_manifest(path: &Path, generation: u64, include_entries: bool) -> TestResult {
    let builder =
        HotsetManifestBuilder::new("ws_01HQTPREWARM00000000000", GenerationGate::new(5, 5))
            .with_profile_tier("standard")
            .with_budget(HotsetBudget::new(128, 8 * 1024 * 1024));

    let manifest = if include_entries {
        let pack_entry = PackHotsetEntry {
            key: "pack:section:prewarm-contract".to_string(),
            kind: PackHotsetEntryKind::PackSection,
            section: Some(PackSection::Evidence),
            generation,
            estimated_bytes: 384,
            hit_count: 2,
            redaction_status: "content_not_stored",
        };
        builder
            .search_entries([
                SearchHotsetEntry::memory("mem_prewarm_contract", generation, 3),
                SearchHotsetEntry::query_shape(
                    "cache prewarm secret should hash only",
                    generation,
                    2,
                )
                .ok_or_else(|| "query shape should normalize".to_string())?,
            ])
            .pack_entries([pack_entry])
            .build()
    } else {
        builder.build()
    };

    fs::write(path, manifest.to_json().to_string())
        .map_err(|error| format!("write manifest: {error}"))
}

fn run_cache_prewarm(args: &[&str]) -> Result<Value, String> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = ee::cli::run(
        args.iter().map(|arg| OsString::from(*arg)),
        &mut stdout,
        &mut stderr,
    );
    ensure(
        exit == ProcessExitCode::Success,
        format!(
            "cache prewarm exit {exit:?}; stderr={}",
            String::from_utf8_lossy(&stderr)
        ),
    )?;
    let stdout = String::from_utf8(stdout).map_err(|error| error.to_string())?;
    serde_json::from_str(&stdout).map_err(|error| format!("parse stdout: {error}; {stdout}"))
}

#[test]
fn cache_prewarm_from_hotset_emits_redaction_safe_report() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let manifest_path = tempdir.path().join("hotset.json");
    write_manifest(&manifest_path, 5, true)?;

    let response = run_cache_prewarm(&[
        "ee",
        "--json",
        "cache",
        "prewarm",
        "--from-hotset",
        manifest_path
            .to_str()
            .ok_or_else(|| "manifest path should be utf8".to_string())?,
        "--profile",
        "standard",
    ])?;

    ensure(
        response["success"].as_bool() == Some(true),
        "success envelope",
    )?;
    let data = &response["data"];
    ensure(
        data["schema"].as_str() == Some("ee.cache.prewarm.v1"),
        "prewarm schema",
    )?;
    ensure(
        data.pointer("/reports/search/schema")
            .and_then(Value::as_str)
            == Some("ee.search.cache_prewarm.v1"),
        "search report schema",
    )?;
    ensure(
        data.pointer("/reports/pack/schema").and_then(Value::as_str)
            == Some("ee.pack.cache_prewarm.v1"),
        "pack report schema",
    )?;
    ensure(
        data.pointer("/admitted/totalEntries")
            .and_then(Value::as_u64)
            .is_some_and(|count| count >= 3),
        "entries should be admitted",
    )?;
    ensure(
        data.pointer("/redactionSafety/summary")
            .and_then(Value::as_str)
            == Some("query_hashes_and_cache_keys_only"),
        "redaction safety summary",
    )?;
    let serialized = data.to_string();
    ensure(
        !serialized.contains("secret should hash only"),
        "raw query text must not leak",
    )
}

#[test]
fn cache_prewarm_requires_current_generation_unless_stale_allowed() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let manifest_path = tempdir.path().join("stale-hotset.json");
    write_manifest(&manifest_path, 5, true)?;

    let response = run_cache_prewarm(&[
        "ee",
        "--json",
        "cache",
        "prewarm",
        "--from-hotset",
        manifest_path
            .to_str()
            .ok_or_else(|| "manifest path should be utf8".to_string())?,
        "--current-generation",
        "8",
    ])?;
    let data = &response["data"];

    ensure(
        data.pointer("/admitted/totalEntries")
            .and_then(Value::as_u64)
            == Some(0),
        "stale generation should admit nothing by default",
    )?;
    ensure(
        data["degraded"].as_array().is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry["code"].as_str() == Some("cache_hotset_stale"))
        }),
        "stale hotset degraded code",
    )
}

#[test]
fn cache_prewarm_empty_hotset_surfaces_no_signal_degraded_code() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let manifest_path = tempdir.path().join("empty-hotset.json");
    write_manifest(&manifest_path, 5, false)?;

    let response = run_cache_prewarm(&[
        "ee",
        "--json",
        "cache",
        "prewarm",
        "--from-hotset",
        manifest_path
            .to_str()
            .ok_or_else(|| "manifest path should be utf8".to_string())?,
    ])?;
    let data = &response["data"];

    ensure(
        data.pointer("/requested/totalEntries")
            .and_then(Value::as_u64)
            == Some(0),
        "empty manifest requested count",
    )?;
    ensure(
        data["degraded"].as_array().is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry["code"].as_str() == Some("hotset_prewarm_no_signals"))
        }),
        "no-signal degraded code",
    )
}
