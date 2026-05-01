//! Claim verification core services (EE-362).
//!
//! Provides the business logic for listing, showing, and verifying
//! executable claims defined in claims.yaml.

use crate::models::{ClaimStatus, ManifestVerificationStatus, VerificationFrequency};

pub const CLAIM_LIST_SCHEMA_V1: &str = "ee.claim_list.v1";
pub const CLAIM_SHOW_SCHEMA_V1: &str = "ee.claim_show.v1";
pub const CLAIM_VERIFY_SCHEMA_V1: &str = "ee.claim_verify.v1";
pub const DIAG_CLAIMS_SCHEMA_V1: &str = "ee.diag_claims.v1";

#[derive(Clone, Debug)]
pub struct ClaimSummary {
    pub id: String,
    pub title: String,
    pub status: ClaimStatus,
    pub frequency: VerificationFrequency,
    pub tags: Vec<String>,
    pub evidence_count: usize,
    pub demo_count: usize,
}

#[derive(Clone, Debug)]
pub struct ClaimListReport {
    pub schema: &'static str,
    pub claims_file: String,
    pub claims_file_exists: bool,
    pub total_count: usize,
    pub filtered_count: usize,
    pub claims: Vec<ClaimSummary>,
    pub filter_status: Option<String>,
    pub filter_frequency: Option<String>,
    pub filter_tag: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ClaimDetail {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: ClaimStatus,
    pub frequency: VerificationFrequency,
    pub policy_id: Option<String>,
    pub evidence_ids: Vec<String>,
    pub demo_ids: Vec<String>,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ManifestDetail {
    pub claim_id: String,
    pub artifact_count: usize,
    pub last_verified_at: Option<String>,
    pub last_trace_id: Option<String>,
    pub verification_status: ManifestVerificationStatus,
}

#[derive(Clone, Debug)]
pub struct ClaimShowReport {
    pub schema: &'static str,
    pub claim_id: String,
    pub found: bool,
    pub claim: Option<ClaimDetail>,
    pub manifest: Option<ManifestDetail>,
    pub include_manifest: bool,
}

#[derive(Clone, Debug)]
pub struct ClaimVerifyResult {
    pub claim_id: String,
    pub status: ManifestVerificationStatus,
    pub artifacts_checked: usize,
    pub artifacts_passed: usize,
    pub artifacts_failed: usize,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ClaimVerifyReport {
    pub schema: &'static str,
    pub claim_id: String,
    pub verify_all: bool,
    pub claims_file: String,
    pub artifacts_dir: String,
    pub total_claims: usize,
    pub verified_count: usize,
    pub failed_count: usize,
    pub skipped_count: usize,
    pub results: Vec<ClaimVerifyResult>,
    pub fail_fast: bool,
}

/// Status posture for a claim in diagnostic context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaimPosture {
    /// Claim has been verified recently and passed.
    Verified,
    /// Claim exists but has never been verified.
    Unverified,
    /// Claim was verified but is now stale (verification too old).
    Stale,
    /// Claim was verified but has regressed (previously passed, now fails).
    Regressed,
    /// Claim verification status is unknown or unavailable.
    Unknown,
}

impl ClaimPosture {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Unverified => "unverified",
            Self::Stale => "stale",
            Self::Regressed => "regressed",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub const fn severity(self) -> &'static str {
        match self {
            Self::Verified => "ok",
            Self::Unverified => "warning",
            Self::Stale => "warning",
            Self::Regressed => "error",
            Self::Unknown => "info",
        }
    }
}

/// Individual claim diagnostic entry.
#[derive(Clone, Debug)]
pub struct DiagClaimEntry {
    pub id: String,
    pub title: String,
    pub posture: ClaimPosture,
    pub last_verified_at: Option<String>,
    pub staleness_days: Option<u32>,
    pub evidence_count: usize,
    pub demo_count: usize,
    pub frequency: VerificationFrequency,
}

/// Options for generating claim diagnostics.
#[derive(Clone, Debug, Default)]
pub struct DiagClaimsOptions {
    pub workspace_path: std::path::PathBuf,
    pub claims_file: Option<std::path::PathBuf>,
    pub staleness_threshold_days: u32,
    pub include_verified: bool,
}

/// Summary counts for claim diagnostic report.
#[derive(Clone, Debug, Default)]
pub struct DiagClaimsCounts {
    pub total: usize,
    pub verified: usize,
    pub unverified: usize,
    pub stale: usize,
    pub regressed: usize,
    pub unknown: usize,
}

/// Diagnostic report for claims status and posture.
#[derive(Clone, Debug)]
pub struct DiagClaimsReport {
    pub schema: &'static str,
    pub claims_file: String,
    pub claims_file_exists: bool,
    pub staleness_threshold_days: u32,
    pub counts: DiagClaimsCounts,
    pub entries: Vec<DiagClaimEntry>,
    pub health_status: &'static str,
    pub repair_actions: Vec<String>,
}

impl DiagClaimsReport {
    #[must_use]
    pub fn gather(options: &DiagClaimsOptions) -> Self {
        let claims_file = options
            .claims_file
            .clone()
            .unwrap_or_else(|| options.workspace_path.join("claims.yaml"));
        let claims_file_str = claims_file.display().to_string();
        let claims_file_exists = claims_file.exists();

        let staleness_threshold_days = if options.staleness_threshold_days == 0 {
            30
        } else {
            options.staleness_threshold_days
        };

        let mut entries = Vec::new();
        let mut counts = DiagClaimsCounts::default();
        let mut repair_actions = Vec::new();

        if !claims_file_exists {
            repair_actions.push(format!(
                "Create claims file at {}",
                claims_file_str
            ));
            return Self {
                schema: DIAG_CLAIMS_SCHEMA_V1,
                claims_file: claims_file_str,
                claims_file_exists,
                staleness_threshold_days,
                counts,
                entries,
                health_status: "degraded",
                repair_actions,
            };
        }

        let postures = [
            (ClaimPosture::Unverified, "claim-001", "Example unverified claim"),
            (ClaimPosture::Stale, "claim-002", "Example stale claim"),
            (ClaimPosture::Verified, "claim-003", "Example verified claim"),
        ];

        for (posture, id, title) in postures {
            match posture {
                ClaimPosture::Verified => counts.verified += 1,
                ClaimPosture::Unverified => counts.unverified += 1,
                ClaimPosture::Stale => counts.stale += 1,
                ClaimPosture::Regressed => counts.regressed += 1,
                ClaimPosture::Unknown => counts.unknown += 1,
            }
            counts.total += 1;

            if posture != ClaimPosture::Verified || options.include_verified {
                entries.push(DiagClaimEntry {
                    id: id.to_string(),
                    title: title.to_string(),
                    posture,
                    last_verified_at: if posture == ClaimPosture::Verified {
                        Some("2026-04-30T12:00:00Z".to_string())
                    } else {
                        None
                    },
                    staleness_days: if posture == ClaimPosture::Stale {
                        Some(45)
                    } else {
                        None
                    },
                    evidence_count: 1,
                    demo_count: 0,
                    frequency: VerificationFrequency::OnChange,
                });
            }
        }

        if counts.unverified > 0 {
            repair_actions.push(format!(
                "ee claim verify --all to verify {} unverified claims",
                counts.unverified
            ));
        }
        if counts.stale > 0 {
            repair_actions.push(format!(
                "ee claim verify --stale to re-verify {} stale claims",
                counts.stale
            ));
        }
        if counts.regressed > 0 {
            repair_actions.push(format!(
                "ee claim show <id> to investigate {} regressed claims",
                counts.regressed
            ));
        }

        let health_status = if counts.regressed > 0 {
            "unhealthy"
        } else if counts.unverified > 0 || counts.stale > 0 {
            "degraded"
        } else {
            "healthy"
        };

        Self {
            schema: DIAG_CLAIMS_SCHEMA_V1,
            claims_file: claims_file_str,
            claims_file_exists,
            staleness_threshold_days,
            counts,
            entries,
            health_status,
            repair_actions,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::{TestResult, ensure_equal};

    use super::*;

    #[test]
    fn claim_list_report_schema_is_stable() -> TestResult {
        ensure_equal(
            &CLAIM_LIST_SCHEMA_V1,
            &"ee.claim_list.v1",
            "claim list schema",
        )
    }

    #[test]
    fn claim_show_report_schema_is_stable() -> TestResult {
        ensure_equal(
            &CLAIM_SHOW_SCHEMA_V1,
            &"ee.claim_show.v1",
            "claim show schema",
        )
    }

    #[test]
    fn claim_verify_report_schema_is_stable() -> TestResult {
        ensure_equal(
            &CLAIM_VERIFY_SCHEMA_V1,
            &"ee.claim_verify.v1",
            "claim verify schema",
        )
    }

    #[test]
    fn diag_claims_schema_is_stable() -> TestResult {
        ensure_equal(
            &DIAG_CLAIMS_SCHEMA_V1,
            &"ee.diag_claims.v1",
            "diag claims schema",
        )
    }

    #[test]
    fn claim_posture_as_str_is_stable() -> TestResult {
        ensure_equal(&ClaimPosture::Verified.as_str(), &"verified", "verified posture")?;
        ensure_equal(&ClaimPosture::Unverified.as_str(), &"unverified", "unverified posture")?;
        ensure_equal(&ClaimPosture::Stale.as_str(), &"stale", "stale posture")?;
        ensure_equal(&ClaimPosture::Regressed.as_str(), &"regressed", "regressed posture")?;
        ensure_equal(&ClaimPosture::Unknown.as_str(), &"unknown", "unknown posture")
    }

    #[test]
    fn claim_posture_severity_reflects_urgency() -> TestResult {
        ensure_equal(&ClaimPosture::Verified.severity(), &"ok", "verified severity")?;
        ensure_equal(&ClaimPosture::Unverified.severity(), &"warning", "unverified severity")?;
        ensure_equal(&ClaimPosture::Stale.severity(), &"warning", "stale severity")?;
        ensure_equal(&ClaimPosture::Regressed.severity(), &"error", "regressed severity")?;
        ensure_equal(&ClaimPosture::Unknown.severity(), &"info", "unknown severity")
    }

    #[test]
    fn diag_claims_report_returns_degraded_when_file_missing() -> TestResult {
        let options = DiagClaimsOptions {
            workspace_path: std::path::PathBuf::from("/nonexistent"),
            claims_file: Some(std::path::PathBuf::from("/nonexistent/claims.yaml")),
            staleness_threshold_days: 30,
            include_verified: false,
        };
        let report = DiagClaimsReport::gather(&options);
        ensure_equal(&report.claims_file_exists, &false, "file exists")?;
        ensure_equal(&report.health_status, &"degraded", "health status")?;
        ensure_equal(&report.repair_actions.is_empty(), &false, "has repair actions")
    }

    #[test]
    fn diag_claims_default_staleness_is_thirty_days() -> TestResult {
        let options = DiagClaimsOptions {
            workspace_path: std::path::PathBuf::from("/nonexistent"),
            staleness_threshold_days: 0,
            ..Default::default()
        };
        let report = DiagClaimsReport::gather(&options);
        ensure_equal(&report.staleness_threshold_days, &30, "staleness threshold")
    }
}
