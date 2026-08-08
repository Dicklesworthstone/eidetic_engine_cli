//! Sealed (commit-reveal) memory models.
//!
//! A seal commits to a memory's content by hash at write time while the
//! content itself is withheld, so an agent can prove afterwards that a
//! protocol, prediction, or expected outcome was registered before its
//! result was observed (bd-sealed-preregistration-memory-b67be). The model
//! layer owns the stable wire strings, the domain-separated commitment
//! computation, and validation; storage and CLI flows are later concerns.

use std::fmt;

pub const MEMORY_SEAL_SCHEMA_V1: &str = "ee.memory_seal.v1";
pub const MEMORY_SEAL_COMMITMENT_SCHEMA_V1: &str = "ee.memory_seal.commitment.v1";

/// Deterministic placeholder stored as `memories.content` for a sealed,
/// not-yet-revealed memory. `memories.content` is NOT NULL with a
/// non-empty CHECK, and this exact byte sequence is what pack/search
/// exclusion recognizes; never localize or reword it without a schema bump.
pub const MEMORY_SEAL_PLACEHOLDER_CONTENT: &str =
    "[sealed memory: content committed by hash; reveal with ee memory reveal <id>]";

const BLAKE3_PREFIX: &str = "blake3:";
const BLAKE3_HEX_LEN: usize = 64;

/// Compute the domain-separated content commitment for a seal.
///
/// The commitment is `blake3` over a length-prefixed domain part followed
/// by the length-prefixed exact content bytes — the same length-prefix
/// idiom the sentinel spec hash uses, so a commitment over content that
/// happens to look like the domain string cannot collide.
#[must_use]
pub fn memory_seal_commitment(content: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in [MEMORY_SEAL_COMMITMENT_SCHEMA_V1.as_bytes(), content] {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    format!("{BLAKE3_PREFIX}{}", hasher.finalize().to_hex())
}

/// A stored seal row joined to its memory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemorySeal {
    pub memory_id: String,
    pub content_commitment: String,
    pub sealed_at: String,
    pub revealed_at: Option<String>,
    /// `Some(true)` once a reveal supplied bytes matching the commitment;
    /// never set on mismatch (failed attempts live in the audit log only).
    pub reveal_verified: Option<bool>,
}

impl MemorySeal {
    /// Whether the seal is still closed (content withheld).
    #[must_use]
    pub const fn is_sealed(&self) -> bool {
        self.revealed_at.is_none()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MemorySealValidationError {
    InvalidCommitment { value: String },
    EmptyContent,
}

impl MemorySealValidationError {
    #[must_use]
    pub fn repair(&self) -> &'static str {
        match self {
            Self::InvalidCommitment { .. } => {
                "Commitments are `blake3:<64 hex>`; recompute with `ee memory reveal <id> --content-file <path>` instead of editing the value by hand."
            }
            Self::EmptyContent => {
                "Sealed content must be non-empty; supply the exact bytes the protocol will be judged against."
            }
        }
    }
}

impl fmt::Display for MemorySealValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCommitment { value } => {
                write!(formatter, "invalid seal commitment {value:?}")
            }
            Self::EmptyContent => write!(formatter, "sealed content must be non-empty"),
        }
    }
}

impl std::error::Error for MemorySealValidationError {}

/// Validate a commitment's wire format (`blake3:` + 64 lowercase hex).
///
/// # Errors
///
/// Returns [`MemorySealValidationError::InvalidCommitment`] when the value
/// is not a well-formed blake3 commitment string.
pub fn validate_memory_seal_commitment(value: &str) -> Result<(), MemorySealValidationError> {
    let well_formed = value.len() == BLAKE3_PREFIX.len() + BLAKE3_HEX_LEN
        && value.starts_with(BLAKE3_PREFIX)
        && value[BLAKE3_PREFIX.len()..]
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase());
    if well_formed {
        Ok(())
    } else {
        Err(MemorySealValidationError::InvalidCommitment {
            value: value.to_owned(),
        })
    }
}

/// Compute a commitment for seal creation, rejecting empty content.
///
/// # Errors
///
/// Returns [`MemorySealValidationError::EmptyContent`] when `content` is
/// empty or whitespace-only.
pub fn seal_commitment_for_content(content: &[u8]) -> Result<String, MemorySealValidationError> {
    if content.is_empty() || content.iter().all(u8::is_ascii_whitespace) {
        return Err(MemorySealValidationError::EmptyContent);
    }
    Ok(memory_seal_commitment(content))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    #[test]
    fn commitment_is_deterministic_and_content_sensitive() -> TestResult {
        let first = memory_seal_commitment(b"pre-registered protocol v1");
        let second = memory_seal_commitment(b"pre-registered protocol v1");
        let different = memory_seal_commitment(b"pre-registered protocol v2");
        ensure(first == second, "identical bytes must commit identically")?;
        ensure(
            first != different,
            "different bytes must produce different commitments",
        )?;
        validate_memory_seal_commitment(&first).map_err(|error| error.to_string())
    }

    #[test]
    fn commitment_is_domain_separated_from_bare_blake3() -> TestResult {
        let content = b"guessable";
        let committed = memory_seal_commitment(content);
        let bare = format!("blake3:{}", blake3::hash(content).to_hex());
        ensure(
            committed != bare,
            "commitment must not equal the bare blake3 of the content",
        )
    }

    #[test]
    fn empty_and_whitespace_content_is_rejected() -> TestResult {
        ensure(
            matches!(
                seal_commitment_for_content(b""),
                Err(MemorySealValidationError::EmptyContent)
            ),
            "empty content must be rejected",
        )?;
        ensure(
            matches!(
                seal_commitment_for_content(b"  \n\t "),
                Err(MemorySealValidationError::EmptyContent)
            ),
            "whitespace-only content must be rejected",
        )
    }

    #[test]
    fn commitment_wire_format_is_validated() -> TestResult {
        ensure(
            validate_memory_seal_commitment("blake3:zz").is_err(),
            "short/invalid hex must be rejected",
        )?;
        let uppercase = format!("blake3:{}", "A".repeat(64));
        ensure(
            validate_memory_seal_commitment(&uppercase).is_err(),
            "uppercase hex must be rejected",
        )?;
        ensure(
            validate_memory_seal_commitment(&memory_seal_commitment(b"x")).is_ok(),
            "real commitments must validate",
        )
    }

    #[test]
    fn sealed_state_tracks_reveal() {
        let mut seal = MemorySeal {
            memory_id: "mem_00000000000000000000000000".to_owned(),
            content_commitment: memory_seal_commitment(b"content"),
            sealed_at: "2026-08-08T00:00:00Z".to_owned(),
            revealed_at: None,
            reveal_verified: None,
        };
        assert!(seal.is_sealed());
        seal.revealed_at = Some("2026-08-08T01:00:00Z".to_owned());
        seal.reveal_verified = Some(true);
        assert!(!seal.is_sealed());
    }
}
