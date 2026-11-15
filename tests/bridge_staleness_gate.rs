#![allow(clippy::expect_used)]
// This contract test intentionally fails fast when the script output shape drifts.

//! Bridge staleness advisory gate (bd-3usjw.33 / CLOSE_THE_GAP §36).
//!
//! Drives `scripts/bridge-staleness.sh --json` and asserts the report
//! conforms to the `ee.bridge.staleness.v1` schema: required top-level
//! fields, an `inputs` block carrying the raw measurements, and a
//! `signals[]` array where each entry pins `{ code, severity, message,
//! repair }` plus the relevant `details`. The gate is non-blocking by
//! design (the script always exits 0 and the test only flags drift via
//! a single advisory assertion at the bottom of the report shape).

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;

use ee::models::DegradationSeverity;

const VALID_CODES: &[&str] = &[
    "plan_mtime_age_days",
    "vision_coverage_gap_low",
    "in_progress_beads_mtime",
];

fn duration_millis_saturating(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn trace_bridge_staleness_gate(phase: &'static str, elapsed_ms: u64, degraded_codes: &[&str]) {
    tracing::info!(
        workspace_id = "repo",
        request_id = "bridge_staleness_gate_contract",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-3usjw.33"),
        surface = "bridge_staleness_gate",
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        "bridge staleness gate contract checkpoint"
    );
}

fn run_script() -> Result<Value, String> {
    let started = Instant::now();
    trace_bridge_staleness_gate("input", 0, &[]);
    let script = workspace_root().join("scripts").join("bridge-staleness.sh");
    let output = Command::new("bash")
        .arg(script.as_os_str())
        .arg("--json")
        .output()
        .map_err(|error| format!("failed to spawn bridge-staleness.sh: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "bridge-staleness.sh exited with status {:?}; stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim_end(),
        ));
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("bridge-staleness.sh stdout was not UTF-8: {error}"))?;
    let report = serde_json::from_str::<Value>(&stdout).map_err(|error| {
        format!("bridge-staleness.sh stdout was not JSON: {error}\nstdout: {stdout}")
    })?;
    trace_bridge_staleness_gate(
        "response",
        duration_millis_saturating(started.elapsed()),
        &[],
    );
    Ok(report)
}

#[test]
fn bridge_staleness_report_matches_schema_v1() {
    let report = run_script().expect("bridge-staleness.sh must produce JSON");

    assert_eq!(
        report.get("schema").and_then(Value::as_str),
        Some("ee.bridge.staleness.v1"),
        "schema field must pin v1 contract: {report}"
    );
    assert!(
        report
            .get("generatedAt")
            .and_then(Value::as_str)
            .is_some_and(|generated| generated.ends_with('Z')),
        "generatedAt must be an RFC 3339 UTC timestamp: {report}"
    );
    assert!(
        report
            .get("dataHash")
            .and_then(Value::as_str)
            .is_some_and(|hash| !hash.is_empty() && hash != "null"),
        "dataHash must be present and non-empty: {report}"
    );

    let inputs = report
        .get("inputs")
        .and_then(Value::as_object)
        .expect("inputs object must be present in report");
    for required in [
        "planPresent",
        "planAgeDays",
        "visionCoverageReportPresent",
        "partIIOpenCount",
        "partIIInProgressCount",
        "partIIMaxStaleDays",
    ] {
        assert!(
            inputs.contains_key(required),
            "inputs.{required} missing from report: {report}"
        );
    }

    let signals = report
        .get("signals")
        .and_then(Value::as_array)
        .expect("signals must be an array");
    for signal in signals {
        let code = signal
            .get("code")
            .and_then(Value::as_str)
            .expect("signal must carry code");
        assert!(
            VALID_CODES.contains(&code),
            "unknown bridge-staleness signal code {code:?}; expected one of {VALID_CODES:?}"
        );
        let severity = signal
            .get("severity")
            .and_then(Value::as_str)
            .expect("signal must carry severity");
        assert!(
            DegradationSeverity::parse(severity).is_some_and(|parsed| parsed.as_str() == severity),
            "unknown or noncanonical severity {severity:?}"
        );
        assert!(
            signal
                .get("message")
                .and_then(Value::as_str)
                .is_some_and(|message| !message.is_empty()),
            "signal {code:?} must carry non-empty message"
        );
        assert!(
            signal.get("repair").and_then(Value::as_str).is_some(),
            "signal {code:?} must carry repair hint"
        );
        assert!(
            signal.get("details").and_then(Value::as_object).is_some(),
            "signal {code:?} must carry details object"
        );
    }
}

#[test]
fn bridge_staleness_writes_report_file_matching_stdout() {
    let report = run_script().expect("bridge-staleness.sh must produce JSON");
    let report_path = workspace_root().join(".bridge-staleness-report.json");
    let disk_report = fs::read_to_string(&report_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", report_path.display()));
    let disk_json = serde_json::from_str::<Value>(&disk_report)
        .unwrap_or_else(|error| panic!("disk report was not JSON: {error}"));

    assert_eq!(
        disk_json, report,
        "disk report should match stdout JSON for one script invocation"
    );
}

#[test]
fn bridge_staleness_inputs_are_numeric_or_boolean() {
    let report = run_script().expect("bridge-staleness.sh must produce JSON");
    let inputs = report
        .get("inputs")
        .and_then(Value::as_object)
        .expect("inputs object must be present");

    for (field, expect_bool) in [("planPresent", true), ("visionCoverageReportPresent", true)] {
        let value = inputs
            .get(field)
            .unwrap_or_else(|| panic!("inputs.{field} missing"));
        assert_eq!(
            value.is_boolean(),
            expect_bool,
            "inputs.{field} must be boolean (got {value:?})"
        );
    }

    for field in [
        "planAgeDays",
        "partIIOpenCount",
        "partIIInProgressCount",
        "partIIMaxStaleDays",
    ] {
        let value = inputs
            .get(field)
            .unwrap_or_else(|| panic!("inputs.{field} missing"));
        assert!(
            value.is_number() || value.is_null(),
            "inputs.{field} must be numeric or null (got {value:?})"
        );
        if let Some(integer) = value.as_i64() {
            assert!(
                integer >= 0,
                "inputs.{field} must be non-negative (got {integer})"
            );
        }
    }
}

#[test]
fn bridge_staleness_signal_codes_are_unique_when_present() {
    let report = run_script().expect("bridge-staleness.sh must produce JSON");
    let signals = report
        .get("signals")
        .and_then(Value::as_array)
        .expect("signals must be array");
    let mut seen = std::collections::BTreeSet::new();
    for signal in signals {
        let code = signal
            .get("code")
            .and_then(Value::as_str)
            .expect("signal code");
        assert!(
            seen.insert(code.to_owned()),
            "duplicate signal code {code} in bridge-staleness report"
        );
    }
}

// ---------------------------------------------------------------------------
// bd-reality-core-convergence-1azkt.36: Bridge completion & closeout oracle
// ---------------------------------------------------------------------------

use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackerDependency {
    issue_id: String,
    depends_on_id: String,
    dep_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackerIssue {
    id: String,
    status: String,
    dependencies: Vec<TrackerDependency>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EvidenceFailureReason {
    Absent,
    Failed,
    Stale,
    WrongSource { expected: String, actual: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BehavioralArtifact {
    owner_id: String,
    required: bool,
    verdict: String,
    source_commit: String,
    stale: bool,
    mtime_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BridgeCompletionVerdict {
    Complete {
        closeout_id: String,
        verified_owners: Vec<String>,
        optional_missing: Vec<String>,
    },
    ArchivedReadyForSuccessor {
        owner_root: String,
    },
    MissingPlan,
    UnsupportedContract {
        reason: String,
    },
    BehaviorUnproven {
        phase: Option<String>,
        owner_root: Option<String>,
        open_count: u64,
        in_progress_count: u64,
    },
    DependencyCycle {
        cycle: Vec<String>,
    },
    MissingBlockingDependency {
        closeout_id: String,
        missing_owners: Vec<String>,
    },
    UnclosedRequiredOwners {
        open_owners: Vec<String>,
    },
    InvalidBehavioralEvidence {
        owner: String,
        reason: EvidenceFailureReason,
    },
}

/// Parses tracker issues from either JSONL (`.beads/issues.jsonl`) or the
/// `{ "issues": [...] }` / `[...]` JSON envelope emitted by `br list --json`.
fn parse_tracker_issues(raw: &str) -> Result<BTreeMap<String, TrackerIssue>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(BTreeMap::new());
    }
    let records: Vec<Value> = if trimmed.starts_with('{') && !trimmed.contains('\n') {
        let single: Value = serde_json::from_str(trimmed)
            .map_err(|err| format!("parse single JSON tracker object: {err}"))?;
        if let Some(arr) = single.get("issues").and_then(Value::as_array) {
            arr.clone()
        } else {
            vec![single]
        }
    } else if let Ok(envelope) = serde_json::from_str::<Value>(trimmed) {
        if let Some(arr) = envelope.get("issues").and_then(Value::as_array) {
            arr.clone()
        } else if let Some(arr) = envelope.as_array() {
            arr.clone()
        } else {
            vec![envelope]
        }
    } else {
        trimmed
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str::<Value>(line)
                    .map_err(|err| format!("parse JSONL tracker line: {err}"))
            })
            .collect::<Result<Vec<_>, _>>()?
    };

    let mut issues = BTreeMap::new();
    for record in records {
        let id = record
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("tracker issue missing id: {record}"))?
            .to_owned();
        let status = record
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let mut dependencies = Vec::new();
        if let Some(deps) = record.get("dependencies").and_then(Value::as_array) {
            for dep in deps {
                let issue_id = dep
                    .get("issue_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_owned();
                let Some(depends_on_id) = dep.get("depends_on_id").and_then(Value::as_str) else {
                    continue;
                };
                let dep_type = dep
                    .get("type")
                    .or_else(|| dep.get("dependency_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("blocks")
                    .to_owned();
                dependencies.push(TrackerDependency {
                    issue_id,
                    depends_on_id: depends_on_id.to_owned(),
                    dep_type,
                });
            }
        }
        issues.insert(
            id.clone(),
            TrackerIssue {
                id,
                status,
                dependencies,
            },
        );
    }
    Ok(issues)
}

/// Computes the `blocks`-only transitive closure starting at `closeout_id`.
/// `parent-child` and `related` links are intentionally ignored.
fn blocks_transitive_closure(
    issues: &BTreeMap<String, TrackerIssue>,
    closeout_id: &str,
) -> BTreeSet<String> {
    let mut visited = BTreeSet::new();
    let mut queue = VecDeque::new();
    if issues.contains_key(closeout_id) {
        visited.insert(closeout_id.to_owned());
        queue.push_back(closeout_id.to_owned());
    }
    while let Some(current) = queue.pop_front() {
        if let Some(issue) = issues.get(&current) {
            for dep in &issue.dependencies {
                if dep.dep_type == "blocks"
                    && dep.issue_id == current
                    && visited.insert(dep.depends_on_id.clone())
                {
                    queue.push_back(dep.depends_on_id.clone());
                }
            }
        }
    }
    visited
}

/// Finds an active `blocks` dependency cycle among non-closed issues.
/// All-closed historical cycles are ignored so archived history is not
/// conflated with an active blocker.
fn detect_active_blocks_cycle(issues: &BTreeMap<String, TrackerIssue>) -> Option<Vec<String>> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum VisitState {
        Visiting,
        Visited,
    }

    fn dfs(
        node: &str,
        issues: &BTreeMap<String, TrackerIssue>,
        state: &mut BTreeMap<String, VisitState>,
        stack: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        state.insert(node.to_owned(), VisitState::Visiting);
        stack.push(node.to_owned());

        if let Some(issue) = issues.get(node) {
            for dep in &issue.dependencies {
                if dep.dep_type != "blocks" || dep.issue_id != node {
                    continue;
                }
                let target = &dep.depends_on_id;
                let Some(target_issue) = issues.get(target) else {
                    continue;
                };
                if target_issue.status == "closed" {
                    continue;
                }
                match state.get(target).copied() {
                    Some(VisitState::Visiting) => {
                        let start = stack.iter().position(|item| item == target).unwrap_or(0);
                        let mut cycle = stack[start..].to_vec();
                        cycle.push(target.clone());
                        return Some(cycle);
                    }
                    Some(VisitState::Visited) => {}
                    None => {
                        if let Some(cycle) = dfs(target, issues, state, stack) {
                            return Some(cycle);
                        }
                    }
                }
            }
        }

        stack.pop();
        state.insert(node.to_owned(), VisitState::Visited);
        None
    }

    let mut state = BTreeMap::new();
    let mut stack = Vec::new();
    for (id, issue) in issues {
        if issue.status == "closed" || state.contains_key(id) {
            continue;
        }
        if let Some(cycle) = dfs(id, issues, &mut state, &mut stack) {
            return Some(cycle);
        }
    }
    None
}

/// Validates bridge completion combining the advisory report, tracker `blocks`
/// graph, required owner inventory (including late-added owners outside the
/// initial child range), and current-source behavioral proof artifacts.
fn evaluate_bridge_completion(
    advisory_report: &Value,
    issues: &BTreeMap<String, TrackerIssue>,
    closeout_id: &str,
    required_owners: &[&str],
    optional_owners: &[&str],
    artifacts: &BTreeMap<String, BehavioralArtifact>,
    candidate_commit: &str,
) -> BridgeCompletionVerdict {
    if advisory_report.get("schema").and_then(Value::as_str) != Some("ee.bridge.staleness.v1") {
        return BridgeCompletionVerdict::UnsupportedContract {
            reason: "unsupported or missing advisory schema".to_owned(),
        };
    }

    let Some(inputs) = advisory_report.get("inputs").and_then(Value::as_object) else {
        return BridgeCompletionVerdict::UnsupportedContract {
            reason: "missing inputs object".to_owned(),
        };
    };

    if inputs.get("planPresent").and_then(Value::as_bool) != Some(true) {
        return BridgeCompletionVerdict::MissingPlan;
    }

    let plan_status = inputs
        .get("activePlanStatus")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let plan_phase = inputs
        .get("activePlanPhase")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let owner_root = inputs
        .get("activeOwnerRoot")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let open_count = inputs
        .get("activeOwnerOpenCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let in_progress_count = inputs
        .get("activeOwnerInProgressCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    if plan_status == "unknown" {
        return BridgeCompletionVerdict::UnsupportedContract {
            reason: "bridge plan status is unknown/unreadable".to_owned(),
        };
    }

    if let Some(cycle) = detect_active_blocks_cycle(issues) {
        return BridgeCompletionVerdict::DependencyCycle { cycle };
    }

    if plan_status == "active" && (open_count > 0 || in_progress_count > 0) {
        return BridgeCompletionVerdict::BehaviorUnproven {
            phase: plan_phase,
            owner_root,
            open_count,
            in_progress_count,
        };
    }

    let closure = blocks_transitive_closure(issues, closeout_id);
    let missing_blocking: Vec<String> = required_owners
        .iter()
        .filter(|owner| !closure.contains(**owner))
        .map(|owner| (*owner).to_owned())
        .collect();
    if !missing_blocking.is_empty() {
        return BridgeCompletionVerdict::MissingBlockingDependency {
            closeout_id: closeout_id.to_owned(),
            missing_owners: missing_blocking,
        };
    }

    let unclosed: Vec<String> = required_owners
        .iter()
        .filter(|owner| issues.get(**owner).map(|issue| issue.status.as_str()) != Some("closed"))
        .map(|owner| (*owner).to_owned())
        .collect();
    if !unclosed.is_empty() {
        return BridgeCompletionVerdict::UnclosedRequiredOwners {
            open_owners: unclosed,
        };
    }

    // Validate required behavioral proof artifacts (file mtime alone never proves freshness).
    let mut verified_owners = Vec::new();
    for &owner in required_owners {
        let Some(artifact) = artifacts.get(owner) else {
            return BridgeCompletionVerdict::InvalidBehavioralEvidence {
                owner: owner.to_owned(),
                reason: EvidenceFailureReason::Absent,
            };
        };
        if artifact.verdict != "pass" {
            return BridgeCompletionVerdict::InvalidBehavioralEvidence {
                owner: owner.to_owned(),
                reason: EvidenceFailureReason::Failed,
            };
        }
        if artifact.stale {
            return BridgeCompletionVerdict::InvalidBehavioralEvidence {
                owner: owner.to_owned(),
                reason: EvidenceFailureReason::Stale,
            };
        }
        if artifact.source_commit != candidate_commit {
            return BridgeCompletionVerdict::InvalidBehavioralEvidence {
                owner: owner.to_owned(),
                reason: EvidenceFailureReason::WrongSource {
                    expected: candidate_commit.to_owned(),
                    actual: artifact.source_commit.clone(),
                },
            };
        }
        verified_owners.push(owner.to_owned());
    }

    let mut optional_missing = Vec::new();
    for &opt_owner in optional_owners {
        let valid = artifacts.get(opt_owner).is_some_and(|art| {
            art.verdict == "pass" && !art.stale && art.source_commit == candidate_commit
        });
        if !valid {
            optional_missing.push(opt_owner.to_owned());
        }
    }

    if plan_status == "archived" {
        return BridgeCompletionVerdict::ArchivedReadyForSuccessor {
            owner_root: owner_root.unwrap_or_else(|| "unknown".to_owned()),
        };
    }

    BridgeCompletionVerdict::Complete {
        closeout_id: closeout_id.to_owned(),
        verified_owners,
        optional_missing,
    }
}

fn run_script_with_fixtures(
    scratch: &Path,
    case_name: &str,
    plan_content: Option<&str>,
    vision_content: &str,
    beads_content: &str,
) -> Result<(Value, PathBuf, PathBuf, PathBuf), String> {
    let case_dir = scratch.join(case_name);
    fs::create_dir_all(&case_dir)
        .map_err(|err| format!("create fixture dir {}: {err}", case_dir.display()))?;
    let plan_path = case_dir.join("CLOSE_THE_GAP_PLAN.md");
    if let Some(plan) = plan_content {
        fs::write(&plan_path, plan)
            .map_err(|err| format!("write fixture plan {}: {err}", plan_path.display()))?;
    }
    let vision_path = case_dir.join("vision.json");
    fs::write(&vision_path, vision_content)
        .map_err(|err| format!("write fixture vision {}: {err}", vision_path.display()))?;
    let beads_path = case_dir.join("issues.jsonl");
    fs::write(&beads_path, beads_content)
        .map_err(|err| format!("write fixture beads {}: {err}", beads_path.display()))?;
    let output_path = case_dir.join("report.json");

    let script = workspace_root().join("scripts").join("bridge-staleness.sh");
    let output = Command::new("bash")
        .arg(script.as_os_str())
        .arg("--json")
        .arg("--plan")
        .arg(&plan_path)
        .arg("--vision")
        .arg(&vision_path)
        .arg("--beads")
        .arg(&beads_path)
        .arg("--output")
        .arg(&output_path)
        .output()
        .map_err(|err| format!("spawn bridge-staleness.sh for {case_name}: {err}"))?;

    let stdout_path = case_dir.join("stdout.json");
    let stderr_path = case_dir.join("stderr.txt");
    fs::write(&stdout_path, &output.stdout)
        .map_err(|err| format!("write stdout for {case_name}: {err}"))?;
    fs::write(&stderr_path, &output.stderr)
        .map_err(|err| format!("write stderr for {case_name}: {err}"))?;

    if !output.status.success() {
        return Err(format!(
            "bridge-staleness.sh failed for {case_name} with {:?}\nstdout: {}\nstderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let report: Value = serde_json::from_slice(&output.stdout).map_err(|err| {
        format!(
            "parse stdout JSON for {case_name}: {err}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })?;
    Ok((report, stdout_path, stderr_path, output_path))
}

fn append_test_event(
    events_path: &Path,
    phase: &str,
    case_id: &str,
    verdict: &str,
    details: Value,
) {
    let event = json!({
        "schema": "ee.test_event.v1",
        "bead_id": "bd-reality-core-convergence-1azkt.36",
        "surface": "bridge_staleness_gate",
        "phase": phase,
        "case_id": case_id,
        "verdict": verdict,
        "details": details,
    });
    let mut line = serde_json::to_string(&event).expect("serialize ee.test_event.v1");
    line.push('\n');
    let existing = fs::read_to_string(events_path).unwrap_or_default();
    fs::write(events_path, format!("{existing}{line}")).expect("append ee.test_event.v1");
}

#[test]
fn bridge_completion_validator_rejects_false_completion_and_missing_closeout_edges() {
    let scratch = tempfile::Builder::new()
        .prefix("ee-bridge-36-")
        .tempdir()
        .expect("create scratch dir")
        .keep();
    let events_path = scratch.join("test_events.jsonl");

    let candidate_commit = "d0f6ad2e90000000000000000000000000000000";
    let closeout_id = "bd-reality-core-convergence-1azkt.22";
    let required_owners = &[
        "bd-reality-core-convergence-1azkt.22",
        "bd-reality-core-convergence-1azkt.27",
        "bd-reality-core-convergence-1azkt.33",
        "bd-wubqf",
        "bd-z368i",
    ];
    let optional_owners = &["bd-optional-telemetry.1"];

    append_test_event(
        &events_path,
        "setup",
        "matrix_init",
        "pass",
        json!({
            "scratch": scratch.display().to_string(),
            "candidate_commit": candidate_commit,
            "required_owners": required_owners,
        }),
    );

    let active_plan = "\
# CLOSE_THE_GAP_PLAN.md
> **Status: ACTIVE (Part III).**
Tracked under bd-reality-core-convergence-1azkt.22, bd-reality-core-convergence-1azkt.27,
and bd-reality-core-convergence-1azkt.33.
";
    let archived_plan = "\
# CLOSE_THE_GAP_PLAN.md
> **Status: ARCHIVED (Part II).**
Tracked under bd-reality-core-convergence-1azkt.22, bd-reality-core-convergence-1azkt.27,
and bd-reality-core-convergence-1azkt.33.
";

    // Build positive complete tracker JSONL where `.22` transitively depends via `blocks`
    // on both `1azkt.*` children and late-added required owners (`bd-wubqf`, `bd-z368i`),
    // plus an archived all-closed cycle that must NOT trip active cycle detection.
    let positive_beads_jsonl = [
        json!({
            "id": "bd-reality-core-convergence-1azkt.22",
            "status": "closed",
            "dependencies": [
                {"issue_id": "bd-reality-core-convergence-1azkt.22", "depends_on_id": "bd-reality-core-convergence-1azkt.27", "type": "blocks"},
                {"issue_id": "bd-reality-core-convergence-1azkt.22", "depends_on_id": "bd-wubqf", "type": "blocks"}
            ]
        }),
        json!({
            "id": "bd-reality-core-convergence-1azkt.27",
            "status": "closed",
            "dependencies": [
                {"issue_id": "bd-reality-core-convergence-1azkt.27", "depends_on_id": "bd-reality-core-convergence-1azkt.33", "type": "blocks"}
            ]
        }),
        json!({
            "id": "bd-reality-core-convergence-1azkt.33",
            "status": "closed",
            "dependencies": [
                {"issue_id": "bd-reality-core-convergence-1azkt.33", "depends_on_id": "bd-z368i", "type": "blocks"}
            ]
        }),
        json!({
            "id": "bd-wubqf",
            "status": "closed",
            "dependencies": []
        }),
        json!({
            "id": "bd-z368i",
            "status": "closed",
            "dependencies": []
        }),
        // Archived all-closed cycle: must not be conflated with an active cycle.
        json!({
            "id": "bd-archived-cycle.1",
            "status": "closed",
            "dependencies": [
                {"issue_id": "bd-archived-cycle.1", "depends_on_id": "bd-archived-cycle.2", "type": "blocks"}
            ]
        }),
        json!({
            "id": "bd-archived-cycle.2",
            "status": "closed",
            "dependencies": [
                {"issue_id": "bd-archived-cycle.2", "depends_on_id": "bd-archived-cycle.1", "type": "blocks"}
            ]
        }),
    ]
    .iter()
    .map(|row| serde_json::to_string(row).expect("serialize row"))
    .collect::<Vec<_>>()
    .join("\n");

    let mut positive_artifacts = BTreeMap::new();
    for &owner in required_owners {
        positive_artifacts.insert(
            owner.to_owned(),
            BehavioralArtifact {
                owner_id: owner.to_owned(),
                required: true,
                verdict: "pass".to_owned(),
                source_commit: candidate_commit.to_owned(),
                stale: false,
                mtime_epoch: 1_700_000_000,
            },
        );
    }

    // 1. Active Part III with live requirements -> BehaviorUnproven, advisory does not recommend Part III.
    let live_beads_jsonl = [
        json!({"id": "bd-reality-core-convergence-1azkt.22", "status": "open", "dependencies": []}),
        json!({"id": "bd-reality-core-convergence-1azkt.27", "status": "in_progress", "dependencies": []}),
    ]
    .iter()
    .map(|row| serde_json::to_string(row).expect("serialize"))
    .collect::<Vec<_>>()
    .join("\n");
    let (active_report, _, _, _) = run_script_with_fixtures(
        &scratch,
        "case_active_part_iii",
        Some(active_plan),
        r#"{"gap_percentage": 0.0}"#,
        &live_beads_jsonl,
    )
    .expect("run active Part III fixture");
    let live_issues = parse_tracker_issues(&live_beads_jsonl).expect("parse live issues");
    let verdict = evaluate_bridge_completion(
        &active_report,
        &live_issues,
        closeout_id,
        required_owners,
        optional_owners,
        &positive_artifacts,
        candidate_commit,
    );
    assert_eq!(
        verdict,
        BridgeCompletionVerdict::BehaviorUnproven {
            phase: Some("Part III".to_owned()),
            owner_root: Some("bd-reality-core-convergence-1azkt".to_owned()),
            open_count: 1,
            in_progress_count: 1,
        }
    );
    append_test_event(
        &events_path,
        "assert",
        "case_active_part_iii",
        "pass",
        json!({"verdict": format!("{verdict:?}")}),
    );

    // 2. Missing plan -> MissingPlan.
    let (missing_plan_report, _, _, _) = run_script_with_fixtures(
        &scratch,
        "case_missing_plan",
        None,
        r#"{"gap_percentage": 0.0}"#,
        &positive_beads_jsonl,
    )
    .expect("run missing plan fixture");
    let positive_issues =
        parse_tracker_issues(&positive_beads_jsonl).expect("parse positive issues");
    assert_eq!(
        evaluate_bridge_completion(
            &missing_plan_report,
            &positive_issues,
            closeout_id,
            required_owners,
            optional_owners,
            &positive_artifacts,
            candidate_commit,
        ),
        BridgeCompletionVerdict::MissingPlan
    );

    // 3. Unsupported contract (plan without readable status marker, or unsupported schema).
    let (unknown_status_report, _, _, _) = run_script_with_fixtures(
        &scratch,
        "case_unknown_status",
        Some("# Plan without status marker\n"),
        r#"{"gap_percentage": 0.0}"#,
        &positive_beads_jsonl,
    )
    .expect("run unknown status fixture");
    assert_eq!(
        evaluate_bridge_completion(
            &unknown_status_report,
            &positive_issues,
            closeout_id,
            required_owners,
            optional_owners,
            &positive_artifacts,
            candidate_commit,
        ),
        BridgeCompletionVerdict::UnsupportedContract {
            reason: "bridge plan status is unknown/unreadable".to_owned(),
        }
    );
    assert_eq!(
        evaluate_bridge_completion(
            &json!({"schema": "ee.bridge.staleness.v999"}),
            &positive_issues,
            closeout_id,
            required_owners,
            optional_owners,
            &positive_artifacts,
            candidate_commit,
        ),
        BridgeCompletionVerdict::UnsupportedContract {
            reason: "unsupported or missing advisory schema".to_owned(),
        }
    );

    // 4. Positive baseline (all required owners closed + in `blocks` closure + valid artifacts;
    //    optional owner missing is differentiated and non-blocking).
    let (positive_report, _, _, _) = run_script_with_fixtures(
        &scratch,
        "case_positive_complete",
        Some(active_plan),
        r#"{"gap_percentage": 0.0}"#,
        &positive_beads_jsonl,
    )
    .expect("run positive complete fixture");
    assert_eq!(
        evaluate_bridge_completion(
            &positive_report,
            &positive_issues,
            closeout_id,
            required_owners,
            optional_owners,
            &positive_artifacts,
            candidate_commit,
        ),
        BridgeCompletionVerdict::Complete {
            closeout_id: closeout_id.to_owned(),
            verified_owners: required_owners.iter().map(|s| (*s).to_owned()).collect(),
            optional_missing: vec!["bd-optional-telemetry.1".to_owned()],
        }
    );

    // 5. Archived prior part with complete evidence -> ArchivedReadyForSuccessor.
    let (archived_report, _, _, _) = run_script_with_fixtures(
        &scratch,
        "case_archived_prior_part",
        Some(archived_plan),
        r#"{"gap_percentage": 0.0}"#,
        &positive_beads_jsonl,
    )
    .expect("run archived prior part fixture");
    assert_eq!(
        evaluate_bridge_completion(
            &archived_report,
            &positive_issues,
            closeout_id,
            required_owners,
            optional_owners,
            &positive_artifacts,
            candidate_commit,
        ),
        BridgeCompletionVerdict::ArchivedReadyForSuccessor {
            owner_root: "bd-reality-core-convergence-1azkt".to_owned(),
        }
    );

    // 6. Negative Oracle A: Strip ONE required `blocks` edge (`bd-wubqf`) while keeping
    //    `parent-child` and `related` links and 100% file coverage intact -> must fail
    //    and name `bd-wubqf`.
    let mut parent_child_only_issues = positive_issues.clone();
    let closeout_entry = parent_child_only_issues
        .get_mut(closeout_id)
        .expect("closeout issue exists");
    for dep in &mut closeout_entry.dependencies {
        if dep.depends_on_id == "bd-wubqf" {
            dep.dep_type = "parent-child".to_owned();
        }
    }
    closeout_entry.dependencies.push(TrackerDependency {
        issue_id: closeout_id.to_owned(),
        depends_on_id: "bd-wubqf".to_owned(),
        dep_type: "related".to_owned(),
    });
    assert_eq!(
        evaluate_bridge_completion(
            &positive_report,
            &parent_child_only_issues,
            closeout_id,
            required_owners,
            optional_owners,
            &positive_artifacts,
            candidate_commit,
        ),
        BridgeCompletionVerdict::MissingBlockingDependency {
            closeout_id: closeout_id.to_owned(),
            missing_owners: vec!["bd-wubqf".to_owned()],
        }
    );
    append_test_event(
        &events_path,
        "assert",
        "negative_oracle_strip_blocks_edge",
        "pass",
        json!({"missing_owner": "bd-wubqf"}),
    );

    // 7. Negative Oracle B: Closed owner with absent, failed, stale, or wrong-source
    //    behavioral evidence (and changing mtime without changing evidence).
    let mut absent_artifacts = positive_artifacts.clone();
    absent_artifacts.remove("bd-z368i");
    assert_eq!(
        evaluate_bridge_completion(
            &positive_report,
            &positive_issues,
            closeout_id,
            required_owners,
            optional_owners,
            &absent_artifacts,
            candidate_commit,
        ),
        BridgeCompletionVerdict::InvalidBehavioralEvidence {
            owner: "bd-z368i".to_owned(),
            reason: EvidenceFailureReason::Absent,
        }
    );

    let mut failed_artifacts = positive_artifacts.clone();
    failed_artifacts
        .get_mut("bd-reality-core-convergence-1azkt.33")
        .expect("owner present")
        .verdict = "failed".to_owned();
    assert_eq!(
        evaluate_bridge_completion(
            &positive_report,
            &positive_issues,
            closeout_id,
            required_owners,
            optional_owners,
            &failed_artifacts,
            candidate_commit,
        ),
        BridgeCompletionVerdict::InvalidBehavioralEvidence {
            owner: "bd-reality-core-convergence-1azkt.33".to_owned(),
            reason: EvidenceFailureReason::Failed,
        }
    );

    let mut stale_artifacts = positive_artifacts.clone();
    stale_artifacts
        .get_mut("bd-wubqf")
        .expect("owner present")
        .stale = true;
    assert_eq!(
        evaluate_bridge_completion(
            &positive_report,
            &positive_issues,
            closeout_id,
            required_owners,
            optional_owners,
            &stale_artifacts,
            candidate_commit,
        ),
        BridgeCompletionVerdict::InvalidBehavioralEvidence {
            owner: "bd-wubqf".to_owned(),
            reason: EvidenceFailureReason::Stale,
        }
    );

    let mut wrong_source_artifacts = positive_artifacts.clone();
    let entry = wrong_source_artifacts
        .get_mut("bd-wubqf")
        .expect("owner present");
    entry.source_commit = "0000000000000000000000000000000000000000".to_owned();
    // Bump mtime to prove changing mtime without fixing source evidence still fails.
    entry.mtime_epoch = 9_999_999_999;
    assert_eq!(
        evaluate_bridge_completion(
            &positive_report,
            &positive_issues,
            closeout_id,
            required_owners,
            optional_owners,
            &wrong_source_artifacts,
            candidate_commit,
        ),
        BridgeCompletionVerdict::InvalidBehavioralEvidence {
            owner: "bd-wubqf".to_owned(),
            reason: EvidenceFailureReason::WrongSource {
                expected: candidate_commit.to_owned(),
                actual: "0000000000000000000000000000000000000000".to_owned(),
            },
        }
    );
    append_test_event(
        &events_path,
        "assert",
        "negative_oracle_wrong_source_and_mtime",
        "pass",
        json!({"owner": "bd-wubqf"}),
    );

    // 8. Restore both the `blocks` edge and the current-source behavioral artifact ->
    //    require the positive fixture to pass again.
    assert_eq!(
        evaluate_bridge_completion(
            &positive_report,
            &positive_issues,
            closeout_id,
            required_owners,
            optional_owners,
            &positive_artifacts,
            candidate_commit,
        ),
        BridgeCompletionVerdict::Complete {
            closeout_id: closeout_id.to_owned(),
            verified_owners: required_owners.iter().map(|s| (*s).to_owned()).collect(),
            optional_missing: vec!["bd-optional-telemetry.1".to_owned()],
        }
    );

    // 9. Active dependency cycle among non-closed issues -> DependencyCycle.
    let mut cycle_issues = positive_issues.clone();
    cycle_issues.insert(
        "bd-active-cycle.a".to_owned(),
        TrackerIssue {
            id: "bd-active-cycle.a".to_owned(),
            status: "open".to_owned(),
            dependencies: vec![TrackerDependency {
                issue_id: "bd-active-cycle.a".to_owned(),
                depends_on_id: "bd-active-cycle.b".to_owned(),
                dep_type: "blocks".to_owned(),
            }],
        },
    );
    cycle_issues.insert(
        "bd-active-cycle.b".to_owned(),
        TrackerIssue {
            id: "bd-active-cycle.b".to_owned(),
            status: "in_progress".to_owned(),
            dependencies: vec![TrackerDependency {
                issue_id: "bd-active-cycle.b".to_owned(),
                depends_on_id: "bd-active-cycle.a".to_owned(),
                dep_type: "blocks".to_owned(),
            }],
        },
    );
    assert_eq!(
        evaluate_bridge_completion(
            &positive_report,
            &cycle_issues,
            closeout_id,
            required_owners,
            optional_owners,
            &positive_artifacts,
            candidate_commit,
        ),
        BridgeCompletionVerdict::DependencyCycle {
            cycle: vec![
                "bd-active-cycle.a".to_owned(),
                "bd-active-cycle.b".to_owned(),
                "bd-active-cycle.a".to_owned(),
            ],
        }
    );

    // 10. Ingestion boundary supports `br list --json` object-with-issues envelope.
    let envelope_json = json!({
        "issues": [
            {
                "id": "bd-reality-core-convergence-1azkt.22",
                "status": "closed",
                "dependencies": [
                    {
                        "issue_id": "bd-reality-core-convergence-1azkt.22",
                        "depends_on_id": "bd-wubqf",
                        "dependency_type": "blocks"
                    }
                ]
            },
            {
                "id": "bd-wubqf",
                "status": "closed",
                "dependencies": []
            }
        ]
    })
    .to_string();
    let parsed_envelope =
        parse_tracker_issues(&envelope_json).expect("parse object-with-issues envelope");
    let closure = blocks_transitive_closure(&parsed_envelope, closeout_id);
    assert!(closure.contains("bd-wubqf"));

    append_test_event(
        &events_path,
        "cleanup",
        "retain_evidence_no_delete",
        "pass",
        json!({"retained_dir": scratch.display().to_string()}),
    );

    let recorded_events = fs::read_to_string(&events_path).expect("read test_events.jsonl");
    let event_lines: Vec<Value> = recorded_events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("valid ee.test_event.v1 line"))
        .collect();
    assert!(
        event_lines.len() >= 4,
        "expected setup/assert/cleanup test events"
    );
}

#[test]
fn bridge_completion_validator_inspects_live_repository_read_only() {
    let report = run_script().expect("live bridge-staleness.sh must succeed");
    let beads_path = workspace_root().join(".beads").join("issues.jsonl");
    let raw_beads = fs::read_to_string(&beads_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", beads_path.display()));
    let issues = parse_tracker_issues(&raw_beads).expect("parse live .beads/issues.jsonl");

    // Active cycle check on live tracker must be clean.
    assert_eq!(
        detect_active_blocks_cycle(&issues),
        None,
        "live tracker must have zero active blocks dependency cycles"
    );

    let closeout_id = "bd-reality-core-convergence-1azkt.22";
    let closure = blocks_transitive_closure(&issues, closeout_id);
    for mandatory in [
        "bd-reality-core-convergence-1azkt.22",
        "bd-reality-core-convergence-1azkt.27",
        "bd-reality-core-convergence-1azkt.30",
        "bd-reality-core-convergence-1azkt.31",
        "bd-reality-core-convergence-1azkt.33",
        "bd-reality-core-convergence-1azkt.34",
        "bd-wubqf",
        "bd-fy92m",
        "bd-sxalp",
        "bd-no-compile-gate-before-a-commit-lands-qdxc0",
        "bd-no-runnable-current-ee-binary-on-the-mac-wu6ay",
        "bd-z368i",
    ] {
        assert!(
            closure.contains(mandatory),
            "mandatory closeout prerequisite {mandatory} must be in the blocks-only transitive closure of {closeout_id}"
        );
    }

    // Live repo is currently in active Part III with open requirements, so the completion
    // validator must report BehaviorUnproven rather than Complete.
    let verdict = evaluate_bridge_completion(
        &report,
        &issues,
        closeout_id,
        &[closeout_id],
        &[],
        &BTreeMap::new(),
        "live-head",
    );
    match verdict {
        BridgeCompletionVerdict::BehaviorUnproven {
            phase,
            owner_root,
            open_count,
            in_progress_count,
        } => {
            assert_eq!(phase.as_deref(), Some("Part III"));
            assert_eq!(
                owner_root.as_deref(),
                Some("bd-reality-core-convergence-1azkt")
            );
            assert!(
                open_count + in_progress_count > 0,
                "active Part III must report live open or in-progress requirements"
            );
        }
        other => panic!("expected BehaviorUnproven on live active Part III repo, got {other:?}"),
    }
}
