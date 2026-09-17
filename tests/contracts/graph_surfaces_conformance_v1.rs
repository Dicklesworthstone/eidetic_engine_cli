//! bd-2rk38 — conformance harness for `ee insights` graph-derived JSON
//! surfaces.
//!
//! The bead asks for per-section coverage over the agent-facing graph
//! insights surfaces (bridges, knowledgeSkyline, causalBottlenecks,
//! proximityHotspots, hubs, authorities) plus the `ee proximity`
//! pairwise report. The contract being pinned:
//!
//! 1. Each `ee insights --section <name> --json` invocation returns the
//!    response envelope (`schema == "ee.response.v2"`).
//! 2. `data.selectedSection` echoes the requested section name (no
//!    section-name silent rewrite under any input shape).
//! 3. `data.degradedSignals` is an array (may be empty, may carry
//!    workspace-empty markers — both are valid envelope shapes).
//! 4. Two consecutive cold-process invocations against the same
//!    workspace produce byte-identical stdout — the cross-process
//!    determinism contract that backs caching, memoization, and
//!    pack-stream conformance further downstream.
//!
//! `ee proximity` is already deeply covered by
//! tests/graph_neighborhood_smoke.rs:1368 (`proximity_json_reports
//! _min_cut_for_seeded_memory_pair`, which asserts the schema, all
//! required fields, and 3-cold-process byte-identity), and by
//! tests/response_envelope_conformance_matrix.rs:152 (schema-document
//! validity). This file does NOT duplicate that coverage; it focuses
//! on the per-section envelope gap that no harness currently pins
//! (only causalBottlenecks via tests/property_pack_metamorphic.rs:585
//! MR6 and proximityHotspots via graph_neighborhood_smoke.rs:1466
//! are byte-identity-covered today).
//!
//! Scope deferred to follow-up sub-beads:
//!  - Full JSON Schema validation of every section's `data.sections[].
//!    items[]` shape against per-section sub-schemas (would need a
//!    JSON Schema validator dep that isn't in the tree yet).
//!  - The remaining 7 insights sections not called out in the bead
//!    (authorities, comprehensiveRules, contradictionClusters, kCore,
//!    kTruss, loadBearingMemories, revisionFrontiers, topMemories)
//!    — those are non-graph-derived; bd-2rk38 explicitly scopes to
//!    graph-derived surfaces.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value as JsonValue;

type TestResult = Result<(), String>;

fn target_root() -> PathBuf {
    env::var_os("CARGO_TARGET_TMPDIR")
        .or_else(|| env::var_os("CARGO_TARGET_DIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"))
}

fn unique_workspace(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let workspace = target_root()
        .join("ee-graph-surfaces-conformance-v1")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&workspace)
        .map_err(|error| format!("create workspace {}: {error}", workspace.display()))?;
    Ok(workspace)
}

fn run_ee_with_workspace(workspace: &Path, args: &[&str]) -> Result<Output, String> {
    crate::common_spawn::serialized_real_ee_with(|command| {
        command
            .arg("--workspace")
            .arg(workspace)
            .args(args)
            .env_remove("EE_WORKSPACE")
            .env_remove("EE_WORKSPACE_REGISTRY");
    })
    .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn ee_stdout_string(output: Output, context: &str) -> Result<String, String> {
    if !output.status.success() {
        return Err(format!(
            "{context} failed: exit={:?} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| format!("{context}: stdout not UTF-8: {error}"))
}

fn ee_stdout_bytes(output: Output, context: &str) -> Result<Vec<u8>, String> {
    if !output.status.success() {
        return Err(format!(
            "{context} failed: exit={:?} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    Ok(output.stdout)
}

fn run_ee_json(workspace: &Path, args: &[&str], context: &str) -> Result<JsonValue, String> {
    let stdout = ee_stdout_string(run_ee_with_workspace(workspace, args)?, context)?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{context}: stdout not JSON: {error}\nstdout: {stdout}"))
}

fn run_ee_bytes(workspace: &Path, args: &[&str], context: &str) -> Result<Vec<u8>, String> {
    ee_stdout_bytes(run_ee_with_workspace(workspace, args)?, context)
}

/// Seed a fresh workspace with a small connected memory graph so that
/// every graph-derived insights section has at least one observable
/// vertex/edge to operate over. The exact seed shape is not part of
/// the contract — only the envelope shape is — but a non-empty graph
/// exercises more interesting code paths than an empty-workspace
/// degradation.
fn seed_graph_workspace(workspace: &Path) -> TestResult {
    run_ee_json(workspace, &["init", "--json"], "ee init")?;

    let seeds = [
        ("Insights conformance memory alpha.", "alpha"),
        ("Insights conformance memory beta.", "beta"),
        ("Insights conformance memory gamma.", "gamma"),
    ];

    let mut ids: Vec<String> = Vec::new();
    for (body, tag) in seeds {
        let envelope = run_ee_json(
            workspace,
            &[
                "remember",
                body,
                "--level",
                "procedural",
                "--kind",
                "rule",
                "--tags",
                tag,
                "--json",
            ],
            "ee remember seed",
        )?;
        let id = envelope
            .pointer("/data/memory_id")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| format!("seed: memory_id missing from envelope: {envelope}"))?
            .to_owned();
        ids.push(id);
    }

    // Wire alpha→beta and beta→gamma so the graph has at least one
    // bridge candidate and at least one articulation candidate
    // structure. `ee link` may reject the chosen relation on future
    // dictionary tightenings — if so, leave the workspace as a
    // disconnected node set (each section will still emit a valid
    // envelope, possibly with an empty-workspace degradation, which
    // is itself a valid envelope shape the contract pins).
    for window in ids.windows(2) {
        let _ = run_ee_with_workspace(
            workspace,
            &[
                "link",
                window[0].as_str(),
                window[1].as_str(),
                "--relation",
                "supports",
            ],
        );
    }

    Ok(())
}

/// Canonical list of graph-derived insights sections covered by
/// bd-2rk38. The bead names "hits" in shorthand for the family
/// {hubs, authorities}; we expand to the canonical CLI-accepted
/// names per `src/cli/mod.rs:50251`'s available-section list.
/// `articulationPoints` from the bead is its own subcommand
/// (`ee articulation`), not an `ee insights --section`; out-of-scope
/// for this harness and noted in the module-level doc as deferred.
const GRAPH_DERIVED_INSIGHTS_SECTIONS: &[&str] = &[
    "bridges",
    "knowledgeSkyline",
    "causalBottlenecks",
    "proximityHotspots",
    "hubs",
    "authorities",
];

/// Per-section conformance: the response envelope shape and the
/// section-name echo are pinned regardless of whether the underlying
/// graph data is sparse, dense, or empty.
fn assert_insights_section_envelope(workspace: &Path, section: &str) -> TestResult {
    let envelope = run_ee_json(
        workspace,
        &["insights", "--section", section, "--json"],
        &format!("ee insights --section {section}"),
    )?;

    let schema = envelope
        .get("schema")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("section {section}: missing top-level `schema` field"))?;
    if schema != "ee.response.v2" {
        return Err(format!(
            "section {section}: schema must be ee.response.v2; got {schema:?}",
        ));
    }

    let data = envelope
        .get("data")
        .ok_or_else(|| format!("section {section}: missing top-level `data` field"))?;
    let selected = data
        .get("selectedSection")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("section {section}: data.selectedSection missing"))?;
    if selected != section {
        return Err(format!(
            "section {section}: data.selectedSection must echo requested section; got {selected:?}",
        ));
    }

    // `degradedSignals` may be absent on some envelope shapes; when
    // present, it MUST be an array. Either is contract-valid; a
    // string/object/scalar in this slot would be a breakage.
    if let Some(degraded) = data.get("degradedSignals") {
        if !degraded.is_array() {
            return Err(format!(
                "section {section}: data.degradedSignals must be an array when present; got {degraded}",
            ));
        }
    }

    Ok(())
}

/// Per-section determinism: two cold-process invocations against the
/// same workspace produce byte-identical stdout. This pins the
/// cross-process reproducibility contract that downstream pack-stream
/// and caching paths depend on.
fn assert_insights_section_byte_identical(workspace: &Path, section: &str) -> TestResult {
    let args = ["insights", "--section", section, "--json"];
    let run1 = run_ee_bytes(workspace, &args, &format!("ee insights {section} #1"))?;
    let run2 = run_ee_bytes(workspace, &args, &format!("ee insights {section} #2"))?;
    if run1 != run2 {
        return Err(format!(
            "section {section}: stdout byte-identity broken across cold processes\n{}",
            describe_byte_divergence(&run1, &run2),
        ));
    }
    Ok(())
}

/// Describe how two supposedly-identical stdout captures diverge.
///
/// A bare length pair cannot be acted on: it reports that the determinism
/// contract broke without reporting what moved, so the next reader has to
/// reproduce the run to learn anything. This names the first differing byte,
/// shows the surrounding text from both runs, and -- when both captures parse
/// as JSON -- lists the specific pointers whose values differ. A failing run
/// then carries its own diagnosis.
fn describe_byte_divergence(left: &[u8], right: &[u8]) -> String {
    let first_diff = left
        .iter()
        .zip(right.iter())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| left.len().min(right.len()));

    let mut detail = format!(
        "  first difference at byte {first_diff} (lengths {}/{})",
        left.len(),
        right.len()
    );

    let start = first_diff.saturating_sub(60);
    detail.push_str(&format!(
        "\n  run1: {}\n  run2: {}",
        String::from_utf8_lossy(&left[start..(first_diff + 60).min(left.len())]),
        String::from_utf8_lossy(&right[start..(first_diff + 60).min(right.len())]),
    ));

    if let (Ok(left_json), Ok(right_json)) = (
        serde_json::from_slice::<JsonValue>(left),
        serde_json::from_slice::<JsonValue>(right),
    ) {
        let mut pointers = Vec::new();
        collect_json_divergences(&left_json, &right_json, "", &mut pointers);
        if !pointers.is_empty() {
            detail.push_str("\n  differing JSON pointers:");
            for pointer in pointers.iter().take(8) {
                detail.push_str(&format!("\n    {pointer}"));
            }
            if pointers.len() > 8 {
                detail.push_str(&format!("\n    ... and {} more", pointers.len() - 8));
            }
        }
    }

    detail
}

/// Collect JSON pointers whose values differ between two envelopes.
///
/// Capped so that a wholesale divergence reports a readable sample rather than
/// one line per leaf.
fn collect_json_divergences(
    left: &JsonValue,
    right: &JsonValue,
    path: &str,
    out: &mut Vec<String>,
) {
    if left == right || out.len() >= 64 {
        return;
    }

    match (left, right) {
        (JsonValue::Object(left_map), JsonValue::Object(right_map)) => {
            let mut keys: Vec<&String> = left_map.keys().chain(right_map.keys()).collect();
            keys.sort_unstable();
            keys.dedup();
            for key in keys {
                match (left_map.get(key), right_map.get(key)) {
                    (Some(left_value), Some(right_value)) => {
                        collect_json_divergences(
                            left_value,
                            right_value,
                            &format!("{path}/{key}"),
                            out,
                        );
                    }
                    (Some(_), None) => out.push(format!("{path}/{key} (only in run1)")),
                    (None, Some(_)) => out.push(format!("{path}/{key} (only in run2)")),
                    (None, None) => {}
                }
            }
        }
        (JsonValue::Array(left_items), JsonValue::Array(right_items)) => {
            if left_items.len() != right_items.len() {
                out.push(format!(
                    "{path} (array length {} vs {})",
                    left_items.len(),
                    right_items.len()
                ));
                return;
            }
            for (index, (left_item, right_item)) in
                left_items.iter().zip(right_items.iter()).enumerate()
            {
                collect_json_divergences(left_item, right_item, &format!("{path}/{index}"), out);
            }
        }
        _ => out.push(format!(
            "{path} ({} vs {})",
            truncate_json_scalar(left),
            truncate_json_scalar(right)
        )),
    }
}

fn truncate_json_scalar(value: &JsonValue) -> String {
    let rendered = value.to_string();
    if rendered.chars().count() <= 80 {
        return rendered;
    }
    let clipped: String = rendered.chars().take(80).collect();
    format!("{clipped}...")
}

#[test]
fn insights_graph_derived_sections_emit_conformant_envelope() -> TestResult {
    let workspace = unique_workspace("envelope")?;
    seed_graph_workspace(&workspace)?;

    for section in GRAPH_DERIVED_INSIGHTS_SECTIONS {
        assert_insights_section_envelope(&workspace, section)?;
    }
    Ok(())
}

#[test]
fn insights_graph_derived_sections_are_byte_identical_across_cold_runs() -> TestResult {
    let workspace = unique_workspace("byte-identity")?;
    seed_graph_workspace(&workspace)?;

    for section in GRAPH_DERIVED_INSIGHTS_SECTIONS {
        assert_insights_section_byte_identical(&workspace, section)?;
    }
    Ok(())
}

#[test]
fn insights_unknown_section_returns_usage_error_envelope() -> TestResult {
    // Negative-path contract: an unknown section name must produce a
    // structured error envelope (ee.error.v2) with a usage code and
    // the canonical available-sections list. This mirrors
    // src/cli/mod.rs:50229 `insights_unknown_section_json_is_clear`
    // but pins the contract at the public-CLI conformance layer too,
    // so a refactor that moves the validation logic without updating
    // the user-facing message would be caught here.
    let workspace = unique_workspace("unknown")?;
    run_ee_json(&workspace, &["init", "--json"], "ee init")?;

    let output = run_ee_with_workspace(
        &workspace,
        &[
            "--json",
            "insights",
            "--section",
            "definitely-not-a-real-section",
        ],
    )?;

    // Unknown section returns a non-success exit; do NOT use the
    // success-only helpers here. Parse stdout directly.
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("unknown-section stdout not UTF-8: {error}"))?;
    let envelope: JsonValue = serde_json::from_str(&stdout)
        .map_err(|error| format!("unknown-section stdout not JSON: {error}\nstdout: {stdout}"))?;

    let schema = envelope
        .get("schema")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("unknown-section: missing schema field: {envelope}"))?;
    if schema != "ee.error.v2" {
        return Err(format!(
            "unknown-section: schema must be ee.error.v2; got {schema:?}",
        ));
    }
    let code = envelope
        .pointer("/error/code")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("unknown-section: error.code missing: {envelope}"))?;
    if code != "usage" {
        return Err(format!(
            "unknown-section: error.code must be `usage`; got {code:?}",
        ));
    }
    let repair = envelope
        .pointer("/error/repair")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("unknown-section: error.repair missing: {envelope}"))?;
    if repair != "ee insights --help" {
        return Err(format!(
            "unknown-section: error.repair must be actionable; got {repair:?}",
        ));
    }
    let repair_kind = envelope
        .pointer("/error/repairKind")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("unknown-section: error.repairKind missing: {envelope}"))?;
    if repair_kind != "actionable" {
        return Err(format!(
            "unknown-section: error.repairKind must be actionable; got {repair_kind:?}",
        ));
    }
    Ok(())
}
