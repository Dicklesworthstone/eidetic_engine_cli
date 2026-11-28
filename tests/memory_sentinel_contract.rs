//! Per-kind contract tests for verifiable memory sentinels (bd-1n0np.16.5)
//! over the landed model in `ee::models::memory_sentinel`.
//!
//! The in-module tests already cover the PathExists happy path, the
//! command-help shell-rejection, malformed-spec repair hints, result-hash
//! stability, and the conservatism mapping. These lock the remaining per-kind
//! contract surface across ALL nine kinds:
//! - every kind round-trips through `parse` / `as_str` (and `FromStr`);
//! - the safety-class taxonomy — exactly one kind escalates to allowlisted
//!   introspection, every other kind is a pure predicate (the structural
//!   no-arbitrary-shell guard);
//! - every kind exposes a non-empty default predicate;
//! - safety-class and result-status tokens round-trip through `parse`.
//!
//! The `json_value_compare` tests (GH #90) drive the real binary: a Gate
//! sentinel withholds its memory from a fresh-sentinel pack once the watched
//! value moves, and a Revive sentinel surfaces its memory once a threshold
//! holds.

use std::fs;
use std::path::PathBuf;

use ee::models::memory_sentinel::{
    MemorySentinelKind, MemorySentinelResultStatus, MemorySentinelSafetyClass,
};
use serde_json::Value;

type TestResult = Result<(), String>;

#[test]
fn every_kind_round_trips_through_parse_and_as_str() {
    let kinds = MemorySentinelKind::all();
    assert_eq!(kinds.len(), 9, "the kind set is nine kinds");
    for kind in kinds {
        let token = kind.as_str();
        assert_eq!(
            MemorySentinelKind::parse(token),
            Some(kind),
            "as_str/parse round trip for {token}"
        );
        assert_eq!(
            token.parse::<MemorySentinelKind>().ok(),
            Some(kind),
            "FromStr parity for {token}"
        );
    }
    assert!(
        MemorySentinelKind::parse("definitely_not_a_sentinel_kind").is_none(),
        "unknown tokens do not parse"
    );
}

#[test]
fn only_command_help_is_allowlisted_introspection_the_rest_are_pure_predicates() {
    // Structural no-arbitrary-shell guard at the model level: exactly one kind
    // escalates to allowlisted introspection; every other kind is a pure
    // filesystem/config/schema/env/fixture predicate.
    for kind in MemorySentinelKind::all() {
        let expected = if matches!(kind, MemorySentinelKind::CommandHelpContainsFlag) {
            MemorySentinelSafetyClass::AllowlistedIntrospection
        } else {
            MemorySentinelSafetyClass::PurePredicate
        };
        assert_eq!(
            kind.safety_class(),
            expected,
            "safety class for {}",
            kind.as_str()
        );
    }
    let allowlisted = MemorySentinelKind::all()
        .into_iter()
        .filter(|kind| kind.safety_class() == MemorySentinelSafetyClass::AllowlistedIntrospection)
        .count();
    assert_eq!(
        allowlisted, 1,
        "exactly one kind may be allowlisted introspection"
    );
}

#[test]
fn every_kind_has_a_nonempty_default_predicate() {
    for kind in MemorySentinelKind::all() {
        assert!(
            !kind.default_predicate().trim().is_empty(),
            "default predicate for {} must be non-empty",
            kind.as_str()
        );
    }
}

#[test]
fn safety_class_and_status_tokens_round_trip_through_parse() {
    for class in [
        MemorySentinelSafetyClass::PurePredicate,
        MemorySentinelSafetyClass::AllowlistedIntrospection,
    ] {
        assert_eq!(
            MemorySentinelSafetyClass::parse(class.as_str()),
            Some(class)
        );
    }
    assert!(MemorySentinelSafetyClass::parse("arbitrary_shell").is_none());

    for status in [
        MemorySentinelResultStatus::Pass,
        MemorySentinelResultStatus::Fail,
        MemorySentinelResultStatus::Unknown,
    ] {
        assert_eq!(
            MemorySentinelResultStatus::parse(status.as_str()),
            Some(status)
        );
    }
    assert!(MemorySentinelResultStatus::parse("maybe").is_none());
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

/// An initialized workspace driven through the real `ee` binary with isolated
/// HOME and XDG dirs.
struct SentinelWorkspace {
    _root: tempfile::TempDir,
    isolation: PathBuf,
    workspace: PathBuf,
}

impl SentinelWorkspace {
    fn init() -> Result<Self, String> {
        let root = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
        let isolation = root.path().join("isolation");
        let workspace = root.path().join("workspace");
        fs::create_dir_all(&workspace)
            .map_err(|error| format!("create {}: {error}", workspace.display()))?;
        let initialized = Self {
            _root: root,
            isolation,
            workspace,
        };
        initialized.run(&["init"])?;
        Ok(initialized)
    }

    fn write_watermark(&self, body: &str) -> TestResult {
        let path = self.workspace.join("watermark.json");
        fs::write(&path, body).map_err(|error| format!("write {}: {error}", path.display()))
    }

    fn run(&self, arguments: &[&str]) -> Result<Value, String> {
        let output = super::isolated_ee::isolated_ee_command(&self.isolation)?
            .arg("--workspace")
            .arg(&self.workspace)
            .arg("--json")
            .args(arguments)
            .current_dir(&self.workspace)
            .output()
            .map_err(|error| format!("spawn ee {arguments:?}: {error}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !output.status.success() {
            return Err(format!(
                "ee {arguments:?} failed: {} (code {:?})\nstdout={stdout}\nstderr={}",
                output.status,
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        serde_json::from_str(&stdout)
            .map_err(|error| format!("ee {arguments:?} stdout is not JSON: {error}\n{stdout}"))
    }

    fn remember(&self, body: &str, extra: &[&str]) -> Result<String, String> {
        let mut arguments = vec!["remember", body];
        arguments.extend_from_slice(extra);
        let remembered = self.run(&arguments)?;
        remembered
            .pointer("/data/memory_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("remember returned no memory id: {remembered}"))
    }
}

fn rows<'a>(document: &'a Value, pointer: &str) -> &'a [Value] {
    document
        .pointer(pointer)
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
}

fn lists_memory(rows: &[Value], memory_id: &str) -> bool {
    rows.iter()
        .any(|row| row.get("memoryId").and_then(Value::as_str) == Some(memory_id))
}

fn sentinel_status(check: &Value, memory_id: &str) -> Option<String> {
    rows(check, "/data/results")
        .iter()
        .find(|row| row.get("memoryId").and_then(Value::as_str) == Some(memory_id))
        .and_then(|row| row.get("status"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

#[test]
fn json_value_compare_is_explained_as_a_pure_predicate() -> TestResult {
    let workspace = SentinelWorkspace::init()?;
    let explained = workspace.run(&["sentinel", "explain"])?;
    let kind = rows(&explained, "/data/kinds")
        .iter()
        .find(|row| row.get("kind").and_then(Value::as_str) == Some("json_value_compare"))
        .ok_or_else(|| format!("sentinel explain does not list json_value_compare: {explained}"))?;
    ensure(
        kind.get("safetyClass").and_then(Value::as_str) == Some("pure_predicate")
            && kind
                .get("targetSyntax")
                .and_then(Value::as_str)
                .is_some_and(|syntax| syntax.contains("#dotted.field<op><json_scalar>")),
        format!("unexpected json_value_compare explain row: {kind}"),
    )
}

#[test]
fn json_value_compare_gate_withholds_the_memory_once_the_watched_value_moves() -> TestResult {
    let workspace = SentinelWorkspace::init()?;
    workspace.write_watermark(r#"{"schema_rev":14,"counters":{"processed_rows":4820}}"#)?;
    let memory_id = workspace.remember(
        "The ingest watermark schema revision is fourteen.",
        &[
            "--level",
            "procedural",
            "--kind",
            "fact",
            "--sentinel",
            "json_value_compare:watermark.json#schema_rev==14",
        ],
    )?;
    let query = "ingest watermark schema revision";

    let passing = workspace.run(&["sentinel", "check", &memory_id])?;
    ensure(
        sentinel_status(&passing, &memory_id).as_deref() == Some("pass"),
        format!("the gate passes while the file says 14: {passing}"),
    )?;
    let served = workspace.run(&["pack", query, "--require-fresh-sentinels"])?;
    ensure(
        lists_memory(rows(&served, "/data/pack/items"), &memory_id),
        format!("a passing gate serves its memory: {served}"),
    )?;

    workspace.write_watermark(r#"{"schema_rev":15,"counters":{"processed_rows":4820}}"#)?;
    let failing = workspace.run(&["sentinel", "check", &memory_id])?;
    ensure(
        sentinel_status(&failing, &memory_id).as_deref() == Some("fail"),
        format!("the gate fails once the file says 15: {failing}"),
    )?;
    let withheld = workspace.run(&["pack", query, "--require-fresh-sentinels"])?;
    ensure(
        !lists_memory(rows(&withheld, "/data/pack/items"), &memory_id)
            && rows(&withheld, "/data/pack/skipped").iter().any(|row| {
                row.get("memoryId").and_then(Value::as_str) == Some(memory_id.as_str())
                    && row.get("reason").and_then(Value::as_str) == Some("excluded_by_policy")
            }),
        format!("a failed gate withholds its memory as a policy omission: {withheld}"),
    )
}

#[test]
fn json_value_compare_revival_surfaces_once_the_threshold_holds() -> TestResult {
    let workspace = SentinelWorkspace::init()?;
    workspace.write_watermark(r#"{"schema_rev":14,"counters":{"processed_rows":4820}}"#)?;
    let memory_id = workspace.remember(
        "Retire the backfill route until five thousand rows are processed.",
        &[
            "--level",
            "episodic",
            "--kind",
            "failure",
            "--revive-when",
            "json_value_compare:watermark.json#counters.processed_rows>=5000",
        ],
    )?;

    let blocked = workspace.run(&["tripwire", "check", "--revivals"])?;
    ensure(
        !lists_memory(rows(&blocked, "/data/revivals"), &memory_id)
            && blocked
                .pointer("/data/summary/fail")
                .and_then(Value::as_u64)
                == Some(1),
        format!("4820 processed rows must not revive the route: {blocked}"),
    )?;

    workspace.write_watermark(r#"{"schema_rev":14,"counters":{"processed_rows":5000}}"#)?;
    let ready = workspace.run(&["tripwire", "check", "--revivals"])?;
    ensure(
        rows(&ready, "/data/revivals").iter().any(|row| {
            row.get("memoryId").and_then(Value::as_str) == Some(memory_id.as_str())
                && row.get("kind").and_then(Value::as_str) == Some("json_value_compare")
                && row.get("polarity").and_then(Value::as_str) == Some("revive")
                && row.get("status").and_then(Value::as_str) == Some("pass")
        }),
        format!("5000 processed rows revive the route: {ready}"),
    )
}
