use std::fs;
use std::path::{Path, PathBuf};

type TestResult = Result<(), String>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Disposition {
    MustFix,
    Allowed,
}

#[derive(Clone, Copy, Debug)]
struct InventoryRule {
    id: &'static str,
    file: &'static str,
    fragment: &'static str,
    disposition: Disposition,
    follow_up: Option<&'static str>,
    reason: &'static str,
}

#[derive(Clone, Copy, Debug)]
struct ManualFinding {
    id: &'static str,
    file: &'static str,
    fragment: &'static str,
    follow_up: &'static str,
    reason: &'static str,
}

#[derive(Clone, Debug)]
struct SourceFinding {
    file: String,
    line: usize,
    text: String,
    context: String,
}

const FOLLOW_UP_BEADS: &[&str] = &[
    "eidetic_engine_cli-sos5.2",
    "eidetic_engine_cli-sos5.3",
    "eidetic_engine_cli-sos5.4",
    "eidetic_engine_cli-sos5.7",
    "eidetic_engine_cli-ogy9",
];

const INVENTORY_RULES: &[InventoryRule] = &[
    must_fix(
        "NSF-CASS-PIPE-READ",
        "src/cass/process.rs",
        "read_to_end",
        "eidetic_engine_cli-sos5.2",
        "CASS subprocess pipe read errors must become CassError or explicit degradations.",
    ),
    must_fix(
        "NSF-CASS-PIPE-JOIN",
        "src/cass/process.rs",
        "join().unwrap_or_default()",
        "eidetic_engine_cli-sos5.2",
        "CASS subprocess reader thread failures must not become empty stdout/stderr.",
    ),
    must_fix(
        "NSF-HOOK-INSTALLER-JSON",
        "src/hooks/installer.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Hook installer JSON is machine-facing output and must not serialize to an empty string on failure.",
    ),
    must_fix(
        "NSF-OUTPUT-RENDERER-JSON",
        "src/output/mod.rs",
        "serde_json::to_string(report).unwrap_or_default()",
        "eidetic_engine_cli-sos5.3",
        "Machine-facing renderers must return a stable error/degradation instead of empty JSON.",
    ),
    must_fix(
        "NSF-OUTPUT-SHADOW-INCUMBENT",
        "src/output/mod.rs",
        "incumbent_outcome.clone().unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Shadow decision output should distinguish missing incumbent evidence from an empty incumbent outcome.",
    ),
    must_fix(
        "NSF-CLI-CERTIFICATE-JSON",
        "src/cli/mod.rs",
        "serde_json::to_string_pretty",
        "eidetic_engine_cli-sos5.3",
        "Certificate JSON handlers bypass the shared renderer and silently erase serialization failures.",
    ),
    must_fix(
        "NSF-CLI-CERTIFICATE-ERROR",
        "src/cli/mod.rs",
        "report.error.clone().unwrap_or_default()",
        "eidetic_engine_cli-sos5.3",
        "Certificate error reports should not convert a missing error message into an empty machine string.",
    ),
    must_fix(
        "NSF-CLI-DEMO-AUDIT",
        "src/cli/mod.rs",
        "latest_demo_audit_by_id",
        "eidetic_engine_cli-sos5.4",
        "Demo status output should distinguish missing audit storage from an empty run map.",
    ),
    must_fix(
        "NSF-CLI-DEMO-FILE",
        "src/cli/mod.rs",
        "fn read_text_lossy",
        "eidetic_engine_cli-sos5.3",
        "Demo file reads should not render missing or unreadable expected output as empty content.",
    ),
    must_fix(
        "NSF-MODELS-JSONL-BUILDERS",
        "src/models/jsonl.rs",
        "unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "JSONL export builders default required IDs, timestamps, content, and schema fields to empty values.",
    ),
    must_fix(
        "NSF-CURATE-CERTIFICATE-BUILDER",
        "src/curate/mod.rs",
        "unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Curation risk certificate builders default machine-facing IDs/timestamps to empty values.",
    ),
    must_fix(
        "NSF-MODELS-DECISION-BUILDER",
        "src/models/decision.rs",
        "unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Decision records should distinguish a missing outcome from an empty outcome string.",
    ),
    must_fix(
        "NSF-MODELS-MUTATION-JSON",
        "src/models/mutation.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Mutation reports are machine-facing and must not serialize to empty strings on failure.",
    ),
    must_fix(
        "NSF-MODELS-PROGRESS-BUILDER",
        "src/models/progress.rs",
        "unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Progress records default required operation/message/timestamp fields to empty values.",
    ),
    must_fix(
        "NSF-CORE-AUDIT-JSON",
        "src/core/audit.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Audit timeline JSON is machine-facing output and must not silently serialize to empty.",
    ),
    must_fix(
        "NSF-CORE-BACKUP-IMPORT",
        "src/core/backup.rs",
        "unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Backup import/export records should distinguish absent message, next action, and audit target fields.",
    ),
    must_fix(
        "NSF-CORE-CLAIMS-INPUT",
        "src/core/claims.rs",
        "unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Claim parsing defaults optional statement/artifact collections into machine-facing records and needs an explicit contract.",
    ),
    must_fix(
        "NSF-CORE-FEEDBACK-JSON",
        "src/core/feedback.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Feedback reports are machine-facing and must not silently serialize to empty.",
    ),
    must_fix(
        "NSF-CORE-HANDOFF-JSON",
        "src/core/handoff.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Handoff JSON render helpers must not hide serialization failures.",
    ),
    must_fix(
        "NSF-CORE-HANDOFF-CAPSULE",
        "src/core/handoff.rs",
        "capsule_content).unwrap_or_default()",
        "eidetic_engine_cli-sos5.3",
        "Handoff capsule serialization failure should not become an empty capsule hash input.",
    ),
    must_fix(
        "NSF-CORE-LAB-JSON",
        "src/core/lab.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Lab report JSON helpers must not silently serialize to empty.",
    ),
    must_fix(
        "NSF-CORE-LEARN-JSON",
        "src/core/learn.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Learning report JSON helpers must not silently serialize to empty.",
    ),
    must_fix(
        "NSF-CORE-LEGACY-JSON",
        "src/core/legacy_import.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Legacy import TOON rendering must not treat failed JSON serialization as empty JSON.",
    ),
    allowed(
        "NSF-CORE-LEGACY-SKIP-DIR",
        "src/core/legacy_import.rs",
        "fn should_skip_directory",
        "A path without a UTF-8 file name cannot match a skipped legacy directory name.",
    ),
    must_fix(
        "NSF-CORE-OUTCOME-WORKSPACE",
        "src/core/outcome.rs",
        "workspace_id.unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Outcome recording should not turn a missing workspace ID into an empty persisted field.",
    ),
    must_fix(
        "NSF-CORE-PREFLIGHT-JSON",
        "src/core/preflight.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Preflight report JSON helpers must not silently serialize to empty.",
    ),
    must_fix(
        "NSF-CORE-PROCEDURE-JSON",
        "src/core/procedure.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Procedure report JSON helpers must not silently serialize to empty.",
    ),
    must_fix(
        "NSF-CORE-REHEARSE-JSON",
        "src/core/rehearse.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Rehearsal report JSON helpers must not silently serialize to empty.",
    ),
    must_fix(
        "NSF-CORE-REPRO-JSON",
        "src/core/repro.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Repro artifact JSON helpers must not silently serialize to empty.",
    ),
    must_fix(
        "NSF-CORE-TRIPWIRE-JSON",
        "src/core/tripwire.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Tripwire report JSON helpers must not silently serialize to empty.",
    ),
    allowed(
        "NSF-CASS-IMPORT-OPTIONAL-FIELDS",
        "src/cass/import.rs",
        "unwrap_or_default()",
        "CASS importer defaults here are parser policy for empty spans, unknown line types, optional counts, or fallback content hashes; malformed required JSON still errors.",
    ),
    allowed(
        "NSF-CLI-WORKSPACE-CWD",
        "src/cli/mod.rs",
        "std::env::current_dir().unwrap_or_default()",
        "CLI workspace fallback is the existing documented relative-workspace behavior; it does not convert parsed machine data to success.",
    ),
    allowed(
        "NSF-CLI-QUERY-FILTERS",
        "src/cli/mod.rs",
        "parse_filters",
        "Missing query filters are an explicit empty-filter case; malformed recognized fields are validated separately.",
    ),
    allowed(
        "NSF-CORE-BUDGET-SATURATION",
        "src/core/budget.rs",
        "unwrap_or_default()",
        "Budget clock math intentionally saturates reversed or expired durations to zero and documents that behavior.",
    ),
    allowed(
        "NSF-CORE-CAUSAL-OPTIONAL-FILTERS",
        "src/core/causal.rs",
        "unwrap_or_default()",
        "Optional memory IDs are query filters and do not represent parsed storage failure.",
    ),
    allowed(
        "NSF-CORE-CONTEXT-TAGS",
        "src/core/context.rs",
        "tags_map.get",
        "A memory with no tag rows has an explicit empty tag set.",
    ),
    allowed(
        "NSF-CORE-DOCTOR-OPTIONAL-REPAIR",
        "src/core/doctor.rs",
        "check.repair.unwrap_or_default()",
        "Doctor command text may be absent; the surrounding check still carries severity and message.",
    ),
    allowed(
        "NSF-CORE-ECONOMY-BASELINE",
        "src/core/economy.rs",
        "unwrap_or_default()",
        "No matching baseline scenario means there are no baseline artifact scores to compare.",
    ),
    allowed(
        "NSF-CORE-HANDOFF-EVIDENCE-LINKS",
        "src/core/handoff.rs",
        "get(\"kind\")",
        "Malformed optional task-frame evidence links are skipped rather than emitted as empty links.",
    ),
    allowed(
        "NSF-CORE-HANDOFF-EVIDENCE-LINK-IDS",
        "src/core/handoff.rs",
        "get(\"id\")",
        "Malformed optional task-frame evidence links are skipped rather than emitted as empty links.",
    ),
    allowed(
        "NSF-CORE-INDEX-HUMAN-DIMENSION",
        "src/core/index.rs",
        "quality_dimension.unwrap_or_default()",
        "Quality embedder dimension is optional human display text and is gated by quality model presence.",
    ),
    allowed(
        "NSF-CORE-INIT-CWD",
        "src/core/init.rs",
        "std::env::current_dir",
        "Relative init paths retain the existing workspace fallback and still render the selected path.",
    ),
    allowed(
        "NSF-CORE-INSTALL-OPTIONALS",
        "src/core/install.rs",
        "unwrap_or_default()",
        "Installer planning treats missing artifacts and PATH as empty collections without reporting a successful install.",
    ),
    allowed(
        "NSF-CORE-JSONL-IMPORT-TAGS",
        "src/core/jsonl_import.rs",
        "tags_by_memory",
        "Imported memories without tag records have an explicit empty tag set.",
    ),
    allowed(
        "NSF-CORE-LAB-OPTIONAL-FIELDS",
        "src/core/lab.rs",
        "as_deref().unwrap_or_default()",
        "Lab hash input includes optional intervention fields as empty components while retaining the surrounding structured record.",
    ),
    allowed(
        "NSF-CORE-LEARN-CWD",
        "src/core/learn.rs",
        "current_dir().unwrap_or_default()",
        "Learning path resolution keeps the existing relative path fallback and does not manufacture learned evidence.",
    ),
    allowed(
        "NSF-CORE-PLAN-RAND-ID",
        "src/core/plan.rs",
        "duration_since(SystemTime::UNIX_EPOCH)",
        "Pseudo-random fallback only handles a clock before UNIX_EPOCH and does not feed persisted evidence.",
    ),
    allowed(
        "NSF-CORE-RECORDER-CASS-CLASSIFIER",
        "src/core/recorder.rs",
        "unwrap_or_default()",
        "Recorder CASS line classification maps missing type/role to a conservative message event.",
    ),
    allowed(
        "NSF-CORE-REPRO-MISSING-HASH",
        "src/core/repro.rs",
        "expected_artifacts",
        "A missing expected hash is paired with a failed verification result, not a successful empty hash.",
    ),
    allowed(
        "NSF-CORE-SEARCH-OPTIONAL-DETAIL",
        "src/core/search.rs",
        "last_check_error",
        "Absent index-check detail appends no extra sentence while preserving the high-severity corruption signal.",
    ),
    allowed(
        "NSF-DB-FEEDBACK-SIGNAL",
        "src/db/mod.rs",
        "optional_text(row, 0)?.unwrap_or_default()",
        "Missing feedback signal maps to no positive/negative bucket and does not create a successful signal.",
    ),
    allowed(
        "NSF-MODELS-DEMO-OPTIONALS",
        "src/models/demo.rs",
        "unwrap_or_default()",
        "Demo fixtures use empty optional descriptions and values for human demonstration metadata only.",
    ),
    allowed(
        "NSF-POLICY-ENV-PROFILE",
        "src/policy/security_profile.rs",
        "EE_SECURITY_PROFILE",
        "Absent or invalid environment profile intentionally falls back to the default security profile.",
    ),
    allowed(
        "NSF-STEWARD-RESOURCE-SUMMARY",
        "src/steward/mod.rs",
        "consumption",
        "No recorded consumption for a budgeted resource means zero consumed, not hidden failed I/O.",
    ),
];

const MANUAL_FINDINGS: &[ManualFinding] = &[ManualFinding {
    id: "NSF-OUTPUT-INTEGRITY-PROVENANCE-SAMPLE",
    file: "src/output/mod.rs",
    fragment: "provenanceSample",
    follow_up: "eidetic_engine_cli-sos5.7",
    reason: "Integrity diagnostics can fabricate an empty provenance sample when none was collected.",
}];

const REQUIRED_SURFACE_FILES: &[&str] = &[
    "src/cass/process.rs",
    "src/output/mod.rs",
    "src/hooks/installer.rs",
    "src/models/jsonl.rs",
];

const fn must_fix(
    id: &'static str,
    file: &'static str,
    fragment: &'static str,
    follow_up: &'static str,
    reason: &'static str,
) -> InventoryRule {
    InventoryRule {
        id,
        file,
        fragment,
        disposition: Disposition::MustFix,
        follow_up: Some(follow_up),
        reason,
    }
}

const fn allowed(
    id: &'static str,
    file: &'static str,
    fragment: &'static str,
    reason: &'static str,
) -> InventoryRule {
    InventoryRule {
        id,
        file,
        fragment,
        disposition: Disposition::Allowed,
        follow_up: None,
        reason,
    }
}

#[test]
fn no_silent_fallback_inventory_covers_current_source_findings() -> TestResult {
    let findings = scan_source_findings()?;
    let mut uncovered = Vec::new();

    for finding in &findings {
        if classify_finding(finding).is_none() {
            uncovered.push(format!(
                "{}:{} `{}`\ncontext:\n{}",
                finding.file, finding.line, finding.text, finding.context
            ));
        }
    }

    if uncovered.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "unclassified production fallback(s):\n{}\n\nRepair: return a contextual error/degradation or add a justified inventory entry with a follow-up bead.",
            uncovered.join("\n\n")
        ))
    }
}

#[test]
fn no_silent_fallback_must_fix_entries_have_follow_up_beads() -> TestResult {
    let mut missing = Vec::new();

    for rule in INVENTORY_RULES {
        if rule.disposition == Disposition::MustFix {
            match rule.follow_up {
                Some(bead) if FOLLOW_UP_BEADS.contains(&bead) => {}
                Some(bead) => missing.push(format!(
                    "{} references unknown follow-up `{bead}`: {}",
                    rule.id, rule.reason
                )),
                None => missing.push(format!(
                    "{} has no follow-up bead: {}",
                    rule.id, rule.reason
                )),
            }
        }
    }

    for finding in MANUAL_FINDINGS {
        if !FOLLOW_UP_BEADS.contains(&finding.follow_up) {
            missing.push(format!(
                "{} references unknown follow-up `{}`: {}",
                finding.id, finding.follow_up, finding.reason
            ));
        }
    }

    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing.join("\n"))
    }
}

#[test]
fn no_silent_fallback_inventory_covers_required_surfaces() -> TestResult {
    let mut missing = Vec::new();
    for required in REQUIRED_SURFACE_FILES {
        if !INVENTORY_RULES.iter().any(|rule| rule.file == *required) {
            missing.push(*required);
        }
    }

    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "required no-silent-fallback inventory surface(s) missing: {}",
            missing.join(", ")
        ))
    }
}

#[test]
fn no_silent_fallback_manual_findings_still_point_at_real_code() -> TestResult {
    let mut missing = Vec::new();
    for finding in MANUAL_FINDINGS {
        let path = repo_path(finding.file);
        let source = fs::read_to_string(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        if !source.contains(finding.fragment) {
            missing.push(format!(
                "{} missing `{}` in {}",
                finding.id, finding.fragment, finding.file
            ));
        }
    }

    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing.join("\n"))
    }
}

#[test]
fn no_silent_fallback_guard_rejects_new_unclassified_renderer_default() -> TestResult {
    let synthetic = SourceFinding {
        file: "src/output/new_renderer.rs".to_owned(),
        line: 1,
        text: "serde_json::to_string(report).unwrap_or_default()".to_owned(),
        context: "serde_json::to_string(report).unwrap_or_default()".to_owned(),
    };

    if classify_finding(&synthetic).is_none() {
        Ok(())
    } else {
        Err("synthetic unclassified renderer fallback was unexpectedly allowlisted".to_owned())
    }
}

fn classify_finding(finding: &SourceFinding) -> Option<&'static InventoryRule> {
    INVENTORY_RULES
        .iter()
        .find(|rule| rule.file == finding.file && finding.context.contains(rule.fragment))
}

fn scan_source_findings() -> Result<Vec<SourceFinding>, String> {
    let mut files = Vec::new();
    collect_rust_files(&repo_path("src"), &mut files)?;
    files.sort();

    let mut findings = Vec::new();
    for path in files {
        let source = fs::read_to_string(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        let relative = relative_path(&path)?;
        let ignored = ignored_test_module_lines(&source);
        let lines = source.lines().collect::<Vec<_>>();

        for (index, line) in lines.iter().enumerate() {
            if ignored[index] || !is_high_risk_line(line) {
                continue;
            }
            findings.push(SourceFinding {
                file: relative.clone(),
                line: index + 1,
                text: line.trim().to_owned(),
                context: context_window(&lines, index),
            });
        }
    }

    Ok(findings)
}

fn collect_rust_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in
        fs::read_dir(dir).map_err(|error| format!("failed to read {}: {error}", dir.display()))?
    {
        let entry = entry.map_err(|error| format!("failed to read dir entry: {error}"))?;
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    Ok(())
}

fn ignored_test_module_lines(source: &str) -> Vec<bool> {
    let lines = source.lines().collect::<Vec<_>>();
    let mut ignored = vec![false; lines.len()];
    let mut pending_cfg_test = false;
    let mut in_test_module = false;
    let mut brace_depth = 0_i32;

    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if in_test_module {
            ignored[index] = true;
            brace_depth += brace_delta(line);
            if brace_depth <= 0 {
                in_test_module = false;
            }
            continue;
        }

        if pending_cfg_test && trimmed.starts_with("mod tests") && trimmed.contains('{') {
            ignored[index] = true;
            in_test_module = true;
            brace_depth = brace_delta(line);
            pending_cfg_test = false;
            if brace_depth <= 0 {
                in_test_module = false;
            }
            continue;
        }

        if trimmed == "#[cfg(test)]" {
            pending_cfg_test = true;
        } else if pending_cfg_test
            && !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && !trimmed.starts_with("//")
        {
            pending_cfg_test = false;
        }
    }

    ignored
}

fn brace_delta(line: &str) -> i32 {
    line.chars().fold(0_i32, |depth, ch| match ch {
        '{' => depth + 1,
        '}' => depth - 1,
        _ => depth,
    })
}

fn is_high_risk_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.contains(".unwrap_or_default()")
        || (trimmed.starts_with("let _ =") && trimmed.contains("read_to_end"))
        || trimmed.contains("join().unwrap_or_default()")
}

fn context_window(lines: &[&str], index: usize) -> String {
    let start = index.saturating_sub(4);
    let end = (index + 5).min(lines.len());
    lines[start..end]
        .iter()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join("\n")
}

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn relative_path(path: &Path) -> Result<String, String> {
    let root = repo_path("");
    let relative = path
        .strip_prefix(&root)
        .map_err(|error| format!("failed to relativize {}: {error}", path.display()))?;
    Ok(relative.to_string_lossy().replace('\\', "/"))
}
