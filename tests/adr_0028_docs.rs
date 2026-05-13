type TestResult = Result<(), String>;

const ADR_0028: &str =
    include_str!("../docs/adr/0028-counterfactual-lab-and-immutable-revisions.md");
const ADR_INDEX: &str = include_str!("../docs/adr/README.md");

fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
    if haystack.contains(needle) {
        Ok(())
    } else {
        Err(format!("{context}: expected to find `{needle}`"))
    }
}

fn section_after<'a>(document: &'a str, heading: &str) -> Result<&'a str, String> {
    let start = document
        .find(heading)
        .ok_or_else(|| format!("missing section heading `{heading}`"))?;
    let rest = &document[start + heading.len()..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    Ok(&rest[..end])
}

#[test]
fn adr_0028_exists_and_is_indexed() -> TestResult {
    ensure_contains(
        ADR_0028,
        "# ADR 0028: Counterfactual Lab And Immutable Revisions",
        "ADR 0028 title",
    )?;
    ensure_contains(
        ADR_INDEX,
        "0028-counterfactual-lab-and-immutable-revisions.md",
        "ADR index",
    )
}

#[test]
fn adr_0028_has_required_sections() -> TestResult {
    for heading in [
        "## Context",
        "## Decision",
        "## Consequences",
        "## Rejected Alternatives",
        "## Verification",
    ] {
        ensure_contains(ADR_0028, heading, "ADR 0028 required section")?;
    }
    Ok(())
}

#[test]
fn adr_0028_documents_required_design_choices() -> TestResult {
    for phrase in [
        "valid_from",
        "valid_to",
        "FrankenSQLite can index range predicates",
        "manifest pointer after an explicit WAL",
        "single-input swap",
        "N3 causal-credit track",
        "not the runtime causal-credit path",
    ] {
        ensure_contains(ADR_0028, phrase, "ADR 0028 required design choice")?;
    }
    Ok(())
}

#[test]
fn adr_0028_lists_rejected_alternatives_with_reasoning() -> TestResult {
    let alternatives = section_after(ADR_0028, "## Rejected Alternatives")?;
    let alternatives_count = alternatives
        .lines()
        .filter(|line| line.starts_with("- **"))
        .count();
    if alternatives_count < 3 {
        return Err(format!(
            "ADR 0028 must list at least 3 rejected alternatives, found {alternatives_count}"
        ));
    }
    for phrase in [
        "Overwrite memories in place",
        "Use a separate revision table as the public model",
        "Use vector clocks for every revision",
        "Copy the full database and derived indexes for every capture",
        "Allow multi-input counterfactual batches first",
        "Just persist every pack and skip frozen episodes",
    ] {
        ensure_contains(alternatives, phrase, "ADR 0028 rejected alternative")?;
    }
    Ok(())
}
