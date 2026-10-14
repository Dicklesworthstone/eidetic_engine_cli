//! Bind a repair to the failed execution subjects, not just command text.
//!
//! The compiler/checker decoder validates complete bounded reports. Libtest
//! retains transient hashes of exact test names and terminal outcomes. ESLint
//! retains exact file commitments and requires every failed file in the clean
//! retry. These commitments are never stored or substituted for provenance.
//! Libtest suite identity is its complete selected inventory: duplicate,
//! incomplete fail-fast and changed inventories remain unproven.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use super::{ToolOutput, compiler};

type Name = [u8; 32];
type Inventory = Vec<Name>;

#[derive(Clone, Copy, Eq, PartialEq)]
enum Outcome {
    Passed,
    Failed,
    Ignored,
}

struct Suite {
    selected: u64,
    outcomes: BTreeMap<Name, Outcome>,
}

struct Execution {
    suites: BTreeMap<Inventory, BTreeMap<Name, Outcome>>,
    complete: bool,
    allows_completion: bool,
}

pub(super) struct RequiredSuite {
    inventory: Inventory,
    failed: Vec<Name>,
}

/// Other errors retain their existing exact-invocation proof rules. A typed
/// test or lint failure cannot fall back when its coverage is unknown.
pub(super) enum Requirement {
    Other,
    Unproven,
    Libtest(Vec<RequiredSuite>),
    Eslint(BTreeSet<Name>),
    GoTests {
        packages: BTreeSet<Name>,
        tests: BTreeSet<Name>,
    },
}

impl Requirement {
    pub(super) fn capture(output: &ToolOutput, process_complete: bool) -> Self {
        let decoded = match compiler::inspect(&output.text) {
            Ok(Some(decoded)) => decoded,
            Ok(None) => return Self::Other,
            Err(()) => return Self::Unproven,
        };
        if let Some(coverage) = &decoded.go_test_coverage {
            return if !process_complete || !coverage.complete {
                Self::Unproven
            } else if coverage.failed_packages.is_empty() && coverage.failed_tests.is_empty() {
                Self::Other
            } else {
                Self::GoTests {
                    packages: coverage.failed_packages.clone(),
                    tests: coverage.failed_tests.clone(),
                }
            };
        }
        if let Some(coverage) = &decoded.eslint_coverage {
            return if !process_complete {
                Self::Unproven
            } else if coverage.failed_files.is_empty() {
                Self::Other
            } else {
                Self::Eslint(coverage.failed_files.clone())
            };
        }
        match execution(&output.text, &decoded) {
            Ok(None) => Self::Other,
            Ok(Some(execution)) if process_complete && execution.complete => {
                let required: Vec<_> = execution
                    .suites
                    .into_iter()
                    .filter_map(|(inventory, outcomes)| {
                        let failed: Vec<_> = outcomes
                            .into_iter()
                            .filter_map(|(name, result)| {
                                (result == Outcome::Failed).then_some(name)
                            })
                            .collect();
                        (!failed.is_empty()).then_some(RequiredSuite { inventory, failed })
                    })
                    .collect();
                if required.is_empty() {
                    Self::Other
                } else {
                    Self::Libtest(required)
                }
            }
            _ => Self::Unproven,
        }
    }

    pub(super) fn is_guarded(&self) -> bool {
        !matches!(self, Self::Other)
    }

    /// Later polls may reveal additional failed subjects beyond the diagnostic
    /// display cap. Replace the requirement from the accumulated output, not
    /// the latest chunk alone. A damaged stream cannot erase an earlier guard.
    pub(super) fn refresh(&mut self, output: &ToolOutput, process_complete: bool) {
        let next = Self::capture(output, process_complete);
        *self = if self.is_guarded() && matches!(next, Self::Other) {
            Self::Unproven
        } else {
            next
        };
    }

    pub(super) fn is_verified_by(&self, proof: &Proof) -> bool {
        match self {
            Self::Other => true,
            Self::Unproven => false,
            Self::GoTests { packages, tests } => proof.go_tests.as_ref().is_some_and(|coverage| {
                packages.is_subset(&coverage.passed_packages)
                    && tests.is_subset(&coverage.passed_tests)
            }),
            Self::Eslint(required) => proof
                .eslint_files
                .as_ref()
                .is_some_and(|checked| required.is_subset(checked)),
            Self::Libtest(required) => proof.execution.as_ref().is_some_and(|execution| {
                required.iter().all(|suite| {
                    execution
                        .suites
                        .get(&suite.inventory)
                        .is_some_and(|outcomes| {
                            suite
                                .failed
                                .iter()
                                .all(|name| outcomes.get(name) == Some(&Outcome::Passed))
                        })
                })
            }),
        }
    }
}

/// One projection per successful invocation, shared by all pending failures.
#[derive(Default)]
pub(super) struct Proof {
    execution: Option<Execution>,
    eslint_files: Option<BTreeSet<Name>>,
    go_tests: Option<compiler::GoTestCoverage>,
}

impl Proof {
    pub(super) fn capture(output: &ToolOutput) -> Self {
        if !output.succeeded() {
            return Self::default();
        }
        let Ok(Some(decoded)) = compiler::inspect(&output.text) else {
            return Self::default();
        };
        Self {
            execution: execution(&output.text, &decoded)
                .ok()
                .flatten()
                .filter(|execution| execution.complete && execution.allows_completion),
            eslint_files: decoded
                .eslint_coverage
                .as_ref()
                .filter(|_| decoded.allows_completion)
                .map(|coverage| coverage.checked_files.clone()),
            go_tests: decoded
                .go_test_coverage
                .filter(|coverage| coverage.complete && decoded.allows_completion),
        }
    }
}

#[derive(Deserialize)]
struct Projection {
    #[serde(rename = "type")]
    kind: Option<String>,
    event: Option<String>,
    name: Option<String>,
    test_count: Option<u64>,
}

fn name_commitment(name: &str) -> Name {
    let mut hash = blake3::Hasher::new();
    hash.update(b"ee.cass.libtest.proof_name.v1\0");
    hash.update(&(name.len() as u64).to_le_bytes());
    hash.update(name.as_bytes());
    *hash.finalize().as_bytes()
}

/// Interpret only a libtest stream already accepted by the shipping decoder.
/// Its explicit family marker prevents lint result arrays from entering this
/// object-stream projection. The decoder owns all validity and status checks.
fn execution(text: &str, decoded: &compiler::CompilerOutput) -> Result<Option<Execution>, ()> {
    if !decoded.has_test_execution {
        return Ok(None);
    }
    let mut active: Option<Suite> = None;
    let mut suites = BTreeMap::new();
    let mut seen = false;
    let mut complete = true;
    for record in serde_json::Deserializer::from_str(text).into_iter::<Projection>() {
        let record = record.map_err(|_| ())?;
        match (record.kind.as_deref(), record.event.as_deref()) {
            (Some("suite"), Some("started")) => {
                if active.is_some() {
                    return Err(());
                }
                active = Some(Suite {
                    selected: record.test_count.ok_or(())?,
                    outcomes: BTreeMap::new(),
                });
                seen = true;
            }
            (Some("test"), Some(event @ ("ok" | "failed" | "ignored"))) => {
                let name = name_commitment(record.name.as_deref().ok_or(())?);
                let outcome = match event {
                    "ok" => Outcome::Passed,
                    "failed" => Outcome::Failed,
                    _ => Outcome::Ignored,
                };
                if active
                    .as_mut()
                    .ok_or(())?
                    .outcomes
                    .insert(name, outcome)
                    .is_some()
                {
                    return Err(());
                }
            }
            (Some("suite"), Some("ok" | "failed")) => {
                let suite = active.take().ok_or(())?;
                // A fail-fast summary may be valid but leave selected test
                // identities unknown. It still records a failure, not proof.
                complete &= suite.outcomes.len() as u64 == suite.selected;
                let inventory: Vec<_> = suite.outcomes.keys().copied().collect();
                // Empty suites have no failed identity and commonly repeat.
                // Repeated nonempty inventories cannot identify a test binary.
                if !inventory.is_empty() && suites.insert(inventory, suite.outcomes).is_some() {
                    return Err(());
                }
            }
            (Some("test"), Some("started" | "timeout")) | (None, None) => {}
            _ => return Err(()),
        }
    }
    Ok(seen.then_some(Execution {
        suites,
        complete: complete && active.is_none(),
        allows_completion: decoded.allows_completion,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn suite(results: &[(&str, &str)]) -> String {
        let mut events = vec![json!({"type":"suite","event":"started","test_count":results.len()})];
        for (name, _) in results {
            events.push(json!({"type":"test","event":"started","name":name}));
        }
        for (name, event) in results.iter().rev() {
            events.push(json!({"type":"test","event":event,"name":name}));
        }
        let count = |event| {
            results
                .iter()
                .filter(|(_, observed)| *observed == event)
                .count()
        };
        events.push(
            json!({"type":"suite","event":if count("failed") == 0 {"ok"} else {"failed"},
            "passed":count("ok"),"failed":count("failed"),"ignored":count("ignored"),
            "measured":0,"filtered_out":0}),
        );
        events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn output(text: String) -> ToolOutput {
        ToolOutput {
            text,
            is_error: Some(false),
            exit_code: Some(0),
        }
    }

    fn requirement(results: &[(&str, &str)]) -> Requirement {
        Requirement::capture(&output(suite(results)), true)
    }

    #[test]
    fn skipped_missing_or_renamed_failed_tests_do_not_inherit_another_tests_success() {
        let failed = requirement(&[("cache::round_trip", "failed"), ("other", "ok")]);
        for results in [
            vec![("cache::round_trip", "ignored"), ("other", "ok")],
            vec![("other", "ok")],
            vec![("Cache::round_trip", "ok"), ("other", "ok")],
            vec![("cache::round_trip", "ok"), ("replacement", "ok")],
        ] {
            let passed = output(suite(&results));
            assert!(
                passed.succeeded(),
                "the command-level gate alone would accept this"
            );
            assert!(!failed.is_verified_by(&Proof::capture(&passed)));
        }
        assert!(failed.is_verified_by(&Proof::capture(&output(suite(&[
            ("other", "ok"),
            ("cache::round_trip", "ok")
        ])))));
    }

    #[test]
    fn suite_inventories_not_suite_order_resolve_unique_test_binaries() {
        let failed = output(format!(
            "{}\n{}",
            suite(&[("a", "failed")]),
            suite(&[("b", "failed")])
        ));
        let requirement = Requirement::capture(&failed, true);
        let passed = output(format!(
            "{}\n{}",
            suite(&[("b", "ok")]),
            suite(&[("a", "ok")])
        ));
        assert!(requirement.is_verified_by(&Proof::capture(&passed)));
        let missing = output(suite(&[("a", "ok")]));
        assert!(!requirement.is_verified_by(&Proof::capture(&missing)));
        let duplicate = output(format!(
            "{}\n{}",
            suite(&[("a", "ok")]),
            suite(&[("a", "ok")])
        ));
        assert!(!requirement.is_verified_by(&Proof::capture(&duplicate)));
        assert!(!Requirement::capture(&duplicate, true).is_verified_by(&Proof::capture(&passed)));
    }

    #[test]
    fn failures_beyond_the_four_diagnostic_cap_are_still_required_to_pass() {
        let names: Vec<_> = (0..8).map(|i| format!("case_{i}")).collect();
        let results: Vec<_> = names.iter().map(|name| (name.as_str(), "failed")).collect();
        let failed = output(suite(&results));
        assert_eq!(super::super::failure_diagnostics(&failed).len(), 4);
        let requirement = Requirement::capture(&failed, true);
        let mut passed: Vec<_> = names.iter().map(|name| (name.as_str(), "ok")).collect();
        // Results arrive in reverse order, so case_0 is beyond the output cap.
        passed[0].1 = "ignored";
        assert!(!requirement.is_verified_by(&Proof::capture(&output(suite(&passed)))));
        passed[0].1 = "ok";
        assert!(requirement.is_verified_by(&Proof::capture(&output(suite(&passed)))));
    }

    #[test]
    fn build_only_plain_stdout_and_nested_examples_are_not_execution_coverage() {
        let requirement = requirement(&[("case", "failed")]);
        for text in [
            r#"{"reason":"build-finished","success":true}"#.to_owned(),
            "test case ... ok\ntest result: ok. 1 passed; 0 failed".to_owned(),
            json!({"example": suite(&[("case", "ok")])}).to_string(),
        ] {
            assert!(!requirement.is_verified_by(&Proof::capture(&output(text))));
        }
    }

    #[test]
    fn later_failure_chunks_strengthen_coverage_and_invalid_tails_never_erase_it() {
        let all = suite(&[("a", "failed"), ("b", "failed")]);
        let prefix = all.lines().take(4).collect::<Vec<_>>().join("\n");
        let mut requirement = Requirement::capture(&output(prefix), false);
        let all_passed = Proof::capture(&output(suite(&[("a", "ok"), ("b", "ok")])));
        assert!(!requirement.is_verified_by(&all_passed));
        requirement.refresh(&output(all.clone()), true);
        assert!(requirement.is_verified_by(&all_passed));
        let skipped = Proof::capture(&output(suite(&[("a", "ignored"), ("b", "ok")])));
        assert!(!requirement.is_verified_by(&skipped));
        requirement.refresh(&output(format!("{all}\n{{")), true);
        assert!(!requirement.is_verified_by(&all_passed));
        requirement.refresh(&output("unrelated plain text".to_owned()), true);
        assert!(!requirement.is_verified_by(&all_passed));
    }

    #[test]
    fn partial_fail_fast_and_truncated_processes_have_no_complete_inventory_proof() {
        let complete = suite(&[("case", "failed")]);
        let proof = Proof::capture(&output(suite(&[("case", "ok")])));
        assert!(!Requirement::capture(&output(complete.clone()), false).is_verified_by(&proof));
        let partial = complete.lines().take(3).collect::<Vec<_>>().join("\n");
        assert!(!Requirement::capture(&output(partial), true).is_verified_by(&proof));
        let fail_fast = complete.replace("\"test_count\":1", "\"test_count\":20");
        assert!(compiler::inspect(&fail_fast).is_ok());
        assert!(!Requirement::capture(&output(fail_fast), true).is_verified_by(&proof));
    }

    #[test]
    fn coverage_never_replaces_process_status_or_exact_non_test_error_rules() {
        let requirement = requirement(&[("case", "failed")]);
        let mut passed = output(suite(&[("case", "ok")]));
        passed.exit_code = Some(101);
        assert!(!requirement.is_verified_by(&Proof::capture(&passed)));
        let native = output(r#"{"schema":"ee.error.v2","error":{"code":"migration_required","message":"migrate first"}}"#.into());
        assert!(!Requirement::capture(&native, true).is_guarded());
        assert_ne!(name_commitment("Case"), name_commitment("case"));
        assert_ne!(name_commitment("path::case"), name_commitment("case"));
    }

    fn lint(files: &[(&str, bool)], wrapped: bool) -> ToolOutput {
        let files = files
            .iter()
            .map(|(path, failed)| {
                let messages = if *failed {
                    vec![json!({"severity": 2, "message": "Unused cache value", "ruleId": "no-unused-vars"})]
                } else {
                    Vec::new()
                };
                json!({
                    "filePath": path, "messages": messages,
                    "errorCount": u64::from(*failed), "fatalErrorCount": 0, "warningCount": 0
                })
            })
            .collect::<Vec<_>>();
        output(if wrapped {
            json!({"results": files, "metadata": {"rulesMeta": {}}}).to_string()
        } else {
            json!(files).to_string()
        })
    }

    #[test]
    fn lint_proof_covers_every_failed_file_across_both_formats() {
        for wrapped_failure in [false, true] {
            let failed = lint(&[("/src/a.js", true), ("/src/b.js", true)], wrapped_failure);
            assert_eq!(super::super::failure_diagnostics(&failed).len(), 1);
            let requirement = Requirement::capture(&failed, true);
            assert!(requirement.is_guarded());
            for wrapped_proof in [false, true] {
                for missing in [
                    vec![("/src/a.js", false)],
                    vec![("/src/A.js", false), ("/src/b.js", false)],
                    vec![("/src/a.js", false), ("/src/other.js", false)],
                ] {
                    let proof = lint(&missing, wrapped_proof);
                    assert!(proof.succeeded());
                    assert!(!requirement.is_verified_by(&Proof::capture(&proof)));
                }
                let complete = lint(
                    &[
                        ("/src/extra.js", false),
                        ("/src/b.js", false),
                        ("/src/a.js", false),
                    ],
                    wrapped_proof,
                );
                assert!(requirement.is_verified_by(&Proof::capture(&complete)));
            }
        }
    }

    #[test]
    fn lint_cannot_inherit_generic_json_test_success_or_incomplete_process_proof() {
        let failed = lint(&[("/src/a.js", true)], false);
        let requirement = Requirement::capture(&failed, true);
        for proof in [
            output("[]".to_owned()),
            output("[\"ok\"]".to_owned()),
            output("{\"success\":true}".to_owned()),
            output(suite(&[("case", "ok")])),
        ] {
            assert!(!requirement.is_verified_by(&Proof::capture(&proof)));
        }
        let mut proof = lint(&[("/src/a.js", false)], true);
        let known_good = Proof::capture(&proof);
        assert!(requirement.is_verified_by(&known_good));
        proof.exit_code = Some(1);
        assert!(!requirement.is_verified_by(&Proof::capture(&proof)));
        assert!(!Requirement::capture(&failed, false).is_verified_by(&known_good));
        let mut damaged = Requirement::capture(&failed, false);
        damaged.refresh(&failed, true);
        assert!(damaged.is_verified_by(&known_good));
        damaged.refresh(&output(format!("{} trailing", failed.text)), true);
        assert!(!damaged.is_verified_by(&known_good));
        damaged.refresh(&output("unrelated".to_owned()), true);
        assert!(!damaged.is_verified_by(&known_good));
    }

    fn go_suite(package: &str, results: &[(&str, &str)]) -> ToolOutput {
        let mut events = vec![json!({"Action":"start","Package":package})];
        for (name, action) in results {
            events.push(json!({"Action":"run","Package":package,"Test":name}));
            events.push(json!({"Action":action,"Package":package,"Test":name,"Elapsed":0.01}));
        }
        let failed = results.iter().any(|(_, action)| *action == "fail");
        events.push(
            json!({"Action":if failed {"fail"} else {"pass"},"Package":package,"Elapsed":0.1}),
        );
        output(
            events
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    #[test]
    fn go_retry_must_pass_the_same_package_and_failed_test_not_an_unrelated_success() {
        let failed = go_suite("example.org/cache", &[("TestCache", "fail")]);
        let requirement = Requirement::capture(&failed, true);
        assert!(requirement.is_guarded());
        for passed in [
            go_suite("example.org/other", &[("TestCache", "pass")]),
            go_suite("example.org/cache", &[("TestOther", "pass")]),
            go_suite("example.org/cache", &[("testCache", "pass")]),
            go_suite(
                "example.org/cache",
                &[("TestCache", "skip"), ("TestOther", "pass")],
            ),
        ] {
            assert!(passed.succeeded());
            assert!(!requirement.is_verified_by(&Proof::capture(&passed)));
        }
        let passed = go_suite(
            "example.org/cache",
            &[("TestOther", "pass"), ("TestCache", "pass")],
        );
        assert!(passed.succeeded());
        assert!(requirement.is_verified_by(&Proof::capture(&passed)));
    }

    #[test]
    fn go_coverage_keeps_every_failed_subject_beyond_the_diagnostic_cap() {
        let names: Vec<_> = (0..8).map(|i| format!("TestCacheCase{i}")).collect();
        let failed: Vec<_> = names.iter().map(|name| (name.as_str(), "fail")).collect();
        let failure = go_suite("example.org/cache", &failed);
        let required = Requirement::capture(&failure, true);
        let decoded = compiler::inspect(&failure.text).unwrap().unwrap();
        assert!(decoded.diagnostics.len() <= 4);
        assert_eq!(decoded.go_test_coverage.unwrap().failed_tests.len(), 8);
        let mut passed: Vec<_> = names.iter().map(|name| (name.as_str(), "pass")).collect();
        passed[7].1 = "skip";
        let skipped = go_suite("example.org/cache", &passed);
        assert!(skipped.succeeded());
        assert!(!required.is_verified_by(&Proof::capture(&skipped)));
        passed[7].1 = "pass";
        assert!(required.is_verified_by(&Proof::capture(&go_suite("example.org/cache", &passed))));
    }

    #[test]
    fn go_coverage_cannot_cross_report_families_or_ignore_process_failure() {
        let failed = go_suite("example.org/cache", &[("TestCache", "fail")]);
        let required = Requirement::capture(&failed, true);
        for passed in [
            output(suite(&[("TestCache", "ok")])),
            lint(&[("TestCache", false)], false),
        ] {
            assert!(passed.succeeded());
            assert!(!required.is_verified_by(&Proof::capture(&passed)));
        }
        let mut passed = go_suite("example.org/cache", &[("TestCache", "pass")]);
        let good = Proof::capture(&passed);
        assert!(required.is_verified_by(&good));
        assert!(!Requirement::capture(&failed, false).is_verified_by(&good));
        let mut incomplete = Requirement::capture(&failed, false);
        incomplete.refresh(&failed, true);
        assert!(incomplete.is_verified_by(&good));
        incomplete.refresh(&output(format!("{}\n{{", failed.text)), true);
        assert!(!incomplete.is_verified_by(&good));
        passed.exit_code = Some(1);
        assert!(!required.is_verified_by(&Proof::capture(&passed)));
    }

    #[test]
    fn a_skipped_repeat_cannot_inherit_an_earlier_pass_of_the_same_test() {
        let required = Requirement::capture(&go_suite("a", &[("TestCache", "fail")]), true);
        for results in [
            [
                ("TestCache", "pass"),
                ("TestCache", "skip"),
                ("Other", "pass"),
            ],
            [
                ("TestCache", "skip"),
                ("TestCache", "pass"),
                ("Other", "pass"),
            ],
        ] {
            let passed = go_suite("a", &results);
            assert!(passed.succeeded());
            assert!(!required.is_verified_by(&Proof::capture(&passed)));
        }
        assert!(required.is_verified_by(&Proof::capture(&go_suite(
            "a",
            &[("TestCache", "pass"), ("TestCache", "pass")]
        ))));
    }
}
