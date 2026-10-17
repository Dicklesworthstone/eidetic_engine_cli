//! Coverage-preserving libtest suite growth without guessing binary identity.
//!
//! Libtest's native stream does not name the test binary. An unchanged complete
//! inventory is the existing identity anchor. For changed inventories require
//! a unique one-to-one extension after reserving those exact anchors. Every old
//! selected subject stays present and every previously executed subject passes.
//! No union across suites, greedy first-fit, or factorial assignment search.

use std::collections::{BTreeSet, VecDeque};

use super::{Execution, Outcome, RequiredSuite};

// A separate work cap bounds correspondence checks even for highly overlapping
// inventories. Exhaustion refuses proof; it never authorizes a checked prefix.
const MAX_GROWTH_CHECKS: usize = 262_144;

pub(super) fn verified_by(required: &[RequiredSuite], execution: &Execution) -> bool {
    verified_with_budget(required, execution, MAX_GROWTH_CHECKS)
}

fn verified_with_budget(
    required: &[RequiredSuite],
    execution: &Execution,
    mut budget: usize,
) -> bool {
    let mut anchored = BTreeSet::new();
    let mut pending = Vec::new();
    for suite in required {
        if let Some(outcomes) = execution.suites.get(&suite.inventory) {
            // An exact suite that skipped an old test cannot borrow a pass
            // from a different expanded binary with a coinciding test name.
            if !suite
                .must_pass
                .iter()
                .all(|name| outcomes.get(name) == Some(&Outcome::Passed))
            {
                return false;
            }
            anchored.insert(&suite.inventory);
        } else {
            pending.push(suite);
        }
    }
    if pending.is_empty() {
        return true;
    }
    let retries: Vec<_> = execution
        .suites
        .iter()
        .filter(|(inventory, _)| !anchored.contains(inventory))
        .collect();
    if pending.len() > retries.len() {
        return false;
    }
    let mut choices = Vec::with_capacity(pending.len());
    let mut users = vec![Vec::new(); retries.len()];
    for (index, suite) in pending.iter().enumerate() {
        let mut eligible = Vec::new();
        for (retry_index, (_, outcomes)) in retries.iter().enumerate() {
            let Some(covered) = covers(suite, outcomes, &mut budget) else {
                return false;
            };
            if covered {
                eligible.push(retry_index);
                users[retry_index].push(index);
            }
        }
        if eligible.is_empty() {
            return false;
        }
        choices.push(eligible);
    }

    let mut counts: Vec<_> = choices.iter().map(Vec::len).collect();
    let mut ready: VecDeque<_> = counts
        .iter()
        .enumerate()
        .filter_map(|(index, count)| (*count == 1).then_some(index))
        .collect();
    let mut used = vec![false; retries.len()];
    let mut assigned = vec![false; pending.len()];
    let mut matched = 0;
    while let Some(index) = ready.pop_front() {
        if assigned[index] {
            continue;
        }
        let Some(&retry) = choices[index].iter().find(|&&retry| !used[retry]) else {
            return false;
        };
        assigned[index] = true;
        used[retry] = true;
        matched += 1;
        for &other in &users[retry] {
            if assigned[other] {
                continue;
            }
            counts[other] -= 1;
            match counts[other] {
                0 => return false,
                1 => ready.push_back(other),
                _ => {}
            }
        }
    }
    // Without a forced choice the remaining correspondence is either
    // impossible or ambiguous. A unique assignment must expose a singleton:
    // otherwise following alternatives produces a cycle or an unused retry.
    matched == pending.len()
}

fn covers(
    required: &RequiredSuite,
    outcomes: &std::collections::BTreeMap<super::Name, Outcome>,
    budget: &mut usize,
) -> Option<bool> {
    *budget = budget.checked_sub(1)?;
    if outcomes.len() < required.inventory.len() {
        return Some(false);
    }
    for name in &required.inventory {
        *budget = budget.checked_sub(1)?;
        if !outcomes.contains_key(name) {
            return Some(false);
        }
    }
    for name in &required.must_pass {
        *budget = budget.checked_sub(1)?;
        if outcomes.get(name) != Some(&Outcome::Passed) {
            return Some(false);
        }
    }
    Some(true)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{output, suite};
    use super::super::{Proof, Requirement};
    use super::*;

    #[test]
    fn correspondence_budget_exhaustion_never_credits_a_prefix() {
        let failure = output(suite(&[("cache", "failed"), ("control", "ok")]));
        let passed = output(suite(&[
            ("cache", "ok"),
            ("control", "ok"),
            ("regression", "ok"),
        ]));
        let Requirement::Libtest(required) = Requirement::capture(&failure, true) else {
            panic!("fixture must carry native libtest coverage");
        };
        let proof = Proof::capture(&passed);
        let execution = proof.execution.as_ref().expect("complete execution");
        assert!(verified_by(&required, execution));
        for budget in 0..5 {
            assert!(!verified_with_budget(&required, execution, budget));
        }
        assert!(verified_with_budget(&required, execution, 5));
    }
}
