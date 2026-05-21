//! bd-2xuu7.6: symbol-graph machine-facing artifacts.

use std::path::PathBuf;
use std::process::Command;

use ee::models::{
    SymbolEvidenceLinkDegradationCode, SymbolGraphDegradationCode, symbol::SYMBOL_INDEX_STALE_CODE,
};
use serde_json::Value;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_json(relative: &str) -> Result<Value, String> {
    let path = repo_root().join(relative);
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn ensure(condition: bool, context: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(context.into())
    }
}

fn schema_enum(schema: &Value, pointer: &str) -> Result<Vec<String>, String> {
    schema
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{pointer}: missing enum array"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("{pointer}: enum value {value:?} is not a string"))
        })
        .collect()
}

#[test]
fn symbol_graph_degraded_fixture_catalog_covers_required_modes() -> TestResult {
    let required = [
        (
            "tests/fixtures/failure_modes/source_unparsable.json",
            SymbolGraphDegradationCode::SourceUnparsable.as_str(),
            "symbol extraction unavailable",
        ),
        (
            "tests/fixtures/failure_modes/symbol_index_stale.json",
            SYMBOL_INDEX_STALE_CODE,
            "symbol index stale",
        ),
        (
            "tests/fixtures/failure_modes/ambiguous_containing_symbols.json",
            SymbolEvidenceLinkDegradationCode::AmbiguousContainingSymbols.as_str(),
            "ambiguous symbol match",
        ),
        (
            "tests/fixtures/failure_modes/stale_line_span.json",
            SymbolEvidenceLinkDegradationCode::StaleLineSpan.as_str(),
            "stale source span",
        ),
    ];

    for (path, code, description) in required {
        let fixture = read_json(path)?;
        ensure(
            fixture.pointer("/schema").and_then(Value::as_str)
                == Some("ee.failure_mode_fixture.v1"),
            format!("{path}: fixture schema drifted"),
        )?;
        ensure(
            fixture.pointer("/code").and_then(Value::as_str) == Some(code),
            format!("{path}: code must cover {description} as {code}"),
        )?;
        ensure(
            fixture
                .pointer("/introduced_by/bead")
                .and_then(Value::as_str)
                == Some("bd-2xuu7.6"),
            format!("{path}: introduced_by.bead should pin bd-2xuu7.6"),
        )?;
        let surfaces = fixture
            .pointer("/surfaces")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{path}: surfaces[] missing"))?;
        ensure(
            surfaces.iter().any(|surface| {
                surface.as_str().is_some_and(|value| {
                    value.contains("symbol") || value == "context" || value == "why"
                })
            }),
            format!("{path}: fixture must identify a symbol/context/why surface"),
        )?;
        let messages = fixture
            .pointer("/expected_emission/message_contains")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{path}: expected_emission.message_contains[] missing"))?;
        ensure(
            !messages.is_empty(),
            format!("{path}: message_contains[] should explain {description}"),
        )?;
    }

    Ok(())
}

#[test]
fn symbol_graph_schemas_admit_fixture_degraded_codes() -> TestResult {
    let snapshot = read_json("docs/schemas/ee.symbol_snapshot.v1.json")?;
    let snapshot_codes = schema_enum(&snapshot, "/$defs/degradation/properties/code/enum")?;
    for code in [
        SymbolGraphDegradationCode::SourceUnparsable.as_str(),
        SYMBOL_INDEX_STALE_CODE,
    ] {
        ensure(
            snapshot_codes.iter().any(|candidate| candidate == code),
            format!("symbol snapshot schema missing degraded code {code}"),
        )?;
    }

    let links = read_json("docs/schemas/ee.symbol_evidence_links.v1.json")?;
    let link_codes = schema_enum(&links, "/$defs/degradation/properties/code/enum")?;
    for code in [
        SymbolEvidenceLinkDegradationCode::AmbiguousContainingSymbols.as_str(),
        SymbolEvidenceLinkDegradationCode::StaleLineSpan.as_str(),
    ] {
        ensure(
            link_codes.iter().any(|candidate| candidate == code),
            format!("symbol evidence links schema missing degraded code {code}"),
        )?;
    }

    Ok(())
}

#[test]
fn symbol_graph_e2e_script_logs_required_surfaces() -> TestResult {
    let script_path = repo_root()
        .join("scripts")
        .join("e2e_overhaul")
        .join("symbol_graph.sh");
    let output = Command::new("bash")
        .arg("-n")
        .arg(&script_path)
        .output()
        .map_err(|error| format!("spawn bash -n {}: {error}", script_path.display()))?;
    if !output.status.success() {
        return Err(format!(
            "bash -n {} failed: {}",
            script_path.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let script = std::fs::read_to_string(&script_path)
        .map_err(|error| format!("read {}: {error}", script_path.display()))?;
    for marker in [
        "ee.test_event.v1",
        "symbolScenario",
        "symbol_graph_contract",
        "symbol_graph_extraction_logged",
        "symbol_graph_linking_logged",
        "context_boost",
        "why_explanation",
        "ee.symbol_snapshot.v1",
        "ee.symbol_evidence_links.v1",
    ] {
        ensure(
            script.contains(marker),
            format!("symbol graph e2e script missing marker {marker:?}"),
        )?;
    }

    for forbidden in ["sourceBody", "rawSource", "sourceText", "secretValue"] {
        ensure(
            !script.contains(forbidden),
            format!("symbol graph e2e script should stay redaction-safe; found {forbidden}"),
        )?;
    }

    Ok(())
}
