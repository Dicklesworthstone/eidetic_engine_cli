#![cfg(all(feature = "fts5", feature = "lexical-bm25"))]

use std::future::Future;

use ee::search::{CanonicalSearchDocument, DocumentSource};
use frankensearch::{
    Fts5Config, Fts5ContentMode, Fts5LexicalSearch, Fts5Tokenizer, IndexableDocument,
    LexicalSearch, ScoreSource, ScoredResult, TantivyIndex,
};

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

fn run_search_future<T>(future: impl Future<Output = TestResult<T>>) -> TestResult<T> {
    ee::core::run_cli_future(future)
        .map_err(|error| format!("asupersync runtime failed: {error}"))?
}

fn map_search_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn canonical_documents() -> Vec<IndexableDocument> {
    vec![
        CanonicalSearchDocument::new(
            "mem-release-format",
            "fmtcheck release safety rule: run cargo fmt before release.",
            DocumentSource::Memory,
        )
        .with_title("Release formatting rule")
        .with_workspace("/workspace/eidetic")
        .with_level("procedural")
        .with_kind("rule")
        .with_created_at("2026-04-30T12:00:00Z")
        .with_tags(["release", "formatting"])
        .into_indexable(),
        CanonicalSearchDocument::new(
            "mem-lexical-fallback",
            "lexicalfallback search works when semantic embedding is unavailable.",
            DocumentSource::Memory,
        )
        .with_title("Lexical fallback rule")
        .with_workspace("/workspace/eidetic")
        .with_level("semantic")
        .with_kind("fact")
        .with_created_at("2026-04-30T12:01:00Z")
        .with_tags(["search", "fallback"])
        .into_indexable(),
        CanonicalSearchDocument::new(
            "sess-cass-import",
            "cassimport session transcript mentions import diagnostics and provenance.",
            DocumentSource::Session,
        )
        .with_title("CASS import session")
        .with_workspace("/workspace/eidetic")
        .with_kind("cass_session")
        .with_created_at("2026-04-30T12:02:00Z")
        .into_indexable(),
    ]
}

fn fts5_search() -> Fts5LexicalSearch {
    Fts5LexicalSearch::new(Fts5Config {
        content_mode: Fts5ContentMode::Stored,
        tokenizer: Fts5Tokenizer::Unicode61,
        ..Fts5Config::default()
    })
}

fn first_doc_id(results: &[ScoredResult]) -> TestResult<&str> {
    results
        .first()
        .map(|result| result.doc_id.as_str())
        .ok_or_else(|| "expected at least one lexical result".to_string())
}

#[test]
fn fts5_indexes_canonical_documents_with_metadata() -> TestResult {
    run_search_future(async {
        let cx = asupersync::Cx::for_testing();
        let documents = canonical_documents();
        let search = fts5_search();

        search
            .index_documents(&cx, &documents)
            .await
            .map_err(map_search_error)?;
        search.commit(&cx).await.map_err(map_search_error)?;

        ensure_equal(&search.doc_count(), &documents.len(), "fts5 doc count")?;

        let results = search
            .search(&cx, "fmtcheck", 10)
            .await
            .map_err(map_search_error)?;
        ensure_equal(&first_doc_id(&results)?, &"mem-release-format", "top hit")?;
        let top_result = results
            .first()
            .ok_or_else(|| "expected an fts5 top result".to_string())?;
        ensure_equal(&top_result.source, &ScoreSource::Lexical, "score source")?;
        ensure(
            top_result.lexical_score.is_some_and(|score| score > 0.0),
            "fts5 hit must include a positive lexical score",
        )?;

        let source = top_result
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("source"))
            .and_then(serde_json::Value::as_str);
        ensure_equal(&source, &Some("memory"), "metadata source")
    })
}

#[test]
fn fts5_snippet_smoke_highlights_matched_term() -> TestResult {
    run_search_future(async {
        let cx = asupersync::Cx::for_testing();
        let documents = canonical_documents();
        let search = fts5_search();

        search
            .index_documents(&cx, &documents)
            .await
            .map_err(map_search_error)?;
        search.commit(&cx).await.map_err(map_search_error)?;

        let hits = search
            .search_with_snippets("lexicalfallback", 5)
            .map_err(map_search_error)?;
        ensure_equal(&hits.len(), &1, "fts5 snippet hit count")?;
        let hit = hits
            .first()
            .ok_or_else(|| "expected an fts5 snippet hit".to_string())?;
        ensure_equal(
            &hit.doc_id,
            &"mem-lexical-fallback".to_string(),
            "snippet hit doc id",
        )?;
        let snippet = hit
            .snippet
            .as_deref()
            .ok_or_else(|| "expected stored-content FTS5 snippet".to_string())?;
        ensure(
            snippet.contains("<b>lexicalfallback</b>"),
            format!("snippet should highlight query term, got {snippet:?}"),
        )
    })
}

#[test]
fn fts5_and_tantivy_lexical_paths_agree_on_top_result() -> TestResult {
    run_search_future(async {
        let cx = asupersync::Cx::for_testing();
        let documents = canonical_documents();

        let fts5 = fts5_search();
        fts5.index_documents(&cx, &documents)
            .await
            .map_err(map_search_error)?;
        fts5.commit(&cx).await.map_err(map_search_error)?;
        let fts5_results = fts5
            .search(&cx, "lexicalfallback", 5)
            .await
            .map_err(map_search_error)?;

        let tantivy_dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let tantivy = TantivyIndex::create(tantivy_dir.path()).map_err(map_search_error)?;
        tantivy
            .index_documents(&cx, &documents)
            .await
            .map_err(map_search_error)?;
        tantivy.commit(&cx).await.map_err(map_search_error)?;
        let tantivy_results = tantivy
            .search(&cx, "lexicalfallback", 5)
            .await
            .map_err(map_search_error)?;

        ensure_equal(
            &first_doc_id(&fts5_results)?,
            &"mem-lexical-fallback",
            "fts5 top hit",
        )?;
        ensure_equal(
            &first_doc_id(&tantivy_results)?,
            &"mem-lexical-fallback",
            "tantivy top hit",
        )?;
        let fts5_top = fts5_results
            .first()
            .ok_or_else(|| "expected an fts5 top result".to_string())?;
        let tantivy_top = tantivy_results
            .first()
            .ok_or_else(|| "expected a tantivy top result".to_string())?;
        ensure_equal(&fts5_top.source, &ScoreSource::Lexical, "fts5 source")?;
        ensure_equal(&tantivy_top.source, &ScoreSource::Lexical, "tantivy source")
    })
}

#[test]
fn fts5_empty_query_matches_lexical_fallback_empty_behavior() -> TestResult {
    run_search_future(async {
        let cx = asupersync::Cx::for_testing();
        let documents = canonical_documents();

        let fts5 = fts5_search();
        fts5.index_documents(&cx, &documents)
            .await
            .map_err(map_search_error)?;
        fts5.commit(&cx).await.map_err(map_search_error)?;

        let tantivy_dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let tantivy = TantivyIndex::create(tantivy_dir.path()).map_err(map_search_error)?;
        tantivy
            .index_documents(&cx, &documents)
            .await
            .map_err(map_search_error)?;
        tantivy.commit(&cx).await.map_err(map_search_error)?;

        let fts5_results = fts5.search(&cx, "   ", 5).await.map_err(map_search_error)?;
        let tantivy_results = tantivy
            .search(&cx, "   ", 5)
            .await
            .map_err(map_search_error)?;

        ensure(fts5_results.is_empty(), "fts5 empty query returns no hits")?;
        ensure(
            tantivy_results.is_empty(),
            "tantivy fallback empty query returns no hits",
        )
    })
}
