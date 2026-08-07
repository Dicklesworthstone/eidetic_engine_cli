//! bd-1n0np.23.4 - mesh redaction contract for new dueling-wizards fields.
//!
//! This manifest is a planning gate. It ensures every storage class allocated
//! by the migration registry has an explicit conservative mesh-export posture
//! before runtime sharing code can expose the fields.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const MANIFEST_REL: &str = "tests/fixtures/contracts/dueling_wizards_mesh_redaction.json";
const MIGRATION_REGISTRY_REL: &str =
    "tests/fixtures/contracts/dueling_wizards_migration_registry.json";
const DOC_REL: &str = "docs/agent-ux/dueling-wizards/mesh-redaction.md";
const PEER_POLICY_DOC_REL: &str = "docs/mesh/peer_policy.md";
const SHARE_PREVIEW_SOURCE_REL: &str = "src/policy/mod.rs";

const REQUIRED_POLICY_DOC_ANCHORS: &[&str] = &[
    "redaction",
    "metadata",
    "body",
    "embedding",
    "graphLink",
    "revisionNotice",
    "curationSignal",
    "payloadExportAllowed",
    "rawPayloadExportAllowed",
    "redactedPayloadRequired",
];

const REQUIRED_SHARE_PREVIEW_SOURCE_ANCHORS: &[&str] = &[
    "SHARE_PREVIEW_SCHEMA_V2",
    "SharePreviewCandidate",
    "redaction_class",
    "build_share_preview",
    "scan_mesh_export_subjects",
    "MESH_EXPORT_POLICY_ATTESTATION_SCHEMA_V1",
];

const FORBIDDEN_SHARE_PREVIEW_ORACLES: &[&str] = &[
    "share_preview_hash",
    "share_preview_content_hash",
    "serialization_error:",
];

const FORBIDDEN_MEMORY_ANCHOR_MESH_FIELDS: &[&str] = &[
    "anchor_value",
    "raw_anchor_value",
    "raw_path",
    "raw_symbol",
    "raw_command",
    "raw_schema",
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct MigrationAllocation {
    asset_kind: String,
    owner_bead: String,
}

fn migration_allocations_by_id() -> Result<BTreeMap<String, MigrationAllocation>, String> {
    let registry = read_json(MIGRATION_REGISTRY_REL)?;
    let mut by_id = BTreeMap::new();
    for (index, allocation) in array_field(&registry, "/allocations", MIGRATION_REGISTRY_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("migration allocation[{index}]");
        let id = string_field(allocation, "/id", &context)?;
        let asset_kind = string_field(allocation, "/backupAssetKind", &context)?;
        let owner_bead = string_field(allocation, "/ownerBead", &context)?;
        by_id.insert(
            id.to_owned(),
            MigrationAllocation {
                asset_kind: asset_kind.to_owned(),
                owner_bead: owner_bead.to_owned(),
            },
        );
    }
    Ok(by_id)
}

fn migration_planned_shape_by_id(id: &str) -> Result<Value, String> {
    let registry = read_json(MIGRATION_REGISTRY_REL)?;
    for allocation in array_field(&registry, "/allocations", MIGRATION_REGISTRY_REL)? {
        if allocation
            .pointer("/id")
            .and_then(Value::as_str)
            .is_some_and(|allocation_id| allocation_id == id)
        {
            return allocation
                .pointer("/plannedShape")
                .cloned()
                .ok_or_else(|| format!("{id} allocation must declare plannedShape"));
        }
    }
    Err(format!("{MIGRATION_REGISTRY_REL}: missing allocation {id}"))
}

fn field_class_by_asset_kind<'a>(
    manifest: &'a Value,
    asset_kind: &str,
) -> Result<&'a Value, String> {
    for class in array_field(manifest, "/fieldClasses", MANIFEST_REL)? {
        if class
            .pointer("/assetKind")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind == asset_kind)
        {
            return Ok(class);
        }
    }
    Err(format!("{MANIFEST_REL}: missing assetKind {asset_kind}"))
}

#[test]
fn mesh_redaction_manifest_identity_and_policy_are_stable() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    if string_field(&manifest, "/schema", MANIFEST_REL)? != "ee.dueling_wizards.mesh_redaction.v1" {
        return Err(format!(
            "{MANIFEST_REL}: schema must be ee.dueling_wizards.mesh_redaction.v1"
        ));
    }
    if string_field(&manifest, "/initiativeBead", MANIFEST_REL)? != "bd-1n0np" {
        return Err("mesh redaction manifest must identify initiativeBead bd-1n0np".to_owned());
    }
    if string_field(&manifest, "/gateBead", MANIFEST_REL)? != "bd-1n0np.23.4" {
        return Err("mesh redaction manifest must identify gateBead bd-1n0np.23.4".to_owned());
    }
    if string_field(&manifest, "/doc", MANIFEST_REL)? != DOC_REL {
        return Err("mesh redaction manifest must point at its doc".to_owned());
    }
    if string_field(&manifest, "/migrationRegistry", MANIFEST_REL)? != MIGRATION_REGISTRY_REL {
        return Err("mesh redaction manifest must point at the migration registry".to_owned());
    }
    if string_field(&manifest, "/peerPolicyDoc", MANIFEST_REL)? != PEER_POLICY_DOC_REL {
        return Err("mesh redaction manifest must point at docs/mesh/peer_policy.md".to_owned());
    }
    if string_field(&manifest, "/sharePreviewSource", MANIFEST_REL)? != SHARE_PREVIEW_SOURCE_REL {
        return Err("mesh redaction manifest must point at src/policy/mod.rs".to_owned());
    }
    if string_field(&manifest, "/policy/defaultPosture", MANIFEST_REL)?
        != "conservative_omit_or_hash"
    {
        return Err("defaultPosture must stay conservative_omit_or_hash".to_owned());
    }
    for pointer in [
        "/policy/requiresPeerPolicy",
        "/policy/rchProofRequiredForRustTests",
    ] {
        if !bool_field(&manifest, pointer, MANIFEST_REL)? {
            return Err(format!("{pointer} must stay true"));
        }
    }
    for pointer in [
        "/policy/rawBodyExportAllowed",
        "/policy/rawEmbeddingExportAllowed",
        "/policy/payloadExportAllowed",
        "/policy/rawPayloadExportAllowed",
    ] {
        if bool_field(&manifest, pointer, MANIFEST_REL)? {
            return Err(format!("{pointer} must stay false"));
        }
    }
    if string_field(&manifest, "/policy/localCargoProof", MANIFEST_REL)? != "invalid" {
        return Err("local Cargo proof must stay invalid".to_owned());
    }
    Ok(())
}

#[test]
fn field_classes_cover_every_migration_backup_asset_kind() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let allocations = migration_allocations_by_id()?;
    let expected_asset_kinds = allocations
        .values()
        .map(|allocation| allocation.asset_kind.clone())
        .collect::<BTreeSet<_>>();
    let mut actual_asset_kinds = BTreeSet::new();

    for (index, class) in array_field(&manifest, "/fieldClasses", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("fieldClasses[{index}]");
        let asset_kind = string_field(class, "/assetKind", &context)?;
        if !actual_asset_kinds.insert(asset_kind.to_owned()) {
            return Err(format!("duplicate mesh redaction assetKind {asset_kind}"));
        }

        let owner_beads = string_set(
            array_field(class, "/ownerBeads", &context)?,
            &format!("{asset_kind}.ownerBeads"),
        )?;
        let allocation_ids = string_set(
            array_field(class, "/migrationAllocationIds", &context)?,
            &format!("{asset_kind}.migrationAllocationIds"),
        )?;
        if owner_beads.is_empty() || allocation_ids.is_empty() {
            return Err(format!(
                "{asset_kind}: ownerBeads and migrationAllocationIds must not be empty"
            ));
        }
        for allocation_id in &allocation_ids {
            let Some(allocation) = allocations.get(allocation_id) else {
                return Err(format!(
                    "{asset_kind}: migration allocation id {allocation_id} is not in {MIGRATION_REGISTRY_REL}"
                ));
            };
            if allocation.asset_kind != asset_kind {
                return Err(format!(
                    "{asset_kind}: allocation {allocation_id} belongs to backupAssetKind {}",
                    allocation.asset_kind
                ));
            }
            if !owner_beads.contains(&allocation.owner_bead) {
                return Err(format!(
                    "{asset_kind}: ownerBeads must include registry owner {}",
                    allocation.owner_bead
                ));
            }
        }
    }

    if actual_asset_kinds != expected_asset_kinds {
        return Err(format!(
            "mesh redaction asset kind set drifted: missing={:?}, extra={:?}",
            expected_asset_kinds
                .difference(&actual_asset_kinds)
                .collect::<Vec<_>>(),
            actual_asset_kinds
                .difference(&expected_asset_kinds)
                .collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn memory_anchor_mesh_class_forbids_raw_anchor_values() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let class = field_class_by_asset_kind(&manifest, "memory_anchors")?;
    let contract = class
        .pointer("/anchorValueContract")
        .ok_or_else(|| "memory_anchors must declare anchorValueContract".to_owned())?;

    if bool_field(
        contract,
        "/rawAnchorValuesAllowed",
        "memory_anchors.anchorValueContract",
    )? {
        return Err("memory_anchors mesh export must not allow raw anchor values".to_owned());
    }
    for (pointer, expected) in [
        (
            "/meshValueMaterialPolicy",
            "hash_or_redacted_anchor_value_only",
        ),
        ("/sharePreviewValue", "hash_only"),
        (
            "/peerPolicyEscalation",
            "required_for_any_raw_value_or_payload_lane_change",
        ),
    ] {
        if string_field(contract, pointer, "memory_anchors.anchorValueContract")? != expected {
            return Err(format!(
                "memory_anchors.anchorValueContract{pointer} must be {expected}"
            ));
        }
    }

    let allowed_value_fields = string_set(
        array_field(
            contract,
            "/allowedValueFields",
            "memory_anchors.anchorValueContract",
        )?,
        "memory_anchors.anchorValueContract.allowedValueFields",
    )?;
    for required in ["anchor_value_hash", "redacted_anchor_value"] {
        if !allowed_value_fields.contains(required) {
            return Err(format!(
                "memory_anchors mesh export must allow value field {required}"
            ));
        }
    }

    let forbidden_fields = string_set(
        array_field(
            contract,
            "/forbiddenOutboundFields",
            "memory_anchors.anchorValueContract",
        )?,
        "memory_anchors.anchorValueContract.forbiddenOutboundFields",
    )?;
    for forbidden in FORBIDDEN_MEMORY_ANCHOR_MESH_FIELDS {
        if !forbidden_fields.contains(*forbidden) {
            return Err(format!(
                "memory_anchors mesh export must forbid raw field {forbidden}"
            ));
        }
        if allowed_value_fields.contains(*forbidden) {
            return Err(format!(
                "memory_anchors allowedValueFields must not include raw field {forbidden}"
            ));
        }
    }

    let planned_shape = migration_planned_shape_by_id("memory_anchors")?;
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
    Ok(())
}

#[test]
fn field_classes_are_conservative_by_default() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let allowed_lanes = string_set(
        array_field(&manifest, "/allowedMaterialLanes", MANIFEST_REL)?,
        "/allowedMaterialLanes",
    )?;
    let allowed_redaction_classes = string_set(
        array_field(&manifest, "/allowedRedactionClasses", MANIFEST_REL)?,
        "/allowedRedactionClasses",
    )?;
    let allowed_export_postures = string_set(
        array_field(&manifest, "/allowedExportPostures", MANIFEST_REL)?,
        "/allowedExportPostures",
    )?;

    for (index, class) in array_field(&manifest, "/fieldClasses", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("fieldClasses[{index}]");
        let asset_kind = string_field(class, "/assetKind", &context)?;
        let lane = string_field(class, "/meshMaterialLane", &context)?;
        if !allowed_lanes.contains(lane) {
            return Err(format!("{asset_kind}: unsupported meshMaterialLane {lane}"));
        }
        let redaction_class = string_field(class, "/defaultRedactionClass", &context)?;
        if !allowed_redaction_classes.contains(redaction_class) {
            return Err(format!(
                "{asset_kind}: unsupported defaultRedactionClass {redaction_class}"
            ));
        }
        let share_preview_class = string_field(class, "/sharePreviewClass", &context)?;
        if !allowed_redaction_classes.contains(share_preview_class) {
            return Err(format!(
                "{asset_kind}: unsupported sharePreviewClass {share_preview_class}"
            ));
        }
        let export_posture = string_field(class, "/meshExportPosture", &context)?;
        if !allowed_export_postures.contains(export_posture) {
            return Err(format!(
                "{asset_kind}: unsupported meshExportPosture {export_posture}"
            ));
        }
        if !bool_field(class, "/requiresPeerPolicy", &context)? {
            return Err(format!("{asset_kind}: requiresPeerPolicy must be true"));
        }
        for pointer in [
            "/payloadExportAllowed",
            "/rawPayloadExportAllowed",
            "/rawBodyExportAllowed",
            "/rawEmbeddingExportAllowed",
        ] {
            if bool_field(class, pointer, &context)? {
                return Err(format!("{asset_kind}: {pointer} must stay false"));
            }
        }
        match redaction_class {
            "omit" => {
                if export_posture != "deny" || share_preview_class != "omit" {
                    return Err(format!(
                        "{asset_kind}: omit classes must deny mesh export and omit share previews"
                    ));
                }
                if bool_field(class, "/redactedPayloadRequired", &context)? {
                    return Err(format!(
                        "{asset_kind}: omitted classes must not require redacted payloads"
                    ));
                }
            }
            "hash" | "redact" => {
                if export_posture != "redact" {
                    return Err(format!(
                        "{asset_kind}: {redaction_class} classes must use redacted mesh export"
                    ));
                }
                if !bool_field(class, "/redactedPayloadRequired", &context)? {
                    return Err(format!(
                        "{asset_kind}: redacted export classes must require redacted payloads"
                    ));
                }
            }
            other => return Err(format!("{asset_kind}: unsupported redaction class {other}")),
        }
        if string_field(class, "/sourceValueHandling", &context)?
            .trim()
            .is_empty()
        {
            return Err(format!(
                "{asset_kind}: sourceValueHandling must not be empty"
            ));
        }
        if string_field(class, "/reason", &context)?.trim().is_empty() {
            return Err(format!("{asset_kind}: reason must not be empty"));
        }
    }
    Ok(())
}

#[test]
fn outbound_decision_examples_match_field_class_policy() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let allowed_export_postures = string_set(
        array_field(&manifest, "/allowedExportPostures", MANIFEST_REL)?,
        "/allowedExportPostures",
    )?;
    let expected_redaction_classes = string_set(
        array_field(&manifest, "/allowedRedactionClasses", MANIFEST_REL)?,
        "/allowedRedactionClasses",
    )?;
    let mut covered_redaction_classes = BTreeSet::new();
    let mut covered_export_postures = BTreeSet::new();
    let mut example_ids = BTreeSet::new();

    for (index, example) in array_field(&manifest, "/outboundDecisionExamples", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("outboundDecisionExamples[{index}]");
        let example_id = string_field(example, "/exampleId", &context)?;
        if example_id.trim().is_empty() || !example_ids.insert(example_id.to_owned()) {
            return Err(format!("{context}: exampleId must be unique and non-empty"));
        }

        let asset_kind = string_field(example, "/assetKind", &context)?;
        let field_class = field_class_by_asset_kind(&manifest, asset_kind)?;
        let requested_material = string_field(example, "/requestedMaterial", &context)?;
        if requested_material.trim().is_empty() {
            return Err(format!("{context}: requestedMaterial must not be empty"));
        }

        for pointer in [
            "/defaultRedactionClass",
            "/meshExportPosture",
            "/sharePreviewClass",
        ] {
            if string_field(example, pointer, &context)?
                != string_field(field_class, pointer, asset_kind)?
            {
                return Err(format!(
                    "{context}: {pointer} must match fieldClasses entry for {asset_kind}"
                ));
            }
        }

        let redaction_class = string_field(example, "/defaultRedactionClass", &context)?;
        covered_redaction_classes.insert(redaction_class.to_owned());
        let export_posture = string_field(example, "/meshExportPosture", &context)?;
        if !allowed_export_postures.contains(export_posture) {
            return Err(format!(
                "{context}: unsupported meshExportPosture {export_posture}"
            ));
        }
        covered_export_postures.insert(export_posture.to_owned());

        for pointer in [
            "/payloadExportAllowed",
            "/rawPayloadExportAllowed",
            "/rawBodyExportAllowed",
            "/rawEmbeddingExportAllowed",
            "/redactedPayloadRequired",
        ] {
            if bool_field(example, pointer, &context)?
                != bool_field(field_class, pointer, asset_kind)?
            {
                return Err(format!(
                    "{context}: {pointer} must match fieldClasses entry for {asset_kind}"
                ));
            }
        }
        for pointer in [
            "/payloadExportAllowed",
            "/rawPayloadExportAllowed",
            "/rawBodyExportAllowed",
            "/rawEmbeddingExportAllowed",
        ] {
            if bool_field(example, pointer, &context)? {
                return Err(format!("{context}: {pointer} must stay false"));
            }
        }

        let decision = string_field(example, "/decision", &context)?;
        match decision {
            "deny" => {
                if export_posture != "deny" || redaction_class != "omit" {
                    return Err(format!("{context}: deny decisions must omit and deny"));
                }
                if bool_field(example, "/redactedPayloadRequired", &context)? {
                    return Err(format!(
                        "{context}: deny decisions must not require a redacted payload"
                    ));
                }
            }
            "redact" => {
                if export_posture != "redact"
                    || !bool_field(example, "/redactedPayloadRequired", &context)?
                {
                    return Err(format!(
                        "{context}: redact decisions must require redacted export posture"
                    ));
                }
            }
            other => return Err(format!("{context}: unsupported decision {other}")),
        }

        if asset_kind == "memory_anchors" {
            let contract = field_class
                .pointer("/anchorValueContract")
                .ok_or_else(|| "memory_anchors must declare anchorValueContract".to_owned())?;
            if string_field(example, "/peerPolicyEscalation", &context)?
                != string_field(
                    contract,
                    "/peerPolicyEscalation",
                    "memory_anchors.anchorValueContract",
                )?
            {
                return Err(format!(
                    "{context}: peerPolicyEscalation must match anchor value contract"
                ));
            }
        }
    }

    if covered_redaction_classes != expected_redaction_classes {
        return Err(format!(
            "outboundDecisionExamples must cover every redaction class: expected {expected_redaction_classes:?}, got {covered_redaction_classes:?}"
        ));
    }
    if covered_export_postures != allowed_export_postures {
        return Err(format!(
            "outboundDecisionExamples must cover every export posture: expected {allowed_export_postures:?}, got {covered_export_postures:?}"
        ));
    }
    Ok(())
}

#[test]
fn peer_policy_and_share_preview_anchors_still_exist() -> TestResult {
    let peer_policy_doc = read_text(PEER_POLICY_DOC_REL)?;
    for anchor in REQUIRED_POLICY_DOC_ANCHORS {
        if !peer_policy_doc.contains(anchor) {
            return Err(format!("{PEER_POLICY_DOC_REL}: missing anchor {anchor}"));
        }
    }

    let share_preview_source = read_text(SHARE_PREVIEW_SOURCE_REL)?;
    for anchor in REQUIRED_SHARE_PREVIEW_SOURCE_ANCHORS {
        if !share_preview_source.contains(anchor) {
            return Err(format!(
                "{SHARE_PREVIEW_SOURCE_REL}: missing anchor {anchor}"
            ));
        }
    }
    for forbidden in FORBIDDEN_SHARE_PREVIEW_ORACLES {
        if share_preview_source.contains(forbidden) {
            return Err(format!(
                "{SHARE_PREVIEW_SOURCE_REL}: public share-preview oracle must remain absent: {forbidden}"
            ));
        }
    }
    Ok(())
}

#[test]
fn documentation_mentions_all_contract_inputs_and_asset_kinds() -> TestResult {
    let doc = read_text(DOC_REL)?;
    for required in [
        MANIFEST_REL,
        MIGRATION_REGISTRY_REL,
        PEER_POLICY_DOC_REL,
        SHARE_PREVIEW_SOURCE_REL,
        "Local Cargo fallback is not valid proof",
        "hash_or_redacted_anchor_value_only",
        "raw_anchor_value",
        "required_for_any_raw_value_or_payload_lane_change",
        "outboundDecisionExamples",
        "memory_anchor_hash_preview",
        "sentinel_spec_omit",
        "typed_memory_field_redaction",
    ] {
        if !doc.contains(required) {
            return Err(format!("{DOC_REL}: missing required reference {required}"));
        }
    }

    let allocations = migration_allocations_by_id()?;
    for allocation in allocations.values() {
        if !doc.contains(&allocation.asset_kind) {
            return Err(format!(
                "{DOC_REL}: missing asset kind {}",
                allocation.asset_kind
            ));
        }
    }
    Ok(())
}
