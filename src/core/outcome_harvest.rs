//! Bounded, review-only attribution of authoritative pack-linked verification.
//!
//! This first harvester observes existing proof and selection ledgers. It does
//! not run verification, infer task/recorder/git links, or write live feedback.
//! Memory ledgers have no historical body commitment: attribution therefore
//! requires a current episodic source unchanged since its pack was assembled.
//! Supported proof producers retain RCH command bytes or validated run-record
//! argv digests. Only proofs in the same command representation can corroborate
//! one another; metadata-only run summaries cannot establish independence.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use sqlmodel_core::Value;

use crate::core::curate::source_admission;
use crate::core::outcome::{DEFAULT_DERIVED_PER_MEMORY_PER_WINDOW, derived_contribution_decision};
use crate::core::pack_selection_identity::{self, PackSelectionIdentity};
use crate::core::verify::{HarvestVerificationProof, verification_proofs_for_pack_harvest};
use crate::db::{
    CreateAuditInput, CreateCurationCandidateInput, CreateOutcomeEvidenceInput, DbConnection,
    OutcomeEvidenceSource, StoredCurationCandidate, StoredMemory, StoredOutcomeEvidence,
    StoredPackRecord, audit_actions, generate_audit_id, pack_ledger_core_array,
    parse_stored_pack_ledger,
};
use crate::models::{CandidateId, DomainError, MemoryId, PackId, TrustClass};

pub const OUTCOME_HARVEST_SCHEMA_V1: &str = "ee.outcome.harvest.v1";
pub const DEFAULT_HARVEST_PACK_LIMIT: u32 = 128;
pub const MAX_HARVEST_PACK_LIMIT: u32 = 256;
pub const HARVEST_RECEIPT_CHANGED: &str = "outcome_harvest_receipt_changed";
const SOURCE_PREFIX: &str = "outcome-harvest:v1:";
const RECEIPT_SCHEMA: &str = "ee.outcome.harvest.receipt.v1";
const MAX_RECEIPT_BYTES: usize = 16 * 1024;
const MAX_PRIOR_CANDIDATES: usize = 2048;
const MAX_SELECTED_PER_PACK: usize = 256;
const MAX_TOTAL_SELECTED_ITEMS: usize = 1024;
const MAX_PACK_BYTES: i64 = 1024 * 1024;
const MAX_REPORT_PROPOSALS: usize = 512;
const MAX_REPORT_SKIPS: usize = 256;
const MAX_SOURCE_BYTES: i64 = 1024 * 1024;
const PROPOSAL_REASON: &str = "Two independent authoritative verification runs followed this pack. Review whether its unchanged episodic memory warrants semantic retention; this is derived evidence, not proof of causation.";

#[derive(Clone, Debug)]
pub struct OutcomeHarvestOptions {
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
    pub since: String,
    pub until: String,
    pub apply: bool,
    pub pack_limit: u32,
    pub pack_id: Option<String>,
    pub actor: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutcomeHarvestProposal {
    pub candidate_id: String,
    pub memory_id: String,
    pub direction: &'static str,
    pub base_weight_milli: u32,
    pub corroborating_count: u32,
    pub pack_id: String,
    pub proof_refs: Vec<String>,
    pub receipt_hash: String,
    pub source_binding: &'static str,
    pub status: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutcomeHarvestSkip {
    pub pack_id: String,
    pub memory_id: Option<String>,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutcomeHarvestReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub status: &'static str,
    pub dry_run: bool,
    pub workspace_id: String,
    pub since: String,
    pub until: String,
    pub scanned_packs: u32,
    pub proposed_count: u32,
    pub persisted_count: u32,
    pub replayed_count: u32,
    pub capped_count: u32,
    pub skipped_count: u32,
    pub proposals: Vec<OutcomeHarvestProposal>,
    pub skipped: Vec<OutcomeHarvestSkip>,
    /// True means a scan/report bound was reached; this is not full coverage.
    pub bounded: bool,
    pub coverage: &'static str,
}

impl OutcomeHarvestReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        serde_json::to_value(self).unwrap_or_else(|_| {
            json!({
                "schema": OUTCOME_HARVEST_SCHEMA_V1,
                "command": "outcome harvest",
                "status": "serialization_failed",
            })
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        format!(
            "Outcome harvest{}\nWindow: [{}, {})\nPacks inspected: {} ({})\nReview proposals: {} | Persisted: {} | Replayed: {} | Capped: {} | Skipped: {}\nNo memory scores or maturity changed. Review pending candidates with ee curate candidates --type promote --json.\n",
            if self.dry_run { " [DRY RUN]" } else { "" },
            self.since,
            self.until,
            self.scanned_packs,
            self.coverage,
            self.proposed_count,
            self.persisted_count,
            self.replayed_count,
            self.capped_count,
            self.skipped_count,
        )
    }

    fn skip(&mut self, pack: &str, memory: Option<&str>, reason: &str) {
        self.skipped_count = self.skipped_count.saturating_add(1);
        if self.skipped.len() < MAX_REPORT_SKIPS {
            self.skipped.push(OutcomeHarvestSkip {
                pack_id: crate::models::public_pack_id(pack),
                memory_id: memory.map(crate::models::public_memory_id),
                reason: reason.to_owned(),
            });
        } else {
            self.bounded = true;
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceReceipt {
    memory_id: String,
    content_hash: String,
    snapshot_hash: String,
    level: String,
    kind: String,
    trust_class: String,
    created_at: String,
    updated_at: String,
    valid_from: Option<String>,
    valid_to: Option<String>,
    binding: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProofReceipt {
    audit_id: String,
    audit_receipt_hash: String,
    content_hash: String,
    authority: String,
    verification_identity_hash: String,
    command_identity: String,
    command_aliases: Vec<String>,
    started_at: String,
    finished_at: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HarvestReceipt {
    schema: String,
    workspace_id: String,
    source: SourceReceipt,
    pack_id: String,
    pack_hash: String,
    ledger_hash: String,
    pack_created_at: String,
    selected_item_hash: String,
    impression_hash: String,
    proofs: Vec<ProofReceipt>,
}

struct PriorCandidate {
    stored: StoredCurationCandidate,
    receipt: HarvestReceipt,
}

struct PackEvidence {
    record: StoredPackRecord,
    selected: BTreeMap<String, JsonValue>,
    impressions: BTreeMap<String, JsonValue>,
    proofs: Vec<ProofReceipt>,
}

#[derive(Clone, Copy)]
struct Window {
    since: DateTime<Utc>,
    until: DateTime<Utc>,
}

impl Window {
    fn contains(self, value: DateTime<Utc>) -> bool {
        value >= self.since && value < self.until
    }
}

fn instant(raw: &str) -> Result<DateTime<Utc>, ()> {
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| ())
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn hash(value: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(value).to_hex())
}

fn json_hash(value: &JsonValue) -> String {
    hash(value.to_string().as_bytes())
}

fn storage(operation: &str) -> DomainError {
    DomainError::Storage {
        message: format!(
            "Could not {operation} for outcome harvest; no unproved attribution was admitted"
        ),
        repair: Some("Inspect the workspace store and verification ledger, then retry.".to_owned()),
    }
}

fn stale() -> DomainError {
    DomainError::UsageCodeWithDetails {
        code: HARVEST_RECEIPT_CHANGED,
        message: "Harvest source, selection, proof, or feedback authority changed; this proposal cannot authorize promotion.".to_owned(),
        repair: Some("Review the original evidence and generate a fresh outcome harvest proposal.".to_owned()),
        details_json: "{}".to_owned(),
    }
}

/// Preview by default. `apply` records review proposals, never their promotion.
pub fn harvest_outcomes(
    options: &OutcomeHarvestOptions,
) -> Result<OutcomeHarvestReport, DomainError> {
    let window = Window {
        since: instant(&options.since)
            .map_err(|_| usage("since must be an explicit RFC3339 instant"))?,
        until: instant(&options.until)
            .map_err(|_| usage("until must be an explicit RFC3339 instant"))?,
    };
    if window.since >= window.until {
        return Err(usage("since must be strictly earlier than until"));
    }
    if !(1..=MAX_HARVEST_PACK_LIMIT).contains(&options.pack_limit) {
        return Err(usage("pack-limit must be between 1 and 256"));
    }
    if options
        .pack_id
        .as_deref()
        .is_some_and(|id| id.parse::<PackId>().is_err())
    {
        return Err(usage("pack-id must be a canonical persisted pack ID"));
    }
    let workspace = options
        .workspace_path
        .canonicalize()
        .map_err(|_| usage("workspace must exist"))?;
    let database = options
        .database_path
        .clone()
        .unwrap_or_else(|| workspace.join(".ee/ee.db"));
    if !database.exists() {
        return Err(crate::core::storeless_workspace_error(&database));
    }
    let connection = if options.apply {
        DbConnection::open_file(&database)
    } else {
        DbConnection::open_file_read_only(&database)
    }
    .map_err(|_| storage("open the existing database"))?;
    if connection
        .needs_migration()
        .map_err(|_| storage("inspect the schema"))?
    {
        return Err(DomainError::MigrationRequired {
            message: "Outcome harvest requires the current workspace schema.".to_owned(),
            repair: Some("ee migrate run --workspace . --json".to_owned()),
        });
    }
    let workspace_id =
        crate::core::workspace::addressed_workspace_row(&connection, &workspace, &database)?
            .ok_or_else(|| usage("workspace is not registered in this database"))?
            .id;
    let collect = || collect_harvest(&connection, options, &workspace_id, window, Utc::now());
    if !options.apply {
        return source_admission::with_snapshot(&connection, collect);
    }
    let operation = crate::core::write_owner::WriteOperation::Custom {
        operation_type: "outcome_harvest".to_owned(),
        payload: json!({"workspaceId": workspace_id, "since": timestamp(window.since), "until": timestamp(window.until)}),
    };
    crate::core::write_owner::run_one_shot_write_intake(&workspace, &operation, || {
        let mut domain_error = None;
        connection
            .with_transaction(|| {
                collect().map_err(|error| {
                    domain_error = Some(error);
                    crate::db::DbError::MalformedRow {
                        operation: crate::db::DbOperation::Execute,
                        message: "outcome harvest transaction refused".to_owned(),
                    }
                })
            })
            .map_err(|_| domain_error.unwrap_or_else(|| storage("commit the harvest transaction")))
    })
}

fn usage(message: &str) -> DomainError {
    DomainError::Usage {
        message: message.to_owned(),
        repair: Some(
            "ee outcome harvest --since <RFC3339> --until <RFC3339> --dry-run --json".to_owned(),
        ),
    }
}

fn collect_harvest(
    connection: &DbConnection,
    options: &OutcomeHarvestOptions,
    workspace_id: &str,
    window: Window,
    live_time: DateTime<Utc>,
) -> Result<OutcomeHarvestReport, DomainError> {
    let prior = prior_candidates(connection, workspace_id)?;
    let mut pack_ids = if let Some(id) = &options.pack_id {
        vec![id.clone()]
    } else {
        connection
            .list_recent_pack_record_ids_for_workspace(workspace_id, options.pack_limit + 1)
            .map_err(|_| storage("list bounded pack identities"))?
    };
    let overflow = pack_ids.len() > options.pack_limit as usize;
    pack_ids.truncate(options.pack_limit as usize);
    let mut report = OutcomeHarvestReport {
        schema: OUTCOME_HARVEST_SCHEMA_V1,
        command: "outcome harvest",
        status: if options.apply { "applied" } else { "dry_run" },
        dry_run: !options.apply,
        workspace_id: crate::models::public_workspace_id(workspace_id),
        since: timestamp(window.since),
        until: timestamp(window.until),
        scanned_packs: 0,
        proposed_count: 0,
        persisted_count: 0,
        replayed_count: 0,
        capped_count: 0,
        skipped_count: 0,
        proposals: Vec::new(),
        skipped: Vec::new(),
        bounded: overflow,
        coverage: if options.pack_id.is_some() {
            "exact_pack"
        } else if overflow {
            "recent_packs_truncated"
        } else {
            "all_recorded_packs"
        },
    };
    let mut contribution_counts: BTreeMap<String, u32> = BTreeMap::new();
    for existing in &prior {
        if existing
            .receipt
            .proofs
            .iter()
            .any(|proof| instant(&proof.finished_at).is_ok_and(|time| window.contains(time)))
        {
            *contribution_counts
                .entry(existing.receipt.source.memory_id.clone())
                .or_default() += 1;
        }
    }
    let mut remaining_selections = MAX_TOTAL_SELECTED_ITEMS;
    for pack_id in pack_ids {
        if remaining_selections == 0 {
            report.bounded = true;
            report.coverage = "selection_work_limit_reached";
            break;
        }
        report.scanned_packs += 1;
        let pack = match load_pack_evidence(
            connection,
            workspace_id,
            &pack_id,
            &mut remaining_selections,
        ) {
            Ok(pack) => pack,
            Err(reason) if reason == "selection_work_limit_reached" => {
                report.bounded = true;
                report.coverage = "selection_work_limit_reached";
                report.skip(&pack_id, None, &reason);
                break;
            }
            Err(reason) => {
                report.skip(&pack_id, None, &reason);
                continue;
            }
        };
        let eligible_proofs: Vec<_> = pack
            .proofs
            .iter()
            .filter(|proof| {
                instant(&proof.finished_at)
                    .is_ok_and(|time| window.contains(time) && time <= live_time)
            })
            .cloned()
            .collect();
        let pair = independent_pair(&eligible_proofs);
        if pair.is_none() {
            report.skip(&pack_id, None, "insufficient_independent_proofs_in_window");
            continue;
        }
        for (memory_id, selected) in &pack.selected {
            if report.proposals.len() >= MAX_REPORT_PROPOSALS {
                report.bounded = true;
                report.coverage = "proposal_limit_reached";
                return Ok(report);
            }
            let Some(impression) = pack.impressions.get(memory_id) else {
                report.skip(
                    &pack_id,
                    Some(memory_id),
                    "selected_memory_impression_missing",
                );
                continue;
            };
            // Reuse an existing valid pair before choosing among newly added
            // proofs. Window spelling and additional corroboration never open
            // another candidate for the same unchanged evidence receipt.
            if let Some(existing) = prior.iter().find(|existing| {
                existing.receipt.pack_id == pack_id
                    && existing.receipt.source.memory_id == *memory_id
                    && existing
                        .receipt
                        .proofs
                        .iter()
                        .all(|proof| eligible_proofs.contains(proof))
            }) {
                if matches!(
                    existing.stored.status.as_str(),
                    "applied" | "rejected" | "expired"
                ) || validate_candidate_in_snapshot(connection, &existing.stored, live_time)
                    .is_ok()
                {
                    report.replayed_count += 1;
                    report
                        .proposals
                        .push(proposal_view(&existing.receipt, "already_recorded")?);
                } else {
                    report.skip(
                        &pack_id,
                        Some(memory_id),
                        "existing_proposal_authority_changed",
                    );
                }
                continue;
            }
            let source = match source_receipt(connection, workspace_id, memory_id, live_time) {
                Ok(source) => source,
                Err(reason) => {
                    report.skip(&pack_id, Some(memory_id), reason);
                    continue;
                }
            };
            if !source_was_unchanged_at_pack(&source, &pack.record, selected) {
                report.skip(
                    &pack_id,
                    Some(memory_id),
                    "source_not_provably_unchanged_since_pack",
                );
                continue;
            }
            let receipt = HarvestReceipt {
                schema: RECEIPT_SCHEMA.to_owned(),
                workspace_id: workspace_id.to_owned(),
                source,
                pack_id: pack_id.clone(),
                pack_hash: pack.record.pack_hash.clone(),
                ledger_hash: pack
                    .record
                    .ledger_hash
                    .clone()
                    .ok_or_else(|| storage("bind the pack ledger"))?,
                pack_created_at: pack.record.created_at.clone(),
                selected_item_hash: json_hash(selected),
                impression_hash: json_hash(impression),
                proofs: pair
                    .clone()
                    .ok_or_else(|| storage("select independent proof"))?,
            };
            let count = contribution_counts.entry(memory_id.clone()).or_default();
            if !derived_contribution_decision(*count, DEFAULT_DERIVED_PER_MEMORY_PER_WINDOW)
                .is_admitted()
            {
                report.capped_count += 1;
                report
                    .proposals
                    .push(proposal_view(&receipt, "cap_exceeded")?);
                continue;
            }
            *count += 1;
            report.proposed_count += 1;
            if options.apply {
                persist_proposal(connection, &receipt, window, options.actor.as_deref())?;
                report.persisted_count += 1;
            }
            report.proposals.push(proposal_view(
                &receipt,
                if options.apply { "pending" } else { "proposed" },
            )?);
        }
    }
    Ok(report)
}

fn source_receipt(
    connection: &DbConnection,
    workspace: &str,
    id: &str,
    now: DateTime<Utc>,
) -> Result<SourceReceipt, &'static str> {
    if id.parse::<MemoryId>().is_err() {
        return Err("invalid_native_memory_identity");
    }
    let sizes = connection.query("SELECT length(CAST(content AS BLOB)), length(CAST(typed_fields_json AS BLOB)) FROM memories WHERE id = ?1 AND workspace_id = ?2", &[Value::Text(id.to_owned()), Value::Text(workspace.to_owned())]).map_err(|_| "source_authority_unavailable")?;
    let size = sizes.first().ok_or("source_missing_or_foreign")?;
    if size
        .get(0)
        .and_then(Value::as_i64)
        .is_none_or(|bytes| bytes > MAX_SOURCE_BYTES)
        || size
            .get(1)
            .and_then(Value::as_i64)
            .is_some_and(|bytes| bytes > MAX_SOURCE_BYTES)
    {
        return Err("source_too_large");
    }
    let memory = connection
        .get_memory(id)
        .map_err(|_| "source_authority_unavailable")?
        .ok_or("source_missing_or_foreign")?;
    if memory.workspace_id != workspace {
        return Err("source_missing_or_foreign");
    }
    if memory.level != "episodic" {
        return Err("source_level_not_actionable");
    }
    if memory.tombstoned_at.is_some()
        || !connection
            .filter_current_memory_ids(&[id.to_owned()])
            .map_err(|_| "source_authority_unavailable")?
            .contains(id)
    {
        return Err("source_not_current");
    }
    let from = memory
        .valid_from
        .as_deref()
        .map(instant)
        .transpose()
        .map_err(|_| "source_lifecycle_invalid")?;
    let to = memory
        .valid_to
        .as_deref()
        .map(instant)
        .transpose()
        .map_err(|_| "source_lifecycle_invalid")?;
    if from.is_some_and(|time| time > now)
        || to.is_some_and(|time| time < now)
        || from.zip(to).is_some_and(|(from, to)| from > to)
    {
        return Err("source_not_currently_applicable");
    }
    if memory.trust_class.parse::<TrustClass>().is_err() {
        return Err("source_trust_invalid");
    }
    source_admission::validate_memory_authority(connection, &memory)
        .map_err(|_| "source_sealed_held_or_unavailable")?;
    let feedback = connection.query("SELECT 1 FROM feedback_events WHERE workspace_id = ?1 AND target_type = 'memory' AND target_id = ?2 LIMIT 1", &[Value::Text(workspace.to_owned()), Value::Text(id.to_owned())]).map_err(|_| "source_feedback_unavailable")?;
    if !feedback.is_empty() {
        return Err("accepted_memory_feedback_present");
    }
    if !crate::policy::redact_secret_like_content(&memory.content)
        .redacted_reasons
        .is_empty()
        || memory.content == crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT
    {
        return Err("source_body_not_admitted");
    }
    let tags = connection
        .get_memory_tags(id)
        .map_err(|_| "source_authority_unavailable")?;
    let typed = connection
        .get_memory_typed_fields_json(id)
        .map_err(|_| "source_authority_unavailable")?;
    let snapshot = memory_snapshot_value(&memory, &tags, typed.as_deref());
    Ok(SourceReceipt {
        memory_id: id.to_owned(),
        content_hash: hash(memory.content.as_bytes()),
        snapshot_hash: json_hash(&snapshot),
        level: memory.level,
        kind: memory.kind,
        trust_class: memory.trust_class,
        created_at: memory.created_at,
        updated_at: memory.updated_at,
        valid_from: memory.valid_from,
        valid_to: memory.valid_to,
        binding: "unchanged_since_pack".to_owned(),
    })
}

fn memory_snapshot_value(memory: &StoredMemory, tags: &[String], typed: Option<&str>) -> JsonValue {
    json!({
        "id": memory.id, "workspaceId": memory.workspace_id, "contentHash": hash(memory.content.as_bytes()),
        "level": memory.level, "kind": memory.kind, "workflowId": memory.workflow_id,
        "confidence": memory.confidence, "utility": memory.utility, "importance": memory.importance,
        "trustClass": memory.trust_class, "trustSubclass": memory.trust_subclass,
        "provenanceUri": memory.provenance_uri, "provenanceChainHash": memory.provenance_chain_hash,
        "provenanceChainHashVersion": memory.provenance_chain_hash_version,
        "provenanceVerificationStatus": memory.provenance_verification_status,
        "provenanceVerifiedAt": memory.provenance_verified_at,
        "provenanceVerificationNote": memory.provenance_verification_note,
        "createdAt": memory.created_at, "updatedAt": memory.updated_at,
        "validFrom": memory.valid_from, "validTo": memory.valid_to, "tags": tags, "typedFields": typed,
    })
}

fn source_was_unchanged_at_pack(
    source: &SourceReceipt,
    pack: &StoredPackRecord,
    selected: &JsonValue,
) -> bool {
    let Ok(packed_at) = instant(&pack.created_at) else {
        return false;
    };
    instant(&source.created_at)
        .ok()
        .zip(instant(&source.updated_at).ok())
        .is_some_and(|(created, updated)| created <= updated && updated <= packed_at)
        && source
            .valid_from
            .as_deref()
            .is_none_or(|raw| instant(raw).is_ok_and(|time| time <= packed_at))
        && source
            .valid_to
            .as_deref()
            .is_none_or(|raw| instant(raw).is_ok_and(|time| time >= packed_at))
        && selected.get("trustClass").and_then(JsonValue::as_str)
            == Some(source.trust_class.as_str())
}

fn proof_receipt(
    proof: HarvestVerificationProof,
    packed_at: DateTime<Utc>,
) -> Option<ProofReceipt> {
    let started_at = proof.record.started_at.as_deref()?;
    let finished_at = proof.record.finished_at.as_deref()?;
    let start = instant(started_at).ok()?;
    let finish = instant(finished_at).ok()?;
    if start < packed_at || finish <= packed_at || finish < start {
        return None;
    }
    Some(ProofReceipt {
        audit_id: proof.audit_id,
        audit_receipt_hash: proof.audit_receipt_hash,
        content_hash: proof.content_hash,
        authority: proof.authority,
        verification_identity_hash: hash(proof.record.verification_id.as_bytes()),
        command_identity: proof.command_identity,
        command_aliases: proof.command_aliases,
        started_at: started_at.to_owned(),
        finished_at: finished_at.to_owned(),
    })
}

fn independent_pair(proofs: &[ProofReceipt]) -> Option<Vec<ProofReceipt>> {
    for (index, first) in proofs.iter().enumerate() {
        if let Some(second) = proofs[index + 1..].iter().find(|second| {
            second.verification_identity_hash != first.verification_identity_hash
                && commands_provably_differ(first, second)
        }) {
            return Some(vec![first.clone(), second.clone()]);
        }
    }
    None
}

fn commands_provably_differ(first: &ProofReceipt, second: &ProofReceipt) -> bool {
    // RCH digests commit to raw command text; validated run records commit to
    // length-prefixed argv. Equal digest algorithms do not make those two
    // preimage domains comparable. Producer authority fixes the representation.
    if first.authority != second.authority
        || !matches!(
            first.authority.as_str(),
            "validated_rch_verify" | "validated_run_record"
        )
    {
        return false;
    }
    // RCH exposes both supported raw-text digests; run records expose their
    // canonical argv digest. Within a representation, incomparable algorithms
    // alone still cannot establish that two commands differ.
    !first
        .command_aliases
        .iter()
        .any(|digest| second.command_aliases.contains(digest))
        && first.command_aliases.iter().any(|left| {
            second.command_aliases.iter().any(|right| {
                left.split_once(':').zip(right.split_once(':')).is_some_and(
                    |((left_algorithm, _), (right_algorithm, _))| left_algorithm == right_algorithm,
                )
            })
        })
}

fn load_pack_evidence(
    connection: &DbConnection,
    workspace: &str,
    id: &str,
    remaining_selections: &mut usize,
) -> Result<PackEvidence, String> {
    let sizes = connection.query("SELECT item_count, length(CAST(ledger_json AS BLOB)), length(CAST(query AS BLOB)), length(CAST(degraded_json AS BLOB)) FROM pack_records WHERE id = ?1 AND workspace_id = ?2", &[Value::Text(id.to_owned()), Value::Text(workspace.to_owned())]).map_err(|_| "pack_read_failed")?;
    let metadata = sizes.first().ok_or("pack_missing_or_foreign")?;
    let selected_count = metadata
        .get(0)
        .and_then(Value::as_i64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or("pack_ledger_unproved")?;
    if selected_count > MAX_SELECTED_PER_PACK {
        return Err("pack_selection_limit_exceeded".to_owned());
    }
    if selected_count > *remaining_selections {
        return Err("selection_work_limit_reached".to_owned());
    }
    // Charge even if a later ledger/proof check refuses the pack; malformed
    // packs must not turn a bounded scan into unbounded selection work.
    *remaining_selections -= selected_count;
    if metadata
        .get(1)
        .and_then(Value::as_i64)
        .is_none_or(|bytes| bytes > MAX_PACK_BYTES)
        || (2..=3).any(|column| {
            metadata
                .get(column)
                .and_then(Value::as_i64)
                .is_some_and(|bytes| bytes > MAX_PACK_BYTES)
        })
    {
        return Err("pack_ledger_size_limit_exceeded".to_owned());
    }
    let record = connection
        .get_pack_record(id)
        .map_err(|_| "pack_read_failed")?
        .ok_or("pack_missing_or_foreign")?;
    if record.workspace_id != workspace {
        return Err("pack_missing_or_foreign".to_owned());
    }
    let packed_at = instant(&record.created_at).map_err(|_| "pack_time_invalid")?;
    let parsed = parse_stored_pack_ledger(&record);
    let ledger = parsed.available_ledger().ok_or("pack_ledger_unproved")?;
    let items = pack_ledger_core_array(ledger, "selectedItems").ok_or("pack_ledger_unproved")?;
    if items.len() != selected_count {
        return Err("pack_selection_count_mismatch".to_owned());
    }
    let mut selected = BTreeMap::new();
    let mut identities = BTreeSet::new();
    for item in items {
        let identity = pack_selection_identity::classify(item)
            .map_err(|_| "pack_selection_identity_invalid")?;
        let key = match identity {
            PackSelectionIdentity::Memory(memory) => {
                if memory.parse::<MemoryId>().is_err() {
                    return Err("pack_selection_identity_invalid".to_owned());
                }
                selected.insert(memory.to_owned(), item.clone());
                ("memory", memory)
            }
            PackSelectionIdentity::Native { kind, entity_id } => (kind, entity_id),
        };
        if !identities.insert(key) {
            return Err("duplicate_pack_selection_identity".to_owned());
        }
    }
    let rows = connection.query("SELECT memory_id, query_hash, lens_hash, rank, section, token_estimate, db_generation, index_generation, graph_generation, created_at FROM pack_candidate_impressions WHERE pack_id = ?1 AND workspace_id = ?2 AND memory_id IS NOT NULL AND selected = 1 ORDER BY rank, memory_id LIMIT ?3", &[Value::Text(id.to_owned()), Value::Text(workspace.to_owned()), Value::BigInt((MAX_SELECTED_PER_PACK + 1) as i64)]).map_err(|_| "pack_impressions_unavailable")?;
    if rows.len() > MAX_SELECTED_PER_PACK {
        return Err("pack_impression_limit_exceeded".to_owned());
    }
    let mut impressions = BTreeMap::new();
    let expected_query_hash = hash(record.query.as_bytes());
    let expected_lens_hash = hash(format!("{}\0{}", record.profile, record.max_tokens).as_bytes());
    for row in rows {
        let text = |index| {
            row.get(index)
                .and_then(Value::as_str)
                .ok_or("pack_impression_invalid")
        };
        let number = |index| {
            row.get(index)
                .and_then(Value::as_i64)
                .filter(|value| *value >= 0)
                .ok_or("pack_impression_invalid")
        };
        let memory = text(0)?;
        let Some(item) = selected.get(memory) else {
            return Err("impression_not_in_selection_ledger".to_owned());
        };
        let rank = number(3)?;
        let tokens = number(5)?;
        if item.get("rank").and_then(JsonValue::as_i64) != Some(rank)
            || item.get("section").and_then(JsonValue::as_str) != Some(text(4)?)
            || item.get("estimatedTokens").and_then(JsonValue::as_i64) != Some(tokens)
            || text(1)? != expected_query_hash
            || text(2)? != expected_lens_hash
            || ledger
                .get("database")
                .and_then(|database| database.get("generation"))
                .and_then(JsonValue::as_i64)
                != Some(number(6)?)
            || instant(text(9)?).map_err(|_| "pack_impression_invalid")? != packed_at
        {
            return Err("impression_selection_mismatch".to_owned());
        }
        let value = json!({"packId": id, "workspaceId": workspace, "memoryId": memory,
            "queryHash": text(1)?, "lensHash": text(2)?, "rank": rank, "section": text(4)?,
            "tokenEstimate": tokens, "databaseGeneration": number(6)?,
            "indexGeneration": row.get(7).and_then(Value::as_i64),
            "graphGeneration": row.get(8).and_then(Value::as_i64), "createdAt": text(9)?, "selected": true});
        if impressions.insert(memory.to_owned(), value).is_some() {
            return Err("duplicate_memory_impression".to_owned());
        }
    }
    let mut proofs: Vec<_> = verification_proofs_for_pack_harvest(connection, workspace, id)?
        .into_iter()
        .filter_map(|proof| proof_receipt(proof, packed_at))
        .collect();
    proofs.sort_by(|left, right| {
        instant(&left.finished_at)
            .ok()
            .cmp(&instant(&right.finished_at).ok())
            .then_with(|| left.command_identity.cmp(&right.command_identity))
            .then_with(|| left.audit_id.cmp(&right.audit_id))
    });
    Ok(PackEvidence {
        record,
        selected,
        impressions,
        proofs,
    })
}

fn receipt_source(receipt: &HarvestReceipt) -> Result<String, DomainError> {
    let raw =
        serde_json::to_string(receipt).map_err(|_| storage("serialize the attribution receipt"))?;
    if raw.len() > MAX_RECEIPT_BYTES {
        return Err(storage("bound the attribution receipt"));
    }
    Ok(format!("{SOURCE_PREFIX}{raw}"))
}

fn receipt_candidate_id(source: &str) -> String {
    let digest = blake3::hash(source.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    format!(
        "curate_{}",
        CandidateId::from_uuid(uuid::Uuid::from_bytes(bytes))
            .to_string()
            .trim_start_matches("cand_")
    )
}

fn proposal_view(
    receipt: &HarvestReceipt,
    status: &str,
) -> Result<OutcomeHarvestProposal, DomainError> {
    let source = receipt_source(receipt)?;
    Ok(OutcomeHarvestProposal {
        candidate_id: receipt_candidate_id(&source),
        memory_id: crate::models::public_memory_id(&receipt.source.memory_id),
        direction: "positive",
        base_weight_milli: OutcomeEvidenceSource::VerifierSuccess.base_weight_milli(),
        corroborating_count: 2,
        pack_id: crate::models::public_pack_id(&receipt.pack_id),
        proof_refs: receipt
            .proofs
            .iter()
            .map(|proof| crate::models::public_audit_id(&proof.audit_id))
            .collect(),
        receipt_hash: hash(source.as_bytes()),
        source_binding: "unchanged_since_pack",
        status: status.to_owned(),
    })
}

fn decoded_candidate(candidate: &StoredCurationCandidate) -> Result<HarvestReceipt, DomainError> {
    let source = candidate.source_id.as_deref().ok_or_else(stale)?;
    let raw = source
        .strip_prefix(SOURCE_PREFIX)
        .filter(|raw| raw.len() <= MAX_RECEIPT_BYTES)
        .ok_or_else(stale)?;
    let receipt: HarvestReceipt = serde_json::from_str(raw).map_err(|_| stale())?;
    if !receipt_shape_is_valid(&receipt)
        || receipt_source(&receipt)? != source
        || candidate.id != receipt_candidate_id(source)
        || candidate.workspace_id != receipt.workspace_id
        || candidate.target_memory_id.as_deref() != Some(receipt.source.memory_id.as_str())
        || candidate.candidate_type != "promote"
        || candidate.source_type != "rule_engine"
        || candidate.proposed_content.is_some()
        || candidate.proposed_confidence.is_some()
        || candidate.proposed_trust_class.is_some()
        || candidate.derivation_source_refs_json.is_some()
        || candidate.derivation_metadata_json.is_some()
        || candidate.reason != PROPOSAL_REASON
        || candidate.confidence != 0.4
    {
        return Err(stale());
    }
    Ok(receipt)
}

fn receipt_shape_is_valid(receipt: &HarvestReceipt) -> bool {
    let source = &receipt.source;
    let Ok(packed_at) = instant(&receipt.pack_created_at) else {
        return false;
    };
    let canonical_hash = crate::db::is_canonical_blake3_hash;
    receipt.schema == RECEIPT_SCHEMA
        && source.binding == "unchanged_since_pack"
        && source.level == "episodic"
        && !source.kind.is_empty()
        && !receipt.workspace_id.is_empty()
        && source.memory_id.parse::<MemoryId>().is_ok()
        && receipt.pack_id.parse::<PackId>().is_ok()
        && source.trust_class.parse::<TrustClass>().is_ok()
        && [
            &source.content_hash,
            &source.snapshot_hash,
            &receipt.pack_hash,
            &receipt.ledger_hash,
            &receipt.selected_item_hash,
            &receipt.impression_hash,
        ]
        .into_iter()
        .all(|value| canonical_hash(value))
        && instant(&source.created_at).is_ok_and(|time| time <= packed_at)
        && instant(&source.updated_at).is_ok_and(|time| time <= packed_at)
        && [&source.valid_from, &source.valid_to]
            .into_iter()
            .flatten()
            .all(|value| instant(value).is_ok())
        && receipt.proofs.len() == 2
        && independent_pair(&receipt.proofs).is_some()
        && receipt.proofs.iter().all(|proof| {
            let audit_valid = proof.audit_id.parse::<crate::models::AuditId>().is_ok()
                || proof.audit_id.strip_prefix("audit_").is_some_and(|value| {
                    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
                });
            audit_valid
                && canonical_hash(&proof.audit_receipt_hash)
                && canonical_hash(&proof.content_hash)
                && canonical_hash(&proof.verification_identity_hash)
                && matches!(
                    proof.authority.as_str(),
                    "validated_rch_verify" | "validated_run_record"
                )
                && (1..=2).contains(&proof.command_aliases.len())
                && proof
                    .command_aliases
                    .windows(2)
                    .all(|pair| pair[0] < pair[1])
                && proof.command_aliases.contains(&proof.command_identity)
                && proof.command_aliases.iter().all(|digest| {
                    digest.split_once(':').is_some_and(|(algorithm, value)| {
                        matches!(algorithm, "sha256" | "blake3")
                            && value.len() == 64
                            && value
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    })
                })
                && instant(&proof.started_at)
                    .ok()
                    .zip(instant(&proof.finished_at).ok())
                    .is_some_and(|(start, finish)| {
                        start >= packed_at && finish > packed_at && finish >= start
                    })
        })
}

fn prior_candidates(
    connection: &DbConnection,
    workspace: &str,
) -> Result<Vec<PriorCandidate>, DomainError> {
    let rows = connection.query("SELECT id, length(CAST(source_id AS BLOB)), proposed_content IS NULL FROM curation_candidates WHERE workspace_id = ?1 AND source_id LIKE 'outcome-harvest:%' ORDER BY id LIMIT ?2", &[Value::Text(workspace.to_owned()), Value::BigInt((MAX_PRIOR_CANDIDATES + 1) as i64)]).map_err(|_| storage("read prior attribution receipts"))?;
    if rows.len() > MAX_PRIOR_CANDIDATES {
        return Err(storage("bound prior attribution receipts"));
    }
    rows.iter()
        .map(|row| {
            if row
                .get(1)
                .and_then(Value::as_i64)
                .is_none_or(|bytes| bytes > (MAX_RECEIPT_BYTES + SOURCE_PREFIX.len()) as i64)
            {
                return Err(stale());
            }
            if row.get(2).and_then(Value::as_i64) != Some(1) {
                return Err(stale());
            }
            let id = row.get(0).and_then(Value::as_str).ok_or_else(stale)?;
            let stored = connection
                .get_curation_candidate(workspace, id)
                .map_err(|_| storage("read a prior candidate"))?
                .ok_or_else(stale)?;
            let receipt = decoded_candidate(&stored)?;
            Ok(PriorCandidate { stored, receipt })
        })
        .collect()
}

fn persist_proposal(
    connection: &DbConnection,
    receipt: &HarvestReceipt,
    window: Window,
    actor: Option<&str>,
) -> Result<(), DomainError> {
    let source = receipt_source(receipt)?;
    let candidate_id = receipt_candidate_id(&source);
    if connection
        .get_curation_candidate(&receipt.workspace_id, &candidate_id)
        .map_err(|_| storage("check proposal identity"))?
        .is_some()
    {
        return Err(stale());
    }
    for proof in &receipt.proofs {
        let evidence = CreateOutcomeEvidenceInput {
            workspace_id: receipt.workspace_id.clone(),
            source: OutcomeEvidenceSource::VerifierSuccess,
            signal_direction: "positive".to_owned(),
            evidence_ref: proof.audit_id.clone(),
            agent_id: None,
            task_id: None,
            run_id: None,
            observed_at: proof.finished_at.clone(),
        };
        // Do not allow INSERT OR IGNORE to hide an incompatible evidence row.
        let rows = connection.query("SELECT evidence_family, signal_direction, base_weight_milli, agent_id, task_id, run_id, provenance_hash FROM outcome_evidence_rows WHERE workspace_id = ?1 AND source_kind = 'verifier_success' AND evidence_ref = ?2 AND observed_at = ?3", &[Value::Text(receipt.workspace_id.clone()), Value::Text(proof.audit_id.clone()), Value::Text(proof.finished_at.clone())]).map_err(|_| storage("check outcome evidence identity"))?;
        if let Some(row) = rows.first() {
            let expected = StoredOutcomeEvidence {
                workspace_id: evidence.workspace_id.clone(),
                source: evidence.source,
                evidence_family: "verification".to_owned(),
                signal_direction: evidence.signal_direction.clone(),
                base_weight_milli: evidence.source.base_weight_milli(),
                evidence_ref: evidence.evidence_ref.clone(),
                agent_id: None,
                task_id: None,
                run_id: None,
                observed_at: evidence.observed_at.clone(),
                provenance_hash: String::new(),
                created_at: String::new(),
            }
            .computed_provenance_hash();
            if rows.len() != 1
                || row.get(0).and_then(Value::as_str) != Some("verification")
                || row.get(1).and_then(Value::as_str) != Some("positive")
                || row.get(2).and_then(Value::as_i64) != Some(400)
                || !(3..6).all(|index| matches!(row.get(index), Some(Value::Null)))
                || row.get(6).and_then(Value::as_str) != Some(expected.as_str())
            {
                return Err(stale());
            }
        }
        connection
            .insert_outcome_evidence_rows(&[evidence])
            .map_err(|_| storage("persist outcome evidence"))?;
    }
    connection
        .insert_curation_candidate(
            &candidate_id,
            &CreateCurationCandidateInput {
                workspace_id: receipt.workspace_id.clone(),
                candidate_type: "promote".to_owned(),
                target_memory_id: Some(receipt.source.memory_id.clone()),
                proposed_content: None,
                proposed_confidence: None,
                proposed_trust_class: None,
                source_type: "rule_engine".to_owned(),
                source_id: Some(source),
                reason: PROPOSAL_REASON.to_owned(),
                confidence: 0.4,
                status: Some("pending".to_owned()),
                created_at: None,
                ttl_expires_at: None,
                derivation_source_refs_json: None,
                derivation_metadata_json: None,
            },
        )
        .map_err(|_| storage("persist the pending review candidate"))?;
    connection.insert_audit(&generate_audit_id(), &CreateAuditInput {
        workspace_id: Some(receipt.workspace_id.clone()), actor: actor.map(str::to_owned),
        action: audit_actions::CURATION_CANDIDATE_CREATE.to_owned(), target_type: Some("curation_candidate".to_owned()), target_id: Some(candidate_id),
        details: Some(json!({"schema": RECEIPT_SCHEMA, "producer": "outcome_harvest", "since": timestamp(window.since), "until": timestamp(window.until), "receipt": receipt, "baseWeightMilli": 400, "reviewRequired": true, "liveFeedbackWritten": false}).to_string()),
    }).map_err(|_| storage("audit the pending review candidate"))?;
    Ok(())
}

/// Ordinary candidates retain their existing behavior. Harvest proposals must
/// retain their exact immutable definition and current source/proof authority.
pub(crate) fn validate_candidate(
    connection: &DbConnection,
    workspace_id: &str,
    candidate: &StoredCurationCandidate,
) -> Result<(), DomainError> {
    if !candidate
        .source_id
        .as_deref()
        .is_some_and(|source| source.starts_with("outcome-harvest:"))
    {
        return Ok(());
    }
    if candidate.workspace_id != workspace_id {
        return Err(stale());
    }
    source_admission::with_snapshot(connection, || {
        validate_candidate_in_snapshot(connection, candidate, Utc::now())
    })
}

fn validate_candidate_in_snapshot(
    connection: &DbConnection,
    candidate: &StoredCurationCandidate,
    now: DateTime<Utc>,
) -> Result<(), DomainError> {
    let receipt = decoded_candidate(candidate)?;
    let current = connection
        .get_curation_candidate(&candidate.workspace_id, &candidate.id)
        .map_err(|_| storage("recheck the review candidate"))?
        .ok_or_else(stale)?;
    if &current != candidate {
        return Err(stale());
    }
    if candidate.status == "applied" && candidate.applied_at.is_some() {
        return Ok(());
    }
    let source = source_receipt(
        connection,
        &receipt.workspace_id,
        &receipt.source.memory_id,
        now,
    )
    .map_err(|_| stale())?;
    if source != receipt.source {
        return Err(stale());
    }
    let mut selection_budget = MAX_SELECTED_PER_PACK;
    let pack = load_pack_evidence(
        connection,
        &receipt.workspace_id,
        &receipt.pack_id,
        &mut selection_budget,
    )
    .map_err(|_| stale())?;
    let selected = pack
        .selected
        .get(&receipt.source.memory_id)
        .ok_or_else(stale)?;
    let impression = pack
        .impressions
        .get(&receipt.source.memory_id)
        .ok_or_else(stale)?;
    if !source_was_unchanged_at_pack(&source, &pack.record, selected)
        || pack.record.pack_hash != receipt.pack_hash
        || pack.record.ledger_hash.as_deref() != Some(receipt.ledger_hash.as_str())
        || pack.record.created_at != receipt.pack_created_at
        || json_hash(selected) != receipt.selected_item_hash
        || json_hash(impression) != receipt.impression_hash
        || !receipt
            .proofs
            .iter()
            .all(|proof| pack.proofs.contains(proof))
    {
        return Err(stale());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::curate::{
        CurateApplyOptions, CurateReviewAction, CurateReviewOptions, CurateValidateOptions,
        apply_curation_candidate, review_curation_candidate, validate_curation_candidate,
    };
    use crate::core::verify::{
        VerificationEvidenceAuthority, VerificationRecordOptions,
        record_validated_verification_evidence,
    };
    use crate::db::{
        CreateMemoryInput, CreatePackItemInput, CreatePackRecordInput, CreateWorkspaceInput,
    };
    use sha2::Digest;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    const PACKED_AT: &str = "2026-01-01T00:00:00Z";
    const FIRST: &str = "2026-01-01T00:00:01.000000001Z";
    const SECOND: &str = "2026-01-01T01:00:02.000000001+01:00";

    struct Fixture {
        _temporary: tempfile::TempDir,
        root: PathBuf,
        database: PathBuf,
        workspace: String,
        memory: String,
        pack: String,
    }

    impl Fixture {
        fn new() -> Result<Self, Box<dyn std::error::Error>> {
            let temporary = tempfile::tempdir()?;
            let root = temporary.path().canonicalize()?;
            std::fs::create_dir_all(root.join(".ee"))?;
            let fixture = Self {
                database: root.join(".ee/harvest.db"),
                root,
                workspace: crate::models::WorkspaceId::from_uuid(uuid::Uuid::now_v7()).to_string(),
                memory: MemoryId::from_uuid(uuid::Uuid::now_v7()).to_string(),
                pack: PackId::from_uuid(uuid::Uuid::now_v7()).to_string(),
                _temporary: temporary,
            };
            let db = fixture.connection()?;
            db.migrate()?;
            db.insert_workspace(
                &fixture.workspace,
                &CreateWorkspaceInput {
                    path: fixture.root.to_string_lossy().into_owned(),
                    name: None,
                },
            )?;
            db.insert_memory_at(
                &fixture.memory,
                &CreateMemoryInput {
                    workspace_id: fixture.workspace.clone(),
                    level: "episodic".to_owned(),
                    kind: "fact".to_owned(),
                    content: "The retry worker completed the retained recovery procedure."
                        .to_owned(),
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.7,
                    importance: 0.6,
                    provenance_uri: Some("manual://harvest-source".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: vec![],
                    valid_from: None,
                    valid_to: None,
                },
                DateTime::parse_from_rfc3339("2025-12-31T00:00:00Z")?.with_timezone(&Utc),
            )?;
            fixture.insert_pack(&db, &fixture.pack)?;
            db.close()?;
            Ok(fixture)
        }

        fn connection(&self) -> crate::db::Result<DbConnection> {
            DbConnection::open_file(&self.database)
        }

        fn insert_pack(&self, db: &DbConnection, pack: &str) -> crate::db::Result<()> {
            db.insert_pack_record_at(
                pack,
                &CreatePackRecordInput {
                    task_paths: vec![],
                    workspace_id: self.workspace.clone(),
                    query: "recovery procedure".to_owned(),
                    profile: "balanced".to_owned(),
                    max_tokens: 500,
                    used_tokens: 30,
                    item_count: 1,
                    omitted_count: 0,
                    pack_hash: hash(pack.as_bytes()),
                    degraded_json: None,
                    created_by: Some("harvest-test".to_owned()),
                },
                &[CreatePackItemInput {
                    pack_id: pack.to_owned(),
                    memory_id: self.memory.clone(),
                    rank: 1,
                    section: "facts".to_owned(),
                    estimated_tokens: 30,
                    relevance: 0.9,
                    utility: 0.7,
                    combined_score: Some(0.85),
                    attempt_family_multiplicity: None,
                    why: "Retained recovery evidence".to_owned(),
                    diversity_key: None,
                    provenance_json: "{\"uri\":\"manual://harvest-source\"}".to_owned(),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                }],
                &[],
                PACKED_AT,
            )
        }

        fn proof(
            &self,
            pack: &str,
            command: &str,
            start: Option<&str>,
            finish: &str,
        ) -> TestResult {
            let evidence = crate::models::verification_evidence_record_from_rch_verify(&json!({
                "schema": crate::models::RCH_VERIFY_SCHEMA_V1, "command_text": command,
                "command_hash": format!("{:x}", sha2::Sha256::digest(command.as_bytes())),
                "command_kind": "cargo_test", "status": "remote_pass", "exit_code": 0,
                "worker_id": "retained-proof-worker", "remote_required": true,
                "started_at": start, "finished_at": finish,
            }))?;
            record_validated_verification_evidence(
                VerificationRecordOptions {
                    database_path: &self.database,
                    workspace_path: &self.root,
                    target_type: "pack",
                    target_id: pack,
                    actor: Some("harvest-test"),
                    evidence,
                },
                VerificationEvidenceAuthority::ValidatedRchVerify,
            )?;
            Ok(())
        }

        fn pair(&self, pack: &str) -> TestResult {
            self.proof(
                pack,
                &format!("cargo test --lib recovery_{pack}"),
                Some(PACKED_AT),
                FIRST,
            )?;
            self.proof(
                pack,
                &format!("cargo test --lib retry_{pack}"),
                Some(FIRST),
                SECOND,
            )
        }

        fn options(&self, apply: bool) -> OutcomeHarvestOptions {
            OutcomeHarvestOptions {
                workspace_path: self.root.clone(),
                database_path: Some(self.database.clone()),
                since: PACKED_AT.to_owned(),
                until: "2026-01-02T00:00:00Z".to_owned(),
                apply,
                pack_limit: DEFAULT_HARVEST_PACK_LIMIT,
                pack_id: None,
                actor: Some("harvest-reviewer".to_owned()),
            }
        }

        fn counts(&self) -> Result<(i64, i64, i64, i64), Box<dyn std::error::Error>> {
            let db = self.connection()?;
            let rows = db.query("SELECT (SELECT count(*) FROM curation_candidates), (SELECT count(*) FROM outcome_evidence_rows), (SELECT count(*) FROM audit_log), (SELECT count(*) FROM feedback_events)", &[])?;
            let row = rows.first().ok_or("missing counts")?;
            Ok((
                row.get(0)
                    .and_then(Value::as_i64)
                    .ok_or("candidate count")?,
                row.get(1).and_then(Value::as_i64).ok_or("evidence count")?,
                row.get(2).and_then(Value::as_i64).ok_or("audit count")?,
                row.get(3).and_then(Value::as_i64).ok_or("feedback count")?,
            ))
        }

        fn feedback(&self, target_type: &str) -> TestResult {
            self.connection()?.insert_feedback_event(
                &format!("feedback_{}", uuid::Uuid::now_v7().simple()),
                &crate::db::CreateFeedbackEventInput {
                    workspace_id: self.workspace.clone(),
                    target_type: target_type.to_owned(),
                    target_id: self.memory.clone(),
                    signal: "helpful".to_owned(),
                    weight: 1.0,
                    source_type: "outcome_observed".to_owned(),
                    source_id: None,
                    reason: None,
                    evidence_json: None,
                    session_id: None,
                },
            )?;
            Ok(())
        }
    }

    #[test]
    fn harvest_replays_equivalent_windows_and_added_proof_then_requires_review_for_promotion()
    -> TestResult {
        let fixture = Fixture::new()?;
        fixture.pair(&fixture.pack)?;
        let original = fixture.counts()?;
        let before = fixture
            .connection()?
            .get_memory(&fixture.memory)?
            .ok_or("memory missing")?;
        let preview = harvest_outcomes(&fixture.options(false))?;
        assert_eq!(preview.proposed_count, 1);
        assert_eq!(preview.proposals[0].source_binding, "unchanged_since_pack");
        assert_eq!(fixture.counts()?, original);
        let first = harvest_outcomes(&fixture.options(true))?;
        let candidate = first.proposals[0].candidate_id.clone();
        assert_eq!(first.persisted_count, 1);
        assert_eq!(
            fixture.connection()?.get_memory(&fixture.memory)?,
            Some(before.clone())
        );
        assert_eq!(fixture.counts()?.3, 0);
        let after = fixture.counts()?;
        let mut equivalent = fixture.options(true);
        equivalent.since = "2026-01-01T01:00:00+01:00".to_owned();
        equivalent.until = "2026-01-02T01:00:00+01:00".to_owned();
        let replay = harvest_outcomes(&equivalent)?;
        assert_eq!(replay.replayed_count, 1);
        assert_eq!(replay.proposals[0].candidate_id, candidate);
        assert_eq!(fixture.counts()?, after);
        fixture.proof(
            &fixture.pack,
            "cargo test --lib added_later",
            Some(SECOND),
            "2026-01-01T00:00:03Z",
        )?;
        let after_added_proof = fixture.counts()?;
        assert_eq!(harvest_outcomes(&equivalent)?.replayed_count, 1);
        assert_eq!(fixture.counts()?, after_added_proof);

        // An identical retained proof imported under an earlier audit identity
        // must not hide the original receipt or reopen this attribution.
        let db = fixture.connection()?;
        let stored = db
            .get_curation_candidate(&fixture.workspace, &candidate)?
            .ok_or("candidate missing")?;
        let receipt = decoded_candidate(&stored)?;
        let proof = db
            .get_audit(&receipt.proofs[0].audit_id)?
            .ok_or("proof missing")?;
        let earlier_audit = "audit_00000000000000000000000000000000";
        assert!(earlier_audit < receipt.proofs[0].audit_id.as_str());
        db.insert_audit(
            earlier_audit,
            &CreateAuditInput {
                workspace_id: proof.workspace_id,
                actor: proof.actor,
                action: proof.action,
                target_type: proof.target_type,
                target_id: proof.target_id,
                details: proof.details,
            },
        )?;
        db.close()?;
        let after_duplicate_proof = fixture.counts()?;
        let duplicate_replay = harvest_outcomes(&equivalent)?;
        assert_eq!(duplicate_replay.replayed_count, 1);
        assert_eq!(duplicate_replay.proposals[0].candidate_id, candidate);
        assert_eq!(fixture.counts()?, after_duplicate_proof);

        let validation = validate_curation_candidate(&CurateValidateOptions {
            workspace_path: &fixture.root,
            database_path: Some(&fixture.database),
            candidate_id: &candidate,
            actor: Some("human-reviewer"),
            dry_run: true,
        })?;
        assert_eq!(validation.validation.status, "passed");
        assert!(!validation.durable_mutation);
        let accept = review_curation_candidate(&CurateReviewOptions {
            workspace_path: &fixture.root,
            database_path: Some(&fixture.database),
            candidate_id: &candidate,
            action: CurateReviewAction::Accept,
            actor: Some("human-reviewer"),
            dry_run: false,
            snoozed_until: None,
            reason: Some("Reviewed both independent retained verification records"),
            merge_into_candidate_id: None,
        })?;
        assert!(accept.mutation.persisted);
        let apply_options = CurateApplyOptions {
            workspace_path: &fixture.root,
            database_path: Some(&fixture.database),
            candidate_id: &candidate,
            actor: Some("human-reviewer"),
            dry_run: false,
            allow_tombstone_load_bearing: false,
        };
        let applied = apply_curation_candidate(&apply_options)?;
        assert_eq!(applied.application.status, "applied");
        let current = fixture
            .connection()?
            .get_memory(&fixture.memory)?
            .ok_or("memory missing")?;
        assert_eq!(current.level, "semantic");
        assert_eq!(current.confidence, before.confidence);
        assert_eq!(current.content, before.content);
        assert_eq!(current.trust_class, before.trust_class);
        let terminal = fixture.counts()?;
        assert_eq!(harvest_outcomes(&equivalent)?.replayed_count, 1);
        assert_eq!(
            apply_curation_candidate(&apply_options)?.application.status,
            "already_applied"
        );
        assert_eq!(fixture.counts()?, terminal);
        Ok(())
    }

    #[test]
    fn harvest_uses_exact_completion_window_and_pack_bound_run_intervals() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.pair(&fixture.pack)?;
        let mut options = fixture.options(false);
        options.since = FIRST.to_owned();
        options.until = "2026-01-01T00:00:02.000000002Z".to_owned();
        assert_eq!(harvest_outcomes(&options)?.proposed_count, 1);
        options.until = "2026-01-01T00:00:02.000000001Z".to_owned();
        assert_eq!(
            harvest_outcomes(&options)?.proposed_count,
            0,
            "until is exclusive at one nanosecond"
        );
        options.until = "2026-01-01T00:00:02.000000002Z".to_owned();
        options.since = "2026-01-01T00:00:01.000000002Z".to_owned();
        assert_eq!(
            harvest_outcomes(&options)?.proposed_count,
            0,
            "since compares full precision"
        );

        for (start, finish) in [
            (Some("2025-12-31T23:59:59.999999999Z"), SECOND),
            (Some("2026-01-01T00:00:03Z"), SECOND),
            (None, SECOND),
            (Some(PACKED_AT), "2099-01-01T00:00:00Z"),
        ] {
            let bounded = Fixture::new()?;
            bounded.proof(
                &bounded.pack,
                "cargo test --lib one_valid",
                Some(PACKED_AT),
                FIRST,
            )?;
            bounded.proof(
                &bounded.pack,
                "cargo test --lib invalid_interval",
                start,
                finish,
            )?;
            let mut window = bounded.options(false);
            if finish.starts_with("2099-") {
                window.until = "2100-01-01T00:00:00Z".to_owned();
            }
            assert_eq!(harvest_outcomes(&window)?.proposed_count, 0);
        }
        let repeated = Fixture::new()?;
        repeated.proof(
            &repeated.pack,
            "cargo test --lib same_command",
            Some(PACKED_AT),
            FIRST,
        )?;
        repeated.proof(
            &repeated.pack,
            "cargo test --lib same_command",
            Some(FIRST),
            SECOND,
        )?;
        assert_eq!(
            harvest_outcomes(&repeated.options(false))?.proposed_count,
            0,
            "rerunning the same command is not independent corroboration"
        );
        Ok(())
    }

    #[test]
    fn harvest_withholds_changed_closed_held_and_known_feedback_sources() -> TestResult {
        for case in [
            "changed",
            "expired",
            "tombstoned",
            "semantic",
            "sealed",
            "held",
            "feedback",
        ] {
            let fixture = Fixture::new()?;
            fixture.pair(&fixture.pack)?;
            let db = fixture.connection()?;
            match case {
                "changed" => {
                    db.execute("UPDATE memories SET content = 'Changed after this historical pack', updated_at = '2026-01-01T00:00:00.000000001Z' WHERE id = ?1", &[Value::Text(fixture.memory.clone())])?;
                }
                "expired" => {
                    db.execute(
                        "UPDATE memories SET valid_to = '2026-01-02T00:00:00Z' WHERE id = ?1",
                        &[Value::Text(fixture.memory.clone())],
                    )?;
                }
                "tombstoned" => {
                    db.execute(
                        "UPDATE memories SET tombstoned_at = '2026-01-02T00:00:00Z' WHERE id = ?1",
                        &[Value::Text(fixture.memory.clone())],
                    )?;
                }
                "semantic" => {
                    db.execute(
                        "UPDATE memories SET level = 'semantic' WHERE id = ?1",
                        &[Value::Text(fixture.memory.clone())],
                    )?;
                }
                "sealed" => {
                    db.insert_memory_seal(
                        &fixture.memory,
                        &hash(b"sealed source"),
                        "2026-01-02T00:00:00Z",
                    )?;
                }
                "held" => {
                    db.insert_feedback_quarantine(
                        "fq_00000000000000000000000001",
                        &crate::db::CreateFeedbackQuarantineInput {
                            workspace_id: fixture.workspace.clone(),
                            source_id: "pending-review".to_owned(),
                            target_type: "memory".to_owned(),
                            target_id: fixture.memory.clone(),
                            signal: "harmful".to_owned(),
                            weight: 1.0,
                            source_type: "outcome_observed".to_owned(),
                            proposed_event_id: None,
                            recorded_at: FIRST.to_owned(),
                            reason: "Review first".to_owned(),
                            event_reason: None,
                            evidence_json: None,
                            session_id: None,
                            raw_event_hash: hash(b"pending-review"),
                        },
                    )?;
                }
                "feedback" => fixture.feedback("memory")?,
                _ => unreachable!(),
            }
            db.close()?;
            let before = fixture.counts()?;
            let report = harvest_outcomes(&fixture.options(true))?;
            assert_eq!(report.proposed_count, 0, "{case}");
            assert_eq!(report.persisted_count, 0, "{case}");
            assert_eq!(
                fixture.counts()?,
                before,
                "withheld {case} must create no candidate/evidence/audit"
            );
            if case == "changed" {
                assert_eq!(
                    report.skipped[0].reason,
                    "source_not_provably_unchanged_since_pack"
                );
            }
        }
        let typed = Fixture::new()?;
        typed.pair(&typed.pack)?;
        typed.feedback("rule")?;
        assert_eq!(
            harvest_outcomes(&typed.options(false))?.proposed_count,
            1,
            "same-string rule feedback cannot alias a memory"
        );
        Ok(())
    }

    #[test]
    fn harvest_rolls_back_partial_proposals_and_caps_only_new_receipts() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.pair(&fixture.pack)?;
        let before = fixture.counts()?;
        let db = fixture.connection()?;
        db.execute("CREATE TRIGGER harvest_audit_refusal BEFORE INSERT ON audit_log WHEN NEW.action = 'curation_candidate.create' BEGIN SELECT RAISE(ABORT, 'harvest-test-refusal'); END", &[])?;
        db.close()?;
        assert!(harvest_outcomes(&fixture.options(true)).is_err());
        assert_eq!(
            fixture.counts()?,
            before,
            "audit failure must roll back candidate and evidence rows"
        );
        fixture
            .connection()?
            .execute("DROP TRIGGER harvest_audit_refusal", &[])?;
        assert_eq!(harvest_outcomes(&fixture.options(true))?.persisted_count, 1);
        for _ in 0..3 {
            let pack = PackId::from_uuid(uuid::Uuid::now_v7()).to_string();
            let db = fixture.connection()?;
            fixture.insert_pack(&db, &pack)?;
            db.close()?;
            fixture.pair(&pack)?;
        }
        let capped = harvest_outcomes(&fixture.options(true))?;
        assert_eq!(capped.persisted_count, 2);
        assert_eq!(capped.replayed_count, 1);
        assert_eq!(capped.capped_count, 1);
        let counts = fixture.counts()?;
        let again = harvest_outcomes(&fixture.options(true))?;
        assert_eq!(again.persisted_count, 0);
        assert_eq!(again.replayed_count, 3);
        assert_eq!(again.capped_count, 1);
        assert_eq!(fixture.counts()?, counts);
        Ok(())
    }

    #[test]
    fn harvest_review_and_apply_recheck_later_default_outcome_feedback() -> TestResult {
        for feedback_after_accept in [false, true] {
            let fixture = Fixture::new()?;
            fixture.pair(&fixture.pack)?;
            let candidate = harvest_outcomes(&fixture.options(true))?.proposals[0]
                .candidate_id
                .clone();
            if feedback_after_accept {
                let validation = validate_curation_candidate(&CurateValidateOptions {
                    workspace_path: &fixture.root,
                    database_path: Some(&fixture.database),
                    candidate_id: &candidate,
                    actor: Some("human-reviewer"),
                    dry_run: false,
                })?;
                assert_eq!(validation.validation.status, "passed");
            }
            fixture.feedback("memory")?;
            let before = fixture.counts()?;
            let validation = validate_curation_candidate(&CurateValidateOptions {
                workspace_path: &fixture.root,
                database_path: Some(&fixture.database),
                candidate_id: &candidate,
                actor: Some("human-reviewer"),
                dry_run: true,
            })?;
            assert!(
                validation
                    .validation
                    .errors
                    .iter()
                    .any(|issue| issue.code == HARVEST_RECEIPT_CHANGED)
            );
            let applied = apply_curation_candidate(&CurateApplyOptions {
                workspace_path: &fixture.root,
                database_path: Some(&fixture.database),
                candidate_id: &candidate,
                actor: Some("human-reviewer"),
                dry_run: false,
                allow_tombstone_load_bearing: false,
            })?;
            assert!(!applied.durable_mutation);
            assert!(
                applied
                    .application
                    .errors
                    .iter()
                    .any(|issue| issue.code == HARVEST_RECEIPT_CHANGED)
            );
            let accepted = review_curation_candidate(&CurateReviewOptions {
                workspace_path: &fixture.root,
                database_path: Some(&fixture.database),
                candidate_id: &candidate,
                action: CurateReviewAction::Accept,
                actor: Some("human-reviewer"),
                dry_run: false,
                snoozed_until: None,
                reason: None,
                merge_into_candidate_id: None,
            });
            assert!(accepted.is_err());
            assert_eq!(fixture.counts()?, before);
            assert_eq!(
                fixture
                    .connection()?
                    .get_memory(&fixture.memory)?
                    .ok_or("missing memory")?
                    .level,
                "episodic"
            );
        }
        Ok(())
    }

    #[test]
    fn harvest_requires_comparable_disjoint_command_digests() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.pair(&fixture.pack)?;
        let db = fixture.connection()?;
        let mut selection_budget = MAX_SELECTED_PER_PACK;
        let pack = source_admission::with_snapshot(&db, || {
            load_pack_evidence(
                &db,
                &fixture.workspace,
                &fixture.pack,
                &mut selection_budget,
            )
            .map_err(|_| stale())
        })?;
        let first = &pack.proofs[0];
        let mut second = pack.proofs[1].clone();
        assert!(commands_provably_differ(first, &second));
        second.authority = "validated_run_record".to_owned();
        assert!(
            !commands_provably_differ(first, &second),
            "different raw-text and argv digests cannot establish command independence"
        );
        assert!(independent_pair(&[first.clone(), second.clone()]).is_none());
        second.authority = first.authority.clone();
        second.command_aliases = vec![first.command_aliases[0].clone()];
        assert!(
            !commands_provably_differ(first, &second),
            "same raw command under another producer/algorithm is not independent"
        );
        let mut opaque = first.clone();
        opaque.authority = "validated_run_record".to_owned();
        second.authority = "validated_run_record".to_owned();
        opaque
            .command_aliases
            .retain(|digest| digest.starts_with("sha256:"));
        second.command_aliases = vec![hash(b"unknown raw command")];
        assert!(
            !commands_provably_differ(&opaque, &second),
            "opaque cross-algorithm summaries cannot prove different commands"
        );
        Ok(())
    }

    #[test]
    fn harvest_rejects_unbound_impressions_and_rechecks_saved_receipts() -> TestResult {
        for field in ["query_hash", "lens_hash"] {
            let fixture = Fixture::new()?;
            fixture.pair(&fixture.pack)?;
            let candidate = harvest_outcomes(&fixture.options(true))?.proposals[0]
                .candidate_id
                .clone();
            let db = fixture.connection()?;
            db.execute(
                &format!("UPDATE pack_candidate_impressions SET {field} = ?1 WHERE pack_id = ?2"),
                &[
                    Value::Text(hash(b"different selection request")),
                    Value::Text(fixture.pack.clone()),
                ],
            )?;
            let stored = db
                .get_curation_candidate(&fixture.workspace, &candidate)?
                .ok_or("missing candidate")?;
            assert!(validate_candidate(&db, &fixture.workspace, &stored).is_err());
            let before = fixture.counts()?;
            let report = harvest_outcomes(&fixture.options(true))?;
            assert_eq!(report.persisted_count, 0);
            assert_eq!(report.replayed_count, 0);
            assert_eq!(report.skipped[0].reason, "impression_selection_mismatch");
            assert_eq!(fixture.counts()?, before);
        }
        Ok(())
    }

    #[test]
    fn harvest_selection_work_is_charged_even_when_later_proof_is_invalid() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.pair(&fixture.pack)?;
        let db = fixture.connection()?;
        // Corruption is rejected after the selected ledger/impressions were
        // examined. That failed pack still consumes its aggregate work slot.
        db.insert_audit(
            &generate_audit_id(),
            &CreateAuditInput {
                workspace_id: Some(fixture.workspace.clone()),
                actor: None,
                action: audit_actions::VERIFICATION_INGEST.to_owned(),
                target_type: Some("pack".to_owned()),
                target_id: Some(fixture.pack.clone()),
                details: Some("invalid retained proof".to_owned()),
            },
        )?;
        let mut budget = 1;
        source_admission::with_snapshot(&db, || {
            assert!(
                load_pack_evidence(&db, &fixture.workspace, &fixture.pack, &mut budget).is_err()
            );
            assert_eq!(budget, 0);
            let second = load_pack_evidence(&db, &fixture.workspace, &fixture.pack, &mut budget);
            assert!(matches!(second, Err(reason) if reason == "selection_work_limit_reached"));
            Ok(())
        })?;
        Ok(())
    }
}
