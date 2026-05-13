//! N4.2 tests for the deterministic runtime capability token.

use ee::runtime::determinism::{
    DeterminismError, Deterministic, RANDOMNESS_INVENTORY_ROWS_CONTENT_HASH, RandomnessConsumer,
    SeedSource,
};

type TestResult<T = ()> = Result<T, String>;

const SNAPSHOT: &str = include_str!("snapshots/determinism_token_doctest_output.snap");

fn assert_send<T: Send>() {}

#[test]
fn explicit_seed_construction_records_source_and_scope() {
    let token = Deterministic::from_seed(42);
    assert_eq!(token.seed().as_u64(), 42);
    assert_eq!(token.source(), SeedSource::Explicit);
    assert_eq!(token.scope(), "root");
    assert_eq!(token.seed_hash_prefix().len(), 12);
}

#[test]
fn persistent_timestamp_and_env_seed_sources_are_deterministic() -> TestResult {
    let persistent_a = Deterministic::from_persistent_seed("workspace:/repo/a");
    let persistent_b = Deterministic::from_persistent_seed("workspace:/repo/a");
    assert_eq!(persistent_a.seed(), persistent_b.seed());
    assert_eq!(persistent_a.source(), SeedSource::PersistentWorkspace);

    let timestamp_a = Deterministic::from_timestamp_second("2026-05-13T10:11:12.999Z")
        .map_err(|error| error.to_string())?;
    let timestamp_b = Deterministic::from_timestamp_second("2026-05-13T10:11:12.001Z")
        .map_err(|error| error.to_string())?;
    assert_eq!(
        timestamp_a.seed(),
        timestamp_b.seed(),
        "timestamp seeds truncate to second precision"
    );
    assert_eq!(timestamp_a.source(), SeedSource::TimestampSecond);

    let env_token = Deterministic::from_env_value("12345").map_err(|error| error.to_string())?;
    assert_eq!(env_token.seed().as_u64(), 12345);
    assert_eq!(env_token.source(), SeedSource::Env);

    Ok(())
}

#[test]
fn invalid_env_seed_value_is_reported() {
    match Deterministic::from_env_value("not-a-number") {
        Ok(_) => panic!("invalid seed should fail"),
        Err(error) => assert_eq!(
            error,
            DeterminismError::InvalidSeed {
                value: "not-a-number".to_owned()
            }
        ),
    }
}

#[test]
fn child_split_is_reproducible_and_distinct() {
    let mut parent_a = Deterministic::from_seed(9);
    let child_a_1 = parent_a.child("retrieval");
    let child_a_2 = parent_a.child("retrieval");

    let mut parent_b = Deterministic::from_seed(9);
    let child_b_1 = parent_b.child("retrieval");

    assert_eq!(child_a_1.seed(), child_b_1.seed());
    assert_ne!(
        child_a_1.seed(),
        child_a_2.seed(),
        "same-label children split from one parent still get distinct ordinals"
    );
    assert!(child_a_1.scope().contains("retrieval#0"));
}

#[test]
fn deterministic_rng_replays_same_bytes() {
    let mut token_a = Deterministic::from_seed(77);
    let mut token_b = Deterministic::from_seed(77);
    let mut bytes_a = [0_u8; 24];
    let mut bytes_b = [0_u8; 24];

    token_a.rng().fill_bytes(&mut bytes_a);
    token_b.rng().fill_bytes(&mut bytes_b);

    assert_eq!(bytes_a, bytes_b);
    assert_ne!(bytes_a, [0_u8; 24]);
}

#[test]
fn deterministic_consumers_are_token_constructed_and_named() {
    let mut token = Deterministic::from_seed(10);
    assert_eq!(token.rng().consumer_kind(), "deterministic_rng");
    assert_eq!(token.clock().consumer_kind(), "deterministic_clock");
    assert_eq!(token.order().consumer_kind(), "deterministic_order");
}

#[test]
fn deterministic_order_sorts_by_stable_key() {
    let mut token = Deterministic::from_seed(12);
    let mut values = vec!["gamma", "alpha", "beta"];

    token.order().sort_by_key(&mut values, |value| *value);

    assert_eq!(values, ["alpha", "beta", "gamma"]);
}

#[test]
fn uuid_v7_clock_is_monotonic_and_replayable() {
    let mut token_a = Deterministic::from_seed(1_000);
    let first = token_a.clock().next_uuid_v7();
    let second = token_a.clock().next_uuid_v7();
    assert!(first < second);

    let mut token_b = Deterministic::from_seed(1_000);
    assert_eq!(first, token_b.clock().next_uuid_v7());
    assert_eq!(second, token_b.clock().next_uuid_v7());
}

#[test]
fn uuid_v7_cross_scope_order_follows_seed_precedence() {
    let mut low_seed = Deterministic::from_seed(1);
    let mut high_seed = Deterministic::from_seed(2);

    let low = low_seed.clock().next_uuid_v7();
    let high = high_seed.clock().next_uuid_v7();

    assert!(low < high);
}

#[test]
fn token_is_send_but_not_sync_by_doctest_contract() {
    assert_send::<Deterministic>();
}

#[test]
fn inventory_hash_is_cited_by_the_module() {
    assert_eq!(
        RANDOMNESS_INVENTORY_ROWS_CONTENT_HASH,
        "blake3-ish:51a8854727a5768008ba8269596e8666cc9ffdd88e8ac3f13101ad36434a3bfc"
    );
}

#[test]
fn deterministic_sequence_snapshot_is_stable() {
    let mut token = Deterministic::from_seed(17);
    let mut child = token.child("pack");
    let uuid = child.clock().next_uuid_v7();
    let first_word = child.rng().next_u64();
    let summary = format!(
        "schema: ee.determinism_token.snapshot.v1\nseed: {}\nsource: {}\nscope: {}\nseed_hash_prefix: {}\nfirst_uuid_v7: {}\nfirst_rng_u64: {}\ninventory_hash: {}\n",
        child.seed().as_u64(),
        child.source().as_str(),
        child.scope(),
        child.seed_hash_prefix(),
        uuid,
        first_word,
        RANDOMNESS_INVENTORY_ROWS_CONTENT_HASH
    );

    assert_eq!(summary, SNAPSHOT);
}
