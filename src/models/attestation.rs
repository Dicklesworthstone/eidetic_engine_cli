//! Canonical attestation bundle model.
//!
//! Attestation bundles are internal chain-of-custody manifests. They attest
//! what local `ee` evidence and hashes were used for a subject; they do not
//! claim that the underlying evidence is objectively true.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// v2 (bd-2ea4a): adds the optional `seal` block so a third party can verify
/// commitment-before-outcome offline. The block is absent for unsealed
/// subjects because it has no evidence to carry there.
pub const ATTESTATION_BUNDLE_SCHEMA_V2: &str = "ee.attestation.bundle.v2";
pub const ATTESTATION_HASH_ALGORITHM: &str = "blake3";
pub const ATTESTATION_LOCAL_TRUTH_STATEMENT: &str =
    "Attests local ee evidence custody and hash consistency; does not attest objective truth.";

fn normalized_attestation_token(input: &str) -> String {
    let trimmed = input.trim();
    let mut normalized = String::with_capacity(trimmed.len());
    let mut previous_was_lowercase = false;
    let mut previous_was_separator = false;

    for character in trimmed.chars() {
        match character {
            '-' | '_' => {
                if !normalized.is_empty() && !previous_was_separator {
                    normalized.push('_');
                }
                previous_was_lowercase = false;
                previous_was_separator = true;
            }
            character if character.is_ascii_uppercase() => {
                if previous_was_lowercase && !previous_was_separator {
                    normalized.push('_');
                }
                normalized.push(character.to_ascii_lowercase());
                previous_was_lowercase = false;
                previous_was_separator = false;
            }
            character => {
                normalized.push(character.to_ascii_lowercase());
                previous_was_lowercase = character.is_ascii_lowercase();
                previous_was_separator = false;
            }
        }
    }

    normalized
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttestationSubjectKind {
    Memory,
    Pack,
    Query,
    CurationCandidate,
    Procedure,
    Backup,
}

impl AttestationSubjectKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Pack => "pack",
            Self::Query => "query",
            Self::CurationCandidate => "curation_candidate",
            Self::Procedure => "procedure",
            Self::Backup => "backup",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 6] {
        [
            Self::Memory,
            Self::Pack,
            Self::Query,
            Self::CurationCandidate,
            Self::Procedure,
            Self::Backup,
        ]
    }
}

impl fmt::Display for AttestationSubjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseAttestationSubjectKindError {
    input: String,
}

impl ParseAttestationSubjectKindError {
    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for ParseAttestationSubjectKindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown attestation subject kind `{}`; expected one of memory, pack, query, curation_candidate, procedure, backup",
            self.input
        )
    }
}

impl std::error::Error for ParseAttestationSubjectKindError {}

impl FromStr for AttestationSubjectKind {
    type Err = ParseAttestationSubjectKindError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match normalized_attestation_token(input).as_str() {
            "memory" => Ok(Self::Memory),
            "pack" => Ok(Self::Pack),
            "query" => Ok(Self::Query),
            "curation_candidate" => Ok(Self::CurationCandidate),
            "procedure" => Ok(Self::Procedure),
            "backup" => Ok(Self::Backup),
            _ => Err(ParseAttestationSubjectKindError {
                input: input.to_owned(),
            }),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttestationHashEntry {
    pub label: String,
    pub algorithm: String,
    pub hash: String,
}

impl AttestationHashEntry {
    #[must_use]
    pub fn blake3(label: impl Into<String>, hash: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            algorithm: ATTESTATION_HASH_ALGORITHM.to_owned(),
            hash: hash.into(),
        }
    }

    #[must_use]
    fn canonical_json_value(&self) -> Value {
        object([
            ("label", Value::String(self.label.clone())),
            ("algorithm", Value::String(self.algorithm.clone())),
            ("hash", Value::String(self.hash.clone())),
        ])
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttestationSubject {
    pub kind: AttestationSubjectKind,
    pub id: String,
    pub content_hashes: Vec<AttestationHashEntry>,
}

impl AttestationSubject {
    #[must_use]
    pub fn new(
        kind: AttestationSubjectKind,
        id: impl Into<String>,
        content_hashes: Vec<AttestationHashEntry>,
    ) -> Self {
        Self {
            kind,
            id: id.into(),
            content_hashes,
        }
    }

    #[must_use]
    fn canonical_json_value(&self) -> Value {
        let mut content_hashes = self.content_hashes.clone();
        content_hashes.sort();

        object([
            ("kind", Value::String(self.kind.as_str().to_owned())),
            ("id", Value::String(self.id.clone())),
            (
                "contentHashes",
                Value::Array(
                    content_hashes
                        .iter()
                        .map(AttestationHashEntry::canonical_json_value)
                        .collect(),
                ),
            ),
        ])
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttestationEvidenceRef {
    pub kind: String,
    pub id: String,
    pub schema: Option<String>,
    pub content_hash: Option<String>,
    pub provenance_uri: Option<String>,
    pub chain_hash: Option<String>,
}

impl AttestationEvidenceRef {
    #[must_use]
    pub fn new(kind: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
            schema: None,
            content_hash: None,
            provenance_uri: None,
            chain_hash: None,
        }
    }

    #[must_use]
    pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
        self.schema = Some(schema.into());
        self
    }

    #[must_use]
    pub fn with_content_hash(mut self, content_hash: impl Into<String>) -> Self {
        self.content_hash = Some(content_hash.into());
        self
    }

    #[must_use]
    pub fn with_provenance_uri(mut self, provenance_uri: impl Into<String>) -> Self {
        self.provenance_uri = Some(provenance_uri.into());
        self
    }

    #[must_use]
    pub fn with_chain_hash(mut self, chain_hash: impl Into<String>) -> Self {
        self.chain_hash = Some(chain_hash.into());
        self
    }

    #[must_use]
    fn canonical_json_value(&self) -> Value {
        let mut fields = serde_json::Map::new();
        insert_string(&mut fields, "kind", &self.kind);
        insert_string(&mut fields, "id", &self.id);
        insert_optional_string(&mut fields, "schema", self.schema.as_deref());
        insert_optional_string(&mut fields, "contentHash", self.content_hash.as_deref());
        insert_optional_string(&mut fields, "provenanceUri", self.provenance_uri.as_deref());
        insert_optional_string(&mut fields, "chainHash", self.chain_hash.as_deref());
        Value::Object(fields)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttestationEvidenceManifest {
    pub entries: Vec<AttestationEvidenceRef>,
}

impl AttestationEvidenceManifest {
    #[must_use]
    pub fn new(entries: Vec<AttestationEvidenceRef>) -> Self {
        Self { entries }
    }

    #[must_use]
    fn canonical_json_value(&self) -> Value {
        let mut entries = self.entries.clone();
        entries.sort();
        object([(
            "entries",
            Value::Array(
                entries
                    .iter()
                    .map(AttestationEvidenceRef::canonical_json_value)
                    .collect(),
            ),
        )])
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttestationRedactionEntry {
    pub field: String,
    pub class: String,
    pub action: String,
    pub reason: String,
    pub replacement_hash: Option<String>,
}

impl AttestationRedactionEntry {
    #[must_use]
    pub fn new(
        field: impl Into<String>,
        class: impl Into<String>,
        action: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            field: field.into(),
            class: class.into(),
            action: action.into(),
            reason: reason.into(),
            replacement_hash: None,
        }
    }

    #[must_use]
    pub fn with_replacement_hash(mut self, replacement_hash: impl Into<String>) -> Self {
        self.replacement_hash = Some(replacement_hash.into());
        self
    }

    #[must_use]
    fn canonical_json_value(&self) -> Value {
        let mut fields = serde_json::Map::new();
        insert_string(&mut fields, "field", &self.field);
        insert_string(&mut fields, "class", &self.class);
        insert_string(&mut fields, "action", &self.action);
        insert_string(&mut fields, "reason", &self.reason);
        insert_optional_string(
            &mut fields,
            "replacementHash",
            self.replacement_hash.as_deref(),
        );
        Value::Object(fields)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttestationRedactionManifest {
    pub policy: String,
    pub entries: Vec<AttestationRedactionEntry>,
}

impl AttestationRedactionManifest {
    #[must_use]
    pub fn new(policy: impl Into<String>, entries: Vec<AttestationRedactionEntry>) -> Self {
        Self {
            policy: policy.into(),
            entries,
        }
    }

    #[must_use]
    fn canonical_json_value(&self) -> Value {
        let mut entries = self.entries.clone();
        entries.sort();
        object([
            ("policy", Value::String(self.policy.clone())),
            (
                "entries",
                Value::Array(
                    entries
                        .iter()
                        .map(AttestationRedactionEntry::canonical_json_value)
                        .collect(),
                ),
            ),
        ])
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttestationHashManifest {
    pub entries: Vec<AttestationHashEntry>,
}

impl AttestationHashManifest {
    #[must_use]
    pub fn new(entries: Vec<AttestationHashEntry>) -> Self {
        Self { entries }
    }

    #[must_use]
    fn canonical_json_value(&self) -> Value {
        let mut entries = self.entries.clone();
        entries.sort();
        object([(
            "entries",
            Value::Array(
                entries
                    .iter()
                    .map(AttestationHashEntry::canonical_json_value)
                    .collect(),
            ),
        )])
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttestationOmission {
    pub field: String,
    pub reason: String,
}

impl AttestationOmission {
    #[must_use]
    pub fn new(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            reason: reason.into(),
        }
    }

    #[must_use]
    fn canonical_json_value(&self) -> Value {
        object([
            ("field", Value::String(self.field.clone())),
            ("reason", Value::String(self.reason.clone())),
        ])
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttestationTrustStatement {
    pub scope: String,
    pub statement: String,
}

impl AttestationTrustStatement {
    #[must_use]
    pub fn local_ee_custody() -> Self {
        Self {
            scope: "local_ee_custody".to_owned(),
            statement: ATTESTATION_LOCAL_TRUTH_STATEMENT.to_owned(),
        }
    }

    #[must_use]
    fn canonical_json_value(&self) -> Value {
        object([
            ("scope", Value::String(self.scope.clone())),
            ("statement", Value::String(self.statement.clone())),
        ])
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttestationBundle {
    pub schema: String,
    pub subject: AttestationSubject,
    pub evidence_manifest: AttestationEvidenceManifest,
    pub redaction_manifest: AttestationRedactionManifest,
    pub hash_manifest: AttestationHashManifest,
    pub omissions: Vec<AttestationOmission>,
    pub trust_statement: AttestationTrustStatement,
    /// Present only for sealed subjects (bd-2ea4a): the commit-reveal proof
    /// material, verifiable without database access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seal: Option<AttestationSeal>,
}

/// Offline commit-reveal evidence for a sealed memory (v2).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttestationSeal {
    /// Domain-separated blake3 commitment over the exact content bytes.
    pub content_commitment: String,
    pub sealed_at: String,
    pub revealed_at: Option<String>,
    /// `Some(true)` only after a reveal matched the commitment; failed
    /// attempts never set this (they live in the audit log).
    pub reveal_verified: Option<bool>,
}

/// Why persisted or externally constructed seal evidence cannot be emitted.
///
/// Variants intentionally carry no rejected values: validation errors may
/// cross a public error boundary and must not echo corrupt stored content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttestationSealValidationError {
    InvalidCommitment,
    InvalidSealedAt,
    InvalidRevealedAt,
    InvalidRevealState,
    RevealPrecedesSeal,
}

impl fmt::Display for AttestationSealValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidCommitment => "seal commitment is not canonical blake3 evidence",
            Self::InvalidSealedAt => "seal creation timestamp is not RFC 3339",
            Self::InvalidRevealedAt => "seal reveal timestamp is not RFC 3339",
            Self::InvalidRevealState => "seal reveal fields describe an impossible state",
            Self::RevealPrecedesSeal => "seal reveal timestamp precedes seal creation",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for AttestationSealValidationError {}

/// Validate the public commit-reveal evidence shared by storage and export.
///
/// A valid seal is either unrevealed (`revealed_at` and `reveal_verified` are
/// both absent) or is a verified reveal at or after the seal timestamp.
pub fn validate_attestation_seal_fields(
    content_commitment: &str,
    sealed_at: &str,
    revealed_at: Option<&str>,
    reveal_verified: Option<bool>,
) -> Result<(), AttestationSealValidationError> {
    crate::models::memory_seal::validate_memory_seal_commitment(content_commitment)
        .map_err(|_| AttestationSealValidationError::InvalidCommitment)?;
    let sealed_at = chrono::DateTime::parse_from_rfc3339(sealed_at)
        .map_err(|_| AttestationSealValidationError::InvalidSealedAt)?;
    let revealed_at = revealed_at
        .map(chrono::DateTime::parse_from_rfc3339)
        .transpose()
        .map_err(|_| AttestationSealValidationError::InvalidRevealedAt)?;

    match (revealed_at, reveal_verified) {
        (None, None) => Ok(()),
        (Some(revealed_at), Some(true)) if revealed_at >= sealed_at => Ok(()),
        (Some(_), Some(true)) => Err(AttestationSealValidationError::RevealPrecedesSeal),
        _ => Err(AttestationSealValidationError::InvalidRevealState),
    }
}

impl AttestationSeal {
    #[must_use]
    fn canonical_json_value(&self) -> Value {
        object([
            (
                "contentCommitment",
                Value::String(self.content_commitment.clone()),
            ),
            ("sealedAt", Value::String(self.sealed_at.clone())),
            (
                "revealedAt",
                self.revealed_at.clone().map_or(Value::Null, Value::String),
            ),
            (
                "revealVerified",
                self.reveal_verified.map_or(Value::Null, Value::Bool),
            ),
        ])
    }
}

impl AttestationBundle {
    #[must_use]
    pub fn new(
        subject: AttestationSubject,
        evidence_manifest: AttestationEvidenceManifest,
        redaction_manifest: AttestationRedactionManifest,
        hash_manifest: AttestationHashManifest,
    ) -> Self {
        Self {
            schema: ATTESTATION_BUNDLE_SCHEMA_V2.to_owned(),
            subject,
            evidence_manifest,
            redaction_manifest,
            hash_manifest,
            omissions: Vec::new(),
            trust_statement: AttestationTrustStatement::local_ee_custody(),
            seal: None,
        }
    }

    /// Attach the commit-reveal seal block (sealed subjects only).
    #[must_use]
    pub fn with_seal(mut self, seal: Option<AttestationSeal>) -> Self {
        self.seal = seal;
        self
    }

    #[must_use]
    pub fn with_omissions(mut self, omissions: Vec<AttestationOmission>) -> Self {
        self.omissions = omissions;
        self
    }

    #[must_use]
    pub fn with_trust_statement(mut self, trust_statement: AttestationTrustStatement) -> Self {
        self.trust_statement = trust_statement;
        self
    }

    #[must_use]
    pub fn canonical_json_value(&self) -> Value {
        let mut omissions = self.omissions.clone();
        omissions.sort();

        let mut fields = vec![
            ("schema", Value::String(self.schema.clone())),
            ("subject", self.subject.canonical_json_value()),
            (
                "evidenceManifest",
                self.evidence_manifest.canonical_json_value(),
            ),
            (
                "redactionManifest",
                self.redaction_manifest.canonical_json_value(),
            ),
            ("hashManifest", self.hash_manifest.canonical_json_value()),
            (
                "omissions",
                Value::Array(
                    omissions
                        .iter()
                        .map(AttestationOmission::canonical_json_value)
                        .collect(),
                ),
            ),
            (
                "trustStatement",
                self.trust_statement.canonical_json_value(),
            ),
        ];
        // v2: the seal block participates in the canonical bytes ONLY when
        // present, so unsealed bundles hash identically to their v1 shape
        // apart from the schema id.
        if let Some(seal) = &self.seal {
            fields.push(("seal", seal.canonical_json_value()));
        }
        object(fields)
    }

    #[must_use]
    pub fn canonical_json(&self) -> String {
        match serde_json::to_string(&self.canonical_json_value()) {
            Ok(serialized) => serialized,
            Err(error) => format!("{{\"serializationError\":\"{error}\"}}"),
        }
    }

    #[must_use]
    pub fn bundle_hash(&self) -> String {
        format!(
            "blake3:{}",
            blake3::hash(self.canonical_json().as_bytes()).to_hex()
        )
    }

    #[must_use]
    pub fn bundle_hash_entry(&self) -> AttestationHashEntry {
        AttestationHashEntry::blake3("attestation_bundle", self.bundle_hash())
    }
}

fn object(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    let mut object = serde_json::Map::new();
    for (key, value) in fields {
        object.insert(key.to_owned(), value);
    }
    Value::Object(object)
}

fn insert_string(fields: &mut serde_json::Map<String, Value>, key: &'static str, value: &str) {
    fields.insert(key.to_owned(), Value::String(value.to_owned()));
}

fn insert_optional_string(
    fields: &mut serde_json::Map<String, Value>,
    key: &'static str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        insert_string(fields, key, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle_with_reordered_entries(reverse: bool) -> AttestationBundle {
        let content_hashes = vec![
            AttestationHashEntry::blake3("body", "blake3:2222"),
            AttestationHashEntry::blake3("metadata", "blake3:1111"),
        ];
        let evidence = vec![
            AttestationEvidenceRef::new("audit", "audit-2")
                .with_content_hash("blake3:bbbb")
                .with_provenance_uri("audit://2"),
            AttestationEvidenceRef::new("memory", "mem-1")
                .with_schema("ee.memory.v1")
                .with_content_hash("blake3:aaaa")
                .with_chain_hash("blake3:cccc"),
        ];
        let redactions = vec![
            AttestationRedactionEntry::new("secret", "api_key", "hash", "redacted before export")
                .with_replacement_hash("blake3:dddd"),
            AttestationRedactionEntry::new(
                "body",
                "prompt_injection",
                "omit",
                "blocked by trust policy",
            ),
        ];
        let hashes = vec![
            AttestationHashEntry::blake3("evidence_manifest", "blake3:eeee"),
            AttestationHashEntry::blake3("redaction_manifest", "blake3:ffff"),
        ];
        let omissions = vec![
            AttestationOmission::new(
                "rawQuery",
                "query text is not stored in attestation bundles",
            ),
            AttestationOmission::new("secret", "redaction manifest records the omission"),
        ];

        let (content_hashes, evidence, redactions, hashes, omissions) = if reverse {
            (
                content_hashes.into_iter().rev().collect(),
                evidence.into_iter().rev().collect(),
                redactions.into_iter().rev().collect(),
                hashes.into_iter().rev().collect(),
                omissions.into_iter().rev().collect(),
            )
        } else {
            (content_hashes, evidence, redactions, hashes, omissions)
        };

        AttestationBundle::new(
            AttestationSubject::new(AttestationSubjectKind::Memory, "mem-1", content_hashes),
            AttestationEvidenceManifest::new(evidence),
            AttestationRedactionManifest::new("standard", redactions),
            AttestationHashManifest::new(hashes),
        )
        .with_omissions(omissions)
    }

    #[test]
    fn bundle_hash_is_order_stable() {
        let first = bundle_with_reordered_entries(false);
        let second = bundle_with_reordered_entries(true);

        assert_eq!(first.canonical_json(), second.canonical_json());
        assert_eq!(first.bundle_hash(), second.bundle_hash());
    }

    #[test]
    fn bundle_hash_changes_when_content_hash_changes() {
        let first = bundle_with_reordered_entries(false);
        let mut second = bundle_with_reordered_entries(false);
        second.subject.content_hashes[0].hash = "blake3:changed".to_owned();

        assert_ne!(first.bundle_hash(), second.bundle_hash());
    }

    #[test]
    fn subject_kind_parse_accepts_normalized_tokens() {
        assert_eq!(
            "curation-candidate".parse::<AttestationSubjectKind>(),
            Ok(AttestationSubjectKind::CurationCandidate)
        );
        assert_eq!(
            "CurationCandidate".parse::<AttestationSubjectKind>(),
            Ok(AttestationSubjectKind::CurationCandidate)
        );
    }

    #[test]
    fn canonical_json_carries_schema_and_trust_scope() {
        let bundle = bundle_with_reordered_entries(false);
        let value = bundle.canonical_json_value();

        assert_eq!(value["schema"], ATTESTATION_BUNDLE_SCHEMA_V2);
        assert_eq!(value["trustStatement"]["scope"], "local_ee_custody");
        assert!(
            value["trustStatement"]["statement"]
                .as_str()
                .is_some_and(|statement| statement.contains("does not attest objective truth"))
        );
        assert!(bundle.bundle_hash().starts_with("blake3:"));
    }

    #[test]
    fn seal_block_participates_in_canonical_bytes_only_when_present() {
        let unsealed = bundle_with_reordered_entries(false);
        let unsealed_value = unsealed.canonical_json_value();
        assert!(
            unsealed_value.get("seal").is_none(),
            "unsealed bundles must not carry a seal key"
        );

        let commitment = "blake3:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let sealed = unsealed.clone().with_seal(Some(AttestationSeal {
            content_commitment: commitment.to_owned(),
            sealed_at: "2026-08-10T00:00:00Z".to_owned(),
            revealed_at: None,
            reveal_verified: None,
        }));
        let sealed_value = sealed.canonical_json_value();
        assert_eq!(sealed_value["seal"]["contentCommitment"], commitment);
        assert_eq!(sealed_value["seal"]["revealedAt"], Value::Null);
        assert_ne!(
            unsealed.bundle_hash(),
            sealed.bundle_hash(),
            "the seal block must be hash-covered"
        );

        // Round trip: the serde shape parses back including the seal.
        let parsed: AttestationBundle =
            serde_json::from_str(&serde_json::to_string(&sealed).expect("serialize"))
                .expect("parse");
        assert_eq!(parsed, sealed);
        let reparsed_unsealed: AttestationBundle =
            serde_json::from_str(&serde_json::to_string(&unsealed).expect("serialize"))
                .expect("parse unsealed v2 payload without a seal key");
        assert_eq!(reparsed_unsealed.seal, None);
    }

    #[test]
    fn seal_validation_accepts_only_canonical_chronological_states_without_echoing_values() {
        let commitment = "blake3:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(
            validate_attestation_seal_fields(commitment, "2026-08-10T00:00:00Z", None, None),
            Ok(())
        );
        assert_eq!(
            validate_attestation_seal_fields(
                commitment,
                "2026-08-10T00:00:00Z",
                Some("2026-08-11T00:00:00+00:00"),
                Some(true)
            ),
            Ok(())
        );

        let hostile_commitment = format!("blake3:{}", "S".repeat(64));
        let cases = [
            validate_attestation_seal_fields(
                &hostile_commitment,
                "2026-08-10T00:00:00Z",
                None,
                None,
            ),
            validate_attestation_seal_fields(commitment, "PRIVATE_SEALED_AT", None, None),
            validate_attestation_seal_fields(
                commitment,
                "2026-08-10T00:00:00Z",
                Some("PRIVATE_REVEALED_AT"),
                Some(true),
            ),
            validate_attestation_seal_fields(
                commitment,
                "2026-08-10T00:00:00Z",
                Some("2026-08-11T00:00:00Z"),
                Some(false),
            ),
            validate_attestation_seal_fields(
                commitment,
                "2026-08-10T00:00:00Z",
                Some("2026-08-09T23:59:59Z"),
                Some(true),
            ),
        ];
        for result in cases {
            let error = result.expect_err("hostile seal evidence must fail validation");
            let rendered = error.to_string();
            assert!(!rendered.contains(&hostile_commitment));
            assert!(!rendered.contains("PRIVATE_SEALED_AT"));
            assert!(!rendered.contains("PRIVATE_REVEALED_AT"));
        }
    }
}
