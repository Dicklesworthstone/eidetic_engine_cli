use super::*;

fn revision() -> CorpusRevision {
    CorpusRevision::from("corpus:resident")
}

fn topics(store: &CassPrefetchHistoryStore, agent: &str, workspace: &str) -> Vec<String> {
    store
        .history_for(&AgentScope::new(agent), workspace)
        .map(|history| {
            history
                .iter()
                .map(|item| item.topic_id.as_str().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn generation_change_does_not_relabel_old_topics() {
    let mut coordinator = CassPrefetchCoordinator::new();
    let agent = AgentScope::new("agent");
    let rev = revision();
    let old = PrefetchGeneration::new(0, 7);
    let new = PrefetchGeneration::new(0, 8);
    for topic in ["old-cass", "old-current"] {
        coordinator.observe(agent.clone(), "ws", topic, old, &rev);
    }
    assert!(
        !coordinator
            .schedule(&agent, "ws", old, &rev)
            .candidates
            .is_empty()
    );
    assert_eq!(
        coordinator.schedule(&agent, "ws", new, &rev).degraded,
        Some(CASS_PREFETCH_STALE_GENERATION_CODE)
    );
    coordinator.observe(agent.clone(), "ws", "new-cass", new, &rev);
    assert!(
        coordinator
            .schedule(&agent, "ws", new, &rev)
            .candidates
            .is_empty()
    );
    coordinator.observe(agent.clone(), "ws", "new-current", new, &rev);
    let prediction = coordinator.schedule(&agent, "ws", new, &rev);
    assert_eq!(prediction.degraded, None);
    assert_eq!(prediction.candidates.len(), 1);
    assert_eq!(prediction.candidates[0].topic_id, "new-cass");
}

#[test]
fn workspace_generation_change_restarts_only_its_owner() {
    let mut store = CassPrefetchHistoryStore::new(10);
    let rev = revision();
    let old = PrefetchGeneration::new(1, 7);
    for (agent, workspace) in [("a", "ws"), ("b", "ws"), ("a", "other")] {
        store.observe(agent, workspace, "old", old, &rev);
    }
    store.observe("a", "ws", "new", PrefetchGeneration::new(2, 7), &rev);
    assert_eq!(topics(&store, "a", "ws"), ["new"]);
    assert_eq!(topics(&store, "b", "ws"), ["old"]);
    assert_eq!(topics(&store, "a", "other"), ["old"]);
}

#[test]
fn corpus_change_recovers_without_reusing_or_waiting_out_old_history() {
    let mut coordinator = CassPrefetchCoordinator::new();
    let agent = AgentScope::new("a");
    let generation = PrefetchGeneration::new(0, 7);
    let old = revision();
    let new = CorpusRevision::from("corpus:replacement");
    for topic in ["old-a", "old-b", "old-c"] {
        coordinator.observe(agent.clone(), "ws", topic, generation, &old);
    }
    assert_eq!(
        coordinator
            .schedule(&agent, "ws", generation, &new)
            .degraded,
        Some(CASS_PREFETCH_STALE_CORPUS_REVISION_CODE)
    );
    coordinator.observe(agent.clone(), "ws", "new-a", generation, &new);
    coordinator.observe(agent.clone(), "ws", "new-b", generation, &new);
    let prediction = coordinator.schedule(&agent, "ws", generation, &new);
    assert_eq!(prediction.degraded, None);
    assert_eq!(prediction.candidates.len(), 1);
    assert_eq!(prediction.candidates[0].topic_id, "new-a");
}

#[test]
fn rotating_agents_cannot_grow_residency_without_bound() {
    let mut store = CassPrefetchHistoryStore::new(10);
    for number in 0..MAX_PREFETCH_RESIDENT_HISTORIES * 3 {
        store.observe(
            format!("agent-{number}"),
            "ws",
            "query",
            PrefetchGeneration::new(0, 1),
            &revision(),
        );
        assert!(store.len() <= MAX_PREFETCH_RESIDENT_HISTORIES);
        assert_eq!(store.observed_order.len(), store.len());
    }
    assert!(
        store
            .history_for(&AgentScope::new("agent-0"), "ws")
            .is_none()
    );
    assert_eq!(store.len(), MAX_PREFETCH_RESIDENT_HISTORIES);
}

#[test]
fn recent_activity_survives_deterministic_eviction() {
    let mut store = CassPrefetchHistoryStore::new(2);
    let generation = PrefetchGeneration::new(0, 1);
    for number in 0..MAX_PREFETCH_RESIDENT_HISTORIES {
        store.observe(
            format!("agent-{number}"),
            "ws",
            "old",
            generation,
            &revision(),
        );
    }
    store.observe("agent-0", "ws", "active", generation, &revision());
    store.observe("new-agent", "ws", "new", generation, &revision());
    assert_eq!(topics(&store, "agent-0", "ws"), ["active", "old"]);
    assert!(
        store
            .history_for(&AgentScope::new("agent-1"), "ws")
            .is_none()
    );
    assert_eq!(store.len(), MAX_PREFETCH_RESIDENT_HISTORIES);
    assert_eq!(store.observed_order.len(), store.len());
}

#[test]
fn oversized_inputs_do_not_pollute_or_evict_valid_history() {
    let mut store = CassPrefetchHistoryStore::new(10);
    let generation = PrefetchGeneration::new(0, 1);
    let rev = revision();
    store.observe("agent", "ws", "valid", generation, &rev);
    store.observe(
        "agent",
        "ws",
        "x".repeat(MAX_PREFETCH_TOPIC_ID_BYTES + 1),
        generation,
        &rev,
    );
    store.observe(
        "a".repeat(MAX_PREFETCH_OWNER_BYTES + 1),
        "ws",
        "valid",
        generation,
        &rev,
    );
    store.observe(
        "agent",
        "w".repeat(MAX_PREFETCH_OWNER_BYTES + 1),
        "valid",
        generation,
        &rev,
    );
    store.observe(
        "agent",
        "ws",
        "valid",
        generation,
        &CorpusRevision::from("r".repeat(MAX_PREFETCH_OWNER_BYTES + 1)),
    );
    assert_eq!(store.len(), 1);
    assert_eq!(topics(&store, "agent", "ws"), ["valid"]);
}

#[test]
fn admitted_topics_still_pass_the_canonical_redactor() {
    let mut store = CassPrefetchHistoryStore::new(10);
    let secret = format!("sk-proj-{}", "a".repeat(44));
    let content = format!("deploy {secret}");
    store.observe(
        "agent",
        "ws",
        content,
        PrefetchGeneration::new(0, 1),
        &revision(),
    );
    let retained = topics(&store, "agent", "ws");
    assert_eq!(retained.len(), 1);
    assert!(!retained[0].contains(&secret));
    assert!(retained[0].starts_with("deploy "));
}

#[test]
fn coherent_history_preserves_the_existing_predictor_order() {
    let mut store = CassPrefetchHistoryStore::new(3);
    let generation = PrefetchGeneration::new(0, 1);
    let rev = revision();
    for topic in ["discarded", "beta", "alpha", "current"] {
        store.observe("agent", "ws", topic, generation, &rev);
    }
    let history = store.history_for(&AgentScope::new("agent"), "ws").unwrap();
    let expected = CassPrefetchHistory::from_topics("agent", ["current", "alpha", "beta"])
        .with_generation(generation)
        .with_corpus_revision(rev.clone());
    let predictor = RecencyWeightedFrequencyPredictor::new();
    assert_eq!(history, &expected);
    assert_eq!(
        predictor.predict_next_n_gated_for_revision(history, generation, &rev, 3),
        predictor.predict_next_n_gated_for_revision(&expected, generation, &rev, 3)
    );
}
