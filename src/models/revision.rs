//! Memory revision tracking (EE-067).
//!
//! Types for tracking memory revisions, supersession chains, and
//! legal hold constraints. These enable:
//!
//! * **Revision groups**: Multiple versions of the same logical memory
//!   share a group ID, enabling history queries and rollback.
//! * **Supersession links**: Explicit pointers from a new memory to
//!   the one it replaces, forming a directed acyclic graph.
//! * **Idempotency keys**: Prevent duplicate imports from external
//!   sources (CASS sessions, agent hooks).
//! * **Legal holds**: Mark memories that must not be deleted or
//!   modified, with audit trail.
//!
//! The revision model follows these invariants:
//! - A revision group ID is stable across all versions of a memory.
//! - Supersession forms a DAG (no cycles allowed).
//! - At most one memory in a group can be "current" (not superseded).
//! - Legal holds are append-only; they can only be released, not modified.

use std::fmt;

/// Prefix for revision group IDs.
pub const REVISION_GROUP_PREFIX: &str = "rev_";

/// Expected length for revision group IDs (prefix + 25 chars = 29 total).
pub const REVISION_GROUP_ID_LEN: usize = 29;

/// Prefix for legal hold IDs.
pub const LEGAL_HOLD_PREFIX: &str = "hold_";

/// Expected length for legal hold IDs (prefix + 25 chars = 30 total).
pub const LEGAL_HOLD_ID_LEN: usize = 30;

/// Error validating a revision or hold ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RevisionIdError {
    /// ID has wrong prefix.
    WrongPrefix {
        input: String,
        expected: &'static str,
    },
    /// ID has wrong length.
    WrongLength {
        input: String,
        expected: usize,
        actual: usize,
    },
}

impl fmt::Display for RevisionIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongPrefix { input, expected } => {
                write!(f, "ID `{input}` must start with `{expected}`")
            }
            Self::WrongLength {
                input,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "ID `{input}` has length {actual}, expected {expected}"
                )
            }
        }
    }
}

impl std::error::Error for RevisionIdError {}

/// A validated revision group ID.
///
/// All versions of a memory share the same revision group ID, enabling
/// history queries ("show me all versions of this memory") and rollback.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RevisionGroupId(String);

impl RevisionGroupId {
    /// Parse and validate a revision group ID.
    ///
    /// # Errors
    ///
    /// Returns error if the ID doesn't match `rev_*` pattern with correct length.
    pub fn parse(input: impl Into<String>) -> Result<Self, RevisionIdError> {
        let id = input.into();
        if !id.starts_with(REVISION_GROUP_PREFIX) {
            return Err(RevisionIdError::WrongPrefix {
                input: id,
                expected: REVISION_GROUP_PREFIX,
            });
        }
        if id.len() != REVISION_GROUP_ID_LEN {
            return Err(RevisionIdError::WrongLength {
                input: id.clone(),
                expected: REVISION_GROUP_ID_LEN,
                actual: id.len(),
            });
        }
        Ok(Self(id))
    }

    /// Create from a string that's already been validated.
    ///
    /// # Panics
    ///
    /// Panics in debug mode if the ID is invalid. Use `parse` for untrusted input.
    #[must_use]
    pub fn from_trusted(id: impl Into<String>) -> Self {
        let id = id.into();
        debug_assert!(
            id.starts_with(REVISION_GROUP_PREFIX) && id.len() == REVISION_GROUP_ID_LEN,
            "invalid revision group ID: {id}"
        );
        Self(id)
    }

    /// The raw ID string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RevisionGroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for RevisionGroupId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// A validated legal hold ID.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LegalHoldId(String);

impl LegalHoldId {
    /// Parse and validate a legal hold ID.
    pub fn parse(input: impl Into<String>) -> Result<Self, RevisionIdError> {
        let id = input.into();
        if !id.starts_with(LEGAL_HOLD_PREFIX) {
            return Err(RevisionIdError::WrongPrefix {
                input: id,
                expected: LEGAL_HOLD_PREFIX,
            });
        }
        if id.len() != LEGAL_HOLD_ID_LEN {
            return Err(RevisionIdError::WrongLength {
                input: id.clone(),
                expected: LEGAL_HOLD_ID_LEN,
                actual: id.len(),
            });
        }
        Ok(Self(id))
    }

    /// Create from a trusted string.
    #[must_use]
    pub fn from_trusted(id: impl Into<String>) -> Self {
        let id = id.into();
        debug_assert!(
            id.starts_with(LEGAL_HOLD_PREFIX) && id.len() == LEGAL_HOLD_ID_LEN,
            "invalid legal hold ID: {id}"
        );
        Self(id)
    }

    /// The raw ID string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LegalHoldId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for LegalHoldId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Supersession relationship between memories.
///
/// Records that a newer memory supersedes an older one, preserving
/// the full history chain for audit and rollback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupersessionLink {
    /// Memory ID being superseded (older version).
    pub superseded_id: String,
    /// Memory ID doing the superseding (newer version).
    pub superseding_id: String,
    /// Why this supersession happened.
    pub reason: SupersessionReason,
    /// Timestamp when the supersession was recorded.
    pub created_at: String,
}

impl SupersessionLink {
    /// Create a new supersession link.
    #[must_use]
    pub fn new(
        superseded_id: impl Into<String>,
        superseding_id: impl Into<String>,
        reason: SupersessionReason,
        created_at: impl Into<String>,
    ) -> Self {
        Self {
            superseded_id: superseded_id.into(),
            superseding_id: superseding_id.into(),
            reason,
            created_at: created_at.into(),
        }
    }
}

/// Why one memory supersedes another.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SupersessionReason {
    /// User explicitly updated the memory.
    #[default]
    UserUpdate,
    /// Memory was refined through curation.
    Curation,
    /// Memory was consolidated with others.
    Consolidation,
    /// Memory was corrected due to feedback.
    Correction,
    /// Memory was imported from an external source.
    Import,
    /// Memory was auto-generated by the system.
    SystemGenerated,
}

impl SupersessionReason {
    /// Stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UserUpdate => "user_update",
            Self::Curation => "curation",
            Self::Consolidation => "consolidation",
            Self::Correction => "correction",
            Self::Import => "import",
            Self::SystemGenerated => "system_generated",
        }
    }

    /// Parse from string.
    #[must_use]
    pub fn from_str(s: &str) -> Self {
        match s {
            "user_update" => Self::UserUpdate,
            "curation" => Self::Curation,
            "consolidation" => Self::Consolidation,
            "correction" => Self::Correction,
            "import" => Self::Import,
            "system_generated" => Self::SystemGenerated,
            _ => Self::UserUpdate,
        }
    }
}

impl fmt::Display for SupersessionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Idempotency key for preventing duplicate imports.
///
/// External sources (CASS sessions, hooks) provide a key that uniquely
/// identifies the import operation. Re-importing with the same key is
/// a no-op rather than creating duplicate memories.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// Maximum length for an idempotency key.
    pub const MAX_LEN: usize = 128;

    /// Create a new idempotency key.
    ///
    /// # Errors
    ///
    /// Returns error if key is empty or exceeds max length.
    pub fn new(key: impl Into<String>) -> Result<Self, IdempotencyKeyError> {
        let key = key.into();
        if key.is_empty() {
            return Err(IdempotencyKeyError::Empty);
        }
        if key.len() > Self::MAX_LEN {
            return Err(IdempotencyKeyError::TooLong {
                len: key.len(),
                max: Self::MAX_LEN,
            });
        }
        Ok(Self(key))
    }

    /// The raw key string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for IdempotencyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for IdempotencyKey {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Errors validating an idempotency key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdempotencyKeyError {
    /// Key is empty.
    Empty,
    /// Key exceeds maximum length.
    TooLong { len: usize, max: usize },
}

impl fmt::Display for IdempotencyKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "idempotency key cannot be empty"),
            Self::TooLong { len, max } => {
                write!(f, "idempotency key too long: {len} bytes (max {max})")
            }
        }
    }
}

impl std::error::Error for IdempotencyKeyError {}

/// Legal hold on a memory.
///
/// Prevents deletion or modification of a memory for compliance,
/// audit, or litigation purposes. Legal holds are append-only:
/// they can be released but not modified.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegalHold {
    /// Unique ID for this hold.
    pub hold_id: LegalHoldId,
    /// Memory ID under hold.
    pub memory_id: String,
    /// Reason for the hold (free text, auditable).
    pub reason: String,
    /// Who placed the hold.
    pub placed_by: String,
    /// When the hold was placed.
    pub placed_at: String,
    /// When the hold was released (if released).
    pub released_at: Option<String>,
    /// Who released the hold.
    pub released_by: Option<String>,
}

impl LegalHold {
    /// Create a new active legal hold.
    #[must_use]
    pub fn new(
        hold_id: LegalHoldId,
        memory_id: impl Into<String>,
        reason: impl Into<String>,
        placed_by: impl Into<String>,
        placed_at: impl Into<String>,
    ) -> Self {
        Self {
            hold_id,
            memory_id: memory_id.into(),
            reason: reason.into(),
            placed_by: placed_by.into(),
            placed_at: placed_at.into(),
            released_at: None,
            released_by: None,
        }
    }

    /// Whether this hold is currently active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.released_at.is_none()
    }

    /// Release this hold.
    #[must_use]
    pub fn release(mut self, released_by: impl Into<String>, released_at: impl Into<String>) -> Self {
        self.released_by = Some(released_by.into());
        self.released_at = Some(released_at.into());
        self
    }
}

/// Memory revision metadata.
///
/// Tracks version history for a memory, enabling history queries
/// and rollback operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevisionMeta {
    /// Revision group ID (stable across versions).
    pub group_id: RevisionGroupId,
    /// Version number within the group (1 = first version).
    pub version: u32,
    /// Memory ID this version supersedes (if any).
    pub supersedes: Option<String>,
    /// Whether this is the current (active) version.
    pub is_current: bool,
    /// Idempotency key for this revision (if imported).
    pub idempotency_key: Option<IdempotencyKey>,
}

impl RevisionMeta {
    /// Create metadata for the first version of a memory.
    #[must_use]
    pub fn first(group_id: RevisionGroupId) -> Self {
        Self {
            group_id,
            version: 1,
            supersedes: None,
            is_current: true,
            idempotency_key: None,
        }
    }

    /// Create metadata for a subsequent version.
    #[must_use]
    pub fn subsequent(
        group_id: RevisionGroupId,
        version: u32,
        supersedes: impl Into<String>,
    ) -> Self {
        Self {
            group_id,
            version,
            supersedes: Some(supersedes.into()),
            is_current: true,
            idempotency_key: None,
        }
    }

    /// Builder: set idempotency key.
    #[must_use]
    pub fn with_idempotency_key(mut self, key: IdempotencyKey) -> Self {
        self.idempotency_key = Some(key);
        self
    }

    /// Builder: mark as not current (superseded).
    #[must_use]
    pub fn as_superseded(mut self) -> Self {
        self.is_current = false;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure_equal<T: std::fmt::Debug + PartialEq>(
        actual: &T,
        expected: &T,
        context: &str,
    ) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn revision_group_id_validates_format() -> TestResult {
        let valid = RevisionGroupId::parse("rev_test000000000000000000000");
        assert!(valid.is_ok(), "valid ID should parse");

        let wrong_prefix = RevisionGroupId::parse("mem_test000000000000000000000");
        assert!(wrong_prefix.is_err(), "wrong prefix should fail");

        let too_short = RevisionGroupId::parse("rev_abc");
        assert!(too_short.is_err(), "too short should fail");

        Ok(())
    }

    #[test]
    fn legal_hold_id_validates_format() -> TestResult {
        let valid = LegalHoldId::parse("hold_test000000000000000000000");
        assert!(valid.is_ok(), "valid ID should parse");

        let wrong_prefix = LegalHoldId::parse("rev_test0000000000000000000");
        assert!(wrong_prefix.is_err(), "wrong prefix should fail");

        Ok(())
    }

    #[test]
    fn supersession_reason_strings_are_stable() -> TestResult {
        ensure_equal(&SupersessionReason::UserUpdate.as_str(), &"user_update", "user_update")?;
        ensure_equal(&SupersessionReason::Curation.as_str(), &"curation", "curation")?;
        ensure_equal(
            &SupersessionReason::Consolidation.as_str(),
            &"consolidation",
            "consolidation",
        )?;
        ensure_equal(&SupersessionReason::Correction.as_str(), &"correction", "correction")?;
        ensure_equal(&SupersessionReason::Import.as_str(), &"import", "import")?;
        ensure_equal(
            &SupersessionReason::SystemGenerated.as_str(),
            &"system_generated",
            "system_generated",
        )
    }

    #[test]
    fn supersession_reason_round_trips() {
        for reason in [
            SupersessionReason::UserUpdate,
            SupersessionReason::Curation,
            SupersessionReason::Consolidation,
            SupersessionReason::Correction,
            SupersessionReason::Import,
            SupersessionReason::SystemGenerated,
        ] {
            let parsed = SupersessionReason::from_str(reason.as_str());
            assert_eq!(reason, parsed, "round trip failed for {reason:?}");
        }
    }

    #[test]
    fn idempotency_key_validates_length() {
        let valid = IdempotencyKey::new("import-session-abc123");
        assert!(valid.is_ok(), "valid key should work");

        let empty = IdempotencyKey::new("");
        assert!(
            matches!(empty, Err(IdempotencyKeyError::Empty)),
            "empty key should fail"
        );

        let too_long = IdempotencyKey::new("x".repeat(200));
        assert!(
            matches!(too_long, Err(IdempotencyKeyError::TooLong { .. })),
            "too long key should fail"
        );
    }

    #[test]
    fn legal_hold_lifecycle() {
        let hold_id = LegalHoldId::from_trusted("hold_test000000000000000000000");
        let hold = LegalHold::new(
            hold_id,
            "mem_test000000000000000000000",
            "Litigation hold",
            "legal@example.com",
            "2026-01-01T00:00:00Z",
        );

        assert!(hold.is_active(), "new hold should be active");

        let released = hold.release("legal@example.com", "2026-02-01T00:00:00Z");
        assert!(!released.is_active(), "released hold should not be active");
        assert_eq!(
            released.released_at,
            Some("2026-02-01T00:00:00Z".to_string())
        );
    }

    #[test]
    fn revision_meta_version_tracking() {
        let group_id = RevisionGroupId::from_trusted("rev_test000000000000000000000");

        let first = RevisionMeta::first(group_id.clone());
        assert_eq!(first.version, 1);
        assert!(first.is_current);
        assert!(first.supersedes.is_none());

        let second = RevisionMeta::subsequent(group_id.clone(), 2, "mem_v1");
        assert_eq!(second.version, 2);
        assert!(second.is_current);
        assert_eq!(second.supersedes, Some("mem_v1".to_string()));

        let superseded = first.as_superseded();
        assert!(!superseded.is_current);
    }

    #[test]
    fn revision_meta_with_idempotency_key() {
        let group_id = RevisionGroupId::from_trusted("rev_test000000000000000000000");
        let key = IdempotencyKey::new("import-xyz").expect("valid key");

        let meta = RevisionMeta::first(group_id).with_idempotency_key(key);
        assert!(meta.idempotency_key.is_some());
        assert_eq!(meta.idempotency_key.unwrap().as_str(), "import-xyz");
    }

    #[test]
    fn supersession_link_creation() {
        let link = SupersessionLink::new(
            "mem_old",
            "mem_new",
            SupersessionReason::Curation,
            "2026-01-01T00:00:00Z",
        );

        assert_eq!(link.superseded_id, "mem_old");
        assert_eq!(link.superseding_id, "mem_new");
        assert_eq!(link.reason, SupersessionReason::Curation);
    }
}
