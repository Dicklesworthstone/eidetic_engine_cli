//! bd-1n0np.23.3 - ingestion-security contract for external text in the
//! dueling-wizards initiative.
//!
//! The manifest is a planning gate. It ensures docs-bootstrap, error-log, and
//! sandbox ingestion work cannot be added without redaction, prompt-injection
//! screening, quarantine-not-store behavior, and regression corpus coverage.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const MANIFEST_REL: &str = "tests/fixtures/contracts/dueling_wizards_ingestion_security.json";
const DOC_REL: &str = "docs/agent-ux/dueling-wizards-ingestion-security.md";
const POLICY_SOURCE_REL: &str = "src/policy/mod.rs";
const CURATE_SOURCE_REL: &str = "src/curate/mod.rs";
const OUTCOME_SOURCE_REL: &str = "src/core/outcome.rs";

const REQUIRED_SURFACES: &[&str] = &["docs_bootstrap", "error_log_diagnosis", "sandbox_import"];

const REQUIRED_PIPELINE: &[&str] = &[
    "source_classification",
    "secret_redaction",
    "prompt_injection_guard",
    "quarantine_not_store",
    "audit_event",
    "regression_corpus",
];

const REQUIRED_REGRESSION_PAYLOADS: &[&str] = &[
    "role_markup",
    "ignore_previous_instructions",
    "destructive_command_coercion",
    "secret_like_token",
    "mixed_benign_and_malicious",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_text(rel: &str) -> Result<String, String> {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).map_err(|error| format!("read {rel}: {error}"))
}

fn read_json(rel: &str) -> Result<Value, String> {
    let text = read_text(rel)?;
    serde_json::from_str(&text).map_err(|error| format!("parse {rel}: {error}"))
}

fn string_field<'a>(value: &'a Value, pointer: &str, context: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{context}: missing string field {pointer}"))
}

fn bool_field(value: &Value, pointer: &str, context: &str) -> Result<bool, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{context}: missing bool field {pointer}"))
}

fn array_field<'a>(
    value: &'a Value,
    pointer: &str,
    context: &str,
) -> Result<&'a Vec<Value>, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{context}: missing array field {pointer}"))
}

fn string_set(values: &[Value], context: &str) -> Result<BTreeSet<String>, String> {
    let mut out = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let text = value
            .as_str()
            .ok_or_else(|| format!("{context}[{index}] must be a string"))?;
        if text.trim().is_empty() {
            return Err(format!("{context}[{index}] must not be empty"));
        }
        out.insert(text.to_owned());
    }
    Ok(out)
}

fn string_vec(values: &[Value], context: &str) -> Result<Vec<String>, String> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let text = value
                .as_str()
                .ok_or_else(|| format!("{context}[{index}] must be a string"))?;
            if text.trim().is_empty() {
                return Err(format!("{context}[{index}] must not be empty"));
            }
            Ok(text.to_owned())
        })
        .collect()
}

fn required_set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn required_vec(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn ingestion_manifest_identity_and_policy_are_stable() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    if string_field(&manifest, "/schema", MANIFEST_REL)?
        != "ee.dueling_wizards.ingestion_security.v1"
    {
        return Err(format!(
            "{MANIFEST_REL}: schema must be ee.dueling_wizards.ingestion_security.v1"
        ));
    }
    if string_field(&manifest, "/initiativeBead", MANIFEST_REL)? != "bd-1n0np" {
        return Err("ingestion-security manifest must identify initiativeBead bd-1n0np".to_owned());
    }
    if string_field(&manifest, "/gateBead", MANIFEST_REL)? != "bd-1n0np.23.3" {
        return Err("ingestion-security manifest must identify gateBead bd-1n0np.23.3".to_owned());
    }
    if string_field(&manifest, "/doc", MANIFEST_REL)? != DOC_REL {
        return Err("ingestion-security manifest must point at its doc".to_owned());
    }
    if string_field(&manifest, "/policySource", MANIFEST_REL)? != POLICY_SOURCE_REL {
        return Err("ingestion-security manifest must point at src/policy/mod.rs".to_owned());
    }
    if string_field(&manifest, "/policy/externalTextDefault", MANIFEST_REL)?
        != "untrusted_until_guarded"
    {
        return Err("externalTextDefault must stay untrusted_until_guarded".to_owned());
    }
    if string_field(&manifest, "/policy/rawExternalTextStorage", MANIFEST_REL)?
        != "forbidden_by_default"
    {
        return Err("rawExternalTextStorage must stay forbidden_by_default".to_owned());
    }
    if string_field(&manifest, "/policy/flaggedInputBehavior", MANIFEST_REL)?
        != "quarantine_not_store"
    {
        return Err("flaggedInputBehavior must stay quarantine_not_store".to_owned());
    }
    if !bool_field(&manifest, "/policy/auditEventRequired", MANIFEST_REL)? {
        return Err("auditEventRequired must stay true".to_owned());
    }
    if !bool_field(
        &manifest,
        "/policy/rchProofRequiredForRustTests",
        MANIFEST_REL,
    )? {
        return Err("Rust proof must stay RCH-only".to_owned());
    }
    if string_field(&manifest, "/policy/localCargoProof", MANIFEST_REL)? != "invalid" {
        return Err("local Cargo proof must stay invalid".to_owned());
    }
    Ok(())
}

#[test]
fn covered_surfaces_require_the_full_guard_pipeline() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let required_pipeline = required_set(REQUIRED_PIPELINE);
    let required_payloads = required_set(REQUIRED_REGRESSION_PAYLOADS);
    let top_level_pipeline = string_set(
        array_field(&manifest, "/requiredPipeline", MANIFEST_REL)?,
        "/requiredPipeline",
    )?;
    let top_level_payloads = string_set(
        array_field(&manifest, "/regressionPayloadClasses", MANIFEST_REL)?,
        "/regressionPayloadClasses",
    )?;

    if top_level_pipeline != required_pipeline {
        return Err(format!(
            "top-level requiredPipeline drifted: expected {required_pipeline:?}, got {top_level_pipeline:?}"
        ));
    }
    if top_level_payloads != required_payloads {
        return Err(format!(
            "top-level regressionPayloadClasses drifted: expected {required_payloads:?}, got {top_level_payloads:?}"
        ));
    }

    let mut surfaces = BTreeSet::new();
    for (index, surface) in array_field(&manifest, "/surfaces", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("surface[{index}]");
        let name = string_field(surface, "/surface", &context)?;
        if !surfaces.insert(name.to_owned()) {
            return Err(format!("duplicate ingestion surface {name}"));
        }
        if string_field(surface, "/ownerBead", &context)? != "bd-1n0np.23.3" {
            return Err(format!("{name}: ownerBead must be bd-1n0np.23.3"));
        }
        if !bool_field(surface, "/externalText", &context)? {
            return Err(format!("{name}: externalText must be true"));
        }
        if string_field(surface, "/redaction", &context)?
            != "crate::policy::redact_secret_like_content"
        {
            return Err(format!("{name}: redaction must use the policy redactor"));
        }
        if string_field(surface, "/promptInjectionGuard", &context)?
            != "crate::policy::detect_instruction_like_content"
        {
            return Err(format!(
                "{name}: promptInjectionGuard must use the policy detector"
            ));
        }
        if string_field(surface, "/flaggedBehavior", &context)? != "quarantine_not_store" {
            return Err(format!(
                "{name}: flaggedBehavior must be quarantine_not_store"
            ));
        }
        if string_field(surface, "/rawStorage", &context)? != "forbidden" {
            return Err(format!("{name}: rawStorage must be forbidden"));
        }
        if string_field(surface, "/sourceClassifier", &context)?
            .trim()
            .is_empty()
        {
            return Err(format!("{name}: sourceClassifier must not be empty"));
        }
        if string_field(surface, "/auditEvent", &context)?
            .trim()
            .is_empty()
        {
            return Err(format!("{name}: auditEvent must not be empty"));
        }

        let pipeline = string_set(
            array_field(surface, "/requiredPipeline", &context)?,
            &format!("{name}.requiredPipeline"),
        )?;
        if pipeline != required_pipeline {
            return Err(format!("{name}: requiredPipeline must carry every guard"));
        }
        let payloads = string_set(
            array_field(surface, "/requiredRegressionPayloadClasses", &context)?,
            &format!("{name}.requiredRegressionPayloadClasses"),
        )?;
        if payloads != required_payloads {
            return Err(format!(
                "{name}: requiredRegressionPayloadClasses must carry every payload class"
            ));
        }
    }

    let expected_surfaces = required_set(REQUIRED_SURFACES);
    if surfaces != expected_surfaces {
        return Err(format!(
            "ingestion surface set drifted: missing={:?}, extra={:?}",
            expected_surfaces.difference(&surfaces).collect::<Vec<_>>(),
            surfaces.difference(&expected_surfaces).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn guard_order_matrix_keeps_external_text_away_from_storage_until_guarded() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let required_order = required_vec(REQUIRED_PIPELINE);
    let top_level_order = string_vec(
        array_field(&manifest, "/requiredPipeline", MANIFEST_REL)?,
        "/requiredPipeline",
    )?;
    if top_level_order != required_order {
        return Err(format!(
            "top-level requiredPipeline order drifted: expected {required_order:?}, got {top_level_order:?}"
        ));
    }

    let mut surface_pipelines = BTreeMap::new();
    for (index, surface) in array_field(&manifest, "/surfaces", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("surface[{index}]");
        let name = string_field(surface, "/surface", &context)?;
        let pipeline = string_vec(
            array_field(surface, "/requiredPipeline", &context)?,
            &format!("{name}.requiredPipeline"),
        )?;
        if pipeline != required_order {
            return Err(format!("{name}: requiredPipeline order drifted"));
        }
        surface_pipelines.insert(name.to_owned(), pipeline);
    }

    let expected_surfaces = required_set(REQUIRED_SURFACES);
    let mut matrix_surfaces = BTreeSet::new();
    for (index, row) in array_field(&manifest, "/guardOrderMatrix", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("guardOrderMatrix[{index}]");
        let surface = string_field(row, "/surface", &context)?;
        if !matrix_surfaces.insert(surface.to_owned()) {
            return Err(format!("{context}: duplicate surface {surface}"));
        }
        let surface_pipeline = surface_pipelines
            .get(surface)
            .ok_or_else(|| format!("{context}: unknown surface {surface}"))?;
        let matrix_pipeline = string_vec(
            array_field(row, "/orderedPipeline", &context)?,
            &format!("{surface}.orderedPipeline"),
        )?;
        if &matrix_pipeline != surface_pipeline {
            return Err(format!(
                "{surface}: guardOrderMatrix must mirror the surface requiredPipeline"
            ));
        }
        if matrix_pipeline != required_order {
            return Err(format!(
                "{surface}: guardOrderMatrix must mirror the top-level order"
            ));
        }
        if !bool_field(row, "/redactionBeforePromptGuard", &context)? {
            return Err(format!(
                "{surface}: secret redaction must precede prompt-injection guard"
            ));
        }
        if !bool_field(row, "/promptGuardBeforeStorage", &context)? {
            return Err(format!(
                "{surface}: prompt-injection guard must precede storage disposition"
            ));
        }
        if string_field(row, "/rawStorageBeforeGuards", &context)? != "forbidden" {
            return Err(format!(
                "{surface}: raw storage before guards must stay forbidden"
            ));
        }
        if string_field(row, "/flaggedStorageDisposition", &context)? != "quarantine_not_store" {
            return Err(format!(
                "{surface}: flagged external text must quarantine-not-store"
            ));
        }
        if !bool_field(row, "/auditAfterDisposition", &context)? {
            return Err(format!(
                "{surface}: audit must happen after the storage disposition is known"
            ));
        }
    }

    if matrix_surfaces != expected_surfaces {
        return Err(format!(
            "guardOrderMatrix surface set drifted: missing={:?}, extra={:?}",
            expected_surfaces
                .difference(&matrix_surfaces)
                .collect::<Vec<_>>(),
            matrix_surfaces
                .difference(&expected_surfaces)
                .collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn regression_payload_examples_cover_all_classes_and_expect_quarantine() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let required_payloads = required_set(REQUIRED_REGRESSION_PAYLOADS);
    let required_surfaces = required_set(REQUIRED_SURFACES);
    let examples = array_field(&manifest, "/regressionPayloadExamples", MANIFEST_REL)?;
    let mut audit_by_surface = BTreeMap::new();

    for (index, surface) in array_field(&manifest, "/surfaces", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("surface[{index}]");
        let name = string_field(surface, "/surface", &context)?;
        let audit_event = string_field(surface, "/auditEvent", &context)?;
        audit_by_surface.insert(name.to_owned(), audit_event.to_owned());
    }

    let mut seen_payloads = BTreeSet::new();
    for (index, example) in examples.iter().enumerate() {
        let context = format!("regressionPayloadExamples[{index}]");
        let payload_class = string_field(example, "/payloadClass", &context)?;
        if !required_payloads.contains(payload_class) {
            return Err(format!(
                "{context}: unexpected payloadClass {payload_class}"
            ));
        }
        if !seen_payloads.insert(payload_class.to_owned()) {
            return Err(format!("{context}: duplicate payloadClass {payload_class}"));
        }

        let sample_name = string_field(example, "/sampleName", &context)?;
        if sample_name.trim().is_empty() {
            return Err(format!("{context}: sampleName must not be empty"));
        }

        let source_surface = string_field(example, "/sourceSurface", &context)?;
        if !required_surfaces.contains(source_surface) {
            return Err(format!(
                "{context}: unexpected sourceSurface {source_surface}"
            ));
        }
        if !bool_field(example, "/mustRunPromptInjectionGuard", &context)? {
            return Err(format!(
                "{context}: mustRunPromptInjectionGuard must stay true"
            ));
        }
        if !bool_field(example, "/mustQuarantineWhenFlagged", &context)? {
            return Err(format!(
                "{context}: mustQuarantineWhenFlagged must stay true"
            ));
        }
        if string_field(example, "/rawStorage", &context)? != "forbidden" {
            return Err(format!("{context}: rawStorage must be forbidden"));
        }

        let expected_audit_event = string_field(example, "/expectedAuditEvent", &context)?;
        let surface_audit_event = audit_by_surface.get(source_surface).ok_or_else(|| {
            format!("{context}: sourceSurface {source_surface} has no audit event")
        })?;
        if expected_audit_event != surface_audit_event {
            return Err(format!(
                "{context}: expectedAuditEvent must match source surface audit event"
            ));
        }
        if !expected_audit_event.ends_with("_ingestion_security") {
            return Err(format!(
                "{context}: expectedAuditEvent must be an ingestion-security event"
            ));
        }

        let must_redact_secrets = bool_field(example, "/mustRedactSecrets", &context)?;
        let should_redact_secrets = matches!(
            payload_class,
            "secret_like_token" | "mixed_benign_and_malicious"
        );
        if should_redact_secrets && !must_redact_secrets {
            return Err(format!(
                "{context}: secret-bearing payload class must require secret redaction"
            ));
        }
    }

    if seen_payloads != required_payloads {
        return Err(format!(
            "regressionPayloadExamples drifted: expected {required_payloads:?}, got {seen_payloads:?}"
        ));
    }
    Ok(())
}

#[test]
fn manifest_source_anchors_still_exist() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    for (source_key, source_rel) in [
        ("policy", POLICY_SOURCE_REL),
        ("curate", CURATE_SOURCE_REL),
        ("outcome", OUTCOME_SOURCE_REL),
    ] {
        let source = read_text(source_rel)?;
        for (index, anchor) in array_field(
            &manifest,
            &format!("/sourceAnchors/{source_key}"),
            MANIFEST_REL,
        )?
        .iter()
        .enumerate()
        {
            let needle = anchor
                .as_str()
                .ok_or_else(|| format!("{source_key}.sourceAnchors[{index}] must be a string"))?;
            if !source.contains(needle) {
                return Err(format!(
                    "{source_rel} must still contain ingestion-security anchor {needle:?}"
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn ingestion_security_doc_names_manifest_sources_and_surfaces() -> TestResult {
    let doc = read_text(DOC_REL)?;
    for needle in [
        MANIFEST_REL,
        "bd-1n0np.23.3",
        POLICY_SOURCE_REL,
        CURATE_SOURCE_REL,
        OUTCOME_SOURCE_REL,
        "docs_bootstrap",
        "error_log_diagnosis",
        "sandbox_import",
        "detect_instruction_like_content",
        "redact_secret_like_content",
        "quarantine_not_store",
        "guardOrderMatrix",
        "redactionBeforePromptGuard",
        "promptGuardBeforeStorage",
        "rawStorageBeforeGuards",
        "regressionPayloadExamples",
        "chat_role_block",
        "api_key_literal",
        "build_error_plus_instruction",
        "Local Cargo fallback is not valid proof",
    ] {
        if !doc.contains(needle) {
            return Err(format!("{DOC_REL} must mention {needle:?}"));
        }
    }
    Ok(())
}
