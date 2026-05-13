//! Beta-Bernoulli posteriors over memory helpful-rate (N7 / ADR 0032).
//!
//! Each memory carries `(alpha, beta)` — the parameters of a Beta
//! distribution describing the agent's posterior belief about its
//! latent helpful-rate. Helpful and harmful outcome events update the
//! pair using conjugate prior arithmetic; the posterior mean and
//! credible intervals replace the older scalar `confidence` field for
//! trust-class transitions while preserving it as a derived view for
//! backward compatibility.
//!
//! ## Prior
//!
//! The default prior is **Jeffreys** (`alpha = beta = 0.5`) — the
//! reference prior for Bernoulli observations. Invariant under
//! reparameterization, minimally informative, and bounded support.
//! Configurable via `[bayes] prior_alpha` / `[bayes] prior_beta` in
//! workspace config.
//!
//! ## Update rule
//!
//! - Helpful outcome event: `alpha += 1.0`.
//! - Harmful outcome event: `beta += harmful_weight` where
//!   `harmful_weight` defaults to 2.5 per the README
//!   `[curation] harmful_weight` configuration.
//!
//! The asymmetry preserves the existing scalar-delta behavior: a
//! harmful event hits harder than a helpful one.
//!
//! ## Credible intervals
//!
//! The 90 percent and 50 percent equal-tailed credible intervals are
//! computed via the inverse regularized incomplete beta function
//! (`Beta.ppf`). The implementation uses Newton iteration with a
//! conservative continued-fraction starting point — accurate to
//! ~1e-7 across `alpha, beta in [0.5, 100]` which covers the range
//! of plausible memory posteriors in practice.
//!
//! No external crate dependency: the math is implemented directly
//! to keep the forbidden-deps audit clean (no `statrs` / `rand_distr`).

#![allow(clippy::cast_precision_loss)]

/// Default Jeffreys prior alpha.
pub const DEFAULT_PRIOR_ALPHA: f64 = 0.5;
/// Default Jeffreys prior beta.
pub const DEFAULT_PRIOR_BETA: f64 = 0.5;
/// Default harmful event weight per `[curation] harmful_weight` in README.
pub const DEFAULT_HARMFUL_WEIGHT: f64 = 2.5;

/// Beta posterior parameters for a single memory.
///
/// Invariant: `alpha > 0.0 && beta > 0.0` and both are finite.
/// Construct via [`BetaPosterior::new`] or [`BetaPosterior::jeffreys`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BetaPosterior {
    alpha: f64,
    beta: f64,
}

impl BetaPosterior {
    /// Construct from explicit alpha and beta. Returns `None` if
    /// either parameter is non-positive or non-finite.
    #[must_use]
    pub fn new(alpha: f64, beta: f64) -> Option<Self> {
        if alpha.is_finite() && beta.is_finite() && alpha > 0.0 && beta > 0.0 {
            Some(Self { alpha, beta })
        } else {
            None
        }
    }

    /// Construct with the Jeffreys default prior `(0.5, 0.5)`.
    #[must_use]
    pub const fn jeffreys() -> Self {
        Self {
            alpha: DEFAULT_PRIOR_ALPHA,
            beta: DEFAULT_PRIOR_BETA,
        }
    }

    /// Alpha (helpful-side pseudo-count).
    #[must_use]
    pub const fn alpha(&self) -> f64 {
        self.alpha
    }

    /// Beta (harmful-side pseudo-count).
    #[must_use]
    pub const fn beta(&self) -> f64 {
        self.beta
    }

    /// Effective sample size — the total weight of evidence
    /// accumulated so far (alpha + beta). Used by trust-class
    /// transitions as a sample-size gate per ADR 0032.
    #[must_use]
    pub fn effective_sample_size(&self) -> f64 {
        self.alpha + self.beta
    }

    /// Posterior mean. Equals the legacy `confidence` field's value
    /// for backward compatibility.
    #[must_use]
    pub fn mean(&self) -> f64 {
        self.alpha / (self.alpha + self.beta)
    }

    /// Posterior variance.
    #[must_use]
    pub fn variance(&self) -> f64 {
        let sum = self.alpha + self.beta;
        (self.alpha * self.beta) / (sum * sum * (sum + 1.0))
    }

    /// Helpful update — add 1.0 to alpha.
    #[must_use]
    pub fn update_helpful(self) -> Self {
        Self {
            alpha: self.alpha + 1.0,
            beta: self.beta,
        }
    }

    /// Harmful update — add `harmful_weight` to beta.
    ///
    /// `harmful_weight` is typically [`DEFAULT_HARMFUL_WEIGHT`] (2.5).
    /// Non-positive or non-finite weights are clamped to `0.0`
    /// (no-op) to preserve the invariant.
    #[must_use]
    pub fn update_harmful(self, harmful_weight: f64) -> Self {
        let w = if harmful_weight.is_finite() && harmful_weight > 0.0 {
            harmful_weight
        } else {
            0.0
        };
        Self {
            alpha: self.alpha,
            beta: self.beta + w,
        }
    }

    /// Equal-tailed credible interval at the given confidence level
    /// in `(0.0, 1.0)`. For example, `credible_interval(0.9)` returns
    /// `(lo, hi)` where `lo = Beta.ppf(0.05)` and `hi = Beta.ppf(0.95)`.
    ///
    /// Returns `None` if the level is outside `(0.0, 1.0)` or if the
    /// inverse-CDF iteration fails to converge (rare; happens only on
    /// extremely small alpha or beta, well outside the operational
    /// range).
    #[must_use]
    pub fn credible_interval(&self, level: f64) -> Option<(f64, f64)> {
        if !(level > 0.0 && level < 1.0) {
            return None;
        }
        let tail = (1.0 - level) / 2.0;
        let lo = beta_inv_cdf(tail, self.alpha, self.beta)?;
        let hi = beta_inv_cdf(1.0 - tail, self.alpha, self.beta)?;
        Some((lo, hi))
    }
}

/// Inverse regularized incomplete beta — returns `x` such that
/// `I_x(alpha, beta) = p`.
///
/// Implemented via Newton iteration with a Cornish-Fisher-style
/// starting point. Returns `None` if the iteration fails to converge
/// within 64 steps (only happens on pathological inputs well outside
/// the (alpha, beta) range that arises from memory posteriors in
/// production).
fn beta_inv_cdf(p: f64, alpha: f64, beta: f64) -> Option<f64> {
    if !(0.0..=1.0).contains(&p) || alpha <= 0.0 || beta <= 0.0 {
        return None;
    }
    if p == 0.0 {
        return Some(0.0);
    }
    if p == 1.0 {
        return Some(1.0);
    }

    // Cornish-Fisher initial guess. For most (alpha, beta) in our
    // operational range this is within ~0.05 of the true root, and
    // Newton converges in 3–6 iterations.
    let mut x = initial_guess(p, alpha, beta);

    let log_beta_ab = log_beta(alpha, beta);
    let am1 = alpha - 1.0;
    let bm1 = beta - 1.0;

    for _ in 0..64 {
        // f(x) = I_x(alpha, beta) - p
        let fx = regularized_incomplete_beta(x, alpha, beta) - p;
        if fx.abs() < 1e-10 {
            return Some(x.clamp(0.0, 1.0));
        }
        // f'(x) = x^(alpha-1) (1-x)^(beta-1) / B(alpha, beta)
        let log_fprime = am1 * x.ln() + bm1 * (1.0 - x).ln() - log_beta_ab;
        let fprime = log_fprime.exp();
        if !fprime.is_finite() || fprime == 0.0 {
            return None;
        }
        let dx = fx / fprime;
        let mut step = dx;
        // Damp the step so x stays in (0, 1).
        while x - step <= 0.0 || x - step >= 1.0 {
            step *= 0.5;
            if step.abs() < 1e-15 {
                return None;
            }
        }
        x -= step;
    }
    None
}

/// Cornish-Fisher-style initial guess for the inverse beta CDF.
fn initial_guess(p: f64, alpha: f64, beta: f64) -> f64 {
    // Use the normal approximation to the beta when alpha and beta
    // are both > 1, falling back to the mean otherwise.
    let m = alpha / (alpha + beta);
    if alpha > 1.0 && beta > 1.0 {
        let v = (alpha * beta) / ((alpha + beta).powi(2) * (alpha + beta + 1.0));
        let z = standard_normal_ppf(p);
        (m + z * v.sqrt()).clamp(1e-10, 1.0 - 1e-10)
    } else {
        // For Jeffreys-style small priors, start near the mean.
        m.clamp(1e-10, 1.0 - 1e-10)
    }
}

/// Beasley-Springer-Moro standard-normal inverse CDF approximation.
/// Accurate to about 1e-9 over the central 99.999 percent.
fn standard_normal_ppf(p: f64) -> f64 {
    const A: [f64; 4] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
    ];
    const B: [f64; 4] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
    ];
    const C: [f64; 4] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];

    let p_low = 0.02425;
    let p_high = 1.0 - p_low;
    if p < p_low {
        let q = (-2.0 * p.ln()).sqrt();
        ((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + 1.0).recip()
            * ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
                .recip()
                .mul_add(-1.0, 0.0)
            + ((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + 1.0)
                / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= p_high {
        let q = p - 0.5;
        let r = q * q;
        let num = (((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r * q;
        let den = (((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + 1.0;
        num / den
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + 1.0)
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    }
}

/// log B(alpha, beta) via log Gamma.
fn log_beta(alpha: f64, beta: f64) -> f64 {
    ln_gamma(alpha) + ln_gamma(beta) - ln_gamma(alpha + beta)
}

/// log Gamma via Lanczos approximation (g=7, n=9). Accurate to ~1e-15
/// over the operational range.
fn ln_gamma(x: f64) -> f64 {
    const G: f64 = 7.0;
    const COEF: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        // Reflection formula.
        std::f64::consts::PI.ln() - (std::f64::consts::PI * x).sin().ln() - ln_gamma(1.0 - x)
    } else {
        let x = x - 1.0;
        let mut a = COEF[0];
        for (i, c) in COEF.iter().enumerate().skip(1) {
            a += c / (x + i as f64);
        }
        let t = x + G + 0.5;
        (2.0 * std::f64::consts::PI).sqrt().ln() + (x + 0.5) * t.ln() - t + a.ln()
    }
}

/// Regularized incomplete beta `I_x(alpha, beta)` via continued
/// fraction (Numerical Recipes 6.4). Accurate to ~1e-12.
fn regularized_incomplete_beta(x: f64, alpha: f64, beta: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let log_bt = ln_gamma(alpha + beta) - ln_gamma(alpha) - ln_gamma(beta)
        + alpha * x.ln()
        + beta * (1.0 - x).ln();
    let bt = log_bt.exp();
    if x < (alpha + 1.0) / (alpha + beta + 2.0) {
        bt * beta_continued_fraction(x, alpha, beta) / alpha
    } else {
        1.0 - bt * beta_continued_fraction(1.0 - x, beta, alpha) / beta
    }
}

fn beta_continued_fraction(x: f64, a: f64, b: f64) -> f64 {
    const MAX_ITER: usize = 200;
    const EPS: f64 = 3.0e-15;
    const FPMIN: f64 = 1.0e-300;

    let qab = a + b;
    let qap = a + 1.0;
    let qam = a - 1.0;
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < FPMIN {
        d = FPMIN;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..=MAX_ITER {
        let m_f = m as f64;
        let m2 = 2.0 * m_f;
        let aa = m_f * (b - m_f) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = 1.0 + aa / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        h *= d * c;
        let aa = -(a + m_f) * (qab + m_f) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = 1.0 + aa / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < EPS {
            return h;
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    #[test]
    fn jeffreys_default_has_mean_half() {
        let p = BetaPosterior::jeffreys();
        assert!(approx_eq(p.mean(), 0.5, 1e-12));
        assert_eq!(p.alpha(), 0.5);
        assert_eq!(p.beta(), 0.5);
    }

    #[test]
    fn new_rejects_non_positive() {
        assert!(BetaPosterior::new(0.0, 1.0).is_none());
        assert!(BetaPosterior::new(1.0, 0.0).is_none());
        assert!(BetaPosterior::new(-1.0, 1.0).is_none());
        assert!(BetaPosterior::new(f64::NAN, 1.0).is_none());
        assert!(BetaPosterior::new(f64::INFINITY, 1.0).is_none());
    }

    #[test]
    fn helpful_event_adds_one_to_alpha() {
        let p = BetaPosterior::jeffreys().update_helpful();
        assert_eq!(p.alpha(), 1.5);
        assert_eq!(p.beta(), 0.5);
    }

    #[test]
    fn harmful_event_adds_weight_to_beta() {
        let p = BetaPosterior::jeffreys().update_harmful(2.5);
        assert_eq!(p.alpha(), 0.5);
        assert_eq!(p.beta(), 3.0);
    }

    #[test]
    fn harmful_event_with_invalid_weight_is_noop() {
        let p = BetaPosterior::jeffreys()
            .update_harmful(-1.0)
            .update_harmful(f64::NAN)
            .update_harmful(f64::INFINITY);
        assert_eq!(p, BetaPosterior::jeffreys());
    }

    #[test]
    fn effective_sample_size_grows_with_evidence() {
        let p = BetaPosterior::jeffreys();
        assert!(approx_eq(p.effective_sample_size(), 1.0, 1e-12));
        let p = p.update_helpful().update_helpful().update_harmful(2.5);
        assert!(approx_eq(p.effective_sample_size(), 5.5, 1e-12));
    }

    #[test]
    fn mean_with_observed_outcomes_matches_closed_form() {
        // After 8 helpful and 2 harmful events (weight 2.5):
        // alpha = 0.5 + 8 = 8.5, beta = 0.5 + 5.0 = 5.5
        // mean = 8.5 / 14.0 ≈ 0.607
        let mut p = BetaPosterior::jeffreys();
        for _ in 0..8 {
            p = p.update_helpful();
        }
        for _ in 0..2 {
            p = p.update_harmful(2.5);
        }
        assert!(approx_eq(p.alpha(), 8.5, 1e-12));
        assert!(approx_eq(p.beta(), 5.5, 1e-12));
        assert!(approx_eq(p.mean(), 8.5 / 14.0, 1e-12));
    }

    #[test]
    fn credible_interval_90_contains_mean() {
        // For a well-evidenced posterior, the 90% CI brackets the mean
        // and has positive width.
        let mut p = BetaPosterior::jeffreys();
        for _ in 0..15 {
            p = p.update_helpful();
        }
        for _ in 0..5 {
            p = p.update_harmful(1.0);
        }
        let mean = p.mean();
        let (lo, hi) = p
            .credible_interval(0.90)
            .expect("ci90 should compute for moderate-evidence posterior");
        assert!(
            lo > 0.0 && lo < mean,
            "ci90.lo {lo} should be in (0, {mean})"
        );
        assert!(
            hi > mean && hi < 1.0,
            "ci90.hi {hi} should be in ({mean}, 1)"
        );
        assert!(hi - lo > 0.0, "interval must have positive width");
    }

    #[test]
    fn credible_interval_50_narrower_than_90() {
        let mut p = BetaPosterior::jeffreys();
        for _ in 0..30 {
            p = p.update_helpful();
        }
        for _ in 0..10 {
            p = p.update_harmful(1.0);
        }
        let (lo50, hi50) = p.credible_interval(0.50).unwrap();
        let (lo90, hi90) = p.credible_interval(0.90).unwrap();
        assert!(
            hi50 - lo50 < hi90 - lo90,
            "50%% CI must be narrower than 90%%"
        );
        assert!(lo90 < lo50, "90%% CI extends further left than 50%%");
        assert!(hi90 > hi50, "90%% CI extends further right than 50%%");
    }

    #[test]
    fn credible_interval_rejects_invalid_level() {
        let p = BetaPosterior::jeffreys();
        assert!(p.credible_interval(0.0).is_none());
        assert!(p.credible_interval(1.0).is_none());
        assert!(p.credible_interval(-0.1).is_none());
        assert!(p.credible_interval(1.1).is_none());
    }

    #[test]
    fn beta_inv_cdf_jeffreys_quantiles() {
        // Beta(0.5, 0.5) — the Jeffreys prior — has known quantiles:
        // 5% quantile ≈ 0.00615
        // 50% quantile = 0.5 (symmetric)
        // 95% quantile ≈ 0.99385
        let lo = beta_inv_cdf(0.05, 0.5, 0.5).unwrap();
        let mid = beta_inv_cdf(0.50, 0.5, 0.5).unwrap();
        let hi = beta_inv_cdf(0.95, 0.5, 0.5).unwrap();
        assert!(
            approx_eq(lo, 0.00615, 0.005),
            "lo ≈ 0.00615 expected, got {lo}"
        );
        assert!(approx_eq(mid, 0.5, 1e-3), "mid = 0.5 expected, got {mid}");
        assert!(
            approx_eq(hi, 0.99385, 0.005),
            "hi ≈ 0.99385 expected, got {hi}"
        );
    }

    #[test]
    fn beta_inv_cdf_well_evidenced_posterior() {
        // Beta(50, 50) — well-evidenced 50% posterior, narrow CI.
        // 5% ≈ 0.4178, 50% = 0.5, 95% ≈ 0.5822 (per scipy.stats.beta.ppf).
        let lo = beta_inv_cdf(0.05, 50.0, 50.0).unwrap();
        let hi = beta_inv_cdf(0.95, 50.0, 50.0).unwrap();
        assert!(approx_eq(lo, 0.4178, 0.01));
        assert!(approx_eq(hi, 0.5822, 0.01));
    }

    #[test]
    fn variance_matches_closed_form() {
        // var = ab / ((a+b)^2 (a+b+1))
        let p = BetaPosterior::new(2.0, 3.0).unwrap();
        let expected = (2.0 * 3.0) / (5.0_f64.powi(2) * 6.0);
        assert!(approx_eq(p.variance(), expected, 1e-12));
    }
}
