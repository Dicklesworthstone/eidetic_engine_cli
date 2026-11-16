//! bd-resume-verb-v0f57: structural contract for the resume wire schema
//! (`ee.resume.v1`).
//!
//! Pins schema identity, `public_schemas()` registry wiring, the report's
//! required field set, bounded open-loop accounting, and per-item
//! provenance/redaction/staleness posture, so surface drift fails loudly. Follows
//! `graph_suggest_links_schema.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use ee::core::resume::{ResumeOptions, build_resume_report};
use ee::core::workspace::stable_workspace_id;
use ee::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};
use ee::output::{public_schemas, render_schema_export_json};
use ee::testing::validate_json_schema_instance;
use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_ID: &str = "ee.resume.v1";
const SCHEMA_REL: &str = "docs/schemas/ee.resume.v1.json";

fn load_schema() -> Result<Value, String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SCHEMA_REL);
    let bytes =
        std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice::<Value>(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn resume_test_tempdir(prefix: &str) -> Result<tempfile::TempDir, String> {
    let canonical_temp_root = std::env::temp_dir()
        .canonicalize()
        .map_err(|error| format!("canonicalize test temp root: {error}"))?;
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(canonical_temp_root)
        .map_err(|error| format!("create test temp directory: {error}"))
}

fn run_real_ee_with_registry(args: &[String], registry: &Path) -> Result<Output, String> {
    // Each invocation spawns a full `ee` binary whose bounded nearby-store
    // scan runs on a wall-clock budget. libtest's default parallelism lets
    // the real-binary bridges compete for the same loaded-machine CPU, so
    // all real-binary spawns are serialized through the crate-wide gate in
    // common_spawn (bd-7vtqm). Measured outcome (2026-08-23): serialization
    // alone does NOT fix the retention bridge — it still truncated 4/4
    // attempts on an RCH worker with the mutex active, so the truncation is
    // systematic, not a scheduling lottery. Root-cause analysis and the
    // proposed discovery-budget fix are tracked on bd-resume-verb-v0f57.
    // Assertions are untouched.
    crate::common_spawn::serialized_real_ee_with(|command| {
        command
            .env("EE_EMBED_DOWNLOAD", "off")
            .env("EE_WORKSPACE_REGISTRY", registry)
            .args(args);
    })
    .map_err(|error| format!("launch real ee {}: {error}", args.join(" ")))
}

fn real_ee_stdout_json(output: &Output, label: &str) -> Result<Value, String> {
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "parse {label} stdout as JSON: {error}; stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn ensure_real_ee_success(output: &Output, label: &str) -> TestResult {
    ensure(
        output.status.success(),
        format!(
            "{label} failed with {:?}; stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

/// Loaded machines can starve any single bounded nearby-store scan of
/// wall-clock time before it probes the seeded candidate. The capability
/// under test is retention of the locally proved retarget, not scheduler
/// luck, so each real-binary invocation may be retried this many times;
/// every attempt must satisfy the identical assertions and only a fully
/// proved attempt ends the loop. Assertions themselves are never relaxed.
const REAL_BINARY_RESUME_ATTEMPTS: usize = 4;

fn run_until_proved<T>(
    attempts: usize,
    mut attempt: impl FnMut() -> Result<T, String>,
) -> Result<T, String> {
    let mut last_error = String::new();
    for _ in 0..attempts {
        match attempt() {
            Ok(proved) => return Ok(proved),
            Err(error) => last_error = error,
        }
    }
    Err(format!(
        "real-binary resume proved nothing within {attempts} attempts; last error: {last_error}"
    ))
}

fn string_set(value: &Value, pointer: &str) -> Result<BTreeSet<String>, String> {
    let array = value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("schema is missing array at {pointer}"))?;
    let mut out = BTreeSet::new();
    for entry in array {
        out.insert(
            entry
                .as_str()
                .ok_or_else(|| format!("{pointer} contains non-string entry: {entry}"))?
                .to_owned(),
        );
    }
    Ok(out)
}

#[test]
fn resume_schema_identity_and_registry_are_pinned() -> TestResult {
    let schema = load_schema()?;
    ensure(
        schema.pointer("/title").and_then(Value::as_str) == Some(SCHEMA_ID),
        "schema title must equal its id",
    )?;
    ensure(
        schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str)
            == Some(SCHEMA_ID),
        "properties.schema.const must pin the id",
    )?;

    let registry = public_schemas();
    let entry = registry
        .iter()
        .find(|entry| entry.id == SCHEMA_ID)
        .ok_or("public schema registry missing ee.resume.v1")?;
    ensure(entry.version == "1", "registry version must be 1")?;
    ensure(
        entry.category == "memory",
        "registry category must be memory",
    )?;
    let exported: Value = serde_json::from_str(&render_schema_export_json(Some(SCHEMA_ID)))
        .map_err(|error| format!("registry export did not parse: {error}"))?;
    ensure(
        exported.pointer("/title").and_then(Value::as_str) == Some(SCHEMA_ID),
        "registry definition must embed the schema",
    )
}

#[test]
fn resume_required_fields_and_staleness_contract_are_pinned() -> TestResult {
    let schema = load_schema()?;

    let required = string_set(&schema, "/required")?;
    let expected: BTreeSet<String> = [
        "schema",
        "workspaceId",
        "episodicTotal",
        "sessions",
        "openLoops",
        "staleCount",
        "nearbyStores",
        "nextCommands",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        required == expected,
        format!("report required set drifted: {required:?}"),
    )?;

    let open_loops = string_set(&schema, "/properties/openLoops/required")?;
    let expected_loops: BTreeSet<String> = [
        "revisitDecisionsTotal",
        "revisitDecisionsTruncated",
        "revisitDecisions",
        "taggedItemsTotal",
        "taggedItemsTruncated",
        "taggedItems",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        open_loops == expected_loops,
        format!("openLoops required set drifted: {open_loops:?}"),
    )?;
    for pointer in [
        "/properties/openLoops/properties/revisitDecisions/maxItems",
        "/properties/openLoops/properties/taggedItems/maxItems",
    ] {
        ensure(
            schema.pointer(pointer).and_then(Value::as_u64) == Some(32),
            format!("resume open-loop bound drifted at {pointer}"),
        )?;
    }
    for (pointer, expected) in [
        ("/properties/sessions/maxItems", 64),
        ("/properties/sessions/items/properties/items/maxItems", 20),
        ("/properties/nearbyStores/properties/stores/maxItems", 5),
        ("/properties/nextCommands/maxItems", 5),
    ] {
        ensure(
            schema.pointer(pointer).and_then(Value::as_u64) == Some(expected),
            format!("resume public bound drifted at {pointer}"),
        )?;
    }
    ensure(
        schema
            .pointer("/properties/nextCommands/minItems")
            .and_then(Value::as_u64)
            == Some(3),
        "resume must always expose its three base next commands",
    )?;

    // Every surfaced item must carry the stale field (nullable), and the
    // flag itself must name what superseded the item and why.
    let item_required = string_set(&schema, "/$defs/item/required")?;
    ensure(
        item_required.contains("stale"),
        "item.stale must be a required (nullable) field",
    )?;
    ensure(
        item_required.contains("selectionReason")
            && item_required.contains("provenance")
            && item_required.contains("redaction"),
        "every resume item must carry selection, safe provenance, and redaction posture",
    )?;
    let provenance_required = string_set(&schema, "/$defs/provenance/required")?;
    let expected_provenance: BTreeSet<String> = ["uri", "trustClass", "verificationStatus"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    ensure(
        provenance_required == expected_provenance,
        format!("resume provenance contract drifted: {provenance_required:?}"),
    )?;
    ensure(
        schema
            .pointer("/$defs/provenance/properties/uri/type")
            .and_then(Value::as_str)
            == Some("string"),
        "resume provenance URI must be explicit; admitted memories use an ee-mem fallback",
    )?;
    let redaction_required = string_set(&schema, "/$defs/redaction/required")?;
    let expected_redaction: BTreeSet<String> = ["applied", "reasons"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    ensure(
        redaction_required == expected_redaction,
        format!("resume redaction contract drifted: {redaction_required:?}"),
    )?;
    let decision_required = string_set(
        &schema,
        "/properties/openLoops/properties/revisitDecisions/items/required",
    )?;
    ensure(
        decision_required.contains("provenance") && decision_required.contains("redaction"),
        "every resume decision must carry provenance and redaction posture",
    )?;
    ensure(
        schema
            .pointer(
                "/properties/openLoops/properties/revisitDecisions/items/properties/revisitStatus/enum",
            )
            .and_then(Value::as_array)
            .is_some_and(|values| values.len() == 4),
        "resume decision revisitStatus vocabulary must stay closed",
    )?;
    let stale_required = string_set(&schema, "/$defs/item/properties/stale/required")?;
    let expected_stale: BTreeSet<String> = ["supersededBy", "supersededByCreatedAt", "sharedTags"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    ensure(
        stale_required == expected_stale,
        format!("staleness contract drifted: {stale_required:?}"),
    )?;
    ensure(
        schema
            .pointer("/properties/staleCount/description")
            .and_then(Value::as_str)
            .is_some_and(|description| {
                description.contains("unique stale memory IDs")
                    && description.contains("counts once")
            }),
        "staleCount must document unique-memory counting across projections",
    )?;
    ensure(
        schema
            .pointer("/$defs/item/properties/stale/properties/sharedTags/minItems")
            .and_then(Value::as_u64)
            == Some(1),
        "stale.sharedTags must require at least one subject tag",
    )?;
    ensure(
        schema
            .pointer("/$defs/item/properties/stale/properties/sharedTags/description")
            .and_then(Value::as_str)
            .is_some_and(|description| {
                description.contains("subject tags")
                    && description.contains("session-*")
                    && description.contains("next/queue/blocking/pending/todo/revisit")
            }),
        "stale.sharedTags must exclude session and open-loop control tags",
    )?;

    let nearby_required = string_set(&schema, "/properties/nearbyStores/required")?;
    let expected_nearby: BTreeSet<String> = ["stores", "outcome"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    ensure(
        nearby_required == expected_nearby,
        format!("nearbyStores required set drifted: {nearby_required:?}"),
    )?;
    let outcomes = string_set(&schema, "/properties/nearbyStores/properties/outcome/enum")?;
    let expected_outcomes: BTreeSet<String> = [
        "complete",
        "truncated",
        "truncated_registry_unavailable",
        "unavailable",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        outcomes == expected_outcomes,
        format!("nearbyStores outcome vocabulary drifted: {outcomes:?}"),
    )?;
    ensure(
        schema
            .pointer("/properties/nearbyStores/properties/truncated")
            .is_none(),
        "resume must not collapse nearby-store outcome into legacy truncated",
    )?;
    ensure(
        schema
            .pointer("/properties/nearbyStores/description")
            .and_then(Value::as_str)
            .is_some_and(|description| {
                description.contains("unavailable") && description.contains("no populated store")
            }),
        "nearbyStores must deny a no-store conclusion when discovery is unavailable",
    )?;
    ensure(
        schema
            .pointer("/properties/nearbyStores/properties/stores/items/additionalProperties")
            .and_then(Value::as_bool)
            == Some(false),
        "nearby store entries must reject undeclared fields",
    )?;
    ensure(
        schema
            .pointer("/$defs/memoryId/pattern")
            .and_then(Value::as_str)
            == Some("^mem_[0-7][0-9A-HJKMNP-TV-Z]{25}$"),
        "resume memory IDs must use the canonical typed-id contract",
    )?;
    ensure(
        schema
            .pointer("/$defs/publicText/maxLength")
            .and_then(Value::as_u64)
            == Some(4096),
        "resume public text must stay within the public-replay scan bound",
    )
}

#[test]
fn real_core_resume_report_validates_against_public_schema() -> TestResult {
    let temp = resume_test_tempdir("ee-resume-schema-core.")?;
    let workspace = temp.path().join("workspace");
    let store = workspace.join(".ee");
    std::fs::create_dir_all(&store)
        .map_err(|error| format!("create {}: {error}", store.display()))?;
    let canonical_workspace = workspace
        .canonicalize()
        .map_err(|error| format!("canonicalize {}: {error}", workspace.display()))?;
    let workspace_id = stable_workspace_id(&canonical_workspace);
    let database = store.join("ee.db");
    let connection = DbConnection::open_file(&database)
        .map_err(|error| format!("open {}: {error}", database.display()))?;
    connection
        .migrate()
        .map_err(|error| format!("migrate resume schema store: {error}"))?;
    connection
        .insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: canonical_workspace.display().to_string(),
                name: Some("resume-schema-core".to_owned()),
            },
        )
        .map_err(|error| format!("insert resume schema workspace: {error}"))?;

    for (seed, tag) in [
        (0x52534d51_u128, "session-public-a"),
        (0x52534d52_u128, "session-public-b"),
        (0x52534d53_u128, "session-public-c"),
    ] {
        connection
            .insert_memory(
                &ee::models::MemoryId::from_uuid(uuid::Uuid::from_u128(seed)).to_string(),
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "episodic".to_owned(),
                    kind: "note".to_owned(),
                    content: format!("Public resume evidence for {tag}."),
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some(format!("test://resume-schema/{tag}")),
                    trust_class: "agent_assertion".to_owned(),
                    trust_subclass: None,
                    tags: vec![tag.to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| format!("insert {tag}: {error}"))?;
    }
    connection
        .insert_memory(
            &ee::models::MemoryId::from_uuid(uuid::Uuid::from_u128(0x52534d54)).to_string(),
            &CreateMemoryInput {
                workspace_id: workspace_id.clone(),
                level: "semantic".to_owned(),
                kind: "decision".to_owned(),
                content: "Topic: Resume schema admission\nChosen: validate emitted reports\nRevisit by: 2099-12-31T00:00:00Z".to_owned(),
                workflow_id: None,
                confidence: 0.8,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: Some("test://resume-schema/decision".to_owned()),
                trust_class: "agent_assertion".to_owned(),
                trust_subclass: None,
                tags: vec!["next".to_owned(), "resume-schema".to_owned()],
                valid_from: None,
                valid_to: None,
            },
        )
        .map_err(|error| format!("insert resume schema decision: {error}"))?;
    drop(connection);

    let report = build_resume_report(&ResumeOptions {
        workspace_path: &canonical_workspace,
        database_path: &database,
        sessions: 3,
    })
    .map_err(|error| format!("build real core resume report: {error}"))?;
    ensure(
        report.sessions.len() == 3,
        format!("real core report did not surface all three tagged sessions: {report:?}"),
    )?;
    ensure(
        report.open_loops.revisit_decisions.len() == 1,
        format!("real core report omitted the revisit decision: {report:?}"),
    )?;
    let emitted = serde_json::to_value(&report)
        .map_err(|error| format!("serialize real core resume report: {error}"))?;
    validate_json_schema_instance(&emitted, &load_schema()?).map_err(|error| {
        format!("real core ee.resume.v1 report failed public schema validation: {error}; {emitted}")
    })
}

#[test]
fn real_binary_resume_retains_locally_proved_candidate_when_registry_is_unavailable() -> TestResult
{
    let temp = resume_test_tempdir("ee-resume-registry-partial.")?;
    let cold_workspace = temp.path().join("cold-workspace");
    let candidate_workspace = cold_workspace.join("proved-candidate");
    std::fs::create_dir_all(cold_workspace.join(".git"))
        .map_err(|error| format!("create cold workspace: {error}"))?;
    std::fs::create_dir_all(&candidate_workspace)
        .map_err(|error| format!("create candidate workspace: {error}"))?;

    let seed_registry = temp.path().join("seed-registry.db");
    let candidate_text = candidate_workspace.display().to_string();
    for (label, args) in [
        (
            "real ee init for resume candidate",
            vec![
                "init".to_owned(),
                "--workspace".to_owned(),
                candidate_text.clone(),
                "--json".to_owned(),
            ],
        ),
        (
            "real ee remember for resume candidate",
            vec![
                "remember".to_owned(),
                "Locally proved resume candidate.".to_owned(),
                "--workspace".to_owned(),
                candidate_text,
                "--level".to_owned(),
                "episodic".to_owned(),
                "--kind".to_owned(),
                "note".to_owned(),
                "--json".to_owned(),
            ],
        ),
    ] {
        let output = run_real_ee_with_registry(&args, &seed_registry)?;
        ensure_real_ee_success(&output, label)?;
    }

    let invalid_registry = temp.path().join("invalid-registry.db");
    let invalid_registry_bytes = b"not a sqlite registry";
    std::fs::write(&invalid_registry, invalid_registry_bytes)
        .map_err(|error| format!("write invalid registry fixture: {error}"))?;
    let canonical_candidate = candidate_workspace
        .canonicalize()
        .map_err(|error| format!("canonicalize candidate workspace: {error}"))?;
    let candidate_database = canonical_candidate.join(".ee").join("ee.db");
    let canonical_candidate_text = canonical_candidate.display().to_string();
    let candidate_store_text = canonical_candidate.join(".ee").display().to_string();
    let expected_retarget = format!(
        "ee resume --workspace {} --database {} --json",
        canonical_candidate_text,
        candidate_database.display()
    );
    let cold_text = cold_workspace.display().to_string();
    let json_args = vec![
        "resume".to_owned(),
        "--workspace".to_owned(),
        cold_text.clone(),
        "--json".to_owned(),
    ];
    run_until_proved(REAL_BINARY_RESUME_ATTEMPTS, || {
        let json_output = run_real_ee_with_registry(&json_args, &invalid_registry)?;
        ensure_real_ee_success(&json_output, "real JSON ee resume with partial registry")?;
        let response = real_ee_stdout_json(&json_output, "partial-registry resume")?;
        let report = response
            .pointer("/data/report")
            .ok_or_else(|| format!("partial-registry resume omitted data.report: {response}"))?;
        let nearby_stores = report
            .pointer("/nearbyStores/stores")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("partial-registry resume omitted nearby stores: {report}"))?;
        let next_commands = report
            .pointer("/nextCommands")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("partial-registry resume omitted nextCommands: {report}"))?;
        ensure(
            report
                .pointer("/nearbyStores/outcome")
                .and_then(Value::as_str)
                == Some("truncated_registry_unavailable")
                && nearby_stores.len() == 1
                && nearby_stores[0]
                    .pointer("/workspaceRoot")
                    .and_then(Value::as_str)
                    == Some(canonical_candidate_text.as_str())
                && nearby_stores[0]
                    .pointer("/storeDir")
                    .and_then(Value::as_str)
                    == Some(candidate_store_text.as_str())
                && nearby_stores[0]
                    .pointer("/documents")
                    .and_then(Value::as_u64)
                    == Some(1)
                && nearby_stores[0]
                    .pointer("/provenance")
                    .and_then(Value::as_str)
                    == Some("child_scan")
                && next_commands.len() == 5
                && next_commands[0].as_str() == Some(expected_retarget.as_str())
                && next_commands[1].as_str()
                    == Some(
                        "ee doctor --workspace . --json  # optional workspace registry unavailable; local nearby stores remain actionable",
                    ),
            format!("partial registry resume must retain its exact proved retarget: {report}"),
        )?;
        Ok(report.clone())
    })?;

    run_until_proved(REAL_BINARY_RESUME_ATTEMPTS, || {
        let human_args = vec![
            "resume".to_owned(),
            "--workspace".to_owned(),
            cold_text.clone(),
        ];
        let human_output = run_real_ee_with_registry(&human_args, &invalid_registry)?;
        ensure_real_ee_success(&human_output, "real human ee resume with partial registry")?;
        let human = String::from_utf8(human_output.stdout)
            .map_err(|error| format!("partial-registry resume stdout was not UTF-8: {error}"))?;
        ensure(
            human.contains(
                "Nearby-store discovery outcome: truncated because the optional workspace registry was unavailable; locally proved candidates remain actionable.",
            ) && human.contains("Nearby populated stores:")
                && human.contains(&candidate_store_text)
                && human.contains(&expected_retarget)
                && !human.contains(
                    "Nearby-store discovery outcome: unavailable; an empty candidate list is not evidence that no populated store exists.",
                ),
            format!("human partial-registry resume suppressed or mislabelled its retarget: {human}"),
        )?;
        Ok(human)
    })?;
    ensure(
        !cold_workspace.join(".ee").exists()
            && std::fs::read(&invalid_registry)
                .map_err(|error| format!("read invalid registry after resume: {error}"))?
                == invalid_registry_bytes,
        "partial-registry resume must not initialize the cold store or mutate the registry",
    )
}

#[test]
fn real_binary_resume_suppresses_retarget_when_discovery_is_globally_unavailable() -> TestResult {
    let temp = resume_test_tempdir("ee-resume-registry-unavailable.")?;
    let cold_workspace = temp.path().join("cold-workspace");
    std::fs::create_dir_all(cold_workspace.join(".git"))
        .map_err(|error| format!("create globally unavailable workspace: {error}"))?;
    let invalid_registry = temp.path().join("invalid-registry.db");
    let invalid_registry_bytes = b"not a sqlite registry";
    std::fs::write(&invalid_registry, invalid_registry_bytes)
        .map_err(|error| format!("write invalid registry fixture: {error}"))?;
    let cold_text = cold_workspace.display().to_string();

    let json_args = vec![
        "resume".to_owned(),
        "--workspace".to_owned(),
        cold_text.clone(),
        "--json".to_owned(),
    ];
    let json_output = run_real_ee_with_registry(&json_args, &invalid_registry)?;
    ensure_real_ee_success(
        &json_output,
        "real JSON ee resume with globally unavailable discovery",
    )?;
    let response = real_ee_stdout_json(&json_output, "globally unavailable resume")?;
    let report = response
        .pointer("/data/report")
        .ok_or_else(|| format!("globally unavailable resume omitted data.report: {response}"))?;
    let next_commands = report
        .pointer("/nextCommands")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("globally unavailable resume omitted nextCommands: {report}"))?;
    ensure(
        report
            .pointer("/nearbyStores/outcome")
            .and_then(Value::as_str)
            == Some("unavailable")
            && report
                .pointer("/nearbyStores/stores")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
            && next_commands.len() == 4
            && next_commands.first().and_then(Value::as_str)
                == Some(
                    "ee doctor --workspace . --json  # diagnose unavailable nearby-store discovery",
                )
            && next_commands.iter().all(|command| {
                command
                    .as_str()
                    .is_some_and(|command| !command.starts_with("ee resume --workspace "))
            }),
        format!("globally unavailable resume must suppress unproved retargets: {report}"),
    )?;

    let human_args = vec!["resume".to_owned(), "--workspace".to_owned(), cold_text];
    let human_output = run_real_ee_with_registry(&human_args, &invalid_registry)?;
    ensure_real_ee_success(
        &human_output,
        "real human ee resume with globally unavailable discovery",
    )?;
    let human = String::from_utf8(human_output.stdout)
        .map_err(|error| format!("globally unavailable resume stdout was not UTF-8: {error}"))?;
    ensure(
        human.contains(
            "Nearby-store discovery outcome: unavailable; an empty candidate list is not evidence that no populated store exists.",
        ) && !human.contains("Nearby populated stores:")
            && !human.contains("ee resume --workspace ")
            && !human.contains("locally proved candidates remain actionable"),
        format!("human globally unavailable resume invented a retarget: {human}"),
    )?;
    ensure(
        !cold_workspace.join(".ee").exists()
            && std::fs::read(&invalid_registry)
                .map_err(|error| format!("read invalid registry after resume: {error}"))?
                == invalid_registry_bytes,
        "globally unavailable resume must not initialize the cold store or mutate the registry",
    )
}

const REQUIRED_FUNCTIONAL_RESUME_E2E_LABELS: &[&str] = &[
    "all_six_open_loop_tags_surfaced",
    "all_three_tagged_sessions_publicly_surfaced",
    "canonical_next_commands_execute",
    "canonical_next_commands_preserved",
    "canonical_typed_revisit_decision_surfaced",
    "cold_root_starts_uninitialized",
    "corpus_seeded",
    "decisions_recorded",
    "emitted_database_resume_executes_campaign_evidence",
    "emitted_resume_leaves_cold_root_uninitialized",
    "empty_store_reports_no_evidence",
    "human_declared_sections_visible",
    "human_open_loop_and_staleness_visible",
    "human_resume_preserves_db_wal_shm",
    "implicit_human_campaign_resume_preserves_db_wal_shm",
    "implicit_human_resume_resolves_campaign_store_under_2s",
    "implicit_resume_resolves_campaign_store",
    "init_ok",
    "json_resume_preserves_db_wal_shm",
    "missing_db_returns_empty_resume_without_initializing",
    "nearby_store_exact_complete_outcome_surfaced",
    "nearby_store_human_complete_outcome_is_explicit",
    "nearby_store_prepends_quoted_database_resume",
    "nearby_store_seeded",
    "next_only_overlap_does_not_mark_stale",
    "open_loop_totals_are_exact",
    "requested_sessions_plus_every_open_loop_in_one_resume",
    "requested_two_newest_sessions_only",
    "resume_human_redacts_planted_secret",
    "resume_items_carry_public_posture",
    "resume_json_redacts_planted_secret",
    "resume_returns_schema",
    "sessions_above_public_cap_structured_nonzero_no_mutation",
    "stale_count_deduplicates_open_loop_and_session_projections",
    "superseded_note_carries_stale_marker",
    "untagged_four_hour_boundary_and_sessions_one_truncation",
    "untagged_sessions_two_returns_both_real_groups",
    "zero_sessions_structured_nonzero_no_mutation",
];

const REQUIRED_SCALE_RESUME_E2E_LABELS: &[&str] = &[
    "fast_orient_human_never_leaks_secret_shaped_tag",
    "fast_orient_json_redacts_secret_shaped_tag",
    "scale_10k_fast_content_repeat_bytes_identical",
    "scale_10k_fast_human_orient_preserves_db_wal_shm",
    "scale_10k_fast_human_orient_under_1s_with_queried_content",
    "scale_10k_fast_orient_preserves_db_wal_shm",
    "scale_10k_fast_orient_sampled_p99_under_1s",
    "scale_10k_fast_orient_under_1s_with_content",
    "scale_10k_index_ready_for_fast_orient",
    "scale_10k_seeded",
];

fn required_resume_e2e_labels_for_scope(scope: &str) -> Result<BTreeSet<String>, String> {
    let slices: &[&[&str]] = match scope {
        "all" => &[
            REQUIRED_FUNCTIONAL_RESUME_E2E_LABELS,
            REQUIRED_SCALE_RESUME_E2E_LABELS,
        ],
        "functional" => &[REQUIRED_FUNCTIONAL_RESUME_E2E_LABELS],
        "scale" => &[REQUIRED_SCALE_RESUME_E2E_LABELS],
        other => {
            return Err(format!(
                "unsupported EE_RESUME_E2E_SCOPE={other:?}; expected all, functional, or scale"
            ));
        }
    };
    Ok(slices
        .iter()
        .flat_map(|slice| slice.iter().copied())
        .map(str::to_owned)
        .collect())
}

fn ingest_resume_e2e_stdout_line(
    line: &str,
    passed_labels: &mut BTreeSet<String>,
    failed_events: &mut Vec<Value>,
) {
    if !line.trim_start().starts_with('{') {
        return;
    }
    let event = match serde_json::from_str::<Value>(line) {
        Ok(event) => event,
        Err(error) => {
            failed_events.push(serde_json::json!({
                "malformedEventLine": line,
                "error": error.to_string(),
            }));
            return;
        }
    };
    if event.get("schema").and_then(Value::as_str) != Some("ee.test_event.v1")
        || event.get("kind").and_then(Value::as_str) != Some("assert_result")
    {
        failed_events.push(event);
        return;
    }

    let label = event.pointer("/fields/label").and_then(Value::as_str);
    let status = event.pointer("/fields/status").and_then(Value::as_str);
    if let (Some(label), Some("pass")) = (label, status) {
        passed_labels.insert(label.to_owned());
    } else {
        failed_events.push(event);
    }
}

fn run_resume_e2e_script_bridge(scope: &str) -> TestResult {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let required_labels = required_resume_e2e_labels_for_scope(scope)?;
    ensure(
        !cfg!(debug_assertions),
        "the real-binary resume/orient bridge must run under cargo test --release so CARGO_BIN_EXE_ee is the public release binary",
    )?;
    let temp = resume_test_tempdir("ee-resume-e2e-bridge.")?;
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/e2e_resume.sh");
    let ee_bin = crate::common_spawn::ee_binary();
    ensure(
        Path::new(ee_bin)
            .components()
            .any(|component| component.as_os_str() == std::ffi::OsStr::new("release")),
        format!(
            "the real-binary resume/orient bridge requires the public release binary, got {ee_bin}"
        ),
    )?;
    let mut child = Command::new(&script)
        .env("HOME", temp.path().join("home"))
        .env("XDG_DATA_HOME", temp.path().join("xdg-data"))
        .env("XDG_CONFIG_HOME", temp.path().join("xdg-config"))
        .env("EE_EMBED_DOWNLOAD", "off")
        .env("EE_BIN", ee_bin)
        .env("EE_BINARY", ee_bin)
        .env("EE_E2E_TMPDIR", temp.path())
        .env("EE_RESUME_E2E_SCOPE", scope)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("launch {}: {error}", script.display()))?;

    let child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| "capture e2e_resume.sh stdout pipe".to_owned())?;
    let child_stderr = child
        .stderr
        .take()
        .ok_or_else(|| "capture e2e_resume.sh stderr pipe".to_owned())?;

    let scope_tag = scope.to_owned();
    let stdout_handle = std::thread::spawn(move || {
        let mut lines = Vec::new();
        let mut passed_labels = BTreeSet::new();
        let mut failed_events = Vec::new();
        for line in BufReader::new(child_stdout).lines() {
            let line = line.unwrap_or_else(|error| format!("<stdout read error: {error}>"));
            eprintln!("[e2e_resume:{scope_tag}:stdout] {line}");
            ingest_resume_e2e_stdout_line(&line, &mut passed_labels, &mut failed_events);
            lines.push(line);
        }
        (lines.join("\n"), passed_labels, failed_events)
    });

    let stderr_scope_tag = scope.to_owned();
    let stderr_handle = std::thread::spawn(move || {
        let mut lines = Vec::new();
        for line in BufReader::new(child_stderr).lines() {
            let line = line.unwrap_or_else(|error| format!("<stderr read error: {error}>"));
            eprintln!("[e2e_resume:{stderr_scope_tag}:stderr] {line}");
            lines.push(line);
        }
        lines.join("\n")
    });

    let status = child
        .wait()
        .map_err(|error| format!("wait for {}: {error}", script.display()))?;
    let (stdout, passed_labels, failed_events) = stdout_handle
        .join()
        .map_err(|_| "join e2e_resume stdout reader thread".to_owned())?;
    let stderr = stderr_handle
        .join()
        .map_err(|_| "join e2e_resume stderr reader thread".to_owned())?;
    let transcript = format!("stdout:\n{stdout}\nstderr:\n{stderr}");

    ensure(
        failed_events.is_empty(),
        format!(
            "resume E2E ({scope}) emitted non-pass or malformed assert_result events: {failed_events:?}; observed_pass={passed_labels:?}\n{transcript}"
        ),
    )?;
    ensure(
        status.success(),
        format!(
            "resume E2E ({scope}) exited with {:?}; expected exit 0; observed_pass={passed_labels:?}\n{transcript}",
            status.code()
        ),
    )?;

    let roots = std::fs::read_dir(temp.path())
        .map_err(|error| format!("read E2E temp root {}: {error}", temp.path().display()))?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_dir())
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("ee-resume-e2e."))
        })
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    ensure(
        roots.len() == 1,
        format!("expected one resume E2E artifact root, found {roots:?}"),
    )?;
    if scope != "scale" {
        let schema = load_schema()?;
        for artifact in ["resume-report.json", "resume-cold-report.json"] {
            let emitted_path = roots[0].join("logs").join(artifact);
            let emitted: Value = serde_json::from_slice(
                &std::fs::read(&emitted_path)
                    .map_err(|error| format!("read {}: {error}", emitted_path.display()))?,
            )
            .map_err(|error| format!("parse {}: {error}", emitted_path.display()))?;
            let emitted_report = emitted.pointer("/data/report").ok_or_else(|| {
                format!("{} has no data.report: {emitted}", emitted_path.display())
            })?;
            validate_json_schema_instance(emitted_report, &schema).map_err(|error| {
                format!(
                    "real compiled-binary resume report from {artifact} failed ee.resume.v1 validation: {error}; report={emitted_report}"
                )
            })?;
        }
    }

    let missing: Vec<_> = required_labels
        .difference(&passed_labels)
        .cloned()
        .collect();
    ensure(
        missing.is_empty(),
        format!(
            "resume E2E ({scope}) omitted required passing assert_result labels: {missing:?}; observed={passed_labels:?}\n{transcript}"
        ),
    )
}

#[test]
#[ignore = "full real-binary resume/orient acceptance; run with cargo test --release as a focused pinned RCH proof"]
fn resume_e2e_script_real_binary_acceptance_bridge() -> TestResult {
    let scope = std::env::var("EE_RESUME_E2E_SCOPE").unwrap_or_else(|_| "all".to_owned());
    run_resume_e2e_script_bridge(&scope)
}

#[test]
#[ignore = "focused 10k fast-orient <1s scale guard (bd-f4pmz); run with cargo test --release --test contracts resume_schema::resume_e2e_script_scale_10k_fast_orient_guard_bridge -- --ignored --nocapture"]
fn resume_e2e_script_scale_10k_fast_orient_guard_bridge() -> TestResult {
    run_resume_e2e_script_bridge("scale")
}

#[test]
fn resume_e2e_bridge_event_stream_parser_tracks_progress_for_all_and_scale_scopes() -> TestResult {
    let scale_labels = required_resume_e2e_labels_for_scope("scale")?;
    let functional_labels = required_resume_e2e_labels_for_scope("functional")?;
    let all_labels = required_resume_e2e_labels_for_scope("all")?;

    ensure(
        scale_labels.contains("scale_10k_fast_orient_under_1s_with_content")
            && scale_labels.contains("scale_10k_fast_orient_sampled_p99_under_1s")
            && !scale_labels.contains("init_ok"),
        "scale scope must require the 10k fast-orient guards without functional setup labels",
    )?;
    ensure(
        functional_labels.contains("init_ok")
            && !functional_labels.contains("scale_10k_fast_orient_under_1s_with_content"),
        "functional scope must require functional resume labels without 10k scale labels",
    )?;
    ensure(
        all_labels.len() == scale_labels.len() + functional_labels.len(),
        "all scope must be the disjoint union of functional and scale label sets",
    )?;
    ensure(
        required_resume_e2e_labels_for_scope("invalid-scope").is_err(),
        "unknown EE_RESUME_E2E_SCOPE must fail closed",
    )?;

    let mut passed = BTreeSet::new();
    let mut failed = Vec::new();
    for label in &scale_labels {
        let line = serde_json::json!({
            "schema": "ee.test_event.v1",
            "kind": "assert_result",
            "fields": {
                "label": label,
                "status": "pass"
            }
        })
        .to_string();
        ingest_resume_e2e_stdout_line(&line, &mut passed, &mut failed);
    }
    ingest_resume_e2e_stdout_line(
        "resume e2e (scale): 40 steps, 0 failures",
        &mut passed,
        &mut failed,
    );
    ensure(
        failed.is_empty() && passed == scale_labels,
        "complete scale stream must satisfy all required scale labels with zero failed events",
    )?;

    let mut partial_passed = BTreeSet::new();
    let mut partial_failed = Vec::new();
    ingest_resume_e2e_stdout_line(
        &serde_json::json!({
            "schema": "ee.test_event.v1",
            "kind": "assert_result",
            "fields": {
                "label": "scale_10k_seeded",
                "status": "pass"
            }
        })
        .to_string(),
        &mut partial_passed,
        &mut partial_failed,
    );
    let missing_on_overrun: Vec<_> = scale_labels.difference(&partial_passed).cloned().collect();
    ensure(
        partial_passed.contains("scale_10k_seeded")
            && missing_on_overrun
                .contains(&"scale_10k_fast_orient_sampled_p99_under_1s".to_owned()),
        "an overrun or truncated stream must retain already-completed labels while naming the unreached guard",
    )
}

#[test]
#[ignore = "10k real-store acceptance scale; run as a focused pinned RCH proof"]
fn resume_real_binary_completes_under_two_seconds_on_10k_store() -> TestResult {
    const CORPUS_SIZE: usize = 10_000;

    let temp = resume_test_tempdir("ee-resume-10k.")?;
    let workspace = temp.path().join("workspace");
    let store_dir = workspace.join(".ee");
    std::fs::create_dir_all(&store_dir)
        .map_err(|error| format!("create {}: {error}", store_dir.display()))?;
    let canonical_workspace = workspace
        .canonicalize()
        .map_err(|error| format!("canonicalize {}: {error}", workspace.display()))?;
    let workspace_id = stable_workspace_id(&canonical_workspace);
    let database = store_dir.join("ee.db");
    let connection = DbConnection::open_file(&database)
        .map_err(|error| format!("open {}: {error}", database.display()))?;
    connection
        .migrate()
        .map_err(|error| format!("migrate 10k acceptance store: {error}"))?;
    connection
        .insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: canonical_workspace.display().to_string(),
                name: Some("resume-10k-acceptance".to_owned()),
            },
        )
        .map_err(|error| format!("insert acceptance workspace: {error}"))?;

    // ONE transaction for the whole corpus, not 10_000. This is fixture SETUP
    // for an assertion about `resume` taking under two seconds, and it was
    // costing far more than the thing it measures: a dispatch on 2026-10-17
    // (bd-fyack) spent >72 minutes in the test phase and was killed at a 4-hour
    // cap before printing a result, having already spent 168 minutes compiling.
    // 10_000 separate `insert_memory` calls are 10_000 separate write
    // transactions, so the cost was WAL fsyncs on a shared worker volume.
    //
    // Safe to wrap: `insert_memory` delegates to `insert_memory_inner`, which
    // opens no transaction of its own, so nothing nests here. The store still
    // ends with CORPUS_SIZE rows and `resume` is still measured against it --
    // no assertion is weakened and no threshold moves.
    //
    // The error mapping moves OUT of the loop because `?` now propagates the
    // db error type; one commit also means a failure leaves no partial corpus
    // rather than rows 0..N, which nothing below depends on (the fingerprints
    // are taken after `drop(connection)`).
    let seeded: ee::db::Result<()> = connection.with_transaction(|| {
        for index in 0..CORPUS_SIZE {
            let input = CreateMemoryInput {
                workspace_id: workspace_id.clone(),
                level: "episodic".to_owned(),
                kind: "note".to_owned(),
                content: format!("Resume 10k acceptance memory {index:05}"),
                workflow_id: None,
                confidence: 0.8,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: Some("test://resume/10k-acceptance".to_owned()),
                trust_class: "agent_assertion".to_owned(),
                trust_subclass: None,
                tags: vec!["session-resume-10k".to_owned()],
                valid_from: None,
                valid_to: None,
            };
            connection.insert_memory(&format!("mem_{index:026}"), &input)?;
        }
        Ok(())
    });
    seeded.map_err(|error| format!("seed 10k acceptance corpus: {error}"))?;
    drop(connection);

    let fingerprint = |label: &str| -> Result<Vec<(String, String)>, String> {
        [
            ("db", database.clone()),
            ("wal", PathBuf::from(format!("{}-wal", database.display()))),
            ("shm", PathBuf::from(format!("{}-shm", database.display()))),
        ]
        .into_iter()
        .map(|(suffix, path)| {
            let digest = if path.exists() {
                let bytes = std::fs::read(&path)
                    .map_err(|error| format!("read {label} {}: {error}", path.display()))?;
                format!("blake3:{}", blake3::hash(&bytes).to_hex())
            } else {
                "missing".to_owned()
            };
            Ok((suffix.to_owned(), digest))
        })
        .collect()
    };
    let baseline = fingerprint("baseline")?;

    let json_started = Instant::now();
    let json_workspace = canonical_workspace
        .to_str()
        .ok_or("acceptance workspace path is not UTF-8")?;
    let json_database = database
        .to_str()
        .ok_or("acceptance database path is not UTF-8")?;
    let json_output = crate::common_spawn::serialized_real_ee_with(|command| {
        command.args([
            "resume",
            "--workspace",
            json_workspace,
            "--database",
            json_database,
            "--sessions",
            "3",
            "--json",
        ]);
    })
    .map_err(|error| format!("launch real JSON ee resume: {error}"))?;
    let json_elapsed = json_started.elapsed();

    ensure(
        json_output.status.success(),
        format!(
            "real JSON ee resume failed with {:?}: {}",
            json_output.status.code(),
            String::from_utf8_lossy(&json_output.stderr)
        ),
    )?;
    let response: Value = serde_json::from_slice(&json_output.stdout)
        .map_err(|error| format!("parse real ee resume response: {error}"))?;
    ensure(
        response.pointer("/schema").and_then(Value::as_str) == Some("ee.response.v2")
            && response.pointer("/success").and_then(Value::as_bool) == Some(true)
            && response
                .pointer("/data/report/schema")
                .and_then(Value::as_str)
                == Some(SCHEMA_ID)
            && response
                .pointer("/data/report/episodicTotal")
                .and_then(Value::as_u64)
                == Some(CORPUS_SIZE as u64)
            && response
                .pointer("/data/report/sessions/0/memberCount")
                .and_then(Value::as_u64)
                == Some(CORPUS_SIZE as u64)
            && response
                .pointer("/data/report/sessions/0/items")
                .and_then(Value::as_array)
                .is_some_and(|items| items.len() == 20),
        format!("real ee resume response contract drifted: {response}"),
    )?;
    ensure(
        json_elapsed < Duration::from_secs(2),
        format!(
            "JSON ee resume took {:.3}s on a {CORPUS_SIZE}-document real store; acceptance requires <2s",
            json_elapsed.as_secs_f64()
        ),
    )?;
    ensure(
        fingerprint("after JSON resume")? == baseline,
        "JSON ee resume changed ee.db, ee.db-wal, or ee.db-shm",
    )?;

    let human_started = Instant::now();
    let human_workspace = canonical_workspace
        .to_str()
        .ok_or("acceptance workspace path is not UTF-8")?;
    let human_database = database
        .to_str()
        .ok_or("acceptance database path is not UTF-8")?;
    let human_output = crate::common_spawn::serialized_real_ee_with(|command| {
        command.args([
            "resume",
            "--workspace",
            human_workspace,
            "--database",
            human_database,
            "--sessions",
            "3",
        ]);
    })
    .map_err(|error| format!("launch real human ee resume: {error}"))?;
    let human_elapsed = human_started.elapsed();
    ensure(
        human_output.status.success(),
        format!(
            "real human ee resume failed with {:?}: {}",
            human_output.status.code(),
            String::from_utf8_lossy(&human_output.stderr)
        ),
    )?;
    let human = String::from_utf8(human_output.stdout)
        .map_err(|error| format!("human ee resume output was not UTF-8: {error}"))?;
    ensure(
        human.contains("resume: 10000 episodic memories, 1 sessions shown")
            && human.contains("[session-resume-10k] 10000 memories")
            && human
                .lines()
                .filter(|line| line.starts_with("  - mem_"))
                .count()
                == 20,
        format!("human ee resume response contract drifted: {human}"),
    )?;
    ensure(
        human_elapsed < Duration::from_secs(2),
        format!(
            "human ee resume took {:.3}s on a {CORPUS_SIZE}-document real store; acceptance requires <2s",
            human_elapsed.as_secs_f64()
        ),
    )?;
    ensure(
        fingerprint("after human resume")? == baseline,
        "human ee resume changed ee.db, ee.db-wal, or ee.db-shm",
    )
}
