//! Optional science analytics module (EE-171).
//!
//! This module provides offline statistical metrics, clustering diagnostics,
//! and deterministic diagram exports for evaluation and diagnostics. It is
//! gated behind the `science-analytics` feature flag to avoid bloating the
//! default agent loop with heavy dependencies.
//!
//! When the feature is disabled, the module exposes only stub types and
//! `is_available()` returning `false`. This allows callers to degrade
//! gracefully without compile-time feature checks everywhere.
//!
//! # Feature Flag
//!
//! Enable with `--features science-analytics` or add to your Cargo.toml:
//!
//! ```toml
//! [dependencies]
//! ee = { version = "0.1", features = ["science-analytics"] }
//! ```
//!
//! # Design Notes
//!
//! - All metrics must be deterministic given the same inputs.
//! - No network calls or LLM API usage.
//! - Diagram exports use text-based formats (Mermaid, DOT) for portability.
//! - Heavy computations should respect budget/cancellation via Asupersync.

/// Science analytics subsystem identifier.
pub const SUBSYSTEM: &str = "science";

/// Check if science analytics is available at runtime.
///
/// Returns `true` when the `science-analytics` feature is enabled,
/// `false` otherwise. This allows callers to degrade gracefully.
#[must_use]
pub const fn is_available() -> bool {
    cfg!(feature = "science-analytics")
}

/// Science analytics availability status for JSON output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScienceStatus {
    /// Feature enabled and ready.
    Available,
    /// Feature not compiled in.
    NotCompiled,
    /// Feature enabled but backend unavailable.
    BackendUnavailable,
}

impl ScienceStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::NotCompiled => "not_compiled",
            Self::BackendUnavailable => "backend_unavailable",
        }
    }

    #[must_use]
    pub const fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Get the current science analytics status.
#[must_use]
pub const fn status() -> ScienceStatus {
    if cfg!(feature = "science-analytics") {
        ScienceStatus::Available
    } else {
        ScienceStatus::NotCompiled
    }
}

/// Degradation code for science analytics unavailable.
pub const DEGRADATION_CODE_NOT_COMPILED: &str = "science_not_compiled";

/// Degradation code for science backend errors.
pub const DEGRADATION_CODE_BACKEND_UNAVAILABLE: &str = "science_backend_unavailable";

/// Degradation code for input too large for science analysis.
pub const DEGRADATION_CODE_INPUT_TOO_LARGE: &str = "science_input_too_large";

/// Degradation code for science budget exceeded.
pub const DEGRADATION_CODE_BUDGET_EXCEEDED: &str = "science_budget_exceeded";

#[cfg(feature = "science-analytics")]
mod enabled {
    //! Science analytics implementation when feature is enabled.
    //!
    //! This submodule contains the actual implementations that depend on
    //! science/numerical crates. It is only compiled when the feature is on.

    use super::*;

    /// Placeholder for science-backed evaluation metrics.
    #[derive(Clone, Debug, Default)]
    pub struct EvaluationMetrics {
        pub precision: Option<f64>,
        pub recall: Option<f64>,
        pub f1_score: Option<f64>,
    }

    impl EvaluationMetrics {
        #[must_use]
        pub fn compute(_predictions: &[bool], _ground_truth: &[bool]) -> Self {
            // Placeholder - actual implementation will use science crates
            Self::default()
        }
    }

    /// Placeholder for clustering diagnostics.
    #[derive(Clone, Debug, Default)]
    pub struct ClusteringDiagnostics {
        pub cluster_count: usize,
        pub silhouette_score: Option<f64>,
    }

    impl ClusteringDiagnostics {
        #[must_use]
        pub fn compute(_embeddings: &[Vec<f32>]) -> Self {
            // Placeholder - actual implementation will use science crates
            Self::default()
        }
    }
}

#[cfg(feature = "science-analytics")]
pub use enabled::*;

#[cfg(not(feature = "science-analytics"))]
mod disabled {
    //! Stub types when science-analytics feature is disabled.
    //!
    //! These stubs allow code to compile and degrade gracefully without
    //! compile-time feature checks scattered throughout the codebase.

    /// Stub evaluation metrics when feature is disabled.
    #[derive(Clone, Debug, Default)]
    pub struct EvaluationMetrics {
        pub precision: Option<f64>,
        pub recall: Option<f64>,
        pub f1_score: Option<f64>,
    }

    impl EvaluationMetrics {
        #[must_use]
        pub fn compute(_predictions: &[bool], _ground_truth: &[bool]) -> Self {
            Self::default()
        }
    }

    /// Stub clustering diagnostics when feature is disabled.
    #[derive(Clone, Debug, Default)]
    pub struct ClusteringDiagnostics {
        pub cluster_count: usize,
        pub silhouette_score: Option<f64>,
    }

    impl ClusteringDiagnostics {
        #[must_use]
        pub fn compute(_embeddings: &[Vec<f32>]) -> Self {
            Self::default()
        }
    }
}

#[cfg(not(feature = "science-analytics"))]
pub use disabled::*;

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: PartialEq + std::fmt::Debug>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn subsystem_name_is_science() -> TestResult {
        ensure(SUBSYSTEM, "science", "subsystem name")
    }

    #[test]
    fn status_returns_consistent_value() -> TestResult {
        let s = status();
        if cfg!(feature = "science-analytics") {
            ensure(s, ScienceStatus::Available, "status when enabled")
        } else {
            ensure(s, ScienceStatus::NotCompiled, "status when disabled")
        }
    }

    #[test]
    fn is_available_matches_feature_flag() -> TestResult {
        let available = is_available();
        if cfg!(feature = "science-analytics") {
            ensure(available, true, "is_available when enabled")
        } else {
            ensure(available, false, "is_available when disabled")
        }
    }

    #[test]
    fn science_status_as_str_is_stable() -> TestResult {
        ensure(
            ScienceStatus::Available.as_str(),
            "available",
            "available str",
        )?;
        ensure(
            ScienceStatus::NotCompiled.as_str(),
            "not_compiled",
            "not_compiled str",
        )?;
        ensure(
            ScienceStatus::BackendUnavailable.as_str(),
            "backend_unavailable",
            "backend_unavailable str",
        )
    }

    #[test]
    fn degradation_codes_are_stable() -> TestResult {
        ensure(
            DEGRADATION_CODE_NOT_COMPILED,
            "science_not_compiled",
            "not compiled code",
        )?;
        ensure(
            DEGRADATION_CODE_BACKEND_UNAVAILABLE,
            "science_backend_unavailable",
            "backend unavailable code",
        )?;
        ensure(
            DEGRADATION_CODE_INPUT_TOO_LARGE,
            "science_input_too_large",
            "input too large code",
        )?;
        ensure(
            DEGRADATION_CODE_BUDGET_EXCEEDED,
            "science_budget_exceeded",
            "budget exceeded code",
        )
    }

    #[test]
    fn evaluation_metrics_default_is_empty() -> TestResult {
        let metrics = EvaluationMetrics::default();
        ensure(metrics.precision, None, "precision is None")?;
        ensure(metrics.recall, None, "recall is None")?;
        ensure(metrics.f1_score, None, "f1_score is None")
    }

    #[test]
    fn clustering_diagnostics_default_is_empty() -> TestResult {
        let diag = ClusteringDiagnostics::default();
        ensure(diag.cluster_count, 0, "cluster_count is 0")?;
        ensure(diag.silhouette_score, None, "silhouette_score is None")
    }
}
