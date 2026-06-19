//! bd-7lvbg.4 — schema-drift contract for the output-governor
//! truncation-point registry (ADR 0063 §2).
//!
//! Forward direction: every public envelope schema in `docs/schemas/`
//! that exposes a top-level `data` array-of-objects (a "list-like"
//! surface) must either declare a truncation point in
//! `OUTPUT_TRUNCATION_REGISTRY` or carry a documented exemption here.
//! Adding a new list-like schema without deciding its governor posture
//! fails this test — the failure message says exactly what to do.
//!
//! Reverse direction: registry entries must be well-formed (an id or
//! command, a non-empty array path, a position key), unique per
//! surface, and reference schemas that exist in `docs/schemas/`
//! (modulo the pinned, pre-existing documentation gaps).
//!
//! Nested truncation points (pack `data.pack.skipped`, recall
//! `data.recall.items`) sit below the top-level detector by design;
//! they are covered by their own surface contracts in
//! `governor_surfaces.rs`. Pack `data.pack.items[]` being absent from
//! the registry is a hard rule pinned by
//! `pack_items_are_never_a_registered_truncation_point`.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use ee::output::OUTPUT_TRUNCATION_REGISTRY;
use serde_json::Value as JsonValue;

type TestResult = Result<(), String>;

/// List-like schemas that intentionally do NOT declare a truncation
/// point. Every entry needs a reason an agent can act on; stale
/// entries (schema gone, or schema later governed) fail the test so
/// the table cannot rot.
const GOVERNOR_EXEMPT_SCHEMAS: &[(&str, &str)] = &[
    (
        "ee.bootstrap.docs.apply.v1",
        "one-shot bootstrap report bounded by the docs corpus; not a hot agent-loop surface",
    ),
    (
        "ee.bootstrap.docs.run.v1",
        "one-shot bootstrap report bounded by the docs corpus; not a hot agent-loop surface",
    ),
    (
        "ee.capabilities.v1",
        "feature-posture reflection must stay complete; truncating capability tables would \
         misreport feature availability to harnesses",
    ),
    (
        "ee.completion_audit.report.v1",
        "audit verdict depends on the complete evidence table; a truncated table would corrupt \
         the verdict",
    ),
    (
        "ee.completion_audit.report.v2",
        "audit verdict depends on the complete evidence table; a truncated table would corrupt \
         the verdict",
    ),
    (
        "ee.conflict.v1",
        "conflict pairs/clusters are decision-critical paired data with no defined truncation \
         semantics yet",
    ),
    (
        "ee.curate.disposition.v1",
        "disposition decisions and structural adjustments must be reported whole; dropping rows \
         could hide an applied mutation",
    ),
    (
        "ee.doctor.v1",
        "dropping checks would hide failures and their repair commands; doctor exposes --quick \
         and --robot-triage for size control instead",
    ),
    (
        "ee.export.report.v1",
        "artifact manifest must list every written file; a partial manifest orphans artifacts",
    ),
    (
        "ee.import.cass.v1",
        "session import summary is already bounded by --limit at the source",
    ),
    (
        "ee.mcp.manifest.v1",
        "MCP tool manifests must be complete for client registration",
    ),
    (
        "ee.status.v1",
        "derivedAssets is a small bounded posture set, not an unbounded listing",
    ),
    (
        "ee.swarm_next_action.v1",
        "swarm surfaces deliberately deferred pending the bd-kua65 fields-preset work (same \
         posture as swarm brief)",
    ),
    (
        "ee.workspace_hygiene.v1",
        "carries its own bounded-output contract (workspace_hygiene_output_truncated) instead \
         of the governor",
    ),
];

/// Registry schema ids whose `docs/schemas/` JSON Schema file does not
/// exist yet. Pre-existing documentation debt pinned so NEW dangling
/// ids still fail; shrink this list, never grow it.
const DOCS_SCHEMA_GAPS: &[&str] = &[];

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn schemas_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("schemas")
}

/// One detected list-like surface: a top-level `data` property that is
/// an array of objects.
struct ListLikeSurface {
    schema_id: String,
    command: Option<String>,
    array_keys: Vec<String>,
    file_name: String,
}

fn is_array_of_objects(property: &JsonValue) -> bool {
    let is_array_type = match property.get("type") {
        Some(JsonValue::String(kind)) => kind == "array",
        Some(JsonValue::Array(kinds)) => kinds.iter().any(|kind| kind.as_str() == Some("array")),
        _ => false,
    };
    if !is_array_type {
        return false;
    }
    let Some(items) = property.get("items") else {
        return false;
    };
    items.get("type").and_then(JsonValue::as_str) == Some("object") || items.get("$ref").is_some()
}

/// Scan `docs/schemas/` for envelope schemas with top-level `data`
/// array-of-objects properties. `degraded` is part of the envelope
/// contract on every surface and is never a truncation point.
fn detect_list_like_surfaces() -> Result<Vec<ListLikeSurface>, String> {
    let dir = schemas_dir();
    let entries =
        fs::read_dir(&dir).map_err(|error| format!("failed to read {}: {error}", dir.display()))?;
    let mut surfaces = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("failed to read dir entry: {error}"))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let raw = fs::read_to_string(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        let parsed: JsonValue = serde_json::from_str(&raw)
            .map_err(|error| format!("{} is not valid JSON: {error}", path.display()))?;
        let Some(data_properties) = parsed
            .pointer("/properties/data/properties")
            .and_then(JsonValue::as_object)
        else {
            continue;
        };
        let array_keys: Vec<String> = data_properties
            .iter()
            .filter(|(key, property)| key.as_str() != "degraded" && is_array_of_objects(property))
            .map(|(key, _)| key.clone())
            .collect();
        if array_keys.is_empty() {
            continue;
        }
        let schema_id = data_properties
            .get("schema")
            .and_then(|schema| schema.get("const"))
            .and_then(JsonValue::as_str)
            .map(str::to_owned)
            .or_else(|| {
                parsed
                    .get("title")
                    .and_then(JsonValue::as_str)
                    .map(str::to_owned)
            })
            .ok_or_else(|| {
                format!(
                    "{} declares list-like data arrays but has neither a data.schema const nor \
                     a title to identify the surface",
                    path.display()
                )
            })?;
        let command = data_properties
            .get("command")
            .and_then(|command| command.get("const"))
            .and_then(JsonValue::as_str)
            .map(str::to_owned);
        surfaces.push(ListLikeSurface {
            schema_id,
            command,
            array_keys,
            file_name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
        });
    }
    ensure(
        !surfaces.is_empty(),
        "the docs/schemas scan found no list-like surfaces at all — the detector or the \
         schema layout changed shape",
    )?;
    Ok(surfaces)
}

fn registry_governs(schema_id: &str, command: Option<&str>) -> bool {
    OUTPUT_TRUNCATION_REGISTRY.iter().any(|point| {
        (!point.schema_id.is_empty() && point.schema_id == schema_id)
            || (!point.command.is_empty() && Some(point.command) == command)
    })
}

#[test]
fn every_list_like_schema_is_governed_or_exempted() -> TestResult {
    let exemptions: BTreeMap<&str, &str> = GOVERNOR_EXEMPT_SCHEMAS.iter().copied().collect();
    let surfaces = detect_list_like_surfaces()?;

    for surface in &surfaces {
        let governed = registry_governs(&surface.schema_id, surface.command.as_deref());
        let exempted = exemptions.contains_key(surface.schema_id.as_str());
        ensure(
            governed || exempted,
            format!(
                "{} ({}) exposes top-level list-like data array(s) [{}] but neither declares a \
                 truncation point in OUTPUT_TRUNCATION_REGISTRY (src/output/mod.rs) nor carries \
                 a documented exemption in GOVERNOR_EXEMPT_SCHEMAS \
                 (tests/contracts/governor_truncation_registry.rs). Decide the surface's \
                 governor posture: wire --max-output-tokens/--cursor support and register the \
                 truncation point, or exempt it here with a reason agents can act on (ADR 0063 \
                 §2).",
                surface.schema_id,
                surface.file_name,
                surface.array_keys.join(", "),
            ),
        )?;
        ensure(
            !(governed && exempted),
            format!(
                "{} is both governed by OUTPUT_TRUNCATION_REGISTRY and exempted in \
                 GOVERNOR_EXEMPT_SCHEMAS — delete the stale exemption",
                surface.schema_id
            ),
        )?;
    }

    // Exemption staleness: every exempted id must still exist as a
    // detected list-like surface, otherwise the table is rotting.
    for (exempt_id, _) in GOVERNOR_EXEMPT_SCHEMAS {
        ensure(
            surfaces
                .iter()
                .any(|surface| surface.schema_id == *exempt_id),
            format!(
                "GOVERNOR_EXEMPT_SCHEMAS lists {exempt_id}, but no docs/schemas file currently \
                 detects as that list-like surface — delete the stale exemption",
            ),
        )?;
    }
    Ok(())
}

#[test]
fn truncation_registry_entries_are_well_formed_and_unique() -> TestResult {
    let mut seen_schema_ids = Vec::new();
    let mut seen_commands = Vec::new();
    for point in OUTPUT_TRUNCATION_REGISTRY {
        ensure(
            !point.schema_id.is_empty() || !point.command.is_empty(),
            "a truncation point must declare a schema id or a command to be reachable",
        )?;
        ensure(
            !point.array_path.is_empty(),
            format!(
                "truncation point {}/{} declares an empty array path",
                point.schema_id, point.command
            ),
        )?;
        ensure(
            !point.position_key_field.is_empty(),
            format!(
                "truncation point {}/{} declares an empty position key field",
                point.schema_id, point.command
            ),
        )?;
        if !point.schema_id.is_empty() {
            ensure(
                !seen_schema_ids.contains(&point.schema_id),
                format!(
                    "duplicate truncation point for schema id {}",
                    point.schema_id
                ),
            )?;
            seen_schema_ids.push(point.schema_id);
        }
        if !point.command.is_empty() {
            ensure(
                !seen_commands.contains(&point.command),
                format!("duplicate truncation point for command {}", point.command),
            )?;
            seen_commands.push(point.command);
        }
    }
    Ok(())
}

#[test]
fn registry_schema_ids_reference_documented_schemas() -> TestResult {
    let dir = schemas_dir();
    for point in OUTPUT_TRUNCATION_REGISTRY {
        if point.schema_id.is_empty() {
            continue;
        }
        let documented = dir.join(format!("{}.json", point.schema_id)).is_file();
        let pinned_gap = DOCS_SCHEMA_GAPS.contains(&point.schema_id);
        ensure(
            documented || pinned_gap,
            format!(
                "truncation point {} references a schema with no docs/schemas/{}.json file and \
                 no pinned gap entry — add the schema doc or (only for pre-existing debt) pin \
                 it in DOCS_SCHEMA_GAPS",
                point.schema_id, point.schema_id
            ),
        )?;
        ensure(
            !(documented && pinned_gap),
            format!(
                "DOCS_SCHEMA_GAPS still lists {} but docs/schemas/{}.json now exists — delete \
                 the stale gap entry",
                point.schema_id, point.schema_id
            ),
        )?;
    }
    Ok(())
}
