//! bd-1n0np.16.3 — pure-predicate memory sentinel checker (per-kind I/O dispatch).
//!
//! A sentinel (model layer: [`crate::models::memory_sentinel`]) is a declarative,
//! deterministic validity predicate attached to a durable memory. This module is
//! the v1 checker: it resolves a sentinel's target against current local state
//! and produces a [`SentinelObservation`], which the model's decision core maps
//! to a [`MemorySentinelResultStatus`] via the conservatism rule (an
//! unverifiable check is `Unknown`, NEVER `Fail` — `ee` never mutates on a
//! sentinel result, so a false `Fail` would be the costly error).
//!
//! Safety envelope (ADR 0060):
//! - Every probe is read-only and bounded by a [`RequestBudget`] (strict
//!   per-check I/O + wall-clock caps); an oversize/unreadable referent resolves
//!   to `Unverifiable`, never to a slurp.
//! - Workspace targets resolve strictly *inside* the workspace root: absolute
//!   paths, `..` traversal, and path prefixes are rejected (`Unverifiable`).
//! - No process execution on this implicit path. The allowlisted-introspection
//!   kind (`CommandHelpContainsFlag`) runs ONLY on the explicit `ee sentinel
//!   check` surface (bd-1n0np.16.4); here it is conservatively `Unverifiable`.
//!
//! v1 fully implements the registry-free pure kinds (env-var membership, path
//! existence, file hash/marker, JSON-schema field presence). The kinds that need
//! infrastructure not present in the pure core (config-key registry, dependency
//! capability resolver, eval fixture discovery) and the allowlisted-introspection
//! kind resolve to `Unverifiable` with a documented reason — never a silent pass
//! and never a false fail.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crate::config::env_registry::EnvVar;
use crate::core::budget::RequestBudget;
use crate::core::source_run::{
    SourceRunArgvRedaction, SourceRunCommand, SourceRunKind, SourceRunRequest, SourceRunSource,
    SourceRunStatus, run_source_command,
};
use crate::models::memory_sentinel::{
    MemorySentinelKind, MemorySentinelResultStatus, MemorySentinelSpec, SentinelObservation,
};

/// Strict per-check I/O cap: a single probe reads at most this many bytes. A
/// referent larger than the cap resolves to `Unverifiable` — the checker will
/// not slurp an unbounded file to answer a predicate.
pub const MAX_SENTINEL_CHECK_IO_BYTES: u64 = 1_048_576; // 1 MiB

/// Strict per-check wall-clock cap.
pub const SENTINEL_CHECK_WALL_CLOCK: Duration = Duration::from_millis(250);

/// Maximum captured bytes per command-help stream. The bounded source runner
/// retains only this redacted tail; any larger stream makes the predicate
/// unverifiable so a flag outside the retained tail cannot fabricate a result.
pub const MAX_SENTINEL_COMMAND_HELP_STREAM_BYTES: usize = 32 * 1024;

/// Resolution context for a sentinel check: the workspace root that relative
/// path / schema / fixture targets resolve against.
#[derive(Clone, Copy, Debug)]
pub struct SentinelCheckContext<'a> {
    pub workspace_root: &'a Path,
}

impl<'a> SentinelCheckContext<'a> {
    #[must_use]
    pub fn new(workspace_root: &'a Path) -> Self {
        Self { workspace_root }
    }
}

/// A fresh per-check budget with the strict sentinel caps applied.
#[must_use]
pub fn sentinel_check_budget() -> RequestBudget {
    RequestBudget::unbounded()
        .with_io_bytes(MAX_SENTINEL_CHECK_IO_BYTES)
        .with_wall_clock(SENTINEL_CHECK_WALL_CLOCK)
}

/// Observe one sentinel spec against current local state (bd-1n0np.16.3).
///
/// Deterministic, conservative, and panic-free: any resolution or I/O ambiguity
/// yields [`SentinelObservation::Unverifiable`] (never `Unsatisfied`), so the
/// decision core maps it to `Unknown`, never `Fail`.
#[must_use]
pub fn observe_sentinel(
    spec: &MemorySentinelSpec,
    ctx: SentinelCheckContext<'_>,
) -> SentinelObservation {
    match spec.sentinel_kind {
        MemorySentinelKind::EnvVarRegistered => observe_env_var_registered(&spec.target),
        MemorySentinelKind::PathExists => observe_path_exists(&spec.target, ctx),
        MemorySentinelKind::FileHashOrMarker => observe_file_hash_or_marker(&spec.target, ctx),
        MemorySentinelKind::JsonSchemaContainsField => {
            observe_json_schema_contains_field(&spec.target, ctx)
        }
        // Conservative v1 (no silent cap): these resolve to Unverifiable
        // (-> Unknown) because the pure core lacks the backing infrastructure:
        //  * ConfigKeyExists            — no enumerated config-key registry exists yet.
        //  * DependencyCapabilityPresent — no capability resolver is wired here.
        //  * DegradedCodeFixtureExists  — needs eval fixture discovery wiring.
        //  * CommandHelpContainsFlag    — allowlisted introspection runs ONLY on the
        //    explicit `ee sentinel check` surface (bd-1n0np.16.4); no process
        //    execution on this implicit path (ADR 0060).
        MemorySentinelKind::ConfigKeyExists
        | MemorySentinelKind::DependencyCapabilityPresent
        | MemorySentinelKind::DegradedCodeFixtureExists
        | MemorySentinelKind::CommandHelpContainsFlag => SentinelObservation::Unverifiable,
    }
}

/// Observe one sentinel spec from the explicit public `ee sentinel check`
/// surface.
///
/// ADR 0060 permits the allowlisted command-help predicate only here. It still
/// does not execute a shell or arbitrary binaries: the target must have already
/// validated as an `ee ... --flag` predicate, and this function resolves the
/// current executable directly.
#[must_use]
pub fn observe_sentinel_explicit(
    spec: &MemorySentinelSpec,
    ctx: SentinelCheckContext<'_>,
) -> SentinelObservation {
    match spec.sentinel_kind {
        MemorySentinelKind::CommandHelpContainsFlag => {
            observe_command_help_contains_flag(&spec.target)
        }
        _ => observe_sentinel(spec, ctx),
    }
}

/// Observe a sentinel then map to its durable result status (conservatism rule
/// applied via [`SentinelObservation::into_status`]).
#[must_use]
pub fn check_sentinel_status(
    spec: &MemorySentinelSpec,
    ctx: SentinelCheckContext<'_>,
) -> MemorySentinelResultStatus {
    observe_sentinel(spec, ctx).into_status()
}

// --- per-kind probes -------------------------------------------------------

fn observe_env_var_registered(target: &str) -> SentinelObservation {
    let name = target.trim();
    if name.is_empty() {
        return SentinelObservation::Unverifiable;
    }
    if EnvVar::all().iter().any(|var| var.name() == name) {
        SentinelObservation::Satisfied
    } else {
        SentinelObservation::Unsatisfied
    }
}

fn observe_path_exists(target: &str, ctx: SentinelCheckContext<'_>) -> SentinelObservation {
    let (path_part, _) = split_target_marker(target);
    match resolve_in_workspace(ctx.workspace_root, path_part) {
        Some(resolved) if resolved.exists() => SentinelObservation::Satisfied,
        Some(_) => SentinelObservation::Unsatisfied,
        None => SentinelObservation::Unverifiable,
    }
}

fn observe_file_hash_or_marker(target: &str, ctx: SentinelCheckContext<'_>) -> SentinelObservation {
    let (path_part, marker) = split_target_marker(target);
    let Some(resolved) = resolve_in_workspace(ctx.workspace_root, path_part) else {
        return SentinelObservation::Unverifiable;
    };
    let bytes = match read_capped(&resolved) {
        CappedRead::Missing => return SentinelObservation::Unsatisfied,
        CappedRead::Unreadable => return SentinelObservation::Unverifiable,
        CappedRead::Contents(bytes) => bytes,
    };
    let Some(expected) = marker.map(str::trim).filter(|m| !m.is_empty()) else {
        // No marker: the default predicate is "hash_or_marker_present" — a
        // present, readable file satisfies it.
        return SentinelObservation::Satisfied;
    };
    if let Some(expected_hex) = expected.strip_prefix("blake3:") {
        let actual_hex = blake3::hash(&bytes).to_hex();
        if actual_hex.as_str().eq_ignore_ascii_case(expected_hex) {
            SentinelObservation::Satisfied
        } else {
            SentinelObservation::Unsatisfied
        }
    } else {
        // Treat the marker as a UTF-8 substring; a binary file is ambiguous.
        match std::str::from_utf8(&bytes) {
            Ok(text) if text.contains(expected) => SentinelObservation::Satisfied,
            Ok(_) => SentinelObservation::Unsatisfied,
            Err(_) => SentinelObservation::Unverifiable,
        }
    }
}

fn observe_json_schema_contains_field(
    target: &str,
    ctx: SentinelCheckContext<'_>,
) -> SentinelObservation {
    let (path_part, field) = split_target_marker(target);
    let Some(field) = field.map(str::trim).filter(|f| !f.is_empty()) else {
        // Without a `#<field>` selector the predicate is unanswerable.
        return SentinelObservation::Unverifiable;
    };
    let Some(resolved) = resolve_in_workspace(ctx.workspace_root, path_part) else {
        return SentinelObservation::Unverifiable;
    };
    let bytes = match read_capped(&resolved) {
        CappedRead::Missing => return SentinelObservation::Unsatisfied,
        CappedRead::Unreadable => return SentinelObservation::Unverifiable,
        CappedRead::Contents(bytes) => bytes,
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return SentinelObservation::Unverifiable;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        // Unparseable JSON is ambiguous, not a definitive negative.
        return SentinelObservation::Unverifiable;
    };
    if json_has_dotted_field(&value, field) {
        SentinelObservation::Satisfied
    } else {
        SentinelObservation::Unsatisfied
    }
}

fn observe_command_help_contains_flag(target: &str) -> SentinelObservation {
    let mut parts = target.split_whitespace();
    if parts.next() != Some("ee") {
        return SentinelObservation::Unverifiable;
    }

    let mut help_args = Vec::new();
    let mut expected_flag = None;
    for part in parts {
        if part.starts_with("--") {
            expected_flag = Some(part.to_string());
            break;
        }
        help_args.push(part.to_string());
    }
    let Some(expected_flag) = expected_flag else {
        return SentinelObservation::Unverifiable;
    };

    let Ok(exe) = std::env::current_exe() else {
        return SentinelObservation::Unverifiable;
    };
    help_args.push("--help".to_string());
    let request = SourceRunRequest::new(
        SourceRunSource::new(
            SourceRunKind::Ee,
            "memory_sentinel_command_help",
            "allowlisted_help_introspection",
        ),
        SourceRunCommand::new(exe.to_string_lossy().into_owned())
            .with_args(help_args)
            .with_display("ee <allowlisted-subcommand> --help")
            .with_argv_redaction(SourceRunArgvRedaction::HashOnly),
        SENTINEL_CHECK_WALL_CLOCK,
    )
    .with_tail_bytes_max(MAX_SENTINEL_COMMAND_HELP_STREAM_BYTES);
    let evidence = run_source_command(&request);

    command_help_observation(
        &expected_flag,
        evidence.status,
        evidence.output.stdout_bytes,
        evidence.output.stderr_bytes,
        evidence.output.stdout_tail.as_deref(),
        evidence.output.stderr_tail.as_deref(),
    )
}

fn command_help_observation(
    expected_flag: &str,
    status: SourceRunStatus,
    stdout_bytes: usize,
    stderr_bytes: usize,
    stdout_tail: Option<&str>,
    stderr_tail: Option<&str>,
) -> SentinelObservation {
    if status != SourceRunStatus::Passed
        || stdout_bytes > MAX_SENTINEL_COMMAND_HELP_STREAM_BYTES
        || stderr_bytes > MAX_SENTINEL_COMMAND_HELP_STREAM_BYTES
    {
        return SentinelObservation::Unverifiable;
    }
    if stdout_tail.is_some_and(|stdout| stdout.contains(expected_flag))
        || stderr_tail.is_some_and(|stderr| stderr.contains(expected_flag))
    {
        SentinelObservation::Satisfied
    } else {
        SentinelObservation::Unsatisfied
    }
}

// --- helpers ---------------------------------------------------------------

/// Split a target into its path part and optional `#`-suffixed marker/field.
/// The path is trimmed; the marker is returned raw (callers trim as needed).
fn split_target_marker(target: &str) -> (&str, Option<&str>) {
    match target.split_once('#') {
        Some((path, marker)) => (path.trim(), Some(marker)),
        None => (target.trim(), None),
    }
}

/// Resolve a relative target strictly inside the workspace root. Rejects empty,
/// absolute, prefixed, and parent-traversing targets (returns `None`).
fn resolve_in_workspace(root: &Path, relative: &str) -> Option<PathBuf> {
    let relative = relative.trim();
    if relative.is_empty() {
        return None;
    }
    let candidate = Path::new(relative);
    if candidate.is_absolute() {
        return None;
    }
    let mut resolved = root.to_path_buf();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => resolved.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(resolved)
}

/// Whether a dotted JSON field path resolves to a present value.
fn json_has_dotted_field(root: &serde_json::Value, dotted: &str) -> bool {
    let mut current = root;
    for segment in dotted.split('.') {
        if segment.is_empty() {
            return false;
        }
        match current.get(segment) {
            Some(next) => current = next,
            None => return false,
        }
    }
    true
}

/// Outcome of a budgeted file read.
enum CappedRead {
    /// File read in full, within the per-check I/O cap.
    Contents(Vec<u8>),
    /// The referent does not exist (or is not a regular file).
    Missing,
    /// The referent exists but could not be read within the cap (oversize,
    /// permissions, or other I/O error) — an ambiguous, not negative, result.
    Unreadable,
}

/// Read a file under the strict per-check I/O budget. A file larger than the cap
/// is `Unreadable` rather than truncated, so the checker never half-reads.
fn read_capped(path: &Path) -> CappedRead {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return CappedRead::Missing,
        Err(_) => return CappedRead::Unreadable,
    };
    if !metadata.is_file() {
        return CappedRead::Missing;
    }
    if metadata.len() > MAX_SENTINEL_CHECK_IO_BYTES {
        return CappedRead::Unreadable;
    }
    match std::fs::read(path) {
        Ok(bytes) => {
            let mut budget = sentinel_check_budget();
            budget.record_io_bytes(bytes.len() as u64);
            if budget.check().is_err() {
                return CappedRead::Unreadable;
            }
            CappedRead::Contents(bytes)
        }
        Err(_) => CappedRead::Unreadable,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{
        MAX_SENTINEL_COMMAND_HELP_STREAM_BYTES, SentinelCheckContext, command_help_observation,
        observe_sentinel, resolve_in_workspace, split_target_marker,
    };
    use crate::config::env_registry::EnvVar;
    use crate::core::source_run::SourceRunStatus;
    use crate::models::memory_sentinel::{
        MEMORY_SENTINEL_SPEC_SCHEMA_V1, MemorySentinelKind, MemorySentinelPolarity,
        MemorySentinelSpec, SentinelObservation,
    };

    fn spec(kind: MemorySentinelKind, target: &str) -> MemorySentinelSpec {
        MemorySentinelSpec {
            schema: MEMORY_SENTINEL_SPEC_SCHEMA_V1,
            spec_hash: "blake3:test".to_string(),
            memory_id: "mem_test".to_string(),
            sentinel_kind: kind,
            polarity: MemorySentinelPolarity::Gate,
            target: target.to_string(),
            expected_predicate: kind.default_predicate().to_string(),
            safety_class: kind.safety_class(),
            provenance: "test".to_string(),
            stale_threshold_seconds: None,
        }
    }

    fn observe(kind: MemorySentinelKind, target: &str, root: &Path) -> SentinelObservation {
        observe_sentinel(&spec(kind, target), SentinelCheckContext::new(root))
    }

    #[test]
    fn env_var_registered_membership() {
        let root = Path::new(".");
        let registered = EnvVar::all()[0].name();
        assert_eq!(
            observe(MemorySentinelKind::EnvVarRegistered, registered, root),
            SentinelObservation::Satisfied
        );
        assert_eq!(
            observe(
                MemorySentinelKind::EnvVarRegistered,
                "EE_DEFINITELY_NOT_A_REAL_VARIABLE_XYZ",
                root
            ),
            SentinelObservation::Unsatisfied
        );
    }

    #[test]
    fn command_help_probe_pass_fail_timeout_and_overflow_are_bounded() {
        assert_eq!(
            command_help_observation(
                "--json",
                SourceRunStatus::Passed,
                18,
                0,
                Some("Usage: ee pack --json"),
                None,
            ),
            SentinelObservation::Satisfied
        );
        assert_eq!(
            command_help_observation(
                "--json",
                SourceRunStatus::Passed,
                15,
                0,
                Some("Usage: ee pack"),
                None,
            ),
            SentinelObservation::Unsatisfied
        );

        let secret = "AKIAIOSFODNN7EXAMPLE-timeout-output";
        let timed_out = command_help_observation(
            "--json",
            SourceRunStatus::TimedOut,
            secret.len(),
            0,
            Some(secret),
            None,
        );
        assert_eq!(timed_out, SentinelObservation::Unverifiable);
        assert!(!format!("{timed_out:?}").contains(secret));

        let overflow = command_help_observation(
            "--json",
            SourceRunStatus::Passed,
            MAX_SENTINEL_COMMAND_HELP_STREAM_BYTES + 1,
            0,
            Some(secret),
            None,
        );
        assert_eq!(overflow, SentinelObservation::Unverifiable);
        assert!(!format!("{overflow:?}").contains(secret));
    }

    #[test]
    fn resolve_in_workspace_rejects_escape_and_absolute() {
        let root = Path::new("/tmp/ws");
        assert!(resolve_in_workspace(root, "a/b.txt").is_some());
        assert!(resolve_in_workspace(root, "./a/b.txt").is_some());
        assert!(resolve_in_workspace(root, "../escape").is_none());
        assert!(resolve_in_workspace(root, "a/../../escape").is_none());
        assert!(resolve_in_workspace(root, "/abs/path").is_none());
        assert!(resolve_in_workspace(root, "   ").is_none());
    }

    #[test]
    fn split_target_marker_splits_on_first_hash() {
        assert_eq!(split_target_marker("path/to.json"), ("path/to.json", None));
        assert_eq!(
            split_target_marker("path/to.json#a.b"),
            ("path/to.json", Some("a.b"))
        );
    }

    #[test]
    fn path_exists_satisfied_missing_and_escape() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::write(root.join("present.txt"), b"hi").expect("write");
        assert_eq!(
            observe(MemorySentinelKind::PathExists, "present.txt", root),
            SentinelObservation::Satisfied
        );
        assert_eq!(
            observe(MemorySentinelKind::PathExists, "absent.txt", root),
            SentinelObservation::Unsatisfied
        );
        // Escape attempt is conservative, never a definitive fail.
        assert_eq!(
            observe(MemorySentinelKind::PathExists, "../outside", root),
            SentinelObservation::Unverifiable
        );
    }

    #[test]
    fn file_hash_or_marker_substring_and_hash() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::write(root.join("notes.txt"), b"alpha beta gamma").expect("write");
        // No marker: present file satisfies.
        assert_eq!(
            observe(MemorySentinelKind::FileHashOrMarker, "notes.txt", root),
            SentinelObservation::Satisfied
        );
        // Substring marker present / absent.
        assert_eq!(
            observe(MemorySentinelKind::FileHashOrMarker, "notes.txt#beta", root),
            SentinelObservation::Satisfied
        );
        assert_eq!(
            observe(
                MemorySentinelKind::FileHashOrMarker,
                "notes.txt#missing",
                root
            ),
            SentinelObservation::Unsatisfied
        );
        // Exact blake3 hash matches.
        let hash = blake3::hash(b"alpha beta gamma").to_hex();
        let target = format!("notes.txt#blake3:{hash}");
        assert_eq!(
            observe(MemorySentinelKind::FileHashOrMarker, &target, root),
            SentinelObservation::Satisfied
        );
        assert_eq!(
            observe(
                MemorySentinelKind::FileHashOrMarker,
                "notes.txt#blake3:0000000000000000000000000000000000000000000000000000000000000000",
                root
            ),
            SentinelObservation::Unsatisfied
        );
        // Missing file is a definitive negative.
        assert_eq!(
            observe(MemorySentinelKind::FileHashOrMarker, "gone.txt#beta", root),
            SentinelObservation::Unsatisfied
        );
    }

    #[test]
    fn json_schema_contains_field_dotted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::write(root.join("schema.json"), br#"{"a":{"b":1},"c":2}"#).expect("write");
        assert_eq!(
            observe(
                MemorySentinelKind::JsonSchemaContainsField,
                "schema.json#a.b",
                root
            ),
            SentinelObservation::Satisfied
        );
        assert_eq!(
            observe(
                MemorySentinelKind::JsonSchemaContainsField,
                "schema.json#a.z",
                root
            ),
            SentinelObservation::Unsatisfied
        );
        // No field selector -> unanswerable -> conservative.
        assert_eq!(
            observe(
                MemorySentinelKind::JsonSchemaContainsField,
                "schema.json",
                root
            ),
            SentinelObservation::Unverifiable
        );
        // Missing file is a definitive negative.
        assert_eq!(
            observe(
                MemorySentinelKind::JsonSchemaContainsField,
                "absent.json#a.b",
                root
            ),
            SentinelObservation::Unsatisfied
        );
    }

    #[test]
    fn deferred_kinds_are_conservatively_unverifiable() {
        let root = Path::new(".");
        for (kind, target) in [
            (MemorySentinelKind::ConfigKeyExists, "pack.budget.tokens"),
            (
                MemorySentinelKind::DependencyCapabilityPresent,
                "frankensearch:rerank",
            ),
            (
                MemorySentinelKind::DegradedCodeFixtureExists,
                "some_fixture",
            ),
            (
                MemorySentinelKind::CommandHelpContainsFlag,
                "ee pack --json",
            ),
        ] {
            assert_eq!(
                observe(kind, target, root),
                SentinelObservation::Unverifiable,
                "{} must be conservatively unverifiable in v1",
                kind.as_str()
            );
        }
    }
}
