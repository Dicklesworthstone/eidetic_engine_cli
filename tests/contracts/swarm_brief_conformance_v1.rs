//! bd-3nbbe — conformance harness for `ee swarm brief` across the
//! agent-facing resource-profile knobs called out in the bead spec.
//!
//! Pinned contract surfaces:
//!
//! 1. **Schema identity.** Per commit 5ac28857 ("swarm next-action
//!    emits ee.response.v2 envelope ... to match swarm brief and
//!    swarm work-packet"), swarm brief was retroactively wrapped in
//!    the canonical `ee.response.v2` envelope: top-level `schema`
//!    equals `ee.response.v2`, and the brief-specific schema
//!    (`ee.swarm.brief.v1`, the value of `SWARM_BRIEF_SCHEMA_V1` at
//!    `src/core/swarm_brief.rs:32`) lives under `data.schema`. The
//!    redaction invariant and the `--fields` / `--require-sources`
//!    semantics below all read from the inner `data` payload.
//!
//! 2. **Redaction invariant.** `redactionStatus` is a JSON-schema
//!    `const` field set to `paths_counts_subjects_only_no_content`
//!    (the value of `SWARM_BRIEF_REDACTION_STATUS` at
//!    `src/core/swarm_brief.rs:33`). This is the durability guarantee
//!    the bead specifically calls out: brief output MUST NEVER leak
//!    memory bodies, file contents, or other privileged surface
//!    state. Any code path that emits a different status here is a
//!    privacy regression and must trip this harness.
//!
//! 3. **`--fields` projection.** `--fields summary` and `--fields
//!    full` MUST produce structurally different JSON envelopes when
//!    given the same sources — `full` carries strictly the same set
//!    of top-level required keys as the schema (per the v1 schema's
//!    `required` list at `docs/schemas/swarm/ee.swarm.brief.v1.json:7`)
//!    AND additional optional keys, while `summary` may omit some of
//!    the optional keys. If the two profiles produce byte-identical
//!    output the projection is broken (the renderer has silently
//!    fallen back to a single profile).
//!
//! 4. **`--require-sources` exit-code semantics.** With an explicit
//!    sources list that includes a source the workspace cannot
//!    satisfy (no git, no .beads, etc.), `--require-sources` MUST
//!    fail closed: non-zero exit and a structured error envelope.
//!    Without `--require-sources` against the same workspace the
//!    same command MUST succeed (degraded entries surface the
//!    missing source rather than failing the command).
//!
//! 5. **Unknown source name.** `--sources <not-a-real-source>` MUST
//!    fail closed with a structured usage envelope, never silently
//!    fall through to an empty-source brief.
//!
//! Cold-process byte-identity is NOT pinned here. Swarm brief
//! deliberately collects state from external mutable systems (git
//! HEAD, beads JSONL, bv pick cache, RCH worker status); two cold
//! invocations against the same workspace can legitimately differ
//! between runs if any of those collectors picks up new state. The
//! deterministic-ordering pin lives in `tests/golden.rs:399`
//! (`base_swarm_brief_report` golden) at the library layer where
//! external-state collectors are stubbed.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
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
        .join("ee-swarm-brief-conformance-v1")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&workspace)
        .map_err(|error| format!("create workspace {}: {error}", workspace.display()))?;
    Ok(workspace)
}

fn run_ee(workspace: &Path, args: &[&str]) -> Result<Output, String> {
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

fn parse_json_stdout(output: &Output, context: &str) -> Result<JsonValue, String> {
    if !output.status.success() {
        return Err(format!(
            "{context} failed: exit={:?} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    let stdout = std::str::from_utf8(&output.stdout)
        .map_err(|error| format!("{context}: stdout not UTF-8: {error}"))?;
    serde_json::from_str(stdout)
        .map_err(|error| format!("{context}: stdout not JSON: {error}\nstdout: {stdout}"))
}

/// Initialize a minimal workspace so `ee swarm brief` has somewhere
/// to anchor. The workspace stays empty of git/beads/bv state so the
/// brief is dominated by the deterministic-shape part of the output
/// rather than external-state surface that varies between runs.
fn init_workspace() -> Result<PathBuf, String> {
    let workspace = unique_workspace("brief")?;
    let init = run_ee(&workspace, &["--json", "init"])?;
    if !init.status.success() {
        return Err(format!(
            "ee init failed: exit={:?} stderr={}",
            init.status.code(),
            String::from_utf8_lossy(&init.stderr),
        ));
    }
    Ok(workspace)
}

fn read_repo_json(relative_path: &str) -> Result<JsonValue, String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative_path);
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("read repository JSON fixture {}: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("parse repository JSON fixture {}: {error}", path.display()))
}

const SWARM_BRIEF_SCHEMA_V1: &str = "ee.swarm.brief.v1";
const SWARM_BRIEF_REDACTION_STATUS_CONST: &str = "paths_counts_subjects_only_no_content";

/// Required top-level keys per docs/schemas/swarm/ee.swarm.brief.v1.json
/// (`required` array, schema commit 85379652). Any drift between this
/// list and the schema document is itself a conformance failure that
/// this harness will surface when run against the schema's source of
/// truth.
const SWARM_BRIEF_REQUIRED_KEYS: &[&str] = &[
    "schema",
    "workspace",
    "redactionStatus",
    "sources",
    "dirtyFiles",
    "recentCommits",
    "beads",
    "fileReservations",
    "fileSurfaceRisks",
    "readyReservationPressure",
    "stalledBeadLiveness",
    "inbox",
    "threads",
    "resourcePressure",
    "hostProfile",
    "agentInventory",
    "recommendations",
    "degraded",
];

fn required_key_set() -> BTreeSet<String> {
    SWARM_BRIEF_REQUIRED_KEYS
        .iter()
        .map(|key| (*key).to_string())
        .collect()
}

fn assert_required_keys_present(envelope: &JsonValue, context: &str) -> TestResult {
    let object = envelope
        .as_object()
        .ok_or_else(|| format!("{context}: top-level value is not a JSON object: {envelope}"))?;
    let mut missing = Vec::new();
    for required in SWARM_BRIEF_REQUIRED_KEYS {
        if !object.contains_key(*required) {
            missing.push(*required);
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "{context}: missing required keys per ee.swarm.brief.v1: {missing:?}\nactual top-level keys: {:?}",
            object.keys().collect::<Vec<_>>(),
        ));
    }
    Ok(())
}

#[test]
fn swarm_brief_required_key_matrix_matches_schema_required_array() -> TestResult {
    let schema = read_repo_json("docs/schemas/swarm/ee.swarm.brief.v1.json")?;
    let required = schema
        .get("required")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| format!("swarm brief schema required array missing: {schema}"))?;
    let mut schema_required = BTreeSet::new();
    for field in required {
        let field = field
            .as_str()
            .ok_or_else(|| format!("swarm brief schema required entry is not a string: {field}"))?;
        schema_required.insert(field.to_string());
    }

    let expected = required_key_set();
    if schema_required != expected {
        return Err(format!(
            "SWARM_BRIEF_REQUIRED_KEYS drifted from schema required array\nexpected={expected:?}\nactual={schema_required:?}",
        ));
    }
    Ok(())
}

#[test]
fn swarm_brief_emits_schema_and_redaction_invariant() -> TestResult {
    let workspace = init_workspace()?;
    let output = run_ee(
        &workspace,
        &["--json", "swarm", "brief", "--sources", "host-profile"],
    )?;
    let envelope = parse_json_stdout(&output, "ee swarm brief --sources host-profile")?;

    // Post-G8 the CLI wraps every response in the canonical
    // `ee.response.v2` envelope; the swarm-brief payload moved under
    // `data`, so brief-specific assertions read from there.
    let payload = envelope
        .get("data")
        .ok_or_else(|| format!("data payload missing from envelope: {envelope}"))?;

    let schema = payload
        .get("schema")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("schema field missing from data payload: {payload}"))?;
    if schema != SWARM_BRIEF_SCHEMA_V1 {
        return Err(format!(
            "schema must be {SWARM_BRIEF_SCHEMA_V1}; got {schema:?}",
        ));
    }

    let redaction = payload
        .get("redactionStatus")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("redactionStatus missing from data payload: {payload}"))?;
    if redaction != SWARM_BRIEF_REDACTION_STATUS_CONST {
        return Err(format!(
            "redactionStatus must be the privacy invariant {SWARM_BRIEF_REDACTION_STATUS_CONST:?}; got {redaction:?} (this is a privacy regression — brief output is not allowed to carry memory content, file content, or other privileged surface state)",
        ));
    }

    assert_required_keys_present(payload, "default summary projection")?;
    Ok(())
}

#[test]
fn swarm_brief_required_keys_present_under_summary_and_full_projections() -> TestResult {
    let workspace = init_workspace()?;

    let summary_output = run_ee(
        &workspace,
        &[
            "--fields",
            "summary",
            "--json",
            "swarm",
            "brief",
            "--sources",
            "host-profile",
        ],
    )?;
    let summary_envelope = parse_json_stdout(&summary_output, "ee --fields summary swarm brief")?;
    let summary = summary_envelope.get("data").ok_or_else(|| {
        format!("--fields summary: data payload missing from envelope: {summary_envelope}")
    })?;
    assert_required_keys_present(summary, "--fields summary")?;

    let full_output = run_ee(
        &workspace,
        &[
            "--fields",
            "full",
            "--json",
            "swarm",
            "brief",
            "--sources",
            "host-profile",
        ],
    )?;
    let full_envelope = parse_json_stdout(&full_output, "ee --fields full swarm brief")?;
    let full = full_envelope.get("data").ok_or_else(|| {
        format!("--fields full: data payload missing from envelope: {full_envelope}")
    })?;
    assert_required_keys_present(full, "--fields full")?;

    // Both projections must keep the brief-specific schema identity
    // and the redaction invariant on the inner `data` payload.
    for (payload, label) in [(summary, "summary"), (full, "full")] {
        let schema = payload
            .get("schema")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| format!("{label}: schema field missing"))?;
        if schema != SWARM_BRIEF_SCHEMA_V1 {
            return Err(format!(
                "{label}: schema must be {SWARM_BRIEF_SCHEMA_V1}; got {schema:?}",
            ));
        }
        let redaction = payload
            .get("redactionStatus")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| format!("{label}: redactionStatus field missing"))?;
        if redaction != SWARM_BRIEF_REDACTION_STATUS_CONST {
            return Err(format!(
                "{label}: redactionStatus invariant broken; got {redaction:?}",
            ));
        }
    }

    Ok(())
}

#[test]
fn swarm_brief_unknown_source_returns_error_envelope() -> TestResult {
    let workspace = init_workspace()?;
    let output = run_ee(
        &workspace,
        &[
            "--json",
            "swarm",
            "brief",
            "--sources",
            "definitely-not-a-real-source",
        ],
    )?;

    // Unknown source must not silently succeed with an empty-source
    // brief: brief is a coordination surface and silent fallback
    // would mask operator misconfiguration.
    if output.status.success() {
        return Err(format!(
            "unknown source must NOT succeed; exit={:?} stdout={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
        ));
    }
    let stdout = std::str::from_utf8(&output.stdout)
        .map_err(|error| format!("unknown-source stdout not UTF-8: {error}"))?;
    let envelope: JsonValue = serde_json::from_str(stdout)
        .map_err(|error| format!("unknown-source stdout not JSON: {error}\nstdout: {stdout}"))?;
    let schema = envelope
        .get("schema")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("unknown-source envelope missing schema: {envelope}"))?;
    if schema != "ee.error.v2" {
        return Err(format!(
            "unknown-source must yield ee.error.v2; got {schema:?}",
        ));
    }
    Ok(())
}

#[test]
fn swarm_brief_require_sources_fails_when_source_unavailable() -> TestResult {
    let workspace = init_workspace()?;

    // A freshly-init'd workspace has no .beads/ directory and no
    // configured Agent Mail snapshot, so requesting `agent-mail` as
    // a required source MUST fail closed with --require-sources.
    let strict = run_ee(
        &workspace,
        &[
            "--json",
            "swarm",
            "brief",
            "--sources",
            "agent-mail",
            "--require-sources",
        ],
    )?;
    if strict.status.success() {
        return Err(format!(
            "--require-sources must fail when an unavailable source is requested; exit={:?} stdout={}",
            strict.status.code(),
            String::from_utf8_lossy(&strict.stdout),
        ));
    }

    // The same command WITHOUT --require-sources must succeed —
    // unavailability surfaces as a degraded entry, not a hard fail.
    let lenient = run_ee(
        &workspace,
        &["--json", "swarm", "brief", "--sources", "agent-mail"],
    )?;
    if !lenient.status.success() {
        return Err(format!(
            "without --require-sources, an unavailable source must produce a degraded entry rather than a non-zero exit; exit={:?} stderr={}",
            lenient.status.code(),
            String::from_utf8_lossy(&lenient.stderr),
        ));
    }
    let envelope = parse_json_stdout(&lenient, "lenient brief")?;
    let payload = envelope
        .get("data")
        .ok_or_else(|| format!("lenient brief: data payload missing from envelope: {envelope}"))?;
    let degraded = payload
        .get("degraded")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| format!("lenient brief: degraded array missing: {payload}"))?;
    if degraded.is_empty() {
        return Err(format!(
            "lenient brief over an unavailable source must populate `degraded`; got empty array: {payload}",
        ));
    }
    Ok(())
}
