//! bd-1eq3l.1 — Pure dirty-path classifier for workspace hygiene.
//!
//! The classifier consumes pre-collected workspace facts (a
//! [`WorkspaceGitSnapshot`] from `swarm_brief.rs` plus optional
//! [`WorkspaceSecretRiskReport`] rows from `policy::workspace_secret_risk_evidence`)
//! and emits a deterministic list of [`ClassificationRow`] entries
//! that downstream commit-readiness logic uses to recommend stage
//! groupings.
//!
//! The classifier itself is **pure**:
//!
//! - No `std::fs::*` writes, opens, or reads.
//! - No process-command shell-outs.
//! - No git mutations, no stash, no reset, no checkout, no rm.
//! - No allocation of background tasks; no I/O of any kind.
//!
//! All filesystem facts (metadata, secret-content scans) come in
//! through input arguments — the caller of this module is responsible
//! for collecting them via the read-only adapters in
//! `core::swarm_brief` and `policy`.
//!
//! ## Bucket semantics
//!
//! - [`Bucket::StageCandidate`] — safe to suggest including in a
//!   commit slice (subject to a later reservation-overlay pass; that
//!   lives in `bd-1eq3l.5`, not here).
//! - [`Bucket::DoNotCommit`] — the classifier is confident this path
//!   should never be committed: scratch artifacts, generated build
//!   output, local-machine files, likely secrets, etc.
//! - [`Bucket::NeedsHumanReview`] — the classifier is unsure or has a
//!   policy reason (binary, oversized, secret-risk on a tracked path)
//!   to escalate to a human.
//! - [`Bucket::IgnoreForNow`] — the path is recognized but should be
//!   deferred to a downstream specialist (e.g. `.beads/issues.jsonl`
//!   handled by bd-1eq3l.4, unknown untracked files that may belong
//!   somewhere we haven't reasoned about yet).
//!
//! ## Determinism
//!
//! For the same `(snapshot, secret_evidence)` inputs the output is
//! byte-identical, including ordering and the `reasons` lists. Rows
//! are sorted by `(path, bucket.rank(), kind.rank(), first_reason)`.

use std::collections::BTreeMap;
use std::str::FromStr;

use serde::Serialize;

use crate::core::swarm_brief::{WorkspaceGitSnapshot, WorkspaceGitStatusEntry};
use crate::policy::{WorkspaceSecretRiskEvidence, WorkspaceSecretRiskReport};

/// JSON schema constant for one classification row. Stable across
/// minor versions; bumping the suffix signals a breaking shape change.
pub const HYGIENE_CLASSIFICATION_ROW_SCHEMA_V1: &str = "ee.hygiene_classification_row.v1";

/// Reason codes — short, stable, snake_case identifiers. The classifier
/// may emit multiple reasons per row; the first is treated as primary
/// for ordering purposes.
pub mod reason {
    pub const SRC_RUST_SOURCE: &str = "src_rust_source";
    pub const SRC_CARGO_MANIFEST: &str = "src_cargo_manifest";
    pub const SRC_BUILD_SCRIPT: &str = "src_build_script";
    pub const SRC_MIGRATION: &str = "src_migration_sql";
    pub const TESTS_DIR: &str = "tests_dir";
    pub const TEST_FILE_SUFFIX: &str = "test_file_suffix";
    pub const BENCH_FILE: &str = "bench_file";
    pub const DOCS_DIR: &str = "docs_dir";
    pub const DOCS_MARKDOWN_ROOT: &str = "docs_markdown_root";
    pub const DOCS_LICENSE: &str = "docs_license";
    pub const DOCS_README: &str = "docs_readme";
    pub const BEADS_DIR: &str = "beads_dir";
    pub const BEADS_JSONL: &str = "beads_jsonl";
    pub const GEN_TARGET_DIR: &str = "generated_target_dir";
    pub const GEN_CARGO_LOCK: &str = "generated_cargo_lock";
    pub const GEN_BUILD_ARTIFACT: &str = "generated_build_artifact";
    pub const SCRATCH_ROOT_HELPER: &str = "scratch_root_helper";
    pub const SCRATCH_ROOT_REPORT: &str = "scratch_root_report";
    pub const SCRATCH_ROOT_TOOL_OUTPUT: &str = "scratch_root_tool_output";
    pub const SCRATCH_ROOT_TMP: &str = "scratch_root_tmp";
    pub const SCRATCH_LINE_LENGTH_PROBE: &str = "scratch_line_length_probe";
    pub const LOCAL_APPLE_DOUBLE: &str = "local_apple_double";
    pub const LOCAL_DS_STORE: &str = "local_ds_store";
    pub const LOCAL_WINDOWS_SHELL: &str = "local_windows_shell";
    pub const LOCAL_DB_FILE: &str = "local_db_file";
    pub const LOCAL_LOG_FILE: &str = "local_log_file";
    pub const SECRET_PATH_PATTERN: &str = "secret_path_pattern";
    pub const SECRET_CONTENT_EVIDENCE: &str = "secret_content_evidence";
    pub const BINARY_LARGE_FILE: &str = "binary_large_file";
    pub const BINARY_SKIP_REASON: &str = "binary_skip_reason";
    pub const UNKNOWN_UNTRACKED: &str = "unknown_untracked";
    pub const UNKNOWN_TRACKED: &str = "unknown_tracked";
    pub const SECRET_RISK_OVERRIDES_TRACKED: &str = "secret_risk_overrides_tracked";
    pub const CONFIG_ALWAYS_REVIEW_PATTERN: &str = "config_always_review_pattern";
    pub const CONFIG_GENERATED_PATTERN: &str = "config_generated_pattern";
    pub const CONFIG_LOCAL_MACHINE_PATTERN: &str = "config_local_machine_pattern";
    pub const CONFIG_SCRATCH_PATTERN: &str = "config_scratch_pattern";
}

/// Top-level classification bucket — the action recommendation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Bucket {
    StageCandidate,
    DoNotCommit,
    NeedsHumanReview,
    IgnoreForNow,
}

impl Bucket {
    /// Stable rank for ordering when multiple rows share a path. Lower
    /// rank sorts earlier.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::DoNotCommit => 0,
            Self::NeedsHumanReview => 1,
            Self::StageCandidate => 2,
            Self::IgnoreForNow => 3,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StageCandidate => "stage_candidate",
            Self::DoNotCommit => "do_not_commit",
            Self::NeedsHumanReview => "needs_human_review",
            Self::IgnoreForNow => "ignore_for_now",
        }
    }
}

/// Path-kind taxonomy. Drives the grouping hint in
/// [`ClassificationRow::suggested_group`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Source,
    Test,
    Docs,
    BeadsMetadata,
    Generated,
    Scratch,
    LocalMachine,
    SecretRisk,
    Binary,
    Unknown,
}

impl Kind {
    /// Rank used as a tie-breaker after bucket rank. Lower sorts
    /// earlier.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::SecretRisk => 0,
            Self::LocalMachine => 1,
            Self::Generated => 2,
            Self::Scratch => 3,
            Self::BeadsMetadata => 4,
            Self::Binary => 5,
            Self::Source => 6,
            Self::Test => 7,
            Self::Docs => 8,
            Self::Unknown => 9,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Test => "test",
            Self::Docs => "docs",
            Self::BeadsMetadata => "beads_metadata",
            Self::Generated => "generated",
            Self::Scratch => "scratch",
            Self::LocalMachine => "local_machine",
            Self::SecretRisk => "secret_risk",
            Self::Binary => "binary",
            Self::Unknown => "unknown",
        }
    }
}

/// Compact git-state echo. We carry the raw porcelain `(staged,
/// unstaged)` codes plus the `entry_kind` string from
/// `WorkspaceGitStatusEntry`, plus the rename source path when
/// applicable, so the classification row is self-describing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitState {
    pub staged: String,
    pub unstaged: String,
    pub entry_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_path: Option<String>,
}

impl GitState {
    fn from_entry(entry: &WorkspaceGitStatusEntry) -> Self {
        Self {
            staged: entry.staged.clone(),
            unstaged: entry.unstaged.clone(),
            entry_kind: entry.entry_kind.clone(),
            original_path: entry.original_path.clone(),
        }
    }

    fn is_untracked(&self) -> bool {
        self.entry_kind == "untracked"
    }
}

/// One classifier output row. Self-contained: includes its schema,
/// path, decoded git state, bucket, kind, confidence, reasons, an
/// optional `suggested_group` for downstream commit-slice grouping,
/// and any redacted secret-risk evidence that survived the policy
/// adapter.
///
/// Not `Eq` because `confidence: f32` only impls `PartialEq` — call
/// sites that need full equality should compare the individual fields
/// they care about.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassificationRow {
    pub schema: &'static str,
    pub path: String,
    pub git_state: GitState,
    pub bucket: Bucket,
    pub kind: Kind,
    pub confidence: f32,
    pub reasons: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_group: Option<String>,
    pub redacted_evidence: Vec<WorkspaceSecretRiskEvidence>,
}

/// Path keyed lookup of secret-risk evidence. The classifier consults
/// this map by the (already-normalized) repo-relative path of the
/// entry. Use [`SecretEvidenceLookup::default`] when no secret-risk
/// data is available — the classifier will still compute path-pattern
/// secret-risk hits on its own.
pub type SecretEvidenceLookup = BTreeMap<String, WorkspaceSecretRiskReport>;

/// Simple deterministic path matcher used by the optional classifier
/// configuration. The CLI/config layer owns parsing from TOML or
/// `EE_*`; this core module only consumes already-normalized patterns.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HygienePathPattern {
    Exact(String),
    Prefix(String),
    Suffix(String),
    Contains(String),
}

impl HygienePathPattern {
    #[must_use]
    pub fn exact(pattern: impl Into<String>) -> Self {
        Self::Exact(pattern.into())
    }

    #[must_use]
    pub fn prefix(pattern: impl Into<String>) -> Self {
        Self::Prefix(pattern.into())
    }

    #[must_use]
    pub fn suffix(pattern: impl Into<String>) -> Self {
        Self::Suffix(pattern.into())
    }

    #[must_use]
    pub fn contains(pattern: impl Into<String>) -> Self {
        Self::Contains(pattern.into())
    }

    #[must_use]
    fn matches(&self, path: &str) -> bool {
        match self {
            Self::Exact(pattern) => path == pattern,
            Self::Prefix(pattern) => path.starts_with(pattern),
            Self::Suffix(pattern) => path.ends_with(pattern),
            Self::Contains(pattern) => path.contains(pattern),
        }
    }
}

impl FromStr for HygienePathPattern {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let trimmed = value.trim();
        let (kind, pattern) = trimmed.split_once(':').ok_or_else(|| {
            format!("workspace hygiene path pattern {trimmed:?} must use kind:value syntax")
        })?;
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return Err(format!(
                "workspace hygiene path pattern {trimmed:?} has an empty value"
            ));
        }
        match kind.trim() {
            "exact" => Ok(Self::exact(pattern)),
            "prefix" => Ok(Self::prefix(pattern)),
            "suffix" => Ok(Self::suffix(pattern)),
            "contains" => Ok(Self::contains(pattern)),
            other => Err(format!(
                "workspace hygiene path pattern kind {other:?} is not supported"
            )),
        }
    }
}

/// Optional caller-supplied classifier configuration. Built-in rules
/// always remain active; these lists only add local patterns.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HygieneClassifierConfig {
    pub generated_patterns: Vec<HygienePathPattern>,
    pub scratch_patterns: Vec<HygienePathPattern>,
    pub local_machine_patterns: Vec<HygienePathPattern>,
    pub always_review_patterns: Vec<HygienePathPattern>,
}

impl HygieneClassifierConfig {
    /// Build classifier config from raw environment/config values. The
    /// caller owns reading `EE_*` through `config::env_registry`; this
    /// parser stays pure and deterministic.
    pub fn from_raw_pattern_values(
        generated: Option<&str>,
        scratch: Option<&str>,
        local_machine: Option<&str>,
        always_review: Option<&str>,
    ) -> Result<Self, String> {
        Ok(Self {
            generated_patterns: parse_pattern_list(generated, "generated")?,
            scratch_patterns: parse_pattern_list(scratch, "scratch")?,
            local_machine_patterns: parse_pattern_list(local_machine, "local_machine")?,
            always_review_patterns: parse_pattern_list(always_review, "always_review")?,
        })
    }

    /// Merge a lower-priority base layer with a higher-priority
    /// overlay. Duplicate patterns are deduplicated deterministically.
    #[must_use]
    pub fn merged_with(&self, overlay: &Self) -> Self {
        Self {
            generated_patterns: merge_pattern_lists(
                &self.generated_patterns,
                &overlay.generated_patterns,
            ),
            scratch_patterns: merge_pattern_lists(
                &self.scratch_patterns,
                &overlay.scratch_patterns,
            ),
            local_machine_patterns: merge_pattern_lists(
                &self.local_machine_patterns,
                &overlay.local_machine_patterns,
            ),
            always_review_patterns: merge_pattern_lists(
                &self.always_review_patterns,
                &overlay.always_review_patterns,
            ),
        }
    }
}

fn parse_pattern_list(value: Option<&str>, label: &str) -> Result<Vec<HygienePathPattern>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let mut patterns = Vec::new();
    for raw in value.split(',') {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let pattern = trimmed.parse::<HygienePathPattern>().map_err(|err| {
            format!("invalid workspace hygiene {label} pattern {trimmed:?}: {err}")
        })?;
        patterns.push(pattern);
    }
    patterns.sort();
    patterns.dedup();
    Ok(patterns)
}

fn merge_pattern_lists(
    base: &[HygienePathPattern],
    overlay: &[HygienePathPattern],
) -> Vec<HygienePathPattern> {
    let mut merged = base.to_vec();
    merged.extend_from_slice(overlay);
    merged.sort();
    merged.dedup();
    merged
}

/// Confidence tiers. The exact values are stable and may be asserted
/// in golden tests.
const CONFIDENCE_HIGH: f32 = 0.95;
const CONFIDENCE_MEDIUM_HIGH: f32 = 0.85;
const CONFIDENCE_MEDIUM: f32 = 0.6;
const CONFIDENCE_LOW: f32 = 0.4;

/// Top-level entry point. Returns classification rows sorted by
/// `(path, bucket.rank(), kind.rank(), first_reason)` for determinism.
#[must_use]
pub fn classify_workspace(
    snapshot: &WorkspaceGitSnapshot,
    secret_evidence: &SecretEvidenceLookup,
) -> Vec<ClassificationRow> {
    classify_workspace_with_config(
        snapshot,
        secret_evidence,
        &HygieneClassifierConfig::default(),
    )
}

/// Top-level entry point with caller-supplied config. Returns
/// classification rows sorted by `(path, bucket.rank(), kind.rank(),
/// first_reason)` for determinism.
#[must_use]
pub fn classify_workspace_with_config(
    snapshot: &WorkspaceGitSnapshot,
    secret_evidence: &SecretEvidenceLookup,
    config: &HygieneClassifierConfig,
) -> Vec<ClassificationRow> {
    let mut rows: Vec<ClassificationRow> = snapshot
        .entries
        .iter()
        .map(|entry| classify_entry(entry, secret_evidence, config))
        .collect();
    rows.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.bucket.rank().cmp(&right.bucket.rank()))
            .then_with(|| left.kind.rank().cmp(&right.kind.rank()))
            .then_with(|| {
                left.reasons
                    .first()
                    .copied()
                    .unwrap_or("")
                    .cmp(right.reasons.first().copied().unwrap_or(""))
            })
    });
    rows
}

fn classify_entry(
    entry: &WorkspaceGitStatusEntry,
    secret_evidence: &SecretEvidenceLookup,
    config: &HygieneClassifierConfig,
) -> ClassificationRow {
    let git_state = GitState::from_entry(entry);
    let path = entry.path.clone();
    let metadata_large = entry
        .metadata
        .as_ref()
        .is_some_and(|metadata| metadata.large_file);
    let metadata_skip = entry
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.skip_reason.clone());
    let report = secret_evidence.get(&path);

    // Rule order: secret-risk (highest priority, wins over almost all
    // other classifications because the cost of a false negative is
    // catastrophic), then local-machine artifacts, generated build
    // output, beads metadata, scratch, binary/large, then the
    // commit-friendly buckets (docs, tests, source), then unknown.
    if let Some((reasons, confidence, redacted_evidence)) =
        secret_risk_classification(&path, report)
    {
        let bucket = if git_state.is_untracked() {
            Bucket::DoNotCommit
        } else {
            // A tracked file that hits the secret-risk surface is a
            // human-review item, not a fast doNotCommit: the file is
            // already in git history (perhaps legitimately, e.g. an
            // example config) and the right answer requires a human.
            Bucket::NeedsHumanReview
        };
        return assemble_row(
            path,
            git_state,
            bucket,
            Kind::SecretRisk,
            confidence,
            reasons,
            Some("secret_risk".to_owned()),
            redacted_evidence,
        );
    }

    if matches_any(&config.always_review_patterns, &path) {
        return assemble_row(
            path,
            git_state,
            Bucket::NeedsHumanReview,
            Kind::Unknown,
            CONFIDENCE_MEDIUM_HIGH,
            vec![reason::CONFIG_ALWAYS_REVIEW_PATTERN],
            Some("human_review".to_owned()),
            Vec::new(),
        );
    }

    if matches_any(&config.local_machine_patterns, &path) {
        return assemble_row(
            path,
            git_state,
            Bucket::DoNotCommit,
            Kind::LocalMachine,
            CONFIDENCE_MEDIUM_HIGH,
            vec![reason::CONFIG_LOCAL_MACHINE_PATTERN],
            Some("local_machine".to_owned()),
            Vec::new(),
        );
    }

    if matches_any(&config.generated_patterns, &path) {
        return assemble_row(
            path,
            git_state,
            Bucket::DoNotCommit,
            Kind::Generated,
            CONFIDENCE_MEDIUM_HIGH,
            vec![reason::CONFIG_GENERATED_PATTERN],
            Some("generated".to_owned()),
            Vec::new(),
        );
    }

    if matches_any(&config.scratch_patterns, &path) {
        return assemble_row(
            path,
            git_state,
            Bucket::DoNotCommit,
            Kind::Scratch,
            CONFIDENCE_MEDIUM_HIGH,
            vec![reason::CONFIG_SCRATCH_PATTERN],
            Some("scratch".to_owned()),
            Vec::new(),
        );
    }

    if let Some((reasons, confidence)) = local_machine_classification(&path) {
        return assemble_row(
            path,
            git_state,
            Bucket::DoNotCommit,
            Kind::LocalMachine,
            confidence,
            reasons,
            Some("local_machine".to_owned()),
            Vec::new(),
        );
    }

    if let Some((reasons, confidence)) = generated_classification(&path) {
        return assemble_row(
            path,
            git_state,
            Bucket::DoNotCommit,
            Kind::Generated,
            confidence,
            reasons,
            Some("generated".to_owned()),
            Vec::new(),
        );
    }

    if let Some((reasons, confidence)) = beads_metadata_classification(&path) {
        return assemble_row(
            path,
            git_state,
            Bucket::IgnoreForNow,
            Kind::BeadsMetadata,
            confidence,
            reasons,
            Some("beads_metadata".to_owned()),
            Vec::new(),
        );
    }

    if let Some((reasons, confidence)) = scratch_classification(&path) {
        return assemble_row(
            path,
            git_state,
            Bucket::DoNotCommit,
            Kind::Scratch,
            confidence,
            reasons,
            Some("scratch".to_owned()),
            Vec::new(),
        );
    }

    if metadata_large || metadata_skip.as_deref() == Some("binary") {
        let mut reasons = Vec::new();
        if metadata_large {
            reasons.push(reason::BINARY_LARGE_FILE);
        }
        if let Some(skip) = metadata_skip.as_deref() {
            if skip == "binary" {
                reasons.push(reason::BINARY_SKIP_REASON);
            }
        }
        return assemble_row(
            path,
            git_state,
            Bucket::NeedsHumanReview,
            Kind::Binary,
            CONFIDENCE_MEDIUM,
            reasons,
            Some("binary".to_owned()),
            Vec::new(),
        );
    }

    if let Some((reasons, confidence, suggested_group)) = source_test_docs_classification(&path) {
        let (kind, group_label) = suggested_group;
        return assemble_row(
            path,
            git_state,
            Bucket::StageCandidate,
            kind,
            confidence,
            reasons,
            Some(group_label.to_owned()),
            Vec::new(),
        );
    }

    // Fallthrough: unknown. Tracked unknowns go to NeedsHumanReview
    // (a tracked file the classifier can't categorize warrants a
    // look). Untracked unknowns go to IgnoreForNow — keep the agent
    // out of the way of files the human dropped in.
    let (bucket, reason_code) = if git_state.is_untracked() {
        (Bucket::IgnoreForNow, reason::UNKNOWN_UNTRACKED)
    } else {
        (Bucket::NeedsHumanReview, reason::UNKNOWN_TRACKED)
    };
    assemble_row(
        path,
        git_state,
        bucket,
        Kind::Unknown,
        CONFIDENCE_LOW,
        vec![reason_code],
        None,
        Vec::new(),
    )
}

#[allow(clippy::too_many_arguments)]
fn assemble_row(
    path: String,
    git_state: GitState,
    bucket: Bucket,
    kind: Kind,
    confidence: f32,
    reasons: Vec<&'static str>,
    suggested_group: Option<String>,
    redacted_evidence: Vec<WorkspaceSecretRiskEvidence>,
) -> ClassificationRow {
    ClassificationRow {
        schema: HYGIENE_CLASSIFICATION_ROW_SCHEMA_V1,
        path,
        git_state,
        bucket,
        kind,
        confidence,
        reasons,
        suggested_group,
        redacted_evidence,
    }
}

// --- per-kind classification helpers ----------------------------------------

fn matches_any(patterns: &[HygienePathPattern], path: &str) -> bool {
    patterns.iter().any(|pattern| pattern.matches(path))
}

fn secret_risk_classification(
    path: &str,
    report: Option<&WorkspaceSecretRiskReport>,
) -> Option<(Vec<&'static str>, f32, Vec<WorkspaceSecretRiskEvidence>)> {
    let mut reasons: Vec<&'static str> = Vec::new();
    let mut evidence: Vec<WorkspaceSecretRiskEvidence> = Vec::new();

    if matches_secret_path_pattern(path) {
        reasons.push(reason::SECRET_PATH_PATTERN);
    }
    if let Some(report) = report {
        if report.secret_risk {
            // We always include the path-pattern reason if the report
            // says so via its `risk_classes`, but the report's content
            // evidence is the load-bearing signal.
            if !report.evidence.is_empty() {
                reasons.push(reason::SECRET_CONTENT_EVIDENCE);
                evidence.extend(report.evidence.iter().cloned());
            } else if !report.risk_classes.is_empty()
                && !reasons.contains(&reason::SECRET_PATH_PATTERN)
            {
                reasons.push(reason::SECRET_PATH_PATTERN);
            }
        }
    }
    if reasons.is_empty() {
        None
    } else {
        Some((reasons, CONFIDENCE_HIGH, evidence))
    }
}

/// Lightweight path-only secret-risk heuristic. The real content scan
/// lives in `policy::workspace_secret_risk_evidence`; this fallback
/// flags obvious cases when the caller did not pre-collect evidence.
fn matches_secret_path_pattern(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let file_name = lower.rsplit('/').next().unwrap_or(lower.as_str());
    // Env / credential / token / key patterns. Conservative — we'd
    // rather flag a benign `.env.example` and let the human override
    // than miss a real secret file.
    if file_name == ".env"
        || file_name.starts_with(".env.")
        || file_name.ends_with(".env")
        || file_name.ends_with(".pem")
        || file_name.ends_with(".p12")
        || file_name.ends_with(".pfx")
        || file_name.ends_with(".key")
        || file_name == "id_rsa"
        || file_name == "id_dsa"
        || file_name == "id_ecdsa"
        || file_name == "id_ed25519"
        || file_name == "credentials"
        || file_name == "credentials.json"
        || file_name == "secrets.toml"
        || file_name == "secrets.yaml"
        || file_name == "secrets.yml"
    {
        return true;
    }
    // Cloud-provider conventional credential locations.
    if lower.starts_with(".aws/") || lower.contains("/.aws/") {
        return true;
    }
    if lower.starts_with(".gcp/") || lower.contains("/.gcp/") {
        return true;
    }
    if lower.starts_with(".azure/") || lower.contains("/.azure/") {
        return true;
    }
    false
}

fn local_machine_classification(path: &str) -> Option<(Vec<&'static str>, f32)> {
    let file_name = path.rsplit('/').next().unwrap_or(path);
    if file_name == ".DS_Store" {
        return Some((vec![reason::LOCAL_DS_STORE], CONFIDENCE_HIGH));
    }
    // AppleDouble files: `._foo` at any depth. These are macOS metadata
    // sidecars that show up on shared volumes; never want them in git.
    if file_name.starts_with("._") {
        return Some((vec![reason::LOCAL_APPLE_DOUBLE], CONFIDENCE_HIGH));
    }
    if file_name == "Thumbs.db" || file_name == "desktop.ini" {
        return Some((vec![reason::LOCAL_WINDOWS_SHELL], CONFIDENCE_HIGH));
    }
    // Local SQLite / log files at the repo root or under top-level
    // workspace dirs are dev-machine state, not source.
    let lower_name = file_name.to_ascii_lowercase();
    if lower_name.ends_with(".db")
        || lower_name.ends_with(".sqlite")
        || lower_name.ends_with(".sqlite3")
    {
        return Some((vec![reason::LOCAL_DB_FILE], CONFIDENCE_MEDIUM_HIGH));
    }
    if lower_name.ends_with(".log") {
        return Some((vec![reason::LOCAL_LOG_FILE], CONFIDENCE_MEDIUM_HIGH));
    }
    None
}

fn generated_classification(path: &str) -> Option<(Vec<&'static str>, f32)> {
    if path == "target" || path.starts_with("target/") {
        return Some((vec![reason::GEN_TARGET_DIR], CONFIDENCE_HIGH));
    }
    if path == "Cargo.lock" {
        // Cargo.lock is tracked in libraries-without-binaries and
        // generated in apps. ee is a CLI app, so it IS tracked. But
        // when it appears as dirty, the change should be reviewed
        // separately from source. Mark it generated → DoNotCommit
        // here; the staging-recommendation layer (bd-1eq3l.2) can
        // override on its own policy.
        return Some((vec![reason::GEN_CARGO_LOCK], CONFIDENCE_MEDIUM));
    }
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".o")
        || lower.ends_with(".a")
        || lower.ends_with(".so")
        || lower.ends_with(".dylib")
        || lower.ends_with(".dll")
        || lower.ends_with(".rlib")
    {
        return Some((vec![reason::GEN_BUILD_ARTIFACT], CONFIDENCE_HIGH));
    }
    None
}

fn beads_metadata_classification(path: &str) -> Option<(Vec<&'static str>, f32)> {
    if path == ".beads/issues.jsonl" {
        return Some((vec![reason::BEADS_JSONL], CONFIDENCE_HIGH));
    }
    if path == ".beads" || path.starts_with(".beads/") {
        return Some((vec![reason::BEADS_DIR], CONFIDENCE_HIGH));
    }
    None
}

fn scratch_classification(path: &str) -> Option<(Vec<&'static str>, f32)> {
    // Root-only scratch helpers. We use `!path.contains('/')` to scope
    // these matches to top-level files — a file named `--help` deep
    // inside a fixture is meaningful test data, but at the repo root
    // it is debris from `ee --help > --help`.
    let at_root = !path.contains('/');
    if at_root {
        if path == "--help" || path == "-h" {
            return Some((vec![reason::SCRATCH_ROOT_HELPER], CONFIDENCE_HIGH));
        }
        if path.starts_with("ubs-report")
            || path.starts_with("ubs_report")
            || path.starts_with("ubs.")
            || path.starts_with("ubs_")
            || path.starts_with("drift-report")
            || path.starts_with("drift_report")
            || path.starts_with(".plan-drift-report")
            || path.starts_with(".plan_drift_report")
            || path.ends_with("-report.txt")
            || path.ends_with("_report.txt")
        {
            return Some((vec![reason::SCRATCH_ROOT_REPORT], CONFIDENCE_HIGH));
        }
        if path == "critical.json" || path == "functions.txt" {
            return Some((vec![reason::SCRATCH_ROOT_TOOL_OUTPUT], CONFIDENCE_HIGH));
        }
        if path.starts_with("tmp.")
            || path == "tmp"
            || path.starts_with("scratch")
            || path.ends_with(".tmp")
            || path.ends_with(".bak")
            || path.ends_with(".swp")
            || path.ends_with(".orig")
        {
            return Some((vec![reason::SCRATCH_ROOT_TMP], CONFIDENCE_MEDIUM_HIGH));
        }
        if path.starts_with("line-length-probe") || path.starts_with("line_length_probe") {
            return Some((vec![reason::SCRATCH_LINE_LENGTH_PROBE], CONFIDENCE_HIGH));
        }
        if path.starts_with("test_ln_") || path.starts_with("test_multibyte") {
            return Some((vec![reason::SCRATCH_LINE_LENGTH_PROBE], CONFIDENCE_HIGH));
        }
    }
    None
}

fn source_test_docs_classification(
    path: &str,
) -> Option<(Vec<&'static str>, f32, (Kind, &'static str))> {
    // Tests / benches first because `tests/fixtures/**` would otherwise
    // be ambiguous with general source.
    if path.starts_with("tests/") || path == "tests" {
        return Some((
            vec![reason::TESTS_DIR],
            CONFIDENCE_MEDIUM_HIGH,
            (Kind::Test, "tests"),
        ));
    }
    if path.starts_with("benches/") || path == "benches" {
        return Some((
            vec![reason::BENCH_FILE],
            CONFIDENCE_MEDIUM_HIGH,
            (Kind::Test, "benches"),
        ));
    }
    let lower = path.to_ascii_lowercase();
    if let Some(file_name) = path.rsplit('/').next() {
        let lower_file = file_name.to_ascii_lowercase();
        if lower_file.ends_with("_test.rs") || lower_file.ends_with("_tests.rs") {
            return Some((
                vec![reason::TEST_FILE_SUFFIX],
                CONFIDENCE_MEDIUM_HIGH,
                (Kind::Test, "tests"),
            ));
        }
    }

    // Docs.
    if path.starts_with("docs/") || path == "docs" {
        return Some((
            vec![reason::DOCS_DIR],
            CONFIDENCE_MEDIUM_HIGH,
            (Kind::Docs, "docs"),
        ));
    }
    if !path.contains('/') && lower.ends_with(".md") {
        return Some((
            vec![reason::DOCS_MARKDOWN_ROOT],
            CONFIDENCE_MEDIUM_HIGH,
            (Kind::Docs, "docs"),
        ));
    }
    if !path.contains('/') {
        let upper = path.to_ascii_uppercase();
        if upper.starts_with("README") {
            return Some((
                vec![reason::DOCS_README],
                CONFIDENCE_MEDIUM_HIGH,
                (Kind::Docs, "docs"),
            ));
        }
        if upper.starts_with("LICENSE") || upper.starts_with("NOTICE") {
            return Some((
                vec![reason::DOCS_LICENSE],
                CONFIDENCE_MEDIUM_HIGH,
                (Kind::Docs, "docs"),
            ));
        }
    }

    // Source.
    if path.starts_with("src/") {
        if lower.ends_with(".rs") {
            return Some((
                vec![reason::SRC_RUST_SOURCE],
                CONFIDENCE_MEDIUM_HIGH,
                (Kind::Source, "source"),
            ));
        }
    }
    if path == "Cargo.toml" {
        return Some((
            vec![reason::SRC_CARGO_MANIFEST],
            CONFIDENCE_MEDIUM_HIGH,
            (Kind::Source, "source"),
        ));
    }
    if path == "build.rs" {
        return Some((
            vec![reason::SRC_BUILD_SCRIPT],
            CONFIDENCE_MEDIUM_HIGH,
            (Kind::Source, "source"),
        ));
    }
    if path.starts_with("migrations/") && lower.ends_with(".sql") {
        return Some((
            vec![reason::SRC_MIGRATION],
            CONFIDENCE_MEDIUM_HIGH,
            (Kind::Source, "migrations"),
        ));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::swarm_brief::{WorkspaceGitPathMetadata, WorkspaceGitStatusEntry};

    fn entry(
        path: &str,
        staged: &str,
        unstaged: &str,
        entry_kind: &str,
    ) -> WorkspaceGitStatusEntry {
        WorkspaceGitStatusEntry {
            path: path.to_owned(),
            original_path: None,
            staged: staged.to_owned(),
            unstaged: unstaged.to_owned(),
            entry_kind: entry_kind.to_owned(),
            submodule_state: None,
            metadata: None,
        }
    }

    fn untracked(path: &str) -> WorkspaceGitStatusEntry {
        entry(path, "?", "?", "untracked")
    }

    fn staged_added(path: &str) -> WorkspaceGitStatusEntry {
        entry(path, "A", ".", "ordinary")
    }

    fn unstaged_modified(path: &str) -> WorkspaceGitStatusEntry {
        entry(path, ".", "M", "ordinary")
    }

    fn snapshot(entries: Vec<WorkspaceGitStatusEntry>) -> WorkspaceGitSnapshot {
        WorkspaceGitSnapshot {
            repository_root: "/tmp/test-repo".to_owned(),
            entries,
        }
    }

    fn no_secret_evidence() -> SecretEvidenceLookup {
        SecretEvidenceLookup::default()
    }

    #[test]
    fn empty_status_returns_empty_rows() {
        let snap = snapshot(Vec::new());
        let rows = classify_workspace(&snap, &no_secret_evidence());
        assert!(rows.is_empty());
    }

    #[test]
    fn source_only_path_classifies_as_stage_candidate_source() {
        let snap = snapshot(vec![untracked("src/core/foo.rs")]);
        let rows = classify_workspace(&snap, &no_secret_evidence());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "src/core/foo.rs");
        assert_eq!(rows[0].kind, Kind::Source);
        assert_eq!(rows[0].bucket, Bucket::StageCandidate);
        assert!(rows[0].reasons.contains(&reason::SRC_RUST_SOURCE));
        assert_eq!(rows[0].suggested_group.as_deref(), Some("source"));
        assert!(rows[0].redacted_evidence.is_empty());
    }

    #[test]
    fn docs_and_tests_paths_classify_as_stage_candidates_with_distinct_groups() {
        let snap = snapshot(vec![
            unstaged_modified("docs/foo.md"),
            staged_added("tests/contracts/bar.rs"),
        ]);
        let rows = classify_workspace(&snap, &no_secret_evidence());
        assert_eq!(rows.len(), 2);
        let docs_row = rows
            .iter()
            .find(|r| r.path == "docs/foo.md")
            .expect("docs row");
        let test_row = rows
            .iter()
            .find(|r| r.path == "tests/contracts/bar.rs")
            .expect("tests row");
        assert_eq!(docs_row.kind, Kind::Docs);
        assert_eq!(docs_row.bucket, Bucket::StageCandidate);
        assert_eq!(docs_row.suggested_group.as_deref(), Some("docs"));
        assert_eq!(test_row.kind, Kind::Test);
        assert_eq!(test_row.bucket, Bucket::StageCandidate);
        assert_eq!(test_row.suggested_group.as_deref(), Some("tests"));
    }

    #[test]
    fn beads_jsonl_classifies_as_ignore_for_now_beads_metadata() {
        let snap = snapshot(vec![unstaged_modified(".beads/issues.jsonl")]);
        let rows = classify_workspace(&snap, &no_secret_evidence());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, Kind::BeadsMetadata);
        assert_eq!(rows[0].bucket, Bucket::IgnoreForNow);
        assert!(rows[0].reasons.contains(&reason::BEADS_JSONL));
        assert_eq!(rows[0].suggested_group.as_deref(), Some("beads_metadata"));
    }

    #[test]
    fn scratch_root_artifacts_classify_as_do_not_commit_scratch() {
        // The literal `--help` file (which `ee --help > --help` would
        // create), an ad-hoc UBS report, and a drift report.
        let snap = snapshot(vec![
            untracked("--help"),
            untracked("ubs-report-2026-05-16.txt"),
            untracked("ubs.json"),
            untracked("ubs_findings.jsonl"),
            untracked("ubs_full.txt"),
            untracked("drift-report.txt"),
            untracked(".plan-drift-report.json"),
            untracked("critical.json"),
            untracked("functions.txt"),
            untracked("line-length-probe-output.txt"),
            untracked("test_ln_1p"),
            untracked("test_ln_1p.rs"),
            untracked("test_multibyte"),
            untracked("test_multibyte.rs"),
        ]);
        let rows = classify_workspace(&snap, &no_secret_evidence());
        assert_eq!(rows.len(), 14);
        for row in &rows {
            assert_eq!(row.kind, Kind::Scratch, "{} should be scratch", row.path);
            assert_eq!(
                row.bucket,
                Bucket::DoNotCommit,
                "{} should be DoNotCommit",
                row.path
            );
        }
        let help_row = rows.iter().find(|r| r.path == "--help").unwrap();
        assert!(help_row.reasons.contains(&reason::SCRATCH_ROOT_HELPER));
        let probe_row = rows
            .iter()
            .find(|r| r.path == "line-length-probe-output.txt")
            .unwrap();
        assert!(
            probe_row
                .reasons
                .contains(&reason::SCRATCH_LINE_LENGTH_PROBE)
        );
        let ubs_json_row = rows.iter().find(|r| r.path == "ubs.json").unwrap();
        assert!(ubs_json_row.reasons.contains(&reason::SCRATCH_ROOT_REPORT));
        let plan_drift_row = rows
            .iter()
            .find(|r| r.path == ".plan-drift-report.json")
            .unwrap();
        assert!(
            plan_drift_row
                .reasons
                .contains(&reason::SCRATCH_ROOT_REPORT)
        );
        let tool_output_row = rows.iter().find(|r| r.path == "functions.txt").unwrap();
        assert!(
            tool_output_row
                .reasons
                .contains(&reason::SCRATCH_ROOT_TOOL_OUTPUT)
        );
        let multibyte_row = rows.iter().find(|r| r.path == "test_multibyte.rs").unwrap();
        assert!(
            multibyte_row
                .reasons
                .contains(&reason::SCRATCH_LINE_LENGTH_PROBE)
        );
    }

    #[test]
    fn generated_target_outputs_classify_as_do_not_commit_generated() {
        let snap = snapshot(vec![
            untracked("target/debug/ee"),
            untracked("target/release/deps/foo.rlib"),
            untracked("Cargo.lock"),
        ]);
        let rows = classify_workspace(&snap, &no_secret_evidence());
        for row in &rows {
            assert_eq!(
                row.kind,
                Kind::Generated,
                "{} should be generated",
                row.path
            );
            assert_eq!(
                row.bucket,
                Bucket::DoNotCommit,
                "{} should be DoNotCommit",
                row.path
            );
        }
        let lock_row = rows.iter().find(|r| r.path == "Cargo.lock").unwrap();
        assert!(lock_row.reasons.contains(&reason::GEN_CARGO_LOCK));
    }

    #[test]
    fn local_machine_artifacts_classify_as_do_not_commit_local_machine() {
        let snap = snapshot(vec![
            untracked(".DS_Store"),
            untracked("src/core/._mod.rs"),
            untracked("test.db"),
            untracked("app.log"),
            untracked("Thumbs.db"),
        ]);
        let rows = classify_workspace(&snap, &no_secret_evidence());
        for row in &rows {
            assert_eq!(
                row.kind,
                Kind::LocalMachine,
                "{} should be local_machine",
                row.path
            );
            assert_eq!(
                row.bucket,
                Bucket::DoNotCommit,
                "{} should be DoNotCommit",
                row.path
            );
        }
        let ds = rows.iter().find(|r| r.path == ".DS_Store").unwrap();
        assert!(ds.reasons.contains(&reason::LOCAL_DS_STORE));
        let apple_double = rows.iter().find(|r| r.path == "src/core/._mod.rs").unwrap();
        assert!(apple_double.reasons.contains(&reason::LOCAL_APPLE_DOUBLE));
        let win = rows.iter().find(|r| r.path == "Thumbs.db").unwrap();
        assert!(win.reasons.contains(&reason::LOCAL_WINDOWS_SHELL));
    }

    #[test]
    fn large_binary_files_classify_as_needs_human_review_binary() {
        let mut entry = unstaged_modified("assets/big.bin");
        entry.metadata = Some(WorkspaceGitPathMetadata {
            exists: true,
            file_type: "regular".to_owned(),
            size_bytes: Some(10 * 1024 * 1024),
            large_file: true,
            skip_reason: Some("binary".to_owned()),
        });
        let snap = snapshot(vec![entry]);
        let rows = classify_workspace(&snap, &no_secret_evidence());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, Kind::Binary);
        assert_eq!(rows[0].bucket, Bucket::NeedsHumanReview);
        assert!(rows[0].reasons.contains(&reason::BINARY_LARGE_FILE));
        assert!(rows[0].reasons.contains(&reason::BINARY_SKIP_REASON));
    }

    #[test]
    fn unknown_untracked_files_go_to_ignore_for_now_unknown() {
        // A file with no recognized prefix/suffix and outside any
        // categorized directory.
        let snap = snapshot(vec![untracked("somewhere/odd-file")]);
        let rows = classify_workspace(&snap, &no_secret_evidence());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, Kind::Unknown);
        assert_eq!(rows[0].bucket, Bucket::IgnoreForNow);
        assert!(rows[0].reasons.contains(&reason::UNKNOWN_UNTRACKED));
    }

    #[test]
    fn unknown_tracked_files_go_to_needs_human_review_unknown() {
        let snap = snapshot(vec![unstaged_modified("somewhere/odd-file")]);
        let rows = classify_workspace(&snap, &no_secret_evidence());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, Kind::Unknown);
        assert_eq!(rows[0].bucket, Bucket::NeedsHumanReview);
        assert!(rows[0].reasons.contains(&reason::UNKNOWN_TRACKED));
    }

    #[test]
    fn secret_risk_path_pattern_alone_classifies_as_do_not_commit_secret_risk() {
        let snap = snapshot(vec![untracked(".env")]);
        let rows = classify_workspace(&snap, &no_secret_evidence());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, Kind::SecretRisk);
        assert_eq!(rows[0].bucket, Bucket::DoNotCommit);
        assert!(rows[0].reasons.contains(&reason::SECRET_PATH_PATTERN));
        assert!(rows[0].redacted_evidence.is_empty());
    }

    #[test]
    fn config_patterns_extend_generated_scratch_and_local_machine_classification() {
        let config = HygieneClassifierConfig {
            generated_patterns: vec![HygienePathPattern::prefix("fixtures/generated/")],
            scratch_patterns: vec![HygienePathPattern::suffix(".scratch.json")],
            local_machine_patterns: vec![HygienePathPattern::exact(".local-agent-state")],
            always_review_patterns: Vec::new(),
        };
        let snap = snapshot(vec![
            untracked("fixtures/generated/report.json"),
            untracked("notes.scratch.json"),
            untracked(".local-agent-state"),
        ]);
        let rows = classify_workspace_with_config(&snap, &no_secret_evidence(), &config);
        assert_eq!(rows.len(), 3);
        let generated = rows
            .iter()
            .find(|row| row.path == "fixtures/generated/report.json")
            .unwrap();
        assert_eq!(generated.kind, Kind::Generated);
        assert_eq!(generated.bucket, Bucket::DoNotCommit);
        assert!(
            generated
                .reasons
                .contains(&reason::CONFIG_GENERATED_PATTERN)
        );

        let scratch = rows
            .iter()
            .find(|row| row.path == "notes.scratch.json")
            .unwrap();
        assert_eq!(scratch.kind, Kind::Scratch);
        assert_eq!(scratch.bucket, Bucket::DoNotCommit);
        assert!(scratch.reasons.contains(&reason::CONFIG_SCRATCH_PATTERN));

        let local = rows
            .iter()
            .find(|row| row.path == ".local-agent-state")
            .unwrap();
        assert_eq!(local.kind, Kind::LocalMachine);
        assert_eq!(local.bucket, Bucket::DoNotCommit);
        assert!(
            local
                .reasons
                .contains(&reason::CONFIG_LOCAL_MACHINE_PATTERN)
        );
    }

    #[test]
    fn config_always_review_overrides_stage_candidate_paths() {
        let config = HygieneClassifierConfig {
            always_review_patterns: vec![HygienePathPattern::prefix("src/generated/")],
            ..HygieneClassifierConfig::default()
        };
        let snap = snapshot(vec![untracked("src/generated/api.rs")]);
        let rows = classify_workspace_with_config(&snap, &no_secret_evidence(), &config);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].bucket, Bucket::NeedsHumanReview);
        assert_eq!(rows[0].kind, Kind::Unknown);
        assert!(
            rows[0]
                .reasons
                .contains(&reason::CONFIG_ALWAYS_REVIEW_PATTERN)
        );
        assert_eq!(rows[0].suggested_group.as_deref(), Some("human_review"));
    }

    #[test]
    fn secret_risk_overrides_configured_scratch_or_generated_patterns() {
        let config = HygieneClassifierConfig {
            scratch_patterns: vec![HygienePathPattern::exact(".env")],
            generated_patterns: vec![HygienePathPattern::suffix(".env")],
            ..HygieneClassifierConfig::default()
        };
        let snap = snapshot(vec![untracked(".env")]);
        let rows = classify_workspace_with_config(&snap, &no_secret_evidence(), &config);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, Kind::SecretRisk);
        assert_eq!(rows[0].bucket, Bucket::DoNotCommit);
        assert!(rows[0].reasons.contains(&reason::SECRET_PATH_PATTERN));
        assert!(!rows[0].reasons.contains(&reason::CONFIG_SCRATCH_PATTERN));
        assert!(!rows[0].reasons.contains(&reason::CONFIG_GENERATED_PATTERN));
    }

    #[test]
    fn classifier_config_merge_deduplicates_patterns_deterministically() {
        let base = HygieneClassifierConfig {
            generated_patterns: vec![
                HygienePathPattern::prefix("generated/"),
                HygienePathPattern::suffix(".pb.rs"),
            ],
            scratch_patterns: vec![HygienePathPattern::exact("scratch.json")],
            local_machine_patterns: Vec::new(),
            always_review_patterns: vec![HygienePathPattern::contains("/review/")],
        };
        let overlay = HygieneClassifierConfig {
            generated_patterns: vec![
                HygienePathPattern::suffix(".pb.rs"),
                HygienePathPattern::prefix("out/"),
            ],
            scratch_patterns: Vec::new(),
            local_machine_patterns: vec![HygienePathPattern::exact(".machine-state")],
            always_review_patterns: vec![HygienePathPattern::contains("/review/")],
        };
        let merged = base.merged_with(&overlay);
        assert_eq!(
            merged.generated_patterns,
            vec![
                HygienePathPattern::Prefix("generated/".to_owned()),
                HygienePathPattern::Prefix("out/".to_owned()),
                HygienePathPattern::Suffix(".pb.rs".to_owned()),
            ]
        );
        assert_eq!(
            merged.scratch_patterns,
            vec![HygienePathPattern::Exact("scratch.json".to_owned())]
        );
        assert_eq!(
            merged.local_machine_patterns,
            vec![HygienePathPattern::Exact(".machine-state".to_owned())]
        );
        assert_eq!(
            merged.always_review_patterns,
            vec![HygienePathPattern::Contains("/review/".to_owned())]
        );
    }

    #[test]
    fn raw_pattern_values_parse_documented_env_syntax_and_deduplicate() {
        let config = HygieneClassifierConfig::from_raw_pattern_values(
            Some("prefix:target/, suffix:.rlib, prefix:target/"),
            Some("exact:--help,contains:/tmp-probe/"),
            Some("suffix:.local.db"),
            Some("contains:/manual-review/"),
        )
        .expect("config parses");
        assert_eq!(
            config.generated_patterns,
            vec![
                HygienePathPattern::Prefix("target/".to_owned()),
                HygienePathPattern::Suffix(".rlib".to_owned()),
            ]
        );
        assert_eq!(
            config.scratch_patterns,
            vec![
                HygienePathPattern::Exact("--help".to_owned()),
                HygienePathPattern::Contains("/tmp-probe/".to_owned()),
            ]
        );
        assert_eq!(
            config.local_machine_patterns,
            vec![HygienePathPattern::Suffix(".local.db".to_owned())]
        );
        assert_eq!(
            config.always_review_patterns,
            vec![HygienePathPattern::Contains("/manual-review/".to_owned())]
        );
    }

    #[test]
    fn raw_pattern_values_reject_unknown_matcher_kinds_and_empty_values() {
        let bad_kind = HygieneClassifierConfig::from_raw_pattern_values(
            Some("glob:target/**"),
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(bad_kind.contains("glob"));

        let empty_value =
            HygieneClassifierConfig::from_raw_pattern_values(None, Some("prefix:"), None, None)
                .unwrap_err();
        assert!(empty_value.contains("empty value"));
    }

    #[test]
    fn secret_risk_with_evidence_carries_redacted_evidence_forward() {
        let evidence = WorkspaceSecretRiskEvidence {
            risk_class: "content_secret",
            pattern_id: "aws_access_key",
            line: Some(7),
            hash_prefix: Some("abcd1234".to_owned()),
            redacted: "AKIA****".to_owned(),
        };
        let report = WorkspaceSecretRiskReport {
            schema: "ee.workspace_secret_risk.v1",
            path: "configs/app.toml".to_owned(),
            secret_risk: true,
            skipped_content_scan: false,
            risk_classes: vec!["content_secret"],
            reasons: vec!["secret_match"],
            evidence: vec![evidence.clone()],
        };
        let mut lookup = SecretEvidenceLookup::default();
        lookup.insert("configs/app.toml".to_owned(), report);
        let snap = snapshot(vec![untracked("configs/app.toml")]);
        let rows = classify_workspace(&snap, &lookup);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, Kind::SecretRisk);
        assert_eq!(rows[0].bucket, Bucket::DoNotCommit);
        assert!(rows[0].reasons.contains(&reason::SECRET_CONTENT_EVIDENCE));
        // The redacted evidence is preserved exactly — no raw secret
        // material is ever introduced by the classifier.
        assert_eq!(rows[0].redacted_evidence, vec![evidence]);
    }

    #[test]
    fn tracked_secret_risk_paths_route_to_needs_human_review_not_do_not_commit() {
        // The path matches a secret pattern but the entry is tracked
        // (staged modification). We escalate to human review because
        // the file is already in history.
        let snap = snapshot(vec![staged_added(".env")]);
        let rows = classify_workspace(&snap, &no_secret_evidence());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, Kind::SecretRisk);
        assert_eq!(rows[0].bucket, Bucket::NeedsHumanReview);
    }

    #[test]
    fn rows_are_sorted_byte_stable_by_path_then_bucket_then_kind() {
        // Same path duplicate is not possible in real snapshots, so we
        // exercise ordering across heterogenous paths.
        let snap = snapshot(vec![
            untracked("z/last.rs"),
            untracked("a/first.rs"),
            untracked("--help"),
            untracked(".beads/issues.jsonl"),
            untracked("src/core/mid.rs"),
        ]);
        let rows = classify_workspace(&snap, &no_secret_evidence());
        let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "--help",
                ".beads/issues.jsonl",
                "a/first.rs",
                "src/core/mid.rs",
                "z/last.rs",
            ],
            "path ordering must be byte-stable"
        );
    }

    #[test]
    fn purity_module_does_not_perform_io() {
        // Compile-time evidence is what we really want: this module's
        // `use` declarations name no filesystem writers, process
        // commands, or networking APIs. This guard keeps that contract
        // visible without embedding the forbidden tokens contiguously
        // in the source it scans.
        let source = include_str!("hygiene_classifier.rs");
        for (prefix, suffix) in [
            ("std::fs::", "write"),
            ("std::fs::", "create_dir"),
            ("std::fs::", "remove_file"),
            ("std::fs::", "rename"),
            ("std::process::", "Command"),
            ("std::process::", "Child"),
            ("tokio::", "process"),
        ] {
            let forbidden = format!("{prefix}{suffix}");
            assert!(
                !source.contains(&forbidden),
                "hygiene_classifier.rs is supposed to be pure but contains `{forbidden}`"
            );
        }
    }

    #[test]
    fn confidence_values_are_finite_for_every_emitted_row() {
        // Drive every classification arm and assert each row has a
        // finite confidence — guards against future regressions where
        // someone introduces NaN via arithmetic.
        let mut large_binary = unstaged_modified("assets/big.bin");
        large_binary.metadata = Some(WorkspaceGitPathMetadata {
            exists: true,
            file_type: "regular".to_owned(),
            size_bytes: Some(20 * 1024 * 1024),
            large_file: true,
            skip_reason: Some("binary".to_owned()),
        });
        let snap = snapshot(vec![
            untracked("src/foo.rs"),
            untracked("tests/foo.rs"),
            untracked("docs/x.md"),
            untracked(".beads/issues.jsonl"),
            untracked("--help"),
            untracked("target/debug/ee"),
            untracked(".DS_Store"),
            large_binary,
            untracked("somewhere/odd"),
            untracked(".env"),
        ]);
        let rows = classify_workspace(&snap, &no_secret_evidence());
        for row in rows {
            assert!(
                row.confidence.is_finite() && (0.0..=1.0).contains(&row.confidence),
                "confidence out of range for {:?}: {}",
                row.path,
                row.confidence
            );
        }
    }
}
