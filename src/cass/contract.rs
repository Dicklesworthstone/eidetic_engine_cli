//! CASS contract preflight types (EE-100).
//!
//! `ee` must check the CASS API version, contract version, and a small
//! set of required capabilities before running import/search flows.
//! The full preflight implementation (which actually invokes
//! `cass api-version`, `cass capabilities`, and `cass introspect`) ships
//! in a follow-on bead; EE-100 only establishes the *types* and the comparison
//! semantics so future work has a stable surface to write against.
//!
//! This module deliberately avoids a production JSON parser — the
//! smallest coherent slice for "Add `cass` module" is to define the
//! shape, not to wire the parse.

use std::fmt;

use super::error::CassError;

/// Required `contract_version` for the CASS surface `ee` consumes.
///
/// The spike pinned this to `"1"` (see the EE-283 closure note); we
/// keep it as an associated constant so future contract bumps land via
/// a single edit and a failing test, never a silent drift.
pub const REQUIRED_CONTRACT_VERSION: &str = "1";

/// Minimum `api_version` `ee` knows how to talk to.
pub const REQUIRED_API_VERSION: u32 = 1;

/// Feature tokens from `cass capabilities --json` that `ee` will not
/// run without. Sourced from the spike's "Adapter Requirements For
/// `ee`" section, then checked against CASS's pinned capabilities
/// fixture. Order is alphabetical for stable diff output and
/// deterministic JSON serialisation.
pub const REQUIRED_CAPABILITIES: &[&str] = &[
    "api_version_command",
    "field_selection",
    "introspect_command",
    "json_output",
    "request_id",
    "robot_meta",
    "status_command",
    "view_command",
];

/// Bag of facts CASS reports about itself: its crate version, its API
/// version, its contract version, and the capabilities it advertises.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CassContract {
    crate_version: String,
    api_version: u32,
    contract_version: String,
    capabilities: Vec<String>,
}

impl CassContract {
    /// Build a contract record from raw fields, normalising
    /// capabilities by trimming whitespace, dropping empties, and
    /// sorting alphabetically (which matches both `ee`'s required-set
    /// ordering and CASS's published capabilities listing).
    #[must_use]
    pub fn new(
        crate_version: impl Into<String>,
        api_version: u32,
        contract_version: impl Into<String>,
        capabilities: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let mut capabilities: Vec<String> = capabilities
            .into_iter()
            .map(Into::into)
            .map(|cap| cap.trim().to_owned())
            .filter(|cap| !cap.is_empty())
            .collect();
        capabilities.sort();
        capabilities.dedup();
        Self {
            crate_version: crate_version.into(),
            api_version,
            contract_version: contract_version.into(),
            capabilities,
        }
    }

    /// Reported `crate_version` string (e.g. `"0.3.0"`).
    #[must_use]
    pub fn crate_version(&self) -> &str {
        self.crate_version.as_str()
    }

    /// Reported `api_version` integer (e.g. `1`).
    #[must_use]
    pub const fn api_version(&self) -> u32 {
        self.api_version
    }

    /// Reported `contract_version` string (e.g. `"1"`).
    #[must_use]
    pub fn contract_version(&self) -> &str {
        self.contract_version.as_str()
    }

    /// Capabilities CASS advertises, in stable sorted order.
    #[must_use]
    pub fn capabilities(&self) -> &[String] {
        self.capabilities.as_slice()
    }

    /// `true` iff the named capability is present.
    #[must_use]
    pub fn has_capability(&self, name: &str) -> bool {
        self.capabilities.iter().any(|cap| cap == name)
    }

    /// Return the capabilities from [`REQUIRED_CAPABILITIES`] that this
    /// contract does *not* advertise. Empty slice means "all required
    /// capabilities present"; callers can then drop straight into the
    /// preflight `Ok` arm.
    #[must_use]
    pub fn missing_required_capabilities(&self) -> Vec<&'static str> {
        REQUIRED_CAPABILITIES
            .iter()
            .copied()
            .filter(|name| !self.has_capability(name))
            .collect()
    }

    /// Verify this contract is compatible with the version `ee` was
    /// built against.
    ///
    /// # Errors
    ///
    /// Returns [`CassError::ContractMismatch`] when either
    /// `api_version` or `contract_version` differs from the pinned
    /// constants. Capability gaps are reported through
    /// [`Self::missing_required_capabilities`] (not as a hard failure)
    /// so degraded modes can surface the reason without aborting.
    pub fn ensure_compatible(&self) -> Result<(), CassError> {
        if self.api_version != REQUIRED_API_VERSION {
            return Err(CassError::ContractMismatch {
                required: format!("api_version={REQUIRED_API_VERSION}"),
                observed: format!("api_version={}", self.api_version),
            });
        }
        if self.contract_version != REQUIRED_CONTRACT_VERSION {
            return Err(CassError::ContractMismatch {
                required: format!("contract_version={REQUIRED_CONTRACT_VERSION}"),
                observed: format!("contract_version={}", self.contract_version),
            });
        }
        Ok(())
    }
}

impl fmt::Display for CassContract {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cass crate={} api={} contract={} capabilities={}",
            self.crate_version,
            self.api_version,
            self.contract_version,
            self.capabilities.join(",")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CassContract, REQUIRED_API_VERSION, REQUIRED_CAPABILITIES, REQUIRED_CONTRACT_VERSION,
    };

    const API_VERSION_FIXTURE: &str = include_str!("../../tests/fixtures/cass/api_version.v1.json");
    const CAPABILITIES_FIXTURE: &str =
        include_str!("../../tests/fixtures/cass/capabilities.v1.json");

    type TestResult = Result<(), String>;

    fn good_contract() -> CassContract {
        CassContract::new(
            "0.3.0",
            REQUIRED_API_VERSION,
            REQUIRED_CONTRACT_VERSION,
            REQUIRED_CAPABILITIES.iter().copied(),
        )
    }

    fn parse_fixture(source: &str, fixture_name: &str) -> Result<serde_json::Value, String> {
        serde_json::from_str(source)
            .map_err(|error| format!("failed to parse {fixture_name}: {error}"))
    }

    fn fixture_features(value: &serde_json::Value) -> Result<Vec<&str>, String> {
        let features = value
            .get("features")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "capabilities fixture must contain a features array".to_string())?;
        let mut result = Vec::with_capacity(features.len());
        for feature in features {
            match feature.as_str() {
                Some(feature) => result.push(feature),
                None => return Err("capabilities fixture feature must be a string".to_string()),
            }
        }
        Ok(result)
    }

    #[test]
    fn capability_list_is_sorted_and_deduped() {
        let contract = CassContract::new(
            "0.3.0",
            1,
            "1",
            ["search", "json_output", "search", "robot_meta"],
        );
        assert_eq!(
            contract.capabilities(),
            &[
                "json_output".to_owned(),
                "robot_meta".to_owned(),
                "search".to_owned()
            ]
        );
    }

    #[test]
    fn capability_list_drops_blanks_and_trims() {
        let contract = CassContract::new("0.3.0", 1, "1", ["", "  ", "  search  "]);
        assert_eq!(contract.capabilities(), &["search".to_owned()]);
    }

    #[test]
    fn ensure_compatible_accepts_pinned_versions() -> TestResult {
        good_contract()
            .ensure_compatible()
            .map_err(|error| error.to_string())
    }

    #[test]
    fn ensure_compatible_rejects_api_mismatch() -> TestResult {
        let contract = CassContract::new("0.3.0", 2, REQUIRED_CONTRACT_VERSION, ["json_output"]);
        let error = match contract.ensure_compatible() {
            Ok(()) => return Err("api mismatch must fail".to_string()),
            Err(error) => error,
        };
        assert_eq!(error.kind_str(), "contract_mismatch");
        assert!(error.to_string().contains("api_version=1"));
        assert!(error.to_string().contains("api_version=2"));
        Ok(())
    }

    #[test]
    fn ensure_compatible_rejects_contract_version_mismatch() -> TestResult {
        let contract = CassContract::new("0.3.0", REQUIRED_API_VERSION, "2", ["json_output"]);
        let error = match contract.ensure_compatible() {
            Ok(()) => return Err("contract version mismatch must fail".to_string()),
            Err(error) => error,
        };
        assert_eq!(error.kind_str(), "contract_mismatch");
        assert!(error.to_string().contains("contract_version=1"));
        assert!(error.to_string().contains("contract_version=2"));
        Ok(())
    }

    #[test]
    fn missing_required_capabilities_reports_gaps_in_stable_order() {
        let contract = CassContract::new("0.3.0", 1, "1", ["json_output", "request_id"]);
        let missing = contract.missing_required_capabilities();
        // Stable: alphabetical from REQUIRED_CAPABILITIES, with our two
        // present caps removed.
        assert_eq!(
            missing,
            vec![
                "api_version_command",
                "field_selection",
                "introspect_command",
                "robot_meta",
                "status_command",
                "view_command",
            ],
        );
    }

    #[test]
    fn missing_required_capabilities_is_empty_when_all_present() {
        let contract = good_contract();
        assert!(contract.missing_required_capabilities().is_empty());
    }

    #[test]
    fn has_capability_is_a_membership_check() {
        let contract = good_contract();
        assert!(contract.has_capability("json_output"));
        assert!(!contract.has_capability("not_a_real_capability"));
    }

    #[test]
    fn display_is_round_trip_friendly_for_logs() {
        let contract = CassContract::new("0.3.0", 1, "1", ["json_output", "request_id"]);
        let rendered = contract.to_string();
        assert!(rendered.contains("crate=0.3.0"));
        assert!(rendered.contains("api=1"));
        assert!(rendered.contains("contract=1"));
        assert!(rendered.contains("json_output"));
        assert!(rendered.contains("request_id"));
    }

    #[test]
    fn pinned_api_version_fixture_matches_required_versions() -> TestResult {
        let value = parse_fixture(API_VERSION_FIXTURE, "api_version.v1.json")?;
        assert_eq!(
            value.get("api_version").and_then(serde_json::Value::as_u64),
            Some(u64::from(REQUIRED_API_VERSION)),
        );
        assert_eq!(
            value
                .get("contract_version")
                .and_then(serde_json::Value::as_str),
            Some(REQUIRED_CONTRACT_VERSION),
        );
        Ok(())
    }

    #[test]
    fn pinned_capabilities_fixture_satisfies_required_features() -> TestResult {
        let value = parse_fixture(CAPABILITIES_FIXTURE, "capabilities.v1.json")?;
        let crate_version = value
            .get("crate_version")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "capabilities fixture must contain crate_version".to_string())?;
        let api_version = value
            .get("api_version")
            .and_then(serde_json::Value::as_u64)
            .and_then(|version| u32::try_from(version).ok())
            .ok_or_else(|| "capabilities fixture must contain a u32 api_version".to_string())?;
        let contract_version = value
            .get("contract_version")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "capabilities fixture must contain contract_version".to_string())?;
        let features = fixture_features(&value)?;

        let contract = CassContract::new(crate_version, api_version, contract_version, features);
        contract
            .ensure_compatible()
            .map_err(|error| error.to_string())?;
        assert!(
            contract.missing_required_capabilities().is_empty(),
            "fixture missing required CASS features: {:?}",
            contract.missing_required_capabilities()
        );
        Ok(())
    }
}
