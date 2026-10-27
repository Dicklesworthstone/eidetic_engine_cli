//! Native Node.js TAP reports, without treating stdout as process completion.
//!
//! Accept the bounded TAP v13 shape emitted by `node --test --test-reporter=tap`.
//! Plans, result numbers, hierarchical subjects and every summary counter agree.
//! TAP omits successful tests' file locations: coverage commits to the unique
//! emitted name paths and test/suite kinds, not inferred source files. The caller
//! additionally requires the exact command and execution context. Ambiguous
//! names are never proof.
//!
//! This is deliberately not a general YAML/TAP interpreter. Diagnostic blocks
//! are data; only their top-level scalar fields can affect interpretation.
//! Unknown, partial and mixed reports cannot fall back to generic success.

use std::collections::BTreeSet;

use super::super::{CanonicalDiagnostic, MAX_DIAGNOSTICS_PER_FAILURE, MAX_SCANNED_OUTPUT_BYTES};
use super::compiler::CompilerOutput;

const MAX_TESTS: usize = 1024;
const MAX_DEPTH: usize = 32;
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
            && proof.failed.is_empty()
            // Adding regressions is normal repair work. Keep every original
            // hierarchical subject and its test/suite kind, not equal counts.
            && self.inventory.is_subset(&proof.inventory)
            // TAP inventory includes SKIP/TODO rows. Presence alone cannot
            // prove that previously passing control tests still ran.
            && self.passed.is_subset(&proof.passed)
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

fn unindent(line: &str, indent: usize) -> Option<&str> {
    line.get(..indent)?
        .bytes()
        .all(|byte| byte == b' ')
        .then(|| &line[indent..])
}

/// Read only fields at the diagnostic mapping's own indentation. Test output,
/// stack frames and nested actual/expected objects cannot introduce a result,
/// an error field, or a premature report terminator.
fn details(lines: &[&str], position: &mut usize, indent: usize) -> Result<Details, ()> {
    if lines.get(*position).and_then(|line| unindent(line, indent)) != Some("  ---") {
        return Err(());
    }
    *position += 1;
    let mut fields = BTreeSet::new();
    let mut result = Details::default();
    let mut saw_duration = false;
    while let Some(raw) = lines.get(*position).copied() {
        *position += 1;
        if raw.trim().is_empty() {
            continue;
        }
        let line = unindent(raw, indent).ok_or(())?;
        if line == "  ..." {
            return saw_duration.then_some(result).ok_or(());
        }
        if line.starts_with("    ") {
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
                        .take_while(|line| {
                            line.trim().is_empty()
                                || unindent(line, indent)
                                    .is_some_and(|line| line.starts_with("    "))
                        })
                        .filter_map(|line| unindent(line, indent)?.strip_prefix("    "))
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
        return Ok(if passed {
            Status::Passed
        } else {
            Status::Failed
        });
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

fn subject(parent: Option<Subject>, name: &str) -> Subject {
    let mut hash = blake3::Hasher::new();
    hash.update(b"ee.cass.node-tap.proof_subject.v2\0");
    hash.update(&[u8::from(parent.is_some())]);
    if let Some(parent) = parent {
        hash.update(&parent);
    }
    hash.update(&(name.len() as u64).to_le_bytes());
    hash.update(name.as_bytes());
    *hash.finalize().as_bytes()
}

fn typed_subject(namespace: Subject, suite: bool) -> Subject {
    let mut hash = blake3::Hasher::new();
    hash.update(b"ee.cass.node-tap.subject_kind.v1\0");
    hash.update(&namespace);
    hash.update(&[u8::from(suite)]);
    *hash.finalize().as_bytes()
}

fn retain(output: &mut CompilerOutput, keys: &mut BTreeSet<String>, detail: &Details) {
    // Do not key on a test title, source location, expected value or stack.
    // Secret redaction and canonicalization also cover decoded YAML strings.
    let message = detail.error.as_deref().unwrap_or("test failed");
    let code = detail
        .code
        .as_deref()
        .or(detail.failure.as_deref())
        .unwrap_or("testFailure");
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

#[derive(Default)]
struct Counters {
    tests: usize,
    suites: usize,
    passed: usize,
    failed: usize,
    cancelled: usize,
    skipped: usize,
    todo: usize,
}

#[derive(Default)]
struct Sequence {
    count: usize,
    failed: bool,
}

struct Walker<'a> {
    lines: Vec<&'a str>,
    position: usize,
    names: BTreeSet<Subject>,
    keys: BTreeSet<String>,
    counts: Counters,
    output: CompilerOutput,
    execution: Execution,
}

/// Ordinary TAP comments (including console output and t.diagnostic) are data.
/// A misplaced structural line or counter is not an ignorable comment.
fn comment(line: &str) -> bool {
    line.starts_with("# ")
        && ![
            "# Subtest:",
            "# tests ",
            "# suites ",
            "# pass ",
            "# fail ",
            "# cancelled ",
            "# skipped ",
            "# todo ",
            "# duration_ms ",
        ]
        .iter()
        .any(|prefix| line.starts_with(*prefix))
}

impl Walker<'_> {
    fn relative(&self, indent: usize) -> Option<&str> {
        self.lines
            .get(self.position)
            .and_then(|line| unindent(line, indent))
    }

    fn sequence(&mut self, depth: usize, parent: Option<Subject>) -> Result<Sequence, ()> {
        if depth > MAX_DEPTH {
            return Err(());
        }
        let indent = depth * 4;
        let mut sequence = Sequence::default();
        loop {
            let line = self.relative(indent).ok_or(())?;
            if let Some(plan) = line.strip_prefix("1..") {
                if count(plan)? != sequence.count {
                    return Err(());
                }
                self.position += 1;
                return Ok(sequence);
            }
            if comment(line) {
                self.position += 1;
                continue;
            }
            let name = line.strip_prefix("# Subtest: ").ok_or(())?.to_owned();
            let namespace = subject(parent, &name);
            if name.trim().is_empty()
                || self.names.len() == MAX_TESTS
                || !self.names.insert(namespace)
            {
                return Err(());
            }
            self.position += 1;
            let children = if self
                .relative(indent + 4)
                .is_some_and(|line| line.starts_with("# ") || line.starts_with("1.."))
            {
                self.sequence(depth + 1, Some(namespace))?
            } else {
                Sequence::default()
            };
            let status = result_line(self.relative(indent).ok_or(())?, sequence.count + 1, &name)?;
            self.position += 1;
            let detail = details(&self.lines, &mut self.position, indent)?;
            let suite = match detail.kind.as_deref() {
                Some("suite") => true,
                None | Some("test") => false,
                _ => return Err(()),
            };
            // A parent result cannot erase a failed child, nor claim a
            // subtest failure for a scope that contains no failing subtests.
            let aggregate = detail.failure.as_deref() == Some("subtestsFailed");
            if (children.failed && matches!(status, Status::Passed | Status::Skipped))
                || (aggregate
                    && (!children.failed || !matches!(status, Status::Failed | Status::Todo)))
            {
                return Err(());
            }
            let id = typed_subject(namespace, suite);
            self.execution.inventory.insert(id);
            if suite {
                self.counts.suites += 1;
            } else {
                self.counts.tests += 1;
            }
            match status {
                Status::Passed => {
                    if detail.failure.is_some() || detail.error.is_some() || detail.code.is_some() {
                        return Err(());
                    }
                    if !suite {
                        self.counts.passed += 1;
                    }
                    self.execution.passed.insert(id);
                }
                Status::Failed => {
                    self.output.failed = true;
                    self.execution.failed.insert(id);
                    sequence.failed = true;
                    let cancelled = matches!(
                        detail.failure.as_deref(),
                        Some("cancelledByParent" | "testTimeoutFailure" | "testAborted")
                    );
                    if !suite {
                        if cancelled {
                            self.counts.cancelled += 1;
                        } else {
                            self.counts.failed += 1;
                        }
                    }
                    // Failed suites already have their leaf diagnostics. A
                    // bootstrap/hook/cancellation cannot disclose a complete
                    // execution inventory and never establishes repair proof.
                    if !aggregate {
                        self.execution.complete &= !suite
                            && detail.failure.as_deref() == Some("testCodeFailure")
                            && !(detail.code.as_deref() == Some("ERR_TEST_FAILURE")
                                && detail.error.as_deref() == Some("test failed"));
                        retain(&mut self.output, &mut self.keys, &detail);
                    }
                }
                Status::Skipped => {
                    if !suite {
                        self.counts.skipped += 1;
                    }
                }
                Status::Todo => {
                    if !suite {
                        self.counts.todo += 1;
                    }
                }
            }
            sequence.count += 1;
        }
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
    let lines: Vec<_> = text.trim().lines().collect();
    if lines.first() != Some(&"TAP version 13") {
        return Err(());
    }
    let mut walker = Walker {
        lines,
        position: 1,
        names: BTreeSet::new(),
        keys: BTreeSet::new(),
        counts: Counters::default(),
        output: CompilerOutput::default(),
        execution: Execution {
            complete: true,
            ..Execution::default()
        },
    };
    walker.sequence(0, None)?;
    let expected = [
        ("tests", walker.counts.tests),
        ("suites", walker.counts.suites),
        ("pass", walker.counts.passed),
        ("fail", walker.counts.failed),
        ("cancelled", walker.counts.cancelled),
        ("skipped", walker.counts.skipped),
        ("todo", walker.counts.todo),
    ];
    for (label, expected) in expected {
        let prefix = format!("# {label} ");
        let value = walker
            .relative(0)
            .and_then(|line| line.strip_prefix(prefix.as_str()))
            .ok_or(())?;
        if count(value)? != expected {
            return Err(());
        }
        walker.position += 1;
    }
    duration(
        walker
            .relative(0)
            .and_then(|line| line.strip_prefix("# duration_ms "))
            .ok_or(())?,
    )?;
    if walker.position + 1 != walker.lines.len() {
        return Err(());
    }
    walker.output.allows_completion =
        !walker.output.failed && walker.counts.passed > 0 && walker.execution.complete;
    Ok(Some(Decoded {
        output: walker.output,
        execution: walker.execution,
    }))
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
        decode(text)
            .expect("valid report")
            .expect("native Node TAP")
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
        assert!(
            !ToolOutput {
                text: PASSED.to_owned(),
                ..ToolOutput::default()
            }
            .succeeded()
        );
        assert!(
            !ToolOutput {
                exit_code: Some(1),
                ..success
            }
            .succeeded()
        );
    }

    #[test]
    fn skipped_todo_removed_renamed_or_failing_extra_tests_cannot_prove_the_failure() {
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
                ("extra", Status::Failed),
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
            valid.replace(
                "  code: 'ERR_ASSERTION'",
                "  code: 'ERR_ASSERTION'\n  code: 'OK'",
            ),
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
        assert!(
            !diagnostic
                .message_template
                .contains(&secret.to_ascii_lowercase())
        );
        assert!(!diagnostic.layered_key().key.contains(secret));
    }

    #[test]
    fn empty_runs_other_families_and_byte_limits_do_not_create_proof() {
        let empty = report(&[]);
        assert!(!parsed(&empty).output.allows_completion);
        assert!(!observed(&empty).succeeded());
        for other in [
            "",
            "ok",
            "# a comment",
            "test result: ok. 1 passed; 0 failed",
        ] {
            assert!(decode(other).expect("different family").is_none());
        }
        assert!(
            decode(&format!(
                "{}{}",
                " ".repeat(MAX_SCANNED_OUTPUT_BYTES),
                PASSED
            ))
            .is_err()
        );
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

    const NESTED_FAILED: &str =
        include_str!("../../tests/fixtures/cass_node_tap/nested_failed.tap");
    const NESTED_PASSED: &str =
        include_str!("../../tests/fixtures/cass_node_tap/nested_passed.tap");

    #[test]
    fn real_nested_suites_and_test_parents_preserve_leaf_failures_and_coverage() {
        let failed = parsed(NESTED_FAILED);
        let passed = parsed(NESTED_PASSED);
        assert_eq!(failed.execution.inventory.len(), 6);
        assert_eq!(failed.execution.failed.len(), 2); // leaf and enclosing suite
        assert_eq!(failed.output.diagnostics.len(), 1); // no aggregate duplicate
        assert!(failed.execution.verified_by(&passed.execution));
        assert!(passed.output.allows_completion);
        let requirement = Requirement::capture(&observed(NESTED_FAILED), true);
        assert!(requirement.is_verified_by(&Proof::capture(&observed(NESTED_PASSED))));
        let moved = NESTED_PASSED.replace("ledger", "different ledger");
        assert!(!requirement.is_verified_by(&Proof::capture(&observed(&moved))));
    }

    #[test]
    fn hierarchy_component_boundaries_and_node_kinds_are_bound() {
        let first = subject(Some(subject(None, "a b")), "c");
        let second = subject(Some(subject(None, "a")), "b c");
        assert_ne!(first, second);
        assert_ne!(typed_subject(first, false), typed_subject(first, true));
        assert_ne!(subject(None, "child"), subject(Some(first), "child"));
        // The same child title in distinct scopes is legitimate, not a
        // duplicate. Keep all six identities despite the shared leaf name.
        let repeated = NESTED_PASSED.replace("retries", "saves the revision");
        assert_eq!(parsed(&repeated).execution.inventory.len(), 6);
        assert!(parsed(&repeated).output.allows_completion);
    }

    #[test]
    fn child_plans_aggregate_status_and_suite_totals_are_independently_checked() {
        for invalid in [
            NESTED_FAILED.replace("    1..3", "    1..2"),
            NESTED_FAILED.replace("not ok 1 - ledger", "ok 1 - ledger"),
            NESTED_FAILED.replace("    1..3\n", ""),
            NESTED_FAILED.replace("# suites 1", "# suites 0"),
            NESTED_FAILED.replace("# tests 5", "# tests 6"),
            NESTED_PASSED.replace(
                "  type: 'suite'\n",
                "  type: 'suite'\n  failureType: 'subtestsFailed'\n",
            ),
            NESTED_PASSED.replace("    ok 1 - retries", "    ok 2 - retries"),
        ] {
            assert!(decode(&invalid).is_err());
            assert!(!observed(&invalid).succeeded());
            assert!(failure_diagnostics(&observed(&invalid)).is_empty());
        }
    }

    #[test]
    fn native_cancellation_is_a_failure_but_never_a_repairable_inventory() {
        for (cancelled, name) in [
            (
                include_str!("../../tests/fixtures/cass_node_tap/cancelled.tap"),
                "pending",
            ),
            (
                include_str!("../../tests/fixtures/cass_node_tap/timeout.tap"),
                "deadline",
            ),
            (
                include_str!("../../tests/fixtures/cass_node_tap/aborted.tap"),
                "aborted",
            ),
        ] {
            let decoded = parsed(cancelled);
            assert!(decoded.output.failed);
            assert_eq!(decoded.output.diagnostics.len(), 1);
            assert!(!decoded.output.allows_completion);
            assert!(!decoded.execution.has_known_failures());
            let requirement = Requirement::capture(&observed(cancelled), true);
            let green = report(&[(name, Status::Passed)]);
            assert!(!requirement.is_verified_by(&Proof::capture(&observed(&green))));
        }
    }

    #[test]
    fn diagnostic_comments_do_not_change_subjects_or_supply_counters() {
        let with_comments = PASSED
            .replace("# Subtest:", "# producer diagnostic\n# Subtest:")
            .replace("1..2\n", "# cache checked\n1..2\n");
        let plain = parsed(PASSED);
        let commented = parsed(&with_comments);
        assert_eq!(plain.execution.inventory, commented.execution.inventory);
        assert!(commented.output.allows_completion);
        let misplaced = PASSED.replace("1..2\n", "# fail 0\n1..2\n");
        assert!(decode(&misplaced).is_err());
    }

    fn failing_todo() -> String {
        FAILED
            .replace(
                "not ok 1 - cache returns the saved revision\n",
                "not ok 1 - cache returns the saved revision # TODO\n",
            )
            .replace("# fail 1", "# fail 0")
            .replace("# todo 0", "# todo 1")
    }

    #[test]
    fn expected_todo_errors_are_data_not_process_status_or_success_evidence() {
        let todo = failing_todo();
        assert!(parsed(&todo).output.allows_completion);
        assert!(!observed(&todo).failed());
        assert!(observed(&todo).succeeded());
        let required = Requirement::capture(&observed(FAILED), true);
        assert!(!required.is_verified_by(&Proof::capture(&observed(&todo))));
        let quoted_exit = todo.replace(
            "Expected values to be strictly equal:",
            "Process exited with code 0",
        );
        assert!(
            !ToolOutput {
                text: quoted_exit,
                ..ToolOutput::default()
            }
            .succeeded()
        );
        let todo_suite = NESTED_FAILED.replace("not ok 1 - ledger\n", "not ok 1 - ledger # TODO\n");
        assert!(parsed(&todo_suite).output.failed);
        assert_eq!(parsed(&todo_suite).output.diagnostics.len(), 1);
    }

    #[test]
    fn native_diagnostic_chunks_do_not_poison_an_expected_todo_across_polls() {
        use crate::core::cass_error_recall::{CommandFamily, InFlightInvocation};

        let text = failing_todo();
        for boundary in (1..text.len()).filter(|index| text.is_char_boundary(*index)) {
            let mut call = InFlightInvocation {
                family: CommandFamily::parse("node --test cache.test.cjs").expect("command"),
                call_index: 0,
                request_index: 0,
                can_poll: true,
                process_id: Some(1),
                output: ToolOutput::default(),
                failure_recorded: false,
                output_truncated: false,
            };
            call.append(ToolOutput {
                text: text[..boundary].to_owned(),
                ..ToolOutput::default()
            });
            call.append(ToolOutput {
                text: text[boundary..].to_owned(),
                exit_code: Some(0),
                ..ToolOutput::default()
            });
            assert!(call.output.succeeded(), "split at byte {boundary}");
        }
    }

    #[test]
    fn recursive_depth_is_bounded_even_for_otherwise_valid_green_reports() {
        fn wrapped(depth: usize) -> String {
            let mut body = String::new();
            for level in 0..=depth {
                body.push_str(&format!("{}# Subtest: nested\n", "    ".repeat(level)));
            }
            for level in (0..=depth).rev() {
                let indent = "    ".repeat(level);
                body.push_str(&format!(
                    "{indent}ok 1 - nested\n{indent}  ---\n{indent}  duration_ms: 1\n{indent}  type: 'test'\n{indent}  ...\n{indent}1..1\n"
                ));
            }
            let tests = depth + 1;
            format!(
                "TAP version 13\n{body}# tests {tests}\n# suites 0\n# pass {tests}\n# fail 0\n# cancelled 0\n# skipped 0\n# todo 0\n# duration_ms 1\n"
            )
        }
        assert!(parsed(&wrapped(MAX_DEPTH)).output.allows_completion);
        assert!(decode(&wrapped(MAX_DEPTH + 1)).is_err());
    }
}

#[cfg(test)]
#[path = "cass_error_recall_node_tap_store_tests.rs"]
mod store_tests;
