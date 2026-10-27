//! Exercise coverage preservation through the shared production proof boundary.
//! Producer fixtures use the existing decoder helpers, not replacement storage
//! or a reference verifier. No persisted diagnostic or evidence shape changes.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::super::{Proof, Requirement};
use super::ToolOutput;
use super::tests::{go_suite, lint, output, suite};

fn verified(failure: &ToolOutput, retry: &ToolOutput) -> bool {
    let required = Requirement::capture(failure, true);
    assert!(required.is_guarded());
    required.is_verified_by(&Proof::capture(retry))
}

#[test]
fn go_preserves_passed_controls_when_accepting_added_regressions() {
    let failure = go_suite(
        "example.org/cache",
        &[("TestCache", "fail"), ("TestControl", "pass")],
    );
    let complete = go_suite(
        "example.org/cache",
        &[
            ("TestRegression", "pass"),
            ("TestControl", "pass"),
            ("TestCache", "pass"),
        ],
    );
    assert!(verified(&failure, &complete));
    for results in [
        vec![("TestCache", "pass"), ("TestRegression", "pass")],
        vec![("TestCache", "pass"), ("testControl", "pass")],
        vec![("TestCache", "pass"), ("TestControl", "skip")],
    ] {
        let retry = go_suite("example.org/cache", &results);
        assert!(retry.succeeded());
        assert!(!verified(&failure, &retry));
    }
}

#[test]
fn go_preserves_successful_packages_in_a_multi_package_invocation() {
    let failed_package = go_suite("example.org/cache", &[("TestCache", "fail")]);
    let clean_package = go_suite("example.org/control", &[("TestControl", "pass")]);
    let failure = output(format!("{}\n{}", failed_package.text, clean_package.text));
    let repaired = go_suite("example.org/cache", &[("TestCache", "pass")]);
    let extra = go_suite("example.org/new", &[("TestRegression", "pass")]);
    let complete = output(format!(
        "{}\n{}\n{}",
        extra.text, clean_package.text, repaired.text
    ));
    assert!(verified(&failure, &complete));
    assert!(!verified(&failure, &repaired));
    let renamed = go_suite("example.org/Control", &[("TestControl", "pass")]);
    let wrong_scope = output(format!("{}\n{}", repaired.text, renamed.text));
    assert!(wrong_scope.succeeded());
    assert!(!verified(&failure, &wrong_scope));
}

#[test]
fn go_does_not_replace_prior_passes_with_cached_package_summaries() {
    let failure = output(format!(
        "{}\n{}",
        go_suite("a", &[("TestFailure", "fail")]).text,
        go_suite("b", &[("TestControl", "pass")]).text,
    ));
    let summary_only = output(format!(
        "{}\n{}",
        go_suite("a", &[("TestFailure", "pass")]).text,
        go_suite("b", &[]).text,
    ));
    assert!(summary_only.succeeded());
    assert!(!verified(&failure, &summary_only));
}

#[test]
fn eslint_preserves_clean_files_across_json_report_shapes() {
    for before_wrapped in [false, true] {
        let failure = lint(
            &[("/src/cache.js", true), ("/src/control.js", false)],
            before_wrapped,
        );
        for after_wrapped in [false, true] {
            let complete = lint(
                &[
                    ("/src/regression.js", false),
                    ("/src/control.js", false),
                    ("/src/cache.js", false),
                ],
                after_wrapped,
            );
            assert!(verified(&failure, &complete));
            for files in [
                vec![("/src/cache.js", false), ("/src/regression.js", false)],
                vec![("/src/cache.js", false), ("/src/Control.js", false)],
            ] {
                let retry = lint(&files, after_wrapped);
                assert!(retry.succeeded());
                assert!(!verified(&failure, &retry));
            }
        }
    }
}

#[test]
fn eslint_added_coverage_does_not_hide_new_errors_or_damaged_reports() {
    let failure = lint(
        &[("/src/cache.js", true), ("/src/control.js", false)],
        false,
    );
    let mut retry = lint(
        &[
            ("/src/cache.js", false),
            ("/src/control.js", false),
            ("/src/new.js", true),
        ],
        true,
    );
    assert!(!verified(&failure, &retry));
    retry = lint(
        &[("/src/control.js", false), ("/src/cache.js", false)],
        true,
    );
    assert!(verified(&failure, &retry));
    retry.text.push_str(" trailing");
    assert!(!verified(&failure, &retry));
}

#[test]
fn libtest_preserves_successful_and_ignored_suite_inventories() {
    let failure = output(format!(
        "{}\n{}\n{}",
        suite(&[("failure", "failed")]),
        suite(&[("control", "ok")]),
        suite(&[("ignored_control", "ignored")]),
    ));
    let complete = output(format!(
        "{}\n{}\n{}\n{}",
        suite(&[("new_binary_regression", "ok")]),
        suite(&[("ignored_control", "ignored")]),
        suite(&[("control", "ok")]),
        suite(&[("failure", "ok")]),
    ));
    assert!(verified(&failure, &complete));
    for missing in [
        format!(
            "{}\n{}",
            suite(&[("failure", "ok")]),
            suite(&[("ignored_control", "ignored")])
        ),
        format!(
            "{}\n{}",
            suite(&[("failure", "ok")]),
            suite(&[("control", "ok")])
        ),
        format!(
            "{}\n{}\n{}",
            suite(&[("failure", "ok")]),
            suite(&[("replacement", "ok")]),
            suite(&[("ignored_control", "ignored")])
        ),
    ] {
        let retry = output(missing);
        assert!(retry.succeeded());
        assert!(!verified(&failure, &retry));
    }
}

#[test]
fn libtest_previous_passes_require_real_execution_on_retry() {
    let failure = output(suite(&[("failure", "failed"), ("control", "ok")]));
    let ignored = output(suite(&[("failure", "ok"), ("control", "ignored")]));
    assert!(ignored.succeeded());
    assert!(!verified(&failure, &ignored));
    let complete = output(suite(&[("control", "ok"), ("failure", "ok")]));
    assert!(verified(&failure, &complete));
    let separate = output(format!(
        "{}\n{}",
        suite(&[("failure", "failed")]),
        suite(&[("control", "ok")])
    ));
    let skipped_suite = output(format!(
        "{}\n{}",
        suite(&[("failure", "ok")]),
        suite(&[("control", "ignored")])
    ));
    assert!(skipped_suite.succeeded());
    assert!(!verified(&separate, &skipped_suite));
}

#[test]
fn libtest_retains_its_anonymous_suite_identity_contract() {
    let failure = output(suite(&[("failure", "failed"), ("control", "ok")]));
    let changed_inventory = output(suite(&[
        ("failure", "ok"),
        ("control", "ok"),
        ("new_test", "ok"),
    ]));
    assert!(changed_inventory.succeeded());
    // A single coverage-preserving extension is unambiguous. Two expanded
    // anonymous binaries containing the old inventory cannot be disambiguated.
    assert!(verified(&failure, &changed_inventory));
    let ambiguous = output(format!(
        "{}\n{}",
        changed_inventory.text,
        suite(&[("failure", "ok"), ("control", "ok"), ("another_test", "ok"),])
    ));
    assert!(ambiguous.succeeded());
    assert!(!verified(&failure, &ambiguous));
}

#[test]
fn clean_controls_beyond_the_diagnostic_display_cap_still_matter() {
    let names = (0..16)
        .map(|index| format!("TestControl{index}"))
        .collect::<Vec<_>>();
    let mut before = vec![("TestFailure", "fail")];
    before.extend(names.iter().map(|name| (name.as_str(), "pass")));
    let failure = go_suite("example.org/cache", &before);
    let mut after = before.clone();
    after[0].1 = "pass";
    let complete = go_suite("example.org/cache", &after);
    assert!(verified(&failure, &complete));
    after.pop();
    assert!(!verified(&failure, &go_suite("example.org/cache", &after)));
    let files = (0..16)
        .map(|index| format!("/src/control-{index}.js"))
        .collect::<Vec<_>>();
    let mut before = vec![("/src/failure.js", true)];
    before.extend(files.iter().map(|file| (file.as_str(), false)));
    let failure = lint(&before, true);
    let mut after = before.clone();
    after[0].1 = false;
    assert!(verified(&failure, &lint(&after, false)));
    after.pop();
    assert!(!verified(&failure, &lint(&after, false)));
}

#[test]
fn later_polls_keep_newly_observed_controls_and_do_not_downgrade() {
    let initial = go_suite("a", &[("TestFailure", "fail")]);
    let accumulated = go_suite("a", &[("TestFailure", "fail"), ("TestControl", "pass")]);
    let mut required = Requirement::capture(&initial, true);
    let old_retry = go_suite("a", &[("TestFailure", "pass")]);
    assert!(required.is_verified_by(&Proof::capture(&old_retry)));
    required.refresh(&accumulated, true);
    assert!(!required.is_verified_by(&Proof::capture(&old_retry)));
    let complete = go_suite("a", &[("TestFailure", "pass"), ("TestControl", "pass")]);
    assert!(required.is_verified_by(&Proof::capture(&complete)));
    required.refresh(&output(format!("{}\n{{", accumulated.text)), true);
    assert!(!required.is_verified_by(&Proof::capture(&complete)));
    required.refresh(&output("unrelated output".to_owned()), true);
    assert!(!required.is_verified_by(&Proof::capture(&complete)));
}

#[test]
fn preserved_scope_does_not_replace_process_completion() {
    for (failure, mut retry) in [
        (
            go_suite("a", &[("TestFailure", "fail"), ("TestControl", "pass")]),
            go_suite("a", &[("TestFailure", "pass"), ("TestControl", "pass")]),
        ),
        (
            lint(&[("/a.js", true), ("/b.js", false)], false),
            lint(&[("/a.js", false), ("/b.js", false)], true),
        ),
        (
            output(suite(&[("failure", "failed"), ("control", "ok")])),
            output(suite(&[("failure", "ok"), ("control", "ok")])),
        ),
    ] {
        assert!(verified(&failure, &retry));
        assert!(!Requirement::capture(&failure, false).is_verified_by(&Proof::capture(&retry)));
        retry.exit_code = Some(1);
        assert!(!verified(&failure, &retry));
        retry.exit_code = Some(0);
        retry.is_error = Some(true);
        assert!(!verified(&failure, &retry));
    }
}

#[test]
fn wholly_clean_reports_do_not_create_new_failure_requirements() {
    for clean in [
        go_suite("a", &[("TestControl", "pass")]),
        lint(&[("/src/control.js", false)], false),
        output(suite(&[("control", "ok")])),
    ] {
        assert!(!Requirement::capture(&clean, true).is_guarded());
    }
}
