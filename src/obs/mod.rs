pub const SUBSYSTEM: &str = "obs";

pub mod audit_events;
pub mod log_envelope;
pub mod test_log;
pub mod volatile_fields;

pub use log_envelope::{
    AUDIT_EVENT_SCHEMA_V1, AuditEvent, AuditOutcome, LOG_ENVELOPE_SCHEMA_V1, LogEnvelope, LogLevel,
    now_rfc3339_nanos,
};
pub use test_log::{
    CommandRecorder, ENV_TEST_LOG_LEVEL, ENV_TEST_LOG_PATH, ENV_TEST_LOG_TEST_ID, EventKind,
    TEST_EVENT_SCHEMA_V1, TestEvent, assert_fail, assert_ok, excerpt_stderr, golden_compare,
    hash_bytes, log_event, log_level, log_path, note, test_id_or,
};
pub use volatile_fields::{
    VOLATILE_FIELD_NAMES, VolatileStripReport, is_volatile_field_name, strip_volatile_fields,
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
