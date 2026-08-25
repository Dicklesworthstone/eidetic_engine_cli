use std::collections::BTreeSet;
use std::path::Path;

type TestResult = Result<(), String>;

const REPORT: &str = include_str!("../docs/plan-sweep-report.md");

#[derive(Debug)]
struct PlanRow<'a> {
    section_id: &'a str,
    section_title: &'a str,
    classification: &'a str,
    evidence_path: &'a str,
    test_bead_id: &'a str,
    verify_cmd: &'a str,
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn split_row(line: &str) -> Vec<&str> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect()
}

fn matrix_rows() -> Result<Vec<PlanRow<'static>>, String> {
    let mut in_matrix = false;
    let mut rows = Vec::new();

    for line in REPORT.lines() {
        if line.trim() == "## Machine-Checked Section Matrix" {
            in_matrix = true;
            continue;
        }
        if in_matrix && line.starts_with("## ") {
            break;
        }
        if !in_matrix || !line.starts_with('|') {
            continue;
        }
        if line.contains("section_id") || line.contains("------------") {
            continue;
        }

        let cells = split_row(line);
        ensure(
            cells.len() == 6,
            format!("plan matrix row must have 6 cells: {line}"),
        )?;
        rows.push(PlanRow {
            section_id: cells[0],
            section_title: cells[1],
            classification: cells[2],
            evidence_path: cells[3],
            test_bead_id: cells[4],
            verify_cmd: cells[5],
        });
    }

    Ok(rows)
}

fn evidence_paths(evidence_path: &str) -> impl Iterator<Item = &str> {
    evidence_path
        .split(';')
        .map(str::trim)
        .filter(|path| !path.is_empty() && *path != "-" && *path != "pending")
}

fn evidence_path_exists(path: &str) -> bool {
    Path::new(path).exists()
}

fn cargo_owned_evidence_path(path: &str) -> bool {
    // The tracker is verified by closure-lint and vision-coverage. RCH's
    // source transport intentionally excludes live `.beads` state, so this
    // Cargo contract must not duplicate that ownership boundary.
    path != ".beads/issues.jsonl"
}

#[test]
fn plan_sweep_matrix_covers_every_major_plan_section() -> TestResult {
    let rows = matrix_rows()?;
    ensure(
        rows.len() == 32,
        format!("expected 32 plan rows, got {}", rows.len()),
    )?;

    let mut ids = BTreeSet::new();
    for row in &rows {
        ensure(
            ids.insert(row.section_id),
            format!("duplicate plan section row {}", row.section_id),
        )?;
    }

    for expected in 0..=31 {
        let expected_id = format!("§{expected}");
        let dotted_heading = format!("### {expected_id}.");
        let spaced_heading = format!("### {expected_id} ");
        ensure(
            ids.contains(expected_id.as_str()),
            format!("missing plan section row {expected_id}"),
        )?;
        ensure(
            REPORT.contains(dotted_heading.as_str()) || REPORT.contains(spaced_heading.as_str()),
            format!("matrix row {expected_id} has no matching narrative heading"),
        )?;
    }

    Ok(())
}

#[test]
fn plan_sweep_matrix_rows_have_evidence_or_tracking_beads() -> TestResult {
    let allowed = BTreeSet::from([
        "Implemented-verified",
        "Implemented-unverified",
        "Stubbed",
        "Missing",
    ]);

    for row in matrix_rows()? {
        ensure(
            allowed.contains(row.classification),
            format!(
                "{} has unsupported classification {}",
                row.section_id, row.classification
            ),
        )?;
        ensure(
            !row.section_title.is_empty(),
            format!("{} has an empty section title", row.section_id),
        )?;

        match row.classification {
            "Implemented-verified" => {
                let evidence = evidence_paths(row.evidence_path).collect::<Vec<_>>();
                ensure(
                    !evidence.is_empty(),
                    format!("{} is verified but has no evidence path", row.section_id),
                )?;
                ensure(
                    row.test_bead_id == "-",
                    format!("{} is verified but has a test bead id", row.section_id),
                )?;
                ensure(
                    !row.verify_cmd.is_empty() && row.verify_cmd != "-",
                    format!("{} is verified but lacks a verify_cmd", row.section_id),
                )?;
                ensure(
                    !row.verify_cmd.contains('|'),
                    format!("{} verify_cmd must not contain a pipe", row.section_id),
                )?;
                for path in evidence
                    .into_iter()
                    .filter(|path| cargo_owned_evidence_path(path))
                {
                    ensure(
                        evidence_path_exists(path),
                        format!(
                            "{} verified evidence path does not exist: {path}",
                            row.section_id
                        ),
                    )?;
                }
            }
            "Implemented-unverified" | "Stubbed" | "Missing" => {
                // Tracker-file integrity is owned by closure-lint and
                // vision-coverage. This Cargo contract only checks that the
                // report carries a syntactically valid tracking reference.
                ensure(
                    row.evidence_path == "pending",
                    format!(
                        "{} is {} but evidence_path is not pending",
                        row.section_id, row.classification
                    ),
                )?;
                ensure(
                    row.test_bead_id.starts_with("bd-"),
                    format!(
                        "{} is {} but lacks a test bead id",
                        row.section_id, row.classification
                    ),
                )?;
                ensure(
                    row.verify_cmd == "-",
                    format!(
                        "{} is {} but has a verify_cmd",
                        row.section_id, row.classification
                    ),
                )?;
            }
            _ => unreachable!("classification was checked above"),
        }
    }

    Ok(())
}
