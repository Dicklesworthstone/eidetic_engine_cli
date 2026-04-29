//! CASS adapter error taxonomy (EE-100).
//!
//! `ee` consumes [`coding_agent_session_search`](https://github.com/Dicklesworthstone/coding_agent_session_search)
//! through its robot/JSON CLI. The integration spike
//! ([`docs/spikes/cass-json-contract-stability.md`]) established two
//! constraints on the error model:
//!
//! 1. Branch on stderr `error.kind`, **not** numeric exit code.
//!    `cass health --json` can exit `1` with valid JSON on stdout and a
//!    JSON error envelope on stderr; treating exit-code as the only
//!    failure signal would discard usable provenance.
//! 2. Preserve **degraded** mode as evidence. A stale-index search can
//!    exit `0` with valid JSON and a warning on stderr — that warning
//!    must reach `ee status` and provenance footers, not be flattened
//!    into a hard failure.
//!
//! [`CassError`] is the public taxonomy `ee` exposes; it never carries
//! the raw stderr blob (provenance is captured elsewhere) and is
//! deliberately small so consumers can pattern-match exhaustively.

use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

/// Stable categorical taxonomy for CASS adapter failures.
///
/// `kind` strings come from the CASS error envelope when available; the
/// adapter normalises the well-known set into the variants below.
/// Anything else is reported as [`CassError::Unknown`] with the raw
/// `kind` preserved so debugging is not blocked by an unmodelled
/// envelope value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CassError {
    /// The configured binary path is outside the EE-100 allowlist.
    /// Explicit binary discovery and path validation land in EE-101;
    /// until then, the subprocess layer only spawns the fixed `cass`
    /// executable name and treats all other paths as configuration
    /// errors.
    InvalidBinary { binary: PathBuf, reason: String },

    /// The configured `cass` binary could not be located on `$PATH` or
    /// at the explicit override path.
    BinaryNotFound { binary: PathBuf },

    /// Spawning or communicating with the `cass` subprocess failed at
    /// the OS level (e.g. permission denied, broken pipe).
    Io { message: String },

    /// `cass` produced no machine-data on stdout where it was required.
    ///
    /// This is distinct from [`CassError::InvalidStdoutJson`]: empty
    /// stdout often means the process was still in a failing degraded
    /// mode and emitted the entire payload on stderr.
    EmptyStdout,

    /// stdout was non-empty but failed JSON validation.
    ///
    /// `ee` does not parse JSON in this slice; the variant exists so
    /// the higher-level adapter (the bead following EE-100) can route a
    /// parse failure through the stable error contract from day one.
    InvalidStdoutJson { hint: String },

    /// `cass` reported a contract or version mismatch (e.g. our
    /// required `contract_version` differs from the running binary).
    ContractMismatch { required: String, observed: String },

    /// `cass` reported a degraded capability (stale lexical index,
    /// missing semantic asset, etc.). The payload is still usable — the
    /// caller decides whether to surface or hide the warning.
    Degraded { kind: String, repair_hint: String },

    /// `cass` reported a runtime failure with a known `error.kind`.
    /// This is the catch-all for nonzero exits whose stderr envelope
    /// belongs to the known taxonomy but isn't degraded.
    Runtime { kind: String, message: String },

    /// `cass` reported an `error.kind` we don't model yet. The raw
    /// `kind` is preserved so callers can switch on it textually
    /// without losing information.
    Unknown { kind: String, message: String },
}

impl CassError {
    /// Return the stable lowercase kind string used in JSON error
    /// envelopes and `ee status` degradation codes.
    #[must_use]
    pub fn kind_str(&self) -> &str {
        match self {
            Self::InvalidBinary { .. } => "invalid_binary",
            Self::BinaryNotFound { .. } => "binary_not_found",
            Self::Io { .. } => "io",
            Self::EmptyStdout => "empty_stdout",
            Self::InvalidStdoutJson { .. } => "invalid_stdout_json",
            Self::ContractMismatch { .. } => "contract_mismatch",
            Self::Degraded { .. } => "degraded",
            Self::Runtime { kind, .. } | Self::Unknown { kind, .. } => kind.as_str(),
        }
    }

    /// True for variants that still expose a usable payload to the
    /// caller (currently only [`Self::Degraded`]). The retrieval path
    /// uses this to decide whether to keep degraded results or fail
    /// closed.
    #[must_use]
    pub const fn is_degraded(&self) -> bool {
        matches!(self, Self::Degraded { .. })
    }

    /// Suggested repair instruction for the human/diagnostic surface.
    /// Returns `None` when no actionable repair is known yet (callers
    /// should fall back to "see ee doctor --json").
    #[must_use]
    pub fn repair_hint(&self) -> Option<&str> {
        match self {
            Self::InvalidBinary { .. } => {
                Some("use the allowlisted cass executable; explicit path discovery lands in EE-101")
            }
            Self::BinaryNotFound { .. } => Some("install cass or set [cass.binary] in config"),
            Self::ContractMismatch { .. } => Some("upgrade cass to a compatible contract version"),
            Self::Degraded { repair_hint, .. } => Some(repair_hint.as_str()),
            Self::EmptyStdout
            | Self::InvalidStdoutJson { .. }
            | Self::Io { .. }
            | Self::Runtime { .. }
            | Self::Unknown { .. } => None,
        }
    }
}

impl fmt::Display for CassError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBinary { binary, reason } => {
                write!(
                    f,
                    "cass binary '{}' is not allowed: {reason}",
                    binary.display()
                )
            }
            Self::BinaryNotFound { binary } => {
                write!(f, "cass binary not found at '{}'", binary.display())
            }
            Self::Io { message } => write!(f, "cass subprocess io error: {message}"),
            Self::EmptyStdout => f.write_str("cass produced no stdout payload"),
            Self::InvalidStdoutJson { hint } => write!(f, "cass stdout was not valid JSON: {hint}"),
            Self::ContractMismatch { required, observed } => write!(
                f,
                "cass contract mismatch: required {required}, observed {observed}",
            ),
            Self::Degraded { kind, repair_hint } => {
                write!(
                    f,
                    "cass reports degraded capability '{kind}': {repair_hint}"
                )
            }
            Self::Runtime { kind, message } => write!(f, "cass runtime error '{kind}': {message}"),
            Self::Unknown { kind, message } => {
                write!(f, "cass reported unknown error kind '{kind}': {message}")
            }
        }
    }
}

impl Error for CassError {}

impl From<io::Error> for CassError {
    fn from(error: io::Error) -> Self {
        Self::Io {
            message: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CassError;

    use std::path::PathBuf;

    #[test]
    fn kind_strings_are_stable_identifiers() {
        let cases = [
            (
                CassError::InvalidBinary {
                    binary: PathBuf::from("/tmp/cass"),
                    reason: "outside allowlist".into(),
                },
                "invalid_binary",
            ),
            (
                CassError::BinaryNotFound {
                    binary: PathBuf::from("cass"),
                },
                "binary_not_found",
            ),
            (
                CassError::Io {
                    message: "broken pipe".into(),
                },
                "io",
            ),
            (CassError::EmptyStdout, "empty_stdout"),
            (
                CassError::InvalidStdoutJson {
                    hint: "expected '{'".into(),
                },
                "invalid_stdout_json",
            ),
            (
                CassError::ContractMismatch {
                    required: "1".into(),
                    observed: "2".into(),
                },
                "contract_mismatch",
            ),
            (
                CassError::Degraded {
                    kind: "stale_index".into(),
                    repair_hint: "cass index --full".into(),
                },
                "degraded",
            ),
            (
                CassError::Runtime {
                    kind: "session_not_found".into(),
                    message: "no such id".into(),
                },
                "session_not_found",
            ),
            (
                CassError::Unknown {
                    kind: "future_kind".into(),
                    message: "unmapped".into(),
                },
                "future_kind",
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(error.kind_str(), expected, "kind for {error:?}");
        }
    }

    #[test]
    fn degraded_is_the_only_recoverable_variant() {
        assert!(
            CassError::Degraded {
                kind: "stale".into(),
                repair_hint: "rebuild".into(),
            }
            .is_degraded()
        );
        assert!(!CassError::EmptyStdout.is_degraded());
        assert!(
            !CassError::Runtime {
                kind: "x".into(),
                message: "y".into(),
            }
            .is_degraded()
        );
    }

    #[test]
    fn repair_hints_are_present_for_actionable_variants() {
        let invalid_binary = CassError::InvalidBinary {
            binary: PathBuf::from("/tmp/cass"),
            reason: "outside allowlist".into(),
        };
        assert!(invalid_binary.repair_hint().is_some());

        let binary_missing = CassError::BinaryNotFound {
            binary: PathBuf::from("cass"),
        };
        assert!(binary_missing.repair_hint().is_some());

        let mismatch = CassError::ContractMismatch {
            required: "1".into(),
            observed: "2".into(),
        };
        assert!(mismatch.repair_hint().is_some());

        let degraded = CassError::Degraded {
            kind: "k".into(),
            repair_hint: "fix it".into(),
        };
        assert_eq!(degraded.repair_hint(), Some("fix it"));

        let opaque = CassError::Runtime {
            kind: "x".into(),
            message: "y".into(),
        };
        assert_eq!(opaque.repair_hint(), None);
    }

    #[test]
    fn display_includes_kind_and_context() {
        let error = CassError::Degraded {
            kind: "stale_lexical".into(),
            repair_hint: "ee index rebuild".into(),
        };
        let rendered = error.to_string();
        assert!(rendered.contains("stale_lexical"), "{rendered}");
        assert!(rendered.contains("ee index rebuild"), "{rendered}");
    }

    #[test]
    fn io_error_round_trips_through_from() {
        let raw = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let cass: CassError = raw.into();
        assert_eq!(cass.kind_str(), "io");
        assert!(cass.to_string().contains("denied"));
    }
}
