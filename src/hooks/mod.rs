mod installer;

pub use installer::{
    GIT_HOOK_AHEAD_RISK_SCHEMA_V1, GIT_HOOK_READINESS_SCHEMA_V1, GitHookAheadRiskSummary,
    GitHookReadinessFinding, GitHookReadinessHook, GitHookReadinessOptions,
    GitHookReadinessRecommendation, GitHookReadinessReport, GitHookReadinessSummary,
    HARNESS_CONFORMANCE_SCHEMA_V1, HARNESS_HOOK_INSTALL_SCHEMA_V1,
    HarnessConformanceArtifactPolicy, HarnessConformanceAssertion, HarnessConformanceCase,
    HarnessConformanceCompatibility, HarnessConformanceExpected, HarnessConformanceInput,
    HarnessConformanceSimulationOptions, HarnessConformanceSupport, HarnessConformanceTranscript,
    HarnessHookCapabilityGap, HarnessHookInstallAuditDocLink, HarnessHookInstallAuditFinding,
    HarnessHookInstallAuditRepair, HarnessHookInstallAuditReport, HarnessHookInstallOptions,
    HarnessHookInstallReport, HarnessHookMarkers, HarnessHookPlanItem, HarnessHookSnippet,
    HarnessHookTarget, check_git_hook_readiness, generate_harness_hook_install,
    simulate_harness_conformance,
};

pub const SUBSYSTEM: &str = "hooks";

#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

#[cfg(test)]
mod tests {
    use super::subsystem_name;

    #[test]
    fn subsystem_name_is_stable() {
        assert_eq!(subsystem_name(), "hooks");
    }
}
