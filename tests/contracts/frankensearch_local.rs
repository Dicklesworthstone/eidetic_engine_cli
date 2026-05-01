use std::collections::BTreeSet;
use std::fs;
use std::future::Future;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use ee::search::{
    CANONICAL_DOCUMENT_SCHEMA, CanonicalSearchDocument, DocumentSource, Embedder, EmbedderStack,
    EmbeddingConfig, FRANKENSEARCH_VERSION, HashEmbedder, IndexBuilder, IndexManifest,
    IndexableDocument, TwoTierConfig, TwoTierIndex, TwoTierSearcher,
};
use toml_edit::DocumentMut;

type TestResult<T = ()> = Result<T, String>;

const QUERY: &str = "fmtcheck release safety rule cargo fmt before release";
const EXPECTED_TOP_DOC: &str = "mem-release-format";
const MANIFEST_GENERATION: u64 = 11;
const FORBIDDEN_CRATES: &[&str] = &[
    "tokio",
    "tokio-util",
    "async-std",
    "smol",
    "rusqlite",
    "sqlx",
    "diesel",
    "sea-orm",
    "petgraph",
    "hyper",
    "axum",
    "tower",
    "reqwest",
];

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

fn hash_embedder_stack() -> EmbedderStack {
    let fast = Arc::new(HashEmbedder::default_256()) as Arc<dyn Embedder>;
    EmbedderStack::from_parts(fast, None)
}

fn canonical_documents() -> TestResult<Vec<IndexableDocument>> {
    let documents = vec![
        CanonicalSearchDocument::new(EXPECTED_TOP_DOC, QUERY, DocumentSource::Memory)
            .with_title("Release formatting guard")
            .with_workspace("/workspace/eidetic")
            .with_level("procedural")
            .with_kind("rule")
            .with_created_at("2026-04-30T12:00:00Z")
            .with_tags(["release", "formatting"])
            .into_indexable(),
        CanonicalSearchDocument::new(
            "mem-import-provenance",
            "cass import provenance diagnostics preserve source evidence",
            DocumentSource::Import,
        )
        .with_title("Import provenance")
        .with_workspace("/workspace/eidetic")
        .with_level("semantic")
        .with_kind("fact")
        .with_created_at("2026-04-30T12:01:00Z")
        .with_tags(["import", "provenance"])
        .into_indexable(),
        CanonicalSearchDocument::new(
            "mem-graph-snapshot",
            "graph snapshot validation records stale derived asset generations",
            DocumentSource::Rule,
        )
        .with_title("Graph snapshot validation")
        .with_workspace("/workspace/eidetic")
        .with_level("procedural")
        .with_kind("check")
        .with_created_at("2026-04-30T12:02:00Z")
        .with_tags(["graph", "status"])
        .into_indexable(),
    ];

    for document in &documents {
        let schema = document.metadata.get("schema").map(String::as_str);
        ensure_equal(&schema, &Some(CANONICAL_DOCUMENT_SCHEMA), "document schema")?;
    }

    Ok(documents)
}

fn write_index_manifest(index_dir: &Path, document_count: usize) -> TestResult<serde_json::Value> {
    let document_count = u64::try_from(document_count)
        .map_err(|error| format!("document count did not fit u64: {error}"))?;
    let mut manifest = IndexManifest::new(
        MANIFEST_GENERATION,
        "2026-04-30T12:00:00Z",
        document_count,
        MANIFEST_GENERATION,
        EmbeddingConfig::default(),
    )
    .with_vector_path("vector.fast.idx");

    if index_dir.join("lexical").exists() {
        manifest = manifest.with_lexical_path("lexical");
    }

    let json = manifest.data_json();
    let path = index_dir.join("ee.index_manifest.json");
    let bytes = serde_json::to_vec_pretty(&json).map_err(|error| error.to_string())?;
    fs::write(&path, bytes).map_err(|error| format!("failed to write manifest: {error}"))?;
    let roundtrip = fs::read(&path).map_err(|error| format!("failed to read manifest: {error}"))?;
    let decoded: serde_json::Value =
        serde_json::from_slice(&roundtrip).map_err(|error| error.to_string())?;
    ensure_equal(&decoded, &json, "manifest roundtrip")?;
    Ok(json)
}

async fn search_snapshot(
    cx: &asupersync::Cx,
    index_dir: &Path,
) -> TestResult<Vec<(String, String)>> {
    let index = Arc::new(
        TwoTierIndex::open(index_dir, TwoTierConfig::default()).map_err(map_search_error)?,
    );
    let fast = Arc::new(HashEmbedder::default_256()) as Arc<dyn Embedder>;
    let searcher = TwoTierSearcher::new(index, fast, TwoTierConfig::default());
    let (results, metrics) = searcher
        .search_collect(cx, QUERY, 3)
        .await
        .map_err(map_search_error)?;

    ensure(!results.is_empty(), "hash search should return results")?;
    ensure(
        metrics.fast_embedder_id.is_some(),
        "search metrics should record the fast hash embedder",
    )?;

    Ok(results
        .iter()
        .map(|result| (result.doc_id.clone(), format!("{:.6}", result.score)))
        .collect())
}

fn cargo_frankensearch_version() -> TestResult<String> {
    let manifest_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let manifest = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("failed to read {}: {error}", manifest_path.display()))?;
    let document = manifest
        .parse::<DocumentMut>()
        .map_err(|error| format!("Cargo.toml did not parse: {error}"))?;
    document["dependencies"]["frankensearch"]["version"]
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| "Cargo.toml frankensearch dependency must have a string version".to_owned())
}

fn run_default_cargo_tree() -> TestResult<String> {
    let output = Command::new(env!("CARGO"))
        .args([
            "tree",
            "--edges",
            "normal,build,dev",
            "--prefix",
            "none",
            "--manifest-path",
            concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"),
        ])
        .output()
        .map_err(|error| format!("failed to invoke cargo tree: {error}"))?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "cargo tree failed for default feature profile\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ));
    }

    String::from_utf8(output.stdout).map_err(|error| error.to_string())
}

fn forbidden_hits(tree_output: &str) -> BTreeSet<&'static str> {
    let mut hits = BTreeSet::new();
    for line in tree_output.lines() {
        let Some(name) = line.split_whitespace().next() else {
            continue;
        };
        for forbidden in FORBIDDEN_CRATES {
            if name == *forbidden {
                hits.insert(*forbidden);
            }
        }
    }
    hits
}

#[test]
fn frankensearch_hash_index_is_searchable_reopenable_and_manifested() -> TestResult {
    run_search_future(async {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let index_dir = temp.path().join("index");
        let documents = canonical_documents()?;
        let cx = asupersync::Cx::for_testing();

        let stats = IndexBuilder::new(&index_dir)
            .with_embedder_stack(hash_embedder_stack())
            .add_documents(documents.clone())
            .build(&cx)
            .await
            .map_err(map_search_error)?;

        ensure_equal(&stats.doc_count, &documents.len(), "indexed document count")?;
        ensure_equal(&stats.error_count, &0, "index build error count")?;
        ensure(
            !stats.has_quality_index,
            "hash contract uses fast tier only",
        )?;

        let manifest = write_index_manifest(&index_dir, stats.doc_count)?;
        ensure_equal(
            &manifest["schema"],
            &serde_json::json!(ee::models::INDEX_MANIFEST_SCHEMA_V1),
            "manifest schema",
        )?;
        ensure_equal(
            &manifest["document_schema"],
            &serde_json::json!(CANONICAL_DOCUMENT_SCHEMA),
            "manifest document schema",
        )?;
        ensure_equal(
            &manifest["generation"],
            &serde_json::json!(MANIFEST_GENERATION),
            "manifest generation",
        )?;
        ensure_equal(
            &manifest["frankensearch_version"],
            &serde_json::json!(FRANKENSEARCH_VERSION),
            "manifest frankensearch version",
        )?;
        ensure_equal(
            &FRANKENSEARCH_VERSION.to_owned(),
            &cargo_frankensearch_version()?,
            "frankensearch dependency version",
        )?;

        let first_snapshot = search_snapshot(&cx, &index_dir).await?;
        let second_snapshot = search_snapshot(&cx, &index_dir).await?;
        ensure_equal(
            &second_snapshot,
            &first_snapshot,
            "stable reopened ordering",
        )?;
        ensure_equal(
            &first_snapshot
                .first()
                .map(|(doc_id, _score)| doc_id.as_str()),
            &Some(EXPECTED_TOP_DOC),
            "top hash result",
        )?;

        let reopened =
            TwoTierIndex::open(&index_dir, TwoTierConfig::default()).map_err(map_search_error)?;
        ensure_equal(
            &reopened.doc_count(),
            &documents.len(),
            "reopened doc count",
        )
    })
}

#[test]
fn default_feature_tree_excludes_forbidden_dependencies_for_search_contract() -> TestResult {
    let tree = run_default_cargo_tree()?;
    let hits = forbidden_hits(&tree);
    ensure(
        hits.is_empty(),
        format!("default feature tree contains forbidden dependencies: {hits:?}"),
    )
}
