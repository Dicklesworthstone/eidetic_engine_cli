//! bd-2w2x7 — adversarial fuzz target for `policy::redact_secret_like_content`.
//!
//! The redactor at `src/policy/mod.rs:1307` chains seven sub-redactors
//! (key-values, URL passwords, PEM blocks, raw API tokens, JWT tokens,
//! high-entropy values, PII). Each sub-redactor walks the input
//! independently with its own pattern matchers; corner cases at the
//! boundaries between sub-redactors (UTF-8 boundaries, overlapping
//! matches, NUL bytes, unicode normalization) are the classic
//! fuzz-discovery hotspots called out in the bead.
//!
//! Invariants checked on every fuzz input:
//!
//! 1. **No panic.** Any input — including pathological UTF-8, embedded
//!    NULs, surrogate-pair boundaries, control characters, mixed
//!    high-entropy noise — must not panic `redact_secret_like_content`.
//! 2. **Determinism.** Calling the redactor twice on the same input
//!    must produce byte-identical reports. Drift here would expose a
//!    non-deterministic ordering bug in one of the sub-redactors.
//! 3. **Match-span bounds.** Each detected `SecretRedactionMatch`
//!    must satisfy `start <= end <= input.len()`. A span beyond the
//!    input length is a UB / wrong-byte-offset bug.
//! 4. **Match-span sort order.** Detected matches should be in
//!    non-decreasing start order; the redactor depends on this for
//!    deterministic span aggregation. A regression that returns
//!    unsorted matches would silently break downstream consumers.
//! 5. **Flag/reasons consistency.** When `redacted == true`, the
//!    `redacted_reasons` list must be non-empty (the redactor flagged
//!    a change, so SOMETHING must have justified it). Conversely
//!    when `redacted == false`, `redacted_reasons` must be empty.
//! 6. **Reasons sorted + deduped.** The redactor explicitly sorts
//!    and dedups reasons after the seven sub-redactor passes. A
//!    regression that drops the sort/dedup leaks call ordering into
//!    the report and breaks determinism for downstream signature
//!    comparisons.
//! 7. **Re-redaction is idempotent on flag.** Feeding the redacted
//!    output back through the redactor must not flip `redacted`
//!    from false to true — i.e., the placeholder text the redactor
//!    emits must not itself look like a secret. A regression that
//!    chose a placeholder containing e.g. a Base64-shaped token
//!    would loop here.
//!
//! Input cap: 64 KiB, matching `WORKSPACE_SECRET_RISK_DEFAULT_MAX_SCAN_BYTES`
//! at `src/policy/mod.rs:857` so the fuzz corpus aligns with the
//! production scan-cap regime. Inputs larger than that are dropped
//! so we don't waste fuzz cycles on cases the production caller
//! would refuse outright.

#![no_main]

use ee::policy::{SecretRedactionReport, redact_secret_like_content};
use libfuzzer_sys::fuzz_target;

const MAX_INPUT_BYTES: usize = 64 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }
    // The redactor takes &str. Fuzz with the lossy-UTF-8 conversion
    // so invalid byte sequences still feed into the function — they
    // become U+FFFD replacement characters which is itself a corner
    // case worth exercising.
    let input = String::from_utf8_lossy(data);
    let input_ref: &str = input.as_ref();

    let first = redact_secret_like_content(input_ref);
    let second = redact_secret_like_content(input_ref);

    assert_reports_equal(&first, &second, "determinism");
    assert_invariants(input_ref, &first);

    // Re-redaction-on-output: feed the redactor's output through the
    // redactor again. The result MUST NOT flip from "no redactions
    // needed" to "redactions needed" — that would mean the
    // placeholder text the redactor emits looks like a secret to
    // itself, and the production caller would loop redacting its
    // own placeholders.
    let twice = redact_secret_like_content(&first.content);
    if !first.redacted && twice.redacted {
        panic!(
            "re-redaction flipped flag: input did not look secret-bearing but redactor's own output does (placeholder leak)"
        );
    }
    assert_invariants(&first.content, &twice);
});

fn assert_reports_equal(a: &SecretRedactionReport, b: &SecretRedactionReport, label: &str) {
    assert_eq!(a.content, b.content, "{label}: content differs");
    assert_eq!(a.redacted, b.redacted, "{label}: redacted flag differs");
    assert_eq!(
        a.redacted_reasons, b.redacted_reasons,
        "{label}: reasons differ",
    );
    assert_eq!(a.matches, b.matches, "{label}: matches differ");
}

fn assert_invariants(input: &str, report: &SecretRedactionReport) {
    let input_len = input.len();

    // Match-span bounds + non-decreasing start order.
    let mut last_start: Option<usize> = None;
    for m in &report.matches {
        assert!(
            m.start <= m.end,
            "match span start > end: pattern={} start={} end={}",
            m.pattern_id,
            m.start,
            m.end
        );
        assert!(
            m.end <= input_len,
            "match span end exceeds input length: pattern={} end={} input_len={input_len}",
            m.pattern_id,
            m.end
        );
        if let Some(prev) = last_start {
            assert!(
                m.start >= prev,
                "match spans must be in non-decreasing start order: prev_start={prev} curr_start={}",
                m.start
            );
        }
        last_start = Some(m.start);
    }

    // Flag/reasons consistency.
    if report.redacted {
        assert!(
            !report.redacted_reasons.is_empty(),
            "redacted=true but redacted_reasons is empty",
        );
    } else {
        assert!(
            report.redacted_reasons.is_empty(),
            "redacted=false but redacted_reasons is non-empty: {:?}",
            report.redacted_reasons,
        );
    }

    // Reasons sorted + deduped (the redactor explicitly does this at
    // the tail of redact_secret_like_content).
    let mut sorted = report.redacted_reasons.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        report.redacted_reasons, sorted,
        "redacted_reasons not sorted+deduped",
    );
}
