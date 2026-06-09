//! bd-1n0np.23.1 - migration sequencing registry for dueling-wizards schema
//! work.
//!
//! The registry is intentionally a plan artifact. It fails when runtime
//! migrations advance without updating the registry, or when planned allocation
//! order stops being contiguous.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const MANIFEST_REL: &str = "tests/fixtures/contracts/dueling_wizards_migration_registry.json";
const DOC_REL: &str = "docs/agent-ux/dueling-wizards/migration-sequencing.md";
const DB_MOD_REL: &str = "src/db/mod.rs";

const REQUIRED_ALLOCATIONS: &[&str] = &[
    "memory_anchors",
    "pack_candidate_impressions",
    "outcome_evidence_rows",
    "error_fingerprints",
    "memory_sentinel_specs",
    "memory_sentinel_results",
    "typed_memory_kind_sidecar",
    "attestation_bundles",
    "query_miss_ledger",
    "workspace_generations",
    "source_write_stats",
];

const REQUIRED_OWNER_BEADS: &[&str] = &[
    "bd-1n0np.2.2",
    "bd-1n0np.2.3",
    "bd-1n0np.3.2",
    "bd-1n0np.4.3",
    "bd-1n0np.6.3",
    "bd-1n0np.8.2",
    "bd-1n0np.8.5",
    "bd-1n0np.12.1",
    "bd-1n0np.16.2",
    "bd-1n0np.22.1",
];

const REQUIRED_MEMORY_ANCHOR_COLUMNS: &[&str] = &[
    "memory_id",
    "anchor_kind",
    "anchor_value_hash",
    "redacted_anchor_value",
    "confidence",
    "source",
    "provenance",
    "captured_span_hash",
    "freshness_state",
    "generation",
    "created_at",
    "updated_at",
];

const REQUIRED_MEMORY_ANCHOR_INDEXES: &[&str] = &[
    "memory_id_anchor_kind_value_hash_unique",
    "anchor_kind_value_hash_lookup",
    "freshness_state_generation_lookup",
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

fn allocation_by_id<'a>(manifest: &'a Value, id: &str) -> Result<&'a Value, String> {
    for allocation in array_field(manifest, "/allocations", MANIFEST_REL)? {
        if allocation
            .pointer("/id")
            .and_then(Value::as_str)
            .is_some_and(|allocation_id| allocation_id == id)
        {
            return Ok(allocation);
        }
    }
    Err(format!("{MANIFEST_REL}: missing allocation id {id}"))
}

fn transition_by_id<'a>(manifest: &'a Value, id: &str) -> Result<&'a Value, String> {
    for transition in array_field(manifest, "/transitionMatrix", MANIFEST_REL)? {
        if transition
            .pointer("/id")
            .and_then(Value::as_str)
            .is_some_and(|transition_id| transition_id == id)
        {
            return Ok(transition);
        }
    }
    Err(format!("{MANIFEST_REL}: missing transition row id {id}"))
}

fn compiled_migration_versions() -> Result<Vec<u64>, String> {
    let text = read_text(DB_MOD_REL)?;
    let start = text
        .find("pub const MIGRATIONS")
        .ok_or_else(|| format!("{DB_MOD_REL}: missing MIGRATIONS array"))?;
    let tail = &text[start..];
    let end = tail
        .find("];")
        .ok_or_else(|| format!("{DB_MOD_REL}: unterminated MIGRATIONS array"))?;
    let array_text = &tail[..end];

    let mut versions = Vec::new();
    for line in array_text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix('V') else {
            continue;
        };
        let digits = rest
            .chars()
            .take_while(|value| value.is_ascii_digit())
            .collect::<String>();
        if !digits.is_empty() {
            let version = digits
                .parse::<u64>()
                .map_err(|error| format!("{DB_MOD_REL}: parse migration version: {error}"))?;
            versions.push(version);
        }
    }
    if versions.is_empty() {
        return Err(format!("{DB_MOD_REL}: no migration versions found"));
    }
    Ok(versions)
}

#[test]
fn registry_identity_matches_current_runtime_tail() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    if string_field(&manifest, "/schema", MANIFEST_REL)?
        != "ee.dueling_wizards.migration_registry.v1"
    {
        return Err(format!(
            "{MANIFEST_REL}: schema must be ee.dueling_wizards.migration_registry.v1"
        ));
    }
    if string_field(&manifest, "/initiativeBead", MANIFEST_REL)? != "bd-1n0np" {
        return Err("registry must identify initiativeBead bd-1n0np".to_owned());
    }
    if string_field(&manifest, "/gateBead", MANIFEST_REL)? != "bd-1n0np.23.1" {
        return Err("registry must identify gateBead bd-1n0np.23.1".to_owned());
    }
    if string_field(&manifest, "/sourceOfTruth", MANIFEST_REL)? != "src/db/mod.rs::MIGRATIONS" {
        return Err("registry must point at src/db/mod.rs::MIGRATIONS".to_owned());
    }

    let versions = compiled_migration_versions()?;
    let compiled_tail = *versions
        .last()
        .ok_or_else(|| "compiled migrations must not be empty".to_owned())?;
    let registered_tail = u64_field(&manifest, "/currentLastCompiledMigration", MANIFEST_REL)?;
    if registered_tail != compiled_tail {
        return Err(format!(
            "registry tail V{registered_tail:03} does not match compiled tail V{compiled_tail:03}; update the registry with the runtime migration"
        ));
    }

    let next = u64_field(&manifest, "/nextPlannedMigration", MANIFEST_REL)?;
    if next != registered_tail + 1 {
        return Err(format!(
            "nextPlannedMigration must be current tail + 1: got V{next:03} after V{registered_tail:03}"
        ));
    }
    Ok(())
}

#[test]
fn planned_allocations_are_contiguous_and_complete() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let allocations = array_field(&manifest, "/allocations", MANIFEST_REL)?;
    let mut ids = BTreeSet::new();
    let mut owner_beads = BTreeSet::new();
    let mut versions = Vec::new();
    let mut first_planned_version: Option<u64> = None;

    for (index, allocation) in allocations.iter().enumerate() {
        let context = format!("allocation[{index}]");
        let id = string_field(allocation, "/id", &context)?;
        if !ids.insert(id.to_owned()) {
            return Err(format!("duplicate migration allocation id {id}"));
        }

        let version = u64_field(allocation, "/version", &context)?;
        versions.push(version);
        let expected_name_prefix = format!("V{version:03}_");
        let migration_name = string_field(allocation, "/migrationName", &context)?;
        if !migration_name.starts_with(&expected_name_prefix) {
            return Err(format!(
                "{id}: migrationName {migration_name} must start with {expected_name_prefix}"
            ));
        }

        let owner = string_field(allocation, "/ownerBead", &context)?;
        if !owner.starts_with("bd-1n0np.") {
            return Err(format!("{id}: ownerBead {owner} must belong to bd-1n0np"));
        }
        owner_beads.insert(owner.to_owned());

        let status = string_field(allocation, "/status", &context)?;
        if !matches!(status, "planned" | "implemented") {
            return Err(format!("{id}: unsupported status {status}"));
        }
        if status == "planned" && first_planned_version.is_none() {
            first_planned_version = Some(version);
        }

        for pointer in [
            "/ownerSurface",
            "/backupAssetKind",
            "/reversibleClass",
            "/idempotency",
        ] {
            if string_field(allocation, pointer, &context)?
                .trim()
                .is_empty()
            {
                return Err(format!("{id}: {pointer} must not be empty"));
            }
        }

        if array_field(allocation, "/tables", &context)?.is_empty() {
            return Err(format!("{id}: tables must not be empty"));
        }
    }

    let expected_ids = REQUIRED_ALLOCATIONS
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<BTreeSet<_>>();
    if ids != expected_ids {
        return Err(format!(
            "allocation id set drifted: missing={:?}, extra={:?}",
            expected_ids.difference(&ids).collect::<Vec<_>>(),
            ids.difference(&expected_ids).collect::<Vec<_>>()
        ));
    }

    let expected_owner_beads = REQUIRED_OWNER_BEADS
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<BTreeSet<_>>();
    if owner_beads != expected_owner_beads {
        return Err(format!(
            "owner bead set drifted: missing={:?}, extra={:?}",
            expected_owner_beads
                .difference(&owner_beads)
                .collect::<Vec<_>>(),
            owner_beads
                .difference(&expected_owner_beads)
                .collect::<Vec<_>>()
        ));
    }

    versions.sort_unstable();
    let first = *versions
        .first()
        .ok_or_else(|| "registry must contain at least one allocation".to_owned())?;
    for (offset, version) in versions.iter().enumerate() {
        let expected = first
            + u64::try_from(offset)
                .map_err(|error| format!("offset {offset} exceeds u64: {error}"))?;
        if *version != expected {
            return Err(format!(
                "migration allocations must be contiguous from V{first:03}; expected V{expected:03}, got V{version:03}"
            ));
        }
    }
    let next_planned = u64_field(&manifest, "/nextPlannedMigration", MANIFEST_REL)?;
    let first_planned_version = first_planned_version
        .ok_or_else(|| "registry must keep at least one planned allocation".to_owned())?;
    if first_planned_version != next_planned {
        return Err(format!(
            "first planned allocation must match nextPlannedMigration: first planned V{first_planned_version:03}, next V{next_planned:03}"
        ));
    }
    Ok(())
}

#[test]
fn allocations_require_backup_and_boundary_migration_coverage() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    if string_field(&manifest, "/boundaryMigrationE2e", MANIFEST_REL)?
        != "scripts/e2e_boundary_migration.sh"
    {
        return Err("registry must name scripts/e2e_boundary_migration.sh".to_owned());
    }
    if string_field(&manifest, "/backupCoverageBead", MANIFEST_REL)? != "bd-1n0np.23.2" {
        return Err("registry must name backup coverage bead bd-1n0np.23.2".to_owned());
    }
    if string_field(&manifest, "/policy/ordering", MANIFEST_REL)? != "strictly_contiguous" {
        return Err("migration ordering policy must be strictly_contiguous".to_owned());
    }
    if string_field(&manifest, "/policy/idempotency", MANIFEST_REL)? != "required" {
        return Err("migration idempotency policy must be required".to_owned());
    }

    for (index, allocation) in array_field(&manifest, "/allocations", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("allocation[{index}]");
        let id = string_field(allocation, "/id", &context)?;
        let reversible = string_field(allocation, "/reversibleClass", &context)?;
        if !matches!(reversible, "forward_only" | "reversible_where_safe") {
            return Err(format!(
                "{id}: reversibleClass must be forward_only or reversible_where_safe"
            ));
        }
        let asset_kind = string_field(allocation, "/backupAssetKind", &context)?;
        if !asset_kind
            .chars()
            .all(|value| value.is_ascii_lowercase() || value.is_ascii_digit() || value == '_')
        {
            return Err(format!(
                "{id}: backupAssetKind {asset_kind} must be snake_case ASCII"
            ));
        }
    }
    Ok(())
}

#[test]
fn transition_matrix_mirrors_allocations_and_runtime_status() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let allocations = array_field(&manifest, "/allocations", MANIFEST_REL)?;
    let transition_rows = array_field(&manifest, "/transitionMatrix", MANIFEST_REL)?;
    if transition_rows.len() != allocations.len() {
        return Err(format!(
            "transitionMatrix must have one row per allocation: {} rows for {} allocations",
            transition_rows.len(),
            allocations.len()
        ));
    }

    let compiled_tail = u64_field(&manifest, "/currentLastCompiledMigration", MANIFEST_REL)?;
    let mut transition_ids = BTreeSet::new();
    for (index, transition) in transition_rows.iter().enumerate() {
        let context = format!("transitionMatrix[{index}]");
        let id = string_field(transition, "/id", &context)?;
        if !transition_ids.insert(id.to_owned()) {
            return Err(format!("duplicate transitionMatrix row id {id}"));
        }
    }

    for allocation in allocations {
        let id = string_field(allocation, "/id", "allocation")?;
        let transition = transition_by_id(&manifest, id)?;
        let version = u64_field(allocation, "/version", id)?;
        let status = string_field(allocation, "/status", id)?;
        let migration_name = string_field(allocation, "/migrationName", id)?;

        if u64_field(transition, "/version", id)? != version {
            return Err(format!(
                "{id}: transitionMatrix version must mirror allocation version"
            ));
        }
        if string_field(transition, "/status", id)? != status {
            return Err(format!(
                "{id}: transitionMatrix status must mirror allocation status"
            ));
        }
        if string_field(transition, "/proofPosture", id)? != "rch_only_no_local_fallback" {
            return Err(format!(
                "{id}: transition proofPosture must forbid local Cargo fallback"
            ));
        }

        match status {
            "implemented" => {
                if version > compiled_tail {
                    return Err(format!(
                        "{id}: implemented migration V{version:03} is ahead of compiled tail V{compiled_tail:03}"
                    ));
                }
                if string_field(transition, "/runtimeRule", id)? != "compiled_migration_present" {
                    return Err(format!(
                        "{id}: implemented row must require compiled migration"
                    ));
                }
                if string_field(transition, "/migrationConstant", id)? != migration_name {
                    return Err(format!(
                        "{id}: implemented row must name compiled migration {migration_name}"
                    ));
                }
                for pointer in ["/boundaryMigrationEvidence", "/backupCoverageEvidence"] {
                    if string_field(transition, pointer, id)? != "required_and_current" {
                        return Err(format!("{id}: {pointer} must be required_and_current"));
                    }
                }
            }
            "planned" => {
                if version <= compiled_tail {
                    return Err(format!(
                        "{id}: planned migration V{version:03} must stay ahead of compiled tail V{compiled_tail:03}"
                    ));
                }
                if string_field(transition, "/runtimeRule", id)? != "planned_allocation_only" {
                    return Err(format!("{id}: planned row must stay registry-only"));
                }
                for pointer in [
                    "/migrationConstant",
                    "/boundaryMigrationEvidence",
                    "/backupCoverageEvidence",
                ] {
                    if string_field(transition, pointer, id)? != "required_before_implemented" {
                        return Err(format!(
                            "{id}: {pointer} must be required_before_implemented"
                        ));
                    }
                }
            }
            other => return Err(format!("{id}: unsupported status {other}")),
        }
    }
    Ok(())
}

#[test]
fn memory_anchor_allocation_declares_schema_privacy_and_indexes() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let allocation = allocation_by_id(&manifest, "memory_anchors")?;
    let shape = allocation
        .pointer("/plannedShape")
        .ok_or_else(|| "memory_anchors allocation must declare plannedShape".to_owned())?;

    let expected_columns = REQUIRED_MEMORY_ANCHOR_COLUMNS
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<BTreeSet<_>>();
    let actual_columns = string_set(
        array_field(shape, "/columns", "memory_anchors.plannedShape")?,
        "memory_anchors.plannedShape.columns",
    )?;
    if actual_columns != expected_columns {
        return Err(format!(
            "memory_anchors columns drifted: expected {expected_columns:?}, got {actual_columns:?}"
        ));
    }

    let expected_indexes = REQUIRED_MEMORY_ANCHOR_INDEXES
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<BTreeSet<_>>();
    let actual_indexes = string_set(
        array_field(shape, "/indexes", "memory_anchors.plannedShape")?,
        "memory_anchors.plannedShape.indexes",
    )?;
    if actual_indexes != expected_indexes {
        return Err(format!(
            "memory_anchors indexes drifted: expected {expected_indexes:?}, got {actual_indexes:?}"
        ));
    }

    for (pointer, expected) in [
        ("/anchorValueStorage", "hash_required_raw_value_forbidden"),
        ("/freshnessMutation", "rank_down_only_no_tombstone"),
        ("/meshExport", "redacted_or_hashed_values_only"),
        ("/writePosture", "append_or_upsert_by_generation"),
    ] {
        if string_field(shape, pointer, "memory_anchors.plannedShape")? != expected {
            return Err(format!(
                "memory_anchors.plannedShape{pointer} must be {expected}"
            ));
        }
    }
    Ok(())
}

#[test]
fn registry_doc_names_manifest_tail_versions_and_owner_beads() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let doc = read_text(DOC_REL)?;
    for needle in [
        MANIFEST_REL,
        "bd-1n0np.23.1",
        "scripts/e2e_boundary_migration.sh",
        "Local Cargo fallback is not valid proof",
    ] {
        if !doc.contains(needle) {
            return Err(format!("{DOC_REL} must mention {needle:?}"));
        }
    }

    let registered_tail = u64_field(&manifest, "/currentLastCompiledMigration", MANIFEST_REL)?;
    let next_planned = u64_field(&manifest, "/nextPlannedMigration", MANIFEST_REL)?;
    let tail_label = format!("V{registered_tail:03}");
    let next_label = format!("V{next_planned:03}");
    for needle in [
        format!("current compiled migration tail in `src/db/mod.rs` is `{tail_label}`"),
        format!("planned allocation starts at `{next_label}`"),
        format!("If the compiled tail moves past `{tail_label}`"),
        format!("the current compiled tail (`{tail_label}`"),
    ] {
        if !doc.contains(&needle) {
            return Err(format!(
                "{DOC_REL} must document manifest-derived tail phrase {needle:?}"
            ));
        }
    }

    for transition in array_field(&manifest, "/transitionMatrix", MANIFEST_REL)? {
        if string_field(transition, "/status", "transitionMatrix")? == "implemented" {
            let migration_constant =
                string_field(transition, "/migrationConstant", "transitionMatrix")?;
            if !doc.contains(migration_constant) {
                return Err(format!(
                    "{DOC_REL} must document implemented migration constant {migration_constant}"
                ));
            }
        }
    }

    for id in REQUIRED_ALLOCATIONS {
        if !doc.contains(id) {
            return Err(format!("{DOC_REL} must document allocation {id}"));
        }
    }
    for owner in REQUIRED_OWNER_BEADS {
        if !doc.contains(owner) {
            return Err(format!("{DOC_REL} must document owner bead {owner}"));
        }
    }
    for needle in [
        "memory_id_anchor_kind_value_hash_unique",
        "hash_required_raw_value_forbidden",
        "rank_down_only_no_tombstone",
        "redacted_or_hashed_values_only",
    ] {
        if !doc.contains(needle) {
            return Err(format!("{DOC_REL} must document {needle:?}"));
        }
    }
    Ok(())
}
