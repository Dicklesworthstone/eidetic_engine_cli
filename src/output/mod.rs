use crate::models::{CapabilityStatus, RESPONSE_SCHEMA_V1};

#[must_use]
pub fn status_response_json() -> String {
    let storage = CapabilityStatus::Unimplemented.as_str();
    let search = CapabilityStatus::Unimplemented.as_str();
    let runtime = CapabilityStatus::Pending.as_str();

    format!(
        "{{\"schema\":\"{schema}\",\"success\":true,\"data\":{{\"command\":\"status\",\"version\":\"{version}\",\"capabilities\":{{\"runtime\":\"{runtime}\",\"storage\":\"{storage}\",\"search\":\"{search}\"}},\"degraded\":[{{\"code\":\"storage_not_implemented\",\"severity\":\"medium\",\"message\":\"Storage subsystem is not wired yet.\",\"repair\":\"Implement EE-040 through EE-044.\"}},{{\"code\":\"search_not_implemented\",\"severity\":\"medium\",\"message\":\"Search subsystem is not wired yet.\",\"repair\":\"Implement EE-120 and dependent search beads.\"}}]}}}}",
        schema = RESPONSE_SCHEMA_V1,
        version = env!("CARGO_PKG_VERSION"),
        runtime = runtime,
        storage = storage,
        search = search
    )
}

#[must_use]
pub fn human_status() -> &'static str {
    "ee status\n\nstorage: unimplemented\nsearch: unimplemented\nruntime: pending\n\nNext:\n  ee status --json\n"
}

#[must_use]
pub fn help_text() -> &'static str {
    "ee - durable memory substrate for coding agents\n\nUsage:\n  ee status [--json]\n  ee --version\n  ee --help\n"
}

#[cfg(test)]
mod tests {
    use super::{help_text, human_status, status_response_json};

    #[test]
    fn status_json_has_stable_schema_and_degradation_codes() {
        let json = status_response_json();
        assert!(json.starts_with("{\"schema\":\"ee.response.v1\""));
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"storage_not_implemented\""));
        assert!(json.contains("\"search_not_implemented\""));
    }

    #[test]
    fn human_status_is_not_json() {
        let status = human_status();
        assert!(status.starts_with("ee status"));
        assert!(!status.starts_with('{'));
    }

    #[test]
    fn help_mentions_supported_skeleton_commands() {
        let help = help_text();
        assert!(help.contains("ee status [--json]"));
        assert!(help.contains("ee --version"));
    }
}
