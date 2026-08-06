//! Degraded-mode honesty checks (EE-253).
//!
//! Validates that degraded responses are honest and actionable:
//! - Severity levels accurately reflect the impact
//! - Repair fields contain valid, executable commands
//! - Messages are clear and informative
//!
//! These checks prevent the system from understating problems or providing
//! unhelpful repair suggestions.

use crate::models::degradation::{
    ALL_DEGRADATION_CODES, ActiveDegradation, DegradationCode, DegradationSeverity,
};

const KNOWN_REPAIR_COMMAND_PREFIXES: &[&str] = &["ee ", "cargo ", "cass ", "chmod ", "sqlite3 "];

fn starts_with_known_repair_prefix(repair: &str) -> bool {
    KNOWN_REPAIR_COMMAND_PREFIXES
        .iter()
        .any(|prefix| repair.starts_with(prefix))
}

/// Result of a single honesty check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HonestyCheckResult {
    /// What was checked.
    pub check_name: &'static str,
    /// Whether the check passed.
    pub passed: bool,
    /// Issue description if failed.
    pub issue: Option<String>,
    /// Degradation code being validated.
    pub code_id: Option<String>,
}

impl HonestyCheckResult {
    /// Create a passing check.
    #[must_use]
    pub const fn pass(check_name: &'static str) -> Self {
        Self {
            check_name,
            passed: true,
            issue: None,
            code_id: None,
        }
    }

    /// Create a passing check for a specific code.
    #[must_use]
    pub fn pass_for(check_name: &'static str, code_id: &str) -> Self {
        Self {
            check_name,
            passed: true,
            issue: None,
            code_id: Some(code_id.to_owned()),
        }
    }

    /// Create a failing check.
    #[must_use]
    pub fn fail(check_name: &'static str, issue: impl Into<String>) -> Self {
        Self {
            check_name,
            passed: false,
            issue: Some(issue.into()),
            code_id: None,
        }
    }

    /// Create a failing check for a specific code.
    #[must_use]
    pub fn fail_for(check_name: &'static str, code_id: &str, issue: impl Into<String>) -> Self {
        Self {
            check_name,
            passed: false,
            issue: Some(issue.into()),
            code_id: Some(code_id.to_owned()),
        }
    }
}

/// Summary of all honesty checks.
#[derive(Clone, Debug)]
pub struct HonestyReport {
    /// Individual check results.
    pub checks: Vec<HonestyCheckResult>,
    /// Overall pass/fail.
    pub passed: bool,
    /// Number of issues found.
    pub issue_count: u32,
}

impl HonestyReport {
    /// Create a report from check results.
    #[must_use]
    pub fn from_checks(checks: Vec<HonestyCheckResult>) -> Self {
        let failed_checks = checks.iter().filter(|c| !c.passed).count();
        let issue_count = u32::try_from(failed_checks).unwrap_or(u32::MAX);
        let passed = issue_count == 0;
        Self {
            checks,
            passed,
            issue_count,
        }
    }
}

/// Validate that a repair command looks actionable.
///
/// Repair commands should:
/// - Start with a known command prefix (ee, cargo, cass, etc.)
/// - Not be empty
/// - Not contain placeholder text
///
/// This is the original permissive check: a repair containing an unresolved
/// `<path>` / `<memory-id>` / `<command>` metavariable still passes here
/// (templates are useful as repair hints even when they are not directly
/// executable). For a finer-grained classification that distinguishes
/// actionable commands from templates, see
/// [`classify_repair_command`] and [`is_repair_command_template`].
#[must_use]
pub fn validate_repair_command(repair: &str) -> HonestyCheckResult {
    if repair.is_empty() {
        return HonestyCheckResult::fail("repair_not_empty", "Repair command is empty");
    }

    if !starts_with_known_repair_prefix(repair) {
        return HonestyCheckResult::fail(
            "repair_known_command",
            format!(
                "Repair command '{}' doesn't start with known prefix",
                repair
            ),
        );
    }

    let placeholder_patterns = ["todo", "fixme", "<placeholder>", "xxx", "???"];
    let lower_repair = repair.to_lowercase();
    for pattern in placeholder_patterns {
        if lower_repair.contains(pattern) {
            return HonestyCheckResult::fail(
                "repair_no_placeholders",
                format!("Repair command contains placeholder pattern: {}", pattern),
            );
        }
    }

    HonestyCheckResult::pass("repair_valid")
}

/// Actionability classification for a repair command (bd-1g7ar).
///
/// `validate_repair_command` is intentionally permissive: it accepts both
/// directly-executable repairs (`ee index rebuild --workspace .`) and
/// templated repair hints that contain unresolved metavariables such as
/// `<path>` or `<memory-id>`. Agent harnesses that want to surface "you can
/// run this command as-is" vs "this is a template to fill in" need a finer
/// classification — this enum names those states.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepairCommandKind {
    /// The repair string is empty.
    Empty,
    /// The repair does not start with a known command prefix.
    Unknown,
    /// The repair matches an explicit placeholder marker (`todo`, `fixme`,
    /// `<placeholder>`, `xxx`, `???`). These signal unfinished authoring,
    /// not a runnable template.
    Placeholder,
    /// The repair contains at least one unresolved angle-bracket metavariable
    /// (e.g. `<file>`, `<path>`, `<memory-id>`). It is meaningful as a
    /// template the agent must fill in, but is not directly executable.
    Template,
    /// The repair starts with a known prefix and contains no metavariables
    /// or placeholder markers — directly executable as-is.
    Actionable,
}

/// Classify a repair command's actionability without mutating the existing
/// permissive `validate_repair_command` contract.
///
/// This is the strict counterpart to [`validate_repair_command`]: it returns
/// a typed kind so downstream code can distinguish runnable commands from
/// templates and placeholders. Callers that need the original boolean-style
/// honesty check should keep using `validate_repair_command`.
#[must_use]
pub fn classify_repair_command(repair: &str) -> RepairCommandKind {
    if repair.is_empty() {
        return RepairCommandKind::Empty;
    }

    if !starts_with_known_repair_prefix(repair) {
        return RepairCommandKind::Unknown;
    }

    let placeholder_patterns = ["todo", "fixme", "<placeholder>", "xxx", "???"];
    let lower_repair = repair.to_lowercase();
    if placeholder_patterns
        .iter()
        .any(|pattern| lower_repair.contains(pattern))
    {
        return RepairCommandKind::Placeholder;
    }

    if contains_unresolved_metavariable(repair) {
        return RepairCommandKind::Template;
    }

    RepairCommandKind::Actionable
}

/// Convenience predicate: true when `repair` contains at least one
/// unresolved `<name>` metavariable (excluding the explicit `<placeholder>`
/// marker, which `classify_repair_command` already maps to
/// `RepairCommandKind::Placeholder`).
#[must_use]
pub fn is_repair_command_template(repair: &str) -> bool {
    contains_unresolved_metavariable(repair)
}

fn contains_unresolved_metavariable(repair: &str) -> bool {
    let bytes = repair.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut j = start;
        while j < bytes.len() {
            let c = bytes[j];
            if c == b'>' {
                break;
            }
            if !(c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'.') {
                break;
            }
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'>' && j > start {
            let name = &bytes[start..j];
            if !name.eq_ignore_ascii_case(b"placeholder") {
                return true;
            }
            i = j + 1;
        } else {
            i = j.max(i + 1);
        }
    }
    false
}

/// Validate that severity accurately reflects impact.
///
/// Critical should only be used when core functionality is impaired.
/// Low is for minor, recoverable limitations; info requires no action.
/// Warning through high are significant but non-critical issues.
#[must_use]
pub fn validate_severity_honesty(code: &DegradationCode) -> HonestyCheckResult {
    match code.severity {
        DegradationSeverity::Critical => {
            if code.auto_recoverable {
                return HonestyCheckResult::fail_for(
                    "severity_critical_not_auto_recoverable",
                    code.id,
                    "Critical degradations should not be auto-recoverable",
                );
            }
        }
        DegradationSeverity::Low => {
            if !code.auto_recoverable && code.repair.is_none() {
                return HonestyCheckResult::fail_for(
                    "severity_low_has_path_forward",
                    code.id,
                    "Low degradations should be auto-recoverable or have repair",
                );
            }
        }
        DegradationSeverity::Info
        | DegradationSeverity::Warning
        | DegradationSeverity::Medium
        | DegradationSeverity::High => {}
    }

    HonestyCheckResult::pass_for("severity_honest", code.id)
}

/// Validate that message is informative.
///
/// Messages should:
/// - Not be empty
/// - Not be generic
/// - Describe what's wrong
#[must_use]
pub fn validate_message_quality(code: &DegradationCode) -> HonestyCheckResult {
    if code.description.is_empty() {
        return HonestyCheckResult::fail_for("message_not_empty", code.id, "Description is empty");
    }

    if code.description.len() < 10 {
        return HonestyCheckResult::fail_for(
            "message_informative",
            code.id,
            format!(
                "Description '{}' is too short to be informative",
                code.description
            ),
        );
    }

    let generic_patterns = [
        "error occurred",
        "something went wrong",
        "unknown error",
        "failed",
    ];
    let lower_description = code.description.to_lowercase();
    for pattern in generic_patterns {
        if lower_description == pattern {
            return HonestyCheckResult::fail_for(
                "message_not_generic",
                code.id,
                format!("Description '{}' is too generic", code.description),
            );
        }
    }

    if code.behavior_change.is_empty() {
        return HonestyCheckResult::fail_for(
            "behavior_change_documented",
            code.id,
            "Behavior change is not documented",
        );
    }

    HonestyCheckResult::pass_for("message_quality", code.id)
}

/// Validate all registered degradation codes for honesty.
#[must_use]
pub fn validate_all_codes() -> HonestyReport {
    let mut checks = Vec::new();

    for code in ALL_DEGRADATION_CODES {
        checks.push(validate_severity_honesty(code));
        checks.push(validate_message_quality(code));

        if let Some(repair) = code.repair {
            checks.push(validate_repair_command(repair));
        }
    }

    HonestyReport::from_checks(checks)
}

/// Validate a specific active degradation.
#[must_use]
pub fn validate_active_degradation(active: &ActiveDegradation) -> HonestyReport {
    let mut checks = Vec::new();

    checks.push(validate_severity_honesty(&active.code));
    checks.push(validate_message_quality(&active.code));

    if let Some(repair) = active.code.repair {
        checks.push(validate_repair_command(repair));
    }

    HonestyReport::from_checks(checks)
}

/// Check that degraded array in a response is honest.
///
/// This validates the runtime degraded state, not just code definitions.
#[must_use]
pub fn validate_degraded_response(
    degraded: &[(String, DegradationSeverity, String, Option<String>)],
) -> HonestyReport {
    let mut checks = Vec::new();

    for (code, _severity, message, repair) in degraded {
        if message.is_empty() {
            checks.push(HonestyCheckResult::fail_for(
                "response_message_not_empty",
                code,
                "Response message is empty",
            ));
        } else if message.len() < 10 {
            checks.push(HonestyCheckResult::fail_for(
                "response_message_informative",
                code,
                format!("Response message '{}' is too short", message),
            ));
        } else {
            checks.push(HonestyCheckResult::pass_for("response_message_ok", code));
        }

        if let Some(r) = repair {
            checks.push(validate_repair_command(r));
        }
    }

    HonestyReport::from_checks(checks)
}

/// Markers that should never appear in normal successful production output.
///
/// Fixture and eval commands may intentionally mention fixture identifiers, but
/// ordinary successful command output must not look like it came from a sample,
/// mock, or stub path. The list is intentionally small and literal so it is
/// explainable when a contract fails.
pub const FORBIDDEN_SUCCESS_MARKERS: &[&str] = &[
    "[sample]",
    "example_",
    "mock_",
    "sample_",
    "stub_",
    "stubbed",
    "stub success",
    "fixture_",
    "tests/fixtures/",
];

/// Successful outputs must not claim evidence-backed validity without evidence.
///
/// The markers are matched against compact lower-case JSON/text so spacing and
/// renderer formatting do not change the result.
pub const UNSUPPORTED_EVIDENCE_CLAIM_MARKERS: &[(&str, &str)] = &[
    ("persisted records", r#""persisted":true"#),
    ("pack selection", r#""selected":true"#),
    ("pack selection reason", r#""selectionreason":"#),
    ("risk assessment", r#""risklevel":"#),
    ("risk score", r#""riskscore":"#),
    ("curation maturity", r#""maturity":"#),
    ("graph PageRank", r#""pagerank":"#),
    ("graph PageRank", "\"pagerank\":"),
    ("graph betweenness", "\"betweenness\":"),
    ("graph explanation", r#""graphexplanation":"#),
    ("certificate validity", r#""result":"valid""#),
    ("certificate hash verification", r#""hashverified":true"#),
    (
        "certificate verification message",
        "certificateverificationpassed",
    ),
    ("replay success", r#""replayoutcome":"success""#),
    ("verified replay hash", r#""episodehashverified":true"#),
    ("procedure validation", r#""overallresult":"passed""#),
    ("procedure verified status", r#""status":"verified""#),
    ("causal uplift", r#""uplift""#),
    ("causal confidence", r#""confidencestate":"#),
];

/// Evidence-source markers that make a validity claim supportable.
pub const CONCRETE_EVIDENCE_SOURCE_MARKERS: &[&str] = &[
    r#""evidenceids":["#,
    r#""sourceids":["#,
    r#""sourceschecked":[{"#,
    r#""manifestpath":""#,
    r#""manifesthash":""#,
    r#""artifacthash":""#,
    r#""payloadhash":""#,
    r#""databasepath":""#,
    r#""auditid":""#,
    r#""provenance":["#,
    r#""scorecomponents":{"#,
    r#""scorebreakdown":{"#,
    r#""packhash":""#,
    r#""graphsnapshotid":""#,
    r#""recorderrunids":["#,
    r#""contextpackids":["#,
    r#""preflightids":["#,
    r#""tripwireids":["#,
    r#""procedureids":["#,
];

const PERSISTED_RECORD_EVIDENCE_MARKERS: &[&str] = &[
    r#""auditid":""#,
    r#""databasepath":""#,
    r#""recorderrunids":["#,
    r#""evidenceids":["#,
];

const PACK_SELECTION_EVIDENCE_MARKERS: &[&str] = &[
    r#""evidenceids":["#,
    r#""sourceids":["#,
    r#""provenance":["#,
    r#""scorecomponents":{"#,
    r#""scorebreakdown":{"#,
    r#""packhash":""#,
    r#""contextpackids":["#,
];

const RISK_EVIDENCE_MARKERS: &[&str] = &[
    r#""sourceschecked":[{"#,
    r#""preflightids":["#,
    r#""tripwireids":["#,
    r#""evidenceids":["#,
    r#""sourceids":["#,
];

const CURATION_EVIDENCE_MARKERS: &[&str] = &[
    r#""evidenceids":["#,
    r#""sourceids":["#,
    r#""procedureids":["#,
    r#""provenance":["#,
];

const GRAPH_EVIDENCE_MARKERS: &[&str] = &[
    r#""graphsnapshotid":""#,
    r#""sourceids":["#,
    r#""evidenceids":["#,
    r#""provenance":["#,
];

const CERTIFICATE_EVIDENCE_MARKERS: &[&str] = &[
    r#""manifestpath":""#,
    r#""manifesthash":""#,
    r#""artifacthash":""#,
    r#""payloadhash":""#,
    r#""sourceids":["#,
];

const REPLAY_EVIDENCE_MARKERS: &[&str] = &[
    r#""manifestpath":""#,
    r#""manifesthash":""#,
    r#""artifacthash":""#,
    r#""payloadhash":""#,
    r#""recorderrunids":["#,
    r#""contextpackids":["#,
    r#""evidenceids":["#,
];

const PROCEDURE_EVIDENCE_MARKERS: &[&str] = &[
    r#""sourceschecked":[{"#,
    r#""procedureids":["#,
    r#""preflightids":["#,
    r#""evidenceids":["#,
    r#""sourceids":["#,
];

const CAUSAL_EVIDENCE_MARKERS: &[&str] = &[
    r#""manifestpath":""#,
    r#""manifesthash":""#,
    r#""artifacthash":""#,
    r#""payloadhash":""#,
    r#""recorderrunids":["#,
    r#""contextpackids":["#,
    r#""evidenceids":["#,
    r#""sourceids":["#,
];

fn compact_ascii_lowercase(input: &str) -> String {
    input
        .chars()
        .filter(|character| !character.is_whitespace())
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

fn has_concrete_evidence_source(compact_output: &str) -> bool {
    CONCRETE_EVIDENCE_SOURCE_MARKERS
        .iter()
        .any(|marker| compact_output.contains(marker))
}

fn evidence_markers_for_claim(claim: &str) -> &'static [&'static str] {
    match claim {
        "persisted records" => PERSISTED_RECORD_EVIDENCE_MARKERS,
        "pack selection" | "pack selection reason" => PACK_SELECTION_EVIDENCE_MARKERS,
        "risk assessment" | "risk score" => RISK_EVIDENCE_MARKERS,
        "curation maturity" => CURATION_EVIDENCE_MARKERS,
        "graph PageRank" | "graph betweenness" | "graph explanation" => GRAPH_EVIDENCE_MARKERS,
        "certificate validity"
        | "certificate hash verification"
        | "certificate verification message" => CERTIFICATE_EVIDENCE_MARKERS,
        "replay success" | "verified replay hash" => REPLAY_EVIDENCE_MARKERS,
        "procedure validation" | "procedure verified status" => PROCEDURE_EVIDENCE_MARKERS,
        "causal uplift" | "causal confidence" => CAUSAL_EVIDENCE_MARKERS,
        _ => CONCRETE_EVIDENCE_SOURCE_MARKERS,
    }
}

fn claim_has_relevant_evidence_source(claim: &str, compact_output: &str) -> bool {
    evidence_markers_for_claim(claim)
        .iter()
        .any(|marker| compact_output.contains(marker))
}

fn object_field_case_insensitive<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<&'a serde_json::Value> {
    object
        .iter()
        .find_map(|(field, value)| field.eq_ignore_ascii_case(key).then_some(value))
}

fn object_has_field_case_insensitive(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> bool {
    object_field_case_insensitive(object, key).is_some()
}

fn object_bool_field_is(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    expected: bool,
) -> bool {
    object_field_case_insensitive(object, key).and_then(serde_json::Value::as_bool)
        == Some(expected)
}

fn object_string_field_eq_ignore_ascii_case(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    expected: &str,
) -> bool {
    object_field_case_insensitive(object, key)
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case(expected))
}

fn object_string_value_compacts_to(
    object: &serde_json::Map<String, serde_json::Value>,
    expected: &str,
) -> bool {
    object
        .values()
        .filter_map(serde_json::Value::as_str)
        .any(|value| compact_ascii_lowercase(value).contains(expected))
}

fn local_unsupported_evidence_claims(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Vec<&'static str> {
    let mut claims = Vec::new();
    if object_bool_field_is(object, "persisted", true) {
        claims.push("persisted records");
    }
    if object_bool_field_is(object, "selected", true) {
        claims.push("pack selection");
    }
    if object_has_field_case_insensitive(object, "selectionReason") {
        claims.push("pack selection reason");
    }
    if object_has_field_case_insensitive(object, "riskLevel") {
        claims.push("risk assessment");
    }
    if object_has_field_case_insensitive(object, "riskScore") {
        claims.push("risk score");
    }
    if object_has_field_case_insensitive(object, "maturity") {
        claims.push("curation maturity");
    }
    if object_has_field_case_insensitive(object, "pageRank") {
        claims.push("graph PageRank");
    }
    if object_has_field_case_insensitive(object, "betweenness") {
        claims.push("graph betweenness");
    }
    if object_has_field_case_insensitive(object, "graphExplanation") {
        claims.push("graph explanation");
    }
    if object_string_field_eq_ignore_ascii_case(object, "result", "valid") {
        claims.push("certificate validity");
    }
    if object_bool_field_is(object, "hashVerified", true) {
        claims.push("certificate hash verification");
    }
    if object_string_value_compacts_to(object, "certificateverificationpassed") {
        claims.push("certificate verification message");
    }
    if object_string_field_eq_ignore_ascii_case(object, "replayOutcome", "success") {
        claims.push("replay success");
    }
    if object_bool_field_is(object, "episodeHashVerified", true) {
        claims.push("verified replay hash");
    }
    if object_string_field_eq_ignore_ascii_case(object, "overallResult", "passed") {
        claims.push("procedure validation");
    }
    if object_string_field_eq_ignore_ascii_case(object, "status", "verified") {
        claims.push("procedure verified status");
    }
    if object_has_field_case_insensitive(object, "uplift") {
        claims.push("causal uplift");
    }
    if object_has_field_case_insensitive(object, "confidenceState") {
        claims.push("causal confidence");
    }
    claims
}

fn collect_json_evidence_claim_checks(
    command_path: &str,
    path: &str,
    value: &serde_json::Value,
    checks: &mut Vec<HonestyCheckResult>,
) {
    match value {
        serde_json::Value::Object(object) => {
            let claims = local_unsupported_evidence_claims(object);
            if !claims.is_empty() {
                let compact_value = serde_json::to_string(value)
                    .ok()
                    .map(|json| compact_ascii_lowercase(&json));
                for claim in claims {
                    let has_evidence = compact_value
                        .as_deref()
                        .is_some_and(|compact| claim_has_relevant_evidence_source(claim, compact));
                    if has_evidence {
                        checks.push(HonestyCheckResult::pass_for(
                            "no_unsupported_evidence_claim",
                            command_path,
                        ));
                    } else {
                        checks.push(HonestyCheckResult::fail_for(
                            "no_unsupported_evidence_claim",
                            command_path,
                            format!(
                                "Successful production output claims {claim} at {path} without concrete evidence source"
                            ),
                        ));
                    }
                }
            }

            for (key, child) in object {
                let child_path = if path.is_empty() {
                    format!("/{key}")
                } else {
                    format!("{path}/{key}")
                };
                collect_json_evidence_claim_checks(command_path, &child_path, child, checks);
            }
        }
        serde_json::Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                let child_path = format!("{path}/{index}");
                collect_json_evidence_claim_checks(command_path, &child_path, child, checks);
            }
        }
        _ => {}
    }
}

/// Validate that a successful production command did not return fake data.
///
/// Failed/degraded commands are allowed to explain that behavior is unavailable.
/// Fixture-mode commands are allowed to identify fixtures explicitly. Everything
/// else that reports success must not include sample/mock/stub markers.
#[must_use]
pub fn validate_no_fake_success_output(
    command_path: &str,
    success: bool,
    fixture_mode: bool,
    output: &str,
) -> HonestyReport {
    if !success {
        return HonestyReport::from_checks(vec![HonestyCheckResult::pass_for(
            "fake_success_not_applicable_for_failure",
            command_path,
        )]);
    }

    if fixture_mode {
        return HonestyReport::from_checks(vec![HonestyCheckResult::pass_for(
            "fake_success_allowed_in_fixture_mode",
            command_path,
        )]);
    }

    let lower_output = output.to_ascii_lowercase();
    let checks = FORBIDDEN_SUCCESS_MARKERS
        .iter()
        .map(|marker| {
            if lower_output.contains(marker) {
                HonestyCheckResult::fail_for(
                    "no_fake_success_output",
                    command_path,
                    format!("Successful production output contains fake-data marker `{marker}`"),
                )
            } else {
                HonestyCheckResult::pass_for("no_fake_success_output", command_path)
            }
        })
        .collect();

    HonestyReport::from_checks(checks)
}

/// Validate that successful output does not overclaim unsupported evidence.
///
/// This complements fake-data marker checks. A command can avoid words like
/// "mock" and still claim that a certificate is valid, a replay succeeded, or a
/// record was persisted without naming any concrete evidence source.
#[must_use]
pub fn validate_no_unsupported_evidence_claims(
    command_path: &str,
    success: bool,
    fixture_mode: bool,
    output: &str,
) -> HonestyReport {
    if !success {
        return HonestyReport::from_checks(vec![HonestyCheckResult::pass_for(
            "evidence_claim_not_applicable_for_failure",
            command_path,
        )]);
    }

    if fixture_mode {
        return HonestyReport::from_checks(vec![HonestyCheckResult::pass_for(
            "evidence_claim_allowed_in_fixture_mode",
            command_path,
        )]);
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(output) {
        let mut checks = Vec::new();
        collect_json_evidence_claim_checks(command_path, "", &value, &mut checks);
        if checks.is_empty() {
            checks.push(HonestyCheckResult::pass_for(
                "no_unsupported_evidence_claim",
                command_path,
            ));
        }
        return HonestyReport::from_checks(checks);
    }

    let compact_output = compact_ascii_lowercase(output);
    let has_evidence = has_concrete_evidence_source(&compact_output);
    let checks = UNSUPPORTED_EVIDENCE_CLAIM_MARKERS
        .iter()
        .map(|(claim, marker)| {
            if compact_output.contains(marker) && !has_evidence {
                HonestyCheckResult::fail_for(
                    "no_unsupported_evidence_claim",
                    command_path,
                    format!("Successful production output claims {claim} without concrete evidence source"),
                )
            } else {
                HonestyCheckResult::pass_for("no_unsupported_evidence_claim", command_path)
            }
        })
        .collect();

    HonestyReport::from_checks(checks)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn validate_repair_rejects_empty() -> TestResult {
        let result = validate_repair_command("");
        ensure(result.passed, false, "empty repair fails")
    }

    #[test]
    fn validate_repair_accepts_ee_command() -> TestResult {
        let result = validate_repair_command("ee index rebuild");
        ensure(result.passed, true, "ee command passes")
    }

    #[test]
    fn validate_repair_accepts_cass_command() -> TestResult {
        let result = validate_repair_command("cass index --full");
        ensure(result.passed, true, "cass command passes")
    }

    #[test]
    fn validate_repair_accepts_chmod_command() -> TestResult {
        let result = validate_repair_command("chmod 600 /path/to/file");
        ensure(result.passed, true, "chmod command passes")
    }

    #[test]
    fn validate_repair_rejects_placeholder() -> TestResult {
        let result = validate_repair_command("ee TODO fix this");
        ensure(result.passed, false, "TODO placeholder fails")
    }

    #[test]
    fn validate_repair_rejects_unknown_prefix() -> TestResult {
        let result = validate_repair_command("unknown command");
        ensure(result.passed, false, "unknown prefix fails")
    }

    #[test]
    fn validate_repair_rejects_raw_rm_command() -> TestResult {
        let result = validate_repair_command("rm -rf target");
        ensure(result.passed, false, "raw rm repair fails")
    }

    #[test]
    fn classify_repair_returns_empty_for_empty_string() -> TestResult {
        ensure(
            classify_repair_command(""),
            RepairCommandKind::Empty,
            "empty repair",
        )
    }

    #[test]
    fn classify_repair_returns_unknown_for_unknown_prefix() -> TestResult {
        ensure(
            classify_repair_command("ssh deploy --rollback"),
            RepairCommandKind::Unknown,
            "unknown prefix",
        )
    }

    #[test]
    fn classify_repair_returns_unknown_for_raw_rm_command() -> TestResult {
        ensure(
            classify_repair_command("rm -rf target"),
            RepairCommandKind::Unknown,
            "raw rm command is not directly actionable",
        )
    }

    #[test]
    fn classify_repair_returns_placeholder_for_marker() -> TestResult {
        ensure(
            classify_repair_command("ee index rebuild TODO"),
            RepairCommandKind::Placeholder,
            "TODO marker is placeholder, not template",
        )?;
        ensure(
            classify_repair_command("ee remember <placeholder>"),
            RepairCommandKind::Placeholder,
            "<placeholder> marker is placeholder, not template",
        )
    }

    #[test]
    fn classify_repair_detects_template_for_angle_bracket_metavariables() -> TestResult {
        ensure(
            classify_repair_command("ee mesh export --peer <peer-id> --out <file>"),
            RepairCommandKind::Template,
            "<file> metavariable -> Template",
        )?;
        ensure(
            classify_repair_command("ee index rebuild --workspace <path>"),
            RepairCommandKind::Template,
            "<path> metavariable -> Template",
        )?;
        ensure(
            classify_repair_command("ee memory show <memory-id> --json"),
            RepairCommandKind::Template,
            "<memory-id> (kebab) metavariable -> Template",
        )?;
        ensure(
            classify_repair_command("ee preflight check --cmd '<command>' --json"),
            RepairCommandKind::Template,
            "<command> inside quotes -> Template",
        )
    }

    #[test]
    fn classify_repair_returns_actionable_for_concrete_command() -> TestResult {
        ensure(
            classify_repair_command("ee index rebuild --workspace ."),
            RepairCommandKind::Actionable,
            "concrete ee command -> Actionable",
        )?;
        ensure(
            classify_repair_command("chmod 600 /var/lib/ee/db.sqlite"),
            RepairCommandKind::Actionable,
            "chmod with concrete path -> Actionable",
        )?;
        // A legitimate quoted command that resembles shell syntax but contains
        // no `<name>` metavariable should remain Actionable, not Template.
        ensure(
            classify_repair_command("ee preflight check --cmd 'rm -rf target' --json"),
            RepairCommandKind::Actionable,
            "quoted concrete command -> Actionable",
        )
    }

    #[test]
    fn is_repair_command_template_matches_classify() -> TestResult {
        ensure(
            is_repair_command_template("ee mesh export --peer <peer-id> --out <file>"),
            true,
            "<file> is template",
        )?;
        ensure(
            is_repair_command_template("ee index rebuild --workspace ."),
            false,
            "concrete command is not template",
        )?;
        ensure(
            is_repair_command_template("ee remember <placeholder>"),
            false,
            "<placeholder> marker is not a template metavariable",
        )?;
        // Stray `<` without a closing `>` must not be treated as a metavariable.
        ensure(
            is_repair_command_template("ee compare a < b"),
            false,
            "stray < is not a metavariable",
        )
    }

    #[test]
    fn all_registered_codes_are_honest() {
        let report = validate_all_codes();
        assert!(
            report.passed,
            "All codes should pass honesty checks. Failures: {:?}",
            report
                .checks
                .iter()
                .filter(|c| !c.passed)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn honesty_report_counts_issues() {
        let checks = vec![
            HonestyCheckResult::pass("check1"),
            HonestyCheckResult::fail("check2", "issue"),
            HonestyCheckResult::pass("check3"),
        ];
        let report = HonestyReport::from_checks(checks);

        assert!(!report.passed);
        assert_eq!(report.issue_count, 1);
    }

    #[test]
    fn honesty_report_passes_with_no_issues() {
        let checks = vec![
            HonestyCheckResult::pass("check1"),
            HonestyCheckResult::pass("check2"),
        ];
        let report = HonestyReport::from_checks(checks);

        assert!(report.passed);
        assert_eq!(report.issue_count, 0);
    }

    #[test]
    fn validate_degraded_response_checks_messages() {
        let degraded = vec![
            (
                "D001".to_string(),
                DegradationSeverity::Warning,
                "Semantic search unavailable".to_string(),
                Some("ee index rebuild".to_string()),
            ),
            (
                "D002".to_string(),
                DegradationSeverity::Warning,
                "".to_string(),
                None,
            ),
        ];

        let report = validate_degraded_response(&degraded);
        assert!(!report.passed);
        assert!(report.issue_count >= 1);
    }

    #[test]
    fn severity_critical_with_auto_recover_fails() {
        use crate::models::degradation::{DegradationCode, DegradedSubsystem};

        let code = DegradationCode {
            id: "TEST",
            subsystem: DegradedSubsystem::Search,
            severity: DegradationSeverity::Critical,
            description: "Test critical issue",
            behavior_change: "Test behavior",
            auto_recoverable: true,
            repair: None,
        };

        let result = validate_severity_honesty(&code);
        assert!(!result.passed);
    }

    #[test]
    fn message_quality_rejects_empty_description() {
        use crate::models::degradation::{DegradationCode, DegradedSubsystem};

        let code = DegradationCode {
            id: "TEST",
            subsystem: DegradedSubsystem::Search,
            severity: DegradationSeverity::Low,
            description: "",
            behavior_change: "Test behavior",
            auto_recoverable: true,
            repair: None,
        };

        let result = validate_message_quality(&code);
        assert!(!result.passed);
    }

    #[test]
    fn message_quality_rejects_empty_behavior_change() {
        use crate::models::degradation::{DegradationCode, DegradedSubsystem};

        let code = DegradationCode {
            id: "TEST",
            subsystem: DegradedSubsystem::Search,
            severity: DegradationSeverity::Low,
            description: "Test description here",
            behavior_change: "",
            auto_recoverable: true,
            repair: None,
        };

        let result = validate_message_quality(&code);
        assert!(!result.passed);
    }

    #[test]
    fn fake_success_output_rejects_stub_marker() {
        let report = validate_no_fake_success_output(
            "preflight show",
            true,
            false,
            r#"{"schema":"ee.response.v2","success":true,"data":{"status":"stubbed"}}"#,
        );

        assert!(!report.passed);
        assert_eq!(report.issue_count, 1);
    }

    #[test]
    fn fake_success_output_allows_fixture_mode() {
        let report = validate_no_fake_success_output(
            "eval run",
            true,
            true,
            r#"{"schema":"ee.response.v2","success":true,"data":{"fixtureId":"fixture_release"}}"#,
        );

        assert!(report.passed);
    }

    #[test]
    fn fake_success_output_ignores_degraded_failure() {
        let report = validate_no_fake_success_output(
            "context",
            false,
            false,
            r#"{"schema":"ee.error.v2","error":{"message":"stub store unavailable"}}"#,
        );

        assert!(report.passed);
    }

    #[test]
    fn unsupported_evidence_claim_rejects_valid_certificate_without_sources() {
        let report = validate_no_unsupported_evidence_claims(
            "certificate verify",
            true,
            false,
            r#"{"schema":"ee.certificate.verify.v1","success":true,"data":{"result":"valid","hashVerified":true,"message":"Certificate verification passed"}}"#,
        );

        assert!(!report.passed);
        assert!(report.issue_count >= 1);
    }

    #[test]
    fn unsupported_evidence_claim_accepts_manifest_backed_certificate() {
        let report = validate_no_unsupported_evidence_claims(
            "certificate verify",
            true,
            false,
            r#"{"schema":"ee.certificate.verify.v1","success":true,"data":{"result":"valid","hashVerified":true,"manifestHash":"blake3:abc123","payloadHash":"blake3:def456"}}"#,
        );

        assert!(report.passed);
    }

    #[test]
    fn unsupported_evidence_claim_rejects_certificate_with_unrelated_audit_id() {
        let report = validate_no_unsupported_evidence_claims(
            "certificate verify",
            true,
            false,
            r#"{"schema":"ee.response.v2","success":true,"data":{"result":"valid","hashVerified":true,"auditId":"audit_unrelated"}}"#,
        );

        assert!(!report.passed);
        assert_eq!(report.issue_count, 2);
        assert!(
            report
                .checks
                .iter()
                .filter_map(|check| check.issue.as_deref())
                .all(|issue| issue.contains("/data")),
            "unsupported certificate claims should stay attributed to the claim-bearing object: {report:?}"
        );
    }

    #[test]
    fn unsupported_evidence_claim_rejects_certificate_success_text_outside_message_field() {
        let report = validate_no_unsupported_evidence_claims(
            "certificate verify",
            true,
            false,
            r#"{"schema":"ee.certificate.verify.v1","success":true,"data":{"summary":"Certificate verification passed"}}"#,
        );
        let backed_report = validate_no_unsupported_evidence_claims(
            "certificate verify",
            true,
            false,
            r#"{"schema":"ee.certificate.verify.v1","success":true,"data":{"summary":"Certificate verification passed","manifestHash":"blake3:abc123"}}"#,
        );

        assert!(!report.passed);
        assert_eq!(report.issue_count, 1);
        assert!(backed_report.passed);
    }

    #[test]
    fn unsupported_evidence_claim_accepts_persisted_record_with_audit_id() {
        let report = validate_no_unsupported_evidence_claims(
            "remember",
            true,
            false,
            r#"{"schema":"ee.response.v2","success":true,"data":{"persisted":true,"auditId":"audit_memory_write"}}"#,
        );

        assert!(report.passed);
    }

    #[test]
    fn unsupported_evidence_claim_rejects_claim_with_unrelated_sibling_sources() {
        let report = validate_no_unsupported_evidence_claims(
            "certificate verify",
            true,
            false,
            r#"{"schema":"ee.response.v2","success":true,"data":{"certificate":{"result":"valid","hashVerified":true},"unrelatedPack":{"provenance":["mem://abc"],"scoreComponents":{"lexical":0.4}}}}"#,
        );

        assert!(!report.passed);
        assert_eq!(report.issue_count, 2);
        assert!(
            report
                .checks
                .iter()
                .filter_map(|check| check.issue.as_deref())
                .all(|issue| issue.contains("/data/certificate")),
            "unsupported claim should be attributed to the claim-bearing object: {report:?}"
        );
    }

    #[test]
    fn unsupported_evidence_claim_rejects_pack_risk_graph_and_curation_claims_without_sources() {
        let outputs = [
            (
                "context",
                r#"{"schema":"ee.response.v2","success":true,"data":{"items":[{"selected":true,"selectionReason":"best match"}]}}"#,
            ),
            (
                "preflight show",
                r#"{"schema":"ee.response.v2","success":true,"data":{"riskLevel":"high","riskScore":0.91}}"#,
            ),
            (
                "rule show",
                r#"{"schema":"ee.response.v2","success":true,"data":{"maturity":"validated"}}"#,
            ),
            (
                "graph neighborhood",
                r#"{"schema":"ee.response.v2","success":true,"data":{"pageRank":0.42,"betweenness":0.11,"graphExplanation":"central evidence node"}}"#,
            ),
        ];

        for (command, output) in outputs {
            let report = validate_no_unsupported_evidence_claims(command, true, false, output);
            assert!(
                !report.passed,
                "{command} should reject unsupported successful reasoning claims"
            );
        }
    }

    #[test]
    fn unsupported_evidence_claim_accepts_pack_and_graph_claims_with_sources() {
        let pack_report = validate_no_unsupported_evidence_claims(
            "context",
            true,
            false,
            r#"{"schema":"ee.response.v2","success":true,"data":{"items":[{"selected":true,"selectionReason":"best match","provenance":["mem://abc"],"scoreComponents":{"lexical":0.4}}],"packHash":"blake3:pack"}}"#,
        );
        let graph_report = validate_no_unsupported_evidence_claims(
            "graph neighborhood",
            true,
            false,
            r#"{"schema":"ee.response.v2","success":true,"data":{"pageRank":0.42,"betweenness":0.11,"graphSnapshotId":"graph_001","sourceIds":["mem_001"]}}"#,
        );

        assert!(pack_report.passed);
        assert!(graph_report.passed);
    }

    #[test]
    fn unsupported_evidence_claim_ignores_failures_and_fixture_mode() {
        let failure = validate_no_unsupported_evidence_claims(
            "certificate verify",
            false,
            false,
            r#"{"schema":"ee.error.v2","error":{"message":"certificate validity unavailable"}}"#,
        );
        let fixture = validate_no_unsupported_evidence_claims(
            "procedure verify",
            true,
            true,
            r#"{"schema":"ee.response.v2","success":true,"data":{"overallResult":"passed","sourcesChecked":[{"sourceId":"fixture_001"}]}}"#,
        );

        assert!(failure.passed);
        assert!(fixture.passed);
    }
}
