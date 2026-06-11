//! Per-surface output-token governor contract (ADR 0063, bd-7lvbg.3).
//!
//! Proves the wired agent surfaces honor the governor contract end to end
//! against the real binary:
//!
//! - A tight `--max-output-tokens` ceiling truncates at the declared
//!   truncation point and reports `output_truncated_budget` with
//!   `details.droppedCount` + `details.continuationCursor`.
//! - `--cursor` resume drains a page sequence that reconstructs the full
//!   untruncated result set exactly once — no duplicates, no gaps.
//! - A rejected cursor yields an EMPTY page plus `cursor_invalid` /
//!   `cursor_stale`, never a restarted page.
//! - Pack `data.pack.items[]` is NEVER a registered truncation point (hard
//!   rule: pack content is governed solely by its own `--max-tokens`
//!   contract).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value as JsonValue;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn isolated_workspace(label: &str) -> Result<PathBuf, String> {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock before unix epoch: {error}"))?
        .as_nanos();
    let path = env::temp_dir().join(format!(
        "ee-governor-contract-{label}-{}-{suffix}",
        std::process::id()
    ));
    fs::create_dir_all(&path).map_err(|error| {
        format!(
            "failed to create temp workspace {}: {error}",
            path.display()
        )
    })?;
    Ok(path)
}

fn run_ee_in(workspace: &Path, args: &[&str]) -> Result<JsonValue, String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--workspace")
        .arg(workspace)
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure(
        output.status.success(),
        format!(
            "ee {} should succeed; stderr: {}\nstdout: {stdout}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    serde_json::from_str(&stdout).map_err(|error| {
        format!(
            "ee {} stdout is not valid JSON: {error}\n{stdout}",
            args.join(" ")
        )
    })
}

/// Collect every degraded entry regardless of placement (`degraded[]` or
/// `data.degraded[]` — surfaces differ).
fn degraded_entries(value: &JsonValue) -> Vec<JsonValue> {
    let mut entries = Vec::new();
    for pointer in ["/degraded", "/data/degraded"] {
        if let Some(array) = value.pointer(pointer).and_then(JsonValue::as_array) {
            entries.extend(array.iter().cloned());
        }
    }
    entries
}

fn degraded_entry_with_code(value: &JsonValue, code: &str) -> Option<JsonValue> {
    degraded_entries(value)
        .into_iter()
        .find(|entry| entry.get("code").and_then(JsonValue::as_str) == Some(code))
}

fn continuation_cursor(value: &JsonValue) -> Option<String> {
    degraded_entries(value).iter().find_map(|entry| {
        entry
            .pointer("/details/continuationCursor")
            .and_then(JsonValue::as_str)
            .map(str::to_owned)
    })
}

/// Element ids at a flat truncation point under `data`.
fn element_ids(value: &JsonValue, array_pointer: &str, id_field: &str) -> Vec<String> {
    value
        .pointer(array_pointer)
        .and_then(JsonValue::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get(id_field))
                .filter_map(JsonValue::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Drain a governed page sequence to exhaustion and return the concatenated
/// element ids. `base_args` must NOT contain `--cursor`.
fn drain_ids(
    workspace: &Path,
    base_args: &[&str],
    array_pointer: &str,
    id_field: &str,
) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    let mut cursor: Option<String> = None;
    for page in 0..32 {
        let mut args: Vec<&str> = base_args.to_vec();
        let token;
        if let Some(current) = &cursor {
            token = current.clone();
            args.push("--cursor");
            args.push(Box::leak(token.into_boxed_str()));
        }
        let value = run_ee_in(workspace, &args)?;
        let page_ids = element_ids(&value, array_pointer, id_field);
        ensure(
            cursor.is_none() || degraded_entry_with_code(&value, "cursor_invalid").is_none(),
            format!("page {page} rejected its own cursor as cursor_invalid"),
        )?;
        ensure(
            cursor.is_none() || degraded_entry_with_code(&value, "cursor_stale").is_none(),
            format!("page {page} rejected its own cursor as cursor_stale"),
        )?;
        ids.extend(page_ids);
        match continuation_cursor(&value) {
            Some(next) => cursor = Some(next),
            None => return Ok(ids),
        }
    }
    Err("page sequence failed to terminate within 32 pages".to_string())
}

fn assert_exact_partition(drained: &[String], full: &[String], surface: &str) -> TestResult {
    ensure(
        drained == full,
        format!(
            "{surface}: drained page sequence must reconstruct the full result set exactly \
             once in order.\nfull ({}): {full:?}\ndrained ({}): {drained:?}",
            full.len(),
            drained.len()
        ),
    )
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("governor")
        .join(format!("{name}.golden"))
}

fn assert_golden(name: &str, actual: &str) -> TestResult {
    let path = golden_path(name);
    if env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("failed to create dir: {e}"))?;
        }
        fs::write(&path, actual).map_err(|e| format!("failed to write golden: {e}"))?;
        eprintln!("Updated golden file: {}", path.display());
        return Ok(());
    }
    let expected = fs::read_to_string(&path).map_err(|e| {
        format!(
            "Golden file not found: {}\nRun with UPDATE_GOLDEN=1 to create it.\nError: {e}",
            path.display()
        )
    })?;
    ensure(
        expected == actual,
        format!(
            "Golden test '{name}' failed.\nGolden file: {}\nRun with UPDATE_GOLDEN=1 to \
             update.\n--- expected\n{expected}\n+++ actual\n{actual}",
            path.display()
        ),
    )
}

/// Scrub size-dependent numerics so the goldens pin the envelope SHAPE
/// without churning when the schema registry grows: token estimates and the
/// estimate-derived repair hint become placeholders.
fn normalize_governor_envelope(value: &JsonValue) -> Result<String, String> {
    let mut normalized = value.clone();
    if let Some(meta) = normalized
        .pointer_mut("/meta/tokensEstimated")
        .filter(|estimated| estimated.is_u64())
    {
        *meta = JsonValue::String("<tokens-estimated>".to_owned());
    }
    if let Some(entries) = normalized
        .get_mut("degraded")
        .and_then(JsonValue::as_array_mut)
    {
        for entry in entries {
            for field in ["message", "repair"] {
                if let Some(text) = entry.get(field).and_then(JsonValue::as_str) {
                    let scrubbed: String = text
                        .split_whitespace()
                        .map(|word| {
                            if word.chars().all(|c| c.is_ascii_digit()) {
                                "<n>"
                            } else {
                                word
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    entry[field] = JsonValue::String(scrubbed);
                }
            }
        }
    }
    serde_json::to_string_pretty(&normalized).map_err(|error| format!("serialize: {error}"))
}

#[test]
fn schema_list_rejected_cursor_envelope_matches_golden() -> TestResult {
    let workspace = isolated_workspace("schema-golden-invalid")?;
    let value = run_ee_in(
        &workspace,
        &[
            "schema",
            "list",
            "--cursor",
            "not-a-valid-cursor",
            "--max-output-tokens",
            "600",
            "--json",
        ],
    )?;
    assert_golden(
        "schema_list_cursor_invalid_empty_page",
        &normalize_governor_envelope(&value)?,
    )
}

#[test]
fn schema_list_unsatisfiable_shell_matches_golden() -> TestResult {
    let workspace = isolated_workspace("schema-golden-floor")?;
    let value = run_ee_in(
        &workspace,
        &["schema", "list", "--max-output-tokens", "50", "--json"],
    )?;
    assert_golden(
        "schema_list_unsatisfiable_shell",
        &normalize_governor_envelope(&value)?,
    )
}

fn seed_memories(workspace: &Path, count: usize) -> TestResult {
    let init = Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--workspace")
        .arg(workspace)
        .arg("init")
        .output()
        .map_err(|error| format!("failed to run ee init: {error}"))?;
    ensure(
        init.status.success(),
        format!(
            "ee init should succeed: {}",
            String::from_utf8_lossy(&init.stderr)
        ),
    )?;
    for index in 0..count {
        let body = format!(
            "Governor contract seed memory {index:02}: deterministic retrieval corpus row \
             about the release workflow and clippy gating conventions."
        );
        let output = Command::new(env!("CARGO_BIN_EXE_ee"))
            .arg("--workspace")
            .arg(workspace)
            .arg("remember")
            .arg(&body)
            .arg("--level")
            .arg("semantic")
            .arg("--kind")
            .arg("fact")
            .arg("--tags")
            .arg("governor,contract")
            .output()
            .map_err(|error| format!("failed to run ee remember: {error}"))?;
        ensure(
            output.status.success(),
            format!(
                "ee remember {index} should succeed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        )?;
    }
    Ok(())
}

// ============================================================================
// schema list — the no-DB demonstration surface
// ============================================================================

#[test]
fn schema_list_tight_ceiling_truncates_with_cursor() -> TestResult {
    // The envelope minimum for a truncated page (shell + one schema entry +
    // the output_truncated_budget entry carrying the ~200-char cursor token)
    // sits around 400 tokens; 600 reliably truncates without hitting the
    // output_budget_unsatisfiable floor.
    const CEILING: u64 = 600;
    let workspace = isolated_workspace("schema-ceiling")?;
    let full = run_ee_in(&workspace, &["schema", "list", "--json"])?;
    let full_ids = element_ids(&full, "/data/schemas", "id");
    ensure(
        full_ids.len() > 3,
        "schema registry must hold more than 3 schemas for this contract",
    )?;

    let governed = run_ee_in(
        &workspace,
        &["schema", "list", "--max-output-tokens", "600", "--json"],
    )?;
    let kept = element_ids(&governed, "/data/schemas", "id");
    ensure(
        !kept.is_empty() && kept.len() < full_ids.len(),
        format!(
            "ceiling {CEILING} must keep a strict non-empty prefix (kept {} of {})",
            kept.len(),
            full_ids.len()
        ),
    )?;
    ensure(
        kept == full_ids[..kept.len()],
        "governed output must be a prefix of the ungoverned output",
    )?;

    let entry = degraded_entry_with_code(&governed, "output_truncated_budget")
        .ok_or("truncated response must report output_truncated_budget")?;
    let dropped = entry
        .pointer("/details/droppedCount")
        .and_then(JsonValue::as_u64)
        .ok_or("details.droppedCount missing")?;
    ensure(
        dropped == (full_ids.len() - kept.len()) as u64,
        format!("droppedCount {dropped} must equal elided element count"),
    )?;
    entry
        .pointer("/details/continuationCursor")
        .and_then(JsonValue::as_str)
        .ok_or("details.continuationCursor missing")?;
    let estimated = governed
        .pointer("/meta/tokensEstimated")
        .and_then(JsonValue::as_u64)
        .ok_or("meta.tokensEstimated must be stamped under a ceiling")?;
    ensure(
        estimated <= CEILING,
        format!("stamped estimate {estimated} exceeds the {CEILING}-token ceiling"),
    )
}

#[test]
fn schema_list_cursor_drain_partitions_exactly() -> TestResult {
    let workspace = isolated_workspace("schema-drain")?;
    let full = run_ee_in(&workspace, &["schema", "list", "--json"])?;
    let full_ids = element_ids(&full, "/data/schemas", "id");
    let drained = drain_ids(
        &workspace,
        &["schema", "list", "--max-output-tokens", "600", "--json"],
        "/data/schemas",
        "id",
    )?;
    assert_exact_partition(&drained, &full_ids, "schema list")
}

#[test]
fn schema_list_invalid_cursor_is_an_empty_page() -> TestResult {
    let workspace = isolated_workspace("schema-invalid")?;
    let value = run_ee_in(
        &workspace,
        &[
            "schema",
            "list",
            "--cursor",
            "not-a-valid-cursor",
            "--max-output-tokens",
            "150",
            "--json",
        ],
    )?;
    ensure(
        element_ids(&value, "/data/schemas", "id").is_empty(),
        "an invalid cursor must yield an empty page, never a restarted one",
    )?;
    let entry = degraded_entry_with_code(&value, "cursor_invalid")
        .ok_or("rejected cursor must report cursor_invalid")?;
    ensure(
        entry.get("severity").and_then(JsonValue::as_str) == Some("low"),
        "cursor_invalid must be severity low",
    )
}

// ============================================================================
// search
// ============================================================================

#[test]
fn search_cursor_drain_partitions_exactly() -> TestResult {
    let workspace = isolated_workspace("search-drain")?;
    seed_memories(&workspace, 8)?;
    let full = run_ee_in(
        &workspace,
        &[
            "search",
            "release workflow clippy",
            "--limit",
            "8",
            "--json",
        ],
    )?;
    let full_ids = element_ids(&full, "/data/results", "docId");
    ensure(
        full_ids.len() >= 4,
        format!(
            "seeded search must return >= 4 results, got {}",
            full_ids.len()
        ),
    )?;
    let drained = drain_ids(
        &workspace,
        &[
            "search",
            "release workflow clippy",
            "--limit",
            "8",
            "--max-output-tokens",
            "2500",
            "--json",
        ],
        "/data/results",
        "docId",
    )?;
    assert_exact_partition(&drained, &full_ids, "search")
}

// ============================================================================
// memory list
// ============================================================================

#[test]
fn memory_list_cursor_drain_partitions_exactly() -> TestResult {
    let workspace = isolated_workspace("memlist-drain")?;
    seed_memories(&workspace, 8)?;
    let full = run_ee_in(&workspace, &["memory", "list", "--limit", "8", "--json"])?;
    let full_ids = element_ids(&full, "/data/memories", "id");
    ensure(
        full_ids.len() == 8,
        format!(
            "memory list must return the 8 seeded rows, got {}",
            full_ids.len()
        ),
    )?;
    let drained = drain_ids(
        &workspace,
        &[
            "memory",
            "list",
            "--limit",
            "8",
            "--max-output-tokens",
            "800",
            "--json",
        ],
        "/data/memories",
        "id",
    )?;
    assert_exact_partition(&drained, &full_ids, "memory list")
}

// ============================================================================
// audit timeline (ee.cursor.v1 codec swap)
// ============================================================================

#[test]
fn audit_timeline_cursor_drain_partitions_exactly() -> TestResult {
    let workspace = isolated_workspace("audit-drain")?;
    seed_memories(&workspace, 6)?;
    // audit timeline emits a top-level ee.audit.timeline.v1 report (not an
    // ee.response.v2 envelope), so the output governor passes it through and
    // its --cursor is the query-level pagination lane on the shared codec.
    let full = run_ee_in(
        &workspace,
        &["audit", "timeline", "--limit", "100", "--json"],
    )?;
    let full_ids = element_ids(&full, "/entries", "id");
    ensure(
        full_ids.len() >= 6,
        format!("seeding must leave >= 6 audit rows, got {}", full_ids.len()),
    )?;

    // Query-level pagination drain: follow data.pagination.next_cursor.
    let mut drained: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..32 {
        let mut args = vec!["audit", "timeline", "--limit", "2", "--json"];
        let token;
        if let Some(current) = &cursor {
            token = current.clone();
            args.push("--cursor");
            args.push(Box::leak(token.into_boxed_str()));
        }
        let value = run_ee_in(&workspace, &args)?;
        drained.extend(element_ids(&value, "/entries", "id"));
        let next = value
            .pointer("/pagination/next_cursor")
            .and_then(JsonValue::as_str)
            .map(str::to_owned);
        match next {
            Some(next) => {
                ensure(
                    next.contains('.'),
                    format!("next_cursor must be an ee.cursor.v1 wire token, got {next:?}"),
                )?;
                cursor = Some(next);
            }
            None => break,
        }
    }
    assert_exact_partition(&drained, &full_ids, "audit timeline")
}

#[test]
fn audit_timeline_legacy_offset_cursor_is_an_empty_cursor_invalid_page() -> TestResult {
    let workspace = isolated_workspace("audit-legacy")?;
    seed_memories(&workspace, 3)?;
    let value = run_ee_in(
        &workspace,
        &[
            "audit", "timeline", "--limit", "2", "--cursor", "2", "--json",
        ],
    )?;
    ensure(
        element_ids(&value, "/entries", "id").is_empty(),
        "a legacy bare-offset cursor must yield an empty page",
    )?;
    ensure(
        degraded_entry_with_code(&value, "cursor_invalid").is_some(),
        "legacy cursors must report cursor_invalid",
    )
}

// ============================================================================
// insights (per-section round-robin)
// ============================================================================

#[test]
fn insights_cursor_drain_never_duplicates_section_items() -> TestResult {
    let workspace = isolated_workspace("insights-drain")?;
    seed_memories(&workspace, 8)?;

    fn section_item_keys(value: &JsonValue) -> Vec<String> {
        value
            .pointer("/data/sections")
            .and_then(JsonValue::as_array)
            .map(|sections| {
                sections
                    .iter()
                    .flat_map(|section| {
                        let section_name = section
                            .get("name")
                            .and_then(JsonValue::as_str)
                            .unwrap_or("?")
                            .to_owned();
                        section
                            .get("items")
                            .and_then(JsonValue::as_array)
                            .map(|items| {
                                items
                                    .iter()
                                    .map(|item| {
                                        let id = item
                                            .get("id")
                                            .or_else(|| item.get("memoryId"))
                                            .map(std::string::ToString::to_string)
                                            .unwrap_or_else(|| item.to_string());
                                        format!("{section_name}:{id}")
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    let full = run_ee_in(&workspace, &["insights", "--json"])?;
    let full_keys = section_item_keys(&full);
    if full_keys.len() < 4 {
        // A tiny corpus can yield too few section items to truncate; the
        // per-section engine semantics are pinned by unit tests — this
        // contract run only proves the wired surface when there is enough
        // material to page.
        return Ok(());
    }

    let mut drained: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for page in 0..32 {
        let mut args = vec!["insights", "--max-output-tokens", "2500", "--json"];
        let token;
        if let Some(current) = &cursor {
            token = current.clone();
            args.push("--cursor");
            args.push(Box::leak(token.into_boxed_str()));
        }
        let value = run_ee_in(&workspace, &args)?;
        ensure(
            cursor.is_none()
                || (degraded_entry_with_code(&value, "cursor_invalid").is_none()
                    && degraded_entry_with_code(&value, "cursor_stale").is_none()),
            format!("insights page {page} rejected its own cursor"),
        )?;
        drained.extend(section_item_keys(&value));
        match continuation_cursor(&value) {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    let mut sorted_drained = drained.clone();
    sorted_drained.sort();
    sorted_drained.dedup();
    ensure(
        sorted_drained.len() == drained.len(),
        "insights page sequence must never duplicate a section item",
    )?;
    let mut sorted_full = full_keys.clone();
    sorted_full.sort();
    let mut resorted_drained = drained;
    resorted_drained.sort();
    ensure(
        resorted_drained == sorted_full,
        format!(
            "insights drain must cover every section item exactly once \
             (full {} vs drained {})",
            sorted_full.len(),
            resorted_drained.len()
        ),
    )
}

// ============================================================================
// pack hard rule — items[] is never a registered truncation point
// ============================================================================

#[test]
fn pack_items_are_never_a_registered_truncation_point() -> TestResult {
    use ee::output::OUTPUT_TRUNCATION_REGISTRY;

    let pack_entry = OUTPUT_TRUNCATION_REGISTRY
        .iter()
        .find(|point| point.command == "pack")
        .ok_or("pack must declare a truncation point")?;
    ensure(
        pack_entry.array_path == ["pack", "skipped"],
        format!(
            "pack's declared truncation point must be data.pack.skipped, got {:?}",
            pack_entry.array_path
        ),
    )?;
    ensure(
        !OUTPUT_TRUNCATION_REGISTRY
            .iter()
            .any(|point| point.array_path.last() == Some(&"items") && point.command == "pack"),
        "data.pack.items[] must NEVER be governor-truncated (hard rule)",
    )
}

// ============================================================================
// capabilities feature detection
// ============================================================================

#[test]
fn capabilities_advertises_governor_feature_detection() -> TestResult {
    let workspace = isolated_workspace("capabilities-governor")?;
    let value = run_ee_in(&workspace, &["capabilities", "--json"])?;
    let governor = value
        .pointer("/data/output/governor")
        .ok_or("data.output.governor must be advertised (ADR 0063 §4)")?;
    ensure(
        governor.pointer("/available").and_then(JsonValue::as_bool) == Some(true),
        "governor must advertise availability",
    )?;
    ensure(
        governor.pointer("/ceilingFlag").and_then(JsonValue::as_str) == Some("--max-output-tokens"),
        "governor must name the ceiling flag",
    )?;
    ensure(
        governor.pointer("/ceilingEnv").and_then(JsonValue::as_str) == Some("EE_MAX_OUTPUT_TOKENS"),
        "governor must name the env mirror",
    )?;
    ensure(
        governor
            .pointer("/cursorSchema")
            .and_then(JsonValue::as_str)
            == Some("ee.cursor.v1"),
        "governor must name the cursor schema",
    )?;
    let points = governor
        .pointer("/truncationPoints")
        .and_then(JsonValue::as_array)
        .ok_or("governor must publish the truncation-point table")?;
    ensure(
        points.len() >= 9,
        format!("expected >= 9 truncation points, got {}", points.len()),
    )?;
    let pack_point = points
        .iter()
        .find(|point| point.get("command").and_then(JsonValue::as_str) == Some("pack"))
        .ok_or("pack must declare its truncation point")?;
    ensure(
        pack_point.get("arrayPath").and_then(JsonValue::as_str) == Some("data.pack.skipped"),
        "pack's advertised truncation point must be data.pack.skipped (items[] hard rule)",
    )?;
    let search_point = points
        .iter()
        .find(|point| point.get("command").and_then(JsonValue::as_str) == Some("search"))
        .ok_or("search must declare its truncation point")?;
    ensure(
        search_point
            .get("positionKeyField")
            .and_then(JsonValue::as_str)
            == Some("docId"),
        "search's advertised position key must be docId",
    )
}

// ============================================================================
// curate candidates
// ============================================================================

#[test]
fn curate_candidates_accepts_cursor_flag() -> TestResult {
    // Candidate seeding is heavyweight (session review / agentsmd import);
    // this contract pins the wired flag surface: an invalid cursor must be
    // honestly rejected (empty page + cursor_invalid), proving the resume
    // lane is wired rather than silently ignored.
    let workspace = isolated_workspace("curate-cursor")?;
    seed_memories(&workspace, 1)?;
    let value = run_ee_in(
        &workspace,
        &[
            "curate",
            "candidates",
            "--cursor",
            "not-a-valid-cursor",
            "--max-output-tokens",
            "400",
            "--json",
        ],
    )?;
    ensure(
        element_ids(&value, "/data/candidates", "id").is_empty(),
        "an invalid cursor must yield an empty candidates page",
    )?;
    ensure(
        degraded_entry_with_code(&value, "cursor_invalid").is_some(),
        "curate candidates must report cursor_invalid for a rejected cursor",
    )
}

// ============================================================================
// Tight-ceiling envelope goldens (bd-7lvbg.4)
//
// Each golden pins the SHAPE of a governed surface — field set/order,
// degraded-entry structure, repair guidance, meta stamp — after a deep
// scrub of everything volatile: ids, timestamps, hashes, cursors,
// paths, versions, float scores, token estimates, and the kept-array
// tails (ULID tokenization noise can shift how many elements a fixed
// ceiling keeps, so the tail length must not be load-bearing). The
// scrub leaves nothing host-specific, so UPDATE_GOLDEN regen is safe
// on any platform.
// ============================================================================

/// Replace volatile leaves in place. Keys are matched by exact name or
/// by the id/hash/timestamp suffix conventions of the JSON contracts;
/// all floating-point numbers are score-like and scrubbed wholesale.
fn scrub_volatile_leaves(value: &mut JsonValue) {
    const VOLATILE_EXACT: &[&str] = &[
        "id",
        "timestamp",
        "version",
        "tokensEstimated",
        "droppedCount",
        "continuationCursor",
        "next_cursor",
        "nextCursor",
        "workspacePath",
        "databasePath",
        "indexPath",
        "actor",
        "durationMs",
        "elapsedMs",
        "tookMs",
        "valid_from",
        "valid_to",
        "validFrom",
        "validTo",
    ];
    fn is_volatile_key(key: &str) -> bool {
        VOLATILE_EXACT.contains(&key)
            || key.ends_with("Id")
            || key.ends_with("_id")
            || key.ends_with("Hash")
            || key.ends_with("_hash")
            || key.ends_with("At")
            || key.ends_with("_at")
            || key.ends_with("_ts")
            || key.ends_with("Generation")
    }
    match value {
        JsonValue::Object(map) => {
            for (key, child) in map.iter_mut() {
                if is_volatile_key(key) && !child.is_object() && !child.is_array() {
                    *child = match child {
                        JsonValue::Number(_) => JsonValue::from(0),
                        _ => JsonValue::String("<scrubbed>".to_owned()),
                    };
                } else {
                    scrub_volatile_leaves(child);
                }
            }
        }
        JsonValue::Array(items) => {
            for item in items {
                scrub_volatile_leaves(item);
            }
        }
        JsonValue::Number(number) => {
            if number.is_f64() {
                *value = JsonValue::from(0);
            }
        }
        _ => {}
    }
}

/// Keep only the first element of the array at `pointer` (its scrubbed
/// shape is the per-element contract) and collapse the tail into one
/// placeholder, so ceiling-boundary noise in the kept count cannot
/// churn the golden.
fn condense_array_tail(value: &mut JsonValue, pointer: &str) {
    if let Some(array) = value.pointer_mut(pointer).and_then(JsonValue::as_array_mut)
        && array.len() > 1
    {
        array.truncate(1);
        array.push(JsonValue::String(
            "<remaining elements scrubbed>".to_owned(),
        ));
    }
}

/// Deep-scrubbed pretty form of a governed data-surface payload.
fn normalize_surface_golden(
    value: &JsonValue,
    condense_pointers: &[&str],
) -> Result<String, String> {
    let mut normalized = value.clone();
    scrub_volatile_leaves(&mut normalized);
    for pointer in condense_pointers {
        condense_array_tail(&mut normalized, pointer);
    }
    // Degraded message/repair prose embeds counts and ceilings; scrub
    // bare numbers the same way normalize_governor_envelope does.
    for degraded_pointer in ["/degraded", "/data/degraded"] {
        if let Some(entries) = normalized
            .pointer_mut(degraded_pointer)
            .and_then(JsonValue::as_array_mut)
        {
            for entry in entries {
                for field in ["message", "repair"] {
                    if let Some(text) = entry.get(field).and_then(JsonValue::as_str) {
                        let scrubbed: String = text
                            .split_whitespace()
                            .map(|word| {
                                if word.chars().all(|c| c.is_ascii_digit()) {
                                    "<n>"
                                } else {
                                    word
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(" ");
                        entry[field] = JsonValue::String(scrubbed);
                    }
                }
            }
        }
    }
    serde_json::to_string_pretty(&normalized).map_err(|error| format!("serialize: {error}"))
}

#[test]
fn search_tight_ceiling_envelope_matches_golden() -> TestResult {
    let workspace = isolated_workspace("search-golden-ceiling")?;
    seed_memories(&workspace, 8)?;
    let value = run_ee_in(
        &workspace,
        &[
            "search",
            "release workflow clippy",
            "--limit",
            "8",
            "--max-output-tokens",
            "700",
            "--json",
        ],
    )?;
    ensure(
        degraded_entry_with_code(&value, "output_truncated_budget").is_some()
            || degraded_entry_with_code(&value, "output_budget_unsatisfiable").is_some(),
        "a 700-token ceiling must engage the governor on an 8-result search",
    )?;
    assert_golden(
        "search_tight_ceiling_envelope",
        &normalize_surface_golden(&value, &["/data/results"])?,
    )
}

#[test]
fn memory_list_tight_ceiling_envelope_matches_golden() -> TestResult {
    let workspace = isolated_workspace("memlist-golden-ceiling")?;
    seed_memories(&workspace, 8)?;
    let value = run_ee_in(
        &workspace,
        &[
            "memory",
            "list",
            "--limit",
            "8",
            "--max-output-tokens",
            "800",
            "--json",
        ],
    )?;
    ensure(
        degraded_entry_with_code(&value, "output_truncated_budget").is_some()
            || degraded_entry_with_code(&value, "output_budget_unsatisfiable").is_some(),
        "an 800-token ceiling must engage the governor on an 8-row memory list",
    )?;
    assert_golden(
        "memory_list_tight_ceiling_envelope",
        &normalize_surface_golden(&value, &["/data/memories"])?,
    )
}

#[test]
fn insights_rejected_cursor_envelope_matches_golden() -> TestResult {
    // Insights section composition varies with corpus and graph posture,
    // so the deterministic governed shape to pin is the rejected-cursor
    // empty page: every section emptied, cursor_invalid reported, no
    // continuation offered (drain coverage lives in
    // insights_cursor_drain_never_duplicates_section_items).
    let workspace = isolated_workspace("insights-golden-invalid")?;
    seed_memories(&workspace, 4)?;
    let value = run_ee_in(
        &workspace,
        &[
            "insights",
            "--cursor",
            "not-a-valid-cursor",
            "--max-output-tokens",
            "2500",
            "--json",
        ],
    )?;
    ensure(
        degraded_entry_with_code(&value, "cursor_invalid").is_some(),
        "insights must report cursor_invalid for a rejected cursor",
    )?;
    assert_golden(
        "insights_cursor_invalid_empty_page",
        &normalize_surface_golden(&value, &["/data/sections"])?,
    )
}

#[test]
fn curate_candidates_rejected_cursor_envelope_matches_golden() -> TestResult {
    // Candidate seeding is heavyweight (session review / agentsmd
    // import), so the stable governed shape to pin is the same
    // rejected-cursor empty page the flag contract above asserts.
    let workspace = isolated_workspace("curate-golden-invalid")?;
    seed_memories(&workspace, 1)?;
    let value = run_ee_in(
        &workspace,
        &[
            "curate",
            "candidates",
            "--cursor",
            "not-a-valid-cursor",
            "--max-output-tokens",
            "400",
            "--json",
        ],
    )?;
    ensure(
        degraded_entry_with_code(&value, "cursor_invalid").is_some(),
        "curate candidates must report cursor_invalid for a rejected cursor",
    )?;
    assert_golden(
        "curate_candidates_cursor_invalid_empty_page",
        &normalize_surface_golden(&value, &["/data/candidates"])?,
    )
}

#[test]
fn audit_timeline_paged_report_matches_golden() -> TestResult {
    let workspace = isolated_workspace("audit-golden-page")?;
    seed_memories(&workspace, 6)?;
    let value = run_ee_in(&workspace, &["audit", "timeline", "--limit", "2", "--json"])?;
    ensure(
        value
            .pointer("/pagination/next_cursor")
            .and_then(JsonValue::as_str)
            .is_some(),
        "a 2-row page over >= 6 audit rows must offer a next_cursor",
    )?;
    assert_golden(
        "audit_timeline_paged_report",
        &normalize_surface_golden(&value, &["/entries"])?,
    )
}
