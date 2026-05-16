//! Doc-consistency gate for `docs/redaction_levels.md` (K6 / bd-17c65.11.6).
//!
//! Asserts the redaction-level spec doc is well-formed and pins the
//! canonical level enumeration. When the level vocabulary or per-surface
//! defaults change, this gate fires until the doc is updated. The full
//! level × surface implementation matrix lands in a sibling sub-bead;
//! this test gates the docs contract independently so the spec doesn't
//! drift away from the eventual implementation.
//!
//! Bead: bd-17c65.11.6 (K6).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

type TestResult = Result<(), String>;

fn doc_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/redaction_levels.md")
}

fn read_doc() -> Result<String, String> {
    std::fs::read_to_string(doc_path()).map_err(|e| format!("read docs/redaction_levels.md: {e}"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

/// The canonical 5 levels in increasing-redaction order. Adding or
/// removing a level requires updating BOTH this constant AND the doc.
const CANONICAL_LEVELS: &[&str] = &["none", "minimal", "standard", "strict", "paranoid"];

/// The 4 surfaces with documented `--redaction` defaults (per the doc's
/// per-surface defaults table). `ee why` is intentionally OUT of this
/// list — it has no override.
const SURFACES_WITH_DEFAULTS: &[(&str, &str)] = &[
    ("ee export", "standard"),
    ("ee handoff create", "standard"),
    ("ee context --json", "minimal"),
    ("ee support bundle", "paranoid"),
];

#[test]
fn doc_declares_five_canonical_levels_in_order() -> TestResult {
    let doc = read_doc()?;

    // The ordering claim appears as `none < minimal < standard < strict < paranoid`.
    let canonical_ordering = "none < minimal < standard < strict < paranoid";
    ensure(
        doc.contains(canonical_ordering),
        format!(
            "docs/redaction_levels.md must contain the canonical ordering line `{canonical_ordering}`"
        ),
    )?;

    // Each level appears at least once as a backticked token.
    for level in CANONICAL_LEVELS {
        let backticked = format!("`{level}`");
        ensure(
            doc.contains(&backticked),
            format!("docs/redaction_levels.md is missing canonical level token `{level}`"),
        )?;
    }

    Ok(())
}

#[test]
fn doc_declares_per_surface_defaults_in_canonical_table() -> TestResult {
    let doc = read_doc()?;

    for (surface, default_level) in SURFACES_WITH_DEFAULTS {
        let surface_backticked = format!("`{surface}`");
        let level_backticked = format!("`{default_level}`");
        ensure(
            doc.contains(&surface_backticked),
            format!("docs/redaction_levels.md missing surface entry `{surface}`"),
        )?;
        ensure(
            doc.contains(&level_backticked),
            format!(
                "docs/redaction_levels.md missing default-level marker `{default_level}` (expected as the default for `{surface}`)"
            ),
        )?;
    }

    Ok(())
}

#[test]
fn doc_distinguishes_current_and_planned_redaction_flags() -> TestResult {
    let doc = read_doc()?;

    for required_phrase in [
        "current `--redaction <level>`",
        "planned `--redaction <level>`",
        "Handoff, context, and support-bundle level flags are part of",
        "not all live yet",
    ] {
        ensure(
            doc.contains(required_phrase),
            format!(
                "docs/redaction_levels.md missing current/planned flag language: `{required_phrase}`"
            ),
        )?;
    }

    Ok(())
}

#[test]
fn doc_declares_round_trip_symmetry_property() -> TestResult {
    let doc = read_doc()?;
    for required_phrase in [
        "Round-trip symmetry property",
        "redaction_markers",
        "Non-redacted fields are byte-identical",
        "Audit chain shows",
    ] {
        ensure(
            doc.contains(required_phrase),
            format!(
                "docs/redaction_levels.md missing canonical round-trip language: `{required_phrase}`"
            ),
        )?;
    }
    Ok(())
}

#[test]
fn doc_cross_references_j6_failure_modes() -> TestResult {
    let doc = read_doc()?;
    for required_fixture in [
        "redaction_pattern_matched",
        "redaction_level_invalid",
        "redaction_round_trip_marker_preserved",
    ] {
        ensure(
            doc.contains(required_fixture),
            format!(
                "docs/redaction_levels.md missing J6 fixture cross-reference: `{required_fixture}`"
            ),
        )?;
    }
    Ok(())
}

#[test]
fn doc_cross_references_test_event_kind() -> TestResult {
    let doc = read_doc()?;
    ensure(
        doc.contains("\"kind\": \"redaction_apply\"")
            || doc.contains("kind: \"redaction_apply\"")
            || doc.contains("`redaction_apply`"),
        "docs/redaction_levels.md must declare the canonical test-event `kind: \"redaction_apply\"`",
    )
}

#[test]
fn doc_references_existing_source_files() -> TestResult {
    let doc = read_doc()?;
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut checked = Vec::new();

    for token in doc.split(|ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                '`' | '(' | ')' | '[' | ']' | '<' | '>' | ',' | ';' | ':' | '"' | '\''
            )
    }) {
        let Some(relative) = token.strip_prefix("src/") else {
            continue;
        };
        let path = relative.split("::").next().unwrap_or(relative);
        if !path.ends_with(".rs") {
            continue;
        }
        let relative_path = PathBuf::from("src").join(path);
        if checked.contains(&relative_path) {
            continue;
        }
        ensure(
            manifest_dir.join(&relative_path).is_file(),
            format!(
                "docs/redaction_levels.md references nonexistent source file `{}`",
                relative_path.display()
            ),
        )?;
        checked.push(relative_path);
    }

    ensure(
        !checked.is_empty(),
        "docs/redaction_levels.md should cross-reference at least one source file",
    )
}

#[test]
fn doc_does_not_claim_unregistered_redaction_env_override() -> TestResult {
    let doc = read_doc()?;
    let env_registry_doc =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/env_vars.md"))
            .map_err(|e| format!("read docs/env_vars.md: {e}"))?;

    if doc.contains("`EE_REDACTION_") {
        ensure(
            env_registry_doc.contains("`EE_REDACTION_"),
            "docs/redaction_levels.md claims an `EE_REDACTION_*` override, but docs/env_vars.md has no registered redaction env override",
        )?;
    }

    Ok(())
}
