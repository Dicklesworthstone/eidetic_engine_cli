const SCALE: i64 = 1_000_000;
const MIN_POSITIVE_SCALED: i64 = 1;

/// Fixed-point BM25 term contribution.
///
/// This keeps the scalar and chunked implementations byte-identical by avoiding
/// floating-point reductions in the hot inner loop. The chunked path is the
/// module boundary for a later CPU-specific SIMD implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FixedPointBm25Term {
    pub idf_scaled: i64,
    pub k1_scaled: i64,
    pub b_scaled: i64,
    pub avg_doc_len_scaled: i64,
}

impl FixedPointBm25Term {
    #[must_use]
    pub fn from_float(idf: f32, k1: f32, b: f32, avg_doc_len: f32) -> Self {
        Self {
            idf_scaled: scale_nonnegative(idf),
            k1_scaled: scale_nonnegative(k1),
            b_scaled: scale_unit(b),
            avg_doc_len_scaled: scale_positive(avg_doc_len),
        }
    }
}

#[must_use]
pub fn score_scalar(term: FixedPointBm25Term, term_freqs: &[u32], doc_lens: &[u32]) -> Vec<i64> {
    term_freqs
        .iter()
        .zip(doc_lens)
        .map(|(&term_freq, &doc_len)| score_one(term, term_freq, doc_len))
        .collect()
}

#[must_use]
pub fn score_chunked(term: FixedPointBm25Term, term_freqs: &[u32], doc_lens: &[u32]) -> Vec<i64> {
    let mut scores = Vec::with_capacity(term_freqs.len().min(doc_lens.len()));
    for (freqs, lens) in term_freqs.chunks(8).zip(doc_lens.chunks(8)) {
        for (&term_freq, &doc_len) in freqs.iter().zip(lens) {
            scores.push(score_one(term, term_freq, doc_len));
        }
    }
    scores
}

fn score_one(term: FixedPointBm25Term, term_freq: u32, doc_len: u32) -> i64 {
    if term_freq == 0 {
        return 0;
    }
    let tf = i64::from(term_freq) * SCALE;
    let doc_len_scaled = i64::from(doc_len) * SCALE;
    let len_ratio = div_scaled(doc_len_scaled, term.avg_doc_len_scaled);
    let length_norm = SCALE
        .saturating_sub(term.b_scaled)
        .saturating_add(mul_scaled(term.b_scaled, len_ratio));
    let denominator = tf.saturating_add(mul_scaled(term.k1_scaled, length_norm));
    if denominator <= 0 {
        return 0;
    }
    let numerator = mul_scaled(tf, term.k1_scaled.saturating_add(SCALE));
    mul_scaled(term.idf_scaled, div_scaled(numerator, denominator)).max(0)
}

fn scale(value: f32) -> i64 {
    if !value.is_finite() {
        return 0;
    }
    let scaled = (f64::from(value) * SCALE as f64).round();
    i128_to_i64_saturating(scaled as i128)
}

fn scale_nonnegative(value: f32) -> i64 {
    scale(if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    })
}

fn scale_unit(value: f32) -> i64 {
    scale(if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    })
}

fn scale_positive(value: f32) -> i64 {
    let scaled = scale(if value.is_finite() && value > 0.0 {
        value
    } else {
        f32::EPSILON
    });
    scaled.max(MIN_POSITIVE_SCALED)
}

fn mul_scaled(left: i64, right: i64) -> i64 {
    i128_to_i64_saturating((left as i128 * right as i128) / i128::from(SCALE))
}

fn div_scaled(left: i64, right: i64) -> i64 {
    if right == 0 {
        return 0;
    }
    i128_to_i64_saturating((left as i128 * i128::from(SCALE)) / i128::from(right))
}

fn i128_to_i64_saturating(value: i128) -> i64 {
    if value > i128::from(i64::MAX) {
        i64::MAX
    } else if value < i128::from(i64::MIN) {
        i64::MIN
    } else {
        value as i64
    }
}

#[cfg(test)]
mod tests {
    use super::{FixedPointBm25Term, score_chunked, score_scalar};

    fn term() -> FixedPointBm25Term {
        FixedPointBm25Term::from_float(1.75, 1.2, 0.75, 42.0)
    }

    fn assert_parity(term_freqs: &[u32], doc_lens: &[u32]) {
        assert_eq!(
            score_scalar(term(), term_freqs, doc_lens),
            score_chunked(term(), term_freqs, doc_lens)
        );
    }

    #[test]
    fn simd_parity_empty() {
        assert_parity(&[], &[]);
    }

    #[test]
    fn simd_parity_single_doc() {
        assert_parity(&[3], &[42]);
    }

    #[test]
    fn simd_parity_sparse() {
        assert_parity(&[0, 1, 0, 3, 0, 8, 0], &[10, 12, 20, 42, 48, 64, 128]);
    }

    #[test]
    fn simd_parity_dense() {
        assert_parity(
            &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
            &[11, 22, 33, 44, 55, 66, 77, 88, 99, 111],
        );
    }

    #[test]
    fn zero_term_frequency_scores_zero() {
        assert_eq!(score_scalar(term(), &[0], &[42]), vec![0]);
    }

    #[test]
    fn malformed_term_parameters_fail_closed() {
        let term =
            FixedPointBm25Term::from_float(f32::NAN, f32::INFINITY, f32::NEG_INFINITY, f32::NAN);

        assert_eq!(term.idf_scaled, 0);
        assert_eq!(term.k1_scaled, 0);
        assert_eq!(term.b_scaled, 0);
        assert!(term.avg_doc_len_scaled > 0);
        assert_eq!(
            score_scalar(term, &[u32::MAX, 4], &[u32::MAX, 0]),
            score_chunked(term, &[u32::MAX, 4], &[u32::MAX, 0])
        );
        assert_eq!(
            score_scalar(term, &[u32::MAX, 4], &[u32::MAX, 0]),
            vec![0, 0]
        );
    }

    #[test]
    fn extreme_fixed_point_values_saturate_without_wrapping_negative() {
        let term = FixedPointBm25Term {
            idf_scaled: i64::MAX,
            k1_scaled: i64::MAX,
            b_scaled: i64::MAX,
            avg_doc_len_scaled: 1,
        };

        let scores = score_scalar(term, &[u32::MAX, 1], &[u32::MAX, 1]);

        assert_eq!(scores.len(), 2);
        assert!(scores.iter().all(|score| *score >= 0));
        assert_eq!(scores, score_chunked(term, &[u32::MAX, 1], &[u32::MAX, 1]));
    }
}
