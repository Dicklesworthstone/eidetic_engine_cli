//! Deterministic path expansion (EE-020).
//!
//! `ee` resolves user-facing paths from config files, CLI flags, and
//! environment variables to concrete filesystem locations before any
//! durable operation runs. The expansion rules are intentionally minimal:
//!
//! * `~` and `~/...` expand to the user's home directory (`HOME` on Unix,
//!   `USERPROFILE` on Windows). A literal `~` not followed by `/` (or a
//!   path separator on Windows) is left untouched so usernames like
//!   `~root` or filenames containing a tilde are preserved.
//! * `$NAME` and `${NAME}` expand to the named environment variable. The
//!   variable must be `[A-Za-z_][A-Za-z0-9_]*`; an unrecognized form is
//!   left literal so accidental shell-script-style expansion does not bite
//!   filenames such as `${weird}.txt` when no `weird` is set — the caller
//!   gets a deterministic [`PathExpansionError::MissingEnvVar`] instead.
//! * Bare relative paths and absolute paths pass through unchanged at the
//!   lexical level. Workspace-relative resolution lives in a higher
//!   layer; this module is purely lexical.
//!
//! The expander is constructed with an explicit environment ([
//! `PathExpander::with_env`]) for deterministic tests, or pulled from the
//! current process environment via [`PathExpander::from_process_env`].
//!
//! Errors are typed and never panic. Every failure mode names the
//! offending input and includes a one-line repair hint where applicable.
//! No filesystem I/O is performed; canonicalization is the caller's job.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

/// Errors produced by [`PathExpander::expand`].
///
/// The variants intentionally carry the offending input so the caller can
/// embed them in user-facing diagnostics without re-quoting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PathExpansionError {
    /// `~` or `~/...` was used but the home directory is unknown.
    MissingHomeDir { input: String },
    /// `$NAME` or `${NAME}` referenced a variable that is not set.
    MissingEnvVar { input: String, name: String },
    /// `${...` opened a brace expansion without closing it.
    UnclosedBrace { input: String },
    /// `${}` or `$` immediately followed by an invalid character.
    EmptyVariable { input: String, position: usize },
    /// `${NAME` contained characters outside `[A-Za-z0-9_]`.
    InvalidVariableName { input: String, name_so_far: String },
}

impl std::fmt::Display for PathExpansionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingHomeDir { input } => write!(
                formatter,
                "cannot expand `~` in `{input}`: HOME (or USERPROFILE on Windows) is not set"
            ),
            Self::MissingEnvVar { input, name } => {
                write!(
                    formatter,
                    "environment variable `{name}` is not set (in `{input}`)"
                )
            }
            Self::UnclosedBrace { input } => write!(
                formatter,
                "unclosed `${{...}}` expansion in `{input}`: add a matching `}}`"
            ),
            Self::EmptyVariable { input, position } => write!(
                formatter,
                "empty variable name at position {position} in `{input}`"
            ),
            Self::InvalidVariableName { input, name_so_far } => write!(
                formatter,
                "variable name must match [A-Za-z_][A-Za-z0-9_]* but found `{name_so_far}` in `{input}`"
            ),
        }
    }
}

impl std::error::Error for PathExpansionError {}

/// Expands user-facing paths into concrete [`PathBuf`] values.
///
/// Construction is explicit so unit tests can pin the home dir and
/// environment without touching the process environment. The expander is
/// `Clone` and cheap to copy.
#[derive(Clone, Debug, Default)]
pub struct PathExpander {
    home: Option<PathBuf>,
    env: BTreeMap<String, OsString>,
}

impl PathExpander {
    /// Build an expander with an explicit home directory and environment.
    ///
    /// `env` keys are matched case-sensitively on every platform so tests
    /// stay deterministic regardless of host. (On real Windows process
    /// environments, callers should pre-uppercase keys before insertion.)
    #[must_use]
    pub fn with_env(home: Option<PathBuf>, env: BTreeMap<String, OsString>) -> Self {
        Self { home, env }
    }

    /// Build an expander from the current process environment.
    ///
    /// `HOME` is consulted on Unix; `USERPROFILE` on Windows. `env::vars`
    /// captures every variable visible to the process at call time. The
    /// returned expander is a snapshot; later mutations to the process
    /// environment are not visible through this instance.
    #[must_use]
    pub fn from_process_env() -> Self {
        let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let home = std::env::var_os(home_var).map(PathBuf::from);
        let mut env = BTreeMap::new();
        for (key, value) in std::env::vars_os() {
            if let Some(key_str) = key.to_str() {
                env.insert(key_str.to_owned(), value);
            }
        }
        Self { home, env }
    }

    /// Expand `input` lexically. Does not touch the filesystem.
    ///
    /// # Errors
    ///
    /// Returns [`PathExpansionError`] for any of: missing home directory
    /// when `~` is used, missing environment variable, unclosed `${...}`,
    /// empty variable, invalid variable name.
    pub fn expand(&self, input: &str) -> Result<PathBuf, PathExpansionError> {
        let after_tilde = self.expand_tilde(input)?;
        let expanded = self.expand_env(input, &after_tilde)?;
        Ok(PathBuf::from(expanded))
    }

    fn expand_tilde(&self, input: &str) -> Result<String, PathExpansionError> {
        if let Some(rest) = input.strip_prefix('~') {
            if rest.is_empty() || rest.starts_with('/') || (cfg!(windows) && rest.starts_with('\\'))
            {
                let home = match self.home.as_ref() {
                    Some(value) => value,
                    None => {
                        return Err(PathExpansionError::MissingHomeDir {
                            input: input.to_owned(),
                        });
                    }
                };
                let mut joined = home.to_string_lossy().into_owned();
                joined.push_str(rest);
                return Ok(joined);
            }
        }
        Ok(input.to_owned())
    }

    fn expand_env(&self, input: &str, source: &str) -> Result<String, PathExpansionError> {
        // Iterate by byte for `$`/`{`/`}` detection (all ASCII) but copy
        // non-dollar runs as borrowed `&str` slices so multi-byte UTF-8
        // codepoints in the input are never split apart.
        let bytes = source.as_bytes();
        let mut out = String::with_capacity(source.len());
        let mut index = 0;
        let mut copy_from = 0;
        while index < bytes.len() {
            let byte = bytes[index];
            if byte != b'$' {
                index += 1;
                continue;
            }
            out.push_str(&source[copy_from..index]);
            if index + 1 >= bytes.len() {
                return Err(PathExpansionError::EmptyVariable {
                    input: input.to_owned(),
                    position: index,
                });
            }
            let next = bytes[index + 1];
            if next == b'{' {
                let name_start = index + 2;
                let close = match find_byte(bytes, b'}', name_start) {
                    Some(value) => value,
                    None => {
                        return Err(PathExpansionError::UnclosedBrace {
                            input: input.to_owned(),
                        });
                    }
                };
                let name = &source[name_start..close];
                if name.is_empty() {
                    return Err(PathExpansionError::EmptyVariable {
                        input: input.to_owned(),
                        position: index,
                    });
                }
                if !is_valid_identifier(name) {
                    return Err(PathExpansionError::InvalidVariableName {
                        input: input.to_owned(),
                        name_so_far: name.to_owned(),
                    });
                }
                out.push_str(&self.lookup_env(input, name)?);
                index = close + 1;
                copy_from = index;
                continue;
            }
            if !is_identifier_start(next) {
                return Err(PathExpansionError::EmptyVariable {
                    input: input.to_owned(),
                    position: index,
                });
            }
            let start = index + 1;
            let mut end = start;
            while end < bytes.len() && is_identifier_continue(bytes[end]) {
                end += 1;
            }
            let name = &source[start..end];
            out.push_str(&self.lookup_env(input, name)?);
            index = end;
            copy_from = index;
        }
        out.push_str(&source[copy_from..]);
        Ok(out)
    }

    fn lookup_env(&self, input: &str, name: &str) -> Result<String, PathExpansionError> {
        match self.env.get(name) {
            Some(value) => Ok(value.to_string_lossy().into_owned()),
            None => Err(PathExpansionError::MissingEnvVar {
                input: input.to_owned(),
                name: name.to_owned(),
            }),
        }
    }
}

fn find_byte(bytes: &[u8], needle: u8, start: usize) -> Option<usize> {
    let mut index = start;
    while index < bytes.len() {
        if bytes[index] == needle {
            return Some(index);
        }
        index += 1;
    }
    None
}

const fn is_identifier_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

const fn is_identifier_continue(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric()
}

fn is_valid_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    match bytes.next() {
        Some(byte) if is_identifier_start(byte) => {}
        _ => return false,
    }
    bytes.all(is_identifier_continue)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::{PathExpander, PathExpansionError};

    fn fixed(home: Option<&str>, env: &[(&str, &str)]) -> PathExpander {
        let mut map = BTreeMap::new();
        for (key, value) in env {
            map.insert((*key).to_owned(), OsString::from(*value));
        }
        PathExpander::with_env(home.map(PathBuf::from), map)
    }

    fn must_err(result: Result<PathBuf, PathExpansionError>) -> PathExpansionError {
        match result {
            Ok(value) => panic!("expected Err, got Ok({value:?})"),
            Err(error) => error,
        }
    }

    #[test]
    fn tilde_alone_expands_to_home() {
        let expander = fixed(Some("/home/agent"), &[]);
        assert_eq!(expander.expand("~"), Ok(PathBuf::from("/home/agent")));
    }

    #[test]
    fn tilde_slash_expands_to_home_subpath() {
        let expander = fixed(Some("/home/agent"), &[]);
        assert_eq!(
            expander.expand("~/.local/share/ee/ee.db"),
            Ok(PathBuf::from("/home/agent/.local/share/ee/ee.db"))
        );
    }

    #[test]
    fn tilde_username_form_is_left_literal() {
        let expander = fixed(Some("/home/agent"), &[]);
        // We do not parse `~user` paths; preserve them verbatim so the
        // caller can decide whether they are valid filenames or errors.
        assert_eq!(expander.expand("~root"), Ok(PathBuf::from("~root")));
    }

    #[test]
    fn tilde_without_home_returns_typed_error() {
        let expander = fixed(None, &[]);
        let err = must_err(expander.expand("~/foo"));
        assert_eq!(
            err,
            PathExpansionError::MissingHomeDir {
                input: "~/foo".to_owned(),
            }
        );
        let rendered = err.to_string();
        assert!(rendered.contains("HOME"));
        assert!(rendered.contains("~/foo"));
    }

    #[test]
    fn dollar_brace_form_expands() {
        let expander = fixed(None, &[("EE_INDEX_DIR", "/var/lib/ee/indexes")]);
        assert_eq!(
            expander.expand("${EE_INDEX_DIR}/combined"),
            Ok(PathBuf::from("/var/lib/ee/indexes/combined"))
        );
    }

    #[test]
    fn dollar_bare_form_expands() {
        let expander = fixed(None, &[("FOO", "bar")]);
        assert_eq!(expander.expand("$FOO/quux"), Ok(PathBuf::from("bar/quux")));
    }

    #[test]
    fn multiple_expansions_in_one_path() {
        let expander = fixed(
            Some("/home/agent"),
            &[("EE_DB", "ee.db"), ("STAGE", "alpha")],
        );
        assert_eq!(
            expander.expand("~/.local/share/${STAGE}/$EE_DB"),
            Ok(PathBuf::from("/home/agent/.local/share/alpha/ee.db"))
        );
    }

    #[test]
    fn missing_env_var_returns_typed_error() {
        let expander = fixed(None, &[]);
        let err = must_err(expander.expand("$MISSING/x"));
        assert_eq!(
            err,
            PathExpansionError::MissingEnvVar {
                input: "$MISSING/x".to_owned(),
                name: "MISSING".to_owned(),
            }
        );
    }

    #[test]
    fn unclosed_brace_returns_typed_error() {
        let expander = fixed(None, &[("FOO", "bar")]);
        let err = must_err(expander.expand("${FOO/x"));
        assert_eq!(
            err,
            PathExpansionError::UnclosedBrace {
                input: "${FOO/x".to_owned(),
            }
        );
    }

    #[test]
    fn empty_brace_returns_typed_error() {
        let expander = fixed(None, &[]);
        let err = must_err(expander.expand("${}"));
        assert!(matches!(err, PathExpansionError::EmptyVariable { .. }));
    }

    #[test]
    fn dollar_at_end_returns_typed_error() {
        let expander = fixed(None, &[]);
        let err = must_err(expander.expand("path/$"));
        assert!(matches!(err, PathExpansionError::EmptyVariable { .. }));
    }

    #[test]
    fn dollar_followed_by_non_identifier_returns_typed_error() {
        let expander = fixed(None, &[]);
        let err = must_err(expander.expand("$/x"));
        assert!(matches!(err, PathExpansionError::EmptyVariable { .. }));
    }

    #[test]
    fn invalid_variable_name_in_braces_returns_typed_error() {
        let expander = fixed(None, &[]);
        let err = must_err(expander.expand("${1FOO}/x"));
        assert!(matches!(
            err,
            PathExpansionError::InvalidVariableName { .. }
        ));
    }

    #[test]
    fn absolute_path_passes_through() {
        let expander = fixed(Some("/home/agent"), &[]);
        assert_eq!(
            expander.expand("/etc/ee/config.toml"),
            Ok(PathBuf::from("/etc/ee/config.toml"))
        );
    }

    #[test]
    fn relative_path_passes_through() {
        let expander = fixed(Some("/home/agent"), &[]);
        assert_eq!(
            expander.expand("./relative/file"),
            Ok(PathBuf::from("./relative/file"))
        );
        assert_eq!(expander.expand("file"), Ok(PathBuf::from("file")));
    }

    #[test]
    fn empty_input_passes_through() {
        let expander = fixed(Some("/home/agent"), &[]);
        assert_eq!(expander.expand(""), Ok(PathBuf::new()));
    }

    #[test]
    fn deterministic_for_equal_input_and_env() {
        let env = [("FOO", "bar"), ("BAZ", "quux")];
        let first = fixed(Some("/home/agent"), &env);
        let second = fixed(Some("/home/agent"), &env);
        let input = "~/$FOO/${BAZ}/file.toml";
        assert_eq!(first.expand(input), second.expand(input));
    }

    #[test]
    fn env_var_with_unicode_value_round_trips() {
        let expander = fixed(None, &[("UNI", "α-β-γ")]);
        assert_eq!(expander.expand("$UNI/x"), Ok(PathBuf::from("α-β-γ/x")));
    }

    #[test]
    fn unicode_literal_segments_pass_through_without_corruption() {
        let expander = fixed(Some("/home/agent"), &[("X", "v")]);
        assert_eq!(
            expander.expand("/tmp/中文/$X/файл"),
            Ok(PathBuf::from("/tmp/中文/v/файл"))
        );
        assert_eq!(
            expander.expand("~/中文.toml"),
            Ok(PathBuf::from("/home/agent/中文.toml"))
        );
    }

    #[test]
    fn variable_name_terminates_at_non_identifier_byte() {
        let expander = fixed(None, &[("FOO", "bar")]);
        // `$FOO.toml` should expand `FOO` and leave `.toml` alone.
        assert_eq!(expander.expand("$FOO.toml"), Ok(PathBuf::from("bar.toml")));
    }

    #[test]
    fn underscore_starts_valid_identifier() {
        let expander = fixed(None, &[("_PRIVATE", "secret")]);
        assert_eq!(
            expander.expand("$_PRIVATE/path"),
            Ok(PathBuf::from("secret/path"))
        );
    }

    #[test]
    fn from_process_env_round_trips_existing_var() {
        // Use CARGO_PKG_NAME, which Cargo sets at compile time and which
        // is also visible at runtime when the test runs through `cargo
        // test`. This makes the assertion robust without mutating the
        // process environment.
        let expander = PathExpander::from_process_env();
        let raw = std::env::var("CARGO_PKG_NAME").unwrap_or_else(|_| String::new());
        if raw.is_empty() {
            return;
        }
        assert_eq!(
            expander.expand("$CARGO_PKG_NAME/foo"),
            Ok(PathBuf::from(format!("{raw}/foo")))
        );
    }
}
