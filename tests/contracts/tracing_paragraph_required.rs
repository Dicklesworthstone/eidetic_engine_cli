//! bd-3usjw.76 -- contract coverage for TRACING closeout paragraphs.
//!
//! The Part II tracing-field gate depends on open/blocked `bd-3usjw.*`
//! surfaces declaring the fields and phases that must be emitted before
//! closeout. This test reads the git-friendly Beads export instead of
//! linking a second tracker database client into the test binary.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const ISSUES_JSONL: &str = ".beads/issues.jsonl";
const CONTRACTS_RS: &str = "tests/contracts.rs";
const VERIFY_SH: &str = "scripts/verify.sh";
const WHITELIST_PATH: &str = "tests/contracts/tracing_paragraph_whitelist.toml";

const TARGET_BEADS: &[&str] = &["bd-3usjw.50", "bd-3usjw.56", "bd-3usjw.69"];

const REQUIRED_FIELDS: &[&str] = &[
    "workspace_id",
    "request_id",
    "bead_id",
    "surface",
    "phase",
    "elapsed_ms",
    "degraded_codes",
];

const STANDARD_PHASES: &[&str] = &[
    "dependency_check",
    "dispatch",
    "input",
    "persistence",
    "response",
];

#[derive(Debug)]
struct IssueRow {
    id: String,
    title: String,
    status: String,
    labels: Vec<String>,
    body: String,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn read_issues() -> Result<BTreeMap<String, IssueRow>, String> {
    let path = repo_root().join(ISSUES_JSONL);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let mut issues = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(trimmed)
            .map_err(|error| format!("parse {} line {}: {error}", path.display(), index + 1))?;
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{} line {} missing string id", path.display(), index + 1))?
            .to_owned();
        let title = value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let labels = value
            .get("labels")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let body = ["description", "design", "acceptance_criteria", "notes"]
            .iter()
            .filter_map(|field| value.get(*field).and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        issues.insert(
            id.clone(),
            IssueRow {
                id,
                title,
                status,
                labels,
                body,
            },
        );
    }
    Ok(issues)
}

fn read_repo_file(relative_path: &str) -> Result<String, String> {
    let path = repo_root().join(relative_path);
    fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn load_whitelist() -> Result<BTreeSet<String>, String> {
    let path = repo_root().join(WHITELIST_PATH);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    let array = document["tracing_paragraph"]["whitelisted_issue_ids"]
        .as_array()
        .ok_or_else(|| {
            format!(
                "{} must define [tracing_paragraph].whitelisted_issue_ids",
                path.display()
            )
        })?;
    let mut ids = BTreeSet::new();
    for value in array {
        let id = value
            .as_str()
            .ok_or_else(|| format!("{} whitelist contains non-string entry", path.display()))?;
        ids.insert(id.to_owned());
    }
    Ok(ids)
}

fn has_tracing_heading(body: &str) -> bool {
    body.lines().any(|line| {
        let trimmed = line.trim();
        trimmed.eq_ignore_ascii_case("## tracing")
            || trimmed.to_ascii_lowercase().starts_with("## tracing ")
            || trimmed.to_ascii_lowercase().starts_with("tracing:")
    })
}

fn missing_terms<'a>(body: &str, terms: &'a [&'a str]) -> Vec<&'a str> {
    terms
        .iter()
        .copied()
        .filter(|term| !body.contains(term))
        .collect()
}

fn require_tracing_contract(issue: &IssueRow) -> TestResult {
    ensure(
        has_tracing_heading(&issue.body),
        format!("{} must include a TRACING heading", issue.id),
    )?;
    let missing_fields = missing_terms(&issue.body, REQUIRED_FIELDS);
    ensure(
        missing_fields.is_empty(),
        format!(
            "{} TRACING paragraph is missing required fields: {missing_fields:?}",
            issue.id
        ),
    )?;
    let missing_phases = missing_terms(&issue.body, STANDARD_PHASES);
    ensure(
        missing_phases.is_empty(),
        format!(
            "{} TRACING paragraph is missing standard phases: {missing_phases:?}",
            issue.id
        ),
    )
}

fn requires_tracing_paragraph(issue: &IssueRow) -> bool {
    if !issue.id.starts_with("bd-3usjw.") {
        return false;
    }
    if issue.status != "open" && issue.status != "blocked" {
        return false;
    }
    let implements_surface = issue
        .labels
        .iter()
        .any(|label| label.starts_with("implements-surface:"))
        || issue.title.contains("implements-surface:");
    let body_lower = issue.body.to_ascii_lowercase();
    let has_file_surface = body_lower.contains("file surface")
        && (issue.body.contains("src/") || issue.body.contains("tests/"));
    implements_surface || has_file_surface
}

#[test]
fn target_part_ii_beads_have_required_tracing_paragraphs() -> TestResult {
    let issues = read_issues()?;
    for id in TARGET_BEADS {
        let issue = issues
            .get(*id)
            .ok_or_else(|| format!("target bead {id} missing from {ISSUES_JSONL}"))?;
        require_tracing_contract(issue)?;
    }
    Ok(())
}

#[test]
fn open_part_ii_surface_beads_have_tracing_or_whitelist_entry() -> TestResult {
    let issues = read_issues()?;
    let whitelist = load_whitelist()?;
    let mut violations = Vec::new();
    for issue in issues
        .values()
        .filter(|issue| requires_tracing_paragraph(issue))
    {
        if whitelist.contains(&issue.id) {
            continue;
        }
        if let Err(error) = require_tracing_contract(issue) {
            violations.push(error);
        }
    }
    ensure(
        violations.is_empty(),
        format!(
            "open/blocked bd-3usjw surface beads without TRACING contract: {}",
            violations.join("; ")
        ),
    )
}

#[test]
fn tracing_whitelist_only_mentions_existing_part_ii_beads() -> TestResult {
    let issues = read_issues()?;
    let whitelist = load_whitelist()?;
    for id in &whitelist {
        let issue = issues
            .get(id)
            .ok_or_else(|| format!("whitelist references unknown bead {id}"))?;
        ensure(
            issue.id.starts_with("bd-3usjw."),
            format!("whitelist entry {id} must stay scoped to bd-3usjw.*"),
        )?;
    }
    Ok(())
}

#[test]
fn verify_stage_reaches_tracing_paragraph_contract() -> TestResult {
    let contracts = read_repo_file(CONTRACTS_RS)?;
    ensure(
        contracts.contains("contracts/tracing_paragraph_required.rs"),
        "tests/contracts.rs must include tracing_paragraph_required",
    )?;
    let verify = read_repo_file(VERIFY_SH)?;
    ensure(
        verify.contains("cargo test --workspace --lib --bins --tests --examples"),
        "scripts/verify.sh must keep the all-tests stage that runs tests/contracts.rs",
    )?;
    ensure(
        verify
            .contains("cargo test --workspace --lib --bins --tests --examples -- --test-threads=1"),
        "scripts/verify.sh must serialize the all-tests stage because the suite contains process-global fixtures",
    )
}
