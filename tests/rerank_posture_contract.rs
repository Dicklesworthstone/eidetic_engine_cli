use ee::core::profile::{OperatingProfile, RuntimeProfileReport};
use ee::core::search::{
    ScoreSource, SearchDegradation, SearchHit, SearchReport, SearchSourceMode, SearchStatus,
};
use ee::models::{EmbedBackend, MemoryScope, MemoryScopeStats};

fn runtime_profile() -> RuntimeProfileReport {
    RuntimeProfileReport::for_profile(OperatingProfile::Swarm, "rerank-posture-contract")
}

fn scope_stats() -> MemoryScopeStats {
    MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0)
}

fn empty_search_report(degraded: Vec<SearchDegradation>) -> SearchReport {
    SearchReport {
        status: SearchStatus::NoResults,
        embed_backend: EmbedBackend::HashFallback,
        query: "release formatting policy".to_string(),
        requested_limit: 10,
        results: Vec::<SearchHit>::new(),
        elapsed_ms: 0.0,
        errors: Vec::new(),
        degraded,
        runtime_profile: runtime_profile(),
        rerank_configured_mode: ee::config::SearchRerankMode::Auto,
        rerank_configured_top_k: 50,
        relevance_floor_applied: Some(0.0),
        candidates_below_floor: 0,
        query_assist: None,
        source_mode_requested: SearchSourceMode::Hybrid,
        source_mode_applied: SearchSourceMode::Hybrid,
        source_mode_fallback: false,
        strict_source_mode: false,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
        scope_stats: scope_stats(),
    }
}

#[test]
fn empty_results_with_repeated_rerank_degradations_stay_fusion_only_degraded() {
    let report = empty_search_report(vec![
        SearchDegradation {
            code: "rerank_model_unavailable".to_string(),
            severity: "low".to_string(),
            message: "No available reranker model is registered for this workspace.".to_string(),
            repair: Some(
                "ee model fetch rerank-default --from-file /path/to/rerank-default-v1.tar.zst"
                    .to_string(),
            ),
        },
        SearchDegradation {
            code: "rerank_model_unavailable".to_string(),
            severity: "low".to_string(),
            message: "Registered reranker artifact is cached but incomplete.".to_string(),
            repair: Some(
                "ee model fetch rerank-default --from-file /path/to/rerank-default-v1.tar.zst"
                    .to_string(),
            ),
        },
    ]);

    let json = report.data_json();

    assert_eq!(json["resultCount"], 0);
    assert_eq!(json["rerank"]["schema"], "ee.rerank_posture.v1");
    assert_eq!(json["rerank"]["mode"], "fusion_only_degraded");
    assert_eq!(json["rerank"]["configured"], "auto");
    assert_eq!(json["rerank"]["topK"], 50);
    assert_eq!(json["rerank"]["available"], false);
    assert_eq!(json["rerank"]["rerankScoreCount"], 0);
    assert_eq!(
        json["rerank"]["scoreKind"],
        ScoreSource::Hybrid.score_kind()
    );
    assert_eq!(json["rerank"]["degradedCode"], "rerank_model_unavailable");
    assert_eq!(
        json["degraded"].as_array().map_or(0, |rows| rows.len()),
        2,
        "all degraded rows should remain visible for diagnosis"
    );
}
