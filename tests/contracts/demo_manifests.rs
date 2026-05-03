//! Gate 14: demo manifest contracts.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

use ee::models::{
    ClaimId, DEMO_FILE_SCHEMA_V1, DemoArtifactOutput, DemoCommand, DemoEntry, DemoFile, DemoId,
    parse_demo_file_yaml, validate_demo_file,
};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("demo")
        .join(format!("{name}.json.golden"))
}

fn assert_golden(name: &str, actual: &str) -> TestResult {
    let path = golden_path(name);
    if env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&path, actual).map_err(|error| error.to_string())?;
        return Ok(());
    }

    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    ensure(actual == expected, format!("golden mismatch for {name}"))
}

fn release_context_demo_file() -> Result<DemoFile, String> {
    let claim_id =
        ClaimId::from_str("claim_00000000000000000000000001").map_err(|error| error.to_string())?;
    let demo_id =
        DemoId::from_str("demo_00000000000000000000000001").map_err(|error| error.to_string())?;
    let stdout_hash = "a".repeat(64);

    Ok(DemoFile::new().with_demo(
        DemoEntry::new(
            demo_id,
            "Release context demo",
            "Verifies release context output against the executable claim.",
        )
        .with_claim_id(claim_id)
        .with_tag("release")
        .with_tag("gate14")
        .with_command(
            DemoCommand::new("ee context \"prepare release\" --workspace . --json")
                .with_stdout_schema("ee.response.v1")
                .with_stdout_contains("\"command\":\"context\"")
                .with_artifact_output(
                    DemoArtifactOutput::new("stdout.json").with_blake3_hash(stdout_hash),
                ),
        ),
    ))
}

#[test]
fn gate14_demo_manifest_links_each_demo_to_claim_id() -> TestResult {
    let file = release_context_demo_file()?;
    let errors = validate_demo_file(&file);

    ensure(
        errors.is_empty(),
        format!("demo should validate: {errors:?}"),
    )?;
    ensure(file.schema == DEMO_FILE_SCHEMA_V1, "schema is stable")?;
    let demo = file
        .demos
        .first()
        .ok_or_else(|| "one demo fixture is present".to_string())?;
    ensure(file.demo_count() == 1, "one demo fixture")?;
    ensure(demo.claim_id.is_some(), "demo links to a claim")
}

#[test]
fn gate14_demo_yaml_parses_to_typed_manifest() -> TestResult {
    let stdout_hash = "a".repeat(64);
    let yaml = format!(
        "\
schema: ee.demo_file.v1
version: 1
demos:
  - id: demo_00000000000000000000000001
    claim_id: claim_00000000000000000000000001
    title: Release context demo
    description: Verifies release context output against the executable claim.
    tags:
      - release
      - gate14
    commands:
      - command: 'ee context \"prepare release\" --workspace . --json'
        expected_stdout_schema: ee.response.v1
        expected_stdout_contains:
          - '\"command\":\"context\"'
        artifact_outputs:
          - path: stdout.json
            blake3_hash: {stdout_hash}
"
    );

    let file = parse_demo_file_yaml(&yaml).map_err(|error| error.to_string())?;
    let demo = file
        .demos
        .first()
        .ok_or_else(|| "parsed demo is present".to_string())?;
    ensure(file.schema == DEMO_FILE_SCHEMA_V1, "schema is stable")?;
    ensure(demo.claim_id.is_some(), "parsed demo links to a claim")?;
    ensure(demo.commands.len() == 1, "parsed demo has one command")?;
    let command = demo
        .commands
        .first()
        .ok_or_else(|| "parsed command is present".to_string())?;
    ensure(
        command.artifact_outputs.len() == 1,
        "parsed command has one artifact output",
    )
}

#[test]
fn gate14_demo_yaml_rejects_unsupported_manifest_version() -> TestResult {
    let yaml = "\
schema: ee.demo_file.v1
version: 2
demos:
  - id: demo_00000000000000000000000001
    title: Unsupported version
    commands:
      - command: \"ee status --json\"
";

    let error = parse_demo_file_yaml(yaml)
        .map(|_| ())
        .map_err(|error| error.to_string());
    ensure(
        error == Err("unsupported demo manifest version `2`; expected `1`".to_string()),
        "unsupported demo manifest versions must fail explicitly",
    )
}

#[test]
fn gate14_demo_yaml_rejects_nonportable_artifact_paths() -> TestResult {
    let yaml = r#"
schema: ee.demo_file.v1
version: 1
demos:
  - id: demo_00000000000000000000000001
    title: Nonportable artifact path
    commands:
      - command: "ee status --json"
        artifact_outputs:
          - path: '..\out.txt'
"#;

    let error = parse_demo_file_yaml(yaml)
        .map(|_| ())
        .map_err(|error| error.to_string());
    ensure(
        error.is_err_and(|message| {
            message.contains("demo.yaml validation failed")
                && message.contains("invalid_artifact_path")
                && message.contains(r"..\out.txt")
        }),
        "nonportable artifact paths must fail manifest parsing",
    )
}

#[test]
fn gate14_demo_yaml_requires_artifact_verification_predicate() -> TestResult {
    let yaml = "\
schema: ee.demo_file.v1
version: 1
demos:
  - id: demo_00000000000000000000000001
    title: Missing artifact predicate
    commands:
      - command: \"ee status --json\"
        artifact_outputs:
          - path: stdout.json
";

    let error = parse_demo_file_yaml(yaml)
        .map(|_| ())
        .map_err(|error| error.to_string());
    ensure(
        error.is_err_and(|message| {
            message.contains("demo.yaml validation failed")
                && message.contains("missing_artifact_verification")
        }),
        "artifact outputs must declare hash or size evidence",
    )
}

#[test]
fn gate14_release_context_demo_matches_golden() -> TestResult {
    let file = release_context_demo_file()?;
    let demo = file
        .demos
        .first()
        .ok_or_else(|| "release context demo is present".to_string())?;
    let command = demo
        .commands
        .first()
        .ok_or_else(|| "release context demo command is present".to_string())?;

    let json = serde_json::json!({
        "schema": file.schema,
        "version": file.version,
        "demos": [{
            "id": demo.id.to_string(),
            "claimId": demo.claim_id.as_ref().map(ToString::to_string),
            "title": &demo.title,
            "description": &demo.description,
            "commands": [{
                "command": &command.command,
                "timeoutMs": command.timeout_ms,
                "expectedExitCode": command.expected_exit_code,
                "expectedStdoutSchema": &command.expected_stdout_schema,
                "expectedStdoutContains": &command.expected_stdout_contains,
                "artifactOutputs": command.artifact_outputs.iter().map(|artifact| {
                    serde_json::json!({
                        "path": &artifact.path,
                        "blake3Hash": &artifact.blake3_hash,
                        "optional": artifact.optional,
                    })
                }).collect::<Vec<_>>(),
            }],
            "tags": &demo.tags,
            "ciEnabled": demo.ci_enabled,
        }]
    });

    let rendered = serde_json::to_string_pretty(&json).map_err(|error| error.to_string())? + "\n";
    assert_golden("release_context_demo", &rendered)
}
