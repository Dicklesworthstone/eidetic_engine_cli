//! Provenance URIs (EE-071).
//!
//! Every memory, evidence span, rule, and pack item that leaves `ee`
//! through the public API carries a provenance URI back to its source.
//! The URI is the user-visible answer to "where did this come from?"
//! and must be both round-trippable through JSON and stable across
//! tool versions.
//!
//! Five schemes are recognised in v1:
//!
//! | Scheme            | Body shape                                          |
//! |-------------------|-----------------------------------------------------|
//! | `cass-session://` | `<session-id>` plus optional `#L<n>` or `#L<n>-<m>` |
//! | `file://`         | `<path>` plus optional `#L<n>` or `#L<n>-<m>`       |
//! | `ee-mem://`       | A full [`MemoryId`] string (`mem_<26-char>`)        |
//! | `https://` `http://` | Any RFC-3986 absolute URL with an authority    |
//! | `agent-mail://`   | `<thread-id>` plus optional `/<message-id>`         |
//!
//! The parser is strict about scheme syntax but does not perform deep
//! semantic validation of the body — the file path can be any
//! non-empty string, and the CASS session id is treated as opaque. The
//! [`MemoryId`] body for `ee-mem://` is fully validated by the typed
//! ID parser from EE-060 so a mistyped reference fails fast at the
//! provenance boundary, not later in the retrieval pipeline.
//!
//! Round-trip is the contract: `ProvenanceUri::from_str(s)?.to_string()
//! == canonical(s)`. Inputs that already match the canonical form are
//! preserved byte-for-byte; inputs that vary only in case or trailing
//! slashes are accepted on parse and emitted in canonical form on
//! display.

use std::fmt;
use std::str::FromStr;

use super::id::{MemoryId, ParseIdError};

/// Inclusive line range used by `cass-session://` and `file://` URIs.
///
/// `start` is one-based. `end`, when present, must be greater than or
/// equal to `start`. The renderer emits `#L<start>` for a single-line
/// span and `#L<start>-<end>` for a range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineSpan {
    pub start: u64,
    pub end: Option<u64>,
}

impl LineSpan {
    /// Construct a single-line span. `start` must be non-zero.
    ///
    /// # Errors
    ///
    /// Returns [`ProvenanceUriError::ZeroLineNumber`] if `start` is `0`.
    pub fn single(start: u64) -> Result<Self, ProvenanceUriError> {
        if start == 0 {
            return Err(ProvenanceUriError::ZeroLineNumber);
        }
        Ok(Self { start, end: None })
    }

    /// Construct a range. `start` and `end` must both be non-zero and
    /// `end >= start`.
    ///
    /// # Errors
    ///
    /// Returns [`ProvenanceUriError::ZeroLineNumber`] if either bound is
    /// `0`, and [`ProvenanceUriError::InvertedLineRange`] if `end <
    /// start`.
    pub fn range(start: u64, end: u64) -> Result<Self, ProvenanceUriError> {
        if start == 0 || end == 0 {
            return Err(ProvenanceUriError::ZeroLineNumber);
        }
        if end < start {
            return Err(ProvenanceUriError::InvertedLineRange { start, end });
        }
        Ok(Self {
            start,
            end: Some(end),
        })
    }

    /// Render as `#L<start>` or `#L<start>-<end>` without the `#`.
    fn fragment(&self) -> String {
        match self.end {
            Some(end) if end != self.start => format!("L{}-{}", self.start, end),
            _ => format!("L{}", self.start),
        }
    }
}

/// Provenance URI for any single source of memory evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProvenanceUri {
    /// CASS session reference. `session` is opaque (UUID, blake3 hex,
    /// or session path slug — the consumer decides).
    CassSession {
        session: String,
        span: Option<LineSpan>,
    },
    /// Local-file reference, optionally with a line span.
    File {
        path: String,
        span: Option<LineSpan>,
    },
    /// Reference to another memory in the same workspace.
    EeMemory(MemoryId),
    /// Web URL (http/https). `url` includes the scheme.
    Web { url: String },
    /// Reference to an MCP Agent Mail thread (and optional message).
    AgentMail {
        thread: String,
        message: Option<String>,
    },
}

impl ProvenanceUri {
    /// Stable scheme name as it appears in the canonical form.
    #[must_use]
    pub fn scheme(&self) -> &'static str {
        match self {
            Self::CassSession { .. } => "cass-session",
            Self::File { .. } => "file",
            Self::EeMemory(_) => "ee-mem",
            Self::Web { url } => {
                if url.starts_with("https://") {
                    "https"
                } else {
                    "http"
                }
            }
            Self::AgentMail { .. } => "agent-mail",
        }
    }
}

impl fmt::Display for ProvenanceUri {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CassSession { session, span } => {
                formatter.write_str("cass-session://")?;
                formatter.write_str(session)?;
                if let Some(span) = span {
                    formatter.write_str("#")?;
                    formatter.write_str(&span.fragment())?;
                }
                Ok(())
            }
            Self::File { path, span } => {
                formatter.write_str("file://")?;
                formatter.write_str(path)?;
                if let Some(span) = span {
                    formatter.write_str("#")?;
                    formatter.write_str(&span.fragment())?;
                }
                Ok(())
            }
            Self::EeMemory(id) => {
                formatter.write_str("ee-mem://")?;
                fmt::Display::fmt(id, formatter)
            }
            Self::Web { url } => formatter.write_str(url),
            Self::AgentMail { thread, message } => {
                formatter.write_str("agent-mail://")?;
                formatter.write_str(thread)?;
                if let Some(message) = message {
                    formatter.write_str("/")?;
                    formatter.write_str(message)?;
                }
                Ok(())
            }
        }
    }
}

impl FromStr for ProvenanceUri {
    type Err = ProvenanceUriError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(ProvenanceUriError::Empty);
        }
        let lower_scheme_end = match trimmed.find("://") {
            Some(index) => index,
            None => {
                return Err(ProvenanceUriError::MissingScheme {
                    input: input.to_owned(),
                });
            }
        };
        let scheme = &trimmed[..lower_scheme_end];
        let body = &trimmed[lower_scheme_end + "://".len()..];
        match scheme {
            "cass-session" => parse_cass_session(input, body),
            "file" => parse_file(input, body),
            "ee-mem" => parse_ee_memory(input, body),
            "http" | "https" => parse_web(input, scheme, body),
            "agent-mail" => parse_agent_mail(input, body),
            other => Err(ProvenanceUriError::UnknownScheme {
                input: input.to_owned(),
                scheme: other.to_owned(),
            }),
        }
    }
}

fn parse_cass_session(input: &str, body: &str) -> Result<ProvenanceUri, ProvenanceUriError> {
    if body.is_empty() {
        return Err(ProvenanceUriError::EmptyBody {
            input: input.to_owned(),
            scheme: "cass-session",
        });
    }
    let (session, span) = split_fragment(input, body)?;
    Ok(ProvenanceUri::CassSession {
        session: session.to_owned(),
        span,
    })
}

fn parse_file(input: &str, body: &str) -> Result<ProvenanceUri, ProvenanceUriError> {
    if body.is_empty() {
        return Err(ProvenanceUriError::EmptyBody {
            input: input.to_owned(),
            scheme: "file",
        });
    }
    let (path, span) = split_fragment(input, body)?;
    Ok(ProvenanceUri::File {
        path: path.to_owned(),
        span,
    })
}

fn parse_ee_memory(input: &str, body: &str) -> Result<ProvenanceUri, ProvenanceUriError> {
    if body.is_empty() {
        return Err(ProvenanceUriError::EmptyBody {
            input: input.to_owned(),
            scheme: "ee-mem",
        });
    }
    let id = MemoryId::from_str(body).map_err(|source| ProvenanceUriError::InvalidMemoryId {
        input: input.to_owned(),
        source,
    })?;
    Ok(ProvenanceUri::EeMemory(id))
}

fn parse_web(input: &str, scheme: &str, body: &str) -> Result<ProvenanceUri, ProvenanceUriError> {
    if body.is_empty() {
        return Err(ProvenanceUriError::EmptyBody {
            input: input.to_owned(),
            scheme: if scheme == "https" { "https" } else { "http" },
        });
    }
    if !body.contains(|character: char| !character.is_whitespace()) {
        return Err(ProvenanceUriError::EmptyBody {
            input: input.to_owned(),
            scheme: if scheme == "https" { "https" } else { "http" },
        });
    }
    if body.contains(|character: char| character.is_ascii_control() || character == ' ') {
        return Err(ProvenanceUriError::InvalidWebBody {
            input: input.to_owned(),
        });
    }
    Ok(ProvenanceUri::Web {
        url: format!("{scheme}://{body}"),
    })
}

fn parse_agent_mail(input: &str, body: &str) -> Result<ProvenanceUri, ProvenanceUriError> {
    if body.is_empty() {
        return Err(ProvenanceUriError::EmptyBody {
            input: input.to_owned(),
            scheme: "agent-mail",
        });
    }
    if let Some((thread, message)) = body.split_once('/') {
        if thread.is_empty() || message.is_empty() {
            return Err(ProvenanceUriError::InvalidAgentMail {
                input: input.to_owned(),
            });
        }
        Ok(ProvenanceUri::AgentMail {
            thread: thread.to_owned(),
            message: Some(message.to_owned()),
        })
    } else {
        Ok(ProvenanceUri::AgentMail {
            thread: body.to_owned(),
            message: None,
        })
    }
}

/// Split a body into `(value, optional_line_span)` using `#` as the
/// fragment separator.
fn split_fragment<'a>(
    input: &str,
    body: &'a str,
) -> Result<(&'a str, Option<LineSpan>), ProvenanceUriError> {
    if let Some((value, fragment)) = body.split_once('#') {
        if value.is_empty() {
            return Err(ProvenanceUriError::EmptyBody {
                input: input.to_owned(),
                scheme: "file",
            });
        }
        let span = parse_line_fragment(input, fragment)?;
        Ok((value, Some(span)))
    } else {
        Ok((body, None))
    }
}

fn parse_line_fragment(input: &str, fragment: &str) -> Result<LineSpan, ProvenanceUriError> {
    let rest = match fragment.strip_prefix('L') {
        Some(value) => value,
        None => {
            return Err(ProvenanceUriError::InvalidFragment {
                input: input.to_owned(),
                fragment: fragment.to_owned(),
            });
        }
    };
    if let Some((start_text, end_text)) = rest.split_once('-') {
        let start = parse_line_number(input, start_text)?;
        let end = parse_line_number(input, end_text)?;
        LineSpan::range(start, end)
    } else {
        let start = parse_line_number(input, rest)?;
        LineSpan::single(start)
    }
}

fn parse_line_number(input: &str, text: &str) -> Result<u64, ProvenanceUriError> {
    if text.is_empty() {
        return Err(ProvenanceUriError::InvalidFragment {
            input: input.to_owned(),
            fragment: text.to_owned(),
        });
    }
    text.parse::<u64>()
        .map_err(|_| ProvenanceUriError::InvalidFragment {
            input: input.to_owned(),
            fragment: text.to_owned(),
        })
}

/// Errors produced by [`ProvenanceUri::from_str`] and the [`LineSpan`]
/// constructors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProvenanceUriError {
    Empty,
    MissingScheme { input: String },
    UnknownScheme { input: String, scheme: String },
    EmptyBody { input: String, scheme: &'static str },
    InvalidWebBody { input: String },
    InvalidAgentMail { input: String },
    InvalidFragment { input: String, fragment: String },
    ZeroLineNumber,
    InvertedLineRange { start: u64, end: u64 },
    InvalidMemoryId { input: String, source: ParseIdError },
}

impl fmt::Display for ProvenanceUriError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("provenance URI cannot be empty"),
            Self::MissingScheme { input } => write!(
                formatter,
                "provenance URI `{input}` is missing a `<scheme>://` prefix"
            ),
            Self::UnknownScheme { input, scheme } => write!(
                formatter,
                "unknown provenance scheme `{scheme}` in `{input}`; expected one of cass-session, file, ee-mem, http, https, agent-mail"
            ),
            Self::EmptyBody { input, scheme } => write!(
                formatter,
                "provenance URI `{input}` has an empty body for scheme `{scheme}`"
            ),
            Self::InvalidWebBody { input } => write!(
                formatter,
                "web provenance URI `{input}` contains whitespace or control characters"
            ),
            Self::InvalidAgentMail { input } => write!(
                formatter,
                "agent-mail URI `{input}` must be `agent-mail://<thread>` or `agent-mail://<thread>/<message>`"
            ),
            Self::InvalidFragment { input, fragment } => write!(
                formatter,
                "provenance URI `{input}` has an invalid line fragment `{fragment}`; expected `L<n>` or `L<n>-<m>`"
            ),
            Self::ZeroLineNumber => formatter.write_str("line numbers must be 1 or greater"),
            Self::InvertedLineRange { start, end } => write!(
                formatter,
                "line range {start}-{end} is inverted; end must be >= start"
            ),
            Self::InvalidMemoryId { input, source } => write!(
                formatter,
                "ee-mem URI `{input}` has an invalid memory id: {source}"
            ),
        }
    }
}

impl std::error::Error for ProvenanceUriError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidMemoryId { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::super::id::MemoryId;
    use super::{LineSpan, ProvenanceUri, ProvenanceUriError};

    fn must_parse(input: &str) -> ProvenanceUri {
        match ProvenanceUri::from_str(input) {
            Ok(value) => value,
            Err(error) => panic!("expected valid URI for `{input}`, got {error:?}"),
        }
    }

    fn must_fail(input: &str) -> ProvenanceUriError {
        match ProvenanceUri::from_str(input) {
            Ok(value) => panic!("expected error for `{input}`, got Ok({value:?})"),
            Err(error) => error,
        }
    }

    #[test]
    fn cass_session_round_trip_without_span() {
        let parsed = must_parse("cass-session://7f4ec0ff");
        match &parsed {
            ProvenanceUri::CassSession { session, span } => {
                assert_eq!(session, "7f4ec0ff");
                assert!(span.is_none());
            }
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(parsed.to_string(), "cass-session://7f4ec0ff");
    }

    #[test]
    fn cass_session_with_single_line_span() {
        let parsed = must_parse("cass-session://abc#L42");
        match &parsed {
            ProvenanceUri::CassSession { session, span } => {
                assert_eq!(session, "abc");
                assert_eq!(
                    *span,
                    Some(LineSpan {
                        start: 42,
                        end: None
                    })
                );
            }
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(parsed.to_string(), "cass-session://abc#L42");
    }

    #[test]
    fn cass_session_with_range_span() {
        let parsed = must_parse("cass-session://abc#L40-50");
        match &parsed {
            ProvenanceUri::CassSession { span, .. } => {
                assert_eq!(
                    *span,
                    Some(LineSpan {
                        start: 40,
                        end: Some(50)
                    })
                );
            }
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(parsed.to_string(), "cass-session://abc#L40-50");
    }

    #[test]
    fn file_with_path_and_span() {
        let parsed = must_parse("file:///etc/ee/config.toml#L7-12");
        match &parsed {
            ProvenanceUri::File { path, span } => {
                assert_eq!(path, "/etc/ee/config.toml");
                assert_eq!(
                    *span,
                    Some(LineSpan {
                        start: 7,
                        end: Some(12)
                    })
                );
            }
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(parsed.to_string(), "file:///etc/ee/config.toml#L7-12");
    }

    #[test]
    fn ee_memory_round_trips_through_full_id() {
        let id = MemoryId::from_uuid(uuid_with_seed(9));
        let rendered = format!("ee-mem://{id}");
        let parsed = must_parse(&rendered);
        match &parsed {
            ProvenanceUri::EeMemory(parsed_id) => assert_eq!(*parsed_id, id),
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(parsed.to_string(), rendered);
    }

    #[test]
    fn ee_memory_rejects_invalid_id() {
        let err = must_fail("ee-mem://not-a-real-id");
        assert!(matches!(err, ProvenanceUriError::InvalidMemoryId { .. }));
    }

    #[test]
    fn ee_memory_rejects_wrong_prefix() {
        // `wsp_<...>` parses as a WorkspaceId, not a MemoryId, so the
        // ee-mem URI must reject it via the MemoryId parser's typed
        // error.
        let bad = format!("ee-mem://{}", "wsp_00000000000000000000000000");
        let err = must_fail(&bad);
        match err {
            ProvenanceUriError::InvalidMemoryId { source, .. } => {
                let rendered = source.to_string();
                assert!(rendered.contains("expected `mem`"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn web_round_trip_for_https_and_http() {
        for url in [
            "https://example.com/",
            "http://example.com/path?x=1",
            "https://docs.example.com/page#section",
        ] {
            let parsed = must_parse(url);
            assert!(matches!(parsed, ProvenanceUri::Web { .. }));
            assert_eq!(parsed.to_string(), url);
        }
    }

    #[test]
    fn web_rejects_whitespace_or_controls() {
        for bad in ["https://exam ple.com", "http://example.com\u{0001}/x"] {
            let err = must_fail(bad);
            assert!(matches!(err, ProvenanceUriError::InvalidWebBody { .. }));
        }
    }

    #[test]
    fn agent_mail_thread_only_round_trips() {
        let parsed = must_parse("agent-mail://onboarding-2026-04-29");
        match &parsed {
            ProvenanceUri::AgentMail { thread, message } => {
                assert_eq!(thread, "onboarding-2026-04-29");
                assert!(message.is_none());
            }
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(parsed.to_string(), "agent-mail://onboarding-2026-04-29");
    }

    #[test]
    fn agent_mail_thread_and_message_round_trips() {
        let parsed = must_parse("agent-mail://br-123/msg-42");
        match &parsed {
            ProvenanceUri::AgentMail { thread, message } => {
                assert_eq!(thread, "br-123");
                assert_eq!(message.as_deref(), Some("msg-42"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(parsed.to_string(), "agent-mail://br-123/msg-42");
    }

    #[test]
    fn agent_mail_rejects_empty_thread_or_message() {
        for bad in ["agent-mail:///msg", "agent-mail://thread/"] {
            let err = must_fail(bad);
            assert!(matches!(err, ProvenanceUriError::InvalidAgentMail { .. }));
        }
    }

    #[test]
    fn empty_input_returns_typed_error() {
        for bad in ["", "   ", "\t\n"] {
            let err = must_fail(bad);
            assert!(matches!(err, ProvenanceUriError::Empty));
        }
    }

    #[test]
    fn missing_scheme_returns_typed_error() {
        let err = must_fail("/etc/ee/config.toml");
        assert!(matches!(err, ProvenanceUriError::MissingScheme { .. }));
    }

    #[test]
    fn unknown_scheme_returns_typed_error_with_scheme_name() {
        let err = must_fail("gopher://example.com/path");
        match err {
            ProvenanceUriError::UnknownScheme { scheme, .. } => assert_eq!(scheme, "gopher"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn empty_body_returns_typed_error_per_scheme() {
        for (input, expected_scheme) in [
            ("cass-session://", "cass-session"),
            ("file://", "file"),
            ("ee-mem://", "ee-mem"),
            ("https://", "https"),
            ("http://", "http"),
            ("agent-mail://", "agent-mail"),
        ] {
            let err = must_fail(input);
            match err {
                ProvenanceUriError::EmptyBody { scheme, .. } => assert_eq!(scheme, expected_scheme),
                other => panic!("wrong variant for `{input}`: {other:?}"),
            }
        }
    }

    #[test]
    fn invalid_line_fragment_returns_typed_error() {
        for bad in [
            "file:///x#42",  // missing leading L
            "file:///x#L",   // missing number
            "file:///x#L-",  // missing both numbers
            "file:///x#Lzz", // non-numeric
            "file:///x#L1-", // missing end
            "file:///x#L-5", // missing start
        ] {
            let err = must_fail(bad);
            assert!(
                matches!(err, ProvenanceUriError::InvalidFragment { .. }),
                "expected InvalidFragment for `{bad}`, got {err:?}"
            );
        }
    }

    #[test]
    fn line_span_constructors_reject_zero_and_inverted_ranges() {
        assert!(matches!(
            LineSpan::single(0),
            Err(ProvenanceUriError::ZeroLineNumber)
        ));
        assert!(matches!(
            LineSpan::range(0, 5),
            Err(ProvenanceUriError::ZeroLineNumber)
        ));
        assert!(matches!(
            LineSpan::range(10, 5),
            Err(ProvenanceUriError::InvertedLineRange { .. })
        ));
    }

    #[test]
    fn line_span_collapses_equal_endpoints_to_single_form() {
        let parsed = must_parse("file:///x#L7-7");
        assert_eq!(parsed.to_string(), "file:///x#L7");
    }

    #[test]
    fn input_is_trimmed_before_parsing() {
        let parsed = must_parse("  https://example.com/  ");
        assert_eq!(parsed.to_string(), "https://example.com/");
    }

    #[test]
    fn scheme_method_returns_canonical_name() {
        assert_eq!(must_parse("cass-session://x").scheme(), "cass-session");
        assert_eq!(must_parse("file:///x").scheme(), "file");
        assert_eq!(must_parse("https://example.com/").scheme(), "https");
        assert_eq!(must_parse("http://example.com/").scheme(), "http");
        assert_eq!(must_parse("agent-mail://thread").scheme(), "agent-mail");
    }

    fn uuid_with_seed(seed: u8) -> uuid::Uuid {
        let mut bytes = [0u8; 16];
        bytes[0] = seed;
        bytes[1] = 0x77;
        bytes[6] = 0x70 | (bytes[6] & 0x0F);
        bytes[8] = 0x80 | (bytes[8] & 0x3F);
        uuid::Uuid::from_bytes(bytes)
    }
}
