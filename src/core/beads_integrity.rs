//! Beads tracker integrity diagnostics (bd-2z5ly.9).
//!
//! Pure data model + classifier for the swarm work-packet's "Beads
//! integrity" section. The packet collector layer is responsible for
//! invoking `br doctor --json` (or reading the JSONL/DB directly) and
//! feeding the results into [`compose_integrity_report`]; this module
//! never spawns subprocesses, opens databases, or mutates `.beads`.
//!
//! ## Why not parse `.beads` ourselves?
//!
//! The bd-2z5ly.9 spec is explicit: "do not implement a second Beads
//! parser if an existing collector can provide the data". The collector
//! invariant for this module is therefore:
//!
//! - Inputs are *already-collected*, redacted summaries (record counts,
//!   SQLite integrity status, merge-artifact path patterns, the first
//!   malformed JSONL line, etc.).
//! - Output is a deterministic [`BeadsIntegrityReport`] whose
//!   serialization is byte-stable for the same inputs.
//!
//! ## Health states
//!
//! - [`BeadsIntegrityHealth::Ok`] — JSONL parses, DB and JSONL agree,
//!   no merge artifacts, no pending import.
//! - [`BeadsIntegrityHealth::JsonlParseError`] — at least one line in
//!   `.beads/issues.jsonl` failed to parse; the packet must downgrade
//!   candidate safety because normal `br` reads are not authoritative.
//! - [`BeadsIntegrityHealth::DbJsonlCountMismatch`] — the SQLite store
//!   and JSONL export disagree on record count. Usually caused by a
//!   half-finished import or peer-staged JSONL the local DB has not
//!   yet absorbed.
//! - [`BeadsIntegrityHealth::ExternalChangesPendingImport`] — JSONL has
//!   more rows than the DB, or sync metadata still reports pending
//!   external changes. A specific case worth surfacing separately so the
//!   collector can suggest `br sync`; metadata-only drift with equal
//!   counts and zero dirty issues is warning evidence, not a hard claim
//!   blocker.
//! - [`BeadsIntegrityHealth::MergeArtifactsWarn`] — non-benign merge
//!   conflict artifacts (`.orig`, `.rej`, `.merge_artifact*`) are
//!   sitting next to `issues.jsonl`. JSONL may parse, but a recent
//!   merge may not have settled, so tracker reads are advisory only.
//!
//! When more than one condition is true at once the *most severe* one
//! is reported (parse error > count mismatch > merge warn). Pending
//! import is reported only when the DB has fewer rows than the JSONL
//! and the JSONL parses cleanly; otherwise it folds into the mismatch
//! state.
//!
//! ## Tracker authority states (bd-3w4pv.6)
//!
//! [`BeadsIntegrityHealth`] folds the doctor `sync.metadata` prose
//! message into `external_changes_pending_import`, which collapsed a
//! metadata-only "External changes pending import" message into a hard
//! `brReadsAuthoritative=false` even when every concrete dirty/import
//! signal was clean. [`BeadsTrackerAuthorityState`] keeps each concrete
//! signal distinct so the claim gate can report *why* tracker reads are
//! (not) authoritative:
//!
//! - Concrete fail-closed states (each keeps `brReadsAuthoritative`
//!   false): `parse_error`, `merge_artifacts`, `count_mismatch`,
//!   `dirty_issues`, `jsonl_newer`, `db_newer`.
//! - `doctor_metadata_message_only` — the doctor metadata message is
//!   present but every concrete signal is clean. Tracker reads stay
//!   authoritative; the contradiction is surfaced as a warning-severity
//!   degradation, never as `beads_tracker_not_authoritative`.
//! - `clean` — no signal at all.
//!
//! Precedence when several concrete signals hold at once (worst first):
//! `parse_error` > `merge_artifacts` > `count_mismatch` >
//! `dirty_issues` > `jsonl_newer` > `db_newer` >
//! `doctor_metadata_message_only` > `clean`.

use serde::Serialize;

/// Maximum length (bytes) of the redacted malformed-line excerpt
/// included in the report.
pub const MAX_EXCERPT_LEN: usize = 240;

/// Maximum number of merge-artifact path patterns to retain in the
/// report. Long lists indicate a deeper merge mess that should be
/// summarized by count, not enumerated in the packet.
pub const MAX_MERGE_ARTIFACTS: usize = 8;

/// Top-level health classification of the Beads tracker.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BeadsIntegrityHealth {
    /// JSONL parses, DB and JSONL agree, no merge artifacts, no
    /// pending import.
    Ok,
    /// Merge-conflict artifacts exist next to `issues.jsonl`. JSONL
    /// may still parse, but a recent merge may not have settled.
    MergeArtifactsWarn,
    /// JSONL contains more rows than the DB; the agent should
    /// `br sync` (or `br --no-auto-import --allow-stale` for
    /// read-only inspection) before claiming work.
    ExternalChangesPendingImport,
    /// SQLite store and JSONL disagree on record count for reasons
    /// other than pending import (e.g. DB has rows JSONL does not).
    DbJsonlCountMismatch,
    /// At least one line in `.beads/issues.jsonl` failed to parse.
    /// Normal `br` reads are not authoritative until the bad line is
    /// inspected.
    JsonlParseError,
}

/// Repair-safety classification for JSONL parse-error diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BeadsIntegrityRepairClassification {
    /// A malformed row appears immediately after all parseable rows, DB
    /// integrity is clean, and DB/JSONL valid counts agree.
    InvalidTrailingLineDbHealthy,
    /// SQLite integrity evidence failed, so DB export cannot be treated
    /// as a safe repair candidate.
    DbIntegrityFailed,
    /// Merge-conflict artifacts make the correct repair source ambiguous.
    MergeArtifactsPresent,
    /// DB row count and parseable JSONL row count diverge.
    DbJsonlCountMismatch,
    /// Dirty Beads issues or pending-import metadata means the DB may
    /// contain unexported or stale state.
    StaleDbGuardRisk,
    /// The parse error is not the narrow trailing-line shape, or the
    /// evidence is otherwise insufficient for bounded repair advice.
    UnknownDurableCorruption,
}

/// Concrete tracker-authority state behind `brReadsAuthoritative`
/// (bd-3w4pv.6).
///
/// Unlike [`BeadsIntegrityHealth`] — which is preserved unchanged for
/// serialized `trackerIntegrity.health` compatibility — this
/// classification never collapses the doctor `sync.metadata` prose
/// message into a hard authority failure. Each variant is derived only
/// from support-bundle-safe evidence: boolean sync fields, counts, and
/// bounded path patterns.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BeadsTrackerAuthorityState {
    /// No parse, merge, count, dirty, or metadata signal at all.
    Clean,
    /// The doctor `sync.metadata` message still says external changes
    /// are pending import while every concrete dirty/import signal is
    /// clean (zero dirty issues, equal DB/JSONL counts, no merge
    /// artifacts, no parse error). Tracker reads stay authoritative;
    /// the contradiction is warning evidence, not a claim blocker.
    DoctorMetadataMessageOnly,
    /// The SQLite store has rows the JSONL export lacks; local state
    /// may be unexported (`br sync --flush-only`).
    DbNewer,
    /// The JSONL export has rows the DB has not imported yet and
    /// auto-import can reconcile them (`br sync --import-only`).
    JsonlNewer,
    /// `br doctor` reports locally dirty Beads issues.
    DirtyIssues,
    /// DB/JSONL record counts differ in a shape auto-import cannot
    /// reconcile (JSONL ahead while auto-import is disabled).
    CountMismatch,
    /// Non-benign merge-conflict artifacts sit next to
    /// `issues.jsonl`; the benign `beads.base.jsonl` merge anchor does
    /// not count.
    MergeArtifacts,
    /// At least one JSONL line failed to parse.
    ParseError,
}

impl BeadsTrackerAuthorityState {
    /// Stable snake_case label used for `sourceAuthority.trackerHealth`
    /// and bounded reason suffixes.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::DoctorMetadataMessageOnly => "doctor_metadata_message_only",
            Self::DbNewer => "db_newer",
            Self::JsonlNewer => "jsonl_newer",
            Self::DirtyIssues => "dirty_issues",
            Self::CountMismatch => "count_mismatch",
            Self::MergeArtifacts => "merge_artifacts",
            Self::ParseError => "parse_error",
        }
    }

    /// Whether normal `br` reads remain authoritative in this state.
    ///
    /// Fail-closed contract: every concrete stale state keeps tracker
    /// authority false. Only the absence of concrete evidence —
    /// [`Self::Clean`] and the metadata-message-only contradiction —
    /// keeps `br` reads authoritative.
    #[must_use]
    pub const fn is_authoritative(self) -> bool {
        matches!(self, Self::Clean | Self::DoctorMetadataMessageOnly)
    }
}

/// Already-collected concrete signals feeding
/// [`classify_tracker_authority_state`].
///
/// Callers derive these from `br doctor --json` / `br sync --status`
/// evidence; the struct itself carries only booleans and a bounded
/// count so it stays support-bundle-safe.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BeadsTrackerAuthoritySignals {
    /// At least one JSONL line failed to parse.
    pub jsonl_parse_error: bool,
    /// Non-benign merge-conflict artifacts are present.
    pub non_benign_merge_artifacts: bool,
    /// DB/JSONL counts differ in a shape auto-import cannot reconcile.
    pub unreconcilable_count_mismatch: bool,
    /// Locally dirty Beads issues reported by `br doctor`.
    pub dirty_issue_count: u64,
    /// JSONL has rows the DB has not imported yet (importable drift).
    pub jsonl_newer: bool,
    /// DB has rows the JSONL export lacks (unexported local state).
    pub db_newer: bool,
    /// The doctor `sync.metadata` message claims external changes are
    /// pending import.
    pub doctor_metadata_message: bool,
}

/// Pick the single [`BeadsTrackerAuthorityState`] implied by the
/// concrete signals.
///
/// Documented precedence when multiple signals are present (worst
/// first): `parse_error` > `merge_artifacts` > `count_mismatch` >
/// `dirty_issues` > `jsonl_newer` > `db_newer` >
/// `doctor_metadata_message_only` > `clean`. A doctor metadata message
/// counts as non-authoritative only when paired with one of the
/// concrete signals above it; alone it classifies as
/// [`BeadsTrackerAuthorityState::DoctorMetadataMessageOnly`].
#[must_use]
pub const fn classify_tracker_authority_state(
    signals: BeadsTrackerAuthoritySignals,
) -> BeadsTrackerAuthorityState {
    if signals.jsonl_parse_error {
        return BeadsTrackerAuthorityState::ParseError;
    }
    if signals.non_benign_merge_artifacts {
        return BeadsTrackerAuthorityState::MergeArtifacts;
    }
    if signals.unreconcilable_count_mismatch {
        return BeadsTrackerAuthorityState::CountMismatch;
    }
    if signals.dirty_issue_count > 0 {
        return BeadsTrackerAuthorityState::DirtyIssues;
    }
    if signals.jsonl_newer {
        return BeadsTrackerAuthorityState::JsonlNewer;
    }
    if signals.db_newer {
        return BeadsTrackerAuthorityState::DbNewer;
    }
    if signals.doctor_metadata_message {
        return BeadsTrackerAuthorityState::DoctorMetadataMessageOnly;
    }
    BeadsTrackerAuthorityState::Clean
}

/// Derive the concrete authority signals from already-collected
/// integrity inputs. Exposed for the work-packet layer and tests.
#[must_use]
pub fn tracker_authority_signals(
    has_parse_error: bool,
    jsonl_record_count: u64,
    db_record_count: u64,
    auto_import_enabled: bool,
    external_changes_pending_import: bool,
    dirty_issue_count: u64,
    merge_artifact_paths: &[String],
) -> BeadsTrackerAuthoritySignals {
    let jsonl_newer = jsonl_record_count > db_record_count && auto_import_enabled;
    let db_newer = db_record_count > jsonl_record_count;
    BeadsTrackerAuthoritySignals {
        jsonl_parse_error: has_parse_error,
        non_benign_merge_artifacts: merge_artifact_paths
            .iter()
            .any(|path| !is_benign_beads_merge_base_artifact(path)),
        unreconcilable_count_mismatch: jsonl_record_count != db_record_count
            && !jsonl_newer
            && !db_newer,
        dirty_issue_count,
        jsonl_newer,
        db_newer,
        doctor_metadata_message: external_changes_pending_import,
    }
}

impl BeadsIntegrityHealth {
    /// Stable severity ordering used internally to pick the worst
    /// state when several conditions hold at once. Larger = worse.
    const fn severity_rank(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::ExternalChangesPendingImport => 1,
            Self::MergeArtifactsWarn => 2,
            Self::DbJsonlCountMismatch => 3,
            Self::JsonlParseError => 4,
        }
    }

    /// Whether this health state normally forces candidate downgrades
    /// when no report-level parity evidence overrides it.
    #[must_use]
    pub const fn requires_candidate_downgrade(self) -> bool {
        !matches!(self, Self::Ok)
    }

    /// Short, agent-facing recovery hint. Returns `None` for
    /// [`BeadsIntegrityHealth::Ok`] because no action is needed.
    #[must_use]
    pub const fn recovery_hint(self) -> Option<&'static str> {
        match self {
            Self::Ok => None,
            Self::MergeArtifactsWarn => {
                Some("Inspect or remove merge artifacts in .beads/ before relying on br reads.")
            }
            Self::ExternalChangesPendingImport => Some(
                "Run br sync (or br --no-auto-import --allow-stale for read-only inspection) \
                 before claiming work.",
            ),
            Self::DbJsonlCountMismatch => {
                Some("Run br doctor --json and reconcile DB and JSONL before claiming work.")
            }
            Self::JsonlParseError => Some(
                "Inspect the first invalid JSONL line and run br doctor --json before any \
                 br update / br claim.",
            ),
        }
    }
}

/// Location of the first malformed JSONL row.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonlParseError {
    /// 1-indexed line number of the first malformed row.
    pub line: u64,
    /// 1-indexed column of the JSON parse error, when the upstream
    /// parser reports it. `None` when the column is unknown (for
    /// example a truncated final line).
    pub column: Option<u64>,
    /// Redacted excerpt (UTF-8, truncated to [`MAX_EXCERPT_LEN`]
    /// bytes) of the offending line. The collector is responsible
    /// for redacting secrets; this struct only truncates length.
    pub excerpt: String,
}

/// Inputs for [`compose_integrity_report`].
///
/// Each field is the *already-collected* result of a single read-only
/// inspection. The collector layer is responsible for invoking
/// `br doctor --json`, reading file metadata, etc.; this struct is
/// the deterministic data interface between the collector and the
/// packet integrity section.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BeadsIntegrityInputs<'a> {
    /// Project-relative, already-redacted path to the JSONL file
    /// (e.g. `".beads/issues.jsonl"`).
    pub jsonl_path: &'a str,
    /// Project-relative, already-redacted path to the SQLite store
    /// (e.g. `".beads/beads.db"`).
    pub db_path: &'a str,
    /// Total parseable rows in the JSONL file.
    pub jsonl_record_count: u64,
    /// Total rows in the SQLite store.
    pub db_record_count: u64,
    /// Whether `br` is configured to auto-import JSONL on read.
    /// Affects pending-import semantics.
    pub auto_import_enabled: bool,
    /// Whether Beads sync metadata says external JSONL changes are
    /// pending import even when the exported row count currently
    /// matches the DB row count.
    pub external_changes_pending_import: bool,
    /// Count of locally dirty Beads issues reported by `br doctor`,
    /// when available. This is a bounded summary only; the report
    /// never includes issue bodies or raw tracker rows.
    pub dirty_issue_count: u64,
    /// Project-relative paths to merge-conflict artifacts found
    /// alongside `issues.jsonl`. Already-redacted by the collector.
    pub merge_artifact_paths: &'a [String],
    /// First malformed JSONL row, when the collector observed one.
    pub jsonl_parse_error: Option<JsonlParseError>,
}

/// Deterministic, redacted Beads tracker integrity report.
///
/// Field ordering is stable and reflects the serialized JSON: top-
/// level health first, then file identifiers, then counts, then any
/// parse-error / merge-artifact evidence, then the agent-facing
/// recovery hint.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BeadsIntegrityReport {
    pub health: BeadsIntegrityHealth,
    pub jsonl_path: String,
    pub db_path: String,
    pub jsonl_record_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jsonl_valid_record_count: Option<u64>,
    pub db_record_count: u64,
    pub pending_import_count: u64,
    pub external_changes_pending_import: bool,
    pub dirty_issue_count: u64,
    pub merge_artifact_paths: Vec<String>,
    pub merge_artifact_count: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub invalid_line_numbers: Vec<u64>,
    pub jsonl_parse_error: Option<JsonlParseError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db_integrity_ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_import_timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_export_timestamp: Option<String>,
    /// Concrete authority state behind `br_reads_authoritative`
    /// (bd-3w4pv.6). Not serialized: the `trackerIntegrity` payload
    /// keeps its existing field set; the claim gate surfaces this
    /// state as `sourceAuthority.trackerHealth`.
    #[serde(skip)]
    pub tracker_authority_state: BeadsTrackerAuthorityState,
    pub br_reads_authoritative: bool,
    pub requires_candidate_downgrade: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safe_repair_candidate: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair_classification: Option<BeadsIntegrityRepairClassification>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutation_must_stop: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair_command_candidate: Option<&'static str>,
    pub recovery_hint: Option<&'static str>,
}

impl BeadsIntegrityReport {
    /// True when the only stale signal is the doctor `sync.metadata`
    /// message: tracker reads stay authoritative, and the work-packet
    /// layer surfaces the contradiction as a warning-severity
    /// degradation instead of `beads_tracker_not_authoritative`.
    #[must_use]
    pub const fn doctor_metadata_message_only(&self) -> bool {
        matches!(
            self.tracker_authority_state,
            BeadsTrackerAuthorityState::DoctorMetadataMessageOnly
        )
    }
}

/// Owned input bundle returned by the `br doctor --json` adapter.
///
/// This lets the collector keep `String`/`Vec` ownership while still
/// feeding the existing borrowed [`BeadsIntegrityInputs`] contract into
/// [`compose_integrity_report`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedBeadsIntegrityInputs {
    pub jsonl_path: String,
    pub db_path: String,
    pub jsonl_record_count: u64,
    pub db_record_count: u64,
    pub db_integrity_ok: bool,
    pub last_import_timestamp: Option<String>,
    pub last_export_timestamp: Option<String>,
    pub auto_import_enabled: bool,
    pub external_changes_pending_import: bool,
    pub dirty_issue_count: u64,
    pub merge_artifact_paths: Vec<String>,
    pub jsonl_parse_error: Option<JsonlParseError>,
}

impl OwnedBeadsIntegrityInputs {
    #[must_use]
    pub fn as_inputs(&self) -> BeadsIntegrityInputs<'_> {
        BeadsIntegrityInputs {
            jsonl_path: &self.jsonl_path,
            db_path: &self.db_path,
            jsonl_record_count: self.jsonl_record_count,
            db_record_count: self.db_record_count,
            auto_import_enabled: self.auto_import_enabled,
            external_changes_pending_import: self.external_changes_pending_import,
            dirty_issue_count: self.dirty_issue_count,
            merge_artifact_paths: &self.merge_artifact_paths,
            jsonl_parse_error: self.jsonl_parse_error.clone(),
        }
    }

    #[must_use]
    pub fn compose_report(&self) -> BeadsIntegrityReport {
        compose_integrity_report_with_metadata(
            self.as_inputs(),
            self.db_integrity_ok,
            self.last_import_timestamp.as_deref(),
            self.last_export_timestamp.as_deref(),
        )
    }
}

/// Parse failure while translating `br doctor --json` into the
/// deterministic work-packet integrity input model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BeadsDoctorJsonError {
    message: String,
}

impl BeadsDoctorJsonError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for BeadsDoctorJsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for BeadsDoctorJsonError {}

/// Compose a deterministic [`BeadsIntegrityReport`] from the
/// already-collected inputs.
///
/// Pure function: no IO, no clock reads, no allocations beyond what's
/// in the inputs. Calling this twice with equal inputs produces equal
/// reports and byte-identical JSON.
///
/// Severity selection rules:
/// 1. `jsonl_parse_error.is_some()` → [`BeadsIntegrityHealth::JsonlParseError`].
/// 2. Otherwise, if DB has rows JSONL does not (db > jsonl) or the
///    record counts differ for any other reason while auto-import is
///    disabled → [`BeadsIntegrityHealth::DbJsonlCountMismatch`].
/// 3. Otherwise, if JSONL has more rows than the DB →
///    [`BeadsIntegrityHealth::ExternalChangesPendingImport`].
/// 4. Otherwise, if any non-benign merge artifacts are present →
///    [`BeadsIntegrityHealth::MergeArtifactsWarn`].
/// 5. Otherwise → [`BeadsIntegrityHealth::Ok`].
#[must_use]
pub fn compose_integrity_report(inputs: BeadsIntegrityInputs<'_>) -> BeadsIntegrityReport {
    compose_integrity_report_with_metadata(inputs, true, None, None)
}

fn compose_integrity_report_with_metadata(
    inputs: BeadsIntegrityInputs<'_>,
    db_integrity_ok: bool,
    last_import_timestamp: Option<&str>,
    last_export_timestamp: Option<&str>,
) -> BeadsIntegrityReport {
    let parse_error = inputs.jsonl_parse_error.as_ref().map(truncate_parse_error);
    let invalid_line_numbers = parse_error
        .as_ref()
        .map(|error| vec![error.line])
        .unwrap_or_default();
    let show_repair_context = parse_error.is_some();
    let pending_import_count = inputs
        .jsonl_record_count
        .saturating_sub(inputs.db_record_count);
    let merge_artifact_count = u64::try_from(inputs.merge_artifact_paths.len()).unwrap_or(u64::MAX);
    let has_non_benign_merge_artifacts = inputs
        .merge_artifact_paths
        .iter()
        .any(|path| !is_benign_beads_merge_base_artifact(path));

    let health = classify_health(
        parse_error.is_some(),
        inputs.jsonl_record_count,
        inputs.db_record_count,
        inputs.auto_import_enabled,
        inputs.external_changes_pending_import,
        has_non_benign_merge_artifacts,
    );

    let mut merge_artifact_paths: Vec<String> = inputs
        .merge_artifact_paths
        .iter()
        .take(MAX_MERGE_ARTIFACTS)
        .cloned()
        .collect();
    merge_artifact_paths.sort();

    let tracker_authority_state = classify_tracker_authority_state(tracker_authority_signals(
        parse_error.is_some(),
        inputs.jsonl_record_count,
        inputs.db_record_count,
        inputs.auto_import_enabled,
        inputs.external_changes_pending_import,
        inputs.dirty_issue_count,
        inputs.merge_artifact_paths,
    ));
    let br_reads_authoritative = tracker_authority_state.is_authoritative();
    let safe_repair_candidate = safe_repair_candidate_for_report(
        &parse_error,
        inputs.jsonl_record_count,
        inputs.db_record_count,
        db_integrity_ok,
        inputs.dirty_issue_count,
        inputs.merge_artifact_paths,
    );
    let repair_classification = repair_classification_for_report(
        &parse_error,
        inputs.jsonl_record_count,
        inputs.db_record_count,
        db_integrity_ok,
        inputs.external_changes_pending_import,
        inputs.dirty_issue_count,
        inputs.merge_artifact_paths,
    );

    BeadsIntegrityReport {
        health,
        jsonl_path: inputs.jsonl_path.to_owned(),
        db_path: inputs.db_path.to_owned(),
        jsonl_record_count: inputs.jsonl_record_count,
        jsonl_valid_record_count: show_repair_context.then_some(inputs.jsonl_record_count),
        db_record_count: inputs.db_record_count,
        pending_import_count,
        external_changes_pending_import: inputs.external_changes_pending_import,
        dirty_issue_count: inputs.dirty_issue_count,
        merge_artifact_paths,
        merge_artifact_count,
        invalid_line_numbers,
        jsonl_parse_error: parse_error,
        db_integrity_ok: show_repair_context.then_some(db_integrity_ok),
        last_import_timestamp: if show_repair_context {
            last_import_timestamp.map(str::to_owned)
        } else {
            None
        },
        last_export_timestamp: if show_repair_context {
            last_export_timestamp.map(str::to_owned)
        } else {
            None
        },
        tracker_authority_state,
        br_reads_authoritative,
        requires_candidate_downgrade: !br_reads_authoritative,
        safe_repair_candidate: show_repair_context.then_some(safe_repair_candidate),
        repair_classification,
        mutation_must_stop: show_repair_context.then_some(!br_reads_authoritative),
        repair_command_candidate: safe_repair_candidate
            .then_some("br sync --flush-only --force --json"),
        recovery_hint: health.recovery_hint(),
    }
}

/// Translate a `br doctor --json` payload into a deterministic
/// [`BeadsIntegrityReport`].
///
/// This adapter intentionally reads only bounded metadata from the
/// doctor payload: check names/statuses, row counts, merge-artifact
/// filenames, dirty issue counts, and the first parse-error location.
/// It does not parse `.beads/issues.jsonl`, open `.beads/beads.db`, run
/// `br`, or mutate tracker state.
pub fn compose_integrity_report_from_br_doctor_json(
    raw_json: &str,
    jsonl_path: &str,
    db_path: &str,
    auto_import_enabled: bool,
) -> Result<BeadsIntegrityReport, BeadsDoctorJsonError> {
    Ok(beads_integrity_inputs_from_br_doctor_json(
        raw_json,
        jsonl_path,
        db_path,
        auto_import_enabled,
    )?
    .compose_report())
}

/// Translate `br doctor --json` into owned integrity inputs for later
/// packet composition.
pub fn beads_integrity_inputs_from_br_doctor_json(
    raw_json: &str,
    jsonl_path: &str,
    db_path: &str,
    auto_import_enabled: bool,
) -> Result<OwnedBeadsIntegrityInputs, BeadsDoctorJsonError> {
    let value = serde_json::from_str::<serde_json::Value>(raw_json)
        .map_err(|error| BeadsDoctorJsonError::new(format!("parse br doctor JSON: {error}")))?;
    let checks = value
        .get("checks")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| BeadsDoctorJsonError::new("br doctor JSON missing checks[]"))?;

    let jsonl_parse = find_doctor_check(checks, "jsonl.parse")
        .ok_or_else(|| BeadsDoctorJsonError::new("br doctor JSON missing jsonl.parse check"))?;
    let counts_check = find_doctor_check(checks, "counts.db_vs_jsonl");
    let merge_check = find_doctor_check(checks, "jsonl.merge_artifacts");
    let sync_check = find_doctor_check(checks, "sync.metadata");
    let db_integrity_check = find_first_doctor_check(
        checks,
        &[
            "sqlite.integrity_check",
            "sqlite.integrity",
            "db.integrity_check",
            "db.integrity",
        ],
    );

    let jsonl_record_count = doctor_u64_detail(jsonl_parse, &["records"])
        .or_else(|| first_u64_from_text(doctor_message(jsonl_parse)))
        .ok_or_else(|| {
            BeadsDoctorJsonError::new("br doctor jsonl.parse check missing record count")
        })?;
    let db_record_count = counts_check
        .and_then(|check| {
            doctor_u64_detail(
                check,
                &[
                    "db_records",
                    "dbRecords",
                    "database_records",
                    "databaseRecords",
                    "db_count",
                    "dbCount",
                    "db",
                    "records",
                ],
            )
            .or_else(|| {
                if doctor_status(check) == Some("ok") {
                    first_u64_from_text(doctor_message(check))
                } else {
                    None
                }
            })
        })
        .unwrap_or(jsonl_record_count);
    let jsonl_record_count = counts_check
        .and_then(|check| {
            doctor_u64_detail(
                check,
                &[
                    "jsonl_records",
                    "jsonlRecords",
                    "jsonl_count",
                    "jsonlCount",
                    "export_records",
                    "exportRecords",
                    "jsonl",
                ],
            )
        })
        .unwrap_or(jsonl_record_count);

    let merge_artifact_paths = merge_check
        .and_then(|check| doctor_string_array_detail(check, &["files", "paths", "artifacts"]))
        .unwrap_or_default();
    let dirty_issue_count = sync_check
        .and_then(|check| doctor_u64_detail(check, &["dirty_issues", "dirtyIssues"]))
        .unwrap_or(0);
    let external_changes_pending_import =
        sync_check.is_some_and(doctor_message_indicates_external_pending_import);
    let jsonl_parse_error = doctor_parse_error(jsonl_parse);
    let db_integrity_ok =
        db_integrity_check.map_or(true, |check| doctor_status(check) == Some("ok"));
    let last_import_timestamp =
        sync_check.and_then(|check| doctor_string_detail(check, &["last_import", "lastImport"]));
    let last_export_timestamp =
        sync_check.and_then(|check| doctor_string_detail(check, &["last_export", "lastExport"]));

    Ok(OwnedBeadsIntegrityInputs {
        jsonl_path: jsonl_path.to_owned(),
        db_path: db_path.to_owned(),
        jsonl_record_count,
        db_record_count,
        db_integrity_ok,
        last_import_timestamp,
        last_export_timestamp,
        auto_import_enabled,
        external_changes_pending_import,
        dirty_issue_count,
        merge_artifact_paths,
        jsonl_parse_error,
    })
}

/// Pick the most severe [`BeadsIntegrityHealth`] state implied by
/// the input signals. Exposed for tests and the packet collector;
/// `compose_integrity_report` uses this internally.
#[must_use]
pub fn classify_health(
    has_parse_error: bool,
    jsonl_count: u64,
    db_count: u64,
    auto_import_enabled: bool,
    external_changes_pending_import: bool,
    has_non_benign_merge_artifacts: bool,
) -> BeadsIntegrityHealth {
    let mut candidate = BeadsIntegrityHealth::Ok;
    let mut promote = |state: BeadsIntegrityHealth| {
        if state.severity_rank() > candidate.severity_rank() {
            candidate = state;
        }
    };

    if has_non_benign_merge_artifacts {
        promote(BeadsIntegrityHealth::MergeArtifactsWarn);
    }
    if jsonl_count != db_count {
        if jsonl_count > db_count && auto_import_enabled && !has_parse_error {
            promote(BeadsIntegrityHealth::ExternalChangesPendingImport);
        } else {
            promote(BeadsIntegrityHealth::DbJsonlCountMismatch);
        }
    }
    if external_changes_pending_import && !has_parse_error {
        promote(BeadsIntegrityHealth::ExternalChangesPendingImport);
    }
    if has_parse_error {
        promote(BeadsIntegrityHealth::JsonlParseError);
    }

    candidate
}

fn repair_classification_for_report(
    parse_error: &Option<JsonlParseError>,
    jsonl_record_count: u64,
    db_record_count: u64,
    db_integrity_ok: bool,
    external_changes_pending_import: bool,
    dirty_issue_count: u64,
    merge_artifact_paths: &[String],
) -> Option<BeadsIntegrityRepairClassification> {
    use BeadsIntegrityRepairClassification::{
        DbIntegrityFailed, DbJsonlCountMismatch, InvalidTrailingLineDbHealthy,
        MergeArtifactsPresent, StaleDbGuardRisk, UnknownDurableCorruption,
    };

    let parse_error = parse_error.as_ref()?;
    if !db_integrity_ok {
        return Some(DbIntegrityFailed);
    }
    if !merge_artifact_paths.is_empty() {
        return Some(MergeArtifactsPresent);
    }
    if jsonl_record_count != db_record_count {
        return Some(DbJsonlCountMismatch);
    }
    if dirty_issue_count > 0 || external_changes_pending_import {
        return Some(StaleDbGuardRisk);
    }
    if parse_error.line == jsonl_record_count.saturating_add(1) {
        Some(InvalidTrailingLineDbHealthy)
    } else {
        Some(UnknownDurableCorruption)
    }
}

fn safe_repair_candidate_for_report(
    parse_error: &Option<JsonlParseError>,
    jsonl_record_count: u64,
    db_record_count: u64,
    db_integrity_ok: bool,
    dirty_issue_count: u64,
    merge_artifact_paths: &[String],
) -> bool {
    let Some(parse_error) = parse_error else {
        return false;
    };

    db_integrity_ok
        && jsonl_record_count == db_record_count
        && parse_error.line == jsonl_record_count.saturating_add(1)
        && dirty_issue_count == 0
        && merge_artifact_paths.is_empty()
}

fn is_benign_beads_merge_base_artifact(path: &str) -> bool {
    matches!(path, "beads.base.jsonl" | ".beads/beads.base.jsonl")
}

fn find_doctor_check<'a>(
    checks: &'a [serde_json::Value],
    name: &str,
) -> Option<&'a serde_json::Value> {
    checks
        .iter()
        .find(|check| check.get("name").and_then(serde_json::Value::as_str) == Some(name))
}

fn find_first_doctor_check<'a>(
    checks: &'a [serde_json::Value],
    names: &[&str],
) -> Option<&'a serde_json::Value> {
    names
        .iter()
        .find_map(|name| find_doctor_check(checks, name))
}

fn doctor_status(check: &serde_json::Value) -> Option<&str> {
    check.get("status").and_then(serde_json::Value::as_str)
}

fn doctor_message(check: &serde_json::Value) -> &str {
    check
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}

fn doctor_message_indicates_external_pending_import(check: &serde_json::Value) -> bool {
    let message = doctor_message(check).to_ascii_lowercase();
    message.contains("external changes pending import")
        && !message.contains("no external changes pending import")
}

fn doctor_details(
    check: &serde_json::Value,
) -> Option<&serde_json::Map<String, serde_json::Value>> {
    check.get("details").and_then(serde_json::Value::as_object)
}

fn doctor_u64_detail(check: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    let details = doctor_details(check)?;
    keys.iter().find_map(|key| {
        let value = details.get(*key)?;
        value
            .as_u64()
            .or_else(|| value.as_i64().and_then(|n| u64::try_from(n).ok()))
            .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))
    })
}

fn doctor_string_detail(check: &serde_json::Value, keys: &[&str]) -> Option<String> {
    let details = doctor_details(check)?;
    keys.iter().find_map(|key| {
        details
            .get(*key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    })
}

fn doctor_string_array_detail(check: &serde_json::Value, keys: &[&str]) -> Option<Vec<String>> {
    let details = doctor_details(check)?;
    keys.iter().find_map(|key| {
        details.get(*key)?.as_array().map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
    })
}

fn doctor_parse_error(check: &serde_json::Value) -> Option<JsonlParseError> {
    let status = doctor_status(check)?;
    if matches!(status, "ok" | "warn") {
        return None;
    }

    let message = doctor_message(check);
    let line = doctor_u64_detail(check, &["line", "line_number", "lineNumber"])
        .or_else(|| u64_after_token(message, "line"))
        .unwrap_or(1);
    let column =
        doctor_u64_detail(check, &["column", "col"]).or_else(|| u64_after_token(message, "column"));
    let excerpt = doctor_string_detail(
        check,
        &[
            "excerpt",
            "line_excerpt",
            "lineExcerpt",
            "offending_line",
            "offendingLine",
        ],
    )
    .unwrap_or_else(|| message.to_owned());

    Some(JsonlParseError {
        line,
        column,
        excerpt,
    })
}

fn first_u64_from_text(text: &str) -> Option<u64> {
    unsigned_numbers(text).into_iter().next()
}

fn u64_after_token(text: &str, token: &str) -> Option<u64> {
    let lower = text.to_ascii_lowercase();
    let index = lower.find(token)?;
    first_u64_from_text(&text[index + token.len()..])
}

fn unsigned_numbers(text: &str) -> Vec<u64> {
    text.split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<u64>().ok())
        .collect()
}

fn truncate_parse_error(err: &JsonlParseError) -> JsonlParseError {
    JsonlParseError {
        line: err.line,
        column: err.column,
        excerpt: truncate_utf8(&err.excerpt, MAX_EXCERPT_LEN),
    }
}

/// Truncate `s` to at most `max_bytes` bytes, preserving UTF-8
/// boundaries. Returns the input unchanged when it is already short
/// enough.
fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn ensure_equal<T: std::fmt::Debug + PartialEq>(
        actual: &T,
        expected: &T,
        ctx: &str,
    ) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn base_inputs<'a>(
        merge_artifacts: &'a [String],
        parse_error: Option<JsonlParseError>,
    ) -> BeadsIntegrityInputs<'a> {
        BeadsIntegrityInputs {
            jsonl_path: ".beads/issues.jsonl",
            db_path: ".beads/beads.db",
            jsonl_record_count: 100,
            db_record_count: 100,
            auto_import_enabled: true,
            external_changes_pending_import: false,
            dirty_issue_count: 0,
            merge_artifact_paths: merge_artifacts,
            jsonl_parse_error: parse_error,
        }
    }

    fn doctor_payload(checks: serde_json::Value) -> String {
        serde_json::json!({
            "ok": true,
            "checks": checks,
        })
        .to_string()
    }

    #[test]
    fn ok_state_has_no_recovery_hint_and_no_downgrade() -> TestResult {
        let report = compose_integrity_report(base_inputs(&[], None));
        ensure_equal(&report.health, &BeadsIntegrityHealth::Ok, "ok health")?;
        ensure_equal(
            &report.requires_candidate_downgrade,
            &false,
            "ok must not downgrade",
        )?;
        ensure_equal(&report.recovery_hint, &None, "ok has no hint")?;
        ensure_equal(&report.pending_import_count, &0, "ok pending=0")
    }

    #[test]
    fn merge_artifacts_promote_to_merge_artifacts_warn() -> TestResult {
        let artifacts = vec![
            ".beads/issues.jsonl.orig".to_owned(),
            ".beads/issues.jsonl.rej".to_owned(),
        ];
        let report = compose_integrity_report(base_inputs(&artifacts, None));
        ensure_equal(
            &report.health,
            &BeadsIntegrityHealth::MergeArtifactsWarn,
            "merge warn",
        )?;
        ensure_equal(
            &report.requires_candidate_downgrade,
            &true,
            "merge warn must downgrade candidate safety",
        )?;
        ensure_equal(
            &report.br_reads_authoritative,
            &false,
            "merge artifacts make br read authority advisory only",
        )?;
        ensure(
            report.recovery_hint.is_some(),
            "merge warn must offer a recovery hint",
        )?;
        ensure_equal(&report.merge_artifact_count, &2, "artifact count")
    }

    #[test]
    fn jsonl_more_than_db_with_auto_import_is_pending_import() -> TestResult {
        let inputs = BeadsIntegrityInputs {
            jsonl_record_count: 105,
            db_record_count: 100,
            auto_import_enabled: true,
            ..base_inputs(&[], None)
        };
        let report = compose_integrity_report(inputs);
        ensure_equal(
            &report.health,
            &BeadsIntegrityHealth::ExternalChangesPendingImport,
            "pending import",
        )?;
        ensure_equal(
            &report.requires_candidate_downgrade,
            &true,
            "pending import must downgrade candidate safety",
        )?;
        ensure_equal(
            &report.br_reads_authoritative,
            &false,
            "pending import makes br reads advisory",
        )?;
        ensure_equal(&report.pending_import_count, &5, "pending=5")
    }

    #[test]
    fn br_doctor_json_ok_maps_to_authoritative_report() -> TestResult {
        let raw = doctor_payload(serde_json::json!([
            {
                "name": "jsonl.merge_artifacts",
                "status": "ok",
                "message": "No merge artifacts",
                "details": { "files": [] }
            },
            {
                "name": "jsonl.parse",
                "status": "ok",
                "message": "Parsed 2708 records",
                "details": { "records": 2708 }
            },
            {
                "name": "sqlite.integrity_check",
                "status": "ok"
            },
            {
                "name": "counts.db_vs_jsonl",
                "status": "ok",
                "message": "Both have 2708 records",
                "details": { "db": 2708, "jsonl": 2708 }
            },
            {
                "name": "sync.metadata",
                "status": "ok",
                "message": "No external changes pending import",
                "details": { "dirty_issues": 0 }
            }
        ]));
        let report = compose_integrity_report_from_br_doctor_json(
            &raw,
            ".beads/issues.jsonl",
            ".beads/beads.db",
            true,
        )
        .map_err(|error| error.to_string())?;

        ensure_equal(&report.health, &BeadsIntegrityHealth::Ok, "doctor ok")?;
        ensure_equal(&report.jsonl_record_count, &2708, "jsonl count")?;
        ensure_equal(&report.db_record_count, &2708, "db count")?;
        ensure_equal(&report.dirty_issue_count, &0, "dirty issues")?;
        ensure_equal(
            &report.br_reads_authoritative,
            &true,
            "ok br reads authoritative",
        )
    }

    #[test]
    fn br_doctor_json_external_pending_import_uses_sync_metadata() -> TestResult {
        let raw = doctor_payload(serde_json::json!([
            {
                "name": "jsonl.merge_artifacts",
                "status": "ok",
                "message": "No merge artifacts",
                "details": { "files": [] }
            },
            {
                "name": "jsonl.parse",
                "status": "ok",
                "message": "Parsed 2708 records",
                "details": { "records": 2708 }
            },
            {
                "name": "sqlite.integrity_check",
                "status": "ok"
            },
            {
                "name": "counts.db_vs_jsonl",
                "status": "ok",
                "message": "Both have 2708 records",
                "details": { "db": 2708, "jsonl": 2708 }
            },
            {
                "name": "sync.metadata",
                "status": "ok",
                "message": "External changes pending import",
                "details": { "dirty_issues": 2 }
            }
        ]));
        let report = compose_integrity_report_from_br_doctor_json(
            &raw,
            ".beads/issues.jsonl",
            ".beads/beads.db",
            true,
        )
        .map_err(|error| error.to_string())?;

        ensure_equal(
            &report.health,
            &BeadsIntegrityHealth::ExternalChangesPendingImport,
            "sync metadata pending import",
        )?;
        ensure_equal(
            &report.external_changes_pending_import,
            &true,
            "external pending flag",
        )?;
        ensure_equal(&report.dirty_issue_count, &2, "dirty issues")?;
        ensure_equal(
            &report.requires_candidate_downgrade,
            &true,
            "dirty pending import downgrades candidates",
        )?;
        ensure_equal(
            &report.br_reads_authoritative,
            &false,
            "pending import means current br reads need caution",
        )
    }

    #[test]
    fn metadata_only_external_pending_import_keeps_br_reads_authoritative() -> TestResult {
        let inputs = BeadsIntegrityInputs {
            external_changes_pending_import: true,
            dirty_issue_count: 0,
            ..base_inputs(&[], None)
        };
        let report = compose_integrity_report(inputs);

        ensure_equal(
            &report.health,
            &BeadsIntegrityHealth::ExternalChangesPendingImport,
            "metadata-only pending import is still surfaced",
        )?;
        ensure_equal(
            &report.pending_import_count,
            &0,
            "equal DB/JSONL counts have no pending rows",
        )?;
        ensure_equal(
            &report.requires_candidate_downgrade,
            &false,
            "metadata-only pending import must not downgrade candidates",
        )?;
        ensure_equal(
            &report.br_reads_authoritative,
            &true,
            "metadata-only pending import keeps br reads authoritative",
        )
    }

    #[test]
    fn br_doctor_json_metadata_only_pending_import_keeps_br_reads_authoritative() -> TestResult {
        let raw = doctor_payload(serde_json::json!([
            {
                "name": "jsonl.merge_artifacts",
                "status": "ok",
                "message": "No merge artifacts",
                "details": { "files": [] }
            },
            {
                "name": "jsonl.parse",
                "status": "ok",
                "message": "Parsed 3347 records",
                "details": { "records": 3347 }
            },
            {
                "name": "sqlite.integrity_check",
                "status": "ok"
            },
            {
                "name": "counts.db_vs_jsonl",
                "status": "ok",
                "message": "Both have 3347 records",
                "details": { "db": 3347, "jsonl": 3347 }
            },
            {
                "name": "sync.metadata",
                "status": "ok",
                "message": "External changes pending import",
                "details": {
                    "dirty_issues": 0,
                    "last_import": "2026-06-04T19:42:30+00:00",
                    "last_export": "2026-06-04T19:42:30+00:00",
                    "jsonl_hash": "e49435f610df6319"
                }
            }
        ]));
        let report = compose_integrity_report_from_br_doctor_json(
            &raw,
            ".beads/issues.jsonl",
            ".beads/beads.db",
            true,
        )
        .map_err(|error| error.to_string())?;

        ensure_equal(
            &report.health,
            &BeadsIntegrityHealth::ExternalChangesPendingImport,
            "sync metadata pending import",
        )?;
        ensure_equal(&report.dirty_issue_count, &0, "metadata-only dirty issues")?;
        ensure_equal(
            &report.pending_import_count,
            &0,
            "metadata-only pending count",
        )?;
        ensure_equal(
            &report.br_reads_authoritative,
            &true,
            "metadata-only pending import keeps br reads authoritative",
        )?;
        ensure_equal(
            &report.requires_candidate_downgrade,
            &false,
            "metadata-only pending import does not downgrade candidates",
        )
    }

    #[test]
    fn br_doctor_json_benign_merge_base_artifact_keeps_br_reads_authoritative() -> TestResult {
        let raw = doctor_payload(serde_json::json!([
            {
                "name": "jsonl.merge_artifacts",
                "status": "warn",
                "message": "Merge artifacts detected in .beads/",
                "details": { "files": ["beads.base.jsonl"] }
            },
            {
                "name": "jsonl.parse",
                "status": "ok",
                "message": "Parsed 3590 records",
                "details": { "records": 3590 }
            },
            {
                "name": "sqlite.integrity_check",
                "status": "ok"
            },
            {
                "name": "counts.db_vs_jsonl",
                "status": "ok",
                "message": "Both have 3590 records",
                "details": { "db": 3590, "jsonl": 3590 }
            },
            {
                "name": "sync.metadata",
                "status": "ok",
                "message": "External changes pending import",
                "details": { "dirty_issues": 0 }
            }
        ]));
        let report = compose_integrity_report_from_br_doctor_json(
            &raw,
            ".beads/issues.jsonl",
            ".beads/beads.db",
            true,
        )
        .map_err(|error| error.to_string())?;

        ensure_equal(
            &report.health,
            &BeadsIntegrityHealth::ExternalChangesPendingImport,
            "metadata warning remains visible",
        )?;
        ensure_equal(
            &report.merge_artifact_paths,
            &vec!["beads.base.jsonl".to_owned()],
            "benign base artifact is retained as evidence",
        )?;
        ensure_equal(
            &report.br_reads_authoritative,
            &true,
            "benign base artifact does not block br read authority",
        )?;
        ensure_equal(
            &report.requires_candidate_downgrade,
            &false,
            "benign base artifact does not downgrade candidates",
        )
    }

    #[test]
    fn br_doctor_json_count_mismatch_reads_db_and_jsonl_detail_keys() -> TestResult {
        let raw = doctor_payload(serde_json::json!([
            {
                "name": "jsonl.merge_artifacts",
                "status": "warn",
                "message": "Merge artifacts detected in .beads/",
                "details": { "files": ["beads.base.jsonl"] }
            },
            {
                "name": "jsonl.parse",
                "status": "ok",
                "message": "Parsed 2708 records",
                "details": { "records": 2708 }
            },
            {
                "name": "sqlite.integrity_check",
                "status": "ok"
            },
            {
                "name": "counts.db_vs_jsonl",
                "status": "warn",
                "message": "DB and JSONL counts differ",
                "details": { "db": 2709, "jsonl": 2708 }
            },
            {
                "name": "sync.metadata",
                "status": "ok",
                "message": "External changes pending import",
                "details": { "dirty_issues": 0 }
            }
        ]));
        let report = compose_integrity_report_from_br_doctor_json(
            &raw,
            ".beads/issues.jsonl",
            ".beads/beads.db",
            true,
        )
        .map_err(|error| error.to_string())?;

        ensure_equal(
            &report.health,
            &BeadsIntegrityHealth::DbJsonlCountMismatch,
            "db/jsonl detail keys must preserve mismatch",
        )?;
        ensure_equal(&report.db_record_count, &2709, "db count")?;
        ensure_equal(&report.jsonl_record_count, &2708, "jsonl count")?;
        ensure_equal(
            &report.pending_import_count,
            &0,
            "db > jsonl saturates pending count at zero",
        )?;
        ensure_equal(
            &report.merge_artifact_paths,
            &vec!["beads.base.jsonl".to_owned()],
            "merge artifacts",
        )
    }

    #[test]
    fn br_doctor_json_parse_error_captures_location_and_excerpt() -> TestResult {
        let raw = doctor_payload(serde_json::json!([
            {
                "name": "jsonl.merge_artifacts",
                "status": "ok",
                "message": "No merge artifacts",
                "details": { "files": [] }
            },
            {
                "name": "jsonl.parse",
                "status": "error",
                "message": "Invalid JSON at line 2703 column 12",
                "details": {
                    "records": 2702,
                    "line": 2703,
                    "column": 12,
                    "excerpt": "{\"id\":\"bd-malformed-tail\""
                }
            },
            {
                "name": "sqlite.integrity_check",
                "status": "ok"
            },
            {
                "name": "counts.db_vs_jsonl",
                "status": "ok",
                "message": "Both have 2702 records",
                "details": { "db": 2702, "jsonl": 2702 }
            },
            {
                "name": "sync.metadata",
                "status": "ok",
                "message": "No external changes pending import",
                "details": {
                    "dirty_issues": 0,
                    "last_import": "2026-06-09T14:12:50.120121+00:00",
                    "last_export": "2026-06-09T15:40:14.092658+00:00"
                }
            }
        ]));
        let report = compose_integrity_report_from_br_doctor_json(
            &raw,
            ".beads/issues.jsonl",
            ".beads/beads.db",
            true,
        )
        .map_err(|error| error.to_string())?;

        ensure_equal(
            &report.health,
            &BeadsIntegrityHealth::JsonlParseError,
            "doctor parse error",
        )?;
        let parse_error = report
            .jsonl_parse_error
            .ok_or_else(|| "expected parse error details".to_owned())?;
        ensure_equal(&parse_error.line, &2703, "parse line")?;
        ensure_equal(&parse_error.column, &Some(12), "parse column")?;
        ensure_equal(
            &parse_error.excerpt,
            &"{\"id\":\"bd-malformed-tail\"".to_owned(),
            "parse excerpt",
        )?;
        ensure_equal(
            &report.requires_candidate_downgrade,
            &true,
            "parse errors must downgrade candidate safety",
        )?;
        ensure_equal(
            &report.invalid_line_numbers,
            &vec![2703],
            "invalid line numbers",
        )?;
        ensure_equal(
            &report.jsonl_valid_record_count,
            &Some(2702),
            "valid JSONL record count",
        )?;
        ensure_equal(&report.db_integrity_ok, &Some(true), "db integrity")?;
        ensure_equal(
            &report.last_import_timestamp,
            &Some("2026-06-09T14:12:50.120121+00:00".to_owned()),
            "last import timestamp",
        )?;
        ensure_equal(
            &report.last_export_timestamp,
            &Some("2026-06-09T15:40:14.092658+00:00".to_owned()),
            "last export timestamp",
        )?;
        ensure_equal(
            &report.safe_repair_candidate,
            &Some(true),
            "db-healthy invalid tail is a bounded repair candidate",
        )?;
        ensure_equal(
            &report.repair_classification,
            &Some(BeadsIntegrityRepairClassification::InvalidTrailingLineDbHealthy),
            "repair classification",
        )?;
        ensure_equal(
            &report.mutation_must_stop,
            &Some(true),
            "parse error stops tracker mutation",
        )?;
        ensure_equal(
            &report.repair_command_candidate,
            &Some("br sync --flush-only --force --json"),
            "repair command candidate",
        )
    }

    #[test]
    fn parse_error_refuses_repair_when_db_integrity_fails() -> TestResult {
        let raw = doctor_payload(serde_json::json!([
            {
                "name": "jsonl.parse",
                "status": "error",
                "message": "Invalid JSON at line 101",
                "details": {
                    "records": 100,
                    "line": 101,
                    "excerpt": "}]}"
                }
            },
            {
                "name": "sqlite.integrity_check",
                "status": "error",
                "message": "database disk image is malformed"
            },
            {
                "name": "counts.db_vs_jsonl",
                "status": "ok",
                "message": "Both have 100 records",
                "details": { "db": 100, "jsonl": 100 }
            },
            {
                "name": "sync.metadata",
                "status": "ok",
                "message": "No external changes pending import",
                "details": { "dirty_issues": 0 }
            }
        ]));
        let report = compose_integrity_report_from_br_doctor_json(
            &raw,
            ".beads/issues.jsonl",
            ".beads/beads.db",
            true,
        )
        .map_err(|error| error.to_string())?;

        ensure_equal(&report.db_integrity_ok, &Some(false), "db integrity")?;
        ensure_equal(
            &report.safe_repair_candidate,
            &Some(false),
            "failed DB integrity refuses repair recommendation",
        )?;
        ensure_equal(
            &report.repair_classification,
            &Some(BeadsIntegrityRepairClassification::DbIntegrityFailed),
            "failed DB integrity repair classification",
        )?;
        ensure_equal(
            &report.repair_command_candidate,
            &None,
            "failed DB integrity has no repair command",
        )
    }

    #[test]
    fn parse_error_refuses_repair_when_merge_artifacts_exist() -> TestResult {
        let artifacts = vec![".beads/issues.jsonl.orig".to_owned()];
        let report = compose_integrity_report(BeadsIntegrityInputs {
            jsonl_record_count: 100,
            db_record_count: 100,
            jsonl_parse_error: Some(JsonlParseError {
                line: 101,
                column: None,
                excerpt: "}] }".to_owned(),
            }),
            ..base_inputs(&artifacts, None)
        });

        ensure_equal(
            &report.safe_repair_candidate,
            &Some(false),
            "merge artifacts refuse repair recommendation",
        )?;
        ensure_equal(
            &report.repair_classification,
            &Some(BeadsIntegrityRepairClassification::MergeArtifactsPresent),
            "merge artifact repair classification",
        )?;
        ensure_equal(
            &report.repair_command_candidate,
            &None,
            "merge artifacts have no repair command",
        )
    }

    #[test]
    fn parse_error_refuses_repair_when_counts_are_ambiguous() -> TestResult {
        let report = compose_integrity_report(BeadsIntegrityInputs {
            jsonl_record_count: 100,
            db_record_count: 99,
            jsonl_parse_error: Some(JsonlParseError {
                line: 101,
                column: None,
                excerpt: "}] }".to_owned(),
            }),
            ..base_inputs(&[], None)
        });

        ensure_equal(
            &report.safe_repair_candidate,
            &Some(false),
            "count mismatch refuses repair recommendation",
        )?;
        ensure_equal(
            &report.repair_classification,
            &Some(BeadsIntegrityRepairClassification::DbJsonlCountMismatch),
            "count mismatch repair classification",
        )?;
        ensure_equal(
            &report.repair_command_candidate,
            &None,
            "count mismatch has no repair command",
        )
    }

    #[test]
    fn parse_error_refuses_repair_when_stale_guard_evidence_exists() -> TestResult {
        let report = compose_integrity_report(BeadsIntegrityInputs {
            jsonl_record_count: 100,
            db_record_count: 100,
            external_changes_pending_import: true,
            dirty_issue_count: 1,
            jsonl_parse_error: Some(JsonlParseError {
                line: 101,
                column: None,
                excerpt: "}] }".to_owned(),
            }),
            ..base_inputs(&[], None)
        });

        ensure_equal(
            &report.safe_repair_candidate,
            &Some(false),
            "stale guard evidence refuses repair recommendation",
        )?;
        ensure_equal(
            &report.repair_classification,
            &Some(BeadsIntegrityRepairClassification::StaleDbGuardRisk),
            "stale guard repair classification",
        )?;
        ensure_equal(
            &report.repair_command_candidate,
            &None,
            "stale guard evidence has no repair command",
        )
    }

    #[test]
    fn parse_error_refuses_repair_for_non_trailing_corruption() -> TestResult {
        let report = compose_integrity_report(BeadsIntegrityInputs {
            jsonl_record_count: 100,
            db_record_count: 100,
            jsonl_parse_error: Some(JsonlParseError {
                line: 42,
                column: Some(7),
                excerpt: "{\"id\":".to_owned(),
            }),
            ..base_inputs(&[], None)
        });

        ensure_equal(
            &report.safe_repair_candidate,
            &Some(false),
            "non-trailing parse error refuses repair recommendation",
        )?;
        ensure_equal(
            &report.repair_classification,
            &Some(BeadsIntegrityRepairClassification::UnknownDurableCorruption),
            "non-trailing repair classification",
        )?;
        ensure_equal(
            &report.repair_command_candidate,
            &None,
            "non-trailing parse error has no repair command",
        )
    }

    #[test]
    fn db_more_than_jsonl_is_count_mismatch() -> TestResult {
        let inputs = BeadsIntegrityInputs {
            jsonl_record_count: 100,
            db_record_count: 110,
            auto_import_enabled: true,
            ..base_inputs(&[], None)
        };
        let report = compose_integrity_report(inputs);
        ensure_equal(
            &report.health,
            &BeadsIntegrityHealth::DbJsonlCountMismatch,
            "db>jsonl -> mismatch",
        )?;
        ensure_equal(
            &report.requires_candidate_downgrade,
            &true,
            "mismatch must downgrade candidate safety",
        )?;
        ensure_equal(
            &report.pending_import_count,
            &0,
            "saturating_sub clamps pending at 0 when db > jsonl",
        )
    }

    #[test]
    fn jsonl_more_than_db_without_auto_import_is_count_mismatch() -> TestResult {
        // Without auto-import, JSONL drift is not "pending import" — it
        // is a real DB/JSONL mismatch the agent has to resolve manually.
        let inputs = BeadsIntegrityInputs {
            jsonl_record_count: 105,
            db_record_count: 100,
            auto_import_enabled: false,
            ..base_inputs(&[], None)
        };
        let report = compose_integrity_report(inputs);
        ensure_equal(
            &report.health,
            &BeadsIntegrityHealth::DbJsonlCountMismatch,
            "auto_import=false escalates to mismatch",
        )
    }

    #[test]
    fn jsonl_parse_error_overrides_other_states() -> TestResult {
        // Even with a count mismatch and merge artifacts, a parse error
        // is the worst signal and must dominate.
        let artifacts = vec![".beads/issues.jsonl.orig".to_owned()];
        let parse_error = JsonlParseError {
            line: 2701,
            column: Some(42),
            excerpt: "{\"id\":\"bd-bad\",\"type\":truncated".to_owned(),
        };
        let inputs = BeadsIntegrityInputs {
            jsonl_record_count: 2700,
            db_record_count: 2702,
            auto_import_enabled: true,
            ..base_inputs(&artifacts, Some(parse_error.clone()))
        };
        let report = compose_integrity_report(inputs);
        ensure_equal(
            &report.health,
            &BeadsIntegrityHealth::JsonlParseError,
            "parse error wins",
        )?;
        ensure_equal(
            &report.requires_candidate_downgrade,
            &true,
            "parse error must downgrade candidate safety",
        )?;
        ensure_equal(
            &report.jsonl_parse_error,
            &Some(parse_error),
            "parse error round-trips",
        )
    }

    #[test]
    fn parse_error_excerpt_is_truncated_to_max_len() -> TestResult {
        let huge = "x".repeat(MAX_EXCERPT_LEN + 500);
        let parse_error = JsonlParseError {
            line: 1,
            column: None,
            excerpt: huge,
        };
        let report = compose_integrity_report(BeadsIntegrityInputs {
            jsonl_parse_error: Some(parse_error),
            ..base_inputs(&[], None)
        });
        let stored = report
            .jsonl_parse_error
            .as_ref()
            .map(|e| e.excerpt.len())
            .unwrap_or_default();
        ensure(
            stored <= MAX_EXCERPT_LEN,
            format!("excerpt must be <= {MAX_EXCERPT_LEN}, got {stored}"),
        )
    }

    #[test]
    fn merge_artifact_list_is_sorted_and_bounded() -> TestResult {
        let artifacts: Vec<String> = (0..MAX_MERGE_ARTIFACTS + 4)
            .rev()
            .map(|i| format!(".beads/issues.jsonl.orig.{i:03}"))
            .collect();
        let report = compose_integrity_report(base_inputs(&artifacts, None));
        ensure_equal(
            &report.merge_artifact_paths.len(),
            &MAX_MERGE_ARTIFACTS,
            "bounded",
        )?;
        let sorted: Vec<String> = {
            let mut s = report.merge_artifact_paths.clone();
            s.sort();
            s
        };
        ensure_equal(&report.merge_artifact_paths, &sorted, "sorted")?;
        ensure_equal(
            &report.merge_artifact_count,
            &(MAX_MERGE_ARTIFACTS as u64 + 4),
            "count reflects the full input, not the truncated list",
        )
    }

    #[test]
    fn merge_artifact_authority_uses_full_untruncated_input() -> TestResult {
        let mut artifacts = vec!["beads.base.jsonl".to_owned(); MAX_MERGE_ARTIFACTS];
        artifacts.push(".beads/issues.jsonl.orig".to_owned());

        let report = compose_integrity_report(base_inputs(&artifacts, None));

        ensure_equal(
            &report.merge_artifact_paths.len(),
            &MAX_MERGE_ARTIFACTS,
            "serialized path sample stays bounded",
        )?;
        ensure_equal(
            &report.merge_artifact_count,
            &(MAX_MERGE_ARTIFACTS as u64 + 1),
            "count retains the full artifact set",
        )?;
        ensure_equal(
            &report.tracker_authority_state,
            &BeadsTrackerAuthorityState::MergeArtifacts,
            "non-benign artifact beyond the retained sample still fails closed",
        )?;
        ensure_equal(
            &report.br_reads_authoritative,
            &false,
            "truncated display sample must not make br reads authoritative",
        )
    }

    #[test]
    fn report_serialization_is_byte_stable_across_runs() -> TestResult {
        let merge_artifacts = [".beads/issues.jsonl.orig".to_owned()];
        let inputs = BeadsIntegrityInputs {
            jsonl_record_count: 100,
            db_record_count: 99,
            auto_import_enabled: false,
            ..base_inputs(&merge_artifacts, None)
        };
        let first = serde_json::to_string(&compose_integrity_report(inputs.clone()))
            .map_err(|e| format!("serialize first: {e}"))?;
        let second = serde_json::to_string(&compose_integrity_report(inputs))
            .map_err(|e| format!("serialize second: {e}"))?;
        ensure_equal(&first, &second, "deterministic serialization")
    }

    #[test]
    fn truncate_utf8_respects_codepoint_boundaries() -> TestResult {
        // Each "🦀" is 4 bytes; truncating at byte 6 would split the
        // second crab without boundary handling.
        let s = "🦀🦀🦀";
        let truncated = truncate_utf8(s, 6);
        ensure(
            truncated.len() <= 6,
            format!("truncated len {} must be <= 6", truncated.len()),
        )?;
        ensure(
            truncated.is_char_boundary(truncated.len()),
            "truncated must end on a UTF-8 boundary",
        )?;
        ensure_equal(&truncated.as_str(), &"🦀", "first crab survives")
    }

    #[test]
    fn classify_tracker_authority_state_covers_each_state() -> TestResult {
        use BeadsTrackerAuthorityState::{
            Clean, CountMismatch, DbNewer, DirtyIssues, DoctorMetadataMessageOnly, JsonlNewer,
            MergeArtifacts, ParseError,
        };

        let cases: [(BeadsTrackerAuthoritySignals, BeadsTrackerAuthorityState); 8] = [
            (BeadsTrackerAuthoritySignals::default(), Clean),
            (
                BeadsTrackerAuthoritySignals {
                    doctor_metadata_message: true,
                    ..BeadsTrackerAuthoritySignals::default()
                },
                DoctorMetadataMessageOnly,
            ),
            (
                BeadsTrackerAuthoritySignals {
                    db_newer: true,
                    ..BeadsTrackerAuthoritySignals::default()
                },
                DbNewer,
            ),
            (
                BeadsTrackerAuthoritySignals {
                    jsonl_newer: true,
                    ..BeadsTrackerAuthoritySignals::default()
                },
                JsonlNewer,
            ),
            (
                BeadsTrackerAuthoritySignals {
                    dirty_issue_count: 1,
                    ..BeadsTrackerAuthoritySignals::default()
                },
                DirtyIssues,
            ),
            (
                BeadsTrackerAuthoritySignals {
                    unreconcilable_count_mismatch: true,
                    ..BeadsTrackerAuthoritySignals::default()
                },
                CountMismatch,
            ),
            (
                BeadsTrackerAuthoritySignals {
                    non_benign_merge_artifacts: true,
                    ..BeadsTrackerAuthoritySignals::default()
                },
                MergeArtifacts,
            ),
            (
                BeadsTrackerAuthoritySignals {
                    jsonl_parse_error: true,
                    ..BeadsTrackerAuthoritySignals::default()
                },
                ParseError,
            ),
        ];
        for (signals, expected) in cases {
            ensure_equal(
                &classify_tracker_authority_state(signals),
                &expected,
                &format!("single-signal state for {signals:?}"),
            )?;
        }
        Ok(())
    }

    #[test]
    fn classify_tracker_authority_state_precedence_matches_documented_order() -> TestResult {
        // Documented precedence (worst first): parse_error >
        // merge_artifacts > count_mismatch > dirty_issues > jsonl_newer
        // > db_newer > doctor_metadata_message_only > clean. Start with
        // every signal raised and peel them off in that order.
        use BeadsTrackerAuthorityState::{
            Clean, CountMismatch, DbNewer, DirtyIssues, DoctorMetadataMessageOnly, JsonlNewer,
            MergeArtifacts, ParseError,
        };

        let mut signals = BeadsTrackerAuthoritySignals {
            jsonl_parse_error: true,
            non_benign_merge_artifacts: true,
            unreconcilable_count_mismatch: true,
            dirty_issue_count: 3,
            jsonl_newer: true,
            db_newer: true,
            doctor_metadata_message: true,
        };
        ensure_equal(
            &classify_tracker_authority_state(signals),
            &ParseError,
            "parse error dominates every other signal",
        )?;
        signals.jsonl_parse_error = false;
        ensure_equal(
            &classify_tracker_authority_state(signals),
            &MergeArtifacts,
            "merge artifacts dominate count/dirty/import signals",
        )?;
        signals.non_benign_merge_artifacts = false;
        ensure_equal(
            &classify_tracker_authority_state(signals),
            &CountMismatch,
            "count mismatch dominates dirty/import signals",
        )?;
        signals.unreconcilable_count_mismatch = false;
        ensure_equal(
            &classify_tracker_authority_state(signals),
            &DirtyIssues,
            "dirty issues dominate directional drift",
        )?;
        signals.dirty_issue_count = 0;
        ensure_equal(
            &classify_tracker_authority_state(signals),
            &JsonlNewer,
            "jsonl_newer dominates db_newer",
        )?;
        signals.jsonl_newer = false;
        ensure_equal(
            &classify_tracker_authority_state(signals),
            &DbNewer,
            "db_newer dominates the metadata message",
        )?;
        signals.db_newer = false;
        ensure_equal(
            &classify_tracker_authority_state(signals),
            &DoctorMetadataMessageOnly,
            "metadata message alone is message-only",
        )?;
        signals.doctor_metadata_message = false;
        ensure_equal(
            &classify_tracker_authority_state(signals),
            &Clean,
            "no signal is clean",
        )
    }

    #[test]
    fn metadata_message_with_clean_concrete_evidence_is_message_only_state() -> TestResult {
        let report = compose_integrity_report(BeadsIntegrityInputs {
            external_changes_pending_import: true,
            dirty_issue_count: 0,
            ..base_inputs(&[], None)
        });
        ensure_equal(
            &report.tracker_authority_state,
            &BeadsTrackerAuthorityState::DoctorMetadataMessageOnly,
            "metadata-only message classifies as doctor_metadata_message_only",
        )?;
        ensure(
            report.doctor_metadata_message_only(),
            "report must expose the contradiction accessor",
        )?;
        ensure_equal(
            &report.br_reads_authoritative,
            &true,
            "metadata-only message keeps br reads authoritative",
        )?;
        ensure_equal(
            &report.health,
            &BeadsIntegrityHealth::ExternalChangesPendingImport,
            "serialized health vocabulary is unchanged",
        )
    }

    #[test]
    fn dirty_issues_without_metadata_message_fail_closed() -> TestResult {
        let report = compose_integrity_report(BeadsIntegrityInputs {
            dirty_issue_count: 2,
            ..base_inputs(&[], None)
        });
        ensure_equal(
            &report.tracker_authority_state,
            &BeadsTrackerAuthorityState::DirtyIssues,
            "dirty issues classify concretely",
        )?;
        ensure_equal(
            &report.br_reads_authoritative,
            &false,
            "dirty issues fail closed",
        )?;
        ensure_equal(
            &report.requires_candidate_downgrade,
            &true,
            "dirty issues downgrade candidates",
        )
    }

    #[test]
    fn directional_drift_states_fail_closed() -> TestResult {
        let jsonl_newer = compose_integrity_report(BeadsIntegrityInputs {
            jsonl_record_count: 105,
            db_record_count: 100,
            auto_import_enabled: true,
            ..base_inputs(&[], None)
        });
        ensure_equal(
            &jsonl_newer.tracker_authority_state,
            &BeadsTrackerAuthorityState::JsonlNewer,
            "importable JSONL drift is jsonl_newer",
        )?;
        ensure_equal(
            &jsonl_newer.br_reads_authoritative,
            &false,
            "jsonl_newer fails closed",
        )?;

        let db_newer = compose_integrity_report(BeadsIntegrityInputs {
            jsonl_record_count: 100,
            db_record_count: 110,
            auto_import_enabled: true,
            ..base_inputs(&[], None)
        });
        ensure_equal(
            &db_newer.tracker_authority_state,
            &BeadsTrackerAuthorityState::DbNewer,
            "unexported DB rows are db_newer",
        )?;
        ensure_equal(
            &db_newer.br_reads_authoritative,
            &false,
            "db_newer fails closed",
        )
    }

    #[test]
    fn count_mismatch_without_auto_import_fails_closed() -> TestResult {
        let report = compose_integrity_report(BeadsIntegrityInputs {
            jsonl_record_count: 105,
            db_record_count: 100,
            auto_import_enabled: false,
            ..base_inputs(&[], None)
        });
        ensure_equal(
            &report.tracker_authority_state,
            &BeadsTrackerAuthorityState::CountMismatch,
            "unreconcilable drift is count_mismatch",
        )?;
        ensure_equal(
            &report.br_reads_authoritative,
            &false,
            "count_mismatch fails closed",
        )
    }

    #[test]
    fn benign_merge_base_artifact_stays_clean_non_benign_fails_closed() -> TestResult {
        let benign = vec!["beads.base.jsonl".to_owned()];
        let benign_report = compose_integrity_report(base_inputs(&benign, None));
        ensure_equal(
            &benign_report.health,
            &BeadsIntegrityHealth::Ok,
            "benign merge anchor is not a merge warning",
        )?;
        ensure_equal(
            &benign_report.tracker_authority_state,
            &BeadsTrackerAuthorityState::Clean,
            "benign merge anchor is not a merge_artifacts signal",
        )?;
        ensure_equal(
            &benign_report.br_reads_authoritative,
            &true,
            "benign merge anchor keeps br reads authoritative",
        )?;

        let artifacts = vec![".beads/issues.jsonl.orig".to_owned()];
        let report = compose_integrity_report(base_inputs(&artifacts, None));
        ensure_equal(
            &report.tracker_authority_state,
            &BeadsTrackerAuthorityState::MergeArtifacts,
            "non-benign artifacts classify as merge_artifacts",
        )?;
        ensure_equal(
            &report.br_reads_authoritative,
            &false,
            "merge_artifacts fails closed",
        )
    }

    #[test]
    fn non_benign_merge_artifacts_dominate_metadata_only_pending_import() -> TestResult {
        let artifacts = vec![".beads/issues.jsonl.orig".to_owned()];
        let report = compose_integrity_report(BeadsIntegrityInputs {
            external_changes_pending_import: true,
            dirty_issue_count: 0,
            ..base_inputs(&artifacts, None)
        });

        ensure_equal(
            &report.health,
            &BeadsIntegrityHealth::MergeArtifactsWarn,
            "non-benign merge artifacts must be the coarse health signal",
        )?;
        ensure_equal(
            &report.tracker_authority_state,
            &BeadsTrackerAuthorityState::MergeArtifacts,
            "non-benign merge artifacts dominate metadata-only drift",
        )?;
        ensure_equal(
            &report.pending_import_count,
            &0,
            "metadata-only drift has no pending rows",
        )?;
        ensure_equal(
            &report.br_reads_authoritative,
            &false,
            "non-benign merge artifacts fail closed",
        )?;
        ensure(
            report
                .recovery_hint
                .is_some_and(|hint| hint.contains("merge artifacts")),
            "recovery hint must point at merge artifacts",
        )
    }

    #[test]
    fn tracker_authority_state_agrees_with_br_reads_authoritative() -> TestResult {
        let artifacts = vec![".beads/issues.jsonl.orig".to_owned()];
        let parse_error = JsonlParseError {
            line: 101,
            column: None,
            excerpt: "}] }".to_owned(),
        };
        let combos: Vec<BeadsIntegrityInputs<'_>> = vec![
            base_inputs(&[], None),
            BeadsIntegrityInputs {
                external_changes_pending_import: true,
                ..base_inputs(&[], None)
            },
            BeadsIntegrityInputs {
                dirty_issue_count: 4,
                external_changes_pending_import: true,
                ..base_inputs(&[], None)
            },
            BeadsIntegrityInputs {
                jsonl_record_count: 105,
                db_record_count: 100,
                ..base_inputs(&[], None)
            },
            BeadsIntegrityInputs {
                jsonl_record_count: 100,
                db_record_count: 105,
                ..base_inputs(&[], None)
            },
            BeadsIntegrityInputs {
                jsonl_record_count: 105,
                db_record_count: 100,
                auto_import_enabled: false,
                ..base_inputs(&[], None)
            },
            base_inputs(&artifacts, None),
            base_inputs(&[], Some(parse_error)),
        ];
        for inputs in combos {
            let report = compose_integrity_report(inputs.clone());
            ensure_equal(
                &report.br_reads_authoritative,
                &report.tracker_authority_state.is_authoritative(),
                &format!("authority agreement for {inputs:?}"),
            )?;
        }
        Ok(())
    }

    #[test]
    fn classify_health_severity_ordering_is_stable() -> TestResult {
        use BeadsIntegrityHealth::{
            DbJsonlCountMismatch, ExternalChangesPendingImport, JsonlParseError,
            MergeArtifactsWarn, Ok as HealthOk,
        };
        let ranks = [
            HealthOk.severity_rank(),
            ExternalChangesPendingImport.severity_rank(),
            MergeArtifactsWarn.severity_rank(),
            DbJsonlCountMismatch.severity_rank(),
            JsonlParseError.severity_rank(),
        ];
        ensure(
            ranks.windows(2).all(|w| w[0] < w[1]),
            format!("severity ranks must be strictly increasing: {ranks:?}"),
        )
    }
}
