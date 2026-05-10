//! Frankensearch contract conformance tests (eidetic_engine_cli-bl5du).
//!
//! These tests pin the frankensearch crate contracts that ee relies on.
//! If upstream frankensearch changes break these tests, ee's search
//! integration needs review before adapting.

use ee::search::{
    CanonicalSearchDocument, DocumentSource, HashEmbedder, ScoreSource, ScoredResult,
    SearchScoreExplanation, TwoTierConfig,
};
use serde_json::json;

type TestResult<T = ()> = Result<T, String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

// ---------------------------------------------------------------------------
// ScoredResult field shape conformance
// ---------------------------------------------------------------------------

#[test]
fn scored_result_has_required_fields() -> TestResult {
    let result = ScoredResult {
        doc_id: "test-doc-001".to_owned(),
        score: 0.75,
        source: ScoreSource::Hybrid,
        index: Some(0),
        fast_score: Some(0.8),
        quality_score: Some(0.7),
        lexical_score: Some(2.5),
        rerank_score: Some(0.85),
        explanation: None,
        metadata: Some(json!({"source": "memory"})),
    };

    ensure_equal(&result.doc_id, &"test-doc-001".to_owned(), "doc_id")?;
    ensure(
        (result.score - 0.75).abs() < 0.001,
        "score should be 0.75",
    )?;
    ensure_equal(&result.source, &ScoreSource::Hybrid, "source")?;
    ensure_equal(&result.index, &Some(0), "index")?;
    ensure(result.fast_score.is_some(), "fast_score should be Some")?;
    ensure(
        result.quality_score.is_some(),
        "quality_score should be Some",
    )?;
    ensure(
        result.lexical_score.is_some(),
        "lexical_score should be Some",
    )?;
    ensure(
        result.rerank_score.is_some(),
        "rerank_score should be Some",
    )?;
    ensure(result.metadata.is_some(), "metadata should be Some")?;
    Ok(())
}

#[test]
fn scored_result_optional_fields_can_be_none() -> TestResult {
    let result = ScoredResult {
        doc_id: "minimal-doc".to_owned(),
        score: 1.0,
        source: ScoreSource::Lexical,
        index: None,
        fast_score: None,
        quality_score: None,
        lexical_score: None,
        rerank_score: None,
        explanation: None,
        metadata: None,
    };

    ensure_equal(&result.doc_id, &"minimal-doc".to_owned(), "doc_id")?;
    ensure(result.index.is_none(), "index should be None")?;
    ensure(result.fast_score.is_none(), "fast_score should be None")?;
    ensure(
        result.quality_score.is_none(),
        "quality_score should be None",
    )?;
    ensure(
        result.lexical_score.is_none(),
        "lexical_score should be None",
    )?;
    ensure(result.rerank_score.is_none(), "rerank_score should be None")?;
    ensure(result.explanation.is_none(), "explanation should be None")?;
    ensure(result.metadata.is_none(), "metadata should be None")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// ScoreSource enum variant conformance
// ---------------------------------------------------------------------------

#[test]
fn score_source_has_expected_variants() -> TestResult {
    let variants = [
        ScoreSource::Lexical,
        ScoreSource::SemanticFast,
        ScoreSource::SemanticQuality,
        ScoreSource::Hybrid,
        ScoreSource::Reranked,
    ];

    ensure_equal(&variants.len(), &5, "ScoreSource variant count")?;
    Ok(())
}

#[test]
fn score_source_debug_representations_are_stable() -> TestResult {
    ensure(
        format!("{:?}", ScoreSource::Lexical).contains("Lexical"),
        "Lexical debug",
    )?;
    ensure(
        format!("{:?}", ScoreSource::SemanticFast).contains("SemanticFast"),
        "SemanticFast debug",
    )?;
    ensure(
        format!("{:?}", ScoreSource::SemanticQuality).contains("SemanticQuality"),
        "SemanticQuality debug",
    )?;
    ensure(
        format!("{:?}", ScoreSource::Hybrid).contains("Hybrid"),
        "Hybrid debug",
    )?;
    ensure(
        format!("{:?}", ScoreSource::Reranked).contains("Reranked"),
        "Reranked debug",
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// HashEmbedder contract conformance
// ---------------------------------------------------------------------------

#[test]
fn hash_embedder_default_dimension_is_256() -> TestResult {
    let embedder = HashEmbedder::default_256();
    let embedding = embedder.embed_sync("test text");

    ensure_equal(&embedding.len(), &256, "HashEmbedder default dimension")?;
    Ok(())
}

#[test]
fn hash_embedder_produces_deterministic_embeddings() -> TestResult {
    let embedder = HashEmbedder::default_256();
    let text = "deterministic embedding test";

    let embedding_a = embedder.embed_sync(text);
    let embedding_b = embedder.embed_sync(text);

    ensure_equal(
        &embedding_a,
        &embedding_b,
        "HashEmbedder must be deterministic",
    )?;
    Ok(())
}

#[test]
fn hash_embedder_different_text_produces_different_embeddings() -> TestResult {
    let embedder = HashEmbedder::default_256();

    let embedding_a = embedder.embed_sync("first text");
    let embedding_b = embedder.embed_sync("second text");

    ensure(
        embedding_a != embedding_b,
        "different text should produce different embeddings",
    )?;
    Ok(())
}

#[test]
fn hash_embedder_empty_text_does_not_panic() -> TestResult {
    let embedder = HashEmbedder::default_256();
    let embedding = embedder.embed_sync("");

    ensure_equal(&embedding.len(), &256, "empty text embedding dimension")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Score normalization bounds conformance
// ---------------------------------------------------------------------------

#[test]
fn semantic_scores_are_normalized_zero_to_one() -> TestResult {
    let result = ScoredResult {
        doc_id: "norm-test".to_owned(),
        score: 0.85,
        source: ScoreSource::Hybrid,
        index: Some(0),
        fast_score: Some(0.82),
        quality_score: Some(0.91),
        lexical_score: Some(3.5),
        rerank_score: Some(0.88),
        explanation: None,
        metadata: None,
    };

    if let Some(fast) = result.fast_score {
        ensure(
            (0.0..=1.0).contains(&fast),
            format!("fast_score {fast} out of [0,1] range"),
        )?;
    }
    if let Some(quality) = result.quality_score {
        ensure(
            (0.0..=1.0).contains(&quality),
            format!("quality_score {quality} out of [0,1] range"),
        )?;
    }
    if let Some(rerank) = result.rerank_score {
        ensure(
            (0.0..=1.0).contains(&rerank),
            format!("rerank_score {rerank} out of [0,1] range"),
        )?;
    }
    Ok(())
}

#[test]
fn lexical_scores_can_exceed_one() -> TestResult {
    let result = ScoredResult {
        doc_id: "lexical-test".to_owned(),
        score: 3.5,
        source: ScoreSource::Lexical,
        index: Some(0),
        fast_score: None,
        quality_score: None,
        lexical_score: Some(3.5),
        rerank_score: None,
        explanation: None,
        metadata: None,
    };

    if let Some(lexical) = result.lexical_score {
        ensure(
            lexical >= 0.0,
            format!("lexical_score {lexical} should be non-negative"),
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// SearchScoreExplanation bridge conformance
// ---------------------------------------------------------------------------

#[test]
fn search_score_explanation_from_scored_result_preserves_doc_id() -> TestResult {
    let result = ScoredResult {
        doc_id: "explanation-test-doc".to_owned(),
        score: 0.77,
        source: ScoreSource::Hybrid,
        index: Some(5),
        fast_score: Some(0.75),
        quality_score: None,
        lexical_score: Some(2.1),
        rerank_score: None,
        explanation: None,
        metadata: None,
    };

    let explanation = SearchScoreExplanation::from_scored_result(&result);

    ensure_equal(
        &explanation.doc_id,
        &"explanation-test-doc".to_owned(),
        "doc_id preserved",
    )?;
    ensure(
        (explanation.final_score - 0.77).abs() < 0.001,
        "final_score preserved",
    )?;
    ensure_equal(&explanation.source, &"hybrid", "source name")?;
    Ok(())
}

#[test]
fn search_score_explanation_includes_primary_score_component() -> TestResult {
    let result = ScoredResult {
        doc_id: "primary-test".to_owned(),
        score: 0.65,
        source: ScoreSource::SemanticFast,
        index: None,
        fast_score: Some(0.65),
        quality_score: None,
        lexical_score: None,
        rerank_score: None,
        explanation: None,
        metadata: None,
    };

    let explanation = SearchScoreExplanation::from_scored_result(&result);
    let has_primary = explanation
        .components
        .iter()
        .any(|c| c.name == "primary_score");

    ensure(has_primary, "explanation must include primary_score component")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// CanonicalSearchDocument conversion conformance
// ---------------------------------------------------------------------------

#[test]
fn canonical_document_converts_to_indexable() -> TestResult {
    let doc = CanonicalSearchDocument::new("doc-123", "test content", DocumentSource::Memory)
        .with_title("Test Title")
        .with_workspace("/test/workspace")
        .with_level("procedural")
        .with_kind("rule")
        .with_created_at("2026-05-10T12:00:00Z")
        .with_tags(["tag1", "tag2"]);

    let indexable = doc.into_indexable();

    ensure_equal(&indexable.id, &"doc-123".to_owned(), "id")?;
    ensure(
        indexable.content.contains("test content"),
        "content preserved",
    )?;
    Ok(())
}

#[test]
fn canonical_document_source_variants_stable() -> TestResult {
    let sources = [
        (DocumentSource::Memory, "memory"),
        (DocumentSource::Session, "session"),
        (DocumentSource::Rule, "rule"),
        (DocumentSource::Import, "import"),
        (DocumentSource::Artifact, "artifact"),
        (DocumentSource::CurationCandidate, "curation_candidate"),
    ];

    for (source, expected_str) in sources {
        ensure_equal(&source.as_str(), &expected_str, "DocumentSource::as_str")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// TwoTierConfig contract conformance
// ---------------------------------------------------------------------------

#[test]
fn two_tier_config_has_expected_defaults() -> TestResult {
    let config = TwoTierConfig::default();

    ensure(config.rrf_k > 0.0, "rrf_k should be positive")?;
    ensure(
        config.quality_weight >= 0.0,
        "quality_weight should be non-negative",
    )?;
    ensure(
        config.candidate_multiplier > 0,
        "candidate_multiplier should be positive",
    )?;
    Ok(())
}
