use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ee::db::{
    CreateMemoryInput, CreatePackItemInput, CreatePackRecordInput, CreateWorkspaceInput,
    DbConnection,
};
use ee::models::{PackId, WorkspaceId};
use ee::obs::volatile_fields::normalize_pack_slo_measurements;
use insta::assert_snapshot;
use serde_json::{Map, Value, json};

type TestResult = Result<(), String>;

const MEMORY_ID: &str = "mem_00000000000000000000000001";
const PACK_ID: &str = "pack_00000000000000000000000001";
const QUERY: &str = "format before release";

#[derive(Debug)]
struct JsonContractFixture {
    workspace: PathBuf,
    database: PathBuf,
    index_dir: PathBuf,
    canonical_workspace: PathBuf,
    canonical_database: PathBuf,
    canonical_index_dir: PathBuf,
    canonical_repo: PathBuf,
    binary: PathBuf,
    canonical_binary: PathBuf,
}

impl JsonContractFixture {
    fn new() -> Result<Self, String> {
        let artifact_dir = unique_artifact_dir("json-contract-snapshots")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        let runtime_dir = workspace.join(".runtime");

        // bd-28kky: a private HOME, for the same reason .runtime is private.
        // run_ee_at points HOME here so the running account's user-global
        // memory lane cannot leak into these contracts.
        let home_dir = workspace.join(".home");

        for dir in [&runtime_dir, &home_dir] {
            fs::create_dir_all(dir).map_err(|error| {
                format!(
                    "failed to create fixture directory {}: {error}",
                    dir.display()
                )
            })?;
        }
        seed_workspace(&workspace, &database)?;
        write_operating_profile_config(&workspace)?;

        let canonical_workspace = canonical_fixture_path(&workspace, "workspace")?;
        let canonical_database = canonical_fixture_path(&database, "database")?;
        let canonical_index_dir_input = canonical_workspace.join(".ee").join("index");
        let binary = PathBuf::from(env!("CARGO_BIN_EXE_ee"));
        let canonical_binary = canonical_fixture_path(&binary, "ee binary")?;
        build_search_index(
            &binary,
            &canonical_workspace,
            &canonical_database,
            &canonical_index_dir_input,
        )?;
        let canonical_index_dir =
            canonical_fixture_path(&canonical_index_dir_input, "index directory")?;
        let canonical_repo =
            canonical_fixture_path(Path::new(env!("CARGO_MANIFEST_DIR")), "repository")?;

        Ok(Self {
            workspace,
            database,
            index_dir,
            canonical_workspace,
            canonical_database,
            canonical_index_dir,
            canonical_repo,
            binary,
            canonical_binary,
        })
    }

    fn workspace_arg(&self) -> String {
        self.canonical_workspace.to_string_lossy().into_owned()
    }

    fn database_arg(&self) -> String {
        self.canonical_database.to_string_lossy().into_owned()
    }

    fn index_dir_arg(&self) -> String {
        self.canonical_index_dir.to_string_lossy().into_owned()
    }

    fn profile_config_arg(&self) -> String {
        self.canonical_workspace
            .join(".ee")
            .join("profile-contract.toml")
            .to_string_lossy()
            .into_owned()
    }
}

fn canonical_fixture_path(path: &Path, label: &str) -> Result<PathBuf, String> {
    fs::canonicalize(path).map_err(|error| {
        format!(
            "failed to canonicalize fixture {label} {}: {error}",
            path.display()
        )
    })
}

fn unique_artifact_dir(prefix: &str) -> Result<PathBuf, String> {
    tempfile::Builder::new()
        .prefix(&format!("{prefix}-"))
        .tempdir()
        .map(tempfile::TempDir::keep)
        .map_err(|error| format!("failed to create {prefix} artifact directory: {error}"))
}

fn assert_graph_surface_snapshot(name: &str, value: Value) {
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path("snapshots");
    settings.set_prepend_module_to_snapshot(false);
    settings.bind(|| {
        assert_snapshot!(name, canonical_json_text(value));
    });
}

fn assert_contract_snapshot(name: &str, value: Value) {
    assert_snapshot!(name, canonical_json_text(value));
}

fn canonical_json_text(value: Value) -> String {
    match serde_json::to_string_pretty(&canonical_json(value)) {
        Ok(serialized) => serialized,
        Err(error) => panic!("serde_json::Value failed canonical serialization: {error}"),
    }
}

fn canonical_json(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(canonical_json).collect()),
        Value::Object(object) => {
            let mut entries: Vec<_> = object.into_iter().collect();
            let preserve_profile_pair_order = entries.len() == 2
                && entries.iter().any(|(key, _)| key == "missingConfigDryRun")
                && entries.iter().any(|(key, _)| key == "existingConfigDryRun");
            if !preserve_profile_pair_order {
                entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            }
            let mut canonical = Map::new();
            for (key, value) in entries {
                canonical.insert(key, canonical_json(value));
            }
            Value::Object(canonical)
        }
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                Value::from(integer)
            } else if let Some(integer) = number.as_u64() {
                Value::from(integer)
            } else if let Some(float) = number.as_f64() {
                serde_json::Number::from_f64(float)
                    .map(Value::Number)
                    .unwrap_or(Value::Number(number))
            } else {
                Value::Number(number)
            }
        }
        scalar => scalar,
    }
}

fn schema_example(schema_id: &str) -> Result<Value, String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("schemas")
        .join(format!("{schema_id}.json"));
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let schema: Value = serde_json::from_str(&text)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    schema
        .get("examples")
        .and_then(Value::as_array)
        .and_then(|examples| examples.first())
        .cloned()
        .ok_or_else(|| format!("{schema_id} must define examples[0] for snapshot coverage"))
}

fn seed_workspace(workspace: &Path, database: &Path) -> TestResult {
    if let Some(parent) = database.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create database parent {}: {error}",
                parent.display()
            )
        })?;
    }

    let canonical_workspace = canonical_fixture_path(workspace, "workspace")?;
    let workspace_id = stable_workspace_id(&canonical_workspace);
    let connection = DbConnection::open_file(database).map_err(|error| error.to_string())?;
    connection.migrate().map_err(|error| error.to_string())?;
    connection
        .insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: canonical_workspace.to_string_lossy().into_owned(),
                name: Some("json-contract-snapshots".to_string()),
            },
        )
        .map_err(|error| error.to_string())?;
    connection
        .insert_memory(
            MEMORY_ID,
            &CreateMemoryInput {
                workspace_id: workspace_id.clone(),
                level: "procedural".to_string(),
                kind: "rule".to_string(),
                content: "Run cargo fmt --check before release.".to_string(),
                workflow_id: None,
                confidence: 0.92,
                utility: 0.8,
                importance: 0.7,
                provenance_uri: Some("file://AGENTS.md#L164-173".to_string()),
                trust_class: "human_explicit".to_string(),
                trust_subclass: Some("project-rule".to_string()),
                tags: vec!["cargo".to_string(), "formatting".to_string()],
                valid_from: None,
                valid_to: None,
            },
        )
        .map_err(|error| error.to_string())?;
    connection
        .insert_pack_record(
            PACK_ID,
            &CreatePackRecordInput {
                task_paths: Vec::new(),
                workspace_id: workspace_id.clone(),
                query: QUERY.to_string(),
                profile: "compact".to_string(),
                max_tokens: 4000,
                used_tokens: 8,
                item_count: 1,
                omitted_count: 0,
                pack_hash: format!(
                    "blake3:{}",
                    blake3::hash(b"JSON contract snapshot fixture pack").to_hex()
                ),
                degraded_json: None,
                created_by: Some("golden-test".to_string()),
            },
            &[CreatePackItemInput {
                pack_id: PACK_ID.to_string(),
                memory_id: MEMORY_ID.to_string(),
                rank: 1,
                section: "procedural_rules".to_string(),
                estimated_tokens: 8,
                relevance: 0.91,
                utility: 0.8,
                combined_score: None,
                attempt_family_multiplicity: None,
                why: "Selected because the memory matches release-formatting work.".to_string(),
                diversity_key: Some("procedural:rule:cargo".to_string()),
                provenance_json: r#"{"schema":"ee.pack_item.provenance.v1","entries":[{"uri":"file://AGENTS.md#L164-173","trustClass":"human_explicit","trustSubclass":"project-rule"}]}"#.to_string(),
                trust_class: "human_explicit".to_string(),
                trust_subclass: Some("project-rule".to_string()),
            }],
            &[],
        )
        .map_err(|error| error.to_string())?;
    connection.close().map_err(|error| error.to_string())
}

fn write_operating_profile_config(workspace: &Path) -> TestResult {
    let path = workspace.join(".ee").join("config.toml");
    fs::write(&path, "profile = { selected = \"portable\" }\n").map_err(|error| {
        format!(
            "failed to write fixture profile {}: {error}",
            path.display()
        )
    })
}

fn stable_workspace_id(workspace: &Path) -> String {
    let canonical_workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let hash =
        blake3::hash(format!("workspace:{}", canonical_workspace.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    for (target, source) in bytes.iter_mut().zip(hash.as_bytes().iter()) {
        *target = *source;
    }
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

#[test]
fn graph_json_surface_examples_match_snapshots() -> TestResult {
    for (snapshot_name, schema_id) in [
        ("insights", "ee.insights.v1"),
        ("skyline", "ee.status.skyline.v1"),
        ("pack_dna", "ee.context.pack_dna.v1"),
        ("causal_explanation", "ee.why.causal.v1"),
        ("health_structural", "ee.health.structural.v1"),
    ] {
        assert_graph_surface_snapshot(snapshot_name, schema_example(schema_id)?);
    }
    Ok(())
}

fn build_search_index(
    binary: &Path,
    workspace: &Path,
    database: &Path,
    index_dir: &Path,
) -> TestResult {
    let rebuild_args = vec![
        "--json".to_string(),
        "--workspace".to_string(),
        workspace.to_string_lossy().into_owned(),
        "index".to_string(),
        "rebuild".to_string(),
        "--database".to_string(),
        database.to_string_lossy().into_owned(),
        "--index-dir".to_string(),
        index_dir.to_string_lossy().into_owned(),
    ];
    let report = parse_ee_json_output(run_ee_at(binary, workspace, &rebuild_args)?, &rebuild_args)?;
    if report["data"]["status"] != "success" {
        return Err(format!("index rebuild failed: {report}"));
    }
    if report["data"]["documents_total"] != 1 {
        return Err(format!(
            "expected one indexed document, got {}",
            report["data"]["documents_total"]
        ));
    }

    let status_args = vec![
        "--json".to_string(),
        "--workspace".to_string(),
        workspace.to_string_lossy().into_owned(),
        "index".to_string(),
        "status".to_string(),
        "--database".to_string(),
        database.to_string_lossy().into_owned(),
        "--index-dir".to_string(),
        index_dir.to_string_lossy().into_owned(),
    ];
    let status = parse_ee_json_output(run_ee_at(binary, workspace, &status_args)?, &status_args)?;
    if status["data"]["health"] != "ready" {
        return Err(format!(
            "fixture must launch JSON contract commands with a ready index: {status}"
        ));
    }

    let metadata_path = index_dir.join("meta.json");
    let metadata_text = fs::read_to_string(&metadata_path)
        .map_err(|error| format!("failed to read {}: {error}", metadata_path.display()))?;
    let metadata: Value = serde_json::from_str(&metadata_text)
        .map_err(|error| format!("failed to parse {}: {error}", metadata_path.display()))?;
    let has_semantic_fingerprint = [
        "storedModelId",
        "storedModelRevision",
        "storedModelHash",
        "storedDimension",
        "storedDistanceMetric",
        "storedVectorDtype",
    ]
    .into_iter()
    .any(|field| metadata.get(field).is_some());
    let manifest_backend = if has_semantic_fingerprint {
        "semantic"
    } else {
        "frankensearch_hash_fallback"
    };
    let subprocess_backend = status["data"]["embedding"]["source"]
        .as_str()
        .ok_or_else(|| format!("index status omitted embedding.source: {status}"))?;
    if manifest_backend != subprocess_backend
        || status["data"]["embedding"]["mode"] != "deterministic_hash"
        || status["data"]["embedding"]["fast_model_id"] != "fnv1a-256"
    {
        return Err(format!(
            "pre-command index backend mismatch: manifest={manifest_backend}, subprocess={subprocess_backend}, embedding={}",
            status["data"]["embedding"]
        ));
    }
    Ok(())
}

fn run_ee_at(binary: &Path, workspace: &Path, args: &[String]) -> Result<Output, String> {
    Command::new(binary)
        .args(args)
        .env("EE_EMBED_DOWNLOAD", "off")
        .env(
            "EE_EMBED_MODEL_DIR",
            workspace.join(".ee/empty-model-cache"),
        )
        .env_remove("EE_EMBED_MODEL_PATH")
        .env_remove("EE_PROFILE")
        .env_remove("EE_WORKSPACE")
        .env("PATH", "/usr/bin:/bin")
        .env("XDG_RUNTIME_DIR", workspace.join(".runtime"))
        // bd-28kky. Keep the RUNNING USER'S user-global memory lane out of these
        // contracts. Without this, `ee` resolves that account's global store and
        // the snapshots record whatever state it happens to be in, INCLUDING its
        // path: regenerating on an RCH worker wrote three
        // `global_lane_migration_required` degradations carrying the worker
        // account's home directory, moved degradationCount 2 -> 3 and rewrote the
        // context summary -- none of which is about the surface under test. The
        // scrubber normalises [WORKSPACE]/[DATABASE]/[INDEX]/[REPO]/[EE_BINARY]
        // and timestamps; it does not know the global-store path, so nothing
        // caught it.
        //
        // HOME ONLY, DELIBERATELY. An earlier attempt set XDG_DATA_HOME too and
        // the test failed outright. src/core/index.rs:7517 process_ee_data_dir()
        // resolves the EMBEDDING REGISTRY through that same XDG root, so moving
        // it plausibly took the model registry with it and broke the fixture's
        // own precondition (:374 requires embedding.mode == deterministic_hash).
        // That mechanism is UNVERIFIED -- I never captured the failure text --
        // so this change isolates the smaller variable and leaves XDG_DATA_HOME
        // alone. scripts/e2e_overhaul/determinism.sh:588 sets HOME the same way.
        .env("HOME", workspace.join(".home"))
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn run_ee(fixture: &JsonContractFixture, args: &[String]) -> Result<Output, String> {
    run_ee_at(&fixture.binary, &fixture.canonical_workspace, args)
}

fn parse_ee_json_output(output: Output, args: &[String]) -> Result<Value, String> {
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("stdout was not UTF-8 for ee {}: {error}", args.join(" ")))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("stderr was not UTF-8 for ee {}: {error}", args.join(" ")))?;

    if !output.status.success() {
        return Err(format!(
            "ee {} failed with status {:?}; stderr: {stderr}; stdout: {stdout}",
            args.join(" "),
            output.status.code()
        ));
    }
    if !stderr.is_empty() {
        return Err(format!(
            "ee {} must keep JSON diagnostics out of stderr, got: {stderr:?}",
            args.join(" ")
        ));
    }
    if !stdout.ends_with('\n') {
        return Err(format!(
            "ee {} stdout must be newline-terminated JSON, got: {stdout:?}",
            args.join(" ")
        ));
    }

    serde_json::from_str(&stdout)
        .map_err(|error| format!("ee {} stdout must be JSON: {error}", args.join(" ")))
}

fn run_json_command(fixture: &JsonContractFixture, args: Vec<String>) -> Result<Value, String> {
    let mut value = parse_ee_json_output(run_ee(fixture, &args)?, &args)?;
    scrub_json_contract(&mut value, fixture)?;
    Ok(value)
}

fn scrub_json_contract(value: &mut Value, fixture: &JsonContractFixture) -> Result<(), String> {
    // Document-level first: the timing normalization has to see the whole
    // response at once, because one degraded list is serialized at both
    // `.degraded` and `.data.degraded` and its length is echoed into counts
    // and prose elsewhere in the tree.
    normalize_timing_degradations(value);
    // ...and the pack SLO is the SECOND place the same wall clock is published.
    //
    // `normalize_timing_degradations` above only filters `degraded[]`. It has
    // no reference to `slo`, so `slo.elapsedStatus` -- classified straight off
    // `elapsed_ms` by `PackAssemblySloStatus::for_elapsed_ms` (src/pack/mod.rs)
    // -- and the `slo.status` rollup that takes the worst of it survived
    // unnormalized. A loaded worker records `warning` there where an idle
    // laptop records `within_budget`, so freezing either into a snapshot bakes
    // one host's load into a committed contract (bd-28kky).
    //
    // This calls the PRODUCTION definition of that volatile channel rather
    // than a second, test-local one. `src/obs/volatile_fields.rs` already names
    // `/data/pack/slo/{status,elapsedStatus}` and `/actuals/elapsedMs` as the
    // unsigned producer measurements; a copy here would be free to drift from
    // it, and the drift would show up as a flake rather than as a red.
    //
    // ORDER IS LOAD-BEARING: this must run BEFORE `scrub_json_contract_recursive`,
    // which zeroes every `*Ms` field in the tree. The validator checks that
    // `elapsedStatus` agrees with `elapsedMs` against the real budget
    // thresholds, and it rejects a `budgetClass.elapsedMsTarget` of 0. After
    // the recursive scrub every one of those inputs reads 0 and the check
    // could no longer be made at all.
    //
    // An `Err` is not volatility, it is an inconsistent SLO -- a status that
    // disagrees with the measurement it is supposed to summarize -- so it fails
    // the test with the producer's own message instead of being scrubbed away.
    // That trade is what keeps this from being a weakening: the snapshot gives
    // up three frozen literals that only ever asserted one host's reading, and
    // gets back an algebraic assertion re-checked against the real numbers on
    // every single run.
    normalize_pack_slo_measurements(value)
        .map_err(|error| format!("pack SLO measurements are not self-consistent: {error}"))?;
    scrub_json_contract_recursive(value, fixture);
    Ok(())
}

fn scrub_json_contract_recursive(value: &mut Value, fixture: &JsonContractFixture) {
    match value {
        Value::Object(object) => {
            for (key, child) in object.iter_mut() {
                scrub_json_contract_recursive(child, fixture);
                if key == "hostCalibration" {
                    scrub_host_calibration(child);
                }
                scrub_value_for_key(key, child);
            }
            let mut entries: Vec<_> = std::mem::take(object).into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            object.extend(entries);
        }
        Value::Array(items) => {
            for item in items {
                scrub_json_contract_recursive(item, fixture);
            }
        }
        Value::String(text) => {
            *text = scrub_string(text, fixture);
        }
        Value::Number(_) | Value::Bool(_) | Value::Null => {}
    }
}

fn scrub_host_calibration(value: &mut Value) {
    const HOST_DEPENDENT: &str = "[HOST_DEPENDENT]";

    let Some(object) = value.as_object_mut() else {
        return;
    };
    for key in [
        "calibrationFreshness",
        "confidence",
        "configuredProfile",
        "effectiveProfile",
        "hostClass",
        "profileCeiling",
        "recommendedProfile",
        "targetDirPosture",
    ] {
        if let Some(field @ (Value::String(_) | Value::Null)) = object.get_mut(key) {
            *field = Value::String(HOST_DEPENDENT.to_string());
        }
    }
    for key in [
        "budgetDeltas",
        "degraded",
        "reasonCodes",
        "repairActions",
        "topologyWarnings",
    ] {
        if let Some(field @ Value::Array(_)) = object.get_mut(key) {
            *field = json!([HOST_DEPENDENT]);
        }
    }
}

fn scrub_value_for_key(key: &str, value: &mut Value) {
    let normalized = key.to_ascii_lowercase();
    if normalized.contains("hash") || normalized.contains("fingerprint") {
        if value.is_string() {
            *value = Value::String("[HASH]".to_string());
        }
        return;
    }
    if normalized == "packid" || normalized == "pack_id" {
        if value.is_string() {
            *value = Value::String("[PACK_ID]".to_string());
        }
        return;
    }
    if normalized == "auditid" || normalized == "audit_id" {
        if value.is_string() {
            *value = Value::String("[AUDIT_ID]".to_string());
        }
        return;
    }
    if normalized == "workspaceid" || normalized == "workspace_id" {
        if value.is_string() {
            *value = Value::String("[WORKSPACE_ID]".to_string());
        }
        return;
    }
    if is_timestamp_key(&normalized) {
        if value.is_string() {
            *value = Value::String("[TIMESTAMP]".to_string());
        }
        return;
    }
    if is_elapsed_key(&normalized) && value.is_number() {
        *value = serde_json::json!(0);
        return;
    }
    if normalized.contains("freshness") && value.is_number() {
        *value = serde_json::json!(0);
    }
}

fn is_timestamp_key(key: &str) -> bool {
    matches!(
        key,
        "createdat"
            | "created_at"
            | "updatedat"
            | "updated_at"
            | "verifiedat"
            | "verified_at"
            | "completedat"
            | "completed_at"
            | "startedat"
            | "started_at"
            | "timestamp"
    )
}

fn is_elapsed_key(key: &str) -> bool {
    key.contains("elapsed")
        || key.contains("duration")
        || key.contains("latency")
        || key == "ms"
        || key.ends_with("ms")
}

// bd-jikgj (found by TurquoiseBirch): a timing observation reaches this
// snapshot through channels the key-based scrubber cannot see.
//
// `is_elapsed_key` above zeroes a NUMBER whose KEY looks like a duration.
// That is the entire existing defense against wall-clock volatility, and it is
// structurally blind to bd-jikgj, which routes elapsed time into:
//   * a `degraded[]` ENTRY whose mere PRESENCE depends on elapsed time --
//     `pack_assembly_elapsed_degradation` (src/pack/mod.rs) emits one iff the
//     observed elapsed is `>=` the profile's warning threshold,
//   * that entry's `message`, which embeds the raw millisecond reading,
//   * `advisoryBanner.degradationCount`, a bare number,
//   * `advisoryBanner.summary` and the rendered `pack.text`, where the count
//     sits INSIDE an English sentence ("Context includes 2 degraded signals",
//     built from `degraded.len()` in `advisory_summary`).
// Not one of those is a key ending in "ms". The signal goes around the
// defense rather than through it.
//
// Production already draws exactly this boundary for the OTHER determinism
// mechanism: `timing_degradations()` is held out of the pack hash and out of
// persistence (src/pack/mod.rs:3927-3939, src/core/context.rs:3895-3899) so a
// loaded machine cannot change the hash. The hash was defended; the snapshot
// was not. This puts the same boundary on the snapshot side.
//
// Pinning the fixture's resource profile is NOT an alternative. The entry
// fires on a wall-clock `>=` against a FINITE threshold (at most 2000ms, the
// swarm_heavy warning), so a sufficiently loaded host crosses any profile's
// threshold. Pinning would only make the flake rarer, and a rare flake whose
// first green reading looks like proof is worse than a red test.
const TIMING_DEGRADED_CODES: &[&str] = &[ee::pack::PACK_ASSEMBLY_ELAPSED_OVER_BUDGET_CODE];

/// Opening words of the timing degradation as rendered into markdown.
///
/// The markdown body carries only severity + message + repair -- the `code` is
/// not rendered -- so the bullet can only be matched on message shape. Mirrors
/// the `format!` in `pack_assembly_elapsed_degradation`. If that wording
/// changes this stops matching and the test goes RED on a slow host, which is
/// the safe direction: it can never turn into a silent pass.
const TIMING_DEGRADED_MESSAGE_PREFIX: &str = "Pack assembly took ";

/// Normalize a response so it reads identically on a fast and a slow host.
///
/// Erases the timing degradation and every count derived from it. Deterministic
/// degradations are untouched, so the snapshot keeps asserting them.
fn normalize_timing_degradations(value: &mut Value) {
    let dropped = strip_timing_degraded_entries(value);
    if dropped == 0 {
        return;
    }
    adjust_timing_derived_counts(value, dropped);
}

/// Filter timing entries out of every `degraded` array, returning the LARGEST
/// number taken from any single array.
///
/// Largest, not total: one logical list is serialized at both `.degraded` and
/// `.data.degraded`, so summing would double-count and over-correct the
/// derived counts.
fn strip_timing_degraded_entries(value: &mut Value) -> usize {
    let mut dropped = 0usize;
    match value {
        Value::Object(object) => {
            for (key, child) in object.iter_mut() {
                if key == "degraded" {
                    if let Value::Array(items) = child {
                        let before = items.len();
                        items.retain(|item| !is_timing_degradation(item));
                        dropped = dropped.max(before.saturating_sub(items.len()));
                    }
                }
                dropped = dropped.max(strip_timing_degraded_entries(child));
            }
        }
        Value::Array(items) => {
            for item in items {
                dropped = dropped.max(strip_timing_degraded_entries(item));
            }
        }
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => {}
    }
    dropped
}

fn is_timing_degradation(item: &Value) -> bool {
    item.get("code")
        .and_then(Value::as_str)
        .is_some_and(|code| TIMING_DEGRADED_CODES.contains(&code))
}

/// Bring every value DERIVED from the degraded list back to its fast-host
/// reading: the numeric count, the count inside prose, and the rendered
/// markdown bullet.
fn adjust_timing_derived_counts(value: &mut Value, dropped: usize) {
    match value {
        Value::Object(object) => {
            for (key, child) in object.iter_mut() {
                if key == "degradationCount" {
                    if let Some(count) = child.as_u64() {
                        *child = json!(count.saturating_sub(dropped as u64));
                        continue;
                    }
                }
                adjust_timing_derived_counts(child, dropped);
            }
        }
        Value::Array(items) => {
            for item in items {
                adjust_timing_derived_counts(item, dropped);
            }
        }
        Value::String(text) => {
            let without_bullet = strip_timing_degradation_markdown(text);
            *text = renumber_degraded_signal_prose(&without_bullet, dropped);
        }
        Value::Number(_) | Value::Bool(_) | Value::Null => {}
    }
}

/// Rewrite "Context includes N degraded signal(s)" down by `dropped`.
///
/// The sentence is built from `degraded.len()`, so it counted the timing entry.
/// The noun is re-pluralized because the renderer pluralizes from the same
/// count, and 2 -> 1 must read "signal", not "signals".
fn renumber_degraded_signal_prose(text: &str, dropped: usize) -> String {
    const PREFIX: &str = "Context includes ";
    const SUFFIX: &str = " degraded signal";
    if !text.contains(SUFFIX) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(PREFIX) {
        let split = at + PREFIX.len();
        out.push_str(&rest[..split]);
        rest = &rest[split..];

        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            continue;
        }
        let Ok(count) = digits.parse::<usize>() else {
            continue;
        };
        let after_digits = &rest[digits.len()..];
        if !after_digits.starts_with(SUFFIX) {
            continue;
        }
        let after_noun = &after_digits[SUFFIX.len()..];
        let tail = after_noun.strip_prefix('s').unwrap_or(after_noun);

        let adjusted = count.saturating_sub(dropped);
        out.push_str(&adjusted.to_string());
        out.push_str(SUFFIX);
        if adjusted != 1 {
            out.push('s');
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// Take the rendered timing bullet, and its indented repair line, out of a
/// markdown body.
fn strip_timing_degradation_markdown(text: &str) -> String {
    if !text.contains(TIMING_DEGRADED_MESSAGE_PREFIX) {
        return text.to_string();
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in text.split('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("- **[") && line.contains(TIMING_DEGRADED_MESSAGE_PREFIX) {
            skipping = true;
            continue;
        }
        if skipping {
            if trimmed.starts_with("- *Repair:*") {
                continue;
            }
            skipping = false;
        }
        kept.push(line);
    }
    kept.join("\n")
}

fn scrub_string(text: &str, fixture: &JsonContractFixture) -> String {
    if text.starts_with("blake3:") || text.starts_with("sha256:") {
        return "[HASH]".to_string();
    }
    if text.parse::<PackId>().is_ok() {
        return "[PACK_ID]".to_string();
    }
    if looks_like_rfc3339(text) {
        return "[TIMESTAMP]".to_string();
    }

    let mut scrubbed = text.to_string();
    for (path, replacement) in [
        (fixture.binary.as_path(), "[EE_BINARY]"),
        (fixture.canonical_binary.as_path(), "[EE_BINARY]"),
        (fixture.database.as_path(), "[DATABASE]"),
        (fixture.canonical_database.as_path(), "[DATABASE]"),
        (fixture.index_dir.as_path(), "[INDEX]"),
        (fixture.canonical_index_dir.as_path(), "[INDEX]"),
        (fixture.workspace.as_path(), "[WORKSPACE]"),
        (fixture.canonical_workspace.as_path(), "[WORKSPACE]"),
        (Path::new(env!("CARGO_MANIFEST_DIR")), "[REPO]"),
        (fixture.canonical_repo.as_path(), "[REPO]"),
    ] {
        scrubbed = scrubbed.replace(path.to_string_lossy().as_ref(), replacement);
    }
    scrub_pack_hash_comments(&scrub_daemon_socket_path(&scrubbed))
}

/// Replace the daemon socket path, which CANNOT be stable in a snapshot.
///
/// bd-28kky. `ee doctor` reports the socket it looked for, and that path varies
/// on two independent axes that the path-literal scrubbing above cannot reach:
///
///   * the PARENT is per-UID. src/daemon/mod.rs:176 uses
///     `${XDG_RUNTIME_DIR}/ee` only when the runtime dir follows the
///     systemd-user 0700 contract, and otherwise falls back to
///     `${TMPDIR:-/tmp}/ee-${uid}`. That is uid 1000 on an RCH worker and a
///     different uid on a dev Mac.
///   * the FILENAME is per-run. workspace_daemon_socket_path() is
///     `d-{blake3(canonical_workspace)[..24]}.sock`, and this fixture builds a
///     UNIQUE temp workspace per run, so the digest changes every time. Measured:
///     two regenerations minutes apart produced d-b5752ef3... and d-f97be5b5...
///
/// The workspace path itself is already scrubbed to [WORKSPACE], but the DIGEST
/// of it is not a substring of it, so no path replacement can catch this. It
/// needs its own rule.
///
/// bd-47x3l: that rule is the ONE shared normaliser,
/// `ee::obs::normalize_workspace_daemon_socket_paths`, which the golden
/// harnesses use too; this harness keeps only its `[DAEMON_SOCKET]` label. The
/// private rule it replaced required an `/ee-<uid>/` parent, so the
/// `${XDG_RUNTIME_DIR}/ee/d-<hex>.sock` form passed through unscrubbed.
fn scrub_daemon_socket_path(text: &str) -> String {
    let (normalized, replaced) = ee::obs::normalize_workspace_daemon_socket_paths(text);
    if replaced == 0 {
        return normalized;
    }
    normalized.replace(
        ee::obs::WORKSPACE_DAEMON_SOCKET_PLACEHOLDER,
        "[DAEMON_SOCKET]",
    )
}

#[test]
fn pack_id_scrubbing_distinguishes_ids_from_degraded_codes() {
    assert!(PACK_ID.parse::<PackId>().is_ok());
    assert!("pack_slot_lock_unavailable".parse::<PackId>().is_err());
}

/// bd-28kky. A scrubber defines a volatile channel, so it needs a NEGATIVE arm:
/// one that only ever fires is indistinguishable from one that eats real values.
/// The two positive cases are the exact strings two regenerations produced
/// minutes apart; the digest differs between them, which is the whole point.
/// The third is the XDG-parent form an RCH worker reports (bd-47x3l).
#[test]
fn daemon_socket_scrubbing_replaces_the_volatile_path_and_nothing_else() {
    // Built from one template so the ONLY difference is the socket path.
    let message = |socket: &str| {
        format!(
            "Optional daemon socket is not present at {socket}; in-process CLI execution remains authoritative."
        )
    };
    let first = message("/tmp/ee-1000/d-b5752ef363443d504fed354a.sock");
    let second = message("/tmp/ee-1000/d-f97be5b58e2da7577117314c.sock");
    let scrubbed_first = scrub_daemon_socket_path(&first);
    assert!(
        scrubbed_first.contains("[DAEMON_SOCKET]"),
        "socket path was not scrubbed: {scrubbed_first}"
    );
    assert!(
        !scrubbed_first.contains("d-b5752ef363443d504fed354a"),
        "per-run digest survived: {scrubbed_first}"
    );
    assert!(
        !scrubbed_first.contains("ee-1000"),
        "per-uid parent survived: {scrubbed_first}"
    );

    // THE POINT OF THE SCRUBBER: two runs whose ONLY difference is the volatile
    // path must normalise to the same text, or the snapshot can never settle.
    assert_eq!(
        scrubbed_first,
        scrub_daemon_socket_path(&second),
        "two runs differing only in socket path must scrub identically"
    );

    // bd-47x3l: the XDG form, as `ee doctor` printed it on RCH worker
    // vmi1227854. The parent is `${XDG_RUNTIME_DIR}/ee/`, not `/tmp/ee-<uid>/`;
    // the private rule this harness used to carry left it in the snapshot.
    let xdg = message("/run/user/1000/ee/d-3f005d529dc893aa9eb9a073.sock");
    assert_eq!(
        scrub_daemon_socket_path(&xdg),
        scrubbed_first,
        "the XDG runtime-dir socket must scrub like the /tmp fallback"
    );

    // NEGATIVE ARMS. None of these is a per-workspace daemon socket and none
    // may be touched -- a scrubber that widens is worse than one that is
    // missing, because it silently deletes evidence from every future snapshot.
    for untouched in [
        "index at [WORKSPACE]/.ee/index is ready",
        "no socket here at all",
        "/tmp/ee-notanumber/d-abc.sock is not a hashed name",
        "/run/user/1000/ee/daemon.sock is the default socket",
        "/tmp/ee-1000/d-b5752ef363443d504fed354.sock has 23 hex digits",
    ] {
        assert_eq!(
            scrub_daemon_socket_path(untouched),
            untouched,
            "scrubber must not touch: {untouched}"
        );
    }
}

fn scrub_pack_hash_comments(text: &str) -> String {
    let marker = "<!-- pack.hash: ";
    let replacement = "<!-- pack.hash: [HASH] -->";
    let mut scrubbed = text.to_owned();
    let mut cursor = 0;
    while let Some(offset) = scrubbed[cursor..].find(marker) {
        let start = cursor + offset;
        let Some(end_offset) = scrubbed[start..].find("-->") else {
            break;
        };
        let end = start + end_offset + "-->".len();
        scrubbed.replace_range(start..end, replacement);
        cursor = start + replacement.len();
    }
    scrubbed
}

fn looks_like_rfc3339(text: &str) -> bool {
    text.len() >= 20
        && text.as_bytes().get(4) == Some(&b'-')
        && text.as_bytes().get(7) == Some(&b'-')
        && text.as_bytes().get(10) == Some(&b'T')
        && (text.ends_with('Z') || text.contains("+00:00"))
}

#[test]
fn fixture_backed_agent_json_contracts_match_snapshots() -> TestResult {
    let fixture = JsonContractFixture::new()?;
    let workspace = fixture.workspace_arg();
    let database = fixture.database_arg();
    let index_dir = fixture.index_dir_arg();

    let status = run_json_command(
        &fixture,
        vec![
            "--json".to_string(),
            "--workspace".to_string(),
            workspace.clone(),
            "status".to_string(),
        ],
    )?;
    assert_contract_snapshot("status_json_contract", status);

    let doctor = run_json_command(
        &fixture,
        vec![
            "--json".to_string(),
            "--workspace".to_string(),
            workspace.clone(),
            "--fields".to_string(),
            "standard".to_string(),
            "doctor".to_string(),
            "--full".to_string(),
        ],
    )?;
    assert_contract_snapshot("doctor_json_contract", doctor);

    let search = run_json_command(
        &fixture,
        vec![
            "--json".to_string(),
            "--workspace".to_string(),
            workspace.clone(),
            "search".to_string(),
            QUERY.to_string(),
            "--database".to_string(),
            database.clone(),
            "--index-dir".to_string(),
            index_dir.clone(),
        ],
    )?;
    assert_contract_snapshot("search_json_contract", search);

    let why = run_json_command(
        &fixture,
        vec![
            "--json".to_string(),
            "--workspace".to_string(),
            workspace.clone(),
            "why".to_string(),
            MEMORY_ID.to_string(),
            "--database".to_string(),
            database.clone(),
        ],
    )?;
    assert_contract_snapshot("why_json_contract", why);

    let context = run_json_command(
        &fixture,
        vec![
            "--json".to_string(),
            "--workspace".to_string(),
            workspace,
            "pack".to_string(),
            QUERY.to_string(),
            "--database".to_string(),
            database,
            "--index-dir".to_string(),
            index_dir,
            "--profile".to_string(),
            "compact".to_string(),
            "--max-tokens".to_string(),
            "4000".to_string(),
            "--candidate-pool".to_string(),
            "10".to_string(),
        ],
    )?;
    assert_contract_snapshot("context_json_contract", context);

    // Profile config plan contract - dry-run mode produces stable JSON showing
    // selected profile, budgets, planned TOML edits, and host probe summary.
    let profile_config_plan = run_profile_json_command(
        &fixture,
        vec![
            "--json".to_string(),
            "--workspace".to_string(),
            fixture.workspace_arg(),
            "profile".to_string(),
            "config".to_string(),
            "plan".to_string(),
            "--profile".to_string(),
            "portable".to_string(),
            "--config".to_string(),
            fixture.profile_config_arg(),
        ],
    )?;
    assert_contract_snapshot("profile_config_plan_json_contract", profile_config_plan);

    let missing_config_apply = run_profile_json_command(
        &fixture,
        vec![
            "--json".to_string(),
            "--workspace".to_string(),
            fixture.workspace_arg(),
            "profile".to_string(),
            "config".to_string(),
            "apply".to_string(),
            "--dry-run".to_string(),
            "--profile".to_string(),
            "portable".to_string(),
            "--config".to_string(),
            fixture.profile_config_arg(),
        ],
    )?;
    ensure_profile_apply_dry_run_shape(&missing_config_apply, false, true)?;

    let applied_config = run_profile_json_command(
        &fixture,
        vec![
            "--json".to_string(),
            "--workspace".to_string(),
            fixture.workspace_arg(),
            "profile".to_string(),
            "config".to_string(),
            "apply".to_string(),
            "--profile".to_string(),
            "portable".to_string(),
            "--config".to_string(),
            fixture.profile_config_arg(),
        ],
    )?;
    ensure_json_bool(&applied_config, "/data/applied", true)?;

    let existing_config_apply = run_profile_json_command(
        &fixture,
        vec![
            "--json".to_string(),
            "--workspace".to_string(),
            fixture.workspace_arg(),
            "profile".to_string(),
            "config".to_string(),
            "apply".to_string(),
            "--dry-run".to_string(),
            "--profile".to_string(),
            "portable".to_string(),
            "--config".to_string(),
            fixture.profile_config_arg(),
        ],
    )?;
    ensure_profile_apply_dry_run_shape(&existing_config_apply, true, false)?;

    let profile_config_apply = json!({
        "missingConfigDryRun": missing_config_apply,
        "existingConfigDryRun": existing_config_apply,
    });
    assert_contract_snapshot("profile_config_apply_json_contract", profile_config_apply);

    Ok(())
}

fn ensure_profile_apply_dry_run_shape(
    value: &Value,
    expected_config_exists: bool,
    expected_would_write: bool,
) -> TestResult {
    ensure_json_bool(value, "/data/dryRun", true)?;
    ensure_json_bool(value, "/data/applied", false)?;
    ensure_json_bool(value, "/data/configExists", expected_config_exists)?;
    ensure_json_bool(value, "/data/wouldWrite", expected_would_write)
}

fn ensure_json_bool(value: &Value, pointer: &str, expected: bool) -> TestResult {
    let actual = value
        .pointer(pointer)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("missing boolean field {pointer}"))?;
    if actual != expected {
        return Err(format!("expected {pointer} to be {expected}, got {actual}"));
    }
    Ok(())
}

/// Run a profile command and scrub host-specific values that vary between machines.
fn run_profile_json_command(
    fixture: &JsonContractFixture,
    args: Vec<String>,
) -> Result<Value, String> {
    let output = run_ee(fixture, &args)?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("stdout was not UTF-8 for ee {}: {error}", args.join(" ")))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("stderr was not UTF-8 for ee {}: {error}", args.join(" ")))?;

    if !output.status.success() {
        return Err(format!(
            "ee {} failed with status {:?}; stderr: {stderr}; stdout: {stdout}",
            args.join(" "),
            output.status.code()
        ));
    }
    if !stderr.is_empty() {
        return Err(format!(
            "ee {} must keep JSON diagnostics out of stderr, got: {stderr:?}",
            args.join(" ")
        ));
    }
    if !stdout.ends_with('\n') {
        return Err(format!(
            "ee {} stdout must be newline-terminated JSON, got: {stdout:?}",
            args.join(" ")
        ));
    }

    let mut value: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("ee {} stdout must be JSON: {error}", args.join(" ")))?;
    scrub_json_contract(&mut value, fixture)?;
    scrub_profile_host_specific(&mut value);
    Ok(value)
}

/// Scrub host-specific values from profile probe output that vary between machines.
fn scrub_profile_host_specific(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, child) in object.iter_mut() {
                scrub_profile_host_specific(child);
                // Scrub memory/CPU values that vary by host
                if (key == "logicalCores" || key == "physicalCores") && child.is_number() {
                    *child = serde_json::json!(0);
                }
                if (key == "totalBytes" || key == "availableBytes" || key == "cgroupLimitBytes")
                    && child.is_number()
                {
                    *child = serde_json::json!(0);
                }
                // Scrub profile recommendation that depends on host resources
                if (key == "recommended" || key == "effective") && child.is_string() {
                    *child = Value::String("[PROFILE]".to_string());
                }
                if key == "confidence" && child.is_string() {
                    *child = Value::String("[HOST_CONFIDENCE]".to_string());
                }
                // Scrub budget values that scale with profile
                if is_profile_budget_key(key) && child.is_number() {
                    *child = serde_json::json!(0);
                }
                // Scrub reasons array which contains host-specific text
                if key == "reasons" {
                    if let Value::Array(_) = child {
                        *child = Value::Array(vec![Value::String("[HOST_REASON]".to_string())]);
                    }
                }
            }
            if let Some(probe) = object.get_mut("probe") {
                scrub_profile_probe(probe);
            }
        }
        Value::Array(items) => {
            for item in items {
                scrub_profile_host_specific(item);
            }
        }
        _ => {}
    }
}

fn scrub_profile_probe(value: &mut Value) {
    let Some(probe) = value.as_object_mut() else {
        return;
    };

    probe.insert("complete".to_string(), json!(false));
    probe.insert("degraded".to_string(), json!([]));

    if let Some(cpu) = probe.get_mut("cpu").and_then(Value::as_object_mut) {
        cpu.insert("logicalCores".to_string(), json!(0));
        cpu.insert("physicalCores".to_string(), Value::Null);
    }
    if let Some(memory) = probe.get_mut("memory").and_then(Value::as_object_mut) {
        memory.insert("availableBytes".to_string(), json!(0));
        memory.insert("cgroupLimitBytes".to_string(), Value::Null);
        memory.insert(
            "source".to_string(),
            Value::String("[HOST_MEMORY_SOURCE]".to_string()),
        );
        memory.insert("totalBytes".to_string(), json!(0));
    }
    if let Some(environment) = probe.get_mut("environment").and_then(Value::as_object_mut) {
        environment.insert("cargoTargetDirConfigured".to_string(), json!(false));
        environment.insert("rchHintConfigured".to_string(), json!(false));
        environment.insert("tmpdirConfigured".to_string(), json!(false));
    }
    if let Some(paths) = probe.get_mut("paths").and_then(Value::as_array_mut) {
        for path in paths {
            let Some(path) = path.as_object_mut() else {
                continue;
            };
            path.insert("availableBytes".to_string(), json!(0));
            path.insert("exists".to_string(), json!(false));
            path.insert("nearestExistingAncestor".to_string(), json!(false));
            path.insert("probeStatus".to_string(), json!("observed"));
            path.insert("sameFilesystemAsWorkspace".to_string(), json!(false));
            path.insert("totalBytes".to_string(), json!(0));
        }
    }
    if let Some(tools) = probe.get_mut("tools").and_then(Value::as_array_mut) {
        for tool in tools {
            if let Some(tool) = tool.as_object_mut() {
                tool.insert("available".to_string(), json!(false));
            }
        }
    }
    if let Some(rch) = probe
        .get_mut("topology")
        .and_then(Value::as_object_mut)
        .and_then(|topology| topology.get_mut("rch"))
        .and_then(Value::as_object_mut)
    {
        rch.insert("available".to_string(), json!(false));
        rch.insert(
            "message".to_string(),
            Value::String("[HOST_RCH_MESSAGE]".to_string()),
        );
        rch.insert(
            "posture".to_string(),
            Value::String("[HOST_RCH_POSTURE]".to_string()),
        );
        rch.insert(
            "repair".to_string(),
            Value::String("[HOST_RCH_REPAIR]".to_string()),
        );
        rch.insert(
            "status".to_string(),
            Value::String("[HOST_RCH_STATUS]".to_string()),
        );
    }
}

fn is_profile_budget_key(key: &str) -> bool {
    matches!(
        key,
        "candidateLimit"
            | "concurrentIndexReaders"
            | "maxTokens"
            | "maxCandidateMemories"
            | "memoryCapMb"
            | "entryCap"
            | "hotsetPrewarmLimit"
            | "queueCap"
            | "batchCap"
            | "retryBudget"
            | "maintenanceWindowMs"
            | "graphRefreshBudget"
    )
}

/// bd-jikgj (found by TurquoiseBirch): the acceptance property, stated as a
/// test rather than as a comment.
///
/// Two captures of the SAME pack -- one from a host that stayed inside its
/// elapsed budget, one from a host that did not -- must normalize to the same
/// document. If they do, the snapshot cannot depend on how loaded the machine
/// was when it ran.
///
/// The slow-host fixture is the shape `pack_assembly_elapsed_degradation`
/// actually emits: a `pack_assembly_elapsed_over_budget` entry carrying a raw
/// millisecond reading in its message, appended to the response `degraded[]`
/// after the pack hash is computed, with every derived count one higher and
/// the bullet rendered into the markdown body.
///
/// Two deterministic degradations, not one: the renderer pluralizes "signal"
/// from the same count it prints, so a one-entry fixture would compare
/// "1 degraded signals" -- prose the renderer never emits -- against a
/// correctly singular normalization, and fail for a reason that has nothing to
/// do with host dependence. The real recorded snapshot carries two.
#[test]
fn timing_degradations_read_the_same_on_a_fast_and_a_slow_host() -> TestResult {
    let embed = json!({
        "code": "embed_model_unavailable",
        "severity": "warning",
        "message": "Embedding model unavailable; semantic similarity is disabled.",
    });
    let freshness = json!({
        "code": "context_evidence_freshness_missing_source",
        "severity": "low",
        "message": "Memory evidence freshness is missing_source.",
    });
    let timing = json!({
        "code": ee::pack::PACK_ASSEMBLY_ELAPSED_OVER_BUDGET_CODE,
        "severity": "low",
        "message": "Pack assembly took 812ms, at or over the standard \
                    resource-profile elapsed warning threshold of 500ms. \
                    The pack contents are unaffected.",
        "repair": "Re-run to see whether the overrun is repeatable.",
    });

    let deterministic_markdown = "## Degradations\n\n\
        - **[warning]** Embedding model unavailable; semantic similarity is disabled.\n  \
        - *Repair:* `ee index reembed`\n\
        - **[low]** Memory evidence freshness is missing_source.\n  \
        - *Repair:* `Reinstate the file.`\n";
    let slow_markdown = format!(
        "{deterministic_markdown}\
         - **[low]** Pack assembly took 812ms, at or over the standard \
         resource-profile elapsed warning threshold of 500ms. The pack contents \
         are unaffected.\n  \
         - *Repair:* `Re-run to see whether the overrun is repeatable.`\n"
    );

    let document = |entries: Value, count: u64, markdown: &str| {
        let sentence = format!(
            "Context includes {count} degraded signals; semantic embedding is unavailable."
        );
        json!({
            "degraded": entries,
            "data": {
                "degraded": entries,
                "pack": {
                    "advisoryBanner": {
                        "degradationCount": count,
                        "summary": sentence,
                    },
                    "text": format!("{sentence}\n\n{markdown}"),
                }
            }
        })
    };

    let fast = document(json!([embed, freshness]), 2, deterministic_markdown);
    let slow = document(json!([embed, freshness, timing]), 3, &slow_markdown);

    // The fixtures must actually differ, or this test would pass against a
    // normalization that does nothing at all.
    if fast == slow {
        return Err("fixtures are identical before normalization; the test proves nothing".into());
    }

    let mut fast_normalized = fast.clone();
    let mut slow_normalized = slow.clone();
    normalize_timing_degradations(&mut fast_normalized);
    normalize_timing_degradations(&mut slow_normalized);

    // The normalization must BITE on the slow document, not silently no-op.
    if slow_normalized == slow {
        return Err(format!(
            "slow-host document was unchanged by normalization; the timing entry survived:\n{slow_normalized:#}"
        ));
    }
    // ...and must leave a document that never had a timing entry alone.
    if fast_normalized != fast {
        return Err(format!(
            "fast-host document must be untouched, but normalization changed it:\n{fast_normalized:#}"
        ));
    }

    if fast_normalized != slow_normalized {
        return Err(format!(
            "host-dependent snapshot: fast and slow captures normalized differently\n\
             fast:\n{fast_normalized:#}\n\nslow:\n{slow_normalized:#}"
        ));
    }

    // The deterministic degradations must SURVIVE. A normalization that simply
    // emptied `degraded[]` would satisfy every assertion above.
    let surviving = slow_normalized
        .pointer("/data/degraded")
        .and_then(Value::as_array)
        .ok_or("normalized document lost /data/degraded")?;
    let codes: Vec<&str> = surviving
        .iter()
        .filter_map(|entry| entry.get("code").and_then(Value::as_str))
        .collect();
    if codes
        != [
            "embed_model_unavailable",
            "context_evidence_freshness_missing_source",
        ]
    {
        return Err(format!(
            "deterministic degradations must survive normalization, got: {codes:?}"
        ));
    }

    // The raw millisecond reading must be gone from the rendered body too --
    // it rides in the message, not in a key the scrubber can see.
    let text = slow_normalized
        .pointer("/data/pack/text")
        .and_then(Value::as_str)
        .ok_or("normalized document lost /data/pack/text")?;
    if text.contains(TIMING_DEGRADED_MESSAGE_PREFIX) {
        return Err(format!(
            "timing bullet survived in the markdown body:\n{text}"
        ));
    }

    // Print what was found rather than only comparing.
    println!("normalized slow-host degraded codes: {codes:?}");
    println!("normalized slow-host body:\n{text}");

    Ok(())
}

/// Build a pack SLO response whose only variable is how long the pack took.
///
/// Thresholds are the shape `PackSloBudgetClass` publishes -- positive and
/// strictly ordered target < warning < failure -- because the validator
/// rejects anything else before it touches a field, and a fixture it rejects
/// would prove nothing about the normalization.
fn slo_document(elapsed_ms: u64, elapsed_status: &str, status: &str) -> Value {
    json!({
        "data": {
            "pack": {
                "slo": {
                    "schema": "ee.pack.slo.v1",
                    "profile": "standard",
                    "degradations": [],
                    "actuals": {
                        "candidateCount": 1,
                        "elapsedMs": elapsed_ms,
                        "graphEdgesTraversed": 0,
                        "memoryBytesPeak": 755,
                        "scannedCount": 1,
                    },
                    "budgetClass": {
                        "candidatesScannedMax": 240,
                        "concurrentPackMax": 4,
                        "elapsedMsTarget": 200,
                        "elapsedMsWarning": 500,
                        "elapsedMsFailure": 2000,
                        "graphTraversalMaxEdges": 8192,
                    },
                    "resourceStatus": "within_budget",
                    "elapsedStatus": elapsed_status,
                    "status": status,
                }
            }
        }
    })
}

/// The pack SLO is the second publication of the same wall clock, and it has
/// to converge across hosts for the same reason `degraded[]` does.
///
/// `slo.elapsedStatus` is `for_elapsed_ms(elapsed_ms, budget)` and `slo.status`
/// is the worst of it and `resourceStatus`, so an idle laptop and a loaded
/// worker disagree on both while every deterministic field agrees. This proves
/// the normalization makes them agree, that it does not simply blank the SLO,
/// and -- the part that matters most -- that it REFUSES a reading whose status
/// contradicts its own measurement instead of scrubbing the contradiction away.
#[test]
fn pack_slo_reads_the_same_on_a_fast_and_a_slow_host() -> TestResult {
    // 120ms is under the 200ms target; 812ms is at or over the 500ms warning
    // and under the 2000ms failure. Same request, two hosts.
    let fast = slo_document(120, "within_budget", "within_budget");
    let slow = slo_document(812, "warning", "warning");

    // Negative control: a normalization that did nothing would pass every
    // convergence check below if the inputs were already equal.
    if fast == slow {
        return Err("fixtures are identical before normalization; the test proves nothing".into());
    }

    let mut fast_normalized = fast.clone();
    let mut slow_normalized = slow.clone();
    if !normalize_pack_slo_measurements(&mut fast_normalized)? {
        return Err("fast fixture was not recognized as carrying a pack SLO".into());
    }
    if !normalize_pack_slo_measurements(&mut slow_normalized)? {
        return Err("slow fixture was not recognized as carrying a pack SLO".into());
    }

    // It has to BITE on the slow document. Unlike the `degraded[]`
    // normalization it also rewrites the fast one -- the measurement is
    // volatile whatever it reads -- so "unchanged on fast" is NOT the property
    // here, and asserting it would red on a working normalization.
    if slow_normalized == slow {
        return Err(format!(
            "slow-host SLO was unchanged by normalization:\n{slow_normalized:#}"
        ));
    }
    if fast_normalized != slow_normalized {
        return Err(format!(
            "host-dependent snapshot: fast and slow SLOs normalized differently\n\
             fast:\n{fast_normalized:#}\n\nslow:\n{slow_normalized:#}"
        ));
    }

    let slo = slow_normalized
        .pointer("/data/pack/slo")
        .ok_or("normalized document lost /data/pack/slo")?;

    // The three measurements go...
    let leaked: Vec<&str> = ["/status", "/elapsedStatus", "/actuals/elapsedMs"]
        .into_iter()
        .filter(|pointer| slo.pointer(pointer).is_some())
        .collect();
    if !leaked.is_empty() {
        return Err(format!(
            "host-dependent SLO measurements survived normalization: {leaked:?}\n{slo:#}"
        ));
    }

    // ...and the deterministic evidence stays. Emptying the SLO outright would
    // satisfy every assertion above.
    let missing: Vec<&str> = [
        "/resourceStatus",
        "/schema",
        "/profile",
        "/budgetClass/elapsedMsWarning",
        "/budgetClass/elapsedMsFailure",
        "/actuals/candidateCount",
        "/actuals/memoryBytesPeak",
        "/actuals/scannedCount",
    ]
    .into_iter()
    .filter(|pointer| slo.pointer(pointer).is_none())
    .collect();
    if !missing.is_empty() {
        return Err(format!(
            "deterministic SLO evidence must survive normalization, missing: {missing:?}\n{slo:#}"
        ));
    }

    // The trade that makes the dropped literals safe: a status that disagrees
    // with its own measurement is REJECTED, and nothing is mutated. If this
    // ever passed, the contract test would be scrubbing a real defect.
    let mut inconsistent = slo_document(812, "within_budget", "within_budget");
    let before = inconsistent.clone();
    match normalize_pack_slo_measurements(&mut inconsistent) {
        Ok(_) => {
            return Err(
                "an elapsedStatus contradicting elapsedMs was accepted; the contract test would \
                 scrub the contradiction instead of failing on it"
                    .into(),
            );
        }
        Err(message) => {
            if inconsistent != before {
                return Err(format!(
                    "a rejected SLO must be left verbatim, but it was mutated:\n{inconsistent:#}"
                ));
            }
            println!("inconsistent SLO correctly rejected: {message}");
        }
    }

    // A response with no pack SLO at all is reported as such, not as an error:
    // `search`, `why`, `doctor` and `status` share this scrub path.
    let mut sloless = json!({"data": {"results": []}});
    if normalize_pack_slo_measurements(&mut sloless)? {
        return Err("a response without a pack SLO must report false".into());
    }

    println!("normalized SLO: {slo:#}");

    Ok(())
}

/// The count is re-pluralized, because the renderer pluralizes from the same
/// number it prints. Dropping the timing entry from a two-signal response must
/// yield "1 degraded signal", never "1 degraded signals".
#[test]
fn renumbering_degraded_prose_repluralizes_at_the_singular_boundary() -> TestResult {
    let renumbered = renumber_degraded_signal_prose(
        "Context includes 2 degraded signals; semantic embedding is unavailable.",
        1,
    );
    if renumbered != "Context includes 1 degraded signal; semantic embedding is unavailable." {
        return Err(format!(
            "singular boundary not handled, got: {renumbered:?}"
        ));
    }

    // A sentence with no count must pass through untouched.
    let untouched = renumber_degraded_signal_prose("Context is clear.", 1);
    if untouched != "Context is clear." {
        return Err(format!("unrelated prose was rewritten, got: {untouched:?}"));
    }

    println!("renumbered: {renumbered}");
    Ok(())
}
