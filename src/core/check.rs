//! Check command handler (EE-027).
//!
//! Provides a quick posture summary indicating whether the workspace
//! is ready for normal operation or requires attention.

use crate::models::CapabilityStatus;

/// Overall posture of the workspace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Posture {
    /// All systems operational, ready for normal use.
    Ready,
    /// Some features degraded but core functionality works.
    Degraded,
    /// Workspace requires initialization or repair.
    NeedsAttention,
}

impl Posture {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Degraded => "degraded",
            Self::NeedsAttention => "needs_attention",
        }
    }

    #[must_use]
    pub const fn is_usable(self) -> bool {
        matches!(self, Self::Ready | Self::Degraded)
    }
}

/// A suggested action for the user.
#[derive(Clone, Debug)]
pub struct SuggestedAction {
    pub priority: u8,
    pub command: &'static str,
    pub reason: &'static str,
}

/// Full check/posture report.
#[derive(Clone, Debug)]
pub struct CheckReport {
    pub version: &'static str,
    pub posture: Posture,
    pub workspace_initialized: bool,
    pub database_ready: bool,
    pub search_ready: bool,
    pub runtime_ready: bool,
    pub suggested_actions: Vec<SuggestedAction>,
}

impl CheckReport {
    /// Gather current posture.
    #[must_use]
    pub fn gather() -> Self {
        let runtime_ready = true;
        let workspace_initialized = false;
        let database_ready = false;
        let search_ready = false;

        let mut suggested_actions = Vec::new();

        if !workspace_initialized {
            suggested_actions.push(SuggestedAction {
                priority: 1,
                command: "ee init --workspace .",
                reason: "Initialize workspace to enable memory storage.",
            });
        }

        if !database_ready {
            suggested_actions.push(SuggestedAction {
                priority: 2,
                command: "ee doctor --json",
                reason: "Run diagnostics to identify database issues.",
            });
        }

        if !search_ready {
            suggested_actions.push(SuggestedAction {
                priority: 3,
                command: "ee index rebuild",
                reason: "Rebuild search index when available.",
            });
        }

        let posture = if runtime_ready && workspace_initialized && database_ready {
            if search_ready {
                Posture::Ready
            } else {
                Posture::Degraded
            }
        } else {
            Posture::NeedsAttention
        };

        Self {
            version: env!("CARGO_PKG_VERSION"),
            posture,
            workspace_initialized,
            database_ready,
            search_ready,
            runtime_ready,
            suggested_actions,
        }
    }

    /// Derive capability status from check state.
    #[must_use]
    pub fn runtime_status(&self) -> CapabilityStatus {
        if self.runtime_ready {
            CapabilityStatus::Ready
        } else {
            CapabilityStatus::Degraded
        }
    }

    #[must_use]
    pub fn storage_status(&self) -> CapabilityStatus {
        if self.database_ready {
            CapabilityStatus::Ready
        } else if self.workspace_initialized {
            CapabilityStatus::Degraded
        } else {
            CapabilityStatus::Unimplemented
        }
    }

    #[must_use]
    pub fn search_status(&self) -> CapabilityStatus {
        if self.search_ready {
            CapabilityStatus::Ready
        } else {
            CapabilityStatus::Unimplemented
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn check_report_gather_returns_posture() -> TestResult {
        let report = CheckReport::gather();

        ensure(report.runtime_ready, true, "runtime should be ready")?;
        ensure(
            report.posture,
            Posture::NeedsAttention,
            "posture needs attention without workspace",
        )
    }

    #[test]
    fn check_report_suggests_init_when_not_initialized() -> TestResult {
        let report = CheckReport::gather();

        let has_init = report
            .suggested_actions
            .iter()
            .any(|a| a.command.contains("init"));

        ensure(has_init, true, "should suggest init")
    }

    #[test]
    fn posture_strings_are_stable() -> TestResult {
        ensure(Posture::Ready.as_str(), "ready", "ready")?;
        ensure(Posture::Degraded.as_str(), "degraded", "degraded")?;
        ensure(
            Posture::NeedsAttention.as_str(),
            "needs_attention",
            "needs_attention",
        )
    }

    #[test]
    fn posture_usable_classification() -> TestResult {
        ensure(Posture::Ready.is_usable(), true, "ready is usable")?;
        ensure(Posture::Degraded.is_usable(), true, "degraded is usable")?;
        ensure(
            Posture::NeedsAttention.is_usable(),
            false,
            "needs_attention is not usable",
        )
    }
}
