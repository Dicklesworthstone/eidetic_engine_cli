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

use std::cmp::Ordering;

use asupersync::lab::{LabConfig, LabRuntime};
use regex_lite::Regex;
use serde_json::Value;

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

/// Validate a JSON value against the bounded Draft 2020-12 keyword subset used
/// by ee's checked-in public schemas.
///
/// This intentionally rejects unsupported external references and supports the
/// scalar, object, collection, composition, local-reference, and RFC 3339
/// constraints exercised by the repository's contract schemas.
pub fn validate_json_schema_instance(value: &Value, schema: &Value) -> TestResult {
    validate_json_schema_value(value, schema, schema, "$")
}

fn validate_json_schema_value(
    value: &Value,
    schema: &Value,
    root_schema: &Value,
    path: &str,
) -> TestResult {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let pointer = reference
            .strip_prefix('#')
            .ok_or_else(|| format!("unsupported non-local $ref {reference}"))?;
        let target = root_schema
            .pointer(pointer)
            .ok_or_else(|| format!("unresolved $ref {reference}"))?;
        return validate_json_schema_value(value, target, root_schema, path);
    }

    if let Some(options) = schema.get("oneOf").and_then(Value::as_array) {
        let matches = options
            .iter()
            .filter(|candidate| {
                validate_json_schema_value(value, candidate, root_schema, path).is_ok()
            })
            .count();
        if matches != 1 {
            return Err(format!(
                "{path} matched {matches} oneOf branches instead of exactly one"
            ));
        }
    }
    if let Some(options) = schema.get("anyOf").and_then(Value::as_array)
        && !options.iter().any(|candidate| {
            validate_json_schema_value(value, candidate, root_schema, path).is_ok()
        })
    {
        return Err(format!("{path} did not match any anyOf branch"));
    }
    if let Some(all_of) = schema.get("allOf").and_then(Value::as_array) {
        for candidate in all_of {
            validate_json_schema_value(value, candidate, root_schema, path)?;
        }
    }

    if let Some(expected) = schema.get("const")
        && value != expected
    {
        return Err(format!("{path} expected const {expected}, got {value}"));
    }
    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array)
        && !enum_values.iter().any(|candidate| candidate == value)
    {
        return Err(format!(
            "{path} value {value} is not in enum {enum_values:?}"
        ));
    }

    if let Some(expected_types) = json_schema_types(schema)
        && !expected_types
            .iter()
            .any(|expected_type| json_schema_type_matches(value, expected_type))
    {
        return Err(format!(
            "{path} expected type {expected_types:?}, got {}",
            json_schema_type_name(value)
        ));
    }

    if let Some(string) = value.as_str() {
        let length = string.chars().count();
        if let Some(min_length) = schema.get("minLength").and_then(Value::as_u64)
            && length < min_length as usize
        {
            return Err(format!("{path} has fewer than {min_length} characters"));
        }
        if let Some(max_length) = schema.get("maxLength").and_then(Value::as_u64)
            && length > max_length as usize
        {
            return Err(format!("{path} has more than {max_length} characters"));
        }
        if let Some(pattern) = schema.get("pattern").and_then(Value::as_str) {
            let regex = Regex::new(pattern).map_err(|error| {
                format!("{path} schema has invalid pattern {pattern:?}: {error}")
            })?;
            if !regex.is_match(string) {
                return Err(format!(
                    "{path} value {string:?} does not match {pattern:?}"
                ));
            }
        }
        if schema.get("format").and_then(Value::as_str) == Some("date-time")
            && chrono::DateTime::parse_from_rfc3339(string).is_err()
        {
            return Err(format!(
                "{path} value {string:?} is not an RFC 3339 date-time"
            ));
        }
    }

    if let Some(number) = value.as_number() {
        if let Some(minimum) = schema.get("minimum").and_then(Value::as_number)
            && compare_json_numbers(number, minimum) == Some(Ordering::Less)
        {
            return Err(format!("{path} value {number} is below minimum {minimum}"));
        }
        if let Some(maximum) = schema.get("maximum").and_then(Value::as_number)
            && compare_json_numbers(number, maximum) == Some(Ordering::Greater)
        {
            return Err(format!("{path} value {number} is above maximum {maximum}"));
        }
        if let Some(minimum) = schema.get("exclusiveMinimum").and_then(Value::as_number)
            && compare_json_numbers(number, minimum).is_some_and(Ordering::is_le)
        {
            return Err(format!(
                "{path} value {number} is not above exclusive minimum {minimum}"
            ));
        }
        if let Some(maximum) = schema.get("exclusiveMaximum").and_then(Value::as_number)
            && compare_json_numbers(number, maximum).is_some_and(Ordering::is_ge)
        {
            return Err(format!(
                "{path} value {number} is not below exclusive maximum {maximum}"
            ));
        }
    }

    if let Some(object) = value.as_object() {
        if let Some(min_properties) = schema.get("minProperties").and_then(Value::as_u64)
            && object.len() < min_properties as usize
        {
            return Err(format!("{path} has fewer than {min_properties} properties"));
        }
        if let Some(max_properties) = schema.get("maxProperties").and_then(Value::as_u64)
            && object.len() > max_properties as usize
        {
            return Err(format!("{path} has more than {max_properties} properties"));
        }
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for field in required {
                let field = field
                    .as_str()
                    .ok_or_else(|| format!("{path} schema required entry is not a string"))?;
                if !object.contains_key(field) {
                    return Err(format!("{path} missing required field {field}"));
                }
            }
        }

        let properties = schema.get("properties").and_then(Value::as_object);
        for (key, child) in object {
            let child_path = format!("{path}.{key}");
            if let Some(property_schema) = properties.and_then(|entries| entries.get(key)) {
                validate_json_schema_value(child, property_schema, root_schema, &child_path)?;
                continue;
            }
            match schema.get("additionalProperties") {
                Some(Value::Bool(false)) => {
                    return Err(format!("{path} contains unexpected field {key}"));
                }
                Some(Value::Object(property_schema)) => validate_json_schema_value(
                    child,
                    &Value::Object(property_schema.clone()),
                    root_schema,
                    &child_path,
                )?,
                Some(Value::Bool(true)) | None => {}
                Some(other) => {
                    return Err(format!(
                        "{path} has unsupported additionalProperties schema {other}"
                    ));
                }
            }
        }
    }

    if let Some(array) = value.as_array() {
        if let Some(min_items) = schema.get("minItems").and_then(Value::as_u64)
            && array.len() < min_items as usize
        {
            return Err(format!("{path} has fewer than {min_items} items"));
        }
        if let Some(max_items) = schema.get("maxItems").and_then(Value::as_u64)
            && array.len() > max_items as usize
        {
            return Err(format!("{path} has more than {max_items} items"));
        }
        if schema.get("uniqueItems").and_then(Value::as_bool) == Some(true) {
            for (index, item) in array.iter().enumerate() {
                if array[..index].iter().any(|existing| existing == item) {
                    return Err(format!("{path}[{index}] duplicates an earlier item"));
                }
            }
        }
        if let Some(prefix_items) = schema.get("prefixItems").and_then(Value::as_array) {
            for (index, item_schema) in prefix_items.iter().enumerate() {
                if let Some(item) = array.get(index) {
                    validate_json_schema_value(
                        item,
                        item_schema,
                        root_schema,
                        &format!("{path}[{index}]"),
                    )?;
                }
            }
        }
        if let Some(item_schema) = schema.get("items") {
            for (index, item) in array.iter().enumerate() {
                validate_json_schema_value(
                    item,
                    item_schema,
                    root_schema,
                    &format!("{path}[{index}]"),
                )?;
            }
        }
    }

    Ok(())
}

fn json_schema_types(schema: &Value) -> Option<Vec<&str>> {
    match schema.get("type")? {
        Value::String(single) => Some(vec![single.as_str()]),
        Value::Array(values) => Some(values.iter().filter_map(Value::as_str).collect()),
        _ => None,
    }
}

fn json_schema_type_matches(value: &Value, expected: &str) -> bool {
    match expected {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "number" => value.is_number(),
        "integer" => value.as_number().is_some_and(json_schema_number_is_integer),
        "string" => value.is_string(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

fn json_schema_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) if json_schema_number_is_integer(number) => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[derive(Debug)]
struct NormalizedJsonDecimal {
    negative: bool,
    digits: Vec<u8>,
    exponent: i64,
}

fn parse_json_decimal_exponent(raw: &str) -> Option<i64> {
    let (negative, digits) = raw
        .strip_prefix('-')
        .map_or((false, raw), |digits| (true, digits));
    let digits = digits.strip_prefix('+').unwrap_or(digits);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let magnitude = digits.bytes().fold(0_i64, |value, byte| {
        value
            .saturating_mul(10)
            .saturating_add(i64::from(byte - b'0'))
    });
    Some(if negative {
        magnitude.saturating_neg()
    } else {
        magnitude
    })
}

// Compare the decimal rendering exactly so a u64::MAX schema boundary is not
// rounded to 2^64 through f64 before applying minimum/maximum keywords.
fn normalize_json_number(number: &serde_json::Number) -> Option<NormalizedJsonDecimal> {
    let rendered = number.to_string();
    let (mantissa, explicit_exponent) =
        rendered
            .find(['e', 'E'])
            .map_or(Some((rendered.as_str(), 0_i64)), |index| {
                parse_json_decimal_exponent(&rendered[index + 1..])
                    .map(|exponent| (&rendered[..index], exponent))
            })?;
    if mantissa.is_empty() {
        return None;
    }
    let (negative, mantissa) = mantissa
        .strip_prefix('-')
        .map_or((false, mantissa), |unsigned| (true, unsigned));
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let mut digits = whole
        .bytes()
        .chain(fraction.bytes())
        .skip_while(|byte| *byte == b'0')
        .collect::<Vec<_>>();
    if digits.is_empty() {
        return Some(NormalizedJsonDecimal {
            negative: false,
            digits: vec![b'0'],
            exponent: 0,
        });
    }
    let fraction_len = i64::try_from(fraction.len()).ok()?;
    let mut exponent = explicit_exponent.checked_sub(fraction_len)?;
    while digits.last() == Some(&b'0') {
        digits.pop();
        exponent = exponent.checked_add(1)?;
    }
    Some(NormalizedJsonDecimal {
        negative,
        digits,
        exponent,
    })
}

fn json_schema_number_is_integer(number: &serde_json::Number) -> bool {
    normalize_json_number(number).is_some_and(|number| number.exponent >= 0)
}

fn compare_json_numbers(left: &serde_json::Number, right: &serde_json::Number) -> Option<Ordering> {
    let left = normalize_json_number(left)?;
    let right = normalize_json_number(right)?;
    let left_is_zero = left.digits == [b'0'];
    let right_is_zero = right.digits == [b'0'];
    if left_is_zero || right_is_zero {
        return Some(match (left_is_zero, right_is_zero) {
            (true, true) => Ordering::Equal,
            (true, false) if right.negative => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, true) if left.negative => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => unreachable!("zero branch requires at least one zero"),
        });
    }
    if left.negative != right.negative {
        return Some(if left.negative {
            Ordering::Less
        } else {
            Ordering::Greater
        });
    }
    let left_magnitude = i64::try_from(left.digits.len())
        .ok()?
        .checked_add(left.exponent)?;
    let right_magnitude = i64::try_from(right.digits.len())
        .ok()?
        .checked_add(right.exponent)?;
    let magnitude_order = left_magnitude.cmp(&right_magnitude);
    let absolute_order = if magnitude_order == Ordering::Equal {
        let width = left.digits.len().max(right.digits.len());
        (0..width)
            .map(|index| {
                left.digits
                    .get(index)
                    .copied()
                    .unwrap_or(b'0')
                    .cmp(&right.digits.get(index).copied().unwrap_or(b'0'))
            })
            .find(|ordering| *ordering != Ordering::Equal)
            .unwrap_or(Ordering::Equal)
    } else {
        magnitude_order
    };
    Some(if left.negative {
        absolute_order.reverse()
    } else {
        absolute_order
    })
}

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

// ============================================================================
// Schema-valid public id builders (frankensqlite 0.1.12 CHECK enforcement)
// ============================================================================

/// Build a deterministic, schema-valid public id of exactly `len` chars.
///
/// The database schema in `src/db/mod.rs` enforces
/// `CHECK (id GLOB '<prefix>_*' AND length(id) = <len>)` on every typed-id
/// column. frankensqlite 0.1.12 enforces these CHECKs that 0.1.9 silently
/// ignored, so test fixtures must use ids of the right shape. Production ids
/// are `<prefix>_<crockford-base32>` (see `src/models/id.rs`); the CHECK only
/// constrains the prefix glob and the total length, so this builder emits
/// readable, deterministic fixtures that satisfy the same invariant without
/// minting real UUIDv7 payloads.
///
/// The body is derived from `seed`: its alphanumeric characters lowercased
/// and truncated to fit, then right-padded with `'0'` to the exact length.
/// The same `(prefix, len, seed)` always yields the same id, so
/// cross-references inside a single test stay stable.
///
/// # Panics
///
/// Panics if `len` is too small to hold `"<prefix>_"` plus at least one body
/// character — a programming error in the caller, never reachable from a
/// well-formed prefix/length pair taken from the schema.
#[must_use]
pub fn valid_id(prefix: &str, len: usize, seed: &str) -> String {
    let head_len = prefix.len() + 1; // "<prefix>_"
    assert!(
        len > head_len,
        "id length {len} is too short for prefix `{prefix}_`"
    );
    let body_len = len - head_len;
    let mut body: String = seed
        .chars()
        .filter_map(|character| {
            let lower = character.to_ascii_lowercase();
            lower.is_ascii_alphanumeric().then_some(lower)
        })
        .take(body_len)
        .collect();
    while body.len() < body_len {
        body.push('0');
    }
    format!("{prefix}_{body}")
}

/// Deterministic, schema-valid workspace id (`wsp_…`, 30 chars).
#[must_use]
pub fn wsp(seed: &str) -> String {
    valid_id("wsp", 30, seed)
}

/// Deterministic, schema-valid agent id (`agt_…`, 30 chars).
#[must_use]
pub fn agt(seed: &str) -> String {
    valid_id("agt", 30, seed)
}

/// Deterministic, schema-valid memory id (`mem_…`, 30 chars).
#[must_use]
pub fn mem(seed: &str) -> String {
    valid_id("mem", 30, seed)
}

/// Deterministic, schema-valid import-ledger id (`imp_…`, 30 chars).
#[must_use]
pub fn imp(seed: &str) -> String {
    valid_id("imp", 30, seed)
}

/// Deterministic, schema-valid episode id (`ep_…`, 30 chars).
#[must_use]
pub fn ep(seed: &str) -> String {
    valid_id("ep", 30, seed)
}

/// Deterministic, schema-valid model-registry id (`mdl_…`, 30 chars).
#[must_use]
pub fn mdl(seed: &str) -> String {
    valid_id("mdl", 30, seed)
}

/// Deterministic, schema-valid agent-installation id (`agi_…`, 30 chars).
#[must_use]
pub fn agi(seed: &str) -> String {
    valid_id("agi", 30, seed)
}

/// Deterministic, schema-valid agent-history-source id (`ahs_…`, 30 chars).
#[must_use]
pub fn ahs(seed: &str) -> String {
    valid_id("ahs", 30, seed)
}

/// Deterministic, schema-valid context-pack id (`pack_…`, 31 chars).
#[must_use]
pub fn pack(seed: &str) -> String {
    valid_id("pack", 31, seed)
}

/// Deterministic, schema-valid session id (`sess_…`, 31 chars).
#[must_use]
pub fn sess(seed: &str) -> String {
    valid_id("sess", 31, seed)
}

/// Deterministic, schema-valid procedural-rule id (`rule_…`, 31 chars).
#[must_use]
pub fn rule(seed: &str) -> String {
    valid_id("rule", 31, seed)
}

/// Deterministic, schema-valid memory-link id (`link_…`, 31 chars).
#[must_use]
pub fn link(seed: &str) -> String {
    valid_id("link", 31, seed)
}

/// Deterministic, schema-valid evidence-span id (`ev_…`, 29 chars).
#[must_use]
pub fn ev(seed: &str) -> String {
    valid_id("ev", 29, seed)
}

/// Deterministic, schema-valid audit-log id (`audit_…`, 32 chars).
#[must_use]
pub fn audit(seed: &str) -> String {
    valid_id("audit", 32, seed)
}

/// Deterministic, schema-valid curation-candidate id (`curate_…`, 33 chars).
#[must_use]
pub fn curate(seed: &str) -> String {
    valid_id("curate", 33, seed)
}

/// Deterministic, schema-valid artifact id (`art_…`, 30 chars).
///
/// The artifact CHECK is stricter (`GLOB 'art_[0-9a-f]*'`), so the body must
/// be lowercase hex; we hash `seed` with blake3 (as production does in
/// `src/core/artifact.rs`) and take the first 26 hex digits.
#[must_use]
pub fn art(seed: &str) -> String {
    let digest = blake3::hash(seed.as_bytes()).to_hex();
    format!("art_{}", &digest.as_str()[..26])
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

    #[test]
    fn draft_2020_12_integer_accepts_integral_decimal_numbers() -> TestResult {
        let schema = serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "integer",
            "minimum": 0,
            "maximum": 18446744073709551615_u64
        });
        let maximum_decimal: Value = serde_json::from_str("18446744073709551615.0")
            .map_err(|error| format!("parse u64::MAX decimal fixture: {error}"))?;
        let maximum_exponent: Value = serde_json::from_str("184467440737095516150e-1")
            .map_err(|error| format!("parse u64::MAX exponent fixture: {error}"))?;
        for value in [
            serde_json::json!(0.0),
            serde_json::json!(1.0),
            serde_json::json!(1e3),
            serde_json::json!(u64::MAX),
            maximum_decimal,
            maximum_exponent,
        ] {
            validate_json_schema_instance(&value, &schema).map_err(|error| {
                format!("schema rejected integral JSON number {value}: {error}")
            })?;
        }
        Ok(())
    }

    #[test]
    fn draft_2020_12_uint64_schema_rejects_out_of_domain_numbers() -> TestResult {
        let schema = serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "integer",
            "minimum": 0,
            "maximum": 18446744073709551615_u64
        });
        let over_u64: Value = serde_json::from_str("18446744073709551616")
            .map_err(|error| format!("parse over-u64 fixture: {error}"))?;
        let over_u64_decimal: Value = serde_json::from_str("18446744073709551616.0")
            .map_err(|error| format!("parse over-u64 decimal fixture: {error}"))?;
        let near_boundary_fraction: Value = serde_json::from_str("18446744073709551614.5")
            .map_err(|error| format!("parse near-boundary fractional fixture: {error}"))?;
        for value in [
            serde_json::json!(-1),
            serde_json::json!(1.5),
            over_u64,
            over_u64_decimal,
            near_boundary_fraction,
        ] {
            if validate_json_schema_instance(&value, &schema).is_ok() {
                return Err(format!(
                    "uint64 schema accepted out-of-domain JSON number {value}"
                ));
            }
        }
        Ok(())
    }

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
