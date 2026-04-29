//! Memory domain validation (EE-061).
//!
//! Defines the validated value types that every durable memory must
//! produce before it reaches the database layer:
//!
//! * [`MemoryLevel`] — the four-level taxonomy (`working`, `episodic`,
//!   `semantic`, `procedural`) used for scoring tilt and packing
//!   priority.
//! * [`MemoryKind`] — the known set of memory shapes (rule, fact,
//!   decision, failure, command, convention, anti-pattern, risk,
//!   playbook step) plus a [`MemoryKind::Custom`] escape hatch for
//!   project-specific extensions.
//! * [`Tag`] — a normalized keyword that survives JSON round-trips.
//! * [`MemoryContent`] — a non-empty, length-bounded body string.
//! * [`Confidence`], [`Utility`], [`Importance`] — bounded `f32`
//!   newtypes in the unit interval `0.0..=1.0`.
//!
//! Validation never panics. Every entry point returns a typed
//! [`MemoryValidationError`] that names the offending field and value.
//! Numeric newtypes treat `NaN` and infinities as invalid; they only
//! accept finite values inside the unit interval.
//!
//! `MemoryLevel` and `MemoryKind` are stable on the wire — their
//! string forms are part of the `ee.response.v1` schema and must not
//! change without a contract bump. `Tag` lowercases incoming
//! identifiers so the canonical wire form matches the canonical
//! Rust form.

use std::fmt;
use std::str::FromStr;

/// Maximum number of bytes accepted for a single tag.
///
/// 64 bytes covers ULIDs, kebab-case slugs, and namespaced tags
/// (`security:auth-bypass`) without being so generous that storage
/// queries blow up on malformed input.
pub const MAX_TAG_BYTES: usize = 64;

/// Maximum number of UTF-8 bytes accepted for a memory body.
///
/// 64 KiB is well above any realistic single-memory size, but small
/// enough that pathological payloads (entire log files, dumps) get a
/// typed error before they reach the index queue.
pub const MAX_CONTENT_BYTES: usize = 64 * 1024;

/// Memory levels enumerated in scoring-tilt order from least to most
/// durable.
///
/// The string form is the lowercased variant name and is stable on the
/// wire. Any future addition is a schema-bump-level change.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MemoryLevel {
    Working,
    Episodic,
    Semantic,
    Procedural,
}

impl MemoryLevel {
    /// Stable lowercase wire form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Episodic => "episodic",
            Self::Semantic => "semantic",
            Self::Procedural => "procedural",
        }
    }

    /// All variants in a stable, schema-aligned order.
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [
            Self::Working,
            Self::Episodic,
            Self::Semantic,
            Self::Procedural,
        ]
    }
}

impl fmt::Display for MemoryLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for MemoryLevel {
    type Err = MemoryValidationError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "working" => Ok(Self::Working),
            "episodic" => Ok(Self::Episodic),
            "semantic" => Ok(Self::Semantic),
            "procedural" => Ok(Self::Procedural),
            _ => Err(MemoryValidationError::UnknownLevel {
                input: input.to_owned(),
            }),
        }
    }
}

/// Memory kinds. The first nine variants are the canonical README set;
/// [`MemoryKind::Custom`] preserves arbitrary project-specific
/// identifiers without losing them through round-trip.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum MemoryKind {
    Rule,
    Fact,
    Decision,
    Failure,
    Command,
    Convention,
    AntiPattern,
    Risk,
    PlaybookStep,
    Custom(String),
}

/// Names of the canonical kinds, in stable order. Useful for help text
/// and golden tests.
pub const KNOWN_MEMORY_KINDS: &[&str] = &[
    "rule",
    "fact",
    "decision",
    "failure",
    "command",
    "convention",
    "anti-pattern",
    "risk",
    "playbook-step",
];

impl MemoryKind {
    /// Stable lowercase wire form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Rule => "rule",
            Self::Fact => "fact",
            Self::Decision => "decision",
            Self::Failure => "failure",
            Self::Command => "command",
            Self::Convention => "convention",
            Self::AntiPattern => "anti-pattern",
            Self::Risk => "risk",
            Self::PlaybookStep => "playbook-step",
            Self::Custom(value) => value.as_str(),
        }
    }

    /// Returns `true` if `name` parses to a known kind (not [`Custom`]).
    #[must_use]
    pub fn is_known(name: &str) -> bool {
        KNOWN_MEMORY_KINDS.contains(&name)
    }
}

impl fmt::Display for MemoryKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for MemoryKind {
    type Err = MemoryValidationError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "rule" => Ok(Self::Rule),
            "fact" => Ok(Self::Fact),
            "decision" => Ok(Self::Decision),
            "failure" => Ok(Self::Failure),
            "command" => Ok(Self::Command),
            "convention" => Ok(Self::Convention),
            "anti-pattern" => Ok(Self::AntiPattern),
            "risk" => Ok(Self::Risk),
            "playbook-step" => Ok(Self::PlaybookStep),
            other => {
                if other.is_empty() {
                    return Err(MemoryValidationError::EmptyKind);
                }
                if !is_valid_kind_identifier(other) {
                    return Err(MemoryValidationError::InvalidKind {
                        input: other.to_owned(),
                    });
                }
                Ok(Self::Custom(other.to_owned()))
            }
        }
    }
}

/// Validated tag — lowercase, alphanumeric or `-`/`:`, 1–64 bytes.
///
/// Tags survive JSON round-trips byte-for-byte: a `Tag` parsed from
/// upper-case input emits its lower-case canonical form on display.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Tag(String);

impl Tag {
    /// Construct a tag from raw input, lowercasing it in-place.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryValidationError::EmptyTag`] for empty input,
    /// [`MemoryValidationError::TagTooLong`] when input exceeds
    /// [`MAX_TAG_BYTES`], and [`MemoryValidationError::InvalidTag`] when
    /// the canonicalized form contains characters outside
    /// `[a-z0-9:-]`.
    pub fn parse(input: &str) -> Result<Self, MemoryValidationError> {
        if input.is_empty() {
            return Err(MemoryValidationError::EmptyTag);
        }
        if input.len() > MAX_TAG_BYTES {
            return Err(MemoryValidationError::TagTooLong {
                input: input.to_owned(),
                limit: MAX_TAG_BYTES,
            });
        }
        let canonical = input.to_ascii_lowercase();
        if !canonical.bytes().all(is_valid_tag_byte) {
            return Err(MemoryValidationError::InvalidTag {
                input: input.to_owned(),
            });
        }
        Ok(Self(canonical))
    }

    /// Return the canonical lower-case form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for Tag {
    type Err = MemoryValidationError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::parse(input)
    }
}

/// Validated memory body. Non-empty, ≤ [`MAX_CONTENT_BYTES`].
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct MemoryContent(String);

impl MemoryContent {
    /// Construct after trimming surrounding ASCII whitespace.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryValidationError::EmptyContent`] if the trimmed
    /// body is empty, and [`MemoryValidationError::ContentTooLarge`] if
    /// the input exceeds [`MAX_CONTENT_BYTES`] before trimming.
    pub fn parse(input: &str) -> Result<Self, MemoryValidationError> {
        if input.len() > MAX_CONTENT_BYTES {
            return Err(MemoryValidationError::ContentTooLarge {
                bytes: input.len(),
                limit: MAX_CONTENT_BYTES,
            });
        }
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(MemoryValidationError::EmptyContent);
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// Borrow the canonical body text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Return the byte length of the canonical body.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.0.len()
    }
}

/// Bounded score in the unit interval `0.0..=1.0`.
///
/// Wraps an `f32`; the bound is enforced at construction time.
/// Equality and ordering reuse the underlying `f32` semantics with
/// `NaN` rejected at parse time so total ordering is safe.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct UnitScore(f32);

impl UnitScore {
    /// Try to wrap `value` if it is finite and in `0.0..=1.0`.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryValidationError::ScoreOutOfRange`] for `NaN`,
    /// infinities, or values outside the unit interval.
    pub fn parse(value: f32) -> Result<Self, MemoryValidationError> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(MemoryValidationError::ScoreOutOfRange { value });
        }
        Ok(Self(value))
    }

    /// Return the underlying `f32`.
    #[must_use]
    pub const fn into_inner(self) -> f32 {
        self.0
    }

    /// Return the lowest possible score (`0.0`).
    #[must_use]
    pub fn zero() -> Self {
        Self(0.0)
    }

    /// Return the default initial score for a freshly captured memory
    /// (`0.5`).
    #[must_use]
    pub fn neutral() -> Self {
        Self(0.5)
    }

    /// Return the maximum possible score (`1.0`).
    #[must_use]
    pub fn one() -> Self {
        Self(1.0)
    }
}

impl fmt::Display for UnitScore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:.4}", self.0)
    }
}

/// Confidence in a memory's correctness.
pub type Confidence = UnitScore;
/// Utility — how often a memory has helped agents.
pub type Utility = UnitScore;
/// Importance — operator-supplied salience boost.
pub type Importance = UnitScore;

/// Errors produced by any of the validators above.
///
/// Only `PartialEq` is derived because the [`ScoreOutOfRange`] variant
/// carries an `f32`. Comparisons against `NaN`-bearing instances do
/// not happen in practice — that path is explicitly tested below — but
/// formal `Eq` is intentionally not implied.
#[derive(Clone, Debug, PartialEq)]
pub enum MemoryValidationError {
    UnknownLevel { input: String },
    EmptyKind,
    InvalidKind { input: String },
    EmptyTag,
    TagTooLong { input: String, limit: usize },
    InvalidTag { input: String },
    EmptyContent,
    ContentTooLarge { bytes: usize, limit: usize },
    ScoreOutOfRange { value: f32 },
}

impl fmt::Display for MemoryValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownLevel { input } => write!(
                formatter,
                "unknown memory level `{input}`; expected one of working, episodic, semantic, procedural"
            ),
            Self::EmptyKind => formatter.write_str("memory kind cannot be empty"),
            Self::InvalidKind { input } => write!(
                formatter,
                "memory kind `{input}` must match [a-z][a-z0-9-]*"
            ),
            Self::EmptyTag => formatter.write_str("tag cannot be empty"),
            Self::TagTooLong { input, limit } => write!(
                formatter,
                "tag `{input}` is {} bytes; limit is {limit}",
                input.len()
            ),
            Self::InvalidTag { input } => write!(
                formatter,
                "tag `{input}` must contain only lowercase ASCII letters, digits, `-`, and `:`"
            ),
            Self::EmptyContent => formatter.write_str("memory content cannot be empty after trim"),
            Self::ContentTooLarge { bytes, limit } => write!(
                formatter,
                "memory content is {bytes} bytes; limit is {limit}"
            ),
            Self::ScoreOutOfRange { value } => write!(
                formatter,
                "score {value} is outside the unit interval [0.0, 1.0]"
            ),
        }
    }
}

impl std::error::Error for MemoryValidationError {}

const fn is_valid_tag_byte(byte: u8) -> bool {
    matches!(byte,
        b'a'..=b'z' | b'0'..=b'9' | b'-' | b':')
}

fn is_valid_kind_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    match bytes.next() {
        Some(b'a'..=b'z') => {}
        _ => return false,
    }
    bytes.all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{
        Confidence, KNOWN_MEMORY_KINDS, MAX_CONTENT_BYTES, MAX_TAG_BYTES, MemoryContent,
        MemoryKind, MemoryLevel, MemoryValidationError, Tag, UnitScore,
    };

    #[test]
    fn level_round_trip_for_every_variant() {
        for level in MemoryLevel::all() {
            let rendered = level.to_string();
            let parsed = match MemoryLevel::from_str(&rendered) {
                Ok(value) => value,
                Err(error) => panic!("level {level:?} failed to round-trip: {error:?}"),
            };
            assert_eq!(parsed, level);
        }
    }

    #[test]
    fn level_rejects_unknown_input() {
        let err = match MemoryLevel::from_str("Working") {
            Ok(value) => panic!("expected error, got Ok({value:?})"),
            Err(error) => error,
        };
        assert_eq!(
            err,
            MemoryValidationError::UnknownLevel {
                input: "Working".to_owned(),
            }
        );
    }

    #[test]
    fn kind_round_trip_for_every_known_variant() {
        for name in KNOWN_MEMORY_KINDS {
            let parsed = match MemoryKind::from_str(name) {
                Ok(value) => value,
                Err(error) => panic!("known kind `{name}` failed: {error:?}"),
            };
            assert_eq!(parsed.as_str(), *name);
            assert!(MemoryKind::is_known(name));
        }
    }

    #[test]
    fn kind_accepts_custom_identifier() {
        let parsed = match MemoryKind::from_str("project-rule") {
            Ok(value) => value,
            Err(error) => panic!("custom kind failed: {error:?}"),
        };
        assert!(matches!(parsed, MemoryKind::Custom(_)));
        assert_eq!(parsed.as_str(), "project-rule");
        assert!(!MemoryKind::is_known("project-rule"));
    }

    #[test]
    fn kind_rejects_empty_and_uppercase_and_leading_digit() {
        for input in ["", "Rule", "1rule", "rule!", "ru le", "-foo"] {
            let err = match MemoryKind::from_str(input) {
                Ok(value) => panic!("expected error for `{input}`, got Ok({value:?})"),
                Err(error) => error,
            };
            assert!(
                matches!(
                    err,
                    MemoryValidationError::EmptyKind | MemoryValidationError::InvalidKind { .. }
                ),
                "wrong variant for `{input}`: {err:?}"
            );
        }
    }

    #[test]
    fn tag_lowercases_and_validates() {
        let tag = match Tag::parse("Security:Auth-Bypass") {
            Ok(value) => value,
            Err(error) => panic!("valid tag rejected: {error:?}"),
        };
        assert_eq!(tag.as_str(), "security:auth-bypass");
        assert_eq!(tag.to_string(), "security:auth-bypass");
    }

    #[test]
    fn tag_rejects_empty_and_too_long_and_invalid_bytes() {
        match Tag::parse("") {
            Ok(_) => panic!("empty tag should fail"),
            Err(MemoryValidationError::EmptyTag) => {}
            Err(other) => panic!("wrong variant: {other:?}"),
        }
        let huge = "a".repeat(MAX_TAG_BYTES + 1);
        match Tag::parse(&huge) {
            Ok(_) => panic!("oversized tag should fail"),
            Err(MemoryValidationError::TagTooLong { limit, .. }) => {
                assert_eq!(limit, MAX_TAG_BYTES);
            }
            Err(other) => panic!("wrong variant: {other:?}"),
        }
        for bad in ["space tag", "under_score", "slash/path", "emoji-🎉"] {
            match Tag::parse(bad) {
                Ok(_) => panic!("invalid tag `{bad}` should fail"),
                Err(MemoryValidationError::InvalidTag { .. }) => {}
                Err(other) => panic!("wrong variant for `{bad}`: {other:?}"),
            }
        }
    }

    #[test]
    fn tag_ordering_is_by_canonical_form() {
        let upper = match Tag::parse("Z-tag") {
            Ok(value) => value,
            Err(error) => panic!("{error:?}"),
        };
        let lower = match Tag::parse("a-tag") {
            Ok(value) => value,
            Err(error) => panic!("{error:?}"),
        };
        let mut tags = [upper, lower];
        tags.sort();
        assert_eq!(tags[0].as_str(), "a-tag");
        assert_eq!(tags[1].as_str(), "z-tag");
    }

    #[test]
    fn content_trims_and_rejects_empty() {
        let content = match MemoryContent::parse("  hello world  \n") {
            Ok(value) => value,
            Err(error) => panic!("valid content rejected: {error:?}"),
        };
        assert_eq!(content.as_str(), "hello world");

        for input in ["", "    ", "\n\t  \r\n"] {
            match MemoryContent::parse(input) {
                Ok(_) => panic!("empty/whitespace content should fail for `{input}`"),
                Err(MemoryValidationError::EmptyContent) => {}
                Err(other) => panic!("wrong variant: {other:?}"),
            }
        }
    }

    #[test]
    fn content_rejects_oversized_input() {
        let huge = "x".repeat(MAX_CONTENT_BYTES + 1);
        match MemoryContent::parse(&huge) {
            Ok(_) => panic!("oversized content should fail"),
            Err(MemoryValidationError::ContentTooLarge { limit, .. }) => {
                assert_eq!(limit, MAX_CONTENT_BYTES);
            }
            Err(other) => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn content_byte_len_matches_canonical_form() {
        let content = match MemoryContent::parse("  abc  ") {
            Ok(value) => value,
            Err(error) => panic!("{error:?}"),
        };
        assert_eq!(content.byte_len(), 3);
    }

    #[test]
    fn unit_score_accepts_unit_interval_endpoints() {
        for value in [0.0_f32, 0.5, 1.0] {
            match UnitScore::parse(value) {
                Ok(score) => assert!((score.into_inner() - value).abs() < f32::EPSILON),
                Err(error) => panic!("{value} rejected: {error:?}"),
            }
        }
    }

    #[test]
    fn unit_score_rejects_non_finite_and_out_of_range() {
        for value in [
            -0.001_f32,
            1.001,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ] {
            match UnitScore::parse(value) {
                Ok(score) => panic!("{value} accepted: {score:?}"),
                Err(MemoryValidationError::ScoreOutOfRange { .. }) => {}
                Err(other) => panic!("wrong variant: {other:?}"),
            }
        }
    }

    #[test]
    fn unit_score_constants_are_in_range() {
        assert_eq!(UnitScore::zero().into_inner(), 0.0);
        assert_eq!(UnitScore::neutral().into_inner(), 0.5);
        assert_eq!(UnitScore::one().into_inner(), 1.0);
    }

    #[test]
    fn confidence_alias_matches_unit_score() {
        let confidence: Confidence = match Confidence::parse(0.7) {
            Ok(value) => value,
            Err(error) => panic!("{error:?}"),
        };
        assert_eq!(confidence.into_inner(), 0.7);
    }

    #[test]
    fn known_memory_kinds_constant_matches_enum_strings() {
        let from_enum = [
            MemoryKind::Rule,
            MemoryKind::Fact,
            MemoryKind::Decision,
            MemoryKind::Failure,
            MemoryKind::Command,
            MemoryKind::Convention,
            MemoryKind::AntiPattern,
            MemoryKind::Risk,
            MemoryKind::PlaybookStep,
        ];
        let from_enum: Vec<&str> = from_enum.iter().map(MemoryKind::as_str).collect();
        let from_const: Vec<&str> = KNOWN_MEMORY_KINDS.to_vec();
        assert_eq!(from_enum, from_const);
    }
}
