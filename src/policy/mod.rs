//! Policy subsystem (EE-278).
//!
//! Implements trust, privacy, and access control policies for memories
//! and import sources.

pub mod trust_decay;

pub use trust_decay::{DecayConfig, SourceTrustState, TrustAdvisory, TrustDecayCalculator};

pub const SUBSYSTEM: &str = "policy";

#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

#[cfg(test)]
mod tests {
    use super::subsystem_name;

    #[test]
    fn subsystem_name_is_stable() {
        assert_eq!(subsystem_name(), "policy");
    }
}
