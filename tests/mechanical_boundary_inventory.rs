const CLI_SOURCE: &str = include_str!("../src/cli/mod.rs");
const INVENTORY: &str = include_str!("../docs/mechanical-boundary-command-inventory.md");

const REQUIRED_MATRIX_HEADERS: &[&str] = &[
    "Surface",
    "Classification",
    "Owner / ADR",
    "README workflow(s)",
    "Mechanical data source",
    "Skill handoff",
    "Degraded code if unavailable",
    "Side-effect / idempotency",
    "Runtime / cancellation posture",
    "Fixture / golden coverage",
    "JSON schema expectation",
    "Required coverage owner",
];

#[test]
fn mechanical_boundary_inventory_covers_all_cli_command_paths() -> Result<(), String> {
    let commands = command_paths_from_extract_function(CLI_SOURCE)?;
    assert_eq!(
        commands.len(),
        144,
        "unexpected CLI command count; update the mechanical boundary inventory"
    );

    let missing = commands
        .iter()
        .filter(|command| !INVENTORY.contains(&format!("`{command}`")))
        .cloned()
        .collect::<Vec<_>>();

    assert!(
        missing.is_empty(),
        "mechanical boundary inventory missing command path(s): {missing:?}"
    );
    assert!(
        INVENTORY.contains("Unmapped command count: 0"),
        "inventory must record the unmapped command count"
    );
    Ok(())
}

#[test]
fn command_boundary_matrix_covers_public_command_families() -> Result<(), String> {
    let commands = command_paths_from_extract_function(CLI_SOURCE)?;
    let matrix = matrix_section(INVENTORY)?;
    let mut families = commands
        .iter()
        .filter_map(|command| command.split_whitespace().next())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    families.sort();
    families.dedup();

    let missing = families
        .iter()
        .filter(|family| !matrix.contains(&format!("`{family}`")))
        .cloned()
        .collect::<Vec<_>>();

    assert!(
        missing.is_empty(),
        "command boundary matrix missing public command families: {missing:?}"
    );
    Ok(())
}

#[test]
fn command_boundary_matrix_has_required_columns_and_complete_rows() -> Result<(), String> {
    let rows = matrix_rows(INVENTORY)?;
    let header = rows
        .first()
        .ok_or_else(|| "command boundary matrix must include a header row".to_owned())?;

    assert_eq!(
        header, REQUIRED_MATRIX_HEADERS,
        "command boundary matrix headers changed; update tests and frv3 contract together"
    );

    let allowed_classifications = [
        "mechanical CLI",
        "mechanical CLI or optional adapter wrapper",
        "optional adapter wrapper",
        "fix backing data",
        "split",
        "move to skill unless static lookup",
        "move to skill or split deterministic tagging",
        "degrade/unavailable pending implementation",
    ];

    for row in rows.iter().skip(2) {
        assert_eq!(
            row.len(),
            header.len(),
            "matrix row has wrong cell count: {row:?}"
        );
        let classification = row_cell(row, 1, "classification")?;
        let owner = row_cell(row, 2, "owner")?;
        let workflow = row_cell(row, 3, "workflow")?;
        let data_source = row_cell(row, 4, "data source")?;
        let side_effect = row_cell(row, 7, "side-effect")?;
        let runtime = row_cell(row, 8, "runtime")?;
        let schema = row_cell(row, 10, "schema")?;
        let coverage = row_cell(row, 11, "coverage")?;

        assert!(
            allowed_classifications
                .iter()
                .any(|allowed| classification.contains(allowed)),
            "matrix row has unsupported classification: {row:?}"
        );
        assert!(
            owner.contains("eidetic_engine_cli-") && owner.contains("ADR 0011"),
            "matrix row must name owning bead and ADR: {row:?}"
        );
        assert!(
            !workflow.is_empty()
                && !data_source.is_empty()
                && !side_effect.is_empty()
                && !runtime.is_empty(),
            "matrix row must include workflow, data source, side-effect, and runtime posture: {row:?}"
        );
        assert!(
            schema.contains("ee.response.v1") || schema.contains("ee.error.v1"),
            "matrix row must name machine schema expectation: {row:?}"
        );
        assert!(
            [
                "unit",
                "contract",
                "e2e",
                "golden",
                "smoke",
                "fixture",
                "skill-boundary",
                "runtime",
            ]
            .iter()
            .any(|needle| coverage.contains(needle)),
            "matrix row must name a concrete coverage owner/type: {row:?}"
        );
    }

    Ok(())
}

#[test]
fn skill_only_matrix_rows_have_skill_handoff_and_boundary_coverage() -> Result<(), String> {
    for row in matrix_rows(INVENTORY)?.iter().skip(2) {
        let classification = row_cell(row, 1, "classification")?;
        let skill_handoff = row_cell(row, 5, "skill handoff")?;
        let coverage = row_cell(row, 11, "coverage")?;
        if classification.contains("move to skill") {
            assert_ne!(
                skill_handoff, "none",
                "skill-only row must name handoff: {row:?}"
            );
            assert!(
                coverage.contains("skill-boundary"),
                "skill-only row must require skill-boundary coverage: {row:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn matrix_e2e_log_schema_records_required_fields() {
    for required in [
        "ee.command_boundary_matrix.e2e_log.v1",
        "generated command list",
        "matrix path and BLAKE3 hash",
        "missing and extra command rows",
        "classification summary",
        "side-effect coverage summary",
        "schema coverage summary",
        "workflow parity coverage",
        "fixture/evidence bundle hashes",
        "stdout and stderr artifact paths",
        "first-failure diagnosis",
    ] {
        assert!(
            INVENTORY.contains(required),
            "matrix E2E log schema missing required field text: {required}"
        );
    }
}

#[test]
fn mechanical_boundary_inventory_names_mock_and_stub_surfaces() {
    for anchor in [
        "src/core/causal.rs:335",
        "src/core/causal.rs:892",
        "src/core/procedure.rs:1174",
        "src/core/rehearse.rs:364",
        "src/output/mod.rs:4092",
        "src/core/tripwire.rs:376",
        "src/core/preflight.rs:870",
        "src/core/situation.rs:1926",
        "src/core/certificate.rs:457",
        "src/core/economy.rs:735",
        "src/core/memory.rs:1572",
    ] {
        assert!(
            INVENTORY.contains(anchor),
            "inventory missing risky surface anchor {anchor}"
        );
    }
}

fn matrix_section(inventory: &str) -> Result<&str, String> {
    let (_, after_start) = inventory
        .split_once("## Command Boundary Matrix")
        .ok_or_else(|| "Command Boundary Matrix section must exist".to_owned())?;
    let (section, _) = after_start
        .split_once("### Matrix Maintenance Rules")
        .ok_or_else(|| "Matrix Maintenance Rules section must follow matrix".to_owned())?;
    Ok(section)
}

fn matrix_rows(inventory: &str) -> Result<Vec<Vec<String>>, String> {
    let rows = matrix_section(inventory)?
        .lines()
        .filter(|line| line.starts_with('|'))
        .map(markdown_row_cells)
        .collect::<Vec<_>>();

    if rows.len() < 3 {
        Err("command boundary matrix must include header, delimiter, and data rows".to_owned())
    } else {
        Ok(rows)
    }
}

fn markdown_row_cells(line: &str) -> Vec<String> {
    line.trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_owned())
        .collect()
}

fn row_cell<'a>(row: &'a [String], index: usize, context: &str) -> Result<&'a str, String> {
    row.get(index)
        .map(String::as_str)
        .ok_or_else(|| format!("matrix row missing {context} cell at index {index}: {row:?}"))
}

fn command_paths_from_extract_function(source: &str) -> Result<Vec<String>, String> {
    let start_marker = "fn extract_command_path(cli: &Cli) -> String {";
    let end_marker = "\n    /// Returns a stable identifier";
    let (_, after_start) = source
        .split_once(start_marker)
        .ok_or_else(|| "extract_command_path function must exist".to_owned())?;
    let (body, _) = after_start
        .split_once(end_marker)
        .ok_or_else(|| "extract_command_path function end marker must exist".to_owned())?;

    let mut strings = Vec::new();
    let bytes = body.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes.get(index) != Some(&b'"') {
            index += 1;
            continue;
        }

        index += 1;
        let start = index;
        while let Some(byte) = bytes.get(index) {
            if *byte == b'"' {
                break;
            }

            if *byte == b'\\' {
                index += 2;
            } else {
                index += 1;
            }
        }
        let value = body
            .get(start..index)
            .ok_or_else(|| format!("invalid string literal span {start}..{index}"))?;
        if value
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase())
        {
            strings.push(value.to_owned());
        }
        index += 1;
    }

    strings.sort();
    strings.dedup();
    Ok(strings)
}
