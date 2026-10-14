//! Native Node.js TAP reports, without treating stdout as process completion.
//!
//! Accept the bounded TAP v13 shape emitted by `node --test --test-reporter=tap`.
//! Plans, result numbers, named subjects and every summary counter must agree.
//! TAP omits successful tests' file locations: coverage commits to the unique
//! emitted names, not inferred paths. The caller additionally requires the
//! exact command and execution context. Ambiguous names are never proof.
//!
//! This is deliberately not a general YAML/TAP interpreter. Diagnostic blocks
//! are data; only their top-level scalar fields can affect interpretation.
//! Unknown, partial and mixed reports cannot fall back to generic success.

use std::collections::BTreeSet;

use super::super::{CanonicalDiagnostic, MAX_DIAGNOSTICS_PER_FAILURE, MAX_SCANNED_OUTPUT_BYTES};
use super::compiler::CompilerOutput;

const MAX_TESTS: usize = 1024;
type Subject = [u8; 32];

#[derive(Default)]
pub(super) struct Execution {
    inventory: BTreeSet<Subject>,
    failed: BTreeSet<Subject>,
    passed: BTreeSet<Subject>,
    complete: bool,
}

impl Execution {
    pub(super) fn has_known_failures(&self) -> bool {
        self.complete && !self.failed.is_empty()
    }

    pub(super) fn verified_by(&self, proof: &Self) -> bool {
        self.has_known_failures()
            && proof.complete
            && self.inventory == proof.inventory
            && self.failed.is_subset(&proof.passed)
    }
}

pub(in crate::core::cass_error_recall) struct Decoded {
    pub output: CompilerOutput,
    pub(super) execution: Execution,
}

#[derive(Default)]
struct Details {
    kind: Option<String>,
    failure: Option<String>,
    code: Option<String>,
    error: Option<String>,
}

fn scalar(value: &str) -> Option<String> {
    if let Some(inner) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        // A literal quote in YAML's single-quoted form must be doubled.
        let mut chars = inner.chars();
        let mut decoded = String::new();
        while let Some(character) = chars.next() {
            if character == '\'' && chars.next() != Some('\'') {
                return None;
            }
            decoded.push(character);
        }
        Some(decoded)
    } else if value.starts_with('"') {
        serde_json::from_str::<String>(value).ok()
    } else {
        None
    }
}

fn count(value: &str) -> Result<usize, ()> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    value.parse().map_err(|_| ())
}

fn duration(value: &str) -> Result<(), ()> {
    let value = value.parse::<f64>().map_err(|_| ())?;
    (value.is_finite() && value >= 0.0).then_some(()).ok_or(())
}

/// Read only fields at the diagnostic mapping's own indentation. Test output,
/// stack frames and nested actual/expected objects cannot introduce a result,
/// an error field, or a premature report terminator.
fn details(lines: &[&str], position: &mut usize) -> Result<Details, ()> {
    if lines.get(*position) != Some(&"  ---") {
        return Err(());
    }
    *position += 1;
    let mut fields = BTreeSet::new();
    let mut result = Details::default();
    let mut saw_duration = false;
    while let Some(line) = lines.get(*position).copied() {
        *position += 1;
        if line == "  ..." {
            return saw_duration.then_some(result).ok_or(());
        }
        if line.trim().is_empty() || line.starts_with("    ") {
            continue;
        }
        let field = line.strip_prefix("  ").ok_or(())?;
        let (key, value) = field.split_once(':').ok_or(())?;
        let value = value.strip_prefix(' ').unwrap_or(value);
        if !fields.insert(key) {
            return Err(());
        }
        match key {
            "duration_ms" => {
                duration(value)?;
                saw_duration = true;
            }
            "type" => result.kind = Some(scalar(value).ok_or(())?),
            "failureType" => result.failure = Some(scalar(value).ok_or(())?),
            "code" => result.code = Some(scalar(value).ok_or(())?),
            "error" => {
                result.error = Some(if matches!(value, "|-" | "|") {
                    lines[*position..]
                        .iter()
                        .take_while(|line| line.starts_with("    ") || line.trim().is_empty())
                        .filter_map(|line| line.strip_prefix("    "))
                        .find(|line| !line.trim().is_empty())
                        .map(str::to_owned)
                        .ok_or(())?
                } else {
                    scalar(value).ok_or(())?
                });
            }
            // These fields never supply identities or diagnostic text.
            _ => {}
        }
    }
    Err(())
}

#[derive(Clone, Copy)]
enum Status {
    Passed,
    Failed,
    Skipped,
    Todo,
}

fn result_line(line: &str, number: usize, name: &str) -> Result<Status, ()> {
    let (passed, rest) = if let Some(rest) = line.strip_prefix("ok ") {
        (true, rest)
    } else {
        (false, line.strip_prefix("not ok ").ok_or(())?)
    };
    let (observed, rest) = rest.split_once(" - ").ok_or(())?;
    if count(observed)? != number {
        return Err(());
    }
    // Bind the header first, before interpreting an optional directive. A
    // name containing '# SKIP' is still a name, not evidence of skipped work.
    let suffix = rest.strip_prefix(name).ok_or(())?;
    if suffix.is_empty() {
        return Ok(if passed { Status::Passed } else { Status::Failed });
    }
    for (marker, status) in [(" # SKIP", Status::Skipped), (" # TODO", Status::Todo)] {
        if suffix
            .strip_prefix(marker)
            .is_some_and(|reason| reason.is_empty() || reason.starts_with(' '))
        {
            return Ok(status);
        }
    }
    Err(())
}

fn subject(name: &str) -> Subject {
    let mut hash = blake3::Hasher::new();
    hash.update(b"ee.cass.node-tap.proof_subject.v1\0");
    hash.update(&(name.len() as u64).to_le_bytes());
    hash.update(name.as_bytes());
    *hash.finalize().as_bytes()
}

fn retain(output: &mut CompilerOutput, keys: &mut BTreeSet<String>, detail: &Details) {
    // Do not key on a test title, source location, expected value or stack.
    // Secret redaction and canonicalization also cover decoded YAML strings.
    let message = detail.error.as_deref().unwrap_or("test failed");
    let code = detail.code.as_deref().unwrap_or("testCodeFailure");
    let safe =
        crate::policy::redact_secret_like_content(&format!("node-test {code}: {message}")).content;
    let diagnostic = CanonicalDiagnostic {
        tool: crate::core::error_recall::DiagnosticTool::Shell,
        canonical_code: None,
        message_template: crate::core::error_recall::canonical_message_template(&safe),
    };
    if keys.insert(diagnostic.layered_key().key)
        && output.diagnostics.len() < MAX_DIAGNOSTICS_PER_FAILURE
    {
        output.diagnostics.push(diagnostic);
    }
}

/// None is a different family, Err is an invalid recognized TAP report. Do
/// not interchange the two: an invalid native report must veto repair proof.
pub(in crate::core::cass_error_recall) fn decode(text: &str) -> Result<Option<Decoded>, ()> {
    if !text.trim_start().starts_with("TAP version") {
        return Ok(None);
    }
    if text.len() > MAX_SCANNED_OUTPUT_BYTES {
        return Err(());
    }
    let text = text.trim();
    let lines: Vec<_> = text.lines().collect();
    if lines.first() != Some(&"TAP version 13") {
        return Err(());
    }
    let mut position = 1;
    let mut output = CompilerOutput::default();
    let mut execution = Execution {
        complete: true,
        ..Execution::default()
    };
    let mut keys = BTreeSet::new();
    let mut counts = [0_usize; 5]; // tests, passed, failed, skipped, todo
    while let Some(name) = lines
        .get(position)
        .and_then(|line| line.strip_prefix("# Subtest: "))
    {
        if counts[0] == MAX_TESTS || name.trim().is_empty() {
            return Err(());
        }
        let id = subject(name);
        if !execution.inventory.insert(id) {
            return Err(());
        }
        counts[0] += 1;
        position += 1;
        let status = result_line(lines.get(position).ok_or(())?, counts[0], name)?;
        position += 1;
        let detail = details(&lines, &mut position)?;
        if detail.kind.as_deref().is_some_and(|kind| kind != "test") {
            return Err(());
        }
        match status {
            Status::Passed => {
                if detail.failure.is_some() || detail.error.is_some() || detail.code.is_some() {
                    return Err(());
                }
                counts[1] += 1;
                execution.passed.insert(id);
            }
            Status::Failed => {
                counts[2] += 1;
                output.failed = true;
                execution.failed.insert(id);
                // File setup, cancellation and timeouts do not disclose an
                // executed test inventory. Their observations are not fixes.
                execution.complete &= detail.failure.as_deref() == Some("testCodeFailure")
                    && !(detail.code.as_deref() == Some("ERR_TEST_FAILURE")
                        && detail.error.as_deref() == Some("test failed"));
                retain(&mut output, &mut keys, &detail);
            }
            Status::Skipped => counts[3] += 1,
            Status::Todo => counts[4] += 1,
        }
    }
    let plan = lines
        .get(position)
        .and_then(|line| line.strip_prefix("1.."))
        .ok_or(())?;
    if count(plan)? != counts[0] {
        return Err(());
    }
    position += 1;
    let expected = [
        ("tests", counts[0]),
        ("suites", 0),
        ("pass", counts[1]),
        ("fail", counts[2]),
        ("cancelled", 0),
        ("skipped", counts[3]),
        ("todo", counts[4]),
    ];
    for (label, expected) in expected {
        let prefix = format!("# {label} ");
        let value = lines
            .get(position)
            .and_then(|line| line.strip_prefix(prefix.as_str()))
            .ok_or(())?;
        if count(value)? != expected {
            return Err(());
        }
        position += 1;
    }
    duration(
        lines
            .get(position)
            .and_then(|line| line.strip_prefix("# duration_ms "))
            .ok_or(())?,
    )?;
    if position + 1 != lines.len() {
        return Err(());
    }
    output.allows_completion = !output.failed && counts[1] > 0 && execution.complete;
    Ok(Some(Decoded { output, execution }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::super::{Proof, Requirement};
    use super::*;
    use crate::core::cass_error_recall::{
        ToolOutput, failure_diagnostics, structured_error_diagnostics, symptom_line,
    };

    const FAILED: &str = include_str!("../../tests/fixtures/cass_node_tap/flat_failed.tap");
    const PASSED: &str = include_str!("../../tests/fixtures/cass_node_tap/flat_passed.tap");

    fn parsed(text: &str) -> Decoded {
        decode(text).expect("valid report").expect("native Node TAP")
    }

    fn observed(text: &str) -> ToolOutput {
        ToolOutput {
            text: text.to_owned(),
            is_error: Some(false),
            exit_code: Some(0),
        }
    }

    // Small complete reports make individual counter/coverage violations
    // explicit. The positive producer contract is separately pinned above to
    // output captured from the real Node 22.16.0 test runner.
    fn report(tests: &[(&str, Status)]) -> String {
        let mut text = "TAP version 13\n".to_owned();
        let mut counts = [0; 4];
        for (index, (name, status)) in tests.iter().enumerate() {
            let (result, directive, category) = match status {
                Status::Passed => ("ok", "", 0),
                Status::Failed => ("not ok", "", 1),
                Status::Skipped => ("ok", " # SKIP", 2),
                Status::Todo => ("ok", " # TODO", 3),
            };
            counts[category] += 1;
            text.push_str(&format!(
                "# Subtest: {name}\n{result} {} - {name}{directive}\n  ---\n  duration_ms: 1\n  type: 'test'\n",
                index + 1
            ));
            if matches!(status, Status::Failed) {
                text.push_str(
                    "  failureType: 'testCodeFailure'\n  error: 'cache revision was stale'\n  code: 'ERR_ASSERTION'\n",
                );
            }
            text.push_str("  ...\n");
        }
        text.push_str(&format!(
            "1..{}\n# tests {}\n# suites 0\n# pass {}\n# fail {}\n# cancelled 0\n# skipped {}\n# todo {}\n# duration_ms 2\n",
            tests.len(), tests.len(), counts[0], counts[1], counts[2], counts[3]
        ));
        text
    }

    #[test]
    fn real_node_failure_and_retry_bind_the_same_executed_test_inventory() {
        let failed = parsed(FAILED);
        let passed = parsed(PASSED);
        assert!(failed.output.failed);
        assert!(!failed.output.allows_completion);
        assert_eq!(failed.output.diagnostics.len(), 1);
        assert_eq!(failed.execution.inventory.len(), 2);
        assert_eq!(failed.execution.failed.len(), 1);
        assert!(failed.execution.verified_by(&passed.execution));
        assert!(passed.output.allows_completion);
        assert!(passed.output.diagnostics.is_empty());
    }

    #[test]
    fn imported_and_error_log_diagnostics_share_keys_without_source_paths_or_names() {
        let output = observed(FAILED);
        let imported = failure_diagnostics(&output);
        assert_eq!(imported, structured_error_diagnostics(FAILED));
        assert_eq!(imported.len(), 1);
        let text = &imported[0].message_template;
        assert!(!text.contains("/mnt/data"));
        assert!(!text.contains("basic.test.cjs"));
        assert!(!text.contains("cache returns the saved revision"));
        assert_eq!(symptom_line(&output).as_deref(), Some(text.as_str()));
    }

    #[test]
    fn native_failure_vetoes_zero_exit_and_success_still_needs_process_completion() {
        let failure = observed(FAILED);
        assert!(failure.failed());
        assert!(!failure.succeeded());
        let success = observed(PASSED);
        assert!(success.succeeded());
        assert!(!ToolOutput {
            text: PASSED.to_owned(),
            ..ToolOutput::default()
        }
        .succeeded());
        assert!(!ToolOutput {
            exit_code: Some(1),
            ..success
        }
        .succeeded());
    }

    #[test]
    fn skipped_todo_removed_renamed_or_extra_tests_cannot_prove_the_failure() {
        let failed = report(&[("repair", Status::Failed), ("control", Status::Passed)]);
        let requirement = Requirement::capture(&observed(&failed), true);
        assert!(requirement.is_guarded());
        let green = report(&[("repair", Status::Passed), ("control", Status::Passed)]);
        assert!(requirement.is_verified_by(&Proof::capture(&observed(&green))));
        for tests in [
            vec![("repair", Status::Skipped), ("control", Status::Passed)],
            vec![("repair", Status::Todo), ("control", Status::Passed)],
            vec![("control", Status::Passed)],
            vec![("different", Status::Passed), ("control", Status::Passed)],
            vec![
                ("repair", Status::Passed),
                ("control", Status::Passed),
                ("extra", Status::Passed),
            ],
        ] {
            let proof = Proof::capture(&observed(&report(&tests)));
            assert!(!requirement.is_verified_by(&proof));
        }
    }

    #[test]
    fn partial_processes_and_invalid_refreshes_do_not_weaken_coverage() {
        let failed = observed(FAILED);
        let proof = Proof::capture(&observed(PASSED));
        let mut requirement = Requirement::capture(&failed, false);
        assert!(requirement.is_guarded());
        assert!(!requirement.is_verified_by(&proof));
        requirement.refresh(&failed, true);
        assert!(requirement.is_verified_by(&proof));
        requirement.refresh(&observed("TAP version 13\n# Subtest: interrupted"), true);
        assert!(!requirement.is_verified_by(&proof));
        requirement.refresh(&observed("ordinary stdout"), true);
        assert!(requirement.is_guarded());
        assert!(!requirement.is_verified_by(&proof));
    }

    #[test]
    fn plans_numbers_counters_duplicates_trailers_and_truncation_fail_closed() {
        let valid = report(&[("repair", Status::Failed), ("control", Status::Passed)]);
        for invalid in [
            valid.replace("1..2", "1..1"),
            valid.replace("not ok 1 -", "not ok 2 -"),
            valid.replace("# fail 1", "# fail 0"),
            valid.replace("# pass 1", "# pass 2"),
            valid.replace("# tests 2", "# tests 999"),
            valid.replace("# cancelled 0", "# cancelled 1"),
            valid.replace("  code: 'ERR_ASSERTION'", "  code: 'ERR_ASSERTION'\n  code: 'OK'"),
            valid.replace("  duration_ms: 1", "  duration_ms: NaN"),
            valid.replace("control", "repair"),
            format!("{valid}Process exited with code 0\n"),
            format!("{valid}TAP version 13\n"),
            valid
                .trim_end()
                .rsplit_once('\n')
                .expect("summary")
                .0
                .to_owned(),
            "TAP version 14\n".to_owned(),
        ] {
            assert!(decode(&invalid).is_err());
            let output = observed(&invalid);
            assert!(!output.succeeded());
            assert!(failure_diagnostics(&output).is_empty());
        }
    }

    #[test]
    fn diagnostic_data_cannot_supply_a_compiler_error_or_a_second_report() {
        let valid = report(&[("repair", Status::Failed)]);
        let nested = valid.replace(
            "  error: 'cache revision was stale'",
            "  error: 'cache revision was stale'\n  actual:\n    error: 'error[E0277]: quoted data'\n    code: 'PRIVATE_CODE'\n  stack: |-\n    TAP version 13\n    1..1\n    # fail 0",
        );
        let expected = failure_diagnostics(&observed(&valid));
        assert_eq!(failure_diagnostics(&observed(&nested)), expected);
        let truncated = nested.split("  stack:").next().expect("prefix");
        assert!(failure_diagnostics(&observed(truncated)).is_empty());
        assert!(!observed(truncated).succeeded());
    }

    #[test]
    fn secrets_are_redacted_before_diagnostic_keying() {
        let secret = "sk-proj-ABCDEF1234567890ABCDEF1234567890";
        let text = report(&[("repair", Status::Failed)]).replace(
            "cache revision was stale",
            &format!("authentication failed api_key={secret}"),
        );
        let decoded = parsed(&text);
        assert_eq!(decoded.output.diagnostics.len(), 1);
        let diagnostic = &decoded.output.diagnostics[0];
        assert!(!diagnostic.message_template.contains(&secret.to_ascii_lowercase()));
        assert!(!diagnostic.layered_key().key.contains(secret));
    }

    #[test]
    fn empty_runs_other_families_and_byte_limits_do_not_create_proof() {
        let empty = report(&[]);
        assert!(!parsed(&empty).output.allows_completion);
        assert!(!observed(&empty).succeeded());
        for other in ["", "ok", "# a comment", "test result: ok. 1 passed; 0 failed"] {
            assert!(decode(other).expect("different family").is_none());
        }
        assert!(decode(&format!("{}{}", " ".repeat(MAX_SCANNED_OUTPUT_BYTES), PASSED)).is_err());
    }

    #[test]
    fn directives_in_names_are_not_skips_and_yaml_quotes_are_decoded_once() {
        let text = report(&[("literal # SKIP is a title", Status::Passed)]);
        assert!(parsed(&text).output.allows_completion);
        assert_eq!(
            scalar("'can''t decode twice'").as_deref(),
            Some("can't decode twice")
        );
        assert!(scalar("'an unescaped ' quote'").is_none());
        assert_eq!(scalar("\"unicode λ\"").as_deref(), Some("unicode λ"));
    }
}
