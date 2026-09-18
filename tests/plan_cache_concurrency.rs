//! Public process-cache regression coverage. Keep the scenarios in one test:
//! this binary owns one global cache, so independent test cases must not reset
//! it underneath one another. Channels coordinate overlap without timing sleeps.

#![allow(clippy::expect_used)]

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use ee::models::query::{EqlQuery, EqlSpeedMode, EqlTagsMode};
use ee::search::plan_cache::{
    CompiledPlan, EnvVarValueSource, PlanCacheDecision, PlanCacheKey,
    lookup_or_insert_process_plan, process_plan_cache_diag_report,
    reset_process_plan_cache_for_tests,
};

const CAPACITY: usize = 8;
const DEADLINE: Duration = Duration::from_secs(5);

fn key(value: u64) -> PlanCacheKey {
    PlanCacheKey::new(value, 10, 100)
}

fn plan(text: &str) -> CompiledPlan {
    CompiledPlan::from_query(EqlQuery {
        q: text.to_owned(),
        workspace: None,
        levels: Vec::new(),
        kinds: Vec::new(),
        tags: Vec::new(),
        tags_mode: EqlTagsMode::Any,
        scope: Vec::new(),
        time: None,
        confidence: None,
        graph: None,
        limit: 10,
        speed: EqlSpeedMode::Default,
        rerank: false,
        return_subgraph: false,
        explain: false,
    })
}

fn cached_keys(capacity: usize) -> Vec<u64> {
    process_plan_cache_diag_report(capacity, EnvVarValueSource::RegistryDefault, usize::MAX)
        .top_keys
        .into_iter()
        .map(|entry| entry.eql_hash)
        .collect()
}

#[test]
fn process_cache_compilation_is_unlocked_and_publication_is_generation_safe() {
    reset_process_plan_cache_for_tests(CAPACITY);
    lookup_or_insert_process_plan(CAPACITY, key(1), || plan("hot"));

    // A cold compiler remains blocked on a channel while a warm lookup and
    // diagnostics complete. Holding the write lock across compile would make
    // the worker's deadline expire before the foreground can release it.
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        lookup_or_insert_process_plan(CAPACITY, key(2), || {
            started_tx.send(()).expect("notify compiler entry");
            release_rx
                .recv_timeout(DEADLINE)
                .expect("release cold compiler");
            plan("cold")
        })
    });
    started_rx
        .recv_timeout(DEADLINE)
        .expect("cold compiler started");
    let hot =
        lookup_or_insert_process_plan(CAPACITY, key(1), || panic!("warm lookup must not compile"));
    assert_eq!(hot.decision, PlanCacheDecision::Hit);
    assert_eq!(cached_keys(CAPACITY), vec![1]);
    release_tx
        .send(())
        .expect("release compiler after warm lookup");
    assert_eq!(
        worker.join().expect("cold compiler completed").decision,
        PlanCacheDecision::Miss
    );
    assert_eq!(cached_keys(CAPACITY), vec![1, 2]);

    // Another caller publishes the same key during compilation. The late
    // compiler must reuse that verified winner, not overwrite it.
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        lookup_or_insert_process_plan(CAPACITY, key(3), || {
            started_tx.send(()).expect("notify late compiler entry");
            release_rx
                .recv_timeout(DEADLINE)
                .expect("release late compiler");
            plan("late")
        })
    });
    started_rx
        .recv_timeout(DEADLINE)
        .expect("late compiler started");
    let winner = lookup_or_insert_process_plan(CAPACITY, key(3), || plan("winner"));
    release_tx.send(()).expect("release losing compiler");
    let late = worker.join().expect("late compiler completed");
    assert_eq!(late.decision, PlanCacheDecision::Hit);
    assert_eq!(late.plan, winner.plan);
    assert_eq!(late.plan_tree_hash, winner.plan_tree_hash);

    // Re-entrant reset at the SAME capacity must invalidate publication. A
    // capacity-only recheck would silently reinsert the stale result.
    let stale = lookup_or_insert_process_plan(CAPACITY, key(4), || {
        reset_process_plan_cache_for_tests(CAPACITY);
        lookup_or_insert_process_plan(CAPACITY, key(5), || plan("new generation"));
        plan("old generation")
    });
    assert_eq!(stale.decision, PlanCacheDecision::Miss);
    assert_eq!(stale.plan.parsed_query.q, "old generation");
    assert_eq!(cached_keys(CAPACITY), vec![5]);
    let mut recompiled = false;
    lookup_or_insert_process_plan(CAPACITY, key(4), || {
        recompiled = true;
        plan("fresh")
    });
    assert!(
        recompiled,
        "invalidated compilation must not have been cached"
    );

    // A late result must not reset a newer capacity or evict its entries.
    let stale = lookup_or_insert_process_plan(CAPACITY, key(6), || {
        lookup_or_insert_process_plan(2, key(7), || plan("resized"));
        plan("before resize")
    });
    assert_eq!(stale.decision, PlanCacheDecision::Miss);
    assert_eq!(cached_keys(2), vec![7]);

    // A compiler panic leaves previously cached plans usable.
    reset_process_plan_cache_for_tests(CAPACITY);
    lookup_or_insert_process_plan(CAPACITY, key(1), || plan("hot"));
    let failure = std::panic::catch_unwind(|| {
        lookup_or_insert_process_plan(CAPACITY, key(8), || panic!("compiler failure"))
    });
    assert!(failure.is_err());
    let hot = lookup_or_insert_process_plan(CAPACITY, key(1), || {
        panic!("compiler failure must not discard a warm plan")
    });
    assert_eq!(hot.decision, PlanCacheDecision::Hit);
    assert_eq!(cached_keys(CAPACITY), vec![1]);

    // Disabling the cache still runs each requested compile and stores nothing.
    reset_process_plan_cache_for_tests(0);
    let mut compiles = 0;
    for _ in 0..2 {
        let result = lookup_or_insert_process_plan(0, key(9), || {
            compiles += 1;
            plan("uncached")
        });
        assert_eq!(result.decision, PlanCacheDecision::Miss);
    }
    assert_eq!(compiles, 2);
    assert!(cached_keys(0).is_empty());
}
