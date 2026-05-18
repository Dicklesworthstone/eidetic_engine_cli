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

use std::path::{Path, PathBuf};

type TestResult = Result<(), String>;

fn doc_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/redaction_levels.md")
}

fn read_doc() -> Result<String, String> {
    std::fs::read_to_string(doc_path()).map_err(|e| format!("read docs/redaction_levels.md: {e}"))
}

fn read_workspace_file(path: impl AsRef<Path>) -> Result<String, String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path);
    std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))
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

/// The 4 redaction-bearing surfaces in the per-surface defaults table.
/// `ee why` is intentionally out of this list: it has no override.
const SURFACES_WITH_DEFAULTS: &[(&str, &str, &str)] = &[
    ("ee export", "standard", "current `--redaction <level>`"),
    (
        "ee handoff create",
        "standard",
        "current `--redaction <level>`",
    ),
    (
        "ee context --json",
        "minimal",
        "current `--redaction <level>`",
    ),
    (
        "ee support bundle",
        "paranoid",
        "current `--redaction <level>`",
    ),
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

    for (surface, default_level, override_status) in SURFACES_WITH_DEFAULTS {
        let surface_backticked = format!("`{surface}`");
        let level_backticked = format!("`{default_level}`");
        let matching_row = doc.lines().find(|line| {
            line.contains(&surface_backticked)
                && line.contains(&level_backticked)
                && line.contains(override_status)
        });
        ensure(
            matching_row.is_some(),
            format!(
                "docs/redaction_levels.md missing canonical table row for `{surface}` with default `{default_level}` and override status `{override_status}`"
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
        "`none`/`--include-raw` keeps collected diagnostics raw",
        "`minimal` applies only the secret detector",
        "`standard`/`strict`/`paranoid`",
        "Per-workspace defaults live in `.ee/config.toml`",
        "CLI flag → workspace config → built-in default",
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
        "tests/contracts/backup_import_roundtrip.rs",
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
fn doc_round_trip_test_reference_points_to_registered_contract() -> TestResult {
    let doc = read_doc()?;
    ensure(
        !doc.contains("tests/redaction_round_trip_unit.rs"),
        "docs/redaction_levels.md must not reference stale nonexistent `tests/redaction_round_trip_unit.rs`",
    )?;

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let roundtrip_path = PathBuf::from("tests/contracts/backup_import_roundtrip.rs");
    ensure(
        manifest_dir.join(&roundtrip_path).is_file(),
        format!(
            "docs/redaction_levels.md references missing round-trip contract `{}`",
            roundtrip_path.display()
        ),
    )?;

    let contracts_rs = read_workspace_file("tests/contracts.rs")?;
    ensure(
        contracts_rs.contains("#[path = \"contracts/backup_import_roundtrip.rs\"]"),
        "tests/contracts.rs must register `tests/contracts/backup_import_roundtrip.rs`",
    )?;

    let roundtrip_test = read_workspace_file(&roundtrip_path)?;
    for required_token in [
        "backup_export_import_roundtrip_imports_all_redaction_levels",
        "RedactionLevel::all()",
        "run_roundtrip(*level)",
    ] {
        ensure(
            roundtrip_test.contains(required_token),
            format!(
                "{} must contain `{required_token}` so the doc's all-level round-trip claim stays grounded",
                roundtrip_path.display()
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
        "redaction_uncertain",
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
    let schema_text = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/schemas/test_event_v1.json"),
    )
    .map_err(|e| format!("read docs/schemas/test_event_v1.json: {e}"))?;
    ensure(
        doc.contains("\"kind\": \"redaction_apply\"")
            || doc.contains("kind: \"redaction_apply\"")
            || doc.contains("`redaction_apply`"),
        "docs/redaction_levels.md must declare the canonical test-event `kind: \"redaction_apply\"`",
    )?;
    for required_schema_token in [
        "\"redaction_apply\"",
        "\"level\"",
        "\"surface\"",
        "\"fields_redacted_count\"",
        "\"patterns_matched\"",
        "\"tokens_truncated\"",
        "\"content_hash_original\"",
        "\"audit_row_id\"",
    ] {
        ensure(
            schema_text.contains(required_schema_token),
            format!(
                "docs/schemas/test_event_v1.json must pin redaction_apply token `{required_schema_token}`"
            ),
        )?;
    }
    Ok(())
}

#[test]
fn doc_test_event_example_matches_schema_required_fields() -> TestResult {
    let doc = read_doc()?;
    for required_field in [
        "\"level\"",
        "\"surface\"",
        "\"fields_redacted_count\"",
        "\"patterns_matched\"",
        "\"tokens_truncated\"",
        "\"content_hash_original\"",
        "\"audit_row_id\"",
    ] {
        ensure(
            doc.contains(required_field),
            format!(
                "docs/redaction_levels.md redaction_apply example missing schema-required field `{required_field}`"
            ),
        )?;
    }
    Ok(())
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

    let claims_live_env_override = doc.contains("`EE_REDACTION_")
        && !doc.contains("No `EE_REDACTION_*` redaction-level override is currently registered");
    if claims_live_env_override {
        ensure(
            env_registry_doc.contains("`EE_REDACTION_"),
            "docs/redaction_levels.md claims an `EE_REDACTION_*` override, but docs/env_vars.md has no registered redaction env override",
        )?;
    }

    Ok(())
}

#[test]
fn current_context_redaction_claim_matches_cli_and_pack_wiring() -> TestResult {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let doc = read_doc()?;
    let cli = std::fs::read_to_string(manifest_dir.join("src/cli/mod.rs"))
        .map_err(|e| format!("read src/cli/mod.rs: {e}"))?;
    let context = std::fs::read_to_string(manifest_dir.join("src/core/context.rs"))
        .map_err(|e| format!("read src/core/context.rs: {e}"))?;

    ensure(
        doc.lines().any(|line| {
            line.contains("`ee context --json`")
                && line.contains("`minimal`")
                && line.contains("current `--redaction <level>`")
        }),
        "docs/redaction_levels.md must mark ee context --json as a current minimal-redaction surface",
    )?;
    ensure(
        cli.contains("pub redaction: Option<BackupRedaction>"),
        "ContextArgs must expose a parsed redaction field",
    )?;
    ensure(
        cli.contains("RedactionDefaultSurface::ContextJson")
            && cli.contains("RedactionLevel::Minimal"),
        "ee context must resolve the documented minimal built-in default through workspace config",
    )?;
    ensure(
        cli.contains("redaction_level,"),
        "handle_context must pass the effective redaction level into ContextPackOptions",
    )?;
    ensure(
        context.contains("pub redaction_level: crate::models::RedactionLevel"),
        "ContextPackOptions must carry the requested redaction level",
    )?;
    ensure(
        context.contains("redaction_level: options.redaction_level"),
        "context pack assembly must use ContextPackOptions.redaction_level instead of a hardcoded level",
    )
}

#[test]
fn current_handoff_redaction_claim_matches_cli_wiring() -> TestResult {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let doc = read_doc()?;
    let cli = std::fs::read_to_string(manifest_dir.join("src/cli/mod.rs"))
        .map_err(|e| format!("read src/cli/mod.rs: {e}"))?;

    ensure(
        doc.lines().any(|line| {
            line.contains("`ee handoff create`")
                && line.contains("`standard`")
                && line.contains("current `--redaction <level>`")
        }),
        "docs/redaction_levels.md must mark ee handoff create as a current standard-redaction surface",
    )?;
    ensure(
        cli.contains("pub struct HandoffCreateArgs"),
        "src/cli/mod.rs must define HandoffCreateArgs",
    )?;
    ensure(
        cli.contains("RedactionDefaultSurface::HandoffCreate")
            && cli.contains("RedactionLevel::Standard"),
        "ee handoff create must resolve the documented standard built-in default through workspace config",
    )?;
    ensure(
        cli.contains("redaction_level,"),
        "handoff create must pass the effective redaction level into HandoffCreateOptions",
    )
}

#[test]
fn current_support_bundle_redaction_claim_matches_cli_and_core_wiring() -> TestResult {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let doc = read_doc()?;
    let cli = std::fs::read_to_string(manifest_dir.join("src/cli/mod.rs"))
        .map_err(|e| format!("read src/cli/mod.rs: {e}"))?;
    let support_bundle = std::fs::read_to_string(manifest_dir.join("src/core/support_bundle.rs"))
        .map_err(|e| format!("read src/core/support_bundle.rs: {e}"))?;

    ensure(
        doc.lines().any(|line| {
            line.contains("`ee support bundle`")
                && line.contains("`paranoid`")
                && line.contains("current `--redaction <level>`")
        }),
        "docs/redaction_levels.md must mark ee support bundle as a current paranoid-redaction surface",
    )?;
    ensure(
        cli.contains("pub struct SupportBundleArgs"),
        "src/cli/mod.rs must define SupportBundleArgs",
    )?;
    ensure(
        cli.contains("RedactionDefaultSurface::SupportBundle")
            && cli.contains("RedactionLevel::Paranoid"),
        "ee support bundle must resolve the documented paranoid built-in default through workspace config",
    )?;
    ensure(
        cli.contains("redaction_level,"),
        "support bundle must pass the effective redaction level into BundleOptions",
    )?;
    ensure(
        support_bundle.contains("pub redaction_level: RedactionLevel"),
        "BundleOptions must carry the requested redaction level",
    )?;
    ensure(
        support_bundle.contains("pub const fn effective_redaction_level(&self) -> RedactionLevel"),
        "BundleOptions must compute include_raw/raw effective redaction behavior",
    )?;
    ensure(
        support_bundle.contains("redact_support_bundle_content(content, redaction_level)"),
        "support bundle creation must apply the requested redaction level during final bundle writes",
    )
}

#[test]
fn workspace_redaction_defaults_claim_matches_config_parser() -> TestResult {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let doc = read_doc()?;
    let config_file = std::fs::read_to_string(manifest_dir.join("src/config/file.rs"))
        .map_err(|e| format!("read src/config/file.rs: {e}"))?;
    let config_mod = std::fs::read_to_string(manifest_dir.join("src/config/mod.rs"))
        .map_err(|e| format!("read src/config/mod.rs: {e}"))?;
    let cli = std::fs::read_to_string(manifest_dir.join("src/cli/mod.rs"))
        .map_err(|e| format!("read src/cli/mod.rs: {e}"))?;

    ensure(
        doc.contains("[redaction.defaults]")
            && doc.contains("export         = \"standard\"")
            && doc.contains("handoff_create = \"standard\"")
            && doc.contains("context_json   = \"minimal\"")
            && doc.contains("support_bundle = \"paranoid\""),
        "docs/redaction_levels.md must document the live redaction.defaults config table",
    )?;
    ensure(
        config_file.contains("pub struct RedactionDefaultsConfig")
            && config_file.contains("export: optional_redaction_level_path")
            && config_file.contains("handoff_create: optional_redaction_level_path")
            && config_file.contains("context_json: optional_redaction_level_path")
            && config_file.contains("support_bundle: optional_redaction_level_path"),
        "ConfigFile must parse redaction.defaults surface defaults",
    )?;
    ensure(
        config_mod.contains("pub enum RedactionDefaultSurface")
            && config_mod.contains("pub fn workspace_redaction_default"),
        "config module must expose workspace redaction default resolution",
    )?;
    ensure(
        cli.contains("fn effective_redaction_level")
            && cli.contains("workspace_redaction_default(workspace_path, surface, built_in)"),
        "CLI handlers must share CLI-over-config redaction default resolution",
    )
}

#[test]
fn doc_declares_live_source_aware_response_metadata() -> TestResult {
    let doc = read_doc()?;
    let cli =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/cli/mod.rs"))
            .map_err(|e| format!("read src/cli/mod.rs: {e}"))?;

    for required_phrase in [
        "Response metadata status",
        "\"level_applied\"",
        "\"level_source\"",
        "\"fields_redacted\"",
        "\"patterns_matched\"",
        "`cli`, `workspace_config`, or `built_in_default`",
        "surfaces that cannot yet produce field-level detail emit an empty array",
        "callers should prefer the `redaction.level_applied` and",
    ] {
        ensure(
            doc.contains(required_phrase),
            format!(
                "docs/redaction_levels.md missing live response-metadata marker: `{required_phrase}`"
            ),
        )?;
    }

    for required_implementation in [
        "enum RedactionLevelSource",
        "\"cli\"",
        "\"workspace_config\"",
        "\"built_in_default\"",
        "fn redaction_metadata_json",
        "\"level_applied\"",
        "\"level_source\"",
        "\"fields_redacted\"",
        "\"patterns_matched\"",
        "json_with_redaction_metadata",
    ] {
        ensure(
            cli.contains(required_implementation),
            format!(
                "src/cli/mod.rs missing redaction metadata implementation marker: `{required_implementation}`"
            ),
        )?;
    }

    Ok(())
}
