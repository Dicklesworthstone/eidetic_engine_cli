//! Tracked-red pins for the lexical relevance contract
//! (bd-reality-core-convergence-1azkt.11).
//!
//! WHY THIS IS ITS OWN TARGET, AND WHY EVERY TEST IS `#[ignore]`
//!
//! These tests are EXPECTED TO FAIL against the current product. They exist to
//! document two live defects with assertions that actually fail on them, so the
//! repair has a pre-existing red to be measured against. A repair that lands
//! without one changed a label and a large golden set in a single motion, with
//! nothing that failed before and passes after.
//!
//! `scripts/verify.sh` runs the required stage
//! `cargo test --workspace --lib --bins --tests --examples`, which builds and
//! runs every `[[test]]` target. A red here would therefore redden a REQUIRED
//! stage that covers hundreds of unrelated tests, and — because a multi-target
//! `cargo test` stops at the first failing target — would destroy the verdicts
//! of everything scheduled behind it. `#[ignore]` keeps that stage honest: this
//! target still compiles on every run (so the pins cannot rot) and reports
//! `2 ignored`.
//!
//! They are executed by `scripts/lexical_relevance_pins.sh`, which runs the
//! binary Gate 5 just built instead of re-entering cargo — measured on RCH hz4
//! at 82926d4c5, these two pins run in 0.00s while the cargo wrapper around
//! them cost 74s re-walking freshness over crates Gate 5 had already walked.
//! That script is wired as two stages: a REQUIRED harness guard, and the
//! tracked-red run arm that `scripts/verify-budget.toml` declares
//! `requirement = "tracked_red"` against this bead. The guard exists because
//! `run_stage` excuses ANY nonzero exit from a tracked-red stage, so a missing
//! or stale binary would otherwise be recorded as the known red and be
//! indistinguishable from it. A red nobody declared is indistinguishable from a
//! regression; a red that is actually an infrastructure failure is worse.
//!
//! WHAT EACH PIN ASSERTS, AND WHAT IS EXPECTED TO CLEAR IT
//!
//! The defect this bead names is a LABEL disagreeing with its CAUSE, so each
//! pin asserts the raw BM25 value beside the rendered one. A pin that asserted
//! only the rendered `1.0` would test the symptom; asserting `1.59`-raw beside
//! `1.0`-rendered pins the RELATIONSHIP, which is the thing any repair has to
//! preserve.
//!
//! * `lexical_score_kind_names_the_projection_that_produced_it` WAS the red
//!   this file was written for, and the rename that clears it has now landed:
//!   `ScoreSource::Lexical`'s score kind is `query_relative_pool_minmax`, not
//!   `unit_normalized`. It is now a REGRESSION GUARD rather than a documented
//!   defect — it holds the tag to a name that describes its cause. Both words
//!   carry weight: `query_relative` says the value is not comparable across
//!   queries, and `pool` names the DENOMINATOR, since min-max over the
//!   retrieved pool is a different number from min-max over the corpus and
//!   `query_relative` alone is silent about which.
//! * `lexical_admission_does_not_invert_on_an_unrelated_documents_score` is
//!   NOT expected to go green on that rename. It owns acceptance bullet 4
//!   ("relevance-floor admission operates in the correct source domain") and
//!   stays red until admission stops being decided in the query-relative
//!   domain. It is filed here rather than deferred so the two defects are
//!   separately observable in one run.
//!
//! Every assertion states the value it EXPECTS rather than the value it
//! rejects: `assert_eq!(kind, "query_relative_pool_minmax")` fails when the field is
//! absent or renamed to a third thing, where `assert_ne!(kind, ...)` would be
//! satisfied by absence.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test code may unwrap/expect (matches lib.rs cfg_attr policy)

use ee::core::search::{
    FrankensearchFinalScoreScale, SearchHit, search_hit_meets_relevance_floor,
    search_hits_from_scored_results,
};
use ee::search::{ScoreSource as FrankensearchScoreSource, ScoredResult};

/// The score kind the lexical projection's CAUSE would justify.
///
/// `search_hits_from_scored_results` min-max normalizes the complete returned
/// lexical pool against a synthetic zero-evidence reference. That is a
/// query-relative projection over one result set: it is not comparable across
/// queries, and it is not comparable against the absolute cosine domain the
/// semantic sources report on.
const EXPECTED_LEXICAL_SCORE_KIND: &str = "query_relative_pool_minmax";

fn lexical_result(doc_id: &str, raw_bm25: f32) -> ScoredResult {
    ScoredResult {
        doc_id: doc_id.to_string().into(),
        score: raw_bm25,
        source: FrankensearchScoreSource::Lexical,
        index: None,
        fast_score: None,
        quality_score: None,
        lexical_score: Some(raw_bm25),
        rerank_score: None,
        explanation: None,
        metadata: None,
    }
}

/// Run one complete pure-lexical pool through the real adapter projection.
fn project_pool(pool: &[(&str, f32)]) -> Vec<SearchHit> {
    assert!(
        !pool.is_empty(),
        "empty-world guard: a pool with no members would let every per-hit \
         assertion below pass vacuously"
    );
    let results = pool
        .iter()
        .map(|(doc_id, raw)| lexical_result(doc_id, *raw))
        .collect();
    let hits =
        search_hits_from_scored_results(results, false, FrankensearchFinalScoreScale::Native);
    assert_eq!(
        hits.len(),
        pool.len(),
        "the projection must return one hit per pooled result"
    );
    hits
}

fn hit_for<'a>(hits: &'a [SearchHit], doc_id: &str) -> &'a SearchHit {
    hits.iter()
        .find(|hit| hit.doc_id == doc_id)
        .unwrap_or_else(|| panic!("projection dropped {doc_id}"))
}

/// The rendered relevance of a lexical pool's top hit is `1.0` whatever the raw
/// BM25 behind it was, and the score kind must say so.
///
/// bd-reality-core-convergence-1azkt.11 was filed against live scores of 1.59
/// through 3.12 all rendering `relevanceScore` 1.0. The raw pass-through that
/// caused it is gone, but min-max normalization maps the pool MAXIMUM to
/// exactly 1.0 by construction, so the same three raw values still render 1.0 —
/// and so does a pool whose only member scored 0.001. The remaining defect is
/// that `scoreKind` called that value `unit_normalized`, a claim about the
/// output. That tag is now `query_relative_pool_minmax`; this pin holds it there. `docs/schemas/ee.search.document.v1.json` used to compound it by
/// telling an agent to "use relevanceScore for cross-source relevance"; that
/// sentence was corrected in the same commit as this pin, which leaves the
/// machine-readable tag as the last place the label still overstates the thing.
#[test]
#[ignore = "regression guard, bd-reality-core-convergence-1azkt.11: lexical scoreKind must stay query_relative_pool_minmax, naming the projection that produced the value rather than asserting a property of it"]
fn lexical_score_kind_names_the_projection_that_produced_it() {
    // The bead's own field numbers, plus a raw value three orders of magnitude
    // weaker, each alone in its pool.
    for raw_bm25 in [1.59_f32, 3.12, 0.001] {
        let hits = project_pool(&[("mem_singleton", raw_bm25)]);
        let hit = hit_for(&hits, "mem_singleton");

        // The CAUSE: the raw engine value is preserved, and it differs by three
        // orders of magnitude across these iterations.
        assert_eq!(
            hit.lexical_score,
            Some(raw_bm25),
            "raw BM25 must survive the projection in lexicalScore"
        );

        // The RENDERED value: identical for every raw magnitude above.
        assert_eq!(
            hit.relevance_score(),
            1.0,
            "raw BM25 {raw_bm25} renders relevanceScore 1.0; this is the \
             relationship the label has to describe honestly, not a value to fix"
        );

        // The LABEL, asserted beside the cause it claims to describe.
        assert_eq!(
            hit.score_kind(),
            EXPECTED_LEXICAL_SCORE_KIND,
            "raw BM25 {raw_bm25} rendered as relevanceScore {rendered} under \
             scoreKind {actual:?}. {actual:?} is a claim about the output; the \
             cause is min-max over the returned pool, which is neither \
             cross-query nor cross-source comparable, so the tag must name that \
             cause: {EXPECTED_LEXICAL_SCORE_KIND:?}",
            rendered = hit.relevance_score(),
            actual = hit.score_kind(),
        );
    }
}

/// Identical lexical evidence must not change admission because some unrelated
/// document scored higher.
///
/// `search_hit_meets_relevance_floor` applies one floor
/// (`DEFAULT_RELEVANCE_FLOOR`, 0.05) to `relevance_score()` for every source.
/// For `Lexical` that value is the hit's share of its own pool's maximum, so a
/// document with fixed raw evidence is admitted or dropped according to what
/// ELSE the query returned. The same floor is applied to the semantic sources'
/// absolute cosine values, which is acceptance bullet 4: cross-source values
/// compared as one scale without explicit calibration.
#[test]
#[ignore = "tracked red, bd-reality-core-convergence-1azkt.11: relevance-floor admission for Lexical is decided in the query-relative domain, so an unrelated document's score flips it"]
fn lexical_admission_does_not_invert_on_an_unrelated_documents_score() {
    const RAW_UNDER_TEST: f32 = 2.0;

    let modest_pool = project_pool(&[("mem_other", 9.0), ("mem_under_test", RAW_UNDER_TEST)]);
    let dominated_pool = project_pool(&[("mem_other", 1000.0), ("mem_under_test", RAW_UNDER_TEST)]);

    let modest = hit_for(&modest_pool, "mem_under_test");
    let dominated = hit_for(&dominated_pool, "mem_under_test");

    // Same cause in both worlds: the raw engine value is byte-identical.
    assert_eq!(modest.lexical_score, Some(RAW_UNDER_TEST));
    assert_eq!(dominated.lexical_score, Some(RAW_UNDER_TEST));

    let modest_admitted = search_hit_meets_relevance_floor(modest, None);
    let dominated_admitted = search_hit_meets_relevance_floor(dominated, None);

    // State the expected value, both times, rather than asserting only that the
    // two agree: two identically-wrong decisions would satisfy bare equality.
    assert!(
        modest_admitted,
        "raw BM25 {RAW_UNDER_TEST} beside a 9.0 renders {rendered} and must be \
         admitted",
        rendered = modest.relevance_score(),
    );
    assert!(
        dominated_admitted,
        "raw BM25 {RAW_UNDER_TEST} is unchanged evidence, but beside a 1000.0 it \
         renders {rendered} and admission flips to {dominated_admitted}. The \
         floor is being applied in the query-relative domain, so a document is \
         dropped for what ELSE the query returned",
        rendered = dominated.relevance_score(),
    );
}
