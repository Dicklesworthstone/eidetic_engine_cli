//! Epic-level contract checks for the agent outcome scenario pack.

use std::collections::BTreeSet;

type TestResult = Result<(), String>;

const MATRIX_DOC: &str = include_str!("../docs/agent-outcome-scenarios.md");
const TRACEABILITY_DOC: &str = include_str!("../docs/fixture-provenance-traceability.md");
const CLOSURE_DOSSIER_DOC: &str = include_str!("../docs/closure-dossier.md");

#[derive(Clone, Copy)]
struct ScenarioExpectation {
    id: &'static str,
    usr_label: &'static str,
    owning_bead: &'static str,
    fixture_family: &'static str,
    fixture_id: &'static str,
    degraded_code: &'static str,
    evidence_reference: &'static str,
}

const SCENARIOS: &[ScenarioExpectation] = &[
    ScenarioExpectation {
        id: "usr_pre_task_brief",
        usr_label: "EE-USR-002",
        owning_bead: "eidetic_engine_cli-gbp2",
        fixture_family: "release_failure",
        fixture_id: "fx.release_failure.v1",
        degraded_code: "semantic_disabled",
        evidence_reference: "tests/usr002_pre_task_brief_scenario.rs",
    },
    ScenarioExpectation {
        id: "usr_in_task_recovery",
        usr_label: "EE-USR-003",
        owning_bead: "eidetic_engine_cli-g2jl",
        fixture_family: "ci_clippy_failure",
        fixture_id: "fx.ci_clippy_failure.v1",
        degraded_code: "search_index_stale",
        evidence_reference: "tests/usr003_in_task_scenario.rs",
    },
    ScenarioExpectation {
        id: "usr_post_task_learning",
        usr_label: "EE-USR-004",
        owning_bead: "eidetic_engine_cli-1mlo",
        fixture_family: "procedure_drift",
        fixture_id: "fx.procedure_drift.v1",
        degraded_code: "contradicted evidence",
        evidence_reference: "tests/advanced_e2e.rs",
    },
    ScenarioExpectation {
        id: "usr_degraded_offline_trust",
        usr_label: "EE-USR-005",
        owning_bead: "eidetic_engine_cli-r8r0",
        fixture_family: "offline_degraded",
        fixture_id: "fx.offline_degraded.v1",
        degraded_code: "cass_unavailable",
        evidence_reference: "tests/usr005_degraded_scenario.rs",
    },
    ScenarioExpectation {
        id: "usr_privacy_export",
        usr_label: "EE-USR-006",
        owning_bead: "eidetic_engine_cli-9sd5",
        fixture_family: "secret_redaction",
        fixture_id: "fx.secret_redaction.v1",
        degraded_code: "redaction_applied",
        evidence_reference: "tests/usr006_privacy_redaction_backup_scenario.rs",
    },
    ScenarioExpectation {
        id: "usr_workspace_continuity",
        usr_label: "EE-USR-007",
        owning_bead: "eidetic_engine_cli-jqhn",
        fixture_family: "multi_workspace",
        fixture_id: "fx.multi_workspace.v1",
        degraded_code: "ambiguous workspace",
        evidence_reference: "tests/smoke.rs",
    },
];

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn markdown_table_rows(doc: &str, section_heading: &str) -> Vec<Vec<String>> {
    let mut in_section = false;
    let mut rows = Vec::new();

    for line in doc.lines() {
        if line.trim() == section_heading {
            in_section = true;
            continue;
        }

        if in_section && line.starts_with("## ") {
            break;
        }

        if !in_section || !line.trim_start().starts_with('|') {
            continue;
        }

        let cells = line
            .trim()
            .trim_matches('|')
            .split('|')
            .map(|cell| cell.trim().to_string())
            .collect::<Vec<_>>();
        let is_separator = cells
            .iter()
            .all(|cell| cell.chars().all(|character| matches!(character, '-' | ' ')));

        if !is_separator {
            rows.push(cells);
        }
    }

    rows
}

fn table_body_rows(rows: &[Vec<String>]) -> &[Vec<String>] {
    if rows.first().is_some_and(|row| {
        row.first()
            .is_some_and(|cell| matches!(cell.as_str(), "ID" | "Scenario ID"))
    }) {
        &rows[1..]
    } else {
        rows
    }
}

fn clean_id(cell: &str) -> &str {
    cell.trim().trim_matches('`')
}

fn valid_coverage_state(cell: &str) -> bool {
    matches!(
        clean_id(cell),
        "executable_e2e" | "golden_contract" | "implemented" | "staged_skip"
    )
}

fn find_row<'a>(rows: &'a [Vec<String>], id: &str) -> Option<&'a [String]> {
    rows.iter()
        .find(|row| row.first().is_some_and(|cell| clean_id(cell) == id))
        .map(Vec::as_slice)
}

fn scenario_ids(rows: &[Vec<String>]) -> BTreeSet<String> {
    rows.iter()
        .filter_map(|row| row.first())
        .map(|cell| clean_id(cell).to_string())
        .filter(|id| id.starts_with("usr_"))
        .collect()
}

#[test]
fn agent_outcome_matrix_covers_all_epic_scenarios() -> TestResult {
    let table = markdown_table_rows(MATRIX_DOC, "## Scenarios");
    let rows = table_body_rows(&table);

    ensure(
        rows.len() == SCENARIOS.len(),
        format!(
            "scenario matrix should contain exactly {} body rows, found {}",
            SCENARIOS.len(),
            rows.len()
        ),
    )?;

    for scenario in SCENARIOS {
        let row = find_row(rows, scenario.id)
            .ok_or_else(|| format!("missing scenario matrix row for {}", scenario.id))?;
        ensure(
            row.len() == 8,
            format!("{} matrix row should have 8 columns", scenario.id),
        )?;
        ensure(
            row[2].contains(scenario.fixture_family),
            format!(
                "{} should list fixture family {}",
                scenario.id, scenario.fixture_family
            ),
        )?;
        ensure(
            row[3].contains("ee "),
            format!("{} should list replayable ee commands", scenario.id),
        )?;
        ensure(
            row[4].contains("golden") || row[4].contains("JSON") || row[4].contains("schema"),
            format!("{} should name golden or schema artifacts", scenario.id),
        )?;
        ensure(
            row[5].contains(scenario.degraded_code),
            format!(
                "{} should name degraded branch {}",
                scenario.id, scenario.degraded_code
            ),
        )?;
        ensure(
            row[6].contains(scenario.owning_bead) && row[6].contains(scenario.usr_label),
            format!(
                "{} should link owning bead {} and label {}",
                scenario.id, scenario.owning_bead, scenario.usr_label
            ),
        )?;
        ensure(
            row[7].len() > 40,
            format!("{} should include an agent success signal", scenario.id),
        )?;
    }

    Ok(())
}

#[test]
fn scenario_traceability_contract_is_attributable() -> TestResult {
    let table = markdown_table_rows(TRACEABILITY_DOC, "## `scenario_traceability.v1`");
    let rows = table_body_rows(&table);

    ensure(
        rows.len() == SCENARIOS.len(),
        format!(
            "traceability table should contain exactly {} body rows, found {}",
            SCENARIOS.len(),
            rows.len()
        ),
    )?;

    for scenario in SCENARIOS {
        let row = find_row(rows, scenario.id)
            .ok_or_else(|| format!("missing traceability row for {}", scenario.id))?;
        ensure(
            row.len() == 8,
            format!("{} traceability row should have 8 columns", scenario.id),
        )?;
        ensure(
            valid_coverage_state(&row[1]),
            format!("{} should have an explicit coverage state", scenario.id),
        )?;
        ensure(
            row[2].contains(scenario.fixture_id),
            format!(
                "{} should link fixture {}",
                scenario.id, scenario.fixture_id
            ),
        )?;
        ensure(
            !row[3].trim().is_empty(),
            format!("{} should list command surfaces", scenario.id),
        )?;
        ensure(
            !row[4].trim().is_empty(),
            format!("{} should list golden or schema contracts", scenario.id),
        )?;
        ensure(
            row[5].contains(scenario.degraded_code),
            format!(
                "{} should list degraded branch {}",
                scenario.id, scenario.degraded_code
            ),
        )?;
        ensure(
            row[6].contains("write") || row[6].contains("read-only") || row[6].contains("denied"),
            format!("{} should state effect expectations", scenario.id),
        )?;
        ensure(
            row[7].contains(scenario.owning_bead),
            format!(
                "{} should link owning bead {}",
                scenario.id, scenario.owning_bead
            ),
        )?;
    }

    Ok(())
}

#[test]
fn executable_evidence_status_covers_every_agent_journey() -> TestResult {
    let table = markdown_table_rows(MATRIX_DOC, "## Executable Evidence Status");
    let rows = table_body_rows(&table);

    ensure(
        rows.len() == SCENARIOS.len(),
        format!(
            "executable evidence table should contain exactly {} body rows, found {}",
            SCENARIOS.len(),
            rows.len()
        ),
    )?;

    for scenario in SCENARIOS {
        let row = find_row(rows, scenario.id)
            .ok_or_else(|| format!("missing executable evidence row for {}", scenario.id))?;
        ensure(
            row.len() == 3,
            format!(
                "{} executable evidence row should have 3 columns",
                scenario.id
            ),
        )?;
        ensure(
            row[1].contains(scenario.evidence_reference),
            format!(
                "{} should link executable evidence {}",
                scenario.id, scenario.evidence_reference
            ),
        )?;
        ensure(
            row[2].len() > 40,
            format!("{} should describe what the evidence proves", scenario.id),
        )?;
    }

    Ok(())
}

#[test]
fn traceability_has_graduated_to_executable_e2e_for_each_scenario() -> TestResult {
    let table = markdown_table_rows(TRACEABILITY_DOC, "## `scenario_traceability.v1`");
    let rows = table_body_rows(&table);

    for scenario in SCENARIOS {
        let row = find_row(rows, scenario.id)
            .ok_or_else(|| format!("missing traceability row for {}", scenario.id))?;
        ensure(
            clean_id(&row[1]) == "executable_e2e",
            format!(
                "{} should be traceable as executable_e2e, found {}",
                scenario.id, row[1]
            ),
        )?;
    }

    Ok(())
}

#[test]
fn epic_rollup_closeout_requires_user_visible_evidence() -> TestResult {
    let profile_rows = markdown_table_rows(CLOSURE_DOSSIER_DOC, "## Profiles");
    let profiles = table_body_rows(&profile_rows);
    let epic_rollup = find_row(profiles, "epic_rollup")
        .ok_or_else(|| "closure dossier profiles must define epic_rollup".to_string())?;

    ensure(
        epic_rollup.len() == 3,
        "epic_rollup profile row should have 3 columns",
    )?;
    ensure(
        epic_rollup[1].contains("Epic") || epic_rollup[1].contains("multi-bead"),
        "epic_rollup profile must apply to epic or multi-bead closure",
    )?;
    ensure(
        epic_rollup[2].contains("Child bead list")
            && epic_rollup[2].contains("aggregate evidence")
            && epic_rollup[2].contains("release readiness"),
        "epic_rollup profile must require child, aggregate, and readiness evidence",
    )?;

    for required in [
        "- `child_beads`",
        "- `workflow_coverage`",
        "- `verification_commands`",
        "- `artifact_roots`",
        "- `logged_fields`",
        "- `degraded_states_covered`",
        "- `postponed_scope`",
    ] {
        ensure(
            CLOSURE_DOSSIER_DOC.contains(required),
            format!("epic rollup requirements should include `{required}`"),
        )?;
    }

    Ok(())
}

#[test]
fn epic_close_reason_must_not_be_child_completion_only() -> TestResult {
    ensure(
        CLOSURE_DOSSIER_DOC.contains("invalid if it only says all children are closed"),
        "closure dossier must reject child-completion-only epic close reasons",
    )?;

    for required in [
        "aggregate user workflows",
        "verification path",
        "degraded states",
        "postponed scope",
    ] {
        ensure(
            CLOSURE_DOSSIER_DOC.contains(required),
            format!("epic close reason must summarize `{required}`"),
        )?;
    }

    ensure(
        CLOSURE_DOSSIER_DOC.contains("artifact/log") && CLOSURE_DOSSIER_DOC.contains("locations"),
        "epic close reason must summarize artifact/log locations",
    )?;

    for invalid_reason in [
        "\"tests pass\"",
        "\"done\"",
        "\"implemented\"",
        "\"manual testing ok\"",
        "\"should work\"",
        "\"fixed the issue\"",
    ] {
        ensure(
            CLOSURE_DOSSIER_DOC.contains(invalid_reason),
            format!("closure dossier should reject shallow reason {invalid_reason}"),
        )?;
    }

    Ok(())
}

#[test]
fn scenario_pack_artifacts_log_epic_required_fields() -> TestResult {
    for required in [
        "command, cwd, sanitized environment",
        "elapsed time, exit code",
        "stdout/stderr artifacts",
        "schema/golden status",
        "redaction status",
        "degradation status",
        "fixture IDs",
        "first-failure diagnosis",
    ] {
        ensure(
            MATRIX_DOC.contains(required),
            format!("matrix doc should require artifact field `{required}`"),
        )?;
    }

    for required in [
        "- `fixture_id`",
        "- `scenario_id`",
        "- `manifest_schema`",
        "- `manifest_content_hash`",
        "- command argv",
        "- cwd and resolved `--workspace`",
        "- sanitized environment overrides",
        "- elapsed time",
        "- exit code",
        "- stdout and stderr artifact paths",
        "- schema/golden validation status",
        "- redaction status",
        "- expected effect class",
        "- degradation codes observed",
        "- first-failure diagnosis",
    ] {
        ensure(
            TRACEABILITY_DOC.contains(required),
            format!("traceability doc should require artifact field `{required}`"),
        )?;
    }

    Ok(())
}

#[test]
fn scenario_matrix_and_traceability_have_identical_scenario_sets() -> TestResult {
    let matrix_rows = markdown_table_rows(MATRIX_DOC, "## Scenarios");
    let traceability_rows = markdown_table_rows(TRACEABILITY_DOC, "## `scenario_traceability.v1`");

    let matrix_ids = scenario_ids(table_body_rows(&matrix_rows));
    let traceability_ids = scenario_ids(table_body_rows(&traceability_rows));

    ensure(
        matrix_ids == traceability_ids,
        format!(
            "matrix scenarios {matrix_ids:?} should match traceability scenarios {traceability_ids:?}"
        ),
    )?;
    ensure(
        matrix_ids.len() == SCENARIOS.len(),
        format!("scenario pack should contain {} journeys", SCENARIOS.len()),
    )
}
