//! Exact subject indexes for unresolved failure observations.
//!
//! These indexes only narrow the existing causal matcher; they never choose a
//! repair, relax admission, or change an evidence identity. An eligible exact
//! resource match excludes all non-anchors in the original matcher. Otherwise
//! only the matching topic, explicit markers, and a sole pronoun antecedent can
//! affect its answer. Preserve key order, including ties between marked spans.

use std::collections::{BTreeMap, BTreeSet};

use super::{FailureKey, PendingFailure};

type Keys = BTreeSet<FailureKey>;

#[derive(Default)]
pub(super) struct PendingFailures<'a> {
    entries: BTreeMap<FailureKey, PendingFailure<'a>>,
    resources: BTreeMap<String, Keys>,
    topics: BTreeMap<String, Keys>,
    marked: Keys,
}

impl<'a> PendingFailures<'a> {
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.resources.clear();
        self.topics.clear();
        self.marked.clear();
    }

    pub(super) fn insert(&mut self, key: FailureKey, failure: PendingFailure<'a>) {
        // A replacement can change marker eligibility even when its subject
        // is unchanged. Remove every old posting before indexing the new row.
        let _ = self.remove(&key);
        for resource in &key.1 {
            self.resources
                .entry(resource.clone())
                .or_default()
                .insert(key.clone());
        }
        self.topics
            .entry(key.0.clone())
            .or_default()
            .insert(key.clone());
        if failure.explicitly_marked {
            self.marked.insert(key.clone());
        }
        self.entries.insert(key, failure);
    }

    pub(super) fn remove(&mut self, key: &FailureKey) -> Option<PendingFailure<'a>> {
        let failure = self.entries.remove(key)?;
        for resource in &key.1 {
            remove_posting(&mut self.resources, resource, key);
        }
        remove_posting(&mut self.topics, &key.0, key);
        self.marked.remove(key);
        Some(failure)
    }

    /// Completed work retains only explicitly disjoint subjects. The eventual
    /// repair must name a retained resource, exactly as before indexing.
    pub(super) fn retain_disjoint(&mut self, observed: &BTreeSet<String>) {
        if observed.is_empty() {
            self.clear();
            return;
        }
        let mut removed = Vec::new();
        for (key, failure) in &mut self.entries {
            if !key.1.is_empty() && key.1.is_disjoint(observed) {
                failure.requires_resource_match = true;
            } else {
                removed.push(key.clone());
            }
        }
        for key in removed {
            let _ = self.remove(&key);
        }
    }

    /// Narrow by subjects before performing temporal and resource-barrier
    /// checks. The predicate is the original matcher's eligibility test.
    /// A resource posting alone never proves eligibility: overlapping source
    /// ranges, repeated IDs and future failures still cannot be antecedents.
    pub(super) fn eligible<'s>(
        &'s self,
        topic: &str,
        resources: &BTreeSet<String>,
        include_marked: bool,
        mut admits: impl FnMut(&FailureKey, &PendingFailure<'a>) -> bool,
    ) -> Vec<(&'s FailureKey, &'s PendingFailure<'a>)> {
        let mut keys = BTreeSet::new();
        for resource in resources {
            if let Some(postings) = self.resources.get(resource) {
                keys.extend(postings.iter());
            }
        }
        let anchored: Vec<_> = keys
            .iter()
            .filter_map(|key| self.entries.get_key_value(*key))
            .filter(|(key, failure)| admits(key, failure))
            .collect();
        if !anchored.is_empty() {
            return anchored;
        }

        keys.clear();
        if let Some(postings) = self.topics.get(topic) {
            keys.extend(
                postings
                    .iter()
                    .filter(|key| resources.is_empty() || key.1.is_empty()),
            );
        }
        if include_marked {
            keys.extend(self.marked.iter());
        }
        if self.entries.len() == 1
            && let Some((key, _)) = self.entries.first_key_value()
        {
            keys.insert(key);
        }
        keys.into_iter()
            .filter_map(|key| self.entries.get_key_value(key))
            .filter(|(key, failure)| admits(key, failure))
            .collect()
    }
}

fn remove_posting(index: &mut BTreeMap<String, Keys>, subject: &str, key: &FailureKey) {
    let empty = index.get_mut(subject).is_some_and(|keys| {
        keys.remove(key);
        keys.is_empty()
    });
    if empty {
        index.remove(subject);
    }
}

#[cfg(test)]
#[path = "curate_session_arc_subject_index_tests.rs"]
mod tests;
