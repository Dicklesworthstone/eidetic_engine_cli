//! Per-kind contract tests for verifiable memory sentinels (bd-1n0np.16.5)
//! over the landed model in `ee::models::memory_sentinel`.
//!
//! The in-module tests already cover the PathExists happy path, the
//! command-help shell-rejection, malformed-spec repair hints, result-hash
//! stability, and the conservatism mapping. These lock the remaining per-kind
//! contract surface across ALL eight kinds:
//! - every kind round-trips through `parse` / `as_str` (and `FromStr`);
//! - the safety-class taxonomy — exactly one kind escalates to allowlisted
//!   introspection, every other kind is a pure predicate (the structural
//!   no-arbitrary-shell guard);
//! - every kind exposes a non-empty default predicate;
//! - safety-class and result-status tokens round-trip through `parse`.

use ee::models::memory_sentinel::{
    MemorySentinelKind, MemorySentinelResultStatus, MemorySentinelSafetyClass,
};

#[test]
fn every_kind_round_trips_through_parse_and_as_str() {
    let kinds = MemorySentinelKind::all();
    assert_eq!(kinds.len(), 8, "the v1 kind set is eight kinds");
    for kind in kinds {
        let token = kind.as_str();
        assert_eq!(
            MemorySentinelKind::parse(token),
            Some(kind),
            "as_str/parse round trip for {token}"
        );
        assert_eq!(
            token.parse::<MemorySentinelKind>().ok(),
            Some(kind),
            "FromStr parity for {token}"
        );
    }
    assert!(
        MemorySentinelKind::parse("definitely_not_a_sentinel_kind").is_none(),
        "unknown tokens do not parse"
    );
}

#[test]
fn only_command_help_is_allowlisted_introspection_the_rest_are_pure_predicates() {
    // Structural no-arbitrary-shell guard at the model level: exactly one kind
    // escalates to allowlisted introspection; every other kind is a pure
    // filesystem/config/schema/env/fixture predicate.
    for kind in MemorySentinelKind::all() {
        let expected = if matches!(kind, MemorySentinelKind::CommandHelpContainsFlag) {
            MemorySentinelSafetyClass::AllowlistedIntrospection
        } else {
            MemorySentinelSafetyClass::PurePredicate
        };
        assert_eq!(
            kind.safety_class(),
            expected,
            "safety class for {}",
            kind.as_str()
        );
    }
    let allowlisted = MemorySentinelKind::all()
        .into_iter()
        .filter(|kind| kind.safety_class() == MemorySentinelSafetyClass::AllowlistedIntrospection)
        .count();
    assert_eq!(
        allowlisted, 1,
        "exactly one kind may be allowlisted introspection"
    );
}

#[test]
fn every_kind_has_a_nonempty_default_predicate() {
    for kind in MemorySentinelKind::all() {
        assert!(
            !kind.default_predicate().trim().is_empty(),
            "default predicate for {} must be non-empty",
            kind.as_str()
        );
    }
}

#[test]
fn safety_class_and_status_tokens_round_trip_through_parse() {
    for class in [
        MemorySentinelSafetyClass::PurePredicate,
        MemorySentinelSafetyClass::AllowlistedIntrospection,
    ] {
        assert_eq!(
            MemorySentinelSafetyClass::parse(class.as_str()),
            Some(class)
        );
    }
    assert!(MemorySentinelSafetyClass::parse("arbitrary_shell").is_none());

    for status in [
        MemorySentinelResultStatus::Pass,
        MemorySentinelResultStatus::Fail,
        MemorySentinelResultStatus::Unknown,
    ] {
        assert_eq!(
            MemorySentinelResultStatus::parse(status.as_str()),
            Some(status)
        );
    }
    assert!(MemorySentinelResultStatus::parse("maybe").is_none());
}
