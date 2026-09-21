//! Preserve validation failures without changing the stable fallback category.

use super::DaemonSearchFallbackReason;

impl DaemonSearchFallbackReason {
    // Search validation returns String; pack handoff decoding returns a serde
    // error. Keep both diagnostics instead of discarding either at map_err.
    fn search_response_drift(error: impl std::fmt::Display) -> Self {
        Self::SearchResponseValidationError(error.to_string())
    }
}

impl std::fmt::Display for DaemonSearchFallbackReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())?;
        if let Self::SearchResponseValidationError(detail) = self {
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
        let reason = Err::<(), _>(error.clone())
            .map_err(DaemonSearchFallbackReason::search_response_drift)
            .unwrap_err();
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
        let reason = DaemonSearchFallbackReason::search_response_drift(error);
        let degradation = daemon_search_fallback_degradation(reason);
        assert_eq!(
            degradation.message,
            format!(
                "Warm daemon search unavailable (search response drift: {error}); used canonical in-process search."
            )
        );
    }

    #[test]
    fn non_validation_fallback_messages_are_unchanged() {
        for reason in [
            DaemonSearchFallbackReason::SearchRoundTripFailed,
            DaemonSearchFallbackReason::SearchResponseDrift,
        ] {
            let expected = format!(
                "Warm daemon search unavailable ({}); used canonical in-process search.",
                reason.as_str()
            );
            assert_eq!(daemon_search_fallback_degradation(reason).message, expected);
        }
    }

    #[test]
    fn serde_decoder_errors_use_the_same_lossless_adapter() {
        let error = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let expected = error.to_string();
        let reason = Err::<(), _>(error)
            .map_err(DaemonSearchFallbackReason::search_response_drift)
            .unwrap_err();
        let degradation = daemon_search_fallback_degradation(reason);
        assert!(degradation.message.contains(&expected), "{}", degradation.message);
    }
}
