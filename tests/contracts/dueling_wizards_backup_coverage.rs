//! bd-1n0np.23.2 - backup/export/restore coverage plan for the
//! dueling-wizards storage assets.
//!
//! This test keeps the backup coverage manifest in lockstep with the migration
//! sequencing registry. Runtime backup support still has to be implemented by
//! the owning schema beads; this contract prevents new storage plans from
//! omitting their backup/restore obligations.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const MANIFEST_REL: &str = "tests/fixtures/contracts/dueling_wizards_backup_coverage.json";
const MIGRATION_REGISTRY_REL: &str =
    "tests/fixtures/contracts/dueling_wizards_migration_registry.json";
const DOC_REL: &str = "docs/agent-ux/dueling-wizards/backup-coverage.md";
const BACKUP_SOURCE_REL: &str = "src/core/backup.rs";

const REQUIRED_COVERAGE_SURFACES: &[&str] = &[
    "backup_create",
    "backup_inspect",
    "backup_verify",
    "backup_restore",
    "manifest_rehash",
    "roundtrip_e2e",
];

const FORBIDDEN_MEMORY_ANCHOR_BACKUP_FIELDS: &[&str] = &[
    "anchor_value",
    "raw_anchor_value",
    "raw_path",
    "raw_symbol",
    "raw_command",
    "raw_schema",
];

const REQUIRED_FAILURE_SCENARIOS: &[&str] = &[
    "missing_derived_asset",
    "corrupt_derived_asset_hash",
    "restore_manifest_rehash_mismatch",
    "raw_anchor_value_present",
];

const REQUIRED_ASSET_MUST_CLAUSES: u64 = 10;
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
        .ok_or_else(|| format!("{context}: missing u64 field {pointer}"))
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

fn migration_backup_asset_kinds() -> Result<BTreeMap<String, String>, String> {
    let registry = read_json(MIGRATION_REGISTRY_REL)?;
    let mut by_asset = BTreeMap::new();
    for (index, allocation) in array_field(&registry, "/allocations", MIGRATION_REGISTRY_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("migration allocation[{index}]");
        let id = string_field(allocation, "/id", &context)?;
        let asset_kind = string_field(allocation, "/backupAssetKind", &context)?;
        by_asset.insert(asset_kind.to_owned(), id.to_owned());
    }
    Ok(by_asset)
}

fn migration_memory_anchor_columns() -> Result<BTreeSet<String>, String> {
    let registry = read_json(MIGRATION_REGISTRY_REL)?;
    for allocation in array_field(&registry, "/allocations", MIGRATION_REGISTRY_REL)? {
        if allocation
            .pointer("/id")
            .and_then(Value::as_str)
            .is_some_and(|id| id == "memory_anchors")
        {
            let shape = allocation
                .pointer("/plannedShape")
                .ok_or_else(|| "memory_anchors allocation must declare plannedShape".to_owned())?;
            return string_set(
                array_field(shape, "/columns", "memory_anchors.plannedShape")?,
                "memory_anchors.plannedShape.columns",
            );
        }
    }
    Err(format!(
        "{MIGRATION_REGISTRY_REL}: missing memory_anchors allocation"
    ))
}

fn backup_asset_by_kind<'a>(manifest: &'a Value, asset_kind: &str) -> Result<&'a Value, String> {
    for asset in array_field(manifest, "/assets", MANIFEST_REL)? {
        if asset
            .pointer("/assetKind")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind == asset_kind)
        {
            return Ok(asset);
        }
    }
    Err(format!("{MANIFEST_REL}: missing assetKind {asset_kind}"))
}

#[test]
fn backup_manifest_identity_and_policy_are_stable() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    if string_field(&manifest, "/schema", MANIFEST_REL)? != "ee.dueling_wizards.backup_coverage.v1"
    {
        return Err(format!(
            "{MANIFEST_REL}: schema must be ee.dueling_wizards.backup_coverage.v1"
        ));
    }
    if string_field(&manifest, "/initiativeBead", MANIFEST_REL)? != "bd-1n0np" {
        return Err("backup manifest must identify initiativeBead bd-1n0np".to_owned());
    }
    if string_field(&manifest, "/gateBead", MANIFEST_REL)? != "bd-1n0np.23.2" {
        return Err("backup manifest must identify gateBead bd-1n0np.23.2".to_owned());
    }
    if string_field(&manifest, "/migrationRegistry", MANIFEST_REL)? != MIGRATION_REGISTRY_REL {
        return Err("backup manifest must point at the migration registry".to_owned());
    }
    if string_field(&manifest, "/runtimeBackupSource", MANIFEST_REL)? != BACKUP_SOURCE_REL {
        return Err("backup manifest must point at src/core/backup.rs".to_owned());
    }
    if string_field(&manifest, "/roundTripE2e", MANIFEST_REL)?
        != "tests/e2e_backup_restore_roundtrip.rs"
    {
        return Err("backup manifest must name the current backup round-trip e2e".to_owned());
    }
    if !bool_field(
        &manifest,
        "/policy/allMigrationBackupAssetKindsCovered",
        MANIFEST_REL,
    )? {
        return Err(
            "backup manifest must require all migration asset kinds to be covered".to_owned(),
        );
    }
    if string_field(&manifest, "/policy/hashPolicy", MANIFEST_REL)? != "blake3_required" {
        return Err("backup manifest hashPolicy must be blake3_required".to_owned());
    }
    if string_field(&manifest, "/policy/missingAssetFailure", MANIFEST_REL)?
        != "degraded_not_silent_loss"
    {
        return Err(
            "backup manifest missingAssetFailure must be degraded_not_silent_loss".to_owned(),
        );
    }
    Ok(())
}

#[test]
fn backup_assets_cover_every_migration_backup_asset_kind() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let expected = migration_backup_asset_kinds()?;
    let mut actual = BTreeMap::new();

    for (index, asset) in array_field(&manifest, "/assets", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("asset[{index}]");
        let asset_kind = string_field(asset, "/assetKind", &context)?;
        let allocation_ids = string_set(
            array_field(asset, "/migrationAllocationIds", &context)?,
            &format!("{asset_kind}.migrationAllocationIds"),
        )?;
        if allocation_ids.is_empty() {
            return Err(format!(
                "{asset_kind}: migrationAllocationIds must not be empty"
            ));
        }
        for allocation_id in &allocation_ids {
            let Some(expected_asset) = expected
                .iter()
                .find_map(|(kind, id)| (id == allocation_id).then_some(kind))
            else {
                return Err(format!(
                    "{asset_kind}: migration allocation id {allocation_id} is not in {MIGRATION_REGISTRY_REL}"
                ));
            };
            if expected_asset.as_str() != asset_kind {
                return Err(format!(
                    "{asset_kind}: allocation {allocation_id} belongs to backupAssetKind {expected_asset}"
                ));
            }
        }
        actual.insert(asset_kind.to_owned(), allocation_ids);
    }

    let expected_kinds = expected.keys().cloned().collect::<BTreeSet<_>>();
    let actual_kinds = actual.keys().cloned().collect::<BTreeSet<_>>();
    if actual_kinds != expected_kinds {
        return Err(format!(
            "backup asset kind set drifted: missing={:?}, extra={:?}",
            expected_kinds.difference(&actual_kinds).collect::<Vec<_>>(),
            actual_kinds.difference(&expected_kinds).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn each_asset_declares_full_backup_restore_coverage() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let required_surfaces = REQUIRED_COVERAGE_SURFACES
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<BTreeSet<_>>();
    let top_level_surfaces = string_set(
        array_field(&manifest, "/coverageSurfaces", MANIFEST_REL)?,
        "/coverageSurfaces",
    )?;
    if top_level_surfaces != required_surfaces {
        return Err(format!(
            "top-level coverageSurfaces drifted: expected {required_surfaces:?}, got {top_level_surfaces:?}"
        ));
    }

    for (index, asset) in array_field(&manifest, "/assets", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("asset[{index}]");
        let asset_kind = string_field(asset, "/assetKind", &context)?;
        let owner_beads = string_set(
            array_field(asset, "/ownerBeads", &context)?,
            &format!("{asset_kind}.ownerBeads"),
        )?;
        if owner_beads.is_empty() {
            return Err(format!("{asset_kind}: ownerBeads must not be empty"));
        }
        for owner in &owner_beads {
            if !owner.starts_with("bd-1n0np.") {
                return Err(format!(
                    "{asset_kind}: owner bead {owner} must belong to bd-1n0np"
                ));
            }
        }

        let storage_class = string_field(asset, "/storageClass", &context)?;
        if !matches!(storage_class, "durable" | "derived" | "durable_and_derived") {
            return Err(format!(
                "{asset_kind}: unsupported storageClass {storage_class}"
            ));
        }
        let manifest_mode = string_field(asset, "/manifestMode", &context)?;
        if !matches!(
            manifest_mode,
            "records_jsonl" | "derived_manifest_v2" | "records_jsonl_and_derived_manifest_v2"
        ) {
            return Err(format!(
                "{asset_kind}: unsupported manifestMode {manifest_mode}"
            ));
        }
        if string_field(asset, "/hashPolicy", &context)? != "blake3_required" {
            return Err(format!("{asset_kind}: hashPolicy must be blake3_required"));
        }
        if string_field(asset, "/missingAssetFailure", &context)? != "degraded_not_silent_loss" {
            return Err(format!(
                "{asset_kind}: missingAssetFailure must be degraded_not_silent_loss"
            ));
        }
        if string_field(asset, "/roundTripEvidence", &context)?
            .trim()
            .is_empty()
        {
            return Err(format!("{asset_kind}: roundTripEvidence must not be empty"));
        }
        let surfaces = string_set(
            array_field(asset, "/coverageSurfaces", &context)?,
            &format!("{asset_kind}.coverageSurfaces"),
        )?;
        if surfaces != required_surfaces {
            return Err(format!(
                "{asset_kind}: coverageSurfaces must carry the full backup checklist"
            ));
        }
    }
    Ok(())
}

#[test]
fn asset_coverage_matrix_accounts_for_every_backup_asset_kind() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let expected = migration_backup_asset_kinds()?;
    let expected_kinds = expected.keys().cloned().collect::<BTreeSet<_>>();
    let mut assets = BTreeMap::new();

    for (index, asset) in array_field(&manifest, "/assets", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("assets[{index}]");
        let asset_kind = string_field(asset, "/assetKind", &context)?;
        if assets.insert(asset_kind.to_owned(), asset).is_some() {
            return Err(format!("{context}: duplicate assetKind {asset_kind}"));
        }
    }

    let actual_asset_kinds = assets.keys().cloned().collect::<BTreeSet<_>>();
    if actual_asset_kinds != expected_kinds {
        return Err(format!(
            "asset registry drifted before matrix check: missing={:?}, extra={:?}",
            expected_kinds
                .difference(&actual_asset_kinds)
                .collect::<Vec<_>>(),
            actual_asset_kinds
                .difference(&expected_kinds)
                .collect::<Vec<_>>()
        ));
    }

    let mut matrix_kinds = BTreeSet::new();
    for (index, row) in array_field(&manifest, "/assetCoverageMatrix", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("assetCoverageMatrix[{index}]");
        let asset_kind = string_field(row, "/assetKind", &context)?;
        if !matrix_kinds.insert(asset_kind.to_owned()) {
            return Err(format!("{context}: duplicate assetKind {asset_kind}"));
        }
        let Some(asset) = assets.get(asset_kind) else {
            return Err(format!(
                "{context}: matrix row has no matching assetKind {asset_kind}"
            ));
        };

        for pointer in [
            "/storageClass",
            "/manifestMode",
            "/hashPolicy",
            "/missingAssetFailure",
        ] {
            let row_value = string_field(row, pointer, &context)?;
            let asset_value = string_field(asset, pointer, asset_kind)?;
            if row_value != asset_value {
                return Err(format!(
                    "{context}{pointer} must mirror {asset_kind}{pointer}: expected {asset_value}, got {row_value}"
                ));
            }
        }

        let allocation_count =
            array_field(asset, "/migrationAllocationIds", asset_kind)?.len() as u64;
        let owner_bead_count = array_field(asset, "/ownerBeads", asset_kind)?.len() as u64;
        let coverage_surface_count =
            array_field(asset, "/coverageSurfaces", asset_kind)?.len() as u64;
        let privacy_forbidden_field_count = if let Some(privacy) = asset.pointer("/privacyContract")
        {
            array_field(
                privacy,
                "/forbiddenFields",
                &format!("{asset_kind}.privacyContract"),
            )?
            .len() as u64
        } else {
            0
        };

        for (pointer, expected_value) in [
            ("/allocationCount", allocation_count),
            ("/ownerBeadCount", owner_bead_count),
            ("/coverageSurfaceCount", coverage_surface_count),
            ("/privacyForbiddenFieldCount", privacy_forbidden_field_count),
        ] {
            let actual_value = u64_field(row, pointer, &context)?;
            if actual_value != expected_value {
                return Err(format!(
                    "{context}{pointer} must be {expected_value}, got {actual_value}"
                ));
            }
        }

        if string_field(row, "/coverageStatus", &context)? != "full_surface_set_declared" {
            return Err(format!(
                "{context}: coverageStatus must be full_surface_set_declared"
            ));
        }
        if string_field(row, "/roundTripEvidenceStatus", &context)?
            != match string_field(asset, "/roundTripEvidence", asset_kind)? {
                "planned" => "planned_contract_only",
                other if !other.trim().is_empty() => "runtime_evidence_declared",
                _ => {
                    return Err(format!("{asset_kind}: roundTripEvidence must not be empty"));
                }
            }
        {
            return Err(format!(
                "{context}: roundTripEvidenceStatus must mirror {asset_kind}.roundTripEvidence"
            ));
        }
        let expected_privacy_status = if asset.pointer("/privacyContract").is_some() {
            "privacy_contract_enforced"
        } else {
            "not_applicable"
        };
        if string_field(row, "/privacyStatus", &context)? != expected_privacy_status {
            return Err(format!(
                "{context}: privacyStatus must be {expected_privacy_status}"
            ));
        }

        let must_clauses = u64_field(row, "/mustClauses", &context)?;
        let tested = u64_field(row, "/tested", &context)?;
        let passing = u64_field(row, "/passing", &context)?;
        let divergent = u64_field(row, "/divergent", &context)?;
        if must_clauses != REQUIRED_ASSET_MUST_CLAUSES {
            return Err(format!(
                "{context}: mustClauses must stay {REQUIRED_ASSET_MUST_CLAUSES}"
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

    if matrix_kinds != expected_kinds {
        return Err(format!(
            "assetCoverageMatrix drifted: missing={:?}, extra={:?}",
            expected_kinds.difference(&matrix_kinds).collect::<Vec<_>>(),
            matrix_kinds.difference(&expected_kinds).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn memory_anchor_backup_asset_forbids_raw_anchor_values() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let asset = backup_asset_by_kind(&manifest, "memory_anchors")?;
    let privacy = asset
        .pointer("/privacyContract")
        .ok_or_else(|| "memory_anchors backup asset must declare privacyContract".to_owned())?;

    if bool_field(
        privacy,
        "/rawAnchorValuesAllowed",
        "memory_anchors.privacyContract",
    )? {
        return Err("memory_anchors backup must not allow raw anchor values".to_owned());
    }
    for (pointer, expected) in [
        ("/valueMaterialPolicy", "hash_or_redacted_only"),
        ("/manifestRedactionClass", "hash"),
        ("/restoreValidation", "hashes_roundtrip_without_raw_values"),
    ] {
        if string_field(privacy, pointer, "memory_anchors.privacyContract")? != expected {
            return Err(format!(
                "memory_anchors.privacyContract{pointer} must be {expected}"
            ));
        }
    }

    let serialized_fields = string_set(
        array_field(
            privacy,
            "/serializedFields",
            "memory_anchors.privacyContract",
        )?,
        "memory_anchors.privacyContract.serializedFields",
    )?;
    let planned_columns = migration_memory_anchor_columns()?;
    if serialized_fields != planned_columns {
        return Err(format!(
            "memory_anchors backup serialized fields must match planned columns: missing={:?}, extra={:?}",
            planned_columns
                .difference(&serialized_fields)
                .collect::<Vec<_>>(),
            serialized_fields
                .difference(&planned_columns)
                .collect::<Vec<_>>()
        ));
    }

    let forbidden_fields = string_set(
        array_field(
            privacy,
            "/forbiddenFields",
            "memory_anchors.privacyContract",
        )?,
        "memory_anchors.privacyContract.forbiddenFields",
    )?;
    for forbidden in FORBIDDEN_MEMORY_ANCHOR_BACKUP_FIELDS {
        if !forbidden_fields.contains(*forbidden) {
            return Err(format!(
                "memory_anchors backup must forbid raw field {forbidden}"
            ));
        }
        if serialized_fields.contains(*forbidden) {
            return Err(format!(
                "memory_anchors backup serializedFields must not include raw field {forbidden}"
            ));
        }
    }
    Ok(())
}

#[test]
fn backup_failure_scenarios_make_loss_visible() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let expected_scenarios = REQUIRED_FAILURE_SCENARIOS
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<BTreeSet<_>>();
    let required_surfaces = REQUIRED_COVERAGE_SURFACES
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<BTreeSet<_>>();
    let runtime_anchors = string_set(
        array_field(&manifest, "/runtimeAnchors", MANIFEST_REL)?,
        "/runtimeAnchors",
    )?;
    let expected_failure = string_field(
        &manifest,
        "/policy/missingAssetFailure",
        "policy.missingAssetFailure",
    )?;
    let expected_hash_policy = string_field(&manifest, "/policy/hashPolicy", "policy.hashPolicy")?;
    let mut seen_scenarios = BTreeSet::new();

    for (index, scenario) in array_field(&manifest, "/failureScenarios", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("failureScenarios[{index}]");
        let scenario_id = string_field(scenario, "/scenario", &context)?;
        if !expected_scenarios.contains(scenario_id) {
            return Err(format!("{context}: unexpected scenario {scenario_id}"));
        }
        if !seen_scenarios.insert(scenario_id.to_owned()) {
            return Err(format!("{context}: duplicate scenario {scenario_id}"));
        }

        let asset_kind = string_field(scenario, "/assetKind", &context)?;
        let asset = backup_asset_by_kind(&manifest, asset_kind)?;
        let coverage_surface = string_field(scenario, "/coverageSurface", &context)?;
        if !required_surfaces.contains(coverage_surface) {
            return Err(format!(
                "{context}: unsupported coverageSurface {coverage_surface}"
            ));
        }
        let asset_surfaces = string_set(
            array_field(asset, "/coverageSurfaces", asset_kind)?,
            &format!("{asset_kind}.coverageSurfaces"),
        )?;
        if !asset_surfaces.contains(coverage_surface) {
            return Err(format!(
                "{context}: asset {asset_kind} does not cover surface {coverage_surface}"
            ));
        }

        if string_field(scenario, "/expectedFailure", &context)? != expected_failure {
            return Err(format!(
                "{context}: expectedFailure must match policy.missingAssetFailure"
            ));
        }
        if string_field(scenario, "/hashPolicy", &context)? != expected_hash_policy {
            return Err(format!(
                "{context}: hashPolicy must match policy.hashPolicy"
            ));
        }
        if string_field(scenario, "/trigger", &context)?
            .trim()
            .is_empty()
        {
            return Err(format!("{context}: trigger must not be empty"));
        }
        if string_field(scenario, "/roundTripEvidence", &context)?
            .trim()
            .is_empty()
        {
            return Err(format!("{context}: roundTripEvidence must not be empty"));
        }

        let runtime_anchor = string_field(scenario, "/expectedRuntimeAnchor", &context)?;
        if !runtime_anchors.contains(runtime_anchor) {
            return Err(format!(
                "{context}: expectedRuntimeAnchor {runtime_anchor} must be in runtimeAnchors"
            ));
        }

        if scenario_id == "raw_anchor_value_present" {
            if asset_kind != "memory_anchors" {
                return Err("raw_anchor_value_present must target memory_anchors".to_owned());
            }
            let privacy = asset.pointer("/privacyContract").ok_or_else(|| {
                "memory_anchors backup asset must declare privacyContract".to_owned()
            })?;
            if bool_field(
                privacy,
                "/rawAnchorValuesAllowed",
                "memory_anchors.privacyContract",
            )? {
                return Err(
                    "raw_anchor_value_present scenario requires rawAnchorValuesAllowed=false"
                        .to_owned(),
                );
            }
            let forbidden_fields = string_set(
                array_field(
                    privacy,
                    "/forbiddenFields",
                    "memory_anchors.privacyContract",
                )?,
                "memory_anchors.privacyContract.forbiddenFields",
            )?;
            let trigger = string_field(scenario, "/trigger", &context)?;
            if !forbidden_fields.contains(trigger) {
                return Err(format!(
                    "raw_anchor_value_present trigger {trigger} must be forbidden"
                ));
            }
            if string_field(scenario, "/privacyRequirement", &context)?
                != "rawAnchorValuesAllowed=false"
            {
                return Err(format!(
                    "{context}: privacyRequirement must stay rawAnchorValuesAllowed=false"
                ));
            }
        }
    }

    if seen_scenarios != expected_scenarios {
        return Err(format!(
            "failureScenarios drifted: expected {expected_scenarios:?}, got {seen_scenarios:?}"
        ));
    }
    Ok(())
}

#[test]
fn runtime_backup_source_still_exposes_derived_asset_hooks() -> TestResult {
    let source = read_text(BACKUP_SOURCE_REL)?;
    let manifest = read_json(MANIFEST_REL)?;
    for (index, anchor) in array_field(&manifest, "/runtimeAnchors", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let needle = anchor
            .as_str()
            .ok_or_else(|| format!("runtimeAnchors[{index}] must be a string"))?;
        if !source.contains(needle) {
            return Err(format!(
                "{BACKUP_SOURCE_REL} must still contain runtime backup anchor {needle:?}"
            ));
        }
    }
    Ok(())
}

#[test]
fn backup_doc_names_manifest_registry_runtime_and_assets() -> TestResult {
    let doc = read_text(DOC_REL)?;
    for needle in [
        MANIFEST_REL,
        MIGRATION_REGISTRY_REL,
        "bd-1n0np.23.2",
        BACKUP_SOURCE_REL,
        "records.jsonl",
        "manifest.json",
        "BackupCreateReport::derived",
        "tests/e2e_backup_restore_roundtrip.rs",
        "rawAnchorValuesAllowed=false",
        "hash_or_redacted_only",
        "hashes_roundtrip_without_raw_values",
        "raw_anchor_value",
        "Asset Coverage Matrix",
        "assetCoverageMatrix",
        "full_surface_set_declared",
        "privacy_contract_enforced",
        "declared_conformant",
        "failureScenarios",
        "missing_derived_asset",
        "corrupt_derived_asset_hash",
        "restore_manifest_rehash_mismatch",
        "raw_anchor_value_present",
        "Local Cargo fallback is not valid proof",
    ] {
        if !doc.contains(needle) {
            return Err(format!("{DOC_REL} must mention {needle:?}"));
        }
    }

    for asset_kind in migration_backup_asset_kinds()?.keys() {
        if !doc.contains(asset_kind) {
            return Err(format!(
                "{DOC_REL} must document backup asset kind {asset_kind}"
            ));
        }
    }
    Ok(())
}
