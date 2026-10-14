//! Pytest collection failures use module coverage, not a fictitious test list.
//!
//! Corroborate named collector headings with the short summary, error counter
//! and collection-interruption trailer. A later complete named execution must
//! actually pass a test in every failed module. Exact invocation/context and
//! independent process completion remain enforced by the shared caller.
//! Traceback frames, exception messages, locals and source code are not read.

use std::collections::BTreeSet;

use super::{
    Decoded, Execution, MAX_DIAGNOSTICS_PER_FAILURE, MAX_LINES, MAX_SCANNED_OUTPUT_BYTES,
    MAX_TESTS, Subject, banner, diagnostic, progress, terminal, totals,
};

pub(super) struct Requirement {
    modules: BTreeSet<Subject>,
}

impl Requirement {
    pub(super) fn verified_by(&self, passed_modules: &BTreeSet<Subject>) -> bool {
        !self.modules.is_empty() && self.modules.is_subset(passed_modules)
    }
}

pub(super) fn module_subject(module: &str) -> Subject {
    let mut hash = blake3::Hasher::new();
    hash.update(b"ee.cass.pytest.collection_module.v1\0");
    hash.update(&(module.len() as u64).to_le_bytes());
    hash.update(module.as_bytes());
    *hash.finalize().as_bytes()
}

fn module_path(path: &str) -> bool {
    path.ends_with(".py")
        && path.len() > 3
        && path == path.trim()
        && !path.contains("::")
        && !path.chars().any(char::is_control)
}

fn heading(line: &str) -> Option<&str> {
    if !line.starts_with("___") || !line.ends_with("___") {
        return None;
    }
    line.trim_matches('_')
        .trim()
        .strip_prefix("ERROR collecting ")
}

fn unsigned(text: &str) -> Option<u64> {
    (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

fn errors(text: &str) -> Option<u64> {
    let (number, label) = text.split_once(' ')?;
    let count = unsigned(number)?;
    (count > 0 && label == if count == 1 { "error" } else { "errors" }).then_some(count)
}

fn interrupted_count(line: &str) -> Option<u64> {
    if !line.starts_with("!!!") || !line.ends_with("!!!") {
        return None;
    }
    let text = line.trim_matches('!').trim();
    errors(
        text.strip_prefix("Interrupted: ")?
            .strip_suffix(" during collection")?,
    )
}

/// Validate the optional verbose collection counters without deriving any test
/// identities from them. Quiet reports omit this line altogether.
fn collected_errors(line: &str) -> Option<u64> {
    let text = line
        .strip_prefix("collecting ... collected ")
        .or_else(|| line.strip_prefix("collected "))?;
    let mut parts = text.split(" / ");
    let (number, label) = parts.next()?.split_once(' ')?;
    let count = unsigned(number)?;
    if label != if count == 1 { "item" } else { "items" } {
        return None;
    }
    let mut error_count = None;
    let mut excluded = None;
    let mut selected = None;
    for part in parts {
        if let Some(errors) = errors(part) {
            if error_count.replace(errors).is_some() {
                return None;
            }
        } else if let Some(number) = part.strip_suffix(" deselected") {
            if excluded.replace(unsigned(number)?).is_some() {
                return None;
            }
        } else if let Some(number) = part.strip_suffix(" selected") {
            if selected.replace(unsigned(number)?).is_some() {
                return None;
            }
        } else {
            return None;
        }
    }
    if excluded.is_some_and(|excluded| excluded > count)
        || selected
            .is_some_and(|selected| selected.checked_add(excluded.unwrap_or(0)) != Some(count))
    {
        return None;
    }
    error_count
}

pub(super) fn decode(text: &str) -> Result<Option<Decoded>, ()> {
    // No full normalization/scan for unrelated output or ordinary green tests.
    // A quoted collection message alone cannot select this producer.
    if !text.contains("ERROR collecting ") {
        return Ok(None);
    }
    let first = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let first = match terminal::normalize(first) {
        Ok(first) => first,
        // A damaged native header is not an unrelated producer. In particular,
        // a quiet ERRORS header must not fall through to generic stdout merely
        // because terminal erasure made its visible interpretation uncertain.
        Err(()) if first.contains("test session starts") || first.contains("ERRORS") => {
            return Err(());
        }
        Err(()) => return Ok(None),
    };
    if !matches!(banner(&first), Some("test session starts" | "ERRORS")) {
        return Ok(None);
    }
    if text.len() > MAX_SCANNED_OUTPUT_BYTES || text.lines().count() > MAX_LINES {
        return Err(());
    }
    let visible = terminal::normalize(text)?;
    let lines: Vec<_> = visible.lines().map(str::trim_end).collect();
    let first_index = lines
        .iter()
        .position(|line| !line.trim().is_empty())
        .ok_or(())?;
    let last = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .ok_or(())?;
    let footer = totals(lines[last]);
    // Mixed execution/collection reports retain the original test decoder.
    // This slice handles collection-aborted runs, not partial test inventories.
    if lines.iter().any(|line| progress(line).is_some())
        || footer.as_ref().is_some_and(|footer| {
            ["passed", "failed", "skipped", "xfailed", "xpassed"]
                .iter()
                .any(|name| footer.get(name) > 0)
        })
    {
        return Ok(None);
    }

    let mut modules = BTreeSet::new();
    let mut summaries = BTreeSet::new();
    let mut in_errors = false;
    let mut errors_seen = false;
    let mut in_summary = false;
    let mut summary_seen = false;
    let mut interrupted = None;
    let mut collected = None;
    for (index, line) in lines.iter().enumerate() {
        if line.trim().is_empty() || (index == last && footer.is_some()) {
            continue;
        }
        if totals(line).is_some() {
            return Err(());
        }
        if interrupted.is_some() {
            // Only the final summary may follow the collection interruption.
            // Do not let an unknown later banner conceal extra output.
            return Err(());
        }
        if let Some(label) = banner(line) {
            match label {
                "test session starts" if index == first_index => {}
                "test session starts" => return Err(()),
                "ERRORS" if !errors_seen && !summary_seen => {
                    errors_seen = true;
                    in_errors = true;
                }
                "short test summary info" if errors_seen && !summary_seen => {
                    summary_seen = true;
                    in_errors = false;
                    in_summary = true;
                }
                "ERRORS" | "short test summary info" => return Err(()),
                _ => {
                    in_errors = false;
                    in_summary = false;
                }
            }
            continue;
        }
        if line.starts_with("!!!") {
            if !summary_seen || interrupted.is_some() {
                return Err(());
            }
            interrupted = Some(interrupted_count(line).ok_or(())?);
            in_summary = false;
            continue;
        }
        if !errors_seen
            && (line.starts_with("collected ") || line.starts_with("collecting ... collected "))
        {
            if collected
                .replace(collected_errors(line).ok_or(())?)
                .is_some()
            {
                return Err(());
            }
        }
        if in_errors && let Some(module) = heading(line) {
            if !module_path(module) || modules.len() == MAX_TESTS || !modules.insert(module) {
                return Err(());
            }
        }
        if in_summary {
            let rest = line.strip_prefix("ERROR ").ok_or(())?;
            let module = rest.split_once(" - ").map_or(rest, |(module, _)| module);
            if !module_path(module) || !modules.contains(module) || !summaries.insert(module) {
                return Err(());
            }
        }
    }
    if modules.is_empty() {
        return Ok(None);
    }
    let count = modules.len() as u64;
    if collected.is_some_and(|errors| errors < count)
        || footer
            .as_ref()
            .is_some_and(|footer| footer.get("error") < count)
        || interrupted.is_some_and(|errors| errors < count)
    {
        return Err(());
    }
    // A partial poll may record the observed failure class, but never certify
    // coverage. Only the completed, mutually consistent report earns a module
    // requirement. Do not turn unknown collection into generic command proof.
    let complete = summaries == modules
        && interrupted == Some(count)
        && collected.is_none_or(|errors| errors == count)
        && footer
            .as_ref()
            .is_some_and(|footer| footer.get("error") == count);
    let execution = Execution {
        collection_failure: complete.then(|| Requirement {
            modules: modules
                .iter()
                .map(|module| module_subject(module))
                .collect(),
        }),
        ..Execution::default()
    };
    Ok(Some(Decoded {
        diagnostics: modules
            .iter()
            .take(MAX_DIAGNOSTICS_PER_FAILURE)
            .map(|module| diagnostic(module))
            .collect(),
        symptom: modules.first().map(|module| {
            crate::policy::redact_secret_like_content(&format!(
                "pytest collection failed in {module}"
            ))
            .content
        }),
        failed: true,
        allows_completion: false,
        execution,
    }))
}

#[cfg(test)]
mod tests {
    use super::super::decode as python_decode;
    use super::*;
    use crate::core::cass_error_recall::test_coverage::{Proof, Requirement as TestRequirement};
    use crate::core::cass_error_recall::{ToolOutput, structured_error_diagnostics};

    fn report(paths: &[&str], quiet: bool) -> String {
        let count = paths.len();
        let label = if count == 1 { "error" } else { "errors" };
        let mut text = if quiet {
            String::new()
        } else {
            format!("=== test session starts ===\ncollected 0 items / {count} {label}\n")
        };
        text.push_str("=== ERRORS ===\n");
        for path in paths {
            text.push_str(&format!(
                "___ ERROR collecting {path} ___\nTraceback:\nE   ImportError: private traceback sentinel\n"
            ));
        }
        text.push_str("=== short test summary info ===\n");
        for path in paths {
            text.push_str(&format!("ERROR {path}\n"));
        }
        text.push_str(&format!(
            "!!! Interrupted: {count} {label} during collection !!!\n=== {count} {label} in 0.01s ===\n"
        ));
        text
    }

    fn green(paths: &[&str]) -> String {
        let count = paths.len();
        let label = if count == 1 { "item" } else { "items" };
        let mut text = format!("=== test session starts ===\ncollected {count} {label}\n");
        for path in paths {
            text.push_str(&format!("{path}::test_repaired PASSED [100%]\n"));
        }
        text.push_str(&format!("=== {count} passed in 0.01s ===\n"));
        text
    }

    #[test]
    fn collection_repairs_require_every_failed_module_to_execute_a_passing_test() {
        let paths = ["tests/test_queue.py", "tests/test_cache.py"];
        for quiet in [false, true] {
            let failure = python_decode(&report(&paths, quiet)).unwrap().unwrap();
            assert!(failure.failed && !failure.allows_completion);
            assert!(failure.execution.has_known_failures());
            let repaired = python_decode(&green(&paths)).unwrap().unwrap();
            assert!(repaired.allows_completion);
            assert!(failure.execution.verified_by(&repaired.execution));
            let partial = python_decode(&green(&paths[..1])).unwrap().unwrap();
            assert!(!failure.execution.verified_by(&partial.execution));
        }
    }

    #[test]
    fn collection_fingerprints_match_public_diagnosis_and_keep_tracebacks_private() {
        let full = report(&["tests/test_queue.py"], false);
        let quiet = report(&["tests/test_queue.py"], true);
        let expected = diagnostic("tests/test_queue.py");
        for text in [full, quiet] {
            let decoded = python_decode(&text).unwrap().unwrap();
            assert_eq!(decoded.diagnostics, std::slice::from_ref(&expected));
            assert_eq!(structured_error_diagnostics(&text), decoded.diagnostics);
            assert!(
                !decoded
                    .symptom
                    .unwrap()
                    .contains("private traceback sentinel")
            );
            assert!(
                !decoded.diagnostics[0]
                    .message_template
                    .contains("private traceback sentinel")
            );
        }
    }

    #[test]
    fn skipped_unrelated_incomplete_and_quiet_success_cannot_certify_collection() {
        let failure = python_decode(&report(&["tests/test_queue.py"], false))
            .unwrap()
            .unwrap();
        let good = green(&["tests/test_queue.py"]);
        for text in [
            good.replace("PASSED", "SKIPPED")
                .replace("1 passed", "1 skipped"),
            good.replace("PASSED", "XFAIL")
                .replace("1 passed", "1 xfailed"),
            good.replace("tests/test_queue.py", "other/test_queue.py"),
            good.replace("test_queue.py", "test_Queue.py"),
            good.replace("=== 1 passed in 0.01s ===", ""),
            ". [100%]\n1 passed in 0.01s\n".to_owned(),
        ] {
            let proof = python_decode(&text).unwrap().unwrap();
            assert!(!failure.execution.verified_by(&proof.execution), "{text}");
        }
    }

    #[test]
    fn partial_collection_reports_never_earn_repair_coverage() {
        let full = report(&["tests/test_queue.py"], false);
        for end in [
            "=== short test summary info ===",
            "!!! Interrupted:",
            "=== 1 error in",
        ] {
            let partial = full.split_once(end).unwrap().0;
            let decoded = python_decode(partial).unwrap().unwrap();
            assert!(decoded.failed);
            assert!(!decoded.execution.has_known_failures());
            assert!(!decoded.allows_completion);
        }
    }

    #[test]
    fn inconsistent_native_collection_reports_cannot_supply_proof() {
        let full = report(&["tests/test_queue.py"], false);
        for invalid in [
            full.replace("ERROR tests/test_queue.py\n", "ERROR tests/test_other.py\n"),
            full.replace(
                "ERROR tests/test_queue.py\n",
                "ERROR tests/test_queue.py\nERROR tests/test_queue.py\n",
            ),
            full.replace("=== 1 error in", "=== 0 errors in"),
            full.replace(
                "Interrupted: 1 error during collection",
                "KeyboardInterrupt",
            ),
            format!("{full}Process exited with code 1\n"),
            format!("{full}{full}"),
        ] {
            assert!(python_decode(&invalid).is_err(), "{invalid}");
        }
        assert!(
            decode("example: ___ ERROR collecting tests/test_queue.py ___")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn completed_report_does_not_replace_the_independent_process_observation() {
        let failure = ToolOutput {
            text: report(&["tests/test_queue.py"], false),
            is_error: Some(true),
            exit_code: Some(2),
        };
        let requirement = TestRequirement::capture(&failure, true);
        let mut proof = ToolOutput {
            text: green(&["tests/test_queue.py"]),
            is_error: None,
            exit_code: None,
        };
        assert!(!requirement.is_verified_by(&Proof::capture(&proof)));
        proof.exit_code = Some(0);
        assert!(requirement.is_verified_by(&Proof::capture(&proof)));
        proof.exit_code = Some(1);
        assert!(!requirement.is_verified_by(&Proof::capture(&proof)));
        let mut pending = TestRequirement::capture(&failure, false);
        proof.exit_code = Some(0);
        assert!(!pending.is_verified_by(&Proof::capture(&proof)));
        pending.refresh(&failure, true);
        assert!(pending.is_verified_by(&Proof::capture(&proof)));
    }

    #[test]
    fn every_failed_module_is_required_beyond_the_diagnostic_display_limit() {
        let paths: Vec<_> = (0..6)
            .map(|index| format!("tests/test_{index}.py"))
            .collect();
        let paths: Vec<_> = paths.iter().map(String::as_str).collect();
        let failure = python_decode(&report(&paths, false)).unwrap().unwrap();
        assert_eq!(failure.diagnostics.len(), MAX_DIAGNOSTICS_PER_FAILURE);
        let partial = python_decode(&green(&paths[..5])).unwrap().unwrap();
        assert!(!failure.execution.verified_by(&partial.execution));
        let all = python_decode(&green(&paths)).unwrap().unwrap();
        assert!(failure.execution.verified_by(&all.execution));
    }

    #[test]
    fn styling_and_redaction_preserve_private_exact_module_bindings() {
        let secret = "ghp_0123456789abcdef0123456789abcdef01234567";
        let path = format!("tests/{secret}/test_資料.py");
        let failure = report(&[&path], true);
        let styled = failure
            .lines()
            .map(|line| format!("\u{1b}[31m{line}\u{1b}[0m"))
            .collect::<Vec<_>>()
            .join("\r\n");
        let decoded = python_decode(&styled).unwrap().unwrap();
        assert!(decoded.execution.has_known_failures());
        assert_eq!(
            decoded.diagnostics,
            python_decode(&failure).unwrap().unwrap().diagnostics
        );
        assert!(!decoded.symptom.as_deref().unwrap().contains(secret));
        assert!(!format!("{:?}", decoded.diagnostics).contains(secret));
        let good = python_decode(&green(&[&path])).unwrap().unwrap();
        assert!(decoded.execution.verified_by(&good.execution));
        let other = path.replace("ghp_", "gho_");
        let other = python_decode(&green(&[&other])).unwrap().unwrap();
        assert!(!decoded.execution.verified_by(&other.execution));
        assert!(python_decode(&failure.replace("ERRORS", "\u{1b}[2JERRORS")).is_err());
    }

    mod store {
        use super::{green, report};
        use crate::core::cass_error_recall::{
            record_session_error_recall, structured_error_diagnostics,
        };
        use crate::core::error_diagnosis::{
            error_recall_report, pack_error_recall_query_seed, recalled_repair_evidence,
        };
        use crate::db::{
            CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, DbConnection,
            EvidenceProducerKind,
        };
        use serde_json::{Value, json};

        type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
        const WS: &str = "wsp_01234567890123456789012345";
        const COMMAND: &str = "pytest -v --color=no tests";
        const MODULE: &str = "tests/test_queue.py";
        const REPAIR: &str = "The pytest collection failed in tests/test_queue.py because Queue was imported from an obsolete module. I fixed tests/test_queue.py by importing Queue from queue_backend.";

        fn id(number: u32) -> String {
            crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(9700 + u128::from(number)))
                .to_string()
        }

        fn insert(
            db: &DbConnection,
            session: &str,
            number: u32,
            kind: &str,
            role: &str,
            block: Value,
        ) -> TestResult {
            let excerpt =
                json!({"type":role,"message":{"role":role,"content":[block]}}).to_string();
            db.insert_evidence_span(
                &id(number),
                &CreateEvidenceSpanInput {
                    workspace_id: WS.to_owned(),
                    session_id: session.to_owned(),
                    memory_id: None,
                    producer_kind: EvidenceProducerKind::CassImport,
                    cass_span_id: format!("{session}:{number}"),
                    span_kind: kind.to_owned(),
                    start_line: number,
                    end_line: number,
                    start_byte: None,
                    end_byte: None,
                    role: Some(role.to_owned()),
                    content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
                    excerpt,
                    metadata_json: None,
                    inherited_redaction_classes: Vec::new(),
                },
            )?;
            Ok(())
        }

        fn episode(proof: &str, proof_command: &str) -> TestResult<(DbConnection, String)> {
            let db = DbConnection::open_memory()?;
            db.migrate()?;
            db.insert_workspace(
                WS,
                &CreateWorkspaceInput {
                    path: "/tmp/pytest-collection-recall".to_owned(),
                    name: None,
                },
            )?;
            let session =
                crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(9690)).to_string();
            db.insert_session(
                &session,
                &CreateSessionInput {
                    workspace_id: WS.to_owned(),
                    cass_session_id: "/sessions/pytest-collection.jsonl".to_owned(),
                    source_path: None,
                    agent_name: Some("claude_code".to_owned()),
                    model: None,
                    started_at: None,
                    ended_at: None,
                    message_count: 5,
                    token_count: None,
                    content_hash: format!(
                        "blake3:{}",
                        blake3::hash(b"pytest-collection-recall").to_hex()
                    ),
                    metadata_json: None,
                },
            )?;
            for (offset, (kind, role, block)) in [
                (
                    "tool_call",
                    "assistant",
                    json!({"type":"tool_use","id":"failed","name":"Bash",
                        "input":{"command":COMMAND}}),
                ),
                (
                    "tool_result",
                    "user",
                    json!({"type":"tool_result","tool_use_id":"failed",
                        "content":report(&[MODULE], false),"is_error":true}),
                ),
                ("message", "assistant", json!({"type":"text","text":REPAIR})),
                (
                    "tool_call",
                    "assistant",
                    json!({"type":"tool_use","id":"retry","name":"Bash",
                        "input":{"command":proof_command}}),
                ),
                (
                    "tool_result",
                    "user",
                    json!({"type":"tool_result","tool_use_id":"retry",
                        "content":proof,"is_error":false}),
                ),
            ]
            .into_iter()
            .enumerate()
            {
                insert(&db, &session, u32::try_from(offset + 1)?, kind, role, block)?;
            }
            Ok((db, session))
        }

        #[test]
        fn collection_repair_populates_cited_recall_only_after_the_module_executes() -> TestResult {
            let good = green(&[MODULE]);
            let skipped = good
                .replace("PASSED", "SKIPPED")
                .replace("1 passed", "1 skipped");
            let (db, session) = episode(&skipped, COMMAND)?;
            let first = record_session_error_recall(&db, WS, &session)?;
            assert_eq!(first.failures_seen, 1);
            assert_eq!(first.resolved_failures, 0);
            assert_eq!(first.repair_links_recorded, 0);
            assert_eq!(first.incident_cards_recorded, 0);
            let diagnostic = structured_error_diagnostics(&report(&[MODULE], true))
                .into_iter()
                .next()
                .ok_or("collection fingerprint")?;
            let recalled = error_recall_report(&db, WS, &diagnostic)?;
            assert!(recalled.exact);
            assert!(recalled.helpful_repairs.is_empty() && recalled.proof_links.is_empty());
            insert(
                &db,
                &session,
                6,
                "tool_call",
                "assistant",
                json!({"type":"tool_use","id":"verified","name":"Bash",
                    "input":{"command":COMMAND}}),
            )?;
            insert(
                &db,
                &session,
                7,
                "tool_result",
                "user",
                json!({"type":"tool_result","tool_use_id":"verified",
                    "content":good,"is_error":false}),
            )?;
            let before = db.list_evidence_spans_for_session(&session)?;
            let result = record_session_error_recall(&db, WS, &session)?;
            assert_eq!(result.failures_seen, 1);
            assert_eq!(result.fingerprints_recorded, 1);
            assert_eq!(result.resolved_failures, 1);
            assert_eq!(result.repair_links_recorded, 2);
            assert_eq!(result.incident_cards_recorded, 1);
            let card = crate::core::incident_card::incident_card_id(WS, &id(2));
            let recalled = error_recall_report(&db, WS, &diagnostic)?;
            assert!(recalled.helpful_repairs.contains(&id(3)));
            assert!(recalled.helpful_repairs.contains(&card));
            assert_eq!(recalled.proof_links, [id(7)]);
            let links = db.list_error_repair_links(WS, &diagnostic.layered_key().key)?;
            assert_eq!(links.len(), 3);
            let failure_id = id(2);
            assert!(
                links
                    .iter()
                    .all(|link| link.evidence_ref.as_deref() == Some(failure_id.as_str()))
            );
            let evidence = recalled_repair_evidence(&db, WS, &recalled)?;
            assert!(evidence.iter().any(|item| {
                item.evidence_id == card
                    && item.role == "incident_card"
                    && item
                        .text
                        .as_deref()
                        .is_some_and(|text| text.contains("Fix:"))
            }));
            assert!(evidence.iter().all(|item| {
                item.text
                    .as_deref()
                    .is_none_or(|text| !text.contains("private traceback sentinel"))
            }));
            let seed = pack_error_recall_query_seed(&db, WS, &diagnostic)?;
            assert!(seed.contains("prior fix:") && seed.contains("queue_backend"));
            for original in before {
                let after = db
                    .get_evidence_span(&original.id)?
                    .ok_or("source preserved")?;
                assert_eq!(after.excerpt, original.excerpt);
                assert_eq!(after.content_hash, original.content_hash);
                assert_eq!(
                    after.canonical_provenance_uri(),
                    original.canonical_provenance_uri()
                );
            }
            let rerun = record_session_error_recall(&db, WS, &session)?;
            assert_eq!(rerun.incident_cards_recorded, 0);
            assert_eq!(
                db.list_error_repair_links(WS, &diagnostic.layered_key().key)?
                    .len(),
                3
            );
            Ok(())
        }

        #[test]
        fn collection_recall_does_not_credit_unrelated_or_unexecuted_success() -> TestResult {
            let good = green(&[MODULE]);
            for (proof, command) in [
                (good.replace(MODULE, "other/test_queue.py"), COMMAND),
                (good.replace("=== 1 passed in 0.01s ===", ""), COMMAND),
                (". [100%]\n1 passed in 0.01s\n".to_owned(), COMMAND),
                (good.clone(), "pytest -v --color=no other"),
                (format!("{good}Process exited with code 1\n"), COMMAND),
            ] {
                let (db, session) = episode(&proof, command)?;
                let result = record_session_error_recall(&db, WS, &session)?;
                assert_eq!(result.failures_seen, 1);
                assert_eq!(result.resolved_failures, 0);
                assert_eq!(result.repair_links_recorded, 0);
                assert_eq!(result.incident_cards_recorded, 0);
            }
            Ok(())
        }
    }
}
