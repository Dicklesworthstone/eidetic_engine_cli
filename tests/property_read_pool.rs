//! bd-2caru.9 property coverage for `PoolConfig` invariants.
//!
//! These are pure-value property tests over the committed read-pool
//! builder surface in `src/db/read_pool.rs`. They do not touch
//! SQLite; they pin the invariants that the read pool's runtime
//! semantics rely on so that future refactors of the builder chain
//! cannot silently break callers (`src/core/context.rs`,
//! `src/core/status.rs`).
//!
//! The bead `bd-2caru.9` also requires `property_read_pool_determinism`
//! and `property_read_pool_snapshot_isolation` against real SQLite
//! pools — those are owned by a follow-up slice once the read-pool
//! API stabilises after `bd-2caru.7` lands its acquire-timeout
//! / ad-hoc-bypass surface. This file is the value-level half of the
//! coverage and is intentionally scoped to not need a fsqlite handle.

use std::time::Duration;

use ee::db::read_pool::PoolConfig;
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

fn small_duration() -> impl Strategy<Value = Duration> {
    (0u64..=600).prop_map(Duration::from_secs)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// `max_size()` always clamps to `>= 1` even when the caller asks
    /// for `0`, and `requested_max_size()` faithfully echoes the
    /// caller-provided value. `size_was_zero()` is true iff the input
    /// was `0`. This is what callers in `acquire()` rely on to decide
    /// whether to emit `read_pool_size_was_zero` degradations.
    #[test]
    fn pool_config_size_zero_clamps_to_one_and_records_origin(
        requested in 0usize..=64,
        idle in small_duration(),
    ) {
        let config = PoolConfig::new(requested, idle);
        prop_assert_eq!(config.requested_max_size(), requested);
        prop_assert!(config.max_size() >= 1, "max_size must always be >= 1");
        if requested == 0 {
            prop_assert_eq!(config.max_size(), 1);
            prop_assert!(config.size_was_zero(), "size_was_zero must be true when requested=0");
        } else {
            prop_assert_eq!(config.max_size(), requested);
            prop_assert!(!config.size_was_zero(), "size_was_zero must be false when requested>0");
        }
    }

    /// `with_max_pin_duration` mutates only `max_pin_duration`; the
    /// other four accessors (`requested_max_size`, `idle_timeout`,
    /// `acquire_timeout`, `size_was_zero`) round-trip untouched.
    #[test]
    fn pool_config_with_max_pin_duration_preserves_other_fields(
        requested in 0usize..=64,
        idle in small_duration(),
        pin in small_duration(),
    ) {
        let base = PoolConfig::new(requested, idle);
        let updated = base.clone().with_max_pin_duration(pin);

        prop_assert_eq!(updated.requested_max_size(), base.requested_max_size());
        prop_assert_eq!(updated.idle_timeout(), base.idle_timeout());
        prop_assert_eq!(updated.acquire_timeout(), base.acquire_timeout());
        prop_assert_eq!(updated.size_was_zero(), base.size_was_zero());
        prop_assert_eq!(updated.max_pin_duration(), pin);
    }

    /// `with_acquire_timeout` mutates only `acquire_timeout`; the
    /// other accessors (`requested_max_size`, `idle_timeout`,
    /// `max_pin_duration`, `size_was_zero`) round-trip untouched.
    #[test]
    fn pool_config_with_acquire_timeout_preserves_other_fields(
        requested in 0usize..=64,
        idle in small_duration(),
        acquire in small_duration(),
    ) {
        let base = PoolConfig::new(requested, idle);
        let updated = base.clone().with_acquire_timeout(acquire);

        prop_assert_eq!(updated.requested_max_size(), base.requested_max_size());
        prop_assert_eq!(updated.idle_timeout(), base.idle_timeout());
        prop_assert_eq!(updated.max_pin_duration(), base.max_pin_duration());
        prop_assert_eq!(updated.size_was_zero(), base.size_was_zero());
        prop_assert_eq!(updated.acquire_timeout(), acquire);
    }

    /// The two builder helpers are order-independent and idempotent.
    /// `cfg.with_max_pin_duration(p).with_acquire_timeout(a)` and
    /// `cfg.with_acquire_timeout(a).with_max_pin_duration(p)` produce
    /// equal `PoolConfig` values, and applying the same setter twice
    /// is the same as applying it once.
    #[test]
    fn pool_config_builders_are_commutative_and_idempotent(
        requested in 0usize..=64,
        idle in small_duration(),
        pin in small_duration(),
        acquire in small_duration(),
    ) {
        let a = PoolConfig::new(requested, idle)
            .with_max_pin_duration(pin)
            .with_acquire_timeout(acquire);
        let b = PoolConfig::new(requested, idle)
            .with_acquire_timeout(acquire)
            .with_max_pin_duration(pin);
        prop_assert_eq!(&a, &b);

        let twice = a
            .clone()
            .with_max_pin_duration(pin)
            .with_acquire_timeout(acquire);
        prop_assert_eq!(&twice, &a);
    }
}

#[test]
fn pool_config_default_single_matches_default_impl() {
    let single = PoolConfig::default_single();
    let default_impl = PoolConfig::default();
    assert_eq!(single, default_impl);
    assert_eq!(single.max_size(), 1);
    assert_eq!(single.requested_max_size(), 1);
    assert!(!single.size_was_zero());
    assert!(single.idle_timeout() > Duration::ZERO);
    assert!(single.max_pin_duration() > Duration::ZERO);
    assert!(single.acquire_timeout() > Duration::ZERO);
}
