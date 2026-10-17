//! A repair may add regression coverage, never substitute or merge old suites.

use super::super::{Proof as CompleteProof, Requirement as CompleteRequirement};
use super::tests::{output, suite};
use super::{Proof, Requirement};

fn report(suites: &[&[(&str, &str)]]) -> super::ToolOutput {
    output(
        suites
            .iter()
            .map(|results| suite(results))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

#[test]
fn added_regression_test_preserves_the_complete_prior_inventory() {
    let failure = report(&[&[("cache", "failed"), ("control", "ok")]]);
    let required = Requirement::capture(&failure, true);
    let retry = report(&[&[("control", "ok"), ("regression", "ok"), ("cache", "ok")]]);
    assert!(retry.succeeded());
    assert!(required.is_verified_by(&Proof::capture(&retry)));
}

#[test]
fn plain_and_json_growth_agree_without_changing_source_bytes() {
    let plain_failed = output("running 2 tests\ntest cache ... FAILED\ntest control ... ok\n\ntest result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n".to_owned());
    let plain_passed = output("running 3 tests\ntest cache ... ok\ntest control ... ok\ntest regression ... ok\n\ntest result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n".to_owned());
    let json_failed = report(&[&[("cache", "failed"), ("control", "ok")]]);
    let json_passed = report(&[&[("cache", "ok"), ("control", "ok"), ("regression", "ok")]]);
    let original = plain_failed.text.clone();
    for failed in [&plain_failed, &json_failed] {
        let required = CompleteRequirement::capture(failed, true);
        assert!(required.is_guarded());
        for passed in [&plain_passed, &json_passed] {
            assert!(required.is_verified_by(&CompleteProof::capture(passed)));
        }
    }
    assert_eq!(plain_failed.text, original);
}

#[test]
fn growth_cannot_drop_rename_skip_or_replace_previous_coverage() {
    let failure = report(&[&[
        ("cache", "failed"),
        ("control", "ok"),
        ("optional", "ignored"),
    ]]);
    let required = Requirement::capture(&failure, true);
    for results in [
        vec![
            ("cache", "ignored"),
            ("control", "ok"),
            ("optional", "ignored"),
            ("new", "ok"),
        ],
        vec![
            ("cache", "ok"),
            ("control", "ignored"),
            ("optional", "ignored"),
            ("new", "ok"),
        ],
        vec![
            ("cache", "ok"),
            ("replacement", "ok"),
            ("optional", "ignored"),
            ("new", "ok"),
        ],
        vec![
            ("Cache", "ok"),
            ("control", "ok"),
            ("optional", "ignored"),
            ("new", "ok"),
        ],
        vec![("cache", "ok"), ("control", "ok"), ("new", "ok")],
    ] {
        let retry = report(&[&results]);
        assert!(retry.succeeded());
        assert!(
            !required.is_verified_by(&Proof::capture(&retry)),
            "{results:?}"
        );
    }
    let retry = report(&[&[
        ("cache", "ok"),
        ("control", "ok"),
        ("optional", "ok"),
        ("new", "ok"),
    ]]);
    assert!(required.is_verified_by(&Proof::capture(&retry)));
}

#[test]
fn successful_control_suites_are_not_disposable_during_growth() {
    let failure = report(&[&[("cache", "failed")], &[("control", "ok")]]);
    let required = Requirement::capture(&failure, true);
    let partial = report(&[&[("cache", "ok"), ("new", "ok")]]);
    assert!(!required.is_verified_by(&Proof::capture(&partial)));
    let complete = report(&[
        &[("control", "ok"), ("control_regression", "ok")],
        &[("cache", "ok"), ("new", "ok")],
    ]);
    assert!(required.is_verified_by(&Proof::capture(&complete)));
}

#[test]
fn suite_growth_is_order_independent_and_never_merges_or_splits_binaries() {
    let required = Requirement::capture(&report(&[&[("a", "failed")], &[("b", "ok")]]), true);
    for retry in [
        report(&[
            &[("a", "ok"), ("a_new", "ok")],
            &[("b", "ok"), ("b_new", "ok")],
        ]),
        report(&[
            &[("b_new", "ok"), ("b", "ok")],
            &[("a_new", "ok"), ("a", "ok")],
        ]),
    ] {
        assert!(required.is_verified_by(&Proof::capture(&retry)));
    }
    let merged = report(&[&[("a", "ok"), ("b", "ok"), ("new", "ok")]]);
    assert!(!required.is_verified_by(&Proof::capture(&merged)));
    let required = Requirement::capture(&report(&[&[("a", "failed"), ("b", "ok")]]), true);
    let split = report(&[
        &[("a", "ok"), ("new", "ok")],
        &[("b", "ok"), ("extra", "ok")],
    ]);
    assert!(!required.is_verified_by(&Proof::capture(&split)));
}

#[test]
fn exact_inventory_bindings_win_before_growth_candidates() {
    let failure = report(&[
        &[("common", "failed")],
        &[("common", "ok"), ("control", "ok")],
    ]);
    let retry = report(&[
        &[("common", "ok")],
        &[("common", "ok"), ("control", "ok"), ("new", "ok")],
    ]);
    assert!(Requirement::capture(&failure, true).is_verified_by(&Proof::capture(&retry)));
    let skipped_exact = report(&[
        &[("common", "ignored")],
        &[("common", "ok"), ("control", "ok"), ("new", "ok")],
    ]);
    assert!(!Requirement::capture(&failure, true).is_verified_by(&Proof::capture(&skipped_exact)));
}

#[test]
fn globally_forced_assignment_is_admitted_but_ambiguous_growth_abstains() {
    let failure = report(&[&[("a", "failed")], &[("a", "ok"), ("b", "ok")]]);
    let forced = report(&[
        &[("a", "ok"), ("b", "ok"), ("new", "ok")],
        &[("a", "ok"), ("other", "ok")],
    ]);
    assert!(Requirement::capture(&failure, true).is_verified_by(&Proof::capture(&forced)));
    let ambiguous_failure = report(&[&[("a", "failed")], &[("b", "ok")]]);
    let ambiguous = report(&[
        &[("a", "ok"), ("b", "ok"), ("new", "ok")],
        &[("a", "ok"), ("b", "ok"), ("other", "ok")],
    ]);
    assert!(
        !Requirement::capture(&ambiguous_failure, true).is_verified_by(&Proof::capture(&ambiguous))
    );
    let one = Requirement::capture(&report(&[&[("a", "failed")]]), true);
    assert!(!one.is_verified_by(&Proof::capture(&ambiguous)));
}

#[test]
fn new_failure_and_original_process_vetoes_remain_authoritative() {
    let failure = report(&[&[("a", "failed")]]);
    let good = report(&[&[("a", "ok"), ("new", "ok")]]);
    let required = Requirement::capture(&failure, true);
    assert!(required.is_verified_by(&Proof::capture(&good)));
    assert!(!Requirement::capture(&failure, false).is_verified_by(&Proof::capture(&good)));
    let extra_failure = report(&[&[("a", "ok"), ("new", "failed")]]);
    assert!(!required.is_verified_by(&Proof::capture(&extra_failure)));
    for broken in [
        output(format!("{}\n{{", good.text)),
        output(good.text.replace("\"test_count\":2", "\"test_count\":3")),
    ] {
        assert!(!required.is_verified_by(&Proof::capture(&broken)));
    }
    let mut veto = good;
    veto.exit_code = Some(101);
    assert!(!required.is_verified_by(&Proof::capture(&veto)));
}

#[test]
fn bounded_correspondence_agrees_with_exhaustive_assignment() {
    fn count_assignments(required: &[u8], retries: &[u8], used: u16) -> usize {
        let Some((&first, rest)) = required.split_first() else {
            return 1;
        };
        let mut count = 0;
        for (index, candidate) in retries.iter().enumerate() {
            if used & (1 << index) == 0 && first & candidate == first {
                count += count_assignments(rest, retries, used | (1 << index));
                if count > 1 {
                    return 2;
                }
            }
        }
        count
    }
    fn oracle(required: &[u8], retries: &[u8]) -> bool {
        let mut used = 0;
        let mut pending = Vec::new();
        for subject in required {
            if let Some(index) = retries.iter().position(|candidate| subject == candidate) {
                used |= 1 << index;
            } else {
                pending.push(*subject);
            }
        }
        count_assignments(&pending, retries, used) == 1
    }
    fn names(mask: u8) -> Vec<super::Name> {
        (0..4)
            .filter(|bit| mask & (1 << bit) != 0)
            .map(|bit| {
                let mut name = [0; 32];
                name[31] = bit;
                name
            })
            .collect()
    }
    let mut cases = 0;
    for first in 1_u8..8 {
        for second in first + 1..8 {
            let required = [first, second].map(|mask| {
                let inventory = names(mask);
                super::RequiredSuite {
                    must_pass: inventory.clone(),
                    inventory,
                }
            });
            for selection in 0_u16..(1 << 15) {
                if selection.count_ones() > 3 {
                    continue;
                }
                let retries = (1_u8..16)
                    .filter(|mask| selection & (1 << (mask - 1)) != 0)
                    .collect::<Vec<_>>();
                let execution = super::Execution {
                    suites: retries
                        .iter()
                        .map(|mask| {
                            let inventory = names(*mask);
                            let outcomes = inventory
                                .iter()
                                .map(|name| (*name, super::Outcome::Passed))
                                .collect();
                            (inventory, outcomes)
                        })
                        .collect(),
                    complete: true,
                    allows_completion: true,
                };
                assert_eq!(
                    super::libtest_growth::verified_by(&required, &execution),
                    oracle(&[first, second], &retries),
                    "required={first},{second}; retries={retries:?}"
                );
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 12_096);
}

#[path = "cass_error_recall_libtest_growth_store_tests.rs"]
mod store_tests;
