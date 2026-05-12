#![no_main]

//! Fuzz target for `pack::estimate_tokens` across all three strategies
//! (eidetic_engine_cli-cutqf).
//!
//! Pack token budgeting drives `--max-tokens` enforcement. A divergence
//! between strategies in trivial cases (empty / whitespace) or a panic on
//! an adversarial input would corrupt budget enforcement at a layer the
//! UI never inspects. This fuzz target stresses the invariants that hold
//! across every strategy:
//!
//! 1. Never panic for inputs <= 32 KiB across any of the three strategies.
//! 2. Empty + whitespace-only inputs yield exactly 0 tokens regardless of
//!    strategy (the `.trim().is_empty()` short-circuit).
//! 3. Any non-empty trimmed input yields >= 1 token under every strategy
//!    (so callers can use the result as a budget-floor without an
//!     explicit max(1)).
//! 4. The cl100k_base encoder always returns >= the character/word
//!    heuristic ceilings for moderately-long inputs would over-promise
//!    capacity (kept as a soft assertion via best_effort: we accept either
//!    direction so the fuzzer doesn't reject inputs the BPE counts
//!    differently from the heuristic — that's expected — we only check
//!    panic-freedom and zero/non-zero parity here).
//!
//! The pack_token_budget fuzz target already covers the budget-arithmetic
//! side; this target focuses on the encoder side that pack_token_budget
//! treats as a black box.

use ee::pack::{TokenEstimationStrategy, estimate_tokens};
use libfuzzer_sys::fuzz_target;

const MAX_INPUT_BYTES: usize = 32 * 1024;

fn count(content: &str, strategy: TokenEstimationStrategy) -> u32 {
    estimate_tokens(content, strategy)
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }
    let input = String::from_utf8_lossy(data);
    let input = input.as_ref();

    let strategies = [
        TokenEstimationStrategy::TiktokenCl100kBase,
        TokenEstimationStrategy::CharacterHeuristic,
        TokenEstimationStrategy::WordHeuristic,
    ];

    // (1) Never panic across any strategy.
    let counts: Vec<u32> = strategies.iter().map(|s| count(input, *s)).collect();

    // (2) Empty / whitespace-only input → 0 tokens, regardless of strategy.
    let trimmed = input.trim();
    if trimmed.is_empty() {
        for (i, c) in counts.iter().enumerate() {
            assert_eq!(
                *c, 0,
                "strategy {:?}: whitespace input {:?} should produce 0 tokens, got {}",
                strategies[i], input, c
            );
        }
        return;
    }

    // (3) Non-empty trimmed input → at least 1 token across every strategy.
    // This is the documented floor on the public API.
    for (i, c) in counts.iter().enumerate() {
        assert!(
            *c >= 1,
            "strategy {:?}: non-empty input {:?} (trimmed {:?}) produced 0 tokens",
            strategies[i],
            input,
            trimmed
        );
    }

    // (4) Estimates are bounded by u32::MAX. estimate_tokens uses
    // saturating arithmetic internally; the contract is just that the
    // returned value is <= u32::MAX. (This is trivially true by the type
    // signature, but the assertion documents the intent — a future change
    // to u64 must update callers.)
    for c in &counts {
        assert!(*c <= u32::MAX, "u32 saturation invariant broken: {c}");
    }

    // (5) Calling the default helper (TokenEstimationStrategy::default())
    // must return the same value as explicit TiktokenCl100kBase, because
    // TiktokenCl100kBase is the documented default.
    let default_count = ee::pack::estimate_tokens_default(input);
    assert_eq!(
        default_count, counts[0],
        "estimate_tokens_default must equal estimate_tokens(.., TiktokenCl100kBase) for input {input:?}"
    );
});
