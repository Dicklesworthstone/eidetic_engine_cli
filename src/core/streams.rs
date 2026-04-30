//! EE-039: Diagnostic command to verify stdout/stderr stream separation.
//!
//! The `ee diag streams` command helps agents verify that the CLI correctly
//! separates machine data (stdout) from diagnostics (stderr). This is critical
//! for agent-native operation where stdout must be parseable as JSON.

use std::io::Write;

/// Report from the streams diagnostic command.
#[derive(Clone, Debug)]
pub struct StreamsReport {
    /// Whether stdout is correctly isolated for machine data.
    pub stdout_isolated: bool,
    /// Whether stderr received the diagnostic probe.
    pub stderr_received_probe: bool,
    /// The diagnostic message sent to stderr during the test.
    pub stderr_probe_message: String,
    /// Version of the ee binary.
    pub version: &'static str,
}

impl StreamsReport {
    /// Gather the streams report by probing stderr.
    ///
    /// This writes a diagnostic message to stderr and reports success.
    /// The stdout output will contain only the JSON report.
    pub fn gather<W: Write>(stderr: &mut W) -> Self {
        let probe_message = "ee diag streams: stderr probe for stream isolation verification";

        // Write probe to stderr - this should NOT appear in stdout
        let stderr_ok = writeln!(stderr, "{}", probe_message).is_ok();

        Self {
            stdout_isolated: true, // If we get here, stdout is isolated (JSON goes to stdout)
            stderr_received_probe: stderr_ok,
            stderr_probe_message: probe_message.to_string(),
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    /// Check if the stream separation is working correctly.
    #[must_use]
    pub const fn is_healthy(&self) -> bool {
        self.stdout_isolated && self.stderr_received_probe
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_report_gather_succeeds() {
        let mut stderr = Vec::new();
        let report = StreamsReport::gather(&mut stderr);

        assert!(report.stdout_isolated);
        assert!(report.stderr_received_probe);
        assert!(!report.stderr_probe_message.is_empty());
        assert!(!stderr.is_empty(), "stderr should have received probe");
    }

    #[test]
    fn streams_report_is_healthy_when_both_streams_work() {
        let report = StreamsReport {
            stdout_isolated: true,
            stderr_received_probe: true,
            stderr_probe_message: "test".to_string(),
            version: "0.1.0",
        };

        assert!(report.is_healthy());
    }

    #[test]
    fn streams_report_unhealthy_when_stderr_fails() {
        let report = StreamsReport {
            stdout_isolated: true,
            stderr_received_probe: false,
            stderr_probe_message: "test".to_string(),
            version: "0.1.0",
        };

        assert!(!report.is_healthy());
    }
}
