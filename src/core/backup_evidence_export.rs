//! Privacy-preserving export projections must be fixed points under re-backup.
//!
//! Evidence kinds and recognized producer roles are schema vocabulary, not
//! prose. Redacting them makes the next restore uninsertable; rehashing already
//! opaque references changes history on every recovery. Neither operation is
//! needed to remove source text, and neither is allowed to confer admission.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{
    BackupCassEvidenceChunk, BackupCassEvidenceRecord, BackupDegradation, BackupDerivedAssetReport,
    MAX_DERIVED_ASSET_BYTES, RedactionLevel, StoredEvidenceSpan, hash_bytes,
    open_backup_artifact_for_read, redact_content, safe_artifact_path,
};

#[path = "backup_evidence_metadata.rs"]
mod metadata;

/// Exact persisted eligibility counts. These are not live admission verdicts:
/// source provenance and policy are still checked again by each reader.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupEvidenceEligibilityCounts {
    pub admitted: u64,
    pub quarantined: u64,
    pub denied: u64,
}

impl BackupEvidenceEligibilityCounts {
    fn observe(&mut self, eligibility: &str) -> Result<(), &'static str> {
        let count = match eligibility {
            "admitted" => &mut self.admitted,
            "quarantined" => &mut self.quarantined,
            "denied" => &mut self.denied,
            _ => return Err("unsupported evidence eligibility"),
        };
        *count = count.checked_add(1).ok_or("evidence count overflow")?;
        Ok(())
    }

    fn total(&self) -> Option<u64> {
        self.admitted
            .checked_add(self.quarantined)?
            .checked_add(self.denied)
    }

    fn can_be_exported_as(&self, exported: &Self, expected: u64) -> bool {
        self.total() == Some(expected)
            && exported.total() == Some(expected)
            && exported.admitted <= self.admitted
            && exported.quarantined <= self.quarantined
            && exported.denied >= self.denied
    }
}

/// Source and artifact histograms captured by the same export snapshot.
/// Redaction may withhold evidence but can never grant stronger eligibility.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupEvidenceAdmissionSummary {
    pub source_search: BackupEvidenceEligibilityCounts,
    pub exported_search: BackupEvidenceEligibilityCounts,
    pub source_pack: BackupEvidenceEligibilityCounts,
    pub exported_pack: BackupEvidenceEligibilityCounts,
}

impl BackupEvidenceAdmissionSummary {
    pub(super) fn observe(
        &mut self,
        source: &StoredEvidenceSpan,
        exported: &BackupCassEvidenceRecord,
    ) -> Result<(), &'static str> {
        self.source_search.observe(&source.search_eligibility)?;
        self.source_pack.observe(&source.pack_eligibility)?;
        self.exported_search.observe(&exported.search_eligibility)?;
        self.exported_pack.observe(&exported.pack_eligibility)
    }

    fn validate(&self, expected: u64) -> Result<(), &'static str> {
        if expected > i64::MAX as u64
            || !self
                .source_search
                .can_be_exported_as(&self.exported_search, expected)
            || !self
                .source_pack
                .can_be_exported_as(&self.exported_pack, expected)
        {
            return Err("backup evidence eligibility counts disagree with its source inventory");
        }
        Ok(())
    }

    pub(super) fn degradation(&self) -> Option<BackupDegradation> {
        let pack_lost = self
            .exported_pack
            .denied
            .saturating_sub(self.source_pack.denied);
        let search_lost = self
            .exported_search
            .denied
            .saturating_sub(self.source_search.denied);
        if pack_lost == 0 && search_lost == 0 {
            return None;
        }
        Some(BackupDegradation::warning(
            "backup_evidence_admission_reduced",
            format!(
                "Backup export changed {pack_lost} evidence span(s) to pack-denied and {search_lost} to search-denied. The artifact retains {} pack-admitted span(s) from {} in the source. Restore preserves these privacy denials; rebuilding an index cannot recover the original evidence eligibility.",
                self.exported_pack.admitted, self.source_pack.admitted,
            ),
            "For a private recovery copy without export redaction, run ee backup create --workspace . --redaction none; source provenance and policy checks still apply. Protect that unredacted artifact as source data.",
        ))
    }
}

/// Version 1 inventories predate this measurement. They carry no evidence
/// fidelity claim; version 2 must state both the source and artifact counts.
pub(super) fn summary_from_manifest(
    manifest: &serde_json::Value,
) -> Result<Option<BackupEvidenceAdmissionSummary>, &'static str> {
    let Some(inventory) = manifest.get("recoveryInventory") else {
        // Older manifests can still be integrity-checked. Restore separately
        // requires a complete durable inventory before writing anything.
        return Ok(None);
    };
    match inventory.get("schema").and_then(serde_json::Value::as_str) {
        Some("ee.backup.recovery_inventory.v1") => return Ok(None),
        Some("ee.backup.recovery_inventory.v2") => {}
        _ => return Err("backup has an unsupported recovery inventory"),
    }
    if inventory
        .get("rowCountBasis")
        .and_then(serde_json::Value::as_str)
        != Some("source_snapshot")
    {
        return Err("backup recovery counts have no source-snapshot basis");
    }
    let summary: BackupEvidenceAdmissionSummary = serde_json::from_value(
        inventory
            .get("evidenceAdmission")
            .cloned()
            .ok_or("backup has no evidence eligibility measurement")?,
    )
    .map_err(|_| "backup has malformed evidence eligibility counts")?;
    let tables = inventory
        .get("tables")
        .and_then(serde_json::Value::as_array)
        .ok_or("backup recovery inventory has no table array")?;
    let mut evidence = tables.iter().filter(|row| {
        row.get("table").and_then(serde_json::Value::as_str) == Some("evidence_spans")
    });
    let count = evidence
        .next()
        .and_then(|row| row.get("rowCount"))
        .and_then(serde_json::Value::as_u64)
        .ok_or("backup recovery inventory has no evidence count")?;
    if evidence.next().is_some() {
        return Err("backup recovery inventory repeats its evidence count");
    }
    summary.validate(count)?;
    Ok(Some(summary))
}

/// A manifest hash proves which claims were signed, not that those claims
/// describe its evidence. Check exact authenticated bytes and native rows.
pub(super) fn verify_summary_assets(
    backup_path: &Path,
    workspace_id: &str,
    assets: &[BackupDerivedAssetReport],
    summary: &BackupEvidenceAdmissionSummary,
) -> Result<(), &'static str> {
    let mut search = BackupEvidenceEligibilityCounts::default();
    let mut pack = BackupEvidenceEligibilityCounts::default();
    let mut identities = BTreeSet::new();
    let mut chunk_indices = BTreeSet::new();
    for asset in assets
        .iter()
        .filter(|asset| asset.kind == "cass_evidence_spans")
    {
        let path = safe_artifact_path(backup_path, &asset.path, &mut Vec::new())
            .ok_or("backup evidence path is unsafe")?;
        let file = open_backup_artifact_for_read(&path)
            .map_err(|_| "backup evidence asset cannot be read")?;
        let mut bytes = Vec::new();
        file.take(MAX_DERIVED_ASSET_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "backup evidence asset cannot be read")?;
        if bytes.len() as u64 > MAX_DERIVED_ASSET_BYTES
            || asset.byte_size != Some(bytes.len() as u64)
            || asset.hash.as_deref() != Some(hash_bytes(&bytes).as_str())
        {
            return Err("backup evidence bytes differ from the authenticated inventory");
        }
        let chunk: BackupCassEvidenceChunk = serde_json::from_slice(&bytes)
            .map_err(|_| "backup evidence asset has malformed rows")?;
        if chunk.schema != "ee.backup.derived.cass_evidence_spans.v1"
            || !chunk_indices.insert(chunk.chunk_index)
            || chunk.evidence_spans.is_empty()
            || chunk.evidence_spans.len() > super::CASS_BACKUP_CHUNK_ROWS
        {
            return Err("backup evidence chunks are incomplete or out of order");
        }
        for row in chunk.evidence_spans {
            if row.workspace_id != workspace_id || !identities.insert(row.id) {
                return Err("backup evidence contains foreign or duplicate identities");
            }
            search.observe(&row.search_eligibility)?;
            pack.observe(&row.pack_eligibility)?;
        }
    }
    if chunk_indices
        .into_iter()
        .enumerate()
        .any(|(index, actual)| usize::try_from(actual).ok() != Some(index))
    {
        return Err("backup evidence chunks are incomplete or out of order");
    }
    if search != summary.exported_search || pack != summary.exported_pack {
        return Err("backup evidence eligibility does not match its artifact summary");
    }
    Ok(())
}

fn canonical_digest(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        suffix.len() == 64
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn opaque_reference(value: &str) -> String {
    if canonical_digest(value, "blake3:") {
        value.to_owned()
    } else {
        hash_bytes(value.as_bytes())
    }
}

/// Identity keys are not prose. Preserve the precise opaque format emitted by
/// this projection, never an arbitrary string merely beginning with `key_`.
pub(super) fn redact_identity(key: &str, level: RedactionLevel) -> String {
    crate::output::jsonl_export::redact_recovery_identity(key, level)
}

fn export_reference(value: &str, level: RedactionLevel, admitted: bool) -> String {
    if canonical_digest(value, "blake3:") || (admitted && redact_content(value, level) == value) {
        value.to_owned()
    } else {
        opaque_reference(value)
    }
}

fn structural_role(role: &str) -> bool {
    matches!(
        role,
        "user"
            | "assistant"
            | "system"
            | "developer"
            | "tool"
            | "unknown"
            | "agentsmd_import"
            | "docs_bootstrap"
            | "journal_distill"
            | "reinforcement"
    )
}

pub(super) fn redact_evidence(
    row: &mut BackupCassEvidenceRecord,
    level: RedactionLevel,
    provenance_admitted: bool,
) {
    // An explicit unredacted backup retains its original recovery contract.
    if level == RedactionLevel::None {
        return;
    }
    let cass_span_id = export_reference(&row.cass_span_id, level, provenance_admitted);
    let upstream_ref_hash = row
        .upstream_ref_hash
        .as_deref()
        .map(|value| export_reference(value, level, provenance_admitted));
    let redact_role = row.role.as_deref().is_some_and(|role| {
        !structural_role(role) && (!provenance_admitted || redact_content(role, level) != role)
    });
    let redact_metadata = row
        .metadata_json
        .as_deref()
        .is_some_and(|raw| !metadata::safe_to_retain(raw, level));
    let auxiliary_changed = cass_span_id != row.cass_span_id
        || upstream_ref_hash != row.upstream_ref_hash
        || redact_role
        || redact_metadata;
    row.cass_span_id = cass_span_id;
    row.upstream_ref_hash = upstream_ref_hash;
    if redact_role {
        row.role = None;
    }
    let excerpt = redact_content(
        &row.excerpt,
        if provenance_admitted && !auxiliary_changed {
            level
        } else {
            RedactionLevel::Full
        },
    );
    if excerpt != row.excerpt || !provenance_admitted || auxiliary_changed {
        row.cass_span_id = opaque_reference(&row.cass_span_id);
        row.upstream_ref_hash = row.upstream_ref_hash.as_deref().map(opaque_reference);
        if row
            .role
            .as_deref()
            .is_some_and(|role| !structural_role(role))
        {
            row.role = None;
        }
        row.excerpt = excerpt;
        row.content_hash = hash_bytes(row.excerpt.as_bytes());
        row.canonical_excerpt_hash = None;
        row.canonical_provenance_revision = 0;
        row.security_policy_epoch = 0;
        row.metadata_json = None;
        row.secret_redaction_status = "redacted".to_owned();
        row.redaction_classes_json = "[\"backup_redaction\"]".to_owned();
        row.search_eligibility = "denied".to_owned();
        row.pack_eligibility = "denied".to_owned();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_admission_summary_requires_exact_counts_and_cannot_claim_promotion() {
        let source = BackupEvidenceEligibilityCounts {
            admitted: 2,
            quarantined: 1,
            denied: 1,
        };
        let exported = BackupEvidenceEligibilityCounts {
            admitted: 1,
            quarantined: 0,
            denied: 3,
        };
        let summary = BackupEvidenceAdmissionSummary {
            source_search: source.clone(),
            exported_search: exported.clone(),
            source_pack: source,
            exported_pack: exported,
        };
        let original = serde_json::json!({"recoveryInventory": {
            "schema": "ee.backup.recovery_inventory.v2",
            "rowCountBasis": "source_snapshot",
            "evidenceAdmission": summary,
            "tables": [{"table": "evidence_spans", "rowCount": 4}],
        }});
        assert_eq!(summary_from_manifest(&original), Ok(Some(summary)));
        for bad_counts in [
            serde_json::json!({"admitted": 3, "quarantined": 0, "denied": 1}),
            serde_json::json!({"admitted": 1, "quarantined": 0, "denied": 2}),
            serde_json::json!({"admitted": 1, "denied": 3}),
            serde_json::json!({"admitted": -1, "quarantined": 1, "denied": 4}),
            serde_json::json!({"admitted": 1.5, "quarantined": 0, "denied": 2.5}),
            serde_json::json!({"admitted": u64::MAX, "quarantined": 2, "denied": 3}),
            serde_json::json!({"admitted": 1, "quarantined": 0, "denied": 3, "unknown": 0}),
        ] {
            for field in ["exportedSearch", "exportedPack"] {
                let mut manifest = original.clone();
                manifest["recoveryInventory"]["evidenceAdmission"][field] = bad_counts.clone();
                assert!(
                    summary_from_manifest(&manifest).is_err(),
                    "{field}: {bad_counts}"
                );
            }
        }
        for basis in [serde_json::Value::Null, serde_json::json!("artifact")] {
            let mut manifest = original.clone();
            manifest["recoveryInventory"]["rowCountBasis"] = basis;
            assert!(summary_from_manifest(&manifest).is_err());
        }
        let mut manifest = original.clone();
        manifest["recoveryInventory"]["tables"] = serde_json::json!([
            {"table": "evidence_spans", "rowCount": 4},
            {"table": "evidence_spans", "rowCount": 4},
        ]);
        assert!(summary_from_manifest(&manifest).is_err());
        manifest = original;
        manifest["recoveryInventory"]["evidenceAdmission"] = serde_json::Value::Null;
        assert!(summary_from_manifest(&manifest).is_err());
        manifest["recoveryInventory"]["schema"] =
            serde_json::json!("ee.backup.recovery_inventory.v1");
        assert_eq!(summary_from_manifest(&manifest), Ok(None));
    }

    #[test]
    fn evidence_admission_empty_and_preserved_histograms_have_no_loss_warning() {
        let empty = BackupEvidenceAdmissionSummary::default();
        assert_eq!(empty.validate(0), Ok(()));
        assert!(empty.degradation().is_none());
        let counts = BackupEvidenceEligibilityCounts {
            admitted: 2,
            quarantined: 1,
            denied: 3,
        };
        let unchanged = BackupEvidenceAdmissionSummary {
            source_search: counts.clone(),
            exported_search: counts.clone(),
            source_pack: counts.clone(),
            exported_pack: counts,
        };
        assert_eq!(unchanged.validate(6), Ok(()));
        assert!(unchanged.degradation().is_none());
        assert!(unchanged.validate(5).is_err());
        assert!(unchanged.validate(u64::MAX).is_err());
    }

    fn row() -> BackupCassEvidenceRecord {
        let excerpt = "The release tests completed successfully.";
        BackupCassEvidenceRecord {
            id: "ev_fixture".to_owned(),
            workspace_id: "ws_fixture".to_owned(),
            session_id: "ses_fixture".to_owned(),
            memory_id: None,
            cass_span_id: hash_bytes(b"upstream reference"),
            span_kind: "message".to_owned(),
            start_line: 1,
            end_line: 2,
            start_byte: None,
            end_byte: None,
            role: Some("assistant".to_owned()),
            excerpt: excerpt.to_owned(),
            content_hash: hash_bytes(excerpt.as_bytes()),
            metadata_json: Some("{}".to_owned()),
            producer_kind: "cass_import".to_owned(),
            screening_version: 1,
            secret_redaction_status: "clean".to_owned(),
            redaction_classes_json: "[]".to_owned(),
            instruction_risk: "none".to_owned(),
            search_eligibility: "admitted".to_owned(),
            pack_eligibility: "admitted".to_owned(),
            canonical_provenance_revision: 1,
            canonical_excerpt_hash: Some(hash_bytes(excerpt.as_bytes())),
            security_policy_epoch: 1,
            upstream_ref_hash: Some(hash_bytes(b"upstream reference")),
            created_at: "2026-09-01T00:00:00Z".to_owned(),
            updated_at: "2026-09-01T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn redacted_evidence_is_a_fixed_point_and_never_regains_admission() {
        for level in [RedactionLevel::Paranoid, RedactionLevel::Full] {
            let mut value = row();
            redact_evidence(&mut value, level, true);
            let first = value.clone();
            for next in [level, RedactionLevel::Standard, RedactionLevel::None] {
                redact_evidence(&mut value, next, false);
                assert_eq!(value, first);
                assert_eq!(value.pack_eligibility, "denied");
                assert_eq!(value.search_eligibility, "denied");
                assert!(value.canonical_excerpt_hash.is_none());
                assert_eq!(value.security_policy_epoch, 0);
            }
        }
    }

    #[test]
    fn denied_evidence_preserves_all_schema_kinds_and_recognized_roles() {
        for kind in ["message", "tool_call", "tool_result", "file", "summary"] {
            for role in [
                "user",
                "assistant",
                "system",
                "developer",
                "tool",
                "unknown",
                "agentsmd_import",
                "docs_bootstrap",
                "journal_distill",
                "reinforcement",
            ] {
                let mut value = row();
                value.span_kind = kind.to_owned();
                value.role = Some(role.to_owned());
                redact_evidence(&mut value, RedactionLevel::Full, false);
                assert_eq!(value.span_kind, kind);
                assert_eq!(value.role.as_deref(), Some(role));
                assert_eq!(value.pack_eligibility, "denied");
            }
        }
    }

    #[test]
    fn unchecked_legacy_text_and_references_are_removed_once() {
        for level in [
            RedactionLevel::Minimal,
            RedactionLevel::Standard,
            RedactionLevel::Strict,
            RedactionLevel::Paranoid,
            RedactionLevel::Full,
        ] {
            let mut value = row();
            value.role = Some("api_key=PRIVATE_SENTINEL".to_owned());
            value.cass_span_id = "/Users/private/PRIVATE_SENTINEL".to_owned();
            value.upstream_ref_hash = Some("PRIVATE_SENTINEL".to_owned());
            value.excerpt = "PRIVATE_SENTINEL".to_owned();
            redact_evidence(&mut value, level, false);
            assert_eq!(value.role, None);
            assert_ne!(value.excerpt, "PRIVATE_SENTINEL");
            assert_eq!(
                value.cass_span_id,
                hash_bytes(b"/Users/private/PRIVATE_SENTINEL")
            );
            assert_eq!(
                value.upstream_ref_hash,
                Some(hash_bytes(b"PRIVATE_SENTINEL"))
            );
            assert!(value.metadata_json.is_none());
            let first = value.clone();
            redact_evidence(&mut value, level, false);
            assert_eq!(value, first);
        }
    }

    #[test]
    fn malformed_opaque_lookalikes_are_not_a_redaction_bypass() {
        for value in ["blake3:secret", "key_secret", "blake3:", "key_"] {
            assert_eq!(opaque_reference(value), hash_bytes(value.as_bytes()));
            assert_ne!(redact_identity(value, RedactionLevel::Full), value);
        }
        for suffix in ["a".repeat(63), "a".repeat(65), "G".repeat(64)] {
            assert!(!canonical_digest(&format!("blake3:{suffix}"), "blake3:"));
            assert!(!canonical_digest(&format!("key_{suffix}"), "key_"));
        }
    }

    #[test]
    fn agent_and_task_pseudonyms_survive_recovery_without_aliasing() {
        for key in [
            "RecoveryAgent",
            "/Users/private/task",
            "api_key=PRIVATE_SENTINEL",
        ] {
            let once = redact_identity(key, RedactionLevel::Full);
            assert_ne!(once, key);
            for level in [
                RedactionLevel::None,
                RedactionLevel::Minimal,
                RedactionLevel::Standard,
                RedactionLevel::Strict,
                RedactionLevel::Paranoid,
                RedactionLevel::Full,
            ] {
                assert_eq!(redact_identity(&once, level), once);
            }
        }
        assert_ne!(
            redact_identity("one", RedactionLevel::Full),
            redact_identity("two", RedactionLevel::Full)
        );
    }

    #[test]
    fn unredacted_and_admitted_unchanged_evidence_remain_exact() {
        let original = row();
        for (level, admitted) in [
            (RedactionLevel::None, false),
            (RedactionLevel::None, true),
            (RedactionLevel::Standard, true),
        ] {
            let mut value = original.clone();
            redact_evidence(&mut value, level, admitted);
            assert_eq!(value, original);
        }
    }

    #[test]
    fn canonical_metadata_key_does_not_hide_a_secret_value_in_backup_export() {
        for raw in [
            r#"{"secretRedactionStatus":"api_key=CANONICAL_VALUE_CANARY"}"#,
            r#"{"secretRedactionStatus":"api_key=CANONICAL_VALUE_CANARY","secretRedactionStatus":"clean"}"#,
        ] {
            let mut value = row();
            value.metadata_json = Some(raw.to_owned());
            // Even independently admitted provenance cannot exempt metadata values.
            redact_evidence(&mut value, RedactionLevel::Standard, true);
            assert!(value.metadata_json.is_none());
            assert_eq!(value.excerpt, "[REDACTED]");
            assert_eq!(value.secret_redaction_status, "redacted");
            assert!(value.canonical_excerpt_hash.is_none());
            assert_eq!(value.canonical_provenance_revision, 0);
            assert_eq!(value.search_eligibility, "denied");
            assert_eq!(value.pack_eligibility, "denied");
            assert!(
                !serde_json::to_string(&value)
                    .expect("backup row")
                    .contains("CANONICAL_VALUE_CANARY")
            );
        }
    }
}
