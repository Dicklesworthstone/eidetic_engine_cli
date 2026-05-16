//! Platform data-directory resolution helpers.
//!
//! This module keeps platform-specific path selection pure and testable.
//! Callers pass an explicit environment map so tests can verify Windows
//! `%APPDATA%` / `%LOCALAPPDATA%` behavior from any host without mutating the
//! process environment.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

/// Degraded code emitted when a Windows data directory cannot be resolved.
pub const WINDOWS_APPDATA_UNAVAILABLE_CODE: &str = "windows_appdata_unavailable";

/// Platform data-directory resolution failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformDataDirError {
    pub code: &'static str,
    pub variable: &'static str,
    pub repair: &'static str,
}

impl std::fmt::Display for PlatformDataDirError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} is not set; {}", self.variable, self.repair)
    }
}

impl std::error::Error for PlatformDataDirError {}

/// Resolve the roaming Windows app data directory used for user-scoped state.
///
/// # Errors
///
/// Returns [`WINDOWS_APPDATA_UNAVAILABLE_CODE`] when `APPDATA` is missing or
/// empty.
pub fn resolve_dir_windows_appdata(
    env: &BTreeMap<String, OsString>,
) -> Result<PathBuf, PlatformDataDirError> {
    required_env_path(env, "APPDATA")
}

/// Resolve the local Windows app data directory used for machine-local caches.
///
/// # Errors
///
/// Returns [`WINDOWS_APPDATA_UNAVAILABLE_CODE`] when `LOCALAPPDATA` is missing
/// or empty.
pub fn resolve_dir_windows_localappdata(
    env: &BTreeMap<String, OsString>,
) -> Result<PathBuf, PlatformDataDirError> {
    required_env_path(env, "LOCALAPPDATA")
}

/// Resolve the Unix XDG data directory for `app_name`.
///
/// If `XDG_DATA_HOME` is present, this returns `$XDG_DATA_HOME/<app_name>`.
/// Otherwise it falls back to `$HOME/.local/share/<app_name>`.
///
/// # Errors
///
/// Returns a typed error when neither `XDG_DATA_HOME` nor `HOME` is available.
pub fn resolve_dir_unix_xdg(
    env: &BTreeMap<String, OsString>,
    app_name: &str,
) -> Result<PathBuf, PlatformDataDirError> {
    if let Some(root) = non_empty_env_path(env, "XDG_DATA_HOME") {
        return Ok(root.join(app_name));
    }
    let home = required_env_path_with_repair(
        env,
        "HOME",
        "set HOME, set XDG_DATA_HOME, or pass --workspace explicitly",
    )?;
    Ok(home.join(".local").join("share").join(app_name))
}

fn required_env_path(
    env: &BTreeMap<String, OsString>,
    variable: &'static str,
) -> Result<PathBuf, PlatformDataDirError> {
    required_env_path_with_repair(env, variable, "set APPDATA or pass --workspace explicitly")
}

fn required_env_path_with_repair(
    env: &BTreeMap<String, OsString>,
    variable: &'static str,
    repair: &'static str,
) -> Result<PathBuf, PlatformDataDirError> {
    non_empty_env_path(env, variable).ok_or(PlatformDataDirError {
        code: WINDOWS_APPDATA_UNAVAILABLE_CODE,
        variable,
        repair,
    })
}

fn non_empty_env_path(env: &BTreeMap<String, OsString>, variable: &str) -> Option<PathBuf> {
    let value = env.get(variable)?;
    let path = PathBuf::from(value);
    (!path.as_os_str().is_empty()).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::{
        WINDOWS_APPDATA_UNAVAILABLE_CODE, resolve_dir_unix_xdg, resolve_dir_windows_appdata,
        resolve_dir_windows_localappdata,
    };
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::path::PathBuf;

    type TestResult = Result<(), String>;

    fn env(entries: &[(&str, &str)]) -> BTreeMap<String, OsString> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), OsString::from(value)))
            .collect()
    }

    #[test]
    fn resolve_dir_windows_appdata_requires_appdata() -> TestResult {
        let err = resolve_dir_windows_appdata(&BTreeMap::new())
            .expect_err("missing APPDATA should be reported");
        assert_eq!(err.code, WINDOWS_APPDATA_UNAVAILABLE_CODE);
        assert_eq!(err.variable, "APPDATA");
        assert!(err.repair.contains("--workspace"));

        let resolved =
            resolve_dir_windows_appdata(&env(&[("APPDATA", r"C:\Users\agent\AppData\Roaming")]))
                .map_err(|error| error.to_string())?;
        assert_eq!(resolved, PathBuf::from(r"C:\Users\agent\AppData\Roaming"));
        Ok(())
    }

    #[test]
    fn resolve_dir_windows_localappdata_requires_localappdata() -> TestResult {
        let err = resolve_dir_windows_localappdata(&env(&[("LOCALAPPDATA", "")]))
            .expect_err("empty LOCALAPPDATA should be reported");
        assert_eq!(err.code, WINDOWS_APPDATA_UNAVAILABLE_CODE);
        assert_eq!(err.variable, "LOCALAPPDATA");

        let resolved = resolve_dir_windows_localappdata(&env(&[(
            "LOCALAPPDATA",
            r"C:\Users\agent\AppData\Local",
        )]))
        .map_err(|error| error.to_string())?;
        assert_eq!(resolved, PathBuf::from(r"C:\Users\agent\AppData\Local"));
        Ok(())
    }

    #[test]
    fn resolve_dir_unix_xdg_prefers_xdg_data_home_then_home() -> TestResult {
        let xdg = resolve_dir_unix_xdg(&env(&[("XDG_DATA_HOME", "/var/tmp/xdg")]), "ee")
            .map_err(|error| error.to_string())?;
        assert_eq!(xdg, PathBuf::from("/var/tmp/xdg").join("ee"));

        let home = resolve_dir_unix_xdg(&env(&[("HOME", "/home/agent")]), "ee")
            .map_err(|error| error.to_string())?;
        assert_eq!(home, PathBuf::from("/home/agent/.local/share/ee"));
        Ok(())
    }
}
