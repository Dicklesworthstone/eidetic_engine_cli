//! Builders for canonical provenance attestation bundles.
//!
//! These builders are read-only over the existing DB repositories. Any text
//! that could carry user content, queries, logs, or notes is redacted before
//! the bundle receives either a redacted value or a hash.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::db::{
    self, DbConnection, StoredAuditEntry, StoredMemory, StoredMemoryLink, StoredPackRecord,
    compute_memory_provenance_chain_hash, parse_stored_pack_ledger,
};
use crate::models::{
    AttestationBundle, AttestationEvidenceManifest, AttestationEvidenceRef, AttestationHashEntry,
    AttestationHashManifest, AttestationOmission, AttestationRedactionEntry,
    AttestationRedactionManifest, AttestationSubject, AttestationSubjectKind, StoredMemoryAnchor,
};
use crate::policy::redact_secret_like_content;

const ATTESTATION_REDACTION_POLICY: &str = "ee.policy.secret_redaction.v1";
const ATTESTATION_AUDIT_LIMIT: u32 = 64;
pub const ATTESTATION_SURFACE_MANIFEST_SCHEMA_V1: &str = "ee.attestation.surface_manifest.v1";

#[derive(Clone, Debug, Eq, PartialEq)]
struct RedactedText {
    content: String,
    hash: String,
    reason: String,
}

/// Build an attestation bundle for a stored memory, if the memory exists.
pub fn build_memory_attestation(
    connection: &DbConnection,
    memory_id: &str,
) -> db::Result<Option<AttestationBundle>> {
    let Some(memory) = connection.get_memory(memory_id)? else {
        return Ok(None);
    };
    let links = connection.list_memory_links_for_memory(memory_id, None)?;
    let anchors = connection.list_memory_anchors(memory_id)?;
    let audits = memory_attestation_audits(connection, memory_id, &memory.workspace_id)?;
    let seal = memory_attestation_seal(connection, &memory)?;

    Ok(Some(
        build_memory_attestation_from_parts(&memory, &links, &anchors, &audits)
            .with_seal(seal.as_ref().map(attestation_seal_from_row)),
    ))
}

fn memory_attestation_audits(
    connection: &DbConnection,
    memory_id: &str,
    workspace_id: &str,
) -> db::Result<Vec<StoredAuditEntry>> {
    connection.list_audit_by_workspace_target(
        workspace_id,
        "memory",
        memory_id,
        Some(crate::db::audit_actions::WHY_INSPECTED),
        ATTESTATION_AUDIT_LIMIT,
    )
}

#[cfg(test)]
fn filter_memory_attestation_audits(
    audits: impl IntoIterator<Item = StoredAuditEntry>,
    workspace_id: &str,
) -> Vec<StoredAuditEntry> {
    audits
        .into_iter()
        .filter(|entry| entry.workspace_id.as_deref() == Some(workspace_id))
        .filter(|entry| entry.action != crate::db::audit_actions::WHY_INSPECTED)
        .take(ATTESTATION_AUDIT_LIMIT as usize)
        .collect()
}

/// Build a memory attestation only when the loaded row belongs to the expected workspace.
pub fn build_memory_attestation_for_workspace(
    connection: &DbConnection,
    memory_id: &str,
    workspace_id: &str,
) -> db::Result<Option<AttestationBundle>> {
    let Some(memory) = connection.get_memory(memory_id)? else {
        return Ok(None);
    };
    if memory.workspace_id != workspace_id {
        return Ok(None);
    }
    let links = connection.list_memory_links_for_memory(memory_id, None)?;
    let anchors = connection.list_memory_anchors(memory_id)?;
    let audits = memory_attestation_audits(connection, memory_id, workspace_id)?;
    let seal = memory_attestation_seal(connection, &memory)?;
    Ok(Some(
        build_memory_attestation_from_parts(&memory, &links, &anchors, &audits)
            .with_seal(seal.as_ref().map(attestation_seal_from_row)),
    ))
}

/// Load the seal attached to this memory's immutable revision lineage.
///
/// A reveal keeps the commitment on the original row and publishes the exact
/// bytes as a new revision with the same `logical_id`. Attesting that live
/// revision must therefore carry the original seal rather than looking like an
/// unrelated unsealed memory.
fn memory_attestation_seal(
    connection: &DbConnection,
    memory: &StoredMemory,
) -> db::Result<Option<crate::models::MemorySeal>> {
    if let Some(seal) = connection.get_memory_seal(&memory.id)? {
        return Ok(Some(seal));
    }
    let Some(logical_id) = connection.get_memory_logical_id(&memory.id)? else {
        return Ok(None);
    };
    if logical_id == memory.id {
        return Ok(None);
    }
    let Some(root) = connection.get_memory(&logical_id)? else {
        return Ok(None);
    };
    if root.workspace_id != memory.workspace_id
        || connection.get_memory_logical_id(&root.id)?.as_deref() != Some(logical_id.as_str())
    {
        return Ok(None);
    }
    connection.get_memory_seal(&logical_id)
}

/// Project a stored seal row into the offline attestation block (bd-2ea4a).
fn attestation_seal_from_row(seal: &crate::models::MemorySeal) -> crate::models::AttestationSeal {
    crate::models::AttestationSeal {
        content_commitment: seal.content_commitment.clone(),
        sealed_at: seal.sealed_at.clone(),
        revealed_at: seal.revealed_at.clone(),
        reveal_verified: seal.reveal_verified,
    }
}

fn pack_attestation_audits(
    connection: &DbConnection,
    pack_id: &str,
    workspace_id: &str,
) -> db::Result<Vec<StoredAuditEntry>> {
    let pack = connection.list_audit_by_workspace_target(
        workspace_id,
        "pack",
        pack_id,
        None,
        ATTESTATION_AUDIT_LIMIT,
    )?;
    let pack_record = connection.list_audit_by_workspace_target(
        workspace_id,
        "pack_record",
        pack_id,
        None,
        ATTESTATION_AUDIT_LIMIT,
    )?;
    let mut audits = pack
        .into_iter()
        .chain(pack_record)
        .filter(|entry| entry.workspace_id.as_deref() == Some(workspace_id))
        .collect::<Vec<_>>();
    audits.sort_by(|left, right| {
        right
            .timestamp
            .cmp(&left.timestamp)
            .then_with(|| right.id.cmp(&left.id))
    });
    audits.truncate(ATTESTATION_AUDIT_LIMIT as usize);
    Ok(audits)
}

/// Build an attestation bundle for a stored pack record, if the pack exists.
pub fn build_pack_attestation(
    connection: &DbConnection,
    pack_id: &str,
) -> db::Result<Option<AttestationBundle>> {
    let Some(record) = connection.get_pack_record(pack_id)? else {
        return Ok(None);
    };
    let ledger = require_available_pack_attestation_ledger(&record)?;
    let audits = pack_attestation_audits(connection, pack_id, &record.workspace_id)?;

    Ok(Some(build_trusted_pack_attestation_from_parts(
        &record, &ledger, &audits,
    )))
}

/// Build a pack attestation from one checked row in the expected workspace.
pub fn build_pack_attestation_for_workspace(
    connection: &DbConnection,
    pack_id: &str,
    workspace_id: &str,
) -> db::Result<Option<AttestationBundle>> {
    let Some(record) = connection.get_pack_record(pack_id)? else {
        return Ok(None);
    };
    if record.workspace_id != workspace_id {
        return Ok(None);
    }
    build_pack_attestation_from_checked_record(connection, &record).map(Some)
}

/// Build from a caller-checked pack row without reloading or changing identity.
pub fn build_pack_attestation_from_checked_record(
    connection: &DbConnection,
    record: &StoredPackRecord,
) -> db::Result<AttestationBundle> {
    let ledger = require_available_pack_attestation_ledger(record)?;
    let audits = pack_attestation_audits(connection, &record.id, &record.workspace_id)?;
    Ok(build_trusted_pack_attestation_from_parts(
        record, &ledger, &audits,
    ))
}

fn require_available_pack_attestation_ledger(
    record: &StoredPackRecord,
) -> db::Result<db::ParsedPackLedger> {
    let ledger = parse_stored_pack_ledger(record);
    if ledger.status != db::PackLedgerStatus::Available {
        return Err(db::DbError::MalformedRow {
            operation: db::DbOperation::Query,
            message: format!(
                "pack attestation requires an available integrity-verified selection ledger; status={}",
                ledger.status.as_str()
            ),
        });
    }
    Ok(ledger)
}

/// Build an attestation bundle for a standalone query string.
#[must_use]
pub fn build_query_attestation(query_text: &str) -> AttestationBundle {
    let mut redactions = Vec::new();

    let query = redact_for_attestation("query.text", "query", query_text);
    push_redaction(&mut redactions, "query.text", "query", &query);

    let query_hash = query.hash.clone();
    let query_manifest_hash = value_hash(&json!({
        "queryHash": query_hash.as_str(),
        "redactionPolicy": ATTESTATION_REDACTION_POLICY,
    }));

    let evidence_entries = vec![
        AttestationEvidenceRef::new("query", query_hash.as_str())
            .with_schema("ee.query.attestation.v1")
            .with_content_hash(query_manifest_hash.as_str()),
    ];

    let subject = AttestationSubject::new(
        AttestationSubjectKind::Query,
        query_hash.as_str(),
        vec![
            AttestationHashEntry::blake3("query.redacted_text", query_hash.as_str()),
            AttestationHashEntry::blake3("query.manifest", query_manifest_hash.as_str()),
        ],
    );

    let hash_manifest = AttestationHashManifest::new(vec![
        AttestationHashEntry::blake3("query.redacted_text", query_hash),
        AttestationHashEntry::blake3("query.manifest", query_manifest_hash),
    ]);

    AttestationBundle::new(
        subject,
        AttestationEvidenceManifest::new(evidence_entries),
        AttestationRedactionManifest::new(ATTESTATION_REDACTION_POLICY, redactions),
        hash_manifest,
    )
    .with_omissions(query_omissions())
}

fn public_attestation_domain_hash(domain: &str, value: &str) -> String {
    format!(
        "blake3:{}",
        blake3::hash(format!("{domain}:{value}").as_bytes()).to_hex()
    )
}

fn public_attestation_evidence_id(kind: &str, id: &str) -> String {
    match kind {
        "memory" => crate::models::public_memory_id(id),
        "pack" | "pack_record" => crate::models::public_pack_id(id),
        "memory_link" => crate::models::public_memory_link_id(id),
        "audit" | "audit_log" => crate::models::public_audit_id(id),
        _ if crate::db::is_canonical_blake3_hash(id) => id.to_owned(),
        _ => public_attestation_domain_hash(&format!("attestation-evidence-id:{kind}"), id),
    }
}

fn public_attestation_subject_id(kind: AttestationSubjectKind, id: &str) -> String {
    match kind {
        AttestationSubjectKind::Pack => crate::models::public_pack_id(id),
        AttestationSubjectKind::Memory => crate::models::public_memory_id(id),
        AttestationSubjectKind::Query
        | AttestationSubjectKind::CurationCandidate
        | AttestationSubjectKind::Procedure
        | AttestationSubjectKind::Backup => {
            if crate::db::is_canonical_blake3_hash(id) {
                id.to_owned()
            } else {
                public_attestation_domain_hash("attestation-subject-id", id)
            }
        }
    }
}

/// Return the canonical redaction-safe bundle used by every public attestation surface.
#[must_use]
pub fn public_attestation_bundle(bundle: &AttestationBundle) -> AttestationBundle {
    let mut public = bundle.clone();
    let mut omitted_noncanonical_hash = false;
    let mut omitted_provenance_uri = false;
    public.schema = crate::models::attestation::ATTESTATION_BUNDLE_SCHEMA_V2.to_owned();
    public.subject.id = public_attestation_subject_id(public.subject.kind, &public.subject.id);
    public.subject.content_hashes.retain_mut(|entry| {
        entry.label = crate::policy::redact_public_replay_field("hashLabel", &entry.label).content;
        let canonical = crate::db::is_canonical_blake3_hash(&entry.hash);
        omitted_noncanonical_hash |= !canonical;
        if canonical {
            entry.algorithm = "blake3".to_owned();
        }
        canonical
    });
    for entry in &mut public.evidence_manifest.entries {
        let original_kind = entry.kind.clone();
        entry.id = public_attestation_evidence_id(&original_kind, &entry.id);
        entry.kind = crate::policy::redact_public_replay_field("kind", &entry.kind).content;
        entry.schema = entry
            .schema
            .as_deref()
            .map(|value| crate::policy::redact_public_replay_field("schema", value).content);
        omitted_provenance_uri |= entry.provenance_uri.is_some();
        entry.provenance_uri = None;
        for hash in [&mut entry.content_hash, &mut entry.chain_hash] {
            if hash
                .as_deref()
                .is_some_and(|value| !crate::db::is_canonical_blake3_hash(value))
            {
                *hash = None;
                omitted_noncanonical_hash = true;
            }
        }
    }
    for entry in &mut public.redaction_manifest.entries {
        entry.field = crate::policy::redact_public_replay_field("field", &entry.field).content;
        entry.class = crate::policy::redact_public_replay_field("class", &entry.class).content;
        entry.action = crate::policy::redact_public_replay_field("action", &entry.action).content;
        entry.reason = crate::policy::redact_public_replay_field("reason", &entry.reason).content;
        if entry
            .replacement_hash
            .as_deref()
            .is_some_and(|value| !crate::db::is_canonical_blake3_hash(value))
        {
            entry.replacement_hash = None;
            omitted_noncanonical_hash = true;
        }
    }
    public.redaction_manifest.policy =
        crate::policy::redact_public_replay_field("policy", &public.redaction_manifest.policy)
            .content;
    public.hash_manifest.entries.retain_mut(|entry| {
        entry.label = crate::policy::redact_public_replay_field("hashLabel", &entry.label).content;
        let canonical = crate::db::is_canonical_blake3_hash(&entry.hash);
        omitted_noncanonical_hash |= !canonical;
        if canonical {
            entry.algorithm = "blake3".to_owned();
        }
        canonical
    });
    for omission in &mut public.omissions {
        omission.field =
            crate::policy::redact_public_replay_field("field", &omission.field).content;
        omission.reason =
            crate::policy::redact_public_replay_field("reason", &omission.reason).content;
    }
    if omitted_noncanonical_hash
        && !public
            .omissions
            .iter()
            .any(|entry| entry.field == "publicProjection.nonCanonicalHashes")
    {
        public.omissions.push(AttestationOmission::new(
            "publicProjection.nonCanonicalHashes",
            "noncanonical hash values were omitted from the public attestation bundle",
        ));
    }
    if omitted_provenance_uri
        && !public
            .omissions
            .iter()
            .any(|entry| entry.field == "publicProjection.provenanceUri")
    {
        public.omissions.push(AttestationOmission::new(
            "publicProjection.provenanceUri",
            "provenance URIs are omitted from the public attestation bundle",
        ));
    }
    public.trust_statement.scope =
        crate::policy::redact_public_replay_field("scope", &public.trust_statement.scope).content;
    public.trust_statement.statement =
        crate::policy::redact_public_replay_field("statement", &public.trust_statement.statement)
            .content;
    public
}

/// Project a bundle into the compact manifest shape consumed by other surfaces.
///
/// The projection is intentionally hash- and count-heavy: it lets support
/// bundles, handoff capsules, pack replay, and why compare the same attested
/// subject without copying raw evidence text into those surfaces.
#[must_use]
pub fn attestation_surface_manifest(bundle: &AttestationBundle) -> Value {
    let public_bundle = public_attestation_bundle(bundle);
    let bundle = &public_bundle;
    let evidence_kinds = count_tokens(
        bundle
            .evidence_manifest
            .entries
            .iter()
            .map(|entry| entry.kind.as_str()),
    );
    let evidence_schemas = sorted_optional_tokens(
        bundle
            .evidence_manifest
            .entries
            .iter()
            .filter_map(|entry| entry.schema.as_deref()),
    );
    let evidence_content_hashes = sorted_optional_tokens(
        bundle
            .evidence_manifest
            .entries
            .iter()
            .filter_map(|entry| entry.content_hash.as_deref())
            .filter(|hash| crate::db::is_canonical_blake3_hash(hash)),
    );
    let evidence_chain_hashes = sorted_optional_tokens(
        bundle
            .evidence_manifest
            .entries
            .iter()
            .filter_map(|entry| entry.chain_hash.as_deref())
            .filter(|hash| crate::db::is_canonical_blake3_hash(hash)),
    );
    let subject_hash_labels = sorted_optional_tokens(
        bundle
            .subject
            .content_hashes
            .iter()
            .map(|entry| entry.label.as_str()),
    );
    let hash_labels = sorted_optional_tokens(
        bundle
            .hash_manifest
            .entries
            .iter()
            .map(|entry| entry.label.as_str()),
    );
    let hash_values = sorted_optional_tokens(
        bundle
            .hash_manifest
            .entries
            .iter()
            .map(|entry| entry.hash.as_str())
            .filter(|hash| crate::db::is_canonical_blake3_hash(hash)),
    );
    let redaction_fields = sorted_optional_tokens(
        bundle
            .redaction_manifest
            .entries
            .iter()
            .map(|entry| entry.field.as_str()),
    );
    let redaction_classes = sorted_optional_tokens(
        bundle
            .redaction_manifest
            .entries
            .iter()
            .map(|entry| entry.class.as_str()),
    );
    let redaction_replacement_hashes = sorted_optional_tokens(
        bundle
            .redaction_manifest
            .entries
            .iter()
            .filter_map(|entry| entry.replacement_hash.as_deref())
            .filter(|hash| crate::db::is_canonical_blake3_hash(hash)),
    );
    let omission_fields =
        sorted_optional_tokens(bundle.omissions.iter().map(|entry| entry.field.as_str()));
    let public_subject_id = bundle.subject.id.clone();

    json!({
        "schema": ATTESTATION_SURFACE_MANIFEST_SCHEMA_V1,
        "status": "available",
        "sourceSchema": &bundle.schema,
        "bundleHash": bundle.bundle_hash(),
        "subject": {
            "kind": bundle.subject.kind.as_str(),
            "id": public_subject_id,
            "contentHashCount": bundle.subject.content_hashes.len(),
            "contentHashLabels": subject_hash_labels,
        },
        "evidenceManifest": {
            "entryCount": bundle.evidence_manifest.entries.len(),
            "kinds": evidence_kinds,
            "schemas": evidence_schemas,
            "contentHashes": evidence_content_hashes,
            "chainHashes": evidence_chain_hashes,
            "provenanceUriIncluded": bundle
                .evidence_manifest
                .entries
                .iter()
                .any(|entry| entry.provenance_uri.is_some()),
        },
        "redactionManifest": {
            "policy": &bundle.redaction_manifest.policy,
            "entryCount": bundle.redaction_manifest.entries.len(),
            "fields": redaction_fields,
            "classes": redaction_classes,
            "replacementHashes": redaction_replacement_hashes,
        },
        "hashManifest": {
            "entryCount": bundle.hash_manifest.entries.len(),
            "labels": hash_labels,
            "hashes": hash_values,
        },
        "omissions": {
            "count": bundle.omissions.len(),
            "fields": omission_fields,
        },
        "trustStatement": {
            "scope": &bundle.trust_statement.scope,
        },
    })
}

fn count_tokens<'a>(tokens: impl Iterator<Item = &'a str>) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for token in tokens {
        *counts.entry(token.to_owned()).or_insert(0) += 1;
    }
    counts
}

fn sorted_optional_tokens<'a>(tokens: impl Iterator<Item = &'a str>) -> Vec<String> {
    tokens
        .filter(|token| !token.trim().is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Build a memory attestation from already-loaded DB rows.
#[must_use]
pub fn build_memory_attestation_from_parts(
    memory: &StoredMemory,
    links: &[StoredMemoryLink],
    anchors: &[StoredMemoryAnchor],
    audits: &[StoredAuditEntry],
) -> AttestationBundle {
    let mut redactions = Vec::new();

    let content = redact_for_attestation("memory.content", "memory_content", &memory.content);
    push_redaction(
        &mut redactions,
        "memory.content",
        "memory_content",
        &content,
    );

    let provenance_uri = memory.provenance_uri.as_deref().map(|uri| {
        let redacted = redact_for_attestation("memory.provenanceUri", "provenance_uri", uri);
        push_redaction(
            &mut redactions,
            "memory.provenanceUri",
            "provenance_uri",
            &redacted,
        );
        redacted.content
    });

    let verification_note_hash = memory.provenance_verification_note.as_deref().map(|note| {
        let redacted =
            redact_for_attestation("memory.provenanceVerificationNote", "provenance_note", note);
        push_redaction(
            &mut redactions,
            "memory.provenanceVerificationNote",
            "provenance_note",
            &redacted,
        );
        redacted.hash
    });

    let provenance_chain_hash = memory
        .provenance_chain_hash
        .clone()
        .unwrap_or_else(|| compute_memory_provenance_chain_hash(memory));

    let trust_validity_hash = value_hash(&json!({
        "confidence": format_score(memory.confidence),
        "importance": format_score(memory.importance),
        "provenanceChainHashVersion": memory.provenance_chain_hash_version,
        "provenanceVerificationNoteHash": verification_note_hash,
        "provenanceVerificationStatus": memory.provenance_verification_status,
        "provenanceVerifiedAt": memory.provenance_verified_at,
        "trustClass": memory.trust_class,
        "trustSubclass": memory.trust_subclass,
        "utility": format_score(memory.utility),
        "validFrom": memory.valid_from,
        "validTo": memory.valid_to,
    }));

    let mut evidence_entries = Vec::new();
    let mut memory_ref = AttestationEvidenceRef::new("memory", memory.id.as_str())
        .with_schema("ee.memory.v1")
        .with_content_hash(content.hash.as_str())
        .with_chain_hash(provenance_chain_hash.as_str());
    if let Some(uri) = provenance_uri {
        memory_ref = memory_ref.with_provenance_uri(uri);
    }
    evidence_entries.push(memory_ref);
    evidence_entries.extend(memory_link_evidence_refs(links, &mut redactions));
    evidence_entries.extend(memory_anchor_evidence_refs(anchors, &mut redactions));
    evidence_entries.extend(audit_evidence_refs(audits, &mut redactions));

    let link_manifest_hash = evidence_kind_manifest_hash(&evidence_entries, "memory_link");
    let anchor_manifest_hash = evidence_kind_manifest_hash(&evidence_entries, "memory_anchor");
    let audit_manifest_hash = evidence_kind_manifest_hash(&evidence_entries, "audit_log");

    let subject = AttestationSubject::new(
        AttestationSubjectKind::Memory,
        memory.id.as_str(),
        vec![
            AttestationHashEntry::blake3("memory.redacted_content", content.hash.as_str()),
            AttestationHashEntry::blake3("memory.provenance_chain", provenance_chain_hash.as_str()),
            AttestationHashEntry::blake3("memory.trust_validity", trust_validity_hash.as_str()),
        ],
    );

    let hash_manifest = AttestationHashManifest::new(vec![
        AttestationHashEntry::blake3("memory.redacted_content", content.hash),
        AttestationHashEntry::blake3("memory.provenance_chain", provenance_chain_hash),
        AttestationHashEntry::blake3("memory.trust_validity", trust_validity_hash),
        AttestationHashEntry::blake3("memory.links", link_manifest_hash),
        AttestationHashEntry::blake3("memory.anchors", anchor_manifest_hash),
        AttestationHashEntry::blake3("memory.audit", audit_manifest_hash),
    ]);

    AttestationBundle::new(
        subject,
        AttestationEvidenceManifest::new(evidence_entries),
        AttestationRedactionManifest::new(ATTESTATION_REDACTION_POLICY, redactions),
        hash_manifest,
    )
    .with_omissions(memory_omissions())
}

/// Build a pack attestation from already-loaded DB rows.
#[must_use]
pub fn build_pack_attestation_from_parts(
    record: &StoredPackRecord,
    audits: &[StoredAuditEntry],
) -> db::Result<AttestationBundle> {
    let ledger = require_available_pack_attestation_ledger(record)?;
    Ok(build_trusted_pack_attestation_from_parts(
        record, &ledger, audits,
    ))
}

fn build_trusted_pack_attestation_from_parts(
    record: &StoredPackRecord,
    ledger: &db::ParsedPackLedger,
    audits: &[StoredAuditEntry],
) -> AttestationBundle {
    let mut redactions = Vec::new();

    let query = redact_for_attestation("pack.query", "query", &record.query);
    push_redaction(&mut redactions, "pack.query", "query", &query);

    let degraded_hash = record.degraded_json.as_deref().map(|degraded| {
        let redacted = redact_for_attestation("pack.degradedJson", "degraded_json", degraded);
        push_redaction(
            &mut redactions,
            "pack.degradedJson",
            "degraded_json",
            &redacted,
        );
        redacted.hash
    });

    let created_by_hash = record.created_by.as_deref().map(|created_by| {
        let redacted = redact_for_attestation("pack.createdBy", "actor", created_by);
        push_redaction(&mut redactions, "pack.createdBy", "actor", &redacted);
        redacted.hash
    });

    let ledger_hash = record
        .ledger_hash
        .clone()
        .or_else(|| ledger.available_ledger().map(value_hash));

    let record_hash = value_hash(&json!({
        "createdAt": record.created_at,
        "createdByHash": created_by_hash,
        "degradedHash": degraded_hash,
        "itemCount": record.item_count,
        "ledgerHash": ledger_hash,
        "maxTokens": record.max_tokens,
        "omittedCount": record.omitted_count,
        "packHash": record.pack_hash,
        "profile": record.profile,
        "queryHash": query.hash,
        "usedTokens": record.used_tokens,
        "workspaceId": record.workspace_id,
    }));

    let mut evidence_entries = vec![
        AttestationEvidenceRef::new("pack_record", record.id.as_str())
            .with_schema("ee.pack_record.v1")
            .with_content_hash(record_hash.as_str()),
    ];
    evidence_entries.extend(pack_ledger_item_evidence_refs(record, ledger));
    evidence_entries.extend(audit_evidence_refs(audits, &mut redactions));

    let item_manifest_hash = evidence_kind_manifest_hash(&evidence_entries, "pack_item");
    let audit_manifest_hash = evidence_kind_manifest_hash(&evidence_entries, "audit_log");
    let ledger_manifest_hash = ledger_hash.clone().unwrap_or_else(|| {
        value_hash(&json!({
            "packId": record.id,
            "status": ledger.status.as_str(),
        }))
    });

    let subject = AttestationSubject::new(
        AttestationSubjectKind::Pack,
        record.id.as_str(),
        vec![
            AttestationHashEntry::blake3("pack.record", record_hash.as_str()),
            AttestationHashEntry::blake3("pack.redacted_query", query.hash.as_str()),
            AttestationHashEntry::blake3("pack.pack_hash", record.pack_hash.as_str()),
            AttestationHashEntry::blake3("pack.ledger", ledger_manifest_hash.as_str()),
        ],
    );

    let hash_manifest = AttestationHashManifest::new(vec![
        AttestationHashEntry::blake3("pack.record", record_hash),
        AttestationHashEntry::blake3("pack.redacted_query", query.hash),
        AttestationHashEntry::blake3("pack.pack_hash", record.pack_hash.clone()),
        AttestationHashEntry::blake3("pack.ledger", ledger_manifest_hash),
        AttestationHashEntry::blake3("pack.items", item_manifest_hash),
        AttestationHashEntry::blake3("pack.audit", audit_manifest_hash),
    ]);

    AttestationBundle::new(
        subject,
        AttestationEvidenceManifest::new(evidence_entries),
        AttestationRedactionManifest::new(ATTESTATION_REDACTION_POLICY, redactions),
        hash_manifest,
    )
    .with_omissions(pack_omissions())
}

fn memory_link_evidence_refs(
    links: &[StoredMemoryLink],
    redactions: &mut Vec<AttestationRedactionEntry>,
) -> Vec<AttestationEvidenceRef> {
    let mut refs = Vec::with_capacity(links.len());
    for link in links {
        let metadata_hash = link.metadata_json.as_deref().map(|metadata| {
            let redacted =
                redact_for_attestation("memoryLinks[].metadataJson", "metadata_json", metadata);
            push_redaction(
                redactions,
                "memoryLinks[].metadataJson",
                "metadata_json",
                &redacted,
            );
            redacted.hash
        });
        let created_by_hash = link.created_by.as_deref().map(|created_by| {
            let redacted = redact_for_attestation("memoryLinks[].createdBy", "actor", created_by);
            push_redaction(redactions, "memoryLinks[].createdBy", "actor", &redacted);
            redacted.hash
        });
        let row_hash = value_hash(&json!({
            "confidence": format_score(link.confidence),
            "createdAt": link.created_at,
            "createdByHash": created_by_hash,
            "directed": link.directed,
            "dstMemoryId": link.dst_memory_id,
            "evidenceCount": link.evidence_count,
            "lastReinforcedAt": link.last_reinforced_at,
            "metadataHash": metadata_hash,
            "relation": link.relation,
            "source": link.source,
            "srcMemoryId": link.src_memory_id,
            "weight": format_score(link.weight),
        }));
        refs.push(
            AttestationEvidenceRef::new("memory_link", link.id.as_str())
                .with_schema("ee.memory_link.v1")
                .with_content_hash(row_hash),
        );
    }
    refs
}

fn memory_anchor_evidence_refs(
    anchors: &[StoredMemoryAnchor],
    redactions: &mut Vec<AttestationRedactionEntry>,
) -> Vec<AttestationEvidenceRef> {
    let mut refs = Vec::with_capacity(anchors.len());
    for anchor in anchors {
        let redacted_value = redact_for_attestation(
            "memoryAnchors[].redactedAnchorValue",
            "redacted_anchor_value",
            &anchor.redacted_anchor_value,
        );
        push_redaction(
            redactions,
            "memoryAnchors[].redactedAnchorValue",
            "redacted_anchor_value",
            &redacted_value,
        );
        let provenance = redact_for_attestation(
            "memoryAnchors[].provenance",
            "provenance",
            &anchor.provenance,
        );
        push_redaction(
            redactions,
            "memoryAnchors[].provenance",
            "provenance",
            &provenance,
        );
        let row_hash = value_hash(&json!({
            "anchorKind": anchor.anchor_kind.as_str(),
            "anchorValueHash": anchor.anchor_value_hash,
            "capturedSpanHash": anchor.captured_span_hash,
            "confidence": format_score(anchor.confidence),
            "createdAt": anchor.created_at,
            "freshnessState": anchor.freshness_state.as_str(),
            "generation": anchor.generation,
            "provenanceHash": provenance.hash,
            "redactedAnchorValueHash": redacted_value.hash,
            "source": anchor.source.as_str(),
            "updatedAt": anchor.updated_at,
        }));
        refs.push(
            AttestationEvidenceRef::new(
                "memory_anchor",
                format!(
                    "{}:{}",
                    anchor.anchor_kind.as_str(),
                    anchor.anchor_value_hash
                ),
            )
            .with_schema("ee.memory_anchor.v1")
            .with_content_hash(row_hash),
        );
    }
    refs
}

fn pack_ledger_item_evidence_refs(
    record: &StoredPackRecord,
    ledger: &db::ParsedPackLedger,
) -> Vec<AttestationEvidenceRef> {
    let Some(ledger) = ledger.available_ledger() else {
        return Vec::new();
    };
    crate::db::pack_ledger_core_array(ledger, "selectedItems")
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let memory_id = item.get("memoryId").and_then(Value::as_str)?;
            Some(
                AttestationEvidenceRef::new("pack_item", format!("{}:{memory_id}", record.id))
                    .with_schema(crate::db::PACK_REPLAY_LEDGER_SCHEMA_V1)
                    .with_content_hash(value_hash(item)),
            )
        })
        .collect()
}

fn audit_evidence_refs(
    audits: &[StoredAuditEntry],
    redactions: &mut Vec<AttestationRedactionEntry>,
) -> Vec<AttestationEvidenceRef> {
    let mut refs = Vec::with_capacity(audits.len());
    for audit in audits {
        let actor_hash = audit.actor.as_deref().map(|actor| {
            let redacted = redact_for_attestation("audit.actor", "actor", actor);
            push_redaction(redactions, "audit.actor", "actor", &redacted);
            redacted.hash
        });
        let details_hash = audit.details.as_deref().map(|details| {
            let redacted = redact_for_attestation("audit.details", "audit_details", details);
            push_redaction(redactions, "audit.details", "audit_details", &redacted);
            redacted.hash
        });
        let row_hash = value_hash(&json!({
            "action": audit.action,
            "actorHash": actor_hash,
            "afterHash": audit.after_hash,
            "beforeHash": audit.before_hash,
            "detailsHash": details_hash,
            "mutationKind": audit.mutation_kind,
            "prevRowHash": audit.prev_row_hash,
            "surface": audit.surface,
            "targetId": audit.target_id,
            "targetType": audit.target_type,
            "timestamp": audit.timestamp,
            "workspaceId": audit.workspace_id,
        }));
        let mut audit_ref = AttestationEvidenceRef::new("audit_log", audit.id.as_str())
            .with_schema("ee.audit_log.v1")
            .with_content_hash(row_hash);
        if let Some(chain_hash) = &audit.this_row_hash {
            audit_ref = audit_ref.with_chain_hash(chain_hash.as_str());
        }
        refs.push(audit_ref);
    }
    refs
}

fn evidence_kind_manifest_hash(entries: &[AttestationEvidenceRef], kind: &str) -> String {
    let mut hashes: Vec<&str> = entries
        .iter()
        .filter(|entry| entry.kind == kind)
        .filter_map(|entry| entry.content_hash.as_deref())
        .collect();
    hashes.sort_unstable();
    value_hash(&json!(hashes))
}

fn redact_for_attestation(_field: &str, _class: &str, value: &str) -> RedactedText {
    let report = redact_secret_like_content(value);
    let reason = if report.redacted {
        format!(
            "redacted before attestation export: {}",
            report.redacted_reasons.join(", ")
        )
    } else {
        "raw text omitted; attestation stores a hash of the redacted value".to_owned()
    };
    RedactedText {
        hash: text_hash(&report.content),
        content: report.content,
        reason,
    }
}

fn push_redaction(
    entries: &mut Vec<AttestationRedactionEntry>,
    field: &str,
    class: &str,
    redacted: &RedactedText,
) {
    entries.push(
        AttestationRedactionEntry::new(
            field,
            class,
            "hash_redacted_text",
            redacted.reason.as_str(),
        )
        .with_replacement_hash(redacted.hash.as_str()),
    );
}

fn format_score(score: f32) -> String {
    format!("{score:.6}")
}

fn text_hash(value: &str) -> String {
    format!("blake3:{}", blake3::hash(value.as_bytes()).to_hex())
}

fn value_hash(value: &Value) -> String {
    // `Value`'s Display impl is the same serde encoder and is infallible
    // for in-memory JSON (finite numbers, string keys), so the attested
    // hash always covers the real serialized content.
    text_hash(&value.to_string())
}

fn memory_omissions() -> Vec<AttestationOmission> {
    vec![
        AttestationOmission::new(
            "memory.content",
            "raw memory content is omitted; only redacted hashes and DB row references are exported",
        ),
        AttestationOmission::new(
            "memory.provenanceVerificationNote",
            "free-text verification notes are represented by redacted hashes",
        ),
        AttestationOmission::new(
            "audit.details",
            "raw audit details are represented by redacted hashes",
        ),
        AttestationOmission::new(
            "memoryLinks[].metadataJson",
            "link metadata is represented by redacted hashes",
        ),
    ]
}

fn pack_omissions() -> Vec<AttestationOmission> {
    vec![
        AttestationOmission::new(
            "pack.query",
            "raw pack query text is omitted; only a redacted query hash is exported",
        ),
        AttestationOmission::new(
            "packItems[].why",
            "raw selection explanations are represented by redacted hashes",
        ),
        AttestationOmission::new(
            "packItems[].provenanceJson",
            "raw pack item provenance JSON is represented by redacted hashes",
        ),
        AttestationOmission::new(
            "audit.details",
            "raw audit details are represented by redacted hashes",
        ),
    ]
}

fn query_omissions() -> Vec<AttestationOmission> {
    vec![AttestationOmission::new(
        "query.text",
        "raw query text is omitted; only a redacted query hash is exported",
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored_memory_with_secret() -> StoredMemory {
        StoredMemory {
            id: "mem_attest_000000000000000001".to_owned(),
            workspace_id: "wsp_attest_000000000000000001".to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: "Set OPENAI_API_KEY=sk-123456789012345678901234567890 before retrying."
                .to_owned(),
            workflow_id: None,
            confidence: 0.75,
            utility: 0.5,
            importance: 0.6,
            provenance_uri: Some("file:///tmp/session.log".to_owned()),
            trust_class: "local".to_owned(),
            trust_subclass: Some("explicit".to_owned()),
            provenance_chain_hash: Some("blake3:existing-chain".to_owned()),
            provenance_chain_hash_version: "v1".to_owned(),
            provenance_verification_status: "verified".to_owned(),
            provenance_verified_at: Some("2026-06-07T00:00:00Z".to_owned()),
            provenance_verification_note: Some(
                "Operator confirmed token sk-123456789012345678901234567890.".to_owned(),
            ),
            created_at: "2026-06-07T00:00:00Z".to_owned(),
            updated_at: "2026-06-07T00:00:00Z".to_owned(),
            tombstoned_at: None,
            valid_from: Some("2026-06-07T00:00:00Z".to_owned()),
            valid_to: None,
        }
    }

    fn stored_pack_with_secret() -> StoredPackRecord {
        StoredPackRecord {
            id: "pack_attest_000000000000000001".to_owned(),
            workspace_id: "wsp_attest_000000000000000001".to_owned(),
            query: "fix release with token sk-123456789012345678901234567890".to_owned(),
            profile: "default".to_owned(),
            max_tokens: 4000,
            used_tokens: 1200,
            item_count: 1,
            omitted_count: 0,
            pack_hash: "blake3:pack-hash".to_owned(),
            degraded_json: None,
            ledger_json: None,
            ledger_hash: None,
            created_at: "2026-06-07T00:00:00Z".to_owned(),
            created_by: Some("IcyCat".to_owned()),
        }
    }

    fn trusted_pack_ledger_for_attestation() -> db::ParsedPackLedger {
        db::ParsedPackLedger::trusted_for_test(json!({
            "ledgerHash": "blake3:trusted-ledger",
            "selectedItems": [{
                "memoryId": "mem_attest_000000000000000001",
                "rank": 1,
                "section": "procedural",
                "estimatedTokens": 42,
                "scores": {"relevance": 0.8, "utility": 0.7},
                "why": {"hash": "blake3:redacted-why"},
                "provenance": {"hash": "blake3:redacted-provenance"}
            }]
        }))
    }

    fn stored_audit_with_secret() -> StoredAuditEntry {
        StoredAuditEntry {
            id: "aud_attest_000000000000000001".to_owned(),
            workspace_id: Some("wsp_attest_000000000000000001".to_owned()),
            timestamp: "2026-06-07T00:00:00Z".to_owned(),
            actor: Some("IcyCat".to_owned()),
            action: "memory.created".to_owned(),
            target_type: Some("memory".to_owned()),
            target_id: Some("mem_attest_000000000000000001".to_owned()),
            details: Some("{\"token\":\"sk-123456789012345678901234567890\"}".to_owned()),
            surface: "memory".to_owned(),
            mutation_kind: "memory.created".to_owned(),
            before_hash: None,
            after_hash: Some("blake3:after".to_owned()),
            prev_row_hash: Some("blake3:prev".to_owned()),
            this_row_hash: Some("blake3:this".to_owned()),
        }
    }

    #[test]
    fn memory_attestation_filters_why_audits_before_evidence_limit() {
        let mut audits = Vec::new();
        for index in 0..ATTESTATION_AUDIT_LIMIT {
            let mut audit = stored_audit_with_secret();
            audit.id = format!("aud_why_{index:024}");
            audit.action = crate::db::audit_actions::WHY_INSPECTED.to_owned();
            audit.mutation_kind = crate::db::audit_actions::WHY_INSPECTED.to_owned();
            audit.timestamp = format!("2026-06-07T00:00:00.{index:09}Z");
            audits.push(audit);
        }

        let mut mutation = stored_audit_with_secret();
        mutation.id = "aud_attest_real_mutation00000001".to_owned();
        mutation.action = "memory.updated".to_owned();
        mutation.mutation_kind = "memory.updated".to_owned();
        mutation.timestamp = "2026-06-06T00:00:00Z".to_owned();
        audits.push(mutation.clone());
        let mut cross_workspace = mutation.clone();
        cross_workspace.id = "aud_attest_cross_workspace000001".to_owned();
        cross_workspace.workspace_id = Some("wsp_attest_other000000000000001".to_owned());
        audits.push(cross_workspace);
        let mut unscoped = mutation.clone();
        unscoped.id = "aud_attest_unscoped000000000001".to_owned();
        unscoped.workspace_id = None;
        audits.push(unscoped);

        let filtered = filter_memory_attestation_audits(audits, "wsp_attest_000000000000000001");

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, mutation.id);
        assert_eq!(filtered[0].action, "memory.updated");
    }

    #[test]
    fn memory_attestation_scoped_builder_rejects_cross_workspace_without_public_egress()
    -> db::Result<()> {
        const SECRET: &str = "AKIAIOSFODNN7EXAMPLE";
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        let source_workspace_id = "wsp_00000000000000000000001011";
        connection.insert_workspace(
            source_workspace_id,
            &db::CreateWorkspaceInput {
                path: "/tmp/attest-source-workspace".to_owned(),
                name: Some("attest-source".to_owned()),
            },
        )?;
        let memory_id = format!("mem_{SECRET}000000");
        connection.insert_memory(
            &memory_id,
            &db::CreateMemoryInput {
                workspace_id: source_workspace_id.to_owned(),
                level: "procedural".to_owned(),
                kind: "rule".to_owned(),
                content: format!("private credential {SECRET}"),
                workflow_id: None,
                confidence: 0.8,
                utility: 0.7,
                importance: 0.6,
                provenance_uri: Some(format!("file:///private/{SECRET}")),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: None,
                valid_to: None,
            },
        )?;

        let attestation = build_memory_attestation_for_workspace(
            &connection,
            &memory_id,
            "wsp_00000000000000000000001012",
        )?;
        assert_eq!(
            attestation, None,
            "workspace mismatch must fail closed before loading public evidence"
        );
        let public_not_found = json!({
            "resource": "memory",
            "id": crate::models::public_memory_id(&memory_id),
            "bundle": attestation.map(|bundle| public_attestation_bundle(&bundle)),
        });
        let rendered = public_not_found.to_string();
        assert!(!rendered.contains(SECRET));
        assert!(!rendered.contains(&memory_id));
        assert!(public_not_found["bundle"].is_null());
        Ok(())
    }

    #[test]
    fn memory_attestation_omits_raw_content_and_is_deterministic() {
        let memory = stored_memory_with_secret();
        let audit = stored_audit_with_secret();
        let first =
            build_memory_attestation_from_parts(&memory, &[], &[], std::slice::from_ref(&audit));
        let second = build_memory_attestation_from_parts(&memory, &[], &[], &[audit]);

        assert_eq!(first.canonical_json(), second.canonical_json());
        let canonical = first.canonical_json();
        assert!(!canonical.contains("sk-123456789012345678901234567890"));
        assert!(canonical.contains("memory.content"));
        assert!(canonical.contains("memory.provenance_chain"));
        assert!(canonical.contains("audit_log"));
    }

    #[test]
    fn pack_attestation_hashes_query_and_integrity_bound_ledger_items() {
        let record = stored_pack_with_secret();
        let ledger = trusted_pack_ledger_for_attestation();
        let audit = stored_audit_with_secret();

        let mut bundle = build_trusted_pack_attestation_from_parts(&record, &ledger, &[audit]);
        bundle.evidence_manifest.entries.push(
            AttestationEvidenceRef::new("audit_log", "audit-hostile")
                .with_content_hash("credential-AKIAIOSFODNN7EXAMPLE")
                .with_chain_hash("credential-AKIAIOSFODNN7EXAMPLE"),
        );
        bundle.redaction_manifest.entries.push(
            AttestationRedactionEntry::new("audit", "secret", "hash", "fixture")
                .with_replacement_hash("credential-AKIAIOSFODNN7EXAMPLE"),
        );
        let canonical = bundle.canonical_json();

        assert!(!canonical.contains("sk-123456789012345678901234567890"));
        assert!(!canonical.contains("fix release with token"));
        assert!(canonical.contains("pack.redacted_query"));
        assert!(canonical.contains("pack_item"));
        assert!(canonical.contains(crate::db::PACK_REPLAY_LEDGER_SCHEMA_V1));
        assert!(canonical.contains("pack.query"));
    }

    #[test]
    fn public_pack_attestation_gate_rejects_unavailable_or_untrusted_ledgers() {
        let missing = stored_pack_with_secret();
        let missing_error = require_available_pack_attestation_ledger(&missing)
            .expect_err("missing ledger must not produce an available pack attestation");
        assert!(missing_error.to_string().contains("status=missing"));
        let missing_parts_error = build_pack_attestation_from_parts(&missing, &[])
            .expect_err("parts API must reject a missing selection ledger");
        assert!(missing_parts_error.to_string().contains("status=missing"));

        let mut oversized = stored_pack_with_secret();
        oversized.ledger_json = Some(crate::db::PACK_REPLAY_LEDGER_OVERSIZED_SENTINEL.to_owned());
        oversized.ledger_hash = Some("blake3:untrusted".to_owned());
        let oversized_error = require_available_pack_attestation_ledger(&oversized)
            .expect_err("oversized ledger must not produce an available pack attestation");
        assert!(oversized_error.to_string().contains("status=malformed"));
        let oversized_parts_error = build_pack_attestation_from_parts(&oversized, &[])
            .expect_err("parts API must reject an untrusted selection ledger");
        assert!(
            oversized_parts_error
                .to_string()
                .contains("status=malformed")
        );
    }

    #[test]
    fn attestation_surface_manifest_is_redaction_safe_and_deterministic() {
        let mut record = stored_pack_with_secret();
        record.id = "pack_AKIAIOSFODNN7EXAMPLE000000".to_owned();
        let ledger = trusted_pack_ledger_for_attestation();
        let mut audit = stored_audit_with_secret();
        audit.id = "audit_AKIAIOSFODNN7EXAMPLE00000".to_owned();
        audit.this_row_hash = Some("credential-AKIAIOSFODNN7EXAMPLE".to_owned());
        let mut bundle = build_trusted_pack_attestation_from_parts(&record, &ledger, &[audit]);
        bundle.evidence_manifest.entries.push(
            AttestationEvidenceRef::new("audit_log", "audit_AKIAIOSFODNN7EXAMPLE00000")
                .with_content_hash("credential-AKIAIOSFODNN7EXAMPLE")
                .with_chain_hash("credential-AKIAIOSFODNN7EXAMPLE")
                .with_provenance_uri("file:///private/AKIAIOSFODNN7EXAMPLE"),
        );
        let public = public_attestation_bundle(&bundle);
        assert_eq!(public_attestation_bundle(&public), public);
        assert!(!public.canonical_json().contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(
            public
                .evidence_manifest
                .entries
                .iter()
                .all(|entry| entry.provenance_uri.is_none())
        );

        let first = attestation_surface_manifest(&bundle);
        let second = attestation_surface_manifest(&bundle);
        assert_eq!(first, second);
        assert_eq!(first["schema"], ATTESTATION_SURFACE_MANIFEST_SCHEMA_V1);
        assert_eq!(first["status"], "available");
        assert_eq!(first["subject"]["kind"], "pack");
        assert_eq!(first["bundleHash"], public.bundle_hash());
        assert_eq!(
            first["redactionManifest"]["policy"],
            ATTESTATION_REDACTION_POLICY
        );
        assert!(
            first["hashManifest"]["hashes"]
                .as_array()
                .is_some_and(|hashes| !hashes.is_empty())
        );

        let rendered = first.to_string();
        assert!(!rendered.contains("sk-123456789012345678901234567890"));
        assert!(!rendered.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!rendered.contains("fix release with token"));
        assert!(!rendered.contains("Selected because"));
        for pointer in [
            "/evidenceManifest/contentHashes",
            "/evidenceManifest/chainHashes",
            "/redactionManifest/replacementHashes",
            "/hashManifest/hashes",
        ] {
            assert!(
                first
                    .pointer(pointer)
                    .and_then(Value::as_array)
                    .is_some_and(|hashes| hashes.iter().all(|hash| hash
                        .as_str()
                        .is_some_and(crate::db::is_canonical_blake3_hash))),
                "surface manifest filters noncanonical hashes at {pointer}"
            );
        }
    }

    #[test]
    fn query_attestation_hashes_query_without_exporting_raw_text() {
        let bundle =
            build_query_attestation("find release token sk-123456789012345678901234567890");
        let canonical = bundle.canonical_json();

        assert!(!canonical.contains("sk-123456789012345678901234567890"));
        assert!(!canonical.contains("find release token"));
        assert!(canonical.contains("query.redacted_text"));
        assert!(canonical.contains("query.text"));
        assert_eq!(bundle.subject.kind, AttestationSubjectKind::Query);
    }
}
