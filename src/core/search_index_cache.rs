//! Resident readers of EE's immutable, atomically published generations.
//!
//! A fresh `TwoTierIndex` loses more than its mmap: frankensearch-index 0.4.0
//! lazily quantizes the complete F16 fast tier into a `OnceLock<Vec<i8>>`.
//! Keep that allocation and tier alignment across warm requests. This cache
//! never replaces canonical per-request source/producer admission and never
//! owns the generation lease or a database snapshot between requests.
//!
//! Published EE vector files are immutable: delta writers use independent
//! staging inodes, compact their WAL, then exchange the complete directory
//! under the publication lease. Read-only vector handles keep shared inode
//! locks, so caching a mutable/WAL-bearing or unidentified artifact would be
//! unsafe. Those cases retain the original uncached open and error behavior.

use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use crate::search::{TwoTierConfig, TwoTierIndex};

const CAPACITY: usize = 8;
const VECTOR_FILES: [&str; 3] = ["vector.fast.idx", "vector.idx", "vector.quality.idx"];

static CACHE: OnceLock<VectorIndexCache> = OnceLock::new();

#[derive(Default)]
struct VectorIndexCache {
    entries: Mutex<Vec<CacheEntry>>,
}

struct CacheEntry {
    key: CacheKey,
    index: Arc<TwoTierIndex>,
    last_used: Instant,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CacheKey {
    directory: PathBuf,
    source: GenerationIdentity,
    configuration: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GenerationIdentity {
    directory_identity: (u64, u64),
    metadata_hash: [u8; 32],
    manifest_hash: Option<[u8; 32]>,
    vectors: [Option<ArtifactIdentity>; 3],
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ArtifactIdentity {
    device: u64,
    inode: u64,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl ArtifactIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Result<Self, &'static str> {
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err("unsafe_artifact");
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}

/// Called only while the request holds its shared generation lease.
pub(super) fn open(
    index_dir: &Path,
    config: TwoTierConfig,
) -> Result<Arc<TwoTierIndex>, frankensearch::SearchError> {
    CACHE
        .get_or_init(VectorIndexCache::default)
        .open(index_dir, config)
}

impl VectorIndexCache {
    fn open(
        &self,
        index_dir: &Path,
        config: TwoTierConfig,
    ) -> Result<Arc<TwoTierIndex>, frankensearch::SearchError> {
        let key = match cache_key(index_dir, &config) {
            Ok(key) => key,
            Err(reason) => {
                // If an artifact becomes mutable or unverifiable, do not
                // leave an old resident mapping holding its shared file lock.
                let canonical = fs::canonicalize(index_dir).ok();
                if let Ok(mut entries) = self.entries.lock() {
                    entries.retain(|entry| {
                        entry.key.directory != index_dir
                            && canonical.as_ref() != Some(&entry.key.directory)
                    });
                }
                trace(index_dir, "bypass", reason);
                return TwoTierIndex::open(index_dir, config).map(Arc::new);
            }
        };
        if let Ok(mut entries) = self.entries.lock() {
            // A same-name replacement or metadata change retires every
            // configuration of that generation, including retained mmaps.
            entries.retain(|entry| {
                entry.key.directory != key.directory || entry.key.source == key.source
            });
            if let Some(entry) = entries.iter_mut().find(|entry| entry.key == key) {
                entry.last_used = Instant::now();
                trace(index_dir, "hit", "immutable_generation");
                return Ok(Arc::clone(&entry.index));
            }
        } else {
            trace(index_dir, "bypass", "cache_unavailable");
            return TwoTierIndex::open(index_dir, config).map(Arc::new);
        }

        // Opening outside the cache mutex does not serialize unrelated
        // workspaces. A racing miss joins the already admitted allocation
        // below, so its first search still shares the lazy quantized tier.
        let index = Arc::new(TwoTierIndex::open(index_dir, config.clone())?);
        if key.source.vectors[2].is_some() && !index.has_quality_index() {
            // Upstream deliberately degrades a failed optional quality open
            // to fast-only. A transient lock/IO failure must be retried next
            // request even if no file identity changed in the meantime.
            trace(index_dir, "bypass", "quality_tier_unavailable");
            return Ok(index);
        }
        if cache_key(index_dir, &config).as_ref() != Ok(&key) {
            trace(index_dir, "bypass", "generation_changed_during_open");
            return Ok(index);
        }
        let Ok(mut entries) = self.entries.lock() else {
            trace(index_dir, "bypass", "cache_unavailable");
            return Ok(index);
        };
        if let Some(entry) = entries.iter_mut().find(|entry| entry.key == key) {
            entry.last_used = Instant::now();
            trace(index_dir, "hit", "concurrent_admission");
            return Ok(Arc::clone(&entry.index));
        }
        if entries.len() >= CAPACITY
            && let Some((oldest, _)) = entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.last_used)
        {
            entries.swap_remove(oldest);
        }
        entries.push(CacheEntry {
            key,
            index: Arc::clone(&index),
            last_used: Instant::now(),
        });
        trace(index_dir, "miss", "immutable_generation");
        Ok(index)
    }
}

fn trace(index_dir: &Path, decision: &'static str, reason: &'static str) {
    tracing::debug!(
        target: "ee::search::index_cache",
        index_dir = %index_dir.display(),
        cache_kind = "vector",
        cache_decision = decision,
        cache_reason = reason,
        "resolved process-scoped vector reader"
    );
}

fn cache_key(index_dir: &Path, config: &TwoTierConfig) -> Result<CacheKey, &'static str> {
    // Serialize all upstream options, including fields added after the old
    // query-plan hash. Metrics callbacks are skipped by upstream serde; do
    // not cache configurations carrying one or ambiguous non-finite floats.
    if config.metrics_exporter.is_some()
        || !config.quality_weight.is_finite()
        || !config.rrf_k.is_finite()
        || !config.graph_ranking_weight.is_finite()
        || !config.neighbor_smoothing_alpha.is_finite()
        || !config.hubness_beta.is_finite()
    {
        return Err("unsupported_configuration");
    }
    let directory_identity =
        super::generation_directory_identity(index_dir).map_err(|_| "directory_unverified")?;
    let directory = fs::canonicalize(index_dir).map_err(|_| "directory_unverified")?;
    for name in VECTOR_FILES {
        match fs::symlink_metadata(index_dir.join(format!("{name}.wal"))) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => return Err("wal_present"),
            Err(_) => return Err("wal_unverified"),
        }
    }
    // This locked dependency graph uses native exact retrieval, not upstream
    // ANN. If a different layout supplies mutable ANN sidecars, preserve its
    // original open semantics instead of caching an incomplete identity.
    for name in ["vector.fast.hnsw", "vector.quality.hnsw"] {
        match fs::symlink_metadata(index_dir.join(name)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => return Err("ann_sidecar_present"),
            Err(_) => return Err("artifact_unverified"),
        }
    }
    let metadata = read_metadata(&index_dir.join("meta.json"))?.ok_or("metadata_unverified")?;
    let parsed: serde_json::Value =
        serde_json::from_slice(&metadata).map_err(|_| "metadata_unverified")?;
    if parsed.get("schema").and_then(serde_json::Value::as_str)
        != Some(crate::core::index::INDEX_METADATA_SCHEMA_V2)
        || parsed
            .get("generation")
            .and_then(serde_json::Value::as_u64)
            .is_none()
    {
        return Err("metadata_unverified");
    }
    let manifest_hash = read_metadata(&index_dir.join("manifest.json"))?
        .map(|bytes| *blake3::hash(&bytes).as_bytes());
    let vectors = [
        artifact_identity(&index_dir.join(VECTOR_FILES[0]))?,
        artifact_identity(&index_dir.join(VECTOR_FILES[1]))?,
        artifact_identity(&index_dir.join(VECTOR_FILES[2]))?,
    ];
    if vectors[0].is_none() && vectors[1].is_none() {
        return Err("vector_unverified");
    }
    Ok(CacheKey {
        directory,
        source: GenerationIdentity {
            directory_identity,
            metadata_hash: *blake3::hash(&metadata).as_bytes(),
            manifest_hash,
            vectors,
        },
        configuration: serde_json::to_vec(config).map_err(|_| "configuration_unverified")?,
    })
}

fn artifact_identity(path: &Path) -> Result<Option<ArtifactIdentity>, &'static str> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => ArtifactIdentity::from_metadata(&metadata).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("artifact_unverified"),
    }
}

fn read_metadata(path: &Path) -> Result<Option<Vec<u8>>, &'static str> {
    let Some(identity) = artifact_identity(path)? else {
        return Ok(None);
    };
    if identity.length > super::MAX_PLAN_CACHE_INDEX_MANIFEST_BYTES {
        return Err("metadata_unverified");
    }
    let file = File::open(path).map_err(|_| "metadata_unverified")?;
    let mut bytes = Vec::new();
    (&file)
        .take(super::MAX_PLAN_CACHE_INDEX_MANIFEST_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| "metadata_unverified")?;
    if bytes.len() as u64 > super::MAX_PLAN_CACHE_INDEX_MANIFEST_BYTES
        || ArtifactIdentity::from_metadata(&file.metadata().map_err(|_| "metadata_unverified")?)?
            != identity
    {
        return Err("metadata_changed");
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use frankensearch::VectorIndex;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn fixture(path: &Path, document: &str) -> TestResult {
        fs::create_dir_all(path)?;
        let mut builder = VectorIndex::create(&path.join(VECTOR_FILES[0]), "cache-test", 4)?;
        builder.write_record(document, &[1.0, 0.0, 0.0, 0.0])?;
        builder.finish()?;
        fs::write(
            path.join("meta.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema": crate::core::index::INDEX_METADATA_SCHEMA_V2,
                "generation": 1,
            }))?,
        )?;
        Ok(())
    }

    #[test]
    fn repeated_generation_keeps_reader_and_quantized_tier_with_canonical_results() -> TestResult {
        let root = tempfile::tempdir()?;
        fixture(root.path(), "first")?;
        let cache = VectorIndexCache::default();
        let config = TwoTierConfig::default();
        let first = cache.open(root.path(), config.clone())?;
        let expected = first.search_fast(&[1.0, 0.0, 0.0, 0.0], 1)?;
        let weak = Arc::downgrade(&first);
        drop(first);
        let second = cache.open(root.path(), config.clone())?;
        assert!(Arc::ptr_eq(
            &weak.upgrade().ok_or("reader was dropped")?,
            &second
        ));
        // The lazy full-corpus int8 allocation is owned by this exact reader.
        assert_eq!(second.search_fast(&[1.0, 0.0, 0.0, 0.0], 1)?, expected);
        let cold = TwoTierIndex::open(root.path(), config)?;
        assert_eq!(cold.search_fast(&[1.0, 0.0, 0.0, 0.0], 1)?, expected);
        Ok(())
    }

    #[test]
    fn configuration_and_generation_changes_refresh_readers() -> TestResult {
        let root = tempfile::tempdir()?;
        fixture(root.path(), "first")?;
        let cache = VectorIndexCache::default();
        let config = TwoTierConfig::default();
        let first = cache.open(root.path(), config.clone())?;
        let changed = TwoTierConfig {
            mrl_rescore_top_k: 7,
            ..config.clone()
        };
        let other = cache.open(root.path(), changed.clone())?;
        assert!(!Arc::ptr_eq(&first, &other));
        assert!(Arc::ptr_eq(&other, &cache.open(root.path(), changed)?));
        fs::write(
            root.path().join("meta.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema": crate::core::index::INDEX_METADATA_SCHEMA_V2,
                "generation": 2,
            }))?,
        )?;
        let refreshed = cache.open(root.path(), config)?;
        assert!(!Arc::ptr_eq(&first, &refreshed));
        assert_eq!(cache.entries.lock().map_err(|_| "cache lock")?.len(), 1);
        Ok(())
    }

    #[test]
    fn same_generation_file_and_directory_replacements_never_reuse_displaced_bytes() -> TestResult {
        let root = tempfile::tempdir()?;
        let live = root.path().join("index");
        fixture(&live, "previous")?;
        let cache = VectorIndexCache::default();
        let config = TwoTierConfig::default();
        let original = cache.open(&live, config.clone())?;
        let staged = root.path().join("staged");
        fixture(&staged, "replacement")?;
        fs::rename(
            live.join(VECTOR_FILES[0]),
            root.path().join("displaced.idx"),
        )?;
        fs::rename(staged.join(VECTOR_FILES[0]), live.join(VECTOR_FILES[0]))?;
        let replaced = cache.open(&live, config.clone())?;
        assert!(!Arc::ptr_eq(&original, &replaced));
        assert_eq!(
            replaced.search_fast(&[1.0, 0.0, 0.0, 0.0], 1)?[0].doc_id,
            "replacement"
        );
        assert_eq!(
            original.search_fast(&[1.0, 0.0, 0.0, 0.0], 1)?[0].doc_id,
            "previous"
        );
        fs::rename(&live, root.path().join("displaced-generation"))?;
        fixture(&live, "current")?;
        let current = cache.open(&live, config)?;
        assert!(!Arc::ptr_eq(&replaced, &current));
        assert_eq!(
            current.search_fast(&[1.0, 0.0, 0.0, 0.0], 1)?[0].doc_id,
            "current"
        );
        Ok(())
    }

    #[test]
    fn wal_and_unidentified_artifacts_bypass_and_retire_cached_readers() -> TestResult {
        let root = tempfile::tempdir()?;
        let live = root.path().join("index");
        fixture(&live, "first")?;
        let cache = VectorIndexCache::default();
        let config = TwoTierConfig::default();
        let first = cache.open(&live, config.clone())?;
        let weak = Arc::downgrade(&first);
        drop(first);
        let staged = root.path().join("staged.idx");
        fs::copy(live.join(VECTOR_FILES[0]), &staged)?;
        let mut writer = VectorIndex::open(&staged)?;
        writer.append("from-wal", &[0.0, 1.0, 0.0, 0.0])?;
        drop(writer);
        fs::copy(
            root.path().join("staged.idx.wal"),
            live.join("vector.fast.idx.wal"),
        )?;
        let wal = cache.open(&live, config.clone())?;
        assert!(
            weak.upgrade().is_none(),
            "WAL appearance must retire resident handles"
        );
        assert_eq!(
            wal.search_fast(&[0.0, 1.0, 0.0, 0.0], 1)?[0].doc_id,
            "from-wal"
        );
        assert!(!Arc::ptr_eq(&wal, &cache.open(&live, config.clone())?));
        fs::rename(
            live.join("vector.fast.idx.wal"),
            root.path().join("saved.wal"),
        )?;
        fs::write(live.join("meta.json"), b"{not valid metadata")?;
        let unidentified = cache.open(&live, config.clone())?;
        assert!(!Arc::ptr_eq(&unidentified, &cache.open(&live, config)?));
        assert!(cache.entries.lock().map_err(|_| "cache lock")?.is_empty());
        Ok(())
    }

    #[test]
    fn resident_reader_count_is_bounded_and_workspace_paths_are_isolated() -> TestResult {
        let root = tempfile::tempdir()?;
        let cache = VectorIndexCache::default();
        let mut readers = Vec::new();
        for i in 0..(CAPACITY + 2) {
            let directory = root.path().join(format!("workspace-{i}"));
            fixture(&directory, &format!("memory-{i}"))?;
            let index = cache.open(&directory, TwoTierConfig::default())?;
            assert_eq!(
                index.search_fast(&[1.0, 0.0, 0.0, 0.0], 1)?[0].doc_id,
                format!("memory-{i}")
            );
            readers.push(Arc::downgrade(&index));
        }
        assert_eq!(
            cache.entries.lock().map_err(|_| "cache lock")?.len(),
            CAPACITY
        );
        assert!(readers[0].upgrade().is_none());
        assert!(readers[1].upgrade().is_none());
        assert!(readers.last().ok_or("missing reader")?.upgrade().is_some());
        Ok(())
    }

    #[test]
    fn transient_optional_quality_failure_is_not_cached() -> TestResult {
        let root = tempfile::tempdir()?;
        fixture(root.path(), "first")?;
        let quality = root.path().join(VECTOR_FILES[2]);
        let mut builder = VectorIndex::create(&quality, "cache-test", 4)?;
        builder.write_record("first", &[1.0, 0.0, 0.0, 0.0])?;
        builder.finish()?;
        let writer = VectorIndex::open(&quality)?;
        let cache = VectorIndexCache::default();
        let config = TwoTierConfig::default();
        let degraded = cache.open(root.path(), config.clone())?;
        assert!(!degraded.has_quality_index());
        assert!(cache.entries.lock().map_err(|_| "cache lock")?.is_empty());
        drop(writer);
        let recovered = cache.open(root.path(), config)?;
        assert!(recovered.has_quality_index());
        assert!(!Arc::ptr_eq(&degraded, &recovered));
        Ok(())
    }
}
