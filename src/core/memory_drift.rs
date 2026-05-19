//! Memory drift hints used by retrieval surfaces.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryDriftStatus {
    Current,
    Changed,
    MissingSource,
    Unverifiable,
}

impl MemoryDriftStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Changed => "changed",
            Self::MissingSource => "missing_source",
            Self::Unverifiable => "unverifiable",
        }
    }

    #[must_use]
    pub const fn severity_rank(self) -> u8 {
        match self {
            Self::Current => 0,
            Self::Unverifiable => 1,
            Self::Changed => 2,
            Self::MissingSource => 3,
        }
    }

    #[must_use]
    pub const fn degraded_code(self) -> Option<&'static str> {
        match self {
            Self::Current => None,
            Self::Changed => Some("memory_drift_source_changed"),
            Self::MissingSource => Some("memory_drift_source_missing"),
            Self::Unverifiable => Some("memory_drift_source_unverifiable"),
        }
    }

    #[must_use]
    pub const fn report_severity(self) -> &'static str {
        match self {
            Self::Current => "info",
            Self::Unverifiable => "medium",
            Self::Changed | Self::MissingSource => "high",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDriftSelectionHint {
    pub memory_id: String,
    pub drift_status: MemoryDriftStatus,
    pub top_reason: String,
    pub evidence_count: u32,
    pub revalidation_command: String,
    pub degraded_code: Option<String>,
    pub severity: String,
}

impl MemoryDriftSelectionHint {
    #[must_use]
    pub fn new(
        memory_id: &str,
        drift_status: MemoryDriftStatus,
        top_reason: &str,
        evidence_count: u32,
    ) -> Self {
        let memory_id = normalized_non_empty(memory_id).unwrap_or_else(|| "unknown".to_owned());
        Self {
            revalidation_command: format!("ee memory drift {memory_id} --json"),
            memory_id,
            drift_status,
            top_reason: normalized_non_empty(top_reason)
                .unwrap_or_else(|| drift_status.default_reason().to_owned()),
            evidence_count,
            degraded_code: drift_status.degraded_code().map(str::to_owned),
            severity: drift_status.report_severity().to_owned(),
        }
    }

    #[must_use]
    pub fn compact_json(&self) -> serde_json::Value {
        serde_json::json!({
            "driftStatus": self.drift_status.as_str(),
            "topReason": &self.top_reason,
            "evidenceCount": self.evidence_count,
            "revalidationCommand": &self.revalidation_command,
        })
    }
}

impl MemoryDriftStatus {
    const fn default_reason(self) -> &'static str {
        match self {
            Self::Current => "provenance_chain_verified",
            Self::Changed => "provenance_chain_mismatch",
            Self::MissingSource => "provenance_chain_missing",
            Self::Unverifiable => "provenance_not_yet_verified",
        }
    }
}

#[must_use]
pub fn memory_drift_report_hint_from_provenance_status(
    memory_id: &str,
    provenance_verification_status: &str,
    provenance_chain_hash: Option<&str>,
) -> MemoryDriftSelectionHint {
    let (status, reason) = match provenance_verification_status.trim() {
        "verified" => (MemoryDriftStatus::Current, "provenance_chain_verified"),
        "mismatch" => (MemoryDriftStatus::Changed, "provenance_chain_mismatch"),
        "missing" => (MemoryDriftStatus::MissingSource, "provenance_chain_missing"),
        "skipped" => (
            MemoryDriftStatus::Unverifiable,
            "provenance_verification_skipped",
        ),
        "unverified" | "" => (
            MemoryDriftStatus::Unverifiable,
            "provenance_not_yet_verified",
        ),
        _ => (
            MemoryDriftStatus::Unverifiable,
            "provenance_verification_status_unknown",
        ),
    };
    let evidence_count = u32::from(
        provenance_chain_hash
            .map(str::trim)
            .is_some_and(|hash| !hash.is_empty()),
    );
    MemoryDriftSelectionHint::new(memory_id, status, reason, evidence_count)
}

#[must_use]
pub fn memory_drift_selection_hint_from_provenance_status(
    memory_id: &str,
    provenance_verification_status: &str,
    provenance_chain_hash: Option<&str>,
) -> Option<MemoryDriftSelectionHint> {
    match provenance_verification_status.trim() {
        "mismatch" | "missing" | "skipped" => {
            Some(memory_drift_report_hint_from_provenance_status(
                memory_id,
                provenance_verification_status,
                provenance_chain_hash,
            ))
        }
        _ => None,
    }
}

fn normalized_non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}
