//! Deterministic test helpers and conventions for ee (EE-013, EE-TST-002).
//!
//! This module provides test utilities, conventions, and builders for writing
//! consistent, deterministic unit tests across ee crates.
//!
//! # Test Conventions (EE-TST-002)
//!
//! ## Required Test Categories
//!
//! Every module should include inline `#[cfg(test)]` tests covering:
//! 1. **Happy path**: normal successful operation
//! 2. **Boundary inputs**: empty collections, zero values, max values
//! 3. **Invalid inputs**: malformed data, out-of-range values
//! 4. **Degraded state**: missing dependencies, stale indexes
//! 5. **Idempotency**: repeated calls produce consistent results
//! 6. **Mutation guards**: read-only operations don't mutate
//!
//! ## Prohibited Test Dependencies
//!
//! Tests must NOT depend on:
//! - Network access (use mock adapters)
//! - Wall-clock time (use TEST_TIMESTAMP or LabRuntime virtual time)
//! - Ambient user config (~/.ee, environment variables)
//! - Hidden global state (static mutable, process-wide singletons)
//! - Filesystem outside tempdir (use tempfile::tempdir)
//!
//! ## Standard Test Pattern
//!
//! ```ignore
//! #[cfg(test)]
//! mod tests {
//!     use super::*;
//!
//!     type TestResult = Result<(), String>;
//!
//!     fn ensure<T: std::fmt::Debug + PartialEq>(
//!         actual: T, expected: T, ctx: &str
//!     ) -> TestResult {
//!         if actual == expected { Ok(()) }
//!         else { Err(format!("{ctx}: expected {expected:?}, got {actual:?}")) }
//!     }
//!
//!     #[test]
//!     fn my_function_happy_path() -> TestResult {
//!         let result = my_function(valid_input());
//!         ensure(result.is_ok(), true, "succeeds with valid input")
//!     }
//!
//!     #[test]
//!     fn my_function_empty_input() -> TestResult {
//!         let result = my_function(&[]);
//!         ensure(result, expected_empty_result(), "handles empty input")
//!     }
//! }
//! ```
//!
//! # Features
//!
//! - Deterministic scheduling: same seed produces identical execution order
//! - Virtual time: no wall-clock dependencies in tests
//! - Fixed test fixtures: canonical timestamps, IDs, and seeds
//! - Assertion helpers: ensure, ensure_equal, ensure_contains, etc.
//! - Test builders: workspace, memory, capability fixtures
//!
//! # Usage
//!
//! ```ignore
//! use ee::testing::{lab_runtime, TEST_SEED, TEST_TIMESTAMP, TestResult};
//!
//! #[test]
//! fn async_test_is_deterministic() -> TestResult {
//!     let mut runtime = lab_runtime(TEST_SEED);
//!     // ... test async logic with deterministic scheduling ...
//!     Ok(())
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

/// Canonical pack ID for test fixtures (31 chars).
pub const TEST_PACK_ID: &str = "pack_test0000000000000000000000";

/// Canonical hash for test fixtures (64 hex chars).
pub const TEST_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Canonical degradation code for test fixtures.
pub const TEST_DEGRADATION_CODE: &str = "test_degraded";

// ============================================================================
// Test Result Type and Assertion Helpers (EE-TST-002)
// ============================================================================

/// Standard test result type for ee unit tests.
///
/// Using `Result<(), String>` allows tests to use `?` for early returns
/// and provides descriptive error messages on failure.
pub type TestResult = Result<(), String>;

/// Assert that two values are equal, with context on failure.
///
/// # Example
///
/// ```ignore
/// use ee::testing::{ensure_equal, TestResult};
///
/// fn test_something() -> TestResult {
///     ensure_equal(&actual, &expected, "values should match")
/// }
/// ```
pub fn ensure_equal<T: std::fmt::Debug + PartialEq>(
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

/// Assert that a condition is true, with context on failure.
///
/// # Example
///
/// ```ignore
/// use ee::testing::{ensure, TestResult};
///
/// fn test_something() -> TestResult {
///     ensure(value > 0, "value should be positive")
/// }
/// ```
pub fn ensure(condition: bool, context: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(context.into())
    }
}

/// Assert that a string contains a substring.
///
/// # Example
///
/// ```ignore
/// use ee::testing::{ensure_contains, TestResult};
///
/// fn test_error_message() -> TestResult {
///     ensure_contains(&error.to_string(), "not found", "error mentions not found")
/// }
/// ```
pub fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
    if haystack.contains(needle) {
        Ok(())
    } else {
        Err(format!(
            "{context}: expected {haystack:?} to contain {needle:?}"
        ))
    }
}

/// Assert that a value is at least a minimum.
///
/// # Example
///
/// ```ignore
/// use ee::testing::{ensure_at_least, TestResult};
///
/// fn test_count() -> TestResult {
///     ensure_at_least(items.len(), 3, "should have at least 3 items")
/// }
/// ```
pub fn ensure_at_least<T: std::fmt::Debug + PartialOrd>(
    actual: T,
    minimum: T,
    context: &str,
) -> TestResult {
    if actual >= minimum {
        Ok(())
    } else {
        Err(format!(
            "{context}: expected at least {minimum:?}, got {actual:?}"
        ))
    }
}

/// Assert that a Result is Ok.
pub fn ensure_ok<T, E: std::fmt::Debug>(result: &Result<T, E>, context: &str) -> TestResult {
    match result {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("{context}: expected Ok, got Err({e:?})")),
    }
}

/// Assert that a Result is Err.
pub fn ensure_err<T: std::fmt::Debug, E>(result: &Result<T, E>, context: &str) -> TestResult {
    match result {
        Ok(v) => Err(format!("{context}: expected Err, got Ok({v:?})")),
        Err(_) => Ok(()),
    }
}

/// Assert that an Option is Some.
pub fn ensure_some<T>(option: &Option<T>, context: &str) -> TestResult {
    match option {
        Some(_) => Ok(()),
        None => Err(format!("{context}: expected Some, got None")),
    }
}

/// Assert that an Option is None.
pub fn ensure_none<T: std::fmt::Debug>(option: &Option<T>, context: &str) -> TestResult {
    match option {
        None => Ok(()),
        Some(v) => Err(format!("{context}: expected None, got Some({v:?})")),
    }
}

// ============================================================================
// Test Builders (EE-TST-002)
// ============================================================================

/// Generate a test memory ID with a numeric suffix.
///
/// # Example
///
/// ```ignore
/// use ee::testing::test_memory_id;
///
/// let id = test_memory_id(1); // "mem_test0000000000000000000001"
/// ```
#[must_use]
pub fn test_memory_id(n: u32) -> String {
    format!("mem_test{n:022}") // 8 + 22 = 30 chars
}

/// Generate a test workspace ID with a numeric suffix.
#[must_use]
pub fn test_workspace_id(n: u32) -> String {
    format!("wsp_test{n:022}") // 8 + 22 = 30 chars
}

/// Generate a test pack ID with a numeric suffix.
#[must_use]
pub fn test_pack_id(n: u32) -> String {
    format!("pack_test{n:022}") // 9 + 22 = 31 chars
}

/// Generate a test audit ID with a numeric suffix.
#[must_use]
pub fn test_audit_id(n: u32) -> String {
    format!("audit_test{n:022}")
}

/// Generate a deterministic test hash from a seed.
#[must_use]
pub fn test_hash(seed: u64) -> String {
    format!("{seed:064x}")
}

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

    // ========================================================================
    // Fixture Constants Tests
    // ========================================================================

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
    fn test_pack_id_has_correct_length() -> TestResult {
        ensure_equal(&TEST_PACK_ID.len(), &31, "pack ID length")
    }

    #[test]
    fn test_hash_has_correct_length() -> TestResult {
        ensure_equal(&TEST_HASH.len(), &64, "hash length")
    }

    // ========================================================================
    // Lab Runtime Tests
    // ========================================================================

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

        ensure_equal(
            &default.now(),
            &explicit.now(),
            "default runtime matches explicit",
        )
    }

    #[test]
    fn different_seeds_are_accepted() -> TestResult {
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

    // ========================================================================
    // Assertion Helper Tests
    // ========================================================================

    #[test]
    fn ensure_equal_passes_for_equal_values() -> TestResult {
        ensure_equal(&42, &42, "integers")?;
        ensure_equal(&"hello", &"hello", "strings")
    }

    #[test]
    fn ensure_equal_fails_for_unequal_values() -> TestResult {
        let result = ensure_equal(&42, &43, "test");
        ensure(result.is_err(), "should fail for unequal values")
    }

    #[test]
    fn ensure_passes_for_true() -> TestResult {
        ensure(true, "condition is true")
    }

    #[test]
    fn ensure_fails_for_false() -> TestResult {
        let result = ensure(false, "test");
        ensure_equal(&result.is_err(), &true, "should fail for false")
    }

    #[test]
    fn ensure_contains_finds_substring() -> TestResult {
        ensure_contains("hello world", "world", "substring found")
    }

    #[test]
    fn ensure_contains_fails_for_missing_substring() -> TestResult {
        let result = ensure_contains("hello", "world", "test");
        ensure(result.is_err(), "should fail for missing substring")
    }

    #[test]
    fn ensure_at_least_passes_for_equal() -> TestResult {
        ensure_at_least(5, 5, "equal values")
    }

    #[test]
    fn ensure_at_least_passes_for_greater() -> TestResult {
        ensure_at_least(10, 5, "greater value")
    }

    #[test]
    fn ensure_at_least_fails_for_less() -> TestResult {
        let result = ensure_at_least(3, 5, "test");
        ensure(result.is_err(), "should fail for less than minimum")
    }

    #[test]
    fn ensure_ok_passes_for_ok() -> TestResult {
        let result: Result<i32, &str> = Ok(42);
        ensure_ok(&result, "should be Ok")
    }

    #[test]
    fn ensure_ok_fails_for_err() -> TestResult {
        let result: Result<i32, &str> = Err("error");
        let check = ensure_ok(&result, "test");
        ensure(check.is_err(), "should fail for Err")
    }

    #[test]
    fn ensure_err_passes_for_err() -> TestResult {
        let result: Result<i32, &str> = Err("error");
        ensure_err(&result, "should be Err")
    }

    #[test]
    fn ensure_some_passes_for_some() -> TestResult {
        ensure_some(&Some(42), "should be Some")
    }

    #[test]
    fn ensure_none_passes_for_none() -> TestResult {
        let none: Option<i32> = None;
        ensure_none(&none, "should be None")
    }

    // ========================================================================
    // Builder Tests
    // ========================================================================

    #[test]
    fn test_memory_id_generates_correct_format() -> TestResult {
        let id = test_memory_id(1);
        ensure_equal(&id.len(), &30, "memory ID length")?;
        ensure(id.starts_with("mem_test"), "starts with mem_test")
    }

    #[test]
    fn test_memory_id_increments_correctly() -> TestResult {
        let id1 = test_memory_id(1);
        let id2 = test_memory_id(2);
        ensure(id1 != id2, "different numbers produce different IDs")
    }

    #[test]
    fn test_workspace_id_generates_correct_format() -> TestResult {
        let id = test_workspace_id(1);
        ensure_equal(&id.len(), &30, "workspace ID length")?;
        ensure(id.starts_with("wsp_test"), "starts with wsp_test")
    }

    #[test]
    fn test_pack_id_generates_correct_format() -> TestResult {
        let id = test_pack_id(1);
        ensure_equal(&id.len(), &31, "pack ID length")?;
        ensure(id.starts_with("pack_test"), "starts with pack_test")
    }

    #[test]
    fn test_audit_id_generates_correct_format() -> TestResult {
        let id = test_audit_id(1);
        ensure_equal(&id.len(), &32, "audit ID length")?;
        ensure(id.starts_with("audit_test"), "starts with audit_test")
    }

    #[test]
    fn test_hash_generates_correct_length() -> TestResult {
        let hash = test_hash(12345);
        ensure_equal(&hash.len(), &64, "hash length")
    }

    #[test]
    fn test_hash_is_deterministic() -> TestResult {
        let hash1 = test_hash(42);
        let hash2 = test_hash(42);
        ensure_equal(&hash1, &hash2, "same seed produces same hash")
    }
}
