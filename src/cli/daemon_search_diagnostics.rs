//! Preserve validation failures without changing the stable fallback category.

use super::DaemonSearchFallbackReason;

impl std::fmt::Display for DaemonSearchFallbackReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())?;
        if let Self::SearchResponseDrift(detail) = self {
            write!(formatter, ": {detail}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::DaemonSearchFallbackReason;
    use crate::cli::daemon_search_fallback_degradation;
    use crate::daemon::server::DaemonSearchResult;

    #[test]
    fn real_decoder_error_survives_fallback_diagnosis() {
        let error = DaemonSearchResult::from_value(serde_json::Value::Null)
            .expect_err("a null reply cannot be a daemon search result");
        let reason = DaemonSearchFallbackReason::SearchResponseDrift(error.clone());
        assert_eq!(reason.as_str(), "search response drift");
        let degradation = daemon_search_fallback_degradation(reason);
        assert_eq!(degradation.code, "daemon_search_fallback");
        assert_eq!(degradation.severity, "warning");
        assert_eq!(degradation.repair.as_deref(), Some("ee daemon status --json"));
        assert!(degradation.message.contains(&error), "{}", degradation.message);
    }

    #[test]
    fn result_location_and_unknown_field_are_not_replaced_by_the_category() {
        let error = "canonical search result[2] contains unknown field `futureField`";
        let reason = DaemonSearchFallbackReason::SearchResponseDrift(error.to_owned());
        let degradation = daemon_search_fallback_degradation(reason);
        assert_eq!(
            degradation.message,
            format!(
                "Warm daemon search unavailable (search response drift: {error}); used canonical in-process search."
            )
        );
    }

    #[test]
    fn non_validation_fallback_message_is_unchanged() {
        let degradation = daemon_search_fallback_degradation(
            DaemonSearchFallbackReason::SearchRoundTripFailed,
        );
        assert_eq!(
            degradation.message,
            "Warm daemon search unavailable (search round-trip failed); used canonical in-process search."
        );
    }
}
