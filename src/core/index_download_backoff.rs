//! Cross-process backoff for the first-use embedding model download.
//!
//! The lazy Model2Vec embedder downloads the bundled model the first time a
//! process needs semantic search. Its failure state was process-local, so on a
//! host that cannot reach the model registry (offline, firewalled CI, a proxy
//! that refuses the host) every `ee search`, `ee pack` and `ee remember` paid a
//! fresh failed download attempt -- about seven seconds each, measured on the
//! real-shape corpus probe -- and left another empty staging directory behind.
//!
//! A failed automatic attempt now records a small marker beside the model
//! root. Later processes inside the backoff window skip the network and use the
//! deterministic hash fallback at once, exactly as the failed process did. The
//! window doubles with each consecutive failure (10 minutes up to 24 hours).
//! An explicit `ee model fetch` never consults the marker, a successful load
//! clears it, and deleting the file retries immediately.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const MARKER_SCHEMA: &str = "ee.model.download_backoff.v1";
const BASE_BACKOFF: Duration = Duration::from_secs(10 * 60);
const MAX_BACKOFF: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_RECORDED_ERROR_CHARS: usize = 400;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct DownloadBackoffMarker {
    schema: String,
    model: String,
    failed_at_unix: u64,
    consecutive_failures: u32,
    error: String,
}

impl DownloadBackoffMarker {
    pub(super) fn retry_after_unix(&self) -> u64 {
        self.failed_at_unix
            .saturating_add(backoff_for(self.consecutive_failures).as_secs())
    }

    pub(super) fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    pub(super) fn error(&self) -> &str {
        &self.error
    }
}

/// Window after the `n`th consecutive failure: 10 min, 20 min, 40 min, ...
/// capped at 24 hours.
pub(super) fn backoff_for(consecutive_failures: u32) -> Duration {
    let doublings = consecutive_failures.saturating_sub(1).min(16);
    BASE_BACKOFF
        .saturating_mul(1_u32 << doublings)
        .min(MAX_BACKOFF)
}

pub(super) fn marker_path(model_root: &Path, model: &str) -> PathBuf {
    model_root.join(format!(".{model}.download-backoff.json"))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

pub(super) fn read_marker(model_root: &Path, model: &str) -> Option<DownloadBackoffMarker> {
    let bytes = std::fs::read(marker_path(model_root, model)).ok()?;
    let marker: DownloadBackoffMarker = serde_json::from_slice(&bytes).ok()?;
    (marker.schema == MARKER_SCHEMA && marker.model == model).then_some(marker)
}

/// The marker that should suppress an automatic download right now, if any.
/// A marker stamped in the future (clock moved backwards) does not suppress:
/// a skewed clock must never pin a host to the fallback for a day.
pub(super) fn active_marker(model_root: &Path, model: &str) -> Option<DownloadBackoffMarker> {
    active_marker_at(model_root, model, now_unix())
}

fn active_marker_at(model_root: &Path, model: &str, now: u64) -> Option<DownloadBackoffMarker> {
    let marker = read_marker(model_root, model)?;
    (marker.failed_at_unix <= now && now < marker.retry_after_unix()).then_some(marker)
}

/// Record a failed automatic download. Best effort: an unwritable model root
/// only loses the cross-process shortcut, never the command.
pub(super) fn record_failure(model_root: &Path, model: &str, error: &str) {
    record_failure_at(model_root, model, error, now_unix());
}

fn record_failure_at(model_root: &Path, model: &str, error: &str, now: u64) {
    let consecutive_failures = read_marker(model_root, model)
        .map_or(0, |previous| previous.consecutive_failures)
        .saturating_add(1);
    let marker = DownloadBackoffMarker {
        schema: MARKER_SCHEMA.to_owned(),
        model: model.to_owned(),
        failed_at_unix: now,
        consecutive_failures,
        error: error.chars().take(MAX_RECORDED_ERROR_CHARS).collect(),
    };
    let Ok(body) = serde_json::to_vec_pretty(&marker) else {
        return;
    };
    if std::fs::create_dir_all(model_root).is_err() {
        return;
    }
    let path = marker_path(model_root, model);
    let staging = path.with_extension(format!("json.{}.tmp", std::process::id()));
    if std::fs::write(&staging, body).is_ok() && std::fs::rename(&staging, &path).is_err() {
        let _ = std::fs::remove_file(&staging);
    }
}

pub(super) fn clear(model_root: &Path, model: &str) {
    let _ = std::fs::remove_file(marker_path(model_root, model));
}

/// Remove the download staging directories this process created under
/// `model_root` (`.<model>-download-<pid>-<suffix>`). Only this process's own
/// pid is matched, so a concurrent download by another process is untouched.
pub(super) fn remove_own_staging_dirs(model_root: &Path, model: &str) -> usize {
    let prefix = format!(".{model}-download-{}-", std::process::id());
    let Ok(entries) = std::fs::read_dir(model_root) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(&prefix) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir()
            && !file_type.is_symlink()
            && std::fs::remove_dir_all(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

/// Staging directories of other processes older than this are abandoned: a
/// live download writes its first file within seconds of creating its dir.
const ABANDONED_STAGING_AGE: Duration = Duration::from_secs(60 * 60);

/// Non-empty staging directories whose owning PID is dead and whose mtime is
/// older than 24 hours are abandoned partial downloads safe to reclaim.
const ABANDONED_NON_EMPTY_STAGING_AGE: Duration = Duration::from_secs(24 * 60 * 60);

const MAX_STAGING_WALK_DEPTH: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StagingPidLiveness {
    Alive,
    Dead,
    Unknown,
}

#[cfg(unix)]
fn staging_process_liveness(pid: u32) -> StagingPidLiveness {
    let Ok(raw_pid) = i32::try_from(pid) else {
        return StagingPidLiveness::Unknown;
    };
    let Some(pid_handle) = rustix::process::Pid::from_raw(raw_pid) else {
        return StagingPidLiveness::Unknown;
    };
    match rustix::process::test_kill_process(pid_handle) {
        Ok(()) => StagingPidLiveness::Alive,
        Err(error) if error == rustix::io::Errno::PERM => StagingPidLiveness::Alive,
        Err(error) if error == rustix::io::Errno::SRCH => StagingPidLiveness::Dead,
        Err(_) => StagingPidLiveness::Unknown,
    }
}

#[cfg(not(unix))]
fn staging_process_liveness(_pid: u32) -> StagingPidLiveness {
    StagingPidLiveness::Unknown
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StagingDirClass {
    ActiveOrFresh,
    AbandonedEmpty,
    AbandonedNonEmptyDeadPid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StagingDirEntry {
    pub model_name: String,
    pub dir_name: String,
    pub path: PathBuf,
    pub pid: u32,
    pub age_secs: u64,
    pub empty: bool,
    pub pid_liveness: StagingPidLiveness,
    pub bytes: u64,
    pub class: StagingDirClass,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct StagingDirInventory {
    pub active_or_fresh_count: usize,
    pub abandoned_empty_count: usize,
    pub abandoned_non_empty_dead_pid_count: usize,
    pub reclaimable_count: usize,
    pub reclaimable_bytes: u64,
    pub entries: Vec<StagingDirEntry>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct StagingReclaimSummary {
    pub inspected_count: usize,
    pub reclaimed_count: usize,
    pub reclaimed_bytes: u64,
    pub kept_count: usize,
    pub failed_count: usize,
}

fn default_model_cache_root() -> PathBuf {
    let env: std::collections::BTreeMap<String, std::ffi::OsString> = std::env::vars_os()
        .filter_map(|(key, value)| key.into_string().ok().map(|key| (key, value)))
        .collect();
    let ee_data_dir = if cfg!(windows) {
        crate::config::path_resolver::resolve_dir_windows_localappdata(&env)
            .ok()
            .map(|root| root.join("ee"))
    } else {
        crate::config::path_resolver::resolve_dir_unix_xdg(&env, "ee").ok()
    }
    .unwrap_or_else(|| std::env::temp_dir().join("ee"));
    ee_data_dir.join("models")
}

fn configured_embed_model_dir() -> Option<PathBuf> {
    crate::config::env_registry::read_os(crate::config::env_registry::EnvVar::EmbedModelDir)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[must_use]
pub(crate) fn staging_reclaim_root() -> PathBuf {
    configured_embed_model_dir().unwrap_or_else(default_model_cache_root)
}

#[must_use]
pub(crate) fn staging_search_roots_under(root: &Path) -> Vec<PathBuf> {
    let mut roots = vec![root.to_path_buf()];
    if configured_embed_model_dir().is_none() {
        let model2vec = root.join("model2vec");
        if model2vec.is_dir() && model2vec != root {
            roots.push(model2vec);
        }
    }
    roots
}

#[must_use]
pub(crate) fn staging_search_roots() -> Vec<PathBuf> {
    staging_search_roots_under(&staging_reclaim_root())
}

fn parse_staging_dir_name(name: &str) -> Option<(String, u32)> {
    let rest = name.strip_prefix('.')?;
    let (model_name, tail) = rest.split_once("-download-")?;
    if model_name.is_empty() {
        return None;
    }
    let (pid_str, seq) = tail.split_once('-')?;
    if seq.is_empty() {
        return None;
    }
    let pid = pid_str.parse::<u32>().ok()?;
    Some((model_name.to_owned(), pid))
}

fn measure_dir_contents(path: &Path, depth: usize) -> Option<(bool, u64)> {
    if depth > MAX_STAGING_WALK_DEPTH {
        return None;
    }
    let entries = std::fs::read_dir(path).ok()?;
    let mut empty = true;
    let mut total_bytes = 0u64;
    for entry in entries {
        let Ok(entry) = entry else {
            return None;
        };
        empty = false;
        let Ok(metadata) = std::fs::symlink_metadata(entry.path()) else {
            return None;
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_file() {
            total_bytes = total_bytes.saturating_add(metadata.len());
        } else if metadata.is_dir() {
            let (_, child_bytes) = measure_dir_contents(&entry.path(), depth + 1)?;
            total_bytes = total_bytes.saturating_add(child_bytes);
        }
    }
    Some((empty, total_bytes))
}

#[must_use]
pub(crate) fn inspect_staging_dirs_default() -> StagingDirInventory {
    let roots = staging_search_roots();
    inspect_staging_dirs_in_roots_at(&roots, SystemTime::now())
}

pub(crate) fn inspect_staging_dirs_in_roots_at(
    roots: &[PathBuf],
    now: SystemTime,
) -> StagingDirInventory {
    let mut inventory = StagingDirInventory::default();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let name_os = entry.file_name();
            let Some(dir_name) = name_os.to_str() else {
                continue;
            };
            let Some((model_name, pid)) = parse_staging_dir_name(dir_name) else {
                continue;
            };
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                continue;
            }
            let age = metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok());
            let age_secs = age.map_or(0, |duration| duration.as_secs());
            let pid_liveness = staging_process_liveness(pid);
            let (empty, bytes, unreadable) = match measure_dir_contents(&path, 0) {
                Some((empty, bytes)) => (empty, bytes, false),
                None => (false, 0, true),
            };

            let class = if unreadable {
                StagingDirClass::ActiveOrFresh
            } else if empty && age.is_some_and(|d| d >= ABANDONED_STAGING_AGE) {
                StagingDirClass::AbandonedEmpty
            } else if !empty
                && age.is_some_and(|d| d >= ABANDONED_NON_EMPTY_STAGING_AGE)
                && pid_liveness == StagingPidLiveness::Dead
            {
                StagingDirClass::AbandonedNonEmptyDeadPid
            } else {
                StagingDirClass::ActiveOrFresh
            };

            match class {
                StagingDirClass::ActiveOrFresh => {
                    inventory.active_or_fresh_count =
                        inventory.active_or_fresh_count.saturating_add(1);
                }
                StagingDirClass::AbandonedEmpty => {
                    inventory.abandoned_empty_count =
                        inventory.abandoned_empty_count.saturating_add(1);
                    inventory.reclaimable_count = inventory.reclaimable_count.saturating_add(1);
                    inventory.reclaimable_bytes = inventory.reclaimable_bytes.saturating_add(bytes);
                }
                StagingDirClass::AbandonedNonEmptyDeadPid => {
                    inventory.abandoned_non_empty_dead_pid_count = inventory
                        .abandoned_non_empty_dead_pid_count
                        .saturating_add(1);
                    inventory.reclaimable_count = inventory.reclaimable_count.saturating_add(1);
                    inventory.reclaimable_bytes = inventory.reclaimable_bytes.saturating_add(bytes);
                }
            }

            inventory.entries.push(StagingDirEntry {
                model_name,
                dir_name: dir_name.to_owned(),
                path,
                pid,
                age_secs,
                empty,
                pid_liveness,
                bytes,
                class,
            });
        }
    }
    inventory
        .entries
        .sort_by(|left, right| left.path.cmp(&right.path));
    inventory
}

pub(crate) fn reclaim_abandoned_staging_dirs(root: &Path, dry_run: bool) -> StagingReclaimSummary {
    let roots = staging_search_roots_under(root);
    reclaim_abandoned_staging_dirs_in_roots_at(&roots, dry_run, SystemTime::now())
}

fn reclaim_abandoned_staging_dirs_in_roots_at(
    roots: &[PathBuf],
    dry_run: bool,
    now: SystemTime,
) -> StagingReclaimSummary {
    let inventory = inspect_staging_dirs_in_roots_at(roots, now);
    let mut summary = StagingReclaimSummary {
        inspected_count: inventory.entries.len(),
        ..StagingReclaimSummary::default()
    };

    for entry in &inventory.entries {
        let pid_alive = matches!(entry.pid_liveness, StagingPidLiveness::Alive);
        let (action, bytes_reclaimed) = match entry.class {
            StagingDirClass::ActiveOrFresh => {
                summary.kept_count = summary.kept_count.saturating_add(1);
                ("keep", 0u64)
            }
            StagingDirClass::AbandonedEmpty => {
                if dry_run {
                    summary.reclaimed_count = summary.reclaimed_count.saturating_add(1);
                    ("dry_run_reclaim_empty", 0u64)
                } else if std::fs::remove_dir(&entry.path).is_ok() {
                    summary.reclaimed_count = summary.reclaimed_count.saturating_add(1);
                    summary.reclaimed_bytes = summary.reclaimed_bytes.saturating_add(entry.bytes);
                    ("reclaim_empty", entry.bytes)
                } else {
                    summary.failed_count = summary.failed_count.saturating_add(1);
                    ("reclaim_failed", 0u64)
                }
            }
            StagingDirClass::AbandonedNonEmptyDeadPid => {
                if dry_run {
                    summary.reclaimed_count = summary.reclaimed_count.saturating_add(1);
                    summary.reclaimed_bytes = summary.reclaimed_bytes.saturating_add(entry.bytes);
                    ("dry_run_reclaim_dead_pid", entry.bytes)
                } else if std::fs::remove_dir_all(&entry.path).is_ok() {
                    summary.reclaimed_count = summary.reclaimed_count.saturating_add(1);
                    summary.reclaimed_bytes = summary.reclaimed_bytes.saturating_add(entry.bytes);
                    ("reclaim_dead_pid", entry.bytes)
                } else {
                    summary.failed_count = summary.failed_count.saturating_add(1);
                    ("reclaim_failed", 0u64)
                }
            }
        };

        tracing::info!(
            target: "ee::model::staging",
            schema = "ee.test_event.v1",
            model_name = %entry.model_name,
            dir_name = %entry.dir_name,
            age_secs = entry.age_secs,
            empty = entry.empty,
            pid_alive,
            action,
            bytes_reclaimed,
            "model download staging directory evaluated"
        );
    }

    summary
}

/// Remove EMPTY `.<model>-download-*` staging directories older than
/// [`ABANDONED_STAGING_AGE`], whatever process made them (bd-4b3j2).
///
/// The downloader moves each verified file out of its staging directory and
/// never removes the directory itself, so every acquisition -- successful or
/// not -- left one empty directory behind (414 on one host). `remove_dir`
/// refuses a non-empty directory, and the age guard spares a concurrent
/// download that has just created its own.
pub(super) fn sweep_abandoned_staging_dirs(model_root: &Path, model: &str) -> usize {
    sweep_abandoned_staging_dirs_at(model_root, model, SystemTime::now())
}

fn sweep_abandoned_staging_dirs_at(model_root: &Path, model: &str, now: SystemTime) -> usize {
    let prefix = format!(".{model}-download-");
    let Ok(entries) = std::fs::read_dir(model_root) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_str().is_some_and(|name| name.starts_with(&prefix)) {
            continue;
        }
        let Ok(metadata) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        let abandoned = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= ABANDONED_STAGING_AGE);
        if abandoned && std::fs::remove_dir(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn temp_root(name: &str) -> Result<tempfile::TempDir, String> {
        tempfile::Builder::new()
            .prefix(name)
            .tempdir()
            .map_err(|error| error.to_string())
    }

    #[test]
    fn backoff_doubles_from_ten_minutes_and_caps_at_a_day() {
        assert_eq!(backoff_for(0), Duration::from_secs(600));
        assert_eq!(backoff_for(1), Duration::from_secs(600));
        assert_eq!(backoff_for(2), Duration::from_secs(1200));
        assert_eq!(backoff_for(3), Duration::from_secs(2400));
        assert_eq!(backoff_for(9), MAX_BACKOFF);
        assert_eq!(backoff_for(u32::MAX), MAX_BACKOFF);
    }

    #[test]
    fn failure_marker_suppresses_only_inside_its_window() -> TestResult {
        let root = temp_root("ee-download-backoff-")?;
        let model = "potion-test";
        assert!(active_marker_at(root.path(), model, 1_000).is_none());

        record_failure_at(root.path(), model, "connect refused", 1_000);
        let marker =
            active_marker_at(root.path(), model, 1_001).ok_or("marker should be active")?;
        assert_eq!(marker.consecutive_failures(), 1);
        assert_eq!(marker.error(), "connect refused");
        assert_eq!(marker.retry_after_unix(), 1_600);
        assert!(active_marker_at(root.path(), model, 1_600).is_none());
        // A clock that moved backwards never pins the host to the fallback.
        assert!(active_marker_at(root.path(), model, 999).is_none());

        record_failure_at(root.path(), model, "connect refused", 2_000);
        let marker = read_marker(root.path(), model).ok_or("marker should persist")?;
        assert_eq!(marker.consecutive_failures(), 2);
        assert_eq!(marker.retry_after_unix(), 3_200);

        clear(root.path(), model);
        assert!(read_marker(root.path(), model).is_none());
        Ok(())
    }

    #[test]
    fn marker_for_another_model_or_schema_is_ignored() -> TestResult {
        let root = temp_root("ee-download-backoff-other-")?;
        record_failure_at(root.path(), "model-a", "boom", 10);
        std::fs::copy(
            marker_path(root.path(), "model-a"),
            marker_path(root.path(), "model-b"),
        )
        .map_err(|error| error.to_string())?;
        assert!(active_marker_at(root.path(), "model-b", 11).is_none());
        std::fs::write(marker_path(root.path(), "model-a"), b"{\"schema\":\"x\"}")
            .map_err(|error| error.to_string())?;
        assert!(active_marker_at(root.path(), "model-a", 11).is_none());
        Ok(())
    }

    #[test]
    fn recorded_error_is_bounded() -> TestResult {
        let root = temp_root("ee-download-backoff-long-")?;
        record_failure_at(root.path(), "m", &"x".repeat(10_000), 5);
        let marker = read_marker(root.path(), "m").ok_or("marker should exist")?;
        assert_eq!(marker.error().chars().count(), MAX_RECORDED_ERROR_CHARS);
        Ok(())
    }

    #[test]
    fn sweep_removes_only_empty_abandoned_staging_dirs() -> TestResult {
        let root = temp_root("ee-download-sweep-")?;
        let model = "potion-test";
        let empty = root
            .path()
            .join(format!(".{model}-download-7-0000000000000001"));
        let busy = root
            .path()
            .join(format!(".{model}-download-8-0000000000000002"));
        let other = root.path().join(".other-model-download-9-0000000000000003");
        for dir in [&empty, &busy, &other] {
            std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        }
        std::fs::write(busy.join("model.safetensors.part"), b"x")
            .map_err(|error| error.to_string())?;

        // Fresh directories belong to downloads that may still be running.
        assert_eq!(
            sweep_abandoned_staging_dirs_at(root.path(), model, SystemTime::now()),
            0
        );
        let later = SystemTime::now() + ABANDONED_STAGING_AGE + Duration::from_secs(1);
        assert_eq!(
            sweep_abandoned_staging_dirs_at(root.path(), model, later),
            1
        );
        assert!(!empty.exists());
        assert!(busy.exists(), "a staging dir with content is never removed");
        assert!(other.exists(), "another model's staging dirs are untouched");
        Ok(())
    }

    #[test]
    fn staging_cleanup_removes_only_this_process_dirs() -> TestResult {
        let root = temp_root("ee-download-staging-")?;
        let model = "potion-test";
        let own = root.path().join(format!(
            ".{model}-download-{}-0000000000000000",
            std::process::id()
        ));
        let other = root
            .path()
            .join(format!(".{model}-download-{}-0000000000000000", u32::MAX));
        let installed = root.path().join(model);
        for dir in [&own, &other, &installed] {
            std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        }
        std::fs::write(
            own.join("model.safetensors.part"),
            b"partial-cancelled-bytes",
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(remove_own_staging_dirs(root.path(), model), 1);
        assert!(!own.exists());
        assert!(other.exists());
        assert!(installed.exists());
        Ok(())
    }

    #[test]
    fn parse_staging_dir_name_validates_format() {
        assert_eq!(
            parse_staging_dir_name(".potion-multilingual-128M-download-12345-0000000000000001"),
            Some(("potion-multilingual-128M".to_owned(), 12345))
        );
        assert_eq!(parse_staging_dir_name("potion-multilingual-128M"), None);
        assert_eq!(
            parse_staging_dir_name(".potion-multilingual-128M-download-notapid-1"),
            None
        );
        assert_eq!(
            parse_staging_dir_name(".potion-multilingual-128M-download-123-"),
            None
        );
    }

    #[test]
    fn reclaim_preserves_fresh_live_pid_symlink_and_installed_model_dirs() -> TestResult {
        let root = temp_root("ee-staging-preserve-")?;
        let model = "potion-multilingual-128M";
        let live_pid = std::process::id();

        let fresh_empty = root
            .path()
            .join(format!(".{model}-download-{live_pid}-0000000000000001"));
        let live_non_empty = root
            .path()
            .join(format!(".{model}-download-{live_pid}-0000000000000002"));
        let unknown_pid_non_empty = root
            .path()
            .join(format!(".{model}-download-{}-0000000000000003", u32::MAX));
        let installed = root.path().join(model);
        for dir in [
            &fresh_empty,
            &live_non_empty,
            &unknown_pid_non_empty,
            &installed,
        ] {
            std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        }
        std::fs::write(live_non_empty.join("model.safetensors.part"), b"live-bytes")
            .map_err(|error| error.to_string())?;
        std::fs::write(
            unknown_pid_non_empty.join("model.safetensors.part"),
            b"unknown-pid-bytes",
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(installed.join("model.safetensors"), b"installed")
            .map_err(|error| error.to_string())?;

        #[cfg(unix)]
        let symlink_staging = {
            let link = root
                .path()
                .join(format!(".{model}-download-999999-0000000000000004"));
            std::os::unix::fs::symlink(&installed, &link).map_err(|error| error.to_string())?;
            link
        };

        let roots = vec![root.path().to_path_buf()];
        // At now: even empty dir is fresh (< 1h).
        let now_inv = inspect_staging_dirs_in_roots_at(&roots, SystemTime::now());
        assert_eq!(now_inv.active_or_fresh_count, 3);
        assert_eq!(now_inv.reclaimable_count, 0);

        // After > 24h: live-PID non-empty and unknown-PID non-empty MUST still stay ActiveOrFresh.
        let future = SystemTime::now() + ABANDONED_NON_EMPTY_STAGING_AGE + Duration::from_secs(60);
        let summary = reclaim_abandoned_staging_dirs_in_roots_at(&roots, false, future);
        // Only `fresh_empty` (now > 1h old and empty) is reclaimed; live_non_empty and unknown_pid_non_empty are kept.
        assert_eq!(summary.reclaimed_count, 1);
        assert_eq!(summary.kept_count, 2);
        assert!(!fresh_empty.exists());
        assert!(
            live_non_empty.exists(),
            "live PID non-empty dir must survive"
        );
        assert!(
            unknown_pid_non_empty.exists(),
            "unknown PID non-empty dir must survive"
        );
        assert!(installed.join("model.safetensors").is_file());
        #[cfg(unix)]
        assert!(
            symlink_staging.exists(),
            "symlink staging dir must stay untouched"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn reclaim_classifies_and_reclaims_empty_and_aged_dead_pid_dirs() -> TestResult {
        let root = temp_root("ee-staging-reclaim-")?;
        let model = "potion-multilingual-128M";

        // Spawn a trivial child and reap it so its PID is guaranteed dead (Errno::SRCH).
        let mut child = std::process::Command::new("true")
            .spawn()
            .map_err(|error| error.to_string())?;
        let dead_pid = child.id();
        let _ = child.wait().map_err(|error| error.to_string())?;

        let abandoned_empty = root
            .path()
            .join(format!(".{model}-download-{dead_pid}-0000000000000001"));
        let abandoned_dead_non_empty = root
            .path()
            .join(format!(".{model}-download-{dead_pid}-0000000000000002"));
        let model2vec_sub = root.path().join("model2vec");
        let nested_empty =
            model2vec_sub.join(format!(".{model}-download-{dead_pid}-0000000000000003"));
        for dir in [&abandoned_empty, &abandoned_dead_non_empty, &nested_empty] {
            std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        }
        let payload = b"0123456789abcdef";
        std::fs::write(
            abandoned_dead_non_empty.join("model.safetensors.part"),
            payload,
        )
        .map_err(|error| error.to_string())?;

        let roots = vec![root.path().to_path_buf(), model2vec_sub];
        // At 2h: empty dirs are AbandonedEmpty, non-empty dead-PID dir is <24h so still ActiveOrFresh.
        let two_hours = SystemTime::now() + Duration::from_secs(2 * 60 * 60);
        let inv_2h = inspect_staging_dirs_in_roots_at(&roots, two_hours);
        assert_eq!(inv_2h.abandoned_empty_count, 2);
        assert_eq!(inv_2h.abandoned_non_empty_dead_pid_count, 0);
        assert_eq!(inv_2h.active_or_fresh_count, 1);

        // At 25h: dry-run sees all 3 reclaimable without deleting.
        let twenty_five_hours = SystemTime::now() + Duration::from_secs(25 * 60 * 60);
        let dry = reclaim_abandoned_staging_dirs_in_roots_at(&roots, true, twenty_five_hours);
        assert_eq!(dry.reclaimed_count, 3);
        assert_eq!(dry.reclaimed_bytes, payload.len() as u64);
        assert!(abandoned_empty.exists());
        assert!(abandoned_dead_non_empty.exists());
        assert!(nested_empty.exists());

        // Real reclaim deletes all 3 and accounts exact bytes.
        let real = reclaim_abandoned_staging_dirs_in_roots_at(&roots, false, twenty_five_hours);
        assert_eq!(real.reclaimed_count, 3);
        assert_eq!(real.reclaimed_bytes, payload.len() as u64);
        assert_eq!(real.failed_count, 0);
        assert!(!abandoned_empty.exists());
        assert!(!abandoned_dead_non_empty.exists());
        assert!(!nested_empty.exists());
        Ok(())
    }
}
