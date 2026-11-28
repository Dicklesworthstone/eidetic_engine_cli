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
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::core::singleflight::SingleFlightGroup;
use crate::models::{SingleFlightKey, SingleFlightKeyInput, SingleFlightSurface};
use crate::search::{TwoTierConfig, TwoTierIndex};

const CAPACITY: usize = 8;
const OPEN_FOLLOWER_TIMEOUT: Duration = Duration::from_millis(250);
const VECTOR_FILES: [&str; 3] = ["vector.fast.idx", "vector.idx", "vector.quality.idx"];

static CACHE: OnceLock<VectorIndexCache> = OnceLock::new();

#[derive(Default)]
struct VectorIndexCache {
    entries: Mutex<Vec<CacheEntry>>,
    opens: SingleFlightGroup<Option<Arc<TwoTierIndex>>>,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct GenerationIdentity {
    generation: u64,
    directory_identity: (u64, u64),
    metadata_hash: [u8; 32],
    manifest_hash: Option<[u8; 32]>,
    vectors: [Option<ArtifactIdentity>; 3],
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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
        self.open_with(index_dir, config, OPEN_FOLLOWER_TIMEOUT, TwoTierIndex::open)
    }

    fn open_with<F>(
        &self,
        index_dir: &Path,
        config: TwoTierConfig,
        follower_timeout: Duration,
        mut open_index: F,
    ) -> Result<Arc<TwoTierIndex>, frankensearch::SearchError>
    where
        F: FnMut(&Path, TwoTierConfig) -> Result<TwoTierIndex, frankensearch::SearchError>,
    {
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
                return open_index(index_dir, config).map(Arc::new);
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
            return open_index(index_dir, config).map(Arc::new);
        }

        let Some(open_key) = index_open_key(&key) else {
            return self.open_and_admit(index_dir, config, key, &mut open_index);
        };
        let mut own_result = None;
        let shared = self.opens.run(&open_key, follower_timeout, || {
            let result =
                self.open_and_admit(index_dir, config.clone(), key.clone(), &mut open_index);
            // Only admitted readers may be shared. In particular, a transient
            // optional-tier failure must leave every follower free to retry.
            let reader = result
                .as_ref()
                .ok()
                .filter(|index| self.is_admitted(&key, index))
                .map(Arc::clone);
            let failed = result.is_err();
            own_result = Some(result);
            if failed {
                Err("vector index open failed".to_owned())
            } else {
                Ok(reader)
            }
        });
        // The opener owns the original typed error, including when a follower
        // times out and opens independently or the coalescer becomes poisoned.
        if let Some(result) = own_result {
            return result;
        }
        if let Ok(run) = shared
            && let Some(index) = run.value
            && cache_key(index_dir, &config).as_ref() == Ok(&key)
        {
            trace(index_dir, "hit", "coalesced_open");
            return Ok(index);
        }
        self.open_and_admit(index_dir, config, key, &mut open_index)
    }

    fn is_admitted(&self, key: &CacheKey, index: &Arc<TwoTierIndex>) -> bool {
        self.entries.lock().is_ok_and(|entries| {
            entries
                .iter()
                .any(|entry| entry.key == *key && Arc::ptr_eq(&entry.index, index))
        })
    }

    fn open_and_admit<F>(
        &self,
        index_dir: &Path,
        config: TwoTierConfig,
        key: CacheKey,
        open_index: &mut F,
    ) -> Result<Arc<TwoTierIndex>, frankensearch::SearchError>
    where
        F: FnMut(&Path, TwoTierConfig) -> Result<TwoTierIndex, frankensearch::SearchError>,
    {
        // A follower can reach this fallback after explicitly rejecting an
        // old shared reader. Never let the delayed cache-hit check below
        // resurrect that reader under its original, now-stale identity.
        let current_key = cache_key(index_dir, &config);
        if current_key.as_ref() != Ok(&key) {
            if let Ok(mut entries) = self.entries.lock() {
                entries.retain(|entry| {
                    entry.key.directory != key.directory
                        || current_key
                            .as_ref()
                            .is_ok_and(|current| entry.key.source == current.source)
                });
            }
            trace(index_dir, "bypass", "generation_changed_before_open");
            return open_index(index_dir, config).map(Arc::new);
        }
        // Another leader may have finished between our initial cache miss
        // and joining its flight. Recheck before doing any filesystem open.
        if let Ok(mut entries) = self.entries.lock()
            && let Some(entry) = entries.iter_mut().find(|entry| entry.key == key)
        {
            entry.last_used = Instant::now();
            trace(index_dir, "hit", "concurrent_admission");
            return Ok(Arc::clone(&entry.index));
        }
        // Neither the global cache mutex nor another workspace's flight is
        // held across filesystem work.
        let index = Arc::new(open_index(index_dir, config.clone())?);
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

fn index_open_key(key: &CacheKey) -> Option<SingleFlightKey> {
    // Preserve non-UTF-8 path identity and every inode/configuration field.
    // Only hashes enter the coalescer's diagnostic key.
    let directory = blake3::hash(key.directory.as_os_str().as_encoded_bytes())
        .to_hex()
        .to_string();
    let source = blake3::hash(&serde_json::to_vec(&key.source).ok()?)
        .to_hex()
        .to_string();
    let configuration = blake3::hash(&key.configuration).to_hex().to_string();
    let options = [
        ("generation_identity", source.as_str()),
        ("configuration", configuration.as_str()),
    ];
    let mut input = SingleFlightKeyInput::new(
        SingleFlightSurface::Search,
        &directory,
        0,
        crate::core::index::INDEX_METADATA_SCHEMA_V2,
    );
    input.index_generation = Some(key.source.generation);
    input.source_mode = Some("immutable_vector_reader");
    input.option_pairs = &options;
    Some(SingleFlightKey::from_input(&input))
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
    let (directory, source) = immutable_generation_identity(index_dir)?;
    Ok(CacheKey {
        directory,
        source,
        configuration: serde_json::to_vec(config).map_err(|_| "configuration_unverified")?,
    })
}

/// An in-flight global hybrid result can be shared for this exact live
/// publication. A lagging, mutable, damaged, missing or busy generation
/// simply runs outside the request coalescer.
pub(crate) fn coalescible_global_generation(
    index_dir: &Path,
    source_generation: u64,
) -> Option<String> {
    let lease =
        crate::core::index::IndexGenerationLease::try_read_for_inspection(index_dir).ok()??;
    let (_, identity) = immutable_generation_identity(index_dir).ok()?;
    if identity.generation != source_generation {
        return None;
    }
    // Best-effort hybrid always selects a complete current generation ahead
    // of retained candidates. An unreadable active tier cannot witness a
    // result that might instead come from a different retained directory.
    if !lease.admits_current_publication(index_dir, source_generation) {
        return None;
    }
    Some(
        blake3::hash(&serde_json::to_vec(&identity).ok()?)
            .to_hex()
            .to_string(),
    )
}

fn immutable_generation_identity(
    index_dir: &Path,
) -> Result<(PathBuf, GenerationIdentity), &'static str> {
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
    {
        return Err("metadata_unverified");
    }
    let generation = parsed
        .get("generation")
        .and_then(serde_json::Value::as_u64)
        .ok_or("metadata_unverified")?;
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
    Ok((
        directory,
        GenerationIdentity {
            generation,
            directory_identity,
            metadata_hash: *blake3::hash(&metadata).as_bytes(),
            manifest_hash,
            vectors,
        },
    ))
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
    fn delayed_open_never_resurrects_a_replaced_cached_generation() -> TestResult {
        let root = tempfile::tempdir()?;
        let live = root.path().join("index");
        fixture(&live, "previous")?;
        let cache = VectorIndexCache::default();
        let config = TwoTierConfig::default();
        let original = cache.open(&live, config.clone())?;
        let original_key = cache_key(&live, &config)?;
        let staged = root.path().join("staged");
        fixture(&staged, "replacement")?;
        fs::rename(
            live.join(VECTOR_FILES[0]),
            root.path().join("displaced.idx"),
        )?;
        fs::rename(staged.join(VECTOR_FILES[0]), live.join(VECTOR_FILES[0]))?;

        let mut opens = 0;
        let replacement =
            cache.open_and_admit(&live, config, original_key, &mut |path, config| {
                opens += 1;
                TwoTierIndex::open(path, config)
            })?;
        assert_eq!(
            opens, 1,
            "a rejected identity must not hit its old cache entry"
        );
        assert!(!Arc::ptr_eq(&original, &replacement));
        assert_eq!(
            replacement.search_fast(&[1.0, 0.0, 0.0, 0.0], 1)?[0].doc_id,
            "replacement"
        );
        assert!(cache.entries.lock().map_err(|_| "cache lock")?.is_empty());
        Ok(())
    }

    fn wait_for_open_follower(cache: &VectorIndexCache) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let posture =
                cache
                    .opens
                    .posture(SingleFlightSurface::Search, true, Duration::from_secs(30));
            if posture.follower_join_count > 0 {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "follower did not join the held open"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn concurrent_misses_open_once_before_sharing_the_reader() -> TestResult {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::mpsc;

        let root = tempfile::tempdir()?;
        fixture(root.path(), "first")?;
        let cache = VectorIndexCache::default();
        let opens = AtomicUsize::new(0);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let cache_ref = &cache;
        let opens_ref = &opens;
        let path = root.path();
        std::thread::scope(|scope| -> TestResult {
            let leader = scope.spawn(move || {
                cache_ref.open_with(
                    path,
                    TwoTierConfig::default(),
                    Duration::from_secs(30),
                    |path, config| {
                        opens_ref.fetch_add(1, Ordering::SeqCst);
                        entered_tx.send(()).expect("announce leader open");
                        release_rx
                            .recv_timeout(Duration::from_secs(10))
                            .expect("release leader open");
                        TwoTierIndex::open(path, config)
                    },
                )
            });
            entered_rx.recv_timeout(Duration::from_secs(5))?;
            let follower = scope.spawn(move || {
                cache_ref.open_with(
                    path,
                    TwoTierConfig::default(),
                    Duration::from_secs(30),
                    |path, config| {
                        opens_ref.fetch_add(1, Ordering::SeqCst);
                        TwoTierIndex::open(path, config)
                    },
                )
            });
            wait_for_open_follower(&cache);
            release_tx.send(())?;
            let first = leader.join().expect("leader panicked")?;
            let second = follower.join().expect("follower panicked")?;
            assert_eq!(
                opens.load(Ordering::SeqCst),
                1,
                "concurrent admission alone is not coalescing"
            );
            assert!(Arc::ptr_eq(&first, &second));
            assert_eq!(
                first.search_fast(&[1.0, 0.0, 0.0, 0.0], 1)?,
                second.search_fast(&[1.0, 0.0, 0.0, 0.0], 1)?
            );
            assert_eq!(cache.opens.active_len()?, 0);
            Ok(())
        })
    }

    #[test]
    fn follower_timeout_opens_independently_then_joins_admitted_reader() -> TestResult {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::mpsc;

        let root = tempfile::tempdir()?;
        fixture(root.path(), "first")?;
        let cache = VectorIndexCache::default();
        let opens = AtomicUsize::new(0);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let cache_ref = &cache;
        let opens_ref = &opens;
        let path = root.path();
        std::thread::scope(|scope| -> TestResult {
            let leader = scope.spawn(move || {
                cache_ref.open_with(
                    path,
                    TwoTierConfig::default(),
                    Duration::from_secs(30),
                    |path, config| {
                        opens_ref.fetch_add(1, Ordering::SeqCst);
                        entered_tx.send(()).expect("announce leader open");
                        release_rx
                            .recv_timeout(Duration::from_secs(10))
                            .expect("release leader open");
                        TwoTierIndex::open(path, config)
                    },
                )
            });
            entered_rx.recv_timeout(Duration::from_secs(5))?;
            let fallback = cache.open_with(
                path,
                TwoTierConfig::default(),
                Duration::ZERO,
                |path, config| {
                    opens.fetch_add(1, Ordering::SeqCst);
                    TwoTierIndex::open(path, config)
                },
            )?;
            release_tx.send(())?;
            let first = leader.join().expect("leader panicked")?;
            assert_eq!(opens.load(Ordering::SeqCst), 2);
            assert!(Arc::ptr_eq(&first, &fallback));
            let posture = cache
                .opens
                .posture(SingleFlightSurface::Search, true, Duration::ZERO);
            assert_eq!(posture.follower_timeout_count, 1);
            assert_eq!(cache.opens.active_len()?, 0);
            Ok(())
        })
    }

    #[test]
    fn failed_leader_preserves_its_error_and_followers_retry() -> TestResult {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::mpsc;

        let root = tempfile::tempdir()?;
        fixture(root.path(), "first")?;
        let cache = VectorIndexCache::default();
        let opens = AtomicUsize::new(0);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let cache_ref = &cache;
        let opens_ref = &opens;
        let path = root.path();
        std::thread::scope(|scope| -> TestResult {
            let leader = scope.spawn(move || {
                cache_ref.open_with(
                    path,
                    TwoTierConfig::default(),
                    Duration::from_secs(30),
                    |_path, _config| {
                        opens_ref.fetch_add(1, Ordering::SeqCst);
                        entered_tx.send(()).expect("announce leader open");
                        release_rx
                            .recv_timeout(Duration::from_secs(10))
                            .expect("release leader open");
                        Err(frankensearch::SearchError::Cancelled {
                            phase: "reader.open.fixture".to_owned(),
                            reason: "original typed failure".to_owned(),
                        })
                    },
                )
            });
            entered_rx.recv_timeout(Duration::from_secs(5))?;
            let follower = scope.spawn(move || {
                cache_ref.open_with(
                    path,
                    TwoTierConfig::default(),
                    Duration::from_secs(30),
                    |path, config| {
                        opens_ref.fetch_add(1, Ordering::SeqCst);
                        TwoTierIndex::open(path, config)
                    },
                )
            });
            wait_for_open_follower(&cache);
            release_tx.send(())?;
            assert!(matches!(
                leader.join().expect("leader panicked"),
                Err(frankensearch::SearchError::Cancelled { phase, reason })
                    if phase == "reader.open.fixture" && reason == "original typed failure"
            ));
            let recovered = follower.join().expect("follower panicked")?;
            assert_eq!(opens.load(Ordering::SeqCst), 2);
            assert_eq!(
                recovered.search_fast(&[1.0, 0.0, 0.0, 0.0], 1)?[0].doc_id,
                "first"
            );
            assert_eq!(cache.opens.active_len()?, 0);
            Ok(())
        })
    }

    #[test]
    fn incomplete_quality_open_is_not_shared_with_waiting_followers() -> TestResult {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::mpsc;

        let root = tempfile::tempdir()?;
        fixture(root.path(), "first")?;
        let quality = root.path().join(VECTOR_FILES[2]);
        let mut builder = VectorIndex::create(&quality, "cache-test", 4)?;
        builder.write_record("first", &[1.0, 0.0, 0.0, 0.0])?;
        builder.finish()?;
        let mut writer = Some(VectorIndex::open(&quality)?);
        assert!(
            !root.path().join("vector.quality.idx.wal").exists(),
            "opening the quality writer must not bypass immutable-cache admission"
        );
        let cache = VectorIndexCache::default();
        let opens = AtomicUsize::new(0);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let cache_ref = &cache;
        let opens_ref = &opens;
        let path = root.path();
        std::thread::scope(|scope| -> TestResult {
            let leader = scope.spawn(move || {
                cache_ref.open_with(
                    path,
                    TwoTierConfig::default(),
                    Duration::from_secs(30),
                    |path, config| {
                        opens_ref.fetch_add(1, Ordering::SeqCst);
                        let index = TwoTierIndex::open(path, config)?;
                        assert!(!index.has_quality_index());
                        drop(writer.take());
                        entered_tx.send(()).expect("announce incomplete open");
                        release_rx
                            .recv_timeout(Duration::from_secs(10))
                            .expect("release incomplete open");
                        Ok(index)
                    },
                )
            });
            entered_rx.recv_timeout(Duration::from_secs(5))?;
            let follower = scope.spawn(move || {
                cache_ref.open_with(
                    path,
                    TwoTierConfig::default(),
                    Duration::from_secs(30),
                    |path, config| {
                        opens_ref.fetch_add(1, Ordering::SeqCst);
                        TwoTierIndex::open(path, config)
                    },
                )
            });
            wait_for_open_follower(&cache);
            release_tx.send(())?;
            let incomplete = leader.join().expect("leader panicked")?;
            let recovered = follower.join().expect("follower panicked")?;
            assert!(!incomplete.has_quality_index());
            assert!(recovered.has_quality_index());
            assert!(!Arc::ptr_eq(&incomplete, &recovered));
            assert_eq!(opens.load(Ordering::SeqCst), 2);
            assert_eq!(cache.entries.lock().map_err(|_| "cache lock")?.len(), 1);
            assert_eq!(cache.opens.active_len()?, 0);
            Ok(())
        })
    }

    #[test]
    fn open_keys_cover_exact_paths_artifacts_and_configuration() -> TestResult {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let root = tempfile::tempdir()?;
        fixture(root.path(), "first")?;
        let key = cache_key(root.path(), &TwoTierConfig::default())?;
        let original = index_open_key(&key).ok_or("missing open key")?;
        assert_eq!(
            original,
            index_open_key(&key).ok_or("missing repeated key")?
        );
        assert_eq!(original.index_generation, Some(1));

        let mut changed = key.clone();
        changed.source.directory_identity.1 += 1;
        assert_ne!(Some(original.clone()), index_open_key(&changed));
        changed = key.clone();
        changed.source.generation += 1;
        assert_ne!(Some(original.clone()), index_open_key(&changed));
        changed = key.clone();
        changed.source.metadata_hash[0] ^= 1;
        assert_ne!(Some(original.clone()), index_open_key(&changed));
        changed = key.clone();
        changed.source.manifest_hash = Some([1; 32]);
        assert_ne!(Some(original.clone()), index_open_key(&changed));
        changed = key.clone();
        changed.source.vectors[0]
            .as_mut()
            .ok_or("missing vector identity")?
            .inode += 1;
        assert_ne!(Some(original.clone()), index_open_key(&changed));
        changed = key.clone();
        changed.configuration.push(0);
        assert_ne!(Some(original), index_open_key(&changed));

        let mut first_path = key.clone();
        let mut second_path = key;
        first_path.directory = PathBuf::from(OsString::from_vec(b"index-\xff".to_vec()));
        second_path.directory = PathBuf::from(OsString::from_vec(b"index-\xfe".to_vec()));
        assert_eq!(
            first_path.directory.to_string_lossy(),
            second_path.directory.to_string_lossy()
        );
        assert_ne!(index_open_key(&first_path), index_open_key(&second_path));
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
