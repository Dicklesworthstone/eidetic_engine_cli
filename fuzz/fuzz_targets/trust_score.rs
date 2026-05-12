#![no_main]

//! Fuzz target for FeedbackCounts trust calculations (eidetic_engine_cli-kewue).
//!
//! FeedbackCounts feeds three predicates that drive memory ranking and
//! redaction posture:
//!
//! - `trust_score(&self) -> f32` — must always return a finite value in
//!   the closed interval [0.0, 1.0].
//! - `is_unreliable(&self) -> bool` — boolean, never panics.
//! - `supports_validation(&self) -> bool` — boolean, never panics.
//!
//! A NaN or out-of-range trust score corrupts memory ranking silently.
//! This target asserts the documented invariants under adversarial
//! inputs derived from the fuzz byte stream: huge positive/negative
//! counts, sub-normal weights, NaN/Inf attempts (skipped — public API
//! takes f32 directly, so we control the input domain), and pathological
//! count vs. weight mismatches.

use ee::db::FeedbackCounts;
use libfuzzer_sys::fuzz_target;

const MIN_BYTES: usize = 28;

fn pick_u32(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}

fn pick_f32(data: &[u8], off: usize) -> f32 {
    let bits = u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
    f32::from_bits(bits)
}

fuzz_target!(|data: &[u8]| {
    if data.len() < MIN_BYTES {
        return;
    }

    // Drive four counts + four weights from the byte stream. Counts are
    // u32 (can be 0..=u32::MAX); weights are f32 (can be any bit pattern,
    // including NaN/Inf/subnormals — that's deliberate, the public API
    // accepts arbitrary f32 and must remain robust).
    let mut counts = FeedbackCounts::default();
    counts.positive_count = pick_u32(data, 0);
    counts.negative_count = pick_u32(data, 4);
    counts.neutral_count = pick_u32(data, 8);
    counts.decay_count = pick_u32(data, 12);
    counts.positive_weight = pick_f32(data, 16);
    counts.negative_weight = pick_f32(data, 20);
    counts.neutral_weight = pick_f32(data, 24);
    // Cap decay_weight to a sane f32 derived from the same bytes; we've
    // exhausted the 28-byte preamble.
    counts.decay_weight = if data.len() >= 32 {
        pick_f32(data, 28)
    } else {
        0.0
    };

    // (1) Never panic.
    let score = counts.trust_score();
    let unreliable = counts.is_unreliable();
    let supports = counts.supports_validation();

    // (2) Trust score is finite and within [0, 1] regardless of weight
    // bit pattern. This is the documented contract: the function
    // .clamp(0.0, 1.0)s its result.
    assert!(
        score.is_finite(),
        "trust_score returned non-finite: {score} for {counts:?}"
    );
    assert!(
        (0.0..=1.0).contains(&score),
        "trust_score returned {score}, expected [0.0, 1.0] for {counts:?}"
    );

    // (3) Empty counts yield the documented neutral 0.5.
    if counts.total_count() == 0 {
        assert_eq!(
            score, 0.5,
            "trust_score must be 0.5 for empty counts, got {score}"
        );
    }

    // (4) Predicates are bools; nothing to assert other than they ran
    // without panicking — and they did, by virtue of having reached
    // this line. Burn the bools so the compiler can't optimize the
    // calls out under release-mode fuzzing.
    std::hint::black_box((unreliable, supports));
});
