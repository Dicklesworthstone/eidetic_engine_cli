use super::*;
use crate::search::{Embedder, EmbedderStack, HashEmbedder, IndexBuilder, IndexableDocument};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

type TestResult = Result<(), String>;

fn fixture() -> Result<(tempfile::TempDir, PathBuf), String> {
    let root = tempfile::tempdir().map_err(|e| e.to_string())?;
    let index = root.path().canonicalize().map_err(|e| e.to_string())?.join("index");
    let build_index = index.clone();
    crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
        let documents = vec![
            IndexableDocument::new("mem_prefetch", "cargo verification release command"),
            IndexableDocument::new("evd_prefetch", "session evidence cargo verification history"),
        ];
        IndexBuilder::new(&build_index)
            .with_embedder_stack(EmbedderStack::from_parts(
                Arc::new(HashEmbedder::default_256()) as Arc<dyn Embedder>, None,
            ))
            .add_documents(documents.clone()).build(&cx).await.map_err(|e| e.to_string())?;
        crate::core::index::build_lexical_tier(&cx, &build_index, &documents)
            .await.map_err(|e| e.to_string())?;
        crate::core::index::write_memory_eval_index_metadata_for_generation(&build_index, 7, 2)
            .map_err(|e| e.to_string())
    }).map_err(|e| e.to_string())??;
    Ok((root, index))
}

fn candidates() -> Vec<CassPrefetchCandidate> {
    ["cargo", "verification", "session", "release", "history"]
        .into_iter().map(|query| CassPrefetchCandidate::new(query, 1.0, "test")).collect()
}

fn fingerprint(root: &Path) -> Result<BTreeMap<PathBuf, String>, String> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(&path).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                pending.push(entry.path());
            } else {
                let content = std::fs::read(entry.path()).map_err(|e| e.to_string())?;
                files.insert(entry.path(), blake3::hash(&content).to_hex().to_string());
            }
        }
    }
    Ok(files)
}

#[test]
fn real_warming_reads_bounded_queries_and_reuses_the_production_reader_without_writes() -> TestResult {
    let (root, index) = fixture()?;
    let before = fingerprint(root.path())?;
    let report = warm_prefetch_lexical(&index, 7, &candidates(), Duration::from_secs(10), || false);
    assert_eq!(report.stop, PrefetchStop::Complete);
    assert_eq!(report.completed_queries, DEFAULT_PREFETCH_TOP_K);
    assert_eq!(report.matching_queries, DEFAULT_PREFETCH_TOP_K);
    let cached = super::super::PROCESS_LEXICAL_SEARCHER_CACHE.get().unwrap()
        .lock().unwrap().get(&index).unwrap().searcher.clone();
    let ordinary_reader = open_lexical_searcher(&index)?.unwrap();
    assert!(Arc::ptr_eq(&cached, &ordinary_reader));
    let ids = crate::core::run_cli_with_cx(Duration::from_secs(10), |cx| async move {
        ordinary_reader.search(&cx, "cargo", 16).await
            .map(|hits| hits.into_iter().map(|hit| hit.doc_id.to_string()).collect::<Vec<_>>())
            .map_err(|e| e.to_string())
    }).map_err(|e| e.to_string())??;
    assert_eq!(ids.len(), 2);
    assert!(ids.iter().any(|id| id == "evd_prefetch"));
    assert!(ids.iter().any(|id| id == "mem_prefetch"));
    assert_eq!(fingerprint(root.path())?, before, "prefetch must not create or rewrite any store/index asset");
    Ok(())
}

#[test]
fn newer_and_older_generation_requests_do_not_open_a_reader() -> TestResult {
    let (_root, index) = fixture()?;
    for generation in [6, 8] {
        let report = warm_prefetch_lexical(&index, generation, &candidates(), Duration::from_secs(10), || false);
        assert_eq!(report.stop, PrefetchStop::StaleGeneration);
        assert_eq!(report.completed_queries, 0);
    }
    if let Some(cache) = super::super::PROCESS_LEXICAL_SEARCHER_CACHE.get() {
        assert!(!cache.lock().unwrap().contains_key(&index));
    }
    Ok(())
}

#[test]
fn changed_corpus_or_evidence_policy_is_not_warmed() -> TestResult {
    let (_root, index) = fixture()?;
    let metadata = index.join("meta.json");
    let original: serde_json::Value = serde_json::from_slice(&std::fs::read(&metadata).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    for key in ["corpusRevision", "evidenceSecurityPolicyEpoch"] {
        let mut changed = original.clone();
        changed[key] = serde_json::Value::Null;
        std::fs::write(&metadata, serde_json::to_vec(&changed).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let report = warm_prefetch_lexical(&index, 7, &candidates(), Duration::from_secs(10), || false);
        assert_eq!(report.stop, PrefetchStop::StaleGeneration);
        assert_eq!(report.completed_queries, 0);
    }
    Ok(())
}

#[test]
fn publisher_lease_is_not_waited_on_and_release_allows_warming() -> TestResult {
    use rustix::fs::{flock, FlockOperation};
    let (_root, index) = fixture()?;
    let publisher = std::fs::File::open(index.parent().unwrap()).map_err(|e| e.to_string())?;
    flock(&publisher, FlockOperation::NonBlockingLockExclusive).map_err(|e| e.to_string())?;
    let report = warm_prefetch_lexical(&index, 7, &candidates(), Duration::from_secs(10), || false);
    assert_eq!(report.stop, PrefetchStop::Unavailable);
    assert_eq!(report.completed_queries, 0);
    drop(publisher);
    let report = warm_prefetch_lexical(&index, 7, &candidates(), Duration::from_secs(10), || false);
    assert_eq!(report.completed_queries, DEFAULT_PREFETCH_TOP_K);
    assert_eq!(report.stop, PrefetchStop::Complete);
    Ok(())
}

#[test]
fn shutdown_and_zero_budget_do_not_even_open_missing_paths() -> TestResult {
    let root = tempfile::tempdir().map_err(|e| e.to_string())?;
    let missing = root.path().join("missing").join("index");
    for (budget, stopping, expected) in [
        (Duration::from_secs(10), true, PrefetchStop::Interrupted),
        (Duration::ZERO, false, PrefetchStop::BudgetExceeded),
    ] {
        let report = warm_prefetch_lexical(&missing, 7, &candidates(), budget, || stopping);
        assert_eq!(report.stop, expected);
        assert_eq!(report.completed_queries, 0);
        assert!(!missing.parent().unwrap().exists());
    }
    Ok(())
}

#[test]
fn a_missing_generation_is_not_created_or_repaired() -> TestResult {
    let root = tempfile::tempdir().map_err(|e| e.to_string())?;
    let index = root.path().join("index");
    let before = fingerprint(root.path())?;
    let report = warm_prefetch_lexical(&index, 7, &candidates(), Duration::from_secs(10), || false);
    assert_eq!(report.completed_queries, 0);
    assert_ne!(report.stop, PrefetchStop::Complete);
    assert_eq!(fingerprint(root.path())?, before);
    Ok(())
}

#[test]
fn symlinked_lexical_assets_are_rejected_before_backend_open() -> TestResult {
    let (root, index) = fixture()?;
    let target = root.path().join("sensitive");
    std::fs::write(&target, "not index data").map_err(|e| e.to_string())?;
    std::os::unix::fs::symlink(&target, index.join("lexical").join("planted-link"))
        .map_err(|e| e.to_string())?;
    let report = warm_prefetch_lexical(&index, 7, &candidates(), Duration::from_secs(10), || false);
    assert_eq!(report.stop, PrefetchStop::Unavailable);
    assert_eq!(report.completed_queries, 0);
    assert_eq!(std::fs::read_to_string(target).map_err(|e| e.to_string())?, "not index data");
    Ok(())
}

#[test]
fn invalid_topics_are_skipped_and_empty_predictions_do_no_io() -> TestResult {
    let (_root, index) = fixture()?;
    let invalid = vec![
        CassPrefetchCandidate::new(" ", 1.0, "test"),
        CassPrefetchCandidate::new("x".repeat(MAX_PREFETCH_TOPIC_ID_BYTES + 1), 1.0, "test"),
        CassPrefetchCandidate::new("cargo", 1.0, "test"),
    ];
    let report = warm_prefetch_lexical(&index, 7, &invalid, Duration::from_secs(10), || false);
    assert_eq!(report.completed_queries, 1);
    assert_eq!(report.matching_queries, 1);
    assert_eq!(report.stop, PrefetchStop::Complete);
    let report = warm_prefetch_lexical(Path::new("absent"), 7, &[], Duration::from_secs(10), || false);
    assert_eq!(report, LexicalPrefetchReport::default());
    Ok(())
}
