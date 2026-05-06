pub const SUBSYSTEM: &str = "obs";

pub mod log_envelope;

pub use log_envelope::{
    AUDIT_EVENT_SCHEMA_V1, AuditEvent, AuditOutcome, LOG_ENVELOPE_SCHEMA_V1, LogEnvelope, LogLevel,
    now_rfc3339_nanos,
};

#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

#[cfg(test)]
mod tests {
    use super::subsystem_name;

    #[test]
    fn subsystem_name_is_stable() {
        assert_eq!(subsystem_name(), "obs");
    }
}
