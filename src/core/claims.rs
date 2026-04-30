//! Claim verification core services (EE-362).
//!
//! Provides the business logic for listing, showing, and verifying
//! executable claims defined in claims.yaml.

use crate::models::{ClaimStatus, ManifestVerificationStatus, VerificationFrequency};

pub const CLAIM_LIST_SCHEMA_V1: &str = "ee.claim_list.v1";
pub const CLAIM_SHOW_SCHEMA_V1: &str = "ee.claim_show.v1";
pub const CLAIM_VERIFY_SCHEMA_V1: &str = "ee.claim_verify.v1";

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
}
