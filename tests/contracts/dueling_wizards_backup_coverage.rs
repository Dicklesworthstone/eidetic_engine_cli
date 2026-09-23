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

/// The named per-asset must-clauses (bd-vxrcu): the six coverage surfaces plus
/// the three obligations the manifest's policy declares for every asset
/// (blake3 hashing, missing assets degrade instead of vanishing, side-path
/// restore). The fixture's `mustClauseList` must equal this list, and the
/// required count is its length. No clause is added to fit a number: a new one
/// needs a repo spec that requires it, cited beside it.
const MUST_CLAUSE_LIST: &[&str] = &[
    "backup_create",
    "backup_inspect",
    "backup_verify",
    "backup_restore",
    "manifest_rehash",
    "roundtrip_e2e",
    "hash_blake3",
    "missing_asset_degraded",
    "side_path_restore",
];
const REQUIRED_ASSET_MUST_CLAUSES: u64 = MUST_CLAUSE_LIST.len() as u64;
/// With nine clauses this floor admits only 9 of 9: 8 of 9 scores 888.
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

    let gate = EvidenceGate::from_manifest(&manifest)?;
    let mut compliance_errors = Vec::new();
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

        // Collected, not returned: one bad row must not hide the next one.
        if let Err(error) = row_compliance_error(row, &context, &gate) {
            compliance_errors.push(format!("{asset_kind}: {error}"));
        }
    }

    if matrix_kinds != expected_kinds {
        return Err(format!(
            "assetCoverageMatrix drifted: missing={:?}, extra={:?}",
            expected_kinds.difference(&matrix_kinds).collect::<Vec<_>>(),
            matrix_kinds.difference(&expected_kinds).collect::<Vec<_>>()
        ));
    }
    if !compliance_errors.is_empty() {
        return Err(format!(
            "assetCoverageMatrix compliance claims disagree with their evidence:\n{}",
            compliance_errors.join("\n")
        ));
    }
    Ok(())
}

/// The values `complianceStatus` may hold. bd-nwyir added the pending value:
/// until then the field had ONE legal value, so "11 of 11 conformant" restated
/// the row count instead of measuring anything. bd-vxrcu added the partial
/// value, for a row whose runtime evidence covers some named clauses but not
/// all of them; without it such a row had no legal state.
const DECLARED_CONFORMANT: &str = "declared_conformant";
const NOT_CONFORMANT_RUNTIME_PARTIAL: &str = "not_conformant_runtime_partial";
const NOT_CONFORMANT_EVIDENCE_PENDING: &str = "not_conformant_evidence_pending";

/// What a row's cited evidence is checked against: each named clause's source
/// anchors, and the runtime backup source that must hold every cited test.
///
/// A clause counts as exercised by a test when every token of at least one of
/// its anchor groups appears in that test's body. That is a necessary
/// condition, not a sufficient one: it proves the test reaches the clause's
/// entry point, not how strongly it asserts on it. It does make one claim
/// impossible, which is the point: a row cannot credit a clause to a test that
/// never touches it.
struct EvidenceGate {
    anchors: BTreeMap<String, Vec<Vec<String>>>,
    source: String,
}

impl EvidenceGate {
    fn from_manifest(manifest: &Value) -> Result<Self, String> {
        let names = array_field(manifest, "/mustClauseList", MANIFEST_REL)?
            .iter()
            .map(|value| value.as_str().unwrap_or_default())
            .collect::<Vec<_>>();
        if names != MUST_CLAUSE_LIST {
            return Err(format!(
                "mustClauseList must name exactly {MUST_CLAUSE_LIST:?}, got {names:?}"
            ));
        }
        let declared = manifest
            .pointer("/mustClauseAnchors")
            .and_then(Value::as_object)
            .ok_or_else(|| "mustClauseAnchors missing or not an object".to_owned())?;
        let mut anchors = BTreeMap::new();
        for (clause, groups) in declared {
            let context = format!("mustClauseAnchors.{clause}");
            let groups = groups
                .as_array()
                .filter(|groups| !groups.is_empty())
                .ok_or_else(|| format!("{context} must be a non-empty array of groups"))?;
            let mut parsed = Vec::new();
            for (index, group) in groups.iter().enumerate() {
                let tokens = group
                    .as_array()
                    .filter(|tokens| !tokens.is_empty())
                    .ok_or_else(|| format!("{context}[{index}] must be a non-empty array"))?;
                parsed.push(
                    string_set(tokens, &format!("{context}[{index}]"))?
                        .into_iter()
                        .collect(),
                );
            }
            anchors.insert(clause.clone(), parsed);
        }
        let anchored = anchors.keys().map(String::as_str).collect::<BTreeSet<_>>();
        if anchored != MUST_CLAUSE_LIST.iter().copied().collect::<BTreeSet<_>>() {
            return Err(format!(
                "mustClauseAnchors must anchor exactly the named clauses, got {anchored:?}"
            ));
        }
        Ok(Self {
            anchors,
            source: read_text(BACKUP_SOURCE_REL)?,
        })
    }

    /// The body of `#[test] fn {name}(`, up to the next item at module indent.
    fn test_body(&self, name: &str) -> Result<&str, String> {
        let start = self
            .source
            .find(&format!("fn {name}("))
            .ok_or_else(|| format!("cited test {name} does not exist in {BACKUP_SOURCE_REL}"))?;
        if !self.source[..start].trim_end().ends_with("#[test]") {
            return Err(format!("cited test {name} is not a #[test] fn"));
        }
        let rest = &self.source[start..];
        let end = ["\n    #[", "\n    fn ", "\n}"]
            .iter()
            .filter_map(|marker| rest.find(marker))
            .min()
            .unwrap_or(rest.len());
        Ok(&rest[..end])
    }

    fn exercises(&self, body: &str, clause: &str) -> bool {
        self.anchors.get(clause).is_some_and(|groups| {
            groups
                .iter()
                .any(|group| group.iter().all(|token| body.contains(token.as_str())))
        })
    }
}

/// The clauses a row's `evidenceTests` actually cover. Every cited test must
/// exist as a `#[test]` fn and must exercise every clause credited to it, and
/// `coveredClauses` must equal the union. A row that cites nothing covers
/// nothing.
fn cited_coverage(
    row: &Value,
    context: &str,
    gate: &EvidenceGate,
) -> Result<BTreeSet<String>, String> {
    let Some(tests) = row.pointer("/evidenceTests") else {
        if row.pointer("/coveredClauses").is_some() {
            return Err(format!(
                "{context}: coveredClauses is declared without any evidenceTests"
            ));
        }
        return Ok(BTreeSet::new());
    };
    let tests = tests
        .as_array()
        .ok_or_else(|| format!("{context}: evidenceTests must be an array"))?;
    let mut covered = BTreeSet::new();
    for (index, entry) in tests.iter().enumerate() {
        let entry_context = format!("{context}.evidenceTests[{index}]");
        let name = string_field(entry, "/test", &entry_context)?;
        let body = gate
            .test_body(name)
            .map_err(|error| format!("{entry_context}: {error}"))?;
        let clauses = string_set(
            array_field(entry, "/clauses", &entry_context)?,
            &format!("{entry_context}.clauses"),
        )?;
        if clauses.is_empty() {
            return Err(format!(
                "{entry_context}: a cited test must credit a clause"
            ));
        }
        for clause in clauses {
            if !MUST_CLAUSE_LIST.contains(&clause.as_str()) {
                return Err(format!(
                    "{entry_context}: {clause} is not a named must-clause"
                ));
            }
            if !gate.exercises(body, &clause) {
                return Err(format!(
                    "{entry_context}: {name} does not exercise {clause}: none of its \
                     anchor groups appears in the test body"
                ));
            }
            covered.insert(clause);
        }
    }
    let declared = string_set(
        array_field(row, "/coveredClauses", context)?,
        &format!("{context}.coveredClauses"),
    )?;
    if declared != covered {
        return Err(format!(
            "{context}: coveredClauses must equal the clauses its evidenceTests cover: \
             declared {declared:?}, cited {covered:?}"
        ));
    }
    Ok(covered)
}

fn require_pending_bead(row: &Value, context: &str, status: &str) -> TestResult {
    let pending_on = row
        .pointer("/evidencePendingOn")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !pending_on.starts_with("bd-") {
        return Err(format!(
            "{context}: {status} must name the bead its missing evidence is pending on in \
             evidencePendingOn, got {pending_on:?}"
        ));
    }
    Ok(())
}

/// Check one matrix row's compliance claim against its counters, its
/// round-trip evidence status, and the tests it cites.
///
/// `declared_conformant` needs runtime evidence whose cited tests cover every
/// named clause, with full counters. `not_conformant_runtime_partial` needs
/// runtime evidence covering some clauses but not all, with `tested` equal to
/// the covered count and a bead owning the rest. `not_conformant_evidence_pending`
/// is legal only while evidence is planned, cites no tests, and names a bead;
/// its counters need only be internally consistent.
fn row_compliance_error(row: &Value, context: &str, gate: &EvidenceGate) -> TestResult {
    let must_clauses = u64_field(row, "/mustClauses", context)?;
    let tested = u64_field(row, "/tested", context)?;
    let passing = u64_field(row, "/passing", context)?;
    let divergent = u64_field(row, "/divergent", context)?;
    if must_clauses != REQUIRED_ASSET_MUST_CLAUSES {
        return Err(format!(
            "{context}: mustClauses must be {REQUIRED_ASSET_MUST_CLAUSES}, the length of the named \
             clause list"
        ));
    }

    let score_milli = u64_field(row, "/scoreMilli", context)?;
    let computed_score = passing * 1000 / must_clauses;
    if score_milli != computed_score {
        return Err(format!(
            "{context}: scoreMilli must be {computed_score}, got {score_milli}"
        ));
    }

    let runtime_evidence =
        string_field(row, "/roundTripEvidenceStatus", context)? == "runtime_evidence_declared";
    let covered = cited_coverage(row, context, gate)?.len() as u64;
    match string_field(row, "/complianceStatus", context)? {
        DECLARED_CONFORMANT => {
            if !runtime_evidence {
                return Err(format!(
                    "{context}: {DECLARED_CONFORMANT} requires roundTripEvidenceStatus \
                     runtime_evidence_declared; conformance cannot rest on planned-only evidence"
                ));
            }
            if covered != must_clauses {
                return Err(format!(
                    "{context}: {DECLARED_CONFORMANT} needs every named clause covered by a \
                     cited test; covered {covered} of {must_clauses}"
                ));
            }
            if tested != must_clauses || passing != tested || divergent != 0 {
                return Err(format!(
                    "{context}: tested, passing, and divergent must describe full conformance"
                ));
            }
            if score_milli < MIN_MUST_COVERAGE_MILLI {
                return Err(format!(
                    "{context}: scoreMilli {score_milli} is below {MIN_MUST_COVERAGE_MILLI}"
                ));
            }
        }
        NOT_CONFORMANT_RUNTIME_PARTIAL => {
            if !runtime_evidence {
                return Err(format!(
                    "{context}: {NOT_CONFORMANT_RUNTIME_PARTIAL} is only legal with \
                     roundTripEvidenceStatus runtime_evidence_declared"
                ));
            }
            require_pending_bead(row, context, NOT_CONFORMANT_RUNTIME_PARTIAL)?;
            if covered == 0 || covered >= must_clauses {
                return Err(format!(
                    "{context}: {NOT_CONFORMANT_RUNTIME_PARTIAL} needs covered clauses strictly \
                     between 0 and {must_clauses}, got {covered}"
                ));
            }
            if tested != covered || passing != tested || divergent != 0 {
                return Err(format!(
                    "{context}: tested and passing must equal the {covered} covered clauses, \
                     with divergent 0"
                ));
            }
        }
        NOT_CONFORMANT_EVIDENCE_PENDING => {
            if runtime_evidence {
                return Err(format!(
                    "{context}: {NOT_CONFORMANT_EVIDENCE_PENDING} is only legal while \
                     roundTripEvidenceStatus is planned_contract_only"
                ));
            }
            require_pending_bead(row, context, NOT_CONFORMANT_EVIDENCE_PENDING)?;
            if covered != 0 {
                return Err(format!(
                    "{context}: {NOT_CONFORMANT_EVIDENCE_PENDING} cannot cite evidenceTests"
                ));
            }
            if tested > must_clauses || passing > tested || divergent > tested {
                return Err(format!(
                    "{context}: tested, passing, and divergent must be consistent with mustClauses"
                ));
            }
        }
        other => {
            return Err(format!(
                "{context}: complianceStatus must be one of {DECLARED_CONFORMANT}, \
                 {NOT_CONFORMANT_RUNTIME_PARTIAL} or {NOT_CONFORMANT_EVIDENCE_PENDING}, got {other}"
            ));
        }
    }
    Ok(())
}

/// A gate over a synthetic source: every clause anchored by `anchor_<clause>`,
/// one test that calls them all, one that calls only `backup_create`'s, and a
/// helper that is not a test.
fn unit_gate() -> EvidenceGate {
    let anchors = MUST_CLAUSE_LIST
        .iter()
        .map(|clause| (clause.to_string(), vec![vec![format!("anchor_{clause}(")]]))
        .collect();
    let all_calls = MUST_CLAUSE_LIST
        .iter()
        .map(|clause| format!("        anchor_{clause}();\n"))
        .collect::<String>();
    let source = format!(
        "    #[test]\n    fn exercises_everything() {{\n{all_calls}    }}\n\n    \
         #[test]\n    fn exercises_create_only() {{\n        anchor_backup_create();\n    }}\n\n    \
         fn not_a_test() {{\n{all_calls}    }}\n}}\n"
    );
    EvidenceGate { anchors, source }
}

/// A row whose counters agree with `tested`, citing `cited` as (test, clauses).
fn unit_row(compliance: &str, evidence: &str, cited: &[(&str, &[&str])], tested: u64) -> Value {
    let mut row = serde_json::json!({
        "complianceStatus": compliance,
        "roundTripEvidenceStatus": evidence,
        "mustClauses": REQUIRED_ASSET_MUST_CLAUSES,
        "tested": tested,
        "passing": tested,
        "divergent": 0,
        "scoreMilli": tested * 1000 / REQUIRED_ASSET_MUST_CLAUSES,
        "evidencePendingOn": "bd-example",
    });
    if !cited.is_empty() {
        row["evidenceTests"] = cited
            .iter()
            .map(|(test, clauses)| serde_json::json!({"test": test, "clauses": clauses}))
            .collect();
        row["coveredClauses"] = serde_json::json!(
            cited
                .iter()
                .flat_map(|(_, clauses)| clauses.iter().copied())
                .collect::<BTreeSet<_>>()
        );
    }
    row
}

fn expect_rejected(result: TestResult, arm: &str, needle: &str) -> TestResult {
    if result.as_ref().is_err_and(|error| error.contains(needle)) {
        Ok(())
    } else {
        Err(format!(
            "{arm} must be rejected with {needle:?}, got {result:?}"
        ))
    }
}

#[test]
fn compliance_status_is_tied_to_counters_and_cited_evidence() -> TestResult {
    let gate = unit_gate();
    let full = REQUIRED_ASSET_MUST_CLAUSES;
    let runtime = "runtime_evidence_declared";
    let planned = "planned_contract_only";
    let everything: &[(&str, &[&str])] = &[("exercises_everything", MUST_CLAUSE_LIST)];
    let create_only: &[(&str, &[&str])] = &[("exercises_create_only", &["backup_create"])];
    let check = |row: &Value, arm: &str| row_compliance_error(row, arm, &gate);

    // A: conformant, runtime evidence covering every named clause, full counters.
    check(
        &unit_row(DECLARED_CONFORMANT, runtime, everything, full),
        "arm A",
    )?;
    // B: pending over planned evidence, nothing cited, nothing tested.
    check(
        &unit_row(NOT_CONFORMANT_EVIDENCE_PENDING, planned, &[], 0),
        "arm B",
    )?;
    // H: partial, runtime evidence covering one clause.
    check(
        &unit_row(NOT_CONFORMANT_RUNTIME_PARTIAL, runtime, create_only, 1),
        "arm H",
    )?;

    // C: pending cannot be claimed over declared runtime evidence.
    expect_rejected(
        check(
            &unit_row(NOT_CONFORMANT_EVIDENCE_PENDING, runtime, &[], 0),
            "arm C",
        ),
        "arm C",
        "only legal while",
    )?;
    // D: no fourth value.
    expect_rejected(
        check(&unit_row("bogus", planned, &[], 0), "arm D"),
        "arm D",
        "complianceStatus must be one of",
    )?;
    // E: full coverage cited but one clause not passing.
    let mut e = unit_row(DECLARED_CONFORMANT, runtime, everything, full);
    e["passing"] = serde_json::json!(full - 1);
    e["scoreMilli"] = serde_json::json!((full - 1) * 1000 / full);
    expect_rejected(check(&e, "arm E"), "arm E", "full conformance")?;
    // F: the bd-nwyir defect -- conformant on planned-only evidence.
    expect_rejected(
        check(&unit_row(DECLARED_CONFORMANT, planned, &[], full), "arm F"),
        "arm F",
        "cannot rest on planned-only evidence",
    )?;
    // G: a pending row must say what it is pending on.
    let mut g = unit_row(NOT_CONFORMANT_EVIDENCE_PENDING, planned, &[], 0);
    if let Some(fields) = g.as_object_mut() {
        fields.remove("evidencePendingOn");
    }
    expect_rejected(check(&g, "arm G"), "arm G", "evidencePendingOn")?;
    // I: a partial row crediting a clause its cited test never exercises.
    let overclaimed: &[(&str, &[&str])] = &[(
        "exercises_create_only",
        &["backup_create", "backup_inspect"],
    )];
    expect_rejected(
        check(
            &unit_row(NOT_CONFORMANT_RUNTIME_PARTIAL, runtime, overclaimed, 2),
            "arm I",
        ),
        "arm I",
        "does not exercise backup_inspect",
    )?;
    // J: conformant with partial coverage, counters inflated to full.
    expect_rejected(
        check(
            &unit_row(DECLARED_CONFORMANT, runtime, create_only, full),
            "arm J",
        ),
        "arm J",
        "covered 1 of",
    )?;
    // K: a cited test that does not exist.
    let missing: &[(&str, &[&str])] = &[("no_such_test", &["backup_create"])];
    expect_rejected(
        check(
            &unit_row(NOT_CONFORMANT_RUNTIME_PARTIAL, runtime, missing, 1),
            "arm K",
        ),
        "arm K",
        "does not exist",
    )?;
    // L: a cited fn that is not a #[test].
    let helper: &[(&str, &[&str])] = &[("not_a_test", &["backup_create"])];
    expect_rejected(
        check(
            &unit_row(NOT_CONFORMANT_RUNTIME_PARTIAL, runtime, helper, 1),
            "arm L",
        ),
        "arm L",
        "is not a #[test] fn",
    )?;
    // M: partial counters that disagree with the covered set.
    expect_rejected(
        check(
            &unit_row(NOT_CONFORMANT_RUNTIME_PARTIAL, runtime, create_only, 2),
            "arm M",
        ),
        "arm M",
        "must equal the 1 covered clauses",
    )?;
    // N: partial is not a place to park full coverage.
    expect_rejected(
        check(
            &unit_row(NOT_CONFORMANT_RUNTIME_PARTIAL, runtime, everything, full),
            "arm N",
        ),
        "arm N",
        "strictly between",
    )?;
    // O: pending cannot cite tests.
    expect_rejected(
        check(
            &unit_row(NOT_CONFORMANT_EVIDENCE_PENDING, planned, create_only, 1),
            "arm O",
        ),
        "arm O",
        "cannot cite evidenceTests",
    )?;
    // P: coveredClauses must be the union the citations prove, not a wish.
    let mut p = unit_row(NOT_CONFORMANT_RUNTIME_PARTIAL, runtime, create_only, 1);
    p["coveredClauses"] = serde_json::json!(["backup_verify"]);
    expect_rejected(check(&p, "arm P"), "arm P", "coveredClauses must equal")?;
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
        "not_conformant_evidence_pending",
        "not_conformant_runtime_partial",
        "evidencePendingOn",
        "mustClauseList",
        "mustClauseAnchors",
        "evidenceTests",
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

/// GRANDFATHERED UNRESOLVED CLAIMS -- **not** approved exceptions. bd-nwyir.
///
/// Each entry would be an asset kind whose `complianceStatus` says
/// `declared_conformant` while its `roundTripEvidenceStatus` says
/// `planned_contract_only`. That pairing is the defect: a surface reporting an
/// asset as proven while recording, in the field beside it, that nothing was
/// ever round-tripped.
///
/// EMPTY, AND IT STAYS EMPTY. This list once held all eleven rows as open
/// questions. The recorded bd-nwyir decision corrected every one of those
/// claims to `not_conformant_evidence_pending`, each naming the bead its
/// evidence is pending on. Nothing may be added back: a row earns
/// `declared_conformant` by producing runtime round-trip evidence, never by
/// appearing here.
///
/// Entries would carry the evidence status beside each kind, because a bare
/// count cannot tell a reader whether a row was fixed or merely swapped for a
/// different offender.
const GRANDFATHERED_UNRESOLVED_CONFORMANCE_CLAIMS: &[(&str, &str)] = &[];

/// A surface may not be declared conformant while its own round-trip evidence
/// field says nothing was round-tripped. bd-nwyir.
///
/// The existing gate checks that `roundTripEvidenceStatus` correctly MIRRORS
/// the asset's `roundTripEvidence`. It never checks `complianceStatus` against
/// either, so those two cannot disagree and the pair is structurally incapable
/// of detecting the state it exists to detect. Measured 2026-09-19: 11 of 11
/// rows declared conformant, 11 of 11 on planned-only evidence. Corrected
/// under bd-nwyir: 0 of 11.
///
/// RATCHETS DOWN ONLY, and it is now at zero: any offender fails, and so would
/// a grandfathered row that got resolved while the list still named it. A
/// baseline that only blocks growth becomes a floor nobody ever descends.
#[test]
fn conformance_is_never_declared_on_planned_only_evidence() -> TestResult {
    // Overridable so the ratchet can be proven against a real copy of the
    // matrix carrying a planted offender, leaving the repo unmodified.
    let manifest_path = match std::env::var("EE_BACKUP_COVERAGE_MANIFEST_PATH") {
        Ok(path) => PathBuf::from(path),
        Err(_) => repo_root().join(MANIFEST_REL),
    };
    let raw = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("read {}: {error}", manifest_path.display()))?;
    let manifest: Value =
        serde_json::from_str(&raw).map_err(|error| format!("parse backup coverage: {error}"))?;

    let rows = manifest
        .get("assetCoverageMatrix")
        .and_then(Value::as_array)
        .ok_or_else(|| "assetCoverageMatrix missing or not an array".to_owned())?;

    // EMPTY-WORLD GUARD. Zero rows is what a rename, a moved file or a broken
    // invocation produces, and every comparison below would then pass
    // vacuously. A consistency gate that goes green while measuring nothing is
    // worse than no gate, because it reports success.
    if rows.is_empty() {
        return Err(format!(
            "assetCoverageMatrix in {} parsed as ZERO rows; this gate measured \
             nothing and must not report success",
            manifest_path.display()
        ));
    }

    let mut observed: BTreeMap<String, String> = BTreeMap::new();
    for row in rows {
        let kind = string_field(row, "/assetKind", "assetCoverageMatrix row")?;
        let compliance = string_field(row, "/complianceStatus", kind)?;
        let evidence = string_field(row, "/roundTripEvidenceStatus", kind)?;
        if compliance == "declared_conformant" && evidence == "planned_contract_only" {
            observed.insert(kind.to_owned(), evidence.to_owned());
        }
    }

    let recorded: BTreeMap<&str, &str> = GRANDFATHERED_UNRESOLVED_CONFORMANCE_CLAIMS
        .iter()
        .copied()
        .collect();

    let mut undeclared = Vec::new();
    for (kind, evidence) in &observed {
        match recorded.get(kind.as_str()) {
            None => undeclared.push(format!(
                "  {kind}: declared conformant on {evidence} evidence, and it is not \
                 one of the {} rows grandfathered unresolved under bd-nwyir",
                recorded.len()
            )),
            Some(expected) if *expected != evidence.as_str() => undeclared.push(format!(
                "  {kind}: grandfathered at evidence {expected} but the matrix now says \
                 {evidence}; the row changed rather than being resolved"
            )),
            Some(_) => {}
        }
    }

    // RATCHET: a grandfathered row that no longer offends must leave the list.
    let mut resolved = Vec::new();
    for kind in recorded.keys() {
        if !observed.contains_key(*kind) {
            resolved.push(format!(
                "  {kind}: no longer declares conformance on planned-only evidence. \
                 Remove it from GRANDFATHERED_UNRESOLVED_CONFORMANCE_CLAIMS -- the \
                 ceiling must come down when the debt does"
            ));
        }
    }

    if !undeclared.is_empty() || !resolved.is_empty() {
        return Err(format!(
            "backup coverage conformance claims disagree with their own evidence.\n\
             NEW OR CHANGED OFFENDERS (declared conformant while round-trip evidence \
             is planned-only):\n{}\n\
             RESOLVED, SO THE BASELINE MUST SHRINK:\n{}",
            if undeclared.is_empty() {
                "  (none)".to_owned()
            } else {
                undeclared.join("\n")
            },
            if resolved.is_empty() {
                "  (none)".to_owned()
            } else {
                resolved.join("\n")
            },
        ));
    }

    Ok(())
}
