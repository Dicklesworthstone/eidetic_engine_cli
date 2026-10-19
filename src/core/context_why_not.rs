//! Read-only explanations observed inside the production mixed-source pack.
//!
//! The probe retains one target, never changes admission or selection, and
//! never enters a cache key, pack hash, or durable record. Source checks and
//! observations use the pack's own read snapshot.

use super::*;
use crate::pack::{
    PackEntityRef, RenderedPackProvenance, WhyNotCounterfactualHint,
    WhyNotSelectionDegradationReport, WhyNotSelectionExclusion, WhyNotSelectionExclusionKind,
    WhyNotSelectionFilterReport,
};
use serde::Serialize;

pub const WHY_NOT_SELECTED_SCHEMA_V2: &str = "ee.why_not_selected.v2";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhyNotEntity {
    pub kind: String,
    pub id: String,
}

impl From<PackEntityRef> for WhyNotEntity {
    fn from(entity: PackEntityRef) -> Self {
        Self {
            kind: entity.kind_str().to_owned(),
            id: entity.id_string(),
        }
    }
}

/// Native identity report. Scores are absent unless the real pipeline
/// constructed an admitted candidate; source bodies are never report fields.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TypedWhyNotSelectedReport {
    pub schema: &'static str,
    pub entity: WhyNotEntity,
    pub task_hash: String,
    pub selected: bool,
    pub retrieval_stage_reached: String,
    pub primary_reason: String,
    pub reason_source: String,
    pub reason_codes: Vec<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub represented_by: Option<WhyNotEntity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub representative_selected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scores: Option<WhyNotEntityScores>,
    pub token_budget_frontier: WhyNotEntityTokenFrontier,
    pub degraded: Vec<WhyNotSelectionDegradationReport>,
    pub counterfactual_hints: Vec<WhyNotCounterfactualHint>,
    pub provenance: Vec<RenderedPackProvenance>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhyNotEntityScores {
    pub target_relevance: f32,
    pub target_utility: f32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhyNotEntityTokenFrontier {
    pub max_tokens: u32,
    pub used_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_estimated_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_additional_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub could_fit_with_budget: Option<u32>,
}

/// Parse the three native pack identities without converting their UUIDs.
pub fn parse_why_not_entity(input: &str) -> Result<PackEntityRef, String> {
    if let Ok(id) = MemoryId::from_str(input) {
        return Ok(PackEntityRef::Memory(id));
    }
    if let Ok(id) = RuleId::from_str(input) {
        return Ok(PackEntityRef::Rule(id));
    }
    if let Ok(id) = EvidenceId::from_str(input) {
        return Ok(PackEntityRef::EvidenceSpan(id));
    }
    Err(format!(
        "Invalid pack entity id: {input}; expected a memory (mem_), rule (rule_), or evidence span (ev_) id."
    ))
}

/// Explain a native target using the same read-only pack path as `ee pack`.
pub fn explain_why_not_entity_default(
    options: &ContextPackOptions,
    target: PackEntityRef,
) -> Result<TypedWhyNotSelectedReport, ContextPackError> {
    explain_entity(options, target, &Deterministic::from_seed(0), None)
}

pub(super) fn explain_memory(
    options: &ContextPackOptions,
    target: MemoryId,
    determinism: &Deterministic<Seed>,
) -> Result<WhyNotSelectedReport, ContextPackError> {
    let (response, observation) =
        run_observed_pack(options, PackEntityRef::Memory(target), determinism, None)?;
    memory_report(options, response, observation)
}

fn memory_report(
    options: &ContextPackOptions,
    response: ContextResponse,
    observation: WhyNotObservation,
) -> Result<WhyNotSelectedReport, ContextPackError> {
    let target = observation.memory.ok_or_else(|| {
        ContextPackError::Pack("The target memory was not found in the requested store.".to_owned())
    })?;
    let selected = response
        .data
        .pack
        .items
        .iter()
        .any(|item| item.memory_id == target.memory_id);
    let index_degraded = retrieval_index_degraded(&response);
    let cause = observation.cause.or_else(|| {
        (observation.retrieved && !observation.reached_candidates).then_some(OmissionCause::Filter)
    });
    let exclusions = if selected {
        Vec::new()
    } else {
        cause
            .and_then(OmissionCause::exclusion)
            .into_iter()
            .collect()
    };
    let candidates = if observation.reached_candidates {
        vec![target.clone()]
    } else {
        Vec::new()
    };
    let degraded = response
        .data
        .degraded
        .iter()
        .map(|entry| {
            crate::pack::WhyNotSelectionDegradation::new(
                entry.code.clone(),
                entry.severity.as_str(),
                entry.message.clone(),
                entry.repair.clone(),
            )
        })
        .collect();
    let input = WhyNotSelectedInput::new(
        options.query.clone(),
        target,
        response.data.pack.budget,
        response.data.request.profile,
        candidates,
    )
    .with_options(crate::pack::PackAssemblyOptions {
        relevance_floor: options.relevance_floor,
        ..crate::pack::PackAssemblyOptions::default()
    })
    .with_exclusions(exclusions)
    .with_degraded(degraded);
    let mut report = crate::pack::explain_why_not_selected_from_draft(input, response.data.pack)
        .map_err(|error| ContextPackError::Pack(error.to_string()))?;
    if selected {
        report.token_budget_frontier.required_additional_tokens = 0;
        report.token_budget_frontier.could_fit_with_budget = None;
    } else if let Some(cause) = cause {
        // v1 keeps its published reason/stage enums. Native v2 can distinguish
        // pagination and result slots, while the memory contract calls both
        // candidate-filter exclusions.
        let legacy_filter = matches!(
            cause,
            OmissionCause::Pagination | OmissionCause::ResultLimit
        );
        report.primary_reason = if legacy_filter {
            "excluded_by_filter"
        } else if matches!(cause, OmissionCause::Duplicate) {
            "omitted_by_redundancy"
        } else {
            cause.reason()
        }
        .to_owned();
        report.retrieval_stage_reached = if legacy_filter {
            "candidate_filter"
        } else {
            cause.stage()
        }
        .to_owned();
        report.reason_source = "authoritative".to_owned();
        let mut hint = cause.hint();
        hint.kind.clone_from(&report.primary_reason);
        report.counterfactual_hints = vec![hint];
        if matches!(cause, OmissionCause::Duplicate) {
            report.filters_applied.push(WhyNotSelectionFilterReport {
                stage: "selection".to_owned(),
                code: "redundant_candidate".to_owned(),
                passed: false,
                detail: cause.message().to_owned(),
            });
        }
        if !matches!(cause, OmissionCause::TokenBudget) {
            report.token_budget_frontier.required_additional_tokens = 0;
            report.token_budget_frontier.could_fit_with_budget = None;
        }
    } else if !observation.reached_candidates && !index_degraded {
        // Optional graph/configuration diagnostics are not evidence that the
        // retrieval index caused this miss. Keep the full diagnostics, but
        // preserve the narrower reconstructed retrieval reason.
        report.primary_reason = "not_retrieved".to_owned();
        report.retrieval_stage_reached = "retrieval".to_owned();
        report.reason_source = "reconstructed".to_owned();
        report.counterfactual_hints = vec![WhyNotCounterfactualHint {
            kind: "retrieval".to_owned(),
            action: "Use a more specific task or increase --candidate-pool.".to_owned(),
            rationale: "The target did not enter this request's candidate pool.".to_owned(),
        }];
    }
    Ok(report)
}

#[cfg(test)]
#[path = "context_why_not_tests.rs"]
mod tests;

fn explain_entity(
    options: &ContextPackOptions,
    target: PackEntityRef,
    determinism: &Deterministic<Seed>,
    embedder: Option<Arc<dyn crate::search::Embedder>>,
) -> Result<TypedWhyNotSelectedReport, ContextPackError> {
    let (response, observation) = run_observed_pack(options, target, determinism, embedder)?;
    Ok(entity_report(target, &response, observation))
}

fn run_observed_pack(
    options: &ContextPackOptions,
    target: PackEntityRef,
    determinism: &Deterministic<Seed>,
    embedder: Option<Arc<dyn crate::search::Embedder>>,
) -> Result<(ContextResponse, WhyNotObservation), ContextPackError> {
    let mut options = options.clone();
    options.persist_pack = false;
    options.baseline_write = None;
    // A cached response has no per-target selection observations. Explanation
    // neither reads nor populates the persistent assembly cache.
    options.output_options.cache_json_response = false;
    let probe = WhyNotSelectionProbe::new(target);
    let options_ref = &options;
    let probe_ref = &probe;
    let response = crate::core::run_cli_with_cx(PACK_COMMAND_TIMEOUT, |cx| async move {
        let mut control = ContextPackControl::new(&cx, None, None);
        control.why_not = Some(probe_ref);
        run_context_pack_with_performance_inner(
            options_ref,
            "why-not",
            determinism,
            PackRecordPersistence::Seeded,
            control,
            embedder,
            None,
        )
        .await
        .map(|run| run.response)
    })
    .map_err(|error| {
        ContextPackError::Pack(format!("Failed to start why-not runtime: {error}"))
    })??;
    let observation = probe.into_observation();
    if !observation.exists && !observation.reached_candidates {
        return Err(ContextPackError::Pack(format!(
            "{} {} not found; cannot explain why it was not selected.",
            target.kind_str(),
            target.id_string()
        )));
    }
    Ok((response, observation))
}

#[derive(Clone, Copy, Debug)]
pub(super) enum OmissionCause {
    Filter,
    Scope,
    Redaction,
    Validity,
    Policy,
    RuleAdmission,
    EvidenceAdmission,
    ScoreFloor,
    Pagination,
    TokenBudget,
    ResultLimit,
    Duplicate,
    IncidentCard,
}

impl OmissionCause {
    fn reason(self) -> &'static str {
        match self {
            Self::Filter => "excluded_by_filter",
            Self::Scope => "excluded_by_scope",
            Self::Redaction => "excluded_by_redaction",
            Self::Validity => "excluded_by_validity_window",
            Self::Policy | Self::RuleAdmission | Self::EvidenceAdmission => "excluded_by_policy",
            Self::ScoreFloor => "omitted_by_score_floor",
            Self::Pagination => "omitted_by_pagination",
            Self::TokenBudget => "omitted_by_token_budget",
            Self::ResultLimit => "omitted_by_result_limit",
            Self::Duplicate => "represented_by_other_item",
            Self::IncidentCard => "represented_by_incident_card",
        }
    }

    fn code(self) -> &'static str {
        match self {
            Self::Scope => "scope_excluded_evidence",
            Self::Redaction => "context_redaction_filtered_results",
            Self::Validity => "context_temporal_filtered_results",
            Self::RuleAdmission => "context_rule_hit_unhydrated",
            Self::EvidenceAdmission => "context_evidence_hit_unhydrated",
            Self::Pagination => "context_pagination_applied",
            Self::ResultLimit => "context_query_max_results_applied",
            _ => "context_filtered_results",
        }
    }

    fn stage(self) -> &'static str {
        match self {
            Self::TokenBudget | Self::ResultLimit | Self::Duplicate => "selection",
            Self::Pagination => "pagination",
            _ => "candidate_filter",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::Filter => "The target did not survive the pack's candidate filters.",
            Self::Scope => "The target is outside the requested workspace or memory scope.",
            Self::Redaction => "The target did not satisfy the redaction allow-category filter.",
            Self::Validity => "The target is outside the requested temporal validity window.",
            Self::Policy => {
                "The target did not satisfy the pack's source or instruction-authority policy."
            }
            Self::RuleAdmission => {
                "The live rule or its required source could not be admitted to this pack."
            }
            Self::EvidenceAdmission => "The live evidence span could not be admitted to this pack.",
            Self::ScoreFloor => "The target fell below the pack's relevance floor.",
            Self::Pagination => "The target falls outside the requested page or result limit.",
            Self::TokenBudget => {
                "The target could not fit the shared budget while retaining higher-priority selected items."
            }
            Self::ResultLimit => {
                "The target could not replace a selected item within the requested result limit."
            }
            Self::Duplicate => {
                "Another candidate represents the same evidence, so the pack omits the duplicate."
            }
            Self::IncidentCard => {
                "An admitted incident card represents this source in the candidate pool."
            }
        }
    }

    fn exclusion(self) -> Option<WhyNotSelectionExclusion> {
        let kind = match self {
            Self::Scope => WhyNotSelectionExclusionKind::Scope,
            Self::Redaction => WhyNotSelectionExclusionKind::Redaction,
            Self::Validity => WhyNotSelectionExclusionKind::ValidityWindow,
            Self::Policy | Self::RuleAdmission | Self::EvidenceAdmission => {
                WhyNotSelectionExclusionKind::Policy
            }
            Self::Filter | Self::Pagination | Self::ResultLimit => {
                WhyNotSelectionExclusionKind::Filter
            }
            _ => return None,
        };
        Some(WhyNotSelectionExclusion::new(
            kind,
            self.code(),
            self.message(),
            Some(self.hint().action),
        ))
    }

    fn hint(self) -> WhyNotCounterfactualHint {
        let action = match self {
            Self::TokenBudget => "Increase --max-tokens or narrow the task.",
            Self::Pagination | Self::ResultLimit => {
                "Increase the result limit or request the page containing the target."
            }
            Self::ScoreFloor => "Use a more specific task or inspect the caller's relevance floor.",
            Self::Scope => "Select the owning workspace and an appropriate --memory-scope.",
            Self::Duplicate | Self::IncidentCard => {
                "Inspect the representative with ee why-not and follow its provenance."
            }
            Self::RuleAdmission | Self::EvidenceAdmission | Self::Policy => {
                "Inspect the target with ee why and repair its source admission before repacking."
            }
            _ => "Inspect the task's filters and the target's current metadata.",
        };
        WhyNotCounterfactualHint {
            kind: self.reason().to_owned(),
            action: action.to_owned(),
            rationale: self.message().to_owned(),
        }
    }
}

#[derive(Default)]
struct WhyNotObservation {
    exists: bool,
    retrieved: bool,
    reached_candidates: bool,
    present: bool,
    memory: Option<PackCandidate>,
    native: Option<NativeDetails>,
    cause: Option<OmissionCause>,
    represented_by: Option<WhyNotEntity>,
}

struct NativeDetails {
    estimated_tokens: u32,
    scores: Option<WhyNotEntityScores>,
    provenance: Vec<RenderedPackProvenance>,
}

pub(super) struct WhyNotSelectionProbe {
    target: PackEntityRef,
    state: Mutex<WhyNotObservation>,
}

impl WhyNotSelectionProbe {
    fn new(target: PackEntityRef) -> Self {
        Self {
            target,
            state: Mutex::new(WhyNotObservation::default()),
        }
    }

    fn with_state<T>(&self, operation: impl FnOnce(&mut WhyNotObservation) -> T) -> T {
        operation(
            &mut self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    fn into_observation(self) -> WhyNotObservation {
        self.state
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn initialize(
        &self,
        connection: &DbConnection,
        options: &ContextPackOptions,
        request: &ContextRequest,
        filters: &crate::models::QueryFilters,
        search: &SearchReport,
        preloaded: &BTreeMap<String, StoredMemory>,
    ) -> Result<(), ContextPackError> {
        let id = self.target.id_string();
        self.with_state(|state| {
            state.retrieved = search.results.iter().any(|hit| hit.doc_id == id);
        });
        let mut local_degraded = Vec::new();
        let workspace_ids =
            context_workspace_ids(connection, &options.workspace_path, &mut local_degraded);
        match self.target {
            PackEntityRef::Memory(memory_id) => {
                let memory = connection.get_memory(&id).map_err(storage_error)?;
                let Some(memory) = memory else {
                    return Ok(());
                };
                let tags = connection.get_memory_tags(&id).map_err(storage_error)?;
                let scope = MemoryScopeContext::for_workspace_with_connection(
                    &options.workspace_path,
                    options.memory_scope,
                    options.strict_scope,
                    Some(connection),
                );
                let workspace_candidate = workspace_ids.contains(&memory.workspace_id)
                    || crate::models::memory_tags_include_global_scope(&tags);
                let cause =
                    if !workspace_candidate || !scope.memory_in_scope_with_tags(&memory, &tags) {
                        Some(OmissionCause::Scope)
                    } else if context_memory_seal_admission(
                        connection,
                        &memory,
                        &mut local_degraded,
                        "context_candidate_memory_batch_unavailable",
                        ContextResponseSeverity::Medium,
                        "Why-not target admission",
                    ) != ContextMemorySealAdmission::Admit
                    {
                        Some(OmissionCause::Policy)
                    } else if (memory.tombstoned_at.is_some() && !options.include_tombstoned)
                        || fallback_memory_validity_visibility(
                            &memory,
                            request.reference_time.unwrap_or_else(Utc::now),
                            context_include_expired(options, filters),
                            context_include_future(options, filters),
                            context_include_stale(options, filters),
                        ) != FallbackMemoryVisibility::Visible
                    {
                        Some(OmissionCause::Validity)
                    } else if !filters.matches_tags(&tags)
                        || !filters.trust.matches(
                            &memory.trust_class,
                            posture_for_trust_class(&memory.trust_class),
                        )
                    {
                        Some(OmissionCause::Filter)
                    } else if !filters.temporal.is_empty()
                        && matches!(
                            temporal_memory_outcome(&memory, &filters.temporal),
                            TemporalCandidateOutcome::Exclude
                        )
                    {
                        Some(OmissionCause::Validity)
                    } else if !redaction_allow_categories(&memory.content, &filters.redaction) {
                        Some(OmissionCause::Redaction)
                    } else if !crate::policy::detect_instruction_like_content(&memory.content)
                        .authority_signal_codes()
                        .is_empty()
                    {
                        Some(OmissionCause::Policy)
                    } else {
                        None
                    };
                let mut candidate = reconstruct_not_retrieved_candidate(
                    connection,
                    &options.workspace_path,
                    memory_id,
                    request.reference_time,
                    &mut local_degraded,
                )?;
                // A rejected target is an identity, not permission to disclose
                // another workspace's provenance or withheld source metadata.
                if matches!(cause, Some(OmissionCause::Scope | OmissionCause::Policy)) {
                    candidate.provenance.clear();
                }
                self.with_state(|state| {
                    state.exists = true;
                    state.memory = Some(candidate);
                    state.cause = cause;
                });
            }
            PackEntityRef::Rule(_) => {
                let rule = connection.get_procedural_rule(&id).map_err(storage_error)?;
                let Some(rule) = rule else {
                    return Ok(());
                };
                self.with_state(|state| state.exists = true);
                if !workspace_ids.contains(&rule.workspace_id) {
                    self.reject(OmissionCause::Scope);
                    return Ok(());
                }
                let mut target_search = search.clone();
                target_search.results = vec![admission_probe_hit(id)];
                let mut rules = Vec::new();
                let _ = candidates_from_search_for_task_paths(
                    connection,
                    &options.workspace_path,
                    &target_search,
                    filters,
                    options.include_tombstoned,
                    &mut local_degraded,
                    Some(preloaded),
                    &request.task_paths,
                    request.reference_time,
                    &mut rules,
                );
                if rules.is_empty() {
                    self.reject(OmissionCause::RuleAdmission);
                    return Ok(());
                }
                let scope = MemoryScopeContext::for_workspace_with_connection(
                    &options.workspace_path,
                    options.memory_scope,
                    options.strict_scope,
                    Some(connection),
                );
                filter_native_rule_sources_by_scope(
                    connection,
                    &mut rules,
                    &scope,
                    &mut local_degraded,
                );
                if rules.is_empty() {
                    self.reject(OmissionCause::Scope);
                    return Ok(());
                }
                if options.require_fresh_sentinels {
                    filter_native_rule_sources_by_fresh_sentinels(
                        connection,
                        &mut rules,
                        request.reference_time.unwrap_or_else(Utc::now),
                        &mut local_degraded,
                    )?;
                }
                let items =
                    direct_rule_pack_candidates(rules, request, filters, true, &mut local_degraded);
                if let Some(item) = items.first() {
                    self.with_state(|state| state.native = Some(rule_details(item, false)));
                } else {
                    let authority = crate::policy::detect_instruction_like_content(&rule.content);
                    self.reject(if authority.authority_signal_codes().is_empty() {
                        OmissionCause::Filter
                    } else {
                        OmissionCause::Policy
                    });
                }
            }
            PackEntityRef::EvidenceSpan(_) => {
                let rows = connection
                    .get_evidence_spans_with_sessions(&[id.as_str()])
                    .map_err(storage_error)?;
                let Some(row) = rows.into_iter().next() else {
                    return Ok(());
                };
                self.with_state(|state| state.exists = true);
                if !workspace_ids.contains(&row.span.workspace_id)
                    || !matches!(
                        options.memory_scope,
                        MemoryScope::Workspace | MemoryScope::Swarm
                    )
                {
                    self.reject(OmissionCause::Scope);
                } else if !row.is_direct_pack_admitted(&row.span.workspace_id) {
                    self.reject(OmissionCause::EvidenceAdmission);
                } else if !direct_evidence_matches_filters(&row.span, filters) {
                    self.reject(OmissionCause::Filter);
                } else if let Some(candidate) = direct_evidence_pack_candidate(
                    row.span,
                    0.0,
                    "admission_probe",
                    &request.query,
                    None,
                ) {
                    self.with_state(|state| {
                        state.native = Some(evidence_details(&candidate.item, false))
                    });
                } else {
                    self.reject(OmissionCause::EvidenceAdmission);
                }
            }
        }
        Ok(())
    }

    pub(super) fn observe_search(&self, search: &SearchReport) {
        self.with_state(|state| {
            if state.retrieved
                && !search
                    .results
                    .iter()
                    .any(|hit| hit.doc_id == self.target.id_string())
            {
                state.cause = Some(OmissionCause::Filter);
            }
        });
    }

    pub(super) fn observe_memories(&self, candidates: &[PackCandidate], absent: OmissionCause) {
        let PackEntityRef::Memory(id) = self.target else {
            return;
        };
        let target = candidates
            .iter()
            .find(|candidate| candidate.memory_id == id);
        self.with_state(|state| {
            update_presence(state, target.is_some(), absent);
            if let Some(target) = target {
                state.memory = Some(target.clone());
            }
        });
    }

    pub(super) fn observe_rule_sources(
        &self,
        rules: &[(SearchHit, RuleIndexProjection)],
        absent: OmissionCause,
    ) {
        let PackEntityRef::Rule(id) = self.target else {
            return;
        };
        self.with_state(|state| {
            update_presence(
                state,
                rules
                    .iter()
                    .any(|(_, projection)| projection.rule().id == id.to_string()),
                absent,
            )
        });
    }

    pub(super) fn observe_native(
        &self,
        rules: &[crate::pack::PackRuleItem],
        evidence: &[DirectEvidencePackCandidate],
        absent: OmissionCause,
    ) {
        match self.target {
            PackEntityRef::Rule(id) => {
                let target = rules.iter().find(|item| item.rule_id == id.to_string());
                self.with_state(|state| {
                    update_presence(state, target.is_some(), absent);
                    if let Some(item) = target {
                        state.native = Some(rule_details(item, true));
                    }
                });
            }
            PackEntityRef::EvidenceSpan(_) => self.observe_evidence(evidence, absent),
            PackEntityRef::Memory(_) => {}
        }
    }

    pub(super) fn observe_evidence(
        &self,
        evidence: &[DirectEvidencePackCandidate],
        absent: OmissionCause,
    ) {
        let PackEntityRef::EvidenceSpan(id) = self.target else {
            return;
        };
        let target = evidence
            .iter()
            .find(|candidate| candidate.item.evidence_id == id.to_string());
        self.with_state(|state| {
            update_presence(state, target.is_some(), absent);
            if let Some(candidate) = target {
                state.native = Some(evidence_details(&candidate.item, true));
            }
        });
    }

    pub(super) fn observe_incident_preference(
        &self,
        connection: &DbConnection,
        evidence: &[DirectEvidencePackCandidate],
    ) {
        let PackEntityRef::EvidenceSpan(id) = self.target else {
            return;
        };
        let id = id.to_string();
        if evidence
            .iter()
            .any(|candidate| candidate.item.evidence_id == id)
        {
            self.observe_evidence(evidence, OmissionCause::EvidenceAdmission);
            return;
        }
        if !self.with_state(|state| state.present) {
            return;
        }
        // Only inspect cards returned by the real same-snapshot proof gate.
        // A line-range overlap does not establish source membership.
        for candidate in evidence.iter().filter(|candidate| candidate.incident_card) {
            if let Ok(Some(card)) = connection.get_evidence_span(&candidate.item.evidence_id)
                && incident_card_source_ids(&card).contains(&id)
                && let Ok(representative) = EvidenceId::from_str(&card.id)
            {
                self.represented_by(
                    PackEntityRef::EvidenceSpan(representative),
                    OmissionCause::IncidentCard,
                );
                return;
            }
        }
        self.reject(OmissionCause::EvidenceAdmission);
    }

    pub(super) fn matches(&self, entity: &ContextPackEntityKey) -> bool {
        match (self.target, entity) {
            (PackEntityRef::Memory(left), ContextPackEntityKey::Memory(right)) => left == *right,
            (PackEntityRef::Rule(left), ContextPackEntityKey::Rule(right)) => {
                left.to_string() == *right
            }
            (PackEntityRef::EvidenceSpan(left), ContextPackEntityKey::Evidence(right)) => {
                left.to_string() == *right
            }
            _ => false,
        }
    }

    pub(super) fn reject(&self, cause: OmissionCause) {
        self.with_state(|state| {
            state.cause = Some(cause);
            state.present = false;
        });
    }

    pub(super) fn represented_by(&self, entity: PackEntityRef, cause: OmissionCause) {
        self.reject(cause);
        self.with_state(|state| state.represented_by = Some(entity.into()));
    }

    pub(super) fn represented_by_key(&self, entity: &ContextPackEntityKey, cause: OmissionCause) {
        let entity = match entity {
            ContextPackEntityKey::Memory(id) => Some(PackEntityRef::Memory(*id)),
            ContextPackEntityKey::Rule(id) => RuleId::from_str(id).ok().map(PackEntityRef::Rule),
            ContextPackEntityKey::Evidence(id) => EvidenceId::from_str(id)
                .ok()
                .map(PackEntityRef::EvidenceSpan),
        };
        if let Some(entity) = entity {
            self.represented_by(entity, cause);
        }
    }
}

fn update_presence(state: &mut WhyNotObservation, present: bool, absent: OmissionCause) {
    if present {
        state.exists = true;
        state.reached_candidates = true;
        state.cause = None;
        state.represented_by = None;
    } else if state.present {
        state.cause = Some(absent);
    }
    state.present = present;
}

fn admission_probe_hit(doc_id: String) -> SearchHit {
    SearchHit {
        doc_id,
        score: 0.5,
        source: ScoreSource::Lexical,
        fast_score: None,
        quality_score: None,
        lexical_score: None,
        rerank_score: None,
        metadata: None,
        explanation: None,
    }
}

fn storage_error(error: crate::db::DbError) -> ContextPackError {
    ContextPackError::Storage(error.to_string())
}

fn retrieval_index_degraded(response: &ContextResponse) -> bool {
    response
        .data
        .degraded
        .iter()
        .any(|entry| entry.code.contains("index") || entry.code == "context_lexical_fallback")
}

fn rule_details(item: &crate::pack::PackRuleItem, scored: bool) -> NativeDetails {
    NativeDetails {
        estimated_tokens: item.estimated_tokens,
        scores: scored.then_some(WhyNotEntityScores {
            target_relevance: item.relevance.into_inner(),
            target_utility: item.utility.into_inner(),
        }),
        provenance: item.rendered_provenance(),
    }
}

fn evidence_details(item: &PackEvidenceItem, scored: bool) -> NativeDetails {
    NativeDetails {
        estimated_tokens: item.estimated_tokens,
        scores: scored.then_some(WhyNotEntityScores {
            target_relevance: item.relevance.into_inner(),
            target_utility: item.utility.into_inner(),
        }),
        provenance: item.rendered_provenance(),
    }
}

fn entity_report(
    target: PackEntityRef,
    response: &ContextResponse,
    observation: WhyNotObservation,
) -> TypedWhyNotSelectedReport {
    let pack = &response.data.pack;
    let selected = match target {
        PackEntityRef::Memory(id) => pack.items.iter().any(|item| item.memory_id == id),
        PackEntityRef::Rule(id) => pack
            .rule_items
            .iter()
            .any(|item| item.rule_id == id.to_string()),
        PackEntityRef::EvidenceSpan(id) => pack
            .evidence_items
            .iter()
            .any(|item| item.evidence_id == id.to_string()),
    };
    let index_degraded = retrieval_index_degraded(response);
    let (reason, stage, source, message) = if selected {
        (
            "selected",
            "selected",
            "authoritative",
            "The target is selected in the actual mixed-source pack.",
        )
    } else if let Some(cause) = observation.cause {
        (
            cause.reason(),
            cause.stage(),
            "authoritative",
            cause.message(),
        )
    } else if observation.retrieved || observation.reached_candidates {
        (
            "excluded_by_filter",
            "candidate_filter",
            "authoritative",
            "The retrieved target did not survive the pack's live admission and candidate filters.",
        )
    } else if index_degraded {
        (
            "not_retrieved_due_to_degraded_index",
            "degraded_index",
            "reconstructed",
            "The target did not enter this request's candidate pool; retrieval reported a degraded index.",
        )
    } else {
        (
            "not_retrieved",
            "retrieval",
            "reconstructed",
            "The target did not enter this request's candidate pool.",
        )
    };
    let target_tokens = observation
        .native
        .as_ref()
        .map(|details| details.estimated_tokens)
        .or_else(|| {
            observation
                .memory
                .as_ref()
                .map(|candidate| candidate.estimated_tokens)
        });
    let could_fit = if !selected && matches!(observation.cause, Some(OmissionCause::TokenBudget)) {
        target_tokens.and_then(|tokens| pack.used_tokens.checked_add(tokens))
    } else {
        None
    };
    let representative_selected =
        observation
            .represented_by
            .as_ref()
            .map(|entity| match entity.kind.as_str() {
                "memory" => pack
                    .items
                    .iter()
                    .any(|item| item.memory_id.to_string() == entity.id),
                "rule" => pack.rule_items.iter().any(|item| item.rule_id == entity.id),
                "evidence_span" => pack
                    .evidence_items
                    .iter()
                    .any(|item| item.evidence_id == entity.id),
                _ => false,
            });
    TypedWhyNotSelectedReport {
        schema: WHY_NOT_SELECTED_SCHEMA_V2,
        entity: target.into(),
        task_hash: blake3::hash(response.data.request.query.as_bytes())
            .to_hex()
            .to_string(),
        selected,
        retrieval_stage_reached: stage.to_owned(),
        primary_reason: reason.to_owned(),
        reason_source: source.to_owned(),
        reason_codes: if selected {
            Vec::new()
        } else {
            observation
                .cause
                .map(|cause| vec![cause.code().to_owned()])
                .unwrap_or_default()
        },
        message: message.to_owned(),
        represented_by: if selected {
            None
        } else {
            observation.represented_by
        },
        representative_selected: if selected {
            None
        } else {
            representative_selected
        },
        token_budget_frontier: WhyNotEntityTokenFrontier {
            max_tokens: pack.budget.max_tokens(),
            used_tokens: pack.used_tokens,
            target_estimated_tokens: target_tokens,
            required_additional_tokens: if selected {
                Some(0)
            } else {
                could_fit.map(|tokens| tokens.saturating_sub(pack.budget.max_tokens()))
            },
            could_fit_with_budget: could_fit,
        },
        degraded: response
            .data
            .degraded
            .iter()
            .map(|entry| WhyNotSelectionDegradationReport {
                code: entry.code.clone(),
                severity: entry.severity.as_str().to_owned(),
                stage: "retrieval".to_owned(),
                message: entry.message.clone(),
                repair_action: entry.repair.clone(),
            })
            .collect(),
        counterfactual_hints: if selected {
            Vec::new()
        } else {
            observation
                .cause
                .map(OmissionCause::hint)
                .into_iter()
                .collect()
        },
        scores: observation
            .native
            .as_ref()
            .and_then(|details| details.scores.clone()),
        provenance: observation
            .native
            .map(|details| details.provenance)
            .unwrap_or_default(),
    }
}
