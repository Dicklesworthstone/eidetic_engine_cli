use std::collections::BTreeSet;

use serde_json::Value;

const MANIFEST: &str = include_str!("fixtures/swarm_scale/workloads.json");
const EXPECTED_SCHEMA: &str = "ee.swarm_scale.workloads.v1";
const EXPECTED_BEAD: &str = "eidetic_engine_cli-fcq1.1";
const EXPECTED_TRAFFIC_FAMILIES: &[&str] = &[
    "read_heavy_context_burst",
    "remember_write_burst",
    "cass_import_spike",
    "index_rebuild",
    "graph_refresh",
    "daemon_maintenance",
    "pack_replay_freshness_scan",
    "mixed_mode_swarm",
];
const EXPECTED_TIERS: &[&str] = &["small", "medium", "large", "stress"];

type TestResult = Result<(), String>;

fn manifest() -> Result<Value, String> {
    serde_json::from_str(MANIFEST)
        .map_err(|error| format!("swarm scale manifest must parse as JSON: {error}"))
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a Value, String> {
    value
        .get(name)
        .ok_or_else(|| format!("missing field `{name}`"))
}

fn string_field<'a>(value: &'a Value, name: &str) -> Result<&'a str, String> {
    field(value, name)?
        .as_str()
        .ok_or_else(|| format!("field `{name}` must be a string"))
}

fn bool_field(value: &Value, name: &str) -> Result<bool, String> {
    field(value, name)?
        .as_bool()
        .ok_or_else(|| format!("field `{name}` must be a boolean"))
}

fn u64_field(value: &Value, name: &str) -> Result<u64, String> {
    field(value, name)?
        .as_u64()
        .ok_or_else(|| format!("field `{name}` must be an unsigned integer"))
}

fn array_field<'a>(value: &'a Value, name: &str) -> Result<&'a Vec<Value>, String> {
    field(value, name)?
        .as_array()
        .ok_or_else(|| format!("field `{name}` must be an array"))
}

fn ensure(condition: bool, context: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(context.to_owned())
    }
}

fn ensure_eq<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, context: &str) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn string_set(values: &[Value], context: &str) -> Result<BTreeSet<String>, String> {
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{context} must contain only strings"))
        })
        .collect()
}

fn stable_memory_id(prefix: &str, ordinal: u64, width: u64) -> String {
    let width = usize::try_from(width).unwrap_or(8);
    format!("{prefix}{ordinal:0width$}")
}

#[test]
fn swarm_scale_manifest_contract_is_pinned() -> TestResult {
    let manifest = manifest()?;

    ensure_eq(
        string_field(&manifest, "schema")?,
        EXPECTED_SCHEMA,
        "schema",
    )?;
    ensure_eq(
        string_field(&manifest, "owning_bead_id")?,
        EXPECTED_BEAD,
        "owning bead",
    )?;
    ensure_eq(
        string_field(&manifest, "embedding_profile")?,
        "hash_embedder",
        "embedding profile",
    )?;
    ensure(
        !bool_field(&manifest, "requires_paid_api")?,
        "manifest must not require paid APIs",
    )?;
    ensure_eq(
        string_field(&manifest, "synthetic_secret_policy")?,
        "none",
        "synthetic secret policy",
    )?;
    ensure_eq(
        string_field(&manifest, "fixed_clock")?,
        "2026-05-06T00:00:00Z",
        "fixed clock",
    )
}

#[test]
fn swarm_scale_tiers_have_stable_generation_ids() -> TestResult {
    let manifest = manifest()?;
    let width = u64_field(&manifest, "memory_id_width")?;
    let tiers = array_field(&manifest, "tiers")?;
    ensure_eq(tiers.len(), EXPECTED_TIERS.len(), "tier count")?;

    let mut names = Vec::new();
    for tier in tiers {
        let name = string_field(tier, "name")?;
        names.push(name.to_owned());
        let memory_count = u64_field(tier, "memory_count")?;
        let generator = field(tier, "generator")?;
        let prefix = string_field(generator, "memory_id_prefix")?;
        let first_ordinal = u64_field(generator, "first_memory_ordinal")?;
        let expected_first = string_field(generator, "expected_first_memory_id")?;
        let expected_last = string_field(generator, "expected_last_memory_id")?;
        let last_ordinal = first_ordinal + memory_count - 1;

        ensure_eq(
            stable_memory_id(prefix, first_ordinal, width),
            expected_first.to_owned(),
            "first generated memory id",
        )?;
        ensure_eq(
            stable_memory_id(prefix, last_ordinal, width),
            expected_last.to_owned(),
            "last generated memory id",
        )?;
        ensure(
            !array_field(generator, "topic_cycle")?.is_empty(),
            "topic cycle must not be empty",
        )?;
        ensure(
            !array_field(generator, "trust_cycle")?.is_empty(),
            "trust cycle must not be empty",
        )?;
    }

    let expected_names: Vec<String> = EXPECTED_TIERS
        .iter()
        .map(|tier| (*tier).to_owned())
        .collect();
    ensure_eq(names, expected_names, "tier ordering")
}

#[test]
fn swarm_scale_profiles_are_monotonic_and_classified_for_ci() -> TestResult {
    let manifest = manifest()?;
    let tiers = array_field(&manifest, "tiers")?;
    let mut previous_memory_count = 0;
    let mut previous_rows = 0;
    let mut previous_index_bytes = 0;
    let mut ci_classes = BTreeSet::new();

    for tier in tiers {
        let name = string_field(tier, "name")?;
        let memory_count = u64_field(tier, "memory_count")?;
        let profile = field(tier, "resource_profile")?;
        let expected_rows = u64_field(profile, "expected_db_rows")?;
        let index_bytes = u64_field(profile, "expected_index_bytes")?;

        ensure(
            memory_count > previous_memory_count,
            "tier memory counts must increase",
        )?;
        ensure(
            expected_rows > previous_rows,
            "DB row estimates must increase",
        )?;
        ensure(
            index_bytes > previous_index_bytes,
            "index byte estimates must increase",
        )?;
        ensure(
            u64_field(profile, "expected_graph_nodes")? >= memory_count,
            "graph node estimate should cover memories",
        )?;
        ensure(
            u64_field(tier, "index_document_count")? == memory_count,
            "index document count should match memories",
        )?;

        ci_classes.insert(string_field(tier, "ci_suitability")?.to_owned());
        previous_memory_count = memory_count;
        previous_rows = expected_rows;
        previous_index_bytes = index_bytes;

        if name == "stress" {
            ensure(
                memory_count >= 100_000,
                "stress tier must model 100k+ memories",
            )?;
            ensure(
                u64_field(tier, "agent_count")? >= 256,
                "stress tier must model hundreds of agents",
            )?;
            ensure_eq(
                string_field(profile, "ram_class")?,
                "64gb",
                "stress RAM class",
            )?;
        }
    }

    for required in [
        "normal_ci",
        "nightly_ci",
        "release_candidate",
        "local_256gb",
    ] {
        ensure(
            ci_classes.contains(required),
            &format!("missing CI suitability class `{required}`"),
        )?;
    }
    Ok(())
}

#[test]
fn swarm_scale_traffic_mix_covers_all_pressure_modes() -> TestResult {
    let manifest = manifest()?;
    let declared = string_set(
        array_field(&manifest, "traffic_families")?,
        "traffic_families",
    )?;
    let expected: BTreeSet<String> = EXPECTED_TRAFFIC_FAMILIES
        .iter()
        .map(|family| (*family).to_owned())
        .collect();
    ensure_eq(declared.clone(), expected, "declared traffic families")?;

    let mut observed = BTreeSet::new();
    for tier in array_field(&manifest, "tiers")? {
        for traffic in array_field(tier, "traffic_mix")? {
            let family = string_field(traffic, "family")?;
            ensure(
                declared.contains(family),
                &format!("traffic family `{family}` must be declared"),
            )?;
            ensure(
                u64_field(traffic, "operations")? > 0,
                "traffic operation count must be positive",
            )?;
            ensure(
                u64_field(traffic, "agents")? > 0,
                "traffic agent count must be positive",
            )?;
            ensure(
                !array_field(traffic, "expected_artifacts")?.is_empty(),
                "traffic must name expected artifacts",
            )?;
            observed.insert(family.to_owned());
        }
    }

    ensure_eq(observed, declared, "observed traffic families")
}
