//! bd-1n0np.15.3 - new-surface contract checklist for the dueling-wizards
//! initiative.
//!
//! The manifest is the single machine-readable list of planned initiative
//! surfaces. Planned entries do not fail CI for missing future code, but every
//! entry must carry the full checklist. Once an entry is marked `implemented`,
//! every listed artifact path must exist.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const MANIFEST_REL: &str = "tests/fixtures/contracts/dueling_wizards_surface_contract.json";
const DOC_REL: &str = "docs/agent-ux/dueling-wizards-surface-contract.md";

const REQUIRED_ARTIFACTS: &[&str] = &[
    "capabilities",
    "agent_docs",
    "robot_docs",
    "help_prelude",
    "json_schema",
    "schema_drift",
    "failure_mode_fixture",
    "degraded_taxonomy",
    "env_registry",
    "determinism",
    "e2e_harness",
];

const REQUIRED_SURFACE_IDS: &[&str] = &[
    "why_not",
    "evidence_harvester",
    "anchors_freshness",
    "error_recall",
    "lod_packing",
    "gap_honesty",
    "contradiction_resolution",
    "read_fence",
    "provenance_reverification",
    "house_rules",
    "docs_bootstrap",
    "typed_memory_kinds",
    "feedback_learning",
    "rejected_ideas",
    "harness_contract",
    "memory_sentinels",
    "task_lens",
    "trauma_guard_loop",
    "causal_ppr",
    "bridge_exemption",
    "memory_sandbox",
    "attestation_bundles",
    "cross_cutting_foundations",
];

const REQUIRED_ANCHOR_KINDS: &[&str] = &[
    "path",
    "symbol",
    "command",
    "env_var",
    "schema",
    "degraded_code",
    "dependency",
    "config_key",
];

const REQUIRED_ANCHOR_EXTRACTION_SOURCES: &[&str] = &[
    "explicit",
    "remember",
    "cass_import",
    "curate_apply",
    "index_rebuild",
];

const MIN_MUST_COVERAGE_MILLI: u64 = 950;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_json(rel: &str) -> Result<Value, String> {
    let path = repo_root().join(rel);
    let text = fs::read_to_string(&path).map_err(|error| format!("read {rel}: {error}"))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {rel}: {error}"))
}

fn string_field<'a>(value: &'a Value, pointer: &str, context: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{context}: missing string field {pointer}"))
}

fn u64_field(value: &Value, pointer: &str, context: &str) -> Result<u64, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{context}: missing integer field {pointer}"))
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

fn surface_by_id<'a>(manifest: &'a Value, id: &str) -> Result<&'a Value, String> {
    for surface in array_field(manifest, "/surfaces", MANIFEST_REL)? {
        if surface
            .pointer("/id")
            .and_then(Value::as_str)
            .is_some_and(|surface_id| surface_id == id)
        {
            return Ok(surface);
        }
    }
    Err(format!("{MANIFEST_REL}: missing surface id {id}"))
}

#[test]
fn manifest_has_stable_identity_and_required_surfaces() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let schema = string_field(&manifest, "/schema", MANIFEST_REL)?;
    if schema != "ee.dueling_wizards.surface_contract.v1" {
        return Err(format!(
            "{MANIFEST_REL}: schema must be ee.dueling_wizards.surface_contract.v1, got {schema}"
        ));
    }
    if string_field(&manifest, "/initiativeBead", MANIFEST_REL)? != "bd-1n0np" {
        return Err("manifest must identify initiativeBead bd-1n0np".to_owned());
    }
    if string_field(&manifest, "/gateBead", MANIFEST_REL)? != "bd-1n0np.15.3" {
        return Err("manifest must identify gateBead bd-1n0np.15.3".to_owned());
    }

    let required = string_set(
        array_field(&manifest, "/requiredArtifacts", MANIFEST_REL)?,
        "/requiredArtifacts",
    )?;
    let expected_required = REQUIRED_ARTIFACTS
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<BTreeSet<_>>();
    if required != expected_required {
        return Err(format!(
            "manifest requiredArtifacts drifted: expected {expected_required:?}, got {required:?}"
        ));
    }

    let surfaces = array_field(&manifest, "/surfaces", MANIFEST_REL)?;
    let mut ids = BTreeSet::new();
    for (index, surface) in surfaces.iter().enumerate() {
        let context = format!("surface[{index}]");
        let id = string_field(surface, "/id", &context)?;
        if !ids.insert(id.to_owned()) {
            return Err(format!("duplicate surface id {id}"));
        }
    }

    let expected_ids = REQUIRED_SURFACE_IDS
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<BTreeSet<_>>();
    if ids != expected_ids {
        return Err(format!(
            "surface id set drifted from bd-1n0np top-level features: missing={:?}, extra={:?}",
            expected_ids.difference(&ids).collect::<Vec<_>>(),
            ids.difference(&expected_ids).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn coverage_matrix_proves_required_artifact_conformance() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let expected_required = REQUIRED_ARTIFACTS
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<BTreeSet<_>>();
    let mut matrix_sections = BTreeSet::new();

    for (index, row) in array_field(&manifest, "/coverageMatrix", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("coverageMatrix[{index}]");
        let section = string_field(row, "/specSection", &context)?;
        if !matrix_sections.insert(section.to_owned()) {
            return Err(format!("duplicate coverage matrix section {section}"));
        }

        let must = u64_field(row, "/mustClauses", &context)?;
        let should = u64_field(row, "/shouldClauses", &context)?;
        let tested = u64_field(row, "/tested", &context)?;
        let passing = u64_field(row, "/passing", &context)?;
        let divergent = u64_field(row, "/divergent", &context)?;
        let score_milli = u64_field(row, "/scoreMilli", &context)?;
        let status = string_field(row, "/status", &context)?;

        if must == 0 {
            return Err(format!("{section}: mustClauses must be non-zero"));
        }
        if tested < must + should {
            return Err(format!(
                "{section}: tested={tested} must cover must+should={}",
                must + should
            ));
        }
        if passing < must {
            return Err(format!(
                "{section}: passing={passing} must cover all MUST clauses={must}"
            ));
        }
        if divergent != 0 {
            return Err(format!("{section}: divergent clauses must stay at 0"));
        }
        let computed_score = passing.saturating_mul(1000) / must;
        if score_milli != computed_score {
            return Err(format!(
                "{section}: scoreMilli={score_milli} must match computed MUST score={computed_score}"
            ));
        }
        if score_milli < MIN_MUST_COVERAGE_MILLI {
            return Err(format!(
                "{section}: MUST coverage {score_milli} is below {MIN_MUST_COVERAGE_MILLI}"
            ));
        }
        if status != "conformant" {
            return Err(format!("{section}: status must be conformant"));
        }
    }

    if matrix_sections != expected_required {
        return Err(format!(
            "coverageMatrix drifted from requiredArtifacts: missing={:?}, extra={:?}",
            expected_required
                .difference(&matrix_sections)
                .collect::<Vec<_>>(),
            matrix_sections
                .difference(&expected_required)
                .collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn surface_coverage_matrix_accounts_for_every_surface_checklist() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let expected_ids = REQUIRED_SURFACE_IDS
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<BTreeSet<_>>();
    let expected_required = REQUIRED_ARTIFACTS
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<BTreeSet<_>>();
    let required_count = REQUIRED_ARTIFACTS.len() as u64;

    let mut surfaces = BTreeMap::new();
    for (index, surface) in array_field(&manifest, "/surfaces", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("surface[{index}]");
        let id = string_field(surface, "/id", &context)?;
        if surfaces.insert(id.to_owned(), surface).is_some() {
            return Err(format!("duplicate surface id {id}"));
        }
    }

    let mut matrix_ids = BTreeSet::new();
    for (index, row) in array_field(&manifest, "/surfaceCoverageMatrix", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("surfaceCoverageMatrix[{index}]");
        let surface_id = string_field(row, "/surface", &context)?;
        if !matrix_ids.insert(surface_id.to_owned()) {
            return Err(format!("duplicate surfaceCoverageMatrix row {surface_id}"));
        }

        let surface = surfaces
            .get(surface_id)
            .ok_or_else(|| format!("{context}: missing manifest surface {surface_id}"))?;
        let surface_context = format!("surface[{surface_id}]");
        let surface_bead = string_field(surface, "/bead", &surface_context)?;
        let surface_status = string_field(surface, "/status", &surface_context)?;
        let expected_runtime_status = match surface_status {
            "planned" => "planned_contract_only",
            "in_progress" => "implemented_artifacts_checked_when_present",
            "implemented" => "implemented_artifacts_required",
            other => {
                return Err(format!("{surface_id}: unsupported manifest status {other}"));
            }
        };

        if string_field(row, "/bead", &context)? != surface_bead {
            return Err(format!(
                "{surface_id}: matrix bead must mirror manifest bead {surface_bead}"
            ));
        }
        if string_field(row, "/status", &context)? != surface_status {
            return Err(format!(
                "{surface_id}: matrix status must mirror manifest status {surface_status}"
            ));
        }
        if string_field(row, "/complianceStatus", &context)? != "declared_conformant" {
            return Err(format!(
                "{surface_id}: complianceStatus must be declared_conformant"
            ));
        }
        if string_field(row, "/runtimeEvidenceStatus", &context)? != expected_runtime_status {
            return Err(format!(
                "{surface_id}: runtimeEvidenceStatus must be {expected_runtime_status}"
            ));
        }

        let surface_required = string_set(
            array_field(surface, "/requiredArtifacts", &surface_context)?,
            &format!("{surface_id}.requiredArtifacts"),
        )?;
        if surface_required != expected_required {
            return Err(format!(
                "{surface_id}: requiredArtifacts must carry the full checklist"
            ));
        }

        let implemented_count = array_field(
            surface,
            "/implementedArtifacts",
            &format!("{surface_id}.implementedArtifacts"),
        )?
        .len() as u64;

        for (pointer, expected) in [
            ("/requiredArtifactCount", required_count),
            (
                "/declaredRequiredArtifactCount",
                surface_required.len() as u64,
            ),
            ("/implementedArtifactCount", implemented_count),
            ("/mustClauses", required_count),
            ("/tested", required_count),
            ("/passing", required_count),
            ("/divergent", 0),
        ] {
            let actual = u64_field(row, pointer, &context)?;
            if actual != expected {
                return Err(format!(
                    "{surface_id}: {pointer} must be {expected}, got {actual}"
                ));
            }
        }

        let score_milli = u64_field(row, "/scoreMilli", &context)?;
        let computed_score = u64_field(row, "/passing", &context)?.saturating_mul(1000)
            / u64_field(row, "/mustClauses", &context)?;
        if score_milli != computed_score {
            return Err(format!(
                "{surface_id}: scoreMilli={score_milli} must match computed MUST score={computed_score}"
            ));
        }
        if score_milli < MIN_MUST_COVERAGE_MILLI {
            return Err(format!(
                "{surface_id}: MUST coverage {score_milli} is below {MIN_MUST_COVERAGE_MILLI}"
            ));
        }
    }

    if matrix_ids != expected_ids {
        return Err(format!(
            "surfaceCoverageMatrix drifted from required surfaces: missing={:?}, extra={:?}",
            expected_ids.difference(&matrix_ids).collect::<Vec<_>>(),
            matrix_ids.difference(&expected_ids).collect::<Vec<_>>()
        ));
    }

    Ok(())
}

#[test]
fn every_surface_declares_full_checklist_and_explicit_empty_lists() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let expected_required = REQUIRED_ARTIFACTS
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<BTreeSet<_>>();

    for (index, surface) in array_field(&manifest, "/surfaces", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("surface[{index}]");
        let id = string_field(surface, "/id", &context)?;
        let bead = string_field(surface, "/bead", &context)?;
        if !bead.starts_with("bd-1n0np.") {
            return Err(format!("{id}: bead {bead} must belong to bd-1n0np"));
        }
        let status = string_field(surface, "/status", &context)?;
        if !matches!(status, "planned" | "in_progress" | "implemented") {
            return Err(format!(
                "{id}: status {status} is not a supported manifest status"
            ));
        }
        let title = string_field(surface, "/title", &context)?;
        if title.trim().is_empty() {
            return Err(format!("{id}: title must not be empty"));
        }

        for pointer in [
            "/plannedCommands",
            "/schemas",
            "/degradedCodes",
            "/envVars",
            "/implementedArtifacts",
        ] {
            let _ = array_field(surface, pointer, &format!("{id}{pointer}"))?;
        }

        let required = string_set(
            array_field(surface, "/requiredArtifacts", &context)?,
            &format!("{id}.requiredArtifacts"),
        )?;
        if required != expected_required {
            return Err(format!(
                "{id}: requiredArtifacts must carry the full checklist; expected {expected_required:?}, got {required:?}"
            ));
        }
    }
    Ok(())
}

#[test]
fn implemented_and_in_progress_artifact_paths_exist() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    for (index, surface) in array_field(&manifest, "/surfaces", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("surface[{index}]");
        let id = string_field(surface, "/id", &context)?;
        let status = string_field(surface, "/status", &context)?;
        let artifacts = array_field(surface, "/implementedArtifacts", &context)?;

        if status == "implemented" && artifacts.is_empty() {
            return Err(format!(
                "{id}: implemented surfaces must list concrete implementedArtifacts"
            ));
        }

        for (artifact_index, artifact) in artifacts.iter().enumerate() {
            let rel = artifact.as_str().ok_or_else(|| {
                format!("{id}.implementedArtifacts[{artifact_index}] must be a string")
            })?;
            let path = repo_root().join(rel);
            if !path.exists() {
                return Err(format!(
                    "{id}: implemented artifact path {rel} does not exist"
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn anchor_surface_declares_precision_first_contract() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let surface = surface_by_id(&manifest, "anchors_freshness")?;
    let contract = surface
        .pointer("/anchorContract")
        .ok_or_else(|| "anchors_freshness must declare anchorContract".to_owned())?;

    let allowed_kinds = string_set(
        array_field(
            contract,
            "/allowedKinds",
            "anchors_freshness.anchorContract",
        )?,
        "anchors_freshness.anchorContract.allowedKinds",
    )?;
    let expected_kinds = REQUIRED_ANCHOR_KINDS
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<BTreeSet<_>>();
    if allowed_kinds != expected_kinds {
        return Err(format!(
            "anchors_freshness allowedKinds drifted: expected {expected_kinds:?}, got {allowed_kinds:?}"
        ));
    }

    let extraction_sources = string_set(
        array_field(
            contract,
            "/extractionSources",
            "anchors_freshness.anchorContract",
        )?,
        "anchors_freshness.anchorContract.extractionSources",
    )?;
    let expected_sources = REQUIRED_ANCHOR_EXTRACTION_SOURCES
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<BTreeSet<_>>();
    if extraction_sources != expected_sources {
        return Err(format!(
            "anchors_freshness extractionSources drifted: expected {expected_sources:?}, got {extraction_sources:?}"
        ));
    }

    for (pointer, expected) in [
        ("/precisionPolicy", "precision_first_no_adversarial_prose"),
        (
            "/valueRedaction",
            "hash_or_redact_anchor_values_keep_kind_and_line",
        ),
        (
            "/freshnessPolicy",
            "rank_down_resolved_symbol_drift_never_tombstone",
        ),
        ("/missingAnchorBehavior", "degraded_not_silent"),
    ] {
        if string_field(contract, pointer, "anchors_freshness.anchorContract")? != expected {
            return Err(format!(
                "anchors_freshness.anchorContract{pointer} must be {expected}"
            ));
        }
    }

    let followup_commands = string_set(
        array_field(
            contract,
            "/followupCommands",
            "anchors_freshness.anchorContract",
        )?,
        "anchors_freshness.anchorContract.followupCommands",
    )?;
    for command in [
        "ee memory anchors <memory-id> --json",
        "ee impact <surface> --json",
        "ee pack <task> --surface <hint> --json",
    ] {
        if !followup_commands.contains(command) {
            return Err(format!(
                "anchors_freshness.anchorContract.followupCommands must include {command}"
            ));
        }
    }
    Ok(())
}

#[test]
fn checklist_doc_points_to_manifest_and_names_required_artifacts() -> TestResult {
    let path = repo_root().join(DOC_REL);
    let doc = fs::read_to_string(&path).map_err(|error| format!("read {DOC_REL}: {error}"))?;

    for needle in [
        MANIFEST_REL,
        "bd-1n0np.15.3",
        "Local Cargo fallback is not valid proof",
        "Coverage Matrix",
    ] {
        if !doc.contains(needle) {
            return Err(format!("{DOC_REL} must mention {needle:?}"));
        }
    }
    for artifact in REQUIRED_ARTIFACTS {
        if !doc.contains(artifact) {
            return Err(format!(
                "{DOC_REL} must document required artifact category {artifact}"
            ));
        }
    }
    for surface_id in REQUIRED_SURFACE_IDS {
        if !doc.contains(surface_id) {
            return Err(format!("{DOC_REL} must mention surface id {surface_id}"));
        }
    }
    for needle in [
        "anchorContract",
        "precision_first_no_adversarial_prose",
        "rank_down_resolved_symbol_drift_never_tombstone",
        "ee memory anchors <memory-id> --json",
    ] {
        if !doc.contains(needle) {
            return Err(format!("{DOC_REL} must mention {needle:?}"));
        }
    }
    Ok(())
}
