//! Collection repairs require executed descendants from explicit report edges.
//!
//! A missing import can prevent pytest from discovering test identities at
//! all. A later successful collection is insufficient: a passed descendant
//! must be observed under the exact formerly failing collector. Never infer
//! this relation from a path prefix, basename, or overall test count.

use std::collections::{BTreeMap, BTreeSet};

use super::{Collector, MAX_RESULTS, Subject, commitment, valid_identity};

pub(super) fn executed_collectors(
    collectors: &[Collector],
    passed: &BTreeSet<Subject>,
) -> Result<BTreeSet<Subject>, ()> {
    let by_id: BTreeMap<_, _> = collectors
        .iter()
        .map(|collector| (collector.nodeid.as_str(), collector))
        .collect();
    if by_id.len() != collectors.len() || collectors.len() > MAX_RESULTS {
        return Err(());
    }
    let mut parents = BTreeMap::new();
    let mut count = 0_usize;
    for collector in collectors {
        for child in collector.result.as_deref().unwrap_or(&[]) {
            count += 1;
            if count > MAX_RESULTS
                || !valid_identity(&child.nodeid)
                || child.nodeid == collector.nodeid
                || parents
                    .insert(child.nodeid.as_str(), collector.nodeid.as_str())
                    .is_some()
            {
                // Duplicate edges or multiple owners make attribution
                // ambiguous; do not choose an owner by iteration order.
                return Err(());
            }
        }
    }
    let mut executed = BTreeSet::new();
    for &node in parents.keys() {
        if !passed.contains(&commitment("nodeid", node)) {
            continue;
        }
        if by_id.contains_key(node) {
            return Err(());
        }
        let mut current = node;
        let mut visited = BTreeSet::new();
        while let Some(&parent) = parents.get(current) {
            if !visited.insert(parent) {
                return Err(());
            }
            let collector = by_id.get(parent).ok_or(())?;
            if collector.outcome != "passed" {
                break;
            }
            if !parent.is_empty() {
                executed.insert(commitment("collector", parent));
            }
            current = parent;
        }
    }
    Ok(executed)
}
