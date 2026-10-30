//! Private commitments to complete CASS source bytes.
//!
//! Public hashes beside a screened excerpt permit offline guesses of removed
//! text. New commitments MAC the transient complete-source digest under the
//! workspace authentication root. Neither that digest nor the root is retained
//! in the evidence, audit, checkpoint, or ordinary backup. Historical V1 claims
//! remain readable without rewriting the immutable evidence they describe.

use serde::{Deserialize, Serialize};

use super::store_auth::{KeyId, KeyVerification, Mac, MacDomain, StoreAuthError, StoreAuthRoot};

#[cfg(test)]
pub(crate) const CASS_SOURCE_COMMITMENT_SCHEMA_V2: &str = "ee.cass.source.v2";
// Match the bounded CASS view capture, not the smaller screening/excerpt budget.
const MAX_SOURCE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(crate) struct CassSourceIdentity<'a> {
    /// Stable session IDs preserve their original workspace scope on restore.
    /// The caller separately validates the current row/session workspace link.
    pub(crate) session_id: &'a str,
    /// The canonical BLAKE3 locator, never the raw source path.
    pub(crate) upstream_ref_hash: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CassSourceVerification {
    Match,
    Mismatch,
    /// Recover the original key; signing with a replacement cannot prove history.
    KeyUnavailable,
}

/// A claim about the complete source, distinct from the screened excerpt hash.
/// This is a strict tagged union so mixed versions, unknown fields and duplicate
/// members cannot silently acquire the semantics of either supported version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "schema", deny_unknown_fields)]
pub(crate) enum CassSourceCommitment {
    #[serde(rename = "ee.cass.source.v1")]
    Legacy {
        #[serde(rename = "contentHash")]
        content_hash: String,
        #[serde(rename = "byteLength")]
        byte_length: u64,
        #[serde(rename = "excerptProducerVersion")]
        excerpt_producer_version: u32,
    },
    #[serde(rename = "ee.cass.source.v2")]
    Keyed {
        #[serde(rename = "contentHash")]
        content_hash: String,
        #[serde(rename = "keyId")]
        key_id: String,
        #[serde(rename = "byteLength")]
        byte_length: u64,
        #[serde(rename = "excerptProducerVersion")]
        excerpt_producer_version: u32,
    },
}

fn canonical_hex(value: &str, bytes: usize) -> bool {
    value.len() == bytes * 2
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl CassSourceCommitment {
    pub(crate) fn is_valid(&self) -> bool {
        let (byte_length, version, hash_valid) = match self {
            Self::Legacy {
                content_hash,
                byte_length,
                excerpt_producer_version,
            } => (
                *byte_length,
                *excerpt_producer_version,
                content_hash
                    .strip_prefix("blake3:")
                    .is_some_and(|hash| canonical_hex(hash, 32)),
            ),
            Self::Keyed {
                content_hash,
                key_id,
                byte_length,
                excerpt_producer_version,
            } => (
                *byte_length,
                *excerpt_producer_version,
                canonical_hex(key_id, 16)
                    && content_hash
                        .strip_prefix("blake3-keyed:")
                        .is_some_and(|hash| canonical_hex(hash, 32)),
            ),
        };
        (1..=MAX_SOURCE_BYTES).contains(&byte_length) && version > 0 && hash_valid
    }

    pub(crate) fn byte_length(&self) -> u64 {
        match self {
            Self::Legacy { byte_length, .. } | Self::Keyed { byte_length, .. } => *byte_length,
        }
    }

    pub(crate) fn content_hash(&self) -> &str {
        match self {
            Self::Legacy { content_hash, .. } | Self::Keyed { content_hash, .. } => content_hash,
        }
    }

    pub(crate) fn is_keyed(&self) -> bool {
        matches!(self, Self::Keyed { .. })
    }

    pub(crate) fn capture(
        root: &StoreAuthRoot,
        identity: CassSourceIdentity<'_>,
        digest: &blake3::Hash,
        byte_length: u64,
        excerpt_producer_version: u32,
    ) -> Result<Self, StoreAuthError> {
        let mac = root.mac(
            MacDomain::CassSourceCommitment,
            &source_message(identity, digest, byte_length),
        )?;
        let source = Self::Keyed {
            content_hash: format!("blake3-keyed:{}", mac.to_hex()),
            key_id: root.current_key_id().to_hex(),
            byte_length,
            excerpt_producer_version,
        };
        if !source.is_valid() {
            return Err(StoreAuthError::Malformed {
                message: "invalid complete CASS source commitment".to_owned(),
            });
        }
        Ok(source)
    }

    pub(crate) fn verify(
        &self,
        root: Option<&StoreAuthRoot>,
        identity: CassSourceIdentity<'_>,
        digest: &blake3::Hash,
        byte_length: u64,
    ) -> Result<CassSourceVerification, StoreAuthError> {
        if !self.is_valid() || self.byte_length() != byte_length {
            return Ok(CassSourceVerification::Mismatch);
        }
        match self {
            Self::Legacy { content_hash, .. } => {
                Ok(if content_hash == &format!("blake3:{}", digest.to_hex()) {
                    CassSourceVerification::Match
                } else {
                    CassSourceVerification::Mismatch
                })
            }
            Self::Keyed {
                content_hash,
                key_id,
                ..
            } => {
                let Some(root) = root else {
                    return Ok(CassSourceVerification::KeyUnavailable);
                };
                let key = KeyId::from_hex(key_id)?;
                let mac = Mac::from_hex(content_hash.trim_start_matches("blake3-keyed:"))?;
                Ok(
                    match root.verify_with_key(
                        key,
                        MacDomain::CassSourceCommitment,
                        &source_message(identity, digest, byte_length),
                        &mac,
                    )? {
                        KeyVerification::Match { .. } => CassSourceVerification::Match,
                        KeyVerification::Mismatch => CassSourceVerification::Mismatch,
                        KeyVerification::KeyOutsideWindow => CassSourceVerification::KeyUnavailable,
                    },
                )
            }
        }
    }
}

fn source_message(
    identity: CassSourceIdentity<'_>,
    digest: &blake3::Hash,
    byte_length: u64,
) -> Vec<u8> {
    let mut message = b"ee.cass.source.v2\0".to_vec();
    for part in [identity.session_id, identity.upstream_ref_hash] {
        message.extend_from_slice(&(part.len() as u64).to_le_bytes());
        message.extend_from_slice(part.as_bytes());
    }
    message.extend_from_slice(&byte_length.to_le_bytes());
    message.extend_from_slice(digest.as_bytes());
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> CassSourceIdentity<'static> {
        CassSourceIdentity {
            session_id: "ses_fixture",
            upstream_ref_hash: "blake3:canonical-source-locator",
        }
    }

    #[test]
    fn private_source_proof_binds_every_byte_and_scope_without_public_digest() {
        let keys = tempfile::tempdir().expect("key directory");
        let root = StoreAuthRoot::create(keys.path()).expect("source key");
        let raw = "Release finished. contact=alice@example.test";
        let digest = blake3::hash(raw.as_bytes());
        let source = CassSourceCommitment::capture(&root, identity(), &digest, raw.len() as u64, 5)
            .expect("private commitment");
        let encoded = serde_json::to_string(&source).expect("serialize");
        assert!(!encoded.contains(&digest.to_hex().to_string()));
        assert!(encoded.contains(CASS_SOURCE_COMMITMENT_SCHEMA_V2));
        assert_eq!(
            source.verify(Some(&root), identity(), &digest, raw.len() as u64),
            Ok(CassSourceVerification::Match)
        );
        let changed = blake3::hash(b"Release finished. contact=clara@example.test");
        assert_eq!(
            source.verify(Some(&root), identity(), &changed, raw.len() as u64),
            Ok(CassSourceVerification::Mismatch)
        );
        for changed_scope in [
            CassSourceIdentity {
                session_id: "ses_other",
                ..identity()
            },
            CassSourceIdentity {
                upstream_ref_hash: "blake3:other-locator",
                ..identity()
            },
        ] {
            assert_eq!(
                source.verify(Some(&root), changed_scope, &digest, raw.len() as u64),
                Ok(CassSourceVerification::Mismatch)
            );
        }
        let other_keys = tempfile::tempdir().expect("other key directory");
        let other = StoreAuthRoot::create(other_keys.path()).expect("other root");
        let other_source =
            CassSourceCommitment::capture(&other, identity(), &digest, raw.len() as u64, 5)
                .expect("other private commitment");
        assert_ne!(source.content_hash(), other_source.content_hash());
        assert_eq!(
            source.verify(Some(&other), identity(), &digest, raw.len() as u64),
            Ok(CassSourceVerification::KeyUnavailable)
        );
    }

    #[test]
    fn rotation_window_verifies_history_and_missing_keys_never_become_proof() {
        let keys = tempfile::tempdir().expect("key directory");
        let mut root = StoreAuthRoot::create(keys.path()).expect("source key");
        let digest = blake3::hash(b"complete source");
        let source = CassSourceCommitment::capture(&root, identity(), &digest, 15, 5)
            .expect("private commitment");
        assert_eq!(
            source.verify(None, identity(), &digest, 15),
            Ok(CassSourceVerification::KeyUnavailable)
        );
        root.rotate().expect("rotate");
        assert_eq!(
            source.verify(Some(&root), identity(), &digest, 15),
            Ok(CassSourceVerification::Match)
        );
        for _ in 0..4 {
            root.rotate().expect("rotate beyond retained window");
        }
        assert_eq!(
            source.verify(Some(&root), identity(), &digest, 15),
            Ok(CassSourceVerification::KeyUnavailable)
        );
    }

    #[test]
    fn keyed_metadata_refuses_mixed_versions_duplicate_keys_and_noncanonical_claims() {
        let keys = tempfile::tempdir().expect("key directory");
        let root = StoreAuthRoot::create(keys.path()).expect("source key");
        let source =
            CassSourceCommitment::capture(&root, identity(), &blake3::hash(b"source"), 6, 5)
                .expect("private commitment");
        let value = serde_json::to_value(&source).expect("serialize");
        let valid = serde_json::json!({"cassSource": value}).to_string();
        assert_eq!(
            CassSourceCommitment::from_metadata(Some(&valid)).expect("valid metadata"),
            Some(source.clone())
        );
        for (field, invalid) in [
            ("schema", serde_json::json!("ee.cass.source.v1")),
            ("keyId", serde_json::Value::Null),
            ("keyId", serde_json::json!("A".repeat(32))),
            (
                "contentHash",
                serde_json::json!(format!("blake3:{}", blake3::hash(b"source").to_hex())),
            ),
            ("byteLength", serde_json::json!(0)),
            ("excerptProducerVersion", serde_json::json!(0)),
            ("extra", serde_json::json!("unrecognized")),
        ] {
            let mut changed = value.clone();
            changed[field] = invalid;
            let metadata = serde_json::json!({"cassSource": changed}).to_string();
            assert!(
                CassSourceCommitment::from_metadata(Some(&metadata)).is_err(),
                "{field}"
            );
        }
        for field in [
            "schema",
            "keyId",
            "contentHash",
            "byteLength",
            "excerptProducerVersion",
        ] {
            let duplicate = format!(
                "{{\"cassSource\":{{\"{field}\":{},{}",
                value[field],
                &serde_json::to_string(&source).expect("serialize")[1..]
            );
            let duplicate = format!("{duplicate}}}");
            assert!(
                CassSourceCommitment::from_metadata(Some(&duplicate)).is_err(),
                "duplicate {field}"
            );
        }
    }
}
