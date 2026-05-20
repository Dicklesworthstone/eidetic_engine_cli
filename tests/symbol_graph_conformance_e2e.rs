use std::fs;
use std::path::{Path, PathBuf};

use ee::core::symbol_graph::{SymbolEvidenceInput, SymbolGraphExtractor, link_symbol_evidence};
use ee::models::{
    SymbolEvidenceLinkSet, SymbolEvidenceResolution, SymbolEvidenceSourceKind, SymbolRecord,
};
use serde_json::json;
use tempfile::{Builder as TempDirBuilder, TempDir};

type TestResult = Result<(), String>;

struct ConformanceCase {
    id: &'static str,
    level: &'static str,
    requirement: &'static str,
}

impl ConformanceCase {
    fn log(&self, phase: &str, payload: serde_json::Value) {
        eprintln!(
            "{}",
            json!({
                "schema": "ee.test_event.v1",
                "kind": "symbol_graph_conformance",
                "caseId": self.id,
                "level": self.level,
                "requirement": self.requirement,
                "phase": phase,
                "payload": payload,
            })
        );
    }
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn worker_local_tempdir(prefix: &str) -> Result<TempDir, String> {
    let tmp_root = Path::new("/tmp");
    if tmp_root.is_dir() {
        TempDirBuilder::new()
            .prefix(prefix)
            .tempdir_in(tmp_root)
            .map_err(|error| format!("tempdir: {error}"))
    } else {
        TempDirBuilder::new()
            .prefix(prefix)
            .tempdir()
            .map_err(|error| format!("tempdir: {error}"))
    }
}

fn write_symbol_fixture(workspace: &Path) -> Result<(PathBuf, PathBuf), String> {
    let src_dir = workspace.join("src");
    fs::create_dir_all(&src_dir).map_err(|error| format!("create src dir: {error}"))?;

    let alpha = src_dir.join("alpha.rs");
    fs::write(
        &alpha,
        r#"
pub mod alpha_lane {
    pub struct AlphaEngine {
        pub value: u64,
    }

    impl AlphaEngine {
        pub fn render_alpha(&self) -> u64 {
            self.value + 1
        }
    }
}
"#,
    )
    .map_err(|error| format!("write {}: {error}", alpha.display()))?;

    let beta = src_dir.join("beta.rs");
    fs::write(
        &beta,
        r#"
pub mod beta_lane {
    pub struct BetaEngine {
        pub value: u64,
    }

    impl BetaEngine {
        pub fn render_beta(&self) -> u64 {
            self.value + 2
        }
    }
}
"#,
    )
    .map_err(|error| format!("write {}: {error}", beta.display()))?;

    Ok((alpha, beta))
}

fn symbol_by_name<'a>(symbols: &'a [SymbolRecord], name: &str) -> Result<&'a SymbolRecord, String> {
    symbols
        .iter()
        .find(|symbol| symbol.canonical_name == name)
        .ok_or_else(|| format!("symbol {name} not found"))
}

fn evidence_for_symbol<'a>(
    source_kind: SymbolEvidenceSourceKind,
    evidence_id: &'a str,
    provenance_uri: &'a str,
    path: &'a str,
    symbol: &SymbolRecord,
) -> SymbolEvidenceInput<'a> {
    SymbolEvidenceInput::new(
        source_kind,
        evidence_id,
        provenance_uri,
        path,
        symbol.range.start_line,
        symbol.range.end_line,
        0.9371,
    )
}

fn assert_equivalent_link_sets(
    forward: &SymbolEvidenceLinkSet,
    reversed: &SymbolEvidenceLinkSet,
) -> TestResult {
    ensure(
        forward.source_manifest_hash == reversed.source_manifest_hash,
        format!(
            "source manifest hash must be invariant to caller order: {} != {}",
            forward.source_manifest_hash, reversed.source_manifest_hash
        ),
    )?;
    ensure(
        forward.links.len() == 2 && reversed.links.len() == 2,
        "both link sets should contain two evidence links",
    )?;
    let forward_ids: Vec<&str> = forward
        .links
        .iter()
        .map(|link| link.evidence_id.as_str())
        .collect();
    let reversed_ids: Vec<&str> = reversed
        .links
        .iter()
        .map(|link| link.evidence_id.as_str())
        .collect();
    ensure(
        forward_ids == reversed_ids,
        format!("link order must be canonical after sorting: {forward_ids:?} != {reversed_ids:?}"),
    )?;
    ensure(
        forward
            .links
            .iter()
            .all(|link| link.resolution == SymbolEvidenceResolution::ExactSymbol),
        "all fixture links should resolve to exact symbols",
    )?;
    Ok(())
}

#[test]
fn symbol_evidence_manifest_hash_conforms_to_order_independent_contract() -> TestResult {
    let case = ConformanceCase {
        id: "symbol-manifest-order-v1",
        level: "MUST",
        requirement: "symbol evidence source manifest hash and link order are canonical for the same evidence set",
    };
    let workspace = worker_local_tempdir("ee-symbol-graph-conformance-")?;
    let (alpha_path, beta_path) = write_symbol_fixture(workspace.path())?;

    case.log(
        "setup",
        json!({
            "workspace": workspace.path().display().to_string(),
            "files": [
                alpha_path.display().to_string(),
                beta_path.display().to_string()
            ],
        }),
    );

    let extractor = SymbolGraphExtractor::default();
    let snapshot = extractor.extract_paths(workspace.path(), vec![beta_path, alpha_path]);
    ensure(
        snapshot.degraded.is_empty(),
        format!(
            "fixture snapshot should not degrade: {:?}",
            snapshot.degraded
        ),
    )?;
    let alpha = symbol_by_name(
        &snapshot.symbols,
        "alpha_lane::impl AlphaEngine::render_alpha",
    )?;
    let beta = symbol_by_name(&snapshot.symbols, "beta_lane::impl BetaEngine::render_beta")?;

    let alpha_evidence = evidence_for_symbol(
        SymbolEvidenceSourceKind::Memory,
        "mem_alpha_contract",
        "file://src/alpha.rs#L7-L9",
        "src/alpha.rs",
        alpha,
    );
    let beta_evidence = evidence_for_symbol(
        SymbolEvidenceSourceKind::CassEvidence,
        "cass_beta_contract",
        "cass-session://symbol-contract#L7-L9",
        "src/beta.rs",
        beta,
    );

    let forward = link_symbol_evidence(&snapshot, &[alpha_evidence.clone(), beta_evidence.clone()]);
    let reversed = link_symbol_evidence(&snapshot, &[beta_evidence, alpha_evidence]);

    case.log(
        "assert",
        json!({
            "snapshotHash": snapshot.snapshot_hash,
            "forwardManifest": forward.source_manifest_hash,
            "reversedManifest": reversed.source_manifest_hash,
            "forwardOrder": forward.links.iter().map(|link| link.evidence_id.as_str()).collect::<Vec<_>>(),
            "reversedOrder": reversed.links.iter().map(|link| link.evidence_id.as_str()).collect::<Vec<_>>(),
        }),
    );

    assert_equivalent_link_sets(&forward, &reversed)?;

    let serialized = serde_json::to_string(&forward)
        .map_err(|error| format!("serialize symbol evidence link set: {error}"))?;
    for raw_source_fragment in [
        "pub mod alpha_lane",
        "pub struct AlphaEngine",
        "pub fn render_beta",
        "self.value + 2",
    ] {
        ensure(
            !serialized.contains(raw_source_fragment),
            format!(
                "symbol evidence JSON should not leak raw source fragment {raw_source_fragment:?}"
            ),
        )?;
    }

    case.log(
        "pass",
        json!({
            "linkCount": forward.links.len(),
            "degradedCount": forward.degraded.len(),
        }),
    );

    Ok(())
}
