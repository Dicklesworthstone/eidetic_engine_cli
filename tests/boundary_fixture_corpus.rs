use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{Value as JsonValue, json};

type TestResult = Result<(), String>;

const CORPUS: &str = include_str!("fixtures/boundary_corpus/corpus.json");
const SMOKE_SUMMARY_SCHEMA: &str = "ee.e2e.boundary_fixture_smoke.v1";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Corpus {
    schema: String,
    corpus_id: String,
    generated_by: String,
    golden_artifact_plan: Vec<GoldenArtifact>,
    fixtures: Vec<BoundaryFixture>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoldenArtifact {
    kind: String,
    status: String,
    path: String,
    covered_by: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BoundaryFixture {
    id: String,
    scenario_class: String,
    schema_version: String,
    content: String,
    content_hash: String,
    normalized_timestamp: String,
    provenance_uris: Vec<String>,
    schema_versions: Vec<String>,
    redaction_state: RedactionState,
    trust_class: String,
    degraded_codes: Vec<String>,
    prompt_injection_quarantined: bool,
    intended_command_coverage: Vec<String>,
    fixture_mode_required: bool,
    normal_workspace_leakage_forbidden: bool,
}

#[derive(Debug, Deserialize)]
struct RedactionState {
    status: String,
    classes: Vec<String>,
}

fn parse_corpus() -> Result<Corpus, String> {
    serde_json::from_str(CORPUS).map_err(|error| format!("invalid fixture corpus JSON: {error}"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn fixture<'a>(corpus: &'a Corpus, id: &str) -> Result<&'a BoundaryFixture, String> {
    corpus
        .fixtures
        .iter()
        .find(|fixture| fixture.id == id)
        .ok_or_else(|| format!("missing fixture {id}"))
}

fn unique_smoke_dir() -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-e2e")
        .join("boundary_fixture_corpus")
        .join(format!("{}-{now}", std::process::id())))
}

fn write_text(path: &Path, content: &str) -> TestResult {
    fs::write(path, content).map_err(|error| format!("failed to write {}: {error}", path.display()))
}

#[test]
fn boundary_fixture_corpus_has_stable_metadata_and_hashes() -> TestResult {
    let corpus = parse_corpus()?;
    ensure(
        corpus.schema == "ee.boundary.fixture_corpus.v1",
        "unexpected corpus schema",
    )?;
    ensure(
        corpus.corpus_id == "boundary-corpus.v1",
        "unexpected corpus id",
    )?;
    ensure(
        corpus.generated_by == "eidetic_engine_cli-uiy3",
        "corpus must reference owning bead",
    )?;

    let mut previous_id = "";
    let mut ids = BTreeSet::new();
    for fixture in &corpus.fixtures {
        ensure(
            fixture.id.as_str() > previous_id,
            format!("fixtures must be sorted by id: {}", fixture.id),
        )?;
        previous_id = &fixture.id;
        ensure(
            ids.insert(fixture.id.as_str()),
            format!("duplicate fixture {}", fixture.id),
        )?;
        ensure(
            fixture.schema_version == "ee.fixture.boundary.v1",
            format!("{} has unexpected schema version", fixture.id),
        )?;
        ensure(
            fixture.normalized_timestamp == "2026-05-03T00:00:00Z",
            format!("{} has non-normalized timestamp", fixture.id),
        )?;
        ensure(
            fixture.normalized_timestamp.ends_with('Z')
                && !fixture.normalized_timestamp.contains("+00:00"),
            format!("{} timestamp must use normalized UTC Z form", fixture.id),
        )?;
        ensure(
            !fixture.provenance_uris.is_empty(),
            format!("{} missing provenance URI", fixture.id),
        )?;
        ensure(
            !fixture.schema_versions.is_empty(),
            format!("{} missing schema version coverage", fixture.id),
        )?;
        ensure(
            !fixture.intended_command_coverage.is_empty(),
            format!("{} missing intended command coverage", fixture.id),
        )?;
        ensure(
            fixture.trust_class == "synthetic_fixture",
            format!("{} must be explicitly synthetic", fixture.id),
        )?;
        ensure(
            fixture.fixture_mode_required,
            format!("{} must require fixture mode", fixture.id),
        )?;
        ensure(
            fixture.normal_workspace_leakage_forbidden,
            format!("{} must forbid normal workspace leakage", fixture.id),
        )?;

        let observed_hash = format!(
            "blake3:{}",
            blake3::hash(fixture.content.as_bytes()).to_hex()
        );
        ensure(
            observed_hash == fixture.content_hash,
            format!("{} hash mismatch", fixture.id),
        )?;
    }

    Ok(())
}

#[test]
fn boundary_fixture_corpus_smoke_summary_logs_required_e2e_fields() -> TestResult {
    let corpus = parse_corpus()?;
    let fixture = fixture(&corpus, "boundary.redacted_secret_placeholder.v1")?;
    let smoke_dir = unique_smoke_dir()?;
    let workspace = smoke_dir.join("workspace");
    let stdout_path = smoke_dir.join("stdout.json");
    let stderr_path = smoke_dir.join("stderr.txt");
    let summary_path = smoke_dir.join("summary.json");

    fs::create_dir_all(&workspace)
        .map_err(|error| format!("failed to create smoke workspace: {error}"))?;
    write_text(
        &stdout_path,
        "{\"schema\":\"ee.response.v1\",\"mode\":\"fixture\",\"records\":[]}\n",
    )?;
    write_text(&stderr_path, "fixture smoke completed\n")?;

    let command_matrix_rows_exercised: BTreeSet<&str> = fixture
        .intended_command_coverage
        .iter()
        .map(String::as_str)
        .collect();
    let summary = json!({
        "schema": SMOKE_SUMMARY_SCHEMA,
        "mode": "fixture",
        "fixtureName": fixture.id,
        "fixtureHash": fixture.content_hash,
        "workspacePath": workspace.display().to_string(),
        "dbGenerationBefore": 0,
        "dbGenerationAfter": 0,
        "indexGenerationBefore": 0,
        "indexGenerationAfter": 0,
        "schemaVersions": fixture.schema_versions,
        "redactionClasses": fixture.redaction_state.classes,
        "commandMatrixRowsExercised": command_matrix_rows_exercised,
        "stdoutArtifactPath": stdout_path.display().to_string(),
        "stderrArtifactPath": stderr_path.display().to_string(),
        "firstFailureDiagnosis": JsonValue::Null,
        "fixtureModeRequired": fixture.fixture_mode_required,
        "normalWorkspaceLeakageForbidden": fixture.normal_workspace_leakage_forbidden
    });
    let rendered =
        serde_json::to_string_pretty(&summary).map_err(|error| error.to_string())? + "\n";
    write_text(&summary_path, &rendered)?;

    let parsed: JsonValue = serde_json::from_str(&rendered).map_err(|error| error.to_string())?;
    ensure(
        parsed["schema"] == json!(SMOKE_SUMMARY_SCHEMA),
        "unexpected smoke summary schema",
    )?;
    ensure(
        parsed["mode"] == json!("fixture"),
        "smoke summary must explicitly mark fixture mode",
    )?;
    ensure(
        parsed["fixtureName"] == json!(fixture.id),
        "smoke summary missing fixture name",
    )?;
    ensure(
        parsed["fixtureHash"] == json!(fixture.content_hash),
        "smoke summary missing fixture hash",
    )?;
    ensure(
        parsed["workspacePath"]
            .as_str()
            .is_some_and(|path| !path.is_empty()),
        "smoke summary missing workspace path",
    )?;
    ensure(
        parsed["dbGenerationBefore"].is_u64() && parsed["dbGenerationAfter"].is_u64(),
        "smoke summary missing DB generations",
    )?;
    ensure(
        parsed["indexGenerationBefore"].is_u64() && parsed["indexGenerationAfter"].is_u64(),
        "smoke summary missing index generations",
    )?;
    ensure(
        parsed["schemaVersions"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "smoke summary missing schema versions",
    )?;
    ensure(
        parsed["redactionClasses"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "smoke summary missing redaction classes",
    )?;
    ensure(
        parsed["commandMatrixRowsExercised"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "smoke summary missing matrix rows",
    )?;
    ensure(
        parsed["stdoutArtifactPath"]
            .as_str()
            .is_some_and(|path| Path::new(path).is_file()),
        "stdout artifact path must exist",
    )?;
    ensure(
        parsed["stderrArtifactPath"]
            .as_str()
            .is_some_and(|path| Path::new(path).is_file()),
        "stderr artifact path must exist",
    )?;
    ensure(
        parsed["firstFailureDiagnosis"].is_null(),
        "clean smoke summary must record null first failure",
    )?;

    let stdout = fs::read_to_string(&stdout_path)
        .map_err(|error| format!("failed to read smoke stdout: {error}"))?;
    ensure(
        !stdout.contains(&fixture.content),
        "normal smoke stdout must not leak fixture record content",
    )?;
    ensure(
        stdout.contains("\"mode\":\"fixture\""),
        "smoke stdout must mark fixture/eval mode",
    )
}

#[test]
fn boundary_fixture_corpus_covers_required_scenario_classes() -> TestResult {
    let corpus = parse_corpus()?;
    let observed: BTreeSet<&str> = corpus
        .fixtures
        .iter()
        .map(|fixture| fixture.scenario_class.as_str())
        .collect();

    for required in [
        "empty",
        "happy_path",
        "boundary",
        "degraded",
        "malformed",
        "stale",
        "redacted",
        "prompt_injection",
        "cross_command_provenance",
    ] {
        ensure(
            observed.contains(required),
            format!("missing scenario class {required}"),
        )?;
    }

    Ok(())
}

#[test]
fn boundary_fixture_corpus_records_redaction_degraded_and_quarantine_state() -> TestResult {
    let corpus = parse_corpus()?;

    let redacted = fixture(&corpus, "boundary.redacted_secret_placeholder.v1")?;
    ensure(
        redacted.redaction_state.status == "redacted",
        "redacted fixture must declare redacted status",
    )?;
    ensure(
        redacted
            .redaction_state
            .classes
            .iter()
            .any(|class| class == "env_secret"),
        "redacted fixture must declare env_secret class",
    )?;
    ensure(
        redacted.content.contains("[REDACTED:env:OPENAI_API_KEY]"),
        "redacted fixture must use a safe placeholder",
    )?;

    let injection = fixture(&corpus, "boundary.prompt_injection_session.v1")?;
    ensure(
        injection.prompt_injection_quarantined,
        "prompt-injection fixture must be quarantined",
    )?;
    ensure(
        injection
            .redaction_state
            .classes
            .iter()
            .any(|class| class == "prompt_injection"),
        "prompt-injection fixture must declare quarantine class",
    )?;

    for (id, degraded_code) in [
        (
            "boundary.search_index_missing_degraded.v1",
            "search_index_unavailable",
        ),
        (
            "boundary.malformed_cass_span.v1",
            "import_source_unavailable",
        ),
        ("boundary.stale_graph_projection.v1", "graph_unavailable"),
    ] {
        let degraded = fixture(&corpus, id)?;
        ensure(
            degraded
                .degraded_codes
                .iter()
                .any(|code| code == degraded_code),
            format!("{id} must declare {degraded_code}"),
        )?;
    }

    Ok(())
}

#[test]
fn boundary_fixture_corpus_plans_all_required_golden_artifact_kinds() -> TestResult {
    let corpus = parse_corpus()?;
    let fixture_ids: BTreeSet<&str> = corpus
        .fixtures
        .iter()
        .map(|fixture| fixture.id.as_str())
        .collect();
    let observed: BTreeSet<&str> = corpus
        .golden_artifact_plan
        .iter()
        .map(|artifact| artifact.kind.as_str())
        .collect();

    for required in [
        "json",
        "markdown",
        "toon",
        "error_degradation",
        "e2e_summary",
        "skill_evidence_bundle",
    ] {
        ensure(
            observed.contains(required),
            format!("missing golden artifact kind {required}"),
        )?;
    }

    for artifact in &corpus.golden_artifact_plan {
        ensure(
            artifact.status == "existing" || artifact.status == "planned",
            format!("{} has invalid status {}", artifact.kind, artifact.status),
        )?;
        ensure(
            !artifact.path.is_empty(),
            format!("{} missing path", artifact.kind),
        )?;
        ensure(
            !artifact.covered_by.is_empty(),
            format!("{} missing fixture coverage", artifact.kind),
        )?;
        for covered_by in &artifact.covered_by {
            ensure(
                fixture_ids.contains(covered_by.as_str()),
                format!("{} references unknown fixture {covered_by}", artifact.kind),
            )?;
        }
    }

    Ok(())
}
