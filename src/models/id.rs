//! Typed, time-ordered public identifiers (EE-060).
//!
//! Public IDs in `ee` carry a stable type tag and a 128-bit
//! time-ordered payload backed by UUID v7. The textual form is
//! `<prefix>_<26-char-base32>` so:
//!
//! * Lexicographic order of the string matches chronological order of
//!   generation. Sorting `mem_…` IDs gives oldest-first ordering without
//!   needing a separate timestamp column.
//! * The `<prefix>_` segment makes the kind unmistakable in JSON output,
//!   logs, and audit records.
//! * Strong typing at the Rust level prevents passing a [`MemoryId`]
//!   where a [`WorkspaceId`] is expected — the error is at compile
//!   time, not runtime.
//!
//! The payload encoding is Crockford Base32 with the canonical ULID
//! alphabet (`0-9`, `A-Z` minus `I L O U`). Two-bit zero padding sits in
//! front of the 128-bit value so the 26-character form decodes exactly
//! to the same 128 bits regardless of UUID layout.
//!
//! Generation uses `uuid::Uuid::now_v7`, which combines a 48-bit
//! millisecond Unix timestamp with 74 bits of cryptographic randomness
//! (the remaining bits are reserved for the v7 version + variant
//! markers). No persistent generator state is needed.
//!
//! Construction does not allocate. Comparison ignores the type tag's
//! marker type and only compares the underlying UUID, which is correct
//! because two IDs of different kinds can never share the same bytes
//! after parsing rejects the wrong prefix.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::str::FromStr;

use uuid::Uuid;

/// Canonical ULID/Crockford Base32 alphabet.
///
/// Excludes `I`, `L`, `O`, `U` to avoid confusion with `1`, `0`, and
/// vulgar substrings. Order matters: index `n` is the digit for value
/// `n`.
const CROCKFORD_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// 26 base-32 characters cover 130 bits, two of which are zero-padding
/// in front of the 128-bit UUID payload.
const ENCODED_LEN: usize = 26;

/// Trait identifying a domain-specific kind of typed ID.
///
/// Implementors are zero-sized markers. `'static` is required so the
/// associated `PREFIX` lives long enough for `Display` and `FromStr`.
pub trait IdKind: 'static {
    /// Stable, lowercase prefix (e.g. `"mem"` for [`MemoryId`]).
    const PREFIX: &'static str;
}

/// Schema version for executable claim/demo/policy/evidence/trace ID schemas.
pub const EXECUTABLE_ID_SCHEMA_V1: &str = "ee.executable_id_schemas.v1";

/// JSON Schema draft used for public ID schema fixtures.
pub const JSON_SCHEMA_DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

/// Canonical Crockford/ULID payload pattern used in public ID schemas.
pub const CANONICAL_ID_PAYLOAD_PATTERN: &str = "[0-7][0-9A-HJKMNP-TV-Z]{25}";

const CANONICAL_ID_EXAMPLE_PAYLOAD: &str = "00000000000000000000000000";

/// Executable-claim-plane ID kinds that downstream schemas can reference.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExecutableIdKind {
    /// Claim made by the project, docs, or product surface.
    Claim,
    /// Evidence artifact used to verify a claim.
    Evidence,
    /// Policy rule governing admission, execution, or verification.
    Policy,
    /// Trace of a decision, replay, or verification run.
    Trace,
    /// Demo scenario that should prove a claim in CI or locally.
    Demo,
}

impl ExecutableIdKind {
    /// Return executable ID kinds in the stable public schema order.
    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Claim,
            Self::Evidence,
            Self::Policy,
            Self::Trace,
            Self::Demo,
        ]
    }

    /// Stable snake-case kind name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claim => "claim",
            Self::Evidence => "evidence",
            Self::Policy => "policy",
            Self::Trace => "trace",
            Self::Demo => "demo",
        }
    }

    /// Rust type name exposed by this kind.
    #[must_use]
    pub const fn type_name(self) -> &'static str {
        match self {
            Self::Claim => "ClaimId",
            Self::Evidence => "EvidenceId",
            Self::Policy => "PolicyId",
            Self::Trace => "TraceId",
            Self::Demo => "DemoId",
        }
    }

    /// Stable ID prefix used in public wire forms.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Claim => ClaimKind::PREFIX,
            Self::Evidence => EvidenceKind::PREFIX,
            Self::Policy => PolicyKind::PREFIX,
            Self::Trace => TraceKind::PREFIX,
            Self::Demo => DemoKind::PREFIX,
        }
    }

    /// Stable schema name for this ID kind.
    #[must_use]
    pub const fn schema_name(self) -> &'static str {
        match self {
            Self::Claim => "ee.claim_id.v1",
            Self::Evidence => "ee.evidence_id.v1",
            Self::Policy => "ee.policy_id.v1",
            Self::Trace => "ee.trace_id.v1",
            Self::Demo => "ee.demo_id.v1",
        }
    }

    /// Stable JSON Schema `$id` for this ID kind.
    #[must_use]
    pub const fn schema_uri(self) -> &'static str {
        match self {
            Self::Claim => "urn:ee:schema:claim-id:v1",
            Self::Evidence => "urn:ee:schema:evidence-id:v1",
            Self::Policy => "urn:ee:schema:policy-id:v1",
            Self::Trace => "urn:ee:schema:trace-id:v1",
            Self::Demo => "urn:ee:schema:demo-id:v1",
        }
    }

    /// Human-readable schema description.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Claim => "Stable identifier for an executable claim.",
            Self::Evidence => "Stable identifier for evidence attached to a claim.",
            Self::Policy => "Stable identifier for a policy that affects executable verification.",
            Self::Trace => "Stable identifier for a verification, replay, or decision trace.",
            Self::Demo => "Stable identifier for a demo scenario that proves a claim.",
        }
    }

    /// Return the JSON schema metadata for this ID kind.
    #[must_use]
    pub const fn json_schema(self) -> IdJsonSchema {
        IdJsonSchema::new(self)
    }
}

impl fmt::Display for ExecutableIdKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ExecutableIdKind {
    type Err = ParseExecutableIdKindError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "claim" => Ok(Self::Claim),
            "evidence" => Ok(Self::Evidence),
            "policy" => Ok(Self::Policy),
            "trace" => Ok(Self::Trace),
            "demo" => Ok(Self::Demo),
            _ => Err(ParseExecutableIdKindError {
                input: input.to_owned(),
            }),
        }
    }
}

/// Error when parsing an invalid executable ID kind string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseExecutableIdKindError {
    input: String,
}

impl ParseExecutableIdKindError {
    /// Return the invalid input.
    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for ParseExecutableIdKindError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unknown executable id kind `{}`; expected one of claim, evidence, policy, trace, demo",
            self.input
        )
    }
}

impl std::error::Error for ParseExecutableIdKindError {}

/// JSON Schema metadata for one executable-claim-plane ID type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdJsonSchema {
    /// Discriminator for the represented ID kind.
    pub kind: ExecutableIdKind,
    /// Rust type name.
    pub type_name: &'static str,
    /// Stable schema name.
    pub schema_name: &'static str,
    /// Stable JSON Schema `$id`.
    pub schema_uri: &'static str,
    /// Public ID prefix.
    pub prefix: &'static str,
    /// Human-readable schema description.
    pub description: &'static str,
}

impl IdJsonSchema {
    /// Build metadata from an executable ID kind.
    #[must_use]
    pub const fn new(kind: ExecutableIdKind) -> Self {
        Self {
            kind,
            type_name: kind.type_name(),
            schema_name: kind.schema_name(),
            schema_uri: kind.schema_uri(),
            prefix: kind.prefix(),
            description: kind.description(),
        }
    }

    /// Regex pattern for the canonical public ID string.
    #[must_use]
    pub fn pattern(&self) -> String {
        format!("^{}_{CANONICAL_ID_PAYLOAD_PATTERN}$", self.prefix)
    }

    /// Deterministic example ID that satisfies this schema.
    #[must_use]
    pub fn example(&self) -> String {
        format!("{}_{}", self.prefix, CANONICAL_ID_EXAMPLE_PAYLOAD)
    }
}

/// Return executable ID schemas in the stable public order.
#[must_use]
pub const fn executable_id_schemas() -> [IdJsonSchema; 5] {
    [
        IdJsonSchema::new(ExecutableIdKind::Claim),
        IdJsonSchema::new(ExecutableIdKind::Evidence),
        IdJsonSchema::new(ExecutableIdKind::Policy),
        IdJsonSchema::new(ExecutableIdKind::Trace),
        IdJsonSchema::new(ExecutableIdKind::Demo),
    ]
}

/// Render the executable ID JSON Schema catalog.
#[must_use]
pub fn executable_id_schema_catalog_json() -> String {
    let schemas = executable_id_schemas();
    let mut output = String::from("{\n");
    output.push_str(&format!("  \"schema\": \"{EXECUTABLE_ID_SCHEMA_V1}\",\n"));
    output.push_str("  \"schemas\": [\n");
    for (index, schema) in schemas.iter().enumerate() {
        output.push_str("    {\n");
        output.push_str(&format!(
            "      \"$schema\": \"{JSON_SCHEMA_DRAFT_2020_12}\",\n"
        ));
        output.push_str(&format!("      \"$id\": \"{}\",\n", schema.schema_uri));
        output.push_str(&format!(
            "      \"eeSchema\": \"{}\",\n",
            schema.schema_name
        ));
        output.push_str(&format!("      \"kind\": \"{}\",\n", schema.kind));
        output.push_str(&format!("      \"title\": \"{}\",\n", schema.type_name));
        output.push_str(&format!(
            "      \"description\": \"{}\",\n",
            schema.description
        ));
        output.push_str("      \"type\": \"string\",\n");
        output.push_str(&format!("      \"pattern\": \"{}\",\n", schema.pattern()));
        output.push_str("      \"examples\": [\n");
        output.push_str(&format!("        \"{}\"\n", schema.example()));
        output.push_str("      ]\n");
        if index + 1 == schemas.len() {
            output.push_str("    }\n");
        } else {
            output.push_str("    },\n");
        }
    }
    output.push_str("  ]\n");
    output.push_str("}\n");
    output
}

/// Time-ordered, type-tagged public identifier.
///
/// The internal representation is a [`Uuid`]. The type parameter `K`
/// only exists to make different domains of IDs incompatible at the
/// Rust type level; it carries no runtime data.
pub struct Id<K: IdKind> {
    inner: Uuid,
    _phantom: PhantomData<fn() -> K>,
}

impl<K: IdKind> Id<K> {
    /// Generate a fresh time-ordered ID using UUID v7.
    ///
    /// Determinism is not promised — successive calls within the same
    /// millisecond differ in their random suffix. Tests that require
    /// determinism should construct via [`Id::from_uuid`] with a fixed
    /// [`Uuid`].
    #[must_use]
    pub fn now() -> Self {
        Self {
            inner: Uuid::now_v7(),
            _phantom: PhantomData,
        }
    }

    /// Wrap an existing [`Uuid`] as an [`Id`] of this kind.
    #[must_use]
    pub const fn from_uuid(inner: Uuid) -> Self {
        Self {
            inner,
            _phantom: PhantomData,
        }
    }

    /// Return the underlying [`Uuid`] without the type tag.
    #[must_use]
    pub const fn into_uuid(self) -> Uuid {
        self.inner
    }

    /// Return a borrowed reference to the underlying [`Uuid`].
    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.inner
    }
}

impl<K: IdKind> Clone for Id<K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K: IdKind> Copy for Id<K> {}

impl<K: IdKind> PartialEq for Id<K> {
    fn eq(&self, other: &Self) -> bool {
        self.inner == other.inner
    }
}

impl<K: IdKind> Eq for Id<K> {}

impl<K: IdKind> PartialOrd for Id<K> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<K: IdKind> Ord for Id<K> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.inner.cmp(&other.inner)
    }
}

impl<K: IdKind> Hash for Id<K> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.inner.hash(state);
    }
}

impl<K: IdKind> fmt::Display for Id<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self.inner.as_bytes();
        formatter.write_str(K::PREFIX)?;
        formatter.write_str("_")?;
        let mut buffer = [0u8; ENCODED_LEN];
        encode_crockford(bytes, &mut buffer);
        // Buffer is guaranteed ASCII because every byte came from
        // `CROCKFORD_ALPHABET`, which is ASCII-only.
        match std::str::from_utf8(&buffer) {
            Ok(text) => formatter.write_str(text),
            Err(_) => Err(fmt::Error),
        }
    }
}

impl<K: IdKind> fmt::Debug for Id<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Id<{}>({})", K::PREFIX, self)
    }
}

impl<K: IdKind> FromStr for Id<K> {
    type Err = ParseIdError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let prefix = K::PREFIX;
        let separator = match input.find('_') {
            Some(index) => index,
            None => {
                return Err(ParseIdError::MissingSeparator {
                    input: input.to_owned(),
                });
            }
        };
        let actual_prefix = &input[..separator];
        if actual_prefix != prefix {
            return Err(ParseIdError::WrongPrefix {
                input: input.to_owned(),
                expected: prefix,
                found: actual_prefix.to_owned(),
            });
        }
        let payload = &input[separator + 1..];
        if payload.len() != ENCODED_LEN {
            return Err(ParseIdError::WrongPayloadLength {
                input: input.to_owned(),
                expected: ENCODED_LEN,
                actual: payload.len(),
            });
        }
        let mut bytes = [0u8; 16];
        decode_crockford(payload.as_bytes(), &mut bytes, input)?;
        Ok(Self {
            inner: Uuid::from_bytes(bytes),
            _phantom: PhantomData,
        })
    }
}

/// Errors produced by [`Id::from_str`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseIdError {
    /// Input lacked the `prefix_payload` separator.
    MissingSeparator { input: String },
    /// The `<prefix>_` segment did not match the expected kind.
    WrongPrefix {
        input: String,
        expected: &'static str,
        found: String,
    },
    /// The base-32 payload was the wrong length.
    WrongPayloadLength {
        input: String,
        expected: usize,
        actual: usize,
    },
    /// The payload contained a character outside the Crockford alphabet.
    InvalidCharacter { input: String, character: char },
    /// The payload's leading two bits would overflow a 128-bit value.
    PayloadOverflow { input: String },
}

impl fmt::Display for ParseIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSeparator { input } => {
                write!(formatter, "id `{input}` is missing the `_` separator")
            }
            Self::WrongPrefix {
                input,
                expected,
                found,
            } => write!(
                formatter,
                "id `{input}` has prefix `{found}` but expected `{expected}`"
            ),
            Self::WrongPayloadLength {
                input,
                expected,
                actual,
            } => write!(
                formatter,
                "id `{input}` payload is {actual} characters; expected {expected}"
            ),
            Self::InvalidCharacter { input, character } => write!(
                formatter,
                "id `{input}` contains invalid base-32 character `{character}`"
            ),
            Self::PayloadOverflow { input } => write!(
                formatter,
                "id `{input}` payload exceeds 128 bits (leading symbol must be 0-7)"
            ),
        }
    }
}

impl std::error::Error for ParseIdError {}

/// Encode 16 bytes (128 bits) into a 26-character Crockford Base32 buffer.
///
/// The buffer is filled left-to-right. Two leading zero bits sit in front
/// of the 128-bit value so the encoded length is exactly 26.
fn encode_crockford(input: &[u8; 16], output: &mut [u8; ENCODED_LEN]) {
    // Pack the 16 input bytes into a 130-bit big-endian buffer with a
    // two-bit zero pad in the most-significant position.
    let mut value: u128 = 0;
    for byte in input {
        value = (value << 8) | u128::from(*byte);
    }
    // The top two bits of the 130-bit space are zero, so the leading
    // base-32 symbol is between 0 and 3.
    for (i, slot) in output.iter_mut().enumerate() {
        let shift = (ENCODED_LEN - 1 - i) * 5;
        let index = if shift >= 128 {
            0u8
        } else {
            ((value >> shift) & 0x1F) as u8
        };
        *slot = CROCKFORD_ALPHABET[index as usize];
    }
}

/// Decode a 26-character Crockford Base32 buffer into 16 bytes.
fn decode_crockford(
    input: &[u8],
    output: &mut [u8; 16],
    full_input: &str,
) -> Result<(), ParseIdError> {
    if input.len() != ENCODED_LEN {
        return Err(ParseIdError::WrongPayloadLength {
            input: full_input.to_owned(),
            expected: ENCODED_LEN,
            actual: input.len(),
        });
    }
    let mut value: u128 = 0;
    for (position, byte) in input.iter().enumerate() {
        let digit = match crockford_digit(*byte) {
            Some(value) => value,
            None => {
                return Err(ParseIdError::InvalidCharacter {
                    input: full_input.to_owned(),
                    character: *byte as char,
                });
            }
        };
        if position == 0 && digit > 7 {
            return Err(ParseIdError::PayloadOverflow {
                input: full_input.to_owned(),
            });
        }
        value = (value << 5) | u128::from(digit);
    }
    let bytes = value.to_be_bytes();
    output.copy_from_slice(&bytes);
    Ok(())
}

const fn crockford_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'H' => Some(byte - b'A' + 10),
        b'J' | b'K' => Some(byte - b'J' + 18),
        b'M' | b'N' => Some(byte - b'M' + 20),
        b'P'..=b'T' => Some(byte - b'P' + 22),
        b'V'..=b'Z' => Some(byte - b'V' + 27),
        // Lowercase mirrors uppercase per Crockford's recommendation.
        b'a'..=b'h' => Some(byte - b'a' + 10),
        b'j' | b'k' => Some(byte - b'j' + 18),
        b'm' | b'n' => Some(byte - b'm' + 20),
        b'p'..=b't' => Some(byte - b'p' + 22),
        b'v'..=b'z' => Some(byte - b'v' + 27),
        _ => None,
    }
}

/// Macro that defines a domain-specific [`IdKind`] marker and a
/// [`pub type`] alias for the associated [`Id`].
macro_rules! define_id_kind {
    ($(#[$kind_meta:meta])* $vis_kind:vis $kind:ident, $vis_alias:vis $alias:ident, $prefix:literal) => {
        $(#[$kind_meta])*
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        $vis_kind struct $kind;

        impl IdKind for $kind {
            const PREFIX: &'static str = $prefix;
        }

        $vis_alias type $alias = Id<$kind>;
    };
}

define_id_kind!(
    /// Marker for [`MemoryId`].
    pub MemoryKind,
    pub MemoryId,
    "mem"
);

define_id_kind!(
    /// Marker for [`MemoryLinkId`].
    pub MemoryLinkKind,
    pub MemoryLinkId,
    "link"
);

define_id_kind!(
    /// Marker for [`WorkspaceId`].
    pub WorkspaceKind,
    pub WorkspaceId,
    "wsp"
);

define_id_kind!(
    /// Marker for [`RuleId`].
    pub RuleKind,
    pub RuleId,
    "rule"
);

define_id_kind!(
    /// Marker for [`PackId`].
    pub PackKind,
    pub PackId,
    "pack"
);

define_id_kind!(
    /// Marker for [`ModelId`].
    pub ModelKind,
    pub ModelId,
    "mdl"
);

define_id_kind!(
    /// Marker for [`SessionId`].
    pub SessionKind,
    pub SessionId,
    "sess"
);

define_id_kind!(
    /// Marker for [`ClaimId`].
    pub ClaimKind,
    pub ClaimId,
    "claim"
);

define_id_kind!(
    /// Marker for [`EvidenceId`].
    pub EvidenceKind,
    pub EvidenceId,
    "ev"
);

define_id_kind!(
    /// Marker for [`PolicyId`].
    pub PolicyKind,
    pub PolicyId,
    "pol"
);

define_id_kind!(
    /// Marker for [`TraceId`].
    pub TraceKind,
    pub TraceId,
    "trace"
);

define_id_kind!(
    /// Marker for [`DemoId`].
    pub DemoKind,
    pub DemoId,
    "demo"
);

define_id_kind!(
    /// Marker for [`AuditId`].
    pub AuditKind,
    pub AuditId,
    "audit"
);

define_id_kind!(
    /// Marker for [`CandidateId`].
    pub CandidateKind,
    pub CandidateId,
    "cand"
);

define_id_kind!(
    /// Marker for [`BackupId`].
    pub BackupKind,
    pub BackupId,
    "bk"
);

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::str::FromStr;

    use uuid::Uuid;

    use super::{
        BackupId, ClaimId, DemoId, ENCODED_LEN, EXECUTABLE_ID_SCHEMA_V1, EvidenceId,
        ExecutableIdKind, Id, IdKind, MemoryId, ModelId, PackId, ParseExecutableIdKindError,
        ParseIdError, PolicyId, RuleId, SessionId, TraceId, WorkspaceId, encode_crockford,
        executable_id_schema_catalog_json, executable_id_schemas,
    };

    const EXECUTABLE_ID_SCHEMA_GOLDEN: &str =
        include_str!("../../tests/fixtures/golden/models/executable_id_schemas.json.golden");

    type TestResult = Result<(), String>;

    #[test]
    fn display_uses_prefix_and_26_char_payload() -> TestResult {
        let id = MemoryId::from_uuid(Uuid::nil());
        let rendered = id.to_string();
        let (prefix, payload) = match rendered.split_once('_') {
            Some(pair) => pair,
            None => {
                return Err(format!(
                    "display form is missing the `_` separator: {rendered}"
                ));
            }
        };
        assert_eq!(prefix, "mem");
        assert_eq!(payload.len(), ENCODED_LEN);
        assert!(payload.is_ascii());
        Ok(())
    }

    #[test]
    fn round_trip_through_string_for_every_kind() -> TestResult {
        let cases: Vec<String> = vec![
            MemoryId::from_uuid(uuid_with_seed(1)).to_string(),
            WorkspaceId::from_uuid(uuid_with_seed(2)).to_string(),
            RuleId::from_uuid(uuid_with_seed(3)).to_string(),
            PackId::from_uuid(uuid_with_seed(4)).to_string(),
            ModelId::from_uuid(uuid_with_seed(5)).to_string(),
            SessionId::from_uuid(uuid_with_seed(6)).to_string(),
            EvidenceId::from_uuid(uuid_with_seed(7)).to_string(),
            BackupId::from_uuid(uuid_with_seed(8)).to_string(),
            ClaimId::from_uuid(uuid_with_seed(9)).to_string(),
            PolicyId::from_uuid(uuid_with_seed(10)).to_string(),
            TraceId::from_uuid(uuid_with_seed(11)).to_string(),
            DemoId::from_uuid(uuid_with_seed(12)).to_string(),
        ];

        let parsed = (
            MemoryId::from_str(cases[0].as_str()),
            WorkspaceId::from_str(cases[1].as_str()),
            RuleId::from_str(cases[2].as_str()),
            PackId::from_str(cases[3].as_str()),
            ModelId::from_str(cases[4].as_str()),
            SessionId::from_str(cases[5].as_str()),
            EvidenceId::from_str(cases[6].as_str()),
            BackupId::from_str(cases[7].as_str()),
            ClaimId::from_str(cases[8].as_str()),
            PolicyId::from_str(cases[9].as_str()),
            TraceId::from_str(cases[10].as_str()),
            DemoId::from_str(cases[11].as_str()),
        );

        let memory = unwrap_ok(parsed.0)?;
        let workspace = unwrap_ok(parsed.1)?;
        let rule = unwrap_ok(parsed.2)?;
        let pack = unwrap_ok(parsed.3)?;
        let model = unwrap_ok(parsed.4)?;
        let session = unwrap_ok(parsed.5)?;
        let evidence = unwrap_ok(parsed.6)?;
        let backup = unwrap_ok(parsed.7)?;
        let claim = unwrap_ok(parsed.8)?;
        let policy = unwrap_ok(parsed.9)?;
        let trace = unwrap_ok(parsed.10)?;
        let demo = unwrap_ok(parsed.11)?;

        assert_eq!(memory.into_uuid(), uuid_with_seed(1));
        assert_eq!(workspace.into_uuid(), uuid_with_seed(2));
        assert_eq!(rule.into_uuid(), uuid_with_seed(3));
        assert_eq!(pack.into_uuid(), uuid_with_seed(4));
        assert_eq!(model.into_uuid(), uuid_with_seed(5));
        assert_eq!(session.into_uuid(), uuid_with_seed(6));
        assert_eq!(evidence.into_uuid(), uuid_with_seed(7));
        assert_eq!(backup.into_uuid(), uuid_with_seed(8));
        assert_eq!(claim.into_uuid(), uuid_with_seed(9));
        assert_eq!(policy.into_uuid(), uuid_with_seed(10));
        assert_eq!(trace.into_uuid(), uuid_with_seed(11));
        assert_eq!(demo.into_uuid(), uuid_with_seed(12));
        Ok(())
    }

    #[test]
    fn parse_rejects_wrong_prefix() -> TestResult {
        let payload = MemoryId::from_uuid(uuid_with_seed(11)).to_string();
        let swapped = payload.replace("mem_", "wsp_");
        let err = unwrap_err(MemoryId::from_str(&swapped))?;
        match err {
            ParseIdError::WrongPrefix {
                expected, found, ..
            } => {
                assert_eq!(expected, "mem");
                assert_eq!(found, "wsp");
            }
            other => return Err(format!("expected WrongPrefix, got {other:?}")),
        }
        Ok(())
    }

    #[test]
    fn parse_rejects_missing_separator() -> TestResult {
        let err = unwrap_err(MemoryId::from_str("mem01HQ3K5Z"))?;
        assert!(matches!(err, ParseIdError::MissingSeparator { .. }));
        Ok(())
    }

    #[test]
    fn parse_rejects_wrong_payload_length() -> TestResult {
        let err = unwrap_err(MemoryId::from_str("mem_TOOSHORT"))?;
        match err {
            ParseIdError::WrongPayloadLength {
                expected, actual, ..
            } => {
                assert_eq!(expected, ENCODED_LEN);
                assert_eq!(actual, 8);
            }
            other => return Err(format!("expected WrongPayloadLength, got {other:?}")),
        }
        Ok(())
    }

    #[test]
    fn parse_rejects_invalid_character() -> TestResult {
        // `I` is not in the Crockford alphabet.
        let bad = format!("mem_{}", "I".repeat(ENCODED_LEN));
        let err = unwrap_err(MemoryId::from_str(&bad))?;
        assert!(matches!(err, ParseIdError::InvalidCharacter { .. }));
        Ok(())
    }

    #[test]
    fn parse_rejects_overflowing_first_digit() -> TestResult {
        // Leading character > 7 means the 130-bit packed value would
        // exceed 128 bits of payload space.
        let mut characters = vec![b'8'];
        characters.extend(std::iter::repeat_n(b'0', ENCODED_LEN - 1));
        let payload = match std::str::from_utf8(&characters) {
            Ok(value) => value.to_owned(),
            Err(error) => return Err(format!("synthetic payload is invalid UTF-8: {error}")),
        };
        let bad = format!("mem_{payload}");
        let err = unwrap_err(MemoryId::from_str(&bad))?;
        assert!(matches!(err, ParseIdError::PayloadOverflow { .. }));
        Ok(())
    }

    #[test]
    fn parse_accepts_lowercase_payload() -> TestResult {
        let upper = MemoryId::from_uuid(uuid_with_seed(13)).to_string();
        let lower = upper.to_lowercase();
        // The prefix is already lowercase, so to_lowercase only changes
        // the payload.
        let parsed = unwrap_ok(MemoryId::from_str(&lower))?;
        assert_eq!(parsed.into_uuid(), uuid_with_seed(13));
        Ok(())
    }

    #[test]
    fn ordering_matches_uuid_ordering() {
        // UUID v7 layout puts the timestamp in the most significant
        // bytes, so a later UUID compares greater.
        let a = MemoryId::from_uuid(uuid_with_seed(0));
        let b = MemoryId::from_uuid(uuid_with_seed(1));
        assert!(a < b);
        assert!(b > a);
        assert!(a == a);
    }

    #[test]
    fn ids_from_now_v7_are_unique_within_a_burst() {
        // Generating many IDs back-to-back should produce distinct
        // payloads thanks to the 74 random bits in v7.
        let mut seen = HashSet::new();
        for _ in 0..256 {
            let id = MemoryId::now();
            assert!(seen.insert(id), "duplicate ID generated: {id}");
        }
    }

    #[test]
    fn type_safety_at_compile_time() {
        // This test does not have a body; it documents that the
        // following snippet would fail to compile because the parser
        // returns a kind-tagged value:
        //
        //   let memory = MemoryId::from_uuid(uuid_with_seed(0));
        //   let workspace: WorkspaceId = memory; // type mismatch
        //
        // The compile-fail enforcement lives in the `compile_fail`
        // doctests at the crate root once they are added. Here we
        // simply assert that the marker structs are zero-sized.
        assert_eq!(std::mem::size_of::<super::MemoryKind>(), 0);
        assert_eq!(std::mem::size_of::<super::WorkspaceKind>(), 0);
        assert_eq!(std::mem::size_of::<MemoryId>(), std::mem::size_of::<Uuid>());
    }

    #[test]
    fn encode_decode_round_trips_for_full_byte_range() -> TestResult {
        for byte in 0u8..=255u8 {
            let bytes = [byte; 16];
            let mut buffer = [0u8; ENCODED_LEN];
            encode_crockford(&bytes, &mut buffer);
            let payload = match std::str::from_utf8(&buffer) {
                Ok(value) => value.to_owned(),
                Err(error) => return Err(format!("encoder produced non-UTF-8 output: {error}")),
            };
            let formatted = format!("mem_{payload}");
            let parsed = unwrap_ok(MemoryId::from_str(&formatted))?;
            assert_eq!(parsed.into_uuid().as_bytes(), &bytes);
        }
        Ok(())
    }

    #[test]
    fn debug_format_includes_prefix() {
        let id = MemoryId::from_uuid(uuid_with_seed(5));
        let rendered = format!("{id:?}");
        assert!(rendered.starts_with("Id<mem>("));
        assert!(rendered.contains("mem_"));
    }

    #[test]
    fn hash_and_equality_use_only_inner_uuid() {
        use std::collections::HashSet;
        let id_a = MemoryId::from_uuid(uuid_with_seed(7));
        let id_b = MemoryId::from_uuid(uuid_with_seed(7));
        let id_c = MemoryId::from_uuid(uuid_with_seed(8));
        assert_eq!(id_a, id_b);
        assert_ne!(id_a, id_c);
        let mut set = HashSet::new();
        set.insert(id_a);
        assert!(set.contains(&id_b));
        assert!(!set.contains(&id_c));
    }

    #[test]
    fn executable_id_kinds_round_trip_in_schema_order() {
        let kinds = ExecutableIdKind::all();
        assert_eq!(
            kinds.map(|kind| kind.as_str()),
            ["claim", "evidence", "policy", "trace", "demo"]
        );
        for kind in kinds {
            let parsed = ExecutableIdKind::from_str(kind.as_str());
            assert_eq!(parsed, Ok(kind));
        }
    }

    #[test]
    fn executable_id_kind_rejects_unknown_input() {
        let err = ExecutableIdKind::from_str("memory");
        assert!(matches!(err, Err(ParseExecutableIdKindError { .. })));
    }

    #[test]
    fn executable_id_schema_metadata_matches_typed_ids() -> TestResult {
        let schemas = executable_id_schemas();
        assert_eq!(schemas.len(), 5);

        assert_eq!(schemas[0].kind, ExecutableIdKind::Claim);
        assert_eq!(schemas[0].type_name, "ClaimId");
        assert_eq!(schemas[0].prefix, "claim");
        assert_eq!(schemas[0].pattern(), "^claim_[0-7][0-9A-HJKMNP-TV-Z]{25}$");
        assert_eq!(
            unwrap_ok(ClaimId::from_str(&schemas[0].example()))?.into_uuid(),
            Uuid::nil()
        );

        assert_eq!(schemas[1].kind, ExecutableIdKind::Evidence);
        assert_eq!(schemas[1].type_name, "EvidenceId");
        assert_eq!(schemas[1].prefix, "ev");
        assert_eq!(
            unwrap_ok(EvidenceId::from_str(&schemas[1].example()))?.into_uuid(),
            Uuid::nil()
        );

        assert_eq!(schemas[2].kind, ExecutableIdKind::Policy);
        assert_eq!(schemas[2].type_name, "PolicyId");
        assert_eq!(schemas[2].prefix, "pol");
        assert_eq!(
            unwrap_ok(PolicyId::from_str(&schemas[2].example()))?.into_uuid(),
            Uuid::nil()
        );

        assert_eq!(schemas[3].kind, ExecutableIdKind::Trace);
        assert_eq!(schemas[3].type_name, "TraceId");
        assert_eq!(schemas[3].prefix, "trace");
        assert_eq!(
            unwrap_ok(TraceId::from_str(&schemas[3].example()))?.into_uuid(),
            Uuid::nil()
        );

        assert_eq!(schemas[4].kind, ExecutableIdKind::Demo);
        assert_eq!(schemas[4].type_name, "DemoId");
        assert_eq!(schemas[4].prefix, "demo");
        assert_eq!(
            unwrap_ok(DemoId::from_str(&schemas[4].example()))?.into_uuid(),
            Uuid::nil()
        );
        Ok(())
    }

    #[test]
    fn executable_id_schema_catalog_matches_golden_fixture() {
        assert_eq!(
            executable_id_schema_catalog_json(),
            EXECUTABLE_ID_SCHEMA_GOLDEN
        );
    }

    #[test]
    fn executable_id_schema_catalog_is_valid_json() -> TestResult {
        let parsed: serde_json::Value = match serde_json::from_str(EXECUTABLE_ID_SCHEMA_GOLDEN) {
            Ok(value) => value,
            Err(error) => {
                return Err(format!(
                    "executable ID schema golden is invalid JSON: {error}"
                ));
            }
        };
        assert_eq!(
            parsed.get("schema").and_then(serde_json::Value::as_str),
            Some(EXECUTABLE_ID_SCHEMA_V1)
        );
        let schemas = match parsed.get("schemas").and_then(serde_json::Value::as_array) {
            Some(value) => value,
            None => return Err("executable ID schema golden is missing schemas array".to_string()),
        };
        assert_eq!(schemas.len(), ExecutableIdKind::all().len());
        Ok(())
    }

    fn uuid_with_seed(seed: u8) -> Uuid {
        let mut bytes = [0u8; 16];
        bytes[0] = seed;
        bytes[1] = 0x77; // arbitrary
        bytes[6] = 0x70 | (bytes[6] & 0x0F); // version 7
        bytes[8] = 0x80 | (bytes[8] & 0x3F); // variant 10
        Uuid::from_bytes(bytes)
    }

    fn unwrap_ok<K: IdKind>(result: Result<Id<K>, ParseIdError>) -> Result<Id<K>, String> {
        result.map_err(|error| format!("expected Ok, got Err({error:?})"))
    }

    fn unwrap_err<K: IdKind>(result: Result<Id<K>, ParseIdError>) -> Result<ParseIdError, String> {
        match result {
            Ok(value) => Err(format!("expected Err, got Ok({value:?})")),
            Err(error) => Ok(error),
        }
    }
}
