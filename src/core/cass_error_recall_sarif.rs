//! Bounded SARIF 2.1.0 diagnostics for the shared import/diagnose/pack reader.
//!
//! Only inline driver-rule results are interpreted. Rule descriptions, fixes,
//! code flows and source snippets are never diagnostic text. Unknown families
//! are left to other readers; a claimed but unsupported report fails closed.
//! A clean report is not proof that an earlier finding was checked again:
//! repair coverage requires an explicit `pass` for the same tool, rule and
//! exact physical location, as well as the caller's process/context gate.
//!
//! Contract: OASIS SARIF 2.1.0, sections 3.20, 3.27, 3.28 and 3.49.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::Value;

use super::super::MAX_DIAGNOSTICS_PER_FAILURE;
use super::{MAX_SCANNED_OUTPUT_BYTES, compiler};
use crate::core::error_recall::{CanonicalDiagnostic, DiagnosticTool, canonical_message_template};

#[path = "cass_error_recall_sarif_context.rs"]
mod context;

const MAX_RECORDS: usize = 256;

type Subject = [u8; 32];

#[derive(Default)]
pub(super) struct Execution {
    failed: BTreeSet<Subject>,
    passed: BTreeSet<Subject>,
    complete: bool,
    unlocated_failure: bool,
    invocation_scope: Option<Subject>,
}

impl Execution {
    pub(super) fn has_known_failures(&self) -> bool {
        self.complete
            && self.invocation_scope.is_some()
            && !self.unlocated_failure
            && !self.failed.is_empty()
    }

    pub(super) fn verified_by(&self, proof: &Self) -> bool {
        self.has_known_failures()
            && proof.complete
            && self.invocation_scope == proof.invocation_scope
            && proof.failed.is_empty()
            && !proof.unlocated_failure
            && self.failed.is_subset(&proof.passed)
    }
}

pub(super) struct Decoded {
    pub(super) output: compiler::CompilerOutput,
    pub(super) execution: Execution,
}

pub(in crate::core::cass_error_recall) fn inspect(
    text: &str,
) -> Result<Option<compiler::CompilerOutput>, ()> {
    Ok(decode(text)?.map(|decoded| decoded.output))
}

fn present<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    serde::de::IgnoredAny::deserialize(deserializer)?;
    Ok(true)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Log {
    version: String,
    runs: Option<Vec<Run>>,
    #[serde(default, deserialize_with = "present")]
    inline_external_properties: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Run {
    tool: Tool,
    results: Option<Vec<Finding>>,
    invocations: Option<Vec<Invocation>>,
    #[serde(default, deserialize_with = "present")]
    external_property_file_references: bool,
    #[serde(default, deserialize_with = "present")]
    original_uri_base_ids: bool,
    column_kind: Option<String>,
    #[serde(default)]
    policies: Vec<Value>,
}

#[derive(Deserialize)]
struct Tool {
    driver: Driver,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Driver {
    name: String,
    version: Option<String>,
    semantic_version: Option<String>,
    #[serde(default)]
    rules: Vec<Rule>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Rule {
    id: String,
    default_configuration: Option<Configuration>,
}

#[derive(Deserialize)]
struct Configuration {
    level: Option<String>,
    enabled: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Invocation {
    execution_successful: bool,
    // The outer tool invocation may merely print a saved report. Preserve the
    // analyzer's own arguments, environment and execution root as well.
    #[serde(flatten)]
    scope: context::InvocationScope,
    #[serde(default)]
    tool_execution_notifications: Vec<Notification>,
    #[serde(default)]
    tool_configuration_notifications: Vec<Notification>,
    #[serde(default)]
    rule_configuration_overrides: Vec<Value>,
}

#[derive(Deserialize)]
struct Notification {
    level: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Finding {
    rule_id: Option<String>,
    rule_index: Option<usize>,
    // Extension/component and external rule references need a different
    // resolver. Presence, including null, must not fall back to driver rules.
    #[serde(default, deserialize_with = "present")]
    rule: bool,
    kind: Option<String>,
    level: Option<String>,
    message: Message,
    #[serde(default)]
    locations: Vec<Location>,
    #[serde(default)]
    suppressions: Vec<Value>,
    baseline_state: Option<String>,
}

#[derive(Deserialize)]
struct Message {
    text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Location {
    physical_location: Option<PhysicalLocation>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PhysicalLocation {
    artifact_location: ArtifactLocation,
    region: Option<Region>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArtifactLocation {
    uri: Option<String>,
    #[serde(default, deserialize_with = "present")]
    uri_base_id: bool,
    #[serde(default, deserialize_with = "present")]
    index: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Region {
    start_line: Option<u64>,
    start_column: Option<u64>,
    end_line: Option<u64>,
    end_column: Option<u64>,
    char_offset: Option<u64>,
    char_length: Option<u64>,
    byte_offset: Option<u64>,
    byte_length: Option<u64>,
    // Ignore source snippets, but permit their normal presence in SARIF.
    #[serde(rename = "snippet")]
    _snippet: Option<serde::de::IgnoredAny>,
    #[serde(rename = "message")]
    _message: Option<serde::de::IgnoredAny>,
    #[serde(rename = "sourceLanguage")]
    _source_language: Option<serde::de::IgnoredAny>,
    #[serde(rename = "properties")]
    _properties: Option<serde::de::IgnoredAny>,
}

fn valid_level(level: &str) -> bool {
    matches!(level, "none" | "note" | "warning" | "error")
}

fn commitment(domain: &[u8], fields: &[&str]) -> Subject {
    let mut hash = blake3::Hasher::new();
    hash.update(domain);
    for field in fields {
        hash.update(&(field.len() as u64).to_le_bytes());
        hash.update(field.as_bytes());
    }
    *hash.finalize().as_bytes()
}

fn canonical(tool: &str, rule: &str, message: &str) -> CanonicalDiagnostic {
    // Arbitrary producer/rule names may contain paths or secrets. Persist a
    // domain-separated exact commitment, not those untrusted strings. Message
    // wording and tool versions do not fragment the same producer/rule class.
    let key = commitment(b"ee.sarif.rule.v1\0", &[tool, rule]);
    let detail = crate::policy::redact_secret_like_content(message).content;
    CanonicalDiagnostic {
        tool: DiagnosticTool::Shell,
        canonical_code: Some(format!("sarif:{}", blake3::Hash::from(key).to_hex())),
        message_template: canonical_message_template(&format!("sarif: {detail}")),
    }
}

fn rule_for<'a>(
    finding: &'a Finding,
    driver: &'a Driver,
) -> Result<(&'a str, Option<&'a Rule>), ()> {
    if finding.rule {
        return Err(());
    }
    let indexed = finding
        .rule_index
        .map(|index| driver.rules.get(index).ok_or(()))
        .transpose()?;
    let id = finding
        .rule_id
        .as_deref()
        .or_else(|| indexed.map(|rule| rule.id.as_str()))
        .filter(|id| !id.trim().is_empty())
        .ok_or(())?;
    if indexed.is_some_and(|rule| rule.id != id) {
        return Err(());
    }
    let rule = indexed.or_else(|| driver.rules.iter().find(|rule| rule.id == id));
    Ok((id, rule))
}

fn charge(remaining: &mut usize, count: usize) -> Result<(), ()> {
    *remaining = remaining.checked_sub(count).ok_or(())?;
    Ok(())
}

pub(super) fn decode(text: &str) -> Result<Option<Decoded>, ()> {
    if !text.trim_start().starts_with('{') {
        return Ok(None);
    }
    if text.len() > MAX_SCANNED_OUTPUT_BYTES {
        return Err(());
    }
    #[derive(Deserialize)]
    struct Family {
        version: Option<Value>,
        #[serde(default, deserialize_with = "present")]
        runs: bool,
        #[serde(rename = "$schema")]
        schema: Option<String>,
    }
    let family = serde_json::Deserializer::from_str(text)
        .into_iter::<Family>()
        .next()
        .ok_or(())?
        .map_err(|_| ())?;
    if !(family.runs && family.version.is_some())
        && !family
            .schema
            .as_deref()
            .is_some_and(|schema| schema.contains("sarif"))
    {
        return Ok(None);
    }
    // Reparse original bytes: a Value probe must never erase duplicate fields,
    // nor may an otherwise valid first object conceal a trailing document.
    let log: Log = serde_json::from_str(text).map_err(|_| ())?;
    if log.version != "2.1.0" || log.inline_external_properties {
        return Err(());
    }
    let runs = log.runs.ok_or(())?;
    let mut remaining = MAX_RECORDS;
    charge(&mut remaining, runs.len())?;
    let mut output = compiler::CompilerOutput::default();
    let mut execution = Execution {
        // Multiple runs may reuse relative URIs in different analysis roots.
        // They still supply diagnostics, but not cross-run repair proofs.
        complete: runs.len() == 1,
        ..Execution::default()
    };
    let mut diagnostics = BTreeMap::new();
    for run in runs {
        if run.tool.driver.name.trim().is_empty()
            || run.external_property_file_references
            || !run.policies.is_empty()
        {
            return Err(());
        }
        let driver = &run.tool.driver;
        charge(&mut remaining, driver.rules.len())?;
        let mut rule_ids = BTreeSet::new();
        for rule in &driver.rules {
            if rule.id.trim().is_empty() || !rule_ids.insert(rule.id.as_str()) {
                return Err(());
            }
            if rule
                .default_configuration
                .as_ref()
                .and_then(|config| config.level.as_deref())
                .is_some_and(|level| !valid_level(level))
            {
                return Err(());
            }
        }
        let mut run_complete = false;
        if let Some(invocations) = &run.invocations {
            charge(&mut remaining, invocations.len())?;
            run_complete = invocations.len() == 1;
            for invocation in invocations {
                if !invocation.rule_configuration_overrides.is_empty() {
                    return Err(());
                }
                let scope = invocation.scope.key(&mut remaining)?;
                run_complete &= invocation.execution_successful && scope.is_some();
                execution.invocation_scope = scope;
                for notification in invocation
                    .tool_execution_notifications
                    .iter()
                    .chain(&invocation.tool_configuration_notifications)
                {
                    charge(&mut remaining, 1)?;
                    let level = notification.level.as_deref().unwrap_or("warning");
                    if !valid_level(level) {
                        return Err(());
                    }
                    run_complete &= level != "error";
                }
            }
        }
        execution.complete &= run_complete;
        let Some(results) = run.results else {
            execution.complete = false;
            continue;
        };
        charge(&mut remaining, results.len())?;
        for finding in results {
            if finding.message.text.trim().is_empty()
                || finding.baseline_state.as_deref().is_some_and(|state| {
                    !matches!(state, "new" | "unchanged" | "updated" | "absent")
                })
            {
                return Err(());
            }
            charge(&mut remaining, finding.locations.len())?;
            charge(&mut remaining, finding.suppressions.len())?;
            let (id, rule) = rule_for(&finding, driver)?;
            let kind = finding.kind.as_deref().unwrap_or("fail");
            if !matches!(
                kind,
                "notApplicable" | "pass" | "fail" | "review" | "open" | "informational"
            ) {
                return Err(());
            }
            let level = if kind == "fail" {
                finding
                    .level
                    .as_deref()
                    .or_else(|| {
                        rule.and_then(|rule| rule.default_configuration.as_ref())
                            .and_then(|config| config.level.as_deref())
                    })
                    .unwrap_or("warning")
            } else {
                // Non-failure kinds do not inherit a rule's error severity.
                finding.level.as_deref().unwrap_or("none")
            };
            if !valid_level(level) || (kind != "fail" && level != "none") {
                return Err(());
            }
            // Suppression/baseline disappearance is not a verified repair.
            if !finding.suppressions.is_empty()
                || finding.baseline_state.as_deref() == Some("absent")
            {
                execution.complete = false;
                continue;
            }
            let failed = kind == "fail" && level == "error";
            let passed = kind == "pass";
            if !failed && !passed {
                // A warning or an inconclusive check must not contradict a
                // pass for the same subject. Withhold proof for the whole run
                // rather than inventing ordering semantics for result arrays.
                if matches!(kind, "fail" | "open" | "review") {
                    execution.complete = false;
                }
                continue;
            }
            let subject = subject_for(
                &finding,
                driver,
                id,
                run.original_uri_base_ids,
                run.column_kind.as_deref().unwrap_or("utf16CodeUnits"),
            );
            if failed {
                output.failed = true;
                let diagnostic = canonical(&driver.name, id, &finding.message.text);
                // Stable selection even when tools reorder result arrays. Keep
                // validating the entire report after the diagnostic cap fills.
                let key = diagnostic.layered_key().key;
                diagnostics
                    .entry(key)
                    .and_modify(|previous: &mut CanonicalDiagnostic| {
                        if diagnostic.message_template < previous.message_template {
                            *previous = diagnostic.clone();
                        }
                    })
                    .or_insert(diagnostic);
                if let Some(subject) = subject {
                    execution.failed.insert(subject);
                } else {
                    execution.unlocated_failure = true;
                }
            } else if let Some(subject) = subject {
                // Disabled rules and absent source locations cannot certify a
                // recheck even if a producer emits a permissive pass record.
                if rule
                    .and_then(|rule| rule.default_configuration.as_ref())
                    .is_none_or(|config| config.enabled != Some(false))
                {
                    execution.passed.insert(subject);
                }
            }
        }
    }
    output.diagnostics = diagnostics
        .into_values()
        .take(MAX_DIAGNOSTICS_PER_FAILURE)
        .collect();
    output.allows_completion = execution.complete && !output.failed && !execution.passed.is_empty();
    Ok(Some(Decoded { output, execution }))
}

fn subject_for(
    finding: &Finding,
    driver: &Driver,
    rule: &str,
    has_bases: bool,
    column_kind: &str,
) -> Option<Subject> {
    if !matches!(column_kind, "utf16CodeUnits" | "unicodeCodePoints") {
        return None;
    }
    // A group of locations need not mean the rule was evaluated independently
    // at each location. Do not turn one group-level pass into several proofs.
    let [location] = finding.locations.as_slice() else {
        return None;
    };
    let physical = location.physical_location.as_ref()?;
    let artifact = &physical.artifact_location;
    let uri = artifact
        .uri
        .as_deref()
        .filter(|uri| !uri.trim().is_empty())?;
    if has_bases || artifact.uri_base_id || artifact.index {
        return None;
    }
    let region = physical.region.as_ref()?;
    if [
        region.start_line,
        region.end_line,
        region.start_column,
        region.end_column,
    ]
    .into_iter()
    .flatten()
    .any(|position| position == 0)
        || region
            .start_line
            .zip(region.end_line)
            .is_some_and(|(start, end)| start > end)
        || (region.start_line == region.end_line
            && region
                .start_column
                .zip(region.end_column)
                .is_some_and(|(start, end)| start > end))
    {
        return None;
    }
    // Bind every supplied coordinate, not just one convenient representation.
    // Conflicting secondary coordinates must never alias a verified subject.
    let mut complete = false;
    for (start, length) in [
        (region.byte_offset, region.byte_length),
        (region.char_offset, region.char_length),
    ] {
        match (start, length) {
            (Some(start), Some(length)) => {
                start.checked_add(length)?;
                complete = true;
            }
            (None, None) => {}
            _ => return None,
        }
    }
    match (region.start_line, region.end_line) {
        (Some(_), Some(_)) => complete = true,
        (None, None) if region.start_column.is_none() && region.end_column.is_none() => {}
        _ => return None,
    }
    if !complete {
        return None;
    }
    let coordinates = format!(
        "{:?}",
        [
            region.start_line,
            region.start_column,
            region.end_line,
            region.end_column,
            region.char_offset,
            region.char_length,
            region.byte_offset,
            region.byte_length,
        ]
    );
    Some(commitment(
        b"ee.sarif.subject.v1\0",
        &[
            &driver.name,
            driver.version.as_deref().unwrap_or(""),
            driver.semantic_version.as_deref().unwrap_or(""),
            rule,
            uri,
            column_kind,
            &coordinates,
        ],
    ))
}

#[cfg(test)]
#[path = "cass_error_recall_sarif_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "cass_error_recall_sarif_context_tests.rs"]
mod context_tests;
