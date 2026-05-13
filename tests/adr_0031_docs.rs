//! Verification hooks for ADR 0031 (Submodular Pack Assembly — Audit, Don't Certify).
//!
//! Closure-lint treats `bd-17c65.14.5.1` as closed only when these tests pass:
//! the ADR file exists, is indexed under `docs/adr/README.md`, carries every
//! required section, documents each design choice the ADR commits to, and
//! enumerates the rejected alternatives with explicit rejection rationale.

type TestResult = Result<(), String>;

const ADR_0031: &str = include_str!("../docs/adr/0031-submodular-pack-or-rename.md");
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
fn adr_0031_exists_and_is_indexed() -> TestResult {
    ensure_contains(
        ADR_0031,
        "# ADR 0031: Submodular Pack Assembly",
        "ADR 0031 title",
    )?;
    ensure_contains(ADR_INDEX, "0031-submodular-pack-or-rename.md", "ADR index")
}

#[test]
fn adr_0031_has_required_sections() -> TestResult {
    for heading in [
        "## Context",
        "## Decision",
        "## Consequences",
        "## Rejected Alternatives",
        "## Verification",
    ] {
        ensure_contains(ADR_0031, heading, "ADR 0031 required section")?;
    }
    Ok(())
}

#[test]
fn adr_0031_documents_required_design_choices() -> TestResult {
    for phrase in [
        // The chosen resolution and the renamed field name.
        "Resolution B (RENAME)",
        "selectionAudit",
        // The descriptive booleans are kept (not deleted).
        "*descriptive* properties of the algorithm",
        // The new identifier fields that replace the prose guarantee strings.
        "algorithmId",
        "algorithmDescription",
        // The guarantee strings are dropped (the load-bearing decision).
        "`guarantee` and `guarantee_status` strings are dropped",
        // The envelope schema version bump that signals the rename.
        "schema version increments",
        // The integration with the D-series envelope schema bump.
        "bd-17c65.4.7",
    ] {
        ensure_contains(ADR_0031, phrase, "ADR 0031 required design choice")?;
    }
    Ok(())
}

#[test]
fn adr_0031_lists_rejected_alternatives_with_reasoning() -> TestResult {
    let alternatives = section_after(ADR_0031, "## Rejected Alternatives")?;
    let alternatives_count = alternatives
        .lines()
        .filter(|line| line.starts_with("### "))
        .count();
    if alternatives_count < 3 {
        return Err(format!(
            "ADR 0031 must list at least 3 rejected alternatives, found {alternatives_count}"
        ));
    }
    for phrase in [
        "Resolution A — Prove and keep the certificate",
        "MMR is not submodular in general",
        "Resolution C — Keep the field name, drop only the guarantee strings",
        "Resolution D — Per-profile field shape",
    ] {
        ensure_contains(alternatives, phrase, "ADR 0031 rejected alternative")?;
    }
    Ok(())
}
