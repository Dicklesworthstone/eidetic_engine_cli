use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

type TestResult = Result<(), String>;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum FindingKind {
    CassImportMetadataDefault,
    EmptyVecFallback,
    IgnoredReadToEnd,
    JsonlRequiredFieldDefault,
    PersistedSourceErrorDrop,
    SerializationDefault,
    ThreadJoinDefault,
}

impl FindingKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CassImportMetadataDefault => "cass_import_metadata_default",
            Self::EmptyVecFallback => "empty_vec_fallback",
            Self::IgnoredReadToEnd => "ignored_read_to_end",
            Self::JsonlRequiredFieldDefault => "jsonl_required_field_default",
            Self::PersistedSourceErrorDrop => "persisted_source_error_drop",
            Self::SerializationDefault => "serialization_default",
            Self::ThreadJoinDefault => "thread_join_default",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Classification {
    MustFix,
}

impl Classification {
    #[allow(dead_code)]
    const fn as_str(self) -> &'static str {
        match self {
            Self::MustFix => "must_fix",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct InventoryEntry {
    kind: FindingKind,
    file: &'static str,
    needle: &'static str,
    expected_count: usize,
    classification: Classification,
    action: &'static str,
    rationale: &'static str,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Finding {
    kind: FindingKind,
    file: String,
    line: usize,
    text: String,
}

const INVENTORY: &[InventoryEntry] = &[
    InventoryEntry {
        kind: FindingKind::IgnoredReadToEnd,
        file: "src/cass/process.rs",
        needle: "let _ = std::io::Read::read_to_end",
        expected_count: 0,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.2",
        rationale: "CASS subprocess pipe read failures must not become empty stdout/stderr.",
    },
    InventoryEntry {
        kind: FindingKind::ThreadJoinDefault,
        file: "src/cass/process.rs",
        needle: ".join().unwrap_or_default()",
        expected_count: 0,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.2",
        rationale: "Reader-thread panics must not become empty subprocess output.",
    },
    InventoryEntry {
        kind: FindingKind::SerializationDefault,
        file: "src/hooks/installer.rs",
        needle: "serde_json::to_string",
        expected_count: 2,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.3",
        rationale: "Hook install/status JSON is machine-facing and must report serialization failures.",
    },
    InventoryEntry {
        kind: FindingKind::SerializationDefault,
        file: "src/output/mod.rs",
        needle: "serde_json::to_string",
        expected_count: 8,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.3",
        rationale: "Output renderers must not wrap empty raw data in a success envelope.",
    },
    InventoryEntry {
        kind: FindingKind::SerializationDefault,
        file: "src/cli/mod.rs",
        needle: "serde_json::to_string",
        expected_count: 2,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.8",
        rationale: "CLI JSON handlers need a stable serialization error path.",
    },
    InventoryEntry {
        kind: FindingKind::SerializationDefault,
        file: "src/core/audit.rs",
        needle: "serde_json::to_string",
        expected_count: 4,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.8",
        rationale: "Audit reports are durable machine evidence.",
    },
    InventoryEntry {
        kind: FindingKind::SerializationDefault,
        file: "src/core/feedback.rs",
        needle: "serde_json::to_string",
        expected_count: 3,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.8",
        rationale: "Feedback reports influence learning state and need honest JSON failures.",
    },
    InventoryEntry {
        kind: FindingKind::SerializationDefault,
        file: "src/core/handoff.rs",
        needle: "serde_json::to_string",
        expected_count: 9,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.8",
        rationale: "Handoff artifacts are consumed by agents and must not serialize to empty data.",
    },
    InventoryEntry {
        kind: FindingKind::SerializationDefault,
        file: "src/core/lab.rs",
        needle: "serde_json::to_string",
        expected_count: 6,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.8",
        rationale: "Lab reports feed evaluations and intervention evidence.",
    },
    InventoryEntry {
        kind: FindingKind::SerializationDefault,
        file: "src/core/learn.rs",
        needle: "serde_json::to_string",
        expected_count: 4,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.8",
        rationale: "Learning reports must not erase serialization failures.",
    },
    InventoryEntry {
        kind: FindingKind::SerializationDefault,
        file: "src/core/legacy_import.rs",
        needle: "serde_json::to_string",
        expected_count: 1,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.8",
        rationale: "Legacy import data still crosses machine-facing boundaries.",
    },
    InventoryEntry {
        kind: FindingKind::SerializationDefault,
        file: "src/core/preflight.rs",
        needle: "serde_json::to_string",
        expected_count: 3,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.8",
        rationale: "Preflight reports guide whether agents may proceed.",
    },
    InventoryEntry {
        kind: FindingKind::SerializationDefault,
        file: "src/core/procedure.rs",
        needle: "serde_json::to_string",
        expected_count: 9,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.8",
        rationale: "Procedure reports and drift reports are stable public contracts.",
    },
    InventoryEntry {
        kind: FindingKind::SerializationDefault,
        file: "src/core/rehearse.rs",
        needle: "serde_json::to_string",
        expected_count: 4,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.8",
        rationale: "Rehearsal reports are agent-facing verification evidence.",
    },
    InventoryEntry {
        kind: FindingKind::SerializationDefault,
        file: "src/core/repro.rs",
        needle: "serde_json::to_string",
        expected_count: 4,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.8",
        rationale: "Repro-pack manifests and locks must not be silently blank.",
    },
    InventoryEntry {
        kind: FindingKind::SerializationDefault,
        file: "src/core/tripwire.rs",
        needle: "serde_json::to_string",
        expected_count: 4,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.8",
        rationale: "Tripwire reports are policy evidence for agents.",
    },
    InventoryEntry {
        kind: FindingKind::SerializationDefault,
        file: "src/models/mutation.rs",
        needle: "serde_json::to_string",
        expected_count: 3,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.8",
        rationale: "Mutation reports describe durable state changes.",
    },
    InventoryEntry {
        kind: FindingKind::JsonlRequiredFieldDefault,
        file: "src/models/jsonl.rs",
        needle: "unwrap_or_default()",
        expected_count: 37,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.4",
        rationale: "JSONL export builders default required IDs, timestamps, and content fields.",
    },
    InventoryEntry {
        kind: FindingKind::CassImportMetadataDefault,
        file: "src/cass/import.rs",
        needle: "message_count: session.message_count.unwrap_or_default()",
        expected_count: 1,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.11",
        rationale: "Missing CASS message counts currently become real-looking zero counts.",
    },
    InventoryEntry {
        kind: FindingKind::CassImportMetadataDefault,
        file: "src/cass/import.rs",
        needle: "unwrap_or_default();",
        expected_count: 2,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.11",
        rationale: "Missing CASS modified/size metadata currently produces path-only content hashes.",
    },
    InventoryEntry {
        kind: FindingKind::PersistedSourceErrorDrop,
        file: "src/core/procedure.rs",
        needle: "Ok(None) | Err(_) => return None",
        expected_count: 4,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.9",
        rationale: "Procedure verification must distinguish missing evidence from storage failures.",
    },
    InventoryEntry {
        kind: FindingKind::EmptyVecFallback,
        file: "src/serve.rs",
        needle: "return Ok(Vec::new());",
        expected_count: 2,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.12",
        rationale: "Serve/daemon empty results need explicit no-table versus failed-read semantics.",
    },
    InventoryEntry {
        kind: FindingKind::EmptyVecFallback,
        file: "src/core/memory.rs",
        needle: "return Ok(Vec::new());",
        expected_count: 4,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.12",
        rationale: "Memory loaders feed user-visible evidence and require audited empty semantics.",
    },
    InventoryEntry {
        kind: FindingKind::EmptyVecFallback,
        file: "src/core/preflight_guard.rs",
        needle: "return Ok(Vec::new());",
        expected_count: 1,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.12",
        rationale: "Preflight history empty-success paths affect release gating.",
    },
    InventoryEntry {
        kind: FindingKind::EmptyVecFallback,
        file: "src/core/recorder.rs",
        needle: "return Ok(Vec::new());",
        expected_count: 1,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.12",
        rationale: "Recorder event/run loaders must distinguish empty traces from failed reads.",
    },
    InventoryEntry {
        kind: FindingKind::EmptyVecFallback,
        file: "src/core/claims.rs",
        needle: "return Ok(Vec::new());",
        expected_count: 1,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.12",
        rationale: "Claim parsing should preserve malformed input versus no claims.",
    },
    InventoryEntry {
        kind: FindingKind::EmptyVecFallback,
        file: "src/core/procedure.rs",
        needle: "return Ok(Vec::new());",
        expected_count: 1,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.12",
        rationale: "Procedure loaders should document empty store versus read failure.",
    },
    InventoryEntry {
        kind: FindingKind::EmptyVecFallback,
        file: "src/models/query.rs",
        needle: "return Ok(Vec::new());",
        expected_count: 1,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.12",
        rationale: "Query parsing no-op paths should remain deliberate and covered.",
    },
    InventoryEntry {
        kind: FindingKind::EmptyVecFallback,
        file: "src/cli/mod.rs",
        needle: "return Ok(Vec::new());",
        expected_count: 2,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.12",
        rationale: "CLI helper empty results must not mask data-loading errors.",
    },
    InventoryEntry {
        kind: FindingKind::EmptyVecFallback,
        file: "src/cli/mod.rs",
        needle: "latest_demo_audit_by_id(cli).unwrap_or_default()",
        expected_count: 1,
        classification: Classification::MustFix,
        action: "eidetic_engine_cli-sos5.10",
        rationale: "Demo list must distinguish empty audit ledgers from ledger load failures.",
    },
];

#[test]
fn no_silent_fallback_inventory_covers_required_sources() -> TestResult {
    if canonical_contract_exists() {
        return Ok(());
    }

    let required_files = [
        "src/cass/process.rs",
        "src/output/mod.rs",
        "src/hooks/installer.rs",
        "src/models/jsonl.rs",
    ];
    let inventoried_files = INVENTORY
        .iter()
        .map(|entry| entry.file)
        .collect::<BTreeSet<_>>();

    for file in required_files {
        ensure(
            inventoried_files.contains(file),
            format!("inventory must cover required source file {file}"),
        )?;
    }

    for entry in INVENTORY {
        ensure(
            entry.classification == Classification::MustFix,
            format!(
                "{}:{} must be explicitly classified",
                entry.file,
                entry.kind.as_str()
            ),
        )?;
        ensure(
            !entry.rationale.trim().is_empty(),
            format!(
                "{}:{} needs a non-empty rationale",
                entry.file,
                entry.kind.as_str()
            ),
        )?;
        ensure(
            entry.action.starts_with("eidetic_engine_cli-"),
            format!(
                "{}:{} must link to a Beads follow-up",
                entry.file,
                entry.kind.as_str()
            ),
        )?;
    }

    Ok(())
}

#[test]
fn production_fallback_scan_matches_inventory() -> TestResult {
    if canonical_contract_exists() {
        return Ok(());
    }

    let findings = scan_findings()?;
    let unmatched = findings
        .iter()
        .filter(|finding| !INVENTORY.iter().any(|entry| entry_matches(entry, finding)))
        .collect::<Vec<_>>();
    ensure(
        unmatched.is_empty(),
        format!(
            "unclassified silent-fallback findings:\n{}",
            format_findings(&unmatched)
        ),
    )?;

    for entry in INVENTORY {
        let actual_count = findings
            .iter()
            .filter(|finding| entry_matches(entry, finding))
            .count();
        ensure(
            actual_count == entry.expected_count,
            format!(
                "{}:{} expected {} classified finding(s), found {actual_count}",
                entry.file,
                entry.kind.as_str(),
                entry.expected_count
            ),
        )?;
    }

    Ok(())
}

#[test]
fn must_fix_actions_reference_existing_beads() -> TestResult {
    if canonical_contract_exists() {
        return Ok(());
    }

    let issues_path = project_root().join(".beads").join("issues.jsonl");
    let issues = fs::read_to_string(&issues_path)
        .map_err(|error| format!("failed to read {}: {error}", issues_path.display()))?;
    let mut missing = Vec::new();
    for action in INVENTORY
        .iter()
        .map(|entry| entry.action)
        .collect::<BTreeSet<_>>()
    {
        let needle = format!(r#""id":"{action}""#);
        if !issues.contains(&needle) {
            missing.push(action);
        }
    }

    ensure(
        missing.is_empty(),
        format!(
            "inventory references missing Beads issues: {}",
            missing.join(", ")
        ),
    )
}

fn scan_findings() -> Result<Vec<Finding>, String> {
    let src_root = project_root().join("src");
    let mut files = Vec::new();
    collect_rust_files(&src_root, &mut files)?;
    let mut findings = Vec::new();
    for path in files {
        scan_file(&path, &mut findings)?;
    }
    findings.sort();
    Ok(findings)
}

fn collect_rust_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        fs::read_dir(dir).map_err(|error| format!("failed to read {}: {error}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("failed to read directory entry: {error}"))?;
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, files)?;
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
            files.push(path);
        }
    }
    Ok(())
}

fn scan_file(path: &Path, findings: &mut Vec<Finding>) -> Result<(), String> {
    let content = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let rel = relative_path(path)?;
    let lines = content.lines().collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        let line_number = index + 1;
        push_if(
            findings,
            line.contains("serde_json::to_string") && line.contains("unwrap_or_default()"),
            FindingKind::SerializationDefault,
            &rel,
            line_number,
            line,
        );
        push_if(
            findings,
            line.contains("let _ = std::io::Read::read_to_end"),
            FindingKind::IgnoredReadToEnd,
            &rel,
            line_number,
            line,
        );
        push_if(
            findings,
            line.contains(".join().unwrap_or_default()"),
            FindingKind::ThreadJoinDefault,
            &rel,
            line_number,
            line,
        );
        push_if(
            findings,
            rel == "src/models/jsonl.rs"
                && line.contains("self.")
                && line.contains(".unwrap_or_default()"),
            FindingKind::JsonlRequiredFieldDefault,
            &rel,
            line_number,
            line,
        );
        push_if(
            findings,
            line.contains("return Ok(Vec::new());"),
            FindingKind::EmptyVecFallback,
            &rel,
            line_number,
            line,
        );
        push_if(
            findings,
            line.contains("Ok(None) | Err(_) => return None"),
            FindingKind::PersistedSourceErrorDrop,
            &rel,
            line_number,
            line,
        );
        push_if(
            findings,
            rel == "src/cass/import.rs" && is_cass_metadata_default(&lines, index, line),
            FindingKind::CassImportMetadataDefault,
            &rel,
            line_number,
            line,
        );
        push_if(
            findings,
            line.contains("latest_demo_audit_by_id(cli).unwrap_or_default()"),
            FindingKind::EmptyVecFallback,
            &rel,
            line_number,
            line,
        );
    }
    Ok(())
}

fn is_cass_metadata_default(lines: &[&str], index: usize, line: &str) -> bool {
    if line.contains("message_count: session.message_count.unwrap_or_default()") {
        return true;
    }
    if !line.contains("unwrap_or_default();") {
        return false;
    }
    lines
        .get(index.saturating_sub(3)..index)
        .unwrap_or(&[])
        .iter()
        .any(|context| context.contains("\"modified\"") || context.contains("\"size_bytes\""))
}

fn push_if(
    findings: &mut Vec<Finding>,
    condition: bool,
    kind: FindingKind,
    file: &str,
    line: usize,
    text: &str,
) {
    if condition {
        findings.push(Finding {
            kind,
            file: file.to_owned(),
            line,
            text: text.trim().to_owned(),
        });
    }
}

fn entry_matches(entry: &InventoryEntry, finding: &Finding) -> bool {
    entry.kind == finding.kind && entry.file == finding.file && finding.text.contains(entry.needle)
}

fn format_findings(findings: &[&Finding]) -> String {
    findings
        .iter()
        .take(20)
        .map(|finding| {
            format!(
                "{}:{}:{}: {}",
                finding.file,
                finding.line,
                finding.kind.as_str(),
                finding.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn canonical_contract_exists() -> bool {
    project_root()
        .join("tests")
        .join("contracts")
        .join("no_silent_fallback.rs")
        .is_file()
}

fn relative_path(path: &Path) -> Result<String, String> {
    path.strip_prefix(project_root())
        .map_err(|error| format!("failed to relativize {}: {error}", path.display()))
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}
