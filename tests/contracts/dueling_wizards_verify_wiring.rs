//! bd-1n0np.15.4 - verify.sh wiring contract for feature E2E scripts.
//!
//! This static contract pins the planned dueling-wizards feature E2E gate list
//! and the evidence each implemented gate must expose. Planned scripts are not
//! required to exist yet; implemented rows must have source and verify anchors.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const MANIFEST_REL: &str = "tests/fixtures/contracts/dueling_wizards_verify_wiring.json";
const DOC_REL: &str = "docs/agent-ux/dueling-wizards/verify-wiring.md";
const VERIFY_REL: &str = "scripts/verify.sh";
const HARNESS_REL: &str = "scripts/lib/e2e_harness.sh";
const EVENT_RADAR_REL: &str = "scripts/e2e_event_contract_radar.sh";

const FEATURE_IDS: &[&str] = &[
    "why_not",
    "evidence_harvester",
    "anchors_freshness",
    "error_recall",
    "lod_packing",
    "gap_honesty",
    "contradiction_resolution",
    "store_integrity",
    "provenance_reverification",
    "house_rules",
    "docs_bootstrap",
    "typed_kinds",
    "feedback_gated",
];

const REQUIRED_EVIDENCE: &[&str] = &[
    "run_stage",
    "exit_code",
    "elapsed_ms",
    "artifact_dir",
    "ee.test_event.v1",
];

const MEMORY_ANCHOR_PREFLIGHT_CONTRACTS: &[&str] = &[
    "dueling_wizards_migration_registry",
    "dueling_wizards_backup_coverage",
    "dueling_wizards_determinism_gate",
    "dueling_wizards_mesh_redaction",
];

const IMPLEMENTATION_REQUIREMENTS: &[&str] = &[
    "script_exists",
    "sources_harness",
    "verify_run_stage",
    "emits_test_event",
    "records_exit_code",
    "records_elapsed_ms",
    "records_artifact_dir",
    "rch_only_cargo_proof",
];

const REQUIRED_GATE_MUST_CLAUSES: u64 = 8;
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

fn feature_gate_by_id<'a>(manifest: &'a Value, id: &str) -> Result<&'a Value, String> {
    for gate in array_field(manifest, "/featureE2eScripts", MANIFEST_REL)? {
        if gate
            .pointer("/id")
            .and_then(Value::as_str)
            .is_some_and(|gate_id| gate_id == id)
        {
            return Ok(gate);
        }
    }
    Err(format!("{MANIFEST_REL}: missing feature gate {id}"))
}

fn verify_gate_matrix_row_by_id<'a>(manifest: &'a Value, id: &str) -> Result<&'a Value, String> {
    for row in array_field(manifest, "/verifyGateMatrix", MANIFEST_REL)? {
        if row
            .pointer("/id")
            .and_then(Value::as_str)
            .is_some_and(|row_id| row_id == id)
        {
            return Ok(row);
        }
    }
    Err(format!("{MANIFEST_REL}: missing verifyGateMatrix row {id}"))
}

#[test]
fn verify_wiring_manifest_identity_and_policy_are_stable() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    if string_field(&manifest, "/schema", MANIFEST_REL)? != "ee.dueling_wizards.verify_wiring.v1" {
        return Err(format!(
            "{MANIFEST_REL}: schema must be ee.dueling_wizards.verify_wiring.v1"
        ));
    }
    if string_field(&manifest, "/initiativeBead", MANIFEST_REL)? != "bd-1n0np" {
        return Err("manifest must identify initiativeBead bd-1n0np".to_owned());
    }
    if string_field(&manifest, "/gateBead", MANIFEST_REL)? != "bd-1n0np.15.4" {
        return Err("manifest must identify gateBead bd-1n0np.15.4".to_owned());
    }
    for (pointer, expected) in [
        (
            "/manifestOwner",
            "tests/contracts/dueling_wizards_verify_wiring.rs",
        ),
        ("/doc", DOC_REL),
        ("/verifyScript", VERIFY_REL),
        ("/harness", HARNESS_REL),
        ("/eventRadar", EVENT_RADAR_REL),
    ] {
        if string_field(&manifest, pointer, MANIFEST_REL)? != expected {
            return Err(format!("{pointer} must point at {expected}"));
        }
    }
    if string_field(&manifest, "/implementationState", MANIFEST_REL)? != "planned_contract" {
        return Err("implementationState must stay planned_contract".to_owned());
    }

    for pointer in [
        "/policy/orderedGatesRequired",
        "/policy/runStageRequiredWhenImplemented",
        "/policy/failFastRequired",
        "/policy/perStageExitCodeRequired",
        "/policy/perStageElapsedRequired",
        "/policy/artifactDirRequired",
        "/policy/rchProofRequiredForCargoBackedStages",
    ] {
        if !bool_field(&manifest, pointer, MANIFEST_REL)? {
            return Err(format!("{pointer} must stay true"));
        }
    }
    if string_field(&manifest, "/policy/eventLogSchema", MANIFEST_REL)? != "ee.test_event.v1" {
        return Err("policy.eventLogSchema must stay ee.test_event.v1".to_owned());
    }
    if string_field(&manifest, "/policy/localCargoProof", MANIFEST_REL)? != "invalid" {
        return Err("policy.localCargoProof must stay invalid".to_owned());
    }
    if string_field(&manifest, "/policy/laterFeatureAppendRule", MANIFEST_REL)?
        != "append_manifest_and_verify_stage"
    {
        return Err("laterFeatureAppendRule must stay append_manifest_and_verify_stage".to_owned());
    }
    Ok(())
}

#[test]
fn required_gate_evidence_vocabulary_is_complete() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let mut expected = required_set(REQUIRED_EVIDENCE);
    expected.insert(HARNESS_REL.to_owned());
    let actual = string_set(
        array_field(&manifest, "/requiredGateEvidence", MANIFEST_REL)?,
        "requiredGateEvidence",
    )?;
    if actual != expected {
        return Err(format!(
            "requiredGateEvidence drifted: missing={:?}, extra={:?}",
            expected.difference(&actual).collect::<Vec<_>>(),
            actual.difference(&expected).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn every_feature_script_has_ordered_verify_gate_metadata() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let expected_ids = required_set(FEATURE_IDS);
    let expected_evidence = required_set(REQUIRED_EVIDENCE);
    let mut actual_ids = BTreeSet::new();
    let mut orders = BTreeSet::new();

    for (index, gate) in array_field(&manifest, "/featureE2eScripts", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("featureE2eScripts[{index}]");
        let id = string_field(gate, "/id", &context)?;
        if !actual_ids.insert(id.to_owned()) {
            return Err(format!("duplicate feature id {id}"));
        }
        let order = u64_field(gate, "/order", &context)?;
        if order == 0 || !orders.insert(order) {
            return Err(format!("{id}: order must be nonzero and unique"));
        }
        let bead = string_field(gate, "/bead", &context)?;
        if !bead.starts_with("bd-1n0np.") {
            return Err(format!("{id}: bead must be in the dueling-wizards tree"));
        }
        let script = string_field(gate, "/script", &context)?;
        if !script.starts_with("scripts/e2e_") || !script.ends_with(".sh") {
            return Err(format!("{id}: script must be a scripts/e2e_*.sh path"));
        }
        if string_field(gate, "/verifyStage", &context)?
            .trim()
            .is_empty()
        {
            return Err(format!("{id}: verifyStage must not be empty"));
        }
        let status = string_field(gate, "/status", &context)?;
        if !matches!(status, "planned" | "in_progress" | "implemented") {
            return Err(format!("{id}: unsupported status {status}"));
        }
        if string_field(gate, "/expectedHarness", &context)? != HARNESS_REL {
            return Err(format!("{id}: expectedHarness must be {HARNESS_REL}"));
        }
        if !bool_field(gate, "/cargoBacked", &context)? {
            return Err(format!("{id}: cargoBacked must stay true for RCH policy"));
        }
        let required_evidence = string_set(
            array_field(gate, "/requiredEvidence", &context)?,
            &format!("{id}.requiredEvidence"),
        )?;
        if required_evidence != expected_evidence {
            return Err(format!("{id}: requiredEvidence must carry the shared set"));
        }

        if status == "implemented" {
            let script_text = read_text(script)?;
            let verify_text = read_text(VERIFY_REL)?;
            if !script_text.contains(HARNESS_REL) && !script_text.contains("e2e_harness.sh") {
                return Err(format!(
                    "{id}: implemented script must source {HARNESS_REL}"
                ));
            }
            if !verify_text.contains(script) {
                return Err(format!(
                    "{id}: implemented script must be wired in {VERIFY_REL}"
                ));
            }
        }
    }

    if actual_ids != expected_ids {
        return Err(format!(
            "feature script set drifted: missing={:?}, extra={:?}",
            expected_ids.difference(&actual_ids).collect::<Vec<_>>(),
            actual_ids.difference(&expected_ids).collect::<Vec<_>>()
        ));
    }
    let expected_order_count = FEATURE_IDS.len() as u64;
    if orders.len() as u64 != expected_order_count
        || *orders.iter().next().unwrap_or(&0) != 1
        || *orders.iter().next_back().unwrap_or(&0) != expected_order_count
    {
        return Err("feature script orders must be contiguous from 1".to_owned());
    }
    Ok(())
}

#[test]
fn verify_gate_matrix_covers_every_feature_and_mirrors_policy() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let expected_ids = required_set(FEATURE_IDS);
    let expected_evidence = required_set(REQUIRED_EVIDENCE);
    let expected_requirements = required_set(IMPLEMENTATION_REQUIREMENTS);
    let event_log_schema = string_field(&manifest, "/policy/eventLogSchema", MANIFEST_REL)?;
    let local_cargo_proof = string_field(&manifest, "/policy/localCargoProof", MANIFEST_REL)?;
    let mut actual_ids = BTreeSet::new();

    for (index, row) in array_field(&manifest, "/verifyGateMatrix", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("verifyGateMatrix[{index}]");
        let id = string_field(row, "/id", &context)?;
        if !expected_ids.contains(id) {
            return Err(format!("{context}: unexpected feature id {id}"));
        }
        if !actual_ids.insert(id.to_owned()) {
            return Err(format!("{context}: duplicate feature id {id}"));
        }

        let gate = feature_gate_by_id(&manifest, id)?;
        let gate_context = format!("featureE2eScripts[{id}]");
        for pointer in ["/script", "/verifyStage", "/status", "/expectedHarness"] {
            if string_field(row, pointer, &context)? != string_field(gate, pointer, &gate_context)?
            {
                return Err(format!(
                    "{id}: matrix {pointer} must mirror featureE2eScripts"
                ));
            }
        }
        if u64_field(row, "/order", &context)? != u64_field(gate, "/order", &gate_context)? {
            return Err(format!("{id}: matrix order must mirror featureE2eScripts"));
        }

        let row_evidence = string_set(
            array_field(row, "/requiredEvidence", &context)?,
            &format!("{id}.matrix.requiredEvidence"),
        )?;
        let gate_evidence = string_set(
            array_field(gate, "/requiredEvidence", &gate_context)?,
            &format!("{id}.gate.requiredEvidence"),
        )?;
        if row_evidence != gate_evidence || row_evidence != expected_evidence {
            return Err(format!(
                "{id}: matrix requiredEvidence must mirror the feature row and shared vocabulary"
            ));
        }

        let row_requirements = string_set(
            array_field(row, "/implementationRequirements", &context)?,
            &format!("{id}.implementationRequirements"),
        )?;
        if row_requirements != expected_requirements {
            return Err(format!(
                "{id}: implementationRequirements must carry the shared closeout checklist"
            ));
        }

        if string_field(row, "/eventLogSchema", &context)? != event_log_schema {
            return Err(format!(
                "{id}: eventLogSchema must mirror policy.eventLogSchema"
            ));
        }
        if string_field(row, "/cargoProof", &context)? != "rch_only" {
            return Err(format!("{id}: cargoProof must stay rch_only"));
        }
        if string_field(row, "/localCargoProof", &context)? != local_cargo_proof {
            return Err(format!(
                "{id}: localCargoProof must mirror policy.localCargoProof"
            ));
        }
        if string_field(row, "/implementedEvidenceMode", &context)?
            != "required_when_status_implemented"
        {
            return Err(format!(
                "{id}: implementedEvidenceMode must require evidence when the row is implemented"
            ));
        }
    }

    if actual_ids != expected_ids {
        return Err(format!(
            "verifyGateMatrix feature set drifted: missing={:?}, extra={:?}",
            expected_ids.difference(&actual_ids).collect::<Vec<_>>(),
            actual_ids.difference(&expected_ids).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn gate_coverage_matrix_accounts_for_every_feature_e2e_gate() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let expected_ids = required_set(FEATURE_IDS);
    let expected_evidence_count = REQUIRED_EVIDENCE.len() as u64;
    let expected_requirement_count = IMPLEMENTATION_REQUIREMENTS.len() as u64;
    let local_cargo_proof = string_field(&manifest, "/policy/localCargoProof", MANIFEST_REL)?;
    let mut actual_ids = BTreeSet::new();

    for (index, row) in array_field(&manifest, "/gateCoverageMatrix", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("gateCoverageMatrix[{index}]");
        let id = string_field(row, "/id", &context)?;
        if !expected_ids.contains(id) {
            return Err(format!("{context}: unexpected feature id {id}"));
        }
        if !actual_ids.insert(id.to_owned()) {
            return Err(format!("{context}: duplicate feature id {id}"));
        }

        let gate = feature_gate_by_id(&manifest, id)?;
        let gate_context = format!("featureE2eScripts[{id}]");
        let verify_row = verify_gate_matrix_row_by_id(&manifest, id)?;
        let verify_context = format!("verifyGateMatrix[{id}]");
        for pointer in ["/status", "/script", "/verifyStage"] {
            if string_field(row, pointer, &context)? != string_field(gate, pointer, &gate_context)?
            {
                return Err(format!(
                    "{id}: gateCoverageMatrix {pointer} must mirror featureE2eScripts"
                ));
            }
        }
        if u64_field(row, "/order", &context)? != u64_field(gate, "/order", &gate_context)? {
            return Err(format!(
                "{id}: gateCoverageMatrix order must mirror featureE2eScripts"
            ));
        }
        if !bool_field(row, "/cargoBacked", &context)?
            || !bool_field(gate, "/cargoBacked", &gate_context)?
        {
            return Err(format!(
                "{id}: cargoBacked must stay true in the feature and coverage rows"
            ));
        }

        let evidence_count =
            array_field(verify_row, "/requiredEvidence", &verify_context)?.len() as u64;
        if u64_field(row, "/requiredEvidenceCount", &context)? != evidence_count
            || evidence_count != expected_evidence_count
        {
            return Err(format!(
                "{id}: requiredEvidenceCount must mirror verifyGateMatrix and shared evidence"
            ));
        }
        let requirement_count =
            array_field(verify_row, "/implementationRequirements", &verify_context)?.len() as u64;
        if u64_field(row, "/implementationRequirementCount", &context)? != requirement_count
            || requirement_count != expected_requirement_count
        {
            return Err(format!(
                "{id}: implementationRequirementCount must mirror verifyGateMatrix"
            ));
        }
        let preflight_count = gate
            .pointer("/preflightContracts")
            .and_then(Value::as_array)
            .map_or(0, |contracts| contracts.len() as u64);
        if u64_field(row, "/preflightContractCount", &context)? != preflight_count {
            return Err(format!(
                "{id}: preflightContractCount must mirror featureE2eScripts"
            ));
        }

        let must_clauses = u64_field(row, "/mustClauses", &context)?;
        let tested = u64_field(row, "/tested", &context)?;
        let passing = u64_field(row, "/passing", &context)?;
        let divergent = u64_field(row, "/divergent", &context)?;
        if must_clauses != REQUIRED_GATE_MUST_CLAUSES
            || tested != must_clauses
            || passing != must_clauses
            || divergent != 0
        {
            return Err(format!(
                "{id}: coverage accounting must be complete and non-divergent"
            ));
        }
        let expected_score = passing * 1000 / must_clauses;
        let score = u64_field(row, "/scoreMilli", &context)?;
        if score != expected_score || score < MIN_MUST_COVERAGE_MILLI {
            return Err(format!("{id}: scoreMilli must reflect full coverage"));
        }

        if string_field(row, "/coverageStatus", &context)? != "planned_gate_declared" {
            return Err(format!(
                "{id}: coverageStatus must stay planned_gate_declared"
            ));
        }
        if string_field(verify_row, "/cargoProof", &verify_context)? != "rch_only"
            || string_field(verify_row, "/localCargoProof", &verify_context)? != local_cargo_proof
            || string_field(row, "/runtimeProofPolicy", &context)? != "rch_required_local_invalid"
        {
            return Err(format!(
                "{id}: runtimeProofPolicy must encode RCH-only proof and invalid local Cargo"
            ));
        }
        if string_field(row, "/eventLogStatus", &context)? != "ee_test_event_required" {
            return Err(format!(
                "{id}: eventLogStatus must require ee.test_event.v1"
            ));
        }
        let expected_preflight_status = if preflight_count == 0 {
            "not_applicable"
        } else {
            "preflight_contracts_declared"
        };
        if string_field(row, "/preflightStatus", &context)? != expected_preflight_status {
            return Err(format!(
                "{id}: preflightStatus must reflect declared preflight contracts"
            ));
        }
        if string_field(row, "/complianceStatus", &context)? != "declared_conformant" {
            return Err(format!(
                "{id}: complianceStatus must stay declared_conformant"
            ));
        }
    }

    if actual_ids != expected_ids {
        return Err(format!(
            "gateCoverageMatrix feature set drifted: missing={:?}, extra={:?}",
            expected_ids.difference(&actual_ids).collect::<Vec<_>>(),
            actual_ids.difference(&expected_ids).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn anchors_freshness_declares_memory_anchor_preflight_contracts() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let gate = feature_gate_by_id(&manifest, "anchors_freshness")?;
    let actual = string_set(
        array_field(gate, "/preflightContracts", "anchors_freshness")?,
        "anchors_freshness.preflightContracts",
    )?;
    let expected = required_set(MEMORY_ANCHOR_PREFLIGHT_CONTRACTS);
    if actual != expected {
        return Err(format!(
            "anchors_freshness preflight contracts drifted: missing={:?}, extra={:?}",
            expected.difference(&actual).collect::<Vec<_>>(),
            actual.difference(&expected).collect::<Vec<_>>()
        ));
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
fn documentation_mentions_every_feature_and_verify_policy() -> TestResult {
    let doc = read_text(DOC_REL)?;
    for needle in [
        MANIFEST_REL,
        "bd-1n0np.15.4",
        VERIFY_REL,
        HARNESS_REL,
        EVENT_RADAR_REL,
        "Local Cargo fallback is not valid proof",
        "run_stage",
        "exit code",
        "elapsed time",
        "artifact directory",
        "ee.test_event.v1",
        "verifyGateMatrix",
        "implementationRequirements",
        "script_exists",
        "verify_run_stage",
        "rch_only_cargo_proof",
        "required_when_status_implemented",
        "gateCoverageMatrix",
        "planned_gate_declared",
        "rch_required_local_invalid",
        "ee_test_event_required",
        "preflight_contracts_declared",
        "declared_conformant",
        "preflightContracts",
        "dueling_wizards_migration_registry",
        "dueling_wizards_backup_coverage",
        "dueling_wizards_determinism_gate",
        "dueling_wizards_mesh_redaction",
    ] {
        if !doc.contains(needle) {
            return Err(format!("{DOC_REL}: missing required reference {needle}"));
        }
    }
    for id in FEATURE_IDS {
        if !doc.contains(id) {
            return Err(format!("{DOC_REL}: missing feature id {id}"));
        }
    }
    for evidence in REQUIRED_EVIDENCE {
        if !doc.contains(evidence) {
            return Err(format!("{DOC_REL}: missing evidence term {evidence}"));
        }
    }
    Ok(())
}
