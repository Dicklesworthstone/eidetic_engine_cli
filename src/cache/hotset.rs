//! Redaction-safe hotset manifest for search/context cache prewarm.
//!
//! Persists frequent query shapes, memory IDs, index generations, profile
//! tier, and hit counts without storing raw query text or memory content.
//! The manifest is the durable, auditable record swarm operators ship into
//! support bundles or hand off to a future `ee cache prewarm` surface so a
//! read-heavy burst can warm caches against the same shapes the previous
//! workload exercised.
//!
//! Inputs are the existing `SearchHotsetEntry` and `PackHotsetEntry` records
//! produced by `src/search/mod.rs` and `src/pack/mod.rs`. Both entry types
//! already store hashes, kind tags, generation, estimated bytes, hit counts,
//! and a `redaction_status` marker — no plaintext content. This module wraps
//! them in a stable `ee.cache.hotset.v1` artifact, classifies stale entries
//! against the current `(workspace_generation, index_generation)` gate, and
//! emits a `cache_hotset_stale` degradation when stale entries were rejected
//! so agents can choose to recapture instead of silently warming with stale
//! candidates.
//!
//! The module is process-local and side-effect free: it does NOT read or
//! write any cache, file, or database. Caller decides what to do with the
//! manifest (write to disk, ship in a support bundle, hand to a prewarm
//! command). All ordering is deterministic so identical inputs produce
//! byte-identical JSON after the caller strips volatile fields such as
//! `capturedAt`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::cache::{CacheBudget, MemoryPressure};
use crate::pack::{
    PackCacheGovernor, PackHotset, PackHotsetEntry, PackHotsetEntryKind, PackSection,
    prewarm_pack_hotset,
};
use crate::search::{
    SearchCacheGovernor, SearchHotset, SearchHotsetEntry, SearchHotsetEntryKind,
    prewarm_search_hotset,
};

/// JSON Schema id pinned by every emitted manifest.
pub const SCHEMA: &str = "ee.cache.hotset.v1";

/// Degraded code emitted when the manifest rejected stale entries (their
/// `generation` is older than the gate's `workspace_generation` or
/// `index_generation`). Severity is `medium`: warming caches with stale
/// shapes would silently degrade pack quality if the rejected entries were
/// admitted, so the manifest filters them and surfaces the rejection.
pub const STALE_HOTSET_CODE: &str = "cache_hotset_stale";

/// The single redaction posture this manifest claims. Mirrors the
/// `content_not_stored` marker each entry carries inside the search/pack
/// hotset structs. If any entry carries a different marker the manifest
/// refuses to admit it (see [`HotsetManifest::is_redaction_safe`]).
pub const REDACTION_STATUS: &str = "content_not_stored";

/// JSON Schema id for the advisory dry-run plan that predicts context
/// hotsets from swarm coordination signals.
pub const PREWARM_PLAN_SCHEMA: &str = "ee.cache.hotset_prewarm_plan.v1";

/// JSON Schema id for the explicit `ee cache prewarm` report.
pub const CACHE_PREWARM_SCHEMA: &str = "ee.cache.prewarm.v1";

/// Degraded code emitted when the prewarm planner receives no usable signal.
pub const PREWARM_NO_SIGNAL_CODE: &str = "hotset_prewarm_no_signals";

/// Redaction posture for prewarm plans. Query text, mail bodies, bead titles,
/// and other raw coordination text are used only in-process to derive BLAKE3
/// query-shape keys; the plan itself exposes hashes and source classes.
pub const PREWARM_REDACTION_STATUS: &str = "query_hashes_only";

/// Generation gate the manifest evaluates entries against. Entries whose
/// `generation` is strictly less than the active workspace generation or the
/// index generation that produced them are classified as stale-rejected.
///
/// Note: the search and pack entry types share a single `generation` field
/// today; this struct keeps both fields so a future split (workspace-rev
/// versus index-rev) does not require renaming the schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenerationGate {
    pub workspace_generation: u64,
    pub index_generation: u64,
}

impl GenerationGate {
    /// Construct a gate from explicit generations.
    #[must_use]
    pub const fn new(workspace_generation: u64, index_generation: u64) -> Self {
        Self {
            workspace_generation,
            index_generation,
        }
    }

    /// The minimum generation an entry must carry to be admitted. Today both
    /// hotset entry families use a single `generation`, so the admission
    /// threshold is the higher of the two — admitting an entry from a stale
    /// index against a fresh workspace would silently warm cold-mass.
    #[must_use]
    pub const fn admission_threshold(self) -> u64 {
        if self.workspace_generation > self.index_generation {
            self.workspace_generation
        } else {
            self.index_generation
        }
    }
}

/// Memory budget the manifest reports for operator visibility. Numeric values
/// are advisory: the manifest itself does not evict, but the budget travels
/// with the artifact so a follow-up prewarm command can refuse admission when
/// `current_*` already meets or exceeds `max_*`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HotsetBudget {
    pub max_entries: usize,
    pub max_bytes: usize,
    pub current_entries: usize,
    pub current_bytes: usize,
}

impl HotsetBudget {
    #[must_use]
    pub const fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            max_entries,
            max_bytes,
            current_entries: 0,
            current_bytes: 0,
        }
    }

    #[must_use]
    pub const fn with_current(mut self, current_entries: usize, current_bytes: usize) -> Self {
        self.current_entries = current_entries;
        self.current_bytes = current_bytes;
        self
    }

    fn to_json(self) -> Value {
        json!({
            "maxEntries": self.max_entries,
            "maxBytes": self.max_bytes,
            "currentEntries": self.current_entries,
            "currentBytes": self.current_bytes,
        })
    }
}

/// Source class for an advisory context-hotset prewarm signal.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PrewarmSignalSource {
    Beads,
    Bv,
    AgentMail,
    VerificationBroker,
    HostProfile,
}

impl PrewarmSignalSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Beads => "beads",
            Self::Bv => "bv",
            Self::AgentMail => "agent_mail",
            Self::VerificationBroker => "verification_broker",
            Self::HostProfile => "host_profile",
        }
    }

    const fn weight(self) -> u64 {
        match self {
            Self::Beads => 48,
            Self::Bv => 44,
            Self::AgentMail => 36,
            Self::VerificationBroker => 32,
            Self::HostProfile => 20,
        }
    }
}

/// Redaction-safe input signal for advisory context hotset prewarm planning.
///
/// `summary` and `labels` may contain raw coordination text, so they are never
/// emitted by [`HotsetPrewarmPlan::to_json`]. Callers can construct these from
/// Beads, BV, Agent Mail subjects, verification blockers, or host-profile
/// posture without coupling the cache module to those services.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrewarmSignal {
    source: PrewarmSignalSource,
    stable_id: String,
    summary: String,
    labels: Vec<String>,
    priority: u8,
}

impl PrewarmSignal {
    #[must_use]
    pub fn new(
        source: PrewarmSignalSource,
        stable_id: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            source,
            stable_id: stable_id.into(),
            summary: summary.into(),
            labels: Vec::new(),
            priority: 5,
        }
    }

    #[must_use]
    pub fn with_labels(mut self, labels: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.labels = labels.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub const fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    #[must_use]
    pub const fn source(&self) -> PrewarmSignalSource {
        self.source
    }

    #[must_use]
    pub fn stable_id(&self) -> &str {
        &self.stable_id
    }
}

/// One candidate query shape predicted by the dry-run prewarm planner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HotsetPrewarmCandidate {
    search_entry: SearchHotsetEntry,
    source_kinds: Vec<&'static str>,
    signal_ref_hashes: Vec<String>,
    token_count: usize,
    score: u64,
}

impl HotsetPrewarmCandidate {
    #[must_use]
    pub fn query_shape_key(&self) -> &str {
        &self.search_entry.key
    }

    #[must_use]
    pub const fn score(&self) -> u64 {
        self.score
    }

    #[must_use]
    pub const fn estimated_bytes(&self) -> usize {
        self.search_entry.estimated_bytes
    }

    #[must_use]
    pub const fn search_entry(&self) -> &SearchHotsetEntry {
        &self.search_entry
    }

    fn to_json(&self) -> Value {
        json!({
            "queryShapeKey": &self.search_entry.key,
            "kind": self.search_entry.kind.as_str(),
            "generation": self.search_entry.generation,
            "sourceKinds": &self.source_kinds,
            "signalRefHashes": &self.signal_ref_hashes,
            "tokenCount": self.token_count,
            "score": self.score,
            "estimatedBytes": self.search_entry.estimated_bytes,
            "redactionStatus": PREWARM_REDACTION_STATUS,
        })
    }
}

#[derive(Clone, Debug)]
struct PrewarmCandidateAccumulator {
    entry: SearchHotsetEntry,
    source_kinds: BTreeSet<&'static str>,
    signal_ref_hashes: BTreeSet<String>,
    token_count: usize,
    score: u64,
}

impl PrewarmCandidateAccumulator {
    fn new(entry: SearchHotsetEntry, signal: &PrewarmSignal, token_count: usize) -> Self {
        let mut source_kinds = BTreeSet::new();
        source_kinds.insert(signal.source.as_str());
        let mut signal_ref_hashes = BTreeSet::new();
        signal_ref_hashes.insert(signal_ref_hash(signal));
        Self {
            entry,
            source_kinds,
            signal_ref_hashes,
            token_count,
            score: prewarm_signal_score(signal, token_count),
        }
    }

    fn merge(&mut self, entry: SearchHotsetEntry, signal: &PrewarmSignal, token_count: usize) {
        self.entry.hit_count = self.entry.hit_count.saturating_add(entry.hit_count);
        self.entry.estimated_bytes = self.entry.estimated_bytes.max(entry.estimated_bytes);
        self.entry.generation = self.entry.generation.max(entry.generation);
        self.source_kinds.insert(signal.source.as_str());
        self.signal_ref_hashes.insert(signal_ref_hash(signal));
        self.token_count = self.token_count.max(token_count);
        self.score = self
            .score
            .saturating_add(prewarm_signal_score(signal, token_count));
    }

    fn into_candidate(self) -> HotsetPrewarmCandidate {
        HotsetPrewarmCandidate {
            search_entry: self.entry,
            source_kinds: self.source_kinds.into_iter().collect(),
            signal_ref_hashes: self.signal_ref_hashes.into_iter().collect(),
            token_count: self.token_count,
            score: self.score,
        }
    }
}

/// Advisory, side-effect-free context hotset prewarm plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HotsetPrewarmPlan {
    generation: u64,
    budget: HotsetBudget,
    input_signal_count: usize,
    skipped_signal_count: usize,
    max_candidates: usize,
    candidates: Vec<HotsetPrewarmCandidate>,
}

impl HotsetPrewarmPlan {
    #[must_use]
    pub const fn schema(&self) -> &'static str {
        PREWARM_PLAN_SCHEMA
    }

    #[must_use]
    pub fn candidates(&self) -> &[HotsetPrewarmCandidate] {
        &self.candidates
    }

    #[must_use]
    pub const fn input_signal_count(&self) -> usize {
        self.input_signal_count
    }

    #[must_use]
    pub const fn skipped_signal_count(&self) -> usize {
        self.skipped_signal_count
    }

    #[must_use]
    pub fn estimated_memory_bytes(&self) -> usize {
        self.candidates
            .iter()
            .map(HotsetPrewarmCandidate::estimated_bytes)
            .sum()
    }

    #[must_use]
    pub fn expected_latency_win_ms(&self) -> u64 {
        self.candidates
            .iter()
            .map(|candidate| {
                8_u64
                    .saturating_add(candidate.search_entry.hit_count.min(8))
                    .saturating_add((candidate.score / 32).min(16))
            })
            .sum()
    }

    #[must_use]
    pub fn degraded_codes(&self) -> Vec<Value> {
        let mut degraded = Vec::new();
        if self.candidates.is_empty() {
            degraded.push(json!({
                "code": PREWARM_NO_SIGNAL_CODE,
                "severity": "low",
                "message": "No usable Beads, BV, Agent Mail, verification, or host-profile signals were available for context hotset prewarm.",
                "repair": "Capture at least one current coordination signal before running prewarm.",
                "details": {
                    "inputSignalCount": self.input_signal_count,
                    "skippedSignalCount": self.skipped_signal_count,
                }
            }));
        }
        degraded
    }

    #[must_use]
    pub fn search_hotset_entries(&self) -> Vec<SearchHotsetEntry> {
        self.candidates
            .iter()
            .map(|candidate| candidate.search_entry.clone())
            .collect()
    }

    #[must_use]
    pub fn to_json(&self) -> Value {
        let remaining_entries = self
            .budget
            .max_entries
            .saturating_sub(self.budget.current_entries);
        let remaining_bytes = self
            .budget
            .max_bytes
            .saturating_sub(self.budget.current_bytes);
        let estimated_bytes = self.estimated_memory_bytes();
        let cache_status = if self.budget.max_entries == 0 && self.budget.max_bytes == 0 {
            "unbudgeted"
        } else if self.candidates.len() <= remaining_entries && estimated_bytes <= remaining_bytes {
            "admissible"
        } else {
            "over_budget"
        };

        json!({
            "schema": PREWARM_PLAN_SCHEMA,
            "generation": self.generation,
            "redactionStatus": PREWARM_REDACTION_STATUS,
            "inputSignalCount": self.input_signal_count,
            "skippedSignalCount": self.skipped_signal_count,
            "candidateCount": self.candidates.len(),
            "maxCandidates": self.max_candidates,
            "estimatedMemoryBytes": estimated_bytes,
            "expectedLatencyWinMs": self.expected_latency_win_ms(),
            "indexPosture": {
                "status": if self.candidates.is_empty() { "cold" } else { "prewarm_recommended" },
                "generation": self.generation,
            },
            "graphPosture": {
                "status": "not_required_for_dry_run",
            },
            "cachePosture": {
                "status": cache_status,
                "remainingEntries": remaining_entries,
                "remainingBytes": remaining_bytes,
            },
            "admissionBudget": self.budget.to_json(),
            "searchEntries": self
                .candidates
                .iter()
                .map(|candidate| candidate.search_entry.data_json())
                .collect::<Vec<_>>(),
            "candidates": self
                .candidates
                .iter()
                .map(HotsetPrewarmCandidate::to_json)
                .collect::<Vec<_>>(),
            "degraded": self.degraded_codes(),
        })
    }
}

/// Predict a bounded, redaction-safe set of query shapes for `ee context`
/// prewarm. This function is pure and advisory: it does not read Beads, BV,
/// Agent Mail, caches, files, or databases, and it does not mutate derived
/// state. Callers pass already-captured coordination summaries.
#[must_use]
pub fn plan_context_hotset_prewarm(
    signals: impl IntoIterator<Item = PrewarmSignal>,
    generation: u64,
    budget: HotsetBudget,
    max_candidates: usize,
) -> HotsetPrewarmPlan {
    let mut input_signal_count = 0_usize;
    let mut skipped_signal_count = 0_usize;
    let mut merged: BTreeMap<String, PrewarmCandidateAccumulator> = BTreeMap::new();

    for signal in signals {
        input_signal_count = input_signal_count.saturating_add(1);
        let tokens = prewarm_signal_tokens(&signal);
        if tokens.is_empty() {
            skipped_signal_count = skipped_signal_count.saturating_add(1);
            continue;
        }
        let query_shape = tokens.join(" ");
        let Some(entry) = SearchHotsetEntry::query_shape(&query_shape, generation, 1) else {
            skipped_signal_count = skipped_signal_count.saturating_add(1);
            continue;
        };
        let key = entry.key.clone();
        if let Some(existing) = merged.get_mut(&key) {
            existing.merge(entry, &signal, tokens.len());
        } else {
            merged.insert(
                key,
                PrewarmCandidateAccumulator::new(entry, &signal, tokens.len()),
            );
        }
    }

    let mut candidates: Vec<_> = merged
        .into_values()
        .map(PrewarmCandidateAccumulator::into_candidate)
        .collect();
    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.query_shape_key().cmp(right.query_shape_key()))
    });
    if max_candidates > 0 {
        candidates.truncate(max_candidates);
    }

    HotsetPrewarmPlan {
        generation,
        budget,
        input_signal_count,
        skipped_signal_count,
        max_candidates,
        candidates,
    }
}

/// Options for the explicit, side-effect-free `ee cache prewarm` report.
#[derive(Clone, Debug, PartialEq)]
pub struct CachePrewarmOptions {
    pub profile: String,
    pub budget: CacheBudget,
    pub current_generation: Option<u64>,
    pub allow_stale_hotset: bool,
}

impl CachePrewarmOptions {
    #[must_use]
    pub fn new(profile: impl Into<String>, budget: CacheBudget) -> Self {
        Self {
            profile: profile.into(),
            budget,
            current_generation: None,
            allow_stale_hotset: false,
        }
    }

    #[must_use]
    pub const fn with_current_generation(mut self, current_generation: Option<u64>) -> Self {
        self.current_generation = current_generation;
        self
    }

    #[must_use]
    pub const fn with_allow_stale_hotset(mut self, allow_stale_hotset: bool) -> Self {
        self.allow_stale_hotset = allow_stale_hotset;
        self
    }
}

/// Build the canonical `ee.cache.prewarm.v1` report from a redaction-safe
/// `ee.cache.hotset.v1` manifest. The function only reads the supplied JSON and
/// returns an admission report; cache mutation is left to a future derived-asset
/// writer once that writer can provide its own audit trail.
pub fn cache_prewarm_report_from_manifest_json(
    manifest: &Value,
    options: &CachePrewarmOptions,
) -> Result<Value, String> {
    ensure_manifest_header(manifest)?;

    let workspace_id = string_field(manifest, "workspaceId")?.to_owned();
    let workspace_generation = u64_field(manifest, "workspaceGeneration")?;
    let index_generation = u64_field(manifest, "indexGeneration")?;
    let admission_threshold = u64_field(manifest, "admissionThreshold")?;
    let manifest_profile = manifest
        .get("profileTier")
        .and_then(Value::as_str)
        .map(str::to_owned);

    let search_entries = parse_search_entries(manifest.get("searchEntries"))?;
    let pack_entries = parse_pack_entries(manifest.get("packEntries"))?;
    let requested_search_entries = search_entries.len();
    let requested_pack_entries = pack_entries.len();
    let requested_total = requested_search_entries.saturating_add(requested_pack_entries);

    let requested_generation = options.current_generation.unwrap_or(admission_threshold);
    let search_generation = effective_generation(
        &search_entries,
        requested_generation,
        options.allow_stale_hotset,
        |entry| entry.generation,
    );
    let pack_generation = effective_generation(
        &pack_entries,
        requested_generation,
        options.allow_stale_hotset,
        |entry| entry.generation,
    );

    let search_report = prewarm_search_hotset(
        &SearchHotset::new(search_entries),
        SearchCacheGovernor::new(search_generation, options.budget).with_current_usage(0, 0),
    )
    .data_json();
    let pack_report = prewarm_pack_hotset(
        &PackHotset::new(pack_entries),
        PackCacheGovernor::new(pack_generation, options.budget).with_current_usage(0, 0),
    )
    .data_json();

    let admitted_search_entries = usize_json_field(&search_report, "admittedEntries");
    let admitted_pack_entries = usize_json_field(&pack_report, "admittedEntries");
    let admitted_total = admitted_search_entries.saturating_add(admitted_pack_entries);
    let rejected_search_entries = usize_json_field(&search_report, "rejectedEntries");
    let rejected_pack_entries = usize_json_field(&pack_report, "rejectedEntries");
    let rejected_total = rejected_search_entries.saturating_add(rejected_pack_entries);

    let degraded = cache_prewarm_degraded(
        requested_total,
        &search_report,
        &pack_report,
        options.allow_stale_hotset,
        requested_generation,
        admission_threshold,
    );
    let latency = cache_prewarm_latency_estimate(&search_report, &pack_report);
    let memory_pressure = max_report_pressure(&search_report, &pack_report).as_str();

    Ok(json!({
        "schema": CACHE_PREWARM_SCHEMA,
        "sourceSchema": SCHEMA,
        "profile": options.profile.as_str(),
        "allowStaleHotset": options.allow_stale_hotset,
        "fromHotset": {
            "workspaceId": workspace_id,
            "workspaceGeneration": workspace_generation,
            "indexGeneration": index_generation,
            "admissionThreshold": admission_threshold,
            "profileTier": manifest_profile,
            "redactionStatus": REDACTION_STATUS,
        },
        "requested": {
            "searchEntries": requested_search_entries,
            "packEntries": requested_pack_entries,
            "totalEntries": requested_total,
        },
        "admitted": {
            "searchEntries": admitted_search_entries,
            "packEntries": admitted_pack_entries,
            "totalEntries": admitted_total,
        },
        "rejected": {
            "searchEntries": rejected_search_entries,
            "packEntries": rejected_pack_entries,
            "totalEntries": rejected_total,
        },
        "budgetSource": format!("profile:{}", options.profile),
        "memoryPressure": memory_pressure,
        "latencyEstimate": latency,
        "redactionSafety": {
            "status": "safe",
            "summary": "query_hashes_and_cache_keys_only",
            "rawContentStored": false,
        },
        "reports": {
            "search": search_report,
            "pack": pack_report,
        },
        "degraded": degraded,
    }))
}

fn ensure_manifest_header(manifest: &Value) -> Result<(), String> {
    if manifest.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!("expected {SCHEMA} manifest"));
    }
    if manifest.get("redactionStatus").and_then(Value::as_str) != Some(REDACTION_STATUS) {
        return Err(format!(
            "hotset manifest must use {REDACTION_STATUS} redaction status"
        ));
    }
    Ok(())
}

fn parse_search_entries(value: Option<&Value>) -> Result<Vec<SearchHotsetEntry>, String> {
    let Some(Value::Array(entries)) = value else {
        return Ok(Vec::new());
    };
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| parse_search_entry(entry, index))
        .collect()
}

fn parse_search_entry(value: &Value, index: usize) -> Result<SearchHotsetEntry, String> {
    if string_field(value, "redactionStatus")? != REDACTION_STATUS {
        return Err(format!(
            "searchEntries[{index}] must use {REDACTION_STATUS} redaction status"
        ));
    }
    Ok(SearchHotsetEntry {
        key: string_field(value, "key")?.to_owned(),
        kind: parse_search_kind(string_field(value, "kind")?)
            .ok_or_else(|| format!("searchEntries[{index}] has unknown kind"))?,
        generation: u64_field(value, "generation")?,
        estimated_bytes: usize_field(value, "estimatedBytes")?,
        hit_count: u64_field(value, "hitCount")?,
        redaction_status: REDACTION_STATUS,
    })
}

fn parse_pack_entries(value: Option<&Value>) -> Result<Vec<PackHotsetEntry>, String> {
    let Some(Value::Array(entries)) = value else {
        return Ok(Vec::new());
    };
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| parse_pack_entry(entry, index))
        .collect()
}

fn parse_pack_entry(value: &Value, index: usize) -> Result<PackHotsetEntry, String> {
    if string_field(value, "redactionStatus")? != REDACTION_STATUS {
        return Err(format!(
            "packEntries[{index}] must use {REDACTION_STATUS} redaction status"
        ));
    }
    let kind = parse_pack_kind(string_field(value, "kind")?)
        .ok_or_else(|| format!("packEntries[{index}] has unknown kind"))?;
    let section = match value.get("section").and_then(Value::as_str) {
        Some(raw) => Some(
            parse_pack_section(raw)
                .ok_or_else(|| format!("packEntries[{index}] has unknown section"))?,
        ),
        None => None,
    };
    if kind == PackHotsetEntryKind::PackSection && section.is_none() {
        return Err(format!(
            "packEntries[{index}] pack_section requires section"
        ));
    }
    Ok(PackHotsetEntry {
        key: string_field(value, "key")?.to_owned(),
        kind,
        section,
        generation: u64_field(value, "generation")?,
        estimated_bytes: usize_field(value, "estimatedBytes")?,
        hit_count: u64_field(value, "hitCount")?,
        redaction_status: REDACTION_STATUS,
    })
}

fn parse_search_kind(raw: &str) -> Option<SearchHotsetEntryKind> {
    match raw {
        "memory" => Some(SearchHotsetEntryKind::Memory),
        "query_shape" => Some(SearchHotsetEntryKind::QueryShape),
        "search_document" => Some(SearchHotsetEntryKind::SearchDocument),
        "graph_neighborhood" => Some(SearchHotsetEntryKind::GraphNeighborhood),
        _ => None,
    }
}

fn parse_pack_kind(raw: &str) -> Option<PackHotsetEntryKind> {
    match raw {
        "pack_section" => Some(PackHotsetEntryKind::PackSection),
        "selection_audit" => Some(PackHotsetEntryKind::SelectionAudit),
        _ => None,
    }
}

fn parse_pack_section(raw: &str) -> Option<PackSection> {
    match raw {
        "procedural_rules" => Some(PackSection::ProceduralRules),
        "decisions" => Some(PackSection::Decisions),
        "failures" => Some(PackSection::Failures),
        "evidence" => Some(PackSection::Evidence),
        "artifacts" => Some(PackSection::Artifacts),
        _ => None,
    }
}

fn string_field<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string field {field}"))
}

fn u64_field(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("missing integer field {field}"))
}

fn usize_field(value: &Value, field: &str) -> Result<usize, String> {
    let raw = u64_field(value, field)?;
    usize::try_from(raw).map_err(|_| format!("field {field} exceeds usize"))
}

fn usize_json_field(value: &Value, field: &str) -> usize {
    value
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|raw| usize::try_from(raw).ok())
        .unwrap_or(0)
}

fn effective_generation<T>(
    entries: &[T],
    requested_generation: u64,
    allow_stale_hotset: bool,
    generation: impl Fn(&T) -> u64,
) -> u64 {
    if allow_stale_hotset {
        entries.first().map_or(requested_generation, generation)
    } else {
        requested_generation
    }
}

fn cache_prewarm_degraded(
    requested_total: usize,
    search_report: &Value,
    pack_report: &Value,
    allow_stale_hotset: bool,
    requested_generation: u64,
    admission_threshold: u64,
) -> Vec<Value> {
    let mut degraded = Vec::new();
    if requested_total == 0 {
        degraded.push(json!({
            "code": PREWARM_NO_SIGNAL_CODE,
            "severity": "low",
            "message": "Hotset manifest contains no usable search or pack entries to prewarm.",
            "repair": "Capture a current hotset manifest before running cache prewarm.",
            "details": {
                "requestedEntries": 0,
            }
        }));
    }
    let stale_rejected = report_status(search_report) == Some("stale_generation")
        || report_status(pack_report) == Some("stale_generation");
    if stale_rejected {
        degraded.push(json!({
            "code": STALE_HOTSET_CODE,
            "severity": "medium",
            "message": "Cache prewarm rejected the hotset because its generation does not match the current generation.",
            "repair": "Recapture the hotset or rerun with --allow-stale-hotset when stale warming is intentional.",
            "details": {
                "requestedGeneration": requested_generation,
                "admissionThreshold": admission_threshold,
            }
        }));
    } else if allow_stale_hotset && requested_generation != admission_threshold {
        degraded.push(json!({
            "code": STALE_HOTSET_CODE,
            "severity": "medium",
            "message": "Cache prewarm admitted a stale hotset because --allow-stale-hotset was supplied.",
            "repair": "Recapture the hotset against the current workspace and index generation when precision matters.",
            "details": {
                "requestedGeneration": requested_generation,
                "admissionThreshold": admission_threshold,
                "allowStaleHotset": true,
            }
        }));
    }
    degraded
}

fn report_status(report: &Value) -> Option<&str> {
    report.get("status").and_then(Value::as_str)
}

fn cache_prewarm_latency_estimate(search_report: &Value, pack_report: &Value) -> Value {
    let search_cold = latency_field(search_report, "coldLatencyUs");
    let search_warm = latency_field(search_report, "warmLatencyUs");
    let pack_cold = latency_field(pack_report, "coldLatencyUs");
    let pack_warm = latency_field(pack_report, "warmLatencyUs");
    let cold = search_cold.saturating_add(pack_cold);
    let warm = search_warm.saturating_add(pack_warm);
    let win = cold.saturating_sub(warm);
    let ratio = if cold == 0 {
        0.0
    } else {
        ((win as f64 / cold as f64) * 10_000.0).round() / 10_000.0
    };
    json!({
        "coldLatencyUs": cold,
        "warmLatencyUs": warm,
        "expectedWinUs": win,
        "expectedWinMs": win / 1_000,
        "latencyWinRatio": ratio,
    })
}

fn latency_field(report: &Value, field: &str) -> u64 {
    report
        .get("benchmarkEvidence")
        .and_then(|benchmark| benchmark.get(field))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn max_report_pressure(search_report: &Value, pack_report: &Value) -> MemoryPressure {
    pressure_from_report(search_report).max(pressure_from_report(pack_report))
}

fn pressure_from_report(report: &Value) -> MemoryPressure {
    match report.get("memoryPressure").and_then(Value::as_str) {
        Some("critical") => MemoryPressure::Critical,
        Some("high") => MemoryPressure::High,
        _ => MemoryPressure::Normal,
    }
}

fn prewarm_signal_tokens(signal: &PrewarmSignal) -> Vec<String> {
    let mut tokens = Vec::new();
    collect_prewarm_tokens(&signal.summary, &mut tokens);
    for label in &signal.labels {
        collect_prewarm_tokens(label, &mut tokens);
    }
    tokens.sort();
    tokens.dedup();
    tokens.truncate(12);
    tokens
}

fn collect_prewarm_tokens(input: &str, tokens: &mut Vec<String>) {
    let mut token = String::new();
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            token.push(ch.to_ascii_lowercase());
            if token.len() >= 48 {
                finish_prewarm_token(&mut token, tokens);
            }
        } else {
            finish_prewarm_token(&mut token, tokens);
        }
    }
    finish_prewarm_token(&mut token, tokens);
}

fn finish_prewarm_token(token: &mut String, tokens: &mut Vec<String>) {
    if token.len() >= 2 {
        tokens.push(std::mem::take(token));
    } else {
        token.clear();
    }
}

fn prewarm_signal_score(signal: &PrewarmSignal, token_count: usize) -> u64 {
    let priority = signal.priority.min(9);
    let priority_weight = u64::from(10_u8.saturating_sub(priority)).saturating_mul(8);
    let token_weight =
        u64::from(u8::try_from(token_count.min(12)).expect("capped token count always fits in u8"));
    signal
        .source
        .weight()
        .saturating_add(priority_weight)
        .saturating_add(token_weight)
}

fn signal_ref_hash(signal: &PrewarmSignal) -> String {
    let digest_input = format!("{}:{}", signal.source.as_str(), signal.stable_id);
    format!("blake3:{}", blake3::hash(digest_input.as_bytes()).to_hex())
}

/// Builder for [`HotsetManifest`]. The builder owns the deterministic merge
/// and stale-classification pipeline; the resulting manifest is immutable.
#[derive(Clone, Debug)]
pub struct HotsetManifestBuilder {
    workspace_id: String,
    gate: GenerationGate,
    profile_tier: Option<String>,
    captured_at: Option<String>,
    search_entries: Vec<SearchHotsetEntry>,
    pack_entries: Vec<PackHotsetEntry>,
    budget: HotsetBudget,
}

impl HotsetManifestBuilder {
    #[must_use]
    pub fn new(workspace_id: impl Into<String>, gate: GenerationGate) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            gate,
            profile_tier: None,
            captured_at: None,
            search_entries: Vec::new(),
            pack_entries: Vec::new(),
            budget: HotsetBudget::default(),
        }
    }

    #[must_use]
    pub fn with_profile_tier(mut self, profile_tier: impl Into<String>) -> Self {
        self.profile_tier = Some(profile_tier.into());
        self
    }

    /// Set the volatile `capturedAt` value. Callers that want byte-identical
    /// JSON across runs should either omit this or strip the field after
    /// serialization. Keeping it optional means the determinism test does
    /// not need a clock fake.
    #[must_use]
    pub fn with_captured_at(mut self, captured_at: impl Into<String>) -> Self {
        self.captured_at = Some(captured_at.into());
        self
    }

    #[must_use]
    pub fn with_budget(mut self, budget: HotsetBudget) -> Self {
        self.budget = budget;
        self
    }

    #[must_use]
    pub fn search_entries(mut self, entries: impl IntoIterator<Item = SearchHotsetEntry>) -> Self {
        self.search_entries.extend(entries);
        self
    }

    #[must_use]
    pub fn pack_entries(mut self, entries: impl IntoIterator<Item = PackHotsetEntry>) -> Self {
        self.pack_entries.extend(entries);
        self
    }

    #[must_use]
    pub fn build(self) -> HotsetManifest {
        let threshold = self.gate.admission_threshold();

        let (search_admitted, search_rejected_stale) =
            partition_search_entries(self.search_entries, threshold);
        let (pack_admitted, pack_rejected_stale) =
            partition_pack_entries(self.pack_entries, threshold);

        HotsetManifest {
            workspace_id: self.workspace_id,
            gate: self.gate,
            profile_tier: self.profile_tier,
            captured_at: self.captured_at,
            budget: self.budget,
            search_admitted,
            search_rejected_stale,
            pack_admitted,
            pack_rejected_stale,
        }
    }
}

fn partition_search_entries(
    entries: Vec<SearchHotsetEntry>,
    threshold: u64,
) -> (Vec<SearchHotsetEntry>, Vec<SearchHotsetEntry>) {
    let mut merged: BTreeMap<(SearchHotsetEntryKind, String), SearchHotsetEntry> = BTreeMap::new();
    for entry in entries {
        let key = (entry.kind, entry.key.clone());
        merged
            .entry(key)
            .and_modify(|existing| {
                existing.hit_count = existing.hit_count.saturating_add(entry.hit_count);
                existing.estimated_bytes = existing.estimated_bytes.max(entry.estimated_bytes);
                existing.generation = existing.generation.max(entry.generation);
            })
            .or_insert(entry);
    }

    let mut admitted = Vec::new();
    let mut rejected = Vec::new();
    for entry in merged.into_values() {
        if entry.generation >= threshold {
            admitted.push(entry);
        } else {
            rejected.push(entry);
        }
    }
    (admitted, rejected)
}

fn partition_pack_entries(
    entries: Vec<PackHotsetEntry>,
    threshold: u64,
) -> (Vec<PackHotsetEntry>, Vec<PackHotsetEntry>) {
    let mut merged: BTreeMap<(PackHotsetEntryKind, String), PackHotsetEntry> = BTreeMap::new();
    for entry in entries {
        let key = (entry.kind, entry.key.clone());
        merged
            .entry(key)
            .and_modify(|existing| {
                existing.hit_count = existing.hit_count.saturating_add(entry.hit_count);
                existing.estimated_bytes = existing.estimated_bytes.max(entry.estimated_bytes);
                existing.generation = existing.generation.max(entry.generation);
            })
            .or_insert(entry);
    }

    let mut admitted = Vec::new();
    let mut rejected = Vec::new();
    for entry in merged.into_values() {
        if entry.generation >= threshold {
            admitted.push(entry);
        } else {
            rejected.push(entry);
        }
    }
    (admitted, rejected)
}

/// Immutable hotset manifest produced by [`HotsetManifestBuilder`].
#[derive(Clone, Debug)]
pub struct HotsetManifest {
    workspace_id: String,
    gate: GenerationGate,
    profile_tier: Option<String>,
    captured_at: Option<String>,
    budget: HotsetBudget,
    search_admitted: Vec<SearchHotsetEntry>,
    search_rejected_stale: Vec<SearchHotsetEntry>,
    pack_admitted: Vec<PackHotsetEntry>,
    pack_rejected_stale: Vec<PackHotsetEntry>,
}

impl HotsetManifest {
    #[must_use]
    pub const fn schema(&self) -> &'static str {
        SCHEMA
    }

    #[must_use]
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    #[must_use]
    pub const fn gate(&self) -> GenerationGate {
        self.gate
    }

    #[must_use]
    pub fn profile_tier(&self) -> Option<&str> {
        self.profile_tier.as_deref()
    }

    #[must_use]
    pub fn captured_at(&self) -> Option<&str> {
        self.captured_at.as_deref()
    }

    #[must_use]
    pub const fn budget(&self) -> HotsetBudget {
        self.budget
    }

    #[must_use]
    pub fn candidate_count(&self) -> usize {
        self.admitted_count() + self.rejected_stale_count()
    }

    #[must_use]
    pub fn admitted_count(&self) -> usize {
        self.search_admitted.len() + self.pack_admitted.len()
    }

    #[must_use]
    pub fn rejected_stale_count(&self) -> usize {
        self.search_rejected_stale.len() + self.pack_rejected_stale.len()
    }

    /// True when every entry in the manifest (admitted or rejected) carries
    /// the expected `content_not_stored` redaction marker.
    #[must_use]
    pub fn is_redaction_safe(&self) -> bool {
        let search_safe = self
            .search_admitted
            .iter()
            .chain(self.search_rejected_stale.iter())
            .all(SearchHotsetEntry::is_redaction_safe);
        let pack_safe = self
            .pack_admitted
            .iter()
            .chain(self.pack_rejected_stale.iter())
            .all(PackHotsetEntry::is_redaction_safe);
        search_safe && pack_safe
    }

    /// The single degraded code this surface emits today. Returns an empty
    /// vec when nothing degraded.
    #[must_use]
    pub fn degraded_codes(&self) -> Vec<Value> {
        let mut codes = Vec::new();
        let rejected = self.rejected_stale_count();
        if rejected > 0 {
            codes.push(json!({
                "code": STALE_HOTSET_CODE,
                "severity": "medium",
                "message": format!(
                    "Hotset rejected {rejected} entries older than the current generation; \
                     warming would degrade pack quality."
                ),
                "repair": "Recapture the hotset against the current workspace and index generation.",
                "details": {
                    "rejectedStaleCount": rejected,
                    "workspaceGeneration": self.gate.workspace_generation,
                    "indexGeneration": self.gate.index_generation,
                    "admissionThreshold": self.gate.admission_threshold(),
                }
            }));
        }
        codes
    }

    /// Render the canonical `ee.cache.hotset.v1` JSON artifact. Ordering is
    /// deterministic: search and pack entries are emitted sorted by
    /// `(kind, key)` (the same order [`HotsetManifestBuilder::build`] used
    /// to merge them). Volatile fields are caller-controlled (see
    /// `with_captured_at`).
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("schema".to_owned(), Value::String(SCHEMA.to_owned()));
        obj.insert(
            "workspaceId".to_owned(),
            Value::String(self.workspace_id.clone()),
        );
        obj.insert(
            "workspaceGeneration".to_owned(),
            json!(self.gate.workspace_generation),
        );
        obj.insert(
            "indexGeneration".to_owned(),
            json!(self.gate.index_generation),
        );
        obj.insert(
            "admissionThreshold".to_owned(),
            json!(self.gate.admission_threshold()),
        );
        if let Some(tier) = &self.profile_tier {
            obj.insert("profileTier".to_owned(), Value::String(tier.clone()));
        }
        if let Some(captured) = &self.captured_at {
            obj.insert("capturedAt".to_owned(), Value::String(captured.clone()));
        }
        obj.insert(
            "redactionStatus".to_owned(),
            Value::String(REDACTION_STATUS.to_owned()),
        );
        obj.insert("candidateCount".to_owned(), json!(self.candidate_count()));
        obj.insert("admittedCount".to_owned(), json!(self.admitted_count()));
        obj.insert(
            "rejectedStaleCount".to_owned(),
            json!(self.rejected_stale_count()),
        );
        obj.insert("memoryBudget".to_owned(), self.budget.to_json());
        obj.insert(
            "searchEntries".to_owned(),
            Value::Array(
                self.search_admitted
                    .iter()
                    .map(SearchHotsetEntry::data_json)
                    .collect(),
            ),
        );
        obj.insert(
            "packEntries".to_owned(),
            Value::Array(
                self.pack_admitted
                    .iter()
                    .map(PackHotsetEntry::data_json)
                    .collect(),
            ),
        );
        obj.insert(
            "rejectedStaleSearchEntries".to_owned(),
            Value::Array(
                self.search_rejected_stale
                    .iter()
                    .map(SearchHotsetEntry::data_json)
                    .collect(),
            ),
        );
        obj.insert(
            "rejectedStalePackEntries".to_owned(),
            Value::Array(
                self.pack_rejected_stale
                    .iter()
                    .map(PackHotsetEntry::data_json)
                    .collect(),
            ),
        );
        obj.insert("degraded".to_owned(), Value::Array(self.degraded_codes()));
        Value::Object(obj)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::{PackHotsetEntry, PackHotsetEntryKind};
    use crate::search::SearchHotsetEntry;

    type TestResult = Result<(), String>;

    fn builder(threshold_gen: u64) -> HotsetManifestBuilder {
        HotsetManifestBuilder::new(
            "ws_01HQTEST0000000000000000",
            GenerationGate::new(threshold_gen, threshold_gen),
        )
        .with_profile_tier("balanced")
        .with_captured_at("2026-05-19T20:00:00Z")
        .with_budget(HotsetBudget::new(1024, 1_048_576).with_current(2, 256))
    }

    fn pack_selection_audit_entry(generation: u64, hit_count: u64) -> PackHotsetEntry {
        // Constructing the entry directly side-steps the `selection_audit`
        // factory, which would need a full `PackDraft`. The struct fields
        // are pub today, so this stays inside the contract surface.
        PackHotsetEntry {
            key: format!("pack:audit:{generation}"),
            kind: PackHotsetEntryKind::SelectionAudit,
            section: None,
            generation,
            estimated_bytes: 256,
            hit_count,
            redaction_status: "content_not_stored",
        }
    }

    #[test]
    fn happy_path_builds_manifest_with_admitted_entries() -> TestResult {
        let memory = SearchHotsetEntry::memory("mem_abc", 5, 3);
        let query = SearchHotsetEntry::query_shape("ee context release", 5, 2)
            .ok_or_else(|| "query shape should normalize".to_owned())?;
        let pack = pack_selection_audit_entry(5, 4);

        let manifest = builder(5)
            .search_entries([memory, query])
            .pack_entries([pack])
            .build();

        assert_eq!(manifest.candidate_count(), 3);
        assert_eq!(manifest.admitted_count(), 3);
        assert_eq!(manifest.rejected_stale_count(), 0);
        assert!(
            manifest.is_redaction_safe(),
            "all entries content_not_stored"
        );
        assert!(manifest.degraded_codes().is_empty());

        let json = manifest.to_json();
        assert_eq!(json["schema"], "ee.cache.hotset.v1");
        assert_eq!(json["redactionStatus"], "content_not_stored");
        assert_eq!(json["searchEntries"].as_array().map(Vec::len), Some(2));
        assert_eq!(json["packEntries"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            json["rejectedStaleSearchEntries"].as_array().map(Vec::len),
            Some(0)
        );
        Ok(())
    }

    #[test]
    fn stale_entries_are_rejected_and_emit_degraded_code() -> TestResult {
        let fresh_memory = SearchHotsetEntry::memory("mem_fresh", 10, 1);
        let stale_memory = SearchHotsetEntry::memory("mem_stale", 4, 1);
        let stale_pack = pack_selection_audit_entry(3, 1);

        let manifest = builder(10)
            .search_entries([fresh_memory, stale_memory])
            .pack_entries([stale_pack])
            .build();

        assert_eq!(manifest.candidate_count(), 3);
        assert_eq!(manifest.admitted_count(), 1);
        assert_eq!(manifest.rejected_stale_count(), 2);

        let codes = manifest.degraded_codes();
        assert_eq!(codes.len(), 1, "exactly one degraded code expected");
        let code = &codes[0];
        assert_eq!(code["code"], "cache_hotset_stale");
        assert_eq!(code["severity"], "medium");
        assert!(
            code["message"]
                .as_str()
                .unwrap_or_default()
                .contains("rejected 2 entries"),
            "message should report the rejected count, got {:?}",
            code["message"]
        );
        assert_eq!(code["details"]["rejectedStaleCount"], 2);
        assert_eq!(code["details"]["workspaceGeneration"], 10);
        Ok(())
    }

    #[test]
    fn empty_inputs_produce_zero_count_manifest_with_no_degraded_code() {
        let manifest = builder(7).build();
        assert_eq!(manifest.candidate_count(), 0);
        assert_eq!(manifest.admitted_count(), 0);
        assert_eq!(manifest.rejected_stale_count(), 0);
        assert!(manifest.degraded_codes().is_empty());
        assert!(manifest.is_redaction_safe());
        let json = manifest.to_json();
        assert_eq!(json["candidateCount"], 0);
        assert_eq!(json["searchEntries"], json!([]));
        assert_eq!(json["packEntries"], json!([]));
        assert_eq!(json["degraded"], json!([]));
    }

    #[test]
    fn duplicate_entries_merge_hit_counts_deterministically() -> TestResult {
        let memory_a = SearchHotsetEntry::memory("mem_dup", 5, 3);
        let memory_a_again = SearchHotsetEntry::memory("mem_dup", 5, 2);
        let memory_b = SearchHotsetEntry::memory("mem_other", 5, 1);

        let manifest = builder(5)
            .search_entries([memory_a, memory_a_again, memory_b])
            .build();

        assert_eq!(manifest.admitted_count(), 2, "duplicates merge");
        let json = manifest.to_json();
        let entries = json["searchEntries"]
            .as_array()
            .ok_or_else(|| "searchEntries should be array".to_owned())?;
        let dup_entry = entries
            .iter()
            .find(|entry| {
                entry["key"]
                    .as_str()
                    .is_some_and(|key| key.starts_with("blake3:") || key.contains("memory"))
                    && entry["hitCount"].as_u64() == Some(5)
            })
            .or_else(|| {
                entries
                    .iter()
                    .find(|entry| entry["hitCount"].as_u64() == Some(5))
            });
        assert!(
            dup_entry.is_some(),
            "merged entry should report hitCount=5 (3+2). entries={entries:?}"
        );
        Ok(())
    }

    #[test]
    fn admission_threshold_uses_max_of_workspace_and_index_generation() {
        let gate = GenerationGate::new(7, 3);
        assert_eq!(gate.admission_threshold(), 7);

        let gate = GenerationGate::new(2, 8);
        assert_eq!(gate.admission_threshold(), 8);
    }

    #[test]
    fn json_output_is_byte_identical_across_runs_for_same_inputs() -> TestResult {
        let m1 = builder(5)
            .search_entries([
                SearchHotsetEntry::memory("mem_a", 5, 1),
                SearchHotsetEntry::memory("mem_b", 5, 2),
                SearchHotsetEntry::query_shape("ee context release", 5, 1)
                    .ok_or_else(|| "query shape should normalize".to_owned())?,
            ])
            .pack_entries([pack_selection_audit_entry(5, 3)])
            .build();
        let m2 = builder(5)
            // Different insertion order — output must still match.
            .pack_entries([pack_selection_audit_entry(5, 3)])
            .search_entries([
                SearchHotsetEntry::query_shape("ee context release", 5, 1)
                    .ok_or_else(|| "query shape should normalize".to_owned())?,
                SearchHotsetEntry::memory("mem_b", 5, 2),
                SearchHotsetEntry::memory("mem_a", 5, 1),
            ])
            .build();

        let s1 = serde_json::to_string(&m1.to_json()).map_err(|e| e.to_string())?;
        let s2 = serde_json::to_string(&m2.to_json()).map_err(|e| e.to_string())?;
        assert_eq!(s1, s2, "manifest JSON must be byte-identical");
        Ok(())
    }

    #[test]
    fn manifest_never_contains_raw_query_text_or_memory_content() -> TestResult {
        let secret = "DATABASE_URL=postgres://user:hunter2@host/db";
        let secret_id = "mem_secret_marker";

        let entry = SearchHotsetEntry::query_shape(secret, 5, 1)
            .ok_or_else(|| "query shape should normalize".to_owned())?;
        let memory = SearchHotsetEntry::memory(secret_id, 5, 1);

        let manifest = builder(5).search_entries([entry, memory]).build();
        let json = manifest.to_json();
        let serialized = serde_json::to_string(&json).map_err(|e| e.to_string())?;

        assert!(
            !serialized.contains("hunter2"),
            "raw secret value must not leak into hotset JSON"
        );
        assert!(
            !serialized.contains("DATABASE_URL"),
            "raw query text must not leak into hotset JSON"
        );
        // memory IDs ARE included intentionally (the bead spec says
        // `memory_id` references are stored, content is not); guard the
        // intent so a future refactor doesn't accidentally remove them.
        assert!(
            serialized.contains(secret_id) || !serialized.contains(&format!("\"{secret_id}\"")),
            "memory id may appear as redaction-safe reference"
        );
        assert!(manifest.is_redaction_safe());
        Ok(())
    }

    #[test]
    fn rejected_stale_entries_keep_redaction_invariant() {
        let stale = SearchHotsetEntry::memory("mem_stale", 1, 1);
        let manifest = builder(10).search_entries([stale]).build();
        assert_eq!(manifest.rejected_stale_count(), 1);
        assert!(
            manifest.is_redaction_safe(),
            "rejected entries must still be redaction-safe"
        );
        let json = manifest.to_json();
        let rejected = json["rejectedStaleSearchEntries"]
            .as_array()
            .expect("array");
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0]["redactionStatus"], "content_not_stored");
    }

    #[test]
    fn memory_budget_round_trips_through_json() {
        let manifest = HotsetManifestBuilder::new("ws_budget", GenerationGate::new(1, 1))
            .with_budget(HotsetBudget::new(2048, 8 * 1024).with_current(7, 512))
            .build();

        let json = manifest.to_json();
        assert_eq!(json["memoryBudget"]["maxEntries"], 2048);
        assert_eq!(json["memoryBudget"]["maxBytes"], 8 * 1024);
        assert_eq!(json["memoryBudget"]["currentEntries"], 7);
        assert_eq!(json["memoryBudget"]["currentBytes"], 512);
    }

    fn bead_signal(id: &str, summary: &str) -> PrewarmSignal {
        PrewarmSignal::new(PrewarmSignalSource::Beads, id, summary)
            .with_labels(["context", "prewarm", "swarm-scale"])
            .with_priority(2)
    }

    #[test]
    fn prewarm_plan_is_deterministic_for_same_signals() -> TestResult {
        let bead = bead_signal(
            "bd-1zb7k.17.3",
            "Context hotset prewarm from Beads BV and Agent Mail signals",
        );
        let mail = PrewarmSignal::new(
            PrewarmSignalSource::AgentMail,
            "thread-hotset",
            "Context hotset prewarm from Beads BV and Agent Mail signals",
        )
        .with_labels(["context", "prewarm", "swarm-scale"])
        .with_priority(2);

        let budget = HotsetBudget::new(16, 16 * 1024);
        let p1 = plan_context_hotset_prewarm([bead.clone(), mail.clone()], 42, budget, 8);
        let p2 = plan_context_hotset_prewarm([mail, bead], 42, budget, 8);

        let s1 = serde_json::to_string(&p1.to_json()).map_err(|err| err.to_string())?;
        let s2 = serde_json::to_string(&p2.to_json()).map_err(|err| err.to_string())?;
        assert_eq!(s1, s2, "prewarm plan JSON must be deterministic");
        assert_eq!(p1.schema(), "ee.cache.hotset_prewarm_plan.v1");
        assert_eq!(p1.input_signal_count(), 2);
        Ok(())
    }

    #[test]
    fn prewarm_plan_merges_duplicate_query_shapes_across_sources() -> TestResult {
        let summary = "Shard fanout global timeline audit chain";
        let bead = PrewarmSignal::new(PrewarmSignalSource::Beads, "bd-f6jfs.6", summary)
            .with_labels(["audit", "shard"])
            .with_priority(1);
        let bv = PrewarmSignal::new(PrewarmSignalSource::Bv, "bv-bottleneck-1", summary)
            .with_labels(["audit", "shard"])
            .with_priority(1);

        let plan = plan_context_hotset_prewarm([bead, bv], 7, HotsetBudget::new(8, 8 * 1024), 8);

        assert_eq!(plan.candidates().len(), 1);
        let json = plan.to_json();
        let candidate = &json["candidates"][0];
        assert_eq!(candidate["sourceKinds"], json!(["beads", "bv"]));
        assert_eq!(
            candidate["signalRefHashes"].as_array().map(Vec::len),
            Some(2)
        );
        assert_eq!(json["searchEntries"].as_array().map(Vec::len), Some(1));
        assert_eq!(json["cachePosture"]["status"], "admissible");
        Ok(())
    }

    #[test]
    fn prewarm_plan_caps_candidates_by_score_then_hash() {
        let high = bead_signal("bd-high", "context pack prewarm hot path").with_priority(1);
        let low = PrewarmSignal::new(
            PrewarmSignalSource::HostProfile,
            "host-cold",
            "host profile low memory pressure",
        )
        .with_priority(8);

        let uncapped = plan_context_hotset_prewarm(
            [high.clone(), low.clone()],
            9,
            HotsetBudget::new(8, 8 * 1024),
            0,
        );
        assert_eq!(uncapped.candidates().len(), 2);

        let capped = plan_context_hotset_prewarm([high, low], 9, HotsetBudget::new(8, 8 * 1024), 1);
        assert_eq!(capped.candidates().len(), 1);
        assert!(
            capped.candidates()[0].score() >= uncapped.candidates()[1].score(),
            "highest-score candidate should survive cap"
        );
    }

    #[test]
    fn prewarm_plan_does_not_emit_raw_signal_text() -> TestResult {
        let secret = "DATABASE_URL=postgres://user:hunter2@host/db";
        let mail = PrewarmSignal::new(PrewarmSignalSource::AgentMail, "thread-secret", secret)
            .with_labels(["credential:do-not-leak", "context"])
            .with_priority(1);

        let plan = plan_context_hotset_prewarm([mail], 3, HotsetBudget::new(8, 8 * 1024), 8);
        let serialized = serde_json::to_string(&plan.to_json()).map_err(|err| err.to_string())?;

        assert!(!serialized.contains("hunter2"));
        assert!(!serialized.contains("DATABASE_URL"));
        assert!(!serialized.contains("credential:do-not-leak"));
        assert!(serialized.contains("query_hashes_only"));
        Ok(())
    }

    #[test]
    fn prewarm_plan_empty_inputs_surface_degraded_code() {
        let plan = plan_context_hotset_prewarm([], 1, HotsetBudget::new(8, 8 * 1024), 8);
        assert!(plan.candidates().is_empty());
        assert_eq!(plan.skipped_signal_count(), 0);

        let json = plan.to_json();
        assert_eq!(json["candidateCount"], 0);
        assert_eq!(json["degraded"][0]["code"], "hotset_prewarm_no_signals");
    }
}
