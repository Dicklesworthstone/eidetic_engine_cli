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

/// `tests/COVERAGE.md` is hand-maintained. Its verdict column is a claim, and
/// until this test existed nothing re-derived any part of it.
///
/// This does NOT check that the cited tests pass -- that would mean running
/// them. It checks the weakest thing that still has teeth: that every test name
/// the matrix cites RESOLVES TO A FUNCTION THAT EXISTS. A row citing a name
/// nothing defines cannot be evidence of anything, whatever its verdict says.
///
/// Measured when this landed: 9 of 65 cited names existed nowhere in the
/// repository, every one of them recorded PASS --
///   FD-01..FD-07  `tokio_is_forbidden`, `async_std_is_forbidden`, ... (7 rows)
///   EC-10         `effect_manifest_tracks_degraded_unavailable_paths_as_non_mutating`
///   EC-16         `effect_manifest_backup_restore_have_side_path_no_delete_contracts`
/// The FD rows and EC-16 were renamed-away citations whose clauses ARE covered,
/// and now cite the real tests. EC-10 had no covering test at all and is
/// recorded UNCOVERED rather than pointed at something that does not cover it.
/// Separately, DH-15 cites a test that exists and is RED (bd-tk7uq); this test
/// deliberately says nothing about that, because it does not run anything.
#[test]
fn coverage_matrix_cites_tests_that_exist() -> TestResult {
    let matrix =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/COVERAGE.md"))
            .map_err(|error| format!("read tests/COVERAGE.md: {error}"))?;

    let mut cited: BTreeSet<String> = BTreeSet::new();
    for line in matrix.lines() {
        let trimmed = line.trim_start();
        // Table rows only. A backticked identifier in prose is a reference, not
        // a coverage claim, and holding prose to this bar would push people
        // toward writing less of it.
        if !trimmed.starts_with('|') {
            continue;
        }
        for cell in trimmed.split('|') {
            let cell = cell.trim();
            let Some(name) = cell.strip_prefix('`').and_then(|c| c.strip_suffix('`')) else {
                continue;
            };
            // Test-function shape only: lower_snake_case, no path separators,
            // no extension. Cells naming scripts or files are not test names.
            if name.len() > 12
                && name.contains('_')
                && !name.contains('.')
                && !name.contains('/')
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            {
                cited.insert(name.to_owned());
            }
        }
    }

    ensure(
        cited.len() > 40,
        format!(
            "expected the matrix's full citation set; parsed {} -- the parser, \
             not the document, is the likely fault",
            cited.len()
        ),
    )?;

    let defined = defined_test_function_names()?;
    ensure(
        defined.len() > 1000,
        format!(
            "expected to have enumerated the repo's test functions; found {} -- \
             the enumerator, not the tree, is the likely fault",
            defined.len()
        ),
    )?;

    let missing: Vec<&str> = cited
        .iter()
        .filter(|name| !defined.contains(name.as_str()))
        .map(String::as_str)
        .collect();

    ensure(
        missing.is_empty(),
        format!(
            "tests/COVERAGE.md cites {} test name(s) that are defined nowhere in \
             src/ or tests/. A row citing a test that does not exist is a verdict \
             with nothing behind it; fix the citation or mark the row UNCOVERED:\n  {}",
            missing.len(),
            missing.join("\n  ")
        ),
    )
}

/// Every `fn <name>(` defined under src/ and tests/, excluding fixture trees.
///
/// Deliberately broader than `#[test] fn`: a matrix row may legitimately cite a
/// helper, and the question this answers is "does this name exist", not "is it
/// a test". Over-collecting makes the check weaker but never wrong, which is
/// the right direction for an enumerator whose failure mode would otherwise be
/// a false accusation.
fn defined_test_function_names() -> Result<BTreeSet<String>, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut names = BTreeSet::new();
    for dir in ["src", "tests"] {
        collect_function_names(&root.join(dir), &mut names)?;
    }
    Ok(names)
}

fn collect_function_names(dir: &Path, names: &mut BTreeSet<String>) -> Result<(), String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if matches!(file_name, "fixtures" | "snapshots" | "golden" | "logs") {
                continue;
            }
            collect_function_names(&path, names)?;
            continue;
        }
        if !file_name.ends_with(".rs") {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in source.lines() {
            let trimmed = line.trim_start();
            // Peel the modifiers in declaration order, so `pub async fn`, `pub
            // const fn` and a bare `fn` all reach the same place.
            let mut rest = trimmed;
            for prefix in [
                "pub(crate) ",
                "pub ",
                "async ",
                "const ",
                "unsafe ",
                "extern ",
            ] {
                rest = rest.strip_prefix(prefix).unwrap_or(rest);
            }
            let Some(after) = rest.strip_prefix("fn ") else {
                continue;
            };
            let name: String = after
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                names.insert(name);
            }
        }
    }
    Ok(())
}
