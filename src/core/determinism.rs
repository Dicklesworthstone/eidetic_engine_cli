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
        "src/pack/mod.rs:assemble_mmr_draft",
        "MMR diversity tie-breaks during pack assembly",
    ),
    SeedLabelDefinition::new(
        "pack.selection_id",
        "src/core/context.rs:persist_pack_record",
        "Context pack selection and persisted pack IDs",
    ),
    SeedLabelDefinition::new(
        "pack.skipped_order",
        "src/pack/mod.rs:PackDraft::skipped_for_output",
        "Stable ordering for omitted or skipped candidates",
    ),
    SeedLabelDefinition::new(
        "search.score_jitter",
        "src/core/search.rs:run_search",
        "Deterministic score perturbation for stability tests",
    ),
    SeedLabelDefinition::new(
        "search.canonical_ties",
        "src/core/search.rs:canonicalize_equivalent_component_scores",
        "Stable ordering for equal-score search results",
    ),
    SeedLabelDefinition::new(
        "search.rerank",
        "src/core/search.rs:search_sync",
        "Rerank-stage deterministic tie-breaks",
    ),
    SeedLabelDefinition::new(
        "ulid.memory",
        "src/models/id.rs:Id::now_seeded",
        "Memory UUIDv7 generation",
    ),
    SeedLabelDefinition::new(
        "ulid.audit",
        "src/db/mod.rs:generate_audit_id_seeded",
        "Audit UUIDv7 generation",
    ),
    SeedLabelDefinition::new(
        "ulid.workspace",
        "src/core/workspace.rs:stable_workspace_id_seeded",
        "Workspace UUIDv7 generation",
    ),
    SeedLabelDefinition::new(
        "ulid.pack",
        "src/core/context.rs:persist_pack_record_seeded",
        "Context pack UUIDv7 generation",
    ),
    SeedLabelDefinition::new(
        "clustering.kmeans_init",
        "src/curate/cluster_coherence.rs:centroid_hash",
        "Cluster centroid hash derivation",
    ),
    SeedLabelDefinition::new(
        "counterfactual.replay",
        "src/core/lab.rs:run_counterfactual",
        "Counterfactual replay child seed",
    ),
    SeedLabelDefinition::new(
        "lab.replay",
        "src/core/lab.rs:replay_episode",
        "Lab replay child seed",
    ),
];

/// Required token shape for an N4.3 threading surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeterministicTokenShape {
    /// The callee may inspect or fork deterministic state, but must not advance
    /// the caller's root token.
    Shared,
    /// The callee advances deterministic state and must own mutable access.
    Mutable,
}

impl DeterministicTokenShape {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "&Deterministic<Seed>",
            Self::Mutable => "&mut Deterministic<Seed>",
        }
    }
}

/// Machine-checkable row for the N4.3 threading checklist.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ThreadingSurfaceDefinition {
    pub surface: &'static str,
    pub token_shape: DeterministicTokenShape,
}

impl ThreadingSurfaceDefinition {
    #[must_use]
    pub const fn new(surface: &'static str, token_shape: DeterministicTokenShape) -> Self {
        Self {
            surface,
            token_shape,
        }
    }
}

/// N4.3 threading checklist.
///
/// Each row becomes an implementation row in
/// `tests/determinism_token_threading_unit.rs` when the token is threaded.
///
/// | Row | Surface | Token shape |
/// | --- | --- | --- |
/// | 1 | `crate::core::search::run_search` | `&Deterministic<Seed>` |
/// | 2 | `crate::core::search::canonicalize_equivalent_component_scores` | `&Deterministic<Seed>` |
/// | 3 | `crate::core::search::search_sync` | `&Deterministic<Seed>` |
/// | 4 | `crate::pack::assemble_draft_with_profile_and_options` | `&Deterministic<Seed>` |
/// | 5 | `crate::pack::assemble_mmr_draft` | `&Deterministic<Seed>` |
/// | 6 | `crate::models::Id::now` | `&mut Deterministic<Seed>` |
/// | 7 | `crate::db::generate_audit_id` | `&mut Deterministic<Seed>` |
/// | 8 | `crate::core::workspace::stable_workspace_id` | `&mut Deterministic<Seed>` |
/// | 9 | `crate::core::context::persist_pack_record` | `&Deterministic<Seed>` |
/// | 10 | `crate::runtime::determinism::DeterministicClock::next_uuid_v7` | `&mut Deterministic<Seed>` |
pub const THREADING_SURFACES: &[&str] = &[
    "crate::core::search::run_search",
    "crate::core::search::canonicalize_equivalent_component_scores",
    "crate::core::search::search_sync",
    "crate::pack::assemble_draft_with_profile_and_options",
    "crate::pack::assemble_mmr_draft",
    "crate::models::Id::now",
    "crate::db::generate_audit_id",
    "crate::core::workspace::stable_workspace_id",
    "crate::core::context::persist_pack_record",
    "crate::runtime::determinism::DeterministicClock::next_uuid_v7",
];

/// N4.3 threading checklist with the required token shape for each row.
pub const THREADING_SURFACE_REGISTRY: &[ThreadingSurfaceDefinition] = &[
    ThreadingSurfaceDefinition::new(
        "crate::core::search::run_search",
        DeterministicTokenShape::Shared,
    ),
    ThreadingSurfaceDefinition::new(
        "crate::core::search::canonicalize_equivalent_component_scores",
        DeterministicTokenShape::Shared,
    ),
    ThreadingSurfaceDefinition::new(
        "crate::core::search::search_sync",
        DeterministicTokenShape::Shared,
    ),
    ThreadingSurfaceDefinition::new(
        "crate::pack::assemble_draft_with_profile_and_options",
        DeterministicTokenShape::Shared,
    ),
    ThreadingSurfaceDefinition::new(
        "crate::pack::assemble_mmr_draft",
        DeterministicTokenShape::Shared,
    ),
    ThreadingSurfaceDefinition::new("crate::models::Id::now", DeterministicTokenShape::Mutable),
    ThreadingSurfaceDefinition::new(
        "crate::db::generate_audit_id",
        DeterministicTokenShape::Mutable,
    ),
    ThreadingSurfaceDefinition::new(
        "crate::core::workspace::stable_workspace_id",
        DeterministicTokenShape::Mutable,
    ),
    ThreadingSurfaceDefinition::new(
        "crate::core::context::persist_pack_record",
        DeterministicTokenShape::Shared,
    ),
    ThreadingSurfaceDefinition::new(
        "crate::runtime::determinism::DeterministicClock::next_uuid_v7",
        DeterministicTokenShape::Mutable,
    ),
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
