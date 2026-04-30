//! Deterministic async test helpers for ee (EE-013).
//!
//! This module wraps Asupersync's `LabRuntime` and related test utilities
//! to provide deterministic, reproducible async testing for ee's domain logic.
//!
//! # Features
//!
//! - Deterministic scheduling: same seed produces identical execution order
//! - Virtual time: no wall-clock dependencies in tests
//! - Fixed test fixtures: canonical timestamps, IDs, and seeds
//! - Helper macros for common test patterns
//!
//! # Usage
//!
//! ```ignore
//! use ee::testing::{lab_runtime, TEST_SEED, TEST_TIMESTAMP};
//!
//! #[test]
//! fn async_test_is_deterministic() {
//!     let mut runtime = lab_runtime(TEST_SEED);
//!     // ... test async logic with deterministic scheduling ...
//! }
//! ```

use asupersync::lab::{LabConfig, LabRuntime};

/// Default seed for deterministic tests.
///
/// Using the same seed ensures identical scheduling across test runs.
/// Choose a different seed only when testing randomness-dependent behavior.
pub const TEST_SEED: u64 = 42;

/// Secondary seed for tests that need two independent runtimes.
pub const TEST_SEED_ALT: u64 = 7;

/// Canonical timestamp for test fixtures (RFC 3339).
///
/// Using a fixed timestamp ensures deterministic ID generation and
/// time-based ordering in tests.
pub const TEST_TIMESTAMP: &str = "2026-01-01T00:00:00Z";

/// Canonical workspace ID for test fixtures (30 chars).
pub const TEST_WORKSPACE_ID: &str = "wsp_test0000000000000000000000";

/// Canonical memory ID for test fixtures (30 chars).
pub const TEST_MEMORY_ID: &str = "mem_test0000000000000000000000";

/// Canonical audit ID for test fixtures (32 chars).
pub const TEST_AUDIT_ID: &str = "audit_test0000000000000000000000";

/// Create a deterministic lab runtime with the given seed.
///
/// The lab runtime provides:
/// - Virtual time (no wall-clock dependencies)
/// - Deterministic task scheduling
/// - Identical execution order for the same seed
///
/// # Example
///
/// ```ignore
/// use ee::testing::{lab_runtime, TEST_SEED};
///
/// let mut runtime = lab_runtime(TEST_SEED);
/// assert_eq!(runtime.steps(), 0);
/// ```
#[must_use]
pub fn lab_runtime(seed: u64) -> LabRuntime {
    LabRuntime::new(LabConfig::new(seed))
}

/// Create a lab runtime with the default test seed.
///
/// Equivalent to `lab_runtime(TEST_SEED)`.
#[must_use]
pub fn default_lab_runtime() -> LabRuntime {
    lab_runtime(TEST_SEED)
}

/// Create a lab runtime with light chaos injection enabled.
///
/// Light chaos is suitable for CI: low probability faults that stress-test
/// error handling without excessive test flakiness.
///
/// Chaos injection includes:
/// - Random cancellations at poll points (1%)
/// - Artificial delays to simulate slow operations (5%)
/// - Spurious wakeups to test waker correctness
#[must_use]
pub fn chaos_lab_runtime(seed: u64) -> LabRuntime {
    LabRuntime::new(LabConfig::new(seed).with_light_chaos())
}

/// Assert that two lab runtimes with the same seed produce identical state.
///
/// This is a contract test: if it fails, determinism is broken.
///
/// # Panics
///
/// Panics if the runtimes have different initial state.
pub fn assert_deterministic_runtimes(seed: u64) {
    let first = lab_runtime(seed);
    let second = lab_runtime(seed);

    assert_eq!(
        first.now(),
        second.now(),
        "Lab runtimes with seed {seed} must have identical start time"
    );
    assert_eq!(
        first.steps(),
        second.steps(),
        "Lab runtimes with seed {seed} must have identical step count"
    );
}

/// Run a synchronous test function with a fresh lab runtime.
///
/// This helper creates a runtime, runs the test, and ensures cleanup.
///
/// # Example
///
/// ```ignore
/// use ee::testing::{with_lab_runtime, TEST_SEED};
///
/// with_lab_runtime(TEST_SEED, |runtime| {
///     // Test logic using the runtime
///     assert_eq!(runtime.steps(), 0);
/// });
/// ```
pub fn with_lab_runtime<F, R>(seed: u64, test_fn: F) -> R
where
    F: FnOnce(&mut LabRuntime) -> R,
{
    let mut runtime = lab_runtime(seed);
    test_fn(&mut runtime)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure_equal<T: std::fmt::Debug + PartialEq>(
        actual: &T,
        expected: &T,
        context: &str,
    ) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn test_seed_constant_is_stable() -> TestResult {
        ensure_equal(&TEST_SEED, &42, "TEST_SEED")
    }

    #[test]
    fn test_timestamp_is_valid_rfc3339() -> TestResult {
        chrono::DateTime::parse_from_rfc3339(TEST_TIMESTAMP)
            .map_err(|e| format!("TEST_TIMESTAMP is not valid RFC 3339: {e}"))?;
        Ok(())
    }

    #[test]
    fn test_workspace_id_has_correct_length() -> TestResult {
        ensure_equal(&TEST_WORKSPACE_ID.len(), &30, "workspace ID length")
    }

    #[test]
    fn test_memory_id_has_correct_length() -> TestResult {
        ensure_equal(&TEST_MEMORY_ID.len(), &30, "memory ID length")
    }

    #[test]
    fn test_audit_id_has_correct_length() -> TestResult {
        ensure_equal(&TEST_AUDIT_ID.len(), &32, "audit ID length")
    }

    #[test]
    fn lab_runtime_is_deterministic() -> TestResult {
        let first = lab_runtime(TEST_SEED);
        let second = lab_runtime(TEST_SEED);

        ensure_equal(&first.now(), &second.now(), "lab runtime start time")?;
        ensure_equal(&first.steps(), &second.steps(), "lab runtime step count")
    }

    #[test]
    fn default_lab_runtime_uses_test_seed() -> TestResult {
        let default = default_lab_runtime();
        let explicit = lab_runtime(TEST_SEED);

        ensure_equal(&default.now(), &explicit.now(), "default runtime matches explicit")
    }

    #[test]
    fn different_seeds_are_accepted() -> TestResult {
        // Different seeds produce independent runtimes. The virtual start time
        // may be identical (both start at time 0), but the random state differs.
        // This test verifies that we can create runtimes with different seeds.
        let _first = lab_runtime(TEST_SEED);
        let _second = lab_runtime(TEST_SEED_ALT);
        Ok(())
    }

    #[test]
    fn with_lab_runtime_provides_mutable_access() {
        let initial_steps = with_lab_runtime(TEST_SEED, |runtime| runtime.steps());
        assert_eq!(initial_steps, 0, "fresh runtime has zero steps");
    }

    #[test]
    fn assert_deterministic_runtimes_passes_for_same_seed() {
        assert_deterministic_runtimes(TEST_SEED);
        assert_deterministic_runtimes(TEST_SEED_ALT);
    }
}
