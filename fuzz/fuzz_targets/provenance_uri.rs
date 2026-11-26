#![no_main]

//! Fuzz target for `ProvenanceUri::from_str` (eidetic_engine_cli-3seem).
//!
//! Stresses the URI parser against the accepted provenance schemes. Asserts:
//!
//! 1. The parser never panics on any byte sequence ≤ 8 KiB.
//! 2. For every input the parser accepts, `Display(parsed) -> reparsed ==
//!    parsed` — the round-trip is byte-stable so pack provenance hashes
//!    are deterministic across the persist → load cycle.
//! 3. Whitespace-trimmed parses agree with the original. Today the parser
//!    trims; this invariant pins that behavior so a future change to drop
//!    trimming won't silently regress.
//! 4. `scheme()` names the scheme the rendered URI starts with.
//!
//! No assumptions are made about which inputs succeed — the fuzzer drives
//! that. The goal is panic-freedom and round-trip stability on the
//! accept branch, not maximizing accept-rate.

use std::str::FromStr;

use ee::models::ProvenanceUri;
use libfuzzer_sys::fuzz_target;

const MAX_INPUT_BYTES: usize = 8 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }
    let input = String::from_utf8_lossy(data);
    let input = input.as_ref();

    // (1) Never panic. Errors are fine; panics are not.
    let parsed = ProvenanceUri::from_str(input);

    if let Ok(uri) = parsed {
        // (2) Round-trip stability: Display ↔ FromStr must be a function on
        // the accepted subset of inputs.
        let rendered = uri.to_string();
        let reparsed = ProvenanceUri::from_str(&rendered)
            .expect("Display output must always round-trip through FromStr");
        assert_eq!(
            uri, reparsed,
            "round-trip mismatch: input {input:?} -> {rendered:?} -> {reparsed:?}"
        );

        // (3) Trim invariant: parsing the trimmed input must yield the same
        // result as parsing the raw input. The parser advertises trimming
        // semantics so pack provenance keys are insensitive to leading or
        // trailing whitespace from carelessly-formatted source records.
        let trimmed = input.trim();
        if trimmed != input {
            let trimmed_parse =
                ProvenanceUri::from_str(trimmed).expect("trimming a valid URI should still parse");
            assert_eq!(
                uri, trimmed_parse,
                "trim invariant: input {input:?} != trimmed {trimmed:?}"
            );
        }

        // (4) The scheme accessor names the scheme the URI renders with. With
        // (2), which reparses `rendered`, that makes it a scheme the parser
        // accepts. Comparing against the parser's own output, not a copied
        // list, keeps this from drifting: the hard-coded list of 11 schemes
        // this replaced omitted 24 of the 29 registered external schemes
        // (`journal`, `jsonl-import`, `sec-filing`, ...), so any input using
        // one of them would have been reported as a crash.
        let scheme = uri.scheme();
        assert!(
            !scheme.is_empty()
                && rendered
                    .strip_prefix(scheme)
                    .is_some_and(|rest| rest.starts_with("://")),
            "scheme `{scheme}` does not prefix the rendered URI `{rendered}`"
        );
    }
});
