//! Conformance checks for the deferred research-backlog ADR trio.
//!
//! These ADRs are the documented closeout for N6, N9, and N11. Keep the
//! contract scoped to this trio so historical ADRs with older templates do not
//! become noisy failures.

type TestResult = Result<(), String>;

const ADR_INDEX: &str = include_str!("../docs/adr/README.md");
const ADR_0048: &str = include_str!("../docs/adr/0048-persistent-homology-N6.md");
const ADR_0049: &str = include_str!("../docs/adr/0049-mmap-frankensearch-N9.md");
const ADR_0050: &str = include_str!("../docs/adr/0050-active-learning-curate-N11.md");

struct DeferredAdr {
    number: &'static str,
    path: &'static str,
    bead: &'static str,
    document: &'static str,
}

const DEFERRED_ADRS: &[DeferredAdr] = &[
    DeferredAdr {
        number: "0048",
        path: "0048-persistent-homology-N6.md",
        bead: "bd-17c65.14.6 (N6)",
        document: ADR_0048,
    },
    DeferredAdr {
        number: "0049",
        path: "0049-mmap-frankensearch-N9.md",
        bead: "bd-17c65.14.9 (N9)",
        document: ADR_0049,
    },
    DeferredAdr {
        number: "0050",
        path: "0050-active-learning-curate-N11.md",
        bead: "bd-17c65.14.11 (N11)",
        document: ADR_0050,
    },
];

fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
    if haystack.contains(needle) {
        Ok(())
    } else {
        Err(format!("{context}: expected to find `{needle}`"))
    }
}

fn metadata_value<'a>(document: &'a str, key: &str) -> Result<&'a str, String> {
    document
        .lines()
        .find_map(|line| line.strip_prefix(key).map(str::trim))
        .ok_or_else(|| format!("missing metadata line `{key}`"))
}

fn section_after<'a>(document: &'a str, heading: &str) -> Result<&'a str, String> {
    let start = document
        .find(heading)
        .ok_or_else(|| format!("missing section heading `{heading}`"))?;
    let rest = &document[start + heading.len()..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    Ok(rest[..end].trim())
}

fn ensure_date_shape(date: &str, context: &str) -> TestResult {
    let bytes = date.as_bytes();
    let valid = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[8..].iter().all(u8::is_ascii_digit);

    if valid {
        Ok(())
    } else {
        Err(format!(
            "{context}: expected YYYY-MM-DD date, found `{date}`"
        ))
    }
}

fn is_reopen_criterion(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("- ")
        || trimmed.starts_with("1.")
        || trimmed.starts_with("2.")
        || trimmed.starts_with("3.")
        || trimmed.starts_with("4.")
        || trimmed.starts_with("5.")
        || trimmed.starts_with("6.")
        || trimmed.starts_with("7.")
        || trimmed.starts_with("8.")
        || trimmed.starts_with("9.")
}

#[test]
fn deferred_research_backlog_adrs_are_indexed() -> TestResult {
    for adr in DEFERRED_ADRS {
        ensure_contains(
            adr.document,
            &format!("# ADR {}", adr.number),
            &format!("ADR {} title", adr.number),
        )?;
        ensure_contains(
            ADR_INDEX,
            adr.path,
            &format!("ADR {} index entry", adr.number),
        )?;
    }
    Ok(())
}

#[test]
fn deferred_research_backlog_adrs_keep_metadata_contract() -> TestResult {
    for adr in DEFERRED_ADRS {
        let context = format!("ADR {}", adr.number);
        if metadata_value(adr.document, "Status:")? != "Deferred (research backlog)" {
            return Err(format!(
                "{context}: expected `Status: Deferred (research backlog)`"
            ));
        }
        ensure_date_shape(metadata_value(adr.document, "Date:")?, &context)?;
        if metadata_value(adr.document, "Bead:")? != adr.bead {
            return Err(format!("{context}: expected `Bead: {}`", adr.bead));
        }
    }
    Ok(())
}

#[test]
fn deferred_research_backlog_adrs_keep_required_sections() -> TestResult {
    for adr in DEFERRED_ADRS {
        for heading in [
            "## Context",
            "## Decision",
            "## Consequences",
            "## Rejected Alternatives",
            "## Verification",
            "### Re-open Criteria",
        ] {
            ensure_contains(
                adr.document,
                heading,
                &format!("ADR {} required heading", adr.number),
            )?;
        }
    }
    Ok(())
}

#[test]
fn deferred_research_backlog_adrs_have_actionable_reopen_criteria() -> TestResult {
    for adr in DEFERRED_ADRS {
        let criteria = section_after(adr.document, "### Re-open Criteria")?;
        let criterion_count = criteria
            .lines()
            .filter(|line| is_reopen_criterion(line))
            .count();
        if criterion_count < 3 {
            return Err(format!(
                "ADR {} must list at least 3 reopen criteria, found {criterion_count}",
                adr.number
            ));
        }
    }
    Ok(())
}
