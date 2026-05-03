//! EE-USR-003: in-task retrieval, why, doctor, and tripwire recovery scenario.
//!
//! Exercises the agent journey *during* active work: surfacing relevant
//! memory through search and context, explaining a selection with
//! `ee why`, producing a non-destructive repair plan via
//! `ee doctor --fix-plan`, and identifying a risky action with
//! `ee tripwire list/check` without mutating memory state.
//!
//! Pairs with `tests/usr002_pre_task_brief_scenario.rs` (pre-task) and
//! `tests/usr005_degraded_scenario.rs` (degraded/offline).

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

#[cfg(unix)]
fn parse_json_stdout(output: &Output, context: &str) -> Result<JsonValue, String> {
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("{context}: stdout must be valid JSON: {error}"))
}

#[cfg(unix)]
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
#[allow(clippy::too_many_arguments)]
fn write_step_artifacts(
    dossier_dir: &Path,
    workspace: &Path,
    args: &[&str],
    output: &Output,
    elapsed_ms: u128,
    fixture_id: &str,
    expected_schema: Option<&str>,
    golden_path: Option<&str>,
) -> TestResult {
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
    let schema_status =
        match (expected_schema, stdout_json.as_ref()) {
            (Some(expected), Some(value)) => value
                .get("schema")
                .and_then(JsonValue::as_str)
                .map_or("missing", |actual| {
                    if actual == expected {
                        "matched"
                    } else {
                        "mismatched"
                    }
                }),
            (Some(_), None) => "not_json",
            (None, _) => "not_applicable",
        };

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

    Ok(())
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

    let started = Instant::now();
    let output = run_ee(args)?;
    let elapsed_ms = started.elapsed().as_millis();

    write_step_artifacts(
        &dossier_dir,
        workspace,
        args,
        &output,
        elapsed_ms,
        fixture_id,
        Some(expected_schema),
        golden_path,
    )?;

    Ok(LoggedStep {
        output,
        dossier_dir,
    })
}

#[cfg(unix)]
#[test]
fn in_task_recovery_scenario_explains_selection_repair_and_tripwires() -> TestResult {
    // Pin the contracts this scenario depends on so detector regressions are
    // caught even before the live commands execute.
    let tripwire_check_contract =
        include_str!("fixtures/golden/preflight/gate16_tripwire_check.json.golden");
    ensure(
        tripwire_check_contract.contains("\"schema\": \"ee.tripwire.check.v1\"")
            && tripwire_check_contract.contains("\"should_halt\": true")
            && tripwire_check_contract.contains("\"evaluation\": \"false_alarm\""),
        "tripwire check contract must keep halt + false_alarm feedback shape",
    )?;
    let tripwire_list_contract =
        include_str!("fixtures/golden/preflight/gate16_tripwire_list.json.golden");
    ensure(
        tripwire_list_contract.contains("\"schema\": \"ee.tripwire.list.v1\""),
        "tripwire list contract must keep schema",
    )?;
    let degradation_matrix = include_str!("fixtures/golden/degradation/matrix.json.golden");
    ensure(
        degradation_matrix.contains("\"id\": \"D003\"")
            && degradation_matrix.contains("\"id\": \"D300\""),
        "degradation matrix must keep search-stale (D003) and graph-stale (D300) codes",
    )?;
    let search_error_contract = include_str!("fixtures/golden/error/search_index.golden");
    ensure(
        search_error_contract.contains("\"code\":\"search_index\""),
        "search index stale error contract must remain stable",
    )?;

    let scenario = "usr_in_task_recovery";
    let scenario_dir = unique_scenario_dir(scenario)?;
    let workspace = scenario_dir.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;

    let workspace_arg = workspace.to_string_lossy().into_owned();
    let mut command_dossiers = Vec::new();

    // 01. init - open the workspace.
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

    // 02. remember - high-confidence procedural rule.
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
            "release,verification",
            "--source",
            "file://tests/fixtures/eval/release_failure/source_memory.json#L1",
            "Run cargo fmt --check, cargo clippy --all-targets -- -D warnings, and cargo test before tagging a release.",
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

    // 03. remember - episodic failure that adds conflicting low-confidence evidence.
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
            "Release v0.1.4 failed because clippy errors slipped past local checks.",
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

    // 04. index rebuild - move from "stale" to "fresh".
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

    // 05. search - retrieval surfaces both pieces of evidence.
    let search = run_logged_json_step(
        &scenario_dir,
        "05_search",
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
        None,
    )?;
    command_dossiers.push(search.dossier_dir.clone());
    ensure(search.output.status.success(), "search should succeed")?;
    assert_json_machine_stdout(&search.output, "search")?;
    let search_json = parse_json_stdout(&search.output, "search")?;
    let search_ids = search_json["data"]["results"]
        .as_array()
        .ok_or_else(|| "search results missing".to_string())?
        .iter()
        .filter_map(|result| result["doc_id"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    ensure(
        search_ids.contains(&rule_id) && search_ids.contains(&failure_id),
        "search must surface both rule and failure evidence",
    )?;

    // 06. context - pack with provenance footer.
    let context = run_logged_json_step(
        &scenario_dir,
        "06_context_json",
        &workspace,
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "context",
            "recover from clippy regression in release",
            "--max-tokens",
            "4000",
        ],
        "fx.release_failure.v1",
        "ee.response.v1",
        None,
    )?;
    command_dossiers.push(context.dossier_dir.clone());
    ensure(context.output.status.success(), "context should succeed")?;
    assert_json_machine_stdout(&context.output, "context")?;
    let context_json = parse_json_stdout(&context.output, "context")?;
    let context_item_ids = context_json["data"]["pack"]["items"]
        .as_array()
        .ok_or_else(|| "context items missing".to_string())?
        .iter()
        .filter_map(|item| item["memoryId"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    ensure(
        !context_item_ids.is_empty(),
        "context pack must include at least one item",
    )?;
    ensure(
        context_json["data"]["pack"]["items"]
            .as_array()
            .is_some_and(|items| {
                items
                    .iter()
                    .all(|item| item["provenance"].as_array().is_some_and(|p| !p.is_empty()))
            }),
        "every context item must carry provenance evidence",
    )?;

    // 07. why - explains selection of the rule, including alternatives /
    //          contradictions / degraded honesty fields.
    let why = run_logged_json_step(
        &scenario_dir,
        "07_why_rule",
        &workspace,
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "why",
            rule_id.as_str(),
        ],
        "fx.manual_memory.v1",
        "ee.response.v1",
        None,
    )?;
    command_dossiers.push(why.dossier_dir.clone());
    ensure(why.output.status.success(), "why should succeed")?;
    assert_json_machine_stdout(&why.output, "why")?;
    let why_json = parse_json_stdout(&why.output, "why")?;
    ensure(
        why_json["data"]["command"] == json!("why"),
        "why must report command=why",
    )?;
    ensure(
        why_json["data"]["memory_id"] == json!(rule_id),
        "why must echo the requested memory_id",
    )?;
    ensure(
        why_json["data"]["found"] == json!(true),
        "why must report the memory as found",
    )?;
    ensure(
        why_json["data"]["storage"]["origin"].is_string()
            && why_json["data"]["storage"]["trust_class"].is_string(),
        "why must explain storage origin and trust class",
    )?;
    ensure(
        why_json["data"]["retrieval"]["confidence"].is_number()
            && why_json["data"]["retrieval"]["utility"].is_number()
            && why_json["data"]["retrieval"]["importance"].is_number()
            && why_json["data"]["retrieval"]["level"] == json!("procedural")
            && why_json["data"]["retrieval"]["kind"] == json!("rule"),
        "why must include retrieval scoring components, level, and kind",
    )?;
    ensure(
        why_json["data"]["contradictions"].is_array(),
        "why must include contradictions array (alternatives / counter-evidence)",
    )?;
    ensure(
        why_json["data"]["links"].is_array(),
        "why must include links array",
    )?;
    ensure(
        why_json["data"]["degraded"].is_array(),
        "why must include degraded array for honesty about missing signals",
    )?;

    // 08. doctor --fix-plan - non-destructive ordered repair plan.
    let doctor = run_logged_json_step(
        &scenario_dir,
        "08_doctor_fix_plan",
        &workspace,
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "doctor",
            "--fix-plan",
        ],
        "fx.fresh_workspace.v1",
        "ee.response.v1",
        None,
    )?;
    command_dossiers.push(doctor.dossier_dir.clone());
    ensure(doctor.output.status.success(), "doctor should succeed")?;
    assert_json_machine_stdout(&doctor.output, "doctor")?;
    let doctor_json = parse_json_stdout(&doctor.output, "doctor")?;
    ensure(
        doctor_json["data"]["mode"] == json!("fix-plan"),
        "doctor must report mode=fix-plan",
    )?;
    let steps = doctor_json["data"]["steps"]
        .as_array()
        .ok_or_else(|| "doctor steps missing".to_string())?;
    let mut repair_actions = Vec::with_capacity(steps.len());
    for (index, step) in steps.iter().enumerate() {
        let order = step["order"]
            .as_u64()
            .ok_or_else(|| "doctor step.order missing".to_string())?;
        ensure(
            order == u64::try_from(index + 1).map_err(|error| error.to_string())?,
            format!("doctor step order mismatch at index {index}"),
        )?;
        let command = step["command"]
            .as_str()
            .ok_or_else(|| "doctor step.command missing".to_string())?
            .to_string();
        ensure(
            !command.contains("rm -rf")
                && !command.contains("git reset --hard")
                && !command.contains("git clean -fd"),
            format!("doctor fix-plan must stay non-destructive (offending command: {command})"),
        )?;
        repair_actions.push(command);
    }

    // 09. tripwire list (--include-disarmed) - remains conservative until
    //     persisted tripwire rules replace generated fixtures.
    let tripwire_list = run_logged_json_step(
        &scenario_dir,
        "09_tripwire_list",
        &workspace,
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "tripwire",
            "list",
            "--include-disarmed",
        ],
        "fx.tripwire_unavailable.v1",
        "ee.response.v1",
        None,
    )?;
    command_dossiers.push(tripwire_list.dossier_dir.clone());
    ensure(
        !tripwire_list.output.status.success(),
        "tripwire list should degrade until persisted tripwire rules are available",
    )?;
    assert_json_machine_stdout(&tripwire_list.output, "tripwire list")?;
    let tripwire_list_json = parse_json_stdout(&tripwire_list.output, "tripwire list")?;
    ensure(
        tripwire_list_json["schema"] == json!("ee.response.v1")
            && tripwire_list_json["data"]["code"] == json!("tripwire_store_unavailable"),
        "tripwire list degraded schema/code",
    )?;
    let tripwire_ids = Vec::<String>::new();

    // 10. tripwire check tw_004 --dry-run - also degrades rather than
    //     evaluating generated sample tripwires as if they were persisted.
    let tripwire_check = run_logged_json_step(
        &scenario_dir,
        "10_tripwire_check",
        &workspace,
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "tripwire",
            "check",
            "tw_004",
            "--task-outcome",
            "success",
            "--dry-run",
        ],
        "fx.tripwire_unavailable.v1",
        "ee.response.v1",
        None,
    )?;
    command_dossiers.push(tripwire_check.dossier_dir.clone());
    ensure(
        !tripwire_check.output.status.success(),
        "tripwire check should degrade until explicit event payloads are available",
    )?;
    assert_json_machine_stdout(&tripwire_check.output, "tripwire check")?;
    let tripwire_check_json = parse_json_stdout(&tripwire_check.output, "tripwire check")?;
    ensure(
        tripwire_check_json["schema"] == json!("ee.response.v1")
            && tripwire_check_json["data"]["code"] == json!("tripwire_store_unavailable")
            && tripwire_check_json["data"]["evidenceIds"] == json!([]),
        "tripwire check degraded schema/code/evidence",
    )?;

    // Re-check tw_004 to assert unavailable output is deterministic and still
    // does not claim a persisted rule evaluation.
    let tripwire_check_replay = run_logged_json_step(
        &scenario_dir,
        "11_tripwire_check_replay",
        &workspace,
        &[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "tripwire",
            "check",
            "tw_004",
            "--task-outcome",
            "success",
            "--dry-run",
        ],
        "fx.tripwire_unavailable.v1",
        "ee.response.v1",
        None,
    )?;
    command_dossiers.push(tripwire_check_replay.dossier_dir.clone());
    ensure(
        !tripwire_check_replay.output.status.success(),
        "tripwire check replay should degrade",
    )?;
    let replay_json = parse_json_stdout(&tripwire_check_replay.output, "tripwire check replay")?;
    ensure(
        replay_json["data"]["code"] == tripwire_check_json["data"]["code"]
            && replay_json["data"]["followUpBead"] == tripwire_check_json["data"]["followUpBead"],
        "tripwire check degradation must be deterministic across dry-run replays",
    )?;

    // Scenario summary - records selected memory IDs, alternatives, degradation
    // codes, repair-plan actions, and tripwire match evidence as the bead
    // acceptance criteria require.
    let dossier_paths = command_dossiers
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    let degradation_codes = why_json["data"]["degraded"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["code"].as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let alternative_links = why_json["data"]["links"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["linkedMemoryId"].as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    write_json(
        &scenario_dir.join("scenario-summary.json"),
        &json!({
            "schema": "ee.e2e.scenario_summary.v1",
            "scenarioId": scenario,
            "beadId": "eidetic_engine_cli-g2jl",
            "fixtureIds": [
                "fx.fresh_workspace.v1",
                "fx.manual_memory.v1",
                "fx.release_failure.v1",
                "fx.tripwire_samples.v1"
            ],
            "commandDossiers": dossier_paths,
            "selection": {
                "task": "recover from clippy regression in release",
                "selectedMemoryIds": context_item_ids,
                "explainedMemoryId": rule_id,
                "alternativeLinkedMemoryIds": alternative_links,
                "rejectedOrAlternativeIds": [failure_id],
            },
            "repairPlan": {
                "mode": "fix-plan",
                "stepCount": steps.len(),
                "actions": repair_actions,
                "destructiveActions": Vec::<String>::new(),
            },
            "tripwireEvidence": {
                "listedIds": tripwire_ids,
                "checkedTripwireId": "tw_004",
                "degradedCode": tripwire_check_json["data"]["code"],
                "shouldHalt": JsonValue::Null,
                "feedbackEvaluation": JsonValue::Null,
                "durableMutation": false,
                "evidencePreserved": false,
            },
            "degradationCodes": degradation_codes,
            "stdoutIsolation": "stdout-only machine data; stderr empty for every JSON command",
            "redactionStatus": "checked"
        }),
    )?;

    ensure(
        scenario_dir.join("scenario-summary.json").is_file(),
        "scenario summary must be written",
    )
}

#[cfg(not(unix))]
#[test]
fn in_task_recovery_scenario_skipped_on_non_unix() -> TestResult {
    // Logged-artifact filesystem layout expects POSIX semantics; skip on Windows
    // where the parallel scenarios in this directory also skip.
    let _ = run_ee;
    Ok(())
}
