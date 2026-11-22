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
    absolute_staging_reclaim_root(
        configured_embed_model_dir().unwrap_or_else(default_model_cache_root),
    )
}

fn absolute_staging_reclaim_root(root: PathBuf) -> PathBuf {
    // Model loading accepts relative overrides, but audited repair requires an
    // absolute path so undo cannot target a different cwd. Do not canonicalize:
    // the lifecycle guard must still see and reject a symlinked model root.
    // If cwd cannot be resolved, retain the relative path so repair fails closed
    // at its existing absolute-path check instead of guessing another store.
    std::path::absolute(&root).unwrap_or(root)
}

#[must_use]
pub(crate) fn staging_search_roots_under(root: &Path) -> Vec<PathBuf> {
    let mut roots = vec![root.to_path_buf()];
    let model2vec = root.join("model2vec");
    if model2vec != root
        && std::fs::symlink_metadata(&model2vec)
            .is_ok_and(|meta| !meta.file_type().is_symlink() && meta.is_dir())
    {
        roots.push(model2vec);
    }
    roots
}

fn direct_model_destination_name(root: &Path) -> Option<&str> {
    let name = root.file_name()?.to_str()?;
    let model = frankensearch::embed::RegisteredModel2Vec::builtin(name)?.ok()?;
    (super::index::model2vec_model_destination_dir(root, &model) == root).then_some(name)
}

fn direct_model_staging_parent(root: &Path) -> Option<PathBuf> {
    if !root.is_absolute()
        || root.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
        || super::path_safety::first_existing_symlink_component(root)
            .ok()?
            .is_some()
    {
        return None;
    }
    match std::fs::symlink_metadata(root) {
        Ok(metadata) if !metadata.is_dir() => return None,
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return None,
        _ => {}
    }
    let parent = root.parent()?;
    if super::path_safety::first_existing_symlink_component(parent)
        .ok()?
        .is_some()
        || !std::fs::symlink_metadata(parent).ok()?.is_dir()
    {
        return None;
    }
    parent.canonicalize().ok()
}

/// Match only one canonical model destination's staging siblings. The parent
/// directory is never an authorized repair root. Missing staging paths remain
/// matchable so an audited quarantine can be undone without broadening scope.
#[must_use]
pub(crate) fn direct_model_staging_path_is_authorized(root: &Path, path: &Path) -> bool {
    let Some(model_name) = direct_model_destination_name(root) else {
        return false;
    };
    if !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
        || !path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(parse_staging_dir_name)
            .is_some_and(|(name, _)| name == model_name)
    {
        return false;
    }
    let Some(parent) = direct_model_staging_parent(root) else {
        return false;
    };
    let Some(candidate_parent) = path.parent() else {
        return false;
    };
    if (Some(candidate_parent) != root.parent() && candidate_parent != parent.as_path())
        || !matches!(
            super::path_safety::first_existing_symlink_component(path),
            Ok(None)
        )
    {
        return false;
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_dir() => return false,
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return false,
        _ => {}
    }
    candidate_parent
        .canonicalize()
        .ok()
        .is_some_and(|candidate_parent| candidate_parent == parent)
}

fn parse_staging_dir_name(name: &str) -> Option<(String, u32)> {
    let rest = name.strip_prefix('.')?;
    let (model_name, tail) = rest.split_once("-download-")?;
    if model_name.is_empty() {
        return None;
    }
    let (pid_str, seq) = tail.split_once('-')?;
    if seq.is_empty() || pid_str.is_empty() || !pid_str.bytes().all(|b| b.is_ascii_digit()) {
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
    inspect_staging_dirs_under(&staging_reclaim_root())
}

#[must_use]
pub(crate) fn inspect_staging_dirs_under(root: &Path) -> StagingDirInventory {
    inspect_staging_dirs_under_at(root, SystemTime::now())
}

fn inspect_staging_dirs_under_at(root: &Path, now: SystemTime) -> StagingDirInventory {
    // A canonical direct destination may also contain another model's cache.
    // Preserve ordinary in-root discovery and filter only the added parent lane.
    let mut roots = staging_search_roots_under(root);
    let direct_parent = direct_model_destination_name(root)
        .and_then(|name| direct_model_staging_parent(root).map(|parent| (parent, name)));
    if let Some((parent, model_name)) = direct_parent {
        roots.push(parent.clone());
        inspect_staging_dirs_matching_at(&roots, now, Some((parent.as_path(), model_name)))
    } else {
        inspect_staging_dirs_in_roots_at(&roots, now)
    }
}

pub(crate) fn inspect_staging_dirs_in_roots_at(
    roots: &[PathBuf],
    now: SystemTime,
) -> StagingDirInventory {
    inspect_staging_dirs_matching_at(roots, now, None)
}

fn inspect_staging_dirs_matching_at(
    roots: &[PathBuf],
    now: SystemTime,
    model_name_filter: Option<(&Path, &str)>,
) -> StagingDirInventory {
    let mut inventory = StagingDirInventory::default();
    for root in roots {
        if !std::fs::symlink_metadata(root)
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        {
            continue;
        }
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
            if model_name_filter.is_some_and(|(parent, expected)| {
                root.as_path() == parent && model_name != expected
            }) {
                continue;
            }
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

            let class = if unreadable || pid_liveness != StagingPidLiveness::Dead {
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

pub(crate) fn reclaim_abandoned_staging_dirs<E>(
    root: &Path,
    dry_run: bool,
    quarantine: impl FnMut(&StagingDirEntry) -> Result<(), E>,
) -> Result<StagingReclaimSummary, E> {
    reclaim_abandoned_staging_dirs_at(root, dry_run, SystemTime::now(), quarantine)
}

fn reclaim_abandoned_staging_dirs_at<E>(
    root: &Path,
    dry_run: bool,
    now: SystemTime,
    mut quarantine: impl FnMut(&StagingDirEntry) -> Result<(), E>,
) -> Result<StagingReclaimSummary, E> {
    let inventory = inspect_staging_dirs_under_at(root, now);
    let mut summary = StagingReclaimSummary {
        inspected_count: inventory.entries.len(),
        ..StagingReclaimSummary::default()
    };

    for entry in &inventory.entries {
        let pid_alive = matches!(entry.pid_liveness, StagingPidLiveness::Alive);
        let (action, bytes_quarantined) = match entry.class {
            StagingDirClass::ActiveOrFresh => {
                summary.kept_count = summary.kept_count.saturating_add(1);
                ("keep", 0u64)
            }
            StagingDirClass::AbandonedEmpty | StagingDirClass::AbandonedNonEmptyDeadPid => {
                if dry_run {
                    summary.reclaimed_count = summary.reclaimed_count.saturating_add(1);
                    summary.reclaimed_bytes = summary.reclaimed_bytes.saturating_add(entry.bytes);
                    ("dry_run_quarantine", entry.bytes)
                } else {
                    // Inventory collection can span many model directories.
                    // Recheck liveness and type immediately before handing the
                    // candidate to the audited doctor mutation chokepoint.
                    if staging_process_liveness(entry.pid) != StagingPidLiveness::Dead
                        || !std::fs::symlink_metadata(&entry.path)
                            .is_ok_and(|meta| !meta.file_type().is_symlink() && meta.is_dir())
                        || (!entry.path.starts_with(root)
                            && !direct_model_staging_path_is_authorized(root, &entry.path))
                    {
                        summary.kept_count = summary.kept_count.saturating_add(1);
                        ("keep_changed", 0u64)
                    } else {
                        quarantine(entry)?;
                        summary.reclaimed_count = summary.reclaimed_count.saturating_add(1);
                        summary.reclaimed_bytes =
                            summary.reclaimed_bytes.saturating_add(entry.bytes);
                        ("quarantine", entry.bytes)
                    }
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
            bytes_quarantined,
            bytes_reclaimed = 0u64,
            "model download staging directory evaluated"
        );
    }

    Ok(summary)
}

/// Remove EMPTY `.<model>-download-*` staging directories older than
/// [`ABANDONED_STAGING_AGE`] whose owning process is confirmed dead (bd-4b3j2).
///
/// The downloader moves each verified file out of its staging directory and
/// never removes the directory itself, so every acquisition -- successful or
/// not -- left one empty directory behind (414 on one host). `remove_dir`
/// refuses a non-empty directory. Age alone cannot establish abandonment:
/// paused downloads can retain empty directories, so live or unknown PIDs
/// are preserved regardless of age.
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
        let Some(name) = name.to_str().filter(|name| name.starts_with(&prefix)) else {
            continue;
        };
        let Some((_, pid)) = parse_staging_dir_name(name) else {
            continue;
        };
        if staging_process_liveness(pid) != StagingPidLiveness::Dead {
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

    #[cfg(unix)]
    #[test]
    fn sweep_removes_only_empty_abandoned_staging_dirs() -> TestResult {
        let root = temp_root("ee-download-sweep-")?;
        let model = "potion-test";
        let mut child = std::process::Command::new("true")
            .spawn()
            .map_err(|error| error.to_string())?;
        let dead_pid = child.id();
        child.wait().map_err(|error| error.to_string())?;
        let empty = root
            .path()
            .join(format!(".{model}-download-{dead_pid}-0000000000000001"));
        let busy = root
            .path()
            .join(format!(".{model}-download-{dead_pid}-0000000000000002"));
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
    fn automatic_sweep_preserves_old_live_and_unknown_pid_empty_dirs() -> TestResult {
        let root = temp_root("ee-download-live-sweep-")?;
        let model = "potion-test";
        let live = root.path().join(format!(
            ".{model}-download-{}-0000000000000001",
            std::process::id()
        ));
        let unknown = root
            .path()
            .join(format!(".{model}-download-{}-0000000000000002", u32::MAX));
        for dir in [&live, &unknown] {
            std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        }
        let later = SystemTime::now() + Duration::from_secs(25 * 60 * 60);
        assert_eq!(
            sweep_abandoned_staging_dirs_at(root.path(), model, later),
            0
        );
        assert!(live.is_dir());
        assert!(unknown.is_dir());
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
        assert_eq!(
            parse_staging_dir_name(".potion-multilingual-128M-download-+123-1"),
            None
        );
    }

    #[test]
    fn relative_staging_reclaim_root_uses_process_working_directory() -> TestResult {
        let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
        let relative = PathBuf::from("ee-relative-model-cache").join("models");

        let resolved = absolute_staging_reclaim_root(relative.clone());

        assert!(resolved.is_absolute());
        assert_eq!(resolved, cwd.join(relative));
        Ok(())
    }

    #[test]
    fn absolute_staging_reclaim_root_preserves_missing_directory() -> TestResult {
        let root = temp_root("ee-staging-root-")?;
        let missing = root.path().join("not-created").join("models");
        assert!(!missing.exists());

        assert_eq!(absolute_staging_reclaim_root(missing.clone()), missing);
        assert!(!missing.exists(), "root resolution must not create directories");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn absolute_staging_reclaim_root_does_not_follow_symlinks() -> TestResult {
        let root = temp_root("ee-staging-root-symlink-")?;
        let target = root.path().join("actual-models");
        let link = root.path().join("configured-models");
        std::fs::create_dir(&target).map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&target, &link).map_err(|error| error.to_string())?;

        let resolved = absolute_staging_reclaim_root(link.clone());

        assert_eq!(resolved, link);
        assert!(
            std::fs::symlink_metadata(&resolved)
                .map_err(|error| error.to_string())?
                .file_type()
                .is_symlink(),
            "the audited repair guard must retain visibility of the symlink"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn staging_search_roots_under_skips_symlinked_model2vec() -> TestResult {
        let root = temp_root("ee-staging-symlink-m2v-")?;
        let external = root.path().join("external");
        std::fs::create_dir_all(&external).map_err(|error| error.to_string())?;
        let model_root = root.path().join("models");
        std::fs::create_dir_all(&model_root).map_err(|error| error.to_string())?;
        let link = model_root.join("model2vec");
        std::os::unix::fs::symlink(&external, &link).map_err(|error| error.to_string())?;

        assert_eq!(staging_search_roots_under(&model_root), vec![model_root]);
        Ok(())
    }

    #[test]
    fn direct_model_destination_recognition_uses_registered_canonical_names() -> TestResult {
        let root = temp_root("ee-staging-model-names-")?;
        for name in ["potion-multilingual-128M", "potion-base-8m", "potion-base-32m"] {
            let destination = root.path().join(name);
            assert_eq!(direct_model_destination_name(&destination), Some(name));
        }
        for name in ["models", "model2vec", "custom-potion", "potion-base-8M"] {
            assert_eq!(direct_model_destination_name(&root.path().join(name)), None);
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn direct_model_reclaim_preserves_other_models_and_active_downloads() -> TestResult {
        let root = temp_root("ee-staging-direct-")?;
        let parent = root.path().canonicalize().map_err(|error| error.to_string())?;
        let model = "potion-multilingual-128M";
        let installed = parent.join(model);
        let mut child = std::process::Command::new("true")
            .spawn()
            .map_err(|error| error.to_string())?;
        let dead_pid = child.id();
        child.wait().map_err(|error| error.to_string())?;
        let empty = parent.join(format!(".{model}-download-{dead_pid}-empty"));
        let partial = parent.join(format!(".{model}-download-{dead_pid}-partial"));
        let fresh = parent.join(format!(".{model}-download-{dead_pid}-fresh"));
        let live = parent.join(format!(".{model}-download-{}-live", std::process::id()));
        let unknown = parent.join(format!(".{model}-download-{}-unknown", u32::MAX));
        let other = parent.join(format!(".potion-base-8m-download-{dead_pid}-other"));
        let nested = installed.join(format!(".potion-base-8m-download-{dead_pid}-nested"));
        let legacy_root = installed.join("model2vec");
        let legacy = legacy_root.join(format!(".potion-base-32m-download-{dead_pid}-legacy"));
        for path in [
            &installed,
            &empty,
            &partial,
            &fresh,
            &live,
            &unknown,
            &other,
            &nested,
            &legacy_root,
            &legacy,
        ] {
            std::fs::create_dir(path).map_err(|error| error.to_string())?;
        }
        std::fs::write(installed.join("model.safetensors"), b"installed")
            .map_err(|error| error.to_string())?;
        std::fs::write(partial.join("model.safetensors.part"), b"partial")
            .map_err(|error| error.to_string())?;
        let now = SystemTime::now();
        let old = now - Duration::from_secs(25 * 60 * 60);
        for path in [&empty, &partial, &live, &unknown, &other, &nested, &legacy] {
            std::fs::File::open(path)
                .and_then(|file| file.set_times(std::fs::FileTimes::new().set_modified(old)))
                .map_err(|error| error.to_string())?;
        }

        let inventory = inspect_staging_dirs_under_at(&installed, now);
        assert_eq!(inventory.entries.len(), 7);
        assert_eq!(inventory.active_or_fresh_count, 3);
        assert_eq!(inventory.abandoned_empty_count, 3);
        assert_eq!(inventory.abandoned_non_empty_dead_pid_count, 1);
        assert_eq!(inventory.reclaimable_bytes, 7);
        assert!(inventory.entries.iter().any(|entry| entry.path == nested));
        assert!(inventory.entries.iter().any(|entry| entry.path == legacy));
        assert!(
            inventory
                .entries
                .iter()
                .filter(|entry| !entry.path.starts_with(&installed))
                .all(|entry| entry.model_name == model)
        );
        let dry = reclaim_abandoned_staging_dirs_at(&installed, true, now, |_| {
            Err("dry run must not quarantine a staging directory".to_owned())
        })?;
        assert_eq!(dry.reclaimed_count, 4);
        assert!(empty.is_dir());
        assert!(partial.is_dir());
        assert!(nested.is_dir());
        assert!(legacy.is_dir());

        let quarantine = parent.join("quarantine");
        std::fs::create_dir(&quarantine).map_err(|error| error.to_string())?;
        let summary = reclaim_abandoned_staging_dirs_at(&installed, false, now, |entry| {
            assert!(
                entry.path.starts_with(&installed)
                    || direct_model_staging_path_is_authorized(&installed, &entry.path)
            );
            std::fs::rename(&entry.path, quarantine.join(&entry.dir_name))
                .map_err(|error| error.to_string())
        })?;
        assert_eq!(summary.reclaimed_count, 4);
        assert_eq!(summary.kept_count, 3);
        assert_eq!(summary.reclaimed_bytes, 7);
        assert!(!empty.exists());
        assert!(!partial.exists());
        assert!(!nested.exists());
        assert!(!legacy.exists());
        assert!(fresh.is_dir());
        assert!(live.is_dir());
        assert!(unknown.is_dir());
        assert!(other.is_dir());
        assert_eq!(
            std::fs::read(installed.join("model.safetensors"))
                .map_err(|error| error.to_string())?,
            b"installed"
        );
        assert_eq!(
            std::fs::read(
                quarantine
                    .join(partial.file_name().ok_or("partial staging leaf")?)
                    .join("model.safetensors.part")
            )
            .map_err(|error| error.to_string())?,
            b"partial"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn direct_model_staging_authority_matches_only_exact_safe_siblings() -> TestResult {
        let root = temp_root("ee-staging-authority-")?;
        let parent = root.path().canonicalize().map_err(|error| error.to_string())?;
        let model = "potion-multilingual-128M";
        let installed = parent.join(model);
        let staging_name = format!(".{model}-download-12345-retained");
        let missing = parent.join(&staging_name);
        // Both the destination and a quarantined staging path may be absent.
        assert!(direct_model_staging_path_is_authorized(&installed, &missing));
        assert!(!installed.exists());
        assert!(!missing.exists());
        std::fs::create_dir(&installed).map_err(|error| error.to_string())?;
        let unrelated = parent.join("unrelated");
        std::fs::create_dir(&unrelated).map_err(|error| error.to_string())?;
        for path in [
            parent.clone(),
            installed.clone(),
            parent.join(".potion-base-8m-download-12345-retained"),
            parent.join(format!(".{model}-extra-download-12345-retained")),
            parent.join(format!(".{model}-download-invalid-retained")),
            unrelated.join(&staging_name),
            unrelated.join("..").join(&staging_name),
            missing.join(&staging_name),
            PathBuf::from(&staging_name),
        ] {
            assert!(
                !direct_model_staging_path_is_authorized(&installed, &path),
                "unexpected sibling authority for {}",
                path.display()
            );
        }
        assert!(!direct_model_staging_path_is_authorized(
            &unrelated.join("..").join(model),
            &missing
        ));
        std::os::unix::fs::symlink(&installed, &missing)
            .map_err(|error| error.to_string())?;
        assert!(!direct_model_staging_path_is_authorized(&installed, &missing));

        let linked_parent = parent.join("linked-parent");
        std::os::unix::fs::symlink(&unrelated, &linked_parent)
            .map_err(|error| error.to_string())?;
        let linked_root = linked_parent.join(model);
        let linked_candidate = linked_parent.join(staging_name);
        assert!(!direct_model_staging_path_is_authorized(&linked_root, &linked_candidate));
        assert!(inspect_staging_dirs_under(&linked_root).entries.is_empty());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn direct_model_reclaim_rechecks_scope_after_an_earlier_quarantine() -> TestResult {
        let root = temp_root("ee-staging-scope-recheck-")?;
        let parent = root.path().canonicalize().map_err(|error| error.to_string())?;
        let model = "potion-multilingual-128M";
        let installed = parent.join(model);
        std::fs::create_dir(&installed).map_err(|error| error.to_string())?;
        let mut child = std::process::Command::new("true")
            .spawn()
            .map_err(|error| error.to_string())?;
        let dead_pid = child.id();
        child.wait().map_err(|error| error.to_string())?;
        let first = parent.join(format!(".{model}-download-{dead_pid}-0001"));
        let second = parent.join(format!(".{model}-download-{dead_pid}-0002"));
        for path in [&first, &second] {
            std::fs::create_dir(path).map_err(|error| error.to_string())?;
        }
        let retained = parent.join("retained-model");
        let quarantined = parent.join("quarantined-first");
        let later = SystemTime::now() + ABANDONED_STAGING_AGE + Duration::from_secs(60);
        let summary = reclaim_abandoned_staging_dirs_at(&installed, false, later, |entry| {
            assert_eq!(entry.path, first, "changed scope must suppress the second callback");
            std::fs::rename(&entry.path, &quarantined).map_err(|error| error.to_string())?;
            std::fs::rename(&installed, &retained).map_err(|error| error.to_string())?;
            std::os::unix::fs::symlink(&retained, &installed)
                .map_err(|error| error.to_string())
        })?;
        assert_eq!(summary.reclaimed_count, 1);
        assert_eq!(summary.kept_count, 1);
        assert!(quarantined.is_dir());
        assert!(second.is_dir());
        assert!(retained.is_dir());
        Ok(())
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

        // Age never overrides a live or unmeasurable owner, even for an empty directory.
        let future = SystemTime::now() + ABANDONED_NON_EMPTY_STAGING_AGE + Duration::from_secs(60);
        let summary = reclaim_abandoned_staging_dirs_at(root.path(), false, future, |_| {
            Err("live or unknown PID staging must not be quarantined".to_owned())
        })?;
        assert_eq!(summary.reclaimed_count, 0);
        assert_eq!(summary.kept_count, 3);
        assert!(fresh_empty.is_dir());
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
        let dry =
            reclaim_abandoned_staging_dirs_at(root.path(), true, twenty_five_hours, |_| {
                Err("dry run must not invoke the mutation callback".to_owned())
            })?;
        assert_eq!(dry.reclaimed_count, 3);
        assert_eq!(dry.reclaimed_bytes, payload.len() as u64);
        assert!(abandoned_empty.exists());
        assert!(abandoned_dead_non_empty.exists());
        assert!(nested_empty.exists());

        // Reclamation delegates every move to its caller; it never deletes
        // a partial model or an empty directory itself.
        let quarantine = root.path().join("quarantine");
        std::fs::create_dir(&quarantine).map_err(|error| error.to_string())?;
        let real = reclaim_abandoned_staging_dirs_at(
            root.path(),
            false,
            twenty_five_hours,
            |entry| {
                std::fs::rename(&entry.path, quarantine.join(&entry.dir_name))
                    .map_err(|error| error.to_string())
            },
        )?;
        assert_eq!(real.reclaimed_count, 3);
        assert_eq!(real.reclaimed_bytes, payload.len() as u64);
        assert_eq!(real.failed_count, 0);
        assert!(!abandoned_empty.exists());
        assert!(!abandoned_dead_non_empty.exists());
        assert!(!nested_empty.exists());
        assert_eq!(
            std::fs::read(
                quarantine
                    .join(abandoned_dead_non_empty.file_name().ok_or("staging leaf")?)
                    .join("model.safetensors.part")
            )
            .map_err(|error| error.to_string())?,
            payload
        );
        Ok(())
    }
}
