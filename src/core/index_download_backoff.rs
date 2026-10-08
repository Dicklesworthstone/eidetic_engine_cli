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
        assert_eq!(remove_own_staging_dirs(root.path(), model), 1);
        assert!(!own.exists());
        assert!(other.exists());
        assert!(installed.exists());
        Ok(())
    }
}
