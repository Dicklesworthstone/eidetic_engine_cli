//! Verification hooks for ADR 0032 (Bayesian (alpha, beta) Posteriors on Memories).
//!
//! Closure-lint treats `bd-17c65.14.7.1` as closed only when these tests pass.

type TestResult = Result<(), String>;

const ADR_0032: &str = include_str!("../docs/adr/0032-bayesian-memory-posteriors.md");
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
fn adr_0032_exists_and_is_indexed() -> TestResult {
    ensure_contains(
        ADR_0032,
        "# ADR 0032: Bayesian (alpha, beta) Posteriors on Memories",
        "ADR 0032 title",
    )?;
    ensure_contains(ADR_INDEX, "0032-bayesian-memory-posteriors.md", "ADR index")
}

#[test]
fn adr_0032_has_required_sections() -> TestResult {
    for heading in [
        "## Context",
        "## Decision",
        "## Consequences",
        "## Rejected Alternatives",
        "## Verification",
    ] {
        ensure_contains(ADR_0032, heading, "ADR 0032 required section")?;
    }
    Ok(())
}

#[test]
fn adr_0032_documents_required_design_choices() -> TestResult {
    for phrase in [
        // Posterior model + new column names.
        "Beta-Bernoulli posterior",
        "bayes_alpha",
        "bayes_beta",
        // Prior choice with rationale.
        "Jeffreys prior",
        "Beta(0.5, 0.5)",
        // Update rule with harmful asymmetry.
        "harmful_weight",
        // Credible-interval driven transitions (the amendment to ADR 0009).
        "credible-interval boundary crossings",
        "hysteresis",
        // Sample-size gate at the critical transition.
        "alpha + beta >= 6",
        // The backfill modes.
        "--bayes-backfill-from-utility",
        "--bayes-backfill-from-feedback-events",
        // Reversibility of the schema migration.
        "Schema migration reversibility",
        // Cross-reference to ADR 0009 amendment.
        "amends ADR 0009",
    ] {
        ensure_contains(ADR_0032, phrase, "ADR 0032 required design choice")?;
    }
    Ok(())
}

#[test]
fn adr_0032_lists_rejected_alternatives_with_reasoning() -> TestResult {
    let alternatives = section_after(ADR_0032, "## Rejected Alternatives")?;
    let alternatives_count = alternatives
        .lines()
        .filter(|line| line.starts_with("### "))
        .count();
    if alternatives_count < 4 {
        return Err(format!(
            "ADR 0032 must list at least 4 rejected alternatives, found {alternatives_count}"
        ));
    }
    for phrase in [
        "Keep scalar deltas; track a sample count separately",
        "Gaussian posterior",
        "Dirichlet over a richer outcome space",
        "Point-estimate thresholds with elapsed-time hysteresis",
    ] {
        ensure_contains(alternatives, phrase, "ADR 0032 rejected alternative")?;
    }
    Ok(())
}
