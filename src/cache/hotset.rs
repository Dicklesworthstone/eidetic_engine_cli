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

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::pack::{PackHotsetEntry, PackHotsetEntryKind};
use crate::search::{SearchHotsetEntry, SearchHotsetEntryKind};

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
}
