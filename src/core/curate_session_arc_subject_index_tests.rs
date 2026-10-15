//! Compare indexed selection to the original full-scan matcher, not a second
//! interpretation of its policy. Cost assertions count eligibility probes.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::super::tests::{endpoints, mine, span};
use super::super::{
    StoredEvidenceSpan, matching_failure, matching_subject, precedes, refers_to_previous_failure,
    session_arc_span_order,
};
use super::*;
use std::borrow::Cow;

fn key(topic: &str, resources: &[&str]) -> FailureKey {
    (
        topic.to_owned(),
        resources
            .iter()
            .map(|resource| (*resource).to_owned())
            .collect(),
    )
}

fn failure(source: &StoredEvidenceSpan, marked: bool) -> PendingFailure<'_> {
    PendingFailure {
        source,
        message: Cow::Borrowed(&source.excerpt),
        explicitly_marked: marked,
        requires_resource_match: false,
    }
}

fn assert_consistent(index: &PendingFailures<'_>) {
    let mut resources = BTreeMap::<String, Keys>::new();
    let mut topics = BTreeMap::<String, Keys>::new();
    let mut marked = Keys::new();
    for (key, failure) in &index.entries {
        topics.entry(key.0.clone()).or_default().insert(key.clone());
        for resource in &key.1 {
            resources
                .entry(resource.clone())
                .or_default()
                .insert(key.clone());
        }
        if failure.explicitly_marked {
            marked.insert(key.clone());
        }
    }
    assert_eq!(index.resources, resources);
    assert_eq!(index.topics, topics);
    assert_eq!(index.marked, marked);
}

// The pre-index implementation, kept as a behavioral oracle. In particular,
// marker recency, ambiguity and the global pending count must not drift.
fn full_scan(
    pending: &PendingFailures<'_>,
    topic: &str,
    message: &str,
    repair: &StoredEvidenceSpan,
    resources: &BTreeSet<String>,
) -> Option<FailureKey> {
    let eligible: Vec<_> = pending
        .entries
        .iter()
        .filter(|(key, failure)| {
            precedes(failure.source, repair)
                && (!failure.requires_resource_match || !resources.is_disjoint(&key.1))
        })
        .collect();
    let subjects: Vec<_> = eligible.iter().map(|(key, _)| *key).collect();
    let anchored = subjects.iter().any(|key| !resources.is_disjoint(&key.1));
    if !anchored && message.to_ascii_lowercase().contains("fix:") {
        if let Some((key, _)) = eligible
            .iter()
            .copied()
            .filter(|(_, failure)| failure.explicitly_marked)
            .max_by(|(_, left), (_, right)| session_arc_span_order(left.source, right.source))
        {
            return (resources.is_empty() || key.1.is_empty()).then(|| key.clone());
        }
    }
    if let Some(key) = matching_subject(&subjects, topic, resources) {
        return Some(key);
    }
    if pending.len() == 1 && refers_to_previous_failure(message) {
        let (key, failure) = eligible.first()?;
        if failure.source.end_line.checked_add(1) == Some(repair.start_line)
            && (resources.is_empty() || key.1.is_empty())
        {
            return Some((*key).clone());
        }
    }
    None
}

#[test]
fn postings_follow_replacement_consumption_barriers_and_clear() {
    let first = span("first", 1, "The build failed.");
    let later = span("later", 2, "The build failed again.");
    let a = key("build", &["src/a.rs", "src/shared.rs"]);
    let b = key("build", &["src/b.rs"]);
    let anonymous = key("cache", &[]);
    let mut index = PendingFailures::default();
    index.insert(a.clone(), failure(&first, true));
    index.insert(b.clone(), failure(&first, true));
    index.insert(anonymous, failure(&first, false));
    assert_consistent(&index);
    index.insert(a.clone(), failure(&later, false));
    assert_consistent(&index);
    assert!(!index.marked.contains(&a));
    assert_eq!(index.entries[&a].source.id, "later");
    index.retain_disjoint(&BTreeSet::from(["src/b.rs".to_owned()]));
    assert_consistent(&index);
    assert_eq!(index.len(), 1);
    assert!(index.entries[&a].requires_resource_match);
    assert!(index.remove(&b).is_none());
    assert!(index.remove(&a).is_some());
    assert_consistent(&index);
    assert!(index.is_empty());
    index.insert(a, failure(&first, true));
    index.clear();
    assert_consistent(&index);
    assert!(index.is_empty());
}

#[test]
fn indexed_matcher_agrees_with_full_scan_across_ambiguous_and_ineligible_subjects() {
    let topics = ["build", "cache", "noise"];
    let resource_sets = [
        vec![],
        vec!["src/a.rs"],
        vec!["src/b.rs"],
        vec!["src/a.rs", "src/b.rs"],
        vec!["src/unseen.rs"],
    ];
    let mut state = 0x81a2_37d9_u64;
    let mut next = || {
        state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        usize::try_from(state >> 32).expect("32-bit generator value")
    };
    for _ in 0..32 {
        let mut sources: Vec<_> = (0..32_u32)
            .map(|i| span(&format!("failure-{i}"), i + 1, "The build failed."))
            .collect();
        // Include inverted locations, which cannot supply an antecedent.
        for source in sources.iter_mut().step_by(7) {
            source.end_line = 0;
        }
        let mut index = PendingFailures::default();
        for source in &sources {
            let key = key(
                topics[next() % 3],
                &resource_sets[next() % 5],
            );
            let mut row = failure(source, next().is_multiple_of(2));
            row.requires_resource_match = next().is_multiple_of(3);
            index.insert(key, row);
        }
        assert_consistent(&index);
        for line in [0, 1, 2, 16, 40, u32::MAX] {
            for repair_id in ["repair", "failure-31"] {
                let repair = span(repair_id, line, "Fixed the build.");
                for topic in topics {
                    for resources in &resource_sets {
                        let resources = key(topic, resources).1;
                        for message in [
                            "Fixed the build.",
                            "Fix: restored the guard.",
                            "Fixed it by restoring the guard.",
                        ] {
                            assert_eq!(
                                matching_failure(&index, topic, message, &repair, &resources),
                                full_scan(&index, topic, message, &repair, &resources),
                                "topic={topic} resources={resources:?} line={line} message={message}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn singleton_pronoun_and_noneligible_anchor_fallback_match_the_original_policy() {
    let source = span("failure", 1, "The build failed.");
    let mut index = PendingFailures::default();
    index.insert(key("build", &["src/a.rs"]), failure(&source, true));
    for line in [1, 2, 3] {
        let repair = span("repair", line, "Fixed it by restoring the guard.");
        for resources in [
            BTreeSet::new(),
            key("noise", &["src/a.rs"]).1,
            key("noise", &["src/b.rs"]).1,
        ] {
            for message in [
                "Fixed it by restoring the guard.",
                "Fix: restored the guard.",
            ] {
                assert_eq!(
                    matching_failure(&index, "noise", message, &repair, &resources),
                    full_scan(&index, "noise", message, &repair, &resources)
                );
            }
        }
    }
    assert!(
        matching_failure(
            &index,
            "noise",
            "Fixed it by restoring the guard.",
            &span("repair", 2, "repair"),
            &BTreeSet::new()
        )
        .is_some()
    );
}

#[test]
fn exact_resource_lookup_does_not_probe_unrelated_pending_failures() {
    let sources: Vec<_> = (0..1024_u32)
        .map(|i| span(&format!("failure-{i}"), i + 1, "The build failed."))
        .collect();
    let mut index = PendingFailures::default();
    for (i, source) in sources.iter().enumerate() {
        index.insert(
            key("build", &[&format!("src/item-{i}.rs")]),
            failure(source, true),
        );
    }
    let resources = BTreeSet::from(["src/item-731.rs".to_owned()]);
    let mut probes = 0;
    let selected = index.eligible("build", &resources, true, |_, _| {
        probes += 1;
        true
    });
    assert_eq!(probes, 1);
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].1.source.id, "failure-731");
}

#[test]
fn interleaved_sessions_keep_every_original_reciprocal_pair() {
    let mut sources = Vec::new();
    for i in 0..64_u32 {
        sources.push(span(
            &format!("failure-{i}"),
            i + 1,
            &format!("The build failed in src/part-{i}.rs."),
        ));
    }
    for i in 0..64_u32 {
        sources.push(span(
            &format!("repair-{i}"),
            i + 65,
            &format!("Fixed src/part-{i}.rs by restoring the guard."),
        ));
    }
    let original = sources.clone();
    let rows = mine(&sources);
    assert_eq!(rows.len(), 128);
    let actual = endpoints(&rows);
    for (i, (failure, repair)) in actual.iter().enumerate() {
        assert_eq!(*failure, format!("failure-{i}"));
        assert_eq!(*repair, format!("repair-{i}"));
    }
    for row in &rows {
        let arc = row.session_arc.as_ref().expect("original source pair");
        for endpoint in [&arc.failure_span, &arc.resolution_span] {
            let source = sources
                .iter()
                .find(|source| source.id == endpoint.evidence_span_id)
                .unwrap();
            assert_eq!(endpoint.content_hash, source.content_hash);
            assert_eq!(endpoint.excerpt, source.excerpt);
        }
    }
    assert_eq!(sources, original);
    sources.reverse();
    assert_eq!(
        mine(&sources),
        rows,
        "input enumeration does not change mining order"
    );
}
