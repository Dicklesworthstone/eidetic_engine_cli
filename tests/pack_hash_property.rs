//! ADR 0087 v2 pack-hash properties (bd-reality-core-convergence-1azkt.1).
//!
//! Every property here reads the product's own output as it is. There is no
//! test-side normalizer: a comparison that scrubbed timing first could not
//! tell whether the product itself kept timing out of canonical data.
//!
//! Filter with `cargo test --test integration_property pack_hash_property::`.

use std::str::FromStr;

use ee::core::context::compute_pack_hash;
use ee::models::{MemoryId, ProvenanceUri, TrustClass, UnitScore};
use ee::output::render_context_response_json;
use ee::pack::{
    ContextRequest, ContextResponse, ContextResponseDegradation, ContextResponseSeverity,
    PackAssemblySlo, PackAssemblySloActuals, PackCandidate, PackCandidateInput, PackDraft,
    PackProvenance, PackResourceProfile, PackSection, PackTrustSignal, TokenBudget, assemble_draft,
};
use proptest::prelude::*;
use uuid::Uuid;

const QUERY: &str = "pack hash v2 property";

/// The v2 `pack.hash` of [`pinned_fixture`], identical on every declared
/// target. A change here is a deliberate `snapshotIdentity.version` bump
/// (ADR 0087 §8), never a re-pin to make a run green.
const PINNED_V2_PACK_HASH: &str =
    "blake3:0c0583d78304d331f6ed2db2591101037cad73a4f02e3f40ab7299b63fd58157";

fn fixture(relevance: f32) -> Result<(ContextRequest, PackDraft), String> {
    let request = ContextRequest::from_query(QUERY).map_err(|error| error.to_string())?;
    let memory_id = MemoryId::from_uuid(Uuid::from_u128(0x0087));
    let candidate = PackCandidate::new(PackCandidateInput {
        memory_id,
        section: PackSection::ProceduralRules,
        content: "Hash every field with a label and a length.".to_owned(),
        estimated_tokens: 9,
        relevance: UnitScore::parse(relevance).map_err(|error| error.to_string())?,
        utility: UnitScore::parse(0.75).map_err(|error| error.to_string())?,
        provenance: vec![
            PackProvenance::new(
                ProvenanceUri::from_str("file://docs/adr/0087.md#L1-10")
                    .map_err(|error| error.to_string())?,
                "ADR 0087 fixture",
            )
            .map_err(|error| error.to_string())?,
        ],
        why: "selected for the pack-hash v2 property".to_owned(),
    })
    .map_err(|error| error.to_string())?
    .with_trust_signal(PackTrustSignal::new(TrustClass::HumanExplicit, None));
    let draft = assemble_draft(QUERY, TokenBudget::default_context(), [candidate])
        .map_err(|error| error.to_string())?;
    Ok((request, draft))
}

fn pinned_fixture() -> Result<(ContextRequest, PackDraft), String> {
    fixture(0.9)
}

fn degradation(
    code: &str,
    severity: ContextResponseSeverity,
    message: &str,
    repair: Option<&str>,
) -> Result<ContextResponseDegradation, String> {
    ContextResponseDegradation::new(code, severity, message, repair.map(str::to_owned))
        .map_err(|error| error.to_string())
}

fn canonical_degraded() -> Result<Vec<ContextResponseDegradation>, String> {
    Ok(vec![
        degradation(
            "search_index_stale",
            ContextResponseSeverity::Medium,
            "Search index is stale.",
            Some("ee index rebuild --workspace ."),
        )?,
        degradation(
            "low_recall_after_floor",
            ContextResponseSeverity::Low,
            "Only one candidate passed the relevance floor.",
            None,
        )?,
        degradation(
            "semantic_embedding_unavailable",
            ContextResponseSeverity::Medium,
            "Semantic similarity is disabled; lexical search remains available.",
            Some("ee index reembed --workspace ."),
        )?,
    ])
}

/// The product's own timing entry for one elapsed reading, exactly as
/// `PackAssemblySlo::timing_degradations` emits it.
fn timing_degradations(
    profile: PackResourceProfile,
    elapsed_ms: u64,
) -> Vec<ContextResponseDegradation> {
    PackAssemblySlo::evaluate(
        profile,
        PackAssemblySloActuals {
            candidate_count: 1,
            scanned_count: 1,
            index_generation: Some(1),
            graph_generation: Some(1),
            graph_edges_traversed: 0,
            elapsed_ms,
            memory_bytes_peak: 4_096,
        },
    )
    .timing_degradations()
}

fn resource_profiles() -> impl Strategy<Value = PackResourceProfile> {
    prop_oneof![
        Just(PackResourceProfile::Lean),
        Just(PackResourceProfile::Standard),
        Just(PackResourceProfile::SwarmHeavy),
    ]
}

/// The pinned digest vector: one fixed input, one fixed v2 hash. Run on two
/// RCH workers, this is the cross-host half of ADR 0087 §9.
#[test]
fn pinned_v2_pack_hash_digest_vector() -> Result<(), String> {
    let (request, draft) = pinned_fixture()?;
    let hash = compute_pack_hash(&request, &draft, &canonical_degraded()?);
    println!("pack_hash_property pinned_v2_pack_hash={hash}");
    if hash != PINNED_V2_PACK_HASH {
        return Err(format!(
            "v2 pack hash of the pinned fixture is {hash}, pinned {PINNED_V2_PACK_HASH}"
        ));
    }
    Ok(())
}

/// Guard for the timing property: the fixture must actually produce a timing
/// entry at a large elapsed reading, or "timing never moves the hash" would
/// pass on an empty world.
#[test]
fn timing_fixture_emits_a_timing_entry_when_slow() {
    for profile in [
        PackResourceProfile::Lean,
        PackResourceProfile::Standard,
        PackResourceProfile::SwarmHeavy,
    ] {
        let timing = timing_degradations(profile, 600_000);
        assert_eq!(
            timing.len(),
            1,
            "{profile:?}: a 600 s pack must emit one timing entry"
        );
        assert_eq!(
            timing[0].code,
            ee::pack::PACK_ASSEMBLY_ELAPSED_OVER_BUDGET_CODE,
            "{profile:?}: the timing entry must carry the telemetry code"
        );
    }
}

/// The shipped JSON envelope of one pack, hashed the way the product hashes
/// it, with `extra` appended to the response degradations.
fn shipped_envelope(extra: Vec<ContextResponseDegradation>) -> Result<serde_json::Value, String> {
    let (request, mut draft) = fixture(0.9)?;
    let mut degraded = canonical_degraded()?;
    draft.hash = Some(compute_pack_hash(&request, &draft, &degraded));
    degraded.extend(extra);
    let response =
        ContextResponse::new(request, draft, degraded).map_err(|error| error.to_string())?;
    serde_json::from_str(&render_context_response_json(&response))
        .map_err(|error| error.to_string())
}

fn shipped_text(envelope: &serde_json::Value) -> Result<String, String> {
    envelope
        .pointer("/data/pack/text")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "shipped envelope has no data.pack.text".to_owned())
}

/// Guard for the text property: the timing entry must still reach the
/// envelope's `degraded[]` as telemetry, or "timing never moves pack.text"
/// could pass because timing was dropped everywhere.
#[test]
fn timing_stays_visible_as_telemetry_in_the_envelope() -> Result<(), String> {
    let envelope = shipped_envelope(timing_degradations(PackResourceProfile::Lean, 600_000))?;
    let carries_timing = envelope
        .pointer("/degraded")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry.get("code").and_then(serde_json::Value::as_str)
                    == Some(ee::pack::PACK_ASSEMBLY_ELAPSED_OVER_BUDGET_CODE)
            })
        });
    if !carries_timing {
        return Err("the timing entry must stay in the envelope's degraded[]".to_owned());
    }
    Ok(())
}

/// The N in the shipped text's advisory sentence. The renderer words it two
/// ways ("Context includes N degraded signal(s)" when semantic embedding is
/// the operative degradation, "This pack invocation had N degraded retrieval
/// signal(s)" otherwise); both put the count right before the noun.
fn text_degraded_signal_count(text: &str) -> Result<usize, String> {
    for noun in [" degraded signal", " degraded retrieval signal"] {
        let Some(at) = text.find(noun) else {
            continue;
        };
        let digits: String = text[..at]
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if !digits.is_empty() {
            return digits
                .parse()
                .map_err(|error| format!("{error}: {digits:?}"));
        }
    }
    Err(format!(
        "shipped text has no degraded-signal sentence:\n{text}"
    ))
}

/// ADR 0087 T2 banner ruling (2026-09-25): the JSON banner is a per-run report
/// and counts every entry in `degraded[]`; canonical `pack.text` omits the
/// volatile timing entry. With a forced overrun the two differ by exactly the
/// volatile entries, so this fails if either side changes silently.
#[test]
fn shipped_text_and_banner_disagree_by_exactly_the_volatile_entries() -> Result<(), String> {
    let envelope = shipped_envelope(timing_degradations(PackResourceProfile::Lean, 600_000))?;
    let degraded = envelope
        .pointer("/data/degraded")
        .and_then(serde_json::Value::as_array)
        .ok_or("shipped envelope has no data.degraded")?;
    let volatile = degraded
        .iter()
        .filter(|entry| {
            entry.get("code").and_then(serde_json::Value::as_str)
                == Some(ee::pack::PACK_ASSEMBLY_ELAPSED_OVER_BUDGET_CODE)
        })
        .count();
    let banner = envelope
        .pointer("/data/pack/advisoryBanner/degradationCount")
        .and_then(serde_json::Value::as_u64)
        .ok_or("shipped envelope has no advisoryBanner.degradationCount")?;
    let banner = usize::try_from(banner).map_err(|error| error.to_string())?;
    let text_count = text_degraded_signal_count(&shipped_text(&envelope)?)?;
    println!(
        "degraded[] {} (volatile {volatile}), banner {banner}, pack.text {text_count}",
        degraded.len()
    );
    if volatile == 0 {
        return Err("the forced overrun must put a timing entry in degraded[]".to_owned());
    }
    if banner != degraded.len() {
        return Err(format!(
            "the banner must count every degraded[] entry: banner {banner}, degraded[] {}",
            degraded.len()
        ));
    }
    if text_count != banner - volatile {
        return Err(format!(
            "pack.text must count the banner minus the volatile entries: text {text_count}, \
             banner {banner}, volatile {volatile}"
        ));
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// ADR 0087 §1 (T2): `pack.text` is canonical. No elapsed reading, on any
    /// resource profile, changes one byte of the shipped text, its degraded
    /// counts or its banner. Compared raw: no normalizer.
    #[test]
    fn timing_never_moves_shipped_pack_text(
        profile in resource_profiles(),
        elapsed_ms in 0_u64..600_000,
    ) {
        let quiet = shipped_envelope(Vec::new()).map_err(TestCaseError::fail)?;
        let timed = shipped_envelope(timing_degradations(profile, elapsed_ms))
            .map_err(TestCaseError::fail)?;
        prop_assert_eq!(
            shipped_text(&quiet).map_err(TestCaseError::fail)?,
            shipped_text(&timed).map_err(TestCaseError::fail)?
        );
    }

    /// ADR 0087 §5: no elapsed reading, on any resource profile, and at any
    /// position in the slice, moves `pack.hash`.
    #[test]
    fn timing_never_moves_pack_hash(
        profile in resource_profiles(),
        elapsed_ms in 0_u64..600_000,
        position in 0_usize..4,
    ) {
        let (request, draft) = fixture(0.9).map_err(TestCaseError::fail)?;
        let canonical = canonical_degraded().map_err(TestCaseError::fail)?;
        let mut with_timing = canonical.clone();
        for entry in timing_degradations(profile, elapsed_ms) {
            with_timing.insert(position.min(with_timing.len()), entry);
        }
        prop_assert_eq!(
            compute_pack_hash(&request, &draft, &canonical),
            compute_pack_hash(&request, &draft, &with_timing)
        );
    }

    /// ADR 0087 §4: degraded order and repetition are presentation.
    #[test]
    fn degraded_order_and_repetition_never_move_pack_hash(
        order in Just(vec![0_usize, 1, 2]).prop_shuffle(),
        repeat in 0_usize..3,
    ) {
        let (request, draft) = fixture(0.9).map_err(TestCaseError::fail)?;
        let canonical = canonical_degraded().map_err(TestCaseError::fail)?;
        let mut shuffled: Vec<ContextResponseDegradation> =
            order.iter().map(|index| canonical[*index].clone()).collect();
        shuffled.push(canonical[repeat].clone());
        prop_assert_eq!(
            compute_pack_hash(&request, &draft, &canonical),
            compute_pack_hash(&request, &draft, &shuffled)
        );
    }

    /// ADR 0087 §3: score noise inside one Q20.12 quantum never moves
    /// `pack.hash`, and a one-quantum step always does. Scores stay at or
    /// above 0.1, clear of DEFAULT_COVERAGE_FILL_RELEVANCE_FLOOR (0.05), so
    /// the item is selected in every case.
    #[test]
    fn score_noise_below_the_quantum_never_moves_pack_hash(
        quantum in 410_u32..4_000,
        noise in -0.4_f32..0.4,
    ) {
        let scale = 4096.0_f32;
        let exact = f32::from(u16::try_from(quantum).map_err(|error| TestCaseError::fail(error.to_string()))?) / scale;
        let noisy = exact + noise / scale;
        let stepped = exact + 1.0 / scale;
        let hash = |relevance: f32| -> Result<String, TestCaseError> {
            let (request, draft) = fixture(relevance).map_err(TestCaseError::fail)?;
            Ok(compute_pack_hash(&request, &draft, &[]))
        };
        prop_assert_eq!(hash(exact)?, hash(noisy)?);
        prop_assert_ne!(hash(exact)?, hash(stepped)?);
    }
}
