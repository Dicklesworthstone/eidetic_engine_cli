//! Filesystem-backed L2 cache for serialized context-pack JSON.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

pub const PACK_L2_CACHE_ENTRY_SCHEMA_V1: &str = "ee.pack.l2_cache.entry.v1";
const DEFAULT_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackL2CacheOptions {
    pub max_bytes: u64,
    pub max_age: Duration,
}

impl PackL2CacheOptions {
    #[must_use]
    pub const fn new(max_bytes: u64, max_age: Duration) -> Self {
        Self { max_bytes, max_age }
    }
}

impl Default for PackL2CacheOptions {
    fn default() -> Self {
        Self {
            max_bytes: 1_073_741_824,
            max_age: DEFAULT_MAX_AGE,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackL2Cache {
    root: PathBuf,
    options: PackL2CacheOptions,
}

impl PackL2Cache {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, options: PackL2CacheOptions) -> Self {
        Self {
            root: root.into(),
            options,
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn options(&self) -> &PackL2CacheOptions {
        &self.options
    }

    #[must_use]
    pub fn entry_path(&self, key: &str) -> PathBuf {
        self.root.join(cache_file_name(key))
    }

    pub fn get(&self, key: &str) -> Result<PackL2CacheLookup, PackL2CacheError> {
        self.get_at(key, system_time_seconds(SystemTime::now())?)
    }

    pub fn get_at(
        &self,
        key: &str,
        now_epoch_seconds: u64,
    ) -> Result<PackL2CacheLookup, PackL2CacheError> {
        let path = self.entry_path(key);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(PackL2CacheLookup::Miss(PackL2CacheMiss {
                    key: key.to_owned(),
                    path,
                    reason: PackL2CacheMissReason::NotFound,
                }));
            }
            Err(error) => {
                return Err(PackL2CacheError::Io {
                    path,
                    operation: "read",
                    source: error,
                });
            }
        };

        let entry = match serde_json::from_slice::<PackL2CacheEntry>(&bytes) {
            Ok(entry) => entry,
            Err(error) => {
                return Ok(PackL2CacheLookup::Miss(PackL2CacheMiss {
                    key: key.to_owned(),
                    path,
                    reason: PackL2CacheMissReason::Corrupt(error.to_string()),
                }));
            }
        };

        if entry.schema != PACK_L2_CACHE_ENTRY_SCHEMA_V1 {
            return Ok(PackL2CacheLookup::Miss(PackL2CacheMiss {
                key: key.to_owned(),
                path,
                reason: PackL2CacheMissReason::Corrupt(format!(
                    "unexpected schema {}",
                    entry.schema
                )),
            }));
        }
        if entry.key != key {
            return Ok(PackL2CacheLookup::Miss(PackL2CacheMiss {
                key: key.to_owned(),
                path,
                reason: PackL2CacheMissReason::KeyMismatch {
                    stored_key: entry.key,
                },
            }));
        }
        if is_expired(
            entry.stored_at_epoch_seconds,
            now_epoch_seconds,
            self.options.max_age,
        ) {
            return Ok(PackL2CacheLookup::Miss(PackL2CacheMiss {
                key: key.to_owned(),
                path,
                reason: PackL2CacheMissReason::Expired {
                    stored_at_epoch_seconds: entry.stored_at_epoch_seconds,
                },
            }));
        }

        Ok(PackL2CacheLookup::Hit(PackL2CacheHit {
            key: entry.key,
            path,
            stored_at_epoch_seconds: entry.stored_at_epoch_seconds,
            pack_json: entry.pack_json,
            byte_len: bytes.len() as u64,
        }))
    }

    pub fn put(
        &self,
        key: &str,
        pack_json: &JsonValue,
    ) -> Result<PackL2WriteReport, PackL2CacheError> {
        self.put_at(key, pack_json, system_time_seconds(SystemTime::now())?)
    }

    pub fn put_at(
        &self,
        key: &str,
        pack_json: &JsonValue,
        stored_at_epoch_seconds: u64,
    ) -> Result<PackL2WriteReport, PackL2CacheError> {
        ensure_cache_dir(&self.root)?;
        let path = self.entry_path(key);
        let entry = PackL2CacheEntry {
            schema: PACK_L2_CACHE_ENTRY_SCHEMA_V1.to_owned(),
            key: key.to_owned(),
            stored_at_epoch_seconds,
            pack_json: pack_json.clone(),
        };
        let bytes = serde_json::to_vec(&entry).map_err(|source| PackL2CacheError::Json {
            path: path.clone(),
            operation: "serialize",
            source,
        })?;
        let temp_path = self.temp_path(key, stored_at_epoch_seconds);

        write_synced_file(&temp_path, &bytes)?;
        fs::rename(&temp_path, &path).map_err(|source| {
            let _ = fs::remove_file(&temp_path);
            PackL2CacheError::Io {
                path: path.clone(),
                operation: "rename",
                source,
            }
        })?;
        sync_directory(&self.root)?;
        let eviction = self.evict_best_effort_at(stored_at_epoch_seconds)?;

        Ok(PackL2WriteReport {
            key: key.to_owned(),
            path,
            byte_len: bytes.len() as u64,
            eviction,
        })
    }

    pub fn evict_best_effort(&self) -> Result<PackL2EvictionReport, PackL2CacheError> {
        self.evict_best_effort_at(system_time_seconds(SystemTime::now())?)
    }

    pub fn evict_best_effort_at(
        &self,
        now_epoch_seconds: u64,
    ) -> Result<PackL2EvictionReport, PackL2CacheError> {
        let mut report = PackL2EvictionReport::default();
        let mut candidates = Vec::new();
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(report),
            Err(error) => {
                return Err(PackL2CacheError::Io {
                    path: self.root.clone(),
                    operation: "read_dir",
                    source: error,
                });
            }
        };

        for entry in entries {
            let Ok(entry) = entry else {
                report.skipped = report.skipped.saturating_add(1);
                continue;
            };
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                report.skipped = report.skipped.saturating_add(1);
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let byte_len = metadata.len();
            report.bytes_before = report.bytes_before.saturating_add(byte_len);
            let modified_epoch_seconds = metadata
                .modified()
                .ok()
                .and_then(|modified| system_time_seconds(modified).ok())
                .unwrap_or(0);
            let expired = modified_epoch_seconds == 0
                || is_expired(
                    modified_epoch_seconds,
                    now_epoch_seconds,
                    self.options.max_age,
                );
            candidates.push(EvictionCandidate {
                path,
                byte_len,
                modified_epoch_seconds,
                expired,
            });
        }

        candidates.sort_by(|left, right| {
            left.expired
                .cmp(&right.expired)
                .reverse()
                .then_with(|| {
                    left.modified_epoch_seconds
                        .cmp(&right.modified_epoch_seconds)
                })
                .then_with(|| left.path.cmp(&right.path))
        });

        let mut bytes_current = report.bytes_before;
        for candidate in candidates {
            if !candidate.expired && bytes_current <= self.options.max_bytes {
                break;
            }
            match fs::remove_file(&candidate.path) {
                Ok(()) => {
                    report.removed = report.removed.saturating_add(1);
                    report.bytes_removed = report.bytes_removed.saturating_add(candidate.byte_len);
                    bytes_current = bytes_current.saturating_sub(candidate.byte_len);
                }
                Err(_) => {
                    report.skipped = report.skipped.saturating_add(1);
                }
            }
        }
        report.bytes_after = bytes_current;
        Ok(report)
    }

    fn temp_path(&self, key: &str, stored_at_epoch_seconds: u64) -> PathBuf {
        let process_id = std::process::id();
        self.root.join(format!(
            ".{}.{}.{}.tmp",
            cache_file_stem(key),
            process_id,
            stored_at_epoch_seconds
        ))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PackL2CacheLookup {
    Hit(PackL2CacheHit),
    Miss(PackL2CacheMiss),
}

impl PackL2CacheLookup {
    #[must_use]
    pub const fn is_hit(&self) -> bool {
        matches!(self, Self::Hit(_))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PackL2CacheHit {
    pub key: String,
    pub path: PathBuf,
    pub stored_at_epoch_seconds: u64,
    pub pack_json: JsonValue,
    pub byte_len: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackL2CacheMiss {
    pub key: String,
    pub path: PathBuf,
    pub reason: PackL2CacheMissReason,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackL2CacheMissReason {
    NotFound,
    Expired { stored_at_epoch_seconds: u64 },
    Corrupt(String),
    KeyMismatch { stored_key: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackL2WriteReport {
    pub key: String,
    pub path: PathBuf,
    pub byte_len: u64,
    pub eviction: PackL2EvictionReport,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PackL2EvictionReport {
    pub removed: u64,
    pub skipped: u64,
    pub bytes_before: u64,
    pub bytes_removed: u64,
    pub bytes_after: u64,
}

#[derive(Debug)]
pub enum PackL2CacheError {
    Io {
        path: PathBuf,
        operation: &'static str,
        source: io::Error,
    },
    Json {
        path: PathBuf,
        operation: &'static str,
        source: serde_json::Error,
    },
    TimeBeforeUnixEpoch {
        source: std::time::SystemTimeError,
    },
}

impl fmt::Display for PackL2CacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                path,
                operation,
                source,
            } => write!(
                formatter,
                "failed to {operation} pack L2 cache path {}: {source}",
                path.display()
            ),
            Self::Json {
                path,
                operation,
                source,
            } => write!(
                formatter,
                "failed to {operation} pack L2 cache JSON at {}: {source}",
                path.display()
            ),
            Self::TimeBeforeUnixEpoch { source } => {
                write!(formatter, "system time predates Unix epoch: {source}")
            }
        }
    }
}

impl std::error::Error for PackL2CacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
            Self::TimeBeforeUnixEpoch { source } => Some(source),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PackL2CacheEntry {
    schema: String,
    key: String,
    stored_at_epoch_seconds: u64,
    pack_json: JsonValue,
}

#[derive(Debug)]
struct EvictionCandidate {
    path: PathBuf,
    byte_len: u64,
    modified_epoch_seconds: u64,
    expired: bool,
}

fn cache_file_name(key: &str) -> String {
    format!("{}.json", cache_file_stem(key))
}

fn cache_file_stem(key: &str) -> String {
    blake3::hash(key.as_bytes()).to_hex().to_string()
}

fn is_expired(stored_at_epoch_seconds: u64, now_epoch_seconds: u64, max_age: Duration) -> bool {
    now_epoch_seconds.saturating_sub(stored_at_epoch_seconds) > max_age.as_secs()
}

fn system_time_seconds(time: SystemTime) -> Result<u64, PackL2CacheError> {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|source| PackL2CacheError::TimeBeforeUnixEpoch { source })
}

fn ensure_cache_dir(path: &Path) -> Result<(), PackL2CacheError> {
    fs::create_dir_all(path).map_err(|source| PackL2CacheError::Io {
        path: path.to_path_buf(),
        operation: "create_dir_all",
        source,
    })?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        PackL2CacheError::Io {
            path: path.to_path_buf(),
            operation: "set_permissions",
            source,
        }
    })?;
    Ok(())
}

fn write_synced_file(path: &Path, bytes: &[u8]) -> Result<(), PackL2CacheError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| PackL2CacheError::Io {
            path: path.to_path_buf(),
            operation: "open_temp",
            source,
        })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| PackL2CacheError::Io {
            path: path.to_path_buf(),
            operation: "write_sync",
            source,
        })?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
        PackL2CacheError::Io {
            path: path.to_path_buf(),
            operation: "set_file_permissions",
            source,
        }
    })?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), PackL2CacheError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| PackL2CacheError::Io {
            path: path.to_path_buf(),
            operation: "sync_dir",
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    type TestResult = Result<(), String>;

    fn cache(
        max_bytes: u64,
        max_age: Duration,
    ) -> Result<(tempfile::TempDir, PackL2Cache), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let cache = PackL2Cache::new(
            temp.path().join("pack-l2"),
            PackL2CacheOptions::new(max_bytes, max_age),
        );
        Ok((temp, cache))
    }

    fn hit_json(lookup: PackL2CacheLookup) -> Result<JsonValue, String> {
        match lookup {
            PackL2CacheLookup::Hit(hit) => Ok(hit.pack_json),
            PackL2CacheLookup::Miss(miss) => {
                Err(format!("expected hit, got miss: {:?}", miss.reason))
            }
        }
    }

    #[test]
    fn happy_path_roundtrip_returns_stored_pack_json() -> TestResult {
        let (_temp, cache) = cache(4096, Duration::from_secs(60))?;
        let pack = json!({"hash": "blake3:test", "items": [{"id": "mem_1"}]});

        let report = cache
            .put_at("blake3:key-a", &pack, 100)
            .map_err(|error| error.to_string())?;
        assert!(
            report.path.exists(),
            "write should publish final cache file"
        );

        let stored = hit_json(
            cache
                .get_at("blake3:key-a", 120)
                .map_err(|error| error.to_string())?,
        )?;
        assert_eq!(stored, pack, "cache hit should preserve pack JSON exactly");
        Ok(())
    }

    #[test]
    fn empty_or_boundary_missing_key_returns_not_found_miss() -> TestResult {
        let (_temp, cache) = cache(4096, Duration::from_secs(60))?;

        let lookup = cache
            .get_at("blake3:missing", 100)
            .map_err(|error| error.to_string())?;

        assert_eq!(
            lookup,
            PackL2CacheLookup::Miss(PackL2CacheMiss {
                key: "blake3:missing".to_owned(),
                path: cache.entry_path("blake3:missing"),
                reason: PackL2CacheMissReason::NotFound,
            })
        );
        Ok(())
    }

    #[test]
    fn empty_or_boundary_expired_entry_returns_expired_miss() -> TestResult {
        let (_temp, cache) = cache(4096, Duration::from_secs(10))?;
        cache
            .put_at("blake3:key-expired", &json!({"hash": "old"}), 100)
            .map_err(|error| error.to_string())?;

        let lookup = cache
            .get_at("blake3:key-expired", 111)
            .map_err(|error| error.to_string())?;

        assert_eq!(
            lookup,
            PackL2CacheLookup::Miss(PackL2CacheMiss {
                key: "blake3:key-expired".to_owned(),
                path: cache.entry_path("blake3:key-expired"),
                reason: PackL2CacheMissReason::Expired {
                    stored_at_epoch_seconds: 100,
                },
            })
        );
        Ok(())
    }

    #[test]
    fn error_or_invalid_corrupt_entry_returns_corrupt_miss() -> TestResult {
        let (_temp, cache) = cache(4096, Duration::from_secs(60))?;
        ensure_cache_dir(cache.root()).map_err(|error| error.to_string())?;
        fs::write(cache.entry_path("blake3:corrupt"), b"{not-json")
            .map_err(|error| error.to_string())?;

        let lookup = cache
            .get_at("blake3:corrupt", 100)
            .map_err(|error| error.to_string())?;

        match lookup {
            PackL2CacheLookup::Miss(miss) => {
                assert!(
                    matches!(miss.reason, PackL2CacheMissReason::Corrupt(_)),
                    "corrupt JSON should be a typed miss"
                );
            }
            PackL2CacheLookup::Hit(_) => return Err("corrupt entry must not hit".to_owned()),
        }
        Ok(())
    }

    #[test]
    fn error_or_invalid_key_mismatch_returns_miss() -> TestResult {
        let (_temp, cache) = cache(4096, Duration::from_secs(60))?;
        cache
            .put_at("blake3:original", &json!({"hash": "mismatch"}), 100)
            .map_err(|error| error.to_string())?;
        let original =
            fs::read(cache.entry_path("blake3:original")).map_err(|error| error.to_string())?;
        ensure_cache_dir(cache.root()).map_err(|error| error.to_string())?;
        fs::write(cache.entry_path("blake3:other"), original).map_err(|error| error.to_string())?;

        let lookup = cache
            .get_at("blake3:other", 100)
            .map_err(|error| error.to_string())?;

        match lookup {
            PackL2CacheLookup::Miss(miss) => assert_eq!(
                miss.reason,
                PackL2CacheMissReason::KeyMismatch {
                    stored_key: "blake3:original".to_owned()
                }
            ),
            PackL2CacheLookup::Hit(_) => return Err("mismatched key must not hit".to_owned()),
        }
        Ok(())
    }

    #[test]
    fn error_or_invalid_unwritable_root_reports_io_error() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let file_root = temp.path().join("not-a-directory");
        fs::write(&file_root, b"already a file").map_err(|error| error.to_string())?;
        let cache = PackL2Cache::new(file_root.clone(), PackL2CacheOptions::default());

        let error = cache
            .put_at("blake3:key", &json!({"hash": "nope"}), 100)
            .expect_err("file root should not be writable as a cache directory");

        match error {
            PackL2CacheError::Io {
                path,
                operation: "create_dir_all",
                ..
            } => assert_eq!(path, file_root),
            other => return Err(format!("unexpected error: {other}")),
        }
        Ok(())
    }

    #[test]
    fn eviction_removes_expired_entries_before_fresh_entries() -> TestResult {
        let (_temp, cache) = cache(10_000, Duration::from_secs(10))?;
        cache
            .put_at("blake3:old", &json!({"payload": "old"}), 100)
            .map_err(|error| error.to_string())?;
        cache
            .put_at("blake3:fresh", &json!({"payload": "fresh"}), 120)
            .map_err(|error| error.to_string())?;

        let report = cache
            .evict_best_effort_at(111)
            .map_err(|error| error.to_string())?;

        assert_eq!(report.removed, 1, "one expired entry should be removed");
        assert!(
            matches!(
                cache
                    .get_at("blake3:old", 111)
                    .map_err(|error| error.to_string())?,
                PackL2CacheLookup::Miss(PackL2CacheMiss {
                    reason: PackL2CacheMissReason::NotFound,
                    ..
                })
            ),
            "old entry should be gone"
        );
        assert!(
            cache
                .get_at("blake3:fresh", 111)
                .map_err(|error| error.to_string())?
                .is_hit(),
            "fresh entry should remain"
        );
        Ok(())
    }

    #[test]
    fn eviction_reduces_cache_to_byte_cap_by_oldest_first() -> TestResult {
        let (_temp, cache) = cache(170, Duration::from_secs(10_000))?;
        cache
            .put_at(
                "blake3:first",
                &json!({"payload": "aaaaaaaaaaaaaaaaaaaaaaaa"}),
                100,
            )
            .map_err(|error| error.to_string())?;
        cache
            .put_at(
                "blake3:second",
                &json!({"payload": "bbbbbbbbbbbbbbbbbbbbbbbb"}),
                200,
            )
            .map_err(|error| error.to_string())?;
        cache
            .put_at(
                "blake3:third",
                &json!({"payload": "cccccccccccccccccccccccc"}),
                300,
            )
            .map_err(|error| error.to_string())?;

        let report = cache
            .evict_best_effort_at(300)
            .map_err(|error| error.to_string())?;

        assert!(
            report.bytes_after <= cache.options().max_bytes,
            "eviction should reduce byte usage below the configured cap"
        );
        assert!(report.removed >= 1, "at least one entry should be evicted");
        assert!(
            matches!(
                cache
                    .get_at("blake3:first", 300)
                    .map_err(|error| error.to_string())?,
                PackL2CacheLookup::Miss(PackL2CacheMiss {
                    reason: PackL2CacheMissReason::NotFound,
                    ..
                })
            ),
            "oldest entry should be evicted first"
        );
        Ok(())
    }

    #[test]
    fn happy_path_cache_directory_uses_private_permissions() -> TestResult {
        let (_temp, cache) = cache(4096, Duration::from_secs(60))?;
        cache
            .put_at("blake3:key-a", &json!({"hash": "perms"}), 100)
            .map_err(|error| error.to_string())?;

        #[cfg(unix)]
        {
            let mode = fs::metadata(cache.root())
                .map_err(|error| error.to_string())?
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "cache directory should be owner-only");
        }
        Ok(())
    }
}
