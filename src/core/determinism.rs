//! Core determinism label registry.
//!
//! Child-seed labels are part of the deterministic output contract: changing a
//! label changes every derived child seed. Keep this registry in sync with
//! `docs/determinism_seed_labels.md`.

/// Canonical labels used to derive child seeds via `Deterministic::child()`.
///
/// Adding a new label requires:
/// 1. Adding it here.
/// 2. Updating `docs/determinism_seed_labels.md`.
/// 3. Updating the production call site to use the same literal label.
pub const SEED_LABELS: &[&str] = &[
    "pack.mmr_tiebreak",
    "pack.selection_id",
    "pack.skipped_order",
    "search.score_jitter",
    "search.canonical_ties",
    "search.rerank",
    "ulid.memory",
    "ulid.audit",
    "ulid.workspace",
    "ulid.pack",
    "clustering.kmeans_init",
    "counterfactual.replay",
    "lab.replay",
];

/// Human-auditable metadata for a registered child-seed label.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SeedLabelDefinition {
    pub label: &'static str,
    pub producer_call_site: &'static str,
    pub consumer: &'static str,
}

impl SeedLabelDefinition {
    #[must_use]
    pub const fn new(
        label: &'static str,
        producer_call_site: &'static str,
        consumer: &'static str,
    ) -> Self {
        Self {
            label,
            producer_call_site,
            consumer,
        }
    }
}

/// Canonical child-seed label registry with call-site intent.
pub const SEED_LABEL_REGISTRY: &[SeedLabelDefinition] = &[
    SeedLabelDefinition::new(
        "pack.mmr_tiebreak",
        "src/core/pack.rs:assemble_pack",
        "MMR diversity tie-breaks during pack assembly",
    ),
    SeedLabelDefinition::new(
        "pack.selection_id",
        "src/core/pack.rs:persist_pack_record",
        "Context pack selection and persisted pack IDs",
    ),
    SeedLabelDefinition::new(
        "pack.skipped_order",
        "src/core/pack.rs:skipped_candidates",
        "Stable ordering for omitted or skipped candidates",
    ),
    SeedLabelDefinition::new(
        "search.score_jitter",
        "src/core/search.rs:run_search",
        "Deterministic score perturbation for stability tests",
    ),
    SeedLabelDefinition::new(
        "search.canonical_ties",
        "src/core/search.rs:canonicalize_result_ties",
        "Stable ordering for equal-score search results",
    ),
    SeedLabelDefinition::new(
        "search.rerank",
        "src/core/search.rs:rerank_top_k",
        "Rerank-stage deterministic tie-breaks",
    ),
    SeedLabelDefinition::new(
        "ulid.memory",
        "src/db/id_gen.rs:next_memory_id",
        "Memory UUIDv7 generation",
    ),
    SeedLabelDefinition::new(
        "ulid.audit",
        "src/db/id_gen.rs:next_audit_id",
        "Audit UUIDv7 generation",
    ),
    SeedLabelDefinition::new(
        "ulid.workspace",
        "src/db/id_gen.rs:next_workspace_id",
        "Workspace UUIDv7 generation",
    ),
    SeedLabelDefinition::new(
        "ulid.pack",
        "src/core/pack.rs:pack_id",
        "Context pack UUIDv7 generation",
    ),
    SeedLabelDefinition::new(
        "clustering.kmeans_init",
        "src/curate/clustering.rs",
        "Cluster centroid initialization",
    ),
    SeedLabelDefinition::new(
        "counterfactual.replay",
        "src/core/lab.rs:counterfactual_replay",
        "Counterfactual replay child seed",
    ),
    SeedLabelDefinition::new(
        "lab.replay",
        "src/core/lab.rs:replay",
        "Lab replay child seed",
    ),
];

/// N4.3 threading checklist. Each row becomes an implementation row in
/// `tests/determinism_token_threading_unit.rs` when the token is threaded.
pub const THREADING_SURFACES: &[&str] = &[
    "crate::core::search::run_search",
    "crate::core::search::canonicalize_result_ties",
    "crate::core::search::rerank_top_k",
    "crate::core::pack::assemble_pack",
    "crate::core::pack::two_pass_mmr_fill",
    "crate::db::id_gen::next_memory_id",
    "crate::db::id_gen::next_audit_id",
    "crate::db::id_gen::next_workspace_id",
    "crate::core::pack::pack_id",
    "crate::core::audit::ulid::Generator::new",
];

/// Return true when `label` is registered for deterministic child-seed use.
#[must_use]
pub fn is_label_registered(label: &str) -> bool {
    SEED_LABELS.contains(&label)
}

/// Return the registry entry for `label`, if it exists.
#[must_use]
pub fn seed_label_definition(label: &str) -> Option<&'static SeedLabelDefinition> {
    SEED_LABEL_REGISTRY
        .iter()
        .find(|definition| definition.label == label)
}

/// Assert that `label` is registered.
///
/// N4.4's lint can use this as a single runtime contract while compile-time
/// enforcement is built out.
pub fn assert_label_registered(label: &str) {
    assert!(
        is_label_registered(label),
        "unregistered deterministic child-seed label `{label}`"
    );
}
