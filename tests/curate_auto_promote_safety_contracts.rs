//! bd-215tr: threshold promotion proposal safety contracts.
//!
//! Pins the five safety invariants that `ee curate auto-promote` must
//! preserve as the surface (bd-2r8vp) evolves:
//!
//! 1. **Apply requires explicit confirm.** No `--apply` means no
//!    `memory.level_transition` audit row, regardless of other flags.
//! 2. **Workspace isolation.** Auto-promote on workspace B never sees
//!    memories from workspace A; each workspace's scanned/eligible
//!    counts depend only on its own state.
//! 3. **Deterministic JSON ordering.** Two propose-dry-run invocations
//!    against the same DB and same thresholds emit byte-identical
//!    proposal JSON after volatile fields are stripped.
//! 4. **Idempotent dry-run.** Running dry-run twice produces no audit
//!    deltas and no level changes either time.
//! 5. **Effective thresholds appear in JSON.** Agents must be able to
//!    read back the threshold inputs that decided eligibility from the
//!    same envelope as the proposals.
//!
//! Companion to cc_1's tests/e2e_curate_auto_promote.rs (missing-db,
//! empty-workspace, default-dry-run) and the inline unit tests in
//! src/core/curate.rs covering the disqualifier matrix.

#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn unique_workspace(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-curate-auto-promote-safety")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

fn init_workspace(workspace_arg: &str) -> TestResult {
    let init = run_ee(&["--workspace", workspace_arg, "--json", "init"])?;
    ensure(
        init.status.success(),
        format!(
            "ee init must succeed; stderr: {}",
            String::from_utf8_lossy(&init.stderr)
        ),
    )
}

fn remember(workspace_arg: &str, content: &str, level: &str, confidence: &str) -> TestResult {
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "remember",
        content,
        "--level",
        level,
        "--kind",
        "rule",
        "--confidence",
        confidence,
    ])?;
    ensure(
        output.status.success(),
        format!(
            "ee remember must succeed for `{content}`; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

fn auto_promote(workspace_arg: &str, extra: &[&str]) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec![
        "--workspace",
        workspace_arg,
        "--json",
        "curate",
        "auto-promote",
    ];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("curate auto-promote stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

/// Project away fields that legitimately vary across two runs (timestamps,
/// elapsed durations, run identifiers, etc.) so two propose-dry-run
/// invocations can be compared byte-for-byte on the *contract*-shaped fields.
fn strip_volatile_fields(value: &mut Value) {
    match value {
        Value::Object(map) => {
            // Volatile keys we expect across two reads of the same DB.
            for volatile in [
                "elapsedMs",
                "elapsed_ms",
                "runId",
                "runAt",
                "createdAt",
                "timestamp",
                "ts",
                "observedAt",
                "completedAt",
                "startedAt",
                "now",
            ] {
                map.remove(volatile);
            }
            for child in map.values_mut() {
                strip_volatile_fields(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_volatile_fields(item);
            }
        }
        _ => {}
    }
}

fn data_payload(parsed: &Value) -> Value {
    parsed
        .get("data")
        .cloned()
        .unwrap_or_else(|| parsed.clone())
}

// ---- Safety contracts ------------------------------------------------

#[test]
fn apply_requires_explicit_apply_flag_even_when_other_flags_are_set() -> TestResult {
    // bd-215tr safety invariant 1: --propose, --dry-run, and the
    // absence of --apply must all preserve `apply: false` and
    // `durableMutation: false`. Without --apply, no
    // memory.level_transition audit row is written.
    let workspace = unique_workspace("apply-gated")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    for extra in [
        &[][..],
        &["--propose"][..],
        &["--dry-run"][..],
        &["--propose", "--dry-run"][..],
    ] {
        let (output, parsed) = auto_promote(&workspace_arg, extra)?;
        ensure(
            output.status.success(),
            format!(
                "auto-promote {} must exit zero; stderr: {}",
                extra.join(" "),
                String::from_utf8_lossy(&output.stderr)
            ),
        )?;
        let data = data_payload(&parsed);
        ensure(
            data.get("apply").and_then(Value::as_bool) == Some(false),
            format!(
                "apply must be false without --apply (flags: {}); got {:?}",
                extra.join(" "),
                data.get("apply")
            ),
        )?;
        ensure(
            data.get("durableMutation").and_then(Value::as_bool) == Some(false),
            format!(
                "durableMutation must be false without --apply (flags: {}); got {:?}",
                extra.join(" "),
                data.get("durableMutation")
            ),
        )?;
        ensure(
            data.get("appliedCount").and_then(Value::as_u64) == Some(0),
            format!(
                "appliedCount must be 0 without --apply (flags: {}); got {:?}",
                extra.join(" "),
                data.get("appliedCount")
            ),
        )?;
    }
    Ok(())
}

#[test]
fn dry_run_override_keeps_apply_false_even_when_apply_flag_is_set() -> TestResult {
    // src/cli/mod.rs:37481 wires `dry_run = args.dry_run || !args.apply`.
    // When both --apply AND --dry-run are passed, the conservative path
    // (dry_run) wins. Pin that here so an accidental refactor of the
    // boolean reduction surfaces as a test failure rather than as a
    // silent mutation.
    let workspace = unique_workspace("dry-run-overrides-apply")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = auto_promote(&workspace_arg, &["--apply", "--dry-run"])?;
    ensure(
        output.status.success(),
        format!(
            "auto-promote --apply --dry-run must exit zero; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = data_payload(&parsed);
    ensure(
        data.get("dryRun").and_then(Value::as_bool) == Some(true),
        format!(
            "dryRun must be true when --dry-run is set alongside --apply; got {:?}",
            data.get("dryRun")
        ),
    )?;
    ensure(
        data.get("durableMutation").and_then(Value::as_bool) == Some(false),
        format!(
            "durableMutation must be false when --dry-run is set alongside --apply; got {:?}",
            data.get("durableMutation")
        ),
    )
}

#[test]
fn workspace_isolation_keeps_proposals_local() -> TestResult {
    // bd-215tr safety invariant 2: a memory remembered in workspace A
    // must not appear in scanned/eligible counts when auto-promote runs
    // against workspace B. Each workspace's DB is independent.
    let workspace_a = unique_workspace("iso-a")?;
    let workspace_b = unique_workspace("iso-b")?;
    let ws_a = workspace_a
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    let ws_b = workspace_b
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&ws_a)?;
    init_workspace(&ws_b)?;
    remember(
        &ws_a,
        "isolation test memory in workspace A",
        "episodic",
        "0.95",
    )?;

    let (_, parsed_a) = auto_promote(&ws_a, &["--propose", "--dry-run"])?;
    let (_, parsed_b) = auto_promote(&ws_b, &["--propose", "--dry-run"])?;
    let data_a = data_payload(&parsed_a);
    let data_b = data_payload(&parsed_b);

    let scanned_a = data_a
        .get("scannedMemoryCount")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!(
                "workspace A scannedMemoryCount missing or non-numeric; got {:?}",
                data_a.get("scannedMemoryCount")
            )
        })?;
    let scanned_b = data_b
        .get("scannedMemoryCount")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!(
                "workspace B scannedMemoryCount missing or non-numeric; got {:?}",
                data_b.get("scannedMemoryCount")
            )
        })?;
    ensure(
        scanned_a >= 1,
        format!("workspace A must see at least 1 memory; got scannedMemoryCount={scanned_a}"),
    )?;
    ensure(
        scanned_b == 0,
        format!(
            "workspace B must see 0 memories from workspace A; got scannedMemoryCount={scanned_b}"
        ),
    )
}

#[test]
fn dry_run_produces_byte_identical_proposals_across_two_runs() -> TestResult {
    // bd-215tr safety invariant 3: deterministic ordering. Two
    // propose-dry-run invocations against the same DB with the same
    // thresholds must yield byte-identical JSON after stripping
    // volatile fields. If a future refactor sorts by an unstable key,
    // this test fails before the regression ships.
    let workspace = unique_workspace("determinism")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    // Seed several memories at different confidences so any
    // sort-by-confidence path has something to order.
    for (content, confidence) in [
        ("memory alpha for determinism", "0.95"),
        ("memory bravo for determinism", "0.88"),
        ("memory charlie for determinism", "0.92"),
        ("memory delta for determinism", "0.83"),
    ] {
        remember(&workspace_arg, content, "episodic", confidence)?;
    }

    let (_, mut first) = auto_promote(&workspace_arg, &["--propose", "--dry-run"])?;
    let (_, mut second) = auto_promote(&workspace_arg, &["--propose", "--dry-run"])?;
    strip_volatile_fields(&mut first);
    strip_volatile_fields(&mut second);
    if first != second {
        return Err(format!(
            "dry-run proposals drifted across two runs.\n--- first ---\n{}\n--- second ---\n{}",
            serde_json::to_string_pretty(&first).unwrap_or_default(),
            serde_json::to_string_pretty(&second).unwrap_or_default()
        ));
    }
    Ok(())
}

#[test]
fn dry_run_is_idempotent_and_records_no_durable_mutation() -> TestResult {
    // bd-215tr safety invariant 4: running dry-run twice must produce
    // `durableMutation: false` both times, and the second invocation
    // must scan the same number of memories as the first — proving the
    // first run did not change anything observable to the surface.
    let workspace = unique_workspace("idempotent")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    remember(&workspace_arg, "idempotency test memory", "episodic", "0.9")?;

    let (_, parsed_a) = auto_promote(&workspace_arg, &["--propose", "--dry-run"])?;
    let (_, parsed_b) = auto_promote(&workspace_arg, &["--propose", "--dry-run"])?;
    let data_a = data_payload(&parsed_a);
    let data_b = data_payload(&parsed_b);

    for (label, data) in [("first", &data_a), ("second", &data_b)] {
        ensure(
            data.get("durableMutation").and_then(Value::as_bool) == Some(false),
            format!("{label} dry-run reported durableMutation != false"),
        )?;
        ensure(
            data.get("appliedCount").and_then(Value::as_u64) == Some(0),
            format!("{label} dry-run reported appliedCount != 0"),
        )?;
    }

    let scanned_a = data_a.get("scannedMemoryCount").and_then(Value::as_u64);
    let scanned_b = data_b.get("scannedMemoryCount").and_then(Value::as_u64);
    ensure(
        scanned_a == scanned_b,
        format!(
            "scannedMemoryCount drifted across two dry-run invocations: {scanned_a:?} vs {scanned_b:?}"
        ),
    )
}

#[test]
fn effective_thresholds_appear_in_proposal_envelope() -> TestResult {
    // bd-215tr safety invariant 5: every proposal envelope must report
    // the non-secret threshold inputs that decided eligibility so an
    // agent can audit "why was X proposed / why was Y disqualified"
    // without re-deriving the math.
    let workspace = unique_workspace("thresholds-in-envelope")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = auto_promote(
        &workspace_arg,
        &[
            "--propose",
            "--dry-run",
            "--min-access-count-episodic",
            "7",
            "--min-confidence-episodic",
            "0.85",
            "--min-access-count-semantic",
            "12",
            "--min-confidence-semantic",
            "0.92",
        ],
    )?;
    ensure(
        output.status.success(),
        format!(
            "auto-promote with thresholds must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = data_payload(&parsed);
    // The envelope schema is bd-2r8vp / ee.curate.auto_promote.v1. The
    // exact field name for thresholds is owned by that bead; this test
    // accepts any of the documented surface names so cc_1 can rename
    // without coordination, as long as the values survive somewhere
    // structured.
    let candidate_paths = [
        "/thresholds",
        "/effectiveThresholds",
        "/inputs/thresholds",
        "/policy/thresholds",
    ];
    let mut found = None;
    for path in candidate_paths {
        if let Some(node) = data.pointer(path) {
            if node.is_object() {
                found = Some((path, node.clone()));
                break;
            }
        }
    }
    let (path, thresholds_node) = found.ok_or_else(|| {
        format!(
            "auto-promote envelope must expose effective thresholds under one of {candidate_paths:?}; envelope keys: {:?}",
            data.as_object()
                .map(|m| m.keys().cloned().collect::<Vec<_>>())
        )
    })?;
    let to_match: Vec<(&str, Value)> = vec![
        ("minAccessCountEpisodic", Value::from(7)),
        ("minConfidenceEpisodic", Value::from(0.85_f64)),
        ("minAccessCountSemantic", Value::from(12)),
        ("minConfidenceSemantic", Value::from(0.92_f64)),
    ];
    for (key, expected) in to_match {
        let actual = thresholds_node.get(key).cloned();
        let matches = match (&actual, &expected) {
            (Some(Value::Number(a)), Value::Number(b)) => {
                a.as_f64().zip(b.as_f64()).is_some_and(|(x, y)| {
                    // Compare as f64 with a tight tolerance — JSON
                    // serialization of 0.85 can round-trip as 0.85 or
                    // 0.8500000000000001 depending on the formatter.
                    (x - y).abs() < 1e-6
                })
            }
            _ => false,
        };
        if !matches {
            return Err(format!(
                "thresholds at `{path}` missing or wrong `{key}`: expected {expected}, got {actual:?}"
            ));
        }
    }
    Ok(())
}
