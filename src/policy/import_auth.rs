//! Authenticated import artifacts (ADR 0086 TC-D14, plan P0.6, bead
//! `bd-tc-epic-qzk7o.2.4` slice 2).
//!
//! Builds on [`super::store_auth`] to authenticate `ee export` and
//! `ee playbook` artifacts so native trust (including `human_explicit`) is
//! *authenticated, not merely identified*. A spoofable `import_source=native`
//! header can no longer inject high-trust rows.
//!
//! Construction (TC-D14):
//!
//! * The exporter streams an ordered **records root** — a domain-separated
//!   BLAKE3 digest over length-prefixed `(ordinal, record_id,
//!   canonical_record_hash)` entries. Position is bound, so a reordered,
//!   duplicated, or truncated record set yields a different root.
//! * The exporter MACs a **constant-size canonical header** with the
//!   surface-specific subkey ([`MacDomain::NativeImportRecordsRoot`] for
//!   `ee export`, [`MacDomain::PlaybookImportRecordsRoot`] for playbooks). The
//!   header binds the artifact family, record-encoding version, source key
//!   namespace, exact workspace scope, key id, record count, and the records
//!   root.
//! * Only a minimal [`AuthenticatedHeader`] `{key_id, record_count,
//!   records_root, mac}` travels in the artifact. The binding *context* is
//!   **not transmitted**: both sides reconstruct it locally, so a header MAC'd
//!   for one workspace/surface/encoding cannot authenticate bytes under a
//!   different one — verification simply fails and native trust is refused.
//!
//! Verification recomputes the records root and count from the *actual*
//! received records and compares them to the MAC-authenticated header, so a
//! late mismatch near EOF still refuses native trust with zero privileged side
//! effects (the caller performs the recompute inside its own rollback
//! transaction). Store-unavailable faults propagate as [`StoreAuthError`],
//! whose degraded code is
//! [`super::store_auth::MESH_STORE_AUTHENTICATION_UNAVAILABLE_CODE`].

use serde::{Deserialize, Serialize};

use super::hex_lower;
use super::store_auth::{
    KeyClass, KeyId, KeyVerification, Mac, MacDomain, StoreAuthError, StoreAuthRoot,
};

/// Length of the records root / a canonical record hash, in bytes.
const ROOT_LEN: usize = 32;

/// Wire schema tag for the transmitted authentication block.
pub const NATIVE_IMPORT_AUTH_SCHEMA: &str = "ee.mesh.native_import_auth.v1";

/// Domain prefix folded into the records-root digest so a raw records root can
/// never be confused with an unrelated BLAKE3 digest of the same bytes.
const RECORDS_ROOT_DOMAIN: &[u8] = b"ee.mesh.import_auth.records_root.v1";
/// Domain prefix for the canonical header pre-image (defense in depth on top of
/// the domain-specific MAC subkey).
const CANONICAL_HEADER_DOMAIN: &[u8] = b"ee.mesh.import_auth.canonical_header.v1";

/// The binding context of an authenticated artifact. It is reconstructed
/// locally by exporter and importer and is bound into the MAC, never sent on
/// the wire, so mismatched context (wrong workspace, surface, or encoding)
/// fails verification instead of silently crossing scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArtifactContext<'a> {
    /// Artifact family, e.g. `ee.export.memories` vs `ee.playbook`.
    pub artifact_family: &'a str,
    /// Canonical record-encoding version.
    pub record_encoding_version: &'a str,
    /// Source store-key namespace tag.
    pub source_key_namespace: &'a str,
    /// Exact source workspace / scope identifier.
    pub workspace_scope: &'a str,
}

/// Ordered, position-binding digest over exported records.
///
/// Each `push` folds `(ordinal, record_id, canonical_record_hash)` with
/// length prefixes so no field boundary is ambiguous. `canonical_record_hash`
/// is the BLAKE3 of the exact emitted record bytes; the caller supplies it so
/// the root reflects precisely what was written (post-redaction, from one read
/// snapshot).
#[derive(Debug)]
pub struct RecordsRootBuilder {
    hasher: blake3::Hasher,
    count: u64,
}

impl Default for RecordsRootBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordsRootBuilder {
    /// Start an empty records-root digest.
    #[must_use]
    pub fn new() -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(RECORDS_ROOT_DOMAIN);
        Self { hasher, count: 0 }
    }

    /// Fold the next record at the current ordinal.
    pub fn push(&mut self, record_id: &str, canonical_record_hash: &[u8; ROOT_LEN]) {
        self.hasher.update(&self.count.to_le_bytes());
        let id = record_id.as_bytes();
        self.hasher.update(&(id.len() as u64).to_le_bytes());
        self.hasher.update(id);
        self.hasher.update(canonical_record_hash);
        self.count = self.count.saturating_add(1);
    }

    /// Number of records folded so far (the record count).
    #[must_use]
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Finalize the ordered records root.
    #[must_use]
    pub fn finalize(&self) -> [u8; ROOT_LEN] {
        *self.hasher.finalize().as_bytes()
    }
}

/// Convenience: BLAKE3 hash of a single record's exact emitted bytes.
#[must_use]
pub fn canonical_record_hash(record_bytes: &[u8]) -> [u8; ROOT_LEN] {
    *blake3::hash(record_bytes).as_bytes()
}

/// The minimal authentication block carried in an artifact. Context is bound
/// via the MAC and deliberately absent here.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticatedHeader {
    /// Wire schema tag; must equal [`NATIVE_IMPORT_AUTH_SCHEMA`].
    pub schema: String,
    /// Hex key id naming which store key authenticated the artifact.
    pub key_id: String,
    /// Number of records covered by `records_root`.
    pub record_count: u64,
    /// Hex ordered records root.
    pub records_root: String,
    /// Hex header MAC.
    pub mac: String,
}

/// Outcome of verifying an artifact's authentication block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportAuthOutcome {
    /// The artifact is authentic under the current or a windowed retired key.
    Authenticated { key_class: KeyClass },
    /// The recomputed records root / count disagree with the header.
    RecordsMismatch,
    /// The header MAC did not verify under its named key + local context.
    MacMismatch,
    /// The header's key id is outside the same-store verification window.
    KeyOutsideWindow,
    /// The authentication block schema tag was not recognized.
    SchemaMismatch,
    /// The authentication block could not be parsed (bad hex/length).
    Malformed,
}

impl ImportAuthOutcome {
    /// Whether native trust may be honored.
    #[must_use]
    pub fn is_authenticated(self) -> bool {
        matches!(self, Self::Authenticated { .. })
    }
}

/// Authenticate an artifact: MAC the canonical header under `domain` with the
/// store's current key and return the transmittable block.
pub fn authenticate_artifact(
    root: &StoreAuthRoot,
    domain: MacDomain,
    context: &ArtifactContext<'_>,
    records_root: &[u8; ROOT_LEN],
    record_count: u64,
) -> Result<AuthenticatedHeader, StoreAuthError> {
    let key_id = root.current_key_id();
    let preimage = canonical_header_bytes(context, key_id, record_count, records_root);
    let mac = root.mac(domain, &preimage)?;
    Ok(AuthenticatedHeader {
        schema: NATIVE_IMPORT_AUTH_SCHEMA.to_owned(),
        key_id: key_id.to_hex(),
        record_count,
        records_root: hex_lower(records_root),
        mac: mac.to_hex(),
    })
}

/// Verify an artifact's authentication block against the local context and the
/// records actually received. The caller supplies `recomputed_records_root` and
/// `recomputed_count` from a fresh pass over the received records (inside its
/// rollback transaction). Any disagreement fails closed.
pub fn verify_artifact(
    root: &StoreAuthRoot,
    domain: MacDomain,
    context: &ArtifactContext<'_>,
    header: &AuthenticatedHeader,
    recomputed_records_root: &[u8; ROOT_LEN],
    recomputed_count: u64,
) -> Result<ImportAuthOutcome, StoreAuthError> {
    if header.schema != NATIVE_IMPORT_AUTH_SCHEMA {
        return Ok(ImportAuthOutcome::SchemaMismatch);
    }
    let Ok(key_id) = KeyId::from_hex(&header.key_id) else {
        return Ok(ImportAuthOutcome::Malformed);
    };
    let Ok(claimed_root) = decode_root_hex(&header.records_root) else {
        return Ok(ImportAuthOutcome::Malformed);
    };
    let Ok(mac) = Mac::from_hex(&header.mac) else {
        return Ok(ImportAuthOutcome::Malformed);
    };

    // The MAC-authenticated header must agree with the records we actually saw.
    // Comparing the claimed root/count first turns a truncated or reordered
    // record set into a plain mismatch rather than a spurious MAC failure.
    if header.record_count != recomputed_count || &claimed_root != recomputed_records_root {
        return Ok(ImportAuthOutcome::RecordsMismatch);
    }

    let preimage = canonical_header_bytes(context, key_id, header.record_count, &claimed_root);
    let outcome = match root.verify_with_key(key_id, domain, &preimage, &mac)? {
        KeyVerification::Match { key_class } => ImportAuthOutcome::Authenticated { key_class },
        KeyVerification::Mismatch => ImportAuthOutcome::MacMismatch,
        KeyVerification::KeyOutsideWindow => ImportAuthOutcome::KeyOutsideWindow,
    };
    Ok(outcome)
}

/// Constant-order, length-prefixed canonical header pre-image. The fixed-width
/// `key_id`, `record_count`, and `records_root` trail the length-prefixed
/// context strings.
fn canonical_header_bytes(
    context: &ArtifactContext<'_>,
    key_id: KeyId,
    record_count: u64,
    records_root: &[u8; ROOT_LEN],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(CANONICAL_HEADER_DOMAIN);
    for field in [
        context.artifact_family,
        context.record_encoding_version,
        context.source_key_namespace,
        context.workspace_scope,
    ] {
        let bytes = field.as_bytes();
        out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(bytes);
    }
    out.extend_from_slice(key_id.as_bytes());
    out.extend_from_slice(&record_count.to_le_bytes());
    out.extend_from_slice(records_root);
    out
}

fn decode_root_hex(value: &str) -> Result<[u8; ROOT_LEN], ()> {
    let trimmed = value.trim();
    if trimmed.len() != ROOT_LEN * 2 {
        return Err(());
    }
    let bytes = trimmed.as_bytes();
    let mut out = [0_u8; ROOT_LEN];
    let mut index = 0;
    while index < ROOT_LEN {
        let high = hex_nibble(bytes[index * 2])?;
        let low = hex_nibble(bytes[index * 2 + 1])?;
        out[index] = (high << 4) | low;
        index += 1;
    }
    Ok(out)
}

fn hex_nibble(byte: u8) -> Result<u8, ()> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys_dir() -> tempfile::TempDir {
        tempfile::TempDir::new().expect("tempdir")
    }

    fn context() -> ArtifactContext<'static> {
        ArtifactContext {
            artifact_family: "ee.export.memories",
            record_encoding_version: "ee.export_record.v1",
            source_key_namespace: "store_ns_alpha",
            workspace_scope: "wsp_alpha",
        }
    }

    fn records_root_of(records: &[(&str, &str)]) -> ([u8; ROOT_LEN], u64) {
        let mut builder = RecordsRootBuilder::new();
        for &(id, content) in records {
            builder.push(id, &canonical_record_hash(content.as_bytes()));
        }
        (builder.finalize(), builder.count())
    }

    #[test]
    fn records_root_is_order_and_count_sensitive() {
        let (root_ab, count_ab) = records_root_of(&[("a", "one"), ("b", "two")]);
        let (root_ba, _) = records_root_of(&[("b", "two"), ("a", "one")]);
        let (root_a, count_a) = records_root_of(&[("a", "one")]);
        assert_ne!(root_ab, root_ba, "reordering must change the root");
        assert_ne!(root_ab, root_a, "dropping a record must change the root");
        assert_eq!(count_ab, 2);
        assert_eq!(count_a, 1);
    }

    #[test]
    fn authenticate_then_verify_round_trips() {
        let dir = keys_dir();
        let root = StoreAuthRoot::create(dir.path()).expect("create");
        let (records_root, count) = records_root_of(&[("a", "one"), ("b", "two")]);
        let header = authenticate_artifact(
            &root,
            MacDomain::NativeImportRecordsRoot,
            &context(),
            &records_root,
            count,
        )
        .expect("authenticate");
        assert_eq!(header.schema, NATIVE_IMPORT_AUTH_SCHEMA);
        assert_eq!(header.record_count, 2);

        let outcome = verify_artifact(
            &root,
            MacDomain::NativeImportRecordsRoot,
            &context(),
            &header,
            &records_root,
            count,
        )
        .expect("verify");
        assert_eq!(
            outcome,
            ImportAuthOutcome::Authenticated {
                key_class: KeyClass::Current
            }
        );
        assert!(outcome.is_authenticated());
    }

    #[test]
    fn tampered_records_are_a_records_mismatch() {
        let dir = keys_dir();
        let root = StoreAuthRoot::create(dir.path()).expect("create");
        let (records_root, count) = records_root_of(&[("a", "one")]);
        let header = authenticate_artifact(
            &root,
            MacDomain::NativeImportRecordsRoot,
            &context(),
            &records_root,
            count,
        )
        .expect("authenticate");

        let (tampered_root, tampered_count) = records_root_of(&[("a", "ONE-EDITED")]);
        let outcome = verify_artifact(
            &root,
            MacDomain::NativeImportRecordsRoot,
            &context(),
            &header,
            &tampered_root,
            tampered_count,
        )
        .expect("verify");
        assert_eq!(outcome, ImportAuthOutcome::RecordsMismatch);
    }

    #[test]
    fn foreign_workspace_scope_fails_the_mac() {
        let dir = keys_dir();
        let root = StoreAuthRoot::create(dir.path()).expect("create");
        let (records_root, count) = records_root_of(&[("a", "one")]);
        let header = authenticate_artifact(
            &root,
            MacDomain::NativeImportRecordsRoot,
            &context(),
            &records_root,
            count,
        )
        .expect("authenticate");

        let mut foreign = context();
        foreign.workspace_scope = "wsp_beta";
        let outcome = verify_artifact(
            &root,
            MacDomain::NativeImportRecordsRoot,
            &foreign,
            &header,
            &records_root,
            count,
        )
        .expect("verify");
        assert_eq!(
            outcome,
            ImportAuthOutcome::MacMismatch,
            "a different workspace scope must not authenticate"
        );
    }

    #[test]
    fn cross_surface_domain_fails_the_mac() {
        let dir = keys_dir();
        let root = StoreAuthRoot::create(dir.path()).expect("create");
        let (records_root, count) = records_root_of(&[("a", "one")]);
        // Authenticate as a native export...
        let header = authenticate_artifact(
            &root,
            MacDomain::NativeImportRecordsRoot,
            &context(),
            &records_root,
            count,
        )
        .expect("authenticate");
        // ...verify as a playbook: the surface subkey differs, so it fails.
        let outcome = verify_artifact(
            &root,
            MacDomain::PlaybookImportRecordsRoot,
            &context(),
            &header,
            &records_root,
            count,
        )
        .expect("verify");
        assert_eq!(outcome, ImportAuthOutcome::MacMismatch);
    }

    #[test]
    fn foreign_store_key_is_outside_the_window() {
        let dir_a = keys_dir();
        let dir_b = keys_dir();
        let root_a = StoreAuthRoot::create(dir_a.path()).expect("a");
        let root_b = StoreAuthRoot::create(dir_b.path()).expect("b");
        let (records_root, count) = records_root_of(&[("a", "one")]);
        let header = authenticate_artifact(
            &root_a,
            MacDomain::NativeImportRecordsRoot,
            &context(),
            &records_root,
            count,
        )
        .expect("authenticate");
        // Store B never minted key_a, so its key id is outside B's window.
        let outcome = verify_artifact(
            &root_b,
            MacDomain::NativeImportRecordsRoot,
            &context(),
            &header,
            &records_root,
            count,
        )
        .expect("verify");
        assert_eq!(outcome, ImportAuthOutcome::KeyOutsideWindow);
    }

    #[test]
    fn retired_key_still_authenticates_within_window() {
        let dir = keys_dir();
        let mut root = StoreAuthRoot::create(dir.path()).expect("create");
        let (records_root, count) = records_root_of(&[("a", "one")]);
        let header = authenticate_artifact(
            &root,
            MacDomain::NativeImportRecordsRoot,
            &context(),
            &records_root,
            count,
        )
        .expect("authenticate");
        root.rotate().expect("rotate");

        let outcome = verify_artifact(
            &root,
            MacDomain::NativeImportRecordsRoot,
            &context(),
            &header,
            &records_root,
            count,
        )
        .expect("verify");
        assert_eq!(
            outcome,
            ImportAuthOutcome::Authenticated {
                key_class: KeyClass::Retired
            }
        );
    }

    #[test]
    fn schema_mismatch_is_rejected() {
        let dir = keys_dir();
        let root = StoreAuthRoot::create(dir.path()).expect("create");
        let (records_root, count) = records_root_of(&[("a", "one")]);
        let mut header = authenticate_artifact(
            &root,
            MacDomain::NativeImportRecordsRoot,
            &context(),
            &records_root,
            count,
        )
        .expect("authenticate");
        header.schema = "ee.mesh.native_import_auth.v0".to_owned();
        let outcome = verify_artifact(
            &root,
            MacDomain::NativeImportRecordsRoot,
            &context(),
            &header,
            &records_root,
            count,
        )
        .expect("verify");
        assert_eq!(outcome, ImportAuthOutcome::SchemaMismatch);
    }

    #[test]
    fn malformed_mac_hex_is_rejected() {
        let dir = keys_dir();
        let root = StoreAuthRoot::create(dir.path()).expect("create");
        let (records_root, count) = records_root_of(&[("a", "one")]);
        let mut header = authenticate_artifact(
            &root,
            MacDomain::NativeImportRecordsRoot,
            &context(),
            &records_root,
            count,
        )
        .expect("authenticate");
        header.mac = "not-hex".to_owned();
        let outcome = verify_artifact(
            &root,
            MacDomain::NativeImportRecordsRoot,
            &context(),
            &header,
            &records_root,
            count,
        )
        .expect("verify");
        assert_eq!(outcome, ImportAuthOutcome::Malformed);
    }

    #[test]
    fn authenticated_header_serde_round_trips() {
        let dir = keys_dir();
        let root = StoreAuthRoot::create(dir.path()).expect("create");
        let (records_root, count) = records_root_of(&[("a", "one")]);
        let header = authenticate_artifact(
            &root,
            MacDomain::NativeImportRecordsRoot,
            &context(),
            &records_root,
            count,
        )
        .expect("authenticate");
        let json = serde_json::to_string(&header).expect("serialize");
        assert!(json.contains("recordsRoot"), "camelCase wire keys");
        let parsed: AuthenticatedHeader = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, header);
    }
}
