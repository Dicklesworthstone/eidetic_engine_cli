//! bd-2w2x7 part 2 — adversarial fuzz target for the preflight
//! command parser.
//!
//! Part 1 (`policy_redaction.rs`, shipped in 4d1d27c1) fuzzes the
//! seven-pass secret redactor. Part 2 covers the OTHER half of the
//! bead: stress-fuzzing the preflight command parser that backs
//! every `match_command` decision in `src/core/preflight_guard.rs`.
//! After the bd-21grg + bd-3gl2x preflight-bypass series, this
//! parser is the single chokepoint deciding whether a destructive
//! command surfaces a guard — env-options (`FOO=bar cmd ...`), sudo
//! wrappers (`sudo -E rm ...`), nested quoting / escaping, and
//! command substitution (`$(...)`, backtick) are the classic
//! discovery hotspots.
//!
//! Invariants checked on every fuzz input:
//!
//! 1. **No panic.** Any 0–64 KiB input (UTF-8 lossy) must not panic
//!    `PreflightGuardRegistry::match_command`. The parser walks
//!    arbitrary shell-shaped bytes and must stay total.
//! 2. **Determinism.** Two consecutive `match_command(input)` calls
//!    against the same registry must return the same set of rule
//!    IDs. Drift here would expose hidden state (cache, time-based
//!    behavior) or non-determinism in the shell-segment splitter.
//! 3. **Match-rule integrity.** Every returned `PreflightGuardRule`
//!    has a non-empty `id` and `pattern`. A rule with an empty id
//!    would silently bypass downstream rule-id-keyed routing.
//! 4. **Empty input has no matches.** `match_command("")` against the
//!    builtin registry must return an empty list. A regression that
//!    matched the empty string against a `*`-glob rule would force
//!    every empty-command harness call to surface a guard.
//! 5. **Whitespace-only input has no matches.** A command of pure
//!    whitespace, tabs, or newlines cannot be a real action. The
//!    parser must drop it before any rule-matching pass.
//!
//! Input cap: 64 KiB. Real shell commands are tiny; the cap matches
//! the policy-redaction fuzz target's cap so the two halves of
//! bd-2w2x7 share a corpus size regime.

#![no_main]

use ee::core::preflight_guard::{PreflightGuardRegistry, PreflightGuardRule};
use libfuzzer_sys::fuzz_target;

const MAX_INPUT_BYTES: usize = 64 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }
    // The parser takes `&str`. UTF-8-lossy converts invalid byte
    // sequences to U+FFFD, which is itself a corner case worth
    // exercising (the shell-segment splitter must not choke on it).
    let input = String::from_utf8_lossy(data);
    let input_ref: &str = input.as_ref();

    // Use the builtin registry rather than constructing rules
    // ourselves — that exercises the actual production rule set
    // (rm -rf, file_deletion, git_clean -fd, kubectl delete, etc.)
    // that the bd-21grg / bd-3gl2x series hardened.
    let registry = PreflightGuardRegistry::with_builtins();

    // Invariant 1: no panic.
    let first = registry.match_command(input_ref);
    let second = registry.match_command(input_ref);

    // Invariant 2: determinism (rule_id set equality).
    assert_rule_ids_equal(&first, &second, "consecutive match_command calls");

    // Invariant 3: every rule has a non-empty id + pattern.
    for rule in &first {
        assert_rule_well_formed(rule, input_ref);
    }

    // Invariants 4 + 5 are checked once per fuzz session via the
    // registry static below; doing so PER input would waste cycles
    // re-asserting the same invariant. The check still runs every
    // tick because each fuzz_target! body is a separate process
    // launch when libFuzzer restarts on a corpus shrink — keep the
    // assertions cheap and conditional.
    if input_ref.is_empty() || input_ref.bytes().all(is_whitespace_byte) {
        assert!(
            first.is_empty(),
            "match_command on empty/whitespace-only input returned {} rules: input={input_ref:?} first={:?}",
            first.len(),
            first.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        );
    }
});

fn assert_rule_ids_equal(a: &[&PreflightGuardRule], b: &[&PreflightGuardRule], label: &str) {
    if a.len() != b.len() {
        panic!(
            "{label}: rule-count mismatch: a={} b={} a_ids={:?} b_ids={:?}",
            a.len(),
            b.len(),
            a.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            b.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        );
    }
    for (left, right) in a.iter().zip(b.iter()) {
        assert_eq!(
            left.id, right.id,
            "{label}: rule id differs in non-determinism guard",
        );
    }
}

fn assert_rule_well_formed(rule: &PreflightGuardRule, input: &str) {
    assert!(
        !rule.id.is_empty(),
        "match_command returned a rule with an empty id (silent routing bypass risk): input={input:?}",
    );
    assert!(
        !rule.pattern.is_empty(),
        "match_command returned a rule with an empty pattern: id={} input={input:?}",
        rule.id,
    );
}

const fn is_whitespace_byte(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}
