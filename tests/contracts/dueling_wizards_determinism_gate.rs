//! bd-1n0np.15.2 - dueling-wizards determinism gate extension.
//!
//! This static contract pins the planned JSON surfaces that must be added to
//! the determinism harness. It does not assert the future commands exist yet;
//! it prevents the gate vocabulary, repeated-run policy, and pack-hash rules
//! from drifting while implementation slices land.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const MANIFEST_REL: &str = "tests/fixtures/contracts/dueling_wizards_determinism_gate.json";
const DOC_REL: &str = "docs/agent-ux/dueling-wizards/determinism-gate.md";
const DETERMINISM_SH_REL: &str = "scripts/e2e_overhaul/determinism.sh";
const DETERMINISM_UNIT_REL: &str = "tests/determinism_unit.rs";
const SURFACE_CONTRACT_REL: &str = "docs/agent-ux/dueling-wizards/surface-contract.md";
const MIGRATION_REGISTRY_REL: &str =
    "tests/fixtures/contracts/dueling_wizards_migration_registry.json";

const REQUIRED_SURFACES: &[&str] = &[
    "why_not",
    "harvest",
    "calibration",
    "impact",
    "error_recall",
    "blind_spots",
    "conflict",
    "attest",
    "docs_bootstrap",
    "read_fence_consistency",
    "pack_lod",
    "feedback_roi",
];

const REQUIRED_ASSERTIONS: &[&str] = &[
    "byte_identical_json",
    "volatile_fields_explicit",
    "stable_ordering",
    "stderr_or_artifact_diagnostics",
];

const PACK_ASSERTIONS: &[&str] = &[
    "pack_hash_reproducible",
    "pack_hash_absence_is_failure_not_skip",
];

const MEMORY_ANCHOR_DETERMINISM_ASSERTIONS: &[&str] = &[
    "stable_anchor_value_hash",
    "stable_redacted_anchor_value",
    "stable_ordering",
    "generation_not_wall_clock",
    "raw_anchor_value_absent",
];

const REQUIRED_SURFACE_MUST_CLAUSES: u64 = 9;
const MIN_MUST_COVERAGE_MILLI: u64 = 950;

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

fn required_set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
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
    Err(format!("{MANIFEST_REL}: missing surface {id}"))
}

fn determinism_matrix_row_by_surface<'a>(
    manifest: &'a Value,
    id: &str,
) -> Result<&'a Value, String> {
    for row in array_field(manifest, "/determinismMatrix", MANIFEST_REL)? {
        if row
            .pointer("/surface")
            .and_then(Value::as_str)
            .is_some_and(|surface_id| surface_id == id)
        {
            return Ok(row);
        }
    }
    Err(format!(
        "{MANIFEST_REL}: missing determinismMatrix row for {id}"
    ))
}

fn memory_anchor_planned_shape() -> Result<Value, String> {
    let registry = read_json(MIGRATION_REGISTRY_REL)?;
    for allocation in array_field(&registry, "/allocations", MIGRATION_REGISTRY_REL)? {
        if allocation
            .pointer("/id")
            .and_then(Value::as_str)
            .is_some_and(|id| id == "memory_anchors")
        {
            return allocation
                .pointer("/plannedShape")
                .cloned()
                .ok_or_else(|| "memory_anchors allocation must declare plannedShape".to_owned());
        }
    }
    Err(format!(
        "{MIGRATION_REGISTRY_REL}: missing memory_anchors allocation"
    ))
}

#[test]
fn determinism_manifest_identity_and_policy_are_stable() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    if string_field(&manifest, "/schema", MANIFEST_REL)? != "ee.dueling_wizards.determinism_gate.v1"
    {
        return Err(format!(
            "{MANIFEST_REL}: schema must be ee.dueling_wizards.determinism_gate.v1"
        ));
    }
    if string_field(&manifest, "/initiativeBead", MANIFEST_REL)? != "bd-1n0np" {
        return Err("manifest must identify initiativeBead bd-1n0np".to_owned());
    }
    if string_field(&manifest, "/gateBead", MANIFEST_REL)? != "bd-1n0np.15.2" {
        return Err("manifest must identify gateBead bd-1n0np.15.2".to_owned());
    }
    for (pointer, expected) in [
        ("/doc", DOC_REL),
        ("/determinismHarness", DETERMINISM_SH_REL),
        ("/determinismUnit", DETERMINISM_UNIT_REL),
        ("/surfaceContract", SURFACE_CONTRACT_REL),
        ("/migrationRegistry", MIGRATION_REGISTRY_REL),
    ] {
        if string_field(&manifest, pointer, MANIFEST_REL)? != expected {
            return Err(format!("{pointer} must point at {expected}"));
        }
    }
    if string_field(&manifest, "/implementationState", MANIFEST_REL)? != "planned_contract" {
        return Err("implementationState must stay planned_contract".to_owned());
    }
    if u64_field(&manifest, "/policy/runCount", MANIFEST_REL)? != 3 {
        return Err("policy.runCount must stay 3".to_owned());
    }
    if string_field(&manifest, "/policy/canonicalization", MANIFEST_REL)?
        != "explicit_volatile_field_removal"
    {
        return Err("canonicalization must stay explicit_volatile_field_removal".to_owned());
    }
    for pointer in [
        "/policy/byteStableJsonRequired",
        "/policy/packHashReproRequiredWhenPackEmitted",
        "/policy/stdoutMachineOnly",
        "/policy/rchProofRequiredForRuntimeTests",
    ] {
        if !bool_field(&manifest, pointer, MANIFEST_REL)? {
            return Err(format!("{pointer} must stay true"));
        }
    }
    if string_field(&manifest, "/policy/localCargoProof", MANIFEST_REL)? != "invalid" {
        return Err("localCargoProof must stay invalid".to_owned());
    }
    Ok(())
}

#[test]
fn required_assertion_vocabularies_are_complete() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    for (pointer, expected) in [
        ("/requiredAssertions", REQUIRED_ASSERTIONS),
        ("/packAssertions", PACK_ASSERTIONS),
    ] {
        let actual = string_set(array_field(&manifest, pointer, MANIFEST_REL)?, pointer)?;
        let expected = required_set(expected);
        if actual != expected {
            return Err(format!(
                "{pointer} drifted: missing={:?}, extra={:?}",
                expected.difference(&actual).collect::<Vec<_>>(),
                actual.difference(&expected).collect::<Vec<_>>()
            ));
        }
    }
    Ok(())
}

#[test]
fn every_surface_declares_determinism_coverage_shape() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let expected_surfaces = required_set(REQUIRED_SURFACES);
    let expected_assertions = required_set(REQUIRED_ASSERTIONS);
    let pack_assertions = required_set(PACK_ASSERTIONS);
    let mut actual_surfaces = BTreeSet::new();

    for (index, surface) in array_field(&manifest, "/surfaces", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("surfaces[{index}]");
        let id = string_field(surface, "/id", &context)?;
        if !actual_surfaces.insert(id.to_owned()) {
            return Err(format!("duplicate surface id {id}"));
        }
        let status = string_field(surface, "/status", &context)?;
        if !matches!(status, "planned" | "in_progress" | "implemented") {
            return Err(format!("{id}: unsupported status {status}"));
        }
        if string_field(surface, "/command", &context)?
            .trim()
            .is_empty()
        {
            return Err(format!("{id}: command must not be empty"));
        }
        let owner_beads = string_set(
            array_field(surface, "/ownerBeads", &context)?,
            &format!("{id}.ownerBeads"),
        )?;
        if !owner_beads.contains("bd-1n0np.15.2") {
            return Err(format!("{id}: ownerBeads must include bd-1n0np.15.2"));
        }
        let schema_refs = string_set(
            array_field(surface, "/schemaRefs", &context)?,
            &format!("{id}.schemaRefs"),
        )?;
        if schema_refs.is_empty() {
            return Err(format!("{id}: schemaRefs must be explicit"));
        }
        let assertions = string_set(
            array_field(surface, "/assertions", &context)?,
            &format!("{id}.assertions"),
        )?;
        if assertions != expected_assertions {
            return Err(format!("{id}: assertions must carry the shared set"));
        }
        let surface_pack_assertions = string_set(
            array_field(surface, "/packAssertions", &context)?,
            &format!("{id}.packAssertions"),
        )?;
        if matches!(id, "read_fence_consistency" | "pack_lod") {
            if surface_pack_assertions != pack_assertions {
                return Err(format!("{id}: must carry pack-hash determinism assertions"));
            }
        } else if !surface_pack_assertions.is_empty() {
            return Err(format!(
                "{id}: non-pack surface should not list packAssertions"
            ));
        }
    }

    if actual_surfaces != expected_surfaces {
        return Err(format!(
            "surface set drifted: missing={:?}, extra={:?}",
            expected_surfaces
                .difference(&actual_surfaces)
                .collect::<Vec<_>>(),
            actual_surfaces
                .difference(&expected_surfaces)
                .collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn determinism_matrix_covers_every_surface_and_mirrors_policy() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let expected_surfaces = required_set(REQUIRED_SURFACES);
    let expected_assertions = required_set(REQUIRED_ASSERTIONS);
    let pack_assertions = required_set(PACK_ASSERTIONS);
    let policy_run_count = u64_field(&manifest, "/policy/runCount", MANIFEST_REL)?;
    let policy_canonicalization =
        string_field(&manifest, "/policy/canonicalization", MANIFEST_REL)?.to_owned();
    let policy_stdout = bool_field(&manifest, "/policy/stdoutMachineOnly", MANIFEST_REL)?;
    let mut actual_surfaces = BTreeSet::new();

    for (index, row) in array_field(&manifest, "/determinismMatrix", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("determinismMatrix[{index}]");
        let surface_id = string_field(row, "/surface", &context)?;
        if !expected_surfaces.contains(surface_id) {
            return Err(format!("{context}: unexpected surface {surface_id}"));
        }
        if !actual_surfaces.insert(surface_id.to_owned()) {
            return Err(format!("{context}: duplicate surface {surface_id}"));
        }
        if u64_field(row, "/runCount", &context)? != policy_run_count {
            return Err(format!(
                "{surface_id}: runCount must mirror policy.runCount"
            ));
        }
        if string_field(row, "/canonicalization", &context)? != policy_canonicalization.as_str() {
            return Err(format!(
                "{surface_id}: canonicalization must mirror policy.canonicalization"
            ));
        }
        if bool_field(row, "/stdoutMachineOnly", &context)? != policy_stdout {
            return Err(format!(
                "{surface_id}: stdoutMachineOnly must mirror policy.stdoutMachineOnly"
            ));
        }
        if string_field(row, "/diagnosticsChannel", &context)? != "stderr_or_artifact" {
            return Err(format!(
                "{surface_id}: diagnosticsChannel must stay stderr_or_artifact"
            ));
        }
        if string_field(row, "/runtimeProof", &context)? != "rch_only" {
            return Err(format!("{surface_id}: runtimeProof must stay rch_only"));
        }

        let surface = surface_by_id(&manifest, surface_id)?;
        let surface_context = format!("surfaces[{surface_id}]");
        let row_assertions = string_set(
            array_field(row, "/requiredAssertions", &context)?,
            &format!("{surface_id}.matrix.requiredAssertions"),
        )?;
        let surface_assertions = string_set(
            array_field(surface, "/assertions", &surface_context)?,
            &format!("{surface_id}.assertions"),
        )?;
        if row_assertions != surface_assertions || row_assertions != expected_assertions {
            return Err(format!(
                "{surface_id}: matrix assertions must mirror the surface shared assertions"
            ));
        }

        let surface_pack_assertions = string_set(
            array_field(surface, "/packAssertions", &surface_context)?,
            &format!("{surface_id}.packAssertions"),
        )?;
        let expects_pack_hash = !surface_pack_assertions.is_empty();
        if bool_field(row, "/packHashExpected", &context)? != expects_pack_hash {
            return Err(format!(
                "{surface_id}: packHashExpected must mirror surface packAssertions"
            ));
        }
        if bool_field(row, "/packHashAbsenceFailure", &context)? != expects_pack_hash {
            return Err(format!(
                "{surface_id}: packHashAbsenceFailure must match pack hash expectation"
            ));
        }
        if expects_pack_hash {
            if surface_pack_assertions != pack_assertions {
                return Err(format!(
                    "{surface_id}: pack surface must carry the shared pack assertions"
                ));
            }
            if row.pointer("/packHashField").and_then(Value::as_str) != Some("data.pack.hash") {
                return Err(format!(
                    "{surface_id}: packHashField must be data.pack.hash"
                ));
            }
        } else if !row.pointer("/packHashField").is_some_and(Value::is_null) {
            return Err(format!(
                "{surface_id}: non-pack surface must set packHashField to null"
            ));
        }

        let row_volatile_fields = string_set(
            array_field(row, "/volatileFields", &context)?,
            &format!("{surface_id}.matrix.volatileFields"),
        )?;
        if let Some(anchor_contract) = surface.pointer("/anchorDeterminism") {
            let anchor_volatile_fields = string_set(
                array_field(
                    anchor_contract,
                    "/volatileFields",
                    &format!("{surface_id}.anchorDeterminism"),
                )?,
                &format!("{surface_id}.anchorDeterminism.volatileFields"),
            )?;
            if row_volatile_fields != anchor_volatile_fields {
                return Err(format!(
                    "{surface_id}: matrix volatileFields must mirror anchorDeterminism"
                ));
            }
        } else if !row_volatile_fields.is_empty() {
            return Err(format!(
                "{surface_id}: volatileFields must be declared on the surface before matrix use"
            ));
        }
    }

    if actual_surfaces != expected_surfaces {
        return Err(format!(
            "determinismMatrix surface set drifted: missing={:?}, extra={:?}",
            expected_surfaces
                .difference(&actual_surfaces)
                .collect::<Vec<_>>(),
            actual_surfaces
                .difference(&expected_surfaces)
                .collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn surface_coverage_matrix_accounts_for_every_determinism_surface() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let expected_surfaces = required_set(REQUIRED_SURFACES);
    let policy_local_cargo = string_field(&manifest, "/policy/localCargoProof", MANIFEST_REL)?;
    let mut actual_surfaces = BTreeSet::new();

    for (index, row) in array_field(&manifest, "/surfaceCoverageMatrix", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("surfaceCoverageMatrix[{index}]");
        let surface_id = string_field(row, "/surface", &context)?;
        if !expected_surfaces.contains(surface_id) {
            return Err(format!("{context}: unexpected surface {surface_id}"));
        }
        if !actual_surfaces.insert(surface_id.to_owned()) {
            return Err(format!("{context}: duplicate surface {surface_id}"));
        }

        let surface = surface_by_id(&manifest, surface_id)?;
        let matrix = determinism_matrix_row_by_surface(&manifest, surface_id)?;
        if string_field(row, "/status", &context)? != string_field(surface, "/status", surface_id)?
        {
            return Err(format!("{context}: status must mirror surface status"));
        }

        let owner_bead_count = array_field(surface, "/ownerBeads", surface_id)?.len() as u64;
        let schema_ref_count = array_field(surface, "/schemaRefs", surface_id)?.len() as u64;
        let required_assertion_count =
            array_field(surface, "/assertions", surface_id)?.len() as u64;
        let pack_assertion_count =
            array_field(surface, "/packAssertions", surface_id)?.len() as u64;
        let volatile_field_count = array_field(
            matrix,
            "/volatileFields",
            &format!("{surface_id}.determinismMatrix"),
        )?
        .len() as u64;

        for (pointer, expected) in [
            ("/ownerBeadCount", owner_bead_count),
            ("/schemaRefCount", schema_ref_count),
            ("/requiredAssertionCount", required_assertion_count),
            ("/packAssertionCount", pack_assertion_count),
            ("/volatileFieldCount", volatile_field_count),
        ] {
            let actual = u64_field(row, pointer, &context)?;
            if actual != expected {
                return Err(format!(
                    "{context}{pointer} must be {expected}, got {actual}"
                ));
            }
        }

        if string_field(row, "/determinismStatus", &context)? != "three_run_contract_declared" {
            return Err(format!(
                "{context}: determinismStatus must be three_run_contract_declared"
            ));
        }
        let expects_pack_hash = bool_field(matrix, "/packHashExpected", surface_id)?;
        let expected_pack_status = if expects_pack_hash {
            "pack_hash_required"
        } else {
            "not_applicable"
        };
        if string_field(row, "/packHashStatus", &context)? != expected_pack_status {
            return Err(format!(
                "{context}: packHashStatus must be {expected_pack_status}"
            ));
        }
        if string_field(row, "/runtimeProofPolicy", &context)? != "rch_required_local_invalid" {
            return Err(format!(
                "{context}: runtimeProofPolicy must be rch_required_local_invalid"
            ));
        }
        if string_field(matrix, "/runtimeProof", surface_id)? != "rch_only"
            || policy_local_cargo != "invalid"
        {
            return Err(format!(
                "{surface_id}: runtime proof must remain rch_only with local Cargo invalid"
            ));
        }
        let expected_volatility_status = if volatile_field_count == 0 {
            "no_volatile_fields"
        } else {
            "volatile_fields_declared"
        };
        if string_field(row, "/volatilityStatus", &context)? != expected_volatility_status {
            return Err(format!(
                "{context}: volatilityStatus must be {expected_volatility_status}"
            ));
        }

        let must_clauses = u64_field(row, "/mustClauses", &context)?;
        let tested = u64_field(row, "/tested", &context)?;
        let passing = u64_field(row, "/passing", &context)?;
        let divergent = u64_field(row, "/divergent", &context)?;
        if must_clauses != REQUIRED_SURFACE_MUST_CLAUSES {
            return Err(format!(
                "{context}: mustClauses must stay {REQUIRED_SURFACE_MUST_CLAUSES}"
            ));
        }
        if tested != must_clauses || passing != tested || divergent != 0 {
            return Err(format!(
                "{context}: tested, passing, and divergent must describe full conformance"
            ));
        }

        let score_milli = u64_field(row, "/scoreMilli", &context)?;
        let computed_score = passing * 1000 / must_clauses;
        if score_milli != computed_score {
            return Err(format!(
                "{context}: scoreMilli must be {computed_score}, got {score_milli}"
            ));
        }
        if score_milli < MIN_MUST_COVERAGE_MILLI {
            return Err(format!(
                "{context}: scoreMilli {score_milli} is below {MIN_MUST_COVERAGE_MILLI}"
            ));
        }
        if string_field(row, "/complianceStatus", &context)? != "declared_conformant" {
            return Err(format!(
                "{context}: complianceStatus must be declared_conformant"
            ));
        }
    }

    if actual_surfaces != expected_surfaces {
        return Err(format!(
            "surfaceCoverageMatrix drifted: missing={:?}, extra={:?}",
            expected_surfaces
                .difference(&actual_surfaces)
                .collect::<Vec<_>>(),
            actual_surfaces
                .difference(&expected_surfaces)
                .collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn impact_surface_declares_memory_anchor_determinism_contract() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let impact = surface_by_id(&manifest, "impact")?;
    let contract = impact
        .pointer("/anchorDeterminism")
        .ok_or_else(|| "impact surface must declare anchorDeterminism".to_owned())?;

    for (pointer, expected) in [
        ("/storageAssetKind", "memory_anchors"),
        ("/ownerBead", "bd-1n0np.3.2"),
        (
            "/hashInputMaterial",
            "normalized_anchor_value_with_anchor_kind_and_source_class",
        ),
        ("/generationSource", "workspace_generation_not_wall_clock"),
        (
            "/ordering",
            "memory_id_anchor_kind_anchor_value_hash_generation",
        ),
    ] {
        if string_field(contract, pointer, "impact.anchorDeterminism")? != expected {
            return Err(format!(
                "impact.anchorDeterminism{pointer} must be {expected}"
            ));
        }
    }
    for pointer in ["/rawAnchorValueExcluded", "/redactedValueDeterministic"] {
        if !bool_field(contract, pointer, "impact.anchorDeterminism")? {
            return Err(format!("impact.anchorDeterminism{pointer} must be true"));
        }
    }

    let actual_assertions = string_set(
        array_field(contract, "/requiredAssertions", "impact.anchorDeterminism")?,
        "impact.anchorDeterminism.requiredAssertions",
    )?;
    let expected_assertions = required_set(MEMORY_ANCHOR_DETERMINISM_ASSERTIONS);
    if actual_assertions != expected_assertions {
        return Err(format!(
            "impact.anchorDeterminism.requiredAssertions drifted: missing={:?}, extra={:?}",
            expected_assertions
                .difference(&actual_assertions)
                .collect::<Vec<_>>(),
            actual_assertions
                .difference(&expected_assertions)
                .collect::<Vec<_>>()
        ));
    }

    let volatile_fields = string_set(
        array_field(contract, "/volatileFields", "impact.anchorDeterminism")?,
        "impact.anchorDeterminism.volatileFields",
    )?;
    for required in ["created_at", "updated_at"] {
        if !volatile_fields.contains(required) {
            return Err(format!(
                "impact.anchorDeterminism.volatileFields must include {required}"
            ));
        }
    }

    let planned_shape = memory_anchor_planned_shape()?;
    for (pointer, expected) in [
        ("/anchorValueStorage", "hash_required_raw_value_forbidden"),
        ("/meshExport", "redacted_or_hashed_values_only"),
    ] {
        if string_field(&planned_shape, pointer, "memory_anchors.plannedShape")? != expected {
            return Err(format!(
                "memory_anchors.plannedShape{pointer} must be {expected}"
            ));
        }
    }
    let planned_columns = string_set(
        array_field(&planned_shape, "/columns", "memory_anchors.plannedShape")?,
        "memory_anchors.plannedShape.columns",
    )?;
    for required in ["anchor_value_hash", "redacted_anchor_value", "generation"] {
        if !planned_columns.contains(required) {
            return Err(format!(
                "memory_anchors planned columns must include {required}"
            ));
        }
    }
    for forbidden in ["anchor_value", "raw_anchor_value"] {
        if planned_columns.contains(forbidden) {
            return Err(format!(
                "memory_anchors planned columns must not include raw field {forbidden}"
            ));
        }
    }
    Ok(())
}

#[test]
fn anchor_files_still_contain_required_terms() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    for (index, anchor) in array_field(&manifest, "/anchors", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("anchors[{index}]");
        let source = string_field(anchor, "/source", &context)?;
        let source_text = read_text(source)?;
        for needle in array_field(anchor, "/needles", &context)? {
            let needle = needle
                .as_str()
                .ok_or_else(|| format!("{context}: needle must be a string"))?;
            if !source_text.contains(needle) {
                return Err(format!("{source}: missing required anchor {needle}"));
            }
        }
    }
    Ok(())
}

#[test]
fn documentation_mentions_all_surfaces_and_determinism_terms() -> TestResult {
    let doc = read_text(DOC_REL)?;
    for needle in [
        MANIFEST_REL,
        "bd-1n0np.15.2",
        DETERMINISM_SH_REL,
        DETERMINISM_UNIT_REL,
        MIGRATION_REGISTRY_REL,
        "Local Cargo fallback is not valid proof",
        "VOLATILE_FIELD_NAMES",
        "byte-identical",
        "pack hash",
        "determinismMatrix",
        "Surface Coverage Matrix",
        "surfaceCoverageMatrix",
        "three_run_contract_declared",
        "pack_hash_required",
        "rch_required_local_invalid",
        "declared_conformant",
        "packHashAbsenceFailure",
        "stdoutMachineOnly",
        "stderr_or_artifact",
        "rch_only",
        "memory_anchors",
        "stable_anchor_value_hash",
        "raw_anchor_value_absent",
        "workspace_generation_not_wall_clock",
    ] {
        if !doc.contains(needle) {
            return Err(format!("{DOC_REL}: missing required reference {needle}"));
        }
    }
    for surface in REQUIRED_SURFACES {
        if !doc.contains(surface) {
            return Err(format!("{DOC_REL}: missing surface {surface}"));
        }
    }
    for assertion in REQUIRED_ASSERTIONS.iter().chain(PACK_ASSERTIONS.iter()) {
        if !doc.contains(assertion) {
            return Err(format!("{DOC_REL}: missing assertion {assertion}"));
        }
    }
    Ok(())
}
