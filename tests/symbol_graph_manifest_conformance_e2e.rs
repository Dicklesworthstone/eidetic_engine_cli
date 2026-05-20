use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ee::core::symbol_graph::{
    RustSourceInput, SymbolEvidenceInput, SymbolGraphExtractor,
    extract_rust_symbol_snapshot_from_sources, link_symbol_evidence,
};
use ee::models::{
    SYMBOL_EVIDENCE_LINKS_SCHEMA_V1, SYMBOL_SNAPSHOT_SCHEMA_V1, SymbolEvidenceSourceKind,
    SymbolGraphDegradationCode,
};
use serde::Serialize;
use serde_json::{Value, json};

type TestResult = Result<(), String>;

static WORKSPACE_COUNTER: AtomicU64 = AtomicU64::new(0);

const ALPHA_SOURCE: &str = r#"
pub struct AlphaService {
    value: u64,
}

impl AlphaService {
    pub fn alpha_handler(&self, input: u64) -> u64 {
        let raw_body_marker_9382 = "symbol-conformance-source-body-marker";
        self.value + input + raw_body_marker_9382.len() as u64
    }
}
"#;

const BETA_SOURCE: &str = r#"
pub enum BetaMode {
    Fast,
    Slow,
}

pub fn beta_router(mode: BetaMode) -> &'static str {
    match mode {
        BetaMode::Fast => "fast",
        BetaMode::Slow => "slow",
    }
}
"#;

struct E2eWorkspace {
    path: PathBuf,
    log_path: PathBuf,
}

impl E2eWorkspace {
    fn create(test_name: &str) -> Result<Self, String> {
        let base = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("clock before UNIX_EPOCH: {error}"))?
            .as_nanos();
        let counter = WORKSPACE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = base.join("ee-review-e2e").join(format!(
            "{test_name}-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(path.join("src"))
            .map_err(|error| format!("create {}: {error}", path.join("src").display()))?;
        let log_path = path.join("symbol_graph_manifest_conformance.events.jsonl");
        Ok(Self { path, log_path })
    }

    fn as_str(&self) -> Result<&str, String> {
        self.path
            .to_str()
            .ok_or_else(|| format!("workspace path is not UTF-8: {}", self.path.display()))
    }

    fn write_source(&self, relative: &str, source: &str) -> TestResult {
        let path = self.path.join(relative);
        let parent = path
            .parent()
            .ok_or_else(|| format!("source path has no parent: {}", path.display()))?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
        fs::write(&path, source).map_err(|error| format!("write {}: {error}", path.display()))
    }

    fn log(&self, phase: &str, payload: Value) -> TestResult {
        log_event(&self.log_path, phase, payload)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
enum RequirementLevel {
    Must,
    Should,
}

impl RequirementLevel {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Must => "MUST",
            Self::Should => "SHOULD",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct RequirementVerdict {
    id: &'static str,
    section: &'static str,
    level: RequirementLevel,
    description: &'static str,
    status: &'static str,
    detail: String,
}

#[derive(Default, Serialize)]
struct SectionStats {
    must_total: usize,
    should_total: usize,
    passing: usize,
    failing: usize,
}

fn conformance_case(
    id: &'static str,
    section: &'static str,
    level: RequirementLevel,
    description: &'static str,
    result: TestResult,
) -> RequirementVerdict {
    match result {
        Ok(()) => RequirementVerdict {
            id,
            section,
            level,
            description,
            status: "PASS",
            detail: String::new(),
        },
        Err(detail) => RequirementVerdict {
            id,
            section,
            level,
            description,
            status: "FAIL",
            detail,
        },
    }
}

fn log_event(path: &Path, phase: &str, payload: Value) -> TestResult {
    let entry = json!({
        "schema": "ee.test_event.v1",
        "suite": "symbol_graph_manifest_conformance_e2e",
        "phase": phase,
        "payload": payload,
    });
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("open {}: {error}", path.display()))?;
    writeln!(file, "{entry}").map_err(|error| format!("write {}: {error}", path.display()))
}

fn emit_verdicts(workspace: &E2eWorkspace, verdicts: &[RequirementVerdict]) -> TestResult {
    let mut by_section: BTreeMap<&'static str, SectionStats> = BTreeMap::new();
    for verdict in verdicts {
        workspace.log(
            "conformance_verdict",
            serde_json::to_value(verdict).map_err(|error| error.to_string())?,
        )?;
        let stats = by_section.entry(verdict.section).or_default();
        match verdict.level {
            RequirementLevel::Must => stats.must_total += 1,
            RequirementLevel::Should => stats.should_total += 1,
        }
        if verdict.status == "PASS" {
            stats.passing += 1;
        } else {
            stats.failing += 1;
        }
    }
    workspace.log(
        "conformance_matrix",
        json!({
            "spec": "ee.symbol_snapshot.v1 manifest identity",
            "sections": by_section,
            "total": verdicts.len(),
            "failing": verdicts.iter().filter(|verdict| verdict.status != "PASS").count(),
        }),
    )
}

fn run_ee(workspace: &E2eWorkspace, phase: &str, args: &[&str]) -> Result<Output, String> {
    workspace.log(
        phase,
        json!({
            "event": "command_start",
            "argv": args,
        }),
    )?;
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))?;
    workspace.log(
        phase,
        json!({
            "event": "command_finish",
            "argv": args,
            "status": output.status.code(),
            "success": output.status.success(),
            "elapsedMs": started.elapsed().as_millis(),
            "stdoutBytes": output.stdout.len(),
            "stderrBytes": output.stderr.len(),
        }),
    )?;
    Ok(output)
}

fn expect_success(output: &Output, label: &str) -> TestResult {
    ensure(
        output.status.success(),
        format!(
            "{label} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

fn stdout_json(output: &Output, label: &str) -> Result<Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{label}: stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{label}: stdout was not JSON: {error}\n{stdout}"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn init_real_memory_stack(workspace: &E2eWorkspace) -> TestResult {
    let workspace_path = workspace.as_str()?;
    let init = run_ee(
        workspace,
        "init",
        &["--workspace", workspace_path, "init", "--json"],
    )?;
    expect_success(&init, "init")?;

    let remember = run_ee(
        workspace,
        "remember",
        &[
            "--workspace",
            workspace_path,
            "remember",
            "Symbol graph manifest conformance uses real fsqlite and search indexing.",
            "--level",
            "semantic",
            "--kind",
            "fact",
            "--tags",
            "symbol,conformance",
            "--no-auto-link",
            "--no-propose-candidates",
            "--json",
        ],
    )?;
    expect_success(&remember, "remember")?;

    let search = run_ee(
        workspace,
        "search",
        &[
            "--workspace",
            workspace_path,
            "search",
            "symbol graph manifest conformance",
            "--json",
        ],
    )?;
    expect_success(&search, "search")?;
    let search_json = stdout_json(&search, "search")?;
    ensure(
        search_json["success"] == json!(true),
        format!("real search response should be successful: {search_json}"),
    )
}

fn source_inputs_alpha_beta() -> Vec<RustSourceInput<'static>> {
    vec![
        RustSourceInput::new("src/alpha.rs", ALPHA_SOURCE),
        RustSourceInput::new("src/beta.rs", BETA_SOURCE),
    ]
}

fn source_inputs_beta_alpha() -> Vec<RustSourceInput<'static>> {
    vec![
        RustSourceInput::new("src/beta.rs", BETA_SOURCE),
        RustSourceInput::new("src/alpha.rs", ALPHA_SOURCE),
    ]
}

fn assert_source_order_conformance() -> TestResult {
    let first = extract_rust_symbol_snapshot_from_sources(&source_inputs_alpha_beta());
    let second = extract_rust_symbol_snapshot_from_sources(&source_inputs_beta_alpha());

    ensure(
        first.schema == SYMBOL_SNAPSHOT_SCHEMA_V1,
        format!("snapshot schema drifted: {}", first.schema),
    )?;
    ensure(
        first.degraded.is_empty() && second.degraded.is_empty(),
        format!(
            "valid source fixtures should not degrade: first={:?} second={:?}",
            first.degraded, second.degraded
        ),
    )?;
    ensure(
        first.snapshot_hash == second.snapshot_hash,
        format!(
            "snapshot hash should be input-order invariant: first={} second={}",
            first.snapshot_hash, second.snapshot_hash
        ),
    )?;
    ensure(
        first.files == second.files,
        "source files should be sorted into the same order",
    )?;
    ensure(
        first.symbols == second.symbols,
        "symbols should be sorted into the same order",
    )
}

fn assert_path_order_conformance(workspace: &E2eWorkspace) -> TestResult {
    let alpha = workspace.path.join("src/alpha.rs");
    let beta = workspace.path.join("src/beta.rs");
    let extractor = SymbolGraphExtractor::default();
    let first = extractor.extract_paths(&workspace.path, vec![beta.clone(), alpha.clone()]);
    let second = extractor.extract_paths(&workspace.path, vec![alpha, beta]);

    ensure(
        first.snapshot_hash == second.snapshot_hash,
        format!(
            "path extraction snapshot hash should be path-order invariant: first={} second={}",
            first.snapshot_hash, second.snapshot_hash
        ),
    )?;
    ensure(
        first
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>()
            == vec!["src/alpha.rs", "src/beta.rs"],
        format!(
            "files should be workspace-relative and sorted: {:?}",
            first.files
        ),
    )?;
    ensure(
        first.symbols == second.symbols,
        "path extraction symbols should be sorted into the same order",
    )
}

fn assert_evidence_order_conformance() -> TestResult {
    let snapshot = extract_rust_symbol_snapshot_from_sources(&source_inputs_alpha_beta());
    let alpha = SymbolEvidenceInput::new(
        SymbolEvidenceSourceKind::Memory,
        "mem_alpha",
        "memory://alpha",
        "src/alpha.rs",
        7,
        9,
        0.83,
    );
    let beta = SymbolEvidenceInput::new(
        SymbolEvidenceSourceKind::Memory,
        "mem_beta",
        "memory://beta",
        "src/beta.rs",
        7,
        10,
        0.61,
    );
    let first = link_symbol_evidence(&snapshot, &[beta.clone(), alpha.clone()]);
    let second = link_symbol_evidence(&snapshot, &[alpha, beta]);

    ensure(
        first.schema == SYMBOL_EVIDENCE_LINKS_SCHEMA_V1,
        format!("link-set schema drifted: {}", first.schema),
    )?;
    ensure(
        first.source_manifest_hash == second.source_manifest_hash,
        format!(
            "evidence source manifest hash should be input-order invariant: first={} second={}",
            first.source_manifest_hash, second.source_manifest_hash
        ),
    )?;
    ensure(
        first.links == second.links,
        "evidence links should be sorted into the same order",
    )?;
    ensure(
        first.degraded.is_empty() && second.degraded.is_empty(),
        format!(
            "valid evidence spans should not degrade: first={:?} second={:?}",
            first.degraded, second.degraded
        ),
    )
}

fn assert_snapshot_redaction_conformance() -> TestResult {
    let snapshot = extract_rust_symbol_snapshot_from_sources(&source_inputs_alpha_beta());
    let json = serde_json::to_string(&snapshot).map_err(|error| error.to_string())?;
    ensure(
        !json.contains("symbol-conformance-source-body-marker"),
        "symbol snapshot JSON must not retain raw source-body literals",
    )?;
    ensure(
        !json.contains("self.value + input"),
        "symbol snapshot JSON must not retain raw function bodies",
    )
}

fn assert_missing_source_degrades_conformance(workspace: &E2eWorkspace) -> TestResult {
    let missing = workspace.path.join("src/missing.rs");
    let snapshot = SymbolGraphExtractor::default().extract_paths(&workspace.path, vec![missing]);
    ensure(
        snapshot.symbols.is_empty(),
        format!(
            "missing source should not produce symbols: {:?}",
            snapshot.symbols
        ),
    )?;
    ensure(
        snapshot.degraded.iter().any(|item| {
            item.code == SymbolGraphDegradationCode::SourceMissing
                && item.path.as_deref() == Some("src/missing.rs")
        }),
        format!(
            "missing source should emit source_missing degradation: {:?}",
            snapshot.degraded
        ),
    )
}

#[test]
fn symbol_graph_manifest_hash_conformance_matrix() -> TestResult {
    let workspace = E2eWorkspace::create("symbol-manifest-conformance")?;
    workspace.write_source("src/alpha.rs", ALPHA_SOURCE)?;
    workspace.write_source("src/beta.rs", BETA_SOURCE)?;
    workspace.log(
        "setup",
        json!({
            "workspace": workspace.path.display().to_string(),
            "spec": "ee.symbol_snapshot.v1 manifest identity",
            "skill": "testing-conformance-harnesses",
        }),
    )?;
    init_real_memory_stack(&workspace)?;

    let verdicts = vec![
        conformance_case(
            "SYM-MANIFEST-MUST-001",
            "snapshot identity",
            RequirementLevel::Must,
            "Source-array extraction is invariant to input order.",
            assert_source_order_conformance(),
        ),
        conformance_case(
            "SYM-MANIFEST-MUST-002",
            "snapshot identity",
            RequirementLevel::Must,
            "Path extraction over a real workspace is invariant to input order.",
            assert_path_order_conformance(&workspace),
        ),
        conformance_case(
            "SYM-MANIFEST-MUST-003",
            "evidence identity",
            RequirementLevel::Must,
            "Evidence link sourceManifestHash is invariant to evidence input order.",
            assert_evidence_order_conformance(),
        ),
        conformance_case(
            "SYM-MANIFEST-MUST-004",
            "redaction",
            RequirementLevel::Must,
            "Symbol snapshots do not serialize raw source bodies.",
            assert_snapshot_redaction_conformance(),
        ),
        conformance_case(
            "SYM-MANIFEST-SHOULD-001",
            "degraded modes",
            RequirementLevel::Should,
            "Missing real source paths emit a structured degradation instead of symbols.",
            assert_missing_source_degrades_conformance(&workspace),
        ),
    ];
    emit_verdicts(&workspace, &verdicts)?;

    let failures = verdicts
        .iter()
        .filter(|verdict| verdict.status != "PASS")
        .map(|verdict| {
            format!(
                "{} {} {}: {}",
                verdict.id,
                verdict.level.as_str(),
                verdict.description,
                verdict.detail
            )
        })
        .collect::<Vec<_>>();
    ensure(
        failures.is_empty(),
        format!(
            "symbol graph conformance failures:\n{}",
            failures.join("\n")
        ),
    )
}
