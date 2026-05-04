//! EE-USR-002: pre-task briefing and context-pack acceptance scenario.

#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
#[cfg(unix)]
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use serde_json::{Value as JsonValue, json};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn parse_json_stdout(output: &Output, context: &str) -> Result<JsonValue, String> {
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("{context}: stdout must be valid JSON: {error}"))
}

fn assert_json_machine_stdout(output: &Output, context: &str) -> TestResult {
    ensure(
        output.stderr.is_empty(),
        format!(
            "{context}: stderr must be empty in JSON mode: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure(
        stdout.trim_start().starts_with('{'),
        format!("{context}: stdout must start with JSON object"),
    )
}

#[cfg(unix)]
fn write_json(path: &Path, value: &JsonValue) -> TestResult {
    let mut content = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    content.push('\n');
    fs::write(path, content).map_err(|error| error.to_string())
}

#[cfg(unix)]
fn unique_scenario_dir(scenario: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-e2e")
        .join(scenario)
        .join(format!("{}-{now}", std::process::id())))
}

#[cfg(unix)]
struct LoggedStep {
    output: Output,
    dossier_dir: PathBuf,
}

#[cfg(unix)]
fn run_logged_json_step(
    scenario_dir: &Path,
    step_slug: &str,
    workspace: &Path,
    args: &[&str],
    fixture_id: &str,
    expected_schema: &str,
    golden_path: Option<&str>,
) -> Result<LoggedStep, String> {
    let dossier_dir = scenario_dir.join(step_slug);
    fs::create_dir_all(&dossier_dir).map_err(|error| error.to_string())?;

    fs::write(
        dossier_dir.join("command.txt"),
        format!("ee {}\n", args.join(" ")),
    )
    .map_err(|error| error.to_string())?;
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    fs::write(dossier_dir.join("cwd.txt"), format!("{}\n", cwd.display()))
        .map_err(|error| error.to_string())?;
    fs::write(
        dossier_dir.join("workspace.txt"),
        format!("{}\n", workspace.display()),
    )
    .map_err(|error| error.to_string())?;
    write_json(
        &dossier_dir.join("env.sanitized.json"),
        &json!({
            "overrides": {},
            "sensitiveEnvOmitted": true,
            "toolchain": "cargo-test",
            "featureProfile": "default"
        }),
    )?;

    let started = Instant::now();
    let output = run_ee(args)?;
    let elapsed_ms = started.elapsed().as_millis();

    fs::write(
        dossier_dir.join("exit-code.txt"),
        format!("{}\n", output.status.code().unwrap_or(-1)),
    )
    .map_err(|error| error.to_string())?;
    fs::write(
        dossier_dir.join("elapsed-ms.txt"),
        format!("{elapsed_ms}\n"),
    )
    .map_err(|error| error.to_string())?;
    fs::write(dossier_dir.join("stdout"), &output.stdout).map_err(|error| error.to_string())?;
    fs::write(dossier_dir.join("stderr"), &output.stderr).map_err(|error| error.to_string())?;

    let stdout_json = serde_json::from_slice::<JsonValue>(&output.stdout).ok();
    let schema_status = stdout_json
        .as_ref()
        .and_then(|value| value.get("schema"))
        .and_then(JsonValue::as_str)
        .map_or("missing", |actual| {
            if actual == expected_schema {
                "matched"
            } else {
                "mismatched"
            }
        });

    write_json(
        &dossier_dir.join("stdout.schema.json"),
        &json!({
            "fixtureId": fixture_id,
            "schema": expected_schema,
            "parseStatus": if stdout_json.is_some() { "parsed" } else { "not_json" },
            "schemaStatus": schema_status,
            "stdoutPath": dossier_dir.join("stdout").display().to_string(),
            "stderrPath": dossier_dir.join("stderr").display().to_string(),
            "goldenPath": golden_path,
            "goldenStatus": if golden_path.is_some() { "covered_by_contract" } else { "not_applicable" }
        }),
    )?;

    let degraded_codes = stdout_json
        .as_ref()
        .and_then(|value| value.pointer("/data/degraded"))
        .and_then(JsonValue::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("code").and_then(JsonValue::as_str))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    write_json(
        &dossier_dir.join("degradation-report.json"),
        &json!({
            "status": if degraded_codes.is_empty() { "none" } else { "present" },
            "codes": degraded_codes,
            "repair": null
        }),
    )?;

    write_json(
        &dossier_dir.join("redaction-report.json"),
        &json!({
            "status": "checked",
            "redactedClasses": [],
            "secretPatternsObserved": String::from_utf8_lossy(&output.stdout).contains("sk-")
        }),
    )?;

    let first_failure = if output.status.success() {
        "No failure observed.\n".to_string()
    } else {
        format!(
            "Exit code: {:?}\n\nstderr excerpt:\n{}\n",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .take(8)
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    fs::write(dossier_dir.join("first-failure.md"), first_failure)
        .map_err(|error| error.to_string())?;

    Ok(LoggedStep {
        output,
        dossier_dir,
    })
}

#[cfg(unix)]
fn run_logged_markdown_step(
    scenario_dir: &Path,
    step_slug: &str,
    workspace: &Path,
    args: &[&str],
    fixture_id: &str,
    golden_path: Option<&str>,
) -> Result<LoggedStep, String> {
    let dossier_dir = scenario_dir.join(step_slug);
    fs::create_dir_all(&dossier_dir).map_err(|error| error.to_string())?;

    fs::write(
        dossier_dir.join("command.txt"),
        format!("ee {}\n", args.join(" ")),
    )
    .map_err(|error| error.to_string())?;
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    fs::write(dossier_dir.join("cwd.txt"), format!("{}\n", cwd.display()))
        .map_err(|error| error.to_string())?;
    fs::write(
        dossier_dir.join("workspace.txt"),
        format!("{}\n", workspace.display()),
    )
    .map_err(|error| error.to_string())?;
    write_json(
        &dossier_dir.join("env.sanitized.json"),
        &json!({
            "overrides": {},
            "sensitiveEnvOmitted": true,
            "toolchain": "cargo-test",
            "featureProfile": "default"
        }),
    )?;

    let started = Instant::now();
    let output = run_ee(args)?;
    let elapsed_ms = started.elapsed().as_millis();

    fs::write(
        dossier_dir.join("exit-code.txt"),
        format!("{}\n", output.status.code().unwrap_or(-1)),
    )
    .map_err(|error| error.to_string())?;
    fs::write(
        dossier_dir.join("elapsed-ms.txt"),
        format!("{elapsed_ms}\n"),
    )
    .map_err(|error| error.to_string())?;
    fs::write(dossier_dir.join("stdout"), &output.stdout).map_err(|error| error.to_string())?;
    fs::write(dossier_dir.join("stderr"), &output.stderr).map_err(|error| error.to_string())?;

    write_json(
        &dossier_dir.join("stdout.schema.json"),
        &json!({
            "fixtureId": fixture_id,
            "schema": null,
            "parseStatus": "not_json",
            "schemaStatus": "not_applicable",
            "stdoutPath": dossier_dir.join("stdout").display().to_string(),
            "stderrPath": dossier_dir.join("stderr").display().to_string(),
            "goldenPath": golden_path,
            "goldenStatus": if golden_path.is_some() { "covered_by_contract" } else { "not_applicable" }
        }),
    )?;

    write_json(
        &dossier_dir.join("degradation-report.json"),
        &json!({
            "status": "not_applicable",
            "codes": [],
            "repair": null
        }),
    )?;
    write_json(
        &dossier_dir.join("redaction-report.json"),
        &json!({
            "status": "checked",
            "redactedClasses": [],
            "secretPatternsObserved": String::from_utf8_lossy(&output.stdout).contains("sk-")
        }),
    )?;

    let first_failure = if output.status.success() {
        "No failure observed.\n".to_string()
    } else {
        format!(
            "Exit code: {:?}\n\nstderr excerpt:\n{}\n",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .take(8)
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    fs::write(dossier_dir.join("first-failure.md"), first_failure)
        .map_err(|error| error.to_string())?;

    Ok(LoggedStep {
        output,
        dossier_dir,
    })
}

#[cfg(unix)]
#[test]
fn pre_task_briefing_scenario_produces_actionable_context_with_logged_artifacts() -> TestResult {
    let scenario = "usr_pre_task_brief";
    let scenario_dir = unique_scenario_dir(scenario)?;
    let workspace = scenario_dir.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;

    let workspace_arg = workspace.to_string_lossy().into_owned();
    let mut command_dossiers = Vec::new();

    let init = run_logged_json_step(
        &scenario_dir,
        "01_init",
        &workspace,
        &["--workspace", workspace_arg.as_str(), "--json", "init"],
        "fx.fresh_workspace.v1",
        "ee.response.v1",
        None,
    )?;
    command_dossiers.push(init.dossier_dir.clone());
    ensure(init.output.status.success(), "init should succeed")?;
    assert_json_machine_stdout(&init.output, "init")?;

    let rule = run_logged_json_step(
        &scenario_dir,
        "02_remember_rule",
        &workspace,
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "remember",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--tags",
            "release,preflight",
            "--source",
            "file://tests/fixtures/eval/release_failure/source_memory.json#L1",
            "Before release, run cargo fmt --check, cargo clippy --all-targets -- -D warnings, and cargo test.",
        ],
        "fx.manual_memory.v1",
        "ee.response.v1",
        None,
    )?;
    command_dossiers.push(rule.dossier_dir.clone());
    ensure(rule.output.status.success(), "remember rule should succeed")?;
    assert_json_machine_stdout(&rule.output, "remember rule")?;
    let rule_json = parse_json_stdout(&rule.output, "remember rule")?;
    let rule_id = rule_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "rule memory_id missing".to_string())?
        .to_string();

    let failure = run_logged_json_step(
        &scenario_dir,
        "03_remember_failure",
        &workspace,
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "remember",
            "--level",
            "episodic",
            "--kind",
            "failure",
            "--tags",
            "release,clippy,workflow",
            "--source",
            "file://tests/fixtures/eval/release_failure/source_memory.json#L10",
            "Release failed after clippy found an unused import in CI because checks were skipped locally.",
        ],
        "fx.release_failure.v1",
        "ee.response.v1",
        None,
    )?;
    command_dossiers.push(failure.dossier_dir.clone());
    ensure(
        failure.output.status.success(),
        "remember failure should succeed",
    )?;
    assert_json_machine_stdout(&failure.output, "remember failure")?;
    let failure_json = parse_json_stdout(&failure.output, "remember failure")?;
    let failure_id = failure_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "failure memory_id missing".to_string())?
        .to_string();

    let rebuild = run_logged_json_step(
        &scenario_dir,
        "04_index_rebuild",
        &workspace,
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "index",
            "rebuild",
        ],
        "fx.manual_memory.v1",
        "ee.response.v1",
        None,
    )?;
    command_dossiers.push(rebuild.dossier_dir.clone());
    ensure(
        rebuild.output.status.success(),
        "index rebuild should succeed",
    )?;
    assert_json_machine_stdout(&rebuild.output, "index rebuild")?;

    let status = run_logged_json_step(
        &scenario_dir,
        "05_status",
        &workspace,
        &["--workspace", workspace_arg.as_str(), "--json", "status"],
        "fx.fresh_workspace.v1",
        "ee.response.v1",
        Some("tests/fixtures/golden/status/status_json.golden"),
    )?;
    command_dossiers.push(status.dossier_dir.clone());
    ensure(status.output.status.success(), "status should succeed")?;
    assert_json_machine_stdout(&status.output, "status")?;
    let status_json = parse_json_stdout(&status.output, "status")?;
    ensure(
        status_json["data"]["workspace"]["root"].is_string(),
        "status should include workspace root",
    )?;
    ensure(
        status_json["data"]["workspace"]["fingerprint"].is_string(),
        "status should include workspace fingerprint",
    )?;
    ensure(
        status_json["data"]["capabilities"].is_object(),
        "status should include capabilities object",
    )?;

    let health = run_logged_json_step(
        &scenario_dir,
        "06_health",
        &workspace,
        &["--json", "health"],
        "fx.fresh_workspace.v1",
        "ee.response.v1",
        Some("tests/fixtures/golden/agent/health_unavailable.json.golden"),
    )?;
    command_dossiers.push(health.dossier_dir.clone());
    ensure(
        health.output.status.success() || health.output.status.code() == Some(6),
        "health should exit 0 or degraded 6",
    )?;
    assert_json_machine_stdout(&health.output, "health")?;
    let health_json = parse_json_stdout(&health.output, "health")?;
    ensure(
        health_json["data"]["command"] == json!("health"),
        "health command field must be stable",
    )?;
    ensure(
        health_json["data"]["issues"].is_array(),
        "health issues array should exist",
    )?;

    let capabilities = run_logged_json_step(
        &scenario_dir,
        "07_capabilities",
        &workspace,
        &["--json", "capabilities"],
        "fx.fresh_workspace.v1",
        "ee.response.v1",
        Some("tests/fixtures/golden/capabilities/capabilities_json.golden"),
    )?;
    command_dossiers.push(capabilities.dossier_dir.clone());
    ensure(
        capabilities.output.status.success(),
        "capabilities should succeed",
    )?;
    assert_json_machine_stdout(&capabilities.output, "capabilities")?;
    let capabilities_json = parse_json_stdout(&capabilities.output, "capabilities")?;
    let subsystem_names = capabilities_json["data"]["subsystems"]
        .as_array()
        .ok_or_else(|| "capabilities subsystems missing".to_string())?
        .iter()
        .filter_map(|subsystem| subsystem["name"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    ensure(
        subsystem_names.contains(&"runtime".to_string())
            && subsystem_names.contains(&"storage".to_string())
            && subsystem_names.contains(&"search".to_string()),
        "capabilities must report runtime/storage/search subsystems",
    )?;

    let search = run_logged_json_step(
        &scenario_dir,
        "08_search",
        &workspace,
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "search",
            "release clippy",
        ],
        "fx.release_failure.v1",
        "ee.response.v1",
        Some("tests/fixtures/golden/agent/search_unavailable.json.golden"),
    )?;
    command_dossiers.push(search.dossier_dir.clone());
    ensure(search.output.status.success(), "search should succeed")?;
    assert_json_machine_stdout(&search.output, "search")?;
    let search_json = parse_json_stdout(&search.output, "search")?;
    let search_ids = search_json["data"]["results"]
        .as_array()
        .ok_or_else(|| "search results missing".to_string())?
        .iter()
        .filter_map(|result| result["docId"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    ensure(
        search_ids.contains(&rule_id) && search_ids.contains(&failure_id),
        "search must include both rule and failure evidence ids",
    )?;

    let context_markdown = run_logged_markdown_step(
        &scenario_dir,
        "09_context_markdown",
        &workspace,
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--format",
            "markdown",
            "context",
            "prepare release",
            "--max-tokens",
            "4000",
        ],
        "fx.release_failure.v1",
        Some("tests/fixtures/golden/agent/context_pack.md.golden"),
    )?;
    command_dossiers.push(context_markdown.dossier_dir.clone());
    ensure(
        context_markdown.output.status.success(),
        "context markdown should succeed",
    )?;
    ensure(
        context_markdown.output.stderr.is_empty(),
        "context markdown should not emit stderr",
    )?;
    let markdown_text = String::from_utf8_lossy(&context_markdown.output.stdout);
    ensure(
        markdown_text.contains("provenance")
            || markdown_text.contains("Provenance")
            || markdown_text.contains("Selected Memory"),
        "context markdown should include provenance-oriented guidance",
    )?;

    let context_json_first = run_logged_json_step(
        &scenario_dir,
        "10_context_json_first",
        &workspace,
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "context",
            "prepare release",
            "--max-tokens",
            "4000",
        ],
        "fx.release_failure.v1",
        "ee.response.v1",
        Some("tests/fixtures/golden/agent/context_pack.json.golden"),
    )?;
    command_dossiers.push(context_json_first.dossier_dir.clone());
    ensure(
        context_json_first.output.status.success(),
        "context json (first) should succeed",
    )?;
    assert_json_machine_stdout(&context_json_first.output, "context json first")?;
    let context_json_first_value =
        parse_json_stdout(&context_json_first.output, "context json first")?;

    let context_json_second = run_logged_json_step(
        &scenario_dir,
        "11_context_json_second",
        &workspace,
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "context",
            "prepare release",
            "--max-tokens",
            "4000",
        ],
        "fx.release_failure.v1",
        "ee.response.v1",
        Some("tests/fixtures/golden/agent/context_pack.json.golden"),
    )?;
    command_dossiers.push(context_json_second.dossier_dir.clone());
    ensure(
        context_json_second.output.status.success(),
        "context json (second) should succeed",
    )?;
    assert_json_machine_stdout(&context_json_second.output, "context json second")?;
    let context_json_second_value =
        parse_json_stdout(&context_json_second.output, "context json second")?;

    let first_item_ids = context_json_first_value["data"]["pack"]["items"]
        .as_array()
        .ok_or_else(|| "context first items missing".to_string())?
        .iter()
        .filter_map(|item| item["memoryId"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let second_item_ids = context_json_second_value["data"]["pack"]["items"]
        .as_array()
        .ok_or_else(|| "context second items missing".to_string())?
        .iter()
        .filter_map(|item| item["memoryId"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    ensure(
        !first_item_ids.is_empty(),
        "context pack should include at least one item",
    )?;
    ensure(
        first_item_ids == second_item_ids,
        format!(
            "context item ordering must be deterministic; first={first_item_ids:?} second={second_item_ids:?}"
        ),
    )?;

    ensure(
        context_json_first_value["data"]["pack"]["items"]
            .as_array()
            .is_some_and(|items| {
                items
                    .iter()
                    .all(|item| item["provenance"].as_array().is_some_and(|p| !p.is_empty()))
            }),
        "context items must carry provenance evidence",
    )?;

    let preflight = run_logged_json_step(
        &scenario_dir,
        "12_preflight_run",
        &workspace,
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "preflight",
            "run",
            "deploy production database migration",
        ],
        "fx.release_failure.v1",
        "ee.response.v1",
        None,
    )?;
    command_dossiers.push(preflight.dossier_dir.clone());
    ensure(
        !preflight.output.status.success(),
        "preflight run should degrade until persisted evidence matching is available",
    )?;
    assert_json_machine_stdout(&preflight.output, "preflight run")?;
    let preflight_json = parse_json_stdout(&preflight.output, "preflight run")?;
    ensure(
        preflight_json["schema"] == json!("ee.response.v1")
            && preflight_json["data"]["code"] == json!("preflight_evidence_unavailable"),
        "preflight run should report unavailable evidence matching",
    )?;
    ensure(
        preflight_json["data"]["evidenceIds"] == json!([])
            && preflight_json["data"]["followUpBead"] == json!("eidetic_engine_cli-bijm"),
        "preflight run should not claim risk evidence",
    )?;

    let metamorphic_fixture = include_str!("fixtures/eval/metamorphic_evaluation/scenario.json");
    ensure(
        metamorphic_fixture.contains("\"semantic_disabled\""),
        "metamorphic fixture must preserve semantic_disabled degraded branch",
    )?;

    let degradation_matrix = include_str!("fixtures/golden/degradation/matrix.json.golden");
    ensure(
        degradation_matrix.contains("\"id\": \"D300\"")
            && degradation_matrix.contains("Graph snapshot is stale"),
        "degradation matrix must preserve stale graph snapshot contract",
    )?;

    let search_error_fixture = include_str!("fixtures/golden/error/search_index.golden");
    ensure(
        search_error_fixture.contains("\"code\":\"search_index\""),
        "search index stale error contract must remain stable",
    )?;

    let dossier_paths = command_dossiers
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    let risks_to_inspect = vec![
        "high_risk_preflight_block".to_string(),
        "semantic_disabled".to_string(),
        "graph_snapshot_stale".to_string(),
        "search_index_stale".to_string(),
    ];
    let context_to_use = first_item_ids.clone();

    write_json(
        &scenario_dir.join("scenario-summary.json"),
        &json!({
            "schema": "ee.e2e.scenario_summary.v1",
            "scenarioId": scenario,
            "beadId": "eidetic_engine_cli-gbp2",
            "fixtureIds": [
                "fx.fresh_workspace.v1",
                "fx.manual_memory.v1",
                "fx.release_failure.v1",
                "fx.async_migration.v1"
            ],
            "commandDossiers": dossier_paths,
            "contextToUse": {
                "task": "prepare release",
                "selectedMemoryIds": context_to_use,
                "packHash": context_json_first_value["data"]["pack"]["hash"],
                "provenanceSchemes": context_json_first_value["data"]["pack"]["provenanceFooter"]["schemes"]
            },
            "risksToInspect": {
                "riskLevel": preflight_json["data"]["risk_level"],
                "preflightCleared": preflight_json["data"]["cleared"],
                "blockReason": preflight_json["data"]["block_reason"],
                "riskBriefId": preflight_json["data"]["risk_brief_id"]
            },
            "missingOrStaleEvidence": {
                "degradationCodes": risks_to_inspect,
                "notes": [
                    "Semantic-disabled fallback remains pinned in metamorphic evaluation fixture.",
                    "Graph stale degradation D300 remains pinned in degradation matrix fixture.",
                    "Search stale index error contract remains pinned in error fixture."
                ]
            },
            "determinism": {
                "contextItemOrderStable": true,
                "firstRunItemIds": first_item_ids,
                "secondRunItemIds": second_item_ids
            },
            "stdoutIsolation": {
                "jsonCommands": "stdout-only machine data; stderr empty",
                "markdownCommands": "stdout markdown only; stderr empty"
            },
            "redactionStatus": "checked"
        }),
    )?;

    ensure(
        scenario_dir.join("scenario-summary.json").is_file(),
        "scenario summary must be written",
    )
}
