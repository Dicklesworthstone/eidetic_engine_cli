//! Bounded, extractive episodic-to-semantic proposals. Sources remain intact;
//! ordinary create-derived validation and apply own every durable memory write.

use super::*;
use crate::core::ask::{score_span, segment_spans, texts_conflict, tokenize_for_ask};
use crate::curate::cluster_coherence::{
    ClusterCoherenceConfig, CoherentCluster, EmbeddingPoint, agglomerate,
};
use crate::models::memory::MemoryConsolidationMetadata;
use serde_json::{Value, json};

const PRODUCER: &str = "episodic_summary";
const SCHEMA: &str = "ee.curate.episodic_summary.v1";
const MAX_SOURCES: usize = 128;
const MAX_SOURCE_BYTES: usize = 16 * 1024;
const MAX_MEMBERS: usize = 16;
const MAX_SPAN_BYTES: usize = 2048;
const MAX_SUMMARY_BYTES: usize = 8 * 1024;
const MIN_MEMBERS: usize = 3;
const TOPICAL_THRESHOLD: f64 = 0.78;
const CONFIDENCE_CAP: f32 = 0.60;
const SOURCE_LIMIT_CODE: &str = "curate_summary_source_limit";
const SOURCE_LIMIT_REPAIR: &str = "Use --target-memory <source-id> to inspect a bounded source neighborhood, or use ee curate propose-derived with explicitly reviewed sources.";

#[derive(Clone)]
struct Source {
    memory: StoredMemory,
    metadata: MemoryConsolidationMetadata,
}

pub(super) fn requested(raw: Option<&str>) -> bool {
    raw.is_some_and(|value| value.trim().eq_ignore_ascii_case("summary"))
}

pub(super) fn is_summary(candidate: &StoredCurationCandidate) -> bool {
    candidate.candidate_type == CandidateType::CreateDerivedMemory.as_str()
        && (candidate.source_id.as_deref() == Some(PRODUCER)
            || parse_derivation_metadata(candidate)
                .is_ok_and(|metadata| metadata.producer.producer == PRODUCER))
}

pub(super) fn matches_source(candidate: &StoredCurationCandidate, target: &str) -> bool {
    parse_derivation_source_refs(candidate).is_ok_and(|refs| {
        refs.iter()
            .any(|source| source.kind == DerivationSourceKind::Memory && source.id == target)
    })
}

pub(super) fn proposal_command(
    prepared: &PreparedCurateRead,
    target_memory_id: Option<&str>,
) -> String {
    let workspace = shell_quote_command_arg(&prepared.workspace_path.display().to_string());
    let database = shell_quote_command_arg(&prepared.database_path.display().to_string());
    let target = target_memory_id.map_or_else(String::new, |id| {
        format!(" --target-memory {}", shell_quote_command_arg(id))
    });
    format!(
        "ee curate candidates --type summary --propose --workspace {workspace} --database {database}{target} --json"
    )
}

fn unavailable(message: impl Into<String>) -> DomainError {
    DomainError::Storage {
        message: message.into(),
        repair: Some("Run ee doctor --json and retry episodic summary discovery.".to_owned()),
    }
}

fn checkpoint() -> Result<(), DomainError> {
    if let Some(cx) = asupersync::Cx::current() {
        cx.checkpoint()
            .map_err(|_| unavailable("Episodic summary operation was cancelled."))?;
    }
    Ok(())
}

fn storage(error: DbError) -> DomainError {
    unavailable(format!(
        "Could not read episodic summary source state: {error}"
    ))
}

fn config() -> ClusterCoherenceConfig {
    ClusterCoherenceConfig {
        merge_threshold: TOPICAL_THRESHOLD,
        silhouette_cutoff: 0.40,
        min_cluster_size: MIN_MEMBERS,
    }
}

/// The caller owns either a coherent read snapshot or the proposal writer
/// transaction. Limits apply before bodies or embeddings can grow with the
/// corpus. An extra row makes a bounded read distinguishable from exhaustion.
pub(super) fn append_candidates(
    connection: &DbConnection,
    workspace_id: &str,
    target: Option<&str>,
    stored: &mut Vec<StoredCurationCandidate>,
    persist: bool,
    degraded: &mut Vec<CurateCandidatesDegradation>,
) -> Result<usize, DomainError> {
    checkpoint()?;
    let mut memories = connection
        .list_episodic_summary_sources(workspace_id, target, (MAX_SOURCES + 1) as u32)
        .map_err(storage)?;
    let mut limited = memories.len() > MAX_SOURCES;
    memories.truncate(MAX_SOURCES);
    let ids = memories
        .iter()
        .map(|memory| memory.id.as_str())
        .collect::<Vec<_>>();
    let mut metadata = connection
        .memory_consolidation_metadata_in_current_snapshot(&ids)
        .map_err(storage)?;
    let now = Utc::now();
    let mut groups: Vec<Vec<Source>> = Vec::new();
    for memory in memories {
        checkpoint()?;
        if memory.content.len() > MAX_SOURCE_BYTES {
            limited = true;
            continue;
        }
        let sidecars = metadata.remove(&memory.id).ok_or_else(|| {
            unavailable("Episodic summary source metadata disappeared from its snapshot.")
        })?;
        let source = Source {
            memory,
            metadata: sidecars,
        };
        if !eligible(connection, &source, workspace_id, now)? {
            continue;
        }
        if let Some(group) = groups
            .iter_mut()
            .find(|group| compatible(&group[0], &source))
        {
            // Repeated identical captures cannot manufacture source diversity.
            // The targeted reader puts its source first, so that identity is
            // retained when another row contains exactly the same body.
            if !group
                .iter()
                .any(|existing| existing.memory.content == source.memory.content)
            {
                group.push(source);
            }
        } else {
            groups.push(vec![source]);
        }
    }

    let mut count = 0;
    for group in groups {
        checkpoint()?;
        if group.len() < MIN_MEMBERS {
            continue;
        }
        let report = clusters(&group)?;
        for cluster in report.into_iter().filter(|cluster| cluster.accepted) {
            checkpoint()?;
            let mut members = group
                .iter()
                .filter(|source| cluster.member_memory_ids.contains(&source.memory.id))
                .cloned()
                .collect::<Vec<_>>();
            if target.is_some_and(|target| !members.iter().any(|source| source.memory.id == target))
            {
                continue;
            }
            // Opposing sources in an oversized cluster must not disappear
            // merely because only a bounded subset fits one proposal.
            if conflicting(connection, &members)? {
                continue;
            }
            members.sort_by(|left, right| {
                let left_target = target == Some(left.memory.id.as_str());
                let right_target = target == Some(right.memory.id.as_str());
                right_target
                    .cmp(&left_target)
                    .then_with(|| left.memory.id.cmp(&right.memory.id))
            });
            limited |= members.len() > MAX_MEMBERS;
            members.truncate(MAX_MEMBERS);
            members.sort_by(|left, right| left.memory.id.cmp(&right.memory.id));
            let Some(candidate) = build_candidate(workspace_id, &members)? else {
                continue;
            };
            if connection
                .get_curation_candidate(workspace_id, &candidate.id)
                .map_err(storage)?
                .is_some()
            {
                continue;
            }
            checkpoint()?;
            if persist {
                persist_candidate(connection, &candidate)?;
            }
            stored.push(candidate);
            count += 1;
        }
    }
    if limited {
        degraded.push(CurateCandidatesDegradation {
            code: SOURCE_LIMIT_CODE.to_owned(),
            severity: "warning".to_owned(),
            message: format!(
                "Episodic summary discovery is incomplete: the bounded source window or proposal limit was reached (at most {MAX_SOURCES} source rows, {MAX_SOURCE_BYTES} bytes per source, and {MAX_MEMBERS} sources per proposal). Additional related memories may exist."
            ),
            repair: SOURCE_LIMIT_REPAIR.to_owned(),
        });
    }
    checkpoint()?;
    Ok(count)
}

fn compatible(left: &Source, right: &Source) -> bool {
    left.memory.consolidation_view().scope == right.memory.consolidation_view().scope
        && left.metadata.tags == right.metadata.tags
}

fn eligible(
    connection: &DbConnection,
    source: &Source,
    workspace_id: &str,
    now: DateTime<Utc>,
) -> Result<bool, DomainError> {
    let memory = &source.memory;
    let metadata = &source.metadata;
    if memory.workspace_id != workspace_id
        || memory.level != MemoryLevel::Episodic.as_str()
        || !matches!(
            memory.kind.as_str(),
            "fact" | "decision" | "observation" | "note" | "summary"
        )
        || memory.content.trim().is_empty()
        || memory.content.len() > MAX_SOURCE_BYTES
        || memory.tombstoned_at.is_some()
        || metadata.superseded
        || metadata.has_sentinels
        || metadata.quarantined
        || metadata
            .typed_fields_json
            .as_deref()
            .is_some_and(|raw| raw != "{}")
        || metadata
            .anchors
            .iter()
            .any(|anchor| anchor.source == "explicit" || anchor.freshness != "current")
        || [memory.confidence, memory.utility, memory.importance]
            .into_iter()
            .any(|score| !score.is_finite() || !(0.0..=1.0).contains(&score))
        || TrustClass::from_str(&memory.trust_class).is_err()
        || crate::policy::redact_secret_like_content(&memory.content).redacted
        || crate::policy::detect_instruction_like_content(&memory.content).is_instruction_like
    {
        return Ok(false);
    }
    let parse =
        |value: &str| DateTime::parse_from_rfc3339(value).map(|value| value.with_timezone(&Utc));
    if !parse(&memory.created_at).is_ok_and(|created| created <= now)
        || memory
            .valid_from
            .as_deref()
            .is_some_and(|value| !parse(value).is_ok_and(|start| start <= now))
        || memory
            .valid_to
            .as_deref()
            .is_some_and(|value| !parse(value).is_ok_and(|end| end > now))
    {
        return Ok(false);
    }
    match source_admission::validate_memory_authority(connection, memory) {
        Ok(()) => Ok(true),
        Err(issue) if issue.code == "derived_source_memory_authority_unavailable" => {
            Err(unavailable(issue.message))
        }
        Err(_) => Ok(false),
    }
}

fn clusters(sources: &[Source]) -> Result<Vec<CoherentCluster>, DomainError> {
    let embedder = HashEmbedder::default_256();
    let mut points = Vec::with_capacity(sources.len());
    for source in sources {
        checkpoint()?;
        points.push(EmbeddingPoint::new(
            &source.memory.id,
            embedder
                .embed_sync(&source.memory.content)
                .into_iter()
                .map(f64::from)
                .collect(),
        ));
    }
    checkpoint()?;
    let report = agglomerate(&points, config()).map_err(|error| unavailable(error.to_string()))?;
    checkpoint()?;
    Ok(report.clusters)
}

fn conflicting(connection: &DbConnection, sources: &[Source]) -> Result<bool, DomainError> {
    let ids = sources
        .iter()
        .map(|source| source.memory.id.as_str())
        .collect::<Vec<_>>();
    if connection
        .summary_sources_have_conflict(&ids)
        .map_err(storage)?
    {
        return Ok(true);
    }
    for (index, left) in sources.iter().enumerate() {
        checkpoint()?;
        for right in &sources[index + 1..] {
            if texts_conflict(&left.memory.content, &right.memory.content) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn state_hash(source: &Source) -> String {
    let memory = &source.memory;
    let metadata = &source.metadata;
    let anchors = metadata
        .anchors
        .iter()
        .map(|anchor| {
            json!({
                "kind":anchor.kind, "valueHash":anchor.value_hash,
                "value":anchor.redacted_value, "confidenceBits":anchor.confidence_bits,
                "source":anchor.source, "provenance":anchor.provenance,
                "capturedSpanHash":anchor.captured_span_hash, "freshness":anchor.freshness,
                "generation":anchor.generation
            })
        })
        .collect::<Vec<_>>();
    memory_content_hash(&json!({
        "id":memory.id,"workspaceId":memory.workspace_id,"level":memory.level,"kind":memory.kind,
        "contentHash":memory_content_hash(&memory.content),"workflowId":memory.workflow_id,
        "confidence":memory.confidence,"utility":memory.utility,"importance":memory.importance,
        "provenanceUri":memory.provenance_uri,"trustClass":memory.trust_class,"trustSubclass":memory.trust_subclass,
        "provenanceChainHash":memory.provenance_chain_hash,"provenanceChainHashVersion":memory.provenance_chain_hash_version,
        "provenanceVerificationStatus":memory.provenance_verification_status,"provenanceVerifiedAt":memory.provenance_verified_at,
        "provenanceVerificationNote":memory.provenance_verification_note,
        "createdAt":memory.created_at,"updatedAt":memory.updated_at,"tombstonedAt":memory.tombstoned_at,
        "validFrom":memory.valid_from,"validTo":memory.valid_to,"tags":metadata.tags,
        "typedFields":metadata.typed_fields_json,"anchors":anchors,"superseded":metadata.superseded,
        "sealPresent":metadata.sealed,"sentinels":metadata.has_sentinels,"quarantined":metadata.quarantined
    }).to_string())
}

/// One whole, distinct answer-bearing span per source; no ellipses, byte
/// truncation, generated claims, or partial code blocks. If the bounded body
/// cannot represent every cited source, there is no proposal.
fn extract(sources: &[Source]) -> Option<(String, Vec<Value>)> {
    let mut counts = BTreeMap::<String, usize>::new();
    for source in sources {
        for term in tokenize_for_ask(&source.memory.content) {
            *counts.entry(term).or_default() += 1;
        }
    }
    let terms = counts
        .into_iter()
        .filter_map(|(term, count)| (count >= 2).then_some(term))
        .collect::<Vec<_>>();
    if terms.len() < 2 {
        return None;
    }
    let mut excerpts: Vec<String> = Vec::new();
    let mut coverage = Vec::new();
    for source in sources {
        let memory = &source.memory;
        let mut spans = segment_spans(&memory.content);
        spans.sort_by(|left, right| {
            let score = |(start, end): &(usize, usize)| {
                score_span(
                    &terms,
                    &memory.content[*start..*end],
                    memory.confidence,
                    &memory.trust_class,
                )
            };
            score(right)
                .total_cmp(&score(left))
                .then_with(|| left.0.cmp(&right.0))
        });
        let (start, end) = spans.into_iter().find(|(start, end)| {
            let text = &memory.content[*start..*end];
            let span_terms = tokenize_for_ask(text);
            !text.trim().is_empty()
                && text.len() <= MAX_SPAN_BYTES
                && terms
                    .iter()
                    .filter(|term| span_terms.binary_search(term).is_ok())
                    .count()
                    >= 2
                && !excerpts.iter().any(|existing| existing == text)
                && source.metadata.anchors.iter().all(|anchor| {
                    !anchor.redacted_value.is_empty() && text.contains(&anchor.redacted_value)
                })
        })?;
        let excerpt = &memory.content[start..end];
        if excerpts
            .iter()
            .any(|existing| texts_conflict(existing, excerpt))
        {
            return None;
        }
        excerpts.push(excerpt.to_owned());
        coverage.push(json!({
            "memoryId":memory.id,"sourceStateHash":state_hash(source),
            "byteStart":start,"byteEnd":end,"excerptHash":memory_content_hash(excerpt)
        }));
    }
    let body = excerpts.join("\n\n");
    (body.len() <= MAX_SUMMARY_BYTES).then_some((body, coverage))
}

fn build_candidate(
    workspace_id: &str,
    sources: &[Source],
) -> Result<Option<StoredCurationCandidate>, DomainError> {
    if !(MIN_MEMBERS..=MAX_MEMBERS).contains(&sources.len())
        || sources
            .iter()
            .map(|source| source.memory.content.as_str())
            .collect::<BTreeSet<_>>()
            .len()
            != sources.len()
        || sources
            .iter()
            .skip(1)
            .any(|source| !compatible(&sources[0], source))
    {
        return Ok(None);
    }
    let mut report = clusters(sources)?;
    if report.len() != 1 {
        return Ok(None);
    }
    let coherence = report.remove(0);
    if !coherence.accepted || coherence.member_count != sources.len() {
        return Ok(None);
    }
    let Some((body, coverage)) = extract(sources) else {
        return Ok(None);
    };
    let confidence = sources.iter().fold(CONFIDENCE_CAP, |cap, source| {
        cap.min(source.memory.confidence)
    });
    let bound = |start: bool| {
        sources
            .iter()
            .filter_map(|source| {
                let raw = if start {
                    source.memory.valid_from.as_ref()
                } else {
                    source.memory.valid_to.as_ref()
                }?;
                let instant = DateTime::parse_from_rfc3339(raw).ok()?;
                Some((instant, raw.clone()))
            })
            .reduce(|left, right| {
                if (start && right.0 > left.0) || (!start && right.0 < left.0) {
                    right
                } else {
                    left
                }
            })
            .map(|(_, raw)| raw)
    };
    let metadata = DerivationMetadata {
        memory_spec: DerivationMemorySpec {
            level: MemoryLevel::Semantic.as_str().to_owned(),
            kind: "summary".to_owned(),
            workflow_id: sources[0].memory.workflow_id.clone(),
            confidence: Some(confidence),
            utility: Some(0.5),
            importance: Some(0.5),
            provenance_uri: sources[0]
                .memory
                .consolidation_view()
                .scope
                .agent_provenance_uri
                .map(str::to_owned),
            trust_class: Some(TrustClass::AgentAssertion.as_str().to_owned()),
            trust_subclass: None,
            tags: sources[0].metadata.tags.clone(),
            valid_from: bound(true),
            valid_to: bound(false),
        },
        producer: DerivationProducerMetadata {
            producer: PRODUCER.to_owned(),
            producer_payload: Some(json!({
                "schema":SCHEMA,"method":"whole_span_extraction","sourceCoverage":coverage,
                "topicalCoherence":{"method":"hash_embedding_average_linkage","threshold":TOPICAL_THRESHOLD,
                    "averageInternalSimilarity":coherence.average_internal_similarity,
                    "semanticModelConfidence":false},
                "confidenceCap":CONFIDENCE_CAP
            })),
        },
    };
    let refs = sources
        .iter()
        .map(|source| {
            DerivationSourceRef::new(
                DerivationSourceKind::Memory,
                &source.memory.id,
                memory_content_hash(&source.memory.content),
            )
        })
        .collect::<Vec<_>>();
    let refs_json = canonical_derivation_source_refs_json(&refs)
        .map_err(|error| unavailable(error.to_string()))?;
    let metadata_json = canonical_derivation_metadata_json(&metadata)
        .map_err(|error| unavailable(error.to_string()))?;
    let id = deterministic_curate_id(&[SCHEMA, workspace_id, &body, &refs_json, &metadata_json]);
    Ok(Some(StoredCurationCandidate {
        id,
        workspace_id: workspace_id.to_owned(),
        candidate_type: CandidateType::CreateDerivedMemory.as_str().to_owned(),
        target_memory_id: None,
        proposed_content: Some(body),
        proposed_confidence: Some(confidence),
        proposed_trust_class: Some(TrustClass::AgentAssertion.as_str().to_owned()),
        source_type: CandidateSource::RuleEngine.as_str().to_owned(),
        source_id: Some(PRODUCER.to_owned()),
        reason: format!(
            "Extractive semantic summary of {} distinct episodic sources; whole-span source coverage, hash-based topical coherence >= {TOPICAL_THRESHOLD}, confidence capped at {CONFIDENCE_CAP}. Original sources remain intact.",
            sources.len()
        ),
        confidence,
        status: CandidateStatus::Pending.as_str().to_owned(),
        created_at: MI_DEDUP_CANDIDATE_CREATED_AT.to_owned(),
        reviewed_at: None,
        reviewed_by: None,
        applied_at: None,
        ttl_expires_at: None,
        review_state: ReviewQueueState::New.as_str().to_owned(),
        snoozed_until: None,
        merged_into_candidate_id: None,
        state_entered_at: Some(MI_DEDUP_CANDIDATE_CREATED_AT.to_owned()),
        last_action_at: Some(MI_DEDUP_CANDIDATE_CREATED_AT.to_owned()),
        ttl_policy_id: Some(
            default_curation_ttl_policy_id_for_review_state(ReviewQueueState::New.as_str())
                .to_owned(),
        ),
        derivation_source_refs_json: Some(refs_json),
        derivation_metadata_json: Some(metadata_json),
    }))
}

fn persist_candidate(
    connection: &DbConnection,
    candidate: &StoredCurationCandidate,
) -> Result<(), DomainError> {
    connection
        .insert_curation_candidate(
            &candidate.id,
            &CreateCurationCandidateInput {
                workspace_id: candidate.workspace_id.clone(),
                candidate_type: candidate.candidate_type.clone(),
                target_memory_id: None,
                proposed_content: candidate.proposed_content.clone(),
                proposed_confidence: candidate.proposed_confidence,
                proposed_trust_class: candidate.proposed_trust_class.clone(),
                source_type: candidate.source_type.clone(),
                source_id: candidate.source_id.clone(),
                reason: candidate.reason.clone(),
                confidence: candidate.confidence,
                status: Some(candidate.status.clone()),
                created_at: Some(candidate.created_at.clone()),
                ttl_expires_at: None,
                derivation_source_refs_json: candidate.derivation_source_refs_json.clone(),
                derivation_metadata_json: candidate.derivation_metadata_json.clone(),
            },
        )
        .map_err(storage)?;
    checkpoint()?;
    connection.insert_audit(&generate_audit_id(), &CreateAuditInput {
        workspace_id:Some(candidate.workspace_id.clone()),actor:Some(proposed_by_for_candidate(PRODUCER)),
        action:audit_actions::CURATION_CANDIDATE_CREATE.to_owned(),target_type:Some("curation_candidate".to_owned()),target_id:Some(candidate.id.clone()),
        details:Some(json!({"schema":SCHEMA,"producer":PRODUCER,"candidateId":candidate.id,
            "candidateType":candidate.candidate_type,"sourceRefs":candidate.derivation_source_refs_json,
            "contentHash":candidate.proposed_content.as_deref().map(memory_content_hash)
        }).to_string()),
    }).map_err(storage)?;
    Ok(())
}

/// Reconstruct the exact reviewed proposal from live source rows and sidecars.
/// The ordinary validator calls this in a read snapshot; apply calls it again
/// inside its writer transaction before creating a memory, link, audit or job.
pub(super) fn validate(
    connection: &DbConnection,
    stored: &StoredCurationCandidate,
    now: &str,
    errors: &mut Vec<CurateValidationIssue>,
) {
    if !is_summary(stored) {
        return;
    }
    let result = source_admission::with_snapshot(connection, || {
        checkpoint()?;
        let now = DateTime::parse_from_rfc3339(now)
            .map_err(|_| unavailable("Invalid summary validation clock."))?
            .with_timezone(&Utc);
        let refs =
            parse_derivation_source_refs(stored).map_err(|issue| unavailable(issue.message))?;
        if !(MIN_MEMBERS..=MAX_MEMBERS).contains(&refs.len())
            || refs
                .iter()
                .any(|source| source.kind != DerivationSourceKind::Memory)
        {
            return Ok(false);
        }
        let ids = refs
            .iter()
            .map(|source| source.id.as_str())
            .collect::<Vec<_>>();
        if ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
            return Ok(false);
        }
        let mut metadata = connection
            .memory_consolidation_metadata_in_current_snapshot(&ids)
            .map_err(storage)?;
        let mut sources = Vec::new();
        for source_ref in refs {
            checkpoint()?;
            let Some(memory) = connection.get_memory(&source_ref.id).map_err(storage)? else {
                return Ok(false);
            };
            let Some(sidecars) = metadata.remove(&memory.id) else {
                return Ok(false);
            };
            let source = Source {
                memory,
                metadata: sidecars,
            };
            if !eligible(connection, &source, &stored.workspace_id, now)? {
                return Ok(false);
            }
            sources.push(source);
        }
        sources.sort_by(|left, right| left.memory.id.cmp(&right.memory.id));
        if conflicting(connection, &sources)? {
            return Ok(false);
        }
        let Some(expected) = build_candidate(&stored.workspace_id, &sources)? else {
            return Ok(false);
        };
        checkpoint()?;
        Ok(expected.id == stored.id
            && expected.proposed_content == stored.proposed_content
            && expected.proposed_confidence == stored.proposed_confidence
            && expected.proposed_trust_class == stored.proposed_trust_class
            && expected.source_type == stored.source_type
            && expected.source_id == stored.source_id
            && expected.derivation_source_refs_json == stored.derivation_source_refs_json
            && expected.derivation_metadata_json == stored.derivation_metadata_json)
    });
    match result {
        Ok(true) => {},
        Ok(false) => errors.push(validation_issue("episodic_summary_source_changed",
            "The summary no longer matches eligible, compatible, nonconflicting source state and exact extractive coverage.",
            "Review current sources and create a fresh proposal with ee curate candidates --type summary --propose --json.")),
        Err(_) => errors.push(validation_issue("episodic_summary_source_unavailable",
            "Could not revalidate the complete episodic summary source package; no summary is authorized.",
            "Restore source access and retry validation; cancelled or unreadable source state cannot authorize apply.")),
    }
}

#[cfg(test)]
#[path = "curate_summary_tests.rs"]
mod tests;
