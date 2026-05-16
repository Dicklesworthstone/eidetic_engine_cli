//! N4.2 tests for the deterministic runtime capability token.

use std::collections::BTreeSet;

use ee::core::determinism::{
    DeterministicTokenShape, SEED_LABEL_REGISTRY, THREADING_SURFACE_REGISTRY, THREADING_SURFACES,
};
use ee::runtime::determinism::{
    DeterminismError, Deterministic, RANDOMNESS_INVENTORY_ROWS_CONTENT_HASH, RandomnessConsumer,
    Seed, SeedSource,
};

type TestResult<T = ()> = Result<T, String>;

const SNAPSHOT: &str = include_str!("snapshots/determinism_token_doctest_output.snap");

fn assert_send<T: Send>() {}

#[test]
fn explicit_seed_construction_records_source_and_scope() {
    let token = Deterministic::from_seed(42);
    assert_eq!(token.seed().as_u64(), 42);
    assert_eq!(token.source(), SeedSource::Explicit);
    assert_eq!(token.scope(), "root");
    assert_eq!(token.seed_hash_prefix().len(), 12);
}

#[test]
fn persistent_timestamp_and_env_seed_sources_are_deterministic() -> TestResult {
    let persistent_a = Deterministic::from_persistent_seed("workspace:/repo/a");
    let persistent_b = Deterministic::from_persistent_seed("workspace:/repo/a");
    assert_eq!(persistent_a.seed(), persistent_b.seed());
    assert_eq!(persistent_a.source(), SeedSource::PersistentWorkspace);

    let timestamp_a = Deterministic::from_timestamp_second("2026-05-13T10:11:12.999Z")
        .map_err(|error| error.to_string())?;
    let timestamp_b = Deterministic::from_timestamp_second("2026-05-13T10:11:12.001Z")
        .map_err(|error| error.to_string())?;
    assert_eq!(
        timestamp_a.seed(),
        timestamp_b.seed(),
        "timestamp seeds truncate to second precision"
    );
    assert_eq!(timestamp_a.source(), SeedSource::TimestampSecond);

    let env_token = Deterministic::from_env_value("12345").map_err(|error| error.to_string())?;
    assert_eq!(env_token.seed().as_u64(), 12345);
    assert_eq!(env_token.source(), SeedSource::Env);

    Ok(())
}

#[test]
fn invalid_env_seed_value_is_reported() {
    match Deterministic::from_env_value("not-a-number") {
        Ok(_) => panic!("invalid seed should fail"),
        Err(error) => assert_eq!(
            error,
            DeterminismError::InvalidSeed {
                value: "not-a-number".to_owned()
            }
        ),
    }
}

#[test]
fn child_split_is_reproducible_and_distinct() {
    let mut parent_a = Deterministic::from_seed(9);
    let child_a_1 = parent_a.child("retrieval");
    let child_a_2 = parent_a.child("retrieval");

    let mut parent_b = Deterministic::from_seed(9);
    let child_b_1 = parent_b.child("retrieval");

    assert_eq!(child_a_1.seed(), child_b_1.seed());
    assert_ne!(
        child_a_1.seed(),
        child_a_2.seed(),
        "same-label children split from one parent still get distinct ordinals"
    );
    assert!(child_a_1.scope().contains("retrieval#0"));
}

#[test]
fn child_split_is_label_keyed_and_parent_replayable() {
    let mut parent_a = Deterministic::from_seed(9);
    let retrieval_a = parent_a.child("retrieval");
    let pack_a = parent_a.child("pack");

    let mut parent_b = Deterministic::from_seed(9);
    let retrieval_b = parent_b.child("retrieval");
    let pack_b = parent_b.child("pack");

    assert_eq!(retrieval_a.seed(), retrieval_b.seed());
    assert_eq!(pack_a.seed(), pack_b.seed());
    assert_ne!(
        retrieval_a.seed(),
        pack_a.seed(),
        "child labels must be part of the derived seed contract"
    );
    assert_eq!(retrieval_a.source(), SeedSource::Child);
    assert_eq!(pack_a.source(), SeedSource::Child);
    assert!(retrieval_a.scope().contains("retrieval#0"));
    assert!(pack_a.scope().contains("pack#1"));
}

#[test]
fn shared_child_does_not_advance_parent_scope() {
    let mut parent = Deterministic::from_seed(9);
    let shared_a = parent.shared_child("pack.mmr_tiebreak");
    let shared_b = parent.shared_child("pack.mmr_tiebreak");

    assert_eq!(
        shared_a.seed(),
        shared_b.seed(),
        "shared child calls replay the same label-derived seed"
    );
    assert_eq!(shared_a.source(), SeedSource::Child);
    assert!(shared_a.scope().contains("pack.mmr_tiebreak"));

    let ordinal_child_after_shared = parent.child("retrieval");
    let mut replay_parent = Deterministic::from_seed(9);
    let first_ordinal_child = replay_parent.child("retrieval");
    assert_eq!(
        ordinal_child_after_shared.seed(),
        first_ordinal_child.seed(),
        "shared child derivation must not consume the parent's ordinal counter"
    );
}

#[test]
fn deterministic_rng_replays_same_bytes() {
    let mut token_a = Deterministic::from_seed(77);
    let mut token_b = Deterministic::from_seed(77);
    let mut bytes_a = [0_u8; 24];
    let mut bytes_b = [0_u8; 24];

    token_a.rng().fill_bytes(&mut bytes_a);
    token_b.rng().fill_bytes(&mut bytes_b);

    assert_eq!(bytes_a, bytes_b);
    assert_ne!(bytes_a, [0_u8; 24]);
}

#[test]
fn deterministic_consumers_are_token_constructed_and_named() {
    let mut token = Deterministic::from_seed(10);
    assert_eq!(token.rng().consumer_kind(), "deterministic_rng");
    assert_eq!(token.clock().consumer_kind(), "deterministic_clock");
    assert_eq!(token.order().consumer_kind(), "deterministic_order");
}

#[test]
fn deterministic_order_sorts_by_stable_key() {
    let mut token = Deterministic::from_seed(12);
    let mut values = vec!["gamma", "alpha", "beta"];

    token.order().sort_by_key(&mut values, |value| *value);

    assert_eq!(values, ["alpha", "beta", "gamma"]);
}

#[test]
fn uuid_v7_clock_is_monotonic_and_replayable() {
    let mut token_a = Deterministic::from_seed(1_000);
    let first = token_a.clock().next_uuid_v7();
    let second = token_a.clock().next_uuid_v7();
    assert!(first < second);

    let mut token_b = Deterministic::from_seed(1_000);
    assert_eq!(first, token_b.clock().next_uuid_v7());
    assert_eq!(second, token_b.clock().next_uuid_v7());
}

#[test]
fn uuid_v7_cross_scope_order_follows_seed_precedence() {
    let mut low_seed = Deterministic::from_seed(1);
    let mut high_seed = Deterministic::from_seed(2);

    let low = low_seed.clock().next_uuid_v7();
    let high = high_seed.clock().next_uuid_v7();

    assert!(low < high);
}

#[test]
fn token_is_send_but_not_sync_by_doctest_contract() {
    assert_send::<Deterministic>();
}

#[test]
fn deterministic_seed_token_compile_time_guards_hold() {
    static_assertions::assert_impl_all!(Deterministic<Seed>: Send);
    static_assertions::assert_not_impl_any!(Deterministic<Seed>: Clone, Copy, Sync);
}

#[test]
fn inventory_hash_is_cited_by_the_module() {
    assert_eq!(
        RANDOMNESS_INVENTORY_ROWS_CONTENT_HASH,
        "blake3-ish:51a8854727a5768008ba8269596e8666cc9ffdd88e8ac3f13101ad36434a3bfc"
    );
}

#[test]
fn deterministic_sequence_snapshot_is_stable() {
    let mut token = Deterministic::from_seed(17);
    let mut child = token.child("pack");
    let uuid = child.clock().next_uuid_v7();
    let first_word = child.rng().next_u64();
    let summary = format!(
        "schema: ee.determinism_token.snapshot.v1\nseed: {}\nsource: {}\nscope: {}\nseed_hash_prefix: {}\nfirst_uuid_v7: {}\nfirst_rng_u64: {}\ninventory_hash: {}\n",
        child.seed().as_u64(),
        child.source().as_str(),
        child.scope(),
        child.seed_hash_prefix(),
        uuid,
        first_word,
        RANDOMNESS_INVENTORY_ROWS_CONTENT_HASH
    );

    assert_eq!(summary, SNAPSHOT);
}

fn child_thread_summary(label: &'static str, mut token: Deterministic<Seed>) -> String {
    let first_uuid = token.clock().next_uuid_v7();
    let first_word = token.rng().next_u64();
    format!(
        "{label}:{scope}:{seed}:{uuid}:{word}",
        scope = token.scope(),
        seed = token.seed().as_u64(),
        uuid = first_uuid,
        word = first_word
    )
}

fn concurrent_child_summaries() -> TestResult<Vec<String>> {
    let mut parent = Deterministic::from_seed(404);
    let left = parent.child("thread.left");
    let right = parent.child("thread.right");

    let left_handle = std::thread::spawn(move || child_thread_summary("left", left));
    let right_handle = std::thread::spawn(move || child_thread_summary("right", right));

    let mut summaries = vec![
        left_handle
            .join()
            .map_err(|_| "left deterministic child thread panicked".to_owned())?,
        right_handle
            .join()
            .map_err(|_| "right deterministic child thread panicked".to_owned())?,
    ];
    summaries.sort();
    Ok(summaries)
}

#[test]
fn child_tokens_can_be_consumed_concurrently_without_shared_parent() -> TestResult {
    let first = concurrent_child_summaries()?;
    let second = concurrent_child_summaries()?;

    assert_eq!(first, second);
    assert_eq!(first.len(), 2);
    assert_ne!(first[0], first[1]);

    Ok(())
}

#[test]
fn n4_3_threading_surface_checklist_is_executable() -> TestResult {
    let expected = [
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

    if THREADING_SURFACES != expected.as_slice() {
        return Err(format!(
            "N4.3 threading checklist drifted.\nexpected: {expected:?}\nactual:   {THREADING_SURFACES:?}",
        ));
    }

    let unique = THREADING_SURFACES.iter().copied().collect::<BTreeSet<_>>();
    if unique.len() != THREADING_SURFACES.len() {
        return Err(format!(
            "N4.3 threading checklist contains duplicates: {THREADING_SURFACES:?}",
        ));
    }
    if THREADING_SURFACES
        .iter()
        .any(|surface| !surface.starts_with("crate::") || !surface.contains("::"))
    {
        return Err(format!(
            "N4.3 threading checklist entries must be fully-qualified Rust paths: {THREADING_SURFACES:?}",
        ));
    }

    Ok(())
}

#[test]
fn n4_3_threading_surface_token_shapes_are_executable() -> TestResult {
    let expected = [
        (
            "crate::core::search::run_search",
            DeterministicTokenShape::Shared,
        ),
        (
            "crate::core::search::canonicalize_equivalent_component_scores",
            DeterministicTokenShape::Shared,
        ),
        (
            "crate::core::search::search_sync",
            DeterministicTokenShape::Shared,
        ),
        (
            "crate::pack::assemble_draft_with_profile_and_options",
            DeterministicTokenShape::Shared,
        ),
        (
            "crate::pack::assemble_mmr_draft",
            DeterministicTokenShape::Shared,
        ),
        ("crate::models::Id::now", DeterministicTokenShape::Mutable),
        (
            "crate::db::generate_audit_id",
            DeterministicTokenShape::Mutable,
        ),
        (
            "crate::core::workspace::stable_workspace_id",
            DeterministicTokenShape::Mutable,
        ),
        (
            "crate::core::context::persist_pack_record",
            DeterministicTokenShape::Shared,
        ),
        (
            "crate::runtime::determinism::DeterministicClock::next_uuid_v7",
            DeterministicTokenShape::Mutable,
        ),
    ];

    if THREADING_SURFACE_REGISTRY.len() != expected.len() {
        return Err(format!(
            "N4.3 threading shape registry length drifted. expected {} rows, got {}",
            expected.len(),
            THREADING_SURFACE_REGISTRY.len()
        ));
    }

    for (definition, (surface, token_shape)) in THREADING_SURFACE_REGISTRY.iter().zip(expected) {
        if definition.surface != surface || definition.token_shape != token_shape {
            return Err(format!(
                "N4.3 threading shape drifted for `{surface}`. expected `{}`, got `{}:{}`",
                token_shape.as_str(),
                definition.surface,
                definition.token_shape.as_str()
            ));
        }
    }

    let registry_surfaces = THREADING_SURFACE_REGISTRY
        .iter()
        .map(|definition| definition.surface)
        .collect::<Vec<_>>();
    if registry_surfaces != THREADING_SURFACES {
        return Err(format!(
            "N4.3 threading surfaces and shape registry diverged.\nsurfaces: {THREADING_SURFACES:?}\nregistry: {registry_surfaces:?}",
        ));
    }

    let shared_count = THREADING_SURFACE_REGISTRY
        .iter()
        .filter(|definition| definition.token_shape == DeterministicTokenShape::Shared)
        .count();
    let mutable_count = THREADING_SURFACE_REGISTRY
        .iter()
        .filter(|definition| definition.token_shape == DeterministicTokenShape::Mutable)
        .count();
    if shared_count != 6 || mutable_count != 4 {
        return Err(format!(
            "N4.3 token shape split drifted. expected shared=6 mutable=4, got shared={shared_count} mutable={mutable_count}",
        ));
    }

    Ok(())
}

#[test]
fn n4_3_threading_surfaces_have_seed_label_registry_rows() -> TestResult {
    for (call_site, label_fragment) in [
        ("src/core/search.rs:run_search", "search."),
        (
            "src/core/search.rs:canonicalize_equivalent_component_scores",
            "search.canonical_ties",
        ),
        ("src/core/search.rs:search_sync", "search.rerank"),
        ("src/pack/mod.rs:assemble_mmr_draft", "pack.mmr_tiebreak"),
        ("src/models/id.rs:Id::now", "ulid.memory"),
        ("src/db/mod.rs:generate_audit_id", "ulid.audit"),
        (
            "src/core/workspace.rs:stable_workspace_id",
            "ulid.workspace",
        ),
        ("src/core/context.rs:persist_pack_record", "ulid.pack"),
    ] {
        let found = SEED_LABEL_REGISTRY.iter().any(|definition| {
            definition.producer_call_site == call_site && definition.label.contains(label_fragment)
        });
        if !found {
            return Err(format!(
                "N4.3 threading surface `{call_site}` lacks a matching seed label registry row containing `{label_fragment}`",
            ));
        }
    }

    Ok(())
}

#[test]
fn n4_3_threading_surfaces_point_at_real_source_symbols() -> TestResult {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

    for surface in THREADING_SURFACES {
        let (file, symbol) = threading_surface_source_site(surface)?;
        let path = manifest_dir.join(&file);
        let source = std::fs::read_to_string(&path).map_err(|error| {
            format!("N4.3 threading surface `{surface}` points at unreadable `{file}`: {error}")
        })?;
        if !source.contains(&symbol) {
            return Err(format!(
                "N4.3 threading surface `{surface}` points at `{file}` but symbol `{symbol}` was not found",
            ));
        }
    }

    Ok(())
}

fn threading_surface_source_site(surface: &str) -> Result<(String, String), String> {
    let surface = surface
        .strip_prefix("crate::")
        .ok_or_else(|| format!("N4.3 threading surface `{surface}` must start with `crate::`"))?;
    let parts = surface.split("::").collect::<Vec<_>>();
    let symbol = parts
        .last()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| format!("N4.3 threading surface `{surface}` has no symbol"))?
        .to_string();
    let file = match parts.as_slice() {
        ["core", module, ..] => format!("src/core/{module}.rs"),
        ["db", ..] => "src/db/mod.rs".to_owned(),
        ["models", "Id", ..] => "src/models/id.rs".to_owned(),
        ["pack", ..] => "src/pack/mod.rs".to_owned(),
        ["runtime", "determinism", ..] => "src/runtime/determinism.rs".to_owned(),
        _ => {
            return Err(format!(
                "N4.3 threading surface `crate::{surface}` has no source-file mapping",
            ));
        }
    };
    Ok((file, symbol))
}

#[test]
fn seed_label_registry_points_at_real_source_files() -> TestResult {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

    for definition in SEED_LABEL_REGISTRY {
        let (file, symbol) = definition
            .producer_call_site
            .split_once(':')
            .ok_or_else(|| {
                format!(
                    "seed label `{}` has invalid producer call site `{}`",
                    definition.label, definition.producer_call_site
                )
            })?;
        let path = manifest_dir.join(file);
        let source = std::fs::read_to_string(&path).map_err(|error| {
            format!(
                "seed label `{}` points at unreadable source file `{}`: {error}",
                definition.label,
                path.display()
            )
        })?;
        let symbol_name = symbol.rsplit("::").next().unwrap_or(symbol);
        if !source.contains(symbol_name) {
            return Err(format!(
                "seed label `{}` producer call site `{}` does not mention symbol `{symbol_name}`",
                definition.label, definition.producer_call_site
            ));
        }
    }

    Ok(())
}
