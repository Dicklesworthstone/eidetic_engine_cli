//! Requested creation guards share the memory's source transaction.

use crate::db::DbConnection;
use crate::models::{DomainError, MemorySeal, MemorySentinelPolarity, MemorySentinelSpec};

/// Caller-owned seal evidence, prepared once before creating its placeholder.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RememberSealInput<'a> {
    pub content_commitment: &'a str,
    pub sealed_at: &'a str,
}

/// Creation companions are deliberately separate from RememberMemoryOptions
/// so existing producers keep the same public request shape.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RememberCreationCompanions<'a> {
    pub sentinels: &'a [String],
    pub revive_when: &'a [String],
    pub stale_threshold_seconds: Option<u64>,
    pub seal: Option<RememberSealInput<'a>>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct PreparedRememberCreationCompanions {
    sentinel_specs: Vec<MemorySentinelSpec>,
    seal: Option<MemorySeal>,
}

impl RememberCreationCompanions<'_> {
    pub(super) fn is_empty(&self) -> bool {
        self.sentinels.is_empty() && self.revive_when.is_empty() && self.seal.is_none()
    }

    pub(super) fn validate_for_write(
        &self,
        options: &super::RememberMemoryOptions<'_>,
        controls: &super::RememberWriteControls<'_>,
    ) -> Result<(), DomainError> {
        if controls.reinforce && !self.is_empty() {
            return Err(super::remember_usage_error(
                "creation sentinels and seals cannot be combined with --reinforce".to_owned(),
            ));
        }
        if self.seal.is_some() {
            if controls.idempotency_key.is_some()
                || !self.sentinels.is_empty()
                || !self.revive_when.is_empty()
            {
                return Err(super::remember_usage_error(
                    "--seal cannot be combined with --idempotency-key or sentinels".to_owned(),
                ));
            }
            if options.content != crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT {
                return Err(super::remember_usage_error(
                    "sealed creation must store only the withheld-content placeholder".to_owned(),
                ));
            }
        }
        // Complete validation precedes store bootstrap and keyed replay. The
        // actual memory ID is bound separately during request preparation.
        self.prepare("mem_00000000000000000000000000")?;
        Ok(())
    }

    pub(super) fn prepare(
        &self,
        memory_id: &str,
    ) -> Result<PreparedRememberCreationCompanions, DomainError> {
        if self
            .stale_threshold_seconds
            .is_some_and(|seconds| seconds == 0 || i64::try_from(seconds).is_err())
        {
            return Err(super::remember_usage_error(
                "sentinel stale threshold must be positive and fit the database's signed 64-bit integer range".to_owned(),
            ));
        }
        let mut sentinel_specs = self
            .sentinels
            .iter()
            .map(|raw| (raw, MemorySentinelPolarity::Gate, "ee remember --sentinel"))
            .chain(self.revive_when.iter().map(|raw| {
                (
                    raw,
                    MemorySentinelPolarity::Revive,
                    "ee remember --revive-when",
                )
            }))
            .map(|(raw, polarity, provenance)| {
                MemorySentinelSpec::from_raw(
                    memory_id,
                    raw,
                    polarity,
                    None,
                    provenance,
                    self.stale_threshold_seconds,
                )
                .map_err(|error| DomainError::Usage {
                    message: error.to_string(),
                    repair: Some(error.repair().to_owned()),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        sentinel_specs.sort_by(|left, right| left.spec_hash.cmp(&right.spec_hash));
        sentinel_specs.dedup_by(|left, right| left.spec_hash == right.spec_hash);
        let seal = self
            .seal
            .map(|seal| {
                crate::models::validate_attestation_seal_fields(
                    seal.content_commitment,
                    seal.sealed_at,
                    None,
                    None,
                )
                .map_err(|_| {
                    super::remember_usage_error(
                        "invalid sealed-memory commitment or registration timestamp".to_owned(),
                    )
                })?;
                Ok::<_, DomainError>(MemorySeal {
                    memory_id: memory_id.to_owned(),
                    content_commitment: seal.content_commitment.to_owned(),
                    sealed_at: seal.sealed_at.to_owned(),
                    revealed_at: None,
                    reveal_verified: None,
                })
            })
            .transpose()?;
        Ok(PreparedRememberCreationCompanions {
            sentinel_specs,
            seal,
        })
    }

    /// Preserve old identities for ordinary writes; companion identities bind
    /// their canonical configuration, independent of flag order and new IDs.
    pub(super) fn request_hash(&self, original_hash: &str) -> Result<String, DomainError> {
        if self.is_empty() {
            return Ok(original_hash.to_owned());
        }
        if self.seal.is_some() {
            return Err(super::remember_usage_error(
                "--seal cannot be combined with --idempotency-key".to_owned(),
            ));
        }
        let prepared = self.prepare("mem_00000000000000000000000000")?;
        let mut configurations = prepared
            .sentinel_specs
            .iter()
            .map(|spec| {
                serde_json::json!({
                    "kind": spec.sentinel_kind.as_str(),
                    "polarity": spec.polarity.as_str(),
                    "target": &spec.target,
                    "expectedPredicate": &spec.expected_predicate,
                    "safetyClass": spec.safety_class.as_str(),
                    "provenance": &spec.provenance,
                    "staleThresholdSeconds": spec.stale_threshold_seconds,
                })
                .to_string()
            })
            .collect::<Vec<_>>();
        configurations.sort();
        configurations.dedup();
        let mut hash = blake3::Hasher::new();
        hash.update(b"ee.remember.creation_companions.v1\0");
        for field in std::iter::once(original_hash).chain(configurations.iter().map(String::as_str))
        {
            hash.update(&(field.len() as u64).to_le_bytes());
            hash.update(field.as_bytes());
        }
        Ok(format!("blake3:{}", hash.finalize().to_hex()))
    }
}

impl PreparedRememberCreationCompanions {
    pub(super) fn persist_in_txn(&self, connection: &DbConnection) -> crate::db::Result<()> {
        connection.upsert_memory_sentinel_specs(&self.sentinel_specs)?;
        if let Some(seal) = &self.seal {
            connection.insert_memory_seal(
                &seal.memory_id,
                &seal.content_commitment,
                &seal.sealed_at,
            )?;
        }
        Ok(())
    }

    /// Only immutable configuration proves the request. Sentinel evaluations
    /// and check timestamps may advance without changing creation identity.
    pub(super) fn matches_persisted(
        &self,
        connection: &DbConnection,
        memory_id: &str,
    ) -> crate::db::Result<bool> {
        let mut actual = connection.list_memory_sentinel_specs(memory_id)?;
        actual.sort_by(|left, right| left.spec_hash.cmp(&right.spec_hash));
        let specs_match = actual.len() == self.sentinel_specs.len()
            && actual
                .iter()
                .zip(&self.sentinel_specs)
                .all(|(actual, expected)| {
                    actual.spec_hash == expected.spec_hash
                        && actual.memory_id == expected.memory_id
                        && actual.sentinel_kind == expected.sentinel_kind
                        && actual.polarity == expected.polarity
                        && actual.target == expected.target
                        && actual.expected_predicate == expected.expected_predicate
                        && actual.safety_class == expected.safety_class
                        && actual.provenance == expected.provenance
                        && actual.stale_threshold_seconds == expected.stale_threshold_seconds
                });
        Ok(specs_match && connection.get_memory_seal(memory_id)? == self.seal)
    }
}
