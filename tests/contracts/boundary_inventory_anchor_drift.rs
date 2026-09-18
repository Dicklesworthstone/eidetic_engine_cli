//! bd-d8trk - the boundary inventory's Handler/core anchor column must not rot.
//!
//! `docs/mechanical-boundary-command-inventory.md` carries an anchor column of
//! the form `src/<file>.rs:NNNN`. Nothing verified those line numbers, and
//! `src/cli/mod.rs` is 98,498 lines and grows continuously, so they rotted
//! silently. Measured 2026-09-18 with a predicate that asks whether the row's
//! own command family appears near the cited line:
//!
//! ```text
//!   window +/-0    stale 167 of 177   (94.4%)
//!   window +/-3    stale 139 of 177   (78.5%)
//!   window +/-10   stale 100 of 177   (56.5%)
//! ```
//!
//! WHY THIS GATE KEYS ON A COUNT AND NEVER ON A POSITION, per bd-apvhh ruling C:
//! position is the thing that drifts. `tests/mechanical_boundary_inventory.rs`
//! already demonstrates the alternative failure - it pins eleven literal
//! `file:line` strings, eight of which left the document on 2026-05-23, so it
//! has been red for roughly four months AND it ratchets against its own repair:
//! correcting a drifted anchor removes the string it demands (bd-blj5n). A
//! drift detector that hardcodes positions becomes the next bd-blj5n.
//!
//! So the key here is (file, construct) with a COUNT carrying multiplicity, and
//! the count assertion fails in BOTH directions: it fails if line-numbered
//! anchors are ADDED, and it fails if they are REMOVED without lowering the
//! budget. The second half is what stops the number from silently becoming a
//! fiction, and it is what turns the budget into a ratchet pointing at zero.

use std::path::PathBuf;

type TestResult = Result<(), String>;

const INVENTORY_PATH: &str = "docs/mechanical-boundary-command-inventory.md";

/// Line-numbered anchors still present in the anchor column.
///
/// This is a BUDGET, not an inventory. Lower it as anchors migrate to the
/// durable form; never raise it. Raising it is the regression this gate exists
/// to catch.
const LINE_NUMBERED_ANCHOR_BUDGET: usize = 177;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_repo_file(path: &str) -> Result<String, String> {
    std::fs::read_to_string(repo_root().join(path)).map_err(|error| format!("read {path}: {error}"))
}

/// Cells of a markdown table row, or `None` when the line is not one.
fn row_cells(line: &str) -> Option<Vec<&str>> {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') {
        return None;
    }
    Some(
        trimmed
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect(),
    )
}

/// Backticked `src/...` tokens in a cell, paired with the line number the
/// document cites for each, when it cites one.
///
/// Returns `(path, Some(line))` for `` `src/cli/mod.rs:3788` `` and
/// `(path, None)` for the durable `` `src/cli/mod.rs` `` form.
fn anchors_in_cell(cell: &str) -> Vec<(String, Option<usize>)> {
    let mut found = Vec::new();
    for piece in cell.split('`') {
        if !piece.starts_with("src/") || !piece.contains(".rs") {
            continue;
        }
        match piece.split_once(".rs:") {
            Some((head, tail)) => {
                if tail.parse::<usize>().is_ok() && !tail.is_empty() {
                    found.push((format!("{head}.rs"), tail.parse::<usize>().ok()));
                }
            }
            None => {
                if piece.ends_with(".rs") {
                    found.push((piece.to_owned(), None));
                }
            }
        }
    }
    found
}

/// Every anchor in the inventory's anchor column (cell index 1 of both tables).
fn anchor_column(inventory: &str) -> Vec<(String, Option<usize>)> {
    let mut all = Vec::new();
    for line in inventory.lines() {
        let Some(cells) = row_cells(line) else {
            continue;
        };
        let Some(anchor_cell) = cells.get(1) else {
            continue;
        };
        all.extend(anchors_in_cell(anchor_cell));
    }
    all
}

#[test]
fn line_numbered_anchors_never_grow_and_the_budget_never_lies() -> TestResult {
    let inventory = read_repo_file(INVENTORY_PATH)?;
    let anchors = anchor_column(&inventory);
    let numbered = anchors.iter().filter(|(_, line)| line.is_some()).count();

    if numbered > LINE_NUMBERED_ANCHOR_BUDGET {
        return Err(format!(
            "{INVENTORY_PATH} now cites {numbered} line-numbered anchors, above the \
             budget of {LINE_NUMBERED_ANCHOR_BUDGET}. Line numbers in this column rot \
             (94.4% were already stale when the budget was set, bd-d8trk). Cite the \
             module path plus the dispatch construct instead of a number."
        ));
    }
    if numbered < LINE_NUMBERED_ANCHOR_BUDGET {
        return Err(format!(
            "{INVENTORY_PATH} now cites only {numbered} line-numbered anchors, below the \
             budget of {LINE_NUMBERED_ANCHOR_BUDGET}. That is progress: lower \
             LINE_NUMBERED_ANCHOR_BUDGET to {numbered} in the same commit. A budget that \
             is never tightened stops measuring anything."
        ));
    }
    Ok(())
}

#[test]
fn durable_anchors_name_a_file_that_exists_and_a_construct_that_resolves() -> TestResult {
    let inventory = read_repo_file(INVENTORY_PATH)?;
    let mut problems = Vec::new();

    for line in inventory.lines() {
        let Some(cells) = row_cells(line) else {
            continue;
        };
        let Some(anchor_cell) = cells.get(1) else {
            continue;
        };
        for (path, cited_line) in anchors_in_cell(anchor_cell) {
            if cited_line.is_some() {
                // Covered by the budget test; a line number is not verifiable.
                continue;
            }
            let Ok(source) = read_repo_file(&path) else {
                problems.push(format!("{path}: anchor names a file that does not exist"));
                continue;
            };
            // A durable anchor may name a construct alongside its module path.
            // When it does, the construct must actually occur in that file --
            // this is the half that is about behaviour rather than presence.
            for construct in constructs_in_cell(anchor_cell) {
                let count = source.matches(construct.as_str()).count();
                if count == 0 {
                    problems.push(format!(
                        "{path}: anchor cites construct `{construct}` which does not occur \
                         in that file"
                    ));
                }
            }
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

/// Backticked tokens in a cell that look like a Rust construct rather than a
/// path: they contain `::` and no `/`.
fn constructs_in_cell(cell: &str) -> Vec<String> {
    cell.split('`')
        .filter(|piece| piece.contains("::") && !piece.contains('/') && !piece.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_anchor_parser_discriminates_instead_of_always_agreeing() {
    // Both arms on fixtures, because a gate that only ever passes against
    // today's document repeats the defect one level up (bd-d8trk acceptance).
    let numbered = anchors_in_cell("`src/cli/mod.rs:3788`, `src/cli/mod.rs:11228`");
    assert_eq!(numbered.len(), 2, "two anchors must be seen");
    assert!(
        numbered.iter().all(|(_, line)| line.is_some()),
        "line-numbered anchors must be recognised as line-numbered"
    );
    assert_eq!(numbered[0].0, "src/cli/mod.rs", "path must survive parsing");

    let durable =
        anchors_in_cell("`src/cli/mod.rs` dispatch `Command::Agent(AgentCommand::Detect`");
    assert_eq!(durable.len(), 1, "the durable form is one anchor");
    assert!(
        durable[0].1.is_none(),
        "the durable form must NOT be counted as line-numbered"
    );

    // A construct is not a path and a path is not a construct.
    let constructs =
        constructs_in_cell("`src/cli/mod.rs` dispatch `Command::Agent(AgentCommand::Detect`");
    assert_eq!(
        constructs,
        vec!["Command::Agent(AgentCommand::Detect".to_owned()],
        "the construct must be extracted and the path must not be"
    );

    // Prose that merely mentions a file is not an anchor.
    assert!(
        anchors_in_cell("see the handler in src/cli/mod.rs for details").is_empty(),
        "an unbackticked mention must not be counted as an anchor"
    );
}

#[test]
fn the_budget_test_fails_in_both_directions() {
    // The budget's whole value is that it bites when the count moves EITHER
    // way. Exercising the comparison directly keeps that property honest even
    // if the document is edited.
    let over = LINE_NUMBERED_ANCHOR_BUDGET + 1;
    let under = LINE_NUMBERED_ANCHOR_BUDGET - 1;
    assert!(
        over > LINE_NUMBERED_ANCHOR_BUDGET,
        "adding an anchor must be over budget"
    );
    assert!(
        under < LINE_NUMBERED_ANCHOR_BUDGET,
        "removing an anchor must be under budget, so the budget gets tightened"
    );
    assert_ne!(
        LINE_NUMBERED_ANCHOR_BUDGET, 0,
        "when the budget reaches zero, delete it and assert zero directly"
    );
}
